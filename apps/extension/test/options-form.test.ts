import { describe, expect, it } from "vitest";

import { bytesToSize, formatFileTypes, formatSites, parseFileTypes, parseSites, sizeToBytes } from "../src/options/form";

describe("options form", () => {
  it("parses file types loosely and cleanly", () => {
    // "tar.gz" is not an extension the rules can see ("gz" is).
    expect(parseFileTypes(" ZIP, .iso  *.mp4;;tar.gz\n7z zip ")).toEqual(["zip", "iso", "mp4", "7z"]);
    expect(parseFileTypes("zip, .ISO *.mp4\n7z zip")).toEqual(["zip", "iso", "mp4", "7z"]);
    expect(parseFileTypes("   ")).toEqual([]);
    expect(parseFileTypes("../etc <b>")).toEqual([]);
    expect(formatFileTypes(["zip", "iso"])).toBe("zip iso");
  });

  it("parses sites one per line and reports what is not a site", () => {
    expect(parseSites("Example.com\n*.foo.org\nhttps://bar.net/path\n\n example.com ")).toEqual({
      sites: ["example.com", "foo.org", "bar.net"],
      invalid: [],
    });
    expect(parseSites("good.com\nnot a site\n!!").invalid).toEqual(["not a site", "!!"]);
    expect(formatSites(["a.com", "b.org"])).toBe("a.com\nb.org");
  });

  it("converts the minimum size both ways", () => {
    expect(sizeToBytes("", "MB")).toBe(0);
    expect(sizeToBytes("1,5", "MB")).toBe(1_572_864);
    expect(sizeToBytes("2", "GB")).toBe(2 * 1024 ** 3);
    expect(sizeToBytes("-1", "KB")).toBeUndefined();
    expect(sizeToBytes("lots", "KB")).toBeUndefined();
    expect(bytesToSize(0)).toEqual({ value: "0", unit: "MB" });
    expect(bytesToSize(1_572_864)).toEqual({ value: "1.5", unit: "MB" });
    expect(bytesToSize(2 * 1024 ** 3)).toEqual({ value: "2", unit: "GB" });
    expect(bytesToSize(1000)).toEqual({ value: "0.98", unit: "KB" });
    for (const bytes of [512 * 1024, 5 * 1024 ** 2, 3 * 1024 ** 3]) {
      const { value, unit } = bytesToSize(bytes);
      expect(sizeToBytes(value, unit)).toBe(bytes);
    }
  });
});
