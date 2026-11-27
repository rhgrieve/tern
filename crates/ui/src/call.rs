//! A call, run by `tern-call` in a process of its own. The window tells it `mute`, `unmute` and
//! `hangup`, and it tells the window how the call goes, a line at a time. If the window goes away
//! its end of the pipe closes, and the call hangs up by itself.
//!
//! A call that rang is answered or declined with the push that told of it, given to `tern-call` as
//! the first line.

use std::cell::{Cell, RefCell};
use std::ffi::{OsStr, OsString};
use std::rc::Rc;
use std::time::Instant;

use gtk4::gio::prelude::*;
use gtk4::prelude::*;
use gtk4::{self as gtk, gio, glib};

use crate::model::Callable;
use crate::row::say;

/// Where the call is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// A call that rang, being answered.
    Answering,
    /// A meeting, being joined.
    Joining,
    Calling,
    Ringing,
    Connected(Instant),
    Reconnecting,
}

/// What a line of `tern-call` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Stage(Stage),
    Muted(bool),
    /// Said once, at the end, in words for the user.
    Ended(String),
    Notice(String),
    /// Who is calling, once a call that rang is reached.
    Caller(String),
    /// In a meeting's lobby, until somebody lets one in.
    Waiting(bool),
    /// Who is in the call, by name.
    People(Vec<String>),
}

pub fn change(line: &str, now: Instant) -> Option<Change> {
    let line = line.trim();
    let (word, rest) = line.split_once(": ").unwrap_or((line, ""));
    Some(match word {
        "calling" | "connecting" => Change::Stage(Stage::Calling),
        "ringing" => Change::Stage(Stage::Ringing),
        "connected" => Change::Stage(Stage::Connected(now)),
        "reconnecting" => Change::Stage(Stage::Reconnecting),
        "muted" => Change::Muted(true),
        "unmuted" => Change::Muted(false),
        "ended" => Change::Ended(match rest {
            "no answer" => "No answer".into(),
            "nobody let you in" => "Nobody let you in".into(),
            "declined" => "Declined".into(),
            "the call was over before it was answered" => "The call was over".into(),
            "hung up" => "Call ended".into(),
            rest if rest.starts_with("by the other side") => "Call ended".into(),
            "the connection dropped" => "The call dropped".into(),
            rest => match rest.strip_prefix("failed: ") {
                Some(why) => format!("The call failed: {why}"),
                None => "Call ended".into(),
            },
        }),
        "failed" => Change::Ended(format!("The call failed: {rest}")),
        "no microphone" => Change::Notice("No microphone was found: you can only listen".into()),
        "caller" if !rest.is_empty() => Change::Caller(rest.to_owned()),
        "waiting" => Change::Waiting(true),
        "admitted" => Change::Waiting(false),
        "people" => Change::People(
            rest.split("; ")
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect(),
        ),
        _ => return None,
    })
}

/// `12:05`, or `1:02:05` past an hour.
pub fn duration(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Bounds for the call process: it handles what the other side sends, so it gets no more. Its
/// description is what the journal says of it: without one, the whole command line, with who is
/// called.
const SCOPE: [&str; 10] = [
    "systemd-run",
    "--user",
    "--scope",
    "--quiet",
    "--description=Tern call",
    "-p",
    "MemoryMax=2G",
    "-p",
    "MemorySwapMax=0",
    "--",
];

/// Starts `tern-call` with `arguments`, its output read and its input written by the window.
fn spawn(arguments: &[&str]) -> Result<(gio::OutputStream, gio::Subprocess), glib::Error> {
    let call: Vec<OsString> = std::iter::once(tern_ipc::program("tern-call").into_os_string())
        .chain(arguments.iter().map(OsString::from))
        .collect();
    let flags = gio::SubprocessFlags::STDIN_PIPE
        | gio::SubprocessFlags::STDOUT_PIPE
        | gio::SubprocessFlags::STDERR_PIPE;
    let scoped: Vec<OsString> = SCOPE
        .iter()
        .map(OsString::from)
        .chain(call.clone())
        .collect();
    let run = |all: &[OsString]| {
        let all: Vec<&OsStr> = all.iter().map(OsString::as_os_str).collect();
        gio::Subprocess::newv(&all, flags)
    };
    // Without systemd, as it is.
    let process = run(&scoped).or_else(|_| run(&call))?;
    match process.stdin_pipe() {
        Some(input) => Ok((input, process)),
        None => {
            process.force_exit();
            Err(glib::Error::new(gio::IOErrorEnum::Failed, "no pipes"))
        }
    }
}

fn process_errors(call: &Call) -> Option<gio::InputStream> {
    call._process.stderr_pipe()
}

/// What a bar shows of a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub name: String,
    pub stage: Stage,
    pub muted: bool,
    /// In a meeting's lobby.
    pub waiting: bool,
    /// Who is in the call, oneself too, when that is known.
    pub people: Vec<String>,
}

