mod store;

use memo_core::{Memo, MemoFilter, MemoSource, Repository};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
};
use store::{LocalStore, SaveMemoInput, SyncSummary};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
    Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    store: LocalStore,
    device_id: String,
    settings_path: PathBuf,
    settings: Arc<Mutex<AppSettings>>,
}

#[derive(Debug, Serialize)]
struct Bootstrap {
    repositories: Vec<Repository>,
    memos: Vec<Memo>,
    device_id: String,
    settings: AppSettings,
}

#[derive(Debug, Clone, Deserialize)]
struct ShortcutCheckRequest {
    quick_capture_shortcut: String,
    clipboard_capture_shortcut: String,
    settings_shortcut: String,
}

#[derive(Debug, Clone, Serialize)]
struct ShortcutCheckResult {
    ok: bool,
    quick_available: bool,
    clipboard_available: bool,
    settings_available: bool,
    message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppSettings {
    server_url: String,
    quick_capture_shortcut: String,
    clipboard_capture_shortcut: String,
    #[serde(default = "default_settings_shortcut")]
    settings_shortcut: String,
    writing_mode: String,
    compact_sidebar_on_start: bool,
}

fn default_settings_shortcut() -> String {
    "Ctrl+Shift+KeyS".to_string()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            server_url: "http://127.0.0.1:7373".to_string(),
            quick_capture_shortcut: "Ctrl+Shift+KeyM".to_string(),
            clipboard_capture_shortcut: "Ctrl+Shift+Alt+KeyV".to_string(),
            settings_shortcut: default_settings_shortcut(),
            writing_mode: "split".to_string(),
            compact_sidebar_on_start: false,
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let store = tauri::async_runtime::block_on(LocalStore::open(
                data_dir.join("memo-sync.sqlite"),
            ))?;
            let device_id = load_or_create_device_id(&data_dir)?;
            let settings_path = data_dir.join("settings.json");
            let settings = load_settings(&settings_path)?;
            let settings = Arc::new(Mutex::new(settings));
            app.manage(AppState {
                store,
                device_id,
                settings_path,
                settings: settings.clone(),
            });
            setup_tray(app)?;
            setup_shortcuts(app.handle(), &settings.lock().unwrap())?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            get_app_settings,
            update_app_settings,
            check_shortcuts,
            create_repository,
            save_memo,
            save_quick_memo,
            delete_memo,
            capture_clipboard_memo,
            read_clipboard_text,
            search_memos,
            sync_now,
            show_main_window,
            show_quick_capture,
            show_settings_window,
            window_minimize,
            window_toggle_maximize,
            window_close
        ])
        .run(tauri::generate_context!())
        .expect("error while running memo desktop");
}

#[tauri::command]
async fn bootstrap(state: State<'_, AppState>) -> Result<Bootstrap, String> {
    let repositories = state.store.repositories().await.map_err(to_string)?;
    let memos = state
        .store
        .memos(MemoFilter::default())
        .await
        .map_err(to_string)?;
    Ok(Bootstrap {
        repositories,
        memos,
        device_id: state.device_id.clone(),
        settings: state.settings.lock().unwrap().clone(),
    })
}

#[tauri::command]
fn get_app_settings(state: State<'_, AppState>) -> Result<AppSettings, String> {
    Ok(state.settings.lock().map_err(to_string)?.clone())
}

#[tauri::command]
fn update_app_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    settings: AppSettings,
) -> Result<AppSettings, String> {
    validate_settings(&settings)?;
    let previous = state.settings.lock().map_err(to_string)?.clone();
    if let Err(error) = apply_shortcuts(&app, &settings) {
        let _ = apply_shortcuts(&app, &previous);
        return Err(error);
    }
    save_settings(&state.settings_path, &settings).map_err(to_string)?;
    *state.settings.lock().map_err(to_string)? = settings.clone();
    Ok(settings)
}

#[tauri::command]
fn check_shortcuts(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: ShortcutCheckRequest,
) -> Result<ShortcutCheckResult, String> {
    let current = state.settings.lock().map_err(to_string)?.clone();
    check_shortcut_availability(&app, &current, request)
}

#[tauri::command]
async fn create_repository(
    state: State<'_, AppState>,
    name: String,
    temporary: bool,
    color: String,
) -> Result<Repository, String> {
    state
        .store
        .create_repository(name, temporary, color, &state.device_id)
        .await
        .map_err(to_string)
}

