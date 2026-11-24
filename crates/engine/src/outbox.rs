//! Messages on their way out. Written to disk before the first try, sent in order per
//! conversation, and never sent twice on purpose.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use store::{MessageRecord, OutboxRecord, OutboxState, OutboxTarget, Store};
use teams_core::{FileReference, files, images, markdown_to_html, readable_text};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::backend::Failure;
use crate::media::{private_folder, safe_name};

const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// The service's clock may differ from ours.
const CLOCK_SLACK: chrono::Duration = chrono::Duration::seconds(10);
const META_SENT: &str = "tern.outbox.sent";
const REMEMBERED: usize = 100;

/// A file that goes with a message. Ours: a copy, so it does not matter what becomes of the
/// original.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attached {
    pub name: String,
    pub path: PathBuf,
    /// Shown in the message instead of as a file.
    #[serde(default)]
    pub picture: bool,
    /// Where it is, once it was uploaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<FileReference>,
}

pub const MAX_FILES: usize = 10;
pub const MAX_FILE_BYTES: u64 = 100 << 20;
const MAX_PICTURE_BYTES: u64 = 10 << 20;

/// Whether the file is a picture Teams shows inline, by its first bytes.
pub fn picture_type(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xff, 0xd8, 0xff, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        _ => None,
    }
}

/// A message to send.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub markdown: String,
    /// Quotes this message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// Answers in the thread of this post.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    /// Starts a new post in a channel.
    #[serde(default)]
    pub post: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// People mentioned as `@name`, in the order of the text: id and name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<Attached>,
}

impl Draft {
    pub fn text(markdown: &str) -> Draft {
        Draft {
            markdown: markdown.to_owned(),
            ..Default::default()
        }
    }
}

pub trait Post: Send + Sync + 'static {
    /// Sends and answers the message id.
    fn post(
        &self,
        conversation: &str,
        draft: &Draft,
        client_id: &str,
    ) -> impl Future<Output = Result<String, Failure>> + Send;
    /// Uploads a file and lets the conversation's members read it.
    fn upload(
        &self,
        conversation: &str,
        file: &Attached,
    ) -> impl Future<Output = Result<FileReference, Failure>> + Send;
    /// The user's own messages, read fresh.
    fn own_since(
        &self,
        conversation: &str,
        since: DateTime<Utc>,
    ) -> impl Future<Output = Result<Vec<MessageRecord>, Failure>> + Send;
}

#[derive(Debug, Serialize, Deserialize)]
struct Payload {
    #[serde(flatten)]
    draft: Draft,
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
    /// Holds the copies of files, one folder per message.
    spool: PathBuf,
}

impl<P> Clone for Outbox<P> {
    fn clone(&self) -> Self {
        Outbox {
            post: self.post.clone(),
            store: self.store.clone(),
            asked: self.asked.clone(),
            spool: self.spool.clone(),
        }
    }
}

/// Two messages without text are only the same when they carry the same.
fn same_files(message: &MessageRecord, draft: &Draft) -> bool {
    let mut wanted: Vec<&str> = draft
        .files
        .iter()
        .filter(|file| !file.picture)
        .map(|file| {
            file.reference
                .as_ref()
                .map_or(file.name.as_str(), |reference| reference.name.as_str())
        })
        .collect();
    let carried = files(message);
    let mut found: Vec<&str> = carried.iter().map(|file| file.name.as_str()).collect();
    wanted.sort_unstable();
    found.sort_unstable();
    let pictures = draft.files.iter().filter(|file| file.picture).count();
    wanted == found && images(message).len() == pictures
}

