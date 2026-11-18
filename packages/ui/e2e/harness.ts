// SPDX-License-Identifier: Apache-2.0
// Test-only page for the accessibility and end-to-end suites. It is built by
// vite.e2e.config.ts into e2e/dist and served only while those suites run:
// no app, extension or package export includes it.
//
//   ?client=mock (default)  the mock daemon. ?lang=bn  ?theme=light|dark
//   ?data=worst|empty|one|bulk  the mock's downloads: the realistic worst case
//                           for every value shown, none, exactly one, or 1 000
//                           more rows (default: the demo set). With ?data or
//                           ?toggle=on a Demo / Worst case switch at the bottom
//                           flips it (test page only).
//   ?view=mobile            the Android app's phone layout (MobileApp)
//   ?view=prompt            the desktop app's prompt window (PromptApp), which
//                           stays open when nothing is left (no shell to close it).
//   ?client=live            a real wdm-daemon. The suite injects the
//                           address and token as window.__WDM_E2E__ before
//                           the page loads (never in the URL).
//
// The AppState is exposed as window.__wdmApp so the suites can open
// screens directly.
import type { WdmClient } from "@wdm/api-client";
import { createMockClient } from "@wdm/api-client/mock";
import { connectWs } from "@wdm/api-client/ws";
import { mount } from "svelte";
import { App, MobileApp, PromptApp, type AppState, type Host } from "../src/index.ts";
import "../src/styles/global.css";

declare global {
  interface Window {
    __WDM_E2E__?: { url: string; token: string };
    __wdmApp?: AppState;
  }
}

const q = new URLSearchParams(location.search);
const live = q.get("client") === "live";

function client(): WdmClient {
  if (live) {
    const conn = window.__WDM_E2E__;
    if (!conn) throw new Error("window.__WDM_E2E__ is missing");
    return connectWs({ url: conn.url, token: conn.token, client: { name: "e2e", version: "0.0.0" } });
  }
  const theme = q.get("theme");
  const data = q.get("data");
  return createMockClient({
    autoTick: false,
    scenario: data === "worst" || data === "empty" || data === "one" ? data : "demo",
    ...(data === "bulk" ? { bulk: 1000 } : {}),
    settings: {
      language: q.get("lang") ?? "en",
      ...(theme === "light" || theme === "dark" ? { theme } : {}),
    },
  });
}

// On the mock, a host with the desktop-only settings, so the accessibility
// suite sees them too.
// The crash-report notice shows as on a first run.
const crash = { enabled: true, noticeShown: false };
const mockHost: Host = {
  desktopPrefs: {
    get: async () => ({ closeToTray: true, stopDaemonOnQuit: false, autoUpdateCheck: false }),
    set: async (p) => p,
  },
  launchAtLogin: { get: async () => false, set: async () => {} },
  // One browser in each state the "Connect your browser" page can show.
  browsers: {
    status: async () => [
      { browser: "chrome", inUse: true, hostRegistered: true, extensionFound: false, store: "github" },
      { browser: "edge", inUse: true, hostRegistered: false, extensionFound: true, store: "edgeAddOns" },
      { browser: "brave", inUse: true, hostRegistered: true, extensionFound: true, store: "github" },
      { browser: "firefox", inUse: true, hostRegistered: false, extensionFound: false, store: "amo" },
      { browser: "vivaldi", inUse: false, hostRegistered: false, extensionFound: false, store: "github" },
      { browser: "chromium", inUse: false, hostRegistered: false, extensionFound: false, store: "github" },
    ],
    connect: async () => {},
    openStore: async () => {},
    firstRun: async () => false,
  },
  updates: {
    check: async () => ({ version: "1.0.0-beta.2", currentVersion: "1.0.0-beta.1", notes: "Fixes and a faster start." }),
    install: async () => {},
  },
  openDownload: async () => {},
  setFollowed: async () => {},
  crashReports: {
    get: async () => ({ ...crash }),
    set: async (on) => void Object.assign(crash, { enabled: on, noticeShown: true }),
    dismissNotice: async () => void (crash.noticeShown = true),
  },
};

const target = document.getElementById("app");
if (!target) throw new Error("#app missing");
const props = {
  client: client(),
  mode: live ? ("live" as const) : ("mock" as const),
  storage: null,
  ...(live ? {} : { host: mockHost }),
  onready: (app: AppState) => {
    window.__wdmApp = app;
  },
};
if (q.get("view") === "prompt") mount(PromptApp, { target, props });
// The phone app's host: what apps/mobile passes (no desktop-only settings).
else if (q.get("view") === "mobile") mount(MobileApp, { target, props: { ...props, host: { platform: "android" as const, readClipboard: async () => "" } } });
else mount(App, { target, props });

// The data switch: plain chrome, not part of the design under test.
// Only when asked for (?data=… or ?toggle=on): the suites check the page as the app is, without it.
if (!live && q.get("toggle") !== "off" && (q.has("data") || q.get("toggle") === "on")) {
  const states = [
    ["demo", "Demo data"],
    ["worst", "Worst case"],
    ["empty", "Empty"],
    ["one", "One"],
    ["bulk", "1,000 rows"],
  ] as const;
  const current = q.get("data") ?? "demo";
  const bar = document.createElement("div");
  bar.setAttribute("role", "group");
  bar.setAttribute("aria-label", "Test data");
  bar.style.cssText =
    "position:fixed;inset-block-end:12px;inset-inline-start:50%;translate:-50% 0;z-index:2147483647;display:flex;gap:2px;padding:3px;border-radius:999px;background:#d4d4d8;font:12px system-ui,sans-serif;box-shadow:0 1px 3px rgb(0 0 0/.25)";
  for (const [value, label] of states) {
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = label;
    b.setAttribute("aria-pressed", String(value === current));
    b.style.cssText = `border:0;border-radius:999px;padding:4px 10px;cursor:pointer;color:#18181b;background:${value === current ? "#fff" : "transparent"}`;
    b.addEventListener("click", () => {
      const next = new URLSearchParams(location.search);
      if (value === "demo") next.delete("data");
      else next.set("data", value);
      location.search = next.toString();
    });
    bar.append(b);
  }
  document.body.append(bar);
}
