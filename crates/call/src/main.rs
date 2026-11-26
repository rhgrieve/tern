//! One Teams call, in a process of its own: when it ends, or this process is stopped, the
//! microphone, the relay and the push connection go with it.
//!
//! Sound only. The call code reads switches from the environment (sound files, traces, a
//! recording of the microphone), so they are cleared before anything else runs.
//!
//! What happens is printed one line at a time: `calling`, `ringing`, `connected`, `reconnecting`,
//! `muted`, `unmuted`, `waiting` and `admitted` for a meeting's lobby, `people: …` with who is in
//! the call, `ended: …` and `failed: …`. With `--pipe` the lines `mute`, `unmute` and
//! `hangup` are read from the standard input, and its end hangs up: the window that started the
//! call cannot go away and leave it running.
//!
//! `answer` and `decline` take a ring that Tern's service heard: its push, as the first line of the
//! standard input. They say `caller: …` once the call is reached.

mod guard;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use calling::breakout::target_from_join_url;
use calling::relay::relay_host_for;
use calling::signaling::TenantRouting;
use calling::video_send::VideoMode;
use calling::{
    AcceptMode, AudioMode, CallCommand, CallEngine, CallOptions, CallReport, CallSpec, CallState,
    CallUpdate, Callee, EndReason, EngineConfig, EngineEvent, MuteCommand, Progress, call_channel,
    run_call,
};
use clap::{Parser, Subcommand};
use session::{Session, SessionConfig};
use tern_auth::{Broker, HttpEndpoint, KeyringStore};
use tokio::io::{AsyncBufReadExt, BufReader, Lines, Stdin};
use tokio::sync::{mpsc, watch};

/// How long a call may last at most once connected.
const LONGEST: Duration = Duration::from_secs(4 * 3600);
/// Time for the call to be answered.
const ANSWER: Duration = Duration::from_secs(75);
/// How long hanging up may take before the process ends anyway.
const HANGING_UP: Duration = Duration::from_secs(5);
/// Time to reach a call that rang, before answering or declining it.
const REACHING: Duration = Duration::from_secs(20);
/// The longest push of a ring that is taken.
const LONGEST_PUSH: usize = 256 * 1024;
/// The most people a group call rings.
const MOST_CALLED: usize = 100;
/// Waiting in a meeting's lobby, before somebody lets one in.
const LOBBY: Duration = Duration::from_secs(15 * 60);

#[derive(Parser)]
#[command(about = "One Teams call, sound only")]
struct Cli {
    #[command(subcommand)]
    call: Call,
}

#[derive(Subcommand)]
enum Call {
    /// Call Teams' Echo test: it records what you say and plays it back.
    /// The first time, Teams starts a chat with the Echo bot in your account.
    Echo {
        /// Hang up this long after connecting
        #[arg(long, default_value_t = 20)]
        seconds: u64,
        /// Send a 440 Hz tone instead of the microphone
        #[arg(long)]
        tone: bool,
        /// Take mute, unmute and hangup on the standard input; its end hangs up
        #[arg(long)]
        pipe: bool,
    },
    /// Call one person, as from your 1:1 chat with them
    Person {
        /// Who, as Teams names them: 8:orgid:…
        mri: String,
        /// Your 1:1 chat with them: 19:…@unq.gbl.spaces
        thread: String,
        /// Their name, as the call shows it
        #[arg(long, default_value = "")]
        name: String,
        /// Take mute, unmute and hangup on the standard input; its end hangs up
        #[arg(long)]
        pipe: bool,
    },
    /// Call the people of a group chat
    Group {
        /// The group chat: 19:…@thread.v2
        thread: String,
        /// Somebody to ring, as `8:orgid:…`, or `8:orgid:…=Name`; once for each
        #[arg(long = "member", required = true)]
        members: Vec<String>,
        /// Take mute, unmute and hangup on the standard input; its end hangs up
        #[arg(long)]
        pipe: bool,
    },
    /// Join a meeting by its link, with sound only, muted
    Meeting {
        /// https://teams.microsoft.com/l/meetup-join/…
        link: String,
        /// Take mute, unmute and hangup on the standard input; its end hangs up
        #[arg(long)]
        pipe: bool,
    },
    /// Answer a call that rang, with sound only; its push is the first line of the input
    Answer {
        /// Take mute, unmute and hangup on the standard input after the push; its end hangs up
        #[arg(long)]
        pipe: bool,
    },
    /// Decline a call that rang, for all your devices; its push is the first line of the input
    Decline,
}

