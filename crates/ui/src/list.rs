//! The message list: stays at the newest message, keeps its place when older ones are added.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk4::glib::BoxedAnyObject;
use gtk4::prelude::*;
use gtk4::{gio, glib};

use crate::row::{Item, MessageRow};

/// A change this soon after input is the user's doing.
const BY_HAND: Duration = Duration::from_millis(250);
/// This close to the end counts as being there.
const AT_END: f64 = 48.0;

#[derive(Clone)]
pub struct MessageList {
    /// The list with what floats over it.
    pub over: gtk::Overlay,
    pub root: gtk::ScrolledWindow,
    pub view: gtk::ListView,
    pub items: gio::ListStore,
    /// The user is at the end and wants to stay there.
    follow: Rc<Cell<bool>>,
    shown: Rc<RefCell<Vec<Rc<Item>>>>,
}

pub fn unboxed(object: &glib::Object) -> Option<Rc<Item>> {
    let object = object.downcast_ref::<BoxedAnyObject>()?;
    Some(object.borrow::<Rc<Item>>().clone())
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
            }
        });
        let view = gtk::ListView::builder()
            .model(&gtk::NoSelection::new(Some(items.clone())))
            .factory(&factory)
            .build();
        view.add_css_class("messages");
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&view)
            .build();
        let over = gtk::Overlay::builder().child(&root).vexpand(true).build();
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
            over,
            root,
            view,
            items,
            follow,
            shown: Rc::default(),
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
            self.view.scroll_to(last, gtk::ListScrollFlags::NONE, None);
        }
        stick(&self.root.vadjustment());
    }

    /// Shows another conversation, at its end.
    pub fn set(&self, items: Vec<Item>) {
        self.shown.replace(Vec::new());
        self.items.remove_all();
        self.update(items);
        self.to_end();
    }

    /// Shows the conversation as it is now, changing only what differs, so the list keeps its
    /// place. Whoever is at the end stays there.
    pub fn update(&self, items: Vec<Item>) {
        let items: Vec<Rc<Item>> = items.into_iter().map(Rc::new).collect();
        let (start, removed, end) = crate::model::change(&self.shown.borrow(), &items);
        let new: Vec<BoxedAnyObject> = items[start..end]
            .iter()
            .cloned()
            .map(BoxedAnyObject::new)
            .collect();
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
