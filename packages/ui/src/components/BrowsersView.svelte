<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  "Connect your browser" (desktop app, ADR 0007 amendment 1): each browser
  this user has, whether it is linked to the app (native messaging host) and
  whether the extension is installed, with one button that does the next
  step. Opens by itself on the first run and from Settings → Browsers.
-->
<script lang="ts">
  import type { BrowserId, BrowserState } from "../lib/host.ts";
  import { getApp } from "../lib/state/app.svelte.ts";
  import Icon from "./Icon.svelte";

  const app = getApp();
  const { i18n } = app;
  const { t } = i18n;
  const browsers = app.host.browsers;

  /** Product names; not translated. */
  const NAMES: Record<BrowserId, string> = {
    chrome: "Google Chrome",
    chromium: "Chromium",
    brave: "Brave",
    edge: "Microsoft Edge",
    vivaldi: "Vivaldi",
    firefox: "Firefox",
  };

  let rows = $state.raw<BrowserState[] | undefined>(undefined);
  let busy = $state<BrowserId | undefined>(undefined);

  async function load(): Promise<void> {
    if (!browsers) return;
    try {
      rows = await browsers.status();
    } catch (e) {
      rows = [];
      app.report(e);
    }
  }

  $effect(() => {
    void load();
  });

  const used = $derived((rows ?? []).filter((r) => r.inUse));
  const others = $derived(
    new Intl.ListFormat(i18n.intlLocale, { type: "conjunction" }).format((rows ?? []).filter((r) => !r.inUse).map((r) => NAMES[r.browser])),
  );

  async function connect(b: BrowserId): Promise<void> {
    if (!browsers || busy) return;
    busy = b;
    try {
      await browsers.connect(b);
      app.toasts.push({ tone: "success", message: t("browsers.connected", { browser: NAMES[b] }) });
    } catch (e) {
      app.toasts.push({ tone: "error", message: t("browsers.connectFailed", { browser: NAMES[b], message: app.describe(e) }) });
    }
    busy = undefined;
    await load();
  }

  function openStore(b: BrowserId): void {
    void browsers?.openStore(b).catch((e: unknown) => app.report(e));
  }

  function hint(r: BrowserState): string {
    if (r.store !== "github") return t("browsers.hint.store");
    return r.browser === "firefox" ? t("browsers.hint.githubFirefox") : t("browsers.hint.github");
  }
</script>

<div class="browsers">
  <div class="inner">
    <h1>{t("browsers.title")}</h1>
    <p class="intro">{t("browsers.intro")}</p>

    {#if rows === undefined}
      <p class="status" role="status"><span class="vy-spinner" aria-hidden="true"></span>{t("browsers.loading")}</p>
    {:else if used.length === 0}
      <p class="status">{t("browsers.none")}</p>
    {:else}
      <ul class="list" aria-label={t("browsers.title")}>
        {#each used as r (r.browser)}
          {@const ready = r.hostRegistered && r.extensionFound}
          <li class="row" class:ready>
            <div class="name">
              <span class="mark" aria-hidden="true"><Icon name={ready ? "check" : r.hostRegistered ? "download" : "link"} size={16} /></span>
              <div>
                <strong>{NAMES[r.browser]}</strong>
                <span class="state">
                  {#if ready}{t("browsers.state.ready")}{:else if !r.hostRegistered}{t("browsers.state.noHost")}{:else}{t("browsers.state.noExtension")}{/if}
                </span>
                {#if r.hostRegistered && !r.extensionFound}<span class="vy-hint">{hint(r)}</span>{/if}
              </div>
            </div>
            {#if !r.hostRegistered}
              <button class="vy-btn vy-btn--primary" disabled={busy !== undefined} onclick={() => connect(r.browser)}>
                {#if busy === r.browser}<span class="vy-spinner" aria-hidden="true"></span>{/if}{t("browsers.connect")}
              </button>
            {:else if !r.extensionFound}
              <button class="vy-btn vy-btn--primary" onclick={() => openStore(r.browser)}>{t(`browsers.store.${r.store}`)}</button>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
    {#if rows !== undefined && used.length > 0 && others}
      <p class="vy-hint">{t("browsers.notFound", { names: others })}</p>
    {/if}

    <div class="actions">
      <button class="vy-btn" onclick={() => load()}><Icon name="refresh" size={15} />{t("browsers.recheck")}</button>
      <button class="vy-btn vy-btn--primary" onclick={() => (app.view = "downloads")}>{t("browsers.done")}</button>
    </div>
    <p class="vy-hint">{t("browsers.later")}</p>
  </div>
</div>

<style>
  .browsers {
    flex: 1;
    overflow-y: auto;
    background: var(--vy-color-bg);
  }
  .inner {
    display: grid;
    gap: var(--vy-space-4);
    max-inline-size: 720px;
    padding: var(--vy-space-6) var(--vy-space-6) var(--vy-space-8);
  }
  h1 {
    margin: 0;
    font-size: var(--vy-font-size-xl);
  }
  .intro {
    margin: 0;
    color: var(--vy-color-text-muted);
  }
  .status {
    display: flex;
    align-items: center;
    gap: var(--vy-space-2);
    color: var(--vy-color-text-muted);
  }
  .list {
    display: grid;
    gap: var(--vy-space-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--vy-space-4);
    padding: var(--vy-space-3) var(--vy-space-4);
    border: 1px solid var(--vy-color-border);
    border-radius: var(--vy-radius-md);
    background: var(--vy-color-surface);
  }
  .name {
    display: flex;
    align-items: flex-start;
    gap: var(--vy-space-3);
    min-inline-size: 0;
  }
  .name > div {
    display: grid;
    gap: var(--vy-space-1);
  }
  .mark {
    display: grid;
    place-items: center;
    inline-size: 28px;
    block-size: 28px;
    flex: none;
    border-radius: 50%;
    background: var(--vy-color-bg-sunken);
    color: var(--vy-color-text-muted);
  }
  .ready .mark {
    color: var(--vy-color-success-text);
    background: var(--vy-color-success-subtle);
  }
  .state {
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-sm);
  }
  .actions {
    display: flex;
    gap: var(--vy-space-2);
    justify-content: flex-end;
  }
  @container vy-app (max-width: 560px) {
    .row {
      flex-direction: column;
      align-items: stretch;
    }
  }
</style>
