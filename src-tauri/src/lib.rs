use tauri::{
    AppHandle, Manager,
    menu::{MenuBuilder, MenuItemBuilder, MenuItem, SubmenuBuilder, CheckMenuItemBuilder},
    tray::TrayIconBuilder,
    image::Image,
};
use tauri_plugin_autostart::{MacosLauncher, AutoLaunchManager};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use log::{info, warn};

mod omlx_manager;
mod config;
use omlx_manager::ServerManager;
use config::AppConfig;

const ICON_ON_PNG: &[u8] = include_bytes!("../icons/tray-on.png");
const ICON_OFF_PNG: &[u8] = include_bytes!("../icons/tray-off.png");

const LOADING_FRAMES: &[&[u8]] = &[
    include_bytes!("../icons/tray-loading-0.png"),
    include_bytes!("../icons/tray-loading-1.png"),
    include_bytes!("../icons/tray-loading-2.png"),
    include_bytes!("../icons/tray-loading-3.png"),
];

const ICON_CPP_ON_PNG: &[u8] = include_bytes!("../icons/tray-cpp-on.png");
const ICON_CPP_OFF_PNG: &[u8] = include_bytes!("../icons/tray-cpp-off.png");

const CPP_LOADING_FRAMES: &[&[u8]] = &[
    include_bytes!("../icons/tray-cpp-loading-0.png"),
    include_bytes!("../icons/tray-cpp-loading-1.png"),
    include_bytes!("../icons/tray-cpp-loading-2.png"),
    include_bytes!("../icons/tray-cpp-loading-3.png"),
];

/// Tray icon set for a backend (selected at startup from `server_type`).
#[derive(Clone, Copy)]
struct IconSet {
    on: &'static [u8],
    off: &'static [u8],
    loading: &'static [&'static [u8]],
}

const ICONSET_OMLX: IconSet = IconSet { on: ICON_ON_PNG, off: ICON_OFF_PNG, loading: LOADING_FRAMES };
const ICONSET_CPP: IconSet = IconSet { on: ICON_CPP_ON_PNG, off: ICON_CPP_OFF_PNG, loading: CPP_LOADING_FRAMES };

fn png_to_tauri_image(png_bytes: &[u8]) -> Image<'static> {
    let img = image::load_from_memory(png_bytes).expect("invalid embedded PNG");
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Image::new_owned(rgba.into_raw(), w, h)
}

struct AppState {
    manager: Arc<ServerManager>,
    status_item: MenuItem<tauri::Wry>,
    animating: Arc<AtomicBool>,
    switching: Arc<AtomicBool>,
    dashboard_url: String,
    default_model: String,
    icons: IconSet,
}

fn set_tray_icon(app: &AppHandle, icon_bytes: &[u8]) {
    if let Some(tray) = app.tray_by_id("main") {
        let img = png_to_tauri_image(icon_bytes);
        tray.set_icon(Some(img)).ok();
    }
}

fn set_tray_status(app: &AppHandle, msg: &str) {
    if let Some(tray) = app.tray_by_id("main") {
        tray.set_title(Some(&format!(" {}", msg))).ok();
    }
}

fn build_backend_submenu(app: &AppHandle, current: &str) -> tauri::Result<tauri::menu::Submenu<tauri::Wry>> {
    let is_omlx = current != "llamacpp";
    SubmenuBuilder::with_id(app, "backend", "Server Type")
        .item(&CheckMenuItemBuilder::with_id("backend:omlx", "oMLX").checked(is_omlx).build(app)?)
        .item(&CheckMenuItemBuilder::with_id("backend:llamacpp", "llama.cpp").checked(!is_omlx).build(app)?)
        .build()
}

