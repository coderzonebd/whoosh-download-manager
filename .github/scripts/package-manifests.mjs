// SPDX-License-Identifier: Apache-2.0
// Fills the package-manager manifests in packaging/ with a release's real
// version, download URLs and SHA-256 hashes, ready to submit by hand
// (each packaging/<channel>/README.md says how). Submits nothing.
//
//   node package-manifests.mjs <assets-dir> <version> <out-dir>
//
// GITHUB_REPOSITORY (owner/name) sets the download URLs (default:
// coderzonebd/whoosh-download-manager); SOURCE_DATE_EPOCH the release date.
// Writes <out-dir>/{winget,scoop,homebrew,flatpak,aur,linux}/ and a README.
// A channel whose installer is missing from <assets-dir> is skipped with a
// warning (a pre-release has no MSI; a failed matrix leg has no files).

import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const packaging = join(root, "packaging");

export function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

/** The release's installers by role, with URL and hash; missing ones are undefined. */
export function findAssets(dir, version, base) {
  const names = {
    nsisX64: `Whoosh_${version}_x64-setup.exe`,
    nsisArm64: `Whoosh_${version}_arm64-setup.exe`,
    msiX64: `Whoosh_${version}_x64_en-US.msi`,
    portableX64: `Whoosh_${version}_x64_portable.zip`,
    portableArm64: `Whoosh_${version}_arm64_portable.zip`,
    dmg: `Whoosh_${version}_universal.dmg`,
    debAmd64: `Whoosh_${version}_amd64.deb`,
    debArm64: `Whoosh_${version}_arm64.deb`,
    source: `whoosh-${version}-source.tar.gz`,
  };
  const assets = {};
  for (const [role, name] of Object.entries(names)) {
    const path = join(dir, name);
    assets[role] = existsSync(path) ? { name, url: `${base}/${encodeURIComponent(name)}`, sha256: sha256(path) } : undefined;
  }
  return assets;
}

