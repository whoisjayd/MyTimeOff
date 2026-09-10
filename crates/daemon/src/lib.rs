//! The MyTimeOff daemon: a loopback HTTP endpoint that turns hook deliveries into
//! decisions, and a timer that expires the grace window.
//!
//! All the rules live in `mytimeoff-core`. This crate only supplies the things the core
//! deliberately refuses to know about: a socket, a clock, and a token.

pub mod paths;
pub mod quiz;
pub mod secret;
pub mod settings;
pub mod store;
pub mod token;

use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, Request, State};
use axum::http::{StatusCode, header::AUTHORIZATION};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use mytimeoff_core::{
    Alert, AskedQuestion, Book, BookFormat, Command, Config, Event, Gate, HookPayload, Machine,
    Millis, NotifyPayload, PageView, Policy, Quiz, ReaderMode, Release, State as TurnState,
    Submission, Verdict,
};
use serde::{Deserialize, Serialize};

use crate::quiz::QuestionSource;
use crate::store::Store;
use tokio::net::TcpListener;
use tokio::sync::{Notify, broadcast};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

/// Shared daemon state.
pub struct Daemon {
    machine: Mutex<Machine>,
    config: Config,
    store: Store,
    /// Where questions come from. Behind a trait object rather than a type parameter so
    /// that swapping the offline stub for a real generator changes one line in `main`.
    questions: Arc<dyn QuestionSource>,
    /// The gate currently being taken, if any. Separate from the state machine because
    /// the machine deliberately knows nothing about quizzes - only that a gate is open.
    gate: Mutex<Option<Gate>>,
    /// Wall-clock milliseconds when the reader was last put on screen.
    ///
    /// Wall-clock rather than the monotonic clock the machine uses, because it is compared
    /// against page views in the store, and those are stamped in calendar time so that
    /// "which day did you read" has an answer.
    reading_since: Mutex<Option<i64>>,
    /// Makes each gate's quiz id unique, so an answer sheet cannot be marked against a
    /// later quiz. Seeded from the wall clock so ids do not repeat across restarts.
    gates_opened: AtomicU64,
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
    /// Fans commands out to every connected surface. The reader subscribes here; a TUI
    /// or a tray icon can subscribe to the same stream without the daemon knowing.
    commands: broadcast::Sender<Command>,
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

/// Buffered commands per subscriber. A surface further behind than this has stopped
/// keeping up entirely, and the resynchronise-on-lag path is what recovers it.
const COMMAND_BUFFER: usize = 32;

/// The largest book the daemon will keep.
///
/// A limit has to exist because the body is held in memory on the way in - by the shell's
/// bridge, by axum, and by SQLite - so a mistaken POST of something enormous would be
/// three copies of it. This is far past any real EPUB or PDF and far short of trouble.
const MAX_BOOK_BYTES: usize = 128 * 1024 * 1024;

/// Pages a gate may draw on. More than it will ask about, because a page can be a chapter
/// heading with nothing to ask, and because the wrong answers are drawn from the other
/// pages you read - too few and the choices stop being plausible.
const GATE_PAGES: u32 = 10;

impl Daemon {
    pub fn new(
        config: Config,
        store: Store,
        token: String,
        questions: Arc<dyn QuestionSource>,
    ) -> Arc<Self> {
        Arc::new(Daemon {
            machine: Mutex::new(Machine::new(config.mode, config.grace_ms)),
            config,
            store,
            questions,
            gate: Mutex::new(None),
            reading_since: Mutex::new(None),
            gates_opened: AtomicU64::new(wall_clock_ms() as u64),
            started: Instant::now(),
            token,
            wake: Notify::new(),
            issued: Mutex::new(Vec::new()),
            log: Mutex::new(Vec::new()),
            commands: broadcast::channel(COMMAND_BUFFER).0,
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

        for command in &commands {
            match command {
                // The gate asks about what you read in this stretch, so the stretch starts
                // here - at the moment the reader actually took the screen.
                Command::ShowReader => {
                    *self.reading_since.lock().expect("reading lock") = Some(wall_clock_ms());
                }
                // A new gate never inherits the last one's quiz or its spent retries.
                Command::StartQuiz => *self.gate.lock().expect("gate lock") = None,
                _ => {}
            }
        }

        if !commands.is_empty() {
            let mut issued = self.issued.lock().expect("issued lock");
            issued.extend(commands.iter().copied());
            if issued.len() > ISSUED_HISTORY {
                let excess = issued.len() - ISSUED_HISTORY;
                issued.drain(..excess);
            }
        }
        for command in &commands {
            // Errors mean nothing is listening yet, which is normal: the daemon runs
            // whether or not a reader is open.
            let _ = self.commands.send(*command);
        }
        // Always wake the timer: even an ignored event may have been the one that would
        // have changed the deadline, and a spurious wake costs nothing.
        self.wake.notify_one();
        commands
    }

    /// The commands a surface needs in order to match the current state from scratch.
    ///
    /// A reader that opens - or reconnects after a dropped connection - mid-takeover has
    /// missed every command already sent, and a broadcast channel replays nothing. Without
    /// this it would sit blank through a takeover that is already in progress.
    fn resync(&self) -> Vec<Command> {
        let machine = self.machine.lock().expect("machine lock");
        match machine.state() {
            // Armed has not taken the screen yet, so it looks the same as idle from here.
            TurnState::Idle | TurnState::Armed { .. } => vec![Command::HideReader],
            TurnState::Reading { .. } => vec![Command::ShowReader],
            TurnState::Ready { alert, .. } => {
                vec![Command::ShowReader, Command::ShowIndicator(*alert)]
            }
            TurnState::Gate { .. } => vec![Command::ShowReader, Command::StartQuiz],
        }
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

fn sse(command: Command) -> Result<SseEvent, Infallible> {
    Ok(SseEvent::default().data(describe_command(command)))
}

/// Streams commands to a reader surface.
///
/// This is the half that was missing: until now the machine decided to show the reader
/// and nothing was listening.
async fn events(
    State(daemon): State<Arc<Daemon>>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    // Subscribe *before* reading the state, so a command issued between the two is queued
    // rather than lost. The cost is a possible duplicate, and every command here is
    // idempotent - which is the safe direction to err in.
    let rx = daemon.commands.subscribe();
    let initial = daemon.resync();

    let live = BroadcastStream::new(rx).filter_map(|received| match received {
        Ok(command) => Some(sse(command)),
        // This surface fell more than COMMAND_BUFFER behind. Replaying the individual
        // commands it missed is not worth it: reconnecting resyncs from the state, which
        // is the truth anyway.
        Err(_) => None,
    });

    Sse::new(tokio_stream::iter(initial).map(sse).chain(live)).keep_alive(KeepAlive::default())
}

/// The user asked to leave the reader.
async fn exit_requested(State(daemon): State<Arc<Daemon>>) -> Json<Vec<&'static str>> {
    let commands = daemon.dispatch(Event::ExitRequested { at: daemon.now() });
    Json(commands.into_iter().map(describe_command).collect())
}

/// The reader telling the daemon what it opened.
///
/// A page view can only be recorded against a book the store knows, so this has to happen
/// before any reading is reported. That is deliberate: without it a typo in a book id
/// would quietly become a second, untitled book, and a day's progress would split in two.
async fn book(State(daemon): State<Arc<Daemon>>, Json(book): Json<Book>) -> Response {
    match daemon.store.record_book(&book) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => store_error(error),
    }
}

/// One page, read.
///
/// Whether it counted is decided here rather than by the reader. The surface measures
/// dwell - only it knows about focus and visibility - but if it also classified, a window
/// and a TUI could disagree about the same day.
async fn page_view(State(daemon): State<Arc<Daemon>>, Json(view): Json<PageView>) -> Response {
    let counted = view.counts(daemon.config.skim_threshold_ms);
    match daemon.store.record_page_view(&view, counted) {
        Ok(stored) => Json(PageViewReceipt { counted, stored }).into_response(),
        Err(error) => store_error(error),
    }
}

/// The book to reopen, and where it was left. 204 on a first run.
///
/// This is the endpoint that answers "do I have to choose a book again": a reader asks it
/// before drawing anything, and only falls back to the picker when the answer is nothing.
async fn library(State(daemon): State<Arc<Daemon>>) -> Response {
    match daemon.store.resume() {
        Ok(Some(resume)) => Json(resume).into_response(),
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => store_error(error),
    }
}

/// The open book's bytes, so a surface that cannot open a path can still open the book.
async fn library_file(State(daemon): State<Arc<Daemon>>) -> Response {
    match daemon.store.book_bytes() {
        Ok(Some((format, bytes))) => {
            ([(axum::http::header::CONTENT_TYPE, media_type(format))], bytes).into_response()
        }
        // A registered book whose bytes were never kept. Not an error - a surface that
        // reads off disk never uploads - but nothing to hand back either.
        Ok(None) => (StatusCode::NOT_FOUND, "no book is kept").into_response(),
        Err(error) => store_error(error),
    }
}

/// Hands the daemon the bytes of the book that was just registered.
///
/// The id travels in the query rather than being inferred from "whatever is open", so a
/// second book opened between the two requests is a refusal instead of a mix-up.
async fn library_upload(
    State(daemon): State<Arc<Daemon>>,
    Query(which): Query<BookId>,
    bytes: Bytes,
) -> Response {
    if bytes.is_empty() {
        return (StatusCode::BAD_REQUEST, "no bytes").into_response();
    }
    match daemon.store.keep_book_bytes(&which.id, &bytes) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => {
            (StatusCode::CONFLICT, "that book is not the one that is open").into_response()
        }
        Err(error) => store_error(error),
    }
}

/// Which book the bytes belong to.
#[derive(Deserialize)]
struct BookId {
    id: String,
}

fn media_type(format: BookFormat) -> &'static str {
    match format {
        BookFormat::Epub => "application/epub+zip",
        BookFormat::Pdf => "application/pdf",
    }
}

/// How the day is going.
async fn progress(State(daemon): State<Arc<Daemon>>) -> Response {
    match daemon.store.pages_read_today() {
        Ok(pages_today) => Json(Progress {
            pages_today,
            goal: daemon.config.daily_page_goal,
            met: pages_today >= daemon.config.daily_page_goal,
        })
        .into_response(),
        Err(error) => store_error(error),
    }
}

/// The gate as a surface sees it: the questions, the rules, and how many tries are left.
#[derive(Serialize)]
#[serde(tag = "gate", rename_all = "snake_case")]
enum GateView {
    Open { quiz_id: String, questions: Vec<AskedQuestion>, policy: Policy, attempts_left: u32 },
    /// Nothing could be asked, so nothing is owed. See [`Release::Ungated`].
    Released { reason: Release },
}

impl Daemon {
    /// Builds the quiz for the open gate, or returns the one already being taken.
    ///
    /// Generation happens here rather than when the gate opens, so a slow or absent
    /// generator delays only the questions and never the state change: the reader is on
    /// screen either way.
    async fn open_gate(self: &Arc<Self>) -> Option<GateView> {
        let mode = self.gated_mode()?;

        // A reader that reconnected mid-gate gets the quiz it was already taking, spent
        // retries and all. Regenerating would hand out a fresh allowance.
        if let Some(view) = self.gate_view() {
            return Some(view);
        }

        let quiz = self.generate(mode).await;
        let mut held = self.gate.lock().expect("gate lock");
        // Two surfaces can ask at once; the first to finish generating wins, so the second
        // does not replace a quiz that is already on someone's screen.
        let gate = held.get_or_insert_with(|| Gate::open(mode, quiz));
        Some(view_of(gate))
    }

