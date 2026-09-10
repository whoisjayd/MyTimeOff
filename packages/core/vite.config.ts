import { defineConfig } from "vite";

// Core is DOM-free by design; running its tests in `node` keeps that honest.
export default defineConfig({
  test: {
    environment: "node",
  },
});
