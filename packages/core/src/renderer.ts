import type { Locator } from "./types";

/** One rendered page of a book, with the text actually shown on it. */
export interface RenderedPage {
  locator: Locator;
  text: string;
}

/**
 * Format- and surface-agnostic view over a book.
 *
 * Note there is no container argument: a DOM renderer takes its element, and a
 * terminal renderer its output stream, at construction. Keeping that out of the
 * interface is what lets the same session logic drive a window or a terminal.
 */
export interface BookRenderer {
  /** Renders and displays the opening (or restored) position. */
  open(): Promise<void>;
  next(): Promise<void>;
  prev(): Promise<void>;
  /** Fires whenever the visible page changes, including the first display. */
  onPageChange(listener: (page: RenderedPage) => void): void;
  /** Total pages once known, for progress display. Undefined while indexing. */
  totalPages(): number | undefined;
  destroy(): void;
}

/**
 * Reports whether the reader is actually in front of the user.
 *
 * A browser implementation watches document visibility and focus; a terminal one
 * might watch focus events or simply always report visible. Dwell accounting is
 * only honest if the surface tells the truth here.
 */
export interface VisibilitySource {
  /** Subscribes to changes and reports the current value. Returns an unsubscribe. */
  subscribe(listener: (visible: boolean) => void): () => void;
}

/** A visibility source for surfaces that are always in view. */
export const ALWAYS_VISIBLE: VisibilitySource = {
  subscribe(listener) {
    listener(true);
    return () => {};
  },
};
