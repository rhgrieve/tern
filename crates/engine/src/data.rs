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
    let folder = match std::env::var_os("TERN_DATA_DIR") {
        Some(folder) => PathBuf::from(folder),
        None => directories::ProjectDirs::from("", "", "tern")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no data directory"))?
            .data_dir()
            .to_owned(),
    };
    Ok(folder.join(account).join("cache.sqlite3"))
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
    Store::open(path).map_err(|error| error.to_string())
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
