//! What was being written, kept on disk so that closing the window loses nothing.

use std::collections::HashMap;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::composer::Written;

/// The file for an account's drafts.
pub fn path(account: &str) -> Option<PathBuf> {
    let database = tern_engine::database_path(account).ok()?;
    Some(database.with_file_name("drafts.json"))
}

/// Drafts whose attached files are gone lose only those.
pub fn load(path: &Path) -> HashMap<String, Written> {
    let mut drafts: HashMap<String, Written> = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    for written in drafts.values_mut() {
        written.files.retain(|file| file.is_file());
    }
    drafts.retain(|_, written| *written != Written::default());
    drafts
}

/// Replaces the file in one step, so a crash leaves the old or the new drafts.
pub fn save(path: &Path, drafts: &HashMap<String, Written>) -> Result<(), String> {
    let said = |error: std::io::Error| error.to_string();
    let bytes = serde_json::to_vec(drafts).map_err(|error| error.to_string())?;
    let next = path.with_extension("json.new");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&next)
        .map_err(said)?;
    // Also when an earlier file had other permissions.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(said)?;
    file.write_all(&bytes).map_err(said)?;
    file.sync_all().map_err(said)?;
    std::fs::rename(&next, path).map_err(said)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_come_back_and_are_private() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("drafts.json");
        assert!(load(&path).is_empty());
        let here = folder.path().join("a.txt");
        std::fs::write(&here, b"x").unwrap();
        let written = Written {
            text: "half a thought ünïcode".into(),
            mentions: vec![("1".into(), "Ann".into())],
            files: vec![here.clone(), folder.path().join("gone.txt")],
        };
        let only_gone = Written {
            files: vec![folder.path().join("gone.txt")],
            ..Default::default()
        };
        let drafts = HashMap::from([
            ("c1".to_owned(), written.clone()),
            ("c2".to_owned(), only_gone),
        ]);
        save(&path, &drafts).unwrap();
        save(&path, &drafts).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let back = load(&path);
        assert_eq!(back.len(), 1, "nothing is left of the other");
        assert_eq!(
            back["c1"],
            Written {
                files: vec![here],
                ..written
            }
        );
        std::fs::write(&path, b"{broken").unwrap();
        assert!(load(&path).is_empty());
    }
}
