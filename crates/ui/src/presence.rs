//! Who is there: what Teams says of other people, and the dot on their faces that shows it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::PI;

use gtk4 as gtk;
use gtk4::cairo;
use gtk4::prelude::*;
use tern_ipc::Availability;

thread_local! {
    /// By id, in small letters.
    static KNOWN: RefCell<HashMap<String, Availability>> = RefCell::default();
}

/// Takes in what is new; answers whether anything is.
pub fn update(people: &[(String, Availability)]) -> bool {
    KNOWN.with_borrow_mut(|known| {
        let mut changed = false;
        for (person, availability) in people {
            let person = person.to_ascii_lowercase();
            changed |= known.insert(person, *availability) != Some(*availability);
        }
        changed
    })
}

/// Old news is worse than none: without a connection nobody is said to be there.
pub fn forget() -> bool {
    KNOWN.with_borrow_mut(|known| !std::mem::take(known).is_empty())
}

pub fn of(person: &str) -> Option<Availability> {
    KNOWN.with_borrow(|known| known.get(&person.to_ascii_lowercase()).copied())
}

/// People whose presence the service knows; not bots, nor guests of other kinds.
pub fn is_person(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(at, byte)| match at {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

pub fn words(availability: Availability) -> &'static str {
    match availability {
        Availability::Available => "Available",
        Availability::Busy => "Busy",
        Availability::DoNotDisturb => "Do not disturb",
        Availability::Away => "Away",
        Availability::Offline => "Offline",
        Availability::OutOfOffice => "Out of office",
    }
}

const SIZE: i32 = 14;
const CLASS: &str = "presence";

/// A dot for the corner of a face.
pub fn dot() -> gtk::DrawingArea {
    let dot = gtk::DrawingArea::builder()
        .content_width(SIZE)
        .content_height(SIZE)
        .halign(gtk::Align::End)
        .valign(gtk::Align::End)
        .visible(false)
        .build();
    dot.add_css_class(CLASS);
    dot.set_draw_func(|dot, context, width, _| {
        if let Some(availability) = of(&dot.widget_name()) {
            draw(context, f64::from(width), availability);
        }
    });
    dot
}

/// The face with its dot.
pub fn with_dot(face: &impl IsA<gtk::Widget>, dot: &gtk::DrawingArea) -> gtk::Overlay {
    let overlay = gtk::Overlay::builder()
        .child(face)
        .valign(gtk::Align::Start)
        .build();
    overlay.add_overlay(dot);
    overlay
}

/// Whose dot it is.
pub fn set(dot: &gtk::DrawingArea, person: Option<&str>) {
    dot.set_widget_name(person.unwrap_or(""));
    show(dot);
}

fn show(dot: &gtk::DrawingArea) {
    let availability = of(&dot.widget_name());
    dot.set_visible(availability.is_some());
    dot.set_tooltip_text(availability.map(words));
    dot.queue_draw();
}

/// Draws all dots within again.
pub fn refresh(within: &gtk::Widget) {
    if let Some(dot) = within.downcast_ref::<gtk::DrawingArea>()
        && dot.has_css_class(CLASS)
    {
        return show(dot);
    }
    let mut child = within.first_child();
    while let Some(widget) = child {
        refresh(&widget);
        child = widget.next_sibling();
    }
}

fn draw(context: &cairo::Context, size: f64, availability: Availability) {
    let (red, green, blue) = match availability {
        Availability::Available => (0.20, 0.69, 0.37),
        Availability::Busy | Availability::DoNotDisturb => (0.85, 0.25, 0.28),
        Availability::Away => (0.95, 0.66, 0.11),
        Availability::Offline => (0.55, 0.55, 0.58),
        Availability::OutOfOffice => (0.69, 0.36, 0.78),
    };
    let middle = size / 2.0;
    // What is around it, from the style, sets it off from the face.
    let radius = middle - 2.0;
    context.set_source_rgb(red, green, blue);
    let hollow = matches!(
        availability,
        Availability::Offline | Availability::OutOfOffice
    );
    if hollow {
        context.set_line_width(1.5);
        context.arc(middle, middle, radius - 0.75, 0.0, 2.0 * PI);
        let _ = context.stroke();
    } else {
        context.arc(middle, middle, radius, 0.0, 2.0 * PI);
        let _ = context.fill();
        context.set_source_rgb(1.0, 1.0, 1.0);
    }
    context.set_line_width(1.4);
    context.set_line_cap(cairo::LineCap::Round);
    context.set_line_join(cairo::LineJoin::Round);
    let at = |x: f64, y: f64| (middle + x * radius, middle + y * radius);
    let line = |points: &[(f64, f64)]| {
        for (index, point) in points.iter().enumerate() {
            let (x, y) = at(point.0, point.1);
            if index == 0 {
                context.move_to(x, y);
            } else {
                context.line_to(x, y);
            }
        }
        let _ = context.stroke();
    };
    // Not by colour alone.
    match availability {
        Availability::Available => line(&[(-0.45, 0.05), (-0.12, 0.38), (0.45, -0.32)]),
        Availability::DoNotDisturb => line(&[(-0.45, 0.0), (0.45, 0.0)]),
        Availability::Away => line(&[(-0.05, -0.5), (-0.05, 0.1), (0.35, 0.35)]),
        Availability::Offline => {
            line(&[(-0.28, -0.28), (0.28, 0.28)]);
            line(&[(-0.28, 0.28), (0.28, -0.28)]);
        }
        Availability::OutOfOffice => line(&[(0.1, -0.35), (-0.25, 0.0), (0.1, 0.35)]),
        Availability::Busy => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_known_is_updated_and_forgotten() {
        let ann = ("ANN".to_owned(), Availability::Away);
        assert!(update(std::slice::from_ref(&ann)));
        assert!(!update(&[ann]), "nothing new");
        assert_eq!(of("ann"), Some(Availability::Away));
        assert!(update(&[("ann".into(), Availability::Busy)]));
        assert_eq!(of("Ann"), Some(Availability::Busy));
        assert!(forget() && !forget());
        assert_eq!(of("ann"), None);
    }

    #[test]
    fn only_people_are_asked_for() {
        assert!(is_person("00000000-0000-4000-8000-00000000000a"));
        for other in ["", "me", "28:bot", "00000000-0000-4000-8000-00000000000g"] {
            assert!(!is_person(other), "{other}");
        }
    }
}