fn build_full_menu(app: &AppHandle, models: &[omlx_manager::ModelInfo]) {
    let state = app.state::<AppState>();

    let mut sub = SubmenuBuilder::with_id(app, "models", "Models");
    if models.is_empty() {
        if let Ok(item) = MenuItemBuilder::with_id("model:none", "No models").enabled(false).build(app) {
            sub = sub.item(&item);
        }
    } else {
        for m in models {
            let text = if m.loaded {
                format!("●  {}", m.label)
            } else {
                format!("     {}", m.label)
            };
            if let Ok(item) = MenuItemBuilder::with_id(format!("model:{}", m.id), text).build(app) {
                sub = sub.item(&item);
            }
        }
    }
    let Ok(model_submenu) = sub.build() else { return };

    let local = state.manager.is_local;
    let start_item = MenuItemBuilder::with_id("start", "Start Server").enabled(local).build(app).unwrap();
    let stop_item = MenuItemBuilder::with_id("stop", "Stop Server").enabled(local).build(app).unwrap();
    let restart_item = MenuItemBuilder::with_id("restart", "Restart Server").enabled(local).build(app).unwrap();

    let backend_submenu = build_backend_submenu(app, &state.manager.server_type).unwrap();

    let autostart_on = app.state::<AutoLaunchManager>().is_enabled().unwrap_or(false);
    let login_item = CheckMenuItemBuilder::with_id("login", "Launch at Login")
        .checked(autostart_on)
        .build(app).unwrap();

    let prefs_item = MenuItemBuilder::with_id("prefs", "Preferences…").build(app).unwrap();
    let quit_item = MenuItemBuilder::with_id("quit", "Quit").build(app).unwrap();

    let menu = MenuBuilder::new(app)
        .item(&state.status_item)
        .separator()
        .item(&start_item)
        .item(&stop_item)
        .item(&restart_item)
        .separator()
        .item(&backend_submenu)
        .item(&model_submenu)
        .separator()
        .item(&login_item)
        .item(&prefs_item)
        .separator()
        .item(&quit_item)
        .build()
        .unwrap();

    if let Some(tray) = app.tray_by_id("main") {
        tray.set_menu(Some(menu)).ok();
    }
}

fn refresh_model_menu(app: &AppHandle, mgr: &Arc<ServerManager>) {
    let app = app.clone();
    let mgr = mgr.clone();
    tauri::async_runtime::spawn(async move {
        let models = match mgr.list_models().await {
            Ok(m) => {
                info!("Model list: {} model(s)", m.len());
                m
            }
            Err(e) => {
                warn!("Failed to list models: {}", e);
                vec![]
            }
        };
        build_full_menu(&app, &models);
    });
}

fn update_tray(app: &AppHandle, running: bool, model: &str) {
    let state = app.state::<AppState>();
    let mgr = &state.manager;

    if !state.animating.load(Ordering::Relaxed) {
        set_tray_icon(app, if running { state.icons.on } else { state.icons.off });
    }

    if let Some(tray) = app.tray_by_id("main") {
        // The pill icon itself identifies the backend (oMLX vs .CPP), so the
        // title only carries the active model name (or nothing when idle).
        if running && !model.is_empty() {
            tray.set_title(Some(&format!(" {}", model))).ok();
        } else if running {
            tray.set_title(Some(" No model loaded")).ok();
        } else {
            tray.set_title(None::<&str>).ok();
        }

        let name = mgr.display_name();
        let tooltip = if running {
            format!("{}: running ({})", name, model)
        } else {
            format!("{}: stopped", name)
        };
        tray.set_tooltip(Some(&tooltip)).ok();
    }

    let text = if running {
        if model.is_empty() {
            "●  Running".to_string()
        } else {
            format!("●  {}", model)
        }
    } else {
        "○  Stopped".to_string()
    };
    state.status_item.set_text(text).ok();
}

fn save_last_model(model_id: &str) {
    if let Ok(mut cfg) = AppConfig::load() {
        if cfg.default_model != model_id {
            cfg.default_model = model_id.to_string();
            cfg.save().ok();
        }
    }
}

#[tauri::command]
fn get_config() -> Result<AppConfig, String> {
    AppConfig::load().map_err(|e| e.to_string())
}

