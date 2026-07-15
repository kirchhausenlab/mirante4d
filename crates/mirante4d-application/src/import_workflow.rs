//! Framework-neutral snapshots and commands for native import.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImportReviewId(u64);

impl ImportReviewId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum ImportWorkflowSnapshot {
    #[default]
    Idle,
    Inspecting(ImportInspectionSnapshot),
    Review(ImportReviewSnapshot),
    Importing(ImportExecutionSnapshot),
    Failed(ImportFailureSnapshot),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportInspectionSnapshot {
    pub source: String,
    pub destination: String,
    pub cancellation_requested: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportReviewSnapshot {
    pub review_id: ImportReviewId,
    pub source: String,
    pub destination: String,
    pub source_layout: ImportSourceLayout,
    pub shape: ImportShapeSnapshot,
    pub source_dtype: ImportSourceDtype,
    pub source_bytes: u64,
    pub ome_spacing_zyx_um: Option<[f64; 3]>,
    pub initial_draft: ImportReviewDraft,
    pub working_memory_choices: [u64; 4],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImportReviewDraft {
    pub spacing_zyx_um: [f64; 3],
    pub calibration_confirmed: bool,
    pub time_step_seconds: Option<f64>,
    pub no_data_sentinel: Option<u8>,
    pub working_memory_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSourceLayout {
    Automatic,
    MultipageStacks,
    ChannelFoldersOfPlanes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSourceDtype {
    Uint8,
    Uint16,
    Float32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportShapeSnapshot {
    pub timepoints: u64,
    pub channels: u32,
    pub depth: u64,
    pub height: u64,
    pub width: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportExecutionSnapshot {
    pub destination: String,
    pub progress: ImportProgressSnapshot,
    pub cancellation_requested: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportProgressSnapshot {
    Preparing,
    Producing {
        completed_work_units: u64,
        total_work_units: u64,
    },
    HashingScience,
    Publishing,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportFailureSnapshot {
    pub message: String,
    pub checkpoint: Option<String>,
    pub retry_id: Option<ImportReviewId>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImportCommand {
    CancelInspection,
    Start {
        review_id: ImportReviewId,
        draft: ImportReviewDraft,
    },
    CancelReview {
        review_id: ImportReviewId,
    },
    CancelImport,
    DismissProblem,
    ResetCheckpointAndRestart {
        retry_id: ImportReviewId,
    },
}
