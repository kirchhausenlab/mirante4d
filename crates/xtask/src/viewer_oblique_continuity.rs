use std::{
    collections::BTreeMap,
    env,
    ffi::c_void,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Write},
    os::{
        fd::AsRawFd,
        raw::{c_char, c_int, c_long, c_uint, c_ulong},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    ptr, thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail, ensure};
use rustix::time::{ClockId, clock_gettime};

use crate::process;

const TRACE_DIR_ENV: &str = "MIRANTE4D_REAL_INTERACTION_TRACE_DIR";
const INPUT_HZ: u64 = 60;
const INPUT_PERIOD_NS: u64 = 1_000_000_000 / INPUT_HZ;
const READY_TIMEOUT: Duration = Duration::from_secs(120);
const WINDOW_TIMEOUT: Duration = Duration::from_secs(30);
const APP_CLOSE_TIMEOUT: Duration = Duration::from_secs(20);
const CAPTURE_LEAD_IN: Duration = Duration::from_millis(500);
const POST_GESTURE_SETTLE: Duration = Duration::from_secs(1);
const MAX_GAP_NS: u64 = 100_000_000;
const P99_VISIBLE_LATENCY_NS: u64 = 50_000_000;
const VISIBLE_CHANGE_YDIF: f64 = 0.05;
const MIN_VISIBLE_YAVG: f64 = 1.0;
const CONTINUOUS_PROBE_MAX_SIDE: u32 = 192;
const ZOOM_INPUT_HZ: u64 = 60;
const LINKED_ZOOM_INPUT_HZ: u64 = 20;
const LINKED_ZOOM_INPUT_PERIOD_NS: u64 = 1_000_000_000 / LINKED_ZOOM_INPUT_HZ;
const INPUT_RECEIPT_TIMEOUT: Duration = Duration::from_secs(2);
const DRAG_RECEIPT_TIMEOUT: Duration = Duration::from_secs(1);
const UI_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_LINKED_LOD_SELECTION_INPUTS: usize = 16;
const ZOOM_NOTCHES_PER_SAMPLE: usize = 1;
const REQUIRED_ZOOM_CYCLES: usize = 2;
const X11_SHIFT_MASK: c_uint = 1 << 0;
const X11_BUTTON1_MASK: c_uint = 1 << 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Workflow {
    LinkedLodDiagnostic,
    LinkedZoom,
    Zoom,
    Combined,
}

#[derive(Debug, Clone)]
struct Config {
    dataset: PathBuf,
    duration: Duration,
    runs: usize,
    skip_build: bool,
    record_video: bool,
    allow_host_stress: bool,
    workflow: Workflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WindowGeometry {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Copy)]
struct InteractionTarget {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Debug, Clone, Copy)]
struct PresentationTargets {
    three_d: InteractionTarget,
    xy: InteractionTarget,
    xz: InteractionTarget,
    yz: InteractionTarget,
}

#[derive(Debug, Clone, Copy)]
struct InputSample {
    monotonic_ns: u64,
    realtime_ns: u64,
    x: i32,
    y: i32,
}

#[derive(Debug, Clone, Copy)]
struct AppEvent {
    realtime_ns: u64,
    kind: AppEventKind,
    x: f64,
    y: f64,
    value: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppEventKind {
    UiBegin,
    InputMove,
    InputScroll,
    CameraSample,
    InputButtonUp,
    UiStatus,
    LinkedLodStatus,
    CoordinatedExecutionThreeD,
    CoordinatedExecutionXy,
    CoordinatedExecutionXz,
    CoordinatedExecutionYz,
    PresentationTargetChanged,
    UiUpdateDuration,
    RendererCpuTiming,
    GpuTimingThreeD,
    GpuTimingXy,
    GpuTimingXz,
    GpuTimingYz,
    EguiTexturePaintThreeD,
    EguiTexturePaintXy,
    EguiTexturePaintXz,
    EguiTexturePaintYz,
    BoundaryCounter(BoundaryCounterKind),
    DroppedEvents,
    UiEnd,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BoundaryCounterKind {
    DemandPlansSubmitted,
    DemandPlansCompleted,
    DemandPlansCancelled,
    DemandPlanningCompletedNs,
    DatasetRequestsSubmitted,
    DatasetDecodesStarted,
    DatasetDecodesCompleted,
    DatasetRequestsReady,
    DatasetRequestsCancelled,
    DatasetRequestsFailed,
    DatasetQueueWaitNs,
    DatasetDecodeTimeNs,
    DatasetDecodedOutputBytes,
    SourcePhysicalRangeReads,
    SourcePhysicalEncodedBytes,
    SourceCodecDecodes,
    SourceCodecDecodedBytes,
    SourceCodecDecodeTimeNs,
    RendererFramesExecuted,
    RendererQueueSubmissions,
    RendererUploadedResources,
    RendererUploadedPayloadBytes,
    RendererColorSubmissions,
}

impl BoundaryCounterKind {
    const ALL: [Self; 23] = [
        Self::DemandPlansSubmitted,
        Self::DemandPlansCompleted,
        Self::DemandPlansCancelled,
        Self::DemandPlanningCompletedNs,
        Self::DatasetRequestsSubmitted,
        Self::DatasetDecodesStarted,
        Self::DatasetDecodesCompleted,
        Self::DatasetRequestsReady,
        Self::DatasetRequestsCancelled,
        Self::DatasetRequestsFailed,
        Self::DatasetQueueWaitNs,
        Self::DatasetDecodeTimeNs,
        Self::DatasetDecodedOutputBytes,
        Self::SourcePhysicalRangeReads,
        Self::SourcePhysicalEncodedBytes,
        Self::SourceCodecDecodes,
        Self::SourceCodecDecodedBytes,
        Self::SourceCodecDecodeTimeNs,
        Self::RendererFramesExecuted,
        Self::RendererQueueSubmissions,
        Self::RendererUploadedResources,
        Self::RendererUploadedPayloadBytes,
        Self::RendererColorSubmissions,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::DemandPlansSubmitted => "demand_plans_submitted",
            Self::DemandPlansCompleted => "demand_plans_completed",
            Self::DemandPlansCancelled => "demand_plans_cancelled",
            Self::DemandPlanningCompletedNs => "demand_planning_completed_ns",
            Self::DatasetRequestsSubmitted => "dataset_requests_submitted",
            Self::DatasetDecodesStarted => "dataset_decodes_started",
            Self::DatasetDecodesCompleted => "dataset_decodes_completed",
            Self::DatasetRequestsReady => "dataset_requests_ready",
            Self::DatasetRequestsCancelled => "dataset_requests_cancelled",
            Self::DatasetRequestsFailed => "dataset_requests_failed",
            Self::DatasetQueueWaitNs => "dataset_queue_wait_ns",
            Self::DatasetDecodeTimeNs => "dataset_decode_time_ns",
            Self::DatasetDecodedOutputBytes => "dataset_decoded_output_bytes",
            Self::SourcePhysicalRangeReads => "source_physical_range_reads",
            Self::SourcePhysicalEncodedBytes => "source_physical_encoded_bytes",
            Self::SourceCodecDecodes => "source_codec_decodes",
            Self::SourceCodecDecodedBytes => "source_codec_decoded_bytes",
            Self::SourceCodecDecodeTimeNs => "source_codec_decode_time_ns",
            Self::RendererFramesExecuted => "renderer_frames_executed",
            Self::RendererQueueSubmissions => "renderer_queue_submissions",
            Self::RendererUploadedResources => "renderer_uploaded_resources",
            Self::RendererUploadedPayloadBytes => "renderer_uploaded_payload_bytes",
            Self::RendererColorSubmissions => "renderer_color_submissions",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct VisibleSample {
    realtime_ns: u64,
    yavg: f64,
    ydif: f64,
}

#[derive(Debug, Clone, Copy)]
struct ZoomInputSample {
    monotonic_ns: u64,
    realtime_ns: u64,
    direction: i8,
}

#[derive(Debug)]
struct ZoomSessionMetrics {
    input_count: usize,
    input_max_gap_ns: u64,
    input_receipt_count: usize,
    input_receipt_max_gap_ns: u64,
    camera_sample_count: usize,
    combined_orbit_sample_count: usize,
    main_loop_count: usize,
    main_loop_max_gap_ns: u64,
    capture_frame_count: usize,
    capture_max_gap_ns: u64,
    visible_change_count: usize,
    visible_change_max_gap_ns: u64,
    input_to_visible_p99_ns: u64,
    minimum_visible_yavg: f64,
    observed_finer_displayed_boundary: bool,
    observed_adaptive_capacity_boundary: bool,
    final_s3_ready: bool,
    invalid_reasons: Vec<String>,
    failures: Vec<String>,
}

impl ZoomSessionMetrics {
    fn passed(&self) -> bool {
        self.invalid_reasons.is_empty() && self.failures.is_empty()
    }
}

#[derive(Debug)]
struct LinkedZoomCorrectnessMetrics {
    generated_input_count: usize,
    received_input_count: usize,
    linked_publication_count: [usize; 3],
    linked_scale_range: [(f64, f64); 3],
    reached_exact_s0: bool,
    recovered_initial_exact_scales: bool,
    client_surface_artifacts_differ: [bool; 3],
    independent_3d_camera_unchanged: bool,
    invalid_reasons: Vec<String>,
    failures: Vec<String>,
}

#[derive(Debug)]
struct LinkedLodDiagnosticMetrics {
    phases: Vec<LinkedLodPhaseMetrics>,
    returned_to_exact_s3: bool,
    trace_dropped_events: u64,
    invalid_reasons: Vec<String>,
}

impl LinkedLodDiagnosticMetrics {
    fn valid(&self) -> bool {
        self.invalid_reasons.is_empty()
    }
}

#[derive(Debug)]
struct LinkedLodPhaseMetrics {
    scale_level: u8,
    duration_ns: u64,
    generated_input_count: usize,
    generated_input_max_gap_ns: u64,
    received_input_count: usize,
    received_input_max_gap_ns: u64,
    ui_update_count: usize,
    ui_update_duration_p99_ns: u64,
    ui_update_duration_max_ns: u64,
    internal_publication_count: [usize; 3],
    internal_publication_max_gap_ns: [u64; 3],
    internal_published_scale_range: [(f64, f64); 3],
    internal_publication_to_egui_paint_p99_ns: [u64; 3],
    egui_paint_queued_count: [usize; 3],
    renderer_cpu_sample_count: usize,
    renderer_cpu_planning_p99_ns: u64,
    renderer_cpu_planning_max_ns: u64,
    renderer_queue_submit_p99_ns: u64,
    gpu_sample_count: usize,
    gpu_batch_p99_ns: Option<u64>,
    gpu_batch_max_ns: Option<u64>,
    linked_gpu_pass_p99_ns: Option<u64>,
    linked_gpu_pass_max_ns: Option<u64>,
    counter_deltas: BTreeMap<&'static str, u64>,
    settled_before: LinkedLodStatus,
    settled_after: LinkedLodStatus,
}

impl LinkedZoomCorrectnessMetrics {
    fn passed(&self) -> bool {
        self.invalid_reasons.is_empty() && self.failures.is_empty()
    }
}

pub(crate) fn run(args: Vec<String>) -> anyhow::Result<PathBuf> {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "help" | "--help" | "-h"))
    {
        print_help();
        return Ok(workspace_root());
    }
    let config = parse_config(args)?;
    initialize_xlib_threads()?;
    preflight(&config)?;
    if !config.skip_build {
        let mut build = process::cargo_command();
        build.args(["build", "--release", "-p", "mirante4d-app"]);
        process::run_command_with_timeout(&mut build, Duration::from_secs(15 * 60))
            .context("release viewer build failed")?;
    }

    let root = workspace_root();
    let output_root = root
        .join("target/mirante4d/viewer-oblique-continuity")
        .join(format!("run-{}-{}", epoch_ms(), std::process::id()));
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    println!(
        "viewer_oblique_continuity output={} workflow={:?} runs={} duration_s={:.1}",
        output_root.display(),
        config.workflow,
        config.runs,
        config.duration.as_secs_f64()
    );

    let binary = root.join("target/release/mirante4d-app");
    ensure!(
        binary.is_file(),
        "release viewer binary is absent at {}",
        binary.display()
    );

    let mut all_passed = true;
    for session_index in 0..config.runs {
        let session_dir = output_root.join(format!("session-{}", session_index + 1));
        fs::create_dir(&session_dir)
            .with_context(|| format!("failed to create {}", session_dir.display()))?;
        println!(
            "viewer_oblique_continuity session={}/{} state=starting",
            session_index + 1,
            config.runs
        );
        match config.workflow {
            Workflow::LinkedZoom => {
                let metrics = run_linked_zoom_session(&config, &binary, &session_dir)
                    .with_context(|| format!("session {} could not complete", session_index + 1))?;
                write_linked_zoom_correctness_summary(&session_dir.join("summary.txt"), &metrics)?;
                println!(
                    "viewer_linked_zoom_correctness session={}/{} result={} generated_inputs={} received_inputs={} xy/xz/yz_internal_publications={}/{}/{} reached_exact_s0={} recovered_initial_scales={} monitor_visibility=UNOBSERVED",
                    session_index + 1,
                    config.runs,
                    if metrics.passed() { "PASS" } else { "FAIL" },
                    metrics.generated_input_count,
                    metrics.received_input_count,
                    metrics.linked_publication_count[0],
                    metrics.linked_publication_count[1],
                    metrics.linked_publication_count[2],
                    metrics.reached_exact_s0,
                    metrics.recovered_initial_exact_scales,
                );
                report_failures(
                    session_index,
                    config.runs,
                    &metrics.invalid_reasons,
                    &metrics.failures,
                );
                all_passed &= metrics.passed();
            }
            Workflow::LinkedLodDiagnostic => {
                let metrics = run_linked_lod_diagnostic_session(&config, &binary, &session_dir)
                    .with_context(|| format!("session {} could not complete", session_index + 1))?;
                write_linked_lod_diagnostic_summary(&session_dir.join("summary.txt"), &metrics)?;
                println!(
                    "viewer_linked_lod_diagnostic session={}/{} result={} phases={} returned_exact_s3={} monitor_continuity=OWNER_OBSERVATION_REQUIRED",
                    session_index + 1,
                    config.runs,
                    if metrics.valid() { "VALID" } else { "INVALID" },
                    metrics.phases.len(),
                    metrics.returned_to_exact_s3,
                );
                for reason in &metrics.invalid_reasons {
                    println!(
                        "viewer_linked_lod_diagnostic session={}/{} invalid={reason}",
                        session_index + 1,
                        config.runs
                    );
                }
                all_passed &= metrics.valid();
            }
            Workflow::Zoom | Workflow::Combined => {
                let metrics = run_zoom_session(&config, &binary, &session_dir)
                    .with_context(|| format!("session {} could not complete", session_index + 1))?;
                write_zoom_summary(&session_dir.join("summary.txt"), &metrics)?;
                println!(
                    "viewer_zoom_continuity session={}/{} result={} input_gap_ms={:.3} receipt_gap_ms={:.3} camera_samples={} orbit_samples={} main_loop_gap_ms={:.3} visible_gap_ms={:.3} p99_input_to_visible_ms={:.3} finer_displayed={} adaptive_capacity={} final_s3_ready={}",
                    session_index + 1,
                    config.runs,
                    if metrics.passed() { "PASS" } else { "FAIL" },
                    ns_ms(metrics.input_max_gap_ns),
                    ns_ms(metrics.input_receipt_max_gap_ns),
                    metrics.camera_sample_count,
                    metrics.combined_orbit_sample_count,
                    ns_ms(metrics.main_loop_max_gap_ns),
                    ns_ms(metrics.visible_change_max_gap_ns),
                    ns_ms(metrics.input_to_visible_p99_ns),
                    metrics.observed_finer_displayed_boundary,
                    metrics.observed_adaptive_capacity_boundary,
                    metrics.final_s3_ready,
                );
                report_failures(
                    session_index,
                    config.runs,
                    &metrics.invalid_reasons,
                    &metrics.failures,
                );
                all_passed &= metrics.passed();
            }
        }
    }

    if all_passed {
        Ok(output_root)
    } else {
        bail!(
            "real-window viewer workflow was invalid or failed; plain evidence is in {}",
            output_root.display()
        )
    }
}

fn report_failures(
    session_index: usize,
    runs: usize,
    invalid_reasons: &[String],
    failures: &[String],
) {
    for reason in invalid_reasons {
        println!(
            "viewer_oblique_continuity session={}/{} invalid={reason}",
            session_index + 1,
            runs
        );
    }
    for failure in failures {
        println!(
            "viewer_oblique_continuity session={}/{} failure={failure}",
            session_index + 1,
            runs
        );
    }
}

fn parse_config(args: Vec<String>) -> anyhow::Result<Config> {
    let mut dataset = env::var_os("MIRANTE4D_DEV_DATASET").map(PathBuf::from);
    let mut duration_secs = 30.0_f64;
    let mut runs = 1_usize;
    let mut skip_build = false;
    let mut record_video = false;
    let mut allow_host_stress = false;
    let mut workflow = Workflow::LinkedLodDiagnostic;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dataset" => {
                dataset = Some(PathBuf::from(
                    args.next().context("--dataset requires an absolute path")?,
                ));
            }
            "--duration-secs" => {
                duration_secs = args
                    .next()
                    .context("--duration-secs requires a number")?
                    .parse()
                    .context("--duration-secs must be a number")?;
            }
            "--runs" => {
                runs = args
                    .next()
                    .context("--runs requires a count")?
                    .parse()
                    .context("--runs must be an integer")?;
            }
            "--skip-build" => skip_build = true,
            "--video" => record_video = true,
            "--allow-host-stress" => allow_host_stress = true,
            "--workflow" => {
                workflow = match args.next().as_deref() {
                    Some("linked-lod-diagnostic") => Workflow::LinkedLodDiagnostic,
                    Some("linked-zoom") => Workflow::LinkedZoom,
                    Some("zoom") => Workflow::Zoom,
                    Some("combined") => Workflow::Combined,
                    Some(other) => {
                        bail!(
                            "unknown workflow {other:?}; expected linked-lod-diagnostic, linked-zoom, zoom, or combined"
                        )
                    }
                    None => {
                        bail!(
                            "--workflow requires linked-lod-diagnostic, linked-zoom, zoom, or combined"
                        )
                    }
                };
            }
            other => bail!("unknown viewer-oblique-continuity option {other:?}"),
        }
    }
    let dataset =
        dataset.context("provide --dataset /absolute/package.m4d or set MIRANTE4D_DEV_DATASET")?;
    ensure!(
        dataset.is_absolute(),
        "representative dataset path must be absolute"
    );
    ensure!(
        dataset.is_dir(),
        "representative dataset package is unavailable"
    );
    ensure!(
        duration_secs.is_finite() && (5.0..=300.0).contains(&duration_secs),
        "--duration-secs must be between 5 and 300"
    );
    ensure!((1..=3).contains(&runs), "--runs must be between 1 and 3");
    Ok(Config {
        dataset,
        duration: Duration::from_secs_f64(duration_secs),
        runs,
        skip_build,
        record_video,
        allow_host_stress,
        workflow,
    })
}

