//! A ringtone while a call rings: the desktop's sound for it, played again and again by PipeWire,
//! or else PulseAudio, until it is stopped. Notification servers that play sounds play it once.
//! From the Flatpak, the sound theme and the player are the computer's, as they are elsewhere.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
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
        let found = if tern_ipc::host::sandboxed() {
            find_on_host()
        } else {
            find_sound(&sound_folders())
        };
        let Some(sound) = found else {
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
    'players: for player in PLAYERS {
        let mut played = false;
        loop {
            let words = tern_ipc::host::watched(vec![player.into(), sound.into()]);
            let child = Command::new(&words[0])
                .args(&words[1..])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn();
            let Ok(mut child) = child else {
                continue 'players;
            };
            let ok = child.wait().await.is_ok_and(|status| status.success());
            // From a sandbox a player that is not there fails rather than not starting.
            if !ok && !played {
                continue 'players;
            }
            played = true;
            tokio::time::sleep(PAUSE).await;
        }
    }
    tracing::info!("no ringtone: neither pw-play nor paplay is here");
}

/// What [`find_sound`] finds in [`sound_folders`], looked for by the computer itself.
const ON_HOST: &str = r#"IFS=:
for dir in "${XDG_DATA_HOME:-$HOME/.local/share}" ${XDG_DATA_DIRS:-/usr/local/share:/usr/share}; do
    for kind in oga ogg wav; do
        file="$dir/sounds/freedesktop/stereo/$1.$kind"
        [ -n "$dir" ] && [ -f "$file" ] && { printf '%s' "$file"; exit 0; }
    done
done
exit 1"#;

fn find_on_host() -> Option<PathBuf> {
    let found = tern_ipc::host::script(ON_HOST, &[OsStr::new(SOUND)])?;
    (!found.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(&found)))
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

    #[test]
    fn the_computer_is_searched_the_same_way() {
        let base = std::env::temp_dir().join(format!("tern-ringtone-sh-{}", std::process::id()));
        let (user, system) = (base.join("user"), base.join("system"));
        for dir in [&user, &system] {
            std::fs::create_dir_all(dir.join("sounds/freedesktop/stereo")).unwrap();
        }
        let sound =
            |dir: &Path, kind: &str| dir.join(format!("sounds/freedesktop/stereo/{SOUND}.{kind}"));
        std::fs::write(sound(&system, "oga"), b"").unwrap();
        let run = |home: &Path| {
            let out = std::process::Command::new("sh")
                .args(["-c", ON_HOST, "sh", SOUND])
                .env("XDG_DATA_HOME", home)
                .env("XDG_DATA_DIRS", format!("::{}", system.display()))
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        assert_eq!(run(&user), sound(&system, "oga").to_str().unwrap());
        std::fs::write(sound(&user, "wav"), b"").unwrap();
        assert_eq!(run(&user), sound(&user, "wav").to_str().unwrap());
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Inside the Flatpak, with the computer's sound theme.
    #[test]
    #[ignore = "needs the Flatpak"]
    fn from_the_flatpak_the_theme_is_the_computers() {
        assert!(tern_ipc::host::sandboxed());
        let sound = find_on_host().expect("the computer's sound");
        assert!(sound.to_string_lossy().ends_with("phone-incoming-call.oga"));
    }
}
