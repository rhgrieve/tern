//! One message in the list. Rows are recycled, so everything is set again on every bind.

use std::cell::RefCell;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use libadwaita as adw;

use crate::rich::Block;

/// A message, ready to show.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Item {
    pub id: String,
    pub sender: String,
    pub sender_id: Option<String>,
    /// Shown next to the name.
    pub time: String,
    pub blocks: Vec<Block>,
    /// Follows a message of the same person closely: no name, no face.
    pub joined: bool,
    pub own: bool,
    pub note: Option<String>,
    pub reactions: Vec<Reaction>,
    /// A line above the message, e.g. the day.
    pub divider: Option<String>,
    /// A post's replies, which a click opens.
    pub thread: Option<String>,
    /// What copying the message gives.
    pub text: String,
    /// The message as it can be edited, for own messages that can.
    pub source: Option<String>,
    /// The service knows this message: it can be answered and reacted to.
    pub sent: bool,
    /// It could not be sent.
    pub failed: bool,
    /// How far it got; for the newest own message only.
    pub progress: Option<crate::mark::Progress>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reaction {
    /// What the service calls it.
    pub emoji: String,
    /// Set for the organization's own.
    pub custom: Option<tern_engine::Emoji>,
    pub count: usize,
    pub mine: bool,
}

impl Reaction {
    pub fn name(&self) -> &str {
        match &self.custom {
            Some(emoji) => &emoji.name,
            None => &self.emoji,
        }
    }
}

impl Item {
    /// The message in one line, as a screen reader says it.
    pub fn spoken(&self) -> String {
        let mut out = format!("{}, {}: {}", self.sender, self.time, self.text.trim());
        for reaction in &self.reactions {
            let who = if reaction.mine {
                ", one of them you"
            } else {
                ""
            };
            out.push_str(&format!(". {} {}{who}", reaction.count, reaction.name()));
        }
        let progress = self.progress.as_ref().map(crate::mark::Progress::words);
        for more in [&self.note, &progress, &self.thread, &self.divider]
            .into_iter()
            .flatten()
        {
            out.push_str(". ");
            out.push_str(more);
        }
        out
    }
}

/// Emoji are drawn as high as text.
const EMOJI: i32 = 18;

/// What a chip shows for an emoji of the organization's own: its name until the picture is here.
fn custom(emoji: &tern_engine::Emoji, count: usize) -> gtk::Box {
    let picture = gtk::Image::builder()
        .pixel_size(EMOJI)
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    picture.add_css_class("missing");
    picture.set_widget_name(&emoji.url);
    let holder = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    holder.append(&picture);
    holder.append(&gtk::Label::new(Some(&format!(":{}:", emoji.name))));
    holder.append(&gtk::Label::new(Some(&count.to_string())));
    show_emoji(&picture);
    holder
}

/// Shows the picture instead of the name, once it is here.
fn show_emoji(picture: &gtk::Image) {
    let shown = picture.clone();
    crate::art::picture(&picture.widget_name(), move |texture| {
        if let Some(texture) = texture {
            shown.set_paintable(Some(texture));
            shown.set_visible(true);
            shown.remove_css_class("missing");
            if let Some(name) = shown.next_sibling() {
                name.set_visible(false);
            }
        }
    });
}

