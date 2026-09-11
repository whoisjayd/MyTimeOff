use std::io::{self, Read};

use mytimeoff_core::Locator;
use mytimeoff_daemon::quiz::{self, Page, Provider};
use mytimeoff_daemon::store::Store;
use mytimeoff_daemon::{Daemon, autostart, bind, paths, secret, serve, settings, token};

#[tokio::main]
async fn main() {
    // Printed, not returned. Returning a Result from `main` makes Rust format the error
    // with `Debug`, which turns a message written for a person into
    // `Custom { kind: Other, error: Unavailable("HTTP 400\n  {\n ...") }` - escaped
    // newlines and all. Every error out of here is something someone has to read and act
    // on, so it is written once and shown as written.
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> io::Result<()> {
    // Before the dispatch, because `check` and `autostart` read the config too, and a
    // command that silently read a *different* config than the one it moved would be a
    // strange thing to debug.
    if let Some(note) = paths::migrate_legacy_state() {
        println!("{note}");
    }

    let word = std::env::args().nth(2);
    match std::env::args().nth(1).as_deref() {
        // No word at all is the daemon itself, which is the only thing this program does
        // that is not a one-off command.
        None => {}
        Some("key") => return store_key(word.as_deref()),
        Some("check") => return check().await,
        Some("autostart") => return autostart_command(word.as_deref()),
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            return Ok(());
        }
        // Anything else is a typo. Ignoring it and starting the daemon meant that
        // `mytimeoff-daemon --help` opened a server, and that a misspelt subcommand
        // opened a second one on a port that was already spoken for.
        Some(other) => {
            return Err(invalid(format!("no command called \"{other}\".

{USAGE}")));
        }
    }

    let config_path = paths::config()?;
    let config = settings::load_or_create(&config_path)?;

    let token_path = paths::token()?;
    let secret = token::load_or_create(&token_path)?;

    let database_path = paths::database()?;
    let store = Store::open(&database_path)?;

    let listener = bind(config.port).await.map_err(|error| busy(error, &config_path))?;
    let addr = listener.local_addr()?;

    println!("mytimeoff daemon listening on http://{addr}");
    println!("mode: {:?}, grace: {}ms", config.mode, config.grace_ms);
    println!("config: {}", config_path.display());
    println!("token:  {}", token_path.display());
    println!("books:  {}", database_path.display());
    let (questions, note) = quiz::source_for(&config).map_err(invalid)?;
    println!("quiz:   {note}");
    println!();
    println!("Wire Claude Code by adding to .claude/settings.json:");
    println!(
        r#"  "hooks": {{
    "UserPromptSubmit": [{{ "hooks": [{{ "type": "http", "url": "http://{addr}/hook",
      "headers": {{ "Authorization": "Bearer $MYTIMEOFF_TOKEN" }},
      "allowedEnvVars": ["MYTIMEOFF_TOKEN"], "async": true }}] }}],
    "Stop": [ ...same, /hook... ],
    "Notification": [ ...same, /hook... ]
  }}"#
    );

    let daemon = Daemon::new(config, store, secret, questions);
    serve(listener, daemon).await
}

/// `mytimeoff-daemon check` - asks the configured provider for questions about two made-up
/// pages, and prints what came back.
///
/// This exists because of how this feature fails. A wrong key, a model name the provider
/// has retired, a request field that moved: none of them stop the daemon, they make every
/// gate fall through to the offline stub, and the only sign is that the questions are
/// worse than they should be. This turns that into something a person can see at setup,
/// once, on purpose.
///
/// It deliberately does *not* wrap the source in the fallback. The fallback's whole job is
/// to hide this failure from the reader; hiding it here would defeat the point.
async fn check() -> io::Result<()> {
    let config = settings::load_or_create(&paths::config()?)?;
    if config.model.is_empty() {
        println!("model is empty: questions are made on this machine, and nothing is sent.");
        return Ok(());
    }
    let Some(provider) = Provider::for_model(&config.model) else {
        return Err(invalid(quiz::unknown_model(&config.model)));
    };
    let Some(key) = provider.key() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "no {} API key. Store one with:  mytimeoff key {}",
                provider,
                provider.word(),
            ),
        ));
    };

    println!("Asking {} ({provider}) for questions about two sample pages…", config.model);
    let source = provider.source(key, config.model.clone()).map_err(io::Error::other)?;
    let pages = samples();
    let questions =
        source.questions(&pages, pages.len()).await.map_err(io::Error::other)?;

    println!();
    for question in &questions {
        println!("{} [{}]", question.prompt, question.source.page_label());
        for (at, choice) in question.choices.iter().enumerate() {
            let mark = if at == question.answer_index { '*' } else { ' ' };
            println!("  {mark} {choice}");
        }
        println!();
    }
    println!("{} question(s). A * marks the answer.", questions.len());
    Ok(())
}

