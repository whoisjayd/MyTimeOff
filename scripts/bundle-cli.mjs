#!/usr/bin/env node
// Builds the `mytimeoff` command line and puts it where the installer will find it.
//
// The window and the command line cannot both sit in the install directory: Windows
// filenames are case-insensitive, so `MyTimeOff.exe` and `mytimeoff.exe` are one file,
// and whichever the installer writes second silently replaces the first. The command
// line therefore installs into a `bin` subdirectory - the same shape VS Code uses, and
// the reason `...\Microsoft VS Code\bin` shows up on so many PATHs. `bin` is what goes
// on PATH; the window stays at the top where people look for it.
//
// Tauri copies it there as a bundle resource, which unlike an externalBin sidecar does
// not demand the Rust target triple in the filename. It does still have to be on disk
// before the shell crate builds at all - tauri-build checks every declared resource - so
// run this once after a fresh clone or `cargo build -p mytimeoff-shell` will stop with
// "resource path binaries/mytimeoff.exe doesn't exist" (or the extension-less name on
// macOS/Linux).
//
// `tauri build` runs this itself through `beforeBuildCommand`, so there is normally no
// reason to call it by hand.

import { spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const configuration = process.argv.includes("--debug") ? "debug" : "release";
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const destinationDir = join(root, "crates", "shell", "binaries");
const exeSuffix = process.platform === "win32" ? ".exe" : "";

console.log(`building the mytimeoff command line (${configuration})`);
const args = ["build", "-p", "mytimeoff-daemon", "--bin", "mytimeoff"];
if (configuration === "release") args.push("--release");

const build = spawnSync("cargo", args, { stdio: "inherit", cwd: root });
if (build.status !== 0) {
  throw new Error(`cargo build failed with ${build.status}`);
}

const targetDir = process.env.CARGO_TARGET_DIR ?? join(root, "target");
const built = join(targetDir, configuration, `mytimeoff${exeSuffix}`);
if (!existsSync(built)) {
  throw new Error(`cargo said it succeeded but ${built} is not there`);
}

mkdirSync(destinationDir, { recursive: true });
const staged = join(destinationDir, `mytimeoff${exeSuffix}`);
copyFileSync(built, staged);
// fs.copyFile does not reliably preserve the exec bit across platforms.
if (process.platform !== "win32") {
  chmodSync(staged, 0o755);
}

console.log(`-> ${staged}`);
