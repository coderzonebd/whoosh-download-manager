import { describe, expect, it } from "vitest";

import { basename, chooseFilename, cleanFilename, extensionOf, filenameFromUrl } from "../src/shared/filename";
import { formatBytes } from "../src/shared/format";
import { frameUrl, hostMatches, isSiteExcluded, normalizeSitePattern, parseHttpUrl, refererFrom, stripFragment } from "../src/shared/url";

describe("file names", () => {
  it("takes the base name of POSIX and Windows paths", () => {
    expect(basename("/home/u/Downloads/a.zip")).toBe("a.zip");
    expect(basename("C:\\Users\\u\\Downloads\\b.exe")).toBe("b.exe");
    expect(basename("plain.iso")).toBe("plain.iso");
    expect(basename("dir/")).toBe("");
  });

  it("finds extensions, ignoring dotfiles and trailing dots", () => {
    expect(extensionOf("Movie.MKV")).toBe("mkv");
    expect(extensionOf("archive.tar.gz")).toBe("gz");
    expect(extensionOf(".bashrc")).toBe("");
    expect(extensionOf("README")).toBe("");
    expect(extensionOf("weird.")).toBe("");
    expect(extensionOf("/a.b/c")).toBe("");
  });

  it("derives a name from the URL path, decoded, without query or fragment", () => {
    expect(filenameFromUrl("https://x.org/pub/a%20b%2B.iso?token=1#t")).toBe("a b+.iso");
    expect(filenameFromUrl("https://x.org/pub/dir/")).toBe("dir");
    expect(filenameFromUrl("https://x.org/")).toBe("");
    expect(filenameFromUrl("https://x.org/bad%E0%A4%A.zip")).toBe("bad%E0%A4%A.zip");
    // An encoded slash must not smuggle in a directory.
    expect(filenameFromUrl("https://x.org/a%2F..%2Fetc%2Fpasswd")).toBe("passwd");
    expect(filenameFromUrl("blob:https://x.org/uuid")).toBe("");
  });

  it("cleans names: no directories, control characters or dot names", () => {
    expect(cleanFilename("  report\u0000\u001f.pdf ")).toBe("report.pdf");
    expect(cleanFilename("../../etc/passwd")).toBe("passwd");
    expect(cleanFilename("..")).toBe("");
    expect(cleanFilename(".")).toBe("");
  });

  it("chooses the browser's name, then the URL's, then none", () => {
    expect(chooseFilename("C:\\dl\\Setup 2.exe", "https://x/download?id=1")).toBe("Setup 2.exe");
    expect(chooseFilename("", "https://x/files/linux.iso")).toBe("linux.iso");
    expect(chooseFilename(undefined, "https://x/")).toBeUndefined();
  });

  it("formats sizes", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1536)).toBe("1.5 KiB");
    expect(formatBytes(50 * 1024 * 1024)).toBe("50 MiB");
    expect(formatBytes(-1)).toBe("");
  });
});

describe("URLs and sites", () => {
  it("accepts only http(s)", () => {
    expect(parseHttpUrl("https://example.com/a")?.hostname).toBe("example.com");
    expect(parseHttpUrl("ftp://example.com/a")).toBeUndefined();
    expect(parseHttpUrl("javascript:alert(1)")).toBeUndefined();
    expect(parseHttpUrl("not a url")).toBeUndefined();
  });

  it("builds a referer the way browsers do", () => {
    expect(refererFrom("https://u:p@example.com/a?b=1#c")).toBe("https://example.com/a?b=1");
    expect(refererFrom("chrome://newtab/")).toBeUndefined();
  });

  it("strips fragments", () => {
    expect(stripFragment("https://x/a#b#c")).toBe("https://x/a");
    expect(stripFragment("https://x/a")).toBe("https://x/a");
  });

  it("normalises exclusion patterns", () => {
    expect(normalizeSitePattern(" *.Example.COM ")).toBe("example.com");
    expect(normalizeSitePattern(".example.com")).toBe("example.com");
    expect(normalizeSitePattern("https://www.example.com:8443/path")).toBe("www.example.com");
    expect(normalizeSitePattern("example.com/path")).toBe("example.com");
  });

  it("matches a host and its subdomains only", () => {
    expect(hostMatches("example.com", "example.com")).toBe(true);
    expect(hostMatches("dl.example.com", "*.example.com")).toBe(true);
    expect(hostMatches("badexample.com", "example.com")).toBe(false);
    expect(hostMatches("example.com.", "example.com")).toBe(true);
    expect(hostMatches("example.com", "")).toBe(false);
  });

  it("checks every URL of a download against the list", () => {
    expect(isSiteExcluded(["https://cdn.net/a", "https://site.org/"], ["site.org"])).toBe(true);
    expect(isSiteExcluded(["https://cdn.net/a", undefined], ["site.org"])).toBe(false);
    expect(isSiteExcluded(["https://cdn.net/a"], [])).toBe(false);
  });
});

describe("the frame a content script spoke from", () => {
  it("takes the page's current address on the same site (YouTube: video, then a Short)", () => {
    expect(frameUrl("https://www.youtube.com/watch?v=On7He5aG9jg", "https://www.youtube.com/shorts/bt6AEv1CrfQ")).toBe(
      "https://www.youtube.com/shorts/bt6AEv1CrfQ",
    );
  });

  it("keeps the browser's address when the page names another site", () => {
    expect(frameUrl("https://www.youtube.com/watch?v=a", "https://evil.example/x")).toBe("https://www.youtube.com/watch?v=a");
    expect(frameUrl("https://www.youtube.com/watch?v=a", "javascript:alert(1)")).toBe("https://www.youtube.com/watch?v=a");
  });

  it("falls back to whichever one is a web address", () => {
    expect(frameUrl(undefined, "https://example.com/v")).toBe("https://example.com/v");
    expect(frameUrl(undefined, "file:///etc/passwd")).toBeUndefined();
  });
});
