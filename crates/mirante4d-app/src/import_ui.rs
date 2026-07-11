use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::mpsc::Receiver,
};

use eframe::egui;
use mirante4d_application::{ApplicationSnapshot, OperationToken, WorkspaceSnapshot};
use mirante4d_dataset::ResourceValidity;
use mirante4d_domain::{IntensityDType, ScaleLevel};
use mirante4d_format::ExistingPackagePolicy;
use mirante4d_import::{
    ImportCancellationToken, ImportError, ImportProgressEvent, TiffDirectoryImportReport,
    TiffDirectoryInspection, TiffImportSource, TiffImportStorageEstimate, TiffNoDataPolicyReview,
    TiffReviewedImportPlan, TiffSourceImportOptions, TiffSourceProfile, TiffValueRangeSummary,
    TiffVoxelSpacingMetadataSource, TiffVoxelSpacingMetadataStatus,
    accepted_tiff_reviewed_import_plan, default_tiff_channel_metadata_override,
    estimate_tiff_import_storage, inspect_tiff_source_for_review,
    inspect_tiff_source_with_grouping,
};

use crate::ui_kit::{self, StatusTone};

pub(crate) struct TiffImportSetupTask {
    pub(crate) source: TiffImportSource,
    pub(crate) output_parent: PathBuf,
    pub(crate) receiver: Receiver<TiffImportSetupTaskMessage>,
}

pub(crate) enum TiffImportSetupTaskMessage {
    Finished(Result<(TiffSourceImportOptions, TiffDirectoryInspection), String>),
}

#[derive(Debug, Clone)]
pub(crate) struct PendingTiffImport {
    pub(crate) options: TiffSourceImportOptions,
    pub(crate) inspection: TiffDirectoryInspection,
    pub(crate) voxel_spacing_confirmed: bool,
    pub(crate) grouping_confirmed: bool,
}

pub(crate) struct ImportTask {
    pub(crate) token: OperationToken,
    pub(crate) cancellation: ImportCancellationToken,
    pub(crate) receiver: Receiver<ImportTaskMessage>,
    pub(crate) latest_event: Option<ImportProgressEvent>,
}

impl Drop for ImportTask {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

pub(crate) enum ImportTaskMessage {
    Progress(ImportProgressEvent),
    Finished(Result<TiffDirectoryImportReport, ImportError>),
}

pub(crate) fn tiff_source_profile_label(profile: TiffSourceProfile) -> &'static str {
    match profile {
        TiffSourceProfile::StackSeriesMovie => "stack-series movie",
        TiffSourceProfile::PlaneSeriesVolume => "plane-series volume",
    }
}

pub(crate) fn tiff_voxel_spacing_metadata_label(inspection: &TiffDirectoryInspection) -> String {
    match inspection.source_metadata.voxel_spacing_status {
        TiffVoxelSpacingMetadataStatus::Complete => {
            let spacing = inspection
                .source_metadata
                .voxel_spacing_um
                .expect("complete TIFF voxel metadata includes spacing");
            let source = match inspection.source_metadata.voxel_spacing_source {
                Some(TiffVoxelSpacingMetadataSource::OmeXml) => "OME-XML",
                None => "metadata",
            };
            format!(
                "{source} x {:.4} y {:.4} z {:.4} um",
                spacing[0], spacing[1], spacing[2]
            )
        }
        TiffVoxelSpacingMetadataStatus::Missing => "missing".to_owned(),
        TiffVoxelSpacingMetadataStatus::Incomplete => "incomplete".to_owned(),
        TiffVoxelSpacingMetadataStatus::Conflicting => "conflicting".to_owned(),
    }
}

pub(crate) fn tiff_import_storage_estimate_label(inspection: &TiffDirectoryInspection) -> String {
    match estimate_tiff_import_storage(inspection) {
        Ok(estimate) => format_tiff_import_storage_estimate(estimate),
        Err(err) => format!("unavailable: {err}"),
    }
}

pub(crate) fn format_tiff_value_range(range: TiffValueRangeSummary) -> String {
    format!("{:.6} to {:.6}", range.min, range.max)
}

fn format_tiff_import_storage_estimate(estimate: TiffImportStorageEstimate) -> String {
    format!(
        "{} package, {} peak stack",
        format_byte_quantity(estimate.estimated_total_bytes),
        format_byte_quantity(estimate.peak_working_stack_bytes)
    )
}

