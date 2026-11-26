//! Meetings and other things only Teams itself shows, in a browser.
//!
//! A browser with Chromium's engine gets a window without its own buttons, and a profile of its own:
//! the sign-in there is for Teams alone. Any other browser gets the address as a page.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk4::{gio, glib};
use tern_engine::{Place, place};
use tern_ipc::Home;

const PROGRAMS: [&str; 11] = [
    "chromium",
    "chromium-browser",
    "google-chrome-stable",
    "google-chrome",
    "microsoft-edge-stable",
    "microsoft-edge",
    "brave",
    "brave-browser",
    "vivaldi-stable",
    "vivaldi",
    "thorium-browser",
];
const SANDBOXED: [&str; 6] = [
    "org.chromium.Chromium",
    "com.google.Chrome",
    "com.microsoft.Edge",
    "com.brave.Browser",
    "com.vivaldi.Vivaldi",
    "io.github.ungoogled_software.ungoogled_chromium",
];

const SCOPE: [&str; 6] = [
    "systemd-run",
    "--user",
    "--scope",
    "--quiet",
    "--collect",
    "--",
];
/// What ends this soon did not run.
const AT_ONCE: Duration = Duration::from_secs(2);

#[derive(Debug, PartialEq, Eq)]
struct Browser {
    command: Vec<OsString>,
    /// Only Teams is in there.
    profile: PathBuf,
}

/// What there is, the user's home being `home`.
fn find(path: &[PathBuf], home: &Path, data: &Path, system: &Path) -> Option<Browser> {
    let program = |name: &str| {
        path.iter()
            .map(|folder| folder.join(name))
            .find(|file| file.is_file())
    };
    if let Some(program) = PROGRAMS.iter().find_map(|name| program(name)) {
        return Some(Browser {
            command: vec![program.into()],
            profile: data.join("tern/browser"),
        });
    }
    let flatpak = program("flatpak")?;
    let there = |id: &&&str| {
        system.join("app").join(id).is_dir() || data.join("flatpak/app").join(id).is_dir()
    };
    let id = SANDBOXED.iter().find(there)?;
    Some(Browser {
        command: vec![flatpak.into(), "run".into(), id.into()],
        // Where it may write whatever its permissions are.
        profile: home.join(".var/app").join(id).join("data/tern"),
    })
}

