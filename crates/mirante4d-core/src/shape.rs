use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ShapeError {
    #[error("{axis} dimension must be positive, got 0")]
    NonPositive { axis: &'static str },
    #[error("element count overflows u64 for shape t={t}, z={z}, y={y}, x={x}")]
    ElementCountOverflow { t: u64, z: u64, y: u64, x: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Shape4D {
    pub t: u64,
    pub z: u64,
    pub y: u64,
    pub x: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Shape3D {
    pub z: u64,
    pub y: u64,
    pub x: u64,
}

impl Shape4D {
    pub fn new(t: u64, z: u64, y: u64, x: u64) -> Result<Self, ShapeError> {
        let shape = Self { t, z, y, x };
        shape.validate()?;
        Ok(shape)
    }

    pub fn validate(self) -> Result<(), ShapeError> {
        validate_axis("t", self.t)?;
        validate_axis("z", self.z)?;
        validate_axis("y", self.y)?;
        validate_axis("x", self.x)?;
        let _ = self.element_count()?;
        Ok(())
    }

    pub fn element_count(self) -> Result<u64, ShapeError> {
        self.t
            .checked_mul(self.z)
            .and_then(|v| v.checked_mul(self.y))
            .and_then(|v| v.checked_mul(self.x))
            .ok_or(ShapeError::ElementCountOverflow {
                t: self.t,
                z: self.z,
                y: self.y,
                x: self.x,
            })
    }

    pub fn spatial(self) -> Shape3D {
        Shape3D {
            z: self.z,
            y: self.y,
            x: self.x,
        }
    }

    pub fn as_zarr_shape(self) -> Vec<u64> {
        vec![self.t, self.z, self.y, self.x]
    }

    pub fn chunk_grid(self, chunk_shape: Self) -> Result<Self, ShapeError> {
        self.validate()?;
        chunk_shape.validate()?;
        Ok(Self {
            t: div_ceil(self.t, chunk_shape.t),
            z: div_ceil(self.z, chunk_shape.z),
            y: div_ceil(self.y, chunk_shape.y),
            x: div_ceil(self.x, chunk_shape.x),
        })
    }
}

impl Shape3D {
    pub fn new(z: u64, y: u64, x: u64) -> Result<Self, ShapeError> {
        validate_axis("z", z)?;
        validate_axis("y", y)?;
        validate_axis("x", x)?;
        Ok(Self { z, y, x })
    }

    pub fn element_count(self) -> Result<u64, ShapeError> {
        self.z
            .checked_mul(self.y)
            .and_then(|v| v.checked_mul(self.x))
            .ok_or(ShapeError::ElementCountOverflow {
                t: 1,
                z: self.z,
                y: self.y,
                x: self.x,
            })
    }
}

fn validate_axis(axis: &'static str, value: u64) -> Result<(), ShapeError> {
    if value == 0 {
        Err(ShapeError::NonPositive { axis })
    } else {
        Ok(())
    }
}

fn div_ceil(value: u64, divisor: u64) -> u64 {
    value.div_ceil(divisor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use proptest::test_runner::RngSeed;

    #[test]
    fn validates_positive_shape() {
        let shape = Shape4D::new(1, 16, 32, 64).unwrap();
        assert_eq!(shape.element_count().unwrap(), 32768);
    }

    #[test]
    fn rejects_zero_dimensions() {
        assert_eq!(
            Shape4D::new(0, 1, 1, 1).unwrap_err(),
            ShapeError::NonPositive { axis: "t" }
        );
    }

    #[test]
    fn computes_chunk_grid() {
        let shape = Shape4D::new(3, 17, 32, 33).unwrap();
        let chunk = Shape4D::new(1, 16, 16, 16).unwrap();
        assert_eq!(
            shape.chunk_grid(chunk).unwrap(),
            Shape4D::new(3, 2, 2, 3).unwrap()
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 64,
            max_shrink_iters: 1_024,
            failure_persistence: None,
            rng_seed: RngSeed::Fixed(0x4d34_5348_4150_4531),
            .. ProptestConfig::default()
        })]

        #[test]
        fn chunk_grid_covers_every_axis(
            t in 1_u64..512,
            z in 1_u64..512,
            y in 1_u64..512,
            x in 1_u64..512,
            chunk_t in 1_u64..128,
            chunk_z in 1_u64..128,
            chunk_y in 1_u64..128,
            chunk_x in 1_u64..128,
        ) {
            let shape = Shape4D::new(t, z, y, x).unwrap();
            let chunk_shape = Shape4D::new(chunk_t, chunk_z, chunk_y, chunk_x).unwrap();
            let grid = shape.chunk_grid(chunk_shape).unwrap();

            prop_assert!(grid.t > 0);
            prop_assert!(grid.z > 0);
            prop_assert!(grid.y > 0);
            prop_assert!(grid.x > 0);

            prop_assert!((grid.t - 1) * chunk_shape.t < shape.t);
            prop_assert!((grid.z - 1) * chunk_shape.z < shape.z);
            prop_assert!((grid.y - 1) * chunk_shape.y < shape.y);
            prop_assert!((grid.x - 1) * chunk_shape.x < shape.x);

            prop_assert!(grid.t * chunk_shape.t >= shape.t);
            prop_assert!(grid.z * chunk_shape.z >= shape.z);
            prop_assert!(grid.y * chunk_shape.y >= shape.y);
            prop_assert!(grid.x * chunk_shape.x >= shape.x);
        }
    }
}
