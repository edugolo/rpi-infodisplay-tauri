use rpi_infodisplay::api;
use rpi_infodisplay::commands::CommandDispatcher;
use rpi_infodisplay::config::AppConfig;
use rpi_infodisplay::display;
use rpi_infodisplay::heartbeat::Heartbeat;
use rpi_infodisplay::keys;
use rpi_infodisplay::poller::Poller;
use rpi_infodisplay::socket::KioskSocket;
use rpi_infodisplay::system_info;

use std::path::PathBuf;
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::RwLock;

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    log::info!("rpi-infodisplay v{} starting", env!("CARGO_PKG_VERSION"));

    // Load config
    let config_path = PathBuf::from("./config.json");
    let app_config = AppConfig::load(&config_path).unwrap_or_else(|e| {
        log::warn!("[config] Failed to load config.json: {}", e);
        AppConfig {
            name: String::new(),
            location: String::new(),
            controller: String::new(),
            display_type: None,
            url: None,
            fullscreen: None,
            frame: None,
            zoom_factor: None,
            refresh_cron_expression: None,
            display_schedule: None,
            vacation_mode: None,
        }
    });

    let config = Arc::new(RwLock::new(app_config.clone()));

    // Gather system info
    let system_info = system_info::get_system_info().unwrap_or_else(|e| {
        log::error!("[system] Failed to get system info: {}", e);
        serde_json::json!({})
    });
    log::info!(
        "[system] Serial: {}, IP: {}",
        system_info["serial"].as_str().unwrap_or("unknown"),
        system_info["ip"].as_str().unwrap_or("unknown")
    );

    // Device info for overlay
    let device_info = Arc::new(RwLock::new(serde_json::json!({
        "config": app_config,
        "device": system_info,
    })));
    let device_info_json = serde_json::to_string(&*device_info.read().await).unwrap_or_default();

    // Setup Tauri
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(move |app| {
            let app_handle = app.handle().clone();

            // Create main kiosk window
            let fullscreen = app_config.fullscreen.unwrap_or(true);
            let decorations = app_config.frame.unwrap_or(false);
            let url = app_config.url.clone().unwrap_or_else(|| "https://edugo.be".to_string());
            let zoom_factor = app_config.zoom_factor.unwrap_or(1.0);

            // Detect screen size.
            // - On bare X (no WM): xrandr fallback for fullscreen sizing
            // - On Wayland (cage): skip xrandr, compositor handles fullscreen
            //   but we still set a safe initial size to avoid GTK assertions.
            let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();

            let screen_size = if fullscreen {
                if is_wayland {
                    // cage will resize to fill the output; provide a safe default
                    // to prevent gtk_window_resize assertion 'width > 0' errors.
                    None // let cage handle it
                } else {
                    std::process::Command::new("xrandr")
                        .arg("--current")
                        .env("DISPLAY", ":0")
                        .output()
                        .ok()
                        .and_then(|o| {
                            let out = String::from_utf8_lossy(&o.stdout);
                            for line in out.lines() {
                                if line.contains("current") {
                                    let dims: Vec<&str> = line.split_whitespace().collect();
                                    for i in 0..dims.len() {
                                        if dims[i] == "current" && i + 3 < dims.len() {
                                            let w = dims[i+1].trim_end_matches(',').parse::<u32>().ok()?;
                                            let h = dims[i+3].trim_end_matches(',').parse::<u32>().ok()?;
                                            if w > 0 && h > 0 {
                                                return Some((w, h));
                                            }
                                        }
                                    }
                                }
                            }
                            None
                        })
                }
            } else {
                None
            };
            if let Some((w, h)) = screen_size {
                log::info!("[kiosk] Detected screen {}x{} via xrandr", w, h);
            } else if fullscreen {
                log::info!("[kiosk] Fullscreen mode, compositor will handle sizing");
            }

            let mut builder = tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::External(url.parse().unwrap()))
                .fullscreen(fullscreen)
                .decorations(decorations)
                .always_on_top(true)
                .skip_taskbar(true)
                .visible(true)
                .background_color(tauri::webview::Color(0, 0, 0, 255))
                // Hide cursor on EVERY page load (cage/kiosk mode).
                // Using on_page_load instead of a post-build eval() ensures
                // the CSS survives the external URL navigation — the initial
                // eval() runs before the page loads and gets wiped when the
                // real document replaces the blank one.
                .on_page_load(|window, payload| {
                    if payload.event() == tauri::webview::PageLoadEvent::Finished {
                        let _ = window.eval(
                            "const s = document.createElement('style'); s.textContent = '* { cursor: none !important; }'; document.head.appendChild(s);"
                        );
                    }
                });

            if let Some((w, h)) = screen_size {
                builder = builder.inner_size(w as f64, h as f64).position(0.0, 0.0);
            }

            let main_window = builder.build().expect("Failed to create main window");

            // Set zoom after a short delay (WebKitGTK needs the page to load)
            let main_win_for_zoom = main_window.clone();
            let zf = zoom_factor;
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                if let Err(e) = main_win_for_zoom.set_zoom(zf) {
                    log::warn!("[kiosk] Failed to set zoom: {}", e);
                }
            });

            // Create info overlay window (shown initially with device info)
            let mut info_builder = tauri::WebviewWindowBuilder::new(
                app,
                "info",
                tauri::WebviewUrl::App("info-overlay/index.html".into()),
            )
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .visible(true)
            .focused(true)
            .fullscreen(true)
            .background_color(tauri::webview::Color(0, 0, 0, 255));

            if let Some((w, h)) = screen_size {
                info_builder = info_builder.inner_size(w as f64, h as f64).position(0.0, 0.0);
            }

            let info_window = info_builder.build().ok();

            // Inject device info into overlay after it loads
            if let Some(ref info_win) = info_window {
                let info_win_clone = info_win.clone();
                let info_json = device_info_json.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    let js = format!("window.__DEVICE_INFO__ = {};", info_json);
                    if let Err(e) = info_win_clone.eval(&js) {
                        log::warn!("[info-overlay] Failed to inject device info: {}", e);
                    }
                });
            }

            // Auto-hide info overlay after 10 seconds (like Electron version)
            if let Some(ref info_win) = info_window {
                let info_win_clone = info_win.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(10));
                    let _ = info_win_clone.hide();
                });
            }

            // Drop windows (they live in Tauri's window manager)
            drop(main_window);
            drop(info_window);

            // Initialise display power management (CEC probe + wake)
            display::init();

            // Start display schedule if configured
            // Init display schedule + controller connection in async context
            let schedule_for_display = Arc::new(RwLock::new(None));
            let vacation_mode = Arc::new(RwLock::new(app_config.vacation_mode.unwrap_or(false)));
            display::spawn_scheduler(schedule_for_display.clone(), vacation_mode.clone());

            // Start background update checker (checks GitHub every 6 hours)
            rpi_infodisplay::commands::spawn_update_checker(
                std::path::PathBuf::from("/opt/rpi-infodisplay"),
            );

            let config_for_connect = config.clone();
            let system_info_for_connect = system_info.clone();
            let app_handle_for_connect = app_handle.clone();
            let config_path_for_connect = config_path.clone();
            let schedule_for_remote = schedule_for_display.clone();
            let vacation_for_remote = vacation_mode.clone();

            tokio::spawn(async move {
                // Read initial schedule value now that we're async
                {
                    let cfg = config_for_connect.read().await;
                    let mut sh = schedule_for_remote.write().await;
                    *sh = cfg.display_schedule.clone();
                    if let Some(ref schedule) = cfg.display_schedule {
                        if schedule.enabled {
                            log::info!(
                                "[display] Schedule enabled: on={}, off={}, days={:?}",
                                schedule.on.as_deref().unwrap_or("N/A"),
                                schedule.off.as_deref().unwrap_or("N/A"),
                                schedule.days
                            );
                        }
                    }
                }
                connect_to_controller(
                    config_for_connect,
                    system_info_for_connect,
                    app_handle_for_connect,
                    config_path_for_connect,
                    device_info.clone(),
                    schedule_for_remote,
                    vacation_for_remote,
                )
                .await;
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Connect to the controller: announce device, wait for adoption, then start full operation.
async fn connect_to_controller(
    config: Arc<RwLock<AppConfig>>,
    system_info: serde_json::Value,
    app_handle: tauri::AppHandle,
    config_path: PathBuf,
    device_info: Arc<RwLock<serde_json::Value>>,
    schedule_handle: Arc<RwLock<Option<rpi_infodisplay::config::DisplaySchedule>>>,
    vacation_mode_handle: Arc<RwLock<bool>>,
) {
    let controller = config.read().await.controller.clone();
    if controller.is_empty() {
        log::info!("[controller] No controller URL configured, skipping connection");
        return;
    }

    loop {
        match try_connect(&config, &system_info, &app_handle, &config_path, &device_info, &schedule_handle, &vacation_mode_handle).await {
            Ok(()) => break,
            Err(e) => {
                log::error!("[controller] Connection failed: {}", e);
                log::info!("[controller] Retrying in 30 seconds...");
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
        }
    }
}

async fn try_connect(
    config: &Arc<RwLock<AppConfig>>,
    system_info: &serde_json::Value,
    app_handle: &tauri::AppHandle,
    config_path: &PathBuf,
    device_info: &Arc<RwLock<serde_json::Value>>,
    schedule_handle: &Arc<RwLock<Option<rpi_infodisplay::config::DisplaySchedule>>>,
    vacation_mode_handle: &Arc<RwLock<bool>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Get or create keys
    let (private_key_pem, public_key_pem) = keys::get_or_create_keys()?;
    log::info!("[controller] Keys ready");

    let controller_url;
    let serial;
    let mac;
    {
        let cfg = config.read().await;
        controller_url = cfg.controller.clone();
        serial = system_info["serial"].as_str().unwrap_or("unknown").to_string();
        mac = system_info["mac"].as_str().unwrap_or("unknown").to_string();
    }

    // Check for existing device ID
    let mut device_id = keys::load_device_id()?.unwrap_or_else(|| String::new());

    if device_id.is_empty() {
        // Announce device
        log::info!("[controller] Announcing device...");
        let config_val = serde_json::to_value(config.read().await.clone())?;
        let result = api::announce(
            &controller_url,
            &serial,
            &mac,
            &public_key_pem,
            &system_info["system"],
            &config_val,
        )
        .await?;

        device_id = result["id"].as_str().unwrap_or("").to_string();
        if device_id.is_empty() {
            return Err("Announce returned no device ID".into());
        }
        keys::save_device_id(&device_id)?;
        log::info!("[controller] Announced as {}, deviceId: {}", result["status"], device_id);
    } else {
        log::info!("[controller] Existing deviceId: {}", device_id);
    }

    // Poll to check current status
    let poll_result = api::poll(&controller_url, &device_id, &private_key_pem).await;

    match poll_result {
        Ok(result) => {
            // Apply server config snapshot
            if let Some(remote_config) = result.get("config") {
                apply_remote_config(config, remote_config, &config_path, app_handle, Some(schedule_handle), Some(vacation_mode_handle)).await;
            }

            let status = result["status"].as_str().unwrap_or("pending");
            if status == "pending" {
                log::info!("[controller] Device is pending adoption. Waiting...");
                wait_for_adoption(
                    config,
                    &controller_url,
                    &device_id,
                    &private_key_pem,
                    &config_path,
                    app_handle,
                    schedule_handle,
                    vacation_mode_handle,
                )
                .await?;
            }

            // Start full operation (after adoption or if already adopted)
            start_full_operation(
                config,
                &controller_url,
                &device_id,
                &private_key_pem,
                &config_path,
                app_handle,
                device_info,
                &system_info,
                schedule_handle,
                vacation_mode_handle,
            )
            .await?;
        }
        Err(e) => {
            log::warn!("[controller] Initial poll failed: {}, assuming pending adoption", e);
            wait_for_adoption(
                config,
                &controller_url,
                &device_id,
                &private_key_pem,
                &config_path,
                app_handle,
                schedule_handle,
                vacation_mode_handle,
            )
            .await?;

            // Now start full operation
            start_full_operation(
                config,
                &controller_url,
                &device_id,
                &private_key_pem,
                &config_path,
                app_handle,
                device_info,
                &system_info,
                schedule_handle,
                vacation_mode_handle,
            )
            .await?;
        }
    }

    Ok(())
}

/// Poll every 30s until the device is adopted.
async fn wait_for_adoption(
    config: &Arc<RwLock<AppConfig>>,
    controller_url: &str,
    device_id: &str,
    private_key_pem: &str,
    config_path: &PathBuf,
    app_handle: &tauri::AppHandle,
    schedule_handle: &Arc<RwLock<Option<rpi_infodisplay::config::DisplaySchedule>>>,
    vacation_mode_handle: &Arc<RwLock<bool>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;

        match api::poll(controller_url, device_id, private_key_pem).await {
            Ok(result) => {
                let status = result["status"].as_str().unwrap_or("pending");
                if status != "pending" {
                    log::info!("[controller] Device adopted! Status: {}", status);
                    if let Some(remote_config) = result.get("config") {
                        apply_remote_config(config, remote_config, config_path, app_handle, Some(schedule_handle), Some(vacation_mode_handle)).await;
                    }
                    return Ok(());
                }
            }
            Err(e) => {
                log::error!("[controller] Adoption poll failed: {}", e);
            }
        }
    }
}

/// Start full operation after adoption: Socket.IO + REST fallback
#[allow(clippy::too_many_arguments)]
async fn start_full_operation(
    config: &Arc<RwLock<AppConfig>>,
    controller_url: &str,
    device_id: &str,
    private_key_pem: &str,
    config_path: &PathBuf,
    app_handle: &tauri::AppHandle,
    _device_info: &Arc<RwLock<serde_json::Value>>,
    system_info: &serde_json::Value,
    schedule_handle: &Arc<RwLock<Option<rpi_infodisplay::config::DisplaySchedule>>>,
    vacation_mode_handle: &Arc<RwLock<bool>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    log::info!("[controller] Starting full operation");

    // Create command dispatcher
    let dispatcher = Arc::new(CommandDispatcher::new(
        config.clone(),
        device_id.to_string(),
        private_key_pem.to_string(),
    ));

    // Start REST fallback (poller + heartbeat) immediately
    let poller_config = config.clone();
    let poller_device_id = device_id.to_string();
    let poller_pk = private_key_pem.to_string();
    let poller_app = app_handle.clone();
    let poller_config_path = config_path.clone();
    let poller_dispatcher = dispatcher.clone();

    let on_commands = {
        let app = poller_app.clone();
        let disp = poller_dispatcher.clone();
        move |commands: Vec<serde_json::Value>| {
            let app = app.clone();
            let disp = disp.clone();
            tokio::spawn(async move {
                disp.dispatch(commands, &app).await;
            });
        }
    };

    let on_config = {
        let cfg = poller_config.clone();
        let path = poller_config_path.clone();
        let app = poller_app.clone();
        let sched = schedule_handle.clone();
        let vm = vacation_mode_handle.clone();
        move |remote_config: serde_json::Value| {
            let cfg = cfg.clone();
            let path = path.clone();
            let app = app.clone();
            let sched = sched.clone();
            let vm = vm.clone();
            tokio::spawn(async move {
                apply_remote_config(&cfg, &remote_config, &path, &app, Some(&sched), Some(&vm)).await;
            });
        }
    };

    let poller = Poller::new(
        poller_config.clone(),
        poller_device_id,
        poller_pk,
        on_commands,
        on_config,
        30,
    );

    // Heartbeat
    let hb_config = config.clone();
    let hb_device_id = device_id.to_string();
    let hb_pk = private_key_pem.to_string();
    let hb_app = app_handle.clone();

    let get_status = {
        let app = hb_app.clone();
        move || {
            let current_url = app.get_webview_window("main")
                .and_then(|w| w.url().ok())
                .map(|u| u.to_string())
                .unwrap_or_default();
            let uptime = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            serde_json::json!({
                "uptime": uptime,
                "currentUrl": current_url,
                "ip": rpi_infodisplay::system_info::get_current_ip(),
            })
        }
    };

    let heartbeat = Heartbeat::new(hb_config, hb_device_id, hb_pk, get_status, 60);

    // Start poller and heartbeat in background
    tokio::spawn(poller.run());
    tokio::spawn(heartbeat.run());

    // Try to connect via Socket.IO
    let socket = KioskSocket::new(
        controller_url.to_string(),
        device_id.to_string(),
        private_key_pem.to_string(),
    );

    // Get a handle to the tokio runtime so socket callbacks (which run on
    // a plain std::thread) can spawn async tasks.
    let rt_handle = tokio::runtime::Handle::current();

    let socket_on_command = {
        let app = app_handle.clone();
        let disp = dispatcher.clone();
        let handle = rt_handle.clone();
        move |cmd: serde_json::Value| {
            let app = app.clone();
            let disp = disp.clone();
            handle.spawn(async move {
                disp.dispatch(vec![cmd], &app).await;
            });
        }
    };

    let socket_on_config = {
        let cfg = config.clone();
        let path = config_path.clone();
        let app = app_handle.clone();
        let handle = rt_handle.clone();
        let sched = schedule_handle.clone();
        let vm = vacation_mode_handle.clone();
        move |remote_config: serde_json::Value| {
            let cfg = cfg.clone();
            let path = path.clone();
            let app = app.clone();
            let sched = sched.clone();
            let vm = vm.clone();
            handle.spawn(async move {
                apply_remote_config(&cfg, &remote_config, &path, &app, Some(&sched), Some(&vm)).await;
            });
        }
    };

    let socket_on_connected = {
        let _disp = dispatcher.clone();
        move || {
            log::info!("[controller] Socket connected — switching to real-time");
            // Note: In a full implementation, we'd stop the REST poller here
            // and switch the dispatcher's ack/screenshot functions to use the socket.
            // For now, both run in parallel (poller is idempotent).
        }
    };

    let socket_on_disconnected = {
        move |reason: String| {
            log::warn!("[controller] Socket disconnected: {} — REST fallback active", reason);
        }
    };

    match socket
        .connect(
            Box::new(socket_on_command),
            Box::new(socket_on_config),
            Box::new(socket_on_connected),
            Box::new(socket_on_disconnected),
            config.clone(),
            config_path.clone(),
        )
        .await
    {
        Ok(()) => log::info!("[controller] Socket.IO connected"),
        Err(e) => log::warn!("[controller] Socket.IO connection failed: {} — REST fallback active", e),
    }

    log::info!("[controller] Fully connected and running");

    // Keep the connection task alive
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}

/// Apply remote config update from the controller
async fn apply_remote_config(
    config: &Arc<RwLock<AppConfig>>,
    remote_config: &serde_json::Value,
    config_path: &PathBuf,
    app_handle: &tauri::AppHandle,
    schedule_handle: Option<&Arc<RwLock<Option<rpi_infodisplay::config::DisplaySchedule>>>>,
    vacation_mode_handle: Option<&Arc<RwLock<bool>>>,
) {
    let mut cfg = config.write().await;
    let changed = cfg.apply_remote(remote_config);

    if changed {
        log::info!("[controller] Remote config applied");

        // Update the display schedule handle so the scheduler picks it up
        if let Some(handle) = schedule_handle {
            let mut sh = handle.write().await;
            *sh = cfg.display_schedule.clone();
            if let Some(ref sched) = *sh {
                log::info!(
                    "[controller] Schedule updated: enabled={}, on={}, off={}, days={:?}",
                    sched.enabled,
                    sched.on.as_deref().unwrap_or("none"),
                    sched.off.as_deref().unwrap_or("none"),
                    sched.days
                );
            }
        }

        // Update vacation mode handle so the scheduler picks it up
        if let Some(handle) = vacation_mode_handle {
            let new_val = cfg.vacation_mode.unwrap_or(false);
            let mut vm = handle.write().await;
            if *vm != new_val {
                *vm = new_val;
                log::info!(
                    "[controller] Vacation mode: {}",
                    if new_val { "ON" } else { "OFF" }
                );
            }
        }

        // Apply immediate effects
        if let Some(webview) = app_handle.get_webview_window("main") {
            if let Some(zoom) = cfg.zoom_factor {
                if let Err(e) = webview.set_zoom(zoom) {
                    log::warn!("[controller] Failed to set zoom: {}", e);
                }
            }
            if let Some(url) = &cfg.url {
                if let Ok(tauri_url) = url.parse() {
                    let _ = webview.navigate(tauri_url);
                }
            }
        }

        // Persist to config.json
        if let Err(e) = cfg.save(config_path) {
            log::error!("[controller] Failed to save config: {}", e);
        }
    } else {
        log::debug!("[controller] Remote config — no changes detected");
    }
}
