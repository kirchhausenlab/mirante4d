use std::time::Instant;

use anyhow::Context;
use mirante4d_application::ApplicationSnapshot;
use mirante4d_data::{DenseVolumeF32, DenseVolumeU8, DenseVolumeU16};
use mirante4d_domain::{
    CameraView, DisplayWindow, DvrOpacityTransfer, IntensityDType, RenderMode, RenderState, Shape3D,
};
use mirante4d_format::LayerId;
use mirante4d_render_api::{CameraFrame, PresentationViewport};
use mirante4d_renderer::{
    CameraRenderMode, CameraRenderModeF32, CameraRenderQuality, DvrRenderParameters,
    FrameDiagnostics, FrameDiagnosticsF32, IntensityTransfer, IsoSurfaceFrameF32,
    IsoSurfaceFrameU16, IsoSurfaceNormal, IsoSurfaceParameters, MipImageF32, MipImageU16,
    PixelCoverage, RenderError, RenderViewport, ScalarDisplayTransfer,
    gpu::{GpuRenderError, GpuRenderer},
    render_camera_f32_with_quality, render_camera_u8_with_quality, render_camera_with_quality,
};

use crate::{
    ChannelFidelityStatus, ChannelFidelityWarning, FrameCompleteness, FrameFailureKind,
    IntensitySummary, LodDecisionReason, RenderBackend, RenderedIntensityChannel,
    SMALL_DENSE_VIEWER_VOXEL_LIMIT,
    brick_streaming::{current_resident_frame_ready, physical_layer_id_for_key, view_for_snapshot},
    current_runtime::{
        analysis::CurrentAnalysisRuntime, dataset::CurrentDatasetRuntime,
        render::CurrentRenderRuntime, ui::CurrentUiRuntime,
    },
    fidelity::visible_channel_fidelity_is_mixed,
    lod_scheduler::update_visible_brick_plan,
    resident_rendering::render_state_from_resident_bricks_with_backend,
    viewport::{camera_render_quality_for_render_state, resident_brick_render_supported},
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_app_frame(
    volume: &DenseVolumeU16,
    camera: CameraView,
    presentation_viewport: PresentationViewport,
    viewport: RenderViewport,
    render_state: RenderState,
    transfer: &IntensityTransfer,
    quality: CameraRenderQuality,
) -> anyhow::Result<(MipImageU16, FrameDiagnostics)> {
    Ok(render_camera_with_quality(
        volume,
        CameraFrame::new(camera, presentation_viewport)?,
        viewport,
        renderer_mode(render_state, transfer)?,
        quality,
    )?)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_u8_app_frame(
    volume: &DenseVolumeU8,
    camera: CameraView,
    presentation_viewport: PresentationViewport,
    viewport: RenderViewport,
    render_state: RenderState,
    transfer: &IntensityTransfer,
    quality: CameraRenderQuality,
) -> anyhow::Result<(MipImageU16, FrameDiagnostics)> {
    Ok(render_camera_u8_with_quality(
        volume,
        CameraFrame::new(camera, presentation_viewport)?,
        viewport,
        renderer_mode(render_state, transfer)?,
        quality,
    )?)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_f32_app_frame(
    volume: &DenseVolumeF32,
    camera: CameraView,
    presentation_viewport: PresentationViewport,
    viewport: RenderViewport,
    render_state: RenderState,
    transfer: &IntensityTransfer,
    quality: CameraRenderQuality,
) -> anyhow::Result<(MipImageF32, FrameDiagnosticsF32)> {
    Ok(render_camera_f32_with_quality(
        volume,
        CameraFrame::new(camera, presentation_viewport)?,
        viewport,
        renderer_mode_f32(render_state, transfer)?,
        quality,
    )?)
}

pub(crate) fn rerender_state_with_backend(
    snapshot: &ApplicationSnapshot,
    dataset: &mut CurrentDatasetRuntime,
    analysis: &CurrentAnalysisRuntime,
    ui_runtime: &CurrentUiRuntime,
    render: &mut CurrentRenderRuntime,
    gpu_renderer: Option<&GpuRenderer>,
) -> anyhow::Result<()> {
    let render_start = Instant::now();
    update_visible_brick_plan(snapshot, dataset, render);
    let view = view_for_snapshot(snapshot);
    let active_layer = view
        .layer(view.active_layer())
        .ok_or_else(|| anyhow::anyhow!("active logical layer is absent from the canonical view"))?;
    let render_state = *active_layer.render_state();
    let transfer = IntensityTransfer::new(active_layer.visible(), active_layer.transfer().clone());
    let camera = *view.camera();
    let quality = camera_render_quality_for_render_state(render_state);

    if current_resident_frame_ready(snapshot, dataset, render)
        && resident_brick_render_supported(render_state.mode())
    {
        render_state_from_resident_bricks_with_backend(
            snapshot,
            dataset,
            render,
            analysis,
            ui_runtime,
            gpu_renderer,
        )?;
        record_completed_frame_time(render, render_start);
        refresh_fidelity_resource_stats(snapshot, dataset, render, gpu_renderer);
        return Ok(());
    }

    if let Some(volume_f32) = &dataset.active_volume_f32 {
        if gpu_renderer.is_some() {
            anyhow::bail!(
                "dense float32 rendering is reference-only; the interactive viewer must stream bricks"
            );
        }
        let (frame_f32, diagnostics_f32) = render_f32_app_frame(
            volume_f32,
            camera,
            render.presentation_viewport,
            render.render_viewport,
            render_state,
            &transfer,
            quality,
        )?;
        render.frame = f32_frame_to_display_u16_for_mode(
            &frame_f32,
            render_state.mode(),
            active_layer.transfer().window(),
        )?;
        render.diagnostics = mirante4d_renderer::frame_diagnostics(
            volume_f32.shape.element_count()?,
            render.frame.pixels(),
        );
        render.frame_f32 = Some(frame_f32);
        render.diagnostics_f32 = Some(diagnostics_f32);
        render.render_backend = RenderBackend::CpuReference;
        render.frame_fidelity.displayed_scale_level = Some(volume_f32.scale_level);
        render.lod_schedule.displayed_scale_level = Some(volume_f32.scale_level);
        render.lod_schedule.pending_scale_level = None;
        render.frame_fidelity.target_scale_level = render.lod_schedule.target_scale_level;
        render.frame_fidelity.completeness = FrameCompleteness::Exact;
        render.frame_fidelity.reason = if volume_f32.scale_level == 0 {
            LodDecisionReason::ExactS0
        } else {
            LodDecisionReason::ScreenEquivalentCoarserScale
        };
        render.frame_fidelity.backend = render.render_backend;
        record_completed_frame_time(render, render_start);
        set_single_rendered_channel(snapshot, dataset, render)?;
        update_visible_brick_plan(snapshot, dataset, render);
        refresh_fidelity_resource_stats(snapshot, dataset, render, gpu_renderer);
        return Ok(());
    }

    if let Some(volume_u8) = &dataset.active_volume_u8 {
        if gpu_renderer.is_some() {
            anyhow::bail!(
                "dense uint8 rendering is reference-only; the interactive viewer must stream bricks"
            );
        }
        let (frame, diagnostics) = render_u8_app_frame(
            volume_u8,
            camera,
            render.presentation_viewport,
            render.render_viewport,
            render_state,
            &transfer,
            quality,
        )?;
        render.frame = frame;
        render.diagnostics = diagnostics;
        render.frame_f32 = None;
        render.diagnostics_f32 = None;
        render.render_backend = RenderBackend::CpuReference;
        render.frame_fidelity.displayed_scale_level = Some(volume_u8.scale_level);
        render.lod_schedule.displayed_scale_level = Some(volume_u8.scale_level);
        render.lod_schedule.pending_scale_level = None;
        render.frame_fidelity.target_scale_level = render.lod_schedule.target_scale_level;
        render.frame_fidelity.completeness = frame_completeness_for_rendered_scale(
            volume_u8.scale_level,
            render.frame_fidelity.target_scale_level,
            render.frame_fidelity.reason,
        );
        render.frame_fidelity.reason = if volume_u8.scale_level == 0 {
            LodDecisionReason::ExactS0
        } else {
            LodDecisionReason::ScreenEquivalentCoarserScale
        };
        render.frame_fidelity.backend = render.render_backend;
        record_completed_frame_time(render, render_start);
        set_single_rendered_channel(snapshot, dataset, render)?;
        refresh_fidelity_resource_stats(snapshot, dataset, render, gpu_renderer);
        return Ok(());
    }

    let Some(active_volume) = dataset.active_volume.as_ref() else {
        set_loading_frame_for_current_viewport(snapshot, render);
        set_single_rendered_channel(snapshot, dataset, render)?;
        render.render_backend = RenderBackend::CpuResidentBricks;
        render.frame_fidelity.backend = render.render_backend;
        render.frame_fidelity.displayed_scale_level = render.lod_schedule.displayed_scale_level;
        if render.frame_fidelity.completeness != FrameCompleteness::BudgetLimited {
            render.frame_fidelity.completeness = FrameCompleteness::Loading;
            render.frame_fidelity.reason = LodDecisionReason::LoadingTargetScale;
        }
        render.frame_fidelity.frame_time_ms = None;
        refresh_fidelity_resource_stats(snapshot, dataset, render, gpu_renderer);
        return Ok(());
    };

    let (frame, diagnostics, backend) = render_app_frame_with_backend(
        active_volume,
        camera,
        render.presentation_viewport,
        render.render_viewport,
        render_state,
        &transfer,
        quality,
        gpu_renderer,
    )?;
    render.frame = frame;
    render.diagnostics = diagnostics;
    render.render_backend = backend;
    render.frame_fidelity.displayed_scale_level = Some(active_volume.scale_level);
    render.lod_schedule.displayed_scale_level = Some(active_volume.scale_level);
    render.lod_schedule.pending_scale_level = None;
    render.frame_fidelity.target_scale_level = render.lod_schedule.target_scale_level;
    render.frame_fidelity.completeness = frame_completeness_for_rendered_scale(
        active_volume.scale_level,
        render.frame_fidelity.target_scale_level,
        render.frame_fidelity.reason,
    );
    render.frame_fidelity.reason = if active_volume.scale_level == 0 {
        LodDecisionReason::ExactS0
    } else {
        LodDecisionReason::ScreenEquivalentCoarserScale
    };
    render.frame_fidelity.backend = backend;
    record_completed_frame_time(render, render_start);
    set_single_rendered_channel(snapshot, dataset, render)?;
    refresh_fidelity_resource_stats(snapshot, dataset, render, gpu_renderer);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_app_frame_with_backend(
    volume: &DenseVolumeU16,
    camera: CameraView,
    presentation_viewport: PresentationViewport,
    viewport: RenderViewport,
    render_state: RenderState,
    transfer: &IntensityTransfer,
    quality: CameraRenderQuality,
    gpu_renderer: Option<&GpuRenderer>,
) -> anyhow::Result<(MipImageU16, FrameDiagnostics, RenderBackend)> {
    if let Some(gpu_renderer) = gpu_renderer {
        let camera_mode = renderer_mode(render_state, transfer)?;
        let backend = match render_state.mode() {
            RenderMode::Mip => Some(RenderBackend::GpuCameraMip),
            RenderMode::Isosurface => Some(RenderBackend::GpuCameraIso),
            RenderMode::Dvr => Some(RenderBackend::GpuCameraDvr),
        };
        if let Some(backend) = backend {
            match gpu_renderer.render_camera_with_quality(
                volume,
                CameraFrame::new(camera, presentation_viewport)?,
                viewport,
                camera_mode,
                quality,
            ) {
                Ok(output) => return Ok((output.image, output.frame, backend)),
                Err(err) => return Err(err).context("dense GPU rendering failed"),
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
        render_state,
        transfer,
        quality,
    )?;
    Ok((frame, diagnostics, RenderBackend::CpuReference))
}

pub(crate) fn set_single_rendered_channel(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
) -> anyhow::Result<()> {
    let view = view_for_snapshot(snapshot);
    let layer = view
        .layer(view.active_layer())
        .ok_or_else(|| anyhow::anyhow!("active logical layer is absent from the canonical view"))?;
    render.rendered_channels = vec![RenderedIntensityChannel {
        layer_id: physical_layer_id_for_key(dataset, view.active_layer())?,
        render_state: *layer.render_state(),
        transfer: IntensityTransfer::new(layer.visible(), layer.transfer().clone()),
        frame: render.frame.clone(),
        frame_f32: render.frame_f32.clone(),
    }];
    update_channel_fidelity_status(snapshot, dataset, render);
    Ok(())
}

pub(crate) fn set_loading_frame_for_current_viewport(
    snapshot: &ApplicationSnapshot,
    render: &mut CurrentRenderRuntime,
) {
    let view = view_for_snapshot(snapshot);
    let render_mode = view
        .layer(view.active_layer())
        .map(|layer| layer.render_state().mode())
        .unwrap_or(RenderMode::Mip);
    if render.frame.width != render.render_viewport.width
        || render.frame.height != render.render_viewport.height
        || (render_mode == RenderMode::Isosurface && render.frame.iso_surface().is_none())
    {
        render.frame = placeholder_frame_for_mode(render.render_viewport, render_mode);
    }
    render.frame_f32 = None;
    let active_shape = snapshot
        .catalog()
        .layer(view.active_layer())
        .map(|layer| layer.shape().spatial());
    render.diagnostics = mirante4d_renderer::frame_diagnostics(
        active_shape
            .and_then(|shape| shape.element_count().ok())
            .unwrap_or(0),
        render.frame.pixels(),
    );
    render.diagnostics_f32 = None;
}

pub(crate) fn refresh_fidelity_resource_stats(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    gpu_renderer: Option<&GpuRenderer>,
) {
    render.frame_fidelity.viewport = render.render_viewport;
    render.frame_fidelity.visible_bricks = render.visible_brick_count;
    render.frame_fidelity.resident_bricks = dataset
        .resident_bricks_u8_by_layer
        .values()
        .map(Vec::len)
        .sum::<usize>()
        + dataset
            .resident_bricks_by_layer
            .values()
            .map(Vec::len)
            .sum::<usize>()
        + dataset
            .resident_bricks_f32_by_layer
            .values()
            .map(Vec::len)
            .sum::<usize>();
    render.frame_fidelity.missing_occupied_bricks = dataset
        .brick_stream_requested
        .saturating_sub(dataset.brick_stream_completed);
    if let Ok(diagnostics) = dataset.dataset.diagnostics() {
        render.frame_fidelity.cpu_cache_bytes = diagnostics.stats.brick_cache_bytes;
    }
    if let Some(renderer) = gpu_renderer
        && let Ok(stats) = renderer.stats()
    {
        render.frame_fidelity.gpu_resident_bytes = stats.brick_atlas_resident_bytes;
    }
    update_channel_fidelity_status(snapshot, dataset, render);
}

pub(crate) fn update_channel_fidelity_status(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
) {
    let visible_status = render.frame_fidelity.clone();
    let view = view_for_snapshot(snapshot);
    let mut channel_fidelity = Vec::with_capacity(view.layers().len());
    for layer_view in view.layers() {
        let Some(layer) = snapshot.catalog().layer(layer_view.layer_key()) else {
            continue;
        };
        let Ok(layer_id) = physical_layer_id_for_key(dataset, layer_view.layer_key()) else {
            continue;
        };
        let visible = layer_view.visible();
        let resident_bricks = if visible {
            resident_brick_count_for_layer(dataset, &layer_id, layer.dtype())
        } else {
            0
        };
        let visible_bricks = if visible {
            render.visible_brick_count
        } else {
            0
        };
        let missing = if visible {
            dataset
                .brick_stream_requested
                .saturating_sub(dataset.brick_stream_completed)
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
        channel_fidelity.push(ChannelFidelityStatus {
            layer_id: layer_id.to_string(),
            layer_name: layer.label().to_owned(),
            visible,
            render_mode: layer_view.render_state().mode(),
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
        });
    }
    render.channel_fidelity = channel_fidelity;

    if visible_channel_fidelity_is_mixed(&render.channel_fidelity) {
        for channel in &mut render.channel_fidelity {
            if channel.visible {
                channel.warning = Some(ChannelFidelityWarning::MixedFidelity);
            }
        }
    }
}

fn resident_brick_count_for_layer(
    dataset: &CurrentDatasetRuntime,
    layer_id: &LayerId,
    dtype: IntensityDType,
) -> usize {
    match dtype {
        IntensityDType::Float32 => dataset
            .resident_bricks_f32_by_layer
            .get(layer_id)
            .map(Vec::len)
            .unwrap_or(0),
        IntensityDType::Uint8 => dataset
            .resident_bricks_u8_by_layer
            .get(layer_id)
            .map(Vec::len)
            .unwrap_or(0),
        IntensityDType::Uint16 => dataset
            .resident_bricks_by_layer
            .get(layer_id)
            .map(Vec::len)
            .unwrap_or(0),
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
        RenderError::EmptyVolume
        | RenderError::Shape(_)
        | RenderError::Space(_)
        | RenderError::Camera(_) => FrameFailureKind::InvalidTransform,
    }
}

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

fn resident_render_failure_can_trigger_lod_downgrade(kind: FrameFailureKind) -> bool {
    matches!(
        kind,
        FrameFailureKind::BudgetExceeded
            | FrameFailureKind::BackendLimit
            | FrameFailureKind::AllocationFailed
    )
}

pub(crate) fn request_lod_downgrade_after_resident_capacity_failure(
    snapshot: &ApplicationSnapshot,
    dataset: &mut CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    failure: ResidentRenderFailureStatus,
) -> bool {
    render.frame_fidelity.last_capacity_error = Some(failure.message.clone());
    render.frame_fidelity.last_failure_kind = Some(failure.kind);
    let (completeness, reason) = fidelity_state_for_resident_render_failure(failure.kind);
    render.frame_fidelity.completeness = completeness;
    render.frame_fidelity.reason = reason;
    dataset.brick_stream_last_error = Some(failure.message);

    if !resident_render_failure_can_trigger_lod_downgrade(failure.kind) {
        return false;
    }

    let Ok(active_layer_id) =
        physical_layer_id_for_key(dataset, view_for_snapshot(snapshot).active_layer())
    else {
        return false;
    };
    let Ok(scale_count) = dataset.dataset.scale_count(&active_layer_id) else {
        return false;
    };
    if dataset.brick_stream_scale_level as usize + 1 >= scale_count {
        return false;
    }

    render.lod_schedule.hard_failed_scale_level = Some(dataset.brick_stream_scale_level);
    render.lod_schedule.hard_failure_reason = Some(reason);
    render.lod_replan_pending = true;
    true
}

/// Records a render failure in the existing fidelity fault channel and, for a
/// typed resident-capacity failure, schedules one coarser LOD attempt.
///
/// Resident rendering wraps its failures in `ResidentRenderFailure`; dense or
/// composition failures remain visible but must not mutate the brick LOD plan.
pub(crate) fn record_render_failure(
    snapshot: &ApplicationSnapshot,
    dataset: &mut CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    error: &anyhow::Error,
) -> bool {
    if let Some(failure) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ResidentRenderFailure>())
        .map(|failure| failure.status.clone())
    {
        return request_lod_downgrade_after_resident_capacity_failure(
            snapshot, dataset, render, failure,
        );
    }

    let failure = render_failure_status(error);
    let (completeness, reason) = fidelity_state_for_resident_render_failure(failure.kind);
    render.frame_fidelity.last_failure_kind = Some(failure.kind);
    render.frame_fidelity.last_capacity_error = Some(failure.message.clone());
    render.frame_fidelity.completeness = completeness;
    render.frame_fidelity.reason = reason;
    dataset.brick_stream_last_error = Some(failure.message);
    false
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

pub(crate) fn f32_values_to_display_u16(values: &[f32], window: DisplayWindow) -> Vec<u16> {
    values
        .iter()
        .map(|value| f32_value_to_display_u16(*value, window))
        .collect()
}

pub(crate) fn f32_frame_to_display_u16(image: &MipImageF32, window: DisplayWindow) -> MipImageU16 {
    MipImageU16::try_new_with_mode_frames(
        image.width,
        image.height,
        f32_values_to_display_u16(image.pixels(), window),
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
    window: DisplayWindow,
) -> MipImageU16 {
    let iso_surface = image
        .iso_surface()
        .map(|surface| f32_iso_surface_to_display_u16_surface(surface, window));
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
    window: DisplayWindow,
) -> IsoSurfaceFrameU16 {
    IsoSurfaceFrameU16::try_new(
        surface.width,
        surface.height,
        f32_values_to_display_u16(surface.source_values(), window),
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
    window: DisplayWindow,
) -> anyhow::Result<MipImageU16> {
    Ok(match mode {
        RenderMode::Isosurface => f32_iso_frame_to_display_u16(image, window),
        RenderMode::Dvr => f32_dvr_frame_to_display_u16(image),
        RenderMode::Mip => f32_frame_to_display_u16(image, window),
    })
}

fn f32_value_to_display_u16(value: f32, window: DisplayWindow) -> u16 {
    if !value.is_finite() {
        return 0;
    }
    let low = window.low();
    let high = window.high();
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
