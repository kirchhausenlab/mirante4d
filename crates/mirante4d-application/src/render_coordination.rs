//! Framework-neutral status for coordinating progressive product presentation.

use mirante4d_render_api::{MAX_RENDER_LAYERS, PresentationViewport, PresentedFrame, RenderExtent};

use crate::PresentationSlot;

/// Bounded timing history for long interactive or loading sequences.
///
/// The capacity matches the product automation input-sequence bound while
/// keeping recording allocation-free.
pub const DISPLAY_TIMING_SAMPLE_CAPACITY: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderBackend {
    Loading,
    Empty,
    GpuCameraMip,
    GpuCameraIso,
    GpuCameraDvr,
    GpuCameraMixed,
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
    AdaptiveCapacity,
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
    /// Uniform projected scale, or `None` for an empty or mixed visible set.
    /// Per-layer truth is carried by `LayerPresentationStatus`.
    pub ideal_scale_level: Option<u32>,
    /// Uniform selected scale, or `None` for an empty or mixed visible set.
    pub target_scale_level: Option<u32>,
    pub displayed_scale_level: Option<u32>,
    pub adaptive_capacity_limited: bool,
    pub refinement_pending: bool,
    pub completeness: FrameCompleteness,
    pub reason: LodDecisionReason,
    pub backend: RenderBackend,
    /// Physical output extent of the logical 3D panel.
    pub viewport: RenderExtent,
    /// Actual renderer-owned extent of the currently visible 3D texture.
    pub three_d_render_viewport: RenderExtent,
    /// The visible 3D texture is a complete provisional preview rather than
    /// the selected exact presentation body.
    pub three_d_preview: bool,
    /// Bounded native-resolution candidate set considered for the latest 3D
    /// preview decision.
    pub three_d_preview_candidate_count: u32,
    pub three_d_preview_resident_candidate_count: u32,
    pub three_d_preview_safe_candidate_count: u32,
    /// The visible preview is the finest complete, target-eligible candidate
    /// inside the fixed interaction-work envelope.
    pub three_d_preview_is_finest_safe: bool,
    /// No candidate satisfied the work envelope, so the unconditional
    /// complete full-volume emergency body was selected.
    pub three_d_preview_uses_emergency_floor: bool,
    /// Completed hidden exact screen tiles. Hidden work is never counted as
    /// visible presentation progress.
    pub three_d_refinement_strips_completed: u32,
    pub three_d_refinement_strips_total: u32,
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
            ideal_scale_level: None,
            target_scale_level: None,
            displayed_scale_level: None,
            adaptive_capacity_limited: false,
            refinement_pending: false,
            completeness: FrameCompleteness::Loading,
            reason: LodDecisionReason::LoadingTargetScale,
            backend: RenderBackend::Loading,
            viewport,
            three_d_render_viewport: viewport,
            three_d_preview: false,
            three_d_preview_candidate_count: 0,
            three_d_preview_resident_candidate_count: 0,
            three_d_preview_safe_candidate_count: 0,
            three_d_preview_is_finest_safe: false,
            three_d_preview_uses_emergency_floor: false,
            three_d_refinement_strips_completed: 0,
            three_d_refinement_strips_total: 0,
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
    Provisional,
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
    ResidentInteraction,
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
        let resident_interaction =
            matches!(self.status, CrossSectionPanelScheduleStatus::Provisional);
        self.status = if resident_interaction {
            CrossSectionPanelScheduleStatus::Provisional
        } else if matches!(self.status, CrossSectionPanelScheduleStatus::Incomplete)
            || self.missing_occupied_bricks > 0
        {
            CrossSectionPanelScheduleStatus::Incomplete
        } else if self.fallback_scale_level.is_some() {
            CrossSectionPanelScheduleStatus::Coarse
        } else {
            CrossSectionPanelScheduleStatus::Current
        };
        self.reason = if resident_interaction {
            CrossSectionPanelScheduleReason::ResidentInteraction
        } else {
            CrossSectionPanelScheduleReason::Rendered
        };
        self
    }

    pub const fn provisional(mut self) -> Self {
        self.status = CrossSectionPanelScheduleStatus::Provisional;
        self.reason = CrossSectionPanelScheduleReason::ResidentInteraction;
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
            CrossSectionPanelScheduleStatus::Ready
                | CrossSectionPanelScheduleStatus::Provisional
                | CrossSectionPanelScheduleStatus::Coarse
        ) && self.missing_occupied_bricks == 0)
            || (matches!(
                self.status,
                CrossSectionPanelScheduleStatus::Loading
                    | CrossSectionPanelScheduleStatus::Incomplete
            ) && has_current_partial)
    }

    fn same_render_contract(self, other: Self) -> bool {
        self.generation == other.generation
            && self.target_scale_level == other.target_scale_level
            && self.render_scale_level == other.render_scale_level
            && self.fallback_scale_level == other.fallback_scale_level
            && self.selected_bricks == other.selected_bricks
            && self.occupied_selected_bricks == other.occupied_selected_bricks
            && self.missing_occupied_bricks == other.missing_occupied_bricks
            && self.estimated_decoded_bytes == other.estimated_decoded_bytes
            && self.decoded_budget_bytes == other.decoded_budget_bytes
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
    /// One scalar is present only when the displayed layer is uniform.
    pub displayed_scale_level: Option<u32>,
    pub target_scale_level: Option<u32>,
    /// Finest eligible non-target level. With `fallback_scale_level`, this is
    /// the truthful range when shared residency makes one scalar unprovable.
    pub finest_fallback_scale_level: Option<u32>,
    pub fallback_scale_level: Option<u32>,
    pub target_available_requirements: u64,
    pub target_total_requirements: u64,
    pub available_requirements: u64,
    pub total_requirements: u64,
    pub mixed: bool,
    pub current: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerPresentationOverflow {
    pub actual: usize,
    pub maximum: usize,
}

/// Targets that must all be semantically complete/current before one admitted
/// display generation can be counted as a coordinated publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinatedPresentationGroup {
    ThreeD,
    Linked2d,
    FullLayout,
}

