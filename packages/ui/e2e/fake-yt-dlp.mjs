#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// A fake yt-dlp for the end-to-end suite, placed where the daemon keeps its
// managed copy (<data>/media/bin/yt-dlp). It answers `--version` and `-J`
// with the media crate's fixture (crates/media/tests/fixtures/info.json),
// with the labserver's address filled in. Enough for a probe; it never
// downloads anything.
import { readFileSync } from "node:fs";

const args = process.argv.slice(2);
if (args.includes("--version")) {
  console.log("2026.10.26");
  process.exit(0);
}
const page = args[args.indexOf("--") + 1] ?? "";
if (!args.includes("-J") || page === "") {
  console.error("ERROR: the fake yt-dlp only answers -J");
  process.exit(2);
}
const text = readFileSync(process.env.WHOOSH_FAKE_FIXTURE ?? "", "utf8");
const base = process.env.WHOOSH_FAKE_BASE ?? "";
// One line, like yt-dlp: the daemon reads the first line of stdout.
const info = JSON.parse(text.replaceAll("{BASE}", base).replaceAll("{PAGE}", page));
process.stdout.write(`${JSON.stringify(info)}\n`);
