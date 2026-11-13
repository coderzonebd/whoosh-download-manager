// SPDX-License-Identifier: GPL-3.0-or-later

// Pro mode: JSON-RPC to the daemon through the `whoosh-nmhost` native
// messaging host. The host performs `session.hello` and holds the token; the
// extension only ever sees the relayed requests, responses and events.

import type { EventMap, MethodMap } from "@whoosh/api-client";

import { HOST_NAME } from "../shared/constants";
import { RpcPeer, TransportError, type TransportErrorKind } from "./rpc";
import type { ConnectionStatus, NmStatus, Transport } from "./types";

/** The parts of `runtime.Port` used here (Chromium and Firefox agree on them). */
export interface NativePort {
  postMessage(message: unknown): void;
  disconnect(): void;
  onMessage: { addListener(callback: (message: unknown) => void): void };
  onDisconnect: { addListener(callback: (port: NativePort) => void): void };
  /** Firefox puts the disconnect reason here. */
  error?: { message?: string } | null;
}

export interface NativeOptions {
  connectNative(hostName: string): NativePort;
  /** Chromium reports the disconnect reason through `runtime.lastError`. */
  lastError?: () => string | undefined;
  hostName?: string;
  requestTimeoutMs?: number;
  /** Close the port after this long without traffic, letting the host exit; 0 keeps it open. */
  idleMs?: number;
  /** After the host failed to start, fail at once for this long instead of retrying. */
  failFastMs?: number;
  now?: () => number;
}

/** Maps the browser's disconnect message to a reason. */
export function classifyDisconnect(message: string | undefined): TransportErrorKind {
  const m = (message ?? "").toLowerCase();
  // Chromium: "Specified native messaging host not found."
  // Firefox:  "No such native application com.coderzonebd.whoosh"
  if (m.includes("not found") || m.includes("no such native application")) return "host-missing";
  // Chromium: "Access to the specified native messaging host is forbidden."
  // Firefox:  "This extension does not have permission to use native application …"
  if (m.includes("forbidden") || m.includes("does not have permission")) return "forbidden";
  return "disconnected";
}

function describe(kind: TransportErrorKind, message: string | undefined): string {
  switch (kind) {
    case "host-missing":
      return "The whoosh native host is not installed for this browser (run `whoosh-nmhost --register`).";
    case "forbidden":
      return "The whoosh native host does not allow this extension ID (re-run `whoosh-nmhost --register`).";
    default:
      return message ? `The whoosh native host disconnected: ${message}` : "The whoosh native host disconnected.";
  }
}

export class NativeMessagingTransport implements Transport {
  readonly mode = "pro" as const;

  private readonly opts: Required<Omit<NativeOptions, "lastError">> & Pick<NativeOptions, "lastError">;
  private readonly peer: RpcPeer;
  private port: NativePort | undefined;
  /** Whether the current port has delivered at least one message. */
  private portAnswered = false;
  private lastFailure: { at: number; error: TransportError } | undefined;
  private idleTimer: ReturnType<typeof setTimeout> | undefined;
  private closed = false;
  private readonly statusListeners = new Set<(status: ConnectionStatus) => void>();

  constructor(options: NativeOptions) {
    this.opts = {
      hostName: HOST_NAME,
      requestTimeoutMs: 15_000,
      idleMs: 5 * 60_000,
      failFastMs: 2_000,
      now: () => Date.now(),
      ...options,
    };
    this.peer = new RpcPeer((message) => this.post(message), this.opts.requestTimeoutMs);
    this.peer.onNotification("event.nm.status", (params) => {
      this.emit(this.fromNmStatus(params as NmStatus));
    });
  }

  get healthy(): boolean {
    return this.recentFailure() === undefined;
  }

  call<M extends keyof MethodMap>(method: M, params: MethodMap[M]["params"]): Promise<MethodMap[M]["result"]> {
    return this.request(method, params) as Promise<MethodMap[M]["result"]>;
  }

  /** Events flow only while the port is open (it opens on the first call). */
  on<E extends keyof EventMap>(event: E, handler: (payload: EventMap[E]) => void): () => void {
    return this.peer.onNotification(event, handler as (params: unknown) => void);
  }

  onStatus(handler: (status: ConnectionStatus) => void): () => void {
    this.statusListeners.add(handler);
    return () => this.statusListeners.delete(handler);
  }