fn preflight(config: &Config) -> anyhow::Result<()> {
    ensure!(
        !matches!(
            config.workflow,
            Workflow::LinkedZoom | Workflow::LinkedLodDiagnostic
        ) || config.allow_host_stress,
        "linked S0 real-window workflows are quarantined after a whole-desktop freeze; \
         they require explicit --allow-host-stress after the owner has chosen a controlled run"
    );
    ensure!(
        env::var("DISPLAY")
            .ok()
            .is_some_and(|value| !value.is_empty()),
        "viewer-oblique-continuity requires a mapped X11 display"
    );
    ensure!(
        env::var("XDG_SESSION_TYPE")
            .ok()
            .is_none_or(|value| value.eq_ignore_ascii_case("x11")),
        "viewer-oblique-continuity currently requires the real X11 input boundary"
    );
    for tool in ["xdotool", "wmctrl", "ffmpeg"] {
        let status = Command::new(tool)
            .arg("-h")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| format!("{tool} is required for the real-window check"))?;
        if tool != "ffmpeg" {
            ensure!(
                status.success() || status.code().is_some(),
                "{tool} could not be executed"
            );
        }
    }
    ensure!(
        config.dataset.is_dir(),
        "representative dataset disappeared"
    );
    if matches!(
        config.workflow,
        Workflow::LinkedZoom | Workflow::LinkedLodDiagnostic | Workflow::Zoom | Workflow::Combined
    ) {
        OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .context("wheel workflows require writable /dev/uinput")?;
    }
    let pointer = XPointer::open().context("could not open the active X11 display")?;
    drop(pointer);
    Ok(())
}

fn run_linked_zoom_session(
    config: &Config,
    binary: &Path,
    directory: &Path,
) -> anyhow::Result<LinkedZoomCorrectnessMetrics> {
    let stdout = File::create(directory.join("viewer.stdout.log"))?;
    let stderr = File::create(directory.join("viewer.stderr.log"))?;
    let mut app_command = Command::new(binary);
    app_command
        .env("MIRANTE4D_DEV_DATASET", &config.dataset)
        .env(TRACE_DIR_ENV, directory)
        .env(
            "RUST_LOG",
            env::var("RUST_LOG").unwrap_or_else(|_| "warn".to_owned()),
        )
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    process::isolate_process_tree(&mut app_command);
    let child = app_command
        .spawn()
        .context("failed to launch normal viewer")?;
    let mut app = ManagedChild::new(child);
    println!("viewer_linked_zoom_correctness viewer_pid={}", app.id());

    let window = wait_for_window(&mut app)?;
    configure_window(window)?;
    click_four_panel(window, &directory.join("interaction-target.txt"))?;
    wait_for_ready(&mut app, &directory.join("ready"))?;
    let targets = read_presentation_targets(&directory.join("interaction-target.txt"))?;
    let geometry = window_geometry(window)?;
    ensure!(
        geometry.width >= 1100 && geometry.height >= 650,
        "mapped viewer is too small for linked zoom: {}x{}",
        geometry.width,
        geometry.height
    );
    let panel = panel_geometry(geometry, targets)?;
    let linked_status_path = directory.join("linked-lod-status.txt");
    let initial_status =
        wait_for_linked_status(&mut app, &linked_status_path, READY_TIMEOUT, |status| {
            status
                .panels
                .into_iter()
                .all(|panel| panel.exact && panel.display_current && !panel.provisional)
        })?;
    fs::write(
        directory.join("initial-linked-lod-status.txt"),
        format!("{initial_status:?}\n"),
    )?;
    let initial_scales = initial_status.panels.map(|panel| panel.displayed);
    ensure!(
        initial_scales
            .into_iter()
            .all(|scale| scale.is_some_and(|scale| scale > 0)),
        "linked zoom workflow requires a complete coarser-than-S0 starting view"
    );

    let mut wheel = UInputWheel::create()?;
    const LINKED_ZOOM_SAMPLES: usize = 12;
    // Cross-section zoom scales world units per screen point directly, so a
    // negative hardware REL_WHEEL step zooms in for linked 2D. This is the
    // inverse of the perspective-camera wheel convention.
    let cold_samples = generate_linked_zoom_samples(
        window,
        geometry,
        targets.xy,
        &mut wheel,
        -1,
        LINKED_ZOOM_SAMPLES,
    )?;
    write_input_csv(&directory.join("cold-input.csv"), &cold_samples)?;
    let cold_gesture_end_realtime_ns = cold_samples
        .last()
        .expect("cold linked zoom emits samples")
        .realtime_ns;
    let cold_status =
        wait_for_linked_status(&mut app, &linked_status_path, config.duration, |status| {
            status.exact_base()
        })?;
    let cold_settled_realtime_ns = clock_ns(ClockId::Realtime);
    let cold_recovery_samples = generate_linked_zoom_samples(
        window,
        geometry,
        targets.xy,
        &mut wheel,
        1,
        LINKED_ZOOM_SAMPLES,
    )?;
    write_input_csv(
        &directory.join("cold-recovery-input.csv"),
        &cold_recovery_samples,
    )?;
    let cold_recovery_status =
        wait_for_linked_status(&mut app, &linked_status_path, config.duration, |status| {
            status.exact_at(initial_scales)
        })?;
    // The measured cycle starts only after both exact bodies have completed
    // once, so its release-to-settlement threshold is genuinely resident.
    thread::sleep(POST_GESTURE_SETTLE);

    let samples = generate_linked_zoom_samples(
        window,
        geometry,
        targets.xy,
        &mut wheel,
        -1,
        LINKED_ZOOM_SAMPLES,
    )?;
    let gesture_start_realtime_ns = samples
        .first()
        .expect("linked zoom emits samples")
        .realtime_ns;
    let gesture_end_realtime_ns = samples
        .last()
        .expect("linked zoom emits samples")
        .realtime_ns;
    write_input_csv(&directory.join("input.csv"), &samples)?;
    let fine_status =
        wait_for_linked_status(&mut app, &linked_status_path, config.duration, |status| {
            status.exact_base()
        })?;
    let fine_settled_realtime_ns = clock_ns(ClockId::Realtime);
    let mut fine_images = Vec::with_capacity(3);
    for capture in panel.captures {
        let path = directory.join(format!("client-surface-fine-{}.ppm", capture.name));
        capture_panel_ppm(window, geometry, capture, &path)?;
        fine_images.push((capture.name, read_ppm_rgb(&path)?));
    }
    // Keep the reverse gesture outside the bounded post-release window used
    // to measure fine-target settlement. Otherwise a valid coarse recovery
    // would be misclassified as a very late fine refinement.
    thread::sleep(POST_GESTURE_SETTLE);

    let recovery_started_realtime_ns = clock_ns(ClockId::Realtime);
    let recovery_samples = generate_linked_zoom_samples(
        window,
        geometry,
        targets.xy,
        &mut wheel,
        1,
        LINKED_ZOOM_SAMPLES,
    )?;
    let recovery_status =
        wait_for_linked_status(&mut app, &linked_status_path, config.duration, |status| {
            status.exact_at(initial_scales)
        })?;
    let mut recovery_images = Vec::with_capacity(3);
    for capture in panel.captures {
        let path = directory.join(format!("client-surface-recovered-{}.ppm", capture.name));
        capture_panel_ppm(window, geometry, capture, &path)?;
        recovery_images.push((capture.name, read_ppm_rgb(&path)?));
    }

    thread::sleep(POST_GESTURE_SETTLE);
    let final_geometry = window_geometry(window)?;
    let final_active_window = active_window()?;
    app.terminate_gracefully(APP_CLOSE_TIMEOUT)?;
    ensure!(
        final_geometry == geometry,
        "mapped viewer geometry changed during linked zoom"
    );
    ensure!(
        final_active_window == window,
        "the real viewer lost active-window ownership during linked zoom"
    );

    let app_events = parse_app_trace(&directory.join("app-trace.csv"))?;
    let received_input_count = app_events
        .iter()
        .filter(|event| {
            event.kind == AppEventKind::InputScroll
                && (gesture_start_realtime_ns..=gesture_end_realtime_ns.saturating_add(MAX_GAP_NS))
                    .contains(&event.realtime_ns)
        })
        .count();
    let publication_kinds = [
        AppEventKind::CoordinatedExecutionXy,
        AppEventKind::CoordinatedExecutionXz,
        AppEventKind::CoordinatedExecutionYz,
    ];
    let linked_publication_count = publication_kinds.map(|kind| {
        app_events
            .iter()
            .filter(|event| {
                event.kind == kind
                    && event.y >= 1024.0
                    && (gesture_start_realtime_ns
                        ..=fine_settled_realtime_ns.saturating_add(MAX_GAP_NS))
                        .contains(&event.realtime_ns)
            })
            .count()
    });
    let linked_scale_range = publication_kinds.map(|kind| {
        app_events
            .iter()
            .filter(|event| event.kind == kind && event.x.is_finite())
            .map(|event| event.x)
            .fold(
                (f64::INFINITY, f64::NEG_INFINITY),
                |(minimum, maximum), scale| (minimum.min(scale), maximum.max(scale)),
            )
    });
    let mut metrics = LinkedZoomCorrectnessMetrics {
        generated_input_count: samples.len(),
        received_input_count,
        linked_publication_count,
        linked_scale_range,
        reached_exact_s0: fine_status.exact_base(),
        recovered_initial_exact_scales: recovery_status.exact_at(initial_scales),
        client_surface_artifacts_differ: [false; 3],
        independent_3d_camera_unchanged: !app_events
            .iter()
            .any(|event| event.kind == AppEventKind::CameraSample),
        invalid_reasons: Vec::new(),
        failures: Vec::new(),
    };
    if !fine_status.exact_base() {
        metrics
            .failures
            .push("linked zoom did not settle all three panels exactly at S0".to_owned());
    }
    if !recovery_status.exact_at(initial_scales) {
        metrics
            .failures
            .push("linked zoom-out did not recover the initial exact scales".to_owned());
    }
    if app_events
        .iter()
        .any(|event| event.kind == AppEventKind::CameraSample)
    {
        metrics
            .failures
            .push("linked zoom changed the independent 3D camera".to_owned());
    }
    if fine_settled_realtime_ns <= gesture_end_realtime_ns
        || recovery_started_realtime_ns < fine_settled_realtime_ns
        || recovery_samples.len() != LINKED_ZOOM_SAMPLES
    {
        metrics
            .invalid_reasons
            .push("linked zoom phase timestamps or recovery sample count are invalid".to_owned());
    }
    if samples.len() != LINKED_ZOOM_SAMPLES {
        metrics.invalid_reasons.push(format!(
            "measured linked zoom emitted {} inputs; expected {LINKED_ZOOM_SAMPLES}",
            samples.len()
        ));
    }
    if received_input_count < LINKED_ZOOM_SAMPLES.saturating_mul(3).div_ceil(4) {
        metrics.failures.push(format!(
            "the normal app received only {received_input_count} of {} measured wheel inputs",
            samples.len()
        ));
    }
    for (index, name) in ["xy", "xz", "yz"].into_iter().enumerate() {
        if linked_publication_count[index] == 0 {
            metrics.failures.push(format!(
                "{name} produced no internal coordinated publication during S0 settlement"
            ));
        }
        let (minimum, maximum) = linked_scale_range[index];
        if !minimum.is_finite() || !maximum.is_finite() || maximum <= minimum {
            metrics.failures.push(format!(
                "{name} internal publication trace did not span distinct linked geometries"
            ));
        }
    }
    for index in 0..3 {
        let (fine_name, fine) = &fine_images[index];
        let (recovered_name, recovered) = &recovery_images[index];
        if fine_name != recovered_name || fine.is_empty() || recovered.is_empty() {
            metrics
                .invalid_reasons
                .push(format!("linked {fine_name} final artifacts are missing"));
        } else if fine == recovered {
            metrics.failures.push(format!(
                "linked {fine_name} fine and recovered client-surface artifacts are identical"
            ));
        } else {
            metrics.client_surface_artifacts_differ[index] = true;
        }
    }
    let evidence = serde_json::json!({
        "schema": "mirante4d-linked-zoom-endpoint-correctness",
        "schema_version": 2,
        "input_boundary": "mapped_X11_ctrl_wheel_over_reported_XY_presentation",
        "monitor_visibility": "unobserved_owner_confirmation_required",
        "continuity_result": "not_measured",
        "cold_input_samples": cold_samples.len(),
        "cold_recovery_samples": cold_recovery_samples.len(),
        "cold_refinement_settlement_ms": ns_ms(
            cold_settled_realtime_ns.saturating_sub(cold_gesture_end_realtime_ns)
        ),
        "cold_settlement": linked_lod_status_json(cold_status),
        "cold_recovered_settlement": linked_lod_status_json(cold_recovery_status),
        "input_samples": samples.len(),
        "recovery_samples": recovery_samples.len(),
        "initial_settlement": linked_lod_status_json(initial_status),
        "fine_settlement": linked_lod_status_json(fine_status),
        "recovered_settlement": linked_lod_status_json(recovery_status),
        "internal_publication_count": {
            "xy": linked_publication_count[0],
            "xz": linked_publication_count[1],
            "yz": linked_publication_count[2],
        },
        "internal_published_geometry_scale_range": {
            "xy": linked_scale_range[0],
            "xz": linked_scale_range[1],
            "yz": linked_scale_range[2],
        },
        "client_surface_panel_artifacts": [
            "client-surface-fine-xy.ppm",
            "client-surface-fine-xz.ppm",
            "client-surface-fine-yz.ppm"
        ],
        "client_surface_recovery_artifacts": [
            "client-surface-recovered-xy.ppm",
            "client-surface-recovered-xz.ppm",
            "client-surface-recovered-yz.ppm"
        ],
        "client_surface_observation": "XGetImage reads the X11 client/window surface; each fine artifact is nonblank and differs from its recovered coarse artifact, but this is not compositor or monitor evidence",
        "absolute_scale_oracle": "trusted deterministic fixture GPU captures compared against mirante4d-render-reference in incremental_linked_zoom_pixels_match_direct_fine_cpu_oracle_after_one_settlement_plan",
        "three_d_independence": {
            "camera_samples": app_events
                .iter()
                .filter(|event| event.kind == AppEventKind::CameraSample)
                .count(),
            "camera_unchanged": metrics.independent_3d_camera_unchanged,
        },
    });
    fs::write(
        directory.join("linked-zoom-evidence.json"),
        serde_json::to_vec_pretty(&evidence)?,
    )?;
    Ok(metrics)
}

#[derive(Debug)]
struct LinkedLodPhaseRun {
    scale_level: u8,
    samples: Vec<InputSample>,
    started_realtime_ns: u64,
    ended_realtime_ns: u64,
    settled_before: LinkedLodStatus,
    settled_after: LinkedLodStatus,
}

