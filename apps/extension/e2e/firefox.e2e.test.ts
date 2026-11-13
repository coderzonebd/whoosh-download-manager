// End-to-end in Firefox: dist/firefox is installed as a temporary add-on over
// WebDriver BiDi (`webExtension.install`), with the fake native host
// registered under a temporary HOME. Covers the onCreated capture path.

import { spawn, type ChildProcess } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterAll, beforeAll, describe, expect, it } from "vitest";

const GECKO_ID = "whoosh@coderzonebd.com";
const HOST_NAME = "com.coderzonebd.whoosh";
const FILE_SIZE = 512 * 1024;
const root = join(import.meta.dirname, "..");

function findFirefox(): string | undefined {
  const candidates = [process.env["WHOOSH_E2E_FIREFOX"], "/usr/bin/firefox", "/usr/local/bin/firefox"];
  return candidates.find((p): p is string => !!p && existsSync(p));
}

const firefoxPath = process.platform === "linux" ? findFirefox() : undefined;
if (!firefoxPath) console.warn("e2e: no Firefox found (set WHOOSH_E2E_FIREFOX); skipping");

interface LogEntry {
  argv?: string[];
  msg?: { id?: number; method?: string; params?: Record<string, unknown> };
}

async function waitFor<T>(what: string, probe: () => T | undefined, timeoutMs = 15_000): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = probe();
    if (value !== undefined) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

/** Just enough WebDriver BiDi. */
class Bidi {
  private nextId = 1;
  private readonly pending = new Map<number, (m: Record<string, unknown>) => void>();

  private constructor(private readonly ws: WebSocket) {
    ws.onmessage = (event) => {
      const message = JSON.parse(String(event.data)) as Record<string, unknown>;
      const id = message["id"];
      if (typeof id === "number") {
        this.pending.get(id)?.(message);
        this.pending.delete(id);
      }
    };
  }

  static async connect(url: string): Promise<Bidi> {
    const ws = new WebSocket(url);
    await new Promise<void>((resolve, reject) => {
      ws.onopen = () => resolve();
      ws.onerror = () => reject(new Error(`cannot connect to ${url}`));
    });
    return new Bidi(ws);
  }

  send<T = Record<string, unknown>>(method: string, params: object): Promise<T> {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, (m) =>
        m["type"] === "error" ? reject(new Error(`${method}: ${String(m["error"])} ${String(m["message"])}`)) : resolve(m["result"] as T),
      );
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  close(): void {
    this.ws.close();
  }
}

