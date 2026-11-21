// SPDX-License-Identifier: Apache-2.0
// What the browser hands over (a download that waits for confirmation, a
// video page offered by the extension's "Download with WDM" button) is asked
// about in the desktop app's own prompt window, never in the main one; a UI
// without a prompt window asks itself.
import { createMockClient, type MockWdmClient } from "@wdm/api-client/mock";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "../src/components/App.svelte";
import PromptApp from "../src/components/PromptApp.svelte";
import type { Host } from "../src/lib/host.ts";
import type { AppState } from "../src/lib/state/app.svelte.ts";
import { readFollowed, writeFollowed } from "../src/lib/state/followed.ts";

let closeClient: (() => void) | undefined;

afterEach(() => {
  cleanup();
  closeClient?.();
  closeClient = undefined;
});

const PAGE = "https://video.example.com/watch?v=lecture42";

function mock(): MockWdmClient {
  const m = createMockClient({ autoTick: false, latencyMs: 0, media: true, settings: { language: "en" } });
  closeClient = () => m.close();
  return m;
}

function log(m: MockWdmClient): { calls: string[] } {
  const calls: string[] = [];
  const call = m.call.bind(m) as (method: string, p: unknown) => Promise<unknown>;
  (m as { call: unknown }).call = (method: string, p: unknown) => {
    calls.push(method);
    return call(method, p);
  };
  return { calls };
}

describe("the main window of a host with a prompt window", () => {
  it("opens the prompt window and never asks or shows itself", async () => {
    const m = mock();
    const { calls } = log(m);
    const openPrompt = vi.fn(() => Promise.resolve());
    const showWindow = vi.fn(() => Promise.resolve());
    let app: AppState | undefined;
    render(App, { props: { client: m, mode: "mock", storage: null, locale: "en", host: { openPrompt, showWindow }, onready: (a: AppState) => (app = a) } });
    await screen.findByRole("grid");

    await act(async () => {
      await m.call("download.add", { url: "https://cdn.example.com/files/report-2026.pdf", fromBrowser: true });
    });
    await waitFor(() => expect(openPrompt).toHaveBeenCalledTimes(1));
    await act(async () => {
      await m.call("media.offer", { url: PAGE, title: "Lecture 42" });
    });
    await waitFor(() => expect(openPrompt).toHaveBeenCalledTimes(2));

    expect(showWindow).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    // The offer is left for the prompt window to take.
    expect(calls).not.toContain("media.takeOffer");
    expect(await m.call("media.offers", undefined)).toHaveLength(1);
    expect(app?.offers).toEqual([]);
  });

  it("opens the prompt window for offers that were waiting when it connected", async () => {
    const m = mock();
    await m.call("media.offer", { url: PAGE });
    const openPrompt = vi.fn(() => Promise.resolve());
    render(App, { props: { client: m, mode: "mock", storage: null, locale: "en", host: { openPrompt } } });
    await waitFor(() => expect(openPrompt).toHaveBeenCalled());
    expect(await m.call("media.offers", undefined)).toHaveLength(1);
  });
});

describe("a window without a prompt window", () => {
  it("takes an offered page and opens the add dialog for it", async () => {
    const m = mock();
    const showWindow = vi.fn(() => Promise.resolve());
    render(App, { props: { client: m, mode: "mock", storage: null, locale: "en", host: { showWindow } } });
    await screen.findByRole("grid");
    await act(async () => {
      await m.call("media.offer", { url: PAGE });
    });
    const dialog = await screen.findByRole("dialog", { name: "Add a download" });
    expect(within(dialog).getByRole("textbox", { name: "Address" })).toHaveValue(PAGE);
    expect(showWindow).toHaveBeenCalled();
    expect(await m.call("media.offers", undefined)).toHaveLength(0);
  });
});

