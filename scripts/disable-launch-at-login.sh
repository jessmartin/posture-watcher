#!/usr/bin/env bash
set -euo pipefail

LABEL="local.posture-watcher"
PLIST="$HOME/Library/LaunchAgents/${LABEL}.plist"

launchctl bootout "gui/$(id -u)" "$PLIST" >/dev/null 2>&1 || true
rm -f "$PLIST"

printf 'Launch-at-login disabled for Posture Watcher.\n'
