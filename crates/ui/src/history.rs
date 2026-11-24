//! When to show or fetch older messages. No widgets, so it can be tested.

/// What to do at the start of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    Nothing,
    /// The cache has more: show a longer part of it.
    Show,
    /// The cache ends here: ask the service.
    Fetch,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    /// How many messages of the cache are shown at most, and how many there were.
    pub limit: usize,
    pub count: usize,
    /// A read or a fetch is on its way.
    reading: bool,
    fetching: bool,
    /// The last fetch brought nothing: not tried again until the user scrolls.
    dry: bool,
}

pub const PAGE: usize = 60;

impl History {
    pub fn new() -> History {
        History {
            limit: PAGE,
            ..Default::default()
        }
    }

    pub fn is_fetching(&self) -> bool {
        self.fetching
    }

    /// `at_start`: the first message is (nearly) in view. `more`: the service may have older ones.
    pub fn need(&mut self, at_start: bool, more: bool) -> Need {
        if !at_start || self.count == 0 || self.reading || self.fetching {
            return Need::Nothing;
        }
        if self.count >= self.limit {
            self.limit += PAGE;
            self.reading = true;
            return Need::Show;
        }
        if !more || self.dry {
            return Need::Nothing;
        }
        self.fetching = true;
        Need::Fetch
    }

    /// The cache was read.
    pub fn read(&mut self, count: usize) {
        self.reading = false;
        if count > self.count {
            self.dry = false;
        }
        self.count = count;
    }

    /// A fetch ended. What it brought shows with the next read, so there has to be room for it.
    pub fn fetched(&mut self) {
        self.fetching = false;
        self.dry = true;
        self.limit = self.limit.max(self.count + PAGE);
        self.reading = true;
    }

    /// The user scrolled: a fetch that brought nothing may be tried again.
    pub fn touched(&mut self) {
        self.dry = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads back with the user at the start, as long as something happens. `cache` is what the
    /// cache has, `service` what each fetch adds, until it is used up.
    fn back(mut cache: usize, service: &[usize]) -> (usize, usize) {
        let mut history = History::new();
        history.read(cache.min(history.limit));
        let mut service = service.iter();
        let mut fetches = 0;
        for _ in 0..100 {
            let more = service.len() > 0;
            match history.need(true, more) {
                Need::Nothing => break,
                Need::Show => history.read(cache.min(history.limit)),
                Need::Fetch => {
                    fetches += 1;
                    cache += service.next().copied().unwrap_or(0);
                    history.fetched();
                    history.read(cache.min(history.limit));
                }
            }
        }
        (history.count, fetches)
    }

    #[test]
    fn everything_is_reached() {
        assert_eq!(back(12, &[50, 50, 7]), (119, 3));
        assert_eq!(back(0, &[50]), (0, 0), "nothing shown, nothing to scroll");
        // Exactly one page in the cache: used to get stuck.
        assert_eq!(back(60, &[50]), (110, 1));
        assert_eq!(back(120, &[50, 50]), (220, 2));
        assert_eq!(back(639, &[]), (639, 0));
        assert_eq!(back(61, &[1, 1, 1]), (64, 3));
    }

    #[test]
    fn a_dry_fetch_waits_for_the_user() {
        let mut history = History::new();
        history.read(10);
        assert_eq!(history.need(true, true), Need::Fetch);
        assert_eq!(history.need(true, true), Need::Nothing, "one at a time");
        history.fetched();
        history.read(10);
        assert_eq!(history.need(true, true), Need::Nothing, "brought nothing");
        history.touched();
        assert_eq!(history.need(true, true), Need::Fetch);
    }

    #[test]
    fn nothing_away_from_the_start_or_past_the_end() {
        let mut history = History::new();
        history.read(60);
        assert_eq!(history.need(false, true), Need::Nothing);
        history.read(20);
        assert_eq!(history.need(true, false), Need::Nothing);
    }
}
