import { ReadingSession, type BookRenderer } from "@mytimeoff/core";
import { EpubRenderer } from "./reader/epub-renderer";
import { DomVisibility } from "./reader/dom-visibility";

const picker = document.querySelector<HTMLElement>("#picker")!;
const stage = document.querySelector<HTMLElement>("#stage")!;
const viewer = document.querySelector<HTMLElement>("#viewer")!;
const fileInput = document.querySelector<HTMLInputElement>("#file")!;
const progress = document.querySelector<HTMLElement>("#progress")!;
const errorBox = document.querySelector<HTMLElement>("#error")!;

let session: ReadingSession | undefined;

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

async function openBook(file: File): Promise<void> {
  errorBox.hidden = true;
  const renderer = await rendererFor(file, await file.arrayBuffer());

  session = new ReadingSession({
    bookId: file.name,
    renderer,
    visibility: new DomVisibility(),
  });
  renderer.onPageChange((page) => {
    const total = renderer.totalPages();
    const counted = session?.countedPageCount() ?? 0;
    progress.textContent = total
      ? `${page.locator.pageLabel} / ${total} · ${counted} read`
      : `${counted} read · indexing…`;
  });

  picker.hidden = true;
  stage.hidden = false;
  // start() subscribes to visibility before opening, so the first page is tracked.
  await session.start();
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

fileInput.addEventListener("change", () => {
  const file = fileInput.files?.[0];
  if (file) handleFile(file);
});

document.querySelector("#next")?.addEventListener("click", () => void session?.next());
document.querySelector("#prev")?.addEventListener("click", () => void session?.prev());

document.addEventListener("keydown", (event) => {
  if (event.key === "ArrowRight") void session?.next();
  if (event.key === "ArrowLeft") void session?.prev();
});

window.addEventListener("dragover", (event) => event.preventDefault());
window.addEventListener("drop", (event) => {
  event.preventDefault();
  const file = event.dataTransfer?.files?.[0];
  if (file) handleFile(file);
});
