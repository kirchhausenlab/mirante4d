use std::time::Instant;

use mirante4d_domain::{DvrOpacityTransfer, RenderMode, RenderState};
use mirante4d_render_api::PresentationViewport;
use mirante4d_renderer::{
    CameraRenderMode, CameraRenderModeF32, DvrRenderParameters, IntensityTransfer,
    IsoSurfaceFrameU16, IsoSurfaceNormal, IsoSurfaceParameters, MipImageU16, PixelCoverage,
    RenderError, RenderViewport, ScalarDisplayTransfer, gpu::GpuRenderError,
};

use crate::{
    FrameCompleteness, FrameFailureKind, LodDecisionReason,
    current_runtime::render::CurrentRenderRuntime,
};

pub(crate) fn placeholder_frame(viewport: RenderViewport) -> MipImageU16 {
    let pixel_count = (viewport.width * viewport.height) as usize;
    MipImageU16::with_coverage(
        viewport.width,
        viewport.height,
        vec![0; pixel_count],
        vec![0; pixel_count],
    )
    .expect("placeholder frame dimensions are internally consistent")
}

pub(crate) fn placeholder_frame_for_mode(
    viewport: RenderViewport,
    mode: RenderMode,
) -> MipImageU16 {
    if mode == RenderMode::Isosurface {
        placeholder_iso_frame(viewport)
    } else {
        placeholder_frame(viewport)
    }
}

pub(crate) fn placeholder_iso_frame(viewport: RenderViewport) -> MipImageU16 {
    let pixel_count = (viewport.width * viewport.height) as usize;
    MipImageU16::try_new_with_iso_surface(
        viewport.width,
        viewport.height,
        vec![0; pixel_count],
        PixelCoverage::Mask(vec![0; pixel_count]),
        Some(empty_iso_surface_frame(viewport)),
    )
    .expect("placeholder ISO frame dimensions are internally consistent")
}

pub(crate) fn empty_iso_surface_frame(viewport: RenderViewport) -> IsoSurfaceFrameU16 {
    let pixel_count = (viewport.width * viewport.height) as usize;
    IsoSurfaceFrameU16::try_new(
        viewport.width,
        viewport.height,
        vec![0; pixel_count],
        vec![0; pixel_count],
        vec![0; pixel_count],
        vec![0.0; pixel_count],
        vec![IsoSurfaceNormal::ZERO; pixel_count],
        vec![u16::MAX; pixel_count],
        vec![0; pixel_count],
        PixelCoverage::Mask(vec![0; pixel_count]),
    )
    .expect("placeholder ISO surface dimensions are internally consistent")
}

pub(crate) fn set_render_viewport(
    render: &mut CurrentRenderRuntime,
    viewport: RenderViewport,
) -> bool {
    if render.render_viewport == viewport {
        return false;
    }
    render.render_viewport = viewport;
    render.frame_fidelity.viewport = viewport;
    true
}

pub(crate) fn set_presentation_viewport(
    render: &mut CurrentRenderRuntime,
    viewport: PresentationViewport,
) -> bool {
    if render.presentation_viewport == viewport {
        return false;
    }
    render.presentation_viewport = viewport;
    render.frame_fidelity.presentation_viewport = viewport;
    true
}

pub(crate) fn frame_completeness_for_rendered_scale(
    displayed_scale_level: u32,
    target_scale_level: u32,
    reason: LodDecisionReason,
) -> FrameCompleteness {
    if displayed_scale_level == target_scale_level {
        if displayed_scale_level == 0 {
            FrameCompleteness::Exact
        } else {
            FrameCompleteness::Complete
        }
    } else if matches!(
        reason,
        LodDecisionReason::GpuBudgetLimited
            | LodDecisionReason::CpuBudgetLimited
            | LodDecisionReason::FrameBudgetLimited
            | LodDecisionReason::BackendLimit
            | LodDecisionReason::AllocationFailed
    ) {
        FrameCompleteness::BudgetLimited
    } else {
        FrameCompleteness::Loading
    }
}

