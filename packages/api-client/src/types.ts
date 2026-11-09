// TypeScript mirror of `crates/proto/src/lib.rs`. Spec: `docs/api/PROTOCOL.md`.
// Change all three together, in one commit.

export const PROTOCOL_VERSION = 1;
export const DEFAULT_PORT = 47210;
export const RPC_PATH = "/rpc";

export const Method = {
  SessionHello: "session.hello",
  DownloadProbe: "download.probe",
  DownloadAdd: "download.add",
  DownloadList: "download.list",
  DownloadGet: "download.get",
  DownloadPause: "download.pause",
  DownloadResume: "download.resume",
  DownloadRemove: "download.remove",
  DownloadRefreshUrl: "download.refreshUrl",
  NetPreconnect: "net.preconnect",
  StatsGet: "stats.get",
  SettingsGet: "settings.get",
  SettingsSet: "settings.set",
} as const;
export type MethodName = (typeof Method)[keyof typeof Method];

export const Event = {
  DownloadAdded: "event.download.added",
  DownloadProgress: "event.download.progress",
  DownloadState: "event.download.state",
  DownloadRemoved: "event.download.removed",
  Stats: "event.stats",
} as const;
export type EventName = (typeof Event)[keyof typeof Event];

export const ErrorCode = {
  Unauthorized: -32001,
  NotFound: -32002,
  InvalidState: -32003,
  Io: -32004,
  Network: -32005,
  Unsupported: -32006,
} as const;

export type DownloadId = string;
export type UnixMillis = number;

// ------------------------------------------------------------ session

export interface ClientInfo {
  /** "desktop" | "cli" | "extension-chromium" | "extension-firefox" | "web-remote" | … */
  name: string;
  version: string;
}

export interface HelloParams {
  token: string;
  protocol: number;
  client: ClientInfo;
}

export interface HelloResult {
  daemonVersion: string;
  protocol: number;
  capabilities: string[];
}

// ------------------------------------------------------------ requests

export interface Header {
  name: string;
  value: string;
}

export interface RequestContext {
  method?: string;
  headers?: Header[];
  cookie?: string;
  referer?: string;
  userAgent?: string;
  bodyBase64?: string;
}

export interface Checksum {
  algorithm: "sha256" | "sha512" | "sha1" | "md5";
  value: string;
}

// ------------------------------------------------------------ download.*

export interface ProbeParams {
  url: string;
  request?: RequestContext;
}

export interface ProbeResult {
  totalBytes?: number;
  resumable: boolean;
  filename: string;
  contentType?: string;
  finalUrl: string;
}

export interface AddParams {
  url: string;
  request?: RequestContext;
  filename?: string;
  directory?: string;
  category?: string;
  /** Defaults to true. */
  start?: boolean;
  mirrors?: string[];
  checksum?: Checksum;
}

export interface AddResult {
  id: DownloadId;
}

export interface ListParams {
  states?: DownloadState[];
  category?: string;
}

export interface IdParams {
  id: DownloadId;
}

export interface IdsParams {
  ids: DownloadId[];
}

export interface RemoveParams {
  ids: DownloadId[];
  deleteFiles?: boolean;
}

export interface RefreshUrlParams {
  id: DownloadId;
  url: string;
  request?: RequestContext;
}

export type DownloadState =
  | "queued"
  | "probing"
  | "downloading"
  | "paused"
  | "needs_refresh"
  | "verifying"
  | "completed"
  | "failed";

export interface ErrorInfo {
  code: string;
  message: string;
}

export interface DownloadSummary {
  id: DownloadId;
  url: string;
  filename: string;
  directory: string;
  state: DownloadState;
  totalBytes?: number;
  downloadedBytes: number;
  speedBps: number;
  etaSecs?: number;
  connections: number;
  resumable: boolean;
  category?: string;
  addedAt: UnixMillis;
  completedAt?: UnixMillis;
  error?: ErrorInfo;
}

export interface SegmentInfo {
  start: number;
  end: number;
  downloadedBytes: number;
  speedBps: number;
  source: string;
}

export interface DownloadDetail extends DownloadSummary {
  segments: SegmentInfo[];
}

// ------------------------------------------------------------ net / stats / settings

export interface PreconnectParams {
  url: string;
}

export interface Stats {
  downloadBps: number;
  uploadBps: number;
  active: number;
  queued: number;
}

export interface Settings {
  defaultDirectory: string;
  maxActiveDownloads: number;
  maxConnectionsPerDownload: number;
  speedLimitBps?: number;
  language: string;
  theme: "system" | "light" | "dark";
}

// ------------------------------------------------------------ events

export interface ProgressItem {
  id: DownloadId;
  downloadedBytes: number;
  totalBytes?: number;
  speedBps: number;
  etaSecs?: number;
  connections: number;
}

export interface ProgressEvent {
  items: ProgressItem[];
}

export interface StateEvent {
  id: DownloadId;
  state: DownloadState;
  error?: ErrorInfo;
}

export interface RemovedEvent {
  ids: DownloadId[];
}

// ------------------------------------------------------------ typed method map

export interface MethodMap {
  "session.hello": { params: HelloParams; result: HelloResult };
  "download.probe": { params: ProbeParams; result: ProbeResult };
  "download.add": { params: AddParams; result: AddResult };
  "download.list": { params: ListParams; result: DownloadSummary[] };
  "download.get": { params: IdParams; result: DownloadDetail };
  "download.pause": { params: IdsParams; result: null };
  "download.resume": { params: IdsParams; result: null };
  "download.remove": { params: RemoveParams; result: null };
  "download.refreshUrl": { params: RefreshUrlParams; result: null };
  "net.preconnect": { params: PreconnectParams; result: null };
  "stats.get": { params: undefined; result: Stats };
  "settings.get": { params: undefined; result: Settings };
  "settings.set": { params: Settings; result: Settings };
}

export interface EventMap {
  "event.download.added": DownloadSummary;
  "event.download.progress": ProgressEvent;
  "event.download.state": StateEvent;
  "event.download.removed": RemovedEvent;
  "event.stats": Stats;
}

/** The transport-agnostic client every UI codes against (WebSocket, native messaging, or mock). */
export interface VayuClient {
  call<M extends keyof MethodMap>(method: M, params: MethodMap[M]["params"]): Promise<MethodMap[M]["result"]>;
  on<E extends keyof EventMap>(event: E, handler: (payload: EventMap[E]) => void): () => void;
  close(): void;
}
