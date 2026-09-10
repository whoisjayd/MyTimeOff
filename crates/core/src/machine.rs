/// Monotonic milliseconds, supplied by the caller so tests need no real clock.
pub type Millis = u64;

/// Identifies one agent session. Several agent windows may run at once, and only the
/// session that took the screen is allowed to give it back.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

impl From<&str> for SessionId {
    fn from(value: &str) -> Self {
        SessionId(value.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderMode {
    Strict,
    Lenient,
    Free,
}

impl ReaderMode {
    /// Whether leaving the reader has to pass a quiz. Free mode has no gate at all.
    pub fn gates_exit(self) -> bool {
        !matches!(self, ReaderMode::Free)
    }
}

/// How loudly the agent is asking for you.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    /// The turn finished. Come back whenever.
    Done,
    /// The agent is blocked waiting on you - worse than merely finished.
    NeedsInput,
}

/// Inputs to the machine: hook deliveries, the user, and the passage of time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A turn began (Claude Code `UserPromptSubmit`, Codex `notify`).
    AgentStart { session: SessionId, at: Millis },
    /// A turn ended (`Stop`).
    AgentDone { session: SessionId, at: Millis },
    /// The agent stalled on a permission prompt (`Notification`).
    AgentNeedsInput { session: SessionId, at: Millis },
    /// Time passed; the only thing that can expire the grace window.
    Tick { at: Millis },
    /// The user asked to leave the reader.
    ExitRequested { at: Millis },
    /// The quiz layer finished with the gate, however it ended.
    GateCleared { at: Millis },
}

impl Event {
    /// The session an event speaks for, if any.
    fn session(&self) -> Option<&SessionId> {
        match self {
            Event::AgentStart { session, .. }
            | Event::AgentDone { session, .. }
            | Event::AgentNeedsInput { session, .. } => Some(session),
            _ => None,
        }
    }
}

/// Outputs: what the daemon should actually do. Returning these rather than acting
/// keeps the rules verifiable and the side effects in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    ShowReader,
    HideReader,
    ShowIndicator(Alert),
    ClearIndicator,
    StartQuiz,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Idle,
    /// A turn is running but the screen has not been taken yet.
    Armed { session: SessionId, since: Millis },
    /// The reader is up and the turn is still running.
    Reading { session: SessionId, since: Millis },
    /// The turn ended; the indicator is showing and the reader is still up.
    Ready { session: SessionId, alert: Alert },
    /// The user asked to leave and the quiz is in progress.
    Gate { session: SessionId },
}

pub struct Machine {
    state: State,
    mode: ReaderMode,
    grace: Millis,
}

impl Machine {
    pub fn new(mode: ReaderMode, grace: Millis) -> Self {
        Machine { state: State::Idle, mode, grace }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn mode(&self) -> ReaderMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: ReaderMode) {
        self.mode = mode;
    }

    /// When the caller must next deliver an [`Event::Tick`], so the daemon can sleep
    /// until the grace window expires instead of polling.
    pub fn next_deadline(&self) -> Option<Millis> {
        match &self.state {
            State::Armed { since, .. } => Some(since + self.grace),
            _ => None,
        }
    }

    /// The session that currently owns the screen.
    pub fn bound_session(&self) -> Option<&SessionId> {
        match &self.state {
            State::Idle => None,
            State::Armed { session, .. }
            | State::Reading { session, .. }
            | State::Ready { session, .. }
            | State::Gate { session } => Some(session),
        }
    }