#[tauri::command]
async fn save_memo(state: State<'_, AppState>, input: SaveMemoInput) -> Result<Memo, String> {
    state
        .store
        .save_memo(input, MemoSource::Manual, &state.device_id)
        .await
        .map_err(to_string)
}

#[tauri::command]
async fn save_quick_memo(state: State<'_, AppState>, input: SaveMemoInput) -> Result<Memo, String> {
    state
        .store
        .save_memo(input, MemoSource::QuickCapture, &state.device_id)
        .await
        .map_err(to_string)
}

#[tauri::command]
async fn delete_memo(state: State<'_, AppState>, id: Uuid) -> Result<(), String> {
    state
        .store
        .delete_memo(id, &state.device_id)
        .await
        .map_err(to_string)
}

#[tauri::command]
async fn capture_clipboard_memo(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    repository_id: Uuid,
) -> Result<Memo, String> {
    let text = app.clipboard().read_text().map_err(to_string)?;
    let input = SaveMemoInput {
        id: None,
        repository_id,
        title: "Clipboard capture".to_string(),
        body_md: text,
        tags: ["clipboard".to_string()].into_iter().collect(),
        pinned: false,
        archived: false,
    };
    state
        .store
        .save_memo(input, MemoSource::Clipboard, &state.device_id)
        .await
        .map_err(to_string)
}

#[tauri::command]
fn read_clipboard_text(app: tauri::AppHandle) -> Result<String, String> {
    app.clipboard().read_text().map_err(to_string)
}

#[tauri::command]
async fn search_memos(state: State<'_, AppState>, filter: MemoFilter) -> Result<Vec<Memo>, String> {
    state.store.memos(filter).await.map_err(to_string)
}

#[tauri::command]
async fn sync_now(state: State<'_, AppState>, server_url: String) -> Result<SyncSummary, String> {
    state
        .store
        .sync_now(&server_url, &state.device_id)
        .await
        .map_err(to_string)
}

#[tauri::command]
fn show_main_window(app: tauri::AppHandle) -> Result<(), String> {
    reveal_window(&app, false)
}

#[tauri::command]
async fn show_quick_capture(app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || reveal_quick_capture(&app))
        .await
        .map_err(to_string)?
}

#[tauri::command]
async fn show_settings_window(app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || reveal_settings(&app))
        .await
        .map_err(to_string)?
}

#[tauri::command]
fn window_minimize(window: tauri::Window) -> Result<(), String> {
    window.minimize().map_err(to_string)
}

#[tauri::command]
fn window_toggle_maximize(window: tauri::Window) -> Result<(), String> {
    if window.is_maximized().map_err(to_string)? {
        window.unmaximize().map_err(to_string)
    } else {
        window.maximize().map_err(to_string)
    }
}

#[tauri::command]
fn window_close(window: tauri::Window) -> Result<(), String> {
    window.hide().map_err(to_string)
}

