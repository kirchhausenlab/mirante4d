use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, bail};
use mirante4d_core::{IntensityDType, LayerId, Shape3D, TimeIndex};
use mirante4d_data::{DatasetHandle, SpatialBrickIndex};
use mirante4d_format::validate::load_manifest;
use mirante4d_format::{ExistingPackagePolicy, NativeManifest};
use mirante4d_import::{
    ImportCancellationToken, TiffImportSource, TiffImportStorageOptions, TiffNoDataPolicyReview,
    TiffSourceImportOptions, TiffSourceProfile, accepted_tiff_reviewed_import_plan,
    import_tiff_source_with_progress, inspect_tiff_source_for_review,
    inspect_tiff_source_with_grouping,
};
use serde_json::{Value, json};
use tiff::encoder::{TiffEncoder, colortype};

use crate::bench::{
    Phase13RendererBenchmarkOptions, list_direct_child_dirs, list_direct_tiff_files,
    phase13_renderer_report, tiff_has_multiple_images,
};
use crate::host::benchmark_host_context;
use crate::smoke::{ReleaseAppSmokeOptions, app_smoke_with_options};

pub(crate) fn phase20_smoke_audit() -> anyhow::Result<PathBuf> {
    let output_root = phase20_output_root();
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let source_root = output_root.join("smoke-sources");
    if source_root.exists() {
        fs::remove_dir_all(&source_root)
            .with_context(|| format!("failed to remove {}", source_root.display()))?;
    }
    fs::create_dir_all(&source_root)
        .with_context(|| format!("failed to create {}", source_root.display()))?;

    let stack_source = source_root.join("stack-series");
    fs::create_dir_all(&stack_source)
        .with_context(|| format!("failed to create {}", stack_source.display()))?;
    for timepoint in 0..3 {
        write_phase20_u16_stack(
            &stack_source.join(format!("timepoint-{timepoint:03}.tif")),
            timepoint,
        )?;
    }

    let plane_source = source_root.join("plane-series");
    for channel in ["alpha", "beta"] {
        fs::create_dir_all(plane_source.join(channel)).with_context(|| {
            format!("failed to create {}", plane_source.join(channel).display())
        })?;
    }
    for (channel_name, channel_base) in [("alpha", 10_u8), ("beta", 110_u8)] {
        for (name, z_offset) in [("a.tif", 0_u8), ("b.tif", 10_u8), ("c.tif", 20_u8)] {
            write_phase20_u8_plane(
                &plane_source.join(channel_name).join(name),
                channel_base + z_offset,
            )?;
        }
    }

    let stack_evidence = phase20_import_source_evidence(
        TiffImportSource::Directory(stack_source),
        output_root.join("phase20-smoke-stack.m4d"),
        "phase20-smoke-stack",
        "Phase 20 Smoke Stack-Series",
        [1.0, 1.0, 1.0],
        None,
        TiffImportStorageOptions::default(),
        true,
    )?;
    let plane_evidence = phase20_import_source_evidence(
        TiffImportSource::Directory(plane_source),
        output_root.join("phase20-smoke-plane.m4d"),
        "phase20-smoke-plane",
        "Phase 20 Smoke Plane-Series",
        [0.001, 0.001, 0.001],
        None,
        TiffImportStorageOptions::default(),
        true,
    )?;

    let report_json = json!({
        "audit": "phase20-smoke-audit",
        "audit_schema_version": 1,
        "phase": "Phase 20: Extreme Dataset Import",
        "hardware": benchmark_host_context(),
        "stack_series": stack_evidence,
        "plane_series": plane_evidence,
    });
    let report_json_path = output_root.join("phase20-smoke-audit-report.json");
    let report_md_path = output_root.join("phase20-smoke-audit-report.md");
    fs::write(
        &report_json_path,
        format!("{}\n", serde_json::to_string_pretty(&report_json)?),
    )
    .with_context(|| format!("failed to write {}", report_json_path.display()))?;
    fs::write(&report_md_path, phase20_audit_markdown(&report_json))
        .with_context(|| format!("failed to write {}", report_md_path.display()))?;
    Ok(report_md_path)
}

