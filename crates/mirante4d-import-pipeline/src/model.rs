use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use mirante4d_dataset::MAX_LAYER_LABEL_BYTES;
use mirante4d_domain::{IntensityDType, Shape4D};
use mirante4d_identity::{
    PackageId, ScientificContentId, Sha256Digest, Sha256Hasher, normalize_nfc,
};
use mirante4d_storage::ProfileKind;

/// Strong digest type for the exact reviewed source generation.
///
/// Product consumers obtain this identity through the import boundary rather
/// than depending on the identity implementation directly.
pub type SourceFingerprint = Sha256Digest;

/// One explicit interpretation for a user-selected channel source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TiffChannelSourceKind {
    /// One regular TIFF file. Its pages form Z and T is one.
    Single3dTiff,
    /// Immediate TIFF files form T in lexicographic filename order; pages form Z.
    FolderOf3dTiffs,
    /// Immediate single-page TIFF files form Z in lexicographic filename order; T is one.
    FolderOf2dTiffs,
}

/// One named logical channel and its explicitly interpreted filesystem source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TiffChannelSource {
    label: String,
    path: PathBuf,
    kind: TiffChannelSourceKind,
}

impl TiffChannelSource {
    pub fn new(
        label: impl AsRef<str>,
        path: impl Into<PathBuf>,
        kind: TiffChannelSourceKind,
    ) -> Result<Self, &'static str> {
        let label = normalize_nfc(label.as_ref().trim());
        if label.is_empty() {
            return Err("channel names must not be empty");
        }
        if label.len() > MAX_LAYER_LABEL_BYTES {
            return Err("channel names must not exceed 256 UTF-8 bytes");
        }
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err("a channel source path must not be empty");
        }
        Ok(Self { label, path, kind })
    }

    pub fn single_3d(
        label: impl AsRef<str>,
        path: impl Into<PathBuf>,
    ) -> Result<Self, &'static str> {
        Self::new(label, path, TiffChannelSourceKind::Single3dTiff)
    }

    pub fn folder_of_3d(
        label: impl AsRef<str>,
        path: impl Into<PathBuf>,
    ) -> Result<Self, &'static str> {
        Self::new(label, path, TiffChannelSourceKind::FolderOf3dTiffs)
    }

    pub fn folder_of_2d(
        label: impl AsRef<str>,
        path: impl Into<PathBuf>,
    ) -> Result<Self, &'static str> {
        Self::new(label, path, TiffChannelSourceKind::FolderOf2dTiffs)
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub const fn kind(&self) -> TiffChannelSourceKind {
        self.kind
    }
}

/// Immutable user-authored source manifest. No filename carries logical meaning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TiffSource {
    channels: Vec<TiffChannelSource>,
}

/// Bounded metadata-inspection progress. Pixel payloads are not read while
/// this counter advances.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TiffInspectionProgress {
    pub inspected_files: u64,
    pub total_files: u64,
}

impl TiffSource {
    pub fn new(channels: Vec<TiffChannelSource>) -> Result<Self, &'static str> {
        if channels.is_empty() {
            return Err("a TIFF source manifest must contain at least one channel");
        }
        if channels.len() > 64 {
            return Err("a TIFF source manifest cannot contain more than 64 channels");
        }
        let mut labels = std::collections::BTreeSet::new();
        if channels
            .iter()
            .any(|channel| !labels.insert(channel.label.as_bytes().to_vec()))
        {
            return Err("channel names must be unique");
        }
        Ok(Self { channels })
    }

    pub fn single_3d(path: impl Into<PathBuf>) -> Self {
        Self::new(vec![
            TiffChannelSource::single_3d("channel 1", path)
                .expect("the default channel label is valid"),
        ])
        .expect("one explicit channel is valid")
    }

    pub fn channels(&self) -> &[TiffChannelSource] {
        &self.channels
    }

    pub fn primary_path(&self) -> &Path {
        self.channels[0].path()
    }

    pub fn channel_labels(&self) -> impl ExactSizeIterator<Item = &str> {
        self.channels.iter().map(TiffChannelSource::label)
    }
}

