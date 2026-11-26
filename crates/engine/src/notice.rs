//! Which messages deserve a notification, and what it says.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use store::{ChatRecord, MessageRecord};
use teams_core::{call_event, mentions, mentions_whole_chat, reactions, readable_text};

const REMEMBERED: usize = 500;
const MAX_TEXT: usize = 300;
/// Older messages are history being filled in, not news.
const NEWS: chrono::Duration = chrono::Duration::hours(12);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub conversation: String,
    pub message: String,
    pub title: String,
    pub text: String,
    /// The user can answer from the notification.
    pub can_reply: bool,
    /// About a reaction to the message, not the message.
    pub reaction: bool,
}

pub struct Rules {
    /// Messages from before the first sync never notify.
    started: DateTime<Utc>,
    shown: VecDeque<String>,
}

impl Rules {
    pub fn new(started: DateTime<Utc>) -> Self {
        Rules {
            started,
            shown: VecDeque::new(),
        }
    }

    /// `chat` is the conversation when it is a chat; a channel has none.
    pub fn decide(
        &mut self,
        message: &MessageRecord,
        chat: Option<&ChatRecord>,
        me: Option<&str>,
        now: DateTime<Utc>,
    ) -> Option<Notice> {
        let sender = message.sender_id.as_deref()?;
        let own = me.is_none_or(|me| me == sender);
        let stale = message.created_at < self.started || now - message.created_at > NEWS;
        if own || stale || message.deleted {
            return None;
        }
        if message.call_event_json.is_some() {
            return self.missed(message, chat?, me);
        }
        let mentioned = mentions(message)
            .iter()
            .any(|mention| mention.user_id.as_deref() == me);
        let wanted = match chat {
            Some(chat) => {
                let read = chat
                    .last_read_at
                    .is_some_and(|read| read >= message.created_at);
                !read && (!chat.muted || mentioned)
            }
            None => mentioned,
        };
        if !wanted || self.shown.contains(&message.message_id) {
            return None;
        }
        self.remember(message.message_id.clone());
        let name = message.sender_name.as_deref().unwrap_or("Someone");
        let title = match chat {
            Some(chat) if chat.kind.eq_ignore_ascii_case("oneOnOne") => name.to_owned(),
            Some(chat) => format!("{name} in {}", chat.display_title()),
            None => format!("{name} mentioned you"),
        };
        let everyone = chat.is_some() && mentions_whole_chat(message);
        let mut text = excerpt(message);
        if text.is_empty() {
            text = "Sent an attachment".into();
        }
        Some(Notice {
            conversation: message.conversation_id.clone(),
            message: message.message_id.clone(),
            title: if everyone {
                format!("{title} (everyone)")
            } else {
                title
            },
            text,
            can_reply: chat.is_some(),
            reaction: false,
        })
    }

    /// A call nobody answered. Those of the user are no news, and answered ones neither.
    fn missed(
        &mut self,
        message: &MessageRecord,
        chat: &ChatRecord,
        me: Option<&str>,
    ) -> Option<Notice> {
        let event = call_event(message).filter(|event| event.missed(me))?;
        let read = chat
            .last_read_at
            .is_some_and(|read| read >= message.created_at);
        if read || chat.muted || self.shown.contains(&message.message_id) {
            return None;
        }
        self.remember(message.message_id.clone());
        let text = match event.initiator_name {
            Some(name) if !chat.kind.eq_ignore_ascii_case("oneOnOne") => {
                format!("{name} in {}", chat.display_title())
            }
            Some(name) => name,
            None => chat.display_title(),
        };
        Some(Notice {
            conversation: message.conversation_id.clone(),
            message: message.message_id.clone(),
            title: "Missed call".into(),
            text,
            can_reply: true,
            reaction: false,
        })
    }

