import {
  AT_REST,
  ReadingSession,
  applyCommand,
  type Book,
  type BookFormat,
  type BookRenderer,
  type Locator,
  type PageView,
  type Resume,
  type SurfaceState,
  type Verdict,
} from "@mytimeoff/core";
import { EpubRenderer } from "./reader/epub-renderer";
import { GateView } from "./reader/gate-view";
import { DomVisibility } from "./reader/dom-visibility";
import { DomSurface } from "./reader/dom-surface";
import { connectCommands } from "./reader/command-stream";
import * as daemon from "./reader/daemon-client";

const picker = document.querySelector<HTMLElement>("#picker")!;
const stage = document.querySelector<HTMLElement>("#stage")!;
const viewer = document.querySelector<HTMLElement>("#viewer")!;
const fileInput = document.querySelector<HTMLInputElement>("#file")!;
const progress = document.querySelector<HTMLElement>("#progress")!;
const errorBox = document.querySelector<HTMLElement>("#error")!;

let session: ReadingSession | undefined;
/**
 * Set once the window is going away, so the page you were on is reported with
 * `keepalive` instead of being cancelled mid-flight along with everything else.
 *
 * It is a flag rather than an argument because the report is raised by the dwell
 * tracker, which knows about pages and nothing about windows.
 */
let closing = false;
/** Last progress the daemon reported, so the line has something to show between fetches. */
let pagesToday = 0;
let pageGoal = 0;

/** Format from the file name; content sniffing is not worth it here. */
function formatOf(name: string): BookFormat {
  return name.toLowerCase().endsWith(".pdf") ? "pdf" : "epub";
}

/**
 * Builds the renderer for a format, opening at `at` when there is somewhere to go back
 * to. The PDF path is imported lazily so EPUB-only sessions never load pdf.js.
 */
async function rendererFor(
  format: BookFormat,
  data: ArrayBuffer,
  at: Locator | null,
): Promise<BookRenderer> {
  const start = at ?? undefined;
  if (format === "pdf") {
    const { PdfRenderer } = await import("./reader/pdf-renderer");
    return new PdfRenderer(data, viewer, start);
  }
  return new EpubRenderer(data, viewer, start);
}

function showError(message: string): void {
  errorBox.textContent = message;
  errorBox.hidden = false;
  // Failure must return you to a usable state, not a blank screen.
  picker.hidden = false;
  stage.hidden = true;
}

/**
 * The book as the daemon will know it.
 *
 * The id is the file name rather than a hash of the bytes: hashing a 40MB PDF to learn
 * something the name already tells us would delay the first page for nothing, and the
 * cost of being wrong is a renamed file counting as a new book.
 */
function describe(file: File): Book {
  return {
    id: file.name,
    format: formatOf(file.name),
    title: file.name.replace(/\.(epub|pdf)$/i, ""),
  };
}

/** Draws the status line from the daemon's count, not from anything measured here. */
function showProgress(pageLabel: string, total: number | undefined): void {
  const read = pageGoal ? `${pagesToday}/${pageGoal} today` : `${pagesToday} today`;
  progress.textContent = total
    ? `${pageLabel} / ${total} · ${read}`
    : `${pageLabel} · ${read} · indexing…`;
}

/** Pulls today's count from the daemon. Silent on failure: it is a status line. */
async function refreshProgress(): Promise<void> {
  try {
    const today = await daemon.progress();
    pagesToday = today.pagesToday;
    pageGoal = today.goal;
  } catch (error: unknown) {
    console.warn("[mytimeoff] could not read progress", error);
  }
}

/**
 * Opens a book that is already in hand, from wherever it came.
 *
 * Both entry points end here - the file someone chose and the book the daemon kept - so
 * a restored session is the same session in every respect, not a lesser one.
 */
async function openBook(book: Book, data: ArrayBuffer, at: Locator | null): Promise<void> {
  errorBox.hidden = true;
  const renderer = await rendererFor(book.format, data, at);

  session = new ReadingSession({
    bookId: book.id,
    renderer,
    visibility: new DomVisibility(),
    onPageView: (view) => void report(view),
  });
  renderer.onPageChange((page) => {
    showProgress(page.locator.pageLabel, renderer.totalPages());
  });

  picker.hidden = true;
  stage.hidden = false;
  // start() subscribes to visibility before opening, so the first page is tracked.
  await session.start();
}

/**
 * Takes on a file as *the* book: registers it, hands over the bytes, then reads it.
 *
 * Registering comes first because the daemon refuses reading against a book it has not
 * been told about, and it is the same call that makes this the open book - so the bytes
 * that follow can only ever be stored against the book they belong to.
 */
async function adopt(file: File): Promise<void> {
  errorBox.hidden = true;
  const data = await file.arrayBuffer();
  const book = describe(file);
  await daemon.reportBook(book);
  // The one upload there will ever be for this book. Everything after this is a reopen.
  await daemon.keepBook(book.id, data);
  await refreshProgress();
  await openBook(book, data, null);
}

/**
 * Reopens the kept book at the page it was left on.
 *
 * Nothing is registered and nothing is uploaded here: the daemon handed this book back,
 * so it already knows it and it is already the open one. A reopen that re-uploaded would
 * make every launch cost the size of the book for no new information.
 */
async function reopen(kept: Resume): Promise<void> {
  await refreshProgress();
  await openBook(kept.book, await daemon.bookBytes(), kept.at);
}

