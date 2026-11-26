//! The list of chats and channels.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk4::glib::{self, BoxedAnyObject};
use gtk4::prelude::*;
use gtk4::{gdk, gio, graphene};
use libadwaita as adw;

use crate::art;
use crate::model::{ChatRow, Step, steps};
use crate::presence;

/// Scrolling has ended when nothing moved for this long.
const CALM: Duration = Duration::from_millis(250);

struct RowWidgets {
    root: gtk::Box,
    face: adw::Avatar,
    there: gtk::DrawingArea,
    title: gtk::Label,
    when: gtk::Label,
    preview: gtk::Label,
    draft: gtk::Label,
    dot: gtk::Box,
}

fn build() -> RowWidgets {
    let label = |class: &str| {
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        label.add_css_class(class);
        label
    };
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    root.add_css_class("chat");
    let face = adw::Avatar::new(40, None, true);
    let title = label("title");
    title.set_hexpand(true);
    let when = label("caption");
    when.add_css_class("dim-label");
    let preview = label("caption");
    preview.add_css_class("dim-label");
    preview.set_hexpand(true);
    let draft = gtk::Label::new(Some("Draft"));
    draft.add_css_class("caption");
    draft.add_css_class("draft");
    draft.set_visible(false);
    // On the line of the text beside it, not centred on it: it is bold.
    draft.set_valign(gtk::Align::BaselineFill);
    preview.set_valign(gtk::Align::BaselineFill);
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("unread-dot");
    dot.set_valign(gtk::Align::Center);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top.append(&title);
    top.append(&when);
    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bottom.append(&draft);
    bottom.append(&preview);
    bottom.append(&dot);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_valign(gtk::Align::Center);
    text.append(&top);
    text.append(&bottom);
    let there = presence::dot();
    root.append(&presence::with_dot(&face, &there));
    root.append(&text);
    RowWidgets {
        root,
        face,
        there,
        title,
        when,
        preview,
        draft,
        dot,
    }
}

fn bind(widgets: &RowWidgets, row: &ChatRow) {
    widgets.face.set_text(Some(&row.title));
    widgets
        .face
        .set_icon_name(row.team.is_some().then_some("view-list-bullet-symbolic"));
    widgets.face.set_show_initials(row.team.is_none());
    let face = row.person.as_deref().and_then(art::face);
    widgets.face.set_custom_image(face.as_ref());
    presence::set(&widgets.there, row.person.as_deref());
    widgets.title.set_text(&row.title);
    widgets.when.set_text(&row.when);
    widgets
        .preview
        .set_text(row.draft.as_deref().unwrap_or(&row.preview));
    widgets.draft.set_visible(row.draft.is_some());
    widgets.dot.set_visible(row.unread || row.mentions);
    let set = |class: &str, on: bool| {
        if on {
            widgets.root.add_css_class(class)
        } else {
            widgets.root.remove_css_class(class)
        }
    };
    set("unread", row.unread && !row.muted);
    set("muted", row.muted);
}

/// What a screen reader says for a row, with what the dot shows.
fn spoken(row: &ChatRow) -> String {
    let unread = if row.unread { ", unread" } else { "" };
    let mentions = if row.mentions { ", mentions you" } else { "" };
    let pinned = if row.pinned { ", pinned" } else { "" };
    let muted = if row.muted { ", muted" } else { "" };
    let there = row
        .person
        .as_deref()
        .and_then(presence::of)
        .map(|availability| format!(", {}", presence::words(availability).to_lowercase()))
        .unwrap_or_default();
    let draft = match &row.draft {
        Some(draft) => format!(" Draft: {draft}."),
        None => String::new(),
    };
    format!(
        "{}{there}{pinned}{unread}{mentions}{muted}.{draft} {} {}",
        row.title, row.preview, row.when
    )
}

/// What can be done with a chat, next to the pointer.
fn show_menu(row: &ChatRow, near: &gtk::Widget, x: f64, y: f64) {
    let menu = gio::Menu::new();
    let item = |label: &str, action: &str, target: glib::Variant| {
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(Some(action), Some(&target));
        menu.append_item(&item);
    };
    if row.team.is_none() {
        let label = if row.pinned { "Unpin" } else { "Pin" };
        item(label, "win.pin", (&row.id, !row.pinned).to_variant());
    }
    item("Open in its own window", "win.apart", row.id.to_variant());
    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);
    // The row may be another one by the time something is chosen. Within what scrolls, the arrow
    // keys would lead back to the list.
    let Some(list) = near
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_then(|scroll| scroll.parent())
    else {
        return;
    };
    let at = graphene::Point::new(x as f32, y as f32);
    let Some(at) = near.compute_point(&list, &at) else {
        return;
    };
    popover.set_parent(&list);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(
        at.x() as i32,
        at.y() as i32,
        1,
        1,
    )));
    let focus = list.root().and_then(|root| root.focus());
    popover.connect_closed(move |popover| {
        // The action runs after it closes.
        let (popover, focus) = (popover.clone(), focus.clone());
        glib::idle_add_local_once(move || {
            popover.unparent();
            if let Some(focus) = focus.filter(WidgetExt::is_mapped) {
                focus.grab_focus();
            }
        });
    });
    popover.popup();
}

