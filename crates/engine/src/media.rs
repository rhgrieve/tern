//! Pictures and files kept for offline use. Pictures of recent chats are fetched ahead; files
//! only when the user asks for one.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};
use store::MessageRecord;
use teams_core::{ImageRef, images};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::backend::Failure;

pub const IMAGE_BYTES: u64 = 1 << 30;
pub const FILE_BYTES: u64 = 1 << 30;
const PARALLEL: usize = 2;
const TRIES: u32 = 4;
const RETRY: Duration = Duration::from_secs(15);
/// A copy this young is used without asking whether the file changed.
const FRESH: Duration = Duration::from_secs(60 * 60);

/// Whether a picture may be fetched before the user looks at it. Only what Teams itself stores:
/// fetching anything else would tell its owner that, when and where the message arrived.
pub fn may_prefetch(url: &str) -> bool {
    let ams = chatsvc::hosted_content_image_url(url);
    let url = ams.as_deref().unwrap_or(url);
    let Some((host, path)) = url
        .strip_prefix("https://")
        .and_then(|rest| rest.split_once('/'))
    else {
        return false;
    };
    // Teams' gateway also serves its own copy of the picture of a linked page: the page's owner
    // does not learn who looks at it.
    let stored = (path.starts_with("v1/objects/") || path.starts_with("urlp/v1/url/image"))
        && session::skype_token_allowed(url);
    let channel = host == "graph.microsoft.com" && path.contains("/hostedContents/");
    stored || channel
}

/// An emoji of the organization's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emoji {
    pub name: String,
    pub url: String,
}

/// The organization's emoji a reaction is, if it is one.
pub fn custom_emoji(reaction: &str) -> Option<Emoji> {
    let emoji = chatsvc::custom_emoji(reaction)?;
    Some(Emoji {
        name: emoji.name.to_owned(),
        // Every gateway serves the objects of all regions; this is the one for old clients, which
        // takes the same token as pictures in messages.
        url: format!(
            "https://us-api.asm.skype.com/v1/objects/{}/views/imgt2_anim",
            emoji.object_id
        ),
    })
}

/// The address of a picture in full, where messages show a smaller one.
pub fn full_size(url: &str) -> Option<String> {
    let full = chatsvc::ams_full_size_url(url)?;
    (full != url && may_prefetch(&full)).then_some(full)
}

/// Pictures worth fetching ahead in a message, and in the reactions to it.
pub fn pictures(message: &MessageRecord) -> Vec<ImageRef> {
    let mut found = Vec::new();
    if message.body_html.contains('<') {
        found.extend(images(message));
    }
    if message.reactions_json.contains(';') && !message.deleted {
        let reactions = teams_core::reactions(message);
        for emoji in reactions
            .iter()
            .filter_map(|reaction| custom_emoji(&reaction.reaction_type))
        {
            if !found.iter().any(|image| image.url == emoji.url) {
                found.push(ImageRef {
                    id: emoji.url.clone(),
                    url: emoji.url,
                    width: None,
                    height: None,
                    alt: Some(emoji.name),
                });
            }
        }
    }
    if message.links_json.len() > "[]".len() {
        found.extend(teams_core::link_preview(message).and_then(|preview| preview.image()));
    }
    found.retain(|image| may_prefetch(&image.url));
    found
}

pub trait Pictures: Send + Sync + 'static {
    fn has(&self, image: &ImageRef) -> bool;
    fn fetch(&self, image: &ImageRef) -> impl Future<Output = Result<(), Failure>> + Send;
    fn avatars(&self, people: &[String]) -> impl Future<Output = Result<(), Failure>> + Send;
}

#[derive(Debug, Clone)]
pub enum Want {
    Pictures(Vec<ImageRef>),
    Avatars(Vec<String>),
}

struct Waiting {
    image: ImageRef,
    tries: u32,
    not_before: Instant,
}

