use anyhow::{anyhow, bail, ensure, Context, Result};
use apriltag::{Detector, Family};
use apriltag_image::prelude::*;
use clap::{Parser, Subcommand, ValueEnum};
use image::imageops::{overlay, FilterType};
#[cfg(test)]
use image::GrayImage;
use image::{DynamicImage, Rgba, RgbaImage};
use imageproc::drawing::{draw_filled_rect_mut, draw_line_segment_mut};
use imageproc::rect::Rect;
use kornia_apriltag::family::{TagFamily, TagFamilyKind};
use serialport::SerialPort;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

const BADGER_WIDTH: i32 = 296;
const BADGER_HEIGHT: i32 = 128;
const BADGER_PROTOCOL: &str = "POSTURE_WATCHER_BADGER_V2";
const DEFAULT_LIVE_INTERVAL_SECS: u64 = 15;
const DEFAULT_NO_PERSON_AFTER_SECS: u64 = 60;
const DEFAULT_BURST_FRAMES: usize = 8;
const QUALITY_HISTORY_CAPACITY: usize = 16;
const MESSAGE_AFTER_CONSECUTIVE_MISSES: usize = 4;
const NO_PERSON_MESSAGE: &str = "No person found";
const CHECK_MARKERS_MESSAGE: &str = "Check markers";

const EAR_ID: usize = 0;
const C7_ID: usize = 1;
const SHOULDER_ID: usize = 2;
const HIP_ID: usize = 3;

#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Generate printable AprilTag stickers for ear/C7/shoulder/hip.
    Stickers {
        #[arg(long, default_value = "artifacts/stickers/posture-tags.svg")]
        out: PathBuf,
        #[arg(long, default_value_t = 18.0)]
        tag_mm: f32,
        #[arg(long)]
        open: bool,
    },
    /// Add fake AprilTags to the sample images for repeatable test data.
    AnnotateSamples {
        #[arg(long, default_value = "sample-images")]
        input_dir: PathBuf,
        #[arg(long, default_value = "artifacts/tagged-samples")]
        out_dir: PathBuf,
        #[arg(long, default_value_t = 120)]
        tag_px: u32,
    },
    /// Analyze one tagged image and optionally write an annotated debug image.
    Analyze {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        annotated_out: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = FrameRotation::None)]
        rotate: FrameRotation,
        #[arg(long, default_value_t = 0.0)]
        c7_anchor_offset_tag_widths: f64,
        #[arg(long)]
        send_badger: bool,
        #[arg(long, default_value = "/dev/cu.usbmodem83201")]
        port: String,
        #[arg(long, value_enum, default_value_t = BadgerOrientation::UsbTop)]
        badger_orientation: BadgerOrientation,
    },
    /// Analyze one frame and write latest-tags.png, latest-tags.txt, and latest-analysis.png.
    Snapshot {
        #[arg(long)]
        input: PathBuf,
        #[arg(long, default_value = "artifacts/snapshot")]
        out_dir: PathBuf,
        #[arg(long, value_enum, default_value_t = FrameRotation::None)]
        rotate: FrameRotation,
        #[arg(long, default_value_t = 0.0)]
        c7_anchor_offset_tag_widths: f64,
        #[arg(long)]
        send_badger: bool,
        #[arg(long, default_value = "/dev/cu.usbmodem83201")]
        port: String,
        #[arg(long, value_enum, default_value_t = BadgerOrientation::UsbTop)]
        badger_orientation: BadgerOrientation,
    },
    /// Analyze all tagged sample images in order, write debug images, and optionally send to Badger.
    RunSamples {
        #[arg(long, default_value = "artifacts/tagged-samples")]
        input_dir: PathBuf,
        #[arg(long, default_value = "artifacts/analysis")]
        out_dir: PathBuf,
        #[arg(long, value_enum, default_value_t = FrameRotation::None)]
        rotate: FrameRotation,
        #[arg(long, default_value_t = 0.0)]
        c7_anchor_offset_tag_widths: f64,
        #[arg(long)]
        send_badger: bool,
        #[arg(long, default_value = "/dev/cu.usbmodem83201")]
        port: String,
        #[arg(long, value_enum, default_value_t = BadgerOrientation::UsbTop)]
        badger_orientation: BadgerOrientation,
        #[arg(long, default_value_t = 120)]
        window_secs: u64,
        #[arg(long, default_value_t = 1500)]
        delay_ms: u64,
    },
    /// Capture frames from the Logitech camera with imagesnap, analyze, and stream rolling averages.
    Live {
        #[arg(long, default_value = "Logitech Webcam C930e")]
        camera: String,
        #[arg(long, value_enum, default_value_t = CaptureBackend::Auto)]
        capture_backend: CaptureBackend,
        #[arg(long, default_value = "0:none")]
        ffmpeg_input: String,
        #[arg(long, default_value_t = 10)]
        capture_timeout_secs: u64,
        #[arg(long, default_value = "/dev/cu.usbmodem83201")]
        port: String,
        #[arg(long, default_value_t = 120)]
        window_secs: u64,
        #[arg(long, default_value_t = DEFAULT_LIVE_INTERVAL_SECS)]
        interval_secs: u64,
        #[arg(long, default_value_t = DEFAULT_NO_PERSON_AFTER_SECS)]
        no_person_after_secs: u64,
        #[arg(long, value_enum, default_value_t = FrameRotation::Ccw90)]
        rotate: FrameRotation,
        #[arg(long, default_value_t = 0.0)]
        c7_anchor_offset_tag_widths: f64,
        #[arg(long, default_value = "artifacts/live")]
        out_dir: PathBuf,
        #[arg(long)]
        baseline: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = DeskModeOverride::Auto)]
        mode: DeskModeOverride,
        #[arg(long, value_enum, default_value_t = BadgerOrientation::UsbTop)]
        badger_orientation: BadgerOrientation,
        #[arg(long)]
        no_badger: bool,
    },
    /// Analyze a repeatedly updated image file, preserving rolling posture state.
    LiveFile {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        burst_dir: Option<PathBuf>,
        #[arg(long, default_value_t = DEFAULT_BURST_FRAMES)]
        burst_frames: usize,
        #[arg(long, default_value = "/dev/cu.usbmodem83201")]
        port: String,
        #[arg(long, default_value_t = 120)]
        window_secs: u64,
        #[arg(long, default_value_t = DEFAULT_LIVE_INTERVAL_SECS)]
        interval_secs: u64,
        #[arg(long, default_value_t = DEFAULT_NO_PERSON_AFTER_SECS)]
        no_person_after_secs: u64,
        #[arg(long, value_enum, default_value_t = FrameRotation::Ccw90)]
        rotate: FrameRotation,
        #[arg(long, default_value_t = 0.0)]
        c7_anchor_offset_tag_widths: f64,
        #[arg(long, default_value = "artifacts/live-file")]
        out_dir: PathBuf,
        #[arg(long)]
        baseline: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = DeskModeOverride::Auto)]
        mode: DeskModeOverride,
        #[arg(long, value_enum, default_value_t = BadgerOrientation::UsbTop)]
        badger_orientation: BadgerOrientation,
        #[arg(long)]
        no_badger: bool,
        #[arg(long)]
        once: bool,
    },
    /// Check camera, Badger, sticker/sample, and serial setup.
    Doctor {
        #[arg(long, default_value = "Logitech Webcam C930e")]
        camera: String,
        #[arg(long, value_enum, default_value_t = CaptureBackend::Auto)]
        capture_backend: CaptureBackend,
        #[arg(long, default_value = "0:none")]
        ffmpeg_input: String,
        #[arg(long, default_value_t = 10)]
        capture_timeout_secs: u64,
        #[arg(long, default_value = "/dev/cu.usbmodem83201")]
        port: String,
        #[arg(long, default_value = "artifacts/doctor")]
        out_dir: PathBuf,
    },
    /// List cameras known to imagesnap.
    ListCameras,
    /// Install the Badger2040 posture receiver as main.py.
    InstallBadger {
        #[arg(long, default_value = "/dev/cu.usbmodem83201")]
        port: String,
    },
    /// Restore the Badger2040 launcher main.py from the latest local backup.
    RestoreBadger {
        #[arg(long, default_value = "/dev/cu.usbmodem83201")]
        port: String,
        #[arg(long)]
        backup: Option<PathBuf>,
    },
    /// Send a deterministic demo curve to the Badger.
    SendDemo {
        #[arg(long, default_value = "/dev/cu.usbmodem83201")]
        port: String,
        #[arg(long, value_enum, default_value_t = DemoPose::Neutral)]
        pose: DemoPose,
        #[arg(long, value_enum, default_value_t = BadgerOrientation::UsbTop)]
        badger_orientation: BadgerOrientation,
    },
    /// Send a status message to the Badger.
    SendStatus {
        #[arg(long, default_value = "/dev/cu.usbmodem83201")]
        port: String,
        #[arg(long, default_value = "No person found")]
        message: String,
        #[arg(long, value_enum, default_value_t = BadgerOrientation::UsbTop)]
        badger_orientation: BadgerOrientation,
    },
    /// Build sitting/standing posture baselines from saved good samples.
    CalibrateBaseline {
        #[arg(long)]
        samples_dir: Option<PathBuf>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, default_value_t = 3)]
        min_samples: usize,
    },
}

