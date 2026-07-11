use std::hash::Hash;

use eframe::egui;
use glam::DVec3;
use mirante4d_analysis::{
    AnnotationArtifact, MeasurementArtifact, MeasurementGeometry as AnalysisMeasurementGeometry,
    MeasurementResult, RoiArtifact, SceneArtifactId, SceneArtifactStore, SceneEditCommand,
    SceneStyleRgba as AnalysisSceneStyleRgba, TrackArtifact,
    WorldGeometry as AnalysisWorldGeometry,
};
use mirante4d_renderer::{PickHit, PickHitKind, PickValue};

use crate::{
    current_runtime::{analysis::CurrentAnalysisRuntime, ui::CurrentUiRuntime},
    tools::ToolSelection,
    ui_kit::{self, StatusTone},
};

const TRACK_POINT_HANDLE: &str = "track_point";
const WORLD_POINT_POSITION_HANDLE: &str = "world_point_position";
const WORLD_LINE_START_HANDLE: &str = "world_line_start";
const WORLD_LINE_END_HANDLE: &str = "world_line_end";
const WORLD_POLYLINE_POINT_HANDLE: &str = "world_polyline_point";
const ROI_BOX_MIN_HANDLE: &str = "world_box_min";
const ROI_BOX_MAX_HANDLE: &str = "world_box_max";
const WORLD_ELLIPSOID_CENTER_HANDLE: &str = "world_ellipsoid_center";
const WORLD_ELLIPSOID_RADIUS_X_HANDLE: &str = "world_ellipsoid_radius_x";
const WORLD_ELLIPSOID_RADIUS_Y_HANDLE: &str = "world_ellipsoid_radius_y";
const WORLD_ELLIPSOID_RADIUS_Z_HANDLE: &str = "world_ellipsoid_radius_z";
const MEASUREMENT_START_HANDLE: &str = "measurement_start";
const MEASUREMENT_END_HANDLE: &str = "measurement_end";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditableSceneArtifactKind {
    Track,
    Roi,
    Annotation,
    Measurement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SceneEditHandleId {
    pub(crate) artifact_kind: EditableSceneArtifactKind,
    pub(crate) artifact_id: String,
    pub(crate) handle: SceneEditHandle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SceneEditHandle {
    TrackPoint { index: usize },
    WorldPointPosition,
    WorldLineStart,
    WorldLineEnd,
    WorldPolylinePoint { index: usize },
    WorldBoxMin,
    WorldBoxMax,
    WorldEllipsoidCenter,
    WorldEllipsoidRadiusX,
    WorldEllipsoidRadiusY,
    WorldEllipsoidRadiusZ,
    MeasurementStart,
    MeasurementEnd,
}

pub(crate) fn show_scene_artifacts_editor(
    ui: &mut egui::Ui,
    analysis: &mut CurrentAnalysisRuntime,
    ui_runtime: &mut CurrentUiRuntime,
) -> anyhow::Result<bool> {
    let mut changed = ui
        .horizontal_wrapped(|ui| -> anyhow::Result<bool> {
            let mut changed = false;
            if ui_kit::toolbar_button(ui, "Undo", analysis.scene_artifacts.can_undo()).clicked() {
                analysis.scene_artifacts.undo()?;
                changed = true;
            }
            if ui_kit::toolbar_button(ui, "Redo", analysis.scene_artifacts.can_redo()).clicked() {
                analysis.scene_artifacts.redo()?;
                changed = true;
            }
            Ok(changed)
        })
        .inner?;

    let tracks = analysis
        .scene_artifacts
        .tracks()
        .cloned()
        .collect::<Vec<_>>();
    let rois = analysis.scene_artifacts.rois().cloned().collect::<Vec<_>>();
    let annotations = analysis
        .scene_artifacts
        .annotations()
        .cloned()
        .collect::<Vec<_>>();
    let measurements = analysis
        .scene_artifacts
        .measurements()
        .cloned()
        .collect::<Vec<_>>();
    ui_kit::property_row(ui, "tracks", tracks.len());
    ui_kit::property_row(ui, "rois", rois.len());
    ui_kit::property_row(ui, "notes", annotations.len());
    ui_kit::property_row(ui, "measure", measurements.len());
    ui_kit::property_row(ui, "revision", analysis.scene_artifacts.revision());

    if tracks.is_empty() && rois.is_empty() && annotations.is_empty() && measurements.is_empty() {
        ui_kit::status_badge(ui, StatusTone::Ready, "no editable artifacts");
        return Ok(changed);
    }

    let rows_changed = egui::ScrollArea::vertical()
        .id_salt("scene-artifact-editor-scroll")
        .max_height(220.0)
        .auto_shrink([false, false])
        .show(ui, |ui| -> anyhow::Result<bool> {
            let mut changed = false;
            for track in tracks {
                changed |= show_scene_track_artifact_row(ui, analysis, ui_runtime, track)?;
            }
            for roi in rois {
                changed |= show_scene_roi_artifact_row(ui, analysis, ui_runtime, roi)?;
            }
            for annotation in annotations {
                changed |=
                    show_scene_annotation_artifact_row(ui, analysis, ui_runtime, annotation)?;
            }
            for measurement in measurements {
                changed |=
                    show_scene_measurement_artifact_row(ui, analysis, ui_runtime, measurement)?;
            }
            Ok(changed)
        })
        .inner?;
    changed |= rows_changed;
    Ok(changed)
}

fn show_scene_roi_artifact_row(
    ui: &mut egui::Ui,
    analysis: &mut CurrentAnalysisRuntime,
    ui_runtime: &mut CurrentUiRuntime,
    artifact: RoiArtifact,
) -> anyhow::Result<bool> {
    let mut name = artifact.name.clone();
    let mut visible = artifact.visible;
    let mut color = artifact.style.color_rgba;
    let mut select_clicked = false;
    let mut remove_clicked = false;
    let selected =
        selected_scene_artifact_matches(ui_runtime, EditableSceneArtifactKind::Roi, &artifact.id);

    ui.horizontal_wrapped(|ui| {
        if ui
            .selectable_label(selected, format!("ROI {}", artifact.id.as_str()))
            .clicked()
        {
            select_clicked = true;
        }
        ui.checkbox(&mut visible, "visible");
        ui.color_edit_button_rgba_unmultiplied(&mut color);
        ui.add(egui::TextEdit::singleline(&mut name).desired_width(120.0));
        if ui_kit::toolbar_button(ui, "Remove", true).clicked() {
            remove_clicked = true;
        }
    });

    if select_clicked {
        select_scene_artifact(ui_runtime, EditableSceneArtifactKind::Roi, &artifact.id);
    }
    if remove_clicked {
        return remove_scene_artifact(
            analysis,
            ui_runtime,
            EditableSceneArtifactKind::Roi,
            &artifact.id,
        );
    }

    let mut updated = artifact.clone();
    updated.name = validated_scene_artifact_name(&name)?;
    updated.visible = visible;
    updated.style = AnalysisSceneStyleRgba::new(color)?;
    if selected {
        show_roi_geometry_controls(ui, &mut updated)?;
    }
    if updated != artifact {
        return update_scene_roi_artifact(analysis, updated);
    }
    Ok(false)
}

fn show_scene_track_artifact_row(
    ui: &mut egui::Ui,
    analysis: &mut CurrentAnalysisRuntime,
    ui_runtime: &mut CurrentUiRuntime,
    artifact: TrackArtifact,
) -> anyhow::Result<bool> {
    let mut name = artifact.name.clone();
    let mut visible = artifact.visible;
    let mut color = artifact.style.color_rgba;
    let mut select_clicked = false;
    let mut remove_clicked = false;
    let selected =
        selected_scene_artifact_matches(ui_runtime, EditableSceneArtifactKind::Track, &artifact.id);

    ui.horizontal_wrapped(|ui| {
        if ui
            .selectable_label(selected, format!("Track {}", artifact.id.as_str()))
            .clicked()
        {
            select_clicked = true;
        }
        ui.label(format!("{} point(s)", artifact.points.len()));
        ui.checkbox(&mut visible, "visible");
        ui.color_edit_button_rgba_unmultiplied(&mut color);
        ui.add(egui::TextEdit::singleline(&mut name).desired_width(120.0));
        if ui_kit::toolbar_button(ui, "Remove", true).clicked() {
            remove_clicked = true;
        }
    });

    if select_clicked {
        select_scene_artifact(ui_runtime, EditableSceneArtifactKind::Track, &artifact.id);
    }
    if remove_clicked {
        return remove_scene_artifact(
            analysis,
            ui_runtime,
            EditableSceneArtifactKind::Track,
            &artifact.id,
        );
    }

    let mut updated = artifact.clone();
    updated.name = validated_scene_artifact_name(&name)?;
    updated.visible = visible;
    updated.style = AnalysisSceneStyleRgba::new(color)?;
    if selected {
        show_track_points_controls(ui, &mut updated);
    }
    if updated != artifact {
        return update_scene_track_artifact(analysis, updated);
    }
    Ok(false)
}

fn show_scene_annotation_artifact_row(
    ui: &mut egui::Ui,
    analysis: &mut CurrentAnalysisRuntime,
    ui_runtime: &mut CurrentUiRuntime,
    artifact: AnnotationArtifact,
) -> anyhow::Result<bool> {
    let mut name = artifact.name.clone();
    let mut text = artifact.text.clone().unwrap_or_default();
    let mut visible = artifact.visible;
    let mut color = artifact.style.color_rgba;
    let mut select_clicked = false;
    let mut remove_clicked = false;
    let selected = selected_scene_artifact_matches(
        ui_runtime,
        EditableSceneArtifactKind::Annotation,
        &artifact.id,
    );

    ui.horizontal_wrapped(|ui| {
        if ui
            .selectable_label(selected, format!("Note {}", artifact.id.as_str()))
            .clicked()
        {
            select_clicked = true;
        }
        ui.checkbox(&mut visible, "visible");
        ui.color_edit_button_rgba_unmultiplied(&mut color);
        ui.add(egui::TextEdit::singleline(&mut name).desired_width(120.0));
        ui.add(egui::TextEdit::singleline(&mut text).desired_width(160.0));
        if ui_kit::toolbar_button(ui, "Remove", true).clicked() {
            remove_clicked = true;
        }
    });

    if select_clicked {
        select_scene_artifact(
            ui_runtime,
            EditableSceneArtifactKind::Annotation,
            &artifact.id,
        );
    }
    if remove_clicked {
        return remove_scene_artifact(
            analysis,
            ui_runtime,
            EditableSceneArtifactKind::Annotation,
            &artifact.id,
        );
    }

    let mut updated = artifact.clone();
    updated.name = validated_scene_artifact_name(&name)?;
    updated.visible = visible;
    updated.style = AnalysisSceneStyleRgba::new(color)?;
    updated.text = if text.trim().is_empty() {
        None
    } else {
        Some(text.trim().to_owned())
    };
    if selected {
        show_world_geometry_controls(
            ui,
            ("annotation-geometry", updated.id.as_str()),
            &mut updated.geometry,
        )?;
    }
    if updated != artifact {
        return update_scene_annotation_artifact(analysis, updated);
    }
    Ok(false)
}

fn show_scene_measurement_artifact_row(
    ui: &mut egui::Ui,
    analysis: &mut CurrentAnalysisRuntime,
    ui_runtime: &mut CurrentUiRuntime,
    artifact: MeasurementArtifact,
) -> anyhow::Result<bool> {
    let mut name = artifact.name.clone();
    let mut visible = artifact.visible;
    let mut color = artifact.style.color_rgba;
    let mut select_clicked = false;
    let mut remove_clicked = false;
    let selected = selected_scene_artifact_matches(
        ui_runtime,
        EditableSceneArtifactKind::Measurement,
        &artifact.id,
    );
    let result = artifact
        .result
        .as_ref()
        .map(|result| format!("{:.3} {}", result.value, result.unit))
        .unwrap_or_else(|| "pending".to_owned());

    ui.horizontal_wrapped(|ui| {
        if ui
            .selectable_label(selected, format!("Measure {}", artifact.id.as_str()))
            .clicked()
        {
            select_clicked = true;
        }
        ui.label(result);
        ui.checkbox(&mut visible, "visible");
        ui.color_edit_button_rgba_unmultiplied(&mut color);
        ui.add(egui::TextEdit::singleline(&mut name).desired_width(120.0));
        if ui_kit::toolbar_button(ui, "Remove", true).clicked() {
            remove_clicked = true;
        }
    });

    if select_clicked {
        select_scene_artifact(
            ui_runtime,
            EditableSceneArtifactKind::Measurement,
            &artifact.id,
        );
    }
    if remove_clicked {
        return remove_scene_artifact(
            analysis,
            ui_runtime,
            EditableSceneArtifactKind::Measurement,
            &artifact.id,
        );
    }

    let mut updated = artifact.clone();
    updated.name = validated_scene_artifact_name(&name)?;
    updated.visible = visible;
    updated.style = AnalysisSceneStyleRgba::new(color)?;
    if selected {
        show_measurement_geometry_controls(ui, &mut updated)?;
    }
    if updated != artifact {
        return update_scene_measurement_artifact(analysis, updated);
    }
    Ok(false)
}

fn show_roi_geometry_controls(ui: &mut egui::Ui, artifact: &mut RoiArtifact) -> anyhow::Result<()> {
    show_world_geometry_controls(
        ui,
        ("roi-geometry", artifact.id.as_str()),
        &mut artifact.geometry,
    )
}

fn show_measurement_geometry_controls(
    ui: &mut egui::Ui,
    artifact: &mut MeasurementArtifact,
) -> anyhow::Result<()> {
    let (updated_start, updated_end) = match &mut artifact.geometry {
        AnalysisMeasurementGeometry::Distance { start, end } => {
            ui.indent(("measurement-geometry", artifact.id.as_str()), |ui| {
                ui.label("geometry");
                scene_vec3_drag_row(ui, "start", start);
                scene_vec3_drag_row(ui, "end", end);
            });
            (*start, *end)
        }
    };
    artifact.geometry = AnalysisMeasurementGeometry::distance(updated_start, updated_end)?;
    refresh_measurement_result(artifact);
    Ok(())
}

fn show_track_points_controls(ui: &mut egui::Ui, artifact: &mut TrackArtifact) {
    ui.indent(("track-points", artifact.id.as_str()), |ui| {
        ui.label("points");
        for point in &mut artifact.points {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("t{}", point.timepoint.get()));
                ui.add(
                    egui::DragValue::new(&mut point.position_world.x)
                        .speed(0.05)
                        .prefix("x "),
                );
                ui.add(
                    egui::DragValue::new(&mut point.position_world.y)
                        .speed(0.05)
                        .prefix("y "),
                );
                ui.add(
                    egui::DragValue::new(&mut point.position_world.z)
                        .speed(0.05)
                        .prefix("z "),
                );
            });
        }
    });
}