fn format_byte_quantity(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * KIB;
    const GIB: f64 = 1024.0 * MIB;
    let bytes_f = bytes as f64;
    if bytes_f >= GIB {
        format!("{:.2} GiB", bytes_f / GIB)
    } else if bytes_f >= MIB {
        format!("{:.2} MiB", bytes_f / MIB)
    } else if bytes_f >= KIB {
        format!("{:.1} KiB", bytes_f / KIB)
    } else {
        format!("{bytes} B")
    }
}

pub(crate) fn prepare_tiff_source_import(
    source: TiffImportSource,
    output_parent: &Path,
) -> anyhow::Result<(TiffSourceImportOptions, TiffDirectoryInspection)> {
    let mut options = import_tiff_source_options(source, output_parent)?;
    let inspection = inspect_tiff_source_for_review(&options.source)?;
    if let Some(voxel_spacing_um) = inspection.source_metadata.voxel_spacing_um {
        options.voxel_spacing_um = voxel_spacing_um;
    }
    if inspection.source_profile == TiffSourceProfile::StackSeriesMovie {
        options.file_grouping = Some(inspection.files.clone());
    }
    for channel in &inspection.channels {
        options
            .channel_metadata
            .entry(channel.channel)
            .or_insert_with(|| default_tiff_channel_metadata_override(channel.channel));
    }
    Ok((options, inspection))
}

