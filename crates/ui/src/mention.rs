//! Mentioning people: which word is being completed, who fits, and what ends up mentioned. No
//! widgets, so it can be tested.

use store::Store;

/// A person: id and name.
pub type Person = (String, String);

const MAX_QUERY: usize = 40;

/// The mention being typed at the end of `before`, the text before the cursor: where its `@`
/// is, in characters, and what follows it.
pub fn query(before: &str) -> Option<(usize, String)> {
    let letters: Vec<char> = before.chars().collect();
    let at = letters.iter().rposition(|letter| *letter == '@')?;
    let starts = at == 0 || letters[at - 1].is_whitespace() || "([{\"'".contains(letters[at - 1]);
    let typed = &letters[at + 1..];
    let fits = typed.len() <= MAX_QUERY
        && !typed.contains(&'\n')
        && typed.first().is_none_or(|first| !first.is_whitespace())
        && typed.iter().filter(|letter| letter.is_whitespace()).count() <= 2;
    (starts && fits).then(|| (at, typed.iter().collect()))
}

/// The people of `all` that fit, the best first: every word typed starts a word of the name.
pub fn matching(all: &[Person], query: &str, limit: usize) -> Vec<Person> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut found: Vec<(bool, &Person)> = all
        .iter()
        .filter_map(|person| {
            let name = person.1.to_lowercase();
            let parts: Vec<&str> = name
                .split(|letter: char| !letter.is_alphanumeric())
                .collect();
            let fits = words
                .iter()
                .all(|word| parts.iter().any(|part| part.starts_with(word.as_str())));
            let first = words
                .first()
                .is_none_or(|word| name.starts_with(word.as_str()));
            fits.then_some((!first, person))
        })
        .collect();
    found.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.1.to_lowercase().cmp(&b.1.1.to_lowercase()))
    });
    found
        .into_iter()
        .take(limit)
        .map(|(_, person)| person.clone())
        .collect()
}

/// The people of a conversation, without the user.
pub fn people(store: &Store, conversation: &str) -> Vec<Person> {
    let me = store.meta("me_user_id").ok().flatten();
    let mut all: Vec<Person> = match store.chat(conversation).ok().flatten() {
        Some(chat) => chat
            .members
            .into_iter()
            .filter_map(|member| Some((member.user_id?, member.display_name)))
            .collect(),
        // A channel has no list of members here: those who wrote.
        None => store
            .conversation_people(conversation, 400)
            .unwrap_or_default(),
    };
    all.retain(|(id, name)| {
        Some(id) != me.as_ref() && !name.trim().is_empty() && !store::is_bot_id(id)
    });
    all.sort_by(|a, b| a.0.cmp(&b.0));
    all.dedup_by(|a, b| a.0 == b.0);
    all
}

/// Which of the people chosen while writing are really in `text` as `@name`, in its order. Each
/// place counts once, for the longest name that fits there.
pub fn placed(text: &str, chosen: &[Person]) -> Vec<Person> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(next) = text[at..].find('@') {
        let start = at + next + 1;
        let rest = &text[start..];
        let best = chosen
            .iter()
            .filter(|(_, name)| !name.is_empty() && rest.starts_with(name.as_str()))
            .filter(|(_, name)| {
                // "@Ann" is not in "@Anna".
                rest[name.len()..]
                    .chars()
                    .next()
                    .is_none_or(|letter| !letter.is_alphanumeric())
            })
            .max_by_key(|(_, name)| name.len());
        at = start;
        if let Some(person) = best {
            found.push(person.clone());
            at += person.1.len();
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(id: &str, name: &str) -> Person {
        (id.into(), name.into())
    }

    #[test]
    fn a_mention_is_recognised_while_typing() {
        assert_eq!(query("hi @an"), Some((3, "an".into())));
        assert_eq!(query("@"), Some((0, String::new())));
        assert_eq!(query("(@bo"), Some((1, "bo".into())));
        assert_eq!(query("ü @Zoë Mül"), Some((2, "Zoë Mül".into())));
        for not in [
            "mail ann@example",
            "hello",
            "@ann\nnext",
            "@ ann",
            "@one two three four",
            "",
        ] {
            assert_eq!(query(not), None, "{not:?}");
        }
    }

    #[test]
    fn people_fit_by_the_start_of_their_names() {
        let all = [
            person("1", "Ann Example"),
            person("2", "Dan Anders"),
            person("3", "Bo-Anne Li"),
            person("4", "Zoë Müller"),
        ];
        let names = |typed: &str| -> Vec<String> {
            matching(&all, typed, 9)
                .into_iter()
                .map(|found| found.0)
                .collect()
        };
        assert_eq!(names("an"), ["1", "3", "2"]);
        assert_eq!(names("ann ex"), ["1"]);
        assert_eq!(names("mü"), ["4"]);
        assert_eq!(names("xyz"), [""; 0]);
        assert_eq!(names("").len(), 4);
        assert_eq!(matching(&all, "", 2).len(), 2);
    }

    #[test]
    fn only_what_is_in_the_text_is_mentioned_in_its_order() {
        let chosen = [
            person("b", "Bo"),
            person("a", "Ann"),
            person("al", "Ann Lee"),
            person("x", "Gone"),
        ];
        let ids = |text: &str| -> Vec<String> {
            placed(text, &chosen)
                .into_iter()
                .map(|found| found.0)
                .collect()
        };
        assert_eq!(ids("@Ann and @Bo"), ["a", "b"]);
        assert_eq!(ids("@Ann Lee, not @Anna"), ["al"]);
        assert_eq!(ids("@Bo @Bo"), ["b", "b"]);
        assert_eq!(ids("Bo Ann"), [""; 0]);
        assert_eq!(ids("@@Bo é@Ann"), ["b", "a"]);
    }

    #[test]
    fn people_come_from_the_conversation() {
        let store = crate::demo::store();
        let names: Vec<String> = people(&store, "c2")
            .into_iter()
            .map(|person| person.1)
            .collect();
        assert!(
            names.contains(&"Ada Quill".to_owned()) && !names.contains(&"Robin Vale".to_owned()),
            "{names:?}"
        );
        assert!(!people(&store, "ch1").is_empty());
    }
}
