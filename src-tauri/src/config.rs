use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub controller: String,
    pub display_type: Option<String>,
    pub url: Option<String>,
    pub fullscreen: Option<bool>,
    pub frame: Option<bool>,
    pub zoom_factor: Option<f64>,
    pub refresh_cron_expression: Option<String>,
    pub display_schedule: Option<DisplaySchedule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplaySchedule {
    /// Whether the schedule is active
    #[serde(default)]
    pub enabled: bool,
    /// Time to turn the display on (HH:MM format, 24h)
    pub on: Option<String>,
    /// Time to turn the display off (HH:MM format, 24h)
    pub off: Option<String>,
    /// Days of week to apply the schedule (e.g. ["mon", "tue", "wed", "thu", "fri"])
    /// If empty or absent, applies every day.
    #[serde(default)]
    pub days: Vec<String>,
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let raw = fs::read_to_string(path)?;
        let config: AppConfig = serde_json::from_str(&raw)?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }

    /// Validate required fields for controller connection
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        if self.controller.is_empty() {
            errors.push("controller URL is required".into());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Merge remote config updates into local config.
    /// Never overwrites `controller` to avoid breaking the active connection.
    pub fn apply_remote(&mut self, remote: &serde_json::Value) -> bool {
        if remote.is_null() || !remote.is_object() {
            return false;
        }

        let mut changed = false;

        if let Some(obj) = remote.as_object() {
            for (key, value) in obj {
                // Never overwrite controller
                if key == "controller" {
                    continue;
                }

                // Apply known fields
                match key.as_str() {
                    "name" => {
                        if let Some(s) = value.as_str() {
                            if self.name != s {
                                self.name = s.to_string();
                                changed = true;
                            }
                        }
                    }
                    "location" => {
                        if let Some(s) = value.as_str() {
                            if self.location != s {
                                self.location = s.to_string();
                                changed = true;
                            }
                        }
                    }
                    "displayType" => {
                        let new_val = value.as_str().map(|s| s.to_string());
                        if self.display_type != new_val {
                            self.display_type = new_val;
                            changed = true;
                        }
                    }
                    "url" => {
                        let new_val = value.as_str().map(|s| s.to_string());
                        if self.url != new_val {
                            self.url = new_val;
                            changed = true;
                        }
                    }
                    "fullscreen" => {
                        if let Some(b) = value.as_bool() {
                            if self.fullscreen != Some(b) {
                                self.fullscreen = Some(b);
                                changed = true;
                            }
                        }
                    }
                    "frame" => {
                        if let Some(b) = value.as_bool() {
                            if self.frame != Some(b) {
                                self.frame = Some(b);
                                changed = true;
                            }
                        }
                    }
                    "zoomFactor" => {
                        if let Some(f) = value.as_f64() {
                            if self.zoom_factor != Some(f) {
                                self.zoom_factor = Some(f);
                                changed = true;
                            }
                        }
                    }
                    "refreshCronExpression" => {
                        let new_val = value.as_str().map(|s| s.to_string());
                        if self.refresh_cron_expression != new_val {
                            self.refresh_cron_expression = new_val;
                            changed = true;
                        }
                    }
                    "displaySchedule" => {
                        if let Ok(new_val) =
                            serde_json::from_value::<DisplaySchedule>(value.clone())
                        {
                            if self.display_schedule.as_ref() != Some(&new_val) {
                                self.display_schedule = Some(new_val);
                                changed = true;
                            }
                        }
                    }
                    _ => {} // ignore unknown fields
                }
            }
        }

        changed
    }
}
