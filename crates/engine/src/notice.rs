//! Which messages deserve a notification, and what it says.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use store::{ChatRecord, MessageRecord};
use teams_core::{mentions, mentions_whole_chat, readable_text};

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
        if own || stale || message.deleted || message.call_event_json.is_some() {
            return None;
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
        self.shown.push_back(message.message_id.clone());
        if self.shown.len() > REMEMBERED {
            self.shown.pop_front();
        }
        let name = message.sender_name.as_deref().unwrap_or("Someone");
        let title = match chat {
            Some(chat) if chat.kind.eq_ignore_ascii_case("oneOnOne") => name.to_owned(),
            Some(chat) => format!("{name} in {}", chat.display_title()),
            None => format!("{name} mentioned you"),
        };
        let everyone = chat.is_some() && mentions_whole_chat(message);
        let mut text: String = readable_text(message)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(MAX_TEXT)
            .collect();
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
        })
    }
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
