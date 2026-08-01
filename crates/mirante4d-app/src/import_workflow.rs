//! Native import coordination and application-facing projection.

use std::{
    fs,
    path::{Path, PathBuf},
};

use mirante4d_application::import_workflow::{
    ImportCapacitySnapshot, ImportChannelInspectionSnapshot, ImportChannelSetupSnapshot,
    ImportChannelSourceKind, ImportExecutionSnapshot, ImportFailureSnapshot,
    ImportInspectionProgressSnapshot, ImportInspectionSnapshot, ImportNoDataValueRule,
    ImportProgressSnapshot, ImportRecoveryAction, ImportRecoverySnapshot, ImportReviewDraft,
    ImportReviewId, ImportReviewSnapshot, ImportSetupSnapshot, ImportShapeSnapshot,
    ImportSourceDtype, ImportStorageProgressSnapshot, ImportWorkflowSnapshot,
};
use mirante4d_domain::IntensityDType;
pub(crate) use mirante4d_import_pipeline::deterministic_tiff_destination as tiff_destination;
use mirante4d_import_pipeline::{
    ImportCapacityPlan, ImportEvent, ImportOptions, ImportStage, NoDataPolicy, NoDataValueRule,
    SpatialCalibration, TiffInspection, TiffSource, combine_tiff_channel_inspections,
    import_capacity_plan, relabel_tiff_channel_inspection, select_supported_profile,
};
use mirante4d_storage::ProfileKind;

use crate::import_worker_service::{ImportWorkerService, ImportWorkerStatus};

#[derive(Debug, Clone)]
pub(crate) struct PendingImportReview {
    pub(crate) id: ImportReviewId,
    pub(crate) source: TiffSource,
    pub(crate) inspection: TiffInspection,
    pub(crate) destination: PathBuf,
    pub(crate) initial_draft: ImportReviewDraft,
    pub(crate) capacity: ImportCapacitySnapshot,
}

impl PendingImportReview {
    fn new(
        id: ImportReviewId,
        source: TiffSource,
        inspection: TiffInspection,
        destination: PathBuf,
    ) -> anyhow::Result<Self> {
        let mut capacity_options = ImportOptions {
            inspection: inspection.clone(),
            destination: destination.clone(),
            checkpoint_directory: checkpoint_directory(&destination)?,
            profile: ProfileKind::Current,
            calibration: SpatialCalibration::new([1.0; 3]),
            time_step_seconds: None,
            // The review reports the conservative explicit-validity route;
            // disabling no-data can only reduce this upper bound.
            no_data: Some(NoDataPolicy::automatic()),
        };
        capacity_options.profile = select_supported_profile(&capacity_options)?;
        let ImportCapacityPlan {
            decoded_base_bytes,
            logical_output_bytes,
            final_package_upper_bound,
            bounded_unit_scratch_bytes,
            maximum_unit_output_upper_bound,
            finalization_headroom_bytes,
            start_required_headroom_bytes,
            ..
        } = import_capacity_plan(&capacity_options)?;
        let destination_available_bytes = destination
            .parent()
            .and_then(|parent| rustix::fs::statvfs(parent).ok())
            .and_then(|facts| facts.f_bavail.checked_mul(facts.f_frsize));
        Ok(Self {
            id,
            source,
            initial_draft: ImportReviewDraft {
                spacing_zyx_um: inspection.ome_spacing_zyx_um.unwrap_or([1.0, 1.0, 1.0]),
                calibration_confirmed: false,
                time_step_seconds: None,
                no_data_value_rule: None,
                hide_constant_z_planes: false,
            },
            capacity: ImportCapacitySnapshot {
                decoded_base_bytes,
                logical_output_bytes,
                final_package_upper_bound,
                bounded_unit_scratch_bytes,
                maximum_unit_output_upper_bound,
                finalization_headroom_bytes,
                start_required_headroom_bytes,
                destination_available_bytes,
            },
            inspection,
            destination,
        })
    }
}

