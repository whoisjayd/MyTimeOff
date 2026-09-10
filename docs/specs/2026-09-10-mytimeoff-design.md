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

### What the first live test found (2026-09-11)

Hooks from a real Claude Code session were delivered and acted on — the first time
anything but a hand-sent payload has driven the machine.

It also exposed a bug no unit test could have caught, because every test started from
idle: `AgentStart` was only handled from `State::Idle`, so once a turn finished and the
indicator went up, the machine wedged in `Ready` and ignored every later prompt. Observed
as forty consecutive one-second samples of `ready` across an actively running turn. In
real use the tool would have worked exactly once per session.

`Ready + AgentStart` now returns to `Reading` and clears the indicator, with **no new
grace window**: grace exists to avoid *switching* for a turn too short to be worth it, and
that switch has already been paid for. Re-arming would strand you on the terminal for 20s
with the reader still up behind it.

The same test showed why `issued` is not enough to debug wiring: a hook that never arrives
and a hook that arrives and is correctly ignored look identical from outside. `GET /state`
now also reports `received` — every delivery, its `notification_type`, and the event it
mapped to (or `ignored`). The watcher prints those as they land.

## Section 6 — The command channel (2026-09-11)

Until now the machine decided to show the reader and nothing was listening. `GET /events`
is the other half: a server-sent event stream carrying one command per frame
(`show_reader`, `indicator_done`, ...), fanned out over a `tokio::sync::broadcast` so a
window, a TUI and a tray icon can all subscribe at once without the daemon tracking them.

Three decisions worth keeping:

**Resync on connect.** A subscriber is sent the commands that reproduce the *current*
state before it sees any live ones. A broadcast channel replays nothing, so a reader that
opens — or reconnects after a drop — mid-takeover would otherwise sit blank through it.
The subscription is taken before the state is read, so a command issued between the two is
duplicated rather than lost; every command is idempotent, which makes that the safe
direction to err in.

**Lag drops the subscriber's backlog, not its correctness.** A surface more than 32
commands behind has stopped keeping up; replaying what it missed is pointless when
reconnecting resyncs it from the state, which is the truth anyway.

**The browser holds no secret.** The reader talks to `/daemon/*` on its own origin and the
Vite dev proxy attaches the token, read per request from the same file the daemon uses.
`EventSource` was rejected for this: it cannot set headers, so the token would have to
travel in the query string, where it lands in logs and history.

The reducer that turns commands into surface state lives in `packages/core`
(`applyCommand`), not in the DOM — same rule as the Rust core, so a second surface cannot
disagree with the first about what "the indicator is up" means.

What this does *not* do: a web page cannot raise itself to the front. `show_reader` shows
as a banner here. Raising the window is a Tauri capability and `DomSurface` is the only
file that changes when it arrives.

## Section 7 — Config and the store (2026-09-11)

Two things came out of compile-time constants and into places that can change without a
rebuild: what the tool is set to do, and what it remembers.

**Config is a file the daemon reads, not a constant it was built with.** Mode, grace
window, skim threshold, daily goal and questions per gate all live in one TOML file with
defaults for every field, so a missing file is a working install rather than an error. It
is parsed in `crates/core` and read from disk in `crates/daemon` — the core stays I/O-free,
which is what lets the parsing be tested without a filesystem.

**One writer, one truth.** Reading history is a SQLite database in the daemon, not in the
browser. A surface measures dwell, because only it knows whether its window was visible and
focused; the daemon decides whether that dwell was reading, because only it knows the
threshold and the rest of the day. `PageView` in TypeScript therefore has no `counted`
field — a surface that classified its own reading could disagree with the day it is
reporting into.

**A page view is reported as it finishes, and never queued.** A lost report costs one page
of a daily goal. A retry queue that outlived the window would be a second store, which is
the thing having a daemon exists to avoid.

**Wall clock in the store, monotonic clock in the machine.** The grace window is measured
in `Instant` elapsed milliseconds, so an NTP correction or a DST change cannot misfire the
takeover. Page views are stamped in epoch milliseconds, because "which day did you read"
is a question about a calendar. Both clocks are in the daemon and neither leaks into the
other's job.

**Reading against an unregistered book is refused.** `POST /book` must precede any page
view for it. Accepting the view instead would quietly create a second, titleless book out
of a typo in an id.

