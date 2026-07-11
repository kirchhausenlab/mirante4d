//! Pure, framework-neutral domain values for Mirante4D.
//!
//! This crate validates small values and owns no filesystem, runtime, UI,
//! renderer, GPU, persistence, or serialization behavior.

#![forbid(unsafe_code)]

mod display;
mod geometry;
mod indices;
mod render;
mod shape;
mod tool;
mod view;

pub use display::{
    DisplayError, DisplayWindow, LayerTransfer, Opacity, RgbColor, TRANSFER_GAMMA_MAX,
    TRANSFER_GAMMA_MIN, TransferCurve,
};
pub use geometry::{GeometryError, GridToWorld, UnitQuaternion, WorldPoint3};
pub use indices::{IntensityDType, LogicalLayerKey, ScaleLevel, TimeIndex};
pub use render::{
    DvrOpacityTransfer, DvrParameters, IsoParameters, IsoShadingPolicy, MipParameters, RenderError,
    RenderMode, RenderState, SamplingPolicy,
};
pub use shape::{Shape3D, Shape4D, ShapeError};
pub use tool::ToolKind;
pub use view::{CameraView, CrossSectionView, IsoLightState, Projection, ViewError, ViewerLayout};
