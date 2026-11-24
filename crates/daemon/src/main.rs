//! Tern's background service: keeps the user's Teams presence true to what they do at this
//! computer, keeps the message cache complete, sends queued messages and shows notifications.

mod link;
mod messages;
mod notify;
mod tray;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use chatsvc::realtime::{Realtime, RealtimeConfig, RealtimeEvent, StatusKind};
use chatsvc::{PresenceService, PresenceStatus};
use session::{Session, SessionConfig};
use tern_auth::{Broker, HttpEndpoint, KeyringStore};
use tern_engine::{Act, Draft, Failure, Teams};
use tern_ipc::{Event as Change, Request, Response, Status};
use tern_presence::{Config, Event, Input, Machine, Sensors, Source, State};
use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, interval, sleep_until, timeout};

use link::{Link, contradicts};
use messages::{Messages, News};
use notify::{Action, Notifier, Target};
use tray::{Dot, Model, Pick, TrayIcon};

const TICK: Duration = Duration::from_secs(15);
const READ_BACK: Duration = Duration::from_secs(5 * 60);
const READ_AFTER_CHANGE: Duration = Duration::from_secs(20);
/// logind gives a delayed sleep about five seconds.
const BEFORE_SLEEP: Duration = Duration::from_secs(4);
/// Sleep may have to wait this long for a running request, so it stays below logind's limit.
const STEP: Duration = Duration::from_secs(3);
const PAUSE: Duration = Duration::from_secs(60 * 60);
const RECONNECT_FIRST: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(60);

type Asked = (Request, oneshot::Sender<Response>);

struct Online {
    realtime: Realtime,
    service: PresenceService,
    object_id: String,
    session: Session,
    region: String,
}

enum Connect {
    Up(Box<Online>),
    /// Nobody is signed in, or the user has to sign in again: waits for `Reload`.
    NeedsUser(String),
    Failed(String),
}

async fn connect(broker: Arc<Broker>, active: bool) -> Connect {
    use tern_auth::Error::{Blocked, InteractionRequired, SignedOut};
    let identity = match broker.identity().await {
        Some(identity) => identity,
        // The keyring may have been locked when the service started.
        None => match broker.load().await {
            Ok(Some(identity)) => identity,
            Ok(None) => return Connect::NeedsUser("not signed in: run `tern-cli login`".into()),
            Err(error) => return Connect::Failed(error.to_string()),
        },
    };
    let regions = match broker.regions().await {
        Ok(regions) => regions,
        Err(error @ (SignedOut | InteractionRequired { .. } | Blocked(_))) => {
            return Connect::NeedsUser(error.to_string());
        }
        Err(error) => return Connect::Failed(error.to_string()),
    };
    // Minted now, so the short steps later do not have to wait for it.
    if let Err(error) = broker.access(session::PRESENCE, None, false).await {
        return match error {
            SignedOut | InteractionRequired { .. } | Blocked(_) => {
                Connect::NeedsUser(error.to_string())
            }
            error => Connect::Failed(error.to_string()),
        };
    }
    let session =
        Session::with_provider(broker, SessionConfig::default()).with_region(&regions.region);
    let mut service = PresenceService::new(&session);
    if let Some(base) = &regions.presence {
        service = service.at(base);
    }
    let config = RealtimeConfig {
        host: regions.trouter_host,
        // Discovery needs a web page; the host came with the regions.
        discover_host: false,
        initially_active: active,
        native: true,
        ..RealtimeConfig::default()
    };
    let region = regions.region;
    match Realtime::start_with(&session, config).await {
        Ok(realtime) => Connect::Up(Box::new(Online {
            realtime,
            service,
            object_id: identity.object_id,
            session,
            region,
        })),
        Err(error) => Connect::Failed(error.to_string()),
    }
}

type Work = std::pin::Pin<Box<dyn Future<Output = Result<(), Failure>> + Send>>;

