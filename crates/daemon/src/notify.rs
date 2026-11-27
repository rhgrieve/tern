//! Desktop notifications, with a button to open the message and, where the desktop can, an
//! answer field.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use futures_util::StreamExt;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use tern_engine::{Notice, Reminder, Stage};
use tokio::sync::mpsc;
use zbus::Variant;

use crate::calls::Ring;

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

/// Where Teams itself can answer a call.
const TEAMS: &str = "https://teams.microsoft.com/v2/";

const OPEN: &str = "default";
const REPLY: &str = "inline-reply";
/// Why a notification closed: the user put it away.
const DISMISSED: u32 = 2;

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
    Go(Place, Option<String>),
    /// The user put away the notification of a call: it rings no more.
    Silence,
}

/// Where else a notification leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    Agenda,
    /// A meeting, at its address.
    Meeting(String),
    Chat(String),
    /// Teams in the browser.
    Teams,
    /// Answering a call in Tern, with the push that told of it.
    Answer(String),
    /// Declining a call, for all the user's devices.
    Decline(String),
}

impl Place {
    /// What was chosen, for the log: never the chat, meeting or call itself.
    pub fn choice(&self) -> &'static str {
        match self {
            Place::Agenda => "the calendar",
            Place::Meeting(_) => "join the meeting",
            Place::Chat(_) => "open the chat",
            Place::Teams => "Teams on the web",
            Place::Answer(_) => "answer",
            Place::Decline(_) => "decline",
        }
    }
}

/// A notification that is not about a message.
struct Aside {
    id: u32,
    /// What its buttons do, by their names.
    choices: Vec<(&'static str, Place)>,
}

/// By what they are about: a meeting, or a call.
type Asides = Arc<Mutex<HashMap<String, Aside>>>;

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
    asides: Asides,
    actions: mpsc::UnboundedSender<Action>,
}

/// What a notification that is not about a message says.
struct Note<'a> {
    about: String,
    icon: &'a str,
    title: String,
    body: String,
    category: &'a str,
    /// Stays, and is shown above others.
    urgent: bool,
    sound: Option<&'a str>,
    choices: Vec<(&'static str, &'a str, Place)>,
}

fn clock(time: DateTime<Utc>) -> String {
    time.with_timezone(&Local).format("%H:%M").to_string()
}

fn reminder(reminder: &Reminder, now: DateTime<Utc>) -> Note<'static> {
    let meeting = &reminder.meeting;
    let name = match meeting.subject.as_str() {
        "" => "Meeting",
        subject => subject,
    };
    // Rounded: told a few seconds late, it is still in ten minutes.
    let minutes = ((meeting.start - now).num_seconds() + 30) / 60;
    let title = match (reminder.stage, minutes) {
        (Stage::Now, _) => format!("{name} begins"),
        (Stage::Soon, ..1) => format!("{name} is about to begin"),
        (Stage::Soon, 1) => format!("{name} in a minute"),
        (Stage::Soon, 2..120) => format!("{name} in {minutes} minutes"),
        (Stage::Soon, _) => format!("{name} at {}", clock(meeting.start)),
    };
    let mut body = format!("{} to {}", clock(meeting.start), clock(meeting.end));
    for more in [&meeting.location, &meeting.organizer]
        .into_iter()
        .flatten()
    {
        body.push('\n');
        body.push_str(more);
    }
    let mut choices = vec![(OPEN, "Calendar", Place::Agenda)];
    if let Some(address) = &meeting.join {
        choices.push(("join", "Join", Place::Meeting(address.clone())));
    }
    if let Some(chat) = &meeting.chat {
        choices.push(("chat", "Chat", Place::Chat(chat.clone())));
    }
    Note {
        about: format!("meeting {} {}", meeting.id, meeting.start.timestamp()),
        icon: "x-office-calendar",
        title,
        body,
        category: "x-tern.meeting",
        urgent: false,
        sound: None,
        choices,
    }
}

