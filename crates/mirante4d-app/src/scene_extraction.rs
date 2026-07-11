use glam::DVec3;
use mirante4d_analysis::{
    AnnotationArtifact, MeasurementArtifact, MeasurementGeometry as AnalysisMeasurementGeometry,
    RoiArtifact, SceneArtifactId, SceneArtifactStore, SceneArtifactTime,
    SceneStyleRgba as AnalysisSceneStyleRgba, TrackTrailWindow,
    WorldGeometry as AnalysisWorldGeometry,
};
use mirante4d_domain::{CameraView, GridToWorld, TimeIndex};
use mirante4d_format::LayerId;
use mirante4d_render_api::CameraFrame;
use mirante4d_renderer::scene_render::SceneProjector;
use mirante4d_renderer::{
    CoordinateSpace, OcclusionPolicy, PickCompleteness, PickHit, PickHitKind, PickPolicy,
    PickPrimitive, PickValue, SceneColorRgba, SceneDrawList, SceneFrameContext, SceneGeometry,
    SceneLayer, SceneLayerId, SceneLayerKind, SceneObject, SceneObjectId, ScenePickTarget,
    SceneStyle, SceneTime, ScreenPosition, extract_scene_draw_list,
};

use crate::{
    current_runtime::{
        analysis::CurrentAnalysisRuntime, render::CurrentRenderRuntime, ui::CurrentUiRuntime,
    },
    scene_artifacts::{EditableSceneArtifactKind, SceneEditHandle, SceneEditHandleId},
    tools::ToolSelection,
};

const SCENE_HANDLE_RADIUS_PX: f32 = 5.0;
const SCENE_HANDLE_PICK_RADIUS_PX: f32 = 9.0;
pub(crate) const SCENE_HANDLE_LAYER_ID: &str = "scene-handles";

#[derive(Debug, Clone, Copy)]
pub(crate) struct SceneViewInput<'a> {
    pub(crate) active_layer_id: &'a LayerId,
    pub(crate) active_timepoint: TimeIndex,
    pub(crate) active_source_grid_to_world: GridToWorld,
    pub(crate) camera: CameraView,
}

pub(crate) fn scene_draw_list(
    analysis: &CurrentAnalysisRuntime,
    ui_runtime: &CurrentUiRuntime,
    view: SceneViewInput<'_>,
) -> anyhow::Result<SceneDrawList> {
    let layers = scene_layers(analysis, ui_runtime, view)?;
    Ok(extract_scene_draw_list(
        &layers,
        SceneFrameContext::new(view.active_timepoint).with_grid_to_world(
            view.active_layer_id.clone(),
            view.active_source_grid_to_world,
        ),
    ))
}

fn scene_layers(
    analysis: &CurrentAnalysisRuntime,
    ui_runtime: &CurrentUiRuntime,
    view: SceneViewInput<'_>,
) -> anyhow::Result<Vec<SceneLayer>> {
    let mut layers = scene_layers_from_artifacts(&analysis.scene_artifacts, view.active_timepoint)?;
    if let Some(handle_layer) = selected_scene_handle_layer(analysis, ui_runtime, view)? {
        layers.push(handle_layer);
    }
    Ok(layers)
}

