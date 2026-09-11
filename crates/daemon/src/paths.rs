//! Where MyTimeOff keeps its things.
//!
//! One module decides this, because the token, the config and (next) the database all
//! have to agree, and because the reason for the location is a security argument that
//! should only be made once.

use std::io;
use std::path::{Path, PathBuf};

/// The folder name. The bundle identifier rather than the product name, and the reason is
/// the installer: Tauri installs a per-user app into `%LOCALAPPDATA%\<product name>`, so a
/// data directory called `MyTimeOff` is the *same directory the program is installed into*,
/// which files a 17MB executable next to someone's reading history. Worse, "uninstall by
/// deleting the folder" then takes the library with it.
///
/// Naming it after the identifier also earns something: it is exactly where the NSIS
/// uninstaller's "delete application data" box points, so that box starts telling the
/// truth instead of quietly doing nothing.
const DIRECTORY: &str = "com.mytimeoff.desktop";

/// What this used to be called. See [`migrate_legacy_state`].
const LEGACY_DIRECTORY: &str = "MyTimeOff";

/// The per-user directory holding everything this tool persists.
///
/// `%LOCALAPPDATA%` is per-user and not roamed, and its default ACL already excludes
/// other non-administrator users - so files inherit the protection we want without
/// hand-rolling Windows ACLs. An administrator can still read them, which is not a
/// boundary this tool can or should try to defend.
pub fn state_dir() -> io::Result<PathBuf> {
    Ok(base_dir()?.join(DIRECTORY))
}

fn base_dir() -> io::Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_STATE_HOME"))
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no LOCALAPPDATA, XDG_STATE_HOME or HOME")
        })?;
    Ok(PathBuf::from(base))
}

/// Moves a pre-0.1 state directory to where the state directory is now.
///
/// Only the files this tool wrote are moved, by name. The old directory is where the
/// installer now puts the program, so anything else in there is somebody else's - and
/// after an upgrade it is quite literally `MyTimeOff.exe`.
///
/// Nothing here is fatal, which is why it returns something to print rather than an error
/// to propagate: an upgrade must not brick the app over a locked file. What it will not do
/// is move *some* of the files - see [`move_state`].
pub fn migrate_legacy_state() -> Option<String> {
    let base = base_dir().ok()?;
    let legacy = base.join(LEGACY_DIRECTORY);
    match move_state(&legacy, &base.join(DIRECTORY)) {
        Ok(true) => Some(format!("moved your settings and books out of {}", legacy.display())),
        Ok(false) => None,
        Err(why) => Some(format!(
            "your settings are still in {} and could not be moved ({why}).\n\
             Close any other copy of MyTimeOff and start this one again.",
            legacy.display()
        )),
    }
}

/// The files this tool writes, and the only ones a migration will touch.
///
/// The write-ahead log and the shared-memory file travel with the database, or the
/// database that arrives is the one from before the last commit.
const STATE_FILES: [&str; 5] = [
    "config.json",
    "hook-token",
    "mytimeoff.sqlite3",
    "mytimeoff.sqlite3-wal",
    "mytimeoff.sqlite3-shm",
];

