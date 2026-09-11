# MyTimeOff

Read a book in the time your coding agent spends thinking.

A long agent turn is dead time. Not enough of it to start anything, too much of it to
sit and watch — so it goes to a second monitor, or a feed, and comes back as nothing.
MyTimeOff puts a book there instead.

When a turn starts, the reader takes the screen. When the turn ends, an indicator says
so. When you ask to leave, it asks what you read.

Both halves are automatic. You do not press anything to start reading, and you do not
have to watch for the agent to finish.

Windows only, for now.

---

## Install

Download `MyTimeOff_0.1.0_x64-setup.exe` from the
[latest release](https://github.com/Yash-Bhambhani/MyTimeOff/releases) and run it. It
installs for you alone, needs no administrator, and puts about 25 MB in
`%LOCALAPPDATA%\MyTimeOff`.

**Windows will warn you.** The installer is not code-signed — a certificate costs a few
hundred dollars a year, which this does not have yet — so SmartScreen shows *"Windows
protected your PC"*. Click **More info**, then **Run anyway**. If you would rather check
first, the release page lists the SHA-256 of the file:

```powershell
Get-FileHash .\MyTimeOff_0.1.0_x64-setup.exe -Algorithm SHA256
```

### First run

1. **Open MyTimeOff.** Drop an EPUB or PDF onto the window, or choose one. It remembers
   the book and the page, so this is a first-run step and not a daily one.
2. **Connect your agent.** The window opens on this the first time, because until it is
   done MyTimeOff cannot hear anything and will never appear on its own. Pick your agent,
   press **Connect**, then restart it — agents read their hooks when they start.
3. **Give your agent a long job.** The book appears.

Connecting adds three hooks to the agent's own settings file and leaves everything else
in it alone. Whatever was there is copied to a `.mytimeoff-backup` beside it first.
**Disconnect** in the same panel takes them back out again.

### While it is running

Closing the window does not stop MyTimeOff. It carries on listening from the
notification area — the `^` at the right-hand end of your taskbar — and the book still
appears when a turn runs long. Click the icon to open the reader again; right-click it
for **Quit**, which is the way to actually stop it.

Windows files a new tray icon under that `^` rather than showing it, so the first time
you close the window MyTimeOff says where it went.

Between turns the window gets out of the way on its own, so most of the time you will
not see it until an agent starts thinking. A second click on the shortcut never opens a
second copy; it brings the one you have back to the front.

While the reader has the screen, the X means the same thing as the **Back** button and
the Escape key: *I want to leave*. In `strict` mode that asks the questions first —
closing the window is not a way around the gate, and neither is minimising it: the
minimise button is unavailable until the screen is yours again. **Quit** in the tray menu
still works, because a program you cannot stop is a program you uninstall.

`mytimeoff autostart on` makes it come back after you sign in. That launch is the one
exception to all of the above: it starts out of sight, because a book that opens itself
across your screen while you are logging in is not a feature.

### Which agents

| | | |
| --- | --- | --- |
| **Claude Code** | `~/.claude/settings.json` | works, and is what this was built against |
| **Codex** | `~/.codex/config.toml` | written from its published hook format, **not yet tested** |

Codex support is honest rather than proven: the config it writes follows the documented
format and the daemon accepts what it sends, but nobody has yet watched a real Codex
install drive it end to end. The panel says so next to the button. If you run it and the
book never appears, that is the first thing to suspect — and an issue would be welcome.

---

## The three modes

Set `mode` in the config file. The default is `strict`.

| Mode | Leaving the reader |
| --- | --- |
| `strict` | Answer questions about the pages you were shown. Two in three to pass — on a short stretch that is one question and one right answer. Fail, and you get one more attempt — then it lets you out anyway. |
| `lenient` | You are asked, but you may skip. |
| `free` | No questions. The reader still appears and still counts pages. |

Strict is not a lock. It costs you one retry and then opens, because a reading tool that
holds your machine hostage is a tool you uninstall on a bad afternoon.

---

## Settings

`%LOCALAPPDATA%\com.mytimeoff.desktop\config.json`, written on first run so there is
something to edit:

```json
{
  "mode": "strict",
  "grace_ms": 20000,
  "daily_page_goal": 20,
  "port": 8787,
  "skim_threshold_ms": 3000,
  "questions_per_gate": 3,
  "model": "claude-haiku-4-5-20251001"
}
```

| Field | What it does |
| --- | --- |
| `mode` | `strict`, `lenient` or `free`. |
| `grace_ms` | How long a turn must run before the reader takes the screen. Short turns are not dead time; 20 seconds keeps the book out of the way of quick questions. |
| `daily_page_goal` | Pages per day. The goal the whole thing is for. |
| `port` | The loopback port the daemon listens on. Change it and connect Claude Code again, or its hooks point at nothing. Codex's hooks name this program rather than a port, so they survive. |
| `skim_threshold_ms` | How long a page has to be in front of you before it counts as read. |
| `questions_per_gate` | How many questions a gate asks. |
| `model` | Who writes the questions. See below. |

Changes are read at startup, so close and reopen MyTimeOff after editing.

---

## What leaves your machine

**By default, nothing.** Questions are made on this machine from the pages you were
shown, and no book, page, or reading history is ever sent anywhere.

That changes the moment you store an API key. With one, MyTimeOff sends **the text of
the pages you have just read** to whoever wrote the model named in `model`, to get better
questions back:

- `claude-*` — to Anthropic
- `gemini-*` — to Google

```
mytimeoff key claude        # or: mytimeoff key gemini
mytimeoff check             # asks for questions about two invented pages, and shows them
```

Keys go into **Windows Credential Manager**, under `MyTimeOff/anthropic-api-key` and
`MyTimeOff/gemini-api-key` — never into a config file. If a machine already has
`ANTHROPIC_API_KEY`, `GEMINI_API_KEY` or `GOOGLE_API_KEY` in its environment, that is used
and nothing needs storing.

`mytimeoff check` uses made-up pages on purpose: it proves the key works without sending
anything you were actually reading.

To turn this off and keep everything local, set `"model": ""`. One field, because a
second one would be a second thing to get wrong.

Your reading itself — which books, which pages, how long — is a SQLite file in your own
profile and stays there.

---

## The command line

`mytimeoff` is the window's smaller sibling. Nothing needs it — everything it does, the
window does too — but it is the shorter way to set a machine up from a script, and the
quicker answer to *why has the book stopped appearing*.

```
mytimeoff                              start the daemon by itself
mytimeoff hooks status                 what both agents' settings say about MyTimeOff
mytimeoff hooks install [claude-code|codex]   wire one of them to it
mytimeoff hooks remove  [claude-code|codex]   take the wiring back out
mytimeoff check                        try the configured model
mytimeoff key [claude|gemini]          store an API key
mytimeoff autostart [on|off|status]    come back after you sign in
mytimeoff help
```

`status` reports both agents; `install` and `remove` mean Claude Code unless told
otherwise.

The installer adds `%LOCALAPPDATA%\MyTimeOff\bin` to your PATH so the name works in a
terminal. **Unless your PATH is very long**: past about a thousand characters the
installer cannot read your PATH safely, so it refuses to touch it and says so. Nothing
else is affected — run the command from that folder, or shorten your PATH and reinstall.

You do not need the daemon running separately. The window starts one inside itself.

---

## Where things are kept

| | |
| --- | --- |
| The program | `%LOCALAPPDATA%\MyTimeOff` |
| Settings, token, reading history | `%LOCALAPPDATA%\com.mytimeoff.desktop` |
| API keys | Windows Credential Manager, under `MyTimeOff/` |
| Hooks | `~/.claude/settings.json`, `~/.codex/config.toml` |

They are separate on purpose: uninstalling removes the program and leaves your books and
progress where they are. The uninstaller offers to delete them too, and only does it if
you tick the box.

Before uninstalling, press **Disconnect** for each agent you connected (or run
`mytimeoff hooks remove`) — the uninstaller does not edit your agents' settings, and
hooks left pointing at a daemon that is gone are harmless but untidy.

---

## Building from source

Needs [Rust](https://rustup.rs), [Node 24](https://nodejs.org) and
[pnpm](https://pnpm.io).

```
pnpm install
pnpm -w run bundle:cli      # once after cloning: the bundler expects the CLI on disk
pnpm exec tauri dev         # or: pnpm exec tauri build
```

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
pnpm run test:installer-hooks   # the NSIS PATH logic, on its own
```

The installer's PATH handling has its own test suite because it once ate a PATH. See the
comment at the top of `crates/shell/installer-hooks.nsh` before changing anything in it.

---

## License

MIT. See [LICENSE](LICENSE).