/// Says what a button with only a picture does.
pub fn say(button: &impl IsA<gtk::Accessible>, what: &str) {
    button.update_property(&[gtk::accessible::Property::Label(what)]);
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MessageRow {
        pub divider: gtk::Label,
        pub avatar: adw::Avatar,
        pub there: std::cell::OnceCell<gtk::DrawingArea>,
        pub person: RefCell<Option<String>>,
        pub gutter: gtk::Box,
        pub header: gtk::Box,
        pub name: gtk::Label,
        pub time: gtk::Label,
        pub body: gtk::Box,
        /// Parts of earlier messages, hidden or in use: changing a part is much cheaper than
        /// building one, and a list that scrolls does little else.
        pub parts: RefCell<Vec<super::Part>>,
        pub note: gtk::Label,
        pub reactions: gtk::Box,
        pub thread: gtk::Button,
        pub mark: crate::mark::Mark,
        pub item: RefCell<Option<std::rc::Rc<Item>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageRow {
        const NAME: &'static str = "TernMessageRow";
        type Type = super::MessageRow;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for MessageRow {
        fn constructed(&self) {
            self.parent_constructed();
            let row = self.obj();
            row.set_orientation(gtk::Orientation::Vertical);
            row.add_css_class("message");

            self.divider.add_css_class("day");
            self.divider.set_halign(gtk::Align::Center);
            row.append(&self.divider);

            let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            self.avatar.set_size(36);
            self.avatar.set_show_initials(true);
            self.avatar.set_valign(gtk::Align::Start);
            self.gutter.set_size_request(36, -1);
            let there = crate::presence::dot();
            self.gutter
                .append(&crate::presence::with_dot(&self.avatar, &there));
            let _ = self.there.set(there);
            line.append(&self.gutter);

            let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
            content.set_hexpand(true);
            self.header.set_spacing(8);
            self.name.add_css_class("heading");
            self.name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            self.time.add_css_class("caption");
            self.time.add_css_class("dim-label");
            // Smaller, but on the same line.
            self.name.set_valign(gtk::Align::BaselineFill);
            self.time.set_valign(gtk::Align::BaselineFill);
            self.header.append(&self.name);
            self.header.append(&self.time);
            content.append(&self.header);

            self.body.set_orientation(gtk::Orientation::Vertical);
            self.body.set_spacing(8);
            self.body.set_hexpand(true);
            let said = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            said.append(&self.body);
            said.append(&self.mark.area);
            content.append(&said);

            self.note.add_css_class("caption");
            self.note.add_css_class("dim-label");
            self.note.set_xalign(0.0);
            content.append(&self.note);
            self.reactions.set_spacing(4);
            content.append(&self.reactions);
            self.thread.set_halign(gtk::Align::Start);
            self.thread.add_css_class("flat");
            self.thread.add_css_class("thread");
            self.thread.set_action_target_value(Some(&"".to_variant()));
            self.thread.set_action_name(Some("win.thread"));
            content.append(&self.thread);
            line.append(&content);
            row.append(&line);
        }
    }

    impl WidgetImpl for MessageRow {}
    impl BoxImpl for MessageRow {}
}

glib::wrapper! {
    pub struct MessageRow(ObjectSubclass<imp::MessageRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for MessageRow {
    fn default() -> Self {
        glib::Object::new()
    }
}

fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

pub fn text(markup: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::Word)
        .natural_wrap_mode(gtk::NaturalWrapMode::Word)
        .selectable(true)
        .build();
    label.add_css_class("text");
    label.connect_copy_clipboard(|label| {
        // The break points we added are not part of what was written.
        let Some((start, end)) = label.selection_bounds() else {
            return;
        };
        let (start, count) = (start.max(0) as usize, (end - start).max(0) as usize);
        let selected: String = label.text().chars().skip(start).take(count).collect();
        label
            .clipboard()
            .set_text(&crate::rich::as_written(&selected));
        label.stop_signal_emission_by_name("copy-clipboard");
    });
    set_text(&label, markup);
    label
}

fn set_text(label: &gtk::Label, markup: &str) {
    // Content we got wrong must show as text, not vanish.
    if gtk::pango::parse_markup(&strip_links(markup), '\0').is_ok() {
        label.set_markup(markup);
    } else {
        label.set_text(markup);
    }
}

/// Labels know `<a>`, Pango's own parser does not.
fn strip_links(markup: &str) -> String {
    let mut out = String::with_capacity(markup.len());
    let mut rest = markup;
    while let Some(at) = rest.find("<a href=\"") {
        out.push_str(&rest[..at]);
        match rest[at..].find('>') {
            Some(end) => rest = &rest[at + end + 1..],
            None => return out,
        }
    }
    out.push_str(rest);
    out.replace("</a>", "")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Heading,
    Code,
    Quote,
    Item,
    Table,
    Picture,
    Card,
    Preview,
    Rule,
}

/// A widget that shows one kind of block.
#[derive(Debug)]
pub struct Part {
    kind: Kind,
    root: gtk::Widget,
    /// The text and, for list items, the mark before it.
    labels: Vec<gtk::Label>,
    /// Holds a quote's blocks.
    inner: Option<Parts>,
}

#[derive(Debug)]
struct Parts {
    holder: gtk::Box,
    parts: Vec<Part>,
}

/// Hidden parts a row keeps beyond what it shows.
const SPARE: usize = 8;
/// Quotes in quotes end somewhere.
const MAX_DEPTH: u32 = 4;

fn kind(block: &Block) -> Kind {
    match block {
        Block::Text(_) => Kind::Text,
        Block::Heading(_) => Kind::Heading,
        Block::Code { .. } => Kind::Code,
        Block::Quote(_) => Kind::Quote,
        Block::Item { .. } => Kind::Item,
        Block::Table { .. } => Kind::Table,
        Block::Picture(_) => Kind::Picture,
        Block::Card(_) => Kind::Card,
        Block::Preview(_) => Kind::Preview,
        Block::Rule => Kind::Rule,
    }
}

impl Part {
    fn new(kind: Kind) -> Part {
        let mut labels = Vec::new();
        let mut inner = None;
        let root: gtk::Widget = match kind {
            Kind::Text | Kind::Heading => {
                let label = text("");
                if kind == Kind::Heading {
                    label.add_css_class("title-4");
                }
                labels.push(label.clone());
                label.upcast()
            }
            Kind::Code => {
                let label = gtk::Label::builder().xalign(0.0).selectable(true).build();
                label.add_css_class("monospace");
                labels.push(label.clone());
                let scroll = gtk::ScrolledWindow::builder()
                    .vscrollbar_policy(gtk::PolicyType::Never)
                    .hscrollbar_policy(gtk::PolicyType::Automatic)
                    .child(&label)
                    .build();
                scroll.add_css_class("code");
                scroll.upcast()
            }
            Kind::Quote => {
                let holder = gtk::Box::new(gtk::Orientation::Vertical, 4);
                holder.add_css_class("quote");
                inner = Some(Parts {
                    holder: holder.clone(),
                    parts: Vec::new(),
                });
                holder.upcast()
            }
            Kind::Item => {
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                let mark = gtk::Label::new(None);
                mark.set_valign(gtk::Align::Start);
                mark.add_css_class("dim-label");
                let label = text("");
                label.set_hexpand(true);
                line.append(&mark);
                line.append(&label);
                labels.extend([label, mark]);
                line.upcast()
            }
            Kind::Table => {
                let scroll = gtk::ScrolledWindow::builder()
                    .vscrollbar_policy(gtk::PolicyType::Never)
                    .build();
                scroll.add_css_class("card");
                scroll.add_css_class("table");
                scroll.upcast()
            }
            Kind::Picture => {
                let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
                let frame = gtk::Button::builder()
                    .child(&holder)
                    .halign(gtk::Align::Start)
                    .overflow(gtk::Overflow::Hidden)
                    .action_target(&"".to_variant())
                    .action_name("win.picture")
                    .build();
                frame.add_css_class("picture");
                frame.set_cursor_from_name(Some("zoom-in"));
                say(&frame, "Show the picture");
                let picture = gtk::Picture::builder()
                    .content_fit(gtk::ContentFit::Contain)
                    .can_shrink(true)
                    .build();
                let missing = gtk::Label::new(Some("Picture not on this computer"));
                missing.add_css_class("dim-label");
                holder.append(&picture);
                holder.append(&missing);
                labels.push(missing);
                frame.upcast()
            }
            Kind::Card => {
                let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
                frame.add_css_class("card");
                frame.add_css_class("adaptive");
                frame.upcast()
            }
            Kind::Preview => {
                let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
                frame.set_halign(gtk::Align::Start);
                frame.upcast()
            }
            Kind::Rule => gtk::Separator::new(gtk::Orientation::Horizontal).upcast(),
        };
        Part {
            kind,
            root,
            labels,
            inner,
        }
    }

    fn set(&mut self, block: &Block, depth: u32) {
        match block {
            Block::Text(markup) | Block::Heading(markup) => set_text(&self.labels[0], markup),
            Block::Code { text, .. } => self.labels[0].set_text(text),
            Block::Item {
                depth,
                mark,
                markup,
            } => {
                self.root.set_margin_start(16 * (*depth).min(6) as i32);
                set_text(&self.labels[0], markup);
                self.labels[1].set_text(mark);
            }
            Block::Quote(blocks) => {
                if let Some(inner) = &mut self.inner {
                    let blocks = if depth < MAX_DEPTH {
                        blocks.as_slice()
                    } else {
                        &[]
                    };
                    show(&inner.holder, &mut inner.parts, blocks, depth + 1);
                }
            }
            Block::Table { header, rows } => {
                let grid = gtk::Grid::builder()
                    .column_spacing(12)
                    .row_spacing(4)
                    .build();
                for (row, cells) in rows.iter().take(200).enumerate() {
                    for (column, cell) in cells.iter().take(30).enumerate() {
                        let label = text(cell);
                        if *header && row == 0 {
                            label.add_css_class("heading");
                        }
                        grid.attach(&label, column as i32, row as i32, 1, 1);
                    }
                }
                if let Some(scroll) = self.root.downcast_ref::<gtk::ScrolledWindow>() {
                    scroll.set_child(Some(&grid));
                }
            }
            Block::Picture(url) => self.set_picture(url),
            Block::Card(card) => {
                if let Some(frame) = self.root.downcast_ref::<gtk::Box>() {
                    clear(frame);
                    let shown = crate::cardview::card(card);
                    shown.set_hexpand(true);
                    frame.append(&shown);
                }
            }
            Block::Preview(preview) => {
                if let Some(frame) = self.root.downcast_ref::<gtk::Box>() {
                    clear(frame);
                    frame.append(&crate::cardview::preview(preview));
                }
            }
            Block::Rule => {}
        }
    }
}

impl Part {
    fn picture(&self) -> Option<(gtk::Button, gtk::Picture)> {
        let frame = self.root.clone().downcast::<gtk::Button>().ok()?;
        let picture = frame.child()?.first_child().and_downcast()?;
        Some((frame, picture))
    }

    fn set_picture(&self, url: &str) {
        let Some((frame, picture)) = self.picture() else {
            return;
        };
        let missing = &self.labels[0];
        frame.set_action_target_value(Some(&url.to_variant()));
        // The row may show another message by the time the picture is decoded.
        frame.set_widget_name(url);
        picture.set_paintable(gtk::gdk::Paintable::NONE);
        picture.set_visible(false);
        missing.set_visible(false);
        frame.remove_css_class("missing");
        frame.set_size_request(240, 160);
        self.load_picture(url);
    }

    /// How far the baseline of its last line of text is above its bottom.
    fn below_baseline(&self) -> Option<i32> {
        match self.kind {
            Kind::Text | Kind::Heading | Kind::Item => {
                let layout = self.labels.first()?.layout();
                let (_, all) = layout.extents();
                let mut lines = layout.iter();
                while lines.next_line() {}
                let below = all.y() + all.height() - lines.baseline();
                Some((f64::from(below) / f64::from(gtk::pango::SCALE)).round() as i32)
            }
            Kind::Quote => {
                let parts = &self.inner.as_ref()?.parts;
                parts
                    .iter()
                    .rfind(|part| part.root.is_visible())?
                    .below_baseline()
            }
            _ => None,
        }
    }

    fn look_again(&self) {
        match self.kind {
            Kind::Picture if self.root.has_css_class("missing") => {
                self.load_picture(&self.root.widget_name());
            }
            Kind::Card => crate::cardview::look_again(&self.root),
            Kind::Preview => {
                crate::cardview::for_missing(&self.root, &crate::cardview::load_cover);
            }
            _ => {}
        }
        for part in self.inner.iter().flat_map(|inner| &inner.parts) {
            part.look_again();
        }
    }

    /// Leaves everything as it is until it knows.
    fn load_picture(&self, url: &str) {
        let Some((frame, picture)) = self.picture() else {
            return;
        };
        let missing = self.labels[0].clone();
        let wanted = url.to_owned();
        crate::art::picture(url, move |texture| {
            if frame.widget_name() != wanted {
                return;
            }
            missing.set_visible(texture.is_none());
            picture.set_visible(texture.is_some());
            picture.set_paintable(texture);
            match texture {
                Some(texture) => {
                    let (width, height) = crate::art::shown_size(
                        texture.width(),
                        texture.height(),
                        crate::art::scale(),
                    );
                    frame.set_size_request(width, height);
                    frame.remove_css_class("missing");
                }
                None => {
                    frame.set_size_request(-1, -1);
                    frame.add_css_class("missing");
                }
            }
        });
    }
}

/// Shows `blocks` in `holder` with the parts it has, adding what is missing.
fn show(holder: &gtk::Box, parts: &mut Vec<Part>, blocks: &[Block], depth: u32) {
    let mut free: Vec<Part> = std::mem::take(parts);
    let mut last: Option<gtk::Widget> = None;
    for block in blocks {
        let kind = kind(block);
        let mut part = match free.iter().position(|part| part.kind == kind) {
            Some(index) => free.swap_remove(index),
            None => {
                let part = Part::new(kind);
                holder.append(&part.root);
                part
            }
        };
        part.set(block, depth);
        part.root.set_visible(true);
        if part.root.prev_sibling() != last {
            holder.reorder_child_after(&part.root, last.as_ref());
        }
        last = Some(part.root.clone());
        parts.push(part);
    }
    for (index, part) in free.into_iter().enumerate() {
        if index < SPARE {
            part.root.set_visible(false);
            parts.push(part);
        } else {
            holder.remove(&part.root);
        }
    }
}

impl MessageRow {
    /// The message it shows.
    pub fn item(&self) -> Option<std::rc::Rc<Item>> {
        self.imp().item.borrow().clone()
    }

    pub fn bind(&self, item: &std::rc::Rc<Item>) {
        let imp = self.imp();
        imp.item.replace(Some(item.clone()));
        imp.thread.set_visible(item.thread.is_some());
        if let Some(thread) = &item.thread {
            imp.thread.set_label(thread);
            imp.thread
                .set_action_target_value(Some(&item.id.to_variant()));
        }
        imp.divider.set_visible(item.divider.is_some());
        imp.divider.set_text(item.divider.as_deref().unwrap_or(""));
        imp.avatar.set_visible(!item.joined);
        imp.header.set_visible(!item.joined);
        imp.avatar.set_text(Some(&item.sender));
        let face = item
            .sender_id
            .as_deref()
            .filter(|_| !item.joined)
            .and_then(crate::art::face);
        imp.avatar.set_custom_image(face.as_ref());
        if let Some(there) = imp.there.get() {
            let person = item.sender_id.as_deref().filter(|_| !item.joined);
            crate::presence::set(there, person);
        }
        imp.person
            .replace(item.sender_id.clone().filter(|_| !item.joined));
        imp.name.set_text(&item.sender);
        imp.time.set_text(&item.time);
        if item.joined {
            self.add_css_class("joined");
        } else {
            self.remove_css_class("joined");
        }
        if item.own {
            self.add_css_class("own");
        } else {
            self.remove_css_class("own");
        }
        self.bind_body(&item.blocks);
        imp.mark.set(item.progress.as_ref());
        self.align_mark();
        imp.note.set_visible(item.note.is_some());
        imp.note.set_text(item.note.as_deref().unwrap_or(""));
        clear(&imp.reactions);
        imp.reactions.set_visible(!item.reactions.is_empty());
        for reaction in &item.reactions {
            let chip = gtk::Button::builder()
                .action_name("win.react")
                .action_target(
                    &(item.id.as_str(), reaction.emoji.as_str(), !reaction.mine).to_variant(),
                )
                .sensitive(item.sent)
                .build();
            match &reaction.custom {
                Some(emoji) => chip.set_child(Some(&custom(emoji, reaction.count))),
                None => chip.set_label(&format!("{} {}", reaction.emoji, reaction.count)),
            }
            chip.add_css_class("chip");
            let verb = if reaction.mine { "Take back" } else { "Add" };
            say(
                &chip,
                &format!("{verb} {}, {} so far", reaction.name(), reaction.count),
            );
            if reaction.mine {
                chip.add_css_class("mine");
            }
            imp.reactions.append(&chip);
        }
    }

    /// The mark stands on the last line of the text, if the message ends with text.
    fn align_mark(&self) {
        let imp = self.imp();
        let parts = imp.parts.borrow();
        let last = parts.iter().rfind(|part| part.root.is_visible());
        let below = last.and_then(Part::below_baseline);
        imp.mark.stand(below);
    }

    /// Shows how far the message got; nothing else of it may have changed.
    pub fn mark(&self, item: &std::rc::Rc<Item>) {
        let imp = self.imp();
        imp.item.replace(Some(item.clone()));
        imp.mark.set(item.progress.as_ref());
    }

    /// Shows the face if it is one of `people`.
    pub fn show_face(&self, people: &[String]) {
        let imp = self.imp();
        if let Some(person) = imp
            .person
            .borrow()
            .as_ref()
            .filter(|person| people.contains(person))
        {
            imp.avatar
                .set_custom_image(crate::art::face(person).as_ref());
        }
    }

    /// Shows pictures that came after the row was bound. Nothing else changes, so a selection stays.
    pub fn show_pictures(&self) {
        for part in self.imp().parts.borrow().iter() {
            part.look_again();
        }
        crate::cardview::for_missing::<gtk::Image>(self.imp().reactions.upcast_ref(), &show_emoji);
    }

    fn bind_body(&self, blocks: &[Block]) {
        let imp = self.imp();
        show(&imp.body, &mut imp.parts.borrow_mut(), blocks, 0);
    }
}
