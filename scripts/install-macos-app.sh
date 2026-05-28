#!/usr/bin/env bash
set -euo pipefail

APP_NAME="Posture Watcher"
LAUNCH_AGENT_LABEL="local.posture-watcher"
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BUILT_APP="$ROOT_DIR/target/macos/${APP_NAME}.app"
OPEN_AFTER_INSTALL=0
LAUNCH_AT_LOGIN=0
BUILD_APP=1

usage() {
  printf 'Usage: %s [--open] [--launch-at-login] [--no-build]\n' "$(basename "$0")"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --open)
      OPEN_AFTER_INSTALL=1
      ;;
    --launch-at-login)
      LAUNCH_AT_LOGIN=1
      ;;
    --no-build)
      BUILD_APP=0
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
  shift
done

choose_install_dir() {
  if [[ -n "${POSTURE_WATCHER_INSTALL_DIR:-}" ]]; then
    printf '%s\n' "$POSTURE_WATCHER_INSTALL_DIR"
    return
  fi
  if [[ -w "/Applications" ]]; then
    printf '%s\n' "/Applications"
  else
    printf '%s\n' "$HOME/Applications"
  fi
}

install_launch_agent() {
  local app_path="$1"
  local agents_dir="$HOME/Library/LaunchAgents"
  local plist="$agents_dir/${LAUNCH_AGENT_LABEL}.plist"
  mkdir -p "$agents_dir"
  cat > "$plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>${LAUNCH_AGENT_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/bin/open</string>
    <string>${app_path}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
PLIST
  launchctl bootout "gui/$(id -u)" "$plist" >/dev/null 2>&1 || true
  launchctl bootstrap "gui/$(id -u)" "$plist"
  printf 'Launch-at-login enabled: %s\n' "$plist"
}

cd "$ROOT_DIR"

if [[ "$BUILD_APP" -eq 1 ]]; then
  scripts/build-macos-app.sh
fi

if [[ ! -d "$BUILT_APP" ]]; then
  printf 'Built app not found: %s\n' "$BUILT_APP" >&2
  exit 1
fi

INSTALL_DIR="$(choose_install_dir)"
mkdir -p "$INSTALL_DIR"
INSTALLED_APP="$INSTALL_DIR/${APP_NAME}.app"

osascript -e "tell application \"${APP_NAME}\" to quit" >/dev/null 2>&1 || true
sleep 1

rm -rf "$INSTALLED_APP"
ditto "$BUILT_APP" "$INSTALLED_APP"
xattr -dr com.apple.quarantine "$INSTALLED_APP" >/dev/null 2>&1 || true

printf 'Installed %s\n' "$INSTALLED_APP"

if [[ "$LAUNCH_AT_LOGIN" -eq 1 ]]; then
  install_launch_agent "$INSTALLED_APP"
else
  printf 'Launch at login is off. Enable it with:\n'
  printf '  scripts/install-macos-app.sh --launch-at-login\n'
fi

if [[ "$OPEN_AFTER_INSTALL" -eq 1 ]]; then
  open "$INSTALLED_APP"
fi
