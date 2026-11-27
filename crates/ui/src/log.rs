//! The service's own account of what it did, for when something went wrong: what systemd kept
//! of it, in a file the default app for text opens.

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

/// Whether a log is kept: only by systemd, which runs the installed service. A development
/// instance, the AppImage and the Flatpak start their own, whose output nobody keeps.
pub fn kept() -> bool {
    tern_ipc::home() == Home::Installed && std::env::var_os("TERN_INSTANCE").is_none()
}

/// Opens what the service wrote lately. Answers why it did not.
pub async fn open(window: &impl IsA<gtk::Window>) -> Result<(), String> {
    if !kept() {
        return Err("Only an installed Tern keeps a log".into());
    }
    let log = read().await?;
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

    /// Needs a user journal: `-- --ignored --exact log::tests::the_journal_is_read`.
    #[test]
    #[ignore = "needs systemd's user journal"]
    fn the_journal_is_read() {
        let log = glib::MainContext::default().block_on(read()).unwrap();
        assert!(!log.is_empty());
    }
}
