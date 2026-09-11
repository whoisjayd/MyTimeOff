//! Doing what a page cannot: putting the window in front of you, and getting out of the
//! way again.
//!
//! This is the second subscriber to `/events`. The reader is the first, and it has been
//! reducing the same commands into the same three booleans since before this crate
//! existed. Nothing here tells the reader anything; both surfaces hear the daemon
//! directly and agree because they are obeying the same sentence, not because one is
//! relaying it to the other.
//!
//! Only two of the six commands mean anything to a window - `show_reader` and
//! `hide_reader`. The other four are about what is drawn *inside* it, which is the page's
//! business and none of this file's. That asymmetry is the shape of a correct adapter: it
//! handles what it uniquely can, and stays out of everything else.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use tauri::{Manager, WebviewWindow};
use tauri_plugin_notification::NotificationExt;

/// Whether the window is up because a person asked for it, rather than because a turn
/// started. Set by [`raise`]; cleared by [`take`] and by [`close_requested`], which are
/// the two ways that answer is spent; obeyed by [`release`] and [`close_requested`].
///
/// A process-wide static rather than state threaded through: there is exactly one reader
/// window, and `tauri-plugin-single-instance` is what makes that true of the whole
/// machine. Relaxed ordering is enough because nothing else is published alongside it -
/// the only question ever asked of it is "did somebody click?".
static WANTED: AtomicBool = AtomicBool::new(false);

/// Whether the tray has been pointed out yet in this run.
///
/// Windows files a new tray icon under the `^` overflow, so the first close-to-hide is
/// the one moment a person can reasonably conclude the program is gone. Once per run
/// rather than once ever: it needs no file on disk to remember, and being told again
/// after a restart is a smaller cost than a marker file that can go stale.
static HINTED: AtomicBool = AtomicBool::new(false);

/// Whether the daemon is holding the screen right now.
///
/// Not the inverse of [`WANTED`]: that one records who asked for the window, and is false
/// both during a takeover and before anybody has asked for anything. This one is set only
/// by the two commands that are the daemon speaking - `show_reader` and `hide_reader` -
/// which is the whole of what it means for the screen to be taken.
static HELD: AtomicBool = AtomicBool::new(false);

/// Records who the screen belongs to, and takes the minimise button away while it is not
/// the person in front of it.
///
/// Minimising is the third way out of a takeover, after the X and Escape, and the only one
/// no code had an opinion about. The gate exists to be answered; a window that can be put
/// on the taskbar and forgotten is a gate with a skip button drawn on the title bar - in
/// strict mode, where skipping is the one thing the policy refuses.
fn hold(window: &WebviewWindow, held: bool) {
    HELD.store(held, Ordering::Relaxed);
    // Greying the button out is the half that explains itself: the control is visibly not
    // available, rather than clicked and silently undone. See `resized` for the other half.
    let _ = window.set_minimizable(!held);
}

/// Comes back if the window was minimised out from under a takeover.
///
/// The button being gone is not the whole story on Windows: Show Desktop, Win+D and
/// Win+M put every window away, button or no button. So the last resort is simply to
/// return - and only while the daemon is holding the screen, because a window somebody
/// opened themselves is theirs to put away, and a program that fought that would be one
/// nobody opens twice.
pub fn resized(window: &WebviewWindow) {
    if !HELD.load(Ordering::Relaxed) {
        return;
    }
    if window.is_minimized().unwrap_or(false) {
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Answers the window's close button.
///
/// The X means two different things and the difference is already recorded in [`WANTED`].
/// A window somebody opened to look at is theirs to close, and nothing was read, so there
/// is no toll to pay and this hides it directly. A window the daemon put on the screen is
/// a different matter: closing it is asking to leave the reader, which is what `#back`
/// and Escape already ask, and the daemon decides what that costs. In strict mode the
/// answer is a quiz, the window stays up to show it, and the X has not been an escape
/// hatch - which it silently would have been, had this hidden the window itself.
pub fn close_requested(window: &WebviewWindow, bridge: SocketAddr) {
    if WANTED.swap(false, Ordering::Relaxed) {
        let _ = window.hide();
        hint(window);
        return;
    }

    let url = format!("http://{bridge}/daemon/exit");
    let window = window.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = reqwest::Client::new().post(&url).send().await {
            // A window that will not close is a worse failure than one that closes
            // without asking, so a daemon that cannot be reached does not get a veto.
            eprintln!("takeover: could not ask the daemon to release the screen: {error}");
            hold(&window, false);
            let _ = window.set_always_on_top(false);
            let _ = window.hide();
        }
    });
}

/// Says where the window went, once.
///
/// Best-effort on purpose: a notification that does not appear is a worse first run, not
/// a broken one, and nothing downstream depends on it.
fn hint(window: &WebviewWindow) {
    if HINTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let _ = window
        .app_handle()
        .notification()
        .builder()
        .title("MyTimeOff is still listening")
        .body("It is in the notification area. Click it to open the reader again.")
        .show();
}

