//! Message sync, the outbox and the rules for notifications, on top of the vendored engine.

mod backend;
mod data;
mod notice;
mod outbox;
mod sync;

pub use backend::{Backend, Failure, Teams, read_only_guard};
pub use data::{database_path, open_read_only, open_store};
pub use notice::{Notice, Rules};
pub use outbox::{Outbox, Post, Queued};
pub use sync::{Arrived, Job, Sync, SyncState};