#[tauri::command]
fn save_config(config: AppConfig) -> Result<(), String> {
    config.save().map_err(|e| e.to_string())
}

#[tauri::command]
fn close_settings(app_handle: AppHandle) {
    if let Some(win) = app_handle.get_webview_window("settings") {
        win.close().ok();
    }
}

fn open_settings(app_handle: &AppHandle) {
    if let Some(win) = app_handle.get_webview_window("settings") {
        win.show().ok();
        win.set_focus().ok();
        return;
    }
    let _win = tauri::WebviewWindowBuilder::new(
        app_handle,
        "settings",
        tauri::WebviewUrl::App("settings.html".into()),
    )
    .title("LLM Menubar — Settings")
    .inner_size(420.0, 650.0)
    .resizable(false)
    .minimizable(false)
    .maximizable(false)
    .closable(false)
    .decorations(false)
    .always_on_top(true)
    .transparent(true)
    .visible(true)
    .focused(true)
    .build()
    .ok();
}

fn auto_load_model(app: &AppHandle, mgr: &Arc<ServerManager>, default_model: &str) {
    let app = app.clone();
    let mgr = mgr.clone();
    let default = default_model.to_string();
    tauri::async_runtime::spawn(async move {
        let models = match mgr.list_models().await {
            Ok(m) => m,
            Err(_) => return,
        };
        if models.iter().any(|m| m.loaded) { return; }

        let to_load = if !default.is_empty() {
            models.iter().find(|m| m.id == default).map(|m| m.id.clone())
        } else if models.len() == 1 {
            models.first().map(|m| m.id.clone())
        } else {
            None
        };

        if let Some(model_id) = to_load {
            info!("Auto-loading model: {}", model_id);
            if mgr.load_model(&model_id).await.is_ok() {
                let status = mgr.check_health().await;
                update_tray(&app, status.running, &status.model);
                refresh_model_menu(&app, &mgr);
            }
        }
    });
}

fn start_and_animate(app_handle: AppHandle, mgr: Arc<ServerManager>, animating: Arc<AtomicBool>, dashboard_url: String) {
    animating.store(true, Ordering::Relaxed);

    let name = mgr.display_name();
    if let Some(state) = app_handle.try_state::<AppState>() {
        state.status_item.set_text("◌  Starting…").ok();
    }
    set_tray_status(&app_handle, "Starting server… 0s");
    if let Some(tray) = app_handle.tray_by_id("main") {
        tray.set_tooltip(Some(&format!("{}: starting…", name))).ok();
    }

    let frames = app_handle.try_state::<AppState>()
        .map(|s| s.icons.loading)
        .unwrap_or(LOADING_FRAMES);

    tauri::async_runtime::spawn(async move {
        let mut frame = 0u32;
        for _ in 0..120 {
            if !animating.load(Ordering::Relaxed) { break; }
            set_tray_icon(&app_handle, frames[frame as usize % frames.len()]);
            frame += 1;
            let secs = frame / 4;
            set_tray_status(&app_handle, &format!("Starting server… {}s", secs));

            tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;

            if frame.is_multiple_of(4) {
                let status = mgr.check_health().await;
                if status.running {
                    animating.store(false, Ordering::Relaxed);
                    if status.model.is_empty() {
                        set_tray_status(&app_handle, "Server started");
                        set_tray_icon(&app_handle, app_handle.try_state::<AppState>()
                            .map(|s| s.icons.on).unwrap_or(ICON_ON_PNG));
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    }
                    update_tray(&app_handle, true, &status.model);
                    info!("Server is up — opening dashboard");
                    open::that(&dashboard_url).ok();
                    if let Some(state) = app_handle.try_state::<AppState>() {
                        refresh_model_menu(&app_handle, &state.manager);
                        auto_load_model(&app_handle, &state.manager, &state.default_model);
                    }
                    return;
                }
            }
        }
        animating.store(false, Ordering::Relaxed);
        update_tray(&app_handle, false, "");
        warn!("Server didn't come up in 30s");
    });
}

