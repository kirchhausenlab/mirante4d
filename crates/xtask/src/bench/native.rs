use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use mirante4d_data::{
    BrickReadPool, BrickReadStatus, BrickRequestPriority, DatasetHandle, DenseVolumeU16,
    SpatialBrickIndex,
};
use mirante4d_domain::{
    DisplayWindow, IntensityDType, IsoLightState, LayerTransfer, Opacity, RgbColor, Shape3D,
    Shape4D, TimeIndex, TransferCurve,
};
use mirante4d_format::{
    ChannelMetadata, CurrentShape4DExt, DenseU16Layer, ExistingPackagePolicy, LayerId,
    NativeU16Dataset, WorldSpace, WorldUnit, default_u16_display, grid_to_world_scale_um,
    write_native_u16_dataset,
};
use mirante4d_render_api::{CameraAxes, CameraFrame};
use mirante4d_renderer::{
    BrickGridSpec, BrickPlanOptions, BrickSkipDiagnostics, CameraRenderMode, CameraRenderQuality,
    IntensityTransfer, RenderViewport, ResidentBrickSetU8, ResidentBrickSetU16,
    gpu::{GpuDisplayFrame, GpuRenderer, GpuResidentDisplayChannel, GpuResidentDisplayRequest},
    plan_visible_bricks, render_camera, render_camera_from_bricks, render_camera_u8,
    render_camera_u8_from_bricks_with_quality,
};
use serde_json::{Value, json};

use crate::host::{
    benchmark_baseline_class, benchmark_hardware_class, benchmark_host_context,
    benchmark_native_package_dataset_class, gpu_stats_json,
};
use crate::{
    benchmark_camera_for_shape, benchmark_camera_for_volume, benchmark_camera_frame, env_u64,
    phase11_gpu_brick_cache_budget_bytes, phase11_gpu_volume_cache_budget_bytes,
    stable_id_from_name,
};

const DEFAULT_STRESS_T: u64 = 3;
const DEFAULT_STRESS_Z: u64 = 64;
const DEFAULT_STRESS_Y: u64 = 128;
const DEFAULT_STRESS_X: u64 = 128;
const DEFAULT_STRESS_BRICK_Z: u64 = 16;
const DEFAULT_STRESS_BRICK_Y: u64 = 16;
const DEFAULT_STRESS_BRICK_X: u64 = 16;
const DEFAULT_STRESS_WORKERS: u64 = 4;
const DEFAULT_STRESS_GPU_SET_SIZE: usize = 32;

