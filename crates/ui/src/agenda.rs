//! The calendar of the next two weeks, day by day.

use chrono::{DateTime, Days, Local, NaiveDate, TimeZone, Utc};
use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use tern_engine::{Agenda, Answer, Meeting};

use crate::row::say;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub subject: String,
    /// "09:00 to 09:30", or what there is to say instead.
    pub time: String,
    pub about: String,
    /// How the user answered, where that says something.
    pub note: Option<&'static str>,
    pub join: Option<String>,
    pub chat: Option<String>,
    pub state: State,
    pub cancelled: bool,
    pub declined: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Past,
    Now,
    Ahead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Day {
    pub title: String,
    pub entries: Vec<Entry>,
}

const DAYS: u64 = 14;

fn title(day: NaiveDate, today: NaiveDate) -> String {
    let date = day.format("%A, %-d %B");
    match (day - today).num_days() {
        0 => format!("Today, {date}"),
        1 => format!("Tomorrow, {date}"),
        _ => date.to_string(),
    }
}

fn entry<Z: TimeZone>(meeting: &Meeting, day: NaiveDate, now: DateTime<Utc>, zone: &Z) -> Entry
where
    Z::Offset: std::fmt::Display,
{
    let clock = |time: DateTime<Utc>| time.with_timezone(zone).format("%H:%M").to_string();
    let (first, last) = meeting.days(zone);
    let time = if meeting.all_day || (first < day && day < last) {
        "All day".to_owned()
    } else if first == last {
        format!("{} to {}", clock(meeting.start), clock(meeting.end))
    } else if day == first {
        format!("From {}", clock(meeting.start))
    } else {
        format!("Until {}", clock(meeting.end))
    };
    let about: Vec<&str> = [&meeting.location, &meeting.organizer]
        .into_iter()
        .flatten()
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
        .collect();
    Entry {
        subject: match meeting.subject.as_str() {
            "" => "No subject".into(),
            subject => subject.into(),
        },
        time,
        about: about.join(" · "),
        note: match (meeting.cancelled, meeting.answer) {
            (true, _) => Some("Cancelled"),
            (_, Answer::Declined) => Some("Declined"),
            (_, Answer::Tentative) => Some("Tentative"),
            (_, Answer::None) if !meeting.all_day => Some("Not answered"),
            _ => None,
        },
        join: meeting.join.clone().filter(|_| !meeting.cancelled),
        chat: meeting.chat.clone(),
        state: match meeting.all_day {
            // It is as long as the day is.
            true => State::Ahead,
            false if meeting.end <= now => State::Past,
            false if meeting.is_now(now) => State::Now,
            false => State::Ahead,
        },
        cancelled: meeting.cancelled,
        declined: meeting.answer == Answer::Declined,
    }
}

/// The days from today on, those without meetings left out except today.
pub fn days<Z: TimeZone>(meetings: &[Meeting], now: DateTime<Utc>, zone: &Z) -> Vec<Day>
where
    Z::Offset: std::fmt::Display,
{
    let today = now.with_timezone(zone).date_naive();
    (0..DAYS)
        .filter_map(|ahead| today.checked_add_days(Days::new(ahead)))
        .filter_map(|day| {
            let mut of_day: Vec<&Meeting> = meetings
                .iter()
                .filter(|meeting| {
                    let (first, last) = meeting.days(zone);
                    first <= day && day <= last
                })
                .collect();
            of_day.sort_by_key(|meeting| (!meeting.all_day, meeting.start, meeting.end));
            if of_day.is_empty() && day != today {
                return None;
            }
            Some(Day {
                title: title(day, today),
                entries: of_day
                    .into_iter()
                    .map(|meeting| entry(meeting, day, now, zone))
                    .collect(),
            })
        })
        .collect()
}

fn row(entry: &Entry) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(&entry.subject)
        .use_markup(false)
        .title_lines(2)
        .subtitle_lines(1)
        .build();
    let time = gtk::Label::builder()
        // Narrow windows have little room beside the subject.
        .label(entry.time.replace(" to ", "\n"))
        .xalign(0.0)
        .width_chars(7)
        .build();
    time.add_css_class("numeric");
    time.add_css_class("caption");
    time.add_css_class("dim-label");
    row.add_prefix(&time);
    let about = [entry.note.unwrap_or_default(), &entry.about];
    let about: Vec<&str> = about.into_iter().filter(|text| !text.is_empty()).collect();
    row.set_subtitle(&about.join(" · "));
    row.add_css_class("meeting");
    if entry.state == State::Past || entry.cancelled || entry.declined {
        row.add_css_class("past");
    }
    if entry.state == State::Now {
        row.add_css_class("now");
    }
    if let Some(chat) = &entry.chat {
        let button = gtk::Button::builder()
            .icon_name("mail-unread-symbolic")
            .tooltip_text("Open its chat")
            .valign(gtk::Align::Center)
            .action_name("win.show")
            .action_target(&chat.to_variant())
            .build();
        button.add_css_class("flat");
        say(&button, &format!("Open the chat of {}", entry.subject));
        row.add_suffix(&button);
    }
    if let Some(address) = &entry.join
        && entry.state != State::Past
    {
        let button = gtk::Button::builder()
            .label("Join")
            .valign(gtk::Align::Center)
            .action_name("app.join")
            .action_target(&address.to_variant())
            .build();
        if entry.state == State::Now {
            button.add_css_class("suggested-action");
        }
        say(&button, &format!("Join {}", entry.subject));
        // Tern joins with sound only; video and sharing are the browser's.
        if crate::join::here(address).is_some() {
            let browser = gtk::Button::builder()
                .icon_name("web-browser-symbolic")
                .tooltip_text("Join in the browser, for video and sharing")
                .valign(gtk::Align::Center)
                .action_name("app.join-in-browser")
                .action_target(&address.to_variant())
                .build();
            browser.add_css_class("flat");
            say(&browser, &format!("Join {} in the browser", entry.subject));
            row.add_suffix(&browser);
        }
        row.add_suffix(&button);
    }
    row
}