fn main() -> ExitCode {
    clear_switches();
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            println!("failed: could not start: {error}");
            return ExitCode::FAILURE;
        }
    };
    let outcome = runtime.block_on(async {
        match cli.call {
            Call::Echo {
                seconds,
                tone,
                pipe,
            } => {
                let connected_for = Duration::from_secs(seconds.clamp(1, LONGEST.as_secs()));
                if !pipe {
                    println!(
                        "Calling the Echo test ({}), hanging up {} s after it answers. Ctrl+C hangs up.",
                        if tone { "a tone" } else { "your microphone" },
                        connected_for.as_secs()
                    );
                }
                let audio = if tone {
                    AudioMode::Tone
                } else {
                    AudioMode::Platform
                };
                place(CallSpec::Echo, connected_for, audio, pipe).await
            }
            Call::Person {
                mri,
                thread,
                name,
                pipe,
            } => match person(mri, thread, name) {
                Ok(spec) => place(spec, LONGEST, AudioMode::Platform, pipe).await,
                Err(error) => Err(error),
            },
            Call::Group {
                thread,
                members,
                pipe,
            } => match group(thread, &members) {
                Ok(spec) => place(spec, LONGEST, AudioMode::Platform, pipe).await,
                Err(error) => Err(error),
            },
            Call::Meeting { link, pipe } => match meeting(&link) {
                Ok(spec) => place(spec, LONGEST, AudioMode::Platform, pipe).await,
                Err(error) => Err(error),
            },
            Call::Answer { pipe } => answer(pipe, false).await,
            Call::Decline => answer(false, true).await,
        }
    });
    // The call code may leave tasks behind; they end with the process, not after it.
    runtime.shutdown_timeout(Duration::from_millis(500));
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            println!("failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The call code's switches: they could write the microphone to disk or show names.
fn clear_switches() {
    let switches: Vec<_> = std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| {
            let name = name.to_string_lossy();
            name.starts_with("CALLING_") || name == "TEAMS_PAGE_TROUTER" || name == "PORTAL_SMOKE"
        })
        .collect();
    for name in switches {
        // SAFETY: nothing else runs yet: this is the first thing `main` does, before any thread.
        unsafe { std::env::remove_var(name) };
    }
    // A call that is answered takes these from the environment: sound only.
    // SAFETY: as above.
    unsafe {
        std::env::set_var("CALLING_VIDEO", "off");
        std::env::set_var("CALLING_DATA_CHANNEL", "off");
    }
}

/// Whether `mri` is a person of an organisation, `8:orgid:` and an id.
fn is_person(mri: &str) -> bool {
    mri.strip_prefix("8:orgid:").is_some_and(|id| {
        id.len() == 36
            && id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    })
}

/// A call to the people of a group chat.
fn group(thread: String, members: &[String]) -> Result<CallSpec, String> {
    let chat = thread
        .strip_prefix("19:")
        .and_then(|rest| {
            rest.strip_suffix("@thread.v2")
                .or(rest.strip_suffix("@thread.skype"))
        })
        .filter(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        });
    if chat.is_none() {
        return Err("That is not a group chat.".into());
    }
    if members.is_empty() || members.len() > MOST_CALLED {
        return Err(format!("A group call rings 1 to {MOST_CALLED} people."));
    }
    let callees = members
        .iter()
        .map(|member| {
            let (mri, name) = member.split_once('=').unwrap_or((member, ""));
            is_person(mri)
                .then(|| Callee {
                    mri: mri.to_owned(),
                    display_name: name.chars().filter(|c| !c.is_control()).take(120).collect(),
                })
                .ok_or_else(|| format!("{mri} is not a person Teams can call."))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CallSpec::People {
        callees,
        thread_id: thread,
    })
}

