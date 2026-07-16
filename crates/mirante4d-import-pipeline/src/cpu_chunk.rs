//! Bounded base/pyramid CPU tasks and canonical inner encoding.

use std::{mem::size_of, sync::Arc, time::Instant};

use mirante4d_domain::IntensityDType;
use mirante4d_identity::{
    PreparedScientificTile, SCIENTIFIC_TILE_SHAPE_TZYX, ScientificLayerDescriptor,
    ScientificLayerHasher, ScientificTile,
};
use mirante4d_storage::{
    CanonicalEncodedInner, INNER_CODEC_WORKING_BYTES_MAX, PackedIndexRecord, ShardProfileKind,
};

use crate::{
    ImportCancellation, ImportError,
    canonical_cache::CanonicalBaseReader,
    chunk::{
        PreparedChunk, checked_voxels, chunk_extent, chunk_shape, compact_byte_len,
        copy_padded_region, prepare_chunk, validity_kind,
    },
    pyramid::downsample_region,
    sentinel::{
        GuardedU8Region, Region3, clipped_halo, downsample_guarded_u8, guarded_u8_core,
        invalid_dilation_radius,
    },
    spool::{SpoolPayloadReader, SpoolWorkUnitDescriptor, SpoolWorkUnitKey},
};

// This reservation covers the native codec context and its transient working
// allocations in addition to the exact decoded/encoded Vec capacities below.
// It is deliberately charged once per live task so worker count falls as the
// selected import budget gets smaller.
const TASK_AND_QUEUE_METADATA_BYTES: u64 = 4 * 1024;

pub(crate) struct BaseChunkCpuTask {
    pub(crate) key: SpoolWorkUnitKey,
    pub(crate) dtype: IntensityDType,
    pub(crate) is_2d: bool,
    pub(crate) logical_shape_zyx: [u64; 3],
    pub(crate) canonical: Arc<CanonicalBaseReader>,
    pub(crate) channel: u32,
    pub(crate) timepoint: u64,
    pub(crate) origin_zyx: [u64; 3],
    pub(crate) source_shape_zyx: [u64; 3],
    pub(crate) u8_sentinel: Option<u8>,
}

#[derive(Clone, Copy)]
pub(crate) struct PyramidSourceChunk {
    pub(crate) chunk_zyx: [u64; 3],
    pub(crate) descriptor: SpoolWorkUnitDescriptor,
}

pub(crate) struct PyramidChunkCpuTask {
    pub(crate) key: SpoolWorkUnitKey,
    pub(crate) dtype: IntensityDType,
    pub(crate) is_2d: bool,
    pub(crate) payload: Arc<SpoolPayloadReader>,
    pub(crate) pixel_source_chunks: Vec<PyramidSourceChunk>,
    pub(crate) validity_source_chunks: Vec<PyramidSourceChunk>,
    pub(crate) source_level_shape_zyx: [u64; 3],
    pub(crate) source_origin_zyx: [u64; 3],
    pub(crate) source_shape_zyx: [u64; 3],
    pub(crate) validity_origin_zyx: [u64; 3],
    pub(crate) validity_shape_zyx: [u64; 3],
    pub(crate) pixel_kind: ShardProfileKind,
    pub(crate) validity_kind: ShardProfileKind,
    pub(crate) explicit_validity: bool,
    pub(crate) target_level_shape_zyx: [u64; 3],
    pub(crate) target_origin_zyx: [u64; 3],
    pub(crate) target_shape_zyx: [u64; 3],
}

pub(crate) const fn pyramid_pixel_source_chunks_max(is_2d: bool) -> usize {
    if is_2d { 4 } else { 8 }
}

pub(crate) const fn pyramid_validity_source_chunks_max(is_2d: bool) -> usize {
    if is_2d { 16 } else { 64 }
}

pub(crate) struct ScientificTileCpuTask {
    pub(crate) canonical: Arc<CanonicalBaseReader>,
    pub(crate) descriptor: ScientificLayerDescriptor,
    pub(crate) linear_index: u64,
    pub(crate) channel: u32,
    pub(crate) origin_tzyx: [u64; 4],
    pub(crate) extent_tzyx: [u64; 4],
    pub(crate) source_shape_zyx: [u64; 3],
    pub(crate) u8_sentinel: Option<u8>,
}

