// SPDX-License-Identifier: Apache-2.0
// node --test .github/scripts/package-manifests.test.mjs
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { aur, findAssets, flatpak, homebrew, scoop, sha256, winget } from "./package-manifests.mjs";

const V = "1.2.0-beta.3";
const REPO = "example/whoosh";
const BASE = `https://github.com/${REPO}/releases/download/v${V}`;

function release(names) {
  const dir = mkdtempSync(join(tmpdir(), "whoosh-pm-"));
  for (const n of names) writeFileSync(join(dir, n), n);
  return { dir, ctx: { version: V, date: "2026-11-14", repo: REPO, assets: findAssets(dir, V, BASE) } };
}

test("every channel gets the real version, URLs and hashes", () => {
  const { dir, ctx } = release([
    `Whoosh_${V}_x64-setup.exe`,
    `Whoosh_${V}_x64_portable.zip`,
    `Whoosh_${V}_universal.dmg`,
    `Whoosh_${V}_amd64.deb`,
    `whoosh-${V}-source.tar.gz`,
  ]);
  try {
    const nsis = sha256(join(dir, `Whoosh_${V}_x64-setup.exe`));
    const w = winget(ctx);
    assert.match(w["CoderZoneBD.Whoosh.installer.yaml"], new RegExp(`InstallerSha256: ${nsis.toUpperCase()}`));
    assert.ok(!w["CoderZoneBD.Whoosh.installer.yaml"].includes("wix"), "no MSI for a pre-release");
    assert.ok(w["CoderZoneBD.Whoosh.locale.en-US.yaml"].includes(`PackageVersion: ${V}`));
    assert.ok(!w["CoderZoneBD.Whoosh.locale.en-US.yaml"].includes("TEMPLATE"));

    const s = JSON.parse(scoop(ctx)["whoosh.json"]);
    assert.equal(s.version, V);
    assert.equal(s.architecture["64bit"].url, `${BASE}/Whoosh_${V}_x64_portable.zip`);
    assert.equal(s.architecture.arm64, undefined);

    const cask = homebrew(ctx)["whoosh.rb"];
    assert.ok(cask.includes(`version "${V}"`) && cask.includes(sha256(join(dir, `Whoosh_${V}_universal.dmg`))));

    const fp = flatpak(ctx)["com.coderzonebd.whoosh.yml"];
    assert.ok(fp.includes(`url: ${BASE}/Whoosh_${V}_amd64.deb`));
    assert.ok(!fp.includes("0000000000000000"));

    const a = aur(ctx);
    assert.match(a.PKGBUILD, /^_version=1\.2\.0-beta\.3$/m);
    assert.ok(a[".SRCINFO"].includes("pkgver = 1.2.0_beta.3"));
    assert.ok(a[".SRCINFO"].includes(`sha256sums = ${sha256(join(dir, `whoosh-${V}-source.tar.gz`))}`));
  } finally {
    rmSync(dir, { recursive: true });
  }
});

test("a channel without its installer is skipped", () => {
  const { dir, ctx } = release([`Whoosh_${V}_amd64.deb`]);
  try {
    assert.equal(winget(ctx), undefined);
    assert.equal(scoop(ctx), undefined);
    assert.equal(homebrew(ctx), undefined);
    assert.equal(aur(ctx), undefined);
    assert.ok(flatpak(ctx));
  } finally {
    rmSync(dir, { recursive: true });
  }
});
