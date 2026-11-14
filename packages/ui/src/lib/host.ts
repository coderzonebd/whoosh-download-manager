// SPDX-License-Identifier: Apache-2.0
// What a host shell can add to the UI. The UI never imports a shell's APIs
// (Tauri, the extension's chrome.*): the host passes these in as the `host`
// prop, and every part is optional. A missing part hides its controls, so the
// same tree runs in a browser tab, the extension and the desktop app.

/** The desktop app's own settings (`desktop.json`), not the daemon's. */
export interface DesktopPrefs {
  /** Closing the window hides it to the tray; downloads go on. */
  closeToTray: boolean;
  /** Quit also stops the daemon. */
  stopDaemonOnQuit: boolean;
}

export interface Host {
  /** Ask the user for a folder; undefined when they cancel. */
  pickFolder?: (opts: { title: string; defaultPath?: string | undefined }) => Promise<string | undefined>;
  /** Desktop-only settings, shown in Settings → This computer. */
  desktopPrefs?: {
    get(): Promise<DesktopPrefs>;
    set(prefs: DesktopPrefs): Promise<DesktopPrefs>;
  };
  /**
   * Open a web page in the user's browser (the app's own webview must not
   * navigate away). Without it, links open the usual way.
   */
  openUrl?: (url: string) => Promise<void>;
  /**
   * "Send crash reports" (off by default). `get` resolves undefined when
   * this build cannot send any (no crash-report address), and the switch is
   * hidden.
   */
  crashReports?: {
    get(): Promise<boolean | undefined>;
    set(on: boolean): Promise<void>;
  };
  /** Start the app when the user logs in. */
  launchAtLogin?: {
    get(): Promise<boolean>;
    set(on: boolean): Promise<void>;
  };
}

/** What a `whoosh://` link asks for. */
export type DeepLink = { kind: "add"; url: string };

const DOWNLOAD_PROTOCOLS = new Set(["http:", "https:", "ftp:"]);

/** A download address the add dialog accepts: http, https or ftp with a host. */
export function isDownloadUrl(s: string): boolean {
  try {
    const u = new URL(s);
    return DOWNLOAD_PROTOCOLS.has(u.protocol) && u.hostname !== "";
  } catch {
    return false;
  }
}

/**
 * Parse a `whoosh://` link. Understood today: `whoosh://add?url=<address>`
 * (the address percent-encoded). Anything else, or an address that is not
 * http/https/ftp, gives undefined, so a web page can't make the app do more
 * than open the add dialog, which still waits for the user.
 */
export function parseDeepLink(link: string): DeepLink | undefined {
  let u: URL;
  try {
    u = new URL(link.trim());
  } catch {
    return undefined;
  }
  if (u.protocol !== "whoosh:") return undefined;
  // `whoosh://add?…` puts "add" in the host; `whoosh:add?…` in the path.
  const action = (u.hostname || u.pathname).replace(/^\/+|\/+$/g, "").toLowerCase();
  if (action !== "add") return undefined;
  const url = u.searchParams.get("url")?.trim() ?? "";
  if (!isDownloadUrl(url)) return undefined;
  return { kind: "add", url };
}