pub struct Call {
    name: RefCell<String>,
    waiting: Cell<bool>,
    people: RefCell<Vec<String>>,
    stage: Cell<Stage>,
    muted: Cell<bool>,
    ended: Cell<bool>,
    input: gio::OutputStream,
    /// Hears how the call goes, for the log.
    said: RefCell<Option<Said>>,
    /// Kept so the process is not let go of.
    _process: gio::Subprocess,
}

/// What hears a call's lines for the log.
type Said = Box<dyn Fn(&str)>;

/// Whether a line of `tern-call` is for the log: not the ones that name people.
fn loggable(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty() && !line.starts_with("caller:") && !line.starts_with("people:")
}

impl Call {
    /// Calls `who`; `changed` hears every change, the last one `Change::Ended`.
    pub fn place(
        who: &Callable,
        changed: impl Fn(&Rc<Call>, Change) + 'static,
    ) -> Result<Rc<Call>, glib::Error> {
        let mut arguments: Vec<String> = match who.people.as_slice() {
            [(mri, _)] if !who.is_group() => [
                "person",
                mri.as_str(),
                who.thread.as_str(),
                "--name",
                who.name.as_str(),
            ]
            .map(str::to_owned)
            .to_vec(),
            people => {
                let mut arguments = vec!["group".to_owned(), who.thread.clone()];
                for (mri, name) in people {
                    arguments.push("--member".into());
                    arguments.push(format!("{mri}={name}"));
                }
                arguments
            }
        };
        arguments.push("--pipe".into());
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
        Call::start(&arguments, None, &who.name, Stage::Calling, changed)
    }

    /// Joins the meeting of `link`, with sound only, muted; `name` is what the bar calls it.
    pub fn meeting(
        link: &str,
        name: &str,
        changed: impl Fn(&Rc<Call>, Change) + 'static,
    ) -> Result<Rc<Call>, glib::Error> {
        Call::start(
            &["meeting", link, "--pipe"],
            None,
            name,
            Stage::Joining,
            changed,
        )
    }

    /// Answers the call that `push` told of.
    pub fn answer(
        push: &str,
        changed: impl Fn(&Rc<Call>, Change) + 'static,
    ) -> Result<Rc<Call>, glib::Error> {
        Call::start(
            &["answer", "--pipe"],
            Some(push),
            "Someone",
            Stage::Answering,
            changed,
        )
    }

    /// Declines the call that `push` told of, for all the user's devices, and lets it be.
    pub fn decline(push: &str) -> Result<(), glib::Error> {
        let (input, _) = spawn(&["decline"])?;
        let push = format!("{push}\n").into_bytes();
        glib::spawn_future_local(async move {
            let _ = input.write_all_future(push, glib::Priority::DEFAULT).await;
            let _ = input.close_future(glib::Priority::DEFAULT).await;
        });
        Ok(())
    }

