#!/usr/bin/env node
// Prints every state change the daemon makes, so a live agent turn is observable.
//
// Run this in its own terminal while Claude Code drives the hooks. Polling is fine
// here: this is an observation tool, not part of the product. The daemon itself
// never polls - it sleeps to the grace deadline.

import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const port = Number(process.argv[2] ?? 8787);
// A fresh connection per poll is fine: this is an observation tool, and every
// transition worth seeing is seconds-scale, so a tighter interval would just pile up
// TIME_WAIT sockets for no benefit.
const intervalMs = 1000;

// Mirrors crates/daemon/src/paths.rs's own resolution exactly, so the two never
// diverge: %LOCALAPPDATA% first, then XDG_STATE_HOME, then HOME.
function stateDir() {
  const base = process.env.LOCALAPPDATA ?? process.env.XDG_STATE_HOME ?? process.env.HOME;
  if (!base) {
    throw new Error("no LOCALAPPDATA, XDG_STATE_HOME or HOME");
  }
  return join(base, "com.mytimeoff.desktop");
}

const tokenPath = join(stateDir(), "hook-token");
let token;
try {
  token = readFileSync(tokenPath, "utf8").trim();
} catch {
  throw new Error(`No token at ${tokenPath}. Start the daemon once to create it.`);
}

const uri = `http://127.0.0.1:${port}/state`;
console.log(`watching ${uri}  (Ctrl+C to stop)`);

let last = null;
let seen = 0;

function color(name, text) {
  const codes = {
    cyan: 36,
    green: 32,
    yellow: 33,
    magenta: 35,
    gray: 90,
    darkYellow: 33,
    darkGray: 90,
    white: 37,
  };
  const code = codes[name] ?? 37;
  return `\x1b[${code}m${text}\x1b[0m`;
}

function timestamp() {
  return new Date().toTimeString().slice(0, 8);
}

async function poll() {
  let report;
  try {
    const response = await fetch(uri, {
      headers: { Authorization: `Bearer ${token}` },
      signal: AbortSignal.timeout(5000),
    });
    report = await response.json();
  } catch {
    if (last !== "down") {
      console.log(color("darkYellow", `${timestamp()}  daemon unreachable`));
      last = "down";
    }
    return;
  }

  const sess = report.session ?? "-";
  const alert = report.alert ?? "-";
  const line = `${report.state.padEnd(8)} session=${String(sess).padEnd(40)} alert=${alert}`;

  // Deliveries first: a hook that never arrived and one that arrived and was correctly
  // ignored look identical from the state alone.
  if (report.received.length < seen) seen = 0; // daemon restarted
  if (report.received.length > seen) {
    for (const entry of report.received.slice(seen)) {
      const shade = entry.endsWith("-> ignored") ? "darkGray" : "white";
      console.log(color(shade, `          recv: ${entry}`));
    }
    seen = report.received.length;
  }

  if (line !== last) {
    const stateColor =
      { reading: "cyan", ready: report.alert === "needs_input" ? "yellow" : "green", gate: "magenta" }[
        report.state
      ] ?? "gray";
    console.log(color(stateColor, `${timestamp()}  ${line}`));
    if (report.issued.length > 0) {
      console.log(color("darkGray", `          issued: ${report.issued.join(" -> ")}`));
    }
    last = line;
  }
}

while (true) {
  await poll();
  await new Promise((resolve) => setTimeout(resolve, intervalMs));
}
