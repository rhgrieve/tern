//! Keeps the cache complete. A conversation that could not be read stays on the list until it
//! could, and every new connection checks for what the last one missed.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chatsvc::realtime::{EventKind, MessageEvent};
use chrono::Utc;
use store::MessageRecord;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until};

use crate::backend::{Backend, Failure};

const PARALLEL: usize = 4;
const REFRESH_EVERY: Duration = Duration::from_secs(5 * 60);
/// Several messages in a row need one refresh.
const SETTLE: Duration = Duration::from_millis(1500);
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(120);
const HISTORY_DAYS: i64 = 14;
const HISTORY_PAUSE: Duration = Duration::from_secs(2);
const NOTES: &str = "48:notes";

#[derive(Debug, Clone)]
pub enum Job {
    Event(MessageEvent),
    /// The push connection is new, or reported a loss: check everything.
    CatchUp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arrived {
    pub conversation: String,
    pub messages: Vec<MessageRecord>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncState {
    pub pending: usize,
    pub problem: Option<String>,
    pub refreshed: bool,
}

#[derive(Debug, Clone, Default)]
struct Work {
    arrivals: BTreeSet<String>,
    changed: BTreeSet<String>,
    attempts: u32,
    not_before: Option<Instant>,
}

pub struct Sync<B> {
    backend: Arc<B>,
    jobs: mpsc::UnboundedReceiver<Job>,
    arrived: mpsc::UnboundedSender<Arrived>,
    state: tokio::sync::watch::Sender<SyncState>,
    work: BTreeMap<String, Work>,
    running: HashSet<String>,
    refresh_at: Instant,
    refresh_failures: u32,
    catch_up: bool,
    refreshing: bool,
    /// The running refresh started after the catch-up was asked for.
    catching_up: bool,
    history_at: Instant,
    no_history: Vec<String>,
    problem: Option<String>,
    refreshed: bool,
}

enum Done {
    Conversation(String, Work, Result<(), Failure>),
    Refresh(Result<(), Failure>),
    Changed(Result<Vec<String>, Failure>),
    History(String, Result<(), Failure>),
}

fn backoff(attempts: u32) -> Duration {
    (RETRY_FIRST * 2u32.pow(attempts.saturating_sub(1).min(10))).min(RETRY_MAX)
}

/// System streams are not conversations.
fn is_conversation(id: &str) -> bool {
    !id.starts_with("48:") || id == NOTES
}

impl<B: Backend> Sync<B> {
    pub fn new(
        backend: Arc<B>,
        jobs: mpsc::UnboundedReceiver<Job>,
        arrived: mpsc::UnboundedSender<Arrived>,
    ) -> (Self, tokio::sync::watch::Receiver<SyncState>) {
        let (state, watch) = tokio::sync::watch::channel(SyncState::default());
        let now = Instant::now();
        let sync = Sync {
            backend,
            jobs,
            arrived,
            state,
            work: BTreeMap::new(),
            running: HashSet::new(),
            refresh_at: now,
            refresh_failures: 0,
            catch_up: true,
            refreshing: false,
            catching_up: false,
            history_at: now,
            no_history: Vec::new(),
            problem: None,
            refreshed: false,
        };
        (sync, watch)
    }

    /// Runs until the job sender is dropped.
    pub async fn run(mut self) {
        let mut tasks: JoinSet<Done> = JoinSet::new();
        loop {
            self.start(&mut tasks);
            self.publish();
            let wake = self.next_wake(tasks.is_empty());
            tokio::select! {
                job = self.jobs.recv() => match job {
                    Some(job) => self.job(job),
                    None => return,
                },
                Some(done) = tasks.join_next() => match done {
                    Ok(done) => self.done(done),
                    Err(error) => {
                        // A panic in the engine: forget who was running and check again.
                        tracing::error!(%error, "reading messages failed unexpectedly; starting again");
                        self.running.clear();
                        self.refreshing = false;
                        tasks.abort_all();
                        self.catch_up = true;
                        self.refresh_at = Instant::now() + RETRY_FIRST;
                    }
                },
                () = sleep_until(wake) => {}
            }
        }
    }

    fn job(&mut self, job: Job) {
        match job {
            Job::CatchUp => {
                self.catch_up = true;
                self.refresh_at = Instant::now();
                self.refresh_failures = 0;
                // The network may just have come back.
                for work in self.work.values_mut() {
                    work.not_before = None;
                    work.attempts = 0;
                }
            }
            Job::Event(event) => {
                let Some(conversation) = event.conversation_id.filter(|id| is_conversation(id))
                else {
                    return;
                };
                let work = self.work.entry(conversation);
                match (event.kind, event.message_id) {
                    (EventKind::NewMessage, message) => {
                        work.or_default().arrivals.extend(message);
                        self.refresh_at = self.refresh_at.min(Instant::now() + SETTLE);
                    }
                    (EventKind::MessageUpdate, Some(message)) => {
                        work.or_default().changed.insert(message);
                    }
                    (EventKind::MessageUpdate, None) => {
                        work.or_default();
                    }
                    _ => {}
                }
            }
        }
    }

    fn start(&mut self, tasks: &mut JoinSet<Done>) {
        let now = Instant::now();
        if !self.refreshing && self.refresh_at <= now {
            self.refreshing = true;
            // Whatever asks for a refresh from here on gets another one.
            self.refresh_at = now + REFRESH_EVERY;
            self.catching_up = std::mem::take(&mut self.catch_up);
            if self.catching_up {
                let backend = self.backend.clone();
                tasks.spawn(async move { Done::Changed(backend.changed().await) });
            }
            let backend = self.backend.clone();
            tasks.spawn(async move { Done::Refresh(backend.refresh().await) });
        }
        let ready: Vec<String> = self
            .work
            .iter()
            .filter(|(id, work)| {
                !self.running.contains(*id) && work.not_before.is_none_or(|at| at <= now)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ready {
            if self.running.len() >= PARALLEL {
                return;
            }
            let Some(work) = self.work.remove(&id) else {
                continue;
            };
            self.running.insert(id.clone());
            let (backend, arrived) = (self.backend.clone(), self.arrived.clone());
            tasks.spawn(async move {
                let outcome = read(&*backend, &id, &work, &arrived).await;
                Done::Conversation(id, work, outcome)
            });
        }
        let idle = self.work.is_empty() && self.running.is_empty() && !self.refreshing;
        if idle && self.refreshed && self.history_at <= now {
            self.history_at = now + HISTORY_PAUSE;
            let since = Utc::now() - chrono::Duration::days(HISTORY_DAYS);
            if let Some(id) = self.backend.shallow(since, &self.no_history) {
                self.running.insert(id.clone());
                let backend = self.backend.clone();
                tasks.spawn(async move {
                    let outcome = backend.older(&id).await;
                    Done::History(id, outcome)
                });
            } else {
                self.history_at = now + REFRESH_EVERY;
            }
        }
    }

    fn done(&mut self, done: Done) {
        match done {
            Done::Conversation(id, work, outcome) => {
                self.running.remove(&id);
                match outcome {
                    Ok(()) => {}
                    Err(Failure::Permanent(error)) => {
                        tracing::debug!(error, "conversation skipped");
                    }
                    Err(Failure::Temporary(error)) => {
                        // What arrived meanwhile joins what could not be read.
                        let entry = self.work.entry(id).or_default();
                        entry.arrivals.extend(work.arrivals);
                        entry.changed.extend(work.changed);
                        entry.attempts = work.attempts + 1;
                        entry.not_before = Some(Instant::now() + backoff(entry.attempts));
                        self.problem = Some(error);
                        return;
                    }
                }
                if self.work.is_empty() && self.running.is_empty() {
                    self.problem = None;
                }
            }
            Done::Changed(Ok(ids)) => {
                for id in ids.into_iter().filter(|id| is_conversation(id)) {
                    self.work.entry(id).or_default();
                }
            }
            // The refresh finds the same, only later.
            Done::Changed(Err(failure)) => {
                tracing::debug!(error = failure.text(), "quick check");
            }
            Done::Refresh(Ok(())) => {
                self.refreshing = false;
                self.refreshed = true;
                self.refresh_failures = 0;
                if std::mem::take(&mut self.catching_up) {
                    for id in self.backend.behind() {
                        self.work.entry(id).or_default();
                    }
                }
            }
            Done::Refresh(Err(failure)) => {
                self.refreshing = false;
                self.catch_up |= std::mem::take(&mut self.catching_up);
                self.refresh_failures += 1;
                self.refresh_at = Instant::now() + backoff(self.refresh_failures);
                tracing::warn!(error = failure.text(), "could not read the list of chats");
                self.problem = Some(failure.text().to_owned());
            }
            Done::History(id, outcome) => {
                self.running.remove(&id);
                if let Err(failure) = outcome {
                    tracing::debug!(error = failure.text(), "older messages");
                    self.no_history.push(id);
                }
            }
        }
    }

    fn next_wake(&self, idle: bool) -> Instant {
        let mut wake = if self.refreshing {
            Instant::now() + REFRESH_EVERY
        } else {
            self.refresh_at
        };
        let retries = self
            .work
            .iter()
            .filter(|(id, _)| !self.running.contains(*id))
            .filter_map(|(_, work)| work.not_before);
        wake = retries.fold(wake, Instant::min);
        if idle && self.refreshed && self.work.is_empty() {
            wake = wake.min(self.history_at);
        }
        // Work that is ready but waits for a free slot is started when a task ends.
        wake.max(Instant::now() + Duration::from_millis(50))
    }

    fn publish(&self) {
        let next = SyncState {
            pending: self.work.len() + self.running.len(),
            problem: self.problem.clone(),
            refreshed: self.refreshed,
        };
        self.state.send_if_modified(|state| {
            let changed = *state != next;
            if changed {
                *state = next;
            }
            changed
        });
    }
}

async fn read<B: Backend>(
    backend: &B,
    id: &str,
    work: &Work,
    arrived: &mpsc::UnboundedSender<Arrived>,
) -> Result<(), Failure> {
    let deliver = |messages: Vec<MessageRecord>| {
        if !messages.is_empty() {
            let _ = arrived.send(Arrived {
                conversation: id.to_owned(),
                messages,
            });
        }
    };
    // Delivered as they come: what is cached is not "new" again on the next try.
    deliver(backend.fetch(id).await?);
    for message in &work.arrivals {
        if !backend.cached(id, message) {
            deliver(backend.fetch_until(id, message).await?);
        }
    }
    for message in &work.changed {
        match backend.reread(id, message).await {
            // Gone, or not a message we can read: nothing to update.
            Ok(()) | Err(Failure::Permanent(_)) => {}
            Err(failure) => return Err(failure),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct Server {
        /// All messages, and how many reads fail first.
        messages: HashMap<String, Vec<String>>,
        failing: usize,
        refresh_failing: usize,
        refresh_takes: Duration,
        refreshes: usize,
        /// Messages that only show up after this many reads.
        late: HashMap<String, usize>,
        reads: usize,
        parallel: usize,
        most_parallel: usize,
        rereads: Vec<String>,
    }

    impl Fake {
        fn refreshes(&self) -> usize {
            self.server.lock().unwrap().refreshes
        }
    }

    #[derive(Default)]
    struct Fake {
        server: Mutex<Server>,
        cache: Mutex<HashMap<String, Vec<String>>>,
    }

    impl Fake {
        fn add(&self, conversation: &str, message: &str) {
            let mut server = self.server.lock().unwrap();
            server
                .messages
                .entry(conversation.into())
                .or_default()
                .push(message.into());
        }

        async fn read(&self, conversation: &str) -> Result<Vec<MessageRecord>, Failure> {
            {
                let mut server = self.server.lock().unwrap();
                server.reads += 1;
                server.parallel += 1;
                server.most_parallel = server.most_parallel.max(server.parallel);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut server = self.server.lock().unwrap();
            server.parallel -= 1;
            if server.failing > 0 {
                server.failing -= 1;
                return Err(Failure::Temporary("HTTP 503".into()));
            }
            let known = server
                .messages
                .get(conversation)
                .cloned()
                .unwrap_or_default();
            let mut cache = self.cache.lock().unwrap();
            let cached = cache.entry(conversation.into()).or_default();
            let mut added = Vec::new();
            for message in known {
                if let Some(left) = server.late.get_mut(&message)
                    && *left > 0
                {
                    *left -= 1;
                    continue;
                }
                if !cached.contains(&message) {
                    cached.push(message.clone());
                    added.push(MessageRecord {
                        conversation_id: conversation.into(),
                        message_id: message,
                        ..Default::default()
                    });
                }
            }
            Ok(added)
        }
    }

    impl Backend for Fake {
        async fn refresh(&self) -> Result<(), Failure> {
            let takes = {
                let mut server = self.server.lock().unwrap();
                server.refreshes += 1;
                server.refresh_takes
            };
            tokio::time::sleep(takes).await;
            let mut server = self.server.lock().unwrap();
            if server.refresh_failing > 0 {
                server.refresh_failing -= 1;
                return Err(Failure::Temporary("offline".into()));
            }
            Ok(())
        }
        async fn fetch(&self, conversation: &str) -> Result<Vec<MessageRecord>, Failure> {
            if conversation == "gone" {
                return Err(Failure::Permanent("unknown conversation".into()));
            }
            self.read(conversation).await
        }
        async fn fetch_until(
            &self,
            conversation: &str,
            message: &str,
        ) -> Result<Vec<MessageRecord>, Failure> {
            let mut all = Vec::new();
            for _ in 0..4 {
                all.extend(self.read(conversation).await?);
                if self.cached(conversation, message) {
                    break;
                }
            }
            Ok(all)
        }
        async fn reread(&self, _: &str, message: &str) -> Result<(), Failure> {
            self.server.lock().unwrap().rereads.push(message.into());
            Ok(())
        }
        fn cached(&self, conversation: &str, message: &str) -> bool {
            let cache = self.cache.lock().unwrap();
            cache
                .get(conversation)
                .is_some_and(|all| all.iter().any(|id| id == message))
        }
        fn behind(&self) -> Vec<String> {
            let server = self.server.lock().unwrap();
            let cache = self.cache.lock().unwrap();
            server
                .messages
                .iter()
                .filter(|(id, all)| cache.get(*id).map_or(0, Vec::len) < all.len())
                .map(|(id, _)| id.clone())
                .collect()
        }
        async fn changed(&self) -> Result<Vec<String>, Failure> {
            if self.server.lock().unwrap().refresh_failing > 0 {
                return Err(Failure::Temporary("offline".into()));
            }
            let known: Vec<String> = self.cache.lock().unwrap().keys().cloned().collect();
            Ok(self
                .behind()
                .into_iter()
                .filter(|id| known.contains(id))
                .collect())
        }
        fn shallow(&self, _: chrono::DateTime<Utc>, _: &[String]) -> Option<String> {
            None
        }
        async fn older(&self, _: &str) -> Result<(), Failure> {
            Ok(())
        }
    }

    struct Rig {
        fake: Arc<Fake>,
        jobs: mpsc::UnboundedSender<Job>,
        arrived: mpsc::UnboundedReceiver<Arrived>,
    }

    impl Rig {
        fn start(setup: impl FnOnce(&Fake)) -> Rig {
            let fake = Arc::new(Fake::default());
            setup(&fake);
            let (jobs, job_queue) = mpsc::unbounded_channel();
            let (sender, arrived) = mpsc::unbounded_channel();
            let (sync, _) = Sync::new(fake.clone(), job_queue, sender);
            tokio::spawn(sync.run());
            Rig {
                fake,
                jobs,
                arrived,
            }
        }

        fn event(&self, kind: EventKind, conversation: &str, message: &str) {
            let _ = self.jobs.send(Job::Event(MessageEvent {
                resource_type: String::new(),
                kind,
                conversation_id: Some(conversation.into()),
                message_id: Some(message.into()),
                received_at: Utc::now(),
            }));
        }

        /// Everything that arrived within `seconds` of the paused clock.
        async fn after(&mut self, seconds: u64) -> Vec<String> {
            tokio::time::sleep(Duration::from_secs(seconds)).await;
            let mut ids = Vec::new();
            while let Ok(arrived) = self.arrived.try_recv() {
                ids.extend(
                    arrived
                        .messages
                        .into_iter()
                        .map(|message| message.message_id),
                );
            }
            ids.sort();
            ids
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_start_reads_what_is_behind() {
        let mut rig = Rig::start(|fake| {
            for index in 0..9 {
                fake.add(&format!("c{index}"), &format!("m{index}"));
            }
        });
        assert_eq!(rig.after(5).await.len(), 9);
        assert!(rig.fake.server.lock().unwrap().most_parallel <= PARALLEL);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_read_is_tried_until_it_works() {
        let mut rig = Rig::start(|_| {});
        rig.after(1).await;
        rig.fake.add("c", "m1");
        rig.fake.server.lock().unwrap().failing = 3;
        rig.event(EventKind::NewMessage, "c", "m1");
        assert!(rig.after(5).await.is_empty());
        assert_eq!(rig.after(30).await, ["m1"]);
        assert!(rig.after(600).await.is_empty(), "delivered once");
    }

    #[tokio::test(start_paused = true)]
    async fn a_message_announced_early_is_waited_for() {
        let mut rig = Rig::start(|_| {});
        rig.after(1).await;
        rig.fake.add("c", "m1");
        rig.fake.server.lock().unwrap().late.insert("m1".into(), 2);
        rig.event(EventKind::NewMessage, "c", "m1");
        assert_eq!(rig.after(5).await, ["m1"]);
    }

    #[tokio::test(start_paused = true)]
    async fn duplicated_and_reordered_events_deliver_once() {
        let mut rig = Rig::start(|_| {});
        rig.after(1).await;
        rig.fake.add("c", "m1");
        rig.fake.add("c", "m2");
        for message in ["m2", "m1", "m2", "m1"] {
            rig.event(EventKind::NewMessage, "c", message);
        }
        assert_eq!(rig.after(5).await, ["m1", "m2"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_message_during_a_read_is_not_lost() {
        let mut rig = Rig::start(|_| {});
        rig.after(1).await;
        rig.fake.add("c", "m1");
        rig.event(EventKind::NewMessage, "c", "m1");
        tokio::time::sleep(Duration::from_millis(50)).await;
        rig.fake.add("c", "m2");
        rig.event(EventKind::NewMessage, "c", "m2");
        assert_eq!(rig.after(5).await, ["m1", "m2"]);
    }

    #[tokio::test(start_paused = true)]
    async fn messages_missed_while_offline_come_with_the_next_connection() {
        let mut rig = Rig::start(|_| {});
        rig.after(1).await;
        rig.fake.add("c", "m1");
        assert!(rig.after(60).await.is_empty(), "nothing announced it");
        rig.fake.server.lock().unwrap().refresh_failing = 2;
        let _ = rig.jobs.send(Job::CatchUp);
        assert_eq!(rig.after(20).await, ["m1"]);
    }

    #[tokio::test(start_paused = true)]
    async fn what_asks_during_a_refresh_gets_another_one() {
        let mut rig = Rig::start(|fake| {
            fake.server.lock().unwrap().refresh_takes = Duration::from_secs(80);
        });
        rig.after(10).await;
        rig.event(EventKind::NewMessage, "c", "m");
        let _ = rig.jobs.send(Job::CatchUp);
        rig.after(75).await;
        assert_eq!(rig.fake.refreshes(), 2);
        rig.fake.add("d", "m2");
        assert_eq!(
            rig.after(90).await,
            ["m2"],
            "the second refresh checks again"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_known_conversation_does_not_wait_for_a_slow_refresh() {
        let mut rig = Rig::start(|fake| {
            fake.server.lock().unwrap().refresh_takes = Duration::from_secs(80);
            fake.cache.lock().unwrap().insert("c".into(), Vec::new());
            fake.add("c", "m1");
            fake.add("new", "m2");
        });
        assert_eq!(rig.after(3).await, ["m1"]);
        assert_eq!(rig.after(90).await, ["m2"]);
    }

    #[tokio::test(start_paused = true)]
    async fn unknown_conversations_and_system_streams_are_dropped() {
        let mut rig = Rig::start(|_| {});
        rig.after(1).await;
        rig.event(EventKind::NewMessage, "gone", "m1");
        rig.event(EventKind::NewMessage, "48:calllogs", "m2");
        let before = rig.fake.server.lock().unwrap().reads;
        assert!(rig.after(300).await.is_empty());
        assert_eq!(rig.fake.server.lock().unwrap().reads, before);
    }

    #[tokio::test(start_paused = true)]
    async fn an_edit_rereads_the_message() {
        let mut rig = Rig::start(|_| {});
        rig.after(1).await;
        rig.event(EventKind::MessageUpdate, "c", "m1");
        rig.after(5).await;
        assert_eq!(rig.fake.server.lock().unwrap().rereads, ["m1"]);
    }
}
