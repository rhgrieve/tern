//! Emoji and pictures that move.

use std::cell::{Cell, RefCell};
use std::io::Read;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use gtk4::{gdk, gdk_pixbuf, glib};

/// Pictures and how long each is shown.
pub type Frames = Rc<Vec<(gdk::Texture, Duration)>>;

const MOST: usize = 200;
/// What claims more pixels than this is not decoded.
const MAX_PIXELS: i64 = 1_000_000;
/// All pictures of one file may take this much memory; with more, it does not move.
const MOST_BYTES: usize = 48 << 20;
/// How often a window nobody sees is looked at again.
const HIDDEN: Duration = Duration::from_secs(1);
/// What says nothing of its speed, or more than can be seen.
const USUAL: Duration = Duration::from_millis(100);
const FASTEST: Duration = Duration::from_millis(20);

/// How many pictures a GIF is made of; one for everything else.
pub fn count(bytes: &[u8]) -> usize {
    let Some(rest) = bytes.strip_prefix(b"GIF8") else {
        return 1;
    };
    frames_of(rest.get(2..).unwrap_or_default())
        .unwrap_or(1)
        .clamp(1, MOST)
}

/// Walks the blocks after the signature.
fn frames_of(data: &[u8]) -> Option<usize> {
    let table = |flags: u8| {
        if flags & 0x80 == 0 {
            0
        } else {
            3usize << ((flags & 7) + 1)
        }
    };
    let mut at = 7 + table(*data.get(4)?);
    let skip = |mut at: usize| -> Option<usize> {
        loop {
            let length = *data.get(at)? as usize;
            at += 1 + length;
            if length == 0 {
                return Some(at);
            }
        }
    };
    let mut frames = 0;
    loop {
        match data.get(at) {
            Some(0x21) => at = skip(at + 2)?,
            Some(0x2c) => {
                frames += 1;
                let flags = *data.get(at + 9)?;
                at = skip(at + 10 + table(flags) + 1)?;
            }
            // The end, also of what was cut off.
            _ => return Some(frames),
        }
        if frames >= MOST {
            return Some(frames);
        }
    }
}

/// Whether a file is made of several pictures.
pub fn moves(path: &Path) -> bool {
    let mut signature = [0; 4];
    let gif = std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut signature))
        .is_ok_and(|()| signature == *b"GIF8");
    gif && std::fs::read(path).is_ok_and(|bytes| count(&bytes) > 1)
}

