//! Canonical inner-chunk assembly and packed-index facts.

use mirante4d_domain::IntensityDType;
use mirante4d_storage::{
    PackedIndexCoordinates, PackedIndexRecord, PackedIndexStatistics, ShardProfileKind,
    StorageShape,
};

use crate::ImportError;

#[derive(Clone, Debug)]
pub(crate) struct PreparedChunk {
    pub pixel: Option<Vec<u8>>,
    pub validity: Option<Vec<u8>>,
    pub record: PackedIndexRecord,
}

pub(crate) const fn pixel_kind(dtype: IntensityDType, is_2d: bool) -> ShardProfileKind {
    match (is_2d, dtype) {
        (false, IntensityDType::Uint8) => ShardProfileKind::Pixel3dUint8,
        (false, IntensityDType::Uint16) => ShardProfileKind::Pixel3dUint16,
        (false, IntensityDType::Float32) => ShardProfileKind::Pixel3dFloat32,
        (true, IntensityDType::Uint8) => ShardProfileKind::Pixel2dUint8,
        (true, IntensityDType::Uint16) => ShardProfileKind::Pixel2dUint16,
        (true, IntensityDType::Float32) => ShardProfileKind::Pixel2dFloat32,
    }
}

pub(crate) const fn validity_kind(is_2d: bool) -> ShardProfileKind {
    if is_2d {
        ShardProfileKind::Validity2d
    } else {
        ShardProfileKind::Validity3d
    }
}

pub(crate) const fn chunk_shape(is_2d: bool) -> [u64; 3] {
    let shape = if is_2d {
        StorageShape::PIXEL_2D.inner_tczyx
    } else {
        StorageShape::PIXEL_3D.inner_tczyx
    };
    [shape[2], shape[3], shape[4]]
}

pub(crate) fn chunk_grid(shape_zyx: [u64; 3], is_2d: bool) -> [u64; 3] {
    let inner = chunk_shape(is_2d);
    [
        shape_zyx[0].div_ceil(inner[0]),
        shape_zyx[1].div_ceil(inner[1]),
        shape_zyx[2].div_ceil(inner[2]),
    ]
}

pub(crate) fn chunk_extent(
    shape_zyx: [u64; 3],
    coordinates_zyx: [u64; 3],
    is_2d: bool,
) -> Result<[u64; 3], ImportError> {
    let inner = chunk_shape(is_2d);
    let mut extent = [0; 3];
    for axis in 0..3 {
        let origin = coordinates_zyx[axis]
            .checked_mul(inner[axis])
            .ok_or(ImportError::Overflow)?;
        if origin >= shape_zyx[axis] {
            return Err(ImportError::InvalidRequest(
                "chunk coordinates lie outside the level shape",
            ));
        }
        extent[axis] = inner[axis].min(shape_zyx[axis] - origin);
    }
    Ok(extent)
}

pub(crate) fn checked_voxels(shape_zyx: [u64; 3]) -> Result<usize, ImportError> {
    if shape_zyx.contains(&0) {
        return Err(ImportError::InvalidRequest(
            "pixel-region dimensions must be positive",
        ));
    }
    let count = shape_zyx
        .into_iter()
        .try_fold(1_u64, |product, value| product.checked_mul(value))
        .ok_or(ImportError::Overflow)?;
    usize::try_from(count).map_err(|_| ImportError::Overflow)
}

pub(crate) fn compact_byte_len(
    dtype: IntensityDType,
    shape_zyx: [u64; 3],
) -> Result<usize, ImportError> {
    checked_voxels(shape_zyx)?
        .checked_mul(usize::from(dtype.bytes_per_sample()))
        .ok_or(ImportError::Overflow)
}

