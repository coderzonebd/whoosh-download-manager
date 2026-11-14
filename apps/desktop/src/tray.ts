// SPDX-License-Identifier: GPL-3.0-or-later
// Mirrors the daemon's total speed into the tray icon's tooltip. (Linux app
// indicators do not show tooltips; see the README.)
import type { WhooshClient } from "@whoosh/api-client";

function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export function startTrayUpdates(client: WhooshClient, speed: (bps: number) => string, active: (n: number) => string): () => void {
  if (!inTauri()) return () => {};
  let last = "";
  let invoke: ((cmd: string, args: Record<string, unknown>) => Promise<unknown>) | undefined;
  void import("@tauri-apps/api/core").then((m) => (invoke = m.invoke));
  return client.on("event.stats", (s) => {
    const text = s.active > 0 ? `Whoosh — ↓ ${speed(s.downloadBps)} · ${active(s.active)}` : "Whoosh";
    if (text === last || !invoke) return;
    last = text;
    invoke("set_tray_tooltip", { text }).catch(() => {
      // tray not available (e.g. no status notifier on this desktop)
    });
  });
}
