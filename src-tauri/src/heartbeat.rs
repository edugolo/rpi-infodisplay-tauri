use crate::api;
use crate::config::AppConfig;
use crate::system_info;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{self, Duration};

/// Periodic heartbeat sender.
pub struct Heartbeat {
    config: Arc<RwLock<AppConfig>>,
    device_id: String,
    private_key_pem: String,
    get_status: Box<dyn Fn() -> Value + Send + Sync>,
    interval: Duration,
}

impl Heartbeat {
    pub fn new(
        config: Arc<RwLock<AppConfig>>,
        device_id: String,
        private_key_pem: String,
        get_status: impl Fn() -> Value + Send + Sync + 'static,
        interval_secs: u64,
    ) -> Self {
        Self {
            config,
            device_id,
            private_key_pem,
            get_status: Box::new(get_status),
            interval: Duration::from_secs(interval_secs),
        }
    }

    pub async fn run(self) {
        let mut interval = time::interval(self.interval);
        interval.tick().await; // first tick immediate

        loop {
            interval.tick().await;
            if let Err(e) = self.do_heartbeat().await {
                log::error!("[heartbeat] Failed: {}", e);
            }
        }
    }

    async fn do_heartbeat(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let controller = {
            self.config.read().await.controller.clone()
        };

        let status = (self.get_status)();
        let cfg = self.config.read().await;
        let data = serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "uptime": status.get("uptime").unwrap_or(&serde_json::json!(0)),
            "currentUrl": status.get("currentUrl"),
            "ip": system_info::get_current_ip(),
            "appVersion": env!("CARGO_PKG_VERSION"),
            "name": cfg.name,
            "location": cfg.location,
            "displaySchedule": cfg.display_schedule,
            "vacationMode": cfg.vacation_mode,
        });
        drop(cfg);

        api::heartbeat(&controller, &self.device_id, &self.private_key_pem, &data).await?;
        log::debug!("[heartbeat] Sent");
        Ok(())
    }
}
