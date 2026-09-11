//! Putting MyTimeOff into Codex's config, and taking it back out.
//!
//! Codex runs commands where Claude Code posts to a URL, so this wiring cannot point at
//! the daemon directly. It points at this program instead, run as `mytimeoff hook`, which
//! reads the delivery off stdin and forwards it to the loopback endpoint. That shim is
//! the whole difference between the two agents; everything else here is the same promise
//! `claude.rs` makes about `settings.json`, kept against a TOML file instead.
//!
//! One thing falls out of the shim in Codex's favour: the config file never holds the
//! token. The shim reads it off disk when it runs, on the machine that wrote it, so
//! `~/.codex/config.toml` stays a file with no secret in it.
//!
//! **This path has not been tested against a real Codex install.** It is written from
//! Codex's published hook format - `[[hooks.<Event>]]` groups holding `[[hooks.<Event>
//! .hooks]]` command entries, payload as JSON on stdin - and the shape is exercised by
//! the tests below, but nobody has yet watched Codex actually deliver through it. That is
//! why [`super::Agent::proven`] says what it says, and why everything here refuses rather
//! than guesses the moment the file looks unfamiliar.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use toml_edit::{ArrayOfTables, DocumentMut, Formatted, Item, Table, Value};

use super::{Outcome, Status, Wired};

/// The events MyTimeOff listens for.
///
/// The first two are the same words Claude Code uses, and mean the same things. The third
/// is where the agents part: Claude Code reports "waiting on you" as a `Notification` with
/// a type attached, while Codex gives it an event of its own. Without it a turn that
/// paused for permission would leave someone reading a book while their agent waited.
pub const EVENTS: [&str; 3] = ["UserPromptSubmit", "Stop", "PermissionRequest"];

/// The name of the copy kept beside the config before each change.
pub const BACKUP: &str = "config.toml.mytimeoff-backup";

/// The argument the shim answers to, and the signature that marks a command as ours.
const WORD: &str = "hook";

/// How long Codex will wait on one delivery.
///
/// Five seconds against a default of six hundred, because this hook talks to a socket on
/// this machine and has nothing to say for itself if that takes longer. There is an
/// `async` field that would take the wait away entirely; it is left out on purpose. Every
/// field written into an untested config is a chance to write a config Codex refuses to
/// load, and the shim gives itself a shorter deadline than this one anyway - so `async`
/// would buy a fraction of a second at the price of the whole file parsing.
const TIMEOUT: i64 = 5;

/// `~/.codex/config.toml`, or wherever `CODEX_HOME` has moved it.
pub fn settings_path() -> io::Result<PathBuf> {
    if let Some(moved) = std::env::var_os("CODEX_HOME") {
        return Ok(PathBuf::from(moved).join("config.toml"));
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no USERPROFILE or HOME, so there is no way to find .codex/config.toml",
            )
        })?;
    Ok(PathBuf::from(home).join(".codex").join("config.toml"))
}

/// The command Codex should run, quoted for a shell.
///
/// No port in it. The shim reads the config for that when it runs, so changing the port
/// leaves this line still correct - where Claude Code, which has to carry the address
/// itself, needs re-wiring.
pub fn command() -> io::Result<String> {
    Ok(format!("\"{}\" {WORD}", shim_exe()?.display()))
}

/// The command-line program, which is the thing Codex has to run.
///
/// Windows compares file names without regard to case, so `dir.join("mytimeoff.exe")` in
/// the install folder opens `MyTimeOff.exe` - the *window* - and a Codex hook pointed at
/// the window would raise a GUI on every prompt. `canonicalize` answers with the name as
/// it is really spelled on disk, which is the only thing that tells the two apart. The
/// path handed back is the one that was looked up rather than the resolved one, because
/// resolving it on Windows returns a `\\?\` path that no shell should be asked to read.
fn shim_exe() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let name = if cfg!(windows) { "mytimeoff.exe" } else { "mytimeoff" };
    let here = exe.parent().map_or_else(|| exe.clone(), Path::to_path_buf);

    // Beside this program first - which covers this program *being* it, and a cargo
    // target directory where the two sit side by side. Then the bin folder below, which
    // is the installed layout seen from the window.
    let looked = [here.join(name), here.join("bin").join(name)];
    for candidate in &looked {
        if really_named(candidate, name) {
            return Ok(candidate.clone());
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "no {name} near this program, so there is nothing for Codex to run.\nLooked for: {}",
            looked.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
        ),
    ))
}

