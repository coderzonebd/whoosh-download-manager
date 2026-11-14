<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  Daemon settings (`Settings` in the protocol) are edited as a draft and
  saved with settings.set; device-only preferences (layout, high contrast)
  and the desktop app's own settings (tray, launch at login) apply
  immediately because they never reach the daemon.
-->
<script lang="ts">
  import type { Settings, SpeedWindow } from "@whoosh/api-client";
  import type { DesktopPrefs } from "../lib/host.ts";
  import { normalizeDays, windowProblem } from "../lib/schedule.ts";
  import { getApp } from "../lib/state/app.svelte.ts";
  import DaysPicker from "./DaysPicker.svelte";
  import Icon from "./Icon.svelte";
  import MediaToolsSection from "./MediaToolsSection.svelte";

  const app = getApp();
  const { i18n, prefs } = app;
  const { t } = i18n;

  type Unit = "KB" | "MB";
  interface WindowDraft {
    days: number[];
    from: string;
    to: string;
    value: string;
    unit: Unit;
  }
  interface Draft {
    defaultDirectory: string;
    maxActive: string;
    maxConnections: string;
    language: string;
    theme: Settings["theme"];
    limitOn: boolean;
    limitValue: string;
    limitUnit: Unit;
    windows: WindowDraft[];
  }

  const UNIT: Record<Unit, number> = { KB: 1024, MB: 1024 * 1024 };

  function unitFor(bps: number | undefined): Unit {
    return bps !== undefined && bps >= UNIT.MB && bps % (UNIT.MB / 10) === 0 ? "MB" : "KB";
  }
  function valueIn(bps: number, unit: Unit): string {
    return String(Math.round((bps / UNIT[unit]) * 10) / 10);
  }

  function toWindow(w: SpeedWindow): WindowDraft {
    const unit = unitFor(w.limitBps);
    return { days: [...(w.days ?? [])], from: w.from, to: w.to, value: valueIn(w.limitBps, unit), unit };
  }

  function toDraft(s: Settings): Draft {
    const bps = s.speedLimitBps;
    const unit = unitFor(bps);
    return {
      defaultDirectory: s.defaultDirectory,
      maxActive: String(s.maxActiveDownloads),
      maxConnections: String(s.maxConnectionsPerDownload),
      language: s.language,
      theme: s.theme,
      limitOn: bps !== undefined,
      limitValue: bps === undefined ? "1" : valueIn(bps, unit),
      limitUnit: unit,
      windows: (s.speedSchedule ?? []).map(toWindow),
    };
  }

  let baseline = $state.raw<Settings | undefined>(undefined);
  let draft = $state<Draft | undefined>(undefined);
  let saving = $state(false);

  // Take the daemon's settings as the starting point (and again after a save).
  $effect(() => {
    const s = app.settings;
    if (s && s !== baseline) {
      baseline = s;
      draft = toDraft(s);
    }
  });

  /** Number inputs bind numbers at runtime even though the draft holds text. */
  function int(v: unknown, min: number, max: number): number | undefined {
    const s = String(v ?? "").trim();
    if (!/^\d+$/.test(s)) return undefined;
    const n = Number(s);
    return n >= min && n <= max ? n : undefined;
  }

  function limitOk(v: string): boolean {
    const n = Number(v);
    return Number.isFinite(n) && n > 0 && n <= 1_000_000;
  }

  /** Per window: what is wrong with its times and its limit. */
  const windowErrors = $derived(
    (draft?.windows ?? []).map((w) => {
      const p = windowProblem(w);
      return {
        time: p === "time" ? t("schedule.badTime") : p === "same" ? t("speedSchedule.same") : undefined,
        limit: limitOk(String(w.value)) ? undefined : t("settings.range", { min: i18n.number(1), max: i18n.number(1_000_000) }),
      };
    }),
  );

  const errors = $derived.by(() => {
    const e: Partial<Record<"defaultDirectory" | "maxActive" | "maxConnections" | "limitValue", string>> = {};
    if (!draft) return e;
    if (!draft.defaultDirectory.trim()) e.defaultDirectory = t("settings.required");
    if (int(draft.maxActive, 1, 64) === undefined) e.maxActive = t("settings.range", { min: i18n.number(1), max: i18n.number(64) });
    if (int(draft.maxConnections, 1, 64) === undefined) e.maxConnections = t("settings.range", { min: i18n.number(1), max: i18n.number(64) });
    const lv = Number(draft.limitValue);
    if (draft.limitOn && !(Number.isFinite(lv) && lv > 0 && lv <= 1_000_000)) e.limitValue = t("settings.range", { min: i18n.number(1), max: i18n.number(1_000_000) });
    return e;
  });
  const valid = $derived(Object.keys(errors).length === 0 && windowErrors.every((w) => !w.time && !w.limit));

  function build(d: Draft): Settings | undefined {
    const maxActiveDownloads = int(d.maxActive, 1, 64);
    const maxConnectionsPerDownload = int(d.maxConnections, 1, 64);
    if (maxActiveDownloads === undefined || maxConnectionsPerDownload === undefined) return undefined;
    // Start from what the daemon sent, so fields this screen doesn't show (durability, …) survive a save.
    const s: Settings = { ...baseline, defaultDirectory: d.defaultDirectory.trim(), maxActiveDownloads, maxConnectionsPerDownload, language: d.language, theme: d.theme };
    delete s.speedLimitBps;
    delete s.speedSchedule;
    if (d.limitOn) s.speedLimitBps = Math.max(1, Math.round(Number(d.limitValue) * UNIT[d.limitUnit]));
    const windows = d.windows.map((w): SpeedWindow => {
      const out: SpeedWindow = { from: w.from, to: w.to, limitBps: Math.max(1, Math.round(Number(w.value) * UNIT[w.unit])) };
      const days = normalizeDays(w.days);
      if (days) out.days = days;
      return out;
    });
    if (windows.length > 0) s.speedSchedule = windows;
    return s;
  }

  const next = $derived(draft && valid ? build(draft) : undefined);
  const dirty = $derived.by(() => {
    if (!draft || !baseline) return false;
    const a = toDraft(baseline);
    return (Object.keys(a) as (keyof Draft)[]).some((k) => JSON.stringify(norm(a[k])) !== JSON.stringify(norm(draft?.[k])));
  });

  /** Number inputs hand back numbers; compare everything as text. */
  function norm(v: unknown): unknown {
    if (Array.isArray(v)) return v.map(norm);
    if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, norm(x)]));
    return String(v);
  }

  function addWindow(): void {
    if (!draft) return;
    draft.windows.push({ days: [1, 2, 3, 4, 5], from: "09:00", to: "18:00", value: "512", unit: "KB" });
    const n = draft.windows.length;
    queueMicrotask(() => document.getElementById(`vy-win-${n - 1}-from`)?.focus());
  }

  function removeWindow(i: number): void {
    draft?.windows.splice(i, 1);
    queueMicrotask(() => document.getElementById("vy-win-add")?.focus());
  }

  // ------------------------------------------------------------ folder picker

  let picking = $state(false);
  async function chooseFolder(): Promise<void> {
    const pick = app.host.pickFolder;
    if (!pick || !draft || picking) return;
    picking = true;
    try {
      const dir = await pick({ title: t("settings.chooseFolder"), defaultPath: draft.defaultDirectory.trim() || undefined });
      if (dir && draft) draft.defaultDirectory = dir;
    } catch (e) {
      app.report(e);
    } finally {
      picking = false;
    }
  }

  // ------------------------------------------------------------ this computer (desktop app only)

  const host = app.host;
  const hasDesktop = host.desktopPrefs !== undefined || host.launchAtLogin !== undefined;
  let desktop = $state<DesktopPrefs | undefined>(undefined);
  let atLogin = $state<boolean | undefined>(undefined);

  $effect(() => {
    host.desktopPrefs
      ?.get()
      .then((p) => (desktop = p))
      .catch(() => (desktop = undefined));
    host.launchAtLogin
      ?.get()
      .then((on) => (atLogin = on))
      .catch(() => (atLogin = undefined));
  });

  async function setDesktop(key: keyof DesktopPrefs, value: boolean): Promise<void> {
    if (!host.desktopPrefs || !desktop) return;
    const before = desktop;
    desktop = { ...desktop, [key]: value };
    try {
      desktop = await host.desktopPrefs.set(desktop);
    } catch (e) {
      desktop = before;
      app.toasts.push({ tone: "error", message: t("settings.saveFailed", { message: app.describe(e) }) });
    }
  }

  async function setAtLogin(on: boolean): Promise<void> {
    if (!host.launchAtLogin) return;
    const before = atLogin;
    atLogin = on;
    try {
      await host.launchAtLogin.set(on);
    } catch (e) {
      atLogin = before;
      app.toasts.push({ tone: "error", message: t("settings.saveFailed", { message: app.describe(e) }) });
    }
  }

  async function save(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    if (!next || saving) return;
    saving = true;
    await app.saveSettings(next);
    saving = false;
  }

  function revert(): void {
    if (baseline) draft = toDraft(baseline);
  }

  const THEMES: Settings["theme"][] = ["system", "light", "dark"];
