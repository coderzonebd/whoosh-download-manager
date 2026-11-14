// The store build never offers video-site downloads (MASTER-PLAN §6): no
// media methods, no yt-dlp, not even their names in the bundle.
import { mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterAll, describe, expect, it } from "vitest";

// @ts-expect-error -- plain ESM build script without type declarations
import { buildExtension } from "../scripts/build.mjs";

const out = mkdtempSync(join(tmpdir(), "whoosh-store-build-"));
afterAll(() => rmSync(out, { recursive: true, force: true }));

describe("store build", () => {
  it("carries nothing of the media (yt-dlp) feature", async () => {
    const dirs: string[] = await buildExtension({ store: true, outDir: out, quiet: true });
    expect(dirs).toHaveLength(2);
    for (const dir of dirs) {
      for (const name of readdirSync(dir).filter((n) => n.endsWith(".js") || n.endsWith(".json") || n.endsWith(".html"))) {
        const text = readFileSync(join(dir, name), "utf8");
        // Method names as they would appear in a request (quoted), and the tool.
        for (const banned of ['"media.probe"', '"media.add"', '"media.tools"', '"media.updateTools"', '"event.media.tools"', "yt-dlp", "ytdlp"]) {
          expect(text.includes(banned), `${name} mentions ${banned}`).toBe(false);
        }
      }
    }
  }, 60_000);
});
