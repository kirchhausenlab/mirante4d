use std::time::Instant;

use glam::{DVec2, DVec3};
use mirante4d_application::ApplicationSnapshot;
use mirante4d_dataset::{DatasetResourceKey, ResourceRegion};
use mirante4d_domain::{
    GridToWorld, IntensityDType, LogicalLayerKey, RenderState, ScaleLevel, ViewerLayout,
};
use mirante4d_render_api::{CameraFrame, PresentationViewport};
use mirante4d_renderer::{
    CameraRenderMode, CameraRenderModeF32, CrossSectionPanelBounds, CrossSectionView,
    CurrentLeaseVolume, IntensityTransfer, RenderViewport,
    gpu::{
        GpuBrickAtlasPagePriority, GpuCrossSectionChunkDraw, GpuDisplayFrame,
        GpuLeaseCrossSectionChannel, GpuLeaseDisplayChannel, GpuLeaseDisplayRequest, GpuRenderer,
    },
};

use crate::{
    FrameFailureKind, LodDecisionReason, RenderBackend, application_view,
    current_runtime::render::CurrentRenderRuntime,
    dataset_demand_plan::semantic_resource_shape,
    dataset_requests::{
        DatasetDemandState, SCOPE_CROSS_SECTION_XY, SCOPE_CROSS_SECTION_XZ, SCOPE_CROSS_SECTION_YZ,
        SCOPE_CURRENT_3D,
    },
    display_graph::DisplayGraph,
    render_state::{
        frame_completeness_for_rendered_scale, placeholder_frame_for_mode,
        record_completed_frame_time, renderer_mode, renderer_mode_f32,
        resident_render_failure_error, resident_render_failure_from_gpu_error,
    },
    viewer_layout::{PanelId, render_cross_section_view_state},
    viewport::{camera_render_quality_for_render_state, resident_brick_render_supported},
};

const PLANE_EPSILON: f64 = 1.0e-7;
const BOX_EDGES: [(usize, usize); 12] = [
    (0, 1),
    (0, 2),
    (0, 4),
    (1, 3),
    (1, 5),
    (2, 3),
    (2, 6),
    (3, 7),
    (4, 5),
    (4, 6),
    (5, 7),
    (6, 7),
];

#[derive(Debug, Clone, Copy)]
struct ResidentLayer {
    key: LogicalLayerKey,
    dtype: IntensityDType,
    render_state: RenderState,
    transfer: IntensityTransfer,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CrossSectionPanelRenderRequest {
    pub(crate) panel_id: PanelId,
    pub(crate) generation: u64,
    pub(crate) view: CrossSectionView,
    pub(crate) presentation_viewport: PresentationViewport,
    pub(crate) render_viewport: RenderViewport,
}

pub(crate) struct CrossSectionPanelDisplayFrame {
    pub(crate) panel_id: PanelId,
    pub(crate) generation: u64,
    pub(crate) frame: GpuDisplayFrame,
}

#[derive(Clone, Copy)]
enum GpuDisplayLayerMode {
    Integer(CameraRenderMode),
    F32(CameraRenderModeF32),
}

struct GpuDisplayLayerInput<'a> {
    dtype: IntensityDType,
    volume: CurrentLeaseVolume<'a>,
    transfer: IntensityTransfer,
    mode: GpuDisplayLayerMode,
}

