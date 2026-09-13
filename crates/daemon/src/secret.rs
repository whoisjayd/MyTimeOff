//! Where the API key lives, which is deliberately not in this project's files.
//!
//! The config is written out on first run so it can be hand-edited, and it sits in a
//! directory a user will happily paste into a bug report. A key in there would be a key
//! in a screenshot, a backup and a support thread. Windows already has a place for this
//! that is encrypted per-user and that other processes cannot read, so the key goes
//! there and nowhere else.
//!
//! The environment is honoured as a second place to look, because it is how every other
//! tool for these APIs is configured and because CI has no credential store. It is a
//! fallback, not the preferred home: a deliberately stored credential wins over an
//! inherited one.
//!
//! Which name belongs to which provider is not decided here - `quiz::Provider` owns that,
//! and this module only knows how to look a name up.

use std::env;
use std::io;

/// A key, from the credential store or the environment, or None if there is neither.
///
/// A missing key is not an error. It is a working install that makes its own questions
/// offline, and the daemon says so at startup rather than failing to start.
pub fn find(target: &str, variables: &[&str]) -> Option<String> {
    match read(target) {
        Ok(Some(key)) => return Some(key),
        Ok(None) => {}
        // Reported rather than propagated: a credential store that will not answer is a
        // reason to fall back, not a reason to refuse to run.
        Err(error) => eprintln!("credential store: {error}"),
    }
    variables
        .iter()
        .filter_map(|name| env::var(name).ok())
        .map(|key| key.trim().to_string())
        .find(|key| !key.is_empty())
}

/// Reads one generic credential by name.
pub fn read(target: &str) -> io::Result<Option<String>> {
    platform::read(target)
}

/// Stores one generic credential by name, replacing any that was there.
pub fn write(target: &str, secret: &str) -> io::Result<()> {
    platform::write(target, secret)
}

/// Forgets one credential. Missing is not an error: the point is that it is gone.
pub fn delete(target: &str) -> io::Result<()> {
    platform::delete(target)
}

