import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { AppSettings, Bootstrap, LocalStats, Memo, MemoAttachment, MemoFilter, RenderFormat, RenderMemoAssetOutput, RenderMemoOutput, RenderTemplate, Repository, SaveAttachmentInput, SaveMemoInput, ShortcutCheckResult } from "./types";
import { DEFAULT_APP_SETTINGS, withDefaultSettings } from "./defaults";

const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const userAgent = typeof navigator === "undefined" ? "" : navigator.userAgent;
export const isNativeApp = isTauri;
export const isAndroidApp = isTauri && /\bAndroid\b/i.test(userAgent);
export const isMobileApp = isTauri && /\b(Android|iPhone|iPad|iPod)\b/i.test(userAgent);
export const isDesktopApp = isTauri && !isMobileApp;

export const APP_EVENTS = {
  openQuickCapture: "open-quick-capture",
  clipboardCaptureRequested: "clipboard-capture-requested",
  memosChanged: "memos-changed",
  syncCompleted: "sync-completed",
} as const;

export type MemosChangedPayload = { active_memo_id?: string | null };
export type SyncCompletedPayload = {
  ok: boolean;
  pushed: number;
  pulled: number;
  server_sequence: number;
  message: string;
  background: boolean;
};

export function currentWindowLabel() {
  if (!isTauri) return "web";
  try {
    return getCurrentWindow().label;
  } catch {
    return "main";
  }
}

const WEB_REPOSITORIES_KEY = "memo-sync-web-repositories";
const WEB_MEMOS_KEY = "memo-sync-web-memos";
const WEB_ATTACHMENTS_KEY = "memo-sync-web-attachments";
const WEB_SETTINGS_KEY = "memo-sync-settings";

const defaultSettings = DEFAULT_APP_SETTINGS;

const demoRepo: Repository = {
  id: "demo-repo",
  name: "Inbox",
  kind: "Persistent",
  sync_enabled: true,
  color: "#c86f52",
  created_at: new Date().toISOString(),
  updated_at: new Date().toISOString(),
};

const defaultDemoMemos: Memo[] = [
  {
    id: "demo-memo",
    repository_id: demoRepo.id,
    title: "晨间札记 / Morning Note",
    body_md:
      "把散落在剪贴板、会议和代码里的句子，收进一个可以同步的地方。\n\nUse **Markdown**, tags, repositories, quick capture, and background sync.\n\n```ts\nconst note = {\n  mood: 'quiet craft',\n  sync: 'local first',\n};\n```",
    tags: ["welcome", "markdown", "中文"],
    pinned: true,
    archived: false,
    deleted: false,
    created_at: new Date().toISOString(),
    updated_at: new Date().toISOString(),
    source: "Manual",
    meta: { byte_len: 120 },
  },
];

let webRepositories = readWebValue<Repository[]>(WEB_REPOSITORIES_KEY, [demoRepo]);
let demoMemos = readWebValue<Memo[]>(WEB_MEMOS_KEY, defaultDemoMemos);
let demoAttachments = readWebValue<Array<MemoAttachment & { data_base64?: string }>>(WEB_ATTACHMENTS_KEY, []);

function demoStats(): LocalStats {
  return {
    memo_count: demoMemos.filter((memo) => !memo.deleted).length,
    repository_count: webRepositories.length,
    attachment_count: demoAttachments.filter((attachment) => !attachment.deleted).length,
    attachment_blob_count: new Set(
      demoAttachments
        .filter((attachment) => !attachment.deleted)
        .map((attachment) => attachment.content_sha256 || attachment.id),
    ).size,
    attachment_blob_bytes: demoAttachments
      .filter((attachment) => !attachment.deleted)
      .reduce((total, attachment) => total + attachment.byte_len, 0),
    missing_attachment_blobs: 0,
    attachment_metadata_mismatches: 0,
    pending_operations: 0,
    last_server_sequence: 0,
  };
}

