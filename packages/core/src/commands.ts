/**
 * The daemon's half of the conversation, as data.
 *
 * The Rust core decides *what should be true* and emits commands; this reduces those
 * commands to the state a surface should be in. Keeping it here rather than in the DOM
 * means the window, a TUI, and a tray icon all agree on what "the indicator is up" means,
 * and that the agreement is testable without a browser.
 */

export const READER_COMMANDS = [
  "show_reader",
  "hide_reader",
  "indicator_done",
  "indicator_needs_input",
  "clear_indicator",
  "start_quiz",
] as const;

export type ReaderCommand = (typeof READER_COMMANDS)[number];

const KNOWN = new Set<string>(READER_COMMANDS);

/**
 * Commands arrive over a socket, so they are untrusted strings until checked.
 *
 * An unknown name means the daemon is newer than this surface. Returning undefined lets
 * the caller ignore it and carry on, which is better than throwing: a surface that dies
 * on an unrecognised command is a surface that a daemon upgrade can brick.
 */
export function parseCommand(raw: string): ReaderCommand | undefined {
  return KNOWN.has(raw) ? (raw as ReaderCommand) : undefined;
}

export type Indicator = "done" | "needs_input";

/** Everything a surface needs to render, derived only from commands. */
export interface SurfaceState {
  /** The reader has the screen. */
  reading: boolean;
  /** The agent wants you back, or is blocked. Null when there is nothing to report. */
  indicator: Indicator | null;
  /** The exit quiz is in progress. */
  quiz: boolean;
}

export const AT_REST: SurfaceState = { reading: false, indicator: null, quiz: false };

export function applyCommand(state: SurfaceState, command: ReaderCommand): SurfaceState {
  switch (command) {
    case "show_reader":
      return { ...state, reading: true };
    // Leaving the reader ends everything it was showing. A stale indicator surviving a
    // hide would reappear on the next takeover, answering a turn that ended long ago.
    case "hide_reader":
      return AT_REST;
    case "indicator_done":
      return { ...state, indicator: "done" };
    case "indicator_needs_input":
      return { ...state, indicator: "needs_input" };
    case "clear_indicator":
      return { ...state, indicator: null };
    // The gate only exists on the way out of the reader, so a quiz implies the screen.
    // The daemon always precedes it with show_reader; this makes that impossible to get
    // wrong if a command is ever dropped.
    case "start_quiz":
      return { ...state, reading: true, quiz: true };
  }
}

/** Folds a burst of commands - a reconnect replay, say - in order. */
export function applyCommands(
  state: SurfaceState,
  commands: readonly ReaderCommand[],
): SurfaceState {
  return commands.reduce(applyCommand, state);
}
