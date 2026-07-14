use glam::{DMat4, DVec3};
use mirante4d_domain::GridToWorld;
use thiserror::Error;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum TransformError {
    #[error("grid-to-world matrix must be invertible")]
    NonInvertible,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct WorldToGrid(DMat4);

pub(crate) trait GridToWorldExt {
    fn to_dmat4(self) -> DMat4;
    fn transform_point_vec(self, grid_xyz: DVec3) -> DVec3;
    fn transform_vector(self, grid_vector: DVec3) -> DVec3;
    fn inverse(self) -> Result<WorldToGrid, TransformError>;
}

impl GridToWorldExt for GridToWorld {
    fn to_dmat4(self) -> DMat4 {
        let row_major = self.row_major();
        let mut column_major = [0.0; 16];
        for row in 0..4 {
            for column in 0..4 {
                column_major[column * 4 + row] = row_major[row * 4 + column];
            }
        }
        DMat4::from_cols_array(&column_major)
    }

    fn transform_point_vec(self, grid_xyz: DVec3) -> DVec3 {
        self.to_dmat4().transform_point3(grid_xyz)
    }

    fn transform_vector(self, grid_vector: DVec3) -> DVec3 {
        self.to_dmat4().transform_vector3(grid_vector)
    }

    fn inverse(self) -> Result<WorldToGrid, TransformError> {
        let matrix = self.to_dmat4();
        let inverse = matrix.inverse();
        if inverse.is_finite() && (matrix * inverse).abs_diff_eq(DMat4::IDENTITY, 1.0e-9) {
            Ok(WorldToGrid(inverse))
        } else {
            Err(TransformError::NonInvertible)
        }
    }
}

impl WorldToGrid {
    pub(crate) fn transform_point(self, world_xyz: DVec3) -> DVec3 {
        self.0.transform_point3(world_xyz)
    }

    pub(crate) fn transform_vector(self, world_vector: DVec3) -> DVec3 {
        self.0.transform_vector3(world_vector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affine_math_preserves_row_major_domain_transform_semantics() {
        let transform = GridToWorld::from_row_major([
            2.0, 0.0, 0.0, 10.0, 0.0, 3.0, 0.0, 20.0, 0.0, 0.0, 4.0, 30.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .unwrap();
        assert_eq!(
            transform.transform_point_vec(DVec3::new(1.0, 2.0, 3.0)),
            DVec3::new(12.0, 26.0, 42.0)
        );
        assert_eq!(
            transform
                .inverse()
                .unwrap()
                .transform_point(DVec3::new(12.0, 26.0, 42.0)),
            DVec3::new(1.0, 2.0, 3.0)
        );
    }

    #[test]
    fn singular_domain_transform_is_rejected_at_render_boundary() {
        let singular = GridToWorld::scale(1.0, 0.0, 1.0).unwrap();
        assert_eq!(singular.inverse(), Err(TransformError::NonInvertible));
    }
}
