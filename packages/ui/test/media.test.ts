// SPDX-License-Identifier: Apache-2.0
// Video and audio sites (yt-dlp/ffmpeg) through the whole shell on the mock
// daemon: the add dialog's format picker, the media tools in Settings, and
// a daemon without media support (the store build's), which must show none
// of it.
import { createMockClient, type MockWhooshClient } from "@whoosh/api-client/mock";
import type { MediaAddParams, WhooshClient } from "@whoosh/api-client";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import App from "../src/components/App.svelte";
import { choiceFor, estimate, isWebPage, pickFormats, videoHeights } from "../src/lib/media.ts";
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

async function setup(opts: { media?: boolean; language?: string; scenario?: "demo" | "empty" } = {}) {
  const mock: MockWhooshClient = createMockClient({
    autoTick: false,
    media: opts.media ?? true,
    scenario: opts.scenario ?? "demo",
    settings: { language: opts.language ?? "en" },
  });
  const client = spy(mock);
  closeClient = () => client.close();
  let app: AppState | undefined;
  const user = userEvent.setup();
  render(App, { props: { client, mode: "mock" as const, storage: null, locale: opts.language ?? "en", onready: (a: AppState) => (app = a) } });
  const grid = await screen.findByRole("grid");
  if (!app) throw new Error("no app");
  const a = app;
  await waitFor(() => expect(a.media.supported).toBeDefined());
  return { grid, user, app: a, client, mock };
}

const PAGE = "https://video.example.com/watch?v=lecture42";

async function openAdd(user: ReturnType<typeof userEvent.setup>, url: string) {
  await user.click(screen.getByRole("button", { name: "Add download" }));
  const dialog = await screen.findByRole("dialog", { name: "Add a download" });
  await user.type(within(dialog).getByRole("textbox", { name: "Address" }), url);
  return dialog;
}