/// Fetches what is wanted, newest request first, a few at a time, until the sender is dropped.
pub async fn prefetch<P: Pictures>(source: Arc<P>, mut wanted: mpsc::UnboundedReceiver<Want>) {
    let mut queue: VecDeque<Waiting> = VecDeque::new();
    let mut known: HashSet<String> = HashSet::new();
    let mut running: JoinSet<(Waiting, Result<(), Failure>)> = JoinSet::new();
    loop {
        let now = Instant::now();
        while running.len() < PARALLEL
            && let Some(index) = queue.iter().position(|waiting| waiting.not_before <= now)
            && let Some(next) = queue.remove(index)
        {
            let source = source.clone();
            running.spawn(async move {
                let outcome = source.fetch(&next.image).await;
                (next, outcome)
            });
        }
        let wake = if running.len() < PARALLEL {
            queue.iter().map(|waiting| waiting.not_before).min()
        } else {
            None
        };
        tokio::select! {
            want = wanted.recv() => match want {
                None => return,
                Some(Want::Avatars(people)) => {
                    if let Err(failure) = source.avatars(&people).await {
                        tracing::debug!(error = failure.text(), "avatars");
                    }
                }
                Some(Want::Pictures(images)) => {
                    for image in images {
                        if !source.has(&image) && known.insert(image.key().to_owned()) {
                            queue.push_front(Waiting { image, tries: 0, not_before: now });
                        }
                    }
                }
            },
            Some(done) = running.join_next() => {
                let Ok((mut waiting, outcome)) = done else { continue };
                waiting.tries += 1;
                match outcome {
                    Err(Failure::Temporary(error)) if waiting.tries < TRIES => {
                        tracing::debug!(error, "picture waits");
                        waiting.not_before = Instant::now() + RETRY * waiting.tries;
                        queue.push_back(waiting);
                    }
                    // Asked for again later, it gets another chance.
                    Err(Failure::Temporary(_)) => {
                        known.remove(waiting.image.key());
                    }
                    Err(Failure::Permanent(error)) => tracing::debug!(error, "picture skipped"),
                    Ok(()) => {}
                }
            }
            () = async {
                match wake {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {}
        }
    }
}

/// Downloads a file piece by piece into `write`; answers its name.
pub trait FileSource: Send + Sync + 'static {
    fn download(
        &self,
        url: &str,
        write: impl FnMut(&[u8]) -> io::Result<()> + Send,
    ) -> impl Future<Output = Result<String, Failure>> + Send;
}

pub struct Files {
    folder: PathBuf,
    max_bytes: u64,
    running: Mutex<HashSet<String>>,
}

/// A file name from a message, safe to create and to show.
pub fn safe_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .filter(|letter| {
            let reorders = matches!(*letter, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}');
            !letter.is_control() && !reorders
        })
        .map(|letter| if matches!(letter, '/' | '\\') { '_' } else { letter })
        .collect();
    let clean = clean.trim().trim_start_matches(['.', '-']).trim();
    if clean.is_empty() {
        return "file".into();
    }
    if clean.len() <= 150 {
        return clean.to_owned();
    }
    // The end says what kind of file it is.
    let (stem, kind) = clean.rsplit_once('.').unwrap_or((clean, ""));
    let kind: String = kind.chars().take(12).collect();
    let mut short = String::new();
    for letter in stem.chars() {
        if short.len() + letter.len_utf8() > 130 {
            break;
        }
        short.push(letter);
    }
    if kind.is_empty() {
        short
    } else {
        format!("{short}.{kind}")
    }
}

pub(crate) fn private_folder(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

struct Running<'a>(&'a Files, String);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.running.lock().expect("files").remove(&self.1);
    }
}

impl Files {
    pub fn open(folder: &Path, max_bytes: u64) -> io::Result<Self> {
        private_folder(folder)?;
        let files = Files {
            folder: folder.to_owned(),
            max_bytes,
            running: Mutex::default(),
        };
        for (_, path, _) in files.all() {
            if path.extension().is_some_and(|kind| kind == "part") {
                let _ = fs::remove_file(path);
            }
        }
        Ok(files)
    }

    fn slot(&self, url: &str) -> PathBuf {
        let hash = Sha256::digest(url.as_bytes());
        let name: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
        self.folder.join(name)
    }