    /// The mode a gate is currently owed under, if one is.
    fn gated_mode(&self) -> Option<ReaderMode> {
        let machine = self.machine.lock().expect("machine lock");
        matches!(machine.state(), TurnState::Gate { .. }).then(|| machine.mode())
    }

    fn gate_view(&self) -> Option<GateView> {
        self.gate.lock().expect("gate lock").as_ref().map(view_of)
    }

    /// Draws a quiz from the pages read since the reader took the screen.
    ///
    /// Every failure path here returns an empty quiz rather than an error. A generator
    /// that is down, a database that will not read, a stretch with nothing counted: none
    /// of those are the user's fault, and none of them may become a lock on their screen.
    async fn generate(self: &Arc<Self>, mode: ReaderMode) -> Quiz {
        let mut quiz = Quiz {
            id: format!("gate-{}", self.gates_opened.fetch_add(1, Ordering::Relaxed)),
            book_id: String::new(),
            questions: Vec::new(),
            generated_at: wall_clock_ms(),
            pre_generated: false,
        };

        let wanted = self.config.questions_per_gate as usize;
        if !Policy::for_mode(mode).quiz || wanted == 0 {
            return quiz;
        }

        let since = self.reading_since.lock().expect("reading lock").unwrap_or(0);
        let book_id = match self.store.latest_book_since(since) {
            Ok(Some(book_id)) => book_id,
            Ok(None) => {
                self.note("gate: nothing was read in this stretch".to_string());
                return quiz;
            }
            Err(error) => {
                self.note(format!("gate: could not read history: {error}"));
                return quiz;
            }
        };
        quiz.book_id = book_id.clone();

        let pages = match self.store.counted_pages_since(&book_id, since, GATE_PAGES) {
            Ok(pages) => pages,
            Err(error) => {
                self.note(format!("gate: could not read pages: {error}"));
                return quiz;
            }
        };

        match self.questions.questions(&pages, wanted).await {
            Ok(questions) => quiz.questions = questions,
            Err(error) => self.note(format!("gate: {error}")),
        }
        quiz
    }

