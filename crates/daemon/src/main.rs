use std::io::{self, Read};
use std::sync::Arc;

use mytimeoff_core::Config;
use mytimeoff_daemon::quiz::claude::Claude;
use mytimeoff_daemon::quiz::stub::Stub;
use mytimeoff_daemon::quiz::{Fallback, QuestionSource};
use mytimeoff_daemon::store::Store;
use mytimeoff_daemon::{Daemon, bind, paths, secret, serve, settings, token};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("key") {
        return store_key();
    }

    let config_path = paths::config()?;
    let config = settings::load_or_create(&config_path)?;

    let token_path = paths::token()?;
    let secret = token::load_or_create(&token_path)?;

    let database_path = paths::database()?;
    let store = Store::open(&database_path)?;

    let listener = bind(config.port).await?;
    let addr = listener.local_addr()?;

    println!("mytimeoff daemon listening on http://{addr}");
    println!("mode: {:?}, grace: {}ms", config.mode, config.grace_ms);
    println!("config: {}", config_path.display());
    println!("token:  {}", token_path.display());
    println!("books:  {}", database_path.display());
    let questions = question_source(&config);
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

/// Picks who writes the questions, and says so out loud.
///
/// Out loud because the two things a user most needs to know about this feature are
/// invisible otherwise: that pages are leaving the machine, or that they are not and the
/// questions are the weak offline ones. Neither should have to be inferred from the quiz.
fn question_source(config: &Config) -> Arc<dyn QuestionSource> {
    let offline = Arc::new(Stub);
    if config.model.is_empty() {
        println!("quiz:   offline (model is empty; nothing you read leaves this machine)");
        return offline;
    }
    let Some(key) = secret::api_key() else {
        println!("quiz:   offline (no API key stored)");
        println!("        store one with:  mytimeoff-daemon key");
        return offline;
    };
    match Claude::new(key, config.model.clone()) {
        Ok(claude) => {
            println!("quiz:   {} (the pages you read are sent to write the questions)", config.model);
            // Behind it, the offline stub. A gate with no questions lets the reader
            // through, so a dropped connection would otherwise be a free pass.
            Arc::new(Fallback::new(Arc::new(claude), offline, |note| eprintln!("{note}")))
        }
        Err(error) => {
            println!("quiz:   offline ({error})");
            offline
        }
    }
}

/// `mytimeoff-daemon key` - puts the API key where the daemon will look for it.
///
/// It reads from stdin rather than taking an argument, so the key never lands in a shell
/// history or a process list. It is echoed as you type it, which is the honest limit of
/// what can be done without dragging in a terminal crate for one prompt.
fn store_key() -> io::Result<()> {
    println!("Paste your Anthropic API key and press Enter.");
    println!("It will be visible while you type, and stored in Windows Credential Manager");
    println!("under \"{}\" - never in this project's config.", secret::API_KEY);

    let mut typed = String::new();
    io::stdin().read_to_string(&mut typed)?;
    let key = typed.trim();
    if key.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no key given"));
    }

    secret::write(secret::API_KEY, key)?;
    // The last four characters only: enough to check you pasted the right one, and not
    // enough to be worth anything to whoever is looking over your shoulder.
    let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    println!("Stored (…{tail}). Restart the daemon to use it.");
    Ok(())
}