/// Joining the meeting of a link, if it is one Teams' own pages would take.
fn meeting(link: &str) -> Result<CallSpec, String> {
    let url = url::Url::parse(link).map_err(|_| "That is not a meeting link.")?;
    let teams = url.scheme() == "https"
        && matches!(
            url.host_str(),
            Some("teams.microsoft.com" | "teams.cloud.microsoft" | "teams.live.com")
        )
        && url.path().starts_with("/l/meetup-join/");
    let target = target_from_join_url(link)
        .filter(|_| teams)
        .ok_or("That is not a meeting link Tern can join.")?;
    let plain = |id: &str| {
        !id.is_empty()
            && id.len() <= 64
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    };
    let thread = target
        .thread_id
        .strip_prefix("19:")
        .and_then(|rest| rest.strip_suffix("@thread.v2"))
        .is_some_and(|id| {
            id.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        });
    if !thread || !plain(&target.tenant_id) || !plain(&target.organizer_id) {
        return Err("That is not a meeting link Tern can join.".into());
    }
    Ok(CallSpec::Meeting(target))
}

/// A call to one person, if `mri` names a person and `thread` is a 1:1 chat with them.
fn person(mri: String, thread: String, name: String) -> Result<CallSpec, String> {
    let id = mri
        .strip_prefix("8:orgid:")
        .filter(|_| is_person(&mri))
        .ok_or("That is not a person Teams can call.")?;
    let chat = thread
        .strip_prefix("19:")
        .and_then(|rest| rest.strip_suffix("@unq.gbl.spaces"))
        .filter(|pair| pair.split('_').any(|one| one == id))
        .ok_or("That is not a 1:1 chat with them.")?;
    if chat.split('_').count() != 2 {
        return Err("That is not a 1:1 chat with them.".into());
    }
    Ok(CallSpec::People {
        callees: vec![Callee {
            mri,
            display_name: name,
        }],
        thread_id: thread,
    })
}

/// The country sent with the call, from the locale; otherwise one for the account's region.
fn client_location(region: &str) -> String {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .unwrap_or_default();
    country(&locale, region)
}

fn country(locale: &str, region: &str) -> String {
    let named = locale
        .split(['.', '@'])
        .next()
        .and_then(|language| language.split_once('_'))
        .map(|(_, country)| country)
        .filter(|country| country.len() == 2 && country.bytes().all(|b| b.is_ascii_uppercase()));
    match (named, region) {
        (Some(country), _) => country.to_owned(),
        (None, "emea") => "DE".into(),
        (None, "apac") => "SG".into(),
        (None, _) => "US".into(),
    }
}

/// What every call needs: the account's routing, and sessions that only reach the call's own
/// services.
struct Setup {
    session: Session,
    poll_session: Session,
    config: EngineConfig,
}

async fn prepare() -> Result<Setup, String> {
    let text = |error: &dyn std::fmt::Display| error.to_string();
    let endpoint = HttpEndpoint::new().map_err(|error| text(&error))?;
    let broker = Broker::new(Box::new(endpoint), Box::new(KeyringStore));
    broker
        .load()
        .await
        .map_err(|error| text(&error))?
        .ok_or("Not signed in.")?;
    let regions = broker.regions().await.map_err(|error| text(&error))?;
    let conversations = regions
        .hosts
        .iter()
        .find(|(name, _)| name == "calling_conversationServiceUrl")
        .map(|(_, host)| host.as_str())
        .ok_or("Teams named no call service for this account.")?;
    let routing = TenantRouting::for_account(
        &regions.region,
        &regions.partition,
        conversations,
        &client_location(&regions.region),
    )
    .map_err(|error| text(&error))?;
    let relay_host = relay_host_for(&regions.region).to_owned();

    let region = regions.region.clone();
    let open = || {
        let region = region.clone();
        Session::with_provider(broker.clone(), SessionConfig::default())
            .with_region(&regions.region)
            .with_guard(Arc::new(move |request| {
                let allowed = guard::allows(request, &region);
                if !allowed {
                    eprintln!(
                        "refused: {} {}",
                        request.method.as_str(),
                        without_query(&request.url)
                    );
                }
                allowed
            }))
    };
    // The second carries the long poll, so it never holds up the call's own requests.
    let (session, poll_session) = (open(), open());

    let config = EngineConfig {
        ringable: false,
        routing: routing.clone(),
        relay_host: relay_host.clone(),
        trace: false,
        dump_dir: None,
        instance: chatsvc::InstanceNames {
            global: "__ternCallTrouter".into(),
            binding: "__ternCallRealtime".into(),
            endpoint_storage_key: "__ternCallEpid".into(),
        },
        trouter_host: regions.trouter_host.clone(),
    };
    Ok(Setup {
        session,
        poll_session,
        config,
    })
}

