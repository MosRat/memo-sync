import { describe, expect, it } from "vitest";
import { memoSearchText, tokenizeTags } from "./search";

describe("tag parsing", () => {
  it("drops empty segments and preserves user order", () => {
    expect(tokenizeTags("rust, sync, , markdown")).toEqual(["rust", "sync", "markdown"]);
  });
});

describe("memo search text", () => {
  it("includes metadata fields", () => {
    expect(
      memoSearchText({
        id: "1",
        repository_id: "repo",
        title: "Snippet",
        body_md: "body",
        tags: ["code"],
        pinned: false,
        archived: false,
        deleted: false,
        created_at: new Date(0).toISOString(),
        updated_at: new Date(0).toISOString(),
        source: "QuickCapture",
        meta: {
          language: "rust",
          url: "https://example.test/spec",
          device_name: "laptop",
          byte_len: 4,
        },
      }),
    ).toContain("rust");
  });
});