#[derive(Debug, Clone, ValueEnum)]
enum DemoPose {
    Neutral,
    Hunched,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CaptureBackend {
    Auto,
    Imagesnap,
    Ffmpeg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum FrameRotation {
    None,
    Cw90,
    Ccw90,
    Deg180,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BadgerOrientation {
    UsbTop,
    UsbBottom,
}

impl BadgerOrientation {
    fn protocol_code(self) -> Option<&'static str> {
        match self {
            Self::UsbTop => None,
            Self::UsbBottom => Some("B"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DetectedDeskMode {
    Sitting,
    Standing,
    Unknown,
}

impl DetectedDeskMode {
    fn label(self) -> &'static str {
        match self {
            Self::Sitting => "sitting",
            Self::Standing => "standing",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum DeskModeOverride {
    Auto,
    Sitting,
    Standing,
}

impl DeskModeOverride {
    fn estimate(self, detections: &[DetectionPoint]) -> ModeEstimate {
        match self {
            Self::Auto => estimate_mode_from_detections(detections),
            Self::Sitting => ModeEstimate::new(
                DetectedDeskMode::Sitting,
                100,
                "manual override from Mode picker",
            ),
            Self::Standing => ModeEstimate::new(
                DetectedDeskMode::Standing,
                100,
                "manual override from Mode picker",
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlacementStatus {
    Good,
    Check,
    Missing,
}

impl PlacementStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Good => "good",
            Self::Check => "check",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlacementEstimate {
    status: PlacementStatus,
    score: u8,
    action: String,
    detail: String,
}

impl PlacementEstimate {
    fn new(
        status: PlacementStatus,
        score: u8,
        action: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            status,
            score,
            action: action.into(),
            detail: detail.into(),
        }
    }

    fn good(detail: impl Into<String>) -> Self {
        Self::new(PlacementStatus::Good, 100, "Ready", detail)
    }

    fn check(score: u8, action: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(PlacementStatus::Check, score, action, detail)
    }

    fn missing_with_action(action: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(PlacementStatus::Missing, 0, action, detail)
    }

    fn is_good(&self) -> bool {
        self.status == PlacementStatus::Good
    }

    fn badger_message(&self) -> &str {
        if self.action.is_empty() {
            CHECK_MARKERS_MESSAGE
        } else {
            &self.action
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModeEstimate {
    mode: DetectedDeskMode,
    confidence: u8,
    detail: String,
}

impl ModeEstimate {
    fn new(mode: DetectedDeskMode, confidence: u8, detail: impl Into<String>) -> Self {
        Self {
            mode,
            confidence,
            detail: detail.into(),
        }
    }

    fn unknown(detail: impl Into<String>) -> Self {
        Self::new(DetectedDeskMode::Unknown, 0, detail)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Point {
    x: f64,
    y: f64,
}

impl Point {
    fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone)]
struct DetectionPoint {
    id: usize,
    center: Point,
    corners: [[f64; 2]; 4],
}

#[derive(Debug, Clone)]
struct PostureFrame {
    landmarks: BTreeMap<usize, Point>,
    display_points: Vec<(usize, Point)>,
    detected_count: usize,
    cva_degrees: Option<f64>,
    head_forward_px: Option<f64>,
}

#[derive(Debug)]
struct RollingWindow {
    window: Duration,
    frames: VecDeque<(Instant, PostureFrame)>,
}

#[derive(Debug)]
struct ModeRollingWindows {
    window: Duration,
    frames_by_mode: BTreeMap<DetectedDeskMode, RollingWindow>,
}

#[derive(Debug)]
struct MissingPersonState {
    first_missing: Option<Instant>,
    message_sent: bool,
}

#[derive(Debug)]
struct QualityHistory {
    entries: VecDeque<bool>,
    capacity: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameAnalysisOutcome {
    Posture,
    MissingRequiredTags,
    InvalidPlacement,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Stickers { out, tag_mm, open } => {
            generate_stickers(&out, tag_mm)?;
            if open {
                open_file(&out)?;
            }
            Ok(())
        }
        Commands::AnnotateSamples {
            input_dir,
            out_dir,
            tag_px,
        } => annotate_samples(&input_dir, &out_dir, tag_px),
        Commands::Analyze {
            input,
            annotated_out,
            rotate,
            c7_anchor_offset_tag_widths,
            send_badger,
            port,
            badger_orientation,
        } => {
            let img =
                image::open(&input).with_context(|| format!("opening {}", input.display()))?;
            let img = apply_rotation(img, rotate);
            let detections = detect_tags(&img)?;
            let detections = apply_c7_anchor_correction(&detections, c7_anchor_offset_tag_widths);
            let posture = posture_from_detections(&detections)?;
            print_posture(&input, &posture, None);
            if let Some(out) = annotated_out {
                write_debug_image(&img, &detections, &posture, &out)?;
                println!("wrote {}", out.display());
            }
            if send_badger {
                send_to_badger(&port, &posture, badger_orientation)?;
            }
            Ok(())
        }
        Commands::Snapshot {
            input,
            out_dir,
            rotate,
            c7_anchor_offset_tag_widths,
            send_badger,
            port,
            badger_orientation,
        } => snapshot_frame(
            &input,
            &out_dir,
            rotate,
            c7_anchor_offset_tag_widths,
            send_badger,
            &port,
            badger_orientation,
        ),
        Commands::RunSamples {
            input_dir,
            out_dir,
            rotate,
            c7_anchor_offset_tag_widths,
            send_badger,
            port,
            badger_orientation,
            window_secs,
            delay_ms,
        } => run_samples(
            &input_dir,
            &out_dir,
            rotate,
            c7_anchor_offset_tag_widths,
            send_badger,
            &port,
            badger_orientation,
            window_secs,
            delay_ms,
        ),
        Commands::Live {
            camera,
            capture_backend,
            ffmpeg_input,
            capture_timeout_secs,
            port,
            window_secs,
            interval_secs,
            no_person_after_secs,
            rotate,
            c7_anchor_offset_tag_widths,
            out_dir,
            baseline,
            mode,
            badger_orientation,
            no_badger,
        } => live(
            &camera,
            capture_backend,
            &ffmpeg_input,
            Duration::from_secs(capture_timeout_secs),
            &port,
            window_secs,
            interval_secs,
            Duration::from_secs(no_person_after_secs),
            rotate,
            c7_anchor_offset_tag_widths,
            &out_dir,
            baseline.as_deref(),
            mode,
            badger_orientation,
            !no_badger,
        ),
        Commands::LiveFile {
            input,
            burst_dir,
            burst_frames,
            port,
            window_secs,
            interval_secs,
            no_person_after_secs,
            rotate,
            c7_anchor_offset_tag_widths,
            out_dir,
            baseline,
            mode,
            badger_orientation,
            no_badger,
            once,
        } => live_file(
            &input,
            burst_dir.as_deref(),
            burst_frames,
            &port,
            window_secs,
            interval_secs,
            Duration::from_secs(no_person_after_secs),
            rotate,
            c7_anchor_offset_tag_widths,
            &out_dir,
            baseline.as_deref(),
            mode,
            badger_orientation,
            !no_badger,
            once,
        ),
        Commands::Doctor {
            camera,
            capture_backend,
            ffmpeg_input,
            capture_timeout_secs,
            port,
            out_dir,
        } => doctor(
            &camera,
            capture_backend,
            &ffmpeg_input,
            Duration::from_secs(capture_timeout_secs),
            &port,
            &out_dir,
        ),
        Commands::ListCameras => list_cameras(),
        Commands::InstallBadger { port } => install_badger(&port),
        Commands::RestoreBadger { port, backup } => restore_badger(&port, backup.as_deref()),
        Commands::SendDemo {
            port,
            pose,
            badger_orientation,
        } => send_demo(&port, pose, badger_orientation),
        Commands::SendStatus {
            port,
            message,
            badger_orientation,
        } => send_badger_message(&port, &message, badger_orientation),
        Commands::CalibrateBaseline {
            samples_dir,
            out,
            min_samples,
        } => calibrate_baseline(samples_dir.as_deref(), out.as_deref(), min_samples),
    }
}

fn generate_stickers(out: &Path, tag_mm: f32) -> Result<()> {
    ensure_parent(out)?;
    let labels = landmark_labels();
    let page_w = 210.0_f32;
    let page_h = 297.0_f32;
    let margin = 14.0_f32;
    let gap = 10.0_f32;
    let label_h = 8.0_f32;
    let cols = 2;
    let cell_w = (page_w - margin * 2.0 - gap) / cols as f32;
    let cell_h = tag_mm + label_h + 14.0;

    let mut svg = String::new();
    svg.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{page_w}mm" height="{page_h}mm" viewBox="0 0 {page_w} {page_h}">
<rect width="100%" height="100%" fill="white"/>
<style>
  text {{ font-family: -apple-system, BlinkMacSystemFont, Helvetica, Arial, sans-serif; font-size: 4px; fill: black; }}
  .tiny {{ font-size: 3px; }}
</style>
"#
    ));

    for (idx, (id, label)) in labels.iter().enumerate() {
        let col = idx % cols;
        let row = idx / cols;
        let x = margin + col as f32 * (cell_w + gap) + (cell_w - tag_mm) / 2.0;
        let y = margin + row as f32 * cell_h;
        svg.push_str(&format!(
            r#"<text x="{}" y="{}">{}: tag36h11-{}</text>
"#,
            x,
            y + tag_mm + 5.0,
            xml_escape(label),
            id
        ));
        let hint = if *id == C7_ID {
            "Use C7 flag below; face camera."
        } else {
            "Print at 100%; tape to skin/tight layer."
        };
        svg.push_str(&format!(
            r#"<text class="tiny" x="{}" y="{}">{}</text>
"#,
            x,
            y + tag_mm + 9.0,
            xml_escape(hint)
        ));
        svg.push_str(&tag_svg(*id, x, y, tag_mm)?);
        svg.push_str(&format!(
            r#"<rect x="{x}" y="{y}" width="{tag_mm}" height="{tag_mm}" fill="none" stroke="black" stroke-width="0.15" stroke-dasharray="1 1"/>
"#
        ));
    }
    svg.push_str(&c7_flag_template_svg(
        margin,
        margin + 2.0 * cell_h + 12.0,
        tag_mm,
    )?);
    svg.push_str("</svg>\n");
    fs::write(out, svg).with_context(|| format!("writing {}", out.display()))?;
    println!("wrote {}", out.display());
    Ok(())
}

fn c7_flag_template_svg(x: f32, y: f32, tag_mm: f32) -> Result<String> {
    let anchor_w = 30.0_f32;
    let hinge_w = 8.0_f32;
    let tag_x = x + anchor_w + hinge_w;
    let h = tag_mm + 12.0;
    let total_w = anchor_w + hinge_w + tag_mm + 8.0;
    let mut s = String::new();
    s.push_str(&format!(
        r#"<g>
  <text x="{x}" y="{y}" font-size="5px" font-weight="700">C7 side-facing flag template</text>
  <text class="tiny" x="{x}" y="{}">Cut outside. Tape ANCHOR over C7. Fold on dashed line so tag face points toward side camera.</text>
  <rect x="{x}" y="{}" width="{total_w}" height="{h}" fill="none" stroke="black" stroke-width="0.2"/>
  <rect x="{x}" y="{}" width="{anchor_w}" height="{h}" fill="none" stroke="black" stroke-width="0.15"/>
  <text class="tiny" x="{}" y="{}">ANCHOR</text>
  <text class="tiny" x="{}" y="{}">over C7</text>
  <line x1="{}" y1="{}" x2="{}" y2="{}" stroke="black" stroke-width="0.25" stroke-dasharray="1 1"/>
  <text class="tiny" x="{}" y="{}" transform="rotate(-90 {} {})">FOLD</text>
"#,
        y + 6.0,
        y + 10.0,
        y + 10.0,
        x + 5.0,
        y + 23.0,
        x + 5.0,
        y + 28.0,
        x + anchor_w,
        y + 10.0,
        x + anchor_w,
        y + 10.0 + h,
        x + anchor_w + 1.5,
        y + 31.0,
        x + anchor_w + 1.5,
        y + 31.0
    ));
    s.push_str(&tag_svg(C7_ID, tag_x, y + 16.0, tag_mm)?);
    s.push_str(&format!(
        r#"
  <rect x="{tag_x}" y="{}" width="{tag_mm}" height="{tag_mm}" fill="none" stroke="black" stroke-width="0.15" stroke-dasharray="1 1"/>
  <text class="tiny" x="{tag_x}" y="{}">faces camera</text>
</g>
"#,
        y + 16.0,
        y + tag_mm + 21.0
    ));
    Ok(s)
}

fn tag_svg(id: usize, x: f32, y: f32, size_mm: f32) -> Result<String> {
    let grid = tag_grid(id)?;
    let cell = size_mm / grid.len() as f32;
    let mut s = String::new();
    for (gy, row) in grid.iter().enumerate() {
        for (gx, black) in row.iter().enumerate() {
            if *black {
                s.push_str(&format!(
                    r#"<rect x="{}" y="{}" width="{cell}" height="{cell}" fill="black"/>
"#,
                    x + gx as f32 * cell,
                    y + gy as f32 * cell
                ));
            }
        }
    }
    Ok(s)
}

fn annotate_samples(input_dir: &Path, out_dir: &Path, tag_px: u32) -> Result<()> {
    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let files = image_files(input_dir)?;
    if files.is_empty() {
        bail!("no images found in {}", input_dir.display());
    }
    for (idx, path) in files.iter().enumerate() {
        let mut img = image::open(path)
            .with_context(|| format!("opening {}", path.display()))?
            .to_rgba8();
        let (w, h) = img.dimensions();
        let positions = fake_positions(idx, w, h);
        for (id, point) in positions {
            let tag = render_tag_rgba(id, tag_px)?;
            let x = (point.x.round() as i64 - tag_px as i64 / 2).max(0);
            let y = (point.y.round() as i64 - tag_px as i64 / 2).max(0);
            overlay(&mut img, &tag, x, y);
        }
        let out = out_dir.join(format!(
            "{}-tagged.png",
            path.file_stem().and_then(OsStr::to_str).unwrap_or("sample")
        ));
        img.save(&out)
            .with_context(|| format!("writing {}", out.display()))?;
        println!("wrote {}", out.display());
    }
    Ok(())
}

fn snapshot_frame(
    input: &Path,
    out_dir: &Path,
    rotate: FrameRotation,
    c7_anchor_offset_tag_widths: f64,
    send_badger_enabled: bool,
    port: &str,
    badger_orientation: BadgerOrientation,
) -> Result<()> {
    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let img = image::open(input).with_context(|| format!("opening {}", input.display()))?;
    let img = apply_rotation(img, rotate);
    let detections = detect_tags(&img)?;
    let detections = apply_c7_anchor_correction(&detections, c7_anchor_offset_tag_widths);

    let tag_debug = out_dir.join("latest-tags.png");
    write_tag_debug_image(&img, &detections, &tag_debug)?;
    println!("wrote {}", tag_debug.display());
    emit_tag_status(&detections);
    emit_mode_status(&estimate_mode_from_detections(&detections));
    let placement = estimate_placement_from_detections(&detections);
    emit_placement_status(&placement);

    let report = out_dir.join("latest-tags.txt");
    if !has_required_posture_tags(&detections) {
        write_tag_report(input, &img, &detections, None, &report)?;
        println!("wrote {}", report.display());
        eprintln!("{}", missing_required_tags_message(&detections));
        if send_badger_enabled && !detections.is_empty() {
            send_badger_message(port, placement.badger_message(), badger_orientation)?;
        }
        return Ok(());
    }

    let posture = posture_from_detections(&detections)?;
    print_posture(input, &posture, None);
    write_tag_report(input, &img, &detections, Some(&posture), &report)?;
    println!("wrote {}", report.display());
    let analysis = out_dir.join("latest-analysis.png");
    write_debug_image(&img, &detections, &posture, &analysis)?;
    println!("wrote {}", analysis.display());
    if send_badger_enabled {
        if placement.is_good() {
            send_to_badger(port, &posture, badger_orientation)?;
        } else {
            send_badger_message(port, placement.badger_message(), badger_orientation)?;
        }
    }
    Ok(())
}

fn run_samples(
    input_dir: &Path,
    out_dir: &Path,
    rotate: FrameRotation,
    c7_anchor_offset_tag_widths: f64,
    send_badger_enabled: bool,
    port: &str,
    badger_orientation: BadgerOrientation,
    window_secs: u64,
    delay_ms: u64,
) -> Result<()> {
    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let mut window = RollingWindow::new(Duration::from_secs(window_secs));
    let files = image_files(input_dir)?;
    if files.is_empty() {
        bail!("no images found in {}", input_dir.display());
    }
    for path in files {
        let img = image::open(&path).with_context(|| format!("opening {}", path.display()))?;
        let img = apply_rotation(img, rotate);
        let detections = detect_tags(&img)?;
        let detections = apply_c7_anchor_correction(&detections, c7_anchor_offset_tag_widths);
        let posture = posture_from_detections(&detections)?;
        window.push(posture);
        let avg = window.average().context("rolling average is empty")?;
        print_posture(&path, &avg, None);
        let out = out_dir.join(format!(
            "{}-analysis.png",
            path.file_stem().and_then(OsStr::to_str).unwrap_or("sample")
        ));
        write_debug_image(&img, &detections, &avg, &out)?;
        println!("wrote {}", out.display());
        if send_badger_enabled {
            publish_posture(port, &avg, None, None, badger_orientation, true)?;
            thread::sleep(Duration::from_millis(delay_ms));
        }
    }
    Ok(())
}

fn live(
    camera: &str,
    capture_backend: CaptureBackend,
    ffmpeg_input: &str,
    capture_timeout: Duration,
    port: &str,
    window_secs: u64,
    interval_secs: u64,
    no_person_after: Duration,
    rotate: FrameRotation,
    c7_anchor_offset_tag_widths: f64,
    out_dir: &Path,
    baseline_path: Option<&Path>,
    mode_override: DeskModeOverride,
    badger_orientation: BadgerOrientation,
    send_badger_enabled: bool,
) -> Result<()> {
    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let mut windows = ModeRollingWindows::new(Duration::from_secs(window_secs));
    let mut missing_person = MissingPersonState::new();
    let mut quality = QualityHistory::new(QUALITY_HISTORY_CAPACITY);
    println!(
        "starting live capture from {camera}; press Ctrl-C to stop; interval={}s window={}s rotate={rotate:?}",
        interval_secs, window_secs
    );
    emit_badger_startup_status(port, send_badger_enabled);
    loop {
        let capture = capture_frame(
            camera,
            capture_backend,
            ffmpeg_input,
            out_dir,
            capture_timeout,
        )?;
        match analyze_frame_file(
            &capture,
            None,
            1,
            out_dir,
            &mut windows,
            &mut missing_person,
            &mut quality,
            send_badger_enabled,
            port,
            no_person_after,
            rotate,
            c7_anchor_offset_tag_widths,
            baseline_path,
            mode_override,
            badger_orientation,
        ) {
            Ok(FrameAnalysisOutcome::Posture) => {}
            Ok(FrameAnalysisOutcome::MissingRequiredTags) => {}
            Ok(FrameAnalysisOutcome::InvalidPlacement) => {}
            Err(err) => {
                eprintln!("{}: {err:#}", capture.display());
            }
        }
        thread::sleep(Duration::from_secs(interval_secs));
    }
}

fn live_file(
    input: &Path,
    burst_dir: Option<&Path>,
    burst_frames: usize,
    port: &str,
    window_secs: u64,
    interval_secs: u64,
    no_person_after: Duration,
    rotate: FrameRotation,
    c7_anchor_offset_tag_widths: f64,
    out_dir: &Path,
    baseline_path: Option<&Path>,
    mode_override: DeskModeOverride,
    badger_orientation: BadgerOrientation,
    send_badger_enabled: bool,
    once: bool,
) -> Result<()> {
    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let mut windows = ModeRollingWindows::new(Duration::from_secs(window_secs));
    let mut missing_person = MissingPersonState::new();
    let mut quality = QualityHistory::new(QUALITY_HISTORY_CAPACITY);
    println!(
        "starting live-file from {}; press Ctrl-C to stop; interval={}s window={}s rotate={rotate:?}",
        input.display(),
        interval_secs,
        window_secs
    );
    if let Some(dir) = burst_dir {
        println!(
            "using burst frames from {}; frames={}",
            dir.display(),
            burst_frames.max(1)
        );
    }
    emit_badger_startup_status(port, send_badger_enabled);

    loop {
        if !input.exists() {
            if once {
                bail!("input does not exist: {}", input.display());
            }
            eprintln!("waiting for {}", input.display());
            thread::sleep(Duration::from_secs(interval_secs));
            continue;
        }

        match analyze_frame_file(
            input,
            burst_dir,
            burst_frames.max(1),
            out_dir,
            &mut windows,
            &mut missing_person,
            &mut quality,
            send_badger_enabled,
            port,
            no_person_after,
            rotate,
            c7_anchor_offset_tag_widths,
            baseline_path,
            mode_override,
            badger_orientation,
        ) {
            Ok(FrameAnalysisOutcome::Posture) => {}
            Ok(FrameAnalysisOutcome::MissingRequiredTags) => {}
            Ok(FrameAnalysisOutcome::InvalidPlacement) => {}
            Err(err) => eprintln!("{}: {err:#}", input.display()),
        }

        if once {
            break;
        }

        thread::sleep(Duration::from_secs(interval_secs));
    }
    Ok(())
}

#[derive(Debug)]
struct AnalyzedFrame {
    path: PathBuf,
    img: DynamicImage,
    detections: Vec<DetectionPoint>,
    frame_count: usize,
}

fn analyze_frame_source(
    input: &Path,
    burst_dir: Option<&Path>,
    burst_frames: usize,
    rotate: FrameRotation,
) -> Result<AnalyzedFrame> {
    let paths = frame_source_paths(input, burst_dir, burst_frames.max(1))?;
    let mut frames = Vec::new();
    for path in paths {
        match image::open(&path) {
            Ok(img) => {
                let img = apply_rotation(img, rotate);
                match detect_tags(&img) {
                    Ok(detections) => frames.push(AnalyzedFrame {
                        path,
                        img,
                        detections,
                        frame_count: 1,
                    }),
                    Err(err) => eprintln!("{}: tag detection failed: {err:#}", path.display()),
                }
            }
            Err(err) => eprintln!("{}: image open failed: {err:#}", path.display()),
        }
    }

    if frames.is_empty() {
        bail!("no readable frame source found for {}", input.display());
    }
    if frames.len() == 1 {
        return Ok(frames.remove(0));
    }

    let frame_count = frames.len();
    let best_index = frames
        .iter()
        .enumerate()
        .max_by_key(|(_, frame)| detection_set_score(&frame.detections))
        .map(|(idx, _)| idx)
        .unwrap_or(0);
    let fused_detections = fuse_burst_detections(&frames, &frames[best_index].detections);
    let mut selected = frames.swap_remove(best_index);
    selected.detections = fused_detections;
    selected.frame_count = frame_count;
    Ok(selected)
}

fn frame_source_paths(
    input: &Path,
    burst_dir: Option<&Path>,
    burst_frames: usize,
) -> Result<Vec<PathBuf>> {
    let mut candidates = Vec::new();
    if let Some(dir) = burst_dir {
        if dir.exists() {
            for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
                let entry = entry?;
                let path = entry.path();
                if !is_image_file(&path) {
                    continue;
                }
                let modified = entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .unwrap_or(UNIX_EPOCH);
                candidates.push((modified, path));
            }
        }
    }
    if input.exists() {
        let modified = fs::metadata(input)
            .and_then(|meta| meta.modified())
            .unwrap_or(UNIX_EPOCH);
        candidates.push((modified, input.to_path_buf()));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));

    let mut paths = Vec::new();
    for (_, path) in candidates {
        if paths.iter().any(|existing| existing == &path) {
            continue;
        }
        paths.push(path);
        if paths.len() >= burst_frames {
            break;
        }
    }
    Ok(paths)
}

fn is_image_file(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"))
        .unwrap_or(false)
}

fn detection_set_score(detections: &[DetectionPoint]) -> i32 {
    let required_count = [EAR_ID, C7_ID, SHOULDER_ID]
        .into_iter()
        .filter(|id| detections.iter().any(|det| det.id == *id))
        .count() as i32;
    let hip_bonus = if detections.iter().any(|det| det.id == HIP_ID) {
        1
    } else {
        0
    };
    let placement_bonus = if required_count == 3 {
        estimate_placement_from_detections(detections).score as i32
    } else {
        0
    };
    let edge = average_tag_edge_px(detections).unwrap_or(0.0).round() as i32;
    required_count * 1000
        + hip_bonus * 250
        + placement_bonus * 5
        + detections.len() as i32 * 100
        + edge
}

fn fuse_burst_detections(
    frames: &[AnalyzedFrame],
    anchor_detections: &[DetectionPoint],
) -> Vec<DetectionPoint> {
    [EAR_ID, C7_ID, SHOULDER_ID, HIP_ID]
        .into_iter()
        .filter_map(|id| fuse_tag_detection(id, frames, anchor_detections))
        .collect()
}

fn fuse_tag_detection(
    id: usize,
    frames: &[AnalyzedFrame],
    anchor_detections: &[DetectionPoint],
) -> Option<DetectionPoint> {
    let anchor = strongest_detection(anchor_detections, id);
    let mut observations = frames
        .iter()
        .filter_map(|frame| strongest_detection(&frame.detections, id).cloned())
        .collect::<Vec<_>>();
    if let Some(anchor) = anchor {
        let radius = (tag_edge_px(anchor) * 6.0).max(90.0);
        observations.retain(|det| point_distance(det.center, anchor.center) <= radius);
        if observations.is_empty() {
            observations.push(anchor.clone());
        }
    }
    if observations.is_empty() {
        return None;
    }

    let best = observations
        .iter()
        .max_by(|a, b| tag_edge_px(a).total_cmp(&tag_edge_px(b)))
        .cloned()?;
    let center = Point::new(
        median_f64(observations.iter().map(|det| det.center.x).collect()),
        median_f64(observations.iter().map(|det| det.center.y).collect()),
    );
    let dx = center.x - best.center.x;
    let dy = center.y - best.center.y;
    let mut corners = best.corners;
    for corner in &mut corners {
        corner[0] += dx;
        corner[1] += dy;
    }
    Some(DetectionPoint {
        id,
        center,
        corners,
    })
}

fn strongest_detection(detections: &[DetectionPoint], id: usize) -> Option<&DetectionPoint> {
    detections
        .iter()
        .filter(|det| det.id == id)
        .max_by(|a, b| tag_edge_px(a).total_cmp(&tag_edge_px(b)))
}

fn point_distance(a: Point, b: Point) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

fn median_f64(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

fn analyze_frame_file(
    input: &Path,
    burst_dir: Option<&Path>,
    burst_frames: usize,
    out_dir: &Path,
    windows: &mut ModeRollingWindows,
    missing_person: &mut MissingPersonState,
    quality: &mut QualityHistory,
    send_badger_enabled: bool,
    port: &str,
    no_person_after: Duration,
    rotate: FrameRotation,
    c7_anchor_offset_tag_widths: f64,
    baseline_path: Option<&Path>,
    mode_override: DeskModeOverride,
    badger_orientation: BadgerOrientation,
) -> Result<FrameAnalysisOutcome> {
    let analyzed = analyze_frame_source(input, burst_dir, burst_frames, rotate)?;
    let img = analyzed.img;
    let detections = apply_c7_anchor_correction(&analyzed.detections, c7_anchor_offset_tag_widths);
    let input = analyzed.path;
    if analyzed.frame_count > 1 {
        println!(
            "BURST,frames={},source={}",
            analyzed.frame_count,
            input.display()
        );
    }
    let tag_debug = out_dir.join("latest-tags.png");
    write_tag_debug_image(&img, &detections, &tag_debug)?;
    emit_tag_status(&detections);
    let mode = mode_override.estimate(&detections);
    emit_mode_status(&mode);
    let placement = estimate_placement_from_detections(&detections);
    emit_placement_status(&placement);
    if !has_required_posture_tags(&detections) {
        quality.push(false);
        let report = out_dir.join("latest-tags.txt");
        write_tag_report(&input, &img, &detections, None, &report)?;
        eprintln!("{}", missing_required_tags_message(&detections));
        if detections.is_empty() {
            missing_person.record_missing(
                no_person_after,
                port,
                send_badger_enabled,
                badger_orientation,
            )?;
        } else {
            missing_person.clear();
            if quality.consecutive_misses() >= MESSAGE_AFTER_CONSECUTIVE_MISSES {
                publish_badger_message(
                    port,
                    placement.badger_message(),
                    badger_orientation,
                    send_badger_enabled,
                )?;
            }
        }
        return Ok(FrameAnalysisOutcome::MissingRequiredTags);
    }
    let posture = posture_from_detections(&detections)?;
    let report = out_dir.join("latest-tags.txt");
    write_tag_report(&input, &img, &detections, Some(&posture), &report)?;
    missing_person.clear();
    if !placement.is_good() {
        quality.push(false);
        print_posture(&input, &posture, None);
        let debug = out_dir.join("latest-analysis.png");
        write_debug_image(&img, &detections, &posture, &debug)?;
        if quality.consecutive_misses() >= MESSAGE_AFTER_CONSECUTIVE_MISSES {
            publish_badger_message(
                port,
                placement.badger_message(),
                badger_orientation,
                send_badger_enabled,
            )?;
        }
        return Ok(FrameAnalysisOutcome::InvalidPlacement);
    }
    quality.push(true);
    let avg = windows
        .push(mode.mode, posture)
        .context("rolling average is empty")?;
    let drift =
        baseline_path.and_then(
            |path| match baseline_drift_for_file(path, mode.mode, &avg) {
                Ok(drift) => drift,
                Err(err) => {
                    eprintln!("baseline drift unavailable: {err:#}");
                    None
                }
            },
        );
    print_posture(&input, &avg, drift.as_ref());
    let debug = out_dir.join("latest-analysis.png");
    write_debug_image(&img, &detections, &avg, &debug)?;
    let quality_bits = quality.bits();
    publish_posture(
        port,
        &avg,
        drift.as_ref(),
        Some(&quality_bits),
        badger_orientation,
        send_badger_enabled,
    )?;
    Ok(FrameAnalysisOutcome::Posture)
}

fn apply_rotation(img: DynamicImage, rotation: FrameRotation) -> DynamicImage {
    match rotation {
        FrameRotation::None => img,
        FrameRotation::Cw90 => img.rotate90(),
        FrameRotation::Ccw90 => img.rotate270(),
        FrameRotation::Deg180 => img.rotate180(),
    }
}

fn has_required_posture_tags(detections: &[DetectionPoint]) -> bool {
    [EAR_ID, C7_ID, SHOULDER_ID]
        .iter()
        .all(|id| detections.iter().any(|det| det.id == *id))
}

fn missing_required_tags_message(detections: &[DetectionPoint]) -> String {
    let mut ids = detections.iter().map(|det| det.id).collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    format!(
        "need at least ear(tag {EAR_ID}), C7(tag {C7_ID}), and shoulder(tag {SHOULDER_ID}); found ids {ids:?}"
    )
}

fn emit_tag_status(detections: &[DetectionPoint]) {
    let mut ids = detections.iter().map(|det| det.id).collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();

    let present = [EAR_ID, C7_ID, SHOULDER_ID, HIP_ID]
        .into_iter()
        .filter(|id| ids.contains(id))
        .map(tag_short_label)
        .collect::<Vec<_>>();
    let missing_required = [EAR_ID, C7_ID, SHOULDER_ID]
        .into_iter()
        .filter(|id| !ids.contains(id))
        .map(tag_short_label)
        .collect::<Vec<_>>();
    let status = if missing_required.is_empty() {
        "ready"
    } else {
        "missing"
    };
    println!(
        "TAGS,{status},{},{}",
        clean_payload_text(&present.join(" ")),
        clean_payload_text(&missing_required.join(" "))
    );
}

fn tag_short_label(id: usize) -> &'static str {
    match id {
        EAR_ID => "ear",
        C7_ID => "C7",
        SHOULDER_ID => "shoulder",
        HIP_ID => "hip",
        _ => "unknown",
    }
}

fn list_cameras() -> Result<()> {
    let output = imagesnap_list_output()?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        bail!("imagesnap -l failed");
    }
    Ok(())
}

fn doctor(
    camera: &str,
    capture_backend: CaptureBackend,
    ffmpeg_input: &str,
    capture_timeout: Duration,
    port: &str,
    out_dir: &Path,
) -> Result<()> {
    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let mut failures = Vec::new();

    println!("doctor: checking imagesnap camera list");
    match imagesnap_list_output() {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            print!("{stdout}");
            if stdout.contains(camera) {
                println!("OK camera listed: {camera}");
            } else {
                failures.push(format!("camera `{camera}` was not listed by imagesnap"));
                println!("FAIL camera not listed: {camera}");
            }
        }
        Ok(output) => {
            failures.push("imagesnap -l failed".to_string());
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }
        Err(err) => failures.push(format!("imagesnap unavailable: {err:#}")),
    }

    println!("doctor: checking camera capture permission");
    match capture_frame(
        camera,
        capture_backend,
        ffmpeg_input,
        out_dir,
        capture_timeout,
    ) {
        Ok(path) => println!("OK camera capture: {}", path.display()),
        Err(err) => match recent_macos_app_frame(Duration::from_secs(180)) {
            Ok(Some((path, age))) => {
                println!(
                    "OK macOS app camera frame: {} (updated {}s ago)",
                    path.display(),
                    age.as_secs()
                );
                println!("NOTE CLI camera capture is still unavailable: {err:#}");
            }
            Ok(None) => {
                failures.push(format!("camera capture failed: {err:#}"));
                println!("FAIL camera capture: {err:#}");
            }
            Err(frame_err) => {
                failures.push(format!(
                    "camera capture failed: {err:#}; macOS app frame check failed: {frame_err:#}"
                ));
                println!("FAIL camera capture: {err:#}");
                println!("FAIL macOS app frame check: {frame_err:#}");
            }
        },
    }

    println!("doctor: checking Badger receiver on {port}");
    match ping_badger(port) {
        Ok(reply) => println!("OK Badger receiver: {reply}"),
        Err(err) => {
            failures.push(format!("Badger receiver check failed: {err:#}"));
            println!("FAIL Badger receiver: {err:#}");
        }
    }

    println!("doctor: checking generated sample image pipeline");
    let tagged = Path::new("artifacts/tagged-samples");
    if !tagged.exists() {
        annotate_samples(Path::new("sample-images"), tagged, 120)
            .context("generating tagged samples for doctor")?;
    }
    let files = image_files(tagged)?;
    if let Some(first) = files.first() {
        match image::open(first)
            .with_context(|| format!("opening {}", first.display()))
            .and_then(|img| {
                let detections = detect_tags(&img)?;
                posture_from_detections(&detections)
            }) {
            Ok(posture) => {
                println!(
                    "OK sample analysis: detected={} cva={}",
                    posture.detected_count,
                    posture
                        .cva_degrees
                        .map(|v| format!("{v:.1}"))
                        .unwrap_or_else(|| "n/a".to_string())
                );
            }
            Err(err) => {
                failures.push(format!("sample analysis failed: {err:#}"));
                println!("FAIL sample analysis: {err:#}");
            }
        }
    } else {
        failures.push("no tagged samples available".to_string());
        println!("FAIL no tagged samples available");
    }

    println!("doctor: checking baseline/manual-mode display payload");
    match doctor_baseline_display_smoke(&files, out_dir) {
        Ok(note) => println!("OK baseline display payload: {note}"),
        Err(err) => {
            failures.push(format!("baseline display smoke failed: {err:#}"));
            println!("FAIL baseline display smoke: {err:#}");
        }
    }

    if failures.is_empty() {
        println!("doctor: all checks passed");
        Ok(())
    } else {
        println!("doctor: {} check(s) failed", failures.len());
        for failure in &failures {
            println!(" - {failure}");
        }
        bail!("doctor checks failed")
    }
}

fn doctor_baseline_display_smoke(files: &[PathBuf], out_dir: &Path) -> Result<String> {
    let (path, posture) = first_good_sample_posture(files)?;
    let cva = posture
        .cva_degrees
        .context("doctor sample did not produce a CVA measurement")?;
    let head = posture
        .head_forward_px
        .context("doctor sample did not produce head-forward measurement")?;
    let baseline_path = out_dir.join("doctor-baseline.txt");
    fs::write(
        &baseline_path,
        format!(
            "created_at_unix=0\nsamples_dir={}\nmin_samples_per_mode=1\nmode.sitting.status=ready\nmode.sitting.accepted=1\nmode.sitting.cva_degrees.mean={:.2}\nmode.sitting.head_forward_px.mean={:.2}\nmode.sitting.display_points={}\nmode.standing.status=needs_more_samples\nmode.standing.accepted=0\n",
            out_dir.display(),
            cva - 5.0,
            head,
            format_display_points(&posture.display_points)
        ),
    )
    .with_context(|| format!("writing {}", baseline_path.display()))?;

    let mut windows = ModeRollingWindows::new(Duration::from_secs(120));
    let avg = windows
        .push(DetectedDeskMode::Sitting, posture)
        .context("doctor smoke rolling average was empty")?;
    let drift = baseline_drift_for_file(&baseline_path, DetectedDeskMode::Sitting, &avg)?
        .context("doctor smoke baseline drift was unavailable")?;
    let note = drift.note();
    ensure!(
        note == "sit +5deg",
        "expected baseline note `sit +5deg`, got `{note}` using {}",
        path.display()
    );
    let baseline_points = (!drift.baseline_display_points.is_empty())
        .then_some(drift.baseline_display_points.as_slice());
    let payload = badger_payload(
        &avg,
        Some(&note),
        baseline_points,
        Some("1"),
        BadgerOrientation::UsbTop,
    );
    ensure!(
        payload.trim().contains(",sit +5deg,B,"),
        "expected Badger payload to include baseline note and baseline curve, got `{}`",
        payload.trim()
    );
    Ok(note)
}

fn first_good_sample_posture(files: &[PathBuf]) -> Result<(PathBuf, PostureFrame)> {
    for path in files {
        let img = image::open(path).with_context(|| format!("opening {}", path.display()))?;
        let detections = detect_tags(&img)?;
        if !estimate_placement_from_detections(&detections).is_good() {
            continue;
        }
        let posture = posture_from_detections(&detections)?;
        return Ok((path.clone(), posture));
    }
    bail!("no tagged sample with good marker placement was available")
}

fn imagesnap_list_output() -> Result<std::process::Output> {
    Command::new("imagesnap")
        .arg("-l")
        .output()
        .context("running imagesnap -l; install with `brew install imagesnap` if missing")
}

fn install_badger(port: &str) -> Result<()> {
    let receiver = Path::new("badger/posture_receiver.py");
    if !receiver.exists() {
        bail!("missing {}", receiver.display());
    }
    let backup_dir = Path::new("artifacts/badger-backups");
    fs::create_dir_all(backup_dir).context("creating Badger backup directory")?;
    let backup = backup_dir.join(format!("main-{}.py", timestamp_for_file()?));

    let mpremote = mpremote_path();
    run_cmd(
        Command::new(&mpremote)
            .args(["connect", port, "fs", "cp", ":main.py"])
            .arg(&backup),
        "backing up Badger main.py",
    )?;
    run_cmd(
        Command::new(&mpremote)
            .args(["connect", port, "fs", "cp"])
            .arg(receiver)
            .arg(":main.py"),
        "installing posture receiver as Badger main.py",
    )?;
    run_cmd(
        Command::new(&mpremote).args(["connect", port, "reset"]),
        "resetting Badger",
    )?;
    wait_for_badger_receiver(port, Duration::from_secs(8))?;
    println!(
        "Badger receiver installed; backup saved to {}",
        backup.display()
    );
    Ok(())
}

fn wait_for_badger_receiver(port: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut last_error = None;
    while Instant::now() < deadline {
        match ping_badger(port) {
            Ok(_) => return Ok(()),
            Err(err) => {
                last_error = Some(err);
                thread::sleep(Duration::from_millis(500));
            }
        }
    }
    match last_error {
        Some(err) => Err(err).context("waiting for Badger receiver after reset"),
        None => bail!("timed out waiting for Badger receiver after reset"),
    }
}

fn restore_badger(port: &str, backup: Option<&Path>) -> Result<()> {
    let backup = match backup {
        Some(path) => path.to_path_buf(),
        None => latest_backup(Path::new("artifacts/badger-backups"))?
            .ok_or_else(|| anyhow!("no backup found in artifacts/badger-backups"))?,
    };
    let mpremote = mpremote_path();
    run_cmd(
        Command::new(&mpremote)
            .args(["connect", port, "fs", "cp"])
            .arg(&backup)
            .arg(":main.py"),
        "restoring Badger main.py",
    )?;
    run_cmd(
        Command::new(&mpremote).args(["connect", port, "reset"]),
        "resetting Badger",
    )?;
    println!("Badger main.py restored from {}", backup.display());
    Ok(())
}

fn send_demo(port: &str, pose: DemoPose, badger_orientation: BadgerOrientation) -> Result<()> {
    let points = match pose {
        DemoPose::Neutral => vec![
            (SHOULDER_ID, Point::new(230.0, 64.0)),
            (C7_ID, Point::new(135.0, 64.0)),
            (EAR_ID, Point::new(42.0, 68.0)),
        ],
        DemoPose::Hunched => vec![
            (SHOULDER_ID, Point::new(230.0, 64.0)),
            (C7_ID, Point::new(135.0, 76.0)),
            (EAR_ID, Point::new(42.0, 108.0)),
        ],
    };
    let posture = PostureFrame {
        landmarks: BTreeMap::new(),
        display_points: points,
        detected_count: 3,
        cva_degrees: None,
        head_forward_px: None,
    };
    send_to_badger(port, &posture, badger_orientation)
}

fn capture_frame(
    camera: &str,
    backend: CaptureBackend,
    ffmpeg_input: &str,
    out_dir: &Path,
    timeout: Duration,
) -> Result<PathBuf> {
    match backend {
        CaptureBackend::Imagesnap => capture_with_imagesnap(camera, out_dir, timeout),
        CaptureBackend::Ffmpeg => capture_with_ffmpeg(ffmpeg_input, out_dir, timeout),
        CaptureBackend::Auto => match capture_with_imagesnap(camera, out_dir, timeout) {
            Ok(path) => Ok(path),
            Err(imagesnap_err) => match capture_with_ffmpeg(ffmpeg_input, out_dir, timeout) {
                Ok(path) => Ok(path),
                Err(ffmpeg_err) => Err(anyhow!(
                    "all capture backends failed\nimagesnap: {imagesnap_err:#}\nffmpeg: {ffmpeg_err:#}"
                )),
            },
        },
    }
}

fn capture_with_imagesnap(camera: &str, out_dir: &Path, timeout: Duration) -> Result<PathBuf> {
    let out = out_dir.join("latest-capture.jpg");
    let mut cmd = Command::new("imagesnap");
    cmd.args(["-d", camera]).arg(&out);
    let output = run_command_with_timeout(&mut cmd, timeout)
        .with_context(|| "running imagesnap; install with `brew install imagesnap` if missing")?;
    if output.timed_out {
        bail!("imagesnap timed out after {}s", timeout.as_secs());
    }
    if !output.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Camera access not granted") {
            bail!(
                "camera access not granted. Grant Camera permission to the app/terminal running this process, then retry."
            );
        }
        bail!("imagesnap failed: {stderr}");
    }
    Ok(out)
}

fn capture_with_ffmpeg(ffmpeg_input: &str, out_dir: &Path, timeout: Duration) -> Result<PathBuf> {
    let out = out_dir.join("latest-capture-ffmpeg.jpg");
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "warning",
        "-f",
        "avfoundation",
        "-pixel_format",
        "uyvy422",
        "-framerate",
        "15",
        "-i",
        ffmpeg_input,
        "-frames:v",
        "1",
        "-y",
    ])
    .arg(&out);
    let output = run_command_with_timeout(&mut cmd, timeout)
        .with_context(|| "running ffmpeg; install with `brew install ffmpeg` if missing")?;
    if output.timed_out {
        bail!("ffmpeg timed out after {}s", timeout.as_secs());
    }
    if !output.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not authorized")
            || stderr.contains("permission")
            || stderr.contains("Operation not permitted")
        {
            bail!(
                "ffmpeg camera access denied. Grant Camera permission to the app/terminal running this process, then retry."
            );
        }
        bail!("ffmpeg failed: {stderr}");
    }
    Ok(out)
}

fn recent_macos_app_frame(max_age: Duration) -> Result<Option<(PathBuf, Duration)>> {
    if !cfg!(target_os = "macos") {
        return Ok(None);
    }
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(None);
    };
    let path = PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Posture Watcher")
        .join("latest-frame.jpg");
    if !path.exists() {
        return Ok(None);
    }
    let modified = fs::metadata(&path)
        .with_context(|| format!("reading {}", path.display()))?
        .modified()
        .with_context(|| format!("reading mtime for {}", path.display()))?;
    let age = SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::ZERO);
    if age > max_age {
        return Ok(None);
    }
    image::open(&path).with_context(|| format!("opening {}", path.display()))?;
    Ok(Some((path, age)))
}

struct TimedCommandOutput {
    stderr: Vec<u8>,
    timed_out: bool,
    status_success: bool,
}

impl TimedCommandOutput {
    fn success(&self) -> bool {
        !self.timed_out && self.status_success
    }
}

fn run_command_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<TimedCommandOutput> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning command")?;
    let deadline = Instant::now() + timeout;
    loop {
        if child
            .try_wait()
            .context("checking command status")?
            .is_some()
        {
            let output = child.wait_with_output().context("reading command output")?;
            return Ok(TimedCommandOutput {
                stderr: output.stderr,
                timed_out: false,
                status_success: output.status.success(),
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child
                .wait_with_output()
                .context("reading killed command output")?;
            return Ok(TimedCommandOutput {
                stderr: output.stderr,
                timed_out: true,
                status_success: false,
            });
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn detect_tags(img: &DynamicImage) -> Result<Vec<DetectionPoint>> {
    let luma = img.to_luma8();
    let image = apriltag::Image::from_image_buffer(&luma);
    let mut detector = Detector::builder()
        .add_family_bits(Family::tag_36h11(), 1)
        .build()
        .context("creating AprilTag detector")?;
    detector.set_thread_number(4);
    detector.set_decimation(1.0);
    detector.set_refine_edges(true);
    let detections = detector
        .detect(&image)
        .into_iter()
        .filter(|d| [EAR_ID, C7_ID, SHOULDER_ID, HIP_ID].contains(&d.id()))
        .map(|d| DetectionPoint {
            id: d.id(),
            center: Point::new(d.center()[0], d.center()[1]),
            corners: d.corners(),
        })
        .collect();
    Ok(detections)
}

fn apply_c7_anchor_correction(
    detections: &[DetectionPoint],
    offset_tag_widths: f64,
) -> Vec<DetectionPoint> {
    if offset_tag_widths <= 0.0 {
        return detections.to_vec();
    }

    let Some(ear) = strongest_detection(detections, EAR_ID).map(|det| det.center) else {
        return detections.to_vec();
    };
    let Some(shoulder) = strongest_detection(detections, SHOULDER_ID).map(|det| det.center) else {
        return detections.to_vec();
    };

    detections
        .iter()
        .map(|det| {
            let mut corrected = det.clone();
            if det.id == C7_ID {
                corrected.center = corrected_c7_anchor(det, ear, shoulder, offset_tag_widths);
            }
            corrected
        })
        .collect()
}

fn corrected_c7_anchor(
    det: &DetectionPoint,
    ear: Point,
    shoulder: Point,
    offset_tag_widths: f64,
) -> Point {
    let tag_width = tag_edge_px(det);
    let shift = tag_width * offset_tag_widths;
    if shift <= 0.0 {
        return det.center;
    }

    let axes = tag_unit_axes(det);
    let current_distance = distance_to_segment(det.center, ear, shoulder);
    let mut best = (current_distance, det.center);
    for axis in axes {
        for direction in [-1.0, 1.0] {
            let candidate = Point::new(
                det.center.x + axis.x * shift * direction,
                det.center.y + axis.y * shift * direction,
            );
            let distance = distance_to_segment(candidate, ear, shoulder);
            if distance < best.0 {
                best = (distance, candidate);
            }
        }
    }

    if best.0 + tag_width * 0.1 < current_distance {
        best.1
    } else {
        det.center
    }
}

fn tag_unit_axes(det: &DetectionPoint) -> [Point; 2] {
    [
        unit_vector(
            det.corners[1][0] - det.corners[0][0],
            det.corners[1][1] - det.corners[0][1],
        ),
        unit_vector(
            det.corners[3][0] - det.corners[0][0],
            det.corners[3][1] - det.corners[0][1],
        ),
    ]
}

fn unit_vector(dx: f64, dy: f64) -> Point {
    let length = dx.hypot(dy);
    if length <= f64::EPSILON {
        Point::new(1.0, 0.0)
    } else {
        Point::new(dx / length, dy / length)
    }
}

fn distance_to_segment(point: Point, a: Point, b: Point) -> f64 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len_sq = dx * dx + dy * dy;
    if len_sq <= f64::EPSILON {
        return point_distance(point, a);
    }
    let t = (((point.x - a.x) * dx + (point.y - a.y) * dy) / len_sq).clamp(0.0, 1.0);
    let projected = Point::new(a.x + t * dx, a.y + t * dy);
    point_distance(point, projected)
}

fn estimate_mode_from_detections(detections: &[DetectionPoint]) -> ModeEstimate {
    let landmarks = detections
        .iter()
        .map(|det| (det.id, det.center))
        .collect::<BTreeMap<_, _>>();
    estimate_mode_from_landmarks(&landmarks)
}

fn estimate_mode_from_landmarks(landmarks: &BTreeMap<usize, Point>) -> ModeEstimate {
    let Some(shoulder) = landmarks.get(&SHOULDER_ID) else {
        return ModeEstimate::unknown("needs shoulder and hip tags");
    };
    let Some(hip) = landmarks.get(&HIP_ID) else {
        return ModeEstimate::unknown("needs hip tag");
    };

    let dx = hip.x - shoulder.x;
    let dy = hip.y - shoulder.y;
    let distance = dx.hypot(dy);
    if distance < 30.0 {
        return ModeEstimate::unknown("shoulder hip too close");
    }

    let angle_from_vertical = dx.abs().atan2(dy.abs().max(1.0)).to_degrees();
    let detail =
        format!("shoulder_hip_from_vertical={angle_from_vertical:.0}deg dx={dx:.0} dy={dy:.0}");
    if angle_from_vertical <= 30.0 && dy >= -30.0 {
        let confidence = (95.0 - angle_from_vertical).round().clamp(60.0, 95.0) as u8;
        return ModeEstimate::new(DetectedDeskMode::Standing, confidence, detail);
    }
    if angle_from_vertical >= 55.0 {
        let confidence = (55.0 + (angle_from_vertical - 55.0) * 1.2)
            .round()
            .clamp(60.0, 95.0) as u8;
        return ModeEstimate::new(DetectedDeskMode::Sitting, confidence, detail);
    }

    ModeEstimate::new(DetectedDeskMode::Unknown, 35, detail)
}

fn estimate_placement_from_detections(detections: &[DetectionPoint]) -> PlacementEstimate {
    let landmarks = detections
        .iter()
        .map(|det| (det.id, det.center))
        .collect::<BTreeMap<_, _>>();
    let missing_required_ids = [EAR_ID, C7_ID, SHOULDER_ID]
        .into_iter()
        .filter(|id| !landmarks.contains_key(id))
        .collect::<Vec<_>>();
    if !missing_required_ids.is_empty() {
        let missing_required = missing_required_ids
            .iter()
            .copied()
            .map(tag_short_label)
            .collect::<Vec<_>>();
        return PlacementEstimate::missing_with_action(
            missing_required_action(&missing_required_ids),
            format!("needs {}", missing_required.join(" ")),
        );
    }

    let ear = landmarks[&EAR_ID];
    let c7 = landmarks[&C7_ID];
    let shoulder = landmarks[&SHOULDER_ID];
    let tag_edge = average_tag_edge_px(detections).unwrap_or(24.0).max(1.0);
    let mut issues = Vec::new();

    if tag_edge < 16.0 {
        issues.push("tags too small".to_string());
    }

    let ear_above_c7 = c7.y - ear.y;
    if ear_above_c7 < tag_edge * 0.5 {
        issues.push("ear not above C7".to_string());
    }

    let cva = cva_degrees(ear, c7);
    if !(15.0..=80.0).contains(&cva) {
        issues.push(format!("ear-C7 angle implausible {cva:.0}deg"));
    }

    let shoulder_below_c7 = shoulder.y - c7.y;
    if shoulder_below_c7 < -tag_edge {
        issues.push("shoulder above C7".to_string());
    }

    if issues.is_empty() {
        return PlacementEstimate::good(format!("cva={cva:.0}deg"));
    }

    let score = (100_i32 - issues.len() as i32 * 35).clamp(0, 70) as u8;
    let action = placement_action_for_issues(&issues);
    PlacementEstimate::check(score, action, issues.join("; "))
}

fn missing_required_action(missing_required_ids: &[usize]) -> &'static str {
    if missing_required_ids.len() == 1 && missing_required_ids[0] == C7_ID {
        return "Aim C7 flag";
    }
    "Place missing tags"
}

fn placement_action_for_issues(issues: &[String]) -> &'static str {
    if issues.iter().any(|issue| issue == "ear not above C7") {
        return "Move ear tag up";
    }
    if issues
        .iter()
        .any(|issue| issue.starts_with("ear-C7 angle implausible"))
    {
        return "Recheck ear and C7";
    }
    if issues.iter().any(|issue| issue == "shoulder above C7") {
        return "Move shoulder tag down";
    }
    if issues.iter().any(|issue| issue == "tags too small") {
        return "Move closer";
    }
    CHECK_MARKERS_MESSAGE
}

fn average_tag_edge_px(detections: &[DetectionPoint]) -> Option<f64> {
    if detections.is_empty() {
        return None;
    }
    Some(detections.iter().map(tag_edge_px).sum::<f64>() / detections.len() as f64)
}

fn cva_degrees(ear: Point, c7: Point) -> f64 {
    let vertical = (c7.y - ear.y).abs();
    let horizontal = (ear.x - c7.x).abs().max(1.0);
    vertical.atan2(horizontal).to_degrees()
}

fn posture_from_detections(detections: &[DetectionPoint]) -> Result<PostureFrame> {
    let mut landmarks = BTreeMap::new();
    for det in detections {
        landmarks.insert(det.id, det.center);
    }
    let ear = landmarks.get(&EAR_ID).copied();
    let c7 = landmarks.get(&C7_ID).copied();
    let shoulder = landmarks.get(&SHOULDER_ID).copied();
    if ear.is_none() || c7.is_none() || shoulder.is_none() {
        bail!(
            "need at least ear(tag {EAR_ID}), C7(tag {C7_ID}), and shoulder(tag {SHOULDER_ID}); found ids {:?}",
            landmarks.keys().collect::<Vec<_>>()
        );
    }
    let display_points = display_curve(&landmarks);
    let ear = ear.unwrap();
    let c7 = c7.unwrap();
    let shoulder = shoulder.unwrap();
    let cva_degrees = Some(cva_degrees(ear, c7));
    let head_forward_px = Some(ear.x - shoulder.x);
    Ok(PostureFrame {
        landmarks,
        display_points,
        detected_count: detections.len(),
        cva_degrees,
        head_forward_px,
    })
}

fn display_curve(landmarks: &BTreeMap<usize, Point>) -> Vec<(usize, Point)> {
    let mut chain = Vec::new();
    if let Some(p) = landmarks.get(&HIP_ID) {
        chain.push((HIP_ID, *p));
    }
    if let Some(p) = landmarks.get(&SHOULDER_ID) {
        chain.push((SHOULDER_ID, *p));
    }
    if let Some(p) = landmarks.get(&C7_ID) {
        chain.push((C7_ID, *p));
    }
    if let Some(p) = landmarks.get(&EAR_ID) {
        chain.push((EAR_ID, *p));
    }
    if chain.is_empty() {
        return chain;
    }
    let base_x = landmarks
        .get(&SHOULDER_ID)
        .or_else(|| landmarks.get(&HIP_ID))
        .map(|p| p.x)
        .unwrap_or(chain[0].1.x);
    let max_abs_dx = chain
        .iter()
        .map(|(_, p)| (p.x - base_x).abs())
        .fold(1.0_f64, f64::max);
    let scale = (52.0 / max_abs_dx).min(0.32);

    chain
        .into_iter()
        .map(|(id, p)| {
            let x = match id {
                HIP_ID => 274.0,
                SHOULDER_ID if landmarks.contains_key(&HIP_ID) => 204.0,
                SHOULDER_ID => 252.0,
                C7_ID => 126.0,
                EAR_ID => 32.0,
                _ => 148.0,
            };
            let y = (BADGER_HEIGHT as f64 / 2.0 + (p.x - base_x) * scale)
                .clamp(8.0, BADGER_HEIGHT as f64 - 8.0);
            (id, Point::new(x, y))
        })
        .collect()
}

fn write_debug_image(
    img: &DynamicImage,
    detections: &[DetectionPoint],
    posture: &PostureFrame,
    out: &Path,
) -> Result<()> {
    ensure_parent(out)?;
    let mut canvas = img.to_rgba8();
    for det in detections {
        draw_tag_outline(&mut canvas, det);
        let c = det.center;
        draw_filled_rect_mut(
            &mut canvas,
            Rect::at(c.x.round() as i32 - 5, c.y.round() as i32 - 5).of_size(10, 10),
            Rgba([255, 0, 0, 255]),
        );
    }
    let chain = [HIP_ID, SHOULDER_ID, C7_ID, EAR_ID]
        .into_iter()
        .filter_map(|id| posture.landmarks.get(&id).map(|p| (id, *p)))
        .collect::<Vec<_>>();
    for pair in chain.windows(2) {
        let a = pair[0].1;
        let b = pair[1].1;
        draw_line_segment_mut(
            &mut canvas,
            (a.x as f32, a.y as f32),
            (b.x as f32, b.y as f32),
            Rgba([0, 255, 80, 255]),
        );
    }
    for det in detections {
        draw_detection_label(&mut canvas, det);
    }
    canvas
        .save(out)
        .with_context(|| format!("writing {}", out.display()))?;
    Ok(())
}

fn write_tag_debug_image(
    img: &DynamicImage,
    detections: &[DetectionPoint],
    out: &Path,
) -> Result<()> {
    ensure_parent(out)?;
    let mut canvas = img.to_rgba8();
    for det in detections {
        draw_tag_outline(&mut canvas, det);
        let c = det.center;
        draw_filled_rect_mut(
            &mut canvas,
            Rect::at(c.x.round() as i32 - 5, c.y.round() as i32 - 5).of_size(10, 10),
            Rgba([255, 0, 0, 255]),
        );
    }
    for det in detections {
        draw_detection_label(&mut canvas, det);
    }
    canvas
        .save(out)
        .with_context(|| format!("writing {}", out.display()))?;
    Ok(())
}

fn draw_detection_label(canvas: &mut RgbaImage, det: &DetectionPoint) {
    let label = tag_short_label(det.id).to_ascii_uppercase();
    draw_bitmap_label(canvas, &label, det.center);
}

fn draw_bitmap_label(canvas: &mut RgbaImage, label: &str, point: Point) {
    let scale = ((canvas.width().min(canvas.height()) / 360).clamp(2, 5)) as i32;
    let padding = scale * 2;
    let text_width = bitmap_text_width(label, scale);
    let text_height = 7 * scale;
    let box_width = text_width + padding * 2;
    let box_height = text_height + padding * 2;
    let image_width = canvas.width() as i32;
    let image_height = canvas.height() as i32;
    let point_x = point.x.round() as i32;
    let point_y = point.y.round() as i32;
    let mut x = point_x + 12;
    if x + box_width >= image_width {
        x = point_x - box_width - 12;
    }
    let mut y = point_y - box_height / 2;
    x = x.clamp(0, (image_width - box_width).max(0));
    y = y.clamp(0, (image_height - box_height).max(0));

    draw_filled_rect_mut(
        canvas,
        Rect::at(x, y).of_size(box_width.max(1) as u32, box_height.max(1) as u32),
        Rgba([0, 0, 0, 210]),
    );
    draw_bitmap_text(
        canvas,
        label,
        x + padding,
        y + padding,
        scale,
        Rgba([255, 255, 255, 255]),
    );
}

fn bitmap_text_width(text: &str, scale: i32) -> i32 {
    text.chars()
        .map(|ch| if ch == ' ' { 4 * scale } else { 6 * scale })
        .sum::<i32>()
        .saturating_sub(scale)
}

fn draw_bitmap_text(
    canvas: &mut RgbaImage,
    text: &str,
    x: i32,
    y: i32,
    scale: i32,
    color: Rgba<u8>,
) {
    let mut cursor = x;
    for ch in text.chars() {
        if ch == ' ' {
            cursor += 4 * scale;
            continue;
        }
        if let Some(glyph) = bitmap_glyph(ch) {
            for (row, cells) in glyph.iter().enumerate() {
                for (col, cell) in cells.as_bytes().iter().enumerate() {
                    if *cell == b'1' {
                        draw_filled_rect_mut(
                            canvas,
                            Rect::at(cursor + col as i32 * scale, y + row as i32 * scale)
                                .of_size(scale as u32, scale as u32),
                            color,
                        );
                    }
                }
            }
        }
        cursor += 6 * scale;
    }
}

fn bitmap_glyph(ch: char) -> Option<[&'static str; 7]> {
    match ch {
        'A' => Some([
            "01110", "10001", "10001", "11111", "10001", "10001", "10001",
        ]),
        'C' => Some([
            "01111", "10000", "10000", "10000", "10000", "10000", "01111",
        ]),
        'D' => Some([
            "11110", "10001", "10001", "10001", "10001", "10001", "11110",
        ]),
        'E' => Some([
            "11111", "10000", "10000", "11110", "10000", "10000", "11111",
        ]),
        'H' => Some([
            "10001", "10001", "10001", "11111", "10001", "10001", "10001",
        ]),
        'I' => Some([
            "11111", "00100", "00100", "00100", "00100", "00100", "11111",
        ]),
        'L' => Some([
            "10000", "10000", "10000", "10000", "10000", "10000", "11111",
        ]),
        'O' => Some([
            "01110", "10001", "10001", "10001", "10001", "10001", "01110",
        ]),
        'P' => Some([
            "11110", "10001", "10001", "11110", "10000", "10000", "10000",
        ]),
        'R' => Some([
            "11110", "10001", "10001", "11110", "10100", "10010", "10001",
        ]),
        'S' => Some([
            "01111", "10000", "10000", "01110", "00001", "00001", "11110",
        ]),
        'U' => Some([
            "10001", "10001", "10001", "10001", "10001", "10001", "01110",
        ]),
        '7' => Some([
            "11111", "00001", "00010", "00100", "01000", "01000", "01000",
        ]),
        _ => None,
    }
}

fn write_tag_report(
    input: &Path,
    img: &DynamicImage,
    detections: &[DetectionPoint],
    posture: Option<&PostureFrame>,
    out: &Path,
) -> Result<()> {
    ensure_parent(out)?;
    let mut ids = detections.iter().map(|det| det.id).collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();

    let present = [EAR_ID, C7_ID, SHOULDER_ID, HIP_ID]
        .into_iter()
        .filter(|id| ids.contains(id))
        .map(tag_short_label)
        .collect::<Vec<_>>();
    let missing_required = [EAR_ID, C7_ID, SHOULDER_ID]
        .into_iter()
        .filter(|id| !ids.contains(id))
        .map(tag_short_label)
        .collect::<Vec<_>>();
    let status = if missing_required.is_empty() {
        "ready"
    } else {
        "missing"
    };

    let mut lines = vec![
        format!("input={}", input.display()),
        format!("image_width={}", img.width()),
        format!("image_height={}", img.height()),
        format!("status={status}"),
        format!("present={}", present.join(" ")),
        format!("missing_required={}", missing_required.join(" ")),
        format!("tag_count={}", detections.len()),
        format!("hip_present={}", ids.contains(&HIP_ID)),
    ];

    let mode_estimate = estimate_mode_from_detections(detections);
    lines.push(format!("detected_mode={}", mode_estimate.mode.label()));
    lines.push(format!(
        "detected_mode_confidence={}",
        mode_estimate.confidence
    ));
    lines.push(format!("detected_mode_detail={}", mode_estimate.detail));
    let placement = estimate_placement_from_detections(detections);
    lines.push(format!("placement_status={}", placement.status.label()));
    lines.push(format!("placement_score={}", placement.score));
    lines.push(format!("placement_action={}", placement.action));
    lines.push(format!("placement_detail={}", placement.detail));

    for det in detections {
        lines.push(format!("tag.{}.label={}", det.id, tag_short_label(det.id)));
        lines.push(format!("tag.{}.center_x={:.1}", det.id, det.center.x));
        lines.push(format!("tag.{}.center_y={:.1}", det.id, det.center.y));
        lines.push(format!("tag.{}.edge_px={:.1}", det.id, tag_edge_px(det)));
    }

    match posture {
        Some(posture) => {
            lines.push(format!(
                "posture.cva_degrees={}",
                posture
                    .cva_degrees
                    .map(|v| format!("{v:.1}"))
                    .unwrap_or_else(|| "n/a".to_string())
            ));
            lines.push(format!(
                "posture.head_forward_px={}",
                posture
                    .head_forward_px
                    .map(|v| format!("{v:.1}"))
                    .unwrap_or_else(|| "n/a".to_string())
            ));
            lines.push(format!(
                "posture.display_points={}",
                format_display_points(&posture.display_points)
            ));
        }
        None => {
            lines.push("posture.cva_degrees=n/a".to_string());
            lines.push("posture.head_forward_px=n/a".to_string());
            lines.push("posture.display_points=".to_string());
        }
    }

    fs::write(out, lines.join("\n") + "\n")
        .with_context(|| format!("writing {}", out.display()))?;
    Ok(())
}

#[derive(Debug, Clone)]
struct BaselineSample {
    path: PathBuf,
    cva_degrees: f64,
    head_forward_px: f64,
    display_points: Vec<(usize, Point)>,
}

#[derive(Debug, Clone)]
struct BaselineSummary {
    mode: DetectedDeskMode,
    accepted: Vec<BaselineSample>,
    ignored: usize,
}

#[derive(Debug, Clone, Copy)]
struct SampleStats {
    count: usize,
    mean: f64,
    stddev: f64,
    min: f64,
    max: f64,
}

#[derive(Debug, Clone)]
struct PostureBaseline {
    modes: BTreeMap<DetectedDeskMode, ModeBaseline>,
}

#[derive(Debug, Clone)]
struct ModeBaseline {
    ready: bool,
    accepted: usize,
    cva_degrees: Option<f64>,
    head_forward_px: Option<f64>,
    display_points: Vec<(usize, Point)>,
}

#[derive(Debug, Clone, PartialEq)]
struct BaselineDrift {
    mode: DetectedDeskMode,
    accepted_samples: usize,
    cva_delta_degrees: Option<f64>,
    head_forward_delta_px: Option<f64>,
    baseline_display_points: Vec<(usize, Point)>,
}

impl BaselineDrift {
    fn note(&self) -> String {
        let mode = match self.mode {
            DetectedDeskMode::Sitting => "sit",
            DetectedDeskMode::Standing => "std",
            DetectedDeskMode::Unknown => "base",
        };
        if let Some(delta) = self.cva_delta_degrees {
            return format!("{mode} {}", signed_round(delta, "deg"));
        }
        if let Some(delta) = self.head_forward_delta_px {
            return format!("{mode} {}", signed_round(delta, "px"));
        }
        format!("{mode} base")
    }
}

fn calibrate_baseline(
    samples_dir: Option<&Path>,
    out: Option<&Path>,
    min_samples: usize,
) -> Result<()> {
    ensure!(min_samples > 0, "--min-samples must be at least 1");
    let samples_dir = match samples_dir {
        Some(path) => path.to_path_buf(),
        None => default_samples_dir()?,
    };
    let out = match out {
        Some(path) => path.to_path_buf(),
        None => default_calibration_path()?,
    };
    let text = build_calibration_baseline(&samples_dir, min_samples)?;
    ensure_parent(&out)?;
    fs::write(&out, &text).with_context(|| format!("writing {}", out.display()))?;
    print!("{text}");
    println!("wrote {}", out.display());
    Ok(())
}

fn build_calibration_baseline(samples_dir: &Path, min_samples: usize) -> Result<String> {
    ensure!(min_samples > 0, "min_samples must be at least 1");
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let mut lines = vec![
        format!("created_at_unix={created_at}"),
        format!("samples_dir={}", samples_dir.display()),
        format!("min_samples_per_mode={min_samples}"),
    ];

    for mode in [DetectedDeskMode::Standing, DetectedDeskMode::Sitting] {
        let summary = summarize_baseline_mode(samples_dir, mode)?;
        append_baseline_summary(&mut lines, &summary, min_samples);
    }

    Ok(lines.join("\n") + "\n")
}

fn summarize_baseline_mode(samples_dir: &Path, mode: DetectedDeskMode) -> Result<BaselineSummary> {
    let dir = samples_dir.join(mode.label());
    let mut accepted = Vec::new();
    let mut ignored = 0;

    if !dir.exists() {
        return Ok(BaselineSummary {
            mode,
            accepted,
            ignored,
        });
    }

    let mut reports = WalkDir::new(&dir)
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(Result::ok)
        .map(|entry| entry.into_path())
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .map(|name| name.ends_with("-tags.txt"))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    reports.sort();

    for report in reports {
        let values = parse_key_value_file(&report)?;
        if values.get("status").map(String::as_str) != Some("ready")
            || values.get("placement_status").map(String::as_str) != Some("good")
        {
            ignored += 1;
            continue;
        }

        let Some(cva_degrees) = parse_report_f64(&values, "posture.cva_degrees") else {
            ignored += 1;
            continue;
        };
        let Some(head_forward_px) = parse_report_f64(&values, "posture.head_forward_px") else {
            ignored += 1;
            continue;
        };

        accepted.push(BaselineSample {
            path: report,
            cva_degrees,
            head_forward_px,
            display_points: values
                .get("posture.display_points")
                .map(|value| parse_display_points(value))
                .transpose()?
                .unwrap_or_default(),
        });
    }

    Ok(BaselineSummary {
        mode,
        accepted,
        ignored,
    })
}

fn append_baseline_summary(lines: &mut Vec<String>, summary: &BaselineSummary, min_samples: usize) {
    let label = summary.mode.label();
    let status = if summary.accepted.len() >= min_samples {
        "ready"
    } else {
        "needs_more_samples"
    };
    lines.push(format!("mode.{label}.status={status}"));
    lines.push(format!("mode.{label}.accepted={}", summary.accepted.len()));
    lines.push(format!("mode.{label}.ignored={}", summary.ignored));

    let cva = summary
        .accepted
        .iter()
        .map(|sample| sample.cva_degrees)
        .collect::<Vec<_>>();
    append_stats(
        lines,
        &format!("mode.{label}.cva_degrees"),
        sample_stats(&cva),
    );

    let head_forward = summary
        .accepted
        .iter()
        .map(|sample| sample.head_forward_px)
        .collect::<Vec<_>>();
    append_stats(
        lines,
        &format!("mode.{label}.head_forward_px"),
        sample_stats(&head_forward),
    );

    let display_points = average_display_points(&summary.accepted);
    lines.push(format!(
        "mode.{label}.display_points={}",
        format_display_points(&display_points)
    ));

    for (idx, sample) in summary.accepted.iter().enumerate() {
        lines.push(format!(
            "mode.{label}.sample.{}={}",
            idx + 1,
            sample.path.display()
        ));
    }
}

fn append_stats(lines: &mut Vec<String>, prefix: &str, stats: Option<SampleStats>) {
    match stats {
        Some(stats) => {
            lines.push(format!("{prefix}.count={}", stats.count));
            lines.push(format!("{prefix}.mean={:.2}", stats.mean));
            lines.push(format!("{prefix}.stddev={:.2}", stats.stddev));
            lines.push(format!("{prefix}.min={:.2}", stats.min));
            lines.push(format!("{prefix}.max={:.2}", stats.max));
        }
        None => {
            lines.push(format!("{prefix}.count=0"));
        }
    }
}

fn sample_stats(values: &[f64]) -> Option<SampleStats> {
    let count = values.len();
    if count == 0 {
        return None;
    }
    let mean = values.iter().sum::<f64>() / count as f64;
    let variance = values
        .iter()
        .map(|value| {
            let delta = value - mean;
            delta * delta
        })
        .sum::<f64>()
        / count as f64;
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Some(SampleStats {
        count,
        mean,
        stddev: variance.sqrt(),
        min,
        max,
    })
}

fn average_display_points(samples: &[BaselineSample]) -> Vec<(usize, Point)> {
    let mut sums: BTreeMap<usize, (f64, f64, usize)> = BTreeMap::new();
    for sample in samples {
        for (id, point) in &sample.display_points {
            let entry = sums.entry(*id).or_insert((0.0, 0.0, 0));
            entry.0 += point.x;
            entry.1 += point.y;
            entry.2 += 1;
        }
    }
    [HIP_ID, SHOULDER_ID, C7_ID, EAR_ID]
        .into_iter()
        .filter_map(|id| {
            let (x, y, count) = sums.get(&id)?;
            Some((id, Point::new(x / *count as f64, y / *count as f64)))
        })
        .collect()
}

fn parse_key_value_file(path: &Path) -> Result<BTreeMap<String, String>> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(text
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect())
}

fn parse_report_f64(values: &BTreeMap<String, String>, key: &str) -> Option<f64> {
    values.get(key)?.parse().ok()
}

fn format_display_points(points: &[(usize, Point)]) -> String {
    points
        .iter()
        .map(|(id, point)| format!("{}:{:.1}:{:.1}", tag_short_label(*id), point.x, point.y))
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_display_points(value: &str) -> Result<Vec<(usize, Point)>> {
    value
        .split_whitespace()
        .map(|entry| {
            let mut parts = entry.split(':');
            let label = parts
                .next()
                .ok_or_else(|| anyhow!("missing display point label"))?;
            let x = parts
                .next()
                .ok_or_else(|| anyhow!("missing display point x for {label}"))?
                .parse::<f64>()
                .with_context(|| format!("parsing display point x for {label}"))?;
            let y = parts
                .next()
                .ok_or_else(|| anyhow!("missing display point y for {label}"))?
                .parse::<f64>()
                .with_context(|| format!("parsing display point y for {label}"))?;
            ensure!(
                parts.next().is_none(),
                "too many fields in display point {entry}"
            );
            let id = tag_id_from_short_label(label)
                .ok_or_else(|| anyhow!("unknown display point label {label}"))?;
            Ok((id, Point::new(x, y)))
        })
        .collect()
}

fn tag_id_from_short_label(label: &str) -> Option<usize> {
    match label {
        "ear" => Some(EAR_ID),
        "C7" | "c7" => Some(C7_ID),
        "shoulder" => Some(SHOULDER_ID),
        "hip" => Some(HIP_ID),
        _ => None,
    }
}

fn baseline_drift_for_file(
    path: &Path,
    mode: DetectedDeskMode,
    posture: &PostureFrame,
) -> Result<Option<BaselineDrift>> {
    if !path.exists() || mode == DetectedDeskMode::Unknown {
        return Ok(None);
    }
    let baseline = read_posture_baseline(path)?;
    Ok(baseline.drift_for(mode, posture))
}

fn read_posture_baseline(path: &Path) -> Result<PostureBaseline> {
    let values = parse_key_value_file(path)?;
    let mut modes = BTreeMap::new();
    for mode in [DetectedDeskMode::Standing, DetectedDeskMode::Sitting] {
        if let Some(baseline) = parse_mode_baseline(&values, mode) {
            modes.insert(mode, baseline);
        }
    }
    Ok(PostureBaseline { modes })
}

fn parse_mode_baseline(
    values: &BTreeMap<String, String>,
    mode: DetectedDeskMode,
) -> Option<ModeBaseline> {
    let prefix = format!("mode.{}", mode.label());
    let status = values.get(&format!("{prefix}.status"))?;
    let accepted = values
        .get(&format!("{prefix}.accepted"))
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    Some(ModeBaseline {
        ready: status == "ready",
        accepted,
        cva_degrees: parse_report_f64(values, &format!("{prefix}.cva_degrees.mean")),
        head_forward_px: parse_report_f64(values, &format!("{prefix}.head_forward_px.mean")),
        display_points: values
            .get(&format!("{prefix}.display_points"))
            .map(|value| parse_display_points(value))
            .transpose()
            .ok()
            .flatten()
            .unwrap_or_default(),
    })
}

impl PostureBaseline {
    fn drift_for(&self, mode: DetectedDeskMode, posture: &PostureFrame) -> Option<BaselineDrift> {
        let baseline = self.modes.get(&mode)?;
        if !baseline.ready {
            return None;
        }
        Some(BaselineDrift {
            mode,
            accepted_samples: baseline.accepted,
            cva_delta_degrees: posture
                .cva_degrees
                .zip(baseline.cva_degrees)
                .map(|(now, base)| now - base),
            head_forward_delta_px: posture
                .head_forward_px
                .zip(baseline.head_forward_px)
                .map(|(now, base)| now - base),
            baseline_display_points: baseline.display_points.clone(),
        })
    }
}

fn signed_round(value: f64, unit: &str) -> String {
    if value >= 0.0 {
        format!("+{value:.0}{unit}")
    } else {
        format!("{value:.0}{unit}")
    }
}

fn default_samples_dir() -> Result<PathBuf> {
    Ok(default_app_support_dir()?.join("samples"))
}

fn default_calibration_path() -> Result<PathBuf> {
    Ok(default_app_support_dir()?
        .join("calibration")
        .join("baseline.txt"))
}

fn default_app_support_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Posture Watcher"))
}

fn tag_edge_px(det: &DetectionPoint) -> f64 {
    let mut sum = 0.0;
    for i in 0..4 {
        let a = det.corners[i];
        let b = det.corners[(i + 1) % 4];
        let dx = a[0] - b[0];
        let dy = a[1] - b[1];
        sum += (dx * dx + dy * dy).sqrt();
    }
    sum / 4.0
}

fn draw_tag_outline(canvas: &mut RgbaImage, det: &DetectionPoint) {
    for i in 0..4 {
        let a = det.corners[i];
        let b = det.corners[(i + 1) % 4];
        draw_line_segment_mut(
            canvas,
            (a[0] as f32, a[1] as f32),
            (b[0] as f32, b[1] as f32),
            Rgba([255, 0, 0, 255]),
        );
    }
}

fn send_to_badger(
    port_name: &str,
    posture: &PostureFrame,
    badger_orientation: BadgerOrientation,
) -> Result<()> {
    let line = badger_payload(posture, None, None, None, badger_orientation);
    emit_display_payload(&line);
    send_payload_to_badger(port_name, &line, "OK,P,")
}

fn publish_posture(
    port_name: &str,
    posture: &PostureFrame,
    drift: Option<&BaselineDrift>,
    quality_bits: Option<&str>,
    badger_orientation: BadgerOrientation,
    send_badger_enabled: bool,
) -> Result<()> {
    let note = drift
        .map(BaselineDrift::note)
        .unwrap_or_else(|| posture_note(posture));
    let baseline_points = drift
        .map(|drift| drift.baseline_display_points.as_slice())
        .filter(|points| !points.is_empty());
    let line = badger_payload(
        posture,
        Some(&note),
        baseline_points,
        quality_bits,
        badger_orientation,
    );
    emit_display_payload(&line);
    if send_badger_enabled {
        if let Err(err) = send_payload_to_badger(port_name, &line, "OK,P,") {
            eprintln!("Badger posture send failed: {err:#}");
            emit_badger_status("disconnected", &format!("{err:#}"));
        }
    }
    Ok(())
}

fn publish_badger_message(
    port_name: &str,
    message: &str,
    badger_orientation: BadgerOrientation,
    send_badger_enabled: bool,
) -> Result<()> {
    let line = badger_message_payload(message, badger_orientation);
    emit_display_payload(&line);
    if send_badger_enabled {
        if let Err(err) = send_payload_to_badger(port_name, &line, "OK,M") {
            eprintln!("Badger message send failed: {err:#}");
            emit_badger_status("disconnected", &format!("{err:#}"));
        }
    }
    Ok(())
}

fn send_badger_message(
    port_name: &str,
    message: &str,
    badger_orientation: BadgerOrientation,
) -> Result<()> {
    let line = badger_message_payload(message, badger_orientation);
    emit_display_payload(&line);
    send_payload_to_badger(port_name, &line, "OK,M")
}

fn send_payload_to_badger(port_name: &str, line: &str, expected_reply_prefix: &str) -> Result<()> {
    let mut port = serialport::new(port_name, 115_200)
        .timeout(Duration::from_millis(100))
        .open()
        .with_context(|| format!("opening serial port {port_name}"))?;
    write_payload(&mut *port, &line)?;
    let reply = read_badger_reply(&mut *port, Duration::from_secs(8))?;
    ensure!(
        reply.starts_with(expected_reply_prefix),
        "Badger rejected payload: {reply}"
    );
    println!("sent to Badger: {} ({reply})", line.trim());
    emit_badger_status("connected", &reply);
    Ok(())
}

fn emit_display_payload(line: &str) {
    println!("DISPLAY,{}", line.trim());
}

fn emit_badger_startup_status(port_name: &str, send_badger_enabled: bool) {
    if !send_badger_enabled {
        emit_badger_status("disabled", "");
        return;
    }
    emit_badger_status("checking", "");
    match ping_badger(port_name) {
        Ok(reply) => emit_badger_status("connected", &reply),
        Err(err) => emit_badger_status("disconnected", &format!("{err:#}")),
    }
}

fn emit_badger_status(status: &str, detail: &str) {
    println!("BADGER,{status},{}", clean_payload_text(detail));
}

fn emit_mode_status(estimate: &ModeEstimate) {
    println!(
        "MODE,{},{},{}",
        estimate.mode.label(),
        estimate.confidence,
        clean_payload_text(&estimate.detail)
    );
}

fn emit_placement_status(estimate: &PlacementEstimate) {
    println!(
        "PLACEMENT,{},{},{},{}",
        estimate.status.label(),
        estimate.score,
        clean_payload_text(&estimate.action),
        clean_payload_text(&estimate.detail)
    );
}

fn write_payload(port: &mut dyn SerialPort, line: &str) -> Result<()> {
    port.write_all(line.as_bytes())
        .context("writing to Badger")?;
    port.flush().context("flushing Badger serial port")?;
    Ok(())
}

fn ping_badger(port_name: &str) -> Result<String> {
    let mut port = serialport::new(port_name, 115_200)
        .timeout(Duration::from_millis(100))
        .open()
        .with_context(|| format!("opening serial port {port_name}"))?;
    write_payload(&mut *port, "PING\n")?;
    let reply = read_badger_reply(&mut *port, Duration::from_secs(4))?;
    ensure!(
        reply == format!("OK,{BADGER_PROTOCOL}"),
        "unexpected Badger reply: {reply}"
    );
    Ok(reply)
}

fn read_badger_reply(port: &mut dyn SerialPort, timeout: Duration) -> Result<String> {
    let deadline = Instant::now() + timeout;
    let mut line = Vec::new();
    while Instant::now() < deadline {
        let mut byte = [0_u8; 1];
        match port.read(&mut byte) {
            Ok(0) => {}
            Ok(_) => {
                if byte[0] == b'\n' || byte[0] == b'\r' {
                    if line.is_empty() {
                        continue;
                    }
                    let text = String::from_utf8_lossy(&line).trim().to_string();
                    if text.starts_with("READY,") {
                        line.clear();
                        continue;
                    }
                    return Ok(text);
                }
                line.push(byte[0]);
            }
            Err(err) if err.kind() == ErrorKind::TimedOut => {}
            Err(err) => return Err(err).context("reading Badger reply"),
        }
    }
    bail!("timed out waiting for Badger ACK")
}

fn badger_payload(
    posture: &PostureFrame,
    note: Option<&str>,
    baseline_points: Option<&[(usize, Point)]>,
    quality_bits: Option<&str>,
    badger_orientation: BadgerOrientation,
) -> String {
    let mut parts = vec!["P".to_string()];
    if let Some(code) = badger_orientation.protocol_code() {
        parts.push(code.to_string());
    }
    parts.push(posture.display_points.len().to_string());
    for (_, p) in &posture.display_points {
        parts.push((p.x.round() as i32).clamp(0, BADGER_WIDTH - 1).to_string());
        parts.push((p.y.round() as i32).clamp(0, BADGER_HEIGHT - 1).to_string());
    }
    let fallback_note;
    let note = match note {
        Some(note) => note,
        None => {
            fallback_note = posture_note(posture);
            &fallback_note
        }
    };
    parts.push(clean_payload_text(note));
    if let Some(baseline_points) = baseline_points {
        parts.push("B".to_string());
        parts.push(baseline_points.len().to_string());
        for (_, p) in baseline_points {
            parts.push((p.x.round() as i32).clamp(0, BADGER_WIDTH - 1).to_string());
            parts.push((p.y.round() as i32).clamp(0, BADGER_HEIGHT - 1).to_string());
        }
    }
    if let Some(bits) = quality_bits {
        if !bits.is_empty() {
            parts.push("Q".to_string());
            parts.push(
                bits.chars()
                    .filter(|bit| *bit == '0' || *bit == '1')
                    .collect(),
            );
        }
    }
    parts.join(",") + "\n"
}

fn posture_note(posture: &PostureFrame) -> String {
    format!(
        "cva={}",
        posture
            .cva_degrees
            .map(|v| format!("{v:.1}"))
            .unwrap_or_else(|| "na".to_string())
    )
}

fn badger_message_payload(message: &str, badger_orientation: BadgerOrientation) -> String {
    match badger_orientation.protocol_code() {
        Some(code) => format!("M,{code},{}\n", clean_payload_text(message)),
        None => format!("M,{}\n", clean_payload_text(message)),
    }
}

fn clean_payload_text(message: &str) -> String {
    message
        .replace([',', '\n', '\r'], " ")
        .trim()
        .chars()
        .take(80)
        .collect()
}

fn print_posture(path: &Path, posture: &PostureFrame, drift: Option<&BaselineDrift>) {
    let mut line = format!(
        "{}: detected={} points={} cva={} head_forward_px={}",
        path.display(),
        posture.detected_count,
        posture.display_points.len(),
        posture
            .cva_degrees
            .map(|v| format!("{v:.1}"))
            .unwrap_or_else(|| "n/a".to_string()),
        posture
            .head_forward_px
            .map(|v| format!("{v:.1}"))
            .unwrap_or_else(|| "n/a".to_string())
    );
    if let Some(drift) = drift {
        line.push_str(&format!(
            " baseline_mode={} cva_delta={} head_forward_delta={}",
            drift.mode.label(),
            drift
                .cva_delta_degrees
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "n/a".to_string()),
            drift
                .head_forward_delta_px
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "n/a".to_string())
        ));
    }
    println!("{line}");
}

