use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use tern_engine::{Place, place};
use tern_ui::desk::Desk;
use tern_ui::join::as_guest;
use tern_ui::service;
use tern_ui::window::{Window, load_style, save_picture};

const APP: &str = "io.github.rhgrieve.Tern";

/// Options for trying things out: `--demo`, and with it `--open ID`, `--own`, `--size WxH`, `--dark`,
/// `--light`, `--calendar`, `--calling`, `--screenshot FILE`. `--measure` times the real cache without showing that
/// anything was read.
#[derive(Clone, Default)]
struct Options {
    demo: bool,
    open: Option<String>,
    /// In a window of its own.
    own: bool,
    size: Option<(i32, i32)>,
    dark: Option<bool>,
    screenshot: Option<String>,
    measure: bool,
    calendar: bool,
    /// Shows a call with the open chat, without calling.
    calling: bool,
    /// Addresses of Teams to open.
    addresses: Vec<String>,
}

fn options() -> Options {
    let mut options = Options::default();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--demo" => options.demo = true,
            "--dark" => options.dark = Some(true),
            "--light" => options.dark = Some(false),
            "--own" => options.own = true,
            "--measure" => options.measure = true,
            "--calendar" => options.calendar = true,
            "--calling" => options.calling = true,
            "--open" => options.open = arguments.next(),
            "--screenshot" => options.screenshot = arguments.next(),
            "--size" => {
                options.size = arguments.next().and_then(|size| {
                    let (width, height) = size.split_once('x')?;
                    Some((width.parse().ok()?, height.parse().ok()?))
                });
            }
            _ if tern_engine::place(&argument).is_some() => options.addresses.push(argument),
            _ => {}
        }
    }
    options
}

fn start(desk: &Rc<Desk>, options: &Options) -> Rc<Window> {
    load_style();
    let app = desk.app();
    if let Some(dark) = options.dark {
        let scheme = if dark {
            adw::ColorScheme::ForceDark
        } else {
            adw::ColorScheme::ForceLight
        };
        adw::StyleManager::default().set_color_scheme(scheme);
    }
    if options.demo {
        desk.use_store(Arc::new(tern_ui::demo::store()), 1);
        // Something half written, to be seen in the list.
        desk.drafts.borrow_mut().insert(
            "c3".into(),
            tern_ui::composer::Written {
                text: "Sounds **good**, I'll bring the `slides` and\n- the [plan](https://example.com/plan)\n- notes from https://example.com".into(),
                ..Default::default()
            },
        );
        tern_ui::presence::update(&tern_ui::demo::presence());
    }
    let window = desk.main();
    if let Some((width, height)) = options.size {
        window.window.set_default_size(width, height);
    }
    window.window.present();
    if let Some(id) = &options.open {
        let (conversation, post) = id.split_once('/').unwrap_or((id, ""));
        window.open(conversation);
        if options.own {
            desk.pop_out(conversation);
        }
        if !post.is_empty() {
            let (window, post) = (window.clone(), post.to_owned());
            glib::timeout_add_local_once(Duration::from_millis(500), move || {
                window.open_thread(&post)
            });
        }
    }
    if options.calendar {
        desk.show_calendar();
    }
    if options.calling && options.demo {
        let window = window.clone();
        // Once the chat is read and its title known.
        glib::timeout_add_local_once(Duration::from_millis(400), move || window.pretend_call());
    }
    if options.measure {
        measure(app, &window);
    }
    if let Some(path) = options.screenshot.clone() {
        let own = options.open.as_deref().and_then(|id| desk.chat(id));
        let (shown, app) = (own.unwrap_or(window.clone()).window.clone(), app.clone());
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            if let Err(error) = save_picture(&shown, &path) {
                eprintln!("tern: {error}");
            }
            app.quit();
        });
    }
    window
}

/// Times the first frame, and opening chats one after the other until their messages are drawn.
fn measure(app: &adw::Application, window: &Rc<Window>) {
    let begun = std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|stat| {
            stat.rsplit(") ")
                .next()?
                .split(' ')
                .nth(19)?
                .parse::<f64>()
                .ok()
        });
    let up = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|uptime| uptime.split(' ').next()?.parse::<f64>().ok());
    let before = begun
        .zip(up)
        .map_or(0.0, |(begun, up)| (up - begun / 100.0) * 1000.0);
    let start = Instant::now();
    let (app, window) = (app.clone(), window.clone());
    glib::spawn_future_local(async move {
        let wait = |milliseconds| glib::timeout_future(Duration::from_millis(milliseconds));
        while window.chats() == 0 && start.elapsed() < Duration::from_secs(5) {
            wait(2).await;
        }
        println!(
            "{} chats listed {:.0} ms after start",
            window.chats(),
            before + start.elapsed().as_secs_f64() * 1000.0
        );
        if std::env::args().any(|argument| argument == "--sidebar") {
            // After the window has settled: start-up is not scrolling.
            wait(2500).await;
            let mut gaps = window.measure_sidebar().await;
            let slow: Vec<String> = gaps
                .iter()
                .enumerate()
                .filter(|(_, gap)| **gap > 12.0)
                .map(|(at, gap)| format!("{at}:{gap:.0}"))
                .collect();
            println!("slow frames {}", slow.join(" "));
            gaps.sort_by(f64::total_cmp);
            let at = |share: f64| gaps[((gaps.len() - 1) as f64 * share) as usize];
            println!(
                "sidebar: median {:.1} ms, 95% {:.1}, 99% {:.1}, worst {:.1}, frames over 12 ms: {}",
                at(0.5),
                at(0.95),
                at(0.99),
                at(1.0),
                gaps.iter().filter(|gap| **gap > 12.0).count()
            );
            app.quit();
            return;
        }
        for number in 0..8 {
            let start = Instant::now();
            window.measure_open();
            while !window.is_shown() && start.elapsed() < Duration::from_secs(5) {
                wait(1).await;
            }
            println!(
                "chat {number}: {} messages in {:.0} ms, at its end: {}",
                window.list().items.n_items(),
                start.elapsed().as_secs_f64() * 1000.0,
                window.list().at_end()
            );
            wait(300).await;
            if !window.list().at_end() {
                println!("  not at the end after 300 ms");
            }
        }
        app.quit();
    });
}

