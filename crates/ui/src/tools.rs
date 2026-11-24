//! What can be done with a message: a small bar over the message under the pointer. One bar for
//! the whole list, so rows stay cheap.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;

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
    /// The emoji chooser is open: the bar stays.
    held: Cell<bool>,
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
        });
        let used = Rc::new(used);
        let run = {
            let (tools, used) = (Rc::downgrade(&tools), used.clone());
            move |tool: Tool| {
                if let Some(tools) = tools.upgrade() {
                    let item = tools.item.borrow().clone();
                    tools.hide();
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
                    tools.held.set(false);
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

    fn moved(&self, list: &MessageList, x: f64, y: f64) {
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
