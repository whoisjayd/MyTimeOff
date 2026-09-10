import type { VisibilitySource } from "@mytimeoff/core";

/**
 * Browser visibility: the page is "visible" only when the document is unhidden *and*
 * the window has focus.
 *
 * Document visibility alone is not enough. A reader window left open behind the
 * terminal stays `visible` to the Page Visibility API, so without the focus check the
 * whole agent turn would be logged as reading time.
 */
export class DomVisibility implements VisibilitySource {
  private readonly target: Window;

  constructor(target: Window = window) {
    this.target = target;
  }

  subscribe(listener: (visible: boolean) => void): () => void {
    const doc = this.target.document;
    const report = (): void => {
      listener(doc.visibilityState === "visible" && doc.hasFocus());
    };

    doc.addEventListener("visibilitychange", report);
    this.target.addEventListener("focus", report);
    this.target.addEventListener("blur", report);
    report();

    return () => {
      doc.removeEventListener("visibilitychange", report);
      this.target.removeEventListener("focus", report);
      this.target.removeEventListener("blur", report);
    };
  }
}
