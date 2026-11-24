use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use tern_ui::client::Client;
use tern_ui::window::{Window, load_style, save_picture};

const APP: &str = "io.github.tern.Tern";

/// Options for trying things out: `--demo`, and with it `--open ID`, `--size WxH`, `--dark`,
/// `--light`, `--screenshot FILE`. `--measure` times the real cache without showing that
/// anything was read.
#[derive(Clone, Default)]
struct Options {
    demo: bool,
    open: Option<String>,
    size: Option<(i32, i32)>,
    dark: Option<bool>,
    screenshot: Option<String>,
    measure: bool,
}

fn options() -> Options {
    let mut options = Options::default();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--demo" => options.demo = true,
            "--dark" => options.dark = Some(true),
            "--light" => options.dark = Some(false),
            "--measure" => options.measure = true,
            "--open" => options.open = arguments.next(),
            "--screenshot" => options.screenshot = arguments.next(),
            "--size" => {
                options.size = arguments.next().and_then(|size| {
                    let (width, height) = size.split_once('x')?;
                    Some((width.parse().ok()?, height.parse().ok()?))
                });
            }
            _ => {}
        }
    }
    options
}

fn start(app: &adw::Application, options: &Options) -> Rc<Window> {
    load_style();
    if let Some(dark) = options.dark {
        let scheme = if dark {
            adw::ColorScheme::ForceDark
        } else {
            adw::ColorScheme::ForceLight
        };
        adw::StyleManager::default().set_color_scheme(scheme);
    }
    let window = if options.measure {
        let window = Window::new(app, Client::nowhere(), true);
        window.use_cache();
        window
    } else if options.demo {
        let window = Window::new(app, Client::nowhere(), true);
        window.use_store(Arc::new(tern_ui::demo::store()), None);
        window
    } else {
        let (client, news) = Client::start();
        let window = Window::new(app, client, false);
        window.use_cache();
        window.follow(news);
        window
    };
    if let Some((width, height)) = options.size {
        window.window.set_default_size(width, height);
    }
    window.window.present();
    if let Some(id) = &options.open {
        let (conversation, post) = id.split_once('/').unwrap_or((id, ""));
        window.open(conversation);
        if !post.is_empty() {
            let (window, post) = (window.clone(), post.to_owned());
            glib::timeout_add_local_once(Duration::from_millis(500), move || {
                window.open_thread(&post)
            });
        }
    }
    if options.measure {
        measure(app, &window);
    }
    if let Some(path) = options.screenshot.clone() {
        let (shown, app) = (window.window.clone(), app.clone());
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
        gio::ApplicationFlags::FLAGS_NONE
    };
    let app = adw::Application::builder()
        .application_id(APP)
        .flags(flags)
        .build();
    let window: Rc<RefCell<Option<Rc<Window>>>> = Rc::default();
    app.connect_activate(move |app| {
        let mut window = window.borrow_mut();
        match window.as_ref().filter(|window| window.window.is_visible()) {
            Some(window) => window.window.present(),
            None => *window = Some(start(app, &options)),
        }
    });
    app.run_with_args::<&str>(&[])
}