pub(crate) struct EncodedPreparedWorkUnit {
    pub(crate) key: SpoolWorkUnitKey,
    pub(crate) pixel: Option<CanonicalEncodedInner>,
    pub(crate) validity: Option<CanonicalEncodedInner>,
    pub(crate) packed_index: PackedIndexRecord,
    pub(crate) codec_encode_calls: u64,
    pub(crate) codec_encode_time_ns: u64,
    pub(crate) codec_decode_calls: u64,
    pub(crate) codec_decode_time_ns: u64,
}

struct DecodedSpoolRegion {
    pixels_le: Vec<u8>,
    validity: Option<Vec<u8>>,
    codec_decode_calls: u64,
    codec_decode_time_ns: u64,
}

pub(crate) fn prepare_base_chunk(
    task: BaseChunkCpuTask,
    cancellation: &ImportCancellation,
) -> Result<EncodedPreparedWorkUnit, ImportError> {
    check_cancelled(cancellation)?;
    let (pixels_le, validity) = match task.u8_sentinel {
        Some(sentinel) => {
            if task.dtype != IntensityDType::Uint8 {
                return Err(ImportError::InvalidRequest(
                    "the guarded sentinel policy requires uint8 pixels",
                ));
            }
            let guarded = read_guarded_u8_core(
                &task.canonical,
                task.channel,
                task.timepoint,
                task.source_shape_zyx,
                Region3 {
                    origin: task.origin_zyx,
                    shape: task.logical_shape_zyx,
                },
                sentinel,
            )?;
            (guarded.pixels, Some(guarded.validity))
        }
        None => {
            let mut pixels = vec![0; compact_byte_len(task.dtype, task.logical_shape_zyx)?];
            task.canonical.read_region_into(
                task.channel,
                task.timepoint,
                task.origin_zyx,
                task.logical_shape_zyx,
                &mut pixels,
            )?;
            (pixels, None)
        }
    };
    check_cancelled(cancellation)?;
    let prepared = prepare_chunk(
        task.key.coordinates(),
        task.dtype,
        task.is_2d,
        task.logical_shape_zyx,
        &pixels_le,
        validity.as_deref(),
    )?;
    check_cancelled(cancellation)?;
    encode_prepared(
        task.key,
        task.is_2d,
        task.dtype,
        prepared,
        0,
        0,
        cancellation,
    )
}

pub(crate) fn prepare_pyramid_chunk(
    task: PyramidChunkCpuTask,
    cancellation: &ImportCancellation,
) -> Result<EncodedPreparedWorkUnit, ImportError> {
    check_cancelled(cancellation)?;
    let decoded = read_spooled_region(&task, cancellation)?;
    let (pixels_le, validity) = if task.explicit_validity {
        if task.dtype != IntensityDType::Uint8 {
            return Err(ImportError::InvalidRequest(
                "the guarded pyramid policy requires uint8 pixels",
            ));
        }
        let parent_validity = decoded.validity.as_deref().ok_or_else(|| {
            ImportError::InvalidCheckpoint(
                "a guarded pyramid task has no parent validity".to_owned(),
            )
        })?;
        let guarded = downsample_guarded_u8(
            &decoded.pixels_le,
            Region3 {
                origin: task.source_origin_zyx,
                shape: task.source_shape_zyx,
            },
            parent_validity,
            Region3 {
                origin: task.validity_origin_zyx,
                shape: task.validity_shape_zyx,
            },
            task.source_level_shape_zyx,
            task.target_level_shape_zyx,
            Region3 {
                origin: task.target_origin_zyx,
                shape: task.target_shape_zyx,
            },
        )?;
        (guarded.pixels, Some(guarded.validity))
    } else {
        let downsampled = downsample_region(task.dtype, task.source_shape_zyx, &decoded.pixels_le)?;
        if downsampled.shape_zyx != task.target_shape_zyx {
            return Err(ImportError::InvalidCheckpoint(
                "coarse chunk shape differs from its import plan".to_owned(),
            ));
        }
        (downsampled.pixels_le, None)
    };
    check_cancelled(cancellation)?;
    let prepared = prepare_chunk(
        task.key.coordinates(),
        task.dtype,
        task.is_2d,
        task.target_shape_zyx,
        &pixels_le,
        validity.as_deref(),
    )?;
    check_cancelled(cancellation)?;
    encode_prepared(
        task.key,
        task.is_2d,
        task.dtype,
        prepared,
        decoded.codec_decode_calls,
        decoded.codec_decode_time_ns,
        cancellation,
    )
}

