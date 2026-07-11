use mirante4d_domain::{IntensityDType, ShapeError};
use mirante4d_format::{BrickIndex, CurrentFormatIdError, FormatError, LayerKind};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DataError {
    #[error(transparent)]
    Format(#[from] FormatError),
    #[error(transparent)]
    InvalidId(#[from] CurrentFormatIdError),
    #[error("invalid runtime shape: {0}")]
    InvalidShape(#[from] ShapeError),
    #[error("layer {0:?} was not found")]
    LayerNotFound(String),
    #[error("scale s{scale_level} was not found for layer {layer_id:?}")]
    ScaleNotFound { layer_id: String, scale_level: u32 },
    #[error("layer {layer_id:?} kind {kind:?} is not supported by this read path")]
    UnsupportedLayerKind { layer_id: String, kind: LayerKind },
    #[error("layer {layer_id:?} stored dtype {dtype:?} is not supported by this read path")]
    UnsupportedDType {
        layer_id: String,
        dtype: IntensityDType,
    },
    #[error(
        "timepoint {timepoint} is out of range for layer {layer_id:?} with {timepoints} timepoints"
    )]
    TimepointOutOfRange {
        layer_id: String,
        timepoint: u64,
        timepoints: u64,
    },
    #[error("failed to read layer {layer_id:?}: {message}")]
    ReadFailed { layer_id: String, message: String },
    #[error("region dimensions must be positive, got z={z_size}, y={y_size}, x={x_size}")]
    InvalidRegionSize {
        z_size: u64,
        y_size: u64,
        x_size: u64,
    },
    #[error("region end overflows u64")]
    RegionOverflow,
    #[error(
        "region z={z_start}..{z_end}, y={y_start}..{y_end}, x={x_start}..{x_end} exceeds shape z={shape_z}, y={shape_y}, x={shape_x}"
    )]
    RegionOutOfBounds {
        z_start: u64,
        z_end: u64,
        y_start: u64,
        y_end: u64,
        x_start: u64,
        x_end: u64,
        shape_z: u64,
        shape_y: u64,
        shape_x: u64,
    },
    #[error(
        "brick index z={z}, y={y}, x={x} exceeds brick grid z={grid_z}, y={grid_y}, x={grid_x}"
    )]
    BrickIndexOutOfBounds {
        z: u64,
        y: u64,
        x: u64,
        grid_z: u64,
        grid_y: u64,
        grid_x: u64,
    },
    #[error(
        "region z={z_start}..{z_end}, y={y_start}..{y_end}, x={x_start}..{x_end} is outside brick region z={brick_z_start}..{brick_z_end}, y={brick_y_start}..{brick_y_end}, x={brick_x_start}..{brick_x_end}"
    )]
    BrickRegionOutOfBounds {
        z_start: u64,
        z_end: u64,
        y_start: u64,
        y_end: u64,
        x_start: u64,
        x_end: u64,
        brick_z_start: u64,
        brick_z_end: u64,
        brick_y_start: u64,
        brick_y_end: u64,
        brick_x_start: u64,
        brick_x_end: u64,
    },
    #[error("brick record is missing for layer {layer_id:?}, brick {index:?}")]
    BrickRecordMissing { layer_id: String, index: BrickIndex },
    #[error(
        "worker count and queue capacity must be positive, got workers={workers}, queue={queue_capacity}"
    )]
    InvalidWorkerConfig {
        workers: usize,
        queue_capacity: usize,
    },
    #[error("brick request queue is full")]
    WorkerQueueFull,
    #[error("brick request queue is closed")]
    WorkerQueueClosed,
    #[error("data engine cache lock is poisoned")]
    CachePoisoned,
    #[error(
        "volume value count mismatch for layer {layer_id:?}: got {actual}, expected {expected}"
    )]
    VolumeValueCountMismatch {
        layer_id: String,
        actual: usize,
        expected: usize,
    },
}
