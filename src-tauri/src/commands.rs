use crate::api;
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Manager;
use tokio::sync::RwLock;

/// Set to true while an update is in progress to prevent concurrent updates.
static UPDATE_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Current app version, baked in at compile time.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// GitHub repository for release binary downloads.
const GITHUB_REPO: &str = "edugolo/rpi-infodisplay-tauri";

/// Check the latest release from GitHub and return the tag name.
pub async fn check_latest_version() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", GITHUB_REPO);
    let client = reqwest::Client::builder()
        .user_agent("rpi-infodisplay")
        .build()?;
    let resp = client.get(&url).send().await?;
    let data: serde_json::Value = resp.json().await?;
    let tag = data["tag_name"]
        .as_str()
        .ok_or("No tag_name in response")?
        .to_string();
    Ok(tag)
}

/// Download binary for the current architecture from a GitHub release.
async fn download_release(tag: &str, dest: &std::path::Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    let binary_name = format!("rpi-infodisplay-{}", arch);
    let url = format!(
        "https://github.com/{}/releases/download/{}/{}",
        GITHUB_REPO, tag, binary_name
    );

    log::info!("[update] Downloading {} from {}", binary_name, url);

    let client = reqwest::Client::builder()
        .user_agent("rpi-infodisplay")
        .build()?;
    let response = client.get(&url).send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("Download failed with HTTP {}", status).into());
    }

    let bytes = response.bytes().await?;
    tokio::fs::write(dest, &bytes).await?;

    log::info!("[update] Downloaded {} bytes to {:?}", bytes.len(), dest);
    Ok(())
}

/// Verify that a file looks like a valid aarch64 ELF binary.
fn verify_elf(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let data = std::fs::read(path)?;
    if data.len() < 64 {
        return Err("File too small to be an ELF binary".into());
    }
    // Check ELF magic: \x7f E L F
    if data[0..4] != [0x7f, 0x45, 0x4c, 0x46] {
        return Err("Not a valid ELF binary (bad magic)".into());
    }
    // Check 64-bit (EI_CLASS = 2)
    if data[4] != 2 {
        return Err("Not a 64-bit ELF".into());
    }
    // Check aarch64 (EM_AARCH64 = 183) on little-endian systems
    if cfg!(target_arch = "aarch64") && data[18] != 0xb7 {
        log::warn!("[update] Binary architecture byte: expected 0xb7 (AArch64), got 0x{:x}", data[18]);
    }
    Ok(())
}

/// Perform the actual binary swap and restart.
async fn apply_update(install_dir: &std::path::Path, downloaded: &std::path::Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::os::unix::fs::PermissionsExt;

    let target = install_dir.join("rpi-infodisplay");
    let backup = install_dir.join("rpi-infodisplay.bak");

    // Backup current binary
    if target.exists() {
        tokio::fs::copy(&target, &backup).await?;
        log::info!("[update] Backed up current binary to {:?}", backup);
    }

    // Remove existing binary (required when updating a running executable —
    // trying to copy over a running binary causes ETXTBUSY "Text file busy")
    let _ = tokio::fs::remove_file(&target).await;
    // Copy new binary into place
    tokio::fs::copy(downloaded, &target).await?;
    tokio::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).await?;
    log::info!("[update] Replaced binary at {:?}", target);

    // Trigger restart
    log::info!("[update] Restarting service...");
    let _ = tokio::process::Command::new("systemctl")
        .args(["restart", "rpi-infodisplay"])
        .spawn();

    Ok(())
}

/// Spawn a background task that checks GitHub for newer releases every 6 hours.
pub fn spawn_update_checker(install_dir: std::path::PathBuf) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(6 * 3600));
        // First check after 5 minutes (give the device time to boot)
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;

        loop {
            interval.tick().await;

            log::info!("[update] Periodic check: looking for newer version...");
            match check_latest_version().await {
                Ok(latest_tag) => {
                    let current = format!("v{}", APP_VERSION);
                    if latest_tag > current {
                        log::info!(
                            "[update] New version available: {} (current: {}). Auto-updating...",
                            latest_tag, current
                        );
                        let tmp = std::env::temp_dir().join("rpi-infodisplay-update");
                        if let Err(e) = download_release(&latest_tag, &tmp).await {
                            log::error!("[update] Download failed: {}", e);
                            continue;
                        }
                        if let Err(e) = verify_elf(&tmp) {
                            log::error!("[update] Verification failed: {}", e);
                            continue;
                        }
                        if let Err(e) = apply_update(&install_dir, &tmp).await {
                            log::error!("[update] Apply failed: {}", e);
                        }
                        // If apply succeeded, we don't return — the restart kills us.
                    } else {
                        log::debug!("[update] Already up-to-date ({} == {})", current, latest_tag);
                    }
                }
                Err(e) => {
                    log::error!("[update] Failed to check latest version: {}", e);
                }
            }
        }
    });
}

