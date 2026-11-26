//! The calendar: what is kept of it, when to remind of a meeting, and where joining may lead.

use chatsvc::{CalendarEvent, ResponseType, ShowAs};
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use store::Store;

use crate::backend::{Failure, Teams};

const META: &str = "tern.agenda";
/// How much of the calendar is kept, in days.
const BACK: i64 = 1;
const AHEAD: i64 = 14;
/// The service answers only so many reminders; nearer ones matter.
const REMINDERS: i64 = 2;
/// For meetings whose own reminder is not known yet.
const USUAL: i64 = 15;
/// A meeting that began this long ago is no longer told of.
const LATE: i64 = 5;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Organizer,
    Accepted,
    Tentative,
    Declined,
    #[default]
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meeting {
    pub id: String,
    pub subject: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub all_day: bool,
    pub cancelled: bool,
    /// Where to join, if that is a Teams meeting.
    pub join: Option<String>,
    /// Its chat.
    pub chat: Option<String>,
    pub organizer: Option<String>,
    pub location: Option<String>,
    pub answer: Answer,
    /// The user is shown as free meanwhile.
    pub free: bool,
    /// When to tell; none when the user wants none.
    pub remind: Option<DateTime<Utc>>,
}

impl Meeting {
    /// The first and the last day of the meeting, in `zone`.
    pub fn days<Z: TimeZone>(&self, zone: &Z) -> (NaiveDate, NaiveDate) {
        // Until just before the end: what ends at midnight is not on the next day.
        let last = (self.end - Duration::seconds(1)).max(self.start);
        if self.all_day {
            // Whole days come as midnight in UTC, wherever the user is.
            return (self.start.date_naive(), last.date_naive());
        }
        let day = |time: DateTime<Utc>| time.with_timezone(zone).date_naive();
        (day(self.start), day(last))
    }