pub(crate) fn reset_checkpoint_directory(path: &Path) -> anyhow::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        anyhow::bail!("the checkpoint path is not a real directory");
    }

    let control = path.join(".mirante4d-import-control");
    if !control.is_dir() {
        anyhow::bail!("the checkpoint is not a current Mirante4D final-layout import stage");
    }
    let header = fs::read(control.join("stage-header"))?;
    if !header.starts_with(b"mirante4d-final-layout-stage-v1\0") {
        anyhow::bail!("the checkpoint has an invalid final-layout stage header");
    }
    validate_checkpoint_tree(path)?;
    fs::remove_dir_all(path)?;
    Ok(())
}

fn validate_checkpoint_tree(root: &Path) -> anyhow::Result<()> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                anyhow::bail!("the checkpoint contains a symbolic link");
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if !metadata.is_file() {
                anyhow::bail!("the checkpoint contains a non-regular entry");
            }
        }
    }
    Ok(())
}

pub(crate) struct ImportWorkflow {
    pub(crate) workers: ImportWorkerService,
    pub(crate) pending_review: Option<PendingImportReview>,
    pub(crate) problem: Option<String>,
    pub(crate) checkpoint_recovery: Option<PendingImportRecovery>,
    pub(crate) setup: Option<PreprocessingSetup>,
    pub(crate) active_setup_inspection: Option<(usize, u64)>,
    next_review_id: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingImportRecovery {
    pub(crate) id: ImportReviewId,
    pub(crate) options: ImportOptions,
    pub(crate) action: ImportRecoveryAction,
}

#[derive(Debug)]
pub(crate) struct PreprocessingSetup {
    pub(crate) id: u64,
    pub(crate) channels: Vec<PreprocessingChannel>,
    pub(crate) validation_error: Option<String>,
}

#[derive(Debug)]
pub(crate) struct PreprocessingChannel {
    pub(crate) label: String,
    pub(crate) source_kind: ImportChannelSourceKind,
    pub(crate) selected_path: Option<PathBuf>,
    pub(crate) inspection: Option<TiffInspection>,
    pub(crate) error: Option<String>,
    pub(crate) revision: u64,
}

impl PreprocessingChannel {
    fn new(ordinal: usize) -> Self {
        Self {
            label: format!("channel {}", ordinal + 1),
            source_kind: ImportChannelSourceKind::Single3dTiff,
            selected_path: None,
            inspection: None,
            error: None,
            revision: 1,
        }
    }
}

impl ImportWorkflow {
    pub(crate) fn new() -> Self {
        Self {
            workers: ImportWorkerService::new(),
            pending_review: None,
            problem: None,
            checkpoint_recovery: None,
            setup: None,
            active_setup_inspection: None,
            next_review_id: 1,
        }
    }

