// SPDX-License-Identifier: Apache-2.0
// "Ask before downloading from the browser": the confirmation dialog for
// downloads that wait, and "Time taken" in the details of finished ones.
import type { WhooshClient } from "@whoosh/api-client";
import { createMockClient, type MockWhooshClient } from "@whoosh/api-client/mock";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "../src/components/App.svelte";
import type { Host } from "../src/lib/host.ts";
import type { AppState } from "../src/lib/state/app.svelte.ts";

let closeClient: (() => void) | undefined;

afterEach(() => {
  cleanup();
  closeClient?.();
  closeClient = undefined;
});

function spy<C extends WhooshClient>(client: C): C & { log: { method: string; params: unknown }[] } {
  const log: { method: string; params: unknown }[] = [];
  const call = client.call.bind(client) as (m: string, p: unknown) => Promise<unknown>;
  return Object.assign(Object.create(client) as C, {
    log,
    on: client.on.bind(client),
    close: client.close.bind(client),
    call: ((m: string, p: unknown) => {
      log.push({ method: m, params: p });
      return call(m, p);
    }) as C["call"],
  });
}

async function setup(opts: { host?: Host; locale?: string; mock?: MockWhooshClient } = {}) {
  const mock = opts.mock ?? createMockClient({ autoTick: false });
  const client = spy(mock);
  closeClient = () => client.close();
  let app: AppState | undefined;
  const user = userEvent.setup();
  const base = { client, mode: "mock" as const, storage: null, locale: opts.locale ?? "en", onready: (a: AppState) => (app = a) };
  render(App, { props: opts.host ? { ...base, host: opts.host } : base });
  const grid = await screen.findByRole("grid");
  await waitFor(() => expect(within(grid).getAllByRole("row").length).toBeGreaterThan(5));
  if (!app) throw new Error("no app");
  return { user, app, client, mock, grid };
}

/** What the browser extension does (through the native host, which sets `fromBrowser`). */
async function fromBrowser(mock: MockWhooshClient, url: string): Promise<string> {
  let id = "";
  await act(async () => {
    const r = await mock.call("download.add", { url, fromBrowser: true });
    expect(r.awaitingConfirmation).toBe(true);
    id = r.id;
  });
  return id;
}

const confirmCalls = (client: { log: { method: string; params: unknown }[] }) => client.log.filter((c) => c.method === "download.confirm").map((c) => c.params);