fn main() -> glib::ExitCode {
    let options = options();
    let flags = if options.demo || options.measure {
        gio::ApplicationFlags::NON_UNIQUE
    } else {
        gio::ApplicationFlags::HANDLES_OPEN
    };
    let app = adw::Application::builder()
        .application_id(APP)
        .flags(flags)
        .build();
    let desk = Desk::new(&app, options.demo || options.measure);
    app.connect_startup({
        let quiet = options.demo || options.measure;
        move |_| {
            if !quiet {
                service::start();
            }
        }
    });
    let addresses = options.addresses.clone();
    let started = Rc::new(Cell::new(false));
    // The options are for the first time only.
    let begin = Rc::new({
        let desk = desk.clone();
        move || {
            let first = !started.replace(true);
            if first {
                start(&desk, &options);
            }
            first
        }
    });
    app.connect_activate({
        let (begin, desk) = (begin.clone(), desk.clone());
        move |_| {
            if !begin() {
                desk.main().window.present();
            }
        }
    });
    // Notifications and the tray open conversations through this.
    app.connect_open({
        let (begin, desk) = (begin.clone(), desk.clone());
        move |_, files, _| {
            // A meeting needs no window of Tern's.
            for file in files {
                let address = file.uri().to_string();
                let meeting = matches!(place(&address), Some(Place::Meeting(_)));
                if meeting && !as_guest(&address) {
                    desk.join(&address);
                } else {
                    begin();
                    desk.go(&address);
                }
            }
        }
    });
    let join = gio::SimpleAction::new("join", Some(glib::VariantTy::STRING));
    join.connect_activate({
        let (begin, desk) = (begin.clone(), desk.clone());
        move |_, address| {
            if let Some(address) = address.and_then(|address| address.get::<String>()) {
                if as_guest(&address) {
                    // It asks something.
                    begin();
                }
                desk.join(&address);
            }
        }
    });
    app.add_action(&join);
    // A meeting in the browser, for video and sharing.
    let browse = gio::SimpleAction::new("join-in-browser", Some(glib::VariantTy::STRING));
    browse.connect_activate({
        let (begin, desk) = (begin.clone(), desk.clone());
        move |_, address| {
            if let Some(address) = address.and_then(|address| address.get::<String>()) {
                if as_guest(&address) {
                    // It asks something.
                    begin();
                }
                desk.join_in_browser(&address);
            }
        }
    });
    app.add_action(&browse);
    let calendar = gio::SimpleAction::new("calendar", None);
    calendar.connect_activate({
        let (begin, desk) = (begin.clone(), desk.clone());
        move |_, _| {
            begin();
            desk.show_calendar();
        }
    });
    app.add_action(&calendar);
    let open = gio::SimpleAction::new("open", Some(glib::VariantTy::STRING));
    open.connect_activate({
        let (begin, desk) = (begin.clone(), desk.clone());
        move |_, id| {
            let id = id
                .and_then(|id| id.get::<String>())
                .filter(|id| !id.is_empty());
            if let Some(id) = id {
                begin();
                desk.open(&id);
            }
        }
    });
    app.add_action(&open);
    // Buttons of the notification of a call, with the push that told of it.
    for (name, answering) in [("answer", true), ("decline", false)] {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
        action.connect_activate({
            let (begin, desk) = (begin.clone(), desk.clone());
            move |_, push| {
                let push = push
                    .and_then(|push| push.get::<String>())
                    .filter(|push| !push.is_empty() && !push.contains('\n'));
                let Some(push) = push else { return };
                if answering {
                    begin();
                    desk.answer(&push);
                } else {
                    desk.decline(&push);
                }
            }
        });
        app.add_action(&action);
    }
    // Started by D-Bus, for one of the above.
    let service = std::env::args().find(|argument| argument == "--gapplication-service");
    let arguments: Vec<String> = std::iter::once("tern".to_owned())
        .chain(service)
        .chain(addresses)
        .collect();
    app.run_with_args(&arguments)
}
