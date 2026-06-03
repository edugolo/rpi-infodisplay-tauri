#!/usr/bin/env bash
#
# install.sh — Install rpi-infodisplay on Raspberry Pi OS
#
# Usage (from your PC, one-liner over SSH):
#
#   ssh pi@kiosk.local 'curl -fsSL https://raw.githubusercontent.com/edugolo/rpi-infodisplay-tauri/main/deploy/install.sh | sudo bash -s -- --version v0.0.1'
#
#   # Or with a config:
#   ssh pi@kiosk.local 'curl -fsSL ... | sudo bash -s -- --version v0.0.1 --config-base64 BASE64ENCODEDCONFIG'
#
#   # Or the latest release:
#   ssh pi@kiosk.local 'curl -fsSL ... | sudo bash -s -- --latest'
#
#   # Or if you already have the script on the Pi:
#   sudo bash install.sh --version v0.0.1
#
set -euo pipefail

# ── Colors ───────────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

info()  { echo -e "${BLUE}[INFO]${NC}  $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}    $*"; }
err()   { echo -e "${RED}[ERROR]${NC} $*" >&2; }

# ── Pre-flight ───────────────────────────────────────────────────────────────
# If not root, save self to a temp file and re-exec with sudo.
# This allows piping over SSH: cat install.sh | ssh pi@host 'bash -s -- --latest'
if [[ $EUID -ne 0 ]]; then
    # We're being piped — save to temp and re-exec
    TMPFILE=$(mktemp /tmp/rpi-infodisplay-install.XXXXXX.sh)
    cat > "$TMPFILE"
    chmod +x "$TMPFILE"
    exec sudo bash "$TMPFILE" "$@"
fi

# ── Defaults ─────────────────────────────────────────────────────────────────
GITHUB_REPO="edugolo/rpi-infodisplay-tauri"
INSTALL_DIR="/opt/rpi-infodisplay"
# Default to the user that invoked sudo (or 'pi' if SUDO_USER is empty)
KIOSK_USER="${SUDO_USER:-pi}"
VERSION=""
BINARY_SOURCE=""
CONFIG_B64=""

# ── Parse arguments ──────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case $1 in
        --version|-v)
            VERSION="$2"; shift 2 ;;
        --latest)
            VERSION="latest"; shift ;;
        --binary)
            BINARY_SOURCE="$2"; shift 2 ;;
        --dir)
            INSTALL_DIR="$2"; shift 2 ;;
        --user)
            KIOSK_USER="$2"; shift 2 ;;
        --config-base64)
            # Pass config.json as base64 to avoid quoting hell over SSH:
            #   --config-base64 $(cat config.json | base64 -w0)
            CONFIG_B64="$2"; shift 2 ;;
        -h|--help)
            echo "Usage: sudo bash install.sh [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --version TAG      Download binary from GitHub release tag (e.g. v0.0.1)"
            echo "  --latest           Download binary from the latest GitHub release"
            echo "  --binary PATH      Use a local binary instead of downloading"
            echo "  --dir DIR          Installation directory (default: /opt/rpi-infodisplay)"
            echo "  --user USER        User to run the service as (default: pi)"
            echo "  --config-base64 B  Base64-encoded config.json"
            echo "  -h, --help         Show this help"
            echo ""
            echo "Examples:"
            echo "  sudo bash install.sh --version v0.0.1"
            echo "  sudo bash install.sh --latest --config-base64 \$(cat config.json | base64 -w0)"
            echo ""
            echo "Remote one-liner from your PC:"
            echo "  ssh pi@kiosk.local 'curl -fsSL RAW_URL | sudo bash -s -- --version v0.0.1'"
            exit 0
            ;;
        *)
            err "Unknown option: $1"; exit 1 ;;
    esac
done

# ── Step 1: System dependencies ──────────────────────────────────────────────
info "Updating package lists..."
apt-get update -qq

