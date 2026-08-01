use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, Read, Seek},
    mem::size_of,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

use mirante4d_dataset::{CpuByteLease, CpuByteLedger, CpuLedgerCategory};
use mirante4d_domain::{IntensityDType, Shape4D};
use mirante4d_identity::{Sha256Digest, Sha256Hasher};
use quick_xml::{
    Reader as XmlReader, XmlVersion,
    events::{BytesStart, Event as XmlEvent},
};
use tiff::{
    ColorType, TiffError, TiffFormatError,
    decoder::{Decoder, DecodingResult, Limits},
    tags::{SampleFormat, Tag},
};

use crate::{
    ImportCancellation, ImportError, NoDataPolicy, NoDataValueRule, TiffChannelSource,
    TiffChannelSourceKind, TiffInspection, TiffInspectionProgress, TiffSource,
    canonical_cache::CanonicalBaseCache,
    model::{
        InspectedSourceFile, ResolvedAutomaticNoDataMask, ResolvedNoDataPolicy,
        ResolvedNoDataValue, SourceFileGeneration,
    },
    ordered_workers::{OrderedWorkerDiagnostics, OrderedWorkerPolicy, run_ordered},
};

const HASH_READ_BYTES: usize = 64 * 1024;
const MAX_ENCODED_CHUNK_OVERHEAD_BYTES: usize = 64 * 1024;
// One explicit channel may contain a substantial plane/time series. This is
// an inventory-memory safety bound, not a filename/provenance convention.
// The aggregate retained path-byte authority is checked independently.
const MAX_SOURCE_FILES: usize = 65_536;
// Retained source paths are exact-fit before this is calculated. Eight text
// copies cover the reviewed index plus the largest simultaneous read_dir,
// discovered-layout, accepted-inventory, and error-path views. The per-file
// allowance covers Vec slots, DirEntry implementation state, sort/index
// pointers, and the aligned generation vector; the fixed allowance covers
// the final SHA buffer and collection/global allocator overhead. This
// reservation is held for the complete in-clock import lifetime.
const MAX_SOURCE_INDEX_TEXT_BYTES: u64 = 16 * 1024 * 1024;
const SOURCE_INDEX_TEXT_COPIES_MAX: u64 = 8;
const SOURCE_INDEX_BYTES_PER_FILE_MAX: u64 = 2 * 1024;
const SOURCE_INDEX_FIXED_BYTES_MAX: u64 = 256 * 1024;
const MAX_PAGES_PER_FILE: u64 = 65_536;
const MAX_NATIVE_CHUNKS_PER_PAGE: usize = 262_144;
const MAX_TIFF_IFD_ENTRIES: u64 = 4_096;
const MAX_OME_XML_BYTES: usize = 8 * 1024 * 1024;
const MAX_OME_XML_EVENTS: usize = 65_536;
const MAX_OME_XML_DEPTH: usize = 64;
const MAX_OME_XML_ATTRIBUTES: usize = 256;
const MAX_OME_XML_ATTRIBUTE_BYTES: usize = 1024 * 1024;
const MAX_OME_XML_VALUE_BYTES: usize = 256;

// Only streaming codecs whose library state fits this authority are admitted
// by `inspect_supported_compression`. JPEG, WebP, Zstd-in-TIFF, and fax paths
// are rejected because their additional allocations are not audited here.
const TIFF_STREAM_CODEC_WORKSPACE_BYTES_MAX: u64 = 1024 * 1024;
const TIFF_CHUNK_TABLE_BYTES_MAX: u64 = (MAX_NATIVE_CHUNKS_PER_PAGE as u64) * 16;
// The decoder retains its visited-IFD chain. This is deliberately generous
// for the current implementation's offsets and cycle-detection hash maps.
const TIFF_RETAINED_BYTES_PER_PAGE_MAX: u64 = 512;
// One live IFD is represented as a BTreeMap by the pinned decoder.
const TIFF_IFD_MAP_BYTES_MAX: u64 = MAX_TIFF_IFD_ENTRIES * 128;

/// Conservative non-pixel allocation ceiling while one TIFF chunk is read.
pub(crate) const SOURCE_DECODE_FIXED_OVERHEAD_BYTES_MAX: u64 =
    (MAX_OME_XML_BYTES + HASH_READ_BYTES + MAX_ENCODED_CHUNK_OVERHEAD_BYTES) as u64
        + TIFF_STREAM_CODEC_WORKSPACE_BYTES_MAX
        + TIFF_CHUNK_TABLE_BYTES_MAX
        + TIFF_IFD_MAP_BYTES_MAX;

pub(crate) const SOURCE_TASK_METADATA_BYTES_MAX: u64 = 1024 * 1024;
// `Decoder::next_image` constructs the next image before replacing the
// previous one, so one prior IFD and its two chunk vectors coexist briefly.
pub(crate) const SOURCE_SERIAL_TRANSITION_BYTES_MAX: u64 =
    TIFF_CHUNK_TABLE_BYTES_MAX + TIFF_IFD_MAP_BYTES_MAX;

pub(crate) const fn retained_decoder_bytes(pages: u64) -> Option<u64> {
    pages.checked_mul(TIFF_RETAINED_BYTES_PER_PAGE_MAX)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SourceReadCounters {
    pub(crate) source_bytes_read: u64,
    pub(crate) decoded_bytes: u64,
    pub(crate) tiff_open_count: u64,
    pub(crate) native_chunk_decode_count: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SourceDecodeReport {
    pub(crate) counters: SourceReadCounters,
    pub(crate) peak_transient_bytes: u64,
    pub(crate) parallel_files: u64,
    pub(crate) serialized_files: u64,
    pub(crate) peak_reorder_results: usize,
}

pub(crate) struct NoDataDetectionReport {
    pub(crate) policy: ResolvedNoDataPolicy,
    pub(crate) counters: SourceReadCounters,
    pub(crate) peak_transient_bytes: u64,
    pub(crate) completed_planes: u64,
    pub(crate) resident_mask_lease: Option<Box<dyn CpuByteLease>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SourceRevalidationCounters {
    pub(crate) source_bytes_read: u64,
    pub(crate) tiff_open_count: u64,
    pub(crate) generation: SourceGeneration,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SourceGeneration {
    files: Vec<SourceFileGeneration>,
}

#[derive(Debug)]
struct TiffFileFacts {
    width: u64,
    height: u64,
    pages: u64,
    dtype: IntensityDType,
    ome_spacing_zyx_um: Option<[f64; 3]>,
    maximum_decoded_chunk_bytes: u64,
    maximum_encoded_chunk_bytes: u64,
    bytes: u64,
    generation: SourceFileGeneration,
}

#[derive(Clone, Debug, PartialEq)]
struct OmePixelsFacts {
    size_x: u64,
    size_y: u64,
    size_z: u64,
    size_c: u64,
    size_t: u64,
    dtype: IntensityDType,
    spacing_zyx_um: Option<[f64; 3]>,
    tiff_data: Option<OmeTiffDataFacts>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OmeTiffDataFacts {
    ifd: Option<u64>,
    first_z: Option<u64>,
    first_c: Option<u64>,
    first_t: Option<u64>,
    plane_count: Option<u64>,
}

pub(crate) fn inspect(source: TiffSource) -> Result<TiffInspection, ImportError> {
    inspect_cancellable(source, &ImportCancellation::new())
}

pub(crate) fn inspect_cancellable(
    source: TiffSource,
    cancellation: &ImportCancellation,
) -> Result<TiffInspection, ImportError> {
    inspect_cancellable_with_progress(source, cancellation, |_| {})
}

pub(crate) fn inspect_cancellable_with_progress(
    source: TiffSource,
    cancellation: &ImportCancellation,
    mut progress: impl FnMut(TiffInspectionProgress),
) -> Result<TiffInspection, ImportError> {
    check_cancelled(cancellation)?;
    let mut dataset_shape: Option<Shape4D> = None;
    let mut dataset_dtype: Option<IntensityDType> = None;
    let mut spacing = SpacingAccumulator::default();
    let mut maximum_decoded_chunk_bytes = 0;
    let mut maximum_encoded_chunk_bytes = 0;
    let mut files = Vec::new();
    let inventories = source
        .channels()
        .iter()
        .map(|channel| explicit_channel_inventory(channel, cancellation))
        .collect::<Result<Vec<_>, _>>()?;
    let total_files = inventories.iter().try_fold(0_u64, |total, inventory| {
        total
            .checked_add(u64::try_from(inventory.len()).map_err(|_| ImportError::Overflow)?)
            .ok_or(ImportError::Overflow)
    })?;
    progress(TiffInspectionProgress {
        inspected_files: 0,
        total_files,
    });

    for (channel_index, (channel, paths)) in source.channels().iter().zip(inventories).enumerate() {
        check_cancelled(cancellation)?;
        let channel_ordinal = u32::try_from(channel_index).map_err(|_| ImportError::Overflow)?;
        let mut channel_common: Option<(u64, u64, u64, IntensityDType)> = None;
        for (ordinal, path) in paths.into_iter().enumerate() {
            check_cancelled(cancellation)?;
            let facts = inspect_file(&path, cancellation)?;
            let logical_pages = match channel.kind() {
                TiffChannelSourceKind::FolderOf2dTiffs if facts.pages != 1 => {
                    return Err(ImportError::UnsupportedSource(format!(
                        "TIFF {path:?} has {} pages, but a folder of 2D TIFFs requires exactly one page per file",
                        facts.pages
                    )));
                }
                TiffChannelSourceKind::FolderOf2dTiffs => 1,
                TiffChannelSourceKind::Single3dTiff | TiffChannelSourceKind::FolderOf3dTiffs => {
                    facts.pages
                }
            };
            let expected = channel_common.get_or_insert((
                facts.width,
                facts.height,
                logical_pages,
                facts.dtype,
            ));
            if *expected != (facts.width, facts.height, logical_pages, facts.dtype) {
                return Err(ImportError::UnsupportedSource(format!(
                    "TIFF {path:?} has shape/dtype x{} y{} z{} {:?}, expected x{} y{} z{} {:?}",
                    facts.width,
                    facts.height,
                    logical_pages,
                    facts.dtype,
                    expected.0,
                    expected.1,
                    expected.2,
                    expected.3,
                )));
            }
            let ordinal = u64::try_from(ordinal).map_err(|_| ImportError::Overflow)?;
            let (timepoint, first_z) = match channel.kind() {
                TiffChannelSourceKind::Single3dTiff => (0, 0),
                TiffChannelSourceKind::FolderOf3dTiffs => (ordinal, 0),
                TiffChannelSourceKind::FolderOf2dTiffs => (0, ordinal),
            };
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    ImportError::UnsupportedSource(format!(
                        "TIFF filename {path:?} must be valid UTF-8"
                    ))
                })?;
            let relative_name = format!("channel-{channel_ordinal:08}/{file_name}");
            spacing.push(&relative_name, facts.ome_spacing_zyx_um)?;
            maximum_decoded_chunk_bytes =
                maximum_decoded_chunk_bytes.max(facts.maximum_decoded_chunk_bytes);
            maximum_encoded_chunk_bytes =
                maximum_encoded_chunk_bytes.max(facts.maximum_encoded_chunk_bytes);
            files.push(InspectedSourceFile {
                path,
                relative_name,
                channel: channel_ordinal,
                timepoint,
                first_z,
                planes: facts.pages,
                bytes: facts.bytes,
                generation: facts.generation,
            });
            progress(TiffInspectionProgress {
                inspected_files: u64::try_from(files.len()).map_err(|_| ImportError::Overflow)?,
                total_files,
            });
        }
        let (width, height, pages, dtype) =
            channel_common.expect("explicit source inventory rejects an empty channel");
        let channel_shape = match channel.kind() {
            TiffChannelSourceKind::Single3dTiff => shape4d(1, pages, height, width)?,
            TiffChannelSourceKind::FolderOf3dTiffs => {
                let timepoints = files
                    .iter()
                    .filter(|file| file.channel == channel_ordinal)
                    .count();
                shape4d(
                    u64::try_from(timepoints).map_err(|_| ImportError::Overflow)?,
                    pages,
                    height,
                    width,
                )?
            }
            TiffChannelSourceKind::FolderOf2dTiffs => {
                let depth = files
                    .iter()
                    .filter(|file| file.channel == channel_ordinal)
                    .count();
                shape4d(
                    1,
                    u64::try_from(depth).map_err(|_| ImportError::Overflow)?,
                    height,
                    width,
                )?
            }
        };
        if let Some(expected) = dataset_shape {
            if expected != channel_shape {
                return Err(ImportError::UnsupportedSource(format!(
                    "channel {:?} has logical shape {:?}, expected {:?}",
                    channel.label(),
                    channel_shape.dimensions(),
                    expected.dimensions()
                )));
            }
        } else {
            dataset_shape = Some(channel_shape);
        }
        if let Some(expected) = dataset_dtype {
            if expected != dtype {
                return Err(ImportError::UnsupportedSource(format!(
                    "channel {:?} has dtype {dtype:?}, expected {expected:?}; mixed channel dtypes are not supported by the current package writer",
                    channel.label()
                )));
            }
        } else {
            dataset_dtype = Some(dtype);
        }
    }

    finish_inspection(
        source,
        dataset_shape.expect("a source manifest always has a channel"),
        dataset_dtype.expect("a source manifest always has a channel"),
        spacing.finish()?,
        maximum_decoded_chunk_bytes,
        maximum_encoded_chunk_bytes,
        files,
    )
}

pub(crate) fn combine_channel_inspections(
    inspections: Vec<TiffInspection>,
) -> Result<TiffInspection, ImportError> {
    if inspections.is_empty() || inspections.len() > 64 {
        return Err(ImportError::InvalidRequest(
            "channel validation requires one through 64 inspected rows",
        ));
    }
    let expected_shape = inspections[0].shape;
    let expected_dtype = inspections[0].dtype;
    let expected_spacing = inspections[0].ome_spacing_zyx_um;
    let mut channels = Vec::with_capacity(inspections.len());
    let mut files = Vec::new();
    let mut maximum_decoded_chunk_bytes = 0;
    let mut maximum_encoded_chunk_bytes = 0;
    for (channel_index, inspection) in inspections.into_iter().enumerate() {
        if inspection.channels != 1 || inspection.source.channels().len() != 1 {
            return Err(ImportError::InvalidRequest(
                "only independently inspected one-channel rows may be combined",
            ));
        }
        if inspection.shape != expected_shape {
            return Err(ImportError::UnsupportedSource(format!(
                "channel {:?} has logical shape {:?}, expected {:?}",
                inspection.channel_labels[0],
                inspection.shape.dimensions(),
                expected_shape.dimensions(),
            )));
        }
        if inspection.dtype != expected_dtype {
            return Err(ImportError::UnsupportedSource(format!(
                "channel {:?} has dtype {:?}, expected {:?}; mixed channel dtypes are not supported by the current package writer",
                inspection.channel_labels[0], inspection.dtype, expected_dtype,
            )));
        }
        if inspection.ome_spacing_zyx_um != expected_spacing {
            return Err(ImportError::UnsupportedSource(format!(
                "OME physical spacing in channel {:?} conflicts with the other channels",
                inspection.channel_labels[0],
            )));
        }
        let channel = u32::try_from(channel_index).map_err(|_| ImportError::Overflow)?;
        channels.push(inspection.source.channels()[0].clone());
        maximum_decoded_chunk_bytes =
            maximum_decoded_chunk_bytes.max(inspection.maximum_decoded_chunk_bytes);
        maximum_encoded_chunk_bytes =
            maximum_encoded_chunk_bytes.max(inspection.maximum_encoded_chunk_bytes);
        for mut file in inspection.files {
            file.channel = channel;
            let file_name = file
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    ImportError::UnsupportedSource(format!(
                        "TIFF filename {:?} must be valid UTF-8",
                        file.path
                    ))
                })?;
            file.relative_name = format!("channel-{channel:08}/{file_name}");
            files.push(file);
        }
    }
    finish_inspection(
        TiffSource::new(channels).map_err(ImportError::InvalidRequest)?,
        expected_shape,
        expected_dtype,
        expected_spacing,
        maximum_decoded_chunk_bytes,
        maximum_encoded_chunk_bytes,
        files,
    )
}

pub(crate) fn relabel_channel_inspection(
    inspection: TiffInspection,
    label: &str,
) -> Result<TiffInspection, ImportError> {
    if inspection.channels != 1 || inspection.source.channels().len() != 1 {
        return Err(ImportError::InvalidRequest(
            "only a one-channel inspection can be relabelled",
        ));
    }
    let previous = &inspection.source.channels()[0];
    let channel = TiffChannelSource::new(label, previous.path(), previous.kind())
        .map_err(ImportError::InvalidRequest)?;
    finish_inspection(
        TiffSource::new(vec![channel]).map_err(ImportError::InvalidRequest)?,
        inspection.shape,
        inspection.dtype,
        inspection.ome_spacing_zyx_um,
        inspection.maximum_decoded_chunk_bytes,
        inspection.maximum_encoded_chunk_bytes,
        inspection.files,
    )
}

fn explicit_channel_inventory(
    channel: &TiffChannelSource,
    cancellation: &ImportCancellation,
) -> Result<Vec<PathBuf>, ImportError> {
    check_cancelled(cancellation)?;
    let path = channel.path();
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ImportError::MissingSource(path.to_path_buf()));
        }
        Err(source) => return Err(io_error("inspect channel source", path, source)),
    };
    if metadata.file_type().is_symlink() {
        return Err(ImportError::UnsupportedSource(format!(
            "channel source {path:?} must not be a symbolic link"
        )));
    }
    if channel.kind() == TiffChannelSourceKind::Single3dTiff {
        ensure_regular_tiff_file(path)?;
        return Ok(vec![path.to_path_buf()]);
    }
    if !metadata.is_dir() {
        return Err(ImportError::UnsupportedSource(format!(
            "channel source {path:?} must be a directory for {:?}",
            channel.kind()
        )));
    }
    let mut paths = Vec::new();
    let directory =
        fs::read_dir(path).map_err(|source| io_error("list source directory", path, source))?;
    for entry in directory {
        check_cancelled(cancellation)?;
        let entry =
            entry.map_err(|source| io_error("read source directory entry", path, source))?;
        let entry_path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| io_error("inspect channel source entry", &entry_path, source))?;
        if file_type.is_file() && !file_type.is_symlink() && is_tiff_path(&entry_path) {
            paths.push(entry_path);
            if paths.len() > MAX_SOURCE_FILES {
                return Err(ImportError::UnsupportedSource(format!(
                    "channel source contains more than {MAX_SOURCE_FILES} TIFF files"
                )));
            }
        }
    }
    paths.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    if paths.is_empty() {
        return Err(ImportError::UnsupportedSource(format!(
            "channel source {path:?} contains no immediate TIFF files"
        )));
    }
    Ok(paths)
}

#[allow(clippy::too_many_arguments)]
fn finish_inspection(
    source: TiffSource,
    shape: Shape4D,
    dtype: IntensityDType,
    ome_spacing_zyx_um: Option<[f64; 3]>,
    maximum_decoded_chunk_bytes: u64,
    maximum_encoded_chunk_bytes: u64,
    mut files: Vec<InspectedSourceFile>,
) -> Result<TiffInspection, ImportError> {
    files.shrink_to_fit();
    for file in &mut files {
        file.path.shrink_to_fit();
        file.relative_name.shrink_to_fit();
    }
    let source_index_working_bytes = source_index_working_bytes(&source, &files)?;
    let source_bytes = files.iter().try_fold(0_u64, |total, file| {
        total.checked_add(file.bytes).ok_or(ImportError::Overflow)
    })?;
    let channels = u32::try_from(source.channels().len()).map_err(|_| ImportError::Overflow)?;
    let channel_labels = source.channel_labels().map(ToOwned::to_owned).collect();
    let source_fingerprint = aggregate_fingerprint(
        &source,
        shape,
        dtype,
        ome_spacing_zyx_um,
        source_bytes,
        &files,
    )?;
    Ok(TiffInspection {
        source,
        files,
        source_index_working_bytes,
        shape,
        channels,
        channel_labels,
        dtype,
        ome_spacing_zyx_um,
        source_bytes,
        source_fingerprint,
        maximum_decoded_chunk_bytes,
        maximum_encoded_chunk_bytes,
    })
}

fn source_index_working_bytes(
    source: &TiffSource,
    files: &[InspectedSourceFile],
) -> Result<u64, ImportError> {
    let root_bytes = source.channels().iter().try_fold(0_u64, |total, channel| {
        let path_bytes = u64::try_from(channel.path().as_os_str().as_bytes().len())
            .map_err(|_| ImportError::Overflow)?;
        let label_bytes =
            u64::try_from(channel.label().len()).map_err(|_| ImportError::Overflow)?;
        total
            .checked_add(path_bytes)
            .and_then(|value| value.checked_add(label_bytes))
            .ok_or(ImportError::Overflow)
    })?;
    let text_bytes = files.iter().try_fold(root_bytes, |total, file| {
        let path_bytes = u64::try_from(file.path.as_os_str().as_bytes().len())
            .map_err(|_| ImportError::Overflow)?;
        let name_bytes =
            u64::try_from(file.relative_name.len()).map_err(|_| ImportError::Overflow)?;
        total
            .checked_add(path_bytes)
            .and_then(|value| value.checked_add(name_bytes))
            .ok_or(ImportError::Overflow)
    })?;
    if text_bytes > MAX_SOURCE_INDEX_TEXT_BYTES {
        return Err(ImportError::UnsupportedSource(format!(
            "source path and filename metadata exceeds the fixed {MAX_SOURCE_INDEX_TEXT_BYTES}-byte aggregate bound"
        )));
    }
    let file_count = u64::try_from(files.len()).map_err(|_| ImportError::Overflow)?;
    let retained_struct_bytes = u64::try_from(size_of::<TiffInspection>())
        .map_err(|_| ImportError::Overflow)?
        .checked_add(
            file_count
                .checked_mul(
                    u64::try_from(size_of::<InspectedSourceFile>())
                        .map_err(|_| ImportError::Overflow)?,
                )
                .ok_or(ImportError::Overflow)?,
        )
        .ok_or(ImportError::Overflow)?;
    text_bytes
        .checked_mul(SOURCE_INDEX_TEXT_COPIES_MAX)
        .and_then(|value| {
            value.checked_add(file_count.checked_mul(SOURCE_INDEX_BYTES_PER_FILE_MAX)?)
        })
        .and_then(|value| value.checked_add(retained_struct_bytes))
        .and_then(|value| value.checked_add(SOURCE_INDEX_FIXED_BYTES_MAX))
        .ok_or(ImportError::Overflow)
}

pub(crate) fn revalidate(
    inspection: &TiffInspection,
    cancellation: &ImportCancellation,
) -> Result<SourceRevalidationCounters, ImportError> {
    check_cancelled(cancellation)?;
    let current_files = enumerate_accepted_layout_files(inspection, cancellation)?;
    let mut recorded = inspection.files.iter().enumerate().collect::<Vec<_>>();
    recorded.sort_by(|left, right| left.1.relative_name.cmp(&right.1.relative_name));

    if current_files.len() != recorded.len() {
        return Err(ImportError::SourceChanged(
            inspection.source.primary_path().to_path_buf(),
        ));
    }

    let mut counters = SourceRevalidationCounters {
        generation: SourceGeneration {
            files: inspection
                .files
                .iter()
                .map(|file| file.generation)
                .collect(),
        },
        ..SourceRevalidationCounters::default()
    };
    for ((relative_name, path), (recorded_index, expected)) in
        current_files.into_iter().zip(recorded)
    {
        if relative_name != expected.relative_name || path != expected.path {
            return Err(ImportError::SourceChanged(path));
        }
        let current = source_file_generation(&path)?;
        if current != expected.generation {
            return Err(ImportError::SourceChanged(path));
        }
        counters.generation.files[recorded_index] = current;
    }

    let source_bytes = inspection.files.iter().try_fold(0_u64, |total, file| {
        total.checked_add(file.bytes).ok_or(ImportError::Overflow)
    })?;
    if source_bytes != inspection.source_bytes {
        return Err(ImportError::SourceChanged(
            inspection.source.primary_path().to_path_buf(),
        ));
    }
    let fingerprint = aggregate_fingerprint(
        &inspection.source,
        inspection.shape,
        inspection.dtype,
        inspection.ome_spacing_zyx_um,
        source_bytes,
        &inspection.files,
    )?;
    if fingerprint != inspection.source_fingerprint {
        return Err(ImportError::SourceChanged(
            inspection.source.primary_path().to_path_buf(),
        ));
    }
    Ok(counters)
}

