use std::{collections::HashSet, time::Instant};

use mirante4d_application::ApplicationSnapshot;
use mirante4d_domain::{IntensityDType, RenderMode, RenderState, Shape3D, ViewerLayout};
use mirante4d_format::LayerId;
use mirante4d_render_api::{CameraFrame, PresentationViewport};
use mirante4d_renderer::{
    BrickAtlasResourceKey, CameraRenderMode, CameraRenderModeF32, CrossSectionView,
    FrameDiagnostics, FrameDiagnosticsF32, IntensityTransfer, MipImageF32, MipImageU16,
    RenderViewport, ResidentBrickSetF32, ResidentBrickSetU8, ResidentBrickSetU16,
    gpu::{
        GpuBrickAtlasPagePriority, GpuCrossSectionChunkDisplayChannel, GpuCrossSectionChunkDraw,
        GpuDisplayFrame, GpuRenderer, GpuResidentDisplayChannel, GpuResidentDisplayRequest,
    },
    render_camera_f32_from_bricks_with_quality, render_camera_from_bricks_with_quality,
    render_camera_u8_from_bricks_with_quality,
};

use crate::{
    FrameFailureKind, GPU_RESIDENT_BRICKS_PER_BATCH, LodDecisionReason, RenderBackend,
    RenderedIntensityChannel, application_view,
    brick_streaming::{current_resident_frame_ready, stream_layer_ids_for_snapshot},
    cross_section_runtime::{
        CrossSectionChunkKey, CrossSectionChunkPayload, CrossSectionChunkPriorityTier,
        CrossSectionChunkState,
    },
    current_physical_layer_id,
    current_runtime::{
        analysis::CurrentAnalysisRuntime, dataset::CurrentDatasetRuntime,
        render::CurrentRenderRuntime, ui::CurrentUiRuntime,
    },
    display_graph::DisplayGraph,
    render_state::{
        f32_frame_to_display_u16_for_mode, frame_completeness_for_rendered_scale,
        placeholder_frame_for_mode, record_completed_frame_time, refresh_fidelity_resource_stats,
        renderer_mode, renderer_mode_f32, resident_render_failure_error,
        resident_render_failure_from_gpu_error, update_channel_fidelity_status,
    },
    scene_extraction::{SceneViewInput, scene_draw_list},
    viewer_layout::{PanelId, render_cross_section_view_state},
    viewport::{camera_render_quality_for_render_state, resident_brick_render_supported},
};

mod dvr;

use self::dvr::render_dvr_state_from_resident_bricks;

#[derive(Debug, Clone)]
pub(super) struct ResidentLayer {
    id: LayerId,
    dtype: IntensityDType,
    render_state: RenderState,
    transfer: IntensityTransfer,
}

fn render_state_for_layer(layer: &ResidentLayer) -> RenderState {
    layer.render_state
}

