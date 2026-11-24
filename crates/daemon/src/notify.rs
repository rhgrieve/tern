//! Desktop notifications, with a button to open the message and, where the desktop can, an
//! answer field.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use tern_engine::Notice;
use tokio::sync::mpsc;
use zbus::Variant;

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Variant<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
    fn close_notification(&self, id: u32) -> zbus::Result<()>;
    fn get_capabilities(&self) -> zbus::Result<Vec<String>>;
    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn activation_token(&self, id: u32, token: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
    #[zbus(signal)]
    fn notification_replied(&self, id: u32, text: String) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.portal.OpenURI",
    default_service = "org.freedesktop.portal.Desktop",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait Portal {
    #[zbus(name = "OpenURI")]
    fn open_uri(
        &self,
        parent_window: &str,
        uri: &str,
        options: HashMap<&str, Variant<'_>>,
    ) -> zbus::Result<zbus::OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.portal.Background",
    default_service = "org.freedesktop.portal.Desktop",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait Background {
    fn request_background(
        &self,
        parent_window: &str,
        options: HashMap<&str, Variant<'_>>,
    ) -> zbus::Result<zbus::OwnedObjectPath>;
}

/// Tern's window. D-Bus starts it when it is not running, outside this service.
const WINDOW: &str = "io.github.rhgrieve.Tern";

#[zbus::proxy(
    interface = "org.freedesktop.Application",
    default_service = "io.github.rhgrieve.Tern",
    default_path = "/io/github/rhgrieve/Tern"
)]
trait Application {
    fn activate(&self, platform_data: HashMap<&str, Variant<'_>>) -> zbus::Result<()>;
    fn activate_action(
        &self,
        action_name: &str,
        parameter: Vec<Variant<'_>>,
        platform_data: HashMap<&str, Variant<'_>>,
    ) -> zbus::Result<()>;
}

const OPEN: &str = "default";
const REPLY: &str = "inline-reply";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub conversation: String,
    pub message: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// With the desktop's permission to take the focus, if it gave one.
    Open(Target, Option<String>),
    Reply {
        conversation: String,
        text: String,
    },
}

struct Shown {
    id: u32,
    message: String,
    at: DateTime<Utc>,
    count: usize,
}

/// The conversation, and the message if it is about reactions to that.
type Key = (String, Option<String>);

/// One notification per conversation, and one for the reactions to a message.
type Open = Arc<Mutex<HashMap<Key, Shown>>>;

fn key(notice: &Notice) -> Key {
    let message = notice.reaction.then(|| notice.message.clone());
    (notice.conversation.clone(), message)
}

#[derive(Clone, Copy, Default)]
struct Can {
    actions: bool,
    reply: bool,
    markup: bool,
}

pub struct Notifier {
    proxy: Option<(NotificationsProxy<'static>, Can)>,
    open: Open,
    actions: mpsc::UnboundedSender<Action>,
}

impl Notifier {
    pub fn start() -> (Self, mpsc::UnboundedReceiver<Action>) {
        let (actions, queue) = mpsc::unbounded_channel();
        let notifier = Notifier {
            proxy: None,
            open: Open::default(),
            actions,
        };
        (notifier, queue)
    }

    /// Connects on first use: the desktop's notification server may start after us.
    async fn server(&mut self) -> Option<(NotificationsProxy<'static>, Can)> {
        if self.proxy.is_none() {
            match connect(self.open.clone(), self.actions.clone()).await {
                Ok(connected) => self.proxy = Some(connected),
                Err(error) => tracing::warn!(%error, "no notifications"),
            }
        }
        self.proxy.clone()
    }

    pub async fn show(&mut self, notice: &Notice) {
        let Some((proxy, can)) = self.server().await else {
            return;
        };
        let (replaces, count) = {
            let open = self.open.lock().expect("notifications");
            open.get(&key(notice))
                .map_or((0, 1), |shown| (shown.id, shown.count + 1))
        };
        let title = match count {
            _ if notice.reaction => notice.title.clone(),
            1 => notice.title.clone(),
            count => format!("{} ({count} new)", notice.title),
        };
        let body = if can.markup {
            escape(&notice.text)
        } else {
            notice.text.clone()
        };
        let mut buttons = Vec::new();
        if can.actions {
            buttons.extend([OPEN, "Open"]);
            if can.reply && notice.can_reply {
                buttons.extend([REPLY, "Reply"]);
            }
        }
        let hints = HashMap::from([
            ("category", Variant::from("im.received")),
            ("desktop-entry", Variant::from(WINDOW)),
        ]);
        match proxy
            .notify(
                "Tern",
                replaces,
                "mail-message-new",
                &title,
                &body,
                &buttons,
                hints,
                -1,
            )
            .await
        {
            Ok(id) => {
                let shown = Shown {
                    id,
                    message: notice.message.clone(),
                    at: Utc::now(),
                    count,
                };
                let mut open = self.open.lock().expect("notifications");
                open.insert(key(notice), shown);
            }
            Err(error) => {
                tracing::warn!(%error, "notification");
                // The server may have restarted: connect again next time.
                self.proxy = None;
            }
        }
    }