fn show_world_geometry_controls(
    ui: &mut egui::Ui,
    id_salt: impl Hash,
    geometry: &mut AnalysisWorldGeometry,
) -> anyhow::Result<()> {
    ui.indent(id_salt, |ui| {
        ui.label("geometry");
        match geometry {
            AnalysisWorldGeometry::Point {
                position,
                radius_px,
            } => {
                scene_vec3_drag_row(ui, "position", position);
                ui.horizontal_wrapped(|ui| {
                    ui.label("radius");
                    ui.add(
                        egui::DragValue::new(radius_px)
                            .speed(0.25)
                            .range(0.0..=f32::MAX)
                            .prefix("px "),
                    );
                });
            }
            AnalysisWorldGeometry::LineSegment {
                start,
                end,
                width_px,
            } => {
                scene_vec3_drag_row(ui, "start", start);
                scene_vec3_drag_row(ui, "end", end);
                ui.horizontal_wrapped(|ui| {
                    ui.label("width");
                    ui.add(
                        egui::DragValue::new(width_px)
                            .speed(0.25)
                            .range(0.0..=f32::MAX)
                            .prefix("px "),
                    );
                });
            }
            AnalysisWorldGeometry::Polyline { points, width_px } => {
                for (index, point) in points.iter_mut().enumerate() {
                    scene_vec3_drag_row(ui, &format!("p{index}"), point);
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label("width");
                    ui.add(
                        egui::DragValue::new(width_px)
                            .speed(0.25)
                            .range(0.0..=f32::MAX)
                            .prefix("px "),
                    );
                });
            }
            AnalysisWorldGeometry::Box3D { min, max } => {
                scene_vec3_drag_row(ui, "min", min);
                scene_vec3_drag_row(ui, "max", max);
            }
            AnalysisWorldGeometry::Ellipsoid { center, radii } => {
                scene_vec3_drag_row(ui, "center", center);
                scene_vec3_drag_row(ui, "radii", radii);
            }
        }
    });
    normalize_world_geometry(geometry)?;
    Ok(())
}

