import type { Memo } from "./types";

export function tokenizeTags(input: string) {
  return input
    .split(",")
    .map((tag) => tag.trim())
    .filter(Boolean);
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
