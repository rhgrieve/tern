//! The service's log, which users read and send on: plain lines, and no web address beyond its
//! host, since paths hold conversation ids and queries can hold more. systemd keeps it for the
//! installed service; the Flatpak's keeps it itself, in a file a day for the last two.

use std::borrow::Cow;
use std::fs::File;
use std::io::{IsTerminal, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{Days, Local, NaiveDate};

const SCHEMES: [&str; 2] = ["https://", "http://"];

type Log = Box<dyn tracing::Subscriber + Send + Sync>;

/// Days kept before today's.
const KEPT: u64 = 2;

/// Starts the log on standard error, which systemd keeps in the journal, and in the Flatpak in
/// files as well.
pub fn start() {
    // Dependencies stay at warnings whatever is asked: their debug output can hold request headers.
    let level = match std::env::var("TERN_LOG").as_deref() {
        Ok("debug") => "warn,ternd=debug,tern_auth=debug,tern_presence=debug,tern_engine=debug",
        _ => "warn,ternd=info,tern_auth=info,tern_presence=info,tern_engine=info",
    };
    let journal = std::env::var_os("JOURNAL_STREAM").is_some();
    let files = tern_ipc::log_folder()
        .filter(|_| tern_ipc::host::sandboxed())
        .map(|folder| Arc::new(Files::new(folder)));
    // The files need the time on each line, whoever else reads standard error.
    let journal = journal && files.is_none();
    let colour = !journal && files.is_none() && std::io::stderr().is_terminal();
    let log = match files {
        Some(files) => to(
            move || Both(std::io::stderr(), files.clone()),
            level,
            journal,
            colour,
        ),
        None => to(std::io::stderr, level, journal, colour),
    };
    let _ = tracing::subscriber::set_global_default(log);
}

/// Today's file, opened on the first line of each day; then the oldest go.
struct Files {
    folder: PathBuf,
    open: Mutex<Option<(NaiveDate, File)>>,
}

impl Files {
    fn new(folder: PathBuf) -> Files {
        Files {
            folder,
            open: Mutex::new(None),
        }
    }

    fn write(&self, bytes: &[u8], today: NaiveDate) -> std::io::Result<()> {
        let mut open = self
            .open
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if open.as_ref().is_none_or(|(day, _)| *day != today) {
            *open = None;
            let file = day_file(&self.folder, today)?;
            forget_before(&self.folder, today.checked_sub_days(Days::new(KEPT)));
            *open = Some((today, file));
        }
        let (_, file) = open.as_mut().expect("opened");
        file.write_all(bytes)
    }
}

fn day_file(folder: &Path, day: NaiveDate) -> std::io::Result<File> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(folder)?;
    let name = format!("{}{}.log", tern_ipc::LOG_NAME, day.format("%Y-%m-%d"));
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(folder.join(name))
}