fn transfer_for_layer(layer: &ResidentLayer) -> IntensityTransfer {
    layer.transfer
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CrossSectionPanelRenderRequest {
    pub(crate) panel_id: PanelId,
    pub(crate) generation: u64,
    pub(crate) view: CrossSectionView,
    pub(crate) presentation_viewport: PresentationViewport,
    pub(crate) render_viewport: RenderViewport,
}

#[allow(dead_code)]
pub(crate) struct CrossSectionPanelDisplayFrame {
    pub(crate) panel_id: PanelId,
    pub(crate) generation: u64,
    pub(crate) frame: GpuDisplayFrame,
    pub(crate) renderer_gpu_resident_chunks: HashSet<CrossSectionChunkKey>,
}

pub(crate) fn cross_section_panel_render_request(
    snapshot: &ApplicationSnapshot,
    render: &CurrentRenderRuntime,
    panel_id: PanelId,
) -> anyhow::Result<CrossSectionPanelRenderRequest> {
    let view = application_view(snapshot);
    if view.layout() != ViewerLayout::FourPanel {
        return Err(resident_render_failure_error(
            FrameFailureKind::InvalidModeParameter,
            "cross-section panel rendering requires the FourPanel layout",
        ));
    }
    let cross_section_panel = panel_id.cross_section_panel().ok_or_else(|| {
        resident_render_failure_error(
            FrameFailureKind::InvalidModeParameter,
            "the 3D panel is not a cross-section render target",
        )
    })?;
    let panel = render
        .cross_section_runtime
        .panel(panel_id)
        .ok_or_else(|| {
            resident_render_failure_error(
                FrameFailureKind::InvalidModeParameter,
                format!(
                    "panel {} is not present in FourPanel runtime",
                    panel_id.label()
                ),
            )
        })?;
    let presentation_viewport = panel.presentation_viewport.ok_or_else(|| {
        resident_render_failure_error(
            FrameFailureKind::InvalidModeParameter,
            format!(
                "panel {} does not have a presentation viewport",
                panel_id.label()
            ),
        )
    })?;
    let render_viewport = panel.render_viewport.ok_or_else(|| {
        resident_render_failure_error(
            FrameFailureKind::InvalidModeParameter,
            format!("panel {} does not have a render viewport", panel_id.label()),
        )
    })?;
    Ok(CrossSectionPanelRenderRequest {
        panel_id,
        generation: panel.generation,
        view: render_cross_section_view_state(*view.cross_section()).view(cross_section_panel),
        presentation_viewport,
        render_viewport,
    })
}

#[cfg(test)]
pub(crate) fn render_state_from_resident_bricks(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    analysis: &CurrentAnalysisRuntime,
    ui_runtime: &CurrentUiRuntime,
) -> anyhow::Result<()> {
    render_state_from_resident_bricks_with_backend(
        snapshot, dataset, render, analysis, ui_runtime, None,
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn product_cross_section_render_path_uses_chunked_renderer_only() {
        let source = include_str!("resident_rendering.rs");
        let forbidden_renderer_call =
            ["render_cross_section", "_channels_to_display_texture("].concat();
        let forbidden_channel_type = ["GpuCrossSection", "DisplayChannel"].concat();

        assert!(
            source.contains("render_cross_section_chunked_channels_to_display_texture"),
            "product 2D display bridge must call the chunked cross-section renderer"
        );
        assert!(
            !source.contains(&forbidden_renderer_call),
            "product 2D display bridge must not call the fullscreen/page-table cross-section renderer"
        );
        assert!(
            !source.contains(&forbidden_channel_type),
            "product 2D display bridge must not construct non-chunked cross-section display channels"
        );
    }
}

pub(crate) fn render_state_from_resident_bricks_with_backend(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    analysis: &CurrentAnalysisRuntime,
    ui_runtime: &CurrentUiRuntime,
    gpu_renderer: Option<&GpuRenderer>,
) -> anyhow::Result<()> {
    let render_start = Instant::now();
    let display_graph = DisplayGraph::from_snapshot(snapshot, dataset)?;
    if display_graph
        .channels
        .iter()
        .any(|channel| !resident_brick_render_supported(channel.render_state.mode()))
    {
        return Err(resident_render_failure_error(
            FrameFailureKind::InvalidModeParameter,
            "resident-brick rendering does not support one or more visible channel modes",
        ));
    }
    if !dataset.brick_stream_complete {
        return Err(resident_render_failure_error(
            FrameFailureKind::IncompleteResidency,
            "resident brick set is incomplete",
        ));
    }
    if !current_resident_frame_ready(snapshot, dataset, render) {
        return Err(resident_render_failure_error(
            FrameFailureKind::IncompleteResidency,
            "resident brick set does not match the current visible brick plan",
        ));
    }
    let view = application_view(snapshot);
    let active_layer_id = current_physical_layer_id(dataset, view.active_layer())?;
    let active_render_state = *view
        .layer(view.active_layer())
        .expect("application view has an active layer")
        .render_state();
    let layers = resident_render_layers(snapshot, dataset)?;
    let all_layers_dvr = layers
        .iter()
        .all(|layer| render_state_for_layer(layer).mode() == RenderMode::Dvr);
    if all_layers_dvr && layers.len() > 1 {
        render_dvr_state_from_resident_bricks(
            snapshot,
            dataset,
            render,
            render_start,
            gpu_renderer,
            layers,
        )?;
        return Ok(());
    }
    let mut rendered_channels = Vec::with_capacity(layers.len());
    let mut active_frame = None;
    for layer in layers {
        let rendered =
            render_resident_layer_with_backend(snapshot, dataset, render, &layer, gpu_renderer)?;
        if layer.id == active_layer_id {
            active_frame = Some((
                rendered.frame.clone(),
                rendered.frame_f32.clone(),
                rendered.diagnostics,
                rendered.diagnostics_f32,
                rendered.backend,
            ));
        }
        rendered_channels.push(RenderedIntensityChannel {
            layer_id: layer.id.clone(),
            render_state: render_state_for_layer(&layer),
            transfer: transfer_for_layer(&layer),
            frame: rendered.frame,
            frame_f32: rendered.frame_f32,
        });
    }

    let (frame, frame_f32, diagnostics, diagnostics_f32, backend) =
        active_frame.unwrap_or_else(|| {
            let frame =
                placeholder_frame_for_mode(render.render_viewport, active_render_state.mode());
            let diagnostics = mirante4d_renderer::frame_diagnostics(0, frame.pixels());
            (
                frame,
                None,
                diagnostics,
                None,
                RenderBackend::CpuResidentBricks,
            )
        });
    render.frame = frame;
    render.frame_f32 = frame_f32;
    render.diagnostics = diagnostics;
    render.diagnostics_f32 = diagnostics_f32;
    render.render_backend = backend;
    render.rendered_channels = rendered_channels;
    render.frame_fidelity.displayed_scale_level = Some(dataset.brick_stream_scale_level);
    render.lod_schedule.displayed_scale_level = Some(dataset.brick_stream_scale_level);
    render.lod_schedule.pending_scale_level = None;
    render.frame_fidelity.completeness = frame_completeness_for_rendered_scale(
        dataset.brick_stream_scale_level,
        render.frame_fidelity.target_scale_level,
        render.frame_fidelity.reason,
    );
    render.frame_fidelity.reason = if dataset.brick_stream_scale_level == 0 {
        LodDecisionReason::ExactS0
    } else {
        render.frame_fidelity.reason
    };
    render.frame_fidelity.backend = backend;
    record_completed_frame_time(render, render_start);
    render.frame_fidelity.last_failure_kind = None;
    render.frame_fidelity.last_capacity_error = None;
    refresh_fidelity_resource_stats(snapshot, dataset, render, gpu_renderer);
    update_channel_fidelity_status(snapshot, dataset, render);
    let _ = (analysis, ui_runtime);
    Ok(())
}

pub(crate) fn render_gpu_display_frame_from_resident_bricks(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    analysis: &CurrentAnalysisRuntime,
    ui_runtime: &CurrentUiRuntime,
    gpu_renderer: &GpuRenderer,
) -> anyhow::Result<GpuDisplayFrame> {
    let display_graph = DisplayGraph::from_snapshot(snapshot, dataset)?;
    let render_start = Instant::now();
    if display_graph
        .channels
        .iter()
        .any(|channel| !resident_brick_render_supported(channel.render_state.mode()))
    {
        return Err(resident_render_failure_error(
            FrameFailureKind::InvalidModeParameter,
            "GPU display rendering does not support one or more visible channel modes",
        ));
    }
    if !dataset.brick_stream_complete || !current_resident_frame_ready(snapshot, dataset, render) {
        return Err(resident_render_failure_error(
            FrameFailureKind::IncompleteResidency,
            "resident brick set is incomplete for GPU display rendering",
        ));
    }
    let layers = resident_render_layers(snapshot, dataset)?;
    let (frame, rendered_channels) = render_gpu_display_frame_for_resident_layers(
        snapshot,
        dataset,
        render,
        gpu_renderer,
        &layers,
    )?;
    let displayed_scale_level = dataset.brick_stream_scale_level;
    finalize_gpu_display_frame_state(
        snapshot,
        dataset,
        render,
        analysis,
        ui_runtime,
        gpu_renderer,
        frame,
        rendered_channels,
        RenderBackend::GpuResidentBricks,
        displayed_scale_level,
        render_start,
    )
}

pub(crate) fn render_gpu_cross_section_panel_frame_from_global_runtime(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    gpu_renderer: &GpuRenderer,
    panel_id: PanelId,
) -> anyhow::Result<CrossSectionPanelDisplayFrame> {
    let request = cross_section_panel_render_request(snapshot, render, panel_id)?;
    let render_scale_level = cross_section_panel_render_scale(render, panel_id)?;
    let layers = resident_render_layers(snapshot, dataset)?;
    if layers.is_empty() {
        return Err(resident_render_failure_error(
            FrameFailureKind::InvalidModeParameter,
            "cross-section display rendering requires at least one visible resident layer",
        ));
    }
    let mut owned_layers = Vec::with_capacity(layers.len());
    for layer in &layers {
        if !cross_section_global_runtime_panel_layer_has_resident_payload(
            snapshot,
            dataset,
            render,
            panel_id,
            layer,
            render_scale_level,
        ) {
            continue;
        }
        owned_layers.push(GpuCrossSectionLayerInput::new_for_panel(
            snapshot,
            dataset,
            render,
            panel_id,
            layer,
            transfer_for_layer(layer),
            render_scale_level,
        )?);
    }
    if owned_layers.is_empty() {
        return Err(resident_render_failure_error(
            FrameFailureKind::IncompleteResidency,
            format!(
                "{} global cross-section runtime has no resident chunks to render",
                panel_id.label()
            ),
        ));
    }
    let chunked_channels = owned_layers
        .iter()
        .map(GpuCrossSectionLayerInput::chunked_channel)
        .collect::<Vec<_>>();
    let mut frame = gpu_renderer.render_cross_section_chunked_channels_to_display_texture(
        &chunked_channels,
        request.view,
        request.presentation_viewport,
        request.render_viewport,
    )?;
    let renderer_gpu_resident_chunks =
        renderer_gpu_resident_cross_section_chunks(gpu_renderer, &owned_layers)?;
    // Four-panel 2D views can have identical render viewport sizes. The renderer
    // reuses its display texture cache by viewport, so store each panel frame in
    // an owned texture before another panel can overwrite the cached texture.
    frame = gpu_renderer.detach_display_frame_texture(frame)?;
    Ok(CrossSectionPanelDisplayFrame {
        panel_id: request.panel_id,
        generation: request.generation,
        frame,
        renderer_gpu_resident_chunks,
    })
}

fn cross_section_panel_render_scale(
    render: &CurrentRenderRuntime,
    panel_id: PanelId,
) -> anyhow::Result<u32> {
    render
        .cross_section_runtime
        .panel(panel_id)
        .and_then(|panel| panel.cross_section_schedule)
        .and_then(|schedule| schedule.render_scale_level)
        .ok_or_else(|| {
            resident_render_failure_error(
                FrameFailureKind::IncompleteResidency,
                format!(
                    "{} cross-section panel does not have a scheduled render scale",
                    panel_id.label()
                ),
            )
        })
}

fn render_gpu_display_frame_for_resident_layers(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    gpu_renderer: &GpuRenderer,
    layers: &[ResidentLayer],
) -> anyhow::Result<(GpuDisplayFrame, Vec<RenderedIntensityChannel>)> {
    if layers.is_empty() {
        return Err(resident_render_failure_error(
            FrameFailureKind::InvalidModeParameter,
            "GPU resident display rendering requires at least one layer",
        ));
    }

    let mut owned_layers = Vec::with_capacity(layers.len());
    for layer in layers {
        let render_state = render_state_for_layer(layer);
        let transfer = transfer_for_layer(layer);
        let (mode, mode_f32) = gpu_display_layer_modes_for_render_state(render_state, &transfer)?;
        owned_layers.push(GpuDisplayLayerInput::new(
            snapshot, dataset, layer, transfer, mode, mode_f32,
        )?);
    }
    let channels = owned_layers
        .iter()
        .map(GpuDisplayLayerInput::channel)
        .collect::<Vec<_>>();
    let view = application_view(snapshot);
    let active_render_state = *view
        .layer(view.active_layer())
        .expect("application view has an active layer")
        .render_state();
    let camera = CameraFrame::new(*view.camera(), render.presentation_viewport)?;
    let frame = gpu_renderer.render_resident_channels_to_display_texture(
        &channels,
        GpuResidentDisplayRequest {
            camera,
            viewport: render.render_viewport,
            quality: camera_render_quality_for_render_state(active_render_state),
            iso_light_state: *view.iso_light(),
            camera_axes: camera.axes(),
        },
    )?;
    let rendered_channels = layers
        .iter()
        .zip(owned_layers.iter())
        .map(|(layer, input)| RenderedIntensityChannel {
            layer_id: layer.id.clone(),
            render_state: render_state_for_layer(layer),
            transfer: *input.transfer(),
            frame: placeholder_frame_for_mode(
                render.render_viewport,
                render_state_for_layer(layer).mode(),
            ),
            frame_f32: None,
        })
        .collect();
    Ok((frame, rendered_channels))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn finalize_gpu_display_frame_state(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    analysis: &CurrentAnalysisRuntime,
    ui_runtime: &CurrentUiRuntime,
    gpu_renderer: &GpuRenderer,
    mut frame: GpuDisplayFrame,
    rendered_channels: Vec<RenderedIntensityChannel>,
    backend: RenderBackend,
    displayed_scale_level: u32,
    render_start: Instant,
) -> anyhow::Result<GpuDisplayFrame> {
    let view = application_view(snapshot);
    let active_render_state = *view
        .layer(view.active_layer())
        .expect("application view has an active layer")
        .render_state();
    render.frame = placeholder_frame_for_mode(render.render_viewport, active_render_state.mode());
    render.frame_f32 = None;
    render.diagnostics = mirante4d_renderer::frame_diagnostics(0, render.frame.pixels());
    render.diagnostics_f32 = None;
    render.render_backend = backend;
    let active_layer_id = current_physical_layer_id(dataset, view.active_layer())?;
    let draw_list = scene_draw_list(
        analysis,
        ui_runtime,
        SceneViewInput {
            active_layer_id: &active_layer_id,
            active_timepoint: view.timepoint(),
            active_source_grid_to_world: snapshot
                .catalog()
                .layer(view.active_layer())
                .expect("application view closes over the dataset catalog")
                .grid_to_world(),
            camera: *view.camera(),
        },
    )?;
    if !draw_list.is_empty() {
        frame = gpu_renderer.render_scene_layers_to_display_texture(
            frame,
            &draw_list,
            CameraFrame::new(*view.camera(), render.presentation_viewport)?,
            render.render_viewport,
        )?;
    }
    render.rendered_channels = rendered_channels;
    render.frame_fidelity.displayed_scale_level = Some(displayed_scale_level);
    render.lod_schedule.displayed_scale_level = Some(displayed_scale_level);
    render.lod_schedule.pending_scale_level = None;
    render.frame_fidelity.completeness = frame_completeness_for_rendered_scale(
        displayed_scale_level,
        render.frame_fidelity.target_scale_level,
        render.frame_fidelity.reason,
    );
    render.frame_fidelity.reason = if displayed_scale_level == 0 {
        LodDecisionReason::ExactS0
    } else {
        render.frame_fidelity.reason
    };
    render.frame_fidelity.backend = render.render_backend;
    record_completed_frame_time(render, render_start);
    render.frame_fidelity.last_failure_kind = None;
    render.frame_fidelity.last_capacity_error = None;
    refresh_fidelity_resource_stats(snapshot, dataset, render, Some(gpu_renderer));
    update_channel_fidelity_status(snapshot, dataset, render);
    Ok(frame)
}

#[derive(Debug)]
struct ResidentLayerRender {
    frame: MipImageU16,
    frame_f32: Option<MipImageF32>,
    diagnostics: FrameDiagnostics,
    diagnostics_f32: Option<FrameDiagnosticsF32>,
    backend: RenderBackend,
}

enum ResidentSetInputU8 {
    Owned(Box<ResidentBrickSetU8>),
}

enum ResidentSetInputU16 {
    Owned(Box<ResidentBrickSetU16>),
}

enum ResidentSetInputF32 {
    Owned(Box<ResidentBrickSetF32>),
}

impl ResidentSetInputU8 {
    fn as_ref(&self) -> &ResidentBrickSetU8 {
        match self {
            Self::Owned(set) => set.as_ref(),
        }
    }
}

impl ResidentSetInputU16 {
    fn as_ref(&self) -> &ResidentBrickSetU16 {
        match self {
            Self::Owned(set) => set.as_ref(),
        }
    }
}

impl ResidentSetInputF32 {
    fn as_ref(&self) -> &ResidentBrickSetF32 {
        match self {
            Self::Owned(set) => set.as_ref(),
        }
    }
}

enum GpuDisplayLayerSet {
    F32 {
        set: ResidentSetInputF32,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
    },
    U8 {
        set: ResidentSetInputU8,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
    },
    U16 {
        set: ResidentSetInputU16,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
    },
}

struct GpuDisplayLayerInput {
    transfer: IntensityTransfer,
    mode: GpuDisplayLayerMode,
    set: GpuDisplayLayerSet,
}

struct GpuCrossSectionLayerInput {
    transfer: IntensityTransfer,
    set: GpuDisplayLayerSet,
    chunk_draws: Vec<GpuCrossSectionChunkDraw>,
}

#[derive(Clone, Copy)]
enum GpuDisplayLayerMode {
    Integer(mirante4d_renderer::CameraRenderMode),
    F32(mirante4d_renderer::CameraRenderModeF32),
}

pub(crate) fn gpu_display_layer_modes_for_render_state(
    render_state: RenderState,
    transfer: &IntensityTransfer,
) -> anyhow::Result<(CameraRenderMode, CameraRenderModeF32)> {
    Ok((
        renderer_mode(render_state, transfer)?,
        renderer_mode_f32(render_state, transfer)?,
    ))
}

impl GpuDisplayLayerInput {
    fn new(
        snapshot: &ApplicationSnapshot,
        dataset: &CurrentDatasetRuntime,
        layer: &ResidentLayer,
        transfer: IntensityTransfer,
        integer_mode: mirante4d_renderer::CameraRenderMode,
        f32_mode: mirante4d_renderer::CameraRenderModeF32,
    ) -> anyhow::Result<Self> {
        let layer_id = layer.id.clone();
        let brick_shape = dataset
            .dataset
            .brick_shape_at_scale(&layer_id, dataset.brick_stream_scale_level)?;
        let brick_grid_shape = dataset
            .dataset
            .brick_grid_shape_at_scale(&layer_id, dataset.brick_stream_scale_level)?;
        let set = match layer.dtype {
            IntensityDType::Uint8 => GpuDisplayLayerSet::U8 {
                set: ResidentSetInputU8::Owned(Box::new(resident_u8_set_for_layer(
                    snapshot, dataset, layer,
                )?)),
                brick_shape,
                brick_grid_shape,
            },
            IntensityDType::Uint16 => GpuDisplayLayerSet::U16 {
                set: ResidentSetInputU16::Owned(Box::new(resident_u16_set_for_layer(
                    snapshot, dataset, layer,
                )?)),
                brick_shape,
                brick_grid_shape,
            },
            IntensityDType::Float32 => GpuDisplayLayerSet::F32 {
                set: ResidentSetInputF32::Owned(Box::new(resident_f32_set_for_layer(
                    snapshot, dataset, layer,
                )?)),
                brick_shape,
                brick_grid_shape,
            },
        };
        let mode = match layer.dtype {
            IntensityDType::Float32 => GpuDisplayLayerMode::F32(f32_mode),
            IntensityDType::Uint8 | IntensityDType::Uint16 => {
                GpuDisplayLayerMode::Integer(integer_mode)
            }
        };
        Ok(Self {
            transfer,
            mode,
            set,
        })
    }

    fn transfer(&self) -> &IntensityTransfer {
        &self.transfer
    }

    fn channel(&self) -> GpuResidentDisplayChannel<'_> {
        match (&self.set, self.mode) {
            (
                GpuDisplayLayerSet::F32 {
                    set,
                    brick_shape,
                    brick_grid_shape,
                },
                GpuDisplayLayerMode::F32(mode),
            ) => GpuResidentDisplayChannel::F32 {
                resident: set.as_ref(),
                brick_shape: *brick_shape,
                brick_grid_shape: *brick_grid_shape,
                mode,
                transfer: self.transfer,
            },
            (
                GpuDisplayLayerSet::U8 {
                    set,
                    brick_shape,
                    brick_grid_shape,
                },
                GpuDisplayLayerMode::Integer(mode),
            ) => GpuResidentDisplayChannel::U8 {
                resident: set.as_ref(),
                brick_shape: *brick_shape,
                brick_grid_shape: *brick_grid_shape,
                mode,
                transfer: self.transfer,
            },
            (
                GpuDisplayLayerSet::U16 {
                    set,
                    brick_shape,
                    brick_grid_shape,
                },
                GpuDisplayLayerMode::Integer(mode),
            ) => GpuResidentDisplayChannel::U16 {
                resident: set.as_ref(),
                brick_shape: *brick_shape,
                brick_grid_shape: *brick_grid_shape,
                mode,
                transfer: self.transfer,
            },
            (GpuDisplayLayerSet::F32 { .. }, GpuDisplayLayerMode::Integer(_))
            | (
                GpuDisplayLayerSet::U8 { .. } | GpuDisplayLayerSet::U16 { .. },
                GpuDisplayLayerMode::F32(_),
            ) => {
                unreachable!("GPU display layer dtype and mode are constructed together")
            }
        }
    }
}

impl GpuCrossSectionLayerInput {
    fn new_for_panel(
        snapshot: &ApplicationSnapshot,
        dataset: &CurrentDatasetRuntime,
        render: &CurrentRenderRuntime,
        panel_id: PanelId,
        layer: &ResidentLayer,
        transfer: IntensityTransfer,
        scale_level: u32,
    ) -> anyhow::Result<Self> {
        let layer_id = layer.id.clone();
        let brick_shape = dataset
            .dataset
            .brick_shape_at_scale(&layer_id, scale_level)?;
        let brick_grid_shape = dataset
            .dataset
            .brick_grid_shape_at_scale(&layer_id, scale_level)?;
        let set = match layer.dtype {
            IntensityDType::Uint8 => GpuDisplayLayerSet::U8 {
                set: ResidentSetInputU8::Owned(Box::new(
                    cross_section_u8_set_for_global_runtime_panel_layer(
                        snapshot,
                        dataset,
                        render,
                        panel_id,
                        layer,
                        scale_level,
                    )?,
                )),
                brick_shape,
                brick_grid_shape,
            },
            IntensityDType::Uint16 => GpuDisplayLayerSet::U16 {
                set: ResidentSetInputU16::Owned(Box::new(
                    cross_section_u16_set_for_global_runtime_panel_layer(
                        snapshot,
                        dataset,
                        render,
                        panel_id,
                        layer,
                        scale_level,
                    )?,
                )),
                brick_shape,
                brick_grid_shape,
            },
            IntensityDType::Float32 => GpuDisplayLayerSet::F32 {
                set: ResidentSetInputF32::Owned(Box::new(
                    cross_section_f32_set_for_global_runtime_panel_layer(
                        snapshot,
                        dataset,
                        render,
                        panel_id,
                        layer,
                        scale_level,
                    )?,
                )),
                brick_shape,
                brick_grid_shape,
            },
        };
        let chunk_draws = cross_section_chunk_draws_for_panel_layer(
            snapshot,
            dataset,
            render,
            panel_id,
            &layer_id,
            layer.dtype,
            scale_level,
        )?;
        if chunk_draws.is_empty() {
            return Err(resident_render_failure_error(
                FrameFailureKind::IncompleteResidency,
                format!(
                    "{} global cross-section runtime has resident payload but no chunk-plane draw geometry for layer {}",
                    panel_id.label(),
                    layer.id
                ),
            ));
        }
        Ok(Self {
            transfer,
            set,
            chunk_draws,
        })
    }

    fn chunked_channel(&self) -> GpuCrossSectionChunkDisplayChannel<'_> {
        match &self.set {
            GpuDisplayLayerSet::U8 {
                set,
                brick_shape,
                brick_grid_shape,
            } => GpuCrossSectionChunkDisplayChannel::U8 {
                resident: set.as_ref(),
                brick_shape: *brick_shape,
                brick_grid_shape: *brick_grid_shape,
                transfer: self.transfer,
                chunks: &self.chunk_draws,
            },
            GpuDisplayLayerSet::U16 {
                set,
                brick_shape,
                brick_grid_shape,
            } => GpuCrossSectionChunkDisplayChannel::U16 {
                resident: set.as_ref(),
                brick_shape: *brick_shape,
                brick_grid_shape: *brick_grid_shape,
                transfer: self.transfer,
                chunks: &self.chunk_draws,
            },
            GpuDisplayLayerSet::F32 {
                set,
                brick_shape,
                brick_grid_shape,
            } => GpuCrossSectionChunkDisplayChannel::F32 {
                resident: set.as_ref(),
                brick_shape: *brick_shape,
                brick_grid_shape: *brick_grid_shape,
                transfer: self.transfer,
                chunks: &self.chunk_draws,
            },
        }
    }

    fn brick_atlas_key(&self) -> anyhow::Result<BrickAtlasResourceKey> {
        self.set.brick_atlas_key()
    }
}

impl GpuDisplayLayerSet {
    fn brick_atlas_key(&self) -> anyhow::Result<BrickAtlasResourceKey> {
        match self {
            Self::U8 {
                set,
                brick_shape,
                brick_grid_shape,
            } => Ok(BrickAtlasResourceKey::from_resident_u8(
                set.as_ref(),
                *brick_shape,
                *brick_grid_shape,
            )?),
            Self::U16 {
                set,
                brick_shape,
                brick_grid_shape,
            } => Ok(BrickAtlasResourceKey::from_resident(
                set.as_ref(),
                *brick_shape,
                *brick_grid_shape,
            )?),
            Self::F32 {
                set,
                brick_shape,
                brick_grid_shape,
            } => Ok(BrickAtlasResourceKey::from_resident_f32(
                set.as_ref(),
                *brick_shape,
                *brick_grid_shape,
            )?),
        }
    }
}

fn renderer_gpu_resident_cross_section_chunks(
    gpu_renderer: &GpuRenderer,
    layers: &[GpuCrossSectionLayerInput],
) -> anyhow::Result<HashSet<CrossSectionChunkKey>> {
    let mut retained_chunks = HashSet::new();
    for layer in layers {
        let atlas_key = layer.brick_atlas_key()?;
        let snapshot = gpu_renderer.brick_atlas_residency(&atlas_key)?;
        if !snapshot.retained {
            continue;
        }
        for brick_index in snapshot.active_pages {
            retained_chunks.insert(CrossSectionChunkKey {
                dataset_id: atlas_key.dataset_id.clone(),
                layer_id: atlas_key.layer_id.clone(),
                timepoint: atlas_key.timepoint,
                scale_level: atlas_key.scale_level.get(),
                brick_index,
            });
        }
    }
    Ok(retained_chunks)
}

fn resident_render_layers(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
) -> anyhow::Result<Vec<ResidentLayer>> {
    let view = application_view(snapshot);
    let layer_ids = stream_layer_ids_for_snapshot(snapshot, dataset)?;
    let visible_layers = view
        .layers()
        .iter()
        .filter(|layer| layer.visible())
        .collect::<Vec<_>>();
    if layer_ids.len() != visible_layers.len() {
        anyhow::bail!("visible logical and physical layer sets are inconsistent");
    }
    Ok(layer_ids
        .into_iter()
        .zip(visible_layers)
        .map(|(id, layer_view)| {
            let layer = snapshot
                .catalog()
                .layer(layer_view.layer_key())
                .expect("application view closes over the dataset catalog");
            ResidentLayer {
                id,
                dtype: layer.dtype(),
                render_state: *layer_view.render_state(),
                transfer: IntensityTransfer::new(
                    layer_view.visible(),
                    layer_view.transfer().clone(),
                ),
            }
        })
        .collect())
}

fn render_resident_layer_with_backend(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    layer: &ResidentLayer,
    gpu_renderer: Option<&GpuRenderer>,
) -> anyhow::Result<ResidentLayerRender> {
    match layer.dtype {
        IntensityDType::Float32 => {
            render_resident_f32_layer_with_backend(snapshot, dataset, render, layer, gpu_renderer)
        }
        IntensityDType::Uint8 => {
            render_resident_u8_layer_with_backend(snapshot, dataset, render, layer, gpu_renderer)
        }
        IntensityDType::Uint16 => {
            render_resident_u16_layer_with_backend(snapshot, dataset, render, layer, gpu_renderer)
        }
    }
}

fn resident_u8_set_for_layer(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    layer: &ResidentLayer,
) -> anyhow::Result<ResidentBrickSetU8> {
    let layer_id = layer.id.clone();
    let active_layer_id =
        current_physical_layer_id(dataset, application_view(snapshot).active_layer())?;
    let bricks = dataset
        .resident_bricks_u8_by_layer
        .get(&layer.id)
        .cloned()
        .unwrap_or_else(|| {
            if layer.id == active_layer_id {
                dataset.resident_bricks_u8.clone()
            } else {
                Vec::new()
            }
        });
    if bricks.is_empty() {
        return Err(resident_render_failure_error(
            FrameFailureKind::IncompleteResidency,
            format!("resident uint8 brick set for layer {} is empty", layer.id),
        ));
    }
    let scale_shape = dataset
        .dataset
        .scale_shape(&layer_id, dataset.brick_stream_scale_level)?;
    let grid_to_world = dataset
        .dataset
        .scale_grid_to_world(&layer_id, dataset.brick_stream_scale_level)?;
    Ok(ResidentBrickSetU8::new(
        layer_id,
        application_view(snapshot).timepoint(),
        scale_shape,
        grid_to_world,
        bricks,
    ))
}

fn resident_u16_set_for_layer(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    layer: &ResidentLayer,
) -> anyhow::Result<ResidentBrickSetU16> {
    let layer_id = layer.id.clone();
    let active_layer_id =
        current_physical_layer_id(dataset, application_view(snapshot).active_layer())?;
    let bricks = dataset
        .resident_bricks_by_layer
        .get(&layer.id)
        .cloned()
        .unwrap_or_else(|| {
            if layer.id == active_layer_id {
                dataset.resident_bricks.clone()
            } else {
                Vec::new()
            }
        });
    if bricks.is_empty() {
        return Err(resident_render_failure_error(
            FrameFailureKind::IncompleteResidency,
            format!("resident brick set for layer {} is empty", layer.id),
        ));
    }
    let scale_shape = dataset
        .dataset
        .scale_shape(&layer_id, dataset.brick_stream_scale_level)?;
    let grid_to_world = dataset
        .dataset
        .scale_grid_to_world(&layer_id, dataset.brick_stream_scale_level)?;
    Ok(ResidentBrickSetU16::new(
        layer_id,
        application_view(snapshot).timepoint(),
        scale_shape,
        grid_to_world,
        bricks,
    ))
}

fn resident_f32_set_for_layer(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    layer: &ResidentLayer,
) -> anyhow::Result<ResidentBrickSetF32> {
    let layer_id = layer.id.clone();
    let active_layer_id =
        current_physical_layer_id(dataset, application_view(snapshot).active_layer())?;
    let bricks = dataset
        .resident_bricks_f32_by_layer
        .get(&layer.id)
        .cloned()
        .unwrap_or_else(|| {
            if layer.id == active_layer_id {
                dataset.resident_bricks_f32.clone()
            } else {
                Vec::new()
            }
        });
    if bricks.is_empty() {
        return Err(resident_render_failure_error(
            FrameFailureKind::IncompleteResidency,
            format!("resident float32 brick set for layer {} is empty", layer.id),
        ));
    }
    let scale_shape = dataset
        .dataset
        .scale_shape(&layer_id, dataset.brick_stream_scale_level)?;
    let grid_to_world = dataset
        .dataset
        .scale_grid_to_world(&layer_id, dataset.brick_stream_scale_level)?;
    Ok(ResidentBrickSetF32::new(
        layer_id,
        application_view(snapshot).timepoint(),
        scale_shape,
        grid_to_world,
        bricks,
    ))
}

fn cross_section_u8_set_for_global_runtime_panel_layer(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    panel_id: PanelId,
    layer: &ResidentLayer,
    scale_level: u32,
) -> anyhow::Result<ResidentBrickSetU8> {
    let layer_id = layer.id.clone();
    let bricks = cross_section_runtime_u8_bricks_for_panel_layer(
        snapshot,
        dataset,
        render,
        panel_id,
        &layer_id,
        scale_level,
    )?;
    if bricks.is_empty() {
        return Err(resident_render_failure_error(
            FrameFailureKind::IncompleteResidency,
            format!(
                "{} global cross-section uint8 chunk set for layer {} is empty",
                panel_id.label(),
                layer.id
            ),
        ));
    }
    let scale_shape = dataset.dataset.scale_shape(&layer_id, scale_level)?;
    let grid_to_world = dataset
        .dataset
        .scale_grid_to_world(&layer_id, scale_level)?;
    Ok(ResidentBrickSetU8::new(
        layer_id,
        application_view(snapshot).timepoint(),
        scale_shape,
        grid_to_world,
        bricks,
    ))
}

fn cross_section_u16_set_for_global_runtime_panel_layer(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    panel_id: PanelId,
    layer: &ResidentLayer,
    scale_level: u32,
) -> anyhow::Result<ResidentBrickSetU16> {
    let layer_id = layer.id.clone();
    let bricks = cross_section_runtime_u16_bricks_for_panel_layer(
        snapshot,
        dataset,
        render,
        panel_id,
        &layer_id,
        scale_level,
    )?;
    if bricks.is_empty() {
        return Err(resident_render_failure_error(
            FrameFailureKind::IncompleteResidency,
            format!(
                "{} global cross-section uint16 chunk set for layer {} is empty",
                panel_id.label(),
                layer.id
            ),
        ));
    }
    let scale_shape = dataset.dataset.scale_shape(&layer_id, scale_level)?;
    let grid_to_world = dataset
        .dataset
        .scale_grid_to_world(&layer_id, scale_level)?;
    Ok(ResidentBrickSetU16::new(
        layer_id,
        application_view(snapshot).timepoint(),
        scale_shape,
        grid_to_world,
        bricks,
    ))
}

fn cross_section_f32_set_for_global_runtime_panel_layer(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    panel_id: PanelId,
    layer: &ResidentLayer,
    scale_level: u32,
) -> anyhow::Result<ResidentBrickSetF32> {
    let layer_id = layer.id.clone();
    let bricks = cross_section_runtime_f32_bricks_for_panel_layer(
        snapshot,
        dataset,
        render,
        panel_id,
        &layer_id,
        scale_level,
    )?;
    if bricks.is_empty() {
        return Err(resident_render_failure_error(
            FrameFailureKind::IncompleteResidency,
            format!(
                "{} global cross-section float32 chunk set for layer {} is empty",
                panel_id.label(),
                layer.id
            ),
        ));
    }
    let scale_shape = dataset.dataset.scale_shape(&layer_id, scale_level)?;
    let grid_to_world = dataset
        .dataset
        .scale_grid_to_world(&layer_id, scale_level)?;
    Ok(ResidentBrickSetF32::new(
        layer_id,
        application_view(snapshot).timepoint(),
        scale_shape,
        grid_to_world,
        bricks,
    ))
}

fn cross_section_runtime_u8_bricks_for_panel_layer(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    panel_id: PanelId,
    layer_id: &LayerId,
    scale_level: u32,
) -> anyhow::Result<Vec<mirante4d_data::VolumeBrickU8>> {
    let panel_runtime = render
        .cross_section_runtime
        .panels
        .get(&panel_id)
        .ok_or_else(|| {
            resident_render_failure_error(
                FrameFailureKind::IncompleteResidency,
                format!(
                    "{} global cross-section panel runtime is missing",
                    panel_id.label()
                ),
            )
        })?;
    let mut bricks = Vec::new();
    for key in &panel_runtime.visible_chunks {
        if !cross_section_runtime_key_matches_current_layer(
            snapshot,
            dataset,
            key,
            layer_id,
            scale_level,
        ) {
            continue;
        }
        let Some(entry) = render.cross_section_runtime.chunks.get(key) else {
            continue;
        };
        if !matches!(
            entry.state,
            CrossSectionChunkState::CpuResident
                | CrossSectionChunkState::UploadQueued
                | CrossSectionChunkState::GpuResident
        ) {
            continue;
        }
        if let Some(CrossSectionChunkPayload::U8(brick)) = entry.payload.as_ref() {
            bricks.push((**brick).clone());
        }
    }
    bricks.sort_by_key(|brick| {
        (
            brick.brick_index.z,
            brick.brick_index.y,
            brick.brick_index.x,
        )
    });
    Ok(bricks)
}

fn cross_section_runtime_u16_bricks_for_panel_layer(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    panel_id: PanelId,
    layer_id: &LayerId,
    scale_level: u32,
) -> anyhow::Result<Vec<mirante4d_data::VolumeBrickU16>> {
    let panel_runtime = render
        .cross_section_runtime
        .panels
        .get(&panel_id)
        .ok_or_else(|| {
            resident_render_failure_error(
                FrameFailureKind::IncompleteResidency,
                format!(
                    "{} global cross-section panel runtime is missing",
                    panel_id.label()
                ),
            )
        })?;
    let mut bricks = Vec::new();
    for key in &panel_runtime.visible_chunks {
        if !cross_section_runtime_key_matches_current_layer(
            snapshot,
            dataset,
            key,
            layer_id,
            scale_level,
        ) {
            continue;
        }
        let Some(entry) = render.cross_section_runtime.chunks.get(key) else {
            continue;
        };
        if !matches!(
            entry.state,
            CrossSectionChunkState::CpuResident
                | CrossSectionChunkState::UploadQueued
                | CrossSectionChunkState::GpuResident
        ) {
            continue;
        }
        if let Some(CrossSectionChunkPayload::U16(brick)) = entry.payload.as_ref() {
            bricks.push((**brick).clone());
        }
    }
    bricks.sort_by_key(|brick| {
        (
            brick.brick_index.z,
            brick.brick_index.y,
            brick.brick_index.x,
        )
    });
    Ok(bricks)
}

fn cross_section_runtime_f32_bricks_for_panel_layer(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    panel_id: PanelId,
    layer_id: &LayerId,
    scale_level: u32,
) -> anyhow::Result<Vec<mirante4d_data::VolumeBrickF32>> {
    let panel_runtime = render
        .cross_section_runtime
        .panels
        .get(&panel_id)
        .ok_or_else(|| {
            resident_render_failure_error(
                FrameFailureKind::IncompleteResidency,
                format!(
                    "{} global cross-section panel runtime is missing",
                    panel_id.label()
                ),
            )
        })?;
    let mut bricks = Vec::new();
    for key in &panel_runtime.visible_chunks {
        if !cross_section_runtime_key_matches_current_layer(
            snapshot,
            dataset,
            key,
            layer_id,
            scale_level,
        ) {
            continue;
        }
        let Some(entry) = render.cross_section_runtime.chunks.get(key) else {
            continue;
        };
        if !matches!(
            entry.state,
            CrossSectionChunkState::CpuResident
                | CrossSectionChunkState::UploadQueued
                | CrossSectionChunkState::GpuResident
        ) {
            continue;
        }
        if let Some(CrossSectionChunkPayload::F32(brick)) = entry.payload.as_ref() {
            bricks.push((**brick).clone());
        }
    }
    bricks.sort_by_key(|brick| {
        (
            brick.brick_index.z,
            brick.brick_index.y,
            brick.brick_index.x,
        )
    });
    Ok(bricks)
}

fn cross_section_chunk_draws_for_panel_layer(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    panel_id: PanelId,
    layer_id: &LayerId,
    dtype: IntensityDType,
    scale_level: u32,
) -> anyhow::Result<Vec<GpuCrossSectionChunkDraw>> {
    let panel_runtime = render
        .cross_section_runtime
        .panels
        .get(&panel_id)
        .ok_or_else(|| {
            resident_render_failure_error(
                FrameFailureKind::IncompleteResidency,
                format!(
                    "{} global cross-section panel runtime is missing",
                    panel_id.label()
                ),
            )
        })?;
    let mut chunk_draws = Vec::new();
    for geometry in &panel_runtime.visible_chunk_geometries {
        let key = &geometry.key;
        if !cross_section_runtime_key_matches_current_layer(
            snapshot,
            dataset,
            key,
            layer_id,
            scale_level,
        ) {
            continue;
        }
        let Some(entry) = render.cross_section_runtime.chunks.get(key) else {
            continue;
        };
        if !matches!(
            entry.state,
            CrossSectionChunkState::CpuResident
                | CrossSectionChunkState::UploadQueued
                | CrossSectionChunkState::GpuResident
        ) {
            continue;
        }
        let dtype_matches = matches!(
            (dtype, entry.payload.as_ref()),
            (IntensityDType::Uint8, Some(CrossSectionChunkPayload::U8(_)))
                | (
                    IntensityDType::Uint16,
                    Some(CrossSectionChunkPayload::U16(_))
                )
                | (
                    IntensityDType::Float32,
                    Some(CrossSectionChunkPayload::F32(_))
                )
        );
        if !dtype_matches {
            continue;
        }
        chunk_draws.push(GpuCrossSectionChunkDraw {
            brick_index: key.brick_index,
            panel_bounds: geometry.panel_bounds,
            vertex_count: u32::try_from(geometry.vertex_count)?,
            cache_priority: cross_section_chunk_cache_priority(entry),
        });
    }
    chunk_draws.sort_by(|left, right| {
        cross_section_chunk_draw_order(left, right).then_with(|| {
            (left.brick_index.z, left.brick_index.y, left.brick_index.x).cmp(&(
                right.brick_index.z,
                right.brick_index.y,
                right.brick_index.x,
            ))
        })
    });
    Ok(chunk_draws)
}

fn cross_section_chunk_cache_priority(
    entry: &crate::cross_section_runtime::CrossSectionChunkEntry,
) -> GpuBrickAtlasPagePriority {
    GpuBrickAtlasPagePriority::new(
        cross_section_cache_priority_tier_rank(entry.priority_tier),
        entry.priority_score.unwrap_or(f64::NEG_INFINITY),
    )
}

fn cross_section_chunk_draw_order(
    left: &GpuCrossSectionChunkDraw,
    right: &GpuCrossSectionChunkDraw,
) -> std::cmp::Ordering {
    left.cache_priority
        .tier_rank
        .cmp(&right.cache_priority.tier_rank)
        .then_with(|| {
            right
                .cache_priority
                .score
                .total_cmp(&left.cache_priority.score)
        })
}

fn cross_section_cache_priority_tier_rank(
    priority_tier: Option<CrossSectionChunkPriorityTier>,
) -> u32 {
    match priority_tier {
        Some(CrossSectionChunkPriorityTier::VisibleActive) => 0,
        Some(CrossSectionChunkPriorityTier::VisibleLinked) => 1,
        Some(CrossSectionChunkPriorityTier::Refinement) => 2,
        Some(CrossSectionChunkPriorityTier::Prefetch) => 3,
        None => u32::MAX,
    }
}

fn cross_section_runtime_key_matches_current_layer(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    key: &crate::cross_section_runtime::CrossSectionChunkKey,
    layer_id: &LayerId,
    scale_level: u32,
) -> bool {
    key.dataset_id == *dataset.dataset.dataset_id()
        && key.layer_id == *layer_id
        && key.timepoint == application_view(snapshot).timepoint()
        && key.scale_level == scale_level
}

fn cross_section_global_runtime_panel_layer_has_resident_payload(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    panel_id: PanelId,
    layer: &ResidentLayer,
    scale_level: u32,
) -> bool {
    let layer_id = layer.id.clone();
    let Some(panel_runtime) = render.cross_section_runtime.panels.get(&panel_id) else {
        return false;
    };
    panel_runtime.visible_chunks.iter().any(|key| {
        if !cross_section_runtime_key_matches_current_layer(
            snapshot,
            dataset,
            key,
            &layer_id,
            scale_level,
        ) {
            return false;
        }
        let Some(entry) = render.cross_section_runtime.chunks.get(key) else {
            return false;
        };
        if !matches!(
            entry.state,
            CrossSectionChunkState::CpuResident
                | CrossSectionChunkState::UploadQueued
                | CrossSectionChunkState::GpuResident
        ) {
            return false;
        }
        matches!(
            (&layer.dtype, entry.payload.as_ref()),
            (IntensityDType::Uint8, Some(CrossSectionChunkPayload::U8(_)))
                | (
                    IntensityDType::Uint16,
                    Some(CrossSectionChunkPayload::U16(_))
                )
                | (
                    IntensityDType::Float32,
                    Some(CrossSectionChunkPayload::F32(_))
                )
        )
    })
}

fn render_resident_u8_layer_with_backend(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    layer: &ResidentLayer,
    gpu_renderer: Option<&GpuRenderer>,
) -> anyhow::Result<ResidentLayerRender> {
    let layer_id = layer.id.clone();
    let resident = resident_u8_set_for_layer(snapshot, dataset, layer)?;
    let brick_shape = dataset
        .dataset
        .brick_shape_at_scale(&layer_id, dataset.brick_stream_scale_level)?;
    let brick_grid_shape = dataset
        .dataset
        .brick_grid_shape_at_scale(&layer_id, dataset.brick_stream_scale_level)?;
    let camera = CameraFrame::new(
        *application_view(snapshot).camera(),
        render.presentation_viewport,
    )?;
    let render_state = render_state_for_layer(layer);
    let transfer = transfer_for_layer(layer);
    let camera_mode = renderer_mode(render_state, &transfer)?;
    let quality = camera_render_quality_for_render_state(render_state);
    if let Some(gpu_renderer) = gpu_renderer {
        let output = gpu_renderer
            .render_camera_u8_from_bricks_with_quality(
                &resident,
                brick_shape,
                brick_grid_shape,
                camera,
                render.render_viewport,
                camera_mode,
                quality,
            )
            .map_err(resident_render_failure_from_gpu_error)?;
        let diagnostics = output
            .brick_frame
            .expect("GPU resident-brick u8 renders must return brick diagnostics");
        if !diagnostics.complete {
            return Err(resident_render_failure_error(
                FrameFailureKind::IncompleteResidency,
                format!(
                    "GPU resident uint8 brick renderer reported incomplete frame for layer {} with {} missing samples",
                    layer.id, diagnostics.missing_voxel_samples
                ),
            ));
        }
        return Ok(ResidentLayerRender {
            frame: output.image,
            frame_f32: None,
            diagnostics: diagnostics.frame,
            diagnostics_f32: None,
            backend: RenderBackend::GpuResidentBricks,
        });
    }

    let (frame, diagnostics) = render_camera_u8_from_bricks_with_quality(
        &resident,
        camera,
        render.render_viewport,
        camera_mode,
        quality,
    )?;
    if !diagnostics.complete {
        return Err(resident_render_failure_error(
            FrameFailureKind::IncompleteResidency,
            format!(
                "resident uint8 brick renderer reported incomplete frame for layer {} with {} missing samples",
                layer.id, diagnostics.missing_voxel_samples
            ),
        ));
    }
    Ok(ResidentLayerRender {
        frame,
        frame_f32: None,
        diagnostics: diagnostics.frame,
        diagnostics_f32: None,
        backend: RenderBackend::CpuResidentBricks,
    })
}

fn render_resident_u16_layer_with_backend(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    layer: &ResidentLayer,
    gpu_renderer: Option<&GpuRenderer>,
) -> anyhow::Result<ResidentLayerRender> {
    let layer_id = layer.id.clone();
    let resident = resident_u16_set_for_layer(snapshot, dataset, layer)?;
    let brick_shape = dataset
        .dataset
        .brick_shape_at_scale(&layer_id, dataset.brick_stream_scale_level)?;
    let brick_grid_shape = dataset
        .dataset
        .brick_grid_shape_at_scale(&layer_id, dataset.brick_stream_scale_level)?;
    render_resident_u16_set_with_backend(
        snapshot,
        render,
        layer,
        resident,
        brick_shape,
        brick_grid_shape,
        gpu_renderer,
    )
}

fn render_resident_u16_set_with_backend(
    snapshot: &ApplicationSnapshot,
    render: &CurrentRenderRuntime,
    layer: &ResidentLayer,
    resident: ResidentBrickSetU16,
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
    gpu_renderer: Option<&GpuRenderer>,
) -> anyhow::Result<ResidentLayerRender> {
    let camera = CameraFrame::new(
        *application_view(snapshot).camera(),
        render.presentation_viewport,
    )?;
    let render_state = render_state_for_layer(layer);
    let transfer = transfer_for_layer(layer);
    let camera_mode = renderer_mode(render_state, &transfer)?;
    let quality = camera_render_quality_for_render_state(render_state);
    if let Some(gpu_renderer) = gpu_renderer {
        let output = match gpu_renderer.render_camera_from_bricks_with_quality(
            &resident,
            brick_shape,
            brick_grid_shape,
            camera,
            render.render_viewport,
            camera_mode,
            quality,
        ) {
            Ok(output) => output,
            Err(err) if matches!(render_state.mode(), RenderMode::Mip) => {
                tracing::warn!(
                    error = %err,
                    batch_size = GPU_RESIDENT_BRICKS_PER_BATCH,
                    "falling back to batched GPU resident MIP"
                );
                gpu_renderer
                    .render_camera_mip_from_bricks_batched(
                        &resident,
                        brick_shape,
                        brick_grid_shape,
                        camera,
                        render.render_viewport,
                        GPU_RESIDENT_BRICKS_PER_BATCH,
                    )
                    .map_err(resident_render_failure_from_gpu_error)?
            }
            Err(err) => return Err(resident_render_failure_from_gpu_error(err)),
        };
        let diagnostics = output
            .brick_frame
            .expect("GPU resident-brick renders must return brick diagnostics");
        if !diagnostics.complete {
            return Err(resident_render_failure_error(
                FrameFailureKind::IncompleteResidency,
                format!(
                    "GPU resident brick renderer reported incomplete frame for layer {} with {} missing samples",
                    layer.id, diagnostics.missing_voxel_samples
                ),
            ));
        }
        return Ok(ResidentLayerRender {
            frame: output.image,
            frame_f32: None,
            diagnostics: diagnostics.frame,
            diagnostics_f32: None,
            backend: RenderBackend::GpuResidentBricks,
        });
    }

    let (frame, diagnostics) = render_camera_from_bricks_with_quality(
        &resident,
        camera,
        render.render_viewport,
        camera_mode,
        quality,
    )?;
    if !diagnostics.complete {
        return Err(resident_render_failure_error(
            FrameFailureKind::IncompleteResidency,
            format!(
                "resident brick renderer reported incomplete frame for layer {} with {} missing samples",
                layer.id, diagnostics.missing_voxel_samples
            ),
        ));
    }
    Ok(ResidentLayerRender {
        frame,
        frame_f32: None,
        diagnostics: diagnostics.frame,
        diagnostics_f32: None,
        backend: RenderBackend::CpuResidentBricks,
    })
}

fn render_resident_f32_layer_with_backend(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    layer: &ResidentLayer,
    gpu_renderer: Option<&GpuRenderer>,
) -> anyhow::Result<ResidentLayerRender> {
    let layer_id = layer.id.clone();
    let resident = resident_f32_set_for_layer(snapshot, dataset, layer)?;
    let brick_shape = dataset
        .dataset
        .brick_shape_at_scale(&layer_id, dataset.brick_stream_scale_level)?;
    let brick_grid_shape = dataset
        .dataset
        .brick_grid_shape_at_scale(&layer_id, dataset.brick_stream_scale_level)?;
    let scale_shape = dataset
        .dataset
        .scale_shape(&layer_id, dataset.brick_stream_scale_level)?;
    let camera = CameraFrame::new(
        *application_view(snapshot).camera(),
        render.presentation_viewport,
    )?;
    let render_state = render_state_for_layer(layer);
    let transfer = transfer_for_layer(layer);
    let camera_mode = renderer_mode_f32(render_state, &transfer)?;
    let quality = camera_render_quality_for_render_state(render_state);
    let (frame_f32, diagnostics, backend) = if let Some(gpu_renderer) = gpu_renderer {
        let output = match gpu_renderer.render_camera_f32_from_bricks_with_quality(
            &resident,
            brick_shape,
            brick_grid_shape,
            camera,
            render.render_viewport,
            camera_mode,
            quality,
        ) {
            Ok(output) => output,
            Err(err) => return Err(resident_render_failure_from_gpu_error(err)),
        };
        let diagnostics = output
            .brick_frame
            .expect("GPU resident-brick f32 renders must return brick diagnostics");
        if !diagnostics.complete {
            return Err(resident_render_failure_error(
                FrameFailureKind::IncompleteResidency,
                format!(
                    "GPU resident float32 brick renderer reported incomplete frame for layer {} with {} missing samples",
                    layer.id, diagnostics.missing_voxel_samples
                ),
            ));
        }
        (
            output.image,
            diagnostics.frame,
            RenderBackend::GpuResidentBricks,
        )
    } else {
        let (frame_f32, diagnostics) = render_camera_f32_from_bricks_with_quality(
            &resident,
            camera,
            render.render_viewport,
            camera_mode,
            quality,
        )?;
        if !diagnostics.complete {
            return Err(resident_render_failure_error(
                FrameFailureKind::IncompleteResidency,
                format!(
                    "resident float32 brick renderer reported incomplete frame for layer {} with {} missing samples",
                    layer.id, diagnostics.missing_voxel_samples
                ),
            ));
        }
        (
            frame_f32,
            diagnostics.frame,
            RenderBackend::CpuResidentBricks,
        )
    };
    let frame =
        f32_frame_to_display_u16_for_mode(&frame_f32, render_state.mode(), transfer.window())?;
    let display_diagnostics =
        mirante4d_renderer::frame_diagnostics(scale_shape.element_count()?, frame.pixels());
    Ok(ResidentLayerRender {
        frame,
        frame_f32: Some(frame_f32),
        diagnostics: display_diagnostics,
        diagnostics_f32: Some(diagnostics),
        backend,
    })
}
