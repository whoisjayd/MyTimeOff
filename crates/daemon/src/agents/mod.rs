//! Wiring MyTimeOff into the coding agents that announce what they are doing.
//!
//! Two agents, and almost nothing shared between them. Claude Code takes JSON in
//! `~/.claude/settings.json` and can POST to a URL by itself; Codex takes TOML in
//! `~/.codex/config.toml` and only knows how to run a command, so it needs this program
//! as a shim in between. Neither file belongs to us: both are hand-edited, both hold
//! settings that have nothing to do with reading, and both have to come out of this
//! unchanged apart from the lines we put in.
//!
//! What this module is for is the part a user actually cares about, which is the same for
//! both: *is my agent connected, and can I connect or disconnect it without opening the
//! file myself*. `claude.rs` and `codex.rs` answer that in their own formats; everything
//! above them - the CLI, the HTTP endpoint, the panel in the window - talks to [`Agent`]
//! and never learns which format is which.

pub mod claude;
pub mod codex;

use std::io;
use std::path::PathBuf;

/// A coding agent this tool can be wired into.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Agent {
    ClaudeCode,
    Codex,
}

impl Agent {
    /// Every agent, in the order they are offered.
    pub const ALL: [Agent; 2] = [Agent::ClaudeCode, Agent::Codex];

    /// The name on the command line and on the wire.
    ///
    /// Separate from [`Agent::label`] because this one is a promise: it appears in URLs
    /// and in commands people type, so it may not be reworded the day the product decides
    /// to call something else by a nicer name.
    pub fn key(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Codex => "codex",
        }
    }

    /// The name a person calls it.
    pub fn label(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Codex => "Codex",
        }
    }

    pub fn from_key(key: &str) -> Option<Agent> {
        Agent::ALL.into_iter().find(|agent| agent.key() == key)
    }

    /// The lifecycle events this agent is wired for.
    ///
    /// Not the same list twice. Both agents say when a turn starts and stops, but they
    /// name the third case differently: Claude Code folds "waiting on you" into
    /// `Notification` with a type attached, and Codex gives it an event of its own.
    pub fn events(self) -> &'static [&'static str] {
        match self {
            Agent::ClaudeCode => &claude::EVENTS,
            Agent::Codex => &codex::EVENTS,
        }
    }

    /// Has this path been run against the real thing?
    ///
    /// Claude Code's has, repeatedly. Codex's is written from its published hook format
    /// and has never met a Codex install, which is a difference the person about to click
    /// the button is entitled to know about before they click it. It is a constant rather
    /// than something measured because only testing can change it, and testing happens
    /// here, not on their machine.
    pub fn proven(self) -> bool {
        match self {
            Agent::ClaudeCode => true,
            Agent::Codex => false,
        }
    }

    /// What the copy kept beside that file before each change is called.
    pub fn backup(self) -> &'static str {
        match self {
            Agent::ClaudeCode => claude::BACKUP,
            Agent::Codex => codex::BACKUP,
        }
    }

    /// The file the wiring goes in.
    pub fn settings_path(self) -> io::Result<PathBuf> {
        match self {
            Agent::ClaudeCode => claude::settings_path(),
            Agent::Codex => codex::settings_path(),
        }
    }
}

/// One MyTimeOff hook found in an agent's settings.
pub struct Wired {
    pub event: &'static str,
    /// Where it points. A URL for Claude Code, a command line for Codex.
    pub target: String,
    /// Whether it is wired the way this machine would wire it today.
    ///
    /// Worth its own field because the failure it catches is invisible otherwise: a hook
    /// carrying a token that has since been replaced, or naming an install that has since
    /// moved, looks exactly like a working one in the file and is turned away at the door
    /// every single time it fires.
    pub current: bool,
}

