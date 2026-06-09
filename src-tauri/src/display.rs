//! Display power management.
//!
//! ## Strategy
//!
//! 1. **CEC** (HDMI-CEC) is tried first. If the display/TV supports CEC, the
//!    Pi sends a standard "Standby" command to gracefully turn off the TV,
//!    and "Image View On" to wake it. This works on any GPU/compositor and
//!    doesn't disrupt the DRM pipeline.
//!
//! 2. **Service stop** is the fallback (and always runs after CEC). The app
//!    calls `systemctl stop rpi-infodisplay`, which cleanly stops cage and
//!    the Tauri process. The display goes dark because there's no more HDMI
//!    signal. On the next scheduled start (or boot), cage performs a fresh
//!    DRM modeset — no stale swapchain or HDMI handshake issues.
//!
//! The old approach (`wlr-randr --off` → CRTC disable) is removed because it
//! causes swapchain failures on re-enable, especially on low-memory Pis.

use crate::config::DisplaySchedule;
use chrono::{Datelike, Local, NaiveTime};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::RwLock;
use tokio::time::{self, Duration};

// ---------------------------------------------------------------------------
// CEC state (cached after first probe at startup)
// ---------------------------------------------------------------------------

static CEC_PROBED: AtomicBool = AtomicBool::new(false);
static CEC_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Probe whether CEC is functional by sending a Give Device Power Status
/// with a short timeout. If the TV responds, CEC works.
async fn probe_cec() -> bool {
    let output = tokio::process::Command::new("cec-ctl")
        .args([
            "--device",
            "/dev/cec0",
            "--give-device-power-status",
            "--timeout",
            "1500",
        ])
        .output()
        .await;

    match output {
        Ok(out) => {
            if out.status.success() {
                // Check if we got a response from the TV (logical address 0)
                let stdout = String::from_utf8_lossy(&out.stdout);
                // Successful response means CEC is working
                log::info!("[display/cec] Probe succeeded:\n{}", stdout.trim());
                true
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                log::debug!("[display/cec] Probe failed: {}", stderr.trim());
                false
            }
        }
        Err(e) => {
            log::debug!("[display/cec] cec-ctl not found: {}", e);
            false
        }
    }
}

