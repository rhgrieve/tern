//! Measures the message list with 10,000 mixed messages: `cargo run --release -p tern --example scroll`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use tern_ui::list::MessageList;
use tern_ui::rich::Block;
use tern_ui::row::{Item, MessageRow};

fn item(number: i64) -> Item {
    let words = "the quick brown fox jumps over a lazy dog while reading messages from people in many places ";
    let text = |count: usize| words.repeat(count);
    // STRESS: every kind equally often. Otherwise like a chat, mostly short.
    const CHAT: [i64; 24] = [
        0, 1, 0, 10, 0, 1, 2, 0, 1, 0, 4, 1, 0, 6, 1, 0, 10, 0, 9, 1, 5, 0, 3, 7,
    ];
    let kind = if std::env::var_os("STRESS").is_some() {
        number.rem_euclid(11)
    } else {
        CHAT[(number.wrapping_mul(7919).rem_euclid(24)) as usize]
    };
    let blocks = match kind {
        0 => vec![Block::Text(format!("{number} ok"))],
        1 => vec![Block::Text(text(1))],
        2 => vec![Block::Text(format!(
            "{} <b>bold</b> <a href=\"https://example.com\">link</a>",
            text(2)
        ))],
        3 => vec![
            Block::Text("Look:".into()),
            Block::Code {
                language: None,
                text: "fn main() {\n    println!(\"hi\");\n}\n".repeat(4),
            },
        ],
        4 => vec![
            Block::Quote(vec![Block::Text(text(1))]),
            Block::Text("agreed".into()),
        ],
        5 => (0..5)
            .map(|index| Block::Item {
                depth: index % 2,
                mark: "•".into(),
                markup: text(1),
            })
            .collect(),
        6 => vec![Block::Picture("x".into())],
        7 => vec![Block::Text(text(12))],
        8 => vec![Block::Table {
            header: true,
            rows: vec![vec!["a".into(), "b".into(), "c".into()]; 6],
        }],
        9 => vec![Block::Text("مرحبا بالعالم 👋🏽 日本語テキスト".into())],
        _ => vec![Block::Text(text(3))],
    };
    Item {
        id: number.to_string(),
        sender: ["Ann", "Bob Example", "Chris"][number.rem_euclid(3) as usize].into(),
        time: "12:34".into(),
        blocks,
        joined: number.rem_euclid(4) == 1,
        reactions: if number.rem_euclid(9) == 0 {
            vec![tern_ui::row::Reaction {
                emoji: "👍".into(),
                count: 3,
                mine: false,
                ..Default::default()
            }]
        } else {
            vec![]
        },
        divider: (number.rem_euclid(50) == 0).then(|| "Monday".into()),
        ..Default::default()
    }
}

/// The topmost visible row: its name label's text is not unique, so the widget's place is tracked by item id.
fn top_row(list: &MessageList) -> Option<(String, f64)> {
    let mut child = list.view.first_child();
    let mut best: Option<(String, f64)> = None;
    while let Some(cell) = child {
        child = cell.next_sibling();
        let Some(row) = cell.first_child().and_downcast::<MessageRow>() else {
            continue;
        };
        if !cell.is_mapped() {
            continue;
        }
        let Some(bounds) = cell.compute_bounds(&list.root) else {
            continue;
        };
        let y = f64::from(bounds.y());
        if y >= 0.0 && best.as_ref().is_none_or(|(_, top)| y < *top) {
            best = Some((row.widget_name().to_string(), y));
        }
    }
    best
}

fn place(list: &MessageList, name: &str) -> Option<f64> {
    let mut child = list.view.first_child();
    while let Some(cell) = child {
        child = cell.next_sibling();
        let Some(row) = cell.first_child().and_downcast::<MessageRow>() else {
            continue;
        };
        if row.widget_name() == name && cell.is_mapped() {
            return cell
                .compute_bounds(&list.root)
                .map(|bounds| f64::from(bounds.y()));
        }
    }
    None
}

async fn wait(milliseconds: u64) {
    glib::timeout_future(std::time::Duration::from_millis(milliseconds)).await;
}

/// Scrolls by `step` every frame for `frames` frames; answers frame gaps in ms and jumps against expectation.
async fn scroll(list: &MessageList, step: f64, frames: usize, track: bool) -> (Vec<f64>, usize) {
    // No hand is scrolling here.
    list.leave_end();
    let gaps = Rc::new(RefCell::new(Vec::new()));
    let last = Rc::new(Cell::new(0i64));
    let jumps = Rc::new(Cell::new(0usize));
    let anchor: Rc<RefCell<Option<(String, f64)>>> = Rc::default();
    let (done, finished) = async_channel(1);
    let place_of = list.root.vadjustment();
    let tracked = list.clone();
    let (g, j) = (gaps.clone(), jumps.clone());
    list.root.add_tick_callback(move |_, clock| {
        let now = clock.frame_time();
        if last.get() != 0 {
            g.borrow_mut().push((now - last.get()) as f64 / 1000.0);
        }
        last.set(now);
        // Did last frame's row move by exactly the step?
        if let Some((name, y)) = anchor.borrow().as_ref()
            && let Some(at) = place(&tracked, name)
            && (at - (y - step)).abs() > 3.0
        {
            j.set(j.get() + 1);
        }
        if g.borrow().len() >= frames {
            let _ = done.try_send(());
            return glib::ControlFlow::Break;
        }
        if track {
            *anchor.borrow_mut() = top_row(&tracked);
        }
        place_of.set_value(place_of.value() + step);
        glib::ControlFlow::Continue
    });
    finished.recv().await;
    let collected = gaps.borrow().clone();
    (collected, jumps.get())
}