impl RollingWindow {
    fn new(window: Duration) -> Self {
        Self {
            window,
            frames: VecDeque::new(),
        }
    }

    fn push(&mut self, frame: PostureFrame) {
        let now = Instant::now();
        self.frames.push_back((now, frame));
        while let Some((ts, _)) = self.frames.front() {
            if now.duration_since(*ts) <= self.window {
                break;
            }
            self.frames.pop_front();
        }
    }

    fn average(&self) -> Option<PostureFrame> {
        if self.frames.is_empty() {
            return None;
        }
        let mut sums: BTreeMap<usize, (f64, f64, usize)> = BTreeMap::new();
        let mut cva_sum = 0.0;
        let mut cva_count = 0;
        let mut head_sum = 0.0;
        let mut head_count = 0;
        let mut detected_count = 0;
        for (_, frame) in &self.frames {
            detected_count = detected_count.max(frame.detected_count);
            for (id, point) in &frame.landmarks {
                let entry = sums.entry(*id).or_insert((0.0, 0.0, 0));
                entry.0 += point.x;
                entry.1 += point.y;
                entry.2 += 1;
            }
            if let Some(cva) = frame.cva_degrees {
                cva_sum += cva;
                cva_count += 1;
            }
            if let Some(head) = frame.head_forward_px {
                head_sum += head;
                head_count += 1;
            }
        }
        let landmarks = sums
            .into_iter()
            .map(|(id, (x, y, n))| (id, Point::new(x / n as f64, y / n as f64)))
            .collect::<BTreeMap<_, _>>();
        let display_points = display_curve(&landmarks);
        Some(PostureFrame {
            landmarks,
            display_points,
            detected_count,
            cva_degrees: (cva_count > 0).then_some(cva_sum / cva_count as f64),
            head_forward_px: (head_count > 0).then_some(head_sum / head_count as f64),
        })
    }
}

