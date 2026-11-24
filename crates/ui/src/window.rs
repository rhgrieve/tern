//! The main window.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use crate::desk::{self, Desk};
use crate::history::{History, Need};
use crate::list::MessageList;
use crate::model::{self, Conversation, Filter, OUTGOING};
use crate::row::{Item, say};
use crate::sidebar::Sidebar;
use crate::tools::{Tool, Tools};
use crate::{art, mention, palette, service};

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
    /// Who is writing in the open conversation, and since when we know.
    typing: Vec<(String, Instant)>,
    /// When the others were last told that the user is writing.
    told: Option<Instant>,
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
    /// Above the chats, which are what one sees first in a narrow window.
    sign_in: adw::Banner,
    toasts: adw::ToastOverlay,
    pages: gtk::Stack,
    back: gtk::Button,
    writing: gtk::Label,
    desk: Rc<Desk>,
    /// Shows one conversation, without the list of chats.
    own: bool,
    out: gtk::Button,
    /// Shown while older messages are fetched.
    loading: gtk::Box,
    tools: RefCell<Option<Rc<Tools>>>,
    client: Client,
    state: RefCell<State>,
    /// Counts reads, so that a slow one does not replace a newer.
    chats_turn: Cell<u64>,
    open_turn: Cell<u64>,
}