    pub async fn failed(&mut self, reason: &str) {
        if let Some((proxy, can)) = self.server().await {
            let body = if can.markup {
                escape(reason)
            } else {
                reason.to_owned()
            };
            let _ = proxy
                .notify(
                    "Tern",
                    0,
                    "dialog-warning",
                    "Message not sent",
                    &body,
                    &[],
                    HashMap::new(),
                    -1,
                )
                .await;
        }
    }

    /// Closes notifications `read` answers `true` for.
    pub async fn close_read(&mut self, read: impl Fn(&str, DateTime<Utc>) -> bool) {
        let Some((proxy, _)) = &self.proxy else {
            return;
        };
        let done: Vec<u32> = {
            let mut open = self.open.lock().expect("notifications");
            let ids: Vec<Key> = open
                .iter()
                .filter(|((conversation, _), shown)| read(conversation, shown.at))
                .map(|(key, _)| key.clone())
                .collect();
            ids.iter()
                .filter_map(|id| open.remove(id))
                .map(|shown| shown.id)
                .collect()
        };
        for id in done {
            let _ = proxy.close_notification(id).await;
        }
    }
}

async fn connect(
    open: Open,
    actions: mpsc::UnboundedSender<Action>,
) -> zbus::Result<(NotificationsProxy<'static>, Can)> {
    let connection = zbus::Connection::session().await?;
    let proxy = NotificationsProxy::new(&connection).await?;
    let capabilities = proxy.get_capabilities().await?;
    let has = |name: &str| capabilities.iter().any(|capability| capability == name);
    let can = Can {
        actions: has("actions"),
        reply: has(REPLY),
        markup: has("body-markup"),
    };
    let mut invoked = proxy.receive_action_invoked().await?;
    let mut replied = proxy.receive_notification_replied().await?;
    let mut closed = proxy.receive_notification_closed().await?;
    let mut tokens = proxy.receive_activation_token().await?;
    tokio::spawn(async move {
        // Sent just before the action it is for.
        let mut token: Option<(u32, String)> = None;
        loop {
            // Signals name every application's notifications; only ours are in `open`.
            let action = tokio::select! {
                Some(signal) = invoked.next() => match signal.args() {
                    Ok(args) if args.action_key == OPEN => {
                        let given = token.take().filter(|(id, _)| *id == args.id).map(|(_, token)| token);
                        take(&open, args.id).map(|target| Action::Open(target, given))
                    }
                    _ => None,
                },
                Some(signal) = replied.next() => match signal.args() {
                    Ok(args) => take(&open, args.id).map(|target| Action::Reply {
                        conversation: target.conversation,
                        text: args.text.clone(),
                    }),
                    Err(_) => None,
                },
                Some(signal) = tokens.next() => {
                    if let Ok(args) = signal.args() {
                        token = Some((args.id, args.token.clone()));
                    }
                    None
                }
                Some(signal) = closed.next() => {
                    if let Ok(args) = signal.args() {
                        // An answer can follow its notification's close.
                        let (open, id) = (open.clone(), args.id);
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            take(&open, id);
                        });
                    }
                    None
                }
                else => return,
            };
            if let Some(action) = action
                && actions.send(action).is_err()
            {
                return;
            }
        }
    });
    Ok((proxy, can))
}

fn take(open: &Open, id: u32) -> Option<Target> {
    let mut open = open.lock().expect("notifications");
    let key = open
        .iter()
        .find(|(_, shown)| shown.id == id)
        .map(|(key, _)| key.clone())?;
    let shown = open.remove(&key)?;
    Some(Target {
        conversation: key.0,
        message: shown.message,
    })
}

/// Message text is someone else's: it must not become markup.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn link(target: &Target) -> String {
    let encode = |text: &str| utf8_percent_encode(text, NON_ALPHANUMERIC).to_string();
    if target.message.is_empty() {
        return format!(
            "https://teams.microsoft.com/l/chat/{}/conversations",
            encode(&target.conversation)
        );
    }
    format!(
        "https://teams.microsoft.com/l/message/{}/{}?context=%7B%22contextType%22%3A%22chat%22%7D",
        encode(&target.conversation),
        encode(&target.message)
    )
}

fn platform(token: Option<&str>) -> HashMap<&str, Variant<'_>> {
    token
        .map(|token| ("activation-token", Variant::from(token)))
        .into_iter()
        .collect()
}

