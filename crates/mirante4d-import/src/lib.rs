use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    fs::File,
    io::{BufReader, Read},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use mirante4d_domain::{DisplayWindow, GridToWorld, IntensityDType, Shape4D};
use mirante4d_format::{
    ChannelMetadata, CurrentGridToWorldExt, ExistingPackagePolicy, LayerDisplay,
    NativeDatasetProvenance, NativeDatasetProvenanceKind, NativeMultiscaleDatasetWriter,
    NoDataPolicy, NoDataPolicyKind, NoDataVisibilityPolicy, ScaleReduction, SourceFileProvenance,
    SourceMetadataProvenance, Statistics, StoragePolicyProvenance, StreamingF32LayerSpec,
    StreamingF32LayerWriter, StreamingF32ScaleSpec, StreamingU8LayerSpec, StreamingU8LayerWriter,
    StreamingU8ScaleSpec, StreamingU16LayerSpec, StreamingU16LayerWriter, StreamingU16ScaleSpec,
    UserCorrectionProvenance, ValueRangeProvenance, WorldSpace, WorldUnit, default_f32_display,
    default_u16_display, load_and_validate_dataset,
};
use quick_xml::Reader as XmlReader;
use quick_xml::events::{BytesStart, Event as XmlEvent};
use tiff::ColorType;
use tiff::TiffFormatError;
use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::{SampleFormat, Tag};

const IMPORT_3D_CHUNK_Z: u64 = 64;
const IMPORT_3D_CHUNK_Y: u64 = 64;
const IMPORT_3D_CHUNK_X: u64 = 64;
const IMPORT_2D_CHUNK_Y: u64 = 256;
const IMPORT_2D_CHUNK_X: u64 = 256;
const MULTISCALE_GENERATE_THRESHOLD: u64 = 256;
const MULTISCALE_STOP_MAX_DIMENSION: u64 = 64;
const MULTISCALE_STOP_VOXELS_PER_TIMEPOINT: u64 = 262_144;
const IMPORT_ESTIMATED_FIXED_METADATA_BYTES: u64 = 1_048_576;
const IMPORT_ESTIMATED_SCALE_METADATA_BYTES: u64 = 65_536;

mod models;

pub use models::*;
use models::{TiffInput, TiffInputSet, TiffStack, TiffStackValues};
pub fn inspect_tiff_directory(
    input_dir: impl AsRef<Path>,
) -> Result<TiffDirectoryInspection, ImportError> {
    let input_dir = input_dir.as_ref();
    let inputs = discover_tiffs(input_dir)?;
    inspect_tiff_inputs(input_dir, TiffSourceProfile::StackSeriesMovie, inputs)
}

pub fn inspect_tiff_source(
    source: &TiffImportSource,
) -> Result<TiffDirectoryInspection, ImportError> {
    let inputs = discover_tiff_source(source)?;
    inspect_tiff_inputs(source.path(), TiffSourceProfile::StackSeriesMovie, inputs)
}

pub fn inspect_tiff_source_for_review(
    source: &TiffImportSource,
) -> Result<TiffDirectoryInspection, ImportError> {
    let inputs = discover_tiff_source_for_review(source)?;
    inspect_tiff_inputs(source.path(), inputs.source_profile, inputs.inputs)
}

pub fn inspect_tiff_source_with_grouping(
    source: &TiffImportSource,
    file_grouping: &[TiffFileGrouping],
) -> Result<TiffDirectoryInspection, ImportError> {
    let inputs = tiff_inputs_from_explicit_grouping(source, file_grouping)?;
    inspect_tiff_inputs(source.path(), TiffSourceProfile::StackSeriesMovie, inputs)
}

mod execute;
pub use execute::{
    estimate_tiff_import_storage, import_tiff_directory, import_tiff_directory_with_progress,
    import_tiff_source, import_tiff_source_with_progress,
};

mod commit;
use commit::*;

mod multiscale;
use multiscale::*;

mod discover;
use discover::*;

mod review;
pub use review::accepted_tiff_reviewed_import_plan;
use review::*;

mod decode;
use decode::*;

pub fn default_tiff_channel_metadata_override(channel: u32) -> TiffChannelMetadataOverride {
    TiffChannelMetadataOverride {
        name: format!("Channel {channel}"),
        color_rgba: channel_color(channel),
    }
}

#[cfg(test)]
mod tests;
