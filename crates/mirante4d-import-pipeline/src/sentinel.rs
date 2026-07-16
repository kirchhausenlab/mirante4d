//! Exact guarded uint8-sentinel semantics for base and multiscale production.

use crate::{ImportError, chunk::checked_voxels};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region3 {
    pub(crate) origin: [u64; 3],
    pub(crate) shape: [u64; 3],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GuardedU8Region {
    pub(crate) pixels: Vec<u8>,
    pub(crate) validity: Vec<u8>,
}

pub(crate) fn clipped_halo(
    core: Region3,
    full_shape: [u64; 3],
    radius: [u64; 3],
) -> Result<Region3, ImportError> {
    validate_region(core, full_shape, "halo core")?;
    let mut origin = [0; 3];
    let mut end = [0; 3];
    for axis in 0..3 {
        origin[axis] = core.origin[axis].saturating_sub(radius[axis]);
        end[axis] = core.origin[axis]
            .checked_add(core.shape[axis])
            .and_then(|value| value.checked_add(radius[axis]))
            .ok_or(ImportError::Overflow)?
            .min(full_shape[axis]);
    }
    Ok(Region3 {
        origin,
        shape: [end[0] - origin[0], end[1] - origin[1], end[2] - origin[2]],
    })
}

pub(crate) fn invalid_dilation_radius(full_shape: [u64; 3]) -> [u64; 3] {
    [u64::from(full_shape[0] > 1), 1, 1]
}

/// Applies exact source-sentinel classification and one-voxel invalid
/// dilation, returning only the requested core in canonical form.
pub(crate) fn guarded_u8_core(
    window_pixels: &[u8],
    window: Region3,
    full_shape: [u64; 3],
    core: Region3,
    sentinel: u8,
) -> Result<GuardedU8Region, ImportError> {
    validate_region(window, full_shape, "sentinel window")?;
    validate_region(core, full_shape, "sentinel core")?;
    require_contains(window, core, "sentinel window does not contain its core")?;
    if window_pixels.len() != checked_voxels(window.shape)? {
        return Err(ImportError::InvalidRequest(
            "sentinel window pixel length does not match its shape",
        ));
    }

    let core_voxels = checked_voxels(core.shape)?;
    let mut pixels = Vec::with_capacity(core_voxels);
    let mut validity = Vec::with_capacity(core_voxels);
    let mut saw_valid = false;
    let mut saw_invalid = false;
    for value in window_pixels {
        if *value == sentinel {
            saw_invalid = true;
        } else {
            saw_valid = true;
        }
    }

    if !saw_invalid {
        copy_u8_core(window_pixels, window, core, &mut pixels)?;
        validity.resize(core_voxels, 1);
        return Ok(GuardedU8Region { pixels, validity });
    }
    if !saw_valid {
        pixels.resize(core_voxels, 0);
        validity.resize(core_voxels, 0);
        return Ok(GuardedU8Region { pixels, validity });
    }

    // Scatter each source-invalid sample into the affected core neighborhood.
    // This is exactly the same Chebyshev dilation as gathering up to 27
    // neighbors per output, but makes the common sparse-sentinel case linear
    // in the halo plus the number of affected samples without another dense
    // mask allocation.
    validity.resize(core_voxels, 1);
    let radius = invalid_dilation_radius(full_shape);
    for z in 0..window.shape[0] {
        for y in 0..window.shape[1] {
            for x in 0..window.shape[2] {
                let local = [z, y, x];
                if window_pixels[linear_index(window.shape, local)?] != sentinel {
                    continue;
                }
                let global = [
                    window.origin[0] + z,
                    window.origin[1] + y,
                    window.origin[2] + x,
                ];
                invalidate_core_neighborhood(&mut validity, core, global, radius)?;
            }
        }
    }
    copy_u8_core(window_pixels, window, core, &mut pixels)?;
    for (pixel, valid) in pixels.iter_mut().zip(&validity) {
        if *valid == 0 {
            *pixel = 0;
        }
    }
    Ok(GuardedU8Region { pixels, validity })
}

/// Reduces a sentinel-bearing uint8 level using predecessor-equivalent
/// valid-only means followed by one child-voxel invalid dilation.
#[allow(clippy::too_many_arguments)]
pub(crate) fn downsample_guarded_u8(
    parent_pixels: &[u8],
    pixel_region: Region3,
    parent_validity: &[u8],
    validity_region: Region3,
    parent_full_shape: [u64; 3],
    target_full_shape: [u64; 3],
    target_core: Region3,
) -> Result<GuardedU8Region, ImportError> {
    validate_region(pixel_region, parent_full_shape, "parent pixel region")?;
    validate_region(validity_region, parent_full_shape, "parent validity region")?;
    validate_region(target_core, target_full_shape, "target core")?;
    for axis in 0..3 {
        if target_full_shape[axis] != parent_full_shape[axis].div_ceil(2) {
            return Err(ImportError::InvalidRequest(
                "target shape is not the factor-two parent shape",
            ));
        }
    }
    if parent_pixels.len() != checked_voxels(pixel_region.shape)?
        || parent_validity.len() != checked_voxels(validity_region.shape)?
    {
        return Err(ImportError::InvalidRequest(
            "guarded downsample region length does not match its shape",
        ));
    }
    if parent_validity.iter().any(|value| !matches!(value, 0 | 1)) {
        return Err(ImportError::InvalidRequest(
            "guarded downsample validity must contain canonical bytes",
        ));
    }

    let target_halo = clipped_halo(
        target_core,
        target_full_shape,
        invalid_dilation_radius(target_full_shape),
    )?;
    let support_voxels = checked_voxels(target_halo.shape)?;
    let mut support = Vec::with_capacity(support_voxels);
    for z in 0..target_halo.shape[0] {
        for y in 0..target_halo.shape[1] {
            for x in 0..target_halo.shape[2] {
                let target = [
                    target_halo.origin[0] + z,
                    target_halo.origin[1] + y,
                    target_halo.origin[2] + x,
                ];
                let (sum, count) = parent_block_sum_count(
                    None,
                    pixel_region,
                    parent_validity,
                    validity_region,
                    parent_full_shape,
                    target,
                )?;
                debug_assert_eq!(sum, 0);
                support.push(u8::from(count != 0));
            }
        }
    }

    let core_voxels = checked_voxels(target_core.shape)?;
    if support.iter().all(|value| *value == 0) {
        return Ok(GuardedU8Region {
            pixels: vec![0; core_voxels],
            validity: vec![0; core_voxels],
        });
    }
    let mut pixels = Vec::with_capacity(core_voxels);
    let mut validity = vec![1; core_voxels];
    let radius = invalid_dilation_radius(target_full_shape);
    if !support.iter().all(|value| *value == 1) {
        for z in 0..target_halo.shape[0] {
            for y in 0..target_halo.shape[1] {
                for x in 0..target_halo.shape[2] {
                    let local = [z, y, x];
                    if support[linear_index(target_halo.shape, local)?] != 0 {
                        continue;
                    }
                    let target = [
                        target_halo.origin[0] + z,
                        target_halo.origin[1] + y,
                        target_halo.origin[2] + x,
                    ];
                    invalidate_core_neighborhood(&mut validity, target_core, target, radius)?;
                }
            }
        }
    }
    for z in 0..target_core.shape[0] {
        for y in 0..target_core.shape[1] {
            for x in 0..target_core.shape[2] {
                let target = [
                    target_core.origin[0] + z,
                    target_core.origin[1] + y,
                    target_core.origin[2] + x,
                ];
                let valid = validity[linear_index(target_core.shape, [z, y, x])?] == 1;
                if valid {
                    let (sum, count) = parent_block_sum_count(
                        Some(parent_pixels),
                        pixel_region,
                        parent_validity,
                        validity_region,
                        parent_full_shape,
                        target,
                    )?;
                    if count == 0 {
                        return Err(ImportError::InvalidCheckpoint(
                            "a dilated-valid coarse sample has no parent support".to_owned(),
                        ));
                    }
                    let rounded = (sum + count / 2) / count;
                    pixels.push(u8::try_from(rounded).map_err(|_| ImportError::Overflow)?);
                } else {
                    pixels.push(0);
                }
            }
        }
    }
    Ok(GuardedU8Region { pixels, validity })
}

fn invalidate_core_neighborhood(
    core_validity: &mut [u8],
    core: Region3,
    invalid_coordinate: [u64; 3],
    radius: [u64; 3],
) -> Result<(), ImportError> {
    if core_validity.len() != checked_voxels(core.shape)? {
        return Err(ImportError::InvalidRequest(
            "dilation core validity length does not match its shape",
        ));
    }
    let core_end = [
        core.origin[0]
            .checked_add(core.shape[0])
            .ok_or(ImportError::Overflow)?,
        core.origin[1]
            .checked_add(core.shape[1])
            .ok_or(ImportError::Overflow)?,
        core.origin[2]
            .checked_add(core.shape[2])
            .ok_or(ImportError::Overflow)?,
    ];
    let start = [
        invalid_coordinate[0]
            .saturating_sub(radius[0])
            .max(core.origin[0]),
        invalid_coordinate[1]
            .saturating_sub(radius[1])
            .max(core.origin[1]),
        invalid_coordinate[2]
            .saturating_sub(radius[2])
            .max(core.origin[2]),
    ];
    let end = [
        invalid_coordinate[0]
            .saturating_add(radius[0])
            .saturating_add(1)
            .min(core_end[0]),
        invalid_coordinate[1]
            .saturating_add(radius[1])
            .saturating_add(1)
            .min(core_end[1]),
        invalid_coordinate[2]
            .saturating_add(radius[2])
            .saturating_add(1)
            .min(core_end[2]),
    ];
    if (0..3).any(|axis| start[axis] >= end[axis]) {
        return Ok(());
    }
    for z in start[0]..end[0] {
        for y in start[1]..end[1] {
            for x in start[2]..end[2] {
                core_validity[linear_index(
                    core.shape,
                    [z - core.origin[0], y - core.origin[1], x - core.origin[2]],
                )?] = 0;
            }
        }
    }
    Ok(())
}

fn parent_block_sum_count(
    parent_pixels: Option<&[u8]>,
    pixel_region: Region3,
    parent_validity: &[u8],
    validity_region: Region3,
    parent_full_shape: [u64; 3],
    target: [u64; 3],
) -> Result<(u64, u64), ImportError> {
    let start = target.map(|coordinate| coordinate.saturating_mul(2));
    let end = [
        start[0].saturating_add(2).min(parent_full_shape[0]),
        start[1].saturating_add(2).min(parent_full_shape[1]),
        start[2].saturating_add(2).min(parent_full_shape[2]),
    ];
    let mut sum = 0_u64;
    let mut count = 0_u64;
    for z in start[0]..end[0] {
        for y in start[1]..end[1] {
            for x in start[2]..end[2] {
                let coordinate = [z, y, x];
                let validity_index = region_index(
                    validity_region,
                    coordinate,
                    "parent validity halo is incomplete",
                )?;
                if parent_validity[validity_index] == 0 {
                    continue;
                }
                count += 1;
                if let Some(pixels) = parent_pixels {
                    let pixel_index =
                        region_index(pixel_region, coordinate, "parent pixel core is incomplete")?;
                    sum = sum
                        .checked_add(u64::from(pixels[pixel_index]))
                        .ok_or(ImportError::Overflow)?;
                }
            }
        }
    }
    Ok((sum, count))
}

fn copy_u8_core(
    source: &[u8],
    source_region: Region3,
    core: Region3,
    destination: &mut Vec<u8>,
) -> Result<(), ImportError> {
    for z in 0..core.shape[0] {
        for y in 0..core.shape[1] {
            for x in 0..core.shape[2] {
                let index = region_index(
                    source_region,
                    [core.origin[0] + z, core.origin[1] + y, core.origin[2] + x],
                    "source region does not contain its core",
                )?;
                destination.push(source[index]);
            }
        }
    }
    Ok(())
}

fn validate_region(
    region: Region3,
    full_shape: [u64; 3],
    label: &'static str,
) -> Result<(), ImportError> {
    if full_shape.contains(&0) || region.shape.contains(&0) {
        return Err(ImportError::InvalidRequest(label));
    }
    for (axis, &full_len) in full_shape.iter().enumerate() {
        if region.origin[axis]
            .checked_add(region.shape[axis])
            .is_none_or(|end| end > full_len)
        {
            return Err(ImportError::InvalidRequest(label));
        }
    }
    Ok(())
}

fn require_contains(
    outer: Region3,
    inner: Region3,
    message: &'static str,
) -> Result<(), ImportError> {
    for axis in 0..3 {
        let outer_end = outer.origin[axis]
            .checked_add(outer.shape[axis])
            .ok_or(ImportError::Overflow)?;
        let inner_end = inner.origin[axis]
            .checked_add(inner.shape[axis])
            .ok_or(ImportError::Overflow)?;
        if inner.origin[axis] < outer.origin[axis] || inner_end > outer_end {
            return Err(ImportError::InvalidRequest(message));
        }
    }
    Ok(())
}

fn region_index(
    region: Region3,
    coordinate: [u64; 3],
    message: &'static str,
) -> Result<usize, ImportError> {
    let mut local = [0; 3];
    for axis in 0..3 {
        let end = region.origin[axis]
            .checked_add(region.shape[axis])
            .ok_or(ImportError::Overflow)?;
        if coordinate[axis] < region.origin[axis] || coordinate[axis] >= end {
            return Err(ImportError::InvalidRequest(message));
        }
        local[axis] = coordinate[axis] - region.origin[axis];
    }
    let index = local[0]
        .checked_mul(region.shape[1])
        .and_then(|value| value.checked_add(local[1]))
        .and_then(|value| value.checked_mul(region.shape[2]))
        .and_then(|value| value.checked_add(local[2]))
        .ok_or(ImportError::Overflow)?;
    usize::try_from(index).map_err(|_| ImportError::Overflow)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn direct_base(raw: &[u8], shape: [u64; 3], sentinel: u8) -> GuardedU8Region {
        let radius = invalid_dilation_radius(shape);
        let mut pixels = Vec::with_capacity(raw.len());
        let mut validity = Vec::with_capacity(raw.len());
        for z in 0..shape[0] {
            for y in 0..shape[1] {
                for x in 0..shape[2] {
                    let coordinate = [z, y, x];
                    let starts = [
                        z.saturating_sub(radius[0]),
                        y.saturating_sub(radius[1]),
                        x.saturating_sub(radius[2]),
                    ];
                    let ends = [
                        z.saturating_add(radius[0]).min(shape[0] - 1),
                        y.saturating_add(radius[1]).min(shape[1] - 1),
                        x.saturating_add(radius[2]).min(shape[2] - 1),
                    ];
                    let mut valid = true;
                    'neighbors: for neighbor_z in starts[0]..=ends[0] {
                        for neighbor_y in starts[1]..=ends[1] {
                            for neighbor_x in starts[2]..=ends[2] {
                                if raw[linear_index(shape, [neighbor_z, neighbor_y, neighbor_x])
                                    .unwrap()]
                                    == sentinel
                                {
                                    valid = false;
                                    break 'neighbors;
                                }
                            }
                        }
                    }
                    validity.push(u8::from(valid));
                    pixels.push(if valid {
                        raw[linear_index(shape, coordinate).unwrap()]
                    } else {
                        0
                    });
                }
            }
        }
        GuardedU8Region { pixels, validity }
    }

    #[test]
    fn corner_sentinel_dilates_in_three_dimensions() {
        let full = [3, 3, 3];
        let region = Region3 {
            origin: [0, 0, 0],
            shape: full,
        };
        let mut pixels = (0_u8..27).collect::<Vec<_>>();
        pixels[0] = 255;
        let guarded = guarded_u8_core(&pixels, region, full, region, 255).unwrap();
        assert_eq!(
            guarded.validity.iter().filter(|value| **value == 0).count(),
            8
        );
        assert_eq!(
            guarded.validity.iter().filter(|value| **value == 1).count(),
            19
        );
        assert_eq!(guarded.pixels[0], 0);
        assert_eq!(guarded.pixels[13], 0);
        assert_eq!(guarded.pixels[2], 2);
    }

    #[test]
    fn two_dimensional_dilation_has_zero_z_radius() {
        let full = [1, 3, 3];
        let region = Region3 {
            origin: [0, 0, 0],
            shape: full,
        };
        let mut pixels = vec![1; 9];
        pixels[0] = 255;
        let guarded = guarded_u8_core(&pixels, region, full, region, 255).unwrap();
        assert_eq!(guarded.validity, vec![0, 0, 1, 0, 0, 1, 1, 1, 1]);
    }

    #[test]
    fn scatter_dilation_matches_the_direct_oracle_exhaustively_with_clipped_cores() {
        for shape in [[1, 1, 1], [1, 2, 3], [2, 2, 2], [2, 2, 3]] {
            let voxels = checked_voxels(shape).unwrap();
            for invalid_bits in 0_u64..(1_u64 << voxels) {
                let raw = (0..voxels)
                    .map(|index| {
                        if invalid_bits & (1 << index) == 0 {
                            u8::try_from(index + 1).unwrap()
                        } else {
                            255
                        }
                    })
                    .collect::<Vec<_>>();
                let expected = direct_base(&raw, shape, 255);
                let full = Region3 {
                    origin: [0, 0, 0],
                    shape,
                };
                assert_eq!(
                    guarded_u8_core(&raw, full, shape, full, 255).unwrap(),
                    expected
                );

                // Exercise the same result through every single-voxel core,
                // forcing clipped halo handling at all faces and corners.
                for z in 0..shape[0] {
                    for y in 0..shape[1] {
                        for x in 0..shape[2] {
                            let core = Region3 {
                                origin: [z, y, x],
                                shape: [1, 1, 1],
                            };
                            let window =
                                clipped_halo(core, shape, invalid_dilation_radius(shape)).unwrap();
                            let mut window_pixels = Vec::new();
                            copy_u8_core(&raw, full, window, &mut window_pixels).unwrap();
                            let actual =
                                guarded_u8_core(&window_pixels, window, shape, core, 255).unwrap();
                            let index = linear_index(shape, [z, y, x]).unwrap();
                            assert_eq!(actual.pixels, [expected.pixels[index]]);
                            assert_eq!(actual.validity, [expected.validity[index]]);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn scatter_dilation_matches_the_direct_oracle_for_larger_randomized_masks() {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for shape in [[1, 5, 7], [4, 5, 6]] {
            let voxels = checked_voxels(shape).unwrap();
            let full = Region3 {
                origin: [0, 0, 0],
                shape,
            };
            for _ in 0..128 {
                let raw = (0..voxels)
                    .map(|index| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        if state.is_multiple_of(11) {
                            255
                        } else {
                            u8::try_from(index % 251).unwrap()
                        }
                    })
                    .collect::<Vec<_>>();
                let expected = direct_base(&raw, shape, 255);
                let actual = guarded_u8_core(&raw, full, shape, full, 255).unwrap();
                assert_eq!(actual, expected);

                let origin = [
                    state % shape[0],
                    state.rotate_left(11) % shape[1],
                    state.rotate_left(23) % shape[2],
                ];
                let core = Region3 {
                    origin,
                    shape: [
                        (shape[0] - origin[0]).min(2),
                        (shape[1] - origin[1]).min(3),
                        (shape[2] - origin[2]).min(4),
                    ],
                };
                let window = clipped_halo(core, shape, invalid_dilation_radius(shape)).unwrap();
                let mut window_pixels = Vec::new();
                copy_u8_core(&raw, full, window, &mut window_pixels).unwrap();
                let cropped = guarded_u8_core(&window_pixels, window, shape, core, 255).unwrap();
                let mut expected_pixels = Vec::new();
                let mut expected_validity = Vec::new();
                copy_u8_core(&expected.pixels, full, core, &mut expected_pixels).unwrap();
                copy_u8_core(&expected.validity, full, core, &mut expected_validity).unwrap();
                assert_eq!(cropped.pixels, expected_pixels);
                assert_eq!(cropped.validity, expected_validity);
            }
        }
    }

    #[test]
    fn masked_mean_is_redilated_and_never_reclassifies_the_sentinel() {
        let parent_full = [1, 2, 8];
        let target_full = [1, 1, 4];
        let parent = Region3 {
            origin: [0, 0, 0],
            shape: parent_full,
        };
        let target = Region3 {
            origin: [0, 0, 0],
            shape: target_full,
        };
        let pixels = [
            127, 129, 10, 10, 20, 20, 30, 30, 127, 129, 10, 10, 20, 20, 30, 30,
        ];
        let validity = [1, 1, 1, 1, 0, 0, 1, 1, 1, 1, 1, 1, 0, 0, 1, 1];
        let reduced = downsample_guarded_u8(
            &pixels,
            parent,
            &validity,
            parent,
            parent_full,
            target_full,
            target,
        )
        .unwrap();
        assert_eq!(reduced.validity, vec![1, 0, 0, 0]);
        assert_eq!(reduced.pixels, vec![128, 0, 0, 0]);
    }
}