    pub fn is_now(&self, now: DateTime<Utc>) -> bool {
        self.start <= now && now < self.end
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agenda {
    pub read: DateTime<Utc>,
    pub meetings: Vec<Meeting>,
}

/// The calendar as it was last read; `None` before that.
pub fn agenda(store: &Store) -> Option<Agenda> {
    let text = store.meta(META).ok().flatten()?;
    serde_json::from_str(&text).ok()
}

/// Keeps it in the cache; answers whether the meetings changed.
pub fn keep_agenda(store: &Store, new: &Agenda) -> Result<bool, store::Error> {
    let changed = agenda(store).is_none_or(|old| old.meetings != new.meetings);
    let text = serde_json::to_string(new).unwrap_or_default();
    store.set_meta(META, &text)?;
    Ok(changed)
}

fn meeting(event: CalendarEvent) -> Meeting {
    Meeting {
        join: event.join_url.filter(|url| meeting_address(url).is_some()),
        id: event.object_id,
        subject: event.subject.trim().to_owned(),
        start: event.start,
        end: event.end,
        all_day: event.all_day,
        cancelled: event.cancelled,
        chat: event.thread_id,
        organizer: event.organizer_name,
        location: event.location,
        answer: match event.response {
            ResponseType::Organizer => Answer::Organizer,
            ResponseType::Accepted => Answer::Accepted,
            ResponseType::Tentative => Answer::Tentative,
            ResponseType::Declined => Answer::Declined,
            ResponseType::NotResponded => Answer::None,
        },
        free: event.show_as == ShowAs::Free,
        remind: None,
    }
}

/// Gives every meeting its time to tell. `known` are the reminders the service answered, for
/// meetings until `covered`; `None` when it did not answer.
fn set_reminders(
    meetings: &mut [Meeting],
    known: Option<&[(String, DateTime<Utc>)]>,
    covered: DateTime<Utc>,
) {
    for meeting in meetings.iter_mut().filter(|meeting| !meeting.all_day) {
        meeting.remind = match known {
            Some(known) if meeting.start < covered => known
                .iter()
                .find(|(id, _)| *id == meeting.id)
                .map(|(_, at)| *at),
            _ => Some(meeting.start - Duration::minutes(USUAL)),
        };
    }
}

impl Teams {
    /// Reads the calendar; answers whether it changed.
    pub async fn read_agenda(&self, now: DateTime<Utc>) -> Result<bool, Failure> {
        let failure =
            |error: chatsvc::Error| -> Failure { Failure::from(teams_core::Error::from(error)) };
        let (from, to) = (now - Duration::days(BACK), now + Duration::days(AHEAD));
        let events = self
            .calendar
            .calendar_view(from, to)
            .await
            .map_err(failure)?;
        let mut meetings: Vec<Meeting> = events.into_iter().map(meeting).collect();
        let covered = now + Duration::days(REMINDERS);
        // Reminders are asked for by when the meeting is.
        let reminders = self
            .calendar
            .reminder_view(now - Duration::hours(1), covered)
            .await;
        let known: Option<Vec<_>> = match reminders {
            Ok(found) => Some(
                found
                    .into_iter()
                    .map(|reminder| (reminder.event_id, reminder.fire_time))
                    .collect(),
            ),
            Err(error) => {
                tracing::debug!(error = failure(error).text(), "reminders");
                None
            }
        };
        set_reminders(&mut meetings, known.as_deref(), covered);
        let read = Agenda {
            read: now,
            meetings,
        };
        keep_agenda(self.store(), &read).map_err(|error| Failure::Temporary(error.to_string()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Soon,
    /// It begins.
    Now,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reminder {
    pub meeting: Meeting,
    pub stage: Stage,
}

/// Remembers what was told.
#[derive(Default)]
pub struct Reminders {
    told: Vec<(String, DateTime<Utc>, Stage)>,
}

impl Reminders {
    /// What is to be told now. Each is answered once: a moved meeting is another one.
    pub fn due(&mut self, meetings: &[Meeting], now: DateTime<Utc>) -> Vec<Reminder> {
        let day_ago = now - Duration::days(1);
        self.told.retain(|(_, start, _)| *start > day_ago);
        let mut due = Vec::new();
        for meeting in meetings {
            let stage = match stage(meeting, now) {
                Some(stage) => stage,
                None => continue,
            };
            let key = (meeting.id.clone(), meeting.start, stage);
            if !self.told.contains(&key) {
                self.told.push(key);
                due.push(Reminder {
                    meeting: meeting.clone(),
                    stage,
                });
            }
        }
        due
    }
}

fn stage(meeting: &Meeting, now: DateTime<Utc>) -> Option<Stage> {
    let remind = meeting.remind?;
    if meeting.all_day || meeting.cancelled || meeting.answer == Answer::Declined {
        return None;
    }
    if now >= meeting.start {
        let late = now >= meeting.start + Duration::minutes(LATE) || now >= meeting.end;
        return (!late).then_some(Stage::Now);
    }
    (now >= remind).then_some(Stage::Soon)
}

/// When `due` may next have something to say.
pub fn next_reminder(meetings: &[Meeting], now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    meetings
        .iter()
        .filter_map(|meeting| Some([meeting.remind?, meeting.start]))
        .flatten()
        .filter(|time| *time > now)
        .min()
}

/// Microsoft's own, as invitations come from anybody.
const HOSTS: [&str; 6] = [
    "teams.microsoft.com",
    "teams.cloud.microsoft",
    "teams.live.com",
    "gov.teams.microsoft.us",
    "dod.teams.microsoft.us",
    "teams.microsoftonline.cn",
];

/// What a Teams address is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// A meeting, at this address.
    Meeting(String),
    Chat(String),
    /// Something only Teams itself shows.
    Other(String),
}

/// Where a Teams address leads, also one that begins with `msteams:`. `None` when it leads
/// anywhere else.
pub fn place(address: &str) -> Option<Place> {
    let address = address.trim();
    if address.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let lower = address.to_ascii_lowercase();
    let url = if lower.starts_with("https://") {
        url::Url::parse(address).ok()?
    } else if lower.starts_with("msteams:") {
        // `msteams:/l/…`, and with two slashes also with or without a host.
        let rest = &address["msteams:".len()..];
        let rest = match rest.strip_prefix("//") {
            Some(rest) if !rest.starts_with("l/") && !rest.starts_with('/') => {
                let (host, path) = rest.split_once('/')?;
                if !HOSTS.contains(&host.to_ascii_lowercase().as_str()) {
                    return None;
                }
                path
            }
            Some(rest) => rest,
            None => rest,
        };
        let rest = rest.trim_start_matches('/');
        url::Url::parse(&format!("https://{}/{rest}", HOSTS[0])).ok()?
    } else {
        return None;
    };
    let host = url.host_str()?.to_ascii_lowercase();
    let plain = url.username().is_empty() && url.password().is_none() && url.port().is_none();
    if !HOSTS.contains(&host.as_str()) || !plain {
        return None;
    }
    let mut parts = url.path_segments()?.filter(|part| !part.is_empty());
    let decoded = |part: &str| {
        percent_encoding::percent_decode_str(part)
            .decode_utf8()
            .ok()
            .map(|text| text.into_owned())
    };
    Some(match (parts.next(), parts.next(), parts.next()) {
        (Some("l"), Some("meetup-join"), Some(_)) | (Some("meet"), Some(_), _) => {
            Place::Meeting(url.into())
        }
        // `/l/chat/0/0` is about people instead.
        (Some("l"), Some("chat" | "message"), Some(id)) => match decoded(id) {
            Some(id) if id.contains(':') => Place::Chat(id),
            _ => Place::Other(url.into()),
        },
        _ => Place::Other(url.into()),
    })
}

/// The address to join at, if it is one of a meeting in Teams.
pub fn meeting_address(address: &str) -> Option<String> {
    match place(address) {
        Some(Place::Meeting(address)) => Some(address),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn standup() -> Meeting {
        Meeting {
            id: "m".into(),
            subject: "Standup".into(),
            start: at("2026-11-25T09:00:00Z"),
            end: at("2026-11-25T09:30:00Z"),
            all_day: false,
            cancelled: false,
            join: None,
            chat: None,
            organizer: None,
            location: None,
            answer: Answer::Accepted,
            free: false,
            remind: Some(at("2026-11-25T08:50:00Z")),
        }
    }

    #[test]
    fn a_meeting_is_told_once_soon_and_once_as_it_begins() {
        let mut reminders = Reminders::default();
        let meetings = [standup()];
        let mut stages = |time: &str| -> Vec<Stage> {
            let due = reminders.due(&meetings, at(time));
            due.into_iter().map(|reminder| reminder.stage).collect()
        };
        assert_eq!(stages("2026-11-25T08:49:59Z"), []);
        assert_eq!(stages("2026-11-25T08:50:00Z"), [Stage::Soon]);
        assert_eq!(stages("2026-11-25T08:55:00Z"), []);
        assert_eq!(stages("2026-11-25T09:00:00Z"), [Stage::Now]);
        assert_eq!(stages("2026-11-25T09:01:00Z"), []);
    }

    #[test]
    fn after_sleep_a_running_meeting_is_told_but_not_an_old_one() {
        let meetings = [standup()];
        let due = Reminders::default().due(&meetings, at("2026-11-25T09:04:00Z"));
        assert_eq!(due[0].stage, Stage::Now);
        assert!(
            Reminders::default()
                .due(&meetings, at("2026-11-25T09:05:00Z"))
                .is_empty()
        );
    }

    #[test]
    fn a_moved_meeting_is_told_again() {
        let mut reminders = Reminders::default();
        let mut meeting = standup();
        assert_eq!(reminders.due(&[meeting.clone()], meeting.start).len(), 1);
        meeting.start += Duration::hours(1);
        meeting.end += Duration::hours(1);
        meeting.remind = Some(meeting.start);
        assert_eq!(reminders.due(&[meeting.clone()], meeting.start).len(), 1);
    }

    #[test]
    fn some_are_not_told() {
        let now = at("2026-11-25T08:55:00Z");
        let with = |change: fn(&mut Meeting)| {
            let mut meeting = standup();
            change(&mut meeting);
            Reminders::default().due(&[meeting], now).len()
        };
        assert_eq!(with(|_| {}), 1);
        assert_eq!(with(|meeting| meeting.cancelled = true), 0);
        assert_eq!(with(|meeting| meeting.answer = Answer::Declined), 0);
        assert_eq!(with(|meeting| meeting.all_day = true), 0);
        assert_eq!(with(|meeting| meeting.remind = None), 0);
    }

    #[test]
    fn the_next_time_to_look_is_the_nearest_ahead() {
        let meetings = [standup()];
        assert_eq!(
            next_reminder(&meetings, at("2026-11-25T08:00:00Z")),
            Some(at("2026-11-25T08:50:00Z"))
        );
        assert_eq!(
            next_reminder(&meetings, at("2026-11-25T08:50:00Z")),
            Some(at("2026-11-25T09:00:00Z"))
        );
        assert_eq!(next_reminder(&meetings, at("2026-11-25T09:00:00Z")), None);
    }

    #[test]
    fn the_users_own_reminder_counts_as_far_as_it_is_known() {
        let covered = at("2026-11-27T00:00:00Z");
        let mut off = standup();
        off.id = "off".into();
        let mut later = standup();
        later.id = "later".into();
        later.start = at("2026-11-30T09:00:00Z");
        let mut meetings = [standup(), off, later];
        let known = [("m".to_owned(), at("2026-11-25T08:30:00Z"))];
        set_reminders(&mut meetings, Some(&known), covered);
        assert_eq!(meetings[0].remind, Some(at("2026-11-25T08:30:00Z")));
        assert_eq!(meetings[1].remind, None, "the user wants none");
        assert_eq!(meetings[2].remind, Some(at("2026-11-30T08:45:00Z")));
        set_reminders(&mut meetings, None, covered);
        assert_eq!(meetings[1].remind, Some(at("2026-11-25T08:45:00Z")));
    }

    #[test]
    fn whole_days_stay_on_their_days_everywhere() {
        let west = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
        let mut meeting = standup();
        meeting.all_day = true;
        meeting.start = at("2026-11-27T00:00:00Z");
        meeting.end = at("2026-11-29T00:00:00Z");
        let days = meeting.days(&west);
        assert_eq!(days.0.to_string(), "2026-11-27");
        assert_eq!(days.1.to_string(), "2026-11-28");
        meeting.all_day = false;
        assert_eq!(meeting.days(&west).0.to_string(), "2026-11-26");
    }

    #[test]
    fn the_agenda_is_kept_and_tells_whether_it_changed() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(agenda(&store), None);
        let mut kept = Agenda {
            read: at("2026-11-25T08:00:00Z"),
            meetings: vec![standup()],
        };
        assert!(keep_agenda(&store, &kept).unwrap());
        kept.read = at("2026-11-25T09:00:00Z");
        assert!(!keep_agenda(&store, &kept).unwrap());
        assert_eq!(agenda(&store), Some(kept.clone()));
        kept.meetings.clear();
        assert!(keep_agenda(&store, &kept).unwrap());
    }

    #[test]
    fn only_teams_itself_is_joined() {
        let good =
            "https://teams.microsoft.com/l/meetup-join/19%3ameeting_a%40thread.v2/0?context=%7b%7d";
        assert_eq!(meeting_address(good).as_deref(), Some(good));
        assert!(meeting_address("https://teams.live.com/meet/123?p=x").is_some());
        assert!(meeting_address("https://teams.microsoft.com/meet/123").is_some());
        for bad in [
            "http://teams.microsoft.com/l/meetup-join/a/0",
            "https://teams.microsoft.com.evil.example/l/meetup-join/a/0",
            "https://teams.microsoft.com@evil.example/l/meetup-join/a/0",
            "https://evil.example/l/meetup-join/a/0",
            "https://user@teams.microsoft.com/l/meetup-join/a/0",
            "https://teams.microsoft.com:8443/l/meetup-join/a/0",
            "https://teams.microsoft.com/l/meetup-join/a/0 --flag",
            "file:///l/meetup-join/a/0",
            "javascript:alert(1)",
            "https://teams.microsoft.com/l/app/x",
            "--app=https://teams.microsoft.com/l/meetup-join/a/0",
            "",
        ] {
            assert_eq!(meeting_address(bad), None, "{bad}");
        }
    }

    #[test]
    fn msteams_addresses_lead_to_the_same_places() {
        let join = "https://teams.microsoft.com/l/meetup-join/a/0";
        for address in [
            "msteams:/l/meetup-join/a/0",
            "msteams://l/meetup-join/a/0",
            "MSTEAMS:l/meetup-join/a/0",
            "msteams://teams.microsoft.com/l/meetup-join/a/0",
            "msteams:///l/meetup-join/a/0",
        ] {
            assert_eq!(
                place(address),
                Some(Place::Meeting(join.into())),
                "{address}"
            );
        }
        assert_eq!(
            place("msteams:/l/chat/19%3Ag%40thread.v2/conversations"),
            Some(Place::Chat("19:g@thread.v2".into()))
        );
        assert_eq!(
            place("https://teams.microsoft.com/l/message/19:g@thread.v2/1?x=y"),
            Some(Place::Chat("19:g@thread.v2".into()))
        );
        assert!(matches!(
            place("msteams:/l/chat/0/0?users=a@b.example"),
            Some(Place::Other(_))
        ));
        assert_eq!(place("msteams://evil.example/l/meetup-join/a/0"), None);
        // The host cannot be changed by what follows.
        for address in ["msteams:@evil.example/x", "msteams:/\\evil.example/x"] {
            if let Some(Place::Other(url) | Place::Meeting(url)) = place(address) {
                assert!(url.starts_with("https://teams.microsoft.com/"), "{url}");
            }
        }
    }
}
