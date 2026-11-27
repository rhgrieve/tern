//! All windows together: the main one, and conversations in windows of their own. Drafts, the
//! account and its cache are theirs in common. A conversation is in one window at a time.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use gtk4::gio::prelude::*;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use store::Store;
use tern_engine::Place;

use crate::call::{Call, Change, Stage};
use crate::client::Client;
use crate::composer::{Drafts, Written};
use crate::model::Callable;
use crate::window::Window;
use crate::{art, drafts, join};

/// Windows with one conversation tell themselves apart from the main one, for the rules of window
/// managers.
pub const CHAT_APP: &str = "io.github.rhgrieve.Tern.Chat";

pub struct Desk {
    app: adw::Application,
    /// Nobody is told anything: for the demo and for measuring.
    pub demo: bool,
    pub drafts: Drafts,
    /// The drafts as they were last saved.
    saved: RefCell<HashMap<String, Written>>,
    account: RefCell<Option<String>>,
    store: RefCell<Option<Arc<Store>>>,
    main: RefCell<Option<Rc<Window>>>,
    /// Closed ones stay, to be used again.
    chats: RefCell<Vec<Rc<Window>>>,
    /// The call that is on, one at a time, whichever window it was started in.
    call: RefCell<Option<Rc<Call>>>,
    /// A clock of the call is running.
    ticking: Cell<bool>,
}

impl Desk {
    pub fn new(app: &adw::Application, demo: bool) -> Rc<Desk> {
        Rc::new(Desk {
            app: app.clone(),
            demo,
            drafts: Rc::default(),
            saved: RefCell::default(),
            account: RefCell::default(),
            store: RefCell::default(),
            main: RefCell::default(),
            chats: RefCell::default(),
            call: RefCell::default(),
            ticking: Cell::default(),
        })
    }

    fn windows(&self) -> Vec<Rc<Window>> {
        let mut all: Vec<Rc<Window>> = self.main.borrow().iter().cloned().collect();
        all.extend(self.chats.borrow().iter().cloned());
        all
    }

    pub fn call_on(&self) -> Option<Rc<Call>> {
        self.call.borrow().clone()
    }

    /// Calls `who`; `from` is told how it ended.
    pub fn call(self: &Rc<Self>, who: &Callable, from: &Rc<Window>) {
        if self.demo || self.call.borrow().is_some() {
            return;
        }
        self.log_call(if who.is_group() {
            "calling a group"
        } else {
            "calling someone"
        });
        let started = Call::place(who, self.follow(Rc::downgrade(from)));
        self.started(started, from);
    }

    /// Answers the call that `push` told of, in the main window.
    pub fn answer(self: &Rc<Self>, push: &str) {
        let main = self.main();
        main.window.present();
        if self.demo {
            return;
        }
        if self.call.borrow().is_some() {
            self.log_call("not answered: another call is on");
            main.toast("Hang up first to answer another call");
            return;
        }
        self.log_call("answering");
        let started = Call::answer(push, self.follow(Rc::downgrade(&main)));
        self.started(started, &main);
    }

    /// Declines the call that `push` told of, for all the user's devices.
    pub fn decline(&self, push: &str) {
        if self.demo {
            return;
        }
        self.log_call("declining");
        if let Err(error) = Call::decline(push) {
            self.log_call(&format!("could not decline: {error}"));
        }
    }

    fn started(self: &Rc<Self>, started: Result<Rc<Call>, glib::Error>, from: &Rc<Window>) {
        match started {
            Ok(call) => {
                let desk = Rc::downgrade(self);
                call.on_said(move |line| {
                    if let Some(desk) = desk.upgrade() {
                        desk.log_call(line);
                    }
                });
                self.call.replace(Some(call));
                self.show_call();
            }
            Err(error) => {
                self.log_call(&format!("could not start: {error}"));
                from.toast(&format!("The call could not start: {error}"));
            }
        }
    }

    /// Tells the service's log how a call goes.
    fn log_call(&self, line: &str) {
        if let Some(window) = self.windows().first() {
            window.log_call(line);
        }
    }

