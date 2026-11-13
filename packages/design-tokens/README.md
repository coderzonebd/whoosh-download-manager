# @whoosh/design-tokens

Colours, type, spacing, radii, shadows and motion for every Whoosh surface
(desktop, extension popup, mobile shell, web remote). Apache-2.0.

- **Source of truth:** [`src/tokens.ts`](src/tokens.ts). `src/tokens.css` is
  generated from it; a test fails if the two drift apart.
- **Themes:** light (default), dark, high contrast. Pick one with
  `data-theme="light" | "dark" | "high-contrast"` on `<html>` or any subtree;
  with no attribute (or `"system"`) the OS decides via `prefers-color-scheme`
  (and `prefers-contrast: more` in dark mode).
- **Contrast is tested:** every text colour is ≥ 4.5:1 on the surfaces it
  sits on (≥ 7:1 in high contrast); focus rings, input borders and progress
  fills are ≥ 3:1 (WCAG 1.4.11). See `test/tokens.test.ts`.
- **Bangla-ready type:** Inter for Latin, then Noto Sans Bengali and the
  Bangla faces Windows/macOS ship, then the system UI font. `:lang(bn)` gets
  taller line heights.
- **Motion:** durations and easings as tokens; all durations become `0ms`
  under `prefers-reduced-motion: reduce`.
- **RTL:** the CSS uses no physical left/right properties (tested). Components
  should use logical properties too.

```css
@import "@whoosh/design-tokens/tokens.css";
.card { background: var(--vy-color-surface); padding-inline: var(--vy-space-4); }
```

```ts
import { tokens, vars, contrastRatio } from "@whoosh/design-tokens";
vars.color.textMuted; // "var(--vy-color-text-muted)"
```

## Commands

```bash
corepack pnpm --filter @whoosh/design-tokens build      # regenerate src/tokens.css
corepack pnpm --filter @whoosh/design-tokens test       # contrast + drift checks
corepack pnpm --filter @whoosh/design-tokens typecheck
```
