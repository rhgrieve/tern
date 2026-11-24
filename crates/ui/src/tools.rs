//! What can be done with a message: a small bar over the message under the pointer. One bar for
//! the whole list, so rows stay cheap.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};

use crate::list::MessageList;
use crate::row::Item;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tool {
    React(String),
    Reply,
    Copy,
    Edit,
    Delete,
    /// For messages that could not be sent.
    Retry,
    Discard,
}

const QUICK: [&str; 4] = ["👍", "❤️", "😆", "😮"];

pub struct Tools {
    root: gtk::Box,
    edit: gtk::Button,
    delete: gtk::Button,
    /// What only sent messages have, and what only failed ones have.
    usual: RefCell<Vec<gtk::Widget>>,
    failed: RefCell<Vec<gtk::Widget>>,
    item: RefCell<Option<Rc<Item>>>,
    /// The emoji chooser is open, or the bar was opened with the keyboard: it stays.
    held: Cell<bool>,
    /// What had the keyboard focus before the bar.
    back: RefCell<Option<gtk::Widget>>,
}

impl Tools {
    /// Puts the bar over `list`; `used` is called with the message and what to do.
    pub fn attach(list: &MessageList, used: impl Fn(&Rc<Item>, Tool) + 'static) -> Rc<Tools> {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.add_css_class("tools");
        root.set_halign(gtk::Align::End);
        root.set_valign(gtk::Align::Start);
        root.set_margin_end(24);
        root.set_visible(false);
        let button = |icon: &str, tip: &str| {
            let button = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tip)
                .build();
            crate::row::say(&button, tip);
            button.add_css_class("flat");
            button
        };
        let edit = button("document-edit-symbolic", "Edit");
        let delete = button("user-trash-symbolic", "Delete");
        let tools = Rc::new(Tools {
            root: root.clone(),
            edit: edit.clone(),
            delete: delete.clone(),
            usual: RefCell::default(),
            failed: RefCell::default(),
            item: RefCell::default(),
            held: Cell::new(false),
            back: RefCell::default(),
        });
        let used = Rc::new(used);
        let run = {
            let (tools, used) = (Rc::downgrade(&tools), used.clone());
            move |tool: Tool| {
                if let Some(tools) = tools.upgrade() {
                    let item = tools.item.borrow().clone();
                    tools.leave();
                    if let Some(item) = item {
                        used(&item, tool);
                    }
                }
            }
        };
        for emoji in QUICK {
            let quick = gtk::Button::builder()
                .label(emoji)
                .tooltip_text("React")
                .build();
            crate::row::say(&quick, &format!("React with {emoji}"));
            quick.add_css_class("flat");
            quick.connect_clicked({
                let run = run.clone();
                move |_| run(Tool::React(emoji.to_owned()))
            });
            root.append(&quick);
            tools.usual.borrow_mut().push(quick.upcast());
        }
        let more = button("face-smile-big-symbolic", "More reactions");
        let chooser = gtk::EmojiChooser::new();
        chooser.set_parent(&more);
        chooser.connect_emoji_picked({
            let run = run.clone();
            move |_, emoji| run(Tool::React(emoji.to_owned()))
        });
        chooser.connect_closed({
            let tools = Rc::downgrade(&tools);
            move |_| {
                if let Some(tools) = tools.upgrade() {
                    tools.held.set(tools.back.borrow().is_some());
                }
            }
        });
        more.connect_clicked({
            let (chooser, tools) = (chooser.clone(), Rc::downgrade(&tools));
            move |_| {
                if let Some(tools) = tools.upgrade() {
                    tools.held.set(true);
                }
                chooser.popup();
            }
        });
        // A popover has to leave its parent before that goes away.
        more.connect_destroy(move |_| chooser.unparent());
        root.append(&more);
        tools.usual.borrow_mut().push(more.clone().upcast());
        let reply = button("mail-reply-sender-symbolic", "Reply");
        let copy = button("edit-copy-symbolic", "Copy");
        for (button, tool) in [
            (&reply, Tool::Reply),
            (&copy, Tool::Copy),
            (&edit, Tool::Edit),
            (&delete, Tool::Delete),
        ] {
            let run = run.clone();
            button.connect_clicked(move |_| run(tool.clone()));
            root.append(button);
        }
        tools.usual.borrow_mut().push(reply.clone().upcast());
        let retry = button("view-refresh-symbolic", "Try again");
        let discard = button("user-trash-symbolic", "Discard");
        for (button, tool) in [(&retry, Tool::Retry), (&discard, Tool::Discard)] {
            let run = run.clone();
            button.connect_clicked(move |_| run(tool.clone()));
            root.append(button);
            tools.failed.borrow_mut().push(button.clone().upcast());
        }
        list.over.add_overlay(&root);
        // The way to the end is not behind the bar.
        list.raise_down();

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion({
            let (tools, list) = (Rc::downgrade(&tools), list.clone());
            move |_, x, y| {
                if let Some(tools) = tools.upgrade() {
                    tools.moved(&list, x, y);
                }
            }
        });
        motion.connect_leave({
            let tools = Rc::downgrade(&tools);
            move |_| {
                if let Some(tools) = tools.upgrade().filter(|tools| !tools.held.get()) {
                    tools.hide();
                }
            }
        });
        list.over.add_controller(motion);

