//! End-to-end tests against a real listener.
//!
//! These deliberately speak HTTP over a socket rather than calling handlers directly:
//! the things most likely to be wrong - the auth layer covering every route, loopback
//! binding, JSON arriving as a body - only exist at that level.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mytimeoff_core::{Config, Question, ReaderMode};
use mytimeoff_daemon::quiz::stub::Stub;
use mytimeoff_daemon::quiz::{Page, QuestionSource, Questions};
use mytimeoff_daemon::store::Store;
use mytimeoff_daemon::{Daemon, bind, serve};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

const TOKEN: &str = "test-token-0123456789";
/// Short enough to expire inside a test, long enough not to race the request.
const GRACE: Duration = Duration::from_millis(80);

/// Starts a daemon on an OS-assigned port.
async fn start(mode: ReaderMode) -> SocketAddr {
    start_with(mode, Arc::new(Stub)).await
}

async fn start_with(mode: ReaderMode, questions: Arc<dyn QuestionSource>) -> SocketAddr {
    let listener = bind(0).await.expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    // In memory, so each test gets its own empty history and none of them touch the
    // database the daemon on this machine is actually using.
    let store = Store::in_memory().expect("store");
    let config = Config { mode, grace_ms: GRACE.as_millis() as u64, ..Config::default() };
    let daemon = Daemon::new(config, store, TOKEN.to_string(), questions);
    tokio::spawn(async move { serve(listener, daemon).await });
    addr
}

/// A question source with a known answer key, so a test can actually pass a quiz.
///
/// This is what the trait is for. Without it, marking could only be tested where the
/// right answer does not matter - and "a correct answer opens the gate" is the one
/// behaviour this whole tool rests on.
struct Rigged;

impl QuestionSource for Rigged {
    fn questions<'a>(&'a self, pages: &'a [Page], wanted: usize) -> Questions<'a> {
        Box::pin(async move {
            Ok(pages
                .iter()
                .take(wanted)
                .enumerate()
                .map(|(index, page)| Question {
                    id: format!("q{index}"),
                    prompt: format!("What was on page {}?", page.locator.page_label()),
                    // The first choice is always the right one, which is exactly why this
                    // must never be anything but a test double.
                    choices: vec!["right".into(), "wrong".into()],
                    answer_index: 0,
                    source: page.locator.clone(),
                })
                .collect())
        })
    }
}

/// Registers a book and reports `pages` pages of it as properly read.
///
/// Real prose, because the offline stub draws its questions out of the text and a page of
/// lorem would have nothing distinctive to ask about.
async fn read_pages(addr: SocketAddr, pages: u32) {
    let book = r#"{"id":"book-1","format":"pdf","title":"A Book","path":null,
                   "author":null,"total_pages":40}"#;
    let (status, body) = post(addr, "/book", book).await;
    assert_eq!(status, 204, "{body}");

    let lines = [
        "The cartographer folded the enormous chart against the freezing wind.",
        "Her brother inherited the observatory and its broken telescope.",
        "Every harbour on that coastline remembered the shipwreck differently.",
        "The librarian catalogued each pamphlet before the building was demolished.",
    ];
    for page in 1..=pages {
        let entered = wall_clock_ms() + i64::from(page);
        let text = lines[(page as usize - 1) % lines.len()];
        let view = format!(
            r#"{{"book_id":"book-1","locator":{{"kind":"page","page":{page},
                "page_label":"{page}"}},"text":"{text}","entered_at":{entered},
                "exited_at":{exited},"dwell_ms":9000}}"#,
            exited = entered + 9_000,
        );
        let (status, body) = post(addr, "/page-view", &view).await;
        assert_eq!(status, 200, "{body}");
        assert!(body.contains(r#""counted":true"#), "{body}");
    }
}

fn wall_clock_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_millis() as i64
}

/// Drives a daemon to the point where the gate is open and pages have been read.
async fn at_the_gate(addr: SocketAddr, pages: u32) {
    post(addr, "/hook", &hook_body("UserPromptSubmit", "s1")).await;
    past_grace().await;
    read_pages(addr, pages).await;
    let (_, body) = post(addr, "/exit", "").await;
    assert!(body.contains("start_quiz"), "the gate should be open: {body}");
}

async fn get(addr: SocketAddr, path: &str) -> (u16, String) {
    request(addr, "GET", path, Some(TOKEN), "").await
}