type Chosen = Rc<RefCell<Option<Rc<dyn Fn(&str)>>>>;

#[derive(Clone)]
pub struct Sidebar {
    pub root: gtk::ScrolledWindow,
    selection: gtk::SingleSelection,
    view: gtk::ListView,
    items: gio::ListStore,
    rows: Rc<RefCell<Vec<ChatRow>>>,
    wanted: Rc<RefCell<Option<String>>>,
    quiet: Rc<std::cell::Cell<bool>>,
    /// When the user last scrolled, and what waits for that to end.
    moved: Rc<std::cell::Cell<Option<Instant>>>,
    later: Rc<RefCell<Option<Vec<ChatRow>>>>,
    /// Called for a row clicked twice.
    twice: Chosen,
    headings: gtk::SignalListItemFactory,
}

impl Sidebar {
    pub fn new(chosen: impl Fn(&str) + 'static) -> Sidebar {
        let items = gio::ListStore::new::<BoxedAnyObject>();
        // The pinned ones have a heading. They come first already: sorting keeps the rest in order.
        let apart = gtk::CustomSorter::new(|one, other| {
            let after = |row: &glib::Object| {
                row.downcast_ref::<BoxedAnyObject>()
                    .is_some_and(|row| !row.borrow::<ChatRow>().apart)
            };
            after(one).cmp(&after(other)).into()
        });
        let sections = gtk::SortListModel::builder()
            .model(&items)
            .section_sorter(&apart)
            .build();
        let selection = gtk::SingleSelection::builder()
            .model(&sections)
            .autoselect(false)
            .can_unselect(true)
            .build();
        let factory = gtk::SignalListItemFactory::new();
        let twice = Chosen::default();
        let clicked = twice.clone();
        let wanted: Rc<RefCell<Option<String>>> = Rc::default();
        let chosen: Rc<dyn Fn(&str)> = Rc::new(chosen);
        let (marked, again) = (wanted.clone(), chosen.clone());
        factory.connect_setup(move |_, cell| {
            if let Some(cell) = cell.downcast_ref::<gtk::ListItem>() {
                let widgets = build();
                let menu = gtk::GestureClick::builder()
                    .button(gdk::BUTTON_SECONDARY)
                    .build();
                let press = gtk::GestureLongPress::builder().touch_only(true).build();
                let show = {
                    let cell = cell.downgrade();
                    move |x: f64, y: f64| {
                        let Some(cell) = cell.upgrade() else { return };
                        let row = cell.item().and_downcast::<BoxedAnyObject>();
                        if let (Some(row), Some(widget)) = (row, cell.child()) {
                            show_menu(&row.borrow::<ChatRow>(), &widget, x, y);
                        }
                    }
                };
                menu.connect_pressed({
                    let show = show.clone();
                    move |menu, _, x, y| {
                        menu.set_state(gtk::EventSequenceState::Claimed);
                        show(x, y);
                    }
                });
                press.connect_pressed(move |_, x, y| show(x, y));
                widgets.root.add_controller(menu);
                widgets.root.add_controller(press);
                let click = gtk::GestureClick::new();
                click.connect_pressed({
                    let (cell, clicked) = (cell.downgrade(), clicked.clone());
                    move |_, count, _, _| {
                        let item = cell.upgrade().and_then(|cell| cell.item());
                        let row = item.and_downcast::<BoxedAnyObject>();
                        let run = clicked.borrow().clone();
                        if let (2, Some(row), Some(run)) = (count, row, run) {
                            let id = row.borrow::<ChatRow>().id.clone();
                            run(&id);
                        }
                    }
                });
                // The selection says nothing when it stays, but in a narrow window the row that is
                // selected already leads back to its conversation.
                let stays = Rc::new(std::cell::Cell::new(false));
                let id = {
                    let cell = cell.downgrade();
                    move || {
                        let item = cell.upgrade().and_then(|cell| cell.item());
                        let row = item.and_downcast::<BoxedAnyObject>()?;
                        let id = row.borrow::<ChatRow>().id.clone();
                        Some(id)
                    }
                };
                click.connect_pressed({
                    let (id, stays, marked) = (id.clone(), stays.clone(), marked.clone());
                    move |_, count, _, _| stays.set(count == 1 && *marked.borrow() == id())
                });
                click.connect_released({
                    let again = again.clone();
                    move |_, _, _, _| {
                        if let Some(id) = id().filter(|_| stays.take()) {
                            again(&id);
                        }
                    }
                });
                widgets.root.add_controller(click);
                cell.set_child(Some(&widgets.root));
                // The widgets live exactly as long as the cell.
                unsafe { cell.set_data("widgets", widgets) };
            }
        });
        factory.connect_bind(|_, cell| {
            let Some(cell) = cell.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(item) = cell.item().and_downcast::<BoxedAnyObject>() else {
                return;
            };
            let widgets = unsafe {
                cell.data::<RowWidgets>("widgets")
                    .map(|widgets| widgets.as_ref())
            };
            if let Some(widgets) = widgets {
                let row = item.borrow::<ChatRow>();
                bind(widgets, &row);
                cell.set_accessible_label(&spoken(&row));
            }
        });
        let view = gtk::ListView::builder()
            .model(&selection)
            .factory(&factory)
            .build();
        view.add_css_class("navigation-sidebar");
        view.add_css_class("chats");
        crate::row::say(&view, "Chats and channels");
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&view)
            .build();
        let headings = gtk::SignalListItemFactory::new();
        headings.connect_setup(|_, header| {
            if let Some(header) = header.downcast_ref::<gtk::ListHeader>() {
                let label = gtk::Label::builder().xalign(0.0).build();
                label.add_css_class("heading");
                label.add_css_class("dim-label");
                header.set_child(Some(&label));
            }
        });
        headings.connect_bind(|_, header| {
            let Some(header) = header.downcast_ref::<gtk::ListHeader>() else {
                return;
            };
            let row = header.item().and_downcast::<BoxedAnyObject>();
            if let (Some(row), Some(label)) = (row, header.child().and_downcast::<gtk::Label>()) {
                let pinned = row.borrow::<ChatRow>().apart;
                label.set_label(if pinned { "Pinned" } else { "Chats" });
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let (view, selection) = (view.downgrade(), selection.clone());
            move |_, key, _, held| {
                let asked = key == gdk::Key::Menu
                    || (key == gdk::Key::F10 && held.contains(gdk::ModifierType::SHIFT_MASK));
                let row = selection.selected_item().and_downcast::<BoxedAnyObject>();
                let (true, Some(row), Some(view)) = (asked, row, view.upgrade()) else {
                    return glib::Propagation::Proceed;
                };
                // Below the row that has the keyboard.
                let focus = view
                    .root()
                    .and_then(|root| root.focus())
                    .filter(|focus| focus.is_ancestor(&view))
                    .unwrap_or(view.upcast());
                let (x, y) = (40.0, f64::from(focus.height().min(56)));
                show_menu(&row.borrow::<ChatRow>(), &focus, x, y);
                glib::Propagation::Stop
            }
        });
        view.add_controller(keys);
        let quiet = Rc::new(std::cell::Cell::new(false));
        selection.connect_selected_item_notify({
            let (wanted, quiet) = (wanted.clone(), quiet.clone());
            move |selection| {
                if quiet.get() {
                    return;
                }
                let Some(item) = selection.selected_item().and_downcast::<BoxedAnyObject>() else {
                    return;
                };
                let id = item.borrow::<ChatRow>().id.clone();
                wanted.replace(Some(id.clone()));
                chosen(&id);
            }
        });
        let moved: Rc<std::cell::Cell<Option<Instant>>> = Rc::default();
        root.vadjustment().connect_value_changed({
            let (moved, quiet) = (moved.clone(), quiet.clone());
            move |_| {
                // Our own changes can move the value, too.
                if !quiet.get() {
                    moved.set(Some(Instant::now()));
                }
            }
        });
        Sidebar {
            root,
            selection,
            view,
            items,
            rows: Rc::default(),
            wanted,
            quiet,
            moved,
            later: Rc::default(),
            twice,
            headings,
        }
    }

