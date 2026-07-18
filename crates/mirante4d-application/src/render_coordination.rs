//! Framework-neutral status for coordinating progressive product presentation.

use mirante4d_render_api::{MAX_RENDER_LAYERS, PresentationViewport, RenderExtent};

use crate::PresentationSlot;

pub const DISPLAY_TIMING_SAMPLE_CAPACITY: usize = 256;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayRefreshPath {
    GpuResidentDisplay,
    UiBackground,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayRefreshTiming {
    pub path: DisplayRefreshPath,
    pub render_ms: f64,
    pub gpu_upload_ms: Option<f64>,
    pub gpu_compute_ms: Option<f64>,
    pub egui_texture_ms: f64,
    pub visible_brick_request_ms: f64,
    pub total_ms: f64,
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

/// Always-on, bounded presentation facts for one logical render layer.
///
/// Coverage is expressed in exact resource requirements rather than pixels so
/// the application can report it without a renderer readback or framework
/// dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerPresentationStatus {
    pub layer_ordinal: u32,
    pub expected_scale_level: Option<u32>,
    pub displayed_scale_level: Option<u32>,
    pub available_requirements: u64,
    pub total_requirements: u64,
    pub current: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerPresentationOverflow {
    pub actual: usize,
    pub maximum: usize,
}

/// Monotonic, framework-neutral counters needed to identify a display freeze.
/// No wall-clock or serializable `Instant` is retained here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisplayGenerationStatus {
    pub input_generation: u64,
    pub current_presentation_generation: Option<u64>,
    pub main_loop_heartbeat: u64,
    pub heartbeat_at_input_generation: u64,
    pub heartbeat_at_current_presentation: u64,
    pub current_presentation_gap_heartbeats: u64,
    pub maximum_presentation_gap_heartbeats: u64,
    pub input_generation_at_ns: u64,
    pub current_presentation_at_ns: Option<u64>,
    pub current_presentation_gap_ns: u64,
    pub maximum_presentation_gap_ns: u64,
    pub current_main_loop_heartbeat_gap_ns: u64,
    pub maximum_main_loop_heartbeat_gap_ns: u64,
    pub raw_current_main_loop_heartbeat_gap_ns: u64,
    pub raw_maximum_main_loop_heartbeat_gap_ns: u64,
    pub durable_gesture_commits: u64,
}

impl DisplayGenerationStatus {
    pub fn presentation_generation_gap(self) -> u64 {
        self.input_generation
            .saturating_sub(self.current_presentation_generation.unwrap_or_default())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayTimingSamples {
    samples: [u64; DISPLAY_TIMING_SAMPLE_CAPACITY],
    retained_count: usize,
    next: usize,
    total_count: u64,
    overwritten_count: u64,
    maximum_ns: u64,
}

impl Default for DisplayTimingSamples {
    fn default() -> Self {
        Self {
            samples: [0; DISPLAY_TIMING_SAMPLE_CAPACITY],
            retained_count: 0,
            next: 0,
            total_count: 0,
            overwritten_count: 0,
            maximum_ns: 0,
        }
    }
}

impl DisplayTimingSamples {
    fn record(&mut self, duration_ns: u64) {
        self.samples[self.next] = duration_ns;
        self.next = (self.next + 1) % DISPLAY_TIMING_SAMPLE_CAPACITY;
        self.total_count = self.total_count.saturating_add(1);
        self.maximum_ns = self.maximum_ns.max(duration_ns);
        if self.retained_count < DISPLAY_TIMING_SAMPLE_CAPACITY {
            self.retained_count += 1;
        } else {
            self.overwritten_count = self.overwritten_count.saturating_add(1);
        }
    }

    pub const fn retained_count(&self) -> usize {
        self.retained_count
    }

    pub const fn total_count(&self) -> u64 {
        self.total_count
    }

    pub const fn overwritten_count(&self) -> u64 {
        self.overwritten_count
    }

    pub const fn maximum_ns(&self) -> u64 {
        self.maximum_ns
    }

    /// Returns retained samples oldest-first without allocating.
    pub fn sample(&self, chronological_index: usize) -> Option<u64> {
        if chronological_index >= self.retained_count {
            return None;
        }
        let start = if self.retained_count == DISPLAY_TIMING_SAMPLE_CAPACITY {
            self.next
        } else {
            0
        };
        Some(self.samples[(start + chronological_index) % DISPLAY_TIMING_SAMPLE_CAPACITY])
    }
}

/// Opt-in counters whose additional detail is reserved for qualification and
/// product automation. The always-on generation/heartbeat facts above remain
/// available even while this surface is disabled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisplayDiagnosticCounters {
    pub enabled: bool,
    pub raw_input_samples: u64,
    pub admitted_input_generations: u64,
    pub coalesced_input_samples: u64,
    pub superseded_input_generations: u64,
    pub current_presentations: u64,
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
    layer_presentations: Vec<LayerPresentationStatus>,
    layer_presentation_overflow: Option<LayerPresentationOverflow>,
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
            layer_presentations: Vec::new(),
            layer_presentation_overflow: None,
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

    pub fn layer_presentations(&self) -> &[LayerPresentationStatus] {
        &self.layer_presentations
    }

    pub const fn layer_presentation_overflow(&self) -> Option<LayerPresentationOverflow> {
        self.layer_presentation_overflow
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
    pub presentation_viewport: PresentationViewport,
    pub render_viewport: RenderExtent,
    pub frame_fidelity: FrameFidelityStatus,
    refresh_requested: bool,
    pub last_display_refresh_timing: Option<DisplayRefreshTiming>,
    surfaces: [RenderSurfaceState; 4],
    display_generation: DisplayGenerationStatus,
    display_diagnostic_counters: DisplayDiagnosticCounters,
    last_main_loop_heartbeat_at_ns: Option<u64>,
    active_last_main_loop_heartbeat_at_ns: Option<u64>,
    presentation_latency_samples: DisplayTimingSamples,
    interaction_task_duration_samples: DisplayTimingSamples,
    active_ui_update_duration_samples: DisplayTimingSamples,
    active_presentation_gap_samples: DisplayTimingSamples,
    active_main_loop_gap_samples: DisplayTimingSamples,
}

impl RenderCoordinationState {
    pub fn new(frame_fidelity: FrameFidelityStatus) -> Self {
        let presentation_viewport = frame_fidelity.presentation_viewport;
        let render_viewport = frame_fidelity.viewport;
        let mut three_d = RenderSurfaceState::new(false);
        three_d.presentation_viewport = Some(presentation_viewport);
        three_d.render_viewport = Some(render_viewport);
        Self {
            presentation_viewport,
            render_viewport,
            frame_fidelity,
            refresh_requested: false,
            last_display_refresh_timing: None,
            surfaces: [
                three_d,
                RenderSurfaceState::new(true),
                RenderSurfaceState::new(true),
                RenderSurfaceState::new(true),
            ],
            display_generation: DisplayGenerationStatus::default(),
            display_diagnostic_counters: DisplayDiagnosticCounters::default(),
            last_main_loop_heartbeat_at_ns: None,
            active_last_main_loop_heartbeat_at_ns: None,
            presentation_latency_samples: DisplayTimingSamples::default(),
            interaction_task_duration_samples: DisplayTimingSamples::default(),
            active_ui_update_duration_samples: DisplayTimingSamples::default(),
            active_presentation_gap_samples: DisplayTimingSamples::default(),
            active_main_loop_gap_samples: DisplayTimingSamples::default(),
        }
    }

    pub const fn display_generation(&self) -> DisplayGenerationStatus {
        self.display_generation
    }

    pub const fn display_diagnostic_counters(&self) -> DisplayDiagnosticCounters {
        self.display_diagnostic_counters
    }

    pub const fn presentation_latency_samples(&self) -> &DisplayTimingSamples {
        &self.presentation_latency_samples
    }

    pub const fn interaction_task_duration_samples(&self) -> &DisplayTimingSamples {
        &self.interaction_task_duration_samples
    }

    pub const fn active_ui_update_duration_samples(&self) -> &DisplayTimingSamples {
        &self.active_ui_update_duration_samples
    }

    pub const fn active_presentation_gap_samples(&self) -> &DisplayTimingSamples {
        &self.active_presentation_gap_samples
    }

    pub const fn active_main_loop_gap_samples(&self) -> &DisplayTimingSamples {
        &self.active_main_loop_gap_samples
    }

    pub fn record_interaction_task_duration(&mut self, duration_ns: u64) {
        self.interaction_task_duration_samples.record(duration_ns);
    }

    pub fn record_active_ui_update_duration(&mut self, duration_ns: u64) {
        self.active_ui_update_duration_samples.record(duration_ns);
    }

    pub fn set_display_diagnostic_counters_enabled(&mut self, enabled: bool) {
        self.display_diagnostic_counters.enabled = enabled;
    }

    /// Records one admitted effective display input. Replacing a generation
    /// before it becomes current is counted as supersession only when the
    /// detailed diagnostics surface is enabled.
    pub fn begin_display_input_generation(&mut self, now_ns: u64) -> u64 {
        let previous = self.display_generation.input_generation;
        if self.display_diagnostic_counters.enabled {
            self.display_diagnostic_counters.raw_input_samples = self
                .display_diagnostic_counters
                .raw_input_samples
                .saturating_add(1);
            self.display_diagnostic_counters.admitted_input_generations = self
                .display_diagnostic_counters
                .admitted_input_generations
                .saturating_add(1);
            if previous > 0
                && self.display_generation.current_presentation_generation != Some(previous)
            {
                self.display_diagnostic_counters
                    .superseded_input_generations = self
                    .display_diagnostic_counters
                    .superseded_input_generations
                    .saturating_add(1);
            }
        }
        self.display_generation.input_generation = previous.saturating_add(1);
        self.display_generation.heartbeat_at_input_generation =
            self.display_generation.main_loop_heartbeat;
        self.display_generation.current_presentation_gap_heartbeats = 0;
        self.display_generation.input_generation_at_ns = now_ns;
        self.display_generation.current_presentation_gap_ns = 0;
        self.display_generation.current_main_loop_heartbeat_gap_ns = 0;
        self.active_last_main_loop_heartbeat_at_ns = Some(now_ns);
        self.display_generation.input_generation
    }

    /// Adds raw samples that were intentionally folded into a later admitted
    /// generation. This is used by bounded framework-command automation; it
    /// does not claim or imply operating-system input injection.
    pub fn record_coalesced_input_samples(&mut self, samples: u64) {
        if !self.display_diagnostic_counters.enabled {
            return;
        }
        self.display_diagnostic_counters.raw_input_samples = self
            .display_diagnostic_counters
            .raw_input_samples
            .saturating_add(samples);
        self.display_diagnostic_counters.coalesced_input_samples = self
            .display_diagnostic_counters
            .coalesced_input_samples
            .saturating_add(samples);
    }

    pub fn record_durable_gesture_commit(&mut self) {
        self.display_generation.durable_gesture_commits = self
            .display_generation
            .durable_gesture_commits
            .saturating_add(1);
    }

    pub fn record_main_loop_heartbeat(&mut self, now_ns: u64) {
        let heartbeat_gap_ns = self
            .last_main_loop_heartbeat_at_ns
            .map_or(0, |previous| now_ns.saturating_sub(previous));
        self.last_main_loop_heartbeat_at_ns = Some(now_ns);
        self.display_generation
            .raw_current_main_loop_heartbeat_gap_ns = heartbeat_gap_ns;
        self.display_generation
            .raw_maximum_main_loop_heartbeat_gap_ns = self
            .display_generation
            .raw_maximum_main_loop_heartbeat_gap_ns
            .max(heartbeat_gap_ns);
        self.display_generation.main_loop_heartbeat = self
            .display_generation
            .main_loop_heartbeat
            .saturating_add(1);
        if self.display_generation.input_generation == 0
            || self.display_generation.current_presentation_generation
                == Some(self.display_generation.input_generation)
        {
            self.display_generation.current_presentation_gap_heartbeats = 0;
            self.display_generation.current_presentation_gap_ns = 0;
            self.display_generation.current_main_loop_heartbeat_gap_ns = 0;
            self.active_last_main_loop_heartbeat_at_ns = None;
            return;
        }
        let active_heartbeat_gap_ns = self
            .active_last_main_loop_heartbeat_at_ns
            .map_or(0, |previous| now_ns.saturating_sub(previous));
        self.active_last_main_loop_heartbeat_at_ns = Some(now_ns);
        self.display_generation.current_main_loop_heartbeat_gap_ns = active_heartbeat_gap_ns;
        self.display_generation.maximum_main_loop_heartbeat_gap_ns = self
            .display_generation
            .maximum_main_loop_heartbeat_gap_ns
            .max(active_heartbeat_gap_ns);
        let gap = self
            .display_generation
            .main_loop_heartbeat
            .saturating_sub(self.display_generation.heartbeat_at_input_generation);
        self.display_generation.current_presentation_gap_heartbeats = gap;
        self.display_generation.maximum_presentation_gap_heartbeats = self
            .display_generation
            .maximum_presentation_gap_heartbeats
            .max(gap);
        let gap_ns = now_ns.saturating_sub(self.display_generation.input_generation_at_ns);
        self.display_generation.current_presentation_gap_ns = gap_ns;
        self.display_generation.maximum_presentation_gap_ns = self
            .display_generation
            .maximum_presentation_gap_ns
            .max(gap_ns);
        if self.display_diagnostic_counters.enabled {
            self.active_main_loop_gap_samples
                .record(active_heartbeat_gap_ns);
            self.active_presentation_gap_samples.record(gap_ns);
        }
    }

    /// Marks the newest admitted generation current. Repeated progressive
    /// publications for the same generation are idempotent.
    pub fn record_current_presentation(&mut self, now_ns: u64) {
        let generation = self.display_generation.input_generation;
        if self.display_generation.current_presentation_generation == Some(generation) {
            return;
        }
        self.display_generation.current_presentation_generation = Some(generation);
        self.display_generation.heartbeat_at_current_presentation =
            self.display_generation.main_loop_heartbeat;
        self.display_generation.current_presentation_at_ns = Some(now_ns);
        self.display_generation.current_presentation_gap_heartbeats = 0;
        self.display_generation.current_presentation_gap_ns = 0;
        self.display_generation.current_main_loop_heartbeat_gap_ns = 0;
        self.active_last_main_loop_heartbeat_at_ns = None;
        if generation > 0 {
            self.presentation_latency_samples
                .record(now_ns.saturating_sub(self.display_generation.input_generation_at_ns));
        }
        if generation > 0 && self.display_diagnostic_counters.enabled {
            self.display_diagnostic_counters.current_presentations = self
                .display_diagnostic_counters
                .current_presentations
                .saturating_add(1);
        }
    }

    pub fn set_layer_presentations(
        &mut self,
        slot: PresentationSlot,
        presentations: Vec<LayerPresentationStatus>,
    ) -> Result<(), LayerPresentationOverflow> {
        let surface = &mut self.surfaces[slot.index()];
        if presentations.len() > MAX_RENDER_LAYERS {
            let overflow = LayerPresentationOverflow {
                actual: presentations.len(),
                maximum: MAX_RENDER_LAYERS,
            };
            surface.layer_presentation_overflow = Some(overflow);
            return Err(overflow);
        }
        surface.layer_presentations = presentations;
        surface.layer_presentation_overflow = None;
        Ok(())
    }

    pub fn set_presentation_viewport(&mut self, viewport: PresentationViewport) -> bool {
        self.record_viewports(PresentationSlot::ThreeD, viewport, self.render_viewport)
    }

    pub fn set_render_viewport(&mut self, viewport: RenderExtent) -> bool {
        self.record_viewports(
            PresentationSlot::ThreeD,
            self.presentation_viewport,
            viewport,
        )
    }

    pub fn request_refresh(&mut self) {
        self.refresh_requested = true;
    }

    /// Invalidates the global 3D presentation while preserving its last
    /// paintable pixels. Any successor view or extent must cross this state
    /// before it can be reported as current.
    pub fn mark_3d_display_stale(&mut self) {
        self.frame_fidelity.display_freshness = DisplayedFrameFreshness::Stale;
        self.frame_fidelity.completeness = FrameCompleteness::Loading;
        self.frame_fidelity.reason = LodDecisionReason::LoadingTargetScale;
        self.frame_fidelity.frame_time_ms = None;
        self.frame_fidelity.last_failure_kind = None;
        self.frame_fidelity.last_capacity_error = None;
        self.request_refresh();
    }

    pub const fn refresh_requested(&self) -> bool {
        self.refresh_requested
    }

    pub fn take_refresh_request(&mut self) -> bool {
        std::mem::take(&mut self.refresh_requested)
    }

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
        if slot == PresentationSlot::ThreeD {
            if self.presentation_viewport == presentation_viewport
                && self.render_viewport == render_viewport
            {
                return false;
            }
            self.presentation_viewport = presentation_viewport;
            self.render_viewport = render_viewport;
            self.frame_fidelity.presentation_viewport = presentation_viewport;
            self.frame_fidelity.viewport = render_viewport;
            self.mark_3d_display_stale();
        }
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
    fn render_coordination_starts_from_the_real_frame_status() {
        let viewport = RenderExtent::new(1280, 720).unwrap();
        let presentation_viewport = PresentationViewport::new(1280.0, 720.0).unwrap();

        let status = FrameFidelityStatus::new_with_presentation(viewport, presentation_viewport);
        let mut state = RenderCoordinationState::new(status);

        assert_eq!(state.render_viewport, viewport);
        assert_eq!(state.presentation_viewport, presentation_viewport);
        assert_eq!(
            state.frame_fidelity.completeness,
            FrameCompleteness::Loading
        );
        assert_eq!(
            state.frame_fidelity.reason,
            LodDecisionReason::LoadingTargetScale
        );
        assert_eq!(state.frame_fidelity.backend, RenderBackend::Loading);
        assert_eq!(
            state.frame_fidelity.display_freshness,
            DisplayedFrameFreshness::Unknown
        );
        assert_eq!(
            state.surface(PresentationSlot::ThreeD).render_viewport(),
            Some(viewport)
        );
        assert_eq!(
            state
                .surface(PresentationSlot::ThreeD)
                .presentation_viewport(),
            Some(presentation_viewport)
        );
        assert!(!state.take_refresh_request());
        state.request_refresh();
        assert!(state.take_refresh_request());
        assert!(!state.take_refresh_request());
    }

    fn viewports() -> (PresentationViewport, RenderExtent) {
        (
            PresentationViewport::new(240.0, 180.0).unwrap(),
            RenderExtent::new(480, 360).unwrap(),
        )
    }

    fn coordination_state() -> RenderCoordinationState {
        let (presentation, render) = viewports();
        RenderCoordinationState::new(FrameFidelityStatus::new_with_presentation(
            render,
            presentation,
        ))
    }

    #[test]
    fn viewport_changes_advance_generation_and_invalidate_display() {
        let mut state = coordination_state();
        state.frame_fidelity.display_freshness = DisplayedFrameFreshness::Current;
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
        assert_eq!(
            state.frame_fidelity.display_freshness,
            DisplayedFrameFreshness::Current,
            "a linked-panel extent must not invalidate the unrelated 3D frame"
        );
    }

    #[test]
    fn three_d_viewport_change_marks_the_old_frame_stale_before_refresh() {
        let mut state = coordination_state();
        state.frame_fidelity.display_freshness = DisplayedFrameFreshness::Current;
        state.frame_fidelity.completeness = FrameCompleteness::Exact;
        state.frame_fidelity.reason = LodDecisionReason::ExactS0;
        state.frame_fidelity.frame_time_ms = Some(2.0);
        state.frame_fidelity.last_failure_kind = Some(FrameFailureKind::AllocationFailed);
        state.frame_fidelity.last_capacity_error = Some("old failure".to_owned());
        let next_render = RenderExtent::new(640, 360).unwrap();

        assert!(state.set_render_viewport(next_render));

        assert_eq!(state.frame_fidelity.viewport, next_render);
        assert_eq!(
            state.frame_fidelity.display_freshness,
            DisplayedFrameFreshness::Stale
        );
        assert_eq!(
            state.frame_fidelity.completeness,
            FrameCompleteness::Loading
        );
        assert_eq!(
            state.frame_fidelity.reason,
            LodDecisionReason::LoadingTargetScale
        );
        assert_eq!(state.frame_fidelity.frame_time_ms, None);
        assert_eq!(state.frame_fidelity.last_failure_kind, None);
        assert_eq!(state.frame_fidelity.last_capacity_error, None);
        assert!(state.take_refresh_request());

        state.frame_fidelity.display_freshness = DisplayedFrameFreshness::Current;
        assert!(!state.set_render_viewport(next_render));
        assert_eq!(
            state.frame_fidelity.display_freshness,
            DisplayedFrameFreshness::Current,
            "an identical observation must not invalidate a current frame"
        );
        assert!(!state.take_refresh_request());

        let next_presentation = PresentationViewport::new(320.0, 200.0).unwrap();
        assert!(state.set_presentation_viewport(next_presentation));
        assert_eq!(
            state.frame_fidelity.display_freshness,
            DisplayedFrameFreshness::Stale,
            "presentation-space changes also require a successor 3D frame"
        );
        assert!(state.take_refresh_request());
    }

    #[test]
    fn identical_viewports_do_not_advance_generation() {
        let mut state = coordination_state();
        let (presentation, render) = viewports();
        assert!(state.record_viewports(PresentationSlot::Xy, presentation, render));
        let generation = state.surface(PresentationSlot::Xy).generation();
        assert!(!state.record_viewports(PresentationSlot::Xy, presentation, render));
        assert_eq!(state.surface(PresentationSlot::Xy).generation(), generation);
    }

    #[test]
    fn stale_presentation_schedule_and_failure_updates_are_rejected_atomically() {
        let mut state = coordination_state();
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
        let mut state = coordination_state();
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
        let mut state = coordination_state();
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
        let mut state = coordination_state();
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

    #[test]
    fn display_generation_heartbeat_exposes_and_clears_current_presentation_gap() {
        let mut state = coordination_state();
        state.set_display_diagnostic_counters_enabled(true);
        assert_eq!(state.begin_display_input_generation(100), 1);
        state.record_main_loop_heartbeat(120);
        state.record_main_loop_heartbeat(150);
        let waiting = state.display_generation();
        assert_eq!(waiting.presentation_generation_gap(), 1);
        assert_eq!(waiting.current_presentation_gap_heartbeats, 2);
        assert_eq!(waiting.maximum_presentation_gap_heartbeats, 2);
        assert_eq!(waiting.current_presentation_gap_ns, 50);
        assert_eq!(waiting.maximum_presentation_gap_ns, 50);
        assert_eq!(waiting.current_main_loop_heartbeat_gap_ns, 30);
        assert_eq!(waiting.maximum_main_loop_heartbeat_gap_ns, 30);
        assert_eq!(waiting.raw_current_main_loop_heartbeat_gap_ns, 30);
        assert_eq!(waiting.raw_maximum_main_loop_heartbeat_gap_ns, 30);

        state.record_current_presentation(160);
        state.record_main_loop_heartbeat(175);
        let current = state.display_generation();
        assert_eq!(current.current_presentation_generation, Some(1));
        assert_eq!(current.current_presentation_gap_heartbeats, 0);
        assert_eq!(current.maximum_presentation_gap_heartbeats, 2);
        assert_eq!(current.current_presentation_at_ns, Some(160));
        assert_eq!(current.current_presentation_gap_ns, 0);
        assert_eq!(current.maximum_presentation_gap_ns, 50);
        assert_eq!(state.presentation_latency_samples().sample(0), Some(60));
        assert_eq!(state.active_main_loop_gap_samples().sample(0), Some(20));
        assert_eq!(state.active_main_loop_gap_samples().sample(1), Some(30));
        assert_eq!(state.active_presentation_gap_samples().sample(0), Some(20));
        assert_eq!(state.active_presentation_gap_samples().sample(1), Some(50));
    }

    #[test]
    fn detailed_generation_counters_are_opt_in_and_count_supersession() {
        let mut state = coordination_state();
        state.begin_display_input_generation(1);
        assert_eq!(
            state.display_diagnostic_counters(),
            DisplayDiagnosticCounters::default()
        );

        state.set_display_diagnostic_counters_enabled(true);
        state.begin_display_input_generation(2);
        state.record_coalesced_input_samples(3);
        state.begin_display_input_generation(3);
        state.record_durable_gesture_commit();
        state.record_current_presentation(4);
        assert_eq!(
            state.display_diagnostic_counters(),
            DisplayDiagnosticCounters {
                enabled: true,
                raw_input_samples: 5,
                admitted_input_generations: 2,
                coalesced_input_samples: 3,
                superseded_input_generations: 2,
                current_presentations: 1,
            }
        );
        assert_eq!(state.display_generation().durable_gesture_commits, 1);
    }

    #[test]
    fn layer_presentation_overflow_is_flagged_without_replacing_current_facts() {
        let mut state = coordination_state();
        let current = LayerPresentationStatus {
            layer_ordinal: 7,
            expected_scale_level: Some(2),
            displayed_scale_level: Some(3),
            available_requirements: 4,
            total_requirements: 5,
            current: false,
        };
        state
            .set_layer_presentations(PresentationSlot::ThreeD, vec![current])
            .unwrap();
        let overflow = state
            .set_layer_presentations(
                PresentationSlot::ThreeD,
                vec![current; MAX_RENDER_LAYERS + 1],
            )
            .unwrap_err();
        assert_eq!(overflow.actual, MAX_RENDER_LAYERS + 1);
        assert_eq!(overflow.maximum, MAX_RENDER_LAYERS);
        let surface = state.surface(PresentationSlot::ThreeD);
        assert_eq!(surface.layer_presentations(), &[current]);
        assert_eq!(surface.layer_presentation_overflow(), Some(overflow));
    }

    #[test]
    fn settled_idle_heartbeat_gap_is_not_an_active_input_freeze_gap() {
        let mut state = coordination_state();
        state.record_main_loop_heartbeat(10);
        state.record_main_loop_heartbeat(1_000_000);
        let idle = state.display_generation();
        assert_eq!(idle.raw_maximum_main_loop_heartbeat_gap_ns, 999_990);
        assert_eq!(idle.maximum_main_loop_heartbeat_gap_ns, 0);

        state.begin_display_input_generation(1_000_010);
        state.record_main_loop_heartbeat(1_000_030);
        let active = state.display_generation();
        assert_eq!(active.current_main_loop_heartbeat_gap_ns, 20);
        assert_eq!(active.maximum_main_loop_heartbeat_gap_ns, 20);
        assert_eq!(active.raw_maximum_main_loop_heartbeat_gap_ns, 999_990);
    }

    #[test]
    fn timing_samples_are_fixed_capacity_and_return_oldest_first() {
        let mut samples = DisplayTimingSamples::default();
        for value in 0..(DISPLAY_TIMING_SAMPLE_CAPACITY as u64 + 3) {
            samples.record(value);
        }
        assert_eq!(samples.retained_count(), DISPLAY_TIMING_SAMPLE_CAPACITY);
        assert_eq!(
            samples.total_count(),
            DISPLAY_TIMING_SAMPLE_CAPACITY as u64 + 3
        );
        assert_eq!(samples.overwritten_count(), 3);
        assert_eq!(samples.sample(0), Some(3));
        assert_eq!(
            samples.sample(DISPLAY_TIMING_SAMPLE_CAPACITY - 1),
            Some(DISPLAY_TIMING_SAMPLE_CAPACITY as u64 + 2)
        );
    }
}
