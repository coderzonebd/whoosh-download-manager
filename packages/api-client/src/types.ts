// TypeScript mirror of `crates/proto/src/lib.rs`. Spec: `docs/api/PROTOCOL.md`.
// Change all three together, in one commit.

export const PROTOCOL_VERSION = 1;
export const DEFAULT_PORT = 47210;
export const RPC_PATH = "/rpc";

export const Method = {
  SessionHello: "session.hello",
  DaemonShutdown: "daemon.shutdown",
  DownloadProbe: "download.probe",
  DownloadAdd: "download.add",
  DownloadList: "download.list",
  DownloadGet: "download.get",
  DownloadPause: "download.pause",
  DownloadResume: "download.resume",
  DownloadRemove: "download.remove",
  DownloadRefreshUrl: "download.refreshUrl",
  DownloadSetQueue: "download.setQueue",
  QueueList: "queue.list",
  QueueCreate: "queue.create",
  QueueUpdate: "queue.update",
  QueueRename: "queue.rename",
  QueueRemove: "queue.remove",
  QueueStart: "queue.start",
  QueueStop: "queue.stop",
  QueueCancelAction: "queue.cancelAction",
  NetPreconnect: "net.preconnect",
  StatsGet: "stats.get",
  SettingsGet: "settings.get",
  SettingsSet: "settings.set",
} as const;
export type MethodName = (typeof Method)[keyof typeof Method];

export const Event = {
  DownloadAdded: "event.download.added",
  DownloadUpdated: "event.download.updated",
  DownloadProgress: "event.download.progress",
  DownloadState: "event.download.state",
  DownloadRemoved: "event.download.removed",
  Stats: "event.stats",
  /** The full `Settings`, sent to every client after any change. */
  Settings: "event.settings",
  /** Every queue (`Queue[]`), sent to every client after any change. */
  Queues: "event.queues",
  /** A queue finished and its after-done action is about to run. */
  QueueFinished: "event.queue.finished",
} as const;
export type EventName = (typeof Event)[keyof typeof Event];

/** Answered by `whoosh-nmhost` itself, never by the daemon (PROTOCOL.md §1). */
export const HostMethod = { NmStatus: "nm.status" } as const;
/** Sent by `whoosh-nmhost` whenever its daemon connection goes up or down. */
export const HostEvent = { NmStatus: "event.nm.status" } as const;
/** Request ids starting with this prefix are reserved for the native host. */
export const HOST_RESERVED_ID_PREFIX = "nmhost:";

/** Built-in category ids; any other string is a user-defined category. */
export const BUILT_IN_CATEGORIES = ["video", "music", "documents", "compressed", "programs", "images", "other"] as const;
export type BuiltInCategory = (typeof BUILT_IN_CATEGORIES)[number];

export const ErrorCode = {
  Unauthorized: -32001,
  NotFound: -32002,
  InvalidState: -32003,
  Io: -32004,
  Network: -32005,
  Unsupported: -32006,
  /** Native host only: daemon not running, token rejected, or connection lost. */
  DaemonUnavailable: -32007,
  /** Native host only: the response would exceed the browser's 1 MiB limit. */
  MessageTooLarge: -32008,
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
  /** Fresh per connection: 32–128 lower-case hex characters (see auth.ts). */
  nonce: string;
  protocol: number;
  client: ClientInfo;
}

export interface HelloResult {
  daemonVersion: string;
  protocol: number;
  capabilities: string[];
  /** HMAC of the nonce under the token; the client checks it before sending anything else. */
  proof: string;
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
  /** Use exactly this many connections (1–64) instead of learning the count. */
  connections?: number;
  /** A named queue (one download at a time); the main queue when absent. */
  queue?: string;
}

/** Upper bound of `AddParams.connections`. */
export const MAX_CONNECTIONS_PER_DOWNLOAD = 64;

export interface AddResult {
  id: DownloadId;
}

export interface ListParams {
  states?: DownloadState[];
  category?: string;
  /** Skip this many results (newest first). */
  offset?: number;
  /** Return at most this many. Extensions must page: native messaging responses are limited to 1 MiB. */
  limit?: number;
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
  /** A built-in id from `BUILT_IN_CATEGORIES` or a user-defined name. */
  category?: string;
  /** The queue this download belongs to; absent means the main queue. */
  queue?: string;
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
  /**
   * "powerSafe" (default): data is forced to disk before a file is finished, so a power cut never
   * damages a finished file. "processSafe": faster, survives a killed whoosh but not a power cut.
   */
  durability?: Durability;
  /** Speed limits by time of day; while one applies it replaces `speedLimitBps`. */
  speedSchedule?: SpeedWindow[];
}

