//! Framework-neutral status for coordinating progressive product presentation.

use mirante4d_render_api::{PresentationViewport, RenderExtent};

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
}