/// Derives the create-only package destination used by every TIFF import
/// entry point.
pub fn deterministic_tiff_destination(source: &TiffSource, output_parent: &Path) -> PathBuf {
    let name = source
        .primary_path()
        .file_stem()
        .or_else(|| source.primary_path().file_name())
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

/// Reviewed source-value rule used to resolve no-data classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoDataValueRule {
    /// Resolve one exact typed value and its seeded spatial mask from the first
    /// logical source volume.
    Automatic,
    /// Use an explicitly reviewed uint8 source value.
    ManualUint8(u8),
}

/// Optional reviewed source no-data policy.
///
/// Value-derived invalidity and constant-Z-plane invalidity are independent.
/// The importer resolves this request once from channel zero at timepoint zero
/// and applies the resulting dataset-wide policy to every channel/timepoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoDataPolicy {
    value_rule: Option<NoDataValueRule>,
    hide_constant_z_planes: bool,
}

impl NoDataPolicy {
    pub const fn new(value_rule: Option<NoDataValueRule>, hide_constant_z_planes: bool) -> Self {
        Self {
            value_rule,
            hide_constant_z_planes,
        }
    }

    pub const fn automatic() -> Self {
        Self::new(Some(NoDataValueRule::Automatic), false)
    }

    pub const fn manual_uint8(value: u8) -> Self {
        Self::new(Some(NoDataValueRule::ManualUint8(value)), false)
    }

    pub const fn constant_z_planes() -> Self {
        Self::new(None, true)
    }

    pub const fn value_rule(self) -> Option<NoDataValueRule> {
        self.value_rule
    }

    pub const fn hides_constant_z_planes(self) -> bool {
        self.hide_constant_z_planes
    }

    pub const fn may_produce_validity(self) -> bool {
        self.value_rule.is_some() || self.hide_constant_z_planes
    }
}

/// Exact typed value resolved by the import pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResolvedNoDataValue {
    Uint8(u8),
    Uint16(u16),
    Float32Bits(u32),
}

impl ResolvedNoDataValue {
    pub(crate) const fn dtype(self) -> IntensityDType {
        match self {
            Self::Uint8(_) => IntensityDType::Uint8,
            Self::Uint16(_) => IntensityDType::Uint16,
            Self::Float32Bits(_) => IntensityDType::Float32,
        }
    }

    pub(crate) fn canonical_le_bytes(self) -> Vec<u8> {
        match self {
            Self::Uint8(value) => vec![value],
            Self::Uint16(value) => value.to_le_bytes().to_vec(),
            Self::Float32Bits(bits) => bits.to_le_bytes().to_vec(),
        }
    }

    pub(crate) const fn canonical_bits(self) -> u32 {
        match self {
            Self::Uint8(value) => value as u32,
            Self::Uint16(value) => value as u32,
            Self::Float32Bits(bits) => bits,
        }
    }
}

const AUTOMATIC_MASK_DIGEST_DOMAIN: &[u8] =
    b"mirante4d-automatic-no-data-spatial-mask-row-packed-lsb0-v1\0";

/// Immutable first-volume spatial mask resolved by automatic no-data mode.
///
/// Bits are row-packed in `[z,y,ceil(x/8)]` order, least-significant bit
/// first. The mask contains the exact six-connected reconstruction before the
/// separate one-voxel sampling guard is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedAutomaticNoDataMask {
    shape_zyx: [u64; 3],
    row_bytes: u64,
    bits: Arc<[u8]>,
    digest: Sha256Digest,
    masked_voxels: u64,
}

