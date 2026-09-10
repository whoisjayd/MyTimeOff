/** Domain types shared by every reader surface and the Rust core. */

export type BookFormat = "epub" | "pdf";

export interface Book {
  id: string;
  format: BookFormat;
  title: string;
  author?: string;
  /** Absolute path on disk, where the surface knows it. A browser does not. */
  path?: string;
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
 *
 * There is no `counted` field. Whether a visit was reading or page-turning is decided
 * by the daemon, which owns the threshold and the history; a surface that classified
 * its own reading could disagree with the day it is reporting into.
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

/**
 * Where reading picks up: the book the daemon kept, and the last place it was left.
 *
 * This is what makes the book a fixture rather than something to choose again. A surface
 * asks for it before drawing anything, and only offers a picker when there is none.
 *
 * `at` is null for a book that was registered and never read - a resume onto page one,
 * not a failure.
 */
export interface Resume {
  book: Book;
  at: Locator | null;
}

/** The daemon's answer to a reported page view. */
export interface PageViewReceipt {
  /** False when dwell fell under the skim threshold: seen, not read. */
  counted: boolean;
  /** False when this visit had already been recorded, which is not an error. */
  stored: boolean;
}

/** How today is going, as the daemon sees it. */
export interface Progress {
  pagesToday: number;
  goal: number;
  met: boolean;
}

export type ReaderMode = "strict" | "lenient" | "free";

/**
 * What the gate will accept, decided by the daemon and sent with the quiz.
 *
 * The values are deliberately not written down here. Every one of them is a rule the
 * daemon enforces, and a second copy in a surface could only ever become a copy that
 * disagrees: a skip button drawn for a mode that refuses skips, or a pass mark shown
 * that is not the one being marked against. The surface asks; it does not know.
 */
export interface Policy {
  quiz: boolean;
  requireAttempt: boolean;
  /** Correct answers needed, as a ratio. Null when nothing has to be right. */
  passMark: { correct: number; of: number } | null;
  retries: number;
  skippable: boolean;
}

/**
 * A question as it is asked. There is no answer index, because the answer key never
 * leaves the daemon - marking happens where the questions were made.
 */
export interface AskedQuestion {
  id: string;
  prompt: string;
  choices: string[];
  /** Locator of the page this question was drawn from. */
  source: Locator;
}

/** Why the gate let you through. */
export type Release = "passed" | "attempted" | "exhausted" | "skipped" | "ungated";

/** Why it did not, without spending an attempt. */
export type Refusal = "not_attempted" | "wrong_quiz" | "not_skippable";

/** What is standing between the reader and the screen they came from. */
export type Gate =
  | {
      gate: "open";
      quizId: string;
      questions: AskedQuestion[];
      policy: Policy;
      attemptsLeft: number;
    }
  | { gate: "released"; reason: Release };

/** One answer: which choice, for which question. Unanswered questions are simply absent. */
export interface Answer {
  questionId: string;
  choice: number;
}

export interface Score {
  correct: number;
  /** Questions with an answer on the sheet, which may be fewer than `total`. */
  answered: number;
  total: number;
}

/** The daemon's ruling on an answer sheet or a skip. */
export type Verdict =
  | { outcome: "released"; reason: Release; score: Score | null }
  | { outcome: "retry"; score: Score; attemptsLeft: number }
  | { outcome: "refused"; reason: Refusal };

/** Daemon-side reading session, bound to the agent session that triggered it. */
export type SessionState = "idle" | "armed" | "reading" | "ready" | "gate";

export interface AgentEvent {
  kind: "agent_start" | "agent_done" | "agent_blocked";
  /** Agent session id, so one window's completion cannot clear another's switch. */
  sessionId: string;
  source: "claude-code" | "codex";
  ts: number;
}
