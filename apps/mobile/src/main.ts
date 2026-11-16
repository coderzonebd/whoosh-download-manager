// SPDX-License-Identifier: GPL-3.0-or-later
// Android entry. The daemon runs inside the app's own process (src-tauri/src/lib.rs);
// before every (re)connection the UI asks the shell for its address and token
// (`daemon_connection`), exactly as the desktop app does with its separate daemon.
//
// In a plain browser (the vite dev server) there is no shell, so the UI runs on
// the mock daemon.
import "@fontsource-variable/baloo-da-2";
import "@fontsource-variable/inter";
import "@fontsource-variable/noto-sans-bengali";
import "@wdm/ui/styles.css";
import type { WdmClient } from "@wdm/api-client";
import { createMockClient } from "@wdm/api-client/mock";
import { connectWs } from "@wdm/api-client/ws";
import { App } from "@wdm/ui";
import { mount } from "svelte";
import { mobileHost } from "./host.ts";

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
  let last: Connection | undefined;
  const lookup = async (): Promise<Connection> => {
    const { invoke } = await import("@tauri-apps/api/core");
    last = await invoke<Connection>("daemon_connection");
    return last;
  };
  return connectWs({
    url: async () => (await lookup()).url,
    token: async () => (last ?? (await lookup())).token,
    client: { name: "android", version: __APP_VERSION__ },
  });
}

const live = inTauri();
const client: WdmClient = live ? liveClient() : createMockClient({ latencyMs: 30 });

const target = document.getElementById("app");
if (!target) throw new Error("#app is missing from index.html");

mount(App, {
  target,
  props: { client, mode: live ? "live" : "mock", ...(live ? { host: mobileHost() } : {}) },
});