struct GpuCrossSectionLayerInput<'a> {
    dtype: IntensityDType,
    volume: CurrentLeaseVolume<'a>,
    transfer: IntensityTransfer,
    draws: Vec<GpuCrossSectionChunkDraw>,
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
                    "panel {} is absent from FourPanel runtime",
                    panel_id.label()
                ),
            )
        })?;
    let presentation_viewport = panel.presentation_viewport.ok_or_else(|| {
        resident_render_failure_error(
            FrameFailureKind::InvalidModeParameter,
            format!("panel {} has no presentation viewport", panel_id.label()),
        )
    })?;
    let render_viewport = panel.render_viewport.ok_or_else(|| {
        resident_render_failure_error(
            FrameFailureKind::InvalidModeParameter,
            format!("panel {} has no render viewport", panel_id.label()),
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

pub(crate) fn render_gpu_display_frame_from_resident_bricks(
    snapshot: &ApplicationSnapshot,
    dataset: &DatasetDemandState,
    render: &mut CurrentRenderRuntime,
    gpu_renderer: &GpuRenderer,
) -> anyhow::Result<GpuDisplayFrame> {
    let started = Instant::now();
    let graph = DisplayGraph::from_snapshot(snapshot);
    if graph.channels.is_empty() {
        return Err(resident_render_failure_error(
            FrameFailureKind::InvalidModeParameter,
            "GPU display requires at least one visible logical layer",
        ));
    }
    if graph
        .channels
        .iter()
        .any(|channel| !resident_brick_render_supported(channel.render_state.mode()))
    {
        return Err(resident_render_failure_error(
            FrameFailureKind::InvalidModeParameter,
            "GPU display does not support one or more visible channel modes",
        ));
    }

    let layers = resident_render_layers(snapshot)?;
    let inputs = layers
        .iter()
        .map(|layer| display_layer_input(snapshot, dataset, render, *layer))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let channels = inputs
        .iter()
        .map(GpuDisplayLayerInput::channel)
        .collect::<Vec<_>>();
    let view = application_view(snapshot);
    let active_render_state = *view
        .layer(view.active_layer())
        .expect("application view has an active layer")
        .render_state();
    let camera = CameraFrame::new(*view.camera(), render.presentation_viewport)?;
    let frame = gpu_renderer
        .render_lease_channels_to_display_texture(
            dataset.cpu_ledger(),
            &channels,
            GpuLeaseDisplayRequest {
                camera,
                viewport: render.render_viewport,
                quality: camera_render_quality_for_render_state(active_render_state),
                iso_light_state: *view.iso_light(),
                camera_axes: camera.axes(),
            },
        )
        .map_err(resident_render_failure_from_gpu_error)?;
    finalize_gpu_display_state(snapshot, dataset, render, started);
    Ok(frame)
}

pub(crate) fn render_gpu_cross_section_panel_frame_from_global_runtime(
    snapshot: &ApplicationSnapshot,
    dataset: &DatasetDemandState,
    render: &CurrentRenderRuntime,
    gpu_renderer: &GpuRenderer,
    panel_id: PanelId,
) -> anyhow::Result<CrossSectionPanelDisplayFrame> {
    let request = cross_section_panel_render_request(snapshot, render, panel_id)?;
    let scope = match panel_id {
        PanelId::Xy => SCOPE_CROSS_SECTION_XY,
        PanelId::Xz => SCOPE_CROSS_SECTION_XZ,
        PanelId::Yz => SCOPE_CROSS_SECTION_YZ,
        PanelId::ThreeD => {
            return Err(resident_render_failure_error(
                FrameFailureKind::InvalidModeParameter,
                "the 3D panel is not a cross-section panel",
            ));
        }
    };
    let requirements = dataset.scope_requirements(scope);
    let layers = resident_render_layers(snapshot)?;
    let inputs = layers
        .iter()
        .map(|layer| {
            cross_section_layer_input(
                snapshot,
                dataset,
                render,
                scope,
                requirements,
                request,
                *layer,
            )
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let channels = inputs
        .iter()
        .filter(|input| !input.draws.is_empty())
        .map(GpuCrossSectionLayerInput::channel)
        .collect::<Vec<_>>();
    if channels.is_empty() {
        return Err(resident_render_failure_error(
            FrameFailureKind::IncompleteResidency,
            format!(
                "{} has no lease-backed resource intersecting its cross-section plane",
                panel_id.label()
            ),
        ));
    }
    let frame = gpu_renderer
        .render_lease_cross_section_channels_to_display_texture(
            dataset.cpu_ledger(),
            &channels,
            request.view,
            request.presentation_viewport,
            request.render_viewport,
        )
        .map_err(resident_render_failure_from_gpu_error)?;
    let frame = gpu_renderer.detach_display_frame_texture(frame)?;
    Ok(CrossSectionPanelDisplayFrame {
        panel_id: request.panel_id,
        generation: request.generation,
        frame,
    })
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

fn resident_render_layers(snapshot: &ApplicationSnapshot) -> anyhow::Result<Vec<ResidentLayer>> {
    let view = application_view(snapshot);
    view.layers()
        .iter()
        .filter(|layer| layer.visible())
        .map(|layer_view| {
            let layer = snapshot
                .catalog()
                .layer(layer_view.layer_key())
                .ok_or_else(|| {
                    anyhow::anyhow!("visible layer is absent from the dataset catalog")
                })?;
            Ok(ResidentLayer {
                key: layer.key(),
                dtype: layer.dtype(),
                render_state: *layer_view.render_state(),
                transfer: IntensityTransfer::new(
                    layer_view.visible(),
                    layer_view.transfer().clone(),
                ),
            })
        })
        .collect()
}

fn display_layer_input<'a>(
    snapshot: &ApplicationSnapshot,
    dataset: &'a DatasetDemandState,
    render: &'a CurrentRenderRuntime,
    layer: ResidentLayer,
) -> anyhow::Result<GpuDisplayLayerInput<'a>> {
    let requirements = dataset.scope_requirements(SCOPE_CURRENT_3D);
    let scale = dataset
        .scope_layer_scale(SCOPE_CURRENT_3D, layer.key)
        .ok_or_else(|| missing_layer_scale_error(layer.key))?;
    let volume = lease_volume(snapshot, render, requirements, layer.key, scale)?;
    let (integer_mode, f32_mode) =
        gpu_display_layer_modes_for_render_state(layer.render_state, &layer.transfer)?;
    let mode = match layer.dtype {
        IntensityDType::Float32 => GpuDisplayLayerMode::F32(f32_mode),
        IntensityDType::Uint8 | IntensityDType::Uint16 => {
            GpuDisplayLayerMode::Integer(integer_mode)
        }
    };
    Ok(GpuDisplayLayerInput {
        dtype: layer.dtype,
        volume,
        transfer: layer.transfer,
        mode,
    })
}

fn cross_section_layer_input<'a>(
    snapshot: &ApplicationSnapshot,
    dataset: &DatasetDemandState,
    render: &'a CurrentRenderRuntime,
    scope: u64,
    requirements: &'a [DatasetResourceKey],
    request: CrossSectionPanelRenderRequest,
    layer: ResidentLayer,
) -> anyhow::Result<GpuCrossSectionLayerInput<'a>> {
    let scale = dataset
        .scope_layer_scale(scope, layer.key)
        .ok_or_else(|| missing_layer_scale_error(layer.key))?;
    let volume = lease_volume(snapshot, render, requirements, layer.key, scale)?;
    let draws = volume
        .resident()
        .resources()
        .filter_map(|resource| {
            resource_plane_draw(
                request.view,
                request.presentation_viewport,
                volume.grid_to_world(),
                resource.key().region(),
            )
        })
        .collect();
    Ok(GpuCrossSectionLayerInput {
        dtype: layer.dtype,
        volume,
        transfer: layer.transfer,
        draws,
    })
}

fn lease_volume<'a>(
    snapshot: &ApplicationSnapshot,
    render: &'a CurrentRenderRuntime,
    requirements: &'a [DatasetResourceKey],
    layer: LogicalLayerKey,
    scale: ScaleLevel,
) -> anyhow::Result<CurrentLeaseVolume<'a>> {
    let catalog_layer = snapshot
        .catalog()
        .layer(layer)
        .ok_or_else(|| anyhow::anyhow!("visible layer is absent from the dataset catalog"))?;
    let scale_metadata = catalog_layer.scale(scale).ok_or_else(|| {
        anyhow::anyhow!(
            "layer {} has no scale {}",
            catalog_layer.label(),
            scale.get()
        )
    })?;
    let resident = render.lease_bridge.resident_subset(
        requirements,
        snapshot.catalog().scientific_identity().resource_identity(),
        layer,
        application_view(snapshot).timepoint(),
        scale,
    );
    if resident.is_empty() {
        return Err(resident_render_failure_error(
            FrameFailureKind::IncompleteResidency,
            format!(
                "layer {} has no retained semantic resource at scale {}",
                catalog_layer.label(),
                scale.get()
            ),
        ));
    }
    Ok(CurrentLeaseVolume::new(
        resident,
        scale_metadata.shape(),
        semantic_resource_shape(scale_metadata.shape()),
        scale_metadata.grid_to_world(),
    ))
}