    /// The copy of `url`, if there is one, and its age.
    pub fn cached(&self, url: &str) -> Option<(PathBuf, Duration)> {
        let file = fs::read_dir(self.slot(url))
            .ok()?
            .flatten()
            .find(|file| !file.file_name().to_string_lossy().ends_with(".part"))?;
        let age = file
            .metadata()
            .ok()?
            .modified()
            .ok()?
            .elapsed()
            .unwrap_or_default();
        Some((file.path(), age))
    }

    /// Every file with its size and last use.
    fn all(&self) -> Vec<(u64, PathBuf, SystemTime)> {
        let slots = fs::read_dir(&self.folder).into_iter().flatten().flatten();
        slots
            .filter_map(|slot| fs::read_dir(slot.path()).ok())
            .flatten()
            .flatten()
            .filter_map(|file| {
                let data = file.metadata().ok()?;
                Some((
                    data.len(),
                    file.path(),
                    data.accessed().or(data.modified()).ok()?,
                ))
            })
            .collect()
    }

    /// The file behind `url`, downloaded when there is no recent copy. Without a connection an
    /// older copy has to do.
    pub async fn get(&self, url: &str, source: &impl FileSource) -> Result<PathBuf, String> {
        let cached = self.cached(url);
        if let Some((path, age)) = &cached
            && *age < FRESH
        {
            return Ok(path.clone());
        }
        if !self.running.lock().expect("files").insert(url.to_owned()) {
            return Err("this file is already being downloaded".into());
        }
        let _running = Running(self, url.to_owned());
        match self.download(url, source).await {
            Ok(path) => {
                if let Some((old, _)) = cached.filter(|(old, _)| *old != path) {
                    let _ = fs::remove_file(old);
                }
                self.evict(&path);
                Ok(path)
            }
            Err(Failure::Temporary(_)) if cached.is_some() => Ok(cached.expect("checked").0),
            Err(failure) => Err(failure.text().to_owned()),
        }
    }

    async fn download(&self, url: &str, source: &impl FileSource) -> Result<PathBuf, Failure> {
        let disk = |error: io::Error| Failure::Permanent(error.to_string());
        let slot = self.slot(url);
        private_folder(&slot).map_err(disk)?;
        let part = slot.join("download.part");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&part)
            .map_err(disk)?;
        let limit = self.max_bytes;
        let mut written = 0u64;
        let outcome = source
            .download(url, |bytes| {
                written += bytes.len() as u64;
                if written > limit {
                    return Err(io::Error::other("the file is larger than the cache"));
                }
                file.write_all(bytes)
            })
            .await;
        let name = match outcome {
            Ok(name) => name,
            Err(failure) => {
                let _ = fs::remove_file(&part);
                return Err(failure);
            }
        };
        let path = slot.join(safe_name(&name));
        fs::rename(&part, &path).map_err(disk)?;
        Ok(path)
    }

    /// Removes the least recently used files until the rest fits.
    fn evict(&self, keep: &Path) {
        let mut files = self.all();
        let mut total: u64 = files.iter().map(|(size, _, _)| size).sum();
        files.sort_by_key(|(_, _, used)| *used);
        for (size, path, _) in files {
            if total <= self.max_bytes {
                return;
            }
            let part = path.extension().is_some_and(|kind| kind == "part");
            if path != keep && !part && fs::remove_file(&path).is_ok() {
                total -= size;
                if let Some(slot) = path.parent() {
                    let _ = fs::remove_dir(slot);
                }
            }
        }
    }
}

/// How many files a folder tree holds, and their bytes.
pub fn usage(folder: &Path) -> (usize, u64) {
    let mut sum = (0, 0);
    for entry in fs::read_dir(folder).into_iter().flatten().flatten() {
        let Ok(data) = entry.metadata() else { continue };
        if data.is_dir() {
            let (count, bytes) = usage(&entry.path());
            sum = (sum.0 + count, sum.1 + bytes);
        } else {
            sum = (sum.0 + 1, sum.1 + data.len());
        }
    }
    sum
}