pub(crate) fn normalize_world_geometry(geometry: &mut AnalysisWorldGeometry) -> anyhow::Result<()> {
    match geometry {
        AnalysisWorldGeometry::Box3D { min, max } => {
            let normalized_min = min.min(*max);
            let normalized_max = min.max(*max);
            *min = normalized_min;
            *max = normalized_max;
        }
        AnalysisWorldGeometry::Ellipsoid { radii, .. } => {
            const MIN_RADIUS: f64 = 1.0e-6;
            radii.x = radii.x.max(MIN_RADIUS);
            radii.y = radii.y.max(MIN_RADIUS);
            radii.z = radii.z.max(MIN_RADIUS);
        }
        AnalysisWorldGeometry::Point { radius_px, .. } => {
            *radius_px = (*radius_px).max(0.0);
        }
        AnalysisWorldGeometry::LineSegment { width_px, .. }
        | AnalysisWorldGeometry::Polyline { width_px, .. } => {
            *width_px = (*width_px).max(0.0);
        }
    }
    geometry.validate()?;
    Ok(())
}

pub(crate) fn refresh_measurement_result(artifact: &mut MeasurementArtifact) {
    artifact.result = Some(MeasurementResult {
        value: artifact.geometry.distance_world(),
        unit: "world_unit".to_owned(),
        description: "distance".to_owned(),
    });
}