    /// What follows the changes of a call; `told` is told how it ended.
    fn follow(self: &Rc<Self>, told: Weak<Window>) -> impl Fn(&Rc<Call>, Change) + 'static {
        let desk = Rc::downgrade(self);
        move |_, change| {
            let Some(desk) = desk.upgrade() else { return };
            match change {
                Change::Ended(text) => {
                    desk.call.replace(None);
                    if let Some(window) = told.upgrade() {
                        window.toast(&text);
                    }
                }
                Change::Notice(text) => {
                    if let Some(window) = told.upgrade() {
                        window.toast(&text);
                    }
                }
                Change::Stage(Stage::Connected(_)) => desk.tick(),
                _ => {}
            }
            desk.show_call();
        }
    }

    /// Every window shows the call as it is.
    fn show_call(&self) {
        let call = self.call_on();
        for window in self.windows() {
            window.show_call(call.as_ref());
        }
    }

    /// Moves the clocks on while a call is connected.
    fn tick(self: &Rc<Self>) {
        if self.ticking.replace(true) {
            return;
        }
        let desk = Rc::downgrade(self);
        glib::timeout_add_seconds_local(1, move || {
            let Some(desk) = desk.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let on = desk
                .call_on()
                .is_some_and(|call| matches!(call.stage(), Stage::Connected(_)));
            if !on {
                desk.ticking.set(false);
                return glib::ControlFlow::Break;
            }
            desk.show_call();
            glib::ControlFlow::Continue
        });
    }

    pub fn app(&self) -> &adw::Application {
        &self.app
    }

    fn window(self: &Rc<Self>, own: bool) -> Rc<Window> {
        if self.demo {
            return Window::new(self, Client::nowhere(), own);
        }
        let (client, news) = Client::start();
        let window = Window::new(self, client, own);
        window.follow(news);
        window.show_call(self.call_on().as_ref());
        window
    }

    /// The main window, made or opened again if need be. Shows the newest cache there is.
    pub fn main(self: &Rc<Self>) -> Rc<Window> {
        let there = self.main.borrow().clone();
        let window = there.unwrap_or_else(|| {
            let window = self.window(false);
            let store = self.store.borrow().clone();
            let account = self.account.borrow().clone();
            match store {
                Some(store) => window.use_store(store, account),
                None => window.use_cache(),
            }
            self.main.replace(Some(window.clone()));
            window
        });
        self.app.add_window(&window.window);
        window
    }

    /// The window that has the conversation to itself.
    pub fn chat(&self, id: &str) -> Option<Rc<Window>> {
        self.chats
            .borrow()
            .iter()
            .find(|window| window.shows(id))
            .cloned()
    }

    /// Shows the conversation wherever it is, else in the main window.
    pub fn open(self: &Rc<Self>, id: &str) {
        match self.chat(id) {
            Some(window) => window.window.present(),
            None => {
                let main = self.main();
                main.open(id);
                main.window.present();
            }
        }
    }

    /// Where an address of Teams leads: to a chat here, else to a browser.
    pub fn go(self: &Rc<Self>, address: &str) {
        match tern_engine::place(address) {
            Some(Place::Chat(id)) => self.open(&id),
            Some(_) => self.join(address),
            None => self.main().window.present(),
        }
    }

    /// Joins a meeting: in Tern, with sound, when it is a meeting Tern can join; else in a browser.
    pub fn join(self: &Rc<Self>, address: &str) {
        match join::here(address).filter(|_| !self.demo) {
            Some(link) => self.meet(&link),
            None => self.join_in_browser(address),
        }
    }

    /// Joins the meeting of `link` in Tern, in the main window.
    fn meet(self: &Rc<Self>, link: &str) {
        let main = self.main();
        main.window.present();
        if self.call.borrow().is_some() {
            main.toast("Hang up first to join the meeting");
            return;
        }
        let name = self
            .meeting_name(link)
            .unwrap_or_else(|| "the meeting".into());
        let started = Call::meeting(link, &name, self.follow(Rc::downgrade(&main)));
        self.started(started, &main);
    }

    /// The subject of the meeting of `link`, if the calendar has it.
    fn meeting_name(&self, link: &str) -> Option<String> {
        let store = self.store.borrow().clone()?;
        tern_engine::agenda(&store)?
            .meetings
            .into_iter()
            .find(|meeting| {
                meeting
                    .join
                    .as_deref()
                    .and_then(join::here)
                    .is_some_and(|other| other == link)
            })
            .map(|meeting| meeting.subject)
            .filter(|subject| !subject.trim().is_empty())
    }

    /// Opens a meeting, or what else only Teams shows, in a browser. Before the first meeting the
    /// user has to sign in there.
    pub fn join_in_browser(self: &Rc<Self>, address: &str) {
        if !join::as_guest(address) {
            return self.browse(address);
        }
        let dialog = adw::AlertDialog::new(
            Some("Sign in for meetings?"),
            Some(
                "Meetings open in a browser window of their own, which does not have Tern's sign-in. Sign in there once, and meetings know you from then on.",
            ),
        );
        dialog.add_responses(&[
            ("cancel", "Cancel"),
            ("guest", "Join as Guest"),
            ("sign-in", "Sign In"),
        ]);
        dialog.set_response_appearance("sign-in", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("sign-in"));
        dialog.set_close_response("cancel");
        let (desk, address) = (self.clone(), address.to_owned());
        dialog.connect_response(None, move |_, response| match response {
            "guest" => desk.browse(&address),
            "sign-in" => {
                desk.browse(join::TEAMS);
                desk.join_signed_in(&address);
            }
            _ => {}
        });
        let main = self.main();
        main.window.present();
        dialog.present(Some(&main.window));
    }

    /// Goes on to the meeting once the user has signed in.
    fn join_signed_in(self: &Rc<Self>, address: &str) {
        let dialog = adw::AlertDialog::new(
            Some("Sign in in the browser window"),
            Some("When Teams shows there, you are signed in, and can join."),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("join", "Join")]);
        dialog.set_response_appearance("join", adw::ResponseAppearance::Suggested);
        dialog.set_close_response("cancel");
        let (desk, address) = (self.clone(), address.to_owned());
        dialog.connect_response(Some("join"), move |_, _| {
            join::remember_sign_in();
            desk.browse(&address);
        });
        dialog.present(Some(&self.main().window));
    }

    fn browse(self: &Rc<Self>, address: &str) {
        let over = self.app.active_window();
        // Until it is open: there may be no window, and then nothing else keeps this running.
        let (hold, desk) = (self.app.hold(), self.clone());
        join::go(address, over, move |reason| {
            let main = desk.main();
            main.window.present();
            main.toast(&reason);
            drop(hold);
        });
    }

    pub fn show_calendar(self: &Rc<Self>) {
        let main = self.main();
        main.show_calendar();
        main.window.present();
    }

    /// Gives the conversation a window of its own.
    pub fn pop_out(self: &Rc<Self>, id: &str) {
        if let Some(window) = self.chat(id) {
            return window.window.present();
        }
        let Some(store) = self.store.borrow().clone() else {
            return;
        };
        let main = self.main.borrow().clone();
        if let Some(main) = main.filter(|main| main.shows(id)) {
            main.leave();
        }
        let free = self
            .chats
            .borrow()
            .iter()
            .find(|window| window.is_free())
            .cloned();
        let window = free.unwrap_or_else(|| {
            let window = self.window(true);
            self.chats.borrow_mut().push(window.clone());
            window
        });
        self.app.add_window(&window.window);
        let account = self.account.borrow().clone();
        window.use_store(store, account);
        window.open(id);
        window.window.present();
    }

    /// The conversation's own window was closed: the main one goes on with it, unless another
    /// conversation is being read there.
    pub fn take_back(&self, id: &str, thread: Option<&str>) {
        let main = self.main.borrow().clone();
        if let Some(main) = main.filter(|main| !main.is_reading()) {
            match thread {
                Some(post) => main.open_post(id, post),
                None => main.open(id),
            }
        }
    }

    /// Uses this cache, without an account: the demo.
    pub fn use_store(&self, store: Arc<Store>, scale: i32) {
        art::set_source(store.clone(), None, scale);
        self.store.replace(Some(store));
    }

    pub fn is_account(&self, account: Option<&str>) -> bool {
        self.account.borrow().as_deref() == account
    }

    /// The cache of `account`. Conversations of another one are closed, and their drafts saved.
    pub fn account(&self, account: &str, scale: i32) -> Result<Arc<Store>, String> {
        if self.is_account(Some(account))
            && let Some(store) = self.store.borrow().clone()
        {
            return Ok(store);
        }
        let store = Arc::new(tern_engine::open_read_only(account).map_err(|e| e.to_string())?);
        let others = self.chats.borrow().clone();
        for window in others.iter().filter(|window| !window.is_free()) {
            window.put_away();
        }
        let main = self.main.borrow().clone();
        if let Some(main) = main {
            main.leave();
        }
        self.save();
        let pictures = tern_engine::database_path(account)
            .ok()
            .map(|path| path.with_file_name("media").join("pictures"));
        art::set_source(store.clone(), pictures, scale);
        let path = drafts::path(account).filter(|_| !self.demo);
        let written = path.map(|path| drafts::load(&path)).unwrap_or_default();
        self.saved.replace(written.clone());
        self.drafts.replace(written);
        self.account.replace(Some(account.to_owned()));
        self.store.replace(Some(store.clone()));
        Ok(store)
    }

    /// Where drafts are kept.
    fn path(&self) -> Option<PathBuf> {
        let account = self.account.borrow();
        drafts::path(account.as_deref()?).filter(|_| !self.demo)
    }

    /// Writes down what changed in the drafts.
    pub fn save(&self) {
        let Some(path) = self.path() else { return };
        let all = self.drafts.borrow().clone();
        if *self.saved.borrow() == all {
            return;
        }
        match drafts::save(&path, &all) {
            Ok(()) => drop(self.saved.replace(all)),
            Err(error) => eprintln!("tern: drafts could not be saved: {error}"),
        }
    }

    /// Gives back what could not be sent, to whichever window has its conversation by now.
    pub fn give_back(&self, key: &str, unsent: Written) {
        let main = self.main.borrow().clone();
        let chats = self.chats.borrow().clone();
        let holder = main
            .into_iter()
            .chain(chats)
            .find(|window| window.composer().is_open(key));
        match holder {
            Some(window) => window.composer().give_back(key, unsent),
            None => {
                let mut drafts = self.drafts.borrow_mut();
                let written = drafts.remove(key).unwrap_or_default();
                drafts.insert(key.to_owned(), unsent.before(written));
            }
        }
    }
}

