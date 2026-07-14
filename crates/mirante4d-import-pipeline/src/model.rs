use std::path::PathBuf;

use mirante4d_domain::{IntensityDType, Shape4D};
use mirante4d_identity::{PackageId, ScientificContentId, Sha256Digest};
use mirante4d_storage::ProfileKind;

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
    pub layout: SourceLayout,
    pub shape: Shape4D,
    pub channels: u32,
    pub dtype: IntensityDType,
    pub ome_spacing_zyx_um: Option<[f64; 3]>,
    pub source_bytes: u64,
    pub source_fingerprint: Sha256Digest,
    pub maximum_decoded_chunk_bytes: u64,
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
}

/// One complete off-product import request.
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

/// Coarse progress suitable for a background caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportEvent {
    Producing {
        completed_work_units: u64,
        total_work_units: u64,
    },
    HashingScience,
    Publishing,
    Finished,
}

/// Bounded counters reported by a successful import.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportStatistics {
    pub source_bytes_read: u64,
    pub peak_working_bytes: u64,
    pub resumed_work_units: u64,
    pub produced_work_units: u64,
}

/// Durable facts returned after atomic publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportReceipt {
    pub package_id: PackageId,
    pub scientific_content_id: ScientificContentId,
    pub statistics: ImportStatistics,
}
