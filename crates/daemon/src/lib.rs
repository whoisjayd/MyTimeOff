//! The MyTimeOff daemon: a loopback HTTP endpoint that turns hook deliveries into
//! decisions, and a timer that expires the grace window.
//!
//! All the rules live in `mytimeoff-core`. This crate only supplies the things the core
//! deliberately refuses to know about: a socket, a clock, and a token.

pub mod token;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{StatusCode, header::AUTHORIZATION};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use mytimeoff_core::{
    Alert, Command, Event, HookPayload, Machine, Millis, NotifyPayload, ReaderMode, State as TurnState,
};
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::Notify;

/// Shared daemon state.
pub struct Daemon {
    machine: Mutex<Machine>,
    /// Monotonic origin. The machine wants milliseconds, not wall-clock time, so a clock
    /// change (NTP, DST, a laptop waking up) cannot make the grace window misfire.
    started: Instant,
    token: String,
    /// Woken whenever the state changes, so the timer can recompute its deadline instead
    /// of polling.
    wake: Notify,
    /// Recently issued commands. A holding pen until the reader window subscribes; for
    /// now it is what makes behaviour observable from `GET /state`.
    issued: Mutex<Vec<Command>>,
    /// What actually arrived, in order, whether or not it changed anything.
    ///
    /// `issued` only shows deliveries the machine acted on, which makes a live wiring
    /// test unreadable: a hook that never arrives and a hook that arrives and is
    /// deliberately ignored look identical from outside.
    log: Mutex<Vec<String>>,
}

/// How many issued commands to keep. The daemon is meant to run for days, so this cannot
/// be an unbounded log - and only the recent tail is useful for seeing what just happened.
const ISSUED_HISTORY: usize = 32;

/// How many deliveries to remember. Longer than ISSUED_HISTORY because most deliveries
/// are correctly ignored and never reach it.
const LOG_HISTORY: usize = 64;

impl Daemon {
    pub fn new(mode: ReaderMode, grace: Duration, token: String) -> Arc<Self> {
        Arc::new(Daemon {
            machine: Mutex::new(Machine::new(mode, grace.as_millis() as Millis)),
            started: Instant::now(),
            token,
            wake: Notify::new(),
            issued: Mutex::new(Vec::new()),
            log: Mutex::new(Vec::new()),
        })
    }

    fn now(&self) -> Millis {
        self.started.elapsed().as_millis() as Millis
    }

    /// Records one delivery for `GET /state`.
    fn note(&self, entry: String) {
        let mut log = self.log.lock().expect("log lock");
        log.push(entry);
        if log.len() > LOG_HISTORY {
            let excess = log.len() - LOG_HISTORY;
            log.drain(..excess);
        }
    }

    /// Feeds one event to the machine and records what it decided.
    fn dispatch(&self, event: Event) -> Vec<Command> {
        let commands = {
            let mut machine = self.machine.lock().expect("machine lock");
            machine.handle(event)
        };

        if !commands.is_empty() {
            let mut issued = self.issued.lock().expect("issued lock");
            issued.extend(commands.iter().copied());
            if issued.len() > ISSUED_HISTORY {
                let excess = issued.len() - ISSUED_HISTORY;
                issued.drain(..excess);
            }
        }
        // Always wake the timer: even an ignored event may have been the one that would
        // have changed the deadline, and a spurious wake costs nothing.
        self.wake.notify_one();
        commands
    }

    /// Expires the grace window. Sleeps until the deadline rather than polling, and
    /// re-checks whenever the state changes underneath it.
    async fn run_timer(self: Arc<Self>) {
        loop {
            let deadline = self.machine.lock().expect("machine lock").next_deadline();
            match deadline {
                Some(deadline) => {
                    let now = self.now();
                    if deadline <= now {
                        self.dispatch(Event::Tick { at: now });
                    } else {
                        let wait = Duration::from_millis(deadline - now);
                        tokio::select! {
                            _ = tokio::time::sleep(wait) => {}
                            _ = self.wake.notified() => {}
                        }
                    }
                }
                // Nothing is armed, so no deadline exists to wait for.
                None => self.wake.notified().await,
            }
        }
    }
}

#[derive(Serialize)]
pub struct StateReport {
    state: &'static str,
    session: Option<String>,
    mode: &'static str,
    alert: Option<&'static str>,
    issued: Vec<&'static str>,
    received: Vec<String>,
}

fn describe_event(event: &Event) -> &'static str {
    match event {
        Event::AgentStart { .. } => "agent_start",
        Event::AgentDone { .. } => "agent_done",
        Event::AgentNeedsInput { .. } => "agent_needs_input",
        Event::Tick { .. } => "tick",
        Event::ExitRequested { .. } => "exit_requested",
        Event::GateCleared { .. } => "gate_cleared",
    }
}

