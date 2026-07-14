use mirante4d_dataset::{DatasetCatalog, DatasetResourceKey, ResourceRegion};
use mirante4d_domain::{IntensityDType, LogicalLayerKey, ScaleLevel, Shape3D, TimeIndex};
use mirante4d_identity::ScientificContentId;

use crate::AnalysisError;

pub const DEFAULT_ANALYSIS_BLOCK_SHAPE: [u64; 3] = [64, 64, 64];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisOperation {
    FullIntensitySummary,
    BoxRoiIntensityStatistics,
}

impl AnalysisOperation {
    pub const fn contract_name(self) -> &'static str {
        match self {
            Self::FullIntensitySummary => "full-intensity-summary-v1",
            Self::BoxRoiIntensityStatistics => "box-roi-intensity-statistics-v1",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisDefinition {
    source_content_id: ScientificContentId,
    layer: LogicalLayerKey,
    dtype: IntensityDType,
    time_start: u64,
    time_end_exclusive: u64,
    spatial_region: ResourceRegion,
    block_shape: Shape3D,
    operation: AnalysisOperation,
}

impl AnalysisDefinition {
    pub fn full_intensity_summary(
        catalog: &DatasetCatalog,
        layer: LogicalLayerKey,
        time_start: u64,
        time_end_exclusive: u64,
    ) -> Result<Self, AnalysisError> {
        let layer_facts = catalog.layer(layer).ok_or(AnalysisError::UnknownLayer)?;
        let region = ResourceRegion::new([0; 3], layer_facts.shape().spatial())
            .map_err(|_| AnalysisError::InvalidRegion)?;
        Self::new(
            catalog,
            layer,
            time_start,
            time_end_exclusive,
            region,
            AnalysisOperation::FullIntensitySummary,
            Shape3D::new(
                DEFAULT_ANALYSIS_BLOCK_SHAPE[0],
                DEFAULT_ANALYSIS_BLOCK_SHAPE[1],
                DEFAULT_ANALYSIS_BLOCK_SHAPE[2],
            )
            .expect("the fixed analysis block shape is nonzero"),
        )
    }

    pub fn box_roi_intensity_statistics(
        catalog: &DatasetCatalog,
        layer: LogicalLayerKey,
        time_start: u64,
        time_end_exclusive: u64,
        spatial_region: ResourceRegion,
    ) -> Result<Self, AnalysisError> {
        Self::new(
            catalog,
            layer,
            time_start,
            time_end_exclusive,
            spatial_region,
            AnalysisOperation::BoxRoiIntensityStatistics,
            Shape3D::new(
                DEFAULT_ANALYSIS_BLOCK_SHAPE[0],
                DEFAULT_ANALYSIS_BLOCK_SHAPE[1],
                DEFAULT_ANALYSIS_BLOCK_SHAPE[2],
            )
            .expect("the fixed analysis block shape is nonzero"),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        catalog: &DatasetCatalog,
        layer: LogicalLayerKey,
        time_start: u64,
        time_end_exclusive: u64,
        spatial_region: ResourceRegion,
        operation: AnalysisOperation,
        block_shape: Shape3D,
    ) -> Result<Self, AnalysisError> {
        let source_content_id = *catalog
            .scientific_identity()
            .verified_id()
            .ok_or(AnalysisError::UnverifiedSource)?;
        let layer_facts = catalog.layer(layer).ok_or(AnalysisError::UnknownLayer)?;
        if time_start >= time_end_exclusive || time_end_exclusive > layer_facts.shape().t() {
            return Err(AnalysisError::InvalidTimeRange);
        }
        if !spatial_region.fits_within(layer_facts.shape().spatial()) {
            return Err(AnalysisError::InvalidRegion);
        }
        if block_shape
            .dimensions()
            .into_iter()
            .any(|dimension| dimension > 64)
        {
            return Err(AnalysisError::InvalidBlockShape);
        }
        Ok(Self {
            source_content_id,
            layer,
            dtype: layer_facts.dtype(),
            time_start,
            time_end_exclusive,
            spatial_region,
            block_shape,
            operation,
        })
    }

    pub const fn source_content_id(&self) -> ScientificContentId {
        self.source_content_id
    }

    pub const fn layer(&self) -> LogicalLayerKey {
        self.layer
    }

    pub const fn dtype(&self) -> IntensityDType {
        self.dtype
    }

    pub const fn time_start(&self) -> u64 {
        self.time_start
    }

    pub const fn time_end_exclusive(&self) -> u64 {
        self.time_end_exclusive
    }

    pub const fn spatial_region(&self) -> ResourceRegion {
        self.spatial_region
    }

    pub const fn block_shape(&self) -> Shape3D {
        self.block_shape
    }

    pub const fn operation(&self) -> AnalysisOperation {
        self.operation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalysisBlock {
    ordinal: u64,
    resource: DatasetResourceKey,
}

impl AnalysisBlock {
    pub const fn ordinal(self) -> u64 {
        self.ordinal
    }

    pub const fn resource(self) -> DatasetResourceKey {
        self.resource
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisPlan {
    definition: AnalysisDefinition,
    tiles_zyx: [u64; 3],
    blocks_per_timepoint: u64,
    total_blocks: u64,
}

impl AnalysisPlan {
    pub fn new(definition: AnalysisDefinition) -> Result<Self, AnalysisError> {
        let spatial = definition.spatial_region().shape().dimensions();
        let block = definition.block_shape().dimensions();
        let tiles_zyx = std::array::from_fn(|axis| spatial[axis].div_ceil(block[axis]));
        let blocks_per_timepoint = tiles_zyx
            .into_iter()
            .try_fold(1_u64, |product, value| product.checked_mul(value))
            .ok_or(AnalysisError::CapacityExceeded)?;
        let timepoints = definition
            .time_end_exclusive()
            .checked_sub(definition.time_start())
            .ok_or(AnalysisError::CapacityExceeded)?;
        let total_blocks = blocks_per_timepoint
            .checked_mul(timepoints)
            .ok_or(AnalysisError::CapacityExceeded)?;
        Ok(Self {
            definition,
            tiles_zyx,
            blocks_per_timepoint,
            total_blocks,
        })
    }

    pub const fn definition(&self) -> &AnalysisDefinition {
        &self.definition
    }

    pub const fn blocks_per_timepoint(&self) -> u64 {
        self.blocks_per_timepoint
    }

    pub const fn total_blocks(&self) -> u64 {
        self.total_blocks
    }

    pub fn block(&self, ordinal: u64) -> Option<AnalysisBlock> {
        if ordinal >= self.total_blocks {
            return None;
        }
        let time_offset = ordinal / self.blocks_per_timepoint;
        let within_time = ordinal % self.blocks_per_timepoint;
        let tile_x = within_time % self.tiles_zyx[2];
        let tile_y = (within_time / self.tiles_zyx[2]) % self.tiles_zyx[1];
        let tile_z = within_time / (self.tiles_zyx[1] * self.tiles_zyx[2]);
        let tile = [tile_z, tile_y, tile_x];
        let block_shape = self.definition.block_shape().dimensions();
        let analysis_origin = self.definition.spatial_region().origin();
        let analysis_end = self.definition.spatial_region().end_exclusive();
        let mut origin = [0_u64; 3];
        let mut shape = [0_u64; 3];
        for axis in 0..3 {
            origin[axis] = analysis_origin[axis] + tile[axis] * block_shape[axis];
            shape[axis] = block_shape[axis].min(analysis_end[axis] - origin[axis]);
        }
        let region = ResourceRegion::new(
            origin,
            Shape3D::new(shape[0], shape[1], shape[2])
                .expect("a clipped analysis block is nonempty"),
        )
        .expect("a planned region is bounded by a validated dataset shape");
        let resource = DatasetResourceKey::new(
            mirante4d_dataset::DatasetResourceIdentity::Verified(
                self.definition.source_content_id(),
            ),
            self.definition.layer(),
            TimeIndex::new(self.definition.time_start() + time_offset),
            ScaleLevel::BASE,
            region,
        );
        Some(AnalysisBlock { ordinal, resource })
    }
}
