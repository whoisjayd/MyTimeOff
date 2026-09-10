//! The decision-making half of the MyTimeOff daemon.
//!
//! Everything here is pure: no timers, no windows, no sockets. Time arrives as a
//! parameter and decisions leave as [`Command`]s for the caller to carry out. That is
//! what makes the rules that actually matter - the grace window, session binding,
//! permission prompts outranking completion - testable without a running desktop.

mod config;
mod hook;
mod machine;
mod quiz;
mod reading;

pub use config::{Config, ConfigError};
pub use hook::{HookPayload, NotifyPayload};
pub use machine::{Alert, Command, Event, Machine, Millis, ReaderMode, SessionId, State};
pub use quiz::{
    Answer, AskedQuestion, Gate, PassMark, Policy, Question, Quiz, Refusal, Release, Score,
    Submission, Verdict,
};
pub use reading::{Book, BookFormat, Locator, PageView, Resume};
