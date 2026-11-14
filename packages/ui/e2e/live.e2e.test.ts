// SPDX-License-Identifier: Apache-2.0
// End-to-end: the shared UI (the harness page, ?client=live) drives a real
// whoosh-daemon started on a temporary data directory, downloading from the
// labserver. Covers adding a download, pause and resume, bulk remove,
// creating a queue, and a media probe through a fake yt-dlp.
//
// The daemon refuses WebSocket upgrades whose Origin is not the desktop
// webview's (PROTOCOL.md §1). The suite does not loosen that: the page
// connects through a small TCP relay in this process that drops the
// browser's Origin header, the way a native client connects. Test-only.
//
// Binaries: WHOOSH_E2E_DAEMON and WHOOSH_E2E_LABSERVER, else target/debug.
// Run: cargo build -p whoosh-daemon -p whoosh-labserver
//      pnpm --filter @whoosh/ui test:e2e
import { spawn, type ChildProcess } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { createServer, connect, type Server, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import type { Browser, BrowserContext, Page } from "playwright-core";
import { afterAll, beforeAll, describe, expect, inject, it } from "vitest";
import { chromeOrSkip, launch } from "./browser.ts";

const repo = join(import.meta.dirname, "..", "..", "..");
const exe = process.platform === "win32" ? ".exe" : "";
const daemonBin = process.env["WHOOSH_E2E_DAEMON"] ?? join(repo, "target", "debug", `whoosh-daemon${exe}`);
const labBin = process.env["WHOOSH_E2E_LABSERVER"] ?? join(repo, "target", "debug", `whoosh-labserver${exe}`);
const fixture = join(repo, "crates", "media", "tests", "fixtures", "info.json");

const chrome = chromeOrSkip();
const haveBins = existsSync(daemonBin) && existsSync(labBin);
if (!haveBins) {
  if (process.env["WHOOSH_E2E_REQUIRE"] === "1") throw new Error(`missing ${daemonBin} or ${labBin}`);
  console.warn("e2e: build whoosh-daemon and whoosh-labserver first (cargo build -p whoosh-daemon -p whoosh-labserver); skipping");
}
// The fake yt-dlp is a Node script with a shebang: not on Windows.
const canFakeYtDlp = process.platform !== "win32";

async function waitFor<T>(what: string, probe: () => T | undefined | Promise<T | undefined>, timeoutMs = 20_000): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const v = await probe();
    if (v !== undefined && v !== false) return v;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

/** Starts the labserver on a free port; resolves with its http base URL. */
function startLab(): Promise<{ proc: ChildProcess; base: string }> {
  const proc = spawn(labBin, ["--port", "0", "--no-tls", "--cache-mb", "32"], { stdio: ["ignore", "pipe", "inherit"] });
  return new Promise((resolve, reject) => {
    let out = "";
    proc.on("error", reject);
    proc.on("exit", (code) => reject(new Error(`labserver exited (${code}): ${out}`)));
    proc.stdout?.on("data", (chunk: Buffer) => {
      out += chunk.toString();
      const m = /http\s+(http:\/\/127\.0\.0\.1:\d+)/.exec(out);
      if (m?.[1]) resolve({ proc, base: m[1] });
    });
  });
}

/**
 * Relays TCP connections to the daemon, dropping the Origin header of the
 * WebSocket upgrade so the browser page connects like a native client.
 */
function startRelay(daemonPort: number): Promise<{ server: Server; port: number }> {
  const sockets = new Set<Socket>();
  const server = createServer((down) => {
    sockets.add(down);
    let head = Buffer.alloc(0);
    const onData = (chunk: Buffer): void => {
      head = Buffer.concat([head, chunk]);
      const end = head.indexOf("\r\n\r\n");
      if (end < 0) return;
      down.off("data", onData);
      const request = head
        .subarray(0, end)
        .toString("latin1")
        .split("\r\n")
        .filter((line) => !/^origin\s*:/i.test(line))
        .join("\r\n");
      const up = connect(daemonPort, "127.0.0.1", () => {
        up.write(Buffer.concat([Buffer.from(request + "\r\n\r\n", "latin1"), head.subarray(end + 4)]));
        down.pipe(up);
        up.pipe(down);
      });
      sockets.add(up);
      const close = (): void => {
        down.destroy();
        up.destroy();
      };
      up.on("error", close).on("close", close);
      down.on("error", close).on("close", close);
    };
    down.on("data", onData);
  });
  server.on("close", () => sockets.forEach((s) => s.destroy()));
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      const addr = server.address();
      resolve({ server, port: typeof addr === "object" && addr ? addr.port : 0 });
    });
  });
}

