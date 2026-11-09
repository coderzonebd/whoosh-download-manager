// SPDX-License-Identifier: Apache-2.0
//
// An in-memory VayuClient that simulates a daemon: segmented downloads with
// wobbling speeds, dynamic segment splitting (when a connection finishes its
// range it takes half of the largest remaining one), a global speed limit,
// the active-download limit, a failure, an expiring link (`needs_refresh`),
// and progress events batched at ≤ 4 Hz exactly like PROTOCOL.md §4.
//
// UIs run on it until the daemon exists (Phase 1); tests drive it with
// `autoTick: false` and `advance(ms)` for deterministic results.

import { CATEGORIES, categoryForFilename } from "./categories";
import { VayuConnectionError, VayuRpcError, JsonRpcErrorCode } from "./errors";
import {
  ErrorCode,
  PROTOCOL_VERSION,
  type AddParams,
  type DownloadDetail,
  type DownloadState,
  type DownloadSummary,
  type EventMap,
  type MethodMap,
  type ProbeResult,
  type ProgressItem,
  type SegmentInfo,
  type Settings,
  type Stats,
  type VayuClient,
} from "./types";

export interface MockClientOptions {
  /** Seed for the deterministic PRNG (default 0x5eed). */
  seed?: number;
  /** Simulation step in ms (default 250). Progress events are still capped at 4 Hz. */
  tickMs?: number;
  /** Run the simulation on a timer (default true). Set false and call `advance()` in tests. */
  autoTick?: boolean;
  /** Initial downloads: a realistic demo set, or nothing. Default "demo". */
  scenario?: "demo" | "empty";
  /** Extra synthetic downloads appended to the scenario (for 10 000-row stress tests). */
  bulk?: number;
  /** Simulated round-trip latency for every call, in ms (default 0). Probes take 6× longer. */
  latencyMs?: number;
  /** Override the default settings. */
  settings?: Partial<Settings>;
  /** Wall clock used for timestamps (default `Date.now`). */
  now?: () => number;
}

export interface MockVayuClient extends VayuClient {
  /** Run the simulation forward by `ms` synchronously (in `tickMs` steps). */
  advance(ms: number): void;
  /** Snapshot of every download with segments, for assertions. */
  snapshot(): DownloadDetail[];
}

export const MOCK_DAEMON_VERSION = "0.0.0-mock";

const MB = 1024 * 1024;
const GB = 1024 * MB;
const PROGRESS_MIN_INTERVAL_MS = 250; // ≤ 4 Hz
const STATS_INTERVAL_MS = 1000;
const PROBE_MS = 700;
const VERIFY_MS = 900;
const MIN_SPLIT_BYTES = 2 * MB;

const DEFAULT_SETTINGS: Settings = {
  defaultDirectory: "~/Downloads",
  maxActiveDownloads: 5,
  maxConnectionsPerDownload: 8,
  language: "en",
  theme: "system",
};

const ACTIVE: ReadonlySet<DownloadState> = new Set(["probing", "downloading", "verifying"]);

const CONTENT_TYPE_BY_CATEGORY: Record<string, string> = {
  compressed: "application/octet-stream",
  documents: "application/pdf",
  music: "audio/mpeg",
  video: "video/mp4",
  programs: "application/octet-stream",
  images: "image/jpeg",
};

export { categoryForFilename, CATEGORIES };

