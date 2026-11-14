// SPDX-License-Identifier: GPL-3.0-or-later
// Keeps the shell's own strings (the tray menu, from Rust) in the UI's
// language. The UI's language follows the daemon's `Settings.language`, so
// a change made here, in the CLI or elsewhere reaches the tray too.
import type { AppState } from "@whoosh/ui";

export function startShellLanguage(app: AppState): () => void {
  let invoke: ((cmd: string, args: Record<string, unknown>) => Promise<unknown>) | undefined;
  let pending: string | undefined;
  const send = (language: string) => {
    if (!invoke) {
      pending = language;
      return;
    }
    invoke("set_shell_language", { language }).catch(() => {
      // no tray on this desktop: nothing to translate
    });
  };
  void import("@tauri-apps/api/core").then((m) => {
    invoke = m.invoke;
    if (pending !== undefined) send(pending);
  });
  return $effect.root(() => {
    $effect(() => send(app.i18n.locale));
  });
}
