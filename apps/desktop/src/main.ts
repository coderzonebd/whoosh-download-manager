// SPDX-License-Identifier: GPL-3.0-or-later
// Desktop entry. In the app, the shared UI talks to the whoosh daemon: before
// every (re)connection it asks the shell for the daemon's address and token
// (`daemon_connection`, which starts the bundled daemon if none is running),
// so a daemon that restarts on another port, or was stopped, is found again.
//
// It also gets the shell's extras (`host.ts`: folder picker, desktop
// settings, launch at login) and `whoosh://add?url=…` links (`deeplinks.ts`).
//
// In a plain browser (the vite dev server) there is no shell, so the UI runs
// on the mock daemon. Dev-only query parameters for that case:
//   ?bulk=10000   add N synthetic downloads    ?lang=bn   start in Bangla
import "@fontsource-variable/baloo-da-2";
import "@fontsource-variable/inter";
import "@fontsource-variable/noto-sans-bengali";
import "@whoosh/ui/styles.css";
import type { WhooshClient } from "@whoosh/api-client";
import { createMockClient } from "@whoosh/api-client/mock";
import { connectWs } from "@whoosh/api-client/ws";
import { App, type AppState } from "@whoosh/ui";
import { mount } from "svelte";
import { startDeepLinks } from "./deeplinks.ts";
import { desktopHost } from "./host.ts";
import { startUpdateNotices } from "./updates.ts";
import { startShellLanguage } from "./shell-language.svelte.ts";
import { startNotifications } from "./notify.ts";
import { startTrayUpdates } from "./tray.ts";

declare const __APP_VERSION__: string;

interface Connection {
  url: string;
  token: string;
  daemonVersion: string;
}

function inTauri(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

function liveClient(): WhooshClient {
  // One lookup per attempt: `url` asks the shell, `token` reuses its answer
  // (the client resolves them in that order).
  let last: Connection | undefined;
  const lookup = async (): Promise<Connection> => {
    const { invoke } = await import("@tauri-apps/api/core");
    last = await invoke<Connection>("daemon_connection");
    return last;
  };
  return connectWs({
    url: async () => (await lookup()).url,
    token: async () => (last ?? (await lookup())).token,
    client: { name: "desktop", version: __APP_VERSION__ },
  });
}

function mockClient(): WhooshClient {
  const q = new URLSearchParams(location.search);
  return createMockClient({
    latencyMs: 30,
    bulk: Number(q.get("bulk") ?? 0),
    settings: { language: q.get("lang") ?? (navigator.language.startsWith("bn") ? "bn" : "en") },
  });
}

const live = inTauri();
const client = live ? liveClient() : mockClient();

const target = document.getElementById("app");
if (!target) throw new Error("#app is missing from index.html");

mount(App, {
  target,
  props: {
    client,
    mode: live ? "live" : "mock",
    // The folder picker, tray and login settings exist only in the app.
    ...(live ? { host: desktopHost() } : {}),
    onready: (app: AppState) => {
      startTrayUpdates(client, (bps, active) => app.i18n.t("tray.tooltip", { speed: app.i18n.speed(bps), active: app.i18n.t("status.active", { count: active }) }));
      if (live) {
        startNotifications(client, app);
        startShellLanguage(app);
        void startDeepLinks(app);
        void startUpdateNotices(app);
      }
    },
  },
});
