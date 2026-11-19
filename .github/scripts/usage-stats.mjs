// SPDX-License-Identifier: Apache-2.0
// Records how often WDM is downloaded, and how often installed copies check
// for updates, on the `stats` branch (.github/workflows/usage-stats.yml).
//
//   node usage-stats.mjs <releases.json> <stats dir> <date YYYY-MM-DD> [active.json]
//
// releases.json  `gh api repos/{owner}/{repo}/releases --paginate | jq -s .`
//                (an array of pages, or a flat array of releases)
// active.json    the update feed's /stats answer (infra/update-feed), if any
//
// Writes into <stats dir>:
//   downloads.csv  date,tag,asset,platform,kind,count: one row per counted
//                  file per day; counts are GitHub's running totals
//   active.csv     date,target,arch,version,checks: update checks per day
//   README.md      the latest totals, readable on GitHub
// Running it twice on the same date replaces that date's rows.

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * What a release file is, or null for files that are not something a person
 * installs (signatures, checksums, SBOMs, the updater manifest, store uploads).
 * Windows and Linux in-app updates download the same installer a new user
 * does, so `app` counts include updates; only macOS has its own update file.
 */
export function classify(name) {
  const n = name.toLowerCase();
  if (/\.(sig|json|sha256|txt)$|sha256sums|cdx\.json$|-source\.|package-manifests|\.aab$|-store\.zip$|nmhost/.test(n)) return null;
  if (n.endsWith("-setup.exe") || n.endsWith("_portable.zip") || n.endsWith(".msi")) return { platform: "windows", kind: "app" };
  if (n.endsWith(".dmg")) return { platform: "macos", kind: "app" };
  if (n.endsWith(".app.tar.gz")) return { platform: "macos", kind: "update" };
  if (n.endsWith(".appimage") || n.endsWith(".deb") || n.endsWith(".rpm")) return { platform: "linux", kind: "app" };
  if (n.endsWith(".apk")) return { platform: "android", kind: "app" };
  const ext = n.match(/^wdm-(chromium|firefox)-/);
  if (ext) return { platform: ext[1], kind: "extension" };
  const cli = n.match(/^wdm-[\d.]+(?:-[0-9a-z.]+)?-(linux|macos|windows)-[a-z0-9]+\.(tar\.gz|zip)$/);
  if (cli) return { platform: cli[1], kind: "cli" };
  return null;
}

/** One row per counted file of every published release. */
export function downloadRows(releases, date) {
  const flat = releases.flat();
  const rows = [];
  for (const r of flat) {
    if (r.draft) continue;
    for (const a of r.assets ?? []) {
      const c = classify(a.name);
      if (c) rows.push({ date, tag: r.tag_name, asset: a.name, platform: c.platform, kind: c.kind, count: a.download_count });
    }
  }
  return rows.sort((a, b) => a.tag.localeCompare(b.tag) || a.asset.localeCompare(b.asset));
}

/** Totals by platform and kind. */
export function totals(rows) {
  const t = { all: 0, byPlatform: {}, byKind: {} };
  for (const r of rows) {
    if (r.kind === "update") continue; // macOS update payloads are not new users
    t.all += r.count;
    t.byPlatform[r.platform] = (t.byPlatform[r.platform] ?? 0) + r.count;
    t.byKind[r.kind] = (t.byKind[r.kind] ?? 0) + r.count;
  }
  return t;
}

const csvCell = (v) => (/[",\n]/.test(String(v)) ? `"${String(v).replaceAll('"', '""')}"` : String(v));

/** Replaces `date`'s rows in a CSV file (first column is the date). */
export function mergeCsv(existing, header, date, rows) {
  const keep = existing ? existing.trim().split("\n").slice(1).filter((l) => l && !l.startsWith(`${date},`)) : [];
  const fresh = rows.map((r) => header.map((h) => csvCell(r[h])).join(","));
  return [header.join(","), ...keep, ...fresh].join("\n") + "\n";
}

/** Rows for active.csv from the feed's /stats answer ({ days: [{ day, target, arch, version, checks }] }). */
export function activeRows(active) {
  return (active?.days ?? []).map((d) => ({ date: d.day, target: d.target, arch: d.arch, version: d.version, checks: d.checks }));
}

/** Update checks per day over all platforms, newest first. */
export function checksPerDay(rows) {
  const m = new Map();
  for (const r of rows) m.set(r.date, (m.get(r.date) ?? 0) + Number(r.checks));
  return [...m.entries()].sort((a, b) => b[0].localeCompare(a[0]));
}

export function readme(date, t, perDay) {
  const lines = [
    "# WDM usage numbers",
    "",
    `Updated ${date} by \`.github/workflows/usage-stats.yml\`. Nothing here identifies a person: downloads are GitHub's counters, and active copies are counted from update checks without IP addresses or IDs (docs/privacy.md).`,
    "",
    "## Downloads (all releases)",
    "",
    `**${t.all.toLocaleString("en-US")}** files downloaded from the releases page. Windows and Linux in-app updates download the installer too, so these include updates.`,
    "",
    "| Platform | Downloads |",
    "|---|---:|",
    ...Object.entries(t.byPlatform).sort((a, b) => b[1] - a[1]).map(([p, n]) => `| ${p} | ${n.toLocaleString("en-US")} |`),
    "",
  ];
  if (perDay.length) {
    lines.push(
      "## Desktop copies in use",
      "",
      "The desktop app checks for updates once a day, so update checks per day are close to the number of copies running that day. Copies that turned the daily check off, or that a package manager updates, are not counted.",
      "",
      "| Day (UTC) | Update checks |",
      "|---|---:|",
      ...perDay.slice(0, 30).map(([d, n]) => `| ${d} | ${n.toLocaleString("en-US")} |`),
      "",
    );
  }
  lines.push("Raw data: [`downloads.csv`](downloads.csv), [`active.csv`](active.csv).", "");
  return lines.join("\n");
}

export function run(releasesFile, dir, date, activeFile) {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(date)) throw new Error(`bad date: ${date}`);
  const releases = JSON.parse(readFileSync(releasesFile, "utf8"));
  const rows = downloadRows(releases, date);
  const dlFile = join(dir, "downloads.csv");
  writeFileSync(dlFile, mergeCsv(existsSync(dlFile) ? readFileSync(dlFile, "utf8") : "", ["date", "tag", "asset", "platform", "kind", "count"], date, rows));

  const acFile = join(dir, "active.csv");
  let acText = existsSync(acFile) ? readFileSync(acFile, "utf8") : "";
  if (activeFile && existsSync(activeFile)) {
    const fresh = activeRows(JSON.parse(readFileSync(activeFile, "utf8")));
    // The feed reports a window of recent days; each reported day replaces its old rows.
    for (const day of new Set(fresh.map((r) => r.date))) {
      acText = mergeCsv(acText, ["date", "target", "arch", "version", "checks"], day, fresh.filter((r) => r.date === day));
    }
    writeFileSync(acFile, acText);
  }
  const acRows = acText
    ? acText.trim().split("\n").slice(1).map((l) => { const [d, , , , c] = l.split(","); return { date: d, checks: c }; })
    : [];
  const t = totals(rows);
  writeFileSync(join(dir, "README.md"), readme(date, t, checksPerDay(acRows)));
  return t;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const [releasesFile, dir, date, activeFile] = process.argv.slice(2);
  const t = run(releasesFile, dir, date, activeFile);
  console.log(`downloads: ${t.all} ${JSON.stringify(t.byPlatform)}`);
}