        // Everything the pointer can do with a message, the keyboard can.
        let keys = gtk::EventControllerKey::new();
        // Before the list, which takes Enter for itself.
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let (tools, list) = (Rc::downgrade(&tools), list.clone());
            move |_, key, _, modifiers| {
                let menu = match key {
                    gdk::Key::Menu | gdk::Key::Return | gdk::Key::KP_Enter => true,
                    gdk::Key::F10 => modifiers.contains(gdk::ModifierType::SHIFT_MASK),
                    _ => false,
                };
                match tools.upgrade() {
                    Some(tools) if menu && tools.open(&list) => glib::Propagation::Stop,
                    _ => glib::Propagation::Proceed,
                }
            }
        });
        list.view.add_controller(keys);
        let escape = gtk::EventControllerKey::new();
        escape.set_propagation_phase(gtk::PropagationPhase::Capture);
        escape.connect_key_pressed({
            let tools = Rc::downgrade(&tools);
            move |_, key, _, _| match tools.upgrade() {
                Some(tools) if key == gdk::Key::Escape => {
                    tools.leave();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        root.add_controller(escape);
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave({
            let tools = Rc::downgrade(&tools);
            move |_| {
                if let Some(tools) = tools
                    .upgrade()
                    .filter(|tools| tools.back.borrow().is_some())
                {
                    tools.back.replace(None);
                    tools.held.set(false);
                    tools.hide();
                }
            }
        });
        root.add_controller(focus);
        list.root.vadjustment().connect_value_changed({
            let tools = Rc::downgrade(&tools);
            move |_| {
                if let Some(tools) = tools.upgrade().filter(|tools| !tools.held.get()) {
                    tools.hide();
                }
            }
        });
        tools
    }

    fn hide(&self) {
        self.root.set_visible(false);
        self.item.replace(None);
    }

    /// Closes the bar; the keyboard focus goes back to where it was.
    fn leave(&self) {
        let back = self.back.take();
        self.held.set(false);
        if let Some(back) = back {
            back.grab_focus();
        }
        self.hide();
    }

    /// Opens the bar for the message with the keyboard focus.
    fn open(&self, list: &MessageList) -> bool {
        let Some((row, cell)) = list.focused() else {
            return false;
        };
        // In the text itself, the keys are the text's.
        if !cell.has_focus() {
            return false;
        }
        let (Some(item), Some(bounds)) = (row.item(), row.compute_bounds(&list.root)) else {
            return false;
        };
        if !(item.sent || item.failed) {
            return false;
        }
        self.show(item, f64::from(bounds.y()));
        self.held.set(true);
        self.back.replace(Some(cell));
        self.root.child_focus(gtk::DirectionType::TabForward);
        true
    }

    pub fn moved(&self, list: &MessageList, x: f64, y: f64) {
        // Pointing at the button that floats over the messages is not pointing at one.
        if list.is_on_down(x, y) {
            if !self.held.get() {
                self.hide();
            }
            return;
        }
        if self.held.get() {
            return;
        }
        let on_bar = self.root.is_visible()
            && self.root.compute_bounds(&list.over).is_some_and(|bounds| {
                bounds.contains_point(&gtk::graphene::Point::new(x as f32, y as f32))
            });
        if on_bar {
            return;
        }
        let Some((row, top)) = list.row_at(x, y) else {
            return self.hide();
        };
        let Some(item) = row.item().filter(|item| item.sent || item.failed) else {
            return self.hide();
        };
        if self.root.is_visible()
            && self
                .item
                .borrow()
                .as_ref()
                .is_some_and(|shown| shown.id == item.id)
        {
            return;
        }
        self.show(item, top);
    }

    fn show(&self, item: Rc<Item>, top: f64) {
        self.edit.set_visible(item.source.is_some());
        self.delete.set_visible(item.own && item.sent);
        for widget in self.usual.borrow().iter() {
            widget.set_visible(item.sent);
        }
        for widget in self.failed.borrow().iter() {
            widget.set_visible(item.failed);
        }
        self.item.replace(Some(item));
        self.root.set_margin_top((top - 10.0).max(2.0) as i32);
        self.root.set_visible(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As the composer's test: needs a display.
    #[test]
    #[ignore = "needs a display"]
    fn the_composer_is_not_the_only_thing_the_keyboard_reaches() {
        gtk::init().unwrap();
        libadwaita::init().unwrap();
        let list = MessageList::default();
        let done: Rc<RefCell<Vec<(String, Tool)>>> = Rc::default();
        let tools = Tools::attach(&list, {
            let done = done.clone();
            move |item, tool| done.borrow_mut().push((item.id.clone(), tool))
        });
        let before = gtk::Button::with_label("before");
        let after = gtk::Button::with_label("after");
        let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        holder.append(&before);
        holder.append(&list.whole);
        holder.append(&after);
        let window = gtk::Window::builder()
            .default_width(600)
            .default_height(500)
            .child(&holder)
            .build();
        window.present();
        let wait = |milliseconds: u64| {
            let end = std::time::Instant::now() + std::time::Duration::from_millis(milliseconds);
            while std::time::Instant::now() < end {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        };
        let items = |count: usize, name: &str| {
            (0..count)
                .map(|at| Item {
                    id: format!("{name}{at}"),
                    sender: "Ann".into(),
                    time: "10:00".into(),
                    text: format!("{name} {at}"),
                    sent: true,
                    ..Default::default()
                })
                .collect::<Vec<_>>()
        };
        for (name, count) in [("a", 60), ("b", 45), ("c", 3)] {
            list.set(items(count, name));
            wait(400);
            assert!(list.is_whole());
            assert_eq!(list.items.n_items() as usize, count);
            before.grab_focus();
            window.child_focus(gtk::DirectionType::TabForward);
            wait(20);
            let row = list.focused().expect("the list is reached").0;
            assert_eq!(row.item().unwrap().id, format!("{name}{}", count - 1));
            assert!(list.follows(), "walking in does not leave the end");
            after.grab_focus();
            list.focus_newest();
            let (row, cell) = list.focused().unwrap();
            assert_eq!(row.item().unwrap().id, format!("{name}{}", count - 1));
            assert!(cell.has_focus());
            assert!(tools.open(&list));
            assert!(tools.root.is_visible());
            assert!(tools.root.focus_child().is_some(), "the first button");
            tools.leave();
            assert!(cell.has_focus() && !tools.root.is_visible());
            tools.open(&list);
            let button = tools.root.first_child().unwrap();
            button.downcast::<gtk::Button>().unwrap().emit_clicked();
            assert!(cell.has_focus());
            let (id, tool) = done.borrow_mut().pop().unwrap();
            assert_eq!(id, format!("{name}{}", count - 1));
            assert_eq!(tool, Tool::React(QUICK[0].into()));
        }
    }
}
