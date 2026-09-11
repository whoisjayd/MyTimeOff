//! The window that can take the screen.
//!
//! Everything this app does that a browser tab cannot comes down to one sentence in
//! `styles.css`: *a browser page cannot raise itself, so a takeover can only be
//! announced, not taken*. The reader has been drawing the takeover for weeks and never
//! been able to perform it. This performs it.
//!
//! What it deliberately is not: a place for rules. It adds no endpoint, no state and no
//! policy. It subscribes to the same `/events` stream the reader subscribes to, reads the
//! same commands, and does the one thing with them that a page cannot. If this file ever
//! starts deciding *when* to take the screen rather than *how*, the decision has escaped
//! the state machine and belongs back in `crates/core`.
//!
//! It does run the daemon inside itself, which looks like an exception and is not: it
//! calls the daemon's own `serve` over the daemon's own router and decides nothing about
//! what that server does. `host.rs` explains why autostart leaves no other option.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod host;
mod takeover;

use std::io;

use mytimeoff_daemon::{autostart, paths, settings, token};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

fn main() {
    tauri::Builder::default()
        // One MyTimeOff, however many times the icon is clicked. Without this a second
        // launch finds the port taken, carries on regardless, and leaves a process with
        // no window behind - which is indistinguishable, from the outside, from clicking
        // an icon and having nothing happen at all.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // Whatever the second launch was asked to do, this one is a person looking
            // for the app they already have. The flag is not consulted: Windows starts
            // the login copy once, so a second launch is always a human.
            if let Some(window) = app.get_webview_window("reader") {
                takeover::raise(&window);
            }
        }))
        .setup(|app| {
            // The same three files the daemon reads, read the same way. The shell does not
            // get its own config format, its own token, or its own idea of where they live
            // - a second answer to "which port is the daemon on" is a second thing to be
            // wrong.
            // First, because everything below reads those files and this is what decides
            // where they are. An upgrade from before the installer existed finds them in
            // the folder the installer now owns.
            if let Some(note) = paths::migrate_legacy_state() {
                println!("{note}");
            }

            let config = settings::load_or_create(&paths::config()?)?;
            let secret = token::load_or_create(&paths::token()?)?;

            // Before the bridge, because the bridge is only useful pointed at a daemon.
            // Whether that daemon is this process or one already running is decided here
            // and nowhere else.
            match tauri::async_runtime::block_on(host::ensure(&config, &secret))? {
                host::Host::Hosted(note) => {
                    println!("daemon hosted here on 127.0.0.1:{}", config.port);
                    println!("quiz:   {note}");
                }
                host::Host::Already => {
                    println!("daemon already running on 127.0.0.1:{}", config.port);
                }
            }

            let assets = app.handle().asset_resolver();
            let addr =
                tauri::async_runtime::block_on(bridge::start(assets, config.port, secret))?;
            println!("reader served on http://{addr} (daemon on 127.0.0.1:{})", config.port);

            // Sign-in is the one launch nobody asked for, and the only one that should
            // stay out of sight. `autostart` writes this word into the `Run` entry; every
            // other way of starting this program - the Start menu, the shortcut, a
            // terminal - is somebody asking to see it.
            let at_login = std::env::args().any(|arg| arg == autostart::AT_LOGIN);

            let window = WebviewWindowBuilder::new(
                app,
                "reader",
                WebviewUrl::External(format!("http://{addr}/").parse().map_err(io::Error::other)?),
            )
            .title("MyTimeOff")
            .inner_size(1_100.0, 820.0)
            .min_inner_size(600.0, 420.0)
            .center()
            // Built hidden either way, so that the window is never drawn in the wrong
            // place and then moved: it is shown below, once, if this launch was a person.
            .visible(false)
            .build()?;

            if !at_login {
                takeover::raise(&window);
            }

            takeover::watch(window, addr);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the shell could not start");
}
