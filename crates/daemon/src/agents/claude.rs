//! Putting MyTimeOff into Claude Code's settings, and taking it back out.
//!
//! Claude Code announces what an agent is doing by POSTing to a URL; this daemon is that
//! URL. The wiring is a few lines of JSON per event - short enough that the README could
//! just print them and leave it there, which is what every version of this before now did.
//!
//! It is here as code instead because of where those lines have to go: a file the user
//! owns, probably hand-edited, holding settings that have nothing to do with this tool and
//! must survive it untouched. Pasting is how people lose that file. So the rule this
//! module is built around is that it adds what it needs, removes exactly what it added,
//! and leaves every other byte of meaning alone.
//!
//! `codex.rs` beside this one obeys the same rule against a different file in a different
//! format. What they share is in `mod.rs`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{Outcome, Status, Wired};

/// The events MyTimeOff listens for.
///
/// `UserPromptSubmit` is a turn starting - the moment the dead time begins.
/// `Stop` is that turn ending, which is what turns the indicator on.
/// `Notification` is the agent stopping to ask for something, which ends the dead time
/// just as truly as finishing does, and without it a turn that pauses for permission
/// would leave someone reading a book while their agent waited.
pub const EVENTS: [&str; 3] = ["UserPromptSubmit", "Stop", "Notification"];

/// The name of the copy kept beside the settings before each change.
///
/// Overwritten every time rather than written once. A backup that is always "the file as
/// it was a moment ago" is worth restoring; one left over from months back is a trap
/// wearing the same name.
pub const BACKUP: &str = "settings.json.mytimeoff-backup";

/// `~/.claude/settings.json`: the settings Claude Code applies in every project.
///
/// Global rather than per-project, because the dead time is not a property of a
/// repository. It happens wherever the agent is working, and wiring each checkout
/// separately is asking someone to forget.
///
/// `CLAUDE_CONFIG_DIR` moves that whole directory and Claude Code honours it, so this
/// does too - otherwise a machine that had moved it would get a perfectly good hook
/// written to a file nothing reads.
pub fn settings_path() -> io::Result<PathBuf> {
    if let Some(moved) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Ok(PathBuf::from(moved).join("settings.json"));
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no USERPROFILE or HOME, so there is no way to find .claude/settings.json",
            )
        })?;
    Ok(PathBuf::from(home).join(".claude").join("settings.json"))
}

/// Where the hooks point.
pub fn url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/hook")
}

/// One hook entry: post to the daemon, and do not wait for it.
///
/// `async` is the field that matters. A synchronous hook makes Claude Code wait on this
/// daemon at every prompt, so the day someone closes MyTimeOff their agent gets slower
/// and nothing on screen says why. Asynchronous, a daemon that is not running costs a
/// failed connection nobody sees.
fn entry(port: u16, token: &str) -> Value {
    json!({
        "type": "http",
        "url": url(port),
        "headers": { "Authorization": format!("Bearer {token}") },
        "async": true,
        "timeout": 5
    })
}

/// Did this tool write this entry?
///
/// There is nowhere to leave a marker. A hook entry has the fields Claude Code defines
/// and an extra one of ours would be a field in someone else's schema, which is exactly
/// the kind of liberty this module exists not to take. So the address is the signature:
/// an HTTP hook aimed at `/hook` on this machine is this tool's work.
///
/// Deliberately not the port. Change the port in the config, run `hooks install` again,
/// and matching on the whole URL would leave the old entry behind forever - a hook
/// pointing at nothing, in a file the user is not expected to read.
fn is_ours(entry: &Value) -> bool {
    entry.get("type").and_then(Value::as_str) == Some("http")
        && entry.get("url").and_then(Value::as_str).is_some_and(is_our_url)
}

fn is_our_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let Some((authority, path)) = rest.split_once('/') else {
        return false;
    };
    path == "hook" && matches!(host_of(authority), "127.0.0.1" | "localhost" | "[::1]")
}

/// The host out of `127.0.0.1:8787`, and out of `[::1]:8787` - which has colons of its
/// own, so splitting on the first one is only right because the brackets come first.
fn host_of(authority: &str) -> &str {
    match authority.find(']') {
        Some(end) => &authority[..=end],
        None => authority.split(':').next().unwrap_or(authority),
    }
}