impl ResolvedAutomaticNoDataMask {
    pub(crate) fn new(shape_zyx: [u64; 3], bits: Vec<u8>) -> Result<Self, &'static str> {
        if shape_zyx.contains(&0) {
            return Err("automatic no-data mask shape must be nonzero");
        }
        let row_bytes = shape_zyx[2].div_ceil(8);
        let expected = shape_zyx[0]
            .checked_mul(shape_zyx[1])
            .and_then(|value| value.checked_mul(row_bytes))
            .ok_or("automatic no-data mask byte length overflowed")?;
        if u64::try_from(bits.len()).ok() != Some(expected) {
            return Err("automatic no-data mask byte length does not match its shape");
        }
        let tail_bits = shape_zyx[2] % 8;
        if tail_bits != 0 {
            let allowed = (1_u8 << tail_bits) - 1;
            let row_bytes_usize = usize::try_from(row_bytes)
                .map_err(|_| "automatic no-data row byte count is not addressable")?;
            if bits
                .chunks_exact(row_bytes_usize)
                .any(|row| row[row_bytes_usize - 1] & !allowed != 0)
            {
                return Err("automatic no-data mask has nonzero row-padding bits");
            }
        }
        let masked_voxels = bits.iter().try_fold(0_u64, |total, byte| {
            total.checked_add(u64::from(byte.count_ones()))
        });
        let Some(masked_voxels) = masked_voxels else {
            return Err("automatic no-data mask voxel count overflowed");
        };
        if masked_voxels == 0 {
            return Err("automatic no-data mask must contain a reconstructed component");
        }
        let mut hasher = Sha256Hasher::new();
        hasher.update(AUTOMATIC_MASK_DIGEST_DOMAIN);
        for dimension in shape_zyx {
            hasher.update(dimension.to_le_bytes());
        }
        hasher.update(&bits);
        Ok(Self {
            shape_zyx,
            row_bytes,
            bits: Arc::from(bits),
            digest: hasher.finalize(),
            masked_voxels,
        })
    }

    pub(crate) const fn shape_zyx(&self) -> [u64; 3] {
        self.shape_zyx
    }

    pub(crate) const fn digest(&self) -> Sha256Digest {
        self.digest
    }

    pub(crate) const fn masked_voxels(&self) -> u64 {
        self.masked_voxels
    }

    pub(crate) fn resident_bytes(&self) -> u64 {
        u64::try_from(self.bits.len()).unwrap_or(u64::MAX)
    }

    pub(crate) fn packed_bits(&self) -> &[u8] {
        &self.bits
    }

    pub(crate) fn contains(&self, z: u64, y: u64, x: u64) -> bool {
        if z >= self.shape_zyx[0] || y >= self.shape_zyx[1] || x >= self.shape_zyx[2] {
            return false;
        }
        let Some(byte) = z
            .checked_mul(self.shape_zyx[1])
            .and_then(|value| value.checked_add(y))
            .and_then(|value| value.checked_mul(self.row_bytes))
            .and_then(|value| value.checked_add(x / 8))
            .and_then(|value| usize::try_from(value).ok())
        else {
            return false;
        };
        self.bits
            .get(byte)
            .is_some_and(|value| value & (1 << (x % 8)) != 0)
    }
}

/// Immutable result of resolving the reviewed policy against the first
/// logical volume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedNoDataPolicy {
    request: Option<NoDataPolicy>,
    value: Option<ResolvedNoDataValue>,
    automatic_mask: Option<ResolvedAutomaticNoDataMask>,
    constant_z_planes: Vec<u64>,
    base_depth: u64,
}

impl ResolvedNoDataPolicy {
    pub(crate) fn all_valid(base_depth: u64) -> Self {
        Self {
            request: None,
            value: None,
            automatic_mask: None,
            constant_z_planes: Vec::new(),
            base_depth,
        }
    }

