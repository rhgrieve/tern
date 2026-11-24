//! Spell checking with Enchant, the library most of the desktop uses for it. It is loaded if it is
//! there; without it, or without a dictionary for the user's language, nothing is checked. Words are
//! looked up on a thread of their own: dictionaries are large and suggestions slow.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use tokio::sync::oneshot;

/// A word in a text, by characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// Longer texts are not written by hand.
const MOST: usize = 20_000;
const SUGGESTIONS: usize = 5;
const WAIT: Duration = Duration::from_millis(250);

/// The words worth looking up: no addresses, names of people mentioned, code, file names or
/// abbreviations.
pub fn words(text: &str, names: &[String]) -> Vec<Word> {
    let mut skipped: Vec<std::ops::Range<usize>> = tern_engine::bare_links(text)
        .into_iter()
        .map(|(range, _)| range)
        .collect();
    skipped.extend(code(text));
    for name in names {
        let mention = format!("@{name}");
        skipped.extend(
            text.match_indices(&mention)
                .map(|(at, found)| at..at + found.len()),
        );
    }
    let mut found = Vec::new();
    let mut character = 0;
    for piece in text.split_inclusive(char::is_whitespace) {
        let at = piece.as_ptr() as usize - text.as_ptr() as usize;
        let token = piece.trim_end().trim_matches(|letter: char| {
            !letter.is_alphanumeric() && !matches!(letter, '@' | '#' | '/' | '_' | ':')
        });
        if plain(token) {
            for word in split(piece) {
                let start = at + word.start;
                let within = skipped
                    .iter()
                    .any(|range| range.start < at + word.end && start < range.end);
                let letters = &piece[word.clone()];
                if !within && worth(letters) {
                    let first = character + piece[..word.start].chars().count();
                    found.push(Word {
                        start: first,
                        end: first + letters.chars().count(),
                        text: letters.replace('’', "'"),
                    });
                }
            }
        }
        character += piece.chars().count();
    }
    found
}

/// Between backticks, or in a block that is not closed yet.
fn code(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(open) = text[at..].find('`').map(|open| at + open) {
        let mark = if text[open..].starts_with("```") {
            "```"
        } else {
            "`"
        };
        let inside = open + mark.len();
        let close = text[inside..].find(mark).map(|close| inside + close);
        let line = text[inside..].find('\n').map(|line| inside + line);
        match close {
            // Inline code ends with its line.
            Some(close) if mark.len() == 3 || line.is_none_or(|line| close < line) => {
                at = close + mark.len();
                out.push(open..at);
            }
            None if mark.len() == 3 => {
                out.push(open..text.len());
                break;
            }
            _ => at = inside,
        }
    }
    out
}

/// A token of prose, not a path, an identifier or a number.
fn plain(token: &str) -> bool {
    let dotted = token
        .char_indices()
        .any(|(at, letter)| letter == '.' && token[at + 1..].starts_with(char::is_alphanumeric));
    !dotted
        && !token.chars().any(|letter| {
            letter.is_numeric() || matches!(letter, '/' | '\\' | '_' | '@' | '#' | '=' | ':')
        })
}

fn split(piece: &str) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut start = None;
    for (at, letter) in piece.char_indices().chain([(piece.len(), ' ')]) {
        let inner = matches!(letter, '\'' | '’')
            && start.is_some()
            && piece[at + letter.len_utf8()..].starts_with(char::is_alphabetic);
        if letter.is_alphabetic() || inner {
            start.get_or_insert(at);
        } else if let Some(start) = start.take() {
            out.push(start..at);
        }
    }
    out
}

fn worth(word: &str) -> bool {
    let mut letters = word.chars();
    let first = letters.next();
    let rest = letters.as_str();
    first.is_some()
        && !rest.is_empty()
        // No capitals but the first: neither abbreviations nor CamelCase.
        && !rest.chars().any(char::is_uppercase)
        // Scripts that do not write words apart are not for word lists.
        && word.chars().all(|letter| letter < '\u{530}' || letter == '’')
}

mod enchant {
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    use std::ptr;

    use libloading::{Library, Symbol};

    type Broker = *mut c_void;
    type Dict = *mut c_void;

