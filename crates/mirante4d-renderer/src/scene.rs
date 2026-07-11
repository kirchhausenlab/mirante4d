use std::fmt;

use glam::{DMat4, DVec3};
use mirante4d_core::{GridToWorld, LayerId, TimeIndex};
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum SceneError {
    #[error("{kind} id must not be empty")]
    EmptyId { kind: &'static str },
    #[error("{kind} id must contain only ASCII letters, digits, '-' or '_', got {value:?}")]
    InvalidId { kind: &'static str, value: String },
    #[error("polyline geometry must contain at least two points")]
    EmptyPolyline,
    #[error("time interval must satisfy start < end, got start={start}, end={end_exclusive}")]
    InvalidTimeInterval { start: u64, end_exclusive: u64 },
    #[error("{kind} pick radius/width must be finite and nonnegative, got {value}")]
    InvalidPickRadius { kind: &'static str, value: f32 },
    #[error("pick rectangle bounds must be finite and ordered")]
    InvalidPickRectangle,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SceneLayerId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SceneObjectId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlaneId(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SceneLayerKind {
    Track,
    Annotation,
    Measurement,
    Interaction,
    Reference,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CoordinateSpace {
    World,
    Grid { layer_id: LayerId },
    Plane { plane_id: PlaneId },
    Screen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SceneTime {
    Static,
    Timepoint(TimeIndex),
    Interval {
        start: TimeIndex,
        end_exclusive: TimeIndex,
    },
    Trajectory {
        start: TimeIndex,
        end_exclusive: TimeIndex,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SceneRenderPass {
    WorldSpace,
    Interaction,
    ScreenSpace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcclusionPolicy {
    AlwaysOnTop,
    DepthTestGeometry,
    VolumeDepthCued,
    Xray,
    ScreenSpace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PickCompleteness {
    Exact,
    Approximate,
    Incomplete,
    Loading,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PickPolicy {
    SceneObject,
    FirstThresholdHit,
    MipArgmax,
    ProbeRay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PickHitKind {
    Voxel,
    Track,
    Roi,
    Annotation,
    AnnotationHandle,
    Measurement,
    Plane,
    Ui,
    Empty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SceneColorRgba {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SceneStyle {
    pub color: SceneColorRgba,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridPosition {
    pub z: f64,
    pub y: f64,
    pub x: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenPosition {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenRect {
    pub min: ScreenPosition,
    pub max: ScreenPosition,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PickValue {
    IntensityU8(u8),
    IntensityU16(u16),
    IntensityF32(f32),
    ObjectMetadata(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PickHit {
    pub kind: PickHitKind,
    pub layer_id: Option<SceneLayerId>,
    pub object_id: Option<SceneObjectId>,
    pub source_layer_id: Option<LayerId>,
    pub timepoint: TimeIndex,
    pub world_position: Option<DVec3>,
    pub grid_position: Option<GridPosition>,
    pub screen_position: Option<ScreenPosition>,
    pub value: Option<PickValue>,
    pub policy: PickPolicy,
    pub completeness: PickCompleteness,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VolumePickProbe {
    pub source_layer_id: LayerId,
    pub timepoint: TimeIndex,
    pub screen_position: ScreenPosition,
    pub world_position: Option<DVec3>,
    pub grid_position: Option<GridPosition>,
    pub policy: PickPolicy,
    pub completeness: PickCompleteness,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PickQuery {
    pub timepoint: TimeIndex,
    pub screen_position: ScreenPosition,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScenePickTarget {
    pub primitive: PickPrimitive,
    pub hit: PickHit,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PickPrimitive {
    Point {
        center: ScreenPosition,
        radius_px: f32,
    },
    LineSegment {
        start: ScreenPosition,
        end: ScreenPosition,
        width_px: f32,
    },
    Rect(ScreenRect),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SceneGeometry {
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
    ScreenLabel {
        anchor: ScreenPosition,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneObject {
    pub id: SceneObjectId,
    pub coordinate_space: CoordinateSpace,
    pub time: SceneTime,
    pub occlusion: OcclusionPolicy,
    pub geometry: SceneGeometry,
    pub style: Option<SceneStyle>,
    pub visible: bool,
    pub selectable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneLayer {
    pub id: SceneLayerId,
    pub kind: SceneLayerKind,
    pub style: SceneStyle,
    pub visible: bool,
    objects: Vec<SceneObject>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneFrameContext {
    pub timepoint: TimeIndex,
    pub grid_transforms: Vec<SceneGridTransform>,
    pub plane_transforms: Vec<ScenePlaneTransform>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneGridTransform {
    pub layer_id: LayerId,
    pub grid_to_world: GridToWorld,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScenePlaneTransform {
    pub plane_id: PlaneId,
    pub plane_to_world: DMat4,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneDrawItem {
    pub layer_id: SceneLayerId,
    pub object_id: SceneObjectId,
    pub layer_kind: SceneLayerKind,
    pub pass: SceneRenderPass,
    pub coordinate_space: CoordinateSpace,
    pub grid_to_world: Option<GridToWorld>,
    pub plane_to_world: Option<DMat4>,
    pub occlusion: OcclusionPolicy,
    pub geometry: SceneGeometry,
    pub style: SceneStyle,
    pub selectable: bool,
}

impl SceneFrameContext {
    pub fn new(timepoint: TimeIndex) -> Self {
        Self {
            timepoint,
            grid_transforms: Vec::new(),
            plane_transforms: Vec::new(),
        }
    }

    pub fn with_grid_to_world(mut self, layer_id: LayerId, grid_to_world: GridToWorld) -> Self {
        self.grid_transforms.push(SceneGridTransform {
            layer_id,
            grid_to_world,
        });
        self
    }

    pub fn with_plane_to_world(mut self, plane_id: PlaneId, plane_to_world: DMat4) -> Self {
        self.plane_transforms.push(ScenePlaneTransform {
            plane_id,
            plane_to_world,
        });
        self
    }

    fn grid_to_world(&self, layer_id: &LayerId) -> Option<GridToWorld> {
        self.grid_transforms
            .iter()
            .find(|transform| &transform.layer_id == layer_id)
            .map(|transform| transform.grid_to_world)
    }

    fn plane_to_world(&self, plane_id: &PlaneId) -> Option<DMat4> {
        self.plane_transforms
            .iter()
            .find(|transform| &transform.plane_id == plane_id)
            .map(|transform| transform.plane_to_world)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneDrawList {
    items: Vec<SceneDrawItem>,
}

impl SceneLayerId {
    pub fn new(value: impl Into<String>) -> Result<Self, SceneError> {
        validate_id("scene layer", value.into()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl SceneObjectId {
    pub fn new(value: impl Into<String>) -> Result<Self, SceneError> {
        validate_id("scene object", value.into()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl PlaneId {
    pub fn new(value: impl Into<String>) -> Result<Self, SceneError> {
        validate_id("plane", value.into()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SceneLayerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl fmt::Display for SceneObjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl fmt::Display for PlaneId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl SceneTime {
    pub fn interval(start: TimeIndex, end_exclusive: TimeIndex) -> Result<Self, SceneError> {
        validate_interval(start, end_exclusive)?;
        Ok(Self::Interval {
            start,
            end_exclusive,
        })
    }

    pub fn trajectory(start: TimeIndex, end_exclusive: TimeIndex) -> Result<Self, SceneError> {
        validate_interval(start, end_exclusive)?;
        Ok(Self::Trajectory {
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
            }
            | Self::Trajectory {
                start,
                end_exclusive,
            } => timepoint >= start && timepoint < end_exclusive,
        }
    }
}

impl SceneColorRgba {
    pub const WHITE: Self = Self::new(255, 255, 255, 255);
    pub const CYAN: Self = Self::new(54, 211, 255, 255);
    pub const AMBER: Self = Self::new(255, 184, 77, 255);
    pub const GREEN: Self = Self::new(87, 211, 117, 255);
    pub const MAGENTA: Self = Self::new(235, 92, 255, 255);
    pub const BLUE: Self = Self::new(92, 141, 255, 255);

    pub const fn new(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }

    pub fn packed_rgba_u32(self) -> u32 {
        u32::from(self.red)
            | (u32::from(self.green) << 8)
            | (u32::from(self.blue) << 16)
            | (u32::from(self.alpha) << 24)
    }

    pub fn from_packed_rgba_u32(value: u32) -> Self {
        Self {
            red: (value & 0xff) as u8,
            green: ((value >> 8) & 0xff) as u8,
            blue: ((value >> 16) & 0xff) as u8,
            alpha: ((value >> 24) & 0xff) as u8,
        }
    }
}

impl SceneStyle {
    pub const fn new(color: SceneColorRgba) -> Self {
        Self { color }
    }

    pub const fn default_for_layer_kind(kind: SceneLayerKind) -> Self {
        match kind {
            SceneLayerKind::Track => Self::new(SceneColorRgba::CYAN),
            SceneLayerKind::Annotation => Self::new(SceneColorRgba::AMBER),
            SceneLayerKind::Measurement => Self::new(SceneColorRgba::GREEN),
            SceneLayerKind::Interaction => Self::new(SceneColorRgba::MAGENTA),
            SceneLayerKind::Reference => Self::new(SceneColorRgba::WHITE),
        }
    }
}

impl SceneGeometry {
    pub fn polyline(points: Vec<DVec3>, width_px: f32) -> Result<Self, SceneError> {
        if points.len() < 2 {
            return Err(SceneError::EmptyPolyline);
        }
        Ok(Self::Polyline { points, width_px })
    }
}

impl ScreenPosition {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    fn distance_squared(self, other: Self) -> f32 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        dx * dx + dy * dy
    }

    fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

impl ScreenRect {
    pub fn new(min: ScreenPosition, max: ScreenPosition) -> Result<Self, SceneError> {
        if !(min.is_finite() && max.is_finite() && min.x <= max.x && min.y <= max.y) {
            return Err(SceneError::InvalidPickRectangle);
        }
        Ok(Self { min, max })
    }

    fn contains(self, position: ScreenPosition) -> bool {
        position.x >= self.min.x
            && position.x <= self.max.x
            && position.y >= self.min.y
            && position.y <= self.max.y
    }
}

impl PickPrimitive {
    pub fn point(center: ScreenPosition, radius_px: f32) -> Result<Self, SceneError> {
        validate_pick_radius("point", radius_px)?;
        Ok(Self::Point { center, radius_px })
    }

    pub fn line_segment(
        start: ScreenPosition,
        end: ScreenPosition,
        width_px: f32,
    ) -> Result<Self, SceneError> {
        validate_pick_radius("line segment", width_px)?;
        Ok(Self::LineSegment {
            start,
            end,
            width_px,
        })
    }

    pub fn rect(min: ScreenPosition, max: ScreenPosition) -> Result<Self, SceneError> {
        Ok(Self::Rect(ScreenRect::new(min, max)?))
    }

    fn contains(self, position: ScreenPosition) -> bool {
        if !position.is_finite() {
            return false;
        }
        match self {
            Self::Point { center, radius_px } => {
                center.is_finite() && position.distance_squared(center) <= radius_px * radius_px
            }
            Self::LineSegment {
                start,
                end,
                width_px,
            } => {
                start.is_finite()
                    && end.is_finite()
                    && distance_to_segment_squared(position, start, end)
                        <= (width_px * 0.5) * (width_px * 0.5)
            }
            Self::Rect(rect) => rect.contains(position),
        }
    }
}

impl ScenePickTarget {
    pub fn from_draw_item(
        item: &SceneDrawItem,
        primitive: PickPrimitive,
        kind: PickHitKind,
        query_timepoint: TimeIndex,
        completeness: PickCompleteness,
    ) -> Self {
        Self {
            primitive,
            hit: PickHit {
                kind,
                layer_id: Some(item.layer_id.clone()),
                object_id: Some(item.object_id.clone()),
                source_layer_id: source_layer_id_for_coordinate_space(&item.coordinate_space),
                timepoint: query_timepoint,
                world_position: None,
                grid_position: None,
                screen_position: None,
                value: None,
                policy: PickPolicy::SceneObject,
                completeness,
            },
        }
    }
}

pub fn pick_scene_targets(targets: &[ScenePickTarget], query: PickQuery) -> PickHit {
    for target in targets.iter().rev() {
        if target.hit.timepoint == query.timepoint
            && target.primitive.contains(query.screen_position)
        {
            let mut hit = target.hit.clone();
            hit.screen_position = Some(query.screen_position);
            return hit;
        }
    }
    empty_pick_hit(query)
}

pub fn empty_pick_hit(query: PickQuery) -> PickHit {
    PickHit {
        kind: PickHitKind::Empty,
        layer_id: None,
        object_id: None,
        source_layer_id: None,
        timepoint: query.timepoint,
        world_position: None,
        grid_position: None,
        screen_position: Some(query.screen_position),
        value: None,
        policy: PickPolicy::SceneObject,
        completeness: PickCompleteness::Exact,
    }
}

pub fn voxel_pick_hit(probe: VolumePickProbe, intensity: u16) -> PickHit {
    PickHit {
        kind: PickHitKind::Voxel,
        layer_id: None,
        object_id: None,
        source_layer_id: Some(probe.source_layer_id),
        timepoint: probe.timepoint,
        world_position: probe.world_position,
        grid_position: probe.grid_position,
        screen_position: Some(probe.screen_position),
        value: Some(PickValue::IntensityU16(intensity)),
        policy: probe.policy,
        completeness: probe.completeness,
    }
}

pub fn voxel_pick_hit_u8(probe: VolumePickProbe, intensity: u8) -> PickHit {
    PickHit {
        kind: PickHitKind::Voxel,
        layer_id: None,
        object_id: None,
        source_layer_id: Some(probe.source_layer_id),
        timepoint: probe.timepoint,
        world_position: probe.world_position,
        grid_position: probe.grid_position,
        screen_position: Some(probe.screen_position),
        value: Some(PickValue::IntensityU8(intensity)),
        policy: probe.policy,
        completeness: probe.completeness,
    }
}

pub fn voxel_pick_hit_f32(probe: VolumePickProbe, intensity: f32) -> PickHit {
    PickHit {
        kind: PickHitKind::Voxel,
        layer_id: None,
        object_id: None,
        source_layer_id: Some(probe.source_layer_id),
        timepoint: probe.timepoint,
        world_position: probe.world_position,
        grid_position: probe.grid_position,
        screen_position: Some(probe.screen_position),
        value: Some(PickValue::IntensityF32(intensity)),
        policy: probe.policy,
        completeness: probe.completeness,
    }
}

impl SceneObject {
    pub fn new(
        id: SceneObjectId,
        coordinate_space: CoordinateSpace,
        time: SceneTime,
        occlusion: OcclusionPolicy,
        geometry: SceneGeometry,
    ) -> Self {
        Self {
            id,
            coordinate_space,
            time,
            occlusion,
            geometry,
            style: None,
            visible: true,
            selectable: true,
        }
    }

    pub fn with_style(mut self, style: SceneStyle) -> Self {
        self.style = Some(style);
        self
    }

    pub fn hidden(mut self) -> Self {
        self.visible = false;
        self
    }

    pub fn non_selectable(mut self) -> Self {
        self.selectable = false;
        self
    }
}

impl SceneLayer {
    pub fn new(id: SceneLayerId, kind: SceneLayerKind) -> Self {
        Self {
            id,
            kind,
            style: SceneStyle::default_for_layer_kind(kind),
            visible: true,
            objects: Vec::new(),
        }
    }

    pub fn with_style(mut self, style: SceneStyle) -> Self {
        self.style = style;
        self
    }

    pub fn hidden(mut self) -> Self {
        self.visible = false;
        self
    }

    pub fn with_object(mut self, object: SceneObject) -> Self {
        self.objects.push(object);
        self
    }

    pub fn objects(&self) -> &[SceneObject] {
        &self.objects
    }
}

impl SceneDrawList {
    pub fn items(&self) -> &[SceneDrawItem] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

pub fn extract_scene_draw_list(layers: &[SceneLayer], context: SceneFrameContext) -> SceneDrawList {
    let mut items = Vec::new();
    for layer in layers {
        if !layer.visible {
            continue;
        }
        for object in layer.objects() {
            if !(object.visible && object.time.is_visible_at(context.timepoint)) {
                continue;
            }
            items.push(SceneDrawItem {
                layer_id: layer.id.clone(),
                object_id: object.id.clone(),
                layer_kind: layer.kind,
                pass: render_pass_for(layer.kind, object.occlusion),
                coordinate_space: object.coordinate_space.clone(),
                grid_to_world: match &object.coordinate_space {
                    CoordinateSpace::Grid { layer_id } => context.grid_to_world(layer_id),
                    CoordinateSpace::World
                    | CoordinateSpace::Plane { .. }
                    | CoordinateSpace::Screen => None,
                },
                plane_to_world: match &object.coordinate_space {
                    CoordinateSpace::Plane { plane_id } => context.plane_to_world(plane_id),
                    CoordinateSpace::World
                    | CoordinateSpace::Grid { .. }
                    | CoordinateSpace::Screen => None,
                },
                occlusion: object.occlusion,
                geometry: object.geometry.clone(),
                style: object.style.unwrap_or(layer.style),
                selectable: object.selectable,
            });
        }
    }
    items.sort_by(|left, right| {
        left.pass
            .cmp(&right.pass)
            .then_with(|| left.layer_id.as_str().cmp(right.layer_id.as_str()))
            .then_with(|| left.object_id.as_str().cmp(right.object_id.as_str()))
    });
    SceneDrawList { items }
}

pub fn render_pass_for(layer_kind: SceneLayerKind, occlusion: OcclusionPolicy) -> SceneRenderPass {
    if occlusion == OcclusionPolicy::ScreenSpace {
        return SceneRenderPass::ScreenSpace;
    }

    match layer_kind {
        SceneLayerKind::Interaction => SceneRenderPass::Interaction,
        SceneLayerKind::Reference if occlusion == OcclusionPolicy::AlwaysOnTop => {
            SceneRenderPass::ScreenSpace
        }
        SceneLayerKind::Reference => SceneRenderPass::WorldSpace,
        SceneLayerKind::Track | SceneLayerKind::Annotation | SceneLayerKind::Measurement => {
            SceneRenderPass::WorldSpace
        }
    }
}

fn validate_id(kind: &'static str, value: String) -> Result<String, SceneError> {
    if value.is_empty() {
        return Err(SceneError::EmptyId { kind });
    }
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        Ok(value)
    } else {
        Err(SceneError::InvalidId { kind, value })
    }
}

fn validate_interval(start: TimeIndex, end_exclusive: TimeIndex) -> Result<(), SceneError> {
    if start >= end_exclusive {
        Err(SceneError::InvalidTimeInterval {
            start: start.0,
            end_exclusive: end_exclusive.0,
        })
    } else {
        Ok(())
    }
}

fn validate_pick_radius(kind: &'static str, value: f32) -> Result<(), SceneError> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(SceneError::InvalidPickRadius { kind, value })
    }
}

fn source_layer_id_for_coordinate_space(coordinate_space: &CoordinateSpace) -> Option<LayerId> {
    match coordinate_space {
        CoordinateSpace::Grid { layer_id } => Some(layer_id.clone()),
        CoordinateSpace::World | CoordinateSpace::Plane { .. } | CoordinateSpace::Screen => None,
    }
}

fn distance_to_segment_squared(
    point: ScreenPosition,
    start: ScreenPosition,
    end: ScreenPosition,
) -> f32 {
    let segment_x = end.x - start.x;
    let segment_y = end.y - start.y;
    let length_squared = segment_x * segment_x + segment_y * segment_y;
    if length_squared == 0.0 {
        return point.distance_squared(start);
    }
    let point_x = point.x - start.x;
    let point_y = point.y - start.y;
    let t = ((point_x * segment_x + point_y * segment_y) / length_squared).clamp(0.0, 1.0);
    let closest = ScreenPosition {
        x: start.x + segment_x * t,
        y: start.y + segment_y * t,
    };
    point.distance_squared(closest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_ids_are_strict_ascii_identifiers() {
        assert_eq!(
            SceneLayerId::new("tracks_01").unwrap().as_str(),
            "tracks_01"
        );
        assert!(SceneObjectId::new("").is_err());
        assert!(PlaneId::new("plane µ").is_err());
    }

    #[test]
    fn time_visibility_is_explicit_and_validated() {
        assert!(SceneTime::Static.is_visible_at(TimeIndex(99)));
        assert!(SceneTime::Timepoint(TimeIndex(3)).is_visible_at(TimeIndex(3)));
        assert!(!SceneTime::Timepoint(TimeIndex(3)).is_visible_at(TimeIndex(4)));

        let interval = SceneTime::interval(TimeIndex(2), TimeIndex(5)).unwrap();
        assert!(!interval.is_visible_at(TimeIndex(1)));
        assert!(interval.is_visible_at(TimeIndex(2)));
        assert!(interval.is_visible_at(TimeIndex(4)));
        assert!(!interval.is_visible_at(TimeIndex(5)));

        assert_eq!(
            SceneTime::interval(TimeIndex(4), TimeIndex(4)).unwrap_err(),
            SceneError::InvalidTimeInterval {
                start: 4,
                end_exclusive: 4,
            }
        );
    }

    #[test]
    fn polyline_requires_two_points() {
        assert_eq!(
            SceneGeometry::polyline(vec![DVec3::ZERO], 1.0).unwrap_err(),
            SceneError::EmptyPolyline
        );
        assert!(SceneGeometry::polyline(vec![DVec3::ZERO, DVec3::X], 1.0).is_ok());
    }

    #[test]
    fn extraction_filters_hidden_layers_objects_and_timepoints() {
        let visible_track = SceneObject::new(
            SceneObjectId::new("track-visible").unwrap(),
            CoordinateSpace::World,
            SceneTime::trajectory(TimeIndex(0), TimeIndex(10)).unwrap(),
            OcclusionPolicy::VolumeDepthCued,
            SceneGeometry::polyline(vec![DVec3::ZERO, DVec3::X], 1.0).unwrap(),
        );
        let wrong_time = SceneObject::new(
            SceneObjectId::new("track-hidden-time").unwrap(),
            CoordinateSpace::World,
            SceneTime::Timepoint(TimeIndex(7)),
            OcclusionPolicy::VolumeDepthCued,
            SceneGeometry::Point {
                position: DVec3::Y,
                radius_px: 3.0,
            },
        );
        let hidden_object = SceneObject::new(
            SceneObjectId::new("track-hidden-object").unwrap(),
            CoordinateSpace::World,
            SceneTime::Static,
            OcclusionPolicy::VolumeDepthCued,
            SceneGeometry::Point {
                position: DVec3::Z,
                radius_px: 3.0,
            },
        )
        .hidden();
        let visible_layer =
            SceneLayer::new(SceneLayerId::new("tracks").unwrap(), SceneLayerKind::Track)
                .with_object(wrong_time)
                .with_object(hidden_object)
                .with_object(visible_track);
        let hidden_layer = SceneLayer::new(
            SceneLayerId::new("hidden-layer").unwrap(),
            SceneLayerKind::Annotation,
        )
        .hidden()
        .with_object(SceneObject::new(
            SceneObjectId::new("hidden-layer-object").unwrap(),
            CoordinateSpace::World,
            SceneTime::Static,
            OcclusionPolicy::AlwaysOnTop,
            SceneGeometry::Point {
                position: DVec3::ZERO,
                radius_px: 1.0,
            },
        ));

        let draw_list = extract_scene_draw_list(
            &[visible_layer, hidden_layer],
            SceneFrameContext::new(TimeIndex(3)),
        );

        assert_eq!(draw_list.len(), 1);
        assert_eq!(draw_list.items()[0].object_id.as_str(), "track-visible");
    }

    #[test]
    fn extraction_assigns_deterministic_pass_order() {
        let interaction = SceneLayer::new(
            SceneLayerId::new("interaction").unwrap(),
            SceneLayerKind::Interaction,
        )
        .with_object(point_object(
            "cursor",
            CoordinateSpace::Screen,
            OcclusionPolicy::AlwaysOnTop,
        ));
        let track = SceneLayer::new(SceneLayerId::new("tracks").unwrap(), SceneLayerKind::Track)
            .with_object(point_object(
                "track",
                CoordinateSpace::World,
                OcclusionPolicy::VolumeDepthCued,
            ));
        let screen_reference = SceneLayer::new(
            SceneLayerId::new("reference").unwrap(),
            SceneLayerKind::Reference,
        )
        .with_object(point_object(
            "scale-bar",
            CoordinateSpace::Screen,
            OcclusionPolicy::ScreenSpace,
        ));

        let draw_list = extract_scene_draw_list(
            &[interaction, screen_reference, track],
            SceneFrameContext::new(TimeIndex(0)),
        );
        let passes = draw_list
            .items()
            .iter()
            .map(|item| item.pass)
            .collect::<Vec<_>>();

        assert_eq!(
            passes,
            vec![
                SceneRenderPass::WorldSpace,
                SceneRenderPass::Interaction,
                SceneRenderPass::ScreenSpace,
            ]
        );
    }

    #[test]
    fn extraction_preserves_coordinate_space_occlusion_and_selectability() {
        let source_layer = LayerId::new("ch0").unwrap();
        let object = SceneObject::new(
            SceneObjectId::new("roi").unwrap(),
            CoordinateSpace::Grid {
                layer_id: source_layer.clone(),
            },
            SceneTime::Static,
            OcclusionPolicy::DepthTestGeometry,
            SceneGeometry::Box3D {
                min: DVec3::ZERO,
                max: DVec3::splat(4.0),
            },
        )
        .non_selectable();
        let layer = SceneLayer::new(
            SceneLayerId::new("rois").unwrap(),
            SceneLayerKind::Annotation,
        )
        .with_object(object);

        let draw_list = extract_scene_draw_list(&[layer], SceneFrameContext::new(TimeIndex(0)));
        let item = &draw_list.items()[0];

        assert_eq!(
            item.coordinate_space,
            CoordinateSpace::Grid {
                layer_id: source_layer
            }
        );
        assert_eq!(item.occlusion, OcclusionPolicy::DepthTestGeometry);
        assert!(!item.selectable);
    }

    #[test]
    fn pick_hit_carries_policy_and_completeness() {
        let hit = PickHit {
            kind: PickHitKind::Voxel,
            layer_id: None,
            object_id: None,
            source_layer_id: Some(LayerId::new("ch0").unwrap()),
            timepoint: TimeIndex(12),
            world_position: Some(DVec3::new(1.0, 2.0, 3.0)),
            grid_position: Some(GridPosition {
                z: 3.0,
                y: 2.0,
                x: 1.0,
            }),
            screen_position: Some(ScreenPosition { x: 10.0, y: 20.0 }),
            value: Some(PickValue::IntensityU16(44)),
            policy: PickPolicy::ProbeRay,
            completeness: PickCompleteness::Incomplete,
        };

        assert_eq!(hit.kind, PickHitKind::Voxel);
        assert_eq!(hit.policy, PickPolicy::ProbeRay);
        assert_eq!(hit.completeness, PickCompleteness::Incomplete);
        assert_eq!(hit.value, Some(PickValue::IntensityU16(44)));
    }

    #[test]
    fn picking_returns_topmost_scene_object_hit() {
        let bottom = scene_pick_target(
            "bottom",
            PickPrimitive::rect(
                ScreenPosition::new(0.0, 0.0),
                ScreenPosition::new(100.0, 100.0),
            )
            .unwrap(),
        );
        let top = scene_pick_target(
            "top",
            PickPrimitive::point(ScreenPosition::new(50.0, 50.0), 10.0).unwrap(),
        );

        let hit = pick_scene_targets(
            &[bottom, top],
            PickQuery {
                timepoint: TimeIndex(0),
                screen_position: ScreenPosition::new(52.0, 50.0),
            },
        );

        assert_eq!(hit.kind, PickHitKind::Annotation);
        assert_eq!(hit.object_id.unwrap().as_str(), "top");
        assert_eq!(hit.screen_position, Some(ScreenPosition::new(52.0, 50.0)));
        assert_eq!(hit.policy, PickPolicy::SceneObject);
        assert_eq!(hit.completeness, PickCompleteness::Exact);
    }

    #[test]
    fn picking_returns_empty_hit_when_no_target_contains_position() {
        let hit = pick_scene_targets(
            &[scene_pick_target(
                "far-away",
                PickPrimitive::point(ScreenPosition::new(10.0, 10.0), 3.0).unwrap(),
            )],
            PickQuery {
                timepoint: TimeIndex(0),
                screen_position: ScreenPosition::new(100.0, 100.0),
            },
        );

        assert_eq!(hit.kind, PickHitKind::Empty);
        assert_eq!(hit.object_id, None);
        assert_eq!(hit.screen_position, Some(ScreenPosition::new(100.0, 100.0)));
        assert_eq!(hit.completeness, PickCompleteness::Exact);
    }

    #[test]
    fn uint8_voxel_pick_hit_preserves_source_value_and_metadata() {
        let hit = voxel_pick_hit_u8(
            VolumePickProbe {
                source_layer_id: LayerId::new("u8-ch0").unwrap(),
                timepoint: TimeIndex(6),
                screen_position: ScreenPosition::new(3.0, 4.0),
                world_position: Some(DVec3::new(1.25, 2.5, 3.75)),
                grid_position: Some(GridPosition {
                    z: 3.5,
                    y: 2.25,
                    x: 1.125,
                }),
                policy: PickPolicy::MipArgmax,
                completeness: PickCompleteness::Exact,
            },
            u8::MAX,
        );

        assert_eq!(hit.kind, PickHitKind::Voxel);
        assert_eq!(hit.source_layer_id.unwrap().as_str(), "u8-ch0");
        assert_eq!(hit.timepoint, TimeIndex(6));
        assert_eq!(hit.value, Some(PickValue::IntensityU8(u8::MAX)));
        assert_eq!(hit.policy, PickPolicy::MipArgmax);
        assert_eq!(hit.completeness, PickCompleteness::Exact);
        assert_eq!(hit.grid_position.unwrap().z, 3.5);
    }

    #[test]
    fn float32_voxel_pick_hit_preserves_source_value_and_metadata() {
        let hit = voxel_pick_hit_f32(
            VolumePickProbe {
                source_layer_id: LayerId::new("float-ch0").unwrap(),
                timepoint: TimeIndex(5),
                screen_position: ScreenPosition::new(3.0, 4.0),
                world_position: Some(DVec3::new(1.25, 2.5, 3.75)),
                grid_position: Some(GridPosition {
                    z: 3.5,
                    y: 2.25,
                    x: 1.125,
                }),
                policy: PickPolicy::MipArgmax,
                completeness: PickCompleteness::Exact,
            },
            -2.5,
        );

        assert_eq!(hit.kind, PickHitKind::Voxel);
        assert_eq!(hit.source_layer_id.unwrap().as_str(), "float-ch0");
        assert_eq!(hit.timepoint, TimeIndex(5));
        assert_eq!(hit.value, Some(PickValue::IntensityF32(-2.5)));
        assert_eq!(hit.policy, PickPolicy::MipArgmax);
        assert_eq!(hit.completeness, PickCompleteness::Exact);
        assert_eq!(hit.grid_position.unwrap().z, 3.5);
    }

    #[test]
    fn pick_primitives_validate_bounds_and_hit_testing() {
        assert_eq!(
            PickPrimitive::point(ScreenPosition::new(0.0, 0.0), -1.0).unwrap_err(),
            SceneError::InvalidPickRadius {
                kind: "point",
                value: -1.0
            }
        );
        assert_eq!(
            PickPrimitive::rect(
                ScreenPosition::new(10.0, 0.0),
                ScreenPosition::new(0.0, 10.0),
            )
            .unwrap_err(),
            SceneError::InvalidPickRectangle
        );

        let line = PickPrimitive::line_segment(
            ScreenPosition::new(0.0, 0.0),
            ScreenPosition::new(10.0, 0.0),
            4.0,
        )
        .unwrap();
        assert!(line.contains(ScreenPosition::new(5.0, 1.9)));
        assert!(!line.contains(ScreenPosition::new(5.0, 2.1)));
    }

    fn point_object(
        id: &str,
        coordinate_space: CoordinateSpace,
        occlusion: OcclusionPolicy,
    ) -> SceneObject {
        SceneObject::new(
            SceneObjectId::new(id).unwrap(),
            coordinate_space,
            SceneTime::Static,
            occlusion,
            SceneGeometry::Point {
                position: DVec3::ZERO,
                radius_px: 3.0,
            },
        )
    }

    fn scene_pick_target(id: &str, primitive: PickPrimitive) -> ScenePickTarget {
        let layer = SceneLayer::new(
            SceneLayerId::new("annotations").unwrap(),
            SceneLayerKind::Annotation,
        )
        .with_object(point_object(
            id,
            CoordinateSpace::World,
            OcclusionPolicy::VolumeDepthCued,
        ));
        let draw_list = extract_scene_draw_list(&[layer], SceneFrameContext::new(TimeIndex(0)));
        ScenePickTarget::from_draw_item(
            &draw_list.items()[0],
            primitive,
            PickHitKind::Annotation,
            TimeIndex(0),
            PickCompleteness::Exact,
        )
    }
}
