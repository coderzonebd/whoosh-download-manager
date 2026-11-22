// SPDX-License-Identifier: GPL-3.0-or-later
// Desktop entry. In the app, the shared UI talks to the wdm daemon: before
// every (re)connection it asks the shell for the daemon's address and token
// (`daemon_connection`, which starts the bundled daemon if none is running),
// so a daemon that restarts on another port, or was stopped, is found again.
//
// It also gets the shell's extras (`host.ts`: folder picker, desktop
// settings, launch at login) and `whoosh://add?url=…` links (`deeplinks.ts`).
//
// `?view=prompt` (the shell's prompt window, `prompt.rs`) mounts only the
// prompt: the browser downloads that wait for confirmation and the video pages
// the extension offered, one at a time; it closes itself when done.
//
// In a plain browser (the vite dev server) there is no shell, so the UI runs
// on the mock daemon. Dev-only query parameters for that case:
//   ?bulk=10000   add N synthetic downloads    ?lang=bn   start in Bangla
import "@fontsource-variable/baloo-da-2";
import "@fontsource-variable/inter";
import "@fontsource-variable/noto-sans-bengali";
import "@wdm/ui/styles.css";
import type { WdmClient } from "@wdm/api-client";
import { createMockClient } from "@wdm/api-client/mock";
import { connectWs } from "@wdm/api-client/ws";
import { App, PromptApp, type AppState } from "@wdm/ui";
import { mount } from "svelte";
import { startCrashReports } from "./crash.ts";
import { startDeepLinks } from "./deeplinks.ts";
import { desktopHost } from "./host.ts";
import { startUpdateNotices } from "./updates.ts";
import { startShellLanguage } from "./shell-language.svelte.ts";
import { startLoginNotice } from "./notify.ts";

declare const __APP_VERSION__: string;

interface Connection {
  url: string;
  token: string;
  daemonVersion: string;
}

function inTauri(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

function liveClient(): WdmClient {
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

function mockClient(): WdmClient {
  const q = new URLSearchParams(location.search);
  return createMockClient({
    latencyMs: 30,
    bulk: Number(q.get("bulk") ?? 0),
    settings: { language: q.get("lang") ?? (navigator.language.startsWith("bn") ? "bn" : "en") },
  });
}

const live = inTauri();
if (live) startCrashReports();
const client = live ? liveClient() : mockClient();

const target = document.getElementById("app");
if (!target) throw new Error("#app is missing from index.html");

/** The shell's prompt window (`prompt.rs`): the query, or the flag its init script sets. */
const promptView =
  new URLSearchParams(location.search).get("view") === "prompt" || (window as { __WDM_VIEW__?: string }).__WDM_VIEW__ === "prompt";

if (promptView) {
  mount(PromptApp, {
    target,
    props: { client, mode: live ? "live" : "mock", ...(live ? { host: desktopHost() } : {}) },
  });
} else {
  mount(App, {
    target,
    props: {
      client,
      mode: live ? "live" : "mock",
      // The folder picker, tray and login settings exist only in the app.
      ...(live ? { host: desktopHost() } : {}),
      onready: (app: AppState) => {
        // The tray tooltip and the system notifications come from the shell (`watch.rs`).
        if (live) {
          startLoginNotice(app);
          startShellLanguage(app);
          void startDeepLinks(app);
          void startUpdateNotices(app);
        }
      },
    },
  });
}
