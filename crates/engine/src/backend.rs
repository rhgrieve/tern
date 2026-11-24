//! What sync and the outbox need from the Teams engine, replaced in tests.

use std::sync::Arc;
use std::time::Duration;

use chatsvc::with_client_message_id;
use chrono::{DateTime, Utc};
use graph::Graph;
use session::{Guard, Method, Session};
use store::{ChatRecord, MessageRecord, Store};
use teams_core::{
    Error, FileReference, HostedImage, ImageRef, MentionInput, MessageExtras, Remote, SyncEngine,
};

/// How a failed call should be handled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Trying again will not help.
    Permanent(String),
    Temporary(String),
}

impl Failure {
    pub fn text(&self) -> &str {
        match self {
            Failure::Permanent(text) | Failure::Temporary(text) => text,
        }
    }
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        // Addresses hold conversation ids; the status says enough.
        let session = match &error {
            Error::Graph(graph::Error::Session(inner)) => Some(inner),
            _ => None,
        };
        match (&error, session) {
            (_, Some(session::Error::Api { status, .. })) => {
                let text = format!("HTTP {status}");
                if (400..500).contains(status) && !matches!(status, 401 | 408 | 429) {
                    Failure::Permanent(text)
                } else {
                    Failure::Temporary(text)
                }
            }
            (
                _,
                Some(
                    session::Error::Refused { .. }
                    | session::Error::PageOnly(_)
                    | session::Error::TokenHostRefused { .. },
                ),
            )
            | (Error::Graph(graph::Error::NotAMember) | Error::Unsupported(_), _) => {
                Failure::Permanent(error.to_string())
            }
            (Error::UnknownConversation(_), _) => Failure::Permanent("unknown conversation".into()),
            (Error::Io(_), _) => Failure::Permanent(error.to_string()),
            _ => Failure::Temporary(error.to_string().chars().take(200).collect()),
        }
    }
}

pub trait Backend: Send + Sync + 'static {
    /// Reads the chat list.
    fn refresh(&self) -> impl Future<Output = Result<(), Failure>> + Send;
    /// Reads what is new in a conversation and answers it.
    fn fetch(
        &self,
        conversation: &str,
    ) -> impl Future<Output = Result<Vec<MessageRecord>, Failure>> + Send;
    /// Like `fetch`, until `message` is cached: the service announces a message a moment before
    /// it can be read.
    fn fetch_until(
        &self,
        conversation: &str,
        message: &str,
    ) -> impl Future<Output = Result<Vec<MessageRecord>, Failure>> + Send;
    /// Reads one changed message again.
    fn reread(
        &self,
        conversation: &str,
        message: &str,
    ) -> impl Future<Output = Result<(), Failure>> + Send;
    fn cached(&self, conversation: &str, message: &str) -> bool;
    /// Recent chats whose cached messages end before their last message.
    fn behind(&self) -> Vec<String>;
    /// Asks the service which cached conversations have something new; quicker than `refresh`.
    fn changed(&self) -> impl Future<Output = Result<Vec<String>, Failure>> + Send;
    /// A chat whose cached history does not reach `since` yet.
    fn shallow(&self, since: DateTime<Utc>, skip: &[String]) -> Option<String>;
    /// Reads one page of older messages.
    fn older(&self, conversation: &str) -> impl Future<Output = Result<(), Failure>> + Send;
}

const ARRIVAL: [Duration; 4] = [
    Duration::from_millis(400),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];
/// Chats kept complete.
const RECENT: usize = 50;

pub struct Teams {
    engine: SyncEngine<Graph>,
}

impl Teams {
    pub fn new(session: Session, store: Arc<Store>) -> Arc<Self> {
        Arc::new(Teams {
            engine: SyncEngine::new(Graph::new(session), store),
        })
    }

    /// With pictures kept as files in `folder`.
    pub fn with_pictures(
        session: Session,
        store: Arc<Store>,
        folder: &std::path::Path,
    ) -> Result<Arc<Self>, String> {
        let engine = SyncEngine::new(Graph::new(session), store)
            .with_image_dir(folder, crate::media::IMAGE_BYTES)
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(Teams { engine }))
    }

    pub fn engine(&self) -> &SyncEngine<Graph> {
        &self.engine
    }

    pub fn store(&self) -> &Arc<Store> {
        self.engine.store()
    }

    fn recent(&self) -> Vec<ChatRecord> {
        self.store().recent_chats(RECENT).unwrap_or_default()
    }
}

