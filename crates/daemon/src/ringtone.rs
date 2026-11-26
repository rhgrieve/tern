//! A ringtone while a call rings: the desktop's sound for it, played again and again by PipeWire,
//! or else PulseAudio, until it is stopped. Notification servers that play sounds play it once.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::task::JoinHandle;

use crate::calls::RINGING;

/// The sound theme's name for it.
const SOUND: &str = "phone-incoming-call";
/// Between one ring and the next.
const PAUSE: Duration = Duration::from_millis(800);
/// Players of a sound file, the first one there is.
const PLAYERS: [&str; 2] = ["pw-play", "paplay"];

#[derive(Default)]
pub struct Ringtone(Option<JoinHandle<()>>);

impl Ringtone {
    /// Rings until stopped, and never longer than a call rings.
    pub fn start(&mut self) {
        if self.0.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        let Some(sound) = find_sound(&sound_folders()) else {
            tracing::info!("no ringtone: the sound theme has no {SOUND}");
            return;
        };
        self.0 = Some(tokio::spawn(async move {
            let _ = tokio::time::timeout(RINGING + PAUSE, ring(&sound)).await;
        }));
    }

    /// The player is stopped with the task.
    pub fn stop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

impl Drop for Ringtone {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn ring(sound: &Path) {
    let mut players = PLAYERS.iter().peekable();
    while let Some(player) = players.peek() {
        let played = Command::new(player)
            .arg(sound)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn();
        match played {
            Ok(mut child) => {
                let _ = child.wait().await;
                tokio::time::sleep(PAUSE).await;
            }
            Err(_) => {
                players.next();
            }
        }
    }
    tracing::info!("no ringtone: neither pw-play nor paplay is here");
}

/// Where sound themes are, the user's own first.
fn sound_folders() -> Vec<PathBuf> {
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    let shared = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    home.into_iter()
        .chain(
            shared
                .split(':')
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from),
        )
        .map(|dir| dir.join("sounds/freedesktop/stereo"))
        .collect()
}

fn find_sound(folders: &[PathBuf]) -> Option<PathBuf> {
    folders.iter().find_map(|folder| {
        ["oga", "ogg", "wav"]
            .iter()
            .map(|kind| folder.join(format!("{SOUND}.{kind}")))
            .find(|path| path.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_theme_that_has_the_sound_is_taken() {
        let base = std::env::temp_dir().join(format!("tern-ringtone-{}", std::process::id()));
        let (none, user, system) = (base.join("none"), base.join("user"), base.join("system"));
        for dir in [&none, &user, &system] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(user.join("phone-incoming-call.wav"), b"").unwrap();
        std::fs::write(system.join("phone-incoming-call.oga"), b"").unwrap();
        let found = find_sound(&[none.clone(), user.clone(), system.clone()]);
        assert_eq!(found, Some(user.join("phone-incoming-call.wav")));
        assert_eq!(find_sound(&[none]), None);
        std::fs::remove_dir_all(base).unwrap();
    }
}
