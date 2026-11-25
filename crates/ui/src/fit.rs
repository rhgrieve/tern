//! Holds a picture at its size, or smaller where there is less room, always in its shape. A list
//! gives its rows no more height than they need at least, and a picture by itself needs none.

use std::cell::{Cell, RefCell};

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;

/// It shrinks to this width.
const LEAST: i32 = 120;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Fit {
        pub child: RefCell<Option<gtk::Widget>>,
        /// Of the picture; none where the child is something else.
        pub size: Cell<Option<(i32, i32)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Fit {
        const NAME: &'static str = "TernFit";
        type Type = super::Fit;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Fit {
        fn dispose(&self) {
            if let Some(child) = self.child.take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Fit {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, other: i32) -> (i32, i32, i32, i32) {
            let child = self.child.borrow();
            let Some(child) = child.as_ref() else {
                return (0, 0, -1, -1);
            };
            let Some((width, height)) = self.size.get() else {
                let (least, natural, _, _) = child.measure(orientation, other);
                return (least, natural, -1, -1);
            };
            if orientation == gtk::Orientation::Horizontal {
                return (width.min(LEAST), width, -1, -1);
            }
            let at = |wide| super::high((width, height), wide);
            if other < 0 {
                // Whatever its width: at least as high as it is at its narrowest.
                return (at(width.min(LEAST)), height, -1, -1);
            }
            (at(other), at(other), -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _: i32) {
            if let Some(child) = self.child.borrow().as_ref() {
                let natural = match self.size.get() {
                    Some((wide, _)) => wide,
                    None => child.measure(gtk::Orientation::Horizontal, -1).1,
                };
                // At the start of the line, as its text is.
                child.allocate(width.min(natural), height, -1, None);
            }
        }
    }
}

glib::wrapper! {
    pub struct Fit(ObjectSubclass<imp::Fit>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// The height of a picture of `size` where there is room for `wide`.
fn high((width, height): (i32, i32), wide: i32) -> i32 {
    if wide < 0 || wide >= width {
        return height;
    }
    let shrunk = f64::from(height) * f64::from(wide) / f64::from(width.max(1));
    (shrunk.round() as i32).max(1)
}

impl Fit {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Fit {
        let fit: Fit = glib::Object::new();
        child.set_parent(&fit);
        fit.imp().child.replace(Some(child.clone().upcast()));
        fit
    }

    pub fn child(&self) -> Option<gtk::Widget> {
        self.imp().child.borrow().clone()
    }

    pub fn size(&self) -> Option<(i32, i32)> {
        self.imp().size.get()
    }

    pub fn set_size(&self, size: Option<(i32, i32)>) {
        if self.imp().size.replace(size) != size {
            self.queue_resize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_keeps_its_shape() {
        assert_eq!(high((400, 200), -1), 200);
        assert_eq!(high((400, 200), 900), 200);
        assert_eq!(high((400, 200), 300), 150);
        assert_eq!(high((400, 2), 100), 1);
    }
}