pub(crate) fn scene_vec3_drag_row(ui: &mut egui::Ui, label: &str, value: &mut DVec3) {
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        ui.add(egui::DragValue::new(&mut value.x).speed(0.05).prefix("x "));
        ui.add(egui::DragValue::new(&mut value.y).speed(0.05).prefix("y "));
        ui.add(egui::DragValue::new(&mut value.z).speed(0.05).prefix("z "));
    });
}

fn validated_scene_artifact_name(name: &str) -> anyhow::Result<String> {
    let name = name.trim();
    if name.is_empty() {
        anyhow::bail!("scene artifact name must not be empty");
    }
    Ok(name.to_owned())
}

pub(crate) fn update_scene_roi_artifact(
    analysis: &mut CurrentAnalysisRuntime,
    artifact: RoiArtifact,
) -> anyhow::Result<bool> {
    analysis
        .scene_artifacts
        .apply(SceneEditCommand::PutRoi { artifact })?;
    Ok(true)
}

pub(crate) fn update_scene_track_artifact(
    analysis: &mut CurrentAnalysisRuntime,
    artifact: TrackArtifact,
) -> anyhow::Result<bool> {
    analysis
        .scene_artifacts
        .apply(SceneEditCommand::PutTrack { artifact })?;
    Ok(true)
}

