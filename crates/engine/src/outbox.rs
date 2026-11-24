//! Messages on their way out. Written to disk before the first try, sent in order per
//! conversation, and never sent twice on purpose.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use store::{MessageRecord, OutboxRecord, OutboxState, OutboxTarget, Store, plain_text};
use teams_core::markdown_to_html;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::backend::Failure;

const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// The service's clock may differ from ours.
const CLOCK_SLACK: chrono::Duration = chrono::Duration::seconds(10);
const META_SENT: &str = "tern.outbox.sent";
const REMEMBERED: usize = 100;

pub trait Post: Send + Sync + 'static {
    /// Sends and answers the message id.
    fn post(
        &self,
        conversation: &str,
        markdown: &str,
        client_id: &str,
    ) -> impl Future<Output = Result<String, Failure>> + Send;
    /// The user's own messages, read fresh.
    fn own_since(
        &self,
        conversation: &str,
        since: DateTime<Utc>,
    ) -> impl Future<Output = Result<Vec<MessageRecord>, Failure>> + Send;
}

#[derive(Debug, Serialize, Deserialize)]
struct Payload {
    markdown: String,
    /// A try was started; whether it reached the service is unknown.
    tried: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Queued {
    Sent {
        conversation: String,
    },
    Failed {
        conversation: String,
        reason: String,
    },
}

pub struct Outbox<P> {
    post: Arc<P>,
    store: Arc<Store>,
    /// Counts up to ask for a pass over the outbox.
    asked: Arc<watch::Sender<u64>>,
}

impl<P> Clone for Outbox<P> {
    fn clone(&self) -> Self {
        Outbox {
            post: self.post.clone(),
            store: self.store.clone(),
            asked: self.asked.clone(),
        }
    }
}

fn comparable(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl<P: Post> Outbox<P> {
    pub fn new(post: Arc<P>, store: Arc<Store>) -> Self {
        Outbox {
            post,
            store,
            asked: Arc::new(watch::Sender::new(0)),
        }
    }

    /// Stores the message; it is sent in the background, now or when there is a connection.
    pub fn add(&self, conversation: &str, markdown: &str) -> Result<(), String> {
        if markdown.trim().is_empty() {
            return Err("the message is empty".into());
        }
        let record = OutboxRecord {
            id: chatsvc::new_client_message_id(),
            conversation_id: conversation.to_owned(),
            target: OutboxTarget::Flat,
            thread_root_id: None,
            payload: payload(markdown, false),
            images: Vec::new(),
            state: OutboxState::Sending,
            last_error: None,
            created_at: Utc::now(),
        };
        self.store
            .put_outbox(&record)
            .map_err(|error| error.to_string())?;
        self.wake();
        Ok(())
    }

    pub fn waiting(&self) -> usize {
        self.store.sending_outbox().map_or(0, |all| all.len())
    }

    /// Try now, e.g. after a reconnect.
    pub fn wake(&self) {
        self.asked.send_modify(|count| *count += 1);
    }

    pub async fn run(self, report: impl Fn(Queued) + Send + 'static) {
        let mut blocked: HashMap<String, (u32, Instant)> = HashMap::new();
        let mut asked = self.asked.subscribe();
        loop {
            // Seen before the outbox is read, so whatever is added from here on asks again.
            asked.borrow_and_update();
            let records = match self.store.sending_outbox() {
                Ok(records) => records,
                Err(error) => {
                    tracing::error!(%error, "outbox");
                    tokio::time::sleep(RETRY_MAX).await;
                    continue;
                }
            };
            blocked.retain(|id, _| records.iter().any(|record| record.conversation_id == *id));
            // Oldest first, and one conversation's messages never overtake each other.
            let mut held: HashSet<String> = HashSet::new();
            for record in records {
                let conversation = record.conversation_id.clone();
                let waits = blocked
                    .get(&conversation)
                    .is_some_and(|(_, until)| *until > Instant::now());
                if waits || held.contains(&conversation) {
                    held.insert(conversation);
                    continue;
                }
                match self.deliver(&record).await {
                    Ok(()) => {
                        blocked.remove(&conversation);
                        report(Queued::Sent { conversation });
                    }
                    Err(Failure::Permanent(reason)) => {
                        blocked.remove(&conversation);
                        let _ = self.store.mark_outbox_failed(&record.id, &reason);
                        report(Queued::Failed {
                            conversation,
                            reason,
                        });
                    }
                    Err(Failure::Temporary(reason)) => {
                        tracing::debug!(reason, "message waits");
                        let attempts =
                            blocked.get(&conversation).map_or(0, |(count, _)| *count) + 1;
                        let wait = (RETRY_FIRST * 2u32.pow((attempts - 1).min(8))).min(RETRY_MAX);
                        blocked.insert(conversation.clone(), (attempts, Instant::now() + wait));
                        held.insert(conversation);
                    }
                }
            }
            let next = blocked.values().map(|(_, until)| *until).min();
            tokio::select! {
                // A new message, or a new connection: no reason to wait any longer.
                Ok(()) = asked.changed() => blocked.clear(),
                () = async {
                    match next {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {}
            }
        }
    }

    async fn deliver(&self, record: &OutboxRecord) -> Result<(), Failure> {
        let stored: Payload = serde_json::from_str(&record.payload)
            .map_err(|_| Failure::Permanent("the saved message is unreadable".into()))?;
        let disk = |error: store::Error| Failure::Temporary(error.to_string());
        if stored.tried {
            if let Some(message) = self.already_sent(record, &stored.markdown).await? {
                self.remember(&message);
                return self.store.delete_outbox(&record.id).map_err(disk);
            }
        } else {
            let tried = OutboxRecord {
                payload: payload(&stored.markdown, true),
                ..record.clone()
            };
            self.store.put_outbox(&tried).map_err(disk)?;
        }
        let message = self
            .post
            .post(&record.conversation_id, &stored.markdown, &record.id)
            .await?;
        self.remember(&message);
        self.store.delete_outbox(&record.id).map_err(disk)
    }

    /// Whether an earlier try reached the service although its answer did not reach us: the same
    /// text from the user since then, that is not another message sent from here.
    async fn already_sent(
        &self,
        record: &OutboxRecord,
        markdown: &str,
    ) -> Result<Option<String>, Failure> {
        let wanted = comparable(&plain_text(&markdown_to_html(markdown)));
        let sent = self.sent();
        let own = self
            .post
            .own_since(&record.conversation_id, record.created_at - CLOCK_SLACK)
            .await?;
        Ok(own
            .into_iter()
            .find(|message| {
                !sent.contains(&message.message_id)
                    && comparable(&plain_text(&message.body_html)) == wanted
            })
            .map(|message| message.message_id))
    }

    fn sent(&self) -> Vec<String> {
        self.store
            .meta(META_SENT)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    fn remember(&self, message: &str) {
        let mut sent = self.sent();
        sent.push(message.to_owned());
        let excess = sent.len().saturating_sub(REMEMBERED);
        sent.drain(..excess);
        if let Ok(json) = serde_json::to_string(&sent) {
            let _ = self.store.set_meta(META_SENT, &json);
        }
    }
}

fn payload(markdown: &str, tried: bool) -> String {
    serde_json::json!({"markdown": markdown, "tried": tried}).to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Clone, Copy, PartialEq)]
    enum Next {
        Works,
        /// The service gets it, we do not get the answer.
        AnswerLost,
        Offline,
        Forbidden,
    }

    #[derive(Default)]
    struct Service {
        plan: Mutex<Vec<Next>>,
        /// conversation, text, client id
        got: Mutex<Vec<(String, String, String)>>,
        tries: Mutex<Vec<String>>,
    }

    impl Post for Service {
        async fn post(
            &self,
            conversation: &str,
            markdown: &str,
            id: &str,
        ) -> Result<String, Failure> {
            self.tries.lock().unwrap().push(id.to_owned());
            let next = {
                let mut plan = self.plan.lock().unwrap();
                if plan.is_empty() {
                    Next::Works
                } else {
                    plan.remove(0)
                }
            };
            if matches!(next, Next::Works | Next::AnswerLost) {
                self.got
                    .lock()
                    .unwrap()
                    .push((conversation.into(), markdown.into(), id.into()));
            }
            match next {
                Next::Works => Ok(format!("id-{}", self.got.lock().unwrap().len())),
                Next::AnswerLost | Next::Offline => Err(Failure::Temporary("timeout".into())),
                Next::Forbidden => Err(Failure::Permanent("HTTP 403".into())),
            }
        }

        async fn own_since(
            &self,
            conversation: &str,
            _: DateTime<Utc>,
        ) -> Result<Vec<MessageRecord>, Failure> {
            let got = self.got.lock().unwrap();
            Ok(got
                .iter()
                .enumerate()
                .filter(|(_, (id, _, _))| id == conversation)
                .map(|(index, (_, text, _))| MessageRecord {
                    message_id: format!("id-{}", index + 1),
                    body_html: markdown_to_html(text),
                    ..Default::default()
                })
                .collect())
        }
    }

    fn outbox(plan: &[Next]) -> (Outbox<Service>, Arc<Service>, Arc<Mutex<Vec<Queued>>>) {
        let service = Arc::new(Service::default());
        *service.plan.lock().unwrap() = plan.to_vec();
        let store = Arc::new(Store::open_in_memory().unwrap());
        (Outbox::new(service.clone(), store), service, Arc::default())
    }

    async fn run(outbox: &Outbox<Service>, reports: &Arc<Mutex<Vec<Queued>>>, seconds: u64) {
        let reports = reports.clone();
        let task = tokio::spawn(
            outbox
                .clone()
                .run(move |queued| reports.lock().unwrap().push(queued)),
        );
        tokio::time::sleep(Duration::from_secs(seconds)).await;
        task.abort();
        let _ = task.await;
    }

    fn texts(service: &Service) -> Vec<String> {
        service
            .got
            .lock()
            .unwrap()
            .iter()
            .map(|(_, text, _)| text.clone())
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn a_lost_answer_does_not_send_twice() {
        let (outbox, service, reports) = outbox(&[Next::AnswerLost]);
        outbox.add("c", "Hello **you**").unwrap();
        run(&outbox, &reports, 60).await;
        assert_eq!(texts(&service), ["Hello **you**"]);
        assert_eq!(outbox.waiting(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_same_text_twice_is_two_messages() {
        let (outbox, service, reports) = outbox(&[Next::Works, Next::AnswerLost]);
        outbox.add("c", "ok").unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        outbox.add("c", "ok").unwrap();
        run(&outbox, &reports, 60).await;
        assert_eq!(texts(&service).len(), 2);
        // And when the second never reached the service, the first does not stand in for it.
        outbox.add("c", "ok").unwrap();
        *service.plan.lock().unwrap() = vec![Next::Offline];
        run(&outbox, &reports, 60).await;
        assert_eq!(texts(&service).len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn every_try_has_the_same_id() {
        let (outbox, service, reports) = outbox(&[Next::Offline, Next::Offline]);
        outbox.add("c", "x").unwrap();
        run(&outbox, &reports, 60).await;
        let tries = service.tries.lock().unwrap();
        assert_eq!(tries.len(), 3);
        assert!(tries.iter().all(|id| *id == tries[0]));
    }

    #[tokio::test(start_paused = true)]
    async fn order_holds_within_a_conversation_but_not_across() {
        let (outbox, service, reports) = outbox(&[Next::Offline]);
        for (conversation, text) in [("a", "a1"), ("a", "a2"), ("b", "b1")] {
            outbox.add(conversation, text).unwrap();
            std::thread::sleep(Duration::from_millis(3));
        }
        run(&outbox, &reports, 60).await;
        assert_eq!(texts(&service), ["b1", "a1", "a2"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_kill_during_a_send_is_settled_after_the_restart() {
        let (outbox, service, reports) = outbox(&[Next::AnswerLost]);
        outbox.add("c", "x").unwrap();
        // Killed right after the first try.
        run(&outbox, &reports, 1).await;
        assert_eq!(outbox.waiting(), 1);
        let restarted = Outbox::new(service.clone(), outbox.store.clone());
        run(&restarted, &reports, 10).await;
        assert_eq!((texts(&service).len(), restarted.waiting()), (1, 0));
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_is_reported_and_does_not_block() {
        let (outbox, service, reports) = outbox(&[Next::Forbidden]);
        outbox.add("c", "x").unwrap();
        std::thread::sleep(Duration::from_millis(3));
        outbox.add("c", "y").unwrap();
        run(&outbox, &reports, 5).await;
        assert_eq!(texts(&service), ["y"]);
        assert!(
            matches!(&reports.lock().unwrap()[0], Queued::Failed { reason, .. } if reason == "HTTP 403")
        );
        assert!(outbox.add("c", "  ").is_err());
    }
}