/// Requests that ask the service something: done beside the loop, which they must not hold up.
fn beside(teams: Arc<Teams>, request: &Request) -> Option<Work> {
    let act = |conversation: &String, message: &String, act: Act| -> Option<Work> {
        let (conversation, message, teams) = (conversation.clone(), message.clone(), teams.clone());
        Some(Box::pin(async move {
            teams.act(&conversation, &message, &act).await
        }))
    };
    match request {
        Request::React {
            conversation,
            message,
            emoji,
            on,
        } => act(
            conversation,
            message,
            Act::React {
                emoji: emoji.clone(),
                on: *on,
            },
        ),
        Request::Edit {
            conversation,
            message,
            text,
        } => act(
            conversation,
            message,
            Act::Edit {
                markdown: text.clone(),
            },
        ),
        Request::Delete {
            conversation,
            message,
        } => act(conversation, message, Act::Delete),
        Request::Look { conversation, post } => {
            let (conversation, post, teams) = (conversation.clone(), post.clone(), teams.clone());
            Some(Box::pin(async move {
                teams.look(&conversation, post.as_deref()).await
            }))
        }
        Request::Older { conversation } => {
            let (conversation, teams) = (conversation.clone(), teams.clone());
            Some(Box::pin(async move {
                teams.back(&conversation).await.map(drop)
            }))
        }
        _ => None,
    }
}

struct Daemon {
    broker: Arc<Broker>,
    machine: Machine,
    changed: Instant,
    link: Link,
    online: Option<Box<Online>>,
    connecting: Option<JoinHandle<Connect>>,
    connect_at: Option<Instant>,
    backoff: Duration,
    push: String,
    problem: Option<String>,
    idle_source: Option<Source>,
    logind: bool,
    shown: Option<(Instant, PresenceStatus)>,
    read_at: Instant,
    mismatches: u32,
    /// A development instance leaves presence to the installed one.
    presence: bool,
    messages: Option<Messages>,
    news: mpsc::UnboundedSender<News>,
    changes: broadcast::Sender<Change>,
    told: Option<Status>,
    notifier: Notifier,
    tray: TrayIcon,
    paused_until: Option<Instant>,
}

impl Daemon {
    fn new(
        broker: Arc<Broker>,
        news: mpsc::UnboundedSender<News>,
        changes: broadcast::Sender<Change>,
        notifier: Notifier,
        tray: TrayIcon,
    ) -> Self {
        let now = Instant::now();
        Daemon {
            broker,
            machine: Machine::default(),
            changed: now,
            link: Link::default(),
            online: None,
            connecting: None,
            connect_at: Some(now),
            backoff: RECONNECT_FIRST,
            push: "offline".into(),
            problem: None,
            idle_source: None,
            logind: false,
            shown: None,
            read_at: now + READ_BACK,
            mismatches: 0,
            presence: std::env::var_os("TERN_NO_PRESENCE").is_none(),
            messages: None,
            news,
            changes,
            told: None,
            notifier,
            tray,
            paused_until: None,
        }
    }

    fn active(&self) -> bool {
        self.machine.desired().active
    }

    async fn reconcile(&mut self) {
        if let Some(online) = self.online.as_ref().filter(|_| self.presence) {
            let desired = self.machine.desired();
            online.realtime.set_activity(desired.active);
            // Bounded, so a hanging request cannot hold up the next sensor event (sleep above all).
            let step = self
                .link
                .reconcile(&online.service, desired, Instant::now());
            if timeout(STEP, step).await.is_err() {
                tracing::debug!("the presence service is slow");
            }
        }
    }

    async fn input(&mut self, input: Input) {
        if let Some(state) = self.machine.apply(input) {
            tracing::info!(?state, "user");
            self.changed = Instant::now();
            self.read_at = self.changed + READ_AFTER_CHANGE;
            self.reconcile().await;
        }
    }

    /// Starts over with a new push connection. The old one is dropped, not waited for: after sleep
    /// or a network change it is dead and would only be noticed after a timeout.
    fn reconnect(&mut self, after: Duration) {
        self.online = None;
        if let Some(task) = self.connecting.take() {
            task.abort();
        }
        self.push = "connecting".into();
        self.link.restart();
        self.connect_at = Some(Instant::now() + after);
    }

