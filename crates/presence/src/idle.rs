//! System-wide input idleness on Wayland, through `ext-idle-notify-v1`.

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::{self, ExtIdleNotifierV1},
};

const INPUT_IDLE_SINCE: u32 = 2;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("no Wayland display: {0}")]
    NoDisplay(String),
    #[error("the compositor has no ext-idle-notify-v1")]
    Unsupported,
    #[error("no input seat")]
    NoSeat,
    #[error("lost the Wayland connection: {0}")]
    Lost(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    /// `exact` when video players and other idle inhibitors do not count as presence.
    Ready {
        exact: bool,
    },
    Idle(bool),
}

struct App {
    timeout: u32,
    notifier: Option<ExtIdleNotifierV1>,
    /// Per seat: its notification, and whether it is idle.
    seats: HashMap<u32, Seat>,
    idle: bool,
    output: UnboundedSender<Signal>,
}

struct Seat {
    seat: wl_seat::WlSeat,
    notification: Option<ExtIdleNotificationV1>,
    idle: bool,
}

impl App {
    fn watch(&mut self, queue: &QueueHandle<App>) {
        let Some(notifier) = &self.notifier else {
            return;
        };
        for (name, seat) in &mut self.seats {
            if seat.notification.is_none() {
                seat.notification = Some(if notifier.version() >= INPUT_IDLE_SINCE {
                    notifier.get_input_idle_notification(self.timeout, &seat.seat, queue, *name)
                } else {
                    notifier.get_idle_notification(self.timeout, &seat.seat, queue, *name)
                });
            }
        }
    }

    /// The user is away only when every seat is.
    fn publish(&mut self) {
        let idle = !self.seats.is_empty() && self.seats.values().all(|seat| seat.idle);
        if idle != self.idle {
            self.idle = idle;
            let _ = self.output.send(Signal::Idle(idle));
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        app: &mut App,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<App>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                if interface == wl_seat::WlSeat::interface().name {
                    let seat = registry.bind(name, version.min(1), queue, ());
                    app.seats.insert(
                        name,
                        Seat {
                            seat,
                            notification: None,
                            idle: false,
                        },
                    );
                    app.watch(queue);
                    app.publish();
                } else if interface == ExtIdleNotifierV1::interface().name {
                    app.notifier =
                        Some(registry.bind(name, version.min(INPUT_IDLE_SINCE), queue, ()));
                    app.watch(queue);
                }
            }
            wl_registry::Event::GlobalRemove { name } => {
                if let Some(seat) = app.seats.remove(&name) {
                    if let Some(notification) = seat.notification {
                        notification.destroy();
                    }
                    app.publish();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtIdleNotificationV1, u32> for App {
    fn event(
        app: &mut App,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        let idle = match event {
            ext_idle_notification_v1::Event::Idled => true,
            ext_idle_notification_v1::Event::Resumed => false,
            _ => return,
        };
        if let Some(seat) = app.seats.get_mut(name) {
            seat.idle = idle;
        }
        app.publish();
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(
        _: &mut App,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for App {
    fn event(
        _: &mut App,
        _: &ExtIdleNotifierV1,
        _: ext_idle_notifier_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

/// Blocks until the connection is lost or nobody listens any more.
pub(crate) fn run(timeout: Duration, output: UnboundedSender<Signal>) -> Result<(), Error> {
    let connection =
        Connection::connect_to_env().map_err(|error| Error::NoDisplay(error.to_string()))?;
    let mut queue = connection.new_event_queue::<App>();
    let handle = queue.handle();
    connection.display().get_registry(&handle, ());
    let mut app = App {
        timeout: u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX),
        notifier: None,
        seats: HashMap::new(),
        idle: false,
        output,
    };
    let lost = |error: wayland_client::DispatchError| Error::Lost(error.to_string());
    queue.roundtrip(&mut app).map_err(lost)?;
    let notifier = app.notifier.as_ref().ok_or(Error::Unsupported)?;
    if app.seats.is_empty() {
        return Err(Error::NoSeat);
    }
    let exact = notifier.version() >= INPUT_IDLE_SINCE;
    let _ = app.output.send(Signal::Ready { exact });
    // A new notification starts as "not idle". If the user is in fact away, it idles after the
    // timeout, so a restart can show them as active for at most that long.
    let _ = app.output.send(Signal::Idle(false));
    while !app.output.is_closed() {
        queue.blocking_dispatch(&mut app).map_err(lost)?;
    }
    Ok(())
}