/// Whether that path opens a file whose name really is spelled that way.
fn really_named(candidate: &Path, name: &str) -> bool {
    fs::canonicalize(candidate).is_ok_and(|real| real.file_name().is_some_and(|it| it == name))
}

/// Did this tool write this command?
///
/// The program and the word it is passed are the signature: nothing else on a machine
/// runs `mytimeoff hook`. Deliberately not the whole string - match on that and moving
/// the install would leave the old command behind forever, in a file the user is not
/// expected to read.
fn is_ours(command: &str) -> bool {
    let (program, rest) = split_program(command);
    rest.trim() == WORD
        && Path::new(program)
            .file_stem()
            .is_some_and(|stem| stem.eq_ignore_ascii_case("mytimeoff"))
}

/// The program out of a command line, and whatever follows it.
///
/// The path is quoted when we write it, because an install under a name with a space in
/// it would otherwise be two arguments. Reading it back has to undo that.
fn split_program(command: &str) -> (&str, &str) {
    let command = command.trim();
    match command.strip_prefix('"') {
        Some(rest) => match rest.split_once('"') {
            Some((program, after)) => (program, after),
            // An opening quote and no closing one. Not something we wrote.
            None => ("", command),
        },
        None => command.split_once(char::is_whitespace).unwrap_or((command, "")),
    }
}

/// What Codex's config says about MyTimeOff right now.
pub fn status() -> io::Result<Status> {
    let path = settings_path()?;
    let present = path.parent().is_some_and(Path::is_dir);
    let doc = match read(&path) {
        Ok(doc) => doc,
        Err(error) => {
            return Ok(Status { path, present, wired: Vec::new(), trouble: Some(error.to_string()) });
        }
    };
    // A shim that cannot be found is worth saying out loud: every hook in the file is
    // then naming a program that is not there, which reads as connected and is not.
    let (expected, trouble) = match command() {
        Ok(expected) => (Some(expected), None),
        Err(error) => (None, Some(error.to_string())),
    };
    Ok(Status { path, present, wired: found(&doc, expected.as_deref()), trouble })
}

/// Puts MyTimeOff into Codex's config.
pub fn install(port: u16) -> io::Result<Outcome> {
    // Before touching the file, because a wiring that names no program is worse than no
    // wiring: it is three commands that fail silently on every prompt.
    let command = command()?;
    let _ = port;

    let path = settings_path()?;
    let replaced = path.is_file();
    let mut doc = read(&path)?;
    wire(&mut doc, &command)?;
    write(&path, &doc)?;
    Ok(Outcome { path, replaced, backup: BACKUP })
}

/// Takes MyTimeOff back out, and says how many hooks there were.
pub fn remove() -> io::Result<usize> {
    let path = settings_path()?;
    let mut doc = read(&path)?;
    let removed = unwire(&mut doc)?;
    if removed > 0 {
        write(&path, &doc)?;
    }
    Ok(removed)
}

/// Every MyTimeOff hook in this config, in the order the events are listed.
///
/// `expected` is the command this machine would write today; anything else is a hook
/// naming an install that has moved.
pub fn found(doc: &DocumentMut, expected: Option<&str>) -> Vec<Wired> {
    let mut wired = Vec::new();
    for event in EVENTS {
        for command in commands(doc, event) {
            if !is_ours(command) {
                continue;
            }
            wired.push(Wired {
                event,
                target: command.to_string(),
                current: expected == Some(command),
            });
        }
    }
    wired
}

