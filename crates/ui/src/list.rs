//! The message list: stays at the newest message, keeps its place when older ones are added.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk4::glib::BoxedAnyObject;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::row::{Item, MessageRow};

/// A change this soon after input is the user's doing.
const BY_HAND: Duration = Duration::from_millis(250);
/// This close to the end counts as being there.
const AT_END: f64 = 48.0;
/// The list builds every row it is given at once, seen or not. So a conversation opens with its
/// newest messages, enough to fill the window, and the others follow in portions.
const FIRST: usize = 30;
const PORTION: usize = 10;

mod imp {
    use super::*;
    use gtk4::subclass::prelude::*;
    use libadwaita::subclass::prelude::*;

    /// Tab leads to the row the list remembers, not to the oldest message.
    #[derive(Default)]
    pub struct Entrance;

    #[glib::object_subclass]
    impl ObjectSubclass for Entrance {
        const NAME: &'static str = "TernMessages";
        type Type = super::Entrance;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for Entrance {}
    impl BinImpl for Entrance {}
    impl WidgetImpl for Entrance {
        fn focus(&self, direction: gtk::DirectionType) -> bool {
            let tab = matches!(
                direction,
                gtk::DirectionType::TabForward | gtk::DirectionType::TabBackward
            );
            let view = self
                .obj()
                .child()
                .and_downcast::<gtk::Overlay>()
                .and_then(|over| over.child())
                .and_downcast::<gtk::ScrolledWindow>()
                .and_then(|scroll| scroll.child())
                .and_downcast::<gtk::ListView>();
            if let Some(view) = view.filter(|_| tab && self.obj().focus_child().is_none())
                && view.grab_focus()
            {
                return true;
            }
            self.parent_focus(direction)
        }
    }
}

glib::wrapper! {
    pub struct Entrance(ObjectSubclass<imp::Entrance>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

#[derive(Clone)]
pub struct MessageList {
    /// All of it, for the window.
    pub whole: adw::Bin,
    /// The list with what floats over it.
    pub over: gtk::Overlay,
    pub root: gtk::ScrolledWindow,
    pub view: gtk::ListView,
    pub items: gio::ListStore,
    /// The user is at the end and wants to stay there.
    follow: Rc<Cell<bool>>,
    shown: Rc<RefCell<Vec<Rc<Item>>>>,
    /// Older messages of the conversation just opened, still to be added.
    rest: Rc<RefCell<Vec<Rc<Item>>>>,
    /// Counts what was opened, so that portions of an earlier one are not added.
    opened: Rc<Cell<u64>>,
}

pub fn unboxed(object: &glib::Object) -> Option<Rc<Item>> {
    let object = object.downcast_ref::<BoxedAnyObject>()?;
    Some(object.borrow::<Rc<Item>>().clone())
}

fn boxed(items: &[Rc<Item>]) -> Vec<BoxedAnyObject> {
    items.iter().cloned().map(BoxedAnyObject::new).collect()
}

fn stick(place: &gtk::Adjustment) {
    place.set_value(place.upper() - place.page_size());
}

impl Default for MessageList {
    fn default() -> Self {
        let items = gio::ListStore::new::<BoxedAnyObject>();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, cell| {
            if let Some(cell) = cell.downcast_ref::<gtk::ListItem>() {
                cell.set_child(Some(&MessageRow::default()));
                cell.set_activatable(false);
                cell.set_selectable(false);
            }
        });
        factory.connect_bind(|_, cell| {
            let Some(cell) = cell.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let row = cell.child().and_downcast::<MessageRow>();
            let item = cell.item().and_then(|item| unboxed(&item));
            if let (Some(row), Some(item)) = (row, item) {
                row.bind(&item);
                cell.set_accessible_label(&item.spoken());
            }
        });
        let view = gtk::ListView::builder()
            .model(&gtk::NoSelection::new(Some(items.clone())))
            .factory(&factory)
            .build();
        view.add_css_class("messages");
        crate::row::say(&view, "Messages");
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&view)
            .build();
        let over = gtk::Overlay::builder().child(&root).vexpand(true).build();
        let whole: adw::Bin = glib::Object::new::<Entrance>().upcast();
        whole.set_child(Some(&over));
        let follow = Rc::new(Cell::new(true));
        let place = root.vadjustment();
        // Row heights become known as rows are shown, and messages grow (pictures, reactions):
        // whoever is at the end stays there.
        place.connect_changed({
            let follow = follow.clone();
            move |place| {
                if follow.get() {
                    stick(place);
                }
            }
        });
        // The list corrects its own position all the time, so numbers cannot tell that the user
        // moved: only their input can.
        let touched: Rc<Cell<Option<Instant>>> = Rc::default();
        let input = gtk::EventControllerLegacy::new();
        input.set_propagation_phase(gtk::PropagationPhase::Capture);
        input.connect_event({
            let touched = touched.clone();
            move |_, event| {
                use gtk::gdk::EventType;
                let moves = matches!(
                    event.event_type(),
                    EventType::Scroll
                        | EventType::TouchpadHold
                        | EventType::TouchBegin
                        | EventType::TouchUpdate
                        | EventType::KeyPress
                        | EventType::ButtonPress
                );
                // Dragging the scrollbar is motion with a button held.
                let drags = event.event_type() == EventType::MotionNotify
                    && event
                        .modifier_state()
                        .contains(gtk::gdk::ModifierType::BUTTON1_MASK);
                if moves || drags {
                    touched.set(Some(Instant::now()));
                }
                glib::Propagation::Proceed
            }
        });
        root.add_controller(input);
        place.connect_value_changed({
            let follow = follow.clone();
            move |place| {
                if place.value() + place.page_size() >= place.upper() - AT_END {
                    follow.set(true);
                } else if touched.get().is_some_and(|at| at.elapsed() < BY_HAND) {
                    follow.set(false);
                }
            }
        });
        MessageList {
            whole,
            over,
            root,
            view,
            items,
            follow,
            shown: Rc::default(),
            rest: Rc::default(),
            opened: Rc::default(),
        }
    }
}

impl MessageList {
    pub fn at_end(&self) -> bool {
        let place = self.root.vadjustment();
        place.value() + place.page_size() >= place.upper() - AT_END
    }

