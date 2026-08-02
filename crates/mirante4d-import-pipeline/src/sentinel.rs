//! Shared bounded geometry for no-data neighborhood processing.

use crate::ImportError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region3 {
    pub(crate) origin: [u64; 3],
    pub(crate) shape: [u64; 3],
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halo_clips_at_every_volume_face() {
        let halo = clipped_halo(
            Region3 {
                origin: [0, 1, 2],
                shape: [2, 3, 4],
            },
            [4, 5, 6],
            [1, 1, 1],
        )
        .unwrap();
        assert_eq!(halo.origin, [0, 0, 1]);
        assert_eq!(halo.shape, [3, 5, 5]);
    }

    #[test]
    fn two_dimensional_data_has_no_z_dilation() {
        assert_eq!(invalid_dilation_radius([1, 10, 10]), [0, 1, 1]);
        assert_eq!(invalid_dilation_radius([2, 10, 10]), [1, 1, 1]);
    }
}