    /// Whether a chat in the list is pinned.
    pub fn is_pinned(&self, id: &str) -> Option<bool> {
        let rows = self.rows.borrow();
        let row = rows.iter().find(|row| row.id == id)?;
        row.team.is_none().then_some(row.pinned)
    }

    /// Calls `run` for a row that is clicked twice.
    pub fn connect_twice(&self, run: impl Fn(&str) + 'static) {
        self.twice.replace(Some(Rc::new(run)));
    }

    /// Shows `rows`, changing as little as possible so the list keeps its place. Waits while the
    /// user scrolls: rows that move under the pointer are worse than rows a moment late.
    /// The rows as they are, or as they will be once scrolling ends.
    pub fn rows(&self) -> Vec<ChatRow> {
        match &*self.later.borrow() {
            Some(rows) => rows.clone(),
            None => self.rows.borrow().clone(),
        }
    }

    pub fn set(&self, rows: Vec<ChatRow>) {
        let waits = self.later.replace(Some(rows)).is_some();
        if self.scrolling() {
            if !waits {
                let sidebar = self.clone();
                glib::timeout_add_local(CALM / 2, move || {
                    if sidebar.scrolling() {
                        return glib::ControlFlow::Continue;
                    }
                    sidebar.apply();
                    glib::ControlFlow::Break
                });
            }
        } else {
            self.apply();
        }
    }

