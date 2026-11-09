// SPDX-License-Identifier: GPL-3.0-or-later
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vite";

// Tauri expects a fixed dev port and must not watch the Rust side.
export default defineConfig({
  plugins: [svelte()],
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