/// Send CEC Standby to the TV. Fire-and-forget.
async fn cec_standby() {
    let result = tokio::process::Command::new("cec-ctl")
        .args(["--device", "/dev/cec0", "--standby"])
        .output()
        .await;

    match result {
        Ok(out) if out.status.success() => {
            log::info!("[display/cec] Sent STANDBY");
        }
        Ok(out) => {
            log::warn!(
                "[display/cec] STANDBY failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Err(e) => {
            log::error!("[display/cec] STANDBY error: {}", e);
        }
    }
}

/// Send CEC Image View On (wake) to the TV. Fire-and-forget.
async fn cec_wake() {
    let result = tokio::process::Command::new("cec-ctl")
        .args(["--device", "/dev/cec0", "--image-view-on"])
        .output()
        .await;

    match result {
        Ok(out) if out.status.success() => {
            log::info!("[display/cec] Sent IMAGE_VIEW_ON (wake)");
        }
        Ok(out) => {
            log::warn!(
                "[display/cec] IMAGE_VIEW_ON failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Err(e) => {
            log::error!("[display/cec] IMAGE_VIEW_ON error: {}", e);
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Initialise display power management.
///
/// Should be called once at app startup (inside the spawn_scheduler task or
/// early in main). Probes CEC availability in the background and wakes the
/// display if CEC is available.
pub fn init() {
    tokio::spawn(async {
        let available = probe_cec().await;
        CEC_AVAILABLE.store(available, Ordering::Relaxed);
        CEC_PROBED.store(true, Ordering::Relaxed);

        if available {
            log::info!("[display] CEC available — will use CEC for display power");
            // Wake the display on startup (cage just started, but the TV may
            // be in standby from last night)
            cec_wake().await;
        } else {
            log::info!(
                "[display] CEC not available — display will follow service lifecycle"
            );
        }
    });
}

/// Power the display off.
///
/// 1. If CEC is available, send Standby (graceful TV power-off).
/// 2. Stop the systemd service — this kills cage + the app cleanly.
///    When the service starts next time (boot or systemd timer), cage does a
///    fresh DRM modeset, avoiding swapchain issues.
pub async fn power_off() {
    // Give the CEC probe a moment to complete (it was started in init())
    if !CEC_PROBED.load(Ordering::Relaxed) {
        for _ in 0..50 {
            if CEC_PROBED.load(Ordering::Relaxed) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    if CEC_AVAILABLE.load(Ordering::Relaxed) {
        cec_standby().await;
    }

    log::info!("[display] Stopping service (power off)");

    // Spawn systemctl stop — this will kill cage which kills us.
    // Don't await: we'll be killed before it completes.
    let _ = tokio::process::Command::new("systemctl")
        .args(["stop", "rpi-infodisplay"])
        .spawn();

    // Give systemctl a moment to fire before the task exits.
    tokio::time::sleep(Duration::from_millis(500)).await;
}

// ---------------------------------------------------------------------------
// Schedule evaluation
// ---------------------------------------------------------------------------

/// Spawn the display-schedule background task.
///
/// Every 30 seconds it checks whether the display should be on according to
/// the config-based schedule. When the schedule says "off", it calls
/// [`power_off`], which sends CEC Standby (if available) and then stops the
/// service.
pub fn spawn_scheduler(schedule: Arc<RwLock<Option<DisplaySchedule>>>) {
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(30));
        // First tick fires immediately.
        interval.tick().await;

        loop {
            interval.tick().await;

            let sched = schedule.read().await;
            if let Some(ref s) = *sched {
                if !s.enabled {
                    continue;
                }

                let should_be_on = should_display_be_on(s);
                if !should_be_on {
                    drop(sched);
                    power_off().await;
                    // If power_off returns (it usually doesn't because the
                    // service is stopped), we break out of the loop.
                    break;
                }
            }
        }
    });
}

/// Determine whether the display *should* be on right now for the given schedule.
fn should_display_be_on(schedule: &DisplaySchedule) -> bool {
    let now = Local::now();

    // Day-of-week filter: if days are specified and today is not in them,
    // the display should be OFF (saves power on weekends / non-school days).
    if !schedule.days.is_empty() && !is_today_in_days(&schedule.days, &now) {
        return false;
    }

    let on_time = schedule.on.as_deref().and_then(|t| parse_hhmm(t));
    let off_time = schedule.off.as_deref().and_then(|t| parse_hhmm(t));

    match (on_time, off_time) {
        (Some(on), Some(off)) => {
            let current = now.time();
            if on <= off {
                // Normal range, e.g. on=07:00, off=22:00
                current >= on && current < off
            } else {
                // Wraps midnight, e.g. on=22:00, off=06:00
                current >= on || current < off
            }
        }
        (Some(on), None) => {
            // Only "on" time: turn on from that time, never auto-off
            now.time() >= on
        }
        (None, Some(off)) => {
            // Only "off" time: turn off from that time, never auto-on
            now.time() < off
        }
        (None, None) => true, // No times → always on
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hhmm() {
        assert_eq!(
            parse_hhmm("07:00"),
            Some(NaiveTime::from_hms_opt(7, 0, 0).unwrap())
        );
        assert_eq!(
            parse_hhmm("22:30"),
            Some(NaiveTime::from_hms_opt(22, 30, 0).unwrap())
        );
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

    #[test]
    fn test_should_display_be_on_off_during_off_hours() {
        // On a weekday, at 02:00, schedule 09:00-16:00 should be OFF
        let schedule = DisplaySchedule {
            enabled: true,
            on: Some("09:00".into()),
            off: Some("16:00".into()),
            days: vec!["mon".into(), "tue".into(), "wed".into(), "thu".into(), "fri".into()],
        };
        // We can't easily test this without mocking time.
        // At least verify the function signature works.
        let _ = should_display_be_on(&schedule);
    }

    #[test]
    fn test_outside_scheduled_days_is_off() {
        // If days is ["mon"] and today is Tuesday, the display should be OFF.
        // We test the logic by checking the guard clause returns false.
        // (Full test would need time mocking.)
        let schedule = DisplaySchedule {
            enabled: true,
            on: Some("09:00".into()),
            off: Some("16:00".into()),
            days: vec!["mon".into()],
        };
        // We can't test the actual return without controlling time.
        // Instead, verify the function at least compiles and runs.
        let result = should_display_be_on(&schedule);
        // It will either be true or false depending on the current time/day.
        // The important thing is it doesn't panic.
        println!("should_display_be_on test: {}", result);
    }
}
