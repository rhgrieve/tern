use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use store::Store;

/// One cache per account, in `$TERN_DATA_DIR` or the user's data directory, so a change of account
/// never mixes two people's messages.
pub fn database_path(account: &str) -> io::Result<PathBuf> {
    let plain = account
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    if account.is_empty() || !plain {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bad account id",
        ));
    }
    Ok(data_folder()?.join(account).join("cache.sqlite3"))
}

fn data_folder() -> io::Result<PathBuf> {
    Ok(match std::env::var_os("TERN_DATA_DIR") {
        Some(folder) => PathBuf::from(folder),
        None => directories::ProjectDirs::from("", "", "tern")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no data directory"))?
            .data_dir()
            .to_owned(),
    })
}

/// The accounts with a cache, the last used first.
pub fn accounts() -> Vec<String> {
    let mut found: Vec<(std::time::SystemTime, String)> = data_folder()
        .and_then(std::fs::read_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|folder| {
            let used = folder
                .path()
                .join("cache.sqlite3")
                .metadata()
                .ok()?
                .modified()
                .ok()?;
            Some((used, folder.file_name().into_string().ok()?))
        })
        .collect();
    found.sort_by(|a, b| b.cmp(a));
    found.into_iter().map(|(_, account)| account).collect()
}

/// Opens the cache for writing; only the daemon does. The folder is private to the user, which
/// also covers the files SQLite puts next to the database.
pub fn open_store(path: &Path) -> Result<Store, String> {
    if let Some(folder) = path.parent() {
        let private = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(folder)
            .and_then(|()| {
                std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))
            });
        private.map_err(|error| error.to_string())?;
    }
    // SQLite gives its companion files the mode of the database.
    let private = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .and_then(|file| file.set_permissions(std::fs::Permissions::from_mode(0o600)));
    private.map_err(|error| error.to_string())?;
    for companion in ["cache.sqlite3-wal", "cache.sqlite3-shm"] {
        let companion = path.with_file_name(companion);
        if companion.exists() {
            std::fs::set_permissions(&companion, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| error.to_string())?;
        }
    }
    let store = Store::open(path).map_err(|error| error.to_string())?;
    match reopen_history(&store) {
        Ok(0) => {}
        Ok(count) => tracing::info!(count, "chats reopened for older messages"),
        Err(error) => tracing::warn!(%error, "could not reopen chats for older messages"),
    }
    Ok(store)
}

const META_HISTORY: &str = "tern.history";

/// Earlier versions took a stretch of time without messages for the start of a chat. Lets every
/// chat be asked again, once; those that really end say so again.
fn reopen_history(store: &Store) -> Result<usize, store::Error> {
    if store.meta(META_HISTORY)?.as_deref() == Some("2") {
        return Ok(0);
    }
    let mut reopened = 0;
    for chat in store.recent_chats(100_000)? {
        if let Some(state) = store.sync_state(&chat.id)?.filter(|state| !state.has_more) {
            let state = store::SyncState {
                has_more: true,
                ..state
            };
            store.set_sync_state(&chat.id, &state)?;
            reopened += 1;
        }
    }
    store.set_meta(META_HISTORY, "2")?;
    Ok(reopened)
}

pub fn open_read_only(account: &str) -> Result<Store, String> {
    let path = database_path(account).map_err(|error| error.to_string())?;
    Store::open_read_only(&path).map_err(|error| format!("no readable cache yet ({error})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_cannot_name_another_folder() {
        for bad in ["", "../x", "a/b", "."] {
            assert!(database_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn finished_chats_are_reopened_once() {
        let store = Store::open_in_memory().unwrap();
        let chat = store::ChatRecord {
            id: "c".into(),
            kind: "group".into(),
            ..Default::default()
        };
        store.upsert_chats(&[chat]).unwrap();
        let ended = store::SyncState {
            has_more: false,
            ..Default::default()
        };
        store.set_sync_state("c", &ended).unwrap();
        assert_eq!(reopen_history(&store).unwrap(), 1);
        assert!(store.sync_state("c").unwrap().unwrap().has_more);
        // A chat that really ends stays ended.
        store.set_sync_state("c", &ended).unwrap();
        assert_eq!(reopen_history(&store).unwrap(), 0);
        assert!(!store.sync_state("c").unwrap().unwrap().has_more);
    }

    #[test]
    fn readers_see_the_cache_but_cannot_write() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("private").join("cache.sqlite3");
        let writer = open_store(&path).unwrap();
        writer.set_meta("k", "v").unwrap();
        let mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        for name in ["cache.sqlite3", "cache.sqlite3-wal"] {
            let file = std::fs::metadata(path.with_file_name(name)).unwrap();
            assert_eq!(file.permissions().mode() & 0o777, 0o600, "{name}");
        }
        let reader = Store::open_read_only(&path).unwrap();
        assert_eq!(reader.meta("k").unwrap().as_deref(), Some("v"));
        assert!(reader.set_meta("k", "w").is_err());
        writer.set_meta("k", "x").unwrap();
        assert_eq!(reader.meta("k").unwrap().as_deref(), Some("x"));
    }
}
