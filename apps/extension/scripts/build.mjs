// SPDX-License-Identifier: GPL-3.0-or-later
// Builds dist/chromium and dist/firefox from one code base.
//
//   node scripts/build.mjs [--target chromium|firefox] [--store] [--zip]
//
// --store  omit the development `key` (store uploads must not carry it)
// --zip    also write dist/vayu-<target>-<version>[-store].zip

import { copyFileSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

import { build } from "rolldown";

import { iconPng } from "./icons.mjs";
import { manifestFor } from "./manifest.mjs";
import { zipDirectory } from "./zip.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const { values } = parseArgs({
  options: {
    target: { type: "string" },
    store: { type: "boolean", default: false },
    zip: { type: "boolean", default: false },
  },
});
const targets = values.target ? [values.target] : ["chromium", "firefox"];
for (const t of targets) {
  if (t !== "chromium" && t !== "firefox") throw new Error(`unknown target ${t}`);
}
const { version } = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));

const entries = {
  background: "src/background/index.ts",
  content: "src/content/index.ts",
  popup: "src/popup/popup.ts",
};

for (const target of targets) {
  const out = join(root, "dist", target);
  rmSync(out, { recursive: true, force: true });
  mkdirSync(join(out, "icons"), { recursive: true });

  // Each script is a classic script (IIFE): content scripts cannot be
  // modules, and Firefox event pages are simplest without them.
  for (const [name, input] of Object.entries(entries)) {
    await build({
      input: join(root, input),
      platform: "browser",
      logLevel: "warn",
      output: { file: join(out, `${name}.js`), format: "iife", minify: false, sourcemap: false },
    });
  }

  copyFileSync(join(root, "src/popup/popup.html"), join(out, "popup.html"));
  copyFileSync(join(root, "src/popup/popup.css"), join(out, "popup.css"));
  for (const size of [16, 32, 48, 128]) writeFileSync(join(out, "icons", `${size}.png`), iconPng(size));
  const manifest = manifestFor(target, { version, store: values.store });
  writeFileSync(join(out, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);

  let line = `built dist/${target}`;
  if (values.zip) {
    const zipName = `vayu-${target}-${version}${values.store ? "-store" : ""}.zip`;
    const count = zipDirectory(out, join(root, "dist", zipName));
    line += ` → dist/${zipName} (${count} files)`;
  }
  console.log(line);
}