function stop(proc: ChildProcess | undefined): Promise<void> {
  if (!proc || proc.exitCode !== null) return Promise.resolve();
  return new Promise((resolve) => {
    const t = setTimeout(() => proc.kill("SIGKILL"), 5000);
    proc.once("exit", () => {
      clearTimeout(t);
      resolve();
    });
    proc.kill("SIGTERM");
  });
}

describe.skipIf(!chrome || !haveBins)("desktop end-to-end against a real daemon", () => {
  let lab: { proc: ChildProcess; base: string };
  let daemon: ChildProcess | undefined;
  let relay: { server: Server; port: number };
  let browser: Browser;
  let context: BrowserContext;
  let page: Page;
  let root: string;
  let downloads: string;
  let token: string;
  const base = inject("harnessUrl");

  async function openPage(): Promise<void> {
    page = await context.newPage();
    page.on("pageerror", (e) => console.error("page error:", e.message));
    await page.goto(`${base}?client=live&lang=en`);
    await page.waitForFunction(() => window.__whooshApp?.connection === "open", undefined, { timeout: 20_000 });
    await page.waitForFunction(() => window.__whooshApp?.store.loaded === true, undefined, { timeout: 20_000 });
  }

  const row = (name: string) => page.locator('[role="grid"] [role="row"][data-row-id]').filter({ hasText: name });

  async function addViaDialog(url: string, name: string): Promise<void> {
    await page.getByRole("button", { name: "Add download" }).click();
    const dialog = page.getByRole("dialog", { name: "Add a download" });
    await dialog.locator("#vy-add-url").fill(url);
    // The probe fills in the file name; then set the folder to the temp dir.
    await expect.poll(() => dialog.locator("#vy-add-name").inputValue(), { timeout: 15_000 }).toBe(name);
    await dialog.locator("#vy-add-dir").fill(downloads);
    await dialog.getByRole("button", { name: "Download", exact: true }).click();
    await dialog.waitFor({ state: "detached" });
    await row(name).first().waitFor();
  }

  beforeAll(async () => {
    root = mkdtempSync(join(tmpdir(), "whoosh-e2e-"));
    const data = join(root, "data");
    downloads = join(root, "downloads");
    mkdirSync(downloads, { recursive: true });
    lab = await startLab();

    if (canFakeYtDlp) {
      const bin = join(data, "media", "bin");
      mkdirSync(bin, { recursive: true });
      copyFileSync(join(import.meta.dirname, "fake-yt-dlp.mjs"), join(bin, "yt-dlp"));
      chmodSync(join(bin, "yt-dlp"), 0o755);
    }
    daemon = spawn(daemonBin, ["--data-dir", data, "--port", "0"], {
      stdio: ["ignore", "inherit", "inherit"],
      env: {
        ...process.env,
        // The fake yt-dlp's `#!/usr/bin/env node` must find this Node.
        PATH: `${dirname(process.execPath)}${process.platform === "win32" ? ";" : ":"}${process.env["PATH"] ?? ""}`,
        WHOOSH_FAKE_FIXTURE: fixture,
        WHOOSH_FAKE_BASE: lab.base,
        RUST_LOG: process.env["RUST_LOG"] ?? "warn",
      },
    });
    const runtime = join(data, "runtime.json");
    const info = await waitFor("runtime.json", () => {
      try {
        return JSON.parse(readFileSync(runtime, "utf8")) as { port: number; token: string };
      } catch {
        return undefined;
      }
    });
    token = info.token;
    relay = await startRelay(info.port);

    browser = await launch(chrome as string);
    context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
    await context.addInitScript(
      (conn) => {
        window.__WHOOSH_E2E__ = conn;
      },
      { url: `ws://127.0.0.1:${relay.port}/rpc`, token },
    );
    await openPage();
  });

  afterAll(async () => {
    await context?.close();
    await browser?.close();
    relay?.server.close();
    await stop(daemon);
    await stop(lab?.proc);
    if (root) rmSync(root, { recursive: true, force: true });
  });

  it("adds a download from the labserver and finishes it", async () => {
    await addViaDialog(`${lab.base}/s/clean/1m.bin`, "1m.bin");
    await expect.poll(() => row("1m.bin").first().getAttribute("data-state"), { timeout: 30_000 }).toBe("completed");
    expect(readdirSync(downloads)).toContain("1m.bin");
  });

  it("pauses and resumes a download", async () => {
    // 1 GiB at 20 MB/s: long enough to pause in the middle.
    await addViaDialog(`${lab.base}/s/global-throttle/1g.bin`, "1g.bin");
    const r = row("1g.bin").first();
    await expect.poll(() => r.getAttribute("data-state"), { timeout: 15_000 }).toBe("downloading");
    await r.click();
    const actions = page.getByRole("group", { name: "Download actions" });
    await actions.getByRole("button", { name: "Pause", exact: true }).click();
    await expect.poll(() => r.getAttribute("data-state"), { timeout: 15_000 }).toBe("paused");
    const progress = (): Promise<number> =>
      page.evaluate(() => {
        const d = window.__whooshApp?.store.all().find((x) => x.data.filename === "1g.bin");
        return d?.data.downloadedBytes ?? -1;
      });
    const atPause = await progress();
    await page.waitForTimeout(1000);
    expect(await progress()).toBe(atPause);
    await actions.getByRole("button", { name: "Resume", exact: true }).click();
    await expect.poll(() => r.getAttribute("data-state"), { timeout: 15_000 }).toBe("downloading");
    await expect.poll(progress, { timeout: 15_000 }).toBeGreaterThan(atPause);
  });

  it("removes several downloads at once with the row checkboxes", async () => {
    await addViaDialog(`${lab.base}/s/clean/10m.bin`, "10m.bin");
    // Tick the three row checkboxes (one may be selected already).
    for (const name of ["1m.bin", "1g.bin", "10m.bin"]) {
      const pick = row(name).first().locator("[data-pick]");
      if ((await pick.getAttribute("aria-checked")) !== "true") await pick.click();
    }
    await page.getByRole("checkbox", { name: /Selected: 3/ }).waitFor();
    await page.getByRole("group", { name: "Download actions" }).getByRole("button", { name: "Remove", exact: true }).click();
    const dialog = page.getByRole("alertdialog", { name: "Remove 3 downloads?" });
    await dialog.getByRole("checkbox", { name: /delete the files/ }).check();
    await dialog.getByRole("button", { name: "Remove", exact: true }).click();
    await expect.poll(() => page.locator('[role="grid"] [role="row"][data-row-id]').count(), { timeout: 15_000 }).toBe(0);
    // The daemon agrees.
    const left = await page.evaluate(async () => (await window.__whooshApp!.client.call("download.list", {})).length);
    expect(left).toBe(0);
  });

  it("creates a queue", async () => {
    await page.getByRole("button", { name: "New queue" }).click();
    const dialog = page.getByRole("dialog", { name: "New queue" });
    await dialog.getByLabel("Name").fill("Night");
    await dialog.getByRole("button", { name: "Create queue" }).click();
    await dialog.waitFor({ state: "detached" });
    const nav = page.getByRole("navigation");
    await nav.getByRole("button", { name: /Night/ }).waitFor();
    // It is the daemon's: a fresh page sees it too.
    await page.close();
    await openPage();
    await page.getByRole("navigation").getByRole("button", { name: /Night/ }).waitFor();
  });

  it.skipIf(!canFakeYtDlp)("probes a video page with yt-dlp and shows the format picker", async () => {
    await page.getByRole("button", { name: "Add download" }).click();
    const dialog = page.getByRole("dialog", { name: "Add a download" });
    await dialog.locator("#vy-add-url").fill(`${lab.base}/watch?v=abc123`);
    try {
      await dialog.locator("#vy-media-title").waitFor({ timeout: 20_000 });
    } catch (e) {
      const debug = await page.evaluate(async (u) => {
        const app = window.__whooshApp!;
        let probe: string;
        try {
          probe = JSON.stringify(await app.client.call("media.probe", { url: u })).slice(0, 200);
        } catch (err) {
          probe = String(err);
        }
        return { supported: app.media.supported, tools: JSON.stringify(app.media.tools), probe };
      }, `${lab.base}/watch?v=abc123`);
      throw new Error(`no format picker; the dialog says: ${await dialog.textContent()}; ${JSON.stringify(debug)}`, { cause: e });
    }
    expect(await dialog.locator("#vy-media-title").textContent()).toBe("Lab clip: AC/DC 100%");
    // Formats from the fixture: 1080p video and m4a audio.
    const qualities = await dialog.locator("option").allTextContents();
    expect(qualities.some((q) => q.startsWith("1080p"))).toBe(true);
    // The audio choice comes from the m4a format.
    await dialog.getByRole("radio", { name: /Audio/ }).waitFor();
    await dialog.getByRole("button", { name: "Cancel" }).click();
  });
});