/// Captures the exact reviewed inventory and a kernel-backed file generation
/// before ingest without consuming the one in-clock strong integrity pass.
///
/// The generation includes file identity and change time, so a source write,
/// replacement, or metadata-restored transient mutation cannot be hidden by
/// merely restoring length and modification time. The final strong
/// [`revalidate`] pass still compares every byte with the reviewed SHA-256.
pub(crate) fn capture_generation(
    inspection: &TiffInspection,
    cancellation: &ImportCancellation,
) -> Result<SourceGeneration, ImportError> {
    check_cancelled(cancellation)?;
    let current_files = enumerate_accepted_layout_files(inspection, cancellation)?;
    let mut recorded = inspection.files.iter().enumerate().collect::<Vec<_>>();
    recorded.sort_by(|left, right| left.1.relative_name.cmp(&right.1.relative_name));
    if current_files.len() != recorded.len() {
        return Err(ImportError::SourceChanged(
            inspection.source.primary_path().to_path_buf(),
        ));
    }
    let mut generation = SourceGeneration {
        files: inspection
            .files
            .iter()
            .map(|file| file.generation)
            .collect(),
    };
    for ((relative_name, path), (recorded_index, expected)) in
        current_files.into_iter().zip(recorded)
    {
        check_cancelled(cancellation)?;
        if relative_name != expected.relative_name || path != expected.path {
            return Err(ImportError::SourceChanged(path));
        }
        let file_generation = source_file_generation(&path)?;
        if file_generation != expected.generation {
            return Err(ImportError::SourceChanged(path));
        }
        generation.files[recorded_index] = file_generation;
    }
    Ok(generation)
}

/// Resolves first-volume no-data facts from the durable canonical cache.
/// TIFF payloads have already been decoded exactly once before this boundary.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_no_data_policy_from_cache(
    inspection: &TiffInspection,
    canonical: &crate::canonical_cache::CanonicalBaseReader,
    checkpoint_directory: &Path,
    request: Option<NoDataPolicy>,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    mut plane_completed: impl FnMut(u64, Option<u64>),
) -> Result<NoDataDetectionReport, ImportError> {
    check_cancelled(cancellation)?;
    let depth = inspection.shape.z();
    let Some(request) = request else {
        return Ok(NoDataDetectionReport {
            policy: ResolvedNoDataPolicy::all_valid(depth),
            counters: SourceReadCounters::default(),
            peak_transient_bytes: 0,
            completed_planes: 0,
            resident_mask_lease: None,
        });
    };
    let automatic = request.value_rule() == Some(NoDataValueRule::Automatic);
    let mut resolved_value = match request.value_rule() {
        Some(NoDataValueRule::ManualUint8(value)) => {
            if inspection.dtype != IntensityDType::Uint8 {
                return Err(ImportError::InvalidRequest(
                    "manual no-data values are supported only for uint8 TIFF input",
                ));
            }
            Some(ResolvedNoDataValue::Uint8(value))
        }
        Some(NoDataValueRule::Automatic) | None => None,
    };
    if !automatic && !request.hides_constant_z_planes() {
        return Ok(NoDataDetectionReport {
            policy: ResolvedNoDataPolicy::new(
                Some(request),
                resolved_value,
                None,
                Vec::new(),
                depth,
            )
            .map_err(ImportError::InvalidRequest)?,
            counters: SourceReadCounters::default(),
            peak_transient_bytes: 0,
            completed_planes: 0,
            resident_mask_lease: None,
        });
    }

    let width = inspection.shape.x();
    let height = inspection.shape.y();
    let plane_bytes = width
        .checked_mul(height)
        .and_then(|samples| samples.checked_mul(u64::from(inspection.dtype.bytes_per_sample())))
        .ok_or(ImportError::Overflow)?;
    let detector_bytes = if automatic {
        automatic_discovery_detector_bytes(width, height).ok_or(ImportError::Overflow)?
    } else {
        0
    };
    let transient_bytes = plane_bytes
        .checked_add(detector_bytes)
        .and_then(|value| {
            value.checked_add(if automatic {
                automatic_reconstruction_transient_bytes([depth, height, width])?
            } else {
                0
            })
        })
        .ok_or(ImportError::Overflow)?;
    let _transient_lease =
        ledger.try_acquire(CpuLedgerCategory::ImportWorkingSet, transient_bytes.max(1))?;
    let mut plane = vec![0_u8; usize::try_from(plane_bytes).map_err(|_| ImportError::Overflow)?];
    let mut detector = automatic
        .then(|| UniformCubeDetector::new(width, height))
        .transpose()?;
    let mut constant_z_planes = Vec::new();
    let mut completed_planes = 0_u64;
    for z in 0..depth {
        check_cancelled(cancellation)?;
        canonical.read_region_into(0, 0, [z, 0, 0], [1, height, width], &mut plane)?;
        let constant = request.hides_constant_z_planes()
            && plane_is_exactly_constant(&plane, inspection.dtype)?;
        if constant {
            constant_z_planes.push(z);
            if let Some(detector) = detector.as_mut() {
                detector.break_z_continuity();
            }
        } else if automatic
            && resolved_value.is_none()
            && let Some(bits) = detector
                .as_mut()
                .expect("automatic mode owns a detector")
                .scan_plane(&plane, inspection.dtype, cancellation)?
        {
            resolved_value = Some(resolved_value_from_bits(inspection.dtype, bits));
            if !request.hides_constant_z_planes() {
                completed_planes = completed_planes
                    .checked_add(1)
                    .ok_or(ImportError::Overflow)?;
                plane_completed(completed_planes, None);
                break;
            }
        }
        completed_planes = completed_planes
            .checked_add(1)
            .ok_or(ImportError::Overflow)?;
        plane_completed(completed_planes, (!automatic).then_some(depth));
    }

    let automatic_mask = if automatic {
        match resolved_value {
            Some(value) => {
                let mut equal = PackedBits::new([depth, height, width])?;
                let mut seed_endpoints = PackedBits::new([depth, height, width])?;
                let mut seed_detector = FixedValueSeedDetector::new(width, height)?;
                for z in 0..depth {
                    check_cancelled(cancellation)?;
                    canonical.read_region_into(0, 0, [z, 0, 0], [1, height, width], &mut plane)?;
                    if constant_z_planes.binary_search(&z).is_ok() {
                        seed_detector.break_z_continuity();
                    } else {
                        seed_detector.scan_plane_packed(
                            z,
                            &plane,
                            inspection.dtype,
                            value.canonical_bits(),
                            &mut equal,
                            &mut seed_endpoints,
                            cancellation,
                        )?;
                    }
                    completed_planes = completed_planes
                        .checked_add(1)
                        .ok_or(ImportError::Overflow)?;
                    plane_completed(completed_planes, None);
                }
                let packed = reconstruct_seeded_runs(
                    [depth, height, width],
                    &equal,
                    &seed_endpoints,
                    checkpoint_directory,
                    cancellation,
                )?;
                Some(
                    ResolvedAutomaticNoDataMask::new([depth, height, width], packed)
                        .map_err(ImportError::InvalidRequest)?,
                )
            }
            None => None,
        }
    } else {
        None
    };
    let resident_mask_lease = automatic_mask
        .as_ref()
        .map(|mask| ledger.try_acquire(CpuLedgerCategory::ImportWorkingSet, mask.resident_bytes()))
        .transpose()?;
    Ok(NoDataDetectionReport {
        policy: ResolvedNoDataPolicy::new(
            Some(request),
            resolved_value,
            automatic_mask,
            constant_z_planes,
            depth,
        )
        .map_err(ImportError::InvalidRequest)?,
        counters: SourceReadCounters::default(),
        peak_transient_bytes: transient_bytes,
        completed_planes,
        resident_mask_lease,
    })
}

struct PackedBits {
    shape: [u64; 3],
    row_bytes: u64,
    len: u64,
    bytes: Vec<u8>,
}

impl PackedBits {
    fn new(shape: [u64; 3]) -> Result<Self, ImportError> {
        let len = shape
            .into_iter()
            .try_fold(1_u64, |product, value| product.checked_mul(value))
            .ok_or(ImportError::Overflow)?;
        let row_bytes = shape[2].div_ceil(8);
        let bytes = shape[0]
            .checked_mul(shape[1])
            .and_then(|rows| rows.checked_mul(row_bytes))
            .ok_or(ImportError::Overflow)?;
        Ok(Self {
            shape,
            row_bytes,
            len,
            bytes: vec![0; usize::try_from(bytes).map_err(|_| ImportError::Overflow)?],
        })
    }

    fn get(&self, index: u64) -> bool {
        let Some((byte, bit)) = self.bit_position(index) else {
            return false;
        };
        self.bytes[byte] & (1 << bit) != 0
    }

    fn set(&mut self, index: u64) -> Result<(), ImportError> {
        let (byte, bit) = self.bit_position(index).ok_or(ImportError::Overflow)?;
        self.bytes[byte] |= 1 << bit;
        Ok(())
    }

    fn bit_position(&self, index: u64) -> Option<(usize, u64)> {
        if index >= self.len {
            return None;
        }
        let plane = self.shape[1].checked_mul(self.shape[2])?;
        let z = index / plane;
        let within = index % plane;
        let y = within / self.shape[2];
        let x = within % self.shape[2];
        let byte = z
            .checked_mul(self.shape[1])?
            .checked_add(y)?
            .checked_mul(self.row_bytes)?
            .checked_add(x / 8)?;
        Some((usize::try_from(byte).ok()?, x % 8))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EqualRun {
    z: u64,
    y: u64,
    x_start: u64,
    x_end: u64,
}

fn reconstruct_seeded_runs(
    shape: [u64; 3],
    equal: &PackedBits,
    seeds: &PackedBits,
    checkpoint_directory: &Path,
    cancellation: &ImportCancellation,
) -> Result<Vec<u8>, ImportError> {
    let mut visited = PackedBits::new(shape)?;
    let mut stack = BoundedRunStack::new(checkpoint_directory)?;
    let row_stride = shape[2];
    let plane_stride = shape[1]
        .checked_mul(shape[2])
        .ok_or(ImportError::Overflow)?;
    for seed in 0..seeds.len {
        if !seeds.get(seed) {
            continue;
        }
        check_cancelled(cancellation)?;
        if !equal.get(seed) || visited.get(seed) {
            continue;
        }
        let z = seed / plane_stride;
        let row_offset = seed % plane_stride;
        let y = row_offset / row_stride;
        let x = row_offset % row_stride;
        push_equal_run(shape, equal, &mut visited, &mut stack, z, y, x)?;
        while let Some(run) = stack.pop()? {
            if stack
                .processed
                .is_multiple_of(RECONSTRUCTION_CANCEL_INTERVAL)
            {
                check_cancelled(cancellation)?;
            }
            for (neighbor_z, neighbor_y) in [
                run.y.checked_sub(1).map(|y| (run.z, y)),
                (run.y + 1 < shape[1]).then_some((run.z, run.y + 1)),
                run.z.checked_sub(1).map(|z| (z, run.y)),
                (run.z + 1 < shape[0]).then_some((run.z + 1, run.y)),
            ]
            .into_iter()
            .flatten()
            {
                let mut x = run.x_start;
                while x <= run.x_end {
                    let index = linear_index(shape, neighbor_z, neighbor_y, x)?;
                    if equal.get(index) && !visited.get(index) {
                        let added = push_equal_run(
                            shape,
                            equal,
                            &mut visited,
                            &mut stack,
                            neighbor_z,
                            neighbor_y,
                            x,
                        )?;
                        x = added.x_end.saturating_add(1);
                    } else {
                        x = x.saturating_add(1);
                    }
                }
            }
        }
    }
    stack.finish()?;
    Ok(visited.bytes)
}

fn push_equal_run(
    shape: [u64; 3],
    equal: &PackedBits,
    visited: &mut PackedBits,
    stack: &mut BoundedRunStack,
    z: u64,
    y: u64,
    x: u64,
) -> Result<EqualRun, ImportError> {
    let mut x_start = x;
    while x_start > 0 {
        let candidate = linear_index(shape, z, y, x_start - 1)?;
        if !equal.get(candidate) || visited.get(candidate) {
            break;
        }
        x_start -= 1;
    }
    let mut x_end = x;
    while x_end + 1 < shape[2] {
        let candidate = linear_index(shape, z, y, x_end + 1)?;
        if !equal.get(candidate) || visited.get(candidate) {
            break;
        }
        x_end += 1;
    }
    for current in x_start..=x_end {
        visited.set(linear_index(shape, z, y, current)?)?;
    }
    let run = EqualRun {
        z,
        y,
        x_start,
        x_end,
    };
    stack.push(run)?;
    Ok(run)
}

/// LIFO frontier with a fixed RAM window and a checkpoint-owned spill file.
/// The file is transient: canonical cache state is the resumable authority,
/// so an interrupted reconstruction safely starts this frontier again.
struct BoundedRunStack {
    path: PathBuf,
    file: File,
    device: u64,
    inode: u64,
    memory: Vec<EqualRun>,
    disk_runs: u64,
    processed: u64,
}

impl BoundedRunStack {
    fn new(checkpoint_directory: &Path) -> Result<Self, ImportError> {
        let path = checkpoint_directory.join(RECONSTRUCTION_RUN_SPOOL_NAME);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                fs::remove_file(&path).map_err(|source| {
                    io_error("remove stale automatic-mask run spool", &path, source)
                })?;
            }
            Ok(_) => {
                return Err(ImportError::InvalidCheckpoint(
                    "automatic-mask run spool is not a regular owned file".to_owned(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(io_error("inspect automatic-mask run spool", &path, source));
            }
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(
                i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits())
                    .expect("O_NOFOLLOW is representable as a platform open flag"),
            )
            .open(&path)
            .map_err(|source| io_error("create automatic-mask run spool", &path, source))?;
        let metadata = file
            .metadata()
            .map_err(|source| io_error("inspect automatic-mask run spool", &path, source))?;
        Ok(Self {
            path,
            file,
            device: metadata.dev(),
            inode: metadata.ino(),
            memory: Vec::with_capacity(RECONSTRUCTION_RUNS_IN_MEMORY),
            disk_runs: 0,
            processed: 0,
        })
    }

    fn push(&mut self, run: EqualRun) -> Result<(), ImportError> {
        if self.memory.len() == RECONSTRUCTION_RUNS_IN_MEMORY {
            self.flush_memory()?;
        }
        self.memory.push(run);
        Ok(())
    }

    fn pop(&mut self) -> Result<Option<EqualRun>, ImportError> {
        if self.memory.is_empty() && self.disk_runs != 0 {
            self.refill_memory()?;
        }
        let run = self.memory.pop();
        if run.is_some() {
            self.processed = self.processed.checked_add(1).ok_or(ImportError::Overflow)?;
        }
        Ok(run)
    }

    fn flush_memory(&mut self) -> Result<(), ImportError> {
        let initial_disk_runs = self.disk_runs;
        let mut bytes = [0_u8; RECONSTRUCTION_IO_BUFFER_BYTES];
        for (batch_index, runs) in self.memory.chunks(RECONSTRUCTION_IO_RUNS).enumerate() {
            let used = runs
                .len()
                .checked_mul(RECONSTRUCTION_RUN_RECORD_BYTES_USIZE)
                .ok_or(ImportError::Overflow)?;
            for (run_index, run) in runs.iter().enumerate() {
                let record_start = run_index * RECONSTRUCTION_RUN_RECORD_BYTES_USIZE;
                for (value_index, value) in [run.z, run.y, run.x_start, run.x_end]
                    .into_iter()
                    .enumerate()
                {
                    let start = record_start + value_index * size_of::<u64>();
                    bytes[start..start + size_of::<u64>()].copy_from_slice(&value.to_le_bytes());
                }
            }
            let batch_runs = u64::try_from(batch_index)
                .map_err(|_| ImportError::Overflow)?
                .checked_mul(
                    u64::try_from(RECONSTRUCTION_IO_RUNS).map_err(|_| ImportError::Overflow)?,
                )
                .ok_or(ImportError::Overflow)?;
            let offset = initial_disk_runs
                .checked_add(batch_runs)
                .and_then(|runs| runs.checked_mul(RECONSTRUCTION_RUN_RECORD_BYTES))
                .ok_or(ImportError::Overflow)?;
            self.file
                .write_all_at(&bytes[..used], offset)
                .map_err(|source| io_error("spill automatic-mask runs", &self.path, source))?;
        }
        self.disk_runs = self
            .disk_runs
            .checked_add(u64::try_from(self.memory.len()).map_err(|_| ImportError::Overflow)?)
            .ok_or(ImportError::Overflow)?;
        self.memory.clear();
        Ok(())
    }

    fn refill_memory(&mut self) -> Result<(), ImportError> {
        let count = self
            .disk_runs
            .min(u64::try_from(RECONSTRUCTION_RUNS_IN_MEMORY).expect("run bound fits u64"));
        let first = self.disk_runs - count;
        let mut bytes = [0_u8; RECONSTRUCTION_IO_BUFFER_BYTES];
        let mut loaded = 0_u64;
        while loaded < count {
            let batch_count = (count - loaded)
                .min(u64::try_from(RECONSTRUCTION_IO_RUNS).map_err(|_| ImportError::Overflow)?);
            let used = usize::try_from(
                batch_count
                    .checked_mul(RECONSTRUCTION_RUN_RECORD_BYTES)
                    .ok_or(ImportError::Overflow)?,
            )
            .map_err(|_| ImportError::Overflow)?;
            let offset = first
                .checked_add(loaded)
                .and_then(|runs| runs.checked_mul(RECONSTRUCTION_RUN_RECORD_BYTES))
                .ok_or(ImportError::Overflow)?;
            self.file
                .read_exact_at(&mut bytes[..used], offset)
                .map_err(|source| io_error("read automatic-mask run spool", &self.path, source))?;
            for record in bytes[..used].chunks_exact(RECONSTRUCTION_RUN_RECORD_BYTES_USIZE) {
                let decode = |offset: usize| {
                    u64::from_le_bytes(
                        record[offset..offset + size_of::<u64>()]
                            .try_into()
                            .expect("fixed run record"),
                    )
                };
                self.memory.push(EqualRun {
                    z: decode(0),
                    y: decode(8),
                    x_start: decode(16),
                    x_end: decode(24),
                });
            }
            loaded = loaded
                .checked_add(batch_count)
                .ok_or(ImportError::Overflow)?;
        }
        self.file
            .set_len(
                first
                    .checked_mul(RECONSTRUCTION_RUN_RECORD_BYTES)
                    .ok_or(ImportError::Overflow)?,
            )
            .map_err(|source| io_error("truncate automatic-mask run spool", &self.path, source))?;
        self.disk_runs = first;
        Ok(())
    }

    fn finish(mut self) -> Result<(), ImportError> {
        if self.disk_runs != 0 || !self.memory.is_empty() {
            return Err(ImportError::InvalidCheckpoint(
                "automatic-mask run reconstruction ended with pending work".to_owned(),
            ));
        }
        self.remove_owned()
    }

    fn remove_owned(&mut self) -> Result<(), ImportError> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(io_error(
                    "inspect automatic-mask run spool for cleanup",
                    &self.path,
                    source,
                ));
            }
        };
        if metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            fs::remove_file(&self.path).map_err(|source| {
                io_error("remove automatic-mask run spool", &self.path, source)
            })?;
        }
        Ok(())
    }
}

impl Drop for BoundedRunStack {
    fn drop(&mut self) {
        let _ = self.remove_owned();
    }
}

fn linear_index(shape: [u64; 3], z: u64, y: u64, x: u64) -> Result<u64, ImportError> {
    z.checked_mul(shape[1])
        .and_then(|value| value.checked_add(y))
        .and_then(|value| value.checked_mul(shape[2]))
        .and_then(|value| value.checked_add(x))
        .ok_or(ImportError::Overflow)
}

pub(crate) fn automatic_mask_packed_bytes(shape_zyx: [u64; 3]) -> Option<u64> {
    shape_zyx[0]
        .checked_mul(shape_zyx[1])?
        .checked_mul(shape_zyx[2].div_ceil(8))
}

pub(crate) fn automatic_discovery_detector_bytes(width: u64, height: u64) -> Option<u64> {
    width
        .checked_mul(height)?
        .checked_add(width)?
        .checked_mul(8)
}

pub(crate) fn automatic_reconstruction_transient_bytes(shape_zyx: [u64; 3]) -> Option<u64> {
    if shape_zyx
        .iter()
        .any(|dimension| *dimension < u64::from(AUTOMATIC_NO_DATA_BLOCK_EDGE))
    {
        return Some(0);
    }
    // Exact-value, seed-endpoint, and visited/final-mask bitsets plus the
    // fixed in-memory window of the disk-backed run frontier.
    automatic_mask_packed_bytes(shape_zyx)?
        .checked_mul(3)?
        .checked_add(
            u64::try_from(RECONSTRUCTION_RUNS_IN_MEMORY)
                .ok()?
                .checked_mul(RECONSTRUCTION_RUN_RECORD_BYTES)?,
        )
        .and_then(|bytes| bytes.checked_add(RECONSTRUCTION_IO_BUFFER_BYTES as u64))
}

pub(crate) fn automatic_reconstruction_spool_bytes(shape_zyx: [u64; 3]) -> Option<u64> {
    if shape_zyx
        .iter()
        .any(|dimension| *dimension < u64::from(AUTOMATIC_NO_DATA_BLOCK_EDGE))
    {
        return Some(0);
    }
    // An exact-value component can contribute at most ceil(X / 2) disjoint
    // runs per Y/Z row. Each run is pushed once because it is marked visited
    // before entering the frontier.
    shape_zyx[0]
        .checked_mul(shape_zyx[1])?
        .checked_mul(shape_zyx[2].div_ceil(2))?
        .checked_mul(RECONSTRUCTION_RUN_RECORD_BYTES)
}

const AUTOMATIC_NO_DATA_BLOCK_EDGE: u32 = 5;
const RECONSTRUCTION_CANCEL_INTERVAL: u64 = 65_536;
const RECONSTRUCTION_RUNS_IN_MEMORY: usize = 4_096;
const RECONSTRUCTION_RUN_RECORD_BYTES: u64 = 32;
const RECONSTRUCTION_RUN_RECORD_BYTES_USIZE: usize = RECONSTRUCTION_RUN_RECORD_BYTES as usize;
const RECONSTRUCTION_IO_RUNS: usize = 64;
const RECONSTRUCTION_IO_BUFFER_BYTES: usize =
    RECONSTRUCTION_IO_RUNS * RECONSTRUCTION_RUN_RECORD_BYTES_USIZE;
const RECONSTRUCTION_RUN_SPOOL_NAME: &str = "automatic-mask-runs";

struct UniformCubeDetector {
    width: usize,
    height: usize,
    vertical_value: Vec<u32>,
    vertical_count: Vec<u32>,
    depth_value: Vec<u32>,
    depth_count: Vec<u32>,
}

/// Marks one deterministic endpoint for every exact-value `5 x 5 x 5`
/// seed in a complete first-volume scan.
///
/// A single marked voxel is sufficient: reconstruction floods the entire
/// six-connected exact-value component containing that endpoint. Running
/// counts are capped at the seed edge, so the detector needs one byte per
/// plane sample plus one byte per row sample.
struct FixedValueSeedDetector {
    width: usize,
    height: usize,
    vertical_count: Vec<u8>,
    depth_count: Vec<u8>,
}

impl FixedValueSeedDetector {
    fn new(width: u64, height: u64) -> Result<Self, ImportError> {
        let width = usize::try_from(width).map_err(|_| ImportError::Overflow)?;
        let height = usize::try_from(height).map_err(|_| ImportError::Overflow)?;
        let plane = width.checked_mul(height).ok_or(ImportError::Overflow)?;
        Ok(Self {
            width,
            height,
            vertical_count: vec![0; width],
            depth_count: vec![0; plane],
        })
    }

