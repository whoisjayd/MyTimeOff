import * as pdfjs from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.mjs?url";
import type { BookRenderer, Locator, RenderedPage } from "@mytimeoff/core";

pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;

/**
 * PDF rendering via pdf.js onto a canvas, one page at a time.
 *
 * Unlike EPUB, a PDF page is a fixed unit, so "page" is unambiguous and no location
 * index is needed. Text comes from the embedded text layer; scanned PDFs with no text
 * layer yield an empty string, which the quiz layer treats as an unquizzable page
 * rather than inventing questions about it.
 */
export class PdfRenderer implements BookRenderer {
  private readonly source: ArrayBuffer;
  private readonly container: HTMLElement;
  private readonly start: Locator | undefined;
  private doc?: pdfjs.PDFDocumentProxy;
  private loadingTask?: pdfjs.PDFDocumentLoadingTask;
  private canvas?: HTMLCanvasElement;
  private listeners: Array<(page: RenderedPage) => void> = [];
  private pageNumber = 1;
  /** Guards against overlapping renders when pages are turned quickly. */
  private renderToken = 0;

  /** `start` is where to open, from the last time this book was read. */
  constructor(source: ArrayBuffer, container: HTMLElement, start?: Locator) {
    this.source = source;
    this.container = container;
    this.start = start;
  }

  async open(): Promise<void> {
    const task = pdfjs.getDocument({ data: this.source });
    this.loadingTask = task;
    this.doc = await task.promise;

    const canvas = document.createElement("canvas");
    this.container.appendChild(canvas);
    this.canvas = canvas;

    await this.show(this.startPage(this.doc.numPages));
  }

  /**
   * The page to open at, which is page one unless there is somewhere to go back to.
   *
   * Clamped rather than trusted: a page number outlives the file it was saved from, and
   * a book re-exported shorter would otherwise open on nothing at all.
   */
  private startPage(total: number): number {
    if (this.start?.kind !== "page") return 1;
    return Math.min(Math.max(this.start.page, 1), total);
  }

  async next(): Promise<void> {
    const total = this.doc?.numPages ?? 1;
    if (this.pageNumber < total) await this.show(this.pageNumber + 1);
  }

  async prev(): Promise<void> {
    if (this.pageNumber > 1) await this.show(this.pageNumber - 1);
  }

  onPageChange(listener: (page: RenderedPage) => void): void {
    this.listeners.push(listener);
  }

  totalPages(): number | undefined {
    return this.doc?.numPages;
  }

  destroy(): void {
    this.listeners = [];
    this.canvas?.remove();
    void this.loadingTask?.destroy();
  }

  private async show(pageNumber: number): Promise<void> {
    const doc = this.doc;
    const canvas = this.canvas;
    if (!doc || !canvas) return;

    const token = ++this.renderToken;
    const page = await doc.getPage(pageNumber);

    // A newer page turn started while this one was loading; abandon this render.
    if (token !== this.renderToken) return;

    const unscaled = page.getViewport({ scale: 1 });
    const availWidth = this.container.clientWidth || unscaled.width;
    const availHeight = this.container.clientHeight || unscaled.height;
    // Fit the whole page inside the viewer rather than cropping either dimension.
    const fit = Math.min(availWidth / unscaled.width, availHeight / unscaled.height);
    const dpr = window.devicePixelRatio || 1;
    const viewport = page.getViewport({ scale: fit * dpr });

    canvas.width = Math.floor(viewport.width);
    canvas.height = Math.floor(viewport.height);
    // CSS size stays in layout pixels; the backing store carries the DPR detail.
    canvas.style.width = `${Math.floor(viewport.width / dpr)}px`;
    canvas.style.height = `${Math.floor(viewport.height / dpr)}px`;

    // Pass `canvas` alone: pdf.js treats `canvasContext` as a back-compat path and
    // requires canvas to be null when it is used.
    await page.render({ canvas, viewport }).promise;
    if (token !== this.renderToken) return;

    this.pageNumber = pageNumber;

    const locator: Locator = {
      kind: "page",
      page: pageNumber,
      pageLabel: String(pageNumber),
    };
    const rendered: RenderedPage = { locator, text: await this.textOf(page) };
    for (const listener of this.listeners) listener(rendered);
  }

  private async textOf(page: pdfjs.PDFPageProxy): Promise<string> {
    const content = await page.getTextContent();
    return content.items
      .map((item) => ("str" in item ? item.str : ""))
      .join(" ")
      .replace(/\s+/g, " ")
      .trim();
  }
}
