//! Where MyTimeOff keeps its things.
//!
//! One module decides this, because the token, the config and (next) the database all
//! have to agree, and because the reason for the location is a security argument that
//! should only be made once.

use std::io;
use std::path::PathBuf;

/// The per-user directory holding everything this tool persists.
///
/// `%LOCALAPPDATA%` is per-user and not roamed, and its default ACL already excludes
/// other non-administrator users - so files inherit the protection we want without
/// hand-rolling Windows ACLs. An administrator can still read them, which is not a
/// boundary this tool can or should try to defend.
pub fn state_dir() -> io::Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_STATE_HOME"))
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no LOCALAPPDATA, XDG_STATE_HOME or HOME")
        })?;
    Ok(PathBuf::from(base).join("MyTimeOff"))
}

/// The shared secret guarding the loopback endpoint.
pub fn token() -> io::Result<PathBuf> {
    Ok(state_dir()?.join("hook-token"))
}

/// Books, page views, and everything read. Never leaves this machine.
pub fn database() -> io::Result<PathBuf> {
    Ok(state_dir()?.join("mytimeoff.sqlite3"))
}

/// The user's settings. Hand-edited, so it is written pretty-printed on first run.
pub fn config() -> io::Result<PathBuf> {
    Ok(state_dir()?.join("config.json"))
}
