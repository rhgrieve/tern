//! Message sync, the outbox and notification rules for the signed-in account.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chatsvc::realtime::{EventKind, MessageEvent};
use chrono::{DateTime, Utc};
use session::Session;
use store::{MessageRecord, Store};
use teams_core::CoreEvent;
use teams_core::Presence;
use tern_engine::{
    Arrived, Draft, FILE_BYTES, Files, Job, Notice, Outbox, Queued, Rules, Sync, SyncState, Teams,
    Want, database_path, media_usage, open_store, people, pictures, prefetch,
};
use tern_ipc::{Availability, Event};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{Notify, broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout_at};

pub enum News {
    Arrived(Arrived),
    /// Messages of the conversation are not as they were.
    Changed {
        conversation: String,
    },
    /// Somebody came or went.
    Presence,
    /// The calendar was read.
    Agenda,
    Failed {
        reason: String,
    },
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
    agenda: Arc<Notify>,
}

impl Drop for Messages {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

const RECENT_CHATS: usize = 50;
/// How far back the chat with somebody who calls is looked for.
const CHATS_WITH: usize = 1000;
/// Chats whose pictures are kept ready, and how many messages of each are looked at.
const PICTURE_CHATS: usize = 20;
const PICTURE_MESSAGES: usize = 60;
/// Reactions to messages further back are not told.
const REACTED: usize = 200;
/// At most this many messages are looked at for a window.
const LOOKED_FOR: usize = 2000;
const PEOPLE: usize = 200;
/// How often in that time a pinned chat is read that is not in the chat list.
const PIN_TRIES: u32 = 2;
const PINS_EVERY: Duration = Duration::from_secs(5 * 60);
/// Nobody tells when the calendar changes.
const AGENDA_EVERY: Duration = Duration::from_secs(5 * 60);
/// Asked for in a row, it is read once.
const AGENDA_PAUSE: Duration = Duration::from_secs(5);
const MEDIA_EVERY: Duration = Duration::from_secs(30 * 60);

pub fn presence<'a>(
    teams: &Teams,
    people: impl IntoIterator<Item = &'a String>,
) -> Vec<(String, Availability)> {
    people
        .into_iter()
        .filter_map(|person| {
            let known = teams.engine().presence(person)?;
            Some((person.clone(), availability(known)?))
        })
        .collect()
}

fn availability(presence: Presence) -> Option<Availability> {
    use teams_core::{Activity, Availability as Known};
    let out = presence.activity == Some(Activity::OutOfOffice);
    Some(match presence.availability {
        Known::Available => Availability::Available,
        Known::Busy => Availability::Busy,
        Known::DoNotDisturb => Availability::DoNotDisturb,
        Known::Away | Known::Offline | Known::Unknown if out => Availability::OutOfOffice,
        Known::Away => Availability::Away,
        Known::Offline => Availability::Offline,
        Known::Unknown => return None,
    })
}

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

/// Reads which chats are pinned, after the first sync and now and then: nobody tells.
async fn read_pins(
    mut state: watch::Receiver<SyncState>,
    teams: Arc<Teams>,
    changes: broadcast::Sender<Event>,
) {
    loop {
        if state.wait_for(|state| state.refreshed).await.is_err() {
            return;
        }
        // Before anything can change.
        let mut core = teams.engine().subscribe();
        match teams.read_pins().await {
            Ok(true) => drop(changes.send(Event::Chats)),
            Ok(false) => {}
            Err(failure) => tracing::debug!(error = failure.text(), "pinned chats"),
        }
        let mut tries: HashMap<String, u32> = HashMap::new();
        let next = Instant::now() + PINS_EVERY;
        loop {
            for id in teams.missing_pins() {
                let tried = tries.entry(id.clone()).or_default();
                *tried += 1;
                // The service may list it as gone, and give it all the same.
                if *tried <= PIN_TRIES
                    && let Err(failure) = teams.read_chat(&id).await
                {
                    tracing::debug!(error = failure.text(), "a pinned chat");
                }
            }
            // A new chat list may have dropped one.
            let dropped = async {
                loop {
                    match core.recv().await {
                        Ok(CoreEvent::SidebarChanged) | Err(RecvError::Lagged(_)) => return,
                        Ok(_) => {}
                        Err(RecvError::Closed) => std::future::pending().await,
                    }
                }
            };
            if timeout_at(next, dropped).await.is_err() {
                break;
            }
        }
    }
}

/// Reads the calendar after the first sync, now and then, and when asked.
async fn read_agenda(
    mut state: watch::Receiver<SyncState>,
    teams: Arc<Teams>,
    asked: Arc<Notify>,
    changes: broadcast::Sender<Event>,
    news: mpsc::UnboundedSender<News>,
) {
    if state.wait_for(|state| state.refreshed).await.is_err() {
        return;
    }
    loop {
        match teams.read_agenda(Utc::now()).await {
            Ok(changed) => {
                // Also when it did not: how old it is did.
                let _ = changes.send(Event::Agenda);
                if changed && news.send(News::Agenda).is_err() {
                    return;
                }
            }
            Err(failure) => tracing::debug!(error = failure.text(), "calendar"),
        }
        tokio::time::sleep(AGENDA_PAUSE).await;
        let _ = tokio::time::timeout(AGENDA_EVERY, asked.notified()).await;
    }
}

