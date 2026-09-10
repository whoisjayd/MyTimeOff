//! End-to-end tests against a real listener.
//!
//! These deliberately speak HTTP over a socket rather than calling handlers directly:
//! the things most likely to be wrong - the auth layer covering every route, loopback
//! binding, JSON arriving as a body - only exist at that level.

use std::net::SocketAddr;
use std::time::Duration;

use mytimeoff_core::ReaderMode;
use mytimeoff_daemon::{Daemon, bind, serve};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const TOKEN: &str = "test-token-0123456789";
/// Short enough to expire inside a test, long enough not to race the request.
const GRACE: Duration = Duration::from_millis(80);

/// Starts a daemon on an OS-assigned port.
async fn start(mode: ReaderMode) -> SocketAddr {
    let listener = bind(0).await.expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let daemon = Daemon::new(mode, GRACE, TOKEN.to_string());
    tokio::spawn(async move { serve(listener, daemon).await });
    addr
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
        ("POST", "/gate/cleared"),
        ("GET", "/state"),
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

    let (status, body) = post(addr, "/exit", "").await;
    assert_eq!(status, 200);
    assert!(body.contains("start_quiz"), "strict mode must gate the exit: {body}");

    let (status, body) = post(addr, "/gate/cleared", "").await;
    assert_eq!(status, 200);
    assert!(body.contains("hide_reader"), "{body}");

    let report = state(addr).await;
    assert!(report.contains(r#""state":"idle""#), "{report}");
    assert!(report.contains(r#""session":null"#), "the binding must be released: {report}");
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
