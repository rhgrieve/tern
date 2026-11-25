//! Message content as blocks GTK can show. Pure, so every case is tested without a display.

use teams_core::{FontSize, Span};

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// Pango markup.
    Text(String),
    Code {
        language: Option<String>,
        text: String,
    },
    Quote(Vec<Block>),
    Heading(String),
    Item {
        depth: u32,
        mark: String,
        markup: String,
    },
    /// Rows of cells in markup; the first is a header.
    Table {
        header: bool,
        rows: Vec<Vec<String>>,
    },
    Picture {
        url: String,
        /// As wide and high as the message says it is.
        size: Option<(i32, i32)>,
    },
    Card(Box<crate::card::Card>),
    /// What is behind a link, as Teams saw it.
    Preview(Box<Preview>),
    Rule,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Preview {
    pub url: String,
    pub title: String,
    pub description: String,
    pub site: String,
    pub picture: Option<String>,
}

/// Where a long word may break; never part of the text itself.
pub const BREAK: char = '\u{200b}';
/// Words longer than this get places to break.
const LONG_WORD: usize = 24;
const PIECE: usize = 12;

/// Lets words that would not fit break. Letting the toolkit break anywhere instead makes it lay
/// out every message one letter wide, just to learn its least width.
fn breakable(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (word, after) = rest.split_at(end);
        if word.chars().count() > LONG_WORD {
            // Since the last place to break, or since letters with rules of their own.
            let (mut run, mut before) = (0, None);
            for letter in word.chars() {
                if run >= PIECE && before.is_some_and(is_plain) && is_plain(letter) {
                    out.push(BREAK);
                    run = 0;
                }
                out.push(letter);
                run += 1;
                before = Some(letter);
            }
        } else {
            out.push_str(word);
        }
        let space = after.chars().next().map_or(0, char::len_utf8);
        out.push_str(&after[..space]);
        rest = &after[space..];
    }
    out
}

/// Letters and signs that stand by themselves: nothing joins them, and nothing is put on them, as
/// in scripts that are written together and in emoji made of several.
fn is_plain(letter: char) -> bool {
    matches!(letter,
        '\u{21}'..='\u{7e}'
        | '\u{a1}'..='\u{2ff}'
        | '\u{370}'..='\u{482}'
        | '\u{48a}'..='\u{52f}'
        | '\u{2010}'..='\u{2027}'
        | '\u{2030}'..='\u{205e}'
        | '\u{2190}'..='\u{25ff}')
}

/// Text of a message inside markup. Other clients put spaces that do not break between mentions,
/// which makes one long word of them all.
pub fn shown(text: &str) -> String {
    escape(&breakable(&text.replace(BREAK, "").replace('\u{a0}', " ")))
}

/// What was selected, as it was written.
pub fn as_written(selected: &str) -> String {
    selected.replace(BREAK, "")
}

/// Text inside markup. Control characters would make Pango refuse the whole text.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for letter in text.chars() {
        match letter {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\n' | '\t' => out.push(letter),
            letter if letter.is_control() => {}
            letter => out.push(letter),
        }
    }
    out
}

