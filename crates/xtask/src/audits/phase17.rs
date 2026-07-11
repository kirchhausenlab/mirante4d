use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, bail};
use mirante4d_core::TimeIndex;
use mirante4d_data::DatasetHandle;
use mirante4d_format::validate::load_manifest;
use mirante4d_format::{ExistingPackagePolicy, NativeDatasetProvenanceKind};
use mirante4d_import::{
    ImportCancellationToken, TiffImportSource, TiffSourceImportOptions,
    accepted_tiff_reviewed_import_plan, import_tiff_source_with_progress,
    inspect_tiff_source_for_review, inspect_tiff_source_with_grouping,
};
use serde_json::{Value, json};
use tiff::encoder::{TiffEncoder, colortype};
use tiff::tags::Tag;

use crate::host::benchmark_host_context;

pub(crate) fn phase17_audit() -> anyhow::Result<PathBuf> {
    let output_root = PathBuf::from("target").join("mirante4d").join("phase17");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let source_root = output_root.join("source");
    fs::create_dir_all(&source_root)
        .with_context(|| format!("failed to create {}", source_root.display()))?;
    let source_path = source_root.join("phase17-ome-source.tif");
    write_phase17_ome_tiff(&source_path)
        .with_context(|| format!("failed to write {}", source_path.display()))?;

    let source = TiffImportSource::SingleFile(source_path.clone());
    let inspect_started = Instant::now();
    let reviewed_inspection = inspect_tiff_source_for_review(&source)?;
    let file_grouping = reviewed_inspection.files.clone();
    let inspection = inspect_tiff_source_with_grouping(&source, &file_grouping)?;
    let inspect_ms = inspect_started.elapsed().as_secs_f64() * 1000.0;
    let voxel_spacing_um = inspection
        .source_metadata
        .voxel_spacing_um
        .context("phase17 OME source should expose complete voxel spacing")?;
    let reviewed_plan = accepted_tiff_reviewed_import_plan(&inspection, voxel_spacing_um, true);

    let output_package = output_root.join("phase17-reviewed-import.m4d");
    let mut progress_events = Vec::new();
    let import_started = Instant::now();
    let import_report = import_tiff_source_with_progress(
        TiffSourceImportOptions {
            source: source.clone(),
            output_package: output_package.clone(),
            dataset_id: "phase17-reviewed-import".to_owned(),
            dataset_name: "Phase 17 Reviewed Import".to_owned(),
            voxel_spacing_um,
            channel_metadata: BTreeMap::new(),
            file_grouping: Some(file_grouping),
            existing_policy: ExistingPackagePolicy::Replace,
            storage: Default::default(),
            reviewed_plan,
        },
        &ImportCancellationToken::new(),
        |event| {
            progress_events.push(format!("{event:?}"));
            Ok(())
        },
    )?;
    let import_ms = import_started.elapsed().as_secs_f64() * 1000.0;

    let dataset = DatasetHandle::open(&import_report.output_package)?;
    let layer_id = dataset.first_layer_id()?;
    let volume = dataset.read_u16_volume(&layer_id, TimeIndex(0))?;
    let manifest = load_manifest(&import_report.output_package)?;
    let provenance = &manifest.provenance;
    if provenance.kind != NativeDatasetProvenanceKind::Imported {
        bail!("phase17 imported package did not record imported provenance kind");
    }
    if provenance.source_files.is_empty() || provenance.source_metadata.is_none() {
        bail!("phase17 imported package did not record source provenance");
    }

    let matrix = mirante4d_import::supported_source_format_matrix()
        .iter()
        .map(|entry| {
            json!({
                "id": entry.id,
                "label": entry.label,
                "status": format!("{:?}", entry.status),
                "parser_owner": entry.parser_owner,
                "metadata_guarantees": entry.metadata_guarantees,
                "unsupported_variants": entry.unsupported_variants,
                "required_tests": entry.required_tests,
            })
        })
        .collect::<Vec<_>>();
    let source_metadata = provenance.source_metadata.as_ref().unwrap();
    let report_json = json!({
        "audit": "phase17-import-metadata-hardening",
        "audit_schema_version": 1,
        "phase": "Phase 17: Import And Metadata Hardening",
        "hardware": benchmark_host_context(),
        "source_format_matrix": matrix,
        "source": {
            "path": source.path(),
            "inspection_ms": inspect_ms,
            "file_count": inspection.file_count,
            "channel_count": inspection.channel_count,
            "timepoint_count": inspection.timepoint_count,
            "shape_zyx": {
                "z": inspection.shape.z,
                "y": inspection.shape.y,
                "x": inspection.shape.x,
            },
            "source_dtype": format!("{:?}", inspection.source_dtype),
            "metadata_confidence": format!("{:?}", inspection.metadata_confidence),
            "voxel_spacing_um": voxel_spacing_um,
            "value_range": {
                "min": inspection.value_range.min,
                "max": inspection.value_range.max,
            },
        },
        "review": {
            "native_axes": source_metadata.native_axes.clone(),
            "channels_as_layers": source_metadata.channels_as_layers,
            "user_corrections": provenance.user_corrections.clone(),
        },
        "import": {
            "output_package": import_report.output_package,
            "import_ms": import_ms,
            "progress_event_count": progress_events.len(),
            "progress_events": progress_events,
            "manifest_format": manifest.format.clone(),
            "manifest_schema_version": manifest.schema_version,
            "provenance_kind": format!("{:?}", provenance.kind),
            "source_format": provenance.source_format.clone(),
            "source_file_count": provenance.source_files.len(),
            "source_fingerprints_recorded": provenance.source_files.iter().all(|file| file.fingerprint_blake3.is_some()),
            "storage_policy": provenance.storage_policy.clone(),
            "checksum_policy": provenance.checksum_policy.clone(),
        },
        "validation": {
            "strict_native_opened": true,
            "first_layer_id": layer_id.as_str(),
            "first_volume_shape": {
                "z": volume.shape.z,
                "y": volume.shape.y,
                "x": volume.shape.x,
            },
            "first_volume_probe_1_1_2": volume.voxel(1, 1, 2),
        },
    });

    let report_json_path = output_root.join("phase17-audit-report.json");
    let report_md_path = output_root.join("phase17-audit-report.md");
    fs::write(
        &report_json_path,
        format!("{}\n", serde_json::to_string_pretty(&report_json)?),
    )
    .with_context(|| format!("failed to write {}", report_json_path.display()))?;
    fs::write(&report_md_path, phase17_audit_markdown(&report_json))
        .with_context(|| format!("failed to write {}", report_md_path.display()))?;
    Ok(report_md_path)
}

