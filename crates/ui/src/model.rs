//! What the window shows, worked out from the cache. No widgets, so it can be tested.

use chrono::{DateTime, Local, NaiveDate, TimeZone, Utc};
use store::{
    ChatRecord, MessageRecord, OutboxRecord, OutboxState, ReadHorizonRecord, SidebarTeam, Store,
};
use teams_core::{
    files, html_to_spans, linked_message_spans, markdown_to_html, reaction_summaries,
};

use crate::card;
use crate::mark::Progress;
use crate::rich::{self, Block};
use crate::row::{Item, Reaction};

/// Starts the id of a message that is not sent yet.
pub const OUTGOING: &str = "outgoing-";
/// The service's clock may be a little behind ours.
const CLOCKS: chrono::Duration = chrono::Duration::seconds(2);
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
                    custom: tern_engine::custom_emoji(&reaction.emoji),
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
            progress: None,
        });
        before = Some((message, event));
    }
    // Shown exactly as they will be once they are sent, so that nothing moves then.
    let mut last_own = before.map(|(last, event)| {
        let own = !event && me.is_some() && last.sender_id.as_deref() == me;
        (last.created_at, own)
    });
    for waiting in outgoing {
        let Ok(draft) = serde_json::from_str::<tern_engine::Draft>(&waiting.payload) else {
            continue;
        };
        let mut blocks = rich::blocks(&html_to_spans(&tern_engine::linked(&markdown_to_html(
            &draft.markdown,
        ))));
        for file in &draft.files {
            blocks.push(Block::Text(format!("📎 {}", rich::escape(&file.name))));
        }
        let sending = waiting.state == OutboxState::Sending;
        // The service has it already, and the outbox is about to know.
        let there = messages.last().is_some_and(|last| {
            me.is_some()
                && last.sender_id.as_deref() == me
                && last.created_at >= waiting.created_at - CLOCKS
                && draft.files.is_empty()
                && content(last).first() == blocks.first()
        });
        if sending && there {
            continue;
        }
        let new_day = last_own.is_none_or(|(at, _)| day(at) != day(waiting.created_at));
        let joined = !new_day
            && draft.reply_to.is_none()
            && waiting.thread_root_id.is_none()
            && last_own.is_some_and(|(at, own)| own && waiting.created_at - at < JOIN);
        last_own = Some((waiting.created_at, true));
        items.push(Item {
            joined,
            divider: new_day.then(|| day_name(waiting.created_at, now)),
            progress: sending.then_some(Progress::Sending),
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
            note: (waiting.state == OutboxState::Failed).then(|| {
                format!(
                    "Not sent: {}",
                    waiting.last_error.as_deref().unwrap_or("unknown error")
                )
            }),
            ..Default::default()
        });
    }
    items
}

/// What is known of who read a conversation, and of who is in it.
#[derive(Default)]
pub struct Readers<'a> {
    pub horizons: &'a [ReadHorizonRecord],
    /// Ids and names.
    pub members: Vec<(&'a str, &'a str)>,
}

/// Says how far the newest own message got, on that message only.
pub fn mark_newest(
    items: &mut [Item],
    messages: &[MessageRecord],
    me: Option<&str>,
    readers: &Readers,
) {
    let newest = items
        .iter()
        .rposition(|item| item.own && (item.sent || item.progress.is_some() || item.failed));
    for (at, item) in items.iter_mut().enumerate() {
        // A message on its way counts as the newest, and none but the newest is marked.
        if Some(at) != newest {
            item.progress = None;
        }
    }
    let Some(item) = newest.map(|at| &mut items[at]).filter(|item| item.sent) else {
        return;
    };
    let message = messages
        .iter()
        .rfind(|message| message.message_id == item.id);
    item.progress = message.map(|message| progress(message, me, readers));
}

fn progress(message: &MessageRecord, me: Option<&str>, readers: &Readers) -> Progress {
    // Ids come with a kind of account before them, or without.
    let user = |id: &str| id.rsplit(':').next().unwrap_or(id).to_ascii_lowercase();
    let me = me.map(user);
    let other = |id: &str| me.as_deref() != Some(user(id).as_str());
    let read: Vec<String> = readers
        .horizons
        .iter()
        .filter(|horizon| other(&horizon.member_id))
        .filter(|horizon| horizon.read_until >= message.created_at)
        .map(|horizon| user(&horizon.member_id))
        .collect();
    if read.is_empty() {
        return Progress::Delivered;
    }
    let others = readers.members.iter().filter(|(id, _)| other(id)).count();
    let known = readers
        .horizons
        .iter()
        .filter(|horizon| other(&horizon.member_id))
        .count();
    let mut names: Vec<String> = read
        .iter()
        .map(|id| {
            readers
                .members
                .iter()
                .find(|(member, _)| user(member) == *id)
                .map_or_else(String::new, |(_, name)| one_line(name))
        })
        .collect();
    names.sort();
    Progress::Seen {
        names,
        others: others.max(known),
    }
}