/// What a click may open. Anything else stays text.
pub fn may_open(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    ["https://", "http://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
        && !url.chars().any(char::is_control)
}

struct Builder {
    blocks: Vec<Block>,
    line: String,
}

impl Builder {
    fn flush(&mut self) {
        let text = self.line.trim_matches('\n');
        if !text.trim().is_empty() {
            self.blocks.push(Block::Text(text.to_owned()));
        }
        self.line.clear();
    }

    fn block(&mut self, block: Block) {
        self.flush();
        self.blocks.push(block);
    }

    fn add(&mut self, spans: &[Span], depth: u32) {
        for span in spans {
            match span {
                Span::CodeBlock { language, code } => self.block(Block::Code {
                    language: language.clone(),
                    text: code.trim_end_matches('\n').to_owned(),
                }),
                Span::Heading { children, .. } => {
                    let markup = inline(children);
                    self.block(Block::Heading(markup));
                }
                Span::Rule => self.block(Block::Rule),
                Span::List {
                    ordered,
                    start,
                    items,
                } => {
                    self.flush();
                    for (index, item) in items.iter().enumerate() {
                        let mark = if *ordered {
                            format!("{}.", u64::from(*start) + index as u64)
                        } else {
                            "•".to_owned()
                        };
                        self.item(item, depth, mark);
                    }
                }
                Span::Table { header, rows } => {
                    let rows = rows
                        .iter()
                        .map(|row| row.iter().map(|cell| inline(cell)).collect())
                        .collect();
                    self.block(Block::Table {
                        header: *header,
                        rows,
                    });
                }
                Span::Image { hosted_content_url } => {
                    self.block(Block::Picture {
                        url: hosted_content_url.clone(),
                        size: None,
                    });
                }
                Span::Quote { children, .. } | Span::BlockQuote(children) => {
                    let inner = blocks(children);
                    if !inner.is_empty() {
                        self.block(Block::Quote(inner));
                    }
                }
                Span::Indent { children, .. } => self.add(children, depth),
                other => push_inline(&mut self.line, other),
            }
        }
    }

    /// The text of a list item, then whatever blocks it holds.
    fn item(&mut self, spans: &[Span], depth: u32, mark: String) {
        let is_block = |span: &Span| {
            matches!(
                span,
                Span::List { .. }
                    | Span::CodeBlock { .. }
                    | Span::Table { .. }
                    | Span::Image { .. }
                    | Span::Quote { .. }
                    | Span::BlockQuote(_)
            )
        };
        let split = spans.iter().position(is_block).unwrap_or(spans.len());
        self.blocks.push(Block::Item {
            depth,
            mark,
            markup: inline(&spans[..split]).trim().to_owned(),
        });
        self.add(&spans[split..], depth + 1);
        self.flush();
    }
}

pub fn blocks(spans: &[Span]) -> Vec<Block> {
    let mut builder = Builder {
        blocks: Vec::new(),
        line: String::new(),
    };
    builder.add(spans, 0);
    builder.flush();
    builder.blocks
}

pub fn inline(spans: &[Span]) -> String {
    let mut out = String::new();
    for span in spans {
        push_inline(&mut out, span);
    }
    out
}

fn wrap(out: &mut String, open: &str, children: &[Span], close: &str) {
    out.push_str(open);
    for child in children {
        push_inline(out, child);
    }
    out.push_str(close);
}

/// Text, with addresses in it made links: not every client sends them as such.
fn push_text(out: &mut String, text: &str) {
    let mut at = 0;
    for (range, url) in tern_engine::bare_links(text) {
        out.push_str(&shown(&text[at..range.start]));
        push_link(out, &url, &text[range.clone()]);
        at = range.end;
    }
    out.push_str(&shown(&text[at..]));
}

fn push_link(out: &mut String, url: &str, text: &str) {
    out.push_str("<a href=\"");
    out.push_str(&escape(url));
    out.push_str("\">");
    out.push_str(&shown(text));
    out.push_str("</a>");
}

/// Blocks inside inline content become their text.
fn push_inline(out: &mut String, span: &Span) {
    match span {
        Span::Text(text) => push_text(out, text),
        Span::Bold(children) => wrap(out, "<b>", children, "</b>"),
        Span::Italic(children) => wrap(out, "<i>", children, "</i>"),
        Span::Strike(children) => wrap(out, "<s>", children, "</s>"),
        Span::Underline(children) => wrap(out, "<u>", children, "</u>"),
        Span::Superscript(children) => wrap(out, "<sup>", children, "</sup>"),
        Span::Subscript(children) => wrap(out, "<sub>", children, "</sub>"),
        Span::Code(code) => {
            out.push_str("<tt>");
            out.push_str(&shown(code));
            out.push_str("</tt>");
        }
        Span::Link { url, children } if may_open(url) => {
            // Pango refuses a link inside a link.
            let mut text = String::new();
            for child in children {
                push_plain(&mut text, child);
            }
            push_link(out, url, if text.trim().is_empty() { url } else { &text });
        }
        Span::Link { children, .. } => wrap(out, "", children, ""),
        Span::Mention { name, .. } => {
            out.push_str("<b>@");
            out.push_str(&escape(name.trim_start_matches('@')));
            out.push_str("</b>");
        }
        Span::Sized(FontSize::Small, children) => wrap(out, "<small>", children, "</small>"),
        Span::Sized(FontSize::Large, children) => wrap(out, "<big>", children, "</big>"),
        Span::Sized(FontSize::Hidden, _) => {}
        Span::Sized(FontSize::Pixels(_), children) => wrap(out, "", children, ""),
        // Senders' colours are for their own theme; ours must stay readable.
        Span::Colored { children, .. }
        | Span::Indent { children, .. }
        | Span::BlockQuote(children) => {
            wrap(out, "", children, "");
        }
        Span::Heading { children, .. } => wrap(out, "<b>", children, "</b>"),
        Span::Quote { children, .. } => wrap(out, "", children, ""),
        Span::LineBreak | Span::Rule => out.push('\n'),
        Span::CustomEmoji { name, .. } => {
            out.push(':');
            out.push_str(&escape(name));
            out.push(':');
        }
        Span::CodeBlock { code, .. } => {
            out.push_str("<tt>");
            out.push_str(&escape(code));
            out.push_str("</tt>");
        }
        Span::Image { .. } => out.push('🖼'),
        Span::List { items, .. } => {
            for item in items {
                out.push_str("\n• ");
                for child in item {
                    push_inline(out, child);
                }
            }
        }
        Span::Table { rows, .. } => {
            for row in rows {
                out.push('\n');
                for cell in row {
                    for child in cell {
                        push_inline(out, child);
                    }
                    out.push_str("  ");
                }
            }
        }
    }
}

fn push_plain(out: &mut String, span: &Span) {
    match span {
        Span::Sized(FontSize::Hidden, _) => {}
        Span::Text(text) | Span::Code(text) => out.push_str(text),
        Span::Mention { name, .. } => out.push_str(name),
        Span::CustomEmoji { name, .. } => out.push_str(name),
        Span::LineBreak => out.push(' '),
        Span::Bold(children)
        | Span::Italic(children)
        | Span::Strike(children)
        | Span::Underline(children)
        | Span::Superscript(children)
        | Span::Subscript(children)
        | Span::Sized(_, children)
        | Span::BlockQuote(children)
        | Span::Colored { children, .. }
        | Span::Heading { children, .. }
        | Span::Indent { children, .. }
        | Span::Quote { children, .. }
        | Span::Link { children, .. } => children.iter().for_each(|child| push_plain(out, child)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use teams_core::html_to_spans;

    fn of(html: &str) -> Vec<Block> {
        blocks(&html_to_spans(html))
    }

    #[test]
    fn plain_and_formatted_text() {
        assert_eq!(
            of("<p>Hi <b>you</b> &amp; <i>me</i></p>"),
            [Block::Text("Hi <b>you</b> &amp; <i>me</i>".into())]
        );
    }

    #[test]
    fn text_cannot_become_markup() {
        assert_eq!(
            of("<p>&lt;span size='1'&gt;&amp;lt;</p>"),
            [Block::Text("&lt;span size=&#39;1&#39;&gt;&amp;lt;".into())]
        );
        assert_eq!(escape("a\u{0}\u{8}b"), "ab");
    }

    #[test]
    fn long_words_can_break_and_copy_as_written() {
        let url = "https://example.com/a/very/long/path/without/any/break";
        let word = "a/very/long/path/without/any/break/in/it/at/all";
        let Block::Text(markup) = &of(&format!("<p>see {word} and short words</p>"))[0] else {
            panic!()
        };
        assert!(markup.contains(BREAK) && markup.ends_with(" and short words"));
        assert_eq!(as_written(markup), format!("see {word} and short words"));
        // Addresses that came as text are links, too.
        let Block::Text(markup) = &of(&format!("<p>see {url}, and</p>"))[0] else {
            panic!()
        };
        assert!(markup.starts_with(&format!("see <a href=\"{url}\">")));
        assert!(markup.ends_with("</a>, and"));
        // The address of a link is never touched.
        let Block::Text(markup) = &of(&format!("<a href=\"{url}\">{url}</a>"))[0] else {
            panic!()
        };
        assert!(markup.starts_with(&format!("<a href=\"{url}\">")));
        // Joined emoji and other scripts keep their own rules.
        assert!(!breakable(&"👩‍👩‍👧".repeat(20)).contains(BREAK));
        assert!(!breakable(&"日本語".repeat(20)).contains(BREAK));
        assert!(!breakable(&"مرحبا".repeat(20)).contains(BREAK));
        assert!(!breakable(&"e\u{301}".repeat(40)).contains(BREAK));
        // One odd letter does not keep the others together.
        for long in [
            "─".repeat(93),
            format!("{url}…"),
            format!("é{url}"),
            format!("{url}👩‍👩‍👧{url}"),
            "Ünïcödé".repeat(9),
        ] {
            let broken = breakable(&long);
            let longest = broken.split(BREAK).map(|piece| piece.chars().count()).max();
            assert!(longest.is_some_and(|longest| longest <= 24), "{broken}");
            assert_eq!(as_written(&broken), long);
        }
        // Mentions in a row, as other clients write them.
        let [Block::Text(markup)] = &of("<p>@Ann&nbsp;@Bo&nbsp;@Cy</p>")[..] else {
            panic!()
        };
        assert_eq!(markup, "@Ann @Bo @Cy");
    }

    #[test]
    fn only_web_and_mail_links_can_be_clicked() {
        assert_eq!(
            of("<a href=\"https://x.example/?a=1&amp;b=%22\">go</a>"),
            [Block::Text(
                "<a href=\"https://x.example/?a=1&amp;b=%22\">go</a>".into()
            )]
        );
        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,x",
            "smb://h/x",
            "https://x\n.example",
        ] {
            assert!(!may_open(bad), "{bad}");
        }
        assert!(may_open("HTTPS://x.example"));
        let Block::Text(markup) = &of("<a href=\"file:///etc/passwd\">x</a>")[0] else {
            panic!()
        };
        assert!(!markup.contains("href"));
    }

    #[test]
    fn blocks_keep_their_order() {
        let blocks = of(
            "<p>before</p><pre><code>let x = 1;</code></pre><blockquote>said</blockquote><hr><p>after</p>",
        );
        assert!(matches!(&blocks[0], Block::Text(text) if text == "before"));
        assert!(
            matches!(&blocks[1], Block::Code { text, .. } if text == "let x = 1;"),
            "{blocks:?}"
        );
        assert!(matches!(&blocks[2], Block::Quote(inner) if inner.len() == 1));
        assert!(blocks.contains(&Block::Rule));
        assert!(matches!(blocks.last(), Some(Block::Text(text)) if text == "after"));
    }

    #[test]
    fn lists_nest_and_count() {
        let blocks = of("<ol start=\"3\"><li>a<ul><li>b</li></ul></li><li>c</li></ol>");
        let items: Vec<(u32, &str, &str)> = blocks
            .iter()
            .filter_map(|block| match block {
                Block::Item {
                    depth,
                    mark,
                    markup,
                } => Some((*depth, mark.as_str(), markup.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(items, [(0, "3.", "a"), (1, "•", "b"), (0, "4.", "c")]);
    }

    #[test]
    fn tables_and_empty_content() {
        let blocks = of("<table><tr><td>a</td><td><b>b</b></td></tr></table>");
        assert!(
            matches!(&blocks[0], Block::Table { rows, .. } if rows[0] == ["a", "<b>b</b>"]),
            "{blocks:?}"
        );
        assert!(of("<p>&nbsp;</p>").is_empty() || of("<p>&nbsp;</p>").len() == 1);
        assert!(of("").is_empty());
    }
}