fn scene_layers_from_artifacts(
    artifacts: &SceneArtifactStore,
    timepoint: TimeIndex,
) -> anyhow::Result<Vec<SceneLayer>> {
    let mut layers = Vec::new();

    let mut tracks = SceneLayer::new(SceneLayerId::new("tracks")?, SceneLayerKind::Track);
    for track in artifacts.tracks() {
        for segment in track.visible_segments(timepoint, TrackTrailWindow::CURRENT_SEGMENT) {
            let object_id = SceneObjectId::new(format!(
                "{}_seg_{}_{}",
                segment.track_id.as_str(),
                segment.start_timepoint.get(),
                segment.end_timepoint.get()
            ))?;
            tracks = tracks.with_object(
                SceneObject::new(
                    object_id,
                    CoordinateSpace::World,
                    SceneTime::Timepoint(timepoint),
                    OcclusionPolicy::VolumeDepthCued,
                    SceneGeometry::LineSegment {
                        start: segment.start_world,
                        end: segment.end_world,
                        width_px: 2.0,
                    },
                )
                .with_style(render_scene_style(track.style)),
            );
        }
    }
    push_scene_layer_if_nonempty(&mut layers, tracks);

    let mut rois = SceneLayer::new(SceneLayerId::new("rois")?, SceneLayerKind::Annotation);
    for roi in artifacts.rois() {
        if roi.visible && roi.time.is_visible_at(timepoint) {
            rois = rois.with_object(scene_object_from_roi(roi)?);
        }
    }
    push_scene_layer_if_nonempty(&mut layers, rois);

    let mut annotations = SceneLayer::new(
        SceneLayerId::new("annotations")?,
        SceneLayerKind::Annotation,
    );
    for annotation in artifacts.annotations() {
        if annotation.visible && annotation.time.is_visible_at(timepoint) {
            annotations = annotations.with_object(scene_object_from_annotation(annotation)?);
        }
    }
    push_scene_layer_if_nonempty(&mut layers, annotations);

    let mut measurements = SceneLayer::new(
        SceneLayerId::new("measurements")?,
        SceneLayerKind::Measurement,
    );
    for measurement in artifacts.measurements() {
        if measurement.visible && measurement.time.is_visible_at(timepoint) {
            measurements = measurements.with_object(scene_object_from_measurement(measurement)?);
        }
    }
    push_scene_layer_if_nonempty(&mut layers, measurements);

    Ok(layers)
}

fn scene_object_from_roi(roi: &RoiArtifact) -> anyhow::Result<SceneObject> {
    Ok(SceneObject::new(
        SceneObjectId::new(roi.id.as_str())?,
        CoordinateSpace::World,
        scene_time_from_artifact_time(roi.time)?,
        OcclusionPolicy::VolumeDepthCued,
        render_geometry_from_world_geometry(&roi.geometry),
    )
    .with_style(render_scene_style(roi.style)))
}

fn scene_object_from_annotation(annotation: &AnnotationArtifact) -> anyhow::Result<SceneObject> {
    Ok(SceneObject::new(
        SceneObjectId::new(annotation.id.as_str())?,
        CoordinateSpace::World,
        scene_time_from_artifact_time(annotation.time)?,
        OcclusionPolicy::AlwaysOnTop,
        render_geometry_from_world_geometry(&annotation.geometry),
    )
    .with_style(render_scene_style(annotation.style)))
}

fn scene_object_from_measurement(measurement: &MeasurementArtifact) -> anyhow::Result<SceneObject> {
    Ok(SceneObject::new(
        SceneObjectId::new(measurement.id.as_str())?,
        CoordinateSpace::World,
        scene_time_from_artifact_time(measurement.time)?,
        OcclusionPolicy::AlwaysOnTop,
        render_geometry_from_measurement_geometry(&measurement.geometry),
    )
    .with_style(render_scene_style(measurement.style)))
}

fn scene_time_from_artifact_time(time: SceneArtifactTime) -> anyhow::Result<SceneTime> {
    Ok(match time {
        SceneArtifactTime::Static => SceneTime::Static,
        SceneArtifactTime::Timepoint(timepoint) => SceneTime::Timepoint(timepoint),
        SceneArtifactTime::Interval {
            start,
            end_exclusive,
        } => SceneTime::interval(start, end_exclusive)?,
    })
}

fn render_geometry_from_world_geometry(geometry: &AnalysisWorldGeometry) -> SceneGeometry {
    match geometry {
        AnalysisWorldGeometry::Point {
            position,
            radius_px,
        } => SceneGeometry::Point {
            position: *position,
            radius_px: *radius_px,
        },
        AnalysisWorldGeometry::LineSegment {
            start,
            end,
            width_px,
        } => SceneGeometry::LineSegment {
            start: *start,
            end: *end,
            width_px: *width_px,
        },
        AnalysisWorldGeometry::Polyline { points, width_px } => SceneGeometry::Polyline {
            points: points.clone(),
            width_px: *width_px,
        },
        AnalysisWorldGeometry::Box3D { min, max } => SceneGeometry::Box3D {
            min: *min,
            max: *max,
        },
        AnalysisWorldGeometry::Ellipsoid { center, radii } => SceneGeometry::Ellipsoid {
            center: *center,
            radii: *radii,
        },
    }
}