impl ModeRollingWindows {
    fn new(window: Duration) -> Self {
        Self {
            window,
            frames_by_mode: BTreeMap::new(),
        }
    }

    fn push(&mut self, mode: DetectedDeskMode, frame: PostureFrame) -> Option<PostureFrame> {
        let window = self
            .frames_by_mode
            .entry(mode)
            .or_insert_with(|| RollingWindow::new(self.window));
        window.push(frame);
        window.average()
    }
}

impl QualityHistory {
    fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn push(&mut self, good: bool) {
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(good);
    }

    fn bits(&self) -> String {
        self.entries
            .iter()
            .map(|good| if *good { '1' } else { '0' })
            .collect()
    }

    fn consecutive_misses(&self) -> usize {
        self.entries.iter().rev().take_while(|good| !**good).count()
    }
}

impl MissingPersonState {
    fn new() -> Self {
        Self {
            first_missing: None,
            message_sent: false,
        }
    }

    fn clear(&mut self) {
        self.first_missing = None;
        self.message_sent = false;
    }

    fn record_missing(
        &mut self,
        threshold: Duration,
        port: &str,
        send_badger_enabled: bool,
        badger_orientation: BadgerOrientation,
    ) -> Result<()> {
        let now = Instant::now();
        let first_missing = *self.first_missing.get_or_insert(now);
        if !self.message_sent && now.duration_since(first_missing) >= threshold {
            publish_badger_message(
                port,
                NO_PERSON_MESSAGE,
                badger_orientation,
                send_badger_enabled,
            )?;
            self.message_sent = true;
        }
        Ok(())
    }
}