/// Command type with its ack function
type AckFn = Arc<dyn Fn(&str, &str, Option<&str>) + Send + Sync>;
type ScreenshotFn = Arc<dyn Fn(&str, &[u8]) + Send + Sync>;
type InfoFn = Arc<dyn Fn(&serde_json::Value) + Send + Sync>;

/// Command dispatcher — executes commands received from the controller.
pub struct CommandDispatcher {
    config: Arc<RwLock<crate::config::AppConfig>>,
    device_id: String,
    private_key_pem: String,
    ack_fn: RwLock<Option<AckFn>>,
    screenshot_fn: RwLock<Option<ScreenshotFn>>,
    info_fn: RwLock<Option<InfoFn>>,
}

impl CommandDispatcher {
    pub fn new(
        config: Arc<RwLock<crate::config::AppConfig>>,
        device_id: String,
        private_key_pem: String,
    ) -> Self {
        Self {
            config,
            device_id,
            private_key_pem,
            ack_fn: RwLock::new(None),
            screenshot_fn: RwLock::new(None),
            info_fn: RwLock::new(None),
        }
    }

    pub async fn set_ack_fn(&self, f: Option<AckFn>) {
        *self.ack_fn.write().await = f;
    }

    pub async fn set_screenshot_fn(&self, f: Option<ScreenshotFn>) {
        *self.screenshot_fn.write().await = f;
    }

    pub async fn set_info_fn(&self, f: Option<InfoFn>) {
        *self.info_fn.write().await = f;
    }

    /// Dispatch a batch of commands
    pub async fn dispatch(&self, commands: Vec<Value>, app_handle: &tauri::AppHandle) {
        for command in commands {
            self.execute(command, app_handle).await;
        }
    }

    async fn execute(&self, command: Value, app_handle: &tauri::AppHandle) {
        let command_id = command["id"].as_str().unwrap_or("unknown").to_string();
        let action = command["action"].as_str().unwrap_or("").to_string();
        let payload = command.get("payload").cloned().unwrap_or(serde_json::json!({}));

        log::info!("[commands] Executing: {} ({})", action, command_id);

        // Ack as "acknowledged"
        self.ack(&command_id, "acknowledged", None).await;

        // Execute the command
        let result = match action.as_str() {
            "refresh" => self.cmd_refresh(app_handle).await,
            "navigate" => self.cmd_navigate(&payload, app_handle).await,
            "screenshot" => self.cmd_screenshot(&command_id, app_handle).await,
            "identify" => self.cmd_identify(app_handle).await,
            "reboot" => self.cmd_reboot().await,
            "os-update" => self.cmd_os_update().await,
            "info" => self.cmd_info(app_handle).await,
            "update" => self.cmd_update(&payload).await,
            "display-substitution" | "display-announcement" => {
                self.cmd_navigate(&payload, app_handle).await
            }
            "zoom" => {
                if let Some(factor) = payload.get("zoomFactor").and_then(|v| v.as_f64()) {
                    self.cmd_zoom(factor, app_handle).await
                } else {
                    Err("Missing zoomFactor".into())
                }
            }
            _ => {
                log::warn!("[commands] Unknown action: {}", action);
                Err(format!("Unknown action: {}", action).into())
            }
        };

        // Ack with result
        match result {
            Ok(()) => self.ack(&command_id, "completed", None).await,
            Err(e) => {
                log::error!("[commands] Command {} failed: {}", command_id, e);
                self.ack(&command_id, "failed", Some(&e.to_string())).await;
            }
        }
    }

    async fn ack(&self, command_id: &str, status: &str, error_message: Option<&str>) {
        // Try socket ack first
        if let Some(ack_fn) = self.ack_fn.read().await.as_ref() {
            ack_fn(command_id, status, error_message);
            return;
        }

        // Fall back to REST ack
        let controller = self.config.read().await.controller.clone();
        if let Err(e) = api::ack_command(
            &controller,
            &self.device_id,
            &self.private_key_pem,
            command_id,
            status,
            error_message,
        )
        .await
        {
            log::error!("[commands] REST ack failed for {}: {}", command_id, e);
        }
    }

    async fn cmd_refresh(&self, app_handle: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(webview) = app_handle.get_webview_window("main") {
            webview.eval("location.reload()")?;
        }
        Ok(())
    }

    async fn cmd_navigate(&self, payload: &Value, app_handle: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(url) = payload.get("url").and_then(|v| v.as_str()) {
            if let Some(webview) = app_handle.get_webview_window("main") {
                let tauri_url = tauri::Url::parse(url)?;
                let _ = webview.navigate(tauri_url);
            }
        }
        Ok(())
    }