    /// Applies a verdict: a release ends the gate and gives the screen back.
    fn settle(self: &Arc<Self>, verdict: Verdict) -> Verdict {
        if matches!(verdict, Verdict::Released { .. }) {
            *self.gate.lock().expect("gate lock") = None;
            self.dispatch(Event::GateCleared { at: self.now() });
        }
        verdict
    }
}

fn view_of(gate: &Gate) -> GateView {
    // Nothing to ask means nothing to answer, and a gate that cannot be answered is a gate
    // that cannot be opened. Say so plainly rather than showing an empty quiz.
    if gate.quiz().questions.is_empty() {
        return GateView::Released { reason: Release::Ungated };
    }
    GateView::Open {
        quiz_id: gate.quiz().id.clone(),
        questions: gate.quiz().for_display(),
        policy: gate.policy(),
        attempts_left: gate.attempts_left(),
    }
}

/// The quiz for the gate that is currently open.
async fn gate(State(daemon): State<Arc<Daemon>>) -> Response {
    match daemon.open_gate().await {
        Some(GateView::Released { reason }) => {
            // Release on the way out, so a reader that only ever reads this endpoint still
            // gets its screen back.
            daemon.settle(Verdict::Released { reason, score: None });
            Json(GateView::Released { reason }).into_response()
        }
        Some(view) => Json(view).into_response(),
        None => (StatusCode::CONFLICT, "no gate is open").into_response(),
    }
}

/// An answer sheet.
async fn gate_answers(
    State(daemon): State<Arc<Daemon>>,
    Json(submission): Json<Submission>,
) -> Response {
    let verdict = {
        let mut held = daemon.gate.lock().expect("gate lock");
        match held.as_mut() {
            Some(gate) => gate.submit(&submission),
            None => return (StatusCode::CONFLICT, "no gate is open").into_response(),
        }
    };
    Json(daemon.settle(verdict)).into_response()
}

/// "I would rather not." Whether that works is the mode's answer, not the button's.
async fn gate_skip(State(daemon): State<Arc<Daemon>>) -> Response {
    // Answered from the mode's policy rather than from the gate, so that pressing skip
    // before the questions have arrived gets the same answer as pressing it after - and so
    // that strict mode's refusal never waits on a generator.
    let Some(mode) = daemon.gated_mode() else {
        return (StatusCode::CONFLICT, "no gate is open").into_response();
    };
    Json(daemon.settle(Policy::for_mode(mode).skip())).into_response()
}

/// Wall-clock milliseconds. Only for things measured against a calendar; the grace window
/// uses the monotonic clock instead, so that a clock change cannot misfire it.
fn wall_clock_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as i64)
}