/// What the reactions will be when the service has heard of it.
pub fn react(reactions: &mut Vec<Reaction>, emoji: &str, on: bool) {
    let emoji = in_colour(emoji);
    let at = reactions
        .iter()
        .position(|reaction| reaction.emoji == emoji);
    match (at, on) {
        (Some(at), _) if reactions[at].mine == on => {}
        (Some(at), true) => {
            reactions[at].mine = true;
            reactions[at].count += 1;
        }
        (Some(at), false) => {
            reactions[at].mine = false;
            reactions[at].count = reactions[at].count.saturating_sub(1);
            if reactions[at].count == 0 {
                reactions.remove(at);
            }
        }
        (None, true) => reactions.push(Reaction {
            custom: tern_engine::custom_emoji(&emoji),
            emoji,
            count: 1,
            mine: true,
        }),
        (None, false) => {}
    }
}

/// A reaction of the user, shown before the service knows.
#[derive(Debug, Clone, PartialEq)]
pub struct Reacting {
    pub conversation: String,
    pub message: String,
    pub emoji: String,
    pub on: bool,
    /// When the service took it.
    pub taken: Option<std::time::Instant>,
}

/// What the cache says counts again after this, even if it says otherwise.
pub const BELIEVE: std::time::Duration = std::time::Duration::from_secs(10);

/// Puts reactions on the way into what the cache has, and forgets those that arrived there.
pub fn apply_reacting(items: &mut [Item], conversation: &str, waiting: &mut Vec<Reacting>) {
    waiting.retain(|reacting| {
        let late = reacting.taken.is_some_and(|at| at.elapsed() > BELIEVE);
        if late || reacting.conversation != conversation {
            return !late;
        }
        let Some(item) = items.iter_mut().find(|item| item.id == reacting.message) else {
            return true;
        };
        let emoji = in_colour(&reacting.emoji);
        let mine = item
            .reactions
            .iter()
            .any(|reaction| reaction.mine && reaction.emoji == emoji);
        if mine == reacting.on {
            return reacting.taken.is_none();
        }
        react(&mut item.reactions, &emoji, reacting.on);
        true
    });
}

