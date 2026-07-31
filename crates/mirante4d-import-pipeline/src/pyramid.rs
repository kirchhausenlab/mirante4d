//! Bounded factor-two multiscale production.

use mirante4d_domain::{IntensityDType, Shape4D};
#[cfg(test)]
use mirante4d_storage::StorageShape;

use crate::ImportError;

const STOP_MAX_DIMENSION: u64 = 64;
const STOP_VOXELS_PER_TIMEPOINT: u64 = 262_144;

/// Result of reducing one caller-owned dense source region.
///
/// Pixels use the target profile's canonical little-endian representation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DownsampledRegion {
    pub shape_zyx: [u64; 3],
    pub pixels_le: Vec<u8>,
}

/// Builds the deterministic spatial pyramid used by the import producer.
///
/// Time is never reduced. Each spatial dimension is ceil-divided by two until
/// the terminal shape independently bounds both maximum ray traversal and
/// decoded volume size. Scale count is therefore a deterministic consequence
/// of source geometry, never a fixed profile tier or runtime hardware probe.
pub(crate) fn pyramid_shapes(base: Shape4D) -> Result<Vec<Shape4D>, ImportError> {
    let mut shapes = vec![base];
    if is_terminal(base) {
        return Ok(shapes);
    }

    loop {
        let previous = *shapes.last().expect("the base shape was inserted");
        let next = factor_two_shape(previous)?;
        if next == previous {
            return Err(ImportError::InvalidRequest(
                "spatial pyramid did not make progress toward its terminal geometry",
            ));
        }
        shapes.push(next);
        if is_terminal(next) {
            return Ok(shapes);
        }
    }
}

/// Returns the fixed target pixel-chunk shape selected by source dimensionality.
#[cfg(test)]
pub(crate) const fn pixel_chunk_shape(shape: Shape4D) -> [u64; 3] {
    let storage = if shape.z() == 1 {
        StorageShape::PIXEL_2D
    } else {
        StorageShape::PIXEL_3D
    };
    [
        storage.inner_tczyx[2],
        storage.inner_tczyx[3],
        storage.inner_tczyx[4],
    ]
}

/// Decimates one bounded dense Z/Y/X source region by a factor of two.
///
/// `pixels_le` contains tightly packed canonical samples. Each output copies
/// the source sample at its even factor-two origin. Ceil division retains the
/// final source coordinate when a dimension is odd, matching a factor-two
/// scale with constant translation. Explicit-validity uint8 imports use the
/// guarded predecessor reducer in `sentinel`; this helper is intentionally
/// incapable of selecting the obsolete exact-only validity policy.
pub(crate) fn downsample_region(
    dtype: IntensityDType,
    source_shape_zyx: [u64; 3],
    pixels_le: &[u8],
) -> Result<DownsampledRegion, ImportError> {
    let source_voxels = checked_voxels(source_shape_zyx)?;
    let bytes_per_sample = usize::from(dtype.bytes_per_sample());
    let expected_pixel_bytes = source_voxels
        .checked_mul(bytes_per_sample)
        .ok_or(ImportError::Overflow)?;
    if pixels_le.len() != expected_pixel_bytes {
        return Err(ImportError::InvalidRequest(
            "downsample source pixel length does not match its shape",
        ));
    }
    if dtype == IntensityDType::Float32
        && pixels_le
            .chunks_exact(4)
            .any(|bytes| !f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]).is_finite())
    {
        return Err(ImportError::InvalidRequest(
            "downsample float samples must be finite",
        ));
    }

    let output_shape_zyx = source_shape_zyx.map(|dimension| dimension.div_ceil(2));
    let output_voxels = checked_voxels(output_shape_zyx)?;
    let output_bytes = output_voxels
        .checked_mul(bytes_per_sample)
        .ok_or(ImportError::Overflow)?;
    let mut output = Vec::with_capacity(output_bytes);

    for output_z in 0..output_shape_zyx[0] {
        for output_y in 0..output_shape_zyx[1] {
            for output_x in 0..output_shape_zyx[2] {
                let origin = [output_z * 2, output_y * 2, output_x * 2];
                let index = source_index(source_shape_zyx, origin[0], origin[1], origin[2])?;
                let byte_start = index
                    .checked_mul(bytes_per_sample)
                    .ok_or(ImportError::Overflow)?;
                output.extend_from_slice(&pixels_le[byte_start..byte_start + bytes_per_sample]);
            }
        }
    }

    debug_assert_eq!(output.len(), output_bytes);
    Ok(DownsampledRegion {
        shape_zyx: output_shape_zyx,
        pixels_le: output,
    })
}

