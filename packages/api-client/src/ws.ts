// SPDX-License-Identifier: Apache-2.0
//
// VayuClient over JSON-RPC 2.0 / WebSocket, per docs/api/PROTOCOL.md:
// - `session.hello` is always the first frame on every (re)connection;
// - requests are correlated by numeric id; responses may arrive in any order;
// - notifications (`event.*`) are dispatched to `on()` handlers;
// - calls made while (re)connecting are queued and flushed after hello;
// - a dropped socket rejects in-flight calls with `connection_lost` (they may
//   or may not have been applied) and reconnects with exponential backoff;
// - close codes 4401 (bad token) and 4426 (protocol) are final: no retry.
//
// `url` and `token` may be functions so a client can re-read the daemon's
// runtime file on every attempt (the daemon may restart on another port).

import { CloseCode, VayuConnectionError, VayuRpcError, type ConnectionErrorReason } from "./errors";
import {
  PROTOCOL_VERSION,
  type ClientInfo,
  type EventMap,
  type HelloResult,
  type MethodMap,
  type VayuClient,
} from "./types";

/** The subset of the WHATWG WebSocket API this client needs (browser, Node ≥ 22, Tauri webview). */
export interface WebSocketLike {
  readonly readyState: number;
  send(data: string): void;
  close(code?: number, reason?: string): void;
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onclose: ((ev: { code: number; reason: string }) => void) | null;
  onerror: ((ev: unknown) => void) | null;
}
export type WebSocketFactory = (url: string) => WebSocketLike;

export interface ReconnectOptions {
  /** First retry delay (default 250 ms). */
  initialDelayMs?: number;
  /** Cap on the delay (default 10 s). */
  maxDelayMs?: number;
  /** Multiplier per failed attempt (default 2). */
  factor?: number;
  /** ± fraction of random spread so many clients don't retry in lockstep (default 0.2). */
  jitter?: number;
  /** Give up after this many consecutive failed attempts (default: never). */
  maxAttempts?: number;
}

type Resolvable = string | (() => string | Promise<string>);

export interface WsClientOptions {
  /** e.g. `ws://127.0.0.1:47210/rpc`. */
  url: Resolvable;
  /** The token from the daemon's runtime file. */
  token: Resolvable;
  client: ClientInfo;
  /** Defaults to PROTOCOL_VERSION. */
  protocol?: number;
  /** Reject a call that has no answer after this long (default 30 s; 0 disables). */
  requestTimeoutMs?: number;
  /** `false` disables reconnecting. */
  reconnect?: ReconnectOptions | false;
  /** Defaults to `new globalThis.WebSocket(url)`. */
  createSocket?: WebSocketFactory;
  /** Called on every connection state change. Same as `onStatus()`. */
  onStatus?: (status: ConnectionStatus) => void;
}

export type ConnectionStatus =
  | { state: "connecting"; attempt: number }
  | { state: "open"; hello: HelloResult }
  | { state: "reconnecting"; attempt: number; delayMs: number; reason?: string }
  | { state: "closed"; reason: Extract<ConnectionErrorReason, "closed" | "unauthorized" | "protocol_mismatch" | "gave_up"> };

interface Pending {
  id: number;
  method: string;
  frame: string;
  resolve: (v: unknown) => void;
  reject: (e: unknown) => void;
  timer: ReturnType<typeof setTimeout> | undefined;
}

const OPEN = 1;

export class WsVayuClient implements VayuClient {
  readonly #opts: WsClientOptions;
  readonly #protocol: number;
  readonly #handlers = new Map<string, Set<(p: unknown) => void>>();
  readonly #statusHandlers = new Set<(s: ConnectionStatus) => void>();
  /** Sent, awaiting a response. */
  readonly #inflight = new Map<number, Pending>();
  /** Waiting for the connection (and hello) before being sent. */
  #outbox: Pending[] = [];
  #socket: WebSocketLike | undefined;
  #nextId = 1;
  #attempt = 0;
  #reconnectTimer: ReturnType<typeof setTimeout> | undefined;
  #status: ConnectionStatus = { state: "connecting", attempt: 0 };
  #final = false;
  #helloId = -1;

  constructor(opts: WsClientOptions) {
    this.#opts = opts;
    this.#protocol = opts.protocol ?? PROTOCOL_VERSION;
    if (opts.onStatus) this.#statusHandlers.add(opts.onStatus);
    void this.#connect();
  }

  get status(): ConnectionStatus {
    return this.#status;
  }

