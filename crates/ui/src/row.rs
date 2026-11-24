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
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reaction {
    pub emoji: String,
    pub count: usize,
    pub mine: bool,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MessageRow {
        pub divider: gtk::Label,
        pub avatar: adw::Avatar,
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
            self.gutter.append(&self.avatar);
            line.append(&self.gutter);

            let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
            content.set_hexpand(true);
            self.header.set_spacing(8);
            self.name.add_css_class("heading");
            self.name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            self.time.add_css_class("caption");
            self.time.add_css_class("dim-label");
            self.header.append(&self.name);
            self.header.append(&self.time);
            content.append(&self.header);

            self.body.set_orientation(gtk::Orientation::Vertical);
            self.body.set_spacing(6);
            content.append(&self.body);

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

fn text(markup: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::Word)
        .natural_wrap_mode(gtk::NaturalWrapMode::Word)
        .selectable(true)
        .build();
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
                let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
                frame.add_css_class("picture");
                frame.set_halign(gtk::Align::Start);
                frame.set_overflow(gtk::Overflow::Hidden);
                let picture = gtk::Picture::builder()
                    .content_fit(gtk::ContentFit::Contain)
                    .can_shrink(true)
                    .build();
                let missing = gtk::Label::new(Some("Picture not on this computer"));
                missing.add_css_class("dim-label");
                frame.append(&picture);
                frame.append(&missing);
                labels.push(missing);
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
            Block::Rule => {}
        }
    }
}

impl Part {
    fn set_picture(&self, url: &str) {
        let Some(picture) = self.root.first_child().and_downcast::<gtk::Picture>() else {
            return;
        };
        let (frame, missing) = (self.root.clone(), self.labels[0].clone());
        // The row may show another message by the time the picture is decoded.
        frame.set_widget_name(url);
        picture.set_paintable(gtk::gdk::Paintable::NONE);
        picture.set_visible(false);
        missing.set_visible(false);
        frame.set_size_request(240, 160);
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
        imp.note.set_visible(item.note.is_some());
        imp.note.set_text(item.note.as_deref().unwrap_or(""));
        clear(&imp.reactions);
        imp.reactions.set_visible(!item.reactions.is_empty());
        for reaction in &item.reactions {
            let chip = gtk::Button::builder()
                .label(format!("{} {}", reaction.emoji, reaction.count))
                .action_name("win.react")
                .action_target(
                    &(item.id.as_str(), reaction.emoji.as_str(), !reaction.mine).to_variant(),
                )
                .sensitive(item.sent)
                .build();
            chip.add_css_class("chip");
            if reaction.mine {
                chip.add_css_class("mine");
            }
            imp.reactions.append(&chip);
        }
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

    fn bind_body(&self, blocks: &[Block]) {
        let imp = self.imp();
        show(&imp.body, &mut imp.parts.borrow_mut(), blocks, 0);
    }
}
