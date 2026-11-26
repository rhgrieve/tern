//! Message sync, the outbox and the rules for notifications, on top of the vendored engine.

mod agenda;
mod backend;
mod data;
mod links;
mod media;
mod notice;
mod outbox;
mod pins;
mod sync;

pub use agenda::{
    Agenda, Answer, Meeting, Place, Reminder, Reminders, Stage, agenda, keep_agenda,
    meeting_address, next_reminder, place,
};
pub use backend::{Act, Backend, Failure, Teams, read_only_guard};
pub use data::{accounts, database_path, open_read_only, open_store};
pub use links::{bare_links, linked};
pub use media::{
    Emoji, FILE_BYTES, Files, Want, custom_emoji, full_size, is_giphy, may_prefetch, may_show,
    people, pictures, prefetch, usage as media_usage,
};
pub use notice::{Notice, Rules};
pub use outbox::{Attached, Draft, MAX_FILES, Outbox, Post, Queued};
pub use pins::{keep_pinned, pinned};
pub use sync::{Arrived, Job, Sync, SyncState};