fn browser() -> Option<Browser> {
    // A sandbox has no browsers in it.
    if matches!(tern_ipc::home(), Home::Flatpak(_)) {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    let path: Vec<PathBuf> = std::env::split_paths(&path).collect();
    find(
        &path,
        &glib::home_dir(),
        &glib::user_data_dir(),
        Path::new("/var/lib/flatpak"),
    )
}

/// The page of the meeting itself. The usual address leads to one that offers to open an app, which
/// would be this one.
pub fn direct(address: &str) -> String {
    let Some(Place::Meeting(address)) = place(address) else {
        return address.to_owned();
    };
    let rest = address.strip_prefix("https://").unwrap_or(&address);
    match rest.split_once('/') {
        // Personal accounts have their own pages.
        Some((host, rest)) if host != "teams.live.com" => {
            format!("https://{host}/v2/?meetingjoin=true#/{rest}")
        }
        _ => address,
    }
}

fn arguments(browser: &Browser, address: &str) -> Vec<OsString> {
    let mut profile = OsString::from("--user-data-dir=");
    profile.push(&browser.profile);
    vec![
        profile,
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        format!("--app={address}").into(),
    ]
}

/// Where the browser asks for the sign-in: a meeting's own page takes everyone else for a guest.
pub const TEAMS: &str = "https://teams.microsoft.com/v2/";
/// Says, in the profile, that the user signed in there.
const SIGNED_IN: &str = "tern-signed-in";

/// The link of a meeting that Tern joins itself, with sound: the usual links of Teams'
/// meetings. Others, such as those with a meeting ID, are for the browser.
pub fn here(address: &str) -> Option<String> {
    let address = tern_engine::meeting_address(address)?;
    let url = url::Url::parse(&address).ok()?;
    let teams = url.scheme() == "https"
        && url.username().is_empty()
        && url.port().is_none()
        && matches!(
            url.host_str(),
            Some("teams.microsoft.com" | "teams.cloud.microsoft" | "teams.live.com")
        )
        && url.path().starts_with("/l/meetup-join/");
    teams.then_some(address)
}

/// Whether the user would come to this meeting as a guest, for all we know. Another browser than
/// one with a profile of its own is the user's own affair.
pub fn as_guest(address: &str) -> bool {
    let meeting = tern_engine::meeting_address(address).is_some();
    meeting && browser().is_some_and(|browser| !browser.profile.join(SIGNED_IN).exists())
}

pub fn remember_sign_in() {
    if let Some(browser) = browser() {
        let kept = std::fs::create_dir_all(&browser.profile)
            .and_then(|()| std::fs::write(browser.profile.join(SIGNED_IN), ""));
        if let Err(error) = kept {
            eprintln!("tern: the sign-in is not remembered: {error}");
        }
    }
}

fn spawn(before: &[&str], browser: &Browser, address: &str) -> std::io::Result<Child> {
    let mut all: Vec<OsString> = before.iter().map(OsString::from).collect();
    all.extend(browser.command.iter().cloned());
    all.extend(arguments(browser, address));
    Command::new(&all[0])
        .args(&all[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
}

/// In a unit of its own, or the meeting would end with Tern's window. Without systemd, just beside.
/// Answers when it is clear that it runs.
fn start(browser: &Browser, address: &str) -> std::io::Result<()> {
    let begun = Instant::now();
    let mut child = match spawn(&SCOPE, browser, address) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => spawn(&[], browser, address)?,
        other => other?,
    };
    while begun.elapsed() < AT_ONCE {
        match child.try_wait()? {
            // It was running already, and took the address.
            Some(status) if status.success() => return Ok(()),
            // There is a systemd, but not for this user.
            Some(_) => {
                child = spawn(&[], browser, address)?;
                break;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Opens an address of Teams, and nothing else. `failed` is told why it did not work, and dropped
/// once it did.
pub fn go(address: &str, over: Option<gtk::Window>, failed: impl FnOnce(String) + 'static) {
    let address = match place(address) {
        Some(Place::Meeting(address)) => direct(&address),
        Some(Place::Other(address)) => address,
        Some(Place::Chat(_)) => address.to_owned(),
        None => return failed("This is not an address of Teams".into()),
    };
    glib::spawn_future_local(async move {
        if let Some(browser) = browser() {
            let to = address.clone();
            match gio::spawn_blocking(move || start(&browser, &to)).await {
                Ok(Ok(())) => return,
                Ok(Err(error)) => eprintln!("tern: the browser could not start: {error}"),
                Err(_) => {}
            }
        }
        let launcher = gtk::UriLauncher::new(&address);
        if let Err(error) = launcher.launch_future(over.as_ref()).await {
            failed(format!("The browser could not be opened: {error}"));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tern_joins_the_usual_meeting_links() {
        let link =
            "https://teams.microsoft.com/l/meetup-join/19%3ameeting_a%40thread.v2/0?context=%7b%7d";
        assert_eq!(here(link).as_deref(), Some(link));
        assert!(here(&link.replace("teams.microsoft.com", "teams.cloud.microsoft")).is_some());
        for elsewhere in [
            "https://teams.microsoft.com/meet/1234567?p=abc",
            "https://teams.microsoft.com/v2/",
            "https://example.com/l/meetup-join/19%3ameeting_a%40thread.v2/0",
            "not a link",
        ] {
            assert_eq!(here(elsewhere), None, "{elsewhere}");
        }
    }

    #[test]
    fn a_meeting_opens_without_the_page_that_offers_an_app() {
        assert_eq!(
            direct(
                "https://teams.microsoft.com/l/meetup-join/19%3ameeting_a%40thread.v2/0?context=%7b%7d"
            ),
            "https://teams.microsoft.com/v2/?meetingjoin=true#/l/meetup-join/19%3ameeting_a%40thread.v2/0?context=%7b%7d"
        );
        assert_eq!(
            direct("https://teams.microsoft.com/meet/12?p=x"),
            "https://teams.microsoft.com/v2/?meetingjoin=true#/meet/12?p=x"
        );
        let personal = "https://teams.live.com/meet/12?p=x";
        assert_eq!(direct(personal), personal);
    }

    #[test]
    fn installed_browsers_come_before_sandboxed_ones() {
        let folder = tempfile::tempdir().unwrap();
        let at = |name: &str| folder.path().join(name);
        let (bin, home, data, system) = (at("bin"), at("home"), at("data"), at("system"));
        std::fs::create_dir_all(&bin).unwrap();
        let path = [bin.clone()];
        assert_eq!(find(&path, &home, &data, &system), None);
        std::fs::create_dir_all(system.join("app/org.chromium.Chromium")).unwrap();
        assert_eq!(find(&path, &home, &data, &system), None, "without flatpak");
        std::fs::write(bin.join("flatpak"), "").unwrap();
        let found = find(&path, &home, &data, &system).unwrap();
        assert_eq!(found.command[1..], ["run", "org.chromium.Chromium"]);
        assert!(
            found
                .profile
                .starts_with(home.join(".var/app/org.chromium.Chromium"))
        );
        std::fs::write(bin.join("chromium"), "").unwrap();
        let found = find(&path, &home, &data, &system).unwrap();
        assert_eq!(found.command, [bin.join("chromium").into_os_string()]);
        assert_eq!(found.profile, data.join("tern/browser"));
    }

    #[test]
    fn an_address_is_one_argument_and_never_an_option() {
        let browser = Browser {
            command: vec!["b".into()],
            profile: "/p".into(),
        };
        let arguments = arguments(&browser, "https://teams.microsoft.com/v2/");
        assert_eq!(
            arguments.last().unwrap(),
            "--app=https://teams.microsoft.com/v2/"
        );
    }
}
