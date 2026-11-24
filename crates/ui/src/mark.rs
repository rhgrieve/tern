//! How far the newest own message got: one check on its way, two when it is there, blue when it was
//! read. Always takes its room, so that nothing moves when it changes.

use std::cell::Cell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::cairo;
use gtk4::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Sending,
    Delivered,
    /// Some have read it, of so many others.
    Seen {
        names: Vec<String>,
        others: usize,
    },
}

impl Progress {
    /// Everybody has read it.
    pub fn is_read(&self) -> bool {
        matches!(self, Progress::Seen { names, others } if names.len() >= *others)
    }

    /// In words, for who points at it or has it read out.
    pub fn words(&self) -> String {
        const NAMED: usize = 8;
        match self {
            Progress::Sending => "Sending".into(),
            Progress::Delivered => "Delivered".into(),
            Progress::Seen { others: 1, .. } => "Read".into(),
            Progress::Seen { names, others } => {
                let known: Vec<&str> = names
                    .iter()
                    .map(String::as_str)
                    .filter(|name| !name.is_empty())
                    .collect();
                let count = if names.len() >= *others {
                    "Read by everyone".to_owned()
                } else {
                    format!("Read by {} of {others}", names.len())
                };
                if known.is_empty() {
                    return count;
                }
                let more = known.len().saturating_sub(NAMED);
                let mut list = known[..known.len() - more].join(", ");
                if more > 0 {
                    list.push_str(&format!(" and {more} more"));
                }
                format!("{count}: {list}")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Mark {
    pub area: gtk::DrawingArea,
    checks: Rc<Cell<u8>>,
}

impl Default for Mark {
    fn default() -> Self {
        let checks = Rc::new(Cell::new(0));
        let area = gtk::DrawingArea::builder()
            .content_width(18)
            .content_height(HEIGHT)
            .valign(gtk::Align::End)
            .build();
        area.add_css_class("mark");
        area.set_draw_func({
            let checks = checks.clone();
            move |area, context, _, _| draw(area, context, checks.get())
        });
        Mark { area, checks }
    }
}

/// Of its height, what is below the checks.
const HEIGHT: i32 = 12;
const BELOW: i32 = 2;

impl Mark {
    /// Puts the checks this far above the bottom of the message.
    pub fn stand(&self, above: Option<i32>) {
        self.area
            .set_margin_bottom(above.map_or(0, |above| (above - BELOW).max(0)));
    }

    pub fn set(&self, progress: Option<&Progress>) {
        let checks = match progress {
            None => 0,
            Some(Progress::Sending) => 1,
            Some(_) => 2,
        };
        if progress.is_some_and(Progress::is_read) {
            self.area.add_css_class("read");
        } else {
            self.area.remove_css_class("read");
        }
        let words = progress.map(Progress::words);
        self.area.set_tooltip_text(words.as_deref());
        self.area.set_can_target(progress.is_some());
        if self.checks.replace(checks) != checks {
            self.area.queue_draw();
        }
    }
}

fn draw(area: &gtk::DrawingArea, context: &cairo::Context, checks: u8) {
    let color = area.color();
    context.set_source_rgba(
        color.red().into(),
        color.green().into(),
        color.blue().into(),
        color.alpha().into(),
    );
    context.set_line_width(1.6);
    context.set_line_cap(cairo::LineCap::Round);
    context.set_line_join(cairo::LineJoin::Round);
    // The first check stays where it is when the second one comes.
    for at in 0..checks {
        let x = f64::from(at) * 5.0;
        if at == 0 {
            context.move_to(x + 1.5, 6.2);
            context.line_to(x + 4.5, 9.2);
        } else {
            // Only what shows behind the first one.
            context.move_to(x + 3.6, 8.3);
            context.line_to(x + 4.5, 9.2);
        }
        context.line_to(x + 11.0, 2.2);
        let _ = context.stroke();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_say_who_read_it() {
        let seen = |names: &[&str], others| Progress::Seen {
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            others,
        };
        assert_eq!(seen(&["Ann"], 1).words(), "Read");
        assert!(seen(&["Ann"], 1).is_read() && !seen(&["Ann"], 2).is_read());
        assert_eq!(seen(&["Ann", ""], 3).words(), "Read by 2 of 3: Ann");
        assert_eq!(seen(&["Ann", "Bo"], 2).words(), "Read by everyone: Ann, Bo");
        let many: Vec<String> = (0..10).map(|at| format!("P{at}")).collect();
        let many = Progress::Seen {
            names: many,
            others: 12,
        }
        .words();
        assert!(many.ends_with("P7 and 2 more"), "{many}");
    }
}