    pub(crate) fn snapshot(&self) -> ImportWorkflowSnapshot {
        if let Some(setup) = self.setup.as_ref() {
            let inspection_progress = match self.workers.status() {
                ImportWorkerStatus::Inspecting { progress, .. } => {
                    progress.map(|progress| ImportInspectionProgressSnapshot {
                        inspected_files: progress.inspected_files,
                        total_files: progress.total_files,
                    })
                }
                _ => None,
            };
            return ImportWorkflowSnapshot::Configure(setup_snapshot(
                setup,
                self.active_setup_inspection.map(|(channel, _)| channel),
                inspection_progress,
            ));
        }
        match self.workers.status() {
            ImportWorkerStatus::Inspecting {
                source,
                destination,
                progress: _,
                cancellation_requested,
            } => ImportWorkflowSnapshot::Inspecting(ImportInspectionSnapshot {
                source: source.primary_path().display().to_string(),
                destination: destination.display().to_string(),
                cancellation_requested,
            }),
            ImportWorkerStatus::Importing {
                destination,
                latest_event,
                storage_progress,
                cancellation_requested,
                elapsed,
            } => ImportWorkflowSnapshot::Importing(ImportExecutionSnapshot {
                destination: destination.display().to_string(),
                progress: latest_event
                    .as_ref()
                    .map(import_progress_snapshot)
                    .unwrap_or(ImportProgressSnapshot::Preparing),
                storage: storage_progress.map(|progress| ImportStorageProgressSnapshot {
                    completed_temporal_units: progress.completed_temporal_units,
                    total_temporal_units: progress.total_temporal_units,
                    active_timepoint: progress.active_timepoint,
                    active_channel: progress.active_channel,
                    preparing_timepoint: progress.preparing_timepoint,
                    preparing_channel: progress.preparing_channel,
                    preparing_completed_planes: progress.preparing_completed_planes,
                    preparing_total_planes: progress.preparing_total_planes,
                    prepared_temporal_units: progress.prepared_temporal_units,
                    temporal_pipeline_width: progress.temporal_pipeline_width,
                    stage_payload_bytes: progress.stage_payload_bytes,
                    remaining_package_output_upper_bound: progress
                        .remaining_package_output_upper_bound,
                    unit_scratch_bytes: progress.unit_scratch_bytes,
                    decode_ahead_scratch_bytes: progress.decode_ahead_scratch_bytes,
                    additional_headroom_required_bytes: progress.additional_headroom_required_bytes,
                }),
                cancellation_requested,
                elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            }),
            ImportWorkerStatus::Idle => {
                if let Some(message) = self.problem.as_ref() {
                    ImportWorkflowSnapshot::Failed(ImportFailureSnapshot {
                        message: message.clone(),
                        checkpoint: self.checkpoint_recovery.as_ref().map(|recovery| {
                            recovery.options.checkpoint_directory.display().to_string()
                        }),
                        recovery: self.checkpoint_recovery.as_ref().map(|recovery| {
                            ImportRecoverySnapshot {
                                retry_id: recovery.id,
                                action: recovery.action,
                            }
                        }),
                    })
                } else if let Some(review) = self.pending_review.as_ref() {
                    ImportWorkflowSnapshot::Review(review_snapshot(review))
                } else {
                    ImportWorkflowSnapshot::Idle
                }
            }
        }
    }

    pub(crate) fn begin_setup(&mut self) {
        if self.workers.status().is_active() {
            return;
        }
        self.pending_review = None;
        self.problem = None;
        self.setup = Some(PreprocessingSetup {
            id: self.next_review_id,
            channels: vec![PreprocessingChannel::new(0)],
            validation_error: None,
        });
    }

    pub(crate) fn set_channel_count(&mut self, count: usize) {
        let Some(setup) = self.setup.as_mut() else {
            return;
        };
        let count = count.clamp(1, 64);
        while setup.channels.len() < count {
            setup
                .channels
                .push(PreprocessingChannel::new(setup.channels.len()));
        }
        setup.channels.truncate(count);
        if self
            .active_setup_inspection
            .is_some_and(|(channel, _)| channel >= count)
        {
            self.workers.cancel_inspection();
            self.active_setup_inspection = None;
        }
        setup.validation_error = None;
    }

    pub(crate) fn set_channel_label(&mut self, channel: usize, label: String) {
        let Some(row) = self
            .setup
            .as_mut()
            .and_then(|setup| setup.channels.get_mut(channel))
        else {
            return;
        };
        if row.label != label {
            row.label = label;
            row.revision = row.revision.saturating_add(1);
        }
        if let Some(setup) = self.setup.as_mut() {
            setup.validation_error = None;
        }
    }

    pub(crate) fn set_channel_kind(&mut self, channel: usize, kind: ImportChannelSourceKind) {
        let Some(row) = self
            .setup
            .as_mut()
            .and_then(|setup| setup.channels.get_mut(channel))
        else {
            return;
        };
        if row.source_kind != kind {
            row.source_kind = kind;
            row.selected_path = None;
            row.inspection = None;
            row.error = None;
            row.revision = row.revision.saturating_add(1);
        }
        if let Some(setup) = self.setup.as_mut() {
            setup.validation_error = None;
        }
    }

    pub(crate) fn install_channel_selection(&mut self, channel: usize, path: PathBuf) {
        let Some(row) = self
            .setup
            .as_mut()
            .and_then(|setup| setup.channels.get_mut(channel))
        else {
            return;
        };
        row.selected_path = Some(path);
        row.inspection = None;
        row.error = None;
        row.revision = row.revision.saturating_add(1);
        if let Some(setup) = self.setup.as_mut() {
            setup.validation_error = None;
        }
    }

