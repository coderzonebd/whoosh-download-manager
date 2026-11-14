// SPDX-License-Identifier: GPL-3.0-or-later
// Store screenshots (1280×800) from the end-to-end setup: Chrome for Testing
// with the unpacked dist/chromium, the real whoosh-nmhost and
// `whoosh-daemon --mock` (demo data, nothing downloaded). Writes
// store/screenshots/*.png. Nothing outside a temporary directory is touched.
//
//   cargo build -p whoosh-nmhost -p whoosh-daemon
//   pnpm --filter @whoosh/extension build
//   WHOOSH_E2E_CHROME=/path/to/chrome node scripts/store-screenshots.mjs
//
// The host manifest is written into the throwaway profile's own
// NativeMessagingHosts directory (Chrome for Testing looks there).

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { chromium } from "playwright-core";

const extRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const repo = join(extRoot, "../..");
const exe = process.env.WHOOSH_E2E_CHROME;
if (!exe) throw new Error("set WHOOSH_E2E_CHROME to a Chrome for Testing executable");
const work = mkdtempSync(join(tmpdir(), "whoosh-shots-"));
const outDir = join(extRoot, "store", "screenshots");
const EXT = "clhjgofaikdmabdnebgcmdffdjenagfl";
const ext = join(extRoot, "dist/chromium");

const home = join(work, "home"), data = join(work, "data");
const profile = join(work, "profile");
mkdirSync(join(profile, "NativeMessagingHosts"), { recursive: true });
mkdirSync(data, { recursive: true, mode: 0o700 });
mkdirSync(outDir, { recursive: true });
const reg = spawnSync(join(repo, "target/debug/whoosh-nmhost"), ["--register", "--browser", "chrome"], { env: { ...process.env, HOME: home }, encoding: "utf8" });
renameSync(reg.stdout.match(/(\/\S.*com\.coderzonebd\.whoosh\.json)/)[1], join(profile, "NativeMessagingHosts/com.coderzonebd.whoosh.json"));
const daemon = spawn(join(repo, "target/debug/whoosh-daemon"), ["--mock", "--port", "0", "--data-dir", data], { stdio: "ignore" });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
while (!existsSync(join(data, "runtime.json"))) await sleep(100);
const server = createServer((req, res) => {
  res.writeHead(200, { "content-type": "text/html" });
  res.end(`<!doctype html><title>Example downloads</title><body style="font:16px system-ui;margin:40px">
<h1>Example Linux downloads</h1><p><a href="/files/distro-24.04-amd64.iso">distro-24.04-amd64.iso</a> (4.7 GB)</p>
<video src="/media/intro.mp4" width="480" controls></video></body>`);
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const b = spawn(exe, ["--headless=new", `--user-data-dir=${profile}`, "--remote-debugging-port=0", "--no-first-run", "--hide-scrollbars",
  "--disable-features=DisableLoadExtensionCommandLineSwitch", `--disable-extensions-except=${ext}`, `--load-extension=${ext}`, "about:blank"],
  { env: { ...process.env, HOME: home, WHOOSH_DATA_DIR: data }, stdio: "ignore" });
let browser;
try {
  const f = join(profile, "DevToolsActivePort");
  while (!existsSync(f)) await sleep(200);
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${readFileSync(f, "utf8").split("\n")[0]}`);
  const ctx = browser.contexts()[0];
  const ours = (w) => w.url().includes(EXT);
  const sw = ctx.serviceWorkers().find(ours) ?? (await ctx.waitForEvent("serviceworker", { predicate: ours }));
  let page = await ctx.newPage();
  await page.setViewportSize({ width: 1280, height: 800 });
  const frame = `html{background:linear-gradient(135deg,#1e3a8a,#2563eb)!important;min-height:100%}
    body{margin:90px auto!important;zoom:1.55;border-radius:10px;box-shadow:0 20px 50px rgba(0,0,0,.35)}`;

  // A remembered download whose link expired, seeded before the background reads its records.
  await sw.evaluate(async () => {
    await chrome.storage.local.set({ healing: { dl_demo: {
      daemonId: "dl_demo", url: "https://cdn.example.com/files/distro-24.04-amd64.iso?sig=old", originPageUrl: "https://example.com/downloads",
      filename: "distro-24.04-amd64.iso", capturedAt: Date.now(), failedRounds: 1, lastState: "needs_refresh",
      ask: { id: "dl_demo", filename: "distro-24.04-amd64.iso", pageUrl: "https://example.com/downloads", kind: "open-page", askedAt: Date.now() } } } });
  });
  const open = async () => {
    await page.goto(`chrome-extension://${EXT}/popup.html`);
    await page.waitForFunction(() => document.querySelector("#status")?.textContent === "Connected", null, { timeout: 20000 });
    await sleep(500);
    await page.addStyleTag({ content: frame });
  };
  // 2. The self-healing question.
  await open();
  await page.waitForFunction(() => !document.querySelector("#heals-section")?.hidden);
  await page.screenshot({ path: join(outDir, "2-expired-link.png") });
  await page.getByRole("button", { name: "Dismiss" }).click();
  await sleep(500);
  // 1. The popup: connected, counts.
  await open();
  await page.screenshot({ path: join(outDir, "1-popup.png") });

  // 3. Options page.
  await page.goto(`chrome-extension://${EXT}/options.html`);
  await page.waitForFunction(() => document.querySelector("#fileTypes")?.value.length > 0);
  await page.addStyleTag({ content: "html{zoom:1.15}" });
  await page.screenshot({ path: join(outDir, "3-settings.png") });
  console.log("shots written");
} finally {
  await browser?.close().catch(() => {});
  b.kill(); daemon.kill(); server.close();
  rmSync(work, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