export async function bootstrap(): Promise<Bootstrap> {
  if (isTauri) return invoke("bootstrap");
  return {
    repositories: webRepositories,
    memos: demoMemos,
    attachments: demoAttachments.filter((attachment) => !attachment.deleted),
    device_id: "web-preview",
    settings: getWebSettings(),
    local_stats: demoStats(),
  };
}

export async function getAppSettings(): Promise<AppSettings> {
  if (isTauri) return invoke("get_app_settings");
  return getWebSettings();
}

export async function updateAppSettings(settings: AppSettings): Promise<AppSettings> {
  if (isTauri) return invoke("update_app_settings", { settings });
  const next = withDefaultSettings(settings);
  writeWebValue(WEB_SETTINGS_KEY, next);
  return next;
}

export async function checkShortcuts(
  quickCaptureShortcut: string,
  clipboardCaptureShortcut: string,
  settingsShortcut: string,
): Promise<ShortcutCheckResult> {
  if (isTauri) {
    return invoke("check_shortcuts", {
      request: {
        quick_capture_shortcut: quickCaptureShortcut,
        clipboard_capture_shortcut: clipboardCaptureShortcut,
        settings_shortcut: settingsShortcut,
      },
    });
  }
  if (new Set([quickCaptureShortcut, clipboardCaptureShortcut, settingsShortcut]).size !== 3) {
    return {
      ok: false,
      quick_available: false,
      clipboard_available: false,
      settings_available: false,
      message: "Shortcuts must be different",
    };
  }
  return {
    ok: true,
    quick_available: true,
    clipboard_available: true,
    settings_available: true,
    message: "Looks available in web preview",
  };
}

export async function createRepository(name: string, temporary: boolean, color: string): Promise<Repository> {
  if (isTauri) return invoke("create_repository", { name, temporary, color });
  const now = new Date().toISOString();
  const repo: Repository = {
    id: crypto.randomUUID(),
    name: name.trim() || "Untitled repository",
    kind: temporary ? "Temporary" : "Persistent",
    sync_enabled: !temporary,
    color: cleanWebColor(color),
    created_at: now,
    updated_at: now,
  };
  webRepositories = [repo, ...webRepositories];
  writeWebValue(WEB_REPOSITORIES_KEY, webRepositories);
  return repo;
}

export async function updateRepository(
  id: string,
  name: string,
  color: string,
  syncEnabled: boolean,
): Promise<Repository> {
  if (isTauri) return invoke("update_repository", { id, name, color, syncEnabled });
  const now = new Date().toISOString();
  const existing = webRepositories.find((repo) => repo.id === id);
  const repo = {
    id,
    name: name.trim() || "Untitled repository",
    kind: existing?.kind ?? "Persistent",
    sync_enabled: existing?.kind === "Temporary" ? false : syncEnabled,
    color: cleanWebColor(color),
    created_at: existing?.created_at ?? now,
    updated_at: now,
  };
  webRepositories = webRepositories.map((item) => (item.id === id ? repo : item));
  writeWebValue(WEB_REPOSITORIES_KEY, webRepositories);
  return repo;
}

export async function saveMemo(input: SaveMemoInput): Promise<Memo> {
  if (isTauri) return invoke("save_memo", { input });
  return saveMemoFallback(input, "Manual");
}

export async function saveQuickMemo(input: SaveMemoInput): Promise<Memo> {
  if (isTauri) return invoke("save_quick_memo", { input });
  return saveMemoFallback(input, "QuickCapture");
}

function saveMemoFallback(input: SaveMemoInput, source: Memo["source"]): Memo {
  const existing = input.id ? demoMemos.find((memo) => memo.id === input.id) : null;
  const now = new Date().toISOString();
  const memo: Memo = {
    id: input.id ?? crypto.randomUUID(),
    repository_id: input.repository_id,
    title: input.title.trim() || titleFromBody(input.body_md),
    body_md: input.body_md,
    tags: input.tags,
    pinned: input.pinned,
    archived: input.archived,
    deleted: false,
    created_at: existing?.created_at ?? now,
    updated_at: now,
    source,
    meta: { byte_len: input.body_md.length },
  };
  demoMemos = [memo, ...demoMemos.filter((item) => item.id !== memo.id)];
  writeWebValue(WEB_MEMOS_KEY, demoMemos);
  return memo;
}

