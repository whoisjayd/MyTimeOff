import ePub, { type Book, type Rendition } from "epubjs";
import type { BookRenderer, Locator, RenderedPage } from "@mytimeoff/core";

/** Characters per generated location. Smaller means finer page granularity. */
const LOCATION_GRANULARITY = 1024;

/**
 * EPUB rendering via epub.js in paginated flow.
 *
 * Location generation is deliberately not awaited before first display: indexing a
 * large book takes seconds, and making the reader wait would defeat the point of an
 * instant switch. Page labels fall back to the raw CFI until the index is ready.
 */
export class EpubRenderer implements BookRenderer {
  private readonly source: ArrayBuffer | string;
  private readonly container: HTMLElement;
  private readonly start: Locator | undefined;
  private book?: Book;
  private rendition?: Rendition;
  private listeners: Array<(page: RenderedPage) => void> = [];
  private locationsReady = false;

  /**
   * `start` is where to open, from the last time this book was read.
   *
   * It is a constructor argument rather than one to `open()` because the interface
   * deliberately takes nothing at open time - a terminal renderer and a DOM one are
   * given what they need when they are made, and `open()` stays the same sentence for
   * both.
   */
  constructor(source: ArrayBuffer | string, container: HTMLElement, start?: Locator) {
    this.source = source;
    this.container = container;
    this.start = start;
  }

  async open(): Promise<void> {
    const book = ePub(this.source as ArrayBuffer);
    this.book = book;

    const rendition = book.renderTo(this.container, {
      width: "100%",
      height: "100%",
      flow: "paginated",
      spread: "auto",
    });
    this.rendition = rendition;

    rendition.on("relocated", () => {
      void this.emitCurrentPage();
    });

    // A CFI is only meaningful inside the book it came from. A file swapped for another
    // edition under the same name would make this throw, and refusing to open a book
    // over a stale bookmark would be a worse answer than opening it at the beginning.
    const at = this.start?.kind === "cfi" ? this.start.cfi : undefined;
    try {
      await rendition.display(at);
    } catch {
      await rendition.display();
    }

    // Index in the background so the first page is instant.
    void book.locations
      .generate(LOCATION_GRANULARITY)
      .then(() => {
        this.locationsReady = true;
      })
      .catch(() => {
        // A failed index only costs us nice page numbers, not the ability to read.
        this.locationsReady = false;
      });
  }

  async next(): Promise<void> {
    await this.rendition?.next();
  }

  async prev(): Promise<void> {
    await this.rendition?.prev();
  }

  onPageChange(listener: (page: RenderedPage) => void): void {
    this.listeners.push(listener);
  }

  totalPages(): number | undefined {
    if (!this.locationsReady) return undefined;
    const total = this.book?.locations.length();
    return typeof total === "number" && total > 0 ? total : undefined;
  }

  destroy(): void {
    this.listeners = [];
    this.rendition?.destroy();
    void this.book?.destroy();
  }

  /** Reads the current CFI and visible text, then notifies listeners. */
  private async emitCurrentPage(): Promise<void> {
    const rendition = this.rendition;
    if (!rendition) return;

    const cfi = rendition.currentLocation() as unknown as {
      start?: { cfi?: string };
    } | null;
    const startCfi = cfi?.start?.cfi;
    if (!startCfi) return;

    const page: RenderedPage = {
      locator: this.toLocator(startCfi),
      text: this.visibleText(),
    };
    for (const listener of this.listeners) listener(page);
  }

  private toLocator(cfi: string): Locator {
    let pageLabel = cfi;
    if (this.locationsReady) {
      const index = this.book?.locations.locationFromCfi(cfi);
      if (typeof index === "number") pageLabel = String(index);
    }
    return { kind: "cfi", cfi, pageLabel };
  }

  /**
   * Extracts text from the rendered iframe(s). `getContents()` returns an array in
   * current epub.js but a single object in older builds, so both are handled.
   */
  private visibleText(): string {
    const raw = this.rendition?.getContents() as unknown;
    if (!raw) return "";

    const contentsList = Array.isArray(raw) ? raw : [raw];
    const parts: string[] = [];
    for (const contents of contentsList) {
      const body = (contents as { document?: Document })?.document?.body;
      const text = body?.innerText ?? body?.textContent ?? "";
      if (text.trim()) parts.push(text.trim());
    }
    return parts.join("\n\n");
  }
}
