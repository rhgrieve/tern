//! Writing a message in the user's own editor: which one, in which terminal, the file it edits,
//! and what comes back. No widgets, so it can be tested.
//!
//! The editor is the default app for Markdown or else plain text, as the desktop sets it; else
//! `$VISUAL`, else `$EDITOR`, which an app started from the desktop often does not see the shell's
//! values of. One that draws in a
//! terminal runs in one: `$TERMINAL`, else the first of [`TERMINALS`] there is. In a terminal the
//! editor runs inside `sh`, which removes a marker file when the editor ends, so it does not
//! matter whether the terminal waits. A graphical editor is asked to wait where it can be.
//!
//! From the Flatpak all of it is the computer's: its settings, programs and default app, asked for
//! once each time, and the editor runs there. The file is in a folder both see alike.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use tern_ipc::host;

use crate::composer::Written;
use crate::mention::{self, Person};

/// Editors that draw in a terminal.
const IN_TERMINAL: [&str; 21] = [
    "vi", "vim", "nvim", "nano", "pico", "micro", "hx", "helix", "kak", "joe", "jed", "jstar",
    "ne", "mg", "vis", "amp", "dte", "mcedit", "ed", "ex", "elvis",
];

/// Graphical editors that return at once unless asked to wait, and how to ask.
const WAIT: [(&str, &str); 17] = [
    ("code", "--wait"),
    ("code-oss", "--wait"),
    ("code-insiders", "--wait"),
    ("codium", "--wait"),
    ("vscodium", "--wait"),
    ("cursor", "--wait"),
    ("subl", "--wait"),
    ("zed", "--wait"),
    ("zeditor", "--wait"),
    ("pulsar", "--wait"),
    ("gvim", "--nofork"),
    ("mvim", "--nofork"),
    ("kate", "--block"),
    ("gedit", "--wait"),
    ("xed", "--wait"),
    ("gnome-text-editor", "--standalone"),
    ("geany", "--new-instance"),
];

/// Terminals to try, in order, after `$TERMINAL`.
pub const TERMINALS: [&str; 11] = [
    "xdg-terminal-exec",
    "kgx",
    "gnome-terminal",
    "ptyxis",
    "foot",
    "kitty",
    "alacritty",
    "wezterm",
    "konsole",
    "xterm",
    "x-terminal-emulator",
];

/// What the user's session says.
#[derive(Debug, Clone, Default)]
pub struct Setting {
    pub visual: Option<String>,
    pub editor: Option<String>,
    pub terminal: Option<String>,
    /// The folders of `$PATH`.
    pub path: Vec<PathBuf>,
    /// The default app for Markdown, else for text: its command, and whether it wants a terminal.
    pub default: Option<(Vec<String>, bool)>,
    /// Where the computer found programs, when it is not this one that looks: by name, or by path
    /// for those given with a folder. What is missing is not there.
    pub found: Option<HashMap<String, PathBuf>>,
}

impl Setting {
    /// This session's.
    pub fn here() -> Setting {
        if host::sandboxed() {
            return Setting::on_host().unwrap_or_default();
        }
        let var = |name| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
        };
        let path = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default();
        Setting {
            visual: var("VISUAL"),
            editor: var("EDITOR"),
            terminal: var("TERMINAL"),
            path,
            default: default_app(),
            found: None,
        }
    }

    /// The computer's, asked from a sandbox: what it says, then where its programs are.
    fn on_host() -> Option<Setting> {
        let said = host::script(SETTING, &[])?;
        let mut setting = from_host(&said);
        let names = setting.wanted();
        let names: Vec<&OsStr> = names.iter().map(OsStr::new).collect();
        setting.found = Some(found(&host::script(FIND, &names)?));
        Some(setting)
    }

    /// The programs [`plan`] may look for.
    fn wanted(&self) -> Vec<String> {
        let first = |value: &Option<String>| value.as_deref().and_then(words).map(|w| w[0].clone());
        let default = self.default.as_ref().map(|(words, _)| words[0].clone());
        [
            default,
            first(&self.visual),
            first(&self.editor),
            first(&self.terminal),
        ]
        .into_iter()
        .flatten()
        .chain(["sh", "xdg-open"].map(String::from))
        .chain(TERMINALS.map(String::from))
        .collect()
    }

    /// Where `program` is: itself when it has a folder, else the first in `$PATH`.
    fn find(&self, program: &str) -> Option<PathBuf> {
        if let Some(found) = &self.found {
            return found.get(program).cloned();
        }
        if program.contains('/') {
            let path = PathBuf::from(program);
            return runnable(&path).then_some(path);
        }
        self.path
            .iter()
            .map(|folder| folder.join(program))
            .find(|file| runnable(file))
    }
}

