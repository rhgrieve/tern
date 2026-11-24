//! Invented people and messages, for trying the window without an account.

use chrono::{Duration, Utc};
use store::{ChatRecord, MemberRecord, MessageRecord, Store};
use tern_ipc::Availability;

const CARD: &str = r#"{"type":"AdaptiveCard","version":"1.5","body":[
{"type":"TextBlock","text":"North buoy is off its place","size":"Large","weight":"Bolder","wrap":true},
{"type":"Badge","text":"Needs a boat","style":"Warning"},
{"type":"TextBlock","text":"It moved **40 m** since noon. The [chart](https://example.com/chart) shows where to.","wrap":true},
{"type":"FactSet","facts":[{"title":"Found","value":"10:20"},{"title":"By","value":"Ada Quill"}]},
{"type":"ColumnSet","separator":true,"columns":[
{"type":"Column","width":"stretch","items":[{"type":"TextBlock","text":"Wind","isSubtle":true},{"type":"TextBlock","text":"6.2 m/s","size":"ExtraLarge"}]},
{"type":"Column","width":"stretch","items":[{"type":"TextBlock","text":"Tide","isSubtle":true},{"type":"TextBlock","text":"falling","size":"ExtraLarge","color":"Good"}]}]},
{"type":"Container","style":"attention","items":[{"type":"TextBlock","text":"Dark in two hours","wrap":true}]}],
"actions":[{"type":"Action.OpenUrl","title":"Open chart","url":"https://example.com/chart"},
{"type":"Action.ShowCard","title":"History","card":{"type":"AdaptiveCard","body":[{"type":"TextBlock","text":"Also in March"}]}},
{"type":"Action.Submit","title":"Send a boat"},{"type":"Action.Submit","title":"Wait"}]}"#;

/// Who is there.
pub fn presence() -> Vec<(String, Availability)> {
    [
        ("me", Availability::Available),
        ("ada", Availability::Available),
        ("bo", Availability::DoNotDisturb),
        ("cy", Availability::Away),
        ("dee", Availability::Offline),
    ]
    .map(|(person, availability)| (person.to_owned(), availability))
    .into()
}

