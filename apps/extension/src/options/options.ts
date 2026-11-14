// SPDX-License-Identifier: GPL-3.0-or-later

// The options page: every setting in `storage.local` under "settings"
// (src/shared/settings.ts). Saved on each change; the background script and
// content scripts pick changes up through `storage.onChanged`.

import { DEFAULT_FILE_TYPES } from "../shared/constants";
import { STORE_BUILD } from "../shared/policy";
import {
  ALT_CLICK_VALUES,
  loadSettings,
  normalizeSettings,
  SETTINGS_KEY,
  updateSettings,
  type AltClick,
  type Settings,
} from "../shared/settings";
import { localizePage, t } from "../shared/i18n";
import {
  bytesToSize,
  formatFileTypes,
  formatSites,
  parseFileTypes,
  parseSites,
  SIZE_UNITS,
  sizeToBytes,
  type SizeUnit,
} from "./form";

type Api = typeof chrome;
const g = globalThis as { browser?: Api; chrome?: Api };
const api = (g.browser ?? g.chrome) as Api;

function el<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`#${id} missing`);
  return node as T;
}

const CHECKBOXES = ["captureEnabled", "captureIncognito", "sniffMedia", "preconnect", "selfHealing"] as const;

function fillText(): void {
  localizePage(document);
  document.title = t("optionsTitle");
  el("store-note").hidden = !STORE_BUILD;
}

function setError(field: HTMLElement, errorId: string, message: string | undefined): void {
  const error = el(errorId);
  error.hidden = message === undefined;
  error.textContent = message ?? "";
  if (message === undefined) field.removeAttribute("aria-invalid");
  else field.setAttribute("aria-invalid", "true");
}

function status(message: string): void {
  const node = el("status");
  node.textContent = message;
}

/** Shows `s` in every field, except the one the user is typing in. */
function render(s: Settings): void {
  const focused = document.activeElement;
  for (const id of CHECKBOXES) el<HTMLInputElement>(id).checked = s[id];

  const minSize = el<HTMLInputElement>("minSize");
  if (focused !== minSize && minSize.getAttribute("aria-invalid") !== "true") {
    const { value, unit } = bytesToSize(s.minSizeBytes);
    minSize.value = value;
    el<HTMLSelectElement>("minSizeUnit").value = unit;
  }
  const fileTypes = el<HTMLTextAreaElement>("fileTypes");
  if (focused !== fileTypes) fileTypes.value = formatFileTypes(s.fileTypes);
  el("fileTypes-note").hidden = s.fileTypes.length > 0;
  const sites = el<HTMLTextAreaElement>("excludedSites");
  if (focused !== sites && sites.getAttribute("aria-invalid") !== "true") sites.value = formatSites(s.excludedSites);
  for (const value of ALT_CLICK_VALUES) el<HTMLInputElement>(`altClick-${value}`).checked = s.altClick === value;
}

async function save(patch: Partial<Settings>): Promise<void> {
  try {
    const next = await updateSettings(api.storage.local, patch);
    render(next);
    status(t("saved"));
  } catch (error) {
    status(t("saveFailed", error instanceof Error ? error.message : String(error)));
  }
}

function saveMinSize(): void {
  const field = el<HTMLInputElement>("minSize");
  const unitValue = el<HTMLSelectElement>("minSizeUnit").value;
  const unit = (SIZE_UNITS as readonly string[]).includes(unitValue) ? (unitValue as SizeUnit) : "MB";
  const bytes = sizeToBytes(field.value, unit);
  if (bytes === undefined) {
    setError(field, "minSize-error", t("minSizeInvalid"));
    return;
  }
  setError(field, "minSize-error", undefined);
  void save({ minSizeBytes: bytes });
}

function saveSites(): void {
  const field = el<HTMLTextAreaElement>("excludedSites");
  const { sites, invalid } = parseSites(field.value);
  if (invalid.length > 0) {
    setError(field, "excludedSites-error", t("excludedSitesInvalid", invalid.join(", ")));
    return;
  }
  setError(field, "excludedSites-error", undefined);
  void save({ excludedSites: sites });
}

async function main(): Promise<void> {
  fillText();
  render(await loadSettings(api.storage.local));

  for (const id of CHECKBOXES) {
    el<HTMLInputElement>(id).addEventListener("change", (event) => {
      void save({ [id]: (event.target as HTMLInputElement).checked });
    });
  }
  el("minSize").addEventListener("change", saveMinSize);
  el("minSizeUnit").addEventListener("change", saveMinSize);
  el("fileTypes").addEventListener("change", (event) => {
    void save({ fileTypes: parseFileTypes((event.target as HTMLTextAreaElement).value) });
  });
  el("fileTypesReset").addEventListener("click", () => {
    el<HTMLTextAreaElement>("fileTypes").value = formatFileTypes(DEFAULT_FILE_TYPES);
    void save({ fileTypes: [...DEFAULT_FILE_TYPES] });
  });
  el("excludedSites").addEventListener("change", saveSites);
  for (const value of ALT_CLICK_VALUES) {
    el<HTMLInputElement>(`altClick-${value}`).addEventListener("change", (event) => {
      if ((event.target as HTMLInputElement).checked) void save({ altClick: value satisfies AltClick });
    });
  }

  // The popup (capture toggle, per-site switch) may change settings meanwhile.
  api.storage.onChanged.addListener((changes, area) => {
    const change = changes[SETTINGS_KEY];
    if (area === "local" && change) render(normalizeSettings(change.newValue));
  });
}

void main().catch((error: unknown) => {
  status(t("saveFailed", error instanceof Error ? error.message : String(error)));
});
