//! The origin the reader is served from, and the only thing in this app that holds the
//! token.
//!
//! In development the Vite dev server does exactly this: it serves the page and proxies
//! `/daemon/*` onward with an `Authorization` header attached, so the page can call the
//! daemon same-origin and never sees the secret. Production has no Vite. Rather than give
//! the page an absolute URL and the token to go with it - or invent a second way to talk
//! to the daemon that only the desktop build uses - the shell stands up the same shape:
//! one loopback origin, assets on one side of it, the daemon on the other.
//!
//! That is what lets `apps/reader` be the same code in a browser tab and in this window,
//! down to the file. It is also why the token never reaches the webview: a page that
//! holds a bearer token is a page one XSS away from handing the daemon to someone else.
//!
//! The assets are Tauri's own - the ones `frontendDist` embedded into this binary - read
//! back out through `AssetResolver`. Nothing is duplicated and nothing is loaded off disk.

use std::collections::HashSet;
use std::io;
use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use tauri::{AssetResolver, Runtime};
use tokio::net::TcpListener;

/// Tried first so the page keeps one origin across restarts. Anything a webview
/// remembers - a permission, a stored setting - is keyed to the origin, and an origin
/// that moves every launch quietly forgets all of it.
const PREFERRED_PORT: u16 = 8788;

/// Headers that describe *this* hop and must not be forwarded onto the next one.
///
/// `connection` and `transfer-encoding` belong to the connection they arrived on;
/// `host` names this bridge, not the daemon; and `authorization` is the one the bridge
/// is about to set itself - a page that sent its own must not be able to override it.
const HOP_BY_HOP: [&str; 4] = ["connection", "transfer-encoding", "host", "authorization"];

struct Bridge<R: Runtime> {
    assets: AssetResolver<R>,
    /// The names of the files that were actually embedded. See `resolve`.
    embedded: HashSet<String>,
    daemon: String,
    token: String,
    http: reqwest::Client,
}

/// Serves the reader on a loopback port and returns where it landed.
///
/// Binds before returning, so by the time a window is pointed at this address there is
/// something listening: a webview that races the server shows an error page and does not
/// retry, which would be a blank window on every cold start.
pub async fn start<R: Runtime>(
    assets: AssetResolver<R>,
    daemon_port: u16,
    token: String,
) -> io::Result<SocketAddr> {
    // No total timeout. It would apply to the whole response body, and `/events` is a
    // stream that is meant to stay open for as long as the daemon is up - a timeout here
    // would sever the takeover commands every N seconds and look like a flaky daemon.
    let http = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(io::Error::other)?;

    // Read once, at startup: the set is fixed at build time, and `iter` borrows rather
    // than copies, so this costs the length of a few file names.
    let embedded = assets.iter().map(|(name, _)| named(&name)).collect();

    let bridge = std::sync::Arc::new(Bridge {
        assets,
        embedded,
        daemon: format!("http://127.0.0.1:{daemon_port}"),
        token,
        http,
    });

    let app = Router::new()
        .route("/daemon/{*path}", any(proxy))
        .fallback(asset)
        .with_state(bridge);

    let listener = listen().await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            eprintln!("bridge stopped: {error}");
        }
    });
    Ok(addr)
}

/// The preferred port, or any free one.
///
/// Falling back rather than failing: something else holding 8788 is not a reason the
/// reader cannot open, and a shell that refuses to start over a port number would be a
/// worse bug than a window whose origin moved.
async fn listen() -> io::Result<TcpListener> {
    match TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, PREFERRED_PORT)).await {
        Ok(listener) => Ok(listener),
        Err(error) => {
            eprintln!("bridge: port {PREFERRED_PORT} is taken ({error}); using another");
            TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await
        }
    }
}

/// Passes a request to the daemon with the token attached, and streams the answer back.
///
/// Streaming rather than buffering is not an optimisation: `/events` never ends, and a
/// proxy that waited for the body would hold the reader's command stream forever.
async fn proxy<R: Runtime>(
    State(bridge): State<std::sync::Arc<Bridge<R>>>,
    request: Request,
) -> Response {
    let path = request.uri().path().trim_start_matches("/daemon");
    let query = request.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let url = format!("{}{path}{query}", bridge.daemon);

    let (parts, body) = request.into_parts();
    let mut onward = bridge.http.request(parts.method, &url);
    for (name, value) in &parts.headers {
        if !HOP_BY_HOP.contains(&name.as_str()) {
            onward = onward.header(name, value);
        }
    }
    // The whole reason this hop exists.
    onward = onward.header(header::AUTHORIZATION, format!("Bearer {}", bridge.token));

    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };

    match onward.body(bytes).send().await {
        Ok(answer) => {
            let mut response = Response::builder().status(answer.status());
            for (name, value) in answer.headers() {
                if !HOP_BY_HOP.contains(&name.as_str()) {
                    response = response.header(name, value);
                }
            }
            response
                .body(Body::from_stream(answer.bytes_stream()))
                .unwrap_or_else(|error| {
                    (StatusCode::BAD_GATEWAY, error.to_string()).into_response()
                })
        }
        // The daemon not being up is the normal case on a cold start, not an exception.
        // Say so in a way the reader's own error handling can show.
        Err(error) => (StatusCode::BAD_GATEWAY, format!("daemon unreachable: {error}")).into_response(),
    }
}

