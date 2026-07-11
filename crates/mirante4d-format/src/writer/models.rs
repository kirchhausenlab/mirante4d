use mirante4d_core::{GridToWorld, IntensityDType, LayerDisplay, Shape4D, WorldSpace};

use crate::manifest::{ChannelMetadata, NoDataPolicy, ScaleReduction};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistingPackagePolicy {
    Fail,
    Replace,
}

#[derive(Debug, Clone)]
pub struct NativeU16Dataset {
    pub id: String,
    pub name: String,
    pub world_space: WorldSpace,
    pub layers: Vec<DenseU16Layer>,
}

#[derive(Debug, Clone)]
pub struct DenseU16Layer {
    pub id: String,
    pub name: String,
    pub channel: ChannelMetadata,
    pub shape: Shape4D,
    pub brick_shape: Shape4D,
    pub grid_to_world: GridToWorld,
    pub display: LayerDisplay,
    pub values_tzyx: Vec<u16>,
}

#[derive(Debug, Clone)]
pub struct NativeU16MultiscaleDataset {
    pub id: String,
    pub name: String,
    pub world_space: WorldSpace,
    pub layers: Vec<DenseU16MultiscaleLayer>,
}

#[derive(Debug, Clone)]
pub struct DenseU16MultiscaleLayer {
    pub id: String,
    pub name: String,
    pub channel: ChannelMetadata,
    pub shape: Shape4D,
    pub grid_to_world: GridToWorld,
    pub display: LayerDisplay,
    pub scales: Vec<DenseU16Scale>,
}

#[derive(Debug, Clone)]
pub struct DenseU16Scale {
    pub level: u32,
    pub shape: Shape4D,
    pub brick_shape: Shape4D,
    pub grid_to_world: GridToWorld,
    pub source_scale: Option<u32>,
    pub reduction: ScaleReduction,
    pub values_tzyx: Vec<u16>,
}

#[derive(Debug, Clone)]
pub struct NativeF32Dataset {
    pub id: String,
    pub name: String,
    pub world_space: WorldSpace,
    pub layers: Vec<DenseF32Layer>,
}

#[derive(Debug, Clone)]
pub struct DenseF32Layer {
    pub id: String,
    pub name: String,
    pub channel: ChannelMetadata,
    pub shape: Shape4D,
    pub brick_shape: Shape4D,
    pub grid_to_world: GridToWorld,
    pub display: LayerDisplay,
    pub values_tzyx: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct NativeF32MultiscaleDataset {
    pub id: String,
    pub name: String,
    pub world_space: WorldSpace,
    pub layers: Vec<DenseF32MultiscaleLayer>,
}

#[derive(Debug, Clone)]
pub struct DenseF32MultiscaleLayer {
    pub id: String,
    pub name: String,
    pub channel: ChannelMetadata,
    pub shape: Shape4D,
    pub grid_to_world: GridToWorld,
    pub display: LayerDisplay,
    pub scales: Vec<DenseF32Scale>,
}

#[derive(Debug, Clone)]
pub struct DenseF32Scale {
    pub level: u32,
    pub shape: Shape4D,
    pub brick_shape: Shape4D,
    pub grid_to_world: GridToWorld,
    pub source_scale: Option<u32>,
    pub reduction: ScaleReduction,
    pub values_tzyx: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct StreamingU16LayerSpec {
    pub id: String,
    pub name: String,
    pub channel: ChannelMetadata,
    pub source_dtype: IntensityDType,
    pub shape: Shape4D,
    pub grid_to_world: GridToWorld,
    pub display: LayerDisplay,
    pub scales: Vec<StreamingU16ScaleSpec>,
}

#[derive(Debug, Clone)]
pub struct StreamingU16ScaleSpec {
    pub level: u32,
    pub shape: Shape4D,
    pub brick_shape: Shape4D,
    pub grid_to_world: GridToWorld,
    pub source_scale: Option<u32>,
    pub reduction: ScaleReduction,
}

#[derive(Debug, Clone)]
pub struct StreamingU8LayerSpec {
    pub id: String,
    pub name: String,
    pub channel: ChannelMetadata,
    pub shape: Shape4D,
    pub no_data_policy: Option<NoDataPolicy>,
    pub grid_to_world: GridToWorld,
    pub display: LayerDisplay,
    pub scales: Vec<StreamingU8ScaleSpec>,
}

#[derive(Debug, Clone)]
pub struct StreamingU8ScaleSpec {
    pub level: u32,
    pub shape: Shape4D,
    pub brick_shape: Shape4D,
    pub grid_to_world: GridToWorld,
    pub source_scale: Option<u32>,
    pub reduction: ScaleReduction,
}

#[derive(Debug, Clone)]
pub struct StreamingF32LayerSpec {
    pub id: String,
    pub name: String,
    pub channel: ChannelMetadata,
    pub shape: Shape4D,
    pub grid_to_world: GridToWorld,
    pub display: LayerDisplay,
    pub scales: Vec<StreamingF32ScaleSpec>,
}

#[derive(Debug, Clone)]
pub struct StreamingF32ScaleSpec {
    pub level: u32,
    pub shape: Shape4D,
    pub brick_shape: Shape4D,
    pub grid_to_world: GridToWorld,
    pub source_scale: Option<u32>,
    pub reduction: ScaleReduction,
}
