use crate::config::DisplaySchedule;
use chrono::{Datelike, Local, NaiveTime};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{self, Duration};

/// Current display power state tracked by the scheduler.
static DISPLAY_ON: AtomicBool = AtomicBool::new(true);

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Force the display on (e.g. on startup). Returns true if the state changed.
pub async fn power_on() -> bool {
    if DISPLAY_ON.load(Ordering::Relaxed) {
        return false;
    }
    do_power_on().await
}

/// Force the display off. Returns true if the state changed.
pub async fn power_off() -> bool {
    if !DISPLAY_ON.load(Ordering::Relaxed) {
        return false;
    }
    do_power_off().await
}

/// Returns whether the display is currently considered on.
pub fn is_display_on() -> bool {
    DISPLAY_ON.load(Ordering::Relaxed)
}

/// Spawn the display-schedule background task. Checks every 30 seconds whether
/// the display should be on or off according to the schedule in config.
pub fn spawn_scheduler(
    schedule: Arc<RwLock<Option<DisplaySchedule>>>,
) {
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(30));
        // First tick fires immediately — sets the correct initial state.
        interval.tick().await;

        loop {
            interval.tick().await;

            let sched = schedule.read().await;
            if let Some(ref s) = *sched {
                if !s.enabled {
                    continue;
                }

                let should_be_on = should_display_be_on(s);
                let currently_on = DISPLAY_ON.load(Ordering::Relaxed);

                if should_be_on && !currently_on {
                    drop(sched);
                    do_power_on().await;
                } else if !should_be_on && currently_on {
                    drop(sched);
                    do_power_off().await;
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Schedule evaluation
// ---------------------------------------------------------------------------

/// Determine whether the display *should* be on right now for the given schedule.
fn should_display_be_on(schedule: &DisplaySchedule) -> bool {
    let now = Local::now();

    // Check day-of-week filter
    if !schedule.days.is_empty() && !is_today_in_days(&schedule.days, &now) {
        // Outside scheduled days → always on (don't turn off on weekends by default)
        return true;
    }

    let on_time = schedule
        .on
        .as_deref()
        .and_then(|t| parse_hhmm(t));
    let off_time = schedule
        .off
        .as_deref()
        .and_then(|t| parse_hhmm(t));

    match (on_time, off_time) {
        (Some(on), Some(off)) => {
            let current = now.time();
            // Normal range, e.g. on=07:00, off=22:00
            if on <= off {
                current >= on && current < off
            } else {
                // Wraps midnight, e.g. on=22:00, off=06:00
                current >= on || current < off
            }
        }
        (Some(on), None) => {
            // Only "on" time specified: turn on from that time, never auto-off
            now.time() >= on
        }
        (None, Some(off)) => {
            // Only "off" time specified: turn off from that time, never auto-on
            now.time() < off
        }
        (None, None) => true, // No times configured → always on
    }
}

/// Parse "HH:MM" into a NaiveTime.
fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.len() != 2 {
        return None;
    }
    let hour: u32 = parts[0].parse().ok()?;
    let minute: u32 = parts[1].parse().ok()?;
    NaiveTime::from_hms_opt(hour, minute, 0)
}

/// Check if the current weekday matches the configured days list.
/// Days are lowercase three-letter abbreviations: "mon", "tue", …
fn is_today_in_days(days: &[String], now: &chrono::DateTime<Local>) -> bool {
    let today = match now.weekday().num_days_from_monday() {
        0 => "mon",
        1 => "tue",
        2 => "wed",
        3 => "thu",
        4 => "fri",
        5 => "sat",
        6 => "sun",
        _ => "mon",
    };
    days.iter().any(|d| d.to_lowercase() == today)
}

// ---------------------------------------------------------------------------
// Power control via wlr-randr
// ---------------------------------------------------------------------------
//
// wlr-randr works on any wlroots-based compositor (cage, sway, etc.).
// It disables/re-enables the GPU output, causing the TV/monitor to enter
// standby when there's no signal. This is the most reliable method for
// kiosk setups using the vc4-kms-v3d DRM driver (Pi 3/4/5).
//
// We look for wlr-randr in PATH. Both the scheduler and manual on/off
// commands use it.

/// Expected output name as reported by `wlr-randr` (cached after first detect).
static WLR_OUTPUT_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Discover the output name from `wlr-randr` (e.g. "HDMI-A-1").
async fn detect_output_name() -> Option<String> {
    let output = tokio::process::Command::new("wlr-randr")
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The first line is the output name (before the space), e.g. "HDMI-A-1 ..."
    stdout.lines().next().and_then(|line| {
        line.split_whitespace().next().map(|s| s.to_string())
    })
}

/// Run `wlr-randr --output <name> --on/--off` to toggle display power.
async fn wlr_set_power(on: bool) -> bool {
    let output_name = match WLR_OUTPUT_NAME.get() {
        Some(name) => name.clone(),
        None => {
            let name = detect_output_name().await;
            match name {
                Some(n) => {
                    let _ = WLR_OUTPUT_NAME.set(n.clone());
                    n
                }
                None => {
                    log::error!("[display] Could not detect wlr-randr output name (is wlr-randr installed?)");
                    return false;
                }
            }
        }
    };

    let cmd = if on { "--on" } else { "--off" };
    let result = tokio::process::Command::new("wlr-randr")
        .args(["--output", &output_name, cmd])
        .output()
        .await;

    match result {
        Ok(out) => {
            if out.status.success() {
                log::info!("[display/wlr] Output '{output_name}' turned {}", if on { "ON" } else { "OFF" });
                true
            } else {
                log::warn!(
                    "[display/wlr] wlr-randr failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                false
            }
        }
        Err(e) => {
            log::error!("[display/wlr] wlr-randr not available: {}", e);
            false
        }
    }
}

async fn do_power_on() -> bool {
    log::info!("[display] Powering ON display");
    if wlr_set_power(true).await {
        DISPLAY_ON.store(true, Ordering::Relaxed);
        return true;
    }
    log::error!("[display] Failed to power ON display");
    false
}

async fn do_power_off() -> bool {
    log::info!("[display] Powering OFF display");
    if wlr_set_power(false).await {
        DISPLAY_ON.store(false, Ordering::Relaxed);
        return true;
    }
    log::error!("[display] Failed to power OFF display");
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hhmm() {
        assert_eq!(parse_hhmm("07:00"), Some(NaiveTime::from_hms_opt(7, 0, 0).unwrap()));
        assert_eq!(parse_hhmm("22:30"), Some(NaiveTime::from_hms_opt(22, 30, 0).unwrap()));
        assert_eq!(parse_hhmm("invalid"), None);
        assert_eq!(parse_hhmm("25:00"), None);
    }

    #[test]
    fn test_should_display_be_on_no_times() {
        let schedule = DisplaySchedule {
            enabled: true,
            on: None,
            off: None,
            days: vec![],
        };
        assert!(should_display_be_on(&schedule));
    }
}