pub(crate) fn prepare_scientific_tile(
    task: ScientificTileCpuTask,
    cancellation: &ImportCancellation,
) -> Result<PreparedScientificTile, ImportError> {
    check_cancelled(cancellation)?;
    let extent = [
        task.extent_tzyx[1],
        task.extent_tzyx[2],
        task.extent_tzyx[3],
    ];
    let core = Region3 {
        origin: [
            task.origin_tzyx[1],
            task.origin_tzyx[2],
            task.origin_tzyx[3],
        ],
        shape: extent,
    };
    let (pixels, per_voxel_validity) = match task.u8_sentinel {
        Some(sentinel) => {
            if task.descriptor.dtype() != IntensityDType::Uint8 {
                return Err(ImportError::InvalidRequest(
                    "the guarded scientific sentinel policy requires uint8 pixels",
                ));
            }
            let guarded = read_guarded_u8_core(
                &task.canonical,
                task.channel,
                task.origin_tzyx[0],
                task.source_shape_zyx,
                core,
                sentinel,
            )?;
            (guarded.pixels, guarded.validity)
        }
        None => {
            let mut pixels = vec![0; compact_byte_len(task.descriptor.dtype(), extent)?];
            task.canonical.read_region_into(
                task.channel,
                task.origin_tzyx[0],
                core.origin,
                extent,
                &mut pixels,
            )?;
            let validity = vec![1; checked_voxels(extent)?];
            (pixels, validity)
        }
    };
    check_cancelled(cancellation)?;
    let validity = pack_scientific_validity(&per_voxel_validity)?;
    let hasher = ScientificLayerHasher::new(task.descriptor)?;
    let prepared = hasher.prepare_tile(
        task.linear_index,
        ScientificTile::new(task.origin_tzyx, task.extent_tzyx, &validity, &pixels),
    )?;
    check_cancelled(cancellation)?;
    Ok(prepared)
}

fn read_guarded_u8_core(
    canonical: &CanonicalBaseReader,
    channel: u32,
    timepoint: u64,
    source_shape_zyx: [u64; 3],
    core: Region3,
    sentinel: u8,
) -> Result<GuardedU8Region, ImportError> {
    let window = clipped_halo(
        core,
        source_shape_zyx,
        invalid_dilation_radius(source_shape_zyx),
    )?;
    let mut window_pixels = vec![0; checked_voxels(window.shape)?];
    canonical.read_region_into(
        channel,
        timepoint,
        window.origin,
        window.shape,
        &mut window_pixels,
    )?;
    guarded_u8_core(&window_pixels, window, source_shape_zyx, core, sentinel)
}

fn encode_prepared(
    key: SpoolWorkUnitKey,
    is_2d: bool,
    dtype: IntensityDType,
    prepared: PreparedChunk,
    codec_decode_calls: u64,
    codec_decode_time_ns: u64,
    cancellation: &ImportCancellation,
) -> Result<EncodedPreparedWorkUnit, ImportError> {
    let PreparedChunk {
        pixel,
        validity,
        record,
    } = prepared;
    let mut codec_encode_calls = 0_u64;
    let mut codec_encode_time_ns = 0_u64;
    let pixel = pixel
        .map(|decoded| {
            check_cancelled(cancellation)?;
            let (encoded, elapsed_ns) =
                encode_one(crate::chunk::pixel_kind(dtype, is_2d), &decoded)?;
            codec_encode_calls = codec_encode_calls
                .checked_add(1)
                .ok_or(ImportError::Overflow)?;
            codec_encode_time_ns = codec_encode_time_ns
                .checked_add(elapsed_ns)
                .ok_or(ImportError::Overflow)?;
            Ok::<_, ImportError>(encoded)
        })
        .transpose()?;
    let validity = validity
        .map(|decoded| {
            check_cancelled(cancellation)?;
            let (encoded, elapsed_ns) = encode_one(crate::chunk::validity_kind(is_2d), &decoded)?;
            codec_encode_calls = codec_encode_calls
                .checked_add(1)
                .ok_or(ImportError::Overflow)?;
            codec_encode_time_ns = codec_encode_time_ns
                .checked_add(elapsed_ns)
                .ok_or(ImportError::Overflow)?;
            Ok::<_, ImportError>(encoded)
        })
        .transpose()?;
    check_cancelled(cancellation)?;
    Ok(EncodedPreparedWorkUnit {
        key,
        pixel,
        validity,
        packed_index: record,
        codec_encode_calls,
        codec_encode_time_ns,
        codec_decode_calls,
        codec_decode_time_ns,
    })
}

