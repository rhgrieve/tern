//! The service's log, which users read and send on: plain lines, and no web address beyond its
//! host, since paths hold conversation ids and queries can hold more.

use std::borrow::Cow;
use std::io::{IsTerminal, Write};

const SCHEMES: [&str; 2] = ["https://", "http://"];

type Log = Box<dyn tracing::Subscriber + Send + Sync>;

/// Starts the log on standard error, which systemd keeps in the journal.
pub fn start() {
    // Dependencies stay at warnings whatever is asked: their debug output can hold request headers.
    let level = match std::env::var("TERN_LOG").as_deref() {
        Ok("debug") => "warn,ternd=debug,tern_auth=debug,tern_presence=debug,tern_engine=debug",
        _ => "warn,ternd=info,tern_auth=info,tern_presence=info,tern_engine=info",
    };
    let journal = std::env::var_os("JOURNAL_STREAM").is_some();
    let colour = !journal && std::io::stderr().is_terminal();
    let log = to(std::io::stderr, level, journal, colour);
    let _ = tracing::subscriber::set_global_default(log);
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