impl GpuDisplayLayerInput<'_> {
    fn channel(&self) -> GpuLeaseDisplayChannel<'_> {
        match (self.dtype, self.mode) {
            (IntensityDType::Uint8, GpuDisplayLayerMode::Integer(mode)) => {
                GpuLeaseDisplayChannel::U8 {
                    volume: self.volume,
                    mode,
                    transfer: self.transfer,
                }
            }
            (IntensityDType::Uint16, GpuDisplayLayerMode::Integer(mode)) => {
                GpuLeaseDisplayChannel::U16 {
                    volume: self.volume,
                    mode,
                    transfer: self.transfer,
                }
            }
            (IntensityDType::Float32, GpuDisplayLayerMode::F32(mode)) => {
                GpuLeaseDisplayChannel::F32 {
                    volume: self.volume,
                    mode,
                    transfer: self.transfer,
                }
            }
            _ => unreachable!("dtype and GPU camera mode are constructed together"),
        }
    }
}

impl GpuCrossSectionLayerInput<'_> {
    fn channel(&self) -> GpuLeaseCrossSectionChannel<'_> {
        match self.dtype {
            IntensityDType::Uint8 => GpuLeaseCrossSectionChannel::U8 {
                volume: self.volume,
                transfer: self.transfer,
                chunks: &self.draws,
            },
            IntensityDType::Uint16 => GpuLeaseCrossSectionChannel::U16 {
                volume: self.volume,
                transfer: self.transfer,
                chunks: &self.draws,
            },
            IntensityDType::Float32 => GpuLeaseCrossSectionChannel::F32 {
                volume: self.volume,
                transfer: self.transfer,
                chunks: &self.draws,
            },
        }
    }
}