impl Backend for Teams {
    async fn refresh(&self) -> Result<(), Failure> {
        self.engine.refresh_sidebar().await?;
        Ok(())
    }

    async fn fetch(&self, conversation: &str) -> Result<Vec<MessageRecord>, Failure> {
        Ok(self.engine.fetch_newer(conversation).await?.added)
    }

    async fn fetch_until(
        &self,
        conversation: &str,
        message: &str,
    ) -> Result<Vec<MessageRecord>, Failure> {
        let delta = self
            .engine
            .fetch_until_cached(conversation, message, &ARRIVAL)
            .await?;
        Ok(delta.added)
    }

    async fn reread(&self, conversation: &str, message: &str) -> Result<(), Failure> {
        self.engine.refresh_message(conversation, message).await?;
        Ok(())
    }

    fn cached(&self, conversation: &str, message: &str) -> bool {
        self.store()
            .messages_by_id(conversation, &[message.to_owned()])
            .is_ok_and(|found| !found.is_empty())
    }

    fn behind(&self) -> Vec<String> {
        self.recent()
            .into_iter()
            .filter(|chat| {
                let seen = self
                    .store()
                    .sync_state(&chat.id)
                    .ok()
                    .flatten()
                    .and_then(|state| state.newest_seen);
                match (seen, chat.last_message_at) {
                    (None, _) => true,
                    (Some(seen), Some(last)) => seen < last,
                    (Some(_), None) => false,
                }
            })
            .map(|chat| chat.id)
            .collect()
    }

    async fn changed(&self) -> Result<Vec<String>, Failure> {
        let (states, _) = self.engine.remote().recent_chat_states().await?;
        Ok(states
            .into_iter()
            .filter(|state| {
                let seen = self
                    .store()
                    .sync_state(&state.id)
                    .ok()
                    .flatten()
                    .and_then(|state| state.newest_seen);
                matches!((seen, state.read.last_message_at()), (Some(seen), Some(last)) if seen < last)
            })
            .map(|state| state.id)
            .collect())
    }

    fn shallow(&self, since: DateTime<Utc>, skip: &[String]) -> Option<String> {
        self.recent()
            .into_iter()
            .map(|chat| chat.id)
            .filter(|id| !skip.contains(id))
            .find(|id| {
                self.store()
                    .sync_state(id)
                    .ok()
                    .flatten()
                    .is_some_and(|state| {
                        state.has_more && state.oldest_loaded.is_some_and(|oldest| oldest > since)
                    })
            })
    }

    async fn older(&self, conversation: &str) -> Result<(), Failure> {
        self.engine.load_older(conversation).await?;
        Ok(())
    }
}

/// A person, or everyone in a chat, who goes by the chat's own id.
fn mentioned(conversation: &str, id: &str, name: &str) -> MentionInput {
    if id == conversation {
        MentionInput::everyone(conversation, name)
    } else {
        MentionInput::user(id, name)
    }
}

impl crate::outbox::Post for Teams {
    async fn post(
        &self,
        conversation: &str,
        draft: &crate::outbox::Draft,
        client_id: &str,
    ) -> Result<String, Failure> {
        let mentions: Vec<MentionInput> = draft
            .mentions
            .iter()
            .map(|(id, name)| mentioned(conversation, id, name))
            .collect();
        let mut extras = MessageExtras::default();
        for file in &draft.files {
            if let Some(reference) = &file.reference {
                extras.files.push(reference.clone());
            } else if file.picture {
                let bytes = std::fs::read(&file.path)
                    .map_err(|error| Failure::Permanent(format!("{}: {error}", file.name)))?;
                let content_type = crate::outbox::picture_type(&bytes)
                    .ok_or_else(|| Failure::Permanent(format!("{} is not a picture", file.name)))?;
                extras.images.push(HostedImage {
                    content_type: content_type.to_owned(),
                    bytes: Arc::new(bytes),
                });
            }
        }
        let html = teams_core::markdown_to_html(&draft.markdown);
        let engine = &self.engine;
        let send = async {
            match (&draft.reply_to, &draft.thread, draft.post) {
                (Some(quoted), _, _) => {
                    engine
                        .reply_to_with_extras(conversation, quoted, &html, &mentions, &extras)
                        .await
                }
                (None, Some(root), _) => {
                    engine
                        .send_message_with_extras(
                            conversation,
                            &html,
                            Some(root),
                            &mentions,
                            &extras,
                        )
                        .await
                }
                (None, None, true) => {
                    let subject = draft
                        .subject
                        .as_deref()
                        .filter(|subject| !subject.trim().is_empty());
                    engine
                        .post_to_channel_with_extras(
                            conversation,
                            &html,
                            subject,
                            &mentions,
                            &extras,
                        )
                        .await
                }
                (None, None, false) => {
                    engine
                        .send_message_with_extras(conversation, &html, None, &mentions, &extras)
                        .await
                }
            }
        };
        Ok(with_client_message_id(client_id.to_owned(), send)
            .await?
            .message_id)
    }

