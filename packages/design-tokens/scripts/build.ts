// SPDX-License-Identifier: Apache-2.0
// Regenerates src/tokens.css from src/tokens.ts. Run: pnpm --filter @whoosh/design-tokens build
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { toCss } from "../src/css.ts";

const out = fileURLToPath(new URL("../src/tokens.css", import.meta.url));
writeFileSync(out, toCss());
console.log(`wrote ${out}`);
