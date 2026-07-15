//! Native import coordination and application-facing projection.

use std::{
    fs,
    path::{Path, PathBuf},
};

use mirante4d_application::import_workflow::{
    ImportExecutionSnapshot, ImportFailureSnapshot, ImportInspectionSnapshot,
    ImportProgressSnapshot, ImportReviewDraft, ImportReviewId, ImportReviewSnapshot,
    ImportShapeSnapshot, ImportSourceDtype, ImportSourceLayout, ImportWorkflowSnapshot,
};
use mirante4d_domain::IntensityDType;
use mirante4d_import_pipeline::{
    ImportEvent, ImportOptions, NoDataPolicy, SourceLayout, SpatialCalibration, TiffInspection,
    TiffSource, select_supported_profile,
};
use mirante4d_storage::ProfileKind;

use crate::import_worker_service::{ImportWorkerService, ImportWorkerStatus};

const MIB: u64 = 1024 * 1024;
const DEFAULT_IMPORT_WORKING_MEMORY_BYTES: u64 = 256 * MIB;
pub(crate) const IMPORT_WORKING_MEMORY_CHOICES: [u64; 4] =
    [128 * MIB, 256 * MIB, 512 * MIB, 1024 * MIB];

#[derive(Debug, Clone)]
pub(crate) struct PendingImportReview {
    pub(crate) id: ImportReviewId,
    pub(crate) source: TiffSource,
    pub(crate) inspection: TiffInspection,
    pub(crate) destination: PathBuf,
    pub(crate) initial_draft: ImportReviewDraft,
}

impl PendingImportReview {
    fn new(
        id: ImportReviewId,
        source: TiffSource,
        inspection: TiffInspection,
        destination: PathBuf,
    ) -> Self {
        Self {
            id,
            source,
            initial_draft: ImportReviewDraft {
                spacing_zyx_um: inspection.ome_spacing_zyx_um.unwrap_or([1.0, 1.0, 1.0]),
                calibration_confirmed: false,
                time_step_seconds: None,
                no_data_sentinel: None,
                working_memory_bytes: DEFAULT_IMPORT_WORKING_MEMORY_BYTES,
            },
            inspection,
            destination,
        }
    }
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

pub(crate) fn reset_checkpoint_directory(path: &Path) -> anyhow::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        anyhow::bail!("the checkpoint path is not a real directory");
    }

    let allowed = ["header", "journal", "payload"];
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("the checkpoint contains a non-UTF-8 entry"))?;
        if !allowed.contains(&name.as_str()) {
            anyhow::bail!("the checkpoint contains an unrelated entry: {name}");
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            anyhow::bail!("checkpoint entry {name} is not a regular file");
        }
        entries.push(entry.path());
    }
    for entry in entries {
        fs::remove_file(entry)?;
    }
    fs::remove_dir(path)?;
    Ok(())
}

pub(crate) struct ImportWorkflow {
    pub(crate) workers: ImportWorkerService,
    pub(crate) pending_review: Option<PendingImportReview>,
    pub(crate) problem: Option<String>,
    pub(crate) checkpoint_retry: Option<(ImportReviewId, ImportOptions)>,
    next_review_id: u64,
}

impl ImportWorkflow {
    pub(crate) const fn new() -> Self {
        Self {
            workers: ImportWorkerService::new(),
            pending_review: None,
            problem: None,
            checkpoint_retry: None,
            next_review_id: 1,
        }
    }

    pub(crate) fn snapshot(&self) -> ImportWorkflowSnapshot {
        match self.workers.status() {
            ImportWorkerStatus::Inspecting {
                source,
                destination,
                cancellation_requested,
            } => ImportWorkflowSnapshot::Inspecting(ImportInspectionSnapshot {
                source: source.path.display().to_string(),
                destination: destination.display().to_string(),
                cancellation_requested,
            }),
            ImportWorkerStatus::Importing {
                destination,
                latest_event,
                cancellation_requested,
            } => ImportWorkflowSnapshot::Importing(ImportExecutionSnapshot {
                destination: destination.display().to_string(),
                progress: latest_event
                    .as_ref()
                    .map(import_progress_snapshot)
                    .unwrap_or(ImportProgressSnapshot::Preparing),
                cancellation_requested,
            }),
            ImportWorkerStatus::Idle => {
                if let Some(message) = self.problem.as_ref() {
                    ImportWorkflowSnapshot::Failed(ImportFailureSnapshot {
                        message: message.clone(),
                        checkpoint: self
                            .checkpoint_retry
                            .as_ref()
                            .map(|(_, options)| options.checkpoint_directory.display().to_string()),
                        retry_id: self.checkpoint_retry.as_ref().map(|(id, _)| *id),
                    })
                } else if let Some(review) = self.pending_review.as_ref() {
                    ImportWorkflowSnapshot::Review(review_snapshot(review))
                } else {
                    ImportWorkflowSnapshot::Idle
                }
            }
        }
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
        ));
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
        self.checkpoint_retry = None;
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
        profile: ProfileKind::Ds0,
        calibration: SpatialCalibration::new(draft.spacing_zyx_um),
        time_step_seconds: draft.time_step_seconds,
        no_data: draft.no_data_sentinel.map(NoDataPolicy::U8Sentinel),
        working_memory_bytes: draft.working_memory_bytes,
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
    if draft.no_data_sentinel.is_some() && review.inspection.dtype != IntensityDType::Uint8 {
        anyhow::bail!("a no-data sentinel is supported only for uint8 TIFF input");
    }
    if !IMPORT_WORKING_MEMORY_CHOICES.contains(&draft.working_memory_bytes) {
        anyhow::bail!("select one of the offered import memory limits");
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
        source: review.source.path.display().to_string(),
        destination: review.destination.display().to_string(),
        source_layout: match review.inspection.layout {
            SourceLayout::Auto => ImportSourceLayout::Automatic,
            SourceLayout::MultipageStacks => ImportSourceLayout::MultipageStacks,
            SourceLayout::ChannelFoldersOfPlanes => ImportSourceLayout::ChannelFoldersOfPlanes,
        },
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
        ome_spacing_zyx_um: review.inspection.ome_spacing_zyx_um,
        initial_draft: review.initial_draft,
        working_memory_choices: IMPORT_WORKING_MEMORY_CHOICES,
    }
}

fn import_progress_snapshot(event: &ImportEvent) -> ImportProgressSnapshot {
    match event {
        ImportEvent::Producing {
            completed_work_units,
            total_work_units,
        } => ImportProgressSnapshot::Producing {
            completed_work_units: *completed_work_units,
            total_work_units: *total_work_units,
        },
        ImportEvent::HashingScience => ImportProgressSnapshot::HashingScience,
        ImportEvent::Publishing => ImportProgressSnapshot::Publishing,
        ImportEvent::Finished => ImportProgressSnapshot::Finished,
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
    fn checkpoint_reset_removes_only_the_known_checkpoint_entries() {
        let temp = tempfile::tempdir().unwrap();
        let checkpoint = temp.path().join(".cells.m4d.import-checkpoint");
        fs::create_dir(&checkpoint).unwrap();
        for name in ["header", "journal", "payload"] {
            fs::write(checkpoint.join(name), name).unwrap();
        }

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
}