    async fn upload(
        &self,
        conversation: &str,
        file: &crate::outbox::Attached,
    ) -> Result<FileReference, Failure> {
        let bytes = tokio::fs::read(&file.path)
            .await
            .map_err(|error| Failure::Permanent(format!("{}: {error}", file.name)))?;
        let uploaded = self
            .engine
            .upload_attachment_file(conversation, &file.name, &bytes, |_| {})
            .await?;
        if let Err(error) = self.engine.share_attachment(conversation, &uploaded).await {
            // Nobody could open it: take it back, so that the next try starts clean.
            let _ = self.engine.discard_attachment(&uploaded).await;
            return Err(error.into());
        }
        uploaded
            .reference()
            .ok_or_else(|| Failure::Permanent("the uploaded file has no id".into()))
    }

    async fn own_since(
        &self,
        conversation: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<MessageRecord>, Failure> {
        self.engine.fetch_newer(conversation).await?;
        let me = self.engine.me().map(|me| me.user_id);
        let messages = self
            .store()
            .messages(conversation, None, 50)
            .map_err(|error| Failure::Temporary(error.to_string()))?;
        Ok(messages
            .into_iter()
            .filter(|message| {
                message.created_at >= since
                    && !message.deleted
                    && message.sender_id.is_some()
                    && message.sender_id == me
            })
            .collect())
    }
}

/// What the user does to a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    React { emoji: String, on: bool },
    Edit { markdown: String },
    Delete,
}

impl Teams {
    pub async fn act(&self, conversation: &str, message: &str, act: &Act) -> Result<(), Failure> {
        let engine = &self.engine;
        match act {
            Act::React { emoji, on: true } => {
                engine.set_reaction(conversation, message, emoji).await?
            }
            Act::React { emoji, on: false } => {
                engine.unset_reaction(conversation, message, emoji).await?
            }
            Act::Edit { markdown } => engine.edit_message(conversation, message, markdown).await?,
            Act::Delete => engine.soft_delete_message(conversation, message).await?,
        }
        Ok(())
    }

    /// Reads what is new, for a conversation that is being opened.
    pub async fn look(&self, conversation: &str, post: Option<&str>) -> Result<(), Failure> {
        match post {
            Some(post) => drop(self.engine.open_post(conversation, post).await?),
            None => drop(self.engine.fetch_newer(conversation).await?),
        }
        Ok(())
    }

    /// Reads older messages; answers whether any came.
    pub async fn back(&self, conversation: &str) -> Result<bool, Failure> {
        Ok(!self.engine.load_older(conversation).await?.is_empty())
    }
}

impl crate::media::Pictures for Teams {
    fn has(&self, image: &ImageRef) -> bool {
        self.engine.image_path(image).is_some()
    }

    async fn fetch(&self, image: &ImageRef) -> Result<(), Failure> {
        self.engine.fetch_image(image).await?;
        Ok(())
    }

    async fn avatars(&self, people: &[String]) -> Result<(), Failure> {
        self.engine.fetch_avatars(people).await?;
        Ok(())
    }
}

impl crate::media::FileSource for Teams {
    async fn download(
        &self,
        url: &str,
        write: impl FnMut(&[u8]) -> std::io::Result<()> + Send,
    ) -> Result<String, Failure> {
        let shared = self.engine.download_file(url, write, |_| {}).await?;
        Ok(shared.name)
    }
}

/// POST addresses that only read.
const READING: [&str; 6] = [
    "/presence/getpresence/",
    "/users/fetchShortProfile",
    "/users/fetch",
    "/users/fetchFederated",
    "/api/authsvc/v1.0/authz",
    "graph.microsoft.com/v1.0/$batch",
];

