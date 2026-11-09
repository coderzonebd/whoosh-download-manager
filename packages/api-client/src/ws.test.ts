// SPDX-License-Identifier: Apache-2.0
// Runs the real WebSocket client (Node's global WebSocket) against a tiny
// fake daemon built on the `ws` package.
import type { AddressInfo } from "node:net";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { WebSocketServer, type WebSocket as ServerSocket } from "ws";
import { VayuConnectionError, VayuRpcError } from "./errors";
import { ErrorCode } from "./types";
import { connectWs, type ConnectionStatus, type WsClientOptions, type WsVayuClient } from "./ws";

interface Req {
  jsonrpc: "2.0";
  id?: number;
  method: string;
  params?: unknown;
}

type Handler = (req: Req, sock: ServerSocket) => unknown;

class FakeDaemon {
  readonly wss: WebSocketServer;
  readonly received: Req[] = [];
  readonly sockets: ServerSocket[] = [];
  token = "secret";
  /** Respond to a method; return undefined to stay silent. Throw {code,message} for an error. */
  handlers = new Map<string, Handler>();

  constructor() {
    this.wss = new WebSocketServer({ port: 0, host: "127.0.0.1" });
    this.wss.on("connection", (sock) => {
      this.sockets.push(sock);
      let authed = false;
      sock.on("message", (raw) => {
        const req = JSON.parse(String(raw)) as Req;
        this.received.push(req);
        if (!authed) {
          const p = req.params as { token?: string; protocol?: number } | undefined;
          if (req.method !== "session.hello" || p?.token !== this.token) return sock.close(4401, "unauthorized");
          if (p.protocol !== 1) return sock.close(4426, "protocol");
          authed = true;
          return this.reply(sock, req.id, { daemonVersion: "test", protocol: 1, capabilities: ["http"] });
        }
        const h = this.handlers.get(req.method);
        if (!h) return this.error(sock, req.id, -32601, "method not found");
        try {
          const result = h(req, sock);
          if (result !== undefined) this.reply(sock, req.id, result);
        } catch (e) {
          const { code, message } = e as { code: number; message: string };
          this.error(sock, req.id, code, message);
        }
      });
    });
  }

  get url(): string {
    return `ws://127.0.0.1:${(this.wss.address() as AddressInfo).port}/rpc`;
  }

  reply(sock: ServerSocket, id: number | undefined, result: unknown): void {
    sock.send(JSON.stringify({ jsonrpc: "2.0", id, result }));
  }
  error(sock: ServerSocket, id: number | undefined, code: number, message: string): void {
    sock.send(JSON.stringify({ jsonrpc: "2.0", id, error: { code, message } }));
  }
  notify(method: string, params: unknown): void {
    for (const s of this.sockets) if (s.readyState === s.OPEN) s.send(JSON.stringify({ jsonrpc: "2.0", method, params }));
  }
  dropAll(): void {
    for (const s of this.sockets) s.terminate();
  }
  close(): Promise<void> {
    for (const s of this.sockets) s.terminate();
    return new Promise((r) => this.wss.close(() => r()));
  }
}

let daemon: FakeDaemon;
let client: WsVayuClient | undefined;

beforeEach(async () => {
  daemon = new FakeDaemon();
  await new Promise((r) => daemon.wss.once("listening", r));
  daemon.handlers.set("stats.get", () => ({ downloadBps: 1, uploadBps: 0, active: 1, queued: 0 }));
});
afterEach(async () => {
  client?.close();
  client = undefined;
  await daemon.close();
});

function connect(extra: Partial<WsClientOptions> = {}): WsVayuClient {
  client = connectWs({
    url: extra.url ?? daemon.url,
    token: daemon.token,
    client: { name: "test", version: "0.0.1" },
    reconnect: { initialDelayMs: 10, maxDelayMs: 40, jitter: 0 },
    ...extra,
  });
  return client;
}

const until = async (cond: () => boolean, ms = 2000) => {
  const t = Date.now();
  while (!cond()) {
    if (Date.now() - t > ms) throw new Error("timed out waiting for condition");
    await new Promise((r) => setTimeout(r, 5));
  }
};