fn call(ring: &Ring, chat: Option<String>) -> Note<'static> {
    let kind = if ring.video { "video call" } else { "call" };
    let mut choices = Vec::new();
    match &ring.push {
        // Tern answers with sound, a call of one person or of a group.
        Some(push) => {
            choices.push(("answer", "Answer", Place::Answer(push.clone())));
            choices.push(("decline", "Decline", Place::Decline(push.clone())));
            if let Some(chat) = chat {
                choices.push((OPEN, "Open Chat", Place::Chat(chat)));
            }
        }
        None => {
            if let Some(chat) = chat {
                choices.push((OPEN, "Open Chat", Place::Chat(chat)));
            }
            choices.push(("teams", "Answer in Browser", Place::Teams));
        }
    }
    let body = match &ring.subject {
        Some(subject) => format!("{subject}\nIncoming {kind}"),
        None => format!("Incoming {kind}"),
    };
    Note {
        about: format!("call {}", ring.call),
        icon: "call-start",
        title: format!("{} is calling", ring.name),
        body,
        category: "call.incoming",
        urgent: true,
        sound: Some("phone-incoming-call"),
        choices,
    }
}

impl Notifier {
    pub fn start() -> (Self, mpsc::UnboundedReceiver<Action>) {
        let (actions, queue) = mpsc::unbounded_channel();
        let notifier = Notifier {
            proxy: None,
            open: Open::default(),
            asides: Asides::default(),
            actions,
        };
        (notifier, queue)
    }

