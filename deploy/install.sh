#!/usr/bin/env bash
#
# install.sh — Install rpi-infodisplay on Raspberry Pi OS
#
# Usage (from your PC, one-liner over SSH):
#
#   ssh pi@kiosk.local 'curl -fsSL RAW_URL | sudo bash -s -- --latest'
#
#   # With config:
#   ssh pi@kiosk.local 'curl -fsSL RAW_URL | sudo bash -s -- --latest \
#       --name "Hall A" --location "Floor 1" --controller "https://ctrl.example.com"'
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
if [[ $EUID -ne 0 ]]; then
    TMPFILE=$(mktemp /tmp/rpi-infodisplay-install.XXXXXX.sh)
    cat > "$TMPFILE"
    chmod +x "$TMPFILE"
    exec sudo bash "$TMPFILE" "$@"
fi

# ── Defaults ─────────────────────────────────────────────────────────────────
GITHUB_REPO="edugolo/rpi-infodisplay-tauri"
INSTALL_DIR="/opt/rpi-infodisplay"
KIOSK_USER="${SUDO_USER:-pi}"
VERSION=""
BINARY_SOURCE=""
CONF_NAME=""
CONF_LOCATION=""
CONF_CONTROLLER=""
CONF_URL=""

# ── Parse arguments ──────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case $1 in
        --version|-v)    VERSION="$2"; shift 2 ;;
        --latest)        VERSION="latest"; shift ;;
        --binary)        BINARY_SOURCE="$2"; shift 2 ;;
        --dir)           INSTALL_DIR="$2"; shift 2 ;;
        --user)          KIOSK_USER="$2"; shift 2 ;;
        --name)          CONF_NAME="$2"; shift 2 ;;
        --location)      CONF_LOCATION="$2"; shift 2 ;;
        --controller)    CONF_CONTROLLER="$2"; shift 2 ;;
        --url)           CONF_URL="$2"; shift 2 ;;
        -h|--help)
            echo "Usage: sudo bash install.sh [OPTIONS]"
            echo ""
            echo "  --version TAG      Download from GitHub release tag"
            echo "  --latest           Download from the latest GitHub release"
            echo "  --binary PATH      Use a local binary"
            echo "  --name NAME        Display name"
            echo "  --location LOC     Display location"
            echo "  --controller URL   Controller URL"
            echo "  --url URL          Start URL (default: https://edugo.be)"
            echo "  --dir DIR          Install dir (default: /opt/rpi-infodisplay)"
            echo "  --user USER        Service user (default: current user)"
            exit 0
            ;;
        *) err "Unknown option: $1"; exit 1 ;;
    esac
done

# ── Step 1: System dependencies ──────────────────────────────────────────────
info "Updating package lists..."
apt-get update -qq

info "Installing system dependencies..."
DEPS=(
    # Wayland kiosk compositor + seat management
    cage seatd

    # Tauri/WebKitGTK runtime
    libwebkit2gtk-4.1-0 libgtk-3-0
    libgdk-pixbuf-2.0-0 libpango-1.0-0 libcairo2
    libglib2.0-0 libayatana-appindicator3-1
    librsvg2-common

    # Fonts
    fonts-dejavu-core fonts-liberation

    # Screenshot tool (Wayland native)
    grim

    # Display power control (wlr-randr for wlroots compositors)
    wlr-randr

    # GPU acceleration (Mesa VC4/V3D DRI driver)
    mesa-utils libgl1-mesa-dri libegl1 libgles2

    # Needed by this script / app
    curl ca-certificates
)
apt-get install -y --no-install-recommends "${DEPS[@]}"
ok "System dependencies installed."

# ── Step 2: Get the binary ──────────────────────────────────────────────────
mkdir -p "${INSTALL_DIR}"

if [[ -n "${BINARY_SOURCE}" ]]; then
    if [[ ! -f "${BINARY_SOURCE}" ]]; then
        err "Binary not found: ${BINARY_SOURCE}"; exit 1
    fi
    cp "${BINARY_SOURCE}" "${INSTALL_DIR}/rpi-infodisplay"
    ok "Binary copied from ${BINARY_SOURCE}"

elif [[ -n "${VERSION}" ]]; then
    # Determine the correct binary name for this architecture
    ARCH=$(uname -m)
    if [[ "${ARCH}" == "aarch64" ]]; then
        BINARY_NAME="rpi-infodisplay-aarch64"
    else
        BINARY_NAME="rpi-infodisplay-x86_64"
    fi

    if [[ "${VERSION}" == "latest" ]]; then
        info "Looking up latest release..."
        TAG=$(curl -fsSL "https://api.github.com/repos/${GITHUB_REPO}/releases/latest" \
            | grep -oP '"tag_name":\s*"\K[^"]+' \
            | head -1)
        if [[ -z "${TAG}" ]]; then
            err "Could not determine latest release tag"
            exit 1
        fi
        info "Latest release: ${TAG}"
        RELEASE_URL="https://github.com/${GITHUB_REPO}/releases/download/${TAG}/${BINARY_NAME}"
    else
        RELEASE_URL="https://github.com/${GITHUB_REPO}/releases/download/${VERSION}/${BINARY_NAME}"
    fi

    info "Downloading binary from ${RELEASE_URL}..."
    HTTP_CODE=$(curl -fsSL -w '%{http_code}' -o "${INSTALL_DIR}/rpi-infodisplay" "${RELEASE_URL}" || true)
    if [[ "${HTTP_CODE}" != "200" ]]; then
        err "Download failed (HTTP ${HTTP_CODE}). URL: ${RELEASE_URL}"
        err "Check: https://github.com/${GITHUB_REPO}/releases"
        exit 1
    fi
    ok "Binary downloaded"