info "Installing system dependencies..."
DEPS=(
    # X server (needed on Pi OS Lite)
    xorg xserver-xorg-video-all xinit

    # Tauri/WebKitGTK runtime
    libwebkit2gtk-4.1-0 libgtk-3-0
    libgdk-pixbuf-2.0-0 libpango-1.0-0 libcairo2
    libglib2.0-0 libayatana-appindicator3-1
    librsvg2-common

    # Fonts
    fonts-dejavu-core fonts-liberation

    # Screenshot tools (app tries: scrot → import from imagemagick)
    scrot imagemagick

    # HDMI CEC display control (cec-client)
    cec-utils

    # Needed by this script / app
    curl ca-certificates
)
apt-get install -y --no-install-recommends "${DEPS[@]}"
ok "System dependencies installed."

# ── Step 2: Get the binary ──────────────────────────────────────────────────
mkdir -p "${INSTALL_DIR}"

if [[ -n "${BINARY_SOURCE}" ]]; then
    # ── Local file ────────────────────────────────────────────────
    if [[ ! -f "${BINARY_SOURCE}" ]]; then
        err "Binary not found: ${BINARY_SOURCE}"; exit 1
    fi
    cp "${BINARY_SOURCE}" "${INSTALL_DIR}/rpi-infodisplay"
    ok "Binary copied from ${BINARY_SOURCE}"

elif [[ -n "${VERSION}" ]]; then
    # ── Download from GitHub Releases ─────────────────────────────
    if [[ "${VERSION}" == "latest" ]]; then
        info "Looking up latest release..."
        RELEASE_URL=$(curl -fsSL "https://api.github.com/repos/${GITHUB_REPO}/releases/latest" \
            | grep -oP '"browser_download_url":\s*"\K[^"]*rpi-infodisplay"' \
            | head -1 | tr -d '"')
        if [[ -z "${RELEASE_URL}" ]]; then
            # Fallback: just grab the first asset
            RELEASE_URL=$(curl -fsSL "https://api.github.com/repos/${GITHUB_REPO}/releases/latest" \
                | grep -oP '"browser_download_url":\s*"\K[^"]+' \
                | head -1)
        fi
    else
        # Determine arch-appropriate binary name
        ARCH=$(uname -m)
        if [[ "${ARCH}" == "aarch64" ]]; then
            # The default (arm) binary
            RELEASE_URL="https://github.com/${GITHUB_REPO}/releases/download/${VERSION}/rpi-infodisplay"
        else
            RELEASE_URL="https://github.com/${GITHUB_REPO}/releases/download/${VERSION}/rpi-infodisplay-x86_64"
        fi
    fi

    info "Downloading binary from ${RELEASE_URL}..."
    HTTP_CODE=$(curl -fsSL -w '%{http_code}' -o "${INSTALL_DIR}/rpi-infodisplay" "${RELEASE_URL}" || true)
    if [[ "${HTTP_CODE}" != "200" ]]; then
        err "Download failed (HTTP ${HTTP_CODE}). URL: ${RELEASE_URL}"
        err "Check available releases at: https://github.com/${GITHUB_REPO}/releases"
        exit 1
    fi
    ok "Binary downloaded"

else
    err "No binary source specified. Use --version TAG, --latest, or --binary PATH"
    exit 1
fi

chmod +x "${INSTALL_DIR}/rpi-infodisplay"
chown -R "${KIOSK_USER}:${KIOSK_USER}" "${INSTALL_DIR}"
ok "Binary installed: ${INSTALL_DIR}/rpi-infodisplay ($(du -h "${INSTALL_DIR}/rpi-infodisplay" | cut -f1))"

# ── Step 3: Config ───────────────────────────────────────────────────────────
if [[ -n "${CONFIG_B64}" ]]; then
    echo "${CONFIG_B64}" | base64 -d > "${INSTALL_DIR}/config.json"
    chown "${KIOSK_USER}:${KIOSK_USER}" "${INSTALL_DIR}/config.json"
    ok "Config installed (from --config-base64)"
elif [[ -f "${INSTALL_DIR}/config.json" ]]; then
    ok "Existing config.json preserved."