/// Lets only reading requests through; for trying sync on a real account.
pub fn read_only_guard() -> Guard {
    Arc::new(|request| match request.method {
        Method::Get => true,
        Method::Post => {
            READING.iter().any(|path| request.url.contains(path)) && batch_only_reads(request)
        }
        _ => false,
    })
}

/// A Graph batch can hold writes.
fn batch_only_reads(request: &session::Request) -> bool {
    if !request.url.contains("/$batch") {
        return true;
    }
    request
        .body
        .as_ref()
        .and_then(|body| body["requests"].as_array())
        .is_some_and(|requests| requests.iter().all(|request| request["method"] == "GET"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use session::Request;

    fn post(url: &str, body: serde_json::Value) -> Request {
        Request::with_body(Method::Post, url, body)
    }

    #[test]
    fn the_guard_lets_only_reads_through() {
        let allows = read_only_guard();
        let chats = "https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations";
        assert!(allows(&Request::get(chats)));
        assert!(allows(&post(
            "https://teams.microsoft.com/ups/noam/v1/presence/getpresence/",
            json!([])
        )));
        assert!(!allows(&post(
            &format!("{chats}/48%3Anotes/messages"),
            json!({})
        )));
        assert!(!allows(&Request::delete(chats)));
        assert!(!allows(&Request::with_body(Method::Put, chats, json!({}))));
        let batch = "https://graph.microsoft.com/v1.0/$batch";
        assert!(allows(&post(
            batch,
            json!({"requests": [{"method": "GET"}]})
        )));
        assert!(!allows(&post(
            batch,
            json!({"requests": [{"method": "GET"}, {"method": "POST"}]})
        )));
        assert!(!allows(&post(batch, json!({}))));
    }

    #[test]
    fn the_offered_reactions_are_known_to_teams() {
        for emoji in ["👍", "❤️", "❤", "😆", "😮", "🙁", "😠"] {
            assert!(chatsvc::reaction_key(emoji).is_some(), "{emoji}");
            assert!(!chatsvc::reaction_keys(emoji).is_empty(), "{emoji}");
        }
    }

    #[test]
    fn mentions_survive_markdown_and_odd_names() {
        let people = [
            ("1", "Sam O'Brien"),
            ("2", "Zoë <Ops> & Co"),
            ("3", "Li_Wei"),
        ];
        let inputs: Vec<MentionInput> = people
            .iter()
            .map(|(id, name)| MentionInput::user(id, name))
            .collect();
        let text = "**@Sam O'Brien** and @Zoë <Ops> & Co, then @Li_Wei!";
        let (html, mentions) =
            teams_core::apply_mentions(&teams_core::markdown_to_html(text), &inputs);
        assert_eq!(mentions.len(), 3, "{html}");
        assert!(!html.contains("<Ops>"), "{html}");
        // Inside code it is text.
        let code = teams_core::markdown_to_html("`@Sam O'Brien`");
        let (html, _) = teams_core::apply_mentions(&code, &inputs[..1]);
        println!("{html}");
    }

    #[test]
    fn everyone_is_the_chat_itself() {
        let everyone = mentioned("19:g@thread.v2", "19:g@thread.v2", "Everyone");
        assert_eq!(
            everyone,
            MentionInput::everyone("19:g@thread.v2", "Everyone")
        );
        let html = teams_core::markdown_to_html("@Everyone, lunch?");
        let (html, mentions) = teams_core::apply_mentions(&html, &[everyone]);
        assert_eq!(mentions.len(), 1, "{html}");
        assert_eq!(
            mentioned("19:g@thread.v2", "u1", "Ann"),
            MentionInput::user("u1", "Ann")
        );
    }

    #[test]
    fn failures_are_sorted_and_hide_addresses() {
        let api = |status| {
            Error::Graph(graph::Error::Session(session::Error::api(
                status,
                "https://x/19%3Asecret",
                json!({}),
            )))
        };
        assert_eq!(
            Failure::from(api(503)),
            Failure::Temporary("HTTP 503".into())
        );
        assert_eq!(
            Failure::from(api(429)),
            Failure::Temporary("HTTP 429".into())
        );
        assert_eq!(
            Failure::from(api(401)),
            Failure::Temporary("HTTP 401".into())
        );
        assert_eq!(
            Failure::from(api(403)),
            Failure::Permanent("HTTP 403".into())
        );
    }
}
