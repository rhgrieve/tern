//! The main window.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use store::Store;
use tern_ipc::{Event, Request, Status};
use tokio::sync::mpsc;

use crate::client::{Client, News};
use crate::composer::{Aim, Composer, Written};
use crate::history::{History, Need};
use crate::list::MessageList;
use crate::model::{self, Conversation, Filter, OUTGOING};
use crate::row::Item;
use crate::sidebar::Sidebar;
use crate::tools::{Tool, Tools};
use crate::{art, mention, palette};

const CHATS: usize = 400;

#[derive(Default)]
struct State {
    store: Option<Arc<Store>>,
    account: Option<String>,
    open: Option<String>,
    /// The post whose thread is open.
    thread: Option<String>,
    channel: bool,
    history: History,
    filter: Filter,
}

pub struct Window {
    pub window: adw::ApplicationWindow,
    split: adw::NavigationSplitView,
    sidebar: Sidebar,
    list: MessageList,
    composer: Composer,
    title: adw::WindowTitle,
    banner: adw::Banner,
    toasts: adw::ToastOverlay,
    pages: gtk::Stack,
    back: gtk::Button,
    /// Shown while older messages are fetched.
    loading: gtk::Box,
    tools: RefCell<Option<Rc<Tools>>>,
    client: Client,
    state: RefCell<State>,
    /// Counts reads, so that a slow one does not replace a newer.
    chats_turn: Cell<u64>,
    open_turn: Cell<u64>,
    demo: bool,
}

/// What a key does.
type Run = fn(&Rc<Window>);

