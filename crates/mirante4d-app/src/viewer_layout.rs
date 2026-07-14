use glam::{DQuat, DVec3};
use mirante4d_application::CrossSectionPanelId;
use mirante4d_domain::{CrossSectionView, UnitQuaternion, WorldPoint3};
use mirante4d_render_api::PresentationViewport;

const CROSS_SECTION_EPSILON: f64 = 1.0e-9;
const MIN_ORIENTATION_LENGTH_SQUARED: f64 = 1.0e-18;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CrossSectionPanel {
    Xy,
    Xz,
    Yz,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct CrossSectionBasis {
    right_world: DVec3,
    down_world: DVec3,
    normal_away_world: DVec3,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CrossSectionPanelView {
    center_world: DVec3,
    basis: CrossSectionBasis,
    scale_world_per_screen_point: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CrossSectionViewState {
    center_world: DVec3,
    orientation: DQuat,
    scale_world_per_screen_point: f64,
    depth_world: f64,
}

impl CrossSectionPanel {
    fn basis(self, cross_section_orientation: DQuat) -> CrossSectionBasis {
        let relative_orientation = match self {
            Self::Xy => DQuat::IDENTITY,
            Self::Xz => DQuat::from_rotation_x(std::f64::consts::FRAC_PI_2),
            Self::Yz => DQuat::from_rotation_y(std::f64::consts::FRAC_PI_2),
        };
        let orientation =
            normalized_orientation_or_identity(cross_section_orientation) * relative_orientation;
        CrossSectionBasis {
            right_world: (orientation * DVec3::X).normalize(),
            down_world: (orientation * DVec3::Y).normalize(),
            normal_away_world: (orientation * DVec3::Z).normalize(),
        }
    }
}

impl CrossSectionPanelView {
    pub(crate) fn world_point_for_panel_point(
        self,
        x_points: f64,
        y_points: f64,
        viewport: PresentationViewport,
    ) -> DVec3 {
        let dx = (x_points - viewport.width_points() * 0.5) * self.scale_world_per_screen_point;
        let dy = (y_points - viewport.height_points() * 0.5) * self.scale_world_per_screen_point;
        self.center_world + self.basis.right_world * dx + self.basis.down_world * dy
    }
}

impl CrossSectionViewState {
    fn from_canonical(view: CrossSectionView) -> Self {
        Self {
            center_world: DVec3::from_array(view.center_world().components()),
            orientation: normalized_orientation_or_identity(DQuat::from_array(
                view.orientation().xyzw(),
            )),
            scale_world_per_screen_point: view.scale_world_per_screen_point(),
            depth_world: view.depth_world(),
        }
    }

    pub(crate) fn into_canonical(self) -> Result<CrossSectionView, String> {
        let [x, y, z] = self.center_world.to_array();
        let [qx, qy, qz, qw] = self.orientation.to_array();
        CrossSectionView::new(
            WorldPoint3::new(x, y, z).map_err(|error| error.to_string())?,
            UnitQuaternion::new_xyzw(qx, qy, qz, qw).map_err(|error| error.to_string())?,
            self.scale_world_per_screen_point,
            self.depth_world,
        )
        .map_err(|error| error.to_string())
    }

    pub(crate) fn view(self, panel: CrossSectionPanel) -> CrossSectionPanelView {
        CrossSectionPanelView {
            center_world: self.center_world,
            basis: panel.basis(self.orientation),
            scale_world_per_screen_point: self.scale_world_per_screen_point,
        }
    }

    pub(crate) fn pan_by_panel_points(
        &mut self,
        panel: CrossSectionPanel,
        motion_x_points: f64,
        motion_y_points: f64,
    ) {
        if !motion_x_points.is_finite() || !motion_y_points.is_finite() {
            return;
        }
        let basis = panel.basis(self.orientation);
        self.center_world -=
            basis.right_world * motion_x_points * self.scale_world_per_screen_point;
        self.center_world -= basis.down_world * motion_y_points * self.scale_world_per_screen_point;
    }

    pub(crate) fn slice_by_world_distance(
        &mut self,
        panel: CrossSectionPanel,
        distance_world: f64,
    ) {
        if !distance_world.is_finite() {
            return;
        }
        self.center_world += panel.basis(self.orientation).normal_away_world * distance_world;
    }

    pub(crate) fn zoom_around_panel_point(
        &mut self,
        panel: CrossSectionPanel,
        viewport: PresentationViewport,
        x_points: f64,
        y_points: f64,
        factor: f64,
    ) {
        if !x_points.is_finite() || !y_points.is_finite() || !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let old_view = self.view(panel);
        let anchored_world = old_view.world_point_for_panel_point(x_points, y_points, viewport);
        let new_scale = (self.scale_world_per_screen_point * factor).max(CROSS_SECTION_EPSILON);
        let dx_points = x_points - viewport.width_points() * 0.5;
        let dy_points = y_points - viewport.height_points() * 0.5;
        self.scale_world_per_screen_point = new_scale;
        self.center_world = anchored_world
            - old_view.basis.right_world * dx_points * new_scale
            - old_view.basis.down_world * dy_points * new_scale;
    }

    pub(crate) fn rotate_oblique_by_panel_drag(
        &mut self,
        panel: CrossSectionPanel,
        delta_x_points: f64,
        delta_y_points: f64,
        radians_per_point: f64,
    ) {
        if !delta_x_points.is_finite()
            || !delta_y_points.is_finite()
            || !radians_per_point.is_finite()
        {
            return;
        }
        let basis = panel.basis(self.orientation);
        let yaw = DQuat::from_axis_angle(basis.down_world, delta_x_points * radians_per_point);
        let pitch = DQuat::from_axis_angle(basis.right_world, -delta_y_points * radians_per_point);
        self.orientation = normalized_orientation_or_identity((yaw * pitch) * self.orientation);
    }
}

fn normalized_orientation_or_identity(orientation: DQuat) -> DQuat {
    if !orientation.is_finite() || orientation.length_squared() <= MIN_ORIENTATION_LENGTH_SQUARED {
        DQuat::IDENTITY
    } else {
        orientation.normalize()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum PanelId {
    Xy,
    Xz,
    ThreeD,
    Yz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CrossSectionPanelScheduleStatus {
    MissingViewport,
    Loading,
    Empty,
    Ready,
    Current,
    Coarse,
    Incomplete,
    BudgetLimited,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CrossSectionPanelScheduleReason {
    MissingViewport,
    GpuUnavailable,
    ResidentFramePending,
    TargetScaleReady,
    ResidentScaleCoarserThanTarget,
    MissingSelectedBricks,
    NoSelectedData,
    PlanningBudgetExceeded,
    PlanningFailed,
    Rendered,
    RenderFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CrossSectionPanelScheduleState {
    pub(crate) generation: u64,
    pub(crate) target_scale_level: Option<u32>,
    pub(crate) render_scale_level: Option<u32>,
    pub(crate) fallback_scale_level: Option<u32>,
    pub(crate) selected_bricks: usize,
    pub(crate) occupied_selected_bricks: usize,
    pub(crate) missing_occupied_bricks: usize,
    pub(crate) estimated_decoded_bytes: u64,
    pub(crate) decoded_budget_bytes: u64,
    pub(crate) status: CrossSectionPanelScheduleStatus,
    pub(crate) reason: CrossSectionPanelScheduleReason,
}

impl CrossSectionPanelScheduleState {
    pub(crate) fn missing_viewport(generation: u64) -> Self {
        Self {
            generation,
            target_scale_level: None,
            render_scale_level: None,
            fallback_scale_level: None,
            selected_bricks: 0,
            occupied_selected_bricks: 0,
            missing_occupied_bricks: 0,
            estimated_decoded_bytes: 0,
            decoded_budget_bytes: 0,
            status: CrossSectionPanelScheduleStatus::MissingViewport,
            reason: CrossSectionPanelScheduleReason::MissingViewport,
        }
    }

    pub(crate) fn rendered(mut self) -> Self {
        self.status = if self.missing_occupied_bricks > 0 {
            CrossSectionPanelScheduleStatus::Incomplete
        } else if self.fallback_scale_level.is_some() {
            CrossSectionPanelScheduleStatus::Coarse
        } else {
            CrossSectionPanelScheduleStatus::Current
        };
        self.reason = CrossSectionPanelScheduleReason::Rendered;
        self
    }

    pub(crate) fn is_renderable(self) -> bool {
        let has_current_partial = self.occupied_selected_bricks > 0;
        (matches!(
            self.status,
            CrossSectionPanelScheduleStatus::Ready | CrossSectionPanelScheduleStatus::Coarse
        ) && self.missing_occupied_bricks == 0)
            || (matches!(
                self.status,
                CrossSectionPanelScheduleStatus::Loading
                    | CrossSectionPanelScheduleStatus::Incomplete
            ) && has_current_partial)
    }

    pub(crate) fn status_label(self) -> &'static str {
        if self.reason == CrossSectionPanelScheduleReason::RenderFailed {
            return "render failed";
        }
        match self.status {
            CrossSectionPanelScheduleStatus::MissingViewport => "waiting for panel size",
            CrossSectionPanelScheduleStatus::Loading => "loading",
            CrossSectionPanelScheduleStatus::Empty => "outside selected data",
            CrossSectionPanelScheduleStatus::Ready => "ready",
            CrossSectionPanelScheduleStatus::Current => "current",
            CrossSectionPanelScheduleStatus::Coarse => "coarse",
            CrossSectionPanelScheduleStatus::Incomplete => "incomplete",
            CrossSectionPanelScheduleStatus::BudgetLimited => "budget limited",
            CrossSectionPanelScheduleStatus::Unavailable => "unavailable",
        }
    }
}

impl PanelId {
    pub(crate) const fn from_application_panel(panel: CrossSectionPanelId) -> Self {
        match panel {
            CrossSectionPanelId::Xy => Self::Xy,
            CrossSectionPanelId::Xz => Self::Xz,
            CrossSectionPanelId::Yz => Self::Yz,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Xy => "XY",
            Self::Xz => "XZ",
            Self::ThreeD => "3D",
            Self::Yz => "YZ",
        }
    }

    pub(crate) fn cross_section_panel(self) -> Option<CrossSectionPanel> {
        match self {
            Self::Xy => Some(CrossSectionPanel::Xy),
            Self::Xz => Some(CrossSectionPanel::Xz),
            Self::Yz => Some(CrossSectionPanel::Yz),
            Self::ThreeD => None,
        }
    }
}

pub(crate) fn render_cross_section_view_state(view: CrossSectionView) -> CrossSectionViewState {
    CrossSectionViewState::from_canonical(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_ids_map_to_cross_section_renderer_panels() {
        assert_eq!(
            PanelId::Xy.cross_section_panel(),
            Some(CrossSectionPanel::Xy)
        );
        assert_eq!(
            PanelId::Xz.cross_section_panel(),
            Some(CrossSectionPanel::Xz)
        );
        assert_eq!(
            PanelId::Yz.cross_section_panel(),
            Some(CrossSectionPanel::Yz)
        );
        assert_eq!(PanelId::ThreeD.cross_section_panel(), None);
    }

    #[test]
    fn one_retained_resource_is_enough_to_attempt_a_useful_partial_panel() {
        let schedule = CrossSectionPanelScheduleState {
            generation: 1,
            target_scale_level: Some(0),
            render_scale_level: Some(0),
            fallback_scale_level: None,
            selected_bricks: 4,
            occupied_selected_bricks: 1,
            missing_occupied_bricks: 3,
            estimated_decoded_bytes: 0,
            decoded_budget_bytes: 0,
            status: CrossSectionPanelScheduleStatus::Loading,
            reason: CrossSectionPanelScheduleReason::MissingSelectedBricks,
        };
        assert!(schedule.is_renderable());
    }
}