fn run_linked_lod_diagnostic_session(
    config: &Config,
    binary: &Path,
    directory: &Path,
) -> anyhow::Result<LinkedLodDiagnosticMetrics> {
    let stdout = File::create(directory.join("viewer.stdout.log"))?;
    let stderr = File::create(directory.join("viewer.stderr.log"))?;
    let mut app_command = Command::new(binary);
    app_command
        .env("MIRANTE4D_DEV_DATASET", &config.dataset)
        .env(TRACE_DIR_ENV, directory)
        .env(
            "RUST_LOG",
            env::var("RUST_LOG").unwrap_or_else(|_| "warn".to_owned()),
        )
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    process::isolate_process_tree(&mut app_command);
    let child = app_command
        .spawn()
        .context("failed to launch normal viewer")?;
    let mut app = ManagedChild::new(child);
    println!(
        "viewer_linked_lod_diagnostic viewer_pid={} phase_duration_s={:.1}",
        app.id(),
        config.duration.as_secs_f64()
    );

    let window = wait_for_window(&mut app)?;
    configure_window(window)?;
    click_four_panel(window, &directory.join("interaction-target.txt"))?;
    wait_for_ready(&mut app, &directory.join("ready"))?;
    let targets = read_presentation_targets(&directory.join("interaction-target.txt"))?;
    let geometry = window_geometry(window)?;
    ensure!(
        geometry.width >= 1100 && geometry.height >= 650,
        "mapped viewer is too small for linked LOD diagnosis: {}x{}",
        geometry.width,
        geometry.height
    );
    let linked_status_path = directory.join("linked-lod-status.txt");
    let input_receipts_path = directory.join("input-receipts.txt");
    let mut transition_inputs = Vec::new();
    let mut wheel = UInputWheel::create()?;
    drive_linked_to_exact_level(
        &mut app,
        window,
        geometry,
        targets.xy,
        &linked_status_path,
        &input_receipts_path,
        &mut wheel,
        3,
        &mut transition_inputs,
    )?;

    let mut phase_runs = Vec::with_capacity(3);
    for scale_level in [3_u8, 1, 0] {
        if scale_level != 3 {
            drive_linked_to_exact_level(
                &mut app,
                window,
                geometry,
                targets.xy,
                &linked_status_path,
                &input_receipts_path,
                &mut wheel,
                scale_level,
                &mut transition_inputs,
            )?;
        }
        let settled_before_warm =
            wait_for_linked_status(&mut app, &linked_status_path, READY_TIMEOUT, |status| {
                status.exact_level(scale_level)
            })?;
        println!(
            "viewer_linked_lod_diagnostic state=warming scale=s{scale_level} status={settled_before_warm:?}"
        );
        let _warm_samples = run_bounded_linked_shift_drag(
            &mut app,
            window,
            geometry,
            targets.xy,
            Duration::from_secs(2),
            &input_receipts_path,
        )?;
        let settled_before =
            wait_for_linked_status(&mut app, &linked_status_path, READY_TIMEOUT, |status| {
                status.exact_level(scale_level)
            })?;
        thread::sleep(CAPTURE_LEAD_IN);
        println!(
            "viewer_linked_lod_diagnostic state=measuring scale=s{scale_level} duration_s={:.1}",
            config.duration.as_secs_f64()
        );
        let samples = run_bounded_linked_shift_drag(
            &mut app,
            window,
            geometry,
            targets.xy,
            config.duration,
            &input_receipts_path,
        )?;
        let started_realtime_ns = samples
            .first()
            .context("bounded linked drag emitted no samples")?
            .realtime_ns;
        let ended_realtime_ns = samples
            .last()
            .context("bounded linked drag emitted no samples")?
            .realtime_ns;
        write_input_csv(
            &directory.join(format!("phase-s{scale_level}-input.csv")),
            &samples,
        )?;
        let settled_after =
            wait_for_linked_status(&mut app, &linked_status_path, READY_TIMEOUT, |status| {
                status.exact_level(scale_level)
            })?;
        phase_runs.push(LinkedLodPhaseRun {
            scale_level,
            samples,
            started_realtime_ns,
            ended_realtime_ns,
            settled_before,
            settled_after,
        });
        println!("viewer_linked_lod_diagnostic state=phase_complete scale=s{scale_level}");
    }

    drive_linked_to_exact_level(
        &mut app,
        window,
        geometry,
        targets.xy,
        &linked_status_path,
        &input_receipts_path,
        &mut wheel,
        3,
        &mut transition_inputs,
    )?;
    let returned_status =
        wait_for_linked_status(&mut app, &linked_status_path, READY_TIMEOUT, |status| {
            status.exact_level(3)
        })?;
    write_input_csv(
        &directory.join("scale-transition-input.csv"),
        &transition_inputs,
    )?;
    thread::sleep(POST_GESTURE_SETTLE);
    let final_geometry = window_geometry(window)?;
    let final_active_window = active_window()?;
    app.terminate_gracefully(APP_CLOSE_TIMEOUT)?;
    ensure!(
        final_geometry == geometry,
        "mapped viewer geometry changed during linked LOD diagnosis"
    );
    ensure!(
        final_active_window == window,
        "the real viewer lost active-window ownership during linked LOD diagnosis"
    );

    let app_events = parse_app_trace(&directory.join("app-trace.csv"))?;
    let mut metrics = analyze_linked_lod_diagnostic(&phase_runs, &app_events);
    metrics.returned_to_exact_s3 = returned_status.exact_level(3);
    let evidence = serde_json::json!({
        "schema": "mirante4d-linked-lod-continuity-diagnostic",
        "schema_version": 1,
        "workload": {
            "application": "normal_release_viewer",
            "layout": "four_panel",
            "linked_panel": "XY",
            "input": "real_X11_Shift_primary_drag",
            "input_rate_hz": INPUT_HZ,
            "phase_duration_seconds": config.duration.as_secs_f64(),
            "bounded_motion": "periodic +/-20x12 client pixels; returns to its starting pointer position",
            "settled_scales": [3, 1, 0],
            "final_return_scale": 3,
        },
        "observation_boundaries": {
            "generated_input": "independent xtask monotonic clock",
            "received_input": "egui raw input receipt",
            "ui_update": "application update duration while interaction/work is active",
            "demand_and_data": "cumulative planner, runtime, source and renderer counters",
            "renderer_cpu": "coordinated renderer planning/encoding and Queue::submit duration",
            "linked_gpu": "WGPU timestamp query for the coordinated batch and active linked render pass",
            "egui_paint_queued": "native texture image command queued during egui UI construction",
            "window_surface_present": "unobserved",
            "compositor_monitor_visibility": "unobserved_owner_confirmation_required",
        },
        "acceptance": "diagnostic_validity_only_no_performance_threshold",
        "summary": "summary.txt",
        "timeline": "app-trace.csv",
    });
    fs::write(
        directory.join("diagnostic-evidence.json"),
        serde_json::to_vec_pretty(&evidence)?,
    )?;
    Ok(metrics)
}

fn linked_lod_status_json(status: LinkedLodStatus) -> serde_json::Value {
    let panels = ["xy", "xz", "yz"]
        .into_iter()
        .zip(status.panels)
        .map(|(name, panel)| {
            serde_json::json!({
                "panel": name,
                "ideal_scale": panel.ideal,
                "installed_scale": panel.installed,
                "displayed_scale": panel.displayed,
                "exact_current": panel.exact,
                "provisional": panel.provisional,
                "display_current": panel.display_current,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({ "panels": panels })
}

fn capture_panel_ppm(
    window: u64,
    geometry: WindowGeometry,
    capture: PanelCapture,
    path: &Path,
) -> anyhow::Result<()> {
    let probe = window_probe(geometry, capture)?;
    let display = XDisplay::open()?;
    let window = c_ulong::try_from(window).context("X11 window ID overflowed")?;
    let rgb = display.capture_rgb(window, probe)?;
    let mut writer = BufWriter::new(File::create(path).with_context(|| {
        format!(
            "failed to create client-surface {} panel image",
            capture.name
        )
    })?);
    writeln!(writer, "P6")?;
    writeln!(writer, "{} {}", capture.width, capture.height)?;
    writeln!(writer, "255")?;
    writer.write_all(&rgb)?;
    writer.flush()?;
    ensure!(
        path.is_file() && path.metadata()?.len() > 32,
        "client-surface {} panel artifact is missing",
        capture.name
    );
    Ok(())
}

fn read_ppm_rgb(path: &Path) -> anyhow::Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut cursor = 0_usize;
    let mut tokens = Vec::with_capacity(4);
    while tokens.len() < 4 {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let start = cursor;
        while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        ensure!(cursor > start, "PPM header is truncated");
        tokens.push(std::str::from_utf8(&bytes[start..cursor])?);
    }
    ensure!(
        tokens[0] == "P6",
        "client-surface panel artifact is not binary PPM"
    );
    let width = tokens[1].parse::<usize>().context("invalid PPM width")?;
    let height = tokens[2].parse::<usize>().context("invalid PPM height")?;
    ensure!(
        tokens[3] == "255",
        "client-surface panel artifact has unsupported depth"
    );
    ensure!(
        cursor < bytes.len() && bytes[cursor].is_ascii_whitespace(),
        "PPM header has no pixel separator"
    );
    if bytes[cursor] == b'\r' && bytes.get(cursor + 1) == Some(&b'\n') {
        cursor += 2;
    } else {
        cursor += 1;
    }
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
        .context("PPM dimensions overflowed")?;
    ensure!(
        bytes.len().saturating_sub(cursor) == expected,
        "client-surface panel artifact byte length is invalid"
    );
    let rgb = bytes[cursor..].to_vec();
    ensure!(
        rgb.iter().any(|value| *value != 0),
        "client-surface panel artifact is blank"
    );
    Ok(rgb)
}

fn run_zoom_session(
    config: &Config,
    binary: &Path,
    directory: &Path,
) -> anyhow::Result<ZoomSessionMetrics> {
    let stdout = File::create(directory.join("viewer.stdout.log"))?;
    let stderr = File::create(directory.join("viewer.stderr.log"))?;
    let mut app_command = Command::new(binary);
    app_command
        .env("MIRANTE4D_DEV_DATASET", &config.dataset)
        .env(TRACE_DIR_ENV, directory)
        .env(
            "RUST_LOG",
            env::var("RUST_LOG").unwrap_or_else(|_| "warn".to_owned()),
        )
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    process::isolate_process_tree(&mut app_command);
    let child = app_command
        .spawn()
        .context("failed to launch normal viewer")?;
    let mut app = ManagedChild::new(child);
    println!("viewer_zoom_continuity viewer_pid={}", app.id());

    let window = wait_for_window(&mut app)?;
    configure_window(window)?;
    if config.workflow == Workflow::Combined {
        exercise_resize_roundtrip(window)?;
    }
    click_four_panel(window, &directory.join("interaction-target.txt"))?;
    wait_for_ready(&mut app, &directory.join("ready"))?;
    let targets = read_presentation_targets(&directory.join("interaction-target.txt"))?;
    let geometry = window_geometry(window)?;
    ensure!(
        geometry.width >= 1100 && geometry.height >= 650,
        "mapped viewer is too small for the representative four-panel workload: {}x{}",
        geometry.width,
        geometry.height
    );
    validate_targets(geometry, targets)?;
    let target = targets.three_d;
    let pointer_x = target.x + target.width / 2;
    let pointer_y = target.y + target.height / 2;
    activate_window(window)?;
    move_pointer_with_xdotool(window, pointer_x, pointer_y)?;

    let capture_inset = 4;
    let capture = continuous_visible_probe(PanelCapture {
        name: "three-d",
        x_root: geometry.x + target.x + capture_inset,
        y_root: geometry.y + target.y + capture_inset,
        width: even_u32(target.width - capture_inset * 2)?,
        height: even_u32(target.height - capture_inset * 2)?,
    })?;
    let visible_path = directory.join("visible-three-d.txt");
    let ffmpeg_stderr = File::create(directory.join("ffmpeg-three-d.stderr.log"))?;
    let display = env::var("DISPLAY").context("DISPLAY disappeared")?;
    let capture_input = format!("{display}+{},{}", capture.x_root, capture.y_root);
    let filter = format!("signalstats,metadata=print:file={}", visible_path.display());
    let mut ffmpeg_command = Command::new("ffmpeg");
    ffmpeg_command
        .args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "warning",
            "-f",
            "x11grab",
            "-framerate",
            "60",
            "-draw_mouse",
            "0",
            "-video_size",
            &format!("{}x{}", capture.width, capture.height),
            "-use_wallclock_as_timestamps",
            "1",
            "-i",
            &capture_input,
            "-copyts",
            "-vf",
            &filter,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(ffmpeg_stderr));
    if config.record_video {
        let video_path = directory.join("visible-three-d.mkv");
        ffmpeg_command.args([
            "-c:v",
            "ffv1",
            "-level",
            "3",
            video_path
                .to_str()
                .context("capture video path is not UTF-8")?,
        ]);
    } else {
        ffmpeg_command.args(["-f", "null", "-"]);
    }
    process::isolate_process_tree(&mut ffmpeg_command);
    let mut capture_process = ManagedChild::new(
        ffmpeg_command
            .spawn()
            .context("failed to start external 3D capture")?,
    );
    println!(
        "viewer_zoom_continuity state=capture_start region={}x{}+{},{}",
        capture.width, capture.height, capture.x_root, capture.y_root
    );
    thread::sleep(CAPTURE_LEAD_IN);
    ensure!(
        capture_process.try_wait()?.is_none(),
        "3D visible-output capture exited before zoom input"
    );

    let orbit_samples = if config.workflow == Workflow::Combined {
        let samples = generate_3d_orbit_roundtrip(window, geometry, target)?;
        write_input_csv(&directory.join("orbit-input.csv"), &samples)?;
        println!(
            "viewer_zoom_continuity state=orbit_roundtrip emitted={} elapsed_s={:.3}",
            samples.len(),
            samples
                .last()
                .zip(samples.first())
                .map_or(
                    0.0,
                    |(last, first)| last.realtime_ns.saturating_sub(first.realtime_ns) as f64
                        / 1_000_000_000.0
                )
        );
        thread::sleep(Duration::from_millis(250));
        Some(samples)
    } else {
        None
    };

    let samples = generate_zoom_cycles(config.duration, &directory.join("lod-status.txt"))?;
    let gesture_start_realtime_ns = samples
        .first()
        .expect("the checked zoom workflow emits samples")
        .realtime_ns;
    let gesture_end_realtime_ns = samples
        .last()
        .expect("the checked zoom workflow emits samples")
        .realtime_ns;
    write_zoom_input_csv(&directory.join("input.csv"), &samples)?;
    println!(
        "viewer_zoom_continuity state=zoom_complete emitted={} elapsed_s={:.3}",
        samples.len(),
        (gesture_end_realtime_ns.saturating_sub(gesture_start_realtime_ns)) as f64
            / 1_000_000_000.0
    );

    thread::sleep(POST_GESTURE_SETTLE);
    capture_process.finish_capture(Duration::from_secs(10))?;
    let final_geometry = window_geometry(window)?;
    let final_active_window = active_window()?;
    app.terminate_gracefully(APP_CLOSE_TIMEOUT)?;
    ensure!(
        final_geometry == geometry,
        "mapped viewer geometry changed during capture: started at {}x{}+{}+{}, ended at {}x{}+{}+{}",
        geometry.width,
        geometry.height,
        geometry.x,
        geometry.y,
        final_geometry.width,
        final_geometry.height,
        final_geometry.x,
        final_geometry.y
    );
    ensure!(
        final_active_window == window,
        "the real viewer lost active-window ownership during capture: expected {window}, found {final_active_window}"
    );

    let app_events = parse_app_trace(&directory.join("app-trace.csv"))?;
    let visible = parse_visible_samples(&visible_path)?;
    let mut metrics = analyze_zoom(
        config.duration,
        &samples,
        &app_events,
        &visible,
        gesture_start_realtime_ns,
        gesture_end_realtime_ns,
    )?;
    if let Some(orbit_samples) = orbit_samples.as_deref() {
        validate_combined_orbit(orbit_samples, &app_events, &visible, &mut metrics);
    }
    Ok(metrics)
}

#[derive(Debug, Clone, Copy)]
struct PanelGeometry {
    captures: [PanelCapture; 3],
}

#[derive(Debug, Clone, Copy)]
struct PanelCapture {
    name: &'static str,
    x_root: i32,
    y_root: i32,
    width: u32,
    height: u32,
}

fn panel_geometry(
    window: WindowGeometry,
    targets: PresentationTargets,
) -> anyhow::Result<PanelGeometry> {
    validate_targets(window, targets)?;
    // The app reports every exact fitted image rectangle. XY remains the
    // interaction target; these direct-window client-surface artifacts retain
    // all three linked endpoints without claiming compositor visibility.
    let capture_inset = 4;
    let capture = |name, target: InteractionTarget| -> anyhow::Result<PanelCapture> {
        Ok(PanelCapture {
            name,
            x_root: window.x + target.x + capture_inset,
            y_root: window.y + target.y + capture_inset,
            width: even_u32(target.width - capture_inset * 2)?,
            height: even_u32(target.height - capture_inset * 2)?,
        })
    };
    Ok(PanelGeometry {
        captures: [
            capture("xy", targets.xy)?,
            capture("xz", targets.xz)?,
            capture("yz", targets.yz)?,
        ],
    })
}

fn validate_targets(window: WindowGeometry, targets: PresentationTargets) -> anyhow::Result<()> {
    let width = i32::try_from(window.width).context("window width overflowed")?;
    let height = i32::try_from(window.height).context("window height overflowed")?;
    let all_targets = [targets.three_d, targets.xy, targets.xz, targets.yz];
    for target in all_targets {
        ensure!(
            target.x >= 0
                && target.y >= 0
                && target.width >= 160
                && target.height >= 160
                && target.x.saturating_add(target.width) <= width
                && target.y.saturating_add(target.height) <= height,
            "normal viewer reported an invalid presentation rectangle: {target:?} in {width}x{height}"
        );
    }
    Ok(())
}

fn read_presentation_targets(path: &Path) -> anyhow::Result<PresentationTargets> {
    let file = File::open(path)
        .with_context(|| format!("normal viewer did not report {}", path.display()))?;
    let mut values = std::collections::BTreeMap::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        let (key, value) = line
            .split_once('=')
            .context("interaction target row is malformed")?;
        let value = value
            .parse::<i32>()
            .context("interaction target value is invalid")?;
        ensure!(
            [
                "three_d_x",
                "three_d_y",
                "three_d_width",
                "three_d_height",
                "xy_x",
                "xy_y",
                "xy_width",
                "xy_height",
                "xz_x",
                "xz_y",
                "xz_width",
                "xz_height",
                "yz_x",
                "yz_y",
                "yz_width",
                "yz_height",
            ]
            .contains(&key),
            "interaction target contains unknown field {key:?}"
        );
        ensure!(
            values.insert(key.to_owned(), value).is_none(),
            "interaction target repeats field {key:?}"
        );
    }
    let target = |name: &str| -> anyhow::Result<InteractionTarget> {
        let field = |suffix: &str| -> anyhow::Result<i32> {
            values
                .get(&format!("{name}_{suffix}"))
                .copied()
                .with_context(|| format!("interaction target omitted {name}_{suffix}"))
        };
        Ok(InteractionTarget {
            x: field("x")?,
            y: field("y")?,
            width: field("width")?,
            height: field("height")?,
        })
    };
    Ok(PresentationTargets {
        three_d: target("three_d")?,
        xy: target("xy")?,
        xz: target("xz")?,
        yz: target("yz")?,
    })
}

fn even_u32(value: i32) -> anyhow::Result<u32> {
    ensure!(value >= 32, "capture dimension is too small");
    let value = u32::try_from(value)?;
    Ok(value - (value % 2))
}

fn continuous_visible_probe(capture: PanelCapture) -> anyhow::Result<PanelCapture> {
    let width = capture.width.min(CONTINUOUS_PROBE_MAX_SIDE) & !1;
    let height = capture.height.min(CONTINUOUS_PROBE_MAX_SIDE) & !1;
    ensure!(
        width >= 32 && height >= 32,
        "continuous visible probe is too small"
    );
    let x_inset = i32::try_from((capture.width - width) / 2)
        .context("continuous probe x inset overflowed")?;
    let y_inset = i32::try_from((capture.height - height) / 2)
        .context("continuous probe y inset overflowed")?;
    Ok(PanelCapture {
        name: capture.name,
        x_root: capture.x_root.saturating_add(x_inset),
        y_root: capture.y_root.saturating_add(y_inset),
        width,
        height,
    })
}

fn window_probe(geometry: WindowGeometry, capture: PanelCapture) -> anyhow::Result<WindowProbe> {
    let relative_x = capture.x_root.saturating_sub(geometry.x);
    let relative_y = capture.y_root.saturating_sub(geometry.y);
    ensure!(
        relative_x >= 0
            && relative_y >= 0
            && u32::try_from(relative_x)
                .ok()
                .is_some_and(|x| x.saturating_add(capture.width) <= geometry.width)
            && u32::try_from(relative_y)
                .ok()
                .is_some_and(|y| y.saturating_add(capture.height) <= geometry.height),
        "{} capture is outside the mapped viewer",
        capture.name
    );
    Ok(WindowProbe {
        x: relative_x,
        y: relative_y,
        width: capture.width,
        height: capture.height,
    })
}

#[derive(Debug, Clone, Copy)]
struct WindowProbe {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

struct XDisplay {
    raw: *mut c_void,
}

impl XDisplay {
    fn open() -> anyhow::Result<Self> {
        // SAFETY: a null display name selects DISPLAY. The checked handle is
        // owned by this sampler thread until Drop.
        let raw = unsafe { XOpenDisplay(ptr::null()) };
        ensure!(
            !raw.is_null(),
            "XOpenDisplay returned null for panel capture"
        );
        Ok(Self { raw })
    }

    fn capture_rgb(&self, window: c_ulong, probe: WindowProbe) -> anyhow::Result<Vec<u8>> {
        let image = self.capture_image(window, probe)?;
        let pixel_count = usize::try_from(probe.width)?
            .checked_mul(usize::try_from(probe.height)?)
            .context("X11 panel artifact dimensions overflowed")?;
        let mut rgb = Vec::with_capacity(
            pixel_count
                .checked_mul(3)
                .context("X11 panel artifact byte count overflowed")?,
        );
        for y in 0..probe.height {
            for x in 0..probe.width {
                rgb.extend_from_slice(&image.rgb(x, y)?);
            }
        }
        Ok(rgb)
    }

    fn capture_image(&self, window: c_ulong, probe: WindowProbe) -> anyhow::Result<XImage> {
        // SAFETY: the display and exact mapped viewer window are live. Xlib
        // copies the bounded rectangle into a newly owned XImage.
        let raw = unsafe {
            XGetImage(
                self.raw,
                window,
                probe.x,
                probe.y,
                probe.width,
                probe.height,
                c_ulong::MAX,
                X11_Z_PIXMAP,
            )
        };
        ensure!(!raw.is_null(), "XGetImage could not sample the viewer");
        let image = XImage { raw };
        image.validate(probe)?;
        Ok(image)
    }
}

impl Drop for XDisplay {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: this wrapper owns the checked display handle.
            unsafe {
                XCloseDisplay(self.raw);
            }
        }
    }
}

