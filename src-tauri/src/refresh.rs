//! Periodic page-refresh scheduler (cron-based).
//!
//! Driven by `refreshCronExpression` in config. Unlike a webview-side
//! `setInterval`, this runs in the Rust tokio runtime, so it keeps firing
//! regardless of whether the screen/TV is on, whether HDMI-CEC worked, or
//! whether the webview's JS timer is throttled. When the cron fires it
//! reloads the webview (`location.reload()`), which re-runs the page's
//! server `load` (so e.g. a `CURRENT_DATE` query drops yesterday's rows).

use chrono::{DateTime, Local};
use cron::Schedule;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::Duration;

/// Normalize a user-provided cron expression into the form the `cron` crate
/// expects. The crate requires a leading seconds field; standard 5-field
/// crontab (`min hour dom mon dow`) gets a `0 ` seconds prefix so users can
/// write midnight the familiar way: `"0 0 * * *"`.
fn normalize_expr(expr: &str) -> String {
	let trimmed = expr.trim();
	let field_count = trimmed.split_whitespace().count();
	if field_count == 5 {
		format!("0 {}", trimmed)
	} else {
		trimmed.to_string()
	}
}

/// Parse + validate, logging on error. Returns `None` if invalid.
fn parse_schedule(expr: &str) -> Option<Schedule> {
	let normalized = normalize_expr(expr);
	match Schedule::from_str(&normalized) {
		Ok(s) => Some(s),
		Err(e) => {
			log::warn!(
				"[refresh] Invalid cron expression {:?} (normalized {:?}): {}",
				expr,
				normalized,
				e
			);
			None
		}
	}
}

/// Spawn the refresh background task.
///
/// Every tick it (re)reads the cron expression from the shared handle, so the
/// controller can change `refreshCronExpression` live and it takes effect
/// without a restart. When the next fire time arrives it reloads the webview.
pub fn spawn_refresh_scheduler(
	cron_handle: Arc<RwLock<Option<String>>>,
	app_handle: tauri::AppHandle,
) {
	tokio::spawn(async move {
		let mut current_expr: Option<String> = None;
		let mut schedule: Option<Schedule> = None;
		let mut next_fire: Option<DateTime<Local>> = None;

		loop {
			let expr = cron_handle.read().await.clone();

			// (Re)parse if the configured expression changed.
			if expr != current_expr {
				current_expr = expr.clone();
				next_fire = None; // force recompute against the new schedule
				schedule = match expr.as_deref() {
					Some(e) if !e.trim().is_empty() => match parse_schedule(e) {
						Some(s) => {
							log::info!("[refresh] Schedule enabled: {}", normalize_expr(e));
							Some(s)
						}
						None => None,
					},
					_ => {
						log::info!(
							"[refresh] No cron expression configured — page will not auto-refresh"
						);
						None
					}
				};
			}

			let Some(sched) = schedule.as_ref() else {
				// Nothing scheduled; re-check shortly in case config changes.
				tokio::time::sleep(Duration::from_secs(60)).await;
				continue;
			};

			// Compute the next fire time if we don't have one yet.
			if next_fire.is_none() {
				next_fire = sched.after(&Local::now()).next();
			}

			match next_fire {
				Some(fire) => {
					let now = Local::now();
					if fire <= now {
						log::info!("[refresh] Cron fired — reloading webview");
						if let Err(e) = crate::commands::refresh_display(&app_handle).await {
							log::error!("[refresh] webview reload failed: {}", e);
						}
						// Next fire relative to now (avoids re-firing the same tick).
						next_fire = sched.after(&Local::now()).next();
					} else {
						// Sleep until fire, but cap the wait so config changes
						// (and any clock drift) are noticed within ~60s.
						let wait = (fire - now).num_seconds();
						let capped = wait.clamp(1, 60) as u64;
						tokio::time::sleep(Duration::from_secs(capped)).await;
					}
				}
				None => {
					tokio::time::sleep(Duration::from_secs(60)).await;
				}
			}
		}
	});
}

#[cfg(test)]
mod tests {
	use super::*;
use chrono::Timelike;

	#[test]
	fn normalizes_five_field_cron() {
		assert_eq!(normalize_expr("0 0 * * *"), "0 0 0 * * *");
	}

	#[test]
	fn leaves_six_field_cron_alone() {
		assert_eq!(normalize_expr("0 0 0 * * *"), "0 0 0 * * *");
	}

	#[test]
	fn parses_midnight_daily() {
		let s = parse_schedule("0 0 * * *").expect("midnight should parse");
		let now = Local::now();
		let next = s.after(&now).next().expect("should have a next fire");
		assert_eq!(next.hour(), 0);
		assert_eq!(next.minute(), 0);
		assert!(next > now);
	}

	#[test]
	fn rejects_garbage() {
		assert!(parse_schedule("not a cron").is_none());
	}
}
