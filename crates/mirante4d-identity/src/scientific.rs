use std::mem;

use mirante4d_domain::{GridToWorld, IntensityDType, LogicalLayerKey, Shape4D};
use thiserror::Error;

use crate::{
    ScientificContentId, Sha256Digest, Sha256Hasher,
    canonical::{f64_hex, update_u8, update_u32, update_u64},
};

pub const SCIENTIFIC_TILE_SHAPE_TZYX: [u64; 4] = [1, 16, 256, 256];

const TILE_DOMAIN: &[u8] = b"M4D-SC-V1-TILE\0";
const NODE_DOMAIN: &[u8] = b"M4D-SC-V1-NODE\0";
const LAYER_DOMAIN: &[u8] = b"M4D-SC-V1-LAYER\0";
const DATASET_DOMAIN: &[u8] = b"M4D-SC-V1-DATASET\0";
const MERKLE_ARITY: usize = 1024;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum ScientificHashError {
    #[error("regular temporal calibration requires a positive finite step")]
    InvalidTemporalStep,
    #[error("explicit temporal calibration has {actual} positions, expected {expected}")]
    TemporalPositionCount { expected: u64, actual: usize },
    #[error("explicit temporal position count does not fit u64")]
    TemporalPositionCountOverflow,
    #[error("explicit temporal position {index} {reason}")]
    InvalidTemporalPosition { index: usize, reason: &'static str },
    #[error("identity tile count overflows u64")]
    TileCountOverflow,
    #[error("identity tile coordinate arithmetic overflowed")]
    TileCoordinateOverflow,
    #[error("expected tile origin {expected:?}, got {actual:?}")]
    UnexpectedTileOrigin {
        expected: [u64; 4],
        actual: [u64; 4],
    },
    #[error("expected tile extent {expected:?}, got {actual:?}")]
    UnexpectedTileExtent {
        expected: [u64; 4],
        actual: [u64; 4],
    },
    #[error("layer already received all {expected} declared identity tiles")]
    TooManyTiles { expected: u64 },
    #[error("tile voxel count overflows u64")]
    TileVoxelCountOverflow,
    #[error("tile byte length does not fit this platform")]
    TileByteLengthOverflow,
    #[error("expected {expected} validity bytes, got {actual}")]
    InvalidValidityByteLength { expected: usize, actual: usize },
    #[error("unused high bits in the final validity byte must be zero")]
    NonZeroValidityPadding,
    #[error("expected {expected} canonical value bytes, got {actual}")]
    InvalidValueByteLength { expected: usize, actual: usize },
    #[error("invalid integer sample {index} must use an all-zero canonical sentinel")]
    InvalidIntegerSentinel { index: usize },
    #[error("invalid float sample {index} must use positive-zero canonical bits")]
    InvalidFloatSentinel { index: usize },
    #[error("valid float sample {index} is not finite")]
    NonFiniteFloatSample { index: usize },
    #[error("scientific hasher was poisoned by an earlier rejected input")]
    PreviouslyFailed,
    #[error("layer ended after {actual} tiles, expected {expected}")]
    IncompleteLayer { expected: u64, actual: u64 },
    #[error("scientific Merkle tree has no leaves")]
    EmptyTree,
    #[error("scientific Merkle tree level exceeds u32")]
    TreeLevelOverflow,
    #[error("scientific Merkle node child count is invalid")]
    InvalidTreeChildCount,
    #[error("dataset layer count must be positive")]
    EmptyDataset,
    #[error("dataset already received all {expected} declared layers")]
    TooManyLayers { expected: u32 },
    #[error("expected logical layer ordinal {expected}, got {actual}")]
    UnexpectedLayerOrdinal { expected: u32, actual: u32 },
    #[error("dataset ended after {actual} layers, expected {expected}")]
    IncompleteDataset { expected: u32, actual: u32 },
    #[error("canonical transform unexpectedly contains a non-finite value")]
    NonFiniteTransform,
}

/// Canonical relative-second calibration for one scientific logical layer.
#[derive(Clone, Debug, PartialEq)]
pub enum ScientificTemporalCalibration {
    Unknown,
    Regular { step_seconds: f64 },
    Explicit { positions_seconds: Vec<f64> },
}

impl ScientificTemporalCalibration {
    fn validate(&self, timepoints: u64) -> Result<(), ScientificHashError> {
        match self {
            Self::Unknown => Ok(()),
            Self::Regular { step_seconds } if step_seconds.is_finite() && *step_seconds > 0.0 => {
                Ok(())
            }
            Self::Regular { .. } => Err(ScientificHashError::InvalidTemporalStep),
            Self::Explicit { positions_seconds } => {
                if u64::try_from(positions_seconds.len()).ok() != Some(timepoints) {
                    return Err(ScientificHashError::TemporalPositionCount {
                        expected: timepoints,
                        actual: positions_seconds.len(),
                    });
                }
                if positions_seconds
                    .first()
                    .is_none_or(|position| position.to_bits() != 0)
                {
                    return Err(ScientificHashError::InvalidTemporalPosition {
                        index: 0,
                        reason: "must be positive zero",
                    });
                }
                for (index, position) in positions_seconds.iter().enumerate() {
                    if !position.is_finite() {
                        return Err(ScientificHashError::InvalidTemporalPosition {
                            index,
                            reason: "must be finite",
                        });
                    }
                    if index > 0 && *position <= positions_seconds[index - 1] {
                        return Err(ScientificHashError::InvalidTemporalPosition {
                            index,
                            reason: "must be strictly increasing",
                        });
                    }
                }
                Ok(())
            }
        }
    }

    fn update_hasher(&self, hasher: &mut Sha256Hasher) -> Result<(), ScientificHashError> {
        match self {
            Self::Unknown => update_u8(hasher, 0),
            Self::Regular { step_seconds } => {
                update_u8(hasher, 1);
                hasher.update(
                    f64_hex(*step_seconds).ok_or(ScientificHashError::InvalidTemporalStep)?,
                );
            }
            Self::Explicit { positions_seconds } => {
                update_u8(hasher, 2);
                update_u64(
                    hasher,
                    positions_seconds
                        .len()
                        .try_into()
                        .map_err(|_| ScientificHashError::TemporalPositionCountOverflow)?,
                );
                for (index, position) in positions_seconds.iter().enumerate() {
                    hasher.update(f64_hex(*position).ok_or(
                        ScientificHashError::InvalidTemporalPosition {
                            index,
                            reason: "must be finite",
                        },
                    )?);
                }
            }
        }
        Ok(())
    }
}

/// The normalized scientific facts bound by one layer root.
#[derive(Clone, Debug, PartialEq)]
pub struct ScientificLayerDescriptor {
    layer: LogicalLayerKey,
    dtype: IntensityDType,
    shape: Shape4D,
    temporal: ScientificTemporalCalibration,
    grid_to_world: GridToWorld,
}

impl ScientificLayerDescriptor {
    pub fn new(
        layer: LogicalLayerKey,
        dtype: IntensityDType,
        shape: Shape4D,
        temporal: ScientificTemporalCalibration,
        grid_to_world: GridToWorld,
    ) -> Result<Self, ScientificHashError> {
        temporal.validate(shape.t())?;
        Ok(Self {
            layer,
            dtype,
            shape,
            temporal,
            grid_to_world,
        })
    }

    pub const fn layer(&self) -> LogicalLayerKey {
        self.layer
    }

    pub const fn dtype(&self) -> IntensityDType {
        self.dtype
    }

    pub const fn shape(&self) -> Shape4D {
        self.shape
    }

    pub fn temporal(&self) -> &ScientificTemporalCalibration {
        &self.temporal
    }

    pub const fn grid_to_world(&self) -> GridToWorld {
        self.grid_to_world
    }
}

/// Borrowed canonical bytes and coordinates for one scientific identity tile.
#[derive(Clone, Copy, Debug)]
pub struct ScientificTile<'a> {
    origin_tzyx: [u64; 4],
    extent_tzyx: [u64; 4],
    validity: &'a [u8],
    values: &'a [u8],
}