/// The sign-in of the browser may end some day.
fn sign_in() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    let row = adw::ActionRow::builder()
        .title("Joining as a guest?")
        .subtitle("Meetings open in a browser window, which has a sign-in of its own")
        .build();
    let button = gtk::Button::builder()
        .label("Sign In")
        .valign(gtk::Align::Center)
        .action_name("app.join")
        .action_target(&crate::join::TEAMS.to_variant())
        .build();
    say(&button, "Sign in for meetings");
    row.add_suffix(&button);
    group.add(&row);
    group
}

pub struct View {
    pub root: gtk::Stack,
    page: adw::PreferencesPage,
    groups: std::cell::RefCell<Vec<adw::PreferencesGroup>>,
    shown: std::cell::RefCell<Vec<Day>>,
}

impl Default for View {
    fn default() -> Self {
        let root = gtk::Stack::new();
        let page = adw::PreferencesPage::new();
        let reading = adw::StatusPage::builder()
            .icon_name("x-office-calendar-symbolic")
            .title("The calendar was not read yet")
            .description("It appears here as soon as there is a connection")
            .build();
        root.add_named(&reading, Some("unread"));
        root.add_named(&page, Some("days"));
        View {
            root,
            page,
            groups: Default::default(),
            shown: Default::default(),
        }
    }
}

impl View {
    pub fn set(&self, agenda: Option<&Agenda>) {
        let Some(agenda) = agenda else {
            self.root.set_visible_child_name("unread");
            return;
        };
        self.root.set_visible_child_name("days");
        let days = days(&agenda.meetings, Utc::now(), &Local);
        // What has the focus stays, unless something is different.
        if *self.shown.borrow() == days {
            return;
        }
        for group in self.groups.take() {
            self.page.remove(&group);
        }
        for day in &days {
            let group = adw::PreferencesGroup::builder()
                .title(gtk::glib::markup_escape_text(&day.title))
                .build();
            if day.entries.is_empty() {
                group.set_description(Some("Nothing today"));
            }
            for entry in &day.entries {
                group.add(&row(entry));
            }
            self.page.add(&group);
            self.groups.borrow_mut().push(group);
        }
        let last = sign_in();
        self.page.add(&last);
        self.groups.borrow_mut().push(last);
        self.shown.replace(days);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, FixedOffset};

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn zone() -> FixedOffset {
        FixedOffset::west_opt(5 * 3600).unwrap()
    }

    fn meeting(subject: &str, start: &str, minutes: i64) -> Meeting {
        Meeting {
            id: subject.into(),
            subject: subject.into(),
            start: at(start),
            end: at(start) + Duration::minutes(minutes),
            all_day: false,
            cancelled: false,
            join: Some("https://teams.microsoft.com/l/meetup-join/a/0".into()),
            chat: None,
            organizer: Some("Ada".into()),
            location: None,
            answer: Answer::Accepted,
            free: false,
            remind: None,
        }
    }

    #[test]
    fn days_follow_the_users_own_clock() {
        let meetings = [
            // Still the 25th in the west.
            meeting("late", "2026-11-26T02:00:00Z", 30),
            meeting("early", "2026-11-25T14:00:00Z", 30),
        ];
        let days = days(&meetings, at("2026-11-25T14:10:00Z"), &zone());
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].title, "Today, Wednesday, 25 November");
        let subjects: Vec<_> = days[0].entries.iter().map(|e| e.subject.as_str()).collect();
        assert_eq!(subjects, ["early", "late"]);
        assert_eq!(days[0].entries[0].time, "09:00 to 09:30");
        assert_eq!(days[0].entries[0].state, State::Now);
        assert_eq!(days[0].entries[1].state, State::Ahead);
    }

    #[test]
    fn today_is_there_without_meetings_and_other_days_are_not() {
        let meetings = [meeting("far", "2026-11-28T14:00:00Z", 30)];
        let days = days(&meetings, at("2026-11-25T14:00:00Z"), &zone());
        assert_eq!(days.len(), 2);
        assert!(days[0].entries.is_empty());
        assert_eq!(days[1].title, "Saturday, 28 November");
    }

    #[test]
    fn what_is_longer_than_a_day_is_on_each_of_them() {
        let mut trip = meeting("trip", "2026-11-26T00:00:00Z", 2 * 24 * 60);
        trip.all_day = true;
        let over = meeting("over midnight", "2026-11-26T04:00:00Z", 120);
        let days = days(&[over, trip], at("2026-11-26T03:00:00Z"), &zone());
        let of = |day: usize| -> Vec<(&str, &str)> {
            days[day]
                .entries
                .iter()
                .map(|entry| (entry.subject.as_str(), entry.time.as_str()))
                .collect()
        };
        assert_eq!(of(0), [("over midnight", "From 23:00")]);
        assert_eq!(
            of(1),
            [("trip", "All day"), ("over midnight", "Until 01:00")]
        );
        assert_eq!(of(2), [("trip", "All day")]);
        assert_eq!(days.len(), 3);
    }

    #[test]
    fn a_cancelled_meeting_cannot_be_joined() {
        let mut gone = meeting("gone", "2026-11-25T15:00:00Z", 30);
        gone.cancelled = true;
        let days = days(&[gone], at("2026-11-25T14:00:00Z"), &zone());
        assert_eq!(days[0].entries[0].join, None);
        assert_eq!(days[0].entries[0].note, Some("Cancelled"));
    }
}