    pub(crate) fn new(
        request: Option<NoDataPolicy>,
        value: Option<ResolvedNoDataValue>,
        automatic_mask: Option<ResolvedAutomaticNoDataMask>,
        constant_z_planes: Vec<u64>,
        base_depth: u64,
    ) -> Result<Self, &'static str> {
        if base_depth == 0
            || constant_z_planes.iter().any(|z| *z >= base_depth)
            || !constant_z_planes.windows(2).all(|pair| pair[0] < pair[1])
        {
            return Err("resolved constant-Z-plane indices are not canonical");
        }
        let value_rule = request.and_then(NoDataPolicy::value_rule);
        match value_rule {
            Some(NoDataValueRule::Automatic) => {
                if value.is_some() != automatic_mask.is_some() {
                    return Err(
                        "automatic no-data value and reconstructed mask must resolve together",
                    );
                }
            }
            Some(NoDataValueRule::ManualUint8(expected)) => {
                if value != Some(ResolvedNoDataValue::Uint8(expected)) || automatic_mask.is_some() {
                    return Err("manual uint8 no-data resolution is inconsistent");
                }
            }
            None => {
                if value.is_some() || automatic_mask.is_some() {
                    return Err("a disabled value rule resolved value-derived invalidity");
                }
            }
        }
        if automatic_mask
            .as_ref()
            .is_some_and(|mask| mask.shape_zyx()[0] != base_depth)
        {
            return Err("automatic no-data mask depth differs from the source");
        }
        Ok(Self {
            request,
            value,
            automatic_mask,
            constant_z_planes,
            base_depth,
        })
    }

    pub(crate) const fn request(&self) -> Option<NoDataPolicy> {
        self.request
    }

    pub(crate) const fn value(&self) -> Option<ResolvedNoDataValue> {
        self.value
    }

    pub(crate) fn manual_value(&self) -> Option<ResolvedNoDataValue> {
        matches!(
            self.request.and_then(NoDataPolicy::value_rule),
            Some(NoDataValueRule::ManualUint8(_))
        )
        .then_some(self.value)
        .flatten()
    }

    pub(crate) const fn automatic_mask(&self) -> Option<&ResolvedAutomaticNoDataMask> {
        self.automatic_mask.as_ref()
    }

    pub(crate) fn automatic_mask_resident_bytes(&self) -> u64 {
        self.automatic_mask
            .as_ref()
            .map_or(0, ResolvedAutomaticNoDataMask::resident_bytes)
    }

    pub(crate) fn constant_z_planes(&self) -> &[u64] {
        &self.constant_z_planes
    }

    pub(crate) const fn base_depth(&self) -> u64 {
        self.base_depth
    }

    pub(crate) fn explicit_validity(&self) -> bool {
        self.manual_value().is_some()
            || self.automatic_mask.is_some()
            || !self.constant_z_planes.is_empty()
    }

    /// Whether the complete base-Z support of one scale sample is hidden.
    pub(crate) fn scale_z_is_hidden(&self, scale: u32, z: u64) -> bool {
        if self.constant_z_planes.is_empty() {
            return false;
        }
        let factor = 1_u64.checked_shl(scale).unwrap_or(u64::MAX);
        let start = z.saturating_mul(factor);
        let end = start.saturating_add(factor).min(self.base_depth);
        if start >= end {
            return false;
        }
        let first = self
            .constant_z_planes
            .partition_point(|candidate| *candidate < start);
        let last = self
            .constant_z_planes
            .partition_point(|candidate| *candidate < end);
        u64::try_from(last - first).is_ok_and(|count| count == end - start)
    }
}

/// Metadata-only source inspection accepted for an import plan.
#[derive(Clone, Debug, PartialEq)]
pub struct TiffInspection {
    pub(crate) source: TiffSource,
    pub(crate) files: Vec<InspectedSourceFile>,
    /// Conservative resident reservation covering the retained reviewed
    /// source index and its largest in-clock inventory/revalidation view.
    pub(crate) source_index_working_bytes: u64,
    pub shape: Shape4D,
    pub channels: u32,
    pub channel_labels: Vec<String>,
    pub dtype: IntensityDType,
    pub ome_spacing_zyx_um: Option<[f64; 3]>,
    pub source_bytes: u64,
    pub source_fingerprint: SourceFingerprint,
    pub maximum_decoded_chunk_bytes: u64,
    pub maximum_encoded_chunk_bytes: u64,
}