    pub fn handle(&mut self, event: Event) -> Vec<Command> {
        // Session binding: once a session owns the screen, no other session's hooks may
        // steer it. Without this, a second agent window finishing its turn would clear a
        // takeover it never triggered.
        if let (Some(bound), Some(incoming)) = (self.bound_session(), event.session())
            && bound != incoming
        {
            return Vec::new();
        }

        match (&self.state, event) {
            // A turn begins: arm, do not switch. Turns that finish inside the grace
            // window never take the screen, which is the whole reason this is bearable.
            (State::Idle, Event::AgentStart { session, at }) => {
                self.state = State::Armed { session, since: at };
                Vec::new()
            }

            // Grace expired with the turn still running: now take the screen.
            (State::Armed { session, since }, Event::Tick { at })
                if at.saturating_sub(*since) >= self.grace =>
            {
                self.state = State::Reading { session: session.clone(), since: at };
                vec![Command::ShowReader]
            }

            // Short turn: it finished before the grace window expired. Stand down.
            (State::Armed { .. }, Event::AgentDone { .. }) => {
                self.state = State::Idle;
                Vec::new()
            }

            // The agent needs you before we ever switched - so don't switch. Taking the
            // screen now would hide the very prompt that is waiting on you.
            (State::Armed { .. }, Event::AgentNeedsInput { .. }) => {
                self.state = State::Idle;
                Vec::new()
            }

            // Asking to leave before the screen was ever taken just disarms.
            (State::Armed { .. }, Event::ExitRequested { .. }) => {
                self.state = State::Idle;
                Vec::new()
            }

            (State::Reading { session, .. }, Event::AgentDone { .. }) => {
                self.state = State::Ready { session: session.clone(), alert: Alert::Done };
                vec![Command::ShowIndicator(Alert::Done)]
            }

            (State::Reading { session, .. }, Event::AgentNeedsInput { .. }) => {
                self.state =
                    State::Ready { session: session.clone(), alert: Alert::NeedsInput };
                vec![Command::ShowIndicator(Alert::NeedsInput)]
            }

            // A permission prompt outranks completion, so it may upgrade the indicator...
            (State::Ready { session, alert: Alert::Done }, Event::AgentNeedsInput { .. }) => {
                self.state =
                    State::Ready { session: session.clone(), alert: Alert::NeedsInput };
                vec![Command::ShowIndicator(Alert::NeedsInput)]
            }

            // ...but nothing may quietly downgrade it back to "just done".
            (State::Ready { alert: Alert::NeedsInput, .. }, Event::AgentDone { .. }) => {
                Vec::new()
            }

            // You read the indicator, went back to the terminal and set the agent going
            // again without leaving the reader. Straight back to reading, and drop the
            // indicator - it is answering a turn that is over.
            //
            // Deliberately no new grace window: grace exists to avoid *switching* for a
            // turn too short to be worth it, and that switch has already been paid for.
            // Re-arming here would strand you on the terminal for 20s with the reader
            // still up behind it.
            (State::Ready { session, .. }, Event::AgentStart { at, .. }) => {
                self.state = State::Reading { session: session.clone(), since: at };
                vec![Command::ClearIndicator]
            }

            // Already reading, and a turn is already running. A second prompt (queued, or
            // typed while reading) changes nothing.
            (State::Reading { .. }, Event::AgentStart { .. }) => Vec::new(),

            // Leaving while the turn is still running is allowed; the gate applies either
            // way, so an early exit cannot dodge the toll.
            (
                State::Reading { session, .. } | State::Ready { session, .. },
                Event::ExitRequested { .. },
            ) => {
                if self.mode.gates_exit() {
                    self.state = State::Gate { session: session.clone() };
                    vec![Command::StartQuiz]
                } else {
                    self.state = State::Idle;
                    vec![Command::ClearIndicator, Command::HideReader]
                }
            }

            // Pass or fail, the gate always releases; the retry-once rule is the quiz
            // layer's business, and this machine only sees that the gate is done.
            (State::Gate { .. }, Event::GateCleared { .. }) => {
                self.state = State::Idle;
                vec![Command::ClearIndicator, Command::HideReader]
            }

            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRACE: Millis = 20_000;

    fn machine() -> Machine {
        Machine::new(ReaderMode::Strict, GRACE)
    }

    fn start(m: &mut Machine, at: Millis) -> Vec<Command> {
        m.handle(Event::AgentStart { session: "s1".into(), at })
    }

    /// Drives a machine to the point where the reader is up.
    fn reading(m: &mut Machine) {
        start(m, 0);
        m.handle(Event::Tick { at: GRACE });
        assert!(matches!(m.state(), State::Reading { .. }));
    }

    /// Regression: found by a live agent, not by a test. The machine only armed from
    /// Idle, so once a turn finished and the indicator was up, every later prompt fell
    /// through to the catch-all and the machine wedged in Ready forever. Observed as 40
    /// consecutive samples of `ready` across an active turn.
    #[test]
    fn a_new_prompt_while_the_indicator_is_up_returns_to_reading() {
        let mut m = machine();
        reading(&mut m);
        m.handle(Event::AgentDone { session: "s1".into(), at: GRACE + 1 });
        assert!(matches!(m.state(), State::Ready { .. }));

        let cmds = start(&mut m, GRACE + 500);
        assert_eq!(cmds, vec![Command::ClearIndicator], "the indicator answers a finished turn");
        assert!(matches!(m.state(), State::Reading { .. }), "{:?}", m.state());
    }

    #[test]
    fn returning_to_reading_does_not_re_arm_a_grace_window() {
        let mut m = machine();
        reading(&mut m);
        m.handle(Event::AgentDone { session: "s1".into(), at: GRACE + 1 });
        start(&mut m, GRACE + 500);
        assert_eq!(
            m.next_deadline(),
            None,
            "the screen is already taken, so there is nothing left for grace to protect"
        );
    }

    #[test]
    fn a_loud_indicator_is_also_cleared_by_a_new_prompt() {
        let mut m = machine();
        reading(&mut m);
        m.handle(Event::AgentNeedsInput { session: "s1".into(), at: GRACE + 1 });
        assert!(matches!(m.state(), State::Ready { alert: Alert::NeedsInput, .. }));

        // Answering the prompt is exactly what a new turn means, so the alert is stale.
        let cmds = start(&mut m, GRACE + 500);
        assert_eq!(cmds, vec![Command::ClearIndicator]);
        assert!(matches!(m.state(), State::Reading { .. }));
    }

    #[test]
    fn a_second_prompt_while_reading_changes_nothing() {
        let mut m = machine();
        reading(&mut m);
        let before = m.state().clone();
        assert!(start(&mut m, GRACE + 500).is_empty());
        assert_eq!(m.state(), &before);
    }

    #[test]
    fn a_new_prompt_does_not_dodge_the_gate() {
        let mut m = machine();
        reading(&mut m);
        m.handle(Event::ExitRequested { at: GRACE + 1 });
        assert!(matches!(m.state(), State::Gate { .. }), "{:?}", m.state());

        assert!(start(&mut m, GRACE + 500).is_empty(), "the toll is already owed");
        assert!(matches!(m.state(), State::Gate { .. }));
    }

    #[test]
    fn arming_does_not_take_the_screen() {
        let mut m = machine();
        assert!(start(&mut m, 0).is_empty());
        assert!(matches!(m.state(), State::Armed { .. }));
    }

    #[test]
    fn short_turn_never_switches() {
        let mut m = machine();
        start(&mut m, 0);
        let cmds = m.handle(Event::AgentDone { session: "s1".into(), at: GRACE - 1 });
        assert!(cmds.is_empty(), "a short turn must not touch the screen");
        assert_eq!(m.state(), &State::Idle);
    }

    #[test]
    fn tick_before_grace_expires_does_nothing() {
        let mut m = machine();
        start(&mut m, 0);
        assert!(m.handle(Event::Tick { at: GRACE - 1 }).is_empty());
        assert!(matches!(m.state(), State::Armed { .. }));
    }

    #[test]
    fn long_turn_takes_the_screen_once_grace_expires() {
        let mut m = machine();
        start(&mut m, 0);
        assert_eq!(m.handle(Event::Tick { at: GRACE }), vec![Command::ShowReader]);
        assert!(matches!(m.state(), State::Reading { .. }));
    }

    #[test]
    fn deadline_is_reported_only_while_armed() {
        let mut m = machine();
        assert_eq!(m.next_deadline(), None);
        start(&mut m, 5_000);
        assert_eq!(m.next_deadline(), Some(5_000 + GRACE));
        m.handle(Event::Tick { at: 5_000 + GRACE });
        assert_eq!(m.next_deadline(), None);
    }

    #[test]
    fn another_session_cannot_clear_a_takeover_it_did_not_trigger() {
        let mut m = machine();
        reading(&mut m);
        let cmds = m.handle(Event::AgentDone { session: "other".into(), at: 30_000 });
        assert!(cmds.is_empty());
        assert!(matches!(m.state(), State::Reading { .. }));
    }

    #[test]
    fn another_session_cannot_arm_over_a_live_one() {
        let mut m = machine();
        start(&mut m, 0);
        m.handle(Event::AgentStart { session: "other".into(), at: 100 });
        match m.state() {
            State::Armed { session, since } => {
                assert_eq!(session, &SessionId::from("s1"));
                assert_eq!(*since, 0);
            }
            other => panic!("expected the first session to keep the arm, got {other:?}"),
        }
    }

    #[test]
    fn finished_turn_shows_the_indicator_and_keeps_the_reader_up() {
        let mut m = machine();
        reading(&mut m);
        let cmds = m.handle(Event::AgentDone { session: "s1".into(), at: 30_000 });
        assert_eq!(cmds, vec![Command::ShowIndicator(Alert::Done)]);
        assert!(matches!(m.state(), State::Ready { alert: Alert::Done, .. }));
    }

    #[test]
    fn permission_prompt_outranks_completion() {
        let mut m = machine();
        reading(&mut m);
        m.handle(Event::AgentDone { session: "s1".into(), at: 30_000 });
        let cmds = m.handle(Event::AgentNeedsInput { session: "s1".into(), at: 31_000 });
        assert_eq!(cmds, vec![Command::ShowIndicator(Alert::NeedsInput)]);
        assert!(matches!(m.state(), State::Ready { alert: Alert::NeedsInput, .. }));
    }

    #[test]
    fn completion_never_downgrades_a_permission_prompt() {
        let mut m = machine();
        reading(&mut m);
        m.handle(Event::AgentNeedsInput { session: "s1".into(), at: 30_000 });
        let cmds = m.handle(Event::AgentDone { session: "s1".into(), at: 31_000 });
        assert!(cmds.is_empty());
        assert!(matches!(m.state(), State::Ready { alert: Alert::NeedsInput, .. }));
    }

    #[test]
    fn a_prompt_during_grace_stands_down_instead_of_switching() {
        let mut m = machine();
        start(&mut m, 0);
        let cmds = m.handle(Event::AgentNeedsInput { session: "s1".into(), at: 1_000 });
        assert!(cmds.is_empty(), "must not hide the prompt that is waiting on you");
        assert_eq!(m.state(), &State::Idle);
    }

    #[test]
    fn exit_opens_the_gate_in_strict_mode() {
        let mut m = machine();
        reading(&mut m);
        m.handle(Event::AgentDone { session: "s1".into(), at: 30_000 });
        assert_eq!(m.handle(Event::ExitRequested { at: 40_000 }), vec![Command::StartQuiz]);
        assert!(matches!(m.state(), State::Gate { .. }));
    }

    #[test]
    fn leaving_early_still_faces_the_gate() {
        let mut m = machine();
        reading(&mut m);
        assert_eq!(m.handle(Event::ExitRequested { at: 25_000 }), vec![Command::StartQuiz]);
    }

    #[test]
    fn free_mode_releases_without_a_quiz() {
        let mut m = Machine::new(ReaderMode::Free, GRACE);
        reading(&mut m);
        assert_eq!(
            m.handle(Event::ExitRequested { at: 25_000 }),
            vec![Command::ClearIndicator, Command::HideReader]
        );
        assert_eq!(m.state(), &State::Idle);
    }

    #[test]
    fn the_gate_always_releases() {
        let mut m = machine();
        reading(&mut m);
        m.handle(Event::ExitRequested { at: 25_000 });
        assert_eq!(
            m.handle(Event::GateCleared { at: 60_000 }),
            vec![Command::ClearIndicator, Command::HideReader]
        );
        assert_eq!(m.state(), &State::Idle);
    }

    #[test]
    fn a_full_cycle_returns_to_idle() {
        let mut m = machine();
        start(&mut m, 0);
        m.handle(Event::Tick { at: GRACE });
        m.handle(Event::AgentDone { session: "s1".into(), at: 30_000 });
        m.handle(Event::ExitRequested { at: 40_000 });
        m.handle(Event::GateCleared { at: 50_000 });
        assert_eq!(m.state(), &State::Idle);
        assert_eq!(m.bound_session(), None);
        // The next turn must be able to arm again.
        start(&mut m, 60_000);
        assert!(matches!(m.state(), State::Armed { .. }));
    }
}
