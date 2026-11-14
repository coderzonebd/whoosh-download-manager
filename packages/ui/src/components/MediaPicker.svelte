<!-- SPDX-License-Identifier: Apache-2.0 -->
<!--
  What `media.probe` found on a page, as a few plain choices: video at a
  quality, or audio only (as it is, or MP3), plus subtitles. Shown inside
  the add dialog; the dialog owns the values and sends `media.add`.
-->
<script lang="ts">
  import type { MediaProbeResult } from "@whoosh/api-client";
  import { choiceFor, estimate, hasAudio, sizeAt, videoHeights, type AudioConvert, type MediaMode, type Quality } from "../lib/media.ts";
  import { getApp } from "../lib/state/app.svelte.ts";
  import Icon from "./Icon.svelte";

  interface Props {
    result: MediaProbeResult;
    mode: MediaMode;
    quality: Quality;
    convert: AudioConvert;
    subtitles: string[];
  }
  let { result, mode = $bindable(), quality = $bindable(), convert = $bindable(), subtitles = $bindable() }: Props = $props();

  const { i18n } = getApp();
  const { t } = i18n;

  const heights = $derived(videoHeights(result.formats));
  /** 1080, not "1,080" (in the user's digits). */
  const plain = (n: number) => n.toLocaleString(i18n.intlLocale, { useGrouping: false });
  const audioOk = $derived(hasAudio(result.formats));
  const est = $derived(estimate(result.formats, choiceFor(mode, quality, convert)));

  function toggleSub(lang: string, on: boolean): void {
    subtitles = on ? [...subtitles.filter((l) => l !== lang), lang] : subtitles.filter((l) => l !== lang);
  }
</script>

<section class="media" aria-labelledby="vy-media-title">
  <div class="head">
    <Icon name="video" size={18} />
    <div class="meta">
      <h3 id="vy-media-title" class="vy-truncate" dir="auto" title={result.title}>{result.title}</h3>
      <p class="sub">
        <span class="vy-sr-only">{t("media.found")}:</span>
        {#if result.durationSecs !== undefined}<span>{t("media.length", { time: i18n.eta(result.durationSecs) })}</span>{/if}
        {#if result.uploader}<span dir="auto">{t("media.by", { name: result.uploader })}</span>{/if}
        {#if result.live}<span class="live">{t("media.live")}</span>{/if}
      </p>
    </div>
  </div>

  <fieldset class="kind">
    <legend class="vy-label">{t("media.kind")}</legend>
    <div class="choice" role="radiogroup">
      <label class:on={mode === "video"}>
        <input type="radio" name="vy-media-kind" value="video" bind:group={mode} />
        <Icon name="video" size={14} />{t("media.video")}
      </label>
      <label class:on={mode === "audio"}>
        <input type="radio" name="vy-media-kind" value="audio" bind:group={mode} disabled={!audioOk} />
        <Icon name="music" size={14} />{t("media.audio")}
      </label>
    </div>
  </fieldset>

  {#if mode === "video"}
    <div class="vy-field">
      <label class="vy-label" for="vy-media-quality">{t("media.quality")}</label>
      <select id="vy-media-quality" class="vy-select" bind:value={quality}>
        <option value="best">{t("media.qualityBest")}</option>
        {#each heights as h (h)}
          {@const size = sizeAt(result.formats, h)}
          <option value={String(h)}>
            {size === undefined ? t("media.qualityHeight", { height: plain(h) }) : t("media.qualityHeightSize", { height: plain(h), size: i18n.bytes(size) })}
          </option>
        {/each}
      </select>
    </div>
  {:else}
    <div class="vy-field">
      <label class="vy-label" for="vy-media-audio">{t("media.audioFormat")}</label>
      <select id="vy-media-audio" class="vy-select" bind:value={convert}>
        <option value="original">{t("media.audioOriginal")}</option>
        <option value="mp3">{t("media.audioMp3")}</option>
      </select>
    </div>
  {/if}

  {#if result.subtitles.length > 0}
    <fieldset class="subs" aria-describedby="vy-media-subs-hint">
      <legend class="vy-label">{t("media.subtitles")}</legend>
      <div class="langs">
        {#each result.subtitles as s (s.lang)}
          {@const name = s.name ?? s.lang}
          <label class="vy-check">
            <input type="checkbox" checked={subtitles.includes(s.lang)} onchange={(e) => toggleSub(s.lang, e.currentTarget.checked)} />
            <span dir="auto">{s.automatic ? t("media.subtitleAuto", { name }) : name}</span>
          </label>
        {/each}
      </div>
      <span id="vy-media-subs-hint" class="vy-hint">{t("media.subtitlesHint")}</span>
    </fieldset>
  {/if}

  <p class="estimate" aria-live="polite">
    <span class="vy-num size">{est.bytes === undefined ? t("media.sizeUnknown") : t("media.size", { size: i18n.bytes(est.bytes) })}</span>
    <span class="via" class:stream={!est.direct}>
      <Icon name={est.direct ? "check" : "alert"} size={13} />
      {est.direct ? t("media.viaEngine") : t("media.viaYtDlp")}
    </span>
  </p>
</section>

<style>
  .media {
    display: grid;
    gap: var(--vy-space-3);
    padding: var(--vy-space-3);
    border-radius: var(--vy-radius-md);
    background: var(--vy-color-bg);
    border: 1px solid var(--vy-color-border);
  }
  .head {
    display: flex;
    align-items: flex-start;
    gap: var(--vy-space-2);
    color: var(--vy-color-text-muted);
  }
  .meta {
    min-inline-size: 0;
  }
  h3 {
    color: var(--vy-color-text);
    font-size: var(--vy-font-size-md);
    font-weight: var(--vy-font-weight-semibold);
  }
  .sub {
    display: flex;
    flex-wrap: wrap;
    gap: var(--vy-space-1) var(--vy-space-3);
    font-size: var(--vy-font-size-sm);
  }
  .live {
    color: var(--vy-color-warning-text);
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
    display: inline-flex;
    padding: 2px;
    border-radius: var(--vy-radius-sm);
    background: var(--vy-color-surface);
    box-shadow: inset 0 0 0 1px var(--vy-color-border-strong);
  }
  .choice label {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: var(--vy-space-1-5);
    min-block-size: 28px;
    padding-inline: var(--vy-space-3);
    border-radius: calc(var(--vy-radius-sm) - 1px);
    color: var(--vy-color-text-muted);
    cursor: pointer;
    white-space: nowrap;
  }
  .choice label.on {
    background: var(--vy-color-bg);
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
  .choice label:has(input:disabled) {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .vy-select {
    max-inline-size: 320px;
  }
  .langs {
    display: flex;
    flex-wrap: wrap;
    gap: var(--vy-space-2) var(--vy-space-4);
  }
  .estimate {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--vy-space-1) var(--vy-space-3);
  }
  .size {
    font-weight: var(--vy-font-weight-semibold);
  }
  .via {
    display: inline-flex;
    align-items: center;
    gap: var(--vy-space-1);
    color: var(--vy-color-success-text);
    font-size: var(--vy-font-size-sm);
  }
  .via.stream {
    color: var(--vy-color-text-muted);
  }
</style>
