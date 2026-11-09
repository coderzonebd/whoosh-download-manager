// SPDX-License-Identifier: Apache-2.0
import type { DownloadState } from "@vayu/api-client";

export type Tone = "accent" | "success" | "warning" | "danger" | "info" | "neutral";

/** One colour language for badges, bars and icons. */
export function toneFor(state: DownloadState): Tone {
  switch (state) {
    case "downloading":
    case "probing":
      return "accent";
    case "verifying":
      return "info";
    case "completed":
      return "success";
    case "failed":
      return "danger";
    case "needs_refresh":
      return "warning";
    case "paused":
    case "queued":
      return "neutral";
  }
}
