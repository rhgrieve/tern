//! What opening conversations costs: reading them, and building, filling and measuring their rows.
//! `cargo run --release -p tern --example opening`, needs a display. Prints times only.
use std::rc::Rc;
use std::time::Instant;

use gtk4::prelude::*;
use tern_ui::row::MessageRow;

const WIDTH: i32 = 940;

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1e3
}

fn main() {
    gtk4::init().expect("a display");
    libadwaita::init().expect("a display");
    tern_ui::window::load_style();
    let account = tern_engine::accounts().into_iter().next().expect("a cache");
    let store = tern_engine::open_read_only(&account).unwrap();
    let now = tern_ui::model::now();
    let spare: Vec<MessageRow> = (0..60).map(|_| MessageRow::default()).collect();
    let mut rows = Vec::new();
    for chat in store.recent_chats(40).unwrap() {
        let start = Instant::now();
        let conversation = tern_ui::model::conversation(&store, &chat.id, None, 60, now);
        let read = ms(start);
        let items: Vec<_> = conversation.items.into_iter().map(Rc::new).collect();
        let start = Instant::now();
        let new: Vec<MessageRow> = items.iter().map(|_| MessageRow::default()).collect();
        let build = ms(start);
        let start = Instant::now();
        new.iter()
            .zip(&items)
            .for_each(|(row, item)| row.bind(item));
        let first = ms(start);
        let start = Instant::now();
        for row in &new {
            row.measure(gtk4::Orientation::Horizontal, -1);
            row.measure(gtk4::Orientation::Vertical, WIDTH);
        }
        let measure = ms(start);
        let start = Instant::now();
        spare
            .iter()
            .zip(&items)
            .for_each(|(row, item)| row.bind(item));
        let again = ms(start);
        let start = Instant::now();
        for (row, _) in spare.iter().zip(&items) {
            row.measure(gtk4::Orientation::Horizontal, -1);
            row.measure(gtk4::Orientation::Vertical, WIDTH);
        }
        let remeasure = ms(start);
        rows.push((read, build, first, measure, again, remeasure, items.len()));
    }
    rows.sort_by(|a, b| (b.2 + b.3).total_cmp(&(a.2 + a.3)));
    println!("  read  build   fill  measure | reused: fill  measure");
    for (read, build, first, measure, again, remeasure, count) in rows.iter().take(8) {
        println!(
            "{read:6.1} {build:6.1} {first:6.1} {measure:8.1} | {again:12.1} {remeasure:8.1}  ({count})"
        );
    }
}
