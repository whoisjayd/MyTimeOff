import { describe, expect, it } from "vitest";

import { AT_REST, applyCommand, applyCommands, parseCommand } from "./commands";

describe("parseCommand", () => {
  it("accepts the commands the daemon issues", () => {
    expect(parseCommand("show_reader")).toBe("show_reader");
    expect(parseCommand("indicator_needs_input")).toBe("indicator_needs_input");
  });

  it("ignores a command it does not recognise", () => {
    // A newer daemon must not be able to brick an older surface.
    expect(parseCommand("start_karaoke")).toBeUndefined();
    expect(parseCommand("")).toBeUndefined();
  });
});

describe("applyCommand", () => {
  it("takes and releases the screen", () => {
    const reading = applyCommand(AT_REST, "show_reader");
    expect(reading.reading).toBe(true);
    expect(applyCommand(reading, "hide_reader")).toEqual(AT_REST);
  });

  it("raises and clears the indicator without disturbing the reader", () => {
    const done = applyCommands(AT_REST, ["show_reader", "indicator_done"]);
    expect(done).toEqual({ reading: true, indicator: "done", quiz: false });

    const cleared = applyCommand(done, "clear_indicator");
    expect(cleared).toEqual({ reading: true, indicator: null, quiz: false });
  });

  it("replaces one indicator with the other", () => {
    const state = applyCommands(AT_REST, [
      "show_reader",
      "indicator_done",
      "indicator_needs_input",
    ]);
    expect(state.indicator).toBe("needs_input");
  });

  it("drops a stale indicator when the reader is dismissed", () => {
    // Otherwise it would still be up at the start of the next takeover, answering a
    // turn that ended long ago.
    const state = applyCommands(AT_REST, ["show_reader", "indicator_done", "hide_reader"]);
    expect(state.indicator).toBeNull();
  });

  it("keeps the screen for a quiz even if show_reader never arrived", () => {
    expect(applyCommand(AT_REST, "start_quiz")).toEqual({
      reading: true,
      indicator: null,
      quiz: true,
    });
  });

  it("leaves the original state untouched", () => {
    const before = { ...AT_REST };
    applyCommand(AT_REST, "show_reader");
    expect(AT_REST).toEqual(before);
  });
});