/// Passes on what the engine changed in the cache.
async fn tell(
    mut core: broadcast::Receiver<CoreEvent>,
    changes: broadcast::Sender<Event>,
    news: mpsc::UnboundedSender<News>,
) {
    loop {
        let event = match core.recv().await {
            Ok(CoreEvent::SidebarChanged) => Event::Chats,
            Ok(
                CoreEvent::MessagesChanged { conversation_id }
                | CoreEvent::MessagesStoredLate {
                    conversation_id, ..
                },
            ) => {
                let _ = news.send(News::Changed {
                    conversation: conversation_id.clone(),
                });
                Event::Messages {
                    conversation: conversation_id,
                }
            }
            Ok(CoreEvent::ReceiptsChanged { conversation_id }) => Event::Messages {
                conversation: conversation_id,
            },
            Ok(CoreEvent::ImagesChanged { .. }) => Event::Pictures,
            Ok(CoreEvent::AvatarsChanged { .. }) => Event::Faces,
            Ok(CoreEvent::PresenceChanged) => {
                let _ = news.send(News::Presence);
                continue;
            }
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
        let (sent, moved) = (news.clone(), changes.clone());
        let agenda = Arc::new(Notify::new());
        let tasks = vec![
            tokio::spawn(read_agenda(
                state.clone(),
                teams.clone(),
                agenda.clone(),
                changes.clone(),
                news.clone(),
            )),
            tokio::spawn(prefetch(teams.clone(), wanted)),
            tokio::spawn(read_pins(state.clone(), teams.clone(), changes.clone())),
            tokio::spawn(tell(core, changes, news.clone())),
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
                let conversation = match queued {
                    Queued::Failed {
                        reason,
                        conversation,
                    } => {
                        let _ = sent.send(News::Failed { reason });
                        conversation
                    }
                    Queued::Sent { conversation } => conversation,
                };
                // It is no longer in the outbox.
                let _ = moved.send(Event::Messages { conversation });
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
            agenda,
        })
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    pub fn event(&self, event: MessageEvent) {
        if event.kind == EventKind::ReadReceipt {
            let (teams, event) = (self.teams.clone(), event.clone());
            tokio::spawn(async move { teams.engine().handle_receipt_event(&event).await });
        }
        let _ = self.jobs.send(Job::Event(event));
    }

    pub fn catch_up(&self) {
        let _ = self.jobs.send(Job::CatchUp);
        self.outbox.wake();
        self.read_agenda();
    }

    pub fn read_agenda(&self) {
        self.agenda.notify_one();
    }

    /// The calendar as it was last read.
    pub fn meetings(&self) -> Vec<tern_engine::Meeting> {
        tern_engine::agenda(&self.store)
            .map(|agenda| agenda.meetings)
            .unwrap_or_default()
    }

    /// The chat of two with this person.
    pub fn chat_with(&self, person: &str) -> Option<String> {
        let chats = self.store.recent_chats(CHATS_WITH).ok()?;
        let with = |chat: &store::ChatRecord| {
            chat.kind.eq_ignore_ascii_case("oneOnOne")
                && chat.members.iter().any(|member| {
                    member
                        .user_id
                        .as_deref()
                        .is_some_and(|id| id.eq_ignore_ascii_case(person))
                })
        };
        chats.into_iter().find(with).map(|chat| chat.id)
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

    /// Tells the others. Not worth a retry.
    pub fn typing(&self, conversation: String, active: bool) {
        let teams = self.teams.clone();
        tokio::spawn(async move {
            let _ = teams
                .engine()
                .send_typing(&conversation, None, active)
                .await;
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

    /// Pictures somebody is looking for, where fetching them tells nobody anything.
    pub fn want_pictures(&self, conversation: &str, messages: usize) {
        let messages = self
            .store
            .messages(conversation, None, messages.min(LOOKED_FOR))
            .unwrap_or_default();
        // Oldest first: the newest request is served first.
        let images = messages.iter().flat_map(pictures).collect();
        let _ = self.want.send(Want::Pictures(images));
    }

    /// Whether these people are there, as far as it is known.
    pub fn presence<'a>(
        &self,
        people: impl IntoIterator<Item = &'a String>,
    ) -> Vec<(String, Availability)> {
        presence(&self.teams, people)
    }

    /// What others think of the user's messages in a conversation that changed.
    pub fn reactions(&mut self, conversation: &str) -> Vec<Notice> {
        let messages = self
            .store
            .messages(conversation, None, REACTED)
            .unwrap_or_default();
        let chat = self.store.chat(conversation).ok().flatten();
        let store = self.store.clone();
        let name = |id: &str| {
            let member = chat.iter().flat_map(|chat| &chat.members).find(|member| {
                member
                    .user_id
                    .as_deref()
                    .is_some_and(|known| known.eq_ignore_ascii_case(id))
            });
            let name = member.map(|member| member.display_name.clone());
            name.or_else(|| {
                let people = store.conversation_people(conversation, PEOPLE).ok()?;
                people
                    .into_iter()
                    .find(|(known, _)| known.eq_ignore_ascii_case(id))
                    .map(|(_, name)| name)
            })
            .filter(|name| !name.trim().is_empty())
        };
        self.rules
            .reactions(&messages, chat.as_ref(), &self.account, name, Utc::now())
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
