use std::time::Instant;

use anyhow::Context;
#[cfg(test)]
use mirante4d_core::LayerId;
use mirante4d_core::{
    CameraView, ChannelTransferFunction, IntensityDType, LayerDisplay, PresentationViewport,
    Shape3D,
};
use mirante4d_data::{DenseVolumeF32, DenseVolumeU8, DenseVolumeU16};
use mirante4d_renderer::{
    CameraRenderMode, CameraRenderModeF32, CameraRenderQuality, DvrRenderParameters,
    FrameDiagnostics, FrameDiagnosticsF32, IsoSurfaceFrameF32, IsoSurfaceFrameU16,
    IsoSurfaceNormal, IsoSurfaceParameters, MipImageF32, MipImageU16, PixelCoverage, RenderError,
    RenderViewport, ScalarDisplayTransfer,
    gpu::{GpuRenderError, GpuRenderer},
    render_camera_f32_with_quality, render_camera_u8_with_quality, render_camera_with_quality,
};

use crate::{
    AppLayerSummary, AppState, ChannelFidelityStatus, ChannelFidelityWarning, ChannelRenderState,
    DvrOpacityTransfer, FrameCompleteness, FrameFailureKind, IntensitySummary, LodDecisionReason,
    RenderBackend, RenderMode, RenderedIntensityChannel, SMALL_DENSE_VIEWER_VOXEL_LIMIT,
    brick_streaming::current_resident_frame_ready,
    fidelity::visible_channel_fidelity_is_mixed,
    lod_scheduler::update_visible_brick_plan,
    resident_rendering::render_state_from_resident_bricks_with_backend,
    viewport::{camera_render_quality, resident_brick_render_supported},
};

pub(crate) fn dense_startup_allowed(shape: Shape3D) -> bool {
    shape
        .element_count()
        .is_ok_and(|voxels| voxels <= SMALL_DENSE_VIEWER_VOXEL_LIMIT)
}

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

pub(crate) fn metadata_intensity_summary(shape: Shape3D) -> anyhow::Result<IntensitySummary> {
    let voxel_count = shape.element_count()?;
    Ok(IntensitySummary {
        voxel_count,
        geometric_voxel_count: voxel_count,
        nonzero_count: 0,
        min: 0,
        max: 0,
        mean: 0.0,
    })
}

pub(crate) fn set_render_viewport(state: &mut AppState, viewport: RenderViewport) -> bool {
    if state.render_viewport == viewport {
        return false;
    }
    state.render_viewport = viewport;
    state.frame_fidelity.viewport = viewport;
    true
}