pub fn store() -> Store {
    let store = Store::open_in_memory().expect("in memory");
    let _ = store.set_meta("me_user_id", "me");
    let _ = store.set_meta("me_display_name", "Robin Vale");
    let now = Utc::now();
    let people = [
        ("ada", "Ada Quill"),
        ("bo", "Bo Lindqvist"),
        ("cy", "Cyrus Amadi"),
        ("dee", "Dee Okafor"),
    ];
    let chats = [
        ("c1", "oneOnOne", "", "Ada Quill", true, 2),
        ("c2", "group", "Harbour launch", "", true, 9),
        ("c3", "oneOnOne", "", "Bo Lindqvist", false, 70),
        ("c4", "group", "Lighthouse crew", "", false, 60 * 26),
        ("c5", "oneOnOne", "", "Dee Okafor", false, 60 * 24 * 4),
        ("c6", "group", "Regatta planning", "", false, 60 * 24 * 5),
        ("c7", "oneOnOne", "", "Cyrus Amadi", false, 60 * 24 * 6),
        ("c8", "group", "Night watch", "", false, 60 * 24 * 9),
    ];
    let last = [
        ("ada", "The pilot asked for the new berth numbers"),
        ("cy", "Cake at the boathouse after 🎂"),
        ("me", "Here is the plan"),
        ("bo", "North buoy is off its place"),
        ("dee", "See you on the early ferry"),
        ("bo", "Twelve boats so far"),
        ("cy", "Thanks, that did it!"),
        ("dee", "All quiet, lamp is fine"),
    ];
    let records: Vec<ChatRecord> = chats
        .iter()
        .zip(last)
        .map(
            |((id, kind, title, summary, unread, minutes), (sender, text))| ChatRecord {
                id: (*id).into(),
                kind: (*kind).into(),
                title: (*title).into(),
                member_summary: (*summary).into(),
                unread: *unread,
                last_message_at: Some(now - Duration::minutes(*minutes)),
                last_message_preview: Some(text.into()),
                last_message_sender_id: Some(sender.into()),
                last_message_sender_name: std::iter::once(("me", "Robin Vale"))
                    .chain(people)
                    .find(|(id, _)| *id == sender)
                    .map(|(_, name)| name.into()),
                members: std::iter::once(("me", "Robin Vale"))
                    .chain(people)
                    .filter(|(user, name)| *kind == "group" || *user == "me" || name == summary)
                    .map(|(user, name)| MemberRecord {
                        user_id: Some(user.into()),
                        display_name: name.into(),
                    })
                    .collect(),
                ..Default::default()
            },
        )
        .collect();
    let _ = store.upsert_chats(&records);
    let bodies = [
        "<p>Morning! Did the <b>tide table</b> make it into the folder?</p>",
        "<p>Yes, see <a href=\"https://example.com/harbour/plans/2026/tide-table-final-version\">https://example.com/harbour/plans/2026/tide-table-final-version</a></p>",
        "<p>Great. Two things left:</p><ul><li>paint the north buoy</li><li>call the pilot<ul><li>before noon</li></ul></li></ul>",
        "<blockquote>call the pilot</blockquote><p>Done, she is in at 14:00 👍🏽</p>",
        "<p>The script for the log:</p><pre><code>for reading in tide {\n    println!(\"{reading:>6.2}\");\n}</code></pre>",
        "<table><tr><td><b>Buoy</b></td><td><b>State</b></td></tr><tr><td>North</td><td>painted</td></tr><tr><td>South</td><td>waiting</td></tr></table>",
        "<p>مرحبا بالجميع، 日本語も大丈夫 ✨</p>",
        "<p>Can someone look at this before the meeting? It is a longer note, so it wraps over a few lines and shows how text flows when the window is wide, and how it behaves when the window is narrow.</p>",
        "<p>On it</p>",
    ];
    let mut messages = Vec::new();
    for (chat, ..) in &chats {
        // The second one ends like a chat people have.
        let older = if *chat == "c2" { 60 * 24 } else { 0 };
        for index in 0..60i64 {
            let sender = [
                ("ada", "Ada Quill"),
                ("me", "Robin Vale"),
                ("bo", "Bo Lindqvist"),
                ("cy", "Cyrus Amadi"),
            ][(index / 2 % 4) as usize];
            messages.push(MessageRecord {
                conversation_id: (*chat).into(),
                message_id: format!("{chat}-{index}"),
                sender_id: Some(sender.0.into()),
                sender_name: Some(sender.1.into()),
                created_at: now - Duration::minutes(older + (60 - index) * 37),
                edited_at: (index % 13 == 0).then_some(now),
                deleted: index % 29 == 7,
                body_html: bodies[(index % bodies.len() as i64) as usize].into(),
                reactions_json: if index % 7 == 3 {
                    r#"[{"reaction_type":"like","user_id":"bo","display_name":"Bo"},{"reaction_type":"heart","user_id":"me","display_name":"Robin"}]"#.into()
                } else {
                    "[]".into()
                },
                ..Default::default()
            });
        }
    }
    let talk = [
        (
            "ada",
            48,
            "<p>Morning all! The harbour opens on <b>Friday</b>. Here is what is still open:</p><ul><li>paint the north buoy</li><li>berth numbers for the pilot</li><li>someone to ring the bell 🔔</li></ul>",
            "[]",
        ),
        (
            "bo",
            41,
            "<p>Buoy is done, second coat this afternoon.</p>",
            r#"[{"reaction_type":"like","user_id":"ada","display_name":"Ada"},{"reaction_type":"like","user_id":"me","display_name":"Robin"},{"reaction_type":"heart","user_id":"cy","display_name":"Cyrus"}]"#,
        ),
        (
            "bo",
            40,
            "<p>I can take the numbers to the pilot on the way back, too.</p>",
            "[]",
        ),
        (
            "me",
            33,
            "<p>I will ring the bell. This is what the tide gauge says for the day:</p><pre><code>06:12  low    0.4 m\n12:31  high   3.1 m\n18:47  low    0.5 m</code></pre>",
            r#"[{"reaction_type":"like","user_id":"ada","display_name":"Ada"}]"#,
        ),
        (
            "ada",
            24,
            "<blockquote>12:31  high   3.1 m</blockquote><p>Then we start at noon, so the big ones can come in.</p>",
            "[]",
        ),
        ("cy", 10, "<p>Perfect.</p>", "[]"),
        (
            "cy",
            9,
            "<p>Cake at the boathouse after 🎂</p>",
            r#"[{"reaction_type":"heart","user_id":"ada","display_name":"Ada"},{"reaction_type":"heart","user_id":"me","display_name":"Robin"}]"#,
        ),
    ];
    for (index, (sender, minutes, html, reactions)) in talk.into_iter().enumerate() {
        messages.push(MessageRecord {
            conversation_id: "c2".into(),
            message_id: format!("talk-{index}"),
            sender_id: Some(sender.into()),
            sender_name: std::iter::once(("me", "Robin Vale"))
                .chain(people)
                .find(|(id, _)| *id == sender)
                .map(|(_, name)| name.into()),
            created_at: now - Duration::minutes(minutes),
            body_html: html.into(),
            reactions_json: reactions.into(),
            ..Default::default()
        });
    }
    let page = "https://example.com/harbour/berths";
    messages.push(MessageRecord {
        conversation_id: "c1".into(),
        message_id: "link".into(),
        sender_id: Some("ada".into()),
        sender_name: Some("Ada Quill".into()),
        created_at: now - Duration::minutes(8),
        body_html: format!("<p>The new numbers are here: <a href=\"{page}\">{page}</a></p>"),
        links_json: serde_json::json!([{
            "url": page,
            "previewenabled": true,
            "preview": {
                "title": "Berths of the new harbour",
                "description": "Which boat goes where, from Friday on. With depths, so that the big ones do not touch ground at low water.",
                "previewurl": "https://us-prod.asyncgw.teams.microsoft.com/urlp/v1/url/image?demo"
            }
        }])
        .to_string(),
        ..Default::default()
    });
    messages.push(MessageRecord {
        conversation_id: "c1".into(),
        message_id: "thanks".into(),
        sender_id: Some("me".into()),
        sender_name: Some("Robin Vale".into()),
        created_at: now - Duration::minutes(2),
        body_html: "<p>Thanks! Also on www.example.com now.</p>".into(),
        ..Default::default()
    });
    messages.push(MessageRecord {
        conversation_id: "c4".into(),
        message_id: "card".into(),
        sender_id: Some("bo".into()),
        sender_name: Some("Harbour watch".into()),
        created_at: now,
        attachments_json: serde_json::json!([{
            "content_type": "application/vnd.microsoft.card.adaptive",
            "id": "card",
            "content": CARD
        }])
        .to_string(),
        ..Default::default()
    });
    let _ = store.upsert_messages(&messages);
    let read: Vec<store::ReadHorizonRecord> = people
        .iter()
        .map(|(id, _)| store::ReadHorizonRecord {
            member_id: format!("8:orgid:{id}"),
            read_until: now,
            read_at: None,
        })
        .collect();
    let _ = store.set_read_horizons("c2", &read);
    let _ = store.set_read_horizons("c1", &read[..1]);
    channel(&store);
    let unsent = |id: &str, state, payload: &str, error: Option<&str>| store::OutboxRecord {
        id: id.into(),
        conversation_id: "c3".into(),
        target: store::OutboxTarget::Flat,
        thread_root_id: None,
        payload: payload.into(),
        images: vec![],
        state,
        last_error: error.map(str::to_owned),
        created_at: now,
    };
    let _ = store.put_outbox(&unsent(
        "1",
        store::OutboxState::Failed,
        r#"{"markdown":"Here is the plan","tried":true,"files":[{"name":"plan.pdf","path":"/x/plan.pdf"}]}"#,
        Some("HTTP 403"),
    ));
    let _ = store.put_outbox(&unsent(
        "2",
        store::OutboxState::Sending,
        r#"{"markdown":"and one on its way","tried":false}"#,
        None,
    ));
    store
}

