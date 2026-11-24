//! Lock, sleep and session switching, from logind.

use futures_util::StreamExt;
use tokio::sync::mpsc::UnboundedSender;
use zbus::{OwnedFd, OwnedObjectPath};

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Manager {
    fn get_session(&self, id: &str) -> zbus::Result<OwnedObjectPath>;
    fn get_user(&self, uid: u32) -> zbus::Result<OwnedObjectPath>;
    fn inhibit(&self, what: &str, who: &str, why: &str, mode: &str) -> zbus::Result<OwnedFd>;
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
    #[zbus(signal)]
    fn prepare_for_shutdown(&self, start: bool) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.User",
    default_service = "org.freedesktop.login1"
)]
trait User {
    #[zbus(property)]
    fn display(&self) -> zbus::Result<(String, OwnedObjectPath)>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait Session {
    #[zbus(property)]
    fn locked_hint(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn active(&self) -> zbus::Result<bool>;
}

#[derive(Debug)]
pub(crate) enum Signal {
    Ready,
    Locked(bool),
    Foreground(bool),
    /// Sleep, or shutdown, waits until the guard is dropped (logind allows a few seconds).
    Sleep(SleepGuard),
    Resume,
}

/// Holds sleep back while it lives.
#[derive(Debug)]
pub struct SleepGuard {
    _lock: Option<OwnedFd>,
}

async fn inhibit(manager: &ManagerProxy<'_>) -> Option<OwnedFd> {
    manager
        .inhibit("sleep:shutdown", "Tern", "Show as away in Teams", "delay")
        .await
        .inspect_err(|error| tracing::warn!(%error, "sleep will not wait for the away status"))
        .ok()
}

fn uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").ok().map(|proc| proc.uid())
}

/// A user service is not part of a session, so the graphical session is looked up by user.
async fn session_path(
    connection: &zbus::Connection,
    manager: &ManagerProxy<'_>,
) -> zbus::Result<OwnedObjectPath> {
    if let Ok(id) = std::env::var("XDG_SESSION_ID")
        && let Ok(path) = manager.get_session(&id).await
    {
        return Ok(path);
    }
    let uid = uid().ok_or_else(|| zbus::Error::Failure("no user id".into()))?;
    let user = UserProxy::builder(connection)
        .path(manager.get_user(uid).await?)
        .build()
        .await?;
    let (id, path) = user.display().await?;
    if id.is_empty() {
        return Err(zbus::Error::Failure("no graphical session".into()));
    }
    Ok(path)
}

/// Runs until the bus is lost or nobody listens any more.
pub(crate) async fn run(output: UnboundedSender<Signal>) -> zbus::Result<()> {
    let connection = zbus::Connection::system().await?;
    let manager = ManagerProxy::new(&connection).await?;
    let session = SessionProxy::builder(&connection)
        .path(session_path(&connection, &manager).await?)
        .build()
        .await?;
    let mut sleeps = manager.receive_prepare_for_sleep().await?;
    let mut shutdowns = manager.receive_prepare_for_shutdown().await?;
    let mut hints = session.receive_locked_hint_changed().await;
    let mut actives = session.receive_active_changed().await;
    let mut guard = inhibit(&manager).await;
    let _ = output.send(Signal::Ready);
    let _ = output.send(Signal::Locked(session.locked_hint().await.unwrap_or(false)));
    let _ = output.send(Signal::Foreground(session.active().await.unwrap_or(true)));
    loop {
        let signal = tokio::select! {
            Some(sleep) = sleeps.next() => {
                if sleep.args()?.start {
                    Signal::Sleep(SleepGuard { _lock: guard.take() })
                } else {
                    guard = inhibit(&manager).await;
                    Signal::Resume
                }
            }
            // Like sleep: the network is still there, which it may not be when we are stopped.
            Some(shutdown) = shutdowns.next() => {
                if shutdown.args()?.start {
                    Signal::Sleep(SleepGuard { _lock: guard.take() })
                } else {
                    guard = inhibit(&manager).await;
                    Signal::Resume
                }
            }
            // Only the hint is trusted: the Lock signal is a request, and a locker that never
            // runs or never answers it would leave the user shown as away for good.
            Some(hint) = hints.next() => match hint.get().await { Ok(locked) => Signal::Locked(locked), Err(_) => continue },
            Some(active) = actives.next() => match active.get().await { Ok(active) => Signal::Foreground(active), Err(_) => continue },
            else => return Err(zbus::Error::Failure("logind stopped answering".into())),
        };
        if output.send(signal).is_err() {
            return Ok(());
        }
    }
}
