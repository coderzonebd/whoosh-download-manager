// SPDX-License-Identifier: GPL-3.0-or-later
// What the desktop shell adds to the shared UI (`Host` in @whoosh/ui): the
// native folder picker, the app's own settings (`desktop.json`, through the
// `desktop_prefs` / `set_desktop_prefs` commands), launch at login, the
// "Send crash reports" switch (`crash.rs`), and opening the translation site
// from the language picker.
import type { DesktopPrefs, Host } from "@whoosh/ui";

/** The full `desktop.json` as the shell sends it (`prefs.rs`). */
interface ShellPrefs extends DesktopPrefs {
  trayNoticeShown: boolean;
  /** Kept by the shell for the tray (`shell_i18n.rs`). */
  language: string;
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const core = await import("@tauri-apps/api/core");
  return core.invoke<T>(cmd, args);
}

function pick({ closeToTray, stopDaemonOnQuit }: DesktopPrefs): DesktopPrefs {
  return { closeToTray, stopDaemonOnQuit };
}

export function desktopHost(): Host {
  return {
    async pickFolder({ title, defaultPath }) {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const opts: Parameters<typeof open>[0] = { directory: true, multiple: false, title };
      if (defaultPath) opts.defaultPath = defaultPath;
      const dir = await open(opts);
      return typeof dir === "string" ? dir : undefined;
    },
    desktopPrefs: {
      async get() {
        return pick(await invoke<ShellPrefs>("desktop_prefs"));
      },
      async set(next) {
        // The command replaces the whole file: keep what the UI doesn't edit.
        const current = await invoke<ShellPrefs>("desktop_prefs");
        const saved = await invoke<ShellPrefs>("set_desktop_prefs", { prefs: { ...current, ...pick(next) } });
        return pick(saved);
      },
    },
    // Only translation pages: the shell refuses anything else.
    async openUrl(url) {
      await invoke("open_translation_page", { url });
    },
    crashReports: {
      async get() {
        const s = await invoke<{ available: boolean; enabled: boolean }>("crash_reports");
        return s.available ? s.enabled : undefined;
      },
      async set(on) {
        await invoke("set_crash_reports", { enabled: on });
      },
    },
    launchAtLogin: {
      async get() {
        const { isEnabled } = await import("@tauri-apps/plugin-autostart");
        return isEnabled();
      },
      async set(on) {
        const autostart = await import("@tauri-apps/plugin-autostart");
        await (on ? autostart.enable() : autostart.disable());
      },
    },
  };
}