/// Two pages with something to ask about, invented so this never sends anything the user
/// has actually been reading.
fn samples() -> Vec<Page> {
    let page = |page: u32, text: &str| Page {
        locator: Locator::Page { page, page_label: page.to_string() },
        text: text.to_string(),
    };
    vec![
        page(
            1,
            "The lighthouse keeper had kept the same log for thirty-one years, and in all \
             that time had recorded the weather twice a day and nothing else. When the \
             inspector asked why he had never noted the ships, he said that the ships \
             were not his business; the light was. The inspector wrote in his report that \
             the keeper was uncooperative, and recommended his replacement. Two winters \
             later the new keeper's logs proved useless in the inquiry, because he had \
             recorded everything and dated nothing.",
        ),
        page(
            2,
            "What makes a measurement useful is not its precision but its consistency. A \
             thermometer that reads two degrees high every day will still tell you when \
             the summer turned, while one that is accurate on average and wrong at random \
             will not tell you anything at all. This is why the older records, taken with \
             worse instruments by people who used them the same way every morning, remain \
             the more valuable of the two archives.",
        ),
    ]
}

/// `mytimeoff-daemon key [claude|gemini]` - puts an API key where the daemon will look.
///
/// It reads from stdin rather than taking an argument, so the key never lands in a shell
/// history or a process list. It is echoed as you type it, which is the honest limit of
/// what can be done without dragging in a terminal crate for one prompt.
///
/// With no provider named it stores the key for whichever one the configured model
/// implies, because that is what someone who has edited their config once and wants it to
/// work means by "the key".
fn store_key(word: Option<&str>) -> io::Result<()> {
    let provider = match word {
        Some(word) => Provider::from_word(word).ok_or_else(|| {
            let known =
                Provider::ALL.iter().map(|p| p.word()).collect::<Vec<_>>().join(" or ");
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("no provider called \"{word}\". Try: mytimeoff key {known}"),
            )
        })?,
        None => {
            let config = settings::load_or_create(&paths::config()?)?;
            Provider::for_model(&config.model)
                .ok_or_else(|| invalid(quiz::unknown_model(&config.model)))?
        }
    };

    println!("Paste your {} API key and press Enter.", provider);
    println!("It will be visible while you type, and stored in Windows Credential Manager");
    println!("under \"{}\" - never in this project's config.", provider.credential());

    let mut typed = String::new();
    io::stdin().read_to_string(&mut typed)?;
    let key = typed.trim();
    if key.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no key given"));
    }

    secret::write(provider.credential(), key)?;
    // The last four characters only: enough to check you pasted the right one, and not
    // enough to be worth anything to whoever is looking over your shoulder.
    let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    println!("Stored (…{tail}) for {provider}. Restart the daemon to use it.");
    println!("Check it works with:  mytimeoff-daemon check");
    Ok(())
}

