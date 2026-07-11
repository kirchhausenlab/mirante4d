use std::collections::BTreeMap;

use glam::DVec3;
use mirante4d_domain::TimeIndex;
use mirante4d_format::LayerId;
use serde::{Deserialize, Serialize};

use crate::AnalysisError;

mod wire {
    use mirante4d_domain::TimeIndex;
    use mirante4d_format::LayerId;
    use serde::{Deserialize, Serialize};

    pub(super) mod time_index {
        use super::*;

        pub(crate) fn serialize<S>(value: &TimeIndex, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            value.get().serialize(serializer)
        }

        pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<TimeIndex, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            Ok(TimeIndex::new(u64::deserialize(deserializer)?))
        }
    }

    pub(super) mod optional_layer_id {
        use super::*;

        pub(crate) fn serialize<S>(
            value: &Option<LayerId>,
            serializer: S,
        ) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            value.as_ref().map(LayerId::as_str).serialize(serializer)
        }

        pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<Option<LayerId>, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            Option::<String>::deserialize(deserializer)?
                .map(LayerId::new)
                .transpose()
                .map_err(serde::de::Error::custom)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SceneArtifactId(String);

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SceneStyleRgba {
    pub color_rgba: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneArtifactTime {
    Static,
    Timepoint(#[serde(with = "wire::time_index")] TimeIndex),
    Interval {
        #[serde(with = "wire::time_index")]
        start: TimeIndex,
        #[serde(with = "wire::time_index")]
        end_exclusive: TimeIndex,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrackTrailWindow {
    pub before: u64,
    pub after: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackPoint {
    #[serde(with = "wire::time_index")]
    pub timepoint: TimeIndex,
    pub position_world: DVec3,
    #[serde(default)]
    pub attributes: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackArtifact {
    pub id: SceneArtifactId,
    pub name: String,
    #[serde(with = "wire::optional_layer_id")]
    pub source_layer_id: Option<LayerId>,
    pub points: Vec<TrackPoint>,
    pub style: SceneStyleRgba,
    pub visible: bool,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrackSegment {
    pub track_id: SceneArtifactId,
    pub start_timepoint: TimeIndex,
    pub end_timepoint: TimeIndex,
    pub start_world: DVec3,
    pub end_world: DVec3,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WorldBounds {
    pub min: DVec3,
    pub max: DVec3,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum WorldGeometry {
    Point {
        position: DVec3,
        radius_px: f32,
    },
    LineSegment {
        start: DVec3,
        end: DVec3,
        width_px: f32,
    },
    Polyline {
        points: Vec<DVec3>,
        width_px: f32,
    },
    Box3D {
        min: DVec3,
        max: DVec3,
    },
    Ellipsoid {
        center: DVec3,
        radii: DVec3,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoiArtifact {
    pub id: SceneArtifactId,
    pub name: String,
    pub geometry: WorldGeometry,
    pub time: SceneArtifactTime,
    pub style: SceneStyleRgba,
    pub visible: bool,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnnotationArtifact {
    pub id: SceneArtifactId,
    pub name: String,
    pub geometry: WorldGeometry,
    pub text: Option<String>,
    pub time: SceneArtifactTime,
    pub style: SceneStyleRgba,
    pub visible: bool,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum MeasurementGeometry {
    Distance { start: DVec3, end: DVec3 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeasurementResult {
    pub value: f64,
    pub unit: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeasurementProvenance {
    pub source: String,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeasurementArtifact {
    pub id: SceneArtifactId,
    pub name: String,
    pub geometry: MeasurementGeometry,
    pub result: Option<MeasurementResult>,
    pub provenance: MeasurementProvenance,
    pub time: SceneArtifactTime,
    pub style: SceneStyleRgba,
    pub visible: bool,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SceneEditCommand {
    PutTrack { artifact: TrackArtifact },
    RemoveTrack { id: SceneArtifactId },
    PutRoi { artifact: RoiArtifact },
    RemoveRoi { id: SceneArtifactId },
    PutAnnotation { artifact: AnnotationArtifact },
    RemoveAnnotation { id: SceneArtifactId },
    PutMeasurement { artifact: MeasurementArtifact },
    RemoveMeasurement { id: SceneArtifactId },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SceneArtifactStore {
    revision: u64,
    tracks: BTreeMap<String, TrackArtifact>,
    rois: BTreeMap<String, RoiArtifact>,
    annotations: BTreeMap<String, AnnotationArtifact>,
    measurements: BTreeMap<String, MeasurementArtifact>,
    #[serde(skip)]
    undo_stack: Vec<SceneEditCommand>,
    #[serde(skip)]
    redo_stack: Vec<SceneEditCommand>,
}

impl SceneArtifactId {
    pub fn new(kind: &'static str, value: impl Into<String>) -> Result<Self, AnalysisError> {
        let value = value.into();
        if value.is_empty()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(AnalysisError::InvalidSceneArtifactId { kind, value });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl SceneStyleRgba {
    pub fn new(color_rgba: [f32; 4]) -> Result<Self, AnalysisError> {
        if !color_rgba
            .iter()
            .all(|component| component.is_finite() && (0.0..=1.0).contains(component))
        {
            return Err(AnalysisError::InvalidSceneColor(color_rgba));
        }
        Ok(Self { color_rgba })
    }

    pub fn track_default() -> Self {
        Self::new([0.21, 0.83, 1.0, 1.0]).expect("constant color is valid")
    }

    pub fn roi_default() -> Self {
        Self::new([1.0, 0.72, 0.30, 1.0]).expect("constant color is valid")
    }

    pub fn annotation_default() -> Self {
        Self::new([1.0, 1.0, 1.0, 1.0]).expect("constant color is valid")
    }

    pub fn measurement_default() -> Self {
        Self::new([0.34, 0.83, 0.46, 1.0]).expect("constant color is valid")
    }
}

impl SceneArtifactTime {
    pub fn interval(start: TimeIndex, end_exclusive: TimeIndex) -> Result<Self, AnalysisError> {
        if start >= end_exclusive {
            return Err(AnalysisError::InvalidSceneTimeInterval {
                start: start.get(),
                end_exclusive: end_exclusive.get(),
            });
        }
        Ok(Self::Interval {
            start,
            end_exclusive,
        })
    }

    pub fn is_visible_at(self, timepoint: TimeIndex) -> bool {
        match self {
            Self::Static => true,
            Self::Timepoint(object_timepoint) => object_timepoint == timepoint,
            Self::Interval {
                start,
                end_exclusive,
            } => timepoint >= start && timepoint < end_exclusive,
        }
    }
}

impl TrackTrailWindow {
    pub const CURRENT_SEGMENT: Self = Self {
        before: 0,
        after: 0,
    };

    pub const fn new(before: u64, after: u64) -> Self {
        Self { before, after }
    }
}

impl TrackPoint {
    pub fn new(timepoint: TimeIndex, position_world: DVec3) -> Result<Self, AnalysisError> {
        validate_finite_vec3(position_world, "track point position")?;
        Ok(Self {
            timepoint,
            position_world,
            attributes: BTreeMap::new(),
        })
    }
}

impl TrackArtifact {
    pub fn new(
        id: SceneArtifactId,
        name: impl Into<String>,
        source_layer_id: Option<LayerId>,
        points: Vec<TrackPoint>,
    ) -> Result<Self, AnalysisError> {
        validate_track_points(id.as_str(), &points)?;
        Ok(Self {
            id,
            name: name.into(),
            source_layer_id,
            points,
            style: SceneStyleRgba::track_default(),
            visible: true,
            metadata: BTreeMap::new(),
        })
    }

    pub fn visible_segments(
        &self,
        timepoint: TimeIndex,
        trail: TrackTrailWindow,
    ) -> Vec<TrackSegment> {
        if !self.visible || self.points.len() < 2 {
            return Vec::new();
        }
        let start_time = timepoint.get().saturating_sub(trail.before);
        let end_time = timepoint.get().saturating_add(trail.after);
        self.points
            .windows(2)
            .filter_map(|pair| {
                let start = &pair[0];
                let end = &pair[1];
                let segment_start = start.timepoint.get();
                let segment_end = end.timepoint.get();
                let overlaps = segment_start <= end_time
                    && segment_end >= start_time
                    && segment_start < segment_end;
                overlaps.then(|| TrackSegment {
                    track_id: self.id.clone(),
                    start_timepoint: start.timepoint,
                    end_timepoint: end.timepoint,
                    start_world: start.position_world,
                    end_world: end.position_world,
                })
            })
            .collect()
    }
}

impl WorldGeometry {
    pub fn validate(&self) -> Result<(), AnalysisError> {
        match self {
            Self::Point {
                position,
                radius_px,
            } => {
                validate_finite_vec3(*position, "point position")?;
                validate_nonnegative_finite(*radius_px, "point radius")
            }
            Self::LineSegment {
                start,
                end,
                width_px,
            } => {
                validate_finite_vec3(*start, "line start")?;
                validate_finite_vec3(*end, "line end")?;
                validate_nonnegative_finite(*width_px, "line width")
            }
            Self::Polyline { points, width_px } => {
                if points.len() < 2 {
                    return Err(AnalysisError::InvalidSceneGeometry(
                        "polyline requires at least two points",
                    ));
                }
                for point in points {
                    validate_finite_vec3(*point, "polyline point")?;
                }
                validate_nonnegative_finite(*width_px, "polyline width")
            }
            Self::Box3D { min, max } => {
                validate_finite_vec3(*min, "box min")?;
                validate_finite_vec3(*max, "box max")?;
                if min.x > max.x || min.y > max.y || min.z > max.z {
                    return Err(AnalysisError::InvalidSceneGeometry(
                        "box min must be <= max on every axis",
                    ));
                }
                Ok(())
            }
            Self::Ellipsoid { center, radii } => {
                validate_finite_vec3(*center, "ellipsoid center")?;
                validate_finite_vec3(*radii, "ellipsoid radii")?;
                if radii.x <= 0.0 || radii.y <= 0.0 || radii.z <= 0.0 {
                    return Err(AnalysisError::InvalidSceneGeometry(
                        "ellipsoid radii must be positive",
                    ));
                }
                Ok(())
            }
        }
    }

    pub fn world_bounds(&self) -> Result<WorldBounds, AnalysisError> {
        self.validate()?;
        let bounds = match self {
            Self::Point { position, .. } => WorldBounds {
                min: *position,
                max: *position,
            },
            Self::LineSegment { start, end, .. } => WorldBounds::from_points([*start, *end]),
            Self::Polyline { points, .. } => WorldBounds::from_points(points.iter().copied()),
            Self::Box3D { min, max } => WorldBounds {
                min: *min,
                max: *max,
            },
            Self::Ellipsoid { center, radii } => WorldBounds {
                min: *center - *radii,
                max: *center + *radii,
            },
        };
        Ok(bounds)
    }

    pub fn world_points(&self) -> Vec<DVec3> {
        match self {
            Self::Point { position, .. } => vec![*position],
            Self::LineSegment { start, end, .. } => vec![*start, *end],
            Self::Polyline { points, .. } => points.clone(),
            Self::Box3D { min, max } => vec![*min, *max],
            Self::Ellipsoid { center, .. } => vec![*center],
        }
    }
}

impl WorldBounds {
    pub fn from_points(points: impl IntoIterator<Item = DVec3>) -> Self {
        let mut iterator = points.into_iter();
        let first = iterator.next().expect("bounds require at least one point");
        let mut min = first;
        let mut max = first;
        for point in iterator {
            min = min.min(point);
            max = max.max(point);
        }
        Self { min, max }
    }
}

impl RoiArtifact {
    pub fn new(
        id: SceneArtifactId,
        name: impl Into<String>,
        geometry: WorldGeometry,
        time: SceneArtifactTime,
    ) -> Result<Self, AnalysisError> {
        geometry.validate()?;
        Ok(Self {
            id,
            name: name.into(),
            geometry,
            time,
            style: SceneStyleRgba::roi_default(),
            visible: true,
            metadata: BTreeMap::new(),
        })
    }

    pub fn world_bounds(&self) -> Result<WorldBounds, AnalysisError> {
        self.geometry.world_bounds()
    }
}

impl AnnotationArtifact {
    pub fn new(
        id: SceneArtifactId,
        name: impl Into<String>,
        geometry: WorldGeometry,
        text: Option<String>,
        time: SceneArtifactTime,
    ) -> Result<Self, AnalysisError> {
        geometry.validate()?;
        Ok(Self {
            id,
            name: name.into(),
            geometry,
            text,
            time,
            style: SceneStyleRgba::annotation_default(),
            visible: true,
            metadata: BTreeMap::new(),
        })
    }
}

impl MeasurementGeometry {
    pub fn distance(start: DVec3, end: DVec3) -> Result<Self, AnalysisError> {
        validate_finite_vec3(start, "measurement start")?;
        validate_finite_vec3(end, "measurement end")?;
        Ok(Self::Distance { start, end })
    }

    pub fn world_bounds(&self) -> WorldBounds {
        match self {
            Self::Distance { start, end } => WorldBounds::from_points([*start, *end]),
        }
    }

    pub fn distance_world(&self) -> f64 {
        match self {
            Self::Distance { start, end } => start.distance(*end),
        }
    }
}

impl MeasurementArtifact {
    pub fn distance(
        id: SceneArtifactId,
        name: impl Into<String>,
        start: DVec3,
        end: DVec3,
        provenance: MeasurementProvenance,
        time: SceneArtifactTime,
    ) -> Result<Self, AnalysisError> {
        let geometry = MeasurementGeometry::distance(start, end)?;
        let result = MeasurementResult {
            value: geometry.distance_world(),
            unit: "world_unit".to_owned(),
            description: "distance".to_owned(),
        };
        Ok(Self {
            id,
            name: name.into(),
            geometry,
            result: Some(result),
            provenance,
            time,
            style: SceneStyleRgba::measurement_default(),
            visible: true,
            metadata: BTreeMap::new(),
        })
    }
}

impl SceneArtifactStore {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn tracks(&self) -> impl Iterator<Item = &TrackArtifact> {
        self.tracks.values()
    }

    pub fn rois(&self) -> impl Iterator<Item = &RoiArtifact> {
        self.rois.values()
    }

    pub fn annotations(&self) -> impl Iterator<Item = &AnnotationArtifact> {
        self.annotations.values()
    }

    pub fn measurements(&self) -> impl Iterator<Item = &MeasurementArtifact> {
        self.measurements.values()
    }

    pub fn track(&self, id: &SceneArtifactId) -> Option<&TrackArtifact> {
        self.tracks.get(id.as_str())
    }

    pub fn roi(&self, id: &SceneArtifactId) -> Option<&RoiArtifact> {
        self.rois.get(id.as_str())
    }

    pub fn annotation(&self, id: &SceneArtifactId) -> Option<&AnnotationArtifact> {
        self.annotations.get(id.as_str())
    }

    pub fn measurement(&self, id: &SceneArtifactId) -> Option<&MeasurementArtifact> {
        self.measurements.get(id.as_str())
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn apply(&mut self, command: SceneEditCommand) -> Result<(), AnalysisError> {
        let inverse = self.apply_without_record(command)?;
        self.undo_stack.push(inverse);
        self.redo_stack.clear();
        self.revision += 1;
        Ok(())
    }

    pub fn undo(&mut self) -> Result<(), AnalysisError> {
        let command = self
            .undo_stack
            .pop()
            .ok_or(AnalysisError::UndoUnavailable)?;
        let redo = self.apply_without_record(command)?;
        self.redo_stack.push(redo);
        self.revision += 1;
        Ok(())
    }

    pub fn redo(&mut self) -> Result<(), AnalysisError> {
        let command = self
            .redo_stack
            .pop()
            .ok_or(AnalysisError::RedoUnavailable)?;
        let undo = self.apply_without_record(command)?;
        self.undo_stack.push(undo);
        self.revision += 1;
        Ok(())
    }

    pub fn visible_track_segments(
        &self,
        timepoint: TimeIndex,
        trail: TrackTrailWindow,
    ) -> Vec<TrackSegment> {
        self.tracks()
            .flat_map(|track| track.visible_segments(timepoint, trail))
            .collect()
    }

    fn apply_without_record(
        &mut self,
        command: SceneEditCommand,
    ) -> Result<SceneEditCommand, AnalysisError> {
        match command {
            SceneEditCommand::PutTrack { artifact } => {
                validate_track_artifact(&artifact)?;
                let id = artifact.id.as_str().to_owned();
                let inverse = self.tracks.insert(id.clone(), artifact).map_or(
                    SceneEditCommand::RemoveTrack {
                        id: SceneArtifactId(id),
                    },
                    |previous| SceneEditCommand::PutTrack { artifact: previous },
                );
                Ok(inverse)
            }
            SceneEditCommand::RemoveTrack { id } => {
                let previous = self.tracks.remove(id.as_str()).ok_or_else(|| {
                    AnalysisError::MissingSceneArtifact {
                        kind: "track",
                        id: id.as_str().to_owned(),
                    }
                })?;
                Ok(SceneEditCommand::PutTrack { artifact: previous })
            }
            SceneEditCommand::PutRoi { artifact } => {
                artifact.geometry.validate()?;
                let id = artifact.id.as_str().to_owned();
                let inverse = self.rois.insert(id.clone(), artifact).map_or(
                    SceneEditCommand::RemoveRoi {
                        id: SceneArtifactId(id),
                    },
                    |previous| SceneEditCommand::PutRoi { artifact: previous },
                );
                Ok(inverse)
            }
            SceneEditCommand::RemoveRoi { id } => {
                let previous = self.rois.remove(id.as_str()).ok_or_else(|| {
                    AnalysisError::MissingSceneArtifact {
                        kind: "roi",
                        id: id.as_str().to_owned(),
                    }
                })?;
                Ok(SceneEditCommand::PutRoi { artifact: previous })
            }
            SceneEditCommand::PutAnnotation { artifact } => {
                artifact.geometry.validate()?;
                let id = artifact.id.as_str().to_owned();
                let inverse = self.annotations.insert(id.clone(), artifact).map_or(
                    SceneEditCommand::RemoveAnnotation {
                        id: SceneArtifactId(id),
                    },
                    |previous| SceneEditCommand::PutAnnotation { artifact: previous },
                );
                Ok(inverse)
            }
            SceneEditCommand::RemoveAnnotation { id } => {
                let previous = self.annotations.remove(id.as_str()).ok_or_else(|| {
                    AnalysisError::MissingSceneArtifact {
                        kind: "annotation",
                        id: id.as_str().to_owned(),
                    }
                })?;
                Ok(SceneEditCommand::PutAnnotation { artifact: previous })
            }
            SceneEditCommand::PutMeasurement { artifact } => {
                validate_measurement_artifact(&artifact)?;
                let id = artifact.id.as_str().to_owned();
                let inverse = self.measurements.insert(id.clone(), artifact).map_or(
                    SceneEditCommand::RemoveMeasurement {
                        id: SceneArtifactId(id),
                    },
                    |previous| SceneEditCommand::PutMeasurement { artifact: previous },
                );
                Ok(inverse)
            }
            SceneEditCommand::RemoveMeasurement { id } => {
                let previous = self.measurements.remove(id.as_str()).ok_or_else(|| {
                    AnalysisError::MissingSceneArtifact {
                        kind: "measurement",
                        id: id.as_str().to_owned(),
                    }
                })?;
                Ok(SceneEditCommand::PutMeasurement { artifact: previous })
            }
        }
    }
}

fn validate_track_artifact(track: &TrackArtifact) -> Result<(), AnalysisError> {
    validate_track_points(track.id.as_str(), &track.points)?;
    SceneStyleRgba::new(track.style.color_rgba)?;
    Ok(())
}

fn validate_track_points(id: &str, points: &[TrackPoint]) -> Result<(), AnalysisError> {
    if points.is_empty() {
        return Err(AnalysisError::EmptyTrackArtifact { id: id.to_owned() });
    }
    let mut previous = None;
    for point in points {
        validate_finite_vec3(point.position_world, "track point position")?;
        if let Some(previous) = previous
            && point.timepoint <= previous
        {
            return Err(AnalysisError::NonMonotonicTrackTimes { id: id.to_owned() });
        }
        previous = Some(point.timepoint);
    }
    Ok(())
}

fn validate_measurement_artifact(measurement: &MeasurementArtifact) -> Result<(), AnalysisError> {
    match measurement.geometry {
        MeasurementGeometry::Distance { start, end } => {
            validate_finite_vec3(start, "measurement start")?;
            validate_finite_vec3(end, "measurement end")?;
        }
    }
    SceneStyleRgba::new(measurement.style.color_rgba)?;
    if let Some(result) = &measurement.result
        && !result.value.is_finite()
    {
        return Err(AnalysisError::InvalidSceneGeometry(
            "measurement result value must be finite",
        ));
    }
    Ok(())
}

fn validate_finite_vec3(value: DVec3, reason: &'static str) -> Result<(), AnalysisError> {
    if value.x.is_finite() && value.y.is_finite() && value.z.is_finite() {
        Ok(())
    } else {
        Err(AnalysisError::InvalidSceneGeometry(reason))
    }
}

fn validate_nonnegative_finite(value: f32, reason: &'static str) -> Result<(), AnalysisError> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(AnalysisError::InvalidSceneGeometry(reason))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_artifact_ids_are_strict_ascii() {
        assert_eq!(
            SceneArtifactId::new("roi", "roi_001").unwrap().as_str(),
            "roi_001"
        );
        assert!(SceneArtifactId::new("roi", "").is_err());
        assert!(SceneArtifactId::new("roi", "bad µ").is_err());
    }

    #[test]
    fn track_artifact_requires_strictly_increasing_timepoints() {
        let id = SceneArtifactId::new("track", "track-a").unwrap();
        let points = vec![
            TrackPoint::new(TimeIndex::new(0), DVec3::ZERO).unwrap(),
            TrackPoint::new(TimeIndex::new(0), DVec3::X).unwrap(),
        ];

        let err = TrackArtifact::new(id, "bad track", None, points).unwrap_err();

        assert!(matches!(err, AnalysisError::NonMonotonicTrackTimes { .. }));
    }

    #[test]
    fn track_segment_extraction_is_time_window_aware() {
        let track = sample_track();

        let exact = track.visible_segments(TimeIndex::new(2), TrackTrailWindow::CURRENT_SEGMENT);
        let windowed = track.visible_segments(TimeIndex::new(2), TrackTrailWindow::new(2, 2));

        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].start_timepoint, TimeIndex::new(1));
        assert_eq!(exact[0].end_timepoint, TimeIndex::new(3));
        assert_eq!(windowed.len(), 2);
    }

    #[test]
    fn roi_geometry_exposes_world_bounds_for_analysis() {
        let roi = RoiArtifact::new(
            SceneArtifactId::new("roi", "box").unwrap(),
            "box",
            WorldGeometry::Box3D {
                min: DVec3::new(-1.0, 2.0, 3.0),
                max: DVec3::new(4.0, 5.0, 6.0),
            },
            SceneArtifactTime::Static,
        )
        .unwrap();

        let bounds = roi.world_bounds().unwrap();

        assert_eq!(bounds.min, DVec3::new(-1.0, 2.0, 3.0));
        assert_eq!(bounds.max, DVec3::new(4.0, 5.0, 6.0));
        assert_eq!(roi.geometry.world_points(), vec![bounds.min, bounds.max]);
    }

    #[test]
    fn annotation_geometry_exposes_world_points_for_analysis() {
        let annotation = AnnotationArtifact::new(
            SceneArtifactId::new("annotation", "path").unwrap(),
            "path",
            WorldGeometry::Polyline {
                points: vec![DVec3::ZERO, DVec3::X, DVec3::Y],
                width_px: 2.0,
            },
            Some("profile path".to_owned()),
            SceneArtifactTime::Static,
        )
        .unwrap();

        assert_eq!(
            annotation.geometry.world_points(),
            vec![DVec3::ZERO, DVec3::X, DVec3::Y]
        );
        assert_eq!(
            annotation.geometry.world_bounds().unwrap(),
            WorldBounds {
                min: DVec3::ZERO,
                max: DVec3::new(1.0, 1.0, 0.0),
            }
        );
    }

    #[test]
    fn measurement_distance_records_result_and_provenance() {
        let measurement = MeasurementArtifact::distance(
            SceneArtifactId::new("measurement", "distance").unwrap(),
            "distance",
            DVec3::ZERO,
            DVec3::new(3.0, 4.0, 0.0),
            MeasurementProvenance {
                source: "manual".to_owned(),
                scope: "world".to_owned(),
            },
            SceneArtifactTime::Static,
        )
        .unwrap();

        assert_eq!(measurement.result.as_ref().unwrap().value, 5.0);
        assert_eq!(measurement.provenance.source, "manual");
    }

    #[test]
    fn scene_artifact_store_applies_undoes_and_redoes_commands() {
        let mut store = SceneArtifactStore::default();
        let track = sample_track();
        let id = track.id.clone();

        store
            .apply(SceneEditCommand::PutTrack {
                artifact: track.clone(),
            })
            .unwrap();
        assert_eq!(store.revision(), 1);
        assert!(store.track(&id).is_some());

        store.undo().unwrap();
        assert_eq!(store.revision(), 2);
        assert!(store.track(&id).is_none());

        store.redo().unwrap();
        assert_eq!(store.revision(), 3);
        assert_eq!(store.track(&id), Some(&track));
    }

    #[test]
    fn scene_artifact_store_serializes_only_durable_state() {
        let mut store = SceneArtifactStore::default();
        store
            .apply(SceneEditCommand::PutTrack {
                artifact: sample_track(),
            })
            .unwrap();

        let encoded = serde_json::to_string(&store).unwrap();
        let mut decoded: SceneArtifactStore = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded.revision(), 1);
        assert_eq!(decoded.tracks().count(), 1);
        assert!(decoded.undo().is_err());
    }

    fn sample_track() -> TrackArtifact {
        TrackArtifact::new(
            SceneArtifactId::new("track", "track-a").unwrap(),
            "track a",
            Some(LayerId::new("ch0").unwrap()),
            vec![
                TrackPoint::new(TimeIndex::new(0), DVec3::ZERO).unwrap(),
                TrackPoint::new(TimeIndex::new(1), DVec3::X).unwrap(),
                TrackPoint::new(TimeIndex::new(3), DVec3::new(3.0, 0.0, 0.0)).unwrap(),
            ],
        )
        .unwrap()
    }
}