    fn break_z_continuity(&mut self) {
        self.depth_count.fill(0);
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the detector boundary names each separately bounded source/mask authority"
    )]
    fn scan_plane_packed(
        &mut self,
        z: u64,
        plane_le: &[u8],
        dtype: IntensityDType,
        target_bits: u32,
        equal: &mut PackedBits,
        seeds: &mut PackedBits,
        cancellation: &ImportCancellation,
    ) -> Result<(), ImportError> {
        let sample_width = usize::from(dtype.bytes_per_sample());
        let plane_samples = self
            .width
            .checked_mul(self.height)
            .ok_or(ImportError::Overflow)?;
        if plane_le.len()
            != plane_samples
                .checked_mul(sample_width)
                .ok_or(ImportError::Overflow)?
        {
            return Err(ImportError::InvalidRequest(
                "automatic no-data reconstruction received a malformed canonical plane",
            ));
        }
        let global_start = z
            .checked_mul(u64::try_from(plane_samples).map_err(|_| ImportError::Overflow)?)
            .ok_or(ImportError::Overflow)?;
        let edge =
            u8::try_from(AUTOMATIC_NO_DATA_BLOCK_EDGE).expect("automatic no-data edge fits u8");
        self.vertical_count.fill(0);
        for y in 0..self.height {
            let mut horizontal_count = 0_u8;
            for x in 0..self.width {
                let local = y
                    .checked_mul(self.width)
                    .and_then(|value| value.checked_add(x))
                    .ok_or(ImportError::Overflow)?;
                if u64::try_from(local)
                    .map_err(|_| ImportError::Overflow)?
                    .is_multiple_of(RECONSTRUCTION_CANCEL_INTERVAL)
                {
                    check_cancelled(cancellation)?;
                }
                let exact = sample_bits(plane_le, dtype, local)? == target_bits;
                if exact {
                    equal.set(
                        global_start
                            .checked_add(u64::try_from(local).map_err(|_| ImportError::Overflow)?)
                            .ok_or(ImportError::Overflow)?,
                    )?;
                    horizontal_count = horizontal_count.saturating_add(1).min(edge);
                } else {
                    horizontal_count = 0;
                }
                if horizontal_count >= edge {
                    self.vertical_count[x] = self.vertical_count[x].saturating_add(1).min(edge);
                } else {
                    self.vertical_count[x] = 0;
                }
                if self.vertical_count[x] >= edge {
                    self.depth_count[local] = self.depth_count[local].saturating_add(1).min(edge);
                } else {
                    self.depth_count[local] = 0;
                }
                if self.depth_count[local] >= edge {
                    seeds.set(
                        global_start
                            .checked_add(u64::try_from(local).map_err(|_| ImportError::Overflow)?)
                            .ok_or(ImportError::Overflow)?,
                    )?;
                }
            }
        }
        Ok(())
    }
}

impl UniformCubeDetector {
    fn new(width: u64, height: u64) -> Result<Self, ImportError> {
        let width = usize::try_from(width).map_err(|_| ImportError::Overflow)?;
        let height = usize::try_from(height).map_err(|_| ImportError::Overflow)?;
        let plane = width.checked_mul(height).ok_or(ImportError::Overflow)?;
        Ok(Self {
            width,
            height,
            vertical_value: vec![0; width],
            vertical_count: vec![0; width],
            depth_value: vec![0; plane],
            depth_count: vec![0; plane],
        })
    }

    fn break_z_continuity(&mut self) {
        self.depth_count.fill(0);
    }

    fn scan_plane(
        &mut self,
        plane_le: &[u8],
        dtype: IntensityDType,
        cancellation: &ImportCancellation,
    ) -> Result<Option<u32>, ImportError> {
        let expected = self
            .width
            .checked_mul(self.height)
            .and_then(|value| value.checked_mul(usize::from(dtype.bytes_per_sample())))
            .ok_or(ImportError::Overflow)?;
        if plane_le.len() != expected {
            return Err(ImportError::InvalidRequest(
                "automatic no-data detector received a malformed plane",
            ));
        }
        self.vertical_count.fill(0);
        let edge = AUTOMATIC_NO_DATA_BLOCK_EDGE;
        for y in 0..self.height {
            let mut horizontal_value = 0_u32;
            let mut horizontal_count = 0_u32;
            for x in 0..self.width {
                let index = y
                    .checked_mul(self.width)
                    .and_then(|value| value.checked_add(x))
                    .ok_or(ImportError::Overflow)?;
                if u64::try_from(index)
                    .map_err(|_| ImportError::Overflow)?
                    .is_multiple_of(RECONSTRUCTION_CANCEL_INTERVAL)
                {
                    check_cancelled(cancellation)?;
                }
                let value = sample_bits(plane_le, dtype, index)?;
                if x > 0 && value == horizontal_value {
                    horizontal_count = horizontal_count.saturating_add(1);
                } else {
                    horizontal_value = value;
                    horizontal_count = 1;
                }
                let patch = if horizontal_count >= edge {
                    if y > 0 && self.vertical_count[x] != 0 && self.vertical_value[x] == value {
                        self.vertical_count[x] = self.vertical_count[x].saturating_add(1);
                    } else {
                        self.vertical_value[x] = value;
                        self.vertical_count[x] = 1;
                    }
                    self.vertical_count[x] >= edge
                } else {
                    self.vertical_count[x] = 0;
                    false
                };
                if patch {
                    if self.depth_count[index] != 0 && self.depth_value[index] == value {
                        self.depth_count[index] = self.depth_count[index].saturating_add(1);
                    } else {
                        self.depth_value[index] = value;
                        self.depth_count[index] = 1;
                    }
                    if self.depth_count[index] >= edge {
                        return Ok(Some(value));
                    }
                } else {
                    self.depth_count[index] = 0;
                }
            }
        }
        Ok(None)
    }
}

fn plane_is_exactly_constant(plane_le: &[u8], dtype: IntensityDType) -> Result<bool, ImportError> {
    let width = usize::from(dtype.bytes_per_sample());
    let mut samples = plane_le.chunks_exact(width);
    let Some(first) = samples.next() else {
        return Err(ImportError::InvalidRequest(
            "constant-plane detection requires a nonempty plane",
        ));
    };
    if !samples.remainder().is_empty() {
        return Err(ImportError::InvalidRequest(
            "constant-plane detection received partial sample bytes",
        ));
    }
    Ok(samples.all(|sample| sample == first))
}

fn sample_bits(plane_le: &[u8], dtype: IntensityDType, index: usize) -> Result<u32, ImportError> {
    let width = usize::from(dtype.bytes_per_sample());
    let start = index.checked_mul(width).ok_or(ImportError::Overflow)?;
    let sample = plane_le
        .get(start..start + width)
        .ok_or(ImportError::InvalidRequest(
            "automatic no-data detector indexed outside its plane",
        ))?;
    Ok(match dtype {
        IntensityDType::Uint8 => u32::from(sample[0]),
        IntensityDType::Uint16 => u32::from(u16::from_le_bytes([sample[0], sample[1]])),
        IntensityDType::Float32 => u32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]),
    })
}

const fn resolved_value_from_bits(dtype: IntensityDType, bits: u32) -> ResolvedNoDataValue {
    match dtype {
        IntensityDType::Uint8 => ResolvedNoDataValue::Uint8(bits as u8),
        IntensityDType::Uint16 => ResolvedNoDataValue::Uint16(bits as u16),
        IntensityDType::Float32 => ResolvedNoDataValue::Float32Bits(bits),
    }
}

/// Decodes the inspected source exactly once in native strip/tile order into
/// the durable canonical base checkpoint.
///
/// A resumed import skips the already durable plane prefix. Within every
/// remaining file, the decoder is opened once and each admitted native chunk
/// is read once. The source files remain immutable and are strongly checked
/// again by the pipeline before publication.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(crate) fn decode_canonical_into_cache(
    inspection: &TiffInspection,
    generation: &SourceGeneration,
    cache: &mut CanonicalBaseCache,
    maximum_decoded_chunk_bytes: u64,
    managed_capacity_bytes: u64,
    resident_working_bytes: u64,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    plane_completed: impl FnMut(u64, u64),
) -> Result<SourceDecodeReport, ImportError> {
    decode_canonical_into_cache_with_parallelism_limit(
        inspection,
        generation,
        cache,
        maximum_decoded_chunk_bytes,
        managed_capacity_bytes,
        resident_working_bytes,
        ledger,
        cancellation,
        crate::ordered_workers::system_parallelism(),
        plane_completed,
    )
}

#[allow(clippy::too_many_arguments)]
fn decode_canonical_into_cache_with_parallelism_limit(
    inspection: &TiffInspection,
    generation: &SourceGeneration,
    cache: &mut CanonicalBaseCache,
    maximum_decoded_chunk_bytes: u64,
    managed_capacity_bytes: u64,
    resident_working_bytes: u64,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    parallelism_limit: usize,
    mut plane_completed: impl FnMut(u64, u64),
) -> Result<SourceDecodeReport, ImportError> {
    check_cancelled(cancellation)?;
    if generation.files.len() != inspection.files.len() {
        return Err(ImportError::SourceChanged(
            inspection.source.primary_path().to_path_buf(),
        ));
    }
    let mut ordered = inspection.files.iter().enumerate().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        left.1
            .channel
            .cmp(&right.1.channel)
            .then(left.1.timepoint.cmp(&right.1.timepoint))
            .then(left.1.first_z.cmp(&right.1.first_z))
            .then(left.1.relative_name.cmp(&right.1.relative_name))
    });
    let resume_plane = cache.durable_planes();
    let total_planes = cache.total_planes();
    let mut next_plane = resume_plane;
    let mut remaining = Vec::new();
    for (file_index, file) in ordered {
        let expected_generation = generation
            .files
            .get(file_index)
            .ok_or_else(|| ImportError::SourceChanged(file.path.clone()))?;
        let first_plane = cache.plane_ordinal(file.channel, file.timepoint, file.first_z)?;
        let end_plane = first_plane
            .checked_add(file.planes)
            .ok_or(ImportError::Overflow)?;
        if end_plane <= resume_plane {
            continue;
        }
        if first_plane > next_plane {
            return Err(ImportError::UnsupportedSource(
                "the inspected source has a gap in canonical plane order".to_owned(),
            ));
        }
        remaining.push((file, expected_generation, first_plane));
        next_plane = end_plane.max(next_plane);
    }
    if next_plane != total_planes {
        return Err(ImportError::UnsupportedSource(
            "the inspected source does not cover every canonical plane".to_owned(),
        ));
    }

    let row_bytes = inspection
        .shape
        .x()
        .checked_mul(u64::from(inspection.dtype.bytes_per_sample()))
        .ok_or(ImportError::Overflow)?;
    let plane_bytes = inspection
        .shape
        .y()
        .checked_mul(row_bytes)
        .ok_or(ImportError::Overflow)?;
    let maximum_encoded_chunk_bytes = inspection.maximum_encoded_chunk_bytes;
    let maximum_file_pages = inspection
        .files
        .iter()
        .map(|file| file.planes)
        .max()
        .unwrap_or(1);
    let serial_retained_bytes =
        retained_decoder_bytes(maximum_file_pages).ok_or(ImportError::Overflow)?;
    let parallel_retained_bytes = retained_decoder_bytes(1).ok_or(ImportError::Overflow)?;
    let serial_transition_bytes = if maximum_file_pages > 1 {
        SOURCE_SERIAL_TRANSITION_BYTES_MAX
    } else {
        0
    };
    let serial_charge = maximum_decoded_chunk_bytes
        .checked_add(maximum_encoded_chunk_bytes)
        .and_then(|value| value.checked_add(SOURCE_DECODE_FIXED_OVERHEAD_BYTES_MAX))
        .and_then(|value| value.checked_add(serial_retained_bytes))
        .and_then(|value| value.checked_add(serial_transition_bytes))
        .and_then(|value| value.checked_add(row_bytes))
        .and_then(|value| value.checked_add(SOURCE_TASK_METADATA_BYTES_MAX))
        .ok_or(ImportError::Overflow)?;
    let parallel_charge = maximum_decoded_chunk_bytes
        .checked_add(maximum_encoded_chunk_bytes)
        .and_then(|value| value.checked_add(SOURCE_DECODE_FIXED_OVERHEAD_BYTES_MAX))
        .and_then(|value| value.checked_add(parallel_retained_bytes))
        .and_then(|value| value.checked_add(plane_bytes))
        .and_then(|value| value.checked_add(SOURCE_TASK_METADATA_BYTES_MAX))
        .ok_or(ImportError::Overflow)?;
    let parallel_policy = OrderedWorkerPolicy::for_system_with_parallelism_limit(
        parallelism_limit,
        managed_capacity_bytes,
        resident_working_bytes,
        parallel_charge,
    )
    .ok();
    let mut report = SourceDecodeReport::default();
    let mut index = 0_usize;
    while index < remaining.len() {
        check_cancelled(cancellation)?;
        if let Some(policy) = parallel_policy.filter(|_| remaining[index].0.planes == 1) {
            let run_start = index;
            while index < remaining.len() && remaining[index].0.planes == 1 {
                index += 1;
            }
            let diagnostics = run_ordered(
                policy,
                ledger,
                cancellation,
                remaining[run_start..index].iter().copied(),
                cache,
                |_cache, (file, expected_generation, plane)| {
                    Ok(SourcePlaneCpuTask {
                        inspection,
                        file,
                        expected_generation,
                        plane,
                        maximum_decoded_chunk_bytes,
                        maximum_encoded_chunk_bytes,
                    })
                },
                decode_single_plane_task,
                CanonicalBaseCache::commit_expired,
                |cache, decoded| {
                    check_cancelled(cancellation)?;
                    if source_file_generation(&decoded.path)? != decoded.expected_generation {
                        return Err(ImportError::SourceChanged(decoded.path));
                    }
                    write_canonical_plane(cache, decoded.plane, inspection, &decoded.pixels_le)?;
                    cache.complete_plane(decoded.plane)?;
                    add_source_counters(&mut report.counters, decoded.counters)?;
                    plane_completed(decoded.plane + 1, cache.total_planes());
                    Ok(())
                },
            )?;
            record_source_worker_diagnostics(&mut report, parallel_charge, diagnostics)?;
            continue;
        }

        let (file, expected_generation, _) = remaining[index];
        // The exact-once multipage/oversized-file boundary can block inside a
        // decoder without returning owner ticks. Do not carry a completed
        // canonical prefix across that unbounded interval.
        cache.commit_pending()?;
        let combined = resident_working_bytes
            .checked_add(serial_charge)
            .ok_or(ImportError::Overflow)?;
        if combined > managed_capacity_bytes {
            return Err(ImportError::ManagedCapacityInsufficient {
                required_bytes: combined,
                capacity_bytes: managed_capacity_bytes,
            });
        }
        let _lease = ledger.try_acquire(
            mirante4d_dataset::CpuLedgerCategory::ImportWorkingSet,
            serial_charge,
        )?;
        decode_file_into_cache(
            inspection,
            file,
            expected_generation,
            resume_plane,
            cache,
            maximum_decoded_chunk_bytes,
            maximum_encoded_chunk_bytes,
            cancellation,
            &mut report.counters,
            &mut plane_completed,
        )?;
        report.serialized_files = report
            .serialized_files
            .checked_add(1)
            .ok_or(ImportError::Overflow)?;
        report.peak_transient_bytes = report.peak_transient_bytes.max(serial_charge);
        index += 1;
    }
    cache.commit_pending()?;
    Ok(report)
}

/// Decodes exactly one reviewed `(channel, timepoint)` volume into a local
/// `T=1,C=1` canonical cache while preserving the explicit source manifest's
/// Z/file order. This is the temporal production boundary used by the
/// importer; total movie length cannot increase decoded checkpoint payload.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_canonical_volume_into_cache(
    inspection: &TiffInspection,
    generation: &SourceGeneration,
    channel: u32,
    timepoint: u64,
    cache: &mut CanonicalBaseCache,
    maximum_decoded_chunk_bytes: u64,
    managed_capacity_bytes: u64,
    resident_working_bytes: u64,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    parallelism_limit: usize,
    plane_completed: impl FnMut(u64, u64),
) -> Result<SourceDecodeReport, ImportError> {
    if channel >= inspection.channels || timepoint >= inspection.shape.t() {
        return Err(ImportError::InvalidRequest(
            "canonical volume coordinate is outside the inspected source",
        ));
    }
    if generation.files.len() != inspection.files.len() {
        return Err(ImportError::SourceChanged(
            inspection.source.primary_path().to_path_buf(),
        ));
    }
    let mut files = Vec::new();
    let mut generations = Vec::new();
    let mut source_bytes = 0_u64;
    for (index, file) in inspection.files.iter().enumerate() {
        if file.channel != channel || file.timepoint != timepoint {
            continue;
        }
        let mut local = file.clone();
        local.channel = 0;
        local.timepoint = 0;
        source_bytes = source_bytes
            .checked_add(local.bytes)
            .ok_or(ImportError::Overflow)?;
        files.push(local);
        generations.push(
            *generation
                .files
                .get(index)
                .ok_or_else(|| ImportError::SourceChanged(file.path.clone()))?,
        );
    }
    if files.is_empty() {
        return Err(ImportError::UnsupportedSource(
            "the reviewed source manifest does not cover a requested volume".to_owned(),
        ));
    }
    let local = TiffInspection {
        source: inspection.source.clone(),
        files,
        source_index_working_bytes: inspection.source_index_working_bytes,
        shape: Shape4D::new(
            1,
            inspection.shape.z(),
            inspection.shape.y(),
            inspection.shape.x(),
        )
        .map_err(|_| ImportError::Overflow)?,
        channels: 1,
        channel_labels: vec![
            inspection.channel_labels
                [usize::try_from(channel).map_err(|_| ImportError::Overflow)?]
            .clone(),
        ],
        dtype: inspection.dtype,
        ome_spacing_zyx_um: inspection.ome_spacing_zyx_um,
        source_bytes,
        source_fingerprint: inspection.source_fingerprint,
        maximum_decoded_chunk_bytes: inspection.maximum_decoded_chunk_bytes,
        maximum_encoded_chunk_bytes: inspection.maximum_encoded_chunk_bytes,
    };
    decode_canonical_into_cache_with_parallelism_limit(
        &local,
        &SourceGeneration { files: generations },
        cache,
        maximum_decoded_chunk_bytes,
        managed_capacity_bytes,
        resident_working_bytes,
        ledger,
        cancellation,
        parallelism_limit,
        plane_completed,
    )
}

/// Conservative transient byte ceiling for one canonical temporal/channel
/// decode. Decode-ahead admission uses the larger serial/parallel route so a
/// speculative lane can never borrow bytes protected for current-unit
/// progress merely because the source happens to choose a different TIFF
/// layout at runtime.
pub(crate) fn canonical_volume_decode_transient_bytes(
    inspection: &TiffInspection,
) -> Result<u64, ImportError> {
    let row_bytes = inspection
        .shape
        .x()
        .checked_mul(u64::from(inspection.dtype.bytes_per_sample()))
        .ok_or(ImportError::Overflow)?;
    let plane_bytes = inspection
        .shape
        .y()
        .checked_mul(row_bytes)
        .ok_or(ImportError::Overflow)?;
    let maximum_file_pages = inspection
        .files
        .iter()
        .map(|file| file.planes)
        .max()
        .unwrap_or(1);
    let common = inspection
        .maximum_decoded_chunk_bytes
        .checked_add(inspection.maximum_encoded_chunk_bytes)
        .and_then(|value| value.checked_add(SOURCE_DECODE_FIXED_OVERHEAD_BYTES_MAX))
        .ok_or(ImportError::Overflow)?;
    let serial = common
        .checked_add(retained_decoder_bytes(maximum_file_pages).ok_or(ImportError::Overflow)?)
        .and_then(|value| {
            value.checked_add(if maximum_file_pages > 1 {
                SOURCE_SERIAL_TRANSITION_BYTES_MAX
            } else {
                0
            })
        })
        .and_then(|value| value.checked_add(row_bytes))
        .and_then(|value| value.checked_add(SOURCE_TASK_METADATA_BYTES_MAX))
        .ok_or(ImportError::Overflow)?;
    let parallel = common
        .checked_add(retained_decoder_bytes(1).ok_or(ImportError::Overflow)?)
        .and_then(|value| value.checked_add(plane_bytes))
        .and_then(|value| value.checked_add(SOURCE_TASK_METADATA_BYTES_MAX))
        .ok_or(ImportError::Overflow)?;
    Ok(serial.max(parallel))
}

#[derive(Clone, Copy)]
struct SourcePlaneCpuTask<'a> {
    inspection: &'a TiffInspection,
    file: &'a InspectedSourceFile,
    expected_generation: &'a SourceFileGeneration,
    plane: u64,
    maximum_decoded_chunk_bytes: u64,
    maximum_encoded_chunk_bytes: u64,
}

struct DecodedCanonicalPlane {
    path: PathBuf,
    expected_generation: SourceFileGeneration,
    plane: u64,
    pixels_le: Vec<u8>,
    counters: SourceReadCounters,
}

fn decode_single_plane_task(
    task: SourcePlaneCpuTask<'_>,
    cancellation: &ImportCancellation,
) -> Result<DecodedCanonicalPlane, ImportError> {
    check_cancelled(cancellation)?;
    if task.file.planes != 1 {
        return Err(ImportError::InvalidRequest(
            "parallel source decode accepts exactly one TIFF plane per file",
        ));
    }
    if source_file_generation(&task.file.path)? != *task.expected_generation {
        return Err(ImportError::SourceChanged(task.file.path.clone()));
    }
    let raw = open_source_file(&task.file.path, "open source for parallel canonical decode")?;
    if source_file_generation_from_open_file(&task.file.path, &raw)? != *task.expected_generation {
        return Err(ImportError::SourceChanged(task.file.path.clone()));
    }
    let preflight_bytes = preflight_tiff_ifd_chain(&task.file.path, &raw, cancellation)?;
    if source_file_generation_from_open_file(&task.file.path, &raw)? != *task.expected_generation {
        return Err(ImportError::SourceChanged(task.file.path.clone()));
    }
    let counting = CountingReader::new(raw);
    let reader = BufReader::with_capacity(HASH_READ_BYTES, counting);
    let decode_limit = usize::try_from(task.maximum_decoded_chunk_bytes).unwrap_or(usize::MAX);
    let mut limits = Limits::default();
    limits.decoding_buffer_size = decode_limit.max(MAX_OME_XML_BYTES);
    let encoded_limit = usize::try_from(task.maximum_encoded_chunk_bytes).unwrap_or(usize::MAX);
    limits.intermediate_buffer_size =
        encoded_limit.saturating_add(MAX_ENCODED_CHUNK_OVERHEAD_BYTES);
    limits.ifd_value_size = MAX_OME_XML_BYTES;
    let mut decoder = Decoder::new(reader)
        .map_err(|error| tiff_error(&task.file.path, error))?
        .with_limits(limits);
    validate_current_page(
        &task.file.path,
        &mut decoder,
        task.inspection.shape.x(),
        task.inspection.shape.y(),
        task.inspection.dtype,
    )?;
    let plane_bytes = task
        .inspection
        .shape
        .y()
        .checked_mul(task.inspection.shape.x())
        .and_then(|value| value.checked_mul(u64::from(task.inspection.dtype.bytes_per_sample())))
        .ok_or(ImportError::Overflow)?;
    let mut pixels_le = vec![0; usize::try_from(plane_bytes).map_err(|_| ImportError::Overflow)?];
    let mut counters = SourceReadCounters {
        tiff_open_count: 1,
        ..SourceReadCounters::default()
    };
    decode_page_into_plane(
        &task.file.path,
        &mut decoder,
        task.inspection.dtype,
        &mut pixels_le,
        task.maximum_decoded_chunk_bytes,
        cancellation,
        &mut counters,
    )?;
    let bytes_read = decoder.inner().get_ref().bytes_read;
    if source_file_generation_from_open_file(&task.file.path, decoder.inner().get_ref().inner())?
        != *task.expected_generation
    {
        return Err(ImportError::SourceChanged(task.file.path.clone()));
    }
    if source_file_generation(&task.file.path)? != *task.expected_generation {
        return Err(ImportError::SourceChanged(task.file.path.clone()));
    }
    counters.source_bytes_read = bytes_read
        .checked_add(preflight_bytes)
        .ok_or(ImportError::Overflow)?;
    Ok(DecodedCanonicalPlane {
        path: task.file.path.clone(),
        expected_generation: *task.expected_generation,
        plane: task.plane,
        pixels_le,
        counters,
    })
}

