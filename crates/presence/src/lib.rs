//! Knows whether the user is at the computer, from the desktop rather than from one window.

mod idle;
mod logind;
mod model;
mod mutter;
mod network;
mod sensors;

pub use logind::SleepGuard;
pub use model::{Desired, Input, Machine, State};
pub use sensors::{Config, Event, Sensors, Source};
