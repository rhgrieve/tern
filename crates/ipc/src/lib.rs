//! The private socket between the daemon and its clients: one JSON line each way.

use std::io;
use std::path::{Path, PathBuf};
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
    /// The user is writing in a conversation, or stopped.
    Typing {
        conversation: String,
        active: bool,
    },
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
        /// People mentioned as `@name`, in the order of the text: id and name.
        #[serde(default)]
        mentions: Vec<(String, String)>,
        /// Copied at once.
        #[serde(default)]
        files: Vec<PathBuf>,
    },
    /// Try a message again that could not be sent, or forget it.
    Retry {
        id: String,
    },
    Discard {
        id: String,
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
    /// Fetch the pictures of the newest messages of a conversation that are not here yet.
    Pictures {
        conversation: String,
        messages: usize,
    },
    /// Whether these people are there, now and from now on.
    Presence {
        people: Vec<String>,
    },
    /// Fetch one picture.
    Picture {
        url: String,
    },
    /// A conversation is in front of the user, or no longer. Forgotten after a minute.
    Viewing {
        conversation: String,
        on: bool,
    },
    /// Read how far the others have read.
    Receipts {
        conversation: String,
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
    /// Whether people are there, of those that were asked for.
    Presence {
        people: Vec<(String, Availability)>,
    },
    /// Someone started or stopped writing.
    Typing {
        conversation: String,
        name: String,
        active: bool,
    },
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
    Presence(Vec<(String, Availability)>),
    Error(String),
}

/// What Teams says of a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Available,
    Busy,
    DoNotDisturb,
    Away,
    Offline,
    OutOfOffice,
}

/// How these programs came here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Home {
    /// systemd and D-Bus know them.
    Installed,
    /// One file, which knows nobody.
    AppImage,
    /// A sandbox; its own name.
    Flatpak(String),
}

pub fn home() -> Home {
    let flatpak = std::env::var("FLATPAK_ID").ok();
    let image = std::env::var_os("APPDIR").map(PathBuf::from);
    home_of(std::env::current_exe().ok().as_deref(), flatpak, image)
}

/// Programs started by some other sandboxed program or image inherit its variables, so those alone
/// say nothing.
fn home_of(program: Option<&Path>, flatpak: Option<String>, image: Option<PathBuf>) -> Home {
    let within = |folder: &Path| program.is_some_and(|program| program.starts_with(folder));
    match (flatpak, image) {
        (Some(name), _) if within(Path::new("/app")) => Home::Flatpak(name),
        (_, Some(folder)) if folder.is_absolute() && within(&folder) => Home::AppImage,
        _ => Home::Installed,
    }
}

/// Another of these programs.
pub fn program(name: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .map(|path| path.with_file_name(name))
        .filter(|path| path.exists())
        .unwrap_or_else(|| name.into())
}

/// What lasts until the user logs out, for all of these programs.
pub fn runtime_folder() -> io::Result<PathBuf> {
    let mut runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))?;
    // The part that all sandboxes of the same program have in common.
    if let Home::Flatpak(name) = home() {
        runtime.extend(["app", &name]);
    }
    // A development instance runs beside the installed one.
    let folder = match std::env::var("TERN_INSTANCE") {
        Ok(name) if name.bytes().all(|byte| byte.is_ascii_alphanumeric()) && !name.is_empty() => {
            format!("tern-{name}")
        }
        _ => "tern".to_owned(),
    };
    Ok(runtime.join(folder))
}

pub fn socket_path() -> io::Result<PathBuf> {
    Ok(runtime_folder()?.join("ternd.sock"))
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

    #[test]
    fn only_programs_of_the_sandbox_or_image_itself_belong_to_it() {
        let of = |program: &str, flatpak: Option<&str>, image: Option<&str>| {
            home_of(
                Some(Path::new(program)),
                flatpak.map(str::to_owned),
                image.map(PathBuf::from),
            )
        };
        let mount = Some("/tmp/.mount_tern1");
        assert_eq!(of("/home/a/.local/bin/tern", None, None), Home::Installed);
        assert_eq!(
            of("/tmp/.mount_tern1/bin/tern", None, mount),
            Home::AppImage
        );
        assert_eq!(
            of("/app/bin/tern", Some("a.b.C"), None),
            Home::Flatpak("a.b.C".into())
        );
        // Started from a terminal that is an image, or a sandbox with the whole computer in it.
        assert_eq!(of("/home/a/.local/bin/tern", None, mount), Home::Installed);
        assert_eq!(
            of("/home/a/.local/bin/tern", Some("a.b.C"), Some("")),
            Home::Installed
        );
    }

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
