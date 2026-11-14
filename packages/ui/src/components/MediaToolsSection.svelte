<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  Settings → Video and audio sites: yt-dlp and ffmpeg, their versions,
  where they come from, and update. Actions apply at once (they are not part
  of the settings draft). Shown only when the daemon has media support.
-->
<script lang="ts">
  import type { MediaTool } from "@whoosh/api-client";
  import { getApp } from "../lib/state/app.svelte.ts";
  import Icon from "./Icon.svelte";

  const app = getApp();
  const { i18n } = app;
  const { t } = i18n;

  let checking = $state(false);
  let checkError = $state<string | undefined>(undefined);
  let busy = $state(false);

  const status = $derived(app.media.tools);

  async function check(): Promise<void> {
    checking = true;
    checkError = undefined;
    try {
      app.media.replace(await app.client.call("media.checkUpdates", undefined));
    } catch (e) {
      checkError = t("mediaTools.checkFailed", { message: app.describe(e) });
    } finally {
      checking = false;
    }
  }

  async function update(tool: MediaTool): Promise<void> {
    try {
      app.media.replace(await app.client.call("media.updateTools", { tools: [tool.name] }));
    } catch (e) {
      app.report(e);
    }
  }

  async function setAuto(on: boolean): Promise<void> {
    busy = true;
    try {
      app.media.replace(await app.client.call("media.configure", { autoUpdate: on }));
    } catch (e) {
      app.report(e);
    } finally {
      busy = false;
    }
  }

  function stateText(tool: MediaTool): string {
    if (tool.state === "installing") {
      const total = tool.totalBytes;
      return total && tool.downloadedBytes !== undefined
        ? t("mediaTools.installingProgress", { percent: i18n.percent(tool.downloadedBytes / total) })
        : t("mediaTools.installing");
    }
    return tool.state === "ready" ? t("mediaTools.ready") : t("mediaTools.missing");
  }
</script>

<section aria-labelledby="vy-set-media" aria-describedby="vy-set-media-hint">
  <h2 id="vy-set-media">{t("settings.media")}</h2>
  <p id="vy-set-media-hint" class="vy-hint intro">{t("settings.mediaHint")}</p>
  {#if status}
    <ul class="tools">
      {#each status.tools as tool (tool.name)}
        <li class="tool" aria-labelledby="vy-tool-{tool.name}">
          <div class="info">
            <h3 id="vy-tool-{tool.name}">{tool.name}</h3>
            <p class="line">
              <span class="state" class:ready={tool.state === "ready"} role="status">
                {#if tool.state === "installing"}<span class="vy-spinner" aria-hidden="true"></span>{/if}
                {stateText(tool)}
              </span>
              {#if tool.version}<span class="vy-num">{t("mediaTools.version", { version: tool.version })}</span>{/if}
              {#if tool.source}<span>{tool.source === "managed" ? t("mediaTools.managed") : t("mediaTools.system")}</span>{/if}
            </p>
            {#if tool.updateAvailable && tool.latestVersion}
              <p class="line update">{t("mediaTools.updateAvailable", { version: tool.latestVersion })}</p>
            {:else if tool.latestVersion && tool.state === "ready"}
              <p class="line">{t("mediaTools.upToDate")}</p>
            {/if}
            <p class="line source">{t("mediaTools.source", { upstream: tool.upstream, license: tool.license })}</p>
            {#if tool.error}<p class="vy-error">{t("mediaTools.error", { message: tool.error })}</p>{/if}
          </div>
          {#if tool.state !== "installing" && (tool.state === "missing" || tool.updateAvailable)}
            <button type="button" class="vy-btn vy-btn--secondary vy-btn--sm" onclick={() => update(tool)}>
              <Icon name="download" size={14} />
              {tool.state === "missing" ? t("mediaTools.install", { name: tool.name }) : t("mediaTools.update", { name: tool.name })}
            </button>
          {/if}
        </li>
      {/each}
    </ul>
    <div class="row">
      <button type="button" class="vy-btn vy-btn--sm" onclick={check} disabled={checking}>
        {#if checking}<span class="vy-spinner" aria-hidden="true"></span>{:else}<Icon name="refresh" size={14} />{/if}
        {t("mediaTools.check")}
      </button>
      {#if status.lastCheckAt}<span class="vy-hint">{t("mediaTools.lastCheck", { time: i18n.date(status.lastCheckAt) })}</span>{/if}
    </div>
    {#if checkError}<p class="vy-error" role="alert">{checkError}</p>{/if}
    <div class="vy-field auto">
      <label class="vy-check">
        <input type="checkbox" checked={status.autoUpdate} disabled={busy} onchange={(e) => setAuto(e.currentTarget.checked)} aria-describedby="vy-set-media-auto-hint" />
        {t("mediaTools.autoUpdate")}
      </label>
      <span id="vy-set-media-auto-hint" class="vy-hint">{t("mediaTools.autoUpdateHint")}</span>
    </div>
  {/if}
</section>

<style>
  section {
    padding: var(--vy-space-5);
    border-radius: var(--vy-radius-lg);
    border: 1px solid var(--vy-color-border);
    background: var(--vy-color-surface);
  }
  h2 {
    margin-block-end: var(--vy-space-2);
    font-size: var(--vy-font-size-md);
    font-weight: var(--vy-font-weight-semibold);
  }
  .intro {
    margin-block-end: var(--vy-space-4);
  }
  .tools {
    display: grid;
    gap: var(--vy-space-3);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .tool {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--vy-space-3);
    padding: var(--vy-space-3);
    border-radius: var(--vy-radius-md);
    border: 1px solid var(--vy-color-border);
    background: var(--vy-color-bg);
  }
  .info {
    display: grid;
    gap: var(--vy-space-1);
    min-inline-size: 0;
  }
  h3 {
    font-family: var(--vy-font-mono);
    font-size: var(--vy-font-size-sm);
    font-weight: var(--vy-font-weight-semibold);
  }
  .line {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--vy-space-1) var(--vy-space-3);
    color: var(--vy-color-text-muted);
    font-size: var(--vy-font-size-sm);
  }
  .state {
    display: inline-flex;
    align-items: center;
    gap: var(--vy-space-1-5);
  }
  .state.ready {
    color: var(--vy-color-success-text);
  }
  .update {
    color: var(--vy-color-accent-text, var(--vy-color-text));
  }
  .source {
    font-size: var(--vy-font-size-xs);
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--vy-space-3);
    margin-block-start: var(--vy-space-4);
  }
  .auto {
    margin-block-start: var(--vy-space-4);
  }
</style>
