import type { Memo } from "./types";

export function tokenizeTags(input: string) {
  const seen = new Set<string>();
  return input
    .split(",")
    .map(normalizeTag)
    .filter((tag) => {
      if (!tag || seen.has(tag.toLowerCase())) return false;
      seen.add(tag.toLowerCase());
      return true;
    });
}

export function normalizeTag(input: string) {
  return input
    .trim()
    .replace(/^#+/, "")
    .replace(/\s+/g, "-")
    .replace(/[,\u0000-\u001f]/g, "")
    .slice(0, 48);
}

export function memoSearchText(memo: Memo) {
  return [
    memo.title,
    memo.body_md,
    memo.tags.join(" "),
    memo.source,
    memo.meta.language ?? "",
    memo.meta.url ?? "",
    memo.meta.device_name ?? "",
  ]
    .join(" ")
    .toLowerCase();
}

export function textStats(input: string) {
  const lines = input ? input.split(/\r\n|\r|\n/).length : 0;
  const latinWords = input.match(/[A-Za-z0-9_]+(?:[-'][A-Za-z0-9_]+)*/g)?.length ?? 0;
  const cjkChars = input.match(/[\u3400-\u9fff]/g)?.length ?? 0;
  return {
    lines,
    words: latinWords + cjkChars,
    chars: input.length,
  };
}

export function textStatsLabel(input: string) {
  const stats = textStats(input);
  return `${stats.lines} lines / ${stats.words} words / ${stats.chars} chars`;
}

export function readingTimeLabel(input: string) {
  const stats = textStats(input);
  const minutes = Math.max(1, Math.ceil(stats.words / 320));
  return `${minutes} min read`;
}

export function memoPreviewText(markdown: string, limit = 110) {
  return markdown
    .replace(/```[\s\S]*?```/g, " code ")
    .replace(/[#*_`>\-[\]()]/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, limit);
}

export function memoHeadings(markdown: string) {
  let inFence = false;
  const headings: Array<{ line: number; level: number; title: string }> = [];
  markdown.split(/\r\n|\r|\n/).forEach((line, index) => {
    if (/^\s*```/.test(line)) {
      inFence = !inFence;
      return;
    }
    if (!inFence) {
      const match = /^(#{1,3})\s+(.+)$/.exec(line.trim());
      if (!match) return;
      headings.push({
        line: index,
        level: match[1].length,
        title: match[2].replace(/[*_`]/g, "").trim().slice(0, 72),
      });
    }
  });
  return headings;
}
