mod store;

use memo_core::{Memo, MemoFilter, MemoSource, Repository};
use memo_render::{RenderCache, RenderMemoInput, RenderMemoMetadata, RenderMemoOutput};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use store::{LocalStats, LocalStore, SaveMemoInput, SyncSummary};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
    Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

const EVENT_OPEN_QUICK_CAPTURE: &str = "open-quick-capture";
const EVENT_CLIPBOARD_CAPTURE_REQUESTED: &str = "clipboard-capture-requested";
const EVENT_MEMOS_CHANGED: &str = "memos-changed";
const EVENT_SYNC_COMPLETED: &str = "sync-completed";
const PREVIEW_PROTOCOL: &str = "memo-preview";

#[derive(Clone)]
struct AppState {
    store: LocalStore,
    device_id: String,
    settings_path: PathBuf,
    settings: Arc<Mutex<AppSettings>>,
    sync_lock: Arc<AsyncMutex<()>>,
    render_cache: Arc<Mutex<RenderCache>>,
}

#[derive(Debug, Serialize)]
struct Bootstrap {
    repositories: Vec<Repository>,
    memos: Vec<Memo>,
    device_id: String,
    settings: AppSettings,
    local_stats: LocalStats,
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

#[derive(Debug, Clone, Serialize)]
struct RenderMemoAssetOutput {
    url: String,
    diagnostics: Vec<String>,
    elapsed_ms: u128,
    cache_key: String,
    cached: bool,
    bytes: usize,
    width_pt: f64,
    height_pt: f64,
    pages: Vec<RenderPageAssetOutput>,
}

#[derive(Debug, Clone, Serialize)]
struct RenderPageAssetOutput {
    index: usize,
    url: String,
    width_pt: f64,
    height_pt: f64,
    bytes: usize,
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
    #[serde(default = "default_auto_sync_enabled")]
    auto_sync_enabled: bool,
    #[serde(default = "default_auto_sync_interval_secs")]
    auto_sync_interval_secs: u64,
    #[serde(default = "default_realtime_sync_enabled")]
    realtime_sync_enabled: bool,
}

fn default_settings_shortcut() -> String {
    "Ctrl+Shift+KeyS".to_string()
}

fn default_auto_sync_enabled() -> bool {
    true
}

fn default_auto_sync_interval_secs() -> u64 {
    60
}

fn default_realtime_sync_enabled() -> bool {
    true
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
            auto_sync_enabled: default_auto_sync_enabled(),
            auto_sync_interval_secs: default_auto_sync_interval_secs(),
            realtime_sync_enabled: default_realtime_sync_enabled(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct SyncCompletedPayload {
    ok: bool,
    pushed: usize,
    pulled: usize,
    server_sequence: i64,
    message: String,
    background: bool,
}

#[derive(Debug, Clone, Serialize)]
struct MemosChangedPayload {
    active_memo_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecondInstanceIntent {
    ShowMain,
    QuickCapture,
    ClipboardCapture,
    Settings,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .register_uri_scheme_protocol(PREVIEW_PROTOCOL, |ctx, request| {
            preview_protocol_response(ctx.app_handle(), request.uri().path())
        })
        .plugin(tauri_plugin_single_instance::init(
            |app, argv, _working_directory| match second_instance_intent(&argv) {
                SecondInstanceIntent::ShowMain => {
                    let _ = reveal_window(app, false);
                }
                SecondInstanceIntent::QuickCapture => spawn_quick_capture(app.clone(), false),
                SecondInstanceIntent::ClipboardCapture => spawn_quick_capture(app.clone(), true),
                SecondInstanceIntent::Settings => spawn_settings_window(app.clone()),
            },
        ))
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
            let sync_lock = Arc::new(AsyncMutex::new(()));
            let render_cache = Arc::new(Mutex::new(RenderCache::new(96, 24 * 1024 * 1024)));
            app.manage(AppState {
                store: store.clone(),
                device_id: device_id.clone(),
                settings_path,
                settings: settings.clone(),
                sync_lock: sync_lock.clone(),
                render_cache,
            });
            spawn_background_sync(
                app.handle().clone(),
                store.clone(),
                device_id.clone(),
                settings.clone(),
                sync_lock.clone(),
            );
            spawn_realtime_sync(
                app.handle().clone(),
                store,
                device_id,
                settings.clone(),
                sync_lock,
            );
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
            update_repository,
            render_memo_preview,
            render_memo_preview_asset,
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
    let settings = state.settings.lock().unwrap().clone();
    let local_stats = state.store.stats().await.map_err(to_string)?;
    Ok(Bootstrap {
        repositories,
        memos,
        device_id: state.device_id.clone(),
        settings,
        local_stats,
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
async fn update_repository(
    state: State<'_, AppState>,
    id: Uuid,
    name: String,
    color: String,
    sync_enabled: bool,
) -> Result<Repository, String> {
    state
        .store
        .update_repository(id, name, color, sync_enabled, &state.device_id)
        .await
        .map_err(to_string)
}

#[tauri::command]
async fn render_memo_preview(
    state: State<'_, AppState>,
    input: RenderMemoInput,
) -> Result<RenderMemoOutput, String> {
    render_memo_output(&state, input).await
}

#[tauri::command]
async fn render_memo_preview_asset(
    state: State<'_, AppState>,
    input: RenderMemoInput,
) -> Result<RenderMemoAssetOutput, String> {
    let cache_key = memo_render::render_cache_key(&input);
    if let Some(metadata) = state
        .render_cache
        .lock()
        .map_err(to_string)?
        .get_metadata(&cache_key)
    {
        return Ok(asset_output_from_metadata(metadata));
    }

    let output = tauri::async_runtime::spawn_blocking(move || memo_render::render_memo(input))
        .await
        .map_err(to_string)?
        .map_err(to_string)?;
    let metadata = output.metadata(false, output.elapsed_ms);
    let inserted = state.render_cache.lock().map_err(to_string)?.insert(output);
    if !inserted {
        return Err("preview is too large for the asset cache".to_string());
    }
    Ok(asset_output_from_metadata(metadata))
}

async fn render_memo_output(
    state: &AppState,
    input: RenderMemoInput,
) -> Result<RenderMemoOutput, String> {
    let cache_key = memo_render::render_cache_key(&input);
    if let Some(output) = state
        .render_cache
        .lock()
        .map_err(to_string)?
        .get(&cache_key)
    {
        return Ok(output);
    }
    let output = tauri::async_runtime::spawn_blocking(move || memo_render::render_memo(input))
        .await
        .map_err(to_string)?
        .map_err(to_string)?;
    state
        .render_cache
        .lock()
        .map_err(to_string)?
        .insert(output.clone());
    Ok(output)
}

fn asset_output_from_metadata(metadata: RenderMemoMetadata) -> RenderMemoAssetOutput {
    let cache_key = metadata.cache_key;
    RenderMemoAssetOutput {
        url: preview_asset_url(&cache_key),
        diagnostics: metadata.diagnostics,
        elapsed_ms: metadata.elapsed_ms,
        cache_key: cache_key.clone(),
        cached: metadata.cached,
        bytes: metadata.bytes,
        width_pt: metadata.width_pt,
        height_pt: metadata.height_pt,
        pages: metadata
            .pages
            .into_iter()
            .map(|page| RenderPageAssetOutput {
                index: page.index,
                url: preview_page_url(&cache_key, page.index),
                width_pt: page.width_pt,
                height_pt: page.height_pt,
                bytes: page.bytes,
            })
            .collect(),
    }
}

fn preview_asset_url(cache_key: &str) -> String {
    preview_protocol_url(&format!("/svg/{cache_key}.svg"))
}

fn preview_page_url(cache_key: &str, page_index: usize) -> String {
    preview_protocol_url(&format!("/page/{cache_key}/{page_index}.svg"))
}

fn preview_protocol_url(path: &str) -> String {
    #[cfg(any(target_os = "windows", target_os = "android"))]
    {
        format!("http://{PREVIEW_PROTOCOL}.localhost{path}")
    }
    #[cfg(not(any(target_os = "windows", target_os = "android")))]
    {
        format!("{PREVIEW_PROTOCOL}://localhost{path}")
    }
}

enum PreviewPath<'a> {
    Merged { cache_key: &'a str },
    Page { cache_key: &'a str, index: usize },
}

fn preview_path(path: &str) -> Option<PreviewPath<'_>> {
    if let Some(key) = path
        .strip_prefix("/svg/")
        .and_then(|path| path.strip_suffix(".svg"))
    {
        if is_preview_cache_key(key) {
            return Some(PreviewPath::Merged { cache_key: key });
        }
        return None;
    }

    let path = path.strip_prefix("/page/")?.strip_suffix(".svg")?;
    let (key, index) = path.split_once('/')?;
    if !is_preview_cache_key(key) {
        return None;
    }
    Some(PreviewPath::Page {
        cache_key: key,
        index: index.parse().ok()?,
    })
}

fn is_preview_cache_key(key: &str) -> bool {
    if key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        true
    } else {
        false
    }
}

fn preview_protocol_response(app: &tauri::AppHandle, path: &str) -> tauri::http::Response<Vec<u8>> {
    let Some(preview_path) = preview_path(path) else {
        return preview_protocol_error(tauri::http::StatusCode::BAD_REQUEST, "bad preview path");
    };
    let Some(state) = app.try_state::<AppState>() else {
        return preview_protocol_error(
            tauri::http::StatusCode::SERVICE_UNAVAILABLE,
            "preview cache is not ready",
        );
    };
    let svg = match state.render_cache.lock() {
        Ok(mut cache) => match preview_path {
            PreviewPath::Merged { cache_key } => cache.get_svg(cache_key),
            PreviewPath::Page { cache_key, index } => cache.get_page_svg(cache_key, index),
        },
        Err(error) => {
            return preview_protocol_error(
                tauri::http::StatusCode::INTERNAL_SERVER_ERROR,
                &error.to_string(),
            )
        }
    };
    let Some(svg) = svg else {
        return preview_protocol_error(tauri::http::StatusCode::NOT_FOUND, "preview expired");
    };
    tauri::http::Response::builder()
        .status(tauri::http::StatusCode::OK)
        .header(
            tauri::http::header::CONTENT_TYPE,
            "image/svg+xml; charset=utf-8",
        )
        .header(
            tauri::http::header::CACHE_CONTROL,
            "private, max-age=300, stale-while-revalidate=30",
        )
        .body(svg.into_bytes())
        .unwrap_or_else(|error| {
            preview_protocol_error(
                tauri::http::StatusCode::INTERNAL_SERVER_ERROR,
                &error.to_string(),
            )
        })
}

fn preview_protocol_error(
    status: tauri::http::StatusCode,
    message: &str,
) -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(status)
        .header(
            tauri::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )
        .body(message.as_bytes().to_vec())
        .expect("preview protocol error response")
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
async fn sync_now(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    server_url: String,
) -> Result<SyncSummary, String> {
    let _guard = state.sync_lock.lock().await;
    let summary = state
        .store
        .sync_now(&server_url, &state.device_id)
        .await
        .map_err(to_string)?;
    emit_sync_completed(&app, &summary, false, None);
    if summary.pulled > 0 {
        emit_memos_changed(&app, None);
    }
    Ok(summary)
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

fn spawn_background_sync(
    app: tauri::AppHandle,
    store: LocalStore,
    device_id: String,
    settings: Arc<Mutex<AppSettings>>,
    sync_lock: Arc<AsyncMutex<()>>,
) {
    tauri::async_runtime::spawn(async move {
        let mut last_attempt = Instant::now()
            .checked_sub(Duration::from_secs(3600))
            .unwrap_or_else(Instant::now);
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let snapshot = match settings_snapshot(&settings) {
                Ok(settings) => settings,
                Err(error) => {
                    let _ = app.emit(
                        EVENT_SYNC_COMPLETED,
                        SyncCompletedPayload {
                            ok: false,
                            pushed: 0,
                            pulled: 0,
                            server_sequence: 0,
                            message: error,
                            background: true,
                        },
                    );
                    continue;
                }
            };
            if !snapshot.auto_sync_enabled {
                continue;
            }
            let interval = Duration::from_secs(snapshot.auto_sync_interval_secs.clamp(15, 3600));
            if last_attempt.elapsed() < interval {
                continue;
            }
            last_attempt = Instant::now();
            try_background_sync(
                &app,
                &store,
                &device_id,
                &snapshot.server_url,
                &sync_lock,
                None,
            )
            .await;
        }
    });
}

fn spawn_realtime_sync(
    app: tauri::AppHandle,
    store: LocalStore,
    device_id: String,
    settings: Arc<Mutex<AppSettings>>,
    sync_lock: Arc<AsyncMutex<()>>,
) {
    tauri::async_runtime::spawn(async move {
        loop {
            let snapshot = match settings_snapshot(&settings) {
                Ok(settings) => settings,
                Err(error) => {
                    emit_background_sync_error(&app, error);
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }
            };
            if !snapshot.auto_sync_enabled || !snapshot.realtime_sync_enabled {
                tokio::time::sleep(Duration::from_secs(10)).await;
                continue;
            }
            let stats = match store.stats().await {
                Ok(stats) => stats,
                Err(error) => {
                    emit_background_sync_error(&app, error.to_string());
                    tokio::time::sleep(Duration::from_secs(15)).await;
                    continue;
                }
            };
            match store
                .wait_for_remote_change(
                    &snapshot.server_url,
                    stats.last_server_sequence,
                    Duration::from_secs(45),
                )
                .await
            {
                Ok(change)
                    if change.changed && change.server_sequence > stats.last_server_sequence =>
                {
                    try_background_sync(
                        &app,
                        &store,
                        &device_id,
                        &snapshot.server_url,
                        &sync_lock,
                        Some("Realtime sync completed".to_string()),
                    )
                    .await;
                }
                Ok(_) => {}
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(15)).await;
                }
            }
        }
    });
}

fn settings_snapshot(settings: &Arc<Mutex<AppSettings>>) -> Result<AppSettings, String> {
    settings
        .lock()
        .map_err(to_string)
        .map(|settings| settings.clone())
}

async fn try_background_sync(
    app: &tauri::AppHandle,
    store: &LocalStore,
    device_id: &str,
    server_url: &str,
    sync_lock: &Arc<AsyncMutex<()>>,
    message: Option<String>,
) {
    let Ok(_guard) = sync_lock.try_lock() else {
        return;
    };
    match store.sync_now(server_url, device_id).await {
        Ok(summary) => {
            emit_sync_completed(app, &summary, true, message);
            if summary.pulled > 0 {
                emit_memos_changed(app, None);
            }
        }
        Err(error) => emit_background_sync_error(app, error.to_string()),
    }
}

fn emit_background_sync_error(app: &tauri::AppHandle, message: String) {
    let _ = app.emit(
        EVENT_SYNC_COMPLETED,
        SyncCompletedPayload {
            ok: false,
            pushed: 0,
            pulled: 0,
            server_sequence: 0,
            message,
            background: true,
        },
    );
}

fn emit_sync_completed(
    app: &tauri::AppHandle,
    summary: &SyncSummary,
    background: bool,
    message: Option<String>,
) {
    let _ = app.emit(
        EVENT_SYNC_COMPLETED,
        SyncCompletedPayload {
            ok: true,
            pushed: summary.pushed,
            pulled: summary.pulled,
            server_sequence: summary.server_sequence,
            message: message.unwrap_or_else(|| "Sync completed".to_string()),
            background,
        },
    );
}

fn emit_memos_changed(app: &tauri::AppHandle, active_memo_id: Option<String>) {
    let _ = app.emit(EVENT_MEMOS_CHANGED, MemosChangedPayload { active_memo_id });
}

fn second_instance_intent(argv: &[String]) -> SecondInstanceIntent {
    for arg in argv.iter().map(|arg| arg.to_ascii_lowercase()) {
        if arg == "--clipboard-capture"
            || arg == "--capture-clipboard"
            || arg == "memo-sync://clipboard-capture"
        {
            return SecondInstanceIntent::ClipboardCapture;
        }
        if arg == "--quick-capture" || arg == "--capture" || arg == "memo-sync://quick-capture" {
            return SecondInstanceIntent::QuickCapture;
        }
        if arg == "--settings" || arg == "memo-sync://settings" {
            return SecondInstanceIntent::Settings;
        }
    }
    SecondInstanceIntent::ShowMain
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
            let _ = app.emit(EVENT_CLIPBOARD_CAPTURE_REQUESTED, ());
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
        window
            .emit(EVENT_OPEN_QUICK_CAPTURE, ())
            .map_err(to_string)?;
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
    window
        .emit(EVENT_OPEN_QUICK_CAPTURE, ())
        .map_err(to_string)?;
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
        "split" | "edit" | "preview" => {}
        _ => return Err("Writing mode must be split, edit, or preview".to_string()),
    }
    if !(15..=3600).contains(&settings.auto_sync_interval_secs) {
        return Err("Auto sync interval must be between 15 and 3600 seconds".to_string());
    }
    Ok(())
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

    #[test]
    fn second_instance_args_choose_specific_windows() {
        assert_eq!(
            second_instance_intent(&["memo-desktop".to_string()]),
            SecondInstanceIntent::ShowMain
        );
        assert_eq!(
            second_instance_intent(&["memo-desktop".to_string(), "--quick-capture".to_string()]),
            SecondInstanceIntent::QuickCapture
        );
        assert_eq!(
            second_instance_intent(&[
                "memo-desktop".to_string(),
                "--clipboard-capture".to_string()
            ]),
            SecondInstanceIntent::ClipboardCapture
        );
        assert_eq!(
            second_instance_intent(&["memo-desktop".to_string(), "--settings".to_string()]),
            SecondInstanceIntent::Settings
        );
    }

    #[test]
    fn preview_protocol_accepts_only_svg_cache_keys() {
        let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(matches!(
            preview_path(&format!("/svg/{key}.svg")),
            Some(PreviewPath::Merged { cache_key }) if cache_key == key
        ));
        assert!(matches!(
            preview_path(&format!("/page/{key}/2.svg")),
            Some(PreviewPath::Page { cache_key, index: 2 }) if cache_key == key
        ));
        assert!(preview_path("/svg/not-a-key.svg").is_none());
        assert!(preview_path("/page/not-a-key/0.svg").is_none());
        assert!(preview_path(
            "/other/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.svg"
        )
        .is_none());
    }
}
