// SPDX-License-Identifier: Apache-2.0
// Writes the Tauri updater manifest (latest.json) for the release assets
// that have a minisign signature next to them (<asset>.sig).
//
//   node updater-manifest.mjs <assets-dir> <version> <download-base-url>
//
// download-base-url: https://github.com/<owner>/<repo>/releases/download/<tag>
// Does nothing (exit 0) when no .sig files exist, i.e. the updater key secret
// is not configured.

import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [dir, version, baseUrl] = process.argv.slice(2);
if (!dir || !version || !baseUrl) {
  console.error("usage: updater-manifest.mjs <assets-dir> <version> <download-base-url>");
  process.exit(2);
}

const ARCH = { amd64: "x86_64", x86_64: "x86_64", x64: "x86_64", arm64: "aarch64", aarch64: "aarch64" };

/** Updater platform keys an asset serves. The first key per OS is the generic fallback. */
function platformsFor(name) {
  let m;
  if (name.endsWith(".app.tar.gz")) {
    const arch = /_(aarch64|x64|x86_64)\.app\.tar\.gz$/.exec(name)?.[1];
    const archs = arch ? [ARCH[arch]] : ["aarch64", "x86_64"]; // universal
    return archs.flatMap((a) => [`darwin-${a}`, `darwin-${a}-app`]);
  }
  if ((m = /_(amd64|aarch64|arm64)\.AppImage$/.exec(name))) {
    const a = ARCH[m[1]];
    return [`linux-${a}`, `linux-${a}-appimage`];
  }
  if ((m = /_(amd64|arm64)\.deb$/.exec(name))) return [`linux-${ARCH[m[1]]}-deb`];
  if ((m = /\.(x86_64|aarch64)\.rpm$/.exec(name))) return [`linux-${ARCH[m[1]]}-rpm`];
  if ((m = /_(x64|arm64)-setup\.exe$/.exec(name))) {
    const a = ARCH[m[1]];
    return [`windows-${a}`, `windows-${a}-nsis`];
  }
  if ((m = /_(x64|arm64)_[A-Za-z-]+\.msi$/.exec(name))) return [`windows-${ARCH[m[1]]}-msi`];
  return [];
}

const platforms = {};
for (const sig of readdirSync(dir).filter((f) => f.endsWith(".sig")).sort()) {
  const asset = sig.slice(0, -".sig".length);
  if (!existsSync(join(dir, asset))) {
    console.error(`${sig}: no matching asset`);
    process.exit(1);
  }
  const keys = platformsFor(asset);
  if (keys.length === 0) {
    console.warn(`${asset}: not an updater bundle, skipped`);
    continue;
  }
  const entry = {
    signature: readFileSync(join(dir, sig), "utf8").trim(),
    url: `${baseUrl.replace(/\/$/, "")}/${encodeURIComponent(asset)}`,
  };
  for (const key of keys) {
    // Generic keys (e.g. "windows-x86_64") go to the first asset that claims them.
    if (!platforms[key]) platforms[key] = entry;
  }
}

if (Object.keys(platforms).length === 0) {
  console.log("no updater signatures found; latest.json not written");
  process.exit(0);
}

const epoch = Number(process.env.SOURCE_DATE_EPOCH);
const manifest = {
  version,
  notes: `See ${baseUrl.replace("/releases/download/", "/releases/tag/")}`,
  pub_date: new Date(Number.isFinite(epoch) && epoch > 0 ? epoch * 1000 : Date.now()).toISOString(),
  platforms: Object.fromEntries(Object.entries(platforms).sort(([a], [b]) => a.localeCompare(b))),
};
writeFileSync(join(dir, "latest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
console.log(`latest.json: ${Object.keys(platforms).join(", ")}`);
