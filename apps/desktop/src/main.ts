// SPDX-License-Identifier: GPL-3.0-or-later
// Desktop entry: the shared UI on the mock daemon until the real daemon
// lands (Phase 1). Then `createMockClient` becomes `connectWs` with the
// port and token from the daemon's runtime file — nothing else changes.
//
// Dev-only query parameters (vite dev server in a browser):
//   ?bulk=10000   add N synthetic downloads    ?lang=bn   start in Bangla
import "@fontsource-variable/inter";
import "@fontsource-variable/noto-sans-bengali";
import "@vayu/ui/styles.css";
import { createMockClient } from "@vayu/api-client/mock";
import { App } from "@vayu/ui";
import { mount } from "svelte";
import { startTrayUpdates } from "./tray.ts";

const q = new URLSearchParams(location.search);
const client = createMockClient({
  latencyMs: 30,
  bulk: Number(q.get("bulk") ?? 0),
  settings: { language: q.get("lang") ?? (navigator.language.startsWith("bn") ? "bn" : "en") },
});

const target = document.getElementById("app");
if (!target) throw new Error("#app is missing from index.html");

mount(App, {
  target,
  props: {
    client,
    mode: "mock",
    onready: (app) => startTrayUpdates(client, (bps) => app.i18n.speed(bps), (n) => app.i18n.t("status.active", { count: n })),
  },
});