/// The name the entry appears under in Task Manager's Startup apps tab.
///
/// It is what a stranger to this code sees beside the switch that turns it off, so it is
/// the product's name and not a binary's.
const LOGIN_ITEM: &str = "MyTimeOff";

/// `mytimeoff-daemon autostart [on|off]` - whether MyTimeOff comes back after a reboot.
///
/// What gets registered is the reader window, not this daemon, and the reason is a console
/// window: a `Run` entry pointing at a console program opens one at every sign-in and
/// leaves it open. The window is a GUI program with no console, and it starts the daemon
/// inside itself, so one entry brings back both halves and none of it is visible.
///
/// With no word it reports rather than changes anything, because "is this on?" is the
/// question someone asks first and it should not be dangerous to ask.
fn autostart_command(word: Option<&str>) -> io::Result<()> {
    match word {
        None | Some("status") => report_autostart(),
        Some("on") => {
            let reader = reader_exe()?;
            autostart::enable(LOGIN_ITEM, &reader.display().to_string())?;
            println!("MyTimeOff will start when you sign in.");
            println!("  {}", reader.display());
            println!();
            println!("To stop it without this command: Task Manager, Startup apps, {LOGIN_ITEM}.");
            Ok(())
        }
        Some("off") => {
            autostart::disable(LOGIN_ITEM)?;
            println!("MyTimeOff will not start when you sign in.");
            Ok(())
        }
        Some(other) => Err(invalid(format!(
            "no autostart setting called \"{other}\". Try: mytimeoff autostart on, off or status"
        ))),
    }
}

fn report_autostart() -> io::Result<()> {
    let item = autostart::read(LOGIN_ITEM)?;
    match (&item.command, item.enabled) {
        (None, _) => {
            println!("MyTimeOff does not start when you sign in.");
            println!("Turn it on with:  mytimeoff autostart on");
        }
        // Registered, and then switched off in Task Manager. Saying "on" here would be a
        // lie about the next reboot, and saying "off" would hide an entry that is still
        // sitting in the registry.
        (Some(command), false) => {
            println!("MyTimeOff is registered but switched off in Task Manager's Startup apps.");
            println!("  {command}");
            println!("Turn the switch back on there, or remove the entry with:");
            println!("  mytimeoff-daemon autostart off");
        }
        (Some(command), true) => {
            println!("MyTimeOff starts when you sign in.");
            println!("  {command}");
        }
    }
    Ok(())
}

/// The reader window, which lives beside this program.
///
/// Beside, because that is how the two are shipped and how they are built. If it is not
/// there this refuses rather than registering the daemon instead: an autostart that opens
/// a console window and no reader is not what anyone asked for, and silently doing the
/// wrong one of two things is worse than doing neither.
///
/// Two names, because the window has two. Installed it is `MyTimeOff.exe`, which is what a
/// person sees; built from source it is `mytimeoff-shell.exe`, which is what Cargo calls
/// the crate. Looking for only one of them breaks autostart either for everybody who
/// installed the app or for everybody working on it.
fn reader_exe() -> io::Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let names = [format!("MyTimeOff{suffix}"), format!("mytimeoff-shell{suffix}")];

    // Two places, because there are two layouts. Installed, this program sits in a bin
    // subdirectory and the window is one level up: they cannot share a directory, since
    // Windows treats MyTimeOff.exe and mytimeoff.exe as one file. In a cargo target
    // directory the two do sit side by side.
    let here = exe.parent().map_or_else(|| exe.clone(), |dir| dir.to_path_buf());
    let mut dirs = vec![here.clone()];
    if let Some(above) = here.parent() {
        dirs.push(above.to_path_buf());
    }

    for dir in &dirs {
        for name in &names {
            let reader = dir.join(name);
            if reader.is_file() && !is_this_program(&reader, &exe) {
                return Ok(reader);
            }
        }
    }

    let looked: Vec<String> = dirs.iter().map(|dir| dir.display().to_string()).collect();
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "no reader window near this program, so there is nothing useful to start.\n\
             Looked in {} for: {}",
            looked.join(" and "),
            names.join(", "),
        ),
    ))
}

