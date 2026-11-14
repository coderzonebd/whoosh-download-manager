// SPDX-License-Identifier: Apache-2.0
// Finds and starts Chrome for Testing for the browser suites: the path in
// WHOOSH_E2E_CHROME, else the newest build in Playwright's cache. A temporary
// profile every time; the mock keychain keeps macOS from prompting.
import { existsSync, readdirSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { chromium, type Browser } from "playwright-core";

export function findChrome(): string | undefined {
  const fromEnv = process.env["WHOOSH_E2E_CHROME"];
  if (fromEnv) return fromEnv;
  const cache =
    process.platform === "darwin"
      ? join(homedir(), "Library", "Caches", "ms-playwright")
      : process.platform === "win32"
        ? join(process.env["LOCALAPPDATA"] ?? homedir(), "ms-playwright")
        : join(homedir(), ".cache", "ms-playwright");
  if (!existsSync(cache)) return undefined;
  const builds = readdirSync(cache)
    .filter((d) => /^chromium-\d+$/.test(d))
    .sort((a, b) => Number(b.split("-")[1]) - Number(a.split("-")[1]));
  const app = "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing";
  const candidates =
    process.platform === "darwin"
      ? [`chrome-mac-arm64/${app}`, `chrome-mac-x64/${app}`, `chrome-mac/${app}`]
      : process.platform === "win32"
        ? ["chrome-win64/chrome.exe"]
        : ["chrome-linux64/chrome"];
  for (const b of builds) {
    for (const c of candidates) {
      const exe = join(cache, b, c);
      if (existsSync(exe)) return exe;
    }
  }
  return undefined;
}

/** CI sets WHOOSH_E2E_REQUIRE=1 so a missing browser fails instead of skipping. */
export function chromeOrSkip(): string | undefined {
  const exe = findChrome();
  if (!exe && process.env["WHOOSH_E2E_REQUIRE"] === "1") {
    throw new Error("no Chrome for Testing found (set WHOOSH_E2E_CHROME)");
  }
  if (!exe) console.warn("e2e: no Chrome for Testing found (set WHOOSH_E2E_CHROME); skipping");
  return exe;
}

export function launch(executablePath: string): Promise<Browser> {
  return chromium.launch({
    executablePath,
    headless: true,
    args: ["--use-mock-keychain", "--password-store=basic", "--disable-gpu"],
  });
}