    /// Owned by one thread, as Enchant asks for.
    pub struct Dictionary {
        library: Library,
        dict: Dict,
    }

    impl Dictionary {
        /// The first of `languages` there is a dictionary for.
        pub fn open(languages: &[String]) -> Option<Dictionary> {
            // SAFETY: Enchant's initialisers have no requirements, and the signatures are those of
            // its stable C interface, which the number in the library's name stands for.
            unsafe {
                let library = Library::new("libenchant-2.so.2").ok()?;
                let init: Symbol<unsafe extern "C" fn() -> Broker> =
                    library.get(b"enchant_broker_init\0").ok()?;
                let exists: Symbol<unsafe extern "C" fn(Broker, *const c_char) -> c_int> =
                    library.get(b"enchant_broker_dict_exists\0").ok()?;
                let request: Symbol<unsafe extern "C" fn(Broker, *const c_char) -> Dict> =
                    library.get(b"enchant_broker_request_dict\0").ok()?;
                // Never freed: the dictionary is in use until the program ends.
                let broker = init();
                if broker.is_null() {
                    return None;
                }
                let dict = languages.iter().find_map(|language| {
                    let tag = CString::new(language.as_str()).ok()?;
                    if exists(broker, tag.as_ptr()) == 0 {
                        return None;
                    }
                    let dict = request(broker, tag.as_ptr());
                    (!dict.is_null()).then_some(dict)
                })?;
                Some(Dictionary { library, dict })
            }
        }

        pub fn knows(&self, word: &str) -> bool {
            // SAFETY: `dict` is valid, and the word's bytes outlive the call.
            unsafe {
                let Ok(check) =
                    self.library
                        .get::<unsafe extern "C" fn(Dict, *const c_char, isize) -> c_int>(
                            b"enchant_dict_check\0",
                        )
                else {
                    return true;
                };
                // Below zero when it cannot tell.
                check(self.dict, word.as_ptr().cast(), word.len() as isize) <= 0
            }
        }

        pub fn suggest(&self, word: &str, most: usize) -> Vec<String> {
            // SAFETY: as above; the list is read before it is given back to Enchant.
            unsafe {
                let suggest = self.library.get::<unsafe extern "C" fn(
                    Dict,
                    *const c_char,
                    isize,
                    *mut usize,
                ) -> *mut *mut c_char>(
                    b"enchant_dict_suggest\0"
                );
                let free = self
                    .library
                    .get::<unsafe extern "C" fn(Dict, *mut *mut c_char)>(
                        b"enchant_dict_free_string_list\0",
                    );
                let (Ok(suggest), Ok(free)) = (suggest, free) else {
                    return Vec::new();
                };
                let mut count = 0usize;
                let list = suggest(
                    self.dict,
                    word.as_ptr().cast(),
                    word.len() as isize,
                    ptr::from_mut(&mut count),
                );
                if list.is_null() {
                    return Vec::new();
                }
                let found = (0..count.min(most))
                    .filter_map(|at| CStr::from_ptr(*list.add(at)).to_str().ok())
                    .map(str::to_owned)
                    .collect();
                free(self.dict, list);
                found
            }
        }

        /// For good, or until the program ends.
        pub fn add(&self, word: &str, for_good: bool) {
            let name: &[u8] = if for_good {
                b"enchant_dict_add\0"
            } else {
                b"enchant_dict_add_to_session\0"
            };
            // SAFETY: as above.
            unsafe {
                if let Ok(add) = self
                    .library
                    .get::<unsafe extern "C" fn(Dict, *const c_char, isize)>(name)
                {
                    add(self.dict, word.as_ptr().cast(), word.len() as isize);
                }
            }
        }
    }
}

enum Job {
    /// Answers with suggestions for each word that is wrong.
    Check(Vec<String>, oneshot::Sender<Vec<Option<Vec<String>>>>),
    Add(String, bool),
}

/// `en_CA` before `en`, and then whatever else is written in the language.
fn languages() -> Vec<String> {
    let mut out: Vec<String> = glib::language_names()
        .iter()
        .map(|name| name.split(['.', '@']).next().unwrap_or(name).to_owned())
        .filter(|name| name != "C" && name != "POSIX")
        .collect();
    if out.iter().any(|name| name.starts_with("en")) {
        out.extend(["en_US", "en_GB"].map(str::to_owned));
    }
    out.dedup();
    out
}