else
    cat > "${INSTALL_DIR}/config.json" << 'EOF'
{
  "name": "",
  "location": "",
  "controller": "",
  "url": "https://edugo.be",
  "fullscreen": true,
  "frame": false,
  "zoomFactor": 1.0
}
EOF
    chown "${KIOSK_USER}:${KIOSK_USER}" "${INSTALL_DIR}/config.json"
    warn "No config provided — wrote default. Edit it:"
    warn "  sudo nano ${INSTALL_DIR}/config.json"
fi

# ── Step 4: Systemd service ─────────────────────────────────────────────────
info "Installing systemd service..."
cat > /etc/systemd/system/rpi-infodisplay.service << EOF
[Unit]
Description=Edugo Kiosk Display (Tauri)
After=graphical.target network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${KIOSK_USER}
Environment=DISPLAY=:0
Environment=XAUTHORITY=/home/${KIOSK_USER}/.Xauthority
WorkingDirectory=${INSTALL_DIR}
ExecStartPre=/bin/sleep 3
ExecStart=${INSTALL_DIR}/rpi-infodisplay
Restart=on-failure
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=kiosk-display

[Install]
WantedBy=graphical.target
EOF

systemctl daemon-reload
ok "Systemd service installed."

# ── Step 5: Auto-login + X11 auto-start (Pi OS Lite) ────────────────────────
info "Configuring auto-login and X11..."

# Auto-login on tty1
if command -v raspi-config &>/dev/null; then
    raspi-config nonint do_boot_behaviour B2 2>/dev/null || true
else
    mkdir -p /etc/systemd/system/getty@tty1.service.d
    cat > /etc/systemd/system/getty@tty1.service.d/autologin.conf << EOF
[Service]
ExecStart=
ExecStart=-/sbin/agetty -a ${KIOSK_USER} --noclear %I \$TERM
EOF
fi
ok "Auto-login configured"

# .xinitrc — disable screensaver/blanking, run the binary
XINITRC="/home/${KIOSK_USER}/.xinitrc"
cat > "${XINITRC}" << 'XEOF'
#!/bin/bash
xset s off
xset -dpms
xset s noblank
exec /opt/rpi-infodisplay/rpi-infodisplay
XEOF
chown "${KIOSK_USER}:${KIOSK_USER}" "${XINITRC}"
chmod +x "${XINITRC}"

# Auto-start X on tty1 login (only if not already there)
PROFILE="/home/${KIOSK_USER}/.profile"
if ! grep -q "startx" "${PROFILE}" 2>/dev/null; then
    cat >> "${PROFILE}" << 'PEOF'

# Auto-start X11 kiosk on tty1
if [[ -z "${DISPLAY:-}" ]] && [[ "$(tty)" == "/dev/tty1" ]]; then
    startx -- -nocursor > /dev/null 2>&1 &
    logout
fi
PEOF
    chown "${KIOSK_USER}:${KIOSK_USER}" "${PROFILE}"
fi
ok "X11 auto-start configured"

# ── Step 6: Enable service ──────────────────────────────────────────────────
systemctl enable rpi-infodisplay.service 2>/dev/null || true
ok "Service enabled"

# ── Done ─────────────────────────────────────────────────────────────────────
echo ""
echo -e "${GREEN}╔══════════════════════════════════════════════════════════════╗${NC}"
echo -e "${GREEN}║  rpi-infodisplay installed successfully!                    ║${NC}"
echo -e "${GREEN}╚══════════════════════════════════════════════════════════════╝${NC}"
echo ""
echo "  Binary:  ${INSTALL_DIR}/rpi-infodisplay"
echo "  Config:  ${INSTALL_DIR}/config.json"
echo "  Logs:    journalctl -u rpi-infodisplay -f"
echo "  Restart: sudo systemctl restart rpi-infodisplay"
echo ""
echo -e "${YELLOW}  Reboot to activate: sudo reboot${NC}"
echo ""
