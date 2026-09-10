import { describe, expect, it } from "vitest";
import { DwellTracker } from "./dwell";
import type { Locator, PageView } from "./types";

const page = (n: number): Locator => ({ kind: "page", page: n, pageLabel: String(n) });

/** Drives the tracker with a controllable clock. */
function makeTracker() {
  let now = 0;
  const reported: PageView[] = [];
  const tracker = new DwellTracker({ now: () => now, onView: (v) => reported.push(v) });
  return {
    tracker,
    reported,
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
  });

  it("treats revisiting a page as a separate view", () => {
    const { tracker, advance } = makeTracker();
    tracker.enterPage("b1", page(1), "a");
    advance(4000);
    tracker.enterPage("b1", page(2), "b");
    advance(4000);
    tracker.enterPage("b1", page(1), "a");
    advance(4000);
    tracker.finish();

    // Three visits, not two pages. Whether that is one page or two of progress is the
    // daemon's question to answer, and it has the whole day's history to answer it with.
    expect(tracker.views.map((v) => v.locator.pageLabel)).toEqual(["1", "2", "1"]);
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

  it("reports each view as it closes, not at the end", () => {
    // A window killed mid-session must cost at most the page you were on.
    const { tracker, advance, reported } = makeTracker();
    tracker.enterPage("b1", page(1), "a");
    advance(4000);
    tracker.enterPage("b1", page(2), "b");

    expect(reported.map((v) => v.locator.pageLabel)).toEqual(["1"]);
    advance(4000);
    tracker.finish();
    expect(reported.map((v) => v.locator.pageLabel)).toEqual(["1", "2"]);
  });

  it("reports a view only once, however often finish is called", () => {
    const { tracker, advance, reported } = makeTracker();
    tracker.enterPage("b1", page(1), "a");
    advance(4000);
    tracker.finish();
    tracker.finish();

    expect(reported).toHaveLength(1);
  });
});
