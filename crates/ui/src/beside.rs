//! Dialogs that a click next to them closes. The click is the dialog's: it fills the window, and
//! nothing behind it hears of one.

use gtk4 as gtk;
use gtk4::graphene;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

pub fn closes(dialog: &adw::Dialog) {
    let click = gtk::GestureClick::new();
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    click.connect_pressed(|click, _, x, y| {
        let Some(dialog) = click.widget().and_downcast::<adw::Dialog>() else {
            return;
        };
        // Not a menu in front of it, which has a surface of its own.
        let ours = click.current_event().and_then(|event| event.surface())
            == dialog.native().and_then(|native| native.surface());
        if ours && is_beside(&dialog, x, y) {
            click.set_state(gtk::EventSequenceState::Claimed);
            dialog.close();
        }
    });
    dialog.add_controller(click);
}

fn is_beside(dialog: &adw::Dialog, x: f64, y: f64) -> bool {
    let place = dialog
        .child()
        .and_then(|child| child.compute_bounds(dialog));
    let at = graphene::Point::new(x as f32, y as f32);
    place.is_some_and(|place| !place.contains_point(&at))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// As the composer's test: needs a display.
    #[test]
    #[ignore = "needs a display"]
    fn the_margins_are_beside_a_dialog() {
        adw::init().unwrap();
        let dialog = adw::Dialog::builder()
            .child(&gtk::Entry::new())
            .content_width(300)
            .content_height(200)
            .build();
        closes(&dialog);
        let behind = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let window = adw::Window::builder()
            .default_width(820)
            .default_height(620)
            .content(&behind)
            .build();
        window.present();
        dialog.present(Some(&behind));
        let end = Instant::now() + Duration::from_secs(10);
        while !is_beside(&dialog, 10.0, 300.0) && Instant::now() < end {
            window.queue_draw();
            gtk::glib::MainContext::default().iteration(false);
            std::thread::sleep(Duration::from_millis(2));
        }
        let (width, height) = (f64::from(dialog.width()), f64::from(dialog.height()));
        assert!(!is_beside(&dialog, width / 2.0, height / 2.0));
        assert!(is_beside(&dialog, 10.0, height / 2.0));
        assert!(is_beside(&dialog, width / 2.0, 10.0));
        assert!(is_beside(&dialog, width - 10.0, height / 2.0));
    }
}