else
    err "No binary source. Use --latest, --version TAG, or --binary PATH"
    exit 1
fi

chmod +x "${INSTALL_DIR}/rpi-infodisplay"
chown -R "${KIOSK_USER}:${KIOSK_USER}" "${INSTALL_DIR}"
ok "Binary installed: ${INSTALL_DIR}/rpi-infodisplay ($(du -h "${INSTALL_DIR}/rpi-infodisplay" | cut -f1))"

# ── Step 2b: Raspberry Pi GPU optimization ───────────────────────────────────
if command -v raspi-config &>/dev/null; then
    info "Raspberry Pi detected — applying GPU optimizations..."

    # Detect Pi model
    PI_MODEL=$(tr -d '\0' < /proc/device-tree/model 2>/dev/null || echo "")
    info "Detected: ${PI_MODEL}"

    BOOT_CONFIG=""
    for f in /boot/firmware/config.txt /boot/config.txt; do
        if [[ -f "$f" ]]; then BOOT_CONFIG="$f"; break; fi
    done

    if [[ -n "${BOOT_CONFIG}" ]]; then
        # Pi 0/1/2/3: shared memory architecture — gpu_mem matters
        # Pi 4+: dedicated GPU memory — gpu_mem is ignored or wastes ARM RAM
        if echo "${PI_MODEL}" | grep -qE 'Pi [0123]'; then
            sed -i '/^gpu_mem/d' "${BOOT_CONFIG}"
            echo "gpu_mem=384" >> "${BOOT_CONFIG}"
            ok "GPU: gpu_mem=384 (shared memory Pi)"
        else
            # Pi 4/5: remove gpu_mem if present, not needed
            sed -i '/^gpu_mem/d' "${BOOT_CONFIG}"
            ok "GPU: gpu_mem not set (dedicated GPU memory Pi)"
        fi

        # Ensure vc4-kms-v3d overlay is present for GPU acceleration
        if ! grep -q 'dtoverlay=vc4-kms-v3d' "${BOOT_CONFIG}"; then
            echo "dtoverlay=vc4-kms-v3d" >> "${BOOT_CONFIG}"
        fi

        ok "GPU: vc4-kms-v3d overlay set (${BOOT_CONFIG})"
    else
        warn "No boot config.txt found — skipping GPU optimizations"
    fi
fi

# ── Step 3: Config ───────────────────────────────────────────────────────────
# Priority: CLI flags → existing config.json → defaults
EXISTING_CONFIG="${INSTALL_DIR}/config.json"

if [[ -f "${EXISTING_CONFIG}" ]] && command -v python3 &>/dev/null; then
    [[ -z "${CONF_NAME}" ]]       && CONF_NAME=$(python3 -c "import json; d=json.load(open('${EXISTING_CONFIG}')); print(d.get('name',''))" 2>/dev/null || true)
    [[ -z "${CONF_LOCATION}" ]]   && CONF_LOCATION=$(python3 -c "import json; d=json.load(open('${EXISTING_CONFIG}')); print(d.get('location',''))" 2>/dev/null || true)
    [[ -z "${CONF_CONTROLLER}" ]] && CONF_CONTROLLER=$(python3 -c "import json; d=json.load(open('${EXISTING_CONFIG}')); print(d.get('controller',''))" 2>/dev/null || true)
    [[ -z "${CONF_URL}" ]]        && CONF_URL=$(python3 -c "import json; d=json.load(open('${EXISTING_CONFIG}')); print(d.get('url',''))" 2>/dev/null || true)
fi
CONF_URL="${CONF_URL:-https://edugo.be}"

info "Config: name='${CONF_NAME}' location='${CONF_LOCATION}' controller='${CONF_CONTROLLER}' url='${CONF_URL}'"

cat > "${INSTALL_DIR}/config.json" << CONFEOF
{
  "name": "${CONF_NAME}",
  "location": "${CONF_LOCATION}",
  "controller": "${CONF_CONTROLLER}",
  "url": "${CONF_URL}",
  "fullscreen": true,
  "frame": false,
  "zoomFactor": 1.0
}
CONFEOF
chown "${KIOSK_USER}:${KIOSK_USER}" "${INSTALL_DIR}/config.json"
ok "Config written to ${INSTALL_DIR}/config.json"

# ── Step 4: Systemd service ─────────────────────────────────────────────────
# Ensure XDG_RUNTIME_DIR exists for Wayland
KIOSK_UID=$(id -u "${KIOSK_USER}")
XDG_RUNTIME_DIR="/run/user/${KIOSK_UID}"
mkdir -p "${XDG_RUNTIME_DIR}"
chown "${KIOSK_USER}:${KIOSK_USER}" "${XDG_RUNTIME_DIR}"
chmod 700 "${XDG_RUNTIME_DIR}"

info "Installing systemd service..."
cat > /etc/systemd/system/rpi-infodisplay.service << EOF
[Unit]
Description=Edugo Kiosk Display (Tauri)
After=seatd.service network-online.target
Wants=seatd.service network-online.target

[Service]
Type=simple
WorkingDirectory=${INSTALL_DIR}
Environment=WEBKIT_DISABLE_DMABUF_RENDERER=1
Environment=XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR}
Environment=WLR_LIBINPUT_NO_DEVICES=1
ExecStartPre=/bin/sleep 3
ExecStart=/usr/bin/cage -d -- ${INSTALL_DIR}/rpi-infodisplay
Restart=on-failure
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=kiosk-display

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
ok "Systemd service installed."

# ── Step 6: Enable services ─────────────────────────────────────────────────
systemctl enable --now seatd.service 2>/dev/null || true
systemctl enable rpi-infodisplay.service
ok "Services enabled"

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