fn masked_channel(pixel: c_ulong, mask: c_ulong) -> u8 {
    let shift = mask.trailing_zeros();
    let maximum = mask >> shift;
    let value = (pixel & mask) >> shift;
    u8::try_from(value.saturating_mul(255) / maximum.max(1)).unwrap_or(u8::MAX)
}

struct XImage {
    raw: *mut c_void,
}

impl XImage {
    fn header(&self) -> &XImageHeader {
        // SAFETY: XGetImage returned an XImage and this wrapper owns it until
        // Drop. XImageHeader declares the public prefix from Xlib/Xutil.h.
        unsafe { &*(self.raw.cast::<XImageHeader>()) }
    }

    fn validate(&self, probe: WindowProbe) -> anyhow::Result<()> {
        let header = self.header();
        ensure!(
            header.width == c_int::try_from(probe.width)?
                && header.height == c_int::try_from(probe.height)?,
            "X11 viewer image dimensions differ from the requested panel"
        );
        ensure!(
            !header.data.is_null()
                && header.bytes_per_line > 0
                && matches!(header.bits_per_pixel, 8 | 16 | 24 | 32)
                && matches!(header.byte_order, 0 | 1),
            "X11 viewer image has an unsupported pixel layout"
        );
        ensure!(
            header.red_mask != 0 && header.green_mask != 0 && header.blue_mask != 0,
            "X11 viewer image has no true-color masks"
        );
        Ok(())
    }

    fn rgb(&self, x: u32, y: u32) -> anyhow::Result<[u8; 3]> {
        let header = self.header();
        ensure!(
            x < u32::try_from(header.width)? && y < u32::try_from(header.height)?,
            "X11 pixel coordinate is outside the captured image"
        );
        let bytes_per_pixel = usize::try_from(header.bits_per_pixel)?.div_ceil(8);
        let offset = usize::try_from(y)?
            .checked_mul(usize::try_from(header.bytes_per_line)?)
            .and_then(|row| row.checked_add(usize::try_from(x).ok()?.checked_mul(bytes_per_pixel)?))
            .context("X11 pixel offset overflowed")?;
        // SAFETY: validation checked the XImage layout; x/y are in bounds;
        // bytes_per_line owns at least width * bytes_per_pixel bytes per row.
        let bytes = unsafe {
            std::slice::from_raw_parts(header.data.cast::<u8>().add(offset), bytes_per_pixel)
        };
        let pixel = if header.byte_order == 0 {
            bytes
                .iter()
                .enumerate()
                .fold(c_ulong::from(0_u8), |value, (shift, byte)| {
                    value | (c_ulong::from(*byte) << (shift * 8))
                })
        } else {
            bytes.iter().fold(c_ulong::from(0_u8), |value, byte| {
                (value << 8) | c_ulong::from(*byte)
            })
        };
        Ok([
            masked_channel(pixel, header.red_mask),
            masked_channel(pixel, header.green_mask),
            masked_channel(pixel, header.blue_mask),
        ])
    }
}

impl Drop for XImage {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: XGetImage returned this owned allocation and this
            // wrapper releases it exactly once.
            unsafe {
                XDestroyImage(self.raw);
            }
        }
    }
}

fn generate_zoom_cycles(
    duration: Duration,
    app_trace: &Path,
) -> anyhow::Result<Vec<ZoomInputSample>> {
    let maximum_samples = u64::try_from(
        duration
            .as_nanos()
            .saturating_mul(u128::from(ZOOM_INPUT_HZ))
            / 1_000_000_000,
    )
    .context("zoom sample count overflowed")?;
    ensure!(
        maximum_samples >= 32,
        "zoom workflow must allow enough samples to cross and recover a real LOD boundary"
    );
    let period_ns = 1_000_000_000 / ZOOM_INPUT_HZ;
    let mut wheel = UInputWheel::create()?;
    let mut samples = Vec::with_capacity(
        usize::try_from(maximum_samples.saturating_mul(2))
            .context("zoom sample capacity overflowed")?,
    );
    let started = Instant::now();
    let mut seeking_finer = true;
    let mut completed_cycles = 0_usize;
    let mut net_direction = 0_i64;
    let mut adaptive_boundary_observed = false;
    for index in 0..maximum_samples {
        sleep_until(started + Duration::from_nanos(index.saturating_mul(period_ns)));
        if let Some(status) = latest_live_lod_status(app_trace)? {
            let reached_turnaround = if adaptive_boundary_observed {
                status.finer_displayed_boundary()
            } else {
                status.adaptive_capacity_boundary() || status.catalog_base_selected()
            };
            adaptive_boundary_observed |= status.adaptive_capacity_boundary();
            if seeking_finer && reached_turnaround {
                seeking_finer = false;
            } else if !seeking_finer && status.complete_current_s3() {
                completed_cycles = completed_cycles.saturating_add(1);
                seeking_finer = true;
            }
        }
        // Positive hardware REL_WHEEL scroll zooms 3D in; negative scroll
        // recovers toward the initial coarser S3 view. Direction is selected
        // from live product LOD facts, not an assumed number of wheel notches,
        // so smoothing cannot silently turn the test into a one-way zoom-out.
        let direction = if seeking_finer { 1 } else { -1 };
        wheel.scroll(direction, ZOOM_NOTCHES_PER_SAMPLE)?;
        net_direction = net_direction.saturating_add(i64::from(direction));
        let (monotonic_ns, realtime_ns) = clock_pair_ns();
        samples.push(ZoomInputSample {
            monotonic_ns,
            realtime_ns,
            direction,
        });
    }
    // Restore the exact generated wheel balance after the independently
    // clocked stress interval. Equal opposite exponential camera deltas
    // return to the starting zoom without guessing a catalog transition or
    // waiting for application-paced status.
    let mut recovery_index = maximum_samples;
    while net_direction != 0 {
        sleep_until(started + Duration::from_nanos(recovery_index.saturating_mul(period_ns)));
        let direction = if net_direction > 0 { -1 } else { 1 };
        wheel.scroll(direction, ZOOM_NOTCHES_PER_SAMPLE)?;
        net_direction = net_direction.saturating_add(i64::from(direction));
        let (monotonic_ns, realtime_ns) = clock_pair_ns();
        samples.push(ZoomInputSample {
            monotonic_ns,
            realtime_ns,
            direction,
        });
        recovery_index = recovery_index.saturating_add(1);
    }
    if completed_cycles < REQUIRED_ZOOM_CYCLES {
        eprintln!(
            "viewer_zoom_continuity live control completed only {completed_cycles} of {REQUIRED_ZOOM_CYCLES} required finer/S3 recovery cycles"
        );
    }
    Ok(samples)
}

