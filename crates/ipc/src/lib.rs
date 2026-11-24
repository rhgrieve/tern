//! The private socket between the daemon and its clients: one JSON line each way.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

const MAX_LINE: u64 = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Status,
    /// The stored sign-in changed.
    Reload,
    /// Follow changes: answered with one `Event` per line until the connection ends.
    Watch,
    /// The user has seen a conversation.
    MarkRead {
        conversation: String,
    },
    /// Downloads a file of a message, or finds its copy; answers the path.
    File {
        conversation: String,
        message: String,
        index: usize,
    },
    /// Queues a message; it is sent in the background.
    Send {
        conversation: String,
        text: String,
        #[serde(default)]
        reply_to: Option<String>,
        #[serde(default)]
        thread: Option<String>,
        #[serde(default)]
        post: bool,
        #[serde(default)]
        subject: Option<String>,
    },
    React {
        conversation: String,
        message: String,
        emoji: String,
        on: bool,
    },
    Edit {
        conversation: String,
        message: String,
        text: String,
    },
    Delete {
        conversation: String,
        message: String,
    },
    /// A conversation, or one post of a channel, is being opened: read what is new.
    Look {
        conversation: String,
        post: Option<String>,
    },
    /// Read older messages than the cache has.
    Older {
        conversation: String,
    },
}

/// What changed. Only names things: the content is in the cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Sent first, and whenever the connection or account changes.
    Status {
        status: Box<Status>,
    },
    Chats,
    Messages {
        conversation: String,
    },
    Pictures,
    Faces,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub account: Option<String>,
    /// Names the cache of the account.
    pub account_id: Option<String>,
    /// What this computer says: active, idle, locked, sleeping or in_call.
    pub state: String,
    pub state_for_secs: u64,
    pub idle_source: Option<String>,
    pub lock_source: bool,
    pub push: String,
    pub registered: bool,
    /// What Teams shows to others, as last read back.
    pub teams_shows: Option<String>,
    pub teams_read_secs_ago: Option<u64>,
    pub problem: Option<String>,
    /// The chat list was read since the start.
    pub synced: bool,
    /// Conversations still to be read.
    pub reading: usize,
    /// Messages not sent yet.
    pub unsent: usize,
    pub sync_problem: Option<String>,
    /// Pictures and files kept, and their bytes.
    pub media: (usize, u64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    Status(Box<Status>),
    Done,
    Path(std::path::PathBuf),
    Error(String),
}

pub fn socket_path() -> io::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))?;
    // A development instance runs beside the installed one.
    let folder = match std::env::var("TERN_INSTANCE") {
        Ok(name) if name.bytes().all(|byte| byte.is_ascii_alphanumeric()) && !name.is_empty() => {
            format!("tern-{name}")
        }
        _ => "tern".to_owned(),
    };
    Ok(PathBuf::from(runtime).join(folder).join("ternd.sock"))
}

/// Binds the socket. Fails when another daemon is answering on it.
pub async fn bind() -> io::Result<UnixListener> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let path = socket_path()?;
    if let Some(folder) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(folder)?;
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))?;
    }
    if UnixStream::connect(&path).await.is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "ternd is already running",
        ));
    }
    match std::fs::remove_file(&path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Only the same user may talk to the daemon, whatever the file permissions say.
pub fn same_user(stream: &UnixStream) -> bool {
    use std::os::unix::fs::MetadataExt;
    let own = std::fs::metadata("/proc/self").map(|proc| proc.uid());
    matches!((own, stream.peer_cred()), (Ok(own), Ok(peer)) if own == peer.uid())
}

async fn line<T: for<'a> Deserialize<'a>>(stream: &mut UnixStream) -> io::Result<T> {
    let mut text = String::new();
    BufReader::new(stream)
        .take(MAX_LINE)
        .read_line(&mut text)
        .await?;
    serde_json::from_str(&text).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

async fn send<T: Serialize>(stream: &mut UnixStream, value: &T) -> io::Result<()> {
    let mut text = serde_json::to_string(value)?;
    text.push('\n');
    stream.write_all(text.as_bytes()).await
}

pub async fn receive(stream: &mut UnixStream) -> io::Result<Request> {
    tokio::time::timeout(TIMEOUT, line(stream)).await?
}

pub async fn respond(stream: &mut UnixStream, response: &Response) -> io::Result<()> {
    tokio::time::timeout(TIMEOUT, send(stream, response)).await?
}

pub async fn ask(request: &Request) -> io::Result<Response> {
    ask_within(request, TIMEOUT).await
}

/// Follows changes until the daemon goes away.
pub async fn watch(mut on: impl FnMut(Event)) -> io::Result<()> {
    let mut stream = UnixStream::connect(socket_path()?).await?;
    send(&mut stream, &Request::Watch).await?;
    let mut lines = BufReader::new(stream).lines();
    while let Some(line) = lines.next_line().await? {
        // An event a newer daemon added is not for us.
        if let Ok(event) = serde_json::from_str(&line) {
            on(event);
        }
    }
    Ok(())
}

pub async fn send_event(stream: &mut UnixStream, event: &Event) -> io::Result<()> {
    tokio::time::timeout(TIMEOUT, send(stream, event)).await?
}

/// For requests that take long, like downloads.
pub async fn ask_within(request: &Request, limit: Duration) -> io::Result<Response> {
    let mut stream = UnixStream::connect(socket_path()?).await?;
    tokio::time::timeout(limit, async {
        send(&mut stream, request).await?;
        line(&mut stream).await
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_request_gets_its_answer() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        assert!(same_user(&server));
        send(&mut client, &Request::Reload).await.unwrap();
        assert_eq!(receive(&mut server).await.unwrap(), Request::Reload);
        respond(&mut server, &Response::Done).await.unwrap();
        assert_eq!(line::<Response>(&mut client).await.unwrap(), Response::Done);
    }

    #[tokio::test]
    async fn junk_is_an_error() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        client.write_all(b"{\"cmd\":\"rm\"}\n").await.unwrap();
        assert!(receive(&mut server).await.is_err());
    }
}
