//! The message list: stays at the newest message, keeps its place when older ones are added.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk4::glib::BoxedAnyObject;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::mark::Progress;
use crate::row::{Item, MessageRow};

/// A change this soon after input is the user's doing.
const BY_HAND: Duration = Duration::from_millis(250);
/// This close to the end counts as being there.
const AT_END: f64 = 48.0;
/// The list builds every row it is given at once, seen or not. So a conversation opens with its
/// newest messages, enough to fill the window, and the others follow in portions.
const FIRST: usize = 30;
const PORTION: usize = 10;
/// Further back no message is the newest of somebody.
const MARKED: usize = 400;

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
            let (follow, waits) = (follow.clone(), Rc::new(Cell::new(false)));
            move |place| {
                let left = place.upper() - place.page_size() - place.value();
                let by_hand = touched.get().is_some_and(|at| at.elapsed() < BY_HAND);
                // The list puts itself elsewhere when its rows change, by its own idea of where it
                // was. Not while it is being laid out.
                if follow.get() && !by_hand && left > 0.5 && !waits.replace(true) {
                    let (place, follow, waits) = (place.clone(), follow.clone(), waits.clone());
                    glib::idle_add_local_once(move || {
                        waits.set(false);
                        if follow.get() {
                            stick(&place);
                        }
                    });
                }
                if left <= AT_END {
                    follow.set(true);
                } else if by_hand {
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
        self.mark(&items);
        let (start, removed, end) = crate::model::change(&self.shown.borrow(), &items);
        let new = boxed(&items[start..end]);
        if removed > 0 || !new.is_empty() {
            self.items.splice(start as u32, removed as u32, &new);
        }
        self.shown.replace(items);
        // Where nothing came or went, there is nowhere to go.
        if self.follow.get() && (removed > 0 || !new.is_empty()) {
            self.to_end();
        }
    }

    /// Shows how far messages got where nothing else changed, without touching their rows
    /// otherwise: rows that are replaced move the list.
    fn mark(&self, items: &[Rc<Item>]) {
        let mut shown = self.shown.borrow_mut();
        for item in items.iter().rev().take(MARKED) {
            let Some(at) = shown.iter().rposition(|old| old.id == item.id) else {
                continue;
            };
            let old = &shown[at];
            if old.progress == item.progress {
                continue;
            }
            let same = Item {
                progress: item.progress.clone(),
                ..Item::clone(old)
            };
            if same != **item {
                continue;
            }
            shown[at] = item.clone();
            if let Some(object) = self.items.item(at as u32).and_downcast::<BoxedAnyObject>() {
                object.replace(item.clone());
            }
            let mut cell = self.view.first_child();
            while let Some(widget) = cell {
                if let Some(row) = widget.first_child().and_downcast::<MessageRow>()
                    && row.item().is_some_and(|bound| bound.id == item.id)
                {
                    row.mark(item);
                }
                cell = widget.next_sibling();
            }
        }
    }

    /// Those who wrote what is shown.
    pub fn people(&self) -> Vec<String> {
        let shown = self.shown.borrow();
        shown
            .iter()
            .filter_map(|item| item.sender_id.clone())
            .collect()
    }

    /// An own message is marked as not read by everyone yet.
    pub fn waits_for_readers(&self) -> bool {
        self.shown.borrow().iter().rev().any(|item| {
            item.progress
                .as_ref()
                .is_some_and(|progress| *progress != Progress::Sending && !progress.is_read())
        })
    }

    /// The message as it is shown now.
    pub fn item(&self, id: &str) -> Option<Rc<Item>> {
        self.shown
            .borrow()
            .iter()
            .find(|item| item.id == id)
            .cloned()
    }

    /// Changes one message.
    pub fn change(&self, id: &str, change: impl FnOnce(&mut Item)) {
        let mut shown = self.shown.borrow_mut();
        let Some(at) = shown.iter().position(|item| item.id == id) else {
            return;
        };
        let mut item = Item::clone(&shown[at]);
        change(&mut item);
        shown[at] = Rc::new(item);
        let new = boxed(&shown[at..=at]);
        drop(shown);
        self.items.splice(at as u32, 1, &new);
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

    /// Looks again for pictures that were not there when their rows were bound.
    pub fn show_pictures(&self) {
        let mut cell = self.view.first_child();
        while let Some(widget) = cell {
            if let Some(row) = widget.first_child().and_downcast::<MessageRow>() {
                row.show_pictures();
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

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::card::{Card, Piece};
    use crate::rich::Block;

    /// As the composer's test: needs a display.
    #[test]
    #[ignore = "needs a display"]
    fn pictures_are_shown_when_they_come() {
        gtk::init().unwrap();
        adw::init().unwrap();
        crate::window::load_style();
        let folder = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(crate::demo::store());
        crate::art::set_source(store, Some(folder.path().to_owned()), 1);
        let url = |name: &str| format!("https://us-api.asm.skype.com/v1/objects/{name}/views/imgo");
        let plain = |id: usize| Item {
            id: id.to_string(),
            sender: "Ann".into(),
            text: "text".into(),
            blocks: vec![Block::Text("text".into())],
            sent: true,
            ..Default::default()
        };
        let emoji = tern_engine::custom_emoji("party;0-eus-d1-abc").unwrap();
        let mut items: Vec<Item> = (0..30).map(plain).collect();
        items.push(Item {
            blocks: vec![
                Block::Text("look".into()),
                Block::Picture(url("a")),
                Block::Quote(vec![Block::Picture(url("b"))]),
                Block::Card(Box::new(Card {
                    pieces: vec![Piece::Picture {
                        url: url("c"),
                        about: String::new(),
                    }],
                    ..Default::default()
                })),
            ],
            reactions: vec![crate::row::Reaction {
                emoji: "party;0-eus-d1-abc".into(),
                custom: Some(emoji.clone()),
                count: 2,
                mine: false,
            }],
            ..plain(99)
        });
        let list = MessageList::default();
        let window = gtk::Window::builder()
            .default_width(600)
            .default_height(500)
            .child(&list.whole)
            .build();
        window.present();
        let wait = |milliseconds: u64| {
            let end = Instant::now() + Duration::from_millis(milliseconds);
            while Instant::now() < end {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        list.set(items);
        wait(500);
        // Counts pictures on screen and what stands in for them.
        fn count(within: &gtk::Widget) -> (usize, usize) {
            let mut found = (0, 0);
            if within.is_mapped() {
                if let Some(picture) = within.downcast_ref::<gtk::Picture>() {
                    found.0 += usize::from(picture.paintable().is_some());
                }
                if let Some(image) = within.downcast_ref::<gtk::Image>() {
                    found.0 += usize::from(image.paintable().is_some());
                }
                if let Some(label) = within.downcast_ref::<gtk::Label>() {
                    let stands_in = ["Picture not on this computer", ":party:"];
                    found.1 += usize::from(stands_in.contains(&label.text().as_str()));
                }
            }
            let mut child = within.first_child();
            while let Some(widget) = child {
                let more = count(&widget);
                found = (found.0 + more.0, found.1 + more.1);
                child = widget.next_sibling();
            }
            found
        }
        let root = list.whole.clone().upcast::<gtk::Widget>();
        assert_eq!(count(&root), (0, 3));
        assert!(list.at_end());

        // One of them is here, and then all.
        let files = store::ImageFileCache::open(folder.path(), 1 << 30).unwrap();
        let picture =
            gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 300, 60)
                .unwrap();
        picture.fill(0x1f6f_8bff);
        let bytes = picture.save_to_bufferv("png", &[]).unwrap();
        files.put(&url("a"), &bytes, "png").unwrap();
        list.show_pictures();
        wait(400);
        assert_eq!(count(&root), (1, 2));
        for name in ["b", "c"] {
            files.put(&url(name), &bytes, "png").unwrap();
        }
        files.put(&emoji.url, &bytes, "png").unwrap();
        list.show_pictures();
        wait(400);
        assert_eq!(count(&root), (4, 0));

        // A reaction changes its message only.
        list.change("99", |item| {
            crate::model::react(&mut item.reactions, "👍", true)
        });
        wait(200);
        assert_eq!(list.item("99").unwrap().reactions.len(), 2);

        // A click on it takes it back.
        let clicked: Rc<RefCell<Vec<(String, String, bool)>>> = Rc::default();
        let react = gio::SimpleAction::new(
            "react",
            Some(&<(String, String, bool)>::static_variant_type()),
        );
        react.connect_activate({
            let clicked = clicked.clone();
            move |_, what| {
                clicked
                    .borrow_mut()
                    .extend(what.and_then(|what| what.get()))
            }
        });
        let opened: Rc<RefCell<Vec<String>>> = Rc::default();
        let picture = gio::SimpleAction::new("picture", Some(glib::VariantTy::STRING));
        picture.connect_activate({
            let opened = opened.clone();
            move |_, url| opened.borrow_mut().extend(url.and_then(|url| url.get()))
        });
        let actions = gio::SimpleActionGroup::new();
        actions.add_action(&react);
        actions.add_action(&picture);
        window.insert_action_group("win", Some(&actions));
        fn chips(within: &gtk::Widget, out: &mut Vec<gtk::Button>) {
            if let Some(chip) = within.downcast_ref::<gtk::Button>()
                && (chip.has_css_class("chip") || chip.has_css_class("picture"))
                && chip.is_mapped()
            {
                out.push(chip.clone());
            }
            let mut child = within.first_child();
            while let Some(widget) = child {
                chips(&widget, out);
                child = widget.next_sibling();
            }
        }
        let mut found = Vec::new();
        chips(&root, &mut found);
        for chip in &found {
            chip.emit_clicked();
        }
        assert_eq!(*opened.borrow(), [url("a"), url("b")]);
        let done = |emoji: &str, on| ("99".to_owned(), emoji.to_owned(), on);
        assert_eq!(
            *clicked.borrow(),
            [done("party;0-eus-d1-abc", true), done("👍", false)]
        );
        assert_eq!(count(&root), (4, 0));
        assert!(list.at_end(), "the newest message stays in view");
    }

    /// As the composer's test: needs a display.
    #[test]
    #[ignore = "needs a display"]
    fn nothing_moves_at_the_end_when_a_message_is_read() {
        gtk::init().unwrap();
        adw::init().unwrap();
        crate::window::load_style();
        let list = MessageList::default();
        let window = gtk::Window::builder()
            .default_width(600)
            .default_height(500)
            .child(&list.whole)
            .build();
        window.present();
        let wait = |milliseconds: u64| {
            let end = Instant::now() + Duration::from_millis(milliseconds);
            while Instant::now() < end {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        let items = |progress: Progress, more: usize| -> Vec<Item> {
            (0..60 + more)
                .map(|at| Item {
                    id: at.to_string(),
                    sender: "Ann".into(),
                    own: at == 59,
                    sent: true,
                    joined: at % 3 == 1,
                    blocks: vec![Block::Text("words ".repeat(1 + at % 40))],
                    progress: (at == 59).then(|| progress.clone()),
                    ..Default::default()
                })
                .collect()
        };
        list.set(items(Progress::Delivered, 0));
        wait(900);
        while !list.is_whole() {
            wait(50);
        }
        // Frames are slow to come without somebody who looks.
        window.queue_draw();
        wait(900);
        let place = list.root.vadjustment();
        let moved: Rc<RefCell<Vec<f64>>> = Rc::default();
        let distance = |place: &gtk::Adjustment| place.upper() - place.page_size() - place.value();
        assert_eq!(distance(&place), 0.0);
        place.connect_value_changed({
            let moved = moved.clone();
            move |place| moved.borrow_mut().push(place.value())
        });
        let before = place.value();
        for round in 0..6 {
            let progress = if round % 2 == 0 {
                Progress::Seen {
                    names: vec!["Bo".into()],
                    others: 1,
                }
            } else {
                Progress::Delivered
            };
            list.update(items(progress, 0));
            window.queue_draw();
            wait(400);
            assert_eq!(moved.take(), [0.0; 0], "round {round}");
        }
        assert_eq!((place.value(), distance(&place)), (before, 0.0));
        list.update(items(Progress::Delivered, 3));
        wait(400);
        assert_eq!(distance(&place), 0.0, "new messages are seen whole");
    }
}
