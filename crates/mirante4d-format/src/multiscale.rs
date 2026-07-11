use mirante4d_domain::{GridToWorld, Shape4D};

use crate::{CurrentGridToWorldExt, CurrentTransformError};

pub(crate) const GRID_TO_WORLD_EPSILON: f64 = 1.0e-9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DownsampleFactors {
    pub x: u64,
    pub y: u64,
    pub z: u64,
}

pub(crate) fn infer_downsample_factors(
    previous_shape: Shape4D,
    shape: Shape4D,
) -> Option<DownsampleFactors> {
    let x = infer_axis_factor(previous_shape.x(), shape.x())?;
    let y = infer_axis_factor(previous_shape.y(), shape.y())?;
    let z = infer_axis_factor(previous_shape.z(), shape.z())?;
    if x == 1 && y == 1 && z == 1 {
        return None;
    }
    Some(DownsampleFactors { x, y, z })
}

pub(crate) fn expected_downsampled_grid_to_world(
    previous_grid_to_world: GridToWorld,
    factors: DownsampleFactors,
) -> Result<GridToWorld, CurrentTransformError> {
    previous_grid_to_world.downsampled_integer_centered(factors.x, factors.y, factors.z)
}

pub(crate) fn grid_to_world_approx_eq(
    actual: GridToWorld,
    expected: GridToWorld,
    epsilon: f64,
) -> bool {
    actual
        .row_major()
        .iter()
        .zip(expected.row_major().iter())
        .all(|(actual, expected)| (actual - expected).abs() <= epsilon)
}

fn infer_axis_factor(previous: u64, current: u64) -> Option<u64> {
    if current == 0 || current > previous {
        return None;
    }
    let max_factor = previous.checked_next_power_of_two()?;
    let mut factor = 1u64;
    while factor <= max_factor {
        if previous.div_ceil(factor) == current {
            return Some(factor);
        }
        factor = factor.checked_mul(2)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_power_of_two_downsample_factors_from_shapes() {
        assert_eq!(
            infer_downsample_factors(
                Shape4D::new(1, 258, 258, 258).unwrap(),
                Shape4D::new(1, 33, 33, 33).unwrap()
            ),
            Some(DownsampleFactors { x: 8, y: 8, z: 8 })
        );
        assert_eq!(
            infer_downsample_factors(
                Shape4D::new(1, 1, 512, 512).unwrap(),
                Shape4D::new(1, 1, 256, 256).unwrap()
            ),
            Some(DownsampleFactors { x: 2, y: 2, z: 1 })
        );
    }

    #[test]
    fn rejects_non_power_of_two_or_non_reduced_shapes() {
        assert_eq!(
            infer_downsample_factors(
                Shape4D::new(1, 258, 258, 258).unwrap(),
                Shape4D::new(1, 32, 32, 32).unwrap()
            ),
            None
        );
        assert_eq!(
            infer_downsample_factors(
                Shape4D::new(1, 4, 4, 4).unwrap(),
                Shape4D::new(1, 4, 4, 4).unwrap()
            ),
            None
        );
    }
}