    async fn sleep(&mut self) {
        // Not through `input`: its own request would use up the time `leave` has.
        self.machine.apply(Input::Sleeping(true));
        self.changed = Instant::now();
        self.leave().await;
        self.connect_at = None;
        self.push = "asleep".into();
    }

    async fn leave(&mut self) {
        if let Some(task) = self.connecting.take() {
            task.abort();
        }
        if let Some(online) = self.online.take() {
            online.realtime.set_activity(false);
            let online = *online;
            // Side by side: each is worth doing even if the other hangs.
            let goodbye =
                async { tokio::join!(self.link.withdraw(&online.service), online.realtime.stop()) };
            if timeout(BEFORE_SLEEP, goodbye).await.is_err() {
                tracing::warn!("leaving took too long");
            }
        }
        self.link.restart();
    }

    async fn sensor(&mut self, event: Event) {
        match event {
            Event::Input(input) => self.input(input).await,
            Event::Sleep(guard) => {
                self.sleep().await;
                drop(guard);
            }
            Event::Resume => {
                self.input(Input::Sleeping(false)).await;
                self.backoff = RECONNECT_FIRST;
                self.reconnect(Duration::ZERO);
            }
            Event::NetworkUp if self.machine.state() != State::Sleeping => {
                tracing::info!("network changed");
                self.backoff = RECONNECT_FIRST;
                self.reconnect(Duration::ZERO);
            }
            Event::NetworkUp => {}
            Event::Ready(Source::Logind) => self.logind = true,
            Event::Ready(source) => {
                tracing::info!(?source, "idle detection");
                self.idle_source = Some(source);
            }
            Event::Lost(Source::Logind, reason) => {
                self.logind = false;
                tracing::warn!(reason, "logind");
            }
            Event::Lost(_, reason) => {
                self.idle_source = None;
                tracing::warn!(reason, "no idle detection");
            }
        }
    }

    async fn pushed(&mut self, event: Option<RealtimeEvent>) {
        match event {
            None => self.reconnect(self.backoff),
            Some(RealtimeEvent::Endpoint(endpoint)) => {
                self.link.endpoint(endpoint.endpoint_id);
                self.reconcile().await;
            }
            Some(RealtimeEvent::Message(message)) => {
                if let Some(messages) = &self.messages {
                    messages.event(message);
                }
            }
            Some(RealtimeEvent::Typing(typing)) => {
                let own = self
                    .online
                    .as_ref()
                    .is_some_and(|online| online.object_id == typing.user_id);
                if !own {
                    let _ = self.changes.send(Change::Typing {
                        conversation: typing.conversation_id,
                        name: typing.display_name,
                        active: typing.active,
                    });
                }
            }
            Some(RealtimeEvent::Status(status)) => match status.kind {
                StatusKind::Connected => {
                    tracing::info!("push connected");
                    self.push = "connected".into();
                    self.problem = None;
                    self.backoff = RECONNECT_FIRST;
                    self.link.connected();
                    if let Some(messages) = &self.messages {
                        messages.catch_up();
                    }
                    self.read_at = Instant::now() + READ_AFTER_CHANGE;
                    self.reconcile().await;
                }
                StatusKind::Disconnected | StatusKind::Error => {
                    tracing::info!(detail = status.detail, "push");
                    self.push = "reconnecting".into();
                    self.link.disconnected(Instant::now());
                }
                StatusKind::MessageLoss => {
                    if let Some(messages) = &self.messages {
                        messages.catch_up();
                    }
                }
            },
            Some(_) => {}
        }
    }

    async fn connected(&mut self, outcome: Connect) {
        match outcome {
            Connect::Up(online) => {
                self.start_messages(&online);
                self.online = Some(online);
            }
            Connect::NeedsUser(reason) => {
                tracing::warn!(reason, "waiting for a sign-in");
                self.push = "signed out".into();
                self.problem = Some(reason);
            }
            Connect::Failed(reason) => {
                tracing::warn!(reason, "could not connect");
                self.problem = Some(reason);
                self.start_messages_offline().await;
                self.connect_at = Some(Instant::now() + self.backoff);
                self.backoff = (self.backoff * 2).min(RECONNECT_MAX);
            }
        }
    }

