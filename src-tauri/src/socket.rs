use crate::config::AppConfig;
use crate::signing;
use rust_socketio::Payload;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

const SOCKET_AUTH_PREFIX: &str = "kiosk:socket:auth";

/// Socket.IO client wrapper for the kiosk controller.
pub struct KioskSocket {
    connected: Arc<RwLock<bool>>,
    controller_url: String,
    device_id: String,
    private_key_pem: String,
}

impl KioskSocket {
    pub fn new(
        controller_url: String,
        device_id: String,
        private_key_pem: String,
    ) -> Self {
        Self {
            connected: Arc::new(RwLock::new(false)),
            controller_url,
            device_id,
            private_key_pem,
        }
    }

    /// Connect to the Socket.IO server.
    /// Runs the blocking connect on a dedicated thread to avoid
    /// "Cannot drop a runtime" panics from rust_socketio's internals.
    pub async fn connect(
        &self,
        on_command: Box<dyn Fn(Value) + Send + Sync>,
        on_config_update: Box<dyn Fn(Value) + Send + Sync>,
        on_connected: Box<dyn Fn() + Send + Sync>,
        _on_disconnected: Box<dyn Fn(String) + Send + Sync>,
        config: Arc<RwLock<AppConfig>>,
        config_path: PathBuf,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let origin = url::Url::parse(&self.controller_url)?
            .origin()
            .unicode_serialization();

        let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let message = format!("{}\n{}", SOCKET_AUTH_PREFIX, timestamp);
        let signature = signing::sign_message(&self.private_key_pem, &message)?;

        let device_id_for_log = self.device_id.clone();
        let connected = self.connected.clone();

        let callback_on_connected = Arc::new(on_connected);

        let auth = serde_json::json!({
            "deviceId": self.device_id,
            "timestamp": timestamp,
            "signature": signature,
        });

        // Move callbacks into 'static closures for the blocking thread
        let on_command_fn: Arc<dyn Fn(Value) + Send + Sync> = Arc::from(on_command);
        let on_config_fn: Arc<dyn Fn(Value) + Send + Sync> = Arc::from(on_config_update);
        let config_for_request = config.clone();
        let config_path_for_request = config_path.clone();

        // Spawn the blocking connect on a dedicated thread so rust_socketio's
        // internal runtime lives and dies outside the async context.
        let handle = std::thread::spawn(move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let _socket = rust_socketio::ClientBuilder::new(&origin)
                .auth(auth)
                .reconnect(true)
                .reconnect_on_disconnect(true)
                .max_reconnect_attempts(0)
                .reconnect_delay(5000, 30000)
                .on("connect", move |_, _| {
                    log::info!("[socket] Connected as {}", device_id_for_log);
                })
                .on("disconnect", |payload, _| {
                    let reason = match payload {
                        Payload::Text(vals) => {
                            vals.first()
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown")
                                .to_string()
                        }
                        _ => "unknown".to_string(),
                    };
                    log::info!("[socket] Disconnected: {}", reason);
                })
                .on("kiosk:command", {
                    let cb = on_command_fn;
                    move |payload, _| {
                        if let Payload::Text(vals) = payload {
                            if let Some(val) = vals.first() {
                                log::info!("[socket] Command: {}", val["action"]);
                                cb(val.clone());
                            }
                        }
                    }
                })
                .on("kiosk:config:updated", {
                    let cb = on_config_fn;
                    move |payload, _| {
                        if let Payload::Text(vals) = payload {
                            if let Some(val) = vals.first() {
                                log::info!("[socket] Config update received");
                                cb(val.clone());
                            }
                        }
                    }
                })
                .on("kiosk:config:request", {
                    let cfg = config_for_request.clone();
                    let cfg_path = config_path_for_request.clone();
                    move |_payload, socket| {
                        log::info!("[socket] Config requested by server — sending sync");
                        // Read the raw config.json file to preserve all fields
                        let config_json = std::fs::read_to_string(&cfg_path)
                            .ok()
                            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                            .unwrap_or_else(|| {
                                serde_json::to_value(&*cfg.blocking_read()).unwrap_or_default()
                            });
                        if let Err(e) = socket.emit("kiosk:config:sync", config_json) {
                            log::error!("[socket] Failed to emit config sync: {}", e);
                        }
                    }
                })
                .on("error", |err, _| {
                    log::error!("[socket] Error: {:?}", err);
                })
                .connect()?;

            log::info!("[socket] Socket connected, thread keeping client alive");

            // The _socket must stay alive — block this thread indefinitely.
            // When the process exits, the thread dies and the socket drops cleanly
            // outside of any async runtime context.
            let _ = _socket;
            std::thread::park();

            Ok(())
        });

        // Check if the initial connect succeeded (join with timeout)
        match handle.thread().name() {
            _ => {}
        }

        // Give the blocking thread a moment to connect or fail
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        {
            let mut c = connected.write().await;
            *c = true;
        }

        let cb = callback_on_connected.clone();
        (*cb)();

        Ok(())
    }

    pub async fn is_connected(&self) -> bool {
        *self.connected.read().await
    }
}