pub(crate) fn bench_native_package(package: &Path) -> anyhow::Result<PathBuf> {
    bench_native_package_with_overrides(package, NativePackageBenchmarkOverrides::default())
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NativePackageBenchmarkOverrides {
    pub(crate) viewport_width: Option<u64>,
    pub(crate) viewport_height: Option<u64>,
    pub(crate) brick_pixel_stride: Option<u64>,
}

pub(crate) fn bench_native_package_with_overrides(
    package: &Path,
    overrides: NativePackageBenchmarkOverrides,
) -> anyhow::Result<PathBuf> {
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
    let timepoint = TimeIndex::new(0);
    let stored_dtype = dataset
        .layer(&layer_id)
        .context("first layer id was not found after opening dataset")?
        .dtype
        .stored;
    if stored_dtype == IntensityDType::Uint8 {
        return bench_native_package_u8(
            package,
            overrides,
            dataset,
            dataset_class,
            layer_id,
            timepoint,
            started,
            open_ms,
        );
    }
    if stored_dtype == IntensityDType::Float32 {
        bail!("bench-native-package does not yet support Float32 first-layer packages");
    }
    let volume_read_started = Instant::now();
    let volume = dataset.read_u16_volume(&layer_id, timepoint)?;
    let volume_read_ms = volume_read_started.elapsed().as_secs_f64() * 1000.0;

    let viewport = benchmark_viewport_for_volume_with_overrides(&volume, overrides)?;
    let brick_pixel_stride = benchmark_brick_pixel_stride_with_overrides(overrides)?;
    let camera = benchmark_camera_frame(benchmark_camera_for_volume(&volume));
    let brick_shape = dataset.brick_shape_at_scale(&layer_id, volume.scale_level)?;
    let brick_grid_shape = dataset.brick_grid_shape_at_scale(&layer_id, volume.scale_level)?;
    let plan_started = Instant::now();
    let visible_bricks = plan_visible_bricks(
        camera,
        viewport,
        BrickGridSpec {
            volume_shape: volume.shape,
            brick_shape,
            grid_to_world: volume.grid_to_world,
        },
        BrickPlanOptions {
            pixel_stride: brick_pixel_stride,
        },
    )?;
    let plan_ms = plan_started.elapsed().as_secs_f64() * 1000.0;
    if visible_bricks.is_empty() {
        bail!(
            "benchmark camera produced no visible bricks for {}",
            package.display()
        );
    }
    let brick_read_started = Instant::now();
    let mut bricks = Vec::with_capacity(visible_bricks.len());
    for brick_index in &visible_bricks {
        bricks.push(dataset.read_u16_brick_at_scale(
            &layer_id,
            volume.scale_level,
            timepoint,
            *brick_index,
        )?);
    }
    let brick_read_ms = brick_read_started.elapsed().as_secs_f64() * 1000.0;

    let resident = ResidentBrickSetU16::new(
        layer_id.clone(),
        timepoint,
        volume.shape,
        volume.grid_to_world,
        bricks,
    );
    let resident_render_started = Instant::now();
    let (_resident_image, resident_diagnostics) =
        render_camera_from_bricks(&resident, camera, viewport, CameraRenderMode::Mip)?;
    let resident_render_ms = resident_render_started.elapsed().as_secs_f64() * 1000.0;
    let dense_render_started = Instant::now();
    let (_dense_image, dense_diagnostics) =
        render_camera(&volume, camera, viewport, CameraRenderMode::Mip)?;
    let dense_render_ms = dense_render_started.elapsed().as_secs_f64() * 1000.0;

    let gpu_benchmark = bench_gpu_render_paths(
        &volume,
        &resident,
        brick_shape,
        brick_grid_shape,
        camera,
        viewport,
    );
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
    let output_path = output_root.join(format!("bench-native-package-{package_name}.json"));
    let report_json = json!({
        "benchmark": "bench-native-package",
        "benchmark_schema_version": 1,
        "baseline_class": benchmark_baseline_class(),
        "hardware_class": benchmark_hardware_class(),
        "dataset_class": dataset_class,
        "hardware": benchmark_host_context(),
        "package": package,
        "layer_id": layer_id.to_string(),
        "timepoint": timepoint.get(),
        "scale_level": volume.scale_level,
        "shape": {
            "z": volume.shape.z(),
            "y": volume.shape.y(),
            "x": volume.shape.x(),
        },
        "brick_shape": {
            "z": brick_shape.z(),
            "y": brick_shape.y(),
            "x": brick_shape.x(),
        },
        "brick_grid_shape": {
            "z": brick_grid_shape.z(),
            "y": brick_grid_shape.y(),
            "x": brick_grid_shape.x(),
        },
        "viewport": {
            "width": viewport.width,
            "height": viewport.height,
            "brick_pixel_stride": brick_pixel_stride,
        },
        "visible_bricks": visible_bricks.len(),
        "resident_complete": resident_diagnostics.complete,
        "timings_ms": {
            "open": open_ms,
            "read_first_volume": volume_read_ms,
            "plan_visible_bricks": plan_ms,
            "read_visible_bricks": brick_read_ms,
            "cpu_resident_brick_mip": resident_render_ms,
            "cpu_dense_mip": dense_render_ms,
            "total": total_ms,
        },
        "gpu": gpu_benchmark,
        "resident_frame": {
            "output_pixels": resident_diagnostics.frame.output_pixels,
            "nonzero_pixels": resident_diagnostics.frame.nonzero_pixels,
            "max_value": resident_diagnostics.frame.max_value,
            "missing_voxel_samples": resident_diagnostics.missing_voxel_samples,
            "skip": brick_skip_json(resident_diagnostics.skip),
        },
        "dense_frame": {
            "output_pixels": dense_diagnostics.output_pixels,
            "nonzero_pixels": dense_diagnostics.nonzero_pixels,
            "max_value": dense_diagnostics.max_value,
        },
        "data_stats": {
            "subset_reads": stats.subset_reads,
            "decoded_values": stats.decoded_values,
            "volume_cache_hits": stats.volume_cache_hits,
            "volume_cache_misses": stats.volume_cache_misses,
            "brick_cache_hits": stats.brick_cache_hits,
            "brick_cache_misses": stats.brick_cache_misses,
            "brick_cache_u8_bytes": stats.brick_cache_u8_bytes,
            "brick_cache_u16_bytes": stats.brick_cache_u16_bytes,
            "brick_cache_f32_bytes": stats.brick_cache_f32_bytes,
            "brick_reads": stats.brick_reads,
            "decoded_brick_values": stats.decoded_brick_values,
            "encoded_payload_bytes_read": stats.encoded_payload_bytes_read,
            "encoded_shard_payloads_read": stats.encoded_shard_payloads_read,
            "shard_index_cache_hits": stats.shard_index_cache_hits,
            "shard_index_cache_misses": stats.shard_index_cache_misses,
            "shard_index_cache_entries": stats.shard_index_cache_entries,
        },
    });
    fs::write(
        &output_path,
        format!("{}\n", serde_json::to_string_pretty(&report_json)?),
    )
    .with_context(|| format!("failed to write {}", output_path.display()))?;
    Ok(output_path)
}

#[allow(clippy::too_many_arguments)]
fn bench_native_package_u8(
    package: &Path,
    overrides: NativePackageBenchmarkOverrides,
    dataset: DatasetHandle,
    dataset_class: String,
    layer_id: LayerId,
    timepoint: TimeIndex,
    started: Instant,
    open_ms: f64,
) -> anyhow::Result<PathBuf> {
    let volume_read_started = Instant::now();
    let volume = dataset.read_u8_volume(&layer_id, timepoint)?;
    let volume_read_ms = volume_read_started.elapsed().as_secs_f64() * 1000.0;

    let viewport = benchmark_viewport_for_shape_with_overrides(volume.shape, overrides)?;
    let brick_pixel_stride = benchmark_brick_pixel_stride_with_overrides(overrides)?;
    let camera = benchmark_camera_frame(benchmark_camera_for_shape(
        volume.shape,
        volume.grid_to_world,
    ));
    let brick_shape = dataset.brick_shape_at_scale(&layer_id, volume.scale_level)?;
    let brick_grid_shape = dataset.brick_grid_shape_at_scale(&layer_id, volume.scale_level)?;
    let plan_started = Instant::now();
    let visible_bricks = plan_visible_bricks(
        camera,
        viewport,
        BrickGridSpec {
            volume_shape: volume.shape,
            brick_shape,
            grid_to_world: volume.grid_to_world,
        },
        BrickPlanOptions {
            pixel_stride: brick_pixel_stride,
        },
    )?;
    let plan_ms = plan_started.elapsed().as_secs_f64() * 1000.0;
    if visible_bricks.is_empty() {
        bail!(
            "benchmark camera produced no visible bricks for {}",
            package.display()
        );
    }
    let brick_read_started = Instant::now();
    let mut bricks = Vec::with_capacity(visible_bricks.len());
    for brick_index in &visible_bricks {
        bricks.push(dataset.read_u8_brick_at_scale(
            &layer_id,
            volume.scale_level,
            timepoint,
            *brick_index,
        )?);
    }
    let brick_read_ms = brick_read_started.elapsed().as_secs_f64() * 1000.0;

    let resident = ResidentBrickSetU8::new(
        layer_id.clone(),
        timepoint,
        volume.shape,
        volume.grid_to_world,
        bricks,
    );
    let resident_render_started = Instant::now();
    let (_resident_image, resident_diagnostics) = render_camera_u8_from_bricks_with_quality(
        &resident,
        camera,
        viewport,
        CameraRenderMode::Mip,
        CameraRenderQuality::voxel_exact(),
    )?;
    let resident_render_ms = resident_render_started.elapsed().as_secs_f64() * 1000.0;
    let dense_render_started = Instant::now();
    let (_dense_image, dense_diagnostics) =
        render_camera_u8(&volume, camera, viewport, CameraRenderMode::Mip)?;
    let dense_render_ms = dense_render_started.elapsed().as_secs_f64() * 1000.0;

    let gpu_benchmark =
        bench_gpu_render_paths_u8(&resident, brick_shape, brick_grid_shape, camera, viewport);
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
    let output_path = output_root.join(format!("bench-native-package-{package_name}.json"));
    let report_json = json!({
        "benchmark": "bench-native-package",
        "benchmark_schema_version": 1,
        "baseline_class": benchmark_baseline_class(),
        "hardware_class": benchmark_hardware_class(),
        "dataset_class": dataset_class,
        "hardware": benchmark_host_context(),
        "package": package,
        "layer_id": layer_id.to_string(),
        "stored_dtype": "Uint8",
        "timepoint": timepoint.get(),
        "scale_level": volume.scale_level,
        "shape": {
            "z": volume.shape.z(),
            "y": volume.shape.y(),
            "x": volume.shape.x(),
        },
        "brick_shape": {
            "z": brick_shape.z(),
            "y": brick_shape.y(),
            "x": brick_shape.x(),
        },
        "brick_grid_shape": {
            "z": brick_grid_shape.z(),
            "y": brick_grid_shape.y(),
            "x": brick_grid_shape.x(),
        },
        "viewport": {
            "width": viewport.width,
            "height": viewport.height,
            "brick_pixel_stride": brick_pixel_stride,
        },
        "visible_bricks": visible_bricks.len(),
        "resident_complete": resident_diagnostics.complete,
        "timings_ms": {
            "open": open_ms,
            "read_first_volume": volume_read_ms,
            "plan_visible_bricks": plan_ms,
            "read_visible_bricks": brick_read_ms,
            "cpu_resident_brick_mip": resident_render_ms,
            "cpu_dense_mip": dense_render_ms,
            "total": total_ms,
        },
        "gpu": gpu_benchmark,
        "resident_frame": {
            "output_pixels": resident_diagnostics.frame.output_pixels,
            "nonzero_pixels": resident_diagnostics.frame.nonzero_pixels,
            "max_value": resident_diagnostics.frame.max_value,
            "missing_voxel_samples": resident_diagnostics.missing_voxel_samples,
            "skip": brick_skip_json(resident_diagnostics.skip),
        },
        "dense_frame": {
            "output_pixels": dense_diagnostics.output_pixels,
            "nonzero_pixels": dense_diagnostics.nonzero_pixels,
            "max_value": dense_diagnostics.max_value,
        },
        "data_stats": {
            "subset_reads": stats.subset_reads,
            "decoded_values": stats.decoded_values,
            "volume_cache_hits": stats.volume_cache_hits,
            "volume_cache_misses": stats.volume_cache_misses,
            "brick_cache_hits": stats.brick_cache_hits,
            "brick_cache_misses": stats.brick_cache_misses,
            "brick_cache_u8_bytes": stats.brick_cache_u8_bytes,
            "brick_cache_u16_bytes": stats.brick_cache_u16_bytes,
            "brick_cache_f32_bytes": stats.brick_cache_f32_bytes,
            "brick_reads": stats.brick_reads,
            "decoded_brick_values": stats.decoded_brick_values,
            "encoded_payload_bytes_read": stats.encoded_payload_bytes_read,
            "encoded_shard_payloads_read": stats.encoded_shard_payloads_read,
            "shard_index_cache_hits": stats.shard_index_cache_hits,
            "shard_index_cache_misses": stats.shard_index_cache_misses,
            "shard_index_cache_entries": stats.shard_index_cache_entries,
        },
    });
    fs::write(
        &output_path,
        format!("{}\n", serde_json::to_string_pretty(&report_json)?),
    )
    .with_context(|| format!("failed to write {}", output_path.display()))?;
    Ok(output_path)
}

pub(crate) fn bench_runtime_stress() -> anyhow::Result<PathBuf> {
    let started = Instant::now();
    let output_root = PathBuf::from("target")
        .join("mirante4d")
        .join("benchmarks")
        .join("runtime-stress");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;

    let shape = runtime_stress_shape()?;
    let chunk_shape = runtime_stress_chunk_shape()?;
    shape.chunk_grid(chunk_shape)?;
    let package = output_root.join("many-spatial-bricks.m4d");

    let write_started = Instant::now();
    write_runtime_stress_package(&package, shape, chunk_shape)?;
    let write_ms = write_started.elapsed().as_secs_f64() * 1000.0;

    let open_started = Instant::now();
    let dataset = DatasetHandle::open(&package)?;
    let open_ms = open_started.elapsed().as_secs_f64() * 1000.0;

    let layer_id = dataset.first_layer_id()?;
    let timepoint = TimeIndex::new(0);
    let read_started = Instant::now();
    let volume = dataset.read_u16_volume(&layer_id, timepoint)?;
    let read_volume_ms = read_started.elapsed().as_secs_f64() * 1000.0;

    let viewport = benchmark_viewport_for_volume(&volume)?;
    let brick_pixel_stride = runtime_stress_brick_pixel_stride()?;
    let camera = benchmark_camera_frame(benchmark_camera_for_volume(&volume));
    let brick_shape = dataset.brick_shape_at_scale(&layer_id, volume.scale_level)?;
    let brick_grid_shape = dataset.brick_grid_shape_at_scale(&layer_id, volume.scale_level)?;
    let plan_started = Instant::now();
    let visible_bricks = plan_visible_bricks(
        camera,
        viewport,
        BrickGridSpec {
            volume_shape: volume.shape,
            brick_shape,
            grid_to_world: volume.grid_to_world,
        },
        BrickPlanOptions {
            pixel_stride: brick_pixel_stride,
        },
    )?;
    let plan_ms = plan_started.elapsed().as_secs_f64() * 1000.0;
    if visible_bricks.is_empty() {
        bail!("runtime stress benchmark produced no visible bricks");
    }

    let read_first_started = Instant::now();
    let mut resident_bricks = Vec::with_capacity(visible_bricks.len());
    for brick_index in &visible_bricks {
        resident_bricks.push(dataset.read_u16_brick_at_scale(
            &layer_id,
            volume.scale_level,
            timepoint,
            *brick_index,
        )?);
    }
    let read_visible_first_ms = read_first_started.elapsed().as_secs_f64() * 1000.0;

    let read_cached_started = Instant::now();
    for brick_index in &visible_bricks {
        dataset.read_u16_brick_at_scale(&layer_id, volume.scale_level, timepoint, *brick_index)?;
    }
    let read_visible_cached_ms = read_cached_started.elapsed().as_secs_f64() * 1000.0;

    let worker_benchmark = bench_worker_prefetch(
        dataset.clone(),
        layer_id.clone(),
        volume.scale_level,
        shape.t(),
        &visible_bricks,
    )?;

    let resident = ResidentBrickSetU16::new(
        layer_id.clone(),
        timepoint,
        volume.shape,
        volume.grid_to_world,
        resident_bricks.clone(),
    );
    let resident_render_started = Instant::now();
    let (_resident_image, resident_diagnostics) =
        render_camera_from_bricks(&resident, camera, viewport, CameraRenderMode::Mip)?;
    let resident_render_ms = resident_render_started.elapsed().as_secs_f64() * 1000.0;

    let dense_render_started = Instant::now();
    let (_dense_image, dense_diagnostics) =
        render_camera(&volume, camera, viewport, CameraRenderMode::Mip)?;
    let dense_render_ms = dense_render_started.elapsed().as_secs_f64() * 1000.0;

    let gpu_stress = bench_gpu_runtime_stress(
        &layer_id,
        &volume,
        &resident_bricks,
        brick_shape,
        brick_grid_shape,
        camera,
        viewport,
    )?;
    let stats = dataset.stats()?;
    let total_ms = started.elapsed().as_secs_f64() * 1000.0;
    let output_path = output_root.join("bench-runtime-stress.json");
    let report_json = json!({
        "benchmark": "bench-runtime-stress",
        "benchmark_schema_version": 1,
        "baseline_class": benchmark_baseline_class(),
        "hardware_class": benchmark_hardware_class(),
        "dataset_class": "synthetic_runtime_stress",
        "hardware": benchmark_host_context(),
        "package": package,
        "layer_id": layer_id.to_string(),
        "shape": {
            "t": shape.t(),
            "z": shape.z(),
            "y": shape.y(),
            "x": shape.x(),
        },
        "brick_shape": {
            "t": chunk_shape.t(),
            "z": chunk_shape.z(),
            "y": chunk_shape.y(),
            "x": chunk_shape.x(),
        },
        "brick_grid_shape": {
            "t": shape.chunk_grid(chunk_shape)?.t(),
            "z": brick_grid_shape.z(),
            "y": brick_grid_shape.y(),
            "x": brick_grid_shape.x(),
        },
        "viewport": {
            "width": viewport.width,
            "height": viewport.height,
            "brick_pixel_stride": brick_pixel_stride,
        },
        "visible_bricks": visible_bricks.len(),
        "worker_prefetch": worker_benchmark,
        "timings_ms": {
            "write_synthetic_package": write_ms,
            "open": open_ms,
            "read_first_volume": read_volume_ms,
            "plan_visible_bricks": plan_ms,
            "read_visible_bricks_first_pass": read_visible_first_ms,
            "read_visible_bricks_cached_pass": read_visible_cached_ms,
            "cpu_resident_brick_mip": resident_render_ms,
            "cpu_dense_mip": dense_render_ms,
            "total": total_ms,
        },
        "gpu": gpu_stress,
        "resident_frame": {
            "output_pixels": resident_diagnostics.frame.output_pixels,
            "nonzero_pixels": resident_diagnostics.frame.nonzero_pixels,
            "max_value": resident_diagnostics.frame.max_value,
            "complete": resident_diagnostics.complete,
            "missing_voxel_samples": resident_diagnostics.missing_voxel_samples,
            "skip": brick_skip_json(resident_diagnostics.skip),
        },
        "dense_frame": {
            "output_pixels": dense_diagnostics.output_pixels,
            "nonzero_pixels": dense_diagnostics.nonzero_pixels,
            "max_value": dense_diagnostics.max_value,
        },
        "data_stats": {
            "subset_reads": stats.subset_reads,
            "decoded_values": stats.decoded_values,
            "volume_cache_hits": stats.volume_cache_hits,
            "volume_cache_misses": stats.volume_cache_misses,
            "brick_cache_hits": stats.brick_cache_hits,
            "brick_cache_misses": stats.brick_cache_misses,
            "brick_cache_u8_bytes": stats.brick_cache_u8_bytes,
            "brick_cache_u16_bytes": stats.brick_cache_u16_bytes,
            "brick_cache_f32_bytes": stats.brick_cache_f32_bytes,
            "brick_reads": stats.brick_reads,
            "decoded_brick_values": stats.decoded_brick_values,
            "encoded_payload_bytes_read": stats.encoded_payload_bytes_read,
            "encoded_shard_payloads_read": stats.encoded_shard_payloads_read,
            "shard_index_cache_hits": stats.shard_index_cache_hits,
            "shard_index_cache_misses": stats.shard_index_cache_misses,
            "shard_index_cache_entries": stats.shard_index_cache_entries,
            "brick_requests_queued": stats.brick_requests_queued,
            "brick_queue_full": stats.brick_queue_full,
        },
    });
    fs::write(
        &output_path,
        format!("{}\n", serde_json::to_string_pretty(&report_json)?),
    )
    .with_context(|| format!("failed to write {}", output_path.display()))?;
    Ok(output_path)
}

fn write_runtime_stress_package(
    package: &Path,
    shape: Shape4D,
    brick_shape: Shape4D,
) -> anyhow::Result<()> {
    let values_tzyx = runtime_stress_values(shape)?;
    write_native_u16_dataset(
        package,
        NativeU16Dataset {
            id: "runtime-stress-many-spatial-bricks".to_owned(),
            name: "Runtime stress many-spatial-brick synthetic dataset".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: WorldUnit::Micrometer,
            },
            layers: vec![DenseU16Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape,
                brick_shape,
                grid_to_world: grid_to_world_scale_um(0.2, 0.2, 0.2),
                display: default_u16_display(),
                values_tzyx,
            }],
        },
        ExistingPackagePolicy::Replace,
    )?;
    Ok(())
}

