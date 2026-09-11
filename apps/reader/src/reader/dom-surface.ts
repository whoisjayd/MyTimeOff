import type { SurfaceState } from "@mytimeoff/core";

const INDICATOR_TEXT = {
  done: "The agent has finished.",
  needs_input: "The agent needs you.",
} as const;

/**
 * The same news, said once and loudly.
 *
 * It carries the way out rather than repeating the sentence above it, because the two are
 * read together: the indicator says what is true, this says it just became true and what
 * to do about it.
 */
const TOAST_TEXT = {
  done: "The agent has finished — press Escape to go back.",
  needs_input: "The agent needs you — press Escape to go back.",
} as const;

/**
 * Long enough to catch someone mid-paragraph, short enough not to sit on the page as a
 * second permanent indicator. The indicator itself stays up either way, so nothing is
 * lost when this goes.
 */
const TOAST_MS = 6_000;

type Kind = keyof typeof INDICATOR_TEXT;

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
  #toast: HTMLElement;
  #fade: ReturnType<typeof setTimeout> | undefined;

  constructor(document: Document) {
    this.#root = document.body;
    this.#indicator = document.querySelector<HTMLElement>("#indicator")!;
    this.#indicatorText = document.querySelector<HTMLElement>("#indicator-text")!;
    this.#quiz = document.querySelector<HTMLElement>("#quiz")!;
    this.#link = document.querySelector<HTMLElement>("#link")!;
    this.#toast = document.querySelector<HTMLElement>("#toast")!;
  }

  render(state: SurfaceState): void {
    this.#root.dataset.reading = String(state.reading);
    this.#indicator.hidden = state.indicator === null;
    if (state.indicator !== null) {
      this.#indicatorText.textContent = INDICATOR_TEXT[state.indicator];
      this.#indicator.dataset.kind = state.indicator;
    } else {
      // A toast that outlived the thing it announced would be saying something untrue.
      this.#dismiss();
    }
    this.#quiz.hidden = !state.quiz;
  }

  /**
   * Announces that the indicator has *just* appeared, which the state itself cannot say.
   *
   * Raised by the caller on the transition rather than worked out here, for the same
   * reason the gate is: reconnecting replays every command the daemon has issued, and a
   * surface that toasted on each render would announce a turn that ended an hour ago.
   */
  alert(kind: Kind): void {
    this.#toast.textContent = TOAST_TEXT[kind];
    this.#toast.dataset.kind = kind;
    // Hiding first replays the animation when one kind replaces the other; without it the
    // second toast would simply appear already in place, which is the thing being missed.
    this.#toast.hidden = true;
    void this.#toast.offsetWidth;
    this.#toast.hidden = false;
    clearTimeout(this.#fade);
    this.#fade = setTimeout(() => this.#dismiss(), TOAST_MS);
  }

  #dismiss(): void {
    clearTimeout(this.#fade);
    this.#fade = undefined;
    this.#toast.hidden = true;
  }

  /** Distinguishes "the agent is quiet" from "the daemon is not running". */
  connection(connected: boolean): void {
    this.#link.dataset.connected = String(connected);
    this.#link.title = connected ? "daemon connected" : "daemon unreachable";
  }
}
