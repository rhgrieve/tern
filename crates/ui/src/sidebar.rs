//! The list of chats and channels.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib::{self, BoxedAnyObject};
use gtk4::prelude::*;
use libadwaita as adw;

use crate::art;
use crate::model::{ChatRow, Step, steps};

/// Scrolling has ended when nothing moved for this long.
const CALM: Duration = Duration::from_millis(250);

struct RowWidgets {
    root: gtk::Box,
    face: adw::Avatar,
    title: gtk::Label,
    when: gtk::Label,
    preview: gtk::Label,
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
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("unread-dot");
    dot.set_valign(gtk::Align::Center);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top.append(&title);
    top.append(&when);
    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bottom.append(&preview);
    bottom.append(&dot);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_valign(gtk::Align::Center);
    text.append(&top);
    text.append(&bottom);
    root.append(&face);
    root.append(&text);
    RowWidgets {
        root,
        face,
        title,
        when,
        preview,
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
    widgets.title.set_text(&row.title);
    widgets.when.set_text(&row.when);
    widgets.preview.set_text(&row.preview);
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
    let muted = if row.muted { ", muted" } else { "" };
    format!(
        "{}{unread}{mentions}{muted}. {} {}",
        row.title, row.preview, row.when
    )
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
}

impl Sidebar {
    pub fn new(chosen: impl Fn(&str) + 'static) -> Sidebar {
        let items = gio::ListStore::new::<BoxedAnyObject>();
        let selection = gtk::SingleSelection::builder()
            .model(&items)
            .autoselect(false)
            .can_unselect(true)
            .build();
        let factory = gtk::SignalListItemFactory::new();
        let twice = Chosen::default();
        let clicked = twice.clone();
        factory.connect_setup(move |_, cell| {
            if let Some(cell) = cell.downcast_ref::<gtk::ListItem>() {
                let widgets = build();
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
        crate::row::say(&view, "Chats and channels");
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&view)
            .build();
        let wanted: Rc<RefCell<Option<String>>> = Rc::default();
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
        }
    }

    /// Calls `run` for a row that is clicked twice.
    pub fn connect_twice(&self, run: impl Fn(&str) + 'static) {
        self.twice.replace(Some(Rc::new(run)));
    }

    /// Shows `rows`, changing as little as possible so the list keeps its place. Waits while the
    /// user scrolls: rows that move under the pointer are worse than rows a moment late.
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