pub(crate) fn record_completed_frame_time(
    render: &mut CurrentRenderRuntime,
    render_start: Instant,
) {
    let frame_time_ms = render_start.elapsed().as_secs_f64() * 1000.0;
    render.frame_fidelity.frame_time_ms = Some(frame_time_ms);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResidentRenderFailureStatus {
    pub(crate) kind: FrameFailureKind,
    pub(crate) message: String,
}

impl ResidentRenderFailureStatus {
    pub(crate) fn new(kind: FrameFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

#[derive(Debug)]
struct ResidentRenderFailure {
    status: ResidentRenderFailureStatus,
}

impl ResidentRenderFailure {
    fn new(kind: FrameFailureKind, message: impl Into<String>) -> Self {
        Self {
            status: ResidentRenderFailureStatus::new(kind, message),
        }
    }
}

impl std::fmt::Display for ResidentRenderFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.status.message)
    }
}

impl std::error::Error for ResidentRenderFailure {}

pub(crate) fn resident_render_failure_error(
    kind: FrameFailureKind,
    message: impl Into<String>,
) -> anyhow::Error {
    ResidentRenderFailure::new(kind, message).into()
}

pub(crate) fn resident_render_failure_from_gpu_error(err: GpuRenderError) -> anyhow::Error {
    let kind = frame_failure_kind_for_gpu_error(&err);
    resident_render_failure_error(kind, err.to_string())
}

pub(crate) fn frame_failure_kind_for_gpu_error(err: &GpuRenderError) -> FrameFailureKind {
    match err {
        GpuRenderError::Render(render) => frame_failure_kind_for_render_error(render),
        GpuRenderError::CpuLedger(mirante4d_dataset::CpuLedgerError::CapacityExceeded {
            ..
        }) => FrameFailureKind::BudgetExceeded,
        GpuRenderError::CpuLedger(
            mirante4d_dataset::CpuLedgerError::ZeroByteReservation
            | mirante4d_dataset::CpuLedgerError::ShuttingDown,
        ) => FrameFailureKind::AllocationFailed,
        GpuRenderError::AdapterUnavailable(_)
        | GpuRenderError::CpuAdapterOnly(_)
        | GpuRenderError::RequiredLimitUnsupported { .. }
        | GpuRenderError::DeviceLimitTooLow { .. }
        | GpuRenderError::RequestDevice(_)
        | GpuRenderError::BufferTooLarge { .. }
        | GpuRenderError::BufferSizeOverflow { .. } => FrameFailureKind::BackendLimit,
        GpuRenderError::UnsupportedCameraMode(_) => FrameFailureKind::InvalidModeParameter,
        GpuRenderError::BudgetExceeded { .. } => FrameFailureKind::BudgetExceeded,
        GpuRenderError::MapFailed(_)
        | GpuRenderError::PollFailed(_)
        | GpuRenderError::ReadbackChannelClosed
        | GpuRenderError::CachePoisoned => FrameFailureKind::AllocationFailed,
    }
}

pub(crate) fn frame_failure_kind_for_render_error(err: &RenderError) -> FrameFailureKind {
    match err {
        RenderError::ResourcePlanCapacityExceeded { .. } => FrameFailureKind::BudgetExceeded,
        RenderError::InvalidViewport { .. }
        | RenderError::InvalidReadoutPixel { .. }
        | RenderError::InvalidBrickPixelStride
        | RenderError::InvalidBrickAtlas(_)
        | RenderError::InvalidRgbaImageBuffer { .. }
        | RenderError::InvalidIntensityFrameBuffer { .. }
        | RenderError::InvalidPixelCoverageBuffer { .. }
        | RenderError::InvalidPixelCoverageValue { .. }
        | RenderError::InvalidPixelLightingBuffer { .. }
        | RenderError::InvalidIsoSurfaceFrameBuffer { .. }
        | RenderError::InvalidIsoSurfaceFrameDimensions { .. }
        | RenderError::InvalidDvrRgbaFrameBuffer { .. }
        | RenderError::InvalidDvrRgbaFrameDimensions { .. }
        | RenderError::InvalidDvrChannelSet(_)
        | RenderError::InvalidChannelComposite(_)
        | RenderError::InvalidIntensitySummaryRegion(_)
        | RenderError::ResourceContract(_)
        | RenderError::ResourceIdentityMismatch(_)
        | RenderError::InvalidResourceId { .. } => FrameFailureKind::InvalidModeParameter,
        RenderError::DimensionTooLarge { .. } => FrameFailureKind::BackendLimit,
        RenderError::EmptyVolume
        | RenderError::Shape(_)
        | RenderError::Space(_)
        | RenderError::Camera(_) => FrameFailureKind::InvalidTransform,
    }
}

