//! Turning real hook deliveries into [`Event`]s.
//!
//! Claude Code and Codex have converged on the same lifecycle vocabulary
//! (`UserPromptSubmit`, `Stop`, `PermissionRequest`, ...), so one parser serves both.
//! Codex's older `notify` mechanism uses a different, kebab-cased shape and is handled
//! separately in [`NotifyPayload`].

use serde::Deserialize;

use crate::machine::{Event, Millis, SessionId};

/// A lifecycle hook delivery, as JSON on stdin (command hooks) or as the POST body
/// (Claude Code's `type: "http"` hooks).
///
/// Only the fields that affect a decision are modelled; the rest of the payload
/// (`transcript_path`, `cwd`, `effort`, ...) is ignored rather than rejected, so a new
/// field in either agent cannot break the daemon.
#[derive(Debug, Clone, Deserialize)]
pub struct HookPayload {
    pub session_id: String,
    pub hook_event_name: String,
    /// Present only in subagents. A subagent's lifecycle must not move the screen.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// `Notification` only: `permission_prompt`, `idle_prompt`, `auth_success`,
    /// `elicitation_dialog`, ...
    #[serde(default)]
    pub notification_type: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

impl HookPayload {
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Maps a delivery onto an [`Event`], or `None` when it should be ignored.
    ///
    /// Ignoring is the common case and deliberately so: most hook traffic says nothing
    /// about whether the screen should move.
    pub fn to_event(&self, at: Millis) -> Option<Event> {
        // Subagent lifecycle is not the user's turn. A subagent finishing does not mean
        // the agent is ready for you, and acting on it would clear the screen early.
        if self.agent_id.is_some() {
            return None;
        }

        let session = SessionId(self.session_id.clone());
        match self.hook_event_name.as_str() {
            "UserPromptSubmit" => Some(Event::AgentStart { session, at }),
            // SessionEnd counts as done: whatever else is true, the turn is not running.
            "Stop" | "SessionEnd" => Some(Event::AgentDone { session, at }),
            // Codex names the blocked-on-you case explicitly.
            "PermissionRequest" => Some(Event::AgentNeedsInput { session, at }),
            "Notification" if self.blocks_on_user() => Some(Event::AgentNeedsInput { session, at }),
            _ => None,
        }
    }

    /// Whether a `Notification` actually means the agent is blocked waiting on you.
    ///
    /// This distinction matters more than it looks. `idle_prompt` fires because *you*
    /// have gone quiet — which, while you are reading, is exactly what is supposed to be
    /// happening. Treating every notification as a permission prompt would raise the
    /// loudest indicator precisely when the tool is working as intended.
    fn blocks_on_user(&self) -> bool {
        matches!(
            self.notification_type.as_deref(),
            Some("permission_prompt") | Some("elicitation_dialog")
        )
    }
}

/// Codex's `notify` payload, passed as the final argv argument rather than on stdin.
///
/// Its keys are kebab-cased, and it fires for exactly one event: `agent-turn-complete`.
/// There is no turn-*start* notification, so `notify` alone can never arm the reader —
/// it can only release it. Arming requires Codex's lifecycle hooks.
#[derive(Debug, Clone, Deserialize)]
pub struct NotifyPayload {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "thread-id")]
    pub thread_id: String,
    #[serde(rename = "turn-id", default)]
    pub turn_id: Option<String>,
    #[serde(rename = "last-assistant-message", default)]
    pub last_assistant_message: Option<String>,
}

impl NotifyPayload {
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn to_event(&self, at: Millis) -> Option<Event> {
        match self.kind.as_str() {
            "agent-turn-complete" => Some(Event::AgentDone {
                // The thread is the stable identity across turns, so it is what the
                // reading session binds to.
                session: SessionId(self.thread_id.clone()),
                at,
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: Millis = 1_000;

    /// Verbatim from the Claude Code hooks reference, common fields included, so a
    /// change in the real payload shape shows up here rather than at runtime.
    const USER_PROMPT_SUBMIT: &str = r#"{
        "session_id": "abc123",
        "prompt_id": "550e8400-e29b-41d4-a716-446655440000",
        "transcript_path": "/tmp/t.json",
        "cwd": "/home/user/project",
        "permission_mode": "default",
        "hook_event_name": "UserPromptSubmit",
        "prompt": "refactor the parser",
        "stop_hook_active": false
    }"#;

    const STOP: &str = r#"{
        "session_id": "abc123",
        "transcript_path": "/tmp/t.json",
        "cwd": "/home/user/project",
        "hook_event_name": "Stop",
        "last_assistant_message": "Done.",
        "stop_reason": "end_turn",
        "tool_use_ids": ["toolu_01ABC"]
    }"#;

    fn notification(kind: &str) -> String {
        format!(
            r#"{{
                "session_id": "abc123",
                "cwd": "/home/user/project",
                "hook_event_name": "Notification",
                "notification_type": "{kind}",
                "message": "Claude needs your permission to use Bash"
            }}"#
        )
    }

