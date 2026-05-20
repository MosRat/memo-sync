export type RepositoryKind = "Temporary" | "Persistent";

export interface Repository {
  id: string;
  name: string;
  kind: RepositoryKind;
  sync_enabled: boolean;
  color: string;
  created_at: string;
  updated_at: string;
}

export interface Memo {
  id: string;
  repository_id: string;
  title: string;
  body_md: string;
  tags: string[];
  pinned: boolean;
  archived: boolean;
  deleted: boolean;
  created_at: string;
  updated_at: string;
  source: "Manual" | "Clipboard" | "QuickCapture" | "Import";
  meta: {
    language?: string | null;
    url?: string | null;
    device_name?: string | null;
    byte_len: number;
  };
}

export interface Bootstrap {
  repositories: Repository[];
  memos: Memo[];
  device_id: string;
  settings: AppSettings;
}

export interface SaveMemoInput {
  id?: string | null;
  repository_id: string;
  title: string;
  body_md: string;
  tags: string[];
  pinned: boolean;
  archived: boolean;
}

export interface AppSettings {
  server_url: string;
  quick_capture_shortcut: string;
  clipboard_capture_shortcut: string;
  settings_shortcut: string;
  writing_mode: "split" | "edit" | "preview";
  compact_sidebar_on_start: boolean;
  auto_sync_enabled: boolean;
  auto_sync_interval_secs: number;
}

export interface ShortcutCheckResult {
  ok: boolean;
  quick_available: boolean;
  clipboard_available: boolean;
  settings_available: boolean;
  message: string;
}
