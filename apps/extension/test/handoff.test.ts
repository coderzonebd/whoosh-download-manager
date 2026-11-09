import type { AddParams, AddResult } from "@vayu/api-client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  handOff,
  type BrowserDownloadState,
  type DaemonCalls,
  type DownloadControl,
} from "../src/background/capture/handoff";
import { HostErrorCode, RpcError, TransportError } from "../src/transport/rpc";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** Shared event log so the order of browser and daemon actions can be asserted. */
type Log = string[];

class FakeDownloads implements DownloadControl {
  readonly states = new Map<number, BrowserDownloadState>();
  /** "cancels": in_progress → interrupted; "ignores": nothing changes; "throws": rejects. */
  cancelMode: "cancels" | "ignores" | "throws" = "cancels";

  constructor(private readonly log: Log) {}

  async cancel(id: number): Promise<void> {
    this.log.push(`browser.cancel ${id}`);
    if (this.cancelMode === "throws") throw new Error("Download must be in progress");
    if (this.cancelMode === "cancels" && this.states.get(id) === "in_progress") this.states.set(id, "interrupted");
  }

  async erase(id: number): Promise<void> {
    this.log.push(`browser.erase ${id}`);
    this.states.delete(id);
  }

  async state(id: number): Promise<BrowserDownloadState> {
    return this.states.get(id) ?? "missing";
  }
}

class FakeDaemon implements DaemonCalls {
  readonly add = deferred<AddResult>();
  removeFails = false;

  constructor(private readonly log: Log) {}

  call(method: string, params: unknown): Promise<never> {
    this.log.push(`daemon.${method} ${JSON.stringify(params)}`);
    if (method === "download.add") return this.add.promise as Promise<never>;
    if (method === "download.remove") {
      return (this.removeFails ? Promise.reject(new Error("gone")) : Promise.resolve(null)) as Promise<never>;
    }
    return Promise.reject(new Error(`unexpected ${method}`));
  }
}

const PARAMS: AddParams = { url: "https://example.com/big.iso" };
const ID = 42;

function setup() {
  const log: Log = [];
  const downloads = new FakeDownloads(log);
  downloads.states.set(ID, "in_progress");
  const daemon = new FakeDaemon(log);
  const release = vi.fn(() => log.push("release"));
  const run = (prepare: () => Promise<AddParams> = async () => PARAMS, timeoutMs = 4000) =>
    handOff(ID, prepare, { daemon, downloads }, { timeoutMs, release });
  return { log, downloads, daemon, release, run };
}