impl<'a> ScientificTile<'a> {
    pub const fn new(
        origin_tzyx: [u64; 4],
        extent_tzyx: [u64; 4],
        validity: &'a [u8],
        values: &'a [u8],
    ) -> Self {
        Self {
            origin_tzyx,
            extent_tzyx,
            validity,
            values,
        }
    }

    pub const fn origin_tzyx(self) -> [u64; 4] {
        self.origin_tzyx
    }

    pub const fn extent_tzyx(self) -> [u64; 4] {
        self.extent_tzyx
    }

    pub const fn validity(self) -> &'a [u8] {
        self.validity
    }

    pub const fn values(self) -> &'a [u8] {
        self.values
    }
}

/// A verified layer root ready for ordered dataset-root assembly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScientificLayerRoot {
    layer: LogicalLayerKey,
    digest: Sha256Digest,
}

impl ScientificLayerRoot {
    pub const fn layer(self) -> LogicalLayerKey {
        self.layer
    }

    pub const fn digest(self) -> Sha256Digest {
        self.digest
    }
}

/// Incrementally computes one bounded scientific layer root.
pub struct ScientificLayerHasher {
    descriptor: ScientificLayerDescriptor,
    tile_count: u64,
    next_tile: u64,
    merkle: MerkleAccumulator,
    failed: bool,
}

