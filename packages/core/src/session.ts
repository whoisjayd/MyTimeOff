import type { PageView } from "./types";
import { DwellTracker } from "./dwell";
import { ALWAYS_VISIBLE, type BookRenderer, type VisibilitySource } from "./renderer";

export interface ReadingSessionOptions {
  bookId: string;
  renderer: BookRenderer;
  /** Defaults to always-visible; DOM surfaces should pass a real source. */
  visibility?: VisibilitySource;
  /**
   * Where finished page views go. A port, not a client: this module knows nothing
   * about HTTP, and a test needs no server to check that reading is reported.
   */
  onPageView?: (view: PageView) => void;
  tracker?: DwellTracker;
  /** Injectable so tests need no real clock. */
  now?: () => number;
}

/**
 * Owns one stretch of reading: drives the renderer, and converts page changes and
 * surface visibility into dwell records.
 *
 * Visibility matters as much as page turns here. The reader is raised and hidden by
 * the daemon rather than by the user, so without pausing on hide, time spent back in
 * the terminal would be counted as reading.
 */
export class ReadingSession {
  private readonly bookId: string;
  private readonly renderer: BookRenderer;
  private readonly tracker: DwellTracker;
  private readonly visibility: VisibilitySource;
  private unsubscribe?: () => void;
  private finished = false;

  constructor(options: ReadingSessionOptions) {
    this.bookId = options.bookId;
    this.renderer = options.renderer;
    this.visibility = options.visibility ?? ALWAYS_VISIBLE;
    this.tracker =
      options.tracker ??
      new DwellTracker({
        now: options.now ?? (() => Date.now()),
        ...(options.onPageView ? { onView: options.onPageView } : {}),
      });

    this.renderer.onPageChange((page) => {
      this.tracker.enterPage(this.bookId, page.locator, page.text);
    });
  }

  /** Opens the book and begins tracking. */
  async start(): Promise<void> {
    this.unsubscribe = this.visibility.subscribe((visible) => {
      this.tracker.setVisible(visible);
    });
    await this.renderer.open();
  }

  next(): Promise<void> {
    return this.renderer.next();
  }

  prev(): Promise<void> {
    return this.renderer.prev();
  }

  /** Every page view of this session, reported or not. */
  get views(): readonly PageView[] {
    return this.tracker.views;
  }

  /** Closes the open page view - which reports it - and unsubscribes. Safe twice. */
  finish(): void {
    if (this.finished) return;
    this.finished = true;
    this.tracker.finish();
    this.unsubscribe?.();
    this.unsubscribe = undefined;
  }
}
