//! Storage-independent semantic tiling shared by every interactive consumer.

#[cfg(test)]
use mirante4d_dataset::{ResourceContractError, ResourceRegion};
#[cfg(test)]
use mirante4d_domain::Shape3D;

pub(crate) const SEMANTIC_TILE_SIDE: u64 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg(test)]
pub(crate) struct SemanticTileIndex {
    pub(crate) z: u64,
    pub(crate) y: u64,
    pub(crate) x: u64,
}

#[derive(Debug, Clone, Copy)]
#[cfg(test)]
pub(crate) struct SemanticTileGrid {
    volume_shape: Shape3D,
    grid_shape: Shape3D,
}

#[cfg(test)]
impl SemanticTileGrid {
    pub(crate) fn new(volume_shape: Shape3D) -> Self {
        let grid_shape = Shape3D::new(
            volume_shape.z().div_ceil(SEMANTIC_TILE_SIDE),
            volume_shape.y().div_ceil(SEMANTIC_TILE_SIDE),
            volume_shape.x().div_ceil(SEMANTIC_TILE_SIDE),
        )
        .expect("a non-empty volume has a non-empty semantic tile grid");
        Self {
            volume_shape,
            grid_shape,
        }
    }

    pub(crate) const fn grid_shape(self) -> Shape3D {
        self.grid_shape
    }

    pub(crate) fn region(
        self,
        index: SemanticTileIndex,
    ) -> Result<ResourceRegion, ResourceContractError> {
        if index.z >= self.grid_shape.z()
            || index.y >= self.grid_shape.y()
            || index.x >= self.grid_shape.x()
        {
            return Err(ResourceContractError::RegionOutOfBounds { level: 0 });
        }
        let origin = [
            index.z.saturating_mul(SEMANTIC_TILE_SIDE),
            index.y.saturating_mul(SEMANTIC_TILE_SIDE),
            index.x.saturating_mul(SEMANTIC_TILE_SIDE),
        ];
        let end = [
            origin[0]
                .saturating_add(SEMANTIC_TILE_SIDE)
                .min(self.volume_shape.z()),
            origin[1]
                .saturating_add(SEMANTIC_TILE_SIDE)
                .min(self.volume_shape.y()),
            origin[2]
                .saturating_add(SEMANTIC_TILE_SIDE)
                .min(self.volume_shape.x()),
        ];
        let shape = Shape3D::new(end[0] - origin[0], end[1] - origin[1], end[2] - origin[2])
            .expect("an in-grid semantic tile is non-empty");
        let region = ResourceRegion::new(origin, shape)?;
        Ok(region)
    }

    pub(crate) fn indices(self) -> impl Iterator<Item = SemanticTileIndex> {
        (0..self.grid_shape.z()).flat_map(move |z| {
            (0..self.grid_shape.y()).flat_map(move |y| {
                (0..self.grid_shape.x()).map(move |x| SemanticTileIndex { z, y, x })
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_grid_is_lazy_deterministic_and_clips_only_edge_tiles() {
        let grid = SemanticTileGrid::new(Shape3D::new(65, 130, 64).unwrap());
        assert_eq!(grid.grid_shape().dimensions(), [2, 3, 1]);
        let indices = grid.indices().collect::<Vec<_>>();
        assert_eq!(indices.len(), 6);
        assert_eq!(indices[0], SemanticTileIndex { z: 0, y: 0, x: 0 });
        assert_eq!(indices[5], SemanticTileIndex { z: 1, y: 2, x: 0 });
        let edge = grid.region(indices[5]).unwrap();
        assert_eq!(edge.origin(), [64, 128, 0]);
        assert_eq!(edge.shape().dimensions(), [1, 2, 64]);
    }

    #[test]
    fn grid_rejects_out_of_range_indices() {
        let grid = SemanticTileGrid::new(Shape3D::new(8, 8, 8).unwrap());
        assert!(grid.region(SemanticTileIndex { z: 1, y: 0, x: 0 }).is_err());
    }
}
