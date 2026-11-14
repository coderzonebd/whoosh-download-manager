// SPDX-License-Identifier: Apache-2.0
// Builds the test-only harness page (e2e/harness.html) for the accessibility
// and end-to-end suites. Never part of a shipped build.
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";

const root = fileURLToPath(new URL("./e2e", import.meta.url));

export default defineConfig({
  root,
  base: "./",
  plugins: [svelte()],
  logLevel: "warn",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2022",
    rollupOptions: { input: { harness: `${root}/harness.html` } },
  },
  preview: { host: "127.0.0.1", port: 0, strictPort: false },
});
