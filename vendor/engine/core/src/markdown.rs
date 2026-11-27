use std::sync::LazyLock;

use regex::Regex;

use crate::italic::italicize;
use crate::spans::Span;

static CODE_FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)```[^\n`]*\n(.*?)```").unwrap());
static INLINE_CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`\n]+)`").unwrap());
static BOLD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\*\*([^*\n]+)\*\*").unwrap());
static STRIKE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"~~([^~\n]+)~~").unwrap());
static LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]\n]+)\]\((https?://[^)\s]+)\)").unwrap());

static MENTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<at>([^<>\n]{1,100})</at>").unwrap());

const LIST_MARKER: &str = "- ";
const BULLETS: [&str; 2] = ["- ", "* "];
const MAX_HEADING: usize = 3;
const MAX_NUMBER_DIGITS: usize = 9;

/// What a line of a message is, by how it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkdownLine {
    Text,
    /// `- ` or `* `.
    Bullet,
    /// `1. `, with any number.
    Numbered,
    /// `# ` to `### `, with something after it.
    Heading(u8),
    /// `> `, or `>` alone.
    Quote,
}

/// What a line of a message is, and the byte where its text starts after the marker.
pub fn markdown_line(line: &str) -> (MarkdownLine, usize) {
    if BULLETS.iter().any(|bullet| line.starts_with(bullet)) {
        return (MarkdownLine::Bullet, 2);
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=MAX_NUMBER_DIGITS).contains(&digits) && line[digits..].starts_with(". ") {
        return (MarkdownLine::Numbered, digits + 2);
    }
    let hashes = line.bytes().take_while(|&byte| byte == b'#').count();
    if (1..=MAX_HEADING).contains(&hashes)
        && line[hashes..].starts_with(' ')
        && !line[hashes..].trim().is_empty()
    {
        return (MarkdownLine::Heading(hashes as u8), hashes + 1);
    }
    if line == ">" {
        return (MarkdownLine::Quote, 1);
    }
    if line.starts_with("> ") {
        return (MarkdownLine::Quote, 2);
    }
    (MarkdownLine::Text, 0)
}

fn line_of(line: &str, dialect: Dialect) -> (MarkdownLine, usize) {
    match dialect {
        Dialect::Message => markdown_line(line),
        Dialect::Card if line.starts_with(LIST_MARKER) => (MarkdownLine::Bullet, LIST_MARKER.len()),
        Dialect::Card => (MarkdownLine::Text, 0),
    }
}

pub fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#x27;"),
            other => escaped.push(other),
        }
    }
    escaped
}

pub fn plain_text_to_html(text: &str) -> String {
    escape_html(text).replace('\n', "<br>")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dialect {
    Message,
    Card,
}

/// Teams chat HTML for light markdown: bold, italic, strike, code, links, and lines that are
/// list items, headings or quotes.
pub fn markdown_to_html(text: &str) -> String {
    render(text, Dialect::Message)
}

/// Like `markdown_to_html`, plus `_italic_`, `*italic*` and `<at>Name</at>` mentions.
pub fn card_markdown_to_html(text: &str) -> String {
    render(text, Dialect::Card)
}

fn render(text: &str, dialect: Dialect) -> String {
    let mut html = String::new();
    let mut position = 0;
    for fence in CODE_FENCE.captures_iter(text) {
        let whole = fence.get(0).expect("group 0 always exists");
        html.push_str(&block_html(&text[position..whole.start()], dialect));
        html.push_str("<pre>");
        html.push_str(&escape_html(fence[1].trim_end_matches('\n')));
        html.push_str("</pre>");
        position = whole.end();
    }
    html.push_str(&block_html(&text[position..], dialect));
    html
}

fn block_html(segment: &str, dialect: Dialect) -> String {
    let lines: Vec<(MarkdownLine, &str)> = segment
        .split('\n')
        .map(|line| {
            let (kind, at) = line_of(line, dialect);
            (kind, &line[at..])
        })
        .collect();
    if lines.iter().all(|(kind, _)| *kind == MarkdownLine::Text) {
        return inline_html(segment, dialect);
    }
    // Runs of lines of a kind; blocks need no breaks between them.
    let mut html = String::new();
    let mut at = 0;
    while at < lines.len() {
        let kind = lines[at].0;
        let mut end = at + 1;
        if !matches!(kind, MarkdownLine::Heading(_)) {
            while end < lines.len() && lines[end].0 == kind {
                end += 1;
            }
        }
        let run: Vec<&str> = lines[at..end].iter().map(|(_, text)| *text).collect();
        match kind {
            MarkdownLine::Text => html.push_str(&inline_html(&run.join("\n"), dialect)),
            MarkdownLine::Bullet | MarkdownLine::Numbered => {
                let tag = if kind == MarkdownLine::Bullet {
                    "ul"
                } else {
                    "ol"
                };
                html.push_str(&format!("<{tag}>"));
                for item in run {
                    html.push_str("<li>");
                    html.push_str(&inline_html(item, dialect));
                    html.push_str("</li>");
                }
                html.push_str(&format!("</{tag}>"));
            }
            MarkdownLine::Heading(level) => {
                html.push_str(&format!("<h{level}>"));
                html.push_str(&inline_html(run[0], dialect));
                html.push_str(&format!("</h{level}>"));
            }
            MarkdownLine::Quote => {
                html.push_str("<blockquote>");
                html.push_str(&inline_html(&run.join("\n"), dialect));
                html.push_str("</blockquote>");
            }
        }
        at = end;
    }
    html
}

pub(crate) fn split_mentions(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut position = 0;
    for mention in MENTION.captures_iter(text) {
        let whole = mention.get(0).expect("group 0 always exists");
        if whole.start() > position {
            spans.push(Span::Text(text[position..whole.start()].to_owned()));
        }
        spans.push(Span::Mention {
            name: mention[1].trim().to_owned(),
            id: None,
        });
        position = whole.end();
    }
    if position < text.len() {
        spans.push(Span::Text(text[position..].to_owned()));
    }
    spans
}

fn inline_html(text: &str, dialect: Dialect) -> String {
    if dialect == Dialect::Message {
        return inline_markdown(text, dialect);
    }
    let mut html = String::new();
    let mut position = 0;
    for mention in MENTION.captures_iter(text) {
        let whole = mention.get(0).expect("group 0 always exists");
        html.push_str(&inline_markdown(&text[position..whole.start()], dialect));
        html.push_str("<at>");
        html.push_str(&escape_html(&mention[1]));
        html.push_str("</at>");
        position = whole.end();
    }
    html.push_str(&inline_markdown(&text[position..], dialect));
    html
}

fn inline_markdown(text: &str, dialect: Dialect) -> String {
    let escaped = plain_text_to_html(text);
    let coded = INLINE_CODE.replace_all(&escaped, "<code>${1}</code>");
    let bolded = BOLD.replace_all(&coded, "<b>${1}</b>");
    let styled = match dialect {
        Dialect::Message => italicize(&STRIKE.replace_all(&bolded, "<s>${1}</s>")),
        Dialect::Card => italicize(&bolded),
    };
    LINK.replace_all(&styled, r#"<a href="${2}">${1}</a>"#)
        .into_owned()
}