/// Separates "you sent something impossible" from "the database is broken".
///
/// A page view for an unregistered book is the caller's mistake and recoverable by
/// registering it; anything else is ours, and saying 500 is the honest answer.
fn store_error(error: rusqlite::Error) -> Response {
    let violated_a_constraint = matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::ConstraintViolation)
    );
    if violated_a_constraint {
        (StatusCode::CONFLICT, "unknown book: register it with POST /book first").into_response()
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response()
    }
}

/// What the daemon did with a reported page view.
///
/// `stored` is false for a retry of a view already recorded, which is not an error and
/// should not make the reader try again.
#[derive(Serialize)]
struct PageViewReceipt {
    counted: bool,
    stored: bool,
}

#[derive(Serialize)]
struct Progress {
    pages_today: u32,
    goal: u32,
    met: bool,
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
        .route("/gate", get(gate))
        .route("/gate/answers", post(gate_answers))
        .route("/gate/skip", post(gate_skip))
        .route("/state", get(state))
        .route("/events", get(events))
        .route("/book", post(book))
        .route("/page-view", post(page_view))
        .route("/progress", get(progress))
        // The one route that carries a whole book, and so the one that opts out of axum's
        // 2MB default. Raised here rather than globally: every other endpoint takes a
        // small JSON body and has no business accepting more.
        .route(
            "/library",
            get(library).post(library_upload).layer(DefaultBodyLimit::max(MAX_BOOK_BYTES)),
        )
        .route("/library/file", get(library_file))
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
