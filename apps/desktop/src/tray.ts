// SPDX-License-Identifier: GPL-3.0-or-later
// Mirrors the total speed into the tray icon's tooltip. A stub until the
// daemon owns the tray: today the numbers come from the mock. (Linux app
// indicators do not show tooltips; see the README.)
import type { VayuClient } from "@vayu/api-client";

function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export function startTrayUpdates(client: VayuClient, speed: (bps: number) => string, active: (n: number) => string): () => void {
  if (!inTauri()) return () => {};
  let last = "";
  let invoke: ((cmd: string, args: Record<string, unknown>) => Promise<unknown>) | undefined;
  void import("@tauri-apps/api/core").then((m) => (invoke = m.invoke));
  return client.on("event.stats", (s) => {
    const text = s.active > 0 ? `Vayu — ↓ ${speed(s.downloadBps)} · ${active(s.active)}` : "Vayu";
    if (text === last || !invoke) return;
    last = text;
    invoke("set_tray_tooltip", { text }).catch(() => {
      // tray not available (e.g. no status notifier on this desktop)
    });
  });
}
