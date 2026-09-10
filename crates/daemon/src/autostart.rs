//! Starting when Windows starts, and being visibly able to stop.
//!
//! This tool takes the screen. A thing that takes the screen and starts itself must be
//! trivially stoppable by someone who has had enough of it, and stoppable *without* this
//! tool's cooperation - if the only way out is a command in this program, then a bug in
//! this program is a machine you cannot use.
//!
//! That is the whole argument for the mechanism here. `HKCU\...\Run` is one string in the
//! current user's own hive: no administrator, no service, no scheduled task, nothing that
//! survives the user deciding otherwise. More importantly it is the list Windows itself
//! shows in Task Manager's Startup apps tab, with a switch beside it. Someone who has
//! never read this file can turn it off in the place they already know to look, and this
//! module's job is to agree with that switch rather than to fight it.
//!
//! Which is why "is it on?" is two questions here, not one. The `Run` value can be
//! present while Windows has been told not to honour it, and a status that reported "on"
//! in that state would be a lie about the next reboot.

use std::io;

/// What Windows will do with one login item at the next sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginItem {
    /// The command line, or None if there is no entry at all.
    pub command: Option<String>,
    /// Whether Windows will actually run it. False means the entry exists but the user
    /// switched it off in Task Manager, which is a different state from "not installed"
    /// and is worth saying out loud.
    pub enabled: bool,
}

impl LoginItem {
    /// Nothing registered.
    pub fn absent() -> Self {
        LoginItem { command: None, enabled: false }
    }

    /// Whether this will actually start at the next sign-in.
    pub fn will_run(&self) -> bool {
        self.command.is_some() && self.enabled
    }
}

/// What Windows sees when it reads its own startup list.
pub fn read(name: &str) -> io::Result<LoginItem> {
    platform::read(name)
}

/// Registers a command to run at sign-in, replacing any entry under the same name.
///
/// It also clears the "user turned this off" record rather than overwriting it with an
/// "on" one. Asking for autostart is an explicit request and should take effect, but the
/// approval byte is Windows' bookkeeping and Windows should be the one to write it.
pub fn enable(name: &str, command: &str) -> io::Result<()> {
    platform::enable(name, command)
}

/// Removes the entry. Missing is not an error: the point is that it is gone.
pub fn disable(name: &str) -> io::Result<()> {
    platform::disable(name)
}

/// A command line that survives a path with a space in it.
///
/// `C:\Program Files\...` unquoted is read by Windows as a program called `C:\Program`
/// with an argument, which is the oldest bug on the platform. Quoting is unconditional
/// because a path that needs no quotes is not harmed by them.
fn quoted(program: &str) -> String {
    format!("\"{program}\"")
}

