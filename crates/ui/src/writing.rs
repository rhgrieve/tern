//! The text view of the composer. It grows with its text, and GTK's forgets two things then: how
//! much of the text is in view now, and that there is nothing left to scroll to. The text would stay
//! shifted up, with an empty strip below.

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Writing;

    #[glib::object_subclass]
    impl ObjectSubclass for Writing {
        const NAME: &'static str = "TernWriting";
        type Type = super::Writing;
        type ParentType = gtk::TextView;
    }

    impl ObjectImpl for Writing {}

    impl WidgetImpl for Writing {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            let Some(place) = self.obj().vadjustment() else {
                return;
            };
            let visible = f64::from(height);
            if place.page_size() != visible {
                place.set_upper(place.upper().max(visible));
                place.set_page_size(visible);
            }
        }
    }

    impl TextViewImpl for Writing {}
}

glib::wrapper! {
    pub struct Writing(ObjectSubclass<imp::Writing>)
        @extends gtk::TextView, gtk::Widget,
        @implements gtk::Accessible, gtk::AccessibleText, gtk::Buildable, gtk::ConstraintTarget,
            gtk::Scrollable;
}

/// Goes back when there is nothing left to scroll to.
pub fn keep_within(place: &gtk::Adjustment) {
    let most = |place: &gtk::Adjustment| (place.upper() - place.page_size()).max(place.lower());
    if place.value() > most(place) {
        // Not while it is being laid out.
        let place = place.clone();
        glib::idle_add_local_once(move || {
            if place.value() > most(&place) {
                place.set_value(most(&place));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As the composer's test: needs a display.
    #[test]
    #[ignore = "needs a display"]
    fn the_text_is_not_left_past_its_end() {
        gtk::init().unwrap();
        let place = gtk::Adjustment::new(50.0, 0.0, 100.0, 1.0, 10.0, 40.0);
        place.connect_changed(keep_within);
        place.connect_value_changed(keep_within);
        // The box grew.
        place.set_page_size(100.0);
        while glib::MainContext::default().iteration(false) {}
        assert_eq!(place.value(), 0.0);

        let text: Writing = glib::Object::new();
        let place = gtk::Adjustment::new(0.0, 0.0, 300.0, 1.0, 10.0, 73.0);
        text.set_vadjustment(Some(&place));
        text.measure(gtk::Orientation::Vertical, 300);
        text.allocate(300, 220, -1, None);
        assert_eq!(text.vadjustment().unwrap().page_size(), 220.0);
    }
}