fn render_geometry_from_measurement_geometry(
    geometry: &AnalysisMeasurementGeometry,
) -> SceneGeometry {
    match geometry {
        AnalysisMeasurementGeometry::Distance { start, end } => SceneGeometry::LineSegment {
            start: *start,
            end: *end,
            width_px: 2.0,
        },
    }
}

fn render_scene_style(style: AnalysisSceneStyleRgba) -> SceneStyle {
    let [red, green, blue, alpha] = style.color_rgba;
    SceneStyle::new(SceneColorRgba::new(
        unit_float_to_u8(red),
        unit_float_to_u8(green),
        unit_float_to_u8(blue),
        unit_float_to_u8(alpha),
    ))
}

fn unit_float_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn push_scene_layer_if_nonempty(layers: &mut Vec<SceneLayer>, layer: SceneLayer) {
    if !layer.objects().is_empty() {
        layers.push(layer);
    }
}

fn selected_scene_handle_layer(
    analysis: &CurrentAnalysisRuntime,
    ui_runtime: &CurrentUiRuntime,
    view: SceneViewInput<'_>,
) -> anyhow::Result<Option<SceneLayer>> {
    let mut layer = SceneLayer::new(
        SceneLayerId::new(SCENE_HANDLE_LAYER_ID)?,
        SceneLayerKind::Interaction,
    );
    for handle in selected_scene_edit_handles(analysis, ui_runtime, view.active_timepoint)? {
        layer = layer.with_object(
            SceneObject::new(
                SceneObjectId::new(format!(
                    "{}_{}",
                    handle.artifact_id.replace('-', "_"),
                    handle.object_suffix()
                ))?,
                CoordinateSpace::World,
                SceneTime::Timepoint(view.active_timepoint),
                OcclusionPolicy::AlwaysOnTop,
                SceneGeometry::Point {
                    position: handle_position(analysis, &handle)?,
                    radius_px: SCENE_HANDLE_RADIUS_PX,
                },
            )
            .with_style(SceneStyle::new(SceneColorRgba::WHITE)),
        );
    }
    Ok((!layer.objects().is_empty()).then_some(layer))
}

fn selected_scene_edit_handles(
    analysis: &CurrentAnalysisRuntime,
    ui_runtime: &CurrentUiRuntime,
    active_timepoint: TimeIndex,
) -> anyhow::Result<Vec<SceneEditHandleId>> {
    let Some(ToolSelection::SceneObject { kind, object_id }) = &ui_runtime.viewer_tools.selection
    else {
        return Ok(Vec::new());
    };
    match kind {
        PickHitKind::Roi => {
            let id = SceneArtifactId::new("roi", object_id.clone())?;
            let Some(roi) = analysis.scene_artifacts.roi(&id) else {
                return Ok(Vec::new());
            };
            if !roi.visible || !roi.time.is_visible_at(active_timepoint) {
                return Ok(Vec::new());
            }
            Ok(world_geometry_edit_handles(
                EditableSceneArtifactKind::Roi,
                object_id,
                &roi.geometry,
            ))
        }
        PickHitKind::Annotation => {
            let id = SceneArtifactId::new("annotation", object_id.clone())?;
            let Some(annotation) = analysis.scene_artifacts.annotation(&id) else {
                return Ok(Vec::new());
            };
            if !annotation.visible || !annotation.time.is_visible_at(active_timepoint) {
                return Ok(Vec::new());
            }
            Ok(world_geometry_edit_handles(
                EditableSceneArtifactKind::Annotation,
                object_id,
                &annotation.geometry,
            ))
        }
        PickHitKind::Track => {
            let id = SceneArtifactId::new("track", object_id.clone())?;
            let Some(track) = analysis.scene_artifacts.track(&id) else {
                return Ok(Vec::new());
            };
            if !track.visible {
                return Ok(Vec::new());
            }
            Ok(track
                .points
                .iter()
                .enumerate()
                .map(|(index, _)| SceneEditHandleId {
                    artifact_kind: EditableSceneArtifactKind::Track,
                    artifact_id: object_id.clone(),
                    handle: SceneEditHandle::TrackPoint { index },
                })
                .collect())
        }
        PickHitKind::Measurement => {
            let id = SceneArtifactId::new("measurement", object_id.clone())?;
            let Some(measurement) = analysis.scene_artifacts.measurement(&id) else {
                return Ok(Vec::new());
            };
            if !measurement.visible || !measurement.time.is_visible_at(active_timepoint) {
                return Ok(Vec::new());
            }
            Ok(match measurement.geometry {
                AnalysisMeasurementGeometry::Distance { .. } => vec![
                    SceneEditHandleId {
                        artifact_kind: EditableSceneArtifactKind::Measurement,
                        artifact_id: object_id.clone(),
                        handle: SceneEditHandle::MeasurementStart,
                    },
                    SceneEditHandleId {
                        artifact_kind: EditableSceneArtifactKind::Measurement,
                        artifact_id: object_id.clone(),
                        handle: SceneEditHandle::MeasurementEnd,
                    },
                ],
            })
        }
        _ => Ok(Vec::new()),
    }
}