fn generate_3d_orbit_roundtrip(
    window: u64,
    geometry: WindowGeometry,
    target: InteractionTarget,
) -> anyhow::Result<Vec<InputSample>> {
    let sample_count = INPUT_HZ * 2;
    let center_x = target.x + target.width / 2;
    let center_y = target.y + target.height / 2;
    let amplitude_x = (target.width / 4).clamp(32, 120);
    let amplitude_y = (target.height / 6).clamp(20, 72);
    activate_window(window)?;
    move_pointer_with_xdotool(window, center_x, center_y)?;
    run_xdotool(&["mousedown", "1"])?;

    let generated = (|| -> anyhow::Result<Vec<InputSample>> {
        let mut pointer = XPointer::open()?;
        let mut samples = Vec::with_capacity(usize::try_from(sample_count)?);
        let started = Instant::now();
        for index in 0..sample_count {
            sleep_until(started + Duration::from_nanos(index.saturating_mul(INPUT_PERIOD_NS)));
            let fraction = index as f64 / (sample_count - 1) as f64;
            let x = center_x
                + (f64::from(amplitude_x) * (std::f64::consts::PI * fraction).sin()).round() as i32;
            let y = center_y
                + (f64::from(amplitude_y) * (std::f64::consts::TAU * fraction).sin()).round()
                    as i32;
            pointer.move_absolute(geometry.x.saturating_add(x), geometry.y.saturating_add(y))?;
            let (monotonic_ns, realtime_ns) = clock_pair_ns();
            samples.push(InputSample {
                monotonic_ns,
                realtime_ns,
                x,
                y,
            });
        }
        Ok(samples)
    })();
    let released = run_xdotool(&["mouseup", "1"]);
    match generated {
        Ok(samples) => {
            released?;
            Ok(samples)
        }
        Err(error) => {
            let _ = released;
            Err(error)
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct LiveLodStatus {
    value: u64,
}

impl LiveLodStatus {
    const fn displayed(self) -> u64 {
        (self.value >> 8) & 0xff
    }

    const fn selected(self) -> u64 {
        (self.value >> 16) & 0xff
    }

    const fn ideal(self) -> u64 {
        (self.value >> 24) & 0xff
    }

    const fn flags(self) -> u64 {
        self.value >> 32
    }

    fn finer_displayed_boundary(self) -> bool {
        self.displayed() < 3
    }

    fn adaptive_capacity_boundary(self) -> bool {
        (self.flags() & (1 << 6)) != 0 && self.ideal() < self.selected().max(self.displayed())
    }

    fn catalog_base_selected(self) -> bool {
        self.selected() == 0 && self.ideal() == 0
    }

    fn complete_current_s3(self) -> bool {
        self.value & 1 == 1 && self.displayed() == 3 && self.selected() == 3 && self.ideal() == 3
    }
}

fn latest_live_lod_status(path: &Path) -> anyhow::Result<Option<LiveLodStatus>> {
    let value = match fs::read_to_string(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("normal app did not write {}", path.display()));
        }
    };
    Ok(value
        .trim()
        .parse()
        .ok()
        .map(|value| LiveLodStatus { value }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LinkedPanelLodStatus {
    ideal: Option<u8>,
    installed: Option<u8>,
    displayed: Option<u8>,
    exact: bool,
    provisional: bool,
    display_current: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LinkedLodStatus {
    panels: [LinkedPanelLodStatus; 3],
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LiveInputReceipts {
    scroll: u64,
    shift_drag: u64,
    ui_turn: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputReceiptKind {
    Scroll,
    ShiftDrag,
}

impl LiveInputReceipts {
    const fn count(self, kind: InputReceiptKind) -> u64 {
        match kind {
            InputReceiptKind::Scroll => self.scroll,
            InputReceiptKind::ShiftDrag => self.shift_drag,
        }
    }
}

impl LinkedLodStatus {
    fn decode(value: u64) -> Self {
        let decode_scale = |value: u64| {
            let value = u8::try_from(value & 0xf).expect("one nibble fits in u8");
            (value != 15).then_some(value)
        };
        let panels = std::array::from_fn(|index| {
            let panel = value >> (index * 16);
            LinkedPanelLodStatus {
                ideal: decode_scale(panel),
                installed: decode_scale(panel >> 4),
                displayed: decode_scale(panel >> 8),
                exact: panel & (1 << 12) != 0,
                provisional: panel & (1 << 13) != 0,
                display_current: panel & (1 << 14) != 0,
            }
        });
        Self { panels }
    }

    fn exact_at(self, scales: [Option<u8>; 3]) -> bool {
        self.panels
            .into_iter()
            .zip(scales)
            .all(|(panel, expected)| {
                panel.exact
                    && !panel.provisional
                    && panel.display_current
                    && panel.installed == expected
                    && panel.displayed == expected
            })
    }

    fn exact_base(self) -> bool {
        self.panels.into_iter().all(|panel| {
            panel.ideal == Some(0)
                && panel.installed == Some(0)
                && panel.displayed == Some(0)
                && panel.exact
                && !panel.provisional
                && panel.display_current
        })
    }

    fn exact_level(self, level: u8) -> bool {
        self.panels.into_iter().all(|panel| {
            panel.ideal == Some(level)
                && panel.installed == Some(level)
                && panel.displayed == Some(level)
                && panel.exact
                && !panel.provisional
                && panel.display_current
        })
    }

    fn shared_ideal(self) -> Option<u8> {
        let first = self.panels[0].ideal?;
        self.panels
            .into_iter()
            .all(|panel| panel.ideal == Some(first))
            .then_some(first)
    }
}

fn latest_live_input_receipts(path: &Path) -> anyhow::Result<LiveInputReceipts> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LiveInputReceipts::default());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("normal app could not expose {}", path.display()));
        }
    };
    parse_live_input_receipts(&contents)
}

fn parse_live_input_receipts(contents: &str) -> anyhow::Result<LiveInputReceipts> {
    let mut receipts = LiveInputReceipts::default();
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("scroll=") {
            receipts.scroll = value.parse().context("invalid live scroll receipt count")?;
        } else if let Some(value) = line.strip_prefix("shift_drag=") {
            receipts.shift_drag = value
                .parse()
                .context("invalid live Shift-drag receipt count")?;
        } else if let Some(value) = line.strip_prefix("ui_turn=") {
            receipts.ui_turn = value
                .parse()
                .context("invalid live UI-turn receipt count")?;
        }
    }
    Ok(receipts)
}

fn wait_for_input_receipt(
    app: &mut ManagedChild,
    path: &Path,
    kind: InputReceiptKind,
    previous: u64,
    timeout: Duration,
) -> anyhow::Result<u64> {
    let started = Instant::now();
    loop {
        ensure!(
            app.try_wait()?.is_none(),
            "normal viewer exited while awaiting {kind:?} input receipt"
        );
        let current = latest_live_input_receipts(path)?.count(kind);
        if current > previous {
            return Ok(current);
        }
        if started.elapsed() >= timeout {
            bail!(
                "normal viewer did not acknowledge {kind:?} input within {timeout:?}; previous={previous}, current={current}"
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn latest_live_linked_lod_status(path: &Path) -> anyhow::Result<Option<LinkedLodStatus>> {
    let value = match fs::read_to_string(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("normal app did not write {}", path.display()));
        }
    };
    Ok(value
        .trim()
        .parse::<u64>()
        .ok()
        .map(LinkedLodStatus::decode))
}

fn wait_for_linked_status(
    app: &mut ManagedChild,
    path: &Path,
    timeout: Duration,
    predicate: impl Fn(LinkedLodStatus) -> bool,
) -> anyhow::Result<LinkedLodStatus> {
    let started = Instant::now();
    let mut last = None;
    let receipt_path = path.with_file_name("input-receipts.txt");
    let mut heartbeat = latest_live_input_receipts(&receipt_path)?.ui_turn;
    let mut heartbeat_changed_at = Instant::now();
    loop {
        ensure!(
            app.try_wait()?.is_none(),
            "normal viewer exited before linked LOD settlement"
        );
        if let Some(status) = latest_live_linked_lod_status(path)? {
            last = Some(status);
            if predicate(status) {
                return Ok(status);
            }
        }
        let current_heartbeat = latest_live_input_receipts(&receipt_path)?.ui_turn;
        if current_heartbeat > heartbeat {
            heartbeat = current_heartbeat;
            heartbeat_changed_at = Instant::now();
        }
        ensure!(
            heartbeat_changed_at.elapsed() < UI_HEARTBEAT_TIMEOUT,
            "normal viewer UI heartbeat stopped while awaiting linked LOD settlement; \
             last status={last:?}"
        );
        if started.elapsed() >= timeout {
            bail!(
                "linked LOD status did not reach its required finite state within {timeout:?}; last={last:?}"
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn generate_linked_zoom_samples(
    window: u64,
    geometry: WindowGeometry,
    target: InteractionTarget,
    wheel: &mut UInputWheel,
    direction: i8,
    count: usize,
) -> anyhow::Result<Vec<InputSample>> {
    ensure!(
        matches!(direction, -1 | 1),
        "linked zoom direction is invalid"
    );
    ensure!(
        (1..=128).contains(&count),
        "linked zoom sample count is invalid"
    );
    activate_window(window)?;
    let x = target.x + target.width / 2;
    let y = target.y + target.height / 2;
    move_pointer_with_xdotool(window, x, y)?;
    run_xdotool(&["keydown", "ctrl"])?;
    // Activation, pointer relocation, and modifier delivery are setup, not
    // wheel samples. Let the real window consume them before starting the
    // independent interaction clock, as a user naturally does when moving
    // to a panel and holding Ctrl before scrolling.
    thread::sleep(Duration::from_millis(150));
    let generated = (|| -> anyhow::Result<Vec<InputSample>> {
        let started = Instant::now();
        let mut samples = Vec::with_capacity(count);
        for index in 0..count {
            let index = u64::try_from(index).unwrap_or(u64::MAX);
            sleep_until(
                started + Duration::from_nanos(index.saturating_mul(LINKED_ZOOM_INPUT_PERIOD_NS)),
            );
            wheel.scroll(direction, 1)?;
            let (monotonic_ns, realtime_ns) = clock_pair_ns();
            samples.push(InputSample {
                monotonic_ns,
                realtime_ns,
                x,
                y,
            });
        }
        Ok(samples)
    })();
    let released = run_xdotool(&["keyup", "ctrl"]);
    match generated {
        Ok(samples) => {
            released?;
            ensure!(
                window_geometry(window)? == geometry,
                "linked zoom changed the mapped window geometry"
            );
            Ok(samples)
        }
        Err(error) => {
            let _ = released;
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn drive_linked_to_exact_level(
    app: &mut ManagedChild,
    window: u64,
    geometry: WindowGeometry,
    target: InteractionTarget,
    status_path: &Path,
    input_receipts_path: &Path,
    wheel: &mut UInputWheel,
    target_level: u8,
    transition_inputs: &mut Vec<InputSample>,
) -> anyhow::Result<LinkedLodStatus> {
    ensure!(target_level <= 3, "diagnostic target LOD is outside S0..S3");
    let started = Instant::now();
    let mut last = None;
    for _attempt in 0..MAX_LINKED_LOD_SELECTION_INPUTS {
        ensure!(
            app.try_wait()?.is_none(),
            "normal viewer exited while selecting linked S{target_level}"
        );
        if let Some(status) = latest_live_linked_lod_status(status_path)? {
            last = Some(status);
            if status.exact_level(target_level) {
                return Ok(status);
            }
            if status.shared_ideal() == Some(target_level) {
                return wait_for_linked_status(app, status_path, READY_TIMEOUT, |status| {
                    status.exact_level(target_level)
                });
            }
        }
        ensure!(
            started.elapsed() < READY_TIMEOUT,
            "linked LOD selection did not reach exact S{target_level}; last={last:?}"
        );
        let current_level = last
            .and_then(LinkedLodStatus::shared_ideal)
            .or_else(|| last.and_then(|status| status.panels[0].displayed))
            .unwrap_or(3);
        let direction = if current_level > target_level { -1 } else { 1 };
        let receipt_before = latest_live_input_receipts(input_receipts_path)?.scroll;
        transition_inputs.extend(generate_linked_zoom_samples(
            window, geometry, target, wheel, direction, 1,
        )?);
        wait_for_input_receipt(
            app,
            input_receipts_path,
            InputReceiptKind::Scroll,
            receipt_before,
            INPUT_RECEIPT_TIMEOUT,
        )
        .with_context(|| {
            format!("aborted linked S{target_level} selection after one unacknowledged wheel input")
        })?;
        let observation_started = Instant::now();
        while observation_started.elapsed() < Duration::from_millis(350) {
            if let Some(status) = latest_live_linked_lod_status(status_path)? {
                last = Some(status);
                if status.exact_level(target_level) {
                    return Ok(status);
                }
                if status.shared_ideal() == Some(target_level) {
                    return wait_for_linked_status(app, status_path, READY_TIMEOUT, |status| {
                        status.exact_level(target_level)
                    });
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    bail!(
        "linked LOD selection exhausted its bounded input count for S{target_level}; last={last:?}"
    )
}

fn run_bounded_linked_shift_drag(
    app: &mut ManagedChild,
    window: u64,
    geometry: WindowGeometry,
    target: InteractionTarget,
    duration: Duration,
    input_receipts_path: &Path,
) -> anyhow::Result<Vec<InputSample>> {
    let sample_count =
        usize::try_from(duration.as_nanos().saturating_mul(u128::from(INPUT_HZ)) / 1_000_000_000)
            .context("bounded gesture sample count overflowed")?;
    ensure!(sample_count >= 2, "bounded gesture has too few samples");
    let center_x = target.x + target.width / 2;
    let center_y = target.y + target.height / 2;
    let amplitude_x = (target.width / 18).clamp(12, 20);
    let amplitude_y = (target.height / 24).clamp(8, 12);
    activate_window(window)?;
    move_pointer_with_xdotool(window, center_x, center_y)?;
    set_input_down(true)?;
    let mut release = InputRelease::new();
    let mut acknowledged_receipts = latest_live_input_receipts(input_receipts_path)?.shift_drag;
    let generated = (|| -> anyhow::Result<Vec<InputSample>> {
        let mut pointer = XPointer::open()?;
        let mut samples = Vec::with_capacity(sample_count);
        let started = Instant::now();
        let cycles = duration.as_secs_f64().floor().max(1.0);
        for index in 0..sample_count {
            let index_u64 = u64::try_from(index).unwrap_or(u64::MAX);
            sleep_until(started + Duration::from_nanos(index_u64.saturating_mul(INPUT_PERIOD_NS)));
            let fraction = index as f64 / (sample_count - 1) as f64;
            let phase = std::f64::consts::TAU * cycles * fraction;
            let (x, y) =
                bounded_pointer_position(center_x, center_y, amplitude_x, amplitude_y, phase);
            pointer.move_absolute(geometry.x.saturating_add(x), geometry.y.saturating_add(y))?;
            let (monotonic_ns, realtime_ns) = clock_pair_ns();
            samples.push(InputSample {
                monotonic_ns,
                realtime_ns,
                x,
                y,
            });
            if index_u64 > 0 && index_u64.is_multiple_of(INPUT_HZ / 2) {
                acknowledged_receipts = wait_for_input_receipt(
                    app,
                    input_receipts_path,
                    InputReceiptKind::ShiftDrag,
                    acknowledged_receipts,
                    DRAG_RECEIPT_TIMEOUT,
                )?;
            }
        }
        Ok(samples)
    })();
    set_input_down(false)?;
    release.disarm();
    generated
}

fn bounded_pointer_position(
    center_x: i32,
    center_y: i32,
    amplitude_x: i32,
    amplitude_y: i32,
    phase: f64,
) -> (i32, i32) {
    (
        center_x + (f64::from(amplitude_x) * phase.sin()).round() as i32,
        center_y + (f64::from(amplitude_y) * (phase * 2.0).sin()).round() as i32,
    )
}

fn sleep_until(deadline: Instant) {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        thread::sleep(deadline.duration_since(now));
    }
}

fn analyze_linked_lod_diagnostic(
    phases: &[LinkedLodPhaseRun],
    app: &[AppEvent],
) -> LinkedLodDiagnosticMetrics {
    let mut invalid_reasons = Vec::new();
    let trace_dropped_events = app
        .iter()
        .filter(|event| event.kind == AppEventKind::DroppedEvents)
        .map(|event| event.value)
        .sum();
    if trace_dropped_events > 0 {
        invalid_reasons.push(format!(
            "the bounded app trace dropped {trace_dropped_events} events"
        ));
    }
    let publication_kinds = [
        AppEventKind::CoordinatedExecutionXy,
        AppEventKind::CoordinatedExecutionXz,
        AppEventKind::CoordinatedExecutionYz,
    ];
    let paint_kinds = [
        AppEventKind::EguiTexturePaintXy,
        AppEventKind::EguiTexturePaintXz,
        AppEventKind::EguiTexturePaintYz,
    ];
    let gpu_kinds = [
        AppEventKind::GpuTimingXy,
        AppEventKind::GpuTimingXz,
        AppEventKind::GpuTimingYz,
    ];
    let mut phase_metrics = Vec::with_capacity(phases.len());
    for phase in phases {
        let start = phase.started_realtime_ns;
        let end = phase.ended_realtime_ns;
        let in_phase = |event: &&AppEvent| (start..=end).contains(&event.realtime_ns);
        let generated_times = phase
            .samples
            .iter()
            .map(|sample| sample.monotonic_ns)
            .collect::<Vec<_>>();
        let received_times = app
            .iter()
            .filter(|event| {
                event.kind == AppEventKind::InputMove
                    && event.value & 0b11 == 0b11
                    && (start..=end).contains(&event.realtime_ns)
            })
            .map(|event| event.realtime_ns)
            .collect::<Vec<_>>();
        let ui_update_durations = app
            .iter()
            .filter(|event| event.kind == AppEventKind::UiUpdateDuration)
            .filter(in_phase)
            .map(|event| event.value)
            .collect::<Vec<_>>();
        let internal_publication_times = publication_kinds.map(|kind| {
            app.iter()
                .filter(|event| {
                    event.kind == kind
                        && event.y >= 1024.0
                        && (start..=end).contains(&event.realtime_ns)
                })
                .map(|event| event.realtime_ns)
                .collect::<Vec<_>>()
        });
        let internal_published_scale_range = publication_kinds.map(|kind| {
            app.iter()
                .filter(|event| {
                    event.kind == kind
                        && event.y >= 1024.0
                        && event.x.is_finite()
                        && (start..=end).contains(&event.realtime_ns)
                })
                .map(|event| event.x)
                .fold(
                    (f64::INFINITY, f64::NEG_INFINITY),
                    |(minimum, maximum), scale| (minimum.min(scale), maximum.max(scale)),
                )
        });
        let paint_times = paint_kinds.map(|kind| {
            app.iter()
                .filter(|event| event.kind == kind && (start..=end).contains(&event.realtime_ns))
                .map(|event| event.realtime_ns)
                .collect::<Vec<_>>()
        });
        let renderer_cpu = app
            .iter()
            .filter(|event| event.kind == AppEventKind::RendererCpuTiming)
            .filter(in_phase)
            .collect::<Vec<_>>();
        let renderer_cpu_planning = renderer_cpu
            .iter()
            .map(|event| event.x.max(0.0) as u64)
            .collect::<Vec<_>>();
        let renderer_queue_submit = renderer_cpu
            .iter()
            .map(|event| event.y.max(0.0) as u64)
            .collect::<Vec<_>>();
        let gpu = app
            .iter()
            .filter(|event| gpu_kinds.contains(&event.kind))
            .filter(in_phase)
            .collect::<Vec<_>>();
        let valid_gpu_value =
            |value: f64| value.is_finite() && value >= 0.0 && value < (u64::MAX / 2) as f64;
        let gpu_batch = gpu
            .iter()
            .filter_map(|event| valid_gpu_value(event.x).then_some(event.x as u64))
            .collect::<Vec<_>>();
        let linked_gpu_pass = gpu
            .iter()
            .filter_map(|event| valid_gpu_value(event.y).then_some(event.y as u64))
            .collect::<Vec<_>>();
        let counter_deltas = BoundaryCounterKind::ALL
            .into_iter()
            .map(|counter| {
                let before = boundary_counter_value_at(app, counter, start);
                let after = boundary_counter_value_at(app, counter, end);
                (counter.label(), after.saturating_sub(before))
            })
            .collect::<BTreeMap<_, _>>();

        let distinct_positions = phase
            .samples
            .iter()
            .map(|sample| (sample.x, sample.y))
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        if phase.samples.len() < 2 || distinct_positions < 8 || end <= start {
            invalid_reasons.push(format!(
                "S{} generated input did not contain a real bounded moving gesture",
                phase.scale_level
            ));
        }
        if received_times.is_empty() {
            invalid_reasons.push(format!(
                "S{} produced no Shift-drag receipt at the egui boundary",
                phase.scale_level
            ));
        }
        if !phase.settled_before.exact_level(phase.scale_level)
            || !phase.settled_after.exact_level(phase.scale_level)
        {
            invalid_reasons.push(format!(
                "S{} was not exact before and after its measured phase",
                phase.scale_level
            ));
        }
        for (index, panel) in ["XY", "XZ", "YZ"].into_iter().enumerate() {
            if internal_publication_times[index].is_empty() {
                invalid_reasons.push(format!(
                    "S{} {panel} produced no internal target publication",
                    phase.scale_level
                ));
            }
            let (minimum, maximum) = internal_published_scale_range[index];
            if !minimum.is_finite() || !maximum.is_finite() || maximum <= minimum {
                invalid_reasons.push(format!(
                    "S{} {panel} publication trace did not span moving linked geometry",
                    phase.scale_level
                ));
            }
            if paint_times[index].is_empty() {
                invalid_reasons.push(format!(
                    "S{} {panel} queued no egui texture paint",
                    phase.scale_level
                ));
            }
        }

        phase_metrics.push(LinkedLodPhaseMetrics {
            scale_level: phase.scale_level,
            duration_ns: end.saturating_sub(start),
            generated_input_count: phase.samples.len(),
            generated_input_max_gap_ns: adjacent_max_gap(&generated_times),
            received_input_count: received_times.len(),
            received_input_max_gap_ns: bounded_max_gap(&received_times, start, end),
            ui_update_count: ui_update_durations.len(),
            ui_update_duration_p99_ns: percentile_ns(&ui_update_durations, 99),
            ui_update_duration_max_ns: ui_update_durations.iter().copied().max().unwrap_or(0),
            internal_publication_count: internal_publication_times.each_ref().map(Vec::len),
            internal_publication_max_gap_ns: internal_publication_times
                .each_ref()
                .map(|times| bounded_max_gap(times, start, end)),
            internal_published_scale_range,
            internal_publication_to_egui_paint_p99_ns: std::array::from_fn(|index| {
                next_event_latency_p99(&internal_publication_times[index], &paint_times[index])
            }),
            egui_paint_queued_count: paint_times.each_ref().map(Vec::len),
            renderer_cpu_sample_count: renderer_cpu.len(),
            renderer_cpu_planning_p99_ns: percentile_ns(&renderer_cpu_planning, 99),
            renderer_cpu_planning_max_ns: renderer_cpu_planning.iter().copied().max().unwrap_or(0),
            renderer_queue_submit_p99_ns: percentile_ns(&renderer_queue_submit, 99),
            gpu_sample_count: gpu.len(),
            gpu_batch_p99_ns: (!gpu_batch.is_empty()).then(|| percentile_ns(&gpu_batch, 99)),
            gpu_batch_max_ns: gpu_batch.iter().copied().max(),
            linked_gpu_pass_p99_ns: (!linked_gpu_pass.is_empty())
                .then(|| percentile_ns(&linked_gpu_pass, 99)),
            linked_gpu_pass_max_ns: linked_gpu_pass.iter().copied().max(),
            counter_deltas,
            settled_before: phase.settled_before,
            settled_after: phase.settled_after,
        });
    }
    LinkedLodDiagnosticMetrics {
        phases: phase_metrics,
        returned_to_exact_s3: false,
        trace_dropped_events,
        invalid_reasons,
    }
}

fn boundary_counter_value_at(
    app: &[AppEvent],
    counter: BoundaryCounterKind,
    realtime_ns: u64,
) -> u64 {
    app.iter()
        .filter(|event| {
            event.kind == AppEventKind::BoundaryCounter(counter) && event.realtime_ns <= realtime_ns
        })
        .max_by_key(|event| event.realtime_ns)
        .map_or(0, |event| event.value)
}

fn percentile_ns(values: &[u64], percentile: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut values = values.to_vec();
    values.sort_unstable();
    let rank = values
        .len()
        .saturating_mul(percentile)
        .div_ceil(100)
        .saturating_sub(1)
        .min(values.len() - 1);
    values[rank]
}

fn next_event_latency_p99(origins: &[u64], destinations: &[u64]) -> u64 {
    let mut latencies = Vec::with_capacity(origins.len());
    let mut destination_index = 0;
    for origin in origins {
        while destinations
            .get(destination_index)
            .is_some_and(|destination| destination < origin)
        {
            destination_index += 1;
        }
        if let Some(destination) = destinations.get(destination_index) {
            latencies.push(destination.saturating_sub(*origin));
        }
    }
    percentile_ns(&latencies, 99)
}

fn analyze_zoom(
    _duration: Duration,
    inputs: &[ZoomInputSample],
    app: &[AppEvent],
    visible: &[VisibleSample],
    gesture_start_ns: u64,
    gesture_end_ns: u64,
) -> anyhow::Result<ZoomSessionMetrics> {
    ensure!(!inputs.is_empty(), "zoom input producer emitted no samples");
    ensure!(!app.is_empty(), "app trace contains no events");
    ensure!(!visible.is_empty(), "3D visible capture contains no frames");
    let input_monotonic = inputs
        .iter()
        .map(|sample| sample.monotonic_ns)
        .collect::<Vec<_>>();
    let input_max_gap_ns = adjacent_max_gap(&input_monotonic);
    let receipt_times = app
        .iter()
        .filter(|event| {
            event.kind == AppEventKind::InputScroll
                && (gesture_start_ns..=gesture_end_ns.saturating_add(MAX_GAP_NS))
                    .contains(&event.realtime_ns)
        })
        .map(|event| event.realtime_ns)
        .collect::<Vec<_>>();
    let camera_sample_times = app
        .iter()
        .filter(|event| {
            event.kind == AppEventKind::CameraSample
                && (gesture_start_ns..=gesture_end_ns.saturating_add(MAX_GAP_NS))
                    .contains(&event.realtime_ns)
        })
        .map(|event| event.realtime_ns)
        .collect::<Vec<_>>();
    let main_loop_times = app
        .iter()
        .filter(|event| {
            event.kind == AppEventKind::UiBegin
                && (gesture_start_ns..=gesture_end_ns).contains(&event.realtime_ns)
        })
        .map(|event| event.realtime_ns)
        .collect::<Vec<_>>();
    let capture_times = visible
        .iter()
        .filter(|sample| (gesture_start_ns..=gesture_end_ns).contains(&sample.realtime_ns))
        .map(|sample| sample.realtime_ns)
        .collect::<Vec<_>>();
    let change_times = visible
        .iter()
        .filter(|sample| {
            (gesture_start_ns..=gesture_end_ns).contains(&sample.realtime_ns)
                && sample.ydif > VISIBLE_CHANGE_YDIF
        })
        .map(|sample| sample.realtime_ns)
        .collect::<Vec<_>>();
    let latency_change_times = visible
        .iter()
        .filter(|sample| {
            (gesture_start_ns..=gesture_end_ns.saturating_add(MAX_GAP_NS))
                .contains(&sample.realtime_ns)
                && sample.ydif > VISIBLE_CHANGE_YDIF
        })
        .map(|sample| sample.realtime_ns)
        .collect::<Vec<_>>();
    let input_receipt_max_gap_ns =
        bounded_max_gap(&receipt_times, gesture_start_ns, gesture_end_ns);
    let main_loop_max_gap_ns = bounded_max_gap(&main_loop_times, gesture_start_ns, gesture_end_ns);
    let capture_max_gap_ns = bounded_max_gap(&capture_times, gesture_start_ns, gesture_end_ns);
    let visible_change_max_gap_ns =
        bounded_max_gap(&change_times, gesture_start_ns, gesture_end_ns);
    let input_to_visible_p99_ns = zoom_input_to_next_change_p99(
        inputs,
        &latency_change_times,
        gesture_end_ns.saturating_add(MAX_GAP_NS),
    );
    let minimum_visible_yavg = visible
        .iter()
        .filter(|sample| (gesture_start_ns..=gesture_end_ns).contains(&sample.realtime_ns))
        .map(|sample| sample.yavg)
        .fold(f64::INFINITY, f64::min);

    let status_values = app
        .iter()
        .filter(|event| {
            matches!(event.kind, AppEventKind::UiStatus | AppEventKind::UiEnd)
                && event.realtime_ns >= gesture_start_ns.saturating_sub(MAX_GAP_NS)
        })
        .map(|event| event.value)
        .collect::<Vec<_>>();
    let observed_finer_displayed_boundary = status_values
        .iter()
        .copied()
        .any(|value| ((value >> 8) & 0xff) < 3);
    let observed_adaptive_capacity_boundary = status_values.iter().copied().any(|value| {
        let displayed = (value >> 8) & 0xff;
        let selected = (value >> 16) & 0xff;
        let ideal = (value >> 24) & 0xff;
        let flags = value >> 32;
        (flags & (1 << 6)) != 0 && ideal < selected.max(displayed)
    });
    let hard_capacity_reported = status_values
        .iter()
        .copied()
        .any(|value| ((value >> 32) & (1 << 7)) != 0);
    let final_s3_ready = app
        .iter()
        .rev()
        .find(|event| event.kind == AppEventKind::UiEnd)
        .is_some_and(|event| event.value & 1 == 1 && ((event.value >> 8) & 0xff) == 3);

    let gesture_seconds = gesture_end_ns.saturating_sub(gesture_start_ns) as f64 / 1_000_000_000.0;
    let minimum_receipts = (inputs.len() * 4).div_ceil(5);
    let minimum_main_loops = (gesture_seconds * 30.0).floor() as usize;
    let minimum_capture_frames = (gesture_seconds * 30.0).floor() as usize;
    let minimum_changes = (gesture_seconds * 10.0).floor() as usize;
    let mut invalid_reasons = Vec::new();
    if input_max_gap_ns > MAX_GAP_NS {
        invalid_reasons.push(format!(
            "independent wheel producer stalled for {:.3} ms",
            ns_ms(input_max_gap_ns)
        ));
    }
    let direction_runs = inputs
        .windows(2)
        .filter(|pair| pair[0].direction != pair[1].direction)
        .count()
        .saturating_add(1);
    if direction_runs < REQUIRED_ZOOM_CYCLES * 2
        || !inputs.iter().any(|sample| sample.direction == 1)
        || !inputs.iter().any(|sample| sample.direction == -1)
    {
        invalid_reasons.push(
            "zoom producer did not complete the required live-guided in/out direction changes"
                .to_owned(),
        );
    }
    if app
        .iter()
        .any(|event| event.kind == AppEventKind::PresentationTargetChanged)
    {
        invalid_reasons
            .push("the 3D presentation rectangle changed after capture alignment".to_owned());
    }
    if let Some(dropped) = app
        .iter()
        .find(|event| event.kind == AppEventKind::DroppedEvents)
    {
        invalid_reasons.push(format!(
            "the bounded app trace dropped {} events",
            dropped.value
        ));
    }
    if capture_times.len() < minimum_capture_frames {
        invalid_reasons.push(format!(
            "3D capture retained only {} in-workflow frames; expected at least {minimum_capture_frames}",
            capture_times.len()
        ));
    }
    if capture_max_gap_ns > MAX_GAP_NS {
        invalid_reasons.push(format!(
            "external 3D capture stalled for {:.3} ms",
            ns_ms(capture_max_gap_ns)
        ));
    }

    let mut failures = Vec::new();
    if receipt_times.len() < minimum_receipts {
        failures.push(format!(
            "the real window/UI boundary received only {} of {} wheel inputs",
            receipt_times.len(),
            inputs.len()
        ));
    }
    if camera_sample_times.len() < minimum_receipts {
        failures.push(format!(
            "the viewer applied only {} authoritative camera samples for {} generated wheel inputs",
            camera_sample_times.len(),
            inputs.len()
        ));
    }
    if camera_sample_times.len() > receipt_times.len() {
        failures.push(format!(
            "the viewer applied {} camera samples from only {} raw wheel receipts; smoothed input was replayed",
            camera_sample_times.len(),
            receipt_times.len()
        ));
    }
    if input_receipt_max_gap_ns > MAX_GAP_NS {
        failures.push(format!(
            "window wheel-input receipt froze for {:.3} ms",
            ns_ms(input_receipt_max_gap_ns)
        ));
    }
    if main_loop_times.len() < minimum_main_loops {
        failures.push(format!(
            "the application completed only {} in-workflow UI turns; expected at least {minimum_main_loops}",
            main_loop_times.len()
        ));
    }
    if main_loop_max_gap_ns > MAX_GAP_NS {
        failures.push(format!(
            "application main-loop progress froze for {:.3} ms",
            ns_ms(main_loop_max_gap_ns)
        ));
    }
    if change_times.len() < minimum_changes {
        failures.push(format!(
            "the 3D image visibly changed only {} times; expected at least {minimum_changes}",
            change_times.len()
        ));
    }
    if visible_change_max_gap_ns > MAX_GAP_NS {
        failures.push(format!(
            "3D visible output froze for {:.3} ms",
            ns_ms(visible_change_max_gap_ns)
        ));
    }
    if input_to_visible_p99_ns > P99_VISIBLE_LATENCY_NS {
        failures.push(format!(
            "3D p99 generated-wheel to next visible change was {:.3} ms",
            ns_ms(input_to_visible_p99_ns)
        ));
    }
    if !minimum_visible_yavg.is_finite() || minimum_visible_yavg < MIN_VISIBLE_YAVG {
        failures.push(format!(
            "captured 3D panel became blank or unreadable (minimum YAVG {minimum_visible_yavg:.3})"
        ));
    }
    if hard_capacity_reported {
        failures.push("ordinary zoom emitted a hard capacity error".to_owned());
    }
    if !observed_finer_displayed_boundary {
        failures
            .push("the workflow did not complete and display a feasible finer level".to_owned());
    }
    if !observed_adaptive_capacity_boundary {
        failures.push(
            "the workflow did not reach an ideal level constrained to a valid coarser level"
                .to_owned(),
        );
    }
    if !final_s3_ready {
        failures.push(
            "zoom-out did not recover complete/current S3 without restarting the runtime"
                .to_owned(),
        );
    }

    Ok(ZoomSessionMetrics {
        input_count: inputs.len(),
        input_max_gap_ns,
        input_receipt_count: receipt_times.len(),
        input_receipt_max_gap_ns,
        camera_sample_count: camera_sample_times.len(),
        combined_orbit_sample_count: 0,
        main_loop_count: main_loop_times.len(),
        main_loop_max_gap_ns,
        capture_frame_count: capture_times.len(),
        capture_max_gap_ns,
        visible_change_count: change_times.len(),
        visible_change_max_gap_ns,
        input_to_visible_p99_ns,
        minimum_visible_yavg,
        observed_finer_displayed_boundary,
        observed_adaptive_capacity_boundary,
        final_s3_ready,
        invalid_reasons,
        failures,
    })
}

fn validate_combined_orbit(
    inputs: &[InputSample],
    app: &[AppEvent],
    visible: &[VisibleSample],
    metrics: &mut ZoomSessionMetrics,
) {
    let Some(first) = inputs.first() else {
        metrics
            .invalid_reasons
            .push("combined orbit producer emitted no samples".to_owned());
        return;
    };
    let end_ns = inputs
        .last()
        .map_or(first.realtime_ns, |sample| sample.realtime_ns);
    let observation_end_ns = end_ns.saturating_add(MAX_GAP_NS);
    let move_times = app
        .iter()
        .filter(|event| {
            event.kind == AppEventKind::InputMove
                && (first.realtime_ns..=observation_end_ns).contains(&event.realtime_ns)
        })
        .map(|event| event.realtime_ns)
        .collect::<Vec<_>>();
    let camera_times = app
        .iter()
        .filter(|event| {
            event.kind == AppEventKind::CameraSample
                && (first.realtime_ns..=observation_end_ns).contains(&event.realtime_ns)
        })
        .map(|event| event.realtime_ns)
        .collect::<Vec<_>>();
    let main_loop_times = app
        .iter()
        .filter(|event| {
            event.kind == AppEventKind::UiBegin
                && (first.realtime_ns..=observation_end_ns).contains(&event.realtime_ns)
        })
        .map(|event| event.realtime_ns)
        .collect::<Vec<_>>();
    let visible_change_times = visible
        .iter()
        .filter(|sample| {
            (first.realtime_ns..=observation_end_ns).contains(&sample.realtime_ns)
                && sample.ydif > VISIBLE_CHANGE_YDIF
        })
        .map(|sample| sample.realtime_ns)
        .collect::<Vec<_>>();
    metrics.combined_orbit_sample_count = inputs.len();
    let minimum_applied = (inputs.len() * 3).div_ceil(5);
    if move_times.len() < minimum_applied {
        metrics.failures.push(format!(
            "combined orbit reached the UI with only {} of {} generated pointer samples",
            move_times.len(),
            inputs.len()
        ));
    }
    if camera_times.len() < minimum_applied {
        metrics.failures.push(format!(
            "combined orbit applied only {} camera samples from {} generated pointer samples",
            camera_times.len(),
            inputs.len()
        ));
    }
    if visible_change_times.len() < inputs.len() / 4 {
        metrics.failures.push(format!(
            "combined orbit visibly changed only {} times for {} generated pointer samples",
            visible_change_times.len(),
            inputs.len()
        ));
    }
    for (name, times) in [
        ("application main loop", main_loop_times.as_slice()),
        ("3D visible output", visible_change_times.as_slice()),
    ] {
        let gap = bounded_max_gap(times, first.realtime_ns, observation_end_ns);
        if gap > MAX_GAP_NS {
            metrics.failures.push(format!(
                "combined orbit {name} froze for {:.3} ms",
                ns_ms(gap)
            ));
        }
    }
    if app.iter().any(|event| {
        event.kind == AppEventKind::UiStatus
            && (first.realtime_ns..=observation_end_ns).contains(&event.realtime_ns)
            && ((event.value >> 32) & (1 << 7)) != 0
    }) {
        metrics
            .failures
            .push("combined orbit emitted a hard capacity error".to_owned());
    }
}

fn zoom_input_to_next_change_p99(
    inputs: &[ZoomInputSample],
    changes: &[u64],
    gesture_end_ns: u64,
) -> u64 {
    let mut latencies = inputs
        .iter()
        .map(|input| {
            let index = changes.partition_point(|change| *change < input.realtime_ns);
            changes
                .get(index)
                .copied()
                .unwrap_or(gesture_end_ns)
                .saturating_sub(input.realtime_ns)
        })
        .collect::<Vec<_>>();
    latencies.sort_unstable();
    if latencies.is_empty() {
        return u64::MAX;
    }
    let index = (latencies.len() * 99).div_ceil(100).saturating_sub(1);
    latencies[index.min(latencies.len() - 1)]
}

fn adjacent_max_gap(times: &[u64]) -> u64 {
    times
        .windows(2)
        .map(|pair| pair[1].saturating_sub(pair[0]))
        .max()
        .unwrap_or(u64::MAX)
}

fn bounded_max_gap(times: &[u64], start_ns: u64, end_ns: u64) -> u64 {
    let mut previous = start_ns;
    let mut maximum = 0_u64;
    for &time in times {
        if time < start_ns || time > end_ns {
            continue;
        }
        maximum = maximum.max(time.saturating_sub(previous));
        previous = time;
    }
    maximum.max(end_ns.saturating_sub(previous))
}

fn parse_app_trace(path: &Path) -> anyhow::Result<Vec<AppEvent>> {
    let file =
        File::open(path).with_context(|| format!("normal app did not write {}", path.display()))?;
    let mut events = Vec::new();
    for (line_index, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line_index == 0 {
            ensure!(
                line == "monotonic_ns,realtime_ns,kind,x,y,value",
                "app trace header is invalid"
            );
            continue;
        }
        let fields = line.split(',').collect::<Vec<_>>();
        ensure!(fields.len() == 6, "app trace row is malformed");
        let realtime_ns = fields[1].parse().context("invalid app realtime")?;
        let kind = match fields[2] {
            "ui_begin" => AppEventKind::UiBegin,
            "input_move" => AppEventKind::InputMove,
            "input_scroll" => AppEventKind::InputScroll,
            "camera_sample" => AppEventKind::CameraSample,
            "input_button_up" => AppEventKind::InputButtonUp,
            "ui_status" => AppEventKind::UiStatus,
            "linked_lod_status" => AppEventKind::LinkedLodStatus,
            "internal_target_publication_3d" | "coordinated_execution_3d" => {
                AppEventKind::CoordinatedExecutionThreeD
            }
            "internal_target_publication_xy" | "coordinated_execution_xy" => {
                AppEventKind::CoordinatedExecutionXy
            }
            "internal_target_publication_xz" | "coordinated_execution_xz" => {
                AppEventKind::CoordinatedExecutionXz
            }
            "internal_target_publication_yz" | "coordinated_execution_yz" => {
                AppEventKind::CoordinatedExecutionYz
            }
            "presentation_target_changed" => AppEventKind::PresentationTargetChanged,
            "ui_update_duration" => AppEventKind::UiUpdateDuration,
            "renderer_cpu_timing" => AppEventKind::RendererCpuTiming,
            "gpu_timing_3d" => AppEventKind::GpuTimingThreeD,
            "gpu_timing_xy" => AppEventKind::GpuTimingXy,
            "gpu_timing_xz" => AppEventKind::GpuTimingXz,
            "gpu_timing_yz" => AppEventKind::GpuTimingYz,
            "egui_texture_paint_queued_3d" => AppEventKind::EguiTexturePaintThreeD,
            "egui_texture_paint_queued_xy" => AppEventKind::EguiTexturePaintXy,
            "egui_texture_paint_queued_xz" => AppEventKind::EguiTexturePaintXz,
            "egui_texture_paint_queued_yz" => AppEventKind::EguiTexturePaintYz,
            "demand_plans_submitted" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::DemandPlansSubmitted)
            }
            "demand_plans_completed" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::DemandPlansCompleted)
            }
            "demand_plans_cancelled" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::DemandPlansCancelled)
            }
            "demand_planning_completed_ns" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::DemandPlanningCompletedNs)
            }
            "dataset_requests_submitted" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::DatasetRequestsSubmitted)
            }
            "dataset_decodes_started" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::DatasetDecodesStarted)
            }
            "dataset_decodes_completed" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::DatasetDecodesCompleted)
            }
            "dataset_requests_ready" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::DatasetRequestsReady)
            }
            "dataset_requests_cancelled" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::DatasetRequestsCancelled)
            }
            "dataset_requests_failed" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::DatasetRequestsFailed)
            }
            "dataset_queue_wait_ns" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::DatasetQueueWaitNs)
            }
            "dataset_decode_time_ns" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::DatasetDecodeTimeNs)
            }
            "dataset_decoded_output_bytes" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::DatasetDecodedOutputBytes)
            }
            "source_physical_range_reads" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::SourcePhysicalRangeReads)
            }
            "source_physical_encoded_bytes" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::SourcePhysicalEncodedBytes)
            }
            "source_codec_decodes" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::SourceCodecDecodes)
            }
            "source_codec_decoded_bytes" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::SourceCodecDecodedBytes)
            }
            "source_codec_decode_time_ns" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::SourceCodecDecodeTimeNs)
            }
            "renderer_frames_executed" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::RendererFramesExecuted)
            }
            "renderer_queue_submissions" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::RendererQueueSubmissions)
            }
            "renderer_uploaded_resources" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::RendererUploadedResources)
            }
            "renderer_uploaded_payload_bytes" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::RendererUploadedPayloadBytes)
            }
            "renderer_color_submissions" => {
                AppEventKind::BoundaryCounter(BoundaryCounterKind::RendererColorSubmissions)
            }
            "dropped_events" => AppEventKind::DroppedEvents,
            "ui_end" => AppEventKind::UiEnd,
            _ => AppEventKind::Other,
        };
        let x = fields[3].parse().context("invalid app trace x")?;
        let y = fields[4].parse().context("invalid app trace y")?;
        let value = fields[5].parse().context("invalid app trace value")?;
        events.push(AppEvent {
            realtime_ns,
            kind,
            x,
            y,
            value,
        });
    }
    Ok(events)
}

