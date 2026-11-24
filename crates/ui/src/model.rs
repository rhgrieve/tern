//! What the window shows, worked out from the cache. No widgets, so it can be tested.

use chrono::{DateTime, Local, NaiveDate, TimeZone, Utc};
use store::{ChatRecord, MessageRecord, OutboxRecord, OutboxState, SidebarTeam, Store};
use teams_core::{
    files, html_to_spans, linked_message_spans, markdown_to_html, reaction_summaries,
};

use crate::rich::{self, Block};
use crate::row::{Item, Reaction};

/// Starts the id of a message that is not sent yet.
pub const OUTGOING: &str = "outgoing-";
/// Messages of one person this close together are shown as one.
const JOIN: chrono::Duration = chrono::Duration::minutes(5);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChatRow {
    pub id: String,
    pub title: String,
    pub preview: String,
    pub when: String,
    pub unread: bool,
    pub muted: bool,
    /// The other person of a 1:1 chat.
    pub person: Option<String>,
    /// The team of a channel.
    pub team: Option<String>,
    pub mentions: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    Chats,
    Unread,
    Channels,
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn day(at: DateTime<Utc>) -> NaiveDate {
    at.with_timezone(&Local).date_naive()
}

/// Short, as in the chat list: the time today, the weekday this week, else the date.
pub fn short_time(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let local = at.with_timezone(&Local);
    let days = (day(now) - day(at)).num_days();
    match days {
        0 => local.format("%H:%M").to_string(),
        1..7 => local.format("%a").to_string(),
        _ if local.format("%Y").to_string()
            == now.with_timezone(&Local).format("%Y").to_string() =>
        {
            local.format("%-d %b").to_string()
        }
        _ => local.format("%-d %b %Y").to_string(),
    }
}

fn day_name(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    match (day(now) - day(at)).num_days() {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..7 => at.with_timezone(&Local).format("%A").to_string(),
        _ => at.with_timezone(&Local).format("%A, %-d %B %Y").to_string(),
    }
}

fn chat_row(chat: &ChatRecord, me: Option<&str>, now: DateTime<Utc>) -> ChatRow {
    let from_me =
        chat.last_message_sender_id.is_some() && chat.last_message_sender_id.as_deref() == me;
    let alone = chat.kind.eq_ignore_ascii_case("oneOnOne");
    let text = one_line(chat.last_message_preview.as_deref().unwrap_or_default());
    let preview = if chat.last_message_deleted {
        "Message deleted".to_owned()
    } else if text.is_empty() {
        String::new()
    } else if from_me {
        format!("You: {text}")
    } else if let Some(name) = chat.last_message_sender_name.as_deref().filter(|_| !alone) {
        format!("{}: {text}", name.split(' ').next().unwrap_or(name))
    } else {
        text
    };
    ChatRow {
        id: chat.id.clone(),
        title: one_line(&chat.display_title()),
        preview,
        when: chat
            .last_message_at
            .map(|at| short_time(at, now))
            .unwrap_or_default(),
        unread: chat.unread,
        muted: chat.muted,
        person: alone
            .then(|| {
                chat.members
                    .iter()
                    .find(|member| member.user_id.as_deref() != me)
            })
            .flatten()
            .and_then(|member| member.user_id.clone()),
        ..Default::default()
    }
}

pub fn chat_rows(
    chats: &[ChatRecord],
    filter: Filter,
    me: Option<&str>,
    now: DateTime<Utc>,
) -> Vec<ChatRow> {
    chats
        .iter()
        .filter(|chat| filter != Filter::Unread || chat.unread)
        .map(|chat| chat_row(chat, me, now))
        .collect()
}

pub fn channel_rows(teams: &[SidebarTeam], now: DateTime<Utc>) -> Vec<ChatRow> {
    let mut rows = Vec::new();
    for team in teams.iter().filter(|team| !team.hidden) {
        for channel in &team.channels {
            if team.hidden_channel_ids.contains(&channel.id) {
                continue;
            }
            rows.push(ChatRow {
                id: channel.id.clone(),
                title: one_line(&channel.name),
                preview: one_line(&team.team.name),
                when: channel
                    .last_message_at
                    .map(|at| short_time(at, now))
                    .unwrap_or_default(),
                unread: channel.unread,
                team: Some(team.team.id.clone()),
                mentions: team.mentions.contains_key(&channel.id),
                ..Default::default()
            });
        }
    }
    rows
}

/// The messages of a conversation and what is still on its way, oldest first.
pub fn items(
    messages: &[MessageRecord],
    outgoing: &[OutboxRecord],
    me: Option<&str>,
    my_name: &str,
    now: DateTime<Utc>,
) -> Vec<Item> {
    let mut items = Vec::with_capacity(messages.len() + outgoing.len());
    let mut before: Option<(&MessageRecord, bool)> = None;
    for message in messages {
        let event = message.call_event_json.is_some();
        let new_day =
            before.is_none_or(|(last, _)| day(last.created_at) != day(message.created_at));
        let joined = !new_day
            && !event
            && message.reply_to_id.is_none()
            && before.is_some_and(|(last, last_event)| {
                !last_event
                    && last.sender_id.is_some()
                    && last.sender_id == message.sender_id
                    && message.created_at - last.created_at < JOIN
            });
        items.push(Item {
            id: message.message_id.clone(),
            sender: if event { String::new() } else { name(message) },
            sender_id: message.sender_id.clone(),
            time: message
                .created_at
                .with_timezone(&Local)
                .format("%H:%M")
                .to_string(),
            blocks: content(message),
            joined: joined || event,
            own: message.sender_id.is_some() && message.sender_id.as_deref() == me,
            note: (message.edited_at.is_some() && !message.deleted).then(|| "Edited".to_owned()),
            reactions: reaction_summaries(message, me)
                .into_iter()
                .map(|reaction| Reaction {
                    emoji: in_colour(&reaction.emoji),
                    count: reaction.count,
                    mine: reaction.mine,
                })
                .collect(),
            divider: new_day.then(|| day_name(message.created_at, now)),
            thread: None,
            // The service keeps the text of a deleted message for a while; nobody should see it.
            text: if message.deleted {
                String::new()
            } else {
                teams_core::copy_text(message)
            },
            source: (message.sender_id.is_some()
                && message.sender_id.as_deref() == me
                && !message.deleted
                && !event
                && files(message).is_empty())
            .then(|| crate::source::markdown(&teams_core::message_spans(message)))
            .flatten(),
            sent: !message.deleted && !event,
            failed: false,
        });
        before = Some((message, event));
    }
    for waiting in outgoing {
        let Ok(draft) = serde_json::from_str::<tern_engine::Draft>(&waiting.payload) else {
            continue;
        };
        let mut blocks = rich::blocks(&html_to_spans(&markdown_to_html(&draft.markdown)));
        for file in &draft.files {
            blocks.push(Block::Text(format!("📎 {}", rich::escape(&file.name))));
        }
        items.push(Item {
            id: format!("{OUTGOING}{}", waiting.id),
            failed: waiting.state == OutboxState::Failed,
            sender: my_name.to_owned(),
            sender_id: me.map(str::to_owned),
            time: waiting
                .created_at
                .with_timezone(&Local)
                .format("%H:%M")
                .to_string(),
            blocks,
            own: true,
            note: Some(match waiting.state {
                OutboxState::Sending => "Sending…".to_owned(),
                OutboxState::Failed => format!(
                    "Not sent: {}",
                    waiting.last_error.as_deref().unwrap_or("unknown error")
                ),
            }),
            ..Default::default()
        });
    }
    items
}

/// Old symbols like the heart are drawn in black unless asked for as emoji.
fn in_colour(emoji: &str) -> String {
    let mut letters = emoji.chars();
    match (letters.next(), letters.next()) {
        (Some(letter), None) if ('\u{2000}'..'\u{1f000}').contains(&letter) => {
            format!("{letter}\u{fe0f}")
        }
        _ => emoji.to_owned(),
    }
}

fn name(message: &MessageRecord) -> String {
    let name = one_line(message.sender_name.as_deref().unwrap_or_default());
    if name.is_empty() {
        "Unknown".into()
    } else {
        name
    }
}

fn content(message: &MessageRecord) -> Vec<Block> {
    if message.deleted {
        return vec![Block::Text("<i>This message was deleted</i>".into())];
    }
    if message.call_event_json.is_some() {
        let text = teams_core::readable_text(message);
        return vec![Block::Text(format!(
            "<small>{}</small>",
            rich::escape(&text)
        ))];
    }
    let mut blocks = Vec::new();
    if let Some(subject) = message
        .subject
        .as_deref()
        .filter(|subject| !subject.trim().is_empty())
    {
        blocks.push(Block::Heading(rich::escape(&one_line(subject))));
    }
    blocks.extend(rich::blocks(&linked_message_spans(message)));
    for card in teams_core::card_texts(message) {
        blocks.push(Block::Quote(vec![Block::Text(rich::escape(card.trim()))]));
    }
    for file in files(message) {
        let name = rich::escape(&one_line(&file.name));
        // Opens in the browser; the daemon's copy is behind `tern-cli get`.
        blocks.push(Block::Text(if rich::may_open(&file.open_url) {
            format!("📎 <a href=\"{}\">{name}</a>", rich::escape(&file.open_url))
        } else {
            format!("📎 {name}")
        }));
    }
    if blocks.is_empty() {
        blocks.push(Block::Text("<i>Nothing to show</i>".into()));
    }
    blocks
}

/// The posts of a channel, the latest active last, each saying how many replies it has.
pub fn posts(
    messages: &[MessageRecord],
    outgoing: &[OutboxRecord],
    me: Option<&str>,
    my_name: &str,
    now: DateTime<Utc>,
) -> Vec<Item> {
    use std::collections::HashMap;
    let mut replies: HashMap<&str, Vec<&MessageRecord>> = HashMap::new();
    for message in messages {
        if let Some(root) = message.reply_to_id.as_deref() {
            replies.entry(root).or_default().push(message);
        }
    }
    let active = |post: &MessageRecord| {
        replies
            .get(post.message_id.as_str())
            .and_then(|replies| replies.iter().map(|reply| reply.created_at).max())
            .unwrap_or(post.created_at)
            .max(post.created_at)
    };
    let mut roots: Vec<&MessageRecord> = messages
        .iter()
        .filter(|message| message.reply_to_id.is_none())
        .collect();
    roots.sort_by_key(|post| active(post));
    let mut all = Vec::with_capacity(roots.len());
    let mut last_day = None;
    for root in roots {
        let at = active(root);
        // One by one, so that posts never join into one.
        let mut item = items(std::slice::from_ref(root), &[], me, my_name, now).remove(0);
        item.divider = (last_day != Some(day(at))).then(|| day_name(at, now));
        last_day = Some(day(at));
        if day(root.created_at) != day(now) {
            item.time = format!("{}, {}", short_time(root.created_at, now), item.time);
        }
        item.thread = item
            .sent
            .then(|| match replies.get(root.message_id.as_str()) {
                None => "Reply".to_owned(),
                Some(replies) => {
                    let count = replies.len();
                    let word = if count == 1 { "reply" } else { "replies" };
                    let who = replies.last().map(|reply| name(reply)).unwrap_or_default();
                    format!("{count} {word}, last by {who} {}", short_time(at, now))
                }
            });
        all.push(item);
    }
    let waiting: Vec<OutboxRecord> = outgoing
        .iter()
        .filter(|record| record.thread_root_id.is_none())
        .cloned()
        .collect();
    all.extend(items(&[], &waiting, me, my_name, now));
    all
}

/// Everything the window needs to know about a conversation.
#[derive(Debug, Clone, Default)]
pub struct Conversation {
    pub items: Vec<Item>,
    /// Messages read from the cache, which can be more than are shown.
    pub read: usize,
    /// When the oldest shown message was written, to ask for older ones.
    pub oldest: Option<DateTime<Utc>>,
    pub title: String,
    pub subtitle: String,
    pub channel: bool,
}

/// `thread` is the post whose replies are shown, in a channel.
pub fn conversation(
    store: &Store,
    id: &str,
    thread: Option<&str>,
    limit: usize,
    now: DateTime<Utc>,
) -> Conversation {
    let me = store.meta("me_user_id").ok().flatten();
    let my_name = store
        .meta("me_display_name")
        .ok()
        .flatten()
        .unwrap_or_default();
    let messages = match thread {
        Some(root) => store.thread_messages(id, root),
        None => store.messages(id, None, limit),
    }
    .unwrap_or_default();
    let outgoing = store.outbox_for_conversation(id).unwrap_or_default();
    let chat = store.chat(id).ok().flatten();
    let channel = chat.is_none() && id != "48:notes";
    let (title, subtitle) = match &chat {
        Some(chat) => {
            let people = chat.members.len();
            let subtitle = if chat.kind.eq_ignore_ascii_case("oneOnOne") || people < 2 {
                String::new()
            } else {
                format!("{people} people")
            };
            (one_line(&chat.display_title()), subtitle)
        }
        None if id == "48:notes" => ("Notes".to_owned(), String::new()),
        None => store.sidebar().map_or_else(
            |_| Default::default(),
            |sidebar| {
                sidebar
                    .teams
                    .iter()
                    .find_map(|team| {
                        let channel = team.channels.iter().find(|channel| channel.id == id)?;
                        Some((one_line(&channel.name), one_line(&team.team.name)))
                    })
                    .unwrap_or_default()
            },
        ),
    };
    Conversation {
        oldest: messages.first().map(|message| message.created_at),
        read: messages.len(),
        items: match (channel, thread) {
            (true, None) => posts(&messages, &outgoing, me.as_deref(), &my_name, now),
            (_, Some(root)) => {
                let here: Vec<OutboxRecord> = outgoing
                    .into_iter()
                    .filter(|record| record.thread_root_id.as_deref() == Some(root))
                    .collect();
                let mut all = items(&messages, &here, me.as_deref(), &my_name, now);
                // A reply to one's own post minutes later is still a reply.
                if let Some(first) = all.get_mut(1) {
                    first.joined = false;
                }
                all
            }
            (false, None) => items(&messages, &outgoing, me.as_deref(), &my_name, now),
        },
        subtitle: if thread.is_some() {
            format!("Thread in {title}")
        } else {
            subtitle
        },
        title: if thread.is_some() {
            "Thread".to_owned()
        } else {
            title
        },
        channel,
    }
}

/// How one list becomes another with the least change, for lists that keep their place: how many
/// at the start stay, how many are removed after them, and the rest of `new` to insert there up
/// to the common end.
pub fn change<T: PartialEq>(old: &[T], new: &[T]) -> (usize, usize, usize) {
    let start = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let end = old[start..]
        .iter()
        .rev()
        .zip(new[start..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (start, old.len() - start - end, new.len() - end)
}

/// One step from an old list to a new one, at a position in the list as it is by then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Remove(usize),
    /// Position, and which of the new rows.
    Insert(usize, usize),
    Replace(usize, usize),
}

/// Few steps from `old` to `new`, for rows known by `key`. A row that moved is removed and
/// inserted; the rows it passed are left alone.
pub fn steps<T: PartialEq>(old: &[T], new: &[T], key: impl Fn(&T) -> &str) -> Vec<Step> {
    use std::collections::{HashMap, HashSet};
    let wanted: HashSet<&str> = new.iter().map(&key).collect();
    let target: HashMap<&str, usize> = new
        .iter()
        .enumerate()
        .map(|(at, row)| (key(row), at))
        .collect();
    let mut steps = Vec::new();
    // What stays, as positions in `new`: the longest run already in order keeps its place.
    let mut current: Vec<(usize, usize)> = Vec::new();
    for (at, row) in old.iter().enumerate().rev() {
        if !wanted.contains(key(row)) {
            steps.push(Step::Remove(at));
        }
    }
    for (from, row) in old.iter().enumerate() {
        if let Some(to) = target.get(key(row)) {
            current.push((from, *to));
        }
    }
    let keep = in_order(&current.iter().map(|(_, to)| *to).collect::<Vec<_>>());
    for at in (0..current.len()).rev() {
        if !keep.contains(&current[at].1) {
            steps.push(Step::Remove(at));
            current.remove(at);
        }
    }
    // Now only insertions are missing, front to back.
    let mut kept = current.into_iter().peekable();
    for (to, row) in new.iter().enumerate() {
        match kept.next_if(|(_, next)| *next == to) {
            Some((from, _)) if old[from] != *row => steps.push(Step::Replace(to, to)),
            Some(_) => {}
            None => steps.push(Step::Insert(to, to)),
        }
    }
    steps
}

/// The longest increasing run of `values`.
fn in_order(values: &[usize]) -> std::collections::HashSet<usize> {
    let mut ends: Vec<usize> = Vec::new();
    let mut before: Vec<Option<usize>> = vec![None; values.len()];
    for (at, value) in values.iter().enumerate() {
        let place = ends.partition_point(|end| values[*end] < *value);
        before[at] = place.checked_sub(1).map(|place| ends[place]);
        if place == ends.len() {
            ends.push(at);
        } else {
            ends[place] = at;
        }
    }
    let mut run = std::collections::HashSet::new();
    let mut at = ends.last().copied();
    while let Some(index) = at {
        run.insert(values[index]);
        at = before[index];
    }
    run
}

/// Midnight-safe "now" for tests and the real thing alike.
pub fn now() -> DateTime<Utc> {
    Utc.timestamp_opt(Utc::now().timestamp(), 0)
        .single()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minutes: i64) -> DateTime<Utc> {
        // Noon local time, so a few minutes or hours never change the day.
        let noon = Local.with_ymd_and_hms(2026, 11, 24, 12, 0, 0).unwrap();
        noon.with_timezone(&Utc) + chrono::Duration::minutes(minutes)
    }

    fn message(id: &str, sender: &str, minutes: i64) -> MessageRecord {
        MessageRecord {
            conversation_id: "c".into(),
            message_id: id.into(),
            sender_id: Some(sender.into()),
            sender_name: Some(sender.to_uppercase()),
            created_at: at(minutes),
            body_html: "<p>hi</p>".into(),
            ..Default::default()
        }
    }

    #[test]
    fn messages_join_and_days_divide() {
        let day = 24 * 60;
        let messages = [
            message("1", "ann", -day),
            message("2", "ann", 0),
            message("3", "ann", 2),
            message("4", "ann", 30),
            message("5", "me", 31),
        ];
        let items = items(&messages, &[], Some("me"), "Me", at(60));
        let shape: Vec<(bool, Option<&str>, bool)> = items
            .iter()
            .map(|item| (item.joined, item.divider.as_deref(), item.own))
            .collect();
        assert_eq!(
            shape,
            [
                (false, Some("Yesterday"), false),
                (false, Some("Today"), false),
                (true, None, false),
                (false, None, false),
                (false, None, true),
            ]
        );
    }

    #[test]
    fn deleted_edited_and_empty_messages() {
        let deleted = MessageRecord {
            deleted: true,
            body_html: "<p>secret</p>".into(),
            ..message("1", "a", 0)
        };
        let edited = MessageRecord {
            edited_at: Some(at(1)),
            ..message("2", "b", 10)
        };
        let empty = MessageRecord {
            body_html: String::new(),
            ..message("3", "c", 20)
        };
        let items = items(&[deleted, edited, empty], &[], None, "", at(30));
        assert!(!format!("{:?}", items[0]).contains("secret"));
        assert_eq!(items[1].note.as_deref(), Some("Edited"));
        assert_eq!(items[2].blocks.len(), 1);
        assert!(items.iter().all(|item| !item.own), "nobody is signed in");
    }

    #[test]
    fn what_is_on_its_way_shows_its_state() {
        let record = |id: &str, state, error: Option<&str>| OutboxRecord {
            id: id.into(),
            conversation_id: "c".into(),
            target: store::OutboxTarget::Flat,
            thread_root_id: None,
            payload: r#"{"markdown":"**hi** <b>","tried":false}"#.into(),
            images: vec![],
            state,
            last_error: error.map(str::to_owned),
            created_at: at(0),
        };
        let outgoing = [
            record("1", OutboxState::Sending, None),
            record("2", OutboxState::Failed, Some("HTTP 403")),
        ];
        let items = items(&[], &outgoing, Some("me"), "Me", at(1));
        assert_eq!(items[0].note.as_deref(), Some("Sending…"));
        assert_eq!(items[1].note.as_deref(), Some("Not sent: HTTP 403"));
        assert!(matches!(&items[0].blocks[0], Block::Text(text) if text.starts_with("<b>hi</b>")));
    }

    #[test]
    fn posts_count_replies_and_sort_by_activity() {
        let reply = |id: &str, root: &str, minutes| MessageRecord {
            reply_to_id: Some(root.into()),
            ..message(id, "bo", minutes)
        };
        let messages = [
            message("p1", "ann", 0),
            message("p2", "ann", 1),
            reply("r1", "p1", 5),
            reply("r2", "p1", 9),
            reply("r3", "unknown", 10),
        ];
        let posts = posts(&messages, &[], Some("me"), "Me", at(20));
        let shape: Vec<(&str, Option<&str>)> = posts
            .iter()
            .map(|post| (post.id.as_str(), post.thread.as_deref()))
            .collect();
        assert_eq!(
            shape,
            [
                ("p2", Some("Reply")),
                ("p1", Some("2 replies, last by BO 12:09"))
            ]
        );
        assert!(posts.iter().all(|post| !post.joined));
    }

    #[test]
    fn only_own_simple_messages_can_be_edited() {
        let mine = message("1", "me", 0);
        let theirs = message("2", "ann", 10);
        let table = MessageRecord {
            body_html: "<table><tr><td>x</td></tr></table>".into(),
            ..message("3", "me", 20)
        };
        let deleted = MessageRecord {
            deleted: true,
            ..message("4", "me", 30)
        };
        let items = items(&[mine, theirs, table, deleted], &[], Some("me"), "", at(40));
        let can: Vec<bool> = items.iter().map(|item| item.source.is_some()).collect();
        assert_eq!(can, [true, false, false, false]);
        assert!(!items[3].sent);
    }

    #[test]
    fn chat_rows_say_who_and_filter() {
        let chat = |id: &str, kind: &str, sender: &str, unread| ChatRecord {
            id: id.into(),
            kind: kind.into(),
            title: format!("Chat\n{id}"),
            last_message_preview: Some("hello\nthere".into()),
            last_message_sender_id: Some(sender.into()),
            last_message_sender_name: Some("Ann Example".into()),
            last_message_at: Some(at(-60)),
            unread,
            ..Default::default()
        };
        let chats = [
            chat("1", "oneOnOne", "ann", true),
            chat("2", "group", "ann", false),
            chat("3", "group", "me", false),
        ];
        let rows = chat_rows(&chats, Filter::Chats, Some("me"), at(0));
        let previews: Vec<&str> = rows.iter().map(|row| row.preview.as_str()).collect();
        assert_eq!(
            previews,
            ["hello there", "Ann: hello there", "You: hello there"]
        );
        assert_eq!(rows[0].title, "Chat 1");
        assert_eq!(rows[0].when, "11:00");
        assert_eq!(
            chat_rows(&chats, Filter::Unread, Some("me"), at(0)).len(),
            1
        );
    }

    #[test]
    fn times_get_shorter_the_closer_they_are() {
        let day = 24 * 60;
        assert_eq!(short_time(at(-30), at(0)), "11:30");
        assert_eq!(short_time(at(-2 * day), at(0)), "Sun");
        assert_eq!(short_time(at(-20 * day), at(0)), "4 Nov");
        assert_eq!(short_time(at(-400 * day), at(0)), "20 Oct 2025");
    }

    #[test]
    fn a_moved_row_leaves_the_others_alone() {
        type Row = (&'static str, u32);
        let apply = |old: &[Row], new: &[Row]| {
            let steps = steps(old, new, |row| row.0);
            let mut list = old.to_vec();
            for step in &steps {
                match *step {
                    Step::Remove(at) => drop(list.remove(at)),
                    Step::Insert(at, from) => list.insert(at, new[from]),
                    Step::Replace(at, from) => list[at] = new[from],
                }
            }
            assert_eq!(list, new, "{old:?} with {steps:?}");
            steps.len()
        };
        let (a, b, c, d) = (("a", 0), ("b", 0), ("c", 0), ("d", 0));
        assert_eq!(apply(&[a, b, c, d], &[("d", 1), a, b, c]), 2);
        assert_eq!(apply(&[a, b, c], &[a, b, c]), 0);
        assert_eq!(apply(&[a, b, c], &[a, ("b", 1), c]), 1);
        assert_eq!(apply(&[a, b], &[]), 2);
        assert_eq!(apply(&[], &[a, b]), 2);
        assert_eq!(apply(&[a, b, c, d], &[d, c, b, a]), 6);
        // Every order of a few rows, with some gone, new or changed.
        let mut seed = 7u32;
        let mut random = move |below: u32| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (seed >> 16) % below
        };
        const NAMES: [&str; 8] = ["a", "b", "c", "d", "e", "f", "g", "h"];
        for _ in 0..2_000 {
            let mut pick = || {
                let mut names = NAMES.to_vec();
                let mut rows = Vec::new();
                for _ in 0..random(9) {
                    let at = random(names.len() as u32) as usize;
                    rows.push((names.remove(at), random(2)));
                }
                rows
            };
            let (old, new) = (pick(), pick());
            apply(&old, &new);
        }
    }

    #[test]
    fn the_smallest_change_is_found() {
        assert_eq!(change(&[1, 2, 3], &[1, 2, 3, 4]), (3, 0, 4));
        assert_eq!(change(&[1, 2, 3], &[0, 1, 2, 3]), (0, 0, 1));
        assert_eq!(change(&[1, 2, 3], &[1, 9, 3]), (1, 1, 2));
        assert_eq!(change(&[1, 2, 3], &[1, 2, 3]), (3, 0, 3));
        assert_eq!(change(&[1, 1, 1], &[1, 1]), (2, 1, 2));
        assert_eq!(change::<i32>(&[], &[]), (0, 0, 0));
    }
}