fn missing_layer_scale_error(layer: LogicalLayerKey) -> anyhow::Error {
    resident_render_failure_error(
        FrameFailureKind::IncompleteResidency,
        format!(
            "visible layer {} has no semantic resource requirement",
            layer.ordinal()
        ),
    )
}

fn finalize_gpu_display_state(
    snapshot: &ApplicationSnapshot,
    dataset: &DatasetDemandState,
    render: &mut CurrentRenderRuntime,
    started: Instant,
) {
    let view = application_view(snapshot);
    let active_render_state = *view
        .layer(view.active_layer())
        .expect("application view has an active layer")
        .render_state();
    render.frame = placeholder_frame_for_mode(render.render_viewport, active_render_state.mode());
    render.frame_f32 = None;
    render.diagnostics = mirante4d_renderer::frame_diagnostics(0, render.frame.pixels());
    render.render_backend = RenderBackend::GpuResidentBricks;
    let displayed_scale = dataset.current_scale().get();
    render.frame_fidelity.displayed_scale_level = Some(displayed_scale);
    render.lod_schedule.displayed_scale_level = Some(displayed_scale);
    render.lod_schedule.pending_scale_level = None;
    render.frame_fidelity.completeness = frame_completeness_for_rendered_scale(
        displayed_scale,
        render.frame_fidelity.target_scale_level,
        render.frame_fidelity.reason,
    );
    if displayed_scale == 0 {
        render.frame_fidelity.reason = LodDecisionReason::ExactS0;
    }
    render.frame_fidelity.backend = RenderBackend::GpuResidentBricks;
    render.frame_fidelity.last_failure_kind = None;
    render.frame_fidelity.last_capacity_error = None;
    record_completed_frame_time(render, started);
}

fn resource_plane_draw(
    view: CrossSectionView,
    viewport: PresentationViewport,
    grid_to_world: GridToWorld,
    region: ResourceRegion,
) -> Option<GpuCrossSectionChunkDraw> {
    let corners = resource_world_corners(grid_to_world, region);
    let normal_length = view.basis.normal_away_world.length();
    if !normal_length.is_finite() || normal_length <= PLANE_EPSILON {
        return None;
    }
    let normal = view.basis.normal_away_world / normal_length;
    let mut vertices = Vec::with_capacity(6);
    for (start_index, end_index) in BOX_EDGES {
        let start = corners[start_index];
        let end = corners[end_index];
        let start_distance = (start - view.center_world).dot(normal);
        let end_distance = (end - view.center_world).dot(normal);
        if start_distance.abs() <= PLANE_EPSILON {
            push_unique(&mut vertices, start);
        }
        if end_distance.abs() <= PLANE_EPSILON {
            push_unique(&mut vertices, end);
        }
        if start_distance * end_distance < -(PLANE_EPSILON * PLANE_EPSILON) {
            let t = start_distance / (start_distance - end_distance);
            if t.is_finite() {
                push_unique(&mut vertices, start + (end - start) * t);
            }
        }
    }
    if vertices.len() < 3 {
        return None;
    }
    let points = vertices
        .iter()
        .copied()
        .map(|world| panel_point(world, view, viewport))
        .collect::<Vec<_>>();
    let min = points
        .iter()
        .copied()
        .fold(DVec2::splat(f64::INFINITY), |a, b| a.min(b));
    let max = points
        .iter()
        .copied()
        .fold(DVec2::splat(f64::NEG_INFINITY), |a, b| a.max(b));
    if !min.is_finite() || !max.is_finite() || min.x >= max.x || min.y >= max.y {
        return None;
    }
    let center = corners
        .iter()
        .copied()
        .fold(DVec3::ZERO, |sum, point| sum + point)
        / 8.0;
    Some(GpuCrossSectionChunkDraw {
        resource_region: region,
        panel_bounds: CrossSectionPanelBounds {
            min_points: min,
            max_points: max,
        },
        vertex_count: u32::try_from(vertices.len()).ok()?,
        cache_priority: GpuBrickAtlasPagePriority::new(
            0,
            -(center - view.center_world).length_squared(),
        ),
    })
}

