#!/usr/bin/env bash
set -euo pipefail

CAMERA_NAME="${1:-Logitech Webcam C930e}"
OUT_DIR="artifacts/captures"
OUT_FILE="$OUT_DIR/permission-probe.jpg"

mkdir -p "$OUT_DIR"

cat <<'MSG'
macOS camera access cannot be granted silently from a shell script.

This helper opens the Camera privacy pane and then runs the same capture tool
used by posture-watcher so macOS can show the permission prompt.

In System Settings > Privacy & Security > Camera, enable camera access for:
- Codex, if it appears there
- Terminal or iTerm, if you run the app manually there
- imagesnap, if macOS lists it separately
- ffmpeg, if macOS lists it separately

If no prompt appears and access is still denied, run:
  tccutil reset Camera
then run this script again and approve the prompt.

MSG

open "x-apple.systempreferences:com.apple.preference.security?Privacy_Camera" || true

if ! command -v imagesnap >/dev/null 2>&1; then
  echo "imagesnap is not installed. Install it with: brew install imagesnap" >&2
  exit 1
fi

echo "Available cameras:"
imagesnap -l || true

echo
echo "Trying one capture from: $CAMERA_NAME"
echo "If macOS prompts for Camera access, approve it, then rerun this script."

if imagesnap -d "$CAMERA_NAME" "$OUT_FILE"; then
  echo "Camera capture succeeded: $OUT_FILE"
  echo "You can now run:"
  echo "  cargo run -- live --camera \"$CAMERA_NAME\""
else
  echo
  echo "Camera capture is still blocked."
  echo "Open the Camera privacy pane that was just opened and enable the relevant app."
  echo "If the app is not listed, reset Camera permissions with:"
  echo "  tccutil reset Camera"
  echo "then rerun this script and approve the prompt."
  exit 1
fi