/// Converts an exact logical region into one fixed target inner chunk.
///
/// `validity` uses one byte per logical voxel. Invalid samples must already
/// carry the canonical zero value. Padding outside `logical_shape_zyx` is
/// always target fill and never contributes to packed-index statistics.
pub(crate) fn prepare_chunk(
    coordinates: PackedIndexCoordinates,
    dtype: IntensityDType,
    is_2d: bool,
    logical_shape_zyx: [u64; 3],
    compact_pixels_le: &[u8],
    validity: Option<&[u8]>,
) -> Result<PreparedChunk, ImportError> {
    let logical_voxels = checked_voxels(logical_shape_zyx)?;
    let bytes_per_sample = usize::from(dtype.bytes_per_sample());
    if compact_pixels_le.len()
        != logical_voxels
            .checked_mul(bytes_per_sample)
            .ok_or(ImportError::Overflow)?
    {
        return Err(ImportError::InvalidRequest(
            "compact pixel bytes do not match the logical chunk extent",
        ));
    }
    if validity.is_some_and(|mask| mask.len() != logical_voxels) {
        return Err(ImportError::InvalidRequest(
            "compact validity bytes do not match the logical chunk extent",
        ));
    }

    let inner = chunk_shape(is_2d);
    if (0..3).any(|axis| logical_shape_zyx[axis] > inner[axis]) {
        return Err(ImportError::InvalidRequest(
            "logical chunk extent exceeds the target inner chunk",
        ));
    }

    let kind = pixel_kind(dtype, is_2d);
    let mut padded = vec![0; kind.decoded_inner_bytes()];
    copy_compact_into_padded(
        compact_pixels_le,
        &mut padded,
        logical_shape_zyx,
        inner,
        bytes_per_sample,
    )?;

    let statistics = statistics(dtype, compact_pixels_le, validity)?;
    let pixel_present = statistics.nonfill_valid_voxel_count() != 0;
    let explicit_validity = validity.is_some();
    let logical_capacity = u64::try_from(logical_voxels).map_err(|_| ImportError::Overflow)?;
    let record = PackedIndexRecord::new(
        coordinates,
        statistics,
        pixel_present,
        explicit_validity,
        dtype,
        logical_capacity,
    )?;

    let validity = match validity {
        None => None,
        Some(_) if statistics.valid_voxel_count() == 0 => None,
        Some(mask) => Some(pack_validity(mask, logical_shape_zyx, inner, is_2d)?),
    };

    Ok(PreparedChunk {
        pixel: pixel_present.then_some(padded),
        validity,
        record,
    })
}

pub(crate) fn normalize_u8_sentinel(pixels: &mut [u8], sentinel: u8) -> Vec<u8> {
    pixels
        .iter_mut()
        .map(|value| {
            let valid = *value != sentinel;
            if !valid {
                *value = 0;
            }
            u8::from(valid)
        })
        .collect()
}