pub(crate) fn world_geometry_edit_handles(
    artifact_kind: EditableSceneArtifactKind,
    artifact_id: &str,
    geometry: &AnalysisWorldGeometry,
) -> Vec<SceneEditHandleId> {
    let handles = match geometry {
        AnalysisWorldGeometry::Point { .. } => vec![SceneEditHandle::WorldPointPosition],
        AnalysisWorldGeometry::LineSegment { .. } => {
            vec![
                SceneEditHandle::WorldLineStart,
                SceneEditHandle::WorldLineEnd,
            ]
        }
        AnalysisWorldGeometry::Polyline { points, .. } => points
            .iter()
            .enumerate()
            .map(|(index, _)| SceneEditHandle::WorldPolylinePoint { index })
            .collect(),
        AnalysisWorldGeometry::Box3D { .. } => {
            vec![SceneEditHandle::WorldBoxMin, SceneEditHandle::WorldBoxMax]
        }
        AnalysisWorldGeometry::Ellipsoid { .. } => vec![
            SceneEditHandle::WorldEllipsoidCenter,
            SceneEditHandle::WorldEllipsoidRadiusX,
            SceneEditHandle::WorldEllipsoidRadiusY,
            SceneEditHandle::WorldEllipsoidRadiusZ,
        ],
    };
    handles
        .into_iter()
        .map(|handle| SceneEditHandleId {
            artifact_kind,
            artifact_id: artifact_id.to_owned(),
            handle,
        })
        .collect()
}

fn handle_position(
    analysis: &CurrentAnalysisRuntime,
    handle: &SceneEditHandleId,
) -> anyhow::Result<DVec3> {
    match handle.artifact_kind {
        EditableSceneArtifactKind::Track => {
            let id = SceneArtifactId::new("track", handle.artifact_id.clone())?;
            let track = analysis
                .scene_artifacts
                .track(&id)
                .ok_or_else(|| anyhow::anyhow!("track {} was not found", id.as_str()))?;
            match handle.handle {
                SceneEditHandle::TrackPoint { index } => Ok(track
                    .points
                    .get(index)
                    .ok_or_else(|| {
                        anyhow::anyhow!("track {} point index {} was not found", id.as_str(), index)
                    })?
                    .position_world),
                _ => anyhow::bail!("unsupported track handle {:?}", handle.handle),
            }
        }
        EditableSceneArtifactKind::Roi => {
            let id = SceneArtifactId::new("roi", handle.artifact_id.clone())?;
            let roi = analysis
                .scene_artifacts
                .roi(&id)
                .ok_or_else(|| anyhow::anyhow!("ROI {} was not found", id.as_str()))?;
            world_geometry_handle_position(&roi.geometry, &handle.handle)
        }
        EditableSceneArtifactKind::Annotation => {
            let id = SceneArtifactId::new("annotation", handle.artifact_id.clone())?;
            let annotation = analysis
                .scene_artifacts
                .annotation(&id)
                .ok_or_else(|| anyhow::anyhow!("annotation {} was not found", id.as_str()))?;
            world_geometry_handle_position(&annotation.geometry, &handle.handle)
        }
        EditableSceneArtifactKind::Measurement => {
            let id = SceneArtifactId::new("measurement", handle.artifact_id.clone())?;
            let measurement = analysis
                .scene_artifacts
                .measurement(&id)
                .ok_or_else(|| anyhow::anyhow!("measurement {} was not found", id.as_str()))?;
            match measurement.geometry {
                AnalysisMeasurementGeometry::Distance { start, end } => match handle.handle {
                    SceneEditHandle::MeasurementStart => Ok(start),
                    SceneEditHandle::MeasurementEnd => Ok(end),
                    _ => anyhow::bail!("unsupported measurement handle {:?}", handle.handle),
                },
            }
        }
    }
}