/// A minimal HTTP/1.1 client: enough to prove the endpoint works, with no dependency.
async fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.expect("connect");

    let auth = match token {
        Some(token) => format!("Authorization: Bearer {token}\r\n"),
        None => String::new(),
    };
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {len}\r\n{auth}\r\n{body}",
        len = body.len()
    );
    stream.write_all(request.as_bytes()).await.expect("write request");

    let mut raw = String::new();
    stream.read_to_string(&mut raw).await.expect("read response");

    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status line in response: {raw:?}"));
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    (status, body)
}

async fn post(addr: SocketAddr, path: &str, body: &str) -> (u16, String) {
    request(addr, "POST", path, Some(TOKEN), body).await
}

async fn state(addr: SocketAddr) -> String {
    let (status, body) = request(addr, "GET", "/state", Some(TOKEN), "").await;
    assert_eq!(status, 200, "state should be readable: {body}");
    body
}

/// Opens an SSE connection and leaves it open, so a test can watch commands arrive on it.
async fn subscribe(addr: SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let request = [
        "GET /events HTTP/1.1",
        "Host: localhost",
        "Accept: text/event-stream",
        &format!("Authorization: Bearer {TOKEN}"),
        "",
        "",
    ]
    .join("\r\n");
    stream.write_all(request.as_bytes()).await.expect("write request");
    stream
}

/// Reads whatever a stream has produced, stopping once it goes quiet.
///
/// An event stream never ends, so `read_to_string` would hang forever: going quiet is
/// the only end a test can wait for.
async fn drain(stream: &mut TcpStream) -> String {
    let mut seen = String::new();
    let mut buf = [0u8; 1024];
    while let Ok(Ok(read)) = timeout(Duration::from_millis(150), stream.read(&mut buf)).await {
        if read == 0 {
            break;
        }
        seen.push_str(&String::from_utf8_lossy(&buf[..read]));
    }
    seen
}