fn resource_world_corners(grid_to_world: GridToWorld, region: ResourceRegion) -> [DVec3; 8] {
    let origin = region.origin();
    let end = region.end_exclusive();
    let xs = [origin[2] as f64 - 0.5, end[2] as f64 - 0.5];
    let ys = [origin[1] as f64 - 0.5, end[1] as f64 - 0.5];
    let zs = [origin[0] as f64 - 0.5, end[0] as f64 - 0.5];
    [
        transform_grid_point(grid_to_world, xs[0], ys[0], zs[0]),
        transform_grid_point(grid_to_world, xs[1], ys[0], zs[0]),
        transform_grid_point(grid_to_world, xs[0], ys[1], zs[0]),
        transform_grid_point(grid_to_world, xs[1], ys[1], zs[0]),
        transform_grid_point(grid_to_world, xs[0], ys[0], zs[1]),
        transform_grid_point(grid_to_world, xs[1], ys[0], zs[1]),
        transform_grid_point(grid_to_world, xs[0], ys[1], zs[1]),
        transform_grid_point(grid_to_world, xs[1], ys[1], zs[1]),
    ]
}

fn transform_grid_point(grid_to_world: GridToWorld, x: f64, y: f64, z: f64) -> DVec3 {
    let matrix = grid_to_world.row_major();
    DVec3::new(
        matrix[0] * x + matrix[1] * y + matrix[2] * z + matrix[3],
        matrix[4] * x + matrix[5] * y + matrix[6] * z + matrix[7],
        matrix[8] * x + matrix[9] * y + matrix[10] * z + matrix[11],
    )
}

fn push_unique(vertices: &mut Vec<DVec3>, candidate: DVec3) {
    if vertices
        .iter()
        .all(|existing| existing.distance_squared(candidate) > PLANE_EPSILON * PLANE_EPSILON)
    {
        vertices.push(candidate);
    }
}

fn panel_point(world: DVec3, view: CrossSectionView, viewport: PresentationViewport) -> DVec2 {
    let delta = world - view.center_world;
    DVec2::new(
        viewport.width_points() * 0.5
            + delta.dot(view.basis.right_world) / view.scale_world_per_screen_point,
        viewport.height_points() * 0.5
            + delta.dot(view.basis.down_world) / view.scale_world_per_screen_point,
    )
}

#[cfg(test)]
mod tests {
    use glam::{DQuat, DVec3};
    use mirante4d_domain::Shape3D;
    use mirante4d_renderer::CrossSectionPanel;

    use super::*;

    #[test]
    fn semantic_resource_plane_draw_uses_resource_region_geometry() {
        let region = ResourceRegion::new([0, 0, 0], Shape3D::new(2, 2, 2).unwrap()).unwrap();
        let view = CrossSectionView::new(
            DVec3::new(0.5, 0.5, 0.5),
            CrossSectionPanel::Xy,
            DQuat::IDENTITY,
            1.0,
            1.0,
        );
        let viewport = PresentationViewport::new(10.0, 10.0).unwrap();
        let draw = resource_plane_draw(view, viewport, GridToWorld::identity(), region).unwrap();
        assert_eq!(draw.resource_region, region);
        assert_eq!(draw.vertex_count, 4);
        assert!(draw.panel_bounds.min_points.x < draw.panel_bounds.max_points.x);
        assert!(draw.panel_bounds.min_points.y < draw.panel_bounds.max_points.y);
    }
}