export type Durability = "powerSafe" | "processSafe";

// ------------------------------------------------------- queues and scheduler

/** The main queue's name: where downloads go without `queue`. Its `maxActive` is `Settings.maxActiveDownloads`. */
export const MAIN_QUEUE = "main";
/** Seconds between `event.queue.finished` and the after-done action. */
export const AFTER_DONE_GRACE_SECS = 60;

export interface Queue {
  name: string;
  /** How many of its downloads run at once (1–64). Named queues default to 1. */
  maxActive: number;
  schedule?: Schedule;
  /** False after `queue.stop`, until `queue.start` or the next scheduled start. */
  running: boolean;
}

/** `queue.create` / `queue.update`: `maxActive` and `schedule` replace the queue's; absent `schedule` removes it. */
export interface QueueConfig {
  name: string;
  maxActive?: number;
  schedule?: Schedule;
}

export interface Schedule {
  start?: ClockTime;
  stop?: ClockTime;
  /** Default "none". */
  afterDone?: AfterDone;
}

/** A local time of day on the daemon's machine. */
export interface ClockTime {
  /** "HH:MM", 24-hour. */
  at: string;
  /** 0 = Sunday … 6 = Saturday; absent or empty means every day. */
  days?: number[];
}

export type AfterDone = "none" | "sleep" | "shutdown";

/** While open, `limitBps` replaces `Settings.speedLimitBps`; overlapping windows: the lowest wins. `from` after `to` spans midnight. */
export interface SpeedWindow {
  /** Days the window opens; absent or empty means every day. */
  days?: number[];
  from: string;
  to: string;
  limitBps: number;
}

export interface QueueNameParams {
  name: string;
}

export interface QueueRenameParams {
  name: string;
  newName: string;
}

/** Move downloads to `queue` (absent = the main queue). */
export interface SetQueueParams {
  ids: DownloadId[];
  queue?: string;
}

/** The queue's after-done action runs at `atMs` unless a client calls `queue.cancelAction`. */
export interface QueueFinishedEvent {
  name: string;
  action: AfterDone;
  atMs: number;
}

// ------------------------------------------------------------ events

export interface ProgressItem {
  id: DownloadId;
  downloadedBytes: number;
  totalBytes?: number;
  speedBps: number;
  etaSecs?: number;
  connections: number;
  /** `[start, end, downloadedBytes]` per segment, at most once per second per download. */
  segments?: [number, number, number][];
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

/** Result of `nm.status` and payload of `event.nm.status` (native host only). */
export interface NmStatus {
  daemonRunning: boolean;
  daemonVersion?: string;
  hostVersion: string;
}

// ------------------------------------------------------------ typed method map

export interface MethodMap {
  "session.hello": { params: HelloParams; result: HelloResult };
  "daemon.shutdown": { params: undefined; result: null };
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
  "download.setQueue": { params: SetQueueParams; result: null };
  "queue.list": { params: undefined; result: Queue[] };
  "queue.create": { params: QueueConfig; result: Queue };
  "queue.update": { params: QueueConfig; result: Queue };
  "queue.rename": { params: QueueRenameParams; result: Queue };
  "queue.remove": { params: QueueNameParams; result: null };
  "queue.start": { params: QueueNameParams; result: null };
  "queue.stop": { params: QueueNameParams; result: null };
  "queue.cancelAction": { params: QueueNameParams; result: null };
}

export interface EventMap {
  "event.download.added": DownloadSummary;
  /** The summary again after its name, size or category changed; replace your copy. */
  "event.download.updated": DownloadSummary;
  "event.download.progress": ProgressEvent;
  "event.download.state": StateEvent;
  "event.download.removed": RemovedEvent;
  "event.stats": Stats;
  "event.settings": Settings;
  "event.queues": Queue[];
  "event.queue.finished": QueueFinishedEvent;
}

/** Extra methods and events available only through the native messaging host. */
export interface HostMethodMap {
  "nm.status": { params: undefined; result: NmStatus };
}

export interface HostEventMap {
  "event.nm.status": NmStatus;
}

/** The transport-agnostic client every UI codes against (WebSocket, native messaging, or mock). */
export interface WhooshClient {
  call<M extends keyof MethodMap>(method: M, params: MethodMap[M]["params"]): Promise<MethodMap[M]["result"]>;
  on<E extends keyof EventMap>(event: E, handler: (payload: EventMap[E]) => void): () => void;
  close(): void;
}