    fn start(
        arguments: &[&str],
        first: Option<&str>,
        name: &str,
        stage: Stage,
        changed: impl Fn(&Rc<Call>, Change) + 'static,
    ) -> Result<Rc<Call>, glib::Error> {
        let (input, process) = spawn(arguments)?;
        let Some(output) = process.stdout_pipe() else {
            process.force_exit();
            return Err(glib::Error::new(gio::IOErrorEnum::Failed, "no pipes"));
        };
        let call = Rc::new(Call {
            name: RefCell::new(name.to_owned()),
            waiting: Cell::new(false),
            people: RefCell::default(),
            stage: Cell::new(stage),
            muted: Cell::new(false),
            ended: Cell::new(false),
            input,
            said: RefCell::default(),
            _process: process,
        });
        // What goes wrong in it, said to nobody else.
        if let Some(errors) = process_errors(&call) {
            let errors = gio::DataInputStream::new(&errors);
            let weak = Rc::downgrade(&call);
            glib::spawn_future_local(async move {
                while let Ok(Some(line)) =
                    errors.read_line_utf8_future(glib::Priority::DEFAULT).await
                {
                    let Some(call) = weak.upgrade() else { return };
                    call.say(&line);
                }
            });
        }
        if let Some(first) = first {
            call.tell(first);
        }
        let lines = gio::DataInputStream::new(&output);
        let weak = Rc::downgrade(&call);
        glib::spawn_future_local(async move {
            loop {
                let line = lines.read_line_utf8_future(glib::Priority::DEFAULT).await;
                let Some(call) = weak.upgrade() else { return };
                if let Ok(Some(line)) = &line
                    && loggable(line)
                {
                    call.say(line);
                }
                let change = match line {
                    Ok(Some(line)) => match self::change(&line, Instant::now()) {
                        Some(change) => change,
                        None => continue,
                    },
                    // The process is gone without saying how.
                    _ => {
                        if !call.ended.get() {
                            call.say("the call program ended without saying how");
                        }
                        Change::Ended("Call ended".into())
                    }
                };
                if call.ended.get() {
                    return;
                }
                match &change {
                    Change::Stage(stage) => call.stage.set(*stage),
                    Change::Muted(muted) => call.muted.set(*muted),
                    Change::Ended(_) => call.ended.set(true),
                    Change::Caller(name) => {
                        call.name.replace(name.clone());
                    }
                    Change::Waiting(waiting) => call.waiting.set(*waiting),
                    Change::People(people) => {
                        call.people.replace(people.clone());
                    }
                    Change::Notice(_) => {}
                }
                let last = matches!(change, Change::Ended(_));
                changed(&call, change);
                if last {
                    return;
                }
            }
        });
        Ok(call)
    }
    pub fn shown(&self) -> Shown {
        Shown {
            name: self.name.borrow().clone(),
            stage: self.stage.get(),
            muted: self.muted.get(),
            waiting: self.waiting.get(),
            people: self.people.borrow().clone(),
        }
    }

    /// `said` hears how the call goes, a line at a time, for the log.
    pub fn on_said(&self, said: impl Fn(&str) + 'static) {
        self.said.replace(Some(Box::new(said)));
    }

    fn say(&self, line: &str) {
        if let Some(said) = self.said.borrow().as_ref() {
            said(line.trim());
        }
    }

    pub fn stage(&self) -> Stage {
        self.stage.get()
    }

    pub fn muted(&self) -> bool {
        self.muted.get()
    }

    fn tell(&self, line: &str) {
        let input = self.input.clone();
        let line = format!("{line}\n").into_bytes();
        glib::spawn_future_local(async move {
            let _ = input.write_all_future(line, glib::Priority::DEFAULT).await;
        });
    }

    pub fn set_muted(&self, muted: bool) {
        self.tell(if muted { "mute" } else { "unmute" });
    }

    pub fn hang_up(&self) {
        self.tell("hangup");
    }
}

/// The bar under a window's header while a call is on.
pub struct Bar {
    pub root: gtk::Revealer,
    what: gtk::Label,
    mute: gtk::ToggleButton,
    /// Set while the button is moved to match the call, so that is not taken for a click.
    quiet: Rc<Cell<bool>>,
    call: RefCell<Option<Rc<Call>>>,
}

impl Bar {
    pub fn new() -> Rc<Bar> {
        let what = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .build();
        let mute = gtk::ToggleButton::builder()
            .icon_name("audio-input-microphone-symbolic")
            .tooltip_text("Mute")
            .valign(gtk::Align::Center)
            .build();
        say(&mute, "Mute");
        let end = gtk::Button::builder()
            .icon_name("call-stop-symbolic")
            .tooltip_text("Hang up")
            .valign(gtk::Align::Center)
            .css_classes(["destructive-action", "circular"])
            .build();
        say(&end, "Hang up");
        let row = gtk::Box::builder()
            .spacing(8)
            .css_classes(["call-bar"])
            .build();
        row.append(&gtk::Image::from_icon_name("call-start-symbolic"));
        row.append(&what);
        row.append(&mute);
        row.append(&end);
        let root = gtk::Revealer::builder()
            .child(&row)
            .reveal_child(false)
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .build();
        let bar = Rc::new(Bar {
            root,
            what,
            mute,
            quiet: Rc::default(),
            call: RefCell::default(),
        });
        let weak = Rc::downgrade(&bar);
        bar.mute.connect_toggled(move |button| {
            let Some(bar) = weak.upgrade() else { return };
            if bar.quiet.get() {
                return;
            }
            if let Some(call) = bar.call.borrow().as_ref() {
                call.set_muted(button.is_active());
            }
        });
        let weak = Rc::downgrade(&bar);
        end.connect_clicked(move |_| {
            if let Some(call) = weak.upgrade().and_then(|bar| bar.call.borrow().clone()) {
                call.hang_up();
            }
        });
        bar
    }

