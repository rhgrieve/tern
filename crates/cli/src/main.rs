//! Command-line access to Tern: sign in, look at the daemon, check what works.

use std::io::Write as _;
use std::process::{ExitCode, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chatsvc::realtime::{Realtime, RealtimeConfig, RealtimeEvent};
use clap::{Parser, Subcommand};
use session::{Session, SessionConfig};
use tern_auth::{
    AUDIENCES, Broker, Error, HttpEndpoint, KeyringStore, SignIn, SignInRequest, TokenInfo,
};
use tern_ipc::{Request, Response, Status};
use tern_presence::{Config, Event, Machine, Sensors};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

#[derive(Parser)]
#[command(name = "tern-cli", about = "Tern: Teams for Linux", version)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Sign in to Teams in a window
    Login,
    /// Forget the sign-in on this computer
    Logout,
    /// Who is signed in
    Whoami,
    /// Try a token for every service and list what it allows (tokens are never printed)
    Tokens,
    /// Show push events as they arrive, without message contents
    Tail,
    /// Try message sync without writing anything, here or in Teams; prints counts only
    ProbeSync,
    /// List chats from the cache; works offline
    Chats {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Read a chat from the cache, by its number in `chats` or its id
    Read {
        chat: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Search cached messages; `from:name` filters the sender
    Search {
        words: Vec<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List the files shared in a chat, newest first
    Files { chat: String },
    /// Get a file by its number in `files`, from the cache or Teams, and print where it is
    Get { chat: String, number: usize },
    /// Send a message; waits for a connection if there is none
    Send { chat: String, text: String },
    /// What the background service is doing
    Status,
    /// Watch activity, lock and sleep detection; needs no account
    Sensors {
        /// Seconds without input until idle
        #[arg(long, default_value_t = 5)]
        idle: u64,
    },
}

/// The sign-in window was closed.
const CANCELLED: i32 = 2;

type Outcome = Result<(), String>;

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn broker() -> Result<Arc<Broker>, String> {
    let endpoint = HttpEndpoint::new().map_err(text)?;
    Ok(Broker::new(Box::new(endpoint), Box::new(KeyringStore)))
}

async fn signed_in() -> Result<(Arc<Broker>, tern_auth::Identity), String> {
    let broker = broker()?;
    let identity = broker
        .load()
        .await
        .map_err(text)?
        .ok_or("Not signed in. Run `tern-cli login`.")?;
    Ok((broker, identity))
}

/// Tells a running daemon about a changed sign-in.
async fn reload_daemon() {
    match tern_ipc::ask(&Request::Reload).await {
        Ok(Response::Error(error)) => eprintln!("The service could not use the sign-in: {error}"),
        Ok(_) => {}
        Err(_) => println!("The background service is not running."),
    }
}

/// The sign-in window, next to this program or on the path.
fn login_program() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .map(|path| path.with_file_name("tern-login"))
        .filter(|path| path.exists())
        .unwrap_or_else(|| "tern-login".into())
}

/// Shows the sign-in window and answers the redirect it ended on.
async fn redirect_from_window(request: &SignInRequest) -> Result<String, String> {
    const HINT: &str = "Run `tern-login` by itself to see why.";
    let mut child = Command::new(login_program())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("The sign-in window could not start ({error}). {HINT}"))?;
    let mut line = serde_json::to_string(request).map_err(text)?;
    line.push('\n');
    if let Some(mut stdin) = child.stdin.take() {
        // A window that failed to start closes its end; the exit code below explains that.
        let _ = stdin.write_all(line.as_bytes()).await;
    }
    let output = child.wait_with_output().await.map_err(text)?;
    match output.status.code() {
        Some(0) => String::from_utf8(output.stdout).map_err(text),
        Some(CANCELLED) => Err("Sign-in was cancelled.".into()),
        _ => Err(format!("The sign-in window could not start. {HINT}")),
    }
}

async fn login() -> Outcome {
    let broker = broker()?;
    // Without a keyring the sign-in would be lost again; say so before asking for a password.
    broker.load().await.map_err(text)?;
    let attempt = SignIn::new();
    let request = attempt.request(None);
    let redirect = redirect_from_window(&request).await?;
    match broker.sign_in(&attempt, redirect.trim()).await {
        Ok(identity) => {
            println!("Signed in as {}.", identity.name);
            reload_daemon().await;
            Ok(())
        }
        Err(error @ Error::Blocked(_)) => Err(format!(
            "{error}. Your organization does not allow this sign-in; that needs the web fallback or a word with IT."
        )),
        Err(error) => Err(error.to_string()),
    }
}

async fn logout() -> Outcome {
    broker()?.sign_out().await.map_err(text)?;
    println!("Signed out on this computer.");
    reload_daemon().await;
    Ok(())
}

async fn tokens() -> Outcome {
    let (broker, _) = signed_in().await?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut failed = 0;
    for (name, resource) in AUDIENCES {
        match broker.access(resource, None, false).await {
            Ok(token) => {
                let info = TokenInfo::of(&token);
                let minutes = info.expires_at.map_or(0, |at| at.saturating_sub(now) / 60);
                println!("ok    {name}: {minutes} min, {} scopes", info.scopes.len());
                if name == "Microsoft Graph" {
                    for wanted in [
                        "ChannelMessage.Read.All",
                        "Chat.ReadWrite",
                        "Presence.ReadWrite",
                        "Calendars.ReadWrite",
                        "Files.ReadWrite.All",
                    ] {
                        let has = info.scopes.iter().any(|scope| scope == wanted);
                        println!("      {} {wanted}", if has { "+" } else { "-" });
                    }
                }
            }
            Err(error) => {
                failed += 1;
                println!("FAIL  {name}: {error}");
            }
        }
    }
    match broker.regions().await {
        Ok(regions) => println!(
            "ok    region {}: presence {}, push {}",
            regions.region,
            regions.presence.as_deref().unwrap_or("default"),
            regions.trouter_host.as_deref().unwrap_or("default"),
        ),
        Err(error) => {
            failed += 1;
            println!("FAIL  region: {error}");
        }
    }
    if failed > 0 {
        Err(format!("{failed} checks failed"))
    } else {
        Ok(())
    }
}

async fn tail() -> Outcome {
    let (broker, _) = signed_in().await?;
    let regions = broker.regions().await.map_err(text)?;
    let session = Session::with_provider(broker, SessionConfig::default());
    let config = RealtimeConfig {
        host: regions.trouter_host,
        discover_host: false,
        native: true,
        ..RealtimeConfig::default()
    };
    let mut realtime = Realtime::start_with(&session, config).await.map_err(text)?;
    println!("Connected to {}. Ctrl+C stops.", realtime.host());
    loop {
        let event = tokio::select! {
            event = realtime.recv() => event,
            _ = tokio::signal::ctrl_c() => break,
        };
        match event {
            None => return Err("the push connection ended".into()),
            Some(RealtimeEvent::Message(message)) => println!("message   {:?}", message.kind),
            Some(RealtimeEvent::Typing(_)) => println!("typing"),
            Some(RealtimeEvent::Presence(updates)) => {
                println!("presence  {} people", updates.len())
            }
            Some(RealtimeEvent::Endpoint(_)) => println!("endpoint  assigned"),
            Some(RealtimeEvent::Status(status)) => {
                println!("status    {:?} {}", status.kind, status.detail)
            }
        }
    }
    realtime.stop().await;
    Ok(())
}

/// An address without ids or parameters.
fn skeleton(text: &str) -> String {
    text.split(['?', ' '])
        .next()
        .unwrap_or_default()
        .split('/')
        .map(|part| {
            let id = part.contains("%3A")
                || part.contains(':') && !part.ends_with(':')
                || part.len() > 30
                || part.len() > 8 && part.bytes().filter(u8::is_ascii_digit).count() > 4;
            if id { "{id}" } else { part }
        })
        .collect::<Vec<_>>()
        .join("/")
}

async fn probe_sync() -> Outcome {
    use tern_engine::{Backend, Teams, read_only_guard};
    let (broker, _) = signed_in().await?;
    let regions = broker.regions().await.map_err(text)?;
    let seen: Arc<std::sync::Mutex<std::collections::BTreeMap<String, usize>>> = Arc::default();
    let (note, allows) = (seen.clone(), read_only_guard());
    let session = Session::with_provider(broker, SessionConfig::default())
        .with_region(&regions.region)
        .with_guard(Arc::new(move |request| {
            let allowed = allows(request);
            let line = format!(
                "{} {} {}",
                if allowed { "sent   " } else { "REFUSED" },
                request.method.as_str(),
                skeleton(&request.url)
            );
            *note.lock().expect("log").entry(line).or_default() += 1;
            allowed
        }));
    let store = Arc::new(store::Store::open_in_memory().map_err(text)?);
    let teams = Teams::new(session, store.clone());
    let mut reports = teams.engine().subscribe();
    let start = std::time::Instant::now();
    let refreshed = teams.refresh().await;
    println!(
        "refresh: {:?} in {} ms",
        refreshed.as_ref().map_err(|failure| failure.text()),
        start.elapsed().as_millis()
    );
    let sidebar = store.sidebar().map_err(text)?;
    println!(
        "chats {}, teams {}, channels {}, unread chats {}",
        sidebar.chats.len(),
        sidebar.teams.len(),
        sidebar
            .teams
            .iter()
            .map(|team| team.channels.len())
            .sum::<usize>(),
        sidebar.chats.iter().filter(|chat| chat.unread).count(),
    );
    let behind = teams.behind();
    println!("behind: {}", behind.len());
    let mut channels: Vec<_> = sidebar
        .teams
        .iter()
        .flat_map(|team| &team.channels)
        .collect();
    channels.sort_by_key(|channel| std::cmp::Reverse(channel.last_message_at));
    let channels: Vec<String> = channels
        .into_iter()
        .take(4)
        .map(|channel| channel.id.clone())
        .collect();
    for (kind, id) in behind
        .iter()
        .take(2)
        .map(|id| ("chat", id))
        .chain(channels.iter().map(|id| ("channel", id)))
    {
        let start = std::time::Instant::now();
        match teams.fetch(id).await {
            Ok(messages) => println!(
                "{kind}: {} messages, {} events, {} from apps, {} with a sender, {} readable, {} ms",
                messages.len(),
                messages
                    .iter()
                    .filter(|message| message.call_event_json.is_some())
                    .count(),
                messages
                    .iter()
                    .filter(|message| message.sender_application_id.is_some())
                    .count(),
                messages
                    .iter()
                    .filter(|message| message.sender_name.is_some())
                    .count(),
                messages
                    .iter()
                    .filter(|message| !teams_core::readable_text(message).is_empty())
                    .count(),
                start.elapsed().as_millis()
            ),
            Err(failure) => println!("{kind}: FAILED {}", failure.text()),
        }
    }
    while let Ok(event) = reports.try_recv() {
        if let teams_core::CoreEvent::Error { message } = event {
            let words: Vec<String> = message.split(' ').map(skeleton).collect();
            println!(
                "engine: {}",
                words.join(" ").chars().take(220).collect::<String>()
            );
        }
    }
    println!("requests:");
    for (line, count) in seen.lock().expect("log").iter() {
        println!("  {count:3} {line}");
    }
    Ok(())
}

async fn cache() -> Result<store::Store, String> {
    let (_, identity) = signed_in().await?;
    tern_engine::open_read_only(&identity.object_id)
}

/// Other people's text: control characters must not reach the terminal.
fn printable(text: &str) -> String {
    text.chars()
        .map(|letter| {
            if letter.is_control() && letter != '\n' {
                ' '
            } else {
                letter
            }
        })
        .collect()
}

fn one_line(text: &str) -> String {
    printable(text).replace('\n', " ")
}

fn local(at: chrono::DateTime<chrono::Utc>) -> String {
    at.with_timezone(&chrono::Local)
        .format("%a %d %b %H:%M")
        .to_string()
}

/// A chat by its number in `chats`, or by its id. The notes chat is `notes`.
fn chat_id(store: &store::Store, chat: &str) -> Result<String, String> {
    if chat == "notes" {
        return Ok("48:notes".into());
    }
    let Ok(number) = chat.parse::<usize>() else {
        return Ok(chat.to_owned());
    };
    let chats = store.recent_chats(number).map_err(text)?;
    number
        .checked_sub(1)
        .and_then(|index| chats.get(index))
        .map(|chat| chat.id.clone())
        .ok_or_else(|| "There is no chat with that number.".into())
}

async fn chats(limit: usize) -> Outcome {
    for (index, chat) in cache()
        .await?
        .recent_chats(limit)
        .map_err(text)?
        .iter()
        .enumerate()
    {
        println!(
            "{:3} {} {:16} {}",
            index + 1,
            if chat.unread { "*" } else { " " },
            chat.last_message_at.map(local).unwrap_or_default(),
            one_line(&chat.display_title())
        );
    }
    Ok(())
}

async fn read(chat: &str, limit: usize) -> Outcome {
    let store = cache().await?;
    let id = chat_id(&store, chat)?;
    let messages = store.messages(&id, None, limit).map_err(text)?;
    if messages.is_empty() {
        return Err("Nothing cached for this chat.".into());
    }
    for message in messages {
        let body = if message.deleted {
            "(deleted)".to_owned()
        } else {
            teams_core::readable_text(&message)
        };
        println!(
            "{}  {}\n    {}",
            local(message.created_at),
            one_line(message.sender_name.as_deref().unwrap_or("")),
            printable(&body).replace('\n', "\n    ")
        );
    }
    Ok(())
}

async fn search(query: &str, limit: usize) -> Outcome {
    let store = cache().await?;
    let hits = store.search_messages(query, None, limit).map_err(text)?;
    if hits.is_empty() {
        return Err("Nothing found.".into());
    }
    for hit in hits {
        let place = store
            .chat(&hit.conversation_id)
            .ok()
            .flatten()
            .map_or_else(|| "channel".to_owned(), |chat| chat.display_title());
        let snippet: String = hit
            .snippet
            .chars()
            .filter(|letter| !matches!(*letter, store::HIGHLIGHT_START | store::HIGHLIGHT_END))
            .collect();
        println!(
            "{}  {} in {}\n    {}",
            local(hit.created_at),
            one_line(hit.sender_name.as_deref().unwrap_or("")),
            one_line(&place),
            one_line(&snippet)
        );
    }
    Ok(())
}

/// The files in a chat's cached messages, newest first: message, index in it, name.
fn shared_files(
    store: &store::Store,
    id: &str,
) -> Result<Vec<(store::MessageRecord, usize, String)>, String> {
    let messages = store.messages(id, None, 500).map_err(text)?;
    let mut all = Vec::new();
    for message in messages
        .into_iter()
        .rev()
        .filter(|message| !message.deleted)
    {
        let files = teams_core::files(&message).into_iter().filter(|file| {
            file.kind != teams_core::FileKind::Loop && file.open_url.starts_with("https://")
        });
        for (index, file) in files.enumerate() {
            all.push((message.clone(), index, file.name));
        }
    }
    Ok(all)
}

async fn list_files(chat: &str) -> Outcome {
    let store = cache().await?;
    let id = chat_id(&store, chat)?;
    let files = shared_files(&store, &id)?;
    if files.is_empty() {
        return Err("No files in the cached messages of this chat.".into());
    }
    for (number, (message, _, name)) in files.iter().enumerate() {
        println!(
            "{:3}  {}  {} from {}",
            number + 1,
            local(message.created_at),
            one_line(name),
            one_line(message.sender_name.as_deref().unwrap_or("someone"))
        );
    }
    Ok(())
}

async fn get_file(chat: &str, number: usize) -> Outcome {
    let store = cache().await?;
    let id = chat_id(&store, chat)?;
    let (message, index, _) = number
        .checked_sub(1)
        .and_then(|at| shared_files(&store, &id).ok()?.into_iter().nth(at))
        .ok_or("There is no file with that number.")?;
    let request = Request::File {
        conversation: id,
        message: message.message_id,
        index,
    };
    match tern_ipc::ask_within(&request, Duration::from_secs(30 * 60)).await {
        Ok(Response::Path(path)) => {
            println!("{}", path.display());
            Ok(())
        }
        Ok(Response::Error(error)) => Err(error),
        Ok(_) => Err("unexpected answer".into()),
        Err(error) => Err(format!("The background service is not running ({error}).")),
    }
}

async fn send(chat: &str, message: String) -> Outcome {
    let conversation = chat_id(&cache().await?, chat)?;
    match tern_ipc::ask(&Request::Send {
        conversation,
        text: message,
    })
    .await
    {
        Ok(Response::Done) => {
            println!("Queued.");
            Ok(())
        }
        Ok(Response::Error(error)) => Err(error),
        Ok(_) => Err("unexpected answer".into()),
        Err(error) => Err(format!("The background service is not running ({error}).")),
    }
}

fn print_status(status: &Status) {
    let line = |name: &str, value: &str| println!("{name:<12} {value}");
    line(
        "Account",
        status.account.as_deref().unwrap_or("not signed in"),
    );
    line(
        "You are",
        &format!("{} for {} s", status.state, status.state_for_secs),
    );
    line(
        "Teams shows",
        &match (&status.teams_shows, status.teams_read_secs_ago) {
            (Some(shown), Some(ago)) => format!("{shown} (read {ago} s ago)"),
            _ => "not read yet".into(),
        },
    );
    line("Push", &status.push);
    line("Registered", if status.registered { "yes" } else { "no" });
    line(
        "Idle from",
        status
            .idle_source
            .as_deref()
            .unwrap_or("NOTHING: you would show as away"),
    );
    line(
        "Lock, sleep",
        if status.lock_source {
            "logind"
        } else {
            "NOT AVAILABLE"
        },
    );
    line(
        "Messages",
        &match (status.synced, status.reading) {
            (false, _) => "reading the chat list".to_owned(),
            (true, 0) => "up to date".to_owned(),
            (true, count) => format!("reading {count} conversations"),
        },
    );
    line(
        "Media",
        &format!(
            "{} pictures and files, {} MB",
            status.media.0,
            status.media.1 / 1_000_000
        ),
    );
    if status.unsent > 0 {
        line("Unsent", &status.unsent.to_string());
    }
    if let Some(problem) = &status.sync_problem {
        line("Sync", problem);
    }
    if let Some(problem) = &status.problem {
        line("Problem", problem);
    }
}

async fn status() -> Outcome {
    match tern_ipc::ask(&Request::Status).await {
        Ok(Response::Status(status)) => {
            print_status(&status);
            Ok(())
        }
        Ok(Response::Error(error)) => Err(error),
        Ok(_) => Err("unexpected answer".into()),
        Err(error) => Err(format!("The background service is not running ({error}).")),
    }
}

async fn sensors(idle: u64) -> Outcome {
    let mut sensors = Sensors::start(Config {
        idle_after: Duration::from_secs(idle),
    });
    let mut machine = Machine::default();
    let start = std::time::Instant::now();
    println!("Idle after {idle} s without input anywhere. Ctrl+C stops.");
    loop {
        let event = tokio::select! {
            Some(event) = sensors.recv() => event,
            _ = tokio::signal::ctrl_c() => return Ok(()),
        };
        let at = start.elapsed().as_secs_f32();
        match event {
            Event::Input(input) => {
                let change = machine.apply(input);
                print!("{at:8.1}  {input:?}");
                match change {
                    Some(state) => println!("  -> {state:?}"),
                    None => println!(),
                }
            }
            other => println!("{at:8.1}  {other:?}"),
        }
        let _ = std::io::stdout().flush();
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let outcome = match Cli::parse().command {
        Cmd::Login => login().await,
        Cmd::Logout => logout().await,
        Cmd::Whoami => signed_in()
            .await
            .map(|(_, identity)| println!("{}", identity.name)),
        Cmd::Tokens => tokens().await,
        Cmd::Tail => tail().await,
        Cmd::Chats { limit } => chats(limit).await,
        Cmd::Read { chat, limit } => read(&chat, limit).await,
        Cmd::Search { words, limit } => search(&words.join(" "), limit).await,
        Cmd::Files { chat } => list_files(&chat).await,
        Cmd::Get { chat, number } => get_file(&chat, number).await,
        Cmd::Send { chat, text } => send(&chat, text).await,
        Cmd::ProbeSync => probe_sync().await,
        Cmd::Status => status().await,
        Cmd::Sensors { idle } => sensors(idle).await,
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
