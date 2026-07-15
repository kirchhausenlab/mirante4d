use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufReader, Read, Seek, SeekFrom},
    mem::size_of,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileExt, MetadataExt, OpenOptionsExt},
    },
    path::{Component, Path, PathBuf},
};

use mirante4d_dataset::CpuByteLedger;
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
    ImportCancellation, ImportError, SourceLayout, TiffInspection, TiffSource,
    canonical_cache::CanonicalBaseCache,
    model::{InspectedSourceFile, SourceFileGeneration},
    ordered_workers::{OrderedWorkerDiagnostics, OrderedWorkerPolicy, run_ordered},
};

const HASH_READ_BYTES: usize = 64 * 1024;
const MAX_ENCODED_CHUNK_OVERHEAD_BYTES: usize = 64 * 1024;
// Keep source discovery within the portable provenance record's bounded file list.
const MAX_SOURCE_FILES: usize = 4_096;
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
    sha256: Sha256Digest,
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

#[derive(Debug)]
struct DirectCandidate {
    path: PathBuf,
    relative_name: String,
    channel_label: Option<u64>,
    time_label: Option<u64>,
}

#[derive(Debug)]
enum DirectoryLayout {
    Direct(Vec<PathBuf>),
    ChannelFolders(Vec<(PathBuf, Vec<PathBuf>)>),
}

pub(crate) fn inspect(source: TiffSource) -> Result<TiffInspection, ImportError> {
    inspect_cancellable(source, &ImportCancellation::new())
}

pub(crate) fn inspect_cancellable(
    source: TiffSource,
    cancellation: &ImportCancellation,
) -> Result<TiffInspection, ImportError> {
    check_cancelled(cancellation)?;
    let metadata = match fs::symlink_metadata(&source.path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ImportError::MissingSource(source.path));
        }
        Err(source_error) => {
            return Err(io_error("inspect source", &source.path, source_error));
        }
    };

    if metadata.file_type().is_symlink() {
        return Err(ImportError::UnsupportedSource(
            "the selected TIFF source root must not be a symbolic link".to_owned(),
        ));
    }

    if metadata.is_file() {
        if !is_tiff_path(&source.path) {
            return Err(ImportError::UnsupportedSource(
                "a single source must have a .tif or .tiff extension".to_owned(),
            ));
        }
        if source.layout == SourceLayout::ChannelFoldersOfPlanes {
            return Err(ImportError::UnsupportedSource(
                "channel-folder layout requires a directory source".to_owned(),
            ));
        }
        return inspect_single_file(source, cancellation);
    }

    if !metadata.is_dir() {
        return Err(ImportError::UnsupportedSource(
            "the selected source is neither a regular TIFF file nor a directory".to_owned(),
        ));
    }

    let discovered = discover_directory_layout(&source.path, cancellation)?;
    match (source.layout, discovered) {
        (SourceLayout::Auto | SourceLayout::MultipageStacks, DirectoryLayout::Direct(paths)) => {
            inspect_direct_stacks(source, paths, cancellation)
        }
        (
            SourceLayout::Auto | SourceLayout::ChannelFoldersOfPlanes,
            DirectoryLayout::ChannelFolders(folders),
        ) => inspect_channel_folders(source, folders, cancellation),
        (SourceLayout::MultipageStacks, DirectoryLayout::ChannelFolders(_)) => {
            Err(ImportError::UnsupportedSource(
                "multipage-stack layout does not accept channel folders".to_owned(),
            ))
        }
        (SourceLayout::ChannelFoldersOfPlanes, DirectoryLayout::Direct(_)) => {
            Err(ImportError::UnsupportedSource(
                "channel-folder layout does not accept direct TIFF files".to_owned(),
            ))
        }
    }
}

fn inspect_single_file(
    source: TiffSource,
    cancellation: &ImportCancellation,
) -> Result<TiffInspection, ImportError> {
    check_cancelled(cancellation)?;
    let relative_name = relative_name_for_file(&source.path, &source.path)?;
    let facts = inspect_file(&source.path, cancellation)?;
    let shape = shape4d(1, facts.pages, facts.height, facts.width)?;
    let files = vec![InspectedSourceFile {
        path: source.path.clone(),
        relative_name,
        channel: 0,
        timepoint: 0,
        first_z: 0,
        planes: facts.pages,
        bytes: facts.bytes,
        sha256: facts.sha256,
        generation: facts.generation,
    }];
    check_cancelled(cancellation)?;
    finish_inspection(
        source,
        SourceLayout::MultipageStacks,
        shape,
        1,
        facts.dtype,
        facts.ome_spacing_zyx_um,
        facts.maximum_decoded_chunk_bytes,
        facts.maximum_encoded_chunk_bytes,
        files,
    )
}