describe("confirming a download from the browser", () => {
  it("asks, brings the window to the front, and starts it with the edited name", async () => {
    const showWindow = vi.fn(() => Promise.resolve());
    const { user, mock, client } = await setup({ host: { showWindow } });
    const id = await fromBrowser(mock, "https://cdn.example.com/files/report-2026.pdf");

    const dialog = await screen.findByRole("dialog", { name: "New download from your browser" });
    expect(showWindow).toHaveBeenCalledTimes(1);
    expect(within(dialog).getByText("From cdn.example.com")).toBeInTheDocument();
    const name = within(dialog).getByRole("textbox", { name: "File name" });
    expect(name).toHaveValue("report-2026.pdf");
    expect(within(dialog).getByRole("button", { name: "Start download" })).toHaveFocus();

    await user.clear(name);
    await user.type(name, "annual report.pdf");
    await user.click(within(dialog).getByRole("button", { name: "Start download" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(confirmCalls(client)).toEqual([{ id, start: true, filename: "annual report.pdf" }]);
    const d = mock.snapshot().find((x) => x.id === id);
    expect(d?.state).toBe("queued");
    expect(d?.awaitingConfirmation).toBeUndefined();
  });

  it("keeps it for later on Escape and removes it on Cancel, one dialog at a time", async () => {
    const { user, mock, client } = await setup();
    const first = await fromBrowser(mock, "https://example.com/a/first.zip");
    const second = await fromBrowser(mock, "https://example.com/a/second.iso");

    let dialog = await screen.findByRole("dialog", { name: "New download from your browser" });
    expect(within(dialog).getByText("1 of 2")).toBeInTheDocument();
    expect(within(dialog).getByRole("textbox", { name: "File name" })).toHaveValue("first.zip");
    await user.keyboard("{Escape}");
    await waitFor(() => expect(within(screen.getByRole("dialog")).getByRole("textbox", { name: "File name" })).toHaveValue("second.iso"));
    expect(confirmCalls(client)).toEqual([{ id: first, start: false }]);
    expect(mock.snapshot().find((x) => x.id === first)?.state).toBe("paused");

    dialog = screen.getByRole("dialog");
    expect(within(dialog).queryByText(/of 2/)).not.toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(client.log.some((c) => c.method === "download.remove" && JSON.stringify(c.params) === JSON.stringify({ ids: [second], deleteFiles: true }))).toBe(true);
    expect(mock.snapshot().some((x) => x.id === second)).toBe(false);
  });

  it("asks about downloads that were already waiting when the window opened", async () => {
    const mock = createMockClient({ autoTick: false });
    await mock.call("download.add", { url: "https://example.com/waiting.bin", fromBrowser: true });
    const { user, client } = await setup({ mock });
    const dialog = await screen.findByRole("dialog", { name: "New download from your browser" });
    await user.click(within(dialog).getByRole("button", { name: "Download later" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(confirmCalls(client)).toHaveLength(1);
  });

  it("lets the user turn the question off in Settings", async () => {
    const { user, app, mock } = await setup();
    await act(() => (app.view = "settings"));
    const box = await screen.findByRole("checkbox", { name: "Ask before downloading from the browser" });
    expect(box).toBeChecked();
    await user.click(box);
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(async () => expect((await mock.call("settings.get", undefined)).confirmBrowserDownloads).toBe(false));
    const r = await mock.call("download.add", { url: "https://example.com/now.bin", fromBrowser: true });
    expect(r.awaitingConfirmation).toBeUndefined();
  });

  it("speaks Bangla", async () => {
    const mock = createMockClient({ autoTick: false, settings: { language: "bn" } });
    await setup({ mock, locale: "bn" });
    await fromBrowser(mock, "https://example.com/gaan.mp3");
    const dialog = await screen.findByRole("dialog", { name: "ব্রাউজার থেকে নতুন ডাউনলোড" });
    expect(within(dialog).getByRole("button", { name: "ডাউনলোড শুরু করুন" })).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "পরে ডাউনলোড করুন" })).toBeInTheDocument();
  });
});

describe("time taken", () => {
  async function details(user: ReturnType<typeof userEvent.setup>, grid: HTMLElement, name: string) {
    const row = within(grid)
      .getAllByRole("row")
      .find((r) => r.textContent?.includes(name));
    if (!row) throw new Error(`no row ${name}`);
    await user.click(row);
    return screen.findByRole("complementary");
  }

  it("shows how long a finished download took and its average speed", async () => {
    const { user, app, grid } = await setup();
    await act(() => (app.prefs.detailOpen = true));
    const panel = await details(user, grid, "node-v24.11.0-x64.msi");
    // 32 MB at the mock's 6.5 MB/s: about 5 s.
    expect(within(panel).getByText("Time taken").nextElementSibling).toHaveTextContent("5s");
    expect(within(panel).getByText("Average speed").nextElementSibling).toHaveTextContent(/6\.\d+ MB\/s/);
  });

  it("hides it for a record from before it was kept, and while downloading", async () => {
    const { user, app, grid } = await setup();
    await act(() => (app.prefs.detailOpen = true));
    let panel = await details(user, grid, "annual-report-2025.pdf");
    expect(within(panel).queryByText("Time taken")).not.toBeInTheDocument();
    panel = await details(user, grid, "blender-5.1.0-linux-x64.tar.xz");
    expect(within(panel).queryByText("Time taken")).not.toBeInTheDocument();
  });

  it("counts only active time in the mock and reports it once finished", async () => {
    const mock = createMockClient({ autoTick: false, scenario: "empty" });
    const { id } = await mock.call("download.add", { url: "https://example.com/small.bin" });
    await mock.call("download.pause", { ids: [id] });
    mock.advance(60_000);
    expect(mock.snapshot()[0]?.activeMs).toBeLessThan(1000);
    await mock.call("download.resume", { ids: [id] });
    for (let i = 0; i < 2000 && mock.snapshot()[0]?.state !== "completed"; i++) mock.advance(1000);
    const d = mock.snapshot()[0];
    expect(d?.state).toBe("completed");
    expect(d?.activeMs).toBeGreaterThan(0);
  });
});