pub(crate) fn phase20_extreme_audit() -> anyhow::Result<PathBuf> {
    let sample_root = env::var_os("MIRANTE4D_SAMPLE_DATA")
        .map(PathBuf::from)
        .context("MIRANTE4D_SAMPLE_DATA must point to the local sample_data root")?;
    let output_root = phase20_output_root();
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;

    let t5_qual_002_source = phase20_real_sample_source(&sample_root, "T5-QUAL-002")?;
    let t5_qual_001_source = phase20_real_sample_source(&sample_root, "T5-QUAL-001")?;
    let t5_qual_002_evidence = phase20_import_source_evidence(
        t5_qual_002_source,
        output_root.join("phase20-extreme-t5-qual-002.m4d"),
        "phase20-extreme-t5-qual-002",
        "Phase 20 Extreme T5-QUAL-002",
        [1.0, 1.0, 1.0],
        None,
        TiffImportStorageOptions::default(),
        true,
    )?;
    let t5_qual_001_evidence = phase20_import_source_evidence(
        t5_qual_001_source,
        output_root.join("phase20-extreme-T5-QUAL-001.m4d"),
        "phase20-extreme-T5-QUAL-001",
        "Phase 20 Extreme T5-QUAL-001",
        [0.001, 0.001, 0.001],
        Some(TiffNoDataPolicyReview {
            source_dtype: IntensityDType::Uint8,
            source_value_uint8: 255,
        }),
        TiffImportStorageOptions::default(),
        true,
    )?;

    let report_json = json!({
        "audit": "phase20-extreme-audit",
        "audit_schema_version": 1,
        "phase": "Phase 20: Extreme Dataset Import",
        "hardware": benchmark_host_context(),
        "T5-QUAL-002": t5_qual_002_evidence,
        "T5-QUAL-001": t5_qual_001_evidence,
    });
    let report_json_path = output_root.join("phase20-extreme-audit-report.json");
    let report_md_path = output_root.join("phase20-extreme-audit-report.md");
    fs::write(
        &report_json_path,
        format!("{}\n", serde_json::to_string_pretty(&report_json)?),
    )
    .with_context(|| format!("failed to write {}", report_json_path.display()))?;
    fs::write(&report_md_path, phase20_audit_markdown(&report_json))
        .with_context(|| format!("failed to write {}", report_md_path.display()))?;
    Ok(report_md_path)
}

