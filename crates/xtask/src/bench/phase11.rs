use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, bail};
use glam::DVec3;
use mirante4d_core::{
    CameraView, DEFAULT_PRESENTATION_VIEWPORT_POINTS, GridToWorld, IntensityDType, LayerId,
    Projection, Shape3D, Shape4D, TimeIndex, WorldSpace, WorldUnit,
};
use mirante4d_data::{DatasetHandle, SpatialBrickIndex};
use mirante4d_format::{
    ChannelMetadata, DenseU16MultiscaleLayer, DenseU16Scale, ExistingPackagePolicy,
    NativeU16MultiscaleDataset, ScaleReduction, default_u16_display,
    write_native_u16_multiscale_dataset,
};
use mirante4d_renderer::{
    BrickFrameDiagnostics, BrickFrameDiagnosticsF32, BrickGridSpec, BrickPlanOptions,
    CameraRenderMode, CameraRenderModeF32, CameraRenderQuality, MipImageU16, RenderViewport,
    ResidentBrickSetF32, ResidentBrickSetU8, ResidentBrickSetU16,
    gpu::{GpuMipOutput, GpuMipOutputF32, GpuRenderError, GpuRenderer},
    plan_visible_bricks, render_camera_f32_from_bricks_with_quality, render_camera_from_bricks,
    render_camera_u8_from_bricks_with_quality,
};
use serde_json::{Value, json};

use crate::fixtures::generate_fixture;
use crate::host::{
    benchmark_baseline_class, benchmark_hardware_class, benchmark_host_context,
    benchmark_native_package_dataset_class, data_stats_json, gpu_stats_delta_json, gpu_stats_json,
    linux_process_peak_rss_kib,
};
use crate::reports::{phase11_gpu_interaction_timings_json, timing_summary_json};
use crate::{
    PHASE11_DEFAULT_MAX_RESPONSIVE_VISIBLE_BRICKS, PHASE11_GPU_MIP_BRICKS_PER_BATCH,
    benchmark_camera_for_shape, env_u64, phase11_benchmark_viewport_for_shape,
    phase11_brick_pixel_stride, phase11_gpu_brick_cache_budget_bytes,
    phase11_gpu_volume_cache_budget_bytes, phase11_interaction_steps_per_scenario,
    phase11_max_decoded_bytes, phase11_max_visible_bricks, stable_id_from_name,
};

use super::phase13::brick_skip_json;

mod sparse_fixture;
use sparse_fixture::*;

pub(crate) fn bench_phase11_large_view(package: &Path) -> anyhow::Result<PathBuf> {
    if !package.is_dir() {
        bail!(
            "native package path does not exist or is not a directory: {}",
            package.display()
        );
    }

    let started = Instant::now();
    let open_started = Instant::now();
    let dataset = DatasetHandle::open(package)?;
    let open_ms = open_started.elapsed().as_secs_f64() * 1000.0;

    let layer_id = dataset.first_layer_id()?;
    let dataset_class =
        benchmark_native_package_dataset_class(package, dataset.manifest().provenance.kind);
    let layer = dataset
        .layer(&layer_id)
        .context("first layer id was not found after opening dataset")?;
    let timepoint = TimeIndex(0);
    let source_shape = dataset.scale_shape(&layer_id, 0)?;
    let source_grid_to_world = dataset.scale_grid_to_world(&layer_id, 0)?;
    let viewport = phase11_benchmark_viewport_for_shape(source_shape)?;
    let viewport_relative_stride = (viewport.width.min(viewport.height) / 32).clamp(1, 16);
    let brick_pixel_stride = phase11_brick_pixel_stride(viewport)?
        .min(viewport_relative_stride)
        .max(1);
    let max_visible_bricks = phase11_max_visible_bricks()?;
    let max_decoded_bytes = phase11_max_decoded_bytes()?;
    let camera_view = benchmark_camera_for_shape(source_shape, source_grid_to_world);
    let camera = camera_view.to_camera_state(DEFAULT_PRESENTATION_VIEWPORT_POINTS);

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
            "benchmark camera produced no visible bricks for {}",
            package.display()
        );
    }

    let pre_stream_stats = dataset.stats()?;
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
    let read_visible_ms = read_started.elapsed().as_secs_f64() * 1000.0;

    let resident_render_started = Instant::now();
    let resident_frame = resident.render_cpu_mip_summary(camera, viewport)?;
    let resident_render_ms = resident_render_started.elapsed().as_secs_f64() * 1000.0;

    let gpu_benchmark = bench_phase11_gpu_resident(
        &resident,
        plan.brick_shape,
        plan.brick_grid_shape,
        camera,
        viewport,
    )?;
    let stats = dataset.stats()?;
    let total_ms = started.elapsed().as_secs_f64() * 1000.0;

    let output_root = PathBuf::from("target/mirante4d/benchmarks");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let package_name = package
        .file_stem()
        .and_then(|name| name.to_str())
        .map(stable_id_from_name)
        .unwrap_or_else(|| "native-package".to_owned());
    let output_path = output_root.join(format!("bench-phase11-large-view-{package_name}.json"));
    let report_json = json!({
        "benchmark": "bench-phase11-large-view",
        "benchmark_schema_version": 1,
        "baseline_class": benchmark_baseline_class(),
        "hardware_class": benchmark_hardware_class(),
        "dataset_class": dataset_class,
        "hardware": benchmark_host_context(),
        "package": package,
        "layer_id": layer_id.to_string(),
        "stored_dtype": resident.stored_dtype_label(),
        "timepoint": timepoint.0,
        "source_shape": {
            "z": source_shape.z,
            "y": source_shape.y,
            "x": source_shape.x,
        },
        "viewport": {
            "width": viewport.width,
            "height": viewport.height,
            "brick_pixel_stride": brick_pixel_stride,
        },
        "lod": {
            "target_scale_level": plan.target_scale_level,
            "displayed_scale_level": plan.displayed_scale_level,
            "reason": plan.reason,
            "max_visible_bricks": max_visible_bricks,
            "max_decoded_bytes": max_decoded_bytes,
            "estimated_decoded_bytes": plan.estimated_decoded_bytes,
        },
        "displayed_shape": {
            "z": plan.displayed_shape.z,
            "y": plan.displayed_shape.y,
            "x": plan.displayed_shape.x,
        },
        "brick_shape": {
            "z": plan.brick_shape.z,
            "y": plan.brick_shape.y,
            "x": plan.brick_shape.x,
        },
        "brick_grid_shape": {
            "z": plan.brick_grid_shape.z,
            "y": plan.brick_grid_shape.y,
            "x": plan.brick_grid_shape.x,
        },
        "visible_bricks": plan.visible_bricks.len(),
        "resident_complete": resident_frame.complete,
        "timings_ms": {
            "open_metadata": open_ms,
            "plan_lod_and_visible_bricks": plan_ms,
            "read_visible_bricks": read_visible_ms,
            "cpu_resident_brick_mip": resident_render_ms,
            "total": total_ms,
        },
        "gpu": gpu_benchmark,
        "resident_frame": resident_frame.json(),
        "pre_stream_data_stats": data_stats_json(pre_stream_stats),
        "data_stats": data_stats_json(stats),
    });
    fs::write(
        &output_path,
        format!("{}\n", serde_json::to_string_pretty(&report_json)?),
    )
    .with_context(|| format!("failed to write {}", output_path.display()))?;
    Ok(output_path)
}