/** Drops the template's "TEMPLATE" comment lines. */
function untemplate(text) {
  return text
    .split("\n")
    .filter((l) => !/^\s*# TEMPLATE/.test(l))
    .join("\n");
}

function read(rel) {
  return readFileSync(join(packaging, rel), "utf8");
}

export function winget({ version, date, repo, assets }) {
  const installers = [];
  const nsis = (arch, a) =>
    a && installers.push(`  # NSIS, per user (no admin); Tauri's installer takes /S for silent installs.
  - Architecture: ${arch}
    InstallerType: nullsoft
    Scope: user
    InstallerUrl: ${a.url}
    InstallerSha256: ${a.sha256.toUpperCase()}`);
  nsis("x64", assets.nsisX64);
  nsis("arm64", assets.nsisArm64);
  if (assets.msiX64) {
    installers.push(`  # MSI (WiX), per machine.
  - Architecture: x64
    InstallerType: wix
    Scope: machine
    InstallerUrl: ${assets.msiX64.url}
    InstallerSha256: ${assets.msiX64.sha256.toUpperCase()}`);
  }
  if (installers.length === 0) return undefined;
  const installer = `# yaml-language-server: $schema=https://aka.ms/winget-manifest.installer.1.10.0.schema.json
PackageIdentifier: CoderZoneBD.Whoosh
PackageVersion: ${version}
MinimumOSVersion: 10.0.17763.0
Platform:
  - Windows.Desktop
InstallerLocale: en-US
ReleaseDate: ${date}
UpgradeBehavior: install
Installers:
${installers.join("\n")}
ManifestType: installer
ManifestVersion: 1.10.0
`;
  const fill = (t) => untemplate(t).replaceAll("0.0.1", version).replaceAll("coderzonebd/whoosh-download-manager", repo);
  return {
    "CoderZoneBD.Whoosh.yaml": fill(read("winget/CoderZoneBD.Whoosh.yaml")),
    "CoderZoneBD.Whoosh.installer.yaml": installer,
    "CoderZoneBD.Whoosh.locale.en-US.yaml": fill(read("winget/CoderZoneBD.Whoosh.locale.en-US.yaml")),
  };
}

export function scoop({ version, repo, assets }) {
  if (!assets.portableX64) return undefined;
  const m = JSON.parse(read("scoop/whoosh.json"));
  m.version = version;
  m.description = "Fast, open-source download manager";
  m.homepage = `https://github.com/${repo}`;
  m.architecture = { "64bit": { url: assets.portableX64.url, hash: assets.portableX64.sha256 } };
  if (assets.portableArm64) m.architecture.arm64 = { url: assets.portableArm64.url, hash: assets.portableArm64.sha256 };
  m.checkver = { github: `https://github.com/${repo}` };
  const auto = (arch) => ({
    url: `https://github.com/${repo}/releases/download/v$version/Whoosh_$version_${arch}_portable.zip`,
    hash: { url: "$baseurl/SHA256SUMS", regex: "$sha256\\s+\\*?$basename" },
  });
  m.autoupdate = { architecture: { "64bit": auto("x64"), ...(assets.portableArm64 ? { arm64: auto("arm64") } : {}) } };
  return { "whoosh.json": `${JSON.stringify(m, null, 4)}\n` };
}

export function homebrew({ version, repo, assets }) {
  if (!assets.dmg) return undefined;
  const text = untemplate(read("homebrew/whoosh.rb"))
    .replace(/^ {2}version "[^"]*"$/m, `  version "${version}"`)
    .replace(/^ {2}sha256 "[0-9a-f]{64}"$/m, `  sha256 "${assets.dmg.sha256}"`)
    .replaceAll("coderzonebd/whoosh-download-manager", repo);
  return { "whoosh.rb": text };
}

export function flatpak({ repo, assets }) {
  if (!assets.debAmd64) return undefined;
  const sources = [
    ["x86_64", assets.debAmd64],
    ["aarch64", assets.debArm64],
  ]
    .filter(([, a]) => a)
    .map(
      ([arch, a]) => `      - type: file
        dest-filename: whoosh.deb
        only-arches: [${arch}]
        url: ${a.url}
        sha256: "${a.sha256}"`,
    )
    .join("\n");
  const text = untemplate(read("flatpak/com.coderzonebd.whoosh.yml"))
    .replace(/ {6}- type: file\n {8}dest-filename: whoosh\.deb\n(?: {8}.*\n)+?(?= {6}- type: file\n {8}path:)/, `${sources}\n`)
    .replaceAll("coderzonebd/whoosh-download-manager", repo);
  if (!text.includes(assets.debAmd64.sha256)) throw new Error("flatpak: the template's .deb source was not found");
  return { "com.coderzonebd.whoosh.yml": text };
}

export function aur({ version, repo, assets }) {
  if (!assets.source) return undefined;
  const pkgver = version.replaceAll("-", "_");
  const pkgbuild = read("aur/PKGBUILD")
    .replace(/^_version=.*$/m, `_version=${version}`)
    .replace(/^sha256sums=\('SKIP'\)$/m, `sha256sums=('${assets.source.sha256}')`)
    .replaceAll("coderzonebd/whoosh-download-manager", repo);
  if (!pkgbuild.includes(assets.source.sha256)) throw new Error("aur: sha256sums line not found in the template");
  // What `makepkg --printsrcinfo` prints for this PKGBUILD.
  const field = (name) => {
    const m = new RegExp(`^${name}=\\(([^)]*)\\)`, "ms").exec(pkgbuild);
    return m ? [...m[1].matchAll(/'([^']*)'/g)].map((x) => x[1]) : [];
  };
  const scalar = (name) => new RegExp(`^${name}=(.*)$`, "m").exec(pkgbuild)?.[1].replace(/^'|'$/g, "") ?? "";
  const url = scalar("url");
  const lines = [
    "pkgbase = whoosh",
    `\tpkgdesc = ${scalar("pkgdesc")}`,
    `\tpkgver = ${pkgver}`,
    `\tpkgrel = ${scalar("pkgrel")}`,
    `\turl = ${url}`,
    ...field("arch").map((a) => `\tarch = ${a}`),
    ...field("license").map((l) => `\tlicense = ${l}`),
    ...field("makedepends").map((d) => `\tmakedepends = ${d}`),
    ...field("depends").map((d) => `\tdepends = ${d}`),
    ...field("options").map((o) => `\toptions = ${o}`),
    `\tsource = whoosh-${version}.tar.gz::${url}/releases/download/v${version}/whoosh-${version}-source.tar.gz`,
    `\tsha256sums = ${assets.source.sha256}`,
    "",
    "pkgname = whoosh",
    "",
  ];
  return { PKGBUILD: pkgbuild, ".SRCINFO": lines.join("\n") };
}

function main() {
  const [dir, version, out] = process.argv.slice(2);
  if (!dir || !version || !out) {
    console.error("usage: package-manifests.mjs <assets-dir> <version> <out-dir>");
    process.exit(2);
  }
  const repo = process.env.GITHUB_REPOSITORY || "coderzonebd/whoosh-download-manager";
  const base = `https://github.com/${repo}/releases/download/v${version}`;
  const epoch = Number(process.env.SOURCE_DATE_EPOCH);
  const date = new Date(Number.isFinite(epoch) && epoch > 0 ? epoch * 1000 : Date.now()).toISOString().slice(0, 10);
  const ctx = { version, date, repo, assets: findAssets(dir, version, base) };

  const channels = { winget, scoop, homebrew, flatpak, aur };
  const made = [];
  for (const [name, make] of Object.entries(channels)) {
    const files = make(ctx);
    if (!files) {
      console.warn(`::warning::${name}: its installer is not among the assets; skipped`);
      continue;
    }
    mkdirSync(join(out, name), { recursive: true });
    for (const [file, text] of Object.entries(files)) writeFileSync(join(out, name, file), text);
    made.push(name);
  }
  // The Flatpak manifest refers to ../linux/ for the desktop entry and metainfo.
  if (made.includes("flatpak")) {
    mkdirSync(join(out, "linux"), { recursive: true });
    for (const f of ["com.coderzonebd.whoosh.desktop", "com.coderzonebd.whoosh.metainfo.xml"]) {
      copyFileSync(join(packaging, "linux", f), join(out, "linux", f));
    }
  }
  writeFileSync(
    join(out, "README.md"),
    `# Package manifests for whoosh ${version}

Generated by \`.github/scripts/package-manifests.mjs\` from the release assets
(URLs under ${base}/, hashes as in SHA256SUMS). Nothing was submitted.
How to submit each one: \`packaging/<channel>/README.md\` in the repository.

${made.map((m) => `- \`${m}/\``).join("\n")}
`,
  );
  console.log(`package manifests for ${version}: ${made.join(", ") || "none"}`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) main();