describe.skipIf(!firefoxPath)("Firefox extension ⇄ native host", () => {
  const work = mkdtempSync(join(tmpdir(), "whoosh-e2e-ff-"));
  const home = join(work, "home");
  const profile = join(work, "profile");
  const downloads = join(work, "downloads");
  const hostLog = join(work, "host.jsonl");
  const control = join(work, "control.json");

  let server: Server;
  let origin = "";
  let firefox: ChildProcess;
  let bidi: Bidi;
  let context = "";

  const setMode = (mode: object) => writeFileSync(control, JSON.stringify(mode));
  const hostCalls = (method: string): LogEntry[] =>
    existsSync(hostLog)
      ? readFileSync(hostLog, "utf8")
          .split("\n")
          .filter(Boolean)
          .map((l) => JSON.parse(l) as LogEntry)
          .filter((e) => e.msg?.method === method)
      : [];

  async function clickDownload(name: string): Promise<void> {
    await bidi.send("browsingContext.navigate", { context, url: `${origin}/?file=${name}`, wait: "complete" });
    await bidi.send("script.evaluate", {
      expression: "document.getElementById('file').click()",
      target: { context },
      awaitPromise: false,
    });
  }

  beforeAll(async () => {
    for (const dir of [home, profile, downloads]) mkdirSync(dir, { recursive: true });
    const prefs: [string, string | number | boolean][] = [
      ["browser.download.dir", downloads],
      ["browser.download.folderList", 2],
      ["browser.download.useDownloadDir", true],
      ["browser.download.always_ask_before_handling_new_types", false],
      ["browser.download.start_downloads_in_tmp_dir", false],
      ["browser.shell.checkDefaultBrowser", false],
      ["datareporting.policy.dataSubmissionEnabled", false],
      ["toolkit.telemetry.reportingpolicy.firstRun", false],
    ];
    writeFileSync(join(profile, "user.js"), prefs.map(([k, v]) => `user_pref("${k}", ${JSON.stringify(v)});`).join("\n"));

    // The same file `whoosh-nmhost --register --browser firefox` writes, under a temporary HOME.
    const wrapper = join(work, "fake-host.sh");
    writeFileSync(wrapper, `#!/bin/sh\nexec "${process.execPath}" "${join(root, "e2e", "fake-host.mjs")}" "$@"\n`);
    chmodSync(wrapper, 0o755);
    const hostDir = join(home, ".mozilla", "native-messaging-hosts");
    mkdirSync(hostDir, { recursive: true });
    writeFileSync(
      join(hostDir, `${HOST_NAME}.json`),
      JSON.stringify({ name: HOST_NAME, description: "fake host for e2e", path: wrapper, type: "stdio", allowed_extensions: [GECKO_ID] }),
    );
    setMode({ mode: "accept" });

    server = createServer((req, res) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (url.pathname === "/") {
        res.writeHead(200, { "content-type": "text/html", "set-cookie": "session=ff-cookie; Path=/" });
        res.end(`<!doctype html><title>e2e</title><a id="file" href="/files/${url.searchParams.get("file")}.zip">download</a>`);
        return;
      }
      // Stream slowly (~1.6 s) so the browser download is still running when the host answers.
      const name = url.pathname.split("/").pop() ?? "x.zip";
      res.writeHead(200, {
        "content-type": "application/zip",
        "content-disposition": `attachment; filename="${name}"`,
        "content-length": String(FILE_SIZE),
      });
      let sent = 0;
      const timer = setInterval(() => {
        const n = Math.min(32 * 1024, FILE_SIZE - sent);
        res.write(Buffer.alloc(n, 1));
        sent += n;
        if (sent >= FILE_SIZE) {
          clearInterval(timer);
          res.end();
        }
      }, 100);
      req.on("close", () => clearInterval(timer));
    });
    await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("no server address");
    origin = `http://127.0.0.1:${address.port}`;

    firefox = spawn(firefoxPath!, ["--headless", "--no-remote", "--profile", profile, "--remote-debugging-port=0", "about:blank"], {
      env: {
        ...process.env,
        HOME: home,
        XDG_CONFIG_HOME: join(home, ".config"),
        WHOOSH_FAKE_HOST_LOG: hostLog,
        WHOOSH_FAKE_HOST_CONTROL: control,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let output = "";
    firefox.stdout?.on("data", (d: Buffer) => (output += d.toString()));
    firefox.stderr?.on("data", (d: Buffer) => (output += d.toString()));
    const wsUrl = await waitFor("WebDriver BiDi", () => output.match(/WebDriver BiDi listening on (ws:\/\/\S+)/)?.[1], 30_000);
    bidi = await Bidi.connect(`${wsUrl}/session`);
    await bidi.send("session.new", { capabilities: {} });
    const tree = await bidi.send<{ contexts: { context: string }[] }>("browsingContext.getTree", {});
    context = tree.contexts[0]?.context ?? "";
  });

  afterAll(async () => {
    await bidi?.send("session.end", {}).catch(() => {});
    bidi?.close();
    firefox?.kill();
    await new Promise<void>((resolve) => server?.close(() => resolve()));
    rmSync(work, { recursive: true, force: true });
  });

  it("installs as a temporary add-on with the gecko ID", async () => {
    const result = await bidi.send<{ extension: string }>("webExtension.install", {
      extensionData: { type: "path", path: join(root, "dist", "firefox") },
    });
    expect(result.extension).toBe(GECKO_ID);
  });

  it("hands a captured download to the host and cancels the browser copy", async () => {
    setMode({ mode: "accept" });
    await clickDownload("ff-captured");
    const add = await waitFor("download.add", () =>
      hostCalls("download.add").find((e) => String(e.msg?.params?.["url"]).endsWith("/ff-captured.zip")),
    );
    expect(add.msg?.params).toMatchObject({
      url: `${origin}/files/ff-captured.zip`,
      filename: "ff-captured.zip",
      request: {
        cookie: "session=ff-cookie",
        referer: `${origin}/?file=ff-captured`,
        userAgent: expect.stringContaining("Firefox/"),
      },
    });
    // Firefox starts the host with the manifest path and the extension ID.
    expect(add.argv?.[1]).toBe(GECKO_ID);
    // Give the (slow) browser download time to finish if it had not been cancelled.
    await new Promise((r) => setTimeout(r, 2_500));
    expect(readdirSync(downloads).filter((f) => f.startsWith("ff-captured"))).toEqual([]);
  });

  it("keeps the download in the browser when the daemon is unavailable", async () => {
    setMode({ mode: "error" });
    await clickDownload("ff-refused");
    const file = join(downloads, "ff-refused.zip");
    await waitFor("complete browser download", () => (existsSync(file) && statSync(file).size === FILE_SIZE ? true : undefined), 20_000);
    expect(hostCalls("download.add").some((e) => String(e.msg?.params?.["url"]).endsWith("/ff-refused.zip"))).toBe(true);
  });
});