/// What one agent's settings say about MyTimeOff right now.
pub struct Status {
    pub path: PathBuf,
    /// Whether the agent's own config directory exists - the nearest thing to "is this
    /// agent installed" that can be answered without running anything.
    pub present: bool,
    pub wired: Vec<Wired>,
    /// Why the settings could not be read, if they could not.
    ///
    /// Carried rather than raised. A settings file somebody has broken is not a reason to
    /// refuse to draw the panel; it is the single most useful thing the panel could say.
    pub trouble: Option<String>,
}

/// What changed, so the caller can say so.
pub struct Outcome {
    pub path: PathBuf,
    /// Whether a file was already there - and therefore whether a backup was kept.
    pub replaced: bool,
    /// The name of that backup, beside the file.
    pub backup: &'static str,
}

impl Status {
    /// Every event wired, and all of them wired the way this machine would wire them.
    pub fn complete(&self, agent: Agent) -> bool {
        self.wired.len() == agent.events().len() && self.wired.iter().all(|one| one.current)
    }
}

pub fn status(agent: Agent, token: &str) -> io::Result<Status> {
    match agent {
        Agent::ClaudeCode => claude::status(token),
        Agent::Codex => codex::status(),
    }
}

/// Wires an agent to this daemon, replacing any wiring that was already there.
pub fn wire(agent: Agent, port: u16, token: &str) -> io::Result<Outcome> {
    match agent {
        Agent::ClaudeCode => claude::install(port, token),
        // No token: the shim reads it off disk when it runs, on the same machine that
        // wrote it. Codex is the agent that gets the better deal here - its config file
        // never holds the secret, because it never has to.
        Agent::Codex => codex::install(port),
    }
}

/// Takes the wiring back out, and says how many hooks there were.
pub fn unwire(agent: Agent) -> io::Result<usize> {
    match agent {
        Agent::ClaudeCode => claude::remove(),
        Agent::Codex => codex::remove(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_agent_has_a_key_that_round_trips() {
        for agent in Agent::ALL {
            assert_eq!(Agent::from_key(agent.key()), Some(agent), "{}", agent.label());
        }
    }

    #[test]
    fn keys_are_the_kind_of_word_that_survives_a_url() {
        for agent in Agent::ALL {
            let key = agent.key();
            assert!(
                key.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "{key} has to travel in a path segment and on a command line"
            );
        }
    }

    #[test]
    fn nothing_answers_to_a_name_no_agent_has() {
        assert_eq!(Agent::from_key("cursor"), None);
        assert_eq!(Agent::from_key(""), None);
        // The label is not the key, and must not be accepted as one - a caller that got
        // away with it once would break the day the label was reworded.
        assert_eq!(Agent::from_key("Claude Code"), None);
    }

    #[test]
    fn both_agents_are_wired_for_a_start_and_a_stop() {
        for agent in Agent::ALL {
            let events = agent.events();
            assert!(events.contains(&"UserPromptSubmit"), "{} cannot arm", agent.label());
            assert!(events.contains(&"Stop"), "{} cannot release", agent.label());
        }
    }

    #[test]
    fn a_status_with_nothing_in_it_is_not_complete() {
        let status = Status {
            path: PathBuf::from("settings.json"),
            present: true,
            wired: Vec::new(),
            trouble: None,
        };
        assert!(!status.complete(Agent::ClaudeCode));
    }

    #[test]
    fn one_stale_hook_is_enough_to_make_the_wiring_incomplete() {
        // The case this guards: every event wired, nothing missing, and every delivery
        // refused because the token moved. Counting alone would call that connected.
        let wired = Agent::ClaudeCode
            .events()
            .iter()
            .enumerate()
            .map(|(at, event)| Wired {
                event,
                target: "http://127.0.0.1:8787/hook".to_string(),
                current: at > 0,
            })
            .collect();
        let status = Status {
            path: PathBuf::from("settings.json"),
            present: true,
            wired,
            trouble: None,
        };
        assert!(!status.complete(Agent::ClaudeCode));
    }
}
