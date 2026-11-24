//! Emoji by name: `:tada` while it is typed, and which emoji fit. No widgets, so it can be tested.

/// An emoji and the name it is offered under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub emoji: &'static str,
    pub name: &'static str,
}

/// With less typed, too much would fit, and `:D` is not the start of a name.
const LEAST: usize = 2;
const MOST: usize = 30;

fn is_name(letter: char) -> bool {
    letter.is_ascii_alphanumeric() || matches!(letter, '_' | '+' | '-')
}

fn starts(before: Option<&char>) -> bool {
    before.is_none_or(|letter| letter.is_whitespace() || "([{\"'".contains(*letter))
}

/// The name being typed at the end of `before`: where its `:` is, in characters, and the name.
pub fn query(before: &str) -> Option<(usize, String)> {
    let letters: Vec<char> = before.chars().collect();
    let at = letters.iter().rposition(|letter| !is_name(*letter))?;
    let typed = &letters[at + 1..];
    let fits = letters[at] == ':'
        && (LEAST..=MOST).contains(&typed.len())
        // Neither a time nor a score.
        && typed.iter().any(|letter| letter.is_ascii_alphabetic() || *letter == '+')
        && starts(at.checked_sub(1).map(|before| &letters[before]));
    fits.then(|| (at, typed.iter().collect()))
}

/// A whole `:name:` at the end of `before`: where it starts, in characters, and its emoji.
pub fn whole(before: &str) -> Option<(usize, &'static str)> {
    let (at, name) = query(before.strip_suffix(':')?)?;
    let emoji = emojis::get_by_shortcode(&name.to_ascii_lowercase())?;
    Some((at, emoji.as_str()))
}

/// The emoji that fit `typed`, the best first: the name itself, names that start with it, names
/// with a word that does, names it is in.
pub fn matching(typed: &str, limit: usize) -> Vec<Found> {
    let typed = typed.to_ascii_lowercase().replace('-', "_");
    let mut found: Vec<((u8, usize, usize), Found)> = Vec::new();
    for (order, emoji) in emojis::iter().enumerate() {
        let best = emoji
            .shortcodes()
            .filter_map(|name| {
                let words = || name.split('_').any(|word| word.starts_with(&typed));
                let rank = if name == typed {
                    0
                } else if name.starts_with(&typed) {
                    1
                } else if words() {
                    2
                } else if name.contains(&typed) {
                    3
                } else {
                    return None;
                };
                Some((rank, name))
            })
            .min_by_key(|(rank, name)| (*rank, name.len()));
        let best = best.or_else(|| {
            // Some have no short name that says it: 🎂 is `birthday`, and a cake.
            let word = emoji
                .name()
                .split(|letter: char| !letter.is_alphanumeric())
                .any(|word| word.to_lowercase().starts_with(&typed));
            let name = emoji.shortcodes().next().filter(|_| word)?;
            Some((4, name))
        });
        if let Some((rank, name)) = best {
            let found_one = Found {
                emoji: emoji.as_str(),
                name,
            };
            found.push(((rank, name.len(), order), found_one));
        }
    }
    found.sort_by_key(|(rank, _)| *rank);
    found
        .into_iter()
        .take(limit)
        .map(|(_, found)| found)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_start_after_a_space_and_two_letters() {
        let of = query;
        assert_eq!(of(":tada"), Some((0, "tada".into())));
        assert_eq!(of("so é :+1"), Some((5, "+1".into())));
        assert_eq!(of("(:thumbs_u"), Some((1, "thumbs_u".into())));
        for none in [
            "",
            ":",
            ":t",
            ":D",
            "12:30",
            "at 12:30",
            "3 :15",
            "a:tada",
            "https://x",
            "::tada",
            ":ta da",
            ":tada:",
            "x\u{200d}",
        ] {
            assert_eq!(of(none), None, "{none}");
        }
        assert_eq!(whole("yes :tada:"), Some((4, "🎉")));
        assert_eq!(whole(":TADA:"), Some((0, "🎉")));
        assert_eq!(whole(":nothinglikeit:"), None);
        assert_eq!(whole("10:30:"), None);
    }

    #[test]
    fn the_best_fit_comes_first() {
        let first = |typed: &str| matching(typed, 8).first().map(|found| found.emoji);
        assert_eq!(first("tada"), Some("🎉"));
        assert_eq!(first("+1"), Some("👍"));
        assert_eq!(first("joy"), Some("😂"));
        assert_eq!(first("heart"), Some("❤️"));
        assert_eq!(first("rocket"), Some("🚀"));
        assert!(
            matching("thumbs", 8)
                .iter()
                .any(|found| found.emoji == "👍")
        );
        assert!(matching("cake", 8).iter().any(|found| found.emoji == "🎂"));
        assert_eq!(matching("s", 8).len(), 8);
        assert!(matching("nothinglikeit", 8).is_empty());
    }
}
