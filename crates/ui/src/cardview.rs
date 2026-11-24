//! Widgets for a card.

use gtk4 as gtk;
use gtk4::gio;
use gtk4::prelude::*;
use libadwaita as adw;

use crate::card::{Button, Card, Does, Piece, Tone, Width, host};
use crate::row;

fn column(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

pub fn card(card: &Card) -> gtk::Widget {
    let holder = column(8);
    fill(&holder, &card.pieces);
    if !card.answers.is_empty() {
        let button = gtk::Button::builder()
            .label("Answer in Teams")
            .halign(gtk::Align::Start)
            .build();
        let titles: Vec<&str> = card
            .answers
            .iter()
            .map(String::as_str)
            .filter(|title| !title.is_empty())
            .collect();
        if titles.is_empty() {
            button.set_label("Open in Teams");
        } else {
            button.set_tooltip_text(Some(&titles.join(", ")));
        }
        open_on_click(&button, &card.teams);
        holder.append(&button);
    }
    holder.upcast()
}

/// What is wide scrolls instead of making the window wide.
fn sideways(inner: &impl IsA<gtk::Widget>) -> gtk::Widget {
    gtk::ScrolledWindow::builder()
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .child(inner)
        .build()
        .upcast()
}

fn open_on_click(button: &gtk::Button, url: &str) {
    let url = url.to_owned();
    button.connect_clicked(move |button| {
        let window = button.root().and_downcast::<gtk::Window>();
        gtk::UriLauncher::new(&url).launch(window.as_ref(), gio::Cancellable::NONE, |opened| {
            if let Err(error) = opened {
                eprintln!("tern: a link could not be opened: {error}");
            }
        });
    });
}

/// Shows the picture a widget is named after, if it is there.
fn load(picture: &gtk::Picture) {
    let shown = picture.clone();
    crate::art::picture(&picture.widget_name(), move |texture| {
        if let Some(texture) = texture {
            let (width, height) =
                crate::art::shown_size(texture.width(), texture.height(), crate::art::scale());
            shown.set_size_request(width.min(240), height.min(180));
            shown.set_paintable(Some(texture));
            shown.set_visible(true);
            shown.remove_css_class("missing");
        }
    });
}

/// The size of a preview's picture. Its room is kept, so that nothing moves when it comes.
const COVER: (i32, i32) = (104, 78);

/// What is behind a link, in a card that opens it.
pub fn preview(preview: &crate::rich::Preview) -> gtk::Button {
    let label = |text: &str, lines: i32| {
        gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .lines(lines)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(44)
            .visible(!text.is_empty())
            .build()
    };
    let title = label(&preview.title, 2);
    title.add_css_class("heading");
    let description = label(&preview.description, 2);
    description.add_css_class("caption");
    let site = label(&preview.site, 1);
    site.add_css_class("caption");
    site.add_css_class("dim-label");
    let text = column(2);
    text.set_valign(gtk::Align::Center);
    text.add_css_class("preview-text");
    text.append(&title);
    text.append(&description);
    text.append(&site);
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    if let Some(url) = &preview.picture {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Cover)
            .can_shrink(true)
            .width_request(COVER.0)
            .height_request(COVER.1)
            .build();
        picture.add_css_class("cover");
        picture.add_css_class("missing");
        picture.set_widget_name(url);
        load_cover(&picture);
        line.append(&picture);
    }
    line.append(&text);
    let button = gtk::Button::builder()
        .child(&line)
        .tooltip_text(&preview.url)
        .overflow(gtk::Overflow::Hidden)
        .build();
    button.add_css_class("preview");
    row::say(
        &button,
        &format!("Open {}, {}", preview.title, preview.site),
    );
    open_on_click(&button, &preview.url);
    button
}

pub fn load_cover(picture: &gtk::Picture) {
    let shown = picture.clone();
    crate::art::picture(&picture.widget_name(), move |texture| {
        if let Some(texture) = texture {
            shown.set_paintable(Some(texture));
            shown.remove_css_class("missing");
        }
    });
}

/// Looks again for pictures that were not there.
pub fn look_again(within: &gtk::Widget) {
    for_missing(within, &load);
}

pub fn for_missing<W: IsA<gtk::Widget>>(within: &gtk::Widget, load: &dyn Fn(&W)) {
    if let Some(picture) = within.downcast_ref::<W>()
        && picture.has_css_class("missing")
    {
        return load(picture);
    }
    let mut child = within.first_child();
    while let Some(widget) = child {
        for_missing(&widget, load);
        child = widget.next_sibling();
    }
}

fn fill(holder: &gtk::Box, pieces: &[Piece]) {
    for piece in pieces {
        holder.append(&widget(piece));
    }
}

fn set_tone(widget: &impl IsA<gtk::Widget>, tone: Tone) {
    if let Some(class) = tone.class() {
        widget.add_css_class(class);
    }
}