/// Serves one of the files Tauri embedded, falling back to the app itself.
///
/// The fallback is what makes a deep link or a reload land on the reader rather than a
/// 404: this is a single-page app, and every path that is not a file is a route into it.
async fn asset<R: Runtime>(State(bridge): State<std::sync::Arc<Bridge<R>>>, uri: Uri) -> Response {
    let path = uri.path();
    let Some(target) = resolve(&bridge.embedded, path) else {
        return (StatusCode::NOT_FOUND, format!("no asset at {path}")).into_response();
    };

    match bridge.assets.get(target) {
        Some(asset) => {
            let mime = HeaderValue::from_str(&asset.mime_type)
                .unwrap_or(HeaderValue::from_static("application/octet-stream"));
            ([(header::CONTENT_TYPE, mime)], asset.bytes).into_response()
        }
        // Unreachable via `resolve`, which only names files that are known to be there.
        None => (StatusCode::NOT_FOUND, format!("no asset at {path}")).into_response(),
    }
}

/// Which embedded file a request path should be answered with, or None for a 404.
///
/// This decides rather than delegating, because `AssetResolver::get` answers *every*
/// unknown path with `index.html` - so a request for a script that did not make it into
/// the build comes back as a 200 of HTML. The browser then reports a syntax error in a
/// file that looks fine on disk, which is a genuinely horrible afternoon. A path that
/// names a file gets that file or a 404; only a path that names no file at all is treated
/// as a route into the app.
fn resolve(embedded: &HashSet<String>, path: &str) -> Option<String> {
    let wanted = named(path);
    if embedded.contains(&wanted) {
        return Some(wanted);
    }
    let names_a_file = path.rsplit('/').next().is_some_and(|last| last.contains('.'));
    (!names_a_file).then(|| "/index.html".to_string())
}

/// One spelling for a file name, so the set and the lookups agree.
fn named(path: &str) -> String {
    if path.starts_with('/') { path.to_string() } else { format!("/{path}") }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderName;

    use super::*;

    #[test]
    fn the_headers_the_bridge_strips_are_real_header_names() {
        for name in HOP_BY_HOP {
            assert!(name.parse::<HeaderName>().is_ok(), "{name} is not a header name");
            assert_eq!(name, name.to_lowercase(), "comparison is against lowercased names");
        }
    }

    fn built() -> HashSet<String> {
        ["/index.html", "/assets/index-Tt7RaoDv.js", "/assets/index-CsNFTMZm.css"]
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn a_file_that_was_built_is_served_as_itself() {
        assert_eq!(resolve(&built(), "/assets/index-Tt7RaoDv.js").as_deref(), Some("/assets/index-Tt7RaoDv.js"));
        assert_eq!(resolve(&built(), "/index.html").as_deref(), Some("/index.html"));
    }

    #[test]
    fn a_route_into_the_app_is_served_the_app() {
        assert_eq!(resolve(&built(), "/").as_deref(), Some("/index.html"));
        assert_eq!(resolve(&built(), "/library/some-book").as_deref(), Some("/index.html"));
    }

    #[test]
    fn a_file_that_is_not_there_is_a_404_rather_than_a_page_of_html() {
        // The whole reason `resolve` exists instead of leaning on the resolver's own
        // fallback: answering this with index.html turns a broken build into a syntax
        // error in a file that is fine.
        assert_eq!(resolve(&built(), "/assets/index-OLDHASH.js"), None);
        assert_eq!(resolve(&built(), "/favicon.ico"), None);
    }

    #[test]
    fn the_page_cannot_supply_its_own_authorization() {
        // The bridge holds the token precisely so the page does not. If a page could send
        // an Authorization header that survived this hop, it could talk to the daemon with
        // a token it guessed - and, worse, the bridge would stop being the only holder.
        assert!(HOP_BY_HOP.contains(&"authorization"));
    }
}