pub(crate) fn unpack_validity(
    packed: &[u8],
    logical_shape_zyx: [u64; 3],
    is_2d: bool,
) -> Result<Vec<u8>, ImportError> {
    let expected = validity_kind(is_2d).decoded_inner_bytes();
    if packed.len() != expected {
        return Err(ImportError::InvalidCheckpoint(
            "decoded validity chunk has the wrong byte length".to_owned(),
        ));
    }
    let inner = chunk_shape(is_2d);
    let mut output = Vec::with_capacity(checked_voxels(logical_shape_zyx)?);
    let row_bytes = usize::try_from(inner[2].div_ceil(8)).map_err(|_| ImportError::Overflow)?;
    for z in 0..logical_shape_zyx[0] {
        for y in 0..logical_shape_zyx[1] {
            let row = usize::try_from(
                z.checked_mul(inner[1])
                    .and_then(|value| value.checked_add(y))
                    .ok_or(ImportError::Overflow)?,
            )
            .map_err(|_| ImportError::Overflow)?;
            let offset = row.checked_mul(row_bytes).ok_or(ImportError::Overflow)?;
            for x in 0..logical_shape_zyx[2] {
                let byte = usize::try_from(x / 8).map_err(|_| ImportError::Overflow)?;
                let bit = u8::try_from(x % 8).expect("modulo eight fits u8");
                output.push(u8::from(packed[offset + byte] & (1 << bit) != 0));
            }
        }
    }
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn copy_padded_region(
    padded: &[u8],
    inner_shape_zyx: [u64; 3],
    source_origin_zyx: [u64; 3],
    extent_zyx: [u64; 3],
    bytes_per_sample: usize,
    destination: &mut [u8],
    destination_shape_zyx: [u64; 3],
    destination_origin_zyx: [u64; 3],
) -> Result<(), ImportError> {
    let expected_source = checked_voxels(inner_shape_zyx)?
        .checked_mul(bytes_per_sample)
        .ok_or(ImportError::Overflow)?;
    let expected_destination = checked_voxels(destination_shape_zyx)?
        .checked_mul(bytes_per_sample)
        .ok_or(ImportError::Overflow)?;
    if padded.len() != expected_source || destination.len() != expected_destination {
        return Err(ImportError::InvalidCheckpoint(
            "decoded pixel chunk has an unexpected byte length".to_owned(),
        ));
    }
    for z in 0..extent_zyx[0] {
        for y in 0..extent_zyx[1] {
            let source_sample = linear_index(
                inner_shape_zyx,
                [
                    source_origin_zyx[0] + z,
                    source_origin_zyx[1] + y,
                    source_origin_zyx[2],
                ],
            )?;
            let destination_sample = linear_index(
                destination_shape_zyx,
                [
                    destination_origin_zyx[0] + z,
                    destination_origin_zyx[1] + y,
                    destination_origin_zyx[2],
                ],
            )?;
            let bytes = usize::try_from(extent_zyx[2])
                .map_err(|_| ImportError::Overflow)?
                .checked_mul(bytes_per_sample)
                .ok_or(ImportError::Overflow)?;
            let source_byte = source_sample
                .checked_mul(bytes_per_sample)
                .ok_or(ImportError::Overflow)?;
            let destination_byte = destination_sample
                .checked_mul(bytes_per_sample)
                .ok_or(ImportError::Overflow)?;
            destination[destination_byte..destination_byte + bytes]
                .copy_from_slice(&padded[source_byte..source_byte + bytes]);
        }
    }
    Ok(())
}

fn copy_compact_into_padded(
    compact: &[u8],
    padded: &mut [u8],
    logical: [u64; 3],
    inner: [u64; 3],
    bytes_per_sample: usize,
) -> Result<(), ImportError> {
    for z in 0..logical[0] {
        for y in 0..logical[1] {
            let source = linear_index(logical, [z, y, 0])?
                .checked_mul(bytes_per_sample)
                .ok_or(ImportError::Overflow)?;
            let target = linear_index(inner, [z, y, 0])?
                .checked_mul(bytes_per_sample)
                .ok_or(ImportError::Overflow)?;
            let bytes = usize::try_from(logical[2])
                .map_err(|_| ImportError::Overflow)?
                .checked_mul(bytes_per_sample)
                .ok_or(ImportError::Overflow)?;
            padded[target..target + bytes].copy_from_slice(&compact[source..source + bytes]);
        }
    }
    Ok(())
}

fn statistics(
    dtype: IntensityDType,
    pixels: &[u8],
    validity: Option<&[u8]>,
) -> Result<PackedIndexStatistics, ImportError> {
    let width = usize::from(dtype.bytes_per_sample());
    let mut valid = 0_u64;
    let mut nonfill = 0_u64;
    let mut minimum: Option<(u64, f64)> = None;
    let mut maximum: Option<(u64, f64)> = None;

    for (index, sample) in pixels.chunks_exact(width).enumerate() {
        let is_valid = match validity {
            Some(mask) => match mask[index] {
                0 => false,
                1 => true,
                _ => {
                    return Err(ImportError::InvalidRequest(
                        "validity bytes must be canonical 0 or 1",
                    ));
                }
            },
            None => true,
        };
        if !is_valid {
            if sample.iter().any(|byte| *byte != 0) {
                return Err(ImportError::InvalidRequest(
                    "invalid samples must use the canonical zero sentinel",
                ));
            }
            continue;
        }
        let (bits, numeric, is_fill) = sample_facts(dtype, sample)?;
        valid = valid.checked_add(1).ok_or(ImportError::Overflow)?;
        if !is_fill {
            nonfill = nonfill.checked_add(1).ok_or(ImportError::Overflow)?;
        }
        if minimum.is_none_or(|(_, value)| numeric < value) {
            minimum = Some((bits, numeric));
        }
        if maximum.is_none_or(|(_, value)| numeric > value) {
            maximum = Some((bits, numeric));
        }
    }

    let range = minimum
        .zip(maximum)
        .map(|(minimum, maximum)| (minimum.0, maximum.0));
    Ok(PackedIndexStatistics::new(valid, nonfill, range))
}

fn sample_facts(dtype: IntensityDType, bytes: &[u8]) -> Result<(u64, f64, bool), ImportError> {
    match dtype {
        IntensityDType::Uint8 => {
            let value = bytes[0];
            Ok((u64::from(value), f64::from(value), value == 0))
        }
        IntensityDType::Uint16 => {
            let value = u16::from_le_bytes([bytes[0], bytes[1]]);
            Ok((u64::from(value), f64::from(value), value == 0))
        }
        IntensityDType::Float32 => {
            let value = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            if !value.is_finite() {
                return Err(ImportError::InvalidRequest("float samples must be finite"));
            }
            let bits = value.to_bits();
            Ok((u64::from(bits), f64::from(value), bits == 0))
        }
    }
}

fn pack_validity(
    mask: &[u8],
    logical: [u64; 3],
    inner: [u64; 3],
    is_2d: bool,
) -> Result<Vec<u8>, ImportError> {
    let mut packed = vec![0; validity_kind(is_2d).decoded_inner_bytes()];
    let row_bytes = usize::try_from(inner[2].div_ceil(8)).map_err(|_| ImportError::Overflow)?;
    for z in 0..logical[0] {
        for y in 0..logical[1] {
            let source = linear_index(logical, [z, y, 0])?;
            let row = usize::try_from(
                z.checked_mul(inner[1])
                    .and_then(|value| value.checked_add(y))
                    .ok_or(ImportError::Overflow)?,
            )
            .map_err(|_| ImportError::Overflow)?;
            let target = row.checked_mul(row_bytes).ok_or(ImportError::Overflow)?;
            for x in 0..logical[2] {
                match mask[source + usize::try_from(x).map_err(|_| ImportError::Overflow)?] {
                    0 => {}
                    1 => {
                        let byte = usize::try_from(x / 8).map_err(|_| ImportError::Overflow)?;
                        let bit = u8::try_from(x % 8).expect("modulo eight fits u8");
                        packed[target + byte] |= 1 << bit;
                    }
                    _ => {
                        return Err(ImportError::InvalidRequest(
                            "validity bytes must be canonical 0 or 1",
                        ));
                    }
                }
            }
        }
    }
    Ok(packed)
}

fn linear_index(shape: [u64; 3], coordinate: [u64; 3]) -> Result<usize, ImportError> {
    let value = coordinate[0]
        .checked_mul(shape[1])
        .and_then(|value| value.checked_add(coordinate[1]))
        .and_then(|value| value.checked_mul(shape[2]))
        .and_then(|value| value.checked_add(coordinate[2]))
        .ok_or(ImportError::Overflow)?;
    usize::try_from(value).map_err(|_| ImportError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_invalid_values_are_zeroed_and_packed() {
        let coordinates = PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0);
        let prepared = prepare_chunk(
            coordinates,
            IntensityDType::Uint8,
            true,
            [1, 1, 4],
            &[7, 0, 9, 0],
            Some(&[1, 0, 1, 0]),
        )
        .unwrap();
        assert_eq!(prepared.record.statistics().valid_voxel_count(), 2);
        assert_eq!(prepared.record.statistics().nonfill_valid_voxel_count(), 2);
        assert_eq!(
            prepared.record.statistics().numeric_range_bits(),
            Some((7, 9))
        );
        let validity = prepared.validity.unwrap();
        assert_eq!(validity[0], 0b0000_0101);
        assert_eq!(
            unpack_validity(&validity, [1, 1, 4], true).unwrap(),
            vec![1, 0, 1, 0]
        );
    }

    #[test]
    fn all_zero_chunks_use_profile_fill_elision() {
        let prepared = prepare_chunk(
            PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0),
            IntensityDType::Uint16,
            false,
            [1, 1, 2],
            &[0; 4],
            None,
        )
        .unwrap();
        assert!(prepared.pixel.is_none());
        assert!(prepared.validity.is_none());
        assert_eq!(
            prepared.record.statistics().numeric_range_bits(),
            Some((0, 0))
        );
    }

    #[test]
    fn sentinel_normalization_uses_scientific_zero_for_invalid_samples() {
        let mut pixels = [4, 255, 8];
        let validity = normalize_u8_sentinel(&mut pixels, 255);
        assert_eq!(pixels, [4, 0, 8]);
        assert_eq!(validity, [1, 0, 1]);
    }

    #[test]
    fn negative_float_zero_is_not_elided_as_profile_fill() {
        let prepared = prepare_chunk(
            PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0),
            IntensityDType::Float32,
            true,
            [1, 1, 1],
            &(-0.0_f32).to_le_bytes(),
            None,
        )
        .unwrap();
        assert!(prepared.pixel.is_some());
        assert_eq!(
            prepared.record.statistics().numeric_range_bits(),
            Some((
                u64::from((-0.0_f32).to_bits()),
                u64::from((-0.0_f32).to_bits())
            ))
        );
    }
}
