//! Where messages are written. Enter sends, Shift+Enter starts a line; drafts stay per conversation.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use serde::{Deserialize, Serialize};

use crate::emoji;
use crate::mention::{self, Person};
use crate::row;
use crate::spell;
use crate::writing;

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

/// What was written: the text, who is mentioned in it, and what goes with it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Written {
    pub text: String,
    pub mentions: Vec<Person>,
    pub files: Vec<PathBuf>,
}

impl Written {
    /// What it says on one line, for the list of chats: nothing when there is nothing.
    pub fn line(&self) -> Option<String> {
        let text = self.text.split_whitespace().collect::<Vec<_>>().join(" ");
        match self.files.len() {
            _ if !text.is_empty() => Some(text),
            0 => None,
            1 => Some("1 file".into()),
            many => Some(format!("{many} files")),
        }
    }

    /// Both, this first.
    pub fn before(mut self, newer: Written) -> Written {
        if !newer.text.is_empty() {
            self.text = if self.text.is_empty() {
                newer.text
            } else {
                format!("{}\n{}", self.text, newer.text)
            };
        }
        self.mentions.extend(newer.mentions);
        for file in newer.files {
            if !self.files.contains(&file) {
                self.files.push(file);
            }
        }
        self
    }
}

/// What is being written, by conversation, for all windows.
pub type Drafts = Rc<RefCell<HashMap<String, Written>>>;

type Search = Rc<dyn Fn(&str) -> Vec<Person>>;

/// How many emoji are offered.
const EMOJI: usize = 8;

/// What can be put in place of what is being typed.
#[derive(Debug, Clone)]
enum Choice {
    Person(Person),
    Emoji(emoji::Found),
}

#[derive(Clone)]
pub struct Composer {
    pub root: gtk::Box,
    pub text: gtk::TextView,
    send: gtk::Button,
    open: Rc<RefCell<Option<String>>>,
    drafts: Drafts,
    files: Rc<RefCell<Vec<PathBuf>>>,
    chips: gtk::Box,
    /// People chosen while writing this text.
    chosen: Rc<RefCell<Vec<Person>>>,
    search: Rc<RefCell<Option<Search>>>,
    popover: gtk::Popover,
    choices: gtk::ListBox,
    found: Rc<RefCell<Vec<Choice>>>,
    aim: Rc<RefCell<Aim>>,
    about: gtk::Box,
    about_text: gtk::Label,
    hint: gtk::Label,
    /// What was being written before an edit began.
    aside: Rc<RefCell<Written>>,
    /// The text is being set by us.
    filling: Rc<std::cell::Cell<bool>>,
}