    /// Message sync outlives a connection: what is queued and cached stays through sleep and
    /// network changes.
    fn start_messages(&mut self, online: &Online) {
        if self
            .messages
            .as_ref()
            .is_some_and(|messages| messages.account() == online.object_id)
        {
            return;
        }
        match Messages::start(
            &online.object_id,
            online.session.clone(),
            self.news.clone(),
            self.changes.clone(),
        ) {
            Ok(messages) => {
                messages.remember_region(&online.region);
                self.messages = Some(messages);
            }
            Err(error) => {
                tracing::error!(error, "no message cache");
                self.problem = Some(error);
            }
        }
    }

    /// Without a connection, so messages can be queued and read already.
    async fn start_messages_offline(&mut self) {
        if self.messages.is_some() {
            return;
        }
        let Some(identity) = self.broker.identity().await else {
            return;
        };
        let Some(region) = messages::stored_region(&identity.object_id) else {
            return;
        };
        let session = Session::with_provider(self.broker.clone(), SessionConfig::default())
            .with_region(&region);
        self.messages = Messages::start(
            &identity.object_id,
            session,
            self.news.clone(),
            self.changes.clone(),
        )
        .ok();
    }

    async fn news(&mut self, news: News) {
        let paused = self.paused();
        let Some(messages) = &mut self.messages else {
            return;
        };
        match news {
            News::Arrived(arrived) => {
                let quiet = paused
                    || self
                        .shown
                        .as_ref()
                        .is_some_and(|(_, status)| status.availability == "DoNotDisturb");
                for notice in messages.notices(&arrived) {
                    if !quiet {
                        self.notifier.show(&notice).await;
                    }
                }
            }
            News::Failed { reason, .. } => self.notifier.failed(&reason).await,
        }
    }

    async fn acted(&mut self, action: Action) {
        match action {
            // Not waited for: the portal may ask the user something.
            Action::Open(target, token) => {
                drop(tokio::spawn(
                    async move { notify::open(&target, token).await },
                ));
            }
            Action::Reply { conversation, text } => {
                let queued = match &self.messages {
                    Some(messages) => messages.send(&conversation, &Draft::text(&text), &[]),
                    None => Err("not signed in".into()),
                };
                if let Err(error) = queued {
                    self.notifier.failed(&error).await;
                }
            }
        }
    }

    fn paused(&self) -> bool {
        self.paused_until
            .is_some_and(|until| Instant::now() < until)
    }

    /// Answers `false` to end the service.
    async fn picked(&mut self, pick: Pick) -> bool {
        match pick {
            Pick::Chat(conversation) => {
                let message = self
                    .messages
                    .as_ref()
                    .and_then(|messages| messages.newest(&conversation));
                let target = Target {
                    conversation,
                    message: message.unwrap_or_default(),
                };
                tokio::spawn(async move { notify::open(&target, None).await });
            }
            Pick::Window => {
                tokio::spawn(notify::open_window());
            }
            Pick::Pause(pause) => {
                self.paused_until = pause.then(|| Instant::now() + PAUSE);
            }
            Pick::Quit => return false,
        }
        true
    }

    async fn update_tray(&mut self) {
        let shown = self
            .shown
            .as_ref()
            .map(|(_, status)| status.availability.as_str());
        let connected = self.push == "connected";
        let dot = match (connected, shown) {
            (false, _) => Dot::Offline,
            (true, Some("Busy" | "BusyIdle" | "DoNotDisturb")) => Dot::Busy,
            (true, _) if self.active() => Dot::Active,
            (true, _) => Dot::Away,
        };
        let unread = self
            .messages
            .as_ref()
            .map(Messages::unread)
            .unwrap_or_default();
        let presence = match (connected, shown) {
            (false, _) => self.push.clone(),
            (true, Some(shown)) => format!("Teams shows you as {shown}"),
            (true, None) => "Connected".to_owned(),
        };
        let line = match unread.len() {
            0 => presence,
            1 => format!("{presence}, 1 unread chat"),
            count => format!("{presence}, {count} unread chats"),
        };
        let paused = self.paused();
        self.tray
            .show(Model {
                dot,
                line,
                unread,
                paused,
            })
            .await;
    }