fn landmark_labels() -> Vec<(usize, &'static str)> {
    vec![
        (EAR_ID, "ear / tragus"),
        (C7_ID, "C7"),
        (SHOULDER_ID, "shoulder / acromion"),
        (HIP_ID, "hip / belt optional"),
    ]
}

fn tag_grid(id: usize) -> Result<Vec<Vec<bool>>> {
    let family: TagFamily = TagFamilyKind::Tag36H11.into();
    let code = *family
        .code_data
        .get(id)
        .ok_or_else(|| anyhow!("tag id {id} out of range for tag36h11"))?;
    let mut grid = vec![vec![false; family.total_width]; family.total_width];
    for y in 0..family.total_width {
        for x in 0..family.total_width {
            grid[y][x] = false;
        }
    }
    for y in 0..family.total_width {
        for x in 0..family.total_width {
            let outer =
                x == 0 || y == 0 || x == family.total_width - 1 || y == family.total_width - 1;
            let border =
                x == 1 || y == 1 || x == family.total_width - 2 || y == family.total_width - 2;
            grid[y][x] = if outer { false } else { border };
        }
    }
    for i in 0..family.nbits {
        let bit = ((code >> (family.nbits - 1 - i)) & 1) == 1;
        let x = family.bit_x[i] as usize + 1;
        let y = family.bit_y[i] as usize + 1;
        grid[y][x] = !bit;
    }
    Ok(grid)
}

