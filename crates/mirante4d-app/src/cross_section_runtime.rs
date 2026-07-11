//! Payload-free cross-section panel presentation state.
//!
//! Dataset demand, decoded bytes, cancellation, and residency belong to the
//! unified dataset runtime. GPU resources belong to the renderer. This module
//! records only panel viewports and generation-scoped presentation facts.

use std::collections::BTreeMap;

use mirante4d_render_api::PresentationViewport;
use mirante4d_renderer::RenderViewport;

use crate::{
    render_state::ResidentRenderFailureStatus,
    viewer_layout::{
        CrossSectionPanelScheduleReason, CrossSectionPanelScheduleState,
        CrossSectionPanelScheduleStatus, PanelId,
    },
};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CrossSectionPanelRuntime {
    pub(crate) panel_id: PanelId,
    pub(crate) presentation_viewport: Option<PresentationViewport>,
    pub(crate) render_viewport: Option<RenderViewport>,
    pub(crate) generation: u64,
    pub(crate) displayed_generation: Option<u64>,
    pub(crate) cross_section_schedule: Option<CrossSectionPanelScheduleState>,
    pub(crate) render_failure: Option<ResidentRenderFailureStatus>,
}

#[derive(Debug, Clone)]
pub(crate) struct CrossSectionRuntime {
    panels: BTreeMap<PanelId, CrossSectionPanelRuntime>,
}

impl Default for CrossSectionRuntime {
    fn default() -> Self {
        Self {
            panels: [PanelId::Xy, PanelId::Xz, PanelId::ThreeD, PanelId::Yz]
                .into_iter()
                .map(|panel_id| (panel_id, CrossSectionPanelRuntime::new(panel_id)))
                .collect(),
        }
    }
}

impl CrossSectionPanelRuntime {
    fn new(panel_id: PanelId) -> Self {
        Self {
            panel_id,
            presentation_viewport: None,
            render_viewport: None,
            generation: 0,
            displayed_generation: None,
            cross_section_schedule: panel_id
                .cross_section_panel()
                .map(|_| CrossSectionPanelScheduleState::missing_viewport(0)),
            render_failure: None,
        }
    }

    pub(crate) fn display_current(&self) -> bool {
        self.displayed_generation == Some(self.generation)
    }

    fn record_viewports(
        &mut self,
        presentation_viewport: PresentationViewport,
        render_viewport: RenderViewport,
    ) -> bool {
        if self.presentation_viewport == Some(presentation_viewport)
            && self.render_viewport == Some(render_viewport)
        {
            return false;
        }
        self.presentation_viewport = Some(presentation_viewport);
        self.render_viewport = Some(render_viewport);
        self.advance_generation();
        true
    }

    fn advance_generation(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.displayed_generation = None;
        self.render_failure = None;
        if self.cross_section_schedule.is_some() {
            self.cross_section_schedule = Some(self.pending_schedule());
        }
    }

    fn pending_schedule(&self) -> CrossSectionPanelScheduleState {
        if self.presentation_viewport.is_none() || self.render_viewport.is_none() {
            return CrossSectionPanelScheduleState::missing_viewport(self.generation);
        }
        CrossSectionPanelScheduleState {
            status: CrossSectionPanelScheduleStatus::Loading,
            reason: CrossSectionPanelScheduleReason::ResidentFramePending,
            ..CrossSectionPanelScheduleState::missing_viewport(self.generation)
        }
    }

    fn mark_displayed(&mut self, generation: u64) -> bool {
        if generation != self.generation {
            return false;
        }
        self.displayed_generation = Some(generation);
        self.render_failure = None;
        true
    }

    fn set_schedule(&mut self, schedule: CrossSectionPanelScheduleState) -> bool {
        if self.panel_id.cross_section_panel().is_none() || schedule.generation != self.generation {
            return false;
        }
        self.cross_section_schedule = Some(schedule);
        true
    }

    fn mark_render_failed(
        &mut self,
        generation: u64,
        failure: ResidentRenderFailureStatus,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.render_failure = Some(failure);
        true
    }
}

impl CrossSectionRuntime {
    pub(crate) fn panels(&self) -> impl ExactSizeIterator<Item = &CrossSectionPanelRuntime> {
        self.panels.values()
    }

    pub(crate) fn panel(&self, panel_id: PanelId) -> Option<&CrossSectionPanelRuntime> {
        self.panels.get(&panel_id)
    }

    pub(crate) fn record_panel_viewports(
        &mut self,
        panel_id: PanelId,
        presentation_viewport: PresentationViewport,
        render_viewport: RenderViewport,
    ) -> bool {
        self.panels
            .get_mut(&panel_id)
            .is_some_and(|panel| panel.record_viewports(presentation_viewport, render_viewport))
    }

