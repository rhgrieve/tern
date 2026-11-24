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
        Ok(Response::Done) => Err("unexpected answer".into()),
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