fn parse_visible_samples(path: &Path) -> anyhow::Result<Vec<VisibleSample>> {
    let file = File::open(path)
        .with_context(|| format!("external capture did not write {}", path.display()))?;
    let mut samples = Vec::new();
    let mut pending_pts_us: Option<u64> = None;
    let mut pending_yavg: Option<f64> = None;
    let mut pending_ydif: Option<f64> = None;
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.starts_with("frame:") {
            if let (Some(pts_us), Some(yavg), Some(ydif)) = (
                pending_pts_us.take(),
                pending_yavg.take(),
                pending_ydif.take(),
            ) {
                samples.push(VisibleSample {
                    realtime_ns: pts_us.saturating_mul(1_000),
                    yavg,
                    ydif,
                });
            }
            pending_pts_us = line
                .split_whitespace()
                .find_map(|field| field.strip_prefix("pts:"))
                .map(str::parse)
                .transpose()
                .context("invalid visible frame timestamp")?;
        } else if let Some(value) = line.strip_prefix("lavfi.signalstats.YAVG=") {
            pending_yavg = Some(value.parse().context("invalid visible YAVG")?);
        } else if let Some(value) = line.strip_prefix("lavfi.signalstats.YDIF=") {
            pending_ydif = Some(value.parse().context("invalid visible YDIF")?);
        }
    }
    if let (Some(pts_us), Some(yavg), Some(ydif)) = (pending_pts_us, pending_yavg, pending_ydif) {
        samples.push(VisibleSample {
            realtime_ns: pts_us.saturating_mul(1_000),
            yavg,
            ydif,
        });
    }
    Ok(samples)
}

fn write_input_csv(path: &Path, samples: &[InputSample]) -> anyhow::Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    writeln!(writer, "monotonic_ns,realtime_ns,x,y")?;
    for sample in samples {
        writeln!(
            writer,
            "{},{},{},{}",
            sample.monotonic_ns, sample.realtime_ns, sample.x, sample.y
        )?;
    }
    writer.flush()?;
    Ok(())
}

