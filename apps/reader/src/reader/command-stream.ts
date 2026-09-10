import { parseCommand, type ReaderCommand } from "@mytimeoff/core";

/**
 * Reads the daemon's command stream.
 *
 * Deliberately `fetch` rather than `EventSource`: EventSource cannot set headers, so the
 * token would have to travel in the query string, where it ends up in logs and history.
 * In development the Vite proxy attaches the header and the page holds no secret at all;
 * a packaged build passes one here.
 */
export interface CommandStreamOptions {
  /** Same-origin path, so the browser never has to be told about the daemon's port. */
  url?: string;
  token?: string;
  onCommand(command: ReaderCommand): void;
  /** Connection state, for telling "nothing is happening" from "nothing is listening". */
  onConnection?(connected: boolean): void;
}

const RECONNECT_MIN_MS = 500;
const RECONNECT_MAX_MS = 10_000;

/**
 * Connects, and keeps reconnecting until the returned function is called.
 *
 * The daemon may be started after the reader, stopped for a rebuild, or restarted mid
 * session, and none of those should require the reader to be reloaded. Every reconnect
 * gets a fresh resync frame from the daemon, so a reader that was disconnected during a
 * takeover catches up rather than guessing.
 */
export function connectCommands(options: CommandStreamOptions): () => void {
  const { url = "/daemon/events", token, onCommand, onConnection } = options;
  const abort = new AbortController();
  let backoff = RECONNECT_MIN_MS;

  async function pump(): Promise<void> {
    const headers: HeadersInit = token ? { Authorization: `Bearer ${token}` } : {};
    const response = await fetch(url, { headers, signal: abort.signal });
    if (!response.ok || !response.body) {
      throw new Error(`command stream refused: ${response.status}`);
    }

    onConnection?.(true);
    backoff = RECONNECT_MIN_MS;

    const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
    // An event ends at a blank line, which can land anywhere in a chunk, so completed
    // events are taken off the front and the remainder waits for more bytes.
    let buffer = "";
    for (;;) {
      const { done, value } = await reader.read();
      if (done) return;
      buffer += value;

      let split = buffer.indexOf("\n\n");
      while (split !== -1) {
        emit(buffer.slice(0, split), onCommand);
        buffer = buffer.slice(split + 2);
        split = buffer.indexOf("\n\n");
      }
    }
  }

  async function run(): Promise<void> {
    while (!abort.signal.aborted) {
      try {
        await pump();
      } catch (error) {
        if (abort.signal.aborted) return;
        console.warn("[mytimeoff] command stream dropped", error);
      }
      onConnection?.(false);
      await new Promise((resolve) => setTimeout(resolve, backoff));
      backoff = Math.min(backoff * 2, RECONNECT_MAX_MS);
    }
  }

  void run();
  return () => abort.abort();
}

/** Turns one SSE frame into a command, ignoring keep-alive comments and unknown names. */
function emit(frame: string, onCommand: (command: ReaderCommand) => void): void {
  for (const line of frame.split("\n")) {
    if (!line.startsWith("data:")) continue;
    const command = parseCommand(line.slice("data:".length).trim());
    if (command) onCommand(command);
  }
}
