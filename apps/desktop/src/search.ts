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
