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
  /** Look for a new version once a day (never installs on its own). */
  autoUpdateCheck: boolean;
}

/** Browsers the "Connect your browser" page knows. */
export type BrowserId = "chrome" | "chromium" | "brave" | "edge" | "vivaldi" | "firefox";

/** Where the extension for a browser comes from. */
export type ExtensionStore = "edgeAddOns" | "chromeWebStore" | "amo" | "github";

/** One browser on the "Connect your browser" page. */
export interface BrowserState {
  browser: BrowserId;
  /** This user has used the browser (its profile exists). */
  inUse: boolean;
  /** The native messaging host is registered for it. */
  hostRegistered: boolean;
  /** The extension appears in one of its profiles. */
  extensionFound: boolean;
  store: ExtensionStore;
}

/** A newer version the app can install. */
export interface UpdateInfo {
  version: string;
  currentVersion: string;
  notes?: string | null;
}

export interface CrashReportsState {
  enabled: boolean;
  /** The first-run notice was dismissed, or the user chose in Settings. */
  noticeShown: boolean;
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
   * "Send crash reports": on by default, can be turned off. `get` resolves
   * undefined when this build cannot send any (no crash-report address):
   * then the switch and the first-run notice are hidden.
   */
  crashReports?: {
    get(): Promise<CrashReportsState | undefined>;
    set(on: boolean): Promise<void>;
    /** The first-run notice was dismissed; it is not shown again. */
    dismissNotice(): Promise<void>;
  };
  /** Start the app when the user logs in. */
  launchAtLogin?: {
    get(): Promise<boolean>;
    set(on: boolean): Promise<void>;
  };
  /** The "Connect your browser" page (desktop app). */
  browsers?: {
    status(): Promise<BrowserState[]>;
    /** Register the native messaging host for one browser, or every browser in use. */
    connect(browser?: BrowserId): Promise<void>;
    /** Open the page that installs the extension in this browser. */
    openStore(browser: BrowserId): Promise<void>;
    /** True once, on the first run: the page opens by itself. */
    firstRun(): Promise<boolean>;
  };
  /** Signed in-app updates (desktop app). */
  updates?: {
    /** undefined when this is the newest version. */
    check(): Promise<UpdateInfo | undefined>;
    /** Download, install and restart. */
    install(): Promise<void>;
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
