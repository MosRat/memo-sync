import { describe, expect, it } from "vitest";
import { attachmentMarkdown, attachmentRefsFromMarkdown, bodyHasOnlyAttachmentImages, removeAttachmentMarkdown } from "./attachments";
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

describe("attachment markdown helpers", () => {
  it("escapes generated image alt text", () => {
    expect(attachmentMarkdown("a[1]\\b.png", "att-1")).toBe("![a\\[1\\]\\\\b.png](memo-attachment:att-1)");
  });

  it("removes whole-line attachment references without collapsing paragraphs", () => {
    expect(removeAttachmentMarkdown("before\n\n![img](memo-attachment:att-1)\n\nafter", "att-1")).toBe("before\n\nafter");
  });

  it("removes inline attachment references and leaves surrounding text", () => {
    expect(removeAttachmentMarkdown("left ![img](memo-attachment:att-1) right\n![keep](memo-attachment:att-2)", "att-1")).toBe(
      "left  right\n![keep](memo-attachment:att-2)",
    );
  });

  it("detects pure image memos for render bypass", () => {
    const id = "018f2b4f-1111-7222-8333-444444444444";
    expect(bodyHasOnlyAttachmentImages(`![one](memo-attachment:${id})\n\n`)).toBe(true);
    expect(attachmentRefsFromMarkdown(`![one](memo-attachment:${id})`)).toEqual([id]);
    expect(bodyHasOnlyAttachmentImages(`caption\n![one](memo-attachment:${id})`)).toBe(false);
  });
});
