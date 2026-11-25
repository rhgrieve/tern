//! Pinned chats, as they are in Teams: the folder it calls Favorites. The rest of the engine's
//! folders is left alone: reading them all also replaces which teams are shown.

use chatsvc::PinnedChats;
use store::{FolderRecord, Store};

use crate::backend::{Failure, Teams};

const FAVORITES: &str = "Favorites";

/// The pinned chats, in the order the user gave them.
pub fn pinned(store: &Store) -> Vec<String> {
    let folders = store.folders().unwrap_or_default();
    folders
        .into_iter()
        .find(|folder| folder.kind == FAVORITES)
        .map(|folder| folder.conversation_ids)
        .unwrap_or_default()
}

/// Keeps them in the cache; answers whether they changed.
pub fn keep_pinned(store: &Store, folder: &str, chats: &[String]) -> Result<bool, store::Error> {
    if pinned(store) == chats {
        return Ok(false);
    }
    let folder = FolderRecord {
        id: folder.to_owned(),
        name: FAVORITES.to_owned(),
        kind: FAVORITES.to_owned(),
        expanded: true,
        conversation_ids: chats.to_vec(),
    };
    store.replace_folders(&[folder], &store.pinned_channel_ids()?)?;
    Ok(true)
}

fn failure(error: chatsvc::Error) -> Failure {
    teams_core::Error::from(error).into()
}

impl Teams {
    /// Reads which chats are pinned; answers whether that changed.
    pub async fn read_pins(&self) -> Result<bool, Failure> {
        match self.pins.pinned_chats().await {
            Ok(pins) => self.keep(&pins),
            // Nothing was ever pinned.
            Err(chatsvc::Error::NoFavoritesFolder) => Ok(false),
            Err(error) => Err(failure(error)),
        }
    }

    /// Pinned chats that are not in the cache: nobody wrote in them for so long that the chat list
    /// does not reach them.
    pub fn missing_pins(&self) -> Vec<String> {
        let mut missing = pinned(self.store());
        missing.retain(|id| matches!(self.store().chat(id), Ok(None)));
        missing
    }

    /// Reads a chat that is not in the cache.
    pub async fn read_chat(&self, conversation: &str) -> Result<(), Failure> {
        self.engine().fetch_newer(conversation).await?;
        Ok(())
    }

    /// Pins a chat or takes that back; only the user sees it.
    pub async fn pin(&self, conversation: &str, on: bool) -> Result<(), Failure> {
        let pins = if on {
            self.pins.pin_chat(conversation, None).await
        } else {
            self.pins.unpin_chat(conversation).await
        };
        self.keep(&pins.map_err(failure)?)?;
        Ok(())
    }

    fn keep(&self, pins: &PinnedChats) -> Result<bool, Failure> {
        keep_pinned(self.store(), &pins.folder_id, &pins.chat_ids)
            .map_err(|error| Failure::Temporary(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_are_kept_in_their_order() {
        let store = Store::open_in_memory().unwrap();
        store
            .replace_pinned_channels(&["19:channel".to_owned()])
            .unwrap();
        assert!(pinned(&store).is_empty());
        let both = ["b".to_owned(), "a".to_owned()];
        assert!(keep_pinned(&store, "f", &both).unwrap());
        assert!(!keep_pinned(&store, "f", &both).unwrap());
        assert_eq!(pinned(&store), both);
        assert!(keep_pinned(&store, "f", &[]).unwrap());
        assert!(pinned(&store).is_empty());
        assert_eq!(store.pinned_channel_ids().unwrap(), ["19:channel"]);
    }
}
