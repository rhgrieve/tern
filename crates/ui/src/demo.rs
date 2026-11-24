//! Invented people and messages, for trying the window without an account.

use chrono::{Duration, Utc};
use store::{ChatRecord, MemberRecord, MessageRecord, Store};

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
    ];
    let records: Vec<ChatRecord> = chats
        .iter()
        .map(|(id, kind, title, summary, unread, minutes)| ChatRecord {
            id: (*id).into(),
            kind: (*kind).into(),
            title: (*title).into(),
            member_summary: (*summary).into(),
            unread: *unread,
            last_message_at: Some(now - Duration::minutes(*minutes)),
            last_message_preview: Some("The tide table is in the shared folder now".into()),
            last_message_sender_id: Some("ada".into()),
            last_message_sender_name: Some("Ada Quill".into()),
            members: std::iter::once(("me", "Robin Vale"))
                .chain(people)
                .map(|(user, name)| MemberRecord {
                    user_id: Some(user.into()),
                    display_name: name.into(),
                })
                .take(if *kind == "group" { 5 } else { 2 })
                .collect(),
            ..Default::default()
        })
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
                created_at: now - Duration::minutes((60 - index) * 37),
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
    let _ = store.upsert_messages(&messages);
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
