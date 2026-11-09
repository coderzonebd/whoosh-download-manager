// SPDX-License-Identifier: Apache-2.0
// File categories by extension. The names follow IDM's defaults, which users
// of every download manager recognise. The daemon will own real category
// rules (Phase 1); UIs use this to show an icon for downloads that have none.

export const CATEGORIES = ["video", "music", "documents", "compressed", "programs", "images", "other"] as const;
export type Category = (typeof CATEGORIES)[number];

const EXTENSIONS: Record<Exclude<Category, "other">, string> = {
  compressed: "zip rar 7z tar gz tgz xz bz2 zst iso dmg img",
  documents: "pdf doc docx xls xlsx ppt pptx odt ods odp txt rtf epub csv md",
  music: "mp3 flac wav ogg opus m4a aac wma",
  video: "mp4 mkv webm avi mov m4v ts m3u8 wmv flv",
  programs: "exe msi deb rpm appimage apk pkg flatpak snap",
  images: "jpg jpeg png gif webp svg avif heic bmp tif tiff",
};

const BY_EXT = new Map<string, Category>();
for (const [cat, exts] of Object.entries(EXTENSIONS)) {
  for (const ext of exts.split(" ")) BY_EXT.set(ext, cat as Category);
}

/** `"Report.Final.PDF"` → `"documents"`; anything unknown → `"other"`. */
export function categoryForFilename(filename: string): Category {
  const lower = filename.toLowerCase();
  if (/\.tar\.(gz|xz|zst|bz2)$/.test(lower)) return "compressed";
  const dot = lower.lastIndexOf(".");
  if (dot < 0) return "other";
  return BY_EXT.get(lower.slice(dot + 1)) ?? "other";
}

export function isCategory(value: unknown): value is Category {
  return typeof value === "string" && (CATEGORIES as readonly string[]).includes(value);
}
