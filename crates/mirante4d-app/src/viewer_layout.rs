use glam::{DQuat, DVec3};
use mirante4d_application::CrossSectionPanelId;
use mirante4d_domain::CrossSectionView;
use mirante4d_renderer::{CrossSectionPanel, CrossSectionViewState};

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
        let has_current_partial = self.occupied_selected_bricks > self.missing_occupied_bricks;
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
    CrossSectionViewState::new(
        DVec3::from_array(view.center_world().components()),
        DQuat::from_array(view.orientation().xyzw()),
        view.scale_world_per_screen_point(),
        view.depth_world(),
    )
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
}