pub(crate) fn bench_phase11_interaction(package: &Path) -> anyhow::Result<PathBuf> {
    let report_json =
        phase11_interaction_report(package, Phase11InteractionBenchmarkOptions::default())?;
    write_phase11_benchmark_report(package, "bench-phase11-interaction", &report_json)
}

pub(crate) fn bench_phase11_viewport_matrix(package: &Path) -> anyhow::Result<PathBuf> {
    let report_json = phase11_viewport_matrix_report(package)?;
    write_phase11_benchmark_report(package, "bench-phase11-viewport-matrix", &report_json)
}

fn phase11_viewport_matrix_report(package: &Path) -> anyhow::Result<Value> {
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
        let report = phase11_interaction_report(
            package,
            Phase11InteractionBenchmarkOptions {
                viewport: Some(scenario.viewport),
            },
        )
        .with_context(|| {
            format!(
                "failed Phase 11 interaction report for viewport scenario {}",
                scenario.label
            )
        })?;
        reports.push(json!({
            "label": scenario.label,
            "viewport": {
                "width": scenario.viewport.width,
                "height": scenario.viewport.height,
            },
            "report": report,
        }));
    }

    let total_ms = started.elapsed().as_secs_f64() * 1000.0;
    let report_json = json!({
        "benchmark": "bench-phase11-viewport-matrix",
        "benchmark_schema_version": 1,
        "baseline_class": benchmark_baseline_class(),
        "hardware_class": benchmark_hardware_class(),
        "dataset_class": dataset_class,
        "hardware": benchmark_host_context(),
        "package": package,
        "summary": {
            "scenario_count": reports.len(),
            "total_command_ms": total_ms,
        },
        "scenarios": reports,
    });
    Ok(report_json)
}

pub(crate) fn bench_phase11_synthetic_matrix() -> anyhow::Result<PathBuf> {
    let output_root = PathBuf::from("target")
        .join("mirante4d")
        .join("benchmarks")
        .join("phase11-synthetic");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;

    let fixtures = [
        Phase11SyntheticFixture {
            label: "time_multichannel_8cube_3t_2c",
            package: generate_fixture("time-multichannel-u16-8cube-3t-2c")?,
            notes: "strict native fixture with separate channel layers and three timepoints",
            metadata: json!({
                "fixture_kind": "time-multichannel-u16-8cube-3t-2c",
                "shape": { "t": 3, "z": 8, "y": 8, "x": 8 },
                "layers": 2,
                "channel_axis": false,
            }),
        },
        Phase11SyntheticFixture {
            label: "large_sparse_empty_bricks_multiscale",
            package: write_phase11_sparse_empty_package(&output_root)?,
            notes: "128^3 source-scale package with three multiscale levels and mostly empty 16^3 bricks",
            metadata: phase11_sparse_empty_fixture_metadata()?,
        },
    ];

    let started = Instant::now();
    let mut reports = Vec::with_capacity(fixtures.len());
    for fixture in fixtures {
        let report = phase11_viewport_matrix_report(&fixture.package).with_context(|| {
            format!(
                "failed Phase 11 viewport matrix for synthetic fixture {} at {}",
                fixture.label,
                fixture.package.display()
            )
        })?;
        reports.push(json!({
            "label": fixture.label,
            "package": fixture.package,
            "notes": fixture.notes,
            "metadata": fixture.metadata,
            "report": report,
        }));
    }

    let report_json = json!({
        "benchmark": "bench-phase11-synthetic-matrix",
        "benchmark_schema_version": 1,
        "baseline_class": benchmark_baseline_class(),
        "hardware_class": benchmark_hardware_class(),
        "dataset_class": "synthetic_fixture_matrix",
        "hardware": benchmark_host_context(),
        "summary": {
            "fixture_count": reports.len(),
            "total_command_ms": started.elapsed().as_secs_f64() * 1000.0,
        },
        "fixtures": reports,
    });
    let output_path = output_root.join("bench-phase11-synthetic-matrix.json");
    fs::write(
        &output_path,
        format!("{}\n", serde_json::to_string_pretty(&report_json)?),
    )
    .with_context(|| format!("failed to write {}", output_path.display()))?;
    Ok(output_path)
}