pub(crate) fn update_scene_annotation_artifact(
    analysis: &mut CurrentAnalysisRuntime,
    artifact: AnnotationArtifact,
) -> anyhow::Result<bool> {
    analysis
        .scene_artifacts
        .apply(SceneEditCommand::PutAnnotation { artifact })?;
    Ok(true)
}

pub(crate) fn update_scene_measurement_artifact(
    analysis: &mut CurrentAnalysisRuntime,
    artifact: MeasurementArtifact,
) -> anyhow::Result<bool> {
    analysis
        .scene_artifacts
        .apply(SceneEditCommand::PutMeasurement { artifact })?;
    Ok(true)
}

pub(crate) fn remove_scene_artifact(
    analysis: &mut CurrentAnalysisRuntime,
    ui_runtime: &mut CurrentUiRuntime,
    kind: EditableSceneArtifactKind,
    id: &SceneArtifactId,
) -> anyhow::Result<bool> {
    let command = match kind {
        EditableSceneArtifactKind::Track => SceneEditCommand::RemoveTrack { id: id.clone() },
        EditableSceneArtifactKind::Roi => SceneEditCommand::RemoveRoi { id: id.clone() },
        EditableSceneArtifactKind::Annotation => {
            SceneEditCommand::RemoveAnnotation { id: id.clone() }
        }
        EditableSceneArtifactKind::Measurement => {
            SceneEditCommand::RemoveMeasurement { id: id.clone() }
        }
    };
    analysis.scene_artifacts.apply(command)?;
    clear_scene_selection_if_matches(ui_runtime, kind, id);
    Ok(true)
}