/// Someone who stopped without saying so is taken off after this.
const TYPING: Duration = Duration::from_secs(12);
/// How often the others are told.
const TELL: Duration = Duration::from_secs(8);

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
    /// The main window, or one with a conversation of its `own`.
    pub fn new(desk: &Rc<Desk>, client: Client, own: bool) -> Rc<Window> {
        let app = desk.app();
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
                        window.choose(id);
                    }
                }
            });
            sidebar.connect_twice({
                let weak = weak.clone();
                move |id| {
                    if let Some(window) = weak.upgrade() {
                        window.desk.pop_out(id);
                    }
                }
            });
            sidebar.connect_enter(later(|window| window.composer.focus()));
            let search = gtk::Button::builder()
                .icon_name("system-search-symbolic")
                .tooltip_text("Search (Ctrl+K)")
                .build();
            say(&search, "Search");
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
            filters.set_margin_start(6);
            filters.set_margin_end(6);
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
            let sign_in = adw::Banner::builder()
                .title("Nobody is signed in")
                .button_label("Sign In")
                .margin_bottom(6)
                .build();
            sign_in.connect_button_clicked({
                let sign_in = later(Window::sign_in);
                move |_| sign_in()
            });
            side.add_top_bar(&side_header);
            side.add_top_bar(&sign_in);
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
                .child(&list.whole)
                .vexpand(true)
                .build();
            let composer = Composer::new(
                desk.drafts.clone(),
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
            let writing = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            writing.add_css_class("caption");
            writing.add_css_class("dim-label");
            writing.add_css_class("writing");
            let writing_clamp = adw::Clamp::builder()
                .maximum_size(980)
                .child(&writing)
                .build();
            let talk = gtk::Box::new(gtk::Orientation::Vertical, 0);
            talk.append(&clamp);
            talk.append(&writing_clamp);
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
            say(&back, "Back to the channel");
            back.connect_clicked({
                let back = later(Window::leave_thread);
                move |_| back()
            });
            header.pack_start(&back);
            let out = gtk::Button::builder()
                .icon_name("window-new-symbolic")
                .tooltip_text("Open in its own window (Ctrl+Shift+O)")
                .visible(false)
                .build();
            say(&out, "Open in its own window");
            out.connect_clicked({
                let out = later(Window::pop_out);
                move |_| out()
            });
            header.pack_end(&out);
            let banner = adw::Banner::new("");
            let main = adw::ToolbarView::builder().content(&pages).build();
            main.add_top_bar(&header);
            main.add_top_bar(&banner);

            let content = adw::NavigationPage::new(&main, "Conversation");
            content.set_can_pop(!own);
            let split = adw::NavigationSplitView::builder()
                .sidebar(&adw::NavigationPage::new(&side, "Chats"))
                .content(&content)
                .collapsed(own)
                .show_content(own)
                .min_sidebar_width(260.0)
                .max_sidebar_width(360.0)
                .sidebar_width_fraction(0.3)
                .build();
            let toasts = adw::ToastOverlay::new();
            toasts.set_child(Some(&split));
            let window = adw::ApplicationWindow::builder()
                .application(app)
                .title("Tern")
                .default_width(if own { 720 } else { 1100 })
                .default_height(760)
                .width_request(340)
                .height_request(300)
                .content(&toasts)
                .build();
            if own {
                desk::name_surface(&window);
            }
            let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
                adw::BreakpointConditionLengthType::MaxWidth,
                720.0,
                adw::LengthUnit::Sp,
            ));
            narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
            if !own {
                window.add_breakpoint(narrow);
            }

            // Keys
            let keys: [(&str, &[&str], Run); 10] = [
                ("leave", &["Escape"], Window::leave_thread),
                ("search", &["<Ctrl>k", "<Ctrl>f"], Window::search),
                ("next", &["<Alt>Down"], |window| window.step(1)),
                ("previous", &["<Alt>Up"], |window| window.step(-1)),
                ("write", &["<Ctrl>r"], |window| window.composer.focus()),
                ("newest", &["<Ctrl>End"], |window| window.list.to_end()),
                ("out", &["<Ctrl><Shift>o"], Window::pop_out),
                ("area", &["F6"], Window::next_area),
                ("keys", &["<Ctrl>question"], Window::show_keys),
                ("close", &["<Ctrl>w"], |window| window.window.close()),
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
            window.connect_close_request({
                let closed = later(Window::closed);
                move |_| {
                    closed();
                    glib::Propagation::Stop
                }
            });
            glib::timeout_add_local(Duration::from_secs(2), {
                let weak = weak.clone();
                move || match weak.upgrade() {
                    Some(window) => {
                        window.save_drafts();
                        window.show_typing();
                        glib::ControlFlow::Continue
                    }
                    None => glib::ControlFlow::Break,
                }
            });
            composer.connect_changed({
                let weak = weak.clone();
                move |text| {
                    if let Some(window) = weak.upgrade() {
                        window.tell_typing(!text.trim().is_empty());
                    }
                }
            });
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
                sign_in,
                toasts,
                pages,
                back,
                writing,
                desk: desk.clone(),
                own,
                out,
                loading,
                tools: RefCell::default(),
                client,
                state: RefCell::default(),
                chats_turn: Cell::new(0),
                open_turn: Cell::new(0),
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
                self.desk.give_back(
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

    fn save_drafts(self: &Rc<Self>) {
        if self.desk.is_account(self.state.borrow().account.as_deref()) {
            self.composer.keep();
            self.desk.save();
        }
    }

    /// The user closed the window.
    fn closed(self: &Rc<Self>) {
        let (open, thread) = {
            let state = self.state.borrow();
            (state.open.clone(), state.thread.clone())
        };
        self.put_away();
        if let Some(id) = open.filter(|_| self.own) {
            self.desk.take_back(&id, thread.as_deref());
        }
    }

    /// Windows are hidden and used again: what is in them may still be working.
    pub fn put_away(self: &Rc<Self>) {
        self.save_drafts();
        if self.own {
            self.leave();
        }
        self.window.set_visible(false);
        self.desk.app().remove_window(&self.window);
    }

    fn pop_out(self: &Rc<Self>) {
        let open = self.state.borrow().open.clone();
        if let Some(id) = open.filter(|_| !self.own) {
            self.desk.pop_out(&id);
        }
    }

    /// Shows no conversation.
    pub fn leave(self: &Rc<Self>) {
        {
            let mut state = self.state.borrow_mut();
            state.open = None;
            state.thread = None;
            state.channel = false;
            state.history = History::new();
            state.typing.clear();
            state.told = None;
        }
        // Whatever is being read is for nobody.
        self.open_turn.set(self.open_turn.get() + 1);
        self.composer.close();
        self.list.set(Vec::new());
        self.loading.set_visible(false);
        self.writing.set_text("");
        self.back.set_visible(false);
        self.out.set_visible(false);
        self.pages.set_visible_child_name("nothing");
        self.set_titles("", "");
        if !self.own {
            self.split.set_show_content(false);
        }
        self.sidebar.select("");
    }

    fn set_titles(&self, title: &str, subtitle: &str) {
        self.title.set_title(title);
        self.title.set_subtitle(subtitle);
        // Window lists and screen readers get the conversation, too.
        self.window.set_title(Some(match (title, self.own) {
            ("", _) => "Tern",
            (title, _) => title,
        }));
    }

    /// Has this conversation open.
    pub fn shows(&self, id: &str) -> bool {
        self.state.borrow().open.as_deref() == Some(id)
    }

    /// A conversation is in view: one is open, and not behind the list of a narrow window.
    pub fn is_reading(&self) -> bool {
        !self.is_free() && (!self.split.is_collapsed() || self.split.shows_content())
    }

    /// Opens a thread without having seen its channel.
    pub fn open_post(self: &Rc<Self>, id: &str, post: &str) {
        self.open(id);
        self.state.borrow_mut().channel = true;
        self.open_thread(post);
    }

    pub fn is_free(&self) -> bool {
        self.state.borrow().open.is_none()
    }

    pub fn composer(&self) -> &Composer {
        &self.composer
    }

    fn typing(self: &Rc<Self>, conversation: &str, name: &str, active: bool) {
        {
            let mut state = self.state.borrow_mut();
            if state.open.as_deref() != Some(conversation) {
                return;
            }
            state.typing.retain(|(known, _)| known != name);
            if active && !name.trim().is_empty() {
                state.typing.push((name.to_owned(), Instant::now()));
            }
        }
        self.show_typing();
    }

    fn show_typing(self: &Rc<Self>) {
        let mut state = self.state.borrow_mut();
        state.typing.retain(|(_, since)| since.elapsed() < TYPING);
        let names: Vec<&str> = state.typing.iter().map(|(name, _)| name.as_str()).collect();
        let text = crate::model::writing(&names);
        if self.writing.text() != text {
            self.writing.set_text(&text);
        }
    }

    /// Tells the others, now and then while writing and once when done.
    fn tell_typing(self: &Rc<Self>, active: bool) {
        let conversation = {
            let mut state = self.state.borrow_mut();
            // In channels nobody is told, as in threads.
            if self.desk.demo || state.channel || state.thread.is_some() {
                return;
            }
            let due = match (active, state.told) {
                (true, Some(at)) => at.elapsed() >= TELL,
                (true, None) => true,
                (false, told) => told.is_some(),
            };
            if !due {
                return;
            }
            state.told = active.then(Instant::now);
            state.open.clone()
        };
        if let Some(conversation) = conversation.filter(|_| self.window.is_active()) {
            self.client.ask(Request::Typing {
                conversation,
                active,
            });
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
        if !self.desk.demo {
            self.client.ask(Request::Look {
                conversation,
                post: thread,
            });
        }
    }

    /// Shows the cache of `account`, or the demo.
    pub fn use_store(self: &Rc<Self>, store: Arc<Store>, account: Option<String>) {
        self.leave();
        let filter = self.state.borrow().filter;
        *self.state.borrow_mut() = State {
            store: Some(store),
            account,
            filter,
            ..Default::default()
        };
        self.read_chats();
    }

    /// Opens the newest cache there is, so the window is useful before the daemon answers.
    pub fn use_cache(self: &Rc<Self>) {
        match tern_engine::accounts().into_iter().next() {
            Some(account) => self.use_account(account),
            None => self.sign_in.set_revealed(true),
        }
    }

    fn sign_in(self: &Rc<Self>) {
        self.sign_in.set_sensitive(false);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let outcome = service::sign_in().await;
            let Some(window) = weak.upgrade() else { return };
            window.sign_in.set_sensitive(true);
            // The service tells when it has used the sign-in.
            if let Err(error) = outcome {
                window
                    .toasts
                    .add_toast(adw::Toast::new(&glib::markup_escape_text(&error)));
            }
        });
    }

    fn use_account(self: &Rc<Self>, account: String) {
        if self.state.borrow().account.as_ref() == Some(&account) {
            return;
        }
        if self.own {
            // The conversation is of another account.
            if self.state.borrow().account.is_some() && self.window.is_visible() {
                self.window.close();
            }
            return;
        }
        match self.desk.account(&account, self.window.scale_factor()) {
            Ok(store) => self.use_store(store, Some(account)),
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
            News::Event(Event::Typing {
                conversation,
                name,
                active,
            }) => {
                self.typing(&conversation, &name, active);
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
        let signed_out = status.push == "signed out";
        self.sign_in.set_title(match status.account {
            Some(_) => "Signed out of Teams",
            None => "Nobody is signed in",
        });
        let nobody = status.account.is_none() && self.state.borrow().account.is_none();
        self.sign_in.set_revealed(signed_out || nobody);
        let text = match (status.push.as_str(), &status.problem) {
            ("connected", _) => String::new(),
            ("signed out", Some(problem)) if status.account.is_some() => problem.clone(),
            ("signed out", _) => String::new(),
            (_, Some(problem)) => problem.clone(),
            _ => "Offline. Messages are sent when the connection is back.".into(),
        };
        self.tell(&text);
        // An outgoing message may have gone out.
        self.read_open();
    }

    fn read_chats(self: &Rc<Self>) {
        if self.own {
            return;
        }
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

    /// The user chose a conversation in the list or by searching.
    fn choose(self: &Rc<Self>, id: &str) {
        if self.own || self.desk.chat(id).is_some() {
            self.desk.open(id);
            // The list still shows what this window has open.
            let open = self.state.borrow().open.clone();
            let sidebar = self.sidebar.clone();
            glib::idle_add_local_once(move || sidebar.select(open.as_deref().unwrap_or("")));
        } else {
            self.open(id);
        }
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
                state.typing.clear();
                state.told = None;
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
        self.set_titles(&conversation.title, &conversation.subtitle);
        let people = conversation
            .items
            .iter()
            .filter_map(|item| item.sender_id.clone())
            .collect();
        let list = self.list.clone();
        art::load_faces(people, move |loaded| list.show_faces(loaded));
        let thread = {
            let mut state = self.state.borrow_mut();
            state.history.read(conversation.read.max(1));
            state.channel = conversation.channel;
            state.thread.is_some()
        };
        self.back.set_visible(thread);
        self.out.set_visible(!self.own);
        self.composer
            .set_hint(match (conversation.channel, thread) {
                (true, false) => "Start a new post",
                (_, true) => "Reply in this thread",
                (false, false) => "Write a message",
            });
        if fresh {
            self.list.set(conversation.items);
            // Who walks through the chats with the keyboard stays in their list.
            if !self.sidebar.has_keyboard() {
                self.composer.focus();
            }
        } else {
            let before = self.list.newest();
            self.list.update(conversation.items);
            if let (Some(before), Some(item)) = (before, self.list.newest())
                && before.id != item.id
                && !item.own
                && self.list.follows()
                && self.window.is_active()
            {
                self.list
                    .view
                    .announce(&item.spoken(), gtk::AccessibleAnnouncementPriority::Medium);
            }
        }
        self.pages.set_visible_child_name("talk");
        self.seen();
        // Once the new rows have their place: more may be wanted still.
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(120), move || match weak.upgrade() {
            Some(window) if !window.list.is_whole() => glib::ControlFlow::Continue,
            Some(window) => {
                window.want_older();
                glib::ControlFlow::Break
            }
            None => glib::ControlFlow::Break,
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
        if !self.list.is_whole() {
            return;
        }
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
            .need(at_start, more && !self.desk.demo);
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
        if self.desk.demo || !self.window.is_active() || !self.list.at_end() {
            return;
        }
        let (store, id) = {
            let state = self.state.borrow();
            (state.store.clone(), state.open.clone())
        };
        let (Some(store), Some(id)) = (store, id) else {
            return;
        };
        let visible = self.own || !self.split.is_collapsed() || self.split.shows_content();
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
        self.state.borrow_mut().told = None;
        self.list.to_end();
        // It is in the outbox within moments.
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(80), move || {
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
                window.choose(id);
            }
        });
    }

    /// From the chats to the messages, to writing, and round again.
    fn next_area(self: &Rc<Self>) {
        let focus = gtk::prelude::RootExt::focus(&self.window);
        let within = |area: &gtk::Widget| focus.as_ref().is_some_and(|w| w.is_ancestor(area));
        let open = !self.is_free();
        if open && within(self.list.view.upcast_ref()) {
            self.composer.focus();
        } else if open && (self.own || within(self.sidebar.root.upcast_ref())) {
            self.split.set_show_content(true);
            self.list.focus_newest();
        } else if !self.own {
            self.split.set_show_content(false);
            self.sidebar.focus();
        }
    }

    fn show_keys(self: &Rc<Self>) {
        let dialog = adw::ShortcutsDialog::new();
        for (title, keys) in [
            (
                "Moving around",
                &[
                    ("Search", "<Ctrl>k"),
                    ("Next chat", "<Alt>Down"),
                    ("Previous chat", "<Alt>Up"),
                    ("Chats, messages, writing", "F6"),
                    ("Newest message", "<Ctrl>End"),
                    ("Back to the channel", "Escape"),
                    ("Open in its own window", "<Ctrl><Shift>o"),
                    ("Close window", "<Ctrl>w"),
                ][..],
            ),
            (
                "Messages",
                &[
                    ("React, reply, copy, edit, delete", "Return Menu <Shift>F10"),
                    ("Write a message", "<Ctrl>r"),
                    ("Send", "Return"),
                    ("New line", "<Shift>Return"),
                    ("Edit your last message", "Up"),
                ][..],
            ),
        ] {
            let section = adw::ShortcutsSection::new(Some(title));
            for (what, keys) in keys {
                section.add(adw::ShortcutsItem::new(what, keys));
            }
            dialog.add(section);
        }
        dialog.present(Some(&self.window));
    }

    fn step(self: &Rc<Self>, step: i64) {
        if let Some(id) = self.sidebar.neighbour(step) {
            self.choose(&id);
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
            glib::timeout_future(Duration::from_millis(20)).await;
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
    // Sharp on dense screens, too.
    snapshot.scale(2.0, 2.0);
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
