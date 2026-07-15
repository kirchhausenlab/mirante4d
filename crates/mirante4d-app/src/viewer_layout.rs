use mirante4d_application::{
    CrossSectionPanelId, CrossSectionPanelScheduleReason, CrossSectionPanelScheduleState,
    CrossSectionPanelScheduleStatus, PresentationSlot, viewport_interaction::CrossSectionPanel,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum PanelId {
    Xy,
    Xz,
    ThreeD,
    Yz,
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

    pub(crate) const fn presentation_slot(self) -> PresentationSlot {
        match self {
            Self::ThreeD => PresentationSlot::ThreeD,
            Self::Xy => PresentationSlot::Xy,
            Self::Xz => PresentationSlot::Xz,
            Self::Yz => PresentationSlot::Yz,
        }
    }

    pub(crate) const fn from_presentation_slot(slot: PresentationSlot) -> Self {
        match slot {
            PresentationSlot::ThreeD => Self::ThreeD,
            PresentationSlot::Xy => Self::Xy,
            PresentationSlot::Xz => Self::Xz,
            PresentationSlot::Yz => Self::Yz,
        }
    }
}

pub(crate) fn cross_section_schedule_status_label(
    schedule: CrossSectionPanelScheduleState,
) -> &'static str {
    if schedule.reason == CrossSectionPanelScheduleReason::RenderFailed {
        return "render failed";
    }
    match schedule.status {
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
