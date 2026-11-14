// SPDX-License-Identifier: GPL-3.0-or-later
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vite";
import pkg from "./package.json" with { type: "json" };

// Tauri expects a fixed dev port and must not watch the Rust side.
export default defineConfig({
  plugins: [svelte()],
  // Sent to the daemon in `session.hello`.
  define: { __APP_VERSION__: JSON.stringify(pkg.version) },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    // WebKitGTK 2.4x+, WebView2 and WKWebView all handle ES2022.
    target: "es2022",
    sourcemap: Boolean(process.env.TAURI_ENV_DEBUG),
  },
});
