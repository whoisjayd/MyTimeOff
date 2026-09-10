import {
  AT_REST,
  ReadingSession,
  applyCommand,
  type Book,
  type BookRenderer,
  type PageView,
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
/** Last progress the daemon reported, so the line has something to show between fetches. */
let pagesToday = 0;
let pageGoal = 0;

/**
 * Picks a renderer by file extension; content sniffing is not worth it here.
 * The PDF path is imported lazily so EPUB-only sessions never load pdf.js.
 */
async function rendererFor(file: File, data: ArrayBuffer): Promise<BookRenderer> {
  if (file.name.toLowerCase().endsWith(".pdf")) {
    const { PdfRenderer } = await import("./reader/pdf-renderer");
    return new PdfRenderer(data, viewer);
  }
  return new EpubRenderer(data, viewer);
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
function describe(file: File, renderer: BookRenderer): Book {
  const total = renderer.totalPages();
  return {
    id: file.name,
    format: file.name.toLowerCase().endsWith(".pdf") ? "pdf" : "epub",
    title: file.name.replace(/\.(epub|pdf)$/i, ""),
    ...(total ? { totalPages: total } : {}),
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

async function openBook(file: File): Promise<void> {
  errorBox.hidden = true;
  const renderer = await rendererFor(file, await file.arrayBuffer());
  const book = describe(file, renderer);
  // Before any page view: the daemon refuses reading against a book it has not been told
  // about, and that refusal is what keeps a mistyped id from becoming a phantom book.
  await daemon.reportBook(book);
  await refreshProgress();

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
 * Sends one finished page view and updates the line with what came back.
 *
 * A view that fails to send is dropped rather than queued. Reading is reported one page
 * at a time as it happens, so a lost report costs one page of a daily goal - and a retry
 * queue that outlives the window would be a second store, which is the thing the daemon
 * exists to avoid.
 */
async function report(view: PageView): Promise<void> {
  try {
    const receipt = await daemon.reportPageView(view);
    if (receipt.counted && receipt.stored) pagesToday += 1;
  } catch (error: unknown) {
    console.warn("[mytimeoff] could not report a page view", error);
  }
}

/** Every entry point funnels through here so no failure can be silent. */
function handleFile(file: File): void {
  openBook(file).catch((error: unknown) => {
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
    surfaceState = next;
    surface.render(surfaceState);
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

window.addEventListener("dragover", (event) => event.preventDefault());
window.addEventListener("drop", (event) => {
  event.preventDefault();
  const file = event.dataTransfer?.files?.[0];
  if (file) handleFile(file);
});