pub(crate) fn render_failure_status(error: &anyhow::Error) -> ResidentRenderFailureStatus {
    if let Some(failure) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ResidentRenderFailure>())
    {
        return failure.status.clone();
    }
    let kind = error
        .chain()
        .find_map(|cause| {
            cause
                .downcast_ref::<GpuRenderError>()
                .map(frame_failure_kind_for_gpu_error)
                .or_else(|| {
                    cause
                        .downcast_ref::<RenderError>()
                        .map(frame_failure_kind_for_render_error)
                })
        })
        .unwrap_or(FrameFailureKind::InvalidModeParameter);
    ResidentRenderFailureStatus::new(kind, format!("{error:#}"))
}

pub(crate) fn take_lod_replan_pending(render: &mut CurrentRenderRuntime) -> bool {
    let pending = render.lod_replan_pending;
    render.lod_replan_pending = false;
    pending
}

pub(crate) fn renderer_mode(
    render_state: RenderState,
    transfer: &IntensityTransfer,
) -> anyhow::Result<CameraRenderMode> {
    Ok(match render_state.mode() {
        RenderMode::Mip => CameraRenderMode::Mip,
        RenderMode::Isosurface => CameraRenderMode::Isosurface {
            parameters: iso_surface_parameters(
                transfer,
                render_state
                    .iso_parameters()
                    .expect("ISO mode has ISO parameters")
                    .display_level(),
            ),
        },
        RenderMode::Dvr => CameraRenderMode::Dvr {
            parameters: {
                let parameters = render_state
                    .dvr_parameters()
                    .expect("DVR mode has DVR parameters");
                dvr_render_parameters(
                    transfer,
                    parameters.opacity_transfer(),
                    parameters.density_scale(),
                )
            },
        },
    })
}

pub(crate) fn renderer_mode_f32(
    render_state: RenderState,
    transfer: &IntensityTransfer,
) -> anyhow::Result<CameraRenderModeF32> {
    Ok(match render_state.mode() {
        RenderMode::Mip => CameraRenderModeF32::Mip,
        RenderMode::Isosurface => CameraRenderModeF32::Isosurface {
            parameters: iso_surface_parameters(
                transfer,
                render_state
                    .iso_parameters()
                    .expect("ISO mode has ISO parameters")
                    .display_level(),
            ),
        },
        RenderMode::Dvr => CameraRenderModeF32::Dvr {
            parameters: {
                let parameters = render_state
                    .dvr_parameters()
                    .expect("DVR mode has DVR parameters");
                dvr_render_parameters(
                    transfer,
                    parameters.opacity_transfer(),
                    parameters.density_scale(),
                )
            },
        },
    })
}

pub(crate) fn iso_surface_parameters(
    transfer: &IntensityTransfer,
    iso_display_level: f32,
) -> IsoSurfaceParameters {
    IsoSurfaceParameters::new(
        iso_display_level,
        ScalarDisplayTransfer::from_intensity_transfer(*transfer),
    )
}

pub(crate) fn dvr_render_parameters(
    transfer: &IntensityTransfer,
    dvr_opacity_transfer: DvrOpacityTransfer,
    dvr_density_scale: f64,
) -> DvrRenderParameters {
    DvrRenderParameters::new(
        ScalarDisplayTransfer::from_intensity_transfer(*transfer),
        ScalarDisplayTransfer::new(
            dvr_opacity_transfer.window(),
            dvr_opacity_transfer.curve(),
            false,
        ),
        transfer.color_rgba(),
        transfer.opacity().get(),
        dvr_density_scale,
    )
}