fn factor_two_shape(shape: Shape4D) -> Result<Shape4D, ImportError> {
    Shape4D::new(
        shape.t(),
        shape.z().div_ceil(2),
        shape.y().div_ceil(2),
        shape.x().div_ceil(2),
    )
    .map_err(|_| ImportError::Overflow)
}

const fn maximum_spatial_dimension(shape: Shape4D) -> u64 {
    let maximum_zy = if shape.z() > shape.y() {
        shape.z()
    } else {
        shape.y()
    };
    if maximum_zy > shape.x() {
        maximum_zy
    } else {
        shape.x()
    }
}

fn is_terminal(shape: Shape4D) -> bool {
    maximum_spatial_dimension(shape) <= STOP_MAX_DIMENSION
        && shape
            .z()
            .checked_mul(shape.y())
            .and_then(|zy| zy.checked_mul(shape.x()))
            .is_some_and(|voxels| voxels <= STOP_VOXELS_PER_TIMEPOINT)
}

fn checked_voxels(shape_zyx: [u64; 3]) -> Result<usize, ImportError> {
    if shape_zyx.contains(&0) {
        return Err(ImportError::InvalidRequest(
            "downsample source dimensions must be positive",
        ));
    }
    let voxels = shape_zyx
        .into_iter()
        .try_fold(1_u64, |product, dimension| product.checked_mul(dimension))
        .ok_or(ImportError::Overflow)?;
    usize::try_from(voxels).map_err(|_| ImportError::Overflow)
}