    /// Tells followers when the connection or account changed, not every passing second.
    async fn tell_status(&mut self) {
        let mut status = self.status(false).await;
        status.state_for_secs = 0;
        status.teams_read_secs_ago = None;
        status.reading = 0;
        if self.told.as_ref() != Some(&status) {
            self.told = Some(status.clone());
            let _ = self.changes.send(Change::Status {
                status: Box::new(status),
            });
        }
    }

    fn settle(
        &self,
        act: impl FnOnce(&tern_engine::Outbox<Teams>) -> Result<(), String>,
    ) -> Response {
        let Some(messages) = &self.messages else {
            return Response::Error("not connected yet".into());
        };
        match act(messages.outbox()) {
            Ok(()) => {
                let _ = self.changes.send(Change::Chats);
                Response::Done
            }
            Err(error) => Response::Error(error),
        }
    }

    /// Takes down notifications for what was read elsewhere.
    async fn sweep(&mut self) {
        if let Some(messages) = &self.messages {
            self.notifier
                .close_read(|conversation, at| {
                    messages
                        .read_until(conversation)
                        .is_some_and(|read| read >= at)
                })
                .await;
        }
    }

    async fn read_back(&mut self) {
        self.read_at = Instant::now() + READ_BACK;
        let Some(online) = self.online.as_ref().filter(|_| self.presence) else {
            return;
        };
        match timeout(STEP, online.service.status(&online.object_id)).await {
            Ok(Ok(status)) => {
                if contradicts(&status, self.active()) {
                    self.mismatches += 1;
                    tracing::warn!(
                        shows = status.availability,
                        times = self.mismatches,
                        "Teams disagrees; registering again"
                    );
                    self.link.repair();
                    online.realtime.resend_activity();
                } else {
                    self.mismatches = 0;
                }
                let before = self.shown.as_ref().map(|(_, shown)| &shown.availability);
                if before != Some(&status.availability) {
                    tracing::info!(shows = status.availability, state = ?self.machine.state(), "Teams");
                }
                self.shown = Some((Instant::now(), status));
                self.reconcile().await;
            }
            Ok(Err(error)) => tracing::debug!(%error, "could not read presence back"),
            Err(_) => tracing::debug!("reading presence back timed out"),
        }
    }

    /// `media` counts files on disk, so only who asks for it pays for it.
    async fn status(&self, media: bool) -> Status {
        let sync = self
            .messages
            .as_ref()
            .map(Messages::state)
            .unwrap_or_default();
        Status {
            account_id: self
                .broker
                .identity()
                .await
                .map(|identity| identity.object_id),
            account: self.broker.identity().await.map(|identity| identity.name),
            state: match self.machine.state() {
                State::Active => "active",
                State::Idle => "idle",
                State::Locked => "locked",
                State::Sleeping => "sleeping",
                State::InCall => "in_call",
            }
            .into(),
            state_for_secs: self.changed.elapsed().as_secs(),
            idle_source: self.idle_source.map(|source| match source {
                Source::Wayland { exact: true } => "Wayland, real input only".into(),
                Source::Wayland { exact: false } => {
                    "Wayland, video players count as activity".into()
                }
                Source::Mutter => "GNOME".into(),
                Source::Logind => "logind".into(),
            }),
            lock_source: self.logind,
            push: self.push.clone(),
            registered: self.link.registered(),
            teams_shows: self
                .shown
                .as_ref()
                .map(|(_, status)| status.availability.clone()),
            teams_read_secs_ago: self.shown.as_ref().map(|(at, _)| at.elapsed().as_secs()),
            problem: self.problem.clone().or_else(|| self.link.problem.clone()),
            synced: sync.refreshed,
            reading: sync.pending,
            unsent: self.messages.as_ref().map_or(0, Messages::unsent),
            sync_problem: sync.problem,
            media: self
                .messages
                .as_ref()
                .filter(|_| media)
                .map(Messages::media)
                .unwrap_or_default(),
        }
    }

