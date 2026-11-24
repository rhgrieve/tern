//! Web addresses written as plain text: where they are, and links made of them.

use std::ops::Range;

const STARTS: [&str; 3] = ["https://", "http://", "www."];
/// What ends a sentence rather than an address.
const TRAILING: &str = ".,;:!?'\"*_~";

/// The addresses in plain text, and where each leads.
pub fn bare_links(text: &str) -> Vec<(Range<usize>, String)> {
    find(text, &[])
        .into_iter()
        .map(|range| {
            let url = target(&text[range.clone()]);
            (range, url)
        })
        .collect()
}

fn target(address: &str) -> String {
    let scheme = address
        .get(..4)
        .is_some_and(|start| start.eq_ignore_ascii_case("http"));
    if scheme {
        address.to_owned()
    } else {
        format!("https://{address}")
    }
}

/// Addresses end at white space, at what cannot be in one, and at any of `ends`.
fn find(text: &str, ends: &[&str]) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let before = text[..at].chars().next_back();
        let start = STARTS
            .iter()
            .find(|start| {
                rest.get(..start.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(start))
            })
            .filter(|_| !before.is_some_and(|letter| letter.is_alphanumeric() || letter == '/'));
        let length = start.and_then(|start| length(rest, start, ends));
        match length {
            Some(length) => {
                found.push(at..at + length);
                at += length;
            }
            None => at += rest.chars().next().map_or(1, char::len_utf8),
        }
    }
    found
}

fn length(rest: &str, start: &str, ends: &[&str]) -> Option<usize> {
    let mut end = rest
        .find(|letter: char| {
            letter.is_whitespace()
                || letter.is_control()
                || matches!(letter, '<' | '>' | '"' | '`' | '\u{200b}')
        })
        .unwrap_or(rest.len());
    for stop in ends {
        end = rest[..end].find(stop).unwrap_or(end);
    }
    while let Some(last) = rest[..end].chars().next_back() {
        if rest[..end].ends_with("&amp;") && !ends.is_empty() {
            end -= "&amp;".len();
            continue;
        }
        let closes =
            |open: char| rest[..end].matches(last).count() > rest[..end].matches(open).count();
        let trailing = match last {
            ')' => closes('('),
            ']' => closes('['),
            '}' => closes('{'),
            other => TRAILING.contains(other),
        };
        if !trailing {
            break;
        }
        end -= last.len_utf8();
    }
    let host = rest.get(start.len()..end)?.split(['/', '?', '#']).next()?;
    let real = host.starts_with(char::is_alphanumeric)
        && (!start.starts_with("www") || host.contains('.'));
    real.then_some(end)
}

/// HTML with its plain addresses made links, as other clients do while one types.
pub fn linked(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut within: Vec<String> = Vec::new();
    let mut rest = html;
    while !rest.is_empty() {
        let (text, after) = rest.split_at(rest.find('<').unwrap_or(rest.len()));
        if within.is_empty() {
            push_linked(&mut out, text);
        } else {
            out.push_str(text);
        }
        let (tag, after) = after.split_at(after.find('>').map_or(after.len(), |end| end + 1));
        out.push_str(tag);
        let name: String = tag
            .trim_start_matches(['<', '/'])
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        if matches!(name.as_str(), "a" | "code" | "pre" | "at") && !tag.ends_with("/>") {
            if tag.starts_with("</") {
                if within.last() == Some(&name) {
                    within.pop();
                }
            } else {
                within.push(name);
            }
        }
        rest = after;
    }
    out
}

/// What characters that cannot be in an address look like in HTML.
const ENTITIES: [&str; 6] = ["&lt;", "&gt;", "&quot;", "&#39;", "&#x27;", "&nbsp;"];

fn push_linked(out: &mut String, text: &str) {
    let mut at = 0;
    for range in find(text, &ENTITIES) {
        let address = &text[range.clone()];
        out.push_str(&text[at..range.start]);
        out.push_str(&format!("<a href=\"{}\">{address}</a>", target(address)));
        at = range.end;
    }
    out.push_str(&text[at..]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_found_without_what_is_around_them() {
        let of = |text: &str| -> Vec<String> {
            bare_links(text)
                .into_iter()
                .map(|(range, _)| text[range].to_owned())
                .collect()
        };
        assert_eq!(
            of("see https://a.example/b?c=1&d=2#e, or (http://b.example)."),
            ["https://a.example/b?c=1&d=2#e", "http://b.example"]
        );
        assert_eq!(
            of("https://en.example/wiki/Tern_(bird) and HTTPS://C.example!"),
            ["https://en.example/wiki/Tern_(bird)", "HTTPS://C.example"]
        );
        assert_eq!(of("www.a.example www.x www."), ["www.a.example"]);
        assert_eq!(
            of("[https://a.example] \"https://b.example\""),
            ["https://a.example", "https://b.example"]
        );
        assert_eq!(
            of("日本https://a.example/ü日 xhttps://b.example https:// http://-"),
            [""; 0]
        );
        assert_eq!(of("、https://a.example/ü"), ["https://a.example/ü"]);
        assert_eq!(bare_links("www.a.example")[0].1, "https://www.a.example");
    }

    #[test]
    fn links_are_made_outside_of_links_and_code() {
        assert_eq!(
            linked("<p>to https://a.example/?b=1&amp;c=2, ok</p>"),
            "<p>to <a href=\"https://a.example/?b=1&amp;c=2\">https://a.example/?b=1&amp;c=2</a>, ok</p>"
        );
        for same in [
            "<a href=\"https://a.example\">https://a.example</a>",
            "<code>https://a.example</code>",
            "<pre>x <b>https://a.example</b></pre>",
            "nothing here &lt;https&gt;",
            "<p>a < https",
        ] {
            assert_eq!(linked(same), same);
        }
        assert_eq!(
            linked("&quot;https://a.example&quot;&lt;https://b.example/&amp;&gt;"),
            "&quot;<a href=\"https://a.example\">https://a.example</a>&quot;&lt;<a href=\"https://b.example/\">https://b.example/</a>&amp;&gt;"
        );
        assert_eq!(
            linked("<br/>www.a.example"),
            "<br/><a href=\"https://www.a.example\">www.a.example</a>"
        );
        // Not twice.
        let once = linked("https://a.example");
        assert_eq!(linked(&once), once);
    }

    #[test]
    fn what_is_written_becomes_a_link_once() {
        let html = linked(&teams_core::markdown_to_html(
            "[a](https://a.example) https://b.example **https://c.example** `https://d.example`",
        ));
        assert_eq!(html.matches("<a ").count(), 3, "{html}");
        let html = linked(&teams_core::markdown_to_html(
            "a <script>https://x.example/\"onx=\" </script>",
        ));
        assert_eq!(html.matches("<a ").count(), 1, "{html}");
        assert!(
            !html.contains("<script") && html.contains("href=\"https://x.example/\""),
            "{html}"
        );
    }
}
