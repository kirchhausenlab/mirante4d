use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use mirante4d_core::{DisplayError, IntensityDType, Shape3D, SpaceError};
use mirante4d_format::ExistingPackagePolicy;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct TiffDirectoryImportOptions {
    pub input_dir: PathBuf,
    pub output_package: PathBuf,
    pub dataset_id: String,
    pub dataset_name: String,
    pub voxel_spacing_um: [f64; 3],
    pub channel_metadata: BTreeMap<u32, TiffChannelMetadataOverride>,
    pub file_grouping: Option<Vec<TiffFileGrouping>>,
    pub existing_policy: ExistingPackagePolicy,
    pub storage: TiffImportStorageOptions,
    pub reviewed_plan: TiffReviewedImportPlan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TiffImportSource {
    Directory(PathBuf),
    SingleFile(PathBuf),
}

impl TiffImportSource {
    pub fn path(&self) -> &Path {
        match self {
            Self::Directory(path) | Self::SingleFile(path) => path,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TiffSourceProfile {
    StackSeriesMovie,
    PlaneSeriesVolume,
}

impl TiffSourceProfile {
    pub fn id(self) -> &'static str {
        match self {
            Self::StackSeriesMovie => "stack_series_movie",
            Self::PlaneSeriesVolume => "plane_series_volume",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::StackSeriesMovie => "Stack-series movie",
            Self::PlaneSeriesVolume => "Plane-series volume",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TiffSourceImportOptions {
    pub source: TiffImportSource,
    pub output_package: PathBuf,
    pub dataset_id: String,
    pub dataset_name: String,
    pub voxel_spacing_um: [f64; 3],
    pub channel_metadata: BTreeMap<u32, TiffChannelMetadataOverride>,
    pub file_grouping: Option<Vec<TiffFileGrouping>>,
    pub existing_policy: ExistingPackagePolicy,
    pub storage: TiffImportStorageOptions,
    pub reviewed_plan: TiffReviewedImportPlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TiffImportStorageOptions {
    pub brick_shape_zyx: Option<Shape3D>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TiffChannelMetadataOverride {
    pub name: String,
    pub color_rgba: [f32; 4],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TiffDirectoryImportReport {
    pub output_package: PathBuf,
    pub channel_count: usize,
    pub timepoint_count: u64,
    pub scale_count: u32,
    pub z_planes: u64,
    pub width: u64,
    pub height: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TiffDirectoryInspection {
    pub input_dir: PathBuf,
    pub source_profile: TiffSourceProfile,
    pub file_count: usize,
    pub channel_count: usize,
    pub timepoint_count: u64,
    pub shape: TiffStackShape,
    pub source_dtype: IntensityDType,
    pub source_metadata: TiffSourceMetadata,
    pub metadata_confidence: TiffMetadataConfidence,
    pub value_range: TiffValueRangeSummary,
    pub files: Vec<TiffFileGrouping>,
    pub channels: Vec<TiffChannelInspection>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceFormatMatrixEntry {
    pub id: &'static str,
    pub label: &'static str,
    pub status: SourceFormatSupportStatus,
    pub parser_owner: &'static str,
    pub metadata_guarantees: &'static [&'static str],
    pub unsupported_variants: &'static [&'static str],
    pub required_tests: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormatSupportStatus {
    Primary,
    ApprovedWithExplicitReview,
    DeveloperFixtureOnly,
}

pub const SOURCE_FORMAT_OME_TIFF: &str = "ome_tiff";
pub const SOURCE_FORMAT_EXPLICIT_TIFF_STACK: &str = "explicit_grayscale_tiff_stack";
pub const SOURCE_FORMAT_PLANE_SERIES_TIFF_VOLUME: &str = "plane_series_tiff_volume";
pub const SOURCE_FORMAT_SYNTHETIC_FIXTURE: &str = "synthetic_fixture";

pub const SUPPORTED_SOURCE_FORMAT_MATRIX: &[SourceFormatMatrixEntry] = &[
    SourceFormatMatrixEntry {
        id: SOURCE_FORMAT_OME_TIFF,
        label: "OME-TIFF",
        status: SourceFormatSupportStatus::Primary,
        parser_owner: "mirante4d-import TIFF/OME parser",
        metadata_guarantees: &[
            "grayscale uint8, uint16, or float32 pixels",
            "OME-XML physical voxel spacing when complete",
            "explicit review still required before native output",
        ],
        unsupported_variants: &[
            "RGB/multisample pixels",
            "unsupported integer or floating dtypes",
            "missing or conflicting OME spatial calibration without user correction",
        ],
        required_tests: &[
            "valid OME voxel spacing",
            "missing spacing",
            "conflicting spacing",
            "unsupported dtype",
            "reviewed import provenance",
        ],
    },
    SourceFormatMatrixEntry {
        id: SOURCE_FORMAT_EXPLICIT_TIFF_STACK,
        label: "Explicit grayscale TIFF stack",
        status: SourceFormatSupportStatus::ApprovedWithExplicitReview,
        parser_owner: "mirante4d-import TIFF parser",
        metadata_guarantees: &[
            "grayscale uint8, uint16, or float32 pixels",
            "channel/time grouping only after explicit review",
            "voxel spacing only after user correction or documented acceptance",
        ],
        unsupported_variants: &[
            "implicit open-anything image readers",
            "unreviewed filename guesses",
            "silent default calibration",
        ],
        required_tests: &[
            "explicit grouping",
            "review gating",
            "source file fingerprint provenance",
        ],
    },
    SourceFormatMatrixEntry {
        id: SOURCE_FORMAT_PLANE_SERIES_TIFF_VOLUME,
        label: "Plane-series grayscale TIFF volume",
        status: SourceFormatSupportStatus::ApprovedWithExplicitReview,
        parser_owner: "mirante4d-import TIFF parser",
        metadata_guarantees: &[
            "grayscale uint8, uint16, or float32 XY plane files",
            "non-recursive folder-per-channel layout",
            "lexicographic plane order after explicit review",
            "voxel spacing only after user correction or documented acceptance",
        ],
        unsupported_variants: &[
            "recursive plane-series folders",
            "plane-series timepoint layouts",
            "mixed direct TIFF files and channel folders",
            "filename-derived spacing or physical Z placement",
        ],
        required_tests: &[
            "folder-per-channel classification",
            "lexicographic plane order",
            "recursive layout rejection",
            "source file fingerprint provenance",
        ],
    },
    SourceFormatMatrixEntry {
        id: SOURCE_FORMAT_SYNTHETIC_FIXTURE,
        label: "Synthetic fixture",
        status: SourceFormatSupportStatus::DeveloperFixtureOnly,
        parser_owner: "mirante4d-format fixture writer",
        metadata_guarantees: &["deterministic developer/test data only"],
        unsupported_variants: &["user-facing import source"],
        required_tests: &["fixture generation and strict native validation"],
    },
];

pub fn supported_source_format_matrix() -> &'static [SourceFormatMatrixEntry] {
    SUPPORTED_SOURCE_FORMAT_MATRIX
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TiffImportStorageEstimate {
    pub source_payload_bytes: u64,
    pub derived_multiscale_payload_bytes: u64,
    pub estimated_metadata_bytes: u64,
    pub estimated_total_bytes: u64,
    pub peak_working_stack_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TiffValueRangeSummary {
    pub min: f64,
    pub max: f64,
}

impl TiffValueRangeSummary {
    pub(crate) fn merge(self, other: Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TiffSourceMetadata {
    pub voxel_spacing_um: Option<[f64; 3]>,
    pub voxel_spacing_status: TiffVoxelSpacingMetadataStatus,
    pub voxel_spacing_source: Option<TiffVoxelSpacingMetadataSource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TiffMetadataConfidence {
    CompleteOmeXml,
    MissingSpatialCalibration,
    IncompleteSpatialCalibration,
    ConflictingSpatialCalibration,
    ExplicitReviewRequired,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TiffReviewedImportPlan {
    pub review_status: TiffImportReviewStatus,
    pub source_profile: TiffSourceProfile,
    pub source_format: String,
    pub metadata_confidence: TiffMetadataConfidence,
    pub source_axes: Vec<String>,
    pub native_axes: Vec<String>,
    pub channels_as_layers: bool,
    pub value_range: Option<TiffValueRangeSummary>,
    pub no_data_policy: Option<TiffNoDataPolicyReview>,
    pub user_corrections: Vec<TiffUserCorrection>,
}

impl TiffReviewedImportPlan {
    pub fn pending() -> Self {
        Self {
            review_status: TiffImportReviewStatus::Pending,
            source_profile: TiffSourceProfile::StackSeriesMovie,
            source_format: SOURCE_FORMAT_EXPLICIT_TIFF_STACK.to_owned(),
            metadata_confidence: TiffMetadataConfidence::ExplicitReviewRequired,
            source_axes: Vec::new(),
            native_axes: ["t", "z", "y", "x"].map(str::to_owned).to_vec(),
            channels_as_layers: true,
            value_range: None,
            no_data_policy: None,
            user_corrections: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TiffNoDataPolicyReview {
    pub source_dtype: IntensityDType,
    pub source_value_uint8: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TiffImportReviewStatus {
    Pending,
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TiffUserCorrection {
    pub field: String,
    pub source_value: Option<String>,
    pub reviewed_value: String,
    pub reason: String,
}

impl TiffSourceMetadata {
    pub(crate) fn missing() -> Self {
        Self {
            voxel_spacing_um: None,
            voxel_spacing_status: TiffVoxelSpacingMetadataStatus::Missing,
            voxel_spacing_source: None,
        }
    }

    pub(crate) fn complete(spacing_um: [f64; 3], source: TiffVoxelSpacingMetadataSource) -> Self {
        Self {
            voxel_spacing_um: Some(spacing_um),
            voxel_spacing_status: TiffVoxelSpacingMetadataStatus::Complete,
            voxel_spacing_source: Some(source),
        }
    }

    pub(crate) fn incomplete() -> Self {
        Self {
            voxel_spacing_um: None,
            voxel_spacing_status: TiffVoxelSpacingMetadataStatus::Incomplete,
            voxel_spacing_source: None,
        }
    }
}

impl Default for TiffSourceMetadata {
    fn default() -> Self {
        Self::missing()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TiffVoxelSpacingMetadataStatus {
    Missing,
    Complete,
    Incomplete,
    Conflicting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TiffVoxelSpacingMetadataSource {
    OmeXml,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TiffFileGrouping {
    pub path: PathBuf,
    pub channel: u32,
    pub stack_index: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TiffChannelInspection {
    pub channel: u32,
    pub timepoint_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportProgressEvent {
    DiscoveredInput {
        file_count: usize,
    },
    EstimatedStorage {
        estimate: TiffImportStorageEstimate,
    },
    ReadStack {
        completed: usize,
        total: usize,
        path: PathBuf,
    },
    BuiltScale {
        channel: u32,
        level: u32,
    },
    WritingPackage {
        output_package: PathBuf,
    },
    Finished {
        output_package: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub struct ImportCancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl ImportCancellationToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl Default for ImportCancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("import was cancelled")]
    Cancelled,
    #[error("input directory does not exist: {0}")]
    MissingInputDirectory(PathBuf),
    #[error("input TIFF file does not exist: {0}")]
    MissingInputFile(PathBuf),
    #[error("input directory contains no .tif/.tiff files: {0}")]
    EmptyInputDirectory(PathBuf),
    #[error("input path is not a .tif/.tiff file: {0}")]
    UnsupportedTiffPath(PathBuf),
    #[error(
        "ambiguous TIFF source layout in {path}: {message}; choose a single stack-series folder or a non-recursive folder-per-channel plane-series layout"
    )]
    AmbiguousTiffSourceLayout { path: PathBuf, message: String },
    #[error("invalid plane-series TIFF layout at {path}: {message}")]
    InvalidPlaneSeriesLayout { path: PathBuf, message: String },
    #[error("plane-series TIFF file {path} has {z} image(s); expected exactly one XY plane")]
    PlaneSeriesFileHasMultipleImages { path: PathBuf, z: u64 },
    #[error("explicit TIFF file grouping must contain at least one file")]
    EmptyFileGrouping,
    #[error("explicit TIFF file grouping contains duplicate path: {0}")]
    DuplicateFileGroupingPath(PathBuf),
    #[error("explicit TIFF file grouping path {path} is outside input directory {input_dir}")]
    GroupedFileOutsideInputDirectory { path: PathBuf, input_dir: PathBuf },
    #[error("failed to list input directory {path}: {source}")]
    ListInput {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to open TIFF {path}: {source}")]
    OpenTiff {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to decode TIFF {path}: {message}")]
    DecodeTiff { path: PathBuf, message: String },
    #[error("TIFF import storage estimate overflowed 64-bit byte counters")]
    StorageEstimateOverflow,
    #[error("TIFF {path} has unsupported pixel type; expected grayscale uint8, uint16, or float32")]
    UnsupportedPixelType { path: PathBuf },
    #[error("TIFF stack {path} has source dtype {actual:?}, expected {expected:?}")]
    SourceDTypeMismatch {
        path: PathBuf,
        actual: IntensityDType,
        expected: IntensityDType,
    },
    #[error("TIFF import metadata review must be accepted before writing native output")]
    UnreviewedImportPlan,
    #[error("TIFF import reviewed source format {0:?} is not approved")]
    UnsupportedReviewedSourceFormat(String),
    #[error(
        "TIFF import reviewed source profile {reviewed:?} does not match inspected source profile {inspected:?}"
    )]
    ReviewedSourceProfileMismatch {
        reviewed: TiffSourceProfile,
        inspected: TiffSourceProfile,
    },
    #[error("TIFF import reviewed native axes must be exactly [\"t\", \"z\", \"y\", \"x\"]")]
    InvalidReviewedNativeAxes,
    #[error("TIFF import reviewed plan must record channels as separate native layers")]
    InvalidReviewedChannelPolicy,
    #[error(
        "TIFF import reviewed value range {reviewed:?} does not match inspected value range {inspected:?}"
    )]
    ReviewedValueRangeMismatch {
        reviewed: TiffValueRangeSummary,
        inspected: TiffValueRangeSummary,
    },
    #[error("could not parse channel from TIFF filename: {0}")]
    MissingChannel(PathBuf),
    #[error("could not parse stack index from TIFF filename: {0}")]
    MissingStackIndex(PathBuf),
    #[error("channel {channel} has {actual} timepoints, expected {expected}")]
    TimepointCountMismatch {
        channel: u32,
        actual: usize,
        expected: usize,
    },
    #[error("TIFF stack {path} dimensions are {actual:?}, expected {expected:?}")]
    StackShapeMismatch {
        path: PathBuf,
        actual: TiffStackShape,
        expected: TiffStackShape,
    },
    #[error("failed to remove temporary import package {path}: {source}")]
    RemoveTemporaryPackage {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to remove existing output package {path}: {source}")]
    RemoveOutputPackage {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "failed to move existing output package {output_path} to replacement backup {backup_path}: {source}"
    )]
    BackupOutputPackage {
        output_path: PathBuf,
        backup_path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "failed to commit temporary import package {temporary_path} to {output_path}: {source}"
    )]
    CommitTemporaryPackage {
        temporary_path: PathBuf,
        output_path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Format(#[from] mirante4d_format::FormatError),
    #[error(transparent)]
    Shape(#[from] mirante4d_core::ShapeError),
    #[error(transparent)]
    Space(#[from] SpaceError),
    #[error(transparent)]
    Display(#[from] DisplayError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TiffStackShape {
    pub z: u64,
    pub y: u64,
    pub x: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct TiffInput {
    pub(crate) path: PathBuf,
    pub(crate) channel: u32,
    pub(crate) stack_index: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct TiffInputSet {
    pub(crate) source_profile: TiffSourceProfile,
    pub(crate) inputs: Vec<TiffInput>,
}

#[derive(Debug, Clone)]
pub(crate) struct TiffStack {
    pub(crate) shape: TiffStackShape,
    pub(crate) source_dtype: IntensityDType,
    pub(crate) source_metadata: TiffSourceMetadata,
    pub(crate) values_zyx: TiffStackValues,
}

#[derive(Debug, Clone)]
pub(crate) enum TiffStackValues {
    U8(Vec<u8>),
    U16(Vec<u16>),
    F32(Vec<f32>),
}

impl TiffStackValues {
    pub(crate) fn dtype(&self) -> IntensityDType {
        match self {
            Self::U8(_) => IntensityDType::Uint8,
            Self::U16(_) => IntensityDType::Uint16,
            Self::F32(_) => IntensityDType::Float32,
        }
    }

    pub(crate) fn value_range(&self) -> TiffValueRangeSummary {
        match self {
            Self::U8(values) => values.iter().fold(
                TiffValueRangeSummary {
                    min: f64::INFINITY,
                    max: f64::NEG_INFINITY,
                },
                |range, value| TiffValueRangeSummary {
                    min: range.min.min(f64::from(*value)),
                    max: range.max.max(f64::from(*value)),
                },
            ),
            Self::U16(values) => values.iter().fold(
                TiffValueRangeSummary {
                    min: f64::INFINITY,
                    max: f64::NEG_INFINITY,
                },
                |range, value| TiffValueRangeSummary {
                    min: range.min.min(f64::from(*value)),
                    max: range.max.max(f64::from(*value)),
                },
            ),
            Self::F32(values) => values.iter().fold(
                TiffValueRangeSummary {
                    min: f64::INFINITY,
                    max: f64::NEG_INFINITY,
                },
                |range, value| TiffValueRangeSummary {
                    min: range.min.min(f64::from(*value)),
                    max: range.max.max(f64::from(*value)),
                },
            ),
        }
    }
}
