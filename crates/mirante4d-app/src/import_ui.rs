use std::{
    path::{Path, PathBuf},
    sync::mpsc::Receiver,
    thread::JoinHandle,
};

use eframe::egui;
use mirante4d_application::{ApplicationSnapshot, OperationToken, WorkspaceSnapshot};
use mirante4d_dataset::ResourceValidity;
use mirante4d_domain::{IntensityDType, ScaleLevel};
use mirante4d_import_pipeline::{
    ImportCancellation, ImportError, ImportEvent, ImportOptions, ImportReceipt, NoDataPolicy,
    SourceLayout, SpatialCalibration, TiffInspection, TiffSource, select_supported_profile,
};
use mirante4d_storage::ProfileKind;

use crate::ui_kit;

const MIB: u64 = 1024 * 1024;
const DEFAULT_IMPORT_WORKING_MEMORY_BYTES: u64 = 256 * MIB;
const IMPORT_WORKING_MEMORY_CHOICES: [u64; 4] = [128 * MIB, 256 * MIB, 512 * MIB, 1024 * MIB];

pub(crate) struct TiffImportSetupTask {
    pub(crate) source: TiffSource,
    pub(crate) destination: PathBuf,
    pub(crate) cancellation: ImportCancellation,
    pub(crate) receiver: Receiver<TiffImportSetupTaskMessage>,
    pub(crate) worker: Option<JoinHandle<()>>,
}

impl Drop for TiffImportSetupTask {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::error!("TIFF inspection worker panicked");
        }
    }
}

pub(crate) enum TiffImportSetupTaskMessage {
    Finished(Result<TiffInspection, ImportError>),
}

#[derive(Debug, Clone)]
pub(crate) struct PendingTiffImport {
    pub(crate) source: TiffSource,
    pub(crate) inspection: TiffInspection,
    pub(crate) destination: PathBuf,
    pub(crate) calibration: SpatialCalibration,
    pub(crate) calibration_confirmed: bool,
    pub(crate) time_step_seconds: Option<f64>,
    pub(crate) no_data_sentinel: Option<u8>,
    pub(crate) working_memory_bytes: u64,
}

impl PendingTiffImport {
    pub(crate) fn from_inspection(
        source: TiffSource,
        inspection: TiffInspection,
        destination: PathBuf,
    ) -> Self {
        let calibration =
            SpatialCalibration::new(inspection.ome_spacing_zyx_um.unwrap_or([1.0, 1.0, 1.0]));
        Self {
            source,
            inspection,
            destination,
            calibration,
            calibration_confirmed: false,
            time_step_seconds: None,
            no_data_sentinel: None,
            working_memory_bytes: DEFAULT_IMPORT_WORKING_MEMORY_BYTES,
        }
    }
}

pub(crate) struct ImportTask {
    pub(crate) token: OperationToken,
    pub(crate) destination: PathBuf,
    pub(crate) cancellation: ImportCancellation,
    pub(crate) receiver: Receiver<ImportTaskMessage>,
    pub(crate) latest_event: Option<ImportEvent>,
    pub(crate) worker: Option<JoinHandle<()>>,
}

impl Drop for ImportTask {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::error!("TIFF import worker panicked");
        }
    }
}

pub(crate) enum ImportTaskMessage {
    Progress(ImportEvent),
    Finished(Result<ImportReceipt, ImportError>),
}

pub(crate) fn tiff_destination(source: &TiffSource, output_parent: &Path) -> PathBuf {
    let name = source
        .path
        .file_stem()
        .or_else(|| source.path.file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("imported-dataset");
    let slug = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() {
        "imported-dataset"
    } else {
        slug
    };
    output_parent.join(format!("{slug}.m4d"))
}

pub(crate) fn build_import_options(pending: &PendingTiffImport) -> anyhow::Result<ImportOptions> {
    validate_pending_tiff_import(pending)?;
    let mut options = ImportOptions {
        inspection: pending.inspection.clone(),
        destination: pending.destination.clone(),
        checkpoint_directory: checkpoint_directory(&pending.destination)?,
        profile: ProfileKind::Ds0,
        calibration: pending.calibration,
        time_step_seconds: pending.time_step_seconds,
        no_data: pending.no_data_sentinel.map(NoDataPolicy::U8Sentinel),
        working_memory_bytes: pending.working_memory_bytes,
    };
    options.profile = select_supported_profile(&options)?;
    Ok(options)
}

fn checkpoint_directory(destination: &Path) -> anyhow::Result<PathBuf> {
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("the import destination needs a parent directory"))?;
    let name = destination
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("the import destination needs a package name"))?
        .to_string_lossy();
    Ok(parent.join(format!(".{name}.import-checkpoint")))
}