fn render_tag_rgba(id: usize, size_px: u32) -> Result<RgbaImage> {
    let grid = tag_grid(id)?;
    let cells = grid.len() as u32;
    let cell_px = (size_px / cells).max(1);
    let actual = cell_px * cells;
    let mut img = RgbaImage::from_pixel(actual, actual, Rgba([255, 255, 255, 255]));
    for (gy, row) in grid.iter().enumerate() {
        for (gx, black) in row.iter().enumerate() {
            if *black {
                draw_filled_rect_mut(
                    &mut img,
                    Rect::at((gx as u32 * cell_px) as i32, (gy as u32 * cell_px) as i32)
                        .of_size(cell_px, cell_px),
                    Rgba([0, 0, 0, 255]),
                );
            }
        }
    }
    if actual == size_px {
        Ok(img)
    } else {
        Ok(image::imageops::resize(
            &img,
            size_px,
            size_px,
            FilterType::Nearest,
        ))
    }
}

#[cfg(test)]
fn render_tag_luma(id: usize, size_px: u32) -> Result<GrayImage> {
    let rgba = render_tag_rgba(id, size_px)?;
    let dyn_img = DynamicImage::ImageRgba8(rgba);
    Ok(dyn_img.to_luma8())
}

fn fake_positions(idx: usize, w: u32, h: u32) -> Vec<(usize, Point)> {
    let wf = w as f64;
    let hf = h as f64;
    if h > w {
        vec![
            (EAR_ID, Point::new(wf * 0.45, hf * 0.30)),
            (C7_ID, Point::new(wf * 0.55, hf * 0.43)),
            (SHOULDER_ID, Point::new(wf * 0.68, hf * 0.47)),
            (HIP_ID, Point::new(wf * 0.68, hf * 0.73)),
        ]
    } else {
        let drift = idx as f64 * 0.015;
        vec![
            (EAR_ID, Point::new(wf * (0.73 + drift), hf * 0.36)),
            (C7_ID, Point::new(wf * (0.64 + drift), hf * 0.45)),
            (SHOULDER_ID, Point::new(wf * (0.56 + drift), hf * 0.54)),
            (HIP_ID, Point::new(wf * 0.33, hf * 0.66)),
        ]
    }
}