fn inspect_direct_stacks(
    source: TiffSource,
    paths: Vec<PathBuf>,
    cancellation: &ImportCancellation,
) -> Result<TiffInspection, ImportError> {
    let mut candidates = Vec::with_capacity(paths.len());
    for path in paths {
        check_cancelled(cancellation)?;
        let relative_name = relative_name_for_file(&source.path, &path)?;
        let filename = path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                ImportError::UnsupportedSource("source TIFF names must be valid UTF-8".to_owned())
            })?
            .to_owned();
        candidates.push(DirectCandidate {
            path,
            relative_name,
            channel_label: parse_unique_numeric_token(&filename, &["channel", "ch"], "channel")?,
            time_label: parse_unique_numeric_token(
                &filename,
                &["stack", "time", "t"],
                "timepoint",
            )?,
        });
    }
    candidates.sort_by(|left, right| left.relative_name.cmp(&right.relative_name));

    let channel_labels =
        consistent_optional_labels(&candidates, |candidate| candidate.channel_label, "channel")?;
    let time_labels =
        consistent_optional_labels(&candidates, |candidate| candidate.time_label, "timepoint")?;
    if candidates.len() > 1 && channel_labels.is_none() && time_labels.is_none() {
        return Err(ImportError::AmbiguousSource(
            "multiple direct TIFF stacks need channel (ch/channel) or timepoint (t/stack/time) numeric filename tokens"
                .to_owned(),
        ));
    }

    let channel_values = sorted_axis_values(channel_labels.as_deref());
    let time_values = sorted_axis_values(time_labels.as_deref());
    let channel_ordinals = ordinal_map(&channel_values)?;
    let time_ordinals = ordinal_map(&time_values)?;
    let channels = u32::try_from(channel_values.len()).map_err(|_| ImportError::Overflow)?;
    let timepoints = u64::try_from(time_values.len()).map_err(|_| ImportError::Overflow)?;

    let mut assigned = Vec::with_capacity(candidates.len());
    let mut occupied = BTreeSet::new();
    for candidate in candidates {
        let channel_label = candidate.channel_label.unwrap_or(0);
        let time_label = candidate.time_label.unwrap_or(0);
        let channel = *channel_ordinals
            .get(&channel_label)
            .expect("the ordinal map contains every observed channel label");
        let timepoint = u64::from(
            *time_ordinals
                .get(&time_label)
                .expect("the ordinal map contains every observed time label"),
        );
        if !occupied.insert((channel, timepoint)) {
            return Err(ImportError::AmbiguousSource(format!(
                "more than one direct TIFF stack maps to channel label {channel_label} and timepoint label {time_label}"
            )));
        }
        assigned.push((channel, timepoint, candidate));
    }

    let expected_assignments = usize::try_from(
        u64::from(channels)
            .checked_mul(timepoints)
            .ok_or(ImportError::Overflow)?,
    )
    .map_err(|_| ImportError::Overflow)?;
    if occupied.len() != expected_assignments {
        return Err(ImportError::AmbiguousSource(
            "direct TIFF stack tokens do not form one complete channel-by-timepoint grid"
                .to_owned(),
        ));
    }
    assigned.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.relative_name.cmp(&right.2.relative_name))
    });

    let mut common: Option<(u64, u64, u64, IntensityDType)> = None;
    let mut spacing = SpacingAccumulator::default();
    let mut maximum_decoded_chunk_bytes = 0;
    let mut maximum_encoded_chunk_bytes = 0;
    let mut files = Vec::with_capacity(assigned.len());
    for (channel, timepoint, candidate) in assigned {
        check_cancelled(cancellation)?;
        let facts = inspect_file(&candidate.path, cancellation)?;
        check_common_stack_facts(
            &candidate.relative_name,
            &mut common,
            facts.width,
            facts.height,
            facts.pages,
            facts.dtype,
        )?;
        spacing.push(&candidate.relative_name, facts.ome_spacing_zyx_um)?;
        maximum_decoded_chunk_bytes =
            maximum_decoded_chunk_bytes.max(facts.maximum_decoded_chunk_bytes);
        maximum_encoded_chunk_bytes =
            maximum_encoded_chunk_bytes.max(facts.maximum_encoded_chunk_bytes);
        files.push(InspectedSourceFile {
            path: candidate.path,
            relative_name: candidate.relative_name,
            channel,
            timepoint,
            first_z: 0,
            planes: facts.pages,
            bytes: facts.bytes,
            sha256: facts.sha256,
            generation: facts.generation,
        });
    }
    let (width, height, pages, dtype) = common.expect("directory discovery rejects no files");
    let shape = shape4d(timepoints, pages, height, width)?;
    check_cancelled(cancellation)?;
    finish_inspection(
        source,
        SourceLayout::MultipageStacks,
        shape,
        channels,
        dtype,
        spacing.finish()?,
        maximum_decoded_chunk_bytes,
        maximum_encoded_chunk_bytes,
        files,
    )
}

