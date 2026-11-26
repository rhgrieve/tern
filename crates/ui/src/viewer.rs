//! A picture on its own, as large as the window lets it be. First what is shown in the message, then
//! the picture in full once it is here.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::{gdk, gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::{art, row};

/// Room left around it.
const AROUND: i32 = 48;

pub struct Viewer {
    pub dialog: adw::Dialog,
    pub picture: gtk::Picture,
    scroll: gtk::ScrolledWindow,
    toasts: adw::ToastOverlay,
    /// The file that is shown, which is also the one to save.
    file: RefCell<Option<PathBuf>>,
    actual: Cell<bool>,
}

impl Viewer {
    pub fn show(parent: &impl IsA<gtk::Widget>, url: &str) -> Rc<Viewer> {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::ScaleDown)
            .can_shrink(true)
            .alternative_text("Picture")
            .build();
        let scroll = gtk::ScrolledWindow::builder()
            .child(&picture)
            .vexpand(true)
            .build();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&scroll));
        let header = adw::HeaderBar::builder().show_title(false).build();
        let view = adw::ToolbarView::builder().content(&toasts).build();
        view.add_top_bar(&header);
        let widget = parent.upcast_ref::<gtk::Widget>();
        let dialog = adw::Dialog::builder()
            .title("Picture")
            .child(&view)
            .content_width((widget.width() - AROUND).max(320))
            .content_height((widget.height() - AROUND).max(280))
            .build();
        crate::beside::closes(&dialog);
        let viewer = Rc::new(Viewer {
            dialog,
            picture,
            scroll,
            toasts,
            file: RefCell::default(),
            actual: Cell::new(false),
        });
        let actions = gio::SimpleActionGroup::new();
        let keys = gtk::ShortcutController::new();
        for (name, icon, what, shortcut, run) in [
            (
                "copy",
                "edit-copy-symbolic",
                "Copy (Ctrl+C)",
                "<Ctrl>c",
                Viewer::copy as fn(&Rc<Viewer>),
            ),
            (
                "save",
                "document-save-symbolic",
                "Save (Ctrl+S)",
                "<Ctrl>s",
                Viewer::save,
            ),
            (
                "open",
                "document-open-symbolic",
                "Open in another app",
                "<Ctrl>o",
                Viewer::open,
            ),
        ] {
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate({
                let viewer = Rc::downgrade(&viewer);
                move |_, _| {
                    if let Some(viewer) = viewer.upgrade() {
                        run(&viewer);
                    }
                }
            });
            actions.add_action(&action);
            let button = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(what)
                .action_name(format!("viewer.{name}"))
                .build();
            row::say(&button, what.split(" (").next().unwrap_or(what));
            header.pack_start(&button);
            keys.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(shortcut),
                Some(gtk::NamedAction::new(&format!("viewer.{name}"))),
            ));
        }
        viewer.dialog.insert_action_group("viewer", Some(&actions));
        viewer.dialog.add_controller(keys);
        let click = gtk::GestureClick::new();
        click.connect_released({
            let viewer = Rc::downgrade(&viewer);
            move |_, _, x, y| {
                let Some(viewer) = viewer.upgrade() else {
                    return;
                };
                if viewer.beside(x, y) {
                    viewer.dialog.close();
                } else {
                    viewer.fit(viewer.actual.get());
                }
            }
        });
        viewer.picture.add_controller(click);
        viewer.fit(true);
        viewer.load(url);
        viewer.dialog.present(Some(parent));
        viewer
    }

    /// Shows the picture if it is on this computer, and keeps what is shown if it is not.
    pub fn load(self: &Rc<Self>, url: &str) {
        let Some(file) = art::file(url) else { return };
        let viewer = Rc::downgrade(self);
        art::whole(file.clone(), move |texture| {
            if let (Some(viewer), Some(texture)) = (viewer.upgrade(), texture) {
                viewer.picture.set_paintable(Some(&texture));
                viewer.file.replace(Some(file.clone()));
                if !crate::moving::wanted(&viewer.picture) {
                    return;
                }
                let (shown, weak) = (file.clone(), Rc::downgrade(&viewer));
                art::moving_file(file, (texture.width(), texture.height()), move |frames| {
                    let viewer = weak.upgrade();
                    let still = |viewer: &Rc<Viewer>| viewer.file.borrow().as_ref() == Some(&shown);
                    if let Some(viewer) = viewer.filter(still) {
                        let moving = crate::moving::Moving::new(frames);
                        viewer.picture.set_paintable(Some(&moving));
                    }
                });
            }
        });
    }

    /// As large as fits, or pixel by pixel.
    fn fit(&self, fit: bool) {
        self.actual.set(!fit);
        let policy = if fit {
            gtk::PolicyType::Never
        } else {
            gtk::PolicyType::Automatic
        };
        self.scroll.set_policy(policy, policy);
        self.picture.set_can_shrink(fit);
        self.picture.set_content_fit(if fit {
            gtk::ContentFit::ScaleDown
        } else {
            gtk::ContentFit::Contain
        });
        self.picture
            .set_cursor_from_name(Some(if fit { "zoom-in" } else { "zoom-out" }));
    }

    /// The place is next to the picture, where there is nothing to see.
    fn beside(&self, x: f64, y: f64) -> bool {
        let Some(texture) = self.texture() else {
            return true;
        };
        let room = (
            f64::from(self.picture.width()),
            f64::from(self.picture.height()),
        );
        let size = (f64::from(texture.width()), f64::from(texture.height()));
        outside(room, size, (x, y))
    }

    fn texture(&self) -> Option<gdk::Texture> {
        self.picture.paintable().and_downcast()
    }

    fn say(&self, text: &str) {
        self.toasts
            .add_toast(adw::Toast::new(&glib::markup_escape_text(text)));
    }

    fn copy(self: &Rc<Self>) {
        if let Some(texture) = self.texture() {
            self.picture.clipboard().set_texture(&texture);
            self.say("Copied");
        }
    }

    fn save(self: &Rc<Self>) {
        let Some(file) = self.file.borrow().clone() else {
            return;
        };
        let kind = file.extension().and_then(|kind| kind.to_str());
        let name = match kind {
            Some(kind) if kind != "bin" => format!("picture.{kind}"),
            _ => "picture".to_owned(),
        };
        let dialog = gtk::FileDialog::builder()
            .title("Save picture")
            .initial_name(name)
            .build();
        let window = self.dialog.root().and_downcast::<gtk::Window>();
        let viewer = self.clone();
        glib::spawn_future_local(async move {
            let Ok(place) = dialog.save_future(window.as_ref()).await else {
                return;
            };
            let (copied, _) = gio::File::for_path(file).copy_future(
                &place,
                gio::FileCopyFlags::OVERWRITE | gio::FileCopyFlags::TARGET_DEFAULT_PERMS,
                glib::Priority::DEFAULT,
            );
            match copied.await {
                Ok(()) => viewer.say("Saved"),
                Err(error) => viewer.say(&format!("Not saved: {error}")),
            }
        });
    }

    fn open(self: &Rc<Self>) {
        let Some(file) = self.file.borrow().clone() else {
            return;
        };
        let window = self.dialog.root().and_downcast::<gtk::Window>();
        let viewer = self.clone();
        let file = gio::File::for_path(file);
        gtk::FileLauncher::new(Some(&file)).launch(
            window.as_ref(),
            gio::Cancellable::NONE,
            move |opened| {
                if let Err(error) = opened {
                    viewer.say(&format!("Not opened: {error}"));
                }
            },
        );
    }
}