    #[test]
    fn user_prompt_submit_arms_the_session() {
        let payload = HookPayload::parse(USER_PROMPT_SUBMIT).expect("valid payload");
        assert_eq!(
            payload.to_event(AT),
            Some(Event::AgentStart { session: "abc123".into(), at: AT })
        );
    }

    #[test]
    fn stop_ends_the_turn() {
        let payload = HookPayload::parse(STOP).expect("valid payload");
        assert_eq!(
            payload.to_event(AT),
            Some(Event::AgentDone { session: "abc123".into(), at: AT })
        );
    }

    #[test]
    fn permission_prompt_blocks_on_the_user() {
        let payload = HookPayload::parse(&notification("permission_prompt")).unwrap();
        assert_eq!(
            payload.to_event(AT),
            Some(Event::AgentNeedsInput { session: "abc123".into(), at: AT })
        );
    }

    #[test]
    fn elicitation_dialog_also_blocks_on_the_user() {
        let payload = HookPayload::parse(&notification("elicitation_dialog")).unwrap();
        assert!(matches!(payload.to_event(AT), Some(Event::AgentNeedsInput { .. })));
    }

    #[test]
    fn idle_prompt_is_ignored_because_reading_is_meant_to_look_idle() {
        let payload = HookPayload::parse(&notification("idle_prompt")).unwrap();
        assert_eq!(payload.to_event(AT), None);
    }

    #[test]
    fn auth_success_is_ignored() {
        let payload = HookPayload::parse(&notification("auth_success")).unwrap();
        assert_eq!(payload.to_event(AT), None);
    }

    #[test]
    fn subagent_lifecycle_never_moves_the_screen() {
        let json = r#"{
            "session_id": "abc123",
            "hook_event_name": "Stop",
            "agent_id": "agent_01",
            "agent_type": "Explore"
        }"#;
        let payload = HookPayload::parse(json).expect("valid payload");
        assert_eq!(payload.to_event(AT), None);
    }

    #[test]
    fn codex_permission_request_blocks_on_the_user() {
        let json = r#"{
            "session_id": "thread-9",
            "hook_event_name": "PermissionRequest"
        }"#;
        let payload = HookPayload::parse(json).expect("valid payload");
        assert!(matches!(payload.to_event(AT), Some(Event::AgentNeedsInput { .. })));
    }

    #[test]
    fn unrelated_events_are_ignored() {
        for name in ["PreToolUse", "PostToolUse", "PreCompact", "SessionStart"] {
            let json = format!(r#"{{"session_id":"abc123","hook_event_name":"{name}"}}"#);
            let payload = HookPayload::parse(&json).expect("valid payload");
            assert_eq!(payload.to_event(AT), None, "{name} must not move the screen");
        }
    }

    #[test]
    fn unknown_fields_do_not_break_parsing() {
        let json = r#"{
            "session_id": "abc123",
            "hook_event_name": "Stop",
            "some_field_added_next_release": {"nested": true}
        }"#;
        assert!(HookPayload::parse(json).is_ok(), "must tolerate new fields");
    }

    #[test]
    fn codex_notify_uses_kebab_case_and_binds_to_the_thread() {
        let json = r#"{
            "type": "agent-turn-complete",
            "thread-id": "b5f6c1c2-1111-2222-3333-444455556666",
            "turn-id": "12345",
            "cwd": "/Users/example/project",
            "client": "codex-tui",
            "input-messages": ["Rename foo to bar."],
            "last-assistant-message": "Rename complete."
        }"#;
        let payload = NotifyPayload::parse(json).expect("valid payload");
        assert_eq!(
            payload.to_event(AT),
            Some(Event::AgentDone {
                session: "b5f6c1c2-1111-2222-3333-444455556666".into(),
                at: AT
            })
        );
    }
}