/// What is known about words, for all windows.
struct Speller {
    jobs: mpsc::Sender<Job>,
    /// Suggestions for what is wrong; nothing for what is right.
    known: RefCell<HashMap<String, Option<Vec<String>>>>,
}

impl Speller {
    fn start() -> Rc<Speller> {
        let (jobs, queue) = mpsc::channel();
        let languages = languages();
        std::thread::spawn(move || work(&languages, &queue));
        Rc::new(Speller {
            jobs,
            known: RefCell::default(),
        })
    }

    fn shared() -> Rc<Speller> {
        thread_local! {
            static SHARED: std::cell::OnceCell<Rc<Speller>> = const { std::cell::OnceCell::new() };
        }
        SHARED.with(|shared| shared.get_or_init(Speller::start).clone())
    }

    fn add(&self, word: &str, for_good: bool) {
        self.known.borrow_mut().insert(word.to_owned(), None);
        let _ = self.jobs.send(Job::Add(word.to_owned(), for_good));
    }
}

fn work(languages: &[String], queue: &mpsc::Receiver<Job>) {
    // Nothing to write, no dictionary in memory.
    let Ok(first) = queue.recv() else { return };
    let Some(dictionary) = enchant::Dictionary::open(languages) else {
        return;
    };
    let mut next = Some(first);
    while let Some(job) = next {
        match job {
            Job::Check(words, reply) => {
                let answers = words.iter().map(|word| {
                    (!dictionary.knows(word)).then(|| dictionary.suggest(word, SUGGESTIONS))
                });
                let _ = reply.send(answers.collect());
            }
            Job::Add(word, for_good) => dictionary.add(&word, for_good),
        }
        next = queue.recv().ok();
    }
}

/// Underlines in one text view what is not in the dictionary, and offers what is close.
struct Check {
    text: glib::WeakRef<gtk::TextView>,
    speller: Rc<Speller>,
    tag: gtk::TextTag,
    names: Box<dyn Fn() -> Vec<String>>,
    waits: Cell<bool>,
    wrong: RefCell<Vec<Word>>,
    menu: gio::Menu,
    /// The word the menu is about.
    about: RefCell<Option<Word>>,
}

/// `names` are those of people mentioned in the text.
pub fn attach(text: &gtk::TextView, names: impl Fn() -> Vec<String> + 'static) {
    let buffer = text.buffer();
    let tag = gtk::TextTag::builder()
        .underline(gtk::pango::Underline::Error)
        .underline_rgba(&gdk::RGBA::new(0.93, 0.33, 0.38, 1.0))
        .build();
    buffer.tag_table().add(&tag);
    let check = Rc::new(Check {
        text: text.downgrade(),
        speller: Speller::shared(),
        tag,
        names: Box::new(names),
        waits: Cell::new(false),
        wrong: RefCell::default(),
        menu: gio::Menu::new(),
        about: RefCell::default(),
    });
    text.set_extra_menu(Some(&check.menu));
    buffer.connect_changed({
        let check = check.clone();
        move |_| check.soon()
    });
    buffer.connect_cursor_position_notify({
        let check = check.clone();
        move |buffer| {
            check.soon();
            check.fill_menu(buffer.cursor_position());
        }
    });
    text.connect_has_focus_notify({
        let check = check.clone();
        move |_| check.soon()
    });
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    click.connect_pressed({
        let check = check.clone();
        move |_, _, x, y| {
            let Some(text) = check.text.upgrade() else {
                return;
            };
            let (x, y) =
                text.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
            let place = text.iter_at_location(x, y);
            check.fill_menu(place.map_or(-1, |place| place.offset()));
        }
    });
    text.add_controller(click);

    let actions = gio::SimpleActionGroup::new();
    let replace = gio::SimpleAction::new("replace", Some(glib::VariantTy::STRING));
    replace.connect_activate({
        let check = check.clone();
        move |_, with| {
            if let Some(with) = with.and_then(|with| with.get::<String>()) {
                check.replace(&with);
            }
        }
    });
    actions.add_action(&replace);
    for (name, for_good) in [("learn", true), ("ignore", false)] {
        let action = gio::SimpleAction::new(name, None);
        action.connect_activate({
            let check = check.clone();
            move |_, _| {
                let about = check.about.borrow().clone();
                if let Some(word) = about {
                    check.speller.add(&word.text, for_good);
                    check.show();
                }
            }
        });
        actions.add_action(&action);
    }
    text.insert_action_group("spell", Some(&actions));
}

