import { describe, expect, it } from "vitest";

import {
  buildAddParams,
  buildRequestContext,
  cookieHeader,
  cookiesFor,
  privateCookieStore,
} from "../src/background/capture/context";

describe("cookieHeader", () => {
  it("joins name=value pairs, longest path first, keeping store order on ties", () => {
    const header = cookieHeader([
      { name: "a", value: "1", path: "/" },
      { name: "b", value: "2", path: "/files/" },
      { name: "c", value: "3", path: "/" },
      { name: "d", value: "x=y", path: "/files/deep/" },
    ]);
    expect(header).toBe("d=x=y; b=2; a=1; c=3");
  });

  it("sends a nameless cookie as its bare value, like browsers do", () => {
    expect(cookieHeader([{ name: "", value: "token", path: "/" }])).toBe("token");
  });

  it("is undefined without cookies", () => {
    expect(cookieHeader([])).toBeUndefined();
  });
});

describe("buildRequestContext", () => {
  it("carries cookies, referer and user agent", () => {
    expect(
      buildRequestContext({
        cookies: [{ name: "session", value: "abc", path: "/" }],
        referrer: "https://example.com/page#section",
        userAgent: "Mozilla/5.0 Test",
      }),
    ).toEqual({ cookie: "session=abc", referer: "https://example.com/page", userAgent: "Mozilla/5.0 Test" });
  });

  it("strips credentials from the referer and ignores non-http pages", () => {
    expect(buildRequestContext({ cookies: [], referrer: "https://user:pw@example.com/a" })).toEqual({
      referer: "https://example.com/a",
    });
    expect(buildRequestContext({ cookies: [], referrer: "about:blank" })).toBeUndefined();
    expect(buildRequestContext({ cookies: [], referrer: "file:///home/u/x.html" })).toBeUndefined();
  });

  it("omits empty fields instead of sending null or empty strings", () => {
    const context = buildRequestContext({ cookies: [], referrer: "", userAgent: "" });
    expect(context).toBeUndefined();
  });
});

describe("buildAddParams", () => {
  it("uses the browser's name, reduced to a base name", () => {
    expect(
      buildAddParams({
        url: "https://cdn.example.com/d/123?sig=x",
        filename: "/home/u/Downloads/My File (1).zip",
        cookies: [],
        userAgent: "UA",
      }),
    ).toEqual({ url: "https://cdn.example.com/d/123?sig=x", filename: "My File (1).zip", request: { userAgent: "UA" } });
  });

  it("falls back to the URL for the name, and leaves it out when there is none", () => {
    expect(buildAddParams({ url: "https://example.com/a%20b.iso?x=1", cookies: [] })).toEqual({
      url: "https://example.com/a%20b.iso?x=1",
      filename: "a b.iso",
    });
    expect(buildAddParams({ url: "https://example.com/", cookies: [] })).toEqual({ url: "https://example.com/" });
  });
});

describe("cookiesFor", () => {
  it("asks the right store and survives a refusal", async () => {
    const seen: unknown[] = [];
    const ok = { getAll: async (d: unknown) => (seen.push(d), [{ name: "a", value: "1", path: "/" }]) };
    await expect(cookiesFor(ok, "https://x/", undefined)).resolves.toHaveLength(1);
    await cookiesFor(ok, "https://x/", privateCookieStore(true));
    expect(seen).toEqual([{ url: "https://x/" }, { url: "https://x/", storeId: "firefox-private" }]);
    expect(privateCookieStore(false)).toBe("1");

    const refusing = {
      getAll: async () => {
        throw new Error("firstPartyDomain is required when first-party isolation is enabled");
      },
    };
    await expect(cookiesFor(refusing, "https://x/", undefined)).resolves.toEqual([]);
  });
});