/// Window managers see a conversation's window under a name of its own.
pub fn name_surface(window: &adw::ApplicationWindow) {
    // GTK gives every window the application's name when it is mapped first.
    window.connect_map(|window| {
        let toplevel = window
            .surface()
            .and_downcast::<gdk4_wayland::WaylandToplevel>();
        if let Some(toplevel) = toplevel {
            toplevel.set_application_id(CHAT_APP);
        }
    });
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use gtk4::{gio, glib};

    use super::*;

    /// As the composer's test: needs a display, and a process of its own.
    #[test]
    #[ignore = "needs a display"]
    fn the_first_meeting_asks_for_a_sign_in_once() {
        use std::os::unix::fs::PermissionsExt;
        let folder = tempfile::tempdir().unwrap();
        let (browser, seen) = (folder.path().join("chromium"), folder.path().join("seen"));
        let script = format!("#!/bin/sh\necho \"$*\" >> '{}'\n", seen.display());
        std::fs::write(&browser, script).unwrap();
        std::fs::set_permissions(&browser, PermissionsExt::from_mode(0o755)).unwrap();
        // Nothing else runs yet that reads these.
        unsafe {
            std::env::set_var("PATH", folder.path());
            std::env::set_var("XDG_DATA_HOME", folder.path().join("data"));
        }
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("io.github.rhgrieve.Tern.Test")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gio::Cancellable::NONE).unwrap();
        let wait = |milliseconds: u64| {
            let end = Instant::now() + Duration::from_millis(milliseconds);
            while Instant::now() < end {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        let desk = Desk::new(&app, true);
        desk.use_store(Arc::new(crate::demo::store()), 1);
        let main = desk.main();
        main.window.present();
        wait(300);
        let opened = || -> Vec<String> {
            let seen = std::fs::read_to_string(&seen).unwrap_or_default();
            seen.lines()
                .filter_map(|line| Some(line.split_once("--app=")?.1.to_owned()))
                .collect()
        };
        let answer = |with: &str| {
            let dialog = main.window.visible_dialog().expect("a dialog");
            let dialog = dialog.downcast::<adw::AlertDialog>().unwrap();
            let detail = glib::Quark::from_str(with);
            dialog.emit_by_name_with_details::<()>("response", detail, &[&with]);
            dialog.force_close();
            wait(500);
        };
        let meeting = "https://teams.microsoft.com/l/meetup-join/a/0";
        let page = "https://teams.microsoft.com/v2/?meetingjoin=true#/l/meetup-join/a/0";

        desk.join_in_browser(meeting);
        assert!(opened().is_empty(), "asks first");
        answer("cancel");
        assert!(opened().is_empty());
        desk.join_in_browser(meeting);
        answer("guest");
        assert_eq!(opened(), [page]);

        // A guest is asked again.
        desk.join_in_browser(meeting);
        answer("sign-in");
        assert_eq!(opened(), [page, join::TEAMS]);
        answer("cancel");
        desk.join_in_browser(meeting);
        answer("sign-in");
        answer("join");
        assert_eq!(opened()[3..], [page]);
        wait(500);
        assert!(main.window.visible_dialog().is_none());

        // From then on nobody is asked.
        desk.join_in_browser(meeting);
        wait(500);
        assert!(main.window.visible_dialog().is_none());
        assert_eq!(opened().len(), 5);
    }

    /// As the composer's test: needs a display.
    #[test]
    #[ignore = "needs a display"]
    fn the_composer_is_not_lost_with_a_closed_window() {
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("io.github.rhgrieve.Tern.Test")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gio::Cancellable::NONE).unwrap();
        let wait = |milliseconds: u64| {
            let end = Instant::now() + Duration::from_millis(milliseconds);
            while Instant::now() < end {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        for width in [400, 1100] {
            let desk = Desk::new(&app, true);
            desk.use_store(Arc::new(crate::demo::store()), 1);
            let main = desk.main();
            main.window.set_default_size(width, 700);
            main.window.present();
            main.open("c4");
            wait(400);

            // Out and back again, with what was being written.
            let written = |text: &str| Written {
                text: text.into(),
                ..Default::default()
            };
            main.composer().give_back("c4", written("half a"));
            desk.pop_out("c4");
            wait(400);
            let own = desk.chat("c4").unwrap();
            assert!(main.is_free() && !main.is_reading());
            own.window.close();
            wait(400);
            assert!(desk.chat("c4").is_none() && own.is_free());
            assert!(main.shows("c4") && main.is_reading(), "{width}");
            assert!(main.composer().is_open("c4") && main.is_shown());
            main.composer().keep();
            assert_eq!(desk.drafts.borrow()["c4"], written("half a"));

            // Not over a conversation that is being read.
            desk.pop_out("c4");
            main.open("c1");
            wait(400);
            desk.chat("c4").unwrap().window.close();
            wait(400);
            assert!(main.shows("c1"), "{width}");

            // In a thread.
            desk.pop_out("c1");
            wait(400);
            desk.take_back("channel", Some("post"));
            assert!(main.composer().is_open("channel/post"));
            main.put_away();
        }
    }
}
