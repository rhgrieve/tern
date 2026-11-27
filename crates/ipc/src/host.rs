//! Programs of the user's own computer: the editor, the browser, the sound player. Outside a sandbox
//! they are started as they are; from inside the Flatpak, on the computer itself by
//! `flatpak-spawn --host`, so that each feature works there as it does everywhere else.
//!
//! What runs there sees the computer's files and its session's environment, not the sandbox's.
//! Paths given to it must be ones both see alike: the home folder, `$XDG_RUNTIME_DIR/app/ID`.

use std::ffi::{OsStr, OsString};
use std::process::{Command, Stdio};

use crate::{Home, home};

const SPAWN: &str = "flatpak-spawn";

/// Whether programs of the computer have to be reached from a sandbox.
pub fn sandboxed() -> bool {
    matches!(home(), Home::Flatpak(_))
}

/// `home` is where it starts: the sandbox's own folder may not be there on the computer.
fn wrap(
    sandboxed: bool,
    watched: bool,
    home: Option<OsString>,
    command: Vec<OsString>,
) -> Vec<OsString> {
    if !sandboxed {
        return command;
    }
    let mut all: Vec<OsString> = vec![SPAWN.into(), "--host".into()];
    if let Some(home) = home {
        let mut directory = OsString::from("--directory=");
        directory.push(home);
        all.push(directory);
    }
    if watched {
        // It ends when the program that asked for it does, as a child would.
        all.push("--watch-bus".into());
    }
    all.extend(command);
    all
}

/// `command` as it runs on the computer. It outlives the program that started it.
pub fn command(command: Vec<OsString>) -> Vec<OsString> {
    wrap(sandboxed(), false, std::env::var_os("HOME"), command)
}

/// `command` as it runs on the computer, ended when this program ends.
pub fn watched(command: Vec<OsString>) -> Vec<OsString> {
    wrap(sandboxed(), true, std::env::var_os("HOME"), command)
}

/// What a `sh` script says on the computer, given `arguments` as `$1`…, when it succeeds.
pub fn script(script: &str, arguments: &[&OsStr]) -> Option<Vec<u8>> {
    let mut words: Vec<OsString> = vec!["sh".into(), "-c".into(), script.into(), "sh".into()];
    words.extend(arguments.iter().map(|word| (*word).to_owned()));
    let words = command(words);
    let out = Command::new(&words[0])
        .args(&words[1..])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn outside_a_sandbox_a_command_is_itself() {
        let home = Some("/home/a".into());
        assert_eq!(
            wrap(false, true, home, words(&["pw-play", "a"])),
            words(&["pw-play", "a"])
        );
    }

    #[test]
    fn inside_one_it_runs_on_the_computer() {
        assert_eq!(
            wrap(
                true,
                false,
                Some("/home/a".into()),
                words(&["code", "--wait", "f"])
            ),
            words(&[
                "flatpak-spawn",
                "--host",
                "--directory=/home/a",
                "code",
                "--wait",
                "f"
            ])
        );
        assert_eq!(
            wrap(true, true, None, words(&["pw-play", "a"])),
            words(&["flatpak-spawn", "--host", "--watch-bus", "pw-play", "a"])
        );
    }

    #[test]
    fn a_script_gets_its_arguments_and_says_what_it_printed() {
        let said = script(
            r#"printf '%s|%s' "$1" "$2""#,
            &[OsStr::new("a b"), OsStr::new("c")],
        );
        assert_eq!(said.as_deref(), Some(&b"a b|c"[..]));
        assert_eq!(script("exit 3", &[]), None);
    }

    /// Inside the Flatpak: `flatpak run --command=…` with the test program in `/app/bin`.
    #[test]
    #[ignore = "needs the Flatpak"]
    fn from_the_flatpak_the_computer_answers() {
        assert!(sandboxed());
        let said = script(
            r#"[ -e /.flatpak-info ] || printf '%s' "$1""#,
            &[OsStr::new("host")],
        );
        assert_eq!(said.as_deref(), Some(&b"host"[..]));
    }
}