fn read_spooled_region(
    task: &PyramidChunkCpuTask,
    cancellation: &ImportCancellation,
) -> Result<DecodedSpoolRegion, ImportError> {
    let width = usize::from(task.dtype.bytes_per_sample());
    let mut pixels = vec![0; compact_byte_len(task.dtype, task.source_shape_zyx)?];
    let inner = chunk_shape(task.is_2d);
    let mut codec_decode_calls = 0_u64;
    let mut codec_decode_time_ns = 0_u64;
    for source in &task.pixel_source_chunks {
        check_cancelled(cancellation)?;
        let logical = chunk_extent(task.source_level_shape_zyx, source.chunk_zyx, task.is_2d)?;
        let capacity =
            u64::try_from(checked_voxels(logical)?).map_err(|_| ImportError::Overflow)?;
        let record = source
            .descriptor
            .packed_index_record(task.dtype, capacity)?;
        let chunk_origin = [
            source.chunk_zyx[0] * inner[0],
            source.chunk_zyx[1] * inner[1],
            source.chunk_zyx[2] * inner[2],
        ];
        let overlap = region_overlap(
            chunk_origin,
            logical,
            task.source_origin_zyx,
            task.source_shape_zyx,
        )?
        .ok_or_else(|| {
            ImportError::InvalidCheckpoint(
                "a described pixel source chunk does not overlap its region".to_owned(),
            )
        })?;
        let decoded = task.payload.read_pixel_component(source.descriptor)?;
        accumulate_decode_metrics(
            &mut codec_decode_calls,
            &mut codec_decode_time_ns,
            decoded.codec_decode_calls,
            decoded.codec_decode_time_ns,
        )?;
        if let Some(pixel) = decoded.chunk {
            if pixel.kind != task.pixel_kind {
                return Err(ImportError::InvalidCheckpoint(
                    "a spooled pixel chunk has the wrong storage kind".to_owned(),
                ));
            }
            copy_padded_region(
                &pixel.decoded,
                inner,
                overlap.source_local,
                overlap.shape,
                width,
                &mut pixels,
                task.source_shape_zyx,
                overlap.destination_local,
            )?;
        } else if record.pixel_payload_present() {
            return Err(ImportError::InvalidCheckpoint(
                "a required spooled pixel chunk is absent".to_owned(),
            ));
        }
    }

    let validity = if task.explicit_validity {
        let mut destination = vec![0; checked_voxels(task.validity_shape_zyx)?];
        for source in &task.validity_source_chunks {
            check_cancelled(cancellation)?;
            let logical = chunk_extent(task.source_level_shape_zyx, source.chunk_zyx, task.is_2d)?;
            let capacity =
                u64::try_from(checked_voxels(logical)?).map_err(|_| ImportError::Overflow)?;
            let record = source
                .descriptor
                .packed_index_record(task.dtype, capacity)?;
            if !record.explicit_validity() {
                return Err(ImportError::InvalidCheckpoint(
                    "a guarded pyramid source has no explicit validity".to_owned(),
                ));
            }
            let chunk_origin = [
                source.chunk_zyx[0] * inner[0],
                source.chunk_zyx[1] * inner[1],
                source.chunk_zyx[2] * inner[2],
            ];
            let overlap = region_overlap(
                chunk_origin,
                logical,
                task.validity_origin_zyx,
                task.validity_shape_zyx,
            )?
            .ok_or_else(|| {
                ImportError::InvalidCheckpoint(
                    "a described validity source chunk does not overlap its region".to_owned(),
                )
            })?;
            if record.all_voxels_valid() {
                fill_mask_region(
                    &mut destination,
                    task.validity_shape_zyx,
                    overlap.destination_local,
                    overlap.shape,
                    1,
                )?;
                continue;
            }
            if record.all_voxels_invalid() {
                continue;
            }
            let decoded = task.payload.read_validity_component(source.descriptor)?;
            accumulate_decode_metrics(
                &mut codec_decode_calls,
                &mut codec_decode_time_ns,
                decoded.codec_decode_calls,
                decoded.codec_decode_time_ns,
            )?;
            let packed = decoded.chunk.ok_or_else(|| {
                ImportError::InvalidCheckpoint(
                    "a mixed-validity source has no validity component".to_owned(),
                )
            })?;
            if packed.kind != task.validity_kind {
                return Err(ImportError::InvalidCheckpoint(
                    "a spooled validity chunk has the wrong storage kind".to_owned(),
                ));
            }
            copy_packed_mask_region(
                &packed.decoded,
                inner,
                logical,
                overlap.source_local,
                overlap.shape,
                &mut destination,
                task.validity_shape_zyx,
                overlap.destination_local,
                task.is_2d,
            )?;
        }
        Some(destination)
    } else {
        None
    };
    Ok(DecodedSpoolRegion {
        pixels_le: pixels,
        validity,
        codec_decode_calls,
        codec_decode_time_ns,
    })
}

