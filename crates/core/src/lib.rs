//! The decision-making half of the MyTimeOff daemon.
//!
//! Everything here is pure: no timers, no windows, no sockets. Time arrives as a
//! parameter and decisions leave as [`Command`]s for the caller to carry out. That is
//! what makes the rules that actually matter — the grace window, session binding,
//! permission prompts outranking completion — testable without a running desktop.

mod machine;

pub use machine::{Alert, Command, Event, Machine, Millis, ReaderMode, SessionId, State};
