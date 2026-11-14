// SPDX-License-Identifier: Apache-2.0
// Writes the installers' browser integration from extension-ids.json (the
// one place the store IDs live; see README.md next to this file):
//
//   apps/desktop/src-tauri/tauri.browsers.conf.json          merged into Linux release builds
//   apps/desktop/src-tauri/tauri.browsers.windows.conf.json  merged into Windows release builds
//   apps/desktop/src-tauri/installer/windows/         MSI fragment + host manifests
//   apps/desktop/src-tauri/installer/linux/           .deb/.rpm host manifests and
//                                                     external-extension files
//
//   node packaging/browsers/generate.mjs           write the files
//   node packaging/browsers/generate.mjs --check   exit 1 if they are out of date
//
// The NSIS installer (per user) runs `whoosh-nmhost --register --offer-extension`
// instead (installer/windows/hooks.nsh, not generated), and the app itself
// registers for the user on macOS and from an AppImage.

import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

export const HOST_NAME = "com.coderzonebd.whoosh";
const DESCRIPTION = "whoosh download manager — browser integration";
export const EDGE_UPDATE_URL = "https://edge.microsoft.com/extensionwebstorebase/v1/crx";
export const CWS_UPDATE_URL = "https://clients2.google.com/service/update2/crx";
/** Where the .deb and .rpm put the sidecar (Tauri installs externalBin into /usr/bin). */
const LINUX_HOST_PATH = "/usr/bin/whoosh-nmhost";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..");
const tauriDir = join(root, "apps", "desktop", "src-tauri");

const CHROMIUM_ID = /^[a-p]{32}$/;

/** Reads and checks the config (the same rules as whoosh-nmhost's `ids.rs`). */
export function loadIds(text) {
  const ids = JSON.parse(text);
  const chromium = [ids.edge?.id, ids.chromeWebStore?.id].filter((x) => x != null);
  for (const id of [...chromium, ids.github?.chromiumId]) {
    if (typeof id !== "string" || !CHROMIUM_ID.test(id)) throw new Error(`${JSON.stringify(id)} is not a Chromium extension ID`);
  }
  if (typeof ids.firefox?.id !== "string" || ids.firefox.id === "" || /\s/.test(ids.firefox.id)) {
    throw new Error("firefox.id is missing");
  }
  if (ids.chromeWebStore.externalInstall === true && ids.chromeWebStore.id == null) {
    throw new Error("chromeWebStore.externalInstall needs chromeWebStore.id");
  }
  return ids;
}

/** Chromium IDs the host allows: the GitHub build's, then the stores'. */
export function allowedChromiumIds(ids) {
  return [ids.github.chromiumId, ids.edge.id, ids.chromeWebStore.id].filter((x) => x != null);
}

function json(value) {
  return `${JSON.stringify(value, null, 2)}\n`;
}

function chromiumManifest(path, ids) {
  return json({
    name: HOST_NAME,
    description: DESCRIPTION,
    path,
    type: "stdio",
    allowed_origins: allowedChromiumIds(ids).map((id) => `chrome-extension://${id}/`),
  });
}

function firefoxManifest(path, ids) {
  return json({ name: HOST_NAME, description: DESCRIPTION, path, type: "stdio", allowed_extensions: [ids.firefox.id] });
}