fn write_linked_zoom_correctness_summary(
    path: &Path,
    metrics: &LinkedZoomCorrectnessMetrics,
) -> anyhow::Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    writeln!(
        writer,
        "result={}",
        if metrics.passed() { "PASS" } else { "FAIL" }
    )?;
    writeln!(writer, "claim=endpoint_correctness_only")?;
    writeln!(writer, "continuity_result=NOT_MEASURED")?;
    writeln!(
        writer,
        "monitor_visibility=UNOBSERVED_OWNER_CONFIRMATION_REQUIRED"
    )?;
    writeln!(
        writer,
        "generated_input_count={}",
        metrics.generated_input_count
    )?;
    writeln!(
        writer,
        "received_input_count={}",
        metrics.received_input_count
    )?;
    for (index, name) in ["xy", "xz", "yz"].into_iter().enumerate() {
        writeln!(
            writer,
            "{name}_internal_publication_count={}",
            metrics.linked_publication_count[index]
        )?;
        writeln!(
            writer,
            "{name}_internal_published_geometry_scale_min={:.6}",
            metrics.linked_scale_range[index].0
        )?;
        writeln!(
            writer,
            "{name}_internal_published_geometry_scale_max={:.6}",
            metrics.linked_scale_range[index].1
        )?;
        writeln!(
            writer,
            "{name}_client_surface_artifacts_differ={}",
            metrics.client_surface_artifacts_differ[index]
        )?;
    }
    writeln!(writer, "reached_exact_s0={}", metrics.reached_exact_s0)?;
    writeln!(
        writer,
        "recovered_initial_exact_scales={}",
        metrics.recovered_initial_exact_scales
    )?;
    writeln!(
        writer,
        "independent_3d_camera_unchanged={}",
        metrics.independent_3d_camera_unchanged
    )?;
    for reason in &metrics.invalid_reasons {
        writeln!(writer, "invalid={reason}")?;
    }
    for failure in &metrics.failures {
        writeln!(writer, "failure={failure}")?;
    }
    writer.flush()?;
    Ok(())
}

fn write_linked_lod_diagnostic_summary(
    path: &Path,
    metrics: &LinkedLodDiagnosticMetrics,
) -> anyhow::Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    writeln!(
        writer,
        "result={}",
        if metrics.valid() { "VALID" } else { "INVALID" }
    )?;
    writeln!(writer, "claim=diagnostic_measurements_only")?;
    writeln!(writer, "performance_acceptance=NOT_EVALUATED")?;
    writeln!(
        writer,
        "monitor_continuity=UNOBSERVED_OWNER_CONFIRMATION_REQUIRED"
    )?;
    writeln!(writer, "window_surface_present=UNOBSERVED")?;
    writeln!(
        writer,
        "egui_paint_boundary=TEXTURE_IMAGE_COMMAND_QUEUED_NOT_PRESENTED"
    )?;
    writeln!(
        writer,
        "returned_to_exact_s3={}",
        metrics.returned_to_exact_s3
    )?;
    writeln!(
        writer,
        "trace_dropped_events={}",
        metrics.trace_dropped_events
    )?;
    for phase in &metrics.phases {
        let prefix = format!("s{}", phase.scale_level);
        writeln!(
            writer,
            "{prefix}_duration_ms={:.3}",
            ns_ms(phase.duration_ns)
        )?;
        writeln!(
            writer,
            "{prefix}_generated_input_count={}",
            phase.generated_input_count
        )?;
        writeln!(
            writer,
            "{prefix}_generated_input_max_gap_ms={:.3}",
            ns_ms(phase.generated_input_max_gap_ns)
        )?;
        writeln!(
            writer,
            "{prefix}_received_input_count={}",
            phase.received_input_count
        )?;
        writeln!(
            writer,
            "{prefix}_received_input_max_gap_ms={:.3}",
            ns_ms(phase.received_input_max_gap_ns)
        )?;
        writeln!(writer, "{prefix}_ui_update_count={}", phase.ui_update_count)?;
        writeln!(
            writer,
            "{prefix}_ui_update_duration_p99_ms={:.3}",
            ns_ms(phase.ui_update_duration_p99_ns)
        )?;
        writeln!(
            writer,
            "{prefix}_ui_update_duration_max_ms={:.3}",
            ns_ms(phase.ui_update_duration_max_ns)
        )?;
        for (index, panel) in ["xy", "xz", "yz"].into_iter().enumerate() {
            writeln!(
                writer,
                "{prefix}_{panel}_internal_publication_count={}",
                phase.internal_publication_count[index]
            )?;
            writeln!(
                writer,
                "{prefix}_{panel}_internal_publication_max_gap_ms={:.3}",
                ns_ms(phase.internal_publication_max_gap_ns[index])
            )?;
            writeln!(
                writer,
                "{prefix}_{panel}_internal_published_scale_min={:.6}",
                phase.internal_published_scale_range[index].0
            )?;
            writeln!(
                writer,
                "{prefix}_{panel}_internal_published_scale_max={:.6}",
                phase.internal_published_scale_range[index].1
            )?;
            writeln!(
                writer,
                "{prefix}_{panel}_internal_publication_to_egui_paint_p99_ms={:.3}",
                ns_ms(phase.internal_publication_to_egui_paint_p99_ns[index])
            )?;
            writeln!(
                writer,
                "{prefix}_{panel}_egui_paint_queued_count={}",
                phase.egui_paint_queued_count[index]
            )?;
        }
        writeln!(
            writer,
            "{prefix}_renderer_cpu_sample_count={}",
            phase.renderer_cpu_sample_count
        )?;
        writeln!(
            writer,
            "{prefix}_renderer_cpu_planning_p99_ms={:.3}",
            ns_ms(phase.renderer_cpu_planning_p99_ns)
        )?;
        writeln!(
            writer,
            "{prefix}_renderer_cpu_planning_max_ms={:.3}",
            ns_ms(phase.renderer_cpu_planning_max_ns)
        )?;
        writeln!(
            writer,
            "{prefix}_renderer_queue_submit_p99_ms={:.3}",
            ns_ms(phase.renderer_queue_submit_p99_ns)
        )?;
        writeln!(
            writer,
            "{prefix}_linked_gpu_sample_count={}",
            phase.gpu_sample_count
        )?;
        write_optional_ns(
            &mut writer,
            &format!("{prefix}_gpu_batch_p99_ms"),
            phase.gpu_batch_p99_ns,
        )?;
        write_optional_ns(
            &mut writer,
            &format!("{prefix}_gpu_batch_max_ms"),
            phase.gpu_batch_max_ns,
        )?;
        write_optional_ns(
            &mut writer,
            &format!("{prefix}_linked_gpu_pass_p99_ms"),
            phase.linked_gpu_pass_p99_ns,
        )?;
        write_optional_ns(
            &mut writer,
            &format!("{prefix}_linked_gpu_pass_max_ms"),
            phase.linked_gpu_pass_max_ns,
        )?;
        for (counter, delta) in &phase.counter_deltas {
            writeln!(writer, "{prefix}_{counter}_delta={delta}")?;
        }
        writeln!(writer, "{prefix}_settled_before={:?}", phase.settled_before)?;
        writeln!(writer, "{prefix}_settled_after={:?}", phase.settled_after)?;
    }
    for reason in &metrics.invalid_reasons {
        writeln!(writer, "invalid={reason}")?;
    }
    writer.flush()?;
    Ok(())
}

fn write_optional_ns(
    writer: &mut impl Write,
    name: &str,
    value: Option<u64>,
) -> anyhow::Result<()> {
    match value {
        Some(value) => writeln!(writer, "{name}={:.3}", ns_ms(value))?,
        None => writeln!(writer, "{name}=UNAVAILABLE")?,
    }
    Ok(())
}

fn write_zoom_input_csv(path: &Path, samples: &[ZoomInputSample]) -> anyhow::Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    writeln!(writer, "monotonic_ns,realtime_ns,direction")?;
    for sample in samples {
        writeln!(
            writer,
            "{},{},{}",
            sample.monotonic_ns, sample.realtime_ns, sample.direction
        )?;
    }
    writer.flush()?;
    Ok(())
}

fn write_zoom_summary(path: &Path, metrics: &ZoomSessionMetrics) -> anyhow::Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    writeln!(
        writer,
        "result={}",
        if metrics.passed() { "PASS" } else { "FAIL" }
    )?;
    writeln!(writer, "input_count={}", metrics.input_count)?;
    writeln!(
        writer,
        "input_max_gap_ms={:.3}",
        ns_ms(metrics.input_max_gap_ns)
    )?;
    writeln!(
        writer,
        "input_receipt_count={}",
        metrics.input_receipt_count
    )?;
    writeln!(
        writer,
        "input_receipt_max_gap_ms={:.3}",
        ns_ms(metrics.input_receipt_max_gap_ns)
    )?;
    writeln!(
        writer,
        "camera_sample_count={}",
        metrics.camera_sample_count
    )?;
    writeln!(
        writer,
        "combined_orbit_sample_count={}",
        metrics.combined_orbit_sample_count
    )?;
    writeln!(writer, "main_loop_count={}", metrics.main_loop_count)?;
    writeln!(
        writer,
        "main_loop_max_gap_ms={:.3}",
        ns_ms(metrics.main_loop_max_gap_ns)
    )?;
    writeln!(
        writer,
        "capture_frame_count={}",
        metrics.capture_frame_count
    )?;
    writeln!(
        writer,
        "capture_max_gap_ms={:.3}",
        ns_ms(metrics.capture_max_gap_ns)
    )?;
    writeln!(
        writer,
        "visible_change_count={}",
        metrics.visible_change_count
    )?;
    writeln!(
        writer,
        "visible_change_max_gap_ms={:.3}",
        ns_ms(metrics.visible_change_max_gap_ns)
    )?;
    writeln!(
        writer,
        "input_to_visible_p99_ms={:.3}",
        ns_ms(metrics.input_to_visible_p99_ns)
    )?;
    writeln!(
        writer,
        "minimum_visible_yavg={:.3}",
        metrics.minimum_visible_yavg
    )?;
    writeln!(
        writer,
        "observed_finer_displayed_boundary={}",
        metrics.observed_finer_displayed_boundary
    )?;
    writeln!(
        writer,
        "observed_adaptive_capacity_boundary={}",
        metrics.observed_adaptive_capacity_boundary
    )?;
    writeln!(writer, "final_s3_ready={}", metrics.final_s3_ready)?;
    for reason in &metrics.invalid_reasons {
        writeln!(writer, "invalid={reason}")?;
    }
    for failure in &metrics.failures {
        writeln!(writer, "failure={failure}")?;
    }
    writer.flush()?;
    Ok(())
}

