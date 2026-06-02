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
# Minimal X11 + WebKitGTK
sudo apt install --no-install-recommends \
  xorg xserver-xorg-video-all \
  libwebkit2gtk-4.1-0 libgtk-3-0 \
  libgdk-pixbuf2.0-0 libpango-1.0-0 libcairo2 \
  libglib2.0-0 libayatana-appindicator3-1 \
  librsvg2-common fonts-dejavu-core scrot
```

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
  "zoomFactor": 1.0
}
```

## Key Storage

Keys and device ID are stored in `~/.config/rpi-infodisplay/`:
- `device.key` — Ed25519 private key (PEM)
- `device.pub` — Ed25519 public key (PEM)
- `device-id` — UUID assigned by the controller

## Deployment

```bash
# Copy binary to Pi
scp target/aarch64-unknown-linux-gnu/release/rpi-infodisplay pi@kiosk.local:~/

# Restart service
ssh pi@kiosk.local "sudo systemctl restart kiosk-display"
```

## License

MIT
