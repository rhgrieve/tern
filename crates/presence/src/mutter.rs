//! GNOME: Mutter has no `ext-idle-notify-v1`, but answers the idle time over D-Bus.

use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

#[zbus::proxy(
    interface = "org.gnome.Mutter.IdleMonitor",
    default_service = "org.gnome.Mutter.IdleMonitor",
    default_path = "/org/gnome/Mutter/IdleMonitor/Core"
)]
trait IdleMonitor {
    fn get_idletime(&self) -> zbus::Result<u64>;
}

const SAMPLE: Duration = Duration::from_secs(5);

/// Runs until Mutter stops answering.
pub(crate) async fn run(timeout: Duration, output: UnboundedSender<bool>) -> zbus::Result<()> {
    let connection = zbus::Connection::session().await?;
    let monitor = IdleMonitorProxy::new(&connection).await?;
    let mut last = None;
    loop {
        let idle = Duration::from_millis(monitor.get_idletime().await?) >= timeout;
        if last != Some(idle) {
            last = Some(idle);
            if output.send(idle).is_err() {
                return Ok(());
            }
        }
        tokio::time::sleep(SAMPLE).await;
    }
}