fn setup_tray(app: &mut tauri::App) -> tauri::Result<()> {
    let show = MenuItemBuilder::with_id("show", "Show").build(app)?;
    let capture = MenuItemBuilder::with_id("capture", "Quick capture").build(app)?;
    let settings = MenuItemBuilder::with_id("settings", "Settings").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
    let menu = MenuBuilder::new(app)
        .items(&[&show, &capture, &settings, &quit])
        .build()?;
    TrayIconBuilder::new()
        .tooltip("Memo Sync")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => {
                let _ = reveal_window(app, false);
            }
            "capture" => {
                spawn_quick_capture(app.clone(), false);
            }
            "settings" => {
                spawn_settings_window(app.clone());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn setup_shortcuts(app: &tauri::AppHandle, settings: &AppSettings) -> anyhow::Result<()> {
    validate_settings(settings).map_err(anyhow::Error::msg)?;
    register_shortcut_handlers(app, settings).map_err(|error| anyhow::anyhow!(error))
}

fn apply_shortcuts(app: &tauri::AppHandle, settings: &AppSettings) -> Result<(), String> {
    Shortcut::from_str(&settings.quick_capture_shortcut).map_err(to_string)?;
    Shortcut::from_str(&settings.clipboard_capture_shortcut).map_err(to_string)?;
    Shortcut::from_str(&settings.settings_shortcut).map_err(to_string)?;
    app.global_shortcut()
        .unregister_all()
        .map_err(|error| error.to_string())?;
    register_shortcut_handlers(app, settings)
}

fn check_shortcut_availability(
    app: &tauri::AppHandle,
    current: &AppSettings,
    request: ShortcutCheckRequest,
) -> Result<ShortcutCheckResult, String> {
    if unique_shortcuts([
        request.quick_capture_shortcut.as_str(),
        request.clipboard_capture_shortcut.as_str(),
        request.settings_shortcut.as_str(),
    ])
    .is_err()
    {
        return Ok(ShortcutCheckResult {
            ok: false,
            quick_available: false,
            clipboard_available: false,
            settings_available: false,
            message: "Shortcuts must be different".to_string(),
        });
    }

    let quick = Shortcut::from_str(&request.quick_capture_shortcut).map_err(to_string)?;
    let clipboard = Shortcut::from_str(&request.clipboard_capture_shortcut).map_err(to_string)?;
    let settings = Shortcut::from_str(&request.settings_shortcut).map_err(to_string)?;
    let manager = app.global_shortcut();
    manager.unregister_all().map_err(to_string)?;

    let quick_available = manager.register(quick).is_ok();
    if quick_available {
        let _ = manager.unregister(quick);
    }

    let clipboard_available = manager.register(clipboard).is_ok();
    if clipboard_available {
        let _ = manager.unregister(clipboard);
    }

    let settings_available = manager.register(settings).is_ok();
    if settings_available {
        let _ = manager.unregister(settings);
    }

    let _ = manager.unregister_all();
    register_shortcut_handlers(app, current)?;

    let ok = quick_available && clipboard_available && settings_available;
    let mut blocked = Vec::new();
    if !quick_available {
        blocked.push("quick capture");
    }
    if !clipboard_available {
        blocked.push("clipboard capture");
    }
    if !settings_available {
        blocked.push("settings");
    }
    let message = if blocked.is_empty() {
        "Shortcuts are available".to_string()
    } else {
        format!("Shortcut blocked or already in use: {}", blocked.join(", "))
    };

    Ok(ShortcutCheckResult {
        ok,
        quick_available,
        clipboard_available,
        settings_available,
        message,
    })
}

fn register_shortcut_handlers(
    app: &tauri::AppHandle,
    settings: &AppSettings,
) -> Result<(), String> {
    let quick = settings.quick_capture_shortcut.clone();
    let clipboard = settings.clipboard_capture_shortcut.clone();
    let settings_shortcut = settings.settings_shortcut.clone();
    let handle = app.clone();
    app.global_shortcut()
        .on_shortcut(quick.as_str(), move |_app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                spawn_quick_capture(handle.clone(), false);
            }
        })
        .map_err(|error| error.to_string())?;
    let handle = app.clone();
    app.global_shortcut()
        .on_shortcut(clipboard.as_str(), move |_app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                spawn_quick_capture(handle.clone(), true);
            }
        })
        .map_err(|error| error.to_string())?;
    let handle = app.clone();
    app.global_shortcut()
        .on_shortcut(settings_shortcut.as_str(), move |_app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                spawn_settings_window(handle.clone());
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn spawn_quick_capture(app: tauri::AppHandle, clipboard: bool) {
    std::thread::spawn(move || {
        if let Err(error) = reveal_quick_capture(&app) {
            eprintln!("failed to reveal quick capture window: {error}");
            return;
        }
        if clipboard {
            let _ = app.emit("clipboard-capture-requested", ());
        }
    });
}

fn spawn_settings_window(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        if let Err(error) = reveal_settings(&app) {
            eprintln!("failed to reveal settings window: {error}");
        }
    });
}

fn reveal_window(app: &tauri::AppHandle, quick_capture: bool) -> Result<(), String> {
    let Some(window) = app.get_webview_window("main") else {
        return Err("main window not found".to_string());
    };
    window.show().map_err(to_string)?;
    window.set_focus().map_err(to_string)?;
    if quick_capture {
        window.emit("open-quick-capture", ()).map_err(to_string)?;
    }
    Ok(())
}

