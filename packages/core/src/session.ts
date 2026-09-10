import type { ClassifiedPageView } from "./types";
import { DwellTracker } from "./dwell";
import { ALWAYS_VISIBLE, type BookRenderer, type VisibilitySource } from "./renderer";

export interface ReadingSessionOptions {
  bookId: string;
  renderer: BookRenderer;
  /** Defaults to always-visible; DOM surfaces should pass a real source. */
  visibility?: VisibilitySource;
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
      options.tracker ?? new DwellTracker({ now: options.now ?? (() => Date.now()) });

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

  /** Pages that met the dwell threshold - what the daily goal counts. */
  countedPageCount(): number {
    return this.tracker.countedPages().length;
  }

  /** The page views a quiz may draw from. Empty means nothing was really read. */
  quizSpan(): ClassifiedPageView[] {
    return this.tracker.quizSpan();
  }

  /** Closes the open page view and unsubscribes. Safe to call twice. */
  finish(): ClassifiedPageView[] {
    if (!this.finished) {
      this.finished = true;
      this.tracker.finish();
      this.unsubscribe?.();
      this.unsubscribe = undefined;
    }
    return this.quizSpan();
  }
}