/// Every MyTimeOff hook in these settings, in the order the events are listed.
pub fn found(settings: &Value, token: &str) -> Vec<Wired> {
    let expected = format!("Bearer {token}");
    let mut wired = Vec::new();
    for event in EVENTS {
        for entry in entries(settings, event) {
            if !is_ours(entry) {
                continue;
            }
            let carried =
                entry.pointer("/headers/Authorization").and_then(Value::as_str).unwrap_or("");
            wired.push(Wired {
                event,
                target: entry["url"].as_str().unwrap_or_default().to_string(),
                current: carried == expected,
            });
        }
    }
    wired
}

/// Every hook entry registered for one event, whoever wrote it.
fn entries<'a>(settings: &'a Value, event: &str) -> impl Iterator<Item = &'a Value> {
    settings
        .pointer("/hooks")
        .and_then(|hooks| hooks.get(event))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|group| group.get("hooks"))
        .filter_map(Value::as_array)
        .flatten()
}

/// Puts one MyTimeOff hook on each event, replacing any that were already there.
///
/// Replacing rather than adding, so that running this twice leaves one hook and not two.
/// Everything else in the file - other hooks on the same events included - is carried
/// through untouched.
pub fn wire(settings: &mut Value, port: u16, token: &str) -> io::Result<()> {
    unwire(settings)?;

    let root = settings.as_object_mut().ok_or_else(|| {
        malformed("the settings are not a JSON object, so there is nowhere to put hooks")
    })?;
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| malformed("\"hooks\" is there but is not a JSON object"))?;

    for event in EVENTS {
        let groups = hooks.entry(event).or_insert_with(|| json!([]));
        let groups = groups
            .as_array_mut()
            .ok_or_else(|| malformed(&format!("\"hooks\" -> \"{event}\" is not a list")))?;
        groups.push(json!({ "hooks": [entry(port, token)] }));
    }
    Ok(())
}

/// Takes every MyTimeOff hook back out, and says how many there were.
///
/// A group left holding nothing goes too, but only if it was this tool that emptied it:
/// an empty group somebody else wrote is somebody else's to explain.
pub fn unwire(settings: &mut Value) -> io::Result<usize> {
    let Some(hooks) = settings.get_mut("hooks") else {
        return Ok(0);
    };
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| malformed("\"hooks\" is there but is not a JSON object"))?;

    let mut removed = 0;
    for event in EVENTS {
        let Some(groups) = hooks.get_mut(event) else {
            continue;
        };
        let groups = groups
            .as_array_mut()
            .ok_or_else(|| malformed(&format!("\"hooks\" -> \"{event}\" is not a list")))?;

        let mut emptied = Vec::new();
        for (at, group) in groups.iter_mut().enumerate() {
            let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            let before = list.len();
            list.retain(|entry| !is_ours(entry));
            if list.len() < before {
                removed += before - list.len();
                if list.is_empty() {
                    emptied.push(at);
                }
            }
        }
        for at in emptied.into_iter().rev() {
            groups.remove(at);
        }
        if groups.is_empty() {
            hooks.remove(event);
        }
    }

    if hooks.is_empty() {
        settings.as_object_mut().expect("checked above").remove("hooks");
    }
    Ok(removed)
}

/// What Claude Code's settings say about MyTimeOff right now.
///
/// A file that will not parse is reported rather than raised. Somebody with a broken
/// settings file still deserves to be told which file and why, and a panel that showed
/// nothing at all would leave them guessing.
pub fn status(token: &str) -> io::Result<Status> {
    let path = settings_path()?;
    let present = path.parent().is_some_and(Path::is_dir);
    match read(&path) {
        Ok(settings) => {
            Ok(Status { path, present, wired: found(&settings, token), trouble: None })
        }
        Err(error) => {
            Ok(Status { path, present, wired: Vec::new(), trouble: Some(error.to_string()) })
        }
    }
}

/// Puts MyTimeOff into Claude Code's settings.
pub fn install(port: u16, token: &str) -> io::Result<Outcome> {
    let path = settings_path()?;
    let replaced = path.is_file();
    let mut settings = read(&path)?;
    wire(&mut settings, port, token)?;
    write(&path, &settings)?;
    Ok(Outcome { path, replaced, backup: BACKUP })
}