/// Is `candidate` the very program that is running?
///
/// Windows matches filenames without regard to case, so looking for `MyTimeOff.exe` in
/// the bin directory finds `mytimeoff.exe` - this program - and would offer the command
/// line to the user as their reading window. Resolving both paths is what tells them
/// apart; comparing the names as written does not.
fn is_this_program(candidate: &std::path::Path, exe: &std::path::Path) -> bool {
    match (std::fs::canonicalize(candidate), std::fs::canonicalize(exe)) {
        (Ok(one), Ok(other)) => one == other,
        _ => false,
    }
}

/// "You asked for something impossible", as an `io::Error`.
fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// What this program can be asked to do.
///
/// Written out rather than generated by an argument crate, because there are four
/// commands and the interesting part of this text is the last paragraph, which no
/// generator would think to write.
const USAGE: &str = "MyTimeOff - reading that fills the time an agent spends thinking.

  mytimeoff-daemon              start the daemon, and print how to wire the hooks
  mytimeoff-daemon check        ask the configured model for questions about two
                                sample pages, and show what came back
  mytimeoff-daemon key [claude|gemini]
                                store an API key in Windows Credential Manager
  mytimeoff-daemon autostart [on|off|status]
                                whether MyTimeOff comes back after you sign in
  mytimeoff-daemon help         this

The reading window is mytimeoff-shell, beside this program, and it starts a daemon
inside itself if none is running. So on most days none of this is needed: open the
window.";

/// The port was taken.
///
/// This is the likeliest error this program will ever print, because the window hosts a
/// daemon of its own and starting one by hand while the window is open is the obvious
/// mistake. What it replaces named neither the port nor the cause: "Only one usage of
/// each socket address (protocol/network address/port) is normally permitted.
/// (os error 10048)".
fn busy(error: io::Error, config_path: &std::path::Path) -> io::Error {
    if error.kind() != io::ErrorKind::AddrInUse {
        return error;
    }
    io::Error::new(
        io::ErrorKind::AddrInUse,
        format!(
            "something is already listening on that port, so this daemon did not start.\n\
             It is most likely MyTimeOff itself: the reading window runs a daemon inside\n\
             it. Close the window first, or change \"port\" in:\n  {}",
            config_path.display(),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that removes itself, so no test goes looking at a real install.
    struct Temp(std::path::PathBuf);

    impl Temp {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "mytimeoff-exe-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }

        fn file(&self, name: &str) -> std::path::PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, b"not really a program").expect("write");
            path
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The bug this guards against: the command line found itself, called it the reading
    /// window, and wrote that into the startup list. `bin\MyTimeOff.exe` is a name that
    /// answers on Windows even though no file was ever written under it.
    #[test]
    #[cfg(windows)]
    fn a_name_in_the_other_case_is_the_same_program() {
        let temp = Temp::new("case");
        let real = temp.file("mytimeoff.exe");
        let asked_for = temp.0.join("MyTimeOff.exe");

        assert!(asked_for.is_file(), "Windows should answer to either spelling");
        assert!(is_this_program(&asked_for, &real));
    }

    #[test]
    fn two_programs_side_by_side_stay_apart() {
        let temp = Temp::new("apart");
        let one = temp.file("mytimeoff.exe");
        let other = temp.file("mytimeoff-shell.exe");

        assert!(!is_this_program(&other, &one));
    }

    /// Nothing is there to be the same as, so nothing is claimed. The caller has already
    /// checked `is_file`; this only decides between two names that both exist.
    #[test]
    fn a_name_with_no_file_behind_it_is_nobody() {
        let temp = Temp::new("absent");
        let real = temp.file("mytimeoff.exe");

        assert!(!is_this_program(&temp.0.join("MyTimeOff.exe.missing"), &real));
    }
}
