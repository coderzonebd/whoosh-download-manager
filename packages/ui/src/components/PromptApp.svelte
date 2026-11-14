<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  The prompt window (desktop app): a small window of its own, like IDM's
  "Download File Info", that asks only about what the browser handed over,
  one thing at a time:

  * browser downloads that wait for confirmation (oldest first), with the
    same dialog the main window used to show;
  * pages offered by the extension's "Download with WDM" button, in the add
    dialog, which probes the page and shows the video/audio format picker.

  When nothing is left it asks the shell to close it (`Host.closePrompt`).
  Starting a download here never shows the main window.
-->
<script lang="ts">
  import type { WdmClient } from "@wdm/api-client";
  import { onDestroy, untrack } from "svelte";
  import { AppState, provideApp, type AppOptions } from "../lib/state/app.svelte.ts";
  import type { Host } from "../lib/host.ts";
  import AddDownloadDialog from "./AddDownloadDialog.svelte";
  import ConfirmDownloadDialog from "./ConfirmDownloadDialog.svelte";

  interface Props {
    client: WdmClient;
    mode?: "mock" | "live";
    storage?: Storage | null;
    locale?: string;
    /** Set lang/dir/data-theme on <html> too (default true). */
    manageDocument?: boolean;
    onready?: (app: AppState) => void;
    host?: Host;
    /** How long the window stays once nothing is left (ms), so the next item can arrive. */
    closeDelayMs?: number;
  }
  let { client, mode = "live", storage, locale, manageDocument = true, onready, host, closeDelayMs = 400 }: Props = $props();

  const app = untrack(() => {
    const opts: AppOptions = { mode, prompt: true };
    if (storage !== undefined) opts.storage = storage;
    if (locale !== undefined) opts.locale = locale;
    if (host !== undefined) opts.host = host;
    return provideApp(new AppState(client, opts));
  });
  untrack(() => onready?.(app));
  onDestroy(() => app.destroy());

  const { i18n } = app;

  const confirming = $derived(app.waiting[0]);
  const offering = $derived(confirming ? undefined : app.offers[0]);
  const empty = $derived(app.loaded && confirming === undefined && offering === undefined);

  // Nothing left to ask: close the window (after a moment, in case the next one is on its way).
  $effect(() => {
    if (!empty) return;
    const timer = setTimeout(() => {
      app.host.closePrompt?.().catch(() => undefined);
    }, closeDelayMs);
    return () => clearTimeout(timer);
  });

  $effect(() => {
    if (!manageDocument) return;
    const el = document.documentElement;
    el.lang = i18n.locale;
    el.dir = i18n.dir;
    if (app.theme === "system") delete el.dataset.theme;
    else el.dataset.theme = app.theme;
  });
</script>

<div class="vy-app prompt" data-theme={app.theme} lang={i18n.locale} dir={i18n.dir}>
  {#if confirming}
    {#key confirming.id}
      <ConfirmDownloadDialog download={confirming} index={1} total={app.waiting.length} />
    {/key}
  {:else if offering}
    {#key offering.id}
      <AddDownloadDialog initialUrl={offering.url} onclose={() => app.doneWithOffer(offering.id)} />
    {/key}
  {:else if empty}
    <p class="empty" role="status">{i18n.t("prompt.empty")}</p>
  {/if}
</div>

<style>
  .prompt {
    block-size: 100%;
    background: var(--vy-color-surface-raised);
  }
  /* The window is the dialog: no scrim, no margins, no floating card. */
  .prompt :global(.layer) {
    padding: 0;
    /* A definite height, so the dialog's body scrolls and its buttons stay in view. */
    grid-template: 100% / 100%;
    place-items: stretch;
  }
  .prompt :global(.scrim) {
    background: transparent;
  }
  .prompt :global(.layer > [role="dialog"]),
  .prompt :global(.layer > [role="alertdialog"]) {
    max-inline-size: none;
    block-size: 100%;
    max-block-size: 100%;
    border-radius: 0;
    box-shadow: none;
  }
  .empty {
    display: grid;
    place-items: center;
    block-size: 100%;
    color: var(--vy-color-text-muted);
  }
</style>