fn reveal_quick_capture(app: &tauri::AppHandle) -> Result<(), String> {
    let window = if let Some(window) = app.get_webview_window("quick-capture") {
        window
    } else {
        WebviewWindowBuilder::new(app, "quick-capture", WebviewUrl::App("index.html".into()))
            .title("Quick Capture")
            .inner_size(560.0, 430.0)
            .min_inner_size(480.0, 360.0)
            .center()
            .resizable(true)
            .decorations(false)
            .always_on_top(true)
            .focused(true)
            .skip_taskbar(false)
            .build()
            .map_err(to_string)?
    };
    let was_visible = window.is_visible().unwrap_or(false);
    window.show().map_err(to_string)?;
    let _ = window.unminimize();
    window.set_always_on_top(true).map_err(to_string)?;
    if !was_visible {
        window.center().map_err(to_string)?;
    }
    window.set_focus().map_err(to_string)?;
    window.emit("open-quick-capture", ()).map_err(to_string)?;
    Ok(())
}

fn reveal_settings(app: &tauri::AppHandle) -> Result<(), String> {
    let window = if let Some(window) = app.get_webview_window("settings") {
        window
    } else {
        WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("index.html".into()))
            .title("Memo Sync Settings")
            .inner_size(680.0, 560.0)
            .min_inner_size(620.0, 500.0)
            .center()
            .resizable(true)
            .decorations(false)
            .always_on_top(false)
            .focused(true)
            .skip_taskbar(false)
            .build()
            .map_err(to_string)?
    };
    let was_visible = window.is_visible().unwrap_or(false);
    window.show().map_err(to_string)?;
    let _ = window.unminimize();
    if !was_visible {
        window.center().map_err(to_string)?;
    }
    window.set_focus().map_err(to_string)?;
    Ok(())
}

fn load_or_create_device_id(data_dir: &std::path::Path) -> anyhow::Result<String> {
    let path = data_dir.join("device-id");
    if path.exists() {
        return Ok(std::fs::read_to_string(path)?.trim().to_string());
    }
    let id = format!("device-{}", Uuid::now_v7());
    std::fs::write(path, &id)?;
    Ok(id)
}

fn load_settings(path: &Path) -> anyhow::Result<AppSettings> {
    if path.exists() {
        let text = std::fs::read_to_string(path)?;
        let settings = serde_json::from_str::<AppSettings>(&text)?;
        validate_settings(&settings).map_err(anyhow::Error::msg)?;
        return Ok(settings);
    }
    let settings = AppSettings::default();
    save_settings(path, &settings)?;
    Ok(settings)
}

fn save_settings(path: &Path, settings: &AppSettings) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(settings)?;
    std::fs::write(path, text)?;
    Ok(())
}

fn validate_settings(settings: &AppSettings) -> Result<(), String> {
    if !(settings.server_url.starts_with("http://") || settings.server_url.starts_with("https://"))
    {
        return Err("Sync endpoint must start with http:// or https://".to_string());
    }
    unique_shortcuts([
        settings.quick_capture_shortcut.as_str(),
        settings.clipboard_capture_shortcut.as_str(),
        settings.settings_shortcut.as_str(),
    ])?;
    Shortcut::from_str(&settings.quick_capture_shortcut).map_err(to_string)?;
    Shortcut::from_str(&settings.clipboard_capture_shortcut).map_err(to_string)?;
    Shortcut::from_str(&settings.settings_shortcut).map_err(to_string)?;
    match settings.writing_mode.as_str() {
        "split" | "edit" | "preview" => Ok(()),
        _ => Err("Writing mode must be split, edit, or preview".to_string()),
    }
}

fn unique_shortcuts(shortcuts: [&str; 3]) -> Result<(), String> {
    for index in 0..shortcuts.len() {
        for next in (index + 1)..shortcuts.len() {
            if shortcuts[index] == shortcuts[next] {
                return Err("Shortcuts must be different".to_string());
            }
        }
    }
    Ok(())
}

fn to_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_are_valid() {
        validate_settings(&AppSettings::default()).unwrap();
    }

    #[test]
    fn settings_reject_duplicate_shortcuts() {
        let settings = AppSettings {
            clipboard_capture_shortcut: "Ctrl+Shift+KeyM".to_string(),
            ..AppSettings::default()
        };

        assert!(validate_settings(&settings).is_err());
    }

    #[test]
    fn settings_reject_non_http_sync_endpoint() {
        let settings = AppSettings {
            server_url: "file:///tmp/memo".to_string(),
            ..AppSettings::default()
        };

        assert!(validate_settings(&settings).is_err());
    }
}
