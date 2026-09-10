import type { Locator, PageView } from "./types";

export interface DwellTrackerOptions {
  /** Injectable clock so the tracker is testable without real time. */
  now: () => number;
  /**
   * Called as each page view closes.
   *
   * Reporting on close rather than at the end of a session means a crash, a killed
   * window or a lost connection costs at most the page you were on.
   */
  onView?: (view: PageView) => void;
}

interface OpenView {
  bookId: string;
  locator: Locator;
  text: string;
  enterTs: number;
  /** Visible milliseconds banked before the current visible stretch. */
  accumulatedMs: number;
  /** Timestamp the current visible stretch began, or null while hidden. */
  visibleSince: number | null;
}

/**
 * Turns raw page-change and visibility events into `PageView` records.
 *
 * Dwell counts only time the reader was actually visible, so a window left open
 * overnight cannot inflate the daily goal. That measurement has to happen here,
 * because only a surface knows whether it is on screen and focused.
 *
 * What this deliberately does *not* do is decide whether a page counted. That is the
 * daemon's call, against the threshold in the config - otherwise the window and a TUI
 * could disagree about the same day, and the quiz could be drawn from pages the goal
 * never counted.
 *
 * Free of any DOM or terminal dependency, so every reader surface shares one definition
 * of how dwell is measured.
 */
export class DwellTracker {
  private readonly nowFn: () => number;
  private readonly onView?: (view: PageView) => void;
  private readonly completed: PageView[] = [];
  private open: OpenView | null = null;
  private visible = true;

  constructor(options: DwellTrackerOptions) {
    this.nowFn = options.now;
    this.onView = options.onView;
  }

  /** Closes the current page view, if any, and begins one for `locator`. */
  enterPage(bookId: string, locator: Locator, text: string): void {
    this.closeOpenView();
    const now = this.nowFn();
    this.open = {
      bookId,
      locator,
      text,
      enterTs: now,
      accumulatedMs: 0,
      visibleSince: this.visible ? now : null,
    };
  }

  /** Reports reader visibility/focus. Repeated identical values are ignored. */
  setVisible(visible: boolean): void {
    if (visible === this.visible) return;
    this.visible = visible;

    if (!this.open) return;
    const now = this.nowFn();
    if (visible) {
      this.open.visibleSince = now;
    } else {
      this.open.accumulatedMs += this.visibleStretch(now);
      this.open.visibleSince = null;
    }
  }

  /** Closes any open view. Safe to call more than once. */
  finish(): void {
    this.closeOpenView();
  }

  get views(): readonly PageView[] {
    return this.completed;
  }

  private visibleStretch(now: number): number {
    const since = this.open?.visibleSince;
    return since === null || since === undefined ? 0 : now - since;
  }

  private closeOpenView(): void {
    if (!this.open) return;
    const now = this.nowFn();
    const dwellMs = this.open.accumulatedMs + this.visibleStretch(now);

    const view: PageView = {
      bookId: this.open.bookId,
      locator: this.open.locator,
      text: this.open.text,
      enterTs: this.open.enterTs,
      exitTs: now,
      dwellMs,
    };
    this.open = null;
    this.completed.push(view);
    this.onView?.(view);
  }
}
