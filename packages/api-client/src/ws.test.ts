// SPDX-License-Identifier: Apache-2.0
// Runs the real WebSocket client (Node's global WebSocket) against a tiny
// fake daemon built on the `ws` package.
import { createHmac } from "node:crypto";
import type { AddressInfo } from "node:net";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { WebSocketServer, type WebSocket as ServerSocket } from "ws";
import { verifyHelloProof } from "./auth";
import { WhooshConnectionError, WhooshRpcError } from "./errors";
import { ErrorCode } from "./types";
import { connectWs, type ConnectionStatus, type WsClientOptions, type WsWhooshClient } from "./ws";

interface Req {
  jsonrpc: "2.0";
  id?: number;
  method: string;
  params?: unknown;
}

type Handler = (req: Req, sock: ServerSocket) => unknown;

/** The daemon's side of PROTOCOL.md §1 authentication, computed independently with node:crypto. */
const proofFor = (token: string, nonce: unknown) =>
  createHmac("sha256", token).update(`whoosh-hello-proof-v1\n${String(nonce)}`).digest("hex");

class FakeDaemon {
  readonly wss: WebSocketServer;
  readonly received: Req[] = [];
  readonly sockets: ServerSocket[] = [];
  token = "secret";
  /** Accept any token and answer with a proof made from the wrong key (a port squatter). */
  impostor = false;
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
          const p = req.params as { token?: string; nonce?: string; protocol?: number } | undefined;
          if (req.method !== "session.hello" || (!this.impostor && p?.token !== this.token)) return sock.close(4401, "unauthorized");
          if (p?.protocol !== 1) return sock.close(4426, "protocol");
          authed = true;
          const proof = proofFor(this.impostor ? "not-the-token" : this.token, p.nonce);
          return this.reply(sock, req.id, { daemonVersion: "test", protocol: 1, capabilities: ["http"], proof });
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
let client: WsWhooshClient | undefined;

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

function connect(extra: Partial<WsClientOptions> = {}): WsWhooshClient {
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

  it("maps JSON-RPC errors to WhooshRpcError with the daemon's code", async () => {
    daemon.handlers.set("download.resume", () => {
      throw { code: ErrorCode.InvalidState, message: "already complete" };
    });
    const c = connect();
    const err = await c.call("download.resume", { ids: ["x"] }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(WhooshRpcError);
    expect((err as WhooshRpcError).isInvalidState).toBe(true);
    expect((err as WhooshRpcError).method).toBe("download.resume");
    expect((err as WhooshRpcError).message).toBe("already complete");
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
    expect(err).toBeInstanceOf(WhooshConnectionError);
    expect((err as WhooshConnectionError).reason).toBe("connection_lost");

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
        const nonce = (r.params as { nonce?: string } | undefined)?.nonce;
        s.send(JSON.stringify({ jsonrpc: "2.0", id: r.id, result: r.method === "session.hello" ? { daemonVersion: "2", protocol: 1, capabilities: [], proof: proofFor("secret", nonce) } : { downloadBps: 0, uploadBps: 0, active: 9, queued: 0 } }));
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

  it("sends a fresh nonce with every hello", async () => {
    const c = connect();
    await c.ready();
    daemon.dropAll();
    await until(() => daemon.received.filter((r) => r.method === "session.hello").length === 2);
    const nonces = daemon.received
      .filter((r) => r.method === "session.hello")
      .map((r) => (r.params as { nonce: string }).nonce);
    for (const n of nonces) expect(n).toMatch(/^[0-9a-f]{32}$/);
    expect(nonces[0]).not.toBe(nonces[1]);
  });

  it("sends nothing to a process that cannot prove it is the daemon", async () => {
    daemon.impostor = true;
    const c = connect({ token: "stale-token" });
    const pending = c.call("download.add", { url: "https://example.com/", request: { cookie: "session=SECRET" } });
    pending.catch(() => {});
    await until(() => daemon.received.filter((r) => r.method === "session.hello").length >= 2);
    expect(daemon.received.every((r) => r.method === "session.hello")).toBe(true);
    expect(c.status.state).not.toBe("open");
    c.close();
    await expect(pending).rejects.toBeInstanceOf(WhooshConnectionError);
  });

  it("does not act on notifications before the proof is checked", async () => {
    daemon.impostor = true;
    const seen: unknown[] = [];
    const c = connect();
    c.on("event.stats", (s) => seen.push(s));
    await until(() => daemon.received.length >= 1);
    daemon.notify("event.stats", { downloadBps: 1, uploadBps: 0, active: 0, queued: 0 });
    await new Promise((r) => setTimeout(r, 50));
    expect(seen).toEqual([]);
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
    await expect(c.call("stats.get", undefined)).rejects.toBeInstanceOf(WhooshConnectionError);
  });

  it("gives up after maxAttempts when the daemon is unreachable", async () => {
    const url = daemon.url;
    await daemon.close();
    const c = connect({ url, reconnect: { initialDelayMs: 5, maxAttempts: 2, jitter: 0 } });
    await expect(c.ready()).rejects.toMatchObject({ reason: "gave_up" });
  });

  it("refuses to send session.hello manually", async () => {
    const c = connect();
    await expect(c.call("session.hello", { token: "", nonce: "", protocol: 1, client: { name: "", version: "" } })).rejects.toThrow(/sent by the client/);
  });
});

describe("hello proof", () => {
  // The same vector as crates/proto/src/auth.rs.
  const token = "test-token";
  const nonce = "00112233445566778899aabbccddeeff";
  const proof = "1805e28c9c7c1323eea89e2da42f497108e2f9b8af12eee4039c6d96164cbdcb";

  it("accepts the shared test vector", async () => {
    expect(proofFor(token, nonce)).toBe(proof);
    expect(await verifyHelloProof(token, nonce, proof)).toBe(true);
  });

  it("rejects a wrong token, nonce or malformed proof", async () => {
    expect(await verifyHelloProof("other", nonce, proof)).toBe(false);
    expect(await verifyHelloProof(token, "0".repeat(32), proof)).toBe(false);
    expect(await verifyHelloProof(token, nonce, proof.toUpperCase())).toBe(false);
    expect(await verifyHelloProof(token, nonce, proof.slice(2))).toBe(false);
    expect(await verifyHelloProof(token, nonce, undefined)).toBe(false);
  });
});