fn record_source_worker_diagnostics(
    report: &mut SourceDecodeReport,
    task_charge_bytes: u64,
    diagnostics: OrderedWorkerDiagnostics,
) -> Result<(), ImportError> {
    let in_flight = u64::try_from(diagnostics.peak_in_flight).map_err(|_| ImportError::Overflow)?;
    report.peak_transient_bytes = report.peak_transient_bytes.max(
        in_flight
            .checked_mul(task_charge_bytes)
            .ok_or(ImportError::Overflow)?,
    );
    report.parallel_files = report
        .parallel_files
        .checked_add(diagnostics.committed_tasks)
        .ok_or(ImportError::Overflow)?;
    report.peak_reorder_results = report
        .peak_reorder_results
        .max(diagnostics.peak_reorder_results);
    Ok(())
}

fn add_source_counters(
    total: &mut SourceReadCounters,
    next: SourceReadCounters,
) -> Result<(), ImportError> {
    total.source_bytes_read = total
        .source_bytes_read
        .checked_add(next.source_bytes_read)
        .ok_or(ImportError::Overflow)?;
    total.decoded_bytes = total
        .decoded_bytes
        .checked_add(next.decoded_bytes)
        .ok_or(ImportError::Overflow)?;
    total.tiff_open_count = total
        .tiff_open_count
        .checked_add(next.tiff_open_count)
        .ok_or(ImportError::Overflow)?;
    total.native_chunk_decode_count = total
        .native_chunk_decode_count
        .checked_add(next.native_chunk_decode_count)
        .ok_or(ImportError::Overflow)?;
    Ok(())
}