struct Channel(Rc<Cell<bool>>);
impl Channel {
    async fn recv(&self) {
        while !self.0.get() {
            wait(10).await;
        }
    }
    fn try_send(&self, _: ()) -> Result<(), ()> {
        self.0.set(true);
        Ok(())
    }
}
fn async_channel(_: usize) -> (Channel, Channel) {
    let flag = Rc::new(Cell::new(false));
    (Channel(flag.clone()), Channel(flag))
}

fn report(name: &str, mut gaps: Vec<f64>, jumps: usize) {
    gaps.sort_by(f64::total_cmp);
    let at = |share: f64| gaps[((gaps.len() - 1) as f64 * share) as usize];
    println!(
        "{name:28} frames {:4}  median {:5.1} ms  95% {:5.1}  99% {:5.1}  worst {:6.1}  jumps {jumps}",
        gaps.len(),
        at(0.5),
        at(0.95),
        at(0.99),
        at(1.0)
    );
}

fn main() {
    let app = adw::Application::builder()
        .application_id("io.github.rhgrieve.Tern.Scroll")
        .build();
    app.connect_activate(|app| {
        let list = MessageList::default();
        // Rows carry their item's id, so the test can follow them.
        let factory = list
            .view
            .factory()
            .and_downcast::<gtk::SignalListItemFactory>()
            .unwrap();
        factory.connect_bind(|_, cell| {
            let cell = cell.downcast_ref::<gtk::ListItem>().unwrap();
            let item = tern_ui::list::unboxed(&cell.item().unwrap()).unwrap();
            cell.child().unwrap().set_widget_name(&item.id);
        });
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .default_width(900)
            .default_height(800)
            .content(&list.root)
            .build();
        window.present();
        let app = app.clone();
        glib::spawn_future_local(async move {
            wait(500).await;
            let start = std::time::Instant::now();
            list.set((0..10_000).map(item).collect());
            let set = start.elapsed();
            wait(800).await;
            println!(
                "10,000 messages set in {} ms; at the end: {}; last row shown: {}",
                set.as_millis(),
                list.at_end(),
                place(&list, "9999").is_some()
            );

            // PLACE: only whether the list keeps its place, which needs no display.
            if std::env::var_os("PLACE").is_none() {
                let (gaps, jumps) = scroll(&list, -40.0, 300, true).await;
                report("up, steady, jumps tracked", gaps, jumps);
                // 25 px a frame at 120 Hz is a brisk 3,000 px a second.
                let (gaps, _) = scroll(&list, -25.0, 600, false).await;
                report("up, brisk", gaps, 0);
                let (gaps, jumps) = scroll(&list, -400.0, 200, true).await;
                report("up, fast", gaps, jumps);
                let (gaps, jumps) = scroll(&list, 25.0, 600, false).await;
                report("down, brisk", gaps, jumps);
            } else {
                list.jump(5_000);
                wait(800).await;
                println!(
                    "moved to the middle: follows the end {}, at the end {}",
                    list.follows(),
                    list.at_end()
                );
            }

            let before = top_row(&list);
            list.update((-50..10_000).map(item).collect());
            wait(400).await;
            let moved = before
                .as_ref()
                .and_then(|(name, y)| place(&list, name).map(|at| at - y));
            println!("50 older added in the middle: the top row moved by {moved:?} px");

            list.update((-50..10_003).map(item).collect());
            wait(400).await;
            let moved = before
                .as_ref()
                .and_then(|(name, y)| place(&list, name).map(|at| at - y));
            println!("3 newer added while reading: the top row moved by {moved:?} px");

            list.to_end();
            wait(600).await;
            println!("jump to the end: at the end {}", list.at_end());
            for number in 10_003..10_013 {
                list.update((-50..=number).map(item).collect());
                wait(120).await;
            }
            // Making 10,000 messages anew each time keeps this test busy longer than a chat would.
            wait(600).await;
            println!(
                "10 arrivals at the end: at the end {}, newest shown {}",
                list.at_end(),
                place(&list, "10012").is_some()
            );

            // To the very top, then older messages arrive there, as when reading back.
            list.jump(0);
            wait(800).await;
            println!("moved to the top: follows the end {}", list.follows());
            let before = top_row(&list);
            list.update((-100..10_013).map(item).collect());
            wait(600).await;
            let moved = before
                .as_ref()
                .and_then(|(name, y)| place(&list, name).map(|at| at - y));
            println!(
                "50 older added at the top: the top row moved by {moved:?} px (None: out of view)"
            );
            let memory = std::fs::read_to_string("/proc/self/status").unwrap();
            println!(
                "{}",
                memory
                    .lines()
                    .find(|line| line.starts_with("VmRSS"))
                    .unwrap()
            );
            app.quit();
        });
    });
    app.run_with_args::<&str>(&[]);
}