impl Composer {
    /// `sent` gets the conversation, what was written and what it is; `last` edits the last own message.
    pub fn new(
        drafts: Drafts,
        sent: impl Fn(&str, &Written, &Aim) + 'static,
        last: impl Fn() + 'static,
    ) -> Composer {
        let text: gtk::TextView = glib::Object::builder::<writing::Writing>()
            .property("wrap-mode", gtk::WrapMode::WordChar)
            .property("accepts-tab", false)
            .property("top-margin", 10)
            .property("bottom-margin", 10)
            .property("left-margin", 12)
            .property("right-margin", 12)
            .property("hexpand", true)
            .build()
            .upcast();
        text.update_property(&[gtk::accessible::Property::Label("Message")]);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(220)
            .child(&text)
            .build();
        scroll.vadjustment().connect_changed(writing::keep_within);
        scroll
            .vadjustment()
            .connect_value_changed(writing::keep_within);
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
        row::say(&send, "Send");
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
        row::say(&cancel, "Cancel");
        cancel.add_css_class("flat");
        cancel.add_css_class("circular");
        about.append(&about_text);
        about.append(&cancel);
        let attach = gtk::Button::builder()
            .icon_name("mail-attachment-symbolic")
            .tooltip_text("Attach files")
            .valign(gtk::Align::End)
            .build();
        row::say(&attach, "Attach files");
        attach.add_css_class("flat");
        attach.add_css_class("circular");
        let chips = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        chips.add_css_class("chips");
        chips.set_visible(false);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        line.append(&attach);
        line.append(&overlay);
        line.append(&send);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("composer");
        root.append(&about);
        root.append(&chips);
        root.append(&line);

        let files: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
        let chosen: Rc<RefCell<Vec<Person>>> = Rc::default();
        let choices = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Browse)
            .build();
        choices.add_css_class("navigation-sidebar");
        let popover = gtk::Popover::builder()
            .child(&choices)
            .autohide(false)
            .has_arrow(false)
            .position(gtk::PositionType::Top)
            .build();
        popover.set_parent(&text);
        text.connect_destroy({
            let popover = popover.clone();
            move |_| popover.unparent()
        });
        let composer = Composer {
            root,
            text: text.clone(),
            send: send.clone(),
            open: Rc::default(),
            drafts,
            files,
            chips,
            chosen,
            search: Rc::default(),
            popover,
            choices,
            found: Rc::default(),
            aim: Rc::default(),
            about,
            about_text,
            hint,
            aside: Rc::default(),
            filling: Rc::default(),
        };
        let submit = {
            let composer = composer.clone();
            move || {
                let written = composer.written();
                let Some(id) = composer.open.borrow().clone() else {
                    return;
                };
                if written.text.is_empty() && written.files.is_empty() {
                    return;
                }
                // An edit changes the text only.
                let editing = matches!(*composer.aim.borrow(), Aim::Edit { .. });
                if editing && written.text.is_empty() {
                    return;
                }
                let was = composer.aim.take();
                composer.about.set_visible(false);
                sent(&id, &written, &was);
                // After an edit, what was being written before comes back.
                let next = if editing {
                    composer.aside.take()
                } else {
                    Written::default()
                };
                composer.fill(next);
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
            let (text, composer) = (text.clone(), composer.clone());
            move |keys, key, _, modifiers| {
                if composer.completes(key) {
                    return glib::Propagation::Stop;
                }
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
        spell::attach(&text, {
            let chosen = composer.chosen.clone();
            move || {
                let chosen = chosen.borrow();
                chosen.iter().map(|(_, name)| name.clone()).collect()
            }
        });
        text.buffer().connect_changed({
            let composer = composer.clone();
            move |_| {
                composer.update();
                composer.complete();
            }
        });
        composer.choices.connect_row_activated({
            let composer = composer.clone();
            move |_, row| composer.choose(row.index())
        });
        attach.connect_clicked({
            let composer = composer.clone();
            move |_| composer.ask_for_files()
        });
        text.connect_paste_clipboard({
            let composer = composer.clone();
            move |text| {
                if composer.paste(&text.clipboard()) {
                    text.stop_signal_emission_by_name("paste-clipboard");
                }
            }
        });
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
                gdk::Key::Up if composer.written() == Written::default() => {
                    last();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        composer.text.add_controller(others);
        composer
    }

    fn raw(&self) -> String {
        let buffer = self.text.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .into()
    }

    fn written(&self) -> Written {
        let text = self.raw().trim().to_owned();
        Written {
            mentions: mention::placed(&text, &self.chosen.borrow()),
            files: self.files.borrow().clone(),
            text,
        }
    }

    fn fill(&self, written: Written) {
        self.chosen.replace(written.mentions);
        self.files.replace(written.files);
        self.filling.set(true);
        self.text.buffer().set_text(&written.text);
        self.filling.set(false);
        self.show_files();
        self.popover.popdown();
    }

    fn update(&self) {
        let raw = self.raw();
        self.hint.set_visible(raw.is_empty());
        self.send
            .set_sensitive(!raw.trim().is_empty() || !self.files.borrow().is_empty());
    }

    /// Who can be mentioned: called with what was typed after the `@`.
    pub fn set_people(&self, search: impl Fn(&str) -> Vec<Person> + 'static) {
        self.search.replace(Some(Rc::new(search)));
    }

    fn cursor(&self) -> gtk::TextIter {
        let buffer = self.text.buffer();
        buffer.iter_at_mark(&buffer.get_insert())
    }

    /// Offers people when a mention is being typed.
    fn complete(&self) {
        let buffer = self.text.buffer();
        let before = buffer.text(&buffer.start_iter(), &self.cursor(), false);
        if let Some((at, emoji)) = emoji::whole(&before).filter(|_| !self.filling.get()) {
            // Not while the buffer is telling about the change that led here.
            let composer = self.clone();
            glib::idle_add_local_once(move || {
                let now = composer.before();
                if emoji::whole(&now) == Some((at, emoji)) {
                    composer.replace(at, &format!("{emoji} "));
                }
            });
        }
        let search = self.search.borrow().clone();
        let found: Vec<Choice> = match (mention::query(&before), search) {
            (Some((_, typed)), Some(search)) => {
                search(&typed).into_iter().map(Choice::Person).collect()
            }
            _ => Vec::new(),
        };
        let found = match emoji::query(&before) {
            Some((_, typed)) if found.is_empty() => emoji::matching(&typed, EMOJI)
                .into_iter()
                .map(Choice::Emoji)
                .collect(),
            _ => found,
        };
        self.choices.remove_all();
        for choice in &found {
            let text = match choice {
                Choice::Person((_, name)) => name.clone(),
                Choice::Emoji(found) => format!("{}  :{}:", found.emoji, found.name),
            };
            self.choices
                .append(&gtk::Label::builder().label(text).xalign(0.0).build());
        }
        self.choices
            .select_row(self.choices.row_at_index(0).as_ref());
        let any = !found.is_empty();
        self.found.replace(found);
        if any && self.text.is_mapped() {
            let place = self.text.iter_location(&self.cursor());
            let (x, y) = self.text.buffer_to_window_coords(
                gtk::TextWindowType::Widget,
                place.x(),
                place.y(),
            );
            self.popover
                .set_pointing_to(Some(&gdk::Rectangle::new(x, y, 1, place.height())));
            self.popover.popup();
        } else {
            self.popover.popdown();
        }
    }

    /// Whether the key was for the list of people.
    fn completes(&self, key: gdk::Key) -> bool {
        if !self.popover.is_visible() || self.found.borrow().is_empty() {
            return false;
        }
        let at = self.choices.selected_row().map_or(0, |row| row.index());
        match key {
            gdk::Key::Down | gdk::Key::Up => {
                let next = if key == gdk::Key::Down {
                    at + 1
                } else {
                    at - 1
                };
                if let Some(row) = self.choices.row_at_index(next.max(0)) {
                    self.choices.select_row(Some(&row));
                }
            }
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Tab => self.choose(at),
            gdk::Key::Escape => self.popover.popdown(),
            _ => return false,
        }
        true
    }

    fn choose(&self, index: i32) {
        let choice = usize::try_from(index)
            .ok()
            .and_then(|index| self.found.borrow().get(index).cloned());
        let before = self.before();
        match choice {
            Some(Choice::Person(person)) => {
                let Some((at, _)) = mention::query(&before) else {
                    return;
                };
                self.replace(at, &format!("@{} ", person.1));
                self.chosen.borrow_mut().push(person);
            }
            Some(Choice::Emoji(found)) => {
                let Some((at, _)) = emoji::query(&before) else {
                    return;
                };
                self.replace(at, &format!("{} ", found.emoji));
            }
            None => return,
        }
        self.popover.popdown();
        self.text.grab_focus();
    }

    /// The text before the cursor.
    fn before(&self) -> glib::GString {
        let buffer = self.text.buffer();
        buffer.text(&buffer.start_iter(), &self.cursor(), false)
    }

    /// Puts `text` in place of what is between a character and the cursor.
    fn replace(&self, at: usize, text: &str) {
        let buffer = self.text.buffer();
        let mut end = self.cursor();
        let mut start = buffer.iter_at_offset(at as i32);
        buffer.begin_user_action();
        buffer.delete(&mut start, &mut end);
        buffer.insert(&mut start, text);
        buffer.end_user_action();
    }

    /// Adds files to go with the message.
    pub fn attach(&self, paths: impl IntoIterator<Item = PathBuf>) {
        {
            let mut files = self.files.borrow_mut();
            for path in paths {
                if !files.contains(&path) && files.len() < tern_engine::MAX_FILES {
                    files.push(path);
                }
            }
        }
        self.show_files();
        self.focus();
    }

    fn show_files(&self) {
        while let Some(chip) = self.chips.first_child() {
            self.chips.remove(&chip);
        }
        let files = self.files.borrow().clone();
        self.chips.set_visible(!files.is_empty());
        for path in files {
            let chip = gtk::Box::new(gtk::Orientation::Horizontal, 2);
            chip.add_css_class("file");
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let label = gtk::Label::builder()
                .label(&name)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .max_width_chars(24)
                .build();
            let remove = gtk::Button::builder()
                .icon_name("window-close-symbolic")
                .tooltip_text(format!("Remove {name}"))
                .build();
            row::say(&remove, &format!("Remove {name}"));
            remove.add_css_class("flat");
            remove.add_css_class("circular");
            let composer = self.clone();
            remove.connect_clicked(move |_| {
                composer.files.borrow_mut().retain(|file| *file != path);
                composer.show_files();
            });
            chip.append(&label);
            chip.append(&remove);
            self.chips.append(&chip);
        }
        self.update();
    }

    fn ask_for_files(&self) {
        let dialog = gtk::FileDialog::builder().title("Attach files").build();
        let window = self.root.root().and_downcast::<gtk::Window>();
        let composer = self.clone();
        dialog.open_multiple(window.as_ref(), gio::Cancellable::NONE, move |chosen| {
            let Ok(files) = chosen else { return };
            let paths = (0..files.n_items())
                .filter_map(|index| files.item(index).and_downcast::<gio::File>()?.path());
            composer.attach(paths);
        });
    }

    /// Files and pictures from the clipboard go with the message; text is pasted as usual.
    fn paste(&self, clipboard: &gdk::Clipboard) -> bool {
        let formats = clipboard.formats();
        if formats.contains_type(gdk::FileList::static_type()) {
            let composer = self.clone();
            clipboard.read_value_async(
                gdk::FileList::static_type(),
                glib::Priority::DEFAULT,
                gio::Cancellable::NONE,
                move |value| {
                    if let Some(files) = value
                        .ok()
                        .and_then(|value| value.get::<gdk::FileList>().ok())
                    {
                        composer
                            .attach(files.files().iter().filter_map(gio::prelude::FileExt::path));
                    }
                },
            );
            return true;
        }
        let text = formats.contains_type(glib::Type::STRING);
        if formats.contains_type(gdk::Texture::static_type()) && !text {
            let composer = self.clone();
            clipboard.read_texture_async(gio::Cancellable::NONE, move |texture| {
                if let Ok(Some(texture)) = texture {
                    composer.attach_picture(&texture);
                }
            });
            return true;
        }
        false
    }

    pub fn attach_picture(&self, texture: &gdk::Texture) {
        match save_picture(texture) {
            Ok(path) => self.attach([path]),
            Err(error) => eprintln!("tern: a pasted picture could not be kept: {error}"),
        }
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
        self.files.replace(Vec::new());
        self.show_files();
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
            self.aim.replace(Aim::New);
            self.fill(self.aside.take());
        }
    }

    pub fn set_hint(&self, hint: &str) {
        self.hint.set_text(hint);
        self.text
            .update_property(&[gtk::accessible::Property::Placeholder(hint)]);
    }

    /// Moves to another conversation, keeping what was being written in this one.
    pub fn open(&self, id: &str) {
        self.close();
        self.open.replace(Some(id.to_owned()));
        let draft = self.drafts.borrow().get(id).cloned().unwrap_or_default();
        self.fill(draft);
    }

    /// Leaves the conversation, keeping what was being written in it.
    pub fn close(&self) {
        self.cancel();
        self.keep();
        self.open.replace(None);
        self.fill(Written::default());
    }

    /// Puts what is being written with the drafts.
    pub fn keep(&self) {
        let Some(id) = self.open.borrow().clone() else {
            return;
        };
        // During an edit, what was put aside is what counts.
        let editing = matches!(*self.aim.borrow(), Aim::Edit { .. });
        let written = if editing {
            self.aside.borrow().clone()
        } else {
            self.written()
        };
        let mut drafts = self.drafts.borrow_mut();
        if written == Written::default() {
            drafts.remove(&id);
        } else {
            drafts.insert(id, written);
        }
    }

    /// What is being written in: a conversation, or `conversation/post` for a thread.
    pub fn opened(&self) -> Option<String> {
        self.open.borrow().clone()
    }

    pub fn is_open(&self, id: &str) -> bool {
        self.open.borrow().as_deref() == Some(id)
    }

    /// Calls `changed` whenever the user changes the text: not when a draft is put in.
    pub fn connect_changed(&self, changed: impl Fn(&str) + 'static) {
        let composer = self.clone();
        self.text.buffer().connect_changed(move |_| {
            if !composer.filling.get() {
                changed(&composer.raw());
            }
        });
    }

    /// Gives back what could not be sent. Whatever is being written by then stays, after it.
    pub fn give_back(&self, id: &str, unsent: Written) {
        let here = self.is_open(id);
        let editing = matches!(*self.aim.borrow(), Aim::Edit { .. });
        if here && editing {
            let written = self.aside.take();
            self.aside.replace(unsent.before(written));
        } else if here {
            let written = self.written();
            self.fill(unsent.before(written));
        } else {
            let mut drafts = self.drafts.borrow_mut();
            let written = drafts.remove(id).unwrap_or_default();
            drafts.insert(id.to_owned(), unsent.before(written));
        }
    }

    pub fn focus(&self) {
        self.text.grab_focus();
    }

    pub fn is_sendable(&self) -> bool {
        self.send.is_sensitive()
    }
}

/// Keeps a pasted picture for the session, readable only by the user.
fn save_picture(texture: &gdk::Texture) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let folder = pasted_folder();
    let said = |error: std::io::Error| error.to_string();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&folder)
        .map_err(said)?;
    let path = free_path(&folder);
    texture
        .save_to_png(&path)
        .map_err(|error| error.to_string())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(said)?;
    Ok(path)
}