/// The people seen most often come first when there are more than `limit`.
pub fn people<'a>(senders: impl Iterator<Item = &'a str>, limit: usize) -> Vec<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for sender in senders {
        *counts.entry(sender).or_default() += 1;
    }
    let mut all: Vec<(&str, usize)> = counts.into_iter().collect();
    all.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    all.into_iter()
        .take(limit)
        .map(|(id, _)| id.to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_of_the_organization_are_fetched_once() {
        let reaction = |kind: &str| format!(r#"{{"reaction_type":"{kind}","user_id":"a"}}"#);
        let message = MessageRecord {
            body_html: "<p>hi</p>".into(),
            reactions_json: format!(
                "[{},{},{},{}]",
                reaction("like"),
                reaction("party;0-eus-d12-abc"),
                reaction("party;0-eus-d12-abc"),
                reaction("x;../evil")
            ),
            ..Default::default()
        };
        let found = pictures(&message);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].url,
            custom_emoji("party;0-eus-d12-abc").unwrap().url
        );
        assert!(may_prefetch(&found[0].url));
        assert_eq!(custom_emoji("like"), None);
    }

    fn image(url: &str) -> ImageRef {
        ImageRef {
            id: url.into(),
            url: url.into(),
            width: None,
            height: None,
            alt: None,
        }
    }

    #[test]
    fn only_what_teams_stores_is_fetched_ahead() {
        for good in [
            "https://us-api.asm.skype.com/v1/objects/0-eus-d1/views/imgo",
            "https://us-prod.asyncgw.teams.microsoft.com/v1/objects/x/views/imgo",
            "https://graph.microsoft.com/v1.0/teams/t/channels/c/messages/m/hostedContents/x/$value",
        ] {
            assert!(may_prefetch(good), "{good}");
        }
        for bad in [
            "https://tracker.example/pixel.gif",
            "https://media0.giphy.com/media/a/giphy.gif",
            "https://us-prod.asyncgw.teams.microsoft.com/urlp/v1/url/content?url=https%3A%2F%2Ftracker.example",
            "https://tracker.example/v1/objects/x/views/imgo",
            "https://us-api.asm.skype.com.tracker.example/v1/objects/x",
            "https://graph.microsoft.com/v1.0/me/photo",
            "http://us-api.asm.skype.com/v1/objects/x",
        ] {
            assert!(!may_prefetch(bad), "{bad}");
        }
    }

    #[test]
    fn names_are_made_safe() {
        for (given, safe) in [
            ("Plan.docx", "Plan.docx"),
            ("../../.bashrc", "_.._.bashrc"),
            (".profile", "profile"),
            ("-rf", "rf"),
            ("a/b\\c.txt", "a_b_c.txt"),
            ("invoice\u{202e}fdp.exe", "invoicefdp.exe"),
            ("x\n\0y", "xy"),
            ("  ", "file"),
            ("..", "file"),
        ] {
            assert_eq!(safe_name(given), safe, "{given:?}");
        }
        let long = safe_name(&format!("{}.pdf", "ü".repeat(300)));
        assert!(long.len() <= 150 && long.ends_with(".pdf"));
    }

    #[derive(Default)]
    struct Shop {
        have: Mutex<HashSet<String>>,
        failing: Mutex<HashMap<String, u32>>,
        tries: Mutex<Vec<String>>,
        most: Mutex<(usize, usize)>,
    }

    impl Pictures for Shop {
        fn has(&self, image: &ImageRef) -> bool {
            self.have.lock().unwrap().contains(image.key())
        }
        async fn fetch(&self, image: &ImageRef) -> Result<(), Failure> {
            {
                let mut most = self.most.lock().unwrap();
                most.0 += 1;
                most.1 = most.1.max(most.0);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            self.most.lock().unwrap().0 -= 1;
            self.tries.lock().unwrap().push(image.url.clone());
            if image.url == "gone" {
                return Err(Failure::Permanent("HTTP 404".into()));
            }
            if let Some(left) = self.failing.lock().unwrap().get_mut(&image.url)
                && *left > 0
            {
                *left -= 1;
                return Err(Failure::Temporary("HTTP 503".into()));
            }
            self.have.lock().unwrap().insert(image.url.clone());
            Ok(())
        }
        async fn avatars(&self, _: &[String]) -> Result<(), Failure> {
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn pictures_are_fetched_once_and_retried() {
        let shop = Arc::new(Shop::default());
        shop.failing.lock().unwrap().insert("flaky".into(), 2);
        let (want, wanted) = mpsc::unbounded_channel();
        tokio::spawn(prefetch(shop.clone(), wanted));
        let all: Vec<ImageRef> = ["a", "b", "c", "flaky", "gone"]
            .iter()
            .map(|url| image(url))
            .collect();
        for _ in 0..3 {
            want.send(Want::Pictures(all.clone())).unwrap();
        }
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_eq!(shop.have.lock().unwrap().len(), 4);
        let tries = shop.tries.lock().unwrap();
        let count = |url: &str| tries.iter().filter(|tried| *tried == url).count();
        assert_eq!((count("a"), count("flaky"), count("gone")), (1, 3, 1));
        assert!(shop.most.lock().unwrap().1 <= PARALLEL);
    }

    struct Drive {
        name: &'static str,
        bytes: usize,
        fails: Option<Failure>,
    }

    impl FileSource for Drive {
        async fn download(
            &self,
            _: &str,
            mut write: impl FnMut(&[u8]) -> io::Result<()> + Send,
        ) -> Result<String, Failure> {
            for _ in 0..self.bytes {
                write(b"x").map_err(|error| Failure::Permanent(error.to_string()))?;
            }
            match &self.fails {
                Some(failure) => Err(failure.clone()),
                None => Ok(self.name.into()),
            }
        }
    }

    fn drive(name: &'static str, bytes: usize) -> Drive {
        Drive {
            name,
            bytes,
            fails: None,
        }
    }

    fn age(path: &Path, hours: u64) {
        let then = SystemTime::now() - Duration::from_secs(hours * 3600);
        let file = fs::File::open(path).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(then).set_accessed(then))
            .unwrap();
    }

    #[tokio::test]
    async fn a_file_is_private_and_stays_inside_the_cache() {
        let folder = tempfile::tempdir().unwrap();
        let files = Files::open(folder.path(), 100).unwrap();
        let path = files
            .get("https://x/1", &drive("../../evil", 10))
            .await
            .unwrap();
        assert!(path.starts_with(folder.path()));
        assert_eq!(path.parent().unwrap().parent().unwrap(), folder.path());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Fresh: not downloaded again.
        let failing = Drive {
            fails: Some(Failure::Permanent("no".into())),
            ..drive("x", 1)
        };
        assert_eq!(files.get("https://x/1", &failing).await.unwrap(), path);
    }

    #[tokio::test]
    async fn an_old_copy_is_renewed_or_kept_offline() {
        let folder = tempfile::tempdir().unwrap();
        let files = Files::open(folder.path(), 100).unwrap();
        let old = files.get("u", &drive("a.txt", 5)).await.unwrap();
        age(&old, 2);
        let offline = Drive {
            fails: Some(Failure::Temporary("offline".into())),
            ..drive("x", 3)
        };
        assert_eq!(files.get("u", &offline).await.unwrap(), old);
        assert_eq!(
            fs::metadata(&old).unwrap().len(),
            5,
            "a broken download does not harm it"
        );
        let gone = Drive {
            fails: Some(Failure::Permanent("HTTP 403".into())),
            ..drive("x", 0)
        };
        assert_eq!(files.get("u", &gone).await, Err("HTTP 403".into()));
        let renamed = files.get("u", &drive("b.txt", 7)).await.unwrap();
        assert!(renamed.ends_with("b.txt") && !old.exists());
    }

    #[tokio::test]
    async fn the_oldest_files_make_room() {
        let folder = tempfile::tempdir().unwrap();
        let files = Files::open(folder.path(), 100).unwrap();
        let first = files.get("1", &drive("1", 40)).await.unwrap();
        age(&first, 5);
        let second = files.get("2", &drive("2", 40)).await.unwrap();
        age(&second, 3);
        let third = files.get("3", &drive("3", 40)).await.unwrap();
        assert!(!first.exists() && second.exists() && third.exists());
        assert!(
            files.get("4", &drive("4", 101)).await.is_err(),
            "larger than the cache"
        );
        assert!(second.exists() && third.exists());
        assert_eq!(usage(folder.path()), (2, 80));
    }
}
