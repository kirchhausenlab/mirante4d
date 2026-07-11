use mirante4d_core::{
    ChannelColor, ChannelTransferFunction, DisplayWindow, IntensityDType, LayerDisplay,
    PresentationViewport, Shape4D, TimeIndex, TransferCurve, TransferPresetId,
};
use mirante4d_data::SpatialBrickIndex;
use mirante4d_renderer::{MipImageF32, MipImageU16, RenderViewport};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq)]
pub struct AppLayerSummary {
    pub id: String,
    pub name: String,
    pub shape: Shape4D,
    pub dtype: IntensityDType,
    pub display: LayerDisplay,
    pub color: ChannelColor,
    pub curve: TransferCurve,
    pub preset: TransferPresetId,
    pub invert: bool,
    pub dvr_opacity_transfer: DvrOpacityTransfer,
    pub render_state: ChannelRenderState,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DvrOpacityTransfer {
    pub window: DisplayWindow,
    pub curve: TransferCurve,
}

impl DvrOpacityTransfer {
    pub(crate) fn new(window: DisplayWindow, curve: TransferCurve) -> anyhow::Result<Self> {
        Ok(Self {
            window: DisplayWindow::new(window.low, window.high)?,
            curve: curve.validate()?,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MipRenderParameters {
    pub sampling_policy: RenderSamplingPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsoRenderParameters {
    pub sampling_policy: RenderSamplingPolicy,
    pub shading_policy: RenderIsoShadingPolicy,
    pub display_level: f32,
}

impl Default for IsoRenderParameters {
    fn default() -> Self {
        Self {
            sampling_policy: RenderSamplingPolicy::default(),
            shading_policy: RenderIsoShadingPolicy::default(),
            display_level: DEFAULT_ISO_DISPLAY_LEVEL,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DvrRenderParameters {
    pub sampling_policy: RenderSamplingPolicy,
    pub opacity_transfer: DvrOpacityTransfer,
    pub density_scale: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "parameters", rename_all = "snake_case")]
pub enum ChannelRenderState {
    Mip(MipRenderParameters),
    Isosurface(IsoRenderParameters),
    Dvr(DvrRenderParameters),
}

impl ChannelRenderState {
    pub fn mip() -> Self {
        Self::Mip(MipRenderParameters::default())
    }

    pub fn for_mode(
        mode: RenderMode,
        sampling_policy: RenderSamplingPolicy,
        iso_shading_policy: RenderIsoShadingPolicy,
        iso_display_level: f32,
        dvr_opacity_transfer: DvrOpacityTransfer,
        dvr_density_scale: f64,
    ) -> Self {
        match mode {
            RenderMode::Mip => Self::Mip(MipRenderParameters { sampling_policy }),
            RenderMode::Isosurface => Self::Isosurface(IsoRenderParameters {
                sampling_policy,
                shading_policy: iso_shading_policy,
                display_level: iso_display_level,
            }),
            RenderMode::Dvr => Self::Dvr(DvrRenderParameters {
                sampling_policy,
                opacity_transfer: dvr_opacity_transfer,
                density_scale: dvr_density_scale,
            }),
        }
    }

    pub fn mode(self) -> RenderMode {
        match self {
            Self::Mip(_) => RenderMode::Mip,
            Self::Isosurface(_) => RenderMode::Isosurface,
            Self::Dvr(_) => RenderMode::Dvr,
        }
    }

    pub fn sampling_policy(self) -> RenderSamplingPolicy {
        match self {
            Self::Mip(parameters) => parameters.sampling_policy,
            Self::Isosurface(parameters) => parameters.sampling_policy,
            Self::Dvr(parameters) => parameters.sampling_policy,
        }
    }

    pub fn iso_shading_policy(self) -> RenderIsoShadingPolicy {
        match self {
            Self::Isosurface(parameters) => parameters.shading_policy,
            _ => RenderIsoShadingPolicy::default(),
        }
    }

    pub fn iso_display_level(self) -> f32 {
        match self {
            Self::Isosurface(parameters) => parameters.display_level,
            _ => DEFAULT_ISO_DISPLAY_LEVEL,
        }
    }

    pub fn dvr_opacity_transfer(self, default: DvrOpacityTransfer) -> DvrOpacityTransfer {
        match self {
            Self::Dvr(parameters) => parameters.opacity_transfer,
            _ => default,
        }
    }

    pub fn dvr_density_scale(self) -> f64 {
        match self {
            Self::Dvr(parameters) => parameters.density_scale,
            _ => DEFAULT_DVR_DENSITY_SCALE,
        }
    }
}

pub const DEFAULT_ISO_DISPLAY_LEVEL: f32 = 0.5;
pub const DEFAULT_DVR_DENSITY_SCALE: f64 = 12.0;

#[derive(Debug, Clone)]
pub struct RenderedIntensityChannel {
    pub layer_id: String,
    pub render_state: ChannelRenderState,
    pub transfer: ChannelTransferFunction,
    pub frame: MipImageU16,
    pub frame_f32: Option<MipImageF32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportHover {
    pub x: u64,
    pub y: u64,
    pub intensity: ViewportIntensity,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewportIntensity {
    U8(u8),
    U16(u16),
    F32(f32),
}

impl std::fmt::Display for ViewportIntensity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::U8(value) => write!(formatter, "{value}"),
            Self::U16(value) => write!(formatter, "{value}"),
            Self::F32(value) => write!(formatter, "{value:.6}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderMode {
    Mip,
    Isosurface,
    Dvr,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderSamplingPolicy {
    #[default]
    SmoothLinear,
    VoxelExact,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderIsoShadingPolicy {
    #[default]
    GradientLighting,
    Flat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderBackend {
    Loading,
    CpuReference,
    CpuResidentBricks,
    GpuResidentBricks,
    GpuCameraMip,
    GpuCameraIso,
    GpuCameraDvr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameCompleteness {
    Exact,
    Complete,
    Loading,
    Incomplete,
    BudgetLimited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayedFrameFreshness {
    Unknown,
    Current,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LodDecisionReason {
    ExactS0,
    ScreenEquivalentCoarserScale,
    PlaybackDownshift,
    LoadingTargetScale,
    FrameBudgetLimited,
    GpuBudgetLimited,
    CpuBudgetLimited,
    BackendLimit,
    AllocationFailed,
    IncompleteResidency,
    InvalidModeParameter,
    UnsupportedDtype,
    InvalidTransform,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameFailureKind {
    BudgetExceeded,
    BackendLimit,
    AllocationFailed,
    IncompleteResidency,
    InvalidModeParameter,
    UnsupportedDtype,
    InvalidTransform,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameFidelityStatus {
    pub target_scale_level: u32,
    pub displayed_scale_level: Option<u32>,
    pub completeness: FrameCompleteness,
    pub reason: LodDecisionReason,
    pub backend: RenderBackend,
    pub viewport: RenderViewport,
    pub presentation_viewport: PresentationViewport,
    pub display_freshness: DisplayedFrameFreshness,
    pub frame_time_ms: Option<f64>,
    pub visible_bricks: usize,
    pub resident_bricks: usize,
    pub missing_occupied_bricks: usize,
    pub cpu_cache_bytes: u64,
    pub gpu_resident_bytes: u64,
    pub upload_queue_depth: usize,
    pub last_failure_kind: Option<FrameFailureKind>,
    pub last_capacity_error: Option<String>,
}

impl FrameFidelityStatus {
    pub(crate) fn new_with_presentation(
        viewport: RenderViewport,
        presentation_viewport: PresentationViewport,
    ) -> Self {
        Self {
            target_scale_level: 0,
            displayed_scale_level: None,
            completeness: FrameCompleteness::Loading,
            reason: LodDecisionReason::LoadingTargetScale,
            backend: RenderBackend::CpuResidentBricks,
            viewport,
            presentation_viewport,
            display_freshness: DisplayedFrameFreshness::Unknown,
            frame_time_ms: None,
            visible_bricks: 0,
            resident_bricks: 0,
            missing_occupied_bricks: 0,
            cpu_cache_bytes: 0,
            gpu_resident_bytes: 0,
            upload_queue_depth: 0,
            last_failure_kind: None,
            last_capacity_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelFidelityWarning {
    Hidden,
    MixedFidelity,
    Incomplete,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChannelFidelityStatus {
    pub layer_id: String,
    pub layer_name: String,
    pub visible: bool,
    pub render_mode: RenderMode,
    pub displayed_scale_level: Option<u32>,
    pub target_scale_level: u32,
    pub completeness: FrameCompleteness,
    pub reason: LodDecisionReason,
    pub backend: RenderBackend,
    pub resident_bricks: usize,
    pub visible_bricks: usize,
    pub missing_occupied_bricks: usize,
    pub warning: Option<ChannelFidelityWarning>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayerHistogramSummary {
    pub status: HistogramStatus,
    pub bin_count: usize,
    pub sample_count: u64,
    pub min_value: f32,
    pub max_value: f32,
    pub bins: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistogramStatus {
    Exact,
    Sampled { source: String },
    Pending { reason: String },
    Unavailable { reason: String },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LodScheduleState {
    pub target_scale_level: u32,
    pub displayed_scale_level: Option<u32>,
    pub fallback_scale_level: Option<u32>,
    pub pending_scale_level: Option<u32>,
    pub hard_failed_scale_level: Option<u32>,
    pub hard_failure_reason: Option<LodDecisionReason>,
}

impl LodScheduleState {
    pub(crate) fn new(displayed_scale_level: Option<u32>) -> Self {
        Self {
            target_scale_level: displayed_scale_level.unwrap_or(0),
            displayed_scale_level,
            fallback_scale_level: None,
            pending_scale_level: displayed_scale_level,
            hard_failed_scale_level: None,
            hard_failure_reason: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LayerHistogramCacheKey {
    pub(crate) layer_id: String,
    pub(crate) dtype: IntensityDType,
    pub(crate) timepoint: TimeIndex,
    pub(crate) scale_level: u32,
    pub(crate) resident_generation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LayerHistogramCache {
    pub(crate) key: LayerHistogramCacheKey,
    pub(crate) summary: LayerHistogramSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ResidentHistogramSampleKey {
    pub(crate) layer_id: String,
    pub(crate) dtype: IntensityDType,
    pub(crate) timepoint: TimeIndex,
    pub(crate) scale_level: u32,
    pub(crate) brick_index: SpatialBrickIndex,
}