    fn scrolling(&self) -> bool {
        self.moved.get().is_some_and(|at| at.elapsed() < CALM)
    }

    fn apply(&self) {
        let Some(rows) = self.later.take() else {
            return;
        };
        self.quiet.set(true);
        let boxed = |row: &ChatRow| BoxedAnyObject::new(row.clone());
        for step in steps(&self.rows.borrow(), &rows, |row| row.id.as_str()) {
            match step {
                Step::Remove(at) => self.items.remove(at as u32),
                Step::Insert(at, from) => self.items.insert(at as u32, &boxed(&rows[from])),
                Step::Replace(at, from) => self.items.splice(at as u32, 1, &[boxed(&rows[from])]),
            }
        }
        // Where nothing is pinned there is nothing to tell apart.
        let headings = rows.iter().any(|row| row.apart).then_some(&self.headings);
        if self.view.header_factory().is_some() != headings.is_some() {
            self.view.set_header_factory(headings);
        }
        let people = rows.iter().filter_map(|row| row.person.clone()).collect();
        self.rows.replace(rows);
        self.mark();
        self.quiet.set(false);
        let sidebar = self.clone();
        art::load_faces(people, move |loaded| sidebar.refresh(loaded));
    }

    /// Draws the rows of these people again.
    fn refresh(&self, people: &[String]) {
        let rows = self.rows.borrow().clone();
        self.quiet.set(true);
        for (at, row) in rows.iter().enumerate() {
            if row
                .person
                .as_ref()
                .is_some_and(|person| people.contains(person))
            {
                let new = BoxedAnyObject::new(row.clone());
                self.items.splice(at as u32, 1, &[new]);
            }
        }
        self.mark();
        self.quiet.set(false);
    }

    /// Selects the open conversation again after the list changed.
    fn mark(&self) {
        let wanted = self.wanted.borrow();
        let at = self
            .rows
            .borrow()
            .iter()
            .position(|row| Some(&row.id) == wanted.as_ref())
            .map_or(gtk::INVALID_LIST_POSITION, |at| at as u32);
        if self.selection.selected() != at {
            self.selection.set_selected(at);
        }
    }

    /// Calls `run` when Enter is pressed on a row.
    pub fn connect_enter(&self, run: impl Fn() + 'static) {
        self.view.connect_activate(move |_, _| run());
    }

    /// Moves the keyboard focus to the open conversation's row.
    pub fn focus(&self) {
        let at = match self.selection.selected() {
            gtk::INVALID_LIST_POSITION => 0,
            at => at,
        };
        if at < self.items.n_items() {
            self.view.scroll_to(at, gtk::ListScrollFlags::FOCUS, None);
            self.view.grab_focus();
        }
    }

    /// The keyboard is in use in the list.
    pub fn has_keyboard(&self) -> bool {
        let Some(window) = self.root.root().and_downcast::<gtk::Window>() else {
            return false;
        };
        window.gets_focus_visible()
            && gtk::prelude::RootExt::focus(&window)
                .is_some_and(|focus| focus.is_ancestor(&self.view))
    }

    pub fn count(&self) -> u32 {
        self.items.n_items()
    }

    /// Shows which conversation is open, without opening it.
    pub fn select(&self, id: &str) {
        self.wanted.replace(Some(id.to_owned()));
        self.quiet.set(true);
        self.mark();
        self.quiet.set(false);
    }

    /// The conversation `step` away from the open one.
    pub fn neighbour(&self, step: i64) -> Option<String> {
        let rows = self.rows.borrow();
        let wanted = self.wanted.borrow();
        let at = rows.iter().position(|row| Some(&row.id) == wanted.as_ref());
        let next = match at {
            Some(at) => at as i64 + step,
            None => 0,
        };
        usize::try_from(next)
            .ok()
            .and_then(|next| rows.get(next))
            .map(|row| row.id.clone())
    }

    /// The people of 1:1 chats.
    pub fn people(&self) -> Vec<String> {
        let rows = self.rows.borrow();
        rows.iter().filter_map(|row| row.person.clone()).collect()
    }

    /// Looks for faces that were not there before.
    pub fn reload_faces(&self) {
        let people = self
            .rows
            .borrow()
            .iter()
            .filter_map(|row| row.person.clone())
            .collect();
        let sidebar = self.clone();
        art::load_faces(people, move |loaded| sidebar.refresh(loaded));
    }
}