fn pasted_folder() -> PathBuf {
    tern_ipc::runtime_folder()
        .unwrap_or_else(|_| glib::user_runtime_dir().join("tern"))
        .join("pasted")
}

/// Removes pasted pictures the daemon has copied. Files the user chose are never touched.
pub fn forget_pasted(files: &[PathBuf]) {
    let folder = pasted_folder();
    for file in files {
        if file.parent() == Some(folder.as_path()) {
            let _ = std::fs::remove_file(file);
        }
    }
}

fn free_path(folder: &Path) -> PathBuf {
    let now = chrono::Local::now().format("%Y-%m-%d %H.%M.%S%.3f");
    folder.join(format!("Picture {now}.png"))
}

#[cfg(test)]
mod tests {
    use super::*;

    type Sent = Rc<RefCell<Vec<(String, Written, Aim)>>>;

    fn plain(text: &str) -> Written {
        Written {
            text: text.into(),
            ..Default::default()
        }
    }

    /// `GDK_BACKEND=broadway BROADWAY_DISPLAY=:9 cargo test -p tern -- --ignored composer`,
    /// with `gtk4-broadwayd :9` running, or on any desktop.
    #[test]
    #[ignore = "needs a display"]
    fn the_composer_never_mixes_up_what_it_writes() {
        gtk::init().unwrap();
        let sent: Sent = Rc::default();
        let lasts = Rc::new(std::cell::Cell::new(0));
        let drafts = Drafts::default();
        let composer = Composer::new(
            drafts.clone(),
            {
                let sent = sent.clone();
                move |id, text, aim| {
                    sent.borrow_mut()
                        .push((id.into(), text.clone(), aim.clone()))
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
        assert_eq!(composer.written().text, "");
        write("for c2");
        composer.open("c1");
        assert_eq!(composer.written().text, "for c1");

        // An edit puts the draft aside and gives it back.
        composer.edit("m1", "old text");
        assert_eq!(composer.written().text, "old text");
        composer.cancel();
        assert_eq!(composer.written().text, "for c1");
        assert_eq!(*composer.aim.borrow(), Aim::New);

        // Sending an edit is an edit, and the next message is new again.
        composer.edit("m1", "old text");
        write("new text");
        press();
        assert_eq!(
            take(),
            Some((
                "c1".into(),
                plain("new text"),
                Aim::Edit {
                    message: "m1".into()
                }
            ))
        );
        assert_eq!(
            composer.written().text,
            "for c1",
            "the draft is back after the edit"
        );
        press();
        assert_eq!(take(), Some(("c1".into(), plain("for c1"), Aim::New)));

        // A reply, and one given up by going elsewhere.
        composer.reply("m2", "Ann", "what she said");
        write("agreed");
        press();
        assert!(
            matches!(take(), Some((_, text, Aim::Reply { message, .. })) if text.text == "agreed" && message == "m2")
        );
        composer.reply("m3", "Bo", "x");
        composer.open("c2");
        press();
        assert_eq!(
            take(),
            Some(("c2".into(), plain("for c2"), Aim::New)),
            "the reply stayed behind"
        );

        // An edit given up by going elsewhere neither sends nor becomes a draft.
        composer.open("c1");
        composer.edit("m4", "half edited");
        composer.open("c2");
        composer.open("c1");
        assert_eq!(composer.written().text, "");
        // From an edit straight to a reply.
        write("draft");
        composer.edit("m5", "edit me");
        composer.reply("m6", "Cy", "y");
        assert_eq!(composer.written().text, "draft");
        press();
        assert!(
            matches!(take(), Some((_, text, Aim::Reply { message, .. })) if text.text == "draft" && message == "m6")
        );

        // Mentions and files travel with the text, and stay with their conversation.
        composer.open("c3");
        composer.set_people(|typed| {
            let all = [
                ("a".to_owned(), "Ann Example".to_owned()),
                ("b".to_owned(), "Bo".to_owned()),
            ];
            mention::matching(&all, typed, 5)
        });
        write("hello @an");
        assert_eq!(composer.found.borrow().len(), 1);
        composer.choose(0);
        assert_eq!(composer.raw(), "hello @Ann Example ");
        composer.attach([PathBuf::from("/x/plan.pdf"), PathBuf::from("/x/plan.pdf")]);
        composer.open("c4");
        assert_eq!(composer.written(), Written::default());
        composer.open("c3");
        press();
        let (_, written, _) = take().unwrap();
        assert_eq!(
            written.mentions,
            [("a".to_owned(), "Ann Example".to_owned())]
        );
        assert_eq!(written.files, [PathBuf::from("/x/plan.pdf")]);
        assert_eq!(
            composer.written(),
            Written::default(),
            "nothing is left for the next message"
        );
        // A file needs no text; a mention that was deleted mentions nobody.
        composer.attach([PathBuf::from("/x/a.png")]);
        press();
        assert_eq!(take().unwrap().1.files.len(), 1);
        write("hi @bo");
        composer.choose(0);
        write("never mind");
        press();
        assert!(take().unwrap().1.mentions.is_empty());
        // What the daemon did not take comes back, before what was written since.
        composer.open("c5");
        write("newer");
        let lost = Written {
            files: vec![PathBuf::from("/x/c.txt")],
            ..plain("older")
        };
        composer.give_back("c5", lost.clone());
        assert_eq!(composer.written().text, "older\nnewer");
        assert_eq!(composer.written().files.len(), 1);
        composer.give_back("c6", lost.clone());
        composer.open("c6");
        assert_eq!(composer.written(), lost);
        composer.edit("m8", "being edited");
        composer.give_back("c6", plain("again"));
        assert_eq!(composer.raw(), "being edited");
        composer.cancel();
        assert_eq!(composer.written().text, "again\nolder");
        composer.open("c3");
        // Putting in a draft is not writing.
        let changes = Rc::new(std::cell::Cell::new(0));
        composer.connect_changed({
            let changes = changes.clone();
            move |_| changes.set(changes.get() + 1)
        });
        composer.open("c5");
        composer.open("c6");
        composer.open("c5");
        assert_eq!(changes.get(), 0);
        write("by hand");
        assert!(changes.get() > 0);
        write("");
        // All of it can be kept and given back.
        composer.open("c7");
        write("seven");
        composer.edit("m9", "an edit is no draft");
        composer.keep();
        let all = drafts.borrow().clone();
        assert_eq!(all["c7"], plain("seven"));
        assert!(
            !all.values()
                .any(|written| written.text.contains("no draft"))
        );
        composer.close();
        drafts.replace(HashMap::from([("c8".to_owned(), plain("eight"))]));
        assert_eq!(composer.raw(), "");
        composer.open("c8");
        assert_eq!(composer.written(), plain("eight"));
        // Another window sees what was written here.
        let other = Composer::new(drafts.clone(), |_, _, _| {}, || {});
        composer.close();
        assert!(!composer.is_open("c8") && composer.raw().is_empty());
        other.open("c8");
        assert_eq!(other.written(), plain("eight"));
        other.text.buffer().set_text("eight, and more");
        other.close();
        composer.open("c8");
        assert_eq!(composer.written(), plain("eight, and more"));
        composer.open("c3");
        // Files put aside by an edit come back after it, and do not go with it.
        composer.attach([PathBuf::from("/x/b.txt")]);
        composer.edit("m7", "text");
        press();
        assert!(take().unwrap().1.files.is_empty());
        assert_eq!(composer.written().files, [PathBuf::from("/x/b.txt")]);
    }
}
