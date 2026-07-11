use eframe::egui;
use glam::DVec3;
use mirante4d_analysis::{
    MeasurementArtifact, MeasurementGeometry as AnalysisMeasurementGeometry, MeasurementProvenance,
    RoiArtifact, SceneArtifactId, SceneArtifactTime, SceneEditCommand,
    WorldGeometry as AnalysisWorldGeometry,
};
use mirante4d_core::LayerId;
use mirante4d_renderer::{
    PickCompleteness, PickHit, PickHitKind, PickPolicy, PickQuery, ScreenPosition, VolumePickProbe,
    empty_pick_hit, pick_camera_volume, pick_camera_volume_f32, pick_camera_volume_u8,
    pick_scene_targets, voxel_pick_hit, voxel_pick_hit_f32, voxel_pick_hit_u8,
};

use crate::{
    AppState, FrameCompleteness, RenderMode, ViewportHover, ViewportIntensity,
    render_state::{renderer_mode, renderer_mode_f32},
    scene_artifacts::{
        EditableSceneArtifactKind, SceneEditHandle, next_scene_artifact_id,
        normalize_world_geometry, refresh_measurement_result, scene_edit_handle_from_pick_hit,
        select_scene_artifact, update_scene_annotation_artifact, update_scene_measurement_artifact,
        update_scene_roi_artifact, update_scene_track_artifact,
    },
    scene_extraction::selected_scene_handle_pick_targets,
    tools::{ViewerToolCommand, ViewerToolEvent},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ToolInteractionOutcome {
    pub(crate) texture_refresh_requested: bool,
    pub(crate) rerender_requested: bool,
}

pub(crate) fn apply_viewport_tool_response(
    state: &mut AppState,
    response: &egui::Response,
    hover: Option<ViewportHover>,
) -> anyhow::Result<ToolInteractionOutcome> {
    let hit = match hover {
        Some(hover) => Some(pick_hit_from_viewport_hover_inner(state, hover, true)?),
        None => None,
    };
    let mut commands = state
        .viewer_tools
        .handle_event(ViewerToolEvent::Hover(hit.clone()));
    if let Some(hit) = hit {
        if response.clicked_by(egui::PointerButton::Primary) {
            commands.extend(
                state
                    .viewer_tools
                    .handle_event(ViewerToolEvent::PrimaryClick(hit.clone())),
            );
        }
        if response.dragged_by(egui::PointerButton::Primary) {
            commands.extend(
                state
                    .viewer_tools
                    .handle_event(ViewerToolEvent::PrimaryDrag(hit.clone())),
            );
        }
        if response.drag_stopped_by(egui::PointerButton::Primary) {
            commands.extend(
                state
                    .viewer_tools
                    .handle_event(ViewerToolEvent::PrimaryRelease(hit)),
            );
        }
    }
    apply_viewer_tool_commands(state, commands)
}

pub(crate) fn pick_hit_from_viewport_hover(
    state: &AppState,
    hover: ViewportHover,
) -> anyhow::Result<PickHit> {
    pick_hit_from_viewport_hover_inner(state, hover, true)
}

fn pick_hit_from_viewport_hover_inner(
    state: &AppState,
    hover: ViewportHover,
    include_scene_handles: bool,
) -> anyhow::Result<PickHit> {
    let query_position = ScreenPosition::new(hover.x as f32, hover.y as f32);
    if include_scene_handles
        && state.viewer_tools.active_scene_handle_drag.is_none()
        && let Some(handle_hit) = pick_selected_scene_handle(state, query_position)?
    {
        return Ok(handle_hit);
    }
    let layer_id = LayerId::new(state.active_layer_id.clone())?;
    if !active_intensity_layer_is_visible(state) {
        return Ok(empty_pick_hit(PickQuery {
            timepoint: state.active_timepoint,
            screen_position: query_position,
        }));
    }
    if let Some(volume_f32) = &state.active_volume_f32 {
        let readout = pick_camera_volume_f32(
            volume_f32,
            state.camera.to_camera_state(state.presentation_viewport),
            state.render_viewport,
            hover.x,
            hover.y,
            renderer_mode_f32(
                state.active_render_mode,
                &state.active_layer_transfer,
                state.active_layer_display,
                state.active_dvr_opacity_transfer,
                state.iso_display_level,
                state.dvr_density_scale,
            )?,
        )?;
        let probe = VolumePickProbe {
            source_layer_id: layer_id.clone(),
            timepoint: state.active_timepoint,
            screen_position: ScreenPosition::new(hover.x as f32, hover.y as f32),
            world_position: readout.world_position,
            grid_position: readout.grid_position,
            policy: readout.policy,
            completeness: readout.completeness,
        };
        return Ok(voxel_pick_hit_f32(probe, readout.intensity));
    }
    if let Some(volume_u8) = &state.active_volume_u8 {
        let readout = pick_camera_volume_u8(
            volume_u8,
            state.camera.to_camera_state(state.presentation_viewport),
            state.render_viewport,
            hover.x,
            hover.y,
            renderer_mode(
                state.active_render_mode,
                &state.active_layer_transfer,
                state.active_dvr_opacity_transfer,
                state.iso_display_level,
                state.dvr_density_scale,
            )?,
        )?;
        let probe = VolumePickProbe {
            source_layer_id: layer_id.clone(),
            timepoint: state.active_timepoint,
            screen_position: ScreenPosition::new(hover.x as f32, hover.y as f32),
            world_position: readout.world_position,
            grid_position: readout.grid_position,
            policy: readout.policy,
            completeness: readout.completeness,
        };
        return Ok(voxel_pick_hit_u8(probe, readout.intensity));
    }
    if let Some(active_volume) = state.active_volume.as_ref() {
        let readout = pick_camera_volume(
            active_volume,
            state.camera.to_camera_state(state.presentation_viewport),
            state.render_viewport,
            hover.x,
            hover.y,
            renderer_mode(
                state.active_render_mode,
                &state.active_layer_transfer,
                state.active_dvr_opacity_transfer,
                state.iso_display_level,
                state.dvr_density_scale,
            )?,
        )?;
        let probe = VolumePickProbe {
            source_layer_id: layer_id.clone(),
            timepoint: state.active_timepoint,
            screen_position: ScreenPosition::new(hover.x as f32, hover.y as f32),
            world_position: readout.world_position,
            grid_position: readout.grid_position,
            policy: readout.policy,
            completeness: readout.completeness,
        };
        return Ok(voxel_pick_hit(probe, readout.intensity));
    }
    let probe = approximate_pick_probe_from_hover(state, layer_id, hover);
    Ok(match hover.intensity {
        ViewportIntensity::U8(value) => voxel_pick_hit_u8(probe, value),
        ViewportIntensity::U16(value) => voxel_pick_hit(probe, value),
        ViewportIntensity::F32(value) => voxel_pick_hit_f32(probe, value),
    })
}

fn approximate_pick_probe_from_hover(
    state: &AppState,
    layer_id: LayerId,
    hover: ViewportHover,
) -> VolumePickProbe {
    VolumePickProbe {
        source_layer_id: layer_id,
        timepoint: state.active_timepoint,
        screen_position: ScreenPosition::new(hover.x as f32, hover.y as f32),
        world_position: None,
        grid_position: None,
        policy: pick_policy_for_render_mode(state.active_render_mode),
        completeness: pick_completeness_for_frame(state.frame_fidelity.completeness),
    }
}

fn active_intensity_layer_is_visible(state: &AppState) -> bool {
    state
        .layers
        .get(state.active_layer_index)
        .filter(|layer| layer.id == state.active_layer_id)
        .map(|layer| layer.display.visible)
        .unwrap_or(state.active_layer_display.visible)
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
    state: &AppState,
    screen_position: ScreenPosition,
) -> anyhow::Result<Option<PickHit>> {
    let targets = selected_scene_handle_pick_targets(state)?;
    if targets.is_empty() {
        return Ok(None);
    }
    let hit = pick_scene_targets(
        &targets,
        PickQuery {
            timepoint: state.active_timepoint,
            screen_position,
        },
    );
    Ok((hit.kind != PickHitKind::Empty).then_some(hit))
}

pub(crate) fn apply_viewer_tool_commands(
    state: &mut AppState,
    commands: Vec<ViewerToolCommand>,
) -> anyhow::Result<ToolInteractionOutcome> {
    let mut outcome = ToolInteractionOutcome::default();
    for command in commands {
        let command_outcome = apply_viewer_tool_command(state, command)?;
        outcome.texture_refresh_requested |= command_outcome.texture_refresh_requested;
        outcome.rerender_requested |= command_outcome.rerender_requested;
    }
    Ok(outcome)
}

fn apply_viewer_tool_command(
    state: &mut AppState,
    command: ViewerToolCommand,
) -> anyhow::Result<ToolInteractionOutcome> {
    match command {
        ViewerToolCommand::SetHover(hit) => {
            state.viewer_tools.hover = hit;
            Ok(ToolInteractionOutcome::default())
        }
        ViewerToolCommand::Select(selection) => {
            state.viewer_tools.selection = selection;
            Ok(ToolInteractionOutcome::default())
        }
        ViewerToolCommand::SetCrosshair(hit) => {
            state.viewer_tools.crosshair = Some(hit);
            Ok(ToolInteractionOutcome::default())
        }
        ViewerToolCommand::BeginRoi { .. } | ViewerToolCommand::PreviewRoi { .. } => {
            Ok(ToolInteractionOutcome::default())
        }
        ViewerToolCommand::CommitRoi { anchor, current } => {
            commit_roi_from_tool_command(state, &anchor, &current)?;
            Ok(ToolInteractionOutcome {
                texture_refresh_requested: false,
                rerender_requested: true,
            })
        }
        ViewerToolCommand::BeginMeasurement { .. }
        | ViewerToolCommand::PreviewMeasurement { .. } => Ok(ToolInteractionOutcome::default()),
        ViewerToolCommand::CommitMeasurement { anchor, current } => {
            commit_measurement_from_tool_command(state, &anchor, &current)?;
            Ok(ToolInteractionOutcome {
                texture_refresh_requested: false,
                rerender_requested: true,
            })
        }
        ViewerToolCommand::BeginSceneHandleDrag { handle } => {
            state.viewer_tools.active_scene_handle_drag = Some(handle);
            Ok(ToolInteractionOutcome::default())
        }
        ViewerToolCommand::DragSceneHandle { .. } => Ok(ToolInteractionOutcome::default()),
        ViewerToolCommand::CommitSceneHandleDrag { handle, current } => {
            update_scene_artifact_from_handle_drag(state, &handle, &current)?;
            state.viewer_tools.active_scene_handle_drag = None;
            Ok(ToolInteractionOutcome {
                texture_refresh_requested: false,
                rerender_requested: true,
            })
        }
        ViewerToolCommand::CancelTransientToolState => Ok(ToolInteractionOutcome::default()),
    }
}

fn commit_roi_from_tool_command(
    state: &mut AppState,
    anchor: &PickHit,
    current: &PickHit,
) -> anyhow::Result<()> {
    let start = tool_hit_world_position(anchor)?;
    let end = tool_hit_world_position(current)?;
    let id = next_scene_artifact_id(&state.scene_artifacts, "roi", "roi")?;
    let name = id.as_str().to_owned();
    let artifact = RoiArtifact::new(
        id,
        name,
        AnalysisWorldGeometry::Box3D {
            min: start.min(end),
            max: start.max(end),
        },
        SceneArtifactTime::Timepoint(state.active_timepoint),
    )?;
    state
        .scene_artifacts
        .apply(SceneEditCommand::PutRoi { artifact })?;
    state.last_workflow_message = Some("Created ROI".to_owned());
    Ok(())
}

fn commit_measurement_from_tool_command(
    state: &mut AppState,
    anchor: &PickHit,
    current: &PickHit,
) -> anyhow::Result<()> {
    let start = tool_hit_world_position(anchor)?;
    let end = tool_hit_world_position(current)?;
    let id = next_scene_artifact_id(&state.scene_artifacts, "measurement", "measurement")?;
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
                state.dataset_name, state.active_layer_id, state.active_timepoint.0
            ),
        },
        SceneArtifactTime::Timepoint(state.active_timepoint),
    )?;
    state
        .scene_artifacts
        .apply(SceneEditCommand::PutMeasurement { artifact })?;
    state.last_workflow_message = Some("Created distance measurement".to_owned());
    Ok(())
}

