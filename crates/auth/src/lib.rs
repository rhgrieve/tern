//! Signing in to Teams and minting access tokens, without a browser running in the background.

mod authz;
mod broker;
mod error;
mod flow;
mod jwt;
mod store;

pub use authz::{Regions, parse_regions};
pub use broker::{Broker, Endpoint, HttpEndpoint, TokenReply};
pub use error::Error;
pub use flow::{REDIRECT, SignIn, SignInRequest};
pub use jwt::{Identity, TokenInfo};
pub use store::{KeyringStore, SecretStore, StoredLogin};

/// The public client id of Teams; its sign-in works in tenants that allow Teams.
pub const CLIENT_ID: &str = "1fec8e78-bce4-4aaf-ab1b-5451cc387264";
pub const SPACES: &str = "https://api.spaces.skype.com";

/// The audiences Tern needs, for `tern-cli tokens`.
pub const AUDIENCES: [(&str, &str); 6] = [
    ("sign-in (spaces)", SPACES),
    ("chat and push (IC3)", "https://ic3.teams.office.com"),
    (
        "chat list (aggregator)",
        "https://chatsvcagg.teams.microsoft.com",
    ),
    ("presence", "https://presence.teams.microsoft.com"),
    ("Microsoft Graph", "https://graph.microsoft.com"),
    ("search (substrate)", "https://substrate.office.com"),
];

pub type Result<T> = std::result::Result<T, Error>;
