import type { AppSettings } from "./types";

export const DEFAULT_APP_SETTINGS: AppSettings = {
  server_url: "http://127.0.0.1:7373",
  quick_capture_shortcut: "Ctrl+Shift+KeyM",
  clipboard_capture_shortcut: "Ctrl+Shift+Alt+KeyV",
  settings_shortcut: "Ctrl+Shift+KeyS",
  writing_mode: "split",
  preview_render_path: "auto",
  preview_markup_mode: "auto",
  preview_template: "literary",
  compact_sidebar_on_start: false,
  auto_sync_enabled: true,
  auto_sync_interval_secs: 60,
  realtime_sync_enabled: true,
};

export function withDefaultSettings(settings: Partial<AppSettings> | null | undefined): AppSettings {
  return { ...DEFAULT_APP_SETTINGS, ...(settings ?? {}) };
}