impl TiffInspection {
    pub fn source(&self) -> &TiffSource {
        &self.source
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }
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
}

/// Placement-aware preprocessing capacity facts. Aggregate source/output
/// sizes are informational. Hard admission protects only one bounded
/// temporal/channel unit's unfinished growth plus fixed finalization room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportCapacityPlan {
    pub compressed_source_bytes: u64,
    pub decoded_base_bytes: u64,
    pub logical_output_bytes: u64,
    pub final_package_upper_bound: u64,
    /// Scratch bounded by one spatial production unit, independent of T/C.
    pub bounded_unit_scratch_bytes: u64,
    /// Compact stage/index/identity control that grows with completed output.
    pub growing_control_bytes: u64,
    /// Maximum final-layout payload produced by one temporal/channel unit.
    pub maximum_unit_output_upper_bound: u64,
    /// Fixed format-derived room retained for packed-index and metadata
    /// finalization after temporal production completes.
    pub finalization_headroom_bytes: u64,
    /// Additional free space required to begin a new import. This is a hard
    /// immediate headroom check, not a reservation for the whole package.
    pub start_required_headroom_bytes: u64,
}

/// One named material phase of TIFF import.
///
/// Stage names are part of the import evidence surface. They describe work
/// actually performed and do not imply a fabricated percentage or ETA.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ImportStage {
    PlanningAndPreflight,
    SourceRevalidation { pass: u8 },
    SourceIngest,
    NoDataDetection,
    CheckpointOpenOrResume,
    BaseProduction,
    PyramidProduction { scale: u32 },
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
            Self::SourceIngest => "source-ingest",
            Self::NoDataDetection => "no-data-detection",
            Self::CheckpointOpenOrResume => "checkpoint-open-or-resume",
            Self::BaseProduction => "base-production",
            Self::PyramidProduction { .. } => "pyramid-production",
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

/// Coalescible storage facts for the currently active temporal production
/// unit. These counters describe physical progress and conservative remaining
/// capacity; they are deliberately independent of stage work-unit progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportStorageProgress {
    pub completed_temporal_units: u64,
    pub total_temporal_units: u64,
    pub active_timepoint: Option<u64>,
    pub active_channel: Option<u32>,
    /// Optional next unit being decoded without changing the canonical
    /// foreground stage.
    pub preparing_timepoint: Option<u64>,
    pub preparing_channel: Option<u32>,
    pub preparing_completed_planes: u64,
    pub preparing_total_planes: u64,
    /// Complete caches waiting immediately ahead of the canonical owner.
    pub prepared_temporal_units: u32,
    /// Current owner plus live/ready decode-ahead slots. Production is fixed
    /// at no more than two.
    pub temporal_pipeline_width: u32,
    pub stage_payload_bytes: u64,
    /// Non-reserved guidance for output that may still be written before the
    /// package reaches its conservative final-size ceiling.
    pub remaining_package_output_upper_bound: u64,
    pub unit_scratch_bytes: u64,
    pub decode_ahead_scratch_bytes: u64,
    /// Hard additional filesystem headroom needed for the next bounded unit
    /// or, once all units are complete, finalization.
    pub additional_headroom_required_bytes: u64,
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
    /// Latest temporal-unit and storage-capacity facts. Callers should retain
    /// this alongside, rather than instead of, the active stage event.
    StorageProgress(ImportStorageProgress),
    /// Staged self-consistency validation succeeded and the create-only
    /// package was published with its one-shot authority ready for transfer.
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
    /// Native TIFF payload bytes decoded while resolving first-volume no-data rules.
    pub no_data_detection_native_decoded_bytes: u64,
    /// Native TIFF payload bytes decoded only to derive the scientific content address.
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
    /// Successful durability calls across checkpoint files/directories, the
    /// staged-package filesystem barrier, staged directories, and the
    /// destination parent.
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
    /// Hard additional filesystem headroom required when this import began.
    /// This deliberately excludes the non-reserved whole-package estimate.
    pub preflight_required_headroom_bytes: u64,
    /// Largest observed sum of importer-owned checkpoint and staged/final
    /// package regular-file bytes during the primary clock.
    pub peak_temporary_bytes: u64,
    /// Largest observed count of regular files in the private final-layout
    /// stage control area, including the active temporal unit's bounded files.
    pub peak_checkpoint_regular_files: u64,
    pub peak_working_bytes: u64,
    pub peak_process_rss_bytes: u64,
    pub resumed_work_units: u64,
    pub produced_work_units: u64,
    /// Maximum current-plus-decode-ahead width observed by this run.
    pub maximum_temporal_pipeline_width: u64,
    pub prefetch_units_admitted: u64,
    pub prefetch_units_consumed: u64,
    pub prefetch_cache_hits: u64,
    /// Sum of actual native-ingest busy intervals. Concurrent intervals may
    /// overlap and therefore are not a substitute for primary wall time.
    pub temporal_ingest_busy_time_ns: u64,
    pub temporal_canonical_processing_time_ns: u64,
    pub prefetch_ingest_busy_time_ns: u64,
    pub prefetch_overlap_time_ns: u64,
    pub prefetch_cpu_capacity_deferrals: u64,
    pub prefetch_disk_headroom_deferrals: u64,
    pub prefetch_queue_capacity_deferrals: u64,
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

