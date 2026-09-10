import { describe, expect, it } from "vitest";
import { DwellTracker } from "./dwell";
import type { Locator } from "./types";

const page = (n: number): Locator => ({ kind: "page", page: n, pageLabel: String(n) });

/** Drives the tracker with a controllable clock. */
function makeTracker(skimThresholdMs = 3000) {
  let now = 0;
  const tracker = new DwellTracker({ now: () => now, skimThresholdMs });
  return {
    tracker,
    advance(ms: number) {
      now += ms;
    },
  };
}

describe("DwellTracker", () => {
  it("records dwell for a page that is read and then left", () => {
    const { tracker, advance } = makeTracker();
    tracker.enterPage("b1", page(1), "text one");
    advance(5000);
    tracker.enterPage("b1", page(2), "text two");

    const views = tracker.views;
    expect(views).toHaveLength(1);
    expect(views[0]!.dwellMs).toBe(5000);
    expect(views[0]!.counted).toBe(true);
  });

  it("marks a page skimmed when dwell is under the threshold", () => {
    const { tracker, advance } = makeTracker(3000);
    tracker.enterPage("b1", page(1), "flipped past");
    advance(900);
    tracker.enterPage("b1", page(2), "also flipped");
    advance(800);
    tracker.finish();

    expect(tracker.views.map((v) => v.counted)).toEqual([false, false]);
  });

  it("excludes hidden time from dwell", () => {
    const { tracker, advance } = makeTracker();
    tracker.enterPage("b1", page(1), "text");
    advance(2000);
    tracker.setVisible(false);
    advance(60_000); // reader hidden - must not count
    tracker.setVisible(true);
    advance(2000);
    tracker.finish();

    expect(tracker.views[0]!.dwellMs).toBe(4000);
  });

  it("does not accrue dwell for a page entered while hidden", () => {
    const { tracker, advance } = makeTracker();
    tracker.setVisible(false);
    tracker.enterPage("b1", page(1), "text");
    advance(10_000);
    tracker.finish();

    expect(tracker.views[0]!.dwellMs).toBe(0);
    expect(tracker.views[0]!.counted).toBe(false);
  });

  it("accumulates dwell across revisits to the same page as separate views", () => {
    const { tracker, advance } = makeTracker();
    tracker.enterPage("b1", page(1), "a");
    advance(4000);
    tracker.enterPage("b1", page(2), "b");
    advance(4000);
    tracker.enterPage("b1", page(1), "a");
    advance(4000);
    tracker.finish();

    expect(tracker.views).toHaveLength(3);
    expect(tracker.countedPages()).toEqual(["1", "2"]);
  });

  it("reports the quiz span as counted pages only, in reading order", () => {
    const { tracker, advance } = makeTracker(3000);
    tracker.enterPage("b1", page(1), "read this");
    advance(9000);
    tracker.enterPage("b1", page(2), "flip");
    advance(500);
    tracker.enterPage("b1", page(3), "read this too");
    advance(9000);
    tracker.finish();

    expect(tracker.quizSpan().map((v) => v.locator.pageLabel)).toEqual(["1", "3"]);
  });

  it("ignores repeated visibility changes of the same value", () => {
    const { tracker, advance } = makeTracker();
    tracker.enterPage("b1", page(1), "text");
    advance(1000);
    tracker.setVisible(true);
    advance(1000);
    tracker.setVisible(false);
    tracker.setVisible(false);
    advance(5000);
    tracker.finish();

    expect(tracker.views[0]!.dwellMs).toBe(2000);
  });

  it("is idempotent on finish", () => {
    const { tracker, advance } = makeTracker();
    tracker.enterPage("b1", page(1), "text");
    advance(4000);
    tracker.finish();
    tracker.finish();

    expect(tracker.views).toHaveLength(1);
    expect(tracker.views[0]!.dwellMs).toBe(4000);
  });
});
