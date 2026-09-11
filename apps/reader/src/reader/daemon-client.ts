import type {
  AgentKey,
  AgentWiring,
  Answer,
  AskedQuestion,
  Book,
  Gate,
  Locator,
  PageView,
  PageViewReceipt,
  Progress,
  Refusal,
  Release,
  Resume,
  Verdict,
} from "@mytimeoff/core";

/**
 * The reader's side of the daemon's HTTP API.
 *
 * Two conventions meet here and neither should leak: the wire is snake_case, matching
 * the hook payloads and the Rust records, while everything in TypeScript is camelCase.
 * The translation lives in this file and nowhere else, so a field renamed on the wire
 * is a one-file change rather than a search.
 *
 * The base path goes through the dev server's proxy, which attaches the token. The
 * browser never sees the secret - see vite.config.ts.
 */
const BASE = "/daemon";

/** Wire shapes. Named for what they are so nothing here reads like a domain type. */
interface WireLocator {
  kind: "cfi" | "page";
  cfi?: string;
  page?: number;
  page_label: string;
}

function wireLocator(locator: Locator): WireLocator {
  return locator.kind === "cfi"
    ? { kind: "cfi", cfi: locator.cfi, page_label: locator.pageLabel }
    : { kind: "page", page: locator.page, page_label: locator.pageLabel };
}

function readLocator(wire: WireLocator): Locator {
  return wire.kind === "cfi"
    ? { kind: "cfi", cfi: wire.cfi ?? "", pageLabel: wire.page_label }
    : { kind: "page", page: wire.page ?? 0, pageLabel: wire.page_label };
}

async function send(path: string, body?: unknown, init?: RequestInit): Promise<Response> {
  const response = await fetch(`${BASE}/${path}`, {
    method: body === undefined ? "GET" : "POST",
    ...(body === undefined
      ? {}
      : { headers: { "content-type": "application/json" }, body: JSON.stringify(body) }),
    ...init,
  });
  if (!response.ok) {
    throw new Error(`${path} failed: ${response.status} ${await response.text()}`);
  }
  return response;
}

/**
 * Registers a book, or refreshes what the daemon knows about one.
 *
 * Has to happen before any page view is reported: the daemon refuses reading against a
 * book it has never heard of, which is what stops a typo in a book id from quietly
 * becoming a second, titleless book.
 */
export async function reportBook(book: Book): Promise<void> {
  await send("book", {
    id: book.id,
    format: book.format,
    title: book.title,
    author: book.author ?? null,
    path: book.path ?? null,
    total_pages: book.totalPages ?? null,
  });
}

/**
 * Reports one finished page view and returns the daemon's verdict on it.
 *
 * `stored: false` means the daemon had already recorded this visit. That is the answer
 * to a retry, not an error, and a caller must not treat it as one.
 */
export async function reportPageView(
  view: PageView,
  options: { keepalive?: boolean } = {},
): Promise<PageViewReceipt> {
  const response = await send(
    "page-view",
    {
      book_id: view.bookId,
      locator: wireLocator(view.locator),
      text: view.text,
      entered_at: view.enterTs,
      exited_at: view.exitTs,
      dwell_ms: view.dwellMs,
    },
    // The last page of a session is reported while the window is closing, and a plain
    // fetch is cancelled the moment it does. `keepalive` is what lets that page count -
    // and what makes the position it resumes to the page you were actually on.
    options.keepalive ? { keepalive: true } : undefined,
  );
  const receipt = (await response.json()) as { counted: boolean; stored: boolean };
  return { counted: receipt.counted, stored: receipt.stored };
}

/** Today's progress, as the daemon counts it - which is the only count that matters. */
export async function progress(): Promise<Progress> {
  const response = await send("progress");
  const body = (await response.json()) as { pages_today: number; goal: number; met: boolean };
  return { pagesToday: body.pages_today, goal: body.goal, met: body.met };
}

/**
 * The gate's wire shapes, written as discriminated unions because that is exactly what
 * serde produces from the Rust enums: `#[serde(tag = "gate")]` and `tag = "outcome"`.
 * Mirroring the tag here means the mapping below needs no non-null assertions - the
 * shape of an "open" gate is known from its own tag.
 */
interface WireQuestion {
  id: string;
  prompt: string;
  choices: string[];
  source: WireLocator;
}

interface WireScore {
  correct: number;
  answered: number;
  total: number;
}

type WireGate =
  | {
      gate: "open";
      quiz_id: string;
      questions: WireQuestion[];
      policy: {
        quiz: boolean;
        require_attempt: boolean;
        pass_mark: { correct: number; of: number } | null;
        retries: number;
        skippable: boolean;
      };
      attempts_left: number;
    }
  | { gate: "released"; reason: Release };

type WireVerdict =
  | { outcome: "released"; reason: Release; score: WireScore | null }
  | { outcome: "retry"; score: WireScore; attempts_left: number }
  | { outcome: "refused"; reason: Refusal };

function readQuestion(wire: WireQuestion): AskedQuestion {
  return {
    id: wire.id,
    prompt: wire.prompt,
    choices: wire.choices,
    source: readLocator(wire.source),
  };
}

function readVerdict(wire: WireVerdict): Verdict {
  switch (wire.outcome) {
    case "retry":
      return { outcome: "retry", score: wire.score, attemptsLeft: wire.attempts_left };
    case "refused":
      return { outcome: "refused", reason: wire.reason };
    case "released":
      return { outcome: "released", reason: wire.reason, score: wire.score };
  }
}

