// SPDX-License-Identifier: GPL-3.0-or-later
// What the Android shell adds to the shared UI (`Host` in @wdm/ui). Little for
// now: reading a link from the clipboard for the add dialog.
import type { Host } from "@wdm/ui";

export function mobileHost(): Host {
  return {
    async readClipboard() {
      const { readText } = await import("@tauri-apps/plugin-clipboard-manager");
      return readText();
    },
  };
}
