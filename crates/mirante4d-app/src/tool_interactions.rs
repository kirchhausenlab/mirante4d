use eframe::egui;
use glam::DVec3;
use mirante4d_analysis::{
    MeasurementArtifact, MeasurementGeometry as AnalysisMeasurementGeometry, MeasurementProvenance,
    RoiArtifact, SceneArtifactId, SceneArtifactTime, SceneEditCommand,
    WorldGeometry as AnalysisWorldGeometry,
};
use mirante4d_application::ApplicationSnapshot;
use mirante4d_domain::{RenderMode, TimeIndex};
use mirante4d_format::LayerId;
use mirante4d_render_api::CameraFrame;
use mirante4d_renderer::{
    IntensityTransfer, PickCompleteness, PickHit, PickHitKind, PickPolicy, PickQuery,
    ScreenPosition, VolumePickProbe, empty_pick_hit, pick_camera_volume, pick_camera_volume_f32,
    pick_camera_volume_u8, pick_scene_targets, voxel_pick_hit, voxel_pick_hit_f32,
    voxel_pick_hit_u8,
};

use crate::{
    FrameCompleteness, ViewportHover, ViewportIntensity, application_view,
    current_physical_layer_id,
    current_runtime::{
        analysis::CurrentAnalysisRuntime, dataset::CurrentDatasetRuntime,
        render::CurrentRenderRuntime, ui::CurrentUiRuntime,
    },
    render_state::{renderer_mode, renderer_mode_f32},
    scene_artifacts::{
        EditableSceneArtifactKind, SceneEditHandle, next_scene_artifact_id,
        normalize_world_geometry, refresh_measurement_result, scene_edit_handle_from_pick_hit,
        select_scene_artifact, update_scene_annotation_artifact, update_scene_measurement_artifact,
        update_scene_roi_artifact, update_scene_track_artifact,
    },
    scene_extraction::{SceneViewInput, selected_scene_handle_pick_targets},
    tools::{ViewerToolCommand, ViewerToolEvent},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ToolInteractionOutcome {
    pub(crate) texture_refresh_requested: bool,
    pub(crate) rerender_requested: bool,
}

pub(crate) fn apply_viewport_tool_response(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    analysis: &mut CurrentAnalysisRuntime,
    ui_runtime: &mut CurrentUiRuntime,
    render: &CurrentRenderRuntime,
    response: &egui::Response,
    hover: Option<ViewportHover>,
) -> anyhow::Result<ToolInteractionOutcome> {
    let hit = match hover {
        Some(hover) => Some(pick_hit_from_viewport_hover_inner(
            snapshot, dataset, analysis, ui_runtime, render, hover, true,
        )?),
        None => None,
    };
    let mut commands = ui_runtime
        .viewer_tools
        .handle_event(ViewerToolEvent::Hover(hit.clone()));
    if response
        .ctx
        .input(|input| input.key_pressed(egui::Key::Escape))
    {
        commands.extend(
            ui_runtime
                .viewer_tools
                .handle_event(ViewerToolEvent::Cancel),
        );
    }
    if let Some(hit) = hit {
        if response.clicked_by(egui::PointerButton::Primary) {
            commands.extend(
                ui_runtime
                    .viewer_tools
                    .handle_event(ViewerToolEvent::PrimaryClick(hit.clone())),
            );
        }
        if response.dragged_by(egui::PointerButton::Primary) {
            commands.extend(
                ui_runtime
                    .viewer_tools
                    .handle_event(ViewerToolEvent::PrimaryDrag(hit.clone())),
            );
        }
        if response.drag_stopped_by(egui::PointerButton::Primary) {
            commands.extend(
                ui_runtime
                    .viewer_tools
                    .handle_event(ViewerToolEvent::PrimaryRelease(hit)),
            );
        }
    }
    apply_viewer_tool_commands(snapshot, analysis, ui_runtime, commands)
}

pub(crate) fn pick_hit_from_viewport_hover(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    analysis: &CurrentAnalysisRuntime,
    ui_runtime: &CurrentUiRuntime,
    render: &CurrentRenderRuntime,
    hover: ViewportHover,
) -> anyhow::Result<PickHit> {
    pick_hit_from_viewport_hover_inner(snapshot, dataset, analysis, ui_runtime, render, hover, true)
}

fn pick_hit_from_viewport_hover_inner(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    analysis: &CurrentAnalysisRuntime,
    ui_runtime: &CurrentUiRuntime,
    render: &CurrentRenderRuntime,
    hover: ViewportHover,
    include_scene_handles: bool,
) -> anyhow::Result<PickHit> {
    let query_position = ScreenPosition::new(hover.x as f32, hover.y as f32);
    if include_scene_handles
        && ui_runtime.viewer_tools.active_scene_handle_drag.is_none()
        && let Some(handle_hit) = pick_selected_scene_handle(
            snapshot,
            dataset,
            analysis,
            ui_runtime,
            render,
            query_position,
        )?
    {
        return Ok(handle_hit);
    }
    let view = application_view(snapshot);
    let active_layer = view
        .layer(view.active_layer())
        .expect("application view has an active layer");
    let layer_id = current_physical_layer_id(dataset, view.active_layer())?;
    let render_state = *active_layer.render_state();
    let transfer = IntensityTransfer::new(active_layer.visible(), active_layer.transfer().clone());
    let camera = CameraFrame::new(*view.camera(), render.presentation_viewport)?;
    if !active_layer.visible() {
        return Ok(empty_pick_hit(PickQuery {
            timepoint: view.timepoint(),
            screen_position: query_position,
        }));
    }
    if let Some(volume_f32) = &dataset.active_volume_f32 {
        let readout = pick_camera_volume_f32(
            volume_f32,
            camera,
            render.render_viewport,
            hover.x,
            hover.y,
            renderer_mode_f32(render_state, &transfer)?,
        )?;
        let probe = VolumePickProbe {
            source_layer_id: layer_id.clone(),
            timepoint: view.timepoint(),
            screen_position: ScreenPosition::new(hover.x as f32, hover.y as f32),
            world_position: readout.world_position,
            grid_position: readout.grid_position,
            policy: readout.policy,
            completeness: readout.completeness,
        };
        return Ok(voxel_pick_hit_f32(probe, readout.intensity));
    }
    if let Some(volume_u8) = &dataset.active_volume_u8 {
        let readout = pick_camera_volume_u8(
            volume_u8,
            camera,
            render.render_viewport,
            hover.x,
            hover.y,
            renderer_mode(render_state, &transfer)?,
        )?;
        let probe = VolumePickProbe {
            source_layer_id: layer_id.clone(),
            timepoint: view.timepoint(),
            screen_position: ScreenPosition::new(hover.x as f32, hover.y as f32),
            world_position: readout.world_position,
            grid_position: readout.grid_position,
            policy: readout.policy,
            completeness: readout.completeness,
        };
        return Ok(voxel_pick_hit_u8(probe, readout.intensity));
    }
    if let Some(active_volume) = dataset.active_volume.as_ref() {
        let readout = pick_camera_volume(
            active_volume,
            camera,
            render.render_viewport,
            hover.x,
            hover.y,
            renderer_mode(render_state, &transfer)?,
        )?;
        let probe = VolumePickProbe {
            source_layer_id: layer_id.clone(),
            timepoint: view.timepoint(),
            screen_position: ScreenPosition::new(hover.x as f32, hover.y as f32),
            world_position: readout.world_position,
            grid_position: readout.grid_position,
            policy: readout.policy,
            completeness: readout.completeness,
        };
        return Ok(voxel_pick_hit(probe, readout.intensity));
    }
    let probe = approximate_pick_probe_from_hover(
        view.timepoint(),
        render_state.mode(),
        render,
        layer_id,
        hover,
    );
    Ok(match hover.intensity {
        ViewportIntensity::U8(value) => voxel_pick_hit_u8(probe, value),
        ViewportIntensity::U16(value) => voxel_pick_hit(probe, value),
        ViewportIntensity::F32(value) => voxel_pick_hit_f32(probe, value),
    })
}

fn approximate_pick_probe_from_hover(
    timepoint: TimeIndex,
    mode: RenderMode,
    render: &CurrentRenderRuntime,
    layer_id: LayerId,
    hover: ViewportHover,
) -> VolumePickProbe {
    VolumePickProbe {
        source_layer_id: layer_id,
        timepoint,
        screen_position: ScreenPosition::new(hover.x as f32, hover.y as f32),
        world_position: None,
        grid_position: None,
        policy: pick_policy_for_render_mode(mode),
        completeness: pick_completeness_for_frame(render.frame_fidelity.completeness),
    }
}

fn pick_policy_for_render_mode(mode: RenderMode) -> PickPolicy {
    match mode {
        RenderMode::Mip => PickPolicy::MipArgmax,
        RenderMode::Isosurface => PickPolicy::FirstThresholdHit,
        RenderMode::Dvr => PickPolicy::ProbeRay,
    }
}

fn pick_completeness_for_frame(completeness: FrameCompleteness) -> PickCompleteness {
    match completeness {
        FrameCompleteness::Exact => PickCompleteness::Exact,
        FrameCompleteness::Complete | FrameCompleteness::BudgetLimited => {
            PickCompleteness::Approximate
        }
        FrameCompleteness::Loading => PickCompleteness::Loading,
        FrameCompleteness::Incomplete => PickCompleteness::Incomplete,
    }
}

fn pick_selected_scene_handle(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    analysis: &CurrentAnalysisRuntime,
    ui_runtime: &CurrentUiRuntime,
    render: &CurrentRenderRuntime,
    screen_position: ScreenPosition,
) -> anyhow::Result<Option<PickHit>> {
    let view = application_view(snapshot);
    let active_layer_id = current_physical_layer_id(dataset, view.active_layer())?;
    let targets = selected_scene_handle_pick_targets(
        analysis,
        ui_runtime,
        render,
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
    if targets.is_empty() {
        return Ok(None);
    }
    let hit = pick_scene_targets(
        &targets,
        PickQuery {
            timepoint: view.timepoint(),
            screen_position,
        },
    );
    Ok((hit.kind != PickHitKind::Empty).then_some(hit))
}

pub(crate) fn apply_viewer_tool_commands(
    snapshot: &ApplicationSnapshot,
    analysis: &mut CurrentAnalysisRuntime,
    ui_runtime: &mut CurrentUiRuntime,
    commands: Vec<ViewerToolCommand>,
) -> anyhow::Result<ToolInteractionOutcome> {
    let mut outcome = ToolInteractionOutcome::default();
    for command in commands {
        let command_outcome = apply_viewer_tool_command(snapshot, analysis, ui_runtime, command)?;
        outcome.texture_refresh_requested |= command_outcome.texture_refresh_requested;
        outcome.rerender_requested |= command_outcome.rerender_requested;
    }
    Ok(outcome)
}

fn apply_viewer_tool_command(
    snapshot: &ApplicationSnapshot,
    analysis: &mut CurrentAnalysisRuntime,
    ui_runtime: &mut CurrentUiRuntime,
    command: ViewerToolCommand,
) -> anyhow::Result<ToolInteractionOutcome> {
    match command {
        ViewerToolCommand::SetHover(hit) => {
            ui_runtime.viewer_tools.hover = hit;
            Ok(ToolInteractionOutcome::default())
        }
        ViewerToolCommand::Select(selection) => {
            ui_runtime.viewer_tools.selection = selection;
            Ok(ToolInteractionOutcome::default())
        }
        ViewerToolCommand::SetCrosshair(hit) => {
            ui_runtime.viewer_tools.crosshair = Some(hit);
            Ok(ToolInteractionOutcome::default())
        }
        ViewerToolCommand::BeginRoi { .. } | ViewerToolCommand::PreviewRoi { .. } => {
            Ok(ToolInteractionOutcome::default())
        }
        ViewerToolCommand::CommitRoi { anchor, current } => {
            commit_roi_from_tool_command(snapshot, analysis, &anchor, &current)?;
            Ok(ToolInteractionOutcome {
                texture_refresh_requested: false,
                rerender_requested: true,
            })
        }
        ViewerToolCommand::BeginMeasurement { .. }
        | ViewerToolCommand::PreviewMeasurement { .. } => Ok(ToolInteractionOutcome::default()),
        ViewerToolCommand::CommitMeasurement { anchor, current } => {
            commit_measurement_from_tool_command(snapshot, analysis, &anchor, &current)?;
            Ok(ToolInteractionOutcome {
                texture_refresh_requested: false,
                rerender_requested: true,
            })
        }
        ViewerToolCommand::BeginSceneHandleDrag { handle } => {
            ui_runtime.viewer_tools.active_scene_handle_drag = Some(handle);
            Ok(ToolInteractionOutcome::default())
        }
        ViewerToolCommand::DragSceneHandle { .. } => Ok(ToolInteractionOutcome::default()),
        ViewerToolCommand::CommitSceneHandleDrag { handle, current } => {
            update_scene_artifact_from_handle_drag(analysis, ui_runtime, &handle, &current)?;
            ui_runtime.viewer_tools.active_scene_handle_drag = None;
            Ok(ToolInteractionOutcome {
                texture_refresh_requested: false,
                rerender_requested: true,
            })
        }
        ViewerToolCommand::CancelTransientToolState => Ok(ToolInteractionOutcome::default()),
    }
}

fn commit_roi_from_tool_command(
    snapshot: &ApplicationSnapshot,
    analysis: &mut CurrentAnalysisRuntime,
    anchor: &PickHit,
    current: &PickHit,
) -> anyhow::Result<()> {
    let view = application_view(snapshot);
    let start = tool_hit_world_position(anchor)?;
    let end = tool_hit_world_position(current)?;
    let id = next_scene_artifact_id(&analysis.scene_artifacts, "roi", "roi")?;
    let name = id.as_str().to_owned();
    let artifact = RoiArtifact::new(
        id,
        name,
        AnalysisWorldGeometry::Box3D {
            min: start.min(end),
            max: start.max(end),
        },
        SceneArtifactTime::Timepoint(view.timepoint()),
    )?;
    analysis
        .scene_artifacts
        .apply(SceneEditCommand::PutRoi { artifact })?;
    Ok(())
}

fn commit_measurement_from_tool_command(
    snapshot: &ApplicationSnapshot,
    analysis: &mut CurrentAnalysisRuntime,
    anchor: &PickHit,
    current: &PickHit,
) -> anyhow::Result<()> {
    let view = application_view(snapshot);
    let layer = snapshot
        .catalog()
        .layer(view.active_layer())
        .expect("application view closes over the dataset catalog");
    let start = tool_hit_world_position(anchor)?;
    let end = tool_hit_world_position(current)?;
    let id = next_scene_artifact_id(&analysis.scene_artifacts, "measurement", "measurement")?;
    let name = id.as_str().to_owned();
    let artifact = MeasurementArtifact::distance(
        id,
        name,
        start,
        end,
        MeasurementProvenance {
            source: "viewer_tool".to_owned(),
            scope: format!(
                "dataset={} layer={} timepoint={}",
                snapshot.catalog().label(),
                layer.label(),
                view.timepoint().get()
            ),
        },
        SceneArtifactTime::Timepoint(view.timepoint()),
    )?;
    analysis
        .scene_artifacts
        .apply(SceneEditCommand::PutMeasurement { artifact })?;
    Ok(())
}

fn update_scene_artifact_from_handle_drag(
    analysis: &mut CurrentAnalysisRuntime,
    ui_runtime: &mut CurrentUiRuntime,
    handle_hit: &PickHit,
    current_hit: &PickHit,
) -> anyhow::Result<()> {
    let handle = scene_edit_handle_from_pick_hit(handle_hit)?;
    let world_position = tool_hit_world_position(current_hit)?;
    match handle.artifact_kind {
        EditableSceneArtifactKind::Track => {
            let id = SceneArtifactId::new("track", handle.artifact_id.clone())?;
            let mut track = analysis
                .scene_artifacts
                .track(&id)
                .ok_or_else(|| anyhow::anyhow!("track {} was not found", id.as_str()))?
                .clone();
            match handle.handle {
                SceneEditHandle::TrackPoint { index } => {
                    let point = track.points.get_mut(index).ok_or_else(|| {
                        anyhow::anyhow!("track {} point index {} was not found", id.as_str(), index)
                    })?;
                    point.position_world = world_position;
                }
                _ => anyhow::bail!("unsupported track handle {:?}", handle.handle),
            }
            update_scene_track_artifact(analysis, track)?;
            select_scene_artifact(ui_runtime, EditableSceneArtifactKind::Track, &id);
        }
        EditableSceneArtifactKind::Roi => {
            let id = SceneArtifactId::new("roi", handle.artifact_id.clone())?;
            let mut roi = analysis
                .scene_artifacts
                .roi(&id)
                .ok_or_else(|| anyhow::anyhow!("ROI {} was not found", id.as_str()))?
                .clone();
            update_world_geometry_from_handle(&mut roi.geometry, &handle.handle, world_position)?;
            update_scene_roi_artifact(analysis, roi)?;
            select_scene_artifact(ui_runtime, EditableSceneArtifactKind::Roi, &id);
        }
        EditableSceneArtifactKind::Annotation => {
            let id = SceneArtifactId::new("annotation", handle.artifact_id.clone())?;
            let mut annotation = analysis
                .scene_artifacts
                .annotation(&id)
                .ok_or_else(|| anyhow::anyhow!("annotation {} was not found", id.as_str()))?
                .clone();
            update_world_geometry_from_handle(
                &mut annotation.geometry,
                &handle.handle,
                world_position,
            )?;
            update_scene_annotation_artifact(analysis, annotation)?;
            select_scene_artifact(ui_runtime, EditableSceneArtifactKind::Annotation, &id);
        }
        EditableSceneArtifactKind::Measurement => {
            let id = SceneArtifactId::new("measurement", handle.artifact_id.clone())?;
            let mut measurement = analysis
                .scene_artifacts
                .measurement(&id)
                .ok_or_else(|| anyhow::anyhow!("measurement {} was not found", id.as_str()))?
                .clone();
            match &mut measurement.geometry {
                AnalysisMeasurementGeometry::Distance { start, end } => match handle.handle {
                    SceneEditHandle::MeasurementStart => *start = world_position,
                    SceneEditHandle::MeasurementEnd => *end = world_position,
                    _ => anyhow::bail!("unsupported measurement handle {:?}", handle.handle),
                },
            }
            refresh_measurement_result(&mut measurement);
            update_scene_measurement_artifact(analysis, measurement)?;
            select_scene_artifact(ui_runtime, EditableSceneArtifactKind::Measurement, &id);
        }
    }
    Ok(())
}

pub(crate) fn update_world_geometry_from_handle(
    geometry: &mut AnalysisWorldGeometry,
    handle: &SceneEditHandle,
    world_position: DVec3,
) -> anyhow::Result<()> {
    match (&mut *geometry, handle) {
        (AnalysisWorldGeometry::Point { position, .. }, SceneEditHandle::WorldPointPosition) => {
            *position = world_position;
        }
        (AnalysisWorldGeometry::LineSegment { start, .. }, SceneEditHandle::WorldLineStart) => {
            *start = world_position;
        }
        (AnalysisWorldGeometry::LineSegment { end, .. }, SceneEditHandle::WorldLineEnd) => {
            *end = world_position;
        }
        (
            AnalysisWorldGeometry::Polyline { points, .. },
            SceneEditHandle::WorldPolylinePoint { index },
        ) => {
            let point = points
                .get_mut(*index)
                .ok_or_else(|| anyhow::anyhow!("polyline point index {index} was not found"))?;
            *point = world_position;
        }
        (AnalysisWorldGeometry::Box3D { min, .. }, SceneEditHandle::WorldBoxMin) => {
            *min = world_position;
        }
        (AnalysisWorldGeometry::Box3D { max, .. }, SceneEditHandle::WorldBoxMax) => {
            *max = world_position;
        }
        (
            AnalysisWorldGeometry::Ellipsoid { center, .. },
            SceneEditHandle::WorldEllipsoidCenter,
        ) => {
            *center = world_position;
        }
        (
            AnalysisWorldGeometry::Ellipsoid { center, radii },
            SceneEditHandle::WorldEllipsoidRadiusX,
        ) => {
            radii.x = (world_position - *center).dot(DVec3::X).abs();
        }
        (
            AnalysisWorldGeometry::Ellipsoid { center, radii },
            SceneEditHandle::WorldEllipsoidRadiusY,
        ) => {
            radii.y = (world_position - *center).dot(DVec3::Y).abs();
        }
        (
            AnalysisWorldGeometry::Ellipsoid { center, radii },
            SceneEditHandle::WorldEllipsoidRadiusZ,
        ) => {
            radii.z = (world_position - *center).dot(DVec3::Z).abs();
        }
        _ => anyhow::bail!("handle {:?} does not apply to geometry", handle),
    }
    normalize_world_geometry(geometry)
}

fn tool_hit_world_position(hit: &PickHit) -> anyhow::Result<DVec3> {
    hit.world_position
        .ok_or_else(|| anyhow::anyhow!("tool command requires an exact world position"))
}
