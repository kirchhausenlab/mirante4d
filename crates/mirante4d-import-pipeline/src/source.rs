use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufReader, Read, Seek},
    path::{Component, Path, PathBuf},
};

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
    model::InspectedSourceFile,
};

const HASH_READ_BYTES: usize = 64 * 1024;
const MAX_ENCODED_CHUNK_OVERHEAD_BYTES: usize = 64 * 1024;
// Keep source discovery within the portable provenance record's bounded file list.
const MAX_SOURCE_FILES: usize = 4_096;
const MAX_PAGES_PER_FILE: u64 = 1_000_000;
const MAX_OME_XML_BYTES: usize = 8 * 1024 * 1024;
const MAX_OME_XML_EVENTS: usize = 65_536;
const MAX_OME_XML_DEPTH: usize = 64;
const MAX_OME_XML_ATTRIBUTES: usize = 256;
const MAX_OME_XML_ATTRIBUTE_BYTES: usize = 1024 * 1024;
const MAX_OME_XML_VALUE_BYTES: usize = 256;

/// Conservative non-pixel allocation ceiling while one TIFF region is read.
pub(crate) const SOURCE_DECODE_OVERHEAD_BYTES_MAX: u64 =
    (MAX_OME_XML_BYTES + HASH_READ_BYTES + MAX_ENCODED_CHUNK_OVERHEAD_BYTES) as u64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SourceReadCounters {
    pub(crate) source_bytes_read: u64,
    pub(crate) decoded_bytes: u64,
}