pub(crate) fn phase20_extreme_sample_audit(experiment: &str) -> anyhow::Result<PathBuf> {
    let sample_root = env::var_os("MIRANTE4D_SAMPLE_DATA")
        .map(PathBuf::from)
        .context("MIRANTE4D_SAMPLE_DATA must point to the local sample_data root")?;
    let output_root = phase20_output_root();
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;

    let experiment = experiment.to_ascii_uppercase();
    let default_t5_qual_001_no_data_policy = Some(TiffNoDataPolicyReview {
        source_dtype: IntensityDType::Uint8,
        source_value_uint8: 255,
    });
    let (source_experiment, dataset_id, dataset_name, voxel_spacing_um, no_data_policy, storage) =
        match experiment.as_str() {
            "T5-QUAL-002" => (
                "T5-QUAL-002",
                "phase20-extreme-t5-qual-002".to_owned(),
                "Phase 20 Extreme T5-QUAL-002",
                [1.0, 1.0, 1.0],
                None,
                TiffImportStorageOptions::default(),
            ),
            "T5-QUAL-001" => (
                "T5-QUAL-001",
                "phase20-extreme-T5-QUAL-001".to_owned(),
                "Phase 20 Extreme T5-QUAL-001",
                [0.001, 0.001, 0.001],
                default_t5_qual_001_no_data_policy,
                TiffImportStorageOptions::default(),
            ),
            "T5-QUAL-003" => (
                "T5-QUAL-003",
                "phase20-extreme-t5-qual-003".to_owned(),
                "Phase 20 Extreme T5-QUAL-003",
                [0.001, 0.001, 0.001],
                default_t5_qual_001_no_data_policy,
                TiffImportStorageOptions {
                    brick_shape_zyx: Some(Shape3D::new(16, 256, 256)?),
                },
            ),
            other => bail!(
                "unsupported Phase 20 qualification input {other:?}; expected T5-QUAL-001, T5-QUAL-002, or T5-QUAL-003"
            ),
        };
    let source = phase20_real_sample_source(&sample_root, source_experiment)?;
    let evidence = phase20_import_source_evidence(
        source,
        output_root.join(format!("{dataset_id}.m4d")),
        dataset_id.as_str(),
        dataset_name,
        voxel_spacing_um,
        no_data_policy,
        storage,
        true,
    )?;

    let report_json = match experiment.as_str() {
        "T5-QUAL-002" => json!({
            "audit": "phase20-extreme-sample-audit",
            "audit_schema_version": 1,
            "phase": "Phase 20: Extreme Dataset Import",
            "hardware": benchmark_host_context(),
            "T5-QUAL-002": evidence,
        }),
        "T5-QUAL-001" => json!({
            "audit": "phase20-extreme-sample-audit",
            "audit_schema_version": 1,
            "phase": "Phase 20: Extreme Dataset Import",
            "hardware": benchmark_host_context(),
            "T5-QUAL-001": evidence,
        }),
        "T5-QUAL-003" => json!({
            "audit": "phase20-extreme-sample-audit",
            "audit_schema_version": 1,
            "phase": "Phase 20: Extreme Dataset Import",
            "hardware": benchmark_host_context(),
            "T5-QUAL-003": evidence,
        }),
        _ => unreachable!("unsupported sample was rejected above"),
    };
    let report_stem = dataset_id.trim_end_matches(".m4d");
    let report_json_path = output_root.join(format!("{report_stem}-audit-report.json"));
    let report_md_path = output_root.join(format!("{report_stem}-audit-report.md"));
    fs::write(
        &report_json_path,
        format!("{}\n", serde_json::to_string_pretty(&report_json)?),
    )
    .with_context(|| format!("failed to write {}", report_json_path.display()))?;
    fs::write(&report_md_path, phase20_audit_markdown(&report_json))
        .with_context(|| format!("failed to write {}", report_md_path.display()))?;
    Ok(report_md_path)
}

fn phase20_output_root() -> PathBuf {
    env::var_os("MIRANTE4D_PHASE20_OUTPUT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target").join("mirante4d").join("phase20"))
}

fn phase20_real_sample_source(
    sample_root: &Path,
    experiment: &str,
) -> anyhow::Result<TiffImportSource> {
    let experiment_root = sample_root.join(experiment);
    if !experiment_root.is_dir() {
        bail!(
            "Phase 20 sample {experiment:?} does not exist under {}",
            sample_root.display()
        );
    }

    let direct_tiffs = list_direct_tiff_files(&experiment_root)?;
    if !direct_tiffs.is_empty() {
        return Ok(TiffImportSource::Directory(experiment_root));
    }

    let child_dirs = list_direct_child_dirs(&experiment_root)?;
    if child_dirs.len() == 1 {
        let child_tiffs = list_direct_tiff_files(&child_dirs[0])?;
        if child_tiffs.is_empty() {
            bail!(
                "Phase 20 sample {experiment:?} child source folder contains no direct TIFF files: {}",
                child_dirs[0].display()
            );
        }
        if tiff_has_multiple_images(&child_tiffs[0])? {
            return Ok(TiffImportSource::Directory(child_dirs[0].clone()));
        }
        return Ok(TiffImportSource::Directory(experiment_root));
    }

    if inspect_tiff_source_for_review(&TiffImportSource::Directory(experiment_root.clone())).is_ok()
    {
        return Ok(TiffImportSource::Directory(experiment_root));
    }

    bail!(
        "Phase 20 sample {experiment:?} is not a reviewable TIFF source and does not contain exactly one child source folder"
    )
}