/// The move itself, told where to move from and to so it can be tested on a machine whose
/// real state directory must not be disturbed.
///
/// Refuses outright in three cases, each of which would do harm: no old config to move, a
/// new config that already exists (moving onto it would overwrite live settings with stale
/// ones), and the two being the same directory.
///
/// It is all or nothing. A half-move is the one outcome worse than not moving at all: the
/// settings arrive, the database does not, and the next launch reads the new config beside
/// an empty library - a person whose reading history just vanished, with no second attempt
/// coming, because the config that would have triggered it is already here. So a file that
/// will not move puts every earlier one back and reports why.
///
/// Rename only, never copy. Both directories are children of one base directory, so the
/// cross-volume failure a copy would rescue cannot happen; what does happen is a lock,
/// held by another copy of this program that is still writing. Copying past that lock
/// forks the library in two and lets both halves drift.
fn move_state(legacy: &Path, current: &Path) -> io::Result<bool> {
    if legacy == current
        || !legacy.join("config.json").is_file()
        || current.join("config.json").is_file()
    {
        return Ok(false);
    }

    std::fs::create_dir_all(current)?;
    let mut moved: Vec<&str> = Vec::new();
    for name in STATE_FILES {
        let from = legacy.join(name);
        if !from.exists() {
            continue;
        }
        if let Err(why) = std::fs::rename(&from, current.join(name)) {
            for done in &moved {
                // Best effort by necessity: there is nothing left to try if putting a file
                // back also fails, and saying so would only bury the failure that matters.
                let _ = std::fs::rename(current.join(done), legacy.join(done));
            }
            return Err(io::Error::new(why.kind(), format!("{name}: {why}")));
        }
        moved.push(name);
    }

    Ok(!moved.is_empty())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that removes itself. Two of these stand in for the old and new
    /// state directories, so no test touches the real `%LOCALAPPDATA%`.
    struct Temp(PathBuf);

    impl Temp {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "mytimeoff-paths-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }

        fn write(&self, name: &str, body: &str) {
            std::fs::write(self.0.join(name), body).expect("write");
        }

        fn read(&self, name: &str) -> Option<String> {
            std::fs::read_to_string(self.0.join(name)).ok()
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn moves_the_files_this_tool_wrote() {
        let legacy = Temp::new("legacy-moves");
        let current = Temp::new("current-moves");
        legacy.write("config.json", "{\"mode\":\"strict\"}");
        legacy.write("hook-token", "secret");
        legacy.write("mytimeoff.sqlite3", "db");
        legacy.write("mytimeoff.sqlite3-wal", "wal");
        legacy.write("mytimeoff.sqlite3-shm", "shm");

        assert!(move_state(&legacy.0, &current.0).expect("migrate"));

        for name in STATE_FILES {
            assert!(current.read(name).is_some(), "{name} should have arrived");
            assert!(legacy.read(name).is_none(), "{name} should have left");
        }
        assert_eq!(current.read("config.json").as_deref(), Some("{\"mode\":\"strict\"}"));
    }

    #[test]
    fn leaves_a_live_config_alone() {
        let legacy = Temp::new("legacy-live");
        let current = Temp::new("current-live");
        legacy.write("config.json", "old");
        legacy.write("hook-token", "old token");
        current.write("config.json", "new");

        assert!(!move_state(&legacy.0, &current.0).expect("migrate"));

        // Not just the config: a half-migration that took the token while leaving the
        // settings would point the new install at the wrong secret.
        assert_eq!(current.read("config.json").as_deref(), Some("new"));
        assert!(current.read("hook-token").is_none());
        assert_eq!(legacy.read("hook-token").as_deref(), Some("old token"));
    }

    #[test]
    fn leaves_everything_else_where_it_found_it() {
        let legacy = Temp::new("legacy-others");
        let current = Temp::new("current-others");
        legacy.write("config.json", "{}");
        // The old state directory is the directory the installer now writes into.
        legacy.write("MyTimeOff.exe", "not ours to move");
        legacy.write("uninstall.exe", "definitely not ours");

        assert!(move_state(&legacy.0, &current.0).expect("migrate"));

        assert_eq!(legacy.read("MyTimeOff.exe").as_deref(), Some("not ours to move"));
        assert_eq!(legacy.read("uninstall.exe").as_deref(), Some("definitely not ours"));
        assert!(current.read("MyTimeOff.exe").is_none());
    }

    #[test]
    fn does_nothing_without_an_old_config() {
        let legacy = Temp::new("legacy-empty");
        let current = Temp::new("current-empty");
        // A stray database with no config is not a state directory worth claiming.
        legacy.write("mytimeoff.sqlite3", "db");

        assert!(!move_state(&legacy.0, &current.0).expect("migrate"));
        assert!(current.read("mytimeoff.sqlite3").is_none());
    }

    #[test]
    fn puts_everything_back_when_one_file_will_not_move() {
        let legacy = Temp::new("legacy-locked");
        let current = Temp::new("current-locked");
        legacy.write("config.json", "{}");
        legacy.write("hook-token", "secret");
        legacy.write("mytimeoff.sqlite3", "db");
        // Standing in for the real obstruction, which is a second copy of this program
        // holding the database open: a destination that cannot be renamed onto.
        std::fs::create_dir(current.0.join("mytimeoff.sqlite3")).expect("blocker");

        let why = move_state(&legacy.0, &current.0).expect_err("should refuse");
        assert!(why.to_string().contains("mytimeoff.sqlite3"), "{why}");

        // The point of the rollback: the old directory is intact, so the next launch sees
        // the same starting position and tries again.
        assert_eq!(legacy.read("config.json").as_deref(), Some("{}"));
        assert_eq!(legacy.read("hook-token").as_deref(), Some("secret"));
        assert_eq!(legacy.read("mytimeoff.sqlite3").as_deref(), Some("db"));
        assert!(current.read("config.json").is_none());
        assert!(current.read("hook-token").is_none());
    }

    #[test]
    fn does_nothing_when_the_two_are_the_same_directory() {
        let dir = Temp::new("same");
        dir.write("config.json", "{}");

        assert!(!move_state(&dir.0, &dir.0).expect("migrate"));
        assert_eq!(dir.read("config.json").as_deref(), Some("{}"));
    }
}
