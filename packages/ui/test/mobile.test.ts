// SPDX-License-Identifier: Apache-2.0
// The phone layout (the Android app): cards, filter chips, the details sheet,
// and Android's back gesture closing the topmost layer.
import { createMockClient, type MockWdmClient } from "@wdm/api-client/mock";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import MobileApp from "../src/mobile/MobileApp.svelte";
import type { AppState } from "../src/lib/state/app.svelte.ts";

let closeClient: (() => void) | undefined;

afterEach(() => {
  cleanup();
  closeClient?.();
  closeClient = undefined;
});

function mock(): MockWdmClient {
  const m = createMockClient({ autoTick: false, latencyMs: 0, settings: { language: "en" } });
  closeClient = () => m.close();
  return m;
}

async function start(): Promise<{ m: MockWdmClient; app: AppState }> {
  const m = mock();
  let app: AppState | undefined;
  render(MobileApp, { props: { client: m, mode: "mock", storage: null, locale: "en", host: { platform: "android" }, onready: (a: AppState) => (app = a) } });
  await waitFor(() => expect(app?.loaded).toBe(true));
  if (!app) throw new Error("no app");
  return { m, app };
}

/** What Android's back gesture does: the shell goes back in the webview's history. */
async function back(): Promise<void> {
  await act(async () => {
    history.back();
    await new Promise((r) => setTimeout(r, 30));
  });
}

describe("the phone layout", () => {
  it("lists every download as a card and filters with the chips", async () => {
    const { app } = await start();
    const cards = await screen.findAllByRole("article");
    expect(cards).toHaveLength(app.store.size);

    const chips = screen.getByRole("tablist");
    const all = within(chips).getByRole("tab", { name: /^All/ });
    expect(all.getAttribute("aria-selected")).toBe("true");
    const done = within(chips).getByRole("tab", { name: /^Completed/ });
    await userEvent.click(done);
    await waitFor(() => expect(screen.getAllByRole("article")).toHaveLength(app.store.counts.groups.completed));
    expect(done.getAttribute("aria-selected")).toBe("true");
  });

  it("opens a download's details in a sheet, and back closes it", async () => {
    const { app } = await start();
    const first = app.store.visible[0];
    if (!first) throw new Error("no downloads");
    const card = (await screen.findAllByRole("article"))[0];
    if (!card) throw new Error("no card");
    await userEvent.click(within(card).getAllByRole("button")[0] as HTMLElement);
    const sheet = await screen.findByRole("dialog", { name: first.data.filename });
    expect(within(sheet).getByRole("heading", { name: first.data.filename })).toBeTruthy();

    await back();
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("closes the add sheet, then the history page, one back at a time", async () => {
    const { app } = await start();
    await userEvent.click(screen.getByRole("button", { name: "More options" }));
    await userEvent.click(await screen.findByRole("menuitem", { name: "History" }));
    await waitFor(() => expect(app.view).toBe("history"));

    act(() => app.openAdd("https://example.com/a.zip"));
    await screen.findByRole("dialog", { name: "Add a download" });

    await back();
    await waitFor(() => expect(app.addOpen).toBe(false));
    expect(app.view).toBe("history");
    await back();
    await waitFor(() => expect(app.view).toBe("downloads"));
  });

  it("leaves out the desktop-only settings", async () => {
    const { app } = await start();
    act(() => (app.view = "settings"));
    await screen.findByRole("heading", { name: "Appearance and language" });
    expect(screen.queryByText("Download list")).toBeNull();
    expect(screen.queryByLabelText("Default folder")).toBeNull();
  });
});
