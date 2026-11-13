import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { classifyDisconnect, NativeMessagingTransport, type NativePort } from "../src/transport/native";
import { HostErrorCode, isUnavailable, RpcError, TransportError } from "../src/transport/rpc";

class FakePort implements NativePort {
  readonly sent: Record<string, unknown>[] = [];
  disconnected = false;
  error: { message?: string } | null = null;
  private readonly messageListeners: ((m: unknown) => void)[] = [];
  private readonly disconnectListeners: ((p: NativePort) => void)[] = [];

  onMessage = { addListener: (cb: (m: unknown) => void) => void this.messageListeners.push(cb) };
  onDisconnect = { addListener: (cb: (p: NativePort) => void) => void this.disconnectListeners.push(cb) };

  postMessage(message: unknown): void {
    if (this.disconnected) throw new Error("Attempting to use a disconnected port object");
    this.sent.push(message as Record<string, unknown>);
  }

  disconnect(): void {
    this.disconnected = true;
  }

  /** The host writes a message. */
  receive(message: unknown): void {
    for (const cb of this.messageListeners) cb(message);
  }

  /** The browser closes the port, as for a missing or crashed host. */
  drop(reason?: string): void {
    this.disconnected = true;
    if (reason) this.error = { message: reason };
    for (const cb of this.disconnectListeners) cb(this);
  }

  last(): Record<string, unknown> {
    const m = this.sent.at(-1);
    if (!m) throw new Error("nothing sent");
    return m;
  }
}