fn runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// The default app for Markdown, else for plain text, without its `%f` and the like.
fn default_app() -> Option<(Vec<String>, bool)> {
    let app = gio::AppInfo::default_for_type("text/markdown", false)
        .or_else(|| gio::AppInfo::default_for_type("text/plain", false))?;
    let line = app.commandline()?;
    let words = glib::shell_parse_argv(line.as_os_str()).ok()?;
    let words: Vec<String> = words
        .iter()
        .map(|word| word.to_string_lossy().into_owned())
        .filter(|word| !(word.len() == 2 && word.starts_with('%')))
        .collect();
    let terminal = app.id().is_some_and(|id| wants_terminal(&id));
    (!words.is_empty()).then_some((words, terminal))
}

/// What the computer's session says, `\0` after each: `$VISUAL`, `$EDITOR`, `$TERMINAL`, `$PATH`,
/// then, when there is one, the id of the default app for Markdown or else text, and its desktop
/// file.
const SETTING: &str = r#"printf '%s\0' "${VISUAL-}" "${EDITOR-}" "${TERMINAL-}" "${PATH-}"
id=$(xdg-mime query default text/markdown 2>/dev/null)
[ -n "$id" ] || id=$(xdg-mime query default text/plain 2>/dev/null)
[ -n "$id" ] || exit 0
IFS=:
for dir in "${XDG_DATA_HOME:-$HOME/.local/share}" ${XDG_DATA_DIRS:-/usr/local/share:/usr/share}; do
    if [ -n "$dir" ] && [ -f "$dir/applications/$id" ]; then
        printf '%s\0' "$id"
        cat "$dir/applications/$id"
        exit 0
    fi
done"#;