fn write_canonical_plane(
    cache: &mut CanonicalBaseCache,
    plane: u64,
    inspection: &TiffInspection,
    pixels_le: &[u8],
) -> Result<(), ImportError> {
    let row_bytes = inspection
        .shape
        .x()
        .checked_mul(u64::from(inspection.dtype.bytes_per_sample()))
        .ok_or(ImportError::Overflow)?;
    let row_bytes = usize::try_from(row_bytes).map_err(|_| ImportError::Overflow)?;
    let expected = usize::try_from(inspection.shape.y())
        .map_err(|_| ImportError::Overflow)?
        .checked_mul(row_bytes)
        .ok_or(ImportError::Overflow)?;
    if pixels_le.len() != expected {
        return Err(ImportError::InvalidRequest(
            "a decoded canonical plane has the wrong byte length",
        ));
    }
    for (y, row) in pixels_le.chunks_exact(row_bytes).enumerate() {
        cache.write_row(
            plane,
            u64::try_from(y).map_err(|_| ImportError::Overflow)?,
            0,
            row,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_file_into_cache(
    inspection: &TiffInspection,
    file: &InspectedSourceFile,
    expected_generation: &SourceFileGeneration,
    resume_plane: u64,
    cache: &mut CanonicalBaseCache,
    maximum_decoded_chunk_bytes: u64,
    maximum_encoded_chunk_bytes: u64,
    cancellation: &ImportCancellation,
    counters: &mut SourceReadCounters,
    plane_completed: &mut impl FnMut(u64, u64),
) -> Result<(), ImportError> {
    check_cancelled(cancellation)?;
    if source_file_generation(&file.path)? != *expected_generation {
        return Err(ImportError::SourceChanged(file.path.clone()));
    }
    let raw = open_source_file(&file.path, "open source for canonical decode")?;
    if source_file_generation_from_open_file(&file.path, &raw)? != *expected_generation {
        return Err(ImportError::SourceChanged(file.path.clone()));
    }
    let preflight_bytes = preflight_tiff_ifd_chain(&file.path, &raw, cancellation)?;
    if source_file_generation_from_open_file(&file.path, &raw)? != *expected_generation {
        return Err(ImportError::SourceChanged(file.path.clone()));
    }
    counters.tiff_open_count = counters
        .tiff_open_count
        .checked_add(1)
        .ok_or(ImportError::Overflow)?;
    let counting = CountingReader::new(raw);
    let reader = BufReader::with_capacity(HASH_READ_BYTES, counting);
    let decode_limit = usize::try_from(maximum_decoded_chunk_bytes).unwrap_or(usize::MAX);
    let mut limits = Limits::default();
    limits.decoding_buffer_size = decode_limit.max(MAX_OME_XML_BYTES);
    let encoded_limit = usize::try_from(maximum_encoded_chunk_bytes).unwrap_or(usize::MAX);
    limits.intermediate_buffer_size =
        encoded_limit.saturating_add(MAX_ENCODED_CHUNK_OVERHEAD_BYTES);
    limits.ifd_value_size = MAX_OME_XML_BYTES;
    let mut decoder = Decoder::new(reader)
        .map_err(|error| tiff_error(&file.path, error))?
        .with_limits(limits);

    for local_page in 0..file.planes {
        check_cancelled(cancellation)?;
        let global_z = file
            .first_z
            .checked_add(local_page)
            .ok_or(ImportError::Overflow)?;
        let plane = cache.plane_ordinal(file.channel, file.timepoint, global_z)?;
        if plane >= resume_plane {
            if source_file_generation(&file.path)? != *expected_generation {
                return Err(ImportError::SourceChanged(file.path.clone()));
            }
            validate_current_page(
                &file.path,
                &mut decoder,
                inspection.shape.x(),
                inspection.shape.y(),
                inspection.dtype,
            )?;
            decode_page_into_cache(
                &file.path,
                &mut decoder,
                inspection.dtype,
                plane,
                cache,
                maximum_decoded_chunk_bytes,
                cancellation,
                counters,
            )?;
            if source_file_generation(&file.path)? != *expected_generation {
                return Err(ImportError::SourceChanged(file.path.clone()));
            }
            cache.complete_plane(plane)?;
            plane_completed(plane + 1, cache.total_planes());
            if local_page + 1 < file.planes {
                // The next TIFF page decode is an unbounded owner-side
                // interval, so close the completed prefix before entering it.
                cache.commit_pending()?;
            }
        }
        if local_page + 1 < file.planes {
            decoder
                .next_image()
                .map_err(|error| tiff_error(&file.path, error))?;
        }
    }
    let bytes_read = decoder.inner().get_ref().bytes_read;
    if source_file_generation_from_open_file(&file.path, decoder.inner().get_ref().inner())?
        != *expected_generation
    {
        return Err(ImportError::SourceChanged(file.path.clone()));
    }
    if source_file_generation(&file.path)? != *expected_generation {
        return Err(ImportError::SourceChanged(file.path.clone()));
    }
    counters.source_bytes_read = counters
        .source_bytes_read
        .checked_add(bytes_read)
        .and_then(|bytes| bytes.checked_add(preflight_bytes))
        .ok_or(ImportError::Overflow)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_page_into_cache<R: Read + Seek>(
    path: &Path,
    decoder: &mut Decoder<R>,
    dtype: IntensityDType,
    plane: u64,
    cache: &mut CanonicalBaseCache,
    maximum_decoded_chunk_bytes: u64,
    cancellation: &ImportCancellation,
    counters: &mut SourceReadCounters,
) -> Result<(), ImportError> {
    let (image_width, image_height) = decoder
        .dimensions()
        .map_err(|error| tiff_error(path, error))?;
    let (chunk_width, chunk_height) = decoder.chunk_dimensions();
    if chunk_width == 0 || chunk_height == 0 {
        return Err(ImportError::UnsupportedSource(
            "TIFF strip/tile dimensions must be nonzero".to_owned(),
        ));
    }
    let chunks_across = image_width.div_ceil(chunk_width);
    let chunks_down = image_height.div_ceil(chunk_height);
    for chunk_y in 0..chunks_down {
        for chunk_x in 0..chunks_across {
            check_cancelled(cancellation)?;
            let chunk_index = chunk_y
                .checked_mul(chunks_across)
                .and_then(|base| base.checked_add(chunk_x))
                .ok_or(ImportError::Overflow)?;
            let layout = decoder
                .image_chunk_buffer_layout(chunk_index)
                .map_err(|error| tiff_error(path, error))?;
            let required_bytes =
                u64::try_from(layout.complete_len).map_err(|_| ImportError::Overflow)?;
            if required_bytes > maximum_decoded_chunk_bytes {
                return Err(ImportError::ManagedCapacityInsufficient {
                    required_bytes,
                    capacity_bytes: maximum_decoded_chunk_bytes,
                });
            }
            let (data_width, data_height) = decoder.chunk_data_dimensions(chunk_index);
            let decoded = decoder
                .read_chunk(chunk_index)
                .map_err(|error| tiff_error(path, error))?;
            counters.native_chunk_decode_count = counters
                .native_chunk_decode_count
                .checked_add(1)
                .ok_or(ImportError::Overflow)?;
            let actual_bytes = decoded_result_bytes(&decoded)?;
            if actual_bytes > maximum_decoded_chunk_bytes {
                return Err(ImportError::ManagedCapacityInsufficient {
                    required_bytes: actual_bytes,
                    capacity_bytes: maximum_decoded_chunk_bytes,
                });
            }
            counters.decoded_bytes = counters
                .decoded_bytes
                .checked_add(actual_bytes)
                .ok_or(ImportError::Overflow)?;
            let x = u64::from(chunk_x)
                .checked_mul(u64::from(chunk_width))
                .ok_or(ImportError::Overflow)?;
            let y = u64::from(chunk_y)
                .checked_mul(u64::from(chunk_height))
                .ok_or(ImportError::Overflow)?;
            write_decoded_chunk_rows(
                path,
                dtype,
                decoded,
                data_width,
                data_height,
                plane,
                y,
                x,
                cache,
            )?;
            check_cancelled(cancellation)?;
        }
    }
    Ok(())
}

fn decode_page_into_plane<R: Read + Seek>(
    path: &Path,
    decoder: &mut Decoder<R>,
    dtype: IntensityDType,
    destination_le: &mut [u8],
    maximum_decoded_chunk_bytes: u64,
    cancellation: &ImportCancellation,
    counters: &mut SourceReadCounters,
) -> Result<(), ImportError> {
    let (image_width, image_height) = decoder
        .dimensions()
        .map_err(|error| tiff_error(path, error))?;
    let (chunk_width, chunk_height) = decoder.chunk_dimensions();
    if chunk_width == 0 || chunk_height == 0 {
        return Err(ImportError::UnsupportedSource(
            "TIFF strip/tile dimensions must be nonzero".to_owned(),
        ));
    }
    let chunks_across = image_width.div_ceil(chunk_width);
    let chunks_down = image_height.div_ceil(chunk_height);
    for chunk_y in 0..chunks_down {
        for chunk_x in 0..chunks_across {
            check_cancelled(cancellation)?;
            let chunk_index = chunk_y
                .checked_mul(chunks_across)
                .and_then(|base| base.checked_add(chunk_x))
                .ok_or(ImportError::Overflow)?;
            let layout = decoder
                .image_chunk_buffer_layout(chunk_index)
                .map_err(|error| tiff_error(path, error))?;
            let required_bytes =
                u64::try_from(layout.complete_len).map_err(|_| ImportError::Overflow)?;
            if required_bytes > maximum_decoded_chunk_bytes {
                return Err(ImportError::ManagedCapacityInsufficient {
                    required_bytes,
                    capacity_bytes: maximum_decoded_chunk_bytes,
                });
            }
            let (data_width, data_height) = decoder.chunk_data_dimensions(chunk_index);
            let decoded = decoder
                .read_chunk(chunk_index)
                .map_err(|error| tiff_error(path, error))?;
            counters.native_chunk_decode_count = counters
                .native_chunk_decode_count
                .checked_add(1)
                .ok_or(ImportError::Overflow)?;
            let actual_bytes = decoded_result_bytes(&decoded)?;
            if actual_bytes > maximum_decoded_chunk_bytes {
                return Err(ImportError::ManagedCapacityInsufficient {
                    required_bytes: actual_bytes,
                    capacity_bytes: maximum_decoded_chunk_bytes,
                });
            }
            counters.decoded_bytes = counters
                .decoded_bytes
                .checked_add(actual_bytes)
                .ok_or(ImportError::Overflow)?;
            let chunk_x_start = u64::from(chunk_x)
                .checked_mul(u64::from(chunk_width))
                .ok_or(ImportError::Overflow)?;
            let chunk_y_start = u64::from(chunk_y)
                .checked_mul(u64::from(chunk_height))
                .ok_or(ImportError::Overflow)?;
            copy_decoded_chunk_into_plane(
                path,
                dtype,
                decoded,
                data_width,
                data_height,
                chunk_x_start,
                chunk_y_start,
                u64::from(image_width),
                u64::from(image_height),
                destination_le,
            )?;
            check_cancelled(cancellation)?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_decoded_chunk_rows(
    path: &Path,
    dtype: IntensityDType,
    decoded: DecodingResult,
    data_width: u32,
    data_height: u32,
    plane: u64,
    y_start: u64,
    x_start: u64,
    cache: &mut CanonicalBaseCache,
) -> Result<(), ImportError> {
    let expected_samples = usize::try_from(
        u64::from(data_width)
            .checked_mul(u64::from(data_height))
            .ok_or(ImportError::Overflow)?,
    )
    .map_err(|_| ImportError::Overflow)?;
    let row_samples = usize::try_from(data_width).map_err(|_| ImportError::Overflow)?;
    match (dtype, decoded) {
        (IntensityDType::Uint8, DecodingResult::U8(values)) => {
            if values.len() != expected_samples {
                return Err(chunk_size_error(path, values.len(), expected_samples));
            }
            for (row, values) in values.chunks_exact(row_samples).enumerate() {
                cache.write_row(
                    plane,
                    y_start + u64::try_from(row).map_err(|_| ImportError::Overflow)?,
                    x_start,
                    values,
                )?;
            }
        }
        (IntensityDType::Uint16, DecodingResult::U16(values)) => {
            if values.len() != expected_samples {
                return Err(chunk_size_error(path, values.len(), expected_samples));
            }
            let mut row_le = vec![0_u8; row_samples * 2];
            for (row, values) in values.chunks_exact(row_samples).enumerate() {
                for (value, output) in values.iter().zip(row_le.chunks_exact_mut(2)) {
                    output.copy_from_slice(&value.to_le_bytes());
                }
                cache.write_row(
                    plane,
                    y_start + u64::try_from(row).map_err(|_| ImportError::Overflow)?,
                    x_start,
                    &row_le,
                )?;
            }
        }
        (IntensityDType::Float32, DecodingResult::F32(values)) => {
            if values.len() != expected_samples {
                return Err(chunk_size_error(path, values.len(), expected_samples));
            }
            let mut row_le = vec![0_u8; row_samples * 4];
            for (row, values) in values.chunks_exact(row_samples).enumerate() {
                for (value, output) in values.iter().zip(row_le.chunks_exact_mut(4)) {
                    if !value.is_finite() {
                        return Err(ImportError::UnsupportedSource(format!(
                            "TIFF {path:?} contains a non-finite float32 sample"
                        )));
                    }
                    output.copy_from_slice(&value.to_bits().to_le_bytes());
                }
                cache.write_row(
                    plane,
                    y_start + u64::try_from(row).map_err(|_| ImportError::Overflow)?,
                    x_start,
                    &row_le,
                )?;
            }
        }
        _ => {
            return Err(ImportError::UnsupportedSource(format!(
                "TIFF {path:?} decoded to a different sample type than inspection"
            )));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn copy_decoded_chunk_into_plane(
    path: &Path,
    dtype: IntensityDType,
    decoded: DecodingResult,
    data_width: u32,
    data_height: u32,
    chunk_x_start: u64,
    chunk_y_start: u64,
    image_width: u64,
    image_height: u64,
    destination_le: &mut [u8],
) -> Result<(), ImportError> {
    let expected_samples = usize::try_from(
        u64::from(data_width)
            .checked_mul(u64::from(data_height))
            .ok_or(ImportError::Overflow)?,
    )
    .map_err(|_| ImportError::Overflow)?;
    let bytes_per_sample = u64::from(dtype.bytes_per_sample());
    let expected_destination = image_width
        .checked_mul(image_height)
        .and_then(|value| value.checked_mul(bytes_per_sample))
        .ok_or(ImportError::Overflow)?;
    if u64::try_from(destination_le.len()).map_err(|_| ImportError::Overflow)?
        != expected_destination
        || chunk_x_start
            .checked_add(u64::from(data_width))
            .is_none_or(|end| end > image_width)
        || chunk_y_start
            .checked_add(u64::from(data_height))
            .is_none_or(|end| end > image_height)
    {
        return Err(ImportError::InvalidRequest(
            "a decoded TIFF chunk does not fit its canonical plane",
        ));
    }
    let row_samples = usize::try_from(data_width).map_err(|_| ImportError::Overflow)?;
    match (dtype, decoded) {
        (IntensityDType::Uint8, DecodingResult::U8(values)) => {
            if values.len() != expected_samples {
                return Err(chunk_size_error(path, values.len(), expected_samples));
            }
            for (row, values) in values.chunks_exact(row_samples).enumerate() {
                let destination = plane_chunk_row_range(
                    chunk_x_start,
                    chunk_y_start,
                    image_width,
                    row,
                    u64::from(data_width),
                    1,
                )?;
                destination_le[destination].copy_from_slice(values);
            }
        }
        (IntensityDType::Uint16, DecodingResult::U16(values)) => {
            if values.len() != expected_samples {
                return Err(chunk_size_error(path, values.len(), expected_samples));
            }
            for (row, values) in values.chunks_exact(row_samples).enumerate() {
                let destination = plane_chunk_row_range(
                    chunk_x_start,
                    chunk_y_start,
                    image_width,
                    row,
                    u64::from(data_width),
                    2,
                )?;
                for (value, output) in values
                    .iter()
                    .zip(destination_le[destination].chunks_exact_mut(2))
                {
                    output.copy_from_slice(&value.to_le_bytes());
                }
            }
        }
        (IntensityDType::Float32, DecodingResult::F32(values)) => {
            if values.len() != expected_samples {
                return Err(chunk_size_error(path, values.len(), expected_samples));
            }
            if values.iter().any(|value| !value.is_finite()) {
                return Err(ImportError::UnsupportedSource(format!(
                    "TIFF {path:?} contains a non-finite float32 sample"
                )));
            }
            for (row, values) in values.chunks_exact(row_samples).enumerate() {
                let destination = plane_chunk_row_range(
                    chunk_x_start,
                    chunk_y_start,
                    image_width,
                    row,
                    u64::from(data_width),
                    4,
                )?;
                for (value, output) in values
                    .iter()
                    .zip(destination_le[destination].chunks_exact_mut(4))
                {
                    output.copy_from_slice(&value.to_bits().to_le_bytes());
                }
            }
        }
        _ => {
            return Err(ImportError::UnsupportedSource(format!(
                "TIFF {path:?} decoded to a different sample type than inspection"
            )));
        }
    }
    Ok(())
}

fn plane_chunk_row_range(
    chunk_x_start: u64,
    chunk_y_start: u64,
    image_width: u64,
    row: usize,
    row_samples: u64,
    bytes_per_sample: u64,
) -> Result<std::ops::Range<usize>, ImportError> {
    let row = u64::try_from(row).map_err(|_| ImportError::Overflow)?;
    let sample_start = chunk_y_start
        .checked_add(row)
        .and_then(|y| y.checked_mul(image_width))
        .and_then(|value| value.checked_add(chunk_x_start))
        .ok_or(ImportError::Overflow)?;
    let byte_start = sample_start
        .checked_mul(bytes_per_sample)
        .ok_or(ImportError::Overflow)?;
    let byte_end = sample_start
        .checked_add(row_samples)
        .and_then(|value| value.checked_mul(bytes_per_sample))
        .ok_or(ImportError::Overflow)?;
    Ok(
        usize::try_from(byte_start).map_err(|_| ImportError::Overflow)?
            ..usize::try_from(byte_end).map_err(|_| ImportError::Overflow)?,
    )
}

#[derive(Clone, Copy)]
enum TiffByteOrder {
    Little,
    Big,
}

impl TiffByteOrder {
    fn u16(self, bytes: &[u8]) -> u16 {
        let bytes = [bytes[0], bytes[1]];
        match self {
            Self::Little => u16::from_le_bytes(bytes),
            Self::Big => u16::from_be_bytes(bytes),
        }
    }

    fn u32(self, bytes: &[u8]) -> u32 {
        let bytes = [bytes[0], bytes[1], bytes[2], bytes[3]];
        match self {
            Self::Little => u32::from_le_bytes(bytes),
            Self::Big => u32::from_be_bytes(bytes),
        }
    }

    fn u64(self, bytes: &[u8]) -> u64 {
        let bytes = [
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ];
        match self {
            Self::Little => u64::from_le_bytes(bytes),
            Self::Big => u64::from_be_bytes(bytes),
        }
    }
}

#[derive(Clone, Copy)]
struct TiffIfdFormat {
    byte_order: TiffByteOrder,
    big_tiff: bool,
    inline_bytes: u64,
    entry_bytes: u64,
    count_bytes: u64,
    next_bytes: u64,
}

struct PreflightIfd {
    entries_offset: u64,
    entry_count: u64,
    next_ifd: u64,
}

/// Validate the complete primary IFD chain with fixed-size positional reads
/// before constructing `tiff::Decoder`. The pinned decoder parses its first
/// image under a 256 MiB default limit, retains page-history maps, and does not
/// bound the IFD entry count. This pass establishes the tighter importer
/// authority before any of those allocations are reachable.
fn preflight_tiff_ifd_chain(
    path: &Path,
    file: &File,
    cancellation: &ImportCancellation,
) -> Result<u64, ImportError> {
    check_cancelled(cancellation)?;
    let file_bytes = file
        .metadata()
        .map_err(|source| io_error("inspect TIFF preflight length", path, source))?
        .len();
    let mut bytes_read = 0_u64;
    let mut header = [0_u8; 8];
    preflight_read_exact(path, file, 0, &mut header, &mut bytes_read)?;
    let byte_order = match &header[..2] {
        b"II" => TiffByteOrder::Little,
        b"MM" => TiffByteOrder::Big,
        _ => return Err(preflight_error(path, "has an invalid byte-order signature")),
    };
    let (format, first_ifd) = match byte_order.u16(&header[2..4]) {
        42 => (
            TiffIfdFormat {
                byte_order,
                big_tiff: false,
                inline_bytes: 4,
                entry_bytes: 12,
                count_bytes: 2,
                next_bytes: 4,
            },
            u64::from(byte_order.u32(&header[4..8])),
        ),
        43 => {
            if byte_order.u16(&header[4..6]) != 8 || byte_order.u16(&header[6..8]) != 0 {
                return Err(preflight_error(path, "has an invalid BigTIFF header"));
            }
            let mut offset = [0_u8; 8];
            preflight_read_exact(path, file, 8, &mut offset, &mut bytes_read)?;
            (
                TiffIfdFormat {
                    byte_order,
                    big_tiff: true,
                    inline_bytes: 8,
                    entry_bytes: 20,
                    count_bytes: 8,
                    next_bytes: 8,
                },
                byte_order.u64(&offset),
            )
        }
        _ => return Err(preflight_error(path, "has an invalid TIFF version")),
    };
    if first_ifd == 0 {
        return Err(preflight_error(path, "does not contain an image directory"));
    }

    // Brent cycle detection adds no page-proportional memory to the preflight.
    let mut cycle_anchor = first_ifd;
    let mut cycle_power = 1_u64;
    let mut cycle_length = 0_u64;
    let mut current = first_ifd;
    let mut pages = 0_u64;
    loop {
        check_cancelled(cancellation)?;
        pages = pages.checked_add(1).ok_or(ImportError::Overflow)?;
        if pages > MAX_PAGES_PER_FILE {
            return Err(preflight_error(
                path,
                &format!("exceeds the {MAX_PAGES_PER_FILE} page limit"),
            ));
        }
        let ifd = preflight_ifd_header(path, file, file_bytes, format, current, &mut bytes_read)?;
        preflight_ifd_entries(
            path,
            file,
            file_bytes,
            format,
            &ifd,
            cancellation,
            &mut bytes_read,
        )?;
        if ifd.next_ifd == 0 {
            break;
        }

        cycle_length = cycle_length.checked_add(1).ok_or(ImportError::Overflow)?;
        if cycle_anchor == ifd.next_ifd {
            return Err(preflight_error(
                path,
                "contains a cycle in its image directories",
            ));
        }
        if cycle_length == cycle_power {
            cycle_anchor = ifd.next_ifd;
            cycle_power = cycle_power.saturating_mul(2);
            cycle_length = 0;
        }
        current = ifd.next_ifd;
    }
    Ok(bytes_read)
}

fn preflight_ifd_header(
    path: &Path,
    file: &File,
    file_bytes: u64,
    format: TiffIfdFormat,
    ifd_offset: u64,
    bytes_read: &mut u64,
) -> Result<PreflightIfd, ImportError> {
    let mut count = [0_u8; 8];
    let count_len = usize::try_from(format.count_bytes).map_err(|_| ImportError::Overflow)?;
    preflight_read_exact(path, file, ifd_offset, &mut count[..count_len], bytes_read)?;
    let entry_count = if format.big_tiff {
        format.byte_order.u64(&count)
    } else {
        u64::from(format.byte_order.u16(&count[..2]))
    };
    if entry_count > MAX_TIFF_IFD_ENTRIES {
        return Err(preflight_error(
            path,
            &format!(
                "has {entry_count} entries in one image directory; the limit is {MAX_TIFF_IFD_ENTRIES}"
            ),
        ));
    }
    let entries_offset = ifd_offset
        .checked_add(format.count_bytes)
        .ok_or(ImportError::Overflow)?;
    let next_offset = entry_count
        .checked_mul(format.entry_bytes)
        .and_then(|bytes| entries_offset.checked_add(bytes))
        .ok_or(ImportError::Overflow)?;
    let end = next_offset
        .checked_add(format.next_bytes)
        .ok_or(ImportError::Overflow)?;
    if end > file_bytes {
        return Err(preflight_error(path, "has a truncated image directory"));
    }
    let mut next = [0_u8; 8];
    let next_len = usize::try_from(format.next_bytes).map_err(|_| ImportError::Overflow)?;
    preflight_read_exact(path, file, next_offset, &mut next[..next_len], bytes_read)?;
    let next_ifd = if format.big_tiff {
        format.byte_order.u64(&next)
    } else {
        u64::from(format.byte_order.u32(&next[..4]))
    };
    Ok(PreflightIfd {
        entries_offset,
        entry_count,
        next_ifd,
    })
}

#[allow(clippy::too_many_arguments)]
fn preflight_ifd_entries(
    path: &Path,
    file: &File,
    file_bytes: u64,
    format: TiffIfdFormat,
    ifd: &PreflightIfd,
    cancellation: &ImportCancellation,
    bytes_read: &mut u64,
) -> Result<(), ImportError> {
    let mut entry = [0_u8; 20];
    let entry_len = usize::try_from(format.entry_bytes).map_err(|_| ImportError::Overflow)?;
    let mut compression_seen = false;
    let mut strip_offsets = None;
    let mut strip_counts = None;
    let mut tile_offsets = None;
    let mut tile_counts = None;
    let mut eager_tags_seen = 0_u32;
    for index in 0..ifd.entry_count {
        check_cancelled(cancellation)?;
        let offset = index
            .checked_mul(format.entry_bytes)
            .and_then(|bytes| ifd.entries_offset.checked_add(bytes))
            .ok_or(ImportError::Overflow)?;
        preflight_read_exact(path, file, offset, &mut entry[..entry_len], bytes_read)?;
        let tag = format.byte_order.u16(&entry[..2]);
        let type_code = format.byte_order.u16(&entry[2..4]);
        let count = if format.big_tiff {
            format.byte_order.u64(&entry[4..12])
        } else {
            u64::from(format.byte_order.u32(&entry[4..8]))
        };
        let value_offset = if format.big_tiff { 12 } else { 8 };
        let type_bytes = tiff_type_bytes(type_code).ok_or_else(|| {
            preflight_error(path, &format!("uses unknown TIFF field type {type_code}"))
        })?;
        let value_bytes = count.checked_mul(type_bytes).ok_or(ImportError::Overflow)?;
        if value_bytes > MAX_OME_XML_BYTES as u64 {
            return Err(preflight_error(
                path,
                &format!(
                    "has a TIFF field of {value_bytes} bytes; the per-field limit is {MAX_OME_XML_BYTES}"
                ),
            ));
        }
        if let Some(bit) = eager_tag_bit(tag) {
            if eager_tags_seen & bit != 0 {
                return Err(preflight_error(path, "duplicates an eager image field"));
            }
            eager_tags_seen |= bit;
        }

        match tag {
            // The admitted source is single-sample grayscale. Tight bounds on
            // eager vectors protect Decoder::new before caller limits apply.
            258 | 339 => {
                if type_code != 3 || count != 1 {
                    return Err(preflight_error(
                        path,
                        "has an unsupported BitsPerSample or SampleFormat cardinality",
                    ));
                }
            }
            277 => {
                if type_code != 3
                    || count != 1
                    || format
                        .byte_order
                        .u16(&entry[value_offset..value_offset + 2])
                        != 1
                {
                    return Err(preflight_error(
                        path,
                        "must declare exactly one sample per pixel",
                    ));
                }
            }
            338 => {
                return Err(preflight_error(
                    path,
                    "uses ExtraSamples outside the grayscale source contract",
                ));
            }
            530 => {
                if type_code != 3 || count != 2 {
                    return Err(preflight_error(
                        path,
                        "has an unsupported ChromaSubsampling cardinality",
                    ));
                }
            }
            347 => {
                return Err(preflight_error(
                    path,
                    "declares JPEG tables outside the accepted compression contract",
                ));
            }
            256 | 257 | 278 | 322 | 323 => {
                if !matches!(type_code, 3 | 4 | 16) || count != 1 {
                    return Err(preflight_error(
                        path,
                        "has an unsupported image-dimension field cardinality",
                    ));
                }
            }
            262 | 284 | 317 => {
                if type_code != 3 || count != 1 {
                    return Err(preflight_error(
                        path,
                        "has an unsupported eager scalar field cardinality",
                    ));
                }
            }
            259 => {
                if compression_seen || type_code != 3 || count != 1 {
                    return Err(preflight_error(path, "has an invalid Compression field"));
                }
                compression_seen = true;
                let compression = format
                    .byte_order
                    .u16(&entry[value_offset..value_offset + 2]);
                if !supported_tiff_compression(compression) {
                    return Err(preflight_error(
                        path,
                        &format!("uses unsupported compression code {compression}"),
                    ));
                }
            }
            273 | 279 | 324 | 325 => {
                if !matches!(type_code, 3 | 4 | 16)
                    || count == 0
                    || count > MAX_NATIVE_CHUNKS_PER_PAGE as u64
                {
                    return Err(preflight_error(
                        path,
                        "has an unsupported strip/tile table cardinality",
                    ));
                }
                match tag {
                    273 => strip_offsets = Some(count),
                    279 => strip_counts = Some(count),
                    324 => tile_offsets = Some(count),
                    325 => tile_counts = Some(count),
                    _ => unreachable!(),
                }
            }
            _ => {}
        }

        if value_bytes > format.inline_bytes {
            let external_offset = if format.big_tiff {
                format
                    .byte_order
                    .u64(&entry[value_offset..value_offset + 8])
            } else {
                u64::from(
                    format
                        .byte_order
                        .u32(&entry[value_offset..value_offset + 4]),
                )
            };
            let external_end = external_offset
                .checked_add(value_bytes)
                .ok_or(ImportError::Overflow)?;
            if external_end > file_bytes {
                return Err(preflight_error(
                    path,
                    "has a TIFF field outside the source file",
                ));
            }
        }
    }

    let valid_chunks = match (strip_offsets, strip_counts, tile_offsets, tile_counts) {
        (Some(offsets), Some(counts), None, None) | (None, None, Some(offsets), Some(counts)) => {
            offsets == counts
        }
        _ => false,
    };
    if !valid_chunks {
        return Err(preflight_error(
            path,
            "must declare one matching strip or tile offset/count table pair",
        ));
    }
    Ok(())
}

const fn eager_tag_bit(tag: u16) -> Option<u32> {
    match tag {
        256 => Some(1 << 0),
        257 => Some(1 << 1),
        258 => Some(1 << 2),
        259 => Some(1 << 3),
        262 => Some(1 << 4),
        273 => Some(1 << 5),
        277 => Some(1 << 6),
        278 => Some(1 << 7),
        279 => Some(1 << 8),
        284 => Some(1 << 9),
        317 => Some(1 << 10),
        322 => Some(1 << 11),
        323 => Some(1 << 12),
        324 => Some(1 << 13),
        325 => Some(1 << 14),
        338 => Some(1 << 15),
        339 => Some(1 << 16),
        347 => Some(1 << 17),
        530 => Some(1 << 18),
        _ => None,
    }
}

const fn tiff_type_bytes(type_code: u16) -> Option<u64> {
    match type_code {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 | 13 => Some(4),
        5 | 10 | 12 | 16 | 17 | 18 => Some(8),
        _ => None,
    }
}

fn preflight_read_exact(
    path: &Path,
    file: &File,
    offset: u64,
    destination: &mut [u8],
    bytes_read: &mut u64,
) -> Result<(), ImportError> {
    file.read_exact_at(destination, offset)
        .map_err(|source| io_error("read TIFF preflight metadata", path, source))?;
    *bytes_read = bytes_read
        .checked_add(u64::try_from(destination.len()).map_err(|_| ImportError::Overflow)?)
        .ok_or(ImportError::Overflow)?;
    Ok(())
}

fn preflight_error(path: &Path, detail: &str) -> ImportError {
    ImportError::UnsupportedSource(format!("TIFF {path:?} {detail}"))
}

fn inspect_file(
    path: &Path,
    cancellation: &ImportCancellation,
) -> Result<TiffFileFacts, ImportError> {
    check_cancelled(cancellation)?;
    ensure_regular_tiff_file(path)?;
    let before = source_file_generation(path)?;
    let raw = open_source_file(path, "open source for inspection")?;
    if source_file_generation_from_open_file(path, &raw)? != before {
        return Err(ImportError::SourceChanged(path.to_path_buf()));
    }
    preflight_tiff_ifd_chain(path, &raw, cancellation)?;
    if source_file_generation_from_open_file(path, &raw)? != before {
        return Err(ImportError::SourceChanged(path.to_path_buf()));
    }
    let reader = BufReader::with_capacity(HASH_READ_BYTES, raw);
    let mut limits = Limits::default();
    limits.decoding_buffer_size = MAX_OME_XML_BYTES;
    limits.intermediate_buffer_size = MAX_OME_XML_BYTES;
    limits.ifd_value_size = MAX_OME_XML_BYTES;
    let mut decoder = Decoder::new(reader)
        .map_err(|error| tiff_error(path, error))?
        .with_limits(limits);
    let mut ome_pixels = None;
    let mut expected = None;
    let mut pages = 0_u64;
    let mut maximum_decoded_chunk_bytes = 0_u64;
    let mut maximum_encoded_chunk_bytes = 0_u64;
    loop {
        check_cancelled(cancellation)?;
        pages = pages.checked_add(1).ok_or(ImportError::Overflow)?;
        if pages > MAX_PAGES_PER_FILE {
            return Err(ImportError::UnsupportedSource(format!(
                "TIFF {path:?} exceeds the {MAX_PAGES_PER_FILE} page inspection limit"
            )));
        }
        if let Some(page_ome_pixels) = read_ome_pixels(path, &mut decoder)? {
            if let Some(expected_ome_pixels) = &ome_pixels {
                if expected_ome_pixels != &page_ome_pixels {
                    return Err(ImportError::UnsupportedSource(format!(
                        "TIFF {path:?} has conflicting OME Pixels metadata between pages"
                    )));
                }
            } else {
                ome_pixels = Some(page_ome_pixels);
            }
        }
        let (width, height) = decoder
            .dimensions()
            .map_err(|error| tiff_error(path, error))?;
        if width == 0 || height == 0 {
            return Err(ImportError::UnsupportedSource(format!(
                "TIFF {path:?} has a zero image dimension"
            )));
        }
        let dtype = current_page_dtype(path, &mut decoder)?;
        inspect_supported_compression(path, &mut decoder)?;
        let page_facts = (u64::from(width), u64::from(height), dtype);
        if let Some(expected) = expected {
            if page_facts != expected {
                return Err(ImportError::UnsupportedSource(format!(
                    "TIFF {path:?} changes dimensions or dtype between pages"
                )));
            }
        } else {
            expected = Some(page_facts);
        }
        let (chunk_width, chunk_height) = decoder.chunk_dimensions();
        if chunk_width == 0 || chunk_height == 0 {
            return Err(ImportError::UnsupportedSource(format!(
                "TIFF {path:?} has a zero strip/tile dimension"
            )));
        }
        let maximum_chunk = u64::from(chunk_width)
            .checked_mul(u64::from(chunk_height))
            .and_then(|value| value.checked_mul(u64::from(dtype.bytes_per_sample())))
            .ok_or(ImportError::Overflow)?;
        maximum_decoded_chunk_bytes = maximum_decoded_chunk_bytes.max(maximum_chunk);
        let encoded_chunk_bytes = current_page_encoded_chunk_bytes(path, &mut decoder)?;
        maximum_encoded_chunk_bytes = maximum_encoded_chunk_bytes.max(encoded_chunk_bytes);
        check_cancelled(cancellation)?;
        if decoder.more_images() {
            decoder
                .next_image()
                .map_err(|error| tiff_error(path, error))?;
        } else {
            break;
        }
    }
    check_cancelled(cancellation)?;
    let opened_after = source_file_generation_from_open_file(path, decoder.inner().get_ref())?;
    let path_after = source_file_generation(path)?;
    if before != opened_after || before != path_after {
        return Err(ImportError::SourceChanged(path.to_path_buf()));
    }
    let (width, height, dtype) = expected.expect("a TIFF decoder always exposes one image");
    let ome_spacing_zyx_um = match ome_pixels {
        Some(ome_pixels) => {
            validate_ome_pixels(path, &ome_pixels, width, height, pages, dtype)?;
            ome_pixels.spacing_zyx_um
        }
        None => None,
    };
    Ok(TiffFileFacts {
        width,
        height,
        pages,
        dtype,
        ome_spacing_zyx_um,
        maximum_decoded_chunk_bytes,
        maximum_encoded_chunk_bytes,
        bytes: before.bytes,
        generation: before,
    })
}

fn current_page_dtype<R: Read + Seek>(
    path: &Path,
    decoder: &mut Decoder<R>,
) -> Result<IntensityDType, ImportError> {
    let color = decoder
        .colortype()
        .map_err(|error| tiff_error(path, error))?;
    let layout = decoder
        .image_chunk_buffer_layout(0)
        .map_err(|error| tiff_error(path, error))?;
    match (color, layout.sample_format) {
        (ColorType::Gray(8), SampleFormat::Uint) => Ok(IntensityDType::Uint8),
        (ColorType::Gray(16), SampleFormat::Uint) => Ok(IntensityDType::Uint16),
        (ColorType::Gray(32), SampleFormat::IEEEFP) => Ok(IntensityDType::Float32),
        _ => Err(ImportError::UnsupportedSource(format!(
            "TIFF {path:?} is not grayscale uint8, uint16, or float32"
        ))),
    }
}

fn inspect_supported_compression<R: Read + Seek>(
    path: &Path,
    decoder: &mut Decoder<R>,
) -> Result<(), ImportError> {
    let compression = decoder
        .find_tag_unsigned::<u16>(Tag::Compression)
        .map_err(|error| tiff_error(path, error))?
        .unwrap_or(1);
    // None, LZW, Deflate, old Deflate, and PackBits are streaming in the
    // pinned TIFF decoder. Other enabled decoder paths have distinct,
    // unaudited whole-chunk workspaces and are deliberately outside the
    // importer contract.
    if !supported_tiff_compression(compression) {
        return Err(ImportError::UnsupportedSource(format!(
            "TIFF {path:?} uses unsupported compression code {compression}; accepted codes are uncompressed, LZW, Deflate, old Deflate, and PackBits"
        )));
    }
    Ok(())
}

const fn supported_tiff_compression(compression: u16) -> bool {
    matches!(compression, 1 | 5 | 8 | 0x80b2 | 0x8005)
}

fn current_page_encoded_chunk_bytes<R: Read + Seek>(
    path: &Path,
    decoder: &mut Decoder<R>,
) -> Result<u64, ImportError> {
    let strip = decoder
        .find_tag_unsigned_vec::<u64>(Tag::StripByteCounts)
        .map_err(|error| tiff_error(path, error))?;
    let tile = decoder
        .find_tag_unsigned_vec::<u64>(Tag::TileByteCounts)
        .map_err(|error| tiff_error(path, error))?;
    let counts = match (strip, tile) {
        (Some(counts), None) | (None, Some(counts)) => counts,
        _ => {
            return Err(ImportError::UnsupportedSource(format!(
                "TIFF {path:?} must declare exactly one strip or tile byte-count table"
            )));
        }
    };
    if counts.is_empty() || counts.len() > MAX_NATIVE_CHUNKS_PER_PAGE {
        return Err(ImportError::UnsupportedSource(format!(
            "TIFF {path:?} has an unsupported native chunk count"
        )));
    }
    counts.into_iter().try_fold(0_u64, |maximum, bytes| {
        if bytes == 0 || usize::try_from(bytes).is_err() {
            return Err(ImportError::UnsupportedSource(format!(
                "TIFF {path:?} has an invalid encoded native chunk length"
            )));
        }
        Ok(maximum.max(bytes))
    })
}

fn validate_current_page<R: Read + Seek>(
    path: &Path,
    decoder: &mut Decoder<R>,
    expected_width: u64,
    expected_height: u64,
    expected_dtype: IntensityDType,
) -> Result<(), ImportError> {
    let (width, height) = decoder
        .dimensions()
        .map_err(|error| tiff_error(path, error))?;
    let dtype = current_page_dtype(path, decoder)?;
    if u64::from(width) != expected_width
        || u64::from(height) != expected_height
        || dtype != expected_dtype
    {
        return Err(ImportError::SourceChanged(path.to_path_buf()));
    }
    Ok(())
}

fn enumerate_accepted_layout_files(
    inspection: &TiffInspection,
    cancellation: &ImportCancellation,
) -> Result<Vec<(String, PathBuf)>, ImportError> {
    check_cancelled(cancellation)?;
    let mut files = Vec::with_capacity(inspection.files.len());
    for (channel_index, channel) in inspection.source.channels().iter().enumerate() {
        let paths =
            explicit_channel_inventory(channel, cancellation).map_err(|error| match error {
                ImportError::Cancelled => ImportError::Cancelled,
                _ => ImportError::SourceChanged(channel.path().to_path_buf()),
            })?;
        let channel = u32::try_from(channel_index).map_err(|_| ImportError::Overflow)?;
        for path in paths {
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| ImportError::SourceChanged(path.clone()))?;
            files.push((format!("channel-{channel:08}/{file_name}"), path));
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn ensure_regular_tiff_file(path: &Path) -> Result<(), ImportError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("inspect TIFF source", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || !is_tiff_path(path) {
        return Err(ImportError::UnsupportedSource(format!(
            "source {path:?} must be a regular .tif or .tiff file"
        )));
    }
    Ok(())
}

fn is_tiff_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("tif") || extension.eq_ignore_ascii_case("tiff")
        })
}

#[derive(Default)]
struct SpacingAccumulator {
    complete: Option<[f64; 3]>,
    saw_complete: bool,
    saw_missing: bool,
}

impl SpacingAccumulator {
    fn push(&mut self, relative_name: &str, spacing: Option<[f64; 3]>) -> Result<(), ImportError> {
        match spacing {
            Some(spacing) => {
                self.saw_complete = true;
                if let Some(expected) = self.complete {
                    if !spacing_equal(expected, spacing) {
                        return Err(ImportError::UnsupportedSource(format!(
                            "OME physical spacing in {relative_name:?} conflicts with the source group"
                        )));
                    }
                } else {
                    self.complete = Some(spacing);
                }
            }
            None => self.saw_missing = true,
        }
        Ok(())
    }

    fn finish(self) -> Result<Option<[f64; 3]>, ImportError> {
        if self.saw_complete && self.saw_missing {
            return Err(ImportError::UnsupportedSource(
                "OME physical spacing is present in only part of the source group".to_owned(),
            ));
        }
        Ok(self.complete)
    }
}

fn spacing_equal(left: [f64; 3], right: [f64; 3]) -> bool {
    left.into_iter()
        .zip(right)
        .all(|(left, right)| left.to_bits() == right.to_bits())
}

fn read_ome_pixels<R: Read + Seek>(
    path: &Path,
    decoder: &mut Decoder<R>,
) -> Result<Option<OmePixelsFacts>, ImportError> {
    let description = match decoder.get_tag_ascii_string(Tag::ImageDescription) {
        Ok(description) => description,
        Err(TiffError::FormatError(TiffFormatError::RequiredTagNotFound(
            Tag::ImageDescription,
        ))) => return Ok(None),
        Err(error) => return Err(tiff_error(path, error)),
    };
    parse_ome_pixels(&description).map_err(|reason| {
        ImportError::UnsupportedSource(format!(
            "invalid OME Pixels metadata in TIFF {path:?}: {reason}"
        ))
    })
}

fn parse_ome_pixels(description: &str) -> Result<Option<OmePixelsFacts>, &'static str> {
    if description.len() > MAX_OME_XML_BYTES {
        return Err("ImageDescription exceeds the bounded OME XML limit");
    }
    let mut reader = XmlReader::from_str(description);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut events = 0_usize;
    let mut depth = 0_usize;
    let mut attribute_bytes = 0_usize;
    let mut root_seen = false;
    let mut ome_root = false;
    let mut pixels = None;
    let mut pixels_depth = None;
    let mut tiff_data_depth = None;

    loop {
        let event = match reader.read_event_into(&mut buffer) {
            Ok(event) => event,
            Err(_) if !ome_root => return Ok(None),
            Err(_) => return Err("malformed OME XML"),
        };
        if !matches!(event, XmlEvent::Eof) {
            events = events
                .checked_add(1)
                .ok_or("OME XML event count overflow")?;
            if events > MAX_OME_XML_EVENTS {
                return Err("OME XML has too many events");
            }
        }
        match event {
            XmlEvent::Start(element) => {
                depth = depth.checked_add(1).ok_or("OME XML depth overflow")?;
                if depth > MAX_OME_XML_DEPTH {
                    return Err("OME XML is too deeply nested");
                }
                account_attributes(&element, &mut attribute_bytes)?;
                if !root_seen {
                    root_seen = true;
                    ome_root = element.local_name().as_ref() == b"OME";
                    if !ome_root {
                        return Ok(None);
                    }
                } else if ome_root {
                    match element.local_name().as_ref() {
                        b"Pixels" => {
                            if pixels.is_some() {
                                return Err("OME must contain exactly one Pixels element");
                            }
                            pixels = Some(parse_pixels_facts(&element)?);
                            pixels_depth = Some(depth);
                        }
                        b"TiffData" => {
                            if pixels_depth.is_none() {
                                return Err("OME TiffData must be inside Pixels");
                            }
                            let pixels = pixels
                                .as_mut()
                                .expect("an open Pixels element has parsed facts");
                            if pixels.tiff_data.is_some() {
                                return Err("OME must not contain multiple TiffData mappings");
                            }
                            pixels.tiff_data = Some(parse_tiff_data_facts(&element)?);
                            tiff_data_depth = Some(depth);
                        }
                        b"UUID" if tiff_data_depth.is_some() => {
                            return Err("OME TiffData must not reference another TIFF file");
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::Empty(element) => {
                account_attributes(&element, &mut attribute_bytes)?;
                if !root_seen {
                    root_seen = true;
                    ome_root = element.local_name().as_ref() == b"OME";
                    if !ome_root {
                        return Ok(None);
                    }
                } else if ome_root {
                    match element.local_name().as_ref() {
                        b"Pixels" => {
                            if pixels.is_some() {
                                return Err("OME must contain exactly one Pixels element");
                            }
                            pixels = Some(parse_pixels_facts(&element)?);
                        }
                        b"TiffData" => {
                            if pixels_depth.is_none() {
                                return Err("OME TiffData must be inside Pixels");
                            }
                            let pixels = pixels
                                .as_mut()
                                .expect("an open Pixels element has parsed facts");
                            if pixels.tiff_data.is_some() {
                                return Err("OME must not contain multiple TiffData mappings");
                            }
                            pixels.tiff_data = Some(parse_tiff_data_facts(&element)?);
                        }
                        b"UUID" if tiff_data_depth.is_some() => {
                            return Err("OME TiffData must not reference another TIFF file");
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::End(_) => {
                if tiff_data_depth == Some(depth) {
                    tiff_data_depth = None;
                }
                if pixels_depth == Some(depth) {
                    pixels_depth = None;
                }
                depth = depth.checked_sub(1).ok_or("malformed OME XML depth")?;
            }
            XmlEvent::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !ome_root {
        return Ok(None);
    }
    pixels
        .map(Some)
        .ok_or("OME must contain exactly one Pixels element")
}

fn account_attributes(
    element: &BytesStart<'_>,
    total_attribute_bytes: &mut usize,
) -> Result<(), &'static str> {
    let mut count = 0_usize;
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|_| "invalid or duplicate OME XML attribute")?;
        count = count
            .checked_add(1)
            .ok_or("OME XML attribute count overflow")?;
        if count > MAX_OME_XML_ATTRIBUTES {
            return Err("OME XML element has too many attributes");
        }
        *total_attribute_bytes = total_attribute_bytes
            .checked_add(attribute.key.as_ref().len())
            .and_then(|value| value.checked_add(attribute.value.as_ref().len()))
            .ok_or("OME XML attribute bytes overflow")?;
        if *total_attribute_bytes > MAX_OME_XML_ATTRIBUTE_BYTES {
            return Err("OME XML has too many attribute bytes");
        }
    }
    Ok(())
}

fn parse_pixels_facts(element: &BytesStart<'_>) -> Result<OmePixelsFacts, &'static str> {
    let mut sizes = [None; 5];
    let mut dimension_order = None;
    let mut dtype = None;
    for attribute in element.attributes().with_checks(false) {
        let attribute = attribute.map_err(|_| "invalid OME Pixels attribute")?;
        if attribute.value.as_ref().len() > MAX_OME_XML_VALUE_BYTES {
            return Err("OME Pixels value is too long");
        }
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, element.decoder())
            .map_err(|_| "OME Pixels value is not valid XML text")?;
        match attribute.key.local_name().as_ref() {
            b"SizeX" | b"SizeY" | b"SizeZ" | b"SizeC" | b"SizeT" => {
                let index = match attribute.key.local_name().as_ref() {
                    b"SizeX" => 0,
                    b"SizeY" => 1,
                    b"SizeZ" => 2,
                    b"SizeC" => 3,
                    b"SizeT" => 4,
                    _ => unreachable!("outer match admits only size attributes"),
                };
                let parsed = value
                    .parse::<u64>()
                    .map_err(|_| "OME Pixels dimensions must be positive integers")?;
                if parsed == 0 {
                    return Err("OME Pixels dimensions must be positive integers");
                }
                if sizes[index].replace(parsed).is_some() {
                    return Err("duplicate OME Pixels dimension");
                }
            }
            b"DimensionOrder" => {
                if dimension_order.replace(value.into_owned()).is_some() {
                    return Err("duplicate OME Pixels DimensionOrder");
                }
            }
            b"Type" => {
                let parsed = match value.as_ref() {
                    "uint8" => IntensityDType::Uint8,
                    "uint16" => IntensityDType::Uint16,
                    "float" => IntensityDType::Float32,
                    _ => return Err("OME Pixels Type must be uint8, uint16, or float"),
                };
                if dtype.replace(parsed).is_some() {
                    return Err("duplicate OME Pixels Type");
                }
            }
            _ => {}
        }
    }

    if dimension_order.as_deref() != Some("XYZCT") {
        return Err("OME Pixels DimensionOrder must be XYZCT");
    }
    Ok(OmePixelsFacts {
        size_x: sizes[0].ok_or("OME Pixels SizeX is required")?,
        size_y: sizes[1].ok_or("OME Pixels SizeY is required")?,
        size_z: sizes[2].ok_or("OME Pixels SizeZ is required")?,
        size_c: sizes[3].ok_or("OME Pixels SizeC is required")?,
        size_t: sizes[4].ok_or("OME Pixels SizeT is required")?,
        dtype: dtype.ok_or("OME Pixels Type is required")?,
        spacing_zyx_um: parse_pixels_spacing(element)?,
        tiff_data: None,
    })
}

fn parse_pixels_spacing(element: &BytesStart<'_>) -> Result<Option<[f64; 3]>, &'static str> {
    let mut sizes = [None; 3];
    let mut units: [Option<String>; 3] = [None, None, None];
    let mut relevant = 0_usize;
    for attribute in element.attributes().with_checks(false) {
        let attribute = attribute.map_err(|_| "invalid OME Pixels attribute")?;
        let index = match attribute.key.local_name().as_ref() {
            b"PhysicalSizeZ" | b"PhysicalSizeZUnit" => 0,
            b"PhysicalSizeY" | b"PhysicalSizeYUnit" => 1,
            b"PhysicalSizeX" | b"PhysicalSizeXUnit" => 2,
            _ => continue,
        };
        relevant += 1;
        if attribute.value.as_ref().len() > MAX_OME_XML_VALUE_BYTES {
            return Err("OME physical spacing value is too long");
        }
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, element.decoder())
            .map_err(|_| "OME physical spacing value is not valid XML text")?;
        if attribute.key.local_name().as_ref().ends_with(b"Unit") {
            if units[index].replace(value.into_owned()).is_some() {
                return Err("duplicate OME physical spacing unit");
            }
        } else {
            let parsed = value
                .parse::<f64>()
                .map_err(|_| "OME physical spacing is not a finite decimal")?;
            if sizes[index].replace(parsed).is_some() {
                return Err("duplicate OME physical spacing value");
            }
        }
    }
    if relevant == 0 {
        return Ok(None);
    }
    let mut spacing = [0.0; 3];
    for axis in 0..3 {
        let value = sizes[axis].ok_or("OME physical spacing is incomplete")?;
        let unit = units[axis]
            .as_deref()
            .ok_or("OME physical spacing units are incomplete")?;
        spacing[axis] = physical_size_to_um(value, unit)
            .ok_or("OME physical spacing has an unsupported unit or value")?;
    }
    Ok(Some(spacing))
}

fn parse_tiff_data_facts(element: &BytesStart<'_>) -> Result<OmeTiffDataFacts, &'static str> {
    let mut values = [None; 5];
    for attribute in element.attributes().with_checks(false) {
        let attribute = attribute.map_err(|_| "invalid OME TiffData attribute")?;
        let index = match attribute.key.local_name().as_ref() {
            b"IFD" => 0,
            b"FirstZ" => 1,
            b"FirstC" => 2,
            b"FirstT" => 3,
            b"PlaneCount" => 4,
            _ => continue,
        };
        if attribute.value.as_ref().len() > MAX_OME_XML_VALUE_BYTES {
            return Err("OME TiffData value is too long");
        }
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, element.decoder())
            .map_err(|_| "OME TiffData value is not valid XML text")?;
        let parsed = value
            .parse::<u64>()
            .map_err(|_| "OME TiffData values must be non-negative integers")?;
        if values[index].replace(parsed).is_some() {
            return Err("duplicate OME TiffData value");
        }
    }
    Ok(OmeTiffDataFacts {
        ifd: values[0],
        first_z: values[1],
        first_c: values[2],
        first_t: values[3],
        plane_count: values[4],
    })
}

fn validate_ome_pixels(
    path: &Path,
    pixels: &OmePixelsFacts,
    decoded_width: u64,
    decoded_height: u64,
    decoded_pages: u64,
    decoded_dtype: IntensityDType,
) -> Result<(), ImportError> {
    let invalid = |reason: &str| {
        ImportError::UnsupportedSource(format!("OME Pixels metadata in TIFF {path:?} {reason}"))
    };
    if pixels.size_x != decoded_width || pixels.size_y != decoded_height {
        return Err(invalid("does not match the decoded SizeX and SizeY"));
    }
    if pixels.size_c != 1 {
        return Err(invalid("must declare SizeC=1"));
    }
    if pixels.size_t != 1 {
        return Err(invalid("must declare SizeT=1"));
    }
    if pixels.size_z != decoded_pages {
        return Err(invalid("must declare SizeZ equal to the TIFF page count"));
    }
    if pixels.dtype != decoded_dtype {
        return Err(invalid(
            "declares a Type that does not match the decoded TIFF dtype",
        ));
    }
    if let Some(tiff_data) = &pixels.tiff_data
        && (tiff_data.ifd.unwrap_or(0) != 0
            || tiff_data.first_z.unwrap_or(0) != 0
            || tiff_data.first_c.unwrap_or(0) != 0
            || tiff_data.first_t.unwrap_or(0) != 0
            || tiff_data.plane_count != Some(decoded_pages))
    {
        return Err(invalid(
            "uses a non-sequential or incomplete TiffData mapping",
        ));
    }
    Ok(())
}

fn physical_size_to_um(value: f64, unit: &str) -> Option<f64> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let normalized = unit.trim().to_ascii_lowercase();
    let factor = match normalized.as_str() {
        "um" | "micrometer" | "micrometre" | "micrometers" | "micrometres" | "micron"
        | "microns" => 1.0,
        "nm" | "nanometer" | "nanometre" | "nanometers" | "nanometres" => 0.001,
        "mm" | "millimeter" | "millimetre" | "millimeters" | "millimetres" => 1000.0,
        "m" | "meter" | "metre" | "meters" | "metres" => 1_000_000.0,
        _ if unit.trim() == "\u{00b5}m" || unit.trim() == "\u{03bc}m" => 1.0,
        _ => return None,
    };
    let converted = value * factor;
    (converted.is_finite() && converted > 0.0).then_some(converted)
}

#[allow(clippy::too_many_arguments)]
fn aggregate_fingerprint(
    source: &TiffSource,
    shape: Shape4D,
    dtype: IntensityDType,
    ome_spacing_zyx_um: Option<[f64; 3]>,
    source_bytes: u64,
    files: &[InspectedSourceFile],
) -> Result<Sha256Digest, ImportError> {
    let mut ordered = files.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        left.channel
            .cmp(&right.channel)
            .then(left.timepoint.cmp(&right.timepoint))
            .then(left.first_z.cmp(&right.first_z))
            .then(left.relative_name.cmp(&right.relative_name))
    });
    let mut hasher = Sha256Hasher::new();
    hasher.update(b"mirante4d-import-structural-source-manifest-v2\0");
    for dimension in shape.dimensions() {
        hasher.update(dimension.to_le_bytes());
    }
    hasher.update(
        u32::try_from(source.channels().len())
            .map_err(|_| ImportError::Overflow)?
            .to_le_bytes(),
    );
    for channel in source.channels() {
        let label = channel.label().as_bytes();
        hasher.update(
            u64::try_from(label.len())
                .map_err(|_| ImportError::Overflow)?
                .to_le_bytes(),
        );
        hasher.update(label);
        hasher.update([match channel.kind() {
            TiffChannelSourceKind::Single3dTiff => 1,
            TiffChannelSourceKind::FolderOf3dTiffs => 2,
            TiffChannelSourceKind::FolderOf2dTiffs => 3,
        }]);
    }
    hasher.update([match dtype {
        IntensityDType::Uint8 => 1,
        IntensityDType::Uint16 => 2,
        IntensityDType::Float32 => 3,
    }]);
    match ome_spacing_zyx_um {
        Some(spacing) => {
            hasher.update([1]);
            for value in spacing {
                hasher.update(value.to_bits().to_le_bytes());
            }
        }
        None => hasher.update([0]),
    }
    hasher.update(source_bytes.to_le_bytes());
    hasher.update(
        u64::try_from(ordered.len())
            .map_err(|_| ImportError::Overflow)?
            .to_le_bytes(),
    );
    for file in ordered {
        let name = file.relative_name.as_bytes();
        hasher.update(
            u64::try_from(name.len())
                .map_err(|_| ImportError::Overflow)?
                .to_le_bytes(),
        );
        hasher.update(name);
        hasher.update(file.channel.to_le_bytes());
        hasher.update(file.timepoint.to_le_bytes());
        hasher.update(file.first_z.to_le_bytes());
        hasher.update(file.planes.to_le_bytes());
        hasher.update(file.bytes.to_le_bytes());
        hasher.update(file.generation.device.to_le_bytes());
        hasher.update(file.generation.inode.to_le_bytes());
        hasher.update(file.generation.modified_seconds.to_le_bytes());
        hasher.update(file.generation.modified_nanoseconds.to_le_bytes());
        hasher.update(file.generation.changed_seconds.to_le_bytes());
        hasher.update(file.generation.changed_nanoseconds.to_le_bytes());
    }
    Ok(hasher.finalize())
}

fn decoded_result_bytes(decoded: &DecodingResult) -> Result<u64, ImportError> {
    let (samples, width) = match decoded {
        DecodingResult::U8(values) => (values.len(), 1_u64),
        DecodingResult::U16(values) => (values.len(), 2),
        DecodingResult::U32(values) => (values.len(), 4),
        DecodingResult::U64(values) => (values.len(), 8),
        DecodingResult::F16(values) => (values.len(), 2),
        DecodingResult::F32(values) => (values.len(), 4),
        DecodingResult::F64(values) => (values.len(), 8),
        DecodingResult::I8(values) => (values.len(), 1),
        DecodingResult::I16(values) => (values.len(), 2),
        DecodingResult::I32(values) => (values.len(), 4),
        DecodingResult::I64(values) => (values.len(), 8),
    };
    u64::try_from(samples)
        .map_err(|_| ImportError::Overflow)?
        .checked_mul(width)
        .ok_or(ImportError::Overflow)
}

fn shape4d(t: u64, z: u64, y: u64, x: u64) -> Result<Shape4D, ImportError> {
    Shape4D::new(t, z, y, x).map_err(|_| ImportError::Overflow)
}

fn check_cancelled(cancellation: &ImportCancellation) -> Result<(), ImportError> {
    if cancellation.is_cancelled() {
        Err(ImportError::Cancelled)
    } else {
        Ok(())
    }
}

fn tiff_error(path: &Path, error: TiffError) -> ImportError {
    ImportError::Tiff {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

fn chunk_size_error(path: &Path, actual: usize, expected: usize) -> ImportError {
    ImportError::Tiff {
        path: path.to_path_buf(),
        message: format!("decoded TIFF chunk has {actual} samples, expected {expected}"),
    }
}

fn source_file_generation(path: &Path) -> Result<SourceFileGeneration, ImportError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("inspect source generation", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ImportError::SourceChanged(path.to_path_buf()));
    }
    Ok(source_file_generation_from_metadata(&metadata))
}

fn open_source_file(path: &Path, operation: &'static str) -> Result<File, ImportError> {
    OpenOptions::new()
        .read(true)
        .custom_flags(
            i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits())
                .expect("O_NOFOLLOW is representable as a platform open flag"),
        )
        .open(path)
        .map_err(|source| io_error(operation, path, source))
}

fn source_file_generation_from_open_file(
    path: &Path,
    file: &File,
) -> Result<SourceFileGeneration, ImportError> {
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect opened source generation", path, source))?;
    if !metadata.is_file() {
        return Err(ImportError::SourceChanged(path.to_path_buf()));
    }
    Ok(source_file_generation_from_metadata(&metadata))
}

fn source_file_generation_from_metadata(metadata: &fs::Metadata) -> SourceFileGeneration {
    SourceFileGeneration {
        device: metadata.dev(),
        inode: metadata.ino(),
        bytes: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    }
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> ImportError {
    ImportError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

struct CountingReader<R> {
    inner: R,
    bytes_read: u64,
}

impl<R> CountingReader<R> {
    const fn new(inner: R) -> Self {
        Self {
            inner,
            bytes_read: 0,
        }
    }

    const fn inner(&self) -> &R {
        &self.inner
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.bytes_read = self
            .bytes_read
            .checked_add(read as u64)
            .ok_or_else(|| std::io::Error::other("source read counter overflow"))?;
        Ok(read)
    }
}

impl<R: Seek> Seek for CountingReader<R> {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(position)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File, OpenOptions},
        io::Write,
        path::Path,
        sync::{Arc, Mutex},
    };

    use mirante4d_dataset::{CpuByteLease, CpuLedgerCategory, CpuLedgerError};
    use tempfile::tempdir;
    use tiff::{
        encoder::{Compression, DeflateLevel, TiffEncoder, colortype},
        tags::Tag,
    };

    use super::*;

    struct TestLease(u64);

    impl CpuByteLease for TestLease {
        fn category(&self) -> CpuLedgerCategory {
            CpuLedgerCategory::ImportWorkingSet
        }

        fn reserved_bytes(&self) -> u64 {
            self.0
        }
    }

    struct TestLedger;

    impl CpuByteLedger for TestLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            assert_eq!(category, CpuLedgerCategory::ImportWorkingSet);
            Ok(Box::new(TestLease(bytes)))
        }
    }

    fn encode_detector_plane(dtype: IntensityDType, values: &[u32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| match dtype {
                IntensityDType::Uint8 => vec![*value as u8],
                IntensityDType::Uint16 => (*value as u16).to_le_bytes().to_vec(),
                IntensityDType::Float32 => value.to_le_bytes().to_vec(),
            })
            .collect()
    }

    fn naive_uniform_cube(volume: &[u32], shape: [usize; 3]) -> Option<u32> {
        if shape.iter().any(|dimension| *dimension < 5) {
            return None;
        }
        for z in 0..=shape[0] - 5 {
            for y in 0..=shape[1] - 5 {
                for x in 0..=shape[2] - 5 {
                    let expected = volume[(z * shape[1] + y) * shape[2] + x];
                    let uniform = (z..z + 5).all(|zz| {
                        (y..y + 5).all(|yy| {
                            (x..x + 5)
                                .all(|xx| volume[(zz * shape[1] + yy) * shape[2] + xx] == expected)
                        })
                    });
                    if uniform {
                        return Some(expected);
                    }
                }
            }
        }
        None
    }

    #[test]
    fn rolling_uniform_cube_detector_matches_brute_force_for_every_dtype() {
        let shape = [7_usize, 8_usize, 9_usize];
        for dtype in [
            IntensityDType::Uint8,
            IntensityDType::Uint16,
            IntensityDType::Float32,
        ] {
            for seed in 0_u32..12 {
                let modulus = match dtype {
                    IntensityDType::Uint8 => 251,
                    IntensityDType::Uint16 => 65_521,
                    IntensityDType::Float32 => 0x007f_ffff,
                };
                let mut volume = (0..shape.iter().product::<usize>())
                    .map(|index| {
                        let mixed = (index as u32)
                            .wrapping_mul(1_664_525)
                            .wrapping_add(seed.wrapping_mul(1_013_904_223));
                        match dtype {
                            IntensityDType::Float32 => 0x3f00_0000 | (mixed % modulus),
                            _ => mixed % modulus,
                        }
                    })
                    .collect::<Vec<_>>();
                if seed % 2 == 0 {
                    let value = match dtype {
                        IntensityDType::Float32 => 1.25_f32.to_bits(),
                        _ => 37,
                    };
                    for z in 1..6 {
                        for y in 2..7 {
                            for x in 3..8 {
                                volume[(z * shape[1] + y) * shape[2] + x] = value;
                            }
                        }
                    }
                }
                let expected = naive_uniform_cube(&volume, shape);
                let mut detector =
                    UniformCubeDetector::new(shape[2] as u64, shape[1] as u64).unwrap();
                let cancellation = ImportCancellation::new();
                let mut actual = None;
                for z in 0..shape[0] {
                    let start = z * shape[1] * shape[2];
                    let end = start + shape[1] * shape[2];
                    actual = detector
                        .scan_plane(
                            &encode_detector_plane(dtype, &volume[start..end]),
                            dtype,
                            &cancellation,
                        )
                        .unwrap();
                    if actual.is_some() {
                        break;
                    }
                }
                assert_eq!(actual, expected, "dtype={dtype:?} seed={seed}");
            }
        }
    }

    #[test]
    fn reconstruction_memory_is_three_packed_bitsets() {
        assert_eq!(
            automatic_reconstruction_transient_bytes([5, 5, 5]),
            Some(
                5 * 5 * 3
                    + RECONSTRUCTION_RUNS_IN_MEMORY as u64 * RECONSTRUCTION_RUN_RECORD_BYTES
                    + RECONSTRUCTION_IO_BUFFER_BYTES as u64
            )
        );
        let wide_shape = [5, 65_536, 13_108];
        let voxels = wide_shape.into_iter().product::<u64>();
        assert!(voxels > u64::from(u32::MAX) + 1);
        assert_eq!(
            automatic_reconstruction_transient_bytes(wide_shape),
            automatic_mask_packed_bytes(wide_shape)
                .and_then(|bytes| bytes.checked_mul(3))
                .and_then(|bytes| {
                    bytes.checked_add(
                        RECONSTRUCTION_RUNS_IN_MEMORY as u64 * RECONSTRUCTION_RUN_RECORD_BYTES,
                    )
                })
                .and_then(|bytes| bytes.checked_add(RECONSTRUCTION_IO_BUFFER_BYTES as u64))
        );
        assert_eq!(
            automatic_reconstruction_transient_bytes([4, 99, 99]),
            Some(0)
        );
        assert_eq!(
            automatic_reconstruction_spool_bytes([5, 7, 9]),
            Some(5 * 7 * 5 * RECONSTRUCTION_RUN_RECORD_BYTES)
        );
        assert_eq!(automatic_reconstruction_spool_bytes([4, 99, 99]), Some(0));
    }

    #[test]
    fn reconstruction_frontier_spills_and_preserves_lifo_order() {
        let temp = tempfile::tempdir().unwrap();
        let mut stack = BoundedRunStack::new(temp.path()).unwrap();
        let total = RECONSTRUCTION_RUNS_IN_MEMORY + RECONSTRUCTION_IO_RUNS + 17;
        for index in 0..total {
            let index = index as u64;
            stack
                .push(EqualRun {
                    z: index,
                    y: index + 1,
                    x_start: index + 2,
                    x_end: index + 3,
                })
                .unwrap();
        }
        assert!(temp.path().join(RECONSTRUCTION_RUN_SPOOL_NAME).exists());
        for expected in (0..total as u64).rev() {
            assert_eq!(
                stack.pop().unwrap(),
                Some(EqualRun {
                    z: expected,
                    y: expected + 1,
                    x_start: expected + 2,
                    x_end: expected + 3,
                })
            );
        }
        assert_eq!(stack.pop().unwrap(), None);
        stack.finish().unwrap();
        assert!(!temp.path().join(RECONSTRUCTION_RUN_SPOOL_NAME).exists());
    }

    #[test]
    fn detector_uses_exact_float_bits_and_constant_plane_test_is_exact() {
        let mut volume = vec![1.0_f32.to_bits(); 5 * 5 * 5];
        volume[62] = f32::from_bits(1.0_f32.to_bits() + 1).to_bits();
        let mut detector = UniformCubeDetector::new(5, 5).unwrap();
        let cancellation = ImportCancellation::new();
        let mut found = None;
        for plane in volume.chunks_exact(25) {
            found = detector
                .scan_plane(
                    &encode_detector_plane(IntensityDType::Float32, plane),
                    IntensityDType::Float32,
                    &cancellation,
                )
                .unwrap();
        }
        assert_eq!(found, None);
        assert!(plane_is_exactly_constant(&[7_u8; 25], IntensityDType::Uint8).unwrap());
        let mut nonconstant = vec![0_u8; 50];
        nonconstant[49] = 1;
        assert!(!plane_is_exactly_constant(&nonconstant, IntensityDType::Uint16).unwrap());
    }

    #[test]
    fn policy_resolution_reads_only_the_first_float_volume() {
        let root = tempdir().unwrap();
        let source = root.path().join("float-series");
        fs::create_dir(&source).unwrap();
        let shape = [6_usize, 6_usize, 6_usize];
        let mut first = vec![0.0_f32; shape.iter().product()];
        for z in 1..shape[0] {
            for y in 0..shape[1] {
                for x in 0..shape[2] {
                    first[(z * shape[1] + y) * shape[2] + x] = if y < 5 && x < 5 {
                        42.25
                    } else {
                        (z * 100 + y * 10 + x) as f32
                    };
                }
            }
        }
        let mut second = (0..shape.iter().product())
            .map(|index| index as f32 + 0.5)
            .collect::<Vec<_>>();
        second[2 * shape[1] * shape[2]..3 * shape[1] * shape[2]].fill(99.0);
        for (timepoint, values) in [(0, &first), (1, &second)] {
            let file = File::create(source.join(format!("sample_time{timepoint}.tif"))).unwrap();
            let mut encoder = TiffEncoder::new(file).unwrap();
            for plane in values.chunks_exact(shape[1] * shape[2]) {
                encoder
                    .write_image::<colortype::Gray32Float>(shape[2] as u32, shape[1] as u32, plane)
                    .unwrap();
            }
        }

        let inspection = inspect(
            TiffSource::new(vec![
                TiffChannelSource::folder_of_3d("channel 1", &source).unwrap(),
            ])
            .unwrap(),
        )
        .unwrap();
        assert_eq!(inspection.shape.t(), 2);
        let generation = capture_generation(&inspection, &ImportCancellation::new()).unwrap();
        let checkpoint = root.path().join("canonical");
        fs::create_dir(&checkpoint).unwrap();
        let mut cache = CanonicalBaseCache::open_or_create(
            &checkpoint,
            crate::canonical_cache::CanonicalCacheBinding::new(
                Sha256Digest::parse(&"2".repeat(64)).unwrap(),
                inspection.source_fingerprint,
            ),
            inspection.shape,
            inspection.channels,
            inspection.dtype,
        )
        .unwrap();
        decode_canonical_into_cache(
            &inspection,
            &generation,
            &mut cache,
            inspection.maximum_decoded_chunk_bytes,
            256 * 1024 * 1024,
            inspection.source_index_working_bytes,
            &TestLedger,
            &ImportCancellation::new(),
            |_, _| {},
        )
        .unwrap();
        let request = NoDataPolicy::new(Some(NoDataValueRule::Automatic), true);
        let report = resolve_no_data_policy_from_cache(
            &inspection,
            &cache.reader().unwrap(),
            &checkpoint,
            Some(request),
            &TestLedger,
            &ImportCancellation::new(),
            |_, _| {},
        )
        .unwrap();

        assert_eq!(report.completed_planes, 12);
        assert_eq!(report.policy.request(), Some(request));
        assert_eq!(report.policy.constant_z_planes(), [0]);
        assert_eq!(
            report.policy.value(),
            Some(ResolvedNoDataValue::Float32Bits(42.25_f32.to_bits()))
        );
        let mask = report.policy.automatic_mask().unwrap();
        assert_eq!(mask.shape_zyx(), [6, 6, 6]);
        assert_eq!(mask.masked_voxels(), 125);
        assert!(mask.contains(5, 4, 4));
        assert!(!mask.contains(5, 5, 5));
    }

    #[derive(Default)]
    struct TrackingState {
        used: u64,
        peak: u64,
    }

    struct TrackingLedger {
        budget: u64,
        state: Arc<Mutex<TrackingState>>,
    }

    struct TrackingLease {
        bytes: u64,
        state: Arc<Mutex<TrackingState>>,
    }

    impl Drop for TrackingLease {
        fn drop(&mut self) {
            let mut state = self.state.lock().unwrap();
            state.used -= self.bytes;
        }
    }

    impl CpuByteLease for TrackingLease {
        fn category(&self) -> CpuLedgerCategory {
            CpuLedgerCategory::ImportWorkingSet
        }

        fn reserved_bytes(&self) -> u64 {
            self.bytes
        }
    }

    impl CpuByteLedger for TrackingLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            assert_eq!(category, CpuLedgerCategory::ImportWorkingSet);
            let mut state = self.state.lock().unwrap();
            let available = self.budget.saturating_sub(state.used);
            if bytes > available {
                return Err(CpuLedgerError::CapacityExceeded {
                    category,
                    requested_bytes: bytes,
                    available_bytes: available,
                });
            }
            state.used += bytes;
            state.peak = state.peak.max(state.used);
            drop(state);
            Ok(Box::new(TrackingLease {
                bytes,
                state: Arc::clone(&self.state),
            }))
        }
    }

    #[test]
    fn single_stack_inspection_and_canonical_decode_are_exact_and_bounded() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("stack.tif");
        write_u16_stack(&path, 4, 3, 2, true).unwrap();

        let inspection = inspect(TiffSource::single_3d(&path)).unwrap();
        assert_eq!(inspection.shape.dimensions(), [1, 2, 3, 4]);
        assert_eq!(inspection.channels, 1);
        assert_eq!(inspection.dtype, IntensityDType::Uint16);
        assert_eq!(inspection.files.len(), 1);
        assert_eq!(inspection.files[0].planes, 2);
        assert_eq!(inspection.source_bytes, fs::metadata(&path).unwrap().len());
        assert_eq!(inspection.maximum_decoded_chunk_bytes, 8);
        let revalidation = revalidate(&inspection, &ImportCancellation::new()).unwrap();
        assert_eq!(revalidation.source_bytes_read, 0);
        assert_eq!(revalidation.tiff_open_count, 0);

        let cancellation = ImportCancellation::new();
        cancellation.cancel();
        assert!(matches!(
            revalidate(&inspection, &cancellation),
            Err(ImportError::Cancelled)
        ));

        let checkpoint = temporary.path().join("canonical-checkpoint");
        fs::create_dir(&checkpoint).unwrap();
        let binding = crate::canonical_cache::CanonicalCacheBinding::new(
            Sha256Digest::parse(&"7".repeat(64)).unwrap(),
            inspection.source_fingerprint,
        );
        let mut cache = crate::canonical_cache::CanonicalBaseCache::open_or_create(
            &checkpoint,
            binding,
            inspection.shape,
            inspection.channels,
            inspection.dtype,
        )
        .unwrap();
        let report = decode_canonical_into_cache(
            &inspection,
            &revalidation.generation,
            &mut cache,
            inspection.maximum_decoded_chunk_bytes,
            256 * 1024 * 1024,
            1,
            &TestLedger,
            &ImportCancellation::new(),
            |_, _| {},
        )
        .unwrap();
        let counters = report.counters;
        assert_eq!(counters.decoded_bytes, 48);
        assert_eq!(counters.tiff_open_count, 1);
        assert_eq!(counters.native_chunk_decode_count, 6);
        assert_eq!(report.serialized_files, 1);
        assert_eq!(report.parallel_files, 0);
        let mut canonical = vec![0; 48];
        cache
            .read_region_into(0, 0, [0, 0, 0], [2, 3, 4], &mut canonical)
            .unwrap();
        let values = canonical
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            [
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 100, 101, 102, 103, 104, 105, 106, 107, 108,
                109, 110, 111,
            ]
        );
        let resumed = decode_canonical_into_cache(
            &inspection,
            &revalidation.generation,
            &mut cache,
            inspection.maximum_decoded_chunk_bytes,
            256 * 1024 * 1024,
            1,
            &TestLedger,
            &ImportCancellation::new(),
            |_, _| {},
        )
        .unwrap();
        assert_eq!(resumed, SourceDecodeReport::default());

        let limited_checkpoint = temporary.path().join("limited-checkpoint");
        fs::create_dir(&limited_checkpoint).unwrap();
        let mut limited = CanonicalBaseCache::open_or_create(
            &limited_checkpoint,
            crate::canonical_cache::CanonicalCacheBinding::new(
                Sha256Digest::parse(&"6".repeat(64)).unwrap(),
                inspection.source_fingerprint,
            ),
            inspection.shape,
            inspection.channels,
            inspection.dtype,
        )
        .unwrap();
        let error = decode_canonical_into_cache(
            &inspection,
            &revalidation.generation,
            &mut limited,
            7,
            256 * 1024 * 1024,
            1,
            &TestLedger,
            &ImportCancellation::new(),
            |_, _| {},
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ImportError::ManagedCapacityInsufficient {
                required_bytes: 8,
                capacity_bytes: 7
            }
        ));

        let cancelled_checkpoint = temporary.path().join("cancelled-checkpoint");
        fs::create_dir(&cancelled_checkpoint).unwrap();
        let mut cancelled_cache = CanonicalBaseCache::open_or_create(
            &cancelled_checkpoint,
            crate::canonical_cache::CanonicalCacheBinding::new(
                Sha256Digest::parse(&"5".repeat(64)).unwrap(),
                inspection.source_fingerprint,
            ),
            inspection.shape,
            inspection.channels,
            inspection.dtype,
        )
        .unwrap();
        let cancellation = ImportCancellation::new();
        cancellation.cancel();
        let error = decode_canonical_into_cache(
            &inspection,
            &revalidation.generation,
            &mut cancelled_cache,
            inspection.maximum_decoded_chunk_bytes,
            256 * 1024 * 1024,
            1,
            &TestLedger,
            &cancellation,
            |_, _| {},
        )
        .unwrap_err();
        assert!(matches!(error, ImportError::Cancelled));

        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"drift")
            .unwrap();
        assert!(matches!(
            revalidate(&inspection, &ImportCancellation::new()),
            Err(ImportError::SourceChanged(changed)) if changed == path
        ));
        assert!(matches!(
            capture_generation(&inspection, &ImportCancellation::new()),
            Err(ImportError::SourceChanged(changed)) if changed == path
        ));
    }

    #[test]
    fn byte_identical_replacement_after_review_is_rejected_at_start_and_final_check() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("stack.tif");
        let backup = temporary.path().join("stack-original.tif");
        write_u16_stack(&path, 4, 3, 1, true).unwrap();
        let inspection = inspect(TiffSource::single_3d(&path)).unwrap();
        fs::rename(&path, &backup).unwrap();
        fs::copy(&backup, &path).unwrap();

        assert!(matches!(
            capture_generation(&inspection, &ImportCancellation::new()),
            Err(ImportError::SourceChanged(changed)) if changed == path
        ));
        assert!(matches!(
            revalidate(&inspection, &ImportCancellation::new()),
            Err(ImportError::SourceChanged(changed)) if changed == path
        ));
    }

    #[test]
    fn opened_descriptor_cannot_be_hidden_by_restoring_the_source_path() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("source.tif");
        let original = temporary.path().join("original.tif");
        let replacement = temporary.path().join("replacement.tif");
        fs::write(&path, b"original-bytes").unwrap();
        fs::write(&replacement, b"replaced-bytes").unwrap();
        let expected = source_file_generation(&path).unwrap();

        fs::rename(&path, &original).unwrap();
        fs::rename(&replacement, &path).unwrap();
        let opened_replacement = open_source_file(&path, "open replacement").unwrap();
        fs::rename(&path, &replacement).unwrap();
        fs::rename(&original, &path).unwrap();

        let restored = source_file_generation(&path).unwrap();
        assert_eq!(
            (restored.device, restored.inode),
            (expected.device, expected.inode)
        );
        assert_ne!(
            source_file_generation_from_open_file(&path, &opened_replacement).unwrap(),
            restored
        );
    }

    #[test]
    fn explicit_channels_ignore_filename_tokens_and_bind_relative_structure() {
        let first = tempdir().unwrap();
        let second = tempdir().unwrap();
        let first_root = first.path().join("source");
        let second_root = second.path().join("source");
        fs::create_dir(&first_root).unwrap();
        fs::create_dir(&second_root).unwrap();
        for root in [&first_root, &second_root] {
            let red = root.join("red");
            let green = root.join("green");
            fs::create_dir(&red).unwrap();
            fs::create_dir(&green).unwrap();
            for (folder, files) in [
                (&red, [("zebra.tif", 90), ("alpha.tif", 50)]),
                (&green, [("unrelated-b.tif", 190), ("unrelated-a.tif", 150)]),
            ] {
                for (filename, base) in files {
                    write_u16_stack_with_base(&folder.join(filename), 2, 2, 1, false, base)
                        .unwrap();
                }
            }
        }
        let manifest = |root: &Path| {
            TiffSource::new(vec![
                TiffChannelSource::folder_of_3d("red", root.join("red")).unwrap(),
                TiffChannelSource::folder_of_3d("green", root.join("green")).unwrap(),
            ])
            .unwrap()
        };
        let first_inspection = inspect(manifest(&first_root)).unwrap();
        let second_inspection = inspect(manifest(&second_root)).unwrap();
        assert_eq!(first_inspection.shape.dimensions(), [2, 1, 2, 2]);
        assert_eq!(first_inspection.channels, 2);
        assert_ne!(
            first_inspection.source_fingerprint,
            second_inspection.source_fingerprint
        );
        let mapping = first_inspection
            .files
            .iter()
            .map(|file| (file.channel, file.timepoint, file.path.file_name().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(mapping[0].2, "alpha.tif");
        assert_eq!(mapping[1].2, "zebra.tif");
        assert_eq!(mapping[2].2, "unrelated-a.tif");
        assert_eq!(mapping[3].2, "unrelated-b.tif");
        revalidate(&first_inspection, &ImportCancellation::new()).unwrap();

        write_u16_stack(&first_root.join("red").join("new.tif"), 2, 2, 1, false).unwrap();
        assert!(matches!(
            revalidate(&first_inspection, &ImportCancellation::new()),
            Err(ImportError::SourceChanged(_))
        ));
    }

    #[test]
    fn explicit_folder_of_3d_accepts_arbitrary_filenames() {
        let temporary = tempdir().unwrap();
        write_u16_stack(&temporary.path().join("alpha.tif"), 2, 2, 1, false).unwrap();
        write_u16_stack(&temporary.path().join("beta.tif"), 2, 2, 1, false).unwrap();
        let source = TiffSource::new(vec![
            TiffChannelSource::folder_of_3d("signal", temporary.path()).unwrap(),
        ])
        .unwrap();
        let inspection = inspect(source).unwrap();
        assert_eq!(inspection.shape.t(), 2);
    }

    #[test]
    fn channel_folders_are_non_recursive_single_plane_channels() {
        let temporary = tempdir().unwrap();
        let channel_five = temporary.path().join("channel5");
        let channel_two = temporary.path().join("channel2");
        fs::create_dir(&channel_five).unwrap();
        fs::create_dir(&channel_two).unwrap();
        for (folder, base) in [(&channel_five, 50), (&channel_two, 20)] {
            write_u16_stack_with_base(&folder.join("z00.tif"), 3, 2, 1, false, base).unwrap();
            write_u16_stack_with_base(&folder.join("z01.tif"), 3, 2, 1, false, base + 10).unwrap();
        }

        let inspection = inspect(
            TiffSource::new(vec![
                TiffChannelSource::folder_of_2d("channel two", &channel_two).unwrap(),
                TiffChannelSource::folder_of_2d("channel five", &channel_five).unwrap(),
            ])
            .unwrap(),
        )
        .unwrap();
        assert_eq!(inspection.shape.dimensions(), [1, 2, 2, 3]);
        assert_eq!(inspection.channels, 2);
        assert_eq!(inspection.channel_labels, ["channel two", "channel five"]);

        let checkpoint = tempdir().unwrap();
        let generation = capture_generation(&inspection, &ImportCancellation::new()).unwrap();
        let mut cache = CanonicalBaseCache::open_or_create(
            checkpoint.path(),
            crate::canonical_cache::CanonicalCacheBinding::new(
                Sha256Digest::parse(&"4".repeat(64)).unwrap(),
                inspection.source_fingerprint,
            ),
            inspection.shape,
            inspection.channels,
            inspection.dtype,
        )
        .unwrap();
        decode_canonical_into_cache(
            &inspection,
            &generation,
            &mut cache,
            inspection.maximum_decoded_chunk_bytes,
            256 * 1024 * 1024,
            1,
            &TestLedger,
            &ImportCancellation::new(),
            |_, _| {},
        )
        .unwrap();
        let mut destination = vec![0_u8; 2 * 2 * 3 * 2];
        cache
            .read_region_into(0, 0, [0, 0, 0], [2, 2, 3], &mut destination)
            .unwrap();
        let values = destination
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(values, [20, 21, 22, 23, 24, 25, 30, 31, 32, 33, 34, 35]);

        let nested = temporary.path().join("channel2").join("nested");
        fs::create_dir(&nested).unwrap();
        let reinspection = inspect(
            TiffSource::new(vec![
                TiffChannelSource::folder_of_2d("channel two", &channel_two).unwrap(),
                TiffChannelSource::folder_of_2d("channel five", &channel_five).unwrap(),
            ])
            .unwrap(),
        )
        .unwrap();
        assert_eq!(reinspection.shape, inspection.shape);
    }

    #[test]
    fn single_plane_files_use_the_ordered_byte_bounded_decode_path() {
        let temporary = tempdir().unwrap();
        let source = temporary.path().join("source");
        let channel = source.join("channel0");
        fs::create_dir_all(&channel).unwrap();
        for z in 0..8_u16 {
            write_u16_stack_with_base(
                &channel.join(format!("z{z:02}.tif")),
                4,
                3,
                1,
                true,
                z * 100,
            )
            .unwrap();
        }
        let inspection = inspect(
            TiffSource::new(vec![
                TiffChannelSource::folder_of_2d("channel 1", &channel).unwrap(),
            ])
            .unwrap(),
        )
        .unwrap();
        let generation = revalidate(&inspection, &ImportCancellation::new())
            .unwrap()
            .generation;
        let checkpoint = temporary.path().join("checkpoint");
        fs::create_dir(&checkpoint).unwrap();
        let mut cache = CanonicalBaseCache::open_or_create(
            &checkpoint,
            crate::canonical_cache::CanonicalCacheBinding::new(
                Sha256Digest::parse(&"8".repeat(64)).unwrap(),
                inspection.source_fingerprint,
            ),
            inspection.shape,
            inspection.channels,
            inspection.dtype,
        )
        .unwrap();
        let budget = 128 * 1024 * 1024;
        let resident = 1024 * 1024;
        let state = Arc::new(Mutex::new(TrackingState::default()));
        let ledger = TrackingLedger {
            budget: budget - resident,
            state: Arc::clone(&state),
        };
        let report = decode_canonical_into_cache(
            &inspection,
            &generation,
            &mut cache,
            inspection.maximum_decoded_chunk_bytes,
            budget,
            resident,
            &ledger,
            &ImportCancellation::new(),
            |_, _| {},
        )
        .unwrap();

        assert_eq!(report.parallel_files, 8);
        assert_eq!(report.serialized_files, 0);
        assert_eq!(report.counters.tiff_open_count, 8);
        assert_eq!(report.counters.native_chunk_decode_count, 24);
        assert!(report.peak_reorder_results <= 8);
        assert!(report.peak_transient_bytes <= budget - resident);
        let state = state.lock().unwrap();
        assert_eq!(state.used, 0);
        assert!(state.peak <= budget - resident);
        drop(state);

        let mut canonical = vec![0; 8 * 3 * 4 * 2];
        cache
            .read_region_into(0, 0, [0, 0, 0], [8, 3, 4], &mut canonical)
            .unwrap();
        let values = canonical
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        for z in 0..8_usize {
            assert_eq!(
                &values[z * 12..z * 12 + 12],
                &(0..12).map(|i| (z * 100 + i) as u16).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn complete_ome_spacing_is_canonical_zyx_and_nonfinite_f32_is_rejected_on_decode() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("float.ome.tif");
        let description = r#"<OME xmlns="http://www.openmicroscopy.org/Schemas/OME/2016-06"><Image ID="Image:0"><Pixels DimensionOrder="XYZCT" Type="float" SizeX="2" SizeY="2" SizeZ="1" SizeC="1" SizeT="1" PhysicalSizeX="200" PhysicalSizeXUnit="nm" PhysicalSizeY="0.3" PhysicalSizeYUnit="um" PhysicalSizeZ="0.0007" PhysicalSizeZUnit="mm"><TiffData IFD="0" PlaneCount="1"/></Pixels></Image></OME>"#;
        write_f32(&path, &[1.0, f32::NAN, -0.0, 4.0], Some(description)).unwrap();

        let inspection = inspect(TiffSource::single_3d(&path)).unwrap();
        assert_eq!(inspection.dtype, IntensityDType::Float32);
        assert_eq!(inspection.ome_spacing_zyx_um, Some([0.7, 0.3, 0.2]));
        let generation = capture_generation(&inspection, &ImportCancellation::new()).unwrap();
        let checkpoint = tempdir().unwrap();
        let mut cache = CanonicalBaseCache::open_or_create(
            checkpoint.path(),
            crate::canonical_cache::CanonicalCacheBinding::new(
                Sha256Digest::parse(&"3".repeat(64)).unwrap(),
                inspection.source_fingerprint,
            ),
            inspection.shape,
            inspection.channels,
            inspection.dtype,
        )
        .unwrap();
        let error = decode_canonical_into_cache(
            &inspection,
            &generation,
            &mut cache,
            inspection.maximum_decoded_chunk_bytes,
            256 * 1024 * 1024,
            1,
            &TestLedger,
            &ImportCancellation::new(),
            |_, _| {},
        )
        .unwrap_err();
        assert!(
            matches!(error, ImportError::UnsupportedSource(message) if message.contains("non-finite"))
        );
    }

    #[test]
    fn ome_dimensions_and_tiff_data_must_describe_the_decoded_stack() {
        let temporary = tempdir().unwrap();
        let accepted = temporary.path().join("accepted.ome.tif");
        let accepted_description =
            simple_ome_description(3, 2, 2, 1, 1, "IFD=\"0\" PlaneCount=\"2\"");
        write_u16_stack_with_description(&accepted, 3, 2, 2, &accepted_description).unwrap();
        let inspection = inspect(TiffSource::single_3d(&accepted)).unwrap();
        assert_eq!(inspection.shape.dimensions(), [1, 2, 2, 3]);

        for (name, description) in [
            (
                "wrong-x",
                simple_ome_description(4, 2, 2, 1, 1, "IFD=\"0\" PlaneCount=\"2\""),
            ),
            (
                "wrong-z",
                simple_ome_description(3, 2, 1, 1, 1, "IFD=\"0\" PlaneCount=\"2\""),
            ),
            (
                "multiple-channels",
                simple_ome_description(3, 2, 2, 2, 1, "IFD=\"0\" PlaneCount=\"2\""),
            ),
            (
                "multiple-times",
                simple_ome_description(3, 2, 2, 1, 2, "IFD=\"0\" PlaneCount=\"2\""),
            ),
            (
                "remapped-ifd",
                simple_ome_description(3, 2, 2, 1, 1, "IFD=\"1\" PlaneCount=\"2\""),
            ),
            (
                "partial-mapping",
                simple_ome_description(3, 2, 2, 1, 1, "IFD=\"0\" PlaneCount=\"1\""),
            ),
        ] {
            let path = temporary.path().join(format!("{name}.ome.tif"));
            write_u16_stack_with_description(&path, 3, 2, 2, &description).unwrap();
            assert!(matches!(
                inspect(TiffSource::single_3d(path)),
                Err(ImportError::UnsupportedSource(_))
            ));
        }
    }

    #[test]
    fn ome_pixel_type_is_required_and_must_match_the_decoded_tiff() {
        let temporary = tempdir().unwrap();
        for (name, description) in [
            (
                "missing-type",
                simple_ome_description_with_type(None, 3, 2, 1, 1, 1, "IFD=\"0\" PlaneCount=\"1\""),
            ),
            (
                "wrong-type",
                simple_ome_description_with_type(
                    Some("uint8"),
                    3,
                    2,
                    1,
                    1,
                    1,
                    "IFD=\"0\" PlaneCount=\"1\"",
                ),
            ),
            (
                "unsupported-type",
                simple_ome_description_with_type(
                    Some("int16"),
                    3,
                    2,
                    1,
                    1,
                    1,
                    "IFD=\"0\" PlaneCount=\"1\"",
                ),
            ),
        ] {
            let path = temporary.path().join(format!("{name}.ome.tif"));
            write_u16_stack_with_description(&path, 3, 2, 1, &description).unwrap();
            assert!(matches!(
                inspect(TiffSource::single_3d(path)),
                Err(ImportError::UnsupportedSource(_))
            ));
        }
    }

    #[test]
    fn unsupported_color_layout_is_rejected_without_a_pixel_read() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("rgb.tif");
        let file = File::create(&path).unwrap();
        let mut encoder = TiffEncoder::new(file).unwrap();
        encoder
            .write_image::<colortype::RGB8>(2, 1, &[1, 2, 3, 4, 5, 6])
            .unwrap();
        assert!(matches!(
            inspect(TiffSource::single_3d(path)),
            Err(ImportError::UnsupportedSource(_))
        ));
    }

    #[test]
    fn tiled_tiff_decodes_each_native_tile_once_and_crops_edge_padding_exactly() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("tiled.tif");
        let values = (0..19 * 17)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect::<Vec<_>>();
        write_tiled_u8(&path, 19, 17, 16, 16, &values).unwrap();

        let inspection = inspect(TiffSource::single_3d(&path)).unwrap();
        assert_eq!(inspection.shape.dimensions(), [1, 1, 17, 19]);
        assert_eq!(inspection.dtype, IntensityDType::Uint8);
        assert_eq!(inspection.maximum_decoded_chunk_bytes, 16 * 16);
        assert_eq!(inspection.maximum_encoded_chunk_bytes, 16 * 16);

        let (report, canonical) = decode_fixture(&inspection, 'a');
        assert_eq!(report.counters.tiff_open_count, 1);
        assert_eq!(report.counters.native_chunk_decode_count, 4);
        assert_eq!(report.counters.decoded_bytes, 19 * 17);
        assert_eq!(canonical, values);
    }

    #[test]
    fn supported_packbits_tiff_decodes_each_native_strip_once_and_preserves_values() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("packbits.tif");
        let values = [vec![7_u8; 16], vec![9_u8; 16]].concat();
        let file = File::create(&path).unwrap();
        let mut encoder = TiffEncoder::new(file)
            .unwrap()
            .with_compression(Compression::Packbits);
        let mut image = encoder.new_image::<colortype::Gray8>(8, 4).unwrap();
        image.rows_per_strip(2).unwrap();
        image.write_data(&values).unwrap();

        let inspection = inspect(TiffSource::single_3d(&path)).unwrap();
        assert_eq!(inspection.shape.dimensions(), [1, 1, 4, 8]);
        assert_eq!(inspection.dtype, IntensityDType::Uint8);
        assert_eq!(inspection.maximum_decoded_chunk_bytes, 16);
        assert_eq!(inspection.maximum_encoded_chunk_bytes, 2);

        let (report, canonical) = decode_fixture(&inspection, 'b');
        assert_eq!(report.counters.tiff_open_count, 1);
        assert_eq!(report.counters.native_chunk_decode_count, 2);
        assert_eq!(report.counters.decoded_bytes, 32);
        assert_eq!(canonical, values);
    }

    #[test]
    fn supported_lzw_and_deflate_tiffs_decode_exactly() {
        let temporary = tempdir().unwrap();
        let values = (0..32).map(|index| (index * 7) as u8).collect::<Vec<_>>();
        for (name, compression, binding) in [
            ("lzw", Compression::Lzw, 'd'),
            (
                "deflate",
                Compression::Deflate(DeflateLevel::default()),
                'e',
            ),
        ] {
            let path = temporary.path().join(format!("{name}.tif"));
            let file = File::create(&path).unwrap();
            let mut encoder = TiffEncoder::new(file)
                .unwrap()
                .with_compression(compression);
            let mut image = encoder.new_image::<colortype::Gray8>(8, 4).unwrap();
            image.rows_per_strip(2).unwrap();
            image.write_data(&values).unwrap();

            let inspection = inspect(TiffSource::single_3d(&path)).unwrap();
            let (report, canonical) = decode_fixture(&inspection, binding);
            assert_eq!(report.counters.native_chunk_decode_count, 2);
            assert_eq!(report.counters.decoded_bytes, 32);
            assert_eq!(canonical, values, "{name}");
        }
    }

    #[test]
    fn supported_old_deflate_tiff_decodes_exactly() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("old-deflate.tif");
        let values = (0..32).map(|index| (index * 11) as u8).collect::<Vec<_>>();
        let file = File::create(&path).unwrap();
        let mut encoder = TiffEncoder::new(file)
            .unwrap()
            .with_compression(Compression::Deflate(DeflateLevel::default()));
        let mut image = encoder.new_image::<colortype::Gray8>(8, 4).unwrap();
        image.rows_per_strip(2).unwrap();
        image.write_data(&values).unwrap();
        patch_classic_le_compression(&path, 0x80b2);

        let inspection = inspect(TiffSource::single_3d(&path)).unwrap();
        let (report, canonical) = decode_fixture(&inspection, '1');
        assert_eq!(report.counters.native_chunk_decode_count, 2);
        assert_eq!(canonical, values);
    }

    #[test]
    fn otherwise_valid_tiffs_with_closed_out_codecs_are_rejected() {
        let temporary = tempdir().unwrap();
        let base = temporary.path().join("base.tif");
        let file = File::create(&base).unwrap();
        TiffEncoder::new(file)
            .unwrap()
            .write_image::<colortype::Gray8>(2, 2, &[1, 2, 3, 4])
            .unwrap();
        for (name, compression) in [
            ("fax3", 3),
            ("fax4", 4),
            ("jpeg", 7),
            ("zstd", 50_000),
            ("webp", 50_001),
            ("other", 65_000),
        ] {
            let path = temporary.path().join(format!("{name}.tif"));
            fs::copy(&base, &path).unwrap();
            patch_classic_le_compression(&path, compression);
            assert!(matches!(
                inspect(TiffSource::single_3d(&path)),
                Err(ImportError::UnsupportedSource(message))
                    if message.contains(&compression.to_string())
            ));
        }
    }

    #[test]
    fn compression_allowlist_is_closed_and_explicit() {
        for compression in [1, 5, 8, 0x80b2, 0x8005] {
            assert!(supported_tiff_compression(compression), "{compression:#x}");
        }
        for compression in [0, 2, 3, 4, 6, 7, 9, 10, 32766, 50000] {
            assert!(!supported_tiff_compression(compression), "{compression:#x}");
        }
    }

    #[test]
    fn raw_preflight_rejects_unbounded_first_ifd_allocations() {
        let temporary = tempdir().unwrap();
        let cancellation = ImportCancellation::new();

        let excessive_entries = temporary.path().join("ifd-entries.tif");
        let mut bytes = classic_tiff_header();
        FixtureEndian::Little
            .push_u16(&mut bytes, u16::try_from(MAX_TIFF_IFD_ENTRIES + 1).unwrap());
        fs::write(&excessive_entries, bytes).unwrap();
        let file = File::open(&excessive_entries).unwrap();
        assert!(matches!(
            preflight_tiff_ifd_chain(&excessive_entries, &file, &cancellation),
            Err(ImportError::UnsupportedSource(message)) if message.contains("entries")
        ));

        for (name, hostile_entry, expected) in [
            ("bits", (258, 3, 1_000_000, 0), "BitsPerSample"),
            ("photometric", (262, 3, 1_000_000, 0), "scalar"),
            (
                "chunks",
                (273, 4, MAX_NATIVE_CHUNKS_PER_PAGE as u32 + 1, 0),
                "strip/tile",
            ),
        ] {
            let path = temporary.path().join(format!("{name}.tif"));
            let entries = if hostile_entry.0 == 273 {
                vec![hostile_entry, (279, 4, 1, 1)]
            } else {
                vec![hostile_entry, (273, 4, 1, 0), (279, 4, 1, 1)]
            };
            write_preflight_fixture(&path, &entries, 0).unwrap();
            let file = File::open(&path).unwrap();
            assert!(matches!(
                preflight_tiff_ifd_chain(&path, &file, &cancellation),
                Err(ImportError::UnsupportedSource(message)) if message.contains(expected)
            ));
        }

        let jpeg = temporary.path().join("jpeg-tables.tif");
        write_preflight_fixture(
            &jpeg,
            &[
                (259, 3, 1, 7),
                (273, 4, 1, 0),
                (279, 4, 1, 1),
                (347, 7, 8_000_000, 0),
            ],
            0,
        )
        .unwrap();
        let file = File::open(&jpeg).unwrap();
        assert!(matches!(
            preflight_tiff_ifd_chain(&jpeg, &file, &cancellation),
            Err(ImportError::UnsupportedSource(message))
                if message.contains("compression code 7")
        ));
    }

    #[test]
    fn raw_preflight_enforces_the_retained_page_authority() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("too-many-pages.tif");
        let pages = MAX_PAGES_PER_FILE + 1;
        let ifd_bytes = 2_u64 + 2 * 12 + 4;
        let mut bytes = classic_tiff_header();
        for page in 0..pages {
            FixtureEndian::Little.push_u16(&mut bytes, 2);
            push_long_ifd_entry(&mut bytes, FixtureEndian::Little, 273, 1, 0);
            push_long_ifd_entry(&mut bytes, FixtureEndian::Little, 279, 1, 1);
            let next = if page + 1 == pages {
                0
            } else {
                u32::try_from(8 + (page + 1) * ifd_bytes).unwrap()
            };
            FixtureEndian::Little.push_u32(&mut bytes, next);
        }
        fs::write(&path, bytes).unwrap();
        let file = File::open(&path).unwrap();
        assert!(matches!(
            preflight_tiff_ifd_chain(&path, &file, &ImportCancellation::new()),
            Err(ImportError::UnsupportedSource(message)) if message.contains("page limit")
        ));
        assert_eq!(
            retained_decoder_bytes(MAX_PAGES_PER_FILE),
            Some(32 * 1024 * 1024)
        );
    }

    #[test]
    fn big_endian_u16_tiff_is_canonicalized_little_endian_per_native_strip() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("big-endian.tif");
        let values = [0x0102, 0x1122, 0x3344, 0x5566, 0x7788, 0x99aa];
        write_big_endian_striped_u16(&path, 3, 2, 1, &values).unwrap();

        let inspection = inspect(TiffSource::single_3d(&path)).unwrap();
        assert_eq!(inspection.shape.dimensions(), [1, 1, 2, 3]);
        assert_eq!(inspection.dtype, IntensityDType::Uint16);
        assert_eq!(inspection.maximum_decoded_chunk_bytes, 6);
        assert_eq!(inspection.maximum_encoded_chunk_bytes, 6);

        let (report, canonical) = decode_fixture(&inspection, 'c');
        assert_eq!(report.counters.tiff_open_count, 1);
        assert_eq!(report.counters.native_chunk_decode_count, 2);
        assert_eq!(report.counters.decoded_bytes, 12);
        let canonical_values = canonical
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(canonical_values, values);
        assert_eq!(&canonical[..2], &0x0102_u16.to_le_bytes());
    }

    #[test]
    fn bigtiff_preflight_and_decode_preserve_values() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("source-bigtiff.tif");
        let values = [3_u16, 5, 8, 13, 21, 34];
        let file = File::create(&path).unwrap();
        let mut encoder = TiffEncoder::new_big(file).unwrap();
        let mut image = encoder.new_image::<colortype::Gray16>(3, 2).unwrap();
        image.rows_per_strip(1).unwrap();
        image.write_data(&values).unwrap();

        let inspection = inspect(TiffSource::single_3d(&path)).unwrap();
        let (report, canonical) = decode_fixture(&inspection, 'f');
        assert_eq!(report.counters.native_chunk_decode_count, 2);
        assert_eq!(
            canonical
                .chunks_exact(2)
                .map(|bytes| u16::from_le_bytes(bytes.try_into().unwrap()))
                .collect::<Vec<_>>(),
            values
        );
    }

    fn decode_fixture(
        inspection: &TiffInspection,
        binding_digit: char,
    ) -> (SourceDecodeReport, Vec<u8>) {
        assert_eq!(inspection.shape.t(), 1);
        assert_eq!(inspection.channels, 1);
        let checkpoint = tempdir().unwrap();
        let binding = crate::canonical_cache::CanonicalCacheBinding::new(
            Sha256Digest::parse(&binding_digit.to_string().repeat(64)).unwrap(),
            inspection.source_fingerprint,
        );
        let mut cache = CanonicalBaseCache::open_or_create(
            checkpoint.path(),
            binding,
            inspection.shape,
            inspection.channels,
            inspection.dtype,
        )
        .unwrap();
        let generation = capture_generation(inspection, &ImportCancellation::new()).unwrap();
        let report = decode_canonical_into_cache(
            inspection,
            &generation,
            &mut cache,
            inspection.maximum_decoded_chunk_bytes,
            256 * 1024 * 1024,
            1,
            &TestLedger,
            &ImportCancellation::new(),
            |_, _| {},
        )
        .unwrap();
        let canonical_bytes = inspection
            .shape
            .z()
            .checked_mul(inspection.shape.y())
            .and_then(|value| value.checked_mul(inspection.shape.x()))
            .and_then(|value| value.checked_mul(u64::from(inspection.dtype.bytes_per_sample())))
            .unwrap();
        let mut canonical = vec![0; usize::try_from(canonical_bytes).unwrap()];
        cache
            .read_region_into(
                0,
                0,
                [0, 0, 0],
                [
                    inspection.shape.z(),
                    inspection.shape.y(),
                    inspection.shape.x(),
                ],
                &mut canonical,
            )
            .unwrap();
        (report, canonical)
    }

    #[derive(Clone, Copy)]
    enum FixtureEndian {
        Little,
        Big,
    }

    impl FixtureEndian {
        fn push_u16(self, destination: &mut Vec<u8>, value: u16) {
            let encoded = match self {
                Self::Little => value.to_le_bytes(),
                Self::Big => value.to_be_bytes(),
            };
            destination.extend_from_slice(&encoded);
        }

        fn push_u32(self, destination: &mut Vec<u8>, value: u32) {
            let encoded = match self {
                Self::Little => value.to_le_bytes(),
                Self::Big => value.to_be_bytes(),
            };
            destination.extend_from_slice(&encoded);
        }
    }

    fn classic_tiff_header() -> Vec<u8> {
        let mut bytes = b"II".to_vec();
        FixtureEndian::Little.push_u16(&mut bytes, 42);
        FixtureEndian::Little.push_u32(&mut bytes, 8);
        bytes
    }

    fn write_preflight_fixture(
        path: &Path,
        entries: &[(u16, u16, u32, u32)],
        next_ifd: u32,
    ) -> std::io::Result<()> {
        let mut bytes = classic_tiff_header();
        FixtureEndian::Little.push_u16(&mut bytes, u16::try_from(entries.len()).unwrap());
        for &(tag, type_code, count, value_or_offset) in entries {
            FixtureEndian::Little.push_u16(&mut bytes, tag);
            FixtureEndian::Little.push_u16(&mut bytes, type_code);
            FixtureEndian::Little.push_u32(&mut bytes, count);
            FixtureEndian::Little.push_u32(&mut bytes, value_or_offset);
        }
        FixtureEndian::Little.push_u32(&mut bytes, next_ifd);
        fs::write(path, bytes)
    }

    fn patch_classic_le_compression(path: &Path, compression: u16) {
        let mut bytes = fs::read(path).unwrap();
        assert_eq!(&bytes[..2], b"II");
        assert_eq!(u16::from_le_bytes(bytes[2..4].try_into().unwrap()), 42);
        let ifd = usize::try_from(u32::from_le_bytes(bytes[4..8].try_into().unwrap())).unwrap();
        let entries = usize::from(u16::from_le_bytes(bytes[ifd..ifd + 2].try_into().unwrap()));
        let mut patched = false;
        for index in 0..entries {
            let entry = ifd + 2 + index * 12;
            if u16::from_le_bytes(bytes[entry..entry + 2].try_into().unwrap()) == 259 {
                assert_eq!(
                    u16::from_le_bytes(bytes[entry + 2..entry + 4].try_into().unwrap()),
                    3
                );
                assert_eq!(
                    u32::from_le_bytes(bytes[entry + 4..entry + 8].try_into().unwrap()),
                    1
                );
                bytes[entry + 8..entry + 10].copy_from_slice(&compression.to_le_bytes());
                patched = true;
                break;
            }
        }
        assert!(patched, "fixture has a Compression field");
        fs::write(path, bytes).unwrap();
    }

    fn push_short_ifd_entry(
        destination: &mut Vec<u8>,
        endian: FixtureEndian,
        tag: u16,
        value: u16,
    ) {
        endian.push_u16(destination, tag);
        endian.push_u16(destination, 3);
        endian.push_u32(destination, 1);
        endian.push_u16(destination, value);
        endian.push_u16(destination, 0);
    }

    fn push_long_ifd_entry(
        destination: &mut Vec<u8>,
        endian: FixtureEndian,
        tag: u16,
        count: u32,
        value_or_offset: u32,
    ) {
        endian.push_u16(destination, tag);
        endian.push_u16(destination, 4);
        endian.push_u32(destination, count);
        endian.push_u32(destination, value_or_offset);
    }

    fn write_tiled_u8(
        path: &Path,
        width: u32,
        height: u32,
        tile_width: u32,
        tile_height: u32,
        values: &[u8],
    ) -> std::io::Result<()> {
        assert_eq!(values.len(), usize::try_from(width * height).unwrap());
        let tiles_across = width.div_ceil(tile_width);
        let tiles_down = height.div_ceil(tile_height);
        let tile_count = tiles_across * tiles_down;
        assert!(tile_count > 1);
        let entry_count = 12_u16;
        let ifd_end = 8_u32 + 2 + u32::from(entry_count) * 12 + 4;
        let offsets_offset = ifd_end;
        let counts_offset = offsets_offset + tile_count * 4;
        let pixels_offset = counts_offset + tile_count * 4;
        let tile_bytes = tile_width * tile_height;

        let endian = FixtureEndian::Little;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"II");
        endian.push_u16(&mut bytes, 42);
        endian.push_u32(&mut bytes, 8);
        endian.push_u16(&mut bytes, entry_count);
        push_long_ifd_entry(&mut bytes, endian, 256, 1, width);
        push_long_ifd_entry(&mut bytes, endian, 257, 1, height);
        push_short_ifd_entry(&mut bytes, endian, 258, 8);
        push_short_ifd_entry(&mut bytes, endian, 259, 1);
        push_short_ifd_entry(&mut bytes, endian, 262, 1);
        push_short_ifd_entry(&mut bytes, endian, 277, 1);
        push_short_ifd_entry(&mut bytes, endian, 284, 1);
        push_long_ifd_entry(&mut bytes, endian, 322, 1, tile_width);
        push_long_ifd_entry(&mut bytes, endian, 323, 1, tile_height);
        push_long_ifd_entry(&mut bytes, endian, 324, tile_count, offsets_offset);
        push_long_ifd_entry(&mut bytes, endian, 325, tile_count, counts_offset);
        push_short_ifd_entry(&mut bytes, endian, 339, 1);
        endian.push_u32(&mut bytes, 0);
        assert_eq!(bytes.len(), usize::try_from(ifd_end).unwrap());

        for tile in 0..tile_count {
            endian.push_u32(&mut bytes, pixels_offset + tile * tile_bytes);
        }
        for _ in 0..tile_count {
            endian.push_u32(&mut bytes, tile_bytes);
        }
        assert_eq!(bytes.len(), usize::try_from(pixels_offset).unwrap());

        for tile_y in 0..tiles_down {
            for tile_x in 0..tiles_across {
                for local_y in 0..tile_height {
                    for local_x in 0..tile_width {
                        let x = tile_x * tile_width + local_x;
                        let y = tile_y * tile_height + local_y;
                        let value = if x < width && y < height {
                            values[usize::try_from(y * width + x).unwrap()]
                        } else {
                            0xfe
                        };
                        bytes.push(value);
                    }
                }
            }
        }
        File::create(path)?.write_all(&bytes)
    }

    fn write_big_endian_striped_u16(
        path: &Path,
        width: u32,
        height: u32,
        rows_per_strip: u32,
        values: &[u16],
    ) -> std::io::Result<()> {
        assert_eq!(values.len(), usize::try_from(width * height).unwrap());
        let strip_count = height.div_ceil(rows_per_strip);
        assert!(strip_count > 1);
        let strip_bytes = width * rows_per_strip * 2;
        let entry_count = 11_u16;
        let ifd_end = 8_u32 + 2 + u32::from(entry_count) * 12 + 4;
        let offsets_offset = ifd_end;
        let counts_offset = offsets_offset + strip_count * 4;
        let pixels_offset = counts_offset + strip_count * 4;

        let endian = FixtureEndian::Big;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"MM");
        endian.push_u16(&mut bytes, 42);
        endian.push_u32(&mut bytes, 8);
        endian.push_u16(&mut bytes, entry_count);
        push_long_ifd_entry(&mut bytes, endian, 256, 1, width);
        push_long_ifd_entry(&mut bytes, endian, 257, 1, height);
        push_short_ifd_entry(&mut bytes, endian, 258, 16);
        push_short_ifd_entry(&mut bytes, endian, 259, 1);
        push_short_ifd_entry(&mut bytes, endian, 262, 1);
        push_long_ifd_entry(&mut bytes, endian, 273, strip_count, offsets_offset);
        push_short_ifd_entry(&mut bytes, endian, 277, 1);
        push_long_ifd_entry(&mut bytes, endian, 278, 1, rows_per_strip);
        push_long_ifd_entry(&mut bytes, endian, 279, strip_count, counts_offset);
        push_short_ifd_entry(&mut bytes, endian, 284, 1);
        push_short_ifd_entry(&mut bytes, endian, 339, 1);
        endian.push_u32(&mut bytes, 0);
        assert_eq!(bytes.len(), usize::try_from(ifd_end).unwrap());

        for strip in 0..strip_count {
            endian.push_u32(&mut bytes, pixels_offset + strip * strip_bytes);
        }
        for _ in 0..strip_count {
            endian.push_u32(&mut bytes, strip_bytes);
        }
        assert_eq!(bytes.len(), usize::try_from(pixels_offset).unwrap());
        for value in values {
            endian.push_u16(&mut bytes, *value);
        }
        File::create(path)?.write_all(&bytes)
    }

    fn write_u16_stack(
        path: &Path,
        width: u32,
        height: u32,
        pages: u32,
        striped: bool,
    ) -> Result<(), tiff::TiffError> {
        write_u16_stack_with_base(path, width, height, pages, striped, 0)
    }

    fn write_u16_stack_with_base(
        path: &Path,
        width: u32,
        height: u32,
        pages: u32,
        striped: bool,
        base: u16,
    ) -> Result<(), tiff::TiffError> {
        let file = File::create(path)?;
        let mut encoder = TiffEncoder::new(file)?;
        for z in 0..pages {
            let values = (0..width * height)
                .map(|index| base + u16::try_from(z * 100 + index).unwrap())
                .collect::<Vec<_>>();
            let mut image = encoder.new_image::<colortype::Gray16>(width, height)?;
            if striped {
                image.rows_per_strip(1)?;
            }
            image.write_data(&values)?;
        }
        Ok(())
    }

    fn write_u16_stack_with_description(
        path: &Path,
        width: u32,
        height: u32,
        pages: u32,
        description: &str,
    ) -> Result<(), tiff::TiffError> {
        let file = File::create(path)?;
        let mut encoder = TiffEncoder::new(file)?;
        for z in 0..pages {
            let values = (0..width * height)
                .map(|index| u16::try_from(z * 100 + index).unwrap())
                .collect::<Vec<_>>();
            let mut image = encoder.new_image::<colortype::Gray16>(width, height)?;
            if z == 0 {
                image
                    .encoder()
                    .write_tag(Tag::ImageDescription, description)?;
            }
            image.write_data(&values)?;
        }
        Ok(())
    }

    fn simple_ome_description(
        size_x: u64,
        size_y: u64,
        size_z: u64,
        size_c: u64,
        size_t: u64,
        tiff_data_attributes: &str,
    ) -> String {
        simple_ome_description_with_type(
            Some("uint16"),
            size_x,
            size_y,
            size_z,
            size_c,
            size_t,
            tiff_data_attributes,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn simple_ome_description_with_type(
        pixel_type: Option<&str>,
        size_x: u64,
        size_y: u64,
        size_z: u64,
        size_c: u64,
        size_t: u64,
        tiff_data_attributes: &str,
    ) -> String {
        let pixel_type = pixel_type
            .map(|pixel_type| format!(r#" Type="{pixel_type}""#))
            .unwrap_or_default();
        format!(
            r#"<OME xmlns="http://www.openmicroscopy.org/Schemas/OME/2016-06"><Image ID="Image:0"><Pixels DimensionOrder="XYZCT"{pixel_type} SizeX="{size_x}" SizeY="{size_y}" SizeZ="{size_z}" SizeC="{size_c}" SizeT="{size_t}"><TiffData {tiff_data_attributes}/></Pixels></Image></OME>"#
        )
    }

    fn write_f32(
        path: &Path,
        values: &[f32],
        description: Option<&str>,
    ) -> Result<(), tiff::TiffError> {
        let file = File::create(path)?;
        let mut encoder = TiffEncoder::new(file)?;
        let mut image = encoder.new_image::<colortype::Gray32Float>(2, 2)?;
        if let Some(description) = description {
            image
                .encoder()
                .write_tag(Tag::ImageDescription, description)?;
        }
        image.write_data(values)
    }
}
