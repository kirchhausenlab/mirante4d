use eframe::egui;
use mirante4d_core::{CameraView, PresentationViewport, Projection, TimeIndex};

use crate::{
    RenderIsoShadingPolicy, RenderMode, RenderSamplingPolicy,
    viewer_layout::{PanelId, ViewerLayout},
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum WorkbenchCommand {
    SetRenderMode(RenderMode),
    SetLayerRenderMode {
        layer_index: usize,
        mode: RenderMode,
    },
    SetIsoDisplayLevel {
        display_level: f32,
    },
    SetIsoLightAttached {
        attached: bool,
    },
    SetIsoLightDetachedPosition {
        x: f32,
        y: f32,
    },
    ResetIsoLight,
    SetDvrDensityScale {
        density_scale: f64,
    },
    SetRenderSamplingPolicy(RenderSamplingPolicy),
    SetRenderIsoShadingPolicy(RenderIsoShadingPolicy),
    SetViewerLayout(ViewerLayout),
    SetProjection(Projection),
    ResetView,
    FitData,
    SelectLayer(usize),
    SetTimepoint(TimeIndex),
    StepTimepoint {
        delta: i64,
    },
    SetPlayback {
        playing: bool,
    },
    SetLayerVisibility {
        layer_index: usize,
        visible: bool,
    },
    SetLayerOpacity {
        layer_index: usize,
        opacity: f32,
    },
    SetLayerWindow {
        layer_index: usize,
        low: f32,
        high: f32,
    },
    SetLayerColor {
        layer_index: usize,
        color_rgba: [f32; 4],
    },
    SetLayerGamma {
        layer_index: usize,
        gamma: f32,
    },
    SetLayerInvert {
        layer_index: usize,
        invert: bool,
    },
    SetLayerDvrOpacityWindow {
        layer_index: usize,
        low: f32,
        high: f32,
    },
    SetLayerDvrOpacityGamma {
        layer_index: usize,
        gamma: f32,
    },
    AutoLayerDvrOpacity {
        layer_index: usize,
    },
    ResetLayerDvrOpacity {
        layer_index: usize,
    },
    SetLayerTransferPreset {
        layer_index: usize,
        preset: BuiltInTransferPreset,
    },
    ApplyChannelPreset {
        preset_index: usize,
    },
    SaveCurrentChannelPreset,
    UpdateChannelPreset {
        preset_index: usize,
    },
    CameraPanDrag {
        motion_points: egui::Vec2,
    },
    CameraOrbitDrag {
        start_camera: CameraView,
        start_position_points: egui::Pos2,
        current_position_points: egui::Pos2,
        viewport_size_points: egui::Vec2,
    },
    CameraZoom {
        scroll_y_points: f32,
    },
    CrossSectionPanDrag {
        panel_id: PanelId,
        motion_points: egui::Vec2,
    },
    CrossSectionSliceStep {
        panel_id: PanelId,
        notches: f64,
        fast: bool,
    },
    CrossSectionZoom {
        panel_id: PanelId,
        presentation_viewport: PresentationViewport,
        pointer_position_points: egui::Pos2,
        scroll_y_points: f32,
    },
    CrossSectionRotateDrag {
        panel_id: PanelId,
        motion_points: egui::Vec2,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct WorkbenchCommandOutcome {
    pub(crate) rerender_requested: bool,
    pub(crate) texture_refresh_requested: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuiltInTransferPreset {
    Linear,
    BrightGamma,
    HighContrast,
}