/// Every command registered for one event, whoever wrote it.
///
/// Only the `[[hooks.<Event>]]` form is looked at. Codex documents that form and it is
/// the only one written here; a config that spelled its hooks as an inline array would be
/// hand-written, and reaching into somebody's hand-written table to edit it is exactly
/// the liberty this module exists not to take.
fn commands<'a>(doc: &'a DocumentMut, event: &str) -> impl Iterator<Item = &'a str> {
    doc.get("hooks")
        .and_then(Item::as_table_like)
        .and_then(|hooks| hooks.get(event))
        .and_then(Item::as_array_of_tables)
        .into_iter()
        .flatten()
        .filter_map(|group| group.get("hooks"))
        .filter_map(Item::as_array_of_tables)
        .flatten()
        .filter(|entry| entry.get("type").and_then(Item::as_str) == Some("command"))
        .filter_map(|entry| entry.get("command").and_then(Item::as_str))
}

/// Puts one MyTimeOff hook on each event, replacing any that were already there.
pub fn wire(doc: &mut DocumentMut, command: &str) -> io::Result<()> {
    unwire(doc)?;

    let root = doc.as_table_mut();
    if !root.contains_key("hooks") {
        let mut table = Table::new();
        // Implicit, so this adds `[[hooks.Stop]]` and not a bare `[hooks]` header above
        // it with nothing under it.
        table.set_implicit(true);
        root.insert("hooks", Item::Table(table));
    }
    let hooks = root
        .get_mut("hooks")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| malformed("\"hooks\" is there but is not a TOML table of its own"))?;

    for event in EVENTS {
        if !hooks.contains_key(event) {
            hooks.insert(event, Item::ArrayOfTables(ArrayOfTables::new()));
        }
        let groups = hooks
            .get_mut(event)
            .and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| malformed(&format!("\"hooks.{event}\" is not a list of tables")))?;

        let mut entry = Table::new();
        entry.insert("type", Item::Value(Value::String(Formatted::new("command".into()))));
        entry.insert("command", Item::Value(literal(command)));
        entry.insert("timeout", Item::Value(Value::Integer(Formatted::new(TIMEOUT))));

        let mut entries = ArrayOfTables::new();
        entries.push(entry);
        let mut group = Table::new();
        group.insert("hooks", Item::ArrayOfTables(entries));
        groups.push(group);
    }
    Ok(())
}

