// SPDX-License-Identifier: GPL-3.0-or-later

import type { VayuClient } from "@vayu/api-client";

/**
 * Result of the host-only `nm.status` method (PROTOCOL.md §1). Not in
 * `@vayu/api-client` yet; see the extension README for the proposed addition.
 */
export interface NmStatus {
  daemonRunning: boolean;
  daemonVersion?: string;
  hostVersion: string;
}

/** What the popup shows about the connection, whatever the transport. */
export interface ConnectionStatus {
  mode: "pro" | "lite";
  /** Pro: the native host answered. Lite: the WASM engine loaded. */
  hostAvailable: boolean;
  daemonRunning: boolean;
  daemonVersion?: string;
  hostVersion?: string;
  /** Why it is unavailable, in words fit for the popup. */
  error?: string;
}

/**
 * How the extension reaches a download engine. Pro mode implements it over
 * native messaging (`NativeMessagingTransport`); Lite mode (Phase 4) will
 * implement it on top of the core compiled to WASM. Everything above this
 * interface — capture, menus, popup — is shared.
 */
export interface Transport extends VayuClient {
  readonly mode: "pro" | "lite";
  /** Asks the transport (and, in Pro mode, the host) for a fresh status. */
  status(): Promise<ConnectionStatus>;
  /** Called when the transport learns the status changed on its own. */
  onStatus(handler: (status: ConnectionStatus) => void): () => void;
  /**
   * False right after the transport failed, so optional traffic (preconnect)
   * does not keep starting a host that is not installed. Required traffic
   * (a captured download) always tries.
   */
  readonly healthy: boolean;
}
