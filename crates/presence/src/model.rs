//! The decision, kept free of clocks and I/O so every case can be tested.

/// What the sensors report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// No keyboard or pointer input for the idle time, anywhere in the session.
    Idle(bool),
    Locked(bool),
    /// About to sleep, until resume.
    Sleeping(bool),
    /// Another session is in front (fast user switching, a text console).
    Foreground(bool),
    InCall(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Active,
    Idle,
    Locked,
    Sleeping,
    InCall,
}

/// What the presence service should be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Desired {
    pub registered: bool,
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Machine {
    idle: bool,
    locked: bool,
    sleeping: bool,
    foreground: bool,
    in_call: bool,
}

impl Default for Machine {
    /// Idle until a sensor says otherwise, so a broken sensor never shows someone as Available.
    fn default() -> Self {
        Machine {
            idle: true,
            locked: false,
            sleeping: false,
            foreground: true,
            in_call: false,
        }
    }
}

impl Machine {
    /// Answers the new state when it changed.
    pub fn apply(&mut self, input: Input) -> Option<State> {
        let before = self.state();
        match input {
            Input::Idle(value) => self.idle = value,
            Input::Locked(value) => self.locked = value,
            Input::Sleeping(value) => self.sleeping = value,
            Input::Foreground(value) => self.foreground = value,
            Input::InCall(value) => self.in_call = value,
        }
        let after = self.state();
        (before != after).then_some(after)
    }

    pub fn state(&self) -> State {
        if self.sleeping {
            State::Sleeping
        } else if self.in_call {
            State::InCall
        } else if self.locked || !self.foreground {
            State::Locked
        } else if self.idle {
            State::Idle
        } else {
            State::Active
        }
    }

    pub fn desired(&self) -> Desired {
        let state = self.state();
        Desired {
            registered: state != State::Sleeping,
            active: matches!(state, State::Active | State::InCall),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(inputs: &[Input]) -> Machine {
        let mut machine = Machine::default();
        for input in inputs {
            machine.apply(*input);
        }
        machine
    }

    #[test]
    fn nothing_known_is_not_available() {
        assert_eq!(
            Machine::default().desired(),
            Desired {
                registered: true,
                active: false
            }
        );
    }

    #[test]
    fn states_by_priority() {
        use Input::*;
        for (inputs, state) in [
            (&[Idle(false)][..], State::Active),
            (&[Idle(false), Idle(true)], State::Idle),
            (&[Idle(false), Locked(true)], State::Locked),
            (&[Idle(false), Locked(true), Locked(false)], State::Active),
            (&[Idle(false), Foreground(false)], State::Locked),
            (&[Idle(true), InCall(true)], State::InCall),
            (&[Idle(false), Locked(true), InCall(true)], State::InCall),
            (
                &[Idle(false), InCall(true), Sleeping(true)],
                State::Sleeping,
            ),
            (
                &[Idle(false), Sleeping(true), Sleeping(false)],
                State::Active,
            ),
            (
                &[Idle(false), Locked(true), Sleeping(true), Sleeping(false)],
                State::Locked,
            ),
        ] {
            assert_eq!(after(inputs).state(), state, "{inputs:?}");
        }
    }

    #[test]
    fn only_sleep_withdraws_the_device() {
        use Input::*;
        assert_eq!(
            after(&[Idle(false), Sleeping(true)]).desired(),
            Desired {
                registered: false,
                active: false
            }
        );
        assert_eq!(
            after(&[Idle(false), Locked(true)]).desired(),
            Desired {
                registered: true,
                active: false
            }
        );
    }

    #[test]
    fn repeats_are_not_changes() {
        let mut machine = Machine::default();
        assert_eq!(machine.apply(Input::Idle(false)), Some(State::Active));
        assert_eq!(machine.apply(Input::Idle(false)), None);
        // Going idle behind a lock screen changes nothing visible.
        machine.apply(Input::Locked(true));
        assert_eq!(machine.apply(Input::Idle(true)), None);
    }
}