/// Nobody knows how to start the window out of an image, so this does, unless it is there.
async fn wake(connection: &zbus::Connection) -> zbus::Result<()> {
    if tern_ipc::home() != tern_ipc::Home::AppImage {
        return Ok(());
    }
    let bus = zbus::fdo::DBusProxy::new(connection).await?;
    let there = async || bus.name_has_owner(WINDOW.try_into()?).await;
    if there().await? {
        return Ok(());
    }
    let mut window = tokio::process::Command::new(tern_ipc::program("tern"))
        .arg("--gapplication-service")
        .spawn()
        .map_err(|error| zbus::Error::Failure(error.to_string()))?;
    tokio::spawn(async move { window.wait().await });
    for _ in 0..50 {
        if there().await? {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

/// In a sandbox, asks the desktop to start the service at the next login.
pub async fn at_login() {
    if !matches!(tern_ipc::home(), tern_ipc::Home::Flatpak(_)) {
        return;
    }
    let asked = async {
        let connection = zbus::Connection::session().await?;
        let options = HashMap::from([
            (
                "reason",
                Variant::from("Keep your Teams status true and tell you about messages"),
            ),
            ("autostart", Variant::from(true)),
            ("commandline", Variant::from(vec!["ternd"])),
        ]);
        BackgroundProxy::new(&connection)
            .await?
            .request_background("", options)
            .await
    };
    if let Err(error) = asked.await {
        tracing::info!(%error, "will not start by itself at the next login");
    }
}

/// Opens Tern's window, or Teams on the web where there is none.
pub async fn open_window() {
    let shown = async {
        let connection = zbus::Connection::session().await?;
        wake(&connection).await?;
        ApplicationProxy::new(&connection)
            .await?
            .activate(platform(None))
            .await
    };
    if let Err(error) = shown.await {
        tracing::info!(%error, window = WINDOW, "no window; opening Teams on the web");
        open_address("https://teams.microsoft.com/").await;
    }
}

/// Opens a conversation in Tern's window, or in Teams on the web.
pub async fn open(target: &Target, token: Option<String>) {
    let shown = async {
        let connection = zbus::Connection::session().await?;
        wake(&connection).await?;
        let parameter = vec![Variant::from(target.conversation.as_str())];
        ApplicationProxy::new(&connection)
            .await?
            .activate_action("open", parameter, platform(token.as_deref()))
            .await
    };
    if let Err(error) = shown.await {
        tracing::info!(%error, window = WINDOW, "no window; opening Teams on the web");
        open_address(&link(target)).await;
    }
}

async fn open_address(address: &str) {
    let opened = async {
        let connection = zbus::Connection::session().await?;
        let portal = PortalProxy::new(&connection).await?;
        portal.open_uri("", address, HashMap::new()).await
    };
    if let Err(error) = opened.await {
        tracing::warn!(%error, "cannot open the browser");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shows a real notification and closes it: `cargo test -p ternd -- --ignored desktop`.
    #[tokio::test]
    #[ignore = "needs a desktop"]
    async fn the_desktop_shows_updates_and_closes_one_notification() {
        let (mut notifier, _actions) = Notifier::start();
        let mut notice = Notice {
            conversation: "c".into(),
            message: "1".into(),
            title: "Tern test".into(),
            text: "This closes by itself".into(),
            can_reply: true,
            reaction: false,
        };
        notifier.show(&notice).await;
        let first = notifier.open.lock().unwrap()[&("c".to_owned(), None)].id;
        notice.message = "2".into();
        notifier.show(&notice).await;
        let (second, count) = {
            let open = notifier.open.lock().unwrap();
            (
                open[&("c".to_owned(), None)].id,
                open[&("c".to_owned(), None)].count,
            )
        };
        assert!(first > 0);
        assert_eq!((first, count), (second, 2), "replaced, not stacked");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        notifier.close_read(|_, _| true).await;
        assert!(notifier.open.lock().unwrap().is_empty());
    }

    #[test]
    fn text_cannot_become_markup() {
        assert_eq!(escape("<a href=\"x\">&"), "&lt;a href=\"x\"&gt;&amp;");
    }

    #[test]
    fn a_link_cannot_be_broken_out_of() {
        let link = link(&Target {
            conversation: "19:a@thread.v2/../x?y#z".into(),
            message: "1".into(),
        });
        assert!(link.starts_with(
            "https://teams.microsoft.com/l/message/19%3Aa%40thread%2Ev2%2F%2E%2E%2Fx%3Fy%23z/1?"
        ));
    }

    #[test]
    fn only_our_own_notifications_lead_somewhere() {
        let open = Open::default();
        open.lock().unwrap().insert(
            ("c".into(), Some("m".into())),
            Shown {
                id: 7,
                message: "m".into(),
                at: Utc::now(),
                count: 1,
            },
        );
        assert_eq!(take(&open, 8), None);
        assert_eq!(
            take(&open, 7),
            Some(Target {
                conversation: "c".into(),
                message: "m".into()
            })
        );
        assert_eq!(take(&open, 7), None);
    }
}