fn runtime_stress_values(shape: Shape4D) -> anyhow::Result<Vec<u16>> {
    let mut values = Vec::with_capacity(shape.element_count()? as usize);
    for t in 0..shape.t() {
        for z in 0..shape.z() {
            for y in 0..shape.y() {
                for x in 0..shape.x() {
                    values.push(runtime_stress_value(t, z, y, x));
                }
            }
        }
    }
    Ok(values)
}

fn runtime_stress_value(t: u64, z: u64, y: u64, x: u64) -> u16 {
    let brick_pattern = ((z / 8) ^ (y / 16) ^ (x / 16)) * 911;
    ((t * 8191 + z * 251 + y * 37 + x * 11 + brick_pattern) % u64::from(u16::MAX)) as u16
}

fn bench_worker_prefetch(
    dataset: DatasetHandle,
    layer_id: LayerId,
    scale_level: u32,
    timepoint_count: u64,
    visible_bricks: &[SpatialBrickIndex],
) -> anyhow::Result<Value> {
    let worker_count = env_u64("MIRANTE4D_BENCH_STRESS_WORKERS")?.unwrap_or(DEFAULT_STRESS_WORKERS);
    let queue_capacity = env_u64("MIRANTE4D_BENCH_STRESS_QUEUE_CAPACITY")?
        .unwrap_or((visible_bricks.len() as u64).saturating_mul(2).max(1));
    let pool = BrickReadPool::new(dataset, worker_count as usize, queue_capacity as usize)?;
    let generation = pool.advance_generation();
    let prefetch_timepoint = (timepoint_count > 1).then_some(TimeIndex::new(1));
    let mut submitted_current = 0usize;
    let mut submitted_prefetch = 0usize;
    for brick_index in visible_bricks {
        pool.submit_brick_at_scale(
            layer_id.clone(),
            scale_level,
            TimeIndex::new(0),
            *brick_index,
            BrickRequestPriority::CurrentFrame,
        )?;
        submitted_current += 1;
    }
    if let Some(prefetch_timepoint) = prefetch_timepoint {
        for brick_index in visible_bricks {
            pool.submit_brick_at_scale(
                layer_id.clone(),
                scale_level,
                prefetch_timepoint,
                *brick_index,
                BrickRequestPriority::Prefetch,
            )?;
            submitted_prefetch += 1;
        }
    }

    let expected = submitted_current + submitted_prefetch;
    let started = Instant::now();
    let mut received = 0usize;
    let mut completed_current = 0usize;
    let mut completed_prefetch = 0usize;
    let mut cancelled = 0usize;
    let mut stale = 0usize;
    let mut failed = Vec::new();
    while received < expected {
        let outcome = pool
            .recv_timeout(Duration::from_secs(30))
            .context("timed out waiting for benchmark worker brick read")?;
        received += 1;
        match outcome.status {
            BrickReadStatus::Completed(_) => match outcome.priority {
                BrickRequestPriority::CurrentFrame => completed_current += 1,
                BrickRequestPriority::Prefetch => completed_prefetch += 1,
                BrickRequestPriority::Warm => {}
            },
            BrickReadStatus::Cancelled => cancelled += 1,
            BrickReadStatus::Stale => stale += 1,
            BrickReadStatus::Failed(message) => failed.push(message),
        }
    }
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(json!({
        "generation": generation.0,
        "worker_count": worker_count,
        "queue_capacity": queue_capacity,
        "submitted_current": submitted_current,
        "submitted_prefetch": submitted_prefetch,
        "completed_current": completed_current,
        "completed_prefetch": completed_prefetch,
        "cancelled": cancelled,
        "stale": stale,
        "failed": failed,
        "timings_ms": {
            "worker_current_and_prefetch_reads": elapsed_ms,
        },
    }))
}

