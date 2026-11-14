// SPDX-License-Identifier: GPL-3.0-or-later
// Builds dist/chromium and dist/firefox from one code base.
//
//   node scripts/build.mjs [--target chromium|firefox] [--store] [--zip] [--out DIR]
//
// --store  the Chrome Web Store / AMO build: no development `key`, and the
//          video-site blocklist applied (src/shared/policy.ts, MASTER-PLAN §6)
// --zip    also write <out>/whoosh-<target>-<version>[-store].zip
// --out    output directory (default: dist)

import { copyFileSync, cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";

import { build } from "rolldown";

import { iconPng } from "./icons.mjs";
import { manifestFor } from "./manifest.mjs";
import { zipDirectory } from "./zip.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

const ENTRIES = {
  background: "src/background/index.ts",
  content: "src/content/index.ts",
  popup: "src/popup/popup.ts",
  options: "src/options/options.ts",
};

/** Static files copied as they are. */
const STATIC = {
  "popup.html": "src/popup/popup.html",
  "popup.css": "src/popup/popup.css",
  "options.html": "src/options/options.html",
  "options.css": "src/options/options.css",
};

/** A path as short as it can be shown from the current directory. */
function shown(path) {
  const rel = relative(process.cwd(), path);
  return rel && !rel.startsWith("..") ? rel : path;
}

/** Compile-time constants: `__WHOOSH_STORE__` switches the store policy on. */
export function defineFor(store) {
  return { __WHOOSH_STORE__: store ? "true" : "false" };
}

/**
 * @param {{ targets?: string[], store?: boolean, zip?: boolean, outDir?: string, quiet?: boolean }} options
 * @returns {Promise<string[]>} the directories written
 */
export async function buildExtension({ targets = ["chromium", "firefox"], store = false, zip = false, outDir, quiet = false } = {}) {
  for (const t of targets) {
    if (t !== "chromium" && t !== "firefox") throw new Error(`unknown target ${t}`);
  }
  const dist = resolve(outDir ?? join(root, "dist"));
  const { version } = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
  const written = [];

  for (const target of targets) {
    const out = join(dist, target);
    rmSync(out, { recursive: true, force: true });
    mkdirSync(join(out, "icons"), { recursive: true });

    // Each script is a classic script (IIFE): content scripts cannot be
    // modules, and Firefox event pages are simplest without them.
    for (const [name, input] of Object.entries(ENTRIES)) {
      await build({
        input: join(root, input),
        platform: "browser",
        logLevel: "warn",
        transform: { define: defineFor(store) },
        output: { file: join(out, `${name}.js`), format: "iife", minify: false, sourcemap: false },
      });
    }

    for (const [to, from] of Object.entries(STATIC)) copyFileSync(join(root, from), join(out, to));
    // Translations (WebExtension i18n): _locales/<lang>/messages.json.
    cpSync(join(root, "_locales"), join(out, "_locales"), { recursive: true });
    for (const size of [16, 32, 48, 128]) writeFileSync(join(out, "icons", `${size}.png`), iconPng(size));
    const manifest = manifestFor(target, { version, store });
    writeFileSync(join(out, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);

    let line = `built ${shown(out)}${store ? " (store)" : ""}`;
    if (zip) {
      const zipName = `whoosh-${target}-${version}${store ? "-store" : ""}.zip`;
      const count = zipDirectory(out, join(dist, zipName));
      line += ` → ${shown(join(dist, zipName))} (${count} files)`;
    }
    if (!quiet) console.log(line);
    written.push(out);
  }
  return written;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const { values } = parseArgs({
    options: {
      target: { type: "string" },
      store: { type: "boolean", default: false },
      zip: { type: "boolean", default: false },
      out: { type: "string" },
    },
  });
  await buildExtension({
    targets: values.target ? [values.target] : undefined,
    store: values.store,
    zip: values.zip,
    outDir: values.out,
  });
}