/** A stable GUID for a WiX component, changing when its contents change. */
function guid(...parts) {
  const h = createHash("sha256").update(parts.join("\n")).digest("hex");
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-5${h.slice(13, 16)}-a${h.slice(17, 20)}-${h.slice(20, 32)}`.toUpperCase();
}

function xml(s) {
  return s.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;");
}

/** HKLM keys of the per-machine (MSI) install. Must match `detect::system_registry_key`. */
const WINDOWS_HOST_KEYS = [
  ["chromium", String.raw`Software\Google\Chrome`],
  ["chromium", String.raw`Software\Chromium`],
  ["chromium", String.raw`Software\Microsoft\Edge`],
  ["chromium", String.raw`Software\BraveSoftware\Brave-Browser`],
  ["firefox", String.raw`Software\Mozilla`],
];

function wixFragment(ids) {
  const regs = WINDOWS_HOST_KEYS.map(
    ([family, prefix]) => ({ key: `${prefix}\\NativeMessagingHosts\\${HOST_NAME}`, name: undefined, value: `[INSTALLDIR]${HOST_NAME}.${family}.json` }),
  );
  // External extensions (per machine). Microsoft documents the 32-bit view
  // (Wow6432Node) on 64-bit Windows; Chrome's documentation says the same.
  const offers = [];
  if (ids.edge.id) offers.push({ key: `Software\\Wow6432Node\\Microsoft\\Edge\\Extensions\\${ids.edge.id}`, name: "update_url", value: EDGE_UPDATE_URL });
  if (ids.chromeWebStore.externalInstall) {
    offers.push({ key: `Software\\Wow6432Node\\Google\\Chrome\\Extensions\\${ids.chromeWebStore.id}`, name: "update_url", value: CWS_UPDATE_URL });
  }
  const regXml = (r, i) =>
    `        <RegistryValue Root="HKLM" Key="${xml(r.key)}"${r.name ? ` Name="${r.name}"` : ""} Type="string" Value="${xml(r.value)}"${i === 0 ? ' KeyPath="yes"' : ""} />`;
  const lines = [
    `<?xml version="1.0" encoding="utf-8"?>`,
    `<!-- GENERATED by packaging/browsers/generate.mjs from extension-ids.json; do not edit. -->`,
    `<!-- Per-machine browser integration for the MSI: the native messaging host for every user`,
    `     (the manifests are installed next to the app as bundle resources) and the store`,
    `     offers (external extensions). Removed with the product. Tauri renders this file`,
    `     with Handlebars and compiles it in its own output directory. -->`,
    `<Wix xmlns="http://schemas.microsoft.com/wix/2006/wi">`,
    `  <Fragment>`,
    `    <ComponentGroup Id="WhooshBrowserIntegration" Directory="INSTALLDIR">`,
    `      <Component Id="WhooshHostRegistry" Guid="${guid("registry", ...regs.map((r) => r.key))}">`,
    ...regs.map(regXml),
    `      </Component>`,
    ...offers.flatMap((r, i) => [
      `      <Component Id="WhooshStoreOffer${i}" Guid="${guid("offer", r.key)}">`,
      regXml(r, 0),
      `      </Component>`,
    ]),
    `    </ComponentGroup>`,
    `  </Fragment>`,
    `</Wix>`,
  ];
  return `${lines.join("\n")}\n`;
}

/** System-wide host manifest directories. Must match `detect::system_manifest_dirs`. */
const LINUX_HOST_DIRS = [
  ["chromium", "/etc/opt/chrome/native-messaging-hosts"],
  ["chromium", "/etc/chromium/native-messaging-hosts"],
  ["chromium", "/etc/opt/edge/native-messaging-hosts"],
  ["firefox", "/usr/lib/mozilla/native-messaging-hosts"],
];

/** Chromium's standalone external-extension directories on Linux. */
const LINUX_OFFER_DIRS = {
  edge: ["/usr/share/microsoft-edge/extensions"],
  chromeWebStore: ["/usr/share/google-chrome/extensions", "/usr/share/chromium/extensions"],
};

/** Every generated file, relative to apps/desktop/src-tauri, with its text. */
export function generate(ids) {
  const out = new Map();
  // Windows: manifests next to whoosh-nmhost.exe; a relative "path" is
  // resolved against the manifest's directory (Chrome and Firefox both).
  out.set(`installer/windows/${HOST_NAME}.chromium.json`, chromiumManifest("whoosh-nmhost.exe", ids));
  out.set(`installer/windows/${HOST_NAME}.firefox.json`, firefoxManifest("whoosh-nmhost.exe", ids));
  out.set("installer/windows/browsers.wxs", wixFragment(ids));

  out.set("installer/linux/host-chromium.json", chromiumManifest(LINUX_HOST_PATH, ids));
  out.set("installer/linux/host-firefox.json", firefoxManifest(LINUX_HOST_PATH, ids));
  const files = {};
  for (const [family, dir] of LINUX_HOST_DIRS) files[`${dir}/${HOST_NAME}.json`] = `installer/linux/host-${family}.json`;
  const offer = (store, id, url) => {
    const name = `installer/linux/offer-${store}-${id}.json`;
    out.set(name, json({ external_update_url: url }));
    for (const dir of LINUX_OFFER_DIRS[store]) files[`${dir}/${id}.json`] = name;
  };
  if (ids.edge.id) offer("edge", ids.edge.id, EDGE_UPDATE_URL);
  if (ids.chromeWebStore.externalInstall) offer("chromeWebStore", ids.chromeWebStore.id, CWS_UPDATE_URL);

  out.set("tauri.browsers.conf.json", json({ $schema: "https://schema.tauri.app/config/2", bundle: { linux: { deb: { files }, rpm: { files } } } }));
  // The same list for packages built without Tauri's bundler (AUR):
  // "<destination> <source relative to src-tauri>" per line.
  out.set(
    "installer/linux/files.txt",
    `${Object.entries(files)
      .map(([to, from]) => `${to} ${from}`)
      .join("\n")}\n`,
  );
  const manifests = [`${HOST_NAME}.chromium.json`, `${HOST_NAME}.firefox.json`];
  out.set(
    "tauri.browsers.windows.conf.json",
    json({
      $schema: "https://schema.tauri.app/config/2",
      bundle: {
        // Next to whoosh-nmhost.exe (the resource directory is the install directory).
        resources: Object.fromEntries(manifests.map((m) => [`installer/windows/${m}`, m])),
        windows: {
          nsis: { installerHooks: "installer/windows/hooks.nsh" },
          wix: { fragmentPaths: ["installer/windows/browsers.wxs"], componentGroupRefs: ["WhooshBrowserIntegration"] },
        },
      },
    }),
  );
  return out;
}

/** Generated files present on disk that `generate` no longer makes (an ID removed). */
function stale(expected) {
  const dir = join(tauriDir, "installer", "linux");
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .map((f) => `installer/linux/${f}`)
    .filter((f) => !expected.has(f));
}

function main() {
  const check = process.argv.includes("--check");
  const ids = loadIds(readFileSync(join(here, "extension-ids.json"), "utf8"));
  const files = generate(ids);
  const bad = [];
  for (const [rel, text] of files) {
    const path = join(tauriDir, rel);
    const now = existsSync(path) ? readFileSync(path, "utf8") : undefined;
    if (now === text) continue;
    if (check) bad.push(rel);
    else {
      mkdirSync(dirname(path), { recursive: true });
      writeFileSync(path, text);
      console.log(`browsers: wrote ${relative(root, path)}`);
    }
  }
  for (const rel of stale(files)) {
    if (check) bad.push(`${rel} (stale)`);
    else {
      rmSync(join(tauriDir, rel));
      console.log(`browsers: removed ${rel}`);
    }
  }
  if (bad.length > 0) {
    console.error(`browsers: out of date, run node packaging/browsers/generate.mjs:\n  ${bad.join("\n  ")}`);
    process.exit(1);
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) main();