    async fn cmd_screenshot(&self, command_id: &str, _app_handle: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let path = "/tmp/kiosk-screenshot.png";

        // Try tools in order: gnome-screenshot first (works on Wayland + X11),
        // then scrot (Pi/X11), then import (ImageMagick)
        let result = self.try_screenshot_tool(path).await?;

        if !result.status.success() {
            return Err(format!("screenshot failed: {}", String::from_utf8_lossy(&result.stderr)).into());
        }

        let png_data = tokio::fs::read("/tmp/kiosk-screenshot.png").await?;
        log::info!("[commands] Screenshot captured: {} bytes", png_data.len());

        // Try socket screenshot first
        if let Some(screenshot_fn) = self.screenshot_fn.read().await.as_ref() {
            screenshot_fn(command_id, &png_data);
            return Ok(());
        }

        // Fall back to REST upload
        let controller = self.config.read().await.controller.clone();
        api::upload_screenshot(
            &controller,
            &self.device_id,
            &self.private_key_pem,
            command_id,
            &png_data,
        )
        .await?;

        Ok(())
    }

    async fn cmd_info(&self, app_handle: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let config = self.config.read().await;
        let current_url = app_handle
            .get_webview_window("main")
            .and_then(|w| w.url().ok())
            .map(|u| u.to_string())
            .unwrap_or_default();

        let info = serde_json::json!({
            "deviceId": self.device_id,
            "appVersion": env!("CARGO_PKG_VERSION"),
            "ip": crate::system_info::get_current_ip(),
            "uptime": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            "currentUrl": current_url,
            "config": &*config,
        });
        drop(config);

        log::info!("[commands] Device info requested via command");

        // Emit via Socket.IO if connected
        if let Some(info_fn) = self.info_fn.read().await.as_ref() {
            info_fn(&info);
        }

        Ok(())
    }

    async fn cmd_identify(&self, app_handle: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let handle = app_handle.clone();
        tokio::spawn(async move {
            if let Some(info_win) = handle.get_webview_window("info") {
                let _ = info_win.show();
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                let _ = info_win.hide();
            }
        });
        Ok(())
    }

    async fn cmd_reboot(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tokio::process::Command::new("sudo")
            .arg("reboot")
            .spawn()?;
        Ok(())
    }

    async fn cmd_os_update(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tokio::process::Command::new("sudo")
            .args(["apt", "update"])
            .spawn()?;
        Ok(())
    }

    async fn cmd_update(&self, payload: &Value) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if UPDATE_IN_PROGRESS.swap(true, Ordering::Relaxed) {
            return Err("Update already in progress".into());
        }

        let version = payload
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("latest");
        let tag = if version == "latest" {
            check_latest_version().await?
        } else {
            format!("v{}", version.trim_start_matches('v'))
        };

        log::info!("[update] Remote update triggered: version={}", tag);

        let tmp = std::env::temp_dir().join("rpi-infodisplay-update");
        download_release(&tag, &tmp).await?;
        verify_elf(&tmp)?;
        apply_update(&std::path::PathBuf::from("/opt/rpi-infodisplay"), &tmp).await?;

        // apply_update triggers a systemctl restart — we won't return
        Ok(())
    }

    async fn cmd_zoom(&self, factor: f64, app_handle: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(webview) = app_handle.get_webview_window("main") {
            webview.set_zoom(factor)?;
        }
        Ok(())
    }

    /// Take a screenshot using grim (Wayland native, wlroots compositors).
    async fn try_screenshot_tool(&self, path: &str) -> Result<std::process::Output, Box<dyn std::error::Error + Send + Sync>> {
        let output = tokio::process::Command::new("grim")
            .arg(path)
            .output()
            .await?;

        if !output.status.success() {
            return Err(format!("grim failed: {}", String::from_utf8_lossy(&output.stderr)).into());
        }

        // Verify the image is not all-black
        if Self::is_black_image(path).await {
            return Err("grim produced a black image".into());
        }

        Ok(output)
    }

    /// Check if a captured PNG is all-black (some tools succeed but produce
    /// black output when they can't actually capture, e.g. scrot on Wayland).
    async fn is_black_image(path: &str) -> bool {
        match tokio::fs::read(path).await {
            Ok(data) => {
                if data.len() < 100 {
                    return true;
                }
                // Sample bytes from the middle — compressed black PNGs are very uniform
                let start = data.len() / 3;
                let end = (start + 1024).min(data.len());
                let non_zero = data[start..end].iter().filter(|&&b| b != 0).count();
                non_zero < 10
            }
            Err(_) => true,
        }
    }
}