pub(crate) fn select_scene_artifact(
    ui_runtime: &mut CurrentUiRuntime,
    kind: EditableSceneArtifactKind,
    id: &SceneArtifactId,
) {
    ui_runtime.viewer_tools.selection = Some(ToolSelection::SceneObject {
        kind: kind.pick_hit_kind(),
        object_id: id.as_str().to_owned(),
    });
}

fn clear_scene_selection_if_matches(
    ui_runtime: &mut CurrentUiRuntime,
    kind: EditableSceneArtifactKind,
    id: &SceneArtifactId,
) {
    if selected_scene_artifact_matches(ui_runtime, kind, id) {
        ui_runtime.viewer_tools.selection = None;
    }
}

pub(crate) fn selected_scene_artifact_matches(
    ui_runtime: &CurrentUiRuntime,
    kind: EditableSceneArtifactKind,
    id: &SceneArtifactId,
) -> bool {
    matches!(
        &ui_runtime.viewer_tools.selection,
        Some(ToolSelection::SceneObject {
            kind: selected_kind,
            object_id,
        }) if *selected_kind == kind.pick_hit_kind() && object_id == id.as_str()
    )
}

impl EditableSceneArtifactKind {
    pub(crate) fn pick_hit_kind(self) -> PickHitKind {
        match self {
            Self::Track => PickHitKind::Track,
            Self::Roi => PickHitKind::Roi,
            Self::Annotation => PickHitKind::Annotation,
            Self::Measurement => PickHitKind::Measurement,
        }
    }

    fn metadata_prefix(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Roi => "roi",
            Self::Annotation => "annotation",
            Self::Measurement => "measurement",
        }
    }

    fn from_metadata_prefix(value: &str) -> anyhow::Result<Self> {
        Ok(match value {
            "track" => Self::Track,
            "roi" => Self::Roi,
            "annotation" => Self::Annotation,
            "measurement" => Self::Measurement,
            _ => anyhow::bail!("unsupported scene handle artifact kind {value}"),
        })
    }
}

impl SceneEditHandle {
    fn metadata_suffix(&self) -> String {
        match self {
            Self::TrackPoint { index } => format!("{TRACK_POINT_HANDLE}:{index}"),
            Self::WorldPointPosition => WORLD_POINT_POSITION_HANDLE.to_owned(),
            Self::WorldLineStart => WORLD_LINE_START_HANDLE.to_owned(),
            Self::WorldLineEnd => WORLD_LINE_END_HANDLE.to_owned(),
            Self::WorldPolylinePoint { index } => format!("{WORLD_POLYLINE_POINT_HANDLE}:{index}"),
            Self::WorldBoxMin => ROI_BOX_MIN_HANDLE.to_owned(),
            Self::WorldBoxMax => ROI_BOX_MAX_HANDLE.to_owned(),
            Self::WorldEllipsoidCenter => WORLD_ELLIPSOID_CENTER_HANDLE.to_owned(),
            Self::WorldEllipsoidRadiusX => WORLD_ELLIPSOID_RADIUS_X_HANDLE.to_owned(),
            Self::WorldEllipsoidRadiusY => WORLD_ELLIPSOID_RADIUS_Y_HANDLE.to_owned(),
            Self::WorldEllipsoidRadiusZ => WORLD_ELLIPSOID_RADIUS_Z_HANDLE.to_owned(),
            Self::MeasurementStart => MEASUREMENT_START_HANDLE.to_owned(),
            Self::MeasurementEnd => MEASUREMENT_END_HANDLE.to_owned(),
        }
    }

    fn object_suffix(&self) -> String {
        self.metadata_suffix().replace(':', "_")
    }

