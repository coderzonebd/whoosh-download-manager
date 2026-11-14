# @whoosh/ui

The download manager interface shared by the desktop app, the extension
popup/side panel, the mobile shell and the web remote. Svelte 5 (runes) +
TypeScript. Apache-2.0.

```ts
import "@whoosh/ui/styles.css";             // tokens + base styles, once per host
import { App } from "@whoosh/ui";
import { mount } from "svelte";

mount(App, { target, props: { client, mode: "live" } }); // any WhooshClient: ws, mock, native messaging
```

Props: `client`, `mode` (`"mock"` shows "Demo data"), `storage` (where device
prefs persist; `null` disables), `locale`, `manageDocument` (set
`lang`/`dir`/`data-theme` on `<html>`; turn off when embedding), `onready(app)`,
`host` (optional shell extras, see below).

### Host extras

`host` (`Host` in `src/lib/host.ts`) is how a shell adds what a browser tab
can't do. Every part is optional and its controls are hidden without it:
`pickFolder` (Browse… buttons), `desktopPrefs` (close to tray, Quit also
stops downloads) and `launchAtLogin` (both under Settings → *This
computer*). `app.openAdd(url)` opens the add dialog prefilled; the desktop
app calls it for `whoosh://add?url=…` links, checked with `parseDeepLink`.

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
- **Queues:** the main queue and named queues in the sidebar, with counts
  of what is still to do. Create, rename and delete; a bar above the list
  shows whether a queue runs and its schedule in words, with Start/Stop.
  "Move to queue…" in the context menu, and a queue picker in the add dialog.
- **Scheduler:** per queue, start and stop times on chosen days and the
  after-done action (nothing, sleep, shut down). `event.queue.finished`
  shows a banner with a countdown and Cancel (`queue.cancelAction`); the
  sentence is announced once, the seconds are visual only.
- **Without queues:** the UI learns from the hello's `capabilities` (or a
  -32006 answer to `queue.list`) that the daemon has no queues, and then
  shows only the main queue, with no error.
- **History:** finished and failed downloads, newest first, with search and
  an All/Completed/Failed filter; clear one entry or everything shown
  (`download.remove` with `deleteFiles: false`, after a confirmation).
- **Settings:** bound to the protocol's `Settings` (validated, Save/Revert,
  unsaved indicator; fields the screen doesn't show are kept), including
  speed limits by time of day (`speedSchedule`), plus device-only layout and
  high contrast.
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

Strings live in `src/lib/i18n/locales/<lang>.json`, one file per language
in i18next JSON v4 (nested keys, `{{name}}` placeholders, `key_one` /
`key_other` / … plurals chosen with `Intl.PluralRules`), translated on
hosted Weblate ([docs/translating.md](../../docs/translating.md)).
`en.json` is the source, and `t()` keys are type-checked against it
(`MessageKey`). `bn.json` is native-written Bangla; ten more languages are
seeded by machine translation and flagged `machine: true` in
`src/lib/i18n/languages.ts`, which the language picker shows with a link to
Weblate. A missing or empty string falls back to English. Tests check that
every locale has the source's keys, placeholders and CLDR plural forms
(`test/i18n.test.ts`); `pnpm i18n:sync` adds new English keys to the other
locales, and `scripts/i18n-import-ts.mjs` converts strings still written in
the old `en.ts`/`bn.ts` format. Numbers, sizes and percentages come from
`Intl` (Bangla digits via `bn-BD`, `%42,3` in Turkish).
`I18n.register(tag, …)` adds locales at runtime. Arabic and Urdu switch the
whole UI to RTL (every style uses logical properties; `test/rtl.test.ts`
renders the app in both and refuses physical `left`/`right` styles).

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
corepack pnpm --filter @whoosh/ui dev         # playground on the mock: http://localhost:5174 (?bulk=10000, ?lang=bn)
corepack pnpm --filter @whoosh/ui test        # vitest + @testing-library (jsdom)
corepack pnpm --filter @whoosh/ui typecheck   # svelte-check --fail-on-warnings (includes a11y rules)
```

svelte-check does not support TypeScript 7 yet, so this package pins
TypeScript 6 locally; the root stays on 7.
