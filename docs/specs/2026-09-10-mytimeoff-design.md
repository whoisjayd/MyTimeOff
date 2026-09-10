# MyTimeOff — Design

**Status:** approved through Section 1; Sections 2–3 are stated defaults pending review.
**Date:** 2026-09-10

## Context

While an AI coding agent (Claude Code / Codex) is thinking, attention leaks away
from the screen with nothing useful to catch it. MyTimeOff catches that dead time
and spends it on reading, then charges a small comprehension toll before returning
you to the terminal — so the time is genuinely read, not merely occupied.

The product is a Windows tray daemon that raises a reader window during long agent
turns and gates the return trip behind questions about the exact pages you were shown.

## Decisions locked with the user

| Decision | Choice |
|---|---|
| Book source | Local EPUB/PDF files (tool renders, so it knows the exact on-screen text) |
| Trigger | Claude Code / Codex hooks, both halves automatic |
| Return behaviour | Indicator on completion; quiz fires when *you* request exit |
| Modes | Strict / Lenient / Free, defined as config-driven policy |
| Strict failure | Retry once with a fresh set, then release; miss logged against stats |
| Quiz generation | Claude API; pre-generate ahead when warranted, on-demand otherwise |
| Offline | No separate offline engine in v1 — Strict degrades to Lenient, reason shown |
| Goal | Pages per day |
| Stack | Tauri v2 (Rust core + TypeScript/web UI) |

## Section 1 — Architecture (approved)

| Piece | Tech | Job |
|---|---|---|
| `core` daemon | Rust, tray, autostart | State machine, loopback HTTP for hooks, global hotkey, quiz generation, SQLite |
| Reader window | WebView + TS, epub.js / pdf.js | Borderless always-on-top takeover; reports page dwell; hosts quiz overlay |
| Library UI | Served by daemon on 127.0.0.1 | Shelf, goals, stats, mode, hook-wiring status |
| Store | SQLite | Books, sessions, page-views, quizzes, answers, goals |

### State machine

```
IDLE --agent_start--> ARMED --grace expires--> READING --agent_done--> READY
  ^                     |                                               |
  |                     +--agent_done before grace--> IDLE       exit requested
  |                        (never switched - short turn)                |
  +-------------- release <---- GATE (quiz) <---------------------------+
```

Load-bearing details:

- **Grace timer (~20s, configurable).** `agent_start` arms rather than switches. Turns
  that finish inside the grace window never take the screen. Without this the tool is
  unbearable within a day.
- **Sessions tracked by ID.** With several agent windows open, only the session that
  triggered the switch may clear it. Hooks carry a session id; the daemon binds the
  reading session to that id and ignores the others.
- **Permission prompts outrank completion.** Claude Code's `Notification` hook fires when
  the agent is *stalled waiting on you* — strictly worse than it being done. Louder
  indicator, and the only case allowed to interrupt hard.
- Loopback-only bind plus a token from a user-readable-only file, so no visited web page
  can drive the daemon.
- Anthropic key in Windows Credential Manager, never a config file.

## Section 2 — Reading tracking, quiz, modes (default, pending review)

### Page-view tracking

Every page view records `{book_id, locator, text, enter_ts, exit_ts}`. Dwell time is
the primitive everything else is built on.

- Pages with dwell below a threshold (~3s) are marked **skimmed**: excluded from the
  quiz span *and* from the pages/day goal.
- This is the anti-cheat, and it needs no separate mechanism: flipping pages cannot
  inflate your goal because skimmed pages do not count, and cannot dodge the quiz
  because the quiz only draws from pages you actually dwelled on.

### Quiz sizing

Scaled to the span so the gate stays roughly under a minute:

| Pages in span | Questions |
|---|---|
| 1–2 | 2 |
| 3–6 | 3 |
| 7+ | 4 |

### Question format

**Multiple choice only for v1.** Instant deterministic grading, no second API round-trip,
no ambiguity to argue with at the moment you are trying to get back to work. Short-answer
grading is a v2 question.

### Modes as policy data

```yaml
modes:
  strict:  { require_attempt: true,  pass_score: 0.67, on_fail: retry_once_then_release, skip: false }
  lenient: { require_attempt: true,  pass_score: null, on_fail: release,                 skip: true  }
  free:    { quiz: false }
```

Adding a fourth mode later is a config entry, not a code change.

### Generation

Pre-generation runs headless in the Rust core while you read: finishing page N schedules
generation for the pages ahead, so the common case is a warm cache and an instant gate.
On a cache miss the on-demand path generates from the exact span. No API reachable means
Strict behaves as Lenient for that gate, with the reason displayed.

## Section 3 — Shipping & setup (default, pending review)

- Tauri bundler produces a signed MSI/NSIS installer (~3–10 MB). WebView2 is preinstalled
  on Windows 11, so there is no secondary dependency for the end user.
