//! Bounded base/pyramid CPU tasks and canonical inner encoding.

use std::{sync::Arc, time::Instant};

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
        copy_padded_region, normalize_u8_sentinel, prepare_chunk, unpack_validity,
    },
    pyramid::downsample_region,
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
    pub(crate) source_chunks: Vec<PyramidSourceChunk>,
    pub(crate) source_level_shape_zyx: [u64; 3],
    pub(crate) source_origin_zyx: [u64; 3],
    pub(crate) source_shape_zyx: [u64; 3],
    pub(crate) pixel_kind: ShardProfileKind,
    pub(crate) validity_kind: ShardProfileKind,
    pub(crate) explicit_validity: bool,
    pub(crate) target_shape_zyx: [u64; 3],
}

pub(crate) struct ScientificTileCpuTask {
    pub(crate) canonical: Arc<CanonicalBaseReader>,
    pub(crate) descriptor: ScientificLayerDescriptor,
    pub(crate) linear_index: u64,
    pub(crate) channel: u32,
    pub(crate) origin_tzyx: [u64; 4],
    pub(crate) extent_tzyx: [u64; 4],
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
    let mut pixels_le = vec![0; compact_byte_len(task.dtype, task.logical_shape_zyx)?];
    task.canonical.read_region_into(
        task.channel,
        task.timepoint,
        task.origin_zyx,
        task.logical_shape_zyx,
        &mut pixels_le,
    )?;
    check_cancelled(cancellation)?;
    let validity = task
        .u8_sentinel
        .map(|sentinel| normalize_u8_sentinel(&mut pixels_le, sentinel));
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
    let downsampled = downsample_region(
        task.dtype,
        task.source_shape_zyx,
        &decoded.pixels_le,
        decoded.validity.as_deref(),
    )?;
    if downsampled.shape_zyx != task.target_shape_zyx {
        return Err(ImportError::InvalidCheckpoint(
            "coarse chunk shape differs from its import plan".to_owned(),
        ));
    }
    check_cancelled(cancellation)?;
    let prepared = prepare_chunk(
        task.key.coordinates(),
        task.dtype,
        task.is_2d,
        task.target_shape_zyx,
        &downsampled.pixels_le,
        downsampled.validity.as_deref(),
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
    let mut pixels = vec![0; compact_byte_len(task.descriptor.dtype(), extent)?];
    task.canonical.read_region_into(
        task.channel,
        task.origin_tzyx[0],
        [
            task.origin_tzyx[1],
            task.origin_tzyx[2],
            task.origin_tzyx[3],
        ],
        extent,
        &mut pixels,
    )?;
    check_cancelled(cancellation)?;
    let per_voxel_validity = match task.u8_sentinel {
        Some(sentinel) => normalize_u8_sentinel(&mut pixels, sentinel),
        None => vec![1; checked_voxels(extent)?],
    };
    let validity = pack_scientific_validity(&per_voxel_validity)?;
    let hasher = ScientificLayerHasher::new(task.descriptor)?;
    let prepared = hasher.prepare_tile(
        task.linear_index,
        ScientificTile::new(task.origin_tzyx, task.extent_tzyx, &validity, &pixels),
    )?;
    check_cancelled(cancellation)?;
    Ok(prepared)
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
    let mut validity = task
        .explicit_validity
        .then(|| vec![0; checked_voxels(task.source_shape_zyx).expect("shape was checked")]);
    let inner = chunk_shape(task.is_2d);
    let mut codec_decode_calls = 0_u64;
    let mut codec_decode_time_ns = 0_u64;
    for source in &task.source_chunks {
        check_cancelled(cancellation)?;
        let decoded = task.payload.read_work_unit(source.descriptor)?;
        codec_decode_calls = codec_decode_calls
            .checked_add(decoded.codec_decode_calls)
            .ok_or(ImportError::Overflow)?;
        codec_decode_time_ns = codec_decode_time_ns
            .checked_add(decoded.codec_decode_time_ns)
            .ok_or(ImportError::Overflow)?;
        let unit = decoded.unit;
        let logical = chunk_extent(task.source_level_shape_zyx, source.chunk_zyx, task.is_2d)?;
        let capacity =
            u64::try_from(checked_voxels(logical)?).map_err(|_| ImportError::Overflow)?;
        let record = PackedIndexRecord::decode(&unit.packed_index, task.dtype, capacity).map_err(
            |error| {
                ImportError::InvalidCheckpoint(format!("a packed-index record is invalid: {error}"))
            },
        )?;
        let chunk_origin = [
            source.chunk_zyx[0] * inner[0],
            source.chunk_zyx[1] * inner[1],
            source.chunk_zyx[2] * inner[2],
        ];
        let mut overlap_origin = [0; 3];
        let mut overlap_end = [0; 3];
        for axis in 0..3 {
            overlap_origin[axis] = task.source_origin_zyx[axis].max(chunk_origin[axis]);
            overlap_end[axis] = (task.source_origin_zyx[axis] + task.source_shape_zyx[axis])
                .min(chunk_origin[axis] + logical[axis]);
        }
        let overlap = [
            overlap_end[0] - overlap_origin[0],
            overlap_end[1] - overlap_origin[1],
            overlap_end[2] - overlap_origin[2],
        ];
        let source_local = [
            overlap_origin[0] - chunk_origin[0],
            overlap_origin[1] - chunk_origin[1],
            overlap_origin[2] - chunk_origin[2],
        ];
        let destination_local = [
            overlap_origin[0] - task.source_origin_zyx[0],
            overlap_origin[1] - task.source_origin_zyx[1],
            overlap_origin[2] - task.source_origin_zyx[2],
        ];
        if let Some(pixel) = unit.pixel {
            if pixel.kind != task.pixel_kind {
                return Err(ImportError::InvalidCheckpoint(
                    "a spooled pixel chunk has the wrong storage kind".to_owned(),
                ));
            }
            copy_padded_region(
                &pixel.decoded,
                inner,
                source_local,
                overlap,
                width,
                &mut pixels,
                task.source_shape_zyx,
                destination_local,
            )?;
        } else if record.pixel_payload_present() {
            return Err(ImportError::InvalidCheckpoint(
                "a required spooled pixel chunk is absent".to_owned(),
            ));
        }

        if let Some(destination) = validity.as_mut() {
            let compact = match unit.validity {
                Some(validity) => {
                    if validity.kind != task.validity_kind {
                        return Err(ImportError::InvalidCheckpoint(
                            "a spooled validity chunk has the wrong storage kind".to_owned(),
                        ));
                    }
                    unpack_validity(&validity.decoded, logical, task.is_2d)?
                }
                None if record.all_voxels_valid() => vec![1; checked_voxels(logical)?],
                None if record.all_voxels_invalid() => vec![0; checked_voxels(logical)?],
                None => {
                    return Err(ImportError::InvalidCheckpoint(
                        "an explicit-validity record has no effective mask".to_owned(),
                    ));
                }
            };
            copy_mask_region(
                &compact,
                logical,
                source_local,
                overlap,
                destination,
                task.source_shape_zyx,
                destination_local,
            )?;
        }
    }
    Ok(DecodedSpoolRegion {
        pixels_le: pixels,
        validity,
        codec_decode_calls,
        codec_decode_time_ns,
    })
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
    validity_kind: ShardProfileKind,
    explicit_validity: bool,
) -> Result<u64, ImportError> {
    let pixel_inner = as_u64(pixel_kind.decoded_inner_bytes())?;
    let pixel_encoded = as_u64(pixel_kind.encoded_inner_bytes_max())?;
    let mut bytes = pixel_inner
        .checked_add(pixel_inner)
        .and_then(|value| value.checked_add(pixel_encoded))
        .and_then(|value| value.checked_add(INNER_CODEC_WORKING_BYTES_MAX))
        .and_then(|value| value.checked_add(TASK_AND_QUEUE_METADATA_BYTES))
        .ok_or(ImportError::Overflow)?;
    if explicit_validity {
        let logical_voxels = pixel_inner / u64::from(dtype.bytes_per_sample());
        bytes = bytes
            .checked_add(logical_voxels)
            .and_then(|value| value.checked_add(as_u64(validity_kind.decoded_inner_bytes()).ok()?))
            .and_then(|value| {
                value.checked_add(as_u64(validity_kind.encoded_inner_bytes_max()).ok()?)
            })
            .ok_or(ImportError::Overflow)?;
    }
    Ok(bytes)
}

pub(crate) fn pyramid_task_charge_bytes(
    dtype: IntensityDType,
    pixel_kind: ShardProfileKind,
    validity_kind: ShardProfileKind,
    is_2d: bool,
    explicit_validity: bool,
) -> Result<u64, ImportError> {
    let source_multiplier = if is_2d { 4_u64 } else { 8_u64 };
    let pixel_inner = as_u64(pixel_kind.decoded_inner_bytes())?;
    let pixel_encoded = as_u64(pixel_kind.encoded_inner_bytes_max())?;
    let mut bytes = pixel_inner
        .checked_mul(source_multiplier)
        .and_then(|value| value.checked_add(pixel_inner)) // one decoded spool chunk
        .and_then(|value| value.checked_add(pixel_encoded)) // one encoded spool chunk
        .and_then(|value| value.checked_add(pixel_inner)) // compact target
        .and_then(|value| value.checked_add(pixel_inner)) // padded target
        .and_then(|value| value.checked_add(pixel_encoded))
        .and_then(|value| value.checked_add(INNER_CODEC_WORKING_BYTES_MAX))
        .and_then(|value| value.checked_add(TASK_AND_QUEUE_METADATA_BYTES))
        .ok_or(ImportError::Overflow)?;
    if explicit_validity {
        let target_voxels = pixel_inner / u64::from(dtype.bytes_per_sample());
        let validity_inner = as_u64(validity_kind.decoded_inner_bytes())?;
        bytes = bytes
            .checked_add(
                target_voxels
                    .checked_mul(source_multiplier)
                    .ok_or(ImportError::Overflow)?,
            )
            .and_then(|value| value.checked_add(validity_inner)) // decoded spool mask
            .and_then(|value| {
                value.checked_add(as_u64(validity_kind.encoded_inner_bytes_max()).ok()?)
            }) // encoded spool mask
            .and_then(|value| value.checked_add(target_voxels)) // target byte mask
            .and_then(|value| value.checked_add(validity_inner)) // packed target mask
            .and_then(|value| {
                value.checked_add(as_u64(validity_kind.encoded_inner_bytes_max()).ok()?)
            })
            .ok_or(ImportError::Overflow)?;
    }
    Ok(bytes)
}

pub(crate) fn scientific_task_charge_bytes(dtype: IntensityDType) -> Result<u64, ImportError> {
    let voxels = SCIENTIFIC_TILE_SHAPE_TZYX[1]
        .checked_mul(SCIENTIFIC_TILE_SHAPE_TZYX[2])
        .and_then(|value| value.checked_mul(SCIENTIFIC_TILE_SHAPE_TZYX[3]))
        .ok_or(ImportError::Overflow)?;
    voxels
        .checked_mul(u64::from(dtype.bytes_per_sample()))
        .and_then(|value| value.checked_add(voxels))
        .and_then(|value| value.checked_add(voxels.div_ceil(8)))
        .and_then(|value| value.checked_add(TASK_AND_QUEUE_METADATA_BYTES))
        .ok_or(ImportError::Overflow)
}

#[allow(clippy::too_many_arguments)]
fn copy_mask_region(
    source: &[u8],
    source_shape: [u64; 3],
    source_origin: [u64; 3],
    extent: [u64; 3],
    destination: &mut [u8],
    destination_shape: [u64; 3],
    destination_origin: [u64; 3],
) -> Result<(), ImportError> {
    if source.len() != checked_voxels(source_shape)?
        || destination.len() != checked_voxels(destination_shape)?
    {
        return Err(ImportError::InvalidCheckpoint(
            "validity region length differs from its shape".to_owned(),
        ));
    }
    for z in 0..extent[0] {
        for y in 0..extent[1] {
            for x in 0..extent[2] {
                let source_index = linear_index(
                    source_shape,
                    [
                        source_origin[0] + z,
                        source_origin[1] + y,
                        source_origin[2] + x,
                    ],
                )?;
                let destination_index = linear_index(
                    destination_shape,
                    [
                        destination_origin[0] + z,
                        destination_origin[1] + y,
                        destination_origin[2] + x,
                    ],
                )?;
                destination[destination_index] = source[source_index];
            }
        }
    }
    Ok(())
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
    }
}