  /** Subscribe to connection state changes; the handler gets the current state immediately. */
  onStatus(handler: (s: ConnectionStatus) => void): () => void {
    this.#statusHandlers.add(handler);
    handler(this.#status);
    return () => this.#statusHandlers.delete(handler);
  }

  /** Resolves with the hello result once the client is (re)connected. */
  ready(): Promise<HelloResult> {
    const s = this.#status;
    if (s.state === "open") return Promise.resolve(s.hello);
    if (s.state === "closed") return Promise.reject(new VayuConnectionError(s.reason));
    return new Promise((resolve, reject) => {
      const off = this.onStatus((st) => {
        if (st.state === "open") {
          queueMicrotask(off);
          resolve(st.hello);
        } else if (st.state === "closed") {
          queueMicrotask(off);
          reject(new VayuConnectionError(st.reason));
        }
      });
    });
  }

  call<M extends keyof MethodMap>(method: M, params: MethodMap[M]["params"]): Promise<MethodMap[M]["result"]> {
    if (this.#final) {
      const reason = this.#status.state === "closed" ? this.#status.reason : "closed";
      return Promise.reject(new VayuConnectionError(reason));
    }
    if (method === "session.hello") {
      return Promise.reject(new Error("session.hello is sent by the client itself on every connection"));
    }
    return new Promise((resolve, reject) => {
      const id = this.#nextId++;
      const msg: Record<string, unknown> = { jsonrpc: "2.0", id, method };
      if (params !== undefined) msg.params = params;
      const p: Pending = { id, method, frame: JSON.stringify(msg), resolve: resolve as (v: unknown) => void, reject, timer: undefined };
      const timeout = this.#opts.requestTimeoutMs ?? 30_000;
      if (timeout > 0) {
        p.timer = setTimeout(() => {
          this.#inflight.delete(id);
          this.#outbox = this.#outbox.filter((q) => q !== p);
          reject(new VayuConnectionError("timeout", `${method} timed out after ${timeout} ms`));
        }, timeout);
      }
      if (this.#status.state === "open" && this.#socket?.readyState === OPEN) this.#send(p);
      else this.#outbox.push(p);
    });
  }

  on<E extends keyof EventMap>(event: E, handler: (payload: EventMap[E]) => void): () => void {
    let set = this.#handlers.get(event);
    if (!set) this.#handlers.set(event, (set = new Set()));
    const h = handler as (p: unknown) => void;
    set.add(h);
    return () => {
      set.delete(h);
    };
  }

  close(): void {
    if (this.#final && this.#status.state === "closed") return;
    this.#shutdown("closed");
    this.#handlers.clear();
  }

  // ---------------------------------------------------------------- internals

  #setStatus(s: ConnectionStatus): void {
    this.#status = s;
    for (const h of [...this.#statusHandlers]) {
      try {
        h(s);
      } catch (err) {
        console.error("[vayu ws] status handler threw", err);
      }
    }
  }

  async #connect(): Promise<void> {
    if (this.#final) return;
    this.#setStatus({ state: "connecting", attempt: this.#attempt });
    let socket: WebSocketLike;
    let token: string;
    try {
      const url = typeof this.#opts.url === "function" ? await this.#opts.url() : this.#opts.url;
      token = typeof this.#opts.token === "function" ? await this.#opts.token() : this.#opts.token;
      if (this.#final) return;
      const create = this.#opts.createSocket ?? ((u: string) => new globalThis.WebSocket(u) as unknown as WebSocketLike);
      socket = create(url);
    } catch (err) {
      this.#scheduleReconnect(err instanceof Error ? err.message : String(err));
      return;
    }
    this.#socket = socket;

    socket.onopen = () => {
      if (socket !== this.#socket) return;
      this.#helloId = this.#nextId++;
      socket.send(
        JSON.stringify({
          jsonrpc: "2.0",
          id: this.#helloId,
          method: "session.hello",
          params: { token, protocol: this.#protocol, client: this.#opts.client },
        }),
      );
    };
    socket.onmessage = (ev) => {
      if (socket !== this.#socket) return;
      if (typeof ev.data !== "string") return; // protocol is text frames only
      let msg: unknown;
      try {
        msg = JSON.parse(ev.data);
      } catch {
        console.warn("[vayu ws] dropping a frame that is not JSON");
        return;
      }
      for (const m of Array.isArray(msg) ? msg : [msg]) this.#handleMessage(m);
    };
    socket.onerror = () => {
      // A close event always follows; reconnect logic lives there.
    };
    socket.onclose = (ev) => {
      if (socket !== this.#socket) return;
      this.#socket = undefined;
      if (this.#final) return;
      if (ev.code === CloseCode.Unauthorized) return this.#shutdown("unauthorized");
      if (ev.code === CloseCode.ProtocolMismatch) return this.#shutdown("protocol_mismatch");
      this.#failInflight();
      this.#scheduleReconnect(ev.reason || `closed with code ${ev.code}`);
    };
  }

  #handleMessage(m: unknown): void {
    if (!m || typeof m !== "object") return;
    const msg = m as { id?: unknown; method?: unknown; params?: unknown; result?: unknown; error?: unknown };

    if (typeof msg.method === "string" && msg.id === undefined) {
      const set = this.#handlers.get(msg.method);
      if (!set) return;
      for (const h of [...set]) {
        try {
          h(msg.params);
        } catch (err) {
          console.error(`[vayu ws] ${msg.method} handler threw`, err);
        }
      }
      return;
    }

    if (typeof msg.id !== "number") return;
    const error = msg.error as { code?: unknown; message?: unknown; data?: unknown } | undefined;

    if (msg.id === this.#helloId) {
      this.#helloId = -1;
      if (error) {
        const code = typeof error.code === "number" ? error.code : 0;
        this.#shutdown(code === -32001 ? "unauthorized" : "protocol_mismatch");
        return;
      }
      const hello = msg.result as HelloResult;
      if (!hello || hello.protocol !== this.#protocol) {
        this.#shutdown("protocol_mismatch");
        return;
      }
      this.#attempt = 0;
      this.#setStatus({ state: "open", hello });
      const queued = this.#outbox;
      this.#outbox = [];
      for (const p of queued) this.#send(p);
      return;
    }

    const p = this.#inflight.get(msg.id);
    if (!p) return;
    this.#inflight.delete(msg.id);
    if (p.timer) clearTimeout(p.timer);
    if (error) {
      p.reject(
        new VayuRpcError(
          typeof error.code === "number" ? error.code : -32603,
          typeof error.message === "string" ? error.message : "unknown error",
          error.data,
          p.method,
        ),
      );
    } else {
      p.resolve(msg.result === undefined ? null : msg.result);
    }
  }

  #send(p: Pending): void {
    const socket = this.#socket;
    if (!socket || socket.readyState !== OPEN) {
      this.#outbox.push(p);
      return;
    }
    this.#inflight.set(p.id, p);
    try {
      socket.send(p.frame);
    } catch {
      this.#inflight.delete(p.id);
      this.#outbox.push(p);
    }
  }

  #failInflight(): void {
    for (const p of this.#inflight.values()) {
      if (p.timer) clearTimeout(p.timer);
      p.reject(new VayuConnectionError("connection_lost", `connection lost during ${p.method}`));
    }
    this.#inflight.clear();
  }

  #scheduleReconnect(reason?: string): void {
    const opts = this.#opts.reconnect;
    if (opts === false) return this.#shutdown("gave_up");
    const max = opts?.maxAttempts;
    this.#attempt++;
    if (max !== undefined && this.#attempt > max) return this.#shutdown("gave_up");
    const initial = opts?.initialDelayMs ?? 250;
    const cap = opts?.maxDelayMs ?? 10_000;
    const factor = opts?.factor ?? 2;
    const jitter = opts?.jitter ?? 0.2;
    const base = Math.min(cap, initial * factor ** (this.#attempt - 1));
    const delayMs = Math.max(0, Math.round(base * (1 + (Math.random() * 2 - 1) * jitter)));
    const status: ConnectionStatus = { state: "reconnecting", attempt: this.#attempt, delayMs };
    if (reason !== undefined) status.reason = reason;
    this.#setStatus(status);
    this.#reconnectTimer = setTimeout(() => {
      this.#reconnectTimer = undefined;
      void this.#connect();
    }, delayMs);
  }

  #shutdown(reason: Extract<ConnectionStatus, { state: "closed" }>["reason"]): void {
    this.#final = true;
    if (this.#reconnectTimer) clearTimeout(this.#reconnectTimer);
    this.#reconnectTimer = undefined;
    const socket = this.#socket;
    this.#socket = undefined;
    if (socket && socket.readyState <= OPEN) {
      try {
        socket.close(CloseCode.Normal);
      } catch {
        // already closing
      }
    }
    const err = (p: Pending) => new VayuConnectionError(reason, `${p.method}: ${reason}`);
    for (const p of [...this.#inflight.values(), ...this.#outbox]) {
      if (p.timer) clearTimeout(p.timer);
      p.reject(err(p));
    }
    this.#inflight.clear();
    this.#outbox = [];
    this.#setStatus({ state: "closed", reason });
  }
}

/** Create a client and start connecting. Calls made before the hello completes are queued. */
export function connectWs(opts: WsClientOptions): WsVayuClient {
  return new WsVayuClient(opts);
}
