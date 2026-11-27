//! The main window.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
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
use crate::model::{self, ChatRow, Conversation, Filter, OUTGOING, Reacting};
use crate::row::{Item, say};
use crate::sidebar::Sidebar;
use crate::tools::{Tool, Tools};
use crate::viewer::Viewer;
use crate::{agenda, art, call, mention, palette, presence, service};

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
    /// The conversation whose pictures were asked for, and for how many messages.
    pictures: Option<(String, usize)>,
    /// Pictures that are here or were asked for.
    asked: HashSet<String>,
    /// Reactions the service does not know yet.
    reacting: Vec<Reacting>,
    /// Chats that are being pinned or unpinned.
    pinning: Vec<(String, bool)>,
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
    agenda: agenda::View,
    back: gtk::Button,
    writing: gtk::Label,
    desk: Rc<Desk>,
    /// Shows one conversation, without the list of chats.
    own: bool,
    out: gtk::Button,
    /// Calls the other person of a 1:1 chat.
    call_button: gtk::Button,
    call_bar: Rc<call::Bar>,
    /// The call above the chats, when they are all there is to see.
    side_call: Rc<call::Bar>,
    /// Whom the call button calls.
    callable: RefCell<Option<model::Callable>>,
    /// Shown while older messages are fetched.
    loading: gtk::Box,
    tools: RefCell<Option<Rc<Tools>>>,
    viewer: RefCell<Option<Rc<Viewer>>>,
    /// Whose presence was asked for.
    watched: RefCell<HashSet<String>>,
    /// What the daemon was told is in front of the user.
    viewing: RefCell<Option<String>>,
    /// The daemon is in touch with Teams.
    connected: Cell<bool>,
    client: Client,
    state: RefCell<State>,
    /// The calendar is shown instead of a conversation.
    calendar: Cell<bool>,
    /// Counts reads, so that a slow one does not replace a newer.
    chats_turn: Cell<u64>,
    open_turn: Cell<u64>,
}

/// Someone who stopped without saying so is taken off after this.
const TYPING: Duration = Duration::from_secs(12);
/// How often the others are told.
const TELL: Duration = Duration::from_secs(8);