/// One successfully published import and its linear package-publication authority.
///
/// The receipt is cloneable evidence. The storage transfer is deliberately
/// mandatory and non-cloneable: a successful import cannot silently discard
/// capability transfer in favor of reopening and validating the package
/// again.
#[must_use = "a published import carries the one-shot package-publication transfer"]
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

    /// Returns the create-only destination bound to the self-consistent transfer.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_tiff_destinations_preserve_the_frozen_slug_policy() {
        for (source, expected) in [
            ("/source/My Cells.ome.tiff", "/output/my-cells-ome.m4d"),
            ("/source/Cell_Stack-7.TIF", "/output/cell_stack-7.m4d"),
            (
                "/source/Nested Acquisition",
                "/output/nested-acquisition.m4d",
            ),
            ("/source/---.tif", "/output/imported-dataset.m4d"),
        ] {
            assert_eq!(
                deterministic_tiff_destination(
                    &TiffSource::single_3d(source),
                    Path::new("/output"),
                ),
                Path::new(expected),
            );
        }
    }

    #[test]
    fn automatic_mask_is_canonical_row_packed_and_shape_bound() {
        let mask = ResolvedAutomaticNoDataMask::new([1, 2, 9], vec![0b0000_0001, 1, 0, 1]).unwrap();
        assert_eq!(mask.masked_voxels(), 3);
        assert!(mask.contains(0, 0, 0));
        assert!(mask.contains(0, 0, 8));
        assert!(mask.contains(0, 1, 8));
        assert!(!mask.contains(0, 1, 0));
        assert!(!mask.contains(1, 0, 0));

        assert!(
            ResolvedAutomaticNoDataMask::new([1, 1, 9], vec![0, 0b1000_0000]).is_err(),
            "row-padding bits must not create alternate mask encodings"
        );
        let reshaped = ResolvedAutomaticNoDataMask::new([2, 1, 9], vec![1, 0, 0, 1]).unwrap();
        assert_ne!(mask.digest(), reshaped.digest());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_tiff_names_use_the_deterministic_fallback() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let source = Path::new("/source").join(OsString::from_vec(vec![
            b'c', b'e', b'l', b'l', b's', 0xff, b'.', b't', b'i', b'f',
        ]));

        assert_eq!(
            deterministic_tiff_destination(&TiffSource::single_3d(source), Path::new("/output")),
            Path::new("/output/imported-dataset.m4d"),
        );
    }
}
