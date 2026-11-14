import { describe, expect, it } from "vitest";

import { asContentMessage, asPopupMessage } from "../src/shared/messages";

describe("message validation", () => {
  it("accepts the content script's messages and trims link text", () => {
    expect(asContentMessage({ type: "wdm/force-next" })).toEqual({ type: "wdm/force-next" });
    const long = "x".repeat(500);
    expect(asContentMessage({ type: "wdm/link-click", url: "https://a/b.zip", text: long })).toEqual({
      type: "wdm/link-click",
      url: "https://a/b.zip",
      text: "x".repeat(200),
    });
    expect(asContentMessage({ type: "wdm/link-click", url: 1, text: "t" })).toBeUndefined();
  });

  it("accepts only known answers to self-healing questions", () => {
    expect(asPopupMessage({ type: "wdm/heal-answer", id: "dl_1", answer: "open-page" })).toEqual({
      type: "wdm/heal-answer",
      id: "dl_1",
      answer: "open-page",
    });
    expect(asPopupMessage({ type: "wdm/heal-answer", id: "dl_1", answer: "delete-everything" })).toBeUndefined();
    expect(asPopupMessage({ type: "wdm/heal-answer", answer: "dismiss" })).toBeUndefined();
  });
});
