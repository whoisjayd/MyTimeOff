use std::time::Duration;

use mytimeoff_core::ReaderMode;
use mytimeoff_daemon::{Daemon, bind, serve, token};

/// Default loopback port. Configurable later; fixed for now so wiring stays stable
/// across restarts.
const DEFAULT_PORT: u16 = 8787;

/// Turns shorter than this never take the screen.
const DEFAULT_GRACE: Duration = Duration::from_secs(20);

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let token_path = token::default_path()?;
    let secret = token::load_or_create(&token_path)?;

    let listener = bind(DEFAULT_PORT).await?;
    let addr = listener.local_addr()?;

    println!("mytimeoff daemon listening on http://{addr}");
    println!("token file: {}", token_path.display());
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

    let daemon = Daemon::new(ReaderMode::Strict, DEFAULT_GRACE, secret);
    serve(listener, daemon).await
}
