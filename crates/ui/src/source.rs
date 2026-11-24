//! A message back as the Markdown it could have been written in, for editing. Only when nothing
//! would be lost: a message with more than Tern can write is not offered for editing.

use teams_core::{Span, html_to_spans, markdown_to_html};

use crate::rich;

pub fn markdown(spans: &[Span]) -> Option<String> {
    let mut out = String::new();
    write(&mut out, spans)?;
    let out = out.trim().to_owned();
    // The proof is that it reads the same when written again.
    let again = rich::blocks(&html_to_spans(&markdown_to_html(&out)));
    (!out.is_empty() && again == rich::blocks(spans)).then_some(out)
}

fn write(out: &mut String, spans: &[Span]) -> Option<()> {
    for span in spans {
        match span {
            Span::Text(text) => out.push_str(text),
            Span::LineBreak => out.push('\n'),
            Span::Bold(children) => around(out, "**", children)?,
            Span::Code(code) => {
                out.push('`');
                out.push_str(code);
                out.push('`');
            }
            Span::CodeBlock { code, .. } => {
                line(out);
                out.push_str("```\n");
                out.push_str(code.trim_end_matches('\n'));
                out.push_str("\n```\n");
            }
            Span::Link { url, children } => {
                let mut text = String::new();
                write(&mut text, children)?;
                if text == *url {
                    out.push_str(url);
                } else {
                    out.push_str(&format!("[{text}]({url})"));
                }
            }
            Span::List {
                ordered: false,
                items,
                ..
            } => {
                line(out);
                for item in items {
                    out.push_str("- ");
                    write(out, item)?;
                    line(out);
                }
            }
            _ => return None,
        }
    }
    Some(())
}

fn line(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn around(out: &mut String, mark: &str, children: &[Span]) -> Option<()> {
    out.push_str(mark);
    write(out, children)?;
    out.push_str(mark);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(html: &str) -> Option<String> {
        markdown(&html_to_spans(html))
    }

    #[test]
    fn what_tern_can_write_comes_back() {
        for text in [
            "hello",
            "two\nlines",
            "some **bold** and `code`",
            "see [here](https://example.com/x)",
            "- one\n- two",
            "```\nlet x = 1;\n```",
        ] {
            assert_eq!(of(&markdown_to_html(text)).as_deref(), Some(text));
        }
    }

    #[test]
    fn what_would_be_lost_is_not_editable() {
        for html in [
            "<p><i>slanted</i></p>",
            "<table><tr><td>a</td></tr></table>",
            "<p>hi <at id=\"0\">Ann</at></p>",
            "<blockquote>quoted</blockquote><p>x</p>",
            "<ol><li>a</li></ol>",
            "<p>1 ** 2 ** 3</p>",
            "<p>a `b</p><p>c` d</p>",
            "",
        ] {
            let source = of(html);
            if let Some(source) = &source {
                // If it is offered, writing it again must give the same message.
                assert_eq!(
                    rich::blocks(&html_to_spans(&markdown_to_html(source))),
                    rich::blocks(&html_to_spans(html)),
                    "{html}"
                );
            }
            if html.contains("<i>")
                || html.contains("table")
                || html.contains("<ol")
                || html.contains("quote")
                || html.is_empty()
            {
                assert_eq!(source, None, "{html}");
            }
        }
    }
}