- **First-run wizard replaces the "one command":** the app launches, detects Claude Code and
  Codex installations, and offers to wire `~/.claude/settings.json` hooks and
  `~/.codex/config.toml` notify, plus register autostart. One click, no terminal.
- Hook wiring is additive and reversible — existing hook entries are preserved, and the
  wizard can unwire cleanly.
- Auto-update via the Tauri updater plugin.

## Section 4 — Hook wiring, verified against official docs (2026-09-11)

Checked against the Claude Code hooks reference and the Codex config reference rather
than assumed. Three findings changed the plan.

**Claude Code supports `type: "http"` hooks.** A hook entry can POST the payload straight
to a URL with an `Authorization` header, its secret named in `allowedEnvVars`. The daemon
can therefore receive deliveries directly, with no shell-script shim in between — which
also removes a per-hook process spawn from every prompt you submit. Hooks should be
registered `"async": true` so the daemon can never delay a turn.

**Codex `notify` cannot arm the reader.** It fires for exactly one event,
`agent-turn-complete`, with kebab-cased keys (`thread-id`, `turn-id`,
`last-assistant-message`) passed as the final argv argument. There is no turn-*start*
notification, so `notify` alone can only release the screen, never take it. Codex support
therefore depends on its lifecycle hooks (`[hooks.<Event>]` in `config.toml`), which use
the same event vocabulary as Claude Code: `UserPromptSubmit`, `Stop`, `PermissionRequest`,
`SessionStart`, `SessionEnd`, `Interrupt`, and more. Codex documents command and MCP-tool
handlers only — no HTTP handler — so Codex needs a `mytimeoff hook` shim binary that
forwards stdin to the loopback endpoint. That shim doubles as the fallback for Claude Code
versions predating HTTP hooks.

**Not every `Notification` means the agent is blocked on you.** The payload carries a
`notification_type`, and `idle_prompt` fires because *you* have gone quiet — which, while
you are reading, is exactly what is meant to be happening. Only `permission_prompt` and
`elicitation_dialog` raise the loud indicator; the rest are ignored. Treating all
notifications alike would have raised the loudest alert precisely when the tool was
working correctly.

Payload fields the daemon reads (everything else is ignored so new fields cannot break it):

| Event | Fields used | Meaning |
|---|---|---|
| `UserPromptSubmit` | `session_id` | Arm |
| `Stop`, `SessionEnd` | `session_id` | Turn over |
| `Notification` | `session_id`, `notification_type` | Blocked on you, if permission/elicitation |
| `PermissionRequest` (Codex) | `session_id` | Blocked on you |
| any | `agent_id` | Present only in subagents — ignored entirely |

Subagent deliveries are dropped: a subagent finishing does not mean the agent is ready for
you, and acting on it would clear the screen early.

## Section 5 — Local test rig (this repo only, 2026-09-11)

Hooks are wired in **this repository only**, never the global `~/.claude/settings.json`.
A bug in the daemon should be able to spoil one project's sessions, not every agent
session on the machine. Delete `.claude/settings.local.json` and the wiring is gone.

The token goes into the settings file **literally**, not as `$MYTIMEOFF_TOKEN` with
`allowedEnvVars`. The env-var form is the right shipping answer, but it needs the variable
to exist in the environment that launched the agent — which cannot be arranged for an
already-running session. `.claude/settings.local.json` is gitignored explicitly
(`*.local` does *not* match `settings.local.json`, which is the easy way to leak this).

Three events are wired, all to `POST /hook`, all `async: true` so a slow or dead daemon
can never stall a prompt:

| Event | Why |
|---|---|
| `UserPromptSubmit` | Arms the grace window |
| `Stop` | Turn over — raise the indicator |
| `Notification` | Only `permission_prompt` / `elicitation_dialog` count (see Section 4) |

### Running the rig

```
cargo run -p mytimeoff-daemon      # terminal 1 — must outlive the agent session
./scripts/watch-state.ps1          # terminal 2 — prints every state change
```

The watcher polls; the daemon does not. Polling is fine for an observation tool and wrong
for the product, which sleeps to the grace deadline instead.

### Known caveat

Claude Code reads hook configuration at startup, so the agent session must be restarted
after the settings file is written. A session that was already running when the file
appeared will not deliver anything.

## Developer prerequisites (Windows)

WebView2 runtime is already present on this machine. Required and currently missing:
rustup toolchain, and MSVC C++ build tools. One-time, dev-machine only; end users need
neither.

## Open questions

- Reader typography and theming — deferred until the reader renders real text.
- Whether exiting *before* the agent finishes should also raise the quiz (current default: yes,
  since the pages were genuinely read).
- Multi-monitor behaviour: which display the takeover claims.
