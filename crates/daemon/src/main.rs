use std::sync::Arc;

use mytimeoff_daemon::quiz::stub::Stub;
use mytimeoff_daemon::store::Store;
use mytimeoff_daemon::{Daemon, bind, paths, serve, settings, token};

#[tokio::main]
async fn main() -> std::io::Result<()> {
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

    // The one line step D changes: the offline stub gives way to a real generator, and
    // nothing else in the daemon has to know.
    let daemon = Daemon::new(config, store, secret, Arc::new(Stub));
    serve(listener, daemon).await
}