/**
 * Asks what the gate wants. Only meaningful while one is open; anything else is a 409,
 * which `send` turns into a throw.
 *
 * Asking twice returns the same quiz, retries and all, so a reader that reloads mid-gate
 * does not hand itself a fresh set of attempts.
 */
export async function gate(): Promise<Gate> {
  const wire = (await (await send("gate")).json()) as WireGate;
  if (wire.gate === "released") {
    return { gate: "released", reason: wire.reason };
  }
  return {
    gate: "open",
    quizId: wire.quiz_id,
    questions: wire.questions.map(readQuestion),
    policy: {
      quiz: wire.policy.quiz,
      requireAttempt: wire.policy.require_attempt,
      passMark: wire.policy.pass_mark,
      retries: wire.policy.retries,
      skippable: wire.policy.skippable,
    },
    attemptsLeft: wire.attempts_left,
  };
}

/** Submits an answer sheet. The daemon marks it; nothing here knows the answers. */
export async function answer(quizId: string, answers: Answer[]): Promise<Verdict> {
  const response = await send("gate/answers", {
    quiz_id: quizId,
    answers: answers.map((one) => ({ question_id: one.questionId, choice: one.choice })),
  });
  return readVerdict((await response.json()) as WireVerdict);
}

/** "I would rather not." Whether that works is the mode's answer, not this button's. */
export async function skip(): Promise<Verdict> {
  const response = await send("gate/skip", {});
  return readVerdict((await response.json()) as WireVerdict);
}

/**
 * The book the daemon kept, and where it was left - or null on a first run.
 *
 * A reader asks this before it draws anything. It is the difference between a tool that
 * demands a file every launch and one where the book is simply open.
 */
export async function resume(): Promise<Resume | null> {
  const response = await send("library");
  // 204: nothing has been read on this machine yet, which is an answer rather than a
  // failure - so it must not be a throw, and `json()` on an empty body would be one.
  if (response.status === 204) return null;
  const wire = (await response.json()) as {
    book: {
      id: string;
      format: "epub" | "pdf";
      title: string;
      author: string | null;
      path: string | null;
      total_pages: number | null;
    };
    at: WireLocator | null;
  };
  return {
    book: {
      id: wire.book.id,
      format: wire.book.format,
      title: wire.book.title,
      ...(wire.book.author ? { author: wire.book.author } : {}),
      ...(wire.book.path ? { path: wire.book.path } : {}),
      ...(wire.book.total_pages ? { totalPages: wire.book.total_pages } : {}),
    },
    at: wire.at ? readLocator(wire.at) : null,
  };
}

/** The kept book's bytes, ready to hand straight to a renderer. */
export async function bookBytes(): Promise<ArrayBuffer> {
  return await (await send("library/file")).arrayBuffer();
}

/**
 * Hands the daemon the bytes of the book just opened, so this is the last time it has to
 * be chosen.
 *
 * The id travels with them because the daemon refuses bytes for a book that is no longer
 * the open one - otherwise a second book opened in the gap between registering and
 * uploading would quietly be given the first one's pages.
 */
export async function keepBook(bookId: string, bytes: ArrayBuffer): Promise<void> {
  await send(`library?id=${encodeURIComponent(bookId)}`, undefined, {
    method: "POST",
    headers: { "content-type": "application/octet-stream" },
    body: bytes,
  });
}

/**
 * The agent wiring, as the daemon reads it off disk.
 *
 * An endpoint rather than something the window does for itself, because this page is the
 * same page in a browser tab and inside the Tauri window, and only one of those has a
 * filesystem. What the window adds is a shorter path to it, not a different one.
 */
interface WireAgent {
  key: AgentKey;
  label: string;
  proven: boolean;
  present: boolean;
  path: string;
  events: string[];
  wired: { event: string; target: string; current: boolean }[];
  complete: boolean;
  trouble: string | null;
}

function readAgent(wire: WireAgent): AgentWiring {
  return {
    key: wire.key,
    label: wire.label,
    proven: wire.proven,
    present: wire.present,
    path: wire.path,
    events: wire.events,
    hooks: wire.wired,
    complete: wire.complete,
    trouble: wire.trouble,
  };
}

/** What every agent's settings say about MyTimeOff right now. */
export async function agents(): Promise<AgentWiring[]> {
  const wire = (await (await send("agents")).json()) as WireAgent[];
  return wire.map(readAgent);
}

/** What connecting changed, so the panel can say it without asking again. */
export interface Connected {
  agent: AgentWiring;
  /** The copy kept beside the settings, where there was something to copy. */
  backup: string | null;
}

/** Writes MyTimeOff into one agent's settings, replacing any wiring already there. */
export async function connectAgent(key: AgentKey): Promise<Connected> {
  const response = await send(`agents/${encodeURIComponent(key)}`, {});
  const wire = (await response.json()) as {
    replaced: boolean;
    backup: string;
    agent: WireAgent;
  };
  return { agent: readAgent(wire.agent), backup: wire.replaced ? wire.backup : null };
}

/** Takes it back out, and says how many hooks there were to take. */
export async function disconnectAgent(key: AgentKey): Promise<{ agent: AgentWiring; removed: number }> {
  const response = await send(`agents/${encodeURIComponent(key)}`, undefined, {
    method: "DELETE",
  });
  const wire = (await response.json()) as { removed: number; agent: WireAgent };
  return { agent: readAgent(wire.agent), removed: wire.removed };
}