export async function deleteMemo(id: string): Promise<void> {
  if (isTauri) return invoke("delete_memo", { id });
  demoMemos = demoMemos.filter((item) => item.id !== id);
  writeWebValue(WEB_MEMOS_KEY, demoMemos);
}

export async function saveMemoAttachment(input: SaveAttachmentInput): Promise<MemoAttachment> {
  if (isTauri) return invoke("save_memo_attachment", { input });
  const memo = demoMemos.find((item) => item.id === input.memo_id);
  if (!memo) throw new Error("Memo not found");
  const now = new Date().toISOString();
  const byteLen = decodedBase64Length(input.data_base64);
  const attachment: MemoAttachment & { data_base64: string } = {
    id: crypto.randomUUID(),
    memo_id: input.memo_id,
    repository_id: memo.repository_id,
    file_name: input.file_name.trim() || "attachment",
    media_type: input.media_type,
    byte_len: byteLen,
    content_sha256: await sha256Base64Payload(input.data_base64),
    deleted: false,
    created_at: now,
    updated_at: now,
    data_base64: input.data_base64,
  };
  demoAttachments = [attachment, ...demoAttachments];
  writeWebValue(WEB_ATTACHMENTS_KEY, demoAttachments);
  return attachment;
}

function decodedBase64Length(dataBase64: string) {
  const padding = dataBase64.endsWith("==") ? 2 : dataBase64.endsWith("=") ? 1 : 0;
  return Math.max(0, Math.floor((dataBase64.length * 3) / 4) - padding);
}