#[derive(Clone, Copy)]
struct RegionOverlap {
    source_local: [u64; 3],
    destination_local: [u64; 3],
    shape: [u64; 3],
}

fn region_overlap(
    source_origin: [u64; 3],
    source_shape: [u64; 3],
    destination_origin: [u64; 3],
    destination_shape: [u64; 3],
) -> Result<Option<RegionOverlap>, ImportError> {
    let mut overlap_origin = [0; 3];
    let mut overlap_end = [0; 3];
    for axis in 0..3 {
        let source_end = source_origin[axis]
            .checked_add(source_shape[axis])
            .ok_or(ImportError::Overflow)?;
        let destination_end = destination_origin[axis]
            .checked_add(destination_shape[axis])
            .ok_or(ImportError::Overflow)?;
        overlap_origin[axis] = source_origin[axis].max(destination_origin[axis]);
        overlap_end[axis] = source_end.min(destination_end);
        if overlap_origin[axis] >= overlap_end[axis] {
            return Ok(None);
        }
    }
    Ok(Some(RegionOverlap {
        source_local: [
            overlap_origin[0] - source_origin[0],
            overlap_origin[1] - source_origin[1],
            overlap_origin[2] - source_origin[2],
        ],
        destination_local: [
            overlap_origin[0] - destination_origin[0],
            overlap_origin[1] - destination_origin[1],
            overlap_origin[2] - destination_origin[2],
        ],
        shape: [
            overlap_end[0] - overlap_origin[0],
            overlap_end[1] - overlap_origin[1],
            overlap_end[2] - overlap_origin[2],
        ],
    }))
}

fn accumulate_decode_metrics(
    total_calls: &mut u64,
    total_time_ns: &mut u64,
    calls: u64,
    time_ns: u64,
) -> Result<(), ImportError> {
    *total_calls = total_calls
        .checked_add(calls)
        .ok_or(ImportError::Overflow)?;
    *total_time_ns = total_time_ns
        .checked_add(time_ns)
        .ok_or(ImportError::Overflow)?;
    Ok(())
}

