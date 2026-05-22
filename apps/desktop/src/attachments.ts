import { attachmentUrl } from "./tauri";

export const ATTACHMENT_MARKDOWN_PREFIX = "memo-attachment:";
export const MAX_IMAGE_ATTACHMENT_BYTES = 16 * 1024 * 1024;
export const SUPPORTED_IMAGE_TYPES = ["image/png", "image/jpeg", "image/webp", "image/gif"] as const;

export function isSupportedImageType(mediaType: string) {
  return SUPPORTED_IMAGE_TYPES.includes(mediaType as (typeof SUPPORTED_IMAGE_TYPES)[number]);
}

export function attachmentMarkdown(fileName: string, id: string) {
  return `![${escapeMarkdownAlt(fileName)}](${ATTACHMENT_MARKDOWN_PREFIX}${id})`;
}

export function resolveMemoImageUrl(url: string) {
  if (url.startsWith(ATTACHMENT_MARKDOWN_PREFIX)) {
    return attachmentUrl(url.slice(ATTACHMENT_MARKDOWN_PREFIX.length).trim());
  }
  return url;
}

export function removeAttachmentMarkdown(body: string, attachmentId: string) {
  const escapedId = attachmentId.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const imageRef = new RegExp(`!\\[[^\\]\\n]*\\]\\(\\s*${ATTACHMENT_MARKDOWN_PREFIX}${escapedId}(?:\\s+["'][^"']*["'])?\\s*\\)`, "g");
  return body
    .split(/\r\n|\r|\n/)
    .map((line) => {
      const cleaned = line.replace(imageRef, "").trimEnd();
      return cleaned.trim().length === 0 && line.includes(`${ATTACHMENT_MARKDOWN_PREFIX}${attachmentId}`) ? null : cleaned;
    })
    .filter((line): line is string => line !== null)
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trimEnd();
}

export function attachmentRefsFromMarkdown(body: string) {
  const refs: string[] = [];
  const imageRef = /!\[[^\]\n]*\]\(\s*memo-attachment:([0-9a-fA-F-]{36})(?:\s+["'][^"']*["'])?\s*\)/g;
  for (const match of body.matchAll(imageRef)) {
    refs.push(match[1]);
  }
  return refs;
}

export function bodyHasOnlyAttachmentImages(body: string) {
  if (!body.trim()) return false;
  const withoutImageRefs = body.replace(/!\[[^\]\n]*\]\(\s*memo-attachment:[0-9a-fA-F-]{36}(?:\s+["'][^"']*["'])?\s*\)/g, "");
  return withoutImageRefs.trim().length === 0 && attachmentRefsFromMarkdown(body).length > 0;
}

export function imageFilesFromTransfer(transfer: DataTransfer) {
  const files = Array.from(transfer.files).filter((file) => isSupportedImageType(file.type));
  if (files.length) return uniqueFiles(files);

  const itemFiles = Array.from(transfer.items ?? [])
    .filter((item) => item.kind === "file")
    .map((item) => item.getAsFile())
    .filter((file): file is File => file !== null && isSupportedImageType(file.type));
  return uniqueFiles(itemFiles);
}

export function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error("Could not read file"));
    reader.onload = () => {
      const result = String(reader.result ?? "");
      resolve(result.includes(",") ? result.split(",").pop() ?? "" : result);
    };
    reader.readAsDataURL(file);
  });
}

function uniqueFiles(files: File[]) {
  const seen = new Set<string>();
  return files.filter((file) => {
    const key = `${file.name}\0${file.type}\0${file.size}\0${file.lastModified}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function escapeMarkdownAlt(value: string) {
  return value.replace(/[\[\]\\]/g, "\\$&");
}
