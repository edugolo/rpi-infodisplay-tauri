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
# Display power management:
#   - The app tries HDMI-CEC (via cec-ctl) first. If the TV supports CEC,
#     it sends a Standby command to gracefully turn off the display.
#   - If CEC is unavailable, the app stops its systemd service, which
#     cleanly terminates cage + the Tauri process. The display goes dark.
#   - A systemd start timer wakes the display on schedule (default:
#     Mon–Fri 09:00). The app also sends a CEC Image View On on startup
#     if CEC is available.
#   - A backup stop timer runs 5 minutes after the off-time in case the
#     app's self-shutdown fails.
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
SCHEDULE_ON="09:00"
SCHEDULE_OFF="16:00"

# ── Parse arguments ──────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case $1 in
        --version|-v)       VERSION="$2"; shift 2 ;;
        --latest)           VERSION="latest"; shift ;;
        --binary)           BINARY_SOURCE="$2"; shift 2 ;;
        --dir)              INSTALL_DIR="$2"; shift 2 ;;
        --user)             KIOSK_USER="$2"; shift 2 ;;
        --name)             CONF_NAME="$2"; shift 2 ;;
        --location)         CONF_LOCATION="$2"; shift 2 ;;
        --controller)       CONF_CONTROLLER="$2"; shift 2 ;;
        --url)              CONF_URL="$2"; shift 2 ;;
        --on-time)          SCHEDULE_ON="$2"; shift 2 ;;
        --off-time)         SCHEDULE_OFF="$2"; shift 2 ;;
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
            echo "  --on-time HH:MM    Display on time (default: 09:00)"
            echo "  --off-time HH:MM   Display off time (default: 16:00)"
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

    # GPU acceleration (Mesa VC4/V3D DRI driver)
    mesa-utils libgl1-mesa-dri libegl1 libgles2

    # HDMI-CEC control (kernel API via cec-ctl)
    cec-utils

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
info "Schedule: on=${SCHEDULE_ON} off=${SCHEDULE_OFF} (weekdays)"

cat > "${INSTALL_DIR}/config.json" << CONFEOF
{
  "name": "${CONF_NAME}",
  "location": "${CONF_LOCATION}",
  "controller": "${CONF_CONTROLLER}",
  "url": "${CONF_URL}",
  "fullscreen": true,
  "frame": false,
  "zoomFactor": 1.0,
  "displaySchedule": {
    "enabled": true,
    "on": "${SCHEDULE_ON}",
    "off": "${SCHEDULE_OFF}",
    "days": ["mon", "tue", "wed", "thu", "fri"]
  }
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
cat > /etc/systemd/system/rpi-infodisplay.service << SERVICEEOF
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
SERVICEEOF
ok "Systemd service installed."

# ── Step 5: Systemd timer units ──────────────────────────────────────────────
#
# The app has an internal scheduler that respects the config-based schedule
# (which can be updated remotely). At the scheduled off-time, the app sends
# CEC Standby (if available) and then calls "systemctl stop" on itself.
#
# The systemd timers below provide:
#   a) A reliable wake-up at the scheduled on-time (the start timer).
#   b) A backup shutdown 5 minutes after the off-time in case the app's
#      self-shutdown didn't fire (e.g., the app crashed mid-day).

# Parse HH:MM into a systemd OnCalendar weekday expression
# Turns "09:00" into "Mon..Fri 09:00:00"
on_hour="${SCHEDULE_ON%%:*}"
on_min="${SCHEDULE_ON##*:}"
off_hour="${SCHEDULE_OFF%%:*}"
off_min="${SCHEDULE_OFF##*:}"

# Backup stop timer: 5 minutes after the off-time
backup_off_min=$((10#${off_min} + 5))
backup_off_hour="${off_hour}"
if [[ "${backup_off_min}" -ge 60 ]]; then
    backup_off_min=$((backup_off_min - 60))
    backup_off_hour=$((10#${off_hour} + 1))
fi
backup_off_min=$(printf "%02d" "${backup_off_min}")
backup_off_hour=$(printf "%02d" "${backup_off_hour}")

info "Installing systemd timer units..."

# Start timer
cat > /etc/systemd/system/rpi-infodisplay-start.timer << TIMEREOF
[Unit]
Description=Start rpi-infodisplay on schedule (weekdays)

[Timer]
OnCalendar=Mon..Fri ${on_hour}:${on_min}:00
Persistent=true
Unit=rpi-infodisplay.service

[Install]
WantedBy=timers.target
TIMEREOF
ok "Start timer: Mon..Fri ${on_hour}:${on_min}"

# Stop service (one-shot, called by the stop timer)
cat > /etc/systemd/system/rpi-infodisplay-stop.service << SERVICEEOF
[Unit]
Description=Stop rpi-infodisplay (backup for scheduled shutdown)

# Also try CEC standby as a last-resort fallback, in case the app
# was unable to send it before self-stopping.
[Service]
Type=oneshot
ExecStart=/usr/bin/systemctl stop rpi-infodisplay
ExecStartPost=/usr/bin/cec-ctl --device /dev/cec0 --standby 2>/dev/null || true
RemainAfterExit=no
SERVICEEOF

# Stop timer (backup, 5 minutes after scheduled off-time)
cat > /etc/systemd/system/rpi-infodisplay-stop.timer << TIMEREOF
[Unit]
Description=Stop rpi-infodisplay on schedule (backup)

[Timer]
OnCalendar=Mon..Fri ${backup_off_hour}:${backup_off_min}:00

[Install]
WantedBy=timers.target
TIMEREOF
ok "Stop backup timer: Mon..Fri ${backup_off_hour}:${backup_off_min}"

systemctl daemon-reload
ok "Timer units installed."

# ── Step 6: Enable services & timers ────────────────────────────────────────
systemctl enable --now seatd.service 2>/dev/null || true
systemctl enable rpi-infodisplay.service
systemctl enable rpi-infodisplay-start.timer
systemctl enable rpi-infodisplay-stop.timer
ok "Services and timers enabled"

# ── Step 7: Pre-warm CEC on first install ────────────────────────────────────
if command -v cec-ctl &>/dev/null && [[ -c /dev/cec0 ]]; then
    info "Configuring CEC logical address..."
    # The VC4 HDMI driver claims /dev/cec0 automatically. We pre-configure
    # a "Playback Device" identity so the Pi shows up on the TV's CEC menu.
    # This is persisted by the kernel driver via udev / config.
    cec-ctl --device /dev/cec0 --playback 2>/dev/null || true
    ok "CEC initialised (playback device)"
fi

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
echo "  Display schedule: ${SCHEDULE_ON} – ${SCHEDULE_OFF} (weekdays)"
echo "  Power control:    CEC (cec-ctl) → service stop"
echo ""
echo -e "${YELLOW}  Reboot to activate: sudo reboot${NC}"
echo ""