describe("handOff", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("cancels and erases the browser copy only after the daemon accepted", async () => {
    const { log, daemon, release, run } = setup();
    const pending = run();
    await vi.advanceTimersByTimeAsync(100);
    expect(log).toEqual([`daemon.download.add ${JSON.stringify(PARAMS)}`]);
    expect(release).not.toHaveBeenCalled();

    daemon.add.resolve({ id: "dl_1" });
    await expect(pending).resolves.toEqual({ outcome: "sent", daemonId: "dl_1" });
    expect(log).toEqual([
      `daemon.download.add ${JSON.stringify(PARAMS)}`,
      `browser.cancel ${ID}`,
      "release",
      `browser.erase ${ID}`,
    ]);
    expect(release).toHaveBeenCalledTimes(1);
  });

  it("keeps the download when the host or daemon is unavailable", async () => {
    for (const error of [
      new TransportError("host-missing", "Specified native messaging host not found."),
      new RpcError({ code: HostErrorCode.DaemonUnavailable, message: "the vayu daemon is not running" }),
    ]) {
      const { log, daemon, release, run, downloads } = setup();
      const pending = run();
      daemon.add.reject(error);
      const result = await pending;
      expect(result).toMatchObject({ outcome: "kept", reason: "error", error });
      expect(release).toHaveBeenCalledTimes(1);
      expect(log.some((l) => l.startsWith("browser."))).toBe(false);
      expect(downloads.states.get(ID)).toBe("in_progress");
    }
  });

  it("keeps the download when the daemon refuses it", async () => {
    const { daemon, release, run } = setup();
    const pending = run();
    daemon.add.reject(new RpcError({ code: -32602, message: "unsupported URL" }));
    await expect(pending).resolves.toMatchObject({ outcome: "kept", reason: "error" });
    expect(release).toHaveBeenCalledTimes(1);
  });

  it("keeps the download when building the request fails", async () => {
    const { log, release, run } = setup();
    const result = await run(async () => {
      throw new Error("cookies unavailable");
    });
    expect(result).toMatchObject({ outcome: "kept", reason: "error" });
    expect(release).toHaveBeenCalledTimes(1);
    expect(log).toEqual(["release"]);
  });

  it("releases at the deadline and removes the daemon copy if a yes arrives late", async () => {
    const { log, daemon, release, run, downloads } = setup();
    const pending = run();
    await vi.advanceTimersByTimeAsync(3999);
    expect(release).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    const result = await pending;
    expect(result).toMatchObject({ outcome: "kept", reason: "timeout" });
    expect(release).toHaveBeenCalledTimes(1);

    daemon.add.resolve({ id: "dl_late" });
    const late = result.outcome === "kept" ? await result.late : undefined;
    expect(late).toEqual({ outcome: "compensated", daemonId: "dl_late" });
    expect(log).toContain(`daemon.download.remove ${JSON.stringify({ ids: ["dl_late"], deleteFiles: true })}`);
    // The browser's copy was never touched.
    expect(log.some((l) => l.startsWith("browser."))).toBe(false);
    expect(downloads.states.get(ID)).toBe("in_progress");
    expect(release).toHaveBeenCalledTimes(1);
  });

  it("reports a duplicate when the late removal fails", async () => {
    const { daemon, run } = setup();
    daemon.removeFails = true;
    const pending = run();
    await vi.advanceTimersByTimeAsync(4000);
    const result = await pending;
    daemon.add.resolve({ id: "dl_dup" });
    const late = result.outcome === "kept" ? await result.late : undefined;
    expect(late).toMatchObject({ outcome: "duplicate", daemonId: "dl_dup" });
  });

  it("does nothing when a late answer is an error", async () => {
    const { log, daemon, run } = setup();
    const pending = run();
    await vi.advanceTimersByTimeAsync(4000);
    const result = await pending;
    daemon.add.reject(new TransportError("disconnected", "host exited"));
    const late = result.outcome === "kept" ? await result.late : undefined;
    expect(late).toEqual({ outcome: "nothing" });
    expect(log.filter((l) => l.includes("remove"))).toEqual([]);
  });

  it("counts building the request against the deadline", async () => {
    const { release, run } = setup();
    const pending = run(() => new Promise<AddParams>(() => {}), 1000);
    await vi.advanceTimersByTimeAsync(1000);
    await expect(pending).resolves.toMatchObject({ outcome: "kept", reason: "timeout" });
    expect(release).toHaveBeenCalledTimes(1);
  });

  it("lets the browser keep a download that finished first, and removes the daemon copy", async () => {
    const { log, daemon, downloads, release, run } = setup();
    downloads.states.set(ID, "complete");
    const pending = run();
    daemon.add.resolve({ id: "dl_2" });
    const result = await pending;
    expect(result).toMatchObject({
      outcome: "kept",
      reason: "browser-finished",
      compensation: { outcome: "compensated", daemonId: "dl_2" },
    });
    expect(log).not.toContain(`browser.erase ${ID}`);
    expect(release).toHaveBeenCalledTimes(1);
  });

  it("lets the browser keep a download it refused to cancel", async () => {
    const { log, daemon, downloads, run } = setup();
    downloads.cancelMode = "ignores";
    const pending = run();
    daemon.add.resolve({ id: "dl_3" });
    await expect(pending).resolves.toMatchObject({ outcome: "kept", reason: "cancel-failed" });
    expect(log).toContain(`daemon.download.remove ${JSON.stringify({ ids: ["dl_3"], deleteFiles: true })}`);
    expect(downloads.states.get(ID)).toBe("in_progress");
  });

  it("treats a download that vanished during cancel as handed over", async () => {
    const { daemon, downloads, run } = setup();
    downloads.cancelMode = "throws";
    downloads.states.delete(ID);
    const pending = run();
    daemon.add.resolve({ id: "dl_4" });
    await expect(pending).resolves.toEqual({ outcome: "sent", daemonId: "dl_4" });
  });

  it("survives a release callback that throws", async () => {
    const { daemon, downloads } = setup();
    const release = vi.fn(() => {
      throw new Error("Invalid download id");
    });
    const pending = handOff(ID, async () => PARAMS, { daemon, downloads }, { timeoutMs: 4000, release });
    daemon.add.resolve({ id: "dl_5" });
    await expect(pending).resolves.toEqual({ outcome: "sent", daemonId: "dl_5" });
    expect(release).toHaveBeenCalledTimes(1);
  });
});