fn bench_gpu_runtime_stress(
    layer_id: &LayerId,
    volume: &DenseVolumeU16,
    resident_bricks: &[mirante4d_data::VolumeBrickU16],
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
    camera: CameraFrame,
    viewport: RenderViewport,
) -> anyhow::Result<Value> {
    if resident_bricks.len() < 2 {
        return Ok(json!({
            "available": false,
            "error": "runtime stress benchmark needs at least two resident bricks for atlas churn",
        }));
    }
    let set_size = env_u64("MIRANTE4D_BENCH_STRESS_GPU_SET_SIZE")?
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_STRESS_GPU_SET_SIZE)
        .max(2)
        .min(resident_bricks.len());
    let second_start = (set_size / 2).min(resident_bricks.len() - 1);
    let first = ResidentBrickSetU16::new(
        layer_id.clone(),
        TimeIndex::new(0),
        volume.shape,
        volume.grid_to_world,
        resident_bricks.iter().take(set_size).cloned().collect(),
    );
    let second = ResidentBrickSetU16::new(
        layer_id.clone(),
        TimeIndex::new(0),
        volume.shape,
        volume.grid_to_world,
        resident_bricks
            .iter()
            .skip(second_start)
            .take(set_size)
            .cloned()
            .collect(),
    );

    let init_started = Instant::now();
    let renderer = match new_native_benchmark_gpu_renderer() {
        Ok(renderer) => renderer,
        Err(err) => {
            return Ok(json!({
                "available": false,
                "error": err.to_string(),
            }));
        }
    };
    let init_ms = init_started.elapsed().as_secs_f64() * 1000.0;
    let adapter = renderer.adapter_diagnostics().clone();

    let first_started = Instant::now();
    renderer.render_camera_from_bricks(
        &first,
        brick_shape,
        brick_grid_shape,
        camera,
        viewport,
        CameraRenderMode::Mip,
    )?;
    let first_ms = first_started.elapsed().as_secs_f64() * 1000.0;

    let second_started = Instant::now();
    renderer.render_camera_from_bricks(
        &second,
        brick_shape,
        brick_grid_shape,
        camera,
        viewport,
        CameraRenderMode::Mip,
    )?;
    let second_ms = second_started.elapsed().as_secs_f64() * 1000.0;

    let first_again_started = Instant::now();
    renderer.render_camera_from_bricks(
        &first,
        brick_shape,
        brick_grid_shape,
        camera,
        viewport,
        CameraRenderMode::Mip,
    )?;
    let first_again_ms = first_again_started.elapsed().as_secs_f64() * 1000.0;
    let stats = renderer.stats()?;

    Ok(json!({
        "available": true,
        "adapter": adapter.name,
        "backend": adapter.backend,
        "device_type": adapter.device_type,
        "driver": adapter.driver,
        "driver_info": adapter.driver_info,
        "resident_set_size": set_size,
        "second_set_start": second_start,
        "timings_ms": {
            "init": init_ms,
            "resident_first_set_mip": first_ms,
            "resident_second_overlapping_set_mip": second_ms,
            "resident_first_set_again_mip": first_again_ms,
        },
        "stats": {
            "brick_atlas_cache_hits": stats.brick_atlas_cache_hits,
            "brick_atlas_cache_misses": stats.brick_atlas_cache_misses,
            "brick_atlas_uploads": stats.brick_atlas_uploads,
            "brick_atlas_uploaded_bytes": stats.brick_atlas_uploaded_bytes,
            "brick_atlas_u8_uploaded_bytes": stats.brick_atlas_u8_uploaded_bytes,
            "brick_atlas_u16_uploaded_bytes": stats.brick_atlas_u16_uploaded_bytes,
            "brick_atlas_f32_uploaded_bytes": stats.brick_atlas_f32_uploaded_bytes,
            "brick_atlas_evictions": stats.brick_atlas_evictions,
            "brick_atlas_resident_bytes": stats.brick_atlas_resident_bytes,
            "brick_atlas_u8_resident_bytes": stats.brick_atlas_u8_resident_bytes,
            "brick_atlas_u16_resident_bytes": stats.brick_atlas_u16_resident_bytes,
            "brick_atlas_f32_resident_bytes": stats.brick_atlas_f32_resident_bytes,
        },
    }))
}

