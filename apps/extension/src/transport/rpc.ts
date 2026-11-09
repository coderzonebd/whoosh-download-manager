// SPDX-License-Identifier: GPL-3.0-or-later

// Transport-independent JSON-RPC 2.0 bookkeeping: ids, pending requests,
// timeouts, notifications.

import { ErrorCode } from "@vayu/api-client";

export type JsonRpcId = number | string;

export interface JsonRpcErrorObject {
  code: number;
  message: string;
  data?: unknown;
}

/** Codes the native host adds (PROTOCOL.md §5). */
export const HostErrorCode = {
  DaemonUnavailable: ErrorCode.DaemonUnavailable,
  MessageTooLarge: ErrorCode.MessageTooLarge,
} as const;

/** An error response from the daemon or the host. */
export class RpcError extends Error {
  readonly code: number;
  readonly data: unknown;

  constructor(error: JsonRpcErrorObject) {
    super(error.message);
    this.name = "RpcError";
    this.code = error.code;
    this.data = error.data;
  }
}

export type TransportErrorKind =
  /** The native host is not installed / not registered for this browser. */
  | "host-missing"
  /** The host exists but refused this extension (ID not in its manifest). */
  | "forbidden"
  /** The port closed (host crashed, exited, or the browser dropped it). */
  | "disconnected"
  /** No answer in time. */
  | "timeout"
  /** The transport was closed by us. */
  | "closed";

/** The request never reached the daemon, or its answer never came back. */
export class TransportError extends Error {
  readonly kind: TransportErrorKind;

  constructor(kind: TransportErrorKind, message: string) {
    super(message);
    this.name = "TransportError";
    this.kind = kind;
  }
}

/** True when the failure means "no engine to talk to" rather than "the engine said no". */
export function isUnavailable(error: unknown): boolean {
  return (
    error instanceof TransportError ||
    (error instanceof RpcError && error.code === HostErrorCode.DaemonUnavailable)
  );
}

interface Pending {
  resolve(value: unknown): void;
  reject(error: Error): void;
  timer: ReturnType<typeof setTimeout>;
}

export class RpcPeer {
  private nextId = 1;
  private readonly pending = new Map<JsonRpcId, Pending>();
  private readonly listeners = new Map<string, Set<(params: unknown) => void>>();

  constructor(
    private readonly send: (message: object) => void,
    private readonly defaultTimeoutMs = 15_000,
  ) {}

  get pendingCount(): number {
    return this.pending.size;
  }

  request(method: string, params: unknown, timeoutMs = this.defaultTimeoutMs): Promise<unknown> {
    const id = this.nextId++;
    const message: Record<string, unknown> = { jsonrpc: "2.0", id, method };
    if (params !== undefined) message["params"] = params;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new TransportError("timeout", `${method}: no answer within ${timeoutMs} ms`));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      try {
        this.send(message);
      } catch (e) {
        clearTimeout(timer);
        this.pending.delete(id);
        reject(e instanceof Error ? e : new TransportError("disconnected", String(e)));
      }
    });
  }

  onNotification(method: string, handler: (params: unknown) => void): () => void {
    let set = this.listeners.get(method);
    if (!set) {
      set = new Set();
      this.listeners.set(method, set);
    }
    set.add(handler);
    return () => set.delete(handler);
  }

  /** Feeds one incoming message (response or notification). */
  handleIncoming(message: unknown): void {
    if (typeof message !== "object" || message === null) return;
    const m = message as Record<string, unknown>;
    if (typeof m["method"] === "string") {
      for (const handler of this.listeners.get(m["method"]) ?? []) {
        try {
          handler(m["params"]);
        } catch (e) {
          console.error("vayu: notification handler failed", e);
        }
      }
      return;
    }
    const id = m["id"];
    if (typeof id !== "number" && typeof id !== "string") return;
    const entry = this.pending.get(id);
    if (!entry) return;
    this.pending.delete(id);
    clearTimeout(entry.timer);
    const error = m["error"];
    if (typeof error === "object" && error !== null) {
      const e = error as Partial<JsonRpcErrorObject>;
      entry.reject(
        new RpcError({
          code: typeof e.code === "number" ? e.code : -32603,
          message: typeof e.message === "string" ? e.message : "unknown error",
          data: e.data,
        }),
      );
    } else {
      entry.resolve(m["result"] ?? null);
    }
  }

  /** Rejects every outstanding request (the connection is gone). */
  failAll(error: Error): void {
    const entries = [...this.pending.values()];
    this.pending.clear();
    for (const entry of entries) {
      clearTimeout(entry.timer);
      entry.reject(error);
    }
  }
}