describe("the prompt window", { timeout: 30_000 }, () => {
  it("asks about the waiting download, then the offered page, then closes", async () => {
    const m = mock();
    await m.call("download.add", { url: "https://cdn.example.com/files/report-2026.pdf", fromBrowser: true });
    await m.call("media.offer", { url: PAGE, title: "Lecture 42" });
    const closePrompt = vi.fn(() => Promise.resolve());
    const showWindow = vi.fn(() => Promise.resolve());
    const openPrompt = vi.fn(() => Promise.resolve());
    const host: Host = { closePrompt, showWindow, openPrompt };
    const user = userEvent.setup();
    render(PromptApp, { props: { client: m, mode: "mock", storage: null, locale: "en", host, closeDelayMs: 0 } });

    // First the download from the browser.
    let dialog = await screen.findByRole("dialog", { name: "New download from your browser" });
    expect(within(dialog).getByRole("textbox", { name: "File name" })).toHaveValue("report-2026.pdf");
    await user.click(within(dialog).getByRole("button", { name: "Start download" }));

    // Then the offered page, in the add dialog with its format picker.
    dialog = await screen.findByRole("dialog", { name: "Add a download" });
    expect(within(dialog).getByRole("textbox", { name: "Address" })).toHaveValue(PAGE);
    expect(await m.call("media.offers", undefined)).toHaveLength(0);
    await act(async () => {
      m.advance(5_000);
      await new Promise((r) => setTimeout(r, 400));
    });
    const picker = await within(dialog).findByRole("region", { name: "Demo video lecture42" }, { timeout: 5000 });
    expect(within(picker).getByRole("radio", { name: "Video" })).toBeChecked();
    expect(closePrompt).not.toHaveBeenCalled();
    await user.click(within(dialog).getByRole("button", { name: "Download" }));

    // The window stays with what it started (IDM's progress window), the browser download first.
    dialog = await screen.findByRole("dialog", { name: "Downloading" });
    expect(within(dialog).getByText("report-2026.pdf")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Pause" })).toBeInTheDocument();
    expect(closePrompt).not.toHaveBeenCalled();
    await user.click(hide(dialog));
    await waitFor(() => expect(screen.queryByText("report-2026.pdf")).not.toBeInTheDocument());
    dialog = await screen.findByRole("dialog", { name: "Downloading" });
    await user.click(hide(dialog));

    // Nothing left to show: the window closes, and the main window was never asked for.
    await waitFor(() => expect(closePrompt).toHaveBeenCalled());
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(showWindow).not.toHaveBeenCalled();
    expect(openPrompt).not.toHaveBeenCalled();
    expect(m.snapshot().some((d) => d.media !== undefined && d.url === PAGE)).toBe(true);
  });

  it("closes when the offered page is not wanted, and shows one that arrives while open", async () => {
    const m = mock();
    const closePrompt = vi.fn(() => Promise.resolve());
    const user = userEvent.setup();
    render(PromptApp, { props: { client: m, mode: "mock", storage: null, locale: "bn", host: { closePrompt }, closeDelayMs: 50 } });
    // Opened with nothing waiting (it was taken elsewhere): closes by itself.
    await waitFor(() => expect(closePrompt).toHaveBeenCalledTimes(1));
    await act(async () => {
      await m.call("media.offer", { url: PAGE });
    });
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByRole("textbox", { name: /./ })).toBeDefined();
    await user.keyboard("{Escape}");
    await waitFor(() => expect(closePrompt).toHaveBeenCalledTimes(2));
  });

  it("keeps asking to close while nothing is left, and offers a Close button", async () => {
    const m = mock();
    // The first requests do nothing (the window stayed open on Windows).
    const closePrompt = vi.fn(() => Promise.resolve());
    const user = userEvent.setup();
    render(PromptApp, { props: { client: m, mode: "mock", storage: null, locale: "en", host: { closePrompt }, closeDelayMs: 0 } });
    await screen.findByText("Nothing is waiting.");
    await waitFor(() => expect(closePrompt).toHaveBeenCalledTimes(2), { timeout: 4000 });
    const before = closePrompt.mock.calls.length;
    await user.click(screen.getByRole("button", { name: "Close" }));
    expect(closePrompt.mock.calls.length).toBeGreaterThan(before);
  });

  it("says when a followed download is complete, also after it was hidden", async () => {
    const m = mock();
    const { id } = await m.call("download.add", { url: "https://cdn.example.com/files/report-2026.pdf", fromBrowser: true });
    const storage = memoryStorage();
    const closePrompt = vi.fn(() => Promise.resolve());
    const openDownload = vi.fn((_id: string, _reveal: boolean) => Promise.resolve());
    const user = userEvent.setup();
    render(PromptApp, { props: { client: m, mode: "mock", storage, locale: "en", host: { closePrompt, openDownload }, closeDelayMs: 0 } });

    let dialog = await screen.findByRole("dialog", { name: "New download from your browser" });
    await user.click(within(dialog).getByRole("button", { name: "Start download" }));
    dialog = await screen.findByRole("dialog", { name: "Downloading" });
    await user.click(hide(dialog));
    await waitFor(() => expect(closePrompt).toHaveBeenCalledTimes(1));
    // The main window reads this to bring the prompt window back.
    expect(readFollowed(storage)).toEqual([{ id, hidden: true }]);

    await act(async () => {
      for (let i = 0; i < 2000 && m.snapshot().find((d) => d.id === id)?.state !== "completed"; i++) m.advance(5_000);
    });
    dialog = await screen.findByRole("dialog", { name: "Download complete" });
    expect(within(dialog).getByText("report-2026.pdf")).toBeInTheDocument();
    expect(within(dialog).getByText("Time taken")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Open folder" }));
    expect(openDownload).toHaveBeenCalledWith(id, true);
    await waitFor(() => expect(closePrompt).toHaveBeenCalledTimes(2));
    expect(readFollowed(storage)).toEqual([]);
  });
});

describe("cancelling from the prompt window", { timeout: 30_000 }, () => {
  it("takes two clicks, then removes the download and closes", async () => {
    const m = mock();
    const { id } = await m.call("download.add", { url: "https://cdn.example.com/files/report-2026.pdf", fromBrowser: true });
    const closePrompt = vi.fn(() => Promise.resolve());
    const user = userEvent.setup();
    render(PromptApp, { props: { client: m, mode: "mock", storage: memoryStorage(), locale: "en", host: { closePrompt }, closeDelayMs: 0 } });
    let dialog = await screen.findByRole("dialog", { name: "New download from your browser" });
    await user.click(within(dialog).getByRole("button", { name: "Start download" }));
    dialog = await screen.findByRole("dialog", { name: "Downloading" });

    // The first click only arms it.
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(m.snapshot().some((d) => d.id === id)).toBe(true);
    await user.click(within(dialog).getByRole("button", { name: "Click again to cancel" }));

    await waitFor(() => expect(m.snapshot().some((d) => d.id === id)).toBe(false));
    await waitFor(() => expect(closePrompt).toHaveBeenCalled());
  });
});

describe("the main window, for a download the prompt window follows", () => {
  it("brings the prompt window back when it ends", async () => {
    const m = mock();
    const { id } = await m.call("download.add", { url: "https://cdn.example.com/files/other.zip" });
    const other = await m.call("download.add", { url: "https://cdn.example.com/files/not-followed.zip" });
    const storage = memoryStorage();
    writeFollowed(storage, [{ id, hidden: true }]);
    const openPrompt = vi.fn(() => Promise.resolve());
    render(App, { props: { client: m, mode: "mock", storage, locale: "en", host: { openPrompt } } });
    await screen.findByRole("grid");
    await act(async () => {
      for (let i = 0; i < 2000 && m.snapshot().find((d) => d.id === id)?.state !== "completed"; i++) m.advance(5_000);
    });
    await waitFor(() => expect(openPrompt).toHaveBeenCalledTimes(1));
    // Only the followed one: the other ending later does not.
    await act(async () => {
      for (let i = 0; i < 2000 && m.snapshot().find((d) => d.id === other.id)?.state !== "completed"; i++) m.advance(5_000);
    });
    expect(m.snapshot().find((d) => d.id === other.id)?.state).toBe("completed");
    expect(openPrompt).toHaveBeenCalledTimes(1);
  });
});

/** The footer's Hide (the close button in the corner says Hide too). */
function hide(dialog: HTMLElement): HTMLElement {
  const all = within(dialog).getAllByRole("button", { name: "Hide" });
  return all[all.length - 1]!;
}

function memoryStorage(): Storage {
  const map = new Map<string, string>();
  return {
    get length() {
      return map.size;
    },
    clear: () => map.clear(),
    getItem: (k) => map.get(k) ?? null,
    key: (i) => [...map.keys()][i] ?? null,
    removeItem: (k) => void map.delete(k),
    setItem: (k, v) => void map.set(k, String(v)),
  };
}
