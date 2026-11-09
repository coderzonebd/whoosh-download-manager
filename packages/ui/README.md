# @vayu/ui

The download manager interface shared by the desktop app, the extension
popup/side panel, the mobile shell and the web remote. Svelte 5 (runes) +
TypeScript. Apache-2.0.

```ts
import "@vayu/ui/styles.css";             // tokens + base styles, once per host
import { App } from "@vayu/ui";
import { mount } from "svelte";

mount(App, { target, props: { client, mode: "live" } }); // any VayuClient: ws, mock, native messaging
```

Props: `client`, `mode` (`"mock"` shows "Demo data"), `storage` (where device
prefs persist; `null` disables), `locale`, `manageDocument` (set
`lang`/`dir`/`data-theme` on `<html>`; turn off when embedding), `onready(app)`.

## What is in it

- **Shell:** sidebar (status groups with counts, categories, queues,
  settings), toolbar, status bar with live total speed, active/queued counts,
  speed limit and connection state. Collapses to a drawer below 760 px
  (container query), so the same tree fits a 400 px extension popup.
- **Download list:** one virtualised ARIA grid, two layouts:
  - *Classic*: the dense table IDM users know. Click a header to sort
    (`aria-sort`); drag a divider to resize, double-click to reset, or
    `Alt+←/→` on a focused header. Widths and sort persist per device.
  - *Modern*: one card per row with status pill, segmented bar and a
    plain-language summary ("1.13 GB of 6.20 GB · 18.6 MB/s · 4m 40s left").
- **Segmented progress:** each connection's range is drawn in place; bytes
  between polls are distributed by segment speed so bars glide at 4 Hz.
- **Add dialog:** paste a URL (or just paste anywhere in the window) → the
  link is probed as you type → file name, folder, category, start now/paused.
- **Detail panel:** speed sparkline (30 s), segment map (200 blocks),
  per-segment table, error explanation, copyable address, actions.
- **Expired links:** `needs_refresh` raises a toast and a "Refresh link…"
  action that continues the download from where it stopped.
- **Settings:** bound to the protocol's `Settings` (validated, Save/Revert,
  unsaved indicator) plus device-only layout and high contrast.
- **Toasts:** completions, failures, expiries; timers pause on hover/focus
  and while the window is hidden; announced via persistent live regions.
- **Context menu:** right-click, `Shift+F10` or the Menu key.

## Accessibility

- Grid pattern with one tab stop, `aria-activedescendant`, `aria-selected`,
  `aria-rowcount`/`aria-rowindex` (correct even when virtualised).
- Keyboard: `↑ ↓ PgUp PgDn Home End` move (selection follows), `Shift` extends,
  `Ctrl` moves without selecting, `Ctrl+Space` toggles, `Ctrl+A` all, `Space`
  pause/resume, `Enter` details, `Delete` remove, `Esc` clear, `Ctrl+N` add,
  `Ctrl+F` or `/` search, `Ctrl+,` settings, `?` the full list.
- Dialogs: labelled, focus moves in and back, Tab trapped, `Esc` closes,
  background `inert`. Confirmations use `alertdialog`.
- Visible focus everywhere (`:focus-visible`), 4.5:1 text contrast in every
  theme (tested in design-tokens), `forced-colors` support, motion removed
  under `prefers-reduced-motion`, hover effects only for fine pointers.

## Internationalisation

`src/lib/i18n/en.ts` is the source; `bn.ts` is a native-written Bangla table
(typed against English, so a missing key fails `typecheck`; a test also fails
if a Bangla string is left in English or invents a placeholder). Bangla uses
Bangla digits via `Intl` (`bn-BD`). Plurals use `Intl.PluralRules`.
`I18n.register(tag, …)` adds locales at runtime; Arabic, Persian, Urdu,
Hebrew… switch the whole UI to RTL (every style uses logical properties).

## Performance (10 000 downloads)

- Only the rows in view (+6 overscan) exist in the DOM: ~37 rows, ~1 000
  nodes total at 10 014 downloads.
- Each download is one `$state.raw` object, so a progress event re-renders
  only that row, and only if it is on screen.
- Filter/sort is recomputed on structural changes (add, remove, state change,
  filter, sort), not on progress ticks — rows never jump under the pointer.
- Measured in the playground on a heavily loaded 8-core laptop: smooth
  scrolling ≈ 6 ms per step (p95 8 ms); sorting 10 014 rows by name ≈ 70 ms.
- Segment detail is polled (`download.get`, ≤ 40 visible rows, 1 Hz) — see
  "Proposed contract changes" in the branch report.

## Commands

```bash
corepack pnpm --filter @vayu/ui dev         # playground on the mock: http://localhost:5174 (?bulk=10000, ?lang=bn)
corepack pnpm --filter @vayu/ui test        # vitest + @testing-library (jsdom)
corepack pnpm --filter @vayu/ui typecheck   # svelte-check --fail-on-warnings (includes a11y rules)
```

svelte-check does not support TypeScript 7 yet, so this package pins
TypeScript 6 locally; the root stays on 7.
