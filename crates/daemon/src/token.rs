//! The shared secret that separates real hook deliveries from anything else that can
//! reach a loopback port.
//!
//! Binding to 127.0.0.1 is not by itself protection: any process on the machine, and any
//! web page the browser is told to open, can send requests there. The token is what makes
//! the endpoint uninteresting to them.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rand::Rng;

/// Hex-encoded 256-bit secret.
///
/// `rand::rng()` is a ChaCha-based CSPRNG seeded from the OS, not a fast non-crypto
/// generator - which matters, because this value is the only thing guarding the endpoint.
pub fn generate() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where the token lives.
///
/// `%LOCALAPPDATA%` is per-user and not roamed, and its default ACL already excludes
/// other non-administrator users - so the file inherits the protection we want without
/// hand-rolling Windows ACLs. An administrator can still read it, which is not a boundary
/// this tool can or should try to defend.
pub fn default_path() -> io::Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_STATE_HOME"))
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no LOCALAPPDATA, XDG_STATE_HOME or HOME")
        })?;
    Ok(PathBuf::from(base).join("MyTimeOff").join("hook-token"))
}

/// Reads the token, creating one on first run.
pub fn load_or_create(path: &Path) -> io::Result<String> {
    if let Ok(existing) = fs::read_to_string(path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let token = generate();
    fs::write(path, &token)?;
    Ok(token)
}

/// Compares without an early return on the first differing byte.
///
/// Length is allowed to leak; the contents are not. A naive `==` on a secret invites a
/// timing oracle, and the cost of avoiding it here is four lines.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_hex_and_unique() {
        let a = generate();
        let b = generate();
        assert_eq!(a.len(), 64, "256 bits as hex");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn comparison_accepts_only_an_exact_match() {
        assert!(constant_time_eq(b"abc123", b"abc123"));
        assert!(!constant_time_eq(b"abc123", b"abc124"));
        assert!(!constant_time_eq(b"abc123", b"abc12"));
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn a_token_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("mytimeoff-token-{}", generate()));
        let path = dir.join("hook-token");

        let first = load_or_create(&path).expect("creates on first run");
        let second = load_or_create(&path).expect("reuses on second run");
        assert_eq!(first, second, "rotating on restart would break wired hooks");

        fs::remove_dir_all(&dir).ok();
    }
}
