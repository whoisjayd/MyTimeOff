use std::io::{self, Read};
use std::sync::Arc;

use mytimeoff_core::{Config, Locator};
use mytimeoff_daemon::quiz::stub::Stub;
use mytimeoff_daemon::quiz::{Fallback, Page, Provider, QuestionSource};
use mytimeoff_daemon::store::Store;
use mytimeoff_daemon::{Daemon, bind, paths, secret, serve, settings, token};

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
    let word = std::env::args().nth(2);
    match std::env::args().nth(1).as_deref() {
        Some("key") => return store_key(word.as_deref()),
        Some("check") => return check().await,
        _ => {}
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
    let questions = question_source(&config)?;
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
fn question_source(config: &Config) -> io::Result<Arc<dyn QuestionSource>> {
    let offline: Arc<dyn QuestionSource> = Arc::new(Stub);
    if config.model.is_empty() {
        println!("quiz:   offline (model is empty; nothing you read leaves this machine)");
        return Ok(offline);
    }

    // A name that belongs to nobody is refused rather than guessed at. Every other outcome
    // here is a working daemon, so a typo would otherwise mean quietly using the offline
    // questions forever while the config looks exactly right.
    let Some(provider) = Provider::for_model(&config.model) else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, unknown_model(&config.model)));
    };

    let Some(key) = provider.key() else {
        println!("quiz:   offline (no {} API key stored)", provider);
        println!("        store one with:  mytimeoff-daemon key {}", provider.word());
        return Ok(offline);
    };

    match provider.source(key, config.model.clone()) {
        Ok(source) => {
            println!(
                "quiz:   {} via {} (the pages you read are sent to write the questions)",
                config.model, provider,
            );
            // Behind it, the offline stub. A gate with no questions lets the reader
            // through, so a dropped connection would otherwise be a free pass.
            Ok(Arc::new(Fallback::new(source, offline, |note| eprintln!("{note}"))))
        }
        Err(error) => {
            println!("quiz:   offline ({error})");
            Ok(offline)
        }
    }
}

fn unknown_model(model: &str) -> String {
    let families = Provider::ALL
        .iter()
        .map(|provider| format!("{}* ({})", provider.prefix(), provider))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "model \"{model}\" names no provider I can reach. Expected one of: {families}. \
         Set model to \"\" to make the questions on this machine instead."
    )
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
        return Err(io::Error::new(io::ErrorKind::InvalidInput, unknown_model(&config.model)));
    };
    let Some(key) = provider.key() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "no {} API key. Store one with:  mytimeoff-daemon key {}",
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
                format!("no provider called \"{word}\". Try: mytimeoff-daemon key {known}"),
            )
        })?,
        None => {
            let config = settings::load_or_create(&paths::config()?)?;
            Provider::for_model(&config.model).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, unknown_model(&config.model))
            })?
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