/**
 * Sends one finished page view and updates the line with what came back.
 *
 * A view that fails to send is dropped rather than queued. Reading is reported one page
 * at a time as it happens, so a lost report costs one page of a daily goal - and a retry
 * queue that outlives the window would be a second store, which is the thing the daemon
 * exists to avoid.
 */
async function report(view: PageView): Promise<void> {
  try {
    const receipt = await daemon.reportPageView(view, { keepalive: closing });
    if (receipt.counted && receipt.stored) pagesToday += 1;
  } catch (error: unknown) {
    console.warn("[mytimeoff] could not report a page view", error);
  }
}

/** Every entry point funnels through here so no failure can be silent. */
function handleFile(file: File): void {
  adopt(file).catch((error: unknown) => {
    const detail = error instanceof Error ? error.message : String(error);
    showError(`Could not open "${file.name}"

${detail}`);
    console.error("[mytimeoff] failed to open book", error);
  });
}

const surface = new DomSurface(document);
let surfaceState: SurfaceState = AT_REST;
surface.render(surfaceState);

const gateView = new GateView(document, {
  onSubmit: (quizId, answers) => void rule(() => daemon.answer(quizId, answers)),
  onSkip: () => void rule(() => daemon.skip()),
});

/**
 * Fetches the questions for a gate that has just opened.
 *
 * The screen is already taken by the time this runs - the daemon issued show_reader and
 * start_quiz before anything was generated - so a slow generator delays the questions and
 * never the takeover. If it fails outright, the gate says so; it does not sit blank.
 */
async function openGate(): Promise<void> {
  gateView.waiting();
  try {
    gateView.ask(await daemon.gate());
  } catch (error: unknown) {
    gateView.problem(error instanceof Error ? error.message : String(error));
    console.warn("[mytimeoff] could not open the gate", error);
  }
}

/** Sends a sheet or a skip and shows the ruling. Releasing the screen is the daemon's. */
async function rule(ask: () => Promise<Verdict>): Promise<void> {
  try {
    const verdict = await ask();
    gateView.verdict(verdict);
    // A sheet for a quiz the daemon has moved past: fetch the current one rather than
    // leaving a stale set of questions on screen that can never be accepted.
    if (verdict.outcome === "refused" && verdict.reason === "wrong_quiz") await openGate();
  } catch (error: unknown) {
    gateView.problem(error instanceof Error ? error.message : String(error));
  }
}

connectCommands({
  onCommand(command) {
    const next = applyCommand(surfaceState, command);
    // Only the transition matters. A replayed start_quiz on reconnect must not throw away
    // a half-filled answer sheet, and `gate()` would return the same quiz in any case.
    const opened = next.quiz && !surfaceState.quiz;
    // The same reasoning for the indicator: it *changing* is news, it being up is not, and
    // a reconnect would otherwise announce a turn that ended an hour ago.
    const raised =
      next.indicator !== null && next.indicator !== surfaceState.indicator ? next.indicator : null;
    surfaceState = next;
    surface.render(surfaceState);
    if (raised) surface.alert(raised);
    if (opened) void openGate();
  },
  onConnection: (connected) => surface.connection(connected),
});

/**
 * Tells the daemon the one thing the hooks cannot: what the person at the keyboard wants.
 *
 * Failures are logged rather than surfaced. These are fire-and-forget signals about
 * screen state, and a modal error about a failed POST would be a worse interruption than
 * the missed signal.
 */
function tell(path: string): void {
  void fetch(`/daemon/${path}`, { method: "POST" }).catch((error: unknown) => {
    console.warn(`[mytimeoff] could not reach the daemon: ${path}`, error);
  });
}

// "I want to leave" - which does not decide whether you may. The daemon answers that,
// with the gate in strict and lenient mode.
document.querySelector("#back")?.addEventListener("click", () => tell("exit"));

fileInput.addEventListener("change", () => {
  const file = fileInput.files?.[0];
  if (file) handleFile(file);
});

document.querySelector("#next")?.addEventListener("click", () => void session?.next());
document.querySelector("#prev")?.addEventListener("click", () => void session?.prev());

document.addEventListener("keydown", (event) => {
  if (event.key === "ArrowRight") void session?.next();
  if (event.key === "ArrowLeft") void session?.prev();
  if (event.key === "Escape") tell("exit");
});

/**
 * The window is going away: close the open page view so the page being read when it
 * closed is the page it comes back to - and so it counts towards the day.
 *
 * `pagehide` rather than `visibilitychange`, deliberately. The daemon hides this window
 * every time the agent wants the screen back, and closing the view on each of those
 * would throw away the rest of that page's dwell: nothing reopens a view until the page
 * turns.
 */
window.addEventListener("pagehide", () => {
  closing = true;
  session?.finish();
});

window.addEventListener("dragover", (event) => event.preventDefault());
window.addEventListener("drop", (event) => {
  event.preventDefault();
  const file = event.dataTransfer?.files?.[0];
  if (file) handleFile(file);
});

/**
 * The last thing this file does: ask whether there is a book to come back to.
 *
 * A failure here is not worth shouting about. The picker is already on screen and
 * choosing a file fixes it, which is a better answer than an error over an empty page.
 */
daemon
  .resume()
  .then(async (kept) => {
    if (kept) await reopen(kept);
  })
  .catch((error: unknown) => {
    console.warn("[mytimeoff] could not reopen the kept book", error);
  });