    pub(crate) fn mark_panel_displayed(&mut self, panel_id: PanelId, generation: u64) -> bool {
        self.panels
            .get_mut(&panel_id)
            .is_some_and(|panel| panel.mark_displayed(generation))
    }

    pub(crate) fn set_panel_schedule(
        &mut self,
        panel_id: PanelId,
        schedule: CrossSectionPanelScheduleState,
    ) -> bool {
        self.panels
            .get_mut(&panel_id)
            .is_some_and(|panel| panel.set_schedule(schedule))
    }

    pub(crate) fn mark_panel_render_failed(
        &mut self,
        panel_id: PanelId,
        generation: u64,
        failure: ResidentRenderFailureStatus,
    ) -> bool {
        self.panels
            .get_mut(&panel_id)
            .is_some_and(|panel| panel.mark_render_failed(generation, failure))
    }

    pub(crate) fn mark_cross_section_panels_dirty(&mut self) -> bool {
        let mut changed = false;
        for panel in self.panels.values_mut() {
            if panel.panel_id.cross_section_panel().is_some() {
                panel.advance_generation();
                changed = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FrameFailureKind, render_state::ResidentRenderFailureStatus};

    fn viewports() -> (PresentationViewport, RenderViewport) {
        (
            PresentationViewport::new(240.0, 180.0).unwrap(),
            RenderViewport::new(480, 360).unwrap(),
        )
    }

    #[test]
    fn viewport_changes_advance_generation_and_invalidate_display() {
        let mut runtime = CrossSectionRuntime::default();
        let (presentation, render) = viewports();
        assert!(runtime.record_panel_viewports(PanelId::Xy, presentation, render));
        let generation = runtime.panel(PanelId::Xy).unwrap().generation;
        assert!(runtime.mark_panel_displayed(PanelId::Xy, generation));
        assert!(runtime.panel(PanelId::Xy).unwrap().display_current());

        let next_render = RenderViewport::new(640, 360).unwrap();
        assert!(runtime.record_panel_viewports(PanelId::Xy, presentation, next_render));
        let panel = runtime.panel(PanelId::Xy).unwrap();
        assert_eq!(panel.generation, generation + 1);
        assert!(!panel.display_current());
        assert_eq!(
            panel.cross_section_schedule.unwrap().reason,
            CrossSectionPanelScheduleReason::ResidentFramePending
        );
    }

    #[test]
    fn identical_viewports_do_not_advance_generation() {
        let mut runtime = CrossSectionRuntime::default();
        let (presentation, render) = viewports();
        assert!(runtime.record_panel_viewports(PanelId::Xy, presentation, render));
        let generation = runtime.panel(PanelId::Xy).unwrap().generation;
        assert!(!runtime.record_panel_viewports(PanelId::Xy, presentation, render));
        assert_eq!(runtime.panel(PanelId::Xy).unwrap().generation, generation);
    }

    #[test]
    fn stale_display_and_schedule_updates_are_rejected() {
        let mut runtime = CrossSectionRuntime::default();
        let (presentation, render) = viewports();
        assert!(runtime.record_panel_viewports(PanelId::Xy, presentation, render));
        let generation = runtime.panel(PanelId::Xy).unwrap().generation;
        assert!(!runtime.mark_panel_displayed(PanelId::Xy, generation - 1));
        assert!(!runtime.set_panel_schedule(
            PanelId::Xy,
            CrossSectionPanelScheduleState::missing_viewport(generation - 1),
        ));
        assert!(runtime.set_panel_schedule(
            PanelId::Xy,
            CrossSectionPanelScheduleState::missing_viewport(generation),
        ));
    }

    #[test]
    fn dirtying_cross_sections_leaves_three_d_generation_unchanged() {
        let mut runtime = CrossSectionRuntime::default();
        let three_d = runtime.panel(PanelId::ThreeD).unwrap().generation;
        assert!(runtime.mark_cross_section_panels_dirty());
        assert_eq!(runtime.panel(PanelId::ThreeD).unwrap().generation, three_d);
        for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
            assert_eq!(runtime.panel(panel_id).unwrap().generation, 1);
        }
    }

    #[test]
    fn render_failures_are_generation_scoped() {
        let mut runtime = CrossSectionRuntime::default();
        let generation = runtime.panel(PanelId::Xy).unwrap().generation;
        let failure = ResidentRenderFailureStatus::new(
            FrameFailureKind::BudgetExceeded,
            "cross-section GPU budget exceeded",
        );
        assert!(runtime.mark_panel_render_failed(PanelId::Xy, generation, failure.clone()));
        assert!(runtime.panel(PanelId::Xy).unwrap().render_failure.is_some());
        assert!(runtime.mark_cross_section_panels_dirty());
        assert!(runtime.panel(PanelId::Xy).unwrap().render_failure.is_none());
        assert!(!runtime.mark_panel_render_failed(PanelId::Xy, generation, failure));
    }
}
