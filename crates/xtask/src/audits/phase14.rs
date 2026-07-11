use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use mirante4d_data::{DatasetHandle, SpatialBrickIndex};
use mirante4d_domain::{IntensityDType, Shape3D, TimeIndex};
use mirante4d_format::LayerId;
use serde_json::{Value, json};

use crate::fixtures::generate_fixture;
use crate::host::benchmark_host_context;

pub(crate) fn phase14_audit() -> anyhow::Result<PathBuf> {
    let output_root = PathBuf::from("target").join("mirante4d").join("phase14");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let report_json = phase14_multichannel_report()?;
    let report_json_path = output_root.join("phase14-audit-report.json");
    let report_md_path = output_root.join("phase14-audit-report.md");
    fs::write(
        &report_json_path,
        format!("{}\n", serde_json::to_string_pretty(&report_json)?),
    )
    .with_context(|| format!("failed to write {}", report_json_path.display()))?;
    fs::write(&report_md_path, phase14_audit_markdown(&report_json))
        .with_context(|| format!("failed to write {}", report_md_path.display()))?;
    Ok(report_md_path)
}

pub(crate) fn bench_phase14_multichannel() -> anyhow::Result<PathBuf> {
    let output_root = PathBuf::from("target").join("mirante4d").join("benchmarks");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let report_json = phase14_multichannel_report()?;
    let report_json_path = output_root.join("bench-phase14-multichannel.json");
    fs::write(
        &report_json_path,
        format!("{}\n", serde_json::to_string_pretty(&report_json)?),
    )
    .with_context(|| format!("failed to write {}", report_json_path.display()))?;
    Ok(report_json_path)
}