#[derive(Debug)]
struct TiffFileFacts {
    width: u64,
    height: u64,
    pages: u64,
    dtype: IntensityDType,
    ome_spacing_zyx_um: Option<[f64; 3]>,
    maximum_decoded_chunk_bytes: u64,
    bytes: u64,
    sha256: Sha256Digest,
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
        files.push(InspectedSourceFile {
            path: candidate.path,
            relative_name: candidate.relative_name,
            channel,
            timepoint,
            first_z: 0,
            planes: facts.pages,
            bytes: facts.bytes,
            sha256: facts.sha256,
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
            files.push(InspectedSourceFile {
                path,
                relative_name,
                channel,
                timepoint: 0,
                first_z: u64::try_from(z).map_err(|_| ImportError::Overflow)?,
                planes: 1,
                bytes: facts.bytes,
                sha256: facts.sha256,
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
        files,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_inspection(
    source: TiffSource,
    layout: SourceLayout,
    shape: Shape4D,
    channels: u32,
    dtype: IntensityDType,
    ome_spacing_zyx_um: Option<[f64; 3]>,
    maximum_decoded_chunk_bytes: u64,
    files: Vec<InspectedSourceFile>,
) -> Result<TiffInspection, ImportError> {
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
        layout,
        shape,
        channels,
        dtype,
        ome_spacing_zyx_um,
        source_bytes,
        source_fingerprint,
        maximum_decoded_chunk_bytes,
    })
}

pub(crate) fn revalidate(
    inspection: &TiffInspection,
    cancellation: &ImportCancellation,
) -> Result<(), ImportError> {
    check_cancelled(cancellation)?;
    let current_files = enumerate_accepted_layout_files(inspection, cancellation)?;
    let recorded = inspection
        .files
        .iter()
        .map(|file| (file.relative_name.as_str(), file))
        .collect::<BTreeMap<_, _>>();

    if current_files.len() != recorded.len() {
        return Err(ImportError::SourceChanged(inspection.source.path.clone()));
    }

    let refreshed = inspection.files.clone();
    for (relative_name, path) in current_files {
        let Some(expected) = recorded.get(relative_name.as_str()) else {
            return Err(ImportError::SourceChanged(path));
        };
        let (bytes, sha256) = hash_file_cancellable(&path, cancellation)?;
        if bytes != expected.bytes || sha256 != expected.sha256 {
            return Err(ImportError::SourceChanged(path));
        }
    }

    let source_bytes = refreshed.iter().try_fold(0_u64, |total, file| {
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
        &refreshed,
    )?;
    if fingerprint != inspection.source_fingerprint {
        return Err(ImportError::SourceChanged(inspection.source.path.clone()));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn read_region_into(
    inspection: &TiffInspection,
    channel: u32,
    timepoint: u64,
    origin_zyx: [u64; 3],
    extent_zyx: [u64; 3],
    destination_le: &mut [u8],
    maximum_decoded_chunk_bytes: u64,
    cancellation: &ImportCancellation,
) -> Result<SourceReadCounters, ImportError> {
    check_cancelled(cancellation)?;
    validate_region_request(
        inspection,
        channel,
        timepoint,
        origin_zyx,
        extent_zyx,
        destination_le.len(),
    )?;

    let end_z = origin_zyx[0]
        .checked_add(extent_zyx[0])
        .ok_or(ImportError::Overflow)?;
    let mut matching = inspection
        .files
        .iter()
        .filter(|file| file.channel == channel && file.timepoint == timepoint)
        .filter(|file| {
            let file_end = file.first_z.saturating_add(file.planes);
            file.first_z < end_z && file_end > origin_zyx[0]
        })
        .collect::<Vec<_>>();
    matching.sort_by(|left, right| {
        left.first_z
            .cmp(&right.first_z)
            .then(left.relative_name.cmp(&right.relative_name))
    });

    let mut next_z = origin_zyx[0];
    let mut counters = SourceReadCounters::default();
    for file in matching {
        let file_end = file
            .first_z
            .checked_add(file.planes)
            .ok_or(ImportError::Overflow)?;
        let read_start = file.first_z.max(origin_zyx[0]);
        let read_end = file_end.min(end_z);
        if read_start != next_z || read_end <= read_start {
            return Err(ImportError::UnsupportedSource(
                "the inspected source does not cover the requested z region exactly once"
                    .to_owned(),
            ));
        }
        read_file_region(
            inspection,
            file,
            read_start,
            read_end,
            origin_zyx,
            extent_zyx,
            destination_le,
            maximum_decoded_chunk_bytes,
            cancellation,
            &mut counters,
        )?;
        next_z = read_end;
    }
    if next_z != end_z {
        return Err(ImportError::UnsupportedSource(
            "the inspected source does not cover the requested z region".to_owned(),
        ));
    }
    Ok(counters)
}

#[allow(clippy::too_many_arguments)]
fn read_file_region(
    inspection: &TiffInspection,
    file: &InspectedSourceFile,
    read_start_z: u64,
    read_end_z: u64,
    origin_zyx: [u64; 3],
    extent_zyx: [u64; 3],
    destination_le: &mut [u8],
    maximum_decoded_chunk_bytes: u64,
    cancellation: &ImportCancellation,
    counters: &mut SourceReadCounters,
) -> Result<(), ImportError> {
    check_cancelled(cancellation)?;
    let metadata = fs::metadata(&file.path)
        .map_err(|source| io_error("stat source before decode", &file.path, source))?;
    if !metadata.is_file() || metadata.len() != file.bytes {
        return Err(ImportError::SourceChanged(file.path.clone()));
    }
    let raw = File::open(&file.path)
        .map_err(|source| io_error("open source for decode", &file.path, source))?;
    let counting = CountingReader::new(raw);
    let reader = BufReader::with_capacity(HASH_READ_BYTES, counting);
    let decode_limit = usize::try_from(maximum_decoded_chunk_bytes).unwrap_or(usize::MAX);
    let mut limits = Limits::default();
    // `tiff` also applies this field to bounded IFD arrays (for example a
    // strip-offset table). The explicit layout check below remains the source
    // chunk allocation gate, while this floor permits already-bounded metadata.
    limits.decoding_buffer_size = decode_limit.max(MAX_OME_XML_BYTES);
    limits.intermediate_buffer_size = decode_limit.saturating_add(MAX_ENCODED_CHUNK_OVERHEAD_BYTES);
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
        if global_z >= read_start_z && global_z < read_end_z {
            validate_current_page(
                &file.path,
                &mut decoder,
                inspection.shape.x(),
                inspection.shape.y(),
                inspection.dtype,
            )?;
            decode_page_region(
                &file.path,
                &mut decoder,
                inspection.dtype,
                global_z,
                origin_zyx,
                extent_zyx,
                destination_le,
                maximum_decoded_chunk_bytes,
                cancellation,
                counters,
            )?;
        }
        if global_z + 1 >= read_end_z {
            break;
        }
        if local_page + 1 < file.planes {
            decoder
                .next_image()
                .map_err(|error| tiff_error(&file.path, error))?;
        }
    }
    let bytes_read = decoder.inner().get_ref().bytes_read;
    counters.source_bytes_read = counters
        .source_bytes_read
        .checked_add(bytes_read)
        .ok_or(ImportError::Overflow)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_page_region<R: Read + Seek>(
    path: &Path,
    decoder: &mut Decoder<R>,
    dtype: IntensityDType,
    global_z: u64,
    origin_zyx: [u64; 3],
    extent_zyx: [u64; 3],
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
    let request_y_end = origin_zyx[1]
        .checked_add(extent_zyx[1])
        .ok_or(ImportError::Overflow)?;
    let request_x_end = origin_zyx[2]
        .checked_add(extent_zyx[2])
        .ok_or(ImportError::Overflow)?;

    for chunk_y in 0..chunks_down {
        for chunk_x in 0..chunks_across {
            let chunk_index = chunk_y
                .checked_mul(chunks_across)
                .and_then(|base| base.checked_add(chunk_x))
                .ok_or(ImportError::Overflow)?;
            let (data_width, data_height) = decoder.chunk_data_dimensions(chunk_index);
            let chunk_x_start = u64::from(chunk_x) * u64::from(chunk_width);
            let chunk_y_start = u64::from(chunk_y) * u64::from(chunk_height);
            let chunk_x_end = chunk_x_start
                .checked_add(u64::from(data_width))
                .ok_or(ImportError::Overflow)?;
            let chunk_y_end = chunk_y_start
                .checked_add(u64::from(data_height))
                .ok_or(ImportError::Overflow)?;
            let copy_x_start = chunk_x_start.max(origin_zyx[2]);
            let copy_y_start = chunk_y_start.max(origin_zyx[1]);
            let copy_x_end = chunk_x_end.min(request_x_end);
            let copy_y_end = chunk_y_end.min(request_y_end);
            if copy_x_start >= copy_x_end || copy_y_start >= copy_y_end {
                continue;
            }

            check_cancelled(cancellation)?;
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
            let decoded = decoder
                .read_chunk(chunk_index)
                .map_err(|error| tiff_error(path, error))?;
            check_cancelled(cancellation)?;
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
            copy_decoded_intersection(
                path,
                dtype,
                decoded,
                data_width,
                data_height,
                chunk_x_start,
                chunk_y_start,
                global_z,
                copy_x_start,
                copy_x_end,
                copy_y_start,
                copy_y_end,
                origin_zyx,
                extent_zyx,
                destination_le,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn copy_decoded_intersection(
    path: &Path,
    dtype: IntensityDType,
    decoded: DecodingResult,
    data_width: u32,
    data_height: u32,
    chunk_x_start: u64,
    chunk_y_start: u64,
    global_z: u64,
    copy_x_start: u64,
    copy_x_end: u64,
    copy_y_start: u64,
    copy_y_end: u64,
    origin_zyx: [u64; 3],
    extent_zyx: [u64; 3],
    destination_le: &mut [u8],
) -> Result<(), ImportError> {
    let expected_samples = usize::try_from(
        u64::from(data_width)
            .checked_mul(u64::from(data_height))
            .ok_or(ImportError::Overflow)?,
    )
    .map_err(|_| ImportError::Overflow)?;
    match (dtype, decoded) {
        (IntensityDType::Uint8, DecodingResult::U8(values)) => {
            if values.len() != expected_samples {
                return Err(chunk_size_error(path, values.len(), expected_samples));
            }
            for y in copy_y_start..copy_y_end {
                let source = source_row_range(
                    chunk_x_start,
                    chunk_y_start,
                    data_width,
                    y,
                    copy_x_start,
                    copy_x_end,
                )?;
                let destination = destination_row_range(
                    global_z,
                    y,
                    copy_x_start,
                    copy_x_end,
                    origin_zyx,
                    extent_zyx,
                    1,
                )?;
                destination_le[destination].copy_from_slice(&values[source]);
            }
        }
        (IntensityDType::Uint16, DecodingResult::U16(values)) => {
            if values.len() != expected_samples {
                return Err(chunk_size_error(path, values.len(), expected_samples));
            }
            copy_numeric_rows(
                &values,
                destination_le,
                data_width,
                chunk_x_start,
                chunk_y_start,
                global_z,
                copy_x_start,
                copy_x_end,
                copy_y_start,
                copy_y_end,
                origin_zyx,
                extent_zyx,
                2,
                |value, output| output.copy_from_slice(&value.to_le_bytes()),
            )?;
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
            copy_numeric_rows(
                &values,
                destination_le,
                data_width,
                chunk_x_start,
                chunk_y_start,
                global_z,
                copy_x_start,
                copy_x_end,
                copy_y_start,
                copy_y_end,
                origin_zyx,
                extent_zyx,
                4,
                |value, output| output.copy_from_slice(&value.to_bits().to_le_bytes()),
            )?;
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
fn copy_numeric_rows<T: Copy>(
    values: &[T],
    destination_le: &mut [u8],
    data_width: u32,
    chunk_x_start: u64,
    chunk_y_start: u64,
    global_z: u64,
    copy_x_start: u64,
    copy_x_end: u64,
    copy_y_start: u64,
    copy_y_end: u64,
    origin_zyx: [u64; 3],
    extent_zyx: [u64; 3],
    bytes_per_sample: u64,
    encode: impl Fn(T, &mut [u8]),
) -> Result<(), ImportError> {
    for y in copy_y_start..copy_y_end {
        let source = source_row_range(
            chunk_x_start,
            chunk_y_start,
            data_width,
            y,
            copy_x_start,
            copy_x_end,
        )?;
        let destination = destination_row_range(
            global_z,
            y,
            copy_x_start,
            copy_x_end,
            origin_zyx,
            extent_zyx,
            bytes_per_sample,
        )?;
        let mut output = destination.start;
        for value in &values[source] {
            let end = output
                .checked_add(usize::try_from(bytes_per_sample).map_err(|_| ImportError::Overflow)?)
                .ok_or(ImportError::Overflow)?;
            encode(*value, &mut destination_le[output..end]);
            output = end;
        }
        debug_assert_eq!(output, destination.end);
    }
    Ok(())
}

fn source_row_range(
    chunk_x_start: u64,
    chunk_y_start: u64,
    data_width: u32,
    y: u64,
    copy_x_start: u64,
    copy_x_end: u64,
) -> Result<std::ops::Range<usize>, ImportError> {
    let row = y.checked_sub(chunk_y_start).ok_or(ImportError::Overflow)?;
    let x = copy_x_start
        .checked_sub(chunk_x_start)
        .ok_or(ImportError::Overflow)?;
    let start = row
        .checked_mul(u64::from(data_width))
        .and_then(|value| value.checked_add(x))
        .ok_or(ImportError::Overflow)?;
    let len = copy_x_end
        .checked_sub(copy_x_start)
        .ok_or(ImportError::Overflow)?;
    let end = start.checked_add(len).ok_or(ImportError::Overflow)?;
    Ok(usize::try_from(start).map_err(|_| ImportError::Overflow)?
        ..usize::try_from(end).map_err(|_| ImportError::Overflow)?)
}

fn destination_row_range(
    global_z: u64,
    y: u64,
    copy_x_start: u64,
    copy_x_end: u64,
    origin_zyx: [u64; 3],
    extent_zyx: [u64; 3],
    bytes_per_sample: u64,
) -> Result<std::ops::Range<usize>, ImportError> {
    let z = global_z
        .checked_sub(origin_zyx[0])
        .ok_or(ImportError::Overflow)?;
    let local_y = y.checked_sub(origin_zyx[1]).ok_or(ImportError::Overflow)?;
    let local_x = copy_x_start
        .checked_sub(origin_zyx[2])
        .ok_or(ImportError::Overflow)?;
    let sample_start = z
        .checked_mul(extent_zyx[1])
        .and_then(|value| value.checked_add(local_y))
        .and_then(|value| value.checked_mul(extent_zyx[2]))
        .and_then(|value| value.checked_add(local_x))
        .ok_or(ImportError::Overflow)?;
    let sample_len = copy_x_end
        .checked_sub(copy_x_start)
        .ok_or(ImportError::Overflow)?;
    let byte_start = sample_start
        .checked_mul(bytes_per_sample)
        .ok_or(ImportError::Overflow)?;
    let byte_end = sample_start
        .checked_add(sample_len)
        .and_then(|value| value.checked_mul(bytes_per_sample))
        .ok_or(ImportError::Overflow)?;
    Ok(
        usize::try_from(byte_start).map_err(|_| ImportError::Overflow)?
            ..usize::try_from(byte_end).map_err(|_| ImportError::Overflow)?,
    )
}

fn inspect_file(
    path: &Path,
    cancellation: &ImportCancellation,
) -> Result<TiffFileFacts, ImportError> {
    check_cancelled(cancellation)?;
    ensure_regular_tiff_file(path)?;
    let before = fs::metadata(path)
        .map_err(|source| io_error("stat source before inspection", path, source))?;
    let raw =
        File::open(path).map_err(|source| io_error("open source for inspection", path, source))?;
    let reader = BufReader::with_capacity(HASH_READ_BYTES, raw);
    let mut limits = Limits::default();
    limits.ifd_value_size = MAX_OME_XML_BYTES;
    let mut decoder = Decoder::new(reader)
        .map_err(|error| tiff_error(path, error))?
        .with_limits(limits);
    let mut ome_pixels = None;
    let mut expected = None;
    let mut pages = 0_u64;
    let mut maximum_decoded_chunk_bytes = 0_u64;
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
        check_cancelled(cancellation)?;
        if decoder.more_images() {
            decoder
                .next_image()
                .map_err(|error| tiff_error(path, error))?;
        } else {
            break;
        }
    }
    drop(decoder);

    let (bytes, sha256) = hash_file_cancellable(path, cancellation)?;
    check_cancelled(cancellation)?;
    let after = fs::metadata(path)
        .map_err(|source| io_error("stat source after inspection", path, source))?;
    if before.len() != bytes || after.len() != bytes {
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
        bytes,
        sha256,
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
    let file =
        File::open(path).map_err(|source| io_error("open source for hashing", path, source))?;
    hash_reader_cancellable(path, file, cancellation)
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
) -> Result<BTreeMap<String, PathBuf>, ImportError> {
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
        return Ok(BTreeMap::from([(relative, inspection.source.path.clone())]));
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
    let mut files = BTreeMap::new();
    for path in paths {
        check_cancelled(cancellation)?;
        let relative = relative_name_for_file(&inspection.source.path, &path)
            .map_err(|_| ImportError::SourceChanged(path.clone()))?;
        if files.insert(relative, path.clone()).is_some() {
            return Err(ImportError::SourceChanged(path));
        }
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

fn validate_region_request(
    inspection: &TiffInspection,
    channel: u32,
    timepoint: u64,
    origin_zyx: [u64; 3],
    extent_zyx: [u64; 3],
    destination_bytes: usize,
) -> Result<(), ImportError> {
    if channel >= inspection.channels || timepoint >= inspection.shape.t() {
        return Err(ImportError::InvalidRequest(
            "source region channel or timepoint is out of bounds",
        ));
    }
    if extent_zyx.contains(&0) {
        return Err(ImportError::InvalidRequest(
            "source region extents must be positive",
        ));
    }
    for axis in 0..3 {
        let end = origin_zyx[axis]
            .checked_add(extent_zyx[axis])
            .ok_or(ImportError::Overflow)?;
        if end > inspection.shape.spatial().dimensions()[axis] {
            return Err(ImportError::InvalidRequest(
                "source region extends outside the inspected shape",
            ));
        }
    }
    let expected = extent_zyx
        .into_iter()
        .try_fold(1_u64, |count, extent| {
            count.checked_mul(extent).ok_or(ImportError::Overflow)
        })?
        .checked_mul(u64::from(inspection.dtype.bytes_per_sample()))
        .ok_or(ImportError::Overflow)?;
    if usize::try_from(expected).map_err(|_| ImportError::Overflow)? != destination_bytes {
        return Err(ImportError::InvalidRequest(
            "source region destination has the wrong byte length",
        ));
    }
    Ok(())
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
    };

    use tempfile::tempdir;
    use tiff::{
        encoder::{TiffEncoder, colortype},
        tags::Tag,
    };

    use super::*;

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
    fn single_stack_inspection_and_strip_bounded_region_are_exact() {
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
        revalidate(&inspection, &ImportCancellation::new()).unwrap();

        let cancellation = ImportCancellation::new();
        cancellation.cancel();
        assert!(matches!(
            revalidate(&inspection, &cancellation),
            Err(ImportError::Cancelled)
        ));

        let mut destination = vec![0_u8; 2 * 2 * 2 * 2];
        let counters = read_region_into(
            &inspection,
            0,
            0,
            [0, 1, 1],
            [2, 2, 2],
            &mut destination,
            8,
            &ImportCancellation::new(),
        )
        .unwrap();
        let values = destination
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(values, [5, 6, 9, 10, 105, 106, 109, 110]);
        assert_eq!(counters.decoded_bytes, 32);
        assert!(counters.source_bytes_read > 0);

        let error = read_region_into(
            &inspection,
            0,
            0,
            [0, 0, 0],
            [1, 1, 1],
            &mut [0; 2],
            7,
            &ImportCancellation::new(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ImportError::WorkingMemoryExceeded {
                required_bytes: 8,
                budget_bytes: 7
            }
        ));

        let cancellation = ImportCancellation::new();
        cancellation.cancel();
        let error = read_region_into(
            &inspection,
            0,
            0,
            [0, 0, 0],
            [1, 1, 1],
            &mut [0; 2],
            8,
            &cancellation,
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

        let mut destination = vec![0_u8; 2 * 2 * 3 * 2];
        read_region_into(
            &inspection,
            0,
            0,
            [0, 0, 0],
            [2, 2, 3],
            &mut destination,
            inspection.maximum_decoded_chunk_bytes,
            &ImportCancellation::new(),
        )
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
        let error = read_region_into(
            &inspection,
            0,
            0,
            [0, 0, 0],
            [1, 2, 2],
            &mut [0; 16],
            inspection.maximum_decoded_chunk_bytes,
            &ImportCancellation::new(),
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
