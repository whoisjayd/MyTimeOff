import type { AgentKey, AgentWiring } from "@mytimeoff/core";

/**
 * The panel that wires a coding agent to MyTimeOff, drawn.
 *
 * This is the half of the tool that used to need a terminal. Everything else here is
 * automatic - the reader takes the screen by itself and gives it back by itself - and
 * then the very first step was `mytimeoff hooks install`, typed into a shell, by someone
 * who had just double-clicked an installer. This is that step, as a button.
 *
 * Like [`GateView`](./gate-view.ts) it knows how to draw and nothing about where the
 * answer comes from: no fetch, no daemon, no opinion about what "connected" means. The
 * daemon reads the settings files and decides; this puts the result on screen.
 */
export interface AgentsHandlers {
  onConnect(key: AgentKey): void;
  onDisconnect(key: AgentKey): void;
  onClose(): void;
}

/** A line pinned under one agent's row after something happened to it. */
interface Note {
  text: string;
  kind: "said" | "problem";
}

export class AgentsView {
  #document: Document;
  #panel: HTMLElement;
  #list: HTMLElement;
  #note: HTMLElement;
  #opener: HTMLButtonElement;
  #handlers: AgentsHandlers;

  #wirings: AgentWiring[] = [];
  /** What was last said about each agent, so a redraw does not wipe it off the screen. */
  #notes = new Map<AgentKey, Note>();
  /** The agent currently being written, if any. Its buttons are dead while it is. */
  #working: AgentKey | undefined;

  constructor(document: Document, handlers: AgentsHandlers) {
    this.#document = document;
    this.#panel = document.querySelector<HTMLElement>("#agents")!;
    this.#list = document.querySelector<HTMLElement>("#agents-list")!;
    this.#note = document.querySelector<HTMLElement>("#agents-note")!;
    this.#opener = document.querySelector<HTMLButtonElement>("#agents-open")!;
    this.#handlers = handlers;

    this.#opener.addEventListener("click", () => this.open());
    document.querySelector("#agents-close")?.addEventListener("click", () => {
      handlers.onClose();
      this.close();
    });
  }

  get isOpen(): boolean {
    return !this.#panel.hidden;
  }

  open(): void {
    this.#panel.hidden = false;
  }

  close(): void {
    this.#panel.hidden = true;
  }

  /** Says the settings are being read, so the panel is never a blank card. */
  waiting(): void {
    this.#note.textContent = "Reading your agent settings…";
    this.#list.replaceChildren();
  }

  /** Draws what the daemon found, and keeps the opener's badge in step with it. */
  show(wirings: AgentWiring[]): void {
    this.#wirings = wirings;
    this.#working = undefined;
    this.#note.textContent =
      "MyTimeOff needs to hear when a turn starts and when it ends. Connecting adds a " +
      "few hooks to the agent's own settings file and leaves everything else in it alone.";
    this.#list.replaceChildren(...wirings.map((one) => this.#draw(one)));
    this.#badge(wirings);
  }

  /** One agent is being written. Its buttons go dead until the answer comes back. */
  working(key: AgentKey, what: string): void {
    this.#working = key;
    this.#notes.set(key, { text: what, kind: "said" });
    this.#redraw();
  }

  /** Replaces one row with what it looks like now, and pins a line under it. */
  changed(wiring: AgentWiring, said: string): void {
    this.#wirings = this.#wirings.map((one) => (one.key === wiring.key ? wiring : one));
    this.#working = undefined;
    this.#notes.set(wiring.key, { text: said, kind: "said" });
    this.#redraw();
    this.#badge(this.#wirings);
  }

  /** Something went wrong with one agent. The others are still usable, so only it changes. */
  problem(key: AgentKey, detail: string): void {
    this.#working = undefined;
    this.#notes.set(key, { text: detail, kind: "problem" });
    this.#redraw();
  }

  /** The daemon could not be asked at all. Nothing below is worth drawing. */
  unreachable(detail: string): void {
    this.#note.textContent = `Your agent settings could not be read: ${detail}`;
    this.#list.replaceChildren();
  }