/// Places the call and stays until it ends: hung up after `connected_for`, by the other side, by
/// a command or signal, or for not being answered in time.
async fn place(
    spec: CallSpec,
    connected_for: Duration,
    audio: AudioMode,
    pipe: bool,
) -> Result<(), String> {
    let Setup {
        session,
        poll_session,
        config,
    } = prepare().await?;
    let (routing, relay_host) = (config.routing.clone(), config.relay_host.clone());
    let options = CallOptions {
        hold_after_connected: Some(connected_for),
        hard_limit: Some(connected_for + LOBBY),
        audio,
        video: VideoMode::Off,
        relay_host,
        routing,
        record_remote: false,
        wav_path: None,
        sdp_dump_dir: None,
        trace: false,
        data_channel: false,
        concurrent: false,
        start_camera: false,
        ..CallOptions::default()
    };

    let is_meeting = matches!(spec, CallSpec::Meeting(_));
    // Waiting in a lobby is not going unanswered: a meeting may take longer.
    let waiting = if is_meeting { LOBBY } else { ANSWER };
    let (mut handle, control) = call_channel();
    let commands = handle.commands.clone();
    let (connected, mut answered) = watch::channel(false);
    let updates = tokio::spawn(async move {
        while let Some(update) = handle.updates.recv().await {
            if matches!(update, CallUpdate::State(CallState::Connected { .. })) {
                let _ = connected.send(true);
            }
            show(&update);
        }
    });
    let mut asked = if pipe {
        commands_from_input()
    } else {
        mpsc::unbounded_channel().1
    };
    let call = run_call(&session, &poll_session, config, spec, options, control);
    tokio::pin!(call);
    // Nothing may keep the process past the longest call, whatever the call code does.
    let deadline = tokio::time::sleep(waiting + connected_for + HANGING_UP);
    tokio::pin!(deadline);
    let unanswered = async {
        if tokio::time::timeout(waiting, answered.wait_for(|on| *on))
            .await
            .is_ok()
        {
            std::future::pending::<()>().await;
        }
    };
    tokio::pin!(unanswered);
    let report = loop {
        tokio::select! {
            report = &mut call => break report,
            command = asked.recv() => {
                let command = command.unwrap_or(CallCommand::Hangup);
                let hanging_up = matches!(command, CallCommand::Hangup);
                let _ = commands.send(command);
                if hanging_up {
                    break hang_up(&mut call).await?;
                }
            }
            _ = stopped() => {
                let _ = commands.send(CallCommand::Hangup);
                break hang_up(&mut call).await?;
            }
            _ = &mut unanswered => {
                println!("{}", if is_meeting { "ended: nobody let you in" } else { "ended: no answer" });
                let _ = commands.send(CallCommand::Hangup);
                break hang_up(&mut call).await?;
            }
            _ = &mut deadline => {
                let _ = commands.send(CallCommand::Hangup);
                let _ = tokio::time::timeout(HANGING_UP, &mut call).await;
                return Err("the call took too long and was ended".into());
            }
        }
    };
    updates.abort();
    match report {
        Ok(report) => {
            summarise(&report);
            Ok(())
        }
        Err(error) => Err(format!("the call failed: {error}")),
    }
}