pub fn run() {
    env_logger::init();

    let cfg = AppConfig::load().expect("Failed to load config");
    info!("Config loaded from {:?}", AppConfig::config_path());

    let manager = Arc::new(ServerManager::new(
        &cfg.server_type,
        &cfg.api_url,
        &cfg.api_key,
        &cfg.service_label,
        &cfg.plist_path,
    ));
    let dashboard_url = cfg.dashboard_url.clone();
    let default_model = cfg.default_model.clone();
    let server_type = cfg.server_type.clone();
    let icons = if cfg.server_type == "llamacpp" { ICONSET_CPP } else { ICONSET_OMLX };

    let manager_for_setup = manager.clone();
    let animating = Arc::new(AtomicBool::new(false));
    let switching = Arc::new(AtomicBool::new(false));

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![get_config, save_config, close_settings])
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(move |app| {
            let status_item = MenuItemBuilder::with_id("status", "○  Stopped")
                .enabled(false)
                .build(app)?;

            app.manage(AppState {
                manager: manager_for_setup.clone(),
                status_item: status_item.clone(),
                animating: animating.clone(),
                switching: switching.clone(),
                dashboard_url: dashboard_url.clone(),
                default_model: default_model.clone(),
                icons,
            });

            let local = manager_for_setup.is_local;
            let start_item = MenuItemBuilder::with_id("start", "Start Server").enabled(local).build(app)?;
            let stop_item = MenuItemBuilder::with_id("stop", "Stop Server").enabled(local).build(app)?;
            let restart_item = MenuItemBuilder::with_id("restart", "Restart Server").enabled(local).build(app)?;

            let model_submenu = SubmenuBuilder::with_id(app, "models", "Models")
                .item(&MenuItemBuilder::with_id("model:loading", "Loading…").enabled(false).build(app)?)
                .build()?;

            let backend_submenu = build_backend_submenu(app.handle(), &server_type)?;

            let autostart_on = app.state::<AutoLaunchManager>().is_enabled().unwrap_or(false);
            let login_item = CheckMenuItemBuilder::with_id("login", "Launch at Login")
                .checked(autostart_on)
                .build(app)?;

            let prefs_item = MenuItemBuilder::with_id("prefs", "Preferences…").build(app)?;
            let quit_item = MenuItemBuilder::with_id("quit", "Quit").build(app)?;

            let menu = MenuBuilder::new(app)
                .item(&status_item)
                .separator()
                .item(&start_item)
                .item(&stop_item)
                .item(&restart_item)
                .separator()
                .item(&backend_submenu)
                .item(&model_submenu)
                .separator()
                .item(&login_item)
                .item(&prefs_item)
                .separator()
                .item(&quit_item)
                .build()?;

            let icon = png_to_tauri_image(icons.off);

            let _tray = TrayIconBuilder::with_id("main")
                .icon(icon)
                .icon_as_template(true)
                .tooltip("LLM Menubar")
                .menu(&menu)
                .on_menu_event(move |app_handle, event| {
                    let state = app_handle.state::<AppState>();
                    let mgr = &state.manager;
                    match event.id().as_ref() {
                        "start" => {
                            if let Err(e) = mgr.start() {
                                warn!("Start failed: {}", e);
                            } else {
                                start_and_animate(
                                    app_handle.clone(),
                                    mgr.clone(),
                                    state.animating.clone(),
                                    state.dashboard_url.clone(),
                                );
                            }
                        }
                        "stop" => {
                            state.animating.store(false, Ordering::Relaxed);
                            let mgr2 = mgr.clone();
                            let ah = app_handle.clone();
                            let done = Arc::new(AtomicBool::new(false));
                            let done2 = done.clone();
                            tauri::async_runtime::spawn(async move { mgr2.stop().ok(); done2.store(true, Ordering::Relaxed); });
                            tauri::async_runtime::spawn(async move {
                                let mut secs = 0u32;
                                loop {
                                    set_tray_status(&ah, &format!("Stopping server… {}s", secs));
                                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                                    secs += 1;
                                    if done.load(Ordering::Relaxed) { break; }
                                }
                                set_tray_status(&ah, "Server stopped");
                                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                                update_tray(&ah, false, "");
                            });
                        }
                        "restart" => {
                            if let Err(e) = mgr.restart() {
                                warn!("Restart failed: {}", e);
                            } else {
                                start_and_animate(
                                    app_handle.clone(),
                                    mgr.clone(),
                                    state.animating.clone(),
                                    state.dashboard_url.clone(),
                                );
                            }
                        }
                        "prefs" => {
                            open_settings(app_handle);
                        }
                        "login" => {
                            let alm = app_handle.state::<AutoLaunchManager>();
                            if alm.is_enabled().unwrap_or(false) {
                                alm.disable().ok();
                            } else {
                                alm.enable().ok();
                            }
                        }
                        id if id.starts_with("backend:") => {
                            let new_type = id.strip_prefix("backend:").unwrap();
                            if new_type != mgr.server_type {
                                let label = if new_type == "llamacpp" { ".CPP" } else { "oMLX" };
                                set_tray_status(app_handle, &format!("Switching to {}…", label));
                                if let Ok(mut cfg) = AppConfig::load() {
                                    cfg.server_type = new_type.to_string();
                                    match new_type {
                                        "llamacpp" => {
                                            cfg.api_url = "http://127.0.0.1:8080".into();
                                            cfg.service_label = "com.llama.server".into();
                                            cfg.plist_path = "~/Library/LaunchAgents/com.llama.server.plist".into();
                                            cfg.dashboard_url = "http://127.0.0.1:8080".into();
                                        }
                                        _ => {
                                            cfg.api_url = "http://127.0.0.1:8000/v1".into();
                                            cfg.service_label = "ai.omlx.server".into();
                                            cfg.plist_path = "~/Library/LaunchAgents/ai.omlx.server.plist".into();
                                            cfg.dashboard_url = "http://127.0.0.1:8000/admin".into();
                                        }
                                    }
                                    cfg.save().ok();
                                    mgr.stop().ok();
                                    app_handle.restart();
                                }
                            }
                        }
                        "quit" => {
                            mgr.stop().ok();
                            std::process::exit(0);
                        }
                        id if id.starts_with("model:") => {
                            let model_id = id.strip_prefix("model:").unwrap().to_string();
                            let mgr = mgr.clone();
                            let ah = app_handle.clone();
                            let anim = state.animating.clone();
                            let sw = state.switching.clone();
                            sw.store(true, Ordering::Relaxed);
                            anim.store(true, Ordering::Relaxed);

                            let frames = state.icons.loading;
                            let dashboard = state.dashboard_url.clone();
                            tauri::async_runtime::spawn(async move {
                                // Auto-start server if not running
                                let status = mgr.check_health().await;
                                if !status.running && mgr.is_local {
                                    set_tray_status(&ah, "Starting server… 0s");
                                    mgr.start().ok();
                                    let mut secs = 0u32;
                                    loop {
                                        set_tray_icon(&ah, frames[secs as usize % frames.len()]);
                                        secs += 1;
                                        set_tray_status(&ah, &format!("Starting server… {}s", secs));
                                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                                        if mgr.check_health().await.running { break; }
                                        if secs > 30 {
                                            anim.store(false, Ordering::Relaxed);
                                            sw.store(false, Ordering::Relaxed);
                                            set_tray_status(&ah, "Server failed to start");
                                            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                                            update_tray(&ah, false, "");
                                            return;
                                        }
                                    }
                                    // Let the server fully stabilize before model operations
                                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                                    refresh_model_menu(&ah, &mgr);
                                    open::that(&dashboard).ok();
                                }

                                // Unload current model if one is loaded
                                if let Ok(models) = mgr.list_models().await {
                                    for m in &models {
                                        if m.loaded && m.id != model_id {
                                            let label = m.label.clone();
                                            let mid = m.id.clone();
                                            let ah2 = ah.clone();
                                            let mgr2 = mgr.clone();
                                            let done = Arc::new(AtomicBool::new(false));
                                            let done2 = done.clone();
                                            tauri::async_runtime::spawn(async move {
                                                mgr2.unload_model(&mid).await.ok();
                                                done2.store(true, Ordering::Relaxed);
                                            });
                                            let mut secs = 0u32;
                                            loop {
                                                set_tray_status(&ah2, &format!("Unloading {}… {}s", label, secs));
                                                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                                                secs += 1;
                                                if done.load(Ordering::Relaxed) { break; }
                                            }
                                        }
                                    }
                                }

                                set_tray_status(&ah, "Loading model… 0s");
                                let mut loaded = false;
                                for attempt in 0..3 {
                                    if attempt > 0 {
                                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                                    }
                                    match mgr.load_model(&model_id).await {
                                        Ok(_) => { loaded = true; break; }
                                        Err(e) => warn!("Load attempt {}: {}", attempt + 1, e),
                                    }
                                }
                                if !loaded {
                                    anim.store(false, Ordering::Relaxed);
                                    sw.store(false, Ordering::Relaxed);
                                    set_tray_status(&ah, "Load failed");
                                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                                    let status = mgr.check_health().await;
                                    update_tray(&ah, status.running, &status.model);
                                    return;
                                }
                                let mut frame = 0u32;
                                for _ in 0..240 {
                                    set_tray_icon(&ah, frames[frame as usize % frames.len()]);
                                    frame += 1;
                                    let secs = frame / 4;
                                    set_tray_status(&ah, &format!("Loading model… {}s", secs));
                                    tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
                                    if frame.is_multiple_of(4) {
                                        let status = mgr.check_health().await;
                                        if !status.model.is_empty() {
                                            anim.store(false, Ordering::Relaxed);
                                            sw.store(false, Ordering::Relaxed);
                                            save_last_model(&model_id);
                                            update_tray(&ah, status.running, &status.model);
                                            refresh_model_menu(&ah, &mgr);
                                            return;
                                        }
                                    }
                                }
                                anim.store(false, Ordering::Relaxed);
                                sw.store(false, Ordering::Relaxed);
                                let status = mgr.check_health().await;
                                update_tray(&ah, status.running, &status.model);
                                refresh_model_menu(&ah, &mgr);
                            });
                        }
                        _ => {}
                    }
                })
                .build(app)?;

            let app_handle = app.handle().clone();
            let mgr = manager_for_setup.clone();
            tauri::async_runtime::spawn({
                let ah = app_handle.clone();
                let m = mgr.clone();
                async move {
                    if m.is_local {
                        set_tray_status(&ah, "Checking server…");
                    }
                    let status = m.check_health().await;
                    info!("Initial health: running={}, model={:?}", status.running, status.model);
                    if !status.running && m.is_local {
                        m.start().ok();
                        if let Some(state) = ah.try_state::<AppState>() {
                            start_and_animate(
                                ah.clone(),
                                m.clone(),
                                state.animating.clone(),
                                state.dashboard_url.clone(),
                            );
                        }
                        return;
                    }
                    update_tray(&ah, status.running, &status.model);
                    if status.running {
                        refresh_model_menu(&ah, &m);
                        if let Some(state) = ah.try_state::<AppState>() {
                            auto_load_model(&ah, &m, &state.default_model);
                        }
                    }
                }
            });

            tauri::async_runtime::spawn(async move {
                let mut tick = 0u32;
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    if let Some(state) = app_handle.try_state::<AppState>() {
                        if state.switching.load(Ordering::Relaxed) { continue; }
                    }
                    let status = mgr.check_health().await;
                    update_tray(&app_handle, status.running, &status.model);
                    tick += 1;
                    if status.running && tick.is_multiple_of(6) {
                        refresh_model_menu(&app_handle, &mgr);
                    }
                }
            });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error building tauri application")
        .run(|_app_handle, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                api.prevent_exit();
            }
        });
}
