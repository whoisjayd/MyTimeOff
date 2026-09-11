//! Reading files a person may have opened in an editor.
//!
//! Everything this tool parses off disk - its own config, and both agents' settings - is
//! a file somebody is invited to edit by hand, and hand-editing on Windows has a way of
//! leaving a byte order mark on the front. Notepad will write one if asked, and
//! PowerShell 5.1's `Set-Content -Encoding utf8` writes one whether asked or not. A BOM
//! is not whitespace: `serde_json`, `toml_edit` and this crate's own parser all see a
//! stray `\u{feff}` before the opening brace and call the file malformed.
//!
//! What that costs is out of proportion to what it is. A BOM on `config.json` stops the
//! daemon before it can say why, and the window is a GUI process with nowhere to print -
//! which is a program that does not start and does not explain itself. A BOM on an
//! agent's settings blocks Connect with a message about invalid JSON, pointing at a file
//! that looks perfectly valid in the editor the mark came from.
//!
//! So it is stripped once, here, rather than argued about at three call sites. Only a
//! *leading* mark, and only one: a `\u{feff}` anywhere else is real content, and a file
//! carrying two of them is not something to quietly repair.

use std::fs;
use std::io;
use std::path::Path;

/// The UTF-8 encoding of U+FEFF, as `read_to_string` decodes it.
const BOM: char = '\u{feff}';

/// Reads a file as text, without the mark an editor may have put on the front.
///
/// The error is [`fs::read_to_string`]'s own, kind and all. Callers tell a missing file
/// apart from an unreadable one by that kind, and a wrapper that flattened the two would
/// turn "you have not made one yet" into "yours is broken".
pub fn read(path: &Path) -> io::Result<String> {
    fs::read_to_string(path).map(|text| strip(&text).to_string())
}

/// Removes one leading byte order mark.
fn strip(text: &str) -> &str {
    text.strip_prefix(BOM).unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leading_mark_is_not_content() {
        assert_eq!(strip("\u{feff}{\"mode\":\"free\"}"), "{\"mode\":\"free\"}");
    }

    #[test]
    fn a_file_without_one_is_left_exactly_as_it_was() {
        assert_eq!(strip("{\"mode\":\"free\"}"), "{\"mode\":\"free\"}");
        assert_eq!(strip(""), "");
    }

    /// Two marks is not a file this should be silently repairing, and a mark in the
    /// middle of a line is somebody's text.
    #[test]
    fn only_the_first_one_and_only_at_the_front() {
        assert_eq!(strip("\u{feff}\u{feff}x"), "\u{feff}x");
        assert_eq!(strip("x\u{feff}y"), "x\u{feff}y");
    }

    #[test]
    fn a_missing_file_still_reports_itself_as_missing() {
        let missing = std::env::temp_dir().join("mytimeoff-no-such-file-6f2a1c.json");
        let error = read(&missing).expect_err("no such file");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