    async fn answer(&mut self, request: Request) -> Response {
        match request {
            Request::Status => Response::Status(Box::new(self.status(true).await)),
            Request::Typing {
                conversation,
                active,
            } => {
                if let Some(messages) = &self.messages {
                    messages.typing(conversation, active);
                }
                Response::Done
            }
            Request::MarkRead { conversation } => {
                if let Some(messages) = &self.messages {
                    messages.mark_read(conversation);
                }
                Response::Done
            }
            // Answered by `serve`.
            Request::Watch => Response::Error("not here".into()),
            Request::Send {
                conversation,
                text,
                reply_to,
                thread,
                post,
                subject,
                mentions,
                files,
            } => {
                let draft = Draft {
                    markdown: text,
                    reply_to,
                    thread,
                    post,
                    subject,
                    mentions,
                    files: Vec::new(),
                };
                match &self.messages {
                    Some(messages) => match messages.send(&conversation, &draft, &files) {
                        Ok(()) => {
                            // It shows as being sent at once.
                            let _ = self.changes.send(Change::Messages { conversation });
                            Response::Done
                        }
                        Err(error) => Response::Error(error),
                    },
                    None => Response::Error("not connected yet".into()),
                }
            }
            Request::Retry { id } => self.settle(|outbox| outbox.retry(&id)),
            Request::Discard { id } => self.settle(|outbox| outbox.discard(&id)),
            // Answered beside the loop, see `run`.
            Request::File { .. }
            | Request::React { .. }
            | Request::Edit { .. }
            | Request::Delete { .. }
            | Request::Look { .. }
            | Request::Older { .. } => Response::Error("not connected yet".into()),
            Request::Reload => {
                self.leave().await;
                self.messages = None;
                self.shown = None;
                match self.broker.load().await {
                    Ok(_) => {
                        self.problem = None;
                        self.reconnect(Duration::ZERO);
                        Response::Done
                    }
                    Err(error) => {
                        self.problem = Some(error.to_string());
                        Response::Error(error.to_string())
                    }
                }
            }
        }
    }