    /// Reactions of others to the user's messages that nobody was told about, one notice for each
    /// message. `name` knows people by their ids.
    pub fn reactions(
        &mut self,
        messages: &[MessageRecord],
        chat: Option<&ChatRecord>,
        me: &str,
        name: impl Fn(&str) -> Option<String>,
        now: DateTime<Utc>,
    ) -> Vec<Notice> {
        if chat.is_some_and(|chat| chat.muted) {
            return Vec::new();
        }
        let is_me = |id: Option<&str>| id.is_some_and(|id| id.eq_ignore_ascii_case(me));
        let mut notices = Vec::new();
        for message in messages {
            if message.deleted || !is_me(message.sender_id.as_deref()) {
                continue;
            }
            let mut names: Vec<String> = Vec::new();
            let mut emoji: Vec<String> = Vec::new();
            for reaction in reactions(message) {
                let recent = reaction
                    .created_at
                    .is_some_and(|at| at >= self.started && now - at <= NEWS);
                let Some(user) = reaction.user_id.as_deref() else {
                    continue;
                };
                // Taken back and given again, it is no news.
                let key = format!("{}|{user}|{}", message.message_id, reaction.reaction_type);
                if !recent || is_me(Some(user)) || self.shown.contains(&key) {
                    continue;
                }
                self.remember(key);
                let who = name(user)
                    .or(reaction.user_name)
                    .unwrap_or_else(|| "Someone".into());
                if !names.contains(&who) {
                    names.push(who);
                }
                let shown = shown(&reaction.reaction_type);
                if !emoji.contains(&shown) {
                    emoji.push(shown);
                }
            }
            if names.is_empty() {
                continue;
            }
            let who = match names.as_slice() {
                [one] => one.clone(),
                [one, two] => format!("{one} and {two}"),
                [one, rest @ ..] => format!("{one} and {} others", rest.len()),
                [] => continue,
            };
            let place = chat
                .filter(|chat| !chat.kind.eq_ignore_ascii_case("oneOnOne"))
                .map(|chat| format!(" in {}", chat.display_title()))
                .unwrap_or_default();
            let text = excerpt(message);
            notices.push(Notice {
                conversation: message.conversation_id.clone(),
                message: message.message_id.clone(),
                title: format!("{who} reacted {}{place}", emoji.join(" ")),
                text: if text.is_empty() {
                    "To your message".into()
                } else {
                    text
                },
                can_reply: false,
                reaction: true,
            });
        }
        notices
    }

    fn remember(&mut self, key: String) {
        self.shown.push_back(key);
        if self.shown.len() > REMEMBERED {
            self.shown.pop_front();
        }
    }
}

/// A reaction in text: an emoji of the organization's own has only a name there.
fn shown(reaction: &str) -> String {
    match crate::media::custom_emoji(reaction) {
        Some(emoji) => format!(":{}:", emoji.name),
        None => match reaction {
            "like" => "👍",
            "heart" => "❤️",
            "laugh" => "😆",
            "surprised" => "😮",
            "sad" => "🙁",
            "angry" => "😠",
            other => other,
        }
        .to_owned(),
    }
}

