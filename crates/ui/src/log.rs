//! The service's own account of what it did, for when something went wrong: what systemd kept
//! of it, or the Flatpak's service kept itself, in a file the default app for text opens.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use tern_ipc::Home;

/// How far back the log goes.
const SINCE: &str = "-2days";
const NAME: &str = "ternd.log";

/// Whether a log is kept: by systemd, which runs the installed service, or by the Flatpak's
/// service itself. A development instance and the AppImage start their own, whose output nobody
/// keeps.
pub fn kept() -> bool {
    match tern_ipc::home() {
        Home::Installed => std::env::var_os("TERN_INSTANCE").is_none(),
        Home::Flatpak(_) => true,
        Home::AppImage => false,
    }
}

/// Opens what the service wrote lately. Answers why it did not.
pub async fn open(window: &impl IsA<gtk::Window>) -> Result<(), String> {
    if !kept() {
        return Err("Only an installed Tern keeps a log".into());
    }
    let log = match tern_ipc::log_folder().filter(|_| tern_ipc::host::sandboxed()) {
        Some(folder) => {
            let now = chrono::Utc::now();
            gio::spawn_blocking(move || from_files(&folder, now))
                .await
                .map_err(|_| "The log could not be read".to_owned())??
        }
        None => read().await?,
    };
    let folder = tern_ipc::runtime_folder()
        .unwrap_or_else(|_| glib::user_runtime_dir().join("tern"))
        .join("logs");
    let path =
        save(&folder, &log).map_err(|error| format!("The log could not be saved: {error}"))?;
    gtk::FileLauncher::new(Some(&gio::File::for_path(&path)))
        .launch_future(Some(window))
        .await
        .map_err(|error| format!("The log could not be opened: {error}"))
}

async fn read() -> Result<String, String> {
    let arguments = [
        "journalctl",
        "--user",
        "--unit=ternd",
        &format!("--since={SINCE}"),
        "--output=short-iso",
        "--no-pager",
    ];
    let arguments: Vec<&std::ffi::OsStr> = arguments.iter().map(std::ffi::OsStr::new).collect();
    let child = gio::Subprocess::newv(
        &arguments,
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_PIPE,
    )
    .map_err(|error| format!("The log could not be read: {error}"))?;
    let (out, said) = child
        .communicate_utf8_future(None)
        .await
        .map_err(|error| format!("The log could not be read: {error}"))?;
    if !child.is_successful() {
        let said = said.unwrap_or_default();
        let last = said.lines().rev().find(|line| !line.trim().is_empty());
        return Err(format!(
            "The log could not be read: {}",
            last.unwrap_or("journalctl failed")
        ));
    }
    Ok(out.map(Into::into).unwrap_or_default())
}

/// The service's own files, from the last two days as the journal would give them.
fn from_files(folder: &Path, now: chrono::DateTime<chrono::Utc>) -> Result<String, String> {
    let since = now - chrono::Duration::days(2);
    let first = since.date_naive().pred_opt().unwrap_or(since.date_naive());
    let entries =
        std::fs::read_dir(folder).map_err(|error| format!("The log could not be read: {error}"))?;
    let mut days: Vec<(chrono::NaiveDate, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let day = name
                .strip_prefix(tern_ipc::LOG_NAME)?
                .strip_suffix(".log")?;
            let day = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
            (day >= first).then(|| (day, entry.path()))
        })
        .collect();
    days.sort();
    let mut log = String::new();
    // Lines without a time of their own go with the one before.
    let mut recent = false;
    for (_, path) in days {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let time = line
                .split_whitespace()
                .next()
                .and_then(|word| chrono::DateTime::parse_from_rfc3339(word).ok());
            if let Some(time) = time {
                recent = time >= since;
            }
            if recent {
                log.push_str(line);
                log.push('\n');
            }
        }
    }
    Ok(log)
}

/// Writes the log over the last one, readable only by the user.
fn save(folder: &Path, log: &str) -> std::io::Result<PathBuf> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(folder)?;
    let path = folder.join(NAME);
    // A new file each time, so that its mode is ours whatever was there.
    let _ = std::fs::remove_file(&path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(log.as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_log_is_for_the_user_alone_and_replaces_the_last() {
        let folder = tempfile::tempdir().unwrap();
        let inside = folder.path().join("logs");
        let path = save(&inside, "first").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let again = save(&inside, "second").unwrap();
        assert_eq!(path, again);
        assert_eq!(std::fs::read_to_string(&again).unwrap(), "second");
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&again), 0o600);
        assert_eq!(mode(&inside), 0o700);
    }

    #[test]
    fn the_files_give_the_last_two_days() {
        let folder = tempfile::tempdir().unwrap();
        let at = |name: &str| folder.path().join(name);
        std::fs::write(
            at("ternd-2026-11-20.log"),
            "2026-11-20T10:00:00.000000Z  INFO old\n",
        )
        .unwrap();
        std::fs::write(
            at("ternd-2026-11-25.log"),
            "2026-11-25T09:00:00.000000Z  INFO too old\n  and its rest\n\
             2026-11-25T11:00:00.000000Z  INFO kept\n  with its rest\n",
        )
        .unwrap();
        std::fs::write(
            at("ternd-2026-11-27.log"),
            "2026-11-27T08:00:00.000000Z  WARN now\n",
        )
        .unwrap();
        std::fs::write(at("notes.log"), "2026-11-27T08:00:00Z nothing\n").unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-27T10:00:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(
            from_files(folder.path(), now).unwrap(),
            "2026-11-25T11:00:00.000000Z  INFO kept\n  with its rest\n\
             2026-11-27T08:00:00.000000Z  WARN now\n"
        );
    }

    /// Needs a user journal: `-- --ignored --exact log::tests::the_journal_is_read`.
    #[test]
    #[ignore = "needs systemd's user journal"]
    fn the_journal_is_read() {
        let log = glib::MainContext::default().block_on(read()).unwrap();
        assert!(!log.is_empty());
    }

    /// Inside the Flatpak, after its service ran.
    #[test]
    #[ignore = "needs the Flatpak"]
    fn from_the_flatpak_the_services_own_log_is_read() {
        assert!(kept());
        let folder = tern_ipc::log_folder().unwrap();
        let log = from_files(&folder, chrono::Utc::now()).unwrap();
        assert!(log.contains("starting"), "{log}");
    }
}