fn inspect_channel_folders(
    source: TiffSource,
    mut folders: Vec<(PathBuf, Vec<PathBuf>)>,
    cancellation: &ImportCancellation,
) -> Result<TiffInspection, ImportError> {
    folders.sort_by(|left, right| {
        relative_name_for_directory(&source.path, &left.0)
            .unwrap_or_default()
            .cmp(&relative_name_for_directory(&source.path, &right.0).unwrap_or_default())
    });

    let mut folder_records = Vec::with_capacity(folders.len());
    for (folder, mut planes) in folders {
        check_cancelled(cancellation)?;
        let relative_folder = relative_name_for_directory(&source.path, &folder)?;
        let folder_name = folder
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                ImportError::UnsupportedSource(
                    "channel folder names must be valid UTF-8".to_owned(),
                )
            })?;
        let label = parse_unique_numeric_token(folder_name, &["channel", "ch"], "channel")?;
        planes.sort_by(|left, right| {
            relative_name_for_file(&source.path, left)
                .unwrap_or_default()
                .cmp(&relative_name_for_file(&source.path, right).unwrap_or_default())
        });
        folder_records.push((relative_folder, label, planes));
    }

    let mut expected_plane_names = None;
    for (relative_folder, _, planes) in &folder_records {
        let plane_names = planes
            .iter()
            .map(|path| {
                path.file_name()
                    .and_then(|value| value.to_str())
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| {
                        ImportError::UnsupportedSource(
                            "channel-plane filenames must be valid UTF-8".to_owned(),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(expected) = &expected_plane_names {
            if &plane_names != expected {
                return Err(ImportError::AmbiguousSource(format!(
                    "channel folder {relative_folder:?} does not contain the same plane filenames as the other channels"
                )));
            }
        } else {
            expected_plane_names = Some(plane_names);
        }
    }

    let labels = consistent_folder_labels(&folder_records)?;
    let channel_values = match labels {
        Some(labels) => sorted_axis_values(Some(&labels)),
        None => (0..folder_records.len())
            .map(|index| u64::try_from(index).map_err(|_| ImportError::Overflow))
            .collect::<Result<Vec<_>, _>>()?,
    };
    if channel_values.len() != folder_records.len() {
        return Err(ImportError::AmbiguousSource(
            "more than one channel folder has the same channel numeric token".to_owned(),
        ));
    }
    let channel_ordinals = ordinal_map(&channel_values)?;
    let channels = u32::try_from(channel_values.len()).map_err(|_| ImportError::Overflow)?;

    let mut common_plane: Option<(u64, u64, IntensityDType)> = None;
    let mut expected_planes = None;
    let mut spacing = SpacingAccumulator::default();
    let mut maximum_decoded_chunk_bytes = 0;
    let mut maximum_encoded_chunk_bytes = 0;
    let mut files = Vec::new();
    for (folder_index, (_relative_folder, label, planes)) in folder_records.into_iter().enumerate()
    {
        check_cancelled(cancellation)?;
        let channel_label =
            label.unwrap_or(u64::try_from(folder_index).map_err(|_| ImportError::Overflow)?);
        let channel = *channel_ordinals
            .get(&channel_label)
            .expect("the channel ordinal map contains every channel folder");
        let plane_count = u64::try_from(planes.len()).map_err(|_| ImportError::Overflow)?;
        if let Some(expected) = expected_planes {
            if plane_count != expected {
                return Err(ImportError::AmbiguousSource(format!(
                    "channel {channel} contains {plane_count} planes, expected {expected}"
                )));
            }
        } else {
            expected_planes = Some(plane_count);
        }

        for (z, path) in planes.into_iter().enumerate() {
            check_cancelled(cancellation)?;
            let relative_name = relative_name_for_file(&source.path, &path)?;
            let facts = inspect_file(&path, cancellation)?;
            if facts.pages != 1 {
                return Err(ImportError::UnsupportedSource(format!(
                    "channel-folder TIFF {relative_name:?} has {} pages; every plane file must contain exactly one page",
                    facts.pages
                )));
            }
            check_common_plane_facts(
                &relative_name,
                &mut common_plane,
                facts.width,
                facts.height,
                facts.dtype,
            )?;
            spacing.push(&relative_name, facts.ome_spacing_zyx_um)?;
            maximum_decoded_chunk_bytes =
                maximum_decoded_chunk_bytes.max(facts.maximum_decoded_chunk_bytes);
            maximum_encoded_chunk_bytes =
                maximum_encoded_chunk_bytes.max(facts.maximum_encoded_chunk_bytes);
            files.push(InspectedSourceFile {
                path,
                relative_name,
                channel,
                timepoint: 0,
                first_z: u64::try_from(z).map_err(|_| ImportError::Overflow)?,
                planes: 1,
                bytes: facts.bytes,
                sha256: facts.sha256,
                generation: facts.generation,
            });
        }
    }
    files.sort_by(|left, right| {
        left.channel
            .cmp(&right.channel)
            .then(left.first_z.cmp(&right.first_z))
            .then(left.relative_name.cmp(&right.relative_name))
    });
    let (width, height, dtype) = common_plane.expect("directory discovery rejects no files");
    let z = expected_planes.expect("directory discovery rejects empty channel folders");
    let shape = shape4d(1, z, height, width)?;
    check_cancelled(cancellation)?;
    finish_inspection(
        source,
        SourceLayout::ChannelFoldersOfPlanes,
        shape,
        channels,
        dtype,
        spacing.finish()?,
        maximum_decoded_chunk_bytes,
        maximum_encoded_chunk_bytes,
        files,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_inspection(
    mut source: TiffSource,
    layout: SourceLayout,
    shape: Shape4D,
    channels: u32,
    dtype: IntensityDType,
    ome_spacing_zyx_um: Option<[f64; 3]>,
    maximum_decoded_chunk_bytes: u64,
    maximum_encoded_chunk_bytes: u64,
    mut files: Vec<InspectedSourceFile>,
) -> Result<TiffInspection, ImportError> {
    source.path.shrink_to_fit();
    files.shrink_to_fit();
    for file in &mut files {
        file.path.shrink_to_fit();
        file.relative_name.shrink_to_fit();
    }
    let source_index_working_bytes = source_index_working_bytes(&source, &files)?;
    let source_bytes = files.iter().try_fold(0_u64, |total, file| {
        total.checked_add(file.bytes).ok_or(ImportError::Overflow)
    })?;
    let source_fingerprint = aggregate_fingerprint(
        layout,
        shape,
        channels,
        dtype,
        ome_spacing_zyx_um,
        source_bytes,
        &files,
    )?;
    Ok(TiffInspection {
        source,
        files,
        source_index_working_bytes,
        layout,
        shape,
        channels,
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
    let root_bytes = u64::try_from(source.path.as_os_str().as_bytes().len())
        .map_err(|_| ImportError::Overflow)?;
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
        return Err(ImportError::SourceChanged(inspection.source.path.clone()));
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
        let before = source_file_generation(&path)?;
        if before != expected.generation {
            return Err(ImportError::SourceChanged(path));
        }
        let (bytes, sha256) = hash_file_cancellable(&path, cancellation)?;
        let after = source_file_generation(&path)?;
        counters.source_bytes_read = counters
            .source_bytes_read
            .checked_add(bytes)
            .ok_or(ImportError::Overflow)?;
        counters.tiff_open_count = counters
            .tiff_open_count
            .checked_add(1)
            .ok_or(ImportError::Overflow)?;
        if before != after
            || after != expected.generation
            || bytes != expected.bytes
            || sha256 != expected.sha256
        {
            return Err(ImportError::SourceChanged(path));
        }
        counters.generation.files[recorded_index] = after;
    }

    let source_bytes = inspection.files.iter().try_fold(0_u64, |total, file| {
        total.checked_add(file.bytes).ok_or(ImportError::Overflow)
    })?;
    if source_bytes != inspection.source_bytes {
        return Err(ImportError::SourceChanged(inspection.source.path.clone()));
    }
    let fingerprint = aggregate_fingerprint(
        inspection.layout,
        inspection.shape,
        inspection.channels,
        inspection.dtype,
        inspection.ome_spacing_zyx_um,
        source_bytes,
        &inspection.files,
    )?;
    if fingerprint != inspection.source_fingerprint {
        return Err(ImportError::SourceChanged(inspection.source.path.clone()));
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
        return Err(ImportError::SourceChanged(inspection.source.path.clone()));
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

/// Decodes the inspected source exactly once in native strip/tile order into
/// the durable canonical base checkpoint.
///
/// A resumed import skips the already durable plane prefix. Within every
/// remaining file, the decoder is opened once and each admitted native chunk
/// is read once. The source files remain immutable and are strongly checked
/// again by the pipeline before publication.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_canonical_into_cache(
    inspection: &TiffInspection,
    generation: &SourceGeneration,
    cache: &mut CanonicalBaseCache,
    maximum_decoded_chunk_bytes: u64,
    working_memory_bytes: u64,
    resident_working_bytes: u64,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    mut plane_completed: impl FnMut(u64, u64),
) -> Result<SourceDecodeReport, ImportError> {
    check_cancelled(cancellation)?;
    if generation.files.len() != inspection.files.len() {
        return Err(ImportError::SourceChanged(inspection.source.path.clone()));
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
    let parallel_policy = OrderedWorkerPolicy::for_system(
        working_memory_bytes,
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
        if combined > working_memory_bytes {
            return Err(ImportError::WorkingMemoryExceeded {
                required_bytes: combined,
                budget_bytes: working_memory_bytes,
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
                return Err(ImportError::WorkingMemoryExceeded {
                    required_bytes,
                    budget_bytes: maximum_decoded_chunk_bytes,
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
                return Err(ImportError::WorkingMemoryExceeded {
                    required_bytes: actual_bytes,
                    budget_bytes: maximum_decoded_chunk_bytes,
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
                return Err(ImportError::WorkingMemoryExceeded {
                    required_bytes,
                    budget_bytes: maximum_decoded_chunk_bytes,
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
                return Err(ImportError::WorkingMemoryExceeded {
                    required_bytes: actual_bytes,
                    budget_bytes: maximum_decoded_chunk_bytes,
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
    decoder
        .inner()
        .seek(SeekFrom::Start(0))
        .map_err(|source| io_error("rewind source for inspection hash", path, source))?;
    let (bytes, sha256) = hash_reader_cancellable(path, decoder.inner(), cancellation)?;
    check_cancelled(cancellation)?;
    let opened_after = source_file_generation_from_open_file(path, decoder.inner().get_ref())?;
    let path_after = source_file_generation(path)?;
    if before != opened_after || before != path_after || before.bytes != bytes {
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
        bytes,
        sha256,
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

fn hash_file_cancellable(
    path: &Path,
    cancellation: &ImportCancellation,
) -> Result<(u64, Sha256Digest), ImportError> {
    let before = source_file_generation(path)?;
    let mut file = open_source_file(path, "open source for hashing")?;
    if source_file_generation_from_open_file(path, &file)? != before {
        return Err(ImportError::SourceChanged(path.to_path_buf()));
    }
    let result = hash_reader_cancellable(path, &mut file, cancellation)?;
    if source_file_generation_from_open_file(path, &file)? != before
        || source_file_generation(path)? != before
    {
        return Err(ImportError::SourceChanged(path.to_path_buf()));
    }
    Ok(result)
}

fn hash_reader_cancellable(
    path: &Path,
    mut reader: impl Read,
    cancellation: &ImportCancellation,
) -> Result<(u64, Sha256Digest), ImportError> {
    let mut buffer = [0_u8; HASH_READ_BYTES];
    let mut bytes = 0_u64;
    let mut hasher = Sha256Hasher::new();
    loop {
        check_cancelled(cancellation)?;
        let read = reader
            .read(&mut buffer)
            .map_err(|source| io_error("hash source", path, source))?;
        check_cancelled(cancellation)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(u64::try_from(read).map_err(|_| ImportError::Overflow)?)
            .ok_or(ImportError::Overflow)?;
        hasher.update(&buffer[..read]);
    }
    Ok((bytes, hasher.finalize()))
}

fn discover_directory_layout(
    root: &Path,
    cancellation: &ImportCancellation,
) -> Result<DirectoryLayout, ImportError> {
    let mut direct = Vec::new();
    let mut folders = Vec::new();
    for entry in read_directory(root, cancellation)? {
        check_cancelled(cancellation)?;
        let file_type = entry
            .file_type()
            .map_err(|source| io_error("inspect source entry", &entry.path(), source))?;
        let path = entry.path();
        if file_type.is_symlink() {
            return Err(ImportError::UnsupportedSource(format!(
                "source entry {path:?} must not be a symbolic link"
            )));
        }
        if file_type.is_file() {
            if !is_tiff_path(&path) {
                return Err(ImportError::UnsupportedSource(format!(
                    "source directory contains non-TIFF file {path:?}"
                )));
            }
            direct.push(path);
        } else if file_type.is_dir() {
            folders.push(path);
        } else {
            return Err(ImportError::UnsupportedSource(format!(
                "source entry {path:?} is not a regular file or directory"
            )));
        }
    }
    if direct.is_empty() && folders.is_empty() {
        return Err(ImportError::UnsupportedSource(
            "source directory contains no TIFF files or channel folders".to_owned(),
        ));
    }
    if !direct.is_empty() && !folders.is_empty() {
        return Err(ImportError::AmbiguousSource(
            "direct TIFF stacks are mixed with channel folders".to_owned(),
        ));
    }
    if !direct.is_empty() {
        if direct.len() > MAX_SOURCE_FILES {
            return Err(ImportError::UnsupportedSource(format!(
                "source contains more than {MAX_SOURCE_FILES} TIFF files"
            )));
        }
        direct.sort();
        return Ok(DirectoryLayout::Direct(direct));
    }

    let mut total = 0_usize;
    let mut channel_folders = Vec::with_capacity(folders.len());
    for folder in folders {
        check_cancelled(cancellation)?;
        let mut planes = Vec::new();
        for entry in read_directory(&folder, cancellation)? {
            check_cancelled(cancellation)?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|source| io_error("inspect channel-folder entry", &path, source))?;
            if file_type.is_symlink() || file_type.is_dir() {
                return Err(ImportError::UnsupportedSource(format!(
                    "channel folder {folder:?} must be non-recursive and contain only regular TIFF planes"
                )));
            }
            if !file_type.is_file() || !is_tiff_path(&path) {
                return Err(ImportError::UnsupportedSource(format!(
                    "channel folder {folder:?} contains non-TIFF entry {path:?}"
                )));
            }
            planes.push(path);
            total = total.checked_add(1).ok_or(ImportError::Overflow)?;
            if total > MAX_SOURCE_FILES {
                return Err(ImportError::UnsupportedSource(format!(
                    "source contains more than {MAX_SOURCE_FILES} TIFF files"
                )));
            }
        }
        if planes.is_empty() {
            return Err(ImportError::UnsupportedSource(format!(
                "channel folder {folder:?} contains no TIFF planes"
            )));
        }
        planes.sort();
        channel_folders.push((folder, planes));
    }
    channel_folders.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(DirectoryLayout::ChannelFolders(channel_folders))
}

fn read_directory(
    path: &Path,
    cancellation: &ImportCancellation,
) -> Result<Vec<fs::DirEntry>, ImportError> {
    let directory =
        fs::read_dir(path).map_err(|source| io_error("list source directory", path, source))?;
    let mut entries = Vec::new();
    for entry in directory {
        check_cancelled(cancellation)?;
        entries
            .push(entry.map_err(|source| io_error("read source directory entry", path, source))?);
        if entries.len() > MAX_SOURCE_FILES {
            return Err(ImportError::UnsupportedSource(format!(
                "source directory contains more than {MAX_SOURCE_FILES} entries"
            )));
        }
    }
    Ok(entries)
}

fn enumerate_accepted_layout_files(
    inspection: &TiffInspection,
    cancellation: &ImportCancellation,
) -> Result<Vec<(String, PathBuf)>, ImportError> {
    check_cancelled(cancellation)?;
    let root_metadata = fs::symlink_metadata(&inspection.source.path)
        .map_err(|_| ImportError::SourceChanged(inspection.source.path.clone()))?;
    if root_metadata.file_type().is_symlink() {
        return Err(ImportError::SourceChanged(inspection.source.path.clone()));
    }
    if root_metadata.is_file() {
        if inspection.layout != SourceLayout::MultipageStacks || inspection.files.len() != 1 {
            return Err(ImportError::SourceChanged(inspection.source.path.clone()));
        }
        let relative = relative_name_for_file(&inspection.source.path, &inspection.source.path)
            .map_err(|_| ImportError::SourceChanged(inspection.source.path.clone()))?;
        return Ok(vec![(relative, inspection.source.path.clone())]);
    }
    if !root_metadata.is_dir() {
        return Err(ImportError::SourceChanged(inspection.source.path.clone()));
    }
    let discovered = match discover_directory_layout(&inspection.source.path, cancellation) {
        Ok(discovered) => discovered,
        Err(ImportError::Cancelled) => return Err(ImportError::Cancelled),
        Err(_) => return Err(ImportError::SourceChanged(inspection.source.path.clone())),
    };
    let paths = match (inspection.layout, discovered) {
        (SourceLayout::MultipageStacks, DirectoryLayout::Direct(paths)) => paths,
        (SourceLayout::ChannelFoldersOfPlanes, DirectoryLayout::ChannelFolders(folders)) => {
            folders.into_iter().flat_map(|(_, planes)| planes).collect()
        }
        _ => return Err(ImportError::SourceChanged(inspection.source.path.clone())),
    };
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        check_cancelled(cancellation)?;
        let relative = relative_name_for_file(&inspection.source.path, &path)
            .map_err(|_| ImportError::SourceChanged(path.clone()))?;
        files.push((relative, path));
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    if files.windows(2).any(|window| window[0].0 == window[1].0) {
        return Err(ImportError::SourceChanged(inspection.source.path.clone()));
    }
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

fn relative_name_for_file(root: &Path, path: &Path) -> Result<String, ImportError> {
    if root == path && root.is_file() {
        let name = path.file_name().ok_or_else(|| {
            ImportError::UnsupportedSource("single TIFF path has no file name".to_owned())
        })?;
        return path_components_to_string(Path::new(name));
    }
    let relative = path.strip_prefix(root).map_err(|_| {
        ImportError::UnsupportedSource("source TIFF escaped its selected root".to_owned())
    })?;
    path_components_to_string(relative)
}

fn relative_name_for_directory(root: &Path, path: &Path) -> Result<String, ImportError> {
    let relative = path.strip_prefix(root).map_err(|_| {
        ImportError::UnsupportedSource("source folder escaped its selected root".to_owned())
    })?;
    path_components_to_string(relative)
}

fn path_components_to_string(path: &Path) -> Result<String, ImportError> {
    let mut components = Vec::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(ImportError::UnsupportedSource(
                "source-relative names may contain only normal path components".to_owned(),
            ));
        };
        components.push(component.to_str().ok_or_else(|| {
            ImportError::UnsupportedSource("source names must be valid UTF-8".to_owned())
        })?);
    }
    if components.is_empty() {
        return Err(ImportError::UnsupportedSource(
            "source-relative name must not be empty".to_owned(),
        ));
    }
    Ok(components.join("/"))
}

fn parse_unique_numeric_token(
    filename: &str,
    tokens: &[&str],
    axis: &str,
) -> Result<Option<u64>, ImportError> {
    let lowercase = filename.to_ascii_lowercase();
    let bytes = lowercase.as_bytes();
    let mut matches = Vec::new();
    for index in 0..bytes.len() {
        if index > 0 && bytes[index - 1].is_ascii_alphanumeric() {
            continue;
        }
        for token in tokens {
            let token_bytes = token.as_bytes();
            if !bytes[index..].starts_with(token_bytes) {
                continue;
            }
            let mut digit_start = index + token_bytes.len();
            while digit_start < bytes.len()
                && matches!(bytes[digit_start], b'_' | b'-' | b'=' | b'.' | b' ')
            {
                digit_start += 1;
            }
            let mut end = digit_start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end == digit_start || (end < bytes.len() && bytes[end].is_ascii_alphanumeric()) {
                continue;
            }
            let value = lowercase[digit_start..end].parse::<u64>().map_err(|_| {
                ImportError::AmbiguousSource(format!(
                    "{axis} numeric token in filename {filename:?} is out of range"
                ))
            })?;
            matches.push(value);
        }
    }
    if matches.len() > 1 {
        return Err(ImportError::AmbiguousSource(format!(
            "filename {filename:?} contains more than one {axis} numeric token"
        )));
    }
    Ok(matches.into_iter().next())
}

fn consistent_optional_labels(
    candidates: &[DirectCandidate],
    label: impl Fn(&DirectCandidate) -> Option<u64>,
    axis: &str,
) -> Result<Option<Vec<u64>>, ImportError> {
    let labels = candidates.iter().map(label).collect::<Vec<_>>();
    let present = labels.iter().filter(|label| label.is_some()).count();
    if present != 0 && present != labels.len() {
        return Err(ImportError::AmbiguousSource(format!(
            "{axis} tokens are present in only some direct TIFF filenames"
        )));
    }
    Ok((present != 0).then(|| labels.into_iter().flatten().collect()))
}

fn consistent_folder_labels(
    folders: &[(String, Option<u64>, Vec<PathBuf>)],
) -> Result<Option<Vec<u64>>, ImportError> {
    let labels = folders
        .iter()
        .map(|(_, label, _)| *label)
        .collect::<Vec<_>>();
    let present = labels.iter().filter(|label| label.is_some()).count();
    if present != 0 && present != labels.len() {
        return Err(ImportError::AmbiguousSource(
            "channel numeric tokens are present in only some channel-folder names".to_owned(),
        ));
    }
    Ok((present != 0).then(|| labels.into_iter().flatten().collect()))
}

fn sorted_axis_values(labels: Option<&[u64]>) -> Vec<u64> {
    let mut values = labels.map_or_else(|| vec![0], ToOwned::to_owned);
    values.sort_unstable();
    values.dedup();
    values
}

fn ordinal_map(values: &[u64]) -> Result<BTreeMap<u64, u32>, ImportError> {
    values
        .iter()
        .enumerate()
        .map(|(ordinal, value)| {
            Ok((
                *value,
                u32::try_from(ordinal).map_err(|_| ImportError::Overflow)?,
            ))
        })
        .collect()
}

fn check_common_stack_facts(
    relative_name: &str,
    common: &mut Option<(u64, u64, u64, IntensityDType)>,
    width: u64,
    height: u64,
    pages: u64,
    dtype: IntensityDType,
) -> Result<(), ImportError> {
    let facts = (width, height, pages, dtype);
    if let Some(expected) = *common {
        if facts != expected {
            return Err(ImportError::UnsupportedSource(format!(
                "direct TIFF stack {relative_name:?} does not match the common dimensions, page count, and dtype"
            )));
        }
    } else {
        *common = Some(facts);
    }
    Ok(())
}

fn check_common_plane_facts(
    relative_name: &str,
    common: &mut Option<(u64, u64, IntensityDType)>,
    width: u64,
    height: u64,
    dtype: IntensityDType,
) -> Result<(), ImportError> {
    let facts = (width, height, dtype);
    if let Some(expected) = *common {
        if facts != expected {
            return Err(ImportError::UnsupportedSource(format!(
                "TIFF plane {relative_name:?} does not match the common dimensions and dtype"
            )));
        }
    } else {
        *common = Some(facts);
    }
    Ok(())
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
    layout: SourceLayout,
    shape: Shape4D,
    channels: u32,
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
    hasher.update(b"mirante4d-import-source-v1\0");
    hasher.update([match layout {
        SourceLayout::Auto => 0,
        SourceLayout::MultipageStacks => 1,
        SourceLayout::ChannelFoldersOfPlanes => 2,
    }]);
    for dimension in shape.dimensions() {
        hasher.update(dimension.to_le_bytes());
    }
    hasher.update(channels.to_le_bytes());
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
        hasher.update(file.sha256.as_bytes());
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
        io::{Cursor, Write},
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

    struct CancelAfterFirstRead {
        source: Cursor<Vec<u8>>,
        cancellation: ImportCancellation,
    }

    impl std::io::Read for CancelAfterFirstRead {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = std::io::Read::read(&mut self.source, buffer)?;
            if read > 0 {
                self.cancellation.cancel();
            }
            Ok(read)
        }
    }

    #[test]
    fn source_hashing_stops_between_bounded_reads() {
        let cancellation = ImportCancellation::new();
        let reader = CancelAfterFirstRead {
            source: Cursor::new(vec![7; HASH_READ_BYTES * 2]),
            cancellation: cancellation.clone(),
        };
        assert!(matches!(
            hash_reader_cancellable(Path::new("source.tif"), reader, &cancellation),
            Err(ImportError::Cancelled)
        ));
    }

    #[test]
    fn single_stack_inspection_and_canonical_decode_are_exact_and_bounded() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("stack.tif");
        write_u16_stack(&path, 4, 3, 2, true).unwrap();

        let inspection = inspect(TiffSource::auto(&path)).unwrap();
        assert_eq!(inspection.layout, SourceLayout::MultipageStacks);
        assert_eq!(inspection.shape.dimensions(), [1, 2, 3, 4]);
        assert_eq!(inspection.channels, 1);
        assert_eq!(inspection.dtype, IntensityDType::Uint16);
        assert_eq!(inspection.files.len(), 1);
        assert_eq!(inspection.files[0].planes, 2);
        assert_eq!(inspection.source_bytes, fs::metadata(&path).unwrap().len());
        assert_eq!(inspection.maximum_decoded_chunk_bytes, 8);
        assert_eq!(
            inspection.files[0].sha256,
            hash_file_cancellable(&path, &ImportCancellation::new())
                .unwrap()
                .1
        );
        let revalidation = revalidate(&inspection, &ImportCancellation::new()).unwrap();
        assert_eq!(revalidation.source_bytes_read, inspection.source_bytes);
        assert_eq!(revalidation.tiff_open_count, 1);

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
            ImportError::WorkingMemoryExceeded {
                required_bytes: 8,
                budget_bytes: 7
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
        let inspection = inspect(TiffSource::auto(&path)).unwrap();
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
    fn direct_stack_tokens_form_a_dense_deterministic_mapping_without_absolute_paths() {
        let first = tempdir().unwrap();
        let second = tempdir().unwrap();
        let first_root = first.path().join("source");
        let second_root = second.path().join("source");
        fs::create_dir(&first_root).unwrap();
        fs::create_dir(&second_root).unwrap();
        for (filename, base) in [
            ("sample_ch10_time9.tif", 190),
            ("sample_ch2_time5.tif", 25),
            ("sample_ch10_time5.tif", 150),
            ("sample_ch2_time9.tif", 29),
        ] {
            write_u16_stack_with_base(&first_root.join(filename), 2, 2, 1, false, base).unwrap();
            fs::copy(first_root.join(filename), second_root.join(filename)).unwrap();
        }

        let first_inspection = inspect(TiffSource::auto(&first_root)).unwrap();
        let second_inspection = inspect(TiffSource::auto(&second_root)).unwrap();
        assert_eq!(first_inspection.shape.dimensions(), [2, 1, 2, 2]);
        assert_eq!(first_inspection.channels, 2);
        assert_eq!(
            first_inspection.source_fingerprint,
            second_inspection.source_fingerprint
        );
        let mapping = first_inspection
            .files
            .iter()
            .map(|file| (file.relative_name.as_str(), file.channel, file.timepoint))
            .collect::<BTreeSet<_>>();
        assert!(mapping.contains(&("sample_ch2_time5.tif", 0, 0)));
        assert!(mapping.contains(&("sample_ch2_time9.tif", 0, 1)));
        assert!(mapping.contains(&("sample_ch10_time5.tif", 1, 0)));
        assert!(mapping.contains(&("sample_ch10_time9.tif", 1, 1)));
        revalidate(&first_inspection, &ImportCancellation::new()).unwrap();

        write_u16_stack(&first_root.join("sample_ch2_time11.tif"), 2, 2, 1, false).unwrap();
        assert!(matches!(
            revalidate(&first_inspection, &ImportCancellation::new()),
            Err(ImportError::SourceChanged(_))
        ));
    }

    #[test]
    fn ambiguous_direct_grouping_and_duplicate_tokens_are_rejected() {
        let temporary = tempdir().unwrap();
        write_u16_stack(&temporary.path().join("alpha.tif"), 2, 2, 1, false).unwrap();
        write_u16_stack(&temporary.path().join("beta.tif"), 2, 2, 1, false).unwrap();
        assert!(matches!(
            inspect(TiffSource::auto(temporary.path())),
            Err(ImportError::AmbiguousSource(_))
        ));

        let duplicate = tempdir().unwrap();
        write_u16_stack(
            &duplicate.path().join("sample_ch1_channel2_t0.tif"),
            2,
            2,
            1,
            false,
        )
        .unwrap();
        assert!(matches!(
            inspect(TiffSource::auto(duplicate.path())),
            Err(ImportError::AmbiguousSource(_))
        ));
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

        let inspection = inspect(TiffSource::auto(temporary.path())).unwrap();
        assert_eq!(inspection.layout, SourceLayout::ChannelFoldersOfPlanes);
        assert_eq!(inspection.shape.dimensions(), [1, 2, 2, 3]);
        assert_eq!(inspection.channels, 2);
        assert!(inspection.files[0].relative_name.starts_with("channel2/"));

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
        assert!(matches!(
            inspect(TiffSource::auto(temporary.path())),
            Err(ImportError::UnsupportedSource(_))
        ));
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
        let inspection = inspect(TiffSource::auto(&source)).unwrap();
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
    fn channel_folders_require_matching_plane_filenames() {
        let temporary = tempdir().unwrap();
        let channel_zero = temporary.path().join("channel0");
        let channel_one = temporary.path().join("channel1");
        fs::create_dir(&channel_zero).unwrap();
        fs::create_dir(&channel_one).unwrap();
        for filename in ["z00.tif", "z01.tif"] {
            write_u16_stack(&channel_zero.join(filename), 2, 2, 1, false).unwrap();
        }
        for filename in ["z00.tif", "z02.tif"] {
            write_u16_stack(&channel_one.join(filename), 2, 2, 1, false).unwrap();
        }

        assert!(matches!(
            inspect(TiffSource::auto(temporary.path())),
            Err(ImportError::AmbiguousSource(message))
                if message.contains("same plane filenames")
        ));
    }

    #[test]
    fn complete_ome_spacing_is_canonical_zyx_and_nonfinite_f32_is_rejected_on_decode() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("float.ome.tif");
        let description = r#"<OME xmlns="http://www.openmicroscopy.org/Schemas/OME/2016-06"><Image ID="Image:0"><Pixels DimensionOrder="XYZCT" Type="float" SizeX="2" SizeY="2" SizeZ="1" SizeC="1" SizeT="1" PhysicalSizeX="200" PhysicalSizeXUnit="nm" PhysicalSizeY="0.3" PhysicalSizeYUnit="um" PhysicalSizeZ="0.0007" PhysicalSizeZUnit="mm"><TiffData IFD="0" PlaneCount="1"/></Pixels></Image></OME>"#;
        write_f32(&path, &[1.0, f32::NAN, -0.0, 4.0], Some(description)).unwrap();

        let inspection = inspect(TiffSource::auto(&path)).unwrap();
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
        let inspection = inspect(TiffSource::auto(&accepted)).unwrap();
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
                inspect(TiffSource::auto(path)),
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
                inspect(TiffSource::auto(path)),
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
            inspect(TiffSource::auto(path)),
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

        let inspection = inspect(TiffSource::auto(&path)).unwrap();
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

        let inspection = inspect(TiffSource::auto(&path)).unwrap();
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

            let inspection = inspect(TiffSource::auto(&path)).unwrap();
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

        let inspection = inspect(TiffSource::auto(&path)).unwrap();
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
                inspect(TiffSource::auto(&path)),
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

        let inspection = inspect(TiffSource::auto(&path)).unwrap();
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

        let inspection = inspect(TiffSource::auto(&path)).unwrap();
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