    fn from_metadata_suffix(value: &str) -> anyhow::Result<Self> {
        if let Some(index) = value
            .strip_prefix(TRACK_POINT_HANDLE)
            .and_then(|remaining| remaining.strip_prefix(':'))
        {
            return Ok(Self::TrackPoint {
                index: parse_scene_handle_index(index, TRACK_POINT_HANDLE)?,
            });
        }
        if let Some(index) = value
            .strip_prefix(WORLD_POLYLINE_POINT_HANDLE)
            .and_then(|remaining| remaining.strip_prefix(':'))
        {
            return Ok(Self::WorldPolylinePoint {
                index: parse_scene_handle_index(index, WORLD_POLYLINE_POINT_HANDLE)?,
            });
        }
        Ok(match value {
            WORLD_POINT_POSITION_HANDLE => Self::WorldPointPosition,
            WORLD_LINE_START_HANDLE => Self::WorldLineStart,
            WORLD_LINE_END_HANDLE => Self::WorldLineEnd,
            ROI_BOX_MIN_HANDLE => Self::WorldBoxMin,
            ROI_BOX_MAX_HANDLE => Self::WorldBoxMax,
            WORLD_ELLIPSOID_CENTER_HANDLE => Self::WorldEllipsoidCenter,
            WORLD_ELLIPSOID_RADIUS_X_HANDLE => Self::WorldEllipsoidRadiusX,
            WORLD_ELLIPSOID_RADIUS_Y_HANDLE => Self::WorldEllipsoidRadiusY,
            WORLD_ELLIPSOID_RADIUS_Z_HANDLE => Self::WorldEllipsoidRadiusZ,
            MEASUREMENT_START_HANDLE => Self::MeasurementStart,
            MEASUREMENT_END_HANDLE => Self::MeasurementEnd,
            _ => anyhow::bail!("unsupported scene handle {value}"),
        })
    }
}

impl SceneEditHandleId {
    pub(crate) fn metadata_value(&self) -> String {
        format!(
            "{}:{}",
            self.artifact_kind.metadata_prefix(),
            self.handle.metadata_suffix()
        )
    }

    pub(crate) fn object_suffix(&self) -> String {
        format!(
            "{}_{}",
            self.artifact_kind.metadata_prefix(),
            self.handle.object_suffix()
        )
    }
}

pub(crate) fn scene_edit_handle_from_pick_hit(hit: &PickHit) -> anyhow::Result<SceneEditHandleId> {
    if hit.kind != PickHitKind::AnnotationHandle {
        anyhow::bail!("expected an annotation-handle pick hit");
    }
    let artifact_id = hit
        .object_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("scene handle pick is missing an object id"))?
        .as_str()
        .to_owned();
    let handle = match &hit.value {
        Some(PickValue::ObjectMetadata(handle)) => handle.as_str(),
        _ => anyhow::bail!("scene handle pick is missing handle metadata"),
    };
    let (artifact_kind, handle) = parse_scene_edit_handle_metadata(handle)?;
    Ok(SceneEditHandleId {
        artifact_kind,
        artifact_id,
        handle,
    })
}

fn parse_scene_edit_handle_metadata(
    value: &str,
) -> anyhow::Result<(EditableSceneArtifactKind, SceneEditHandle)> {
    let (kind, handle) = value
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("scene handle metadata must contain an artifact kind"))?;
    Ok((
        EditableSceneArtifactKind::from_metadata_prefix(kind)?,
        SceneEditHandle::from_metadata_suffix(handle)?,
    ))
}

fn parse_scene_handle_index(value: &str, handle: &'static str) -> anyhow::Result<usize> {
    value
        .parse::<usize>()
        .map_err(|err| anyhow::anyhow!("invalid {handle} index {value:?}: {err}"))
}

pub(crate) fn next_scene_artifact_id(
    artifacts: &SceneArtifactStore,
    kind: &'static str,
    prefix: &str,
) -> anyhow::Result<SceneArtifactId> {
    for offset in 1..=10_000 {
        let candidate = SceneArtifactId::new(
            kind,
            format!("{}-{}", prefix, artifacts.revision() + offset),
        )?;
        if !scene_artifact_id_exists(artifacts, kind, &candidate) {
            return Ok(candidate);
        }
    }
    anyhow::bail!("could not allocate a unique {kind} artifact id");
}

fn scene_artifact_id_exists(
    artifacts: &SceneArtifactStore,
    kind: &'static str,
    id: &SceneArtifactId,
) -> bool {
    match kind {
        "roi" => artifacts.roi(id).is_some(),
        "measurement" => artifacts
            .measurements()
            .any(|measurement| measurement.id == *id),
        "track" => artifacts.track(id).is_some(),
        "annotation" => artifacts
            .annotations()
            .any(|annotation| annotation.id == *id),
        _ => false,
    }
}
