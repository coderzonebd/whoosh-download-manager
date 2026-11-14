// SPDX-License-Identifier: GPL-3.0-or-later
// What the desktop shell adds to the shared UI (`Host` in @whoosh/ui): the
// native folder picker, the app's own settings (`desktop.json`, through the
// `desktop_prefs` / `set_desktop_prefs` commands), launch at login, the
// "Send crash reports" switch (`crash.rs`), opening the translation site
// from the language picker, "Connect your browser" (`browsers.rs`) and signed
// updates (`updates.rs`).
import type { BrowserState, CrashReportsState, DesktopPrefs, Host, UpdateInfo } from "@whoosh/ui";

/** The full `desktop.json` as the shell sends it (`prefs.rs`). */
interface ShellPrefs extends DesktopPrefs {
  trayNoticeShown: boolean;
  /** Kept by the shell for the tray (`shell_i18n.rs`). */
  language: string;
  lastUpdateCheck: number;
  browserSetupShown: boolean;
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const core = await import("@tauri-apps/api/core");
  return core.invoke<T>(cmd, args);
}

function pick({ closeToTray, stopDaemonOnQuit, autoUpdateCheck }: DesktopPrefs): DesktopPrefs {
  return { closeToTray, stopDaemonOnQuit, autoUpdateCheck };
}

/** Changes some fields of `desktop.json`, keeping the rest. */
async function patchPrefs(patch: Partial<ShellPrefs>): Promise<ShellPrefs> {
  const current = await invoke<ShellPrefs>("desktop_prefs");
  return invoke<ShellPrefs>("set_desktop_prefs", { prefs: { ...current, ...patch } });
}

/** Download, install and restart (the shell restarts the app on success). */
export async function installUpdate(): Promise<void> {
  await invoke("install_update");
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
        return pick(await patchPrefs(pick(next)));
      },
    },
    // Only translation pages: the shell refuses anything else.
    async openUrl(url) {
      await invoke("open_translation_page", { url });
    },
    crashReports: {
      async get() {
        const s = await invoke<{ available: boolean; enabled: boolean; noticeShown: boolean }>("crash_reports");
        return s.available ? { enabled: s.enabled, noticeShown: s.noticeShown } : undefined;
      },
      async set(on) {
        await invoke("set_crash_reports", { enabled: on });
      },
      async dismissNotice() {
        await invoke("dismiss_crash_notice");
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
    browsers: {
      status: () => invoke<BrowserState[]>("browser_status"),
      async connect(browser) {
        await invoke("connect_browser", { browser: browser ?? null });
      },
      async openStore(browser) {
        await invoke("open_extension_page", { browser });
      },
      async firstRun() {
        const prefs = await invoke<ShellPrefs>("desktop_prefs");
        if (prefs.browserSetupShown) return false;
        await patchPrefs({ browserSetupShown: true });
        return true;
      },
    },
    updates: {
      async check() {
        return (await invoke<UpdateInfo | null>("check_for_update")) ?? undefined;
      },
      install: installUpdate,
    },
  };
}
