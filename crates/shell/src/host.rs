//! Running the daemon inside this process, unless someone else already is.
//!
//! This exists because of autostart, and it is worth saying why rather than leaving it to
//! look like a shortcut. Windows starts login items by running a command line. A command
//! line pointing at a console program opens a console window at every sign-in and leaves
//! it sitting there; the daemon is a console program, and there is no flag on a `Run`
//! entry that hides it. So the thing registered to start at sign-in has to be this window,
//! which is a GUI program - and then this window has to bring the daemon with it, or
//! autostart brings back a reader with nothing to read the hooks.
//!
//! There are two ways to bring it: spawn the daemon's exe as a child, or run its server
//! here. This is the second. The shell already links the daemon as a library and already
//! reads its config and its token, so hosting adds no dependency and no new idea.
//! Spawning would add a path to guess, a window flag to remember, and a child to orphan
//! if this process dies badly.
//!
//! What it does *not* do is take the daemon's decisions. `serve` is the daemon's own
//! function over the daemon's own router; this file chooses nothing except whether to call
//! it.

use std::io;

use mytimeoff_core::Config;
use mytimeoff_daemon::{Daemon, bind, paths, secret, serve};
use mytimeoff_quiz as quiz;
use mytimeoff_store::Store;

/// What happened when this process tried to be the daemon.
pub enum Host {
    /// Nothing was listening, so the daemon now runs here.
    Hosted(String),
    /// The port was already taken. Assumed to be a daemon someone started deliberately -
    /// in a terminal, or by an earlier copy of this window - and left alone.
    ///
    /// Not fought over: `bind` either succeeds or it does not, so two daemons on one port
    /// is not a state this can reach. If the port belongs to something that is not a
    /// daemon at all, the reader will fail to load and say so, which is a better failure
    /// than a second daemon quietly disagreeing with the first about what state the user
    /// is in.
    Already,
}

/// Starts the daemon here if nothing is serving on its port.
///
/// The bind is what decides, rather than a health check first: asking "is something there?"
/// and then binding leaves a gap between the question and the answer, and binding is the
/// question and the answer at once.
pub async fn ensure(config: &Config, token: &str) -> io::Result<Host> {
    let listener = match bind(config.port).await {
        Ok(listener) => listener,
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => return Ok(Host::Already),
        Err(error) => return Err(error),
    };

    let store = Store::open(&paths::database()?)?;
    let (questions, note) = quiz::source_for(config, &secret::find).map_err(io::Error::other)?;
    let daemon = Daemon::new(config.clone(), store, token.to_string(), questions);

    tauri::async_runtime::spawn(async move {
        if let Err(error) = serve(listener, daemon).await {
            // There is no console to print to in a release build, and no honest way to
            // recover: the reader is about to start failing every request. Saying it here
            // at least puts it in a debug run and in any log the window is started with.
            eprintln!("the daemon stopped: {error}");
        }
    });

    Ok(Host::Hosted(note))
}
