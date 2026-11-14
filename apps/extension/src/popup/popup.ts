// SPDX-License-Identifier: GPL-3.0-or-later

// Minimal, dependency-free popup. It will move to packages/ui later.

import { filenameFromUrl } from "../shared/filename";
import { formatBytes } from "../shared/format";
import type { ActionReply, HealAnswer, HealQuestion, MediaEntry, PopupMessage, PopupState } from "../shared/messages";
import { t } from "../shared/strings";

type Api = typeof chrome;
const g = globalThis as { browser?: Api; chrome?: Api };
const api = (g.browser ?? g.chrome) as Api;

function $(id: string): HTMLElement {
  const el = document.getElementById(id);
  if (!el) throw new Error(`#${id} missing`);
  return el;
}

function ask<T>(message: PopupMessage): Promise<T> {
  return api.runtime.sendMessage(message) as Promise<T>;
}

let tab: chrome.tabs.Tab | undefined;

function renderStatus(state: PopupState): void {
  const status = $("status");
  const detail = $("detail");
  const s = state.status;
  if (!s.hostAvailable) {
    status.dataset["state"] = "bad";
    status.textContent = "Not connected";
    detail.textContent = s.error ?? "The whoosh native host is not installed.";
  } else if (!s.daemonRunning) {
    status.dataset["state"] = "warn";
    status.textContent = "App not running";
    detail.textContent = "Start whoosh; downloads stay in the browser until then.";
  } else {
    status.dataset["state"] = "ok";
    status.textContent = "Connected";
    detail.textContent = s.daemonVersion ? `whoosh ${s.daemonVersion}` : "";
  }

  const counts = $("counts");
  counts.hidden = state.active === undefined;
  $("active").textContent = String(state.active ?? 0);
  $("queued").textContent = String(state.queued ?? 0);
}

function renderToggles(state: PopupState): void {
  $("alt-hint").hidden = state.altClick === "off";
  $("alt-hint-text").textContent = state.altClick === "whoosh" ? t("altHintWhoosh") : t("altHintBrowser");
  const capture = $("capture") as HTMLInputElement;
  capture.checked = state.captureEnabled;

  const row = $("site-row");
  row.hidden = state.host === undefined;
  if (state.host) {
    $("site-host").textContent = state.host;
    const site = $("site") as HTMLInputElement;
    site.checked = !state.siteExcluded;
    site.disabled = !state.captureEnabled;
  }
}

function mediaItem(entry: MediaEntry): HTMLLIElement {
  const li = document.createElement("li");
  const kind = document.createElement("span");
  kind.className = "kind";
  kind.textContent = entry.kind;
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = filenameFromUrl(entry.url) || entry.url;
  name.title = entry.url;
  if (entry.size !== undefined) {
    const size = document.createElement("span");
    size.className = "size";
    size.textContent = ` ${formatBytes(entry.size)}`;
    name.append(size);
  }
  const button = document.createElement("button");
  button.type = "button";
  button.textContent = "Download";
  button.addEventListener("click", () => {
    button.disabled = true;
    const message: PopupMessage = { type: "whoosh/download-media", url: entry.url };
    if (tab?.url) message.pageUrl = tab.url;
    void ask<ActionReply>(message).then((reply) => {
      button.textContent = reply.ok ? "Sent" : "Failed";
      if (!reply.ok) button.title = reply.error;
    });
  });
  li.append(kind, name, button);
  return li;
}

function renderMedia(state: PopupState): void {
  const list = $("media");
  list.replaceChildren(...state.media.map(mediaItem));
  $("media-empty").hidden = state.media.length > 0;
}

function healButton(question: HealQuestion, label: string, answer: HealAnswer): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.textContent = label;
  button.addEventListener("click", () => {
    button.disabled = true;
    void ask<ActionReply>({ type: "whoosh/heal-answer", id: question.id, answer }).then((reply) => {
      if (!reply.ok) $("detail").textContent = reply.error;
      if (answer === "open-page") window.close();
      else void refresh();
    });
  });
  return button;
}

function healItem(question: HealQuestion): HTMLLIElement {
  const li = document.createElement("li");
  const text = document.createElement("p");
  if (question.kind === "confirm" && question.candidateUrl) {
    text.textContent = `${t("healConfirm")} ${question.filename}`;
    const link = document.createElement("span");
    link.className = "size";
    link.textContent = ` → ${question.candidateUrl}`;
    text.append(link);
  } else {
    text.textContent = `${t("healOpenPage")} ${question.filename}.`;
  }
  const actions = document.createElement("div");
  actions.className = "actions";
  if (question.kind === "confirm" && question.candidateUrl) {
    actions.append(healButton(question, t("healUseButton"), "use-candidate"));
  }
  if (question.pageUrl) actions.append(healButton(question, t("healOpenPageButton"), "open-page"));
  actions.append(healButton(question, t("healDismissButton"), "dismiss"));
  li.append(text, actions);
  return li;
}

function renderHeals(state: PopupState): void {
  $("heals-section").hidden = state.heals.length === 0;
  $("heals-title").textContent = t("healTitle");
  $("heals").replaceChildren(...state.heals.map(healItem));
  const blocked = $("page-blocked");
  blocked.hidden = !state.pageBlocked;
  blocked.textContent = state.pageBlocked ? t("pageBlocked") : "";
  $("site-row").hidden ||= state.pageBlocked;
}

function renderLast(state: PopupState): void {
  const last = state.lastCapture;
  $("last").textContent = last ? `Last: ${filenameFromUrl(last.url) || last.url} — ${last.outcome}` : "";
}

async function refresh(): Promise<void> {
  const message: PopupMessage = { type: "whoosh/popup-state" };
  if (tab?.id !== undefined) message.tabId = tab.id;
  if (tab?.url) message.pageUrl = tab.url;
  const state = await ask<PopupState>(message);
  renderStatus(state);
  renderToggles(state);
  renderMedia(state);
  renderHeals(state);
  renderLast(state);
}

async function main(): Promise<void> {
  [tab] = await api.tabs.query({ active: true, currentWindow: true });

  ($("capture") as HTMLInputElement).addEventListener("change", (event) => {
    const enabled = (event.target as HTMLInputElement).checked;
    void ask<ActionReply>({ type: "whoosh/set-capture", enabled }).then(refresh);
  });
  $("open-options").textContent = t("openSettings");
  $("open-options").addEventListener("click", () => {
    void api.runtime.openOptionsPage().then(() => window.close());
  });
  ($("site") as HTMLInputElement).addEventListener("change", (event) => {
    const host = $("site-host").textContent ?? "";
    const excluded = !(event.target as HTMLInputElement).checked;
    void ask<ActionReply>({ type: "whoosh/set-site-excluded", host, excluded }).then(refresh);
  });

  await refresh();
}

void main().catch((error: unknown) => {
  $("status").dataset["state"] = "bad";
  $("status").textContent = "Error";
  $("detail").textContent = error instanceof Error ? error.message : String(error);
});