pub(crate) fn import_tiff_source_options(
    source: TiffImportSource,
    output_parent: &Path,
) -> anyhow::Result<TiffSourceImportOptions> {
    let dataset_name = source
        .path()
        .file_stem()
        .or_else(|| source.path().file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("imported-dataset")
        .to_owned();
    let dataset_id = dataset_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    let dataset_id = if dataset_id.is_empty() {
        "imported-dataset".to_owned()
    } else {
        dataset_id
    };
    Ok(TiffSourceImportOptions {
        source,
        output_package: output_parent.join(format!("{dataset_id}.m4d")),
        dataset_id,
        dataset_name,
        voxel_spacing_um: [1.0, 1.0, 1.0],
        channel_metadata: BTreeMap::new(),
        file_grouping: None,
        existing_policy: ExistingPackagePolicy::Fail,
        storage: Default::default(),
        reviewed_plan: TiffReviewedImportPlan::pending(),
    })
}

pub(crate) fn validate_tiff_import_options(
    options: &TiffSourceImportOptions,
) -> anyhow::Result<()> {
    if options.dataset_name.trim().is_empty() {
        anyhow::bail!("dataset name must not be empty");
    }
    for (axis, spacing) in [
        ("x", options.voxel_spacing_um[0]),
        ("y", options.voxel_spacing_um[1]),
        ("z", options.voxel_spacing_um[2]),
    ] {
        if !spacing.is_finite() || spacing <= 0.0 {
            anyhow::bail!("voxel spacing {axis} must be positive and finite");
        }
    }
    for (channel, metadata) in &options.channel_metadata {
        if metadata.name.trim().is_empty() {
            anyhow::bail!("channel {channel} name must not be empty");
        }
        if !metadata
            .color_rgba
            .iter()
            .all(|component| component.is_finite() && (0.0..=1.0).contains(component))
        {
            anyhow::bail!("channel {channel} color components must be finite values in [0, 1]");
        }
    }
    Ok(())
}

pub(crate) fn validate_pending_tiff_import(pending: &PendingTiffImport) -> anyhow::Result<()> {
    validate_tiff_import_options(&pending.options)?;
    if !pending.voxel_spacing_confirmed {
        anyhow::bail!(
            "voxel spacing must be reviewed before import; enter calibrated spacing or explicitly accept the current values"
        );
    }
    if !pending.grouping_confirmed {
        anyhow::bail!("TIFF source layout must be reviewed before import");
    }
    if let Some(file_grouping) = &pending.options.file_grouping {
        let inspection = inspect_tiff_source_with_grouping(&pending.options.source, file_grouping)?;
        if inspection.source_profile != pending.inspection.source_profile {
            anyhow::bail!("TIFF source profile changed during import review");
        }
    } else {
        let inspection = inspect_tiff_source_for_review(&pending.options.source)?;
        if inspection.source_profile != pending.inspection.source_profile {
            anyhow::bail!("TIFF source profile changed during import review");
        }
    }
    Ok(())
}

pub(crate) fn pending_tiff_import_ready_to_start(pending: &PendingTiffImport) -> bool {
    pending.voxel_spacing_confirmed && pending.grouping_confirmed
}

pub(crate) fn accepted_reviewed_plan_for_pending_tiff_import(
    pending: &PendingTiffImport,
) -> TiffReviewedImportPlan {
    let no_data_policy = pending.options.reviewed_plan.no_data_policy;
    let mut reviewed_plan = accepted_tiff_reviewed_import_plan(
        &pending.inspection,
        pending.options.voxel_spacing_um,
        pending.grouping_confirmed,
    );
    reviewed_plan.no_data_policy = no_data_policy;
    reviewed_plan
}

fn tiff_no_data_policy_supported(inspection: &TiffDirectoryInspection) -> bool {
    inspection.source_dtype == IntensityDType::Uint8
}

pub(crate) fn set_pending_tiff_no_data_policy(pending: &mut PendingTiffImport, enabled: bool) {
    pending.options.reviewed_plan.no_data_policy =
        if enabled && tiff_no_data_policy_supported(&pending.inspection) {
            Some(TiffNoDataPolicyReview {
                source_dtype: IntensityDType::Uint8,
                source_value_uint8: 255,
            })
        } else {
            None
        };
}

pub(crate) fn normalize_pending_tiff_no_data_policy(pending: &mut PendingTiffImport) {
    if !tiff_no_data_policy_supported(&pending.inspection) {
        pending.options.reviewed_plan.no_data_policy = None;
        return;
    }
    if let Some(policy) = pending.options.reviewed_plan.no_data_policy
        && (policy.source_dtype != pending.inspection.source_dtype
            || policy.source_value_uint8 != 255)
    {
        pending.options.reviewed_plan.no_data_policy = None;
    }
}

pub(crate) fn show_tiff_no_data_controls(ui: &mut egui::Ui, pending: &mut PendingTiffImport) {
    normalize_pending_tiff_no_data_policy(pending);
    ui.add_space(6.0);
    ui.label("no-data mask");
    let supported = tiff_no_data_policy_supported(&pending.inspection);
    let mut enabled = pending.options.reviewed_plan.no_data_policy.is_some();
    let response = ui.add_enabled(
        supported,
        egui::Checkbox::new(&mut enabled, "enable value 255 no-data mask"),
    );
    if response.changed() {
        set_pending_tiff_no_data_policy(pending, enabled);
    }
    if !supported {
        ui_kit::property_row(ui, "status", "disabled for non-uint8 sources");
        ui_kit::property_row(ui, "supported policy", "uint8 value 255");
        return;
    }
    if pending.options.reviewed_plan.no_data_policy.is_some() {
        ui_kit::property_row(ui, "value", "255");
        ui_kit::property_row(ui, "dtype", "uint8");
        ui_kit::property_row(ui, "visibility", "invisible with 1 voxel invalid dilation");
        ui_kit::property_row(
            ui,
            "scope",
            "display statistics, multiscales, rendering, default analysis",
        );
    } else {
        ui_kit::property_row(ui, "status", "disabled");
    }
}

pub(crate) fn active_layer_no_data_policy_label(snapshot: &ApplicationSnapshot) -> Option<String> {
    let active_layer = match snapshot.workspace() {
        WorkspaceSnapshot::Unbound { workspace } => workspace.view().active_layer(),
        WorkspaceSnapshot::Bound { project, .. } => project.view().active_layer(),
    };
    match snapshot
        .catalog()
        .layer(active_layer)
        .and_then(|layer| layer.validity(ScaleLevel::BASE))
    {
        Some(ResourceValidity::BitMask) => Some("explicit per-sample validity mask".to_owned()),
        Some(ResourceValidity::AllValid) | None => None,
    }
}

pub(crate) fn show_tiff_grouping_controls(ui: &mut egui::Ui, pending: &mut PendingTiffImport) {
    let Some(file_grouping) = pending.options.file_grouping.as_mut() else {
        if pending.inspection.source_profile == TiffSourceProfile::PlaneSeriesVolume {
            ui.add_space(6.0);
            ui.label("source layout");
            ui.label(format!(
                "{} channel folder(s), {} z plane(s), lexicographic order",
                pending.inspection.channel_count, pending.inspection.shape.z
            ));
            ui.checkbox(
                &mut pending.grouping_confirmed,
                "plane-series layout reviewed",
            );
        }
        return;
    };
    if file_grouping.len() <= 1 {
        return;
    }

    ui.add_space(6.0);
    ui.label("file grouping");
    let mut changed = false;
    egui::ScrollArea::vertical()
        .max_height(160.0)
        .show(ui, |ui| {
            for grouping in file_grouping {
                ui.horizontal_wrapped(|ui| {
                    let file_name = grouping
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("<unnamed>");
                    ui.label(file_name);
                    let mut channel = i64::from(grouping.channel);
                    if ui
                        .add(
                            egui::DragValue::new(&mut channel)
                                .range(0..=1024)
                                .prefix("ch "),
                        )
                        .changed()
                    {
                        grouping.channel = channel as u32;
                        changed = true;
                    }
                    let mut stack_index = grouping.stack_index.min(1_000_000) as i64;
                    if ui
                        .add(
                            egui::DragValue::new(&mut stack_index)
                                .range(0..=1_000_000)
                                .prefix("t "),
                        )
                        .changed()
                    {
                        grouping.stack_index = stack_index as u64;
                        changed = true;
                    }
                });
            }
        });

    if changed {
        pending.grouping_confirmed = false;
        match refresh_pending_tiff_grouping(pending) {
            Ok(()) => {}
            Err(err) => ui_kit::status_badge(ui, StatusTone::Error, err.to_string()),
        }
    }
    ui.checkbox(
        &mut pending.grouping_confirmed,
        "filename grouping reviewed",
    );
}

pub(crate) fn refresh_pending_tiff_grouping(pending: &mut PendingTiffImport) -> anyhow::Result<()> {
    let Some(file_grouping) = &pending.options.file_grouping else {
        return Ok(());
    };
    let inspection = inspect_tiff_source_with_grouping(&pending.options.source, file_grouping)?;
    for channel in &inspection.channels {
        pending
            .options
            .channel_metadata
            .entry(channel.channel)
            .or_insert_with(|| default_tiff_channel_metadata_override(channel.channel));
    }
    pending.inspection = inspection;
    normalize_pending_tiff_no_data_policy(pending);
    Ok(())
}

pub(crate) fn show_tiff_channel_metadata_controls(
    ui: &mut egui::Ui,
    options: &mut TiffSourceImportOptions,
    inspection: &TiffDirectoryInspection,
    name_width: f32,
) {
    ui.add_space(6.0);
    ui.label("channels");
    for channel in &inspection.channels {
        let metadata = options
            .channel_metadata
            .entry(channel.channel)
            .or_insert_with(|| default_tiff_channel_metadata_override(channel.channel));
        ui.horizontal_wrapped(|ui| {
            let channel_scope = if inspection.source_profile == TiffSourceProfile::PlaneSeriesVolume
            {
                format!("{} z", inspection.shape.z)
            } else {
                format!("{} t", channel.timepoint_count)
            };
            ui.label(format!("ch{} ({channel_scope})", channel.channel));
            ui.color_edit_button_rgba_unmultiplied(&mut metadata.color_rgba);
            ui.add(egui::TextEdit::singleline(&mut metadata.name).desired_width(name_width));
        });
    }
}

pub(crate) fn import_task_status_text(task: &ImportTask) -> String {
    task.latest_event
        .as_ref()
        .map(import_progress_message)
        .unwrap_or_else(|| "Preparing import".to_owned())
}

pub(crate) fn import_progress_message(event: &ImportProgressEvent) -> String {
    match event {
        ImportProgressEvent::DiscoveredInput { file_count } => {
            format!("Discovered {file_count} TIFF file(s)")
        }
        ImportProgressEvent::EstimatedStorage { estimate } => {
            format!(
                "Estimated native package size {}",
                format_byte_quantity(estimate.estimated_total_bytes)
            )
        }
        ImportProgressEvent::ReadStack {
            completed, total, ..
        } => {
            format!("Reading TIFF stack {completed}/{total}")
        }
        ImportProgressEvent::BuiltScale { channel, level } => {
            format!("Built channel {channel} scale s{level}")
        }
        ImportProgressEvent::WritingPackage { output_package } => {
            format!("Writing {}", output_package.display())
        }
        ImportProgressEvent::Finished { output_package } => {
            format!("Finished {}", output_package.display())
        }
    }
}

pub(crate) fn import_progress_fraction(event: Option<&ImportProgressEvent>) -> Option<f32> {
    match event? {
        ImportProgressEvent::DiscoveredInput { .. } => Some(0.02),
        ImportProgressEvent::EstimatedStorage { .. } => Some(0.04),
        ImportProgressEvent::ReadStack {
            completed, total, ..
        } => {
            if *total == 0 {
                None
            } else {
                Some((0.05 + 0.55 * (*completed as f32 / *total as f32)).clamp(0.0, 0.60))
            }
        }
        ImportProgressEvent::BuiltScale { level, .. } => {
            Some((0.60 + 0.05 * *level as f32).clamp(0.60, 0.85))
        }
        ImportProgressEvent::WritingPackage { .. } => Some(0.90),
        ImportProgressEvent::Finished { .. } => Some(1.0),
    }
}