/// Whether Task Manager's switch is on, given that key's bytes.
///
/// The record is twelve bytes and only the first matters: bit 0 set means the user turned
/// it off. No record at all means nobody has ever touched the switch, which is on.
fn approved(record: Option<&[u8]>) -> bool {
    match record {
        None => true,
        Some(bytes) => bytes.first().is_none_or(|first| first & 1 == 0),
    }
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::ptr;

    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey,
        RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    };

    use super::{LoginItem, approved, quoted};

    /// The startup list itself.
    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    /// Task Manager's record of which of those the user has switched off. A separate key,
    /// because it is a separate decision: one is what was installed, the other is what the
    /// person wants today.
    const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

    pub fn read(name: &str) -> io::Result<LoginItem> {
        let Some(bytes) = value(RUN, name)? else {
            return Ok(LoginItem::absent());
        };
        Ok(LoginItem {
            command: text(&bytes),
            enabled: approved(value(APPROVED, name)?.as_deref()),
        })
    }

    pub fn enable(name: &str, command: &str) -> io::Result<()> {
        let command = quoted(command);
        let key = open(RUN, KEY_SET_VALUE)?;
        let name = wide(name);
        let data = wide(&command);
        // SAFETY: `key` is live until closed below, and both pointers borrow locals that
        // outlive the call. The length is in bytes, including the terminating NUL, which
        // is what REG_SZ is documented to want.
        let code = unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * size_of::<u16>()) as u32,
            )
        };
        close(key);
        checked(code)?;

        // Best effort, and deliberately not fatal: the entry is installed either way, and
        // a machine where this key cannot be written is one where Windows will make its
        // own record at the next sign-in.
        let _ = delete(APPROVED, &name);
        Ok(())
    }

    pub fn disable(name: &str) -> io::Result<()> {
        delete(RUN, &wide(name))
    }

    /// One value's raw bytes, or None if either the key or the value is missing.
    ///
    /// A missing key is not an error here. `StartupApproved` does not exist until
    /// something has been switched off for the first time, and treating its absence as a
    /// failure would make the common case the broken one.
    fn value(subkey: &str, name: &str) -> io::Result<Option<Vec<u8>>> {
        let key = match open(subkey, KEY_QUERY_VALUE) {
            Ok(key) => key,
            Err(error) if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let name = wide(name);

        let mut size = 0u32;
        // SAFETY: a null buffer with a live size out-pointer is the documented way to ask
        // how big the value is.
        let code = unsafe {
            RegQueryValueExW(key, name.as_ptr(), ptr::null(), ptr::null_mut(), ptr::null_mut(), &mut size)
        };
        if code == ERROR_FILE_NOT_FOUND {
            close(key);
            return Ok(None);
        }
        if code != ERROR_SUCCESS {
            close(key);
            return Err(io::Error::from_raw_os_error(code as i32));
        }

        let mut bytes = vec![0u8; size as usize];
        // SAFETY: `bytes` is exactly the size Windows just asked for, and `size` is
        // updated in place with how much was written.
        let code = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                bytes.as_mut_ptr(),
                &mut size,
            )
        };
        close(key);
        checked(code)?;
        bytes.truncate(size as usize);
        Ok(Some(bytes))
    }

    fn delete(subkey: &str, name: &[u16]) -> io::Result<()> {
        let key = match open(subkey, KEY_SET_VALUE) {
            Ok(key) => key,
            Err(error) if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        // SAFETY: `key` is live and `name` is a NUL-terminated wide string.
        let code = unsafe { RegDeleteValueW(key, name.as_ptr()) };
        close(key);
        if code == ERROR_FILE_NOT_FOUND { Ok(()) } else { checked(code) }
    }

    fn open(subkey: &str, access: u32) -> io::Result<HKEY> {
        let path = wide(subkey);
        let mut key: HKEY = ptr::null_mut();
        // SAFETY: `path` outlives the call and `key` is a valid out-pointer. On success
        // Windows hands back a handle that every path below closes.
        let code =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, access, &mut key) };
        checked(code)?;
        Ok(key)
    }

    fn close(key: HKEY) {
        // SAFETY: `key` came from a successful RegOpenKeyExW and is not used again.
        unsafe { RegCloseKey(key) };
    }

    fn checked(code: u32) -> io::Result<()> {
        if code == ERROR_SUCCESS { Ok(()) } else { Err(io::Error::from_raw_os_error(code as i32)) }
    }

    /// A REG_SZ blob as text, trimmed of the NUL the registry keeps and any it does not.
    fn text(blob: &[u8]) -> Option<String> {
        let units: Vec<u16> =
            blob.as_chunks::<2>().0.iter().copied().map(u16::from_le_bytes).collect();
        let text = String::from_utf16(&units).ok()?;
        let text = text.trim_end_matches('\0').to_string();
        (!text.is_empty()).then_some(text)
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

/// Everywhere else there is no startup list, so there is nothing to read and nothing to
/// write. The daemon still builds and runs; it just does not offer to start itself.
#[cfg(not(windows))]
mod platform {
    use std::io;

    use super::LoginItem;

    pub fn read(_name: &str) -> io::Result<LoginItem> {
        Ok(LoginItem::absent())
    }

    pub fn enable(_name: &str, _command: &str) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "no startup list here"))
    }

    pub fn disable(_name: &str) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_with_a_space_in_it_is_still_one_program() {
        assert_eq!(
            quoted(r"C:\Program Files\MyTimeOff\mytimeoff-shell.exe"),
            "\"C:\\Program Files\\MyTimeOff\\mytimeoff-shell.exe\"",
        );
    }

    #[test]
    fn a_switch_nobody_has_touched_is_on() {
        assert!(approved(None));
    }

    #[test]
    fn task_managers_switch_is_believed_in_both_positions() {
        // The first byte is the whole record as far as this is concerned: 2 and 6 are both
        // states Windows writes for "on", and 3 is what it writes when the user says no.
        assert!(approved(Some(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])));
        assert!(approved(Some(&[6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])));
        assert!(!approved(Some(&[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])));
    }

    #[test]
    fn an_empty_record_is_not_read_as_a_refusal() {
        // A truncated or empty value is Windows bookkeeping we do not understand. Reading
        // it as "off" would report a working install as broken.
        assert!(approved(Some(&[])));
    }

    #[test]
    fn an_entry_that_exists_but_is_switched_off_is_not_going_to_run() {
        let installed = LoginItem { command: Some("x".into()), enabled: true };
        let refused = LoginItem { command: Some("x".into()), enabled: false };
        assert!(installed.will_run());
        assert!(!refused.will_run());
        assert!(!LoginItem::absent().will_run());
    }

    /// The one thing the tests above cannot reach: whether the FFI is right.
    ///
    /// Ignored because it writes to the machine's real startup list. It uses a scratch
    /// name, never the real one, and removes what it wrote. Run it after touching
    /// anything in `platform`:
    ///
    /// ```text
    /// cargo test -p mytimeoff-daemon --lib autostart -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes to the real startup list"]
    fn an_entry_survives_a_round_trip_through_the_registry() {
        let name = format!("MyTimeOff test {}", std::process::id());
        assert_eq!(read(&name).expect("read"), LoginItem::absent(), "must start empty");

        let exe = r"C:\Program Files\MyTimeOff\mytimeoff-shell.exe";
        enable(&name, exe).expect("enable");
        let item = read(&name).expect("read back");
        assert_eq!(item.command.as_deref(), Some(format!("\"{exe}\"").as_str()));
        assert!(item.will_run());

        // Replacing, not accumulating.
        enable(&name, r"C:\other.exe").expect("re-enable");
        assert_eq!(read(&name).expect("read back").command.as_deref(), Some("\"C:\\other.exe\""));

        disable(&name).expect("disable");
        assert_eq!(read(&name).expect("read after disable"), LoginItem::absent());
        disable(&name).expect("disabling nothing is not an error");
    }
}
