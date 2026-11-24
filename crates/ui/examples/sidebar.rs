//! Scrolls the chat list while chats keep moving to the top, as they do when messages arrive.
use gtk4 as gtk;
use gtk4::{glib, prelude::*};
use libadwaita as adw;
use libadwaita::prelude::AdwApplicationWindowExt;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use tern_ui::model::ChatRow;
use tern_ui::sidebar::Sidebar;
use tern_ui::window::load_style;

fn row(number: usize, when: &str) -> ChatRow {
    ChatRow {
        id: number.to_string(),
        title: format!("Person {number} Example"),
        preview: format!("Some words about number {number} that go on for a while"),
        when: when.into(),
        unread: number.is_multiple_of(5),
        ..Default::default()
    }
}

/// The title and place of the first row that is fully in view.
fn top(sidebar: &Sidebar) -> Option<(String, f64)> {
    let view = sidebar.root.child()?;
    let mut cell = view.first_child();
    let mut best: Option<(String, f64)> = None;
    while let Some(widget) = cell {
        cell = widget.next_sibling();
        let Some(bounds) = widget.compute_bounds(&sidebar.root) else {
            continue;
        };
        let y = f64::from(bounds.y());
        let Some(title) = title(&widget).filter(|_| widget.is_mapped()) else {
            continue;
        };
        if y >= 0.0 && best.as_ref().is_none_or(|(_, at)| y < *at) {
            best = Some((title, y));
        }
    }
    best
}

fn title(cell: &gtk::Widget) -> Option<String> {
    let text = cell
        .first_child()?
        .last_child()?
        .first_child()?
        .first_child()?;
    Some(text.downcast_ref::<gtk::Label>()?.text().into())
}

fn place(sidebar: &Sidebar, wanted: &str) -> Option<f64> {
    let mut cell = sidebar.root.child()?.first_child();
    while let Some(widget) = cell {
        if title(&widget).as_deref() == Some(wanted) {
            return widget
                .compute_bounds(&sidebar.root)
                .map(|bounds| f64::from(bounds.y()));
        }
        cell = widget.next_sibling();
    }
    None
}

async fn wait(milliseconds: u64) {
    glib::timeout_future(Duration::from_millis(milliseconds)).await;
}

fn main() {
    let app = adw::Application::builder()
        .application_id("io.github.rhgrieve.Tern.SidebarTest")
        .build();
    app.connect_activate(|app| {
        load_style();
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .default_width(340)
            .default_height(760)
            .build();
        window.present();
        let app = app.clone();
        glib::spawn_future_local(async move {
            wait(1500).await;
            for moving in [false, true, false, true] {
                let sidebar = Sidebar::new(|_| {});
                window.set_content(Some(&sidebar.root));
                let mut rows: Vec<ChatRow> = (0..225).map(|number| row(number, "12:34")).collect();
                sidebar.set(rows.clone());
                wait(1500).await;
                let gaps: Rc<RefCell<Vec<f64>>> = Rc::default();
                let (done, last, jumps) = (
                    Rc::new(Cell::new(false)),
                    Cell::new(0i64),
                    Rc::new(Cell::new(0)),
                );
                let before: RefCell<Option<(String, f64)>> = RefCell::default();
                let (g, d, j, tracked) =
                    (gaps.clone(), done.clone(), jumps.clone(), sidebar.clone());
                sidebar.root.add_tick_callback(move |_, clock| {
                    let now = clock.frame_time();
                    let earlier = last.replace(now);
                    if earlier != 0 {
                        g.borrow_mut().push((now - earlier) as f64 / 1000.0);
                    }
                    if g.borrow().len() >= 450 {
                        d.set(true);
                        return glib::ControlFlow::Break;
                    }
                    let expected = before.borrow().clone();
                    if let Some((name, y)) = expected
                        && let Some(at) = place(&tracked, &name)
                        && (at - (y - 12.0)).abs() > 3.0
                    {
                        j.set(j.get() + 1);
                    }
                    before.replace(top(&tracked));
                    let place = tracked.root.vadjustment();
                    place.set_value(place.value() + 12.0);
                    glib::ControlFlow::Continue
                });
                let mut turn = 0;
                while !done.get() {
                    wait(150).await;
                    if moving {
                        turn += 1;
                        let mut moved = rows.remove(60 + turn * 7 % 120);
                        moved.when = format!("12:{:02}", 35 + turn);
                        moved.unread = true;
                        rows.insert(0, moved);
                        sidebar.set(rows.clone());
                    }
                }
                let gaps = gaps.borrow();
                println!(
                    "chats {}: frames over 12 ms {:3} of 450, worst {:.0}, jumps {}",
                    if moving { "moving" } else { "still " },
                    gaps.iter().filter(|gap| **gap > 12.0).count(),
                    gaps.iter().copied().fold(0.0, f64::max),
                    jumps.get()
                );
            }
            app.quit();
        });
    });
    app.run_with_args::<&str>(&[]);
}