fn update_scene_artifact_from_handle_drag(
    state: &mut AppState,
    handle_hit: &PickHit,
    current_hit: &PickHit,
) -> anyhow::Result<()> {
    let handle = scene_edit_handle_from_pick_hit(handle_hit)?;
    let world_position = tool_hit_world_position(current_hit)?;
    match handle.artifact_kind {
        EditableSceneArtifactKind::Track => {
            let id = SceneArtifactId::new("track", handle.artifact_id.clone())?;
            let mut track = state
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
            update_scene_track_artifact(state, track)?;
            select_scene_artifact(state, EditableSceneArtifactKind::Track, &id);
        }
        EditableSceneArtifactKind::Roi => {
            let id = SceneArtifactId::new("roi", handle.artifact_id.clone())?;
            let mut roi = state
                .scene_artifacts
                .roi(&id)
                .ok_or_else(|| anyhow::anyhow!("ROI {} was not found", id.as_str()))?
                .clone();
            update_world_geometry_from_handle(&mut roi.geometry, &handle.handle, world_position)?;
            update_scene_roi_artifact(state, roi)?;
            select_scene_artifact(state, EditableSceneArtifactKind::Roi, &id);
        }
        EditableSceneArtifactKind::Annotation => {
            let id = SceneArtifactId::new("annotation", handle.artifact_id.clone())?;
            let mut annotation = state
                .scene_artifacts
                .annotation(&id)
                .ok_or_else(|| anyhow::anyhow!("annotation {} was not found", id.as_str()))?
                .clone();
            update_world_geometry_from_handle(
                &mut annotation.geometry,
                &handle.handle,
                world_position,
            )?;
            update_scene_annotation_artifact(state, annotation)?;
            select_scene_artifact(state, EditableSceneArtifactKind::Annotation, &id);
        }
        EditableSceneArtifactKind::Measurement => {
            let id = SceneArtifactId::new("measurement", handle.artifact_id.clone())?;
            let mut measurement = state
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
            update_scene_measurement_artifact(state, measurement)?;
            select_scene_artifact(state, EditableSceneArtifactKind::Measurement, &id);
        }
    }
    state.last_workflow_message = Some("Updated scene artifact from viewport handle".to_owned());
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
