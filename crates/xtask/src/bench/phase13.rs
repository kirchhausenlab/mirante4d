use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use crate::deps::cargo_metadata;
use anyhow::{Context, bail};
use mirante4d_data::DatasetHandle;
use mirante4d_domain::{
    CameraView, DisplayWindow, GridToWorld, IsoLightState, LayerTransfer, Opacity, RgbColor,
    Shape3D, TimeIndex, TransferCurve,
};
use mirante4d_format::{LayerDisplay, LayerId, LayerKind};
use mirante4d_render_api::CameraFrame;
use mirante4d_renderer::{
    CameraRenderMode, CameraRenderModeF32, CameraRenderQuality, DvrRenderParameters,
    IsoSurfaceFrameF32, IsoSurfaceFrameU16, IsoSurfaceNormal, IsoSurfaceParameters, PixelCoverage,
    RenderViewport, ScalarDisplayTransfer,
    gpu::{GpuRenderError, GpuRenderer, GpuRendererStats},
    render_camera_f32_from_bricks_with_quality,
    transfer::{
        IntensityTransfer, IsoSurfaceChannelFrame, IsoSurfaceChannelFrameF32,
        composite_iso_surface_channels, composite_iso_surface_f32_channels,
    },
};
use serde_json::{Value, json};

use super::phase11::Phase11GpuMipFrame;
use super::{
    Phase11InteractionBenchmarkOptions, Phase11LodPlan, Phase11LodPlanningInput,
    Phase11ResidentBrickSet, Phase11ResidentReadInput, phase11_interaction_report,
    phase11_read_resident_for_layer, phase11_select_lod_plan, phase11_stored_dtype_for_layer,
    phase11_viewport_matrix_for_shape, phase11_visible_bricks_at_scale,
};
use crate::host::{
    benchmark_baseline_class, benchmark_hardware_class, benchmark_host_context,
    benchmark_native_package_dataset_class, data_stats_json, gpu_stats_delta_json, gpu_stats_json,
    linux_process_peak_rss_kib,
};
use crate::package;
use crate::reports::{phase13_gpu_timings_json, timing_summary_json};
use crate::{
    BENCHMARK_PRESENTATION_POINTS, PHASE11_DEFAULT_MAX_RESPONSIVE_VISIBLE_BRICKS,
    PHASE11_GPU_MIP_BRICKS_PER_BATCH, benchmark_camera_for_shape, benchmark_camera_frame,
    benchmark_camera_orbit, benchmark_camera_pan, benchmark_camera_world_per_screen_point,
    benchmark_camera_zoom, phase11_benchmark_viewport_for_shape, phase11_brick_pixel_stride,
    phase11_gpu_brick_cache_budget_bytes, phase11_gpu_volume_cache_budget_bytes,
    phase11_max_decoded_bytes, phase11_max_visible_bricks, stable_id_from_name,
};

mod failure_policy;
mod refinement;
use refinement::*;

pub(crate) use failure_policy::{
    phase13_failure_policy_probe_report, phase13_gpu_error_kind, phase13_render_error_kind,
};

const PHASE13_CAPACITY_PROBE_BRICK_BUDGET_BYTES: u64 = 1;

pub(crate) fn bench_phase13_renderer(package: &Path) -> anyhow::Result<PathBuf> {
    let report_json = phase13_renderer_report(package, Phase13RendererBenchmarkOptions::default())?;
    write_phase13_benchmark_report(package, "bench-phase13-renderer", &report_json)
}

pub(crate) fn bench_phase13_viewport_matrix(package: &Path) -> anyhow::Result<PathBuf> {
    let report_json = phase13_viewport_matrix_report(package)?;
    write_phase13_benchmark_report(package, "bench-phase13-viewport-matrix", &report_json)
}