</script>

<div class="settings">
  {#if !draft}
    <p class="loading" role="status"><span class="vy-spinner" aria-hidden="true"></span>{t("settings.loading")}</p>
  {:else}
    <form class="form" onsubmit={save} novalidate>
      <div class="bar" class:dirty>
        <h1>{t("settings.title")}</h1>
        {#if dirty}<span class="unsaved" role="status"><span class="dot" aria-hidden="true"></span>{t("settings.unsaved")}</span>{/if}
        <button type="button" class="vy-btn vy-btn--secondary" disabled={!dirty || saving} onclick={revert}>{t("action.revert")}</button>
        <button type="submit" class="vy-btn vy-btn--primary" disabled={!dirty || !valid || saving}>
          {#if saving}<span class="vy-spinner" aria-hidden="true"></span>{/if}{t("action.save")}
        </button>
      </div>

      <section aria-labelledby="vy-set-appearance">
        <h2 id="vy-set-appearance">{t("settings.appearance")}</h2>
        <div class="grid">
          <div class="vy-field">
            <label class="vy-label" for="vy-set-lang">{t("settings.language")}</label>
            <select id="vy-set-lang" class="vy-select" bind:value={draft.language}>
              {#each i18n.available as l (l.tag)}
                <option value={l.tag} lang={l.tag}>{l.nativeName}</option>
              {/each}
            </select>
          </div>
          <fieldset class="vy-field">
            <legend class="vy-label">{t("settings.theme")}</legend>
            <div class="choice">
              {#each THEMES as th (th)}
                <label class:on={draft.theme === th}>
                  <input type="radio" name="vy-theme" value={th} bind:group={draft.theme} />
                  {t(`theme.${th}`)}
                </label>
              {/each}
            </div>
          </fieldset>
          <fieldset class="vy-field">
            <legend class="vy-label">{t("settings.layout")}</legend>
            <div class="choice">
              <label class:on={prefs.layout === "classic"}>
                <input type="radio" name="vy-layout" value="classic" bind:group={prefs.layout} />
                <Icon name="rows" size={15} />{t("layout.classic")}
              </label>
              <label class:on={prefs.layout === "modern"}>
                <input type="radio" name="vy-layout" value="modern" bind:group={prefs.layout} />
                <Icon name="cards" size={15} />{t("layout.modern")}
              </label>
            </div>
          </fieldset>
          <div class="vy-field">
            <label class="vy-check">
              <input type="checkbox" bind:checked={prefs.highContrast} aria-describedby="vy-set-hc-hint" />
              {t("settings.highContrast")}
            </label>
            <span id="vy-set-hc-hint" class="vy-hint">{t("settings.highContrastHint")}</span>
          </div>
        </div>
      </section>

      <section aria-labelledby="vy-set-downloads">
        <h2 id="vy-set-downloads">{t("settings.downloads")}</h2>
        <div class="grid">
          <div class="vy-field wide">
            <label class="vy-label" for="vy-set-dir">{t("settings.defaultDirectory")}</label>
            <div class="folder-row">
              <input
                id="vy-set-dir"
                class="vy-input"
                dir="auto"
                bind:value={draft.defaultDirectory}
                aria-invalid={errors.defaultDirectory !== undefined}
                aria-describedby={errors.defaultDirectory ? "vy-set-dir-err" : undefined}
              />
              {#if host.pickFolder}
                <button type="button" class="vy-btn" disabled={picking} onclick={chooseFolder}>
                  <Icon name="folder" size={15} />{t("action.browse")}
                </button>
              {/if}
            </div>
            {#if errors.defaultDirectory}<span id="vy-set-dir-err" class="vy-error">{errors.defaultDirectory}</span>{/if}
          </div>
          <div class="vy-field">
            <label class="vy-label" for="vy-set-active">{t("settings.maxActive")}</label>
            <input
              id="vy-set-active"
              class="vy-input num"
              type="number"
              min="1"
              max="64"
              inputmode="numeric"
              bind:value={draft.maxActive}
              aria-invalid={errors.maxActive !== undefined}
              aria-describedby="vy-set-active-hint"
            />
            <span id="vy-set-active-hint" class={errors.maxActive ? "vy-error" : "vy-hint"}>{errors.maxActive ?? t("settings.maxActiveHint")}</span>
          </div>
          <div class="vy-field">
            <label class="vy-label" for="vy-set-conn">{t("settings.maxConnections")}</label>
            <input
              id="vy-set-conn"
              class="vy-input num"
              type="number"
              min="1"
              max="64"
              inputmode="numeric"
              bind:value={draft.maxConnections}
              aria-invalid={errors.maxConnections !== undefined}
              aria-describedby="vy-set-conn-hint"
            />
            <span id="vy-set-conn-hint" class={errors.maxConnections ? "vy-error" : "vy-hint"}>{errors.maxConnections ?? t("settings.maxConnectionsHint")}</span>
          </div>
        </div>
      </section>

      <section aria-labelledby="vy-set-speed">
        <h2 id="vy-set-speed">{t("settings.speed")}</h2>
        <div class="grid">
          <label class="vy-check wide">
            <input type="checkbox" bind:checked={draft.limitOn} />
            {t("settings.speedLimit")}
          </label>
          {#if draft.limitOn}
            <div class="vy-field">
              <label class="vy-label" for="vy-set-limit">{t("settings.speedLimitValue")}</label>
              <div class="limit">
                <input
                  id="vy-set-limit"
                  class="vy-input num"
                  type="number"
                  min="0.1"
                  step="0.1"
                  inputmode="decimal"
                  bind:value={draft.limitValue}
                  aria-invalid={errors.limitValue !== undefined}
                  aria-describedby={errors.limitValue ? "vy-set-limit-err" : undefined}
                />
                <select class="vy-select unit" bind:value={draft.limitUnit} aria-label={t("settings.speedLimitValue")}>
                  <option value="KB">{t("unit.speed", { size: "KB" })}</option>
                  <option value="MB">{t("unit.speed", { size: "MB" })}</option>
                </select>
              </div>
              {#if errors.limitValue}<span id="vy-set-limit-err" class="vy-error">{errors.limitValue}</span>{/if}
            </div>
          {/if}
        </div>

        <div class="windows" role="group" aria-labelledby="vy-set-sched" aria-describedby="vy-set-sched-hint">
          <h3 id="vy-set-sched">{t("speedSchedule.title")}</h3>
          <p id="vy-set-sched-hint" class="vy-hint">{t("speedSchedule.hint")}</p>
          {#each draft.windows as w, i (i)}
            {@const err = windowErrors[i]}
            <div class="window" role="group" aria-label={t("speedSchedule.window", { n: i18n.number(i + 1) })}>
              <div class="row">
                <div class="vy-field">
                  <label class="vy-label" for="vy-win-{i}-from">{t("speedSchedule.from")}</label>
                  <input id="vy-win-{i}-from" class="vy-input time" type="time" bind:value={w.from} aria-invalid={err?.time !== undefined} />
                </div>
                <div class="vy-field">
                  <label class="vy-label" for="vy-win-{i}-to">{t("speedSchedule.to")}</label>
                  <input id="vy-win-{i}-to" class="vy-input time" type="time" bind:value={w.to} aria-invalid={err?.time !== undefined} />
                </div>
                <div class="vy-field">
                  <label class="vy-label" for="vy-win-{i}-limit">{t("speedSchedule.limit")}</label>
                  <div class="limit">
                    <input
                      id="vy-win-{i}-limit"
                      class="vy-input num"
                      type="number"
                      min="0.1"
                      step="0.1"
                      inputmode="decimal"
                      bind:value={w.value}
                      aria-invalid={err?.limit !== undefined}
                    />
                    <select class="vy-select unit" bind:value={w.unit} aria-label={t("speedSchedule.unit")}>
                      <option value="KB">{t("unit.speed", { size: "KB" })}</option>
                      <option value="MB">{t("unit.speed", { size: "MB" })}</option>
                    </select>
                  </div>
                </div>
                <button
                  type="button"
                  class="vy-btn vy-btn--icon remove"
                  aria-label={t("speedSchedule.remove", { n: i18n.number(i + 1) })}
                  title={t("speedSchedule.remove", { n: i18n.number(i + 1) })}
                  onclick={() => removeWindow(i)}
                >
                  <Icon name="trash" size={15} />
                </button>
              </div>
              {#if err?.time}<span class="vy-error">{err.time}</span>{/if}
              {#if err?.limit}<span class="vy-error">{err.limit}</span>{/if}
              <DaysPicker bind:days={w.days} legend={t("speedSchedule.days")} />
            </div>
          {/each}
          <div>
            <button id="vy-win-add" type="button" class="vy-btn" onclick={addWindow}>
              <Icon name="plus" size={15} />{t("speedSchedule.add")}
            </button>
          </div>
        </div>
      </section>

      {#if hasDesktop}
        <section aria-labelledby="vy-set-desktop">
          <h2 id="vy-set-desktop">{t("settings.desktop")}</h2>
          <div class="checks">
            {#if host.launchAtLogin}
              <div class="vy-field">
                <label class="vy-check">
                  <input type="checkbox" checked={atLogin === true} disabled={atLogin === undefined} onchange={(e) => setAtLogin(e.currentTarget.checked)} aria-describedby="vy-set-login-hint" />
                  {t("settings.launchAtLogin")}
                </label>
                <span id="vy-set-login-hint" class="vy-hint">{t("settings.launchAtLoginHint")}</span>
              </div>
            {/if}
            {#if host.desktopPrefs}
              <div class="vy-field">
                <label class="vy-check">
                  <input type="checkbox" checked={desktop?.closeToTray === true} disabled={!desktop} onchange={(e) => setDesktop("closeToTray", e.currentTarget.checked)} aria-describedby="vy-set-tray-hint" />
                  {t("settings.closeToTray")}
                </label>
                <span id="vy-set-tray-hint" class="vy-hint">{t("settings.closeToTrayHint")}</span>
              </div>
              <div class="vy-field">
                <label class="vy-check">
                  <input type="checkbox" checked={desktop?.stopDaemonOnQuit === true} disabled={!desktop} onchange={(e) => setDesktop("stopDaemonOnQuit", e.currentTarget.checked)} aria-describedby="vy-set-quit-hint" />
                  {t("settings.stopDaemonOnQuit")}
                </label>
                <span id="vy-set-quit-hint" class="vy-hint">{t("settings.stopDaemonOnQuitHint")}</span>
              </div>
            {/if}
          </div>
          <p class="vy-hint applies">{t("settings.desktopHint")}</p>
        </section>
      {/if}

      {#if app.media.enabled}
        <MediaToolsSection />
      {/if}
    </form>
  {/if}
</div>

<style>
  .settings {
    flex: 1;
    overflow-y: auto;
    background: var(--vy-color-bg);
  }
  .loading {
    display: flex;
    align-items: center;
    gap: var(--vy-space-2);
    padding: var(--vy-space-8);
    color: var(--vy-color-text-muted);
  }
  .form {
    display: grid;
    gap: var(--vy-space-4);
    max-inline-size: 760px;
    margin-inline: auto;
    padding-block: 0 var(--vy-space-8);
    padding-inline: var(--vy-space-6);
  }
  h1 {
    font-size: var(--vy-font-size-2xl);
    font-weight: var(--vy-font-weight-semibold);
    letter-spacing: var(--vy-letter-spacing-tight);
  }
  section {
    padding: var(--vy-space-5);
    border-radius: var(--vy-radius-lg);
    border: 1px solid var(--vy-color-border);
    background: var(--vy-color-surface);
  }
  h2 {
    margin-block-end: var(--vy-space-4);
    font-size: var(--vy-font-size-md);
    font-weight: var(--vy-font-weight-semibold);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
    gap: var(--vy-space-5);
  }
  .wide {
    grid-column: 1 / -1;
  }
  fieldset {
    margin: 0;
    padding: 0;
    border: 0;
    min-inline-size: 0;
  }
  legend {
    padding: 0;
    margin-block-end: var(--vy-space-1);
  }
  .choice {
    display: flex;
    padding: 2px;
    border-radius: var(--vy-radius-sm);
    background: var(--vy-color-bg);
    box-shadow: inset 0 0 0 1px var(--vy-color-border-strong);
  }
  .choice label {
    position: relative;
    flex: 1;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--vy-space-1-5);
    min-block-size: 28px;
    padding-inline: var(--vy-space-2);
    border-radius: calc(var(--vy-radius-sm) - 1px);
    color: var(--vy-color-text-muted);
    cursor: pointer;
    white-space: nowrap;
    transition:
      background-color var(--vy-duration-fast) var(--vy-ease-standard),
      color var(--vy-duration-fast) var(--vy-ease-standard);
  }
  .choice label.on {
    background: var(--vy-color-surface);
    color: var(--vy-color-text);
    font-weight: var(--vy-font-weight-medium);
    box-shadow: var(--vy-shadow-sm), inset 0 0 0 1px var(--vy-color-border);
  }
  :global([data-theme="high-contrast"]) .choice label.on {
    box-shadow: inset 0 0 0 2px var(--vy-color-accent);
  }
  .choice input {
    position: absolute;
    opacity: 0;
    inset: 0;
    margin: 0;
    cursor: pointer;
  }
  .choice label:has(input:focus-visible) {
    outline: var(--vy-size-focus-width) solid var(--vy-color-focus);
    outline-offset: 1px;
  }
  .num {
    max-inline-size: 140px;
  }
  .limit {
    display: flex;
    gap: var(--vy-space-2);
  }
  .unit {
    inline-size: auto;
  }
  .folder-row {
    display: flex;
    gap: var(--vy-space-2);
  }
  .folder-row input {
    flex: 1;
    min-inline-size: 0;
  }
  .windows {
    display: grid;
    gap: var(--vy-space-3);
    margin-block-start: var(--vy-space-5);
    padding-block-start: var(--vy-space-4);
    border-block-start: 1px solid var(--vy-color-border);
  }
  h3 {
    font-size: var(--vy-font-size-base, inherit);
    font-weight: var(--vy-font-weight-semibold);
  }
  .window {
    display: grid;
    gap: var(--vy-space-2);
    padding: var(--vy-space-3);
    border-radius: var(--vy-radius-md);
    border: 1px solid var(--vy-color-border);
    background: var(--vy-color-bg);
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: var(--vy-space-3);
  }
  .time {
    inline-size: 9rem;
  }
  .remove {
    margin-inline-start: auto;
  }
  .checks {
    display: grid;
    gap: var(--vy-space-4);
  }
  .applies {
    margin-block-start: var(--vy-space-4);
  }
  .bar {
    position: sticky;
    inset-block-start: 0;
    z-index: 1;
    display: flex;
    align-items: center;
    gap: var(--vy-space-2);
    margin-inline: calc(-1 * var(--vy-space-6));
    padding-block: var(--vy-space-4) var(--vy-space-3);
    padding-inline: var(--vy-space-6);
    background: color-mix(in srgb, var(--vy-color-bg) 88%, transparent);
    backdrop-filter: blur(8px);
    border-block-end: 1px solid transparent;
  }
  .bar.dirty {
    border-block-end-color: var(--vy-color-border);
  }
  .bar h1 {
    margin-inline-end: auto;
  }
  .unsaved {
    display: inline-flex;
    align-items: center;
    gap: var(--vy-space-2);
    margin-inline-end: var(--vy-space-2);
    color: var(--vy-color-warning-text);
    font-size: var(--vy-font-size-sm);
  }
  .dot {
    inline-size: 7px;
    block-size: 7px;
    border-radius: 50%;
    background: var(--vy-color-warning);
  }
</style>