fn bench_gpu_render_paths(
    volume: &DenseVolumeU16,
    resident: &ResidentBrickSetU16,
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
    camera: CameraFrame,
    viewport: RenderViewport,
) -> serde_json::Value {
    let started = Instant::now();
    let renderer = match new_native_benchmark_gpu_renderer() {
        Ok(renderer) => renderer,
        Err(err) => {
            return json!({
                "available": false,
                "error": err.to_string(),
            });
        }
    };
    let init_ms = started.elapsed().as_secs_f64() * 1000.0;
    let adapter = renderer.adapter_diagnostics().clone();
    let mut timings_ms = serde_json::Map::new();
    timings_ms.insert("init".to_owned(), json!(init_ms));

    let dense_first_started = Instant::now();
    let dense_first = renderer.render_camera(volume, camera, viewport, CameraRenderMode::Mip);
    let dense_first_ms = dense_first_started.elapsed().as_secs_f64() * 1000.0;
    timings_ms.insert("dense_first_mip".to_owned(), json!(dense_first_ms));
    let mut dense_frame = None;
    let mut dense_error = None;
    match dense_first {
        Ok(output) => {
            dense_frame = Some(json!({
                "output_pixels": output.frame.output_pixels,
                "nonzero_pixels": output.frame.nonzero_pixels,
                "max_value": output.frame.max_value,
            }));
            let dense_cached_started = Instant::now();
            let dense_cached =
                renderer.render_camera(volume, camera, viewport, CameraRenderMode::Mip);
            let dense_cached_ms = dense_cached_started.elapsed().as_secs_f64() * 1000.0;
            timings_ms.insert("dense_cached_mip".to_owned(), json!(dense_cached_ms));
            if let Err(err) = dense_cached {
                dense_error = Some(format!("cached dense render failed: {err}"));
            }
        }
        Err(err) => {
            dense_error = Some(err.to_string());
        }
    }

    let resident_first_started = Instant::now();
    let resident_first = renderer.render_camera_from_bricks(
        resident,
        brick_shape,
        brick_grid_shape,
        camera,
        viewport,
        CameraRenderMode::Mip,
    );
    let resident_first_ms = resident_first_started.elapsed().as_secs_f64() * 1000.0;
    timings_ms.insert("resident_first_mip".to_owned(), json!(resident_first_ms));
    let mut resident_error = None;
    let mut resident_frame = None;
    match resident_first {
        Ok(output) => {
            let brick_frame = output.brick_frame;
            resident_frame = Some(json!({
                "output_pixels": output.frame.output_pixels,
                "nonzero_pixels": output.frame.nonzero_pixels,
                "max_value": output.frame.max_value,
                "complete": brick_frame.map(|diagnostics| diagnostics.complete),
                "skip": brick_frame.map(|diagnostics| brick_skip_json(diagnostics.skip)),
            }));
            let resident_cached_started = Instant::now();
            let resident_cached = renderer.render_camera_from_bricks(
                resident,
                brick_shape,
                brick_grid_shape,
                camera,
                viewport,
                CameraRenderMode::Mip,
            );
            let resident_cached_ms = resident_cached_started.elapsed().as_secs_f64() * 1000.0;
            timings_ms.insert("resident_cached_mip".to_owned(), json!(resident_cached_ms));
            if let Err(err) = resident_cached {
                resident_error = Some(format!("cached resident render failed: {err}"));
            }
        }
        Err(err) => {
            resident_error = Some(err.to_string());
        }
    }

    let display_transfer = benchmark_display_transfer(f32::from(u16::MAX));
    let resident_display_first_started = Instant::now();
    let resident_display_first = renderer.render_resident_channels_to_display_texture(
        &[GpuResidentDisplayChannel::U16 {
            resident,
            brick_shape,
            brick_grid_shape,
            mode: CameraRenderMode::Mip,
            transfer: display_transfer,
        }],
        display_request_for_camera(camera, viewport, CameraRenderQuality::voxel_exact()),
    );
    let resident_display_first_ms = resident_display_first_started.elapsed().as_secs_f64() * 1000.0;
    timings_ms.insert(
        "resident_display_first_mip".to_owned(),
        json!(resident_display_first_ms),
    );
    let mut resident_display_error = None;
    let mut resident_display_frame = None;
    match resident_display_first {
        Ok(frame) => {
            resident_display_frame = Some(gpu_display_frame_json(&frame));
            let resident_display_cached_started = Instant::now();
            let resident_display_cached = renderer.render_resident_channels_to_display_texture(
                &[GpuResidentDisplayChannel::U16 {
                    resident,
                    brick_shape,
                    brick_grid_shape,
                    mode: CameraRenderMode::Mip,
                    transfer: display_transfer,
                }],
                display_request_for_camera(camera, viewport, CameraRenderQuality::voxel_exact()),
            );
            let resident_display_cached_ms =
                resident_display_cached_started.elapsed().as_secs_f64() * 1000.0;
            timings_ms.insert(
                "resident_display_cached_mip".to_owned(),
                json!(resident_display_cached_ms),
            );
            if let Err(err) = resident_display_cached {
                resident_display_error =
                    Some(format!("cached resident display render failed: {err}"));
            }
        }
        Err(err) => {
            resident_display_error = Some(err.to_string());
        }
    }
    if dense_frame.is_none() && resident_frame.is_none() && resident_display_frame.is_none() {
        return json!({
            "available": true,
            "adapter": adapter.name,
            "backend": adapter.backend,
            "device_type": adapter.device_type,
            "driver": adapter.driver,
            "driver_info": adapter.driver_info,
            "timings_ms": Value::Object(timings_ms),
            "dense_error": dense_error,
            "resident_error": resident_error,
            "resident_display_error": resident_display_error,
        });
    }
    let stats = renderer.stats().ok();

    json!({
        "available": true,
        "adapter": adapter.name,
        "backend": adapter.backend,
        "device_type": adapter.device_type,
        "driver": adapter.driver,
        "driver_info": adapter.driver_info,
        "timings_ms": Value::Object(timings_ms),
        "dense_error": dense_error,
        "resident_error": resident_error,
        "resident_display_error": resident_display_error,
        "frame": dense_frame,
        "resident_frame": resident_frame,
        "resident_display_frame": resident_display_frame,
        "stats": stats.map(gpu_stats_json),
    })
}

