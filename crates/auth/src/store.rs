use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use session::BoxFuture;

use crate::{Error, Identity, Result};

/// What outlives a restart. The refresh token never appears in `Debug` output.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredLogin {
    pub refresh_token: String,
    pub identity: Identity,
}

impl std::fmt::Debug for StoredLogin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredLogin")
            .finish_non_exhaustive()
    }
}

pub trait SecretStore: Send + Sync {
    fn load(&self) -> BoxFuture<'_, Result<Option<StoredLogin>>>;
    fn save<'a>(&'a self, login: &'a StoredLogin) -> BoxFuture<'a, Result<()>>;
    fn clear(&self) -> BoxFuture<'_, Result<()>>;
}

/// The desktop's keyring (Secret Service, or the portal inside a sandbox).
pub struct KeyringStore;

const LABEL: &str = "Tern sign-in";

fn attributes() -> HashMap<&'static str, &'static str> {
    HashMap::from([("app", "tern"), ("kind", "login")])
}

fn keyring_error(error: oo7::Error) -> Error {
    Error::Keyring(error.to_string())
}

async fn open() -> Result<oo7::Keyring> {
    let keyring = oo7::Keyring::new().await.map_err(keyring_error)?;
    keyring.unlock().await.map_err(keyring_error)?;
    Ok(keyring)
}

impl SecretStore for KeyringStore {
    fn load(&self) -> BoxFuture<'_, Result<Option<StoredLogin>>> {
        Box::pin(async {
            let items = open()
                .await?
                .search_items(&attributes())
                .await
                .map_err(keyring_error)?;
            let Some(item) = items.first() else {
                return Ok(None);
            };
            let secret = item.secret().await.map_err(keyring_error)?;
            serde_json::from_slice(secret.as_bytes())
                .map(Some)
                .map_err(|_| Error::Keyring("the stored sign-in is unreadable".into()))
        })
    }

    fn save<'a>(&'a self, login: &'a StoredLogin) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let secret =
                serde_json::to_vec(login).map_err(|error| Error::Keyring(error.to_string()))?;
            open()
                .await?
                .create_item(LABEL, &attributes(), secret, true)
                .await
                .map_err(keyring_error)
                .map(drop)
        })
    }

    fn clear(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            open()
                .await?
                .delete(&attributes())
                .await
                .map_err(keyring_error)
        })
    }
}
