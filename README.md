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
```bash
# Minimal X11 + WebKitGTK + screenshot tools + display control
sudo apt install --no-install-recommends \
  xorg xserver-xorg-video-all xinit \
  libwebkit2gtk-4.1-0 libgtk-3-0 \
  libgdk-pixbuf-2.0-0 libpango-1.0-0 libcairo2 \
  libglib2.0-0 libayatana-appindicator3-1 \
  librsvg2-common fonts-dejavu-core \
  scrot imagemagick \
  cec-utils curl ca-certificates
```

| Package | Why |
|---|---|
| `xorg`, `xserver-xorg-video-all`, `xinit` | X11 server (Pi OS Lite has no desktop) |
| `libwebkit2gtk-4.1-0` | WebKit rendering engine (Tauri runtime) |
| `libgtk-3-0` | GTK3 (Tauri runtime) |
| `scrot` | Primary screenshot tool (X11, reliable on Pi) |
| `imagemagick` | Fallback screenshot (`import -window root`) |
| `cec-utils` | HDMI CEC control (turn TV on/off via `cec-client`) |

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

The `displaySchedule` section controls automatic power on/off of the connected display:

| Field | Type | Description |
|-------|------|-------------|
| `enabled` | `bool` | Enable or disable the schedule |
| `on` | `string` | Time to power on (HH:MM, 24h format) |
| `off` | `string` | Time to power off (HH:MM, 24h format) |
| `days` | `string[]` | Days to apply (e.g. `["mon","fri"]`). Empty = every day |

Power-off uses **CEC** (via `cec-client`) first; if that fails it falls back to **HDMI** power control (`vcgencmd display_power`). This requires `cec-client` (from `libcec-dev`) to be installed for CEC, and `vcgencmd` (present on Raspberry Pi OS) for the HDMI fallback.

## Key Storage

Keys and device ID are stored in `~/.config/rpi-infodisplay/`:
- `device.key` — Ed25519 private key (PEM)
- `device.pub` — Ed25519 public key (PEM)
- `device-id` — UUID assigned by the controller

## Deployment

### One-liner from your PC (recommended)

Downloads the binary from GitHub Releases, installs everything, no files to copy:

```bash
# Minimal — reuses existing config (or defaults) on the Pi:
ssh pi@kiosk.local 'curl -fsSL https://raw.githubusercontent.com/edugolo/rpi-infodisplay-tauri/main/deploy/install.sh | sudo bash -s -- --latest'

# With config values via env vars:
NAME="Hall A" LOCATION="Floor 1" CONTROLLER="https://controller.example.com/tenant" URL="https://edugo.be" \
  ssh pi@kiosk.local 'curl -fsSL https://raw.githubusercontent.com/edugolo/rpi-infodisplay-tauri/main/deploy/install.sh | sudo bash -s -- --latest'

# Or a specific version:
ssh pi@kiosk.local 'curl -fsSL https://raw.githubusercontent.com/edugolo/rpi-infodisplay-tauri/main/deploy/install.sh | sudo bash -s -- --version v0.0.1'

# Then reboot
ssh pi@kiosk.local 'sudo reboot'
```

The script installs all system deps (X11, WebKitGTK, screenshot tools, CEC),
downloads the binary, sets up the systemd service, and configures auto-login
+ X11 auto-start — all in one go.

### Managing the service

```bash
ssh pi@kiosk.local 'sudo systemctl status rpi-infodisplay'     # check status
ssh pi@kiosk.local 'journalctl -u rpi-infodisplay -f'           # follow logs
ssh pi@kiosk.local 'sudo systemctl restart rpi-infodisplay'     # restart
ssh pi@kiosk.local 'sudo systemctl stop rpi-infodisplay'        # stop
```

## License

MIT
