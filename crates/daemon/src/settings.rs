//! Loading the user's config off disk.
//!
//! The parsing rules live in `mytimeoff-core`; this is only the part that touches a
//! filesystem, kept separate so the rules stay testable without one.

use std::fs;
use std::io;
use std::path::Path;

use mytimeoff_core::Config;

use crate::text;

/// Reads the config, writing the defaults out on first run.
///
/// Writing the file rather than keeping the defaults in memory is deliberate: a config
/// you cannot see is a config you cannot edit, and the whole point of these knobs is that
/// they get turned.
pub fn load_or_create(path: &Path) -> io::Result<Config> {
    match text::read(path) {
        Ok(text) => Config::parse(&text).map_err(|error| {
            // Refusing to start beats starting on settings the user did not choose:
            // silently falling back to strict mode after a typo would gate their exit
            // with a quiz they never asked for.
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is not valid: {error}", path.display()),
            )
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let config = Config::default();
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, config.to_json())?;
            Ok(config)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use mytimeoff_core::ReaderMode;

    use super::*;

    /// A unique path under the OS temp dir. Enough isolation for these three tests, and
    /// cheaper than a dev-dependency on a tempdir crate.
    fn scratch(name: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        env::temp_dir().join(format!("mytimeoff-{name}-{unique}")).join("config.json")
    }

    #[test]
    fn first_run_writes_the_defaults_where_they_can_be_edited() {
        let path = scratch("first-run");
        let config = load_or_create(&path).expect("first run");

        assert_eq!(config, Config::default());
        let written = fs::read_to_string(&path).expect("config written");
        assert!(written.contains("\"mode\": \"strict\""), "{written}");
        assert!(written.contains('\n'), "hand-edited files are written pretty: {written}");

        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn an_existing_file_wins_over_the_defaults() {
        let path = scratch("existing");
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, r#"{"mode":"free","grace_ms":3}"#).expect("write");

        let config = load_or_create(&path).expect("load");
        assert_eq!(config.mode, ReaderMode::Free);
        assert_eq!(config.grace_ms, 3);

        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    /// The file the README tells people to edit, saved by something that marks it.
    ///
    /// This one is worth a test of its own rather than trusting `text`: the failure it
    /// guards against is the daemon refusing to start at all, and in the window there is
    /// no console for it to say so on.
    #[test]
    fn a_config_saved_with_a_byte_order_mark_still_loads() {
        let path = scratch("bom");
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, "\u{feff}{\"mode\":\"free\",\"grace_ms\":3}").expect("write");

        let config = load_or_create(&path).expect("a mark is not a syntax error");
        assert_eq!(config.mode, ReaderMode::Free);

        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn a_broken_file_stops_the_daemon_rather_than_being_overwritten() {
        let path = scratch("broken");
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, "{ not json").expect("write");

        let error = load_or_create(&path).expect_err("must refuse");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        // The user's file is still theirs. Overwriting it with defaults would destroy
        // the settings they were trying to fix.
        assert_eq!(fs::read_to_string(&path).expect("untouched"), "{ not json");

        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }
}