/// Takes MyTimeOff back out, and says how many hooks there were.
///
/// Nothing is written when there was nothing to remove: a `remove` on a machine that was
/// never wired should not reformat a settings file, nor leave a backup of it.
pub fn remove() -> io::Result<usize> {
    let path = settings_path()?;
    let mut settings = read(&path)?;
    let removed = unwire(&mut settings)?;
    if removed > 0 {
        write(&path, &settings)?;
    }
    Ok(removed)
}

/// Reads the settings, treating a file that is not there as an empty set of them.
///
/// A file that is there and unreadable is a different matter and stops everything. The
/// alternative - starting from `{}` and writing that back - would delete settings this
/// tool was only ever asked to add to.
pub fn read(path: &Path) -> io::Result<Value> {
    let text = match crate::text::read(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(json!({})),
        Err(error) => return Err(error),
    };
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&text).map_err(|error| {
        malformed(&format!(
            "{} is not valid JSON ({error}), and this refuses to write over a file it \
             cannot read",
            path.display()
        ))
    })
}

/// Writes the settings back, keeping a copy of what was there first.
///
/// Through a temporary file and a rename, because the failure this guards against is not
/// hypothetical: a write interrupted halfway leaves a settings file that parses as
/// nothing, and Claude Code would then start with no settings at all. A rename either
/// happened or did not.
pub fn write(path: &Path, settings: &Value) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        if path.is_file() {
            fs::copy(path, parent.join(BACKUP))?;
        }
    }

    let mut text = serde_json::to_string_pretty(settings).map_err(io::Error::other)?;
    text.push('\n');

    let temporary = path.with_extension("json.mytimeoff-new");
    fs::write(&temporary, text)?;
    fs::rename(&temporary, path)
}