pub(crate) fn validate_pending_tiff_import(pending: &PendingTiffImport) -> anyhow::Result<()> {
    if !pending.calibration_confirmed {
        anyhow::bail!("review the spatial calibration before starting the import");
    }
    for (axis, spacing) in ["z", "y", "x"]
        .into_iter()
        .zip(pending.calibration.spacing_zyx_um)
    {
        if !spacing.is_finite() || spacing <= 0.0 {
            anyhow::bail!("{axis} spacing must be positive and finite");
        }
    }
    if pending
        .time_step_seconds
        .is_some_and(|value| !value.is_finite() || value <= 0.0)
    {
        anyhow::bail!("the time step must be positive and finite");
    }
    if pending.no_data_sentinel.is_some() && pending.inspection.dtype != IntensityDType::Uint8 {
        anyhow::bail!("a no-data sentinel is supported only for uint8 TIFF input");
    }
    if !IMPORT_WORKING_MEMORY_CHOICES.contains(&pending.working_memory_bytes) {
        anyhow::bail!("select one of the offered import memory limits");
    }
    if pending.destination.try_exists()? {
        anyhow::bail!(
            "the destination already exists; imports create a new package and never replace one"
        );
    }
    Ok(())
}

pub(crate) fn pending_tiff_import_ready_to_start(pending: &PendingTiffImport) -> bool {
    pending.calibration_confirmed
        && pending
            .calibration
            .spacing_zyx_um
            .iter()
            .all(|value| value.is_finite() && *value > 0.0)
        && pending
            .time_step_seconds
            .is_none_or(|value| value.is_finite() && value > 0.0)
        && (pending.no_data_sentinel.is_none() || pending.inspection.dtype == IntensityDType::Uint8)
        && IMPORT_WORKING_MEMORY_CHOICES.contains(&pending.working_memory_bytes)
}

pub(crate) fn show_pending_tiff_import_controls(
    ui: &mut egui::Ui,
    pending: &mut PendingTiffImport,
) {
    ui_kit::property_row(ui, "source", pending.source.path.display());
    ui_kit::property_row(ui, "destination", pending.destination.display());
    ui_kit::property_row(
        ui,
        "layout",
        match pending.inspection.layout {
            SourceLayout::Auto => "automatic",
            SourceLayout::MultipageStacks => "multipage stacks",
            SourceLayout::ChannelFoldersOfPlanes => "channel folders of planes",
        },
    );
    ui_kit::property_row(
        ui,
        "shape",
        format!(
            "t{} c{} z{} y{} x{}",
            pending.inspection.shape.t(),
            pending.inspection.channels,
            pending.inspection.shape.z(),
            pending.inspection.shape.y(),
            pending.inspection.shape.x()
        ),
    );
    ui_kit::property_row(
        ui,
        "source dtype",
        format!("{:?}", pending.inspection.dtype),
    );
    ui_kit::property_row(
        ui,
        "source size",
        format_byte_quantity(pending.inspection.source_bytes),
    );
    ui_kit::property_row(
        ui,
        "calibration metadata",
        match pending.inspection.ome_spacing_zyx_um {
            Some(spacing) => format!(
                "OME z {:.4}, y {:.4}, x {:.4} micrometers",
                spacing[0], spacing[1], spacing[2]
            ),
            None => "not present; enter calibrated values".to_owned(),
        },
    );

    ui.add_space(6.0);
    ui.label("spatial calibration (micrometers)");
    ui.horizontal_wrapped(|ui| {
        ui.add(
            egui::DragValue::new(&mut pending.calibration.spacing_zyx_um[0])
                .speed(0.01)
                .prefix("z "),
        );
        ui.add(
            egui::DragValue::new(&mut pending.calibration.spacing_zyx_um[1])
                .speed(0.01)
                .prefix("y "),
        );
        ui.add(
            egui::DragValue::new(&mut pending.calibration.spacing_zyx_um[2])
                .speed(0.01)
                .prefix("x "),
        );
    });
    ui.checkbox(
        &mut pending.calibration_confirmed,
        "spatial calibration reviewed",
    );

    ui.add_space(6.0);
    let mut time_step_enabled = pending.time_step_seconds.is_some();
    if ui
        .checkbox(&mut time_step_enabled, "regular time step")
        .changed()
    {
        pending.time_step_seconds = time_step_enabled.then_some(1.0);
    }
    if let Some(time_step) = pending.time_step_seconds.as_mut() {
        ui.horizontal(|ui| {
            ui.label("seconds per timepoint");
            ui.add(egui::DragValue::new(time_step).speed(0.01));
        });
    }

    ui.add_space(6.0);
    let sentinel_supported = pending.inspection.dtype == IntensityDType::Uint8;
    if !sentinel_supported {
        pending.no_data_sentinel = None;
    }
    let mut sentinel_enabled = pending.no_data_sentinel.is_some();
    if ui
        .add_enabled(
            sentinel_supported,
            egui::Checkbox::new(&mut sentinel_enabled, "uint8 no-data sentinel"),
        )
        .changed()
    {
        pending.no_data_sentinel = sentinel_enabled.then_some(255);
    }
    if let Some(sentinel) = pending.no_data_sentinel.as_mut() {
        let mut value = u16::from(*sentinel);
        ui.horizontal(|ui| {
            ui.label("sentinel value");
            if ui
                .add(egui::DragValue::new(&mut value).range(0..=255))
                .changed()
            {
                *sentinel = value as u8;
            }
        });
    } else if !sentinel_supported {
        ui_kit::property_row(ui, "no-data sentinel", "available only for uint8 sources");
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("working memory");
        egui::ComboBox::from_id_salt("tiff-import-working-memory")
            .selected_text(format_byte_quantity(pending.working_memory_bytes))
            .show_ui(ui, |ui| {
                for bytes in IMPORT_WORKING_MEMORY_CHOICES {
                    ui.selectable_value(
                        &mut pending.working_memory_bytes,
                        bytes,
                        format_byte_quantity(bytes),
                    );
                }
            });
    });
    ui_kit::property_row(
        ui,
        "publication",
        "create new package; never replace source or output",
    );
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

