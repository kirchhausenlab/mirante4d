//! Typed no-data classification and validity-aware multiscale reduction.

use mirante4d_domain::IntensityDType;

use crate::{
    ImportError,
    chunk::checked_voxels,
    model::ResolvedNoDataPolicy,
    sentinel::{Region3, clipped_halo, invalid_dilation_radius},
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GuardedRegion {
    pub(crate) pixels_le: Vec<u8>,
    pub(crate) validity: Vec<u8>,
}

/// Applies the resolved value rule plus its one-voxel dilation, then overlays
/// strictly plane-local invalidity. Manual uint8 mode classifies exact source
/// values; automatic mode reuses its immutable first-volume spatial mask for
/// every channel and timepoint. Only the requested core is returned and every
/// invalid sample is canonical zero.
pub(crate) fn guarded_base_core(
    dtype: IntensityDType,
    window_pixels_le: &[u8],
    window: Region3,
    full_shape: [u64; 3],
    core: Region3,
    policy: &ResolvedNoDataPolicy,
) -> Result<GuardedRegion, ImportError> {
    validate_region(window, full_shape, "no-data window")?;
    validate_region(core, full_shape, "no-data core")?;
    require_contains(window, core, "no-data window does not contain its core")?;
    require_policy_dtype(dtype, policy)?;
    let sample_width = usize::from(dtype.bytes_per_sample());
    let expected = checked_voxels(window.shape)?
        .checked_mul(sample_width)
        .ok_or(ImportError::Overflow)?;
    if window_pixels_le.len() != expected {
        return Err(ImportError::InvalidRequest(
            "no-data window pixel length does not match its shape",
        ));
    }

    let core_voxels = checked_voxels(core.shape)?;
    let mut validity = vec![1; core_voxels];
    if let Some(mask) = policy.automatic_mask() {
        if mask.shape_zyx() != full_shape {
            return Err(ImportError::InvalidRequest(
                "automatic no-data mask shape differs from the source volume",
            ));
        }
        let radius = invalid_dilation_radius(full_shape);
        for z in 0..window.shape[0] {
            let global_z = window.origin[0] + z;
            if policy.scale_z_is_hidden(0, global_z) {
                continue;
            }
            for y in 0..window.shape[1] {
                for x in 0..window.shape[2] {
                    let global = [global_z, window.origin[1] + y, window.origin[2] + x];
                    if mask.contains(global[0], global[1], global[2]) {
                        invalidate_core_neighborhood(&mut validity, core, global, radius)?;
                    }
                }
            }
        }
    } else if let Some(value) = policy.manual_value() {
        let sentinel = value.canonical_le_bytes();
        let radius = invalid_dilation_radius(full_shape);
        for z in 0..window.shape[0] {
            let global_z = window.origin[0] + z;
            // A plane selected by the independent plane rule must never
            // acquire value-rule morphology, even if it contains the value.
            if policy.scale_z_is_hidden(0, global_z) {
                continue;
            }
            for y in 0..window.shape[1] {
                for x in 0..window.shape[2] {
                    let index = linear_index(window.shape, [z, y, x])?;
                    let start = index
                        .checked_mul(sample_width)
                        .ok_or(ImportError::Overflow)?;
                    if window_pixels_le[start..start + sample_width] == sentinel {
                        invalidate_core_neighborhood(
                            &mut validity,
                            core,
                            [global_z, window.origin[1] + y, window.origin[2] + x],
                            radius,
                        )?;
                    }
                }
            }
        }
    }

    let mut pixels_le = Vec::with_capacity(
        core_voxels
            .checked_mul(sample_width)
            .ok_or(ImportError::Overflow)?,
    );
    for z in 0..core.shape[0] {
        let hidden = policy.scale_z_is_hidden(0, core.origin[0] + z);
        for y in 0..core.shape[1] {
            for x in 0..core.shape[2] {
                let core_index = linear_index(core.shape, [z, y, x])?;
                if hidden {
                    validity[core_index] = 0;
                }
                let source_index = region_index(
                    window,
                    [core.origin[0] + z, core.origin[1] + y, core.origin[2] + x],
                    "no-data window does not contain its core sample",
                )?;
                let start = source_index
                    .checked_mul(sample_width)
                    .ok_or(ImportError::Overflow)?;
                if validity[core_index] == 0 {
                    pixels_le.extend(std::iter::repeat_n(0, sample_width));
                } else {
                    pixels_le.extend_from_slice(&window_pixels_le[start..start + sample_width]);
                }
            }
        }
    }
    Ok(GuardedRegion {
        pixels_le,
        validity,
    })
}

/// Produces one factor-two level from parent pixels and final parent validity.
/// Sentinel morphology and constant-plane geometry remain separate until the
/// final validity union; values average only final-valid parent samples.
#[allow(clippy::too_many_arguments)]
pub(crate) fn downsample_guarded(
    dtype: IntensityDType,
    parent_pixels_le: &[u8],
    pixel_region: Region3,
    parent_validity: &[u8],
    validity_region: Region3,
    parent_full_shape: [u64; 3],
    target_full_shape: [u64; 3],
    target_core: Region3,
    target_scale: u32,
    policy: &ResolvedNoDataPolicy,
) -> Result<GuardedRegion, ImportError> {
    validate_region(pixel_region, parent_full_shape, "parent pixel region")?;
    validate_region(validity_region, parent_full_shape, "parent validity region")?;
    validate_region(target_core, target_full_shape, "target core")?;
    require_policy_dtype(dtype, policy)?;
    if target_scale == 0 {
        return Err(ImportError::InvalidRequest(
            "guarded reduction requires a non-base target scale",
        ));
    }
    for axis in 0..3 {
        if target_full_shape[axis] != parent_full_shape[axis].div_ceil(2) {
            return Err(ImportError::InvalidRequest(
                "target shape is not the factor-two parent shape",
            ));
        }
    }
    let sample_width = usize::from(dtype.bytes_per_sample());
    if parent_pixels_le.len()
        != checked_voxels(pixel_region.shape)?
            .checked_mul(sample_width)
            .ok_or(ImportError::Overflow)?
        || parent_validity.len() != checked_voxels(validity_region.shape)?
        || parent_validity.iter().any(|value| !matches!(value, 0 | 1))
    {
        return Err(ImportError::InvalidRequest(
            "guarded reduction input lengths or validity bytes are malformed",
        ));
    }

    let core_voxels = checked_voxels(target_core.shape)?;
    let mut sentinel_validity = vec![1; core_voxels];
    if policy.value().is_some() {
        let target_halo = clipped_halo(
            target_core,
            target_full_shape,
            invalid_dilation_radius(target_full_shape),
        )?;
        let mut support = vec![1; checked_voxels(target_halo.shape)?];
        for z in 0..target_halo.shape[0] {
            for y in 0..target_halo.shape[1] {
                for x in 0..target_halo.shape[2] {
                    let target = [
                        target_halo.origin[0] + z,
                        target_halo.origin[1] + y,
                        target_halo.origin[2] + x,
                    ];
                    support[linear_index(target_halo.shape, [z, y, x])?] =
                        u8::from(parent_block_has_sentinel_support(
                            parent_validity,
                            validity_region,
                            parent_full_shape,
                            target,
                            target_scale - 1,
                            policy,
                        )?);
                }
            }
        }
        let radius = invalid_dilation_radius(target_full_shape);
        for z in 0..target_halo.shape[0] {
            for y in 0..target_halo.shape[1] {
                for x in 0..target_halo.shape[2] {
                    if support[linear_index(target_halo.shape, [z, y, x])?] == 0 {
                        invalidate_core_neighborhood(
                            &mut sentinel_validity,
                            target_core,
                            [
                                target_halo.origin[0] + z,
                                target_halo.origin[1] + y,
                                target_halo.origin[2] + x,
                            ],
                            radius,
                        )?;
                    }
                }
            }
        }
    }

    let mut pixels_le = Vec::with_capacity(
        core_voxels
            .checked_mul(sample_width)
            .ok_or(ImportError::Overflow)?,
    );
    let mut validity = Vec::with_capacity(core_voxels);
    for z in 0..target_core.shape[0] {
        for y in 0..target_core.shape[1] {
            for x in 0..target_core.shape[2] {
                let local = [z, y, x];
                let target = [
                    target_core.origin[0] + z,
                    target_core.origin[1] + y,
                    target_core.origin[2] + x,
                ];
                let mean = parent_block_mean(
                    dtype,
                    parent_pixels_le,
                    pixel_region,
                    parent_validity,
                    validity_region,
                    parent_full_shape,
                    target,
                )?;
                let valid = sentinel_validity[linear_index(target_core.shape, local)?] == 1
                    && !policy.scale_z_is_hidden(target_scale, target[0])
                    && mean.is_some();
                validity.push(u8::from(valid));
                if valid {
                    pixels_le.extend_from_slice(&mean.expect("valid mean was checked"));
                } else {
                    pixels_le.extend(std::iter::repeat_n(0, sample_width));
                }
            }
        }
    }
    Ok(GuardedRegion {
        pixels_le,
        validity,
    })
}

fn require_policy_dtype(
    dtype: IntensityDType,
    policy: &ResolvedNoDataPolicy,
) -> Result<(), ImportError> {
    if policy.value().is_some_and(|value| value.dtype() != dtype) {
        return Err(ImportError::InvalidRequest(
            "resolved no-data value has a different dtype than the source",
        ));
    }
    Ok(())
}

fn parent_block_has_sentinel_support(
    parent_validity: &[u8],
    validity_region: Region3,
    parent_full_shape: [u64; 3],
    target: [u64; 3],
    parent_scale: u32,
    policy: &ResolvedNoDataPolicy,
) -> Result<bool, ImportError> {
    let (start, end) = parent_block_bounds(parent_full_shape, target);
    for z in start[0]..end[0] {
        for y in start[1]..end[1] {
            for x in start[2]..end[2] {
                if policy.scale_z_is_hidden(parent_scale, z)
                    || parent_validity[region_index(
                        validity_region,
                        [z, y, x],
                        "parent validity halo is incomplete",
                    )?] == 1
                {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

#[allow(clippy::too_many_arguments)]
fn parent_block_mean(
    dtype: IntensityDType,
    parent_pixels_le: &[u8],
    pixel_region: Region3,
    parent_validity: &[u8],
    validity_region: Region3,
    parent_full_shape: [u64; 3],
    target: [u64; 3],
) -> Result<Option<Vec<u8>>, ImportError> {
    let (start, end) = parent_block_bounds(parent_full_shape, target);
    let mut integer_sum = 0_u64;
    let mut float_sum = 0_f64;
    let mut count = 0_u64;
    let width = usize::from(dtype.bytes_per_sample());
    for z in start[0]..end[0] {
        for y in start[1]..end[1] {
            for x in start[2]..end[2] {
                if parent_validity[region_index(
                    validity_region,
                    [z, y, x],
                    "parent validity halo is incomplete",
                )?] == 0
                {
                    continue;
                }
                let index =
                    region_index(pixel_region, [z, y, x], "parent pixel core is incomplete")?;
                let byte = index.checked_mul(width).ok_or(ImportError::Overflow)?;
                match dtype {
                    IntensityDType::Uint8 => {
                        integer_sum = integer_sum
                            .checked_add(u64::from(parent_pixels_le[byte]))
                            .ok_or(ImportError::Overflow)?;
                    }
                    IntensityDType::Uint16 => {
                        integer_sum = integer_sum
                            .checked_add(u64::from(u16::from_le_bytes([
                                parent_pixels_le[byte],
                                parent_pixels_le[byte + 1],
                            ])))
                            .ok_or(ImportError::Overflow)?;
                    }
                    IntensityDType::Float32 => {
                        let value = f32::from_le_bytes([
                            parent_pixels_le[byte],
                            parent_pixels_le[byte + 1],
                            parent_pixels_le[byte + 2],
                            parent_pixels_le[byte + 3],
                        ]);
                        if !value.is_finite() {
                            return Err(ImportError::InvalidCheckpoint(
                                "guarded float pyramid contains a non-finite parent".to_owned(),
                            ));
                        }
                        float_sum += f64::from(value);
                    }
                }
                count = count.checked_add(1).ok_or(ImportError::Overflow)?;
            }
        }
    }
    if count == 0 {
        return Ok(None);
    }
    let bytes = match dtype {
        IntensityDType::Uint8 => vec![
            u8::try_from((integer_sum + count / 2) / count).map_err(|_| ImportError::Overflow)?,
        ],
        IntensityDType::Uint16 => u16::try_from((integer_sum + count / 2) / count)
            .map_err(|_| ImportError::Overflow)?
            .to_le_bytes()
            .to_vec(),
        IntensityDType::Float32 => {
            let value = (float_sum / count as f64) as f32;
            if !value.is_finite() {
                return Err(ImportError::InvalidCheckpoint(
                    "guarded float pyramid produced a non-finite mean".to_owned(),
                ));
            }
            value.to_bits().to_le_bytes().to_vec()
        }
    };
    Ok(Some(bytes))
}

fn parent_block_bounds(parent_full_shape: [u64; 3], target: [u64; 3]) -> ([u64; 3], [u64; 3]) {
    let start = target.map(|coordinate| coordinate.saturating_mul(2));
    let end = [
        start[0].saturating_add(2).min(parent_full_shape[0]),
        start[1].saturating_add(2).min(parent_full_shape[1]),
        start[2].saturating_add(2).min(parent_full_shape[2]),
    ];
    (start, end)
}

fn invalidate_core_neighborhood(
    core_validity: &mut [u8],
    core: Region3,
    invalid: [u64; 3],
    radius: [u64; 3],
) -> Result<(), ImportError> {
    let core_end = [
        core.origin[0] + core.shape[0],
        core.origin[1] + core.shape[1],
        core.origin[2] + core.shape[2],
    ];
    let start = [
        invalid[0].saturating_sub(radius[0]).max(core.origin[0]),
        invalid[1].saturating_sub(radius[1]).max(core.origin[1]),
        invalid[2].saturating_sub(radius[2]).max(core.origin[2]),
    ];
    let end = [
        invalid[0].saturating_add(radius[0] + 1).min(core_end[0]),
        invalid[1].saturating_add(radius[1] + 1).min(core_end[1]),
        invalid[2].saturating_add(radius[2] + 1).min(core_end[2]),
    ];
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

fn validate_region(
    region: Region3,
    full_shape: [u64; 3],
    message: &'static str,
) -> Result<(), ImportError> {
    if region.shape.contains(&0)
        || full_shape.contains(&0)
        || (0..3).any(|axis| {
            region.origin[axis]
                .checked_add(region.shape[axis])
                .is_none_or(|end| end > full_shape[axis])
        })
    {
        return Err(ImportError::InvalidRequest(message));
    }
    Ok(())
}

fn require_contains(
    outer: Region3,
    inner: Region3,
    message: &'static str,
) -> Result<(), ImportError> {
    if (0..3).any(|axis| {
        inner.origin[axis] < outer.origin[axis]
            || inner.origin[axis] + inner.shape[axis] > outer.origin[axis] + outer.shape[axis]
    }) {
        return Err(ImportError::InvalidRequest(message));
    }
    Ok(())
}

fn region_index(
    region: Region3,
    coordinate: [u64; 3],
    message: &'static str,
) -> Result<usize, ImportError> {
    if (0..3).any(|axis| {
        coordinate[axis] < region.origin[axis]
            || coordinate[axis] >= region.origin[axis] + region.shape[axis]
    }) {
        return Err(ImportError::InvalidRequest(message));
    }
    linear_index(
        region.shape,
        [
            coordinate[0] - region.origin[0],
            coordinate[1] - region.origin[1],
            coordinate[2] - region.origin[2],
        ],
    )
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
    use crate::{
        NoDataPolicy, NoDataValueRule,
        model::{ResolvedAutomaticNoDataMask, ResolvedNoDataValue},
        source::automatic_mask_packed_bytes,
    };

    fn policy(
        value: Option<ResolvedNoDataValue>,
        planes: Vec<u64>,
        shape: [u64; 3],
    ) -> ResolvedNoDataPolicy {
        let automatic_mask = value.map(|_| {
            let mut bits = vec![0; automatic_mask_packed_bytes(shape).unwrap() as usize];
            bits[0] = 1;
            ResolvedAutomaticNoDataMask::new(shape, bits).unwrap()
        });
        ResolvedNoDataPolicy::new(
            Some(NoDataPolicy::new(
                Some(NoDataValueRule::Automatic),
                !planes.is_empty(),
            )),
            value,
            automatic_mask,
            planes,
            shape[0],
        )
        .unwrap()
    }

    #[test]
    fn constant_plane_is_strict_while_value_is_dilated() {
        let shape = [3, 3, 3];
        let region = Region3 {
            origin: [0; 3],
            shape,
        };
        let mut pixels = vec![7_u8; 27];
        pixels[linear_index(shape, [0, 0, 0]).unwrap()] = 255;
        let guarded = guarded_base_core(
            IntensityDType::Uint8,
            &pixels,
            region,
            shape,
            region,
            &policy(Some(ResolvedNoDataValue::Uint8(255)), vec![2], shape),
        )
        .unwrap();
        assert_eq!(guarded.validity[linear_index(shape, [1, 2, 2]).unwrap()], 1);
        assert!(guarded.validity[..9].iter().filter(|v| **v == 0).count() >= 4);
        assert!(guarded.validity[18..].iter().all(|v| *v == 0));
    }

    #[test]
    fn partial_hidden_z_support_disappears_without_fill_contamination() {
        let parent_shape = [2, 1, 1];
        let target_shape = [1, 1, 1];
        let reduced = downsample_guarded(
            IntensityDType::Uint16,
            &[100, 0, 200, 0],
            Region3 {
                origin: [0; 3],
                shape: parent_shape,
            },
            &[0, 1],
            Region3 {
                origin: [0; 3],
                shape: parent_shape,
            },
            parent_shape,
            target_shape,
            Region3 {
                origin: [0; 3],
                shape: target_shape,
            },
            1,
            &policy(None, vec![0], parent_shape),
        )
        .unwrap();
        assert_eq!(reduced.validity, vec![1]);
        assert_eq!(
            u16::from_le_bytes([reduced.pixels_le[0], reduced.pixels_le[1]]),
            200
        );
    }

    #[test]
    fn fully_hidden_coarse_interval_is_invalid_without_neighbor_dilation() {
        let parent_shape = [4, 1, 1];
        let target_shape = [2, 1, 1];
        let reduced = downsample_guarded(
            IntensityDType::Uint8,
            &[0, 0, 30, 40],
            Region3 {
                origin: [0; 3],
                shape: parent_shape,
            },
            &[0, 0, 1, 1],
            Region3 {
                origin: [0; 3],
                shape: parent_shape,
            },
            parent_shape,
            target_shape,
            Region3 {
                origin: [0; 3],
                shape: target_shape,
            },
            1,
            &policy(None, vec![0, 1], parent_shape),
        )
        .unwrap();
        assert_eq!(reduced.validity, vec![0, 1]);
        assert_eq!(reduced.pixels_le, vec![0, 35]);
    }

    #[test]
    fn float_reduction_uses_only_valid_finite_parents() {
        let parent_shape = [2, 1, 2];
        let target_shape = [1, 1, 1];
        let pixels = [2.0_f32, 1000.0, 4.0, 6.0]
            .into_iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>();
        let reduced = downsample_guarded(
            IntensityDType::Float32,
            &pixels,
            Region3 {
                origin: [0; 3],
                shape: parent_shape,
            },
            &[1, 0, 1, 1],
            Region3 {
                origin: [0; 3],
                shape: parent_shape,
            },
            parent_shape,
            target_shape,
            Region3 {
                origin: [0; 3],
                shape: target_shape,
            },
            1,
            &policy(
                Some(ResolvedNoDataValue::Float32Bits(1000.0_f32.to_bits())),
                Vec::new(),
                parent_shape,
            ),
        )
        .unwrap();
        assert_eq!(reduced.validity, vec![1]);
        assert_eq!(
            f32::from_le_bytes(reduced.pixels_le.try_into().unwrap()),
            4.0
        );
    }
}
