//! Adaptive Cards as what can be shown of them. Pure, so it is tested without a display. Apps and
//! strangers write cards: nothing in one is fetched or opened by itself.

use teams_core::{
    AdaptiveCard, CardAction, CardActionKind, CardElement, CardItem, CardText, ColumnWidth,
    ContainerStyle, Span, TextColor, TextSize,
};

use crate::rich;

/// A card may hold cards, and lists as long as it likes.
const MAX_DEPTH: usize = 8;
const MAX_PIECES: usize = 400;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Card {
    pub pieces: Vec<Piece>,
    /// What can be answered, which needs Teams: the titles of its buttons.
    pub answers: Vec<String>,
    /// The message in Teams.
    pub teams: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    #[default]
    Plain,
    Accent,
    Good,
    Warning,
    Bad,
}

impl Tone {
    /// The style class.
    pub fn class(self) -> Option<&'static str> {
        match self {
            Tone::Plain => None,
            Tone::Accent => Some("accent"),
            Tone::Good => Some("success"),
            Tone::Warning => Some("warning"),
            Tone::Bad => Some("error"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    Text {
        markup: String,
        tone: Tone,
        subtle: bool,
        lines: Option<i32>,
        /// 0 for the start, 1 for the end.
        align: f32,
    },
    Picture {
        url: String,
        about: String,
    },
    /// Names and values, both in markup.
    Facts(Vec<(String, String)>),
    Columns(Vec<(Width, Vec<Piece>)>),
    Box {
        tone: Tone,
        filled: bool,
        pieces: Vec<Piece>,
    },
    Table {
        header: bool,
        rows: Vec<Vec<Vec<Piece>>>,
    },
    Code(String),
    Badge {
        text: String,
        tone: Tone,
    },
    Progress(Option<f64>),
    Buttons(Vec<Button>),
    Rule,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Width {
    Own,
    /// Its share of what is left.
    Share(f32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Button {
    pub title: String,
    pub does: Does,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Does {
    Open(String),
    /// Shows more of the card.
    More(Vec<Piece>),
}

struct Reader {
    answers: Vec<String>,
    left: usize,
}

fn plain(spans: &[Span]) -> String {
    rich::inline(spans).trim().to_owned()
}

fn note(text: &str) -> Piece {
    Piece::Text {
        markup: format!("<small>{}</small>", rich::escape(text)),
        tone: Tone::Plain,
        subtle: true,
        lines: None,
        align: 0.0,
    }
}

fn line(text: &str) -> Piece {
    Piece::Text {
        markup: rich::escape(text),
        tone: Tone::Plain,
        subtle: false,
        lines: None,
        align: 0.0,
    }
}

fn tone(color: TextColor) -> Tone {
    match color {
        // Dark and light are for the sender's theme.
        TextColor::Default | TextColor::Dark | TextColor::Light => Tone::Plain,
        TextColor::Accent => Tone::Accent,
        TextColor::Good => Tone::Good,
        TextColor::Warning => Tone::Warning,
        TextColor::Attention => Tone::Bad,
    }
}

fn filled(style: ContainerStyle) -> (bool, Tone) {
    match style {
        ContainerStyle::Default => (false, Tone::Plain),
        ContainerStyle::Emphasis => (true, Tone::Plain),
        ContainerStyle::Accent => (true, Tone::Accent),
        ContainerStyle::Good => (true, Tone::Good),
        ContainerStyle::Warning => (true, Tone::Warning),
        ContainerStyle::Attention => (true, Tone::Bad),
    }
}

/// The host of a link, to say where it leads.
pub fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#', '\\']).next().unwrap_or(rest);
    host.rsplit('@').next().unwrap_or(host)
}

impl Reader {
    fn text(text: &CardText) -> Option<Piece> {
        let mut markup = plain(&text.spans);
        if markup.is_empty() {
            return None;
        }
        if text.bold {
            markup = format!("<b>{markup}</b>");
        }
        if text.monospace {
            markup = format!("<tt>{markup}</tt>");
        }
        markup = match text.size {
            TextSize::Small => format!("<small>{markup}</small>"),
            TextSize::Default => markup,
            TextSize::Medium => format!("<span size=\"110%\">{markup}</span>"),
            TextSize::Large => format!("<span size=\"125%\">{markup}</span>"),
            TextSize::ExtraLarge => format!("<span size=\"150%\">{markup}</span>"),
        };
        Some(Piece::Text {
            markup,
            tone: tone(text.color),
            subtle: text.subtle,
            lines: text
                .max_lines
                .and_then(|lines| i32::try_from(lines).ok())
                .filter(|lines| *lines > 0),
            align: match text.alignment {
                Some(teams_core::HorizontalAlignment::Center) => 0.5,
                Some(teams_core::HorizontalAlignment::Right) => 1.0,
                _ => 0.0,
            },
        })
    }

    fn items(&mut self, items: &[CardItem], depth: usize) -> Vec<Piece> {
        let mut pieces = Vec::new();
        if depth > MAX_DEPTH {
            return pieces;
        }
        for item in items.iter().filter(|item| item.visible) {
            if self.left == 0 {
                break;
            }
            self.left -= 1;
            let before = pieces.len();
            self.element(&item.element, depth, &mut pieces);
            if item.separator && before > 0 && pieces.len() > before {
                pieces.insert(before, Piece::Rule);
            }
        }
        pieces
    }

    fn element(&mut self, element: &CardElement, depth: usize, pieces: &mut Vec<Piece>) {
        match element {
            CardElement::Text(text) => pieces.extend(Self::text(text)),
            CardElement::Image(image) => {
                self.image(&image.url, image.alt_text.as_deref(), pieces);
                self.select(&image.select_action, depth, pieces);
            }
            CardElement::ImageSet(images) => {
                for image in images.iter().take(20) {
                    self.image(&image.url, image.alt_text.as_deref(), pieces);
                }
            }
            CardElement::Columns { columns, .. } => {
                let columns: Vec<(Width, Vec<Piece>)> = columns
                    .iter()
                    .map(|column| {
                        let width = match column.width {
                            ColumnWidth::Auto | ColumnWidth::Pixels(_) => Width::Own,
                            ColumnWidth::Stretch => Width::Share(1.0),
                            ColumnWidth::Weighted(weight) => Width::Share(weight.clamp(0.1, 100.0)),
                        };
                        let mut inner = self.items(&column.items, depth + 1);
                        self.select(&column.select_action, depth, &mut inner);
                        (width, inner)
                    })
                    .filter(|(_, pieces)| !pieces.is_empty())
                    .collect();
                if !columns.is_empty() {
                    pieces.push(Piece::Columns(columns));
                }
            }
            CardElement::Container {
                layout,
                items,
                select_action,
            } => {
                let mut inner = self.items(items, depth + 1);
                self.select(select_action, depth, &mut inner);
                let (filled, tone) = filled(layout.style);
                match (inner.is_empty(), filled) {
                    (true, _) => {}
                    (false, true) => pieces.push(Piece::Box {
                        tone,
                        filled,
                        pieces: inner,
                    }),
                    (false, false) => pieces.extend(inner),
                }
            }
            CardElement::Facts(facts) => {
                let facts: Vec<_> = facts
                    .iter()
                    .take(100)
                    .map(|fact| (rich::escape(&fact.title), plain(&fact.value)))
                    .collect();
                if !facts.is_empty() {
                    pieces.push(Piece::Facts(facts));
                }
            }
            CardElement::Media(media) => {
                if rich::may_open(&media.source_url) {
                    let title = media.alt_text.clone().unwrap_or_else(|| "Play".into());
                    pieces.push(Piece::Buttons(vec![Button {
                        title,
                        does: Does::Open(media.source_url.clone()),
                    }]));
                }
            }
            // Decoration.
            CardElement::Icon(_) => {}
            CardElement::Table(table) => {
                let rows: Vec<Vec<Vec<Piece>>> = table
                    .rows
                    .iter()
                    .take(100)
                    .map(|row| {
                        row.cells
                            .iter()
                            .take(20)
                            .map(|cell| self.items(&cell.items, depth + 1))
                            .collect()
                    })
                    .collect();
                if !rows.is_empty() {
                    pieces.push(Piece::Table {
                        header: table.first_row_as_header,
                        rows,
                    });
                }
            }
            CardElement::CodeBlock(code) => pieces.push(Piece::Code(code.code.clone())),
            CardElement::Badge(badge) if badge.text.trim().is_empty() => {}
            CardElement::Badge(badge) => {
                use teams_core::BadgeStyle;
                pieces.push(Piece::Badge {
                    text: badge.text.clone(),
                    tone: match badge.style {
                        BadgeStyle::Accent => Tone::Accent,
                        BadgeStyle::Good => Tone::Good,
                        BadgeStyle::Warning => Tone::Warning,
                        BadgeStyle::Attention => Tone::Bad,
                        _ => Tone::Plain,
                    },
                });
            }
            CardElement::ProgressBar(bar) => pieces.push(Piece::Progress(
                bar.value
                    .map(|value| f64::from(value.clamp(0.0, 100.0)) / 100.0),
            )),
            CardElement::ProgressRing(ring) => {
                pieces.extend(ring.label.as_deref().map(note));
            }
            CardElement::CompoundButton(button) => {
                pieces.push(Piece::Text {
                    markup: format!("<b>{}</b>", rich::escape(&button.title)),
                    tone: Tone::Plain,
                    subtle: false,
                    lines: None,
                    align: 0.0,
                });
                pieces.extend(button.description.as_deref().map(line));
                self.select(&button.select_action, depth, pieces);
            }
            CardElement::Carousel(carousel) => {
                for (at, page) in carousel.pages.iter().take(20).enumerate() {
                    let inner = self.items(&page.items, depth + 1);
                    if at > 0 && !inner.is_empty() {
                        pieces.push(Piece::Rule);
                    }
                    pieces.extend(inner);
                    self.select(&page.select_action, depth, pieces);
                }
            }
            CardElement::Actions(actions) => self.actions(actions, depth, pieces),
            CardElement::Input(input) => {
                if let Some(label) = input.label.as_deref().filter(|label| !label.is_empty()) {
                    pieces.push(note(label));
                    self.answer(label);
                } else {
                    self.answer("");
                }
            }
            CardElement::Rating(rating) => {
                let count = rating
                    .count
                    .map(|count| format!(" ({count})"))
                    .unwrap_or_default();
                pieces.push(line(&format!(
                    "★ {} of {}{count}",
                    rating.value, rating.max
                )));
            }
            CardElement::Chart(chart) => {
                let title = chart.title.as_deref().unwrap_or("untitled");
                pieces.push(note(&format!(
                    "Chart: {title}. Open it in Teams to see it."
                )));
                self.answer("");
            }
        }
    }

    /// Only what Teams stores is shown: fetching others would tell their owner about the reader.
    fn image(&mut self, url: &str, about: Option<&str>, pieces: &mut Vec<Piece>) {
        let about = about.unwrap_or("").trim();
        if tern_engine::may_prefetch(url) {
            pieces.push(Piece::Picture {
                url: url.to_owned(),
                about: about.to_owned(),
            });
        } else if !about.is_empty() {
            pieces.push(note(about));
        }
    }

    fn answer(&mut self, title: &str) {
        if !self.answers.iter().any(|known| known == title) {
            self.answers.push(title.to_owned());
        }
    }

    fn select(&mut self, action: &Option<CardAction>, depth: usize, pieces: &mut Vec<Piece>) {
        if let Some(action) = action {
            self.actions(std::slice::from_ref(action), depth, pieces);
        }
    }

    fn actions(&mut self, actions: &[CardAction], depth: usize, pieces: &mut Vec<Piece>) {
        let mut buttons = Vec::new();
        for action in actions.iter().filter(|action| action.visible).take(30) {
            let title = action.title.trim();
            match &action.kind {
                CardActionKind::OpenUrl(url) if rich::may_open(url) => buttons.push(Button {
                    title: if title.is_empty() { host(url) } else { title }.to_owned(),
                    does: Does::Open(url.clone()),
                }),
                CardActionKind::ShowCard(card) => {
                    let mut inner = self.items(&card.items, depth + 1);
                    if depth < MAX_DEPTH {
                        self.actions(&card.actions, depth + 1, &mut inner);
                    }
                    if !inner.is_empty() {
                        buttons.push(Button {
                            title: if title.is_empty() { "More" } else { title }.to_owned(),
                            does: Does::More(inner),
                        });
                    }
                }
                // What these do, only the app behind the card knows; Teams asks it.
                CardActionKind::Submit(_)
                | CardActionKind::Execute(_)
                | CardActionKind::ToggleVisibility(_) => self.answer(title),
                CardActionKind::OpenUrl(_)
                | CardActionKind::Unavailable(_)
                | CardActionKind::Unsupported => {}
            }
        }
        if !buttons.is_empty() {
            pieces.push(Piece::Buttons(buttons));
        }
    }
}

/// The card, or `None` where nothing of it can be shown.
pub fn read(card: &AdaptiveCard, teams: String) -> Option<Card> {
    let mut reader = Reader {
        answers: Vec::new(),
        left: MAX_PIECES,
    };
    let mut pieces = reader.items(&card.items, 0);
    reader.select(&card.select_action, 0, &mut pieces);
    reader.actions(&card.actions, 0, &mut pieces);
    let answers = reader.answers;
    (!pieces.is_empty() || !answers.is_empty()).then_some(Card {
        pieces,
        answers,
        teams,
    })
}

/// Where Teams shows a message.
pub fn link(conversation: &str, message: &str) -> String {
    let encode = |text: &str| {
        let mut out = String::new();
        for byte in text.bytes() {
            if byte.is_ascii_alphanumeric() {
                out.push(char::from(byte));
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
        out
    };
    let context = if conversation.ends_with("@thread.tacv2") {
        ""
    } else {
        "?context=%7B%22contextType%22%3A%22chat%22%7D"
    };
    format!(
        "https://teams.microsoft.com/l/message/{}/{}{context}",
        encode(conversation),
        encode(message)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn card(body: Value, actions: Value) -> Option<Card> {
        let value = json!({
            "type": "AdaptiveCard", "version": "1.5", "body": body, "actions": actions
        });
        read(&AdaptiveCard::parse(&value.to_string())?, String::new())
    }

    #[test]
    fn text_keeps_its_looks_and_cannot_smuggle_markup() {
        let card = card(
            json!([
                {"type": "TextBlock", "text": "Build <b>failed</b> & more", "weight": "Bolder", "size": "Large", "color": "Attention"},
                {"type": "TextBlock", "text": ""},
                {"type": "TextBlock", "text": "two", "maxLines": 2, "isSubtle": true, "horizontalAlignment": "Right", "separator": true}
            ]),
            json!([]),
        )
        .unwrap();
        let Piece::Text { markup, tone, .. } = &card.pieces[0] else {
            panic!("{card:?}")
        };
        assert!(
            markup.contains("&lt;b&gt;failed&lt;/b&gt; &amp; more"),
            "{markup}"
        );
        assert!(markup.starts_with("<span size=\"125%\"><b>"));
        assert_eq!(*tone, Tone::Bad);
        assert_eq!(card.pieces[1], Piece::Rule);
        assert!(matches!(
            card.pieces[2],
            Piece::Text { subtle: true, lines: Some(2), align, .. } if align == 1.0
        ));
    }

    #[test]
    fn pictures_from_elsewhere_are_not_shown_but_told() {
        let card = card(
            json!([
                {"type": "Image", "url": "https://tracker.example/x.png", "altText": "a chart"},
                {"type": "Image", "url": "https://tracker.example/pixel.gif"}
            ]),
            json!([]),
        )
        .unwrap();
        assert_eq!(card.pieces.len(), 1);
        assert!(
            matches!(&card.pieces[0], Piece::Text { markup, .. } if markup.contains("a chart"))
        );
    }

    #[test]
    fn links_open_and_answers_go_to_teams() {
        let card = card(
            json!([{"type": "TextBlock", "text": "Approve?"}]),
            json!([
                {"type": "Action.OpenUrl", "title": "Details", "url": "https://x.example/1"},
                {"type": "Action.OpenUrl", "title": "Bad", "url": "file:///etc/passwd"},
                {"type": "Action.Submit", "title": "Approve", "data": {}},
                {"type": "Action.Submit", "title": "Reject", "data": {}},
                {"type": "Action.ShowCard", "title": "Why", "card": {
                    "type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "because"}]
                }}
            ]),
        )
        .unwrap();
        assert_eq!(card.answers, ["Approve", "Reject"]);
        let Some(Piece::Buttons(buttons)) = card.pieces.last() else {
            panic!("{card:?}")
        };
        assert_eq!(buttons.len(), 2, "{buttons:?}");
        assert_eq!(buttons[0].does, Does::Open("https://x.example/1".into()));
        assert!(matches!(&buttons[1].does, Does::More(inner) if inner.len() == 1));
    }

    #[test]
    fn columns_facts_and_tables_nest() {
        let card = card(
            json!([
                {"type": "ColumnSet", "columns": [
                    {"type": "Column", "width": "auto", "items": [{"type": "TextBlock", "text": "a"}]},
                    {"type": "Column", "width": "stretch", "items": [{"type": "TextBlock", "text": "b"}]},
                    {"type": "Column", "width": 2, "items": []}
                ]},
                {"type": "FactSet", "facts": [{"title": "Who", "value": "Ann"}]},
                {"type": "Container", "style": "emphasis", "items": [{"type": "TextBlock", "text": "c"}]},
                {"type": "Container", "items": [{"type": "TextBlock", "text": "d"}]}
            ]),
            json!([]),
        )
        .unwrap();
        assert!(
            matches!(&card.pieces[0], Piece::Columns(columns) if columns.len() == 2
            && columns[0].0 == Width::Own && columns[1].0 == Width::Share(1.0))
        );
        assert_eq!(
            card.pieces[1],
            Piece::Facts(vec![("Who".into(), "Ann".into())])
        );
        assert!(matches!(&card.pieces[2], Piece::Box { filled: true, .. }));
        assert!(matches!(&card.pieces[3], Piece::Text { .. }));
    }

    #[test]
    fn a_deep_or_endless_card_ends() {
        let mut inner = json!({"type": "TextBlock", "text": "deep"});
        for _ in 0..40 {
            inner = json!({"type": "Container", "style": "emphasis", "items": [inner]});
        }
        let _ = card(json!([inner]), json!([]));
        let many: Vec<Value> = (0..3000)
            .map(|at| json!({"type": "TextBlock", "text": at.to_string()}))
            .collect();
        assert_eq!(
            card(json!(many), json!([])).unwrap().pieces.len(),
            MAX_PIECES
        );
    }

    #[test]
    fn links_say_where_they_lead() {
        assert_eq!(host("https://a.example/x?y"), "a.example");
        assert_eq!(host("https://bank.example@evil.example/x"), "evil.example");
        assert_eq!(host("https://evil.example\\@bank.example/"), "evil.example");
        assert!(!link("19:a@thread.v2/../x?y", "1").contains("/../"));
        assert!(!link("19:a@thread.tacv2", "1").contains("context"));
    }
}