fn hook_body(event: &str, session: &str) -> String {
    format!(r#"{{"session_id":"{session}","hook_event_name":"{event}"}}"#)
}

/// Waits out the grace window with margin, so the timer has actually fired.
async fn past_grace() {
    tokio::time::sleep(GRACE + Duration::from_millis(120)).await;
}

#[tokio::test]
async fn requests_without_a_token_are_rejected() {
    let addr = start(ReaderMode::Strict).await;
    let (status, _) = request(addr, "GET", "/state", None, "").await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn requests_with_the_wrong_token_are_rejected() {
    let addr = start(ReaderMode::Strict).await;
    let (status, _) = request(addr, "GET", "/state", Some("not-the-token"), "").await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn every_route_is_behind_the_token() {
    let addr = start(ReaderMode::Strict).await;
    for (method, path) in [
        ("POST", "/hook"),
        ("POST", "/notify"),
        ("POST", "/exit"),
        ("GET", "/gate"),
        ("POST", "/gate/answers"),
        ("POST", "/gate/skip"),
        ("GET", "/state"),
        ("GET", "/events"),
        ("POST", "/book"),
        ("POST", "/page-view"),
        ("GET", "/progress"),
    ] {
        let (status, _) = request(addr, method, path, None, "{}").await;
        assert_eq!(status, 401, "{method} {path} must require the token");
    }
}

#[tokio::test]
async fn a_prompt_arms_without_taking_the_screen() {
    let addr = start(ReaderMode::Strict).await;
    let (status, _) = post(addr, "/hook", &hook_body("UserPromptSubmit", "s1")).await;
    assert_eq!(status, 204);

    let report = state(addr).await;
    assert!(report.contains(r#""state":"armed""#), "{report}");
    assert!(report.contains(r#""issued":[]"#), "nothing should have happened yet: {report}");
}

#[tokio::test]
async fn a_short_turn_never_takes_the_screen() {
    let addr = start(ReaderMode::Strict).await;
    post(addr, "/hook", &hook_body("UserPromptSubmit", "s1")).await;
    post(addr, "/hook", &hook_body("Stop", "s1")).await;

    past_grace().await;

    let report = state(addr).await;
    assert!(report.contains(r#""state":"idle""#), "{report}");
    assert!(report.contains(r#""issued":[]"#), "a short turn must issue nothing: {report}");
}

#[tokio::test]
async fn a_long_turn_takes_the_screen_when_the_timer_fires() {
    let addr = start(ReaderMode::Strict).await;
    post(addr, "/hook", &hook_body("UserPromptSubmit", "s1")).await;

    past_grace().await;

    let report = state(addr).await;
    assert!(report.contains(r#""state":"reading""#), "{report}");
    assert!(report.contains("show_reader"), "{report}");
}

#[tokio::test]
async fn a_permission_prompt_raises_the_loud_indicator() {
    let addr = start(ReaderMode::Strict).await;
    post(addr, "/hook", &hook_body("UserPromptSubmit", "s1")).await;
    past_grace().await;

    let body = r#"{"session_id":"s1","hook_event_name":"Notification",
                   "notification_type":"permission_prompt","message":"needs permission"}"#;
    post(addr, "/hook", body).await;

    let report = state(addr).await;
    assert!(report.contains("indicator_needs_input"), "{report}");
    assert!(report.contains(r#""alert":"needs_input""#), "{report}");
}

#[tokio::test]
async fn an_idle_notification_is_ignored_while_reading() {
    let addr = start(ReaderMode::Strict).await;
    post(addr, "/hook", &hook_body("UserPromptSubmit", "s1")).await;
    past_grace().await;

    let body = r#"{"session_id":"s1","hook_event_name":"Notification",
                   "notification_type":"idle_prompt","message":"still there?"}"#;
    post(addr, "/hook", body).await;

    let report = state(addr).await;
    assert!(!report.contains("needs_input"), "reading is meant to look idle: {report}");
    assert!(report.contains(r#""state":"reading""#), "{report}");
}

#[tokio::test]
async fn another_session_cannot_end_the_takeover() {
    let addr = start(ReaderMode::Strict).await;
    post(addr, "/hook", &hook_body("UserPromptSubmit", "s1")).await;
    past_grace().await;

    post(addr, "/hook", &hook_body("Stop", "other-window")).await;

    let report = state(addr).await;
    assert!(report.contains(r#""state":"reading""#), "{report}");
    assert!(report.contains(r#""session":"s1""#), "{report}");
}

#[tokio::test]
async fn a_full_turn_runs_from_prompt_to_release() {
    let addr = start(ReaderMode::Strict).await;

    post(addr, "/hook", &hook_body("UserPromptSubmit", "s1")).await;
    past_grace().await;
    post(addr, "/hook", &hook_body("Stop", "s1")).await;
    read_pages(addr, 3).await;

    let (status, body) = post(addr, "/exit", "").await;
    assert_eq!(status, 200);
    assert!(body.contains("start_quiz"), "strict mode must gate the exit: {body}");

    let (status, body) = get(addr, "/gate").await;
    assert_eq!(status, 200, "{body}");
    let quiz_id = quiz_id_of(&body);

    let answers = format!(
        r#"{{"quiz_id":"{quiz_id}","answers":[{{"question_id":"{id}","choice":0}}]}}"#,
        id = first_question_id(&body),
    );
    let (status, body) = post(addr, "/gate/answers", &answers).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""outcome":"released""#) || body.contains(r#""outcome":"retry""#));

    // However that went, one more attempt always ends it: retry once, then release.
    if body.contains(r#""outcome":"retry""#) {
        post(addr, "/gate/answers", &answers).await;
    }

    let report = state(addr).await;
    assert!(report.contains(r#""state":"idle""#), "{report}");
    assert!(report.contains(r#""session":null"#), "the binding must be released: {report}");
}

/// Pulls a field out of a JSON body without a parser, which is all these tests need.
fn field_after(body: &str, key: &str) -> String {
    let start = body.find(key).unwrap_or_else(|| panic!("{key} missing from {body}")) + key.len();
    body[start..].split('"').next().expect("value").to_string()
}

fn quiz_id_of(body: &str) -> String {
    field_after(body, r#""quiz_id":""#)
}

fn first_question_id(body: &str) -> String {
    field_after(body, r#""id":""#)
}

#[tokio::test]
async fn the_gate_asks_about_what_was_just_read() {
    let addr = start(ReaderMode::Strict).await;
    at_the_gate(addr, 4).await;

    let (status, body) = get(addr, "/gate").await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""gate":"open""#), "{body}");
    assert!(body.contains("which word completes this line"), "{body}");
    assert!(!body.contains("answer_index"), "the answer key must stay in the daemon: {body}");
}

#[tokio::test]
async fn asking_twice_returns_the_same_quiz() {
    // A reader that reconnects mid-gate must not be handed a fresh set of attempts.
    let addr = start(ReaderMode::Strict).await;
    at_the_gate(addr, 4).await;

    let (_, first) = get(addr, "/gate").await;
    let (_, second) = get(addr, "/gate").await;
    assert_eq!(quiz_id_of(&first), quiz_id_of(&second));
}

#[tokio::test]
async fn there_is_no_quiz_when_no_gate_is_open() {
    let addr = start(ReaderMode::Strict).await;
    let (status, _) = get(addr, "/gate").await;
    assert_eq!(status, 409, "asking for a quiz you do not owe is a mistake, not a quiz");
}

#[tokio::test]
async fn a_gate_with_nothing_to_ask_about_lets_you_go() {
    // Exiting a stretch where nothing was read. The tool has not earned the screen.
    let addr = start(ReaderMode::Strict).await;
    post(addr, "/hook", &hook_body("UserPromptSubmit", "s1")).await;
    past_grace().await;
    post(addr, "/exit", "").await;

    let (status, body) = get(addr, "/gate").await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""reason":"ungated""#), "{body}");

    let report = state(addr).await;
    assert!(report.contains(r#""state":"idle""#), "and the screen comes back: {report}");
}

#[tokio::test]
async fn a_correct_answer_opens_the_gate() {
    let addr = start_with(ReaderMode::Strict, Arc::new(Rigged)).await;
    at_the_gate(addr, 3).await;

    let (_, body) = get(addr, "/gate").await;
    let answers = format!(
        r#"{{"quiz_id":"{}","answers":[
            {{"question_id":"q0","choice":0}},
            {{"question_id":"q1","choice":0}},
            {{"question_id":"q2","choice":0}}]}}"#,
        quiz_id_of(&body),
    );

    let (status, body) = post(addr, "/gate/answers", &answers).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""reason":"passed""#), "{body}");

    let report = state(addr).await;
    assert!(report.contains(r#""state":"idle""#), "{report}");
    assert!(report.contains("hide_reader"), "{report}");
}

#[tokio::test]
async fn a_wrong_answer_buys_one_retry_and_no_more() {
    let addr = start_with(ReaderMode::Strict, Arc::new(Rigged)).await;
    at_the_gate(addr, 3).await;

    let (_, body) = get(addr, "/gate").await;
    let answers = format!(
        r#"{{"quiz_id":"{}","answers":[
            {{"question_id":"q0","choice":1}},
            {{"question_id":"q1","choice":1}},
            {{"question_id":"q2","choice":1}}]}}"#,
        quiz_id_of(&body),
    );

    let (_, first) = post(addr, "/gate/answers", &answers).await;
    assert!(first.contains(r#""outcome":"retry""#), "{first}");
    let report = state(addr).await;
    assert!(report.contains(r#""state":"gate""#), "still owed: {report}");

    let (_, second) = post(addr, "/gate/answers", &answers).await;
    assert!(second.contains(r#""reason":"exhausted""#), "the tool nags, it does not jail: {second}");
    let report = state(addr).await;
    assert!(report.contains(r#""state":"idle""#), "{report}");
}

#[tokio::test]
async fn a_blank_sheet_is_refused_in_strict_mode() {
    let addr = start_with(ReaderMode::Strict, Arc::new(Rigged)).await;
    at_the_gate(addr, 3).await;

    let (_, body) = get(addr, "/gate").await;
    let blank = format!(r#"{{"quiz_id":"{}","answers":[]}}"#, quiz_id_of(&body));

    let (status, body) = post(addr, "/gate/answers", &blank).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""reason":"not_attempted""#), "{body}");

    let report = state(addr).await;
    assert!(report.contains(r#""state":"gate""#), "a refusal must not open the gate: {report}");
}

#[tokio::test]
async fn strict_mode_has_no_skip_button() {
    let addr = start(ReaderMode::Strict).await;
    at_the_gate(addr, 4).await;

    let (status, body) = post(addr, "/gate/skip", "").await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""reason":"not_skippable""#), "{body}");

    let report = state(addr).await;
    assert!(report.contains(r#""state":"gate""#), "{report}");
}

#[tokio::test]
async fn lenient_mode_lets_you_walk_away() {
    let addr = start(ReaderMode::Lenient).await;
    at_the_gate(addr, 4).await;

    let (status, body) = post(addr, "/gate/skip", "").await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""reason":"skipped""#), "{body}");

    let report = state(addr).await;
    assert!(report.contains(r#""state":"idle""#), "{report}");
}

#[tokio::test]
async fn an_answer_sheet_for_a_stale_quiz_is_refused() {
    let addr = start_with(ReaderMode::Strict, Arc::new(Rigged)).await;
    at_the_gate(addr, 3).await;
    get(addr, "/gate").await;

    let stale = r#"{"quiz_id":"gate-0","answers":[{"question_id":"q0","choice":0}]}"#;
    let (status, body) = post(addr, "/gate/answers", stale).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""reason":"wrong_quiz""#), "{body}");
}

#[tokio::test]
async fn codex_notify_ends_a_turn_it_did_not_start() {
    let addr = start(ReaderMode::Strict).await;
    // Codex has no turn-start notification, so the arm has to come from its hooks.
    post(addr, "/hook", &hook_body("UserPromptSubmit", "thread-7")).await;
    past_grace().await;

    let body = r#"{"type":"agent-turn-complete","thread-id":"thread-7","turn-id":"1",
                   "last-assistant-message":"done"}"#;
    let (status, _) = post(addr, "/notify", body).await;
    assert_eq!(status, 204);

    let report = state(addr).await;
    assert!(report.contains("indicator_done"), "{report}");
}

/// The bug a live agent found: the machine only armed from idle, so once the indicator
/// was up every later prompt was swallowed and the reader could never be reached again.
#[tokio::test]
async fn a_new_prompt_after_the_indicator_returns_to_reading() {
    let addr = start(ReaderMode::Strict).await;
    post(addr, "/hook", &hook_body("UserPromptSubmit", "s1")).await;
    past_grace().await;
    post(addr, "/hook", &hook_body("Stop", "s1")).await;

    let report = state(addr).await;
    assert!(report.contains(r#""state":"ready""#), "{report}");

    // Back to the terminal, another prompt, without ever leaving the reader.
    post(addr, "/hook", &hook_body("UserPromptSubmit", "s1")).await;

    let report = state(addr).await;
    assert!(report.contains(r#""state":"reading""#), "must not wedge in ready: {report}");
    assert!(report.contains("clear_indicator"), "the stale indicator must go: {report}");
}

/// A hook that never arrives and a hook that arrives and is correctly ignored are
/// indistinguishable from `issued` alone, which makes a live wiring test unreadable.
#[tokio::test]
async fn deliveries_are_logged_even_when_they_change_nothing() {
    let addr = start(ReaderMode::Strict).await;
    post(addr, "/hook", &hook_body("PreToolUse", "s1")).await;

    let report = state(addr).await;
    assert!(report.contains("PreToolUse"), "the delivery must be visible: {report}");
    assert!(report.contains("ignored"), "and marked as ignored: {report}");
    assert!(report.contains(r#""issued":[]"#), "while changing nothing: {report}");
    assert!(report.contains(r#""state":"idle""#), "{report}");
}

#[tokio::test]
async fn malformed_json_is_refused_without_touching_the_screen() {
    let addr = start(ReaderMode::Strict).await;
    let (status, _) = post(addr, "/hook", "{not json").await;
    assert_eq!(status, 400);

    let report = state(addr).await;
    assert!(report.contains(r#""state":"idle""#), "{report}");
}

#[tokio::test]
async fn free_mode_releases_without_a_quiz() {
    let addr = start(ReaderMode::Free).await;
    post(addr, "/hook", &hook_body("UserPromptSubmit", "s1")).await;
    past_grace().await;

    let (_, body) = post(addr, "/exit", "").await;
    assert!(!body.contains("start_quiz"), "free mode must not gate: {body}");
    assert!(body.contains("hide_reader"), "{body}");
}

#[tokio::test]
async fn a_subscriber_is_told_the_current_state_when_it_connects() {
    let addr = start(ReaderMode::Strict).await;
    post(addr, "/hook", &hook_body("UserPromptSubmit", "s1")).await;
    past_grace().await;

    // The reader that connects here missed show_reader entirely - it was sent before
    // anything was listening. Without the resync it would sit blank mid-takeover.
    let mut stream = subscribe(addr).await;
    let seen = drain(&mut stream).await;
    assert!(seen.contains("data: show_reader"), "{seen}");
}

#[tokio::test]
async fn an_idle_daemon_tells_a_subscriber_to_stay_hidden() {
    let addr = start(ReaderMode::Strict).await;

    let mut stream = subscribe(addr).await;
    let seen = drain(&mut stream).await;
    assert!(seen.contains("data: hide_reader"), "{seen}");
}

#[tokio::test]
async fn commands_reach_a_subscriber_as_they_are_issued() {
    let addr = start(ReaderMode::Strict).await;
    let mut stream = subscribe(addr).await;
    drain(&mut stream).await; // the resync

    post(addr, "/hook", &hook_body("UserPromptSubmit", "s1")).await;
    past_grace().await;
    let seen = drain(&mut stream).await;
    assert!(seen.contains("data: show_reader"), "grace expiry should reach the reader: {seen}");

    post(addr, "/hook", &hook_body("Stop", "s1")).await;
    let seen = drain(&mut stream).await;
    assert!(seen.contains("data: indicator_done"), "the turn ending should too: {seen}");
}

#[tokio::test]
async fn every_subscriber_sees_the_same_commands() {
    let addr = start(ReaderMode::Strict).await;
    let mut first = subscribe(addr).await;
    let mut second = subscribe(addr).await;
    drain(&mut first).await;
    drain(&mut second).await;

    post(addr, "/hook", &hook_body("UserPromptSubmit", "s1")).await;
    past_grace().await;

    // A window and a TUI can be open at once, and neither may steal the other's commands.
    assert!(drain(&mut first).await.contains("data: show_reader"));
    assert!(drain(&mut second).await.contains("data: show_reader"));
}

/// Wall-clock now: page views are dated against a calendar, not the machine's uptime.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64
}

fn book_body() -> String {
    r#"{"id":"book-1","format":"pdf","title":"Some Book","author":null,
        "path":null,"total_pages":200}"#
        .to_string()
}

fn page_view_body(page: u32, dwell_ms: u64, entered_at: i64) -> String {
    format!(
        r#"{{"book_id":"book-1",
            "locator":{{"kind":"page","page":{page},"page_label":"{page}"}},
            "text":"a paragraph that was actually on the screen",
            "entered_at":{entered_at},"exited_at":{exited},"dwell_ms":{dwell_ms}}}"#,
        exited = entered_at + dwell_ms as i64,
    )
}

#[tokio::test]
async fn a_page_that_was_read_becomes_progress() {
    let addr = start(ReaderMode::Strict).await;
    assert_eq!(post(addr, "/book", &book_body()).await.0, 204);

    let (status, body) = post(addr, "/page-view", &page_view_body(1, 9_000, now_ms())).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""counted":true"#), "{body}");

    let (_, progress) = request(addr, "GET", "/progress", Some(TOKEN), "").await;
    assert!(progress.contains(r#""pages_today":1"#), "{progress}");
}

#[tokio::test]
async fn a_page_turned_past_is_stored_but_is_not_progress() {
    let addr = start(ReaderMode::Strict).await;
    post(addr, "/book", &book_body()).await;

    // Under the 3s default: seen, not read.
    let (_, body) = post(addr, "/page-view", &page_view_body(1, 900, now_ms())).await;
    assert!(body.contains(r#""counted":false"#), "{body}");
    assert!(body.contains(r#""stored":true"#), "a skim is still history: {body}");

    let (_, progress) = request(addr, "GET", "/progress", Some(TOKEN), "").await;
    assert!(progress.contains(r#""pages_today":0"#), "{progress}");
}

#[tokio::test]
async fn a_retried_report_does_not_count_twice() {
    let addr = start(ReaderMode::Strict).await;
    post(addr, "/book", &book_body()).await;
    let view = page_view_body(1, 9_000, now_ms());

    post(addr, "/page-view", &view).await;
    let (status, body) = post(addr, "/page-view", &view).await;
    assert_eq!(status, 200, "a retry is not an error: {body}");
    assert!(body.contains(r#""stored":false"#), "{body}");

    let (_, progress) = request(addr, "GET", "/progress", Some(TOKEN), "").await;
    assert!(progress.contains(r#""pages_today":1"#), "{progress}");
}

#[tokio::test]
async fn reading_reported_against_an_unregistered_book_is_refused() {
    let addr = start(ReaderMode::Strict).await;
    // No POST /book first. Accepting this would silently create a second, untitled book.
    let (status, body) = post(addr, "/page-view", &page_view_body(1, 9_000, now_ms())).await;
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("/book"), "the error should say how to fix it: {body}");
}

#[tokio::test]
async fn the_goal_comes_from_the_config() {
    let addr = start(ReaderMode::Strict).await;
    let (_, progress) = request(addr, "GET", "/progress", Some(TOKEN), "").await;
    assert!(progress.contains(r#""goal":20"#), "{progress}");
    assert!(progress.contains(r#""met":false"#), "{progress}");
}
