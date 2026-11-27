use serde::Deserialize;
use teams_core::{card_markdown_to_html, markdown_to_html};

#[derive(Deserialize)]
struct Golden {
    input: String,
    expected: String,
}

#[test]
fn matches_the_teams_chat_html() {
    let goldens: Vec<Golden> =
        serde_json::from_str(include_str!("fixtures/markdown_golden.json")).unwrap();
    assert!(goldens.len() > 15);
    for golden in goldens {
        assert_eq!(
            markdown_to_html(&golden.input),
            golden.expected,
            "input: {:?}",
            golden.input
        );
    }
}

#[test]
fn list_lines_become_a_list() {
    assert_eq!(
        markdown_to_html("- one\n- **two**"),
        "<ul><li>one</li><li><b>two</b></li></ul>"
    );
}

#[test]
fn list_between_text_has_no_stray_breaks() {
    assert_eq!(
        markdown_to_html("intro\n- a\n- b\noutro"),
        "intro<ul><li>a</li><li>b</li></ul>outro"
    );
}

#[test]
fn separate_lists_stay_separate() {
    assert_eq!(
        markdown_to_html("- a\ntext\n- b"),
        "<ul><li>a</li></ul>text<ul><li>b</li></ul>"
    );
}

#[test]
fn list_marker_needs_a_space_and_line_start() {
    assert_eq!(markdown_to_html("-a\nx - y"), "-a<br>x - y");
}

#[test]
fn list_items_escape_html() {
    assert_eq!(markdown_to_html("- <b>"), "<ul><li>&lt;b&gt;</li></ul>");
}

#[test]
fn list_lines_inside_a_fence_stay_code() {
    assert_eq!(markdown_to_html("```\n- a\n```"), "<pre>- a</pre>");
}

#[test]
fn card_markdown_adds_italic_and_mentions_to_the_message_dialect() {
    assert_eq!(
        card_markdown_to_html("_soft_ and *slanted* and **bold**"),
        "<i>soft</i> and <i>slanted</i> and <b>bold</b>"
    );
    assert_eq!(
        card_markdown_to_html("hi <at>Ada & Co</at> <x>"),
        "hi <at>Ada &amp; Co</at> &lt;x&gt;"
    );
    assert_eq!(
        markdown_to_html("_soft_ <at>Ada</at>"),
        "<i>soft</i> &lt;at&gt;Ada&lt;/at&gt;"
    );
}

#[test]
fn card_markdown_keeps_snake_case_and_urls() {
    assert_eq!(
        card_markdown_to_html("my_var_name https://a.example/_x_/ [l](https://a.example/_y_)"),
        "my_var_name https://a.example/_x_/ <a href=\"https://a.example/_y_\">l</a>"
    );
}

#[test]
fn italic_and_strike_as_teams_writes_them() {
    assert_eq!(
        markdown_to_html("_soft_, *slanted* and ~~gone~~"),
        "<i>soft</i>, <i>slanted</i> and <s>gone</s>"
    );
    assert_eq!(
        markdown_to_html("***both*** and ~~**bold gone**~~"),
        "<i><b>both</b></i> and <s><b>bold gone</b></s>"
    );
    // Names, sums and code stay as they are.
    assert_eq!(
        markdown_to_html("@Sam_O_Brien 2*3*4 `*x*` ~~ a ~"),
        "@Sam_O_Brien 2*3*4 <code>*x*</code> ~~ a ~"
    );
}

#[test]
fn headings_quotes_and_numbers_are_blocks() {
    assert_eq!(
        markdown_to_html("# One\n## Two **b**\n### Three\n#### Four"),
        "<h1>One</h1><h2>Two <b>b</b></h2><h3>Three</h3>#### Four"
    );
    assert_eq!(
        markdown_to_html("said:\n> a\n>\n> *b*\nthen"),
        "said:<blockquote>a<br><br><i>b</i></blockquote>then"
    );
    assert_eq!(
        markdown_to_html("1. one\n2. two\n- dot\n* star"),
        "<ol><li>one</li><li>two</li></ol><ul><li>dot</li><li>star</li></ul>"
    );
}

#[test]
fn what_only_looks_like_a_block_stays_text() {
    assert_eq!(
        markdown_to_html("#tag\n# \n>no\n1.5 kg\na > b"),
        "#tag<br># <br>&gt;no<br>1.5 kg<br>a &gt; b"
    );
    assert_eq!(
        markdown_to_html("```\n# not\n> this\n```"),
        "<pre># not\n&gt; this</pre>"
    );
}

#[test]
fn cards_keep_their_own_lines() {
    assert_eq!(
        card_markdown_to_html("# no\n> no\n1. no\n- yes"),
        "# no<br>&gt; no<br>1. no<ul><li>yes</li></ul>"
    );
}

#[test]
fn pipe_tables_become_tables() {
    assert_eq!(
        markdown_to_html("|a|table|\n|-|-----|\n|it|**worked**|"),
        "<table><thead><tr><th>a</th><th>table</th></tr></thead>\
         <tbody><tr><td>it</td><td><b>worked</b></td></tr></tbody></table>"
    );
    // Without the outer pipes, with alignment, short and long rows, and text around it. A
    // line without a pipe ends it, so what follows a table is not taken into it.
    assert_eq!(
        markdown_to_html("before\nx | y\n:-- | --:\n1 |\n2 | 3 | 4\nafter"),
        "before<table><thead><tr><th>x</th><th>y</th></tr></thead>\
         <tbody><tr><td>1</td><td></td></tr><tr><td>2</td><td>3</td></tr></tbody></table>after"
    );
    assert_eq!(
        markdown_to_html("| h |\n|---|"),
        "<table><thead><tr><th>h</th></tr></thead></table>"
    );
}

#[test]
fn what_only_looks_like_a_table_stays_text() {
    // A rule of another width, no rule, or no pipe.
    assert_eq!(markdown_to_html("a|b\n|-|"), "a|b<br>|-|");
    assert_eq!(markdown_to_html("a | b\nc | d"), "a | b<br>c | d");
    assert_eq!(markdown_to_html("a\n---"), "a<br>---");
    assert_eq!(card_markdown_to_html("a|b\n-|-"), "a|b<br>-|-");
}
