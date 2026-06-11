# rpi-infodisplay (Tauri v2)

Raspberry Pi kiosk display application — migrated from Electron to Tauri v2.

## Why Tauri?

| | Electron | Tauri v2 |
|---|---|---|
| Binary size | ~200MB | **~10-15MB** |
| RAM at idle | ~200-350MB | **~30-60MB** |
| Processes | 4-8 | **1** |
| Boot time | ~15-20s | **~3-6s** |

## Prerequisites

### Development machine
```bash
# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Tauri system deps (Debian/Ubuntu)
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev
```

### Raspberry Pi OS Lite (target)

No manual setup needed — the install script handles everything. See [Deployment](#deployment) below.

<details>
<summary>Manual dependency list (for reference)</summary>

```bash
sudo apt install --no-install-recommends \
  cage seatd \
  libwebkit2gtk-4.1-0 libgtk-3-0 \
  libgdk-pixbuf-2.0-0 libpango-1.0-0 libcairo2 \
  libglib2.0-0 libayatana-appindicator3-1 \
  librsvg2-common fonts-dejavu-core fonts-liberation \
  grim wlr-randr \
  mesa-utils libgl1-mesa-dri libegl1 libgles2 \
  curl ca-certificates
```

| Package | Why |
|---|---|
| `cage` | Wayland kiosk compositor |
| `seatd` | DRM/seat management for cage |
| `libwebkit2gtk-4.1-0` | WebKit rendering engine (Tauri runtime) |
| `libgtk-3-0` | GTK3 (Tauri runtime) |
| `mesa-utils`, `libgl1-mesa-dri` | GPU acceleration (VC4/V3D) |
| `grim` | Wayland-native screenshot tool |
| `wlr-randr` | Display power control (wake/sleep HDMI output) |

</details>

## Build

```bash
# Development (native)
cd src-tauri
cargo build

# Cross-compile for Pi (aarch64)
rustup target add aarch64-unknown-linux-gnu
cargo build --target aarch64-unknown-linux-gnu --release
```

## Configuration

Create `config.json` in the working directory:

```json
{
  "name": "Display 01",
  "location": "Hall A",
  "controller": "https://controller.example.com/tenant",
  "url": "https://edugo.be",
  "fullscreen": true,
  "frame": false,
  "zoomFactor": 1.0,
  "displaySchedule": {
    "enabled": true,
    "on": "07:00",
    "off": "22:00",
    "days": ["mon", "tue", "wed", "thu", "fri"]
  }
}
```

### Display Schedule

The `displaySchedule` section controls CEC-based automatic power on/off of the connected display:

| Field | Type | Description |
|-------|------|-------------|
| `enabled` | `bool` | Enable or disable the schedule |
| `on` | `string` | Time to power on (HH:MM, 24h format) |
| `off` | `string` | Time to power off (HH:MM, 24h format) |
| `days` | `string[]` | Days to apply (e.g. `["mon","fri"]`). Empty = every day |

The service runs 24/7 — only the screen is powered via HDMI-CEC. At the configured `on` time the app sends `IMAGE_VIEW_ON`, at `off` time it sends `STANDBY`. No systemd timers are involved.

## Key Storage

The service runs as root, so keys and device ID are stored in `/root/.config/rpi-infodisplay/`:
- `device.key` — Ed25519 private key (PEM)
- `device.pub` — Ed25519 public key (PEM)
- `device-id` — UUID assigned by the controller

To reset the identity (re-register as a new device):
```bash
sudo rm -rf /root/.config/rpi-infodisplay/
sudo reboot
```

## Deployment

### One-liner install on a fresh Pi (recommended)

SSH into the Pi and run:

```bash
# Download and run the installer (latest release)
curl -fsSL https://raw.githubusercontent.com/edugolo/rpi-infodisplay-tauri/main/deploy/install.sh | sudo bash -s -- --latest
```

With config values:

```bash
curl -fsSL https://raw.githubusercontent.com/edugolo/rpi-infodisplay-tauri/main/deploy/install.sh | sudo bash -s -- --latest \
  --name "helpdesk" --location "g006" \
  --controller "https://app.edugo.be/lochristi" \
  --url "https://app.edugo.be/lochristi/display"
```

All install options:

| Option | Default | Description |
|--------|---------|-------------|
| `--version TAG` | — | Download from a specific GitHub release tag |
| `--latest` | — | Download the latest GitHub release |
| `--binary PATH` | — | Use a local binary instead of downloading |
| `--name NAME` | — | Display name (shown in controller) |
| `--location LOC` | — | Physical location label |
| `--controller URL` | — | Controller API base URL |
| `--url URL` | `https://edugo.be` | Start URL displayed in the kiosk |
| `--on-time HH:MM` | `09:00` | CEC-on time (screen wake) |
| `--off-time HH:MM` | `16:00` | CEC-off time (screen standby) |
| `--schedule-enabled true\|false` | `true` | Enable/disable display schedule |
| `--no-schedule` | — | Shortcut for `--schedule-enabled=false` |
| `--dir DIR` | `/opt/rpi-infodisplay` | Install directory |
| `--user USER` | `pi` | System user for runtime dirs |

Then reboot:

```bash
sudo reboot
```

What the script does:
1. Installs all system dependencies (cage, WebKitGTK, GPU drivers, fonts)
2. Downloads the latest binary from GitHub Releases
3. Applies Raspberry Pi GPU optimizations (`gpu_mem`, `vc4-kms-v3d`)
4. Creates systemd service — starts on boot, runs 24/7 (screen power is CEC-only)
5. Writes config to `/opt/rpi-infodisplay/config.json`
6. Configures **autologin** on tty1 — cage needs an active console session to bind to seatd
7. Enables seatd for DRM access

### Updating an existing Pi

```bash
# Quick binary swap (keeps config):
sudo systemctl stop rpi-infodisplay
curl -fsSL -o /opt/rpi-infodisplay/rpi-infodisplay \
  https://github.com/edugolo/rpi-infodisplay-tauri/releases/latest/download/rpi-infodisplay-aarch64
sudo chmod +x /opt/rpi-infodisplay/rpi-infodisplay
sudo systemctl start rpi-infodisplay
```

Or re-run the full installer — it preserves existing config.

### Managing the service

```bash
sudo systemctl status rpi-infodisplay      # check status
journalctl -u rpi-infodisplay -f            # follow logs
sudo systemctl restart rpi-infodisplay      # restart
sudo systemctl stop rpi-infodisplay         # stop
```

## License

MIT