    pub(crate) fn mark_channel_inspection_active(&mut self, channel: usize) {
        self.active_setup_inspection = self
            .setup
            .as_ref()
            .and_then(|setup| setup.channels.get(channel))
            .map(|row| (channel, row.revision));
    }

    pub(crate) fn take_current_inspection_channel(&mut self) -> Option<usize> {
        let (channel, revision) = self.active_setup_inspection.take()?;
        self.setup
            .as_ref()
            .and_then(|setup| setup.channels.get(channel))
            .filter(|row| row.revision == revision)
            .map(|_| channel)
    }

    pub(crate) fn cancel_setup(&mut self) {
        self.workers.cancel_inspection();
        self.setup = None;
        self.active_setup_inspection = None;
        self.problem = None;
    }

    pub(crate) fn validated_setup_inspection(&mut self) -> anyhow::Result<TiffInspection> {
        let setup = self
            .setup
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no preprocessing setup is active"))?;
        let mut inspections = Vec::with_capacity(setup.channels.len());
        for row in &setup.channels {
            let inspection = row.inspection.clone().ok_or_else(|| {
                anyhow::anyhow!("inspect every channel before validating channels")
            })?;
            inspections.push(relabel_tiff_channel_inspection(inspection, &row.label)?);
        }
        Ok(combine_tiff_channel_inspections(inspections)?)
    }

    pub(crate) fn install_review(
        &mut self,
        source: TiffSource,
        inspection: TiffInspection,
        destination: PathBuf,
    ) -> anyhow::Result<ImportReviewId> {
        let id = ImportReviewId::new(self.next_review_id);
        self.next_review_id = self
            .next_review_id
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("import review id space exhausted"))?;
        self.pending_review = Some(PendingImportReview::new(
            id,
            source,
            inspection,
            destination,
        )?);
        self.problem = None;
        Ok(id)
    }

    pub(crate) fn start_options(
        &mut self,
        review_id: ImportReviewId,
        draft: ImportReviewDraft,
    ) -> anyhow::Result<Option<ImportOptions>> {
        let Some(review) = self
            .pending_review
            .as_ref()
            .filter(|review| review.id == review_id)
        else {
            return Ok(None);
        };
        let options = build_import_options(review, draft)?;
        Ok(Some(options))
    }

    pub(crate) fn complete_review(&mut self, review_id: ImportReviewId) {
        if self
            .pending_review
            .as_ref()
            .is_some_and(|review| review.id == review_id)
        {
            self.pending_review = None;
            self.problem = None;
        }
    }

    pub(crate) fn cancel_review(&mut self, review_id: ImportReviewId) {
        if self
            .pending_review
            .as_ref()
            .is_some_and(|review| review.id == review_id)
        {
            self.pending_review = None;
            self.problem = None;
        }
    }

    pub(crate) fn clear_for_source_replacement(&mut self) {
        self.workers.shutdown();
        self.pending_review = None;
        self.problem = None;
        self.checkpoint_recovery = None;
        self.setup = None;
        self.active_setup_inspection = None;
    }
}

fn setup_snapshot(
    setup: &PreprocessingSetup,
    active_inspection: Option<usize>,
    active_inspection_progress: Option<ImportInspectionProgressSnapshot>,
) -> ImportSetupSnapshot {
    ImportSetupSnapshot {
        setup_id: setup.id,
        channels: setup
            .channels
            .iter()
            .map(|row| ImportChannelSetupSnapshot {
                label: row.label.clone(),
                source_kind: row.source_kind,
                selected_path: row
                    .selected_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
                inspection: row.inspection.as_ref().map(|inspection| {
                    ImportChannelInspectionSnapshot {
                        timepoints: inspection.shape.t(),
                        depth: inspection.shape.z(),
                        height: inspection.shape.y(),
                        width: inspection.shape.x(),
                        dtype: match inspection.dtype {
                            IntensityDType::Uint8 => ImportSourceDtype::Uint8,
                            IntensityDType::Uint16 => ImportSourceDtype::Uint16,
                            IntensityDType::Float32 => ImportSourceDtype::Float32,
                        },
                        source_bytes: inspection.source_bytes,
                        file_count: u64::try_from(inspection.file_count()).unwrap_or(u64::MAX),
                    }
                }),
                error: row.error.clone(),
            })
            .collect(),
        active_inspection,
        active_inspection_progress,
        validation_error: setup.validation_error.clone(),
    }
}

