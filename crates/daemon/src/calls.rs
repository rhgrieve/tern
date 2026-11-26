//! Who is calling. Calls are told on a push connection of their own: it has to register as something
//! that can ring, and the one for messages should not depend on that.
//!
//! This connection does not answer: it keeps the push, and `tern-call`, started by the window,
//! answers with it. It does not join the call, so nobody tells it when the ringing ends either:
//! that is noticed in the chat, or assumed after a while.

use std::collections::VecDeque;
use std::io::Read;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chatsvc::realtime::{Realtime, RealtimeConfig};
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use serde_json::Value;
use session::Session;
use tokio::sync::{mpsc, oneshot};

/// Notifications about calls, of one person and of a group.
const CALLS: [u64; 2] = [107, 109];
const MAX_INFLATED: u64 = 1024 * 1024;
const MAX_NAME: usize = 120;
const REMEMBERED: usize = 20;
/// The longest push that is kept for answering.
const LONGEST_PUSH: usize = 256 * 1024;
/// Teams stops ringing after about this long.
pub const RINGING: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ring {
    pub call: String,
    /// The caller as the chat service knows people: their object id.
    pub from: Option<String>,
    pub name: String,
    /// The chat of a group call.
    pub chat: Option<String>,
    pub subject: Option<String>,
    pub video: bool,
    /// The push itself, which `tern-call` answers or declines with. Only one line.
    pub push: Option<String>,
}

fn text(value: &Value, pointer: &str) -> Option<String> {
    let text = value.pointer(pointer)?.as_str()?.trim();
    let clean: String = text
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME)
        .collect();
    (!clean.is_empty()).then_some(clean)
}

/// An id, whole or not at all.
fn id(value: &Value, pointer: &str) -> Option<String> {
    let id = value.pointer(pointer)?.as_str()?;
    (!id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control)).then(|| id.to_owned())
}

fn inflated(bytes: &[u8]) -> Option<Value> {
    fn read(reader: impl Read) -> Option<Value> {
        serde_json::from_reader(reader.take(MAX_INFLATED)).ok()
    }
    read(ZlibDecoder::new(bytes))
        .or_else(|| read(DeflateDecoder::new(bytes)))
        .or_else(|| read(GzDecoder::new(bytes)))
}

/// A call, if that is what a push is about.
fn ring(body: &str) -> Option<Ring> {
    let push: Value = serde_json::from_str(body).ok()?;
    if !CALLS.contains(&push.get("evt")?.as_u64()?) {
        return None;
    }
    let bytes = |key: &str| STANDARD.decode(push.get(key)?.as_str()?.trim()).ok();
    let payload: Value = match bytes("gp") {
        Some(plain) => serde_json::from_slice(&plain).ok()?,
        None => inflated(&bytes("cp")?)?,
    };
    let from = id(&payload, "/callNotification/from/id")?;
    let call = id(&payload, "/debugContent/callId")
        .or_else(|| id(&payload, "/callNotification/links/attach"))?;
    Some(Ring {
        call,
        name: text(&payload, "/callNotification/from/displayName")
            .unwrap_or_else(|| "Someone".into()),
        from: from.strip_prefix("8:orgid:").map(str::to_ascii_lowercase),
        chat: id(&payload, "/groupChat/threadId"),
        subject: text(&payload, "/conversationInvitation/subject"),
        video: payload
            .pointer("/callNotification/callModalities")
            .and_then(Value::as_array)
            .is_some_and(|all| all.iter().any(|one| one == "Video")),
        push: (body.len() <= LONGEST_PUSH && !body.contains('\n')).then(|| body.to_owned()),
    })
}

enum Command {
    Activity(bool),
    Stop(oneshot::Sender<()>),
}

pub struct Calls {
    commands: mpsc::UnboundedSender<Command>,
}