impl ScientificLayerHasher {
    pub fn new(descriptor: ScientificLayerDescriptor) -> Result<Self, ScientificHashError> {
        let tile_count = identity_tile_count(descriptor.shape)?;
        Ok(Self {
            descriptor,
            tile_count,
            next_tile: 0,
            merkle: MerkleAccumulator::new(),
            failed: false,
        })
    }

    pub const fn expected_tile_count(&self) -> u64 {
        self.tile_count
    }

    pub const fn accepted_tile_count(&self) -> u64 {
        self.next_tile
    }

    pub fn push_tile(&mut self, tile: ScientificTile<'_>) -> Result<(), ScientificHashError> {
        if self.failed {
            return Err(ScientificHashError::PreviouslyFailed);
        }
        let result = self.validate_and_hash_tile(tile);
        match result {
            Ok(digest) => {
                if let Err(error) = self.merkle.push_leaf(digest) {
                    self.failed = true;
                    return Err(error);
                }
                self.next_tile += 1;
                Ok(())
            }
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    /// Consumes an in-progress computation without producing an identity.
    pub fn cancel(self) {}

    pub fn finalize(self) -> Result<ScientificLayerRoot, ScientificHashError> {
        if self.failed {
            return Err(ScientificHashError::PreviouslyFailed);
        }
        if self.next_tile != self.tile_count {
            return Err(ScientificHashError::IncompleteLayer {
                expected: self.tile_count,
                actual: self.next_tile,
            });
        }
        let tree_root = self.merkle.finalize()?;
        let mut hasher = Sha256Hasher::new();
        hasher.update(LAYER_DOMAIN);
        update_u32(&mut hasher, self.descriptor.layer.ordinal());
        update_u8(&mut hasher, dtype_tag(self.descriptor.dtype));
        for dimension in self.descriptor.shape.dimensions() {
            update_u64(&mut hasher, dimension);
        }
        self.descriptor.temporal.update_hasher(&mut hasher)?;
        for value in self.descriptor.grid_to_world.row_major() {
            hasher.update(f64_hex(value).ok_or(ScientificHashError::NonFiniteTransform)?);
        }
        update_u64(&mut hasher, self.tile_count);
        hasher.update(tree_root.as_bytes());
        Ok(ScientificLayerRoot {
            layer: self.descriptor.layer,
            digest: hasher.finalize(),
        })
    }

    fn validate_and_hash_tile(
        &self,
        tile: ScientificTile<'_>,
    ) -> Result<Sha256Digest, ScientificHashError> {
        if self.next_tile >= self.tile_count {
            return Err(ScientificHashError::TooManyTiles {
                expected: self.tile_count,
            });
        }
        let (expected_origin, expected_extent) =
            expected_tile(self.descriptor.shape, self.next_tile)?;
        if tile.origin_tzyx != expected_origin {
            return Err(ScientificHashError::UnexpectedTileOrigin {
                expected: expected_origin,
                actual: tile.origin_tzyx,
            });
        }
        if tile.extent_tzyx != expected_extent {
            return Err(ScientificHashError::UnexpectedTileExtent {
                expected: expected_extent,
                actual: tile.extent_tzyx,
            });
        }

        let voxel_count =
            checked_product(tile.extent_tzyx).ok_or(ScientificHashError::TileVoxelCountOverflow)?;
        let validity_len_u64 = voxel_count
            .checked_add(7)
            .ok_or(ScientificHashError::TileVoxelCountOverflow)?
            / 8;
        let validity_len = usize::try_from(validity_len_u64)
            .map_err(|_| ScientificHashError::TileByteLengthOverflow)?;
        if tile.validity.len() != validity_len {
            return Err(ScientificHashError::InvalidValidityByteLength {
                expected: validity_len,
                actual: tile.validity.len(),
            });
        }
        let used_bits = (voxel_count % 8) as u8;
        if used_bits != 0 {
            let used_mask = (1_u8 << used_bits) - 1;
            if tile
                .validity
                .last()
                .is_some_and(|byte| byte & !used_mask != 0)
            {
                return Err(ScientificHashError::NonZeroValidityPadding);
            }
        }

        let value_len_u64 = voxel_count
            .checked_mul(u64::from(self.descriptor.dtype.bytes_per_sample()))
            .ok_or(ScientificHashError::TileVoxelCountOverflow)?;
        let value_len = usize::try_from(value_len_u64)
            .map_err(|_| ScientificHashError::TileByteLengthOverflow)?;
        if tile.values.len() != value_len {
            return Err(ScientificHashError::InvalidValueByteLength {
                expected: value_len,
                actual: tile.values.len(),
            });
        }
        validate_samples(
            self.descriptor.dtype,
            tile.validity,
            tile.values,
            usize::try_from(voxel_count)
                .map_err(|_| ScientificHashError::TileByteLengthOverflow)?,
        )?;

        let mut hasher = Sha256Hasher::new();
        hasher.update(TILE_DOMAIN);
        update_u32(&mut hasher, self.descriptor.layer.ordinal());
        update_u8(&mut hasher, dtype_tag(self.descriptor.dtype));
        for coordinate in tile.origin_tzyx {
            update_u64(&mut hasher, coordinate);
        }
        for extent in tile.extent_tzyx {
            update_u64(&mut hasher, extent);
        }
        update_u64(&mut hasher, validity_len_u64);
        hasher.update(tile.validity);
        update_u64(&mut hasher, value_len_u64);
        hasher.update(tile.values);
        Ok(hasher.finalize())
    }
}

/// Incrementally binds ordered layer roots into one scientific-content ID.
pub struct ScientificDatasetHasher {
    hasher: Sha256Hasher,
    layer_count: u32,
    next_layer: u32,
    failed: bool,
}

impl ScientificDatasetHasher {
    pub fn new(layer_count: u32) -> Result<Self, ScientificHashError> {
        if layer_count == 0 {
            return Err(ScientificHashError::EmptyDataset);
        }
        let mut hasher = Sha256Hasher::new();
        hasher.update(DATASET_DOMAIN);
        hasher.update([1, 1, 1, 1]);
        update_u32(&mut hasher, layer_count);
        Ok(Self {
            hasher,
            layer_count,
            next_layer: 0,
            failed: false,
        })
    }

    pub fn push_layer(&mut self, root: ScientificLayerRoot) -> Result<(), ScientificHashError> {
        if self.failed {
            return Err(ScientificHashError::PreviouslyFailed);
        }
        if self.next_layer >= self.layer_count {
            self.failed = true;
            return Err(ScientificHashError::TooManyLayers {
                expected: self.layer_count,
            });
        }
        let actual = root.layer.ordinal();
        if actual != self.next_layer {
            self.failed = true;
            return Err(ScientificHashError::UnexpectedLayerOrdinal {
                expected: self.next_layer,
                actual,
            });
        }
        update_u32(&mut self.hasher, actual);
        self.hasher.update(root.digest.as_bytes());
        self.next_layer += 1;
        Ok(())
    }

    /// Consumes an in-progress computation without producing an identity.
    pub fn cancel(self) {}

    pub fn finalize(self) -> Result<ScientificContentId, ScientificHashError> {
        if self.failed {
            return Err(ScientificHashError::PreviouslyFailed);
        }
        if self.next_layer != self.layer_count {
            return Err(ScientificHashError::IncompleteDataset {
                expected: self.layer_count,
                actual: self.next_layer,
            });
        }
        Ok(ScientificContentId::from_digest(self.hasher.finalize()))
    }
}

struct MerkleAccumulator {
    levels: Vec<Vec<Sha256Digest>>,
    leaf_count: u64,
}

impl MerkleAccumulator {
    fn new() -> Self {
        Self {
            levels: Vec::new(),
            leaf_count: 0,
        }
    }

    fn push_leaf(&mut self, digest: Sha256Digest) -> Result<(), ScientificHashError> {
        self.leaf_count = self
            .leaf_count
            .checked_add(1)
            .ok_or(ScientificHashError::TileCountOverflow)?;
        self.push_at_level(0, digest)
    }

    fn push_at_level(
        &mut self,
        level: usize,
        digest: Sha256Digest,
    ) -> Result<(), ScientificHashError> {
        if self.levels.len() <= level {
            self.levels.resize_with(level + 1, Vec::new);
        }
        self.levels[level].push(digest);
        if self.levels[level].len() == MERKLE_ARITY {
            let children = mem::take(&mut self.levels[level]);
            let parent = hash_node(level + 1, &children)?;
            self.push_at_level(level + 1, parent)?;
        }
        Ok(())
    }

    fn finalize(mut self) -> Result<Sha256Digest, ScientificHashError> {
        if self.leaf_count == 0 {
            return Err(ScientificHashError::EmptyTree);
        }
        loop {
            let buffered = self.levels.iter().map(Vec::len).sum::<usize>();
            if buffered == 1 {
                return self
                    .levels
                    .into_iter()
                    .find_map(|mut level| level.pop())
                    .ok_or(ScientificHashError::EmptyTree);
            }
            let level = self
                .levels
                .iter()
                .position(|digests| !digests.is_empty())
                .ok_or(ScientificHashError::EmptyTree)?;
            let children = mem::take(&mut self.levels[level]);
            let parent = hash_node(level + 1, &children)?;
            self.push_at_level(level + 1, parent)?;
        }
    }
}

fn hash_node(level: usize, children: &[Sha256Digest]) -> Result<Sha256Digest, ScientificHashError> {
    if children.is_empty() || children.len() > MERKLE_ARITY {
        return Err(ScientificHashError::InvalidTreeChildCount);
    }
    let level = u32::try_from(level).map_err(|_| ScientificHashError::TreeLevelOverflow)?;
    let child_count =
        u32::try_from(children.len()).map_err(|_| ScientificHashError::InvalidTreeChildCount)?;
    let mut hasher = Sha256Hasher::new();
    hasher.update(NODE_DOMAIN);
    update_u32(&mut hasher, level);
    update_u32(&mut hasher, child_count);
    for child in children {
        hasher.update(child.as_bytes());
    }
    Ok(hasher.finalize())
}

fn identity_tile_count(shape: Shape4D) -> Result<u64, ScientificHashError> {
    let dimensions = shape.dimensions();
    let mut count = 1_u64;
    for (dimension, tile) in dimensions.into_iter().zip(SCIENTIFIC_TILE_SHAPE_TZYX) {
        let along_axis = dimension.div_ceil(tile);
        count = count
            .checked_mul(along_axis)
            .ok_or(ScientificHashError::TileCountOverflow)?;
    }
    Ok(count)
}

fn expected_tile(
    shape: Shape4D,
    linear_index: u64,
) -> Result<([u64; 4], [u64; 4]), ScientificHashError> {
    let dimensions = shape.dimensions();
    let counts: [u64; 4] =
        std::array::from_fn(|axis| dimensions[axis].div_ceil(SCIENTIFIC_TILE_SHAPE_TZYX[axis]));
    let mut remainder = linear_index;
    let mut tile_indices = [0_u64; 4];
    for axis in (0..4).rev() {
        tile_indices[axis] = remainder % counts[axis];
        remainder /= counts[axis];
    }
    let mut origin = [0_u64; 4];
    let mut extent = [0_u64; 4];
    for axis in 0..4 {
        origin[axis] = tile_indices[axis]
            .checked_mul(SCIENTIFIC_TILE_SHAPE_TZYX[axis])
            .ok_or(ScientificHashError::TileCoordinateOverflow)?;
        extent[axis] = SCIENTIFIC_TILE_SHAPE_TZYX[axis].min(dimensions[axis] - origin[axis]);
    }
    Ok((origin, extent))
}

fn checked_product(values: [u64; 4]) -> Option<u64> {
    values
        .into_iter()
        .try_fold(1_u64, |product, value| product.checked_mul(value))
}

fn dtype_tag(dtype: IntensityDType) -> u8 {
    match dtype {
        IntensityDType::Uint8 => 1,
        IntensityDType::Uint16 => 2,
        IntensityDType::Float32 => 3,
    }
}

fn validate_samples(
    dtype: IntensityDType,
    validity: &[u8],
    values: &[u8],
    voxel_count: usize,
) -> Result<(), ScientificHashError> {
    for index in 0..voxel_count {
        let valid = validity[index / 8] & (1 << (index % 8)) != 0;
        match dtype {
            IntensityDType::Uint8 => {
                if !valid && values[index] != 0 {
                    return Err(ScientificHashError::InvalidIntegerSentinel { index });
                }
            }
            IntensityDType::Uint16 => {
                let offset = index * 2;
                if !valid && values[offset..offset + 2] != [0, 0] {
                    return Err(ScientificHashError::InvalidIntegerSentinel { index });
                }
            }
            IntensityDType::Float32 => {
                let offset = index * 4;
                let bits = u32::from_le_bytes(
                    values[offset..offset + 4]
                        .try_into()
                        .expect("validated sample width"),
                );
                if !valid && bits != 0 {
                    return Err(ScientificHashError::InvalidFloatSentinel { index });
                }
                if valid && !f32::from_bits(bits).is_finite() {
                    return Err(ScientificHashError::NonFiniteFloatSample { index });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(layer: u32, dtype: IntensityDType, shape: Shape4D) -> ScientificLayerDescriptor {
        ScientificLayerDescriptor::new(
            LogicalLayerKey::new(layer),
            dtype,
            shape,
            ScientificTemporalCalibration::Unknown,
            GridToWorld::identity(),
        )
        .unwrap()
    }

    fn all_valid(voxels: usize) -> Vec<u8> {
        let mut bytes = vec![0xff; voxels.div_ceil(8)];
        if !voxels.is_multiple_of(8) {
            *bytes.last_mut().unwrap() = (1 << (voxels % 8)) - 1;
        }
        bytes
    }

    #[test]
    fn exact_one_voxel_scientific_vector_matches() {
        let shape = Shape4D::new(1, 1, 1, 1).unwrap();
        let mut layer =
            ScientificLayerHasher::new(descriptor(0, IntensityDType::Uint8, shape)).unwrap();
        layer
            .push_tile(ScientificTile::new(
                [0, 0, 0, 0],
                [1, 1, 1, 1],
                &[0x01],
                &[0x07],
            ))
            .unwrap();
        let root = layer.finalize().unwrap();
        assert_eq!(
            root.digest().to_string(),
            "a2ef9d5a469e4934434ef7dcb9df27a63dce43df2c6e31d77a4e894cfaf24e4f"
        );

        let mut dataset = ScientificDatasetHasher::new(1).unwrap();
        dataset.push_layer(root).unwrap();
        assert_eq!(
            dataset.finalize().unwrap().to_string(),
            concat!(
                "m4d-sc-v1-sha256:",
                "1dd0a7a4ce0561326783f5cdf7b6eeff476a9628061d35c705f4af2c863ad392"
            )
        );
    }

    #[test]
    fn nondivisible_edge_tiles_are_required_in_canonical_order() {
        let shape = Shape4D::new(1, 1, 1, 257).unwrap();
        let mut layer =
            ScientificLayerHasher::new(descriptor(0, IntensityDType::Uint8, shape)).unwrap();
        assert_eq!(layer.expected_tile_count(), 2);

        let first_validity = all_valid(256);
        let first_values = vec![0_u8; 256];
        layer
            .push_tile(ScientificTile::new(
                [0, 0, 0, 0],
                [1, 1, 1, 256],
                &first_validity,
                &first_values,
            ))
            .unwrap();
        layer
            .push_tile(ScientificTile::new(
                [0, 0, 0, 256],
                [1, 1, 1, 1],
                &[0x01],
                &[0],
            ))
            .unwrap();
        assert!(layer.finalize().is_ok());
    }

    #[test]
    fn rejected_tile_poisoning_prevents_an_identity() {
        let shape = Shape4D::new(1, 1, 1, 1).unwrap();
        let mut layer =
            ScientificLayerHasher::new(descriptor(0, IntensityDType::Uint8, shape)).unwrap();
        assert!(matches!(
            layer.push_tile(ScientificTile::new([0, 0, 0, 1], [1, 1, 1, 1], &[1], &[0])),
            Err(ScientificHashError::UnexpectedTileOrigin { .. })
        ));
        assert_eq!(layer.finalize(), Err(ScientificHashError::PreviouslyFailed));
    }

    #[test]
    fn canonical_validity_padding_and_invalid_sentinels_are_enforced() {
        let shape = Shape4D::new(1, 1, 1, 1).unwrap();
        let mut bad_padding =
            ScientificLayerHasher::new(descriptor(0, IntensityDType::Uint8, shape)).unwrap();
        assert_eq!(
            bad_padding.push_tile(ScientificTile::new([0; 4], [1; 4], &[0x81], &[0])),
            Err(ScientificHashError::NonZeroValidityPadding)
        );

        let mut bad_sentinel =
            ScientificLayerHasher::new(descriptor(0, IntensityDType::Uint16, shape)).unwrap();
        assert_eq!(
            bad_sentinel.push_tile(ScientificTile::new([0; 4], [1; 4], &[0], &[1, 0])),
            Err(ScientificHashError::InvalidIntegerSentinel { index: 0 })
        );
    }

    #[test]
    fn nonfinite_valid_float_rejects_but_signed_zero_and_subnormal_survive() {
        let shape = Shape4D::new(1, 1, 1, 3).unwrap();
        let mut accepted =
            ScientificLayerHasher::new(descriptor(0, IntensityDType::Float32, shape)).unwrap();
        let values = [(-0.0_f32).to_bits(), 1_u32, 1.0_f32.to_bits()]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        accepted
            .push_tile(ScientificTile::new(
                [0; 4],
                [1, 1, 1, 3],
                &[0b0000_0111],
                &values,
            ))
            .unwrap();
        assert!(accepted.finalize().is_ok());

        let bad_shape = Shape4D::new(1, 1, 1, 1).unwrap();
        let mut rejected =
            ScientificLayerHasher::new(descriptor(0, IntensityDType::Float32, bad_shape)).unwrap();
        assert_eq!(
            rejected.push_tile(ScientificTile::new(
                [0; 4],
                [1; 4],
                &[1],
                &f32::NAN.to_bits().to_le_bytes()
            )),
            Err(ScientificHashError::NonFiniteFloatSample { index: 0 })
        );
    }

    #[test]
    fn temporal_encodings_reject_ambiguous_or_noncanonical_values() {
        let shape = Shape4D::new(2, 1, 1, 1).unwrap();
        assert_eq!(
            ScientificLayerDescriptor::new(
                LogicalLayerKey::new(0),
                IntensityDType::Uint8,
                shape,
                ScientificTemporalCalibration::Regular { step_seconds: 0.0 },
                GridToWorld::identity()
            ),
            Err(ScientificHashError::InvalidTemporalStep)
        );
        assert!(matches!(
            ScientificLayerDescriptor::new(
                LogicalLayerKey::new(0),
                IntensityDType::Uint8,
                shape,
                ScientificTemporalCalibration::Explicit {
                    positions_seconds: vec![-0.0, 1.0]
                },
                GridToWorld::identity()
            ),
            Err(ScientificHashError::InvalidTemporalPosition { index: 0, .. })
        ));
    }

    #[test]
    fn dataset_rejects_noncanonical_layer_order_and_remains_failed() {
        let root = ScientificLayerRoot {
            layer: LogicalLayerKey::new(1),
            digest: Sha256Hasher::digest(b"layer-one"),
        };
        let mut dataset = ScientificDatasetHasher::new(2).unwrap();
        assert_eq!(
            dataset.push_layer(root),
            Err(ScientificHashError::UnexpectedLayerOrdinal {
                expected: 0,
                actual: 1
            })
        );
        assert_eq!(
            dataset.finalize(),
            Err(ScientificHashError::PreviouslyFailed)
        );
    }

    #[test]
    fn merkle_boundaries_match_independent_fixed_vectors() {
        let expected = [
            (
                1,
                "af5570f5a1810b7af78caf4bc70a660f0df51e42baf91d4de5b2328de0e83dfc",
            ),
            (
                1023,
                "8fbc67fcf2006a1b4a7e490a741c1ef87f922619525471730a1bcf1922adc4e0",
            ),
            (
                1024,
                "e5acecfbb30a6a36d183c6dc02f069fd98d07c6fa5e052d6efb94fbec81ca05b",
            ),
            (
                1025,
                "4ecfc5c533a154d369ffad4004dd8133c18c7e8d26b4e3a0d1ec97c91bbacc8a",
            ),
        ];
        for (count, expected_root) in expected {
            let mut merkle = MerkleAccumulator::new();
            for index in 0_u64..count {
                merkle
                    .push_leaf(Sha256Hasher::digest(index.to_be_bytes()))
                    .unwrap();
            }
            assert_eq!(merkle.finalize().unwrap().to_string(), expected_root);
        }
    }
}