/// And as many until the presence of others is read again, in case it is not pushed.
const PRESENCE: u32 = 60;
/// Every so many ticks of two seconds it is asked who has read.
const RECEIPTS: u32 = 10;

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
            sidebar.connect_enter(later(|window| {
                if !window.is_free() {
                    window.split.set_show_content(true);
                    window.composer.focus();
                }
            }));
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
            let calendar = gtk::Button::builder()
                .icon_name("x-office-calendar-symbolic")
                .tooltip_text("Calendar (Ctrl+E)")
                .build();
            say(&calendar, "Calendar");
            calendar.connect_clicked({
                let calendar = later(Window::calendar);
                move |_| calendar()
            });
            side_header.pack_start(&calendar);
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
            let side_call = call::Bar::new();
            side.add_top_bar(&side_call.root);
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
            let agenda = agenda::View::default();
            pages.add_named(&agenda.root, Some("calendar"));
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
            let call_button = gtk::Button::builder()
                .icon_name("call-start-symbolic")
                .visible(false)
                .build();
            call_button.connect_clicked({
                let call = later(Window::call);
                move |_| call()
            });
            header.pack_end(&call_button);
            let call_bar = call::Bar::new();
            let banner = adw::Banner::new("");
            let main = adw::ToolbarView::builder().content(&pages).build();
            main.add_top_bar(&header);
            main.add_top_bar(&call_bar.root);
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
            // Side by side, the call is above the conversation already.
            split
                .bind_property("collapsed", &side_call.root, "visible")
                .sync_create()
                .build();
            // Back at the list of a narrow window, what was being written is a draft.
            for property in ["show-content", "collapsed"] {
                let drafts = later(Window::drafts_changed);
                split.connect_notify_local(Some(property), move |_, _| drafts());
            }
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
            let keys: [(&str, &[&str], Run); 14] = [
                ("leave", &["Escape"], Window::escape),
                ("search", &["<Ctrl>k", "<Ctrl>f"], Window::search),
                ("calendar", &["<Ctrl>e"], Window::calendar),
                ("next", &["<Alt>Down"], |window| window.step(1)),
                ("previous", &["<Alt>Up"], |window| window.step(-1)),
                ("write", &["<Ctrl>r"], |window| window.composer.focus()),
                ("elsewhere", &["<Ctrl><Shift>e"], |window| {
                    window.composer.write_elsewhere()
                }),
                ("newest", &["<Ctrl>End"], |window| window.list.to_end()),
                ("out", &["<Ctrl><Shift>o"], Window::pop_out),
                ("pin-open", &["<Ctrl><Shift>p"], Window::pin_open),
                ("area", &["F6"], Window::next_area),
                ("keys", &["<Ctrl>question"], Window::show_keys),
                ("log", &["<Ctrl><Shift>l"], Window::open_log),
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
            let pin = gio::SimpleAction::new("pin", Some(&<(String, bool)>::static_variant_type()));
            pin.connect_activate({
                let weak = weak.clone();
                move |_, what| {
                    let what = what.and_then(|what| what.get::<(String, bool)>());
                    if let (Some(window), Some((id, on))) = (weak.upgrade(), what) {
                        window.pin(&id, on);
                    }
                }
            });
            window.add_action(&pin);
            let show = gio::SimpleAction::new("show", Some(glib::VariantTy::STRING));
            show.connect_activate({
                let weak = weak.clone();
                move |_, id| {
                    if let (Some(window), Some(id)) =
                        (weak.upgrade(), id.and_then(|id| id.get::<String>()))
                    {
                        window.choose(&id);
                    }
                }
            });
            window.add_action(&show);
            let apart = gio::SimpleAction::new("apart", Some(glib::VariantTy::STRING));
            apart.connect_activate({
                let weak = weak.clone();
                move |_, id| {
                    if let (Some(window), Some(id)) =
                        (weak.upgrade(), id.and_then(|id| id.get::<String>()))
                    {
                        window.desk.pop_out(&id);
                    }
                }
            });
            window.add_action(&apart);
            let picture = gio::SimpleAction::new("picture", Some(glib::VariantTy::STRING));
            picture.connect_activate({
                let weak = weak.clone();
                move |_, url| {
                    if let (Some(window), Some(url)) =
                        (weak.upgrade(), url.and_then(|url| url.get::<String>()))
                    {
                        window.view_picture(&url);
                    }
                }
            });
            window.add_action(&picture);
            window.connect_close_request({
                let closed = later(Window::closed);
                move |_| {
                    closed();
                    glib::Propagation::Stop
                }
            });
            glib::timeout_add_local(Duration::from_secs(2), {
                let (weak, ticks) = (weak.clone(), Cell::new(0u32));
                move || match weak.upgrade() {
                    Some(window) => {
                        window.save_drafts();
                        window.show_drafts();
                        window.show_typing();
                        ticks.set(ticks.get() + 1);
                        if ticks.get().is_multiple_of(RECEIPTS) {
                            window.ask_receipts();
                            window.tell_viewing();
                        }
                        if ticks.get().is_multiple_of(RECEIPTS) {
                            // Meetings begin and end.
                            window.read_agenda();
                        }
                        if ticks.get().is_multiple_of(PRESENCE) && window.window.is_active() {
                            window.ask_presence(true);
                        }
                        glib::ControlFlow::Continue
                    }
                    None => glib::ControlFlow::Break,
                }
            });
            composer.connect_trouble({
                let weak = weak.clone();
                move |text| {
                    if let Some(window) = weak.upgrade() {
                        window.toast(text);
                    }
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
                let (seen, viewing) = (later(Window::seen), later(Window::tell_viewing));
                let giphy = later(Window::want_giphy);
                move |_| {
                    seen();
                    giphy();
                    viewing();
                }
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
                agenda,
                back,
                writing,
                desk: desk.clone(),
                own,
                out,
                call_button,
                call_bar,
                side_call,
                callable: RefCell::default(),
                loading,
                tools: RefCell::default(),
                viewer: RefCell::default(),
                watched: RefCell::default(),
                viewing: RefCell::default(),
                connected: Cell::new(false),
                client,
                state: RefCell::default(),
                calendar: Cell::new(false),
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
                // As it is by now: the bar may still have it from before the last click.
                let now = self.list.item(&item.id);
                let mine = now
                    .as_ref()
                    .unwrap_or(item)
                    .reactions
                    .iter()
                    .any(|reaction| reaction.mine && reaction.emoji == model::in_colour(&emoji));
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

    /// Marks the chats that have something written in them, but the one being written in here.
    fn mark_drafts(&self, rows: &mut [ChatRow]) {
        let lines: HashMap<String, String> = self
            .desk
            .drafts
            .borrow()
            .iter()
            .filter_map(|(key, written)| Some((key.clone(), written.line()?)))
            .collect();
        // Behind the list of a narrow window, the open one is not being written in.
        let writing = self.composer.opened().filter(|_| self.is_reading());
        model::mark_drafts(rows, &lines, writing.as_deref());
    }

    /// The draft of the open conversation may have come into view, or gone out of it.
    fn drafts_changed(self: &Rc<Self>) {
        self.save_drafts();
        self.show_drafts();
    }

    /// Shows in the list of chats the drafts as they are now.
    fn show_drafts(&self) {
        if self.own {
            return;
        }
        let before = self.sidebar.rows();
        let mut rows = before.clone();
        self.mark_drafts(&mut rows);
        if rows != before {
            self.sidebar.set(rows);
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

    pub fn toast(&self, text: &str) {
        self.toasts
            .add_toast(adw::Toast::new(&glib::markup_escape_text(text)));
    }

    fn calendar(self: &Rc<Self>) {
        self.desk.show_calendar();
    }

    /// Whom the call button calls, if anyone.
    fn set_callable(&self, who: Option<model::Callable>) {
        if let Some(who) = &who {
            let tip = format!("Call {}", who.name);
            self.call_button.set_tooltip_text(Some(&tip));
            say(&self.call_button, &tip);
        }
        self.call_button
            .set_visible(who.is_some() && !self.desk.demo);
        self.call_button
            .set_sensitive(self.desk.call_on().is_none());
        self.callable.replace(who);
    }

    fn call(self: &Rc<Self>) {
        let who = self.callable.borrow().clone();
        if let Some(who) = who {
            self.desk.call(&who, self);
        }
    }

    /// Shows a call with someone in this chat, connected for a while, without calling: for
    /// trying things out.
    pub fn pretend_call(&self) {
        let name = self.title.title().to_string();
        self.set_callable(Some(model::Callable {
            thread: String::new(),
            name: name.clone(),
            people: Vec::new(),
        }));
        self.call_button.set_visible(true);
        // A screenshot may come before any animation runs.
        let people = ["Ada Quill", "Bo Lindqvist", "Cyrus Amadi", "Robin Vale"];
        for bar in [&self.call_bar, &self.side_call] {
            bar.root
                .set_transition_type(gtk::RevealerTransitionType::None);
            bar.draw(Some(call::Shown {
                name: name.clone(),
                stage: call::Stage::Connected(Instant::now() - Duration::from_secs(754)),
                muted: true,
                waiting: false,
                people: people.map(str::to_owned).to_vec(),
            }));
        }
    }

    /// Shows the call that is on, if any.
    pub fn show_call(&self, call: Option<&Rc<call::Call>>) {
        self.call_bar.show(call);
        self.side_call.show(call);
        self.call_button.set_sensitive(call.is_none());
    }

    /// Shows the calendar instead of a conversation.
    pub fn show_calendar(self: &Rc<Self>) {
        if !self.calendar.get() {
            self.leave();
            self.calendar.set(true);
            self.set_titles("Calendar", "");
            self.pages.set_visible_child_name("calendar");
            self.read_agenda();
            if !self.desk.demo {
                self.client.ask(Request::Agenda);
            }
        }
        self.split.set_show_content(true);
    }

    fn read_agenda(self: &Rc<Self>) {
        let store = self.state.borrow().store.clone();
        let Some(store) = store.filter(|_| self.calendar.get()) else {
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let agenda = gio::spawn_blocking(move || tern_engine::agenda(&store)).await;
            if let (Some(window), Ok(agenda)) = (weak.upgrade(), agenda) {
                window.agenda.set(agenda.as_ref());
            }
        });
    }

    /// Shows no conversation.
    pub fn leave(self: &Rc<Self>) {
        self.calendar.set(false);
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
        self.set_callable(None);
        self.pages.set_visible_child_name("nothing");
        self.set_titles("", "");
        if !self.own {
            self.split.set_show_content(false);
        }
        self.sidebar.select("");
        self.show_drafts();
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
        {
            let mut state = self.state.borrow_mut();
            state
                .reacting
                .retain(|other| other.message != message || other.emoji != emoji);
            state.reacting.push(Reacting {
                conversation: conversation.clone(),
                message: message.to_owned(),
                emoji: emoji.to_owned(),
                on,
                taken: None,
            });
        }
        self.list
            .change(message, |item| model::react(&mut item.reactions, emoji, on));
        self.client.ask(Request::React {
            conversation,
            message: message.to_owned(),
            emoji: emoji.to_owned(),
            on,
        });
    }

    fn reacted(self: &Rc<Self>, message: &str, emoji: &str, on: bool, refused: Option<String>) {
        let same =
            |other: &Reacting| other.message == message && other.emoji == emoji && other.on == on;
        match refused {
            Some(reason) => {
                self.state
                    .borrow_mut()
                    .reacting
                    .retain(|other| !same(other));
                self.toasts
                    .add_toast(adw::Toast::new(&glib::markup_escape_text(&reason)));
            }
            None => {
                let mut state = self.state.borrow_mut();
                if let Some(reacting) = state.reacting.iter_mut().find(|other| same(other)) {
                    reacting.taken = Some(Instant::now());
                }
                drop(state);
                // If the cache still disagrees by then, it is right.
                let weak = Rc::downgrade(self);
                glib::timeout_add_local_once(model::BELIEVE + Duration::from_secs(1), move || {
                    if let Some(window) = weak.upgrade()
                        && !window.state.borrow().reacting.is_empty()
                    {
                        window.read_open();
                    }
                });
            }
        }
        self.read_open();
    }

    /// The daemon does not notify about what the user is looking at.
    fn tell_viewing(self: &Rc<Self>) {
        if self.desk.demo {
            return;
        }
        let open = self.state.borrow().open.clone();
        let now = open.filter(|_| self.window.is_active() && self.is_reading());
        let before = self.viewing.replace(now.clone());
        if let Some(conversation) = before.filter(|before| Some(before) != now.as_ref()) {
            self.client.ask(Request::Viewing {
                conversation,
                on: false,
            });
        }
        if let Some(conversation) = now {
            self.client.ask(Request::Viewing {
                conversation,
                on: true,
            });
        }
    }

    /// Asks whether the people shown are there; `again` also for those that were asked for.
    fn ask_presence(self: &Rc<Self>, again: bool) {
        if self.desk.demo {
            return;
        }
        let mut people = self.sidebar.people();
        people.extend(self.list.people());
        people.retain(|person| presence::is_person(person));
        people.sort();
        people.dedup();
        let mut asked = self.watched.borrow_mut();
        if !again {
            people.retain(|person| !asked.contains(person));
        }
        if !people.is_empty() {
            asked.extend(people.iter().cloned());
            self.client.ask(Request::Presence { people });
        }
    }

    fn forget_presence(&self) {
        // Asked for again when there is a connection.
        self.watched.borrow_mut().clear();
        if presence::forget() {
            self.show_presence();
        }
    }

    fn show_presence(&self) {
        presence::refresh(self.sidebar.root.upcast_ref());
        presence::refresh(self.list.view.upcast_ref());
    }

    /// Mostly the service tells by itself that somebody read a message.
    fn ask_receipts(self: &Rc<Self>) {
        let open = {
            let state = self.state.borrow();
            let chat = !state.channel && state.thread.is_none();
            state.open.clone().filter(|_| chat)
        };
        if let Some(conversation) = open
            && !self.desk.demo
            && self.window.is_active()
            && self.list.waits_for_readers()
        {
            self.client.ask(Request::Receipts { conversation });
        }
    }

    /// Shows a picture on its own, and asks for it in full.
    pub fn view_picture(self: &Rc<Self>, url: &str) {
        if !art::has_picture(url) {
            return self.find_picture(url);
        }
        let viewer = Viewer::show(&self.window, url);
        let full = tern_engine::full_size(url);
        if let Some(full) = full {
            if art::file(&full).is_some() {
                viewer.load(&full);
            } else if !self.desk.demo {
                self.client.ask(Request::Picture { url: full });
            }
        }
        self.viewer.replace(Some(viewer));
    }

    /// A picture that is not here: asks for it again, or opens it where it is.
    fn find_picture(self: &Rc<Self>, url: &str) {
        if tern_engine::may_show(url) {
            // Already on its way.
            if art::wait_for(url).is_none() && !self.desk.demo {
                art::wait_again(url);
                self.list.show_pictures();
                self.client.ask(Request::Picture { url: url.into() });
            }
        } else if crate::rich::may_open(url) {
            let toasts = self.toasts.clone();
            gtk::UriLauncher::new(url).launch(
                Some(&self.window),
                gio::Cancellable::NONE,
                move |opened| {
                    if let Err(error) = opened {
                        let text = format!("The picture could not be opened: {error}");
                        toasts.add_toast(adw::Toast::new(&glib::markup_escape_text(&text)));
                    }
                },
            );
        }
    }

    /// Asks for pictures from elsewhere, only while the user looks at the conversation: who has
    /// them is not to learn when a message arrives.
    fn want_giphy(self: &Rc<Self>) {
        if self.desk.demo || !self.window.is_active() || !self.is_reading() {
            return;
        }
        let mut wanted = false;
        for url in self.list.giphy() {
            let new = self.state.borrow_mut().asked.insert(url.clone());
            if new && !art::has_picture(&url) {
                art::wait_again(&url);
                self.client.ask(Request::Picture { url });
                wanted = true;
            }
        }
        if wanted {
            self.list.show_pictures();
        }
    }

    /// Asks for pictures of what is shown that nobody asked for yet.
    fn want_pictures(self: &Rc<Self>, items: &[Item]) {
        if self.desk.demo {
            return;
        }
        let mut state = self.state.borrow_mut();
        let mut missing = false;
        for url in model::pictures(items) {
            if tern_engine::is_giphy(url) {
                continue;
            }
            if !state.asked.contains(url) {
                state.asked.insert(url.to_owned());
                missing |= !art::has_picture(url);
            }
        }
        if let (true, Some(conversation)) = (missing, state.open.clone()) {
            self.client.ask(Request::Pictures {
                conversation,
                messages: state.history.limit,
            });
        }
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

    /// Closes what is in front, else the thread.
    fn escape(self: &Rc<Self>) {
        match self.window.visible_dialog() {
            Some(dialog) => {
                dialog.close();
            }
            None => self.leave_thread(),
        }
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
        self.show_drafts();
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
            News::Event(Event::Agenda) => self.read_agenda(),
            News::Event(Event::Pictures) => self.list.show_pictures(),
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
                self.connected.set(false);
                self.forget_presence();
                self.tell("The background service is not running. You can read, but not send.")
            }
            News::Unsent { request, reason } => {
                self.toasts
                    .add_toast(adw::Toast::new(&glib::markup_escape_text(&reason)));
                self.not_taken(*request);
            }
            News::Event(Event::Presence { people }) | News::Presence(people) => {
                if presence::update(&people) {
                    self.show_presence();
                }
            }
            News::Picture { url, here } => {
                if !here {
                    art::give_up(&url);
                }
                self.list.show_pictures();
                if let Some(viewer) = self.viewer.borrow().as_ref() {
                    viewer.load(&url);
                }
            }
            News::Reacted {
                message,
                emoji,
                on,
                refused,
            } => self.reacted(&message, &emoji, on, refused),
            News::Pinned {
                conversation,
                on,
                refused,
            } => self.pinned(&conversation, on, refused),
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
        self.connected.set(status.push == "connected");
        if self.connected.get() {
            self.ask_presence(false);
        } else {
            self.forget_presence();
        }
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
        // Some may have come while nobody told.
        self.list.show_pictures();
        // An outgoing message may have gone out.
        self.read_open();
    }

    fn read_chats(self: &Rc<Self>) {
        if self.own {
            return;
        }
        let (store, filter, pinning) = {
            let state = self.state.borrow();
            (state.store.clone(), state.filter, state.pinning.clone())
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
                let mut chats = store.recent_chats(CHATS).unwrap_or_default();
                let pinned = model::pins(tern_engine::pinned(&store), &pinning);
                // However long nobody wrote in them.
                for id in &pinned {
                    if !chats.iter().any(|chat| chat.id == *id) {
                        chats.extend(store.chat(id).ok().flatten());
                    }
                }
                model::chat_rows(&chats, &pinned, filter, me.as_deref(), now)
            })
            .await;
            if let (Some(window), Ok(mut rows)) = (weak.upgrade(), rows)
                && window.chats_turn.get() == turn
            {
                window.mark_drafts(&mut rows);
                window.sidebar.set(rows);
                if window.connected.get() {
                    window.ask_presence(false);
                }
            }
        });
    }

    fn pin(self: &Rc<Self>, id: &str, on: bool) {
        {
            let mut state = self.state.borrow_mut();
            state.pinning.retain(|(other, _)| other != id);
            state.pinning.push((id.to_owned(), on));
        }
        self.read_chats();
        self.client.ask(Request::Pin {
            conversation: id.to_owned(),
            on,
        });
    }

    fn pinned(self: &Rc<Self>, id: &str, on: bool, refused: Option<String>) {
        // Unless it was changed again meanwhile.
        self.state
            .borrow_mut()
            .pinning
            .retain(|pinning| *pinning != (id.to_owned(), on));
        if let Some(reason) = refused {
            let what = if on { "pinned" } else { "unpinned" };
            let text = glib::markup_escape_text(&format!("The chat was not {what}: {reason}"));
            self.toasts.add_toast(adw::Toast::new(&text));
        }
        self.read_chats();
    }

    /// Pins the open chat, or unpins it.
    fn pin_open(self: &Rc<Self>) {
        let open = self.state.borrow().open.clone();
        if let Some(on) = open.as_deref().and_then(|id| self.sidebar.is_pinned(id)) {
            self.pin(&open.unwrap_or_default(), !on);
        }
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
        self.calendar.set(false);
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
        let looked = (id.clone(), limit);
        if !self.desk.demo && self.state.borrow().pictures.as_ref() != Some(&looked) {
            self.client.ask(Request::Pictures {
                conversation: id.clone(),
                messages: limit,
            });
            self.state.borrow_mut().pictures = Some(looked);
        }
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

    fn show(self: &Rc<Self>, mut conversation: Conversation, fresh: bool) {
        {
            let mut state = self.state.borrow_mut();
            let open = state.open.clone().unwrap_or_default();
            model::apply_reacting(&mut conversation.items, &open, &mut state.reacting);
        }
        self.want_pictures(&conversation.items);
        self.set_titles(&conversation.title, &conversation.subtitle);
        self.set_callable(conversation.call.clone());
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
        self.want_giphy();
        if fresh {
            self.tell_viewing();
        }
        if self.connected.get() {
            self.ask_presence(false);
        }
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
        // Not behind what is in front, nor twice.
        if self.window.visible_dialog().is_some() {
            return;
        }
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

    /// Tells the service's log how a call goes: never who is in it.
    pub fn log_call(&self, line: &str) {
        self.client.ask(Request::CallSaid { line: line.into() });
    }

    /// Opens what the service wrote lately, for when something went wrong.
    fn open_log(self: &Rc<Self>) {
        let window = self.clone();
        glib::spawn_future_local(async move {
            if let Err(reason) = crate::log::open(&window.window).await {
                window.toast(&reason);
            }
        });
    }

    fn show_keys(self: &Rc<Self>) {
        let dialog = adw::ShortcutsDialog::new();
        for (title, keys) in [
            (
                "Moving around",
                &[
                    ("Search", "<Ctrl>k"),
                    ("Calendar", "<Ctrl>e"),
                    ("Next chat", "<Alt>Down"),
                    ("Previous chat", "<Alt>Up"),
                    ("Chats, messages, writing", "F6"),
                    ("Newest message", "<Ctrl>End"),
                    ("Back to the channel", "Escape"),
                    ("Open in its own window", "<Ctrl><Shift>o"),
                    ("Pin the chat, or unpin it", "<Ctrl><Shift>p"),
                    ("More for a chat in the list", "Menu <Shift>F10"),
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
            if title == "Moving around" && crate::log::kept() {
                section.add(adw::ShortcutsItem::new(
                    "Open the service's log",
                    "<Ctrl><Shift>l",
                ));
            }
            if title == "Messages" {
                section.add(adw::ShortcutsItem::new(
                    "Write in your editor",
                    "<Ctrl><Shift>e",
                ));
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
