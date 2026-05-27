# Posture Watcher

Rust host app plus a Badger2040 MicroPython receiver for AprilTag-based posture feedback.

## Current Device Notes

- Badger2040 is expected at `/dev/cu.usbmodem83201`.
- CLI camera capture can use `imagesnap` with `Logitech Webcam C930e`; the macOS app can select any detected AVFoundation camera.
- If live capture says `Camera access not granted`, grant Camera permission to the app/terminal running the command in macOS Privacy & Security settings.
- Badger writes are ACK-verified. A successful send prints replies like `OK,P,4`.
- The Badger display is portrait-first: place it on its short edge so the posture curve uses the 296px dimension vertically.
- Live capture defaults to `--capture-backend auto`, which tries `imagesnap` first and then an `ffmpeg` AVFoundation fallback. Both still require macOS Camera permission.
- For the most reliable Camera permission flow, build and launch the macOS app wrapper. The wrapper captures frames with AVFoundation inside the named app bundle, then feeds those images to the Rust analyzer.

## Setup

Build the Rust app:

```sh
cargo build
```

Install the Badger receiver:

```sh
cargo run -- install-badger
```

The installer backs up the current Badger `main.py` in `artifacts/badger-backups/`.

Grant camera permission:

```sh
scripts/request-camera-permission.sh
```

macOS will not allow a script to silently grant Camera access. If the script still reports `Camera access not granted`, enable Camera access for Codex, Terminal/iTerm, or `imagesnap` in `System Settings > Privacy & Security > Camera`. If nothing relevant appears, run `tccutil reset Camera`, rerun the script, and approve the prompt.

## macOS App

Build the app bundle:

```sh
scripts/build-macos-app.sh
```

Launch it:

```sh
open "target/macos/Posture Watcher.app"
```

The app should prompt for Camera permission as `Posture Watcher`. It opens a small preview window that mirrors the Badger display, shows Badger connection and visible-tag status, lets you choose the camera, captures frames natively, and runs the bundled Rust binary in `live-file` mode. Runtime outputs go under `~/Library/Application Support/Posture Watcher/`.

Watch the app log:

```sh
scripts/watch-macos-app-log.sh
```

After approving the Camera prompt, the log should show camera selection, 5-second frame writes, analyzer startup, preview payloads, and Badger ACKs when a Badger is connected.

Optional environment overrides:

```sh
POSTURE_WATCHER_CAMERA="Logitech Webcam C930e" \
POSTURE_WATCHER_PORT="/dev/cu.usbmodem83201" \
POSTURE_WATCHER_INTERVAL_SECS=5 \
POSTURE_WATCHER_NO_PERSON_AFTER_SECS=30 \
POSTURE_WATCHER_ROTATE=ccw90 \
open "target/macos/Posture Watcher.app"
```

Set `POSTURE_WATCHER_NO_BADGER=1` to use only the macOS preview window.

## Doctor

Run the diagnostic:

```sh
cargo run -- doctor
```

Expected checks:

- C930e appears in the camera list.
- A one-frame C930e capture succeeds, or a fresh `Posture Watcher.app` frame is present.
- Badger receiver answers `OK,POSTURE_WATCHER_BADGER_V2`.
- Tagged sample analysis detects the posture tags.

On macOS, CLI tools like `imagesnap` can remain blocked even when the app bundle has Camera permission. In that case, launch `target/macos/Posture Watcher.app`; `doctor` will accept its recent `latest-frame.jpg` as the camera check.

## Quick Test

```sh
cargo run -- stickers --open
cargo run -- annotate-samples
cargo run -- run-samples --send-badger
```

Restore the original Badger launcher:

```sh
cargo run -- restore-badger
```

## Live Mode

```sh
cargo run -- live --camera "Logitech Webcam C930e" --port /dev/cu.usbmodem83201
```

The displayed curve is meant for a physically vertical Badger. The 296px axis is the body axis; the 128px axis shows forward/back drift from the reference line.

Live mode defaults to `--rotate ccw90` because the camera is mounted on its side, `--interval-secs 5`, and a 120-second rolling average. If the required tags are missing for 30 seconds, the Badger and macOS preview show `No person found`.

Useful live-capture flags:

```sh
cargo run -- live --capture-backend imagesnap
cargo run -- live --capture-backend ffmpeg --ffmpeg-input "0:none"
cargo run -- live --capture-timeout-secs 5
cargo run -- live --rotate none
```

The CLI live mode is useful for debugging, but the macOS app wrapper is preferred for day-to-day use because it owns the Camera permission prompt.

## Sticker Meaning

- `tag36h11-0`: ear / tragus
- `tag36h11-1`: C7
- `tag36h11-2`: shoulder / acromion
- `tag36h11-3`: optional hip / belt marker

## Sitting vs Standing Mode

Do not rely on a hard-coded sitting/standing classifier yet. With ear/C7/shoulder tags only, the image usually cannot prove whether you are sitting or standing; the hip marker and camera framing are what make it possible. The practical path is to add a manual mode override, collect tagged frames in both modes, and then auto-classify from calibrated marker geometry once the camera position and sticker placement are stable.
