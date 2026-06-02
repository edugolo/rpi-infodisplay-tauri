use crate::api;
use crate::config::AppConfig;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{self, Duration};

/// REST fallback poller — polls for commands and config when Socket.IO is unavailable.
pub struct Poller {
    config: Arc<RwLock<AppConfig>>,
    device_id: String,
    private_key_pem: String,
    on_commands: Box<dyn Fn(Vec<Value>) + Send + Sync>,
    on_config: Box<dyn Fn(Value) + Send + Sync>,
    interval: Duration,
}

impl Poller {
    pub fn new(
        config: Arc<RwLock<AppConfig>>,
        device_id: String,
        private_key_pem: String,
        on_commands: impl Fn(Vec<Value>) + Send + Sync + 'static,
        on_config: impl Fn(Value) + Send + Sync + 'static,
        interval_secs: u64,
    ) -> Self {
        Self {
            config,
            device_id,
            private_key_pem,
            on_commands: Box::new(on_commands),
            on_config: Box::new(on_config),
            interval: Duration::from_secs(interval_secs),
        }
    }

    pub async fn run(mut self) {
        let mut interval = time::interval(self.interval);
        // First tick is immediate
        interval.tick().await;

        loop {
            interval.tick().await;
            if let Err(e) = self.do_poll().await {
                log::error!("[poller] Poll failed: {}", e);
            }
        }
    }

    async fn do_poll(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let controller = {
            self.config.read().await.controller.clone()
        };

        let result = api::poll(&controller, &self.device_id, &self.private_key_pem).await?;

        if let Some(config) = result.get("config") {
            if !config.is_null() {
                (self.on_config)(config.clone());
            }
        }

        if let Some(commands) = result.get("pendingCommands") {
            if let Some(cmds) = commands.as_array() {
                if !cmds.is_empty() {
                    (self.on_commands)(cmds.clone());
                }
            }
        }

        Ok(())
    }
}
