//! Framework-neutral, bounded viewer-tool interaction state.
//!
//! This module deliberately retains at most one two-click anchor and one
//! small overlay. It is not an annotation store, scene graph, or persistence
//! authority.

use std::{error::Error, fmt};

use glam::{DMat3, DVec3};
use mirante4d_domain::{GridToWorld, LogicalLayerKey, Shape3D, TimeIndex, ToolKind, WorldPoint3};
use mirante4d_render_api::{VolumePickCompleteness, VolumePickResult};
pub use mirante4d_render_api::{
    VolumePickHitKind as PickHitKind, VolumePickPolicy as PickPolicy, VolumePickValue as PickValue,
};

use crate::SourceSessionGeneration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PickCompleteness {
    Exact,
    Approximate,
    Incomplete,
    Loading,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenPosition {
    pub x: f32,
    pub y: f32,
}

impl ScreenPosition {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Scientific identity needed to reject a result from an obsolete source,
/// timepoint, or active layer. Camera changes are intentionally absent: an
/// accepted world-space anchor remains meaningful while the camera moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ViewerToolContext {
    pub source_generation: SourceSessionGeneration,
    pub timepoint: TimeIndex,
    pub layer: LogicalLayerKey,
}

impl ViewerToolContext {
    pub const fn new(
        source_generation: SourceSessionGeneration,
        timepoint: TimeIndex,
        layer: LogicalLayerKey,
    ) -> Self {
        Self {
            source_generation,
            timepoint,
            layer,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PickHit {
    pub kind: PickHitKind,
    pub context: ViewerToolContext,
    pub screen_position: Option<ScreenPosition>,
    pub world_position: Option<WorldPoint3>,
    pub value: Option<PickValue>,
    pub policy: PickPolicy,
    pub completeness: PickCompleteness,
}

impl PickHit {
    /// Adapts a renderer result only after the composition root has validated
    /// its presentation token and frame against the currently displayed
    /// surface.
    pub fn from_volume_result(
        source_generation: SourceSessionGeneration,
        screen_position: ScreenPosition,
        result: VolumePickResult,
    ) -> Self {
        let query = result.query();
        Self {
            kind: result.kind(),
            context: ViewerToolContext::new(source_generation, query.timepoint(), query.layer()),
            screen_position: Some(screen_position),
            world_position: result.world_position(),
            value: result.value(),
            policy: query.policy(),
            completeness: match result.completeness() {
                VolumePickCompleteness::Exact => PickCompleteness::Exact,
                VolumePickCompleteness::Approximate => PickCompleteness::Approximate,
                VolumePickCompleteness::Incomplete => PickCompleteness::Incomplete,
            },
        }
    }

    fn exact_world_position(&self) -> Option<WorldPoint3> {
        if matches!(
            self.kind,
            PickHitKind::Voxel | PickHitKind::InterpolatedSample
        ) && self.completeness == PickCompleteness::Exact
        {
            self.world_position
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PickQuery {
    pub context: ViewerToolContext,
    pub screen_position: ScreenPosition,
}

pub fn empty_pick_hit(query: PickQuery, policy: PickPolicy) -> PickHit {
    PickHit {
        kind: PickHitKind::Empty,
        context: query.context,
        screen_position: Some(query.screen_position),
        world_position: None,
        value: None,
        policy,
        completeness: PickCompleteness::Exact,
    }
}

/// Immutable base-scale geometry used to convert exact world picks into the
/// numeric ZYX ROI authority consumed by analysis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActiveVolumeGeometry {
    pub base_grid_to_world: GridToWorld,
    pub base_shape: Shape3D,
}

impl ActiveVolumeGeometry {
    pub const fn new(base_grid_to_world: GridToWorld, base_shape: Shape3D) -> Self {
        Self {
            base_grid_to_world,
            base_shape,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseGridRoiBox {
    origin_zyx: [u64; 3],
    shape_zyx: [u64; 3],
}

impl BaseGridRoiBox {
    pub const fn origin_zyx(self) -> [u64; 3] {
        self.origin_zyx
    }

    pub const fn shape_zyx(self) -> [u64; 3] {
        self.shape_zyx
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerOverlayPhase {
    Preview,
    Committed,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoiBoxOverlay {
    roi: BaseGridRoiBox,
    world_corners: [WorldPoint3; 8],
    phase: ViewerOverlayPhase,
}

impl RoiBoxOverlay {
    pub const fn roi(self) -> BaseGridRoiBox {
        self.roi
    }

    pub const fn world_corners(&self) -> &[WorldPoint3; 8] {
        &self.world_corners
    }

    pub const fn phase(self) -> ViewerOverlayPhase {
        self.phase
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DistanceMeasurement {
    start_world: WorldPoint3,
    end_world: WorldPoint3,
    distance_micrometers: f64,
    phase: ViewerOverlayPhase,
}

impl DistanceMeasurement {
    pub const fn start_world(self) -> WorldPoint3 {
        self.start_world
    }

    pub const fn end_world(self) -> WorldPoint3 {
        self.end_world
    }

    pub const fn distance_micrometers(self) -> f64 {
        self.distance_micrometers
    }

    pub const fn phase(self) -> ViewerOverlayPhase {
        self.phase
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewerToolOverlay {
    RoiBox(RoiBoxOverlay),
    Distance(DistanceMeasurement),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerToolError {
    NonInvertibleGridToWorld,
    GridCoordinateNotFinite,
    OverlayWorldPointNotFinite,
    DistanceNotFinite,
}

impl fmt::Display for ViewerToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NonInvertibleGridToWorld => {
                "the active layer base grid-to-world transform is not invertible"
            }
            Self::GridCoordinateNotFinite => {
                "the world pick maps to a non-finite base-grid coordinate"
            }
            Self::OverlayWorldPointNotFinite => "the ROI boundary maps to a non-finite world point",
            Self::DistanceNotFinite => "the world-space distance is not finite",
        })
    }
}

impl Error for ViewerToolError {}

/// Converts two exact world-space picks to one inclusive, clamped base-grid
/// box. Grid coordinates are rounded to the nearest voxel center (halfway
/// values select the higher nonnegative index) before clamping to the layer.
pub fn base_grid_roi_from_world_points(
    geometry: ActiveVolumeGeometry,
    first_world: WorldPoint3,
    second_world: WorldPoint3,
) -> Result<BaseGridRoiBox, ViewerToolError> {
    let first_xyz = world_to_base_grid_voxel(geometry, first_world)?;
    let second_xyz = world_to_base_grid_voxel(geometry, second_world)?;
    let first_zyx = [first_xyz[2], first_xyz[1], first_xyz[0]];
    let second_zyx = [second_xyz[2], second_xyz[1], second_xyz[0]];
    let origin_zyx = std::array::from_fn(|axis| first_zyx[axis].min(second_zyx[axis]));
    let shape_zyx = std::array::from_fn(|axis| first_zyx[axis].abs_diff(second_zyx[axis]) + 1);
    Ok(BaseGridRoiBox {
        origin_zyx,
        shape_zyx,
    })
}

pub fn distance_between_world_points(
    first_world: WorldPoint3,
    second_world: WorldPoint3,
) -> Result<f64, ViewerToolError> {
    let [first_x, first_y, first_z] = first_world.components();
    let [second_x, second_y, second_z] = second_world.components();
    let distance = (second_x - first_x)
        .hypot(second_y - first_y)
        .hypot(second_z - first_z);
    if distance.is_finite() {
        Ok(if distance == 0.0 { 0.0 } else { distance })
    } else {
        Err(ViewerToolError::DistanceNotFinite)
    }
}

fn world_to_base_grid_voxel(
    geometry: ActiveVolumeGeometry,
    world: WorldPoint3,
) -> Result<[u64; 3], ViewerToolError> {
    let matrix = geometry.base_grid_to_world.row_major();
    let linear = DMat3::from_cols(
        DVec3::new(matrix[0], matrix[4], matrix[8]),
        DVec3::new(matrix[1], matrix[5], matrix[9]),
        DVec3::new(matrix[2], matrix[6], matrix[10]),
    );
    let determinant = linear.determinant();
    if determinant == 0.0 || !determinant.is_finite() {
        return Err(ViewerToolError::NonInvertibleGridToWorld);
    }
    let inverse = linear.inverse();
    if !inverse.is_finite() {
        return Err(ViewerToolError::NonInvertibleGridToWorld);
    }
    let translation = DVec3::new(matrix[3], matrix[7], matrix[11]);
    let grid = inverse * (DVec3::from_array(world.components()) - translation);
    if !grid.is_finite() {
        return Err(ViewerToolError::GridCoordinateNotFinite);
    }
    let [z, y, x] = geometry.base_shape.dimensions();
    Ok([
        nearest_clamped_index(grid.x, x),
        nearest_clamped_index(grid.y, y),
        nearest_clamped_index(grid.z, z),
    ])
}

fn nearest_clamped_index(coordinate: f64, dimension: u64) -> u64 {
    coordinate
        .round()
        .clamp(0.0, dimension.saturating_sub(1) as f64) as u64
}

fn roi_overlay(
    geometry: ActiveVolumeGeometry,
    first_world: WorldPoint3,
    second_world: WorldPoint3,
    phase: ViewerOverlayPhase,
) -> Result<RoiBoxOverlay, ViewerToolError> {
    let roi = base_grid_roi_from_world_points(geometry, first_world, second_world)?;
    let [origin_z, origin_y, origin_x] = roi.origin_zyx();
    let [shape_z, shape_y, shape_x] = roi.shape_zyx();
    let low = [
        origin_x as f64 - 0.5,
        origin_y as f64 - 0.5,
        origin_z as f64 - 0.5,
    ];
    let high = [
        origin_x.saturating_add(shape_x) as f64 - 0.5,
        origin_y.saturating_add(shape_y) as f64 - 0.5,
        origin_z.saturating_add(shape_z) as f64 - 0.5,
    ];
    let world_corners = [
        [low[0], low[1], low[2]],
        [high[0], low[1], low[2]],
        [low[0], high[1], low[2]],
        [high[0], high[1], low[2]],
        [low[0], low[1], high[2]],
        [high[0], low[1], high[2]],
        [low[0], high[1], high[2]],
        [high[0], high[1], high[2]],
    ]
    .map(|grid| {
        let grid =
            WorldPoint3::new(grid[0], grid[1], grid[2]).expect("finite base-grid ROI boundaries");
        geometry
            .base_grid_to_world
            .transform_point(grid)
            .map_err(|_| ViewerToolError::OverlayWorldPointNotFinite)
    });
    let [a, b, c, d, e, f, g, h] = world_corners;
    Ok(RoiBoxOverlay {
        roi,
        world_corners: [a?, b?, c?, d?, e?, f?, g?, h?],
        phase,
    })
}

fn distance_overlay(
    first_world: WorldPoint3,
    second_world: WorldPoint3,
    phase: ViewerOverlayPhase,
) -> Result<DistanceMeasurement, ViewerToolError> {
    Ok(DistanceMeasurement {
        start_world: first_world,
        end_world: second_world,
        distance_micrometers: distance_between_world_points(first_world, second_world)?,
        phase,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerTool {
    Navigate,
    Inspect,
    Crosshair,
    RoiBox,
    MeasureDistance,
}

impl From<ToolKind> for ViewerTool {
    fn from(tool: ToolKind) -> Self {
        match tool {
            ToolKind::Navigate => Self::Navigate,
            ToolKind::Inspect => Self::Inspect,
            ToolKind::Crosshair => Self::Crosshair,
            ToolKind::RoiBox => Self::RoiBox,
            ToolKind::MeasureDistance => Self::MeasureDistance,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ToolAnchor {
    context: ViewerToolContext,
    world_position: WorldPoint3,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewerToolState {
    pub active_tool: ViewerTool,
    pub hover: Option<PickHit>,
    pub crosshair: Option<PickHit>,
    context: Option<ViewerToolContext>,
    anchor: Option<ToolAnchor>,
    overlay: Option<ViewerToolOverlay>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ViewerToolEvent {
    Hover(Option<PickHit>),
    PrimaryClick(PickHit),
    Cancel,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ViewerToolCommand {
    SetCrosshair(PickHit),
    CommitRoi(BaseGridRoiBox),
    SetDistanceMeasurement(DistanceMeasurement),
}

impl Default for ViewerToolState {
    fn default() -> Self {
        Self {
            active_tool: ViewerTool::Navigate,
            hover: None,
            crosshair: None,
            context: None,
            anchor: None,
            overlay: None,
        }
    }
}

impl ViewerToolState {
    pub fn set_active_tool(&mut self, tool: ViewerTool) {
        if self.active_tool != tool {
            self.active_tool = tool;
            self.anchor = None;
            self.overlay = None;
        }
    }

    pub fn synchronize_context(&mut self, context: ViewerToolContext) {
        if self.context != Some(context) {
            self.context = Some(context);
            self.hover = None;
            self.crosshair = None;
            self.anchor = None;
            self.overlay = None;
        }
    }

    pub const fn context(&self) -> Option<ViewerToolContext> {
        self.context
    }

    pub const fn overlay(&self) -> Option<&ViewerToolOverlay> {
        self.overlay.as_ref()
    }

    pub const fn has_pending_anchor(&self) -> bool {
        self.anchor.is_some()
    }

    /// Clears only pointer-derived state. Committed overlays and crosshair
    /// state remain valid; an unfinished two-click preview waits for a new
    /// exact hover before it can be drawn again.
    pub fn clear_hover(&mut self) {
        self.hover = None;
        if self.anchor.is_some() {
            self.overlay = None;
        }
    }

    pub fn handle_event(
        &mut self,
        event: ViewerToolEvent,
        geometry: ActiveVolumeGeometry,
    ) -> Result<Vec<ViewerToolCommand>, ViewerToolError> {
        match event {
            ViewerToolEvent::Hover(hit) => {
                self.handle_hover(hit, geometry)?;
                Ok(Vec::new())
            }
            ViewerToolEvent::PrimaryClick(hit) => self.handle_primary_click(hit, geometry),
            ViewerToolEvent::Cancel => {
                self.cancel_pending_gesture();
                Ok(Vec::new())
            }
        }
    }

    fn handle_hover(
        &mut self,
        hit: Option<PickHit>,
        geometry: ActiveVolumeGeometry,
    ) -> Result<(), ViewerToolError> {
        self.hover = hit.filter(|candidate| self.context == Some(candidate.context));
        if self.hover.is_none() {
            self.clear_hover();
            return Ok(());
        }
        let Some(anchor) = self.anchor.clone() else {
            return Ok(());
        };
        let Some(world_position) = self.hover.as_ref().and_then(PickHit::exact_world_position)
        else {
            self.overlay = None;
            return Ok(());
        };
        self.overlay = match self.active_tool {
            ViewerTool::RoiBox => Some(ViewerToolOverlay::RoiBox(roi_overlay(
                geometry,
                anchor.world_position,
                world_position,
                ViewerOverlayPhase::Preview,
            )?)),
            ViewerTool::MeasureDistance => Some(ViewerToolOverlay::Distance(distance_overlay(
                anchor.world_position,
                world_position,
                ViewerOverlayPhase::Preview,
            )?)),
            ViewerTool::Navigate | ViewerTool::Inspect | ViewerTool::Crosshair => None,
        };
        Ok(())
    }

    fn handle_primary_click(
        &mut self,
        hit: PickHit,
        geometry: ActiveVolumeGeometry,
    ) -> Result<Vec<ViewerToolCommand>, ViewerToolError> {
        if self.context != Some(hit.context) {
            return Ok(Vec::new());
        }
        let Some(world_position) = hit.exact_world_position() else {
            return Ok(Vec::new());
        };
        match self.active_tool {
            ViewerTool::Navigate | ViewerTool::Inspect => Ok(Vec::new()),
            ViewerTool::Crosshair => {
                self.crosshair = Some(hit.clone());
                Ok(vec![ViewerToolCommand::SetCrosshair(hit)])
            }
            ViewerTool::RoiBox | ViewerTool::MeasureDistance => {
                self.handle_two_click(world_position, hit.context, geometry)
            }
        }
    }

    fn handle_two_click(
        &mut self,
        world_position: WorldPoint3,
        context: ViewerToolContext,
        geometry: ActiveVolumeGeometry,
    ) -> Result<Vec<ViewerToolCommand>, ViewerToolError> {
        let Some(anchor) = self.anchor.take() else {
            self.overlay = match self.active_tool {
                ViewerTool::RoiBox => Some(ViewerToolOverlay::RoiBox(roi_overlay(
                    geometry,
                    world_position,
                    world_position,
                    ViewerOverlayPhase::Preview,
                )?)),
                ViewerTool::MeasureDistance => Some(ViewerToolOverlay::Distance(distance_overlay(
                    world_position,
                    world_position,
                    ViewerOverlayPhase::Preview,
                )?)),
                ViewerTool::Navigate | ViewerTool::Inspect | ViewerTool::Crosshair => None,
            };
            self.anchor = Some(ToolAnchor {
                context,
                world_position,
            });
            return Ok(Vec::new());
        };
        if anchor.context != context {
            self.overlay = None;
            return Ok(Vec::new());
        }

        match self.active_tool {
            ViewerTool::RoiBox => {
                let overlay = roi_overlay(
                    geometry,
                    anchor.world_position,
                    world_position,
                    ViewerOverlayPhase::Committed,
                )?;
                let roi = overlay.roi();
                self.overlay = Some(ViewerToolOverlay::RoiBox(overlay));
                Ok(vec![ViewerToolCommand::CommitRoi(roi)])
            }
            ViewerTool::MeasureDistance => {
                let measurement = distance_overlay(
                    anchor.world_position,
                    world_position,
                    ViewerOverlayPhase::Committed,
                )?;
                self.overlay = Some(ViewerToolOverlay::Distance(measurement));
                Ok(vec![ViewerToolCommand::SetDistanceMeasurement(measurement)])
            }
            ViewerTool::Navigate | ViewerTool::Inspect | ViewerTool::Crosshair => {
                self.overlay = None;
                Ok(Vec::new())
            }
        }
    }

    fn cancel_pending_gesture(&mut self) {
        self.anchor = None;
        if matches!(
            self.overlay,
            Some(ViewerToolOverlay::RoiBox(RoiBoxOverlay {
                phase: ViewerOverlayPhase::Preview,
                ..
            })) | Some(ViewerToolOverlay::Distance(DistanceMeasurement {
                phase: ViewerOverlayPhase::Preview,
                ..
            }))
        ) {
            self.overlay = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(source: u64, timepoint: u64, layer: u32) -> ViewerToolContext {
        ViewerToolContext::new(
            SourceSessionGeneration::new(source),
            TimeIndex::new(timepoint),
            LogicalLayerKey::new(layer),
        )
    }

    fn identity_geometry() -> ActiveVolumeGeometry {
        ActiveVolumeGeometry::new(GridToWorld::identity(), Shape3D::new(8, 8, 8).unwrap())
    }

    fn voxel_hit(
        context: ViewerToolContext,
        world_position: [f64; 3],
        completeness: PickCompleteness,
    ) -> PickHit {
        PickHit {
            kind: PickHitKind::Voxel,
            context,
            screen_position: Some(ScreenPosition::new(1.0, 2.0)),
            world_position: Some(
                WorldPoint3::new(world_position[0], world_position[1], world_position[2]).unwrap(),
            ),
            value: Some(PickValue::IntensityU16(42)),
            policy: PickPolicy::MipArgmax,
            completeness,
        }
    }

    #[test]
    fn hover_is_currentness_checked_and_can_be_cleared() {
        let current = context(2, 3, 4);
        let mut state = ViewerToolState::default();
        state.synchronize_context(current);
        let current_hit = voxel_hit(current, [1.0, 2.0, 3.0], PickCompleteness::Exact);
        state
            .handle_event(
                ViewerToolEvent::Hover(Some(current_hit.clone())),
                identity_geometry(),
            )
            .unwrap();
        assert_eq!(state.hover, Some(current_hit));

        let stale = voxel_hit(context(1, 3, 4), [1.0, 2.0, 3.0], PickCompleteness::Exact);
        state
            .handle_event(ViewerToolEvent::Hover(Some(stale)), identity_geometry())
            .unwrap();
        assert_eq!(state.hover, None);
    }

    #[test]
    fn crosshair_requires_an_exact_current_world_pick() {
        let current = context(1, 0, 0);
        let mut state = ViewerToolState::default();
        state.synchronize_context(current);
        state.set_active_tool(ViewerTool::Crosshair);
        for completeness in [
            PickCompleteness::Approximate,
            PickCompleteness::Incomplete,
            PickCompleteness::Loading,
        ] {
            let commands = state
                .handle_event(
                    ViewerToolEvent::PrimaryClick(voxel_hit(
                        current,
                        [1.0, 2.0, 3.0],
                        completeness,
                    )),
                    identity_geometry(),
                )
                .unwrap();
            assert!(commands.is_empty());
            assert_eq!(state.crosshair, None);
        }

        let hit = voxel_hit(current, [1.0, 2.0, 3.0], PickCompleteness::Exact);
        assert_eq!(
            state
                .handle_event(
                    ViewerToolEvent::PrimaryClick(hit.clone()),
                    identity_geometry(),
                )
                .unwrap(),
            vec![ViewerToolCommand::SetCrosshair(hit.clone())]
        );
        assert_eq!(state.crosshair, Some(hit));
    }

    #[test]
    fn exact_interpolated_samples_remain_valid_world_space_tool_anchors() {
        let current = context(1, 0, 0);
        let mut state = ViewerToolState::default();
        state.synchronize_context(current);
        state.set_active_tool(ViewerTool::Crosshair);
        let mut hit = voxel_hit(current, [1.25, 2.5, 3.75], PickCompleteness::Exact);
        hit.kind = PickHitKind::InterpolatedSample;

        let commands = state
            .handle_event(
                ViewerToolEvent::PrimaryClick(hit.clone()),
                identity_geometry(),
            )
            .unwrap();

        assert_eq!(commands, vec![ViewerToolCommand::SetCrosshair(hit)]);
    }

    #[test]
    fn affine_world_picks_create_an_inclusive_clamped_base_grid_roi() {
        // world = A * grid + translation, with shear, nonuniform scale, and
        // an axis coupling that a diagonal-only implementation gets wrong.
        let transform = GridToWorld::from_row_major([
            2.0, 1.0, 0.0, 10.0, -1.0, 3.0, 0.5, 20.0, 0.0, 0.0, 4.0, 30.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .unwrap();
        let geometry = ActiveVolumeGeometry::new(transform, Shape3D::new(5, 6, 7).unwrap());
        let first = transform
            .transform_point(WorldPoint3::new(1.2, 2.6, 3.49).unwrap())
            .unwrap();
        let second = transform
            .transform_point(WorldPoint3::new(99.0, -9.0, 8.0).unwrap())
            .unwrap();

        let roi = base_grid_roi_from_world_points(geometry, first, second).unwrap();
        assert_eq!(roi.origin_zyx(), [3, 0, 1]);
        assert_eq!(roi.shape_zyx(), [2, 4, 6]);
    }

    #[test]
    fn roi_tool_uses_two_exact_clicks_and_emits_one_bounded_commit() {
        let current = context(1, 0, 0);
        let geometry = identity_geometry();
        let mut state = ViewerToolState::default();
        state.synchronize_context(current);
        state.set_active_tool(ViewerTool::RoiBox);

        assert!(
            state
                .handle_event(
                    ViewerToolEvent::PrimaryClick(voxel_hit(
                        current,
                        [1.1, 2.2, 3.3],
                        PickCompleteness::Exact,
                    )),
                    geometry,
                )
                .unwrap()
                .is_empty()
        );
        assert!(state.has_pending_anchor());
        assert!(matches!(
            state.overlay(),
            Some(ViewerToolOverlay::RoiBox(overlay))
                if overlay.phase() == ViewerOverlayPhase::Preview
        ));

        let commands = state
            .handle_event(
                ViewerToolEvent::PrimaryClick(voxel_hit(
                    current,
                    [4.2, 5.2, 6.2],
                    PickCompleteness::Exact,
                )),
                geometry,
            )
            .unwrap();
        assert_eq!(
            commands,
            vec![ViewerToolCommand::CommitRoi(BaseGridRoiBox {
                origin_zyx: [3, 2, 1],
                shape_zyx: [4, 4, 4],
            })]
        );
        assert!(!state.has_pending_anchor());
        assert!(matches!(
            state.overlay(),
            Some(ViewerToolOverlay::RoiBox(overlay))
                if overlay.phase() == ViewerOverlayPhase::Committed
        ));
    }

    #[test]
    fn distance_is_euclidean_in_world_micrometers() {
        let current = context(1, 0, 0);
        let geometry = identity_geometry();
        let mut state = ViewerToolState::default();
        state.synchronize_context(current);
        state.set_active_tool(ViewerTool::MeasureDistance);
        state
            .handle_event(
                ViewerToolEvent::PrimaryClick(voxel_hit(
                    current,
                    [0.0, 0.0, 0.0],
                    PickCompleteness::Exact,
                )),
                geometry,
            )
            .unwrap();
        let commands = state
            .handle_event(
                ViewerToolEvent::PrimaryClick(voxel_hit(
                    current,
                    [3.0, 4.0, 0.0],
                    PickCompleteness::Exact,
                )),
                geometry,
            )
            .unwrap();
        assert!(matches!(
            commands.as_slice(),
            [ViewerToolCommand::SetDistanceMeasurement(measurement)]
                if measurement.distance_micrometers() == 5.0
        ));
    }

    #[test]
    fn source_time_or_layer_change_cancels_all_spatial_transients() {
        let current = context(1, 2, 3);
        let mut state = ViewerToolState::default();
        state.synchronize_context(current);
        state.set_active_tool(ViewerTool::MeasureDistance);
        state
            .handle_event(
                ViewerToolEvent::PrimaryClick(voxel_hit(
                    current,
                    [1.0, 2.0, 3.0],
                    PickCompleteness::Exact,
                )),
                identity_geometry(),
            )
            .unwrap();
        state.crosshair = Some(voxel_hit(current, [1.0, 2.0, 3.0], PickCompleteness::Exact));

        state.synchronize_context(context(1, 3, 3));
        assert_eq!(state.hover, None);
        assert_eq!(state.crosshair, None);
        assert!(!state.has_pending_anchor());
        assert_eq!(state.overlay(), None);
    }

    #[test]
    fn cancel_discards_only_an_unfinished_gesture() {
        let current = context(1, 0, 0);
        let geometry = identity_geometry();
        let mut state = ViewerToolState::default();
        state.synchronize_context(current);
        state.set_active_tool(ViewerTool::MeasureDistance);
        state
            .handle_event(
                ViewerToolEvent::PrimaryClick(voxel_hit(
                    current,
                    [0.0, 0.0, 0.0],
                    PickCompleteness::Exact,
                )),
                geometry,
            )
            .unwrap();
        state
            .handle_event(ViewerToolEvent::Cancel, geometry)
            .unwrap();
        assert!(!state.has_pending_anchor());
        assert_eq!(state.overlay(), None);
    }

    #[test]
    fn singular_affine_fails_explicitly() {
        let geometry = ActiveVolumeGeometry::new(
            GridToWorld::scale(1.0, 0.0, 1.0).unwrap(),
            Shape3D::new(2, 2, 2).unwrap(),
        );
        assert_eq!(
            base_grid_roi_from_world_points(
                geometry,
                WorldPoint3::origin(),
                WorldPoint3::new(1.0, 1.0, 1.0).unwrap(),
            ),
            Err(ViewerToolError::NonInvertibleGridToWorld)
        );
    }
}