    /// Goes to the newest message and stays there until the user scrolls up.
    pub fn to_end(&self) {
        self.follow.set(true);
        if let Some(last) = self.items.n_items().checked_sub(1) {
            // Tab leads there, too: not to the oldest. A row that has the focus keeps it.
            let flags = if self.focused().is_some() {
                gtk::ListScrollFlags::NONE
            } else {
                gtk::ListScrollFlags::FOCUS
            };
            self.view.scroll_to(last, flags, None);
        }
        stick(&self.root.vadjustment());
    }

    /// Shows another conversation, at its end.
    pub fn set(&self, items: Vec<Item>) {
        let mut items: Vec<Rc<Item>> = items.into_iter().map(Rc::new).collect();
        let newest = items.split_off(items.len().saturating_sub(FIRST));
        self.opened.set(self.opened.get() + 1);
        self.rest.replace(items);
        self.follow.set(true);
        // In one step, so that the rows there are get used again.
        self.items.splice(0, self.items.n_items(), &boxed(&newest));
        self.shown.replace(newest);
        self.to_end();
        if !self.is_whole() {
            let (list, opened) = (self.clone(), self.opened.get());
            // Two frames later the first are on screen.
            glib::timeout_add_local_full(
                Duration::from_millis(34),
                glib::Priority::DEFAULT_IDLE,
                move || list.add_portion(opened),
            );
        }
    }