/// Answers or declines the ring whose push is the first line of the input. Once answered, stays
/// until the call ends, as `place` does.
async fn answer(pipe: bool, decline: bool) -> Result<(), String> {
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    let push = input
        .next_line()
        .await
        .ok()
        .flatten()
        .filter(|push| push.len() <= LONGEST_PUSH)
        .ok_or("no ring was given")?;
    let Setup {
        session,
        poll_session,
        config,
    } = prepare().await?;
    let (engine, mut events) = CallEngine::start(session, poll_session, config)
        .await
        .map_err(|error| error.to_string())?;
    let outcome = answer_with(&engine, &mut events, &push, input, pipe, decline).await;
    engine.stop().await;
    outcome
}

async fn answer_with(
    engine: &CallEngine,
    events: &mut mpsc::UnboundedReceiver<EngineEvent>,
    push: &str,
    input: Lines<BufReader<Stdin>>,
    pipe: bool,
    decline: bool,
) -> Result<(), String> {
    let ring = engine.take_ring(push).map_err(|error| error.to_string())?;
    let reached = tokio::time::timeout(REACHING, async {
        while let Some(event) = events.recv().await {
            match event {
                EngineEvent::Incoming(incoming) if incoming.ring_id == ring => {
                    let name: String = incoming
                        .caller
                        .display_name
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(120)
                        .collect();
                    if !name.trim().is_empty() {
                        println!("caller: {}", name.trim());
                    }
                    return Ok(());
                }
                EngineEvent::RingEnded { ring_id, .. } if ring_id == ring => {
                    return Err("ended: the call was over before it was answered");
                }
                _ => {}
            }
        }
        Err("failed: the call could not be reached")
    })
    .await
    .unwrap_or(Err("failed: the call could not be reached"));
    if let Err(said) = reached {
        println!("{said}");
        return Ok(());
    }
    if decline {
        engine.decline_ring(ring);
        // Declining is sent beside; it is short.
        tokio::time::sleep(Duration::from_secs(3)).await;
        println!("ended: declined");
        return Ok(());
    }
    let mut handle = engine
        .accept_ring(ring, AcceptMode::Audio)
        .await
        .ok_or("the call was over before it was answered")?;
    let mut asked = if pipe {
        commands_from(input)
    } else {
        mpsc::unbounded_channel().1
    };
    let deadline = tokio::time::sleep(LONGEST + HANGING_UP);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            update = handle.updates.recv() => {
                let Some(update) = update else { return Ok(()) };
                show(&update);
                if matches!(update, CallUpdate::State(CallState::Ended { .. })) {
                    return Ok(());
                }
            }
            command = asked.recv() => {
                let command = command.unwrap_or(CallCommand::Hangup);
                let hanging_up = matches!(command, CallCommand::Hangup);
                let _ = handle.commands.send(command);
                if hanging_up {
                    return ended(&mut handle).await;
                }
            }
            _ = stopped() => {
                let _ = handle.commands.send(CallCommand::Hangup);
                return ended(&mut handle).await;
            }
            _ = &mut deadline => {
                let _ = handle.commands.send(CallCommand::Hangup);
                let _ = ended(&mut handle).await;
                return Err("the call took too long and was ended".into());
            }
        }
    }
}

/// Waits for an answered call to say it ended, after it was told to hang up.
async fn ended(handle: &mut calling::CallHandle) -> Result<(), String> {
    let end = async {
        while let Some(update) = handle.updates.recv().await {
            show(&update);
            if matches!(update, CallUpdate::State(CallState::Ended { .. })) {
                return;
            }
        }
    };
    tokio::time::timeout(HANGING_UP, end)
        .await
        .map_err(|_| "hanging up took too long; the call was left".to_owned())
}

/// Waits for the call to end after it was told to hang up.
async fn hang_up<F>(call: &mut std::pin::Pin<&mut F>) -> Result<calling::Result<CallReport>, String>
where
    F: Future<Output = calling::Result<CallReport>>,
{
    tokio::time::timeout(HANGING_UP, call.as_mut())
        .await
        .map_err(|_| "hanging up took too long; the call was left".to_owned())
}