/// How long to wait before trying the stream again.
///
/// The daemon is a separate process that the user can stop and start, so a dead stream is
/// an expected condition, not a crash. Reconnecting also resyncs: `/events` replays the
/// current state on connect, so a shell that was asleep through a takeover catches up on
/// the next attempt rather than staying wrong.
const RETRY: Duration = Duration::from_secs(2);

/// Starts listening. Returns at once; the watching happens on a background task.
///
/// Through the bridge, not straight at the daemon, though this process holds the token
/// and could go direct. Two reasons. The token then lives in exactly one place instead of
/// two, which is one fewer copy to leak or to forget to rotate. And the shell now depends
/// on the same hop the page does, so a broken bridge stops the takeover too - visibly -
/// rather than leaving a window that raises itself over a reader that cannot load.
pub fn watch(window: WebviewWindow, bridge: SocketAddr) {
    tauri::async_runtime::spawn(async move {
        let url = format!("http://{bridge}/daemon/events");
        loop {
            if let Err(error) = follow(&window, &url).await {
                eprintln!("takeover: {error}");
            }
            tokio::time::sleep(RETRY).await;
        }
    });
}

/// Reads one connection's worth of commands, returning when it ends.
async fn follow(
    window: &WebviewWindow,
    url: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // No total timeout: this stream is meant to stay open. See the same note in `bridge`.
    let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).build()?;
    let mut stream = http
        .get(url)
        .header("accept", "text/event-stream")
        .send()
        .await?
        .error_for_status()?
        .bytes_stream();

    let mut buffer = String::new();
    // Streamed bytes split wherever the network felt like it, so a line can arrive in two
    // pieces. Anything before the last newline is complete; the rest waits for more.
    while let Some(chunk) = stream.next().await {
        buffer.push_str(&String::from_utf8_lossy(&chunk?));
        while let Some(end) = buffer.find('\n') {
            let line = buffer[..end].trim().to_string();
            buffer.drain(..=end);
            if let Some(command) = line.strip_prefix("data:") {
                obey(window, command.trim());
            }
        }
    }
    Ok(())
}

/// Acts on one command, and ignores the ones that are not about the window.
///
/// An unknown name is ignored rather than treated as an error, for the reason
/// `parseCommand` gives on the other side: a surface that dies on a command it does not
/// recognise is a surface that a daemon upgrade can brick.
fn obey(window: &WebviewWindow, command: &str) {
    match command {
        "show_reader" => take(window),
        "hide_reader" => release(window),
        _ => {}
    }
}

/// Puts the reader in front of whatever the user was doing.
///
/// Always-on-top is set *and* the window is focused, because neither alone is enough:
/// Windows will refuse a focus steal from a background process, and a window that is
/// merely on top without focus swallows the first keypress. Setting on-top first means
/// that even when the focus request is refused, the reader is still visible - which is
/// the whole point, and a takeover that only half-worked is still a takeover.
fn take(window: &WebviewWindow) {
    // A takeover now owns whether this window is up, so whatever the person asked for
    // earlier has been answered. Without this, opening the window once would stop it ever
    // getting out of the way again.
    WANTED.store(false, Ordering::Relaxed);
    hold(window, true);
    let _ = window.show();
    let _ = window.set_always_on_top(true);
    let _ = window.unminimize();
    let _ = window.set_focus();
}

/// Shows the window because a person asked to see it.
///
/// Deliberately not [`take`]: always-on-top is what an interruption looks like, and being
/// interrupted is not what somebody who just double-clicked the icon asked for. They want
/// to look at the app, and an app that then pins itself over everything else is one they
/// will close and not open again.
pub fn raise(window: &WebviewWindow) {
    WANTED.store(true, Ordering::Relaxed);
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
}

/// Gives the screen back.
///
/// Always-on-top is dropped before hiding rather than after. A hidden window that is
/// still marked on-top comes back on-top the next time anything shows it, including the
/// user clicking the tray icon to read voluntarily - which would pin a window over their
/// work that the daemon never asked to take the screen.
///
/// It gives back the screen, not the window. A window somebody opened on purpose is not
/// this command's to close: `/events` replays the current state to every new subscriber,
/// so an idle daemon says `hide_reader` the instant the shell connects - which, before
/// [`WANTED`] existed, closed the window a second after it was clicked open and looked
/// exactly like a program that does not start.
fn release(window: &WebviewWindow) {
    // Before the early return below: the screen is given back either way, and a window
    // left with no minimise button because somebody happened to click the tray icon
    // during a takeover would stay that way until the next one.
    hold(window, false);
    let _ = window.set_always_on_top(false);
    if WANTED.load(Ordering::Relaxed) {
        return;
    }
    let _ = window.hide();
}
