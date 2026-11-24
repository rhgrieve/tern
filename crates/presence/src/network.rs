//! Tells when the machine got a (new) way online, so the push connection need not wait for a timeout.

use futures_util::StreamExt;
use tokio::sync::mpsc::UnboundedSender;
use zbus::OwnedObjectPath;

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    #[zbus(property)]
    fn primary_connection(&self) -> zbus::Result<OwnedObjectPath>;
}

/// Sends once per change to a usable network. Without NetworkManager this simply ends: the push
/// connection still notices a dead link by itself, only later.
pub(crate) async fn run(output: UnboundedSender<()>) -> zbus::Result<()> {
    let connection = zbus::Connection::system().await?;
    let manager = NetworkManagerProxy::new(&connection).await?;
    let mut current = manager.primary_connection().await?;
    let mut changes = manager.receive_primary_connection_changed().await;
    while let Some(change) = changes.next().await {
        let Ok(next) = change.get().await else {
            continue;
        };
        let online = next.as_str() != "/";
        if next != current && online && output.send(()).is_err() {
            return Ok(());
        }
        current = next;
    }
    Ok(())
}
