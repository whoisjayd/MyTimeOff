//! The icon left behind when the window is gone.
//!
//! This module exists for what it makes safe rather than for what it does. Closing the
//! window used to quit MyTimeOff, which was at least honest: the hooks stayed in the
//! agent's settings and nothing was on the other end of them, and the README said so.
//! Hiding instead is better - but hiding with nothing in the notification area leaves a
//! running program with no window and no icon, which is exactly the bug that made
//! double-clicking the shortcut appear to do nothing. Close-to-hide and this file are one
//! change, not two, and `main` will not enable the first without the second.
//!
//! There is no state here and no policy. Left click asks `takeover` to raise the window;
//! the menu asks for the same thing, or asks Tauri to exit. Everything about *when* the
//! screen is taken still lives in the daemon.

use std::io;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::takeover;

/// Menu item ids. Strings because that is what Tauri hands back in the event, and two
/// spellings of the same word is a bug that compiles.
const OPEN: &str = "open";
const QUIT: &str = "quit";

/// What Windows shows when the pointer rests on the icon.
///
/// It answers the only question a hidden program raises - whether it is still doing
/// anything - and with the window closed there is nowhere else for that answer to be.
const TOOLTIP: &str = "MyTimeOff - listening for your agent";

/// Puts the icon in the notification area.
///
/// The error is the caller's to care about rather than something to swallow here: without
/// an icon, closing the window has to go back to quitting.
pub fn install(app: &AppHandle) -> io::Result<()> {
    let open = MenuItem::with_id(app, OPEN, "Open MyTimeOff", true, None::<&str>)
        .map_err(io::Error::other)?;
    let quit = MenuItem::with_id(app, QUIT, "Quit MyTimeOff", true, None::<&str>)
        .map_err(io::Error::other)?;
    let separator = PredefinedMenuItem::separator(app).map_err(io::Error::other)?;
    let menu = Menu::with_items(app, &[&open, &separator, &quit]).map_err(io::Error::other)?;

    // The window's own icon, not a second one to keep in step with it. Tauri embedded it
    // from `bundle.icon`, so this is the image already on the taskbar and in the title
    // bar - which is what makes the tray icon recognisable as this program.
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| io::Error::other("this build embedded no window icon"))?;

    TrayIconBuilder::with_id("reader")
        .icon(icon)
        .tooltip(TOOLTIP)
        .menu(&menu)
        // Left click opens, and only right click opens the menu. Windows users expect
        // that of a tray icon; a menu on the left button makes getting the window back a
        // two-step operation, and getting the window back is the common case.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            OPEN => show(app),
            // Deliberately unconditional, including while a gate is open. Strict mode is
            // not a lock - the README says so - and somebody who has right-clicked a tray
            // icon and chosen Quit has been explicit enough.
            QUIT => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // On release, not press: a click that started on the icon and ended elsewhere
            // is not a click on the icon.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show(tray.app_handle());
            }
        })
        .build(app)
        .map_err(io::Error::other)?;

    Ok(())
}

/// Brings the reader back, if there is one.
///
/// [`takeover::raise`] rather than anything stronger: somebody who clicked a tray icon
/// asked to see the app, not to be interrupted by it.
fn show(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("reader") {
        takeover::raise(&window);
    }
}