/// Turns a credential blob back into text.
///
/// The blob is bytes, and what wrote it decided what they mean. This daemon writes
/// UTF-8; PowerShell's `New-StoredCredential` and most of the .NET wrappers write
/// UTF-16LE. Both are read here, so a key stored by whichever tool the user reached for
/// first still works. An API key is ASCII, which is what makes the two tellable apart:
/// UTF-16LE puts a zero byte after every character, and UTF-8 never does.
fn decode(blob: &[u8]) -> Option<String> {
    if blob.is_empty() {
        return None;
    }
    let utf16 = blob.len().is_multiple_of(2) && blob[1] == 0;
    let text = if utf16 {
        let units: Vec<u16> =
            blob.as_chunks::<2>().0.iter().copied().map(u16::from_le_bytes).collect();
        String::from_utf16(&units).ok()?
    } else {
        String::from_utf8(blob.to_vec()).ok()?
    };
    // Trailing NULs are common in blobs written as C strings; a stray newline is common
    // in blobs written by a person.
    let text = text.trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::slice;

    use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
        CredReadW, CredWriteW,
    };

    use super::decode;

    pub fn read(target: &str) -> io::Result<Option<String>> {
        let name = wide(target);
        let mut credential: *mut CREDENTIALW = std::ptr::null_mut();

        // SAFETY: `name` is a NUL-terminated wide string that outlives the call, and
        // `credential` is a valid out-pointer. On success Windows hands back an
        // allocation that is freed below on every path.
        let found = unsafe { CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
        if found == 0 {
            let code = unsafe { GetLastError() };
            return if code == ERROR_NOT_FOUND {
                Ok(None)
            } else {
                Err(io::Error::from_raw_os_error(code as i32))
            };
        }

        // SAFETY: CredReadW returned success, so `credential` points at a live
        // CREDENTIALW whose blob is `CredentialBlobSize` bytes long.
        let secret = unsafe {
            let blob = (*credential).CredentialBlob;
            let size = (*credential).CredentialBlobSize as usize;
            let bytes = if blob.is_null() || size == 0 {
                &[][..]
            } else {
                slice::from_raw_parts(blob, size)
            };
            let decoded = decode(bytes);
            CredFree(credential.cast());
            decoded
        };
        Ok(secret)
    }

    pub fn write(target: &str, secret: &str) -> io::Result<()> {
        let mut name = wide(target);
        let blob = secret.as_bytes();

        // SAFETY: CREDENTIALW is plain data with no invalid bit patterns, and every
        // pointer field left zeroed is documented as optional.
        let mut credential: CREDENTIALW = unsafe { std::mem::zeroed() };
        credential.Type = CRED_TYPE_GENERIC;
        credential.TargetName = name.as_mut_ptr();
        credential.CredentialBlobSize = blob.len() as u32;
        credential.CredentialBlob = blob.as_ptr().cast_mut();
        // Local, not roaming: an API key should not follow the user onto another machine
        // without them deciding to put it there.
        credential.Persist = CRED_PERSIST_LOCAL_MACHINE;

        // SAFETY: every pointer in `credential` borrows a local that outlives the call.
        let ok = unsafe { CredWriteW(&credential, 0) };
        if ok == 0 {
            return Err(io::Error::from_raw_os_error(unsafe { GetLastError() } as i32));
        }
        Ok(())
    }

    pub fn delete(target: &str) -> io::Result<()> {
        let name = wide(target);
        // SAFETY: `name` is a NUL-terminated wide string that outlives the call.
        let ok = unsafe { CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) };
        if ok == 0 {
            let code = unsafe { GetLastError() };
            if code != ERROR_NOT_FOUND {
                return Err(io::Error::from_raw_os_error(code as i32));
            }
        }
        Ok(())
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

/// Everywhere else there is no credential store, so there is nothing to read and nothing
/// to write. `api_key` still finds the environment, which is how the daemon builds and
/// runs on a machine that is not the one it is for.
#[cfg(not(windows))]
mod platform {
    use std::io;

    pub fn read(_target: &str) -> io::Result<Option<String>> {
        Ok(None)
    }

    pub fn write(_target: &str, _secret: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no credential store here; set the API key in the environment instead",
        ))
    }

    pub fn delete(_target: &str) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_written_as_utf8_reads_back() {
        assert_eq!(decode(b"sk-ant-abc123"), Some("sk-ant-abc123".to_string()));
    }

    #[test]
    fn a_key_written_as_utf16_by_something_else_reads_back() {
        let blob: Vec<u8> =
            "sk-ant-abc123".encode_utf16().flat_map(|unit| unit.to_le_bytes()).collect();
        assert_eq!(decode(&blob), Some("sk-ant-abc123".to_string()));
    }

    #[test]
    fn padding_a_person_or_a_c_string_left_behind_is_trimmed() {
        assert_eq!(decode(b"sk-ant-abc123\r\n"), Some("sk-ant-abc123".to_string()));
        assert_eq!(decode(b"sk-ant-abc123\0"), Some("sk-ant-abc123".to_string()));
    }

    /// The one thing the tests above cannot reach: whether the FFI is right.
    ///
    /// Ignored because it writes to the machine's real credential store, which is not
    /// something a plain `cargo test` should do. It uses a scratch name, never the key's,
    /// and deletes what it wrote. Run it after touching anything in `platform`:
    ///
    /// ```text
    /// cargo test -p mytimeoff-daemon --lib secret -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes to the real credential store"]
    fn a_credential_survives_a_round_trip_through_windows() {
        let target = format!("MyTimeOff/test-{}", std::process::id());
        assert_eq!(read(&target).expect("read"), None, "the scratch name must start empty");

        write(&target, "sk-ant-not-a-real-key").expect("write");
        assert_eq!(read(&target).expect("read back"), Some("sk-ant-not-a-real-key".to_string()));

        // Replacing, not accumulating: storing a new key must not leave the old one.
        write(&target, "sk-ant-second").expect("overwrite");
        assert_eq!(read(&target).expect("read back"), Some("sk-ant-second".to_string()));

        delete(&target).expect("delete");
        assert_eq!(read(&target).expect("read after delete"), None);
        delete(&target).expect("deleting nothing is not an error");
    }

    #[test]
    fn a_name_nothing_has_stored_and_nothing_exports_is_simply_absent() {
        // Read-only on purpose: the variables are made up so no machine's real setup can
        // make this pass or fail, and nothing here writes to the store.
        let target = format!("MyTimeOff/absent-{}", std::process::id());
        assert_eq!(find(&target, &["MYTIMEOFF_NO_SUCH_KEY_A", "MYTIMEOFF_NO_SUCH_KEY_B"]), None);
    }

    #[test]
    fn nothing_is_nothing_rather_than_an_empty_key() {
        // An empty key would be sent as a header and refused by the API on every gate.
        // Better to have no key and know it.
        assert_eq!(decode(b""), None);
        assert_eq!(decode(b"   "), None);
    }
}
