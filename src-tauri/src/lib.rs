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
    dashboard_url: String,
    default_model: String,
}

fn set_tray_icon(app: &AppHandle, icon_bytes: &[u8]) {
    if let Some(tray) = app.tray_by_id("main") {
        let img = png_to_tauri_image(icon_bytes);
        tray.set_icon(Some(img)).ok();
    }
}

fn refresh_model_menu(app: &AppHandle, mgr: &Arc<ServerManager>) {
    let app = app.clone();
    let mgr = mgr.clone();
    tauri::async_runtime::spawn(async move {
        let models = match mgr.list_models().await {
            Ok(m) => m,
            Err(e) => {
                warn!("Failed to list models: {}", e);
                return;
            }
        };

        let state = app.state::<AppState>();

        let mut sub = SubmenuBuilder::with_id(&app, "models", "Models");
        for m in &models {
            let label = if m.loaded {
                format!("●  {}", m.id)
            } else {
                format!("     {}", m.id)
            };
            if let Ok(item) = MenuItemBuilder::with_id(
                format!("model:{}", m.id),
                label,
            ).build(&app) {
                sub = sub.item(&item);
            }
        }
        let Ok(model_submenu) = sub.build() else { return };

        let start_item = MenuItemBuilder::with_id("start", "Start").build(&app).unwrap();
        let stop_item = MenuItemBuilder::with_id("stop", "Stop").build(&app).unwrap();
        let restart_item = MenuItemBuilder::with_id("restart", "Restart").build(&app).unwrap();

        let autostart_on = app.state::<AutoLaunchManager>().is_enabled().unwrap_or(false);
        let login_item = CheckMenuItemBuilder::with_id("login", "Launch at Login")
            .checked(autostart_on)
            .build(&app).unwrap();

        let prefs_item = MenuItemBuilder::with_id("prefs", "Preferences…").build(&app).unwrap();
        let quit_item = MenuItemBuilder::with_id("quit", "Quit").build(&app).unwrap();

        let menu = MenuBuilder::new(&app)
            .item(&state.status_item)
            .separator()
            .item(&start_item)
            .item(&stop_item)
            .item(&restart_item)
            .separator()
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
    });
}

fn update_tray(app: &AppHandle, running: bool, model: &str) {
    let state = app.state::<AppState>();
    let mgr = &state.manager;

    if !state.animating.load(Ordering::Relaxed) {
        set_tray_icon(app, if running { ICON_ON_PNG } else { ICON_OFF_PNG });
    }

    if let Some(tray) = app.tray_by_id("main") {
        if running && !model.is_empty() {
            let prefix = mgr.display_prefix();
            if prefix.is_empty() {
                tray.set_title(Some(model)).ok();
            } else {
                tray.set_title(Some(&format!("{} {}", prefix, model))).ok();
            }
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
        } else {
            models.first().map(|m| m.id.clone())
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
        state.status_item.set_text("◌  Starting…".to_string()).ok();
    }
    if let Some(tray) = app_handle.tray_by_id("main") {
        tray.set_tooltip(Some(&format!("{}: starting…", name))).ok();
        tray.set_title(None::<&str>).ok();
    }

    tauri::async_runtime::spawn(async move {
        let mut frame = 0;
        for _ in 0..120 {
            if !animating.load(Ordering::Relaxed) { break; }
            set_tray_icon(&app_handle, LOADING_FRAMES[frame % LOADING_FRAMES.len()]);
            frame += 1;

            tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;

            if frame % 4 == 0 {
                let status = mgr.check_health().await;
                if status.running {
                    animating.store(false, Ordering::Relaxed);
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

    let manager_for_setup = manager.clone();
    let animating = Arc::new(AtomicBool::new(false));

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
                dashboard_url: dashboard_url.clone(),
                default_model: default_model.clone(),
            });

            let start_item = MenuItemBuilder::with_id("start", "Start").build(app)?;
            let stop_item = MenuItemBuilder::with_id("stop", "Stop").build(app)?;
            let restart_item = MenuItemBuilder::with_id("restart", "Restart").build(app)?;

            let model_submenu = SubmenuBuilder::with_id(app, "models", "Models")
                .item(&MenuItemBuilder::with_id("model:loading", "Loading…").enabled(false).build(app)?)
                .build()?;

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
                .item(&model_submenu)
                .separator()
                .item(&login_item)
                .item(&prefs_item)
                .separator()
                .item(&quit_item)
                .build()?;

            let icon = png_to_tauri_image(ICON_OFF_PNG);

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
                            if let Err(e) = mgr.stop() {
                                warn!("Stop failed: {}", e);
                            }
                            update_tray(app_handle, false, "");
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
                        "quit" => {
                            mgr.stop().ok();
                            std::process::exit(0);
                        }
                        id if id.starts_with("model:") => {
                            let model_id = id.strip_prefix("model:").unwrap().to_string();
                            let mgr = mgr.clone();
                            let ah = app_handle.clone();
                            tauri::async_runtime::spawn(async move {
                                if let Err(e) = mgr.load_model(&model_id).await {
                                    warn!("Failed to load model {}: {}", model_id, e);
                                } else {
                                    let status = mgr.check_health().await;
                                    update_tray(&ah, status.running, &status.model);
                                    refresh_model_menu(&ah, &mgr);
                                }
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
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    let status = m.check_health().await;
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
                    let status = mgr.check_health().await;
                    update_tray(&app_handle, status.running, &status.model);
                    tick += 1;
                    if status.running && tick % 6 == 0 {
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
