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

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod takeover;

use std::io;

use mytimeoff_daemon::{paths, settings, token};
use tauri::{WebviewUrl, WebviewWindowBuilder};

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            // The same three files the daemon reads, read the same way. The shell does not
            // get its own config format, its own token, or its own idea of where they live
            // - a second answer to "which port is the daemon on" is a second thing to be
            // wrong.
            let config = settings::load_or_create(&paths::config()?)?;
            let secret = token::load_or_create(&paths::token()?)?;

            let assets = app.handle().asset_resolver();
            let addr =
                tauri::async_runtime::block_on(bridge::start(assets, config.port, secret))?;
            println!("reader served on http://{addr} (daemon on 127.0.0.1:{})", config.port);

            let window = WebviewWindowBuilder::new(
                app,
                "reader",
                WebviewUrl::External(format!("http://{addr}/").parse().map_err(io::Error::other)?),
            )
            .title("MyTimeOff")
            .inner_size(1_100.0, 820.0)
            .min_inner_size(600.0, 420.0)
            .center()
            // Opened hidden. The screen is taken when the daemon says a turn has started,
            // not when the app happens to launch - starting visible would make every
            // reboot an interruption.
            .visible(false)
            .build()?;

            takeover::watch(window, addr);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the shell could not start");
}
