use thiserror::Error;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ShapeError {
    #[error("{axis} dimension must be positive")]
    NonPositive { axis: &'static str },
    #[error("element count overflows u64")]
    ElementCountOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Shape3D {
    z: u64,
    y: u64,
    x: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Shape4D {
    t: u64,
    z: u64,
    y: u64,
    x: u64,
}

impl Shape3D {
    pub fn new(z: u64, y: u64, x: u64) -> Result<Self, ShapeError> {
        validate_dimension("z", z)?;
        validate_dimension("y", y)?;
        validate_dimension("x", x)?;
        let shape = Self { z, y, x };
        shape.element_count()?;
        Ok(shape)
    }

    pub const fn z(self) -> u64 {
        self.z
    }

    pub const fn y(self) -> u64 {
        self.y
    }

    pub const fn x(self) -> u64 {
        self.x
    }

    pub const fn dimensions(self) -> [u64; 3] {
        [self.z, self.y, self.x]
    }

    pub fn element_count(self) -> Result<u64, ShapeError> {
        self.z
            .checked_mul(self.y)
            .and_then(|value| value.checked_mul(self.x))
            .ok_or(ShapeError::ElementCountOverflow)
    }
}

impl Shape4D {
    pub fn new(t: u64, z: u64, y: u64, x: u64) -> Result<Self, ShapeError> {
        validate_dimension("t", t)?;
        validate_dimension("z", z)?;
        validate_dimension("y", y)?;
        validate_dimension("x", x)?;
        let shape = Self { t, z, y, x };
        shape.element_count()?;
        Ok(shape)
    }

    pub const fn t(self) -> u64 {
        self.t
    }

    pub const fn z(self) -> u64 {
        self.z
    }

    pub const fn y(self) -> u64 {
        self.y
    }

    pub const fn x(self) -> u64 {
        self.x
    }

    pub const fn dimensions(self) -> [u64; 4] {
        [self.t, self.z, self.y, self.x]
    }

    pub fn element_count(self) -> Result<u64, ShapeError> {
        self.t
            .checked_mul(self.z)
            .and_then(|value| value.checked_mul(self.y))
            .and_then(|value| value.checked_mul(self.x))
            .ok_or(ShapeError::ElementCountOverflow)
    }

    pub fn spatial(self) -> Shape3D {
        Shape3D {
            z: self.z,
            y: self.y,
            x: self.x,
        }
    }
}

fn validate_dimension(axis: &'static str, value: u64) -> Result<(), ShapeError> {
    if value == 0 {
        Err(ShapeError::NonPositive { axis })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use proptest::test_runner::RngSeed;

    use super::*;

    #[test]
    fn rejects_zero_dimensions() {
        assert_eq!(
            Shape4D::new(1, 0, 1, 1),
            Err(ShapeError::NonPositive { axis: "z" })
        );
    }

    #[test]
    fn rejects_overflowing_element_counts_at_construction() {
        assert_eq!(
            Shape3D::new(u64::MAX, 2, 1),
            Err(ShapeError::ElementCountOverflow)
        );
    }

    #[test]
    fn spatial_projection_preserves_zyx() {
        let shape = Shape4D::new(3, 5, 7, 11).unwrap();
        assert_eq!(shape.spatial().dimensions(), [5, 7, 11]);
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 64,
            max_shrink_iters: 1_024,
            failure_persistence: None,
            rng_seed: RngSeed::Fixed(0x4d34_444f_4d53_4850),
            ..ProptestConfig::default()
        })]

        #[test]
        fn valid_small_shapes_have_exact_element_counts(
            t in 1_u64..256,
            z in 1_u64..256,
            y in 1_u64..256,
            x in 1_u64..256,
        ) {
            let shape = Shape4D::new(t, z, y, x).unwrap();
            prop_assert_eq!(shape.element_count().unwrap(), t * z * y * x);
        }
    }
}