/// The pictures of a file, no wider and higher than `size`, as pixels and how long each is shown.
pub fn decode(
    path: &Path,
    (wide, high): (i32, i32),
) -> Option<Vec<(gdk_pixbuf::Pixbuf, Duration)>> {
    let (_, width, height) = gdk_pixbuf::Pixbuf::file_info(path)?;
    if i64::from(width) * i64::from(height) > MAX_PIXELS {
        return None;
    }
    let count = count(&std::fs::read(path).ok()?);
    let small = |picture: gdk_pixbuf::Pixbuf| {
        let fit = (f64::from(wide) / f64::from(picture.width().max(1)))
            .min(f64::from(high) / f64::from(picture.height().max(1)));
        if fit >= 1.0 {
            return Some(picture);
        }
        let scaled = |length: i32| ((f64::from(length) * fit) as i32).max(1);
        picture.scale_simple(
            scaled(picture.width()),
            scaled(picture.height()),
            gdk_pixbuf::InterpType::Bilinear,
        )
    };
    if count == 1 {
        let picture = gdk_pixbuf::Pixbuf::from_file(path).ok()?;
        return Some(vec![(small(picture)?, USUAL)]);
    }
    let moving = gdk_pixbuf::PixbufAnimation::from_file(path).ok()?;
    let mut now = SystemTime::UNIX_EPOCH;
    let at = moving.iter(Some(now));
    let mut frames = Vec::with_capacity(count);
    for _ in 0..count {
        let delay = at.delay_time().unwrap_or(USUAL);
        let delay = if delay < FASTEST { USUAL } else { delay };
        // A copy: the next frame may be drawn into the same place.
        let picture = small(at.pixbuf().copy()?)?;
        if picture.byte_length() * count > MOST_BYTES {
            return None;
        }
        frames.push((picture, delay));
        now += delay;
        at.advance(now);
    }
    Some(frames)
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Moving {
        pub frames: RefCell<Option<Frames>>,
        pub at: Cell<usize>,
        pub waiting: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Moving {
        const NAME: &'static str = "TernMoving";
        type Type = super::Moving;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for Moving {}

    impl Moving {
        fn now(&self) -> Option<gdk::Texture> {
            let frames = self.frames.borrow();
            Some(frames.as_ref()?.get(self.at.get())?.0.clone())
        }

        /// Only what is drawn moves on: nothing happens for what is hidden, or in a window nobody
        /// sees.
        fn go_on(&self) {
            let pause = match self.frames.borrow().as_ref() {
                Some(frames) if frames.len() > 1 => frames[self.at.get()].1,
                _ => return,
            };
            if self.waiting.replace(true) {
                return;
            }
            let moving = self.obj().downgrade();
            glib::timeout_add_local_once(pause, move || {
                let Some(moving) = moving.upgrade() else {
                    return;
                };
                let inner = moving.imp();
                let count = inner.frames.borrow().as_ref().map_or(1, |all| all.len());
                inner.waiting.set(false);
                inner.at.set((inner.at.get() + 1) % count.max(1));
                moving.invalidate_contents();
            });
        }
    }

    impl PaintableImpl for Moving {
        fn current_image(&self) -> gdk::Paintable {
            match self.now() {
                Some(now) => now.upcast(),
                None => gdk::Paintable::new_empty(0, 0),
            }
        }

        fn flags(&self) -> gdk::PaintableFlags {
            gdk::PaintableFlags::STATIC_SIZE
        }

        fn intrinsic_width(&self) -> i32 {
            self.now().map_or(0, |now| now.width())
        }

        fn intrinsic_height(&self) -> i32 {
            self.now().map_or(0, |now| now.height())
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            if let Some(now) = self.now() {
                now.snapshot(snapshot, width, height);
            }
            self.go_on();
        }
    }
}

glib::wrapper! {
    /// A picture that moves wherever it is shown.
    pub struct Moving(ObjectSubclass<imp::Moving>) @implements gdk::Paintable;
}

impl Moving {
    pub fn new(frames: Frames) -> Moving {
        let moving: Moving = glib::Object::new();
        moving.imp().frames.replace(Some(frames));
        moving
    }
}

/// Who does not want things to move has said so.
pub fn wanted(widget: &impl IsA<gtk::Widget>) -> bool {
    widget.settings().is_gtk_enable_animations()
}

/// Shows the frames one after the other while the image is on screen.
pub fn play(image: &gtk::Image, frames: Frames) {
    let Some((first, _)) = frames.first() else {
        return;
    };
    image.set_paintable(Some(first));
    if frames.len() < 2 || !wanted(image) {
        return;
    }
    let running = Rc::new(Cell::new(false));
    let start = {
        let running = running.clone();
        move |image: &gtk::Image| {
            if image.is_mapped() && !running.replace(true) {
                next(image.downgrade(), frames.clone(), 0, running.clone());
            }
        }
    };
    start(image);
    image.connect_map(start);
}

fn next(image: glib::WeakRef<gtk::Image>, frames: Frames, at: usize, running: Rc<Cell<bool>>) {
    let hidden = image
        .upgrade()
        .and_then(|image| image.root().and_downcast::<gtk::Window>())
        .is_some_and(|window| window.is_suspended());
    let pause = if hidden { HIDDEN } else { frames[at].1 };
    glib::timeout_add_local_once(pause, move || {
        let Some(shown) = image.upgrade().filter(WidgetExt::is_mapped) else {
            return running.set(false);
        };
        let at = if hidden { at } else { (at + 1) % frames.len() };
        shown.set_paintable(Some(&frames[at].0));
        next(image, frames, at, running);
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A GIF of `frames` one-pixel pictures.
    pub fn gif(frames: usize) -> Vec<u8> {
        let mut out = b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff".to_vec();
        for _ in 0..frames {
            out.extend(b"\x21\xf9\x04\x04\x05\0\0\0");
            out.extend(b"\x2c\0\0\0\0\x01\0\x01\0\0\x02\x02\x44\x01\0");
        }
        out.push(0x3b);
        out
    }

    #[test]
    fn frames_are_counted() {
        assert_eq!(count(&gif(1)), 1);
        assert_eq!(count(&gif(12)), 12);
        assert_eq!(count(&gif(1000)), MOST);
        assert_eq!(count(b"\x89PNG\r\n\x1a\n"), 1);
        assert_eq!(count(b"GIF8"), 1);
        // Cut off in the middle.
        let whole = gif(5);
        for length in 0..whole.len() {
            assert!((1..=5).contains(&count(&whole[..length])));
        }
    }

    /// As the composer's test: needs a display.
    #[test]
    #[ignore = "needs a display"]
    fn what_moves_is_seen_to() {
        use std::time::Instant;
        gtk::init().unwrap();
        let picture = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, 4, 4).unwrap();
        let frame = || {
            (
                gdk::Texture::for_pixbuf(&picture),
                Duration::from_millis(20),
            )
        };
        let frames: Frames = Rc::new((0..3).map(|_| frame()).collect());
        let image = gtk::Image::new();
        let window = gtk::Window::builder().child(&image).build();
        play(&image, frames.clone());
        window.present();
        let mut seen = vec![];
        let end = Instant::now() + Duration::from_secs(20);
        while seen.len() < 3 && Instant::now() < end {
            glib::MainContext::default().iteration(false);
            let shown = image.paintable();
            let at = frames
                .iter()
                .position(|(frame, _)| shown.as_ref() == Some(frame.upcast_ref()));
            if let Some(at) = at.filter(|at| !seen.contains(at)) {
                seen.push(at);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        seen.sort();
        assert_eq!(seen, [0, 1, 2]);
    }

    /// As above.
    #[test]
    #[ignore = "needs a display"]
    fn a_picture_moves_while_it_is_drawn_and_only_then() {
        use std::time::Instant;
        gtk::init().unwrap();
        let pixels = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, 4, 4).unwrap();
        let frame = || (gdk::Texture::for_pixbuf(&pixels), Duration::from_millis(20));
        let moving = Moving::new(Rc::new((0..3).map(|_| frame()).collect()));
        let picture = gtk::Picture::for_paintable(&moving);
        let window = gtk::Window::builder().child(&picture).build();
        let steps = |milliseconds: u64| {
            let mut seen = std::collections::HashSet::new();
            let end = Instant::now() + Duration::from_millis(milliseconds);
            while Instant::now() < end {
                glib::MainContext::default().iteration(false);
                seen.insert(moving.imp().at.get());
                std::thread::sleep(Duration::from_millis(2));
            }
            seen.len()
        };
        assert_eq!(steps(200), 1, "not shown yet");
        window.present();
        assert_eq!(steps(3000), 3);
        window.set_visible(false);
        steps(100);
        assert_eq!(steps(300), 1, "hidden");
        window.present();
        assert_eq!(steps(3000), 3);
    }

    #[test]
    fn what_moves_is_decoded() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("a.gif");
        std::fs::write(&path, gif(3)).unwrap();
        assert!(moves(&path));
        let frames = decode(&path, (36, 36)).unwrap();
        assert_eq!(frames.len(), 3);
        assert!(
            frames
                .iter()
                .all(|(_, delay)| *delay == Duration::from_millis(50))
        );
        std::fs::write(&path, b"nothing").unwrap();
        assert!(decode(&path, (36, 36)).is_none() && !moves(&path));
        std::fs::write(&path, gif(1)).unwrap();
        assert!(!moves(&path));
    }
}