impl CoordinatedPresentationGroup {
    pub const fn name(self) -> &'static str {
        match self {
            Self::ThreeD => "three_d",
            Self::Linked2d => "linked_2d",
            Self::FullLayout => "full_layout",
        }
    }
}

/// Monotonic, framework-neutral counters needed to identify a display freeze.
/// No wall-clock or serializable `Instant` is retained here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisplayGenerationStatus {
    pub input_generation: u64,
    pub current_presentation_generation: Option<u64>,
    pub main_loop_heartbeat: u64,
    pub input_generation_at_ns: u64,
    pub current_presentation_at_ns: Option<u64>,
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
    samples: Box<[u64]>,
    retained_count: usize,
    next: usize,
    total_count: u64,
    overwritten_count: u64,
    maximum_ns: u64,
}

impl Default for DisplayTimingSamples {
    fn default() -> Self {
        Self {
            samples: vec![0; DISPLAY_TIMING_SAMPLE_CAPACITY].into_boxed_slice(),
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

    fn clear(&mut self) {
        self.retained_count = 0;
        self.next = 0;
        self.total_count = 0;
        self.overwritten_count = 0;
        self.maximum_ns = 0;
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

/// One fixed-capacity aggregate for coordinated publication timing.
///
/// Admission latency and publication cadence retain current cutoffs during
/// the active gesture plus its generation-bound final durable cutoff when
/// that cutoff becomes current after dispatch ends. In particular, admitting
/// another input never resets the gap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatedPublicationDiagnostics {
    required_group: CoordinatedPresentationGroup,
    current_presentation_generation: Option<u64>,
    current_presentation_at_ns: Option<u64>,
    total_current_publications: u64,
    total_superseded_before_current_publication: u64,
    window_group: Option<CoordinatedPresentationGroup>,
    window_started_at_ns: Option<u64>,
    window_ended_at_ns: Option<u64>,
    final_input_generation: Option<u64>,
    last_active_publication_at_ns: Option<u64>,
    active_publication_count: u64,
    maximum_active_publication_gap_ns: u64,
    window_group_mismatches: u64,
    window_transition_errors: u64,
    admission_latency_samples: DisplayTimingSamples,
    publication_interval_samples: DisplayTimingSamples,
}

impl Default for CoordinatedPublicationDiagnostics {
    fn default() -> Self {
        Self {
            required_group: CoordinatedPresentationGroup::FullLayout,
            current_presentation_generation: None,
            current_presentation_at_ns: None,
            total_current_publications: 0,
            total_superseded_before_current_publication: 0,
            window_group: None,
            window_started_at_ns: None,
            window_ended_at_ns: None,
            final_input_generation: None,
            last_active_publication_at_ns: None,
            active_publication_count: 0,
            maximum_active_publication_gap_ns: 0,
            window_group_mismatches: 0,
            window_transition_errors: 0,
            admission_latency_samples: DisplayTimingSamples::default(),
            publication_interval_samples: DisplayTimingSamples::default(),
        }
    }
}

impl CoordinatedPublicationDiagnostics {
    pub const fn required_group(&self) -> CoordinatedPresentationGroup {
        self.required_group
    }

    pub const fn current_presentation_generation(&self) -> Option<u64> {
        self.current_presentation_generation
    }

    pub const fn current_presentation_at_ns(&self) -> Option<u64> {
        self.current_presentation_at_ns
    }

    pub const fn total_current_publications(&self) -> u64 {
        self.total_current_publications
    }

    pub const fn total_superseded_before_current_publication(&self) -> u64 {
        self.total_superseded_before_current_publication
    }

    pub const fn window_group(&self) -> Option<CoordinatedPresentationGroup> {
        self.window_group
    }

    pub const fn window_started_at_ns(&self) -> Option<u64> {
        self.window_started_at_ns
    }

    pub const fn window_ended_at_ns(&self) -> Option<u64> {
        self.window_ended_at_ns
    }

    pub const fn window_active(&self) -> bool {
        self.window_started_at_ns.is_some() && self.window_ended_at_ns.is_none()
    }

    pub const fn final_input_generation(&self) -> Option<u64> {
        self.final_input_generation
    }

    pub const fn active_publication_count(&self) -> u64 {
        self.active_publication_count
    }

    pub const fn maximum_active_publication_gap_ns(&self) -> u64 {
        self.maximum_active_publication_gap_ns
    }

    pub const fn window_group_mismatches(&self) -> u64 {
        self.window_group_mismatches
    }

    pub const fn window_transition_errors(&self) -> u64 {
        self.window_transition_errors
    }

    pub const fn admission_latency_samples(&self) -> &DisplayTimingSamples {
        &self.admission_latency_samples
    }

    pub const fn publication_interval_samples(&self) -> &DisplayTimingSamples {
        &self.publication_interval_samples
    }

    pub const fn samples_complete(&self) -> bool {
        self.admission_latency_samples.overwritten_count() == 0
            && self.publication_interval_samples.overwritten_count() == 0
    }

    pub const fn window_well_formed(&self) -> bool {
        self.window_transition_errors == 0 && self.window_group_mismatches == 0
    }

    fn begin_window(&mut self, now_ns: u64, group: CoordinatedPresentationGroup) {
        let replaced_active_window = u64::from(self.window_active());
        self.window_group = Some(group);
        self.window_started_at_ns = Some(now_ns);
        self.window_ended_at_ns = None;
        self.final_input_generation = None;
        self.last_active_publication_at_ns = None;
        self.active_publication_count = 0;
        self.maximum_active_publication_gap_ns = 0;
        self.window_group_mismatches = 0;
        self.window_transition_errors = replaced_active_window;
        self.admission_latency_samples.clear();
        self.publication_interval_samples.clear();
    }

    fn end_window(&mut self, now_ns: u64, input_generation: u64) {
        if !self.window_active() {
            self.window_transition_errors = self.window_transition_errors.saturating_add(1);
            return;
        }
        let segment_started_at = self
            .last_active_publication_at_ns
            .or(self.window_started_at_ns)
            .unwrap_or(now_ns);
        self.maximum_active_publication_gap_ns = self
            .maximum_active_publication_gap_ns
            .max(now_ns.saturating_sub(segment_started_at));
        self.window_ended_at_ns = Some(now_ns);
        self.final_input_generation = Some(input_generation);
    }

    fn note_admitted_generation(
        &mut self,
        previous_generation: u64,
        generation: u64,
        group: CoordinatedPresentationGroup,
    ) {
        if previous_generation > 0
            && self.current_presentation_generation != Some(previous_generation)
        {
            self.total_superseded_before_current_publication = self
                .total_superseded_before_current_publication
                .saturating_add(1);
        }
        self.required_group = group;
        if self.window_active() && self.window_group != Some(group) {
            self.window_group_mismatches = self.window_group_mismatches.saturating_add(1);
        }
        debug_assert!(generation >= previous_generation);
    }

    fn record_current_publication(
        &mut self,
        generation: u64,
        input_generation_at_ns: u64,
        now_ns: u64,
    ) {
        if generation == 0 || self.current_presentation_generation == Some(generation) {
            return;
        }
        self.current_presentation_generation = Some(generation);
        self.current_presentation_at_ns = Some(now_ns);
        self.total_current_publications = self.total_current_publications.saturating_add(1);

        let publication_matches_window = self.window_group == Some(self.required_group);
        let final_publication_after_window =
            self.window_ended_at_ns.is_some() && self.final_input_generation == Some(generation);
        if publication_matches_window && (self.window_active() || final_publication_after_window) {
            self.admission_latency_samples
                .record(now_ns.saturating_sub(input_generation_at_ns));
        }

        if !publication_matches_window || (!self.window_active() && !final_publication_after_window)
        {
            return;
        }
        let segment_started_at = self
            .last_active_publication_at_ns
            .or(self.window_started_at_ns)
            .unwrap_or(now_ns);
        self.maximum_active_publication_gap_ns = self
            .maximum_active_publication_gap_ns
            .max(now_ns.saturating_sub(segment_started_at));
        if let Some(previous) = self.last_active_publication_at_ns {
            self.publication_interval_samples
                .record(now_ns.saturating_sub(previous));
        }
        self.last_active_publication_at_ns = Some(now_ns);
        self.active_publication_count = self.active_publication_count.saturating_add(1);
    }
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
    presented_frame: Option<PresentedFrame>,
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
            presented_frame: None,
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

    pub const fn presented_frame(&self) -> Option<&PresentedFrame> {
        self.presented_frame.as_ref()
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
    coordinated_publication: CoordinatedPublicationDiagnostics,
    last_main_loop_heartbeat_at_ns: Option<u64>,
    presentation_latency_samples: DisplayTimingSamples,
    interaction_task_duration_samples: DisplayTimingSamples,
    active_ui_update_duration_samples: DisplayTimingSamples,
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
            coordinated_publication: CoordinatedPublicationDiagnostics::default(),
            last_main_loop_heartbeat_at_ns: None,
            presentation_latency_samples: DisplayTimingSamples::default(),
            interaction_task_duration_samples: DisplayTimingSamples::default(),
            active_ui_update_duration_samples: DisplayTimingSamples::default(),
        }
    }

    pub const fn display_generation(&self) -> DisplayGenerationStatus {
        self.display_generation
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

    pub const fn coordinated_publication_diagnostics(&self) -> &CoordinatedPublicationDiagnostics {
        &self.coordinated_publication
    }

    pub const fn required_coordinated_presentation_group(&self) -> CoordinatedPresentationGroup {
        self.coordinated_publication.required_group()
    }

    pub fn record_interaction_task_duration(&mut self, duration_ns: u64) {
        self.interaction_task_duration_samples.record(duration_ns);
    }

    pub fn record_active_ui_update_duration(&mut self, duration_ns: u64) {
        self.active_ui_update_duration_samples.record(duration_ns);
    }

    /// Records one admitted effective display input and its currentness group.
    pub fn begin_display_input_generation(
        &mut self,
        now_ns: u64,
        group: CoordinatedPresentationGroup,
    ) -> u64 {
        let previous = self.display_generation.input_generation;
        self.display_generation.input_generation = previous.saturating_add(1);
        self.display_generation.input_generation_at_ns = now_ns;
        self.coordinated_publication.note_admitted_generation(
            previous,
            self.display_generation.input_generation,
            group,
        );
        self.display_generation.input_generation
    }

    pub fn begin_active_publication_window(
        &mut self,
        now_ns: u64,
        group: CoordinatedPresentationGroup,
    ) {
        self.coordinated_publication.begin_window(now_ns, group);
    }

    pub fn end_active_publication_window(&mut self, now_ns: u64) {
        self.coordinated_publication
            .end_window(now_ns, self.display_generation.input_generation);
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
    }

    /// Marks the newest admitted generation current. Repeated progressive
    /// publications for the same generation are idempotent.
    pub fn record_current_presentation(&mut self, now_ns: u64) {
        let generation = self.display_generation.input_generation;
        if self.display_generation.current_presentation_generation == Some(generation) {
            return;
        }
        self.display_generation.current_presentation_generation = Some(generation);
        self.display_generation.current_presentation_at_ns = Some(now_ns);
        if generation > 0 {
            self.presentation_latency_samples
                .record(now_ns.saturating_sub(self.display_generation.input_generation_at_ns));
        }
    }

    /// Marks the newest admitted generation current for its required
    /// synchronization group. Repeated observations of one generation are
    /// idempotent and cannot double-count panel publications.
    pub fn record_current_group_presentation(&mut self, now_ns: u64) {
        self.coordinated_publication.record_current_publication(
            self.display_generation.input_generation,
            self.display_generation.input_generation_at_ns,
            now_ns,
        );
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

    /// Records a semantic frame only for the surface generation and extent it
    /// was rendered against. Renderer target allocation and texture revision
    /// remain owned by the native renderer.
    pub fn record_presented_frame(
        &mut self,
        slot: PresentationSlot,
        generation: u64,
        frame: PresentedFrame,
    ) -> bool {
        let surface = &mut self.surfaces[slot.index()];
        if frame.target() != slot
            || generation != surface.generation
            || surface.render_viewport != Some(frame.extent())
        {
            return false;
        }
        surface.presented_frame = Some(frame);
        surface.displayed_generation = Some(generation);
        true
    }

    pub fn clear_presented_frame(&mut self, slot: PresentationSlot) {
        let surface = &mut self.surfaces[slot.index()];
        surface.presented_frame = None;
        surface.displayed_generation = None;
    }

    /// Publishes a terminal no-data result for the current 3D surface without
    /// manufacturing a renderer frame. The surface generation check gives an
    /// empty publication the same stale-result protection as rendered pixels.
    pub fn record_empty_3d_presentation(&mut self, generation: u64) -> bool {
        let surface = &mut self.surfaces[PresentationSlot::ThreeD.index()];
        if generation != surface.generation {
            return false;
        }
        surface.presented_frame = None;
        surface.displayed_generation = Some(generation);
        surface.render_failure = None;
        surface.layer_presentations.clear();
        surface.layer_presentation_overflow = None;
        true
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
            self.invalidate_cross_section(slot);
        }
        true
    }

    pub fn invalidate_cross_section(&mut self, slot: PresentationSlot) -> bool {
        if !slot.is_cross_section() {
            return false;
        }
        self.surfaces[slot.index()].advance_generation();
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
        if schedule.status == CrossSectionPanelScheduleStatus::Ready
            && surface.display_current()
            && surface.cross_section_schedule.is_some_and(|displayed| {
                matches!(
                    displayed.status,
                    CrossSectionPanelScheduleStatus::Current
                        | CrossSectionPanelScheduleStatus::Provisional
                        | CrossSectionPanelScheduleStatus::Coarse
                        | CrossSectionPanelScheduleStatus::Incomplete
                ) && displayed.same_render_contract(schedule)
            })
        {
            // A prefetch-only wake may re-observe the already rendered
            // required prefix. Keep the rendered currentness authority until
            // a genuinely changed render contract arrives.
            return true;
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

    pub fn record_empty_cross_section_presentation(
        &mut self,
        slot: PresentationSlot,
        generation: u64,
        schedule: CrossSectionPanelScheduleState,
    ) -> bool {
        if !slot.is_cross_section()
            || schedule.status != CrossSectionPanelScheduleStatus::Empty
            || schedule.reason != CrossSectionPanelScheduleReason::NoSelectedData
        {
            return false;
        }
        let surface = &mut self.surfaces[slot.index()];
        if generation != surface.generation || schedule.generation != surface.generation {
            return false;
        }
        surface.displayed_generation = Some(generation);
        surface.cross_section_schedule = Some(schedule);
        surface.render_failure = None;
        surface.layer_presentations.clear();
        surface.layer_presentation_overflow = None;
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
    fn resident_interaction_presentation_remains_provisional_after_render() {
        let schedule = CrossSectionPanelScheduleState {
            status: CrossSectionPanelScheduleStatus::Ready,
            reason: CrossSectionPanelScheduleReason::TargetScaleReady,
            ..CrossSectionPanelScheduleState::missing_viewport(7)
        }
        .provisional()
        .rendered();

        assert_eq!(
            schedule.status,
            CrossSectionPanelScheduleStatus::Provisional
        );
        assert_eq!(
            schedule.reason,
            CrossSectionPanelScheduleReason::ResidentInteraction
        );
    }

    #[test]
    fn viewport_changes_advance_generation_and_retain_stale_display_identity() {
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
        assert_eq!(surface.displayed_generation(), Some(generation));
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
        state.clear_presented_frame(PresentationSlot::Xy);
        assert_eq!(
            state.surface(PresentationSlot::Xy).displayed_generation(),
            None,
            "removing the retained frame must remove its displayed-generation identity"
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
    fn full_layout_publication_clock_is_independent_of_main_loop_heartbeats() {
        let mut state = coordination_state();
        assert_eq!(
            state.begin_display_input_generation(100, CoordinatedPresentationGroup::FullLayout),
            1
        );
        state.record_main_loop_heartbeat(120);
        state.record_main_loop_heartbeat(150);
        let waiting = state.display_generation();
        assert_eq!(waiting.presentation_generation_gap(), 1);
        assert_eq!(waiting.raw_current_main_loop_heartbeat_gap_ns, 30);
        assert_eq!(waiting.raw_maximum_main_loop_heartbeat_gap_ns, 30);
        assert_eq!(
            state.required_coordinated_presentation_group(),
            CoordinatedPresentationGroup::FullLayout
        );
        assert_eq!(
            state
                .coordinated_publication_diagnostics()
                .active_publication_count(),
            0
        );

        state.record_current_presentation(160);
        state.record_main_loop_heartbeat(175);
        let current = state.display_generation();
        assert_eq!(current.current_presentation_generation, Some(1));
        assert_eq!(current.current_presentation_at_ns, Some(160));
        assert_eq!(state.presentation_latency_samples().sample(0), Some(60));
        assert_eq!(
            state
                .coordinated_publication_diagnostics()
                .current_presentation_generation(),
            None,
            "full-layout settlement and active-group publication are separate clocks"
        );
    }

    #[test]
    fn coordinated_publication_window_tracks_direct_gaps_and_final_latency() {
        let mut state = coordination_state();
        state.begin_active_publication_window(100, CoordinatedPresentationGroup::Linked2d);

        assert_eq!(
            state.begin_display_input_generation(105, CoordinatedPresentationGroup::Linked2d),
            1
        );
        assert_eq!(
            state.begin_display_input_generation(115, CoordinatedPresentationGroup::Linked2d),
            2
        );
        state.record_main_loop_heartbeat(120);
        state.record_current_group_presentation(125);
        state.record_current_group_presentation(130);

        assert_eq!(
            state.begin_display_input_generation(135, CoordinatedPresentationGroup::Linked2d),
            3
        );
        state.record_current_group_presentation(150);
        assert_eq!(
            state.begin_display_input_generation(160, CoordinatedPresentationGroup::Linked2d),
            4
        );
        assert_eq!(
            state.begin_display_input_generation(170, CoordinatedPresentationGroup::Linked2d),
            5
        );
        state.record_main_loop_heartbeat(190);
        state.end_active_publication_window(200);

        let ended = state.coordinated_publication_diagnostics();
        assert!(!ended.window_active());
        assert_eq!(ended.window_started_at_ns(), Some(100));
        assert_eq!(ended.window_ended_at_ns(), Some(200));
        assert_eq!(ended.final_input_generation(), Some(5));
        assert_eq!(ended.active_publication_count(), 2);
        assert_eq!(ended.maximum_active_publication_gap_ns(), 50);
        assert_eq!(ended.publication_interval_samples().total_count(), 1);
        assert_eq!(ended.publication_interval_samples().sample(0), Some(25));
        assert_eq!(ended.admission_latency_samples().total_count(), 2);
        assert_eq!(ended.admission_latency_samples().sample(0), Some(10));
        assert_eq!(ended.admission_latency_samples().sample(1), Some(15));
        assert_eq!(ended.total_superseded_before_current_publication(), 2);

        state.record_current_group_presentation(220);
        let settled = state.coordinated_publication_diagnostics();
        assert_eq!(settled.active_publication_count(), 3);
        assert_eq!(settled.maximum_active_publication_gap_ns(), 70);
        assert_eq!(settled.publication_interval_samples().total_count(), 2);
        assert_eq!(settled.publication_interval_samples().sample(1), Some(70));
        assert_eq!(settled.admission_latency_samples().total_count(), 3);
        assert_eq!(settled.admission_latency_samples().sample(2), Some(50));
        assert_eq!(settled.current_presentation_generation(), Some(5));
        assert_eq!(settled.current_presentation_at_ns(), Some(220));
        assert_eq!(settled.total_current_publications(), 3);
        assert!(settled.samples_complete());
        assert!(settled.window_well_formed());
    }

    #[test]
    fn final_only_publication_remains_insufficient_for_a_fixed_gesture() {
        let mut state = coordination_state();
        state.begin_active_publication_window(100, CoordinatedPresentationGroup::Linked2d);
        state.begin_display_input_generation(105, CoordinatedPresentationGroup::Linked2d);
        state.begin_display_input_generation(150, CoordinatedPresentationGroup::Linked2d);
        state.end_active_publication_window(200);
        state.record_current_group_presentation(225);

        let diagnostics = state.coordinated_publication_diagnostics();
        assert_eq!(diagnostics.active_publication_count(), 1);
        assert_eq!(diagnostics.maximum_active_publication_gap_ns(), 125);
        assert_eq!(diagnostics.publication_interval_samples().total_count(), 0);
        assert_eq!(diagnostics.admission_latency_samples().total_count(), 1);
        assert_eq!(diagnostics.admission_latency_samples().sample(0), Some(75));
        assert_eq!(diagnostics.total_superseded_before_current_publication(), 1);
    }

    #[test]
    fn admitting_input_never_resets_the_active_publication_gap() {
        let mut state = coordination_state();
        state.begin_active_publication_window(1_000, CoordinatedPresentationGroup::ThreeD);
        state.begin_display_input_generation(1_010, CoordinatedPresentationGroup::ThreeD);
        state.record_current_group_presentation(1_020);
        for now_ns in [1_030, 1_040, 1_050, 1_060] {
            state.begin_display_input_generation(now_ns, CoordinatedPresentationGroup::ThreeD);
        }
        state.end_active_publication_window(1_100);

        let diagnostics = state.coordinated_publication_diagnostics();
        assert_eq!(diagnostics.active_publication_count(), 1);
        assert_eq!(diagnostics.maximum_active_publication_gap_ns(), 80);
        assert_eq!(diagnostics.publication_interval_samples().total_count(), 0);
    }

    #[test]
    fn layer_presentation_overflow_is_flagged_without_replacing_current_facts() {
        let mut state = coordination_state();
        let current = LayerPresentationStatus {
            layer_ordinal: 7,
            expected_scale_level: Some(2),
            displayed_scale_level: Some(3),
            target_scale_level: Some(2),
            finest_fallback_scale_level: Some(3),
            fallback_scale_level: Some(3),
            target_available_requirements: 0,
            target_total_requirements: 5,
            available_requirements: 4,
            total_requirements: 5,
            mixed: false,
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

        state.begin_display_input_generation(1_000_010, CoordinatedPresentationGroup::Linked2d);
        state.record_main_loop_heartbeat(1_000_030);
        let active = state.display_generation();
        assert_eq!(active.raw_maximum_main_loop_heartbeat_gap_ns, 999_990);
        assert_eq!(
            state
                .coordinated_publication_diagnostics()
                .maximum_active_publication_gap_ns(),
            0,
            "heartbeat observation cannot manufacture an active publication gap"
        );
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

        samples.clear();
        assert_eq!(samples.retained_count(), 0);
        assert_eq!(samples.total_count(), 0);
        assert_eq!(samples.overwritten_count(), 0);
        assert_eq!(samples.maximum_ns(), 0);
        assert_eq!(samples.sample(0), None);
    }

    #[test]
    fn coordinated_publication_sample_overflow_invalidates_the_window() {
        let mut state = coordination_state();
        state.begin_active_publication_window(1, CoordinatedPresentationGroup::ThreeD);
        for index in 0..=DISPLAY_TIMING_SAMPLE_CAPACITY as u64 {
            let admitted_at = index.saturating_mul(10).saturating_add(2);
            state.begin_display_input_generation(admitted_at, CoordinatedPresentationGroup::ThreeD);
            state.record_current_group_presentation(admitted_at.saturating_add(1));
        }
        state.end_active_publication_window(
            (DISPLAY_TIMING_SAMPLE_CAPACITY as u64 + 1).saturating_mul(10),
        );

        let diagnostics = state.coordinated_publication_diagnostics();
        assert!(!diagnostics.samples_complete());
        assert_eq!(
            diagnostics.admission_latency_samples().overwritten_count(),
            1
        );
        assert_eq!(
            diagnostics
                .publication_interval_samples()
                .overwritten_count(),
            0
        );
    }
}