/// Commands from the window, one per line. The end of the input hangs up.
fn commands_from_input() -> mpsc::UnboundedReceiver<CallCommand> {
    commands_from(BufReader::new(tokio::io::stdin()).lines())
}

fn commands_from(mut lines: Lines<BufReader<Stdin>>) -> mpsc::UnboundedReceiver<CallCommand> {
    let (sender, receiver) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let command = match lines.next_line().await {
                Ok(Some(line)) => match command(&line) {
                    Some(command) => command,
                    None => continue,
                },
                _ => CallCommand::Hangup,
            };
            let last = matches!(command, CallCommand::Hangup);
            if sender.send(command).is_err() || last {
                return;
            }
        }
    });
    receiver
}

fn command(line: &str) -> Option<CallCommand> {
    match line.trim() {
        "mute" => Some(CallCommand::Mute(MuteCommand::Mute)),
        "unmute" => Some(CallCommand::Mute(MuteCommand::Unmute)),
        "hangup" => Some(CallCommand::Hangup),
        _ => None,
    }
}

async fn stopped() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    let terminated = async {
        match term.as_mut() {
            Some(term) => {
                term.recv().await;
            }
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminated => {}
    }
}

fn without_query(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

fn show(update: &CallUpdate) {
    match update {
        CallUpdate::Progress(Progress::Calling) => println!("calling"),
        CallUpdate::Progress(Progress::Ringing) => println!("ringing"),
        CallUpdate::State(CallState::Connecting) => println!("connecting"),
        CallUpdate::State(CallState::Connected { .. }) => println!("connected"),
        CallUpdate::State(CallState::Reconnecting { .. }) => println!("reconnecting"),
        CallUpdate::State(CallState::Ended { reason }) => println!("ended: {}", ending(reason)),
        CallUpdate::Muted(muted) => println!("{}", if *muted { "muted" } else { "unmuted" }),
        CallUpdate::ListenOnly(true) => println!("no microphone: listening only"),
        CallUpdate::Notice(text) => println!("notice: {text}"),
        CallUpdate::Lobby(true) => println!("waiting"),
        CallUpdate::Lobby(false) => println!("admitted"),
        CallUpdate::Roster(entries) => println!("people: {}", people(entries)),
        _ => {}
    }
}

/// Who is in the call and not waiting, by name, joined with `; `.
fn people(entries: &[calling::RosterEntry]) -> String {
    entries
        .iter()
        .filter(|entry| !entry.in_lobby)
        .map(|entry| {
            let name: String = entry
                .display_name
                .chars()
                .filter(|c| !c.is_control() && *c != ';')
                .take(120)
                .collect();
            if name.trim().is_empty() {
                "Someone".to_owned()
            } else {
                name.trim().to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn ending(reason: &EndReason) -> String {
    match reason {
        EndReason::LocalHangup => "hung up".into(),
        EndReason::Cancelled => "cancelled".into(),
        EndReason::Remote(kind) => format!("by the other side ({kind:?})"),
        EndReason::Dropped => "the connection dropped".into(),
        EndReason::Failed(why) => format!("failed: {why}"),
    }
}

fn summarise(report: &CallReport) {
    let ms = |at: Option<u128>| at.map_or("never".to_owned(), |at| format!("{at} ms"));
    println!("media connected after {}", ms(report.peer_connected_ms));
    let path = report.selected_pair.as_deref().map_or("none", |pair| {
        if pair.contains("relay") {
            "through Teams' relay"
        } else {
            "direct"
        }
    });
    println!("path: {path}");
    println!(
        "sound: {} packets sent, {} received, {} s with sound coming in",
        report.outbound_packets, report.inbound_packets, report.seconds_with_inbound_audio
    );
    match &report.end {
        Some(end) => println!("Teams ended it with {} / {}", end.code, end.sub_code),
        None => println!("Teams sent no end"),
    }
    println!(
        "left cleanly: {}",
        if report.left_cleanly { "yes" } else { "no" }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_country_comes_from_the_locale() {
        assert_eq!(country("en_CA.UTF-8", "amer"), "CA");
        assert_eq!(country("de_AT@euro", "emea"), "AT");
        assert_eq!(country("C.UTF-8", "amer"), "US");
        assert_eq!(country("", "emea"), "DE");
        assert_eq!(country("en_ca", "amer"), "US");
    }

    const ME: &str = "11111111-1111-1111-1111-111111111111";
    const ANN: &str = "22222222-2222-2222-2222-222222222222";

    #[test]
    fn only_a_person_in_a_one_to_one_chat_is_called() {
        let mri = format!("8:orgid:{ANN}");
        let chat = format!("19:{ME}_{ANN}@unq.gbl.spaces");
        assert!(person(mri.clone(), chat.clone(), "Ann".into()).is_ok());
        for (mri, chat) in [
            (
                "28:cf28171e-fcfd-47e4-a1d6-79460b0b3ca0".to_owned(),
                chat.clone(),
            ),
            (format!("8:orgid:{ANN}/x"), chat.clone()),
            (mri.clone(), format!("19:{ME}_{ME}@unq.gbl.spaces")),
            (mri.clone(), "19:abc@thread.v2".to_owned()),
            (mri.clone(), format!("19:{ME}_{ANN}_{ME}@unq.gbl.spaces")),
        ] {
            assert!(
                person(mri.clone(), chat.clone(), String::new()).is_err(),
                "{mri} {chat}"
            );
        }
    }

    #[test]
    fn a_group_call_rings_the_people_of_a_group_chat() {
        let ann = format!("8:orgid:{ANN}=Ann Example");
        let me = format!("8:orgid:{ME}");
        let spec = group("19:abc_DEF-1@thread.v2".into(), &[ann.clone(), me.clone()]).unwrap();
        let CallSpec::People { callees, thread_id } = spec else {
            panic!("not people")
        };
        assert_eq!(thread_id, "19:abc_DEF-1@thread.v2");
        assert_eq!(callees[0].display_name, "Ann Example");
        assert_eq!(callees[1].display_name, "");
        for (thread, members) in [
            ("19:x@unq.gbl.spaces".to_owned(), vec![ann.clone()]),
            ("19:a/b@thread.v2".to_owned(), vec![ann.clone()]),
            ("19:x@thread.v2".to_owned(), vec![]),
            ("19:x@thread.v2".to_owned(), vec!["28:bot".to_owned()]),
            (
                "19:x@thread.v2".to_owned(),
                vec![ann.clone(); MOST_CALLED + 1],
            ),
        ] {
            assert!(group(thread.clone(), &members).is_err(), "{thread}");
        }
    }

    #[test]
    fn only_teams_meeting_links_are_joined() {
        let link = "https://teams.microsoft.com/l/meetup-join/19%3ameeting_Abc-1%40thread.v2/0?context=%7b%22Tid%22%3a%2211111111-1111-1111-1111-111111111111%22%2c%22Oid%22%3a%2222222222-2222-2222-2222-222222222222%22%7d";
        assert!(matches!(meeting(link), Ok(CallSpec::Meeting(_))));
        for wrong in [
            link.replace("teams.microsoft.com", "evil.example"),
            link.replace("https:", "http:"),
            link.replace("%2211111111", "%22..%2F11111111"),
            "https://teams.microsoft.com/meet/123?p=x".to_owned(),
            "not a link".to_owned(),
        ] {
            assert!(meeting(&wrong).is_err(), "{wrong}");
        }
    }

    #[test]
    fn the_window_says_three_things() {
        assert!(matches!(
            command("mute\n"),
            Some(CallCommand::Mute(MuteCommand::Mute))
        ));
        assert!(matches!(
            command("unmute"),
            Some(CallCommand::Mute(MuteCommand::Unmute))
        ));
        assert!(matches!(command("hangup"), Some(CallCommand::Hangup)));
        assert!(command("transfer 8:orgid:x").is_none());
    }
}
