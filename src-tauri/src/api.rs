use crate::signing;
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

/// Build the full API URL and return (full_url, pathname_for_signing)
fn build_url(base_url: &str, api_path: &str) -> (String, String) {
    let base = base_url.trim_end_matches('/');
    let full = format!("{}{}", base, api_path);
    let pathname = url::Url::parse(&full)
        .map(|u| u.path().to_string())
        .unwrap_or_else(|_| api_path.to_string());
    (full, pathname)
}

/// Build authentication headers for a device request
fn build_auth_headers(
    private_key_pem: &str,
    device_id: &str,
    method: &str,
    path: &str,
    body: &str,
) -> Result<reqwest::header::HeaderMap, Box<dyn std::error::Error + Send + Sync>> {
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let message = format!("{}\n{}\n{}\n{}", method, path, timestamp, body);
    let signature = signing::sign_message(private_key_pem, &message)?;

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("X-Device-Id", device_id.parse()?);
    headers.insert("X-Timestamp", timestamp.parse()?);
    headers.insert("X-Signature", signature.parse()?);

    if !body.is_empty() {
        headers.insert("Content-Type", "application/json".parse()?);
    }

    Ok(headers)
}

/// Announce device to the controller (no auth required)
pub async fn announce(
    controller_url: &str,
    serial: &str,
    mac: &str,
    public_key_pem: &str,
    system_info: &Value,
    config: &Value,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let (url, _) = build_url(controller_url, "/api/v1/kiosk/announce");

    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let body = serde_json::json!({
        "serial": serial,
        "mac": mac,
        "publicKey": public_key_pem,
        "systemInfo": system_info,
        "config": config,
    });

    let resp = client.post(&url).json(&body).send().await?;
    let result: Value = resp.json().await?;

    if result["success"].as_bool() == Some(true) {
        Ok(result["data"].clone())
    } else {
        let error = result["error"].as_str().unwrap_or("Unknown error");
        Err(format!("Announce failed: {}", error).into())
    }
}

/// Poll for config updates and pending commands
pub async fn poll(
    controller_url: &str,
    device_id: &str,
    private_key_pem: &str,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let (url, pathname) = build_url(controller_url, &format!("/api/v1/kiosk/{}/poll", device_id));

    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let headers = build_auth_headers(private_key_pem, device_id, "GET", &pathname, "")?;

    let resp = client.get(&url).headers(headers).send().await?;
    let result: Value = resp.json().await?;

    if result["success"].as_bool() == Some(true) {
        Ok(result)
    } else {
        let error = result["error"].as_str().unwrap_or("Unknown error");
        Err(format!("Poll failed: {}", error).into())
    }
}

/// Send heartbeat to the controller
pub async fn heartbeat(
    controller_url: &str,
    device_id: &str,
    private_key_pem: &str,
    data: &Value,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let (url, pathname) = build_url(controller_url, &format!("/api/v1/kiosk/{}/heartbeat", device_id));

    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let body_str = serde_json::to_string(data)?;
    let headers = build_auth_headers(private_key_pem, device_id, "POST", &pathname, &body_str)?;

    let resp = client.post(&url).headers(headers).json(data).send().await?;
    let result: Value = resp.json().await?;

    if result["success"].as_bool() == Some(true) {
        Ok(result)
    } else {
        let error = result["error"].as_str().unwrap_or("Unknown error");
        Err(format!("Heartbeat failed: {}", error).into())
    }
}

/// Acknowledge a command
pub async fn ack_command(
    controller_url: &str,
    device_id: &str,
    private_key_pem: &str,
    command_id: &str,
    status: &str,
    error_message: Option<&str>,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let (url, pathname) = build_url(
        controller_url,
        &format!("/api/v1/kiosk/{}/commands/{}/ack", device_id, command_id),
    );

    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let body = serde_json::json!({
        "status": status,
        "errorMessage": error_message,
    });
    let body_str = serde_json::to_string(&body)?;
    let headers = build_auth_headers(private_key_pem, device_id, "POST", &pathname, &body_str)?;

    let resp = client.post(&url).headers(headers).json(&body).send().await?;
    let result: Value = resp.json().await?;

    if result["success"].as_bool() == Some(true) {
        Ok(result)
    } else {
        let error = result["error"].as_str().unwrap_or("Unknown error");
        Err(format!("Ack failed: {}", error).into())
    }
}

/// Upload screenshot to the controller
pub async fn upload_screenshot(
    controller_url: &str,
    device_id: &str,
    private_key_pem: &str,
    command_id: &str,
    png_data: &[u8],
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let (url, pathname) = build_url(
        controller_url,
        &format!("/api/v1/kiosk/{}/screenshot", device_id),
    );

    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    // Encode PNG as base64 — server expects JSON { commandId, image: base64 }
    let base64_image = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png_data);
    let body = serde_json::json!({
        "commandId": command_id,
        "image": base64_image,
    });
    let body_str = serde_json::to_string(&body)?;

    let headers = build_auth_headers(private_key_pem, device_id, "POST", &pathname, &body_str)?;

    let resp = client.post(&url).headers(headers).body(body_str).send().await?;
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();

    // Try to parse as JSON, fall back to treating non-2xx as error
    let result: Value = serde_json::from_str(&body_text).unwrap_or_else(|_| {
        serde_json::json!({ "success": status.is_success(), "raw": body_text })
    });

    if result["success"].as_bool() == Some(true) {
        Ok(result)
    } else {
        let error = result["error"].as_str().unwrap_or_else(|| body_text.as_str());
        Err(format!("Screenshot upload failed ({}): {}", status, error).into())
    }
}