#[derive(Debug)]
struct Phase11SyntheticFixture {
    label: &'static str,
    package: PathBuf,
    notes: &'static str,
    metadata: Value,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Phase11InteractionBenchmarkOptions {
    pub(crate) viewport: Option<RenderViewport>,
}

pub(crate) fn phase11_interaction_report(
    package: &Path,
    options: Phase11InteractionBenchmarkOptions,
) -> anyhow::Result<Value> {
    if !package.is_dir() {
        bail!(
            "native package path does not exist or is not a directory: {}",
            package.display()
        );
    }

    let started = Instant::now();
    let open_started = Instant::now();
    let dataset = DatasetHandle::open(package)?;
    let open_ms = open_started.elapsed().as_secs_f64() * 1000.0;

    let layer_id = dataset.first_layer_id()?;
    let dataset_class =
        benchmark_native_package_dataset_class(package, dataset.manifest().provenance.kind);
    let layer = dataset
        .layer(&layer_id)
        .context("first layer id was not found after opening dataset")?;
    let timepoint = TimeIndex(0);
    let source_shape = dataset.scale_shape(&layer_id, 0)?;
    let source_grid_to_world = dataset.scale_grid_to_world(&layer_id, 0)?;
    let viewport = options
        .viewport
        .map(Ok)
        .unwrap_or_else(|| phase11_benchmark_viewport_for_shape(source_shape))?;
    let brick_pixel_stride = phase11_brick_pixel_stride(viewport)?;
    let max_visible_bricks = phase11_max_visible_bricks()?;
    let max_decoded_bytes = phase11_max_decoded_bytes()?;
    let steps_per_scenario = phase11_interaction_steps_per_scenario()?;
    let base_camera = benchmark_camera_for_shape(source_shape, source_grid_to_world);
    let camera_sequence = phase11_interaction_camera_sequence(base_camera, steps_per_scenario);

    let gpu_init_started = Instant::now();
    let gpu_renderer = GpuRenderer::new_with_cache_budgets_blocking(
        phase11_gpu_volume_cache_budget_bytes()?,
        phase11_gpu_brick_cache_budget_bytes()?,
    );
    let gpu_init_ms = gpu_init_started.elapsed().as_secs_f64() * 1000.0;
    let gpu_renderer = match gpu_renderer {
        Ok(renderer) => Some(renderer),
        Err(err) => {
            eprintln!("bench-phase11-interaction: GPU unavailable: {err}");
            None
        }
    };
    let gpu_adapter = gpu_renderer
        .as_ref()
        .map(|renderer| renderer.adapter_diagnostics().clone());
    let pre_interaction_stats = dataset.stats()?;

    let mut frame_reports = Vec::with_capacity(camera_sequence.len());
    let mut total_frame_times = Vec::with_capacity(camera_sequence.len());
    let mut plan_times = Vec::with_capacity(camera_sequence.len());
    let mut read_times = Vec::with_capacity(camera_sequence.len());
    let mut gpu_times = Vec::new();
    let mut gpu_upload_times = Vec::new();
    let mut reason_counts = BTreeMap::<String, u64>::new();
    let mut displayed_scale_counts = BTreeMap::<String, u64>::new();
    let mut target_scale_counts = BTreeMap::<String, u64>::new();
    let mut refinement_scale_counts = BTreeMap::<String, u64>::new();
    let mut budget_limited_frames = 0_u64;
    let mut gpu_error_frames = 0_u64;
    let mut gpu_batched_frames = 0_u64;
    let mut gpu_incomplete_frames = 0_u64;
    let mut gpu_missing_voxel_sample_frames = 0_u64;
    let mut gpu_max_missing_voxel_samples = 0_u64;
    let mut final_plan = None;
    let mut final_camera = None;

    for (frame_index, camera_sample) in camera_sequence.iter().enumerate() {
        let frame_started = Instant::now();
        let camera = camera_sample
            .camera
            .to_camera_state(DEFAULT_PRESENTATION_VIEWPORT_POINTS);
        let plan_started = Instant::now();
        let plan = phase11_select_lod_plan(
            &dataset,
            &layer_id,
            Phase11LodPlanningInput {
                camera_view: camera_sample.camera,
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
                "interaction benchmark camera produced no visible bricks for {} frame {}",
                package.display(),
                frame_index
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

        let gpu_frame = if let Some(renderer) = &gpu_renderer {
            let frame = phase11_render_gpu_interaction_frame(
                renderer,
                &resident,
                plan.brick_shape,
                plan.brick_grid_shape,
                camera,
                viewport,
            );
            if let Some(render_ms) = frame.render_ms {
                gpu_times.push(render_ms);
            }
            if let Some(upload_ms) = frame.upload_ms {
                gpu_upload_times.push(upload_ms);
            }
            if frame.batched {
                gpu_batched_frames += 1;
            }
            if !frame.ok {
                gpu_error_frames += 1;
            }
            if matches!(frame.complete, Some(false)) {
                gpu_incomplete_frames += 1;
            }
            if let Some(missing_voxel_samples) = frame.missing_voxel_samples
                && missing_voxel_samples > 0
            {
                gpu_missing_voxel_sample_frames += 1;
                gpu_max_missing_voxel_samples =
                    gpu_max_missing_voxel_samples.max(missing_voxel_samples);
            }
            frame.report
        } else {
            gpu_error_frames += 1;
            json!({
                "available": false,
                "error": "no usable GPU renderer",
            })
        };

        let frame_ms = frame_started.elapsed().as_secs_f64() * 1000.0;
        total_frame_times.push(frame_ms);
        plan_times.push(plan_ms);
        read_times.push(read_ms);
        *reason_counts.entry(plan.reason.to_owned()).or_default() += 1;
        *displayed_scale_counts
            .entry(format!("s{}", plan.displayed_scale_level))
            .or_default() += 1;
        *target_scale_counts
            .entry(format!("s{}", plan.target_scale_level))
            .or_default() += 1;
        if let Some(refinement_scale_level) = plan.refinement_scale_level {
            *refinement_scale_counts
                .entry(format!("s{}", refinement_scale_level))
                .or_default() += 1;
        }
        if matches!(
            plan.reason,
            "visible_brick_budget_limited" | "decoded_byte_budget_limited"
        ) {
            budget_limited_frames += 1;
        }
        if frame_index + 1 == camera_sequence.len() {
            final_plan = Some(plan.clone());
            final_camera = Some(camera);
        }

        frame_reports.push(json!({
            "frame_index": frame_index,
            "scenario": camera_sample.scenario,
            "scenario_frame": camera_sample.scenario_frame,
            "camera": phase11_camera_json(camera_sample.camera),
            "lod": {
                "target_scale_level": plan.target_scale_level,
                "displayed_scale_level": plan.displayed_scale_level,
                "reason": plan.reason,
                "estimated_decoded_bytes": plan.estimated_decoded_bytes,
                "refinement_scale_level": plan.refinement_scale_level,
                "refinement_visible_bricks": plan.refinement_visible_bricks,
                "refinement_estimated_decoded_bytes": plan.refinement_estimated_decoded_bytes,
            },
            "displayed_shape": {
                "z": plan.displayed_shape.z,
                "y": plan.displayed_shape.y,
                "x": plan.displayed_shape.x,
            },
            "visible_bricks": plan.visible_bricks.len(),
            "timings_ms": {
                "plan_lod_and_visible_bricks": plan_ms,
                "read_visible_bricks": read_ms,
                "total_interaction_frame": frame_ms,
            },
            "gpu": gpu_frame,
        }));
    }

    let settled_refinement = match (final_plan.as_ref(), final_camera) {
        (Some(plan), Some(camera)) => phase11_settled_refinement_report(
            &dataset,
            &layer_id,
            timepoint,
            plan,
            gpu_renderer.as_ref(),
            camera,
            viewport,
        )?,
        _ => json!({
            "available": false,
            "reason": "no_final_interaction_plan",
        }),
    };
    let stats = dataset.stats()?;
    let total_ms = started.elapsed().as_secs_f64() * 1000.0;
    let report_json = json!({
        "benchmark": "bench-phase11-interaction",
        "benchmark_schema_version": 2,
        "baseline_class": benchmark_baseline_class(),
        "hardware_class": benchmark_hardware_class(),
        "dataset_class": dataset_class,
        "hardware": benchmark_host_context(),
        "package": package,
        "layer_id": layer_id.to_string(),
        "stored_dtype": format!("{:?}", layer.dtype.stored),
        "timepoint": timepoint.0,
        "source_shape": {
            "z": source_shape.z,
            "y": source_shape.y,
            "x": source_shape.x,
        },
        "viewport": {
            "width": viewport.width,
            "height": viewport.height,
            "brick_pixel_stride": brick_pixel_stride,
        },
        "policy": {
            "steps_per_scenario": steps_per_scenario,
            "max_visible_bricks": max_visible_bricks,
            "max_decoded_bytes": max_decoded_bytes,
            "gpu_mip_bricks_per_batch": PHASE11_GPU_MIP_BRICKS_PER_BATCH,
        },
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
        "summary": {
            "frames": frame_reports.len(),
            "timings_ms": {
                "open_metadata": open_ms,
                "total_command": total_ms,
                "interaction_frame": timing_summary_json(&total_frame_times),
                "plan_lod_and_visible_bricks": timing_summary_json(&plan_times),
                "read_visible_bricks": timing_summary_json(&read_times),
                "gpu_resident_mip": timing_summary_json(&gpu_times),
                "gpu_resident_mip_upload": timing_summary_json(&gpu_upload_times),
            },
            "reason_counts": reason_counts,
            "displayed_scale_counts": displayed_scale_counts,
            "target_scale_counts": target_scale_counts,
            "refinement_scale_counts": refinement_scale_counts,
            "budget_limited_frames": budget_limited_frames,
            "gpu_error_frames": gpu_error_frames,
            "gpu_batched_frames": gpu_batched_frames,
            "gpu_incomplete_frames": gpu_incomplete_frames,
            "gpu_missing_voxel_sample_frames": gpu_missing_voxel_sample_frames,
            "gpu_max_missing_voxel_samples": gpu_max_missing_voxel_samples,
            "peak_rss_kib": linux_process_peak_rss_kib(),
        },
        "frames": frame_reports,
        "settled_refinement": settled_refinement,
        "pre_interaction_data_stats": data_stats_json(pre_interaction_stats),
        "data_stats": data_stats_json(stats),
    });
    Ok(report_json)
}

fn write_phase11_benchmark_report(
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

#[derive(Debug, Clone)]
pub(crate) struct Phase11LodPlan {
    pub(crate) target_scale_level: u32,
    pub(crate) displayed_scale_level: u32,
    pub(crate) reason: &'static str,
    pub(crate) displayed_shape: Shape3D,
    pub(crate) displayed_grid_to_world: GridToWorld,
    pub(crate) brick_shape: Shape3D,
    pub(crate) brick_grid_shape: Shape3D,
    pub(crate) visible_bricks: Vec<SpatialBrickIndex>,
    pub(crate) estimated_decoded_bytes: u64,
    pub(crate) refinement_scale_level: Option<u32>,
    pub(crate) refinement_shape: Option<Shape3D>,
    pub(crate) refinement_grid_to_world: Option<GridToWorld>,
    pub(crate) refinement_brick_shape: Option<Shape3D>,
    pub(crate) refinement_brick_grid_shape: Option<Shape3D>,
    pub(crate) refinement_brick_indices: Option<Vec<SpatialBrickIndex>>,
    pub(crate) refinement_visible_bricks: Option<usize>,
    pub(crate) refinement_estimated_decoded_bytes: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) enum Phase11ResidentBrickSet {
    U8(ResidentBrickSetU8),
    U16(ResidentBrickSetU16),
    F32(ResidentBrickSetF32),
}

#[derive(Debug, Clone)]
struct Phase11ResidentFrameSummary {
    output_pixels: u64,
    nonzero_pixels: u64,
    max_value: Value,
    complete: bool,
    missing_voxel_samples: u64,
    skip: Option<Value>,
}

#[derive(Debug, Clone)]
pub(crate) enum Phase11GpuMipFrame {
    Integer(GpuMipOutput),
    F32(GpuMipOutputF32),
}

impl Phase11ResidentBrickSet {
    pub(crate) fn stored_dtype_label(&self) -> &'static str {
        match self {
            Self::U8(_) => "Uint8",
            Self::U16(_) => "Uint16",
            Self::F32(_) => "Float32",
        }
    }

    pub(crate) fn positive_signal_range(&self) -> (Option<f64>, Option<f64>, usize) {
        let mut resident_min = f64::INFINITY;
        let mut resident_max = f64::NEG_INFINITY;
        let mut occupied_bricks = 0_usize;
        match self {
            Self::U8(resident) => {
                for brick in resident.bricks().iter().filter(|brick| brick.occupied) {
                    occupied_bricks += 1;
                    if brick.min.is_finite() {
                        resident_min = resident_min.min(brick.min);
                    }
                    if brick.max.is_finite() {
                        resident_max = resident_max.max(brick.max);
                    }
                }
            }
            Self::U16(resident) => {
                for brick in resident.bricks().iter().filter(|brick| brick.occupied) {
                    occupied_bricks += 1;
                    if brick.min.is_finite() {
                        resident_min = resident_min.min(brick.min);
                    }
                    if brick.max.is_finite() {
                        resident_max = resident_max.max(brick.max);
                    }
                }
            }
            Self::F32(resident) => {
                for brick in resident.bricks().iter().filter(|brick| brick.occupied) {
                    occupied_bricks += 1;
                    if brick.min.is_finite() {
                        resident_min = resident_min.min(brick.min);
                    }
                    if brick.max.is_finite() {
                        resident_max = resident_max.max(brick.max);
                    }
                }
            }
        }
        (
            (occupied_bricks > 0 && resident_min.is_finite()).then_some(resident_min),
            (occupied_bricks > 0 && resident_max.is_finite()).then_some(resident_max),
            occupied_bricks,
        )
    }

    pub(crate) fn render_cpu(
        &self,
        camera: mirante4d_core::CameraState,
        viewport: RenderViewport,
        mode: CameraRenderMode,
    ) -> Result<(MipImageU16, BrickFrameDiagnostics), mirante4d_renderer::RenderError> {
        match self {
            Self::U8(resident) => render_camera_u8_from_bricks_with_quality(
                resident,
                camera,
                viewport,
                mode,
                CameraRenderQuality::voxel_exact(),
            ),
            Self::U16(resident) => render_camera_from_bricks(resident, camera, viewport, mode),
            Self::F32(_) => Err(mirante4d_renderer::RenderError::InvalidChannelComposite(
                "Phase 13 integer render-mode probe does not support Float32 through CameraRenderMode",
            )),
        }
    }

    fn render_cpu_mip_summary(
        &self,
        camera: mirante4d_core::CameraState,
        viewport: RenderViewport,
    ) -> Result<Phase11ResidentFrameSummary, mirante4d_renderer::RenderError> {
        match self {
            Self::U8(resident) => {
                let (_image, diagnostics) = render_camera_u8_from_bricks_with_quality(
                    resident,
                    camera,
                    viewport,
                    CameraRenderMode::Mip,
                    CameraRenderQuality::voxel_exact(),
                )?;
                Ok(Phase11ResidentFrameSummary::from_integer_diagnostics(
                    diagnostics,
                ))
            }
            Self::U16(resident) => {
                let (_image, diagnostics) =
                    render_camera_from_bricks(resident, camera, viewport, CameraRenderMode::Mip)?;
                Ok(Phase11ResidentFrameSummary::from_integer_diagnostics(
                    diagnostics,
                ))
            }
            Self::F32(resident) => {
                let (_image, diagnostics) = render_camera_f32_from_bricks_with_quality(
                    resident,
                    camera,
                    viewport,
                    CameraRenderModeF32::Mip,
                    CameraRenderQuality::voxel_exact(),
                )?;
                Ok(Phase11ResidentFrameSummary::from_f32_diagnostics(
                    diagnostics,
                ))
            }
        }
    }

    pub(crate) fn render_gpu_direct(
        &self,
        renderer: &GpuRenderer,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        camera: mirante4d_core::CameraState,
        viewport: RenderViewport,
        mode: CameraRenderMode,
    ) -> Result<GpuMipOutput, GpuRenderError> {
        match self {
            Self::U8(resident) => renderer.render_camera_u8_from_bricks(
                resident,
                brick_shape,
                brick_grid_shape,
                camera,
                viewport,
                mode,
            ),
            Self::U16(resident) => renderer.render_camera_from_bricks(
                resident,
                brick_shape,
                brick_grid_shape,
                camera,
                viewport,
                mode,
            ),
            Self::F32(_) => Err(mirante4d_renderer::RenderError::InvalidChannelComposite(
                "Phase 13 integer render-mode probe does not support Float32 through CameraRenderMode",
            )
            .into()),
        }
    }

    fn render_gpu_mip_direct(
        &self,
        renderer: &GpuRenderer,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        camera: mirante4d_core::CameraState,
        viewport: RenderViewport,
    ) -> Result<Phase11GpuMipFrame, GpuRenderError> {
        match self {
            Self::U8(resident) => renderer
                .render_camera_u8_from_bricks(
                    resident,
                    brick_shape,
                    brick_grid_shape,
                    camera,
                    viewport,
                    CameraRenderMode::Mip,
                )
                .map(Phase11GpuMipFrame::Integer),
            Self::U16(resident) => renderer
                .render_camera_from_bricks(
                    resident,
                    brick_shape,
                    brick_grid_shape,
                    camera,
                    viewport,
                    CameraRenderMode::Mip,
                )
                .map(Phase11GpuMipFrame::Integer),
            Self::F32(resident) => renderer
                .render_camera_f32_from_bricks(
                    resident,
                    brick_shape,
                    brick_grid_shape,
                    camera,
                    viewport,
                    CameraRenderModeF32::Mip,
                )
                .map(Phase11GpuMipFrame::F32),
        }
    }

    pub(crate) fn render_gpu_mip_with_u16_batched_fallback(
        &self,
        renderer: &GpuRenderer,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        camera: mirante4d_core::CameraState,
        viewport: RenderViewport,
    ) -> (
        Result<Phase11GpuMipFrame, GpuRenderError>,
        bool,
        Option<String>,
    ) {
        match self {
            Self::U8(_) | Self::F32(_) => (
                self.render_gpu_mip_direct(
                    renderer,
                    brick_shape,
                    brick_grid_shape,
                    camera,
                    viewport,
                ),
                false,
                None,
            ),
            Self::U16(resident) => {
                match renderer.render_camera_from_bricks(
                    resident,
                    brick_shape,
                    brick_grid_shape,
                    camera,
                    viewport,
                    CameraRenderMode::Mip,
                ) {
                    Ok(output) => (Ok(Phase11GpuMipFrame::Integer(output)), false, None),
                    Err(err) => (
                        renderer
                            .render_camera_mip_from_bricks_batched(
                                resident,
                                brick_shape,
                                brick_grid_shape,
                                camera,
                                viewport,
                                PHASE11_GPU_MIP_BRICKS_PER_BATCH,
                            )
                            .map(Phase11GpuMipFrame::Integer),
                        true,
                        Some(err.to_string()),
                    ),
                }
            }
        }
    }
}

impl Phase11ResidentFrameSummary {
    fn from_integer_diagnostics(diagnostics: BrickFrameDiagnostics) -> Self {
        Self {
            output_pixels: diagnostics.frame.output_pixels,
            nonzero_pixels: diagnostics.frame.nonzero_pixels,
            max_value: json!(diagnostics.frame.max_value),
            complete: diagnostics.complete,
            missing_voxel_samples: diagnostics.missing_voxel_samples,
            skip: Some(brick_skip_json(diagnostics.skip)),
        }
    }

    fn from_f32_diagnostics(diagnostics: BrickFrameDiagnosticsF32) -> Self {
        Self {
            output_pixels: diagnostics.frame.output_pixels,
            nonzero_pixels: diagnostics.frame.nonzero_pixels,
            max_value: json!(diagnostics.frame.max_value),
            complete: diagnostics.complete,
            missing_voxel_samples: diagnostics.missing_voxel_samples,
            skip: None,
        }
    }

    fn json(&self) -> Value {
        json!({
            "output_pixels": self.output_pixels,
            "nonzero_pixels": self.nonzero_pixels,
            "max_value": self.max_value,
            "complete": self.complete,
            "missing_voxel_samples": self.missing_voxel_samples,
            "skip": self.skip,
        })
    }
}

impl Phase11GpuMipFrame {
    pub(crate) fn upload_ms(&self) -> Option<f64> {
        match self {
            Self::Integer(output) => output.timings.map(|timings| timings.upload_ms()),
            Self::F32(output) => output.timings.map(|timings| timings.upload_ms()),
        }
    }

    pub(crate) fn complete(&self) -> Option<bool> {
        match self {
            Self::Integer(output) => output
                .brick_frame
                .as_ref()
                .map(|diagnostics| diagnostics.complete),
            Self::F32(output) => output
                .brick_frame
                .as_ref()
                .map(|diagnostics| diagnostics.complete),
        }
    }

    pub(crate) fn missing_voxel_samples(&self) -> Option<u64> {
        match self {
            Self::Integer(output) => output
                .brick_frame
                .as_ref()
                .map(|diagnostics| diagnostics.missing_voxel_samples),
            Self::F32(output) => output
                .brick_frame
                .as_ref()
                .map(|diagnostics| diagnostics.missing_voxel_samples),
        }
    }

    pub(crate) fn nonzero_pixels(&self) -> u64 {
        match self {
            Self::Integer(output) => output.frame.nonzero_pixels,
            Self::F32(output) => output.frame.nonzero_pixels,
        }
    }

    pub(crate) fn frame_json(&self) -> Value {
        match self {
            Self::Integer(output) => json!({
                "output_pixels": output.frame.output_pixels,
                "nonzero_pixels": output.frame.nonzero_pixels,
                "max_value": output.frame.max_value,
                "complete": self.complete(),
                "missing_voxel_samples": self.missing_voxel_samples(),
                "skip": output.brick_frame.as_ref().map(|diagnostics| brick_skip_json(diagnostics.skip)),
            }),
            Self::F32(output) => json!({
                "output_pixels": output.frame.output_pixels,
                "nonzero_pixels": output.frame.nonzero_pixels,
                "max_value": output.frame.max_value,
                "complete": self.complete(),
                "missing_voxel_samples": self.missing_voxel_samples(),
                "skip": Value::Null,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Phase11InteractionCameraSample {
    scenario: &'static str,
    scenario_frame: u64,
    camera: CameraView,
}

#[derive(Debug)]
struct Phase11GpuInteractionFrame {
    report: Value,
    render_ms: Option<f64>,
    upload_ms: Option<f64>,
    ok: bool,
    batched: bool,
    complete: Option<bool>,
    missing_voxel_samples: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Phase11LodPlanningInput {
    pub(crate) camera_view: CameraView,
    pub(crate) camera: mirante4d_core::CameraState,
    pub(crate) viewport: RenderViewport,
    pub(crate) brick_pixel_stride: u64,
    pub(crate) max_visible_bricks: usize,
    pub(crate) max_decoded_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Phase11ResidentReadInput {
    pub(crate) stored_dtype: IntensityDType,
    pub(crate) scale_level: u32,
    pub(crate) timepoint: TimeIndex,
    pub(crate) volume_shape: Shape3D,
    pub(crate) grid_to_world: GridToWorld,
}

pub(crate) fn phase11_select_lod_plan(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    input: Phase11LodPlanningInput,
) -> anyhow::Result<Phase11LodPlan> {
    let scale_count = dataset.scale_count(layer_id)?;
    let target_scale_level = phase11_screen_equivalent_scale(
        dataset,
        layer_id,
        input.camera_view,
        input.viewport,
        scale_count,
    )?;
    let target_reason = if target_scale_level == 0 {
        "exact_s0"
    } else {
        "screen_equivalent_coarser_scale"
    };
    let mut reason = target_reason;
    for scale_index in target_scale_level as usize..scale_count {
        let plan = phase11_visible_bricks_at_scale(
            dataset,
            layer_id,
            input.camera,
            input.viewport,
            input.brick_pixel_stride,
            scale_index as u32,
        )?;
        if plan.visible_bricks.len() > input.max_visible_bricks {
            reason = "visible_brick_budget_limited";
            continue;
        }
        if plan.estimated_decoded_bytes > input.max_decoded_bytes {
            reason = "decoded_byte_budget_limited";
            continue;
        }
        if scale_index as u32 > target_scale_level
            && plan.visible_bricks.len() > PHASE11_DEFAULT_MAX_RESPONSIVE_VISIBLE_BRICKS
            && let Some(coarse_plan) = phase11_coarse_loading_plan_before_selected(
                dataset,
                layer_id,
                input,
                &plan,
                target_scale_level,
                reason,
                scale_count,
            )?
        {
            return Ok(coarse_plan);
        }
        return Ok(Phase11LodPlan {
            target_scale_level,
            reason,
            ..plan
        });
    }

    let mut plan = phase11_visible_bricks_at_scale(
        dataset,
        layer_id,
        input.camera,
        input.viewport,
        input.brick_pixel_stride,
        (scale_count - 1) as u32,
    )?;
    plan.target_scale_level = target_scale_level;
    plan.reason = reason;
    Ok(plan)
}

fn phase11_coarse_loading_plan_before_selected(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    input: Phase11LodPlanningInput,
    selected_plan: &Phase11LodPlan,
    target_scale_level: u32,
    selected_reason: &'static str,
    scale_count: usize,
) -> anyhow::Result<Option<Phase11LodPlan>> {
    for coarse_scale_index in (selected_plan.displayed_scale_level as usize + 1)..scale_count {
        let mut coarse_plan = phase11_visible_bricks_at_scale(
            dataset,
            layer_id,
            input.camera,
            input.viewport,
            input.brick_pixel_stride,
            coarse_scale_index as u32,
        )?;
        if coarse_plan.visible_bricks.len() > input.max_visible_bricks {
            continue;
        }
        if coarse_plan.estimated_decoded_bytes > input.max_decoded_bytes {
            continue;
        }
        coarse_plan.target_scale_level = target_scale_level;
        coarse_plan.reason = selected_reason;
        coarse_plan.refinement_scale_level = Some(selected_plan.displayed_scale_level);
        coarse_plan.refinement_shape = Some(selected_plan.displayed_shape);
        coarse_plan.refinement_grid_to_world = Some(selected_plan.displayed_grid_to_world);
        coarse_plan.refinement_brick_shape = Some(selected_plan.brick_shape);
        coarse_plan.refinement_brick_grid_shape = Some(selected_plan.brick_grid_shape);
        coarse_plan.refinement_brick_indices = Some(selected_plan.visible_bricks.clone());
        coarse_plan.refinement_visible_bricks = Some(selected_plan.visible_bricks.len());
        coarse_plan.refinement_estimated_decoded_bytes =
            Some(selected_plan.estimated_decoded_bytes);
        return Ok(Some(coarse_plan));
    }
    Ok(None)
}

pub(crate) fn phase11_visible_bricks_at_scale(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    camera: mirante4d_core::CameraState,
    viewport: RenderViewport,
    brick_pixel_stride: u64,
    scale_level: u32,
) -> anyhow::Result<Phase11LodPlan> {
    let displayed_shape = dataset.scale_shape(layer_id, scale_level)?;
    let displayed_grid_to_world = dataset.scale_grid_to_world(layer_id, scale_level)?;
    let brick_shape = dataset.brick_shape_at_scale(layer_id, scale_level)?;
    let brick_grid_shape = dataset.brick_grid_shape_at_scale(layer_id, scale_level)?;
    let visible_bricks = plan_visible_bricks(
        camera,
        viewport,
        BrickGridSpec {
            volume_shape: displayed_shape,
            brick_shape,
            grid_to_world: displayed_grid_to_world,
        },
        BrickPlanOptions {
            pixel_stride: brick_pixel_stride,
        },
    )?;
    let estimated_decoded_bytes =
        phase11_estimated_decoded_bytes(dataset, layer_id, scale_level, &visible_bricks)?;
    Ok(Phase11LodPlan {
        target_scale_level: scale_level,
        displayed_scale_level: scale_level,
        reason: "exact_s0",
        displayed_shape,
        displayed_grid_to_world,
        brick_shape,
        brick_grid_shape,
        visible_bricks,
        estimated_decoded_bytes,
        refinement_scale_level: None,
        refinement_shape: None,
        refinement_grid_to_world: None,
        refinement_brick_shape: None,
        refinement_brick_grid_shape: None,
        refinement_brick_indices: None,
        refinement_visible_bricks: None,
        refinement_estimated_decoded_bytes: None,
    })
}

pub(crate) fn phase11_read_resident_for_layer(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    input: Phase11ResidentReadInput,
    visible_bricks: &[SpatialBrickIndex],
) -> anyhow::Result<Phase11ResidentBrickSet> {
    match input.stored_dtype {
        IntensityDType::Uint8 => {
            let mut bricks = Vec::with_capacity(visible_bricks.len());
            for brick_index in visible_bricks {
                bricks.push(dataset.read_u8_brick_at_scale(
                    layer_id,
                    input.scale_level,
                    input.timepoint,
                    *brick_index,
                )?);
            }
            Ok(Phase11ResidentBrickSet::U8(ResidentBrickSetU8::new(
                layer_id.clone(),
                input.timepoint,
                input.volume_shape,
                input.grid_to_world,
                bricks,
            )))
        }
        IntensityDType::Uint16 => {
            let mut bricks = Vec::with_capacity(visible_bricks.len());
            for brick_index in visible_bricks {
                bricks.push(dataset.read_u16_brick_at_scale(
                    layer_id,
                    input.scale_level,
                    input.timepoint,
                    *brick_index,
                )?);
            }
            Ok(Phase11ResidentBrickSet::U16(ResidentBrickSetU16::new(
                layer_id.clone(),
                input.timepoint,
                input.volume_shape,
                input.grid_to_world,
                bricks,
            )))
        }
        IntensityDType::Float32 => {
            let mut bricks = Vec::with_capacity(visible_bricks.len());
            for brick_index in visible_bricks {
                bricks.push(dataset.read_f32_brick_at_scale(
                    layer_id,
                    input.scale_level,
                    input.timepoint,
                    *brick_index,
                )?);
            }
            Ok(Phase11ResidentBrickSet::F32(ResidentBrickSetF32::new(
                layer_id.clone(),
                input.timepoint,
                input.volume_shape,
                input.grid_to_world,
                bricks,
            )))
        }
    }
}

pub(crate) fn phase11_stored_dtype_for_layer(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
) -> anyhow::Result<IntensityDType> {
    Ok(dataset
        .layer(layer_id)
        .with_context(|| format!("layer {} was not found after opening dataset", layer_id))?
        .dtype
        .stored)
}

pub(crate) fn phase11_decoded_bytes_per_voxel(dtype: IntensityDType) -> anyhow::Result<u64> {
    match dtype {
        IntensityDType::Uint8 => Ok(std::mem::size_of::<u8>() as u64),
        IntensityDType::Uint16 => Ok(std::mem::size_of::<u16>() as u64),
        IntensityDType::Float32 => Ok(std::mem::size_of::<f32>() as u64),
    }
}

pub(crate) fn phase11_estimated_decoded_bytes(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    scale_level: u32,
    visible_bricks: &[SpatialBrickIndex],
) -> anyhow::Result<u64> {
    let bytes_per_voxel =
        phase11_decoded_bytes_per_voxel(phase11_stored_dtype_for_layer(dataset, layer_id)?)?;
    let mut total = 0u64;
    for brick_index in visible_bricks {
        let metadata =
            dataset.brick_metadata_at_scale(layer_id, scale_level, TimeIndex(0), *brick_index)?;
        if !metadata.occupied {
            continue;
        }
        total = total.saturating_add(
            metadata
                .region
                .shape()?
                .element_count()?
                .saturating_mul(bytes_per_voxel),
        );
    }
    Ok(total)
}

fn phase11_screen_equivalent_scale(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    camera_view: CameraView,
    viewport: RenderViewport,
    scale_count: usize,
) -> anyhow::Result<u32> {
    let _ = viewport;
    let world_per_pixel = camera_view
        .world_per_screen_point_at_target()
        .max(f64::EPSILON);
    let mut selected = 0;
    for scale_index in 0..scale_count {
        let scale_level = scale_index as u32;
        let grid_to_world = dataset.scale_grid_to_world(layer_id, scale_level)?;
        let voxel_size = representative_voxel_world_size(grid_to_world);
        if voxel_size <= world_per_pixel {
            selected = scale_level;
        }
    }
    Ok(selected)
}

fn representative_voxel_world_size(grid_to_world: GridToWorld) -> f64 {
    let x = grid_to_world.transform_vector(DVec3::X).length();
    let y = grid_to_world.transform_vector(DVec3::Y).length();
    let z = grid_to_world.transform_vector(DVec3::Z).length();
    x.max(y).max(z).max(f64::EPSILON)
}

fn bench_phase11_gpu_resident(
    resident: &Phase11ResidentBrickSet,
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
    camera: mirante4d_core::CameraState,
    viewport: RenderViewport,
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
                "error": err.to_string(),
            }));
        }
    };
    let init_ms = started.elapsed().as_secs_f64() * 1000.0;
    let adapter = renderer.adapter_diagnostics().clone();
    let render_started = Instant::now();
    let (output, mip_batched, monolithic_error) = resident
        .render_gpu_mip_with_u16_batched_fallback(
            &renderer,
            brick_shape,
            brick_grid_shape,
            camera,
            viewport,
        );
    let render_ms = render_started.elapsed().as_secs_f64() * 1000.0;
    match output {
        Ok(output) => {
            let cached_started = Instant::now();
            let cached = match (mip_batched, resident) {
                (true, Phase11ResidentBrickSet::U16(resident_u16)) => renderer
                    .render_camera_mip_from_bricks_batched(
                        resident_u16,
                        brick_shape,
                        brick_grid_shape,
                        camera,
                        viewport,
                        PHASE11_GPU_MIP_BRICKS_PER_BATCH,
                    )
                    .map(Phase11GpuMipFrame::Integer),
                _ => resident.render_gpu_mip_direct(
                    &renderer,
                    brick_shape,
                    brick_grid_shape,
                    camera,
                    viewport,
                ),
            };
            let cached_ms = cached_started.elapsed().as_secs_f64() * 1000.0;
            let cached_error = cached.err().map(|err| err.to_string());
            let stats = renderer.stats().ok();
            Ok(json!({
                "available": true,
                "adapter": adapter.name,
                "backend": adapter.backend,
                "device_type": adapter.device_type,
                "driver": adapter.driver,
                "driver_info": adapter.driver_info,
                "stored_dtype": resident.stored_dtype_label(),
                "mip_bricks_per_batch": PHASE11_GPU_MIP_BRICKS_PER_BATCH,
                "mip_batched": mip_batched,
                "monolithic_error": monolithic_error,
                "timings_ms": {
                    "init": init_ms,
                    "resident_first_mip": render_ms,
                    "resident_cached_mip": cached_ms,
                },
                "cached_error": cached_error,
                "frame": output.frame_json(),
                "stats": stats.map(gpu_stats_json),
            }))
        }
        Err(err) => Ok(json!({
            "available": true,
            "adapter": adapter.name,
            "backend": adapter.backend,
            "device_type": adapter.device_type,
            "driver": adapter.driver,
            "driver_info": adapter.driver_info,
            "stored_dtype": resident.stored_dtype_label(),
            "mip_bricks_per_batch": PHASE11_GPU_MIP_BRICKS_PER_BATCH,
            "timings_ms": {
                "init": init_ms,
                "resident_first_mip": render_ms,
            },
            "resident_error": err.to_string(),
            "stats": renderer.stats().ok().map(gpu_stats_json),
        })),
    }
}

fn phase11_render_gpu_interaction_frame(
    renderer: &GpuRenderer,
    resident: &Phase11ResidentBrickSet,
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
    camera: mirante4d_core::CameraState,
    viewport: RenderViewport,
) -> Phase11GpuInteractionFrame {
    let stats_before = renderer.stats().ok();
    let started = Instant::now();
    let (output, mip_batched, monolithic_error) = resident
        .render_gpu_mip_with_u16_batched_fallback(
            renderer,
            brick_shape,
            brick_grid_shape,
            camera,
            viewport,
        );
    let render_ms = started.elapsed().as_secs_f64() * 1000.0;
    let stats_after = renderer.stats().ok();
    let resource_delta = gpu_stats_delta_json(stats_before, stats_after);
    match output {
        Ok(output) => {
            let upload_ms = output.upload_ms();
            let complete = output.complete();
            let missing_voxel_samples = output.missing_voxel_samples();
            Phase11GpuInteractionFrame {
                report: json!({
                    "available": true,
                    "ok": true,
                    "stored_dtype": resident.stored_dtype_label(),
                    "mip_batched": mip_batched,
                    "monolithic_error": monolithic_error,
                    "timings_ms": phase11_gpu_interaction_timings_json(render_ms, upload_ms),
                    "upload_timing_source": "renderer_atlas_update_wall_time",
                    "resource_delta": resource_delta,
                    "frame": output.frame_json(),
                    "stats": renderer.stats().ok().map(gpu_stats_json),
                }),
                render_ms: Some(render_ms),
                upload_ms,
                ok: true,
                batched: mip_batched,
                complete,
                missing_voxel_samples,
            }
        }
        Err(err) => Phase11GpuInteractionFrame {
            report: json!({
                "available": true,
                "ok": false,
                "stored_dtype": resident.stored_dtype_label(),
                "mip_batched": mip_batched,
                "monolithic_error": monolithic_error,
                "timings_ms": phase11_gpu_interaction_timings_json(render_ms, None),
                "upload_timing_source": "unavailable_on_render_error",
                "resource_delta": resource_delta,
                "error": err.to_string(),
                "stats": renderer.stats().ok().map(gpu_stats_json),
            }),
            render_ms: Some(render_ms),
            upload_ms: None,
            ok: false,
            batched: mip_batched,
            complete: None,
            missing_voxel_samples: None,
        },
    }
}

fn phase11_settled_refinement_report(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    timepoint: TimeIndex,
    plan: &Phase11LodPlan,
    gpu_renderer: Option<&GpuRenderer>,
    camera: mirante4d_core::CameraState,
    viewport: RenderViewport,
) -> anyhow::Result<Value> {
    let Some(scale_level) = plan.refinement_scale_level else {
        return Ok(json!({
            "available": false,
            "reason": "no_refinement_plan",
        }));
    };
    let Some(visible_bricks) = plan.refinement_brick_indices.as_ref() else {
        return Ok(json!({
            "available": false,
            "reason": "missing_refinement_brick_indices",
            "scale_level": scale_level,
        }));
    };
    let Some(displayed_shape) = plan.refinement_shape else {
        return Ok(json!({
            "available": false,
            "reason": "missing_refinement_shape",
            "scale_level": scale_level,
        }));
    };
    let Some(grid_to_world) = plan.refinement_grid_to_world else {
        return Ok(json!({
            "available": false,
            "reason": "missing_refinement_transform",
            "scale_level": scale_level,
        }));
    };
    let Some(brick_shape) = plan.refinement_brick_shape else {
        return Ok(json!({
            "available": false,
            "reason": "missing_refinement_brick_shape",
            "scale_level": scale_level,
        }));
    };
    let Some(brick_grid_shape) = plan.refinement_brick_grid_shape else {
        return Ok(json!({
            "available": false,
            "reason": "missing_refinement_brick_grid_shape",
            "scale_level": scale_level,
        }));
    };

    let started = Instant::now();
    let stored_dtype = phase11_stored_dtype_for_layer(dataset, layer_id)?;
    let resident = phase11_read_resident_for_layer(
        dataset,
        layer_id,
        Phase11ResidentReadInput {
            stored_dtype,
            scale_level,
            timepoint,
            volume_shape: displayed_shape,
            grid_to_world,
        },
        visible_bricks,
    )?;
    let read_ms = started.elapsed().as_secs_f64() * 1000.0;

    let gpu = if let Some(renderer) = gpu_renderer {
        phase11_render_gpu_interaction_frame(
            renderer,
            &resident,
            brick_shape,
            brick_grid_shape,
            camera,
            viewport,
        )
        .report
    } else {
        json!({
            "available": false,
            "error": "no usable GPU renderer",
        })
    };

    Ok(json!({
        "available": true,
        "scale_level": scale_level,
        "stored_dtype": resident.stored_dtype_label(),
        "visible_bricks": visible_bricks.len(),
        "estimated_decoded_bytes": plan.refinement_estimated_decoded_bytes,
        "timings_ms": {
            "read_visible_bricks": read_ms,
        },
        "gpu": gpu,
    }))
}

fn phase11_interaction_camera_sequence(
    base_camera: CameraView,
    steps_per_scenario: u64,
) -> Vec<Phase11InteractionCameraSample> {
    let steps = steps_per_scenario.max(1);
    let mut sequence = Vec::with_capacity(1 + steps as usize * 3);
    sequence.push(Phase11InteractionCameraSample {
        scenario: "first_visible",
        scenario_frame: 0,
        camera: base_camera,
    });

    for step in 0..steps {
        let fraction = (step + 1) as f64 / steps as f64;
        let mut camera = base_camera;
        camera.orbit_by(0.45 * fraction, 0.20 * fraction);
        sequence.push(Phase11InteractionCameraSample {
            scenario: "orbit",
            scenario_frame: step,
            camera,
        });
    }

    for step in 0..steps {
        let fraction = (step + 1) as f64 / steps as f64;
        let mut camera = base_camera;
        camera.pan_by(
            base_camera.world_per_screen_point_at_target()
                * DEFAULT_PRESENTATION_VIEWPORT_POINTS.height_points
                * 0.10
                * fraction,
            -base_camera.world_per_screen_point_at_target()
                * DEFAULT_PRESENTATION_VIEWPORT_POINTS.height_points
                * 0.06
                * fraction,
        );
        sequence.push(Phase11InteractionCameraSample {
            scenario: "pan",
            scenario_frame: step,
            camera,
        });
    }

    for step in 0..steps {
        let fraction = (step + 1) as f64 / steps as f64;
        let mut camera = base_camera;
        camera.zoom_by(1.0 - 0.50 * fraction);
        sequence.push(Phase11InteractionCameraSample {
            scenario: "zoom",
            scenario_frame: step,
            camera,
        });
    }

    sequence
}

fn phase11_camera_json(camera: CameraView) -> Value {
    json!({
        "projection": projection_label(camera.projection),
        "target": {
            "x": camera.target.x,
            "y": camera.target.y,
            "z": camera.target.z,
        },
        "orientation": {
            "x": camera.orientation.x,
            "y": camera.orientation.y,
            "z": camera.orientation.z,
            "w": camera.orientation.w,
        },
        "orthographic_world_per_screen_point": camera.orthographic_world_per_screen_point,
        "perspective_focal_length_screen_points": camera.perspective_focal_length_screen_points,
        "perspective_view_distance_world": camera.perspective_view_distance_world,
    })
}

fn projection_label(projection: Projection) -> &'static str {
    match projection {
        Projection::Perspective => "perspective",
        Projection::Orthographic => "orthographic",
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Phase11ViewportScenario {
    pub(crate) label: String,
    pub(crate) viewport: RenderViewport,
}

pub(crate) fn phase11_viewport_matrix_for_shape(
    shape: Shape3D,
) -> anyhow::Result<Vec<Phase11ViewportScenario>> {
    let mut scenarios = Vec::new();
    push_unique_phase11_viewport(&mut scenarios, "square_512", RenderViewport::new(512, 512)?);
    push_unique_phase11_viewport(&mut scenarios, "hd_720p", RenderViewport::new(1280, 720)?);
    push_unique_phase11_viewport(
        &mut scenarios,
        "full_hd_1080p",
        RenderViewport::new(1920, 1080)?,
    );
    push_unique_phase11_viewport(
        &mut scenarios,
        "default_package_capped",
        phase11_benchmark_viewport_for_shape(shape)?,
    );
    if let Some(viewport) = optional_phase11_viewport_from_env(
        "MIRANTE4D_PHASE11_HIDPI_VIEWPORT_WIDTH",
        "MIRANTE4D_PHASE11_HIDPI_VIEWPORT_HEIGHT",
    )? {
        push_unique_phase11_viewport(&mut scenarios, "local_hidpi_window", viewport);
    }
    if let Some(viewport) = optional_phase11_viewport_from_env(
        "MIRANTE4D_PHASE11_MAXIMIZED_VIEWPORT_WIDTH",
        "MIRANTE4D_PHASE11_MAXIMIZED_VIEWPORT_HEIGHT",
    )? {
        push_unique_phase11_viewport(&mut scenarios, "local_maximized_window", viewport);
    }
    Ok(scenarios)
}

fn optional_phase11_viewport_from_env(
    width_name: &str,
    height_name: &str,
) -> anyhow::Result<Option<RenderViewport>> {
    match (env_u64(width_name)?, env_u64(height_name)?) {
        (Some(width), Some(height)) => Ok(Some(RenderViewport::new(width, height)?)),
        (None, None) => Ok(None),
        _ => bail!("{width_name} and {height_name} must be set together"),
    }
}

fn push_unique_phase11_viewport(
    scenarios: &mut Vec<Phase11ViewportScenario>,
    label: &str,
    viewport: RenderViewport,
) {
    if scenarios
        .iter()
        .any(|scenario| scenario.viewport == viewport)
    {
        return;
    }
    scenarios.push(Phase11ViewportScenario {
        label: label.to_owned(),
        viewport,
    });
}

#[cfg(test)]
mod tests;