  #redraw(): void {
    this.#list.replaceChildren(...this.#wirings.map((one) => this.#draw(one)));
  }

  /**
   * Marks the opener when something wants attention.
   *
   * "Nothing connected" and "connected but pointing at the wrong place" are the two ways
   * for this tool to sit there doing nothing while looking fine, so they are the two
   * cases worth a mark on a button somebody can see without opening anything.
   */
  #badge(wirings: AgentWiring[]): void {
    const connected = wirings.some((one) => one.complete);
    this.#opener.dataset["state"] = connected ? "connected" : "loose";
    this.#opener.textContent = connected ? "Agents" : "Connect your agent";
  }

  #draw(wiring: AgentWiring): HTMLElement {
    const row = this.#element("section", "agent");
    row.dataset["state"] = state(wiring);

    const head = this.#element("header");
    head.append(this.#element("h3", undefined, wiring.label));
    head.append(this.#element("span", "agent-state", headline(wiring)));
    // The one thing this panel must not be quiet about. A button that looks exactly like
    // the tested one, next to a note in the release text nobody read, is how somebody
    // ends up assuming their evening of reading was being tracked.
    if (!wiring.proven) head.append(this.#element("span", "agent-untested", "untested"));
    row.append(head);

    row.append(this.#element("p", "agent-path", wiring.path));

    if (wiring.hooks.length > 0) {
      const list = this.#element("ul", "agent-hooks");
      for (const event of wiring.events) {
        const found = wiring.hooks.find((hook) => hook.event === event);
        const item = this.#element("li");
        item.append(this.#element("span", "agent-event", event));
        item.append(
          this.#element(
            "span",
            found?.current ? "agent-target" : "agent-target agent-stale",
            found ? found.target : "not wired",
          ),
        );
        list.append(item);
      }
      row.append(list);
    }

    if (!wiring.proven) {
      row.append(
        this.#element(
          "p",
          "agent-caution",
          `${wiring.label} support is written from its published hook format and has not ` +
            `been tested against a real install. It should work; nobody has watched it. ` +
            `If the book never appears, this is the first thing to suspect.`,
        ),
      );
    }

    if (wiring.trouble) row.append(this.#element("p", "agent-trouble", wiring.trouble));

    const note = this.#notes.get(wiring.key);
    if (note) row.append(this.#element("p", `agent-${note.kind}`, note.text));

    row.append(this.#buttons(wiring));
    return row;
  }

  #buttons(wiring: AgentWiring): HTMLElement {
    const actions = this.#element("footer", "agent-actions");
    const busy = this.#working !== undefined;

    const connect = this.#button(wiring.complete ? "Reconnect" : "Connect");
    connect.disabled = busy;
    connect.addEventListener("click", () => this.#handlers.onConnect(wiring.key));
    actions.append(connect);

    // Offered only where there is something to take out. A disconnect button on an agent
    // that was never wired is a button whose only possible outcome is "nothing happened".
    if (wiring.hooks.length > 0) {
      const remove = this.#button("Disconnect", "agent-remove");
      remove.disabled = busy;
      remove.addEventListener("click", () => this.#handlers.onDisconnect(wiring.key));
      actions.append(remove);
    }
    return actions;
  }

  #element(tag: string, className?: string, text?: string): HTMLElement {
    const node = this.#document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  #button(label: string, className?: string): HTMLButtonElement {
    const button = this.#document.createElement("button");
    button.type = "button";
    button.textContent = label;
    if (className) button.className = className;
    return button;
  }
}

/** Three states, because "wired" and "wired correctly" are not the same thing. */
function state(wiring: AgentWiring): string {
  if (wiring.complete) return "connected";
  return wiring.hooks.length > 0 ? "stale" : "loose";
}

function headline(wiring: AgentWiring): string {
  if (wiring.complete) return "connected";
  if (wiring.hooks.length > 0) return "connected, but not correctly";
  // "Not installed" is a guess from a directory that is not there, so it is worded as
  // one: somebody who moved their config elsewhere should not be told they have no Codex.
  return wiring.present ? "not connected" : "not connected (and not found on this machine)";
}