fn comparable(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl<P: Post> Outbox<P> {
    pub fn new(post: Arc<P>, store: Arc<Store>, spool: &Path) -> Self {
        Outbox {
            post,
            store,
            asked: Arc::new(watch::Sender::new(0)),
            spool: spool.to_owned(),
        }
    }

    /// Stores the message; it is sent in the background, now or when there is a connection.
    /// `files` are copied, so they can be changed or removed right after.
    pub fn add(&self, conversation: &str, draft: &Draft, files: &[PathBuf]) -> Result<(), String> {
        if draft.markdown.trim().is_empty() && files.is_empty() {
            return Err("the message is empty".into());
        }
        if files.len() > MAX_FILES {
            return Err(format!("at most {MAX_FILES} files go with a message"));
        }
        let id = chatsvc::new_client_message_id();
        let mut draft = draft.clone();
        match self.copy(&id, files) {
            Ok(files) => draft.files = files,
            Err(error) => {
                self.forget_files(&id);
                return Err(error);
            }
        }
        let record = OutboxRecord {
            id: id.clone(),
            conversation_id: conversation.to_owned(),
            target: match (&draft.thread, draft.post) {
                (Some(_), _) => OutboxTarget::Thread,
                (None, true) => OutboxTarget::Post,
                (None, false) => OutboxTarget::Flat,
            },
            thread_root_id: draft.thread.clone(),
            payload: payload(&draft, false),
            images: Vec::new(),
            state: OutboxState::Sending,
            last_error: None,
            created_at: Utc::now(),
        };
        if let Err(error) = self.store.put_outbox(&record) {
            self.forget_files(&id);
            return Err(error.to_string());
        }
        self.wake();
        Ok(())
    }

    fn copy(&self, id: &str, files: &[PathBuf]) -> Result<Vec<Attached>, String> {
        let folder = self.spool.join(id);
        let mut copies = Vec::new();
        for (index, from) in files.iter().enumerate() {
            let name = safe_name(&from.file_name().unwrap_or_default().to_string_lossy());
            let said = |error: std::io::Error| format!("{name}: {error}");
            let data = std::fs::metadata(from).map_err(said)?;
            if !data.is_file() {
                return Err(format!("{name} is not a file"));
            }
            if data.len() == 0 {
                return Err(format!("{name} is empty"));
            }
            if data.len() > MAX_FILE_BYTES {
                return Err(format!("{name} is larger than {} MB", MAX_FILE_BYTES >> 20));
            }
            private_folder(&folder).map_err(said)?;
            let path = folder.join(format!("{index}-{name}"));
            std::fs::copy(from, &path).map_err(said)?;
            let mut start = [0u8; 8];
            let read = std::fs::File::open(&path)
                .and_then(|mut file| std::io::Read::read(&mut file, &mut start))
                .map_err(said)?;
            copies.push(Attached {
                picture: picture_type(&start[..read]).is_some() && data.len() <= MAX_PICTURE_BYTES,
                name,
                path,
                reference: None,
            });
        }
        Ok(copies)
    }

    fn forget_files(&self, id: &str) {
        let folder = self.spool.join(id);
        if folder.is_dir()
            && let Err(error) = std::fs::remove_dir_all(&folder)
        {
            tracing::warn!(%error, "a sent file's copy stays");
        }
    }

    /// Tries a message again that was given up.
    pub fn retry(&self, id: &str) -> Result<(), String> {
        let mut record = self.failed(id)?;
        record.state = OutboxState::Sending;
        record.last_error = None;
        self.store
            .put_outbox(&record)
            .map_err(|error| error.to_string())?;
        self.wake();
        Ok(())
    }

    /// Forgets a message that was given up.
    pub fn discard(&self, id: &str) -> Result<(), String> {
        let record = self.failed(id)?;
        self.store
            .delete_outbox(&record.id)
            .map_err(|error| error.to_string())?;
        self.forget_files(&record.id);
        Ok(())
    }

    fn failed(&self, id: &str) -> Result<OutboxRecord, String> {
        let conversations = self
            .store
            .failed_outbox_conversations()
            .map_err(|error| error.to_string())?;
        conversations
            .iter()
            .filter_map(|conversation| self.store.outbox_for_conversation(conversation).ok())
            .flatten()
            .find(|record| record.id == id && record.state == OutboxState::Failed)
            .ok_or_else(|| "there is no such unsent message".into())
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
        let mut stored: Payload = serde_json::from_str(&record.payload)
            .map_err(|_| Failure::Permanent("the saved message is unreadable".into()))?;
        let disk = |error: store::Error| Failure::Temporary(error.to_string());
        let save = |draft: &Draft, tried: bool| {
            let record = OutboxRecord {
                payload: payload(draft, tried),
                ..record.clone()
            };
            self.store.put_outbox(&record).map_err(disk)
        };
        // Files first, each remembered as soon as it is there: a message that has to wait does
        // not upload them again.
        for index in 0..stored.draft.files.len() {
            let file = &stored.draft.files[index];
            if file.picture || file.reference.is_some() {
                continue;
            }
            let reference = self.post.upload(&record.conversation_id, file).await?;
            stored.draft.files[index].reference = Some(reference);
            save(&stored.draft, stored.tried)?;
        }
        if stored.tried {
            if let Some(message) = self.already_sent(record, &stored.draft).await? {
                self.remember(&message);
                return self.done(record).map_err(disk);
            }
        } else {
            save(&stored.draft, true)?;
        }
        let message = self
            .post
            .post(&record.conversation_id, &stored.draft, &record.id)
            .await?;
        self.remember(&message);
        self.done(record).map_err(disk)
    }

    fn done(&self, record: &OutboxRecord) -> Result<(), store::Error> {
        self.store.delete_outbox(&record.id)?;
        self.forget_files(&record.id);
        Ok(())
    }

    /// Whether an earlier try reached the service although its answer did not reach us: the same
    /// text from the user since then, that is not another message sent from here.
    async fn already_sent(
        &self,
        record: &OutboxRecord,
        draft: &Draft,
    ) -> Result<Option<String>, Failure> {
        // Without quotes, which the service adds to a reply.
        let wanted = comparable(&readable_text(&MessageRecord {
            body_html: markdown_to_html(&draft.markdown),
            ..Default::default()
        }));
        let sent = self.sent();
        let own = self
            .post
            .own_since(&record.conversation_id, record.created_at - CLOCK_SLACK)
            .await?;
        Ok(own
            .into_iter()
            .find(|message| {
                !sent.contains(&message.message_id)
                    && message.reply_to_id == draft.thread
                    && same_files(message, draft)
                    && comparable(&readable_text(message)) == wanted
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

fn payload(draft: &Draft, tried: bool) -> String {
    let payload = Payload {
        draft: draft.clone(),
        tried,
    };
    serde_json::to_string(&payload).unwrap_or_default()
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
        quoting: std::sync::atomic::AtomicBool,
        uploads: Mutex<Vec<String>>,
        /// Uploads that fail first.
        upload_failures: Mutex<usize>,
        sent_files: Mutex<Vec<Vec<String>>>,
    }

    fn folder() -> PathBuf {
        tempfile::tempdir().unwrap().keep()
    }

    impl Post for Service {
        async fn post(
            &self,
            conversation: &str,
            draft: &Draft,
            id: &str,
        ) -> Result<String, Failure> {
            self.tries.lock().unwrap().push(id.to_owned());
            assert!(
                draft
                    .files
                    .iter()
                    .all(|file| file.picture || file.reference.is_some())
            );
            let names = draft.files.iter().map(|file| file.name.clone()).collect();
            self.sent_files.lock().unwrap().push(names);
            let next = {
                let mut plan = self.plan.lock().unwrap();
                if plan.is_empty() {
                    Next::Works
                } else {
                    plan.remove(0)
                }
            };
            if matches!(next, Next::Works | Next::AnswerLost) {
                self.got.lock().unwrap().push((
                    conversation.into(),
                    draft.markdown.clone(),
                    id.into(),
                ));
            }
            match next {
                Next::Works => Ok(format!("id-{}", self.got.lock().unwrap().len())),
                Next::AnswerLost | Next::Offline => Err(Failure::Temporary("timeout".into())),
                Next::Forbidden => Err(Failure::Permanent("HTTP 403".into())),
            }
        }

        async fn upload(&self, _: &str, file: &Attached) -> Result<FileReference, Failure> {
            assert!(file.path.exists());
            let mut failures = self.upload_failures.lock().unwrap();
            if *failures > 0 {
                *failures -= 1;
                return Err(Failure::Temporary("HTTP 503".into()));
            }
            self.uploads.lock().unwrap().push(file.name.clone());
            Ok(FileReference {
                attachment_id: "1".into(),
                content_url: "https://contoso.sharepoint.com/x".into(),
                name: file.name.clone(),
            })
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
                    body_html: if self.quoting.load(std::sync::atomic::Ordering::SeqCst) {
                        format!("<blockquote itemtype=\"http://schema.skype.com/Reply\" itemscope itemid=\"1\"><p>quoted</p></blockquote>{}", markdown_to_html(text))
                    } else {
                        markdown_to_html(text)
                    },
                    ..Default::default()
                })
                .collect())
        }
    }

    fn outbox(plan: &[Next]) -> (Outbox<Service>, Arc<Service>, Arc<Mutex<Vec<Queued>>>) {
        let service = Arc::new(Service::default());
        *service.plan.lock().unwrap() = plan.to_vec();
        let store = Arc::new(Store::open_in_memory().unwrap());
        (
            Outbox::new(service.clone(), store, &folder()),
            service,
            Arc::default(),
        )
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
        outbox.add("c", &Draft::text("Hello **you**"), &[]).unwrap();
        run(&outbox, &reports, 60).await;
        assert_eq!(texts(&service), ["Hello **you**"]);
        assert_eq!(outbox.waiting(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_same_text_twice_is_two_messages() {
        let (outbox, service, reports) = outbox(&[Next::Works, Next::AnswerLost]);
        outbox.add("c", &Draft::text("ok"), &[]).unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        outbox.add("c", &Draft::text("ok"), &[]).unwrap();
        run(&outbox, &reports, 60).await;
        assert_eq!(texts(&service).len(), 2);
        // And when the second never reached the service, the first does not stand in for it.
        outbox.add("c", &Draft::text("ok"), &[]).unwrap();
        *service.plan.lock().unwrap() = vec![Next::Offline];
        run(&outbox, &reports, 60).await;
        assert_eq!(texts(&service).len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn every_try_has_the_same_id() {
        let (outbox, service, reports) = outbox(&[Next::Offline, Next::Offline]);
        outbox.add("c", &Draft::text("x"), &[]).unwrap();
        run(&outbox, &reports, 60).await;
        let tries = service.tries.lock().unwrap();
        assert_eq!(tries.len(), 3);
        assert!(tries.iter().all(|id| *id == tries[0]));
    }

    #[tokio::test(start_paused = true)]
    async fn order_holds_within_a_conversation_but_not_across() {
        let (outbox, service, reports) = outbox(&[Next::Offline]);
        for (conversation, text) in [("a", "a1"), ("a", "a2"), ("b", "b1")] {
            outbox.add(conversation, &Draft::text(text), &[]).unwrap();
            std::thread::sleep(Duration::from_millis(3));
        }
        run(&outbox, &reports, 60).await;
        assert_eq!(texts(&service), ["b1", "a1", "a2"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_kill_during_a_send_is_settled_after_the_restart() {
        let (outbox, service, reports) = outbox(&[Next::AnswerLost]);
        outbox.add("c", &Draft::text("x"), &[]).unwrap();
        // Killed right after the first try.
        run(&outbox, &reports, 1).await;
        assert_eq!(outbox.waiting(), 1);
        let restarted = Outbox::new(service.clone(), outbox.store.clone(), &outbox.spool);
        run(&restarted, &reports, 10).await;
        assert_eq!((texts(&service).len(), restarted.waiting()), (1, 0));
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_is_reported_and_does_not_block() {
        let (outbox, service, reports) = outbox(&[Next::Forbidden]);
        outbox.add("c", &Draft::text("x"), &[]).unwrap();
        std::thread::sleep(Duration::from_millis(3));
        outbox.add("c", &Draft::text("y"), &[]).unwrap();
        run(&outbox, &reports, 5).await;
        assert_eq!(texts(&service), ["y"]);
        assert!(
            matches!(&reports.lock().unwrap()[0], Queued::Failed { reason, .. } if reason == "HTTP 403")
        );
        assert!(outbox.add("c", &Draft::text("  "), &[]).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_reply_is_recognised_in_spite_of_its_quote() {
        let (outbox, service, reports) = outbox(&[Next::AnswerLost]);
        let reply = Draft {
            reply_to: Some("m1".into()),
            ..Draft::text("agreed")
        };
        outbox.add("c", &reply, &[]).unwrap();
        service
            .quoting
            .store(true, std::sync::atomic::Ordering::SeqCst);
        run(&outbox, &reports, 60).await;
        assert_eq!(texts(&service).len(), 1);
    }

    #[test]
    fn older_payloads_are_still_read() {
        let old: Payload = serde_json::from_str(r#"{"markdown":"x","tried":true}"#).unwrap();
        assert_eq!((old.draft, old.tried), (Draft::text("x"), true));
    }

    fn file(name: &str, bytes: &[u8]) -> PathBuf {
        let path = folder().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[tokio::test(start_paused = true)]
    async fn files_are_copied_uploaded_once_and_cleaned_up() {
        let (outbox, service, reports) = outbox(&[Next::Offline, Next::Offline]);
        *service.upload_failures.lock().unwrap() = 1;
        let (plan, notes) = (file("plan.pdf", b"%PDF"), file("notes.txt", b"hi"));
        outbox
            .add("c", &Draft::text("see"), &[plan.clone(), notes])
            .unwrap();
        // The original may go.
        std::fs::remove_file(plan).unwrap();
        run(&outbox, &reports, 120).await;
        assert_eq!(
            *service.uploads.lock().unwrap(),
            ["plan.pdf", "notes.txt"],
            "once each, in spite of retries"
        );
        assert_eq!(
            service.sent_files.lock().unwrap().last().unwrap(),
            &["plan.pdf", "notes.txt"]
        );
        assert_eq!(outbox.waiting(), 0);
        assert_eq!(
            std::fs::read_dir(&outbox.spool).unwrap().count(),
            0,
            "copies are gone"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn pictures_go_inside_and_need_no_text() {
        let (outbox, service, reports) = outbox(&[]);
        let picture = file("shot.png", b"\x89PNG\r\n\x1a\n....");
        // Named like a picture, but none.
        let fake = file("fake.png", b"MZ....");
        outbox.add("c", &Draft::text(""), &[picture, fake]).unwrap();
        run(&outbox, &reports, 10).await;
        assert_eq!(*service.uploads.lock().unwrap(), ["fake.png"]);
        assert_eq!(service.sent_files.lock().unwrap().len(), 1);
    }

    #[test]
    fn what_cannot_be_sent_is_refused_at_once() {
        let (outbox, ..) = outbox(&[]);
        let draft = Draft::text("x");
        assert!(
            outbox
                .add("c", &draft, &[PathBuf::from("/nonexistent/x")])
                .is_err()
        );
        assert!(outbox.add("c", &draft, &[folder()]).is_err(), "a folder");
        assert!(outbox.add("c", &draft, &[file("empty", b"")]).is_err());
        let many = vec![file("a", b"a"); MAX_FILES + 1];
        assert!(outbox.add("c", &draft, &many).is_err());
        assert_eq!(outbox.waiting(), 0);
        assert!(!outbox.spool.exists() || std::fs::read_dir(&outbox.spool).unwrap().count() == 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_given_up_message_can_be_tried_again_or_discarded() {
        let (outbox, service, reports) = outbox(&[Next::Forbidden, Next::Forbidden]);
        outbox
            .add("c", &Draft::text("one"), &[file("a.txt", b"a")])
            .unwrap();
        run(&outbox, &reports, 5).await;
        let id = outbox.store.outbox_for_conversation("c").unwrap()[0]
            .id
            .clone();
        assert!(outbox.retry("other").is_err());
        outbox.retry(&id).unwrap();
        run(&outbox, &reports, 5).await;
        assert_eq!(
            service.uploads.lock().unwrap().len(),
            1,
            "not uploaded again"
        );
        outbox.retry(&id).unwrap();
        run(&outbox, &reports, 5).await;
        assert_eq!(texts(&service), ["one"]);
        assert!(outbox.discard(&id).is_err(), "it was sent");
        service.plan.lock().unwrap().push(Next::Forbidden);
        outbox
            .add("c", &Draft::text("two"), &[file("b.txt", b"b")])
            .unwrap();
        run(&outbox, &reports, 5).await;
        let id = outbox.store.outbox_for_conversation("c").unwrap()[0]
            .id
            .clone();
        outbox.discard(&id).unwrap();
        assert_eq!(std::fs::read_dir(&outbox.spool).unwrap().count(), 0);
    }
}
