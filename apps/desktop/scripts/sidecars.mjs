// SPDX-License-Identifier: GPL-3.0-or-later
// Builds the programs the installers ship next to the app (Tauri
// `externalBin`, see src-tauri/tauri.sidecars.conf.json) and puts them where
// Tauri looks: src-tauri/binaries/<name>-<target triple>[.exe].
//
//   node scripts/sidecars.mjs [--target <triple>]   (default: this machine)
//
// `universal-apple-darwin` builds both macOS architectures and joins them
// with lipo. Release builds only: `tauri dev` and `cargo build` find the
// daemon next to the app in target/<profile>/ without this.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const NAMES = ["whoosh-daemon", "whoosh-nmhost"];
const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const out = join(root, "apps", "desktop", "src-tauri", "binaries");

function run(cmd, args) {
  console.log(`sidecars: ${cmd} ${args.join(" ")}`);
  execFileSync(cmd, args, { cwd: root, stdio: "inherit" });
}

function hostTriple() {
  const v = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  const line = v.split("\n").find((l) => l.startsWith("host: "));
  if (!line) throw new Error("rustc -vV printed no host triple");
  return line.slice("host: ".length).trim();
}

function build(triple) {
  run("cargo", ["build", "--release", "--locked", "--target", triple, ...NAMES.flatMap((n) => ["-p", n])]);
}

const i = process.argv.indexOf("--target");
const target = i > 0 ? process.argv[i + 1] : hostTriple();
const exe = target.includes("windows") ? ".exe" : "";
mkdirSync(out, { recursive: true });

if (target === "universal-apple-darwin") {
  const arches = ["aarch64-apple-darwin", "x86_64-apple-darwin"];
  for (const t of arches) build(t);
  for (const name of NAMES) {
    const parts = arches.map((t) => join(root, "target", t, "release", name));
    run("lipo", ["-create", "-output", join(out, `${name}-${target}`), ...parts]);
  }
} else {
  build(target);
  for (const name of NAMES) {
    copyFileSync(join(root, "target", target, "release", `${name}${exe}`), join(out, `${name}-${target}${exe}`));
  }
}
console.log(`sidecars: ready in ${out}`);
