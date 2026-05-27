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
use std::time::{Duration, Instant, SystemTime};
use walkdir::WalkDir;

const BADGER_WIDTH: i32 = 296;
const BADGER_HEIGHT: i32 = 128;
const BADGER_PROTOCOL: &str = "POSTURE_WATCHER_BADGER_V2";
const DEFAULT_LIVE_INTERVAL_SECS: u64 = 5;
const DEFAULT_NO_PERSON_AFTER_SECS: u64 = 30;
const NO_PERSON_MESSAGE: &str = "No person found";

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
        #[arg(long)]
        send_badger: bool,
        #[arg(long, default_value = "/dev/cu.usbmodem83201")]
        port: String,
    },
    /// Analyze all tagged sample images in order, write debug images, and optionally send to Badger.
    RunSamples {
        #[arg(long, default_value = "artifacts/tagged-samples")]
        input_dir: PathBuf,
        #[arg(long, default_value = "artifacts/analysis")]
        out_dir: PathBuf,
        #[arg(long, value_enum, default_value_t = FrameRotation::None)]
        rotate: FrameRotation,
        #[arg(long)]
        send_badger: bool,
        #[arg(long, default_value = "/dev/cu.usbmodem83201")]
        port: String,
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
        #[arg(long, default_value = "artifacts/live")]
        out_dir: PathBuf,
        #[arg(long)]
        no_badger: bool,
    },
    /// Analyze a repeatedly updated image file, preserving rolling posture state.
    LiveFile {
        #[arg(long)]
        input: PathBuf,
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
        #[arg(long, default_value = "artifacts/live-file")]
        out_dir: PathBuf,
        #[arg(long)]
        no_badger: bool,
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
    },
    /// Send a status message to the Badger.
    SendStatus {
        #[arg(long, default_value = "/dev/cu.usbmodem83201")]
        port: String,
        #[arg(long, default_value = "No person found")]
        message: String,
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

#[derive(Debug, Clone, Copy)]
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
struct MissingPersonState {
    first_missing: Option<Instant>,
    message_sent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameAnalysisOutcome {
    Posture,
    MissingRequiredTags,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Stickers { out, tag_mm } => generate_stickers(&out, tag_mm),
        Commands::AnnotateSamples {
            input_dir,
            out_dir,
            tag_px,
        } => annotate_samples(&input_dir, &out_dir, tag_px),
        Commands::Analyze {
            input,
            annotated_out,
            rotate,
            send_badger,
            port,
        } => {
            let img =
                image::open(&input).with_context(|| format!("opening {}", input.display()))?;
            let img = apply_rotation(img, rotate);
            let detections = detect_tags(&img)?;
            let posture = posture_from_detections(&detections)?;
            print_posture(&input, &posture);
            if let Some(out) = annotated_out {
                write_debug_image(&img, &detections, &posture, &out)?;
                println!("wrote {}", out.display());
            }
            if send_badger {
                send_to_badger(&port, &posture)?;
            }
            Ok(())
        }
        Commands::RunSamples {
            input_dir,
            out_dir,
            rotate,
            send_badger,
            port,
            window_secs,
            delay_ms,
        } => run_samples(
            &input_dir,
            &out_dir,
            rotate,
            send_badger,
            &port,
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
            out_dir,
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
            &out_dir,
            !no_badger,
        ),
        Commands::LiveFile {
            input,
            port,
            window_secs,
            interval_secs,
            no_person_after_secs,
            rotate,
            out_dir,
            no_badger,
        } => live_file(
            &input,
            &port,
            window_secs,
            interval_secs,
            Duration::from_secs(no_person_after_secs),
            rotate,
            &out_dir,
            !no_badger,
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
        Commands::SendDemo { port, pose } => send_demo(&port, pose),
        Commands::SendStatus { port, message } => send_badger_message(&port, &message),
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
        svg.push_str(&format!(
            r#"<text class="tiny" x="{}" y="{}">Print at 100%; tape to skin/tight layer.</text>
"#,
            x,
            y + tag_mm + 9.0
        ));
        svg.push_str(&tag_svg(*id, x, y, tag_mm)?);
        svg.push_str(&format!(
            r#"<rect x="{x}" y="{y}" width="{tag_mm}" height="{tag_mm}" fill="none" stroke="black" stroke-width="0.15" stroke-dasharray="1 1"/>
"#
        ));
    }
    svg.push_str("</svg>\n");
    fs::write(out, svg).with_context(|| format!("writing {}", out.display()))?;
    println!("wrote {}", out.display());
    Ok(())
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

fn run_samples(
    input_dir: &Path,
    out_dir: &Path,
    rotate: FrameRotation,
    send_badger_enabled: bool,
    port: &str,
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
        let posture = posture_from_detections(&detections)?;
        window.push(posture);
        let avg = window.average().context("rolling average is empty")?;
        print_posture(&path, &avg);
        let out = out_dir.join(format!(
            "{}-analysis.png",
            path.file_stem().and_then(OsStr::to_str).unwrap_or("sample")
        ));
        write_debug_image(&img, &detections, &avg, &out)?;
        println!("wrote {}", out.display());
        if send_badger_enabled {
            publish_posture(port, &avg, true)?;
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
    out_dir: &Path,
    send_badger_enabled: bool,
) -> Result<()> {
    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let mut window = RollingWindow::new(Duration::from_secs(window_secs));
    let mut missing_person = MissingPersonState::new();
    println!(
        "starting live capture from {camera}; press Ctrl-C to stop; interval={}s window={}s rotate={rotate:?}",
        interval_secs, window_secs
    );
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
            out_dir,
            &mut window,
            &mut missing_person,
            send_badger_enabled,
            port,
            no_person_after,
            rotate,
        ) {
            Ok(FrameAnalysisOutcome::Posture) => {}
            Ok(FrameAnalysisOutcome::MissingRequiredTags) => {}
            Err(err) => {
                eprintln!("{}: {err:#}", capture.display());
            }
        }
        thread::sleep(Duration::from_secs(interval_secs));
    }
}

fn live_file(
    input: &Path,
    port: &str,
    window_secs: u64,
    interval_secs: u64,
    no_person_after: Duration,
    rotate: FrameRotation,
    out_dir: &Path,
    send_badger_enabled: bool,
) -> Result<()> {
    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let mut window = RollingWindow::new(Duration::from_secs(window_secs));
    let mut missing_person = MissingPersonState::new();
    println!(
        "starting live-file from {}; press Ctrl-C to stop; interval={}s window={}s rotate={rotate:?}",
        input.display(),
        interval_secs,
        window_secs
    );

    loop {
        if !input.exists() {
            eprintln!("waiting for {}", input.display());
            thread::sleep(Duration::from_secs(interval_secs));
            continue;
        }

        match analyze_frame_file(
            input,
            out_dir,
            &mut window,
            &mut missing_person,
            send_badger_enabled,
            port,
            no_person_after,
            rotate,
        ) {
            Ok(FrameAnalysisOutcome::Posture) => {}
            Ok(FrameAnalysisOutcome::MissingRequiredTags) => {}
            Err(err) => eprintln!("{}: {err:#}", input.display()),
        }

        thread::sleep(Duration::from_secs(interval_secs));
    }
}

fn analyze_frame_file(
    input: &Path,
    out_dir: &Path,
    window: &mut RollingWindow,
    missing_person: &mut MissingPersonState,
    send_badger_enabled: bool,
    port: &str,
    no_person_after: Duration,
    rotate: FrameRotation,
) -> Result<FrameAnalysisOutcome> {
    let img = image::open(input).with_context(|| format!("opening {}", input.display()))?;
    let img = apply_rotation(img, rotate);
    let detections = detect_tags(&img)?;
    if !has_required_posture_tags(&detections) {
        eprintln!("{}", missing_required_tags_message(&detections));
        missing_person.record_missing(no_person_after, port, send_badger_enabled)?;
        return Ok(FrameAnalysisOutcome::MissingRequiredTags);
    }
    let posture = posture_from_detections(&detections)?;
    missing_person.clear();
    window.push(posture);
    let avg = window.average().context("rolling average is empty")?;
    print_posture(input, &avg);
    let debug = out_dir.join("latest-analysis.png");
    write_debug_image(&img, &detections, &avg, &debug)?;
    publish_posture(port, &avg, send_badger_enabled)?;
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

fn send_demo(port: &str, pose: DemoPose) -> Result<()> {
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
    send_to_badger(port, &posture)
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
    let cva_degrees = Some((-(ear.y - c7.y)).atan2(ear.x - c7.x).to_degrees().abs());
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
    canvas
        .save(out)
        .with_context(|| format!("writing {}", out.display()))?;
    Ok(())
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

fn send_to_badger(port_name: &str, posture: &PostureFrame) -> Result<()> {
    let line = badger_payload(posture);
    emit_display_payload(&line);
    send_payload_to_badger(port_name, &line, "OK,P,")
}

fn publish_posture(
    port_name: &str,
    posture: &PostureFrame,
    send_badger_enabled: bool,
) -> Result<()> {
    let line = badger_payload(posture);
    emit_display_payload(&line);
    if send_badger_enabled {
        if let Err(err) = send_payload_to_badger(port_name, &line, "OK,P,") {
            eprintln!("Badger posture send failed: {err:#}");
        }
    }
    Ok(())
}

fn publish_badger_message(port_name: &str, message: &str, send_badger_enabled: bool) -> Result<()> {
    let line = badger_message_payload(message);
    emit_display_payload(&line);
    if send_badger_enabled {
        if let Err(err) = send_payload_to_badger(port_name, &line, "OK,M") {
            eprintln!("Badger message send failed: {err:#}");
        }
    }
    Ok(())
}

fn send_badger_message(port_name: &str, message: &str) -> Result<()> {
    let line = badger_message_payload(message);
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
    Ok(())
}

fn emit_display_payload(line: &str) {
    println!("DISPLAY,{}", line.trim());
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

fn badger_payload(posture: &PostureFrame) -> String {
    let mut parts = vec!["P".to_string(), posture.display_points.len().to_string()];
    for (_, p) in &posture.display_points {
        parts.push((p.x.round() as i32).clamp(0, BADGER_WIDTH - 1).to_string());
        parts.push((p.y.round() as i32).clamp(0, BADGER_HEIGHT - 1).to_string());
    }
    parts.push(format!(
        "cva={}",
        posture
            .cva_degrees
            .map(|v| format!("{v:.1}"))
            .unwrap_or_else(|| "na".to_string())
    ));
    parts.join(",") + "\n"
}

fn badger_message_payload(message: &str) -> String {
    let clean = message.replace([',', '\n', '\r'], " ");
    format!("M,{}\n", clean.trim())
}

fn print_posture(path: &Path, posture: &PostureFrame) {
    println!(
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
    ) -> Result<()> {
        let now = Instant::now();
        let first_missing = *self.first_missing.get_or_insert(now);
        if !self.message_sent && now.duration_since(first_missing) >= threshold {
            publish_badger_message(port, NO_PERSON_MESSAGE, send_badger_enabled)?;
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
}
