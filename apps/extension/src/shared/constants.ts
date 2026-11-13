// SPDX-License-Identifier: GPL-3.0-or-later

/** Native messaging host name (`docs/api/PROTOCOL.md` §1). */
export const HOST_NAME = "com.coderzonebd.whoosh";

/**
 * File types captured by default (lower-case extensions, no dot). Roughly the
 * list users of other download managers expect; editable in settings.
 */
export const DEFAULT_FILE_TYPES: readonly string[] = [
  // archives and disk images
  "7z", "ace", "arj", "bz2", "cab", "gz", "img", "iso", "lz", "lzh", "lzma", "rar", "tar", "tbz", "tgz",
  "txz", "xz", "z", "zip", "zst",
  // installers and packages
  "apk", "appimage", "bin", "deb", "dmg", "exe", "flatpak", "msi", "msix", "msu", "pkg", "rpm", "run", "snap",
  // video
  "3gp", "asf", "avi", "flv", "m4v", "mkv", "mov", "mp4", "mpeg", "mpg", "ogv", "rm", "rmvb", "ts", "webm",
  "wmv",
  // audio
  "aac", "aif", "aiff", "flac", "m4a", "mp3", "ogg", "opus", "wav", "wma",
  // documents that tend to be large
  "pdf", "epub",
  // torrents and metalinks are handed over too; the daemon decides what to do
  "torrent", "metalink", "meta4",
];

/** MIME types that identify a file type when the name does not. */
export const MIME_EXTENSIONS: Readonly<Record<string, string>> = {
  "application/zip": "zip",
  "application/x-zip-compressed": "zip",
  "application/x-7z-compressed": "7z",
  "application/vnd.rar": "rar",
  "application/x-rar-compressed": "rar",
  "application/gzip": "gz",
  "application/x-gzip": "gz",
  "application/x-xz": "xz",
  "application/x-bzip2": "bz2",
  "application/x-tar": "tar",
  "application/zstd": "zst",
  "application/x-iso9660-image": "iso",
  "application/x-apple-diskimage": "dmg",
  "application/vnd.android.package-archive": "apk",
  "application/x-msdownload": "exe",
  "application/vnd.microsoft.portable-executable": "exe",
  "application/x-msi": "msi",
  "application/x-ms-installer": "msi",
  "application/vnd.debian.binary-package": "deb",
  "application/x-rpm": "rpm",
  "application/pdf": "pdf",
  "application/epub+zip": "epub",
  "application/x-bittorrent": "torrent",
  "application/metalink4+xml": "meta4",
  "video/mp4": "mp4",
  "video/x-matroska": "mkv",
  "video/webm": "webm",
  "video/quicktime": "mov",
  "video/x-msvideo": "avi",
  "audio/mpeg": "mp3",
  "audio/flac": "flac",
  "audio/ogg": "ogg",
  "audio/wav": "wav",
};