/// Takes every MyTimeOff hook back out, and says how many there were.
///
/// A group left holding nothing goes too, but only if it was this tool that emptied it:
/// an empty group somebody else wrote is somebody else's to explain.
pub fn unwire(doc: &mut DocumentMut) -> io::Result<usize> {
    let root = doc.as_table_mut();
    if !root.contains_key("hooks") {
        return Ok(0);
    }
    let hooks = root
        .get_mut("hooks")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| malformed("\"hooks\" is there but is not a TOML table of its own"))?;

    let mut removed = 0;
    for event in EVENTS {
        let Some(groups) = hooks.get_mut(event).and_then(Item::as_array_of_tables_mut) else {
            continue;
        };

        let mut emptied = Vec::new();
        for (at, group) in groups.iter_mut().enumerate() {
            let Some(entries) = group.get_mut("hooks").and_then(Item::as_array_of_tables_mut)
            else {
                continue;
            };
            let before = entries.len();
            entries.retain(|entry| {
                !entry.get("command").and_then(Item::as_str).is_some_and(is_ours)
            });
            if entries.len() < before {
                removed += before - entries.len();
                if entries.is_empty() {
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
        root.remove("hooks");
    }
    Ok(removed)
}

/// Reads the config, treating a file that is not there as an empty one.
///
/// A file that is there and will not parse stops everything. Starting from blank and
/// writing that back would delete a config this tool was only ever asked to add to.
pub fn read(path: &Path) -> io::Result<DocumentMut> {
    let text = match crate::text::read(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    text.parse::<DocumentMut>().map_err(|error| {
        malformed(&format!(
            "{} is not valid TOML ({error}), and this refuses to write over a file it cannot read",
            path.display()
        ))
    })
}

/// Writes the config back, keeping a copy of what was there first.
///
/// Through a temporary file and a rename, for the reason `claude.rs` gives: a write
/// interrupted halfway leaves a config that parses as nothing, and a rename either
/// happened or did not.
pub fn write(path: &Path, doc: &DocumentMut) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        if path.is_file() {
            fs::copy(path, parent.join(BACKUP))?;
        }
    }

    let temporary = path.with_extension("toml.mytimeoff-new");
    fs::write(&temporary, doc.to_string())?;
    fs::rename(&temporary, path)
}

/// A TOML string, written as a literal where that is legal.
///
/// Windows paths are made of backslashes and TOML's ordinary strings escape every one of
/// them, so the command would land in the file as
/// `"\"C:\\Users\\me\\AppData\\Local\\MyTimeOff\\bin\\mytimeoff.exe\" hook"`. A literal
/// string takes it verbatim. It cannot hold a quote of its own, so a home directory with
/// an apostrophe in the name falls back to the escaped form - correct, just harder on
/// whoever opens the file.
fn literal(text: &str) -> Value {
    // Built by parsing rather than by setting the spelling directly, because toml_edit
    // keeps that door shut: a value carries the spelling it was read with, and reading is
    // the only way offered to choose one.
    if !text.contains('\'')
        && !text.contains('\n')
        && let Ok(parsed) = format!("x = '{text}'").parse::<DocumentMut>()
        && let Some(value) = parsed.get("x").and_then(Item::as_value)
    {
        let mut value = value.clone();
        // It arrives wearing the spacing of the line it was parsed out of, and it is
        // going somewhere else.
        value.decor_mut().set_prefix(" ");
        value.decor_mut().set_suffix("");
        return value;
    }
    Value::String(Formatted::new(text.to_string()))
}

fn malformed(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OURS: &str = r#""C:\Program Files\MyTimeOff\bin\mytimeoff.exe" hook"#;

    fn wired() -> DocumentMut {
        let mut doc = DocumentMut::new();
        wire(&mut doc, OURS).expect("wire");
        doc
    }

    #[test]
    fn wiring_an_empty_config_covers_every_event() {
        let doc = wired();
        for event in EVENTS {
            let ours: Vec<_> = commands(&doc, event).filter(|c| is_ours(c)).collect();
            assert_eq!(ours.len(), 1, "{event} should have exactly one MyTimeOff hook");
            assert_eq!(ours[0], OURS);
        }
    }

    #[test]
    fn what_is_written_is_the_shape_codex_documents() {
        let toml = wired().to_string();
        // Group, then the entry inside it. Getting these two headers the wrong way round
        // is the single most likely way for this file to be silently ignored.
        assert!(toml.contains("[[hooks.Stop]]"), "{toml}");
        assert!(toml.contains("[[hooks.Stop.hooks]]"), "{toml}");
        assert!(toml.contains(r#"type = "command""#), "{toml}");
        assert!(toml.contains(&format!("timeout = {TIMEOUT}")), "{toml}");
        // And it has to parse as what it claims to be.
        let back: DocumentMut = toml.parse().expect("what we wrote must be valid TOML");
        assert_eq!(found(&back, Some(OURS)).len(), EVENTS.len());
    }

    #[test]
    fn a_windows_path_goes_in_without_a_thicket_of_backslashes() {
        let toml = wired().to_string();
        assert!(toml.contains(&format!("command = '{OURS}'")), "{toml}");
        assert!(!toml.contains(r"\\"), "a literal string escapes nothing:\n{toml}");
    }

    #[test]
    fn a_path_with_an_apostrophe_falls_back_rather_than_writing_broken_toml() {
        let odd = r#""C:\Users\O'Brien\mytimeoff.exe" hook"#;
        let mut doc = DocumentMut::new();
        wire(&mut doc, odd).expect("wire");
        let back: DocumentMut = doc.to_string().parse().expect("must still be valid TOML");
        assert_eq!(found(&back, Some(odd)).len(), EVENTS.len());
    }

    #[test]
    fn wiring_twice_leaves_one_hook_not_two() {
        let mut doc = wired();
        wire(&mut doc, OURS).expect("wire again");
        for event in EVENTS {
            assert_eq!(commands(&doc, event).filter(|c| is_ours(c)).count(), 1, "{event}");
        }
    }

    #[test]
    fn everything_that_was_already_there_is_still_there() {
        // The promise the whole module is built around, against a file with comments,
        // ordinary settings and a hook of somebody else's in it.
        let before = r#"# my codex settings
model = "gpt-5-codex"
approval_policy = "on-request"

[[hooks.Stop]]
matcher = "^build$"

[[hooks.Stop.hooks]]
type = "command"
command = "notify-send done"
"#;
        let mut doc: DocumentMut = before.parse().expect("valid TOML");
        wire(&mut doc, OURS).expect("wire");
        assert_eq!(unwire(&mut doc).expect("unwire"), EVENTS.len());
        assert_eq!(doc.to_string(), before, "the file has to come back exactly as it was");
    }

    #[test]
    fn a_shared_group_keeps_its_other_hooks() {
        let mut doc: DocumentMut = r#"
[[hooks.Stop]]

[[hooks.Stop.hooks]]
type = "command"
command = "notify-send done"

[[hooks.Stop.hooks]]
type = "command"
command = '"C:\MyTimeOff\bin\mytimeoff.exe" hook'
"#
        .parse()
        .expect("valid TOML");
        assert_eq!(unwire(&mut doc).expect("unwire"), 1);
        let left: Vec<_> = commands(&doc, "Stop").collect();
        assert_eq!(left, vec!["notify-send done"]);
    }

    #[test]
    fn an_empty_group_somebody_else_left_stays() {
        let before = "[[hooks.Stop]]\nmatcher = \"^build$\"\n";
        let mut doc: DocumentMut = before.parse().expect("valid TOML");
        assert_eq!(unwire(&mut doc).expect("unwire"), 0);
        assert_eq!(doc.to_string(), before);
    }

    #[test]
    fn a_hook_naming_an_install_that_moved_is_still_ours() {
        // It has to be, or `install` after moving the app would leave the old command
        // behind and add a second one beside it.
        let doc = wired();
        let elsewhere = r#""D:\MyTimeOff\bin\mytimeoff.exe" hook"#;
        let wired = found(&doc, Some(elsewhere));
        assert_eq!(wired.len(), EVENTS.len());
        assert!(wired.iter().all(|one| !one.current), "and it must not read as current");
    }

    #[test]
    fn somebody_elses_command_is_left_alone() {
        for command in [
            "notify-send done",
            "mytimeofff hook",
            r#""C:\other\mytimeoff.exe" check"#,
            // The window, not the command line - and it takes no `hook` argument.
            r#""C:\MyTimeOff\MyTimeOff.exe""#,
            "",
        ] {
            assert!(!is_ours(command), "{command:?} is not ours to remove");
        }
    }

    #[test]
    fn the_program_is_read_back_out_of_its_quotes() {
        assert_eq!(split_program(r#""C:\a b\mytimeoff.exe" hook"#), (r"C:\a b\mytimeoff.exe", " hook"));
        assert_eq!(split_program("mytimeoff hook"), ("mytimeoff", "hook"));
        assert_eq!(split_program("mytimeoff"), ("mytimeoff", ""));
    }

    #[test]
    fn an_unquoted_command_is_still_recognised() {
        // Not what is written here, but a plausible thing for somebody to type by hand
        // once the program is on their PATH - and removing it should still work.
        assert!(is_ours("mytimeoff hook"));
        assert!(is_ours("  mytimeoff.exe   hook  "));
    }

    #[test]
    fn a_config_that_is_not_toml_is_refused_rather_than_written_over() {
        let dir = std::env::temp_dir().join(format!("mytimeoff-codex-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        fs::write(&path, "this = = not toml").expect("write");
        assert!(read(&path).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_config_that_is_not_there_reads_as_an_empty_one() {
        let doc = read(Path::new("no-such-directory-anywhere/config.toml")).expect("read");
        assert!(doc.to_string().is_empty());
    }

    #[test]
    fn hooks_written_as_something_other_than_a_table_are_refused() {
        // Writing an array of tables into an inline table would produce a config file
        // that no longer parses. Refusing is the only safe answer.
        let mut doc: DocumentMut =
            "hooks = { Stop = [] }\n".parse().expect("valid TOML");
        assert!(wire(&mut doc, OURS).is_err());
        assert!(unwire(&mut doc).is_err());
    }
}