fn describe_command(command: Command) -> &'static str {
    match command {
        Command::ShowReader => "show_reader",
        Command::HideReader => "hide_reader",
        Command::ShowIndicator(Alert::Done) => "indicator_done",
        Command::ShowIndicator(Alert::NeedsInput) => "indicator_needs_input",
        Command::ClearIndicator => "clear_indicator",
        Command::StartQuiz => "start_quiz",
    }
}

async fn state(State(daemon): State<Arc<Daemon>>) -> Json<StateReport> {
    let machine = daemon.machine.lock().expect("machine lock");
    let (name, alert) = match machine.state() {
        TurnState::Idle => ("idle", None),
        TurnState::Armed { .. } => ("armed", None),
        TurnState::Reading { .. } => ("reading", None),
        TurnState::Ready { alert: Alert::Done, .. } => ("ready", Some("done")),
        TurnState::Ready { alert: Alert::NeedsInput, .. } => ("ready", Some("needs_input")),
        TurnState::Gate { .. } => ("gate", None),
    };
    let mode = match machine.mode() {
        ReaderMode::Strict => "strict",
        ReaderMode::Lenient => "lenient",
        ReaderMode::Free => "free",
    };
    let session = machine.bound_session().map(|s| s.0.clone());
    drop(machine);

    let issued = daemon
        .issued
        .lock()
        .expect("issued lock")
        .iter()
        .map(|c| describe_command(*c))
        .collect();

    let received = daemon.log.lock().expect("log lock").clone();

    Json(StateReport { state: name, session, mode, alert, issued, received })
}

/// Claude Code `type: "http"` hooks POST here, as does the Codex shim.
async fn hook(State(daemon): State<Arc<Daemon>>, body: String) -> Response {
    let payload = match HookPayload::parse(&body) {
        Ok(payload) => payload,
        // A malformed delivery is the agent's problem, not a reason to take the screen.
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };

    let now = daemon.now();
    let mapped = payload.to_event(now);
    daemon.note(format!(
        "{now} {} {} -> {}",
        payload.hook_event_name,
        payload.notification_type.as_deref().unwrap_or("-"),
        mapped.as_ref().map_or("ignored", describe_event),
    ));
    if let Some(event) = mapped {
        daemon.dispatch(event);
    }
    // Hooks read our response for permission decisions we never make, so say nothing.
    StatusCode::NO_CONTENT.into_response()
}

/// Codex `notify` deliveries, forwarded by the shim.
async fn notify(State(daemon): State<Arc<Daemon>>, body: String) -> Response {
    let payload = match NotifyPayload::parse(&body) {
        Ok(payload) => payload,
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };

    let now = daemon.now();
    let mapped = payload.to_event(now);
    daemon.note(format!(
        "{now} notify:{} -> {}",
        payload.kind,
        mapped.as_ref().map_or("ignored", describe_event),
    ));
    if let Some(event) = mapped {
        daemon.dispatch(event);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// The user asked to leave the reader.
async fn exit_requested(State(daemon): State<Arc<Daemon>>) -> Json<Vec<&'static str>> {
    let commands = daemon.dispatch(Event::ExitRequested { at: daemon.now() });
    Json(commands.into_iter().map(describe_command).collect())
}

/// The quiz layer is done with the gate, however it ended.
async fn gate_cleared(State(daemon): State<Arc<Daemon>>) -> Json<Vec<&'static str>> {
    let commands = daemon.dispatch(Event::GateCleared { at: daemon.now() });
    Json(commands.into_iter().map(describe_command).collect())
}

async fn require_token(
    State(daemon): State<Arc<Daemon>>,
    request: Request,
    next: middleware::Next,
) -> Result<Response, StatusCode> {
    let presented = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));

    match presented {
        Some(candidate)
            if token::constant_time_eq(candidate.as_bytes(), daemon.token.as_bytes()) =>
        {
            Ok(next.run(request).await)
        }
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

pub fn router(daemon: Arc<Daemon>) -> Router {
    Router::new()
        .route("/hook", post(hook))
        .route("/notify", post(notify))
        .route("/exit", post(exit_requested))
        .route("/gate/cleared", post(gate_cleared))
        .route("/state", get(state))
        // Applied last so it wraps every route above; a new route cannot be added
        // without inheriting authentication.
        .layer(middleware::from_fn_with_state(daemon.clone(), require_token))
        .with_state(daemon)
}

/// Binds the loopback listener. Port 0 asks the OS for a free port, which is what tests
/// use.
pub async fn bind(port: u16) -> std::io::Result<TcpListener> {
    // Loopback only, never 0.0.0.0: this endpoint must not be reachable from the network.
    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await
}

/// Serves until the process ends.
pub async fn serve(listener: TcpListener, daemon: Arc<Daemon>) -> std::io::Result<()> {
    tokio::spawn(daemon.clone().run_timer());
    axum::serve(listener, router(daemon)).await
}