fn wait_for_window(app: &mut ManagedChild) -> anyhow::Result<u64> {
    let started = Instant::now();
    let mut next_progress = started;
    loop {
        if let Some(status) = app.try_wait()? {
            bail!("normal viewer exited before mapping its window: {status}");
        }
        // `xdotool search --pid` walks the X11 tree and can return the window
        // manager's decoration windows before the application client. On a
        // desktop with another Mirante4D window it can even lead activation
        // into the wrong client. `wmctrl -lp` is the EWMH client list and
        // carries the exact client PID, which is the boundary needed here.
        let output = Command::new("wmctrl")
            .arg("-lp")
            .output()
            .context("failed to list mapped X11 client windows")?;
        if output.status.success()
            && let Some(id) = String::from_utf8_lossy(&output.stdout)
                .lines()
                .find_map(|line| {
                    let fields = line.split_whitespace().collect::<Vec<_>>();
                    (fields.len() >= 5
                        && fields[2].parse::<u32>().ok() == Some(app.id())
                        && fields[4..].join(" ") == "Mirante4D")
                        .then(|| u64::from_str_radix(fields[0].trim_start_matches("0x"), 16).ok())
                        .flatten()
                })
        {
            return Ok(id);
        }
        let now = Instant::now();
        if now >= started + WINDOW_TIMEOUT {
            bail!("normal viewer did not map within {WINDOW_TIMEOUT:?}");
        }
        if now >= next_progress {
            println!(
                "viewer_oblique_continuity state=waiting_for_window elapsed_s={:.1}",
                started.elapsed().as_secs_f64()
            );
            next_progress = now + Duration::from_secs(2);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn configure_window(window: u64) -> anyhow::Result<()> {
    // Some X11 window managers ignore an EWMH state request made in the small
    // interval after a client maps but before it has been placed on a desktop.
    // Activating first closes that race and makes the geometry observed after
    // this function the geometry the later resize round trip must restore.
    activate_window(window)?;
    let window_hex = format!("0x{window:08x}");
    let status = Command::new("wmctrl")
        .args([
            "-i",
            "-r",
            &window_hex,
            "-b",
            "remove,maximized_vert,maximized_horz",
        ])
        .status()
        .context("failed to restore the normal viewer window")?;
    ensure!(
        status.success(),
        "window manager rejected viewer window restoration"
    );
    run_xdotool(&["windowsize", "--sync", &window.to_string(), "1280", "720"])?;
    thread::sleep(Duration::from_millis(750));
    Ok(())
}

fn exercise_resize_roundtrip(window: u64) -> anyhow::Result<()> {
    let baseline = window_geometry(window)?;
    let window_hex = format!("0x{window:08x}");
    let status = Command::new("wmctrl")
        .args([
            "-i",
            "-r",
            &window_hex,
            "-b",
            "remove,maximized_vert,maximized_horz",
        ])
        .status()
        .context("failed to unmaximize normal viewer for resize exercise")?;
    ensure!(
        status.success(),
        "window manager rejected viewer unmaximization"
    );
    activate_window(window)?;
    run_xdotool(&["windowsize", "--sync", &window.to_string(), "1100", "650"])?;
    thread::sleep(Duration::from_millis(500));
    let resized = window_geometry(window)?;
    ensure!(
        resized.width != baseline.width || resized.height != baseline.height,
        "combined workflow did not produce a real viewer resize"
    );
    ensure!(
        resized.width >= 900 && resized.height >= 600,
        "combined workflow resize violated the normal viewer minimum"
    );

    configure_window(window)?;
    let restored = window_geometry(window)?;
    ensure!(
        restored.width == baseline.width && restored.height == baseline.height,
        "combined workflow did not restore the mapped viewer size: baseline {}x{}, restored {}x{}",
        baseline.width,
        baseline.height,
        restored.width,
        restored.height
    );
    println!(
        "viewer_zoom_continuity state=resize_roundtrip baseline={}x{} resized={}x{} restored={}x{}",
        baseline.width,
        baseline.height,
        resized.width,
        resized.height,
        restored.width,
        restored.height
    );
    Ok(())
}

fn activate_window(window: u64) -> anyhow::Result<()> {
    run_xdotool(&["windowactivate", "--sync", &window.to_string()])
}

fn active_window() -> anyhow::Result<u64> {
    let output = Command::new("xdotool")
        .arg("getactivewindow")
        .output()
        .context("failed to read the active X11 window")?;
    ensure!(
        output.status.success(),
        "xdotool could not read the active X11 window"
    );
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .context("active X11 window id is invalid")
}

fn click_four_panel(window: u64, target_path: &Path) -> anyhow::Result<()> {
    // These points cover the compact selector row across the supported egui
    // font/WM-decoration combinations. Success is not inferred from a click:
    // the normal app must report all four fitted presentation rectangles
    // before setup may continue.
    let candidates = [(100, 52), (115, 52), (90, 52), (105, 46), (105, 58)];
    for (x, y) in candidates {
        activate_window(window)?;
        move_pointer_with_xdotool(window, x, y)?;
        run_xdotool(&["click", "1"]).context("failed to click the real 4 Panel UI control")?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if target_path.is_file() {
                println!("viewer_oblique_continuity state=four_panel_selected x={x} y={y}");
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
    bail!("real pointer clicks did not select the reported 4 Panel control")
}

fn move_pointer_with_xdotool(window: u64, x: i32, y: i32) -> anyhow::Result<()> {
    run_xdotool(&[
        "mousemove",
        "--window",
        &window.to_string(),
        &x.to_string(),
        &y.to_string(),
    ])
}

fn set_input_down(down: bool) -> anyhow::Result<()> {
    if down {
        for attempt in 1..=3 {
            // `keydown` consumes every remaining token as another key
            // sequence; it cannot be command-chained with `mousedown`.
            run_xdotool(&["keydown", "Shift_L"])?;
            run_xdotool(&["mousedown", "1"])?;
            let mask = XPointer::open()?.pointer_mask()?;
            if mask & (X11_SHIFT_MASK | X11_BUTTON1_MASK) == X11_SHIFT_MASK | X11_BUTTON1_MASK {
                return Ok(());
            }
            let _ = run_xdotool(&["mouseup", "1"]);
            let _ = run_xdotool(&["keyup", "Shift_L"]);
            if attempt < 3 {
                thread::sleep(Duration::from_millis(50));
            }
        }
        bail!("X11 did not retain simultaneous Shift and primary-button state")
    } else {
        let mouse = run_xdotool(&["mouseup", "1"]);
        let shift = run_xdotool(&["keyup", "Shift_L"]);
        mouse.and(shift)
    }
}

fn run_xdotool(args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("xdotool")
        .args(args)
        .status()
        .context("failed to execute xdotool")?;
    ensure!(status.success(), "xdotool command failed: {args:?}");
    Ok(())
}

fn wait_for_ready(app: &mut ManagedChild, ready_path: &Path) -> anyhow::Result<()> {
    let started = Instant::now();
    let mut next_progress = started;
    loop {
        if ready_path.is_file() {
            return Ok(());
        }
        if let Some(status) = app.try_wait()? {
            bail!("normal viewer exited before four-panel S3 readiness: {status}");
        }
        let now = Instant::now();
        if now >= started + READY_TIMEOUT {
            bail!(
                "normal viewer did not reach complete/current four-panel S3 readiness within {READY_TIMEOUT:?}"
            );
        }
        if now >= next_progress {
            println!(
                "viewer_oblique_continuity state=loading_four_panel_s3 elapsed_s={:.1}",
                started.elapsed().as_secs_f64()
            );
            next_progress = now + Duration::from_secs(2);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn window_geometry(window: u64) -> anyhow::Result<WindowGeometry> {
    let output = Command::new("xdotool")
        .args(["getwindowgeometry", "--shell", &window.to_string()])
        .output()
        .context("failed to read viewer window geometry")?;
    ensure!(
        output.status.success(),
        "xdotool could not read viewer window geometry"
    );
    let mut width = None;
    let mut height = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(value) = line.strip_prefix("WIDTH=") {
            width = Some(value.parse().context("invalid viewer width")?);
        } else if let Some(value) = line.strip_prefix("HEIGHT=") {
            height = Some(value.parse().context("invalid viewer height")?);
        }
    }
    // `xdotool getwindowgeometry` reports the window-manager frame origin on
    // this X11 desktop, while the app's rectangles are client coordinates.
    // Translate the actual client window to root coordinates so captures do
    // not miss the top of each image and include unrelated UI below it.
    let (x, y) = XPointer::open()?.window_root_origin(window)?;
    Ok(WindowGeometry {
        x,
        y,
        width: width.context("viewer geometry omitted width")?,
        height: height.context("viewer geometry omitted height")?,
    })
}

struct ManagedChild {
    child: Option<Child>,
}

impl ManagedChild {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn id(&self) -> u32 {
        self.child.as_ref().expect("managed child is present").id()
    }

    fn try_wait(&mut self) -> anyhow::Result<Option<std::process::ExitStatus>> {
        self.child
            .as_mut()
            .expect("managed child is present")
            .try_wait()
            .context("failed to poll child process")
    }

    fn terminate_gracefully(&mut self, timeout: Duration) -> anyhow::Result<()> {
        let pid = self.id();
        send_signal(pid, 15)?;
        let started = Instant::now();
        loop {
            if let Some(status) = self.try_wait()? {
                self.child.take();
                ensure!(status.success(), "normal viewer closed with {status}");
                return Ok(());
            }
            if started.elapsed() >= timeout {
                let child = self.child.as_mut().expect("managed child is present");
                process::terminate_process_tree(child);
                let _ = child.wait();
                self.child.take();
                bail!("normal viewer did not close within {timeout:?}");
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn finish_capture(&mut self, timeout: Duration) -> anyhow::Result<()> {
        let pid = self.id();
        // ffmpeg handles SIGINT by flushing the current filter/output state.
        // Its conventional interrupted exit status is not a capture failure;
        // the parsed frame timeline below remains the evidence authority.
        send_signal(pid, 2)?;
        let started = Instant::now();
        loop {
            if self.try_wait()?.is_some() {
                self.child.take();
                return Ok(());
            }
            if started.elapsed() >= timeout {
                let child = self.child.as_mut().expect("managed child is present");
                process::terminate_process_tree(child);
                let _ = child.wait();
                self.child.take();
                bail!("visible-output capture did not flush within {timeout:?}");
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        let Some(pid) = self.child.as_ref().map(Child::id) else {
            return;
        };
        let _ = send_signal(pid, 15);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let exited = self
                .child
                .as_mut()
                .and_then(|child| child.try_wait().ok().flatten())
                .is_some();
            if exited {
                self.child.take();
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        if let Some(child) = self.child.as_mut() {
            process::terminate_process_tree(child);
            let _ = child.wait();
        }
    }
}

struct InputRelease {
    armed: bool,
}

impl InputRelease {
    fn new() -> Self {
        Self { armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for InputRelease {
    fn drop(&mut self) {
        if self.armed {
            let _ = set_input_down(false);
        }
    }
}

const LINUX_EV_SYN: u16 = 0;
const LINUX_EV_KEY: c_int = 1;
const LINUX_EV_REL: c_int = 2;
const LINUX_SYN_REPORT: u16 = 0;
const LINUX_REL_X: c_int = 0;
const LINUX_REL_Y: c_int = 1;
const LINUX_REL_WHEEL: c_int = 8;
const LINUX_BTN_LEFT: c_int = 0x110;
const LINUX_BUS_USB: u16 = 0x03;
const UINPUT_NAME_SIZE: usize = 80;
const UI_DEV_CREATE: c_ulong = 0x5501;
const UI_DEV_DESTROY: c_ulong = 0x5502;
const UI_DEV_SETUP: c_ulong = 0x405c_5503;
const UI_SET_EVBIT: c_ulong = 0x4004_5564;
const UI_SET_KEYBIT: c_ulong = 0x4004_5565;
const UI_SET_RELBIT: c_ulong = 0x4004_5566;

#[repr(C)]
struct LinuxInputId {
    bus_type: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct LinuxUInputSetup {
    id: LinuxInputId,
    name: [u8; UINPUT_NAME_SIZE],
    ff_effects_max: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct LinuxTimeval {
    seconds: c_long,
    microseconds: c_long,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct LinuxInputEvent {
    time: LinuxTimeval,
    event_type: u16,
    code: u16,
    value: i32,
}

struct UInputWheel {
    file: File,
}

impl UInputWheel {
    fn create() -> anyhow::Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .context("could not open /dev/uinput for the real wheel boundary")?;
        let fd = file.as_raw_fd();
        ioctl_scalar(fd, UI_SET_EVBIT, LINUX_EV_KEY, "UI_SET_EVBIT(EV_KEY)")?;
        ioctl_scalar(fd, UI_SET_EVBIT, LINUX_EV_REL, "UI_SET_EVBIT(EV_REL)")?;
        ioctl_scalar(fd, UI_SET_KEYBIT, LINUX_BTN_LEFT, "UI_SET_KEYBIT(BTN_LEFT)")?;
        for (code, label) in [
            (LINUX_REL_X, "UI_SET_RELBIT(REL_X)"),
            (LINUX_REL_Y, "UI_SET_RELBIT(REL_Y)"),
            (LINUX_REL_WHEEL, "UI_SET_RELBIT(REL_WHEEL)"),
        ] {
            ioctl_scalar(fd, UI_SET_RELBIT, code, label)?;
        }
        let mut setup = LinuxUInputSetup {
            id: LinuxInputId {
                bus_type: LINUX_BUS_USB,
                vendor: 0x4d34,
                product: 0x0001,
                version: 1,
            },
            name: [0; UINPUT_NAME_SIZE],
            ff_effects_max: 0,
        };
        let name = b"Mirante4D E2E Wheel";
        setup.name[..name.len()].copy_from_slice(name);
        // SAFETY: `fd` is an open uinput descriptor and `setup` has the exact
        // Linux `uinput_setup` C layout for the duration of this call.
        let configured = unsafe { ioctl(fd, UI_DEV_SETUP, &setup as *const LinuxUInputSetup) };
        ensure!(
            configured == 0,
            "UI_DEV_SETUP failed: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: the configured descriptor owns one not-yet-created device.
        let created = unsafe { ioctl(fd, UI_DEV_CREATE) };
        ensure!(
            created == 0,
            "UI_DEV_CREATE failed: {}",
            std::io::Error::last_os_error()
        );
        // Let udev/libinput and the X input stack attach the kernel device
        // before the independently timed interaction starts.
        thread::sleep(Duration::from_millis(750));
        Ok(Self { file })
    }

    fn scroll(&mut self, direction: i8, notches: usize) -> anyhow::Result<()> {
        ensure!(
            matches!(direction, -1 | 1),
            "wheel direction must be +1 or -1"
        );
        let wheel_code = u16::try_from(LINUX_REL_WHEEL).expect("REL_WHEEL fits u16");
        for _ in 0..notches {
            let events = [
                LinuxInputEvent {
                    time: LinuxTimeval {
                        seconds: 0,
                        microseconds: 0,
                    },
                    event_type: u16::try_from(LINUX_EV_REL).expect("EV_REL fits u16"),
                    code: wheel_code,
                    value: i32::from(direction),
                },
                LinuxInputEvent {
                    time: LinuxTimeval {
                        seconds: 0,
                        microseconds: 0,
                    },
                    event_type: LINUX_EV_SYN,
                    code: LINUX_SYN_REPORT,
                    value: 0,
                },
            ];
            // SAFETY: `events` is a live contiguous POD C array and the
            // borrowed bytes do not outlive it.
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    events.as_ptr().cast::<u8>(),
                    std::mem::size_of_val(&events),
                )
            };
            self.file
                .write_all(bytes)
                .context("kernel wheel event injection failed")?;
        }
        Ok(())
    }
}

impl Drop for UInputWheel {
    fn drop(&mut self) {
        // SAFETY: this descriptor created one device and remains open here.
        unsafe {
            ioctl(self.file.as_raw_fd(), UI_DEV_DESTROY);
        }
    }
}

fn ioctl_scalar(fd: c_int, request: c_ulong, value: c_int, operation: &str) -> anyhow::Result<()> {
    // SAFETY: each request is a fixed uinput scalar capability operation.
    let result = unsafe { ioctl(fd, request, value) };
    ensure!(
        result == 0,
        "{operation} failed: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

struct XPointer {
    display: *mut c_void,
    root: c_ulong,
}

impl XPointer {
    fn open() -> anyhow::Result<Self> {
        // SAFETY: a null display name asks Xlib to use the process DISPLAY
        // environment. The returned handle is checked and owned by this
        // wrapper until `Drop`.
        let display = unsafe { XOpenDisplay(ptr::null()) };
        ensure!(!display.is_null(), "XOpenDisplay returned null");
        // SAFETY: `display` is a checked live handle.
        let root = unsafe { XDefaultRootWindow(display) };
        ensure!(root != 0, "XDefaultRootWindow returned no root window");
        Ok(Self { display, root })
    }

    fn move_absolute(&mut self, x: i32, y: i32) -> anyhow::Result<()> {
        // SAFETY: `display` and `root` are live Xlib handles and
        // `XWarpPointer` copies the integer coordinates during this call. A
        // root destination keeps motion independent of implicit widget grabs.
        let result = unsafe { XWarpPointer(self.display, 0, self.root, 0, 0, 0, 0, x, y) };
        ensure!(
            result != 0,
            "XWarpPointer rejected a generated input sample"
        );
        // SAFETY: the same live display handle is synchronized with the X
        // server. This proves the generated event left the independent input
        // process without waiting for application progress.
        unsafe {
            XSync(self.display, 0);
        }
        Ok(())
    }

    fn window_root_origin(&self, window: u64) -> anyhow::Result<(i32, i32)> {
        let window = c_ulong::try_from(window).context("X11 window ID overflowed")?;
        let mut x = 0;
        let mut y = 0;
        let mut child = 0;
        // SAFETY: the display and root are live Xlib handles, `window` came
        // from X11, and all output pointers remain valid for the call.
        let translated = unsafe {
            XTranslateCoordinates(
                self.display,
                window,
                self.root,
                0,
                0,
                &mut x,
                &mut y,
                &mut child,
            )
        };
        ensure!(
            translated != 0,
            "XTranslateCoordinates could not locate the viewer client"
        );
        Ok((x, y))
    }

    fn pointer_mask(&self) -> anyhow::Result<c_uint> {
        let mut root_return = 0;
        let mut child_return = 0;
        let mut root_x = 0;
        let mut root_y = 0;
        let mut window_x = 0;
        let mut window_y = 0;
        let mut mask = 0;
        // SAFETY: the display and root are live handles and every output
        // pointer remains valid for the duration of this synchronous query.
        let queried = unsafe {
            XQueryPointer(
                self.display,
                self.root,
                &mut root_return,
                &mut child_return,
                &mut root_x,
                &mut root_y,
                &mut window_x,
                &mut window_y,
                &mut mask,
            )
        };
        ensure!(
            queried != 0,
            "XQueryPointer could not read real input state"
        );
        Ok(mask)
    }
}

impl Drop for XPointer {
    fn drop(&mut self) {
        if !self.display.is_null() {
            // SAFETY: this wrapper owns the live handle and closes it once.
            unsafe {
                XCloseDisplay(self.display);
            }
        }
    }
}

fn send_signal(pid: u32, signal: c_int) -> anyhow::Result<()> {
    let pid = c_int::try_from(pid).context("process ID overflowed")?;
    // SAFETY: `pid` is the exact spawned child and the signal is a fixed
    // POSIX value selected by this module.
    let result = unsafe { kill(pid, signal) };
    ensure!(result == 0, "failed to signal normal viewer process {pid}");
    Ok(())
}

fn clock_pair_ns() -> (u64, u64) {
    (clock_ns(ClockId::Monotonic), clock_ns(ClockId::Realtime))
}

fn clock_ns(clock: ClockId) -> u64 {
    let time = clock_gettime(clock);
    u64::try_from(time.tv_sec)
        .unwrap_or(0)
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::try_from(time.tv_nsec).unwrap_or(0))
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn ns_ms(ns: u64) -> f64 {
    ns as f64 / 1_000_000.0
}

fn print_help() {
    println!(
        "\
usage: cargo xtask viewer-oblique-continuity \
  [--workflow linked-lod-diagnostic|linked-zoom|zoom|combined] [--dataset /absolute/package.m4d] \
  [--duration-secs 30] [--runs 1] [--skip-build] [--video] [--allow-host-stress]

Launches the normal release viewer on the real X11 display, clicks the actual
4 Panel control, and waits for complete/current S3. The default
linked-lod-diagnostic workflow settles and warms linked S3, S1, and S0, then
drives bounded continuous Shift + primary-button XY motion at an independent
60 Hz for the requested duration at each level. It records generated/received
input, internal target publication, planning/data/renderer counters, renderer
CPU and linked GPU timings, and egui texture-paint queuing. It deliberately
does not claim to observe window-surface presentation or monitor continuity;
the owner remains the visual authority.

The linked-lod-diagnostic and linked-zoom workflows are quarantined by default
after an S0 transition froze the development desktop. `--allow-host-stress`
is an explicit acknowledgement for a later controlled run; it is not a claim
that the workload has been made safe or product-validated.

The linked-zoom workflow sends kernel-level Ctrl+REL_WHEEL input over XY,
requires exact S0 settlement and recovery, keeps 3D independent, and retains
fine/recovered X11 client-surface artifacts. It is endpoint correctness only:
those artifacts are not compositor or monitor evidence and no visibility
latency is reported. The zoom workflow drives
balanced real X11 wheel cycles over the 3D panel, crosses a finer/adaptive LOD
boundary, returns to the original zoom, and externally observes the 3D pixels
and truthful LOD status. The combined workflow adds a real window-resize
round-trip and a real 3D orbit round-trip before the same zoom exercise.
All workflows use finite deadlines and close automatically. Lossless video is omitted by default;
--video retains it for diagnosis.

Plain local CSV/text is written below
target/mirante4d/viewer-oblique-continuity/. No product-automation command,
readback, receipt, provenance graph, or result hash is used."
    );
}

const X11_Z_PIXMAP: c_int = 2;

#[repr(C)]
struct XImageHeader {
    width: c_int,
    height: c_int,
    xoffset: c_int,
    format: c_int,
    data: *mut c_char,
    byte_order: c_int,
    bitmap_unit: c_int,
    bitmap_bit_order: c_int,
    bitmap_pad: c_int,
    depth: c_int,
    bytes_per_line: c_int,
    bits_per_pixel: c_int,
    red_mask: c_ulong,
    green_mask: c_ulong,
    blue_mask: c_ulong,
    obdata: *mut c_char,
}

fn initialize_xlib_threads() -> anyhow::Result<()> {
    // SAFETY: this is called before the xtask makes any other in-process Xlib
    // call, enabling the independent sampler and input handles to coexist.
    let initialized = unsafe { XInitThreads() };
    ensure!(initialized != 0, "XInitThreads could not initialize Xlib");
    Ok(())
}

#[link(name = "X11")]
unsafe extern "C" {
    fn XInitThreads() -> c_int;
    fn XOpenDisplay(display_name: *const c_char) -> *mut c_void;
    fn XCloseDisplay(display: *mut c_void) -> c_int;
    fn XDefaultRootWindow(display: *mut c_void) -> c_ulong;
    fn XSync(display: *mut c_void, discard: c_int) -> c_int;
    fn XWarpPointer(
        display: *mut c_void,
        source_window: c_ulong,
        destination_window: c_ulong,
        source_x: c_int,
        source_y: c_int,
        source_width: c_uint,
        source_height: c_uint,
        destination_x: c_int,
        destination_y: c_int,
    ) -> c_int;
    fn XTranslateCoordinates(
        display: *mut c_void,
        source_window: c_ulong,
        destination_window: c_ulong,
        source_x: c_int,
        source_y: c_int,
        destination_x: *mut c_int,
        destination_y: *mut c_int,
        child: *mut c_ulong,
    ) -> c_int;
    fn XQueryPointer(
        display: *mut c_void,
        window: c_ulong,
        root_return: *mut c_ulong,
        child_return: *mut c_ulong,
        root_x_return: *mut c_int,
        root_y_return: *mut c_int,
        window_x_return: *mut c_int,
        window_y_return: *mut c_int,
        mask_return: *mut c_uint,
    ) -> c_int;
    fn XGetImage(
        display: *mut c_void,
        window: c_ulong,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
        plane_mask: c_ulong,
        format: c_int,
    ) -> *mut c_void;
    fn XDestroyImage(image: *mut c_void) -> c_int;
    fn kill(pid: c_int, signal: c_int) -> c_int;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_receipts_preserve_each_supervised_boundary() {
        let receipts = parse_live_input_receipts("scroll=7\nshift_drag=91\nui_turn=311\n").unwrap();

        assert_eq!(
            receipts,
            LiveInputReceipts {
                scroll: 7,
                shift_drag: 91,
                ui_turn: 311,
            }
        );
    }

    #[test]
    fn linked_status_requires_truthful_exact_scale_for_every_panel() {
        let exact_s0_panel = (1 << 12) | (1 << 14);
        let exact_s0 = LinkedLodStatus::decode(
            exact_s0_panel | (exact_s0_panel << 16) | (exact_s0_panel << 32),
        );
        assert!(exact_s0.exact_level(0));

        let provisional_xy = LinkedLodStatus::decode(
            exact_s0_panel | (1 << 13) | (exact_s0_panel << 16) | (exact_s0_panel << 32),
        );
        assert!(!provisional_xy.exact_level(0));
    }

    #[test]
    fn diagnostic_motion_is_bounded_and_visibly_nonstationary() {
        let positions = (0..=120)
            .map(|sample| {
                let phase = std::f64::consts::TAU * sample as f64 / 120.0;
                bounded_pointer_position(300, 200, 20, 12, phase)
            })
            .collect::<std::collections::BTreeSet<_>>();

        assert!(positions.len() >= 80);
        assert!(
            positions
                .iter()
                .all(|(x, y)| { (280..=320).contains(x) && (188..=212).contains(y) })
        );
        assert!(positions.contains(&(300, 200)));
    }
}

unsafe extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
}