fn source_index(shape_zyx: [u64; 3], z: u64, y: u64, x: u64) -> Result<usize, ImportError> {
    let index = z
        .checked_mul(shape_zyx[1])
        .and_then(|zy| zy.checked_add(y))
        .and_then(|zy| zy.checked_mul(shape_zyx[2]))
        .and_then(|zyx| zyx.checked_add(x))
        .ok_or(ImportError::Overflow)?;
    usize::try_from(index).map_err(|_| ImportError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_bytes(values: &[u16]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    fn f32_bytes(values: &[f32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    fn decode_u16(bytes: &[u8]) -> Vec<u16> {
        bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect()
    }

    fn decode_f32(bytes: &[u8]) -> Vec<f32> {
        bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect()
    }

    #[test]
    fn pyramid_geometry_matches_the_bounded_import_policy() {
        let tiny = Shape4D::new(3, 8, 31, 256).unwrap();
        assert_eq!(
            pyramid_shapes(tiny).unwrap(),
            vec![
                tiny,
                Shape4D::new(3, 4, 16, 128).unwrap(),
                Shape4D::new(3, 2, 8, 64).unwrap(),
            ]
        );

        let large = Shape4D::new(3, 65, 300, 300).unwrap();
        assert_eq!(
            pyramid_shapes(large).unwrap(),
            vec![
                large,
                Shape4D::new(3, 33, 150, 150).unwrap(),
                Shape4D::new(3, 17, 75, 75).unwrap(),
                Shape4D::new(3, 9, 38, 38).unwrap(),
            ]
        );
    }

    #[test]
    fn terminal_geometry_requires_both_volume_and_longest_dimension_bounds() {
        let already_terminal = Shape4D::new(2, 64, 64, 64).unwrap();
        assert_eq!(
            pyramid_shapes(already_terminal).unwrap(),
            vec![already_terminal]
        );

        let long_thin = Shape4D::new(1, 1, 1, 1_000_000).unwrap();
        let shapes = pyramid_shapes(long_thin).unwrap();
        let terminal = *shapes.last().unwrap();
        assert!(maximum_spatial_dimension(terminal) <= STOP_MAX_DIMENSION);
        assert!(
            terminal
                .z()
                .checked_mul(terminal.y())
                .and_then(|zy| zy.checked_mul(terminal.x()))
                .unwrap()
                <= STOP_VOXELS_PER_TIMEPOINT
        );
        assert!(
            maximum_spatial_dimension(shapes[shapes.len() - 2]) > STOP_MAX_DIMENSION,
            "a small voxel count cannot terminate a pathologically long volume"
        );
    }

    #[test]
    fn geometry_can_require_more_than_the_legacy_seven_level_ceiling() {
        let base = Shape4D::new(1, 1, 1, 1_048_576).unwrap();
        let expected_x = [
            1_048_576, 524_288, 262_144, 131_072, 65_536, 32_768, 16_384, 8_192, 4_096, 2_048,
            1_024, 512, 256, 128, 64,
        ];
        let expected = expected_x
            .into_iter()
            .map(|x| Shape4D::new(1, 1, 1, x).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(pyramid_shapes(base).unwrap(), expected);
    }

    #[test]
    fn every_representable_dimension_reaches_terminal_geometry_within_64_levels() {
        let base = Shape4D::new(1, 1, 1, u64::MAX).unwrap();
        let shapes = pyramid_shapes(base).unwrap();

        assert_eq!(shapes.len(), 59);
        assert_eq!(
            shapes.last().unwrap().dimensions(),
            [1, 1, 1, STOP_MAX_DIMENSION]
        );
    }

    #[test]
    fn profile_chunk_shape_preserves_2d_and_3d_selection() {
        assert_eq!(
            pixel_chunk_shape(Shape4D::new(1, 1, 999, 999).unwrap()),
            [1, 256, 256]
        );
        assert_eq!(
            pixel_chunk_shape(Shape4D::new(1, 2, 2, 2).unwrap()),
            [64, 64, 64]
        );
    }

    #[test]
    fn uint8_decimation_selects_even_origins_and_retains_odd_edges() {
        let reduced = downsample_region(
            IntensityDType::Uint8,
            [1, 3, 3],
            &[0, 1, 2, 3, 4, 5, 6, 7, 9],
        )
        .unwrap();

        assert_eq!(reduced.shape_zyx, [1, 2, 2]);
        assert_eq!(reduced.pixels_le, vec![0, 2, 6, 9]);
    }

    #[test]
    fn uint16_decimation_preserves_selected_sample_bits() {
        let reduced = downsample_region(
            IntensityDType::Uint16,
            [3, 2, 2],
            &u16_bytes(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]),
        )
        .unwrap();

        assert_eq!(decode_u16(&reduced.pixels_le), vec![0, 8]);
    }

    #[test]
    fn float_decimation_preserves_bits_and_rejects_non_finite_input() {
        let reduced = downsample_region(
            IntensityDType::Float32,
            [1, 1, 3],
            &f32_bytes(&[f32::MAX, -f32::MAX, 3.0]),
        )
        .unwrap();
        assert_eq!(decode_f32(&reduced.pixels_le), vec![f32::MAX, 3.0]);

        let error = downsample_region(IntensityDType::Float32, [1, 1, 1], &f32_bytes(&[f32::NAN]))
            .unwrap_err();
        assert!(matches!(error, ImportError::InvalidRequest(_)));
    }

    #[test]
    fn malformed_dense_regions_are_rejected() {
        assert!(downsample_region(IntensityDType::Uint16, [1, 1, 2], &[0; 2]).is_err());
        assert!(downsample_region(IntensityDType::Uint8, [0, 1, 1], &[]).is_err());
    }
}
