import type { SurfaceState } from "@mytimeoff/core";

const INDICATOR_TEXT = {
  done: "The agent has finished.",
  needs_input: "The agent needs you.",
} as const;

/**
 * Paints the surface state onto the page.
 *
 * The state is computed elsewhere and this only renders it, so what the reader shows can
 * be tested without a browser and a second surface can be added without a second copy of
 * the rules.
 *
 * One thing this surface genuinely cannot do: raise itself to the front. A web page has
 * no such power, so `reading` shows here as a banner rather than a takeover. That is a
 * Tauri window's job, and the only part of this file that will change to get it.
 */
export class DomSurface {
  #root: HTMLElement;
  #indicator: HTMLElement;
  #indicatorText: HTMLElement;
  #quiz: HTMLElement;
  #link: HTMLElement;

  constructor(document: Document) {
    this.#root = document.body;
    this.#indicator = document.querySelector<HTMLElement>("#indicator")!;
    this.#indicatorText = document.querySelector<HTMLElement>("#indicator-text")!;
    this.#quiz = document.querySelector<HTMLElement>("#quiz")!;
    this.#link = document.querySelector<HTMLElement>("#link")!;
  }

  render(state: SurfaceState): void {
    this.#root.dataset.reading = String(state.reading);
    this.#indicator.hidden = state.indicator === null;
    if (state.indicator !== null) {
      this.#indicatorText.textContent = INDICATOR_TEXT[state.indicator];
      this.#indicator.dataset.kind = state.indicator;
    }
    this.#quiz.hidden = !state.quiz;
  }

  /** Distinguishes "the agent is quiet" from "the daemon is not running". */
  connection(connected: boolean): void {
    this.#link.dataset.connected = String(connected);
    this.#link.title = connected ? "daemon connected" : "daemon unreachable";
  }
}