pub fn load_style() {
    let style = gtk::CssProvider::new();
    style.load_from_string(include_str!("style.css"));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &style,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

impl Window {
    pub fn new(app: &adw::Application, client: Client, demo: bool) -> Rc<Window> {
        Rc::new_cyclic(|weak: &Weak<Window>| {
            let later = |run: fn(&Rc<Window>)| {
                let weak = weak.clone();
                move || {
                    if let Some(window) = weak.upgrade() {
                        run(&window);
                    }
                }
            };

            // Chats
            let sidebar = Sidebar::new({
                let weak = weak.clone();
                move |id| {
                    if let Some(window) = weak.upgrade() {
                        window.open(id);
                    }
                }
            });
            let search = gtk::Button::builder()
                .icon_name("system-search-symbolic")
                .tooltip_text("Search (Ctrl+K)")
                .build();
            search.connect_clicked({
                let search = later(Window::search);
                move |_| search()
            });
            let side_header = adw::HeaderBar::builder()
                .title_widget(&adw::WindowTitle::new("Tern", ""))
                .build();
            side_header.pack_start(&search);
            let filters = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            filters.add_css_class("linked");
            filters.set_halign(gtk::Align::Center);
            filters.set_margin_bottom(6);
            let mut first: Option<gtk::ToggleButton> = None;
            for (name, filter) in [
                ("Chats", Filter::Chats),
                ("Unread", Filter::Unread),
                ("Channels", Filter::Channels),
            ] {
                let button = gtk::ToggleButton::builder()
                    .label(name)
                    .active(first.is_none())
                    .build();
                button.set_group(first.as_ref());
                button.connect_toggled({
                    let weak = weak.clone();
                    move |button| {
                        if let Some(window) = weak.upgrade().filter(|_| button.is_active()) {
                            window.state.borrow_mut().filter = filter;
                            window.read_chats();
                        }
                    }
                });
                filters.append(&button);
                first.get_or_insert(button);
            }
            let side = adw::ToolbarView::builder().content(&sidebar.root).build();
            side.add_top_bar(&side_header);
            side.add_top_bar(&filters);

            // Conversation
            let list = MessageList::default();
            let loading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            loading.add_css_class("loading");
            loading.set_halign(gtk::Align::Center);
            loading.set_valign(gtk::Align::Start);
            loading.set_can_target(false);
            loading.set_visible(false);
            loading.append(&adw::Spinner::new());
            loading.append(&gtk::Label::new(Some("Loading older messages")));
            list.over.add_overlay(&loading);
            let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
            wheel.connect_scroll({
                let scrolled = later(Window::scrolled);
                move |_, _, up| {
                    if up < 0.0 {
                        scrolled();
                    }
                    glib::Propagation::Proceed
                }
            });
            list.over.add_controller(wheel);
            let clamp = adw::Clamp::builder()
                .maximum_size(980)
                .child(&list.over)
                .vexpand(true)
                .build();
            let composer = Composer::new(
                {
                    let weak = weak.clone();
                    move |id, text, aim| {
                        if let Some(window) = weak.upgrade() {
                            window.send(id, text, aim);
                        }
                    }
                },
                later(Window::edit_last),
            );
            let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
            drop.set_types(&[gdk::FileList::static_type(), gdk::Texture::static_type()]);
            drop.connect_drop({
                let composer = composer.clone();
                move |_, value, _, _| {
                    if let Ok(files) = value.get::<gdk::FileList>() {
                        composer
                            .attach(files.files().iter().filter_map(gio::prelude::FileExt::path));
                    } else if let Ok(picture) = value.get::<gdk::Texture>() {
                        composer.attach_picture(&picture);
                    } else {
                        return false;
                    }
                    true
                }
            });
            let write = adw::Clamp::builder()
                .maximum_size(980)
                .child(&composer.root)
                .build();
            let talk = gtk::Box::new(gtk::Orientation::Vertical, 0);
            talk.append(&clamp);
            talk.append(&write);
            talk.add_controller(drop);
            let pages = gtk::Stack::new();
            let nothing = adw::StatusPage::builder()
                .icon_name("mail-unread-symbolic")
                .title("No conversation open")
                .description("Choose one on the left, or press Ctrl+K")
                .build();
            pages.add_named(&nothing, Some("nothing"));
            pages.add_named(&talk, Some("talk"));
            let title = adw::WindowTitle::new("", "");
            let header = adw::HeaderBar::builder().title_widget(&title).build();
            let back = gtk::Button::builder()
                .icon_name("go-previous-symbolic")
                .tooltip_text("Back to the channel (Esc)")
                .visible(false)
                .build();
            back.connect_clicked({
                let back = later(Window::leave_thread);
                move |_| back()
            });
            header.pack_start(&back);
            let banner = adw::Banner::new("");
            let main = adw::ToolbarView::builder().content(&pages).build();
            main.add_top_bar(&header);
            main.add_top_bar(&banner);

            let split = adw::NavigationSplitView::builder()
                .sidebar(&adw::NavigationPage::new(&side, "Chats"))
                .content(&adw::NavigationPage::new(&main, "Conversation"))
                .min_sidebar_width(260.0)
                .max_sidebar_width(360.0)
                .sidebar_width_fraction(0.3)
                .build();
            let toasts = adw::ToastOverlay::new();
            toasts.set_child(Some(&split));
            let window = adw::ApplicationWindow::builder()
                .application(app)
                .title("Tern")
                .default_width(1100)
                .default_height(760)
                .width_request(340)
                .height_request(300)
                .content(&toasts)
                .build();
            let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
                adw::BreakpointConditionLengthType::MaxWidth,
                720.0,
                adw::LengthUnit::Sp,
            ));
            narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
            window.add_breakpoint(narrow);

            // Keys
            let keys: [(&str, &[&str], Run); 6] = [
                ("leave", &["Escape"], Window::leave_thread),
                ("search", &["<Ctrl>k", "<Ctrl>f"], Window::search),
                ("next", &["<Alt>Down"], |window| window.step(1)),
                ("previous", &["<Alt>Up"], |window| window.step(-1)),
                ("write", &["<Ctrl>r"], |window| window.composer.focus()),
                ("newest", &["<Ctrl>End"], |window| window.list.to_end()),
            ];
            for (name, shortcuts, run) in keys {
                let action = gio::SimpleAction::new(name, None);
                action.connect_activate({
                    let run = later(run);
                    move |_, _| run()
                });
                window.add_action(&action);
                app.set_accels_for_action(&format!("win.{name}"), shortcuts);
            }
            let thread = gio::SimpleAction::new("thread", Some(glib::VariantTy::STRING));
            thread.connect_activate({
                let weak = weak.clone();
                move |_, id| {
                    if let (Some(window), Some(id)) =
                        (weak.upgrade(), id.and_then(|id| id.get::<String>()))
                    {
                        window.open_thread(&id);
                    }
                }
            });
            window.add_action(&thread);
            let react = gio::SimpleAction::new(
                "react",
                Some(&<(String, String, bool)>::static_variant_type()),
            );
            react.connect_activate({
                let weak = weak.clone();
                move |_, what| {
                    let what = what.and_then(|what| what.get::<(String, String, bool)>());
                    if let (Some(window), Some((message, emoji, on))) = (weak.upgrade(), what) {
                        window.react(&message, &emoji, on);
                    }
                }
            });
            window.add_action(&react);
            window.connect_is_active_notify({
                let seen = later(Window::seen);
                move |_| seen()
            });
            list.root.vadjustment().connect_value_changed({
                let moved = later(Window::moved);
                move |_| moved()
            });

            Window {
                window,
                split,
                sidebar,
                list,
                composer,
                title,
                banner,
                toasts,
                pages,
                back,
                loading,
                tools: RefCell::default(),
                client,
                state: RefCell::default(),
                chats_turn: Cell::new(0),
                open_turn: Cell::new(0),
                demo,
            }
        })
    }

    /// The bar over messages needs the window to exist.
    fn add_tools(self: &Rc<Self>) {
        if self.tools.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        let tools = Tools::attach(&self.list, move |item, tool| {
            if let Some(window) = weak.upgrade() {
                window.use_tool(item, tool);
            }
        });
        self.tools.replace(Some(tools));
    }

    fn use_tool(self: &Rc<Self>, item: &Rc<Item>, tool: Tool) {
        match tool {
            Tool::React(emoji) => {
                let mine = item
                    .reactions
                    .iter()
                    .any(|reaction| reaction.mine && reaction.emoji == emoji);
                self.react(&item.id, &emoji, !mine);
            }
            // In a channel an answer belongs into the thread of its post.
            Tool::Reply if item.thread.is_some() => self.open_thread(&item.id),
            Tool::Reply => self.composer.reply(&item.id, &item.sender, &item.text),
            Tool::Copy => self.window.clipboard().set_text(&item.text),
            Tool::Edit => {
                if let Some(source) = &item.source {
                    self.composer.edit(&item.id, source);
                }
            }
            Tool::Delete => self.delete(item),
            Tool::Retry | Tool::Discard => {
                let Some(id) = item.id.strip_prefix(OUTGOING).map(str::to_owned) else {
                    return;
                };
                self.client.ask(if tool == Tool::Retry {
                    Request::Retry { id }
                } else {
                    Request::Discard { id }
                });
            }
        }
    }

    fn offer_people(self: &Rc<Self>, conversation: &str) {
        let Some(store) = self.state.borrow().store.clone() else {
            return;
        };
        let people = mention::people(&store, conversation);
        self.composer
            .set_people(move |typed| mention::matching(&people, typed, 8));
    }

    fn edit_last(self: &Rc<Self>) {
        if let Some(item) = self.list.last_editable()
            && let Some(source) = &item.source
        {
            self.composer.edit(&item.id, source);
        }
    }

    /// Gives back what was written when the daemon did not take it.
    fn not_taken(self: &Rc<Self>, request: Request) {
        match request {
            Request::Send {
                conversation,
                text,
                thread,
                mentions,
                files,
                ..
            } => {
                let key = match thread {
                    Some(post) => format!("{conversation}/{post}"),
                    None => conversation,
                };
                self.composer.give_back(
                    &key,
                    Written {
                        text,
                        mentions,
                        files,
                    },
                );
            }
            Request::Edit {
                conversation,
                message,
                text,
            } => {
                let open = self.state.borrow().open.clone();
                if open.as_ref() == Some(&conversation) {
                    self.composer.edit(&message, &text);
                }
            }
            _ => {}
        }
    }

    fn react(self: &Rc<Self>, message: &str, emoji: &str, on: bool) {
        let Some(conversation) = self.state.borrow().open.clone() else {
            return;
        };
        self.client.ask(Request::React {
            conversation,
            message: message.to_owned(),
            emoji: emoji.to_owned(),
            on,
        });
    }

    fn delete(self: &Rc<Self>, item: &Rc<Item>) {
        let Some(conversation) = self.state.borrow().open.clone() else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some("Delete this message?"),
            Some("It is removed for everyone in the conversation."),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let (weak, message) = (Rc::downgrade(self), item.id.clone());
        dialog.connect_response(Some("delete"), move |_, _| {
            if let Some(window) = weak.upgrade() {
                window.client.ask(Request::Delete {
                    conversation: conversation.clone(),
                    message: message.clone(),
                });
            }
        });
        dialog.present(Some(&self.window));
    }

    pub fn open_thread(self: &Rc<Self>, post: &str) {
        {
            let mut state = self.state.borrow_mut();
            if !state.channel || state.thread.is_some() {
                return;
            }
            state.thread = Some(post.to_owned());
            state.history.count = 0;
        }
        self.enter();
    }

    fn leave_thread(self: &Rc<Self>) {
        {
            let mut state = self.state.borrow_mut();
            if state.thread.take().is_none() {
                return;
            }
            state.history.count = 0;
        }
        self.enter();
    }

    /// Shows what was just opened and asks the service for what is new in it.
    fn enter(self: &Rc<Self>) {
        let (open, thread) = {
            let state = self.state.borrow();
            (state.open.clone(), state.thread.clone())
        };
        let Some(conversation) = open else { return };
        let key = match &thread {
            Some(post) => format!("{conversation}/{post}"),
            None => conversation.clone(),
        };
        self.composer.open(&key);
        self.loading.set_visible(false);
        self.read(true);
        if !self.demo {
            self.client.ask(Request::Look {
                conversation,
                post: thread,
            });
        }
    }

    /// Shows the cache of `account`, or the demo.
    pub fn use_store(self: &Rc<Self>, store: Arc<Store>, account: Option<String>) {
        let pictures = account
            .as_deref()
            .and_then(|account| tern_engine::database_path(account).ok())
            .map(|path| path.with_file_name("media").join("pictures"));
        art::set_source(store.clone(), pictures, self.window.scale_factor());
        let filter = self.state.borrow().filter;
        *self.state.borrow_mut() = State {
            store: Some(store),
            account,
            filter,
            ..Default::default()
        };
        self.pages.set_visible_child_name("nothing");
        self.title.set_title("");
        self.title.set_subtitle("");
        self.read_chats();
    }

    /// Opens the newest cache there is, so the window is useful before the daemon answers.
    pub fn use_cache(self: &Rc<Self>) {
        match tern_engine::accounts().into_iter().next() {
            Some(account) => self.use_account(account),
            None => self.tell("Nobody is signed in yet. Run “tern-cli login” in a terminal."),
        }
    }

    fn use_account(self: &Rc<Self>, account: String) {
        if self.state.borrow().account.as_ref() == Some(&account) {
            return;
        }
        match tern_engine::open_read_only(&account) {
            Ok(store) => self.use_store(Arc::new(store), Some(account)),
            Err(error) => self.tell(&format!("The message cache cannot be read: {error}")),
        }
    }

    fn tell(&self, text: &str) {
        self.banner.set_title(&glib::markup_escape_text(text));
        self.banner.set_revealed(!text.is_empty());
    }

    pub fn follow(self: &Rc<Self>, mut news: mpsc::UnboundedReceiver<News>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Some(news) = news.recv().await {
                let Some(window) = weak.upgrade() else { return };
                window.heard(news);
            }
        });
    }

    fn heard(self: &Rc<Self>, news: News) {
        match news {
            News::Event(Event::Status { status }) => self.status(&status),
            News::Event(Event::Chats) => {
                self.read_chats();
                self.read_open();
            }
            News::Event(Event::Messages { conversation }) => {
                if self.state.borrow().open.as_ref() == Some(&conversation) {
                    self.read_open();
                }
            }
            // Pictures appear when their row is shown again; redrawing all would lose selections.
            News::Event(Event::Pictures) => {}
            News::Event(Event::Faces) => {
                art::forget_faces();
                self.sidebar.reload_faces();
            }
            News::Older { conversation, done } => self.older_came(&conversation, done),
            News::Gone => {
                self.tell("The background service is not running. You can read, but not send.")
            }
            News::Unsent { request, reason } => {
                self.toasts
                    .add_toast(adw::Toast::new(&glib::markup_escape_text(&reason)));
                self.not_taken(*request);
            }
            News::Refused(reason) => self
                .toasts
                .add_toast(adw::Toast::new(&glib::markup_escape_text(&reason))),
        }
    }

    fn status(self: &Rc<Self>, status: &Status) {
        if let Some(account) = &status.account_id {
            self.use_account(account.clone());
        }
        let text = match (status.push.as_str(), &status.problem) {
            ("connected", _) => String::new(),
            (_, Some(problem)) => problem.clone(),
            ("signed out", None) => "Signed out. Run “tern-cli login” in a terminal.".into(),
            _ => "Offline. Messages are sent when the connection is back.".into(),
        };
        self.tell(&text);
        // An outgoing message may have gone out.
        self.read_open();
    }

    fn read_chats(self: &Rc<Self>) {
        let (store, filter) = {
            let state = self.state.borrow();
            (state.store.clone(), state.filter)
        };
        let Some(store) = store else { return };
        let turn = self.chats_turn.get() + 1;
        self.chats_turn.set(turn);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let rows = gio::spawn_blocking(move || {
                let now = model::now();
                if filter == Filter::Channels {
                    let teams = store
                        .sidebar()
                        .map(|sidebar| sidebar.teams)
                        .unwrap_or_default();
                    return model::channel_rows(&teams, now);
                }
                let me = store.meta("me_user_id").ok().flatten();
                let chats = store.recent_chats(CHATS).unwrap_or_default();
                model::chat_rows(&chats, filter, me.as_deref(), now)
            })
            .await;
            if let (Some(window), Ok(rows)) = (weak.upgrade(), rows)
                && window.chats_turn.get() == turn
            {
                window.sidebar.set(rows);
            }
        });
    }

    pub fn open(self: &Rc<Self>, id: &str) {
        let already = {
            let mut state = self.state.borrow_mut();
            let already = state.open.as_deref() == Some(id);
            if !already {
                state.open = Some(id.to_owned());
                state.thread = None;
                state.channel = false;
                state.history = History::new();
            }
            already
        };
        self.split.set_show_content(true);
        self.sidebar.select(id);
        if already {
            return;
        }
        self.add_tools();
        self.offer_people(id);
        self.enter();
    }

    fn read_open(self: &Rc<Self>) {
        self.read(false);
    }

    /// Reads the open conversation; `fresh` when it was not shown before.
    fn read(self: &Rc<Self>, fresh: bool) {
        let (store, id, thread, limit) = {
            let state = self.state.borrow();
            (
                state.store.clone(),
                state.open.clone(),
                state.thread.clone(),
                state.history.limit,
            )
        };
        let (Some(store), Some(id)) = (store, id) else {
            return;
        };
        let turn = self.open_turn.get() + 1;
        self.open_turn.set(turn);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let conversation = gio::spawn_blocking(move || {
                model::conversation(&store, &id, thread.as_deref(), limit, model::now())
            })
            .await;
            if let (Some(window), Ok(conversation)) = (weak.upgrade(), conversation) {
                // A newer read of the same conversation must still start at the end.
                if window.open_turn.get() == turn {
                    window.show(
                        conversation,
                        fresh || window.state.borrow().history.count == 0,
                    );
                }
            }
        });
    }

    fn show(self: &Rc<Self>, conversation: Conversation, fresh: bool) {
        self.title.set_title(&conversation.title);
        let people = conversation
            .items
            .iter()
            .filter_map(|item| item.sender_id.clone())
            .collect();
        let list = self.list.clone();
        art::load_faces(people, move |loaded| list.show_faces(loaded));
        self.title.set_subtitle(&conversation.subtitle);
        let thread = {
            let mut state = self.state.borrow_mut();
            state.history.read(conversation.read.max(1));
            state.channel = conversation.channel;
            state.thread.is_some()
        };
        self.back.set_visible(thread);
        self.composer
            .set_hint(match (conversation.channel, thread) {
                (true, false) => "Start a new post",
                (_, true) => "Reply in this thread",
                (false, false) => "Write a message",
            });
        if fresh {
            self.list.set(conversation.items);
            self.composer.focus();
        } else {
            self.list.update(conversation.items);
        }
        self.pages.set_visible_child_name("talk");
        self.seen();
        // Once the new rows have their place: more may be wanted still.
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
            if let Some(window) = weak.upgrade() {
                window.want_older();
            }
        });
    }

    fn moved(self: &Rc<Self>) {
        self.want_older();
        self.seen();
    }

    /// The user tried to scroll, also where there is nothing left to scroll.
    fn scrolled(self: &Rc<Self>) {
        self.state.borrow_mut().history.touched();
        self.want_older();
    }

    /// Near the start, or with room left, older messages are added.
    fn want_older(self: &Rc<Self>) {
        let (store, open) = {
            let state = self.state.borrow();
            if state.thread.is_some() {
                return;
            }
            (state.store.clone(), state.open.clone())
        };
        let (Some(store), Some(conversation)) = (store, open) else {
            return;
        };
        let short = self.list.is_short();
        let at_start = short || (self.list.at_start() && !self.list.follows());
        if !at_start {
            return;
        }
        let more = store
            .sync_state(&conversation)
            .ok()
            .flatten()
            .is_some_and(|state| state.has_more);
        let need = self
            .state
            .borrow_mut()
            .history
            .need(at_start, more && !self.demo);
        match need {
            Need::Nothing => {}
            Need::Show => self.read(false),
            Need::Fetch => {
                self.loading.set_visible(true);
                self.client.ask(Request::Older { conversation });
            }
        }
    }

    fn older_came(self: &Rc<Self>, conversation: &str, done: bool) {
        {
            let mut state = self.state.borrow_mut();
            // Another conversation is open by now, with its own history.
            if state.open.as_deref() != Some(conversation) || !state.history.is_fetching() {
                return;
            }
            state.history.fetched();
        }
        self.loading.set_visible(false);
        if !done {
            self.toasts
                .add_toast(adw::Toast::new("Older messages could not be loaded"));
        }
        self.read(false);
    }

    /// Others see that a message was read, so this needs the user to really have it on screen.
    fn seen(self: &Rc<Self>) {
        if self.demo || !self.window.is_active() || !self.list.at_end() {
            return;
        }
        let (store, id) = {
            let state = self.state.borrow();
            (state.store.clone(), state.open.clone())
        };
        let (Some(store), Some(id)) = (store, id) else {
            return;
        };
        let visible = !self.split.is_collapsed() || self.split.shows_content();
        let unread = store
            .chat(&id)
            .ok()
            .flatten()
            .is_some_and(|chat| chat.unread);
        if visible && unread {
            self.client.ask(Request::MarkRead { conversation: id });
        }
    }

    fn send(self: &Rc<Self>, _key: &str, written: &Written, aim: &Aim) {
        let (conversation, thread, channel) = {
            let state = self.state.borrow();
            (state.open.clone(), state.thread.clone(), state.channel)
        };
        let Some(conversation) = conversation else {
            return;
        };
        let text = written.text.clone();
        self.client.ask(match aim {
            Aim::Edit { message } => Request::Edit {
                conversation,
                message: message.clone(),
                text,
            },
            aim => Request::Send {
                conversation,
                text,
                reply_to: match aim {
                    Aim::Reply { message, .. } => Some(message.clone()),
                    _ => None,
                },
                post: channel && thread.is_none(),
                thread,
                subject: None,
                mentions: written.mentions.clone(),
                files: written.files.clone(),
            },
        });
        if matches!(aim, Aim::Edit { .. }) {
            return;
        }
        self.list.to_end();
        // It is in the outbox within moments.
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(80), move || {
            if let Some(window) = weak.upgrade() {
                window.read_open();
            }
        });
    }

    fn search(self: &Rc<Self>) {
        let Some(store) = self.state.borrow().store.clone() else {
            return;
        };
        let weak = Rc::downgrade(self);
        palette::show(&self.window, store, move |id| {
            if let Some(window) = weak.upgrade() {
                window.open(id);
            }
        });
    }

    fn step(self: &Rc<Self>, step: i64) {
        if let Some(id) = self.sidebar.neighbour(step) {
            self.open(&id);
        }
    }

    /// How many conversations are listed.
    pub fn chats(&self) -> u32 {
        self.sidebar.count()
    }

    /// Opens the next conversation in the list.
    pub fn measure_open(self: &Rc<Self>) {
        self.step(1);
    }

    /// The open conversation is on screen.
    pub fn is_shown(&self) -> bool {
        self.state.borrow().history.count > 0
    }

    /// Scrolls the chat list down and up again; answers the time between frames in ms.
    pub async fn measure_sidebar(&self) -> Vec<f64> {
        let gaps: Rc<RefCell<Vec<f64>>> = Rc::default();
        let done = Rc::new(Cell::new(false));
        let place = self.sidebar.root.vadjustment();
        let (collected, finished, last) = (gaps.clone(), done.clone(), Cell::new(0i64));
        self.sidebar.root.add_tick_callback(move |_, clock| {
            let now = clock.frame_time();
            let before = last.replace(now);
            if before != 0 {
                collected.borrow_mut().push((now - before) as f64 / 1000.0);
            }
            let frame = collected.borrow().len();
            if frame >= 900 {
                finished.set(true);
                return glib::ControlFlow::Break;
            }
            let step = if frame < 450 { 22.0 } else { -22.0 };
            place.set_value(place.value() + step);
            glib::ControlFlow::Continue
        });
        while !done.get() {
            glib::timeout_future(std::time::Duration::from_millis(20)).await;
        }
        gaps.take()
    }

    /// For tests and screenshots.
    pub fn list(&self) -> &MessageList {
        &self.list
    }
}

/// Saves what the window shows, without capturing the screen.
pub fn save_picture(window: &adw::ApplicationWindow, path: &str) -> Result<(), String> {
    let (width, height) = (window.width(), window.height());
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(window)).snapshot(
        &snapshot,
        f64::from(width),
        f64::from(height),
    );
    let node = snapshot.to_node().ok_or("nothing drawn")?;
    let renderer = gtk::gsk::CairoRenderer::new();
    let display = gtk::prelude::WidgetExt::display(window);
    renderer
        .realize_for_display(&display)
        .map_err(|error| error.to_string())?;
    let texture = renderer.render_texture(&node, None);
    let saved = texture.save_to_png(path).map_err(|error| error.to_string());
    renderer.unrealize();
    saved
}
