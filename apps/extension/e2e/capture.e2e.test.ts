// End-to-end: the built Chromium extension, loaded unpacked into Chrome for
// Testing, talks to a fake native host over real native messaging.
//
// Chrome is started directly and Playwright attaches over CDP, then download
// handling is given back to Chrome: Playwright's own (Browser.setDownloadBehavior
// "allowAndName") bypasses extensions' downloads.onDeterminingFilename —
// exactly what is under test.

import { spawn, type ChildProcess } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { chromium, type Browser, type BrowserContext, type Worker } from "playwright-core";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

const EXTENSION_ID = "clhjgofaikdmabdnebgcmdffdjenagfl";
const HOST_NAME = "com.coderzonebd.whoosh";
const root = join(import.meta.dirname, "..");
const extensionDir = join(root, "dist", "chromium");

function findChrome(): string | undefined {
  const fromEnv = process.env["WHOOSH_E2E_CHROME"];
  if (fromEnv) return fromEnv;
  const cache = join(homedir(), ".cache", "ms-playwright");
  if (!existsSync(cache)) return undefined;
  const builds = readdirSync(cache)
    .filter((d) => /^chromium-\d+$/.test(d))
    .sort((a, b) => Number(b.split("-")[1]) - Number(a.split("-")[1]));
  for (const build of builds) {
    const exe = join(cache, build, "chrome-linux64", "chrome");
    if (existsSync(exe)) return exe;
  }
  return undefined;
}

const chromePath = process.platform === "linux" ? findChrome() : undefined;
if (!chromePath) {
  console.warn("e2e: no Chrome for Testing found (set WHOOSH_E2E_CHROME); skipping");
}

interface LogEntry {
  at: number;
  msg?: { id?: number; method?: string; params?: Record<string, unknown> };
}

async function waitFor<T>(what: string, probe: () => Promise<T | undefined> | T | undefined, timeoutMs = 15_000): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await probe();
    if (value !== undefined) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

