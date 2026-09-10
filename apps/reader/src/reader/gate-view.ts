import type { Answer, AskedQuestion, Gate, Refusal, Release, Verdict } from "@mytimeoff/core";

/**
 * The gate, drawn.
 *
 * It knows how to ask and how to show a ruling, and nothing about where either came from:
 * no fetch, no daemon, no rules of its own. What may be skipped, how many tries are left
 * and what counts as a pass all arrive with the gate, because the daemon is the only
 * place any of those are decided - and a surface that decided them again would eventually
 * decide them differently.
 */
export interface GateHandlers {
  /** The sheet as filled in. Questions left blank are simply absent from it. */
  onSubmit(quizId: string, answers: Answer[]): void;
  onSkip(): void;
}

const RELEASED: Record<Release, string> = {
  passed: "That was right. Back you go.",
  attempted: "Thanks for answering. Back you go.",
  exhausted: "Not quite - but the screen is yours. This nags; it does not jail.",
  skipped: "Skipped. Back you go.",
  ungated: "Nothing was read, so there is nothing to ask. Back you go.",
};

const REFUSED: Record<Refusal, string> = {
  not_attempted: "Answer at least one question first.",
  wrong_quiz: "That sheet was for a different quiz. Reopening this one.",
  not_skippable: "Not in strict mode. Answer what you can.",
};

export class GateView {
  #note: HTMLElement;
  #form: HTMLFormElement;
  #submit: HTMLButtonElement;
  #skip: HTMLButtonElement;
  #quizId: string | undefined;
  /** Asked in order, so the sheet can be collected without re-reading the DOM's shape. */
  #asked: AskedQuestion[] = [];

  constructor(document: Document, handlers: GateHandlers) {
    this.#note = document.querySelector<HTMLElement>("#quiz-note")!;
    this.#form = document.querySelector<HTMLFormElement>("#quiz-questions")!;
    this.#submit = document.querySelector<HTMLButtonElement>("#quiz-submit")!;
    this.#skip = document.querySelector<HTMLButtonElement>("#quiz-skip")!;

    this.#submit.addEventListener("click", () => {
      if (this.#quizId !== undefined) handlers.onSubmit(this.#quizId, this.#sheet());
    });
    this.#skip.addEventListener("click", () => handlers.onSkip());
  }

  /** Says the questions are on their way, so the gate is never a blank panel. */
  waiting(): void {
    this.#quizId = undefined;
    this.#asked = [];
    this.#form.replaceChildren();
    this.#note.textContent = "Working out what you just read…";
    this.#submit.hidden = true;
    this.#skip.hidden = true;
  }

  /** Draws a gate. A released one has nothing to ask and says so. */
  ask(gate: Gate): void {
    if (gate.gate === "released") {
      this.#quizId = undefined;
      this.#form.replaceChildren();
      this.#note.textContent = RELEASED[gate.reason];
      this.#submit.hidden = true;
      this.#skip.hidden = true;
      return;
    }

    this.#quizId = gate.quizId;
    this.#asked = gate.questions;
    this.#form.replaceChildren(...gate.questions.map((question) => this.#draw(question)));
    this.#note.textContent = this.#brief(gate.policy.passMark, gate.attemptsLeft);
    this.#submit.hidden = false;
    this.#submit.disabled = false;
    // The one thing the policy is for: a skip button that exists only where skipping does.
    this.#skip.hidden = !gate.policy.skippable;
  }

  /** Shows what came back. A release needs no button; the daemon takes the screen away. */
  verdict(verdict: Verdict): void {
    switch (verdict.outcome) {
      case "released":
        this.#note.textContent = RELEASED[verdict.reason];
        this.#submit.disabled = true;
        this.#skip.hidden = true;
        break;
      case "retry":
        this.#note.textContent =
          `${verdict.score.correct} of ${verdict.score.total} right. ` +
          `${tries(verdict.attemptsLeft)} left.`;
        break;
      case "refused":
        this.#note.textContent = REFUSED[verdict.reason];
        break;
    }
  }

  /** The daemon could not be asked. Say so rather than showing an empty quiz. */
  problem(detail: string): void {
    this.#note.textContent = `The gate could not be reached: ${detail}`;
    this.#submit.hidden = true;
  }

  #brief(passMark: { correct: number; of: number } | null, attemptsLeft: number): string {
    const bar = passMark
      ? `${passMark.correct} of every ${passMark.of} must be right.`
      : "Answering is enough; nothing has to be right.";
    return `${bar} ${tries(attemptsLeft)} left.`;
  }

  #draw(question: AskedQuestion): HTMLElement {
    const fieldset = this.#form.ownerDocument.createElement("fieldset");
    const legend = this.#form.ownerDocument.createElement("legend");
    legend.textContent = question.prompt;
    fieldset.append(legend);

    question.choices.forEach((choice, index) => {
      const label = this.#form.ownerDocument.createElement("label");
      const input = this.#form.ownerDocument.createElement("input");
      input.type = "radio";
      // Grouping by question id is what makes the choices mutually exclusive, and what
      // lets the sheet be read back below without any bookkeeping in between.
      input.name = question.id;
      input.value = String(index);
      label.append(input, this.#form.ownerDocument.createTextNode(` ${choice}`));
      fieldset.append(label);
    });
    return fieldset;
  }

  /** Reads the form back. Unanswered questions are left off, not sent as a guess. */
  #sheet(): Answer[] {
    return this.#asked.flatMap((question) => {
      const chosen = this.#form.querySelector<HTMLInputElement>(
        `input[name="${CSS.escape(question.id)}"]:checked`,
      );
      return chosen ? [{ questionId: question.id, choice: Number(chosen.value) }] : [];
    });
  }
}

function tries(count: number): string {
  return count === 1 ? "1 try" : `${count} tries`;
}