fn phase13_viewport_matrix_report(package: &Path) -> anyhow::Result<Value> {
    if !package.is_dir() {
        bail!(
            "native package path does not exist or is not a directory: {}",
            package.display()
        );
    }

    let dataset = DatasetHandle::open(package)?;
    let layer_id = dataset.first_layer_id()?;
    let dataset_class =
        benchmark_native_package_dataset_class(package, dataset.manifest().provenance.kind);
    let source_shape = dataset.scale_shape(&layer_id, 0)?;
    let scenarios = phase11_viewport_matrix_for_shape(source_shape)?;
    drop(dataset);

    let started = Instant::now();
    let mut reports = Vec::with_capacity(scenarios.len());
    for scenario in scenarios {
        let report = phase13_renderer_report(
            package,
            Phase13RendererBenchmarkOptions {
                viewport: Some(scenario.viewport),
            },
        )
        .with_context(|| {
            format!(
                "failed Phase 13 renderer report for viewport scenario {}",
                scenario.label
            )
        })?;
        reports.push(json!({
            "label": scenario.label,
            "viewport": {
                "width": scenario.viewport.width,
                "height": scenario.viewport.height,
                "physical_pixels": scenario.viewport.width.saturating_mul(scenario.viewport.height),
            },
            "report": report,
        }));
    }

    let total_ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(json!({
        "benchmark": "bench-phase13-viewport-matrix",
        "benchmark_schema_version": 1,
        "baseline_class": benchmark_baseline_class(),
        "hardware_class": benchmark_hardware_class(),
        "dataset_class": dataset_class,
        "evolved_from": {
            "benchmark": "bench-phase13-renderer",
            "benchmark_schema_version": 1,
        },
        "hardware": benchmark_host_context(),
        "package": package,
        "summary": {
            "scenario_count": reports.len(),
            "total_command_ms": total_ms,
        },
        "scenarios": reports,
    }))
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Phase13RendererBenchmarkOptions {
    viewport: Option<RenderViewport>,
}

pub(crate) fn phase13_renderer_report(
    package: &Path,
    options: Phase13RendererBenchmarkOptions,
) -> anyhow::Result<Value> {
    if !package.is_dir() {
        bail!(
            "native package path does not exist or is not a directory: {}",
            package.display()
        );
    }

    let started = Instant::now();
    let metadata = cargo_metadata()?;
    let app_version = package::package_version(&metadata, "mirante4d-app")?;
    let open_started = Instant::now();
    let dataset = DatasetHandle::open(package)?;
    let open_ms = open_started.elapsed().as_secs_f64() * 1000.0;
    let manifest = dataset.manifest();
    let dataset_class = benchmark_native_package_dataset_class(package, manifest.provenance.kind);

    let layer_id = dataset.first_layer_id()?;
    let layer = dataset
        .layer(&layer_id)
        .context("first layer id was not found after opening dataset")?;

    let timepoint = TimeIndex::new(0);
    let source_shape = dataset.scale_shape(&layer_id, 0)?;
    let source_grid_to_world = dataset.scale_grid_to_world(&layer_id, 0)?;
    let viewport = options
        .viewport
        .map(Ok)
        .unwrap_or_else(|| phase11_benchmark_viewport_for_shape(source_shape))?;
    let brick_pixel_stride = phase11_brick_pixel_stride(viewport)?;
    let max_visible_bricks = phase11_max_visible_bricks()?;
    let max_decoded_bytes = phase11_max_decoded_bytes()?;
    let camera_view = benchmark_camera_for_shape(source_shape, source_grid_to_world);
    let camera = benchmark_camera_frame(camera_view);

    let plan_started = Instant::now();
    let plan = phase11_select_lod_plan(
        &dataset,
        &layer_id,
        Phase11LodPlanningInput {
            camera_view,
            camera,
            viewport,
            brick_pixel_stride,
            max_visible_bricks,
            max_decoded_bytes,
        },
    )?;
    let plan_ms = plan_started.elapsed().as_secs_f64() * 1000.0;
    if plan.visible_bricks.is_empty() {
        bail!(
            "Phase 13 renderer benchmark camera produced no visible bricks for {}",
            package.display()
        );
    }

    let read_started = Instant::now();
    let resident = phase11_read_resident_for_layer(
        &dataset,
        &layer_id,
        Phase11ResidentReadInput {
            stored_dtype: layer.dtype.stored,
            scale_level: plan.displayed_scale_level,
            timepoint,
            volume_shape: plan.displayed_shape,
            grid_to_world: plan.displayed_grid_to_world,
        },
        &plan.visible_bricks,
    )?;
    let read_ms = read_started.elapsed().as_secs_f64() * 1000.0;

    let gpu_init_started = Instant::now();
    let gpu_renderer = GpuRenderer::new_with_cache_budgets_blocking(
        phase11_gpu_volume_cache_budget_bytes()?,
        phase11_gpu_brick_cache_budget_bytes()?,
    );
    let gpu_init_ms = gpu_init_started.elapsed().as_secs_f64() * 1000.0;
    let gpu_renderer = match gpu_renderer {
        Ok(renderer) => Some(renderer),
        Err(err) => {
            eprintln!("bench-phase13-renderer: GPU unavailable: {err}");
            None
        }
    };
    let gpu_adapter = gpu_renderer
        .as_ref()
        .map(|renderer| renderer.adapter_diagnostics().clone());

    let iso_threshold_policy = phase13_iso_threshold_policy(layer.display, &resident);
    let mode_cases = phase13_render_mode_cases(
        plan.displayed_shape,
        plan.displayed_grid_to_world,
        &iso_threshold_policy,
    );
    let mut mode_reports = Vec::with_capacity(mode_cases.len());
    let mut cpu_error_modes = 0_u64;
    let mut gpu_error_modes = 0_u64;
    let mut incomplete_modes = 0_u64;
    let mut nonblank_modes = 0_u64;
    let mut cpu_render_times = Vec::with_capacity(mode_cases.len());
    let mut gpu_render_times = Vec::new();

    for mode_case in &mode_cases {
        let report = phase13_render_mode_report(
            &resident,
            plan.brick_shape,
            plan.brick_grid_shape,
            camera,
            viewport,
            mode_case,
            gpu_renderer.as_ref(),
        );
        if report.cpu_render_ms.is_some() {
            cpu_render_times.extend(report.cpu_render_ms);
        }
        if let Some(render_ms) = report.gpu_render_ms {
            gpu_render_times.push(render_ms);
        }
        if report.cpu_error {
            cpu_error_modes += 1;
        }
        if report.gpu_error {
            gpu_error_modes += 1;
        }
        if report.incomplete {
            incomplete_modes += 1;
        }
        if report.nonblank {
            nonblank_modes += 1;
        }
        mode_reports.push(report.json);
    }
    let capacity_probe = phase13_capacity_probe_report(
        &resident,
        plan.brick_shape,
        plan.brick_grid_shape,
        camera,
        viewport,
        &mode_cases,
    )?;
    let failure_policy_probe = phase13_failure_policy_probe_report();
    let refinement_budget_probe = phase13_refinement_budget_probe_report(
        &dataset,
        &layer_id,
        Phase11LodPlanningInput {
            camera_view,
            camera,
            viewport,
            brick_pixel_stride,
            max_visible_bricks,
            max_decoded_bytes,
        },
        &plan,
    )?;
    let transition_cache_probe = phase13_transition_cache_probe_report(
        &dataset,
        &layer_id,
        &plan,
        camera_view,
        viewport,
        &mode_cases,
    )?;
    let iso_relighting_probe =
        phase13_iso_relighting_probe_report(&resident, camera, camera_view, viewport, &mode_cases)?;

    let interaction_started = Instant::now();
    let interaction_report = phase11_interaction_report(
        package,
        Phase11InteractionBenchmarkOptions {
            viewport: Some(viewport),
        },
    )?;
    let interaction_ms = interaction_started.elapsed().as_secs_f64() * 1000.0;
    let interaction_frames = interaction_report
        .get("frames")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let interaction_summary = interaction_report
        .get("summary")
        .cloned()
        .unwrap_or(Value::Null);
    let settled_refinement = interaction_report
        .get("settled_refinement")
        .cloned()
        .unwrap_or_else(|| {
            json!({
                "available": false,
                "reason": "missing_embedded_interaction_refinement_report",
            })
        });

    let stats = dataset.stats()?;
    let report_json = json!({
        "benchmark": "bench-phase13-renderer",
        "benchmark_schema_version": 1,
        "baseline_class": benchmark_baseline_class(),
        "hardware_class": benchmark_hardware_class(),
        "dataset_class": dataset_class,
        "evolved_from": {
            "benchmark": "bench-phase11-interaction",
            "benchmark_schema_version": 2,
        },
        "app_version": app_version,
        "native_format": {
            "format": manifest.format,
            "schema_version": manifest.schema_version,
        },
        "hardware": benchmark_host_context(),
        "package": package,
        "dataset": {
            "id": dataset.dataset_id().as_str(),
            "name": dataset.dataset_name(),
        },
        "layer": {
            "id": layer_id.to_string(),
            "stored_dtype": format!("{:?}", layer.dtype.stored),
        },
        "visible_layers": [layer_id.to_string()],
        "timepoint": timepoint.get(),
        "source_shape": {
            "z": source_shape.z(),
            "y": source_shape.y(),
            "x": source_shape.x(),
        },
        "viewport": {
            "width": viewport.width,
            "height": viewport.height,
            "physical_pixels": viewport.width.saturating_mul(viewport.height),
            "brick_pixel_stride": brick_pixel_stride,
        },
        "projection": format!("{:?}", camera_view.projection()),
        "lod": {
            "target_scale_level": plan.target_scale_level,
            "displayed_scale_level": plan.displayed_scale_level,
            "completeness": if incomplete_modes == 0 { "complete" } else { "incomplete" },
            "reason": plan.reason,
            "estimated_decoded_bytes": plan.estimated_decoded_bytes,
            "refinement_scale_level": plan.refinement_scale_level,
            "refinement_visible_bricks": plan.refinement_visible_bricks,
            "refinement_estimated_decoded_bytes": plan.refinement_estimated_decoded_bytes,
        },
        "resident_set": {
            "visible_bricks": plan.visible_bricks.len(),
            "brick_shape": shape3d_json(plan.brick_shape),
            "brick_grid_shape": shape3d_json(plan.brick_grid_shape),
            "displayed_shape": shape3d_json(plan.displayed_shape),
        },
        "policy": {
            "max_visible_bricks": max_visible_bricks,
            "max_decoded_bytes": max_decoded_bytes,
            "gpu_volume_cache_budget_bytes": phase11_gpu_volume_cache_budget_bytes()?,
            "gpu_brick_cache_budget_bytes": phase11_gpu_brick_cache_budget_bytes()?,
            "gpu_mip_bricks_per_batch": PHASE11_GPU_MIP_BRICKS_PER_BATCH,
        },
        "render_mode_defaults": {
            "iso_threshold": iso_threshold_policy.threshold,
            "iso_threshold_policy": iso_threshold_policy.json(),
        },
        "capacity_probe": capacity_probe,
        "failure_policy_probe": failure_policy_probe,
        "refinement_budget_probe": refinement_budget_probe,
        "transition_cache_probe": transition_cache_probe,
        "iso_relighting_probe": iso_relighting_probe,
        "gpu": {
            "available": gpu_renderer.is_some(),
            "init_ms": gpu_init_ms,
            "adapter": gpu_adapter.as_ref().map(|adapter| adapter.name.clone()),
            "backend": gpu_adapter.as_ref().map(|adapter| adapter.backend.clone()),
            "device_type": gpu_adapter.as_ref().map(|adapter| adapter.device_type.clone()),
            "driver": gpu_adapter.as_ref().map(|adapter| adapter.driver.clone()),
            "driver_info": gpu_adapter.as_ref().map(|adapter| adapter.driver_info.clone()),
            "final_stats": gpu_renderer.as_ref().and_then(|renderer| renderer.stats().ok()).map(gpu_stats_json),
        },
        "timings_ms": {
            "open_metadata": open_ms,
            "plan_lod_and_visible_bricks": plan_ms,
            "read_visible_bricks": read_ms,
            "cpu_render_modes": timing_summary_json(&cpu_render_times),
            "gpu_render_modes": timing_summary_json(&gpu_render_times),
            "interaction_timeline": interaction_ms,
            "total_command": started.elapsed().as_secs_f64() * 1000.0,
        },
        "summary": {
            "mode_count": mode_reports.len(),
            "cpu_error_modes": cpu_error_modes,
            "gpu_error_modes": gpu_error_modes,
            "incomplete_modes": incomplete_modes,
            "nonblank_modes": nonblank_modes,
            "peak_rss_kib": linux_process_peak_rss_kib(),
        },
        "timeline": {
            "interaction_source": {
                "benchmark": interaction_report.get("benchmark").and_then(Value::as_str),
                "benchmark_schema_version": interaction_report.get("benchmark_schema_version").and_then(Value::as_u64),
                "embedded_for": "phase13_renderer_schema",
            },
            "interaction_summary": interaction_summary,
            "interaction_frames": interaction_frames,
            "settled_frames": mode_reports,
            "settled_refinement": settled_refinement,
            "note": "Phase 13 embeds a Phase 11-derived interaction timeline plus one settled renderer frame per supported mode so interaction/refinement and mode-quality evidence live in one report."
        },
        "data_stats": data_stats_json(stats),
    });
    Ok(report_json)
}

fn write_phase13_benchmark_report(
    package: &Path,
    benchmark_name: &str,
    report_json: &Value,
) -> anyhow::Result<PathBuf> {
    let output_root = PathBuf::from("target/mirante4d/benchmarks");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let package_name = package
        .file_stem()
        .and_then(|name| name.to_str())
        .map(stable_id_from_name)
        .unwrap_or_else(|| "native-package".to_owned());
    let output_path = output_root.join(format!("{benchmark_name}-{package_name}.json"));
    fs::write(
        &output_path,
        format!("{}\n", serde_json::to_string_pretty(report_json)?),
    )
    .with_context(|| format!("failed to write {}", output_path.display()))?;
    Ok(output_path)
}

#[derive(Debug, Clone, Copy)]
struct Phase13RenderModeCase {
    label: &'static str,
    integer_mode: CameraRenderMode,
    f32_mode: CameraRenderModeF32,
    order_dependent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase13CacheExpectation {
    ReuseExistingAtlas,
    DistinctAtlasForIdentityChange,
}

impl Phase13CacheExpectation {
    fn label(self) -> &'static str {
        match self {
            Self::ReuseExistingAtlas => "reuse_existing_brick_atlas_without_uploads",
            Self::DistinctAtlasForIdentityChange => "identity_change_requires_distinct_brick_atlas",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Phase13GpuStatsDelta {
    brick_atlas_cache_hits: u64,
    brick_atlas_cache_misses: u64,
    brick_atlas_uploads: u64,
    brick_atlas_uploaded_bytes: u64,
}

#[derive(Debug)]
struct Phase13RenderModeReport {
    json: Value,
    cpu_render_ms: Option<f64>,
    gpu_render_ms: Option<f64>,
    cpu_error: bool,
    gpu_error: bool,
    incomplete: bool,
    nonblank: bool,
}

fn phase13_render_mode_cases(
    _volume_shape: Shape3D,
    _grid_to_world: GridToWorld,
    iso_threshold_policy: &Phase13IsoThresholdPolicy,
) -> Vec<Phase13RenderModeCase> {
    let iso_transfer = ScalarDisplayTransfer::new(
        iso_threshold_policy.display_window,
        TransferCurve::linear(),
        false,
    );
    let iso_parameters = IsoSurfaceParameters::new(
        iso_transfer.map_source_value(f32::from(iso_threshold_policy.threshold)),
        iso_transfer,
    );
    let f32_iso_parameters = IsoSurfaceParameters::new(
        iso_transfer.map_source_value(iso_threshold_policy.threshold_source_value as f32),
        iso_transfer,
    );
    vec![
        Phase13RenderModeCase {
            label: "mip",
            integer_mode: CameraRenderMode::Mip,
            f32_mode: CameraRenderModeF32::Mip,
            order_dependent: false,
        },
        Phase13RenderModeCase {
            label: "dvr",
            integer_mode: CameraRenderMode::Dvr {
                parameters: phase13_dvr_parameters_u16(),
            },
            f32_mode: CameraRenderModeF32::Dvr {
                parameters: phase13_dvr_parameters_f32(),
            },
            order_dependent: true,
        },
        Phase13RenderModeCase {
            label: "iso",
            integer_mode: CameraRenderMode::Isosurface {
                parameters: iso_parameters,
            },
            f32_mode: CameraRenderModeF32::Isosurface {
                parameters: f32_iso_parameters,
            },
            order_dependent: true,
        },
    ]
}

fn phase13_mip_mode_case() -> Phase13RenderModeCase {
    Phase13RenderModeCase {
        label: "mip",
        integer_mode: CameraRenderMode::Mip,
        f32_mode: CameraRenderModeF32::Mip,
        order_dependent: false,
    }
}

fn phase13_dvr_parameters_u16() -> DvrRenderParameters {
    phase13_dvr_parameters(ScalarDisplayTransfer::identity_u16())
}

fn phase13_dvr_parameters_f32() -> DvrRenderParameters {
    phase13_dvr_parameters(ScalarDisplayTransfer::identity_f32())
}

fn phase13_dvr_parameters(transfer: ScalarDisplayTransfer) -> DvrRenderParameters {
    DvrRenderParameters::new(transfer, transfer, [1.0, 1.0, 1.0, 1.0], 1.0, 12.0)
}

#[derive(Debug, Clone)]
struct Phase13IsoThresholdPolicy {
    threshold: u16,
    threshold_source_value: f64,
    source: &'static str,
    display_window: DisplayWindow,
    resident_min: Option<f64>,
    resident_max: Option<f64>,
    occupied_bricks: usize,
}

impl Phase13IsoThresholdPolicy {
    fn json(&self) -> Value {
        json!({
            "threshold": self.threshold,
            "threshold_source_value": self.threshold_source_value,
            "source": self.source,
            "display_window": {
                "low": self.display_window.low(),
                "high": self.display_window.high(),
            },
            "resident_signal": {
                "min": self.resident_min,
                "max": self.resident_max,
                "occupied_bricks": self.occupied_bricks,
            },
        })
    }
}

fn phase13_iso_threshold_policy(
    display: LayerDisplay,
    resident: &Phase11ResidentBrickSet,
) -> Phase13IsoThresholdPolicy {
    let (resident_min, resident_max, occupied_bricks) = resident.positive_signal_range();
    let (threshold_source_value, source) =
        phase13_iso_threshold_source_value_from_range(display, resident_min, resident_max);
    let threshold =
        phase13_iso_threshold_from_value(threshold_source_value, resident_max.unwrap_or(1.0));
    Phase13IsoThresholdPolicy {
        threshold,
        threshold_source_value,
        source,
        display_window: display.window(),
        resident_min,
        resident_max,
        occupied_bricks,
    }
}

#[cfg(test)]
fn phase13_iso_threshold_from_range(
    display: LayerDisplay,
    resident_min: Option<f64>,
    resident_max: Option<f64>,
) -> (u16, &'static str) {
    let (value, source) =
        phase13_iso_threshold_source_value_from_range(display, resident_min, resident_max);
    (
        phase13_iso_threshold_from_value(value, resident_max.unwrap_or(1.0)),
        source,
    )
}

fn phase13_iso_threshold_source_value_from_range(
    display: LayerDisplay,
    _resident_min: Option<f64>,
    resident_max: Option<f64>,
) -> (f64, &'static str) {
    let Some(max_value) =
        resident_max.filter(|max_value| max_value.is_finite() && *max_value > 0.0)
    else {
        return (1.0, "no_positive_resident_signal_default");
    };

    let low = f64::from(display.window().low());
    let high = f64::from(display.window().high());
    let midpoint = (low + high) * 0.5;
    let display_window_matches_resident_data = low.is_finite()
        && high.is_finite()
        && high > low
        && low < max_value
        && midpoint > 0.0
        && high <= max_value * 1.25;
    if display_window_matches_resident_data {
        return (midpoint, "display_window_midpoint");
    }

    (max_value * 0.5, "resident_max_midpoint")
}

fn phase13_iso_threshold_from_value(value: f64, resident_max: f64) -> u16 {
    value
        .round()
        .clamp(1.0, resident_max.min(f64::from(u16::MAX))) as u16
}

fn phase13_render_mode_report(
    resident: &Phase11ResidentBrickSet,
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
    camera: CameraFrame,
    viewport: RenderViewport,
    mode_case: &Phase13RenderModeCase,
    gpu_renderer: Option<&GpuRenderer>,
) -> Phase13RenderModeReport {
    let cpu_started = Instant::now();
    let cpu = phase13_render_cpu_mode(resident, camera, viewport, mode_case);
    let cpu_render_ms = cpu_started.elapsed().as_secs_f64() * 1000.0;
    let (cpu_json, cpu_error, cpu_incomplete, cpu_nonblank) = match cpu {
        Ok(frame) => (
            json!({
                "available": true,
                "ok": true,
                "timings_ms": {
                    "render": cpu_render_ms,
                },
                "frame": frame.json,
            }),
            false,
            !frame.complete,
            frame.nonzero_pixels > 0,
        ),
        Err(err) => (
            json!({
                "available": true,
                "ok": false,
                "timings_ms": {
                    "render": cpu_render_ms,
                },
                "error_kind": phase13_render_error_kind(&err),
                "error": err.to_string(),
            }),
            true,
            true,
            false,
        ),
    };

    let gpu = phase13_gpu_render_mode_report(
        resident,
        brick_shape,
        brick_grid_shape,
        camera,
        viewport,
        mode_case,
        gpu_renderer,
    );

    Phase13RenderModeReport {
        json: json!({
            "render_mode": mode_case.label,
            "stored_dtype": resident.stored_dtype_label(),
            "order_dependent": mode_case.order_dependent,
            "cpu": cpu_json,
            "gpu": gpu.json,
        }),
        cpu_render_ms: (!cpu_error).then_some(cpu_render_ms),
        gpu_render_ms: gpu.render_ms,
        cpu_error,
        gpu_error: gpu.error,
        incomplete: cpu_incomplete || gpu.incomplete,
        nonblank: cpu_nonblank || gpu.nonblank,
    }
}

#[derive(Debug)]
struct Phase13CpuModeFrame {
    json: Value,
    complete: bool,
    nonzero_pixels: u64,
}

fn phase13_render_cpu_mode(
    resident: &Phase11ResidentBrickSet,
    camera: CameraFrame,
    viewport: RenderViewport,
    mode_case: &Phase13RenderModeCase,
) -> Result<Phase13CpuModeFrame, mirante4d_renderer::RenderError> {
    match resident {
        Phase11ResidentBrickSet::U8(_) | Phase11ResidentBrickSet::U16(_) => {
            let (_image, diagnostics) =
                resident.render_cpu(camera, viewport, mode_case.integer_mode)?;
            Ok(Phase13CpuModeFrame {
                json: brick_frame_json(diagnostics),
                complete: diagnostics.complete,
                nonzero_pixels: diagnostics.frame.nonzero_pixels,
            })
        }
        Phase11ResidentBrickSet::F32(resident) => {
            let (_image, diagnostics) = render_camera_f32_from_bricks_with_quality(
                resident,
                camera,
                viewport,
                mode_case.f32_mode,
                CameraRenderQuality::voxel_exact(),
            )?;
            Ok(Phase13CpuModeFrame {
                json: brick_frame_f32_json(diagnostics),
                complete: diagnostics.complete,
                nonzero_pixels: diagnostics.frame.nonzero_pixels,
            })
        }
    }
}

fn phase13_iso_relighting_probe_report(
    resident: &Phase11ResidentBrickSet,
    camera: CameraFrame,
    camera_view: CameraView,
    viewport: RenderViewport,
    mode_cases: &[Phase13RenderModeCase],
) -> anyhow::Result<Value> {
    let iso_mode = mode_cases
        .iter()
        .find(|mode_case| matches!(mode_case.integer_mode, CameraRenderMode::Isosurface { .. }))
        .context("Phase 13 renderer benchmark did not define an ISO render mode")?;

    let package_surface =
        phase13_package_iso_relighting_case(resident, camera, camera_view, viewport, iso_mode)?;
    let synthetic_one =
        phase13_synthetic_iso_relighting_case("synthetic_one_channel", viewport, camera_view, 1)?;
    let synthetic_multi =
        phase13_synthetic_iso_relighting_case("synthetic_multi_channel", viewport, camera_view, 3)?;

    Ok(json!({
        "purpose": "cached_iso_surface_light_drag_relighting",
        "coordinate_space": "surface normals and light vectors are compared in world space",
        "measured_operation": "typed ISO surface compositing on an existing cached package surface; synthetic probes remain U16 fixtures",
        "package_cached_surface": package_surface,
        "synthetic_one_channel": synthetic_one,
        "synthetic_multi_channel": synthetic_multi,
        "light_drag_work": {
            "data_engine_brick_reads": 0,
            "raymarches": 0,
            "lod_changes": 0,
            "surface_frame_reuse": true,
        },
    }))
}

fn phase13_package_iso_relighting_case(
    resident: &Phase11ResidentBrickSet,
    camera: CameraFrame,
    camera_view: CameraView,
    viewport: RenderViewport,
    iso_mode: &Phase13RenderModeCase,
) -> anyhow::Result<Value> {
    match resident {
        Phase11ResidentBrickSet::U8(_) | Phase11ResidentBrickSet::U16(_) => {
            let render_started = Instant::now();
            let (image, diagnostics) =
                resident.render_cpu(camera, viewport, iso_mode.integer_mode)?;
            let initial_iso_render_ms = render_started.elapsed().as_secs_f64() * 1000.0;
            let surface = image
                .iso_surface()
                .context("Phase 13 ISO render did not return a cached integer surface frame")?;
            let relight = phase13_relight_cached_surfaces(
                "package_cached_surface",
                viewport,
                camera_view,
                &[surface],
                Some(initial_iso_render_ms),
            )?;

            Ok(json!({
                "source": "package_cpu_iso_surface_frame",
                "stored_dtype": resident.stored_dtype_label(),
                "initial_iso_render": {
                    "timings_ms": {
                        "render": initial_iso_render_ms,
                    },
                    "frame": brick_frame_json(diagnostics),
                },
                "relight": relight,
            }))
        }
        Phase11ResidentBrickSet::F32(resident) => {
            let render_started = Instant::now();
            let (image, diagnostics) = render_camera_f32_from_bricks_with_quality(
                resident,
                camera,
                viewport,
                iso_mode.f32_mode,
                CameraRenderQuality::voxel_exact(),
            )?;
            let initial_iso_render_ms = render_started.elapsed().as_secs_f64() * 1000.0;
            let surface = image
                .iso_surface()
                .context("Phase 13 ISO render did not return a cached Float32 surface frame")?;
            let relight = phase13_relight_cached_f32_surfaces(
                "package_cached_surface",
                viewport,
                camera_view,
                &[surface],
                Some(initial_iso_render_ms),
            )?;

            Ok(json!({
                "source": "package_cpu_iso_surface_frame",
                "stored_dtype": "Float32",
                "initial_iso_render": {
                    "timings_ms": {
                        "render": initial_iso_render_ms,
                    },
                    "frame": brick_frame_f32_json(diagnostics),
                },
                "relight": relight,
            }))
        }
    }
}

fn phase13_synthetic_iso_relighting_case(
    label: &'static str,
    viewport: RenderViewport,
    camera_view: CameraView,
    channel_count: usize,
) -> anyhow::Result<Value> {
    let pixel_count = phase13_viewport_pixel_count(viewport)?;
    let normal = IsoSurfaceNormal::from_unit_components(0.0, 0.0, 1.0);
    let mut surfaces = Vec::with_capacity(channel_count);
    for channel_index in 0..channel_count {
        surfaces.push(IsoSurfaceFrameU16::try_new(
            viewport.width,
            viewport.height,
            vec![u16::MAX; pixel_count],
            vec![u16::MAX; pixel_count],
            vec![u16::MAX / 2; pixel_count],
            vec![channel_index as f32; pixel_count],
            vec![normal; pixel_count],
            vec![0; pixel_count],
            vec![0; pixel_count],
            PixelCoverage::All,
        )?);
    }
    let surface_refs = surfaces.iter().collect::<Vec<_>>();
    phase13_relight_cached_surfaces(label, viewport, camera_view, &surface_refs, None)
}

fn phase13_relight_cached_surfaces(
    label: &'static str,
    viewport: RenderViewport,
    camera_view: CameraView,
    surfaces: &[&IsoSurfaceFrameU16],
    initial_iso_render_ms: Option<f64>,
) -> anyhow::Result<Value> {
    let pixel_count = phase13_viewport_pixel_count(viewport)?;
    let transfer = IntensityTransfer::new(
        true,
        LayerTransfer::new(
            DisplayWindow::new(0.0, 1.0)?,
            RgbColor::new([1.0, 1.0, 1.0])?,
            Opacity::new(1.0)?,
            TransferCurve::linear(),
            false,
        ),
    );
    let channels = surfaces
        .iter()
        .map(|surface| IsoSurfaceChannelFrame::new(surface, transfer))
        .collect::<Vec<_>>();
    let light_states = [
        IsoLightState::attached_camera(),
        IsoLightState::detached_screen(0.8, 0.0)?,
        IsoLightState::detached_screen(-0.35, 0.55)?,
    ];
    let axes = benchmark_camera_frame(camera_view).axes();
    let covered_surface_pixels = surfaces
        .iter()
        .map(|surface| surface.coverage().covered_count(pixel_count))
        .sum::<usize>();

    let relight_started = Instant::now();
    let mut relit_nonblank_pixels = 0_u64;
    for light_state in light_states {
        let image = composite_iso_surface_channels(&channels, light_state, axes)?;
        relit_nonblank_pixels += phase13_display_nonblank_pixels(image.pixels());
    }
    let relight_total_ms = relight_started.elapsed().as_secs_f64() * 1000.0;
    let iterations = light_states.len() as f64;

    Ok(json!({
        "label": label,
        "channel_count": surfaces.len(),
        "viewport": {
            "width": viewport.width,
            "height": viewport.height,
            "physical_pixels": viewport.width.saturating_mul(viewport.height),
        },
        "cached_surface_pixels": pixel_count,
        "covered_surface_pixels": covered_surface_pixels,
        "light_state_iterations": light_states.len(),
        "initial_iso_render_ms": initial_iso_render_ms,
        "relit_nonblank_pixels": relit_nonblank_pixels,
        "timings_ms": {
            "relight_total": relight_total_ms,
            "relight_per_light_state": relight_total_ms / iterations,
        },
        "during_relight": {
            "data_engine_brick_reads": 0,
            "raymarches": 0,
            "lod_changes": 0,
            "surface_frame_reuse": true,
        },
    }))
}

fn phase13_relight_cached_f32_surfaces(
    label: &'static str,
    viewport: RenderViewport,
    camera_view: CameraView,
    surfaces: &[&IsoSurfaceFrameF32],
    initial_iso_render_ms: Option<f64>,
) -> anyhow::Result<Value> {
    let pixel_count = phase13_viewport_pixel_count(viewport)?;
    let transfer = IntensityTransfer::new(
        true,
        LayerTransfer::new(
            DisplayWindow::new(0.0, 1.0)?,
            RgbColor::new([1.0, 1.0, 1.0])?,
            Opacity::new(1.0)?,
            TransferCurve::linear(),
            false,
        ),
    );
    let channels = surfaces
        .iter()
        .map(|surface| IsoSurfaceChannelFrameF32::new(surface, transfer))
        .collect::<Vec<_>>();
    let light_states = [
        IsoLightState::attached_camera(),
        IsoLightState::detached_screen(0.8, 0.0)?,
        IsoLightState::detached_screen(-0.35, 0.55)?,
    ];
    let axes = benchmark_camera_frame(camera_view).axes();
    let covered_surface_pixels = surfaces
        .iter()
        .map(|surface| surface.coverage().covered_count(pixel_count))
        .sum::<usize>();

    let relight_started = Instant::now();
    let mut relit_nonblank_pixels = 0_u64;
    for light_state in light_states {
        let image = composite_iso_surface_f32_channels(&channels, light_state, axes)?;
        relit_nonblank_pixels += phase13_display_nonblank_pixels(image.pixels());
    }
    let relight_total_ms = relight_started.elapsed().as_secs_f64() * 1000.0;
    let iterations = light_states.len() as f64;

    Ok(json!({
        "label": label,
        "channel_count": surfaces.len(),
        "viewport": {
            "width": viewport.width,
            "height": viewport.height,
            "physical_pixels": viewport.width.saturating_mul(viewport.height),
        },
        "cached_surface_pixels": pixel_count,
        "covered_surface_pixels": covered_surface_pixels,
        "light_state_iterations": light_states.len(),
        "initial_iso_render_ms": initial_iso_render_ms,
        "relit_nonblank_pixels": relit_nonblank_pixels,
        "timings_ms": {
            "relight_total": relight_total_ms,
            "relight_per_light_state": relight_total_ms / iterations,
        },
        "during_relight": {
            "data_engine_brick_reads": 0,
            "raymarches": 0,
            "lod_changes": 0,
            "surface_frame_reuse": true,
        },
    }))
}

fn phase13_viewport_pixel_count(viewport: RenderViewport) -> anyhow::Result<usize> {
    viewport
        .width
        .checked_mul(viewport.height)
        .and_then(|pixels| usize::try_from(pixels).ok())
        .context("Phase 13 ISO relighting viewport pixel count overflowed usize")
}

fn phase13_display_nonblank_pixels(pixels: &[u8]) -> u64 {
    pixels
        .chunks_exact(4)
        .filter(|rgba| rgba[3] != 0 || rgba[0] != 0 || rgba[1] != 0 || rgba[2] != 0)
        .count() as u64
}

#[derive(Debug)]
struct Phase13GpuModeReport {
    json: Value,
    render_ms: Option<f64>,
    error: bool,
    incomplete: bool,
    nonblank: bool,
}

fn phase13_gpu_render_mode_report(
    resident: &Phase11ResidentBrickSet,
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
    camera: CameraFrame,
    viewport: RenderViewport,
    mode_case: &Phase13RenderModeCase,
    gpu_renderer: Option<&GpuRenderer>,
) -> Phase13GpuModeReport {
    let Some(renderer) = gpu_renderer else {
        return Phase13GpuModeReport {
            json: json!({
                "available": false,
                "ok": false,
                "error": "no usable GPU renderer",
            }),
            render_ms: None,
            error: true,
            incomplete: true,
            nonblank: false,
        };
    };

    let stats_before = renderer.stats().ok();
    let started = Instant::now();
    let (output, mip_batched, monolithic_error) =
        if matches!(mode_case.integer_mode, CameraRenderMode::Mip) {
            resident.render_gpu_mip_with_u16_batched_fallback(
                renderer,
                brick_shape,
                brick_grid_shape,
                camera,
                viewport,
            )
        } else {
            (
                phase13_render_gpu_direct(
                    resident,
                    renderer,
                    brick_shape,
                    brick_grid_shape,
                    camera,
                    viewport,
                    mode_case,
                ),
                false,
                None,
            )
        };
    let render_ms = started.elapsed().as_secs_f64() * 1000.0;
    let stats_after = renderer.stats().ok();
    let resource_delta = gpu_stats_delta_json(stats_before, stats_after);

    match output {
        Ok(output) => {
            let upload_ms = output.upload_ms();
            let incomplete = output.complete().map(|complete| !complete).unwrap_or(false);
            let nonblank = output.nonzero_pixels() > 0;
            Phase13GpuModeReport {
                json: json!({
                    "available": true,
                    "ok": true,
                    "stored_dtype": resident.stored_dtype_label(),
                    "mip_batched": mip_batched,
                    "monolithic_error": monolithic_error,
                    "timings_ms": phase13_gpu_timings_json(render_ms, upload_ms),
                    "upload_timing_source": "renderer_atlas_update_wall_time",
                    "resource_delta": resource_delta,
                    "frame": output.frame_json(),
                    "stats": stats_after.map(gpu_stats_json),
                }),
                render_ms: Some(render_ms),
                error: false,
                incomplete,
                nonblank,
            }
        }
        Err(err) => Phase13GpuModeReport {
            json: json!({
                "available": true,
                "ok": false,
                "stored_dtype": resident.stored_dtype_label(),
                "timings_ms": phase13_gpu_timings_json(render_ms, None),
                "upload_timing_source": "unavailable_on_render_error",
                "resource_delta": resource_delta,
                "error_kind": phase13_gpu_error_kind(&err),
                "error": err.to_string(),
                "stats": stats_after.map(gpu_stats_json),
            }),
            render_ms: Some(render_ms),
            error: true,
            incomplete: true,
            nonblank: false,
        },
    }
}

fn phase13_render_gpu_direct(
    resident: &Phase11ResidentBrickSet,
    renderer: &GpuRenderer,
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
    camera: CameraFrame,
    viewport: RenderViewport,
    mode_case: &Phase13RenderModeCase,
) -> Result<Phase11GpuMipFrame, GpuRenderError> {
    match resident {
        Phase11ResidentBrickSet::U8(_) | Phase11ResidentBrickSet::U16(_) => resident
            .render_gpu_direct(
                renderer,
                brick_shape,
                brick_grid_shape,
                camera,
                viewport,
                mode_case.integer_mode,
            )
            .map(Phase11GpuMipFrame::Integer),
        Phase11ResidentBrickSet::F32(resident) => renderer
            .render_camera_f32_from_bricks(
                resident,
                brick_shape,
                brick_grid_shape,
                camera,
                viewport,
                mode_case.f32_mode,
            )
            .map(Phase11GpuMipFrame::F32),
    }
}

fn phase13_capacity_probe_report(
    resident: &Phase11ResidentBrickSet,
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
    camera: CameraFrame,
    viewport: RenderViewport,
    mode_cases: &[Phase13RenderModeCase],
) -> anyhow::Result<Value> {
    let init_started = Instant::now();
    let renderer = GpuRenderer::new_with_cache_budgets_blocking(
        phase11_gpu_volume_cache_budget_bytes()?,
        PHASE13_CAPACITY_PROBE_BRICK_BUDGET_BYTES,
    );
    let init_ms = init_started.elapsed().as_secs_f64() * 1000.0;
    let renderer = match renderer {
        Ok(renderer) => renderer,
        Err(err) => {
            return Ok(json!({
                "available": false,
                "ok": false,
                "init_ms": init_ms,
                "probe_brick_cache_budget_bytes": PHASE13_CAPACITY_PROBE_BRICK_BUDGET_BYTES,
                "error_kind": phase13_gpu_error_kind(&err),
                "error": err.to_string(),
            }));
        }
    };

    let mut mode_reports = Vec::with_capacity(mode_cases.len());
    let mut budget_exceeded_modes = 0_u64;
    let mut unexpected_success_modes = 0_u64;
    let mut order_dependent_modes = 0_u64;
    let mut order_dependent_budget_exceeded_modes = 0_u64;
    for mode_case in mode_cases {
        if mode_case.order_dependent {
            order_dependent_modes += 1;
        }
        let started = Instant::now();
        let result = phase13_render_gpu_direct(
            resident,
            &renderer,
            brick_shape,
            brick_grid_shape,
            camera,
            viewport,
            mode_case,
        );
        let render_ms = started.elapsed().as_secs_f64() * 1000.0;
        match result {
            Ok(output) => {
                unexpected_success_modes += 1;
                mode_reports.push(phase13_capacity_probe_success_json(
                    mode_case,
                    render_ms,
                    output.nonzero_pixels(),
                ));
            }
            Err(err) => {
                let error_kind = phase13_gpu_error_kind(&err);
                if error_kind == "budget_exceeded" {
                    budget_exceeded_modes += 1;
                    if mode_case.order_dependent {
                        order_dependent_budget_exceeded_modes += 1;
                    }
                }
                mode_reports.push(phase13_capacity_probe_error_json(
                    mode_case, render_ms, &err,
                ));
            }
        }
    }

    Ok(json!({
        "available": true,
        "ok": unexpected_success_modes == 0 && budget_exceeded_modes == mode_reports.len() as u64,
        "purpose": "intentional_tiny_gpu_brick_budget_capacity_probe",
        "stored_dtype": resident.stored_dtype_label(),
        "direct_render_only": true,
        "batched_fallback_attempted": false,
        "probe_brick_cache_budget_bytes": PHASE13_CAPACITY_PROBE_BRICK_BUDGET_BYTES,
        "init_ms": init_ms,
        "summary": {
            "modes_tested": mode_reports.len(),
            "budget_exceeded_modes": budget_exceeded_modes,
            "unexpected_success_modes": unexpected_success_modes,
            "order_dependent_modes": order_dependent_modes,
            "order_dependent_budget_exceeded_modes": order_dependent_budget_exceeded_modes,
        },
        "modes": mode_reports,
    }))
}

fn phase13_transition_cache_probe_report(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    plan: &Phase11LodPlan,
    camera_view: CameraView,
    viewport: RenderViewport,
    mode_cases: &[Phase13RenderModeCase],
) -> anyhow::Result<Value> {
    let started = Instant::now();
    let renderer = match GpuRenderer::new_with_cache_budgets_blocking(
        phase11_gpu_volume_cache_budget_bytes()?,
        phase11_gpu_brick_cache_budget_bytes()?,
    ) {
        Ok(renderer) => renderer,
        Err(err) => {
            return Ok(json!({
                "available": false,
                "ok": false,
                "error": err.to_string(),
            }));
        }
    };
    let init_ms = started.elapsed().as_secs_f64() * 1000.0;
    let base_resident = phase13_read_resident_for_plan(dataset, layer_id, plan, TimeIndex::new(0))?;
    let base_camera = benchmark_camera_frame(camera_view);
    let mip_mode = phase13_mip_mode_case();

    let baseline = phase13_transition_render_probe_item(Phase13TransitionProbeRender {
        label: "baseline_warm_mip",
        transition_kind: "baseline",
        resident_identity: "initial render populates the controlled probe atlas",
        renderer: &renderer,
        resident: &base_resident,
        brick_shape: plan.brick_shape,
        brick_grid_shape: plan.brick_grid_shape,
        camera: base_camera,
        viewport,
        mode_case: mip_mode,
        cache_expectation: None,
    });

    let mut transitions = Vec::new();
    let orbit_camera = benchmark_camera_orbit(camera_view, 0.22, 0.10);
    transitions.push(phase13_transition_render_probe_item(
        Phase13TransitionProbeRender {
            label: "camera_orbit_same_identity",
            transition_kind: "camera",
            resident_identity: "dataset/layer/timepoint/scale/brick set unchanged",
            renderer: &renderer,
            resident: &base_resident,
            brick_shape: plan.brick_shape,
            brick_grid_shape: plan.brick_grid_shape,
            camera: benchmark_camera_frame(orbit_camera),
            viewport,
            mode_case: mip_mode,
            cache_expectation: Some(Phase13CacheExpectation::ReuseExistingAtlas),
        },
    ));

    let pan_distance =
        benchmark_camera_world_per_screen_point(camera_view) * BENCHMARK_PRESENTATION_POINTS * 0.05;
    let pan_camera = benchmark_camera_pan(camera_view, pan_distance, -pan_distance * 0.5);
    transitions.push(phase13_transition_render_probe_item(
        Phase13TransitionProbeRender {
            label: "camera_pan_same_identity",
            transition_kind: "camera",
            resident_identity: "dataset/layer/timepoint/scale/brick set unchanged",
            renderer: &renderer,
            resident: &base_resident,
            brick_shape: plan.brick_shape,
            brick_grid_shape: plan.brick_grid_shape,
            camera: benchmark_camera_frame(pan_camera),
            viewport,
            mode_case: mip_mode,
            cache_expectation: Some(Phase13CacheExpectation::ReuseExistingAtlas),
        },
    ));

    let zoom_camera = benchmark_camera_zoom(camera_view, 0.75);
    transitions.push(phase13_transition_render_probe_item(
        Phase13TransitionProbeRender {
            label: "camera_zoom_same_identity",
            transition_kind: "camera",
            resident_identity: "dataset/layer/timepoint/scale/brick set unchanged",
            renderer: &renderer,
            resident: &base_resident,
            brick_shape: plan.brick_shape,
            brick_grid_shape: plan.brick_grid_shape,
            camera: benchmark_camera_frame(zoom_camera),
            viewport,
            mode_case: mip_mode,
            cache_expectation: Some(Phase13CacheExpectation::ReuseExistingAtlas),
        },
    ));

    for mode_case in mode_cases {
        transitions.push(phase13_transition_render_probe_item(
            Phase13TransitionProbeRender {
                label: &format!("render_mode_{}_same_identity", mode_case.label),
                transition_kind: "render_mode",
                resident_identity: "dataset/layer/timepoint/scale/brick set unchanged",
                renderer: &renderer,
                resident: &base_resident,
                brick_shape: plan.brick_shape,
                brick_grid_shape: plan.brick_grid_shape,
                camera: base_camera,
                viewport,
                mode_case: *mode_case,
                cache_expectation: Some(Phase13CacheExpectation::ReuseExistingAtlas),
            },
        ));
    }

    transitions.push(
        match phase13_scale_transition_resident(dataset, layer_id, plan, base_camera, viewport)? {
            Some(transition) => {
                let identity = format!(
                    "scale changed from s{} to s{}; atlas key must not be reused as the same LOD",
                    plan.displayed_scale_level, transition.scale_level
                );
                phase13_transition_render_probe_item(Phase13TransitionProbeRender {
                    label: "scale_neighbor_same_camera",
                    transition_kind: "scale",
                    resident_identity: &identity,
                    renderer: &renderer,
                    resident: &transition.resident,
                    brick_shape: transition.brick_shape,
                    brick_grid_shape: transition.brick_grid_shape,
                    camera: base_camera,
                    viewport,
                    mode_case: mip_mode,
                    cache_expectation: Some(
                        Phase13CacheExpectation::DistinctAtlasForIdentityChange,
                    ),
                })
            }
            None => phase13_transition_unavailable_json(
                "scale_neighbor_same_camera",
                "scale",
                "package has no neighboring scale that fits the transition probe budgets",
            ),
        },
    );

    transitions.push(
        match phase13_timepoint_transition_resident(dataset, layer_id, plan)? {
            Some(resident) => phase13_transition_render_probe_item(Phase13TransitionProbeRender {
                label: "timepoint_next_same_bricks",
                transition_kind: "timepoint",
                resident_identity: "timepoint changed; atlas key must not be reused as the same scientific frame",
                renderer: &renderer,
                resident: &resident,
                brick_shape: plan.brick_shape,
                brick_grid_shape: plan.brick_grid_shape,
                camera: base_camera,
                viewport,
                mode_case: mip_mode,
                cache_expectation: Some(Phase13CacheExpectation::DistinctAtlasForIdentityChange),
            }),
            None => phase13_transition_unavailable_json(
                "timepoint_next_same_bricks",
                "timepoint",
                "package has only one timepoint for the benchmark layer",
            ),
        },
    );

    transitions.push(match phase13_channel_transition_resident(dataset, layer_id, plan)? {
        Some((transition_layer_id, resident)) => {
            let identity = format!(
                "layer changed from {} to {}; atlas key must not be reused as the same scientific channel",
                layer_id, transition_layer_id
            );
            phase13_transition_render_probe_item(Phase13TransitionProbeRender {
                label: "channel_next_same_bricks",
                transition_kind: "channel",
                resident_identity: &identity,
                renderer: &renderer,
                resident: &resident,
                brick_shape: plan.brick_shape,
                brick_grid_shape: plan.brick_grid_shape,
                camera: base_camera,
                viewport,
                mode_case: mip_mode,
                cache_expectation: Some(Phase13CacheExpectation::DistinctAtlasForIdentityChange),
            })
        }
        None => phase13_transition_unavailable_json(
            "channel_next_same_bricks",
            "channel",
            "package has no second dense intensity layer compatible with the displayed scale and brick grid",
        ),
    });

    let mut available_transitions = 0_u64;
    let mut ok_transitions = 0_u64;
    let mut expectation_checked = 0_u64;
    let mut expectation_met = 0_u64;
    let mut reuse_transitions = 0_u64;
    let mut reuse_expectation_met = 0_u64;
    let mut identity_change_transitions = 0_u64;
    let mut identity_change_expectation_met = 0_u64;
    let mut unavailable_transitions = 0_u64;

    for transition in &transitions {
        if transition
            .get("available")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            available_transitions += 1;
        } else {
            unavailable_transitions += 1;
            continue;
        }
        if transition
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            ok_transitions += 1;
        }
        let Some(expectation) = transition.get("cache_expectation").and_then(Value::as_str) else {
            continue;
        };
        let met = transition
            .get("cache_expectation_met")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        expectation_checked += 1;
        if met {
            expectation_met += 1;
        }
        match expectation {
            "reuse_existing_brick_atlas_without_uploads" => {
                reuse_transitions += 1;
                if met {
                    reuse_expectation_met += 1;
                }
            }
            "identity_change_requires_distinct_brick_atlas" => {
                identity_change_transitions += 1;
                if met {
                    identity_change_expectation_met += 1;
                }
            }
            _ => {}
        }
    }

    Ok(json!({
        "available": true,
        "ok": expectation_checked == expectation_met && available_transitions == ok_transitions,
        "purpose": "controlled GPU brick-atlas transition cache probe",
        "init_ms": init_ms,
        "baseline": baseline,
        "summary": {
            "transitions_total": transitions.len(),
            "transitions_available": available_transitions,
            "transitions_unavailable": unavailable_transitions,
            "transitions_ok": ok_transitions,
            "cache_expectations_checked": expectation_checked,
            "cache_expectations_met": expectation_met,
            "reuse_transitions": reuse_transitions,
            "reuse_expectations_met": reuse_expectation_met,
            "identity_change_transitions": identity_change_transitions,
            "identity_change_expectations_met": identity_change_expectation_met,
        },
        "transitions": transitions,
        "final_stats": renderer.stats().ok().map(gpu_stats_json),
    }))
}

fn phase13_read_resident_for_plan(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    plan: &Phase11LodPlan,
    timepoint: TimeIndex,
) -> anyhow::Result<Phase11ResidentBrickSet> {
    phase11_read_resident_for_layer(
        dataset,
        layer_id,
        Phase11ResidentReadInput {
            stored_dtype: phase11_stored_dtype_for_layer(dataset, layer_id)?,
            scale_level: plan.displayed_scale_level,
            timepoint,
            volume_shape: plan.displayed_shape,
            grid_to_world: plan.displayed_grid_to_world,
        },
        &plan.visible_bricks,
    )
}

fn phase13_timepoint_transition_resident(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    plan: &Phase11LodPlan,
) -> anyhow::Result<Option<Phase11ResidentBrickSet>> {
    let Some(layer) = dataset.layer(layer_id) else {
        return Ok(None);
    };
    if layer.shape.t() <= 1 {
        return Ok(None);
    }
    phase13_read_resident_for_plan(dataset, layer_id, plan, TimeIndex::new(1)).map(Some)
}

fn phase13_channel_transition_resident(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    plan: &Phase11LodPlan,
) -> anyhow::Result<Option<(LayerId, Phase11ResidentBrickSet)>> {
    for candidate in &dataset.manifest().layers {
        if candidate.id == layer_id.as_str()
            || candidate.kind != LayerKind::DenseIntensity
            || candidate.shape.t() == 0
        {
            continue;
        }
        let candidate_id = LayerId::new(candidate.id.clone())?;
        let Ok(candidate_shape) = dataset.scale_shape(&candidate_id, plan.displayed_scale_level)
        else {
            continue;
        };
        if candidate_shape != plan.displayed_shape {
            continue;
        }
        let Ok(candidate_brick_shape) =
            dataset.brick_shape_at_scale(&candidate_id, plan.displayed_scale_level)
        else {
            continue;
        };
        let Ok(candidate_brick_grid_shape) =
            dataset.brick_grid_shape_at_scale(&candidate_id, plan.displayed_scale_level)
        else {
            continue;
        };
        if candidate_brick_shape != plan.brick_shape
            || candidate_brick_grid_shape != plan.brick_grid_shape
        {
            continue;
        }
        let candidate_grid_to_world =
            dataset.scale_grid_to_world(&candidate_id, plan.displayed_scale_level)?;
        let resident = phase11_read_resident_for_layer(
            dataset,
            &candidate_id,
            Phase11ResidentReadInput {
                stored_dtype: candidate.dtype.stored,
                scale_level: plan.displayed_scale_level,
                timepoint: TimeIndex::new(0),
                volume_shape: candidate_shape,
                grid_to_world: candidate_grid_to_world,
            },
            &plan.visible_bricks,
        )?;
        return Ok(Some((candidate_id, resident)));
    }
    Ok(None)
}

#[derive(Debug)]
struct Phase13ScaleTransitionResident {
    scale_level: u32,
    resident: Phase11ResidentBrickSet,
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
}

fn phase13_scale_transition_resident(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    plan: &Phase11LodPlan,
    camera: CameraFrame,
    viewport: RenderViewport,
) -> anyhow::Result<Option<Phase13ScaleTransitionResident>> {
    let scale_count = dataset.scale_count(layer_id)?;
    let mut candidate_scales = Vec::new();
    if (plan.displayed_scale_level as usize + 1) < scale_count {
        candidate_scales.push(plan.displayed_scale_level + 1);
    }
    if plan.displayed_scale_level > 0 {
        candidate_scales.push(plan.displayed_scale_level - 1);
    }

    let brick_pixel_stride = phase11_brick_pixel_stride(viewport)?;
    let max_visible_bricks = phase11_max_visible_bricks()?;
    let max_decoded_bytes = phase11_max_decoded_bytes()?;
    for scale_level in candidate_scales {
        let scale_plan = phase11_visible_bricks_at_scale(
            dataset,
            layer_id,
            camera,
            viewport,
            brick_pixel_stride,
            scale_level,
        )?;
        if scale_plan.visible_bricks.is_empty()
            || scale_plan.visible_bricks.len() > max_visible_bricks
            || scale_plan.estimated_decoded_bytes > max_decoded_bytes
        {
            continue;
        }
        let resident = phase11_read_resident_for_layer(
            dataset,
            layer_id,
            Phase11ResidentReadInput {
                stored_dtype: phase11_stored_dtype_for_layer(dataset, layer_id)?,
                scale_level,
                timepoint: TimeIndex::new(0),
                volume_shape: scale_plan.displayed_shape,
                grid_to_world: scale_plan.displayed_grid_to_world,
            },
            &scale_plan.visible_bricks,
        )?;
        return Ok(Some(Phase13ScaleTransitionResident {
            scale_level,
            resident,
            brick_shape: scale_plan.brick_shape,
            brick_grid_shape: scale_plan.brick_grid_shape,
        }));
    }
    Ok(None)
}

struct Phase13TransitionProbeRender<'a> {
    label: &'a str,
    transition_kind: &'a str,
    resident_identity: &'a str,
    renderer: &'a GpuRenderer,
    resident: &'a Phase11ResidentBrickSet,
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
    camera: CameraFrame,
    viewport: RenderViewport,
    mode_case: Phase13RenderModeCase,
    cache_expectation: Option<Phase13CacheExpectation>,
}

fn phase13_transition_render_probe_item(input: Phase13TransitionProbeRender<'_>) -> Value {
    let stats_before = input.renderer.stats().ok();
    let started = Instant::now();
    let output = phase13_render_gpu_direct(
        input.resident,
        input.renderer,
        input.brick_shape,
        input.brick_grid_shape,
        input.camera,
        input.viewport,
        &input.mode_case,
    );
    let render_ms = started.elapsed().as_secs_f64() * 1000.0;
    let stats_after = input.renderer.stats().ok();
    let stats_delta = phase13_gpu_stats_delta(stats_before, stats_after);
    let resource_delta = gpu_stats_delta_json(stats_before, stats_after);
    let cache_expectation_met = input.cache_expectation.map(|expectation| {
        output.is_ok() && phase13_cache_expectation_met(expectation, stats_delta)
    });

    match output {
        Ok(output) => json!({
            "label": input.label,
            "transition_kind": input.transition_kind,
            "resident_identity": input.resident_identity,
            "available": true,
            "ok": true,
            "stored_dtype": input.resident.stored_dtype_label(),
            "render_mode": input.mode_case.label,
            "cache_expectation": input.cache_expectation.map(Phase13CacheExpectation::label),
            "cache_expectation_met": cache_expectation_met,
            "timings_ms": phase13_gpu_timings_json(
                render_ms,
                output.upload_ms(),
            ),
            "upload_timing_source": "renderer_atlas_update_wall_time",
            "resource_delta": resource_delta,
            "frame": output.frame_json(),
        }),
        Err(err) => json!({
            "label": input.label,
            "transition_kind": input.transition_kind,
            "resident_identity": input.resident_identity,
            "available": true,
            "ok": false,
            "stored_dtype": input.resident.stored_dtype_label(),
            "render_mode": input.mode_case.label,
            "cache_expectation": input.cache_expectation.map(Phase13CacheExpectation::label),
            "cache_expectation_met": false,
            "timings_ms": phase13_gpu_timings_json(render_ms, None),
            "upload_timing_source": "unavailable_on_render_error",
            "resource_delta": resource_delta,
            "error_kind": phase13_gpu_error_kind(&err),
            "error": err.to_string(),
        }),
    }
}

fn phase13_transition_unavailable_json(label: &str, transition_kind: &str, reason: &str) -> Value {
    json!({
        "label": label,
        "transition_kind": transition_kind,
        "available": false,
        "ok": false,
        "reason": reason,
    })
}

fn phase13_gpu_stats_delta(
    before: Option<GpuRendererStats>,
    after: Option<GpuRendererStats>,
) -> Option<Phase13GpuStatsDelta> {
    let before = before?;
    let after = after?;
    Some(Phase13GpuStatsDelta {
        brick_atlas_cache_hits: after
            .brick_atlas_cache_hits
            .saturating_sub(before.brick_atlas_cache_hits),
        brick_atlas_cache_misses: after
            .brick_atlas_cache_misses
            .saturating_sub(before.brick_atlas_cache_misses),
        brick_atlas_uploads: after
            .brick_atlas_uploads
            .saturating_sub(before.brick_atlas_uploads),
        brick_atlas_uploaded_bytes: after
            .brick_atlas_uploaded_bytes
            .saturating_sub(before.brick_atlas_uploaded_bytes),
    })
}

fn phase13_cache_expectation_met(
    expectation: Phase13CacheExpectation,
    delta: Option<Phase13GpuStatsDelta>,
) -> bool {
    let Some(delta) = delta else {
        return false;
    };
    match expectation {
        Phase13CacheExpectation::ReuseExistingAtlas => {
            delta.brick_atlas_cache_hits >= 1
                && delta.brick_atlas_cache_misses == 0
                && delta.brick_atlas_uploads == 0
                && delta.brick_atlas_uploaded_bytes == 0
        }
        Phase13CacheExpectation::DistinctAtlasForIdentityChange => {
            delta.brick_atlas_cache_misses >= 1
                && delta.brick_atlas_uploads >= 1
                && delta.brick_atlas_uploaded_bytes > 0
        }
    }
}

fn phase13_capacity_probe_success_json(
    mode_case: &Phase13RenderModeCase,
    render_ms: f64,
    nonzero_pixels: u64,
) -> Value {
    json!({
        "render_mode": mode_case.label,
        "order_dependent": mode_case.order_dependent,
        "ok": true,
        "unexpected_success": true,
        "timings_ms": {
            "render": render_ms,
        },
        "nonzero_pixels": nonzero_pixels,
        "expected_policy": "capacity probe should produce a typed budget_exceeded failure under the intentionally tiny GPU brick budget",
    })
}

fn phase13_capacity_probe_error_json(
    mode_case: &Phase13RenderModeCase,
    render_ms: f64,
    err: &GpuRenderError,
) -> Value {
    let error_kind = phase13_gpu_error_kind(err);
    json!({
        "render_mode": mode_case.label,
        "order_dependent": mode_case.order_dependent,
        "ok": false,
        "error_kind": error_kind,
        "error": err.to_string(),
        "timings_ms": {
            "render": render_ms,
        },
        "expected_policy": if mode_case.order_dependent {
            "order-dependent modes must downgrade or fail visibly; capacity probe does not attempt batch fallback"
        } else {
            "capacity failure must remain typed and visible"
        },
        "batched_fallback_attempted": false,
    })
}

pub(crate) fn brick_frame_json(diagnostics: mirante4d_renderer::BrickFrameDiagnostics) -> Value {
    json!({
        "input_voxels": diagnostics.frame.input_voxels,
        "output_pixels": diagnostics.frame.output_pixels,
        "nonzero_pixels": diagnostics.frame.nonzero_pixels,
        "max_value": diagnostics.frame.max_value,
        "complete": diagnostics.complete,
        "missing_voxel_samples": diagnostics.missing_voxel_samples,
        "skip": brick_skip_json(diagnostics.skip),
    })
}

fn brick_frame_f32_json(diagnostics: mirante4d_renderer::BrickFrameDiagnosticsF32) -> Value {
    json!({
        "input_voxels": diagnostics.frame.input_voxels,
        "output_pixels": diagnostics.frame.output_pixels,
        "nonzero_pixels": diagnostics.frame.nonzero_pixels,
        "max_value": diagnostics.frame.max_value,
        "complete": diagnostics.complete,
        "missing_voxel_samples": diagnostics.missing_voxel_samples,
        "skip": Value::Null,
    })
}

pub(crate) fn brick_skip_json(diagnostics: mirante4d_renderer::BrickSkipDiagnostics) -> Value {
    json!({
        "skipped_brick_intervals": diagnostics.skipped_brick_intervals,
        "empty_brick_intervals": diagnostics.empty_brick_intervals,
        "mip_range_intervals": diagnostics.mip_range_intervals,
        "iso_range_intervals": diagnostics.iso_range_intervals,
        "dvr_range_intervals": diagnostics.dvr_range_intervals,
    })
}

pub(crate) fn shape3d_json(shape: Shape3D) -> Value {
    json!({
        "z": shape.z(),
        "y": shape.y(),
        "x": shape.x(),
    })
}

#[cfg(test)]
mod tests;