pub(crate) fn set_presentation_viewport(
    state: &mut AppState,
    viewport: PresentationViewport,
) -> bool {
    if state.presentation_viewport == viewport {
        return false;
    }
    state.presentation_viewport = viewport;
    state.frame_fidelity.presentation_viewport = viewport;
    true
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_app_frame(
    volume: &DenseVolumeU16,
    camera: CameraView,
    presentation_viewport: PresentationViewport,
    viewport: RenderViewport,
    mode: RenderMode,
    transfer: &ChannelTransferFunction,
    dvr_opacity_transfer: DvrOpacityTransfer,
    iso_display_level: f32,
    dvr_density_scale: f64,
    quality: CameraRenderQuality,
) -> anyhow::Result<(MipImageU16, FrameDiagnostics)> {
    Ok(render_camera_with_quality(
        volume,
        camera.to_camera_state(presentation_viewport),
        viewport,
        renderer_mode(
            mode,
            transfer,
            dvr_opacity_transfer,
            iso_display_level,
            dvr_density_scale,
        )?,
        quality,
    )?)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_u8_app_frame(
    volume: &DenseVolumeU8,
    camera: CameraView,
    presentation_viewport: PresentationViewport,
    viewport: RenderViewport,
    mode: RenderMode,
    transfer: &ChannelTransferFunction,
    dvr_opacity_transfer: DvrOpacityTransfer,
    iso_display_level: f32,
    dvr_density_scale: f64,
    quality: CameraRenderQuality,
) -> anyhow::Result<(MipImageU16, FrameDiagnostics)> {
    Ok(render_camera_u8_with_quality(
        volume,
        camera.to_camera_state(presentation_viewport),
        viewport,
        renderer_mode(
            mode,
            transfer,
            dvr_opacity_transfer,
            iso_display_level,
            dvr_density_scale,
        )?,
        quality,
    )?)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_f32_app_frame(
    volume: &DenseVolumeF32,
    camera: CameraView,
    presentation_viewport: PresentationViewport,
    viewport: RenderViewport,
    mode: RenderMode,
    transfer: &ChannelTransferFunction,
    display: LayerDisplay,
    dvr_opacity_transfer: DvrOpacityTransfer,
    iso_display_level: f32,
    dvr_density_scale: f64,
    quality: CameraRenderQuality,
) -> anyhow::Result<(MipImageF32, FrameDiagnosticsF32)> {
    Ok(render_camera_f32_with_quality(
        volume,
        camera.to_camera_state(presentation_viewport),
        viewport,
        renderer_mode_f32(
            mode,
            transfer,
            display,
            dvr_opacity_transfer,
            iso_display_level,
            dvr_density_scale,
        )?,
        quality,
    )?)
}

pub(crate) fn rerender_state_with_backend(
    state: &mut AppState,
    gpu_renderer: Option<&GpuRenderer>,
) -> anyhow::Result<()> {
    let render_start = Instant::now();
    update_visible_brick_plan(state);

    if current_resident_frame_ready(state)
        && resident_brick_render_supported(state.active_render_mode)
    {
        render_state_from_resident_bricks_with_backend(state, gpu_renderer)?;
        record_completed_frame_time(state, render_start);
        refresh_fidelity_resource_stats(state, gpu_renderer);
        return Ok(());
    }

    if let Some(volume_f32) = &state.active_volume_f32 {
        let (frame_f32, diagnostics_f32) = render_f32_app_frame(
            volume_f32,
            state.camera,
            state.presentation_viewport,
            state.render_viewport,
            state.active_render_mode,
            &state.active_layer_transfer,
            state.active_layer_display,
            state.active_dvr_opacity_transfer,
            state.iso_display_level,
            state.dvr_density_scale,
            camera_render_quality(state),
        )?;
        state.frame = f32_frame_to_display_u16_for_mode(
            &frame_f32,
            state.active_render_mode,
            state.active_layer_display,
        )?;
        state.diagnostics = mirante4d_renderer::frame_diagnostics(
            volume_f32.shape.element_count()?,
            state.frame.pixels(),
        );
        state.frame_f32 = Some(frame_f32);
        state.diagnostics_f32 = Some(diagnostics_f32);
        state.render_backend = RenderBackend::CpuReference;
        state.frame_fidelity.displayed_scale_level = Some(volume_f32.scale_level);
        state.lod_schedule.displayed_scale_level = Some(volume_f32.scale_level);
        state.lod_schedule.pending_scale_level = None;
        state.frame_fidelity.target_scale_level = state.lod_schedule.target_scale_level;
        state.frame_fidelity.completeness = FrameCompleteness::Exact;
        state.frame_fidelity.reason = if volume_f32.scale_level == 0 {
            LodDecisionReason::ExactS0
        } else {
            LodDecisionReason::ScreenEquivalentCoarserScale
        };
        state.frame_fidelity.backend = state.render_backend;
        record_completed_frame_time(state, render_start);
        set_single_rendered_channel(state);
        update_visible_brick_plan(state);
        refresh_fidelity_resource_stats(state, gpu_renderer);
        return Ok(());
    }

    if let Some(volume_u8) = &state.active_volume_u8 {
        let (frame, diagnostics) = render_u8_app_frame(
            volume_u8,
            state.camera,
            state.presentation_viewport,
            state.render_viewport,
            state.active_render_mode,
            &state.active_layer_transfer,
            state.active_dvr_opacity_transfer,
            state.iso_display_level,
            state.dvr_density_scale,
            camera_render_quality(state),
        )?;
        state.frame = frame;
        state.diagnostics = diagnostics;
        state.frame_f32 = None;
        state.diagnostics_f32 = None;
        state.render_backend = RenderBackend::CpuReference;
        state.frame_fidelity.displayed_scale_level = Some(volume_u8.scale_level);
        state.lod_schedule.displayed_scale_level = Some(volume_u8.scale_level);
        state.lod_schedule.pending_scale_level = None;
        state.frame_fidelity.target_scale_level = state.lod_schedule.target_scale_level;
        state.frame_fidelity.completeness = frame_completeness_for_rendered_scale(
            volume_u8.scale_level,
            state.frame_fidelity.target_scale_level,
            state.frame_fidelity.reason,
        );
        state.frame_fidelity.reason = if volume_u8.scale_level == 0 {
            LodDecisionReason::ExactS0
        } else {
            LodDecisionReason::ScreenEquivalentCoarserScale
        };
        state.frame_fidelity.backend = state.render_backend;
        record_completed_frame_time(state, render_start);
        set_single_rendered_channel(state);
        refresh_fidelity_resource_stats(state, gpu_renderer);
        return Ok(());
    }

    let Some(active_volume) = state.active_volume.as_ref() else {
        set_loading_frame_for_current_viewport(state);
        set_single_rendered_channel(state);
        state.render_backend = RenderBackend::CpuResidentBricks;
        state.frame_fidelity.backend = state.render_backend;
        state.frame_fidelity.displayed_scale_level = state.lod_schedule.displayed_scale_level;
        if state.frame_fidelity.completeness != FrameCompleteness::BudgetLimited {
            state.frame_fidelity.completeness = FrameCompleteness::Loading;
            state.frame_fidelity.reason = LodDecisionReason::LoadingTargetScale;
        }
        state.frame_fidelity.frame_time_ms = None;
        refresh_fidelity_resource_stats(state, gpu_renderer);
        return Ok(());
    };

    let (frame, diagnostics, backend) = render_app_frame_with_backend(
        active_volume,
        state.camera,
        state.presentation_viewport,
        state.render_viewport,
        state.active_render_mode,
        &state.active_layer_transfer,
        state.active_dvr_opacity_transfer,
        state.iso_display_level,
        state.dvr_density_scale,
        camera_render_quality(state),
        gpu_renderer,
    )?;
    state.frame = frame;
    state.diagnostics = diagnostics;
    state.render_backend = backend;
    state.frame_fidelity.displayed_scale_level = Some(active_volume.scale_level);
    state.lod_schedule.displayed_scale_level = Some(active_volume.scale_level);
    state.lod_schedule.pending_scale_level = None;
    state.frame_fidelity.target_scale_level = state.lod_schedule.target_scale_level;
    state.frame_fidelity.completeness = frame_completeness_for_rendered_scale(
        active_volume.scale_level,
        state.frame_fidelity.target_scale_level,
        state.frame_fidelity.reason,
    );
    state.frame_fidelity.reason = if active_volume.scale_level == 0 {
        LodDecisionReason::ExactS0
    } else {
        LodDecisionReason::ScreenEquivalentCoarserScale
    };
    state.frame_fidelity.backend = backend;
    record_completed_frame_time(state, render_start);
    set_single_rendered_channel(state);
    refresh_fidelity_resource_stats(state, gpu_renderer);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_app_frame_with_backend(
    volume: &DenseVolumeU16,
    camera: CameraView,
    presentation_viewport: PresentationViewport,
    viewport: RenderViewport,
    mode: RenderMode,
    transfer: &ChannelTransferFunction,
    dvr_opacity_transfer: DvrOpacityTransfer,
    iso_display_level: f32,
    dvr_density_scale: f64,
    quality: CameraRenderQuality,
    gpu_renderer: Option<&GpuRenderer>,
) -> anyhow::Result<(MipImageU16, FrameDiagnostics, RenderBackend)> {
    if let Some(gpu_renderer) = gpu_renderer {
        let camera_mode = renderer_mode(
            mode,
            transfer,
            dvr_opacity_transfer,
            iso_display_level,
            dvr_density_scale,
        )?;
        let backend = match mode {
            RenderMode::Mip => Some(RenderBackend::GpuCameraMip),
            RenderMode::Isosurface => Some(RenderBackend::GpuCameraIso),
            RenderMode::Dvr => Some(RenderBackend::GpuCameraDvr),
        };
        if let Some(backend) = backend {
            match gpu_renderer.render_camera_with_quality(
                volume,
                camera.to_camera_state(presentation_viewport),
                viewport,
                camera_mode,
                quality,
            ) {
                Ok(output) => return Ok((output.image, output.frame, backend)),
                Err(err) => {
                    if !dense_startup_allowed(volume.shape) {
                        return Err(err).context(
                            "dense GPU rendering failed and dense CPU fallback is disabled for large interactive volumes",
                        );
                    }
                }
            }
        }
    }

    if !dense_startup_allowed(volume.shape) {
        anyhow::bail!("dense CPU rendering is disabled for large interactive volumes");
    }
    let (frame, diagnostics) = render_app_frame(
        volume,
        camera,
        presentation_viewport,
        viewport,
        mode,
        transfer,
        dvr_opacity_transfer,
        iso_display_level,
        dvr_density_scale,
        quality,
    )?;
    Ok((frame, diagnostics, RenderBackend::CpuReference))
}

pub(crate) fn set_single_rendered_channel(state: &mut AppState) {
    state.rendered_channels = vec![RenderedIntensityChannel {
        layer_id: state.active_layer_id.clone(),
        render_state: ChannelRenderState::for_mode(
            state.active_render_mode,
            state.render_sampling_policy,
            state.render_iso_shading_policy,
            state.iso_display_level,
            state.active_dvr_opacity_transfer,
            state.dvr_density_scale,
        ),
        transfer: state.active_layer_transfer.clone(),
        frame: state.frame.clone(),
        frame_f32: state.frame_f32.clone(),
    }];
    update_channel_fidelity_status(state);
}

pub(crate) fn set_loading_frame_for_current_viewport(state: &mut AppState) {
    if state.frame.width != state.render_viewport.width
        || state.frame.height != state.render_viewport.height
        || (state.active_render_mode == RenderMode::Isosurface
            && state.frame.iso_surface().is_none())
    {
        state.frame = placeholder_frame_for_mode(state.render_viewport, state.active_render_mode);
    }
    state.frame_f32 = None;
    state.diagnostics = mirante4d_renderer::frame_diagnostics(
        state.active_source_shape.element_count().unwrap_or(0),
        state.frame.pixels(),
    );
    state.diagnostics_f32 = None;
}

pub(crate) fn refresh_fidelity_resource_stats(
    state: &mut AppState,
    gpu_renderer: Option<&GpuRenderer>,
) {
    state.frame_fidelity.viewport = state.render_viewport;
    state.frame_fidelity.visible_bricks = state.visible_brick_count;
    state.frame_fidelity.resident_bricks = state.resident_bricks_u8.len()
        + state.resident_bricks.len()
        + state.resident_bricks_f32.len();
    state.frame_fidelity.missing_occupied_bricks = state
        .brick_stream_requested
        .saturating_sub(state.brick_stream_completed);
    if let Ok(diagnostics) = state.dataset.diagnostics() {
        state.frame_fidelity.cpu_cache_bytes = diagnostics.stats.brick_cache_bytes;
    }
    if let Some(renderer) = gpu_renderer
        && let Ok(stats) = renderer.stats()
    {
        state.frame_fidelity.gpu_resident_bytes = stats.brick_atlas_resident_bytes;
    }
    update_channel_fidelity_status(state);
}

pub(crate) fn update_channel_fidelity_status(state: &mut AppState) {
    let visible_status = state.frame_fidelity.clone();
    state.channel_fidelity = state
        .layers
        .iter()
        .map(|layer| {
            let visible = layer.display.visible;
            let resident_bricks = if visible {
                resident_brick_count_for_layer(state, layer)
            } else {
                0
            };
            let visible_bricks = if visible {
                state.visible_brick_count
            } else {
                0
            };
            let missing = if visible {
                state
                    .brick_stream_requested
                    .saturating_sub(state.brick_stream_completed)
            } else {
                0
            };
            let warning = if !visible {
                Some(ChannelFidelityWarning::Hidden)
            } else if visible_status.completeness == FrameCompleteness::Incomplete {
                Some(ChannelFidelityWarning::Incomplete)
            } else {
                None
            };
            ChannelFidelityStatus {
                layer_id: layer.id.clone(),
                layer_name: layer.name.clone(),
                visible,
                render_mode: if layer.id == state.active_layer_id {
                    state.active_render_mode
                } else {
                    layer.render_state.mode()
                },
                displayed_scale_level: visible_status.displayed_scale_level,
                target_scale_level: visible_status.target_scale_level,
                completeness: if visible {
                    visible_status.completeness
                } else {
                    FrameCompleteness::Complete
                },
                reason: visible_status.reason,
                backend: visible_status.backend,
                resident_bricks,
                visible_bricks,
                missing_occupied_bricks: missing,
                warning,
            }
        })
        .collect();

    if visible_channel_fidelity_is_mixed(&state.channel_fidelity) {
        for channel in &mut state.channel_fidelity {
            if channel.visible {
                channel.warning = Some(ChannelFidelityWarning::MixedFidelity);
            }
        }
    }
}

fn resident_brick_count_for_layer(state: &AppState, layer: &AppLayerSummary) -> usize {
    match layer.dtype {
        IntensityDType::Float32 => state
            .resident_bricks_f32_by_layer
            .get(&layer.id)
            .map(Vec::len)
            .unwrap_or_else(|| {
                if layer.id == state.active_layer_id {
                    state.resident_bricks_f32.len()
                } else {
                    0
                }
            }),
        IntensityDType::Uint8 => state
            .resident_bricks_u8_by_layer
            .get(&layer.id)
            .map(Vec::len)
            .unwrap_or_else(|| {
                if layer.id == state.active_layer_id {
                    state.resident_bricks_u8.len()
                } else {
                    0
                }
            }),
        IntensityDType::Uint16 => state
            .resident_bricks_by_layer
            .get(&layer.id)
            .map(Vec::len)
            .unwrap_or_else(|| {
                if layer.id == state.active_layer_id {
                    state.resident_bricks.len()
                } else {
                    0
                }
            }),
    }
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

pub(crate) fn record_completed_frame_time(state: &mut AppState, render_start: Instant) {
    #[cfg(test)]
    let frame_time_ms = state
        .fixed_frame_time_ms_for_snapshots
        .unwrap_or_else(|| render_start.elapsed().as_secs_f64() * 1000.0);
    #[cfg(not(test))]
    let frame_time_ms = render_start.elapsed().as_secs_f64() * 1000.0;
    state.frame_fidelity.frame_time_ms = Some(frame_time_ms);
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
        | RenderError::ResourceIdentityMismatch(_)
        | RenderError::InvalidResourceId { .. } => FrameFailureKind::InvalidModeParameter,
        RenderError::DimensionTooLarge { .. } => FrameFailureKind::BackendLimit,
        RenderError::EmptyVolume | RenderError::Shape(_) | RenderError::Space(_) => {
            FrameFailureKind::InvalidTransform
        }
    }
}

#[cfg(test)]
pub(crate) fn fidelity_state_for_resident_render_failure(
    kind: FrameFailureKind,
) -> (FrameCompleteness, LodDecisionReason) {
    match kind {
        FrameFailureKind::BudgetExceeded => (
            FrameCompleteness::BudgetLimited,
            LodDecisionReason::GpuBudgetLimited,
        ),
        FrameFailureKind::BackendLimit => (
            FrameCompleteness::BudgetLimited,
            LodDecisionReason::BackendLimit,
        ),
        FrameFailureKind::AllocationFailed => (
            FrameCompleteness::BudgetLimited,
            LodDecisionReason::AllocationFailed,
        ),
        FrameFailureKind::IncompleteResidency => (
            FrameCompleteness::Incomplete,
            LodDecisionReason::IncompleteResidency,
        ),
        FrameFailureKind::InvalidModeParameter => (
            FrameCompleteness::Incomplete,
            LodDecisionReason::InvalidModeParameter,
        ),
        FrameFailureKind::UnsupportedDtype => (
            FrameCompleteness::Incomplete,
            LodDecisionReason::UnsupportedDtype,
        ),
        FrameFailureKind::InvalidTransform => (
            FrameCompleteness::Incomplete,
            LodDecisionReason::InvalidTransform,
        ),
    }
}

#[cfg(test)]
fn resident_render_failure_can_trigger_lod_downgrade(kind: FrameFailureKind) -> bool {
    matches!(
        kind,
        FrameFailureKind::BudgetExceeded
            | FrameFailureKind::BackendLimit
            | FrameFailureKind::AllocationFailed
    )
}

#[cfg(test)]
pub(crate) fn request_lod_downgrade_after_resident_capacity_failure(
    state: &mut AppState,
    failure: ResidentRenderFailureStatus,
) -> bool {
    state.frame_fidelity.last_capacity_error = Some(failure.message.clone());
    state.frame_fidelity.last_failure_kind = Some(failure.kind);
    let (completeness, reason) = fidelity_state_for_resident_render_failure(failure.kind);
    state.frame_fidelity.completeness = completeness;
    state.frame_fidelity.reason = reason;
    state.brick_stream_last_error = Some(failure.message);

    if !resident_render_failure_can_trigger_lod_downgrade(failure.kind) {
        return false;
    }

    let Ok(layer_id) = LayerId::new(state.active_layer_id.clone()) else {
        return false;
    };
    let Ok(scale_count) = state.dataset.scale_count(&layer_id) else {
        return false;
    };
    if state.brick_stream_scale_level as usize + 1 >= scale_count {
        return false;
    }

    state.lod_schedule.hard_failed_scale_level = Some(state.brick_stream_scale_level);
    state.lod_schedule.hard_failure_reason = Some(reason);
    state.lod_replan_pending = true;
    true
}

pub(crate) fn take_lod_replan_pending(state: &mut AppState) -> bool {
    let pending = state.lod_replan_pending;
    state.lod_replan_pending = false;
    pending
}

pub(crate) fn renderer_mode(
    mode: RenderMode,
    transfer: &ChannelTransferFunction,
    dvr_opacity_transfer: DvrOpacityTransfer,
    iso_display_level: f32,
    dvr_density_scale: f64,
) -> anyhow::Result<CameraRenderMode> {
    Ok(match mode {
        RenderMode::Mip => CameraRenderMode::Mip,
        RenderMode::Isosurface => CameraRenderMode::Isosurface {
            parameters: iso_surface_parameters(transfer, iso_display_level),
        },
        RenderMode::Dvr => CameraRenderMode::Dvr {
            parameters: dvr_render_parameters(transfer, dvr_opacity_transfer, dvr_density_scale),
        },
    })
}

pub(crate) fn renderer_mode_f32(
    mode: RenderMode,
    transfer: &ChannelTransferFunction,
    _display: LayerDisplay,
    dvr_opacity_transfer: DvrOpacityTransfer,
    iso_display_level: f32,
    dvr_density_scale: f64,
) -> anyhow::Result<CameraRenderModeF32> {
    Ok(match mode {
        RenderMode::Mip => CameraRenderModeF32::Mip,
        RenderMode::Isosurface => CameraRenderModeF32::Isosurface {
            parameters: iso_surface_parameters(transfer, iso_display_level),
        },
        RenderMode::Dvr => CameraRenderModeF32::Dvr {
            parameters: dvr_render_parameters(transfer, dvr_opacity_transfer, dvr_density_scale),
        },
    })
}

pub(crate) fn iso_surface_parameters(
    transfer: &ChannelTransferFunction,
    iso_display_level: f32,
) -> IsoSurfaceParameters {
    IsoSurfaceParameters::new(
        iso_display_level,
        ScalarDisplayTransfer::from_transfer_function(transfer),
    )
}

pub(crate) fn dvr_render_parameters(
    transfer: &ChannelTransferFunction,
    dvr_opacity_transfer: DvrOpacityTransfer,
    dvr_density_scale: f64,
) -> DvrRenderParameters {
    DvrRenderParameters::new(
        ScalarDisplayTransfer::from_transfer_function(transfer),
        ScalarDisplayTransfer::new(
            dvr_opacity_transfer.window,
            dvr_opacity_transfer.curve,
            false,
        ),
        transfer.color.color_rgba,
        transfer.display.opacity,
        dvr_density_scale,
    )
}

pub(crate) fn f32_values_to_display_u16(values: &[f32], display: LayerDisplay) -> Vec<u16> {
    values
        .iter()
        .map(|value| f32_value_to_display_u16(*value, display))
        .collect()
}

pub(crate) fn f32_frame_to_display_u16(image: &MipImageF32, display: LayerDisplay) -> MipImageU16 {
    MipImageU16::try_new_with_mode_frames(
        image.width,
        image.height,
        f32_values_to_display_u16(image.pixels(), display),
        image.coverage().clone(),
        None,
        image.dvr_rgba().cloned(),
    )
    .expect("display-converted f32 frame preserves validated source coverage")
}

pub(crate) fn f32_dvr_frame_to_display_u16(image: &MipImageF32) -> MipImageU16 {
    MipImageU16::try_new_with_mode_frames(
        image.width,
        image.height,
        image
            .pixels()
            .iter()
            .map(|value| normalized_f32_to_u16(*value))
            .collect(),
        image.coverage().clone(),
        None,
        image.dvr_rgba().cloned(),
    )
    .expect("display-converted f32 DVR frame preserves validated DVR payload")
}

pub(crate) fn f32_iso_frame_to_display_u16(
    image: &MipImageF32,
    display: LayerDisplay,
) -> MipImageU16 {
    let iso_surface = image
        .iso_surface()
        .map(|surface| f32_iso_surface_to_display_u16_surface(surface, display));
    MipImageU16::try_new_with_mode_frames(
        image.width,
        image.height,
        image
            .pixels()
            .iter()
            .map(|value| normalized_f32_to_u16(*value))
            .collect(),
        image.coverage().clone(),
        iso_surface,
        None,
    )
    .expect("display-converted f32 ISO frame preserves validated source coverage")
}

fn f32_iso_surface_to_display_u16_surface(
    surface: &IsoSurfaceFrameF32,
    display: LayerDisplay,
) -> IsoSurfaceFrameU16 {
    IsoSurfaceFrameU16::try_new(
        surface.width,
        surface.height,
        f32_values_to_display_u16(surface.source_values(), display),
        surface
            .display_scalars()
            .iter()
            .map(|value| normalized_f32_to_u16(*value))
            .collect(),
        surface
            .material_scalars()
            .iter()
            .map(|value| normalized_f32_to_u16(*value))
            .collect(),
        surface.hit_depth().to_vec(),
        surface.normals().to_vec(),
        surface.diffuse_lighting().to_vec(),
        surface.specular_lighting().to_vec(),
        surface.coverage().clone(),
    )
    .expect("display-converted f32 ISO surface preserves validated source dimensions")
}

pub(crate) fn f32_frame_to_display_u16_for_mode(
    image: &MipImageF32,
    mode: RenderMode,
    display: LayerDisplay,
) -> anyhow::Result<MipImageU16> {
    Ok(match mode {
        RenderMode::Isosurface => f32_iso_frame_to_display_u16(image, display),
        RenderMode::Dvr => f32_dvr_frame_to_display_u16(image),
        RenderMode::Mip => f32_frame_to_display_u16(image, display),
    })
}

fn f32_value_to_display_u16(value: f32, display: LayerDisplay) -> u16 {
    if !value.is_finite() {
        return 0;
    }
    let low = display.window.low;
    let high = display.window.high;
    let normalized = if high > low {
        ((value - low) / (high - low)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    normalized_f32_to_u16(normalized)
}

fn normalized_f32_to_u16(value: f32) -> u16 {
    if !value.is_finite() {
        return 0;
    }
    (value.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16
}
