//! The other programs: the service, where nothing else starts it, and the sign-in.

use std::ffi::OsStr;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use gtk4::gio;
use tern_ipc::Home;

/// Starts the service unless systemd does. One that is already there makes this one end at once.
pub fn start() {
    if tern_ipc::home() == Home::Installed {
        return;
    }
    let child = Command::new(tern_ipc::program("ternd"))
        .stdin(Stdio::null())
        // It stays when the window is closed.
        .process_group(0)
        .spawn();
    match child {
        Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
        Err(error) => eprintln!("tern: the background service could not start: {error}"),
    }
}

/// Shows the sign-in. Answers why it did not work.
pub async fn sign_in() -> Result<(), String> {
    let program = tern_ipc::program("tern-cli");
    let child = gio::Subprocess::newv(
        &[program.as_os_str(), OsStr::new("login")],
        gio::SubprocessFlags::STDERR_PIPE | gio::SubprocessFlags::STDOUT_SILENCE,
    )
    .map_err(|error| error.to_string())?;
    let (_, said) = child
        .communicate_utf8_future(None)
        .await
        .map_err(|error| error.to_string())?;
    if child.is_successful() {
        return Ok(());
    }
    let said = said.unwrap_or_default();
    let last = said.lines().rev().find(|line| !line.trim().is_empty());
    Err(last.unwrap_or("Signing in did not work.").to_owned())
}