fn build_import_options(
    review: &PendingImportReview,
    draft: ImportReviewDraft,
) -> anyhow::Result<ImportOptions> {
    validate_review(review, draft)?;
    let mut options = ImportOptions {
        inspection: review.inspection.clone(),
        destination: review.destination.clone(),
        checkpoint_directory: checkpoint_directory(&review.destination)?,
        profile: ProfileKind::Current,
        calibration: SpatialCalibration::new(draft.spacing_zyx_um),
        time_step_seconds: draft.time_step_seconds,
        no_data: if draft.no_data_value_rule.is_some() || draft.hide_constant_z_planes {
            Some(NoDataPolicy::new(
                draft.no_data_value_rule.map(|rule| match rule {
                    ImportNoDataValueRule::Automatic => NoDataValueRule::Automatic,
                    ImportNoDataValueRule::ManualUint8(value) => {
                        NoDataValueRule::ManualUint8(value)
                    }
                }),
                draft.hide_constant_z_planes,
            ))
        } else {
            None
        },
    };
    options.profile = select_supported_profile(&options)?;
    Ok(options)
}

fn validate_review(review: &PendingImportReview, draft: ImportReviewDraft) -> anyhow::Result<()> {
    if !draft.calibration_confirmed {
        anyhow::bail!("review the spatial calibration before starting the import");
    }
    for (axis, spacing) in ["z", "y", "x"].into_iter().zip(draft.spacing_zyx_um) {
        if !spacing.is_finite() || spacing <= 0.0 {
            anyhow::bail!("{axis} spacing must be positive and finite");
        }
    }
    if draft
        .time_step_seconds
        .is_some_and(|value| !value.is_finite() || value <= 0.0)
    {
        anyhow::bail!("the time step must be positive and finite");
    }
    if matches!(
        draft.no_data_value_rule,
        Some(ImportNoDataValueRule::ManualUint8(_))
    ) && review.inspection.dtype != IntensityDType::Uint8
    {
        anyhow::bail!("manual no-data values are supported only for uint8 TIFF input");
    }
    if review.destination.try_exists()? {
        anyhow::bail!(
            "the destination already exists; imports create a new package and never replace one"
        );
    }
    Ok(())
}

fn review_snapshot(review: &PendingImportReview) -> ImportReviewSnapshot {
    ImportReviewSnapshot {
        review_id: review.id,
        source: review
            .source
            .channels()
            .iter()
            .map(|channel| format!("{}: {}", channel.label(), channel.path().display()))
            .collect::<Vec<_>>()
            .join("; "),
        destination: review.destination.display().to_string(),
        shape: ImportShapeSnapshot {
            timepoints: review.inspection.shape.t(),
            channels: review.inspection.channels,
            depth: review.inspection.shape.z(),
            height: review.inspection.shape.y(),
            width: review.inspection.shape.x(),
        },
        source_dtype: match review.inspection.dtype {
            IntensityDType::Uint8 => ImportSourceDtype::Uint8,
            IntensityDType::Uint16 => ImportSourceDtype::Uint16,
            IntensityDType::Float32 => ImportSourceDtype::Float32,
        },
        source_bytes: review.inspection.source_bytes,
        capacity: review.capacity,
        ome_spacing_zyx_um: review.inspection.ome_spacing_zyx_um,
        initial_draft: review.initial_draft,
    }
}

fn import_progress_snapshot(event: &ImportEvent) -> ImportProgressSnapshot {
    match event {
        ImportEvent::StageStarted {
            stage,
            completed_work_units,
            total_work_units,
        } => stage_progress_snapshot(*stage, Some(*completed_work_units), *total_work_units),
        ImportEvent::StageProgress {
            stage,
            completed_work_units,
            total_work_units,
        } => stage_progress_snapshot(*stage, Some(*completed_work_units), Some(*total_work_units)),
        ImportEvent::StageFinished(timing) => stage_progress_snapshot(timing.stage, None, None),
        ImportEvent::StorageProgress(_) => ImportProgressSnapshot::Preparing,
        ImportEvent::Published => ImportProgressSnapshot::Published,
    }
}