fn new_native_benchmark_gpu_renderer() -> anyhow::Result<GpuRenderer> {
    GpuRenderer::new_with_cache_budgets_blocking(
        phase11_gpu_volume_cache_budget_bytes()?,
        phase11_gpu_brick_cache_budget_bytes()?,
    )
    .map_err(Into::into)
}

fn bench_gpu_render_paths_u8(
    resident: &ResidentBrickSetU8,
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
    camera: CameraFrame,
    viewport: RenderViewport,
) -> serde_json::Value {
    let started = Instant::now();
    let renderer = match new_native_benchmark_gpu_renderer() {
        Ok(renderer) => renderer,
        Err(err) => {
            return json!({
                "available": false,
                "error": err.to_string(),
            });
        }
    };
    let init_ms = started.elapsed().as_secs_f64() * 1000.0;
    let adapter = renderer.adapter_diagnostics().clone();
    let mut timings_ms = serde_json::Map::new();
    timings_ms.insert("init".to_owned(), json!(init_ms));

    let resident_first_started = Instant::now();
    let resident_first = renderer.render_camera_u8_from_bricks(
        resident,
        brick_shape,
        brick_grid_shape,
        camera,
        viewport,
        CameraRenderMode::Mip,
    );
    let resident_first_ms = resident_first_started.elapsed().as_secs_f64() * 1000.0;
    timings_ms.insert("resident_first_mip".to_owned(), json!(resident_first_ms));
    let mut resident_error = None;
    let mut resident_frame = None;
    match resident_first {
        Ok(output) => {
            let brick_frame = output.brick_frame;
            resident_frame = Some(json!({
                "output_pixels": output.frame.output_pixels,
                "nonzero_pixels": output.frame.nonzero_pixels,
                "max_value": output.frame.max_value,
                "complete": brick_frame.map(|diagnostics| diagnostics.complete),
                "skip": brick_frame.map(|diagnostics| brick_skip_json(diagnostics.skip)),
            }));
            let resident_cached_started = Instant::now();
            let resident_cached = renderer.render_camera_u8_from_bricks(
                resident,
                brick_shape,
                brick_grid_shape,
                camera,
                viewport,
                CameraRenderMode::Mip,
            );
            let resident_cached_ms = resident_cached_started.elapsed().as_secs_f64() * 1000.0;
            timings_ms.insert("resident_cached_mip".to_owned(), json!(resident_cached_ms));
            if let Err(err) = resident_cached {
                resident_error = Some(format!("cached resident render failed: {err}"));
            }
        }
        Err(err) => {
            resident_error = Some(err.to_string());
        }
    }

    let display_transfer = benchmark_display_transfer(f32::from(u8::MAX));
    let resident_display_first_started = Instant::now();
    let resident_display_first = renderer.render_resident_channels_to_display_texture(
        &[GpuResidentDisplayChannel::U8 {
            resident,
            brick_shape,
            brick_grid_shape,
            mode: CameraRenderMode::Mip,
            transfer: display_transfer,
        }],
        display_request_for_camera(camera, viewport, CameraRenderQuality::voxel_exact()),
    );
    let resident_display_first_ms = resident_display_first_started.elapsed().as_secs_f64() * 1000.0;
    timings_ms.insert(
        "resident_display_first_mip".to_owned(),
        json!(resident_display_first_ms),
    );
    let mut resident_display_error = None;
    let mut resident_display_frame = None;
    match resident_display_first {
        Ok(frame) => {
            resident_display_frame = Some(gpu_display_frame_json(&frame));
            let resident_display_cached_started = Instant::now();
            let resident_display_cached = renderer.render_resident_channels_to_display_texture(
                &[GpuResidentDisplayChannel::U8 {
                    resident,
                    brick_shape,
                    brick_grid_shape,
                    mode: CameraRenderMode::Mip,
                    transfer: display_transfer,
                }],
                display_request_for_camera(camera, viewport, CameraRenderQuality::voxel_exact()),
            );
            let resident_display_cached_ms =
                resident_display_cached_started.elapsed().as_secs_f64() * 1000.0;
            timings_ms.insert(
                "resident_display_cached_mip".to_owned(),
                json!(resident_display_cached_ms),
            );
            if let Err(err) = resident_display_cached {
                resident_display_error =
                    Some(format!("cached resident display render failed: {err}"));
            }
        }
        Err(err) => {
            resident_display_error = Some(err.to_string());
        }
    }
    let stats = renderer.stats().ok();

    json!({
        "available": true,
        "adapter": adapter.name,
        "backend": adapter.backend,
        "device_type": adapter.device_type,
        "driver": adapter.driver,
        "driver_info": adapter.driver_info,
        "timings_ms": Value::Object(timings_ms),
        "dense_error": "dense_gpu_u8_not_supported",
        "resident_error": resident_error,
        "resident_display_error": resident_display_error,
        "frame": Value::Null,
        "resident_frame": resident_frame,
        "resident_display_frame": resident_display_frame,
        "stats": stats.map(gpu_stats_json),
    })
}