fn fill_mask_region(
    destination: &mut [u8],
    destination_shape: [u64; 3],
    destination_origin: [u64; 3],
    extent: [u64; 3],
    value: u8,
) -> Result<(), ImportError> {
    if !matches!(value, 0 | 1) || destination.len() != checked_voxels(destination_shape)? {
        return Err(ImportError::InvalidCheckpoint(
            "validity fill region is malformed".to_owned(),
        ));
    }
    for z in 0..extent[0] {
        for y in 0..extent[1] {
            for x in 0..extent[2] {
                let index = linear_index(
                    destination_shape,
                    [
                        destination_origin[0] + z,
                        destination_origin[1] + y,
                        destination_origin[2] + x,
                    ],
                )?;
                destination[index] = value;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn copy_packed_mask_region(
    packed: &[u8],
    inner: [u64; 3],
    logical: [u64; 3],
    source_origin: [u64; 3],
    extent: [u64; 3],
    destination: &mut [u8],
    destination_shape: [u64; 3],
    destination_origin: [u64; 3],
    is_2d: bool,
) -> Result<(), ImportError> {
    if packed.len() != validity_kind(is_2d).decoded_inner_bytes()
        || destination.len() != checked_voxels(destination_shape)?
    {
        return Err(ImportError::InvalidCheckpoint(
            "packed validity region is malformed".to_owned(),
        ));
    }
    for axis in 0..3 {
        if source_origin[axis]
            .checked_add(extent[axis])
            .is_none_or(|end| end > logical[axis])
            || destination_origin[axis]
                .checked_add(extent[axis])
                .is_none_or(|end| end > destination_shape[axis])
        {
            return Err(ImportError::InvalidCheckpoint(
                "packed validity overlap lies outside its region".to_owned(),
            ));
        }
    }
    let row_bytes = usize::try_from(inner[2].div_ceil(8)).map_err(|_| ImportError::Overflow)?;
    for z in 0..extent[0] {
        for y in 0..extent[1] {
            let source_z = source_origin[0] + z;
            let source_y = source_origin[1] + y;
            let row = usize::try_from(
                source_z
                    .checked_mul(inner[1])
                    .and_then(|value| value.checked_add(source_y))
                    .ok_or(ImportError::Overflow)?,
            )
            .map_err(|_| ImportError::Overflow)?;
            let row_offset = row.checked_mul(row_bytes).ok_or(ImportError::Overflow)?;
            for x in 0..extent[2] {
                let source_x = source_origin[2] + x;
                let byte = usize::try_from(source_x / 8).map_err(|_| ImportError::Overflow)?;
                let bit = u8::try_from(source_x % 8).expect("modulo eight fits u8");
                let valid = u8::from(packed[row_offset + byte] & (1 << bit) != 0);
                let destination_index = linear_index(
                    destination_shape,
                    [
                        destination_origin[0] + z,
                        destination_origin[1] + y,
                        destination_origin[2] + x,
                    ],
                )?;
                destination[destination_index] = valid;
            }
        }
    }
    Ok(())
}

fn encode_one(
    kind: ShardProfileKind,
    decoded: &[u8],
) -> Result<(CanonicalEncodedInner, u64), ImportError> {
    let started = Instant::now();
    let encoded = CanonicalEncodedInner::encode(kind, decoded)?;
    let elapsed_ns =
        u64::try_from(started.elapsed().as_nanos()).map_err(|_| ImportError::Overflow)?;
    Ok((encoded, elapsed_ns))
}

pub(crate) fn base_task_charge_bytes(
    dtype: IntensityDType,
    pixel_kind: ShardProfileKind,
    validity_profile: ShardProfileKind,
    explicit_validity: bool,
) -> Result<u64, ImportError> {
    let pixel_inner = as_u64(pixel_kind.decoded_inner_bytes())?;
    let pixel_encoded = as_u64(pixel_kind.encoded_inner_bytes_max())?;
    let compact_source = if explicit_validity {
        maximum_base_halo_voxels(validity_profile)?
            .checked_mul(u64::from(dtype.bytes_per_sample()))
            .ok_or(ImportError::Overflow)?
    } else {
        pixel_inner
    };
    let decoded_pixels = if explicit_validity {
        compact_source
            .checked_add(pixel_inner) // cropped compact target
            .and_then(|value| value.checked_add(pixel_inner)) // padded target
            .ok_or(ImportError::Overflow)?
    } else {
        pixel_inner.checked_mul(2).ok_or(ImportError::Overflow)?
    };
    let mut bytes = decoded_pixels
        .checked_add(pixel_encoded)
        .and_then(|value| value.checked_add(INNER_CODEC_WORKING_BYTES_MAX))
        .and_then(|value| value.checked_add(TASK_AND_QUEUE_METADATA_BYTES))
        .ok_or(ImportError::Overflow)?;
    if explicit_validity {
        let logical_voxels = pixel_inner / u64::from(dtype.bytes_per_sample());
        bytes = bytes
            .checked_add(logical_voxels)
            .and_then(|value| {
                value.checked_add(as_u64(validity_profile.decoded_inner_bytes()).ok()?)
            })
            .and_then(|value| {
                value.checked_add(as_u64(validity_profile.encoded_inner_bytes_max()).ok()?)
            })
            .ok_or(ImportError::Overflow)?;
    }
    Ok(bytes)
}

pub(crate) fn pyramid_task_charge_bytes(
    dtype: IntensityDType,
    pixel_kind: ShardProfileKind,
    validity_profile: ShardProfileKind,
    is_2d: bool,
    explicit_validity: bool,
) -> Result<u64, ImportError> {
    let source_multiplier = pyramid_pixel_source_chunks_max(is_2d) as u64;
    let descriptor_multiplier = if explicit_validity {
        source_multiplier + pyramid_validity_source_chunks_max(is_2d) as u64
    } else {
        source_multiplier
    };
    let pixel_inner = as_u64(pixel_kind.decoded_inner_bytes())?;
    let pixel_encoded = as_u64(pixel_kind.encoded_inner_bytes_max())?;
    let mut bytes = pixel_inner
        .checked_mul(source_multiplier)
        .and_then(|value| value.checked_add(pixel_inner)) // one decoded spool chunk
        .and_then(|value| value.checked_add(pixel_encoded)) // encoded source component
        .and_then(|value| value.checked_add(pixel_inner)) // compact target
        .and_then(|value| value.checked_add(pixel_inner)) // padded target
        .and_then(|value| value.checked_add(pixel_encoded)) // encoded target component
        .and_then(|value| value.checked_add(INNER_CODEC_WORKING_BYTES_MAX))
        .and_then(|value| {
            value.checked_add(
                u64::try_from(size_of::<PyramidSourceChunk>())
                    .ok()?
                    .checked_mul(descriptor_multiplier)?,
            )
        })
        .and_then(|value| value.checked_add(TASK_AND_QUEUE_METADATA_BYTES))
        .ok_or(ImportError::Overflow)?;
    if explicit_validity {
        let target_voxels = pixel_inner / u64::from(dtype.bytes_per_sample());
        let validity_inner = as_u64(validity_profile.decoded_inner_bytes())?;
        let parent_validity_voxels = maximum_parent_validity_halo_voxels(is_2d)?;
        let target_support_voxels = maximum_target_support_halo_voxels(is_2d)?;
        bytes = bytes
            .checked_add(parent_validity_voxels)
            .and_then(|value| value.checked_add(validity_inner)) // decoded spool mask
            .and_then(|value| {
                value.checked_add(as_u64(validity_profile.encoded_inner_bytes_max()).ok()?)
            }) // encoded source mask
            .and_then(|value| value.checked_add(target_support_voxels))
            .and_then(|value| value.checked_add(target_voxels)) // target byte mask
            .and_then(|value| value.checked_add(validity_inner)) // packed target mask
            .and_then(|value| {
                value.checked_add(as_u64(validity_profile.encoded_inner_bytes_max()).ok()?)
            })
            .ok_or(ImportError::Overflow)?;
    }
    Ok(bytes)
}

pub(crate) fn scientific_task_charge_bytes(
    dtype: IntensityDType,
    explicit_validity: bool,
) -> Result<u64, ImportError> {
    let voxels = SCIENTIFIC_TILE_SHAPE_TZYX[1]
        .checked_mul(SCIENTIFIC_TILE_SHAPE_TZYX[2])
        .and_then(|value| value.checked_mul(SCIENTIFIC_TILE_SHAPE_TZYX[3]))
        .ok_or(ImportError::Overflow)?;
    let source_voxels = if explicit_validity {
        SCIENTIFIC_TILE_SHAPE_TZYX[1]
            .checked_add(2)
            .and_then(|z| {
                SCIENTIFIC_TILE_SHAPE_TZYX[2]
                    .checked_add(2)
                    .and_then(|y| z.checked_mul(y))
            })
            .and_then(|zy| {
                SCIENTIFIC_TILE_SHAPE_TZYX[3]
                    .checked_add(2)
                    .and_then(|x| zy.checked_mul(x))
            })
            .ok_or(ImportError::Overflow)?
    } else {
        voxels
    };
    source_voxels
        .checked_mul(u64::from(dtype.bytes_per_sample()))
        .and_then(|value| {
            if explicit_validity {
                value.checked_add(voxels.checked_mul(u64::from(dtype.bytes_per_sample()))?)
            } else {
                Some(value)
            }
        })
        .and_then(|value| value.checked_add(voxels))
        .and_then(|value| value.checked_add(voxels.div_ceil(8)))
        .and_then(|value| value.checked_add(TASK_AND_QUEUE_METADATA_BYTES))
        .ok_or(ImportError::Overflow)
}

fn maximum_base_halo_voxels(validity_profile: ShardProfileKind) -> Result<u64, ImportError> {
    let is_2d = validity_profile == ShardProfileKind::Validity2d;
    let inner = chunk_shape(is_2d);
    checked_product_u64([
        if is_2d { 1 } else { inner[0] + 2 },
        inner[1] + 2,
        inner[2] + 2,
    ])
}

fn maximum_parent_validity_halo_voxels(is_2d: bool) -> Result<u64, ImportError> {
    let inner = chunk_shape(is_2d);
    checked_product_u64([
        if is_2d { 1 } else { inner[0] * 2 + 4 },
        inner[1] * 2 + 4,
        inner[2] * 2 + 4,
    ])
}

fn maximum_target_support_halo_voxels(is_2d: bool) -> Result<u64, ImportError> {
    let inner = chunk_shape(is_2d);
    checked_product_u64([
        if is_2d { 1 } else { inner[0] + 2 },
        inner[1] + 2,
        inner[2] + 2,
    ])
}

fn checked_product_u64(values: [u64; 3]) -> Result<u64, ImportError> {
    values
        .into_iter()
        .try_fold(1_u64, |product, value| product.checked_mul(value))
        .ok_or(ImportError::Overflow)
}

fn linear_index(shape: [u64; 3], coordinate: [u64; 3]) -> Result<usize, ImportError> {
    let index = coordinate[0]
        .checked_mul(shape[1])
        .and_then(|value| value.checked_add(coordinate[1]))
        .and_then(|value| value.checked_mul(shape[2]))
        .and_then(|value| value.checked_add(coordinate[2]))
        .ok_or(ImportError::Overflow)?;
    usize::try_from(index).map_err(|_| ImportError::Overflow)
}

fn pack_scientific_validity(per_voxel: &[u8]) -> Result<Vec<u8>, ImportError> {
    let mut packed = vec![0; per_voxel.len().div_ceil(8)];
    for (index, valid) in per_voxel.iter().copied().enumerate() {
        match valid {
            0 => {}
            1 => packed[index / 8] |= 1 << (index % 8),
            _ => {
                return Err(ImportError::InvalidRequest(
                    "scientific validity must contain canonical bits",
                ));
            }
        }
    }
    Ok(packed)
}

fn as_u64(value: usize) -> Result<u64, ImportError> {
    u64::try_from(value).map_err(|_| ImportError::Overflow)
}

fn check_cancelled(cancellation: &ImportCancellation) -> Result<(), ImportError> {
    if cancellation.is_cancelled() {
        Err(ImportError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mirante4d_storage::ShardProfileKind;

    use super::*;

    #[test]
    fn task_charges_cover_more_than_all_exact_live_vec_capacities() {
        let base = base_task_charge_bytes(
            IntensityDType::Float32,
            ShardProfileKind::Pixel3dFloat32,
            ShardProfileKind::Validity3d,
            true,
        )
        .unwrap();
        let pyramid = pyramid_task_charge_bytes(
            IntensityDType::Float32,
            ShardProfileKind::Pixel3dFloat32,
            ShardProfileKind::Validity3d,
            false,
            true,
        )
        .unwrap();
        assert!(base > 10 * 1024 * 1024);
        assert!(pyramid > 20 * 1024 * 1024);
        assert!(pyramid > base);
        assert_eq!(pyramid_pixel_source_chunks_max(true), 4);
        assert_eq!(pyramid_pixel_source_chunks_max(false), 8);
        assert_eq!(pyramid_validity_source_chunks_max(true), 16);
        assert_eq!(pyramid_validity_source_chunks_max(false), 64);
    }
}