fn phase20_import_source_evidence(
    source: TiffImportSource,
    output_package: PathBuf,
    dataset_id: &str,
    dataset_name: &str,
    voxel_spacing_um: [f64; 3],
    no_data_policy: Option<TiffNoDataPolicyReview>,
    storage: TiffImportStorageOptions,
    run_app_open_smoke: bool,
) -> anyhow::Result<Value> {
    let inspect_started = Instant::now();
    let reviewed_inspection = inspect_tiff_source_for_review(&source)?;
    let file_grouping = if reviewed_inspection.source_profile == TiffSourceProfile::StackSeriesMovie
    {
        Some(reviewed_inspection.files.clone())
    } else {
        None
    };
    let inspection = if let Some(file_grouping) = &file_grouping {
        inspect_tiff_source_with_grouping(&source, file_grouping)?
    } else {
        reviewed_inspection.clone()
    };
    let inspect_ms = inspect_started.elapsed().as_secs_f64() * 1000.0;
    let mut reviewed_plan = accepted_tiff_reviewed_import_plan(&inspection, voxel_spacing_um, true);
    reviewed_plan.no_data_policy = no_data_policy;
    let mut progress_events = Vec::new();
    let import_started = Instant::now();
    let import_report = import_tiff_source_with_progress(
        TiffSourceImportOptions {
            source: source.clone(),
            output_package: output_package.clone(),
            dataset_id: dataset_id.to_owned(),
            dataset_name: dataset_name.to_owned(),
            voxel_spacing_um,
            channel_metadata: BTreeMap::new(),
            file_grouping,
            existing_policy: ExistingPackagePolicy::Replace,
            storage,
            reviewed_plan,
        },
        &ImportCancellationToken::new(),
        |event| {
            progress_events.push(format!("{event:?}"));
            Ok(())
        },
    )?;
    let import_ms = import_started.elapsed().as_secs_f64() * 1000.0;
    let manifest = load_manifest(&import_report.output_package)?;
    let dataset = DatasetHandle::open(&import_report.output_package)?;
    let layer_id = dataset.first_layer_id()?;
    let first_volume_shape = first_layer_shape_from_manifest(&manifest, &layer_id)?;
    let first_brick_probe = read_first_brick_probe(&dataset, &manifest, &layer_id)?;
    let app_smoke_report = if run_app_open_smoke {
        let playback_steps = if inspection.timepoint_count > 1 {
            Some(usize::try_from(inspection.timepoint_count.min(4)).unwrap_or(4))
        } else {
            None
        };
        Some(app_smoke_with_options(
            &import_report.output_package,
            "phase20-app-smoke",
            ReleaseAppSmokeOptions {
                playback_steps,
                timeout_secs: Some(60),
            },
        )?)
    } else {
        None
    };
    let renderer_report = phase20_renderer_evidence_report(
        &import_report.output_package,
        dataset_id,
        inspection.source_profile,
        manifest
            .layers
            .first()
            .map(|layer| layer.dtype.stored)
            .context("imported manifest has no layers")?,
    )?;

    Ok(json!({
        "source": {
            "path": source.path(),
            "source_profile": format!("{:?}", inspection.source_profile),
            "file_count": inspection.file_count,
            "channel_count": inspection.channel_count,
            "timepoint_count": inspection.timepoint_count,
            "shape_zyx": {
                "z": inspection.shape.z,
                "y": inspection.shape.y,
                "x": inspection.shape.x,
            },
            "source_dtype": format!("{:?}", inspection.source_dtype),
            "value_range": {
                "min": inspection.value_range.min,
                "max": inspection.value_range.max,
            },
            "inspect_ms": inspect_ms,
        },
        "import": {
            "output_package": import_report.output_package,
            "import_ms": import_ms,
            "progress_event_count": progress_events.len(),
            "progress_events": progress_events,
            "scale_count": import_report.scale_count,
            "source_format": manifest.provenance.source_format.clone(),
            "source_file_count": manifest.provenance.source_files.len(),
            "source_fingerprints_recorded": manifest.provenance.source_files.iter().all(|file| file.fingerprint_blake3.is_some()),
            "voxel_spacing_um": voxel_spacing_um,
            "no_data_policy": manifest.layers.first().and_then(|layer| layer.no_data_policy),
            "storage": import_storage_evidence_json(storage, &manifest),
        },
        "validation": {
            "manifest_loaded": true,
            "dataset_opened": true,
            "first_layer_id": layer_id.as_str(),
            "first_volume_shape": {
                "z": first_volume_shape.z,
                "y": first_volume_shape.y,
                "x": first_volume_shape.x,
            },
            "first_brick_probe": first_brick_probe,
            "app_smoke_report": app_smoke_report,
            "renderer_report": renderer_report,
        },
    }))
}

