// SPDX-License-Identifier: GPL-3.0-or-later

// "Download with WDM" (the button over videos) and "Download this video with
// WDM" (the popup): hand a page to the app's media picker with `media.offer`.
// The daemon downloads nothing by itself: the app's add dialog probes the
// page with yt-dlp and lets the user choose video or audio, the quality and
// subtitles. When no app window is open, the native host starts the app with
// only its small prompt window.
//
// Only the page's address and title leave the browser, and only on the
// user's click (PRIVACY.md).

import type { MediaOfferParams, MediaOfferResult } from "@wdm/api-client";

import { t } from "../shared/i18n";
import { MAX_TITLE, type ActionReply } from "../shared/messages";
import type { Settings } from "../shared/settings";
import { isSiteExcluded, parseHttpUrl } from "../shared/url";

export interface OfferDeps {
  daemon: { call(method: "media.offer", params: MediaOfferParams): Promise<MediaOfferResult> };
  settings(): Promise<Settings>;
  /** The store policy (store build only; nothing is blocked otherwise). */
  blocked(...urls: (string | undefined)[]): boolean;
}

export interface OfferRequest {
  /** The page; for the video button, the frame the video plays in. */
  url: string;
  title?: string | undefined;
  /** The tab's page, when it differs (a player embedded in a frame). */
  tabUrl?: string | undefined;
  /** The video button: never on excluded sites. The popup is an explicit request and is not. */
  fromButton: boolean;
}

export async function offerPage(deps: OfferDeps, req: OfferRequest): Promise<ActionReply> {
  const page = parseHttpUrl(req.url);
  if (!page) return { ok: false, error: t("outcomeBlocked") };
  if (deps.blocked(page.href, req.tabUrl)) return { ok: false, error: t("errorStoreBlocked") };
  if (req.fromButton) {
    const s = await deps.settings();
    if (!s.videoButton || isSiteExcluded([page.href, req.tabUrl], s.excludedSites)) {
      return { ok: false, error: t("outcomeBlocked") };
    }
  }
  const params: MediaOfferParams = { url: page.href };
  const title = req.title?.replace(/\s+/g, " ").trim().slice(0, MAX_TITLE);
  if (title) params.title = title;
  try {
    await deps.daemon.call("media.offer", params);
    return { ok: true };
  } catch (error) {
    return { ok: false, error: error instanceof Error ? error.message : String(error) };
  }
}