    /// Shows the call as it is now, or nothing.
    pub fn show(&self, call: Option<&Rc<Call>>) {
        self.call.replace(call.cloned());
        self.draw(call.map(|call| call.shown()));
    }

    /// Shows a call, whether there is one or not: for trying things out.
    pub fn draw(&self, shown: Option<Shown>) {
        let Some(shown) = shown else {
            self.root.set_reveal_child(false);
            return;
        };
        let name = &shown.name;
        // Two are a call with somebody; more are worth counting.
        let count = match shown.people.len() {
            0..=2 => String::new(),
            many => format!(" · {many} people"),
        };
        self.what.set_text(&match shown.stage {
            _ if shown.waiting => format!("{name} · waiting to be let in…"),
            Stage::Answering => format!("Answering {name}…"),
            Stage::Joining => format!("Joining {name}…"),
            Stage::Calling => format!("Calling {name}…"),
            Stage::Ringing => format!("Ringing {name}…"),
            Stage::Reconnecting => format!("{name} · reconnecting…"),
            Stage::Connected(since) => {
                format!("{name}{count} · {}", duration(since.elapsed().as_secs()))
            }
        });
        let tip = shown.people.join(", ");
        self.what
            .set_tooltip_text((!tip.is_empty()).then_some(tip.as_str()));
        self.quiet.set(true);
        self.mute.set_active(shown.muted);
        self.quiet.set(false);
        let (icon, tip) = if shown.muted {
            ("microphone-disabled-symbolic", "Unmute")
        } else {
            ("audio-input-microphone-symbolic", "Mute")
        };
        self.mute.set_icon_name(icon);
        self.mute.set_tooltip_text(Some(tip));
        say(&self.mute, tip);
        self.root.set_reveal_child(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_hears_how_a_call_goes_but_not_who_is_in_it() {
        for line in [
            "ringing",
            "connected",
            "ended: no answer",
            "failed: HTTP 403",
            "refused: GET x",
        ] {
            assert!(loggable(line), "{line}");
        }
        for line in ["caller: Ann Example", "people: Ann; Bo", "  ", ""] {
            assert!(!loggable(line), "{line:?}");
        }
    }

    #[test]
    fn the_lines_of_tern_call_are_understood() {
        let now = Instant::now();
        assert_eq!(
            change("ringing\n", now),
            Some(Change::Stage(Stage::Ringing))
        );
        assert_eq!(
            change("connected", now),
            Some(Change::Stage(Stage::Connected(now)))
        );
        assert_eq!(change("muted", now), Some(Change::Muted(true)));
        assert_eq!(
            change("ended: no answer", now),
            Some(Change::Ended("No answer".into()))
        );
        assert_eq!(
            change("ended: by the other side (Normal)", now),
            Some(Change::Ended("Call ended".into()))
        );
        assert_eq!(
            change("failed: Not signed in.", now),
            Some(Change::Ended("The call failed: Not signed in.".into()))
        );
        assert_eq!(
            change("caller: Ada Quill", now),
            Some(Change::Caller("Ada Quill".into()))
        );
        assert_eq!(change("caller: ", now), None);
        assert_eq!(
            change("ended: declined", now),
            Some(Change::Ended("Declined".into()))
        );
        assert_eq!(change("waiting", now), Some(Change::Waiting(true)));
        assert_eq!(change("admitted", now), Some(Change::Waiting(false)));
        assert_eq!(
            change("people: Ada Quill; Bo Li, Jr.", now),
            Some(Change::People(vec![
                "Ada Quill".into(),
                "Bo Li, Jr.".into()
            ]))
        );
        assert_eq!(
            change("ended: nobody let you in", now),
            Some(Change::Ended("Nobody let you in".into()))
        );
        assert_eq!(change("path: direct", now), None);
        assert_eq!(change("media connected after 1520 ms", now), None);
    }

    #[test]
    fn durations_read_like_a_clock() {
        assert_eq!(duration(5), "0:05");
        assert_eq!(duration(725), "12:05");
        assert_eq!(duration(3725), "1:02:05");
    }
}
