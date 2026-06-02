use crate::api;
use serde_json::Value;
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::RwLock;

/// Command type with its ack function
type AckFn = Arc<dyn Fn(&str, &str, Option<&str>) + Send + Sync>;
type ScreenshotFn = Arc<dyn Fn(&str, &[u8]) + Send + Sync>;

/// Command dispatcher — executes commands received from the controller.
pub struct CommandDispatcher {
    config: Arc<RwLock<crate::config::AppConfig>>,
    device_id: String,
    private_key_pem: String,
    ack_fn: RwLock<Option<AckFn>>,
    screenshot_fn: RwLock<Option<ScreenshotFn>>,
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
        }
    }

    pub async fn set_ack_fn(&self, f: Option<AckFn>) {
        *self.ack_fn.write().await = f;
    }

    pub async fn set_screenshot_fn(&self, f: Option<ScreenshotFn>) {
        *self.screenshot_fn.write().await = f;
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

    async fn cmd_zoom(&self, factor: f64, app_handle: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(webview) = app_handle.get_webview_window("main") {
            webview.set_zoom(factor)?;
        }
        Ok(())
    }

    /// Try screenshot tools in order, verifying each produces a non-black image.
    /// spectacle: KDE Plasma (Wayland + X11)
    /// grim: wlroots-based Wayland compositors (Sway, Hyprland, etc.)
    /// gnome-screenshot: GNOME via D-Bus portal
    /// scrot: X11 only (reliable on Pi)
    /// import: ImageMagick fallback (X11)
    async fn try_screenshot_tool(&self, path: &str) -> Result<std::process::Output, Box<dyn std::error::Error + Send + Sync>> {
        // spectacle — KDE Plasma, works on Wayland
        let output = tokio::process::Command::new("spectacle")
            .args(["-b", "-n", "-o", path])
            .output()
            .await;
        if let Ok(out) = output {
            if out.status.success() && !Self::is_black_image(path).await {
                return Ok(out);
            }
        }

        // grim — Wayland native (wlroots compositors)
        let output = tokio::process::Command::new("grim")
            .arg(path)
            .output()
            .await;
        if let Ok(out) = output {
            if out.status.success() && !Self::is_black_image(path).await {
                return Ok(out);
            }
        }

        // gnome-screenshot — GNOME via XDG portal
        let output = tokio::process::Command::new("gnome-screenshot")
            .args(["-f", path])
            .output()
            .await;
        if let Ok(out) = output {
            if out.status.success() && !Self::is_black_image(path).await {
                return Ok(out);
            }
        }

        // scrot — reliable on Pi/X11 (produces black on Wayland)
        let output = tokio::process::Command::new("scrot")
            .args(["-z", "-o", path])
            .output()
            .await;
        if let Ok(out) = output {
            if out.status.success() && !Self::is_black_image(path).await {
                return Ok(out);
            }
        }

        // import (ImageMagick) — X11 fallback
        let output = tokio::process::Command::new("import")
            .args(["-window", "root", path])
            .output()
            .await;
        if let Ok(out) = output {
            if out.status.success() {
                return Ok(out);
            }
        }

        Err("No screenshot tool produced a valid image (tried: spectacle, grim, gnome-screenshot, scrot, import)".into())
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
