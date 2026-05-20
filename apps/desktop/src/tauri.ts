import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { AppSettings, Bootstrap, LocalStats, Memo, MemoFilter, RenderFormat, RenderMemoAssetOutput, RenderMemoOutput, RenderTemplate, Repository, SaveMemoInput, ShortcutCheckResult } from "./types";

const isTauri = "__TAURI_INTERNALS__" in window;
export const isDesktopApp = isTauri;

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

const defaultSettings: AppSettings = {
  server_url: "http://127.0.0.1:7373",
  quick_capture_shortcut: "Ctrl+Shift+KeyM",
  clipboard_capture_shortcut: "Ctrl+Shift+Alt+KeyV",
  settings_shortcut: "Ctrl+Shift+KeyS",
  writing_mode: "split",
  preview_render_path: "typst-inline",
  preview_template: "literary",
  compact_sidebar_on_start: false,
  auto_sync_enabled: true,
  auto_sync_interval_secs: 60,
  realtime_sync_enabled: true,
};

const demoRepo: Repository = {
  id: "demo-repo",
  name: "Inbox",
  kind: "Persistent",
  sync_enabled: true,
  color: "#c86f52",
  created_at: new Date().toISOString(),
  updated_at: new Date().toISOString(),
};

let demoMemos: Memo[] = [
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

function demoStats(): LocalStats {
  return {
    memo_count: demoMemos.filter((memo) => !memo.deleted).length,
    repository_count: 1,
    pending_operations: 0,
    last_server_sequence: 0,
  };
}

export async function bootstrap(): Promise<Bootstrap> {
  if (isTauri) return invoke("bootstrap");
  return {
    repositories: [demoRepo],
    memos: demoMemos,
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
  localStorage.setItem("memo-sync-settings", JSON.stringify(settings));
  return settings;
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
  const repo: Repository = {
    id: crypto.randomUUID(),
    name,
    kind: temporary ? "Temporary" : "Persistent",
    sync_enabled: !temporary,
    color,
    created_at: new Date().toISOString(),
    updated_at: new Date().toISOString(),
  };
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
  const existing = id === demoRepo.id ? demoRepo : null;
  return {
    id,
    name: name.trim() || "Untitled repository",
    kind: existing?.kind ?? "Persistent",
    sync_enabled: existing?.kind === "Temporary" ? false : syncEnabled,
    color,
    created_at: existing?.created_at ?? now,
    updated_at: now,
  };
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
  const memo: Memo = {
    id: input.id ?? crypto.randomUUID(),
    repository_id: input.repository_id,
    title: input.title || "Untitled memo",
    body_md: input.body_md,
    tags: input.tags,
    pinned: input.pinned,
    archived: input.archived,
    deleted: false,
    created_at: new Date().toISOString(),
    updated_at: new Date().toISOString(),
    source,
    meta: { byte_len: input.body_md.length },
  };
  demoMemos = [memo, ...demoMemos.filter((item) => item.id !== memo.id)];
  return memo;
}

export async function deleteMemo(id: string): Promise<void> {
  if (isTauri) return invoke("delete_memo", { id });
  demoMemos = demoMemos.filter((item) => item.id !== id);
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
  const stored = localStorage.getItem("memo-sync-settings");
  if (!stored) return defaultSettings;
  try {
    return { ...defaultSettings, ...JSON.parse(stored) };
  } catch {
    return defaultSettings;
  }
}
