use std::ops::Range;
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

/// Where what is in each cell of a table row is: between the `|`, without the space around it.
pub fn table_cells(line: &str) -> Vec<Range<usize>> {
    let mut inner = line.len() - line.trim_start().len()..line.trim_end().len();
    if line[inner.clone()].starts_with('|') {
        inner.start += 1;
    }
    if inner.end > inner.start && line[inner.clone()].ends_with('|') {
        inner.end -= 1;
    }
    let trimmed = |cell: Range<usize>| {
        let text = &line[cell.clone()];
        let start = cell.start + (text.len() - text.trim_start().len());
        start..start + text.trim().len()
    };
    let mut cells = Vec::new();
    let mut at = inner.start;
    for (pipe, _) in line[inner.clone()].match_indices('|') {
        cells.push(trimmed(at..inner.start + pipe));
        at = inner.start + pipe + 1;
    }
    cells.push(trimmed(at..inner.end));
    cells
}

/// How many columns a line under a table's first row makes, as `|---|:-:|`.
fn table_rule(line: &str) -> Option<usize> {
    if !line.contains('|') {
        return None;
    }
    let cells = table_cells(line);
    let rule = |cell: &str| {
        let dashes = cell.strip_prefix(':').unwrap_or(cell);
        let dashes = dashes.strip_suffix(':').unwrap_or(dashes);
        !dashes.is_empty() && dashes.bytes().all(|byte| byte == b'-')
    };
    cells
        .iter()
        .all(|cell| rule(&line[cell.clone()]))
        .then_some(cells.len())
}

/// How many of `lines` make a table from the first: a row with a `|` in it, a rule of as many
/// cells, and the rows with a `|` after them.
pub fn markdown_table(lines: &[&str]) -> Option<usize> {
    let head = lines.first()?;
    if !head.contains('|') || markdown_line(head).0 != MarkdownLine::Text {
        return None;
    }
    if table_rule(lines.get(1)?)? != table_cells(head).len() {
        return None;
    }
    let rows = lines[2..]
        .iter()
        .take_while(|line| line.contains('|') && markdown_line(line).0 == MarkdownLine::Text)
        .count();
    Some(2 + rows)
}

fn table_html(rows: &[&str], dialect: Dialect) -> String {
    let columns = table_cells(rows[0]).len();
    let cells = |row: &str, tag: &str| {
        let mut html = String::from("<tr>");
        let found = table_cells(row);
        // Missing cells are empty, and cells beyond the first row's are left out.
        for column in 0..columns {
            let text = found.get(column).map_or("", |cell| &row[cell.clone()]);
            html.push_str(&format!("<{tag}>{}</{tag}>", inline_html(text, dialect)));
        }
        html.push_str("</tr>");
        html
    };
    let mut html = format!("<table><thead>{}</thead>", cells(rows[0], "th"));
    if rows.len() > 2 {
        html.push_str("<tbody>");
        for row in &rows[2..] {
            html.push_str(&cells(row, "td"));
        }
        html.push_str("</tbody>");
    }
    html.push_str("</table>");
    html
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
    let raw: Vec<&str> = segment.split('\n').collect();
    let table = |at: usize| match dialect {
        Dialect::Message => markdown_table(&raw[at..]),
        Dialect::Card => None,
    };
    let lines: Vec<(MarkdownLine, &str)> = segment
        .split('\n')
        .map(|line| {
            let (kind, at) = line_of(line, dialect);
            (kind, &line[at..])
        })
        .collect();
    if lines.iter().all(|(kind, _)| *kind == MarkdownLine::Text)
        && (0..raw.len()).all(|at| table(at).is_none())
    {
        return inline_html(segment, dialect);
    }
    // Runs of lines of a kind; blocks need no breaks between them.
    let mut html = String::new();
    let mut at = 0;
    while at < lines.len() {
        if let Some(rows) = table(at) {
            html.push_str(&table_html(&raw[at..at + rows], dialect));
            at += rows;
            continue;
        }
        let kind = lines[at].0;
        let mut end = at + 1;
        if !matches!(kind, MarkdownLine::Heading(_)) {
            while end < lines.len() && lines[end].0 == kind && table(end).is_none() {
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
