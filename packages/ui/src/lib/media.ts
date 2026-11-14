// SPDX-License-Identifier: Apache-2.0
// The add dialog's format picker: turns what `media.probe` found into a few
// plain choices (best video at a height, or audio only) and estimates what
// they will download. The daemon (yt-dlp) makes the final pick.

import type { MediaChoice, MediaFormat } from "@whoosh/api-client";

export type MediaMode = "video" | "audio";
/** "best", or a height in pixels as a string ("1080"). */
export type Quality = string;
export type AudioConvert = "original" | "mp3";

/** A web page rather than a file, judging by the probe's content type. */
export function isWebPage(contentType: string | undefined): boolean {
  return contentType !== undefined && /\b(text\/html|application\/xhtml\+xml)\b/i.test(contentType);
}

/** Heights offered for video, highest first. */
export function videoHeights(formats: MediaFormat[]): number[] {
  const hs = new Set<number>();
  for (const f of formats) if (f.kind !== "audioOnly" && f.height) hs.add(f.height);
  return [...hs].sort((a, b) => b - a);
}

export function hasAudio(formats: MediaFormat[]): boolean {
  return formats.some((f) => f.kind !== "videoOnly");
}

export function choiceFor(mode: MediaMode, quality: Quality, convert: AudioConvert): MediaChoice {
  if (mode === "audio") return convert === "mp3" ? { kind: "audio", convert: "mp3" } : { kind: "audio" };
  const h = Number.parseInt(quality, 10);
  return Number.isFinite(h) && h > 0 ? { kind: "best", maxHeight: h } : { kind: "best" };
}

/** Roughly what yt-dlp will pick (`bv*+ba/b`, `ba/b`); formats are worst first. */
export function pickFormats(formats: MediaFormat[], choice: MediaChoice): MediaFormat[] {
  const audio = formats.filter((f) => f.kind === "audioOnly");
  const bestAudio = audio[audio.length - 1];
  if (choice.kind === "audio") {
    const last = formats[formats.length - 1];
    return bestAudio ? [bestAudio] : last ? [last] : [];
  }
  if (choice.kind === "formats") return formats.filter((f) => choice.ids.includes(f.id));
  const max = choice.maxHeight;
  const videos = formats.filter((f) => f.kind !== "audioOnly" && (max === undefined || (f.height ?? 0) <= max));
  const v = videos[videos.length - 1];
  if (!v) return [];
  return v.kind === "videoOnly" && bestAudio ? [v, bestAudio] : [v];
}

export interface Estimate {
  bytes?: number;
  /** Some size is yt-dlp's estimate, or unknown. */
  approximate: boolean;
  /** Whoosh's engine downloads every part (multi-connection, resumable). */
  direct: boolean;
}

export function estimate(formats: MediaFormat[], choice: MediaChoice): Estimate {
  const picked = pickFormats(formats, choice);
  let bytes = 0;
  let approximate = false;
  let known = picked.length > 0;
  for (const f of picked) {
    if (f.sizeBytes === undefined) known = false;
    else bytes += f.sizeBytes;
    if (f.sizeApproximate) approximate = true;
  }
  const out: Estimate = { approximate: approximate || !known, direct: picked.length > 0 && picked.every((f) => f.direct) };
  if (known) out.bytes = bytes;
  return out;
}

/** Size of the best format at each height (for the quality list). */
export function sizeAt(formats: MediaFormat[], height: number): number | undefined {
  return estimate(formats, { kind: "best", maxHeight: height }).bytes;
}
