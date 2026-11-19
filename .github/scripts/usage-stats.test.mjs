// SPDX-License-Identifier: Apache-2.0
// node --test .github/scripts/usage-stats.test.mjs
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { classify, mergeCsv, run } from "./usage-stats.mjs";

test("installers count, signatures and build files do not", () => {
  const cases = {
    "Whoosh-Download-Manager_1.0.0-beta.1_x64-setup.exe": ["windows", "app"],
    "Whoosh-Download-Manager_1.0.0-beta.1_arm64_portable.zip": ["windows", "app"],
    "Whoosh-Download-Manager_1.0.0-beta.1_universal.dmg": ["macos", "app"],
    "Whoosh-Download-Manager_1.0.0-beta.1_universal.app.tar.gz": ["macos", "update"],
    "Whoosh-Download-Manager_1.0.0-beta.1_amd64.AppImage": ["linux", "app"],
    "Whoosh-Download-Manager_1.0.0-beta.1_arm64.deb": ["linux", "app"],
    "Whoosh-Download-Manager_1.0.0-beta.1_android.apk": ["android", "app"],
    "wdm-chromium-1.0.0-beta.1.zip": ["chromium", "extension"],
    "wdm-1.0.0-beta.1-linux-x64.tar.gz": ["linux", "cli"],
    "wdm-1.0.0-windows-arm64.zip": ["windows", "cli"],
  };
  for (const [name, [platform, kind]] of Object.entries(cases)) assert.deepEqual(classify(name), { platform, kind }, name);
  for (const name of [
    "Whoosh-Download-Manager_1.0.0-beta.1_x64-setup.exe.sig",
    "latest.json",
    "SHA256SUMS",
    "wdm-core-1.0.0-beta.1.cargo.cdx.json",
    "wdm-1.0.0-beta.1-source.tar.gz",
    "wdm-1.0.0-beta.1-package-manifests.tar.gz",
    "Whoosh-Download-Manager_1.0.0-beta.1_android.aab",
    "wdm-firefox-1.0.0-beta.1-store.zip",
    "wdm-nmhost-1.0.0-beta.1-linux-x64.tar.gz",
  ]) assert.equal(classify(name), null, name);
});

test("a second run on the same day replaces that day's rows", () => {
  const h = ["date", "n"];
  let csv = mergeCsv("", h, "2026-11-20", [{ date: "2026-11-20", n: 1 }]);
  csv = mergeCsv(csv, h, "2026-11-21", [{ date: "2026-11-21", n: 2 }]);
  csv = mergeCsv(csv, h, "2026-11-21", [{ date: "2026-11-21", n: 3 }]);
  assert.equal(csv, "date,n\n2026-11-20,1\n2026-11-21,3\n");
});

test("end to end: totals, csv files and readme", () => {
  const dir = mkdtempSync(join(tmpdir(), "wdm-stats-"));
  try {
    const releases = [[
      { tag_name: "v1.0.0-beta.1", draft: false, assets: [
        { name: "Whoosh-Download-Manager_1.0.0-beta.1_x64-setup.exe", download_count: 10 },
        { name: "Whoosh-Download-Manager_1.0.0-beta.1_x64-setup.exe.sig", download_count: 9 },
        { name: "Whoosh-Download-Manager_1.0.0-beta.1_universal.app.tar.gz", download_count: 4 },
        { name: "Whoosh-Download-Manager_1.0.0-beta.1_android.apk", download_count: 5 },
      ] },
      { tag_name: "v1.0.0-beta.2", draft: true, assets: [{ name: "x-setup.exe", download_count: 99 }] },
    ]];
    const rf = join(dir, "releases.json");
    writeFileSync(rf, JSON.stringify(releases));
    const af = join(dir, "active.json");
    writeFileSync(af, JSON.stringify({ days: [
      { day: "2026-11-19", target: "windows", arch: "x86_64", version: "1.0.0-beta.1", checks: 7 },
      { day: "2026-11-19", target: "linux", arch: "x86_64", version: "1.0.0-beta.1", checks: 3 },
    ] }));
    const t = run(rf, dir, "2026-11-20", af);
    assert.equal(t.all, 15);
    assert.deepEqual(t.byPlatform, { windows: 10, android: 5 });
    assert.equal(readFileSync(join(dir, "downloads.csv"), "utf8").trim().split("\n").length, 4);
    const md = readFileSync(join(dir, "README.md"), "utf8");
    assert.match(md, /\*\*15\*\* files/);
    assert.match(md, /\| 2026-11-19 \| 10 \|/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
