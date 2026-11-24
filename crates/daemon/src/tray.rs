//! The icon in the desktop's tray: whether you show as active, what is unread, and a menu.

use ksni::menu::{MenuItem, StandardItem};
use ksni::{Category, Handle, Icon, Status, ToolTip, Tray, TrayMethods};
use tokio::sync::mpsc;

const MAX_CHATS: usize = 8;
const MAX_TITLE: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dot {
    Active,
    Away,
    Busy,
    #[default]
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Model {
    pub dot: Dot,
    /// One line on how things are.
    pub line: String,
    /// Unread chats, newest first: id and title.
    pub unread: Vec<(String, String)>,
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    Chat(String),
    Window,
    Pause(bool),
    Quit,
}

struct Item {
    model: Model,
    picks: mpsc::UnboundedSender<Pick>,
}

/// Others choose chat titles: no line breaks, and `_` would mark a shortcut key.
fn label(title: &str) -> String {
    let mut text: String = title
        .chars()
        .map(|letter| if letter.is_control() { ' ' } else { letter })
        .take(MAX_TITLE)
        .collect();
    if title.chars().count() > MAX_TITLE {
        text.push('…');
    }
    text.replace('_', "__")
}

fn color(dot: Dot) -> [u8; 3] {
    match dot {
        Dot::Active => [0x57, 0xc2, 0x7a],
        Dot::Away => [0xe8, 0xb3, 0x4b],
        Dot::Busy => [0xe0, 0x5f, 0x65],
        Dot::Offline => [0x8f, 0x96, 0xa3],
    }
}

/// How much of a pixel is within `radius` of the centre, with a soft edge.
fn cover(x: f32, y: f32, centre: (f32, f32), radius: f32) -> f32 {
    let distance = ((x - centre.0).powi(2) + (y - centre.1).powi(2)).sqrt();
    (radius + 0.5 - distance).clamp(0.0, 1.0)
}

/// A disc in the status colour, a ring when notifications are paused, with a mark for unread.
fn draw(size: i32, model: &Model) -> Icon {
    let edge = size as f32;
    let centre = (edge / 2.0 - 0.5, edge / 2.0 - 0.5);
    let [red, green, blue] = color(model.dot);
    let mark = (edge * 0.78, edge * 0.22);
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for row in 0..size {
        for column in 0..size {
            let (x, y) = (column as f32, row as f32);
            let mut alpha = cover(x, y, centre, edge * 0.42);
            if model.paused {
                alpha -= cover(x, y, centre, edge * 0.26);
            }
            let mut rgb = [red, green, blue];
            if !model.unread.is_empty() {
                // A gap around the mark keeps it apart from the disc.
                alpha *= 1.0 - cover(x, y, mark, edge * 0.28);
                let inside = cover(x, y, mark, edge * 0.2);
                if inside > 0.0 {
                    rgb = [0xff, 0xff, 0xff];
                    alpha = inside;
                }
            }
            data.push((alpha.clamp(0.0, 1.0) * 255.0) as u8);
            data.extend(rgb);
        }
    }
    Icon {
        width: size,
        height: size,
        data,
    }
}

impl Item {
    fn pick(&self, pick: Pick) {
        let _ = self.picks.send(pick);
    }
}

fn entry(label: String, pick: Pick) -> MenuItem<Item> {
    StandardItem {
        label,
        activate: Box::new(move |item: &mut Item| item.pick(pick.clone())),
        ..Default::default()
    }
    .into()
}

impl Tray for Item {
    fn id(&self) -> String {
        "tern".into()
    }
    fn title(&self) -> String {
        "Tern".into()
    }
    fn category(&self) -> Category {
        Category::Communications
    }
    fn status(&self) -> Status {
        if self.model.unread.is_empty() {
            Status::Active
        } else {
            Status::NeedsAttention
        }
    }
    fn icon_pixmap(&self) -> Vec<Icon> {
        [22, 32, 48].map(|size| draw(size, &self.model)).into()
    }
    fn attention_icon_pixmap(&self) -> Vec<Icon> {
        self.icon_pixmap()
    }
    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Tern".into(),
            description: self.model.line.clone(),
            ..Default::default()
        }
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        match self.model.unread.first() {
            Some((id, _)) => self.pick(Pick::Chat(id.clone())),
            None => self.pick(Pick::Window),
        }
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut menu = vec![
            StandardItem {
                label: label(&self.model.line),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
        ];
        for (id, title) in self.model.unread.iter().take(MAX_CHATS) {
            menu.push(entry(label(title), Pick::Chat(id.clone())));
        }
        if !self.model.unread.is_empty() {
            menu.push(MenuItem::Separator);
        }
        menu.push(if self.model.paused {
            entry("Resume notifications".into(), Pick::Pause(false))
        } else {
            entry("Pause notifications for 1 hour".into(), Pick::Pause(true))
        });
        menu.push(entry("Open Tern".into(), Pick::Window));
        menu.push(entry("Quit until next login".into(), Pick::Quit));
        menu
    }
}

pub struct TrayIcon {
    handle: Option<Handle<Item>>,
    shown: Model,
}

impl TrayIcon {
    /// Without a tray on this desktop everything else still works; the icon appears when a tray does.
    pub async fn start() -> (Self, mpsc::UnboundedReceiver<Pick>) {
        let (picks, queue) = mpsc::unbounded_channel();
        let item = Item {
            model: Model::default(),
            picks,
        };
        let handle = item
            .disable_dbus_name(true)
            .assume_sni_available(true)
            .spawn()
            .await
            .inspect_err(|error| tracing::info!(%error, "no tray"))
            .ok();
        let icon = TrayIcon {
            handle,
            shown: Model::default(),
        };
        (icon, queue)
    }

    pub async fn show(&mut self, model: Model) {
        if model == self.shown {
            return;
        }
        self.shown = model.clone();
        if let Some(handle) = &self.handle {
            handle.update(move |item| item.model = model).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_cannot_break_the_menu() {
        assert_eq!(label("a_b\nc"), "a__b c");
        assert!(label(&"x".repeat(200)).ends_with('…'));
    }

    #[test]
    fn the_icon_shows_status_unread_and_pause() {
        let model = |unread: bool, paused| Model {
            dot: Dot::Active,
            unread: if unread {
                vec![("c".into(), "C".into())]
            } else {
                vec![]
            },
            paused,
            ..Default::default()
        };
        let plain = draw(32, &model(false, false));
        assert_eq!(plain.data.len(), 32 * 32 * 4);
        let pixel = |icon: &Icon, x: usize, y: usize| icon.data[(y * 32 + x) * 4..][..4].to_vec();
        assert_eq!(pixel(&plain, 16, 16), [255, 0x57, 0xc2, 0x7a]);
        assert_eq!(pixel(&plain, 0, 0)[0], 0);
        assert_eq!(
            pixel(&draw(32, &model(false, true)), 16, 16)[0],
            0,
            "a ring"
        );
        assert_eq!(
            pixel(&draw(32, &model(true, false)), 25, 7),
            [255, 255, 255, 255]
        );
    }

    #[test]
    fn a_click_opens_the_newest_unread_chat() {
        let (picks, mut queue) = mpsc::unbounded_channel();
        let mut item = Item {
            model: Model::default(),
            picks,
        };
        item.activate(0, 0);
        item.model.unread = vec![("new".into(), "N".into()), ("old".into(), "O".into())];
        item.activate(0, 0);
        assert_eq!(queue.try_recv(), Ok(Pick::Window));
        assert_eq!(queue.try_recv(), Ok(Pick::Chat("new".into())));
    }
}