fn world_geometry_handle_position(
    geometry: &AnalysisWorldGeometry,
    handle: &SceneEditHandle,
) -> anyhow::Result<DVec3> {
    match (geometry, handle) {
        (AnalysisWorldGeometry::Point { position, .. }, SceneEditHandle::WorldPointPosition) => {
            Ok(*position)
        }
        (AnalysisWorldGeometry::LineSegment { start, .. }, SceneEditHandle::WorldLineStart) => {
            Ok(*start)
        }
        (AnalysisWorldGeometry::LineSegment { end, .. }, SceneEditHandle::WorldLineEnd) => Ok(*end),
        (
            AnalysisWorldGeometry::Polyline { points, .. },
            SceneEditHandle::WorldPolylinePoint { index },
        ) => points
            .get(*index)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("polyline point index {index} was not found")),
        (AnalysisWorldGeometry::Box3D { min, .. }, SceneEditHandle::WorldBoxMin) => Ok(*min),
        (AnalysisWorldGeometry::Box3D { max, .. }, SceneEditHandle::WorldBoxMax) => Ok(*max),
        (
            AnalysisWorldGeometry::Ellipsoid { center, .. },
            SceneEditHandle::WorldEllipsoidCenter,
        ) => Ok(*center),
        (
            AnalysisWorldGeometry::Ellipsoid { center, radii },
            SceneEditHandle::WorldEllipsoidRadiusX,
        ) => Ok(*center + DVec3::X * radii.x),
        (
            AnalysisWorldGeometry::Ellipsoid { center, radii },
            SceneEditHandle::WorldEllipsoidRadiusY,
        ) => Ok(*center + DVec3::Y * radii.y),
        (
            AnalysisWorldGeometry::Ellipsoid { center, radii },
            SceneEditHandle::WorldEllipsoidRadiusZ,
        ) => Ok(*center + DVec3::Z * radii.z),
        _ => anyhow::bail!(
            "handle {:?} does not apply to geometry {:?}",
            handle,
            geometry
        ),
    }
}

pub(crate) fn selected_scene_handle_pick_targets(
    analysis: &CurrentAnalysisRuntime,
    ui_runtime: &CurrentUiRuntime,
    render: &CurrentRenderRuntime,
    view: SceneViewInput<'_>,
) -> anyhow::Result<Vec<ScenePickTarget>> {
    let projector = SceneProjector::new(
        CameraFrame::new(view.camera, render.presentation_viewport)?,
        render.render_viewport,
    );
    let mut targets = Vec::new();
    for handle in selected_scene_edit_handles(analysis, ui_runtime, view.active_timepoint)? {
        let world_position = handle_position(analysis, &handle)?;
        let Some(screen_point) = projector.project_world(world_position) else {
            continue;
        };
        let screen_position = ScreenPosition::new(screen_point.x, screen_point.y);
        targets.push(ScenePickTarget {
            primitive: PickPrimitive::point(screen_position, SCENE_HANDLE_PICK_RADIUS_PX)?,
            hit: PickHit {
                kind: PickHitKind::AnnotationHandle,
                layer_id: Some(SceneLayerId::new(SCENE_HANDLE_LAYER_ID)?),
                object_id: Some(SceneObjectId::new(handle.artifact_id.clone())?),
                source_layer_id: None,
                timepoint: view.active_timepoint,
                world_position: Some(world_position),
                grid_position: None,
                screen_position: Some(screen_position),
                value: Some(PickValue::ObjectMetadata(handle.metadata_value())),
                policy: PickPolicy::SceneObject,
                completeness: PickCompleteness::Exact,
            },
        });
    }
    Ok(targets)
}
