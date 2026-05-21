import { describe, expect, it } from "vitest";
import { memoHeadings, memoPreviewText, normalizeTag, readingTimeLabel, memoSearchText, textStats, textStatsLabel, tokenizeTags } from "./search";

describe("tag parsing", () => {
  it("drops empty segments and preserves user order", () => {
    expect(tokenizeTags("rust, sync, , markdown")).toEqual(["rust", "sync", "markdown"]);
  });

  it("normalizes hashes, spaces, and duplicates", () => {
    expect(tokenizeTags("#Rust, rust, 中文 标签, a,b")).toEqual(["Rust", "中文-标签", "a", "b"]);
    expect(normalizeTag("  #long memo tag  ")).toBe("long-memo-tag");
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

describe("text stats", () => {
  it("counts mixed English and Chinese writing without allocating UI state", () => {
    expect(textStats("Hello memo\n中文")).toEqual({ lines: 2, words: 4, chars: 13 });
    expect(textStatsLabel("")).toBe("0 lines / 0 words / 0 chars");
    expect(readingTimeLabel("短 memo")).toBe("1 min read");
  });
});

describe("memo outline helpers", () => {
  it("extracts headings and cleans card previews", () => {
    expect(memoHeadings("# Title\nbody\n## Child\n```rust\n# no\n```")).toEqual([
      { line: 0, level: 1, title: "Title" },
      { line: 2, level: 2, title: "Child" },
    ]);
    expect(memoPreviewText("# Title\n\n```rust\nfn main() {}\n```\n正文")).toBe("Title code 正文");
  });
});
