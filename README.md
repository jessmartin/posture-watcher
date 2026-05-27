# Posture Watcher

Posture Watcher is a small end-to-end posture feedback loop:

1. A side-mounted webcam watches AprilTags on the ear, C7, shoulder, and optional hip.
2. A Rust analyzer turns those tags into marker geometry, placement diagnostics, and a simple spine/head curve.
3. A Badger2040 e-ink display, used in portrait orientation, shows the feedback where it is easy to glance at while working.
4. A native macOS wrapper owns Camera permission, shows the same display as the Badger, and gives debugging controls when the hardware is not nearby.

The goal is not to nag on every frame. The app samples slowly, averages over a rolling window, and refuses to show a posture curve when the markers are visible but anatomically implausible.

<p align="center">
  <img src="docs/screenshots/posture-watcher-check-markers.svg" width="340" alt="Posture Watcher app showing placement check and Check markers warning">
</p>

## Current State

The live loop is working with the plugged-in Logitech C930e and Badger2040:

- The macOS app captures frames through AVFoundation.
- The Rust analyzer detects AprilTags and writes debug images/reports.
- The Badger receiver ACKs messages over USB serial.
- The app distinguishes `Tags ready` from `Placement check`.
- When marker geometry is implausible, the Badger shows `Check markers` instead of a misleading curve.

The remaining calibration work is physical: place the tags on the actual landmarks, collect sitting and standing samples, then tune the sitting/standing and placement heuristics against those examples.

## Gallery

| Badger in action | Wearing the tags |
| --- | --- |
| <img src="docs/screenshots/badger-photo-placeholder.svg" width="320" alt="Placeholder for Badger in action photo"> | <img src="docs/screenshots/apriltag-wearing-placeholder.svg" width="320" alt="Placeholder for photo of wearing AprilTags"> |

The two placeholder images above are intentional. Drop in real photos after the next physical test.

## Hardware

- Badger2040 connected over USB-C.
- Serial port defaults to `/dev/cu.usbmodem83201`.
- Logitech Webcam C930e, mounted sideways for maximum vertical image height.
- AprilTags from the `tag36h11` family.
- Badger should be used vertically, on its short edge, so the 296px axis maps to the body axis.

## First Setup

Build the Rust CLI:

```sh
cargo build
```

Install the Badger receiver:

```sh
cargo run -- install-badger
```

The installer backs up the current Badger `main.py` into `artifacts/badger-backups/`.

Build and launch the macOS app:

```sh
scripts/build-macos-app.sh
open "target/macos/Posture Watcher.app"
```

macOS should prompt for Camera permission as `Posture Watcher`. The app is the preferred daily entry point because it owns the Camera permission flow and feeds captured frames to the bundled Rust analyzer.

## Daily Loop

1. Print the AprilTag sheet:

   ```sh
   cargo run -- stickers --open
   ```

2. Put the tags on:

   - `tag36h11-0`: ear / tragus region
   - `tag36h11-1`: C7
   - `tag36h11-2`: shoulder / acromion
   - `tag36h11-3`: hip / belt marker, optional but useful for sitting/standing

3. Launch the app and check the status rows:

   - `Badger connected` means the e-ink receiver is ACKing payloads.
   - `Tags ready` means the required tags are visible.
   - `Placement good` means the marker geometry is plausible enough to show a curve.
   - `Placement check` means the tags are visible but probably wrong.

4. Use `Save Sample` whenever you have a useful sitting, standing, good, or bad setup. Samples are saved under:

   ```text
   ~/Library/Application Support/Posture Watcher/samples/<mode>/
   ```

Each saved sample includes the raw frame, debug images, and a `*-tags.txt` report with marker coordinates, detected mode, placement score, and posture measurements.

## Safety Rails

Posture feedback is only useful if the markers are trustworthy. The analyzer therefore keeps tag visibility separate from marker plausibility.

If tags are missing for long enough, the Badger says:

```text
No person found
```

If tags are visible but the geometry is implausible, the Badger says:

```text
Check markers
```

That prevents the e-ink display from encouraging posture changes based on bad marker placement. Current placement checks include things like whether the ear marker is actually above C7 and whether the ear-to-C7 angle is geometrically plausible.

## Sitting vs Standing

The macOS app shows a first-pass auto-detected Sitting/Standing estimate when shoulder and hip tags are visible. The current heuristic treats a mostly vertical shoulder-to-hip axis as standing, a mostly horizontal shoulder-to-hip axis as sitting, and ambiguous or missing hip geometry as unknown.

Keep using the Mode picker as the ground-truth label while saving samples. The saved `latest-tags.txt` reports include:

```text
detected_mode=sitting
detected_mode_confidence=95
placement_status=check
placement_detail=ear not above C7; ear-C7 angle implausible 1deg
```

## Debugging

Watch the app log:

```sh
scripts/watch-macos-app-log.sh
```

Analyze the current app frame without starting the live loop:

```sh
cargo run -- snapshot \
  --input "$HOME/Library/Application Support/Posture Watcher/latest-frame.jpg" \
  --rotate ccw90 \
  --out-dir artifacts/snapshot
```

Run the full diagnostic:

```sh
cargo run -- doctor
```

Expected checks:

- C930e appears in the camera list.
- A one-frame capture succeeds, or a fresh app frame exists.
- Badger receiver answers `OK,POSTURE_WATCHER_BADGER_V2`.
- Tagged sample analysis detects the posture tags.

## CLI Toolbox

Generate fake tagged samples from `sample-images/`:

```sh
cargo run -- annotate-samples
```

Run the sample sequence and send curves to the Badger:

```sh
cargo run -- run-samples --send-badger
```

Run direct live capture from the CLI:

```sh
cargo run -- live --camera "Logitech Webcam C930e" --port /dev/cu.usbmodem83201
```

Useful live flags:

```sh
cargo run -- live --capture-backend imagesnap
cargo run -- live --capture-backend ffmpeg --ffmpeg-input "0:none"
cargo run -- live --capture-timeout-secs 5
cargo run -- live --rotate none
```

Restore the original Badger launcher:

```sh
cargo run -- restore-badger
```

## Notes

Live mode defaults to `--rotate ccw90` because the camera is mounted on its side, `--interval-secs 5`, and a 120-second rolling average. Set `POSTURE_WATCHER_NO_BADGER=1` to use only the macOS preview window.

Optional app overrides:

```sh
POSTURE_WATCHER_CAMERA="Logitech Webcam C930e" \
POSTURE_WATCHER_PORT="/dev/cu.usbmodem83201" \
POSTURE_WATCHER_INTERVAL_SECS=5 \
POSTURE_WATCHER_NO_PERSON_AFTER_SECS=30 \
POSTURE_WATCHER_ROTATE=ccw90 \
open "target/macos/Posture Watcher.app"
```