fn benchmark_display_transfer(window_high: f32) -> IntensityTransfer {
    IntensityTransfer::new(
        true,
        LayerTransfer::new(
            DisplayWindow::new(0.0, window_high).expect("benchmark display window is valid"),
            RgbColor::new([1.0, 1.0, 1.0]).expect("benchmark display color is valid"),
            Opacity::new(1.0).expect("benchmark display opacity is valid"),
            TransferCurve::linear(),
            false,
        ),
    )
}

fn camera_axes(camera: CameraFrame) -> CameraAxes {
    camera.axes()
}

fn display_request_for_camera(
    camera: CameraFrame,
    viewport: RenderViewport,
    quality: CameraRenderQuality,
) -> GpuResidentDisplayRequest {
    let axes = camera_axes(camera);
    GpuResidentDisplayRequest {
        camera,
        viewport,
        quality,
        iso_light_state: IsoLightState::default(),
        camera_axes: axes,
    }
}

fn gpu_display_frame_json(frame: &GpuDisplayFrame) -> Value {
    json!({
        "viewport": {
            "width": frame.viewport.width,
            "height": frame.viewport.height,
        },
        "diagnostics": {
            "channels": frame.diagnostics.channels,
            "output_bytes": frame.diagnostics.output_bytes,
            "accumulator_bytes": frame.diagnostics.accumulator_bytes,
            "texture_bytes": frame.diagnostics.texture_bytes,
            "draw_calls": frame.diagnostics.draw_calls,
            "vertex_count": frame.diagnostics.vertex_count,
        },
        "timings_ms": {
            "upload": frame.timings.upload_ms(),
            "gpu_compute": frame.timings.gpu_compute_ms(),
        },
    })
}

