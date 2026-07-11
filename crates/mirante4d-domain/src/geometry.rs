use thiserror::Error;

#[derive(Debug, Error, Clone, Copy, PartialEq)]
pub enum GeometryError {
    #[error("world point components must be finite")]
    NonFiniteWorldPoint,
    #[error("quaternion components must be finite")]
    NonFiniteQuaternion,
    #[error("quaternion must have nonzero length")]
    ZeroQuaternion,
    #[error("grid-to-world matrix components must be finite")]
    NonFiniteTransform,
    #[error("grid-to-world matrix must be affine with final row [0, 0, 0, 1]")]
    NonAffineTransform,
    #[error("transformed world point is not finite")]
    TransformedPointNotFinite,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldPoint3([f64; 3]);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnitQuaternion([f64; 4]);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridToWorld([f64; 16]);

impl WorldPoint3 {
    pub fn new(x: f64, y: f64, z: f64) -> Result<Self, GeometryError> {
        let components = [x, y, z];
        if components.iter().all(|value| value.is_finite()) {
            Ok(Self(components.map(canonical_zero)))
        } else {
            Err(GeometryError::NonFiniteWorldPoint)
        }
    }

    pub const fn origin() -> Self {
        Self([0.0; 3])
    }

    pub const fn components(self) -> [f64; 3] {
        self.0
    }

    pub const fn x(self) -> f64 {
        self.0[0]
    }

    pub const fn y(self) -> f64 {
        self.0[1]
    }

    pub const fn z(self) -> f64 {
        self.0[2]
    }
}

impl UnitQuaternion {
    pub fn new_xyzw(x: f64, y: f64, z: f64, w: f64) -> Result<Self, GeometryError> {
        let components = [x, y, z, w];
        if !components.iter().all(|value| value.is_finite()) {
            return Err(GeometryError::NonFiniteQuaternion);
        }
        let scale = components
            .iter()
            .map(|value| value.abs())
            .fold(0.0, f64::max);
        if scale == 0.0 {
            return Err(GeometryError::ZeroQuaternion);
        }
        let scaled = components.map(|value| value / scale);
        let norm_squared = scaled.iter().map(|value| value * value).sum::<f64>();
        let inverse_norm = norm_squared.sqrt().recip();
        let mut normalized = scaled.map(|value| canonical_zero(value * inverse_norm));
        if quaternion_sign_is_negative(normalized) {
            normalized = normalized.map(|value| canonical_zero(-value));
        }
        Ok(Self(normalized))
    }

    pub const fn identity() -> Self {
        Self([0.0, 0.0, 0.0, 1.0])
    }

    pub const fn xyzw(self) -> [f64; 4] {
        self.0
    }
}

impl GridToWorld {
    pub fn from_row_major(matrix: [f64; 16]) -> Result<Self, GeometryError> {
        if !matrix.iter().all(|value| value.is_finite()) {
            return Err(GeometryError::NonFiniteTransform);
        }
        if matrix[12] != 0.0 || matrix[13] != 0.0 || matrix[14] != 0.0 || matrix[15] != 1.0 {
            return Err(GeometryError::NonAffineTransform);
        }
        Ok(Self(matrix.map(canonical_zero)))
    }

    pub const fn identity() -> Self {
        Self([
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ])
    }

    pub fn scale(x_world: f64, y_world: f64, z_world: f64) -> Result<Self, GeometryError> {
        Self::from_row_major([
            x_world, 0.0, 0.0, 0.0, 0.0, y_world, 0.0, 0.0, 0.0, 0.0, z_world, 0.0, 0.0, 0.0, 0.0,
            1.0,
        ])
    }

    pub const fn row_major(self) -> [f64; 16] {
        self.0
    }

