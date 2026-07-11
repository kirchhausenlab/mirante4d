use glam::{DMat4, DVec3};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SpaceError {
    #[error("grid_to_world matrix must be invertible")]
    NonInvertibleTransform,
    #[error("downsample factors must be positive")]
    InvalidDownsampleFactor,
    #[error("world unit {0:?} is not supported")]
    UnsupportedUnit(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorldUnit {
    Micrometer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldSpace {
    pub name: String,
    pub unit: WorldUnit,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GridToWorld {
    pub matrix4x4_row_major: [f64; 16],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldToGrid {
    matrix: DMat4,
}

impl GridToWorld {
    pub fn scale_um(x_um: f64, y_um: f64, z_um: f64) -> Self {
        let matrix = DMat4::from_cols_array(&[
            x_um, 0.0, 0.0, 0.0, 0.0, y_um, 0.0, 0.0, 0.0, 0.0, z_um, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]);
        Self::from_dmat4(matrix)
    }

    pub fn downsampled_integer_centered(
        self,
        factor_x: u64,
        factor_y: u64,
        factor_z: u64,
    ) -> Result<Self, SpaceError> {
        if factor_x == 0 || factor_y == 0 || factor_z == 0 {
            return Err(SpaceError::InvalidDownsampleFactor);
        }
        let factors = DVec3::new(factor_x as f64, factor_y as f64, factor_z as f64);
        let offset = (factors - DVec3::ONE) * 0.5;
        let coarse_to_source_grid = DMat4::from_translation(offset) * DMat4::from_scale(factors);
        Ok(Self::from_dmat4(self.to_dmat4() * coarse_to_source_grid))
    }

    pub fn identity() -> Self {
        Self::from_dmat4(DMat4::IDENTITY)
    }

    pub fn from_dmat4(matrix: DMat4) -> Self {
        let column_major = matrix.to_cols_array();
        let mut row_major = [0.0; 16];
        for row in 0..4 {
            for col in 0..4 {
                row_major[row * 4 + col] = column_major[col * 4 + row];
            }
        }
        Self {
            matrix4x4_row_major: row_major,
        }
    }

    pub fn to_dmat4(self) -> DMat4 {
        let mut column_major = [0.0; 16];
        for row in 0..4 {
            for col in 0..4 {
                column_major[col * 4 + row] = self.matrix4x4_row_major[row * 4 + col];
            }
        }
        DMat4::from_cols_array(&column_major)
    }

    pub fn transform_point(self, grid_xyz: DVec3) -> DVec3 {
        self.to_dmat4().transform_point3(grid_xyz)
    }

    pub fn transform_vector(self, grid_vector: DVec3) -> DVec3 {
        self.to_dmat4().transform_vector3(grid_vector)
    }

    pub fn inverse(self) -> Result<WorldToGrid, SpaceError> {
        let matrix = self.to_dmat4();
        let inverse = matrix.inverse();
        if inverse.is_finite() && (matrix * inverse).abs_diff_eq(DMat4::IDENTITY, 1e-9) {
            Ok(WorldToGrid { matrix: inverse })
        } else {
            Err(SpaceError::NonInvertibleTransform)
        }
    }
}

impl WorldToGrid {
    pub fn transform_point(self, world_xyz: DVec3) -> DVec3 {
        self.matrix.transform_point3(world_xyz)
    }

    pub fn transform_vector(self, world_vector: DVec3) -> DVec3 {
        self.matrix.transform_vector3(world_vector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;
    use proptest::prelude::*;
    use proptest::test_runner::RngSeed;

    #[test]
    fn anisotropic_scale_round_trips_points() {
        let grid_to_world = GridToWorld::scale_um(0.2, 0.2, 0.5);
        let world_to_grid = grid_to_world.inverse().unwrap();
        let grid = DVec3::new(3.0, 7.0, 11.0);
        let world = grid_to_world.transform_point(grid);
        let back = world_to_grid.transform_point(world);

        assert_abs_diff_eq!(back.x, grid.x, epsilon = 1e-9);
        assert_abs_diff_eq!(back.y, grid.y, epsilon = 1e-9);
        assert_abs_diff_eq!(back.z, grid.z, epsilon = 1e-9);
    }

    #[test]
    fn anisotropic_scale_round_trips_vectors() {
        let grid_to_world = GridToWorld::scale_um(0.2, 0.2, 0.5);
        let world_to_grid = grid_to_world.inverse().unwrap();
        let grid_vector = DVec3::new(3.0, 7.0, 11.0);
        let world_vector = grid_to_world.transform_vector(grid_vector);
        let back = world_to_grid.transform_vector(world_vector);

        assert_abs_diff_eq!(back.x, grid_vector.x, epsilon = 1e-9);
        assert_abs_diff_eq!(back.y, grid_vector.y, epsilon = 1e-9);
        assert_abs_diff_eq!(back.z, grid_vector.z, epsilon = 1e-9);
    }

    #[test]
    fn downsampled_integer_centered_maps_to_source_block_centers() {
        let base = GridToWorld::scale_um(0.2, 0.3, 0.5);
        let downsampled = base.downsampled_integer_centered(2, 4, 8).unwrap();

        let origin_world = downsampled.transform_point(DVec3::ZERO);
        let expected_origin_world = base.transform_point(DVec3::new(0.5, 1.5, 3.5));
        assert_abs_diff_eq!(origin_world.x, expected_origin_world.x, epsilon = 1e-12);
        assert_abs_diff_eq!(origin_world.y, expected_origin_world.y, epsilon = 1e-12);
        assert_abs_diff_eq!(origin_world.z, expected_origin_world.z, epsilon = 1e-12);

        let next_world = downsampled.transform_point(DVec3::new(1.0, 1.0, 1.0));
        let expected_next_world = base.transform_point(DVec3::new(2.5, 5.5, 11.5));
        assert_abs_diff_eq!(next_world.x, expected_next_world.x, epsilon = 1e-12);
        assert_abs_diff_eq!(next_world.y, expected_next_world.y, epsilon = 1e-12);
        assert_abs_diff_eq!(next_world.z, expected_next_world.z, epsilon = 1e-12);
    }

    #[test]
    fn stores_manifest_matrix_as_row_major() {
        let grid_to_world = GridToWorld::scale_um(0.2, 0.3, 0.5);
        assert_eq!(
            grid_to_world.matrix4x4_row_major,
            [
                0.2, 0.0, 0.0, 0.0, 0.0, 0.3, 0.0, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 1.0
            ]
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 64,
            max_shrink_iters: 1_024,
            failure_persistence: None,
            rng_seed: RngSeed::Fixed(0x4d34_5350_4143_4531),
            .. ProptestConfig::default()
        })]

        #[test]
        fn positive_scale_round_trips_arbitrary_points(
            x_um in 0.001_f64..1000.0,
            y_um in 0.001_f64..1000.0,
            z_um in 0.001_f64..1000.0,
            x in -1.0e6_f64..1.0e6,
            y in -1.0e6_f64..1.0e6,
            z in -1.0e6_f64..1.0e6,
        ) {
            let grid_to_world = GridToWorld::scale_um(x_um, y_um, z_um);
            let world_to_grid = grid_to_world.inverse().unwrap();
            let grid = DVec3::new(x, y, z);
            let world = grid_to_world.transform_point(grid);
            let back = world_to_grid.transform_point(world);

            prop_assert!((back.x - grid.x).abs() <= 1.0e-6);
            prop_assert!((back.y - grid.y).abs() <= 1.0e-6);
            prop_assert!((back.z - grid.z).abs() <= 1.0e-6);
        }
    }
}
