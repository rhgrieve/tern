//! Message sync, the outbox and the rules for notifications, on top of the vendored engine.

mod backend;
mod data;
mod links;
mod media;
mod notice;
mod outbox;
mod sync;

pub use backend::{Act, Backend, Failure, Teams, read_only_guard};
pub use data::{accounts, database_path, open_read_only, open_store};
pub use links::{bare_links, linked};
pub use media::{
    Emoji, FILE_BYTES, Files, Want, custom_emoji, full_size, may_prefetch, people, pictures,
    prefetch, usage as media_usage,
};
pub use notice::{Notice, Rules};
pub use outbox::{Attached, Draft, MAX_FILES, Outbox, Post, Queued};
pub use sync::{Arrived, Job, Sync, SyncState};
