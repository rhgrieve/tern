//! Ctrl+K: jump to a chat, or find a message.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use store::{HIGHLIGHT_END, HIGHLIGHT_START, Store};

use crate::model::short_time;
use crate::rich::escape;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub conversation: String,
    pub title: String,
    /// Markup.
    pub detail: String,
}

/// A found text with its matches in bold.
fn marked(snippet: &str) -> String {
    let line = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    let mut bold = false;
    for letter in line.chars() {
        match letter {
            HIGHLIGHT_START if !bold => {
                bold = true;
                out.push_str("<b>");
            }
            HIGHLIGHT_END if bold => {
                bold = false;
                out.push_str("</b>");
            }
            HIGHLIGHT_START | HIGHLIGHT_END => {}
            letter => out.push_str(&escape(letter.encode_utf8(&mut [0; 4]))),
        }
    }
    if bold {
        out.push_str("</b>");
    }
    out
}

/// Chats whose name matches, then messages. An empty query lists the recent chats.
pub fn find(store: &Store, query: &str) -> Vec<Hit> {
    let now = chrono::Utc::now();
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let chats = store.recent_chats(300).unwrap_or_default();
    let mut hits: Vec<Hit> = chats
        .iter()
        .map(|chat| (chat, chat.display_title()))
        .filter(|(_, title)| {
            let title = title.to_lowercase();
            words.iter().all(|word| title.contains(word.as_str()))
        })
        .take(8)
        .map(|(chat, title)| Hit {
            conversation: chat.id.clone(),
            title: title.split_whitespace().collect::<Vec<_>>().join(" "),
            detail: chat
                .last_message_at
                .map(|at| short_time(at, now))
                .unwrap_or_default(),
        })
        .collect();
    if words.is_empty() {
        return hits;
    }
    if let Ok(sidebar) = store.sidebar() {
        for team in &sidebar.teams {
            for channel in &team.channels {
                let title = format!("{} {}", team.team.name, channel.name).to_lowercase();
                if hits.len() < 12 && words.iter().all(|word| title.contains(word.as_str())) {
                    hits.push(Hit {
                        conversation: channel.id.clone(),
                        title: channel.name.clone(),
                        detail: escape(&team.team.name),
                    });
                }
            }
        }
    }
    for found in store.search_messages(query, None, 30).unwrap_or_default() {
        let place = chats
            .iter()
            .find(|chat| chat.id == found.conversation_id)
            .map_or_else(|| "Channel".to_owned(), |chat| chat.display_title());
        hits.push(Hit {
            conversation: found.conversation_id.clone(),
            title: format!(
                "{} in {}, {}",
                found.sender_name.as_deref().unwrap_or("Someone"),
                place.split_whitespace().collect::<Vec<_>>().join(" "),
                short_time(found.created_at, now)
            ),
            detail: marked(&found.snippet),
        });
    }
    hits
}

pub fn show(
    parent: &impl IsA<gtk::Widget>,
    store: Arc<Store>,
    chosen: impl Fn(&str) + 'static,
) -> adw::Dialog {
    let entry = gtk::SearchEntry::builder()
        .placeholder_text("Go to a chat or find a message")
        .hexpand(true)
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Browse)
        .build();
    list.add_css_class("navigation-sidebar");
    let empty = gtk::Label::builder()
        .label("Nothing found")
        .margin_top(24)
        .margin_bottom(24)
        .build();
    empty.add_css_class("dim-label");
    list.set_placeholder(Some(&empty));
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    let header = adw::HeaderBar::builder()
        .title_widget(&entry)
        .show_title(true)
        .build();
    let view = adw::ToolbarView::builder().content(&scroll).build();
    view.add_top_bar(&header);
    let dialog = adw::Dialog::builder()
        .child(&view)
        .content_width(620)
        .content_height(520)
        .build();
    crate::beside::closes(&dialog);

    let hits: Rc<RefCell<Vec<Hit>>> = Rc::default();
    let turn = Rc::new(std::cell::Cell::new(0u64));
    let fill = {
        let (list, hits, turn) = (list.clone(), hits.clone(), turn.clone());
        Rc::new(move |query: String| {
            let mine = turn.get() + 1;
            turn.set(mine);
            let store = store.clone();
            let (list, hits, turn) = (list.clone(), hits.clone(), turn.clone());
            glib::spawn_future_local(async move {
                let found = gio::spawn_blocking(move || find(&store, &query))
                    .await
                    .unwrap_or_default();
                // A later query was faster.
                if turn.get() != mine {
                    return;
                }
                list.remove_all();
                for hit in &found {
                    let title = gtk::Label::builder()
                        .label(&hit.title)
                        .xalign(0.0)
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .build();
                    let detail = gtk::Label::builder()
                        .label(&hit.detail)
                        .use_markup(true)
                        .xalign(0.0)
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .build();
                    detail.add_css_class("caption");
                    detail.add_css_class("dim-label");
                    let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
                    row.add_css_class("hit");
                    row.append(&title);
                    row.append(&detail);
                    list.append(&row);
                }
                hits.replace(found);
                list.select_row(list.row_at_index(0).as_ref());
            });
        })
    };
    entry.connect_search_changed({
        let fill = fill.clone();
        move |entry| fill(entry.text().into())
    });
    list.connect_row_activated({
        let (dialog, hits) = (dialog.clone(), hits.clone());
        move |_, row| {
            let hit = usize::try_from(row.index())
                .ok()
                .and_then(|index| hits.borrow().get(index).cloned());
            if let Some(hit) = hit {
                dialog.close();
                chosen(&hit.conversation);
            }
        }
    });
    entry.connect_activate({
        let list = list.clone();
        move |_| {
            if let Some(row) = list.selected_row() {
                row.activate();
            }
        }
    });
    // The entry takes Escape for itself.
    entry.connect_stop_search({
        let dialog = dialog.downgrade();
        move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
        }
    });
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed({
        let list = list.clone();
        move |_, key, _, _| {
            let step = match key {
                gdk::Key::Down => 1,
                gdk::Key::Up => -1,
                _ => return glib::Propagation::Proceed,
            };
            let at = list.selected_row().map_or(-1, |row| row.index());
            if let Some(row) = list.row_at_index((at + step).max(0)) {
                list.select_row(Some(&row));
            }
            glib::Propagation::Stop
        }
    });
    entry.add_controller(keys);
    fill(String::new());
    dialog.present(Some(parent));
    entry.grab_focus();
    dialog
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_chats_then_messages_and_marks_matches() {
        let store = crate::demo::store();
        assert!(!find(&store, "").is_empty());
        let hits = find(&store, "harbour");
        assert_eq!(hits[0].title, "Harbour launch");
        let hits = find(&store, "pilot");
        assert!(
            hits.iter().any(|hit| hit.detail.contains("<b>pilot</b>")),
            "{hits:?}"
        );
        assert!(find(&store, "zzzz").is_empty());
    }

    #[test]
    fn found_text_cannot_become_markup() {
        let text = format!("a <i> {HIGHLIGHT_START}x&y{HIGHLIGHT_END} {HIGHLIGHT_START}open");
        assert_eq!(marked(&text), "a &lt;i&gt; <b>x&amp;y</b> <b>open</b>");
    }
}