fn write_phase17_ome_tiff(path: &Path) -> anyhow::Result<()> {
    let file = fs::File::create(path)?;
    let mut encoder = TiffEncoder::new(file)?;
    let ome_xml = r#"<OME xmlns="http://www.openmicroscopy.org/Schemas/OME/2016-06"><Image ID="Image:0"><Pixels ID="Pixels:0" DimensionOrder="XYZCT" Type="uint16" SizeX="3" SizeY="2" SizeZ="2" SizeC="1" SizeT="1" PhysicalSizeX="0.25" PhysicalSizeXUnit="um" PhysicalSizeY="0.5" PhysicalSizeYUnit="um" PhysicalSizeZ="0.75" PhysicalSizeZUnit="um"><Channel ID="Channel:0:0" SamplesPerPixel="1"/></Pixels></Image></OME>"#;
    for z in 0..2 {
        let values = (0..2)
            .flat_map(|y| (0..3).map(move |x| (z * 10 + y * 3 + x) as u16))
            .collect::<Vec<_>>();
        let mut image = encoder.new_image::<colortype::Gray16>(3, 2)?;
        if z == 0 {
            image.encoder().write_tag(Tag::ImageDescription, ome_xml)?;
        }
        image.write_data(&values)?;
    }
    Ok(())
}

fn phase17_audit_markdown(report: &Value) -> String {
    let source = &report["source"];
    let import = &report["import"];
    let validation = &report["validation"];
    let mut out = String::new();
    out.push_str("# Phase 17 Import Audit\n\n");
    out.push_str("Phase 17 deterministic import audit passed.\n\n");
    out.push_str("## Source\n\n");
    out.push_str(&format!(
        "- format confidence: `{}`\n",
        source["metadata_confidence"].as_str().unwrap_or("unknown")
    ));
    out.push_str(&format!(
        "- files/channels/timepoints: `{}/{}/{}`\n",
        source["file_count"].as_u64().unwrap_or(0),
        source["channel_count"].as_u64().unwrap_or(0),
        source["timepoint_count"].as_u64().unwrap_or(0)
    ));
    out.push_str(&format!(
        "- value range: `{}` to `{}`\n",
        source["value_range"]["min"].as_f64().unwrap_or(0.0),
        source["value_range"]["max"].as_f64().unwrap_or(0.0)
    ));
    out.push_str("\n## Import\n\n");
    out.push_str(&format!(
        "- output package: `{}`\n",
        import["output_package"].as_str().unwrap_or("<unknown>")
    ));
    out.push_str(&format!(
        "- provenance kind: `{}`\n",
        import["provenance_kind"].as_str().unwrap_or("<unknown>")
    ));
    out.push_str(&format!(
        "- source fingerprints recorded: `{}`\n",
        import["source_fingerprints_recorded"]
            .as_bool()
            .unwrap_or(false)
    ));
    out.push_str("\n## Validation\n\n");
    out.push_str(&format!(
        "- strict native open: `{}`\n",
        validation["strict_native_opened"]
            .as_bool()
            .unwrap_or(false)
    ));
    out.push_str(&format!(
        "- first layer: `{}`\n",
        validation["first_layer_id"].as_str().unwrap_or("<unknown>")
    ));
    out
}