/// Whether a place is outside of a picture of `size` in the middle of `room`, shrunk to fit.
fn outside(room: (f64, f64), size: (f64, f64), (x, y): (f64, f64)) -> bool {
    let fit = (room.0 / size.0).min(room.1 / size.1).min(1.0);
    let off = |at: f64, room: f64, size: f64| (at - room / 2.0).abs() > size * fit / 2.0;
    off(x, room.0, size.0) || off(y, room.1, size.1)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn what_is_beside_a_picture() {
        let wide = |x, y| outside((600.0, 400.0), (1200.0, 400.0), (x, y));
        assert!(!wide(1.0, 200.0));
        assert!(!wide(300.0, 101.0));
        assert!(wide(300.0, 99.0));
        assert!(wide(300.0, 301.0));
        let small = |x, y| outside((600.0, 400.0), (100.0, 100.0), (x, y));
        assert!(!small(300.0, 200.0));
        assert!(small(240.0, 200.0));
    }

    /// As the composer's test: needs a display.
    #[test]
    #[ignore = "needs a display"]
    fn pictures_open_and_become_whole() {
        adw::init().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(crate::demo::store());
        art::set_source(store, Some(folder.path().to_owned()), 1);
        let url = "https://us-api.asm.skype.com/v1/objects/a/views/imgo";
        let full = tern_engine::full_size(url).unwrap();
        let files = store::ImageFileCache::open(folder.path(), 1 << 30).unwrap();
        let put = |url: &str, width, height| {
            let picture = gtk::gdk_pixbuf::Pixbuf::new(
                gtk::gdk_pixbuf::Colorspace::Rgb,
                false,
                8,
                width,
                height,
            )
            .unwrap();
            picture.fill(0x1f6f_8bff);
            let bytes = picture.save_to_bufferv("png", &[]).unwrap();
            files.put(url, &bytes, "png").unwrap();
        };
        put(url, 600, 400);
        let window = adw::Window::builder()
            .default_width(700)
            .default_height(500)
            .content(&gtk::Box::new(gtk::Orientation::Vertical, 0))
            .build();
        window.present();
        let wait = |milliseconds: u64| {
            let end = Instant::now() + Duration::from_millis(milliseconds);
            while Instant::now() < end {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        wait(300);
        let viewer = Viewer::show(&window, url);
        wait(500);
        let size = || viewer.texture().map(|it| (it.width(), it.height()));
        assert_eq!(size(), Some((600, 400)));
        // Not here: what is shown stays.
        viewer.load(&full);
        wait(200);
        assert_eq!(size(), Some((600, 400)));
        put(&full, 3000, 2000);
        viewer.load(&full);
        wait(800);
        assert_eq!(size(), Some((3000, 2000)), "not made smaller");
        assert_eq!(viewer.file.borrow().clone(), art::file(&full));
        assert!(viewer.picture.width() <= 700, "{}", viewer.picture.width());
        let least = || viewer.picture.measure(gtk::Orientation::Horizontal, -1).0;
        assert!(least() <= 700);
        viewer.fit(false);
        assert_eq!(least(), 3000, "pixel by pixel");
        viewer.fit(true);
        assert!(least() <= 700);
        assert!(viewer.dialog.close());
    }
}