    async fn run(
        mut self,
        mut asked: mpsc::Receiver<Asked>,
        mut news: mpsc::UnboundedReceiver<News>,
        mut actions: mpsc::UnboundedReceiver<Action>,
        mut picks: mpsc::UnboundedReceiver<Pick>,
    ) -> std::io::Result<()> {
        let mut sensors = Sensors::start(Config::default());
        let mut tick = interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut terminate = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        loop {
            self.update_tray().await;
            self.tell_status().await;
            let connect_at = self.connect_at;
            tokio::select! {
                Some(event) = sensors.recv() => self.sensor(event).await,
                event = async {
                    match self.online.as_mut() {
                        Some(online) => online.realtime.recv().await,
                        None => std::future::pending().await,
                    }
                } => self.pushed(event).await,
                outcome = async {
                    match self.connecting.as_mut() {
                        Some(task) => task.await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.connecting = None;
                    self.connected(outcome.unwrap_or_else(|error| Connect::Failed(error.to_string()))).await;
                }
                () = async {
                    match connect_at {
                        Some(at) => sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                }, if self.connecting.is_none() => {
                    self.connect_at = None;
                    self.push = "connecting".into();
                    self.connecting = Some(tokio::spawn(connect(self.broker.clone(), self.active())));
                }
                () = sleep_until(self.read_at) => self.read_back().await,
                _ = tick.tick() => {
                    self.reconcile().await;
                    self.sweep().await;
                }
                Some(item) = news.recv() => self.news(item).await,
                Some(action) = actions.recv() => self.acted(action).await,
                Some(pick) = picks.recv() => {
                    if !self.picked(pick).await {
                        break;
                    }
                }
                Some((request, reply)) = asked.recv() => {
                    if let Request::File { conversation, message, index } = &request
                        && let Some(messages) = &self.messages
                    {
                        // A download takes its time; nothing else waits for it.
                        let file = messages.file(conversation, message, *index);
                        tokio::spawn(async move {
                            let _ = reply.send(match file.await {
                                Ok(path) => Response::Path(path),
                                Err(error) => Response::Error(error),
                            });
                        });
                        continue;
                    }
                    if let Some(messages) = &self.messages
                        && let Some(work) = beside(messages.teams(), &request)
                    {
                        tokio::spawn(async move {
                            let _ = reply.send(match work.await {
                                Ok(()) => Response::Done,
                                Err(failure) => Response::Error(failure.text().to_owned()),
                            });
                        });
                        continue;
                    }
                    let response = self.answer(request).await;
                    let _ = reply.send(response);
                }
                _ = terminate.recv() => break,
                _ = interrupt.recv() => break,
            }
        }
        tracing::info!("leaving");
        self.leave().await;
        Ok(())
    }
}

/// Streams changes to a follower, its first line being how things are.
async fn follow(
    mut stream: UnixStream,
    ask: &mpsc::Sender<Asked>,
    mut changes: broadcast::Receiver<Change>,
) {
    let (reply, answer) = oneshot::channel();
    if ask.send((Request::Status, reply)).await.is_err() {
        return;
    }
    if let Ok(Response::Status(status)) = answer.await
        && tern_ipc::send_event(&mut stream, &Change::Status { status })
            .await
            .is_err()
    {
        return;
    }
    loop {
        let change = match changes.recv().await {
            Ok(change) => change,
            Err(broadcast::error::RecvError::Lagged(_)) => Change::Chats,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if tern_ipc::send_event(&mut stream, &change).await.is_err() {
            return;
        }
    }
}

async fn serve(
    mut stream: UnixStream,
    ask: mpsc::Sender<Asked>,
    changes: broadcast::Sender<Change>,
) {
    if !tern_ipc::same_user(&stream) {
        return;
    }
    let response = match tern_ipc::receive(&mut stream).await {
        Ok(Request::Watch) => {
            let changes = changes.subscribe();
            return follow(stream, &ask, changes).await;
        }
        Ok(request) => {
            let (reply, answer) = oneshot::channel();
            if ask.send((request, reply)).await.is_err() {
                return;
            }
            answer
                .await
                .unwrap_or_else(|_| Response::Error("no answer".into()))
        }
        Err(error) => Response::Error(error.to_string()),
    };
    let _ = tern_ipc::respond(&mut stream, &response).await;
}

async fn start() -> Result<(), String> {
    let listener = tern_ipc::bind().await.map_err(|error| error.to_string())?;
    let endpoint = HttpEndpoint::new().map_err(|error| error.to_string())?;
    let broker = Broker::new(Box::new(endpoint), Box::new(KeyringStore));
    if let Err(error) = broker.load().await {
        tracing::warn!(%error, "could not read the stored sign-in");
    }
    let (ask, asked) = mpsc::channel(8);
    let changes = broadcast::channel(256).0;
    let followers = changes.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve(stream, ask.clone(), followers.clone()));
        }
    });
    let (news, news_queue) = mpsc::unbounded_channel();
    let (notifier, actions) = Notifier::start();
    let (tray, picks) = TrayIcon::start().await;
    let outcome = Daemon::new(broker, news, changes, notifier, tray)
        .run(asked, news_queue, actions, picks)
        .await
        .map_err(|error| error.to_string());
    if let Ok(path) = tern_ipc::socket_path() {
        let _ = std::fs::remove_file(path);
    }
    outcome
}

fn main() -> ExitCode {
    // Dependencies stay at warnings whatever is asked: their debug output can hold request headers.
    let level = match std::env::var("TERN_LOG").as_deref() {
        Ok("debug") => "warn,ternd=debug,tern_auth=debug,tern_presence=debug,tern_engine=debug",
        _ => "warn,ternd=info,tern_auth=info,tern_presence=info,tern_engine=info",
    };
    tracing_subscriber::fmt()
        .with_env_filter(level)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("ternd: {error}");
            return ExitCode::FAILURE;
        }
    };
    let outcome = runtime.block_on(start());
    // The idle watcher blocks on the compositor; it must not hold up the exit.
    runtime.shutdown_timeout(Duration::from_secs(1));
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ternd: {error}");
            ExitCode::FAILURE
        }
    }
}