/// The pictures messages show, of the messages and of reactions to them.
pub fn pictures(items: &[Item]) -> Vec<&str> {
    fn within<'a>(blocks: &'a [Block], out: &mut Vec<&'a str>) {
        for block in blocks {
            match block {
                Block::Picture(url) => out.push(url),
                Block::Quote(inner) => within(inner, out),
                Block::Preview(preview) => out.extend(preview.picture.as_deref()),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for item in items {
        within(&item.blocks, &mut out);
        out.extend(
            item.reactions
                .iter()
                .filter_map(|reaction| Some(reaction.custom.as_ref()?.url.as_str())),
        );
    }
    out
}

/// Old symbols like the heart are drawn in black unless asked for as emoji.
pub fn in_colour(emoji: &str) -> String {
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
    for attachment in teams_core::attachments(message) {
        let card = attachment.adaptive_card().and_then(|card| {
            let teams = card::link(&message.conversation_id, &message.message_id);
            card::read(&card, teams)
        });
        if let Some(card) = card {
            blocks.push(Block::Card(Box::new(card)));
        } else if let Some(text) = attachment.card_text() {
            blocks.push(Block::Quote(vec![Block::Text(rich::escape(text.trim()))]));
        }
    }
    if let Some(preview) = teams_core::link_preview(message).filter(|it| rich::may_open(&it.url)) {
        let line = |text: Option<&String>| one_line(text.map_or("", String::as_str));
        let site = preview.domain();
        let title = line(preview.title.as_ref());
        blocks.push(Block::Preview(Box::new(rich::Preview {
            title: if title.is_empty() {
                site.clone()
            } else {
                title
            },
            description: line(preview.description.as_ref()),
            site,
            picture: preview.image_url.clone(),
            url: preview.url,
        })));
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
    for mut item in items(&[], &waiting, me, my_name, now) {
        item.joined = false;
        item.divider = item.divider.filter(|_| last_day != Some(day(now)));
        last_day = Some(day(now));
        all.push(item);
    }
    all
}

/// Who is writing, in a few words.
pub fn writing(names: &[&str]) -> String {
    let first = |name: &&str| {
        name.split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned()
    };
    match names {
        [] => String::new(),
        [one] => format!("{} is writing…", one_line(one)),
        [one, two] => format!("{} and {} are writing…", first(one), first(two)),
        more => format!("{} people are writing…", more.len()),
    }
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
    let mut items = match (channel, thread) {
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
    };
    let horizons = store.read_horizons(id).unwrap_or_default();
    let readers = Readers {
        horizons: &horizons,
        members: chat
            .iter()
            .flat_map(|chat| &chat.members)
            .filter_map(|member| Some((member.user_id.as_deref()?, member.display_name.as_str())))
            .collect(),
    };
    mark_newest(&mut items, &messages, me.as_deref(), &readers);
    Conversation {
        oldest: messages.first().map(|message| message.created_at),
        read: messages.len(),
        items,
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

    fn reacted(all: &[(&str, usize, bool)]) -> Vec<Reaction> {
        all.iter()
            .map(|(emoji, count, mine)| Reaction {
                custom: tern_engine::custom_emoji(emoji),
                emoji: (*emoji).into(),
                count: *count,
                mine: *mine,
            })
            .collect()
    }

    #[test]
    fn a_reaction_is_shown_before_the_service_knows() {
        let mut now = reacted(&[("👍", 2, false)]);
        react(&mut now, "👍", true);
        react(&mut now, "👍", true);
        assert_eq!(now, reacted(&[("👍", 3, true)]), "counted once");
        react(&mut now, "❤", true);
        assert_eq!(now, reacted(&[("👍", 3, true), ("❤\u{fe0f}", 1, true)]));
        react(&mut now, "❤\u{fe0f}", false);
        react(&mut now, "👍", false);
        react(&mut now, "👍", false);
        assert_eq!(now, reacted(&[("👍", 2, false)]));
        react(&mut now, "party;0-eus-d1-abc", true);
        assert_eq!(now[1].name(), "party");
    }

    #[test]
    fn reactions_on_the_way_stay_until_the_cache_has_them() {
        let items = |reactions: &[(&str, usize, bool)]| {
            vec![Item {
                id: "m".into(),
                reactions: reacted(reactions),
                ..Default::default()
            }]
        };
        let reacting = |on, taken| Reacting {
            conversation: "c".into(),
            message: "m".into(),
            emoji: "👍".into(),
            on,
            taken,
        };
        let just = std::time::Instant::now();
        let long_ago = just.checked_sub(BELIEVE * 2);
        // Not taken yet, whatever the cache says meanwhile.
        let mut waiting = vec![reacting(true, None)];
        for _ in 0..2 {
            let mut shown = items(&[]);
            apply_reacting(&mut shown, "c", &mut waiting);
            assert_eq!(shown[0].reactions, reacted(&[("👍", 1, true)]));
        }
        let mut shown = items(&[("👍", 1, true)]);
        apply_reacting(&mut shown, "c", &mut waiting);
        assert_eq!(shown[0].reactions, reacted(&[("👍", 1, true)]));
        assert_eq!(waiting.len(), 1);
        // Taken: until the cache agrees, or had its time.
        waiting = vec![reacting(false, Some(just))];
        let mut shown = items(&[("👍", 2, true)]);
        apply_reacting(&mut shown, "c", &mut waiting);
        assert_eq!(shown[0].reactions, reacted(&[("👍", 1, false)]));
        apply_reacting(&mut shown, "c", &mut waiting);
        assert!(waiting.is_empty());
        waiting = vec![reacting(false, long_ago)];
        let mut shown = items(&[("👍", 2, true)]);
        apply_reacting(&mut shown, "c", &mut waiting);
        assert_eq!(shown[0].reactions, reacted(&[("👍", 2, true)]));
        assert!(waiting.is_empty());
        // Another conversation has nothing to do with it.
        waiting = vec![reacting(true, Some(just))];
        let mut shown = items(&[]);
        apply_reacting(&mut shown, "other", &mut waiting);
        assert!(shown[0].reactions.is_empty() && waiting.len() == 1);
    }

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
        assert_eq!(items[0].progress, Some(Progress::Sending));
        assert_eq!(items[0].note, None);
        assert_eq!(items[1].progress, None);
        assert_eq!(items[1].note.as_deref(), Some("Not sent: HTTP 403"));
        assert!(matches!(&items[0].blocks[0], Block::Text(text) if text.starts_with("<b>hi</b>")));
    }

    #[test]
    fn nothing_moves_when_a_message_is_sent() {
        let waiting = OutboxRecord {
            id: "1".into(),
            conversation_id: "c".into(),
            target: store::OutboxTarget::Flat,
            thread_root_id: None,
            payload: r#"{"markdown":"see https://a.example/x","tried":true}"#.into(),
            images: vec![],
            state: OutboxState::Sending,
            last_error: None,
            created_at: at(4),
        };
        let sent = MessageRecord {
            body_html: tern_engine::linked("<p>see https://a.example/x</p>"),
            ..message("9", "me", 4)
        };
        for earlier in [
            vec![],
            vec![message("1", "ann", 1)],
            vec![message("1", "ann", 0), message("2", "me", 1)],
            vec![message("1", "me", -60)],
        ] {
            let before = items(
                &earlier,
                std::slice::from_ref(&waiting),
                Some("me"),
                "ME",
                at(5),
            );
            let all = [earlier.clone(), vec![sent.clone()]].concat();
            let after = items(&all, &[], Some("me"), "ME", at(5));
            let (old, new) = (before.last().unwrap(), after.last().unwrap());
            let shown = |item: &Item| {
                let item = item.clone();
                (
                    item.sender,
                    item.time,
                    item.blocks,
                    item.joined,
                    item.divider,
                    item.note,
                )
            };
            assert_eq!(shown(old), shown(new));
            // For the moment in which both are there, it is shown once.
            let both = items(
                &all,
                std::slice::from_ref(&waiting),
                Some("me"),
                "ME",
                at(5),
            );
            assert_eq!(both.len(), after.len());
        }
        // The same words again are another message.
        let again = [
            message("1", "ann", 1),
            MessageRecord {
                created_at: at(3),
                ..sent
            },
        ];
        assert_eq!(items(&again, &[waiting], Some("me"), "ME", at(5)).len(), 3);
    }

    #[test]
    fn only_the_newest_own_message_says_how_far_it_got() {
        let horizon = |user: &str, minutes| ReadHorizonRecord {
            member_id: format!("8:orgid:{user}"),
            read_until: at(minutes),
            read_at: None,
        };
        let messages = [
            message("1", "me", 0),
            message("2", "me", 1),
            message("3", "ann", 2),
        ];
        let members = vec![("me", "Me"), ("ann", "Ann"), ("Bo", "Bo")];
        let of = |horizons: &[ReadHorizonRecord]| {
            let mut all = items(&messages, &[], Some("me"), "Me", at(5));
            let readers = Readers {
                horizons,
                members: members.clone(),
            };
            mark_newest(&mut all, &messages, Some("me"), &readers);
            assert!(all[0].progress.is_none() && all[2].progress.is_none());
            all[1].progress.clone().unwrap()
        };
        assert_eq!(of(&[]), Progress::Delivered);
        // One's own reading does not count, nor reading earlier messages.
        assert_eq!(
            of(&[horizon("me", 9), horizon("ann", 0)]),
            Progress::Delivered
        );
        let seen = |names: &[&str]| Progress::Seen {
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            others: 2,
        };
        assert_eq!(of(&[horizon("ann", 1), horizon("bo", 0)]), seen(&["Ann"]));
        assert!(of(&[horizon("ann", 1), horizon("bo", 2)]).is_read());
        // A message on its way is the newest.
        let mut all = items(&messages, &[], Some("me"), "Me", at(5));
        all.push(Item {
            own: true,
            progress: Some(Progress::Sending),
            ..Default::default()
        });
        mark_newest(&mut all, &messages, Some("me"), &Readers::default());
        let marked: Vec<bool> = all.iter().map(|item| item.progress.is_some()).collect();
        assert_eq!(marked, [false, false, false, true]);
    }

    #[test]
    fn links_show_what_is_behind_them() {
        let mut linked = message("1", "ann", 0);
        linked.body_html = "<p><a href=\"https://a.example/x\">this</a></p>".into();
        linked.links_json = serde_json::json!([{
            "url": "https://a.example/x", "previewenabled": true,
            "preview": {
                "title": "A <b>page</b>", "description": "About\nit",
                "previewurl": "https://us-prod.asyncgw.teams.microsoft.com/urlp/v1/url/image?x=1"
            }
        }])
        .to_string();
        let Some(Block::Preview(preview)) = content(&linked).pop() else {
            panic!()
        };
        assert_eq!(preview.title, "A <b>page</b>", "shown as text");
        assert_eq!(preview.description, "About it");
        assert_eq!(preview.site, "a.example");
        let item = items(&[linked.clone()], &[], None, "", at(1));
        assert_eq!(pictures(&item).len(), 1);
        // Not for another page than the one that is linked, and only Teams' own pictures.
        linked.body_html = linked.body_html.replace("/x", "/y");
        assert!(!format!("{:?}", content(&linked)).contains("Preview"));
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
    fn who_is_writing() {
        assert_eq!(writing(&[]), "");
        assert_eq!(writing(&["Ann Example"]), "Ann Example is writing…");
        assert_eq!(
            writing(&["Ann Example", "Bo Li"]),
            "Ann and Bo are writing…"
        );
        assert_eq!(writing(&["a", "b", "c"]), "3 people are writing…");
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