function setup(options: { idleMs?: number } = {}) {
  const ports: FakePort[] = [];
  let lastError: string | undefined;
  let clock = 0;
  const transport = new NativeMessagingTransport({
    connectNative: (name) => {
      expect(name).toBe("com.coderzonebd.whoosh");
      const port = new FakePort();
      ports.push(port);
      return port;
    },
    lastError: () => lastError,
    idleMs: options.idleMs ?? 0,
    failFastMs: 2_000,
    now: () => clock,
  });
  return {
    transport,
    ports,
    port: () => {
      const p = ports.at(-1);
      if (!p) throw new Error("no port");
      return p;
    },
    setLastError: (e: string | undefined) => (lastError = e),
    advance: (ms: number) => (clock += ms),
  };
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("NativeMessagingTransport", () => {
  it("sends JSON-RPC requests and resolves their responses", async () => {
    const { transport, port } = setup();
    const pending = transport.call("download.add", { url: "https://x.org/a.zip" });
    expect(port().last()).toEqual({
      jsonrpc: "2.0",
      id: 1,
      method: "download.add",
      params: { url: "https://x.org/a.zip" },
    });
    port().receive({ jsonrpc: "2.0", id: 1, result: { id: "dl_1" } });
    await expect(pending).resolves.toEqual({ id: "dl_1" });
  });

  it("omits params for parameterless methods", async () => {
    const { transport, port } = setup();
    const pending = transport.call("stats.get", undefined);
    expect(port().last()).toEqual({ jsonrpc: "2.0", id: 1, method: "stats.get" });
    port().receive({ jsonrpc: "2.0", id: 1, result: { downloadBps: 0, uploadBps: 0, active: 2, queued: 1 } });
    await expect(pending).resolves.toMatchObject({ active: 2 });
  });

  it("turns error responses into RpcError", async () => {
    const { transport, port } = setup();
    const pending = transport.call("download.get", { id: "nope" });
    port().receive({ jsonrpc: "2.0", id: 1, error: { code: -32002, message: "not found" } });
    const error = await pending.catch((e: unknown) => e);
    expect(error).toBeInstanceOf(RpcError);
    expect((error as RpcError).code).toBe(-32002);
    expect(isUnavailable(error)).toBe(false);
  });

  it("treats the host's daemon-unavailable error as unavailable", async () => {
    const { transport, port } = setup();
    const pending = transport.call("download.add", { url: "https://x.org/a.zip" });
    port().receive({ jsonrpc: "2.0", id: 1, error: { code: HostErrorCode.DaemonUnavailable, message: "not running" } });
    expect(isUnavailable(await pending.catch((e: unknown) => e))).toBe(true);
  });

  it("delivers daemon events to subscribers", async () => {
    const { transport, port } = setup();
    const seen: unknown[] = [];
    const off = transport.on("event.download.state", (e) => seen.push(e));
    void transport.call("stats.get", undefined).catch(() => {});
    port().receive({ jsonrpc: "2.0", method: "event.download.state", params: { id: "dl_1", state: "completed" } });
    off();
    port().receive({ jsonrpc: "2.0", method: "event.download.state", params: { id: "dl_2", state: "failed" } });
    expect(seen).toEqual([{ id: "dl_1", state: "completed" }]);
  });

  it("fails fast for a while after the host turned out to be missing", async () => {
    const { transport, ports, port, setLastError, advance } = setup();
    const first = transport.call("download.add", { url: "https://x.org/a.zip" });
    setLastError("Specified native messaging host not found.");
    port().drop();
    const error = await first.catch((e: unknown) => e);
    expect(error).toBeInstanceOf(TransportError);
    expect((error as TransportError).kind).toBe("host-missing");
    expect(transport.healthy).toBe(false);

    await expect(transport.call("download.add", { url: "https://x.org/b.zip" })).rejects.toMatchObject({
      kind: "host-missing",
    });
    expect(ports).toHaveLength(1); // no second attempt yet

    advance(2_000);
    expect(transport.healthy).toBe(true);
    void transport.call("download.add", { url: "https://x.org/c.zip" }).catch(() => {});
    expect(ports).toHaveLength(2);
  });

  it("reads Firefox's port.error", async () => {
    const { transport, port } = setup();
    const pending = transport.call("stats.get", undefined);
    port().drop("No such native application com.coderzonebd.whoosh");
    await expect(pending).rejects.toMatchObject({ kind: "host-missing" });
  });

  it("reconnects at once after a working host exits", async () => {
    const { transport, ports, port } = setup();
    const ok = transport.call("stats.get", undefined);
    port().receive({ jsonrpc: "2.0", id: 1, result: { downloadBps: 0, uploadBps: 0, active: 0, queued: 0 } });
    await ok;
    const inFlight = transport.call("download.list", {});
    port().drop("Native host has exited.");
    await expect(inFlight).rejects.toMatchObject({ kind: "disconnected" });
    expect(transport.healthy).toBe(true);
    void transport.call("stats.get", undefined).catch(() => {});
    expect(ports).toHaveLength(2);
  });

  it("reports status from nm.status, and status always retries", async () => {
    const { transport, port, setLastError, ports } = setup();
    const statuses: unknown[] = [];
    transport.onStatus((s) => statuses.push(s));

    const missing = transport.call("stats.get", undefined);
    setLastError("Specified native messaging host not found.");
    port().drop();
    await missing.catch(() => {});

    // Within the fail-fast window, but an explicit status check tries again.
    const pending = transport.status();
    expect(ports).toHaveLength(2);
    expect(port().last()).toEqual({ jsonrpc: "2.0", id: 2, method: "nm.status" });
    port().receive({ jsonrpc: "2.0", id: 2, result: { daemonRunning: true, daemonVersion: "0.1.0", hostVersion: "0.0.1" } });
    await expect(pending).resolves.toEqual({
      mode: "pro",
      hostAvailable: true,
      daemonRunning: true,
      daemonVersion: "0.1.0",
      hostVersion: "0.0.1",
    });

    port().receive({ jsonrpc: "2.0", method: "event.nm.status", params: { daemonRunning: false, hostVersion: "0.0.1" } });
    expect(statuses.at(-1)).toMatchObject({ hostAvailable: true, daemonRunning: false });
  });

  it("reports a missing host as a status, not an exception", async () => {
    const { transport, port, setLastError } = setup();
    const pending = transport.status();
    setLastError("Specified native messaging host not found.");
    port().drop();
    await expect(pending).resolves.toMatchObject({ mode: "pro", hostAvailable: false, daemonRunning: false });
  });

  it("closes an idle port so the host can exit, and reopens on demand", async () => {
    vi.useFakeTimers();
    try {
      const { transport, ports, port } = setup({ idleMs: 1_000 });
      const call = transport.call("stats.get", undefined);
      port().receive({ jsonrpc: "2.0", id: 1, result: { downloadBps: 0, uploadBps: 0, active: 0, queued: 0 } });
      await call;
      await vi.advanceTimersByTimeAsync(999);
      expect(port().disconnected).toBe(false);
      await vi.advanceTimersByTimeAsync(1);
      expect(port().disconnected).toBe(true);
      void transport.call("stats.get", undefined).catch(() => {});
      expect(ports).toHaveLength(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("times out requests the host never answers", async () => {
    vi.useFakeTimers();
    try {
      const { transport } = setup();
      const pending = transport.call("stats.get", undefined);
      const settled = pending.catch((e: unknown) => e);
      await vi.advanceTimersByTimeAsync(15_000);
      expect(await settled).toMatchObject({ kind: "timeout" });
    } finally {
      vi.useRealTimers();
    }
  });

  it("refuses calls after close()", async () => {
    const { transport } = setup();
    transport.close();
    await expect(transport.call("stats.get", undefined)).rejects.toMatchObject({ kind: "closed" });
    await flush();
  });
});

describe("classifyDisconnect", () => {
  it.each([
    ["Specified native messaging host not found.", "host-missing"],
    ["No such native application com.coderzonebd.whoosh", "host-missing"],
    ["Access to the specified native messaging host is forbidden.", "forbidden"],
    ["This extension does not have permission to use native application com.coderzonebd.whoosh", "forbidden"],
    ["Native host has exited.", "disconnected"],
    [undefined, "disconnected"],
  ])("%s → %s", (message, kind) => {
    expect(classifyDisconnect(message)).toBe(kind);
  });
});
