// SPDX-License-Identifier: Apache-2.0
// Updates the in-app updater's feed from a published release's latest.json.
//
//   node updater-feed.mjs <release latest.json> <feed dir> <prerelease: true|false>
//
// The feed lives on the `updater` branch and is read by the app from
// raw.githubusercontent.com (tauri.conf.json, plugins.updater.endpoints):
//   latest.json  every published release, pre-releases included (the
//                endpoint the app uses during the beta)
//   stable.json  published releases that are not pre-releases
// A file is only replaced by a newer version, so publishing an older release
// (a backport) never moves the feed back. Prints what changed.

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

/** SemVer precedence: <0, 0 or >0. */
export function compareVersions(a, b) {
  const parse = (v) => {
    const [core, pre] = v.replace(/^v/, "").split("+")[0].split(/-(.*)/s);
    return { core: core.split(".").map(Number), pre: pre ? pre.split(".") : [] };
  };
  const x = parse(a);
  const y = parse(b);
  for (let i = 0; i < 3; i++) {
    const d = (x.core[i] ?? 0) - (y.core[i] ?? 0);
    if (d !== 0) return d;
  }
  if (x.pre.length === 0 || y.pre.length === 0) return y.pre.length - x.pre.length;
  for (let i = 0; i < Math.max(x.pre.length, y.pre.length); i++) {
    const p = x.pre[i];
    const q = y.pre[i];
    if (p === undefined) return -1;
    if (q === undefined) return 1;
    const pn = /^\d+$/.test(p);
    const qn = /^\d+$/.test(q);
    if (pn && qn && Number(p) !== Number(q)) return Number(p) - Number(q);
    if (pn !== qn) return pn ? -1 : 1;
    if (p !== q) return p < q ? -1 : 1;
  }
  return 0;
}

/** Writes `manifest` to `file` unless the file already holds the same or a newer version. */
export function offer(file, manifest) {
  if (existsSync(file)) {
    const current = JSON.parse(readFileSync(file, "utf8")).version;
    if (compareVersions(manifest.version, current) <= 0) return `${file}: keeps ${current}`;
  }
  writeFileSync(file, `${JSON.stringify(manifest, null, 2)}\n`);
  return `${file}: now ${manifest.version}`;
}

function main() {
  const [src, dir, prerelease] = process.argv.slice(2);
  if (!src || !dir || !["true", "false"].includes(prerelease)) {
    console.error("usage: updater-feed.mjs <latest.json> <feed dir> <prerelease: true|false>");
    process.exit(2);
  }
  const manifest = JSON.parse(readFileSync(src, "utf8"));
  if (!manifest.version || !manifest.platforms) throw new Error(`${src} is not an updater manifest`);
  console.log(offer(join(dir, "latest.json"), manifest));
  if (prerelease === "false") console.log(offer(join(dir, "stable.json"), manifest));
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) main();