describe("format picker", { timeout: 30_000 }, () => {
  it("finds the video on a page and adds it as audio-only MP3 with subtitles", async () => {
    const { user, client, grid } = await setup();
    const dialog = await openAdd(user, PAGE);
    const picker = await within(dialog).findByRole("region", { name: "Demo video lecture42" }, { timeout: 3000 });
    expect(within(picker).getByRole("radio", { name: "Video" })).toBeChecked();
    const quality = within(picker).getByRole("combobox", { name: "Quality" });
    const options = within(quality).getAllByRole("option").map((o) => o.textContent?.trim());
    expect(options[0]).toBe("Best available");
    expect(options.some((o) => o?.startsWith("1080p"))).toBe(true);
    await user.selectOptions(quality, "720");

    await user.click(within(picker).getByRole("radio", { name: "Audio only" }));
    await user.selectOptions(within(picker).getByRole("combobox", { name: "Audio format" }), "mp3");
    await user.click(within(picker).getByRole("checkbox", { name: "English" }));
    await user.click(within(picker).getByRole("checkbox", { name: "Bangla (auto-generated)" }));
    expect(within(dialog).getByRole("textbox", { name: "File name" })).toHaveValue("Demo video lecture42");

    await user.click(screen.getByRole("button", { name: "Download" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    const add = client.log.find((c) => c.method === "media.add")?.params as MediaAddParams;
    expect(add).toMatchObject({ url: PAGE, choice: { kind: "audio", convert: "mp3" }, subtitles: ["en", "bn"], start: true });
    expect(add.filename).toBeUndefined(); // the title is the default
    expect(client.log.some((c) => c.method === "download.add")).toBe(false);
    const row = within(grid)
      .getAllByRole("row")
      .find((r) => r.textContent?.includes("Demo video lecture42.mp3"));
    expect(row).toBeDefined();
  });

  it("adds the best video at a chosen quality, with a user-chosen name", async () => {
    const { user, client } = await setup();
    const dialog = await openAdd(user, PAGE);
    const picker = await within(dialog).findByRole("region", { name: "Demo video lecture42" }, { timeout: 3000 });
    await user.selectOptions(within(picker).getByRole("combobox", { name: "Quality" }), "1080");
    expect(within(picker).getByText(/several connections/)).toBeInTheDocument();
    const name = within(dialog).getByRole("textbox", { name: "File name" });
    await user.clear(name);
    await user.type(name, "lecture 42");
    await user.click(screen.getByRole("button", { name: "Download" }));
    await waitFor(() => expect(client.log.some((c) => c.method === "media.add")).toBe(true));
    const add = client.log.find((c) => c.method === "media.add")?.params as MediaAddParams;
    expect(add.choice).toEqual({ kind: "best", maxHeight: 1080 });
    expect(add.filename).toBe("lecture 42");
  });

  it("can still download the page itself", async () => {
    const { user, client } = await setup();
    const dialog = await openAdd(user, PAGE);
    await within(dialog).findByRole("region", { name: "Demo video lecture42" }, { timeout: 3000 });
    await user.click(within(dialog).getByRole("button", { name: "Download the page itself instead" }));
    expect(within(dialog).queryByRole("region", { name: "Demo video lecture42" })).not.toBeInTheDocument();
    expect(within(dialog).getByRole("textbox", { name: "File name" })).toHaveValue("watch.html");
    await user.click(screen.getByRole("button", { name: "Download" }));
    await waitFor(() => expect(client.log.some((c) => c.method === "download.add")).toBe(true));
    expect(client.log.some((c) => c.method === "media.add")).toBe(false);
  });

  it("does not ask about video for a plain file link", async () => {
    const { user, client } = await setup();
    const dialog = await openAdd(user, "https://example.com/files/thesis.pdf");
    await within(dialog).findByRole("textbox", { name: "File name" }, { timeout: 2000 });
    expect(client.log.some((c) => c.method === "media.probe")).toBe(false);
  });

  it("shows the merge step while a media download is finishing", async () => {
    const { user, mock, grid } = await setup({ scenario: "empty" });
    const dialog = await openAdd(user, PAGE);
    await within(dialog).findByRole("region", { name: "Demo video lecture42" }, { timeout: 3000 });
    await user.click(screen.getByRole("button", { name: "Download" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    const row = () => within(grid).getAllByRole("row").find((r) => r.textContent?.includes("Demo video lecture42"));
    for (let i = 0; i < 400 && !row()?.textContent?.includes("Merging"); i++) await act(() => mock.advance(500));
    expect(row()?.textContent).toContain("Merging");
  });
});

describe("media tools in Settings", { timeout: 30_000 }, () => {
  it("shows versions, installs ffmpeg, checks for updates and turns on daily updates", async () => {
    const { user, mock, client } = await setup();
    await user.click(screen.getByRole("button", { name: "Settings" }));
    const section = await screen.findByRole("region", { name: "Video and audio sites" });
    const yt = within(section).getByRole("listitem", { name: "yt-dlp" });
    expect(yt).toHaveTextContent("Ready");
    expect(yt).toHaveTextContent("Version 2026.10.26");
    expect(yt).toHaveTextContent("github.com/yt-dlp/yt-dlp · Unlicense");
    const ff = within(section).getByRole("listitem", { name: "ffmpeg" });
    expect(ff).toHaveTextContent("Downloaded the first time it is needed");
    expect(ff).toHaveTextContent("LGPL-2.1-or-later");

    await user.click(within(ff).getByRole("button", { name: "Download ffmpeg" }));
    await waitFor(() => expect(within(section).getByRole("listitem", { name: "ffmpeg" })).toHaveTextContent("Downloading"));
    await act(() => mock.advance(2000));
    await waitFor(() => expect(within(section).getByRole("listitem", { name: "ffmpeg" })).toHaveTextContent("Ready"));

    await user.click(within(section).getByRole("button", { name: "Check for updates" }));
    const updated = await within(section).findByText("Version 2026.11.10 is available");
    expect(updated).toBeInTheDocument();
    await user.click(within(section).getByRole("button", { name: "Update yt-dlp" }));
    expect(client.log.some((c) => c.method === "media.updateTools" && JSON.stringify(c.params) === JSON.stringify({ tools: ["yt-dlp"] }))).toBe(true);

    const auto = within(section).getByRole("checkbox", { name: "Check for updates once a day" });
    expect(auto).not.toBeChecked();
    await user.click(auto);
    await waitFor(() => expect(auto).toBeChecked());
    expect(client.log.some((c) => c.method === "media.configure")).toBe(true);
  });

  it("is in Bangla too", async () => {
    const { user } = await setup({ language: "bn" });
    await user.click(screen.getByRole("button", { name: "সেটিংস" }));
    const section = await screen.findByRole("region", { name: "ভিডিও ও অডিও সাইট" });
    expect(within(section).getByRole("button", { name: "আপডেট আছে কি না দেখুন" })).toBeInTheDocument();
  });
});

describe("a daemon without media support (store build)", { timeout: 30_000 }, () => {
  it("shows no media settings and never probes for video", async () => {
    const { user, client, app } = await setup({ media: false });
    expect(app.media.supported).toBe(false);
    await user.click(screen.getByRole("button", { name: "Settings" }));
    await screen.findByRole("region", { name: "Speed" });
    expect(screen.queryByRole("region", { name: "Video and audio sites" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Add download" }));
    const dialog = await screen.findByRole("dialog", { name: "Add a download" });
    await user.type(within(dialog).getByRole("textbox", { name: "Address" }), PAGE);
    await within(dialog).findByRole("textbox", { name: "File name" }, { timeout: 2000 });
    expect(client.log.some((c) => c.method === "media.probe")).toBe(false);
  });
});

describe("picker helpers", () => {
  const formats = [
    { id: "140", kind: "audioOnly", ext: "m4a", sizeBytes: 3, direct: true },
    { id: "18", kind: "videoAudio", ext: "mp4", height: 360, sizeBytes: 10, direct: true },
    { id: "hls", kind: "videoAudio", ext: "mp4", height: 480, direct: false },
    { id: "137", kind: "videoOnly", ext: "mp4", height: 1080, sizeBytes: 50, sizeApproximate: true, direct: true },
  ] as const;
  const list = formats.map((f) => ({ ...f }));

  it("offers heights, highest first", () => {
    expect(videoHeights(list)).toEqual([1080, 480, 360]);
  });

  it("estimates what yt-dlp will pick", () => {
    expect(pickFormats(list, { kind: "best" }).map((f) => f.id)).toEqual(["137", "140"]);
    expect(estimate(list, { kind: "best" })).toEqual({ bytes: 53, approximate: true, direct: true });
    expect(estimate(list, { kind: "best", maxHeight: 480 })).toEqual({ approximate: true, direct: false });
    expect(estimate(list, { kind: "best", maxHeight: 360 })).toEqual({ bytes: 10, approximate: false, direct: true });
    expect(estimate(list, choiceFor("audio", "best", "original"))).toEqual({ bytes: 3, approximate: false, direct: true });
  });

  it("builds choices and recognises pages", () => {
    expect(choiceFor("video", "best", "original")).toEqual({ kind: "best" });
    expect(choiceFor("video", "720", "mp3")).toEqual({ kind: "best", maxHeight: 720 });
    expect(choiceFor("audio", "720", "mp3")).toEqual({ kind: "audio", convert: "mp3" });
    expect(isWebPage("text/html; charset=utf-8")).toBe(true);
    expect(isWebPage("video/mp4")).toBe(false);
    expect(isWebPage(undefined)).toBe(false);
  });
});
