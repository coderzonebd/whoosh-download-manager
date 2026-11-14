// SPDX-License-Identifier: GPL-3.0-or-later
// A stand-in for whoosh-nmhost in end-to-end tests: speaks native messaging on
// stdio, logs every message, and answers as the control file says:
//   {
//     "mode": "accept" | "error" | "slow", "delayMs"?: number,
//     "expire"?: { "<url substring>": <times> }   // accepted downloads whose URL
//         contains the key report needs_refresh, and again after each
//         download.refreshUrl, <times> times in all; then "downloading"
//     "list"?: DownloadSummary[]                  // the answer to download.list
//     "exitStartedBefore"?: <ms>                  // hosts started earlier exit
//   }

import { appendFileSync, readFileSync } from "node:fs";
import { endianness } from "node:os";

const LOG = process.env.WHOOSH_FAKE_HOST_LOG;
const CONTROL = process.env.WHOOSH_FAKE_HOST_CONTROL;
const LE = endianness() === "LE";

function control() {
  try {
    return JSON.parse(readFileSync(CONTROL, "utf8"));
  } catch {
    return { mode: "accept" };
  }
}

function log(entry) {
  if (LOG) appendFileSync(LOG, `${JSON.stringify({ at: Date.now(), ...entry })}\n`);
}

function send(message) {
  const body = Buffer.from(JSON.stringify(message), "utf8");
  const head = Buffer.alloc(4);
  if (LE) head.writeUInt32LE(body.length);
  else head.writeUInt32BE(body.length);
  process.stdout.write(Buffer.concat([head, body]));
}

const startedAt = Date.now();
/** id → { url, left } for downloads that keep expiring. */
const expiring = new Map();

function event(method, params) {
  send({ jsonrpc: "2.0", method, params });
}

function expireLater(id) {
  const entry = expiring.get(id);
  if (!entry) return;
  setTimeout(() => {
    if (entry.left > 0) {
      entry.left--;
      event("event.download.state", { id, state: "needs_refresh", error: { code: "link_expired", message: "expired" } });
    } else {
      event("event.download.state", { id, state: "downloading" });
    }
  }, 200);
}

setInterval(() => {
  const c = control();
  if (typeof c.exitStartedBefore === "number" && startedAt < c.exitStartedBefore) {
    log({ exiting: true });
    process.exit(0);
  }
}, 100);

function handle(msg) {
  log({ argv: process.argv.slice(2), msg });
  if (msg.id === undefined) return;
  const reply = (result) => send({ jsonrpc: "2.0", id: msg.id, result });
  switch (msg.method) {
    case "nm.status":
      return reply({ daemonRunning: true, daemonVersion: "e2e", hostVersion: "fake" });
    case "stats.get":
      return reply({ downloadBps: 0, uploadBps: 0, active: 3, queued: 1 });
    case "download.add": {
      const c = control();
      if (c.mode === "error") {
        return send({ jsonrpc: "2.0", id: msg.id, error: { code: -32007, message: "the whoosh daemon is not running" } });
      }
      const id = `dl_e2e_${msg.id}`;
      const accept = () => {
        reply({ id });
        for (const [needle, times] of Object.entries(c.expire ?? {})) {
          if (String(msg.params?.url).includes(needle)) {
            expiring.set(id, { left: times });
            expireLater(id);
          }
        }
      };
      if (c.mode === "slow") setTimeout(accept, c.delayMs ?? 2000);
      else accept();
      return;
    }
    case "download.refreshUrl":
      reply(null);
      return expireLater(msg.params?.id);
    case "download.list":
      return reply(control().list ?? []);
    default:
      return reply(null);
  }
}

let buffer = Buffer.alloc(0);
process.stdin.on("data", (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  while (buffer.length >= 4) {
    const length = LE ? buffer.readUInt32LE(0) : buffer.readUInt32BE(0);
    if (buffer.length < 4 + length) break;
    const body = buffer.subarray(4, 4 + length).toString("utf8");
    buffer = buffer.subarray(4 + length);
    handle(JSON.parse(body));
  }
});
process.stdin.on("end", () => process.exit(0));
log({ started: true, argv: process.argv.slice(2) });
