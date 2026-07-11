use std::{collections::BTreeMap, env, fs, path::PathBuf, time::Instant};

use anyhow::{Context, bail};
use mirante4d_data::{DatasetHandle, SpatialBrickIndex};
use mirante4d_domain::{IntensityDType, Shape3D, TimeIndex};
use mirante4d_format::{ExistingPackagePolicy, LayerId, NativeManifest, validate::load_manifest};
use mirante4d_import::{
    ImportCancellationToken, ImportProgressEvent, TiffImportSource, TiffSourceImportOptions,
    TiffSourceProfile, accepted_tiff_reviewed_import_plan, import_tiff_source_with_progress,
    inspect_tiff_source_for_review, inspect_tiff_source_with_grouping,
};
use serde_json::{Value, json};

use super::{benchmark_sample_source, sample_import_file_limit};
use crate::host::{benchmark_baseline_class, benchmark_hardware_class, benchmark_host_context};
use crate::stable_id_from_name;

pub(crate) fn bench_import_sample(experiment: &str) -> anyhow::Result<PathBuf> {
    bench_import_sample_with_limit(experiment, sample_import_file_limit()?)
}

pub(crate) fn bench_import_sample_with_limit(
    experiment: &str,
    file_limit: usize,
) -> anyhow::Result<PathBuf> {
    let sample_root = env::var_os("MIRANTE4D_SAMPLE_DATA")
        .context("MIRANTE4D_SAMPLE_DATA must point to the local sample_data root")?;
    let input_dir = PathBuf::from(sample_root).join(experiment);
    if !input_dir.is_dir() {
        bail!(
            "sample experiment directory does not exist: {}",
            input_dir.display()
        );
    }

    let output_root = PathBuf::from("target")
        .join("mirante4d")
        .join("benchmarks")
        .join("import-sample");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let dataset_id = stable_id_from_name(experiment);
    let output_package = output_root.join(format!("{dataset_id}.m4d"));
    let benchmark_source =
        benchmark_sample_source(&input_dir, &output_root, &dataset_id, file_limit)?;

    let inspect_started = Instant::now();
    let reviewed_inspection = inspect_tiff_source_for_review(&benchmark_source.source)?;
    let file_grouping = if reviewed_inspection.source_profile == TiffSourceProfile::StackSeriesMovie
    {
        Some(reviewed_inspection.files.clone())
    } else {
        None
    };
    let inspection = if let Some(file_grouping) = &file_grouping {
        inspect_tiff_source_with_grouping(&benchmark_source.source, file_grouping)?
    } else {
        reviewed_inspection.clone()
    };
    let inspect_ms = inspect_started.elapsed().as_secs_f64() * 1000.0;

    let mut discovered_files = 0usize;
    let mut estimated_total_bytes = None;
    let mut estimated_peak_stack_bytes = None;
    let mut read_stack_events = 0usize;
    let mut built_scale_events = 0usize;
    let mut writing_events = 0usize;
    let cancellation = ImportCancellationToken::new();
    let import_started = Instant::now();
    let report = import_tiff_source_with_progress(
        TiffSourceImportOptions {
            source: benchmark_source.source.clone(),
            output_package: output_package.clone(),
            dataset_id: dataset_id.clone(),
            dataset_name: experiment.to_owned(),
            voxel_spacing_um: [1.0, 1.0, 1.0],
            channel_metadata: BTreeMap::new(),
            file_grouping,
            existing_policy: ExistingPackagePolicy::Replace,
            storage: Default::default(),
            reviewed_plan: accepted_tiff_reviewed_import_plan(&inspection, [1.0, 1.0, 1.0], true),
        },
        &cancellation,
        |event| {
            match event {
                ImportProgressEvent::DiscoveredInput { file_count } => {
                    discovered_files = file_count;
                }
                ImportProgressEvent::EstimatedStorage { estimate } => {
                    estimated_total_bytes = Some(estimate.estimated_total_bytes);
                    estimated_peak_stack_bytes = Some(estimate.peak_working_stack_bytes);
                }
                ImportProgressEvent::ReadStack { .. } => {
                    read_stack_events += 1;
                }
                ImportProgressEvent::BuiltScale { .. } => {
                    built_scale_events += 1;
                }
                ImportProgressEvent::WritingPackage { .. } => {
                    writing_events += 1;
                }
                ImportProgressEvent::Finished { .. } => {}
            }
            Ok(())
        },
    )?;
    let import_ms = import_started.elapsed().as_secs_f64() * 1000.0;

    let open_started = Instant::now();
    let dataset = DatasetHandle::open(&report.output_package)?;
    let manifest = load_manifest(&report.output_package)?;
    let layer_id = dataset.first_layer_id()?;
    let first_volume_shape = first_layer_shape_from_manifest(&manifest, &layer_id)?;
    let first_brick_probe = read_first_brick_probe(&dataset, &manifest, &layer_id)?;
    let open_and_read_ms = open_started.elapsed().as_secs_f64() * 1000.0;
    let stats = dataset.stats()?;

    let output_path = output_root.join(format!("bench-import-sample-{dataset_id}.json"));
    let report_json = json!({
        "benchmark": "bench-import-sample",
        "benchmark_schema_version": 1,
        "baseline_class": benchmark_baseline_class(),
        "hardware_class": benchmark_hardware_class(),
        "dataset_class": "bounded_real_sample_import",
        "hardware": benchmark_host_context(),
        "experiment": experiment,
        "input_dir": input_dir,
        "benchmark_source": {
            "kind": match &benchmark_source.source {
                TiffImportSource::Directory(_) => "directory",
                TiffImportSource::SingleFile(_) => "single_file",
            },
            "path": benchmark_source.source.path(),
            "selection_reason": benchmark_source.selection_reason,
            "source_file_count": benchmark_source.source_file_count,
            "selected_file_count": benchmark_source.selected_file_count,
            "file_limit": benchmark_source.file_limit,
            "source_profile": format!("{:?}", inspection.source_profile),
            "reviewed_grouping": inspection.source_profile == TiffSourceProfile::StackSeriesMovie,
        },
        "output_package": report.output_package,
        "inspection": {
            "file_count": inspection.file_count,
            "channel_count": inspection.channel_count,
            "timepoint_count": inspection.timepoint_count,
            "shape": {
                "z": inspection.shape.z,
                "y": inspection.shape.y,
                "x": inspection.shape.x,
            },
            "source_dtype": format!("{:?}", inspection.source_dtype),
            "source_profile": format!("{:?}", inspection.source_profile),
            "metadata_confidence": format!("{:?}", inspection.metadata_confidence),
            "voxel_spacing_status": format!("{:?}", inspection.source_metadata.voxel_spacing_status),
            "voxel_spacing_um": inspection.source_metadata.voxel_spacing_um,
            "value_range": {
                "min": inspection.value_range.min,
                "max": inspection.value_range.max,
            },
        },
        "review": {
            "native_axes": manifest.provenance.source_metadata.as_ref().map(|metadata| metadata.native_axes.clone()),
            "channels_as_layers": manifest.provenance.source_metadata.as_ref().map(|metadata| metadata.channels_as_layers),
            "user_corrections": manifest.provenance.user_corrections.clone(),
        },
        "provenance": {
            "kind": format!("{:?}", manifest.provenance.kind),
            "source_format": manifest.provenance.source_format.clone(),
            "source_file_count": manifest.provenance.source_files.len(),
            "source_fingerprints_recorded": manifest.provenance.source_files.iter().all(|file| file.fingerprint_blake3.is_some()),
            "storage_policy": manifest.provenance.storage_policy.clone(),
            "checksum_policy": manifest.provenance.checksum_policy.clone(),
            "conversion_policy": manifest.provenance.conversion_policy.clone(),
        },
        "import_report": {
            "channel_count": report.channel_count,
            "timepoint_count": report.timepoint_count,
            "scale_count": report.scale_count,
            "z_planes": report.z_planes,
            "height": report.height,
            "width": report.width,
        },
        "progress_events": {
            "discovered_files": discovered_files,
            "estimated_total_bytes": estimated_total_bytes,
            "estimated_peak_stack_bytes": estimated_peak_stack_bytes,
            "read_stack": read_stack_events,
            "built_scale": built_scale_events,
            "writing": writing_events,
        },
        "timings_ms": {
            "inspect": inspect_ms,
            "import": import_ms,
            "open_and_read_first_volume": open_and_read_ms,
        },
        "first_volume": {
            "layer_id": layer_id.to_string(),
            "shape": {
                "z": first_volume_shape.z(),
                "y": first_volume_shape.y(),
                "x": first_volume_shape.x(),
            },
            "first_brick_probe": first_brick_probe,
        },
        "data_stats": {
            "subset_reads": stats.subset_reads,
            "decoded_values": stats.decoded_values,
            "volume_cache_hits": stats.volume_cache_hits,
            "volume_cache_misses": stats.volume_cache_misses,
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

fn first_layer_shape_from_manifest(
    manifest: &NativeManifest,
    layer_id: &LayerId,
) -> anyhow::Result<Shape3D> {
    let layer = manifest
        .layers
        .iter()
        .find(|layer| layer.id == layer_id.as_str())
        .with_context(|| format!("manifest missing first layer {}", layer_id.as_str()))?;
    Ok(Shape3D::new(
        layer.shape.z(),
        layer.shape.y(),
        layer.shape.x(),
    )?)
}

fn first_layer_dtype_from_manifest(
    manifest: &NativeManifest,
    layer_id: &LayerId,
) -> anyhow::Result<IntensityDType> {
    let layer = manifest
        .layers
        .iter()
        .find(|layer| layer.id == layer_id.as_str())
        .with_context(|| format!("manifest missing first layer {}", layer_id.as_str()))?;
    Ok(layer.dtype.stored)
}

fn read_first_brick_probe(
    dataset: &DatasetHandle,
    manifest: &NativeManifest,
    layer_id: &LayerId,
) -> anyhow::Result<Value> {
    match first_layer_dtype_from_manifest(manifest, layer_id)? {
        IntensityDType::Uint8 => {
            let brick = dataset.read_u8_brick_at_scale(
                layer_id,
                0,
                TimeIndex::new(0),
                SpatialBrickIndex::new(0, 0, 0),
            )?;
            Ok(json!({
                "dtype_read_path": "u8",
                "scale_level": brick.scale_level,
                "brick_index": {
                    "z": brick.brick_index.z,
                    "y": brick.brick_index.y,
                    "x": brick.brick_index.x,
                },
                "region": {
                    "z_start": brick.region.z_start,
                    "y_start": brick.region.y_start,
                    "x_start": brick.region.x_start,
                    "z_size": brick.region.z_size,
                    "y_size": brick.region.y_size,
                    "x_size": brick.region.x_size,
                },
                "occupied": brick.occupied,
                "value_count": brick.volume.values().len(),
            }))
        }
        IntensityDType::Uint16 => {
            let brick = dataset.read_u16_brick_at_scale(
                layer_id,
                0,
                TimeIndex::new(0),
                SpatialBrickIndex::new(0, 0, 0),
            )?;
            Ok(json!({
                "dtype_read_path": "u16",
                "scale_level": brick.scale_level,
                "brick_index": {
                    "z": brick.brick_index.z,
                    "y": brick.brick_index.y,
                    "x": brick.brick_index.x,
                },
                "region": {
                    "z_start": brick.region.z_start,
                    "y_start": brick.region.y_start,
                    "x_start": brick.region.x_start,
                    "z_size": brick.region.z_size,
                    "y_size": brick.region.y_size,
                    "x_size": brick.region.x_size,
                },
                "occupied": brick.occupied,
                "value_count": brick.volume.values().len(),
            }))
        }
        IntensityDType::Float32 => {
            let brick = dataset.read_f32_brick_at_scale(
                layer_id,
                0,
                TimeIndex::new(0),
                SpatialBrickIndex::new(0, 0, 0),
            )?;
            Ok(json!({
                "dtype_read_path": "f32",
                "scale_level": brick.scale_level,
                "brick_index": {
                    "z": brick.brick_index.z,
                    "y": brick.brick_index.y,
                    "x": brick.brick_index.x,
                },
                "region": {
                    "z_start": brick.region.z_start,
                    "y_start": brick.region.y_start,
                    "x_start": brick.region.x_start,
                    "z_size": brick.region.z_size,
                    "y_size": brick.region.y_size,
                    "x_size": brick.region.x_size,
                },
                "occupied": brick.occupied,
                "value_count": brick.volume.values().len(),
            }))
        }
    }
}