fn stage_progress_snapshot(
    stage: ImportStage,
    completed_work_units: Option<u64>,
    total_work_units: Option<u64>,
) -> ImportProgressSnapshot {
    ImportProgressSnapshot::Stage {
        name: stage.name(),
        completed_work_units,
        total_work_units,
    }
}

fn checkpoint_directory(destination: &std::path::Path) -> anyhow::Result<PathBuf> {
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("the import destination needs a parent directory"))?;
    let name = destination
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("the import destination needs a package name"))?
        .to_string_lossy();
    Ok(parent.join(format!(".{name}.import-checkpoint")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_progress_projects_named_stage_and_only_known_stage_work() {
        assert_eq!(
            import_progress_snapshot(&ImportEvent::StageStarted {
                stage: ImportStage::SourceIngest,
                completed_work_units: 0,
                total_work_units: None,
            }),
            ImportProgressSnapshot::Stage {
                name: "source-ingest",
                completed_work_units: Some(0),
                total_work_units: None,
            }
        );
        assert_eq!(
            import_progress_snapshot(&ImportEvent::StageProgress {
                stage: ImportStage::BaseProduction,
                completed_work_units: 7,
                total_work_units: 11,
            }),
            ImportProgressSnapshot::Stage {
                name: "base-production",
                completed_work_units: Some(7),
                total_work_units: Some(11),
            }
        );
        assert_eq!(
            import_progress_snapshot(&ImportEvent::StageFinished(
                mirante4d_import_pipeline::ImportStageTiming {
                    stage: ImportStage::BaseProduction,
                    wall_time_ns: 1,
                    cpu_time_ns: 1,
                },
            )),
            ImportProgressSnapshot::Stage {
                name: "base-production",
                completed_work_units: None,
                total_work_units: None,
            }
        );
    }

    #[test]
    fn shared_destination_keeps_its_checkpoint_as_a_sibling() {
        let source = TiffSource::single_3d("/source/My Cells.ome.tiff");
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
    fn checkpoint_reset_removes_only_a_bound_current_final_layout_stage() {
        let temp = tempfile::tempdir().unwrap();
        let checkpoint = temp.path().join(".cells.m4d.import-checkpoint");
        let control = checkpoint.join(".mirante4d-import-control");
        fs::create_dir_all(&control).unwrap();
        fs::write(
            control.join("stage-header"),
            b"mirante4d-final-layout-stage-v1\0fixture",
        )
        .unwrap();
        let payload = checkpoint.join("images/i00000000/s00/c/0");
        fs::create_dir_all(&payload).unwrap();
        fs::write(payload.join("0"), b"private staged payload").unwrap();

        reset_checkpoint_directory(&checkpoint).unwrap();

        assert!(!checkpoint.exists());
    }

    #[test]
    fn checkpoint_reset_preserves_a_directory_with_unrelated_content() {
        let temp = tempfile::tempdir().unwrap();
        let checkpoint = temp.path().join(".cells.m4d.import-checkpoint");
        fs::create_dir(&checkpoint).unwrap();
        fs::write(checkpoint.join("header"), b"checkpoint").unwrap();
        fs::write(checkpoint.join("notes.txt"), b"unrelated").unwrap();

        assert!(reset_checkpoint_directory(&checkpoint).is_err());
        assert_eq!(fs::read(checkpoint.join("header")).unwrap(), b"checkpoint");
        assert_eq!(
            fs::read(checkpoint.join("notes.txt")).unwrap(),
            b"unrelated"
        );
    }

    #[test]
    fn setup_inspection_completion_is_current_only_for_its_row_revision() {
        let mut workflow = ImportWorkflow::new();
        workflow.begin_setup();
        workflow.mark_channel_inspection_active(0);
        assert_eq!(workflow.take_current_inspection_channel(), Some(0));

        workflow.mark_channel_inspection_active(0);
        workflow.set_channel_label(0, "edited while inspecting".to_owned());
        assert_eq!(
            workflow.take_current_inspection_channel(),
            None,
            "a completion from the previous row generation must be discarded"
        );
        workflow.workers.shutdown();
    }
}
