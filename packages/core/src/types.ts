/** Domain types shared by every reader surface and the Rust core. */

export type BookFormat = "epub" | "pdf";

export interface Book {
  id: string;
  format: BookFormat;
  title: string;
  author?: string;
  /** Absolute path on disk. */
  path: string;
  /** Total pages for PDF; for EPUB this is the generated location count. */
  totalPages?: number;
}

/**
 * A locator identifies a position in a book in a format-appropriate way.
 * EPUB uses CFI strings; PDF uses 1-based page numbers.
 */
export type Locator =
  | { kind: "cfi"; cfi: string; pageLabel: string }
  | { kind: "page"; page: number; pageLabel: string };

/**
 * One contiguous visit to one page. Dwell excludes time the reader was hidden or
 * unfocused, so an abandoned open window cannot inflate progress.
 */
export interface PageView {
  bookId: string;
  locator: Locator;
  /** Visible text of the page, used to generate questions. */
  text: string;
  enterTs: number;
  exitTs: number;
  /** Milliseconds actually visible and focused. */
  dwellMs: number;
}

/** A page-view enriched with the classification the goal and quiz depend on. */
export interface ClassifiedPageView extends PageView {
  /** False when dwell fell under the skim threshold. */
  counted: boolean;
}

export type ReaderMode = "strict" | "lenient" | "free";

export interface ModePolicy {
  quiz: boolean;
  requireAttempt: boolean;
  /** Fraction of questions that must be correct; null means no pass gate. */
  passScore: number | null;
  onFail: "retry_once_then_release" | "release";
  skippable: boolean;
}

export const MODE_POLICIES: Record<ReaderMode, ModePolicy> = {
  strict: {
    quiz: true,
    requireAttempt: true,
    passScore: 0.67,
    onFail: "retry_once_then_release",
    skippable: false,
  },
  lenient: {
    quiz: true,
    requireAttempt: true,
    passScore: null,
    onFail: "release",
    skippable: true,
  },
  free: {
    quiz: false,
    requireAttempt: false,
    passScore: null,
    onFail: "release",
    skippable: true,
  },
};

export interface QuizQuestion {
  id: string;
  prompt: string;
  choices: string[];
  /** Index into `choices`. */
  answerIndex: number;
  /** Locator of the page this question was drawn from. */
  source: Locator;
}

export interface Quiz {
  id: string;
  bookId: string;
  questions: QuizQuestion[];
  /** Page locators the quiz was drawn from. */
  span: Locator[];
  generatedAt: number;
  /** True when served from the pre-generation cache rather than on demand. */
  preGenerated: boolean;
}

/** Daemon-side reading session, bound to the agent session that triggered it. */
export type SessionState = "idle" | "armed" | "reading" | "ready" | "gate";

export interface AgentEvent {
  kind: "agent_start" | "agent_done" | "agent_blocked";
  /** Agent session id, so one window's completion cannot clear another's switch. */
  sessionId: string;
  source: "claude-code" | "codex";
  ts: number;
}
