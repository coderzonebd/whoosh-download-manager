// SPDX-License-Identifier: Apache-2.0
// Public surface of @whoosh/ui. Import "@whoosh/ui/styles.css" once in the host.

export { default as App } from "./components/App.svelte";
export { default as DownloadList } from "./components/DownloadList.svelte";
export { default as SegmentBar } from "./components/SegmentBar.svelte";
export { default as StatusBadge } from "./components/StatusBadge.svelte";
export { default as FileIcon } from "./components/FileIcon.svelte";
export { default as Icon } from "./components/Icon.svelte";
export { default as Dialog } from "./components/Dialog.svelte";
export { default as AddDownloadDialog } from "./components/AddDownloadDialog.svelte";
export { default as DetailPanel } from "./components/DetailPanel.svelte";
export { default as SettingsView } from "./components/SettingsView.svelte";
export { default as Toasts } from "./components/Toasts.svelte";

export { AppState, getApp, provideApp, type AppOptions, type Connection, type Theme } from "./lib/state/app.svelte.ts";
export { DownloadStore, Download, type View, type Sort, type SortKey, type StateGroup } from "./lib/state/downloads.svelte.ts";
export { ToastStore, type Toast, type ToastTone } from "./lib/state/toasts.svelte.ts";
export { Prefs, type Layout } from "./lib/state/prefs.svelte.ts";
export { I18n, type LocaleInfo } from "./lib/i18n/i18n.svelte.ts";
export { en, type MessageKey, type Messages } from "./lib/i18n/en.ts";
export { bn } from "./lib/i18n/bn.ts";
export { scaleBytes, splitDuration } from "./lib/format.ts";
export { parseDeepLink, isDownloadUrl, type DeepLink, type DesktopPrefs, type Host } from "./lib/host.ts";
export { QueueStore, type PendingAction } from "./lib/state/queues.svelte.ts";