fn malformed(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "abc123";

    fn wired() -> Value {
        let mut settings = json!({});
        wire(&mut settings, 8787, TOKEN).expect("wire");
        settings
    }

    #[test]
    fn wiring_an_empty_file_covers_every_event() {
        let settings = wired();
        for event in EVENTS {
            let ours: Vec<_> = entries(&settings, event).filter(|e| is_ours(e)).collect();
            assert_eq!(ours.len(), 1, "{event} should have exactly one MyTimeOff hook");
            assert_eq!(ours[0]["url"], "http://127.0.0.1:8787/hook");
            assert_eq!(ours[0]["headers"]["Authorization"], "Bearer abc123");
            assert_eq!(ours[0]["async"], true, "a synchronous hook would stall every prompt");
        }
    }

    /// The whole reason this is a command and not a paragraph in the README.
    #[test]
    fn everything_that_was_already_there_is_still_there() {
        let mut settings = json!({
            "model": "opus",
            "permissions": { "allow": ["Bash(git status)"] },
            "hooks": {
                "Stop": [{ "matcher": "*", "hooks": [{ "type": "command", "command": "say done" }] }],
                "PreToolUse": [{ "hooks": [{ "type": "command", "command": "lint" }] }]
            }
        });
        let before = settings.clone();

        wire(&mut settings, 8787, TOKEN).expect("wire");

        assert_eq!(settings["model"], before["model"]);
        assert_eq!(settings["permissions"], before["permissions"]);
        assert_eq!(settings["hooks"]["PreToolUse"], before["hooks"]["PreToolUse"]);
        assert_eq!(
            settings["hooks"]["Stop"][0], before["hooks"]["Stop"][0],
            "somebody else's Stop hook, with its matcher, untouched"
        );

        unwire(&mut settings).expect("unwire");
        assert_eq!(settings, before, "removing must put the file back exactly");
    }

    #[test]
    fn wiring_twice_leaves_one_hook_not_two() {
        let mut settings = wired();
        wire(&mut settings, 9000, TOKEN).expect("second wire");

        let ours: Vec<_> = entries(&settings, "Stop").filter(|e| is_ours(e)).collect();
        assert_eq!(ours.len(), 1);
        assert_eq!(ours[0]["url"], "http://127.0.0.1:9000/hook", "the new port wins");
    }

    /// Someone changes the port, then asks for the hooks to be removed. Matching on the
    /// full URL instead of the path would leave all three behind.
    #[test]
    fn a_hook_wired_to_another_port_is_still_ours() {
        let mut settings = json!({});
        wire(&mut settings, 1234, TOKEN).expect("wire");

        assert_eq!(unwire(&mut settings).expect("unwire"), 3);
        assert_eq!(settings, json!({}), "and the empty scaffolding goes with them");
    }

    #[test]
    fn an_http_hook_somewhere_else_is_not_ours() {
        for url in [
            "http://example.com/hook",
            "https://127.0.0.1:8787/hook",
            "http://127.0.0.1:8787/quiz",
            "http://10.0.0.5:8787/hook",
        ] {
            assert!(!is_ours(&json!({ "type": "http", "url": url })), "{url}");
        }
        assert!(!is_ours(&json!({ "type": "command", "command": "curl 127.0.0.1/hook" })));
    }

    #[test]
    fn loopback_has_more_than_one_spelling() {
        for url in ["http://localhost:8787/hook", "http://[::1]:8787/hook", "http://127.0.0.1/hook"]
        {
            assert!(is_ours(&json!({ "type": "http", "url": url })), "{url}");
        }
    }

    #[test]
    fn removing_what_is_not_there_changes_nothing() {
        let mut settings = json!({ "model": "opus" });
        assert_eq!(unwire(&mut settings).expect("unwire"), 0);
        assert_eq!(settings, json!({ "model": "opus" }));
    }

    /// A group that was empty before this ran is not ours to tidy away.
    #[test]
    fn an_empty_group_somebody_else_left_stays() {
        let mut settings = json!({ "hooks": { "Stop": [{ "hooks": [] }] } });
        let before = settings.clone();
        assert_eq!(unwire(&mut settings).expect("unwire"), 0);
        assert_eq!(settings, before);
    }

    /// Ours sharing a group with somebody else's: the group survives, minus one entry.
    #[test]
    fn a_shared_group_keeps_its_other_hooks() {
        let mut settings = json!({
            "hooks": { "Stop": [{ "hooks": [
                { "type": "http", "url": "http://127.0.0.1:8787/hook" },
                { "type": "command", "command": "say done" }
            ] }] }
        });
        assert_eq!(unwire(&mut settings).expect("unwire"), 1);
        assert_eq!(
            settings,
            json!({ "hooks": { "Stop": [{ "hooks": [
                { "type": "command", "command": "say done" }
            ] }] } })
        );
    }

    #[test]
    fn a_settings_file_shaped_wrongly_is_refused_rather_than_rewritten() {
        let mut settings = json!({ "hooks": "please" });
        assert!(wire(&mut settings, 8787, TOKEN).is_err());

        let mut settings = json!({ "hooks": { "Stop": "please" } });
        assert!(unwire(&mut settings).is_err());
    }

    #[test]
    fn status_notices_a_token_that_has_since_changed() {
        let settings = wired();

        let now = found(&settings, TOKEN);
        assert_eq!(now.len(), 3);
        assert!(now.iter().all(|w| w.current));

        let later = found(&settings, "a-new-token");
        assert_eq!(later.len(), 3, "still found");
        assert!(later.iter().all(|w| !w.current), "and reported as stale");
    }

    /// A scratch directory that removes itself.
    struct Temp(PathBuf);

    impl Temp {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "mytimeoff-hooks-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }

        fn path(&self) -> PathBuf {
            self.0.join("settings.json")
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_missing_file_reads_as_no_settings_at_all() {
        let temp = Temp::new("missing");
        assert_eq!(read(&temp.path()).expect("read"), json!({}));
    }

    #[test]
    fn a_file_that_is_not_json_stops_everything() {
        let temp = Temp::new("broken");
        fs::write(temp.path(), "{ not json").expect("write");
        assert!(read(&temp.path()).is_err(), "writing over this would destroy it");
    }

    #[test]
    fn writing_keeps_the_file_that_was_there() {
        let temp = Temp::new("backup");
        fs::write(temp.path(), r#"{"model":"opus"}"#).expect("write");

        let mut settings = read(&temp.path()).expect("read");
        wire(&mut settings, 8787, TOKEN).expect("wire");
        write(&temp.path(), &settings).expect("write");

        let back = read(&temp.path()).expect("read back");
        assert_eq!(back["model"], "opus");
        assert_eq!(found(&back, TOKEN).len(), 3);

        let kept = fs::read_to_string(temp.0.join(BACKUP)).expect("backup");
        assert_eq!(kept, r#"{"model":"opus"}"#);
        assert!(!temp.0.join("settings.json.mytimeoff-new").exists(), "no litter");
    }
}
