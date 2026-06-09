use crate::config::AppConfig;
use crate::signing;
use rust_socketio::Payload;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;
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

    /// Connect to the Socket.IO server with automatic reconnection.
    /// Runs the blocking connect on a dedicated thread and retries
    /// on connection failure or unexpected disconnect.
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

        let connected = self.connected.clone();
        let device_id = self.device_id.clone();
        let private_key_pem = self.private_key_pem.clone();

        // Move callbacks into 'static closures for the blocking thread
        let on_command_fn: Arc<dyn Fn(Value) + Send + Sync> = Arc::from(on_command);
        let on_config_fn: Arc<dyn Fn(Value) + Send + Sync> = Arc::from(on_config_update);
        let config_for_request = config.clone();
        let config_path_for_request = config_path.clone();
        let origin_for_thread = origin.clone();

        // Spawn a thread that loops connecting with fresh auth on each attempt.
        // The socket's internal reconnect handles transient drops after a successful
        // connect; this loop catches failures at the transport/connection level.
        std::thread::spawn(move || {
            loop {
                // Generate fresh auth (timestamp + signature) for this attempt
                let timestamp =
                    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                let message = format!("{}\n{}", SOCKET_AUTH_PREFIX, timestamp);
                let signature = match signing::sign_message(&private_key_pem, &message) {
                    Ok(s) => s,
                    Err(e) => {
                        log::error!("[socket] Auth signing failed: {} — retrying in 30s", e);
                        std::thread::sleep(Duration::from_secs(30));
                        continue;
                    }
                };

                let auth = serde_json::json!({
                    "deviceId": device_id,
                    "timestamp": timestamp,
                    "signature": signature,
                });

                let on_command = on_command_fn.clone();
                let on_config = on_config_fn.clone();
                let cfg_req = config_for_request.clone();
                let cfg_path_req = config_path_for_request.clone();
                let did = device_id.clone();

                // Channel so the error handler can wake the main thread
                let (error_tx, error_rx) = mpsc::channel::<()>();

                match rust_socketio::ClientBuilder::new(&origin_for_thread)
                    .auth(auth)
                    .reconnect(true)
                    .reconnect_on_disconnect(true)
                    .max_reconnect_attempts(0)
                    .reconnect_delay(5000, 30_000)
                    .on("connect", move |_, _| {
                        log::info!("[socket] Connected as {}", did);
                    })
                    .on("disconnect", |payload, _| {
                        let reason = match payload {
                            Payload::Text(vals) => vals
                                .first()
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown")
                                .to_string(),
                            _ => "unknown".to_string(),
                        };
                        log::info!("[socket] Disconnected: {}", reason);
                    })
                    .on("kiosk:command", {
                        let cb = on_command;
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
                        let cb = on_config;
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
                        let cfg = cfg_req;
                        let cfg_path = cfg_path_req;
                        move |_payload, socket| {
                            log::info!("[socket] Config requested by server — sending sync");
                            let config_json =
                                std::fs::read_to_string(&cfg_path)
                                    .ok()
                                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                                    .unwrap_or_else(|| {
                                        serde_json::to_value(&*cfg.blocking_read())
                                            .unwrap_or_default()
                                    });
                            if let Err(e) = socket.emit("kiosk:config:sync", config_json) {
                                log::error!("[socket] Failed to emit config sync: {}", e);
                            }
                        }
                    })
                    .on("error", {
                        let et = error_tx.clone();
                        move |err, _| {
                            log::error!("[socket] Error: {:?}", err);
                            // Signal the main thread to reconnect
                            let _ = et.send(());
                        }
                    })
                    .connect()
                {
                    Ok(_socket) => {
                        log::info!("[socket] Socket connected, thread keeping client alive");
                        // Block until an error occurs (error handler sends on the channel)
                        // or the socket drops (recv returns Err).
                        let reason = match error_rx.recv() {
                            Ok(()) => "error event received",
                            Err(_) => "sender dropped (socket disconnected)",
                        };
                        log::warn!("[socket] Socket reconnecting — {}", reason);
                        std::thread::sleep(Duration::from_secs(5));
                    }
                    Err(e) => {
                        log::error!(
                            "[socket] Connection failed: {} — retrying in 10s",
                            e
                        );
                        std::thread::sleep(Duration::from_secs(10));
                    }
                }
            }
        });

        // Give the thread a moment to connect or fail
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        {
            let mut c = connected.write().await;
            *c = true;
        }

        let cb = on_connected;
        (*cb)();

        Ok(())
    }

    pub async fn is_connected(&self) -> bool {
        *self.connected.read().await
    }
}