**Migrations are `PRAGMA user_version`.** No migration framework, no dependency: the
schema version is an integer in the file, and each step is a numbered block that runs once.

The HTTP API is snake_case throughout, matching the Rust records and the hook payloads;
TypeScript stays camelCase. The two conventions meet in exactly one file,
`apps/reader/src/reader/daemon-client.ts`, so renaming a wire field is a one-file change
rather than a search.

## Section 8 — The gate (2026-09-11)

This is the part the whole tool rests on, so it is split by how certain each half can be.

**Rules in the core, questions in the daemon.** `crates/core/src/quiz.rs` holds what a
quiz is and who may leave: `Policy`, `Gate::submit`, `Gate::skip`, and the `Verdict` they
produce. It is pure, and it is the part that must never be wrong. Generating questions is
the opposite — it needs pages, a model and a network, and it will be replaced — so it sits
behind `trait QuestionSource` in the daemon, held as `Arc<dyn QuestionSource>`. Swapping
the offline stub for a real generator is one line in `main.rs`.

**The pass mark is an integer ratio, not a float.** This supersedes the `passScore: 0.67`
in Section 2, which was a bug caught by its own test: two correct out of three is
0.6666667, which is not `>= 0.67`, so strict mode failed a passing answer sheet. The bar is
now `PassMark { correct: 2, of: 3 }`, met by cross-multiplying integers. A gate that fails
someone who answered correctly is the single worst bug this tool could have, and floats
were the way to get it.

**A gate can never trap anyone.** Every failure path in generation — source unreachable,
database unreadable, nothing counted as read in this stretch — produces an empty quiz, and
an empty quiz releases immediately as `Release::Ungated`: *a tool that cannot pose a
question has not earned the right to hold the screen*. Strict mode's `require_attempt` is
guarded on `total > 0` for the same reason; without it, an empty quiz in strict mode was a
gate that could never be opened.

**The answer key never leaves the daemon.** `Quiz::for_display()` strips `answer_index`,
so what a surface receives is `AskedQuestion`. Marking happens where the questions were
made.

**Policy is served, not duplicated.** `GET /gate` returns the mode's policy alongside the
questions, and the reader draws its skip button from `policy.skippable`. The TypeScript
`MODE_POLICIES` table from Section 2 is deleted: a second copy of the rules could only ever
become a copy that disagrees — a skip button on a mode that refuses skips, or a pass mark
shown that is not the one being marked against.

**Skipping is answered from the mode, not from the quiz.** `POST /gate/skip` consults
`Policy::for_mode` directly, so pressing skip before the questions have arrived gets the
same answer as pressing it after, and strict mode's refusal never waits on a generator.

**Generation is lazy, and asking twice returns the same quiz.** The gate opens the instant
the exit is requested — the reader is on screen either way — and the questions are built on
the first `GET /gate`. A reader that reloads mid-gate is handed the quiz it was already
taking, spent retries and all; regenerating would hand out a fresh allowance. A new
`StartQuiz` clears the previous gate, so retries are never inherited either.

**What a gate asks about**: the pages counted as read since the reader took the screen
(stamped on `show_reader`), for the most recently read book, deduplicated by page — a page
revisited three times is one page to ask about — most recent ten, in reading order.

**A refusal costs nothing.** A blank sheet or a sheet for the wrong quiz is `Refused` and
does not spend an attempt. Only a marked attempt does.

The offline stub (`crates/daemon/src/quiz/stub.rs`) makes cloze questions: a real line from
a page with its longest distinctive word blanked, offered among words taken from the other
pages read. It is correct by construction — the right answer really was on that page, the
wrong ones really were not on that line — and it is deterministic, which is what makes it
testable and also what makes it the wrong thing to ship. A quiz you can memorise is not a
gate. Replacing it is step D.

## Developer prerequisites (Windows)

WebView2 runtime is already present on this machine. Required and currently missing:
rustup toolchain, and MSVC C++ build tools. One-time, dev-machine only; end users need
neither.

## Open questions

- Reader typography and theming — deferred until the reader renders real text.
- Whether exiting *before* the agent finishes should also raise the quiz (current default: yes,
  since the pages were genuinely read).
- Multi-monitor behaviour: which display the takeover claims.