fn widget(piece: &Piece) -> gtk::Widget {
    match piece {
        Piece::Text {
            markup,
            tone,
            subtle,
            lines,
            align,
        } => {
            let label = row::text(markup);
            label.set_xalign(*align);
            label.set_justify(match *align {
                0.0 => gtk::Justification::Left,
                1.0 => gtk::Justification::Right,
                _ => gtk::Justification::Center,
            });
            set_tone(&label, *tone);
            if *subtle {
                label.add_css_class("dim-label");
            }
            if let Some(lines) = lines {
                label.set_lines(*lines);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            }
            label.upcast()
        }
        Piece::Picture { url, about } => {
            let picture = gtk::Picture::builder()
                .content_fit(gtk::ContentFit::Contain)
                .can_shrink(true)
                .halign(gtk::Align::Start)
                .visible(false)
                .build();
            picture.set_alternative_text(Some(about));
            picture.add_css_class("missing");
            picture.set_widget_name(url);
            load(&picture);
            picture.upcast()
        }
        Piece::Facts(facts) => {
            let inner = column(4);
            let names = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
            for (name, value) in facts {
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                let name = row::text(&format!("<b>{name}</b>"));
                name.set_valign(gtk::Align::Start);
                names.add_widget(&name);
                let value = row::text(value);
                value.set_hexpand(true);
                line.append(&name);
                line.append(&value);
                inner.append(&line);
            }
            inner.upcast()
        }
        Piece::Columns(columns) => {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            for (width, pieces) in columns {
                let inner = column(6);
                inner.set_hexpand(matches!(width, Width::Share(_)));
                fill(&inner, pieces);
                line.append(&inner);
            }
            if columns.len() > 3 {
                return sideways(&line);
            }
            line.upcast()
        }
        Piece::Box {
            tone,
            filled,
            pieces,
        } => {
            let inner = column(6);
            if *filled {
                inner.add_css_class("part");
                // Its colour, not that of the text in it.
                if let Some(class) = tone.class() {
                    inner.add_css_class(&format!("part-{class}"));
                }
            }
            fill(&inner, pieces);
            inner.upcast()
        }
        Piece::Table { header, rows } => {
            let grid = gtk::Grid::builder()
                .column_spacing(16)
                .row_spacing(6)
                .build();
            for (at, cells) in rows.iter().enumerate() {
                for (column_at, pieces) in cells.iter().enumerate() {
                    let cell = column(4);
                    if *header && at == 0 {
                        cell.add_css_class("heading");
                    }
                    fill(&cell, pieces);
                    grid.attach(&cell, column_at as i32, at as i32, 1, 1);
                }
            }
            sideways(&grid)
        }
        Piece::Code(code) => {
            let label = gtk::Label::builder()
                .label(code)
                .xalign(0.0)
                .selectable(true)
                .build();
            label.add_css_class("monospace");
            label.add_css_class("code");
            label.upcast()
        }
        Piece::Badge { text, tone } => {
            let label = gtk::Label::builder()
                .label(text)
                .halign(gtk::Align::Start)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            label.add_css_class("badge");
            set_tone(&label, *tone);
            label.upcast()
        }
        Piece::Progress(value) => {
            let bar = gtk::ProgressBar::new();
            match value {
                Some(value) => bar.set_fraction(*value),
                None => bar.set_text(Some("In progress")),
            }
            bar.set_show_text(value.is_none());
            bar.upcast()
        }
        Piece::Buttons(buttons) => row_of(buttons),
        Piece::Rule => gtk::Separator::new(gtk::Orientation::Horizontal).upcast(),
    }
}

fn row_of(buttons: &[Button]) -> gtk::Widget {
    let holder = column(6);
    let line = adw::WrapBox::builder()
        .child_spacing(6)
        .line_spacing(6)
        .build();
    holder.append(&line);
    for button in buttons {
        let label = gtk::Label::builder()
            .label(&button.title)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(40)
            .build();
        match &button.does {
            Does::Open(url) => {
                let open = gtk::Button::builder().child(&label).build();
                // The title is the sender's word; this is where it leads.
                open.set_tooltip_text(Some(host(url)));
                open_on_click(&open, url);
                line.append(&open);
            }
            Does::More(pieces) => {
                let more = column(6);
                more.add_css_class("part");
                fill(&more, pieces);
                let toggle = gtk::ToggleButton::builder().child(&label).build();
                more.set_visible(false);
                toggle.bind_property("active", &more, "visible").build();
                line.append(&toggle);
                holder.append(&more);
            }
        }
    }
    holder.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As the composer's test: needs a display. A grid of facts once took more room than it used.
    #[test]
    #[ignore = "needs a display"]
    fn the_composer_is_not_below_rows_of_the_wrong_height() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let store = crate::demo::store();
        let conversation = crate::model::conversation(&store, "c4", None, 20, crate::model::now());
        let item = conversation.items.last().unwrap();
        let Some(crate::rich::Block::Card(card)) = item.blocks.last() else {
            panic!("no card")
        };
        let row = row::MessageRow::default();
        for piece in &card.pieces {
            let mut item = item.clone();
            item.blocks = vec![crate::rich::Block::Card(Box::new(Card {
                pieces: vec![piece.clone()],
                ..Default::default()
            }))];
            row.bind(&std::rc::Rc::new(item));
            let line = row.last_child().unwrap();
            let content = line.first_child().unwrap().next_sibling().unwrap();
            let height =
                |widget: &gtk::Widget, width| widget.measure(gtk::Orientation::Vertical, width).0;
            // The face is 36 high.
            let least = height(&content, 576).max(36);
            assert_eq!(height(&line, 624), least, "{piece:?}");
        }
    }
}
