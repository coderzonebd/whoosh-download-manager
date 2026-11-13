// SPDX-License-Identifier: Apache-2.0
// Typed errors shared by every WhooshClient transport (WebSocket, mock, …).

import { ErrorCode } from "./types";

/** Standard JSON-RPC 2.0 error codes (the daemon's own codes are `ErrorCode` in types.ts). */
export const JsonRpcErrorCode = {
  ParseError: -32700,
  InvalidRequest: -32600,
  MethodNotFound: -32601,
  InvalidParams: -32602,
  InternalError: -32603,
} as const;

/** WebSocket close codes used by the daemon (PROTOCOL.md §1). */
export const CloseCode = {
  Normal: 1000,
  GoingAway: 1001,
  /** Final. */
  Unauthorized: 4401,
  /** No hello within 10 s; retryable. */
  HelloTimeout: 4408,
  /** Final. */
  ProtocolMismatch: 4426,
  /** The client fell too far behind on notifications; reconnect and re-read state. */
  TooSlow: 4429,
} as const;

/** The daemon answered a request with a JSON-RPC error object. */
export class WhooshRpcError extends Error {
  override readonly name = "WhooshRpcError";
  readonly code: number;
  readonly data: unknown;
  /** The method that failed, when known. */
  readonly method: string | undefined;

  constructor(code: number, message: string, data?: unknown, method?: string) {
    super(message);
    this.code = code;
    this.data = data;
    this.method = method;
  }

  get isUnauthorized(): boolean {
    return this.code === ErrorCode.Unauthorized;
  }
  get isNotFound(): boolean {
    return this.code === ErrorCode.NotFound;
  }
  get isInvalidState(): boolean {
    return this.code === ErrorCode.InvalidState;
  }
  get isNetwork(): boolean {
    return this.code === ErrorCode.Network;
  }
  get isInvalidParams(): boolean {
    return this.code === JsonRpcErrorCode.InvalidParams;
  }
}

export type ConnectionErrorReason =
  /** `close()` was called on the client. */
  | "closed"
  /** No response within the request timeout. */
  | "timeout"
  /** The socket dropped while the request was in flight; it may or may not have been applied. */
  | "connection_lost"
  /** The daemon rejected the token (close code 4401). */
  | "unauthorized"
  /** The daemon speaks an incompatible protocol version (close code 4426 or hello mismatch). */
  | "protocol_mismatch"
  /** Reconnecting gave up after `maxAttempts`. */
  | "gave_up";

/** The request never got an answer because of the transport, not the daemon. */
export class WhooshConnectionError extends Error {
  override readonly name = "WhooshConnectionError";
  readonly reason: ConnectionErrorReason;

  constructor(reason: ConnectionErrorReason, message?: string) {
    super(message ?? `connection error: ${reason}`);
    this.reason = reason;
  }
}

export function isWhooshRpcError(e: unknown): e is WhooshRpcError {
  return e instanceof WhooshRpcError;
}

export function isWhooshConnectionError(e: unknown): e is WhooshConnectionError {
  return e instanceof WhooshConnectionError;
}