/// Removes the days' files from before `first`.
fn forget_before(folder: &Path, first: Option<NaiveDate>) {
    let (Some(first), Ok(entries)) = (first, std::fs::read_dir(folder)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let day = name
            .to_str()
            .and_then(|name| name.strip_prefix(tern_ipc::LOG_NAME))
            .and_then(|name| name.strip_suffix(".log"))
            .and_then(|day| NaiveDate::parse_from_str(day, "%Y-%m-%d").ok());
        if day.is_some_and(|day| day < first) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Standard error, and the file. A file that cannot be written does not stop the log.
struct Both(std::io::Stderr, Arc<Files>);

impl Write for Both {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.write_all(bytes);
        let _ = self.1.write(bytes, Local::now().date_naive());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// Lines to what `open` gives. The journal has its own time, and shows colour codes as they are.
fn to<W: Write + 'static>(
    open: impl Fn() -> W + Send + Sync + 'static,
    level: &str,
    journal: bool,
    colour: bool,
) -> Log {
    let log = tracing_subscriber::fmt()
        .with_env_filter(level)
        .with_writer(move || Scrubbed(open()))
        .with_target(false)
        .with_ansi(colour);
    if journal {
        Box::new(log.without_time().finish())
    } else {
        Box::new(log.finish())
    }
}

/// A writer, with web addresses cut to their host.
struct Scrubbed<W>(W);

impl<W: Write> Write for Scrubbed<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(bytes);
        self.0.write_all(scrub(&text).as_bytes())?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// `text` with every web address cut to its scheme and host, `…` where something was cut.
fn scrub(text: &str) -> Cow<'_, str> {
    if !SCHEMES.iter().any(|scheme| text.contains(scheme)) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let found = SCHEMES
            .iter()
            .filter_map(|scheme| rest.find(scheme).map(|at| (at, scheme.len())))
            .min();
        let Some((at, scheme)) = found else {
            out.push_str(rest);
            return Cow::Owned(out);
        };
        let address = &rest[at..];
        let end = address
            .find(|letter: char| {
                letter.is_whitespace() || matches!(letter, '"' | '\'' | ')' | '<' | '>' | '`')
            })
            .unwrap_or(address.len());
        let host_end = address[scheme..end]
            .find(['/', '?', '#'])
            .map_or(end, |cut| scheme + cut);
        out.push_str(&rest[..at]);
        out.push_str(&address[..host_end]);
        if host_end < end {
            out.push_str("/…");
        }
        rest = &address[end..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Kept(Arc<Mutex<Vec<u8>>>);

    impl Write for Kept {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn journal_lines_are_plain() {
        let kept = Kept::default();
        let log = to(
            {
                let kept = kept.clone();
                move || kept.clone()
            },
            "info",
            true,
            false,
        );
        tracing::subscriber::with_default(log, || {
            tracing::warn!(
                reason = "error sending request for url (https://a.example/19:secret/messages)",
                "could not connect to Teams"
            );
        });
        let line = String::from_utf8(kept.0.lock().unwrap().clone()).unwrap();
        assert_eq!(
            line,
            " WARN could not connect to Teams \
             reason=\"error sending request for url (https://a.example/…)\"\n"
        );
    }

    #[test]
    fn a_file_a_day_for_the_user_alone_and_only_the_last_days() {
        use std::os::unix::fs::PermissionsExt;
        let folder = tempfile::tempdir().unwrap();
        let logs = folder.path().join("log");
        let files = Files::new(logs.clone());
        let day = |text| NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap();
        for (text, line) in [
            ("2026-11-20", "a\n"),
            ("2026-11-25", "b\n"),
            ("2026-11-25", "c\n"),
        ] {
            files.write(line.as_bytes(), day(text)).unwrap();
        }
        std::fs::write(logs.join("other.txt"), "").unwrap();
        files.write(b"d\n", day("2026-11-27")).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&logs)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["other.txt", "ternd-2026-11-25.log", "ternd-2026-11-27.log"]
        );
        let read = |name: &str| std::fs::read_to_string(logs.join(name)).unwrap();
        assert_eq!(read("ternd-2026-11-25.log"), "b\nc\n");
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&logs.join("ternd-2026-11-27.log")), 0o600);
        assert_eq!(mode(&logs), 0o700);
    }

    #[test]
    fn addresses_keep_only_their_host() {
        assert_eq!(
            scrub(
                "error sending request for url (https://emea.ng.msg.teams.microsoft.com/v1/users/ME/conversations/19:secret@thread.v2/messages?x=1)"
            ),
            "error sending request for url (https://emea.ng.msg.teams.microsoft.com/…)"
        );
        assert_eq!(
            scrub("a http://host.example b https://other.example/path c"),
            "a http://host.example b https://other.example/… c"
        );
        assert_eq!(
            scrub(r#"reason="see https://x.example/a?token=t" done"#),
            r#"reason="see https://x.example/…" done"#
        );
        assert_eq!(scrub("https://x.example?token=t"), "https://x.example/…");
    }

    #[test]
    fn lines_without_addresses_are_left_be() {
        let line = "could not connect to Teams reason=\"HTTP 503\"\n";
        assert!(matches!(scrub(line), std::borrow::Cow::Borrowed(same) if same == line));
        assert_eq!(scrub("é 🎉 https:// end"), "é 🎉 https:// end");
    }
}