impl Check {
    fn soon(self: &Rc<Self>) {
        if self.waits.replace(true) {
            return;
        }
        let check = self.clone();
        glib::timeout_add_local_once(WAIT, move || {
            check.waits.set(false);
            check.now();
        });
    }

    fn words(&self) -> Vec<Word> {
        let Some(text) = self.text.upgrade() else {
            return Vec::new();
        };
        let buffer = text.buffer();
        if buffer.char_count() > MOST as i32 {
            return Vec::new();
        }
        let (start, end) = buffer.bounds();
        words(&buffer.text(&start, &end, false), &(self.names)())
    }

    fn now(self: &Rc<Self>) {
        let unknown: Vec<String> = {
            let known = self.speller.known.borrow();
            let new: HashSet<String> = self
                .words()
                .into_iter()
                .map(|word| word.text)
                .filter(|word| !known.contains_key(word))
                .collect();
            new.into_iter().collect()
        };
        if unknown.is_empty() {
            return self.show();
        }
        let (reply, answer) = oneshot::channel();
        if self
            .speller
            .jobs
            .send(Job::Check(unknown.clone(), reply))
            .is_err()
        {
            return;
        }
        let check = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(answers) = answer.await {
                check
                    .speller
                    .known
                    .borrow_mut()
                    .extend(unknown.into_iter().zip(answers));
                check.show();
            }
        });
    }

    fn show(&self) {
        let Some(text) = self.text.upgrade() else {
            return;
        };
        let buffer = text.buffer();
        // Nobody is told about a word while writing it.
        let writing = text.has_focus().then(|| buffer.cursor_position() as usize);
        let wrong: Vec<Word> = {
            let known = self.speller.known.borrow();
            self.words()
                .into_iter()
                .filter(|word| matches!(known.get(&word.text), Some(Some(_))))
                .filter(|word| writing != Some(word.end))
                .collect()
        };
        let (start, end) = buffer.bounds();
        buffer.remove_tag(&self.tag, &start, &end);
        for word in &wrong {
            buffer.apply_tag(
                &self.tag,
                &buffer.iter_at_offset(word.start as i32),
                &buffer.iter_at_offset(word.end as i32),
            );
        }
        self.wrong.replace(wrong);
    }

    fn fill_menu(&self, offset: i32) {
        let word = usize::try_from(offset).ok().and_then(|offset| {
            let wrong = self.wrong.borrow();
            wrong
                .iter()
                .find(|word| (word.start..=word.end).contains(&offset))
                .cloned()
        });
        if *self.about.borrow() == word {
            return;
        }
        self.menu.remove_all();
        if let Some(word) = &word {
            let known = self.speller.known.borrow();
            let suggestions = known.get(&word.text).cloned().flatten().unwrap_or_default();
            let close = gio::Menu::new();
            for suggestion in &suggestions {
                // Underscores would be shortcuts.
                let item = gio::MenuItem::new(Some(&suggestion.replace('_', "__")), None);
                item.set_action_and_target_value(
                    Some("spell.replace"),
                    Some(&suggestion.to_variant()),
                );
                close.append_item(&item);
            }
            let keep = gio::Menu::new();
            keep.append(Some("Add to Dictionary"), Some("spell.learn"));
            keep.append(Some("Ignore"), Some("spell.ignore"));
            self.menu.append_section(None, &close);
            self.menu.append_section(None, &keep);
        }
        self.about.replace(word);
    }

    fn replace(&self, with: &str) {
        let (Some(word), Some(text)) = (self.about.borrow().clone(), self.text.upgrade()) else {
            return;
        };
        let buffer = text.buffer();
        let mut start = buffer.iter_at_offset(word.start as i32);
        let mut end = buffer.iter_at_offset(word.end as i32);
        // The text may have changed behind an open menu.
        if buffer.text(&start, &end, false).replace('’', "'") != word.text {
            return;
        }
        buffer.begin_user_action();
        buffer.delete(&mut start, &mut end);
        buffer.insert(&mut start, with);
        buffer.end_user_action();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(text: &str) -> Vec<String> {
        words(text, &["Ann Quux".into()])
            .into_iter()
            .map(|word| word.text)
            .collect()
    }

    #[test]
    fn only_prose_is_checked() {
        assert_eq!(
            of("Hello, wrld! It's a well-knwon \"fact\" (really)."),
            ["Hello", "wrld", "It's", "well", "knwon", "fact", "really"]
        );
        assert_eq!(of("don’t 'quoted' it's'"), ["don't", "quoted", "it's"]);
        for none in [
            "https://exmple.com/abot www.exmple.com",
            "`cde`",
            "```\nsme cde\n```",
            "```\nnot clsed",
            "@Ann Quux",
            "@smbody #tpic :smle: :smle",
            "src/mian.rs main.rs foo_bar FooBar NASA x 3rd a1b2 k=v C:\\drve",
            "日本語も 🎉",
        ] {
            assert_eq!(of(none), [""; 0], "{none}");
        }
        assert_eq!(of("a `cde` wrd ` open"), ["wrd", "open"]);
        assert_eq!(of("end of sentnce. Next"), ["end", "of", "sentnce", "Next"]);
        assert_eq!(of("@Ann Quux helo"), ["helo"]);
    }

    #[test]
    fn words_are_placed_by_characters() {
        let text = "é 🎉 wrd, ü-wrt\nnxt";
        for word in words(text, &[]) {
            let at: String = text
                .chars()
                .skip(word.start)
                .take(word.end - word.start)
                .collect();
            assert_eq!(at, word.text);
        }
        assert_eq!(words(text, &[]).len(), 3);
    }

    /// As the composer's test: needs a display.
    #[test]
    #[ignore = "needs a display"]
    fn wrong_words_are_marked_and_replaced() {
        gtk::init().unwrap();
        // As if a dictionary had answered.
        Speller::shared().known.borrow_mut().extend([
            ("helo".to_owned(), Some(vec!["hello".to_owned()])),
            ("world".to_owned(), None),
        ]);
        let text = gtk::TextView::new();
        attach(&text, Vec::new);
        let other = gtk::Button::with_label("other");
        let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        holder.append(&text);
        holder.append(&other);
        let window = gtk::Window::builder().child(&holder).build();
        window.present();
        let wait = || {
            let end = std::time::Instant::now() + WAIT * 3;
            while std::time::Instant::now() < end {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        let buffer = text.buffer();
        let marked = || -> String {
            let (mut at, end) = buffer.bounds();
            let mut out = String::new();
            while at < end {
                let wrong = at
                    .tags()
                    .iter()
                    .any(|tag| tag.underline() == gtk::pango::Underline::Error);
                out.push(if wrong { '~' } else { '.' });
                at.forward_char();
            }
            out
        };
        text.grab_focus();
        buffer.insert_at_cursor("helo world helo");
        wait();
        let writing = if text.has_focus() {
            "....".to_owned()
        } else {
            "~~~~".to_owned()
        };
        assert_eq!(marked(), format!("~~~~.......{writing}"));
        // Done with it.
        other.grab_focus();
        wait();
        assert_eq!(marked(), "~~~~.......~~~~");
        // The menu is about the word at the cursor.
        buffer.place_cursor(&buffer.iter_at_offset(12));
        text.activate_action("spell.replace", Some(&"hello".to_variant()))
            .unwrap();
        wait();
        let all = || buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
        assert_eq!(all(), "helo world hello");
        buffer.place_cursor(&buffer.start_iter());
        text.activate_action("spell.ignore", None).unwrap();
        assert!(!marked().contains('~'), "{}", marked());
        assert_eq!(all(), "helo world hello");
        // Off a wrong word there is nothing to replace.
        buffer.place_cursor(&buffer.iter_at_offset(7));
        text.activate_action("spell.replace", Some(&"x".to_variant()))
            .unwrap();
        assert_eq!(all(), "helo world hello");
    }
}