describe.skipIf(!chromePath)("Chromium extension ⇄ native host", () => {
  const work = mkdtempSync(join(tmpdir(), "whoosh-e2e-"));
  const profile = join(work, "profile");
  const downloads = join(work, "downloads");
  const hostLog = join(work, "host.jsonl");
  const control = join(work, "control.json");
  const hostDir = join(profile, "NativeMessagingHosts");

  let server: Server;
  let origin = "";
  let chromeProcess: ChildProcess;
  let browser: Browser;
  let context: BrowserContext;

  const setMode = (mode: object) => writeFileSync(control, JSON.stringify(mode));
  const hostMessages = (): LogEntry[] =>
    existsSync(hostLog)
      ? readFileSync(hostLog, "utf8")
          .split("\n")
          .filter(Boolean)
          .map((l) => JSON.parse(l) as LogEntry)
      : [];
  const hostCalls = (method: string) => hostMessages().filter((e) => e.msg?.method === method);

  const swConsole: string[] = [];
  const watched = new WeakSet<Worker>();

  async function worker(): Promise<Worker> {
    // Only our extension's worker: Chrome may start other service workers
    // first, and taking the next one of any kind made this flaky.
    const ours = (w: Worker) => w.url().startsWith(`chrome-extension://${EXTENSION_ID}/`);
    const w =
      context.serviceWorkers().find(ours) ??
      (await context.waitForEvent("serviceworker", { predicate: ours, timeout: 15_000 }).catch((e: unknown) => {
        const seen = context.serviceWorkers().map((x) => x.url());
        throw new Error(`no service worker of ${EXTENSION_ID}; saw: ${seen.join(", ") || "none"} (${String(e)})`);
      }));
    if (!watched.has(w)) {
      watched.add(w);
      w.on("console", (m) => swConsole.push(`${m.type()}: ${m.text()}`));
    }
    return w;
  }

  async function browserDownloads(): Promise<chrome.downloads.DownloadItem[]> {
    return (await worker()).evaluate(() => chrome.downloads.search({}));
  }

  async function clickDownload(name: string): Promise<void> {
    const page = await context.newPage();
    await page.goto(`${origin}/?file=${name}`);
    await page.click("#file");
    await page.waitForTimeout(200);
    await page.close();
  }

  beforeAll(async () => {
    mkdirSync(join(profile, "Default"), { recursive: true });
    mkdirSync(downloads, { recursive: true });
    // Keep test downloads out of the real ~/Downloads, without a prompt.
    writeFileSync(
      join(profile, "Default", "Preferences"),
      JSON.stringify({ download: { default_directory: downloads, prompt_for_download: false, directory_upgrade: true } }),
    );
    setMode({ mode: "accept" });

    server = createServer((req, res) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (url.pathname === "/") {
        const file = url.searchParams.get("file") ?? "test";
        res.writeHead(200, { "content-type": "text/html", "set-cookie": "session=e2e-cookie; Path=/" });
        res.end(`<!doctype html><title>e2e</title><a id="file" href="/files/${file}.zip">download</a>`);
        return;
      }
      if (url.pathname.startsWith("/files/")) {
        const name = url.pathname.slice("/files/".length);
        const body = Buffer.alloc(64 * 1024, 7);
        res.writeHead(200, {
          "content-type": "application/zip",
          "content-disposition": `attachment; filename="${name}"`,
          "content-length": String(body.length),
        });
        res.end(body);
        return;
      }
      res.writeHead(404).end();
    });
    await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("no server address");
    origin = `http://127.0.0.1:${address.port}`;

    chromeProcess = spawn(
      chromePath!,
      [
        "--headless=new",
        `--user-data-dir=${profile}`,
        "--remote-debugging-port=0",
        "--no-first-run",
        "--no-default-browser-check",
        "--no-sandbox",
        "--disable-gpu",
        "--disable-features=DisableLoadExtensionCommandLineSwitch",
        `--disable-extensions-except=${extensionDir}`,
        `--load-extension=${extensionDir}`,
        "about:blank",
      ],
      {
        env: { ...process.env, WHOOSH_FAKE_HOST_LOG: hostLog, WHOOSH_FAKE_HOST_CONTROL: control },
        stdio: "ignore",
      },
    );
    const portFile = join(profile, "DevToolsActivePort");
    const port = await waitFor("DevToolsActivePort", () =>
      existsSync(portFile) ? readFileSync(portFile, "utf8").split("\n")[0] || undefined : undefined,
    );
    browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
    const first = browser.contexts()[0];
    if (!first) throw new Error("no default context");
    context = first;
    // Even over CDP, Playwright installs its own download handling, which
    // would bypass the extension. Hand downloads back to Chrome.
    const cdp = await browser.newBrowserCDPSession();
    await cdp.send("Browser.setDownloadBehavior", { behavior: "default" });
    await worker();
  });

  afterAll(async () => {
    await browser?.close().catch(() => {});
    chromeProcess?.kill();
    await new Promise<void>((resolve) => server?.close(() => resolve()));
    if (process.env["WHOOSH_E2E_KEEP"]) {
      writeFileSync(join(work, "sw-console.log"), swConsole.join("\n"));
      console.log(`e2e: kept ${work}`);
    } else {
      rmSync(work, { recursive: true, force: true });
    }
  });

  it("loads with the pinned development ID", async () => {
    expect((await worker()).url()).toBe(`chrome-extension://${EXTENSION_ID}/background.js`);
  });

  it("leaves the download to the browser when no native host is installed", async () => {
    await clickDownload("no-host");
    const item = await waitFor("browser download", async () =>
      (await browserDownloads()).find((d) => d.url.endsWith("/no-host.zip") && d.state === "complete"),
    );
    expect(item.filename.endsWith("no-host.zip")).toBe(true);
    expect(existsSync(join(downloads, "no-host.zip"))).toBe(true);
    expect(hostMessages()).toEqual([]);
  });

  it("registers the fake host", async () => {
    const wrapper = join(work, "fake-host.sh");
    writeFileSync(wrapper, `#!/bin/sh\nexec "${process.execPath}" "${join(root, "e2e", "fake-host.mjs")}" "$@"\n`);
    chmodSync(wrapper, 0o755);
    mkdirSync(hostDir, { recursive: true });
    writeFileSync(
      join(hostDir, `${HOST_NAME}.json`),
      JSON.stringify({
        name: HOST_NAME,
        description: "fake host for e2e",
        path: wrapper,
        type: "stdio",
        allowed_origins: [`chrome-extension://${EXTENSION_ID}/`],
      }),
    );
    // Let the transport's fail-fast window from the previous test pass.
    await new Promise((r) => setTimeout(r, 2_100));
  });

  it("shows the connection and counts in the popup", async () => {
    const page = await context.newPage();
    await page.goto(`chrome-extension://${EXTENSION_ID}/popup.html`);
    await expect.poll(() => page.textContent("#status")).toBe("Connected");
    expect(await page.textContent("#detail")).toBe("whoosh e2e");
    expect(await page.textContent("#active")).toBe("3");
    expect(hostCalls("nm.status").length).toBeGreaterThan(0);
    // Chrome passes the caller's origin to the host.
    expect(hostMessages()[0]).toMatchObject({ argv: [`chrome-extension://${EXTENSION_ID}/`] });
    await page.close();
  });

  it("hands a captured download to the host with cookies and referrer, and cancels the browser copy", async () => {
    setMode({ mode: "accept" });
    await clickDownload("captured");
    const add = await waitFor("download.add", () =>
      hostCalls("download.add").find((e) => String(e.msg?.params?.["url"]).endsWith("/captured.zip")),
    );
    expect(add.msg?.params).toMatchObject({
      url: `${origin}/files/captured.zip`,
      filename: "captured.zip",
      request: {
        cookie: "session=e2e-cookie",
        referer: `${origin}/?file=captured`,
        userAgent: expect.stringContaining("Mozilla/5.0"),
      },
    });
    // The browser's copy is cancelled and erased from its list.
    await waitFor("browser copy gone", async () =>
      (await browserDownloads()).some((d) => d.url.endsWith("/captured.zip")) ? undefined : true,
    );
    expect(existsSync(join(downloads, "captured.zip"))).toBe(false);
  });

  it("asks for a preconnect after hovering a download link", async () => {
    const page = await context.newPage();
    await page.goto(`${origin}/?file=hovered`);
    await page.hover("#file");
    const call = await waitFor("net.preconnect", () => hostCalls("net.preconnect")[0]);
    // Only the origin is shared, never the full link.
    expect(call.msg?.params).toEqual({ url: `${origin}/` });
    await page.close();
  });

  it("keeps the download in the browser when the daemon is unavailable", async () => {
    setMode({ mode: "error" });
    await clickDownload("refused");
    await waitFor("download.add", () => hostCalls("download.add").find((e) => String(e.msg?.params?.["url"]).endsWith("/refused.zip")));
    await waitFor("browser download", async () =>
      (await browserDownloads()).find((d) => d.url.endsWith("/refused.zip") && d.state === "complete"),
    );
    expect(existsSync(join(downloads, "refused.zip"))).toBe(true);
  });

  it("keeps the download when the daemon is too slow, and removes the late daemon copy", async () => {
    const sw = await worker();
    await sw.evaluate(async () => {
      const { settings } = await chrome.storage.local.get("settings");
      await chrome.storage.local.set({ settings: { ...(settings ?? {}), handoffTimeoutMs: 1000 } });
    });
    setMode({ mode: "slow", delayMs: 2500 });
    await clickDownload("slow");
    await waitFor("browser download", async () =>
      (await browserDownloads()).find((d) => d.url.endsWith("/slow.zip") && d.state === "complete"),
    );
    expect(existsSync(join(downloads, "slow.zip"))).toBe(true);
    const add = hostCalls("download.add").find((e) => String(e.msg?.params?.["url"]).endsWith("/slow.zip"));
    expect(add).toBeDefined();
    const remove = await waitFor("download.remove", () => hostCalls("download.remove")[0]);
    expect(remove.msg?.params).toEqual({ ids: [`dl_e2e_${add?.msg?.id}`], deleteFiles: true });
  });
});