    /// Connects on first use: the desktop's notification server may start after us.
    async fn server(&mut self) -> Option<(NotificationsProxy<'static>, Can)> {
        if self.proxy.is_none() {
            match connect(self.open.clone(), self.asides.clone(), self.actions.clone()).await {
                Ok(connected) => self.proxy = Some(connected),
                Err(error) => tracing::warn!(%error, "cannot show notifications"),
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
                tracing::warn!(%error, "could not show a notification");
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

    /// Tells of a meeting, in place of what was told of it before.
    pub async fn remind(&mut self, due: &Reminder) {
        self.note(reminder(due, Utc::now())).await;
    }

    /// Forgets that the desktop showed it, and takes it down.
    async fn take_down(&mut self, about: &str) {
        let aside = self.asides.lock().expect("notifications").remove(about);
        if let (Some(aside), Some((proxy, _))) = (aside, &self.proxy) {
            let _ = proxy.close_notification(aside.id).await;
        }
    }

    pub async fn ring(&mut self, ring: &Ring, chat: Option<String>) {
        if let Some((_, can)) = self.server().await
            && !can.actions
        {
            tracing::warn!(
                "the notification server shows no buttons, so the call cannot be answered from it"
            );
        }
        self.note(call(ring, chat)).await;
    }

    pub async fn stop_ringing(&mut self, ring: &Ring) {
        self.take_down(&format!("call {}", ring.call)).await;
    }

    async fn note(&mut self, note: Note<'_>) {
        let Some((proxy, can)) = self.server().await else {
            return;
        };
        let replaces = {
            let asides = self.asides.lock().expect("notifications");
            asides.get(&note.about).map_or(0, |aside| aside.id)
        };
        let body = if can.markup {
            escape(&note.body)
        } else {
            note.body
        };
        let mut buttons = Vec::new();
        if can.actions {
            for (key, label, _) in &note.choices {
                buttons.extend([*key, *label]);
            }
        }
        let mut hints = HashMap::from([
            ("category", Variant::from(note.category)),
            ("desktop-entry", Variant::from(WINDOW)),
        ]);
        if note.urgent {
            hints.insert("urgency", Variant::from(2u8));
        }
        if let Some(sound) = note.sound {
            hints.insert("sound-name", Variant::from(sound));
        }
        let shown = proxy
            .notify(
                "Tern",
                replaces,
                note.icon,
                &note.title,
                &body,
                &buttons,
                hints,
                -1,
            )
            .await;
        match shown {
            Ok(id) => {
                let choices = note.choices.into_iter().map(|(key, _, to)| (key, to));
                let aside = Aside {
                    id,
                    choices: choices.collect(),
                };
                let mut asides = self.asides.lock().expect("notifications");
                asides.insert(note.about, aside);
            }
            Err(error) => {
                tracing::warn!(%error, "could not show a notification");
                self.proxy = None;
            }
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
    asides: Asides,
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
                    Ok(args) => {
                        let given = token.take().filter(|(id, _)| *id == args.id).map(|(_, token)| token);
                        match choose(&asides, args.id, &args.action_key) {
                            Some(place) => Some(Action::Go(place, given)),
                            None if args.action_key == OPEN => {
                                take(&open, args.id).map(|target| Action::Open(target, given))
                            }
                            None => None,
                        }
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
                    let mut silence = false;
                    if let Ok(args) = signal.args() {
                        // Put away by the user, not by its time or by us.
                        silence = args.reason == DISMISSED && asides
                            .lock()
                            .expect("notifications")
                            .iter()
                            .any(|(about, aside)| aside.id == args.id && about.starts_with("call "));
                        // An answer can follow its notification's close.
                        let (open, asides, id) = (open.clone(), asides.clone(), args.id);
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            take(&open, id);
                            let mut asides = asides.lock().expect("notifications");
                            asides.retain(|_, aside| aside.id != id);
                        });
                    }
                    silence.then_some(Action::Silence)
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

/// Where a button of a notification leads, if it is one of those.
fn choose(asides: &Asides, id: u32, key: &str) -> Option<Place> {
    let asides = asides.lock().expect("notifications");
    let aside = asides.values().find(|aside| aside.id == id)?;
    let (_, place) = aside.choices.iter().find(|(name, _)| *name == key)?;
    Some(place.clone())
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

/// Where a notification about a meeting or a call leads. The window does it: what this service
/// starts is as confined as itself, which no browser bears.
pub async fn go(place: &Place, token: Option<String>) {
    let (action, parameter) = match place {
        Place::Agenda => ("calendar", None),
        Place::Meeting(address) => ("join", Some(address.as_str())),
        Place::Chat(chat) => ("open", Some(chat.as_str())),
        Place::Teams => ("join", Some(TEAMS)),
        Place::Answer(push) => ("answer", Some(push.as_str())),
        Place::Decline(push) => ("decline", Some(push.as_str())),
    };
    let shown = async {
        let connection = zbus::Connection::session().await?;
        wake(&connection).await?;
        let parameter = parameter.map(Variant::from).into_iter().collect();
        ApplicationProxy::new(&connection)
            .await?
            .activate_action(action, parameter, platform(token.as_deref()))
            .await
    };
    let shown = shown.await;
    if shown.is_ok() {
        tracing::info!(choice = place.choice(), "the window took it");
    }
    if let Err(error) = shown {
        tracing::info!(%error, window = WINDOW, "no window; opening Teams on the web");
        match place {
            Place::Agenda => open_address("https://teams.microsoft.com/v2/calendar").await,
            Place::Meeting(address) => open_address(address).await,
            Place::Teams | Place::Answer(_) => open_address(TEAMS).await,
            // Without the window nothing declines; the call rings out.
            Place::Decline(_) => {}
            Place::Chat(chat) => {
                let target = Target {
                    conversation: chat.clone(),
                    message: String::new(),
                };
                open_address(&link(&target)).await;
            }
        }
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

    fn meeting() -> tern_engine::Meeting {
        let start: DateTime<Utc> = "2026-11-25T09:00:00Z".parse().unwrap();
        tern_engine::Meeting {
            id: "m".into(),
            subject: "Standup".into(),
            start,
            end: start + chrono::Duration::minutes(30),
            all_day: false,
            cancelled: false,
            join: Some("https://teams.microsoft.com/l/meetup-join/a/0".into()),
            chat: None,
            organizer: Some("Ada Quill".into()),
            location: None,
            answer: tern_engine::Answer::Accepted,
            free: false,
            remind: None,
        }
    }

    #[test]
    fn calls_are_answered_here() {
        let ring = Ring {
            call: "x".into(),
            from: Some("ada".into()),
            name: "Ada Quill".into(),
            chat: None,
            subject: None,
            video: false,
            push: Some("{\"evt\":107}".into()),
        };
        let keys = |note: Note| {
            note.choices
                .iter()
                .map(|(key, ..)| *key)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            keys(call(&ring, Some("19:a_b@unq.gbl.spaces".into()))),
            ["answer", "decline", OPEN]
        );
        let note = call(&ring, None);
        assert!(matches!(&note.choices[0].2, Place::Answer(push) if push == "{\"evt\":107}"));
        let group = Ring {
            chat: Some("19:g@thread.v2".into()),
            ..ring.clone()
        };
        assert_eq!(
            keys(call(&group, Some("19:g@thread.v2".into()))),
            ["answer", "decline", OPEN]
        );
        // One whose push was not kept is for the browser.
        let unkept = Ring { push: None, ..ring };
        assert_eq!(keys(call(&unkept, None)), ["teams"]);
    }

    #[test]
    fn a_reminder_says_when_and_leads_to_the_meeting() {
        let due = |stage| Reminder {
            meeting: meeting(),
            stage,
        };
        let title = |stage, seconds| {
            let now = meeting().start - chrono::Duration::seconds(seconds);
            reminder(&due(stage), now).title
        };
        assert_eq!(title(Stage::Soon, 598), "Standup in 10 minutes");
        assert_eq!(title(Stage::Soon, 60), "Standup in a minute");
        assert_eq!(title(Stage::Soon, 10), "Standup is about to begin");
        assert_eq!(title(Stage::Now, -20), "Standup begins");
        let note = reminder(&due(Stage::Soon), meeting().start);
        let keys: Vec<_> = note.choices.iter().map(|(key, ..)| *key).collect();
        assert_eq!(keys, [OPEN, "join"]);
        // One notification, updated as it begins.
        assert_eq!(
            note.about,
            reminder(&due(Stage::Now), meeting().start).about
        );
    }

    /// Needs a notification server that presses the button named "join" or "answer" by itself.
    #[tokio::test]
    #[ignore = "needs a bus of its own"]
    async fn buttons_of_reminders_and_calls_come_back() {
        let (mut notifier, mut actions) = Notifier::start();
        let mut due = Reminder {
            meeting: meeting(),
            stage: Stage::Soon,
        };
        notifier.remind(&due).await;
        let address = meeting().join.unwrap();
        let wait = Duration::from_secs(5);
        let pressed = tokio::time::timeout(wait, actions.recv()).await.unwrap();
        assert!(
            matches!(pressed, Some(Action::Go(Place::Meeting(to), Some(_))) if to == address),
            "with the permission to take the focus"
        );
        due.stage = Stage::Now;
        notifier.remind(&due).await;
        let first = tokio::time::timeout(wait, actions.recv()).await.unwrap();
        assert!(matches!(first, Some(Action::Go(Place::Meeting(_), _))));
        assert_eq!(notifier.asides.lock().unwrap().len(), 1, "replaced");
        let ring = Ring {
            call: "x".into(),
            from: None,
            name: "Ada <b>".into(),
            chat: None,
            subject: None,
            video: false,
            push: Some("{}".into()),
        };
        notifier.ring(&ring, None).await;
        let pressed = tokio::time::timeout(wait, actions.recv()).await.unwrap();
        assert!(matches!(pressed, Some(Action::Go(Place::Answer(_), _))));
        notifier.stop_ringing(&ring).await;
        assert_eq!(notifier.asides.lock().unwrap().len(), 1);
    }

    #[test]
    fn only_buttons_that_were_shown_lead_somewhere() {
        let asides = Asides::default();
        let aside = Aside {
            id: 3,
            choices: vec![("chat", Place::Chat("c".into()))],
        };
        asides.lock().unwrap().insert("call x".into(), aside);
        assert_eq!(choose(&asides, 3, "chat"), Some(Place::Chat("c".into())));
        assert_eq!(choose(&asides, 3, "join"), None);
        assert_eq!(choose(&asides, 4, "chat"), None);
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