fn import_storage_evidence_json(
    storage: TiffImportStorageOptions,
    manifest: &NativeManifest,
) -> Value {
    let requested_brick_shape_zyx = storage
        .brick_shape_zyx
        .map(|shape| {
            json!({
                "z": shape.z,
                "y": shape.y,
                "x": shape.x,
            })
        })
        .unwrap_or(Value::Null);
    let scale_storage = manifest
        .layers
        .first()
        .map(|layer| {
            layer
                .scales
                .iter()
                .map(|scale| {
                    json!({
                        "level": scale.level,
                        "shape": scale.shape,
                        "brick_shape": scale.storage.brick_shape,
                        "brick_grid_shape": scale.storage.brick_grid_shape,
                        "shard_shape": scale.storage.shard_shape,
                        "chunks_per_shard": scale.storage.chunks_per_shard,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let source_scale_brick_shape = scale_storage
        .first()
        .and_then(|scale| scale.get("brick_shape"))
        .cloned()
        .unwrap_or(Value::Null);

    json!({
        "requested_brick_shape_zyx": requested_brick_shape_zyx,
        "source_scale_brick_shape": source_scale_brick_shape,
        "scales": scale_storage,
    })
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
    Ok(Shape3D::new(layer.shape.z, layer.shape.y, layer.shape.x)?)
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
                TimeIndex(0),
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
                TimeIndex(0),
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
                TimeIndex(0),
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

fn phase20_renderer_evidence_report(
    package: &Path,
    dataset_id: &str,
    source_profile: TiffSourceProfile,
    stored_dtype: IntensityDType,
) -> anyhow::Result<Value> {
    if source_profile != TiffSourceProfile::PlaneSeriesVolume {
        return Ok(json!({
            "available": false,
            "reason": "renderer_lod_evidence_required_for_plane_series_volume_only",
        }));
    }
    if !matches!(stored_dtype, IntensityDType::Uint8 | IntensityDType::Uint16) {
        return Ok(json!({
            "available": false,
            "reason": format!("phase13_renderer_report_does_not_support_{stored_dtype:?}"),
        }));
    }
    let output_root = PathBuf::from("target").join("mirante4d").join("phase20");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let report_path = output_root.join(format!("phase20-renderer-{dataset_id}.json"));
    let report = phase13_renderer_report(package, Phase13RendererBenchmarkOptions::default())?;
    fs::write(
        &report_path,
        format!("{}\n", serde_json::to_string_pretty(&report)?),
    )
    .with_context(|| format!("failed to write {}", report_path.display()))?;
    Ok(json!({
        "available": true,
        "path": report_path,
        "lod": report.get("lod").cloned().unwrap_or(Value::Null),
        "summary": report.get("summary").cloned().unwrap_or(Value::Null),
        "gpu": report.get("gpu").cloned().unwrap_or(Value::Null),
        "resident_set": report.get("resident_set").cloned().unwrap_or(Value::Null),
    }))
}

pub(crate) fn write_phase20_u16_stack(path: &Path, timepoint: u16) -> anyhow::Result<()> {
    let file = fs::File::create(path)?;
    let mut encoder = TiffEncoder::new(file)?;
    for z in 0..2 {
        let values = (0..3)
            .flat_map(|y| (0..4).map(move |x| timepoint * 100 + z * 20 + y * 4 + x))
            .collect::<Vec<_>>();
        encoder.write_image::<colortype::Gray16>(4, 3, &values)?;
    }
    Ok(())
}

pub(crate) fn write_phase20_u8_plane(path: &Path, base: u8) -> anyhow::Result<()> {
    let file = fs::File::create(path)?;
    let mut encoder = TiffEncoder::new(file)?;
    let values = (0..3)
        .flat_map(|y| (0..4).map(move |x| base + (y * 4 + x) as u8))
        .collect::<Vec<_>>();
    encoder.write_image::<colortype::Gray8>(4, 3, &values)?;
    Ok(())
}

fn phase20_audit_markdown(report: &Value) -> String {
    let mut out = String::new();
    out.push_str("# Phase 20 Audit\n\n");
    out.push_str(&format!(
        "Generated by `{}`.\n\n",
        report["audit"].as_str().unwrap_or("phase20 audit")
    ));
    for key in [
        "stack_series",
        "plane_series",
        "T5-QUAL-001",
        "T5-QUAL-002",
        "T5-QUAL-003",
    ] {
        let evidence = &report[key];
        if evidence.is_null() {
            continue;
        }
        out.push_str(&format!("## {key}\n\n"));
        out.push_str(&format!(
            "- source profile: `{}`\n",
            evidence["source"]["source_profile"]
                .as_str()
                .unwrap_or("<unknown>")
        ));
        out.push_str(&format!(
            "- files/channels/timepoints: `{}/{}/{}`\n",
            evidence["source"]["file_count"].as_u64().unwrap_or(0),
            evidence["source"]["channel_count"].as_u64().unwrap_or(0),
            evidence["source"]["timepoint_count"].as_u64().unwrap_or(0)
        ));
        out.push_str(&format!(
            "- shape z/y/x: `{}/{}/{}`\n",
            evidence["source"]["shape_zyx"]["z"].as_u64().unwrap_or(0),
            evidence["source"]["shape_zyx"]["y"].as_u64().unwrap_or(0),
            evidence["source"]["shape_zyx"]["x"].as_u64().unwrap_or(0)
        ));
        out.push_str(&format!(
            "- output package: `{}`\n",
            evidence["import"]["output_package"]
                .as_str()
                .unwrap_or("<unknown>")
        ));
        out.push_str(&format!(
            "- app smoke report: `{}`\n\n",
            evidence["validation"]["app_smoke_report"]
                .as_str()
                .unwrap_or("<not run>")
        ));
        if let Some(renderer) = evidence["validation"]["renderer_report"].as_object() {
            out.push_str(&format!(
                "- renderer evidence: `{}`\n",
                renderer
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("<not available>")
            ));
            if let Some(lod) = renderer.get("lod")
                && !lod.is_null()
            {
                out.push_str(&format!(
                    "- renderer LOD: target `s{}`, displayed `s{}`, completeness `{}`\n",
                    lod["target_scale_level"].as_u64().unwrap_or(0),
                    lod["displayed_scale_level"].as_u64().unwrap_or(0),
                    lod["completeness"].as_str().unwrap_or("<unknown>")
                ));
            }
        }
        out.push('\n');
    }
    out
}