impl Calls {
    pub async fn start(
        session: &Session,
        host: Option<String>,
        active: bool,
        rings: mpsc::UnboundedSender<Ring>,
    ) -> Result<Calls, String> {
        let config = RealtimeConfig {
            host,
            discover_host: false,
            initially_active: active,
            native: true,
            ringable: true,
            forward_callbacks: true,
            ..RealtimeConfig::default()
        };
        let mut realtime = Realtime::start_with(session, config)
            .await
            .map_err(|error| error.to_string())?;
        let mut callbacks = realtime.take_callbacks().ok_or("no callbacks")?;
        let (commands, mut queue) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            // The service repeats itself.
            let mut known: VecDeque<String> = VecDeque::new();
            loop {
                tokio::select! {
                    // Messages come on the other connection.
                    event = realtime.recv() => if event.is_none() { return },
                    callback = callbacks.recv() => {
                        let Some(callback) = callback else { return };
                        let Some(ring) = ring(&callback.body) else { continue };
                        if known.contains(&ring.call) {
                            continue;
                        }
                        known.push_back(ring.call.clone());
                        if known.len() > REMEMBERED {
                            known.pop_front();
                        }
                        if rings.send(ring).is_err() {
                            return;
                        }
                    }
                    command = queue.recv() => match command {
                        Some(Command::Activity(active)) => realtime.set_activity(active),
                        Some(Command::Stop(done)) => {
                            realtime.stop().await;
                            let _ = done.send(());
                            return;
                        }
                        // Dropped.
                        None => return,
                    },
                }
            }
        });
        Ok(Calls { commands })
    }

    /// The same as on the other connection, or they would contradict each other.
    pub fn set_activity(&self, active: bool) {
        let _ = self.commands.send(Command::Activity(active));
    }

    pub async fn stop(self) {
        let (done, stopped) = oneshot::channel();
        if self.commands.send(Command::Stop(done)).is_ok() {
            let _ = stopped.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};
    use serde_json::json;
    use std::io::Write;

    fn payload() -> Value {
        json!({
            "callNotification": {
                "from": {"id": "8:orgid:ADA-1", "displayName": "Ada Quill"},
                "links": {"attach": "https://example.com/attach"},
                "callModalities": ["Audio", "Video"],
            },
            "debugContent": {"callId": "call-1"},
        })
    }

    fn push(kind: u64, key: &str, bytes: &[u8]) -> String {
        json!({"evt": kind, key: STANDARD.encode(bytes)}).to_string()
    }

    fn plain(payload: &Value) -> String {
        push(107, "gp", payload.to_string().as_bytes())
    }

    #[test]
    fn a_call_names_its_caller() {
        let ring = ring(&plain(&payload())).unwrap();
        assert_eq!(ring.call, "call-1");
        assert_eq!(ring.name, "Ada Quill");
        assert_eq!(ring.from.as_deref(), Some("ada-1"));
        assert_eq!(ring.chat, None);
        assert!(ring.video);
        assert_eq!(ring.push.as_deref(), Some(plain(&payload()).as_str()));
    }

    #[test]
    fn every_kind_of_compression_is_read() {
        let text = payload().to_string();
        let mut zlib = ZlibEncoder::new(Vec::new(), Compression::default());
        zlib.write_all(text.as_bytes()).unwrap();
        let mut deflate = DeflateEncoder::new(Vec::new(), Compression::default());
        deflate.write_all(text.as_bytes()).unwrap();
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(text.as_bytes()).unwrap();
        for bytes in [
            zlib.finish().unwrap(),
            deflate.finish().unwrap(),
            gzip.finish().unwrap(),
        ] {
            assert!(ring(&push(109, "cp", &bytes)).is_some());
        }
    }

    #[test]
    fn a_group_call_has_its_chat() {
        let mut group = payload();
        group["groupChat"] = json!({"threadId": "19:g@thread.v2"});
        group["conversationInvitation"] = json!({"subject": "Harbour\u{7} launch"});
        let ring = ring(&plain(&group)).unwrap();
        assert_eq!(ring.chat.as_deref(), Some("19:g@thread.v2"));
        assert_eq!(ring.subject.as_deref(), Some("Harbour launch"));
    }

    #[test]
    fn anything_else_does_not_ring() {
        let mut nobody = payload();
        nobody["callNotification"]["from"] = json!({});
        for body in [
            "",
            "null",
            "{}",
            "{\"evt\":107}",
            "{\"evt\":107,\"gp\":\"!\"}",
            "{\"evt\":107,\"cp\":\"AAAA\"}",
            &plain(&nobody),
            &push(110, "gp", payload().to_string().as_bytes()),
        ] {
            assert_eq!(ring(body), None, "{body}");
        }
    }

    #[test]
    fn a_bomb_is_not_inflated_to_the_end() {
        let mut zlib = ZlibEncoder::new(Vec::new(), Compression::best());
        zlib.write_all(b"\"").unwrap();
        for _ in 0..64 {
            zlib.write_all(&[b'a'; 65536]).unwrap();
        }
        zlib.write_all(b"\"").unwrap();
        assert_eq!(inflated(&zlib.finish().unwrap()), None);
    }
}
