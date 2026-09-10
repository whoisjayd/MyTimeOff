import type { ClassifiedPageView, Locator } from "./types";

export interface DwellTrackerOptions {
  /** Injectable clock so the tracker is testable without real time. */
  now: () => number;
  /** Dwell below this is treated as a flip-past rather than a read. */
  skimThresholdMs?: number;
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

export const DEFAULT_SKIM_THRESHOLD_MS = 3000;

/**
 * Turns raw page-change and visibility events into `ClassifiedPageView` records.
 *
 * Dwell counts only time the reader was actually visible, so a window left open
 * overnight cannot inflate the daily goal, and pages flipped past faster than the
 * skim threshold are excluded from both the goal and the quiz span. Those two
 * rules are the whole anti-cheat story: flipping cannot earn progress, and it
 * cannot dodge questions either, because questions only come from counted pages.
 *
 * Deliberately free of any DOM or terminal dependency so every reader surface
 * shares one definition of what counts as having read a page.
 */
export class DwellTracker {
  private readonly nowFn: () => number;
  private readonly skimThresholdMs: number;
  private readonly completed: ClassifiedPageView[] = [];
  private open: OpenView | null = null;
  private visible = true;

  constructor(options: DwellTrackerOptions) {
    this.nowFn = options.now;
    this.skimThresholdMs = options.skimThresholdMs ?? DEFAULT_SKIM_THRESHOLD_MS;
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

  get views(): readonly ClassifiedPageView[] {
    return this.completed;
  }

  /** Unique page labels that met the dwell threshold, in first-read order. */
  countedPages(): string[] {
    const seen = new Set<string>();
    const labels: string[] = [];
    for (const view of this.completed) {
      if (!view.counted) continue;
      const label = view.locator.pageLabel;
      if (seen.has(label)) continue;
      seen.add(label);
      labels.push(label);
    }
    return labels;
  }

  /** The page views a quiz may draw from: counted pages only, in reading order. */
  quizSpan(): ClassifiedPageView[] {
    return this.completed.filter((view) => view.counted);
  }

  private visibleStretch(now: number): number {
    const since = this.open?.visibleSince;
    return since === null || since === undefined ? 0 : now - since;
  }

  private closeOpenView(): void {
    if (!this.open) return;
    const now = this.nowFn();
    const dwellMs = this.open.accumulatedMs + this.visibleStretch(now);

    this.completed.push({
      bookId: this.open.bookId,
      locator: this.open.locator,
      text: this.open.text,
      enterTs: this.open.enterTs,
      exitTs: now,
      dwellMs,
      counted: dwellMs >= this.skimThresholdMs,
    });
    this.open = null;
  }
}
