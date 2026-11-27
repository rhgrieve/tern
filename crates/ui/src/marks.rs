//! The message box shows Markdown as it will be sent: bold, code, code blocks, links and list
//! items get their look, and the characters that only say so stay, dimmed. The text itself is
//! never changed. What counts is what sending makes of it: `teams_core::markdown_to_html` and
//! then `tern_engine::linked`, so `*this*` or `# this`, which go as they are, are not styled.

use std::cell::Cell;
use std::ops::Range;
use std::rc::{Rc, Weak};
use std::sync::LazyLock;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib, pango};
use libadwaita as adw;
use regex::{Captures, Regex};

// As `teams_core::markdown`, which keeps them to itself.
static CODE_FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)```[^\n`]*\n(.*?)```").unwrap());
static INLINE_CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`\n]+)`").unwrap());
static BOLD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\*\*([^*\n]+)\*\*").unwrap());
static LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]\n]+)\]\((https?://[^)\s]+)\)").unwrap());
const BULLET: &str = "- ";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Bold,
    Code,
    /// What is inside a fenced code block.
    Block,
    Link,
    /// The `- ` that makes a line a list item.
    Bullet,
    /// Characters that say how text looks and are not sent as text.
    Marker,
}

/// A look over characters (not bytes) of the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub kind: Kind,
    pub range: Range<usize>,
}

/// Where `text` gets which look once sent. Sending trims the text, so this does too.
pub fn marks(text: &str) -> Vec<Mark> {
    let lead = text.len() - text.trim_start().len();
    let body = text.trim();
    let mut found = Vec::new();
    let mut at = 0;
    for fence in CODE_FENCE.captures_iter(body) {
        let (whole, code) = (group(&fence, 0), group(&fence, 1));
        block(&mut found, body, at..whole.start);
        let end = code.start + body[code.clone()].trim_end_matches('\n').len();
        found.push((Kind::Marker, whole.start..code.start));
        found.push((Kind::Block, code.start..end));
        found.push((Kind::Marker, end..whole.end));
        at = whole.end;
    }
    block(&mut found, body, at..body.len());

    let mut chars = vec![0; text.len() + 1];
    let mut count = 0;
    for (byte, character) in text.char_indices() {
        chars[byte..byte + character.len_utf8()].fill(count);
        count += 1;
    }
    chars[text.len()] = count;
    found
        .into_iter()
        .filter(|(_, range)| !range.is_empty())
        .map(|(kind, range)| Mark {
            kind,
            range: chars[lead + range.start]..chars[lead + range.end],
        })
        .collect()
}

fn group(captures: &Captures, index: usize) -> Range<usize> {
    captures.get(index).expect("the pattern has it").range()
}

/// Outside code blocks: lines that start with `- ` are list items, the rest runs of text.
fn block(found: &mut Vec<(Kind, Range<usize>)>, body: &str, range: Range<usize>) {
    let segment = &body[range.clone()];
    if !segment.lines().any(|line| line.starts_with(BULLET)) {
        return inline(found, body, range);
    }
    let mut run: Option<Range<usize>> = None;
    let mut at = range.start;
    for line in segment.split('\n') {
        let here = at..at + line.len();
        at = here.end + 1;
        if line.starts_with(BULLET) {
            if let Some(run) = run.take() {
                inline(found, body, run);
            }
            let item = here.start + BULLET.len();
            found.push((Kind::Bullet, here.start..item));
            inline(found, body, item..here.end);
        } else {
            run = Some(run.map_or(here.clone(), |run| run.start..here.end));
        }
    }
    if let Some(run) = run {
        inline(found, body, run);
    }
}

/// As the converter does it: on escaped text, code first, then bold, then links, then the
/// addresses written plainly.
fn inline(all: &mut Vec<(Kind, Range<usize>)>, body: &str, range: Range<usize>) {
    let mut found = Vec::new();
    let found = &mut found;
    let mut shown = Shown::escaped(&body[range.clone()], range.start);
    shown = shown.replace(
        found,
        &INLINE_CODE,
        Kind::Code,
        |_| "<code>".into(),
        "</code>",
    );
    shown = shown.replace(found, &BOLD, Kind::Bold, |_| "<b>".into(), "</b>");
    shown = shown.replace(
        found,
        &LINK,
        Kind::Link,
        |link| format!("<a href=\"{}\">", &link[2]),
        "</a>",
    );
    shown.bare_links(found);
    // Code is only code when shown: what is bold or a link inside it is not.
    let code: Vec<Range<usize>> = found
        .iter()
        .filter(|(kind, _)| *kind == Kind::Code)
        .map(|(_, range)| range.clone())
        .collect();
    all.extend(found.drain(..).filter(|(kind, range)| {
        !matches!(kind, Kind::Bold | Kind::Link)
            || !code
                .iter()
                .any(|code| code.start <= range.start && range.end <= code.end)
    }));
}

/// HTML on its way to being sent, each byte knowing which bytes of the text it stands for.
struct Shown {
    html: String,
    from: Vec<Range<usize>>,
}

impl Shown {
    fn escaped(text: &str, offset: usize) -> Shown {
        let mut shown = Shown {
            html: String::with_capacity(text.len()),
            from: Vec::with_capacity(text.len()),
        };
        for (at, character) in text.char_indices() {
            let source = offset + at..offset + at + character.len_utf8();
            let mut buffer = [0; 4];
            let escaped = match character {
                '&' => "&amp;",
                '<' => "&lt;",
                '>' => "&gt;",
                '"' => "&quot;",
                '\'' => "&#x27;",
                '\n' => "<br>",
                other => other.encode_utf8(&mut buffer),
            };
            shown.push(escaped, source);
        }
        shown
    }

    fn push(&mut self, html: &str, source: Range<usize>) {
        self.html.push_str(html);
        self.from.extend(std::iter::repeat_n(source, html.len()));
    }

    fn copy(&self, into: &mut Shown, range: Range<usize>) {
        into.html.push_str(&self.html[range.clone()]);
        into.from.extend_from_slice(&self.from[range]);
    }

    /// The bytes of the text that these bytes of HTML stand for.
    fn source(&self, range: Range<usize>) -> Range<usize> {
        self.from[range.start].start..self.from[range.end - 1].end
    }

    /// Each match is the `kind` over its first group; the rest of it only marks.
    fn replace(
        self,
        found: &mut Vec<(Kind, Range<usize>)>,
        pattern: &Regex,
        kind: Kind,
        open: impl Fn(&Captures) -> String,
        close: &str,
    ) -> Shown {
        let mut out = Shown {
            html: String::with_capacity(self.html.len()),
            from: Vec::with_capacity(self.from.len()),
        };
        let mut at = 0;
        for captures in pattern.captures_iter(&self.html) {
            let (whole, inner) = (group(&captures, 0), group(&captures, 1));
            self.copy(&mut out, at..whole.start);
            let opening = self.source(whole.start..inner.start);
            let closing = self.source(inner.end..whole.end);
            found.push((Kind::Marker, opening.clone()));
            found.push((kind, self.source(inner.clone())));
            found.push((Kind::Marker, closing.clone()));
            out.push(&open(&captures), opening);
            self.copy(&mut out, inner);
            out.push(close, closing);
            at = whole.end;
        }
        self.copy(&mut out, at..self.html.len());
        out
    }

    /// Where `linked` makes links of addresses: it only adds tags, so what it adds is found by
    /// walking both.
    fn bare_links(&self, found: &mut Vec<(Kind, Range<usize>)>) {
        let linked = tern_engine::linked(&self.html);
        let (before, after) = (self.html.as_bytes(), linked.as_bytes());
        let (mut at, mut to) = (0, 0);
        let mut link: Option<usize> = None;
        while to < after.len() {
            match link {
                Some(start) if after[to..].starts_with(b"</a>") => {
                    found.push((Kind::Link, self.source(start..at)));
                    link = None;
                    to += "</a>".len();
                }
                // An address never starts with a tag, so a tag where the HTML has none is new.
                None if after[to] == b'<' && before.get(at) != Some(&b'<') => {
                    let end = after[to..].iter().position(|&byte| byte == b'>');
                    to += end.map_or(after.len() - to, |end| end + 1);
                    link = Some(at);
                }
                _ => {
                    at += 1;
                    to += 1;
                }
            }
        }
    }
}

/// The looks, as tags of the text view's own, which the spell checker's do not touch.
pub struct Styled {
    text: glib::WeakRef<gtk::TextView>,
    tags: Vec<(Kind, gtk::TextTag)>,
    waits: Cell<bool>,
}

/// How strong the colour of the text is in markers, and behind code.
const DIM: f32 = 0.45;
const SHADE: f32 = 0.08;

pub fn attach(text: &gtk::TextView) -> Rc<Styled> {
    let looks = [
        (Kind::Bold, gtk::TextTag::builder().weight(700).build()),
        (
            Kind::Code,
            gtk::TextTag::builder().family("monospace").build(),
        ),
        (
            Kind::Block,
            gtk::TextTag::builder().family("monospace").build(),
        ),
        (
            Kind::Link,
            gtk::TextTag::builder()
                .underline(pango::Underline::Single)
                .build(),
        ),
        (Kind::Bullet, gtk::TextTag::builder().weight(700).build()),
        (Kind::Marker, gtk::TextTag::new(None)),
    ];
    let table = text.buffer().tag_table();
    for (_, tag) in &looks {
        table.add(tag);
    }
    let styled = Rc::new(Styled {
        text: text.downgrade(),
        tags: looks.into(),
        waits: Cell::new(false),
    });
    styled.colour();
    // Colours follow the theme: read again once the view has its style, and when it changes.
    text.connect_map({
        let styled = Rc::downgrade(&styled);
        move |_| {
            if let Some(styled) = styled.upgrade() {
                styled.colour();
            }
        }
    });
    adw::StyleManager::default().connect_dark_notify({
        let styled = Rc::downgrade(&styled);
        move |_| {
            let styled = styled.clone();
            glib::idle_add_local_once(move || {
                if let Some(styled) = Weak::upgrade(&styled) {
                    styled.colour();
                }
            });
        }
    });
    styled
}

impl Styled {
    /// Restyles once the main loop is free, however often it is asked meanwhile.
    pub fn soon(self: &Rc<Self>) {
        if self.waits.replace(true) {
            return;
        }
        let styled = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(styled) = styled.upgrade() {
                styled.now();
            }
        });
    }

    pub fn now(&self) {
        self.waits.set(false);
        let Some(text) = self.text.upgrade() else {
            return;
        };
        let buffer = text.buffer();
        let (start, end) = buffer.bounds();
        for (_, tag) in &self.tags {
            buffer.remove_tag(tag, &start, &end);
        }
        let written = buffer.text(&start, &end, false);
        for mark in marks(&written) {
            if let Some((_, tag)) = self.tags.iter().find(|(kind, _)| *kind == mark.kind) {
                buffer.apply_tag(
                    tag,
                    &buffer.iter_at_offset(mark.range.start as i32),
                    &buffer.iter_at_offset(mark.range.end as i32),
                );
            }
        }
    }

    /// The looks at a character.
    #[cfg(test)]
    pub fn kinds(&self, at: i32) -> Vec<Kind> {
        let Some(text) = self.text.upgrade() else {
            return Vec::new();
        };
        let here = text.buffer().iter_at_offset(at);
        self.tags
            .iter()
            .filter(|(_, tag)| here.has_tag(tag))
            .map(|(kind, _)| *kind)
            .collect()
    }

    fn colour(&self) {
        let Some(text) = self.text.upgrade() else {
            return;
        };
        let ink = text.color();
        let with = |alpha: f32| gdk::RGBA::new(ink.red(), ink.green(), ink.blue(), alpha);
        for (kind, tag) in &self.tags {
            match kind {
                Kind::Marker | Kind::Bullet => tag.set_foreground_rgba(Some(&with(DIM))),
                Kind::Code => tag.set_background_rgba(Some(&with(SHADE))),
                Kind::Block => tag.set_paragraph_background_rgba(Some(&with(SHADE))),
                Kind::Bold | Kind::Link => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use teams_core::{Span, html_to_spans, markdown_to_html};

    /// Each character of `text` with the looks over it, as `char:looks`, markers as `~`.
    fn shown(text: &str) -> Vec<String> {
        let marks = marks(text);
        text.chars()
            .enumerate()
            .map(|(at, character)| {
                let mut looks: Vec<char> = marks
                    .iter()
                    .filter(|mark| mark.range.contains(&at))
                    .map(|mark| match mark.kind {
                        Kind::Bold => 'b',
                        Kind::Code => 'c',
                        Kind::Block => 'p',
                        Kind::Link => 'l',
                        Kind::Bullet => '-',
                        Kind::Marker => '~',
                    })
                    .collect();
                looks.sort_unstable();
                looks.dedup();
                format!("{character}:{}", looks.into_iter().collect::<String>())
            })
            .collect()
    }

    /// The looks as a line under the text: one letter per character, `.` for none.
    fn under(text: &str) -> String {
        shown(text)
            .into_iter()
            .map(|one| {
                let first = one.chars().next().map_or(0, char::len_utf8);
                let looks = &one[first + 1..];
                match looks {
                    "" => '.',
                    looks if looks.contains('~') => '~',
                    looks => looks.chars().next().unwrap_or('.'),
                }
            })
            .collect()
    }

    #[test]
    fn bold_code_and_links_are_found_with_their_markers() {
        assert_eq!(under("a **b** c"), "..~~b~~..");
        assert_eq!(under("`x*y*z` q"), "~ccccc~..");
        assert_eq!(
            under("[go](https://a.io) on"),
            format!("~ll{}...", "~".repeat(15))
        );
        assert_eq!(under("see https://a.io."), "....llllllllllll.");
    }

    #[test]
    fn nested_looks_are_both() {
        let looks = shown("**a `b` c**");
        assert_eq!(looks[5], "b:bc");
        assert_eq!(looks[4], "`:b~");
        assert_eq!(shown("**[x](https://a.io)**")[3], "x:bl");
        // What would be bold inside code is code, as it reads once sent.
        assert_eq!(shown("`**x**`")[3], "x:c");
        assert_eq!(shown("`**x**`")[1], "*:c~");
        assert_eq!(shown("**https://a.io**")[2], "h:bl");
    }

    #[test]
    fn unclosed_and_unsent_markers_stay_plain() {
        assert_eq!(under("**open"), "......");
        assert_eq!(under("`open"), ".....");
        assert_eq!(under("a ** b"), "......");
        assert_eq!(under("[x](not a link)"), "...............");
        // Teams messages from here have no italic, strike, headings or quotes.
        for plain in ["*it*", "_it_", "~~gone~~", "# Title", "> quoted", "* item"] {
            assert!(marks(plain).is_empty(), "{plain}");
        }
    }

    #[test]
    fn code_keeps_what_would_be_markers() {
        assert_eq!(under("`a*b` and *c*"), "~ccc~........");
        assert_eq!(under("`[x](https://a.io)`").chars().nth(2), Some('c'));
        assert_eq!(under("```\n**a**\n```"), "~~~~ccccc~~~~".replace('c', "p"));
    }

    #[test]
    fn fences_lists_and_lines() {
        let text = "```rs\nlet a = 1;\n```\n- one **two**\n- three\nafter";
        let line = under(text);
        assert_eq!(&line[..6], "~~~~~~");
        assert_eq!(&line[6..16], "pppppppppp");
        assert_eq!(&line[16..21], "~~~~.");
        assert_eq!(&line[21..23], "--");
        assert_eq!(&line[27..34], "~~bbb~~");
        assert_eq!(&line[35..37], "--");
        assert!(line.ends_with(".....") && !line[37..].contains('~'));
        // Not at the start of a line, or without its space, it is not a list.
        assert_eq!(under("a - b\n-c"), "........");
        // Bold reaches over a line break, as sending does.
        assert_eq!(
            under("**a\nb**"),
            "~~a.b~~".replace('a', "b").replace(".b", "bb")
        );
    }

    #[test]
    fn places_are_characters_and_trimmed_as_sent() {
        assert_eq!(under("🎉 **é** ü"), "..~~b~~..");
        assert_eq!(under("  \n**a**  "), "...~~b~~..");
        // Leading spaces go, so this is a list item when sent.
        assert_eq!(under("  - a"), "..--.");
        // Names with underscores are only names.
        assert!(marks("@Sam_O_Brien and @Jo_Ann_").is_empty());
        assert_eq!(under("@Ann_B **hi** @Cy_D"), ".......~~bb~~......");
    }

    /// What sending makes, character by character: looks of each character that is not space.
    fn sent(text: &str) -> Option<Vec<(char, [bool; 4])>> {
        let html = tern_engine::linked(&markdown_to_html(text.trim()));
        // Misnested HTML (bold over the end of code) is read anyhow by whoever gets it.
        if !well_formed(&html) {
            return None;
        }
        let mut out = Vec::new();
        walk(&html_to_spans(&html), [false; 4], &mut out);
        Some(out)
    }

    fn well_formed(html: &str) -> bool {
        let mut open: Vec<&str> = Vec::new();
        let mut rest = html;
        while let Some(start) = rest.find('<') {
            let Some(end) = rest[start..].find('>') else {
                return false;
            };
            let tag = &rest[start + 1..start + end];
            if tag.contains('<') {
                return false;
            }
            let name = tag.trim_start_matches('/').split(' ').next().unwrap_or("");
            if tag.starts_with('/') {
                if open.pop() != Some(name) {
                    return false;
                }
            } else if name != "br" {
                open.push(name);
            }
            rest = &rest[start + end + 1..];
        }
        open.is_empty()
    }

    // Looks: bold, code (either kind), link, list item.
    fn walk(spans: &[Span], looks: [bool; 4], out: &mut Vec<(char, [bool; 4])>) {
        fn text(out: &mut Vec<(char, [bool; 4])>, text: &str, looks: [bool; 4]) {
            out.extend(
                text.chars()
                    .filter(|character| !character.is_whitespace())
                    .map(|character| (character, looks)),
            );
        }
        for span in spans {
            let [bold, code, link, item] = looks;
            match span {
                Span::Text(said) => text(out, said, looks),
                Span::Code(said) | Span::CodeBlock { code: said, .. } => {
                    text(out, said, [bold, true, link, item]);
                }
                Span::Bold(children) => walk(children, [true, code, link, item], out),
                Span::Link { children, .. } => walk(children, [bold, code, true, item], out),
                Span::List { items, .. } => {
                    for one in items {
                        walk(one, [bold, code, link, true], out);
                    }
                }
                Span::LineBreak => {}
                other => panic!("not from Markdown: {other:?}"),
            }
        }
    }

    /// The same from the marks: markers are not sent, and inside code only code shows.
    fn styled(text: &str) -> Vec<(char, [bool; 4])> {
        let marks = marks(text);
        let has = |at: usize, kind: Kind| {
            marks
                .iter()
                .any(|mark| mark.kind == kind && mark.range.contains(&at))
        };
        let mut item = false;
        let mut out = Vec::new();
        for (at, character) in text.chars().enumerate() {
            if character == '\n' && !has(at, Kind::Block) {
                item = false;
            }
            if has(at, Kind::Bullet) {
                item = true;
                continue;
            }
            if character.is_whitespace() || has(at, Kind::Marker) {
                continue;
            }
            let code = has(at, Kind::Code) || has(at, Kind::Block);
            let bold = has(at, Kind::Bold);
            let link = has(at, Kind::Link);
            out.push((character, [bold, code, link, item && !has(at, Kind::Block)]));
        }
        out
    }

    #[test]
    fn marks_agree_with_what_is_sent() {
        const PIECES: [&str; 24] = [
            "a",
            "b",
            " ",
            "\n",
            "*",
            "**",
            "`",
            "```",
            "```rs\n",
            "[",
            "]",
            "(",
            ")",
            "](https://x.io)",
            "https://e.io",
            "www.w.io",
            "- ",
            "_",
            "@Sam_O",
            "é",
            "🎉",
            "~~",
            "&",
            "<",
        ];
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let (mut checked, mut skipped) = (0, 0);
        for _ in 0..4000 {
            let length = 1 + next() % 14;
            let text: String = (0..length)
                .map(|_| PIECES[(next() % PIECES.len() as u64) as usize])
                .collect();
            let Some(sent) = sent(&text) else {
                skipped += 1;
                continue;
            };
            assert_eq!(styled(&text), sent, "{text:?}");
            checked += 1;
        }
        assert!(
            checked > skipped * 4,
            "{checked} checked, {skipped} skipped"
        );
    }
}