async function sha256Base64Payload(dataBase64: string) {
  const binary = atob(dataBase64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

export async function deleteMemoAttachment(id: string): Promise<void> {
  if (isTauri) return invoke("delete_memo_attachment", { id });
  demoAttachments = demoAttachments.map((attachment) => (attachment.id === id ? { ...attachment, deleted: true } : attachment));
  writeWebValue(WEB_ATTACHMENTS_KEY, demoAttachments);
}

export function attachmentUrl(id: string): string {
  if (isTauri) return previewProtocolUrl(`/attachment/${id}`);
  const attachment = demoAttachments.find((item) => item.id === id);
  return attachment?.data_base64 ? `data:${attachment.media_type};base64,${attachment.data_base64}` : "";
}

export async function captureClipboardMemo(repositoryId: string): Promise<Memo> {
  if (isTauri) return invoke("capture_clipboard_memo", { repositoryId });
  const text = await navigator.clipboard.readText();
  return saveMemo({
    repository_id: repositoryId,
    title: "Clipboard capture",
    body_md: text,
    tags: ["clipboard"],
    pinned: false,
    archived: false,
  });
}

export async function readClipboardText(): Promise<string> {
  if (isTauri) return invoke("read_clipboard_text");
  return navigator.clipboard.readText();
}

export async function searchMemos(filter: MemoFilter): Promise<Memo[]> {
  if (isTauri) return invoke("search_memos", { filter });
  return demoMemos.filter((memo) => {
    if (memo.deleted) return false;
    if (filter.repository_id && memo.repository_id !== filter.repository_id) return false;
    if (filter.pinned !== undefined && filter.pinned !== null && memo.pinned !== filter.pinned) return false;
    if (filter.archived !== undefined && filter.archived !== null && memo.archived !== filter.archived) return false;
    if (filter.source && memo.source !== filter.source) return false;
    if (!filter.tags.every((tag) => memo.tags.includes(tag))) return false;
    const query = filter.query?.trim().toLowerCase();
    if (!query) return true;
    return `${memo.title} ${memo.body_md} ${memo.tags.join(" ")} ${memo.source} ${JSON.stringify(memo.meta)}`
      .toLowerCase()
      .includes(query);
  });
}

export async function syncNow(serverUrl: string): Promise<{ pushed: number; pulled: number; server_sequence: number }> {
  if (isTauri) return invoke("sync_now", { serverUrl });
  return { pushed: 0, pulled: 0, server_sequence: 0 };
}

export async function renderMemoPreview(body: string, format: RenderFormat, template: RenderTemplate): Promise<RenderMemoOutput> {
  if (isTauri) return invoke("render_memo_preview", { input: { body, format, template } });
  throw new Error("Typst renderer is available in the desktop app");
}

export async function renderMemoPreviewAsset(body: string, format: RenderFormat, template: RenderTemplate): Promise<RenderMemoAssetOutput> {
  if (isTauri) return invoke("render_memo_preview_asset", { input: { body, format, template } });
  throw new Error("Typst renderer is available in the desktop app");
}

export async function windowAction(action: "window_minimize" | "window_toggle_maximize" | "window_close") {
  if (isTauri) return invoke(action);
}

export function listenCurrentWindowFocus(handler: (focused: boolean) => void) {
  if (!isTauri) return Promise.resolve(() => {});
  return getCurrentWindow().onFocusChanged((event) => handler(event.payload));
}

export async function showQuickCaptureWindow() {
  if (isTauri) return invoke("show_quick_capture");
}

export async function showSettingsWindow() {
  if (isTauri) return invoke("show_settings_window");
}

export function emitAppEvent<T>(name: string, payload: T) {
  if (!isTauri) return Promise.resolve();
  return emit(name, payload);
}

export function listenAppEvent<T = unknown>(name: string, handler: (payload: T) => void) {
  if (!isTauri) return Promise.resolve(() => {});
  return listen<T>(name, (event) => handler(event.payload));
}

export function emitMemosChanged(payload: MemosChangedPayload) {
  return emitAppEvent(APP_EVENTS.memosChanged, payload);
}

export function listenMemosChanged(handler: (payload: MemosChangedPayload) => void) {
  return listenAppEvent<MemosChangedPayload>(APP_EVENTS.memosChanged, handler);
}

export function listenSyncCompleted(handler: (payload: SyncCompletedPayload) => void) {
  return listenAppEvent<SyncCompletedPayload>(APP_EVENTS.syncCompleted, handler);
}

function getWebSettings(): AppSettings {
  const stored = getWebStorage()?.getItem(WEB_SETTINGS_KEY);
  if (!stored) return defaultSettings;
  try {
    return withDefaultSettings(JSON.parse(stored));
  } catch {
    return defaultSettings;
  }
}

function getWebStorage(): Storage | null {
  if (typeof localStorage === "undefined") return null;
  return localStorage;
}

function readWebValue<T>(key: string, fallback: T): T {
  const stored = getWebStorage()?.getItem(key);
  if (!stored) return fallback;
  try {
    return JSON.parse(stored) as T;
  } catch {
    return fallback;
  }
}

function writeWebValue<T>(key: string, value: T) {
  getWebStorage()?.setItem(key, JSON.stringify(value));
}

function previewProtocolUrl(path: string): string {
  const isWindowsLike = navigator.userAgent.includes("Windows") || navigator.userAgent.includes("Android");
  return isWindowsLike ? `http://memo-preview.localhost${path}` : `memo-preview://localhost${path}`;
}

function cleanWebColor(color: string) {
  return /^#[0-9a-f]{6}$/i.test(color) ? color : "#c86f52";
}

function titleFromBody(body: string) {
  return body
    .split(/\r\n|\r|\n/)
    .find((line) => line.trim())
    ?.trim()
    .replace(/^#+\s*/, "")
    .slice(0, 64) || "Untitled memo";
}