fn image_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = WalkDir::new(dir)
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(Result::ok)
        .map(|e| e.into_path())
        .filter(|p| {
            p.extension()
                .and_then(OsStr::to_str)
                .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    Ok(())
}

fn open_file(path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        run_cmd(
            Command::new("open").arg(path),
            "opening generated sticker sheet",
        )?;
        return Ok(());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        bail!("--open is only implemented on macOS")
    }
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn mpremote_path() -> PathBuf {
    let local = PathBuf::from(".venv/bin/mpremote");
    if local.exists() {
        local
    } else {
        PathBuf::from("mpremote")
    }
}

fn run_cmd(cmd: &mut Command, description: &str) -> Result<()> {
    let output = cmd.output().with_context(|| description.to_string())?;
    if !output.status.success() {
        bail!(
            "{description} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn latest_backup(dir: &Path) -> Result<Option<PathBuf>> {
    if !dir.exists() {
        return Ok(None);
    }
    let mut files = image_files_any_ext(dir, "py")?;
    files.sort();
    Ok(files.pop())
}

fn image_files_any_ext(dir: &Path, wanted_ext: &str) -> Result<Vec<PathBuf>> {
    Ok(WalkDir::new(dir)
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(Result::ok)
        .map(|e| e.into_path())
        .filter(|p| {
            p.extension()
                .and_then(OsStr::to_str)
                .map(|ext| ext.eq_ignore_ascii_case(wanted_ext))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>())
}

fn timestamp_for_file() -> Result<String> {
    let output = Command::new("date")
        .arg("+%Y%m%d-%H%M%S")
        .output()
        .context("running date")?;
    if !output.status.success() {
        bail!("date failed");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tag_is_detectable() -> Result<()> {
        let tag = render_tag_luma(EAR_ID, 240)?;
        let img = DynamicImage::ImageLuma8(tag);
        let detections = detect_tags(&img)?;
        assert!(detections.iter().any(|d| d.id == EAR_ID));
        Ok(())
    }

    #[test]
    fn tag_report_includes_readiness_and_measurements() -> Result<()> {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(100, 100));
        let detections = vec![
            test_detection(EAR_ID, 20.0, 20.0),
            test_detection(C7_ID, 28.0, 42.0),
            test_detection(SHOULDER_ID, 40.0, 52.0),
            test_detection(HIP_ID, 43.0, 82.0),
        ];
        let posture = posture_from_detections(&detections)?;
        let out = Path::new("target/test-output/tag-report.txt");
        write_tag_report(
            Path::new("frame.jpg"),
            &img,
            &detections,
            Some(&posture),
            out,
        )?;
        let text = fs::read_to_string(out)?;
        assert!(text.contains("status=ready"));
        assert!(text.contains("present=ear C7 shoulder hip"));
        assert!(text.contains("hip_present=true"));
        assert!(text.contains("detected_mode=standing"));
        assert!(text.contains("placement_status=good"));
        assert!(text.contains("placement_action=Ready"));
        assert!(text.contains("tag.0.center_x=20.0"));
        assert!(text.contains("posture.cva_degrees="));
        Ok(())
    }

    #[test]
    fn mode_estimator_uses_shoulder_to_hip_axis() {
        let standing = BTreeMap::from([
            (SHOULDER_ID, Point::new(50.0, 20.0)),
            (HIP_ID, Point::new(56.0, 110.0)),
        ]);
        assert_eq!(
            estimate_mode_from_landmarks(&standing).mode,
            DetectedDeskMode::Standing
        );

        let sitting = BTreeMap::from([
            (SHOULDER_ID, Point::new(50.0, 20.0)),
            (HIP_ID, Point::new(135.0, 36.0)),
        ]);
        assert_eq!(
            estimate_mode_from_landmarks(&sitting).mode,
            DetectedDeskMode::Sitting
        );
    }

    #[test]
    fn mode_override_forces_live_mode() {
        let detections = vec![
            test_detection(SHOULDER_ID, 50.0, 20.0),
            test_detection(HIP_ID, 56.0, 110.0),
        ];
        assert_eq!(
            DeskModeOverride::Auto.estimate(&detections).mode,
            DetectedDeskMode::Standing
        );
        let sitting = DeskModeOverride::Sitting.estimate(&detections);
        assert_eq!(sitting.mode, DetectedDeskMode::Sitting);
        assert_eq!(sitting.confidence, 100);
        assert!(sitting.detail.contains("manual override"));
    }

    #[test]
    fn placement_estimator_flags_implausible_ear_c7_geometry() {
        let detections = vec![
            test_detection(EAR_ID, 247.0, 1186.0),
            test_detection(C7_ID, 429.0, 1187.0),
            test_detection(SHOULDER_ID, 243.0, 1262.0),
            test_detection(HIP_ID, 424.0, 1260.0),
        ];
        let estimate = estimate_placement_from_detections(&detections);
        assert_eq!(estimate.status, PlacementStatus::Check);
        assert_eq!(estimate.action, "Move ear tag up");
        assert!(estimate.detail.contains("ear not above C7"));
    }

    #[test]
    fn placement_estimator_points_to_c7_flag_when_only_c7_is_missing() {
        let detections = vec![
            test_detection(EAR_ID, 247.0, 1186.0),
            test_detection(SHOULDER_ID, 243.0, 1262.0),
            test_detection(HIP_ID, 424.0, 1260.0),
        ];
        let estimate = estimate_placement_from_detections(&detections);
        assert_eq!(estimate.status, PlacementStatus::Missing);
        assert_eq!(estimate.action, "Aim C7 flag");
        assert_eq!(estimate.detail, "needs C7");
    }

    #[test]
    fn placement_estimator_uses_generic_message_when_all_required_tags_are_missing() {
        let estimate = estimate_placement_from_detections(&[]);
        assert_eq!(estimate.status, PlacementStatus::Missing);
        assert_eq!(estimate.action, "Place missing tags");
        assert_eq!(estimate.detail, "needs ear C7 shoulder");
    }

    #[test]
    fn cva_degrees_is_acute_for_left_or_right_facing_profile() {
        let right_facing = cva_degrees(Point::new(80.0, 40.0), Point::new(40.0, 90.0));
        let left_facing = cva_degrees(Point::new(40.0, 40.0), Point::new(80.0, 90.0));
        assert!((right_facing - left_facing).abs() < 0.1);
        assert!((right_facing - 51.3).abs() < 0.5);
    }

    #[test]
    fn calibration_baseline_uses_only_good_samples() -> Result<()> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let root = std::env::temp_dir().join(format!(
            "posture-watcher-baseline-{}-{nanos}",
            std::process::id()
        ));
        let samples = root.join("samples");
        let sitting = samples.join("sitting");
        write_sample_report(&sitting.join("001-tags.txt"), "good", 49.0, 12.0)?;
        write_sample_report(&sitting.join("002-tags.txt"), "good", 50.0, 14.0)?;
        write_sample_report(&sitting.join("003-tags.txt"), "good", 51.0, 16.0)?;
        write_sample_report(&sitting.join("004-tags.txt"), "check", 88.0, 99.0)?;

        let text = build_calibration_baseline(&samples, 3)?;
        assert!(text.contains("mode.sitting.status=ready"));
        assert!(text.contains("mode.sitting.accepted=3"));
        assert!(text.contains("mode.sitting.ignored=1"));
        assert!(text.contains("mode.sitting.cva_degrees.mean=50.00"));
        assert!(text.contains("mode.sitting.head_forward_px.mean=14.00"));
        assert!(text.contains(
            "mode.sitting.display_points=shoulder:204.0:64.0 C7:126.0:70.0 ear:32.0:80.0"
        ));
        assert!(text.contains("mode.standing.status=needs_more_samples"));

        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[test]
    fn baseline_drift_uses_matching_ready_mode() {
        let baseline = PostureBaseline {
            modes: BTreeMap::from([(
                DetectedDeskMode::Sitting,
                ModeBaseline {
                    ready: true,
                    accepted: 3,
                    cva_degrees: Some(50.0),
                    head_forward_px: Some(10.0),
                    display_points: vec![
                        (SHOULDER_ID, Point::new(204.0, 64.0)),
                        (C7_ID, Point::new(126.0, 70.0)),
                        (EAR_ID, Point::new(32.0, 80.0)),
                    ],
                },
            )]),
        };
        let posture = test_posture_frame(47.2, 18.0);
        let drift = baseline
            .drift_for(DetectedDeskMode::Sitting, &posture)
            .expect("sitting baseline should apply");
        assert_eq!(drift.accepted_samples, 3);
        assert!((drift.cva_delta_degrees.unwrap() + 2.8).abs() < 0.1);
        assert!((drift.head_forward_delta_px.unwrap() - 8.0).abs() < 0.1);
        assert_eq!(drift.baseline_display_points.len(), 3);
        assert_eq!(drift.note(), "sit -3deg");
        assert!(baseline
            .drift_for(DetectedDeskMode::Standing, &posture)
            .is_none());
    }

    #[test]
    fn badger_payload_includes_baseline_note() {
        let mut posture = test_posture_frame(47.2, 18.0);
        posture.display_points = vec![
            (SHOULDER_ID, Point::new(204.0, 64.0)),
            (C7_ID, Point::new(126.0, 70.0)),
            (EAR_ID, Point::new(32.0, 80.0)),
        ];
        let payload = badger_payload(
            &posture,
            Some("sit -3deg"),
            None,
            None,
            BadgerOrientation::UsbTop,
        );
        assert_eq!(payload.trim(), "P,3,204,64,126,70,32,80,sit -3deg");
    }

    #[test]
    fn badger_payload_can_include_baseline_curve() {
        let mut posture = test_posture_frame(47.2, 18.0);
        posture.display_points = vec![
            (SHOULDER_ID, Point::new(204.0, 72.0)),
            (C7_ID, Point::new(126.0, 78.0)),
            (EAR_ID, Point::new(32.0, 88.0)),
        ];
        let baseline = vec![
            (SHOULDER_ID, Point::new(204.0, 64.0)),
            (C7_ID, Point::new(126.0, 70.0)),
            (EAR_ID, Point::new(32.0, 80.0)),
        ];
        let payload = badger_payload(
            &posture,
            Some("sit -3deg"),
            Some(&baseline),
            Some("101"),
            BadgerOrientation::UsbTop,
        );
        assert_eq!(
            payload.trim(),
            "P,3,204,72,126,78,32,88,sit -3deg,B,3,204,64,126,70,32,80,Q,101"
        );
    }

    #[test]
    fn badger_payload_can_request_usb_bottom_orientation() {
        let mut posture = test_posture_frame(47.2, 18.0);
        posture.display_points = vec![
            (SHOULDER_ID, Point::new(204.0, 64.0)),
            (C7_ID, Point::new(126.0, 70.0)),
            (EAR_ID, Point::new(32.0, 80.0)),
        ];
        let payload = badger_payload(
            &posture,
            Some("sit -3deg"),
            None,
            None,
            BadgerOrientation::UsbBottom,
        );
        assert_eq!(payload.trim(), "P,B,3,204,64,126,70,32,80,sit -3deg");
        assert_eq!(
            badger_message_payload("Check markers", BadgerOrientation::UsbBottom).trim(),
            "M,B,Check markers"
        );
    }

    #[test]
    fn rolling_window_keeps_modes_separate() {
        let mut windows = ModeRollingWindows::new(Duration::from_secs(120));
        let sitting = windows
            .push(DetectedDeskMode::Sitting, test_posture_frame(40.0, 10.0))
            .expect("sitting window");
        assert_eq!(sitting.cva_degrees, Some(40.0));

        let standing = windows
            .push(DetectedDeskMode::Standing, test_posture_frame(70.0, 20.0))
            .expect("standing window");
        assert_eq!(standing.cva_degrees, Some(70.0));

        let sitting = windows
            .push(DetectedDeskMode::Sitting, test_posture_frame(50.0, 20.0))
            .expect("sitting average");
        assert_eq!(sitting.cva_degrees, Some(45.0));
        assert_eq!(sitting.head_forward_px, Some(15.0));
    }

    #[test]
    fn burst_fusion_combines_tags_seen_across_frames() {
        let frames = vec![
            test_analyzed_frame(vec![
                test_detection(EAR_ID, 10.0, 10.0),
                test_detection(SHOULDER_ID, 10.0, 40.0),
            ]),
            test_analyzed_frame(vec![
                test_detection(C7_ID, 10.0, 25.0),
                test_detection(HIP_ID, 10.0, 80.0),
            ]),
        ];
        let fused = fuse_burst_detections(&frames, &frames[0].detections);
        let ids = fused.iter().map(|det| det.id).collect::<Vec<_>>();
        assert_eq!(ids, vec![EAR_ID, C7_ID, SHOULDER_ID, HIP_ID]);
    }

    #[test]
    fn c7_anchor_correction_moves_flag_center_toward_neck_line() {
        let detections = vec![
            test_detection(EAR_ID, 0.0, 0.0),
            test_detection(C7_ID, 20.0, 20.0),
            test_detection(SHOULDER_ID, 0.0, 40.0),
        ];
        let corrected = apply_c7_anchor_correction(&detections, 1.0);
        let c7 = corrected
            .iter()
            .find(|det| det.id == C7_ID)
            .expect("C7 detection");
        assert!((c7.center.x - 0.0).abs() < 0.1);
        assert!((c7.center.y - 20.0).abs() < 0.1);
    }

    fn test_analyzed_frame(detections: Vec<DetectionPoint>) -> AnalyzedFrame {
        AnalyzedFrame {
            path: PathBuf::from("test.jpg"),
            img: DynamicImage::new_rgba8(1, 1),
            detections,
            frame_count: 1,
        }
    }

    fn test_detection(id: usize, x: f64, y: f64) -> DetectionPoint {
        DetectionPoint {
            id,
            center: Point::new(x, y),
            corners: [
                [x - 10.0, y - 10.0],
                [x + 10.0, y - 10.0],
                [x + 10.0, y + 10.0],
                [x - 10.0, y + 10.0],
            ],
        }
    }

    fn test_posture_frame(cva_degrees: f64, head_forward_px: f64) -> PostureFrame {
        PostureFrame {
            landmarks: BTreeMap::new(),
            display_points: Vec::new(),
            detected_count: 3,
            cva_degrees: Some(cva_degrees),
            head_forward_px: Some(head_forward_px),
        }
    }

    fn write_sample_report(
        path: &Path,
        placement_status: &str,
        cva_degrees: f64,
        head_forward_px: f64,
    ) -> Result<()> {
        ensure_parent(path)?;
        fs::write(
            path,
            format!(
                "status=ready\nplacement_status={placement_status}\nposture.cva_degrees={cva_degrees}\nposture.head_forward_px={head_forward_px}\nposture.display_points=shoulder:204.0:64.0 C7:126.0:70.0 ear:32.0:80.0\n"
            ),
        )?;
        Ok(())
    }
}
