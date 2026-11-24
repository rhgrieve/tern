//! All windows together: the main one, and conversations in windows of their own. Drafts, the
//! account and its cache are theirs in common. A conversation is in one window at a time.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gtk4::prelude::*;
use libadwaita as adw;
use store::Store;

use crate::client::Client;
use crate::composer::{Drafts, Written};
use crate::window::Window;
use crate::{art, drafts};

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
        })
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
