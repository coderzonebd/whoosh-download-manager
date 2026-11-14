// SPDX-License-Identifier: Apache-2.0
// Video and audio sites through yt-dlp/ffmpeg (PROTOCOL.md "Media"). Only
// desktop and GitHub daemons have the `media` capability; elsewhere every
// media method answers -32006, `supported` is false, and the UI shows
// nothing about media at all.

import { JsonRpcErrorCode, isWhooshRpcError } from "@whoosh/api-client/errors";
import { ErrorCode, MEDIA_CAPABILITY, type MediaToolsStatus } from "@whoosh/api-client";

export { MEDIA_CAPABILITY };

/** -32006 (no media in this build) or -32601 (a daemon from before media). */
export function isMediaUnsupported(err: unknown): boolean {
  return isWhooshRpcError(err) && (err.code === ErrorCode.Unsupported || err.code === JsonRpcErrorCode.MethodNotFound);
}

export class MediaStore {
  /** undefined until known; false when the daemon has no media support. */
  supported = $state<boolean | undefined>(undefined);
  /** yt-dlp and ffmpeg as the daemon last reported them. */
  tools = $state.raw<MediaToolsStatus | undefined>(undefined);

  get enabled(): boolean {
    return this.supported === true;
  }

  replace(status: MediaToolsStatus): void {
    this.supported = true;
    this.tools = status;
  }

  disable(): void {
    this.supported = false;
    this.tools = undefined;
  }
}