/// Where each of the programs given is, as [`Setting::find`] would say: name, `\0`, path, `\0`.
const FIND: &str = r#"for name; do
    case $name in
    */*) [ -f "$name" ] && [ -x "$name" ] && printf '%s\0%s\0' "$name" "$name" ;;
    *) found=$(command -v -- "$name") && case $found in
        /*) [ -f "$found" ] && [ -x "$found" ] && printf '%s\0%s\0' "$name" "$found" ;;
        esac ;;
    esac
done
exit 0"#;

fn from_host(said: &[u8]) -> Setting {
    let mut parts = said.splitn(6, |byte| *byte == 0);
    let mut next = || {
        let part = String::from_utf8_lossy(parts.next().unwrap_or_default()).into_owned();
        (!part.trim().is_empty()).then_some(part)
    };
    let (visual, editor, terminal, path) = (next(), next(), next(), next());
    let id = next();
    let desktop = next();
    Setting {
        visual,
        editor,
        terminal,
        path: path
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default(),
        default: id.zip(desktop).and_then(|(_, desktop)| app_of(&desktop)),
        found: None,
    }
}

fn found(said: &[u8]) -> HashMap<String, PathBuf> {
    let mut parts = said.split(|byte| *byte == 0);
    let mut found = HashMap::new();
    while let (Some(name), Some(path)) = (parts.next(), parts.next()) {
        if !name.is_empty() && path.starts_with(b"/") {
            let name = String::from_utf8_lossy(name).into_owned();
            found.insert(name, PathBuf::from(OsStr::from_bytes(path)));
        }
    }
    found
}

/// The command of a desktop file, without its `%f` and the like, and whether it wants a terminal.
fn app_of(desktop: &str) -> Option<(Vec<String>, bool)> {
    let keys = glib::KeyFile::new();
    keys.load_from_data(desktop, glib::KeyFileFlags::NONE)
        .ok()?;
    let line = keys.string("Desktop Entry", "Exec").ok()?;
    let words = glib::shell_parse_argv(line.as_str()).ok()?;
    let words: Vec<String> = words
        .iter()
        .map(|word| word.to_string_lossy().into_owned())
        .filter(|word| !(word.len() == 2 && word.starts_with('%')))
        .collect();
    let terminal = keys.boolean("Desktop Entry", "Terminal").unwrap_or(false);
    (!words.is_empty()).then_some((words, terminal))
}

/// Whether the app whose desktop file is `id` says it runs in a terminal.
fn wants_terminal(id: &str) -> bool {
    let folders = std::iter::once(glib::user_data_dir()).chain(glib::system_data_dirs());
    folders
        .map(|folder| folder.join("applications").join(id))
        .find(|file| file.is_file())
        .is_some_and(|file| {
            let keys = glib::KeyFile::new();
            keys.load_from_file(&file, glib::KeyFileFlags::NONE).is_ok()
                && keys.boolean("Desktop Entry", "Terminal").unwrap_or(false)
        })
}

/// How the file gets edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub command: Vec<OsString>,
    /// What the user knows the editor as.
    pub name: String,
    /// It runs in a terminal, and ends when the marker is gone.
    pub marker: Option<PathBuf>,
}

/// The name of a program, without its folder.
fn base(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

fn in_terminal(words: &[String]) -> bool {
    let name = base(&words[0]);
    if matches!(name, "emacs" | "emacsclient") {
        return words[1..]
            .iter()
            .any(|word| matches!(word.as_str(), "-nw" | "-t" | "--tty" | "--no-window-system"));
    }
    IN_TERMINAL.contains(&name)
}

/// Asks a graphical editor to stay until the file is closed, unless it was.
fn waiting(mut words: Vec<String>) -> Vec<String> {
    let name = base(&words[0]);
    if let Some((_, flag)) = WAIT.iter().find(|(program, _)| *program == name) {
        let asked = words[1..]
            .iter()
            .any(|word| word == flag || matches!(word.as_str(), "-w" | "-f" | "-b" | "-s" | "-i"));
        if !asked {
            words.insert(1, (*flag).to_owned());
        }
    }
    words
}

/// The command for `words` in `terminal`, whose own arguments are in `terminal[1..]`.
fn terminal_command(terminal: &[String], words: Vec<OsString>) -> Vec<OsString> {
    let before: &[&str] = match base(&terminal[0]) {
        "xdg-terminal-exec" | "foot" | "kitty" => &[],
        "gnome-terminal" => &["--wait", "--"],
        "kgx" => &["--"],
        "ptyxis" => &["--new-window", "--"],
        "wezterm" => &["start", "--"],
        "konsole" => &["--nofork", "-e"],
        _ => &["-e"],
    };
    terminal
        .iter()
        .map(OsString::from)
        .chain(before.iter().map(OsString::from))
        .chain(words)
        .collect()
}

/// The words of a setting: `code --wait`, `"emacs" -nw`.
fn words(setting: &str) -> Option<Vec<String>> {
    let words = glib::shell_parse_argv(setting).ok()?;
    let words: Vec<String> = words
        .iter()
        .map(|word| word.to_string_lossy().into_owned())
        .collect();
    (!words.is_empty()).then_some(words)
}

/// How to edit `file` with what `setting` says, or nothing when there is no way.
pub fn plan(setting: &Setting, file: &Path) -> Option<Plan> {
    let chosen = setting
        .default
        .clone()
        .filter(|(words, _)| setting.find(&words[0]).is_some())
        .or_else(|| {
            let words = [&setting.visual, &setting.editor]
                .into_iter()
                .flatten()
                .filter_map(|value| words(value))
                .find(|words| setting.find(&words[0]).is_some())?;
            let terminal = in_terminal(&words);
            Some((words, terminal))
        });
    let Some((words, terminal)) = chosen else {
        // It may not wait, but the composer offers Done for that.
        setting.find("xdg-open")?;
        return Some(Plan {
            command: vec!["xdg-open".into(), file.into()],
            name: "your editor".into(),
            marker: None,
        });
    };
    let name = base(&words[0]).to_owned();
    if !terminal {
        let mut command: Vec<OsString> = waiting(words).into_iter().map(Into::into).collect();
        command.push(file.into());
        return Some(Plan {
            command,
            name,
            marker: None,
        });
    }
    let shell = setting.find("sh")?;
    let marker = marker(file);
    // `sh -c '"$@"; rm -f -- "$0"' marker editor… file`
    let inside: Vec<OsString> = [
        shell.into_os_string(),
        "-c".into(),
        r#""$@"; rm -f -- "$0""#.into(),
        marker.clone().into_os_string(),
    ]
    .into_iter()
    .chain(words.into_iter().map(Into::into))
    .chain([file.as_os_str().to_owned()])
    .collect();
    let terminal = setting
        .terminal
        .as_deref()
        .and_then(self::words)
        .filter(|words| setting.find(&words[0]).is_some())
        .or_else(|| {
            TERMINALS
                .iter()
                .find(|name| setting.find(name).is_some())
                .map(|name| vec![(*name).to_owned()])
        })?;
    Some(Plan {
        command: terminal_command(&terminal, inside),
        name,
        marker: Some(marker),
    })
}

/// Present while the editor in a terminal is.
pub fn marker(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".open");
    PathBuf::from(name)
}

/// Where the files being edited are, readable only by the user.
pub fn folder() -> PathBuf {
    tern_ipc::runtime_folder()
        .unwrap_or_else(|_| glib::user_runtime_dir().join("tern"))
        .join("editing")
}

/// Writes `text` to a new file in `folder` that only the user can read, and the marker for `plan`
/// beside it when there is one.
pub fn write(folder: &Path, text: &str) -> std::io::Result<PathBuf> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(folder)?;
    let now = chrono::Local::now().format("%Y-%m-%d-%H%M%S%.3f");
    for count in 0.. {
        let path = folder.join(format!("message-{now}-{count}.md"));
        let made = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path);
        match made {
            Ok(mut file) => {
                file.write_all(text.as_bytes())?;
                if !text.is_empty() && !text.ends_with('\n') {
                    file.write_all(b"\n")?;
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    unreachable!()
}

/// Makes the marker that the terminal's shell removes.
pub fn mark(marker: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(marker)
        .map(drop)
}

/// The file and its marker are gone.
pub fn forget(file: &Path, marker: Option<&Path>) {
    let _ = std::fs::remove_file(file);
    if let Some(marker) = marker {
        let _ = std::fs::remove_file(marker);
    }
}

/// What was handed to the editor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sent {
    pub text: String,
    /// Everyone mentioned so far: who is mentioned after is whose `@Name` is still in the text.
    pub chosen: Vec<Person>,
}

/// What the conversation has when the editor gives back `edited`, its draft being `now`: nothing
/// when the editor changed nothing or left it empty. The editor's text replaces what was sent to
/// it, or comes after the draft when that was changed meanwhile. Files stay as they are.
pub fn came_back(sent: &Sent, edited: &str, now: &Written) -> Option<Written> {
    let edited = edited.trim();
    if edited.is_empty() || edited == sent.text.trim() {
        return None;
    }
    let mut chosen = sent.chosen.clone();
    chosen.extend(now.mentions.iter().cloned());
    let back = Written {
        mentions: mention::placed(edited, &chosen),
        text: edited.to_owned(),
        files: Vec::new(),
    };
    if now.text.trim() == sent.text.trim() {
        return Some(Written {
            files: now.files.clone(),
            ..back
        });
    }
    Some(now.clone().before(back))
}

/// Puts what the editor gave back into the draft of `id`, for a conversation not in view.
pub fn into_drafts(drafts: &mut HashMap<String, Written>, id: &str, sent: &Sent, edited: &str) {
    let now = drafts.get(id).cloned().unwrap_or_default();
    if let Some(written) = came_back(sent, edited, &now) {
        drafts.insert(id.to_owned(), written);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A folder with these programs in it.
    fn programs(names: &[&str]) -> tempfile::TempDir {
        let folder = tempfile::tempdir().unwrap();
        for name in names {
            let path = folder.path().join(name);
            std::fs::write(&path, b"#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        folder
    }

    fn in_folder(folder: &tempfile::TempDir) -> Setting {
        Setting {
            path: vec![folder.path().to_owned()],
            ..Default::default()
        }
    }

    fn said(plan: &Plan) -> Vec<String> {
        plan.command
            .iter()
            .map(|word| {
                let word = word.to_string_lossy();
                base(&word).to_owned()
            })
            .collect()
    }

    const FILE: &str = "/run/x/message.md";

    #[test]
    fn a_graphical_editor_is_asked_to_wait() {
        let folder = programs(&["code", "gedit", "sh", "foot"]);
        let mut setting = in_folder(&folder);
        setting.visual = Some("code".into());
        let plan = plan(&setting, Path::new(FILE)).unwrap();
        assert_eq!(said(&plan), ["code", "--wait", "message.md"]);
        assert_eq!((plan.name.as_str(), plan.marker), ("code", None));
        // Not twice, and the setting's own words stay.
        setting.visual = Some("code -n --wait".into());
        let plan = super::plan(&setting, Path::new(FILE)).unwrap();
        assert_eq!(said(&plan), ["code", "-n", "--wait", "message.md"]);
        setting.visual = Some("gedit".into());
        let plan = super::plan(&setting, Path::new(FILE)).unwrap();
        assert_eq!(said(&plan), ["gedit", "--wait", "message.md"]);
    }

    #[test]
    fn the_default_then_visual_then_editor_then_xdg_open() {
        let folder = programs(&["nano", "gedit", "kate", "nvim", "xdg-open", "sh", "xterm"]);
        let mut setting = in_folder(&folder);
        // The desktop's default comes before the shell's settings.
        setting.visual = Some("nvim".into());
        setting.editor = Some("kate".into());
        setting.default = Some((vec!["gedit".into()], false));
        let plan = plan(&setting, Path::new(FILE)).unwrap();
        assert_eq!(said(&plan), ["gedit", "--wait", "message.md"]);
        // A default that wants a terminal gets one.
        setting.default = Some((vec!["nano".into()], true));
        let plan = super::plan(&setting, Path::new(FILE)).unwrap();
        assert_eq!(said(&plan)[..2], ["xterm", "-e"]);
        assert_eq!(plan.name, "nano");
        // A default that is not there gives way to `$VISUAL`.
        setting.default = Some((vec!["gone".into()], false));
        let plan = super::plan(&setting, Path::new(FILE)).unwrap();
        assert_eq!(plan.name, "nvim");
        // `$VISUAL` that is not there gives way to `$EDITOR`.
        setting.default = None;
        setting.visual = Some("not-installed".into());
        let plan = super::plan(&setting, Path::new(FILE)).unwrap();
        assert_eq!(said(&plan), ["kate", "--block", "message.md"]);
        setting.editor = None;
        let plan = super::plan(&setting, Path::new(FILE)).unwrap();
        assert_eq!(said(&plan), ["xdg-open", "message.md"]);
        assert_eq!(plan.name, "your editor");
        let nothing = programs(&[]);
        assert_eq!(super::plan(&in_folder(&nothing), Path::new(FILE)), None);
    }

    #[test]
    fn a_terminal_editor_runs_in_a_terminal_until_it_ends() {
        let folder = programs(&["nvim", "sh", "alacritty", "foot"]);
        let mut setting = in_folder(&folder);
        setting.editor = Some("nvim".into());
        let plan = plan(&setting, Path::new(FILE)).unwrap();
        // The first of the list there is: foot before alacritty.
        let marker = format!("{FILE}.open");
        assert_eq!(plan.marker.as_deref(), Some(Path::new(&marker)));
        let command: Vec<String> = plan
            .command
            .iter()
            .map(|word| word.to_string_lossy().into_owned())
            .collect();
        assert_eq!(command[0], "foot");
        assert!(command[1].ends_with("/sh"));
        assert_eq!(command[2..4], ["-c", r#""$@"; rm -f -- "$0""#]);
        assert_eq!(command[4], marker);
        assert!(command[5].ends_with("nvim"));
        assert_eq!(command[6], FILE);
        assert_eq!(plan.name, "nvim");
        // `$TERMINAL` comes first, and a terminal it does not know gets `-e`.
        let more = programs(&["my-term"]);
        setting.path.push(more.path().to_owned());
        setting.terminal = Some("my-term --class editing".into());
        let plan = super::plan(&setting, Path::new(FILE)).unwrap();
        assert_eq!(said(&plan)[..4], ["my-term", "--class", "editing", "-e"]);
        // One it knows gets its own way.
        setting.terminal = Some("alacritty".into());
        let plan = super::plan(&setting, Path::new(FILE)).unwrap();
        assert_eq!(said(&plan)[..3], ["alacritty", "-e", "sh"]);
        // `$TERMINAL` that is not there gives way to the list.
        setting.terminal = Some("gone".into());
        let plan = super::plan(&setting, Path::new(FILE)).unwrap();
        assert_eq!(said(&plan)[..2], ["foot", "sh"]);
        // No terminal: no way to edit.
        let lonely = programs(&["nvim", "sh"]);
        let mut setting = in_folder(&lonely);
        setting.editor = Some("nvim".into());
        assert_eq!(super::plan(&setting, Path::new(FILE)), None);
    }

    #[test]
    fn each_terminal_is_told_its_own_way() {
        let inside = vec![OsString::from("sh"), OsString::from("x")];
        let told = |terminal: &str| -> Vec<String> {
            terminal_command(&[terminal.to_owned()], inside.clone())
                .into_iter()
                .map(|word| word.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(told("xdg-terminal-exec"), ["xdg-terminal-exec", "sh", "x"]);
        assert_eq!(told("kgx"), ["kgx", "--", "sh", "x"]);
        assert_eq!(
            told("/usr/bin/gnome-terminal"),
            ["/usr/bin/gnome-terminal", "--wait", "--", "sh", "x"]
        );
        assert_eq!(told("ptyxis"), ["ptyxis", "--new-window", "--", "sh", "x"]);
        assert_eq!(told("foot"), ["foot", "sh", "x"]);
        assert_eq!(told("kitty"), ["kitty", "sh", "x"]);
        assert_eq!(told("alacritty"), ["alacritty", "-e", "sh", "x"]);
        assert_eq!(told("wezterm"), ["wezterm", "start", "--", "sh", "x"]);
        assert_eq!(told("konsole"), ["konsole", "--nofork", "-e", "sh", "x"]);
        assert_eq!(told("xterm"), ["xterm", "-e", "sh", "x"]);
    }

    #[test]
    fn emacs_wants_a_terminal_only_when_told() {
        let words = |line: &str| words(line).unwrap();
        assert!(in_terminal(&words("emacs -nw")));
        assert!(in_terminal(&words("emacsclient -t")));
        assert!(!in_terminal(&words("emacs")));
        assert!(!in_terminal(&words("emacsclient -c")));
        assert!(in_terminal(&words("/usr/bin/vi")));
        assert!(in_terminal(&words("hx")));
        assert!(!in_terminal(&words("gnome-text-editor")));
        assert_eq!(words("'my editor' -x"), ["my editor", "-x"]);
    }

    #[test]
    fn the_file_is_private_and_the_marker_goes_with_it() {
        let folder = tempfile::tempdir().unwrap();
        let place = folder.path().join("editing");
        let file = write(&place, "hello").unwrap();
        let other = write(&place, "").unwrap();
        assert_ne!(file, other);
        let mode = |path: &Path| path.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&place), 0o700);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello\n");
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "");
        let marker = marker(&file);
        mark(&marker).unwrap();
        assert_eq!(mode(&marker), 0o600);
        forget(&file, Some(&marker));
        assert!(!file.exists() && !marker.exists());
    }

    fn ann() -> Person {
        ("a".into(), "Ann Example".into())
    }

    #[test]
    fn what_comes_back_replaces_what_went() {
        let sent = Sent {
            text: "hi @Ann Example".into(),
            chosen: vec![ann()],
        };
        let now = Written {
            text: "hi @Ann Example".into(),
            mentions: vec![ann()],
            files: vec![PathBuf::from("/x/plan.pdf")],
        };
        // Unchanged, or emptied: as it was.
        assert_eq!(came_back(&sent, "hi @Ann Example\n", &now), None);
        assert_eq!(came_back(&sent, "  \n\n", &now), None);
        // The mention stays where its name is, and the file stays.
        let back = came_back(&sent, "hi @Ann Example, the plan:\n\n- one\n", &now).unwrap();
        assert_eq!(back.text, "hi @Ann Example, the plan:\n\n- one");
        assert_eq!(back.mentions, [ann()]);
        assert_eq!(back.files, now.files);
        // A name changed by hand mentions nobody.
        let back = came_back(&sent, "hi @Anne", &now).unwrap();
        assert!(back.mentions.is_empty());
        // A draft written meanwhile stays first.
        let meanwhile = Written {
            text: "meanwhile".into(),
            ..Default::default()
        };
        let back = came_back(&sent, "from the editor", &meanwhile).unwrap();
        assert_eq!(back.text, "meanwhile\nfrom the editor");
    }

    #[test]
    fn a_chat_left_meanwhile_gets_it_as_its_draft() {
        let mut drafts = HashMap::from([(
            "c1".to_owned(),
            Written {
                text: "short".into(),
                files: vec![PathBuf::from("/x/a.png")],
                ..Default::default()
            },
        )]);
        let sent = Sent {
            text: "short".into(),
            chosen: Vec::new(),
        };
        into_drafts(&mut drafts, "c1", &sent, "short, and then much longer\n");
        assert_eq!(drafts["c1"].text, "short, and then much longer");
        assert_eq!(drafts["c1"].files, [PathBuf::from("/x/a.png")]);
        // A chat that had no draft gets one; nothing new leaves it be.
        let empty = Sent::default();
        into_drafts(&mut drafts, "c2", &empty, "");
        assert!(!drafts.contains_key("c2"));
        into_drafts(&mut drafts, "c2", &empty, "from nothing");
        assert_eq!(drafts["c2"].text, "from nothing");
        // A thread reply has a draft of its own.
        into_drafts(&mut drafts, "c1/p9", &empty, "in the thread");
        assert_eq!(drafts["c1/p9"].text, "in the thread");
        assert_eq!(drafts["c1"].text, "short, and then much longer");
    }

    #[test]
    fn the_computer_says_its_setting() {
        let desktop = "[Desktop Entry]\nName=Text\nExec=gnome-text-editor %U\nTerminal=false\n";
        let said = format!("code --wait\0\0foot\0/usr/bin:/bin\0text.desktop\0{desktop}");
        let setting = from_host(said.as_bytes());
        assert_eq!(setting.visual.as_deref(), Some("code --wait"));
        assert_eq!(setting.editor, None);
        assert_eq!(setting.terminal.as_deref(), Some("foot"));
        assert_eq!(
            setting.path,
            [PathBuf::from("/usr/bin"), PathBuf::from("/bin")]
        );
        assert_eq!(
            setting.default,
            Some((vec!["gnome-text-editor".into()], false))
        );
        let wanted = setting.wanted();
        assert_eq!(wanted[..3], ["gnome-text-editor", "code", "foot"]);
        assert!(wanted.iter().any(|name| name == "sh"));
        let vim = "[Desktop Entry]\nExec=vim %F\nTerminal=true\n";
        let said = format!("\0\0\0\0vim.desktop\0{vim}");
        assert_eq!(
            from_host(said.as_bytes()).default,
            Some((vec!["vim".into()], true))
        );
        assert_eq!(from_host(b"\0\0\0\0").default, None);
    }

    #[test]
    fn programs_are_where_the_computer_found_them() {
        let folder = programs(&["nvim", "foot", "sh"]);
        let bin = folder.path();
        let names = ["nvim", "code", "foot", "sh"].map(String::from);
        let given = bin.join("foot");
        let out = std::process::Command::new("/bin/sh")
            .args(["-c", FIND, "sh"])
            .args(&names)
            .arg(&given)
            .arg(bin.join("missing"))
            .env("PATH", bin)
            .output()
            .unwrap();
        let found = found(&out.stdout);
        assert_eq!(found.get("nvim"), Some(&bin.join("nvim")));
        assert_eq!(found.get("code"), None);
        assert_eq!(found.get(given.to_str().unwrap()), Some(&given));
        let setting = Setting {
            editor: Some("nvim".into()),
            found: Some(found),
            ..Default::default()
        };
        let plan = plan(&setting, Path::new(FILE)).unwrap();
        assert_eq!(said(&plan)[0], "foot");
        assert!(said(&plan).iter().any(|word| word == "nvim"));
    }

    /// Inside the Flatpak: the computer's setting, and a file both see.
    #[test]
    #[ignore = "needs the Flatpak"]
    fn from_the_flatpak_the_editor_is_the_computers() {
        assert!(host::sandboxed());
        let setting = Setting::on_host().expect("the computer's setting");
        assert!(setting.find("sh").is_some_and(|sh| sh.is_absolute()));
        let file = write(&folder(), "from the sandbox").unwrap();
        let plan = plan(&setting, &file).expect("an editor");
        eprintln!("would edit with {}: {:?}", plan.name, plan.command);
        let appended = host::script(r#"printf ' and back' >>"$1""#, &[file.as_os_str()]);
        assert!(appended.is_some());
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text, "from the sandbox\n and back");
        forget(&file, None);
    }
}
