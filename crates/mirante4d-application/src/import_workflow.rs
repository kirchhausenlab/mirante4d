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
    Configure(ImportSetupSnapshot),
    Inspecting(ImportInspectionSnapshot),
    Review(ImportReviewSnapshot),
    Importing(ImportExecutionSnapshot),
    Failed(ImportFailureSnapshot),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportSetupSnapshot {
    pub setup_id: u64,
    pub channels: Vec<ImportChannelSetupSnapshot>,
    pub active_inspection: Option<usize>,
    pub active_inspection_progress: Option<ImportInspectionProgressSnapshot>,
    pub validation_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportInspectionProgressSnapshot {
    pub inspected_files: u64,
    pub total_files: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportChannelSetupSnapshot {
    pub label: String,
    pub source_kind: ImportChannelSourceKind,
    pub selected_path: Option<String>,
    pub inspection: Option<ImportChannelInspectionSnapshot>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportChannelInspectionSnapshot {
    pub timepoints: u64,
    pub depth: u64,
    pub height: u64,
    pub width: u64,
    pub dtype: ImportSourceDtype,
    pub source_bytes: u64,
    pub file_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportChannelSourceKind {
    Single3dTiff,
    FolderOf3dTiffs,
    FolderOf2dTiffs,
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
    pub shape: ImportShapeSnapshot,
    pub source_dtype: ImportSourceDtype,
    pub source_bytes: u64,
    pub capacity: ImportCapacitySnapshot,
    pub ome_spacing_zyx_um: Option<[f64; 3]>,
    pub initial_draft: ImportReviewDraft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportCapacitySnapshot {
    pub decoded_base_bytes: u64,
    pub logical_output_bytes: u64,
    pub final_package_upper_bound: u64,
    pub bounded_unit_scratch_bytes: u64,
    pub maximum_unit_output_upper_bound: u64,
    pub finalization_headroom_bytes: u64,
    pub start_required_headroom_bytes: u64,
    pub destination_available_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImportReviewDraft {
    pub spacing_zyx_um: [f64; 3],
    pub calibration_confirmed: bool,
    pub time_step_seconds: Option<f64>,
    pub no_data_value_rule: Option<ImportNoDataValueRule>,
    pub hide_constant_z_planes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportNoDataValueRule {
    Automatic,
    ManualUint8(u8),
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
    pub storage: Option<ImportStorageProgressSnapshot>,
    pub cancellation_requested: bool,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportStorageProgressSnapshot {
    pub completed_temporal_units: u64,
    pub total_temporal_units: u64,
    pub active_timepoint: Option<u64>,
    pub active_channel: Option<u32>,
    pub preparing_timepoint: Option<u64>,
    pub preparing_channel: Option<u32>,
    pub preparing_completed_planes: u64,
    pub preparing_total_planes: u64,
    pub prepared_temporal_units: u32,
    pub temporal_pipeline_width: u32,
    pub stage_payload_bytes: u64,
    pub remaining_package_output_upper_bound: u64,
    pub unit_scratch_bytes: u64,
    pub decode_ahead_scratch_bytes: u64,
    pub additional_headroom_required_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportProgressSnapshot {
    Preparing,
    Stage {
        name: &'static str,
        completed_work_units: Option<u64>,
        total_work_units: Option<u64>,
    },
    Published,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportFailureSnapshot {
    pub message: String,
    pub checkpoint: Option<String>,
    pub recovery: Option<ImportRecoverySnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportRecoverySnapshot {
    pub retry_id: ImportReviewId,
    pub action: ImportRecoveryAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportRecoveryAction {
    Resume,
    ResetAndRestart,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImportCommand {
    BeginSetup,
    SetChannelCount {
        count: usize,
    },
    SetChannelLabel {
        channel: usize,
        label: String,
    },
    SetChannelSourceKind {
        channel: usize,
        kind: ImportChannelSourceKind,
    },
    ChooseChannelSource {
        channel: usize,
    },
    ValidateChannels,
    CancelSetup,
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
    RecoverCheckpoint {
        retry_id: ImportReviewId,
        action: ImportRecoveryAction,
    },
}
