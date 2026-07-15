use std::{
    fmt,
    path::{Path, PathBuf},
};

use mirante4d_domain::{IntensityDType, Shape4D};
use mirante4d_identity::{PackageId, ScientificContentId, Sha256Digest};
use mirante4d_storage::ProfileKind;

/// Strong digest type for the exact reviewed source generation.
///
/// Product consumers obtain this identity through the import boundary rather
/// than depending on the identity implementation directly.
pub type SourceFingerprint = Sha256Digest;

/// User-selected TIFF source root and layout interpretation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TiffSource {
    pub path: PathBuf,
    pub layout: SourceLayout,
}

impl TiffSource {
    pub fn auto(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            layout: SourceLayout::Auto,
        }
    }
}

/// Supported reviewed source layouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceLayout {
    Auto,
    MultipageStacks,
    ChannelFoldersOfPlanes,
}

/// Explicit spatial calibration in canonical micrometers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpatialCalibration {
    pub spacing_zyx_um: [f64; 3],
}

impl SpatialCalibration {
    pub const fn new(spacing_zyx_um: [f64; 3]) -> Self {
        Self { spacing_zyx_um }
    }
}

/// Optional reviewed source no-data rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoDataPolicy {
    U8Sentinel(u8),
}

/// Metadata-only source inspection accepted for an import plan.
#[derive(Clone, Debug, PartialEq)]
pub struct TiffInspection {
    pub(crate) source: TiffSource,
    pub(crate) files: Vec<InspectedSourceFile>,
    /// Conservative resident reservation covering the retained reviewed
    /// source index and its largest in-clock inventory/revalidation view.
    pub(crate) source_index_working_bytes: u64,
    pub layout: SourceLayout,
    pub shape: Shape4D,
    pub channels: u32,
    pub dtype: IntensityDType,
    pub ome_spacing_zyx_um: Option<[f64; 3]>,
    pub source_bytes: u64,
    pub source_fingerprint: SourceFingerprint,
    pub maximum_decoded_chunk_bytes: u64,
    pub maximum_encoded_chunk_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InspectedSourceFile {
    pub path: PathBuf,
    pub relative_name: String,
    pub channel: u32,
    pub timepoint: u64,
    pub first_z: u64,
    pub planes: u64,
    pub bytes: u64,
    pub sha256: Sha256Digest,
    /// Kernel-backed generation proved stable while this file was reviewed.
    /// It is deliberately excluded from scientific and package identity.
    pub generation: SourceFileGeneration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SourceFileGeneration {
    pub device: u64,
    pub inode: u64,
    pub bytes: u64,
    pub modified_seconds: i64,
    pub modified_nanoseconds: i64,
    pub changed_seconds: i64,
    pub changed_nanoseconds: i64,
}

/// One complete product import request.
#[derive(Clone, Debug)]
pub struct ImportOptions {
    pub inspection: TiffInspection,
    pub destination: PathBuf,
    pub checkpoint_directory: PathBuf,
    pub profile: ProfileKind,
    pub calibration: SpatialCalibration,
    pub time_step_seconds: Option<f64>,
    pub no_data: Option<NoDataPolicy>,
    pub working_memory_bytes: u64,
}

/// One named material phase of TIFF import.
///
/// Stage names are part of the import evidence surface. They describe work
/// actually performed and do not imply a fabricated percentage or ETA.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ImportStage {
    PlanningAndPreflight,
    SourceRevalidation { pass: u8 },
    CheckpointOpenOrResume,
    BaseProduction,
    PyramidProduction { scale: u32 },
    SourceScientificIdentity,
    ShardPublication,
    StagedStructureValidation,
    StagedExactValidation,
    StagedScientificValidation,
    Commit,
}

impl ImportStage {
    pub const fn name(self) -> &'static str {
        match self {
            Self::PlanningAndPreflight => "planning-and-preflight",
            Self::SourceRevalidation { .. } => "source-revalidation",
            Self::CheckpointOpenOrResume => "checkpoint-open-or-resume",
            Self::BaseProduction => "base-production",
            Self::PyramidProduction { .. } => "pyramid-production",
            Self::SourceScientificIdentity => "source-scientific-identity",
            Self::ShardPublication => "shard-publication",
            Self::StagedStructureValidation => "staged-structure-validation",
            Self::StagedExactValidation => "staged-exact-validation",
            Self::StagedScientificValidation => "staged-scientific-validation",
            Self::Commit => "commit",
        }
    }
}

/// A completed stage measured with monotonic wall and process-CPU clocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportStageTiming {
    pub stage: ImportStage,
    pub wall_time_ns: u64,
    pub cpu_time_ns: u64,
}

/// Progress suitable for a background caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportEvent {
    StageStarted {
        stage: ImportStage,
        completed_work_units: u64,
        total_work_units: Option<u64>,
    },
    StageProgress {
        stage: ImportStage,
        completed_work_units: u64,
        total_work_units: u64,
    },
    StageFinished(ImportStageTiming),
    /// Staged validation succeeded and the create-only package was published
    /// with its one-shot verified authority ready for transfer.
    Published,
}

