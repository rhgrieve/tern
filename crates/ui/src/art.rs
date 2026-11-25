//! Faces and pictures from the cache, decoded off the main thread at the size they are shown.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk4::prelude::*;
use gtk4::{gdk, gdk_pixbuf, gio, glib};
use store::Store;

use crate::moving::Frames;

pub const WIDTH: i32 = 420;
pub const HEIGHT: i32 = 320;
/// Decoded pictures kept; a screen shows a handful.
const KEEP: usize = 60;
/// A picture claiming more pixels than this is not decoded.
const MAX_PIXELS: i64 = 80_000_000;

#[derive(Default)]
struct Art {
    store: Option<Arc<Store>>,
    folder: Option<PathBuf>,
    faces: HashMap<String, Option<gdk::Texture>>,
    pictures: HashMap<String, gdk::Texture>,
    order: Vec<String>,
    emoji: HashMap<String, Frames>,
    /// Since when pictures are waited for; none where that was given up.
    waiting: HashMap<String, Option<Instant>>,
    scale: i32,
}

thread_local! {
    static ART: RefCell<Art> = RefCell::default();
}

pub fn set_source(store: Arc<Store>, folder: Option<PathBuf>, scale: i32) {
    ART.with_borrow_mut(|art| {
        *art = Art {
            store: Some(store),
            folder,
            scale: scale.max(1),
            ..Default::default()
        };
    });
}

/// New faces may have arrived.
pub fn forget_faces() {
    // Those we have stay: they would only flicker.
    ART.with_borrow_mut(|art| art.faces.retain(|_, face| face.is_some()));
}

/// Faces are drawn at 40 points or less.
const FACE: i32 = 40;

fn decode_face(bytes: &[u8], scale: i32) -> Option<Decoded> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    // Ends the loader whatever happens, as it insists on.
    let written = loader.write(bytes);
    let closed = loader.close();
    written.ok()?;
    closed.ok()?;
    let picture = loader.pixbuf()?;
    let size = FACE * scale;
    let picture = if picture.width() > size || picture.height() > size {
        let fit = f64::from(size) / f64::from(picture.width().min(picture.height()).max(1));
        let scaled = |length: i32| ((f64::from(length) * fit) as i32).max(1);
        picture.scale_simple(
            scaled(picture.width()),
            scaled(picture.height()),
            gdk_pixbuf::InterpType::Bilinear,
        )?
    } else {
        picture
    };
    Some(Decoded::of(&picture))
}

/// The face of `user` when it is ready: `None` while it is not.
pub fn face(user: &str) -> Option<gdk::Texture> {
    ART.with_borrow(|art| art.faces.get(user).cloned().flatten())
}

