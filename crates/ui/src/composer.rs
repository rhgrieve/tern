//! Where messages are written. Enter sends, Shift+Enter starts a line; drafts stay per conversation.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};

/// What the text will be.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Aim {
    #[default]
    New,
    Reply {
        message: String,
        name: String,
    },
    Edit {
        message: String,
    },
}

#[derive(Clone)]
pub struct Composer {
    pub root: gtk::Box,
    pub text: gtk::TextView,
    send: gtk::Button,
    open: Rc<RefCell<Option<String>>>,
    drafts: Rc<RefCell<HashMap<String, String>>>,
    aim: Rc<RefCell<Aim>>,
    about: gtk::Box,
    about_text: gtk::Label,
    hint: gtk::Label,
    /// What was being written before an edit began.
    aside: Rc<RefCell<String>>,
}

impl Composer {
    /// `sent` gets the conversation, the text and what it is; `last` edits the last own message.
    pub fn new(sent: impl Fn(&str, &str, &Aim) + 'static, last: impl Fn() + 'static) -> Composer {
        let text = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .top_margin(10)
            .bottom_margin(10)
            .left_margin(12)
            .right_margin(12)
            .hexpand(true)
            .build();
        text.update_property(&[gtk::accessible::Property::Label("Message")]);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(220)
            .child(&text)
            .build();
        let hint = gtk::Label::builder()
            .label("Write a message")
            .xalign(0.0)
            .valign(gtk::Align::Start)
            .margin_start(13)
            .margin_top(10)
            .can_target(false)
            .build();
        hint.add_css_class("dim-label");
        let overlay = gtk::Overlay::builder().child(&scroll).hexpand(true).build();
        overlay.add_overlay(&hint);
        let send = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text("Send (Enter)")
            .valign(gtk::Align::End)
            .sensitive(false)
            .build();
        send.add_css_class("circular");
        send.add_css_class("suggested-action");
        let about = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        about.add_css_class("about");
        about.set_visible(false);
        let about_text = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        about_text.add_css_class("caption");
        let cancel = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Cancel (Esc)")
            .build();
        cancel.add_css_class("flat");
        cancel.add_css_class("circular");
        about.append(&about_text);
        about.append(&cancel);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        line.append(&overlay);
        line.append(&send);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("composer");
        root.append(&about);
        root.append(&line);
        let aim: Rc<RefCell<Aim>> = Rc::default();
        let aside: Rc<RefCell<String>> = Rc::default();

        let open: Rc<RefCell<Option<String>>> = Rc::default();
        let submit = {
            let (text, open, aim, about, aside) = (
                text.clone(),
                open.clone(),
                aim.clone(),
                about.clone(),
                aside.clone(),
            );
            move || {
                let buffer = text.buffer();
                let message = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
                let id = open.borrow().clone();
                if let Some(id) = id
                    && !message.trim().is_empty()
                {
                    let was = aim.take();
                    about.set_visible(false);
                    sent(&id, message.trim(), &was);
                    // After an edit, what was being written before comes back.
                    let next = match was {
                        Aim::Edit { .. } => aside.take(),
                        _ => String::new(),
                    };
                    buffer.set_text(&next);
                }
            }
        };
        let submit = Rc::new(submit);
        send.connect_clicked({
            let submit = submit.clone();
            move |_| submit()
        });
        let keys = gtk::EventControllerKey::new();
        // Before the text view, but after an input method had its turn: Enter while composing
        // Japanese or Chinese chooses a word and must not send.
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let text = text.clone();
            move |keys, key, _, modifiers| {
                let enter = matches!(key, gdk::Key::Return | gdk::Key::KP_Enter);
                if !enter || modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
                    return glib::Propagation::Proceed;
                }
                if let Some(event) = keys.current_event()
                    && text.im_context_filter_keypress(&event)
                {
                    return glib::Propagation::Stop;
                }
                submit();
                glib::Propagation::Stop
            }
        });
        text.add_controller(keys);
        text.buffer().connect_changed({
            let (send, hint) = (send.clone(), hint.clone());
            move |buffer| {
                let empty = buffer.char_count() == 0;
                hint.set_visible(empty);
                let blank = buffer
                    .text(&buffer.start_iter(), &buffer.end_iter(), false)
                    .trim()
                    .is_empty();
                send.set_sensitive(!blank);
            }
        });
        let composer = Composer {
            root,
            text,
            send,
            open,
            drafts: Rc::default(),
            aim,
            about,
            about_text,
            hint,
            aside: aside.clone(),
        };
        cancel.connect_clicked({
            let composer = composer.clone();
            move |_| composer.cancel()
        });
        let others = gtk::EventControllerKey::new();
        others.connect_key_pressed({
            let composer = composer.clone();
            move |_, key, _, _| match key {
                gdk::Key::Escape if *composer.aim.borrow() != Aim::New => {
                    composer.cancel();
                    glib::Propagation::Stop
                }
                gdk::Key::Up if composer.written().is_empty() => {
                    last();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        composer.text.add_controller(others);
        composer
    }

    fn written(&self) -> String {
        let buffer = self.text.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .into()
    }

    pub fn reply(&self, message: &str, name: &str, text: &str) {
        self.leave_edit();
        let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
        self.about_text
            .set_text(&format!("Replying to {name}: {line}"));
        self.aim.replace(Aim::Reply {
            message: message.to_owned(),
            name: name.to_owned(),
        });
        self.about.set_visible(true);
        self.focus();
    }

    pub fn edit(&self, message: &str, source: &str) {
        self.leave_edit();
        self.aside.replace(self.written());
        self.about_text.set_text("Editing your message");
        self.aim.replace(Aim::Edit {
            message: message.to_owned(),
        });
        self.about.set_visible(true);
        self.text.buffer().set_text(source);
        self.focus();
    }

    /// Back to a new message.
    pub fn cancel(&self) {
        self.leave_edit();
        self.aim.replace(Aim::New);
        self.about.set_visible(false);
    }

    /// Puts back what was being written before an edit.
    fn leave_edit(&self) {
        let editing = matches!(*self.aim.borrow(), Aim::Edit { .. });
        if editing {
            self.text.buffer().set_text(&self.aside.take());
            self.aim.replace(Aim::New);
        }
    }

    pub fn set_hint(&self, hint: &str) {
        self.hint.set_text(hint);
    }

    /// Moves to another conversation, keeping what was being written in this one.
    pub fn open(&self, id: &str) {
        self.cancel();
        let written = self.written();
        if let Some(old) = self.open.replace(Some(id.to_owned())) {
            if written.trim().is_empty() {
                self.drafts.borrow_mut().remove(&old);
            } else {
                self.drafts.borrow_mut().insert(old, written);
            }
        }
        let draft = self.drafts.borrow().get(id).cloned().unwrap_or_default();
        self.text.buffer().set_text(&draft);
    }

    pub fn focus(&self) {
        self.text.grab_focus();
    }

    pub fn is_sendable(&self) -> bool {
        self.send.is_sensitive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Sent = Rc<RefCell<Vec<(String, String, Aim)>>>;

    /// `GDK_BACKEND=broadway BROADWAY_DISPLAY=:9 cargo test -p tern -- --ignored composer`,
    /// with `gtk4-broadwayd :9` running, or on any desktop.
    #[test]
    #[ignore = "needs a display"]
    fn the_composer_never_mixes_up_what_it_writes() {
        gtk::init().unwrap();
        let sent: Sent = Rc::default();
        let lasts = Rc::new(std::cell::Cell::new(0));
        let composer = Composer::new(
            {
                let sent = sent.clone();
                move |id, text, aim| {
                    sent.borrow_mut()
                        .push((id.into(), text.into(), aim.clone()))
                }
            },
            {
                let lasts = lasts.clone();
                move || lasts.set(lasts.get() + 1)
            },
        );
        let write = |text: &str| composer.text.buffer().set_text(text);
        let press = || composer.send.emit_clicked();
        let take = || sent.borrow_mut().pop();

        // Nothing without a conversation, and nothing empty.
        write("lost?");
        press();
        assert_eq!(take(), None);
        composer.open("c1");
        write("   ");
        press();
        assert_eq!(take(), None);

        // Drafts stay with their conversation.
        write("for c1");
        composer.open("c2");
        assert_eq!(composer.written(), "");
        write("for c2");
        composer.open("c1");
        assert_eq!(composer.written(), "for c1");

        // An edit puts the draft aside and gives it back.
        composer.edit("m1", "old text");
        assert_eq!(composer.written(), "old text");
        composer.cancel();
        assert_eq!(composer.written(), "for c1");
        assert_eq!(*composer.aim.borrow(), Aim::New);

        // Sending an edit is an edit, and the next message is new again.
        composer.edit("m1", "old text");
        write("new text");
        press();
        assert_eq!(
            take(),
            Some((
                "c1".into(),
                "new text".into(),
                Aim::Edit {
                    message: "m1".into()
                }
            ))
        );
        assert_eq!(
            composer.written(),
            "for c1",
            "the draft is back after the edit"
        );
        press();
        assert_eq!(take(), Some(("c1".into(), "for c1".into(), Aim::New)));

        // A reply, and one given up by going elsewhere.
        composer.reply("m2", "Ann", "what she said");
        write("agreed");
        press();
        assert!(
            matches!(take(), Some((_, text, Aim::Reply { message, .. })) if text == "agreed" && message == "m2")
        );
        composer.reply("m3", "Bo", "x");
        composer.open("c2");
        press();
        assert_eq!(
            take(),
            Some(("c2".into(), "for c2".into(), Aim::New)),
            "the reply stayed behind"
        );

        // An edit given up by going elsewhere neither sends nor becomes a draft.
        composer.open("c1");
        composer.edit("m4", "half edited");
        composer.open("c2");
        composer.open("c1");
        assert_eq!(composer.written(), "");
        // From an edit straight to a reply.
        write("draft");
        composer.edit("m5", "edit me");
        composer.reply("m6", "Cy", "y");
        assert_eq!(composer.written(), "draft");
        press();
        assert!(
            matches!(take(), Some((_, text, Aim::Reply { message, .. })) if text == "draft" && message == "m6")
        );
    }
}
