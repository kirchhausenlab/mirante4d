//! Framework-neutral status for coordinating progressive product presentation.

use mirante4d_render_api::{PresentationViewport, RenderExtent};

use crate::PresentationSlot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderBackend {
    Loading,
    Empty,
    GpuCameraMip,
    GpuCameraIso,
    GpuCameraDvr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameCompleteness {
    Exact,
    Complete,
    Loading,
    Incomplete,
    BudgetLimited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayedFrameFreshness {
    Unknown,
    Current,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LodDecisionReason {
    ExactS0,
    ScreenEquivalentCoarserScale,
    PlaybackDownshift,
    LoadingTargetScale,
    NoVisibleData,
    FrameBudgetLimited,
    GpuBudgetLimited,
    CpuBudgetLimited,
    BackendLimit,
    AllocationFailed,
    IncompleteResidency,
    InvalidModeParameter,
    UnsupportedDtype,
    InvalidTransform,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameFailureKind {
    BudgetExceeded,
    BackendLimit,
    AllocationFailed,
    IncompleteResidency,
    InvalidModeParameter,
    UnsupportedDtype,
    InvalidTransform,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameFidelityStatus {
    pub target_scale_level: u32,
    pub displayed_scale_level: Option<u32>,
    pub completeness: FrameCompleteness,
    pub reason: LodDecisionReason,
    pub backend: RenderBackend,
    pub viewport: RenderExtent,
    pub presentation_viewport: PresentationViewport,
    pub display_freshness: DisplayedFrameFreshness,
    pub frame_time_ms: Option<f64>,
    pub visible_bricks: usize,
    pub resident_bricks: usize,
    pub missing_occupied_bricks: usize,
    pub cpu_cache_bytes: u64,
    pub gpu_resident_bytes: u64,
    pub upload_queue_depth: usize,
    pub last_failure_kind: Option<FrameFailureKind>,
    pub last_capacity_error: Option<String>,
}

impl FrameFidelityStatus {
    pub fn new_with_presentation(
        viewport: RenderExtent,
        presentation_viewport: PresentationViewport,
    ) -> Self {
        Self {
            target_scale_level: 0,
            displayed_scale_level: None,
            completeness: FrameCompleteness::Loading,
            reason: LodDecisionReason::LoadingTargetScale,
            backend: RenderBackend::Loading,
            viewport,
            presentation_viewport,
            display_freshness: DisplayedFrameFreshness::Unknown,
            frame_time_ms: None,
            visible_bricks: 0,
            resident_bricks: 0,
            missing_occupied_bricks: 0,
            cpu_cache_bytes: 0,
            gpu_resident_bytes: 0,
            upload_queue_depth: 0,
            last_failure_kind: None,
            last_capacity_error: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossSectionPanelScheduleStatus {
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
pub enum CrossSectionPanelScheduleReason {
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
pub struct CrossSectionPanelScheduleState {
    pub generation: u64,
    pub target_scale_level: Option<u32>,
    pub render_scale_level: Option<u32>,
    pub fallback_scale_level: Option<u32>,
    pub selected_bricks: usize,
    pub occupied_selected_bricks: usize,
    pub missing_occupied_bricks: usize,
    pub estimated_decoded_bytes: u64,
    pub decoded_budget_bytes: u64,
    pub status: CrossSectionPanelScheduleStatus,
    pub reason: CrossSectionPanelScheduleReason,
}

impl CrossSectionPanelScheduleState {
    pub const fn missing_viewport(generation: u64) -> Self {
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

    pub const fn rendered(mut self) -> Self {
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

    const fn render_failed(mut self) -> Self {
        self.status = CrossSectionPanelScheduleStatus::Unavailable;
        self.reason = CrossSectionPanelScheduleReason::RenderFailed;
        self
    }

    pub const fn is_renderable(self) -> bool {
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentRenderFailureStatus {
    kind: FrameFailureKind,
    message: String,
}

impl ResidentRenderFailureStatus {
    pub fn new(kind: FrameFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub const fn kind(&self) -> FrameFailureKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderSurfaceState {
    presentation_viewport: Option<PresentationViewport>,
    render_viewport: Option<RenderExtent>,
    generation: u64,
    displayed_generation: Option<u64>,
    cross_section_schedule: Option<CrossSectionPanelScheduleState>,
    render_failure: Option<ResidentRenderFailureStatus>,
}

impl RenderSurfaceState {
    const fn new(cross_section: bool) -> Self {
        Self {
            presentation_viewport: None,
            render_viewport: None,
            generation: 0,
            displayed_generation: None,
            cross_section_schedule: if cross_section {
                Some(CrossSectionPanelScheduleState::missing_viewport(0))
            } else {
                None
            },
            render_failure: None,
        }
    }

    pub const fn presentation_viewport(&self) -> Option<PresentationViewport> {
        self.presentation_viewport
    }

    pub const fn render_viewport(&self) -> Option<RenderExtent> {
        self.render_viewport
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn displayed_generation(&self) -> Option<u64> {
        self.displayed_generation
    }

    pub const fn cross_section_schedule(&self) -> Option<CrossSectionPanelScheduleState> {
        self.cross_section_schedule
    }

    pub const fn render_failure(&self) -> Option<&ResidentRenderFailureStatus> {
        self.render_failure.as_ref()
    }

    pub fn display_current(&self) -> bool {
        self.displayed_generation == Some(self.generation)
    }

    fn record_viewports(
        &mut self,
        presentation_viewport: PresentationViewport,
        render_viewport: RenderExtent,
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderCoordinationState {
    surfaces: [RenderSurfaceState; 4],
}

impl Default for RenderCoordinationState {
    fn default() -> Self {
        Self {
            surfaces: [
                RenderSurfaceState::new(false),
                RenderSurfaceState::new(true),
                RenderSurfaceState::new(true),
                RenderSurfaceState::new(true),
            ],
        }
    }
}

impl RenderCoordinationState {
    pub fn surface(&self, slot: PresentationSlot) -> &RenderSurfaceState {
        &self.surfaces[slot.index()]
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (PresentationSlot, &RenderSurfaceState)> {
        PresentationSlot::ALL
            .into_iter()
            .map(|slot| (slot, self.surface(slot)))
    }

    pub fn record_viewports(
        &mut self,
        slot: PresentationSlot,
        presentation_viewport: PresentationViewport,
        render_viewport: RenderExtent,
    ) -> bool {
        self.surfaces[slot.index()].record_viewports(presentation_viewport, render_viewport)
    }

    pub fn invalidate_cross_sections(&mut self) -> bool {
        for slot in [
            PresentationSlot::Xy,
            PresentationSlot::Xz,
            PresentationSlot::Yz,
        ] {
            self.surfaces[slot.index()].advance_generation();
        }
        true
    }

    pub fn set_cross_section_schedule(
        &mut self,
        slot: PresentationSlot,
        schedule: CrossSectionPanelScheduleState,
    ) -> bool {
        if !slot.is_cross_section() {
            return false;
        }
        let surface = &mut self.surfaces[slot.index()];
        if schedule.generation != surface.generation {
            return false;
        }
        surface.cross_section_schedule = Some(schedule);
        true
    }

    pub fn record_cross_section_presentation(
        &mut self,
        slot: PresentationSlot,
        generation: u64,
        schedule: CrossSectionPanelScheduleState,
    ) -> bool {
        if !slot.is_cross_section() {
            return false;
        }
        let surface = &mut self.surfaces[slot.index()];
        if generation != surface.generation || schedule.generation != surface.generation {
            return false;
        }
        surface.displayed_generation = Some(generation);
        surface.cross_section_schedule = Some(schedule.rendered());
        surface.render_failure = None;
        true
    }

    pub fn record_cross_section_failure(
        &mut self,
        slot: PresentationSlot,
        schedule: CrossSectionPanelScheduleState,
        failure: ResidentRenderFailureStatus,
    ) -> bool {
        if !slot.is_cross_section() {
            return false;
        }
        let surface = &mut self.surfaces[slot.index()];
        if schedule.generation != surface.generation {
            return false;
        }
        surface.cross_section_schedule = Some(schedule.render_failed());
        surface.render_failure = Some(failure);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_fidelity_starts_in_an_explicit_loading_state() {
        let viewport = RenderExtent::new(1280, 720).unwrap();
        let presentation_viewport = PresentationViewport::new(1280.0, 720.0).unwrap();

        let status = FrameFidelityStatus::new_with_presentation(viewport, presentation_viewport);

        assert_eq!(status.viewport, viewport);
        assert_eq!(status.presentation_viewport, presentation_viewport);
        assert_eq!(status.completeness, FrameCompleteness::Loading);
        assert_eq!(status.reason, LodDecisionReason::LoadingTargetScale);
        assert_eq!(status.backend, RenderBackend::Loading);
        assert_eq!(status.display_freshness, DisplayedFrameFreshness::Unknown);
        assert_eq!(status.displayed_scale_level, None);
        assert_eq!(status.last_failure_kind, None);
        assert_eq!(status.last_capacity_error, None);
    }

    fn viewports() -> (PresentationViewport, RenderExtent) {
        (
            PresentationViewport::new(240.0, 180.0).unwrap(),
            RenderExtent::new(480, 360).unwrap(),
        )
    }

    #[test]
    fn viewport_changes_advance_generation_and_invalidate_display() {
        let mut state = RenderCoordinationState::default();
        let (presentation, render) = viewports();
        assert!(state.record_viewports(PresentationSlot::Xy, presentation, render));
        let generation = state.surface(PresentationSlot::Xy).generation();
        let schedule = CrossSectionPanelScheduleState::missing_viewport(generation);
        assert!(state.record_cross_section_presentation(
            PresentationSlot::Xy,
            generation,
            schedule
        ));
        assert!(state.surface(PresentationSlot::Xy).display_current());

        let next_render = RenderExtent::new(640, 360).unwrap();
        assert!(state.record_viewports(PresentationSlot::Xy, presentation, next_render));
        let surface = state.surface(PresentationSlot::Xy);
        assert_eq!(surface.generation(), generation + 1);
        assert!(!surface.display_current());
        assert_eq!(
            surface.cross_section_schedule().unwrap().reason,
            CrossSectionPanelScheduleReason::ResidentFramePending
        );
    }

    #[test]
    fn identical_viewports_do_not_advance_generation() {
        let mut state = RenderCoordinationState::default();
        let (presentation, render) = viewports();
        assert!(state.record_viewports(PresentationSlot::Xy, presentation, render));
        let generation = state.surface(PresentationSlot::Xy).generation();
        assert!(!state.record_viewports(PresentationSlot::Xy, presentation, render));
        assert_eq!(state.surface(PresentationSlot::Xy).generation(), generation);
    }

    #[test]
    fn stale_presentation_schedule_and_failure_updates_are_rejected_atomically() {
        let mut state = RenderCoordinationState::default();
        let (presentation, render) = viewports();
        assert!(state.record_viewports(PresentationSlot::Xy, presentation, render));
        let generation = state.surface(PresentationSlot::Xy).generation();
        let stale = CrossSectionPanelScheduleState::missing_viewport(generation - 1);
        let failure = ResidentRenderFailureStatus::new(
            FrameFailureKind::BudgetExceeded,
            "cross-section GPU budget exceeded",
        );

        assert!(!state.set_cross_section_schedule(PresentationSlot::Xy, stale));
        assert!(!state.record_cross_section_presentation(
            PresentationSlot::Xy,
            generation - 1,
            stale
        ));
        assert!(!state.record_cross_section_failure(PresentationSlot::Xy, stale, failure));
        let surface = state.surface(PresentationSlot::Xy);
        assert_eq!(surface.displayed_generation(), None);
        assert_eq!(surface.render_failure(), None);
        assert_ne!(surface.cross_section_schedule(), Some(stale));
    }

    #[test]
    fn invalidating_cross_sections_leaves_three_d_generation_unchanged() {
        let mut state = RenderCoordinationState::default();
        let three_d = state.surface(PresentationSlot::ThreeD).generation();
        assert!(state.invalidate_cross_sections());
        assert_eq!(
            state.surface(PresentationSlot::ThreeD).generation(),
            three_d
        );
        for slot in [
            PresentationSlot::Xy,
            PresentationSlot::Xz,
            PresentationSlot::Yz,
        ] {
            assert_eq!(state.surface(slot).generation(), 1);
        }
    }

    #[test]
    fn render_failures_are_generation_scoped_and_cleared_by_invalidation() {
        let mut state = RenderCoordinationState::default();
        let generation = state.surface(PresentationSlot::Xy).generation();
        let schedule = CrossSectionPanelScheduleState::missing_viewport(generation);
        let failure = ResidentRenderFailureStatus::new(
            FrameFailureKind::BudgetExceeded,
            "cross-section GPU budget exceeded",
        );
        assert!(state.record_cross_section_failure(
            PresentationSlot::Xy,
            schedule,
            failure.clone()
        ));
        assert!(
            state
                .surface(PresentationSlot::Xy)
                .render_failure()
                .is_some()
        );
        assert!(state.invalidate_cross_sections());
        assert!(
            state
                .surface(PresentationSlot::Xy)
                .render_failure()
                .is_none()
        );
        assert!(!state.record_cross_section_failure(PresentationSlot::Xy, schedule, failure));
    }

    #[test]
    fn three_d_rejects_cross_section_schedule_and_failure_updates() {
        let mut state = RenderCoordinationState::default();
        let schedule = CrossSectionPanelScheduleState::missing_viewport(0);
        let failure = ResidentRenderFailureStatus::new(
            FrameFailureKind::InvalidModeParameter,
            "not a cross-section",
        );
        assert!(!state.set_cross_section_schedule(PresentationSlot::ThreeD, schedule));
        assert!(!state.record_cross_section_presentation(PresentationSlot::ThreeD, 0, schedule));
        assert!(!state.record_cross_section_failure(PresentationSlot::ThreeD, schedule, failure));
        assert_eq!(
            state
                .surface(PresentationSlot::ThreeD)
                .cross_section_schedule(),
            None
        );
    }
}