/// Reads and decodes faces off the main thread, then calls `ready` if any is new.
pub fn load_faces(users: Vec<String>, ready: impl FnOnce(&[String]) + 'static) {
    let (store, scale, wanted) = ART.with_borrow_mut(|art| {
        let wanted: Vec<String> = users
            .into_iter()
            .filter(|user| !art.faces.contains_key(user))
            .collect();
        // Asked for once, whatever comes of it.
        for user in &wanted {
            art.faces.insert(user.clone(), None);
        }
        (art.store.clone(), art.scale.max(1), wanted)
    });
    let Some(store) = store.filter(|_| !wanted.is_empty()) else {
        return;
    };
    glib::spawn_future_local(async move {
        let decoded = gio::spawn_blocking(move || {
            wanted
                .into_iter()
                .filter_map(|user| {
                    let bytes = store.avatar(&user).ok().flatten()?.bytes?;
                    Some((user, decode_face(&bytes, scale)?))
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        let mut loaded = Vec::new();
        ART.with_borrow_mut(|art| {
            for (user, face) in decoded {
                art.faces.insert(user.clone(), Some(face.texture()));
                loaded.push(user);
            }
        });
        if !loaded.is_empty() {
            ready(&loaded);
        }
    });
}

struct Decoded {
    bytes: Vec<u8>,
    width: i32,
    height: i32,
    stride: usize,
    alpha: bool,
}

impl Decoded {
    fn of(picture: &gdk_pixbuf::Pixbuf) -> Decoded {
        Decoded {
            bytes: picture.read_pixel_bytes().to_vec(),
            width: picture.width(),
            height: picture.height(),
            stride: picture.rowstride() as usize,
            alpha: picture.has_alpha(),
        }
    }

    fn texture(self) -> gdk::Texture {
        let format = if self.alpha {
            gdk::MemoryFormat::R8g8b8a8
        } else {
            gdk::MemoryFormat::R8g8b8
        };
        let bytes = glib::Bytes::from_owned(self.bytes);
        gdk::MemoryTexture::new(self.width, self.height, format, &bytes, self.stride).upcast()
    }
}

fn decode(path: &std::path::Path, scale: i32) -> Option<Decoded> {
    let (_, width, height) = gdk_pixbuf::Pixbuf::file_info(path)?;
    if i64::from(width) * i64::from(height) > MAX_PIXELS {
        return None;
    }
    let (most_wide, most_high) = (WIDTH * scale, HEIGHT * scale);
    let picture = if width > most_wide || height > most_high {
        gdk_pixbuf::Pixbuf::from_file_at_scale(path, most_wide, most_high, true)
    } else {
        gdk_pixbuf::Pixbuf::from_file(path)
    }
    .ok()?;
    Some(Decoded::of(&picture))
}

/// Emoji are drawn at 18 points.
const EMOJI: i32 = 18;

/// The pictures of an emoji, once: at once when they are decoded already; none when it is not here.
pub fn emoji(url: &str, show: impl FnOnce(Option<Frames>) + 'static) {
    if let Some(known) = ART.with_borrow(|art| art.emoji.get(url).cloned()) {
        return show(Some(known));
    }
    let Some(path) = file(url) else {
        return show(None);
    };
    let size = EMOJI * scale();
    let url = url.to_owned();
    glib::spawn_future_local(async move {
        let decoded = gio::spawn_blocking(move || {
            let frames = crate::moving::decode(&path, size)?;
            let frames = frames
                .iter()
                .map(|(picture, delay)| (Decoded::of(picture), *delay));
            Some(frames.collect::<Vec<_>>())
        })
        .await
        .ok()
        .flatten();
        let frames = decoded.map(|frames| {
            let frames = frames
                .into_iter()
                .map(|(picture, delay)| (picture.texture(), delay));
            Frames::new(frames.collect())
        });
        if let Some(frames) = &frames {
            ART.with_borrow_mut(|art| art.emoji.insert(url, frames.clone()));
        }
        show(frames);
    });
}

/// Gives the picture to `show`, at once when it is decoded already; `None` when it is not cached.
pub fn picture(url: &str, show: impl FnOnce(Option<&gdk::Texture>) + 'static) {
    let (known, path, scale) = ART.with_borrow(|art| {
        let path = art
            .folder
            .as_ref()
            .and_then(|folder| store::find_image_file(folder, url));
        (art.pictures.get(url).cloned(), path, art.scale.max(1))
    });
    if known.is_some() {
        return show(known.as_ref());
    }
    let Some(path) = path else {
        return show(None);
    };
    let url = url.to_owned();
    glib::spawn_future_local(async move {
        let decoded = gio::spawn_blocking(move || decode(&path, scale))
            .await
            .ok()
            .flatten();
        let texture = decoded.map(Decoded::texture);
        if let Some(texture) = &texture {
            ART.with_borrow_mut(|art| {
                art.pictures.insert(url.clone(), texture.clone());
                art.order.push(url);
                if art.order.len() > KEEP {
                    let oldest = art.order.remove(0);
                    art.pictures.remove(&oldest);
                }
            });
        }
        show(texture.as_ref());
    });
}

/// A picture is waited for this long.
const PATIENCE: Duration = Duration::from_secs(20);

/// How much longer a picture is waited for; none when it did not come. It may come yet.
pub fn wait_for(url: &str) -> Option<Duration> {
    ART.with_borrow_mut(|art| {
        let since = *art
            .waiting
            .entry(url.to_owned())
            .or_insert_with(|| Some(Instant::now()));
        PATIENCE
            .checked_sub(since?.elapsed())
            .filter(|left| !left.is_zero())
    })
}

/// It was asked for again.
pub fn wait_again(url: &str) {
    ART.with_borrow_mut(|art| {
        art.waiting.insert(url.to_owned(), Some(Instant::now()));
    });
}

/// It will not come.
pub fn give_up(url: &str) {
    ART.with_borrow_mut(|art| {
        art.waiting.insert(url.to_owned(), None);
    });
}

/// The file of a picture that is on this computer.
pub fn file(url: &str) -> Option<PathBuf> {
    ART.with_borrow(|art| store::find_image_file(art.folder.as_ref()?, url))
}

/// Decodes a picture in full, off the main thread.
pub fn whole(file: PathBuf, show: impl FnOnce(Option<gdk::Texture>) + 'static) {
    glib::spawn_future_local(async move {
        // At no scale is it made smaller.
        let decoded = gio::spawn_blocking(move || decode(&file, i32::MAX / WIDTH.max(HEIGHT)))
            .await
            .ok()
            .flatten();
        show(decoded.map(Decoded::texture));
    });
}

/// The picture is on this computer.
pub fn has_picture(url: &str) -> bool {
    ART.with_borrow(|art| {
        art.pictures.contains_key(url)
            || art
                .folder
                .as_ref()
                .is_some_and(|folder| store::find_image_file(folder, url).is_some())
    })
}

/// How large a picture is shown that says how large it is.
pub fn room(size: Option<(i32, i32)>) -> Option<(i32, i32)> {
    size.map(|(width, height)| shown_size(width, height, 1))
}

/// How large a picture of `width` by `height` pixels is shown.
pub fn shown_size(width: i32, height: i32, scale: i32) -> (i32, i32) {
    let (width, height) = (
        f64::from(width.max(1)) / f64::from(scale.max(1)),
        f64::from(height.max(1)) / f64::from(scale.max(1)),
    );
    let fit = (f64::from(WIDTH) / width)
        .min(f64::from(HEIGHT) / height)
        .min(1.0);
    (
        ((width * fit) as i32).max(24),
        ((height * fit) as i32).max(24),
    )
}

pub fn scale() -> i32 {
    ART.with_borrow(|art| art.scale.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_fit_and_small_ones_are_not_blown_up() {
        assert_eq!(shown_size(840, 640, 2), (420, 320));
        assert_eq!(shown_size(100, 80, 2), (50, 40));
        assert_eq!(shown_size(4000, 100, 1), (420, 24));
        assert_eq!(shown_size(0, 0, 0), (24, 24));
        assert_eq!(room(Some((840, 640))), Some((420, 320)));
        assert_eq!(room(Some((100, 80))), Some((100, 80)));
        assert_eq!(room(None), None);
    }

    #[test]
    fn pictures_are_waited_for_until_given_up() {
        assert!(wait_for("a").is_some());
        give_up("a");
        assert_eq!(wait_for("a"), None);
        wait_again("a");
        assert!(wait_for("a").is_some());
    }

    #[test]
    fn faces_shrink_to_what_is_drawn() {
        let picture =
            gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, 240, 120).unwrap();
        picture.fill(0x3366_99ff);
        let bytes = picture.save_to_bufferv("png", &[]).unwrap();
        let face = decode_face(&bytes, 2).unwrap();
        assert_eq!(
            (face.width, face.height),
            (160, 80),
            "covers the circle, no more"
        );
        assert!(decode_face(b"junk", 2).is_none());
    }

    #[test]
    fn junk_and_bombs_are_not_decoded() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("x.png");
        std::fs::write(&path, b"not a picture").unwrap();
        assert!(decode(&path, 1).is_none());
        // A PNG header that claims 60,000 by 60,000 pixels.
        let mut header = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        header.extend(60_000u32.to_be_bytes());
        header.extend(60_000u32.to_be_bytes());
        header.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
        std::fs::write(&path, header).unwrap();
        assert!(decode(&path, 1).is_none());
    }
}