    pub fn transform_point(self, point: WorldPoint3) -> Result<WorldPoint3, GeometryError> {
        let [x, y, z] = point.components();
        WorldPoint3::new(
            self.0[0] * x + self.0[1] * y + self.0[2] * z + self.0[3],
            self.0[4] * x + self.0[5] * y + self.0[6] * z + self.0[7],
            self.0[8] * x + self.0[9] * y + self.0[10] * z + self.0[11],
        )
        .map_err(|_| GeometryError::TransformedPointNotFinite)
    }
}

fn quaternion_sign_is_negative(value: [f64; 4]) -> bool {
    value[3] < 0.0
        || (value[3] == 0.0
            && (value[0] < 0.0
                || (value[0] == 0.0 && (value[1] < 0.0 || (value[1] == 0.0 && value[2] < 0.0)))))
}

fn canonical_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use proptest::test_runner::RngSeed;

    use super::*;

    #[test]
    fn rejects_non_finite_world_points() {
        assert_eq!(
            WorldPoint3::new(f64::NAN, 0.0, 0.0),
            Err(GeometryError::NonFiniteWorldPoint)
        );
    }

    #[test]
    fn normalizes_and_sign_canonicalizes_quaternions() {
        let positive = UnitQuaternion::new_xyzw(0.0, 0.0, 0.0, 2.0).unwrap();
        let negative = UnitQuaternion::new_xyzw(0.0, 0.0, 0.0, -2.0).unwrap();
        assert_eq!(positive, UnitQuaternion::identity());
        assert_eq!(negative, UnitQuaternion::identity());
        assert_eq!(
            UnitQuaternion::new_xyzw(0.0, 0.0, 0.0, f64::MAX).unwrap(),
            UnitQuaternion::identity()
        );
        assert_eq!(
            UnitQuaternion::new_xyzw(0.0, 0.0, 0.0, f64::MIN_POSITIVE).unwrap(),
            UnitQuaternion::identity()
        );
    }

    #[test]
    fn preserves_finite_affine_transforms_without_invertibility_policy() {
        let mut non_affine = GridToWorld::identity().row_major();
        non_affine[15] = 2.0;
        assert_eq!(
            GridToWorld::from_row_major(non_affine),
            Err(GeometryError::NonAffineTransform)
        );
        assert!(GridToWorld::scale(1.0, 0.0, 1.0).is_ok());
        assert!(GridToWorld::scale(1.0, 1.0, 1.0e-20).is_ok());
        assert!(
            GridToWorld::scale(f64::MIN_POSITIVE, f64::MIN_POSITIVE, f64::MIN_POSITIVE).is_ok()
        );
        assert!(GridToWorld::scale(1.0, f64::MIN_POSITIVE, f64::MIN_POSITIVE).is_ok());
    }

    #[test]
    fn transforms_voxel_centers_with_row_major_affine_values() {
        let transform = GridToWorld::from_row_major([
            2.0, 0.0, 0.0, 10.0, 0.0, 3.0, 0.0, 20.0, 0.0, 0.0, 4.0, 30.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .unwrap();
        let point = transform
            .transform_point(WorldPoint3::new(1.0, 2.0, 3.0).unwrap())
            .unwrap();
        assert_eq!(point.components(), [12.0, 26.0, 42.0]);
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 64,
            max_shrink_iters: 1_024,
            failure_persistence: None,
            rng_seed: RngSeed::Fixed(0x4d34_444f_4d47_454f),
            ..ProptestConfig::default()
        })]

        #[test]
        fn finite_nonzero_quaternions_are_normalized(
            x in -1.0e6_f64..1.0e6,
            y in -1.0e6_f64..1.0e6,
            z in -1.0e6_f64..1.0e6,
            w in -1.0e6_f64..1.0e6,
        ) {
            prop_assume!(x != 0.0 || y != 0.0 || z != 0.0 || w != 0.0);
            let quaternion = UnitQuaternion::new_xyzw(x, y, z, w).unwrap();
            let squared_norm = quaternion.xyzw().iter().map(|value| value * value).sum::<f64>();
            prop_assert!((squared_norm - 1.0).abs() <= 1.0e-12);
        }
    }
}