/// Bounded counters reported by a successful import.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportStatistics {
    /// Application-level bytes returned by source reads during the primary
    /// import clock, including integrity traversals and TIFF decoding.
    pub source_bytes_read: u64,
    /// Source bytes read only for strong source-integrity revalidation.
    pub source_revalidation_bytes_read: u64,
    /// Native TIFF payload bytes decoded in all stages.
    pub native_decoded_bytes: u64,
    /// Native TIFF payload bytes decoded while producing the base level.
    pub base_native_decoded_bytes: u64,
    /// Native TIFF payload bytes decoded only to derive scientific identity.
    pub scientific_identity_native_decoded_bytes: u64,
    pub tiff_open_count: u64,
    pub native_chunk_decode_count: u64,
    /// Canonical, unpadded logical output bytes admitted across all scales.
    pub logical_output_bytes: u64,
    pub checkpoint_payload_bytes: u64,
    pub checkpoint_journal_bytes: u64,
    pub checkpoint_watermark_bytes: u64,
    pub checkpoint_durable_work_units: u64,
    pub checkpoint_pending_work_units: u64,
    pub checkpoint_committed_batches: u64,
    pub codec_encode_calls: u64,
    pub codec_encode_time_ns: u64,
    pub codec_decode_calls: u64,
    pub codec_decode_time_ns: u64,
    /// Successful durability calls across checkpoint files/directories and
    /// package objects, staged directories, and the destination parent.
    pub sync_calls: u64,
    /// Monotonic wall time spent in the durability calls above.
    pub sync_time_ns: u64,
    pub scientific_brick_reads: u64,
    /// Package-object open operations during staged structure validation.
    /// Whole reads, ranges, hashes, and snapshot-only revalidations each count
    /// the object access they perform; directory-only inspection does not.
    pub staged_structure_object_reads: u64,
    /// Package-object open operations during staged exact validation, with the
    /// same operation-count semantics as `staged_structure_object_reads`.
    pub staged_exact_object_reads: u64,
    /// Package-object open operations during staged scientific validation,
    /// including its authority and final snapshot revalidations.
    pub scientific_object_reads: u64,
    /// Payload-object opens directly needed by scientific brick reads. This
    /// locality diagnostic is a subset of `scientific_object_reads`.
    pub scientific_payload_object_reads: u64,
    pub scientific_range_requests: u64,
    pub scientific_encoded_bytes_read: u64,
    pub scientific_decoded_bytes: u64,
    /// Checked sum of staged structure, exact, and scientific package-object
    /// open operations. This is not a distinct-object count.
    pub object_reads: u64,
    /// Peak descriptors sampled throughout the primary import whose Linux
    /// `/proc` targets fall in reviewed source, checkpoint, destination, or
    /// private-stage path scopes. Path attribution can conservatively include
    /// an unrelated descriptor opened to the exact same scoped path.
    pub sampled_peak_open_file_descriptors: u64,
    /// Conservative structural maximum derived from all import descriptor
    /// authorities, including resident checkpoint handles and shared worker
    /// readers. Worker tasks open no per-task descriptors.
    pub open_file_descriptor_structural_bound: u64,
    /// Gate value: the greater of the sampled path-attributed peak and the
    /// conservative structural bound. This prevents the sampling interval
    /// from understating the admitted descriptor maximum.
    pub peak_open_file_descriptors: u64,
    /// Preflight upper bound for importer-owned checkpoint plus staged/final
    /// package regular-file bytes during the primary clock.
    pub preflight_temporary_bytes_bound: u64,
    /// Largest observed sum of importer-owned checkpoint and staged/final
    /// package regular-file bytes during the primary clock.
    pub peak_temporary_bytes: u64,
    /// Largest observed count of regular files in the fixed-object checkpoint.
    pub peak_checkpoint_regular_files: u64,
    pub peak_working_bytes: u64,
    pub peak_process_rss_bytes: u64,
    pub resumed_work_units: u64,
    pub produced_work_units: u64,
    pub primary_wall_time_ns: u64,
    pub primary_cpu_time_ns: u64,
    pub stages: Vec<ImportStageTiming>,
}

/// Durable facts returned after atomic publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportReceipt {
    pub package_id: PackageId,
    pub scientific_content_id: ScientificContentId,
    pub statistics: ImportStatistics,
}

/// One successfully published import and its linear verified-package authority.
///
/// The receipt is cloneable evidence. The storage transfer is deliberately
/// mandatory and non-cloneable: a successful import cannot silently discard
/// capability transfer in favor of reopening and validating the package
/// again.
#[must_use = "a published import carries the one-shot verified package transfer"]
pub struct PublishedImport {
    receipt: ImportReceipt,
    transfer: mirante4d_storage::PublishedScientificPackageTransfer,
}

impl PublishedImport {
    pub(crate) const fn new(
        receipt: ImportReceipt,
        transfer: mirante4d_storage::PublishedScientificPackageTransfer,
    ) -> Self {
        Self { receipt, transfer }
    }

    /// Returns cloneable evidence for the completed import.
    pub const fn receipt(&self) -> &ImportReceipt {
        &self.receipt
    }

    /// Returns the create-only destination bound to the verified transfer.
    pub fn destination(&self) -> &Path {
        self.transfer.destination()
    }

    /// Consumes the import result into its evidence and one-shot authority.
    pub fn into_parts(
        self,
    ) -> (
        ImportReceipt,
        mirante4d_storage::PublishedScientificPackageTransfer,
    ) {
        (self.receipt, self.transfer)
    }
}

impl fmt::Debug for PublishedImport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublishedImport")
            .field("receipt", &self.receipt)
            .field("destination", &self.destination())
            .finish_non_exhaustive()
    }
}