pub(crate) fn import_task_status_text(task: &ImportTask) -> String {
    task.latest_event
        .as_ref()
        .map(import_progress_message)
        .unwrap_or_else(|| "Preparing import".to_owned())
}

pub(crate) fn import_progress_message(event: &ImportEvent) -> String {
    match event {
        ImportEvent::Producing {
            completed_work_units,
            total_work_units,
        } => format!("Building package {completed_work_units}/{total_work_units}"),
        ImportEvent::HashingScience => "Checking scientific content".to_owned(),
        ImportEvent::Publishing => "Validating and publishing package".to_owned(),
        ImportEvent::Finished => "Import finished".to_owned(),
    }
}

pub(crate) fn import_progress_fraction(event: Option<&ImportEvent>) -> Option<f32> {
    match event? {
        ImportEvent::Producing {
            completed_work_units,
            total_work_units,
        } => {
            if *total_work_units == 0 {
                None
            } else {
                Some(
                    (0.05 + 0.70 * (*completed_work_units as f32 / *total_work_units as f32))
                        .clamp(0.05, 0.75),
                )
            }
        }
        ImportEvent::HashingScience => Some(0.80),
        ImportEvent::Publishing => Some(0.90),
        ImportEvent::Finished => Some(1.0),
    }
}

fn format_byte_quantity(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB_F: f64 = 1024.0 * KIB;
    const GIB: f64 = 1024.0 * MIB_F;
    let bytes_f = bytes as f64;
    if bytes_f >= GIB {
        format!("{:.2} GiB", bytes_f / GIB)
    } else if bytes_f >= MIB_F {
        format!("{:.2} MiB", bytes_f / MIB_F)
    } else if bytes_f >= KIB {
        format!("{:.1} KiB", bytes_f / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_is_a_create_only_package_name_under_the_selected_parent() {
        let source = TiffSource::auto("/source/My Cells.ome.tiff");
        let destination = tiff_destination(&source, Path::new("/output"));

        assert_eq!(destination, Path::new("/output/my-cells-ome.m4d"));
        assert_eq!(
            checkpoint_directory(&destination).unwrap(),
            Path::new("/output/.my-cells-ome.m4d.import-checkpoint")
        );
        assert!(
            !checkpoint_directory(&destination)
                .unwrap()
                .starts_with(&destination)
        );
    }

    #[test]
    fn progress_is_coarse_and_monotonic() {
        let producing = ImportEvent::Producing {
            completed_work_units: 5,
            total_work_units: 10,
        };
        assert_eq!(import_progress_fraction(Some(&producing)), Some(0.4));
        assert!(
            import_progress_fraction(Some(&ImportEvent::HashingScience))
                < import_progress_fraction(Some(&ImportEvent::Publishing))
        );
        assert_eq!(
            import_progress_fraction(Some(&ImportEvent::Finished)),
            Some(1.0)
        );
    }
}
