use serde_json::Value;

/// Gather system information for device announcement and status
pub fn get_system_info() -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    // Get serial: Pi-specific /proc/cpuinfo first, then fallback to /etc/machine-id
    let serial = get_pi_serial()
        .or_else(get_machine_id)
        .unwrap_or_else(|| "unknown".to_string());

    // Get MAC and IP from default network interface
    let (mac, ip) = get_network_info().unwrap_or_else(|| ("unknown".to_string(), "unknown".to_string()));

    Ok(serde_json::json!({
        "serial": serial,
        "mac": mac,
        "ip": ip,
        "system": {
            "osInfo": {
                "platform": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "kernel": get_kernel_version().unwrap_or_default(),
            },
            "defaultNetworkInterface": {
                "mac": mac,
                "ip4": ip,
            }
        }
    }))
}

fn get_pi_serial() -> Option<String> {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    for line in cpuinfo.lines() {
        if line.starts_with("Serial") {
            return line.split(':').nth(1).map(|s| s.trim().to_string());
        }
    }
    None
}

/// Fallback: read /etc/machine-id (stable across boots, unique per OS install)
fn get_machine_id() -> Option<String> {
    let id = std::fs::read_to_string("/etc/machine-id").ok()?;
    Some(id.trim().to_string())
}

fn get_kernel_version() -> Option<String> {
    let output = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Get the current IP address of the default interface at runtime.
pub fn get_current_ip() -> String {
    if let Some((_, ip)) = get_network_info() {
        ip
    } else {
        "unknown".to_string()
    }
}

fn get_network_info() -> Option<(String, String)> {
    // Use `ip` command to find default interface
    let output = std::process::Command::new("ip")
        .args(["route", "show", "default"])
        .output()
        .ok()?;
    let route = String::from_utf8_lossy(&output.stdout);
    let iface = route.split_whitespace().nth(4)?;

    // Get MAC
    let mac_output = std::process::Command::new("cat")
        .arg(format!("/sys/class/net/{}/address", iface))
        .output()
        .ok()?;
    let mac = String::from_utf8_lossy(&mac_output.stdout).trim().to_string();

    // Get IPv4
    let ip_output = std::process::Command::new("ip")
        .args(["-4", "addr", "show", iface])
        .output()
        .ok()?;
    let ip_text = String::from_utf8_lossy(&ip_output.stdout);
    let ip = ip_text
        .lines()
        .find(|l| l.contains("inet "))?
        .split_whitespace()
        .nth(1)?
        .split('/')
        .next()?
        .to_string();

    Some((mac, ip))
}