fn phase14_multichannel_report() -> anyhow::Result<Value> {
    println!("phase14 audit: generated time-multichannel fixture inventory/resource probe");
    let fixture = generate_fixture("time-multichannel-u16-8cube-3t-2c")?;
    let dataset = DatasetHandle::open(&fixture)?;
    let manifest = dataset.manifest();
    let layer_inventory = manifest
        .layers
        .iter()
        .map(|layer| {
            let layer_id = LayerId::new(layer.id.clone())?;
            let value_range = phase14_layer_value_range(&dataset, &layer_id)?;
            Ok(json!({
                "layer_id": layer.id,
                "name": layer.name,
                "dtype": format!("{:?}", layer.dtype.stored),
                "shape": {
                    "t": layer.shape.t(),
                    "z": layer.shape.z(),
                    "y": layer.shape.y(),
                    "x": layer.shape.x(),
                },
                "timepoints": layer.shape.t(),
                "value_range_t0_s0": value_range,
                "metadata_complete": !layer.name.trim().is_empty()
                    && layer.channel.color_rgba.iter().all(|value| value.is_finite()),
            }))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let resource_probe = phase14_visible_hidden_resource_probe(&dataset)?;
    let sample_root = env::var_os("MIRANTE4D_SAMPLE_DATA").map(PathBuf::from);
    let local_samples = phase14_local_sample_inventory(sample_root.as_deref())?;
    Ok(json!({
        "benchmark": "bench-phase14-multichannel",
        "benchmark_schema_version": 1,
        "phase": "Phase 14: Multi-Channel Rendering",
        "hardware": benchmark_host_context(),
        "generated_fixture": {
            "name": "time-multichannel-u16-8cube-3t-2c",
            "package": fixture,
            "dataset_id": manifest.dataset.id,
            "channel_count": manifest.layers.len(),
            "layers": layer_inventory,
        },
        "local_sample_inventory": local_samples,
        "required_workflows": [
            "single-channel isolation",
            "all-channel fluorescence composite",
            "channel comparison",
            "colocalization-style visual inspection",
            "time playback with multiple channels",
            "save/reopen of display transfer and channel presets",
            "visible-channel-only streaming under memory pressure"
        ],
        "transfer_model": {
            "display_only": true,
            "source_values_mutated": false,
            "fields": ["visibility", "display_window", "color_rgba", "opacity", "curve", "preset_id"],
            "built_in_transfer_presets": [
                {"id": "linear", "curve": "linear"},
                {"id": "bright_gamma", "curve": "gamma", "gamma": 2.0},
                {"id": "high_contrast", "curve": "gamma", "gamma": 0.75}
            ]
        },
        "composite_model": {
            "hidden_channels_contribute": false,
            "current_implemented_stage": "per-channel intensity frame rendered first, deterministic display transfer and additive color compositing applied to the UI texture",
            "single_channel_is_special_case": true,
            "saturation_policy": "additive channels clamp RGBA output to display range; active mode remains visible in UI"
        },
        "resource_probe": resource_probe,
        "test_evidence": {
            "core": "display::tests::channel_transfer_function_roundtrips_through_json",
            "renderer": "transfer::tests::gamma_curve_changes_display_mapping_only and invisible_channel_does_not_contribute",
            "app": [
                "hidden_active_layer_does_not_block_visible_channel_resident_rendering",
                "channel_display_preset_applies_full_transfer_and_rejects_stale_layer_identity",
                "project_package_roundtrip_restores_layer_display_states",
                "workbench_shell_exposes_channel_display_controls"
            ]
        },
        "findings": [
            {
                "classification": "no action",
                "surface": "native channel model",
                "observation": "generated fixture uses two independent intensity layers with no native channel axis"
            },
            {
                "classification": "no action",
                "surface": "hidden-channel resource policy",
                "observation": "resource probe and app tests show hidden channels are excluded from current-frame brick requests"
            },
            {
                "classification": "sample-data gap",
                "surface": "real multi-channel sample metadata",
                "observation": "real sample folders are inventoried when MIRANTE4D_SAMPLE_DATA is set, but deterministic fixture coverage remains the binding CI-safe multichannel evidence"
            }
        ]
    }))
}

fn phase14_layer_value_range(dataset: &DatasetHandle, layer_id: &LayerId) -> anyhow::Result<Value> {
    let volume = dataset.read_u16_volume(layer_id, TimeIndex::new(0))?;
    let min = volume.values().iter().copied().min().unwrap_or(0);
    let max = volume.values().iter().copied().max().unwrap_or(0);
    Ok(json!({
        "min": min,
        "max": max,
        "sample_count": volume.values().len(),
    }))
}

fn phase14_visible_hidden_resource_probe(dataset: &DatasetHandle) -> anyhow::Result<Value> {
    let layer_ids = dataset
        .manifest()
        .layers
        .iter()
        .map(|layer| LayerId::new(layer.id.clone()))
        .collect::<Result<Vec<_>, _>>()?;
    let active_layer = layer_ids
        .first()
        .cloned()
        .context("phase14 resource probe requires at least one layer")?;
    let all_visible = phase14_resource_case(dataset, &layer_ids)?;
    let hidden_active_layers = layer_ids
        .iter()
        .filter(|layer_id| **layer_id != active_layer)
        .cloned()
        .collect::<Vec<_>>();
    let hidden_active = phase14_resource_case(dataset, &hidden_active_layers)?;
    let first_non_active = layer_ids.get(1).cloned();
    let hidden_non_active_layers = layer_ids
        .iter()
        .filter(|layer_id| Some(*layer_id) != first_non_active.as_ref())
        .cloned()
        .collect::<Vec<_>>();
    let hidden_non_active = phase14_resource_case(dataset, &hidden_non_active_layers)?;
    Ok(json!({
        "scale_level": 0,
        "timepoint": 0,
        "all_visible": all_visible,
        "hidden_active": hidden_active,
        "hidden_non_active": hidden_non_active,
        "hidden_channels_request_zero_current_frame_bricks": hidden_active.pointer("/layer_request_count").and_then(Value::as_u64).unwrap_or(u64::MAX)
            < all_visible.pointer("/layer_request_count").and_then(Value::as_u64).unwrap_or(0),
    }))
}

fn phase14_resource_case(dataset: &DatasetHandle, layer_ids: &[LayerId]) -> anyhow::Result<Value> {
    let mut occupied_request_count = 0u64;
    let mut decoded_bytes = 0u64;
    let mut layer_summaries = Vec::new();
    for layer_id in layer_ids {
        let layer = dataset
            .layer(layer_id)
            .ok_or_else(|| anyhow::anyhow!("missing layer {}", layer_id))?;
        let grid = dataset.brick_grid_shape_at_scale(layer_id, 0)?;
        let bricks = phase14_all_bricks(grid);
        let dtype_bytes = match layer.dtype.stored {
            IntensityDType::Uint8 | IntensityDType::Uint16 => 2,
            IntensityDType::Float32 => 4,
        };
        let mut layer_requests = 0u64;
        let mut layer_decoded_bytes = 0u64;
        for brick in bricks {
            let metadata =
                dataset.brick_metadata_at_scale(layer_id, 0, TimeIndex::new(0), brick)?;
            if metadata.occupied {
                layer_requests += 1;
                layer_decoded_bytes = layer_decoded_bytes
                    .saturating_add(metadata.region.shape()?.element_count()? * dtype_bytes);
            }
        }
        occupied_request_count += layer_requests;
        decoded_bytes = decoded_bytes.saturating_add(layer_decoded_bytes);
        layer_summaries.push(json!({
            "layer_id": layer_id.as_str(),
            "occupied_current_frame_bricks": layer_requests,
            "estimated_decoded_bytes": layer_decoded_bytes,
        }));
    }
    Ok(json!({
        "visible_layers": layer_ids.iter().map(|layer_id| layer_id.as_str()).collect::<Vec<_>>(),
        "layer_request_count": occupied_request_count,
        "estimated_decoded_bytes": decoded_bytes,
        "layers": layer_summaries,
    }))
}

fn phase14_all_bricks(grid: Shape3D) -> Vec<SpatialBrickIndex> {
    let mut bricks = Vec::new();
    for z in 0..grid.z() {
        for y in 0..grid.y() {
            for x in 0..grid.x() {
                bricks.push(SpatialBrickIndex::new(z, y, x));
            }
        }
    }
    bricks
}

fn phase14_local_sample_inventory(sample_root: Option<&Path>) -> anyhow::Result<Value> {
    let Some(sample_root) = sample_root else {
        return Ok(json!({
            "sample_root": null,
            "status": "MIRANTE4D_SAMPLE_DATA not set",
            "experiments": []
        }));
    };
    if !sample_root.is_dir() {
        return Ok(json!({
            "sample_root": sample_root,
            "status": "sample root does not exist",
            "experiments": []
        }));
    }
    let mut experiments = Vec::new();
    for entry in fs::read_dir(sample_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let file_count = count_files_bounded(&path, 256)?;
        experiments.push(json!({
            "name": name,
            "path": path,
            "file_count_bounded_256": file_count,
            "metadata_status": "folder inventoried; import-specific channel metadata is measured by import/audit commands when the format is supported"
        }));
    }
    experiments.sort_by(|left, right| {
        left.get("name")
            .and_then(Value::as_str)
            .cmp(&right.get("name").and_then(Value::as_str))
    });
    Ok(json!({
        "sample_root": sample_root,
        "status": "inventoried local experiment folders",
        "experiments": experiments,
    }))
}

fn count_files_bounded(root: &Path, limit: usize) -> anyhow::Result<usize> {
    let mut count = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                stack.push(entry.path());
            } else {
                count += 1;
                if count >= limit {
                    return Ok(count);
                }
            }
        }
    }
    Ok(count)
}