function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function hashString(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

function filenameFromUrl(raw: string): string {
  try {
    const u = new URL(raw);
    const last = u.pathname.split("/").filter(Boolean).pop();
    if (last) return decodeURIComponent(last);
    return `${u.hostname}.html`;
  } catch {
    return "download";
  }
}

function invalidParams(message: string): VayuRpcError {
  return new VayuRpcError(JsonRpcErrorCode.InvalidParams, message);
}

function parseUrl(raw: unknown): URL {
  if (typeof raw !== "string" || raw.trim() === "") throw invalidParams("url is required");
  let u: URL;
  try {
    u = new URL(raw.trim());
  } catch {
    throw invalidParams(`not a valid URL: ${raw}`);
  }
  if (!["http:", "https:", "ftp:"].includes(u.protocol)) {
    throw new VayuRpcError(ErrorCode.Unsupported, `unsupported scheme ${u.protocol.replace(":", "")}`);
  }
  return u;
}

interface Sim {
  d: DownloadDetail;
  /** Nominal speed of the whole download (bytes/s) before wobble and limits. */
  baseBps: number;
  phase: number;
  /** For unknown-size downloads: the size at which the stream ends. */
  hiddenTotal?: number;
  failAt?: number;
  expireAt?: number;
  /** Simulated time at which probing / verifying finishes. */
  until?: number;
  /** Bytes still on the wire at the last progress emission (to emit only changes). */
  dirty: boolean;
}

interface Seed {
  url: string;
  filename?: string;
  size?: number;
  state: DownloadState;
  fraction?: number;
  bps?: number;
  connections?: number;
  resumable?: boolean;
  ageMin: number;
  failAt?: number;
  expireAt?: number;
  error?: { code: string; message: string };
}

const DEMO: Seed[] = [
  { url: "https://releases.ubuntu.com/26.04/ubuntu-26.04-desktop-amd64.iso", size: 6.2 * GB, state: "downloading", fraction: 0.18, bps: 22 * MB, connections: 8, ageMin: 6 },
  { url: "https://download.blender.org/release/Blender5.1/blender-5.1.0-linux-x64.tar.xz", size: 381 * MB, state: "downloading", fraction: 0.42, bps: 9.5 * MB, connections: 6, ageMin: 3 },
  {
    url: "https://cdn.example-university.edu/lectures/cse-4105/lecture-12-distributed-systems.mp4?Expires=1762790400&Signature=k3J9fQ",
    size: 1.4 * GB, state: "downloading", fraction: 0.27, bps: 4.2 * MB, connections: 4, ageMin: 11, expireAt: 0.34,
  },
  { url: "https://data.example.gov.bd/census/2022/bd-census-2022-full.zip", size: 820 * MB, state: "downloading", fraction: 0.15, bps: 2.6 * MB, connections: 4, ageMin: 9, failAt: 0.2 },
  { url: "http://live.example.net/radio/recording-2026-11-09.ts", state: "downloading", fraction: 0, bps: 380 * 1024, connections: 1, resumable: false, ageMin: 2 },
  { url: "https://archive.example.org/music/%E0%A6%B0%E0%A6%AC%E0%A7%80%E0%A6%A8%E0%A7%8D%E0%A6%A6%E0%A7%8D%E0%A6%B0%E0%A6%B8%E0%A6%99%E0%A7%8D%E0%A6%97%E0%A7%80%E0%A6%A4-%E0%A6%B8%E0%A6%82%E0%A6%95%E0%A6%B2%E0%A6%A8.zip", size: 452 * MB, state: "paused", fraction: 0.61, ageMin: 95 },
  { url: "https://media.example.org/films/big-buck-bunny-4k.mkv", size: 2.9 * GB, state: "paused", fraction: 0.12, ageMin: 240 },
  { url: "https://photos.example.com/share/%E0%A6%95%E0%A6%95%E0%A7%8D%E0%A6%B8%E0%A6%AC%E0%A6%BE%E0%A6%9C%E0%A6%BE%E0%A6%B0-%E0%A6%AD%E0%A7%8D%E0%A6%B0%E0%A6%AE%E0%A6%A3.zip", size: 1.1 * GB, state: "queued", ageMin: 1 },
  { url: "https://podcasts.example.fm/episodes/episode-214-open-source-in-bangladesh.mp3", size: 64 * MB, state: "queued", ageMin: 1 },
  { url: "https://nodejs.org/dist/v24.11.0/node-v24.11.0-x64.msi", size: 32 * MB, state: "completed", ageMin: 60 * 26 },
  { url: "https://www.example.com/investors/annual-report-2025.pdf", size: 8.4 * MB, state: "completed", ageMin: 60 * 50 },
  { url: "https://update.code.visualstudio.com/1.106.0/win32-x64-user/VSCodeUserSetup-x64-1.106.0.exe", size: 110 * MB, state: "completed", ageMin: 60 * 72 },
  { url: "https://wallpapers.example.com/sundarbans-mangrove-8k.jpg", size: 24 * MB, state: "completed", ageMin: 60 * 5 },
  {
    url: "https://mirror.example.net/old/fedora-workstation-41.iso", size: 2.2 * GB, state: "failed", fraction: 0.48, ageMin: 60 * 30,
    error: { code: "http_404", message: "The server says this file no longer exists (HTTP 404)." },
  },
];

const BULK_NAMES = [
  ["lecture", "mp4"], ["dataset", "csv"], ["backup", "tar.gz"], ["photo", "jpg"], ["song", "mp3"],
  ["installer", "exe"], ["paper", "pdf"], ["firmware", "img"], ["episode", "mkv"], ["package", "deb"],
] as const;

export function createMockClient(options: MockClientOptions = {}): MockVayuClient {
  const tickMs = options.tickMs ?? 250;
  const latencyMs = options.latencyMs ?? 0;
  const wallNow = options.now ?? Date.now;
  const rng = mulberry32(options.seed ?? 0x5eed);

  const settings: Settings = { ...DEFAULT_SETTINGS, ...options.settings };
  const sims = new Map<string, Sim>();
  const handlers = new Map<keyof EventMap, Set<(p: never) => void>>();
  let nextId = 1;
  let simTime = 0;
  let lastProgressAt = -Infinity;
  let lastStatsAt = 0;
  let closed = false;
  let timer: ReturnType<typeof setInterval> | undefined;

  // ---------------------------------------------------------------- helpers

  const newId = () => `dl_${(nextId++).toString(36).padStart(6, "0")}`;

  function emit<E extends keyof EventMap>(event: E, payload: EventMap[E]): void {
    const set = handlers.get(event);
    if (!set) return;
    for (const h of [...set]) {
      try {
        (h as (p: EventMap[E]) => void)(structuredClone(payload));
      } catch (err) {
        console.error(`[vayu mock] ${event} handler threw`, err);
      }
    }
  }

  function summary(d: DownloadDetail): DownloadSummary {
    // Shallow on purpose: call results and event payloads are cloned once on the way out.
    const { segments: _segments, ...rest } = d;
    return rest;
  }

  function makeSegments(total: number, n: number, fraction: number): SegmentInfo[] {
    const segs: SegmentInfo[] = [];
    const len = Math.floor(total / n);
    for (let i = 0; i < n; i++) {
      const start = i * len;
      const end = i === n - 1 ? total : start + len;
      const f = Math.min(1, Math.max(0, fraction + (rng() - 0.5) * 0.3));
      segs.push({ start, end, downloadedBytes: Math.floor((end - start) * f), speedBps: 0, source: "" });
    }
    return segs;
  }

  function sourceOf(url: string): string {
    try {
      return new URL(url).host;
    } catch {
      return "";
    }
  }

  function uniqueFilename(directory: string, filename: string): string {
    const taken = new Set([...sims.values()].filter((s) => s.d.directory === directory).map((s) => s.d.filename));
    if (!taken.has(filename)) return filename;
    const dot = filename.lastIndexOf(".");
    const [base, ext] = dot > 0 ? [filename.slice(0, dot), filename.slice(dot)] : [filename, ""];
    for (let i = 1; ; i++) {
      const candidate = `${base} (${i})${ext}`;
      if (!taken.has(candidate)) return candidate;
    }
  }

  function setState(sim: Sim, state: DownloadState, error?: { code: string; message: string }): void {
    const d = sim.d;
    d.state = state;
    if (error) d.error = error;
    else delete d.error;
    if (state !== "downloading") {
      d.speedBps = 0;
      delete d.etaSecs;
      for (const s of d.segments) s.speedBps = 0;
      d.connections = 0;
    }
    if (state === "completed") d.completedAt = wallNow();
    sim.dirty = true;
    emit("event.download.state", error ? { id: d.id, state, error } : { id: d.id, state });
  }

  function addSim(seed: Seed, at: number): Sim {
    const id = newId();
    const filename = seed.filename ?? filenameFromUrl(seed.url);
    const size = seed.size !== undefined ? Math.round(seed.size) : undefined;
    const fraction = seed.state === "completed" ? 1 : (seed.fraction ?? 0);
    const conns = seed.connections ?? settings.maxConnectionsPerDownload;
    const d: DownloadDetail = {
      id,
      url: seed.url,
      filename,
      directory: settings.defaultDirectory,
      state: seed.state,
      downloadedBytes: 0,
      speedBps: 0,
      connections: 0,
      resumable: seed.resumable ?? true,
      category: categoryForFilename(filename),
      addedAt: at,
      segments: [],
    };
    if (size !== undefined) {
      d.totalBytes = size;
      if (seed.state === "completed") {
        d.segments = [{ start: 0, end: size, downloadedBytes: size, speedBps: 0, source: sourceOf(seed.url) }];
      } else if (fraction > 0) {
        d.segments = makeSegments(size, Math.max(1, conns), fraction);
        for (const s of d.segments) s.source = sourceOf(seed.url);
      }
      d.downloadedBytes = d.segments.reduce((a, s) => a + s.downloadedBytes, 0);
    }
    if (seed.state === "completed") d.completedAt = at + 60_000;
    if (seed.error) d.error = seed.error;
    const sim: Sim = {
      d,
      baseBps: seed.bps ?? (1.5 + rng() * 12) * MB,
      phase: rng() * Math.PI * 2,
      dirty: false,
    };
    if (size === undefined) sim.hiddenTotal = Math.round((40 + rng() * 60) * MB);
    if (seed.failAt !== undefined) sim.failAt = seed.failAt;
    if (seed.expireAt !== undefined) sim.expireAt = seed.expireAt;
    if (seed.state === "downloading") {
      ensureSegments(sim, conns);
      d.connections = d.segments.filter((s) => s.downloadedBytes < s.end - s.start).length || 1;
    }
    sims.set(id, sim);
    return sim;
  }

  /** Make sure a starting download has segments to work on. */
  function ensureSegments(sim: Sim, n = settings.maxConnectionsPerDownload): void {
    const d = sim.d;
    const src = sourceOf(d.url);
    if (d.totalBytes === undefined || !d.resumable) {
      // One connection, no ranges.
      if (d.segments.length === 0) d.segments = [{ start: 0, end: d.totalBytes ?? 0, downloadedBytes: d.downloadedBytes, speedBps: 0, source: src }];
      return;
    }
    if (d.segments.length === 0) {
      const count = Math.max(1, Math.min(n, Math.ceil(d.totalBytes / (4 * MB))));
      d.segments = makeSegments(d.totalBytes, count, 0).map((s) => ({ ...s, downloadedBytes: 0, source: src }));
    }
  }

  /** IDM-style dynamic segmentation: a finished connection takes half of the largest remaining range. */
  function rebalance(sim: Sim): void {
    const d = sim.d;
    if (!d.resumable || d.totalBytes === undefined) return;
    const open = () => d.segments.filter((s) => s.start + s.downloadedBytes < s.end).length;
    while (open() < settings.maxConnectionsPerDownload) {
      let best: SegmentInfo | undefined;
      let bestLeft = 0;
      for (const s of d.segments) {
        const left = s.end - (s.start + s.downloadedBytes);
        if (left > bestLeft) {
          best = s;
          bestLeft = left;
        }
      }
      if (!best || bestLeft < MIN_SPLIT_BYTES * 2) return;
      const mid = best.start + best.downloadedBytes + Math.floor(bestLeft / 2);
      const added: SegmentInfo = { start: mid, end: best.end, downloadedBytes: 0, speedBps: 0, source: best.source };
      best.end = mid;
      d.segments.splice(d.segments.indexOf(best) + 1, 0, added);
    }
  }

  // ---------------------------------------------------------------- simulation

  function step(dtMs: number): void {
    simTime += dtMs;
    const dt = dtMs / 1000;

    // Probing / verifying timers.
    for (const sim of sims.values()) {
      if (sim.until === undefined || simTime < sim.until) continue;
      delete sim.until;
      if (sim.d.state === "probing") {
        ensureSegments(sim);
        setState(sim, "downloading");
      } else if (sim.d.state === "verifying") {
        setState(sim, "completed");
      }
    }

    // Promote the queue while there is room.
    let active = 0;
    for (const sim of sims.values()) if (ACTIVE.has(sim.d.state)) active++;
    if (active < settings.maxActiveDownloads) {
      for (const sim of sims.values()) {
        if (active >= settings.maxActiveDownloads) break;
        if (sim.d.state !== "queued") continue;
        sim.until = simTime + PROBE_MS;
        setState(sim, "probing");
        active++;
      }
    }

    // Desired speeds, then scale to the global limit.
    const downloading = [...sims.values()].filter((s) => s.d.state === "downloading");
    const desired = new Map<Sim, number>();
    let totalDesired = 0;
    for (const sim of downloading) {
      const wobble = 0.78 + 0.22 * Math.sin(simTime / 1700 + sim.phase) + (rng() - 0.5) * 0.18;
      const v = Math.max(0, sim.baseBps * wobble);
      desired.set(sim, v);
      totalDesired += v;
    }
    const limit = settings.speedLimitBps;
    const scale = limit !== undefined && totalDesired > limit ? limit / totalDesired : 1;

    for (const sim of downloading) {
      const d = sim.d;
      const target = (desired.get(sim) ?? 0) * scale;
      const open = d.segments.filter((s) => d.totalBytes === undefined || s.start + s.downloadedBytes < s.end);
      let gained = 0;
      for (const s of d.segments) s.speedBps = 0;
      for (const [i, s] of open.entries()) {
        const share = (target / open.length) * (0.7 + 0.6 * ((Math.sin(simTime / 900 + sim.phase + i) + 1) / 2));
        let bytes = Math.round(share * dt);
        if (d.totalBytes !== undefined) bytes = Math.min(bytes, s.end - (s.start + s.downloadedBytes));
        s.downloadedBytes += bytes;
        if (d.totalBytes === undefined) s.end = s.downloadedBytes;
        s.speedBps = Math.round(bytes / dt);
        gained += bytes;
      }
      d.downloadedBytes += gained;
      const instant = gained / dt;
      d.speedBps = Math.round(d.speedBps === 0 ? instant : d.speedBps * 0.6 + instant * 0.4);
      rebalance(sim);
      d.connections = d.segments.filter((s) => d.totalBytes === undefined || s.start + s.downloadedBytes < s.end).length;
      sim.dirty = true;

      const total = d.totalBytes ?? sim.hiddenTotal ?? Infinity;
      const fraction = d.downloadedBytes / total;
      if (d.totalBytes !== undefined && d.speedBps > 0) {
        d.etaSecs = Math.ceil((d.totalBytes - d.downloadedBytes) / d.speedBps);
      }
      if (sim.failAt !== undefined && fraction >= sim.failAt) {
        delete sim.failAt;
        setState(sim, "failed", { code: "remote_changed", message: "The file on the server changed while downloading." });
      } else if (sim.expireAt !== undefined && fraction >= sim.expireAt) {
        delete sim.expireAt;
        setState(sim, "needs_refresh", { code: "link_expired", message: "The download link expired (HTTP 403)." });
      } else if (fraction >= 1) {
        if (d.totalBytes === undefined) {
          d.totalBytes = d.downloadedBytes;
        }
        d.downloadedBytes = d.totalBytes;
        sim.until = simTime + VERIFY_MS;
        setState(sim, "verifying");
      }
    }

    if (simTime - lastProgressAt >= PROGRESS_MIN_INTERVAL_MS) {
      const items: ProgressItem[] = [];
      for (const sim of sims.values()) {
        if (!sim.dirty) continue;
        sim.dirty = false;
        const d = sim.d;
        const item: ProgressItem = { id: d.id, downloadedBytes: d.downloadedBytes, speedBps: d.speedBps, connections: d.connections };
        if (d.totalBytes !== undefined) item.totalBytes = d.totalBytes;
        if (d.etaSecs !== undefined) item.etaSecs = d.etaSecs;
        items.push(item);
      }
      if (items.length > 0) {
        lastProgressAt = simTime;
        emit("event.download.progress", { items });
      }
    }

    if (simTime - lastStatsAt >= STATS_INTERVAL_MS) {
      lastStatsAt = simTime;
      emit("event.stats", stats());
    }
  }

  function stats(): Stats {
    let downloadBps = 0;
    let active = 0;
    let queued = 0;
    for (const sim of sims.values()) {
      if (ACTIVE.has(sim.d.state)) active++;
      if (sim.d.state === "queued") queued++;
      downloadBps += sim.d.speedBps;
    }
    return { downloadBps, uploadBps: 0, active, queued };
  }

  // ---------------------------------------------------------------- methods

  function get(id: unknown): Sim {
    const sim = typeof id === "string" ? sims.get(id) : undefined;
    if (!sim) throw new VayuRpcError(ErrorCode.NotFound, `no download with id ${String(id)}`);
    return sim;
  }

  function idsOf(params: unknown): Sim[] {
    const ids = (params as { ids?: unknown } | undefined)?.ids;
    if (!Array.isArray(ids)) throw invalidParams("ids must be an array");
    return ids.map(get);
  }

  function probe(rawUrl: string): ProbeResult {
    const u = parseUrl(rawUrl);
    const href = u.toString();
    if (/(^|\/)404(\/|$)|not-found/.test(u.pathname)) {
      throw new VayuRpcError(ErrorCode.Network, "The server says this file does not exist (HTTP 404).");
    }
    if (u.hostname.endsWith(".invalid")) {
      throw new VayuRpcError(ErrorCode.Network, `Could not resolve host ${u.hostname}.`);
    }
    const filename = filenameFromUrl(href);
    const category = categoryForFilename(filename);
    const result: ProbeResult = { resumable: !/noresume|live|stream/.test(href), filename, finalUrl: href };
    if (!/live|stream/.test(href)) result.totalBytes = Math.round(2 * MB + (hashString(href) / 0xffffffff) * 3.5 * GB);
    const ct = CONTENT_TYPE_BY_CATEGORY[category];
    if (ct) result.contentType = ct;
    return result;
  }

  function add(params: AddParams): string {
    const p = probe(params.url);
    const directory = params.directory?.trim() || settings.defaultDirectory;
    const filename = uniqueFilename(directory, params.filename?.trim() || p.filename);
    const start = params.start ?? true;
    const seed: Seed = { url: p.finalUrl, filename, state: start ? "queued" : "paused", ageMin: 0, resumable: p.resumable };
    if (p.totalBytes !== undefined) seed.size = p.totalBytes;
    const sim = addSim(seed, wallNow());
    sim.d.directory = directory;
    if (params.category) sim.d.category = params.category;
    emit("event.download.added", summary(sim.d));
    return sim.d.id;
  }

  function validateSettings(s: unknown): Settings {
    const v = s as Partial<Settings> | undefined;
    if (!v || typeof v !== "object") throw invalidParams("settings object required");
    const intIn = (n: unknown, lo: number, hi: number) => Number.isInteger(n) && (n as number) >= lo && (n as number) <= hi;
    if (typeof v.defaultDirectory !== "string" || v.defaultDirectory.trim() === "") throw invalidParams("defaultDirectory is required");
    if (!intIn(v.maxActiveDownloads, 1, 64)) throw invalidParams("maxActiveDownloads must be 1–64");
    if (!intIn(v.maxConnectionsPerDownload, 1, 64)) throw invalidParams("maxConnectionsPerDownload must be 1–64");
    if (v.speedLimitBps !== undefined && !intIn(v.speedLimitBps, 1, Number.MAX_SAFE_INTEGER)) throw invalidParams("speedLimitBps must be a positive integer");
    if (typeof v.language !== "string" || v.language === "") throw invalidParams("language is required");
    if (!["system", "light", "dark"].includes(v.theme as string)) throw invalidParams("theme must be system, light or dark");
    const out: Settings = {
      defaultDirectory: v.defaultDirectory.trim(),
      maxActiveDownloads: v.maxActiveDownloads as number,
      maxConnectionsPerDownload: v.maxConnectionsPerDownload as number,
      language: v.language,
      theme: v.theme as Settings["theme"],
    };
    if (v.speedLimitBps !== undefined) out.speedLimitBps = v.speedLimitBps;
    return out;
  }

  const handlersByMethod: { [M in keyof MethodMap]: (params: MethodMap[M]["params"]) => MethodMap[M]["result"] } = {
    "session.hello": (p) => {
      if (p?.protocol !== PROTOCOL_VERSION) {
        throw new VayuRpcError(ErrorCode.Unsupported, `protocol ${String(p?.protocol)} not supported`);
      }
      return { daemonVersion: MOCK_DAEMON_VERSION, protocol: PROTOCOL_VERSION, capabilities: ["http", "mock"] };
    },
    "download.probe": (p) => probe(p?.url),
    "download.add": (p) => ({ id: add(p) }),
    "download.list": (p) => {
      const states = p?.states && p.states.length > 0 ? new Set(p.states) : undefined;
      const out: DownloadSummary[] = [];
      for (const sim of sims.values()) {
        if (states && !states.has(sim.d.state)) continue;
        if (p?.category !== undefined && sim.d.category !== p.category) continue;
        out.push(summary(sim.d));
      }
      return out;
    },
    "download.get": (p) => structuredClone(get(p?.id).d),
    "download.pause": (p) => {
      const targets = idsOf(p);
      const bad = targets.filter((s) => !["queued", "probing", "downloading", "paused"].includes(s.d.state));
      if (bad.length > 0) {
        throw new VayuRpcError(ErrorCode.InvalidState, `cannot pause a ${bad[0]?.d.state} download`, { ids: bad.map((s) => s.d.id) });
      }
      for (const sim of targets) {
        if (sim.d.state === "paused") continue;
        delete sim.until;
        setState(sim, "paused");
      }
      return null;
    },
    "download.resume": (p) => {
      const targets = idsOf(p);
      const bad = targets.filter((s) => s.d.state === "completed" || s.d.state === "needs_refresh" || s.d.state === "verifying");
      if (bad.length > 0) {
        const why = bad[0]?.d.state === "needs_refresh" ? "the link expired; refresh it first" : `cannot resume a ${bad[0]?.d.state} download`;
        throw new VayuRpcError(ErrorCode.InvalidState, why, { ids: bad.map((s) => s.d.id) });
      }
      for (const sim of targets) {
        if (sim.d.state !== "paused" && sim.d.state !== "failed") continue;
        if (sim.d.state === "failed" && !sim.d.resumable) {
          sim.d.downloadedBytes = 0;
          sim.d.segments = [];
        }
        setState(sim, "queued");
      }
      return null;
    },
    "download.remove": (p) => {
      const targets = idsOf(p);
      for (const sim of targets) sims.delete(sim.d.id);
      if (targets.length > 0) emit("event.download.removed", { ids: targets.map((s) => s.d.id) });
      return null;
    },
    "download.refreshUrl": (p) => {
      const sim = get(p?.id);
      const u = parseUrl(p?.url);
      if (sim.d.state === "completed") throw new VayuRpcError(ErrorCode.InvalidState, "the download is already complete");
      sim.d.url = u.toString();
      delete sim.expireAt;
      if (sim.d.state === "needs_refresh" || sim.d.state === "failed") setState(sim, "queued");
      return null;
    },
    "net.preconnect": (p) => {
      parseUrl(p?.url);
      return null;
    },
    "stats.get": () => stats(),
    "settings.get": () => ({ ...settings }),
    "settings.set": (p) => {
      const next = validateSettings(p);
      if (next.speedLimitBps === undefined) delete settings.speedLimitBps;
      Object.assign(settings, next);
      return { ...settings };
    },
  };

  // ---------------------------------------------------------------- seed data

  const t0 = wallNow();
  if ((options.scenario ?? "demo") === "demo") {
    for (const seed of DEMO) addSim(seed, t0 - seed.ageMin * 60_000);
  }
  for (let i = 0; i < (options.bulk ?? 0); i++) {
    const [stem, ext] = BULK_NAMES[i % BULK_NAMES.length] ?? ["file", "bin"];
    const r = rng();
    const state: DownloadState = r < 0.86 ? "completed" : r < 0.94 ? "paused" : r < 0.98 ? "failed" : "queued";
    const seed: Seed = {
      url: `https://files.example.net/bulk/${stem}-${String(i + 1).padStart(5, "0")}.${ext}`,
      size: Math.round((0.5 + rng() * rng() * 900) * MB),
      state,
      fraction: state === "completed" ? 1 : rng(),
      connections: 4,
      ageMin: 60 * 24 * 30 * rng(),
    };
    if (state === "failed") seed.error = { code: "network", message: "The connection was reset by the server." };
    addSim(seed, t0 - seed.ageMin * 60_000);
  }

  // ---------------------------------------------------------------- client

  const client: MockVayuClient = {
    call(method, params) {
      if (closed) return Promise.reject(new VayuConnectionError("closed"));
      const fn = handlersByMethod[method] as ((p: unknown) => unknown) | undefined;
      const delay = method === "download.probe" ? latencyMs * 6 : latencyMs;
      const run = () => {
        if (!fn) throw new VayuRpcError(JsonRpcErrorCode.MethodNotFound, `method not found: ${method}`);
        return structuredClone(fn(params));
      };
      if (delay <= 0) {
        try {
          return Promise.resolve(run() as never);
        } catch (e) {
          return Promise.reject(e);
        }
      }
      return new Promise((resolve, reject) => {
        setTimeout(() => {
          if (closed) return reject(new VayuConnectionError("connection_lost"));
          try {
            resolve(run() as never);
          } catch (e) {
            reject(e);
          }
        }, delay);
      });
    },
    on(event, handler) {
      let set = handlers.get(event);
      if (!set) handlers.set(event, (set = new Set()));
      set.add(handler as (p: never) => void);
      return () => {
        set.delete(handler as (p: never) => void);
      };
    },
    close() {
      closed = true;
      if (timer !== undefined) clearInterval(timer);
      timer = undefined;
      handlers.clear();
    },
    advance(ms) {
      for (let t = 0; t < ms; t += tickMs) step(Math.min(tickMs, ms - t));
    },
    snapshot() {
      return [...sims.values()].map((s) => structuredClone(s.d));
    },
  };

  if (options.autoTick ?? true) timer = setInterval(() => step(tickMs), tickMs);
  return client;
}
