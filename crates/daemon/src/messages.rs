//! Message sync, the outbox and notification rules for the signed-in account.

use std::sync::Arc;
use std::time::Duration;

use chatsvc::realtime::MessageEvent;
use chrono::{DateTime, Utc};
use session::Session;
use store::{MessageRecord, Store};
use teams_core::CoreEvent;
use tern_engine::{
    Arrived, Draft, FILE_BYTES, Files, Job, Notice, Outbox, Queued, Rules, Sync, SyncState, Teams,
    Want, database_path, media_usage, open_store, people, pictures, prefetch,
};
use tern_ipc::Event;
use tokio::sync::{broadcast, mpsc, watch};
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
    teams: Arc<Teams>,
    files: Arc<Files>,
    media: std::path::PathBuf,
    want: mpsc::UnboundedSender<Want>,
}

impl Drop for Messages {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

const RECENT_CHATS: usize = 50;
/// Chats whose pictures are kept ready, and how many messages of each are looked at.
const PICTURE_CHATS: usize = 20;
const PICTURE_MESSAGES: usize = 60;
const PEOPLE: usize = 200;
const MEDIA_EVERY: Duration = Duration::from_secs(30 * 60);

/// The files of a message that can be downloaded.
pub fn downloadable(message: &MessageRecord) -> Vec<teams_core::FileCard> {
    teams_core::files(message)
        .into_iter()
        .filter(|file| file.kind != teams_core::FileKind::Loop)
        .filter(|file| file.open_url.starts_with("https://"))
        .collect()
}

/// Asks for the pictures and faces of recent chats, after the first sync and now and then.
async fn want_recent(
    mut state: watch::Receiver<SyncState>,
    store: Arc<Store>,
    want: mpsc::UnboundedSender<Want>,
) {
    loop {
        if state
            .wait_for(|state| state.refreshed && state.pending == 0)
            .await
            .is_err()
        {
            return;
        }
        let chats = store.recent_chats(PICTURE_CHATS).unwrap_or_default();
        let mut images = Vec::new();
        let mut seen = Vec::new();
        // Oldest first: the newest request is served first.
        for chat in chats.iter().rev() {
            let messages = store
                .messages(&chat.id, None, PICTURE_MESSAGES)
                .unwrap_or_default();
            images.extend(messages.iter().flat_map(pictures));
            seen.extend(messages.into_iter().filter_map(|message| message.sender_id));
            seen.extend(
                chat.members
                    .iter()
                    .take(10)
                    .filter_map(|member| member.user_id.clone()),
            );
        }
        let faces = people(seen.iter().map(String::as_str), PEOPLE);
        if want.send(Want::Pictures(images)).is_err() || want.send(Want::Avatars(faces)).is_err() {
            return;
        }
        tokio::time::sleep(MEDIA_EVERY).await;
    }
}

/// Passes on what the engine changed in the cache.
async fn tell(mut core: broadcast::Receiver<CoreEvent>, changes: broadcast::Sender<Event>) {
    loop {
        let event = match core.recv().await {
            Ok(CoreEvent::SidebarChanged) => Event::Chats,
            Ok(
                CoreEvent::MessagesChanged { conversation_id }
                | CoreEvent::MessagesStoredLate {
                    conversation_id, ..
                },
            ) => Event::Messages {
                conversation: conversation_id,
            },
            Ok(CoreEvent::ImagesChanged { .. }) => Event::Pictures,
            Ok(CoreEvent::AvatarsChanged { .. }) => Event::Faces,
            Ok(_) => continue,
            // Some were missed; the list says which conversations moved.
            Err(broadcast::error::RecvError::Lagged(_)) => Event::Chats,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        let _ = changes.send(event);
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
        changes: broadcast::Sender<Event>,
    ) -> Result<Self, String> {
        let path = database_path(account).map_err(|error| error.to_string())?;
        let store = Arc::new(open_store(&path)?);
        let media = path.with_file_name("media");
        let files =
            Files::open(&media.join("files"), FILE_BYTES).map_err(|error| error.to_string())?;
        let teams = Teams::with_pictures(session, store.clone(), &media.join("pictures"))?;
        let (want, wanted) = mpsc::unbounded_channel();
        let core = teams.engine().subscribe();
        let (jobs, job_queue) = mpsc::unbounded_channel();
        let (arrived, mut arrivals) = mpsc::unbounded_channel();
        let (sync, state) = Sync::new(teams.clone(), job_queue, arrived);
        let outbox = Outbox::new(
            teams.clone(),
            store.clone(),
            &path.with_file_name("outgoing"),
        );
        let sent = news.clone();
        let tasks = vec![
            tokio::spawn(prefetch(teams.clone(), wanted)),
            tokio::spawn(tell(core, changes)),
            tokio::spawn(want_recent(state.clone(), store.clone(), want.clone())),
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
            teams,
            files: Arc::new(files),
            media,
            want,
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

    pub fn teams(&self) -> Arc<Teams> {
        self.teams.clone()
    }

    pub fn outbox(&self) -> &Outbox<Teams> {
        &self.outbox
    }

    pub fn send(
        &self,
        conversation: &str,
        draft: &Draft,
        files: &[std::path::PathBuf],
    ) -> Result<(), String> {
        self.outbox.add(conversation, draft, files)
    }

    /// Marks a conversation read in Teams, which its members can see.
    pub fn mark_read(&self, conversation: String) {
        let teams = self.teams.clone();
        tokio::spawn(async move {
            if let Err(error) = teams.engine().mark_read(&conversation).await {
                let failure = tern_engine::Failure::from(error);
                tracing::debug!(error = failure.text(), "mark read");
            }
        });
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

    /// Unread chats that are not muted, newest first: id and title.
    pub fn unread(&self) -> Vec<(String, String)> {
        let chats = self.store.recent_chats(RECENT_CHATS).unwrap_or_default();
        chats
            .into_iter()
            .filter(|chat| chat.unread && !chat.muted)
            .map(|chat| (chat.id.clone(), chat.display_title()))
            .collect()
    }

    /// The newest message of a chat, to open it at.
    pub fn newest(&self, conversation: &str) -> Option<String> {
        let messages = self.store.messages(conversation, None, 1).ok()?;
        messages
            .into_iter()
            .next_back()
            .map(|message| message.message_id)
    }

    pub fn media(&self) -> (usize, u64) {
        media_usage(&self.media)
    }

    /// Gets a file of a message. The address comes from the cached message, never from the asker.
    pub fn file(
        &self,
        conversation: &str,
        message: &str,
        index: usize,
    ) -> impl Future<Output = Result<std::path::PathBuf, String>> + Send + 'static {
        let url = self
            .store
            .messages_by_id(conversation, &[message.to_owned()])
            .ok()
            .and_then(|mut found| found.remove(message))
            .and_then(|message| downloadable(&message).into_iter().nth(index))
            .map(|file| file.open_url);
        let (files, teams) = (self.files.clone(), self.teams.clone());
        async move {
            match url {
                Some(url) => files.get(&url, &*teams).await,
                None => Err("there is no such file".into()),
            }
        }
    }

    pub fn notices(&mut self, arrived: &Arrived) -> Vec<Notice> {
        let images: Vec<_> = arrived.messages.iter().flat_map(pictures).collect();
        let _ = self.want.send(Want::Pictures(images));
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