describe("WebSocket client", () => {
  it("sends session.hello first, with token, protocol and client info", async () => {
    const c = connect();
    const hello = await c.ready();
    expect(hello.daemonVersion).toBe("test");
    expect(daemon.received[0]).toMatchObject({
      method: "session.hello",
      params: { token: "secret", protocol: 1, client: { name: "test", version: "0.0.1" } },
    });
  });

  it("queues calls made before the hello completes and sends them after it", async () => {
    const c = connect();
    const stats = await c.call("stats.get", undefined);
    expect(stats.active).toBe(1);
    expect(daemon.received.map((r) => r.method)).toEqual(["session.hello", "stats.get"]);
    expect(daemon.received[1]).not.toHaveProperty("params");
  });

  it("correlates responses that arrive out of order", async () => {
    const parked: [Req, ServerSocket][] = [];
    daemon.handlers.set("download.get", (req, sock) => {
      parked.push([req, sock]);
      if (parked.length === 2) {
        for (const [r, s] of [...parked].reverse()) {
          daemon.reply(s, r.id, { id: (r.params as { id: string }).id });
        }
      }
      return undefined;
    });
    const c = connect();
    const [a, b] = await Promise.all([
      c.call("download.get", { id: "a" }),
      c.call("download.get", { id: "b" }),
    ]);
    expect(a.id).toBe("a");
    expect(b.id).toBe("b");
  });

  it("maps JSON-RPC errors to VayuRpcError with the daemon's code", async () => {
    daemon.handlers.set("download.resume", () => {
      throw { code: ErrorCode.InvalidState, message: "already complete" };
    });
    const c = connect();
    const err = await c.call("download.resume", { ids: ["x"] }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(VayuRpcError);
    expect((err as VayuRpcError).isInvalidState).toBe(true);
    expect((err as VayuRpcError).method).toBe("download.resume");
    expect((err as VayuRpcError).message).toBe("already complete");
  });

  it("dispatches events to subscribers and stops after unsubscribe", async () => {
    const c = connect();
    const seen: number[] = [];
    const off = c.on("event.stats", (s) => seen.push(s.downloadBps));
    await c.ready();
    daemon.notify("event.stats", { downloadBps: 42, uploadBps: 0, active: 0, queued: 0 });
    await until(() => seen.length === 1);
    off();
    daemon.notify("event.stats", { downloadBps: 43, uploadBps: 0, active: 0, queued: 0 });
    await c.call("stats.get", undefined); // round-trip so the second notification has arrived
    expect(seen).toEqual([42]);
  });

  it("rejects in-flight calls on disconnect, then reconnects with a fresh hello", async () => {
    daemon.handlers.set("download.probe", () => undefined); // never answers
    const statuses: ConnectionStatus["state"][] = [];
    let tokenReads = 0;
    const c = connect({
      token: () => {
        tokenReads++;
        return daemon.token;
      },
      onStatus: (s) => statuses.push(s.state),
    });
    await c.ready();
    const inflight = c.call("download.probe", { url: "https://example.com/x" });
    await until(() => daemon.received.some((r) => r.method === "download.probe"));
    daemon.dropAll();
    const err = await inflight.catch((e: unknown) => e);
    expect(err).toBeInstanceOf(VayuConnectionError);
    expect((err as VayuConnectionError).reason).toBe("connection_lost");

    await until(() => c.status.state === "open");
    expect(tokenReads).toBe(2); // re-read on reconnect
    expect(statuses).toContain("reconnecting");
    expect(daemon.received.filter((r) => r.method === "session.hello")).toHaveLength(2);
    expect((await c.call("stats.get", undefined)).active).toBe(1);
  });

  it("keeps retrying with backoff while the daemon is down", async () => {
    const delays: number[] = [];
    const c = connect({
      onStatus: (s) => {
        if (s.state === "reconnecting") delays.push(s.delayMs);
      },
    });
    await c.ready();
    const url = daemon.url;
    await daemon.close();
    await until(() => delays.length >= 3);
    expect(delays.slice(0, 3)).toEqual([10, 20, 40]);
    // Daemon comes back on the same port.
    const port = Number(new URL(url).port);
    const back = new WebSocketServer({ port, host: "127.0.0.1" });
    back.on("connection", (s) =>
      s.on("message", (raw) => {
        const r = JSON.parse(String(raw)) as Req;
        s.send(JSON.stringify({ jsonrpc: "2.0", id: r.id, result: r.method === "session.hello" ? { daemonVersion: "2", protocol: 1, capabilities: [] } : { downloadBps: 0, uploadBps: 0, active: 9, queued: 0 } }));
      }),
    );
    try {
      await until(() => c.status.state === "open", 3000);
      expect((await c.call("stats.get", undefined)).active).toBe(9);
    } finally {
      c.close();
      for (const s of back.clients) s.terminate();
      await new Promise((r) => back.close(() => r(undefined)));
    }
  });

  it("does not reconnect after the daemon rejects the token (4401)", async () => {
    daemon.token = "different";
    const c = connect({ token: "wrong" });
    const pending = c.call("stats.get", undefined);
    await expect(c.ready()).rejects.toMatchObject({ reason: "unauthorized" });
    await expect(pending).rejects.toMatchObject({ reason: "unauthorized" });
    await new Promise((r) => setTimeout(r, 60));
    expect(c.status).toEqual({ state: "closed", reason: "unauthorized" });
    expect(daemon.received.filter((r) => r.method === "session.hello")).toHaveLength(1);
    await expect(c.call("stats.get", undefined)).rejects.toMatchObject({ reason: "unauthorized" });
  });

  it("stops on a protocol mismatch (4426)", async () => {
    const c = connect({ protocol: 2 });
    await expect(c.ready()).rejects.toMatchObject({ reason: "protocol_mismatch" });
  });

  it("times out calls that get no answer", async () => {
    daemon.handlers.set("download.probe", () => undefined);
    const c = connect({ requestTimeoutMs: 50 });
    await expect(c.call("download.probe", { url: "https://example.com" })).rejects.toMatchObject({ reason: "timeout" });
  });

  it("close() rejects pending calls and stops reconnecting", async () => {
    daemon.handlers.set("download.probe", () => undefined);
    const c = connect();
    await c.ready();
    const p = c.call("download.probe", { url: "https://example.com" });
    await until(() => daemon.received.some((r) => r.method === "download.probe"));
    c.close();
    await expect(p).rejects.toMatchObject({ reason: "closed" });
    expect(c.status).toEqual({ state: "closed", reason: "closed" });
    await expect(c.call("stats.get", undefined)).rejects.toBeInstanceOf(VayuConnectionError);
  });

  it("gives up after maxAttempts when the daemon is unreachable", async () => {
    const url = daemon.url;
    await daemon.close();
    const c = connect({ url, reconnect: { initialDelayMs: 5, maxAttempts: 2, jitter: 0 } });
    await expect(c.ready()).rejects.toMatchObject({ reason: "gave_up" });
  });

  it("refuses to send session.hello manually", async () => {
    const c = connect();
    await expect(c.call("session.hello", { token: "", protocol: 1, client: { name: "", version: "" } })).rejects.toThrow(/sent by the client/);
  });
});
