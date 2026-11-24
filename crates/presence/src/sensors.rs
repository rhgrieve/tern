use std::time::Duration;

use tokio::sync::mpsc;

use crate::logind::{self, SleepGuard};
use crate::model::Input;
use crate::{idle, mutter, network};

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub idle_after: Duration,
}

impl Default for Config {
    /// Teams' own idle time.
    fn default() -> Self {
        Config {
            idle_after: Duration::from_secs(5 * 60),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `exact`: only real input counts, not a playing video.
    Wayland {
        exact: bool,
    },
    Mutter,
    Logind,
}

#[derive(Debug)]
pub enum Event {
    Input(Input),
    /// Drop the guard once the away status is sent.
    Sleep(SleepGuard),
    Resume,
    NetworkUp,
    Ready(Source),
    Lost(Source, String),
}

const RETRY: Duration = Duration::from_secs(5);

pub struct Sensors {
    events: mpsc::UnboundedReceiver<Event>,
}

impl Sensors {
    /// Must be called inside a Tokio runtime.
    pub fn start(config: Config) -> Self {
        let (output, events) = mpsc::unbounded_channel();
        tokio::spawn(watch_idle(config.idle_after, output.clone()));
        tokio::spawn(watch_logind(output.clone()));
        tokio::spawn(watch_network(output));
        Sensors { events }
    }

    pub async fn recv(&mut self) -> Option<Event> {
        self.events.recv().await
    }
}

async fn watch_idle(after: Duration, output: mpsc::UnboundedSender<Event>) {
    while !output.is_closed() {
        let reason = match watch_wayland(after, &output).await {
            Err(idle::Error::Unsupported) => watch_mutter(after, &output).await,
            Err(error) => error.to_string(),
            Ok(()) => return,
        };
        // Without a sensor nobody can be shown as active.
        let _ = output.send(Event::Input(Input::Idle(true)));
        let _ = output.send(Event::Lost(Source::Wayland { exact: false }, reason));
        tokio::time::sleep(RETRY).await;
    }
}

async fn watch_wayland(
    after: Duration,
    output: &mpsc::UnboundedSender<Event>,
) -> Result<(), idle::Error> {
    let (signals, mut received) = mpsc::unbounded_channel();
    // A plain thread: it blocks for as long as the daemon runs and must not hold up shutdown.
    let (ended, end) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("wayland-idle".into())
        .spawn(move || {
            let _ = ended.send(idle::run(after, signals));
        })
        .map_err(|error| idle::Error::Lost(error.to_string()))?;
    while let Some(signal) = received.recv().await {
        let event = match signal {
            idle::Signal::Ready { exact } => Event::Ready(Source::Wayland { exact }),
            idle::Signal::Idle(idle) => Event::Input(Input::Idle(idle)),
        };
        if output.send(event).is_err() {
            return Ok(());
        }
    }
    end.await
        .unwrap_or_else(|_| Err(idle::Error::Lost("the watcher stopped".into())))
}

async fn watch_mutter(after: Duration, output: &mpsc::UnboundedSender<Event>) -> String {
    let (signals, mut received) = mpsc::unbounded_channel();
    let task = tokio::spawn(mutter::run(after, signals));
    let mut ready = false;
    while let Some(idle) = received.recv().await {
        if !ready {
            ready = true;
            let _ = output.send(Event::Ready(Source::Mutter));
        }
        if output.send(Event::Input(Input::Idle(idle))).is_err() {
            task.abort();
            break;
        }
    }
    match task.await {
        Ok(Err(error)) => format!("no idle source: {error}"),
        _ => "no idle source".into(),
    }
}

async fn watch_logind(output: mpsc::UnboundedSender<Event>) {
    while !output.is_closed() {
        let (signals, mut received) = mpsc::unbounded_channel();
        let task = tokio::spawn(logind::run(signals));
        while let Some(signal) = received.recv().await {
            let event = match signal {
                logind::Signal::Ready => Event::Ready(Source::Logind),
                logind::Signal::Locked(locked) => Event::Input(Input::Locked(locked)),
                logind::Signal::Foreground(front) => Event::Input(Input::Foreground(front)),
                logind::Signal::Sleep(guard) => Event::Sleep(guard),
                logind::Signal::Resume => Event::Resume,
            };
            if output.send(event).is_err() {
                return;
            }
        }
        let reason = match task.await {
            Ok(Err(error)) => error.to_string(),
            _ => "ended".into(),
        };
        let _ = output.send(Event::Lost(Source::Logind, reason));
        tokio::time::sleep(RETRY).await;
    }
}

async fn watch_network(output: mpsc::UnboundedSender<Event>) {
    let (signals, mut received) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        if let Err(error) = network::run(signals).await {
            tracing::info!(%error, "no NetworkManager; reconnects rely on timeouts");
        }
    });
    while received.recv().await.is_some() {
        if output.send(Event::NetworkUp).is_err() {
            return;
        }
    }
}
