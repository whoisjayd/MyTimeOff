//! What the user gets to decide.
//!
//! Parsing lives here rather than in the daemon for the same reason the state machine
//! does: the interesting behaviour is in what a *partial* or *wrong* file does, and that
//! is worth testing without touching a disk.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::machine::ReaderMode;

/// Turns shorter than this never take the screen.
const DEFAULT_GRACE_MS: u64 = 20_000;
/// Loopback port. Fixed by default so hook wiring survives restarts.
const DEFAULT_PORT: u16 = 8787;
/// A day's reading, in pages that actually counted.
const DEFAULT_DAILY_PAGE_GOAL: u32 = 20;
/// Below this, a page was turned past rather than read.
const DEFAULT_SKIM_THRESHOLD_MS: u64 = 3_000;
/// Questions asked at the gate. Short enough to answer from memory of what you just read.
const DEFAULT_QUESTIONS_PER_GATE: u32 = 3;
/// Asked for the gate's questions. The fast model on purpose: this runs while the reader
/// is waiting to get back to work, and a better question is not worth twenty more seconds
/// of standing at a closed gate.
const DEFAULT_MODEL: &str = "claude-haiku-4-5-20251001";

/// Every field is optional in the file and independently defaulted, so a config written
/// by an older version keeps working when a field is added.
///
/// Unknown fields are refused, which is the opposite trade: it means a *newer* file
/// breaks an older binary. That is a downgrade, and rare. A mistyped key silently doing
/// nothing is neither - it is the failure a hand-edited config actually has, and the one
/// worth catching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub mode: ReaderMode,
    /// Zero is allowed and means "take the screen the moment a turn starts". In
    /// milliseconds, like every other duration here - two units in one small file is a
    /// bug waiting for someone to write 20 where they meant 20000.
    pub grace_ms: u64,
    /// Zero means no goal rather than an impossible one.
    pub daily_page_goal: u32,
    pub port: u16,
    pub skim_threshold_ms: u64,
    /// Zero means the gate asks nothing, which releases immediately - the same effect as
    /// free mode, reached from the other direction.
    pub questions_per_gate: u32,
    /// Which model writes the questions.
    ///
    /// Empty means none: the gate falls back to questions made on this machine, and no
    /// page you read is ever sent anywhere. That is the whole of the privacy switch, and
    /// it is one field because a second one would be a second thing to get wrong.
    pub model: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mode: ReaderMode::Strict,
            grace_ms: DEFAULT_GRACE_MS,
            daily_page_goal: DEFAULT_DAILY_PAGE_GOAL,
            port: DEFAULT_PORT,
            skim_threshold_ms: DEFAULT_SKIM_THRESHOLD_MS,
            questions_per_gate: DEFAULT_QUESTIONS_PER_GATE,
            model: DEFAULT_MODEL.to_string(),
        }
    }
}

impl Config {
    pub fn parse(json: &str) -> Result<Self, ConfigError> {
        let config: Config = serde_json::from_str(json).map_err(ConfigError::Malformed)?;
        config.validate()?;
        Ok(config)
    }

    /// Only refuses what cannot mean anything.
    ///
    /// A surprising grace window or an unreachable goal is the user's business; port 0 is
    /// not, because the OS would assign a random one and every hook URL already written
    /// into `settings.json` would point at nothing.
    fn validate(&self) -> Result<(), ConfigError> {
        if self.port == 0 {
            return Err(ConfigError::Invalid("port 0 would move the daemon every restart"));
        }
        Ok(())
    }

    pub fn grace(&self) -> Duration {
        Duration::from_millis(self.grace_ms)
    }

    /// Pretty-printed, because the point of writing this file out is that it gets edited.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("config is plain data")
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Malformed(serde_json::Error),
    Invalid(&'static str),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Malformed(error) => write!(f, "{error}"),
            ConfigError::Invalid(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Malformed(error) => Some(error),
            ConfigError::Invalid(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_is_the_defaults() {
        assert_eq!(Config::parse("{}").expect("empty config"), Config::default());
    }

    #[test]
    fn a_partial_file_keeps_the_defaults_for_everything_else() {
        let config = Config::parse(r#"{"mode":"lenient"}"#).expect("partial config");
        assert_eq!(config.mode, ReaderMode::Lenient);
        assert_eq!(config.grace_ms, Config::default().grace_ms);
    }

    #[test]
    fn modes_are_written_the_way_a_person_would_type_them() {
        for (text, mode) in [
            ("strict", ReaderMode::Strict),
            ("lenient", ReaderMode::Lenient),
            ("free", ReaderMode::Free),
        ] {
            let config = Config::parse(&format!(r#"{{"mode":"{text}"}}"#)).expect(text);
            assert_eq!(config.mode, mode);
        }
    }

    #[test]
    fn a_mistyped_key_is_refused_rather_than_ignored() {
        // The whole failure mode this guards: you edit grace_secondz, nothing changes, and
        // nothing tells you why.
        let error = Config::parse(r#"{"grace_secondz":5}"#).expect_err("typo must be caught");
        assert!(error.to_string().contains("grace_secondz"), "{error}");
    }

    #[test]
    fn an_unknown_mode_is_refused() {
        assert!(Config::parse(r#"{"mode":"relaxed"}"#).is_err());
    }

    #[test]
    fn port_zero_is_refused() {
        // It would move the daemon on every restart and strand the wired hook URLs.
        assert!(Config::parse(r#"{"port":0}"#).is_err());
    }

    #[test]
    fn zero_grace_is_allowed_and_means_take_the_screen_at_once() {
        let config = Config::parse(r#"{"grace_ms":0}"#).expect("zero grace");
        assert_eq!(config.grace(), Duration::ZERO);
    }

    #[test]
    fn an_empty_model_is_allowed_and_means_ask_no_one() {
        // The privacy switch. Refusing it would leave no way to keep the pages local.
        let config = Config::parse(r#"{"model":""}"#).expect("no model");
        assert!(config.model.is_empty());
    }

    #[test]
    fn what_is_written_can_be_read_back() {
        let config = Config { mode: ReaderMode::Free, port: 9000, ..Config::default() };
        assert_eq!(Config::parse(&config.to_json()).expect("round trip"), config);
    }
}