fn excerpt(message: &MessageRecord) -> String {
    readable_text(message)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_TEXT)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minute: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + minute * 60, 0).unwrap()
    }

    fn message(id: &str) -> MessageRecord {
        MessageRecord {
            conversation_id: "c".into(),
            message_id: id.into(),
            sender_id: Some("ann".into()),
            sender_name: Some("Ann".into()),
            created_at: at(5),
            body_html: "<p>Hi <b>there</b></p>".into(),
            ..Default::default()
        }
    }

    fn chat() -> ChatRecord {
        ChatRecord {
            id: "c".into(),
            kind: "oneOnOne".into(),
            ..Default::default()
        }
    }

    fn decide(message: &MessageRecord, chat: Option<&ChatRecord>) -> Option<Notice> {
        Rules::new(at(0)).decide(message, chat, Some("me"), at(6))
    }

    #[test]
    fn only_a_call_the_user_missed_notifies() {
        let call = |by: &str, people: &[&str], phase: &str| {
            let mut message = message("1");
            message.sender_id = Some(by.into());
            message.call_event_json = Some(
                serde_json::json!({
                    "phase": phase, "call_type": "Call", "call_id": "x",
                    "duration_seconds": null, "initiator_id": by, "initiator_name": "Ann",
                    "participant_ids": people,
                })
                .to_string(),
            );
            message
        };
        let notice = decide(&call("ann", &["ann"], "Ended"), Some(&chat())).unwrap();
        assert_eq!(notice.title, "Missed call");
        assert!(notice.text.starts_with("Ann"));
        for other in [
            call("ann", &["ann", "me"], "Ended"),
            call("me", &["me"], "Ended"),
            call("ann", &["ann"], "Started"),
        ] {
            assert_eq!(decide(&other, Some(&chat())), None);
        }
        assert_eq!(decide(&call("ann", &["ann"], "Ended"), None), None);
    }

    #[test]
    fn a_chat_message_notifies_once_as_plain_text() {
        let mut rules = Rules::new(at(0));
        let notice = rules
            .decide(&message("1"), Some(&chat()), Some("me"), at(6))
            .unwrap();
        assert_eq!(
            (notice.title.as_str(), notice.text.as_str()),
            ("Ann", "Hi there")
        );
        assert!(notice.can_reply);
        assert_eq!(
            rules.decide(&message("1"), Some(&chat()), Some("me"), at(6)),
            None
        );
    }

    #[test]
    fn what_stays_quiet() {
        let own = MessageRecord {
            sender_id: Some("me".into()),
            ..message("1")
        };
        let old = MessageRecord {
            created_at: at(-5),
            ..message("1")
        };
        let deleted = MessageRecord {
            deleted: true,
            ..message("1")
        };
        let event = MessageRecord {
            sender_id: None,
            ..message("1")
        };
        for quiet in [own, old, deleted, event] {
            assert_eq!(decide(&quiet, Some(&chat())), None);
        }
        let read = ChatRecord {
            last_read_at: Some(at(5)),
            ..chat()
        };
        assert_eq!(decide(&message("1"), Some(&read)), None);
        let muted = ChatRecord {
            muted: true,
            ..chat()
        };
        assert_eq!(decide(&message("1"), Some(&muted)), None);
        assert_eq!(decide(&message("1"), None), None);
        assert_eq!(
            Rules::new(at(0)).decide(&message("1"), Some(&chat()), None, at(6)),
            None
        );
    }

    fn reacted(id: &str, all: &[(&str, &str, i64)]) -> MessageRecord {
        let all: Vec<_> = all
            .iter()
            .map(|(user, kind, minute)| {
                serde_json::json!({
                    "reaction_type": kind, "user_id": user, "user_name": null,
                    "created_at": at(*minute)
                })
            })
            .collect();
        MessageRecord {
            sender_id: Some("me".into()),
            reactions_json: serde_json::Value::from(all).to_string(),
            ..message(id)
        }
    }

    #[test]
    fn reactions_to_own_messages_notify_once() {
        let mut rules = Rules::new(at(0));
        let name = |id: &str| (id != "xx").then(|| id.to_uppercase());
        let mut told = |messages: &[MessageRecord], chat: Option<&ChatRecord>, minute| {
            rules.reactions(messages, chat, "ME", name, at(minute))
        };
        let first = reacted("1", &[("ann", "👍", 5), ("me", "❤", 5)]);
        let notices = told(std::slice::from_ref(&first), Some(&chat()), 6);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "ANN reacted 👍");
        assert_eq!(notices[0].text, "Hi there");
        assert!(notices[0].reaction && !notices[0].can_reply);
        assert!(told(&[first], Some(&chat()), 6).is_empty(), "once");
        // Others join, in a group.
        let more = reacted(
            "1",
            &[
                ("ann", "👍", 5),
                ("bo", "👍", 7),
                ("cy", "party;0-eus-d1-ab", 7),
                ("xx", "like", 7),
            ],
        );
        let group = ChatRecord {
            kind: "group".into(),
            title: "Crew".into(),
            ..chat()
        };
        let notices = told(&[more], Some(&group), 8);
        assert_eq!(
            notices[0].title,
            "BO and 2 others reacted 👍 :party: in Crew"
        );
        // Given again, or from before the start, or long ago.
        assert!(told(&[reacted("1", &[("ann", "👍", 9)])], Some(&chat()), 9).is_empty());
        assert!(told(&[reacted("2", &[("ann", "👍", -1)])], Some(&chat()), 9).is_empty());
        assert!(told(&[reacted("3", &[("ann", "👍", 5)])], Some(&chat()), 24 * 60).is_empty());
        // To somebody else's message, or in a muted chat.
        let other = MessageRecord {
            sender_id: Some("bo".into()),
            ..reacted("4", &[("ann", "👍", 5)])
        };
        assert!(told(&[other], Some(&chat()), 6).is_empty());
        let muted = ChatRecord {
            muted: true,
            ..chat()
        };
        assert!(told(&[reacted("5", &[("ann", "👍", 5)])], Some(&muted), 6).is_empty());
    }

    #[test]
    fn a_mention_gets_through_mute_and_from_channels() {
        let mention = MessageRecord {
            mentions_json: r#"[{"user_id":"me","name":"Me"}]"#.into(),
            ..message("1")
        };
        let muted = ChatRecord {
            muted: true,
            ..chat()
        };
        assert!(decide(&mention, Some(&muted)).is_some());
        let notice = decide(&mention, None).unwrap();
        assert_eq!(notice.title, "Ann mentioned you");
        assert!(!notice.can_reply);
    }
}