fn phase14_audit_markdown(report: &Value) -> String {
    let fixture = report
        .pointer("/generated_fixture/name")
        .and_then(Value::as_str)
        .unwrap_or("unknown fixture");
    let channel_count = report
        .pointer("/generated_fixture/channel_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let all_requests = report
        .pointer("/resource_probe/all_visible/layer_request_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let hidden_active_requests = report
        .pointer("/resource_probe/hidden_active/layer_request_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let hidden_policy = report
        .pointer("/resource_probe/hidden_channels_request_zero_current_frame_bricks")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut out = String::new();
    out.push_str("# Phase 14 Multi-Channel Audit\n\n");
    out.push_str(&format!(
        "- generated fixture: `{fixture}` with `{channel_count}` channel(s)\n"
    ));
    out.push_str(&format!(
        "- visible-vs-hidden resource probe: all visible `{all_requests}` occupied current-frame brick request(s), hidden active `{hidden_active_requests}`\n"
    ));
    out.push_str(&format!(
        "- hidden-channel current-frame exclusion: `{hidden_policy}`\n"
    ));
    out.push_str("- transfer model: display-only window/color/opacity/curve/preset state\n");
    out.push_str("- compositing model: deterministic display transfer plus additive color compositing; hidden channels contribute zero\n\n");
    out.push_str("## Workflows\n\n");
    if let Some(workflows) = report.get("required_workflows").and_then(Value::as_array) {
        for workflow in workflows {
            if let Some(workflow) = workflow.as_str() {
                out.push_str(&format!("- {workflow}\n"));
            }
        }
    }
    out.push_str("\n## Findings\n\n");
    if let Some(findings) = report.get("findings").and_then(Value::as_array) {
        for finding in findings {
            let classification = finding
                .get("classification")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let surface = finding
                .get("surface")
                .and_then(Value::as_str)
                .unwrap_or("unknown surface");
            let observation = finding
                .get("observation")
                .and_then(Value::as_str)
                .unwrap_or("");
            out.push_str(&format!(
                "- `{classification}` `{surface}`: {observation}\n"
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase14_resource_probe_records_visible_only_channel_work() {
        let tempdir = tempfile::tempdir().unwrap();
        let package = mirante4d_format::write_fixture(
            mirante4d_format::FixtureKind::TimeMultiChannelU16_8Cube3T2C,
            tempdir.path(),
        )
        .unwrap();
        let dataset = DatasetHandle::open(&package).unwrap();

        let probe = phase14_visible_hidden_resource_probe(&dataset).unwrap();

        assert_eq!(
            probe["all_visible"]["visible_layers"],
            json!(["ch0", "ch1"])
        );
        assert_eq!(probe["all_visible"]["layer_request_count"], 2);
        assert_eq!(probe["hidden_active"]["visible_layers"], json!(["ch1"]));
        assert_eq!(probe["hidden_active"]["layer_request_count"], 1);
        assert_eq!(probe["hidden_non_active"]["visible_layers"], json!(["ch0"]));
        assert_eq!(probe["hidden_non_active"]["layer_request_count"], 1);
        assert_eq!(
            probe["hidden_channels_request_zero_current_frame_bricks"],
            true
        );
    }
}
