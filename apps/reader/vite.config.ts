import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { fileURLToPath, URL } from "node:url";
import { defineConfig } from "vite";

/** Where the daemon keeps its token. Mirrors `token::default_path` in the Rust crate. */
function daemonToken(): string | undefined {
  const base = process.env["LOCALAPPDATA"] ?? process.env["XDG_STATE_HOME"] ?? homedir();
  try {
    return readFileSync(join(base, "MyTimeOff", "hook-token"), "utf8").trim();
  } catch {
    // The daemon has not run yet, so there is no token to forward. Requests will come
    // back 401 until it has, which is the honest answer.
    return undefined;
  }
}

export default defineConfig({
  // Dev-only bridge to the daemon. Proxying instead of calling 127.0.0.1:8787 from the
  // page keeps everything same-origin, so there is no CORS to open up - and it lets the
  // proxy attach the token, so the browser never holds the secret at all.
  server: {
    proxy: {
      "/daemon": {
        target: "http://127.0.0.1:8787",
        rewrite: (path) => path.replace(/^\/daemon/, ""),
        configure(proxy) {
          // Read per request rather than at startup: the daemon may be started after
          // the dev server, and the token file only exists once it has.
          proxy.on("proxyReq", (proxyReq) => {
            const token = daemonToken();
            if (token) proxyReq.setHeader("authorization", `Bearer ${token}`);
          });
        },
      },
    },
  },
  resolve: {
    alias: {
      "@mytimeoff/core": fileURLToPath(
        new URL("../../packages/core/src/index.ts", import.meta.url),
      ),
    },
  },
  test: {
    environment: "node",
  },
});
