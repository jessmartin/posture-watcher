#!/usr/bin/env bash
set -euo pipefail

LOG_FILE="$HOME/Library/Application Support/Posture Watcher/posture-watcher.log"

if [[ ! -f "$LOG_FILE" ]]; then
  echo "No Posture Watcher log yet: $LOG_FILE" >&2
  echo "Launch the app first: open \"target/macos/Posture Watcher.app\"" >&2
  exit 1
fi

tail -f "$LOG_FILE"
