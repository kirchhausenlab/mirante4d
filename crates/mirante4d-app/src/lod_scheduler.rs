//! Small camera/LOD geometry helpers retained by the current UI shell.

use mirante4d_domain::GridToWorld;

pub(crate) fn representative_voxel_world_size(grid_to_world: GridToWorld) -> f64 {
    let matrix = grid_to_world.row_major();
    let x = (matrix[0].powi(2) + matrix[4].powi(2) + matrix[8].powi(2)).sqrt();
    let y = (matrix[1].powi(2) + matrix[5].powi(2) + matrix[9].powi(2)).sqrt();
    let z = (matrix[2].powi(2) + matrix[6].powi(2) + matrix[10].powi(2)).sqrt();
    x.max(y).max(z).max(f64::EPSILON)
}