  async status(): Promise<ConnectionStatus> {
    // An explicit status check always retries, even right after a failure.
    this.lastFailure = undefined;
    try {
      const result = (await this.request("nm.status", undefined, 5_000)) as NmStatus;
      const status = this.fromNmStatus(result);
      this.emit(status);
      return status;
    } catch (e) {
      const status = this.unavailable(e);
      this.emit(status);
      return status;
    }
  }

  close(): void {
    this.closed = true;
    this.closePort(new TransportError("closed", "the transport was closed"));
  }

  // ---------------------------------------------------------------- internals

  private async request(method: string, params: unknown, timeoutMs?: number): Promise<unknown> {
    if (this.closed) throw new TransportError("closed", "the transport was closed");
    this.ensurePort();
    this.touch();
    try {
      return await this.peer.request(method, params, timeoutMs);
    } finally {
      this.touch();
    }
  }

  private recentFailure(): TransportError | undefined {
    const failure = this.lastFailure;
    if (!failure) return undefined;
    if (this.opts.now() - failure.at >= this.opts.failFastMs) return undefined;
    return failure.error;
  }

  private ensurePort(): void {
    if (this.port) return;
    const failure = this.recentFailure();
    if (failure) throw failure;

    let port: NativePort;
    try {
      port = this.opts.connectNative(this.opts.hostName);
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      const kind = classifyDisconnect(message);
      const error = new TransportError(kind, describe(kind, message));
      this.lastFailure = { at: this.opts.now(), error };
      throw error;
    }
    this.port = port;
    this.portAnswered = false;
    port.onMessage.addListener((message) => {
      if (this.port !== port) return;
      this.portAnswered = true;
      this.lastFailure = undefined;
      this.peer.handleIncoming(message);
    });
    port.onDisconnect.addListener((p) => this.onDisconnect(port, p));
  }

  private onDisconnect(port: NativePort, reported: NativePort): void {
    // Read lastError unconditionally so Chromium does not log it as unchecked.
    const message = reported.error?.message ?? port.error?.message ?? this.opts.lastError?.();
    if (this.port !== port) return;
    this.port = undefined;
    this.clearIdle();
    const kind = classifyDisconnect(message);
    const error = new TransportError(kind, describe(kind, message));
    if (!this.portAnswered || kind !== "disconnected") {
      this.lastFailure = { at: this.opts.now(), error };
    }
    this.peer.failAll(error);
    this.emit({ mode: "pro", hostAvailable: false, daemonRunning: false, error: error.message });
  }

  private post(message: object): void {
    if (!this.port) throw new TransportError("disconnected", "the native host is not connected");
    this.port.postMessage(message);
  }

  private touch(): void {
    if (this.opts.idleMs <= 0) return;
    this.clearIdle();
    this.idleTimer = setTimeout(() => {
      if (this.peer.pendingCount > 0) {
        this.touch();
      } else {
        this.closePort(new TransportError("closed", "closed after being idle"));
      }
    }, this.opts.idleMs);
  }

  private clearIdle(): void {
    if (this.idleTimer !== undefined) clearTimeout(this.idleTimer);
    this.idleTimer = undefined;
  }

  private closePort(reason: TransportError): void {
    this.clearIdle();
    const port = this.port;
    this.port = undefined;
    if (port) {
      try {
        port.disconnect();
      } catch {
        // already gone
      }
    }
    this.peer.failAll(reason);
  }

  private fromNmStatus(s: NmStatus): ConnectionStatus {
    const status: ConnectionStatus = {
      mode: "pro",
      hostAvailable: true,
      daemonRunning: s.daemonRunning === true,
    };
    if (typeof s.hostVersion === "string") status.hostVersion = s.hostVersion;
    if (s.daemonRunning && typeof s.daemonVersion === "string") status.daemonVersion = s.daemonVersion;
    if (!status.daemonRunning) status.error = "The whoosh app is not running.";
    return status;
  }

  private unavailable(error: unknown): ConnectionStatus {
    return {
      mode: "pro",
      hostAvailable: false,
      daemonRunning: false,
      error: error instanceof Error ? error.message : String(error),
    };
  }

  private emit(status: ConnectionStatus): void {
    for (const listener of this.statusListeners) {
      try {
        listener(status);
      } catch (e) {
        console.error("whoosh: status listener failed", e);
      }
    }
  }
}