    fn add_portion(&self, opened: u64) -> glib::ControlFlow {
        if self.opened.get() != opened {
            return glib::ControlFlow::Break;
        }
        let portion = {
            let mut rest = self.rest.borrow_mut();
            let start = rest.len().saturating_sub(PORTION);
            rest.split_off(start)
        };
        if !portion.is_empty() {
            self.items.splice(0, 0, &boxed(&portion));
            self.shown.borrow_mut().splice(0..0, portion);
            if self.follow.get() {
                self.to_end();
            }
        }
        if self.is_whole() {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    }

    /// Every message of the conversation that was asked for is in the list.
    pub fn is_whole(&self) -> bool {
        self.rest.borrow().is_empty()
    }

    /// Shows the conversation as it is now, changing only what differs, so the list keeps its
    /// place. Whoever is at the end stays there.
    pub fn update(&self, items: Vec<Item>) {
        self.rest.borrow_mut().clear();
        let items: Vec<Rc<Item>> = items.into_iter().map(Rc::new).collect();
        let (start, removed, end) = crate::model::change(&self.shown.borrow(), &items);
        let new = boxed(&items[start..end]);
        if removed > 0 || !new.is_empty() {
            self.items.splice(start as u32, removed as u32, &new);
        }
        self.shown.replace(items);
        if self.follow.get() {
            self.to_end();
        }
    }

    /// Sets faces that were not ready when their rows were bound.
    pub fn show_faces(&self, people: &[String]) {
        let mut cell = self.view.first_child();
        while let Some(widget) = cell {
            if let Some(row) = widget.first_child().and_downcast::<MessageRow>() {
                row.show_face(people);
            }
            cell = widget.next_sibling();
        }
    }

    /// The newest own message that can be edited.
    pub fn last_editable(&self) -> Option<Rc<Item>> {
        self.shown
            .borrow()
            .iter()
            .rev()
            .find(|item| item.source.is_some() && item.sent)
            .cloned()
    }

    /// The row at a place in the list, and its top.
    pub fn row_at(&self, x: f64, y: f64) -> Option<(MessageRow, f64)> {
        let picked = self.root.pick(x, y, gtk::PickFlags::DEFAULT)?;
        let row = picked
            .ancestor(MessageRow::static_type())
            .and_downcast::<MessageRow>()
            .or_else(|| picked.first_child().and_downcast::<MessageRow>())?;
        let bounds = row.compute_bounds(&self.root)?;
        Some((row, f64::from(bounds.y())))
    }

    /// The row that has the keyboard focus, with the cell it is in.
    pub fn focused(&self) -> Option<(MessageRow, gtk::Widget)> {
        let focus = self.view.root()?.focus()?;
        let row = focus
            .ancestor(MessageRow::static_type())
            .and_downcast::<MessageRow>()
            .or_else(|| focus.first_child().and_downcast::<MessageRow>())?;
        let cell = row.parent()?;
        cell.is_ancestor(&self.view).then_some((row, cell))
    }

    /// Moves the keyboard focus to the newest message.
    pub fn focus_newest(&self) {
        if let Some(last) = self.items.n_items().checked_sub(1) {
            self.view.scroll_to(last, gtk::ListScrollFlags::FOCUS, None);
            // That only says which one; this gives the list the focus.
            self.view.grab_focus();
        }
    }

    /// The newest message.
    pub fn newest(&self) -> Option<Rc<Item>> {
        self.shown.borrow().last().cloned()
    }

    /// Goes to one message and stays there.
    pub fn jump(&self, position: u32) {
        self.follow.set(false);
        self.view
            .scroll_to(position, gtk::ListScrollFlags::NONE, None);
    }

    /// Stops following the newest message, as scrolling up does.
    pub fn leave_end(&self) {
        self.follow.set(false);
    }

    pub fn follows(&self) -> bool {
        self.follow.get()
    }

    pub fn at_start(&self) -> bool {
        self.root.vadjustment().value() < 400.0
    }

    /// Everything fits: there is nothing to scroll.
    pub fn is_short(&self) -> bool {
        let place = self.root.vadjustment();
        place.upper() <= place.page_size() + 1.0
    }
}