fn brick_skip_json(diagnostics: BrickSkipDiagnostics) -> Value {
    json!({
        "skipped_brick_intervals": diagnostics.skipped_brick_intervals,
        "empty_brick_intervals": diagnostics.empty_brick_intervals,
        "mip_range_intervals": diagnostics.mip_range_intervals,
        "iso_range_intervals": diagnostics.iso_range_intervals,
        "dvr_range_intervals": diagnostics.dvr_range_intervals,
    })
}

fn benchmark_viewport_for_volume(volume: &DenseVolumeU16) -> anyhow::Result<RenderViewport> {
    benchmark_viewport_for_shape_with_overrides(
        volume.shape,
        NativePackageBenchmarkOverrides::default(),
    )
}

fn benchmark_viewport_for_volume_with_overrides(
    volume: &DenseVolumeU16,
    overrides: NativePackageBenchmarkOverrides,
) -> anyhow::Result<RenderViewport> {
    benchmark_viewport_for_shape_with_overrides(volume.shape, overrides)
}

fn benchmark_viewport_for_shape_with_overrides(
    shape: Shape3D,
    overrides: NativePackageBenchmarkOverrides,
) -> anyhow::Result<RenderViewport> {
    let width = overrides
        .viewport_width
        .or(env_u64("MIRANTE4D_BENCH_VIEWPORT_WIDTH")?)
        .unwrap_or(shape.x().min(1024));
    let height = overrides
        .viewport_height
        .or(env_u64("MIRANTE4D_BENCH_VIEWPORT_HEIGHT")?)
        .unwrap_or(shape.y().min(1024));
    Ok(RenderViewport::new(width, height)?)
}

fn benchmark_brick_pixel_stride_with_overrides(
    overrides: NativePackageBenchmarkOverrides,
) -> anyhow::Result<u64> {
    Ok(overrides
        .brick_pixel_stride
        .or(env_u64("MIRANTE4D_BENCH_BRICK_PIXEL_STRIDE")?)
        .unwrap_or(16)
        .max(1))
}

fn runtime_stress_brick_pixel_stride() -> anyhow::Result<u64> {
    Ok(env_u64("MIRANTE4D_BENCH_STRESS_BRICK_PIXEL_STRIDE")?
        .or(env_u64("MIRANTE4D_BENCH_BRICK_PIXEL_STRIDE")?)
        .unwrap_or(1)
        .max(1))
}

fn runtime_stress_shape() -> anyhow::Result<Shape4D> {
    Shape4D::new(
        env_u64("MIRANTE4D_BENCH_STRESS_T")?.unwrap_or(DEFAULT_STRESS_T),
        env_u64("MIRANTE4D_BENCH_STRESS_Z")?.unwrap_or(DEFAULT_STRESS_Z),
        env_u64("MIRANTE4D_BENCH_STRESS_Y")?.unwrap_or(DEFAULT_STRESS_Y),
        env_u64("MIRANTE4D_BENCH_STRESS_X")?.unwrap_or(DEFAULT_STRESS_X),
    )
    .map_err(Into::into)
}

fn runtime_stress_chunk_shape() -> anyhow::Result<Shape4D> {
    Shape4D::new(
        1,
        env_u64("MIRANTE4D_BENCH_STRESS_BRICK_Z")?.unwrap_or(DEFAULT_STRESS_BRICK_Z),
        env_u64("MIRANTE4D_BENCH_STRESS_BRICK_Y")?.unwrap_or(DEFAULT_STRESS_BRICK_Y),
        env_u64("MIRANTE4D_BENCH_STRESS_BRICK_X")?.unwrap_or(DEFAULT_STRESS_BRICK_X),
    )
    .map_err(Into::into)
}
