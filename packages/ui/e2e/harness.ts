// SPDX-License-Identifier: Apache-2.0
// Test-only page for the accessibility and end-to-end suites. It is built by
// vite.e2e.config.ts into e2e/dist and served only while those suites run:
// no app, extension or package export includes it.
//
//   ?client=mock (default)  the mock daemon. ?lang=bn  ?theme=light|dark
//   ?client=live            a real whoosh-daemon. The suite injects the
//                           address and token as window.__WHOOSH_E2E__ before
//                           the page loads (never in the URL).
//
// The AppState is exposed as window.__whooshApp so the suites can open
// screens directly.
import type { WhooshClient } from "@whoosh/api-client";
import { createMockClient } from "@whoosh/api-client/mock";
import { connectWs } from "@whoosh/api-client/ws";
import { mount } from "svelte";
import { App, type AppState, type Host } from "../src/index.ts";
import "../src/styles/global.css";

declare global {
  interface Window {
    __WHOOSH_E2E__?: { url: string; token: string };
    __whooshApp?: AppState;
  }
}

const q = new URLSearchParams(location.search);
const live = q.get("client") === "live";

function client(): WhooshClient {
  if (live) {
    const conn = window.__WHOOSH_E2E__;
    if (!conn) throw new Error("window.__WHOOSH_E2E__ is missing");
    return connectWs({ url: conn.url, token: conn.token, client: { name: "e2e", version: "0.0.0" } });
  }
  const theme = q.get("theme");
  return createMockClient({
    autoTick: false,
    settings: {
      language: q.get("lang") ?? "en",
      ...(theme === "light" || theme === "dark" ? { theme } : {}),
    },
  });
}

// On the mock, a host with the desktop-only settings, so the accessibility
// suite sees them too.
let crashOn = false;
const mockHost: Host = {
  desktopPrefs: {
    get: async () => ({ closeToTray: true, stopDaemonOnQuit: false }),
    set: async (p) => p,
  },
  launchAtLogin: { get: async () => false, set: async () => {} },
  crashReports: {
    get: async () => crashOn,
    set: async (on) => void (crashOn = on),
  },
};

const target = document.getElementById("app");
if (!target) throw new Error("#app missing");
mount(App, {
  target,
  props: {
    client: client(),
    mode: live ? "live" : "mock",
    storage: null,
    ...(live ? {} : { host: mockHost }),
    onready: (app: AppState) => {
      window.__whooshApp = app;
    },
  },
});
