//! Message sync, the outbox and notification rules for the signed-in account.

use std::sync::Arc;

use chatsvc::realtime::MessageEvent;
use chrono::{DateTime, Utc};
use session::Session;
use store::Store;
use tern_engine::{
    Arrived, Job, Notice, Outbox, Queued, Rules, Sync, SyncState, Teams, database_path, open_store,
};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

pub enum News {
    Arrived(Arrived),
    Failed { reason: String },
}

pub struct Messages {
    account: String,
    store: Arc<Store>,
    jobs: mpsc::UnboundedSender<Job>,
    state: watch::Receiver<SyncState>,
    outbox: Outbox<Teams>,
    rules: Rules,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Messages {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// The region of the last connection, so a start without a network can queue messages.
const META_REGION: &str = "tern.region";

/// The stored region of an account that was connected before.
pub fn stored_region(account: &str) -> Option<String> {
    let path = database_path(account).ok().filter(|path| path.exists())?;
    Store::open_read_only(&path).ok()?.meta(META_REGION).ok()?
}

impl Messages {
    pub fn remember_region(&self, region: &str) {
        let _ = self.store.set_meta(META_REGION, region);
    }

    pub fn start(
        account: &str,
        session: Session,
        news: mpsc::UnboundedSender<News>,
    ) -> Result<Self, String> {
        let path = database_path(account).map_err(|error| error.to_string())?;
        let store = Arc::new(open_store(&path)?);
        let teams = Teams::new(session, store.clone());
        let (jobs, job_queue) = mpsc::unbounded_channel();
        let (arrived, mut arrivals) = mpsc::unbounded_channel();
        let (sync, state) = Sync::new(teams.clone(), job_queue, arrived);
        let outbox = Outbox::new(teams, store.clone());
        let sent = news.clone();
        let tasks = vec![
            tokio::spawn(sync.run()),
            tokio::spawn(async move {
                while let Some(arrival) = arrivals.recv().await {
                    if news.send(News::Arrived(arrival)).is_err() {
                        return;
                    }
                }
            }),
            tokio::spawn(outbox.clone().run(move |queued| {
                if let Queued::Failed { reason, .. } = queued {
                    let _ = sent.send(News::Failed { reason });
                }
            })),
        ];
        Ok(Messages {
            account: account.to_owned(),
            store,
            jobs,
            state,
            outbox,
            rules: Rules::new(Utc::now()),
            tasks,
        })
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    pub fn event(&self, event: MessageEvent) {
        let _ = self.jobs.send(Job::Event(event));
    }

    pub fn catch_up(&self) {
        let _ = self.jobs.send(Job::CatchUp);
        self.outbox.wake();
    }

    pub fn send(&self, conversation: &str, text: &str) -> Result<(), String> {
        self.outbox.add(conversation, text)
    }

    pub fn state(&self) -> SyncState {
        self.state.borrow().clone()
    }

    pub fn unsent(&self) -> usize {
        self.outbox.waiting()
    }

    pub fn read_until(&self, conversation: &str) -> Option<DateTime<Utc>> {
        self.store
            .chat(conversation)
            .ok()
            .flatten()
            .and_then(|chat| chat.last_read_at)
    }

    pub fn notices(&mut self, arrived: &Arrived) -> Vec<Notice> {
        let chat = self.store.chat(&arrived.conversation).ok().flatten();
        let now = Utc::now();
        arrived
            .messages
            .iter()
            .filter_map(|message| {
                self.rules
                    .decide(message, chat.as_ref(), Some(&self.account), now)
            })
            .collect()
    }
}