fn channel(store: &Store) {
    let now = Utc::now();
    let team = store::TeamRecord {
        id: "t1".into(),
        name: "Harbour".into(),
    };
    let _ = store.upsert_teams(&[team]);
    let channel = store::ChannelRecord {
        id: "ch1".into(),
        team_id: "t1".into(),
        name: "General".into(),
        membership_type: None,
        last_message_at: Some(now - Duration::minutes(15)),
        unread: true,
    };
    let _ = store.upsert_channels(&[channel]);
    let post = |id: &str, root: Option<&str>, sender: (&str, &str), minutes: i64, html: &str| {
        MessageRecord {
            conversation_id: "ch1".into(),
            message_id: id.into(),
            reply_to_id: root.map(str::to_owned),
            sender_id: Some(sender.0.into()),
            sender_name: Some(sender.1.into()),
            created_at: now - Duration::minutes(minutes),
            body_html: html.into(),
            ..Default::default()
        }
    };
    let (ada, bo, me) = (
        ("ada", "Ada Quill"),
        ("bo", "Bo Lindqvist"),
        ("me", "Robin Vale"),
    );
    let _ = store.upsert_messages(&[
        post(
            "p1",
            None,
            ada,
            300,
            "<p>The new moorings are in. Photos to follow.</p>",
        ),
        post("p1r1", Some("p1"), bo, 200, "<p>They look solid!</p>"),
        post(
            "p1r2",
            Some("p1"),
            me,
            15,
            "<p>Thanks, <b>great work</b></p>",
        ),
        post(
            "p2",
            None,
            bo,
            120,
            "<p>Who has the key to the boathouse?</p>",
        ),
        post(
            "p3",
            None,
            me,
            60,
            "<p>Minutes from today are in the folder.</p>",
        ),
    ]);
}
