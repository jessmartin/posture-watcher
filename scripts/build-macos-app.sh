#!/usr/bin/env bash
set -euo pipefail

APP_NAME="Posture Watcher"
BUNDLE_ID="local.posture-watcher"
APP_DIR="target/macos/${APP_NAME}.app"
CONTENTS="$APP_DIR/Contents"
MACOS="$CONTENTS/MacOS"
RESOURCES="$CONTENTS/Resources"
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

cd "$ROOT_DIR"
cargo build

rm -rf "$APP_DIR"
mkdir -p "$MACOS" "$RESOURCES"

swiftc macos/PostureWatcherLauncher.swift \
  -framework AppKit \
  -framework AVFoundation \
  -o "$MACOS/PostureWatcherLauncher"

cp target/debug/posture-watcher "$RESOURCES/posture-watcher"
chmod +x "$RESOURCES/posture-watcher"

cat > "$CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleExecutable</key>
  <string>PostureWatcherLauncher</string>
  <key>CFBundleIdentifier</key>
  <string>${BUNDLE_ID}</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>${APP_NAME}</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleVersion</key>
  <string>1</string>
  <key>LSMinimumSystemVersion</key>
  <string>14.0</string>
  <key>NSCameraUsageDescription</key>
  <string>Posture Watcher uses the Logitech webcam to detect your posture AprilTags and display a rolling posture curve on the Badger2040.</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

codesign --force --deep --sign - "$APP_DIR" >/dev/null

echo "Built $APP_DIR"
echo "Run it with:"
echo "  open \"$APP_DIR\""
