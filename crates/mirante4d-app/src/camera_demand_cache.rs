//! Bounded latest-only planning for camera-dependent 3D dataset demand.
//!
//! Exact candidate testing and contribution sorting can touch the complete
//! 131,072-brick planning envelope. This owner keeps that work off the UI
//! thread. It has one running job, one replaceable pending job, and one
//! replaceable result; source/view currentness is represented by the opaque
//! revision allocated by the render-intent mailbox and checked before a
//! result is exposed.

use std::{
    collections::BTreeMap,
    fmt, io,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use mirante4d_application::RenderIntentRevision;
use mirante4d_dataset::{BrickKey, CpuByteLease, CpuByteLedger, DatasetCatalog};
use mirante4d_domain::{LogicalLayerKey, ScaleLevel, TimeIndex};
use mirante4d_project_model::ViewState;
use mirante4d_render_api::{
    PreparedRenderRequirements, PreparedResourceBody, PresentationViewport, RenderExtent,
    RenderLayerScaleChain,
};

use crate::dataset_demand_plan::{
    DatasetDemandPlan, DatasetDemandPlanCapacityError, DatasetDemandPlanLimits,
    NavigationLadderBaseline, PreparedAllocationReservations, PreparedDatasetDemandPlan,
    PreparedDemandRequirements, PreparedProgressiveDatasetDemandPlan, key_array_bytes,
    plan_adaptive_cross_section_panel_with_obligations_cancellable,
    plan_cross_section_navigation_floor_cancellable, plan_cross_section_playback_body_cancellable,
    plan_guarded_progressive_current_3d_with_obligations_cancellable,
    plane_guard_attempt_should_retry_exact,
};
#[cfg(test)]
use crate::dataset_demand_plan::{
    plan_cross_section_panel_attempt, plan_progressive_current_3d_cancellable,
};
use crate::playback_session::{PlaybackFrameContract, PlaybackTargetSet};
use crate::retained_leases::RetainedRequirementHandle;
use crate::semantic_demand::SemanticPlaneReuseEnvelope;
use crate::viewer_layout::PanelId;

const WORKER_NAME: &str = "mirante4d-camera-demand";
/// After cancellation, wait briefly for an input burst to settle before
/// starting the sole replacement. This turns sustained orbit traffic into
/// one latest request rather than repeatedly traversing stale candidates.
const REPLACEMENT_QUIET_PERIOD: Duration = Duration::from_millis(4);

pub(crate) struct CameraDemandRequest {
    revision: RenderIntentRevision,
    catalog: Arc<DatasetCatalog>,
    cpu_ledger: Arc<dyn CpuByteLedger>,
    view: ViewState,
    global_limits: DatasetDemandPlanLimits,
    current_3d: Option<Current3dDemandRequest>,
    cross_sections: Box<[CrossSectionDemandRequest]>,
    previous_renderer_requirement_union: RetainedRequirementHandle,
    preserve_previous_renderer_requirement_union: bool,
    unchanged_renderer_requirement_bodies: Box<[PreparedResourceBody]>,
    post_refinement_promotion_unchanged_bodies: Option<Box<[PreparedResourceBody]>>,
    fixed_playback_layer_scales: Option<Arc<BTreeMap<LogicalLayerKey, ScaleLevel>>>,
    temporal_frame_contract: Option<PlaybackFrameContract>,
}

pub(crate) struct Current3dDemandRequest {
    presentation: PresentationViewport,
    viewport: RenderExtent,
    limits: DatasetDemandPlanLimits,
    playback_active: bool,
    playback_timepoints: Box<[TimeIndex]>,
    playback_required_timepoint_count: usize,
    fixed_playback_layer_scales: Option<Arc<BTreeMap<LogicalLayerKey, ScaleLevel>>>,
    preserve_complete_presentation: bool,
    navigation_ladder_baseline: Option<NavigationLadderBaseline>,
    baselines: Current3dDemandBaselines,
}

pub(crate) struct Current3dDemandBaselines {
    current: ScopeDemandBaseline,
    refinement: ScopeDemandBaseline,
    playback: ScopeDemandBaseline,
}

impl Current3dDemandBaselines {
    pub(crate) fn new(
        current: ScopeDemandBaseline,
        refinement: ScopeDemandBaseline,
        playback: ScopeDemandBaseline,
    ) -> Self {
        Self {
            current,
            refinement,
            playback,
        }
    }

    #[cfg(test)]
    fn empty() -> Self {
        Self::new(
            ScopeDemandBaseline::empty(),
            ScopeDemandBaseline::empty(),
            ScopeDemandBaseline::empty(),
        )
    }
}

pub(crate) struct ScopeDemandBaseline {
    body: PreparedResourceBody,
    admitted_prefix_len: usize,
}

impl ScopeDemandBaseline {
    pub(crate) fn new(body: PreparedResourceBody, admitted_prefix_len: usize) -> Self {
        debug_assert!(admitted_prefix_len <= body.ranked().len());
        Self {
            body,
            admitted_prefix_len,
        }
    }

    fn admitted_prefix(&self) -> &[BrickKey] {
        &self.body.ranked()[..self.admitted_prefix_len.min(self.body.ranked().len())]
    }

    #[cfg(test)]
    fn empty() -> Self {
        Self::new(
            PreparedResourceBody::new(Arc::from([]), Arc::from([]), None).unwrap(),
            0,
        )
    }
}

impl Current3dDemandRequest {
    pub(crate) fn new(
        presentation: PresentationViewport,
        viewport: RenderExtent,
        limits: DatasetDemandPlanLimits,
        baselines: Current3dDemandBaselines,
    ) -> Self {
        Self {
            presentation,
            viewport,
            limits,
            playback_active: false,
            playback_timepoints: Box::new([]),
            playback_required_timepoint_count: 0,
            fixed_playback_layer_scales: None,
            preserve_complete_presentation: false,
            navigation_ladder_baseline: None,
            baselines,
        }
    }

    pub(crate) fn with_playback(
        mut self,
        active: bool,
        timepoints: Vec<TimeIndex>,
        required_timepoint_count: usize,
        fixed_layer_scales: Option<Arc<BTreeMap<LogicalLayerKey, ScaleLevel>>>,
    ) -> Self {
        self.playback_active = active;
        self.playback_required_timepoint_count = required_timepoint_count.min(timepoints.len());
        self.playback_timepoints = timepoints.into_boxed_slice();
        self.fixed_playback_layer_scales = fixed_layer_scales;
        self
    }

    pub(crate) fn with_complete_presentation_preserved(mut self, preserve: bool) -> Self {
        self.preserve_complete_presentation = preserve;
        self
    }

    pub(crate) fn with_navigation_ladder_baseline(
        mut self,
        baseline: Option<NavigationLadderBaseline>,
    ) -> Self {
        self.navigation_ladder_baseline = baseline;
        self
    }
}

pub(crate) struct CrossSectionDemandRequest {
    panel: PanelId,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    limits: DatasetDemandPlanLimits,
    baseline: ScopeDemandBaseline,
}

impl CrossSectionDemandRequest {
    pub(crate) fn new(
        panel: PanelId,
        presentation: PresentationViewport,
        viewport: RenderExtent,
        limits: DatasetDemandPlanLimits,
        baseline: ScopeDemandBaseline,
    ) -> Self {
        Self {
            panel,
            presentation,
            viewport,
            limits,
            baseline,
        }
    }
}

impl CameraDemandRequest {
    #[allow(
        clippy::too_many_arguments,
        reason = "the worker request keeps each independently replaceable demand/union cohort explicit"
    )]
    pub(crate) fn new(
        revision: RenderIntentRevision,
        catalog: Arc<DatasetCatalog>,
        cpu_ledger: Arc<dyn CpuByteLedger>,
        view: ViewState,
        global_limits: DatasetDemandPlanLimits,
        current_3d: Option<Current3dDemandRequest>,
        cross_sections: Vec<CrossSectionDemandRequest>,
        previous_renderer_requirement_union: RetainedRequirementHandle,
        unchanged_renderer_requirement_bodies: Vec<PreparedResourceBody>,
        post_refinement_promotion_unchanged_bodies: Option<Vec<PreparedResourceBody>>,
    ) -> Self {
        Self {
            revision,
            catalog,
            cpu_ledger,
            view,
            global_limits,
            current_3d,
            cross_sections: cross_sections.into_boxed_slice(),
            previous_renderer_requirement_union,
            preserve_previous_renderer_requirement_union: false,
            unchanged_renderer_requirement_bodies: unchanged_renderer_requirement_bodies
                .into_boxed_slice(),
            post_refinement_promotion_unchanged_bodies: post_refinement_promotion_unchanged_bodies
                .map(Vec::into_boxed_slice),
            fixed_playback_layer_scales: None,
            temporal_frame_contract: None,
        }
    }

    pub(crate) const fn revision(&self) -> RenderIntentRevision {
        self.revision
    }

    pub(crate) fn with_previous_renderer_requirement_union_preserved(
        mut self,
        preserve: bool,
    ) -> Self {
        self.preserve_previous_renderer_requirement_union = preserve;
        self
    }

    pub(crate) fn with_temporal_frame_contract(
        mut self,
        contract: Option<PlaybackFrameContract>,
    ) -> Self {
        self.temporal_frame_contract = contract;
        self
    }

    pub(crate) fn with_fixed_playback_layer_scales(
        mut self,
        layer_scales: Option<Arc<BTreeMap<LogicalLayerKey, ScaleLevel>>>,
    ) -> Self {
        self.fixed_playback_layer_scales = layer_scales;
        self
    }
}

/// One canonical union and the exact ledger authority for only that union's
/// immutable key array. Keeping the pair indivisible prevents an Arc from
/// escaping its accounting lifetime.
#[derive(Clone)]
pub(crate) struct PreparedRequirementUnion {
    pub(crate) requirements: Arc<[BrickKey]>,
    pub(crate) charge: Arc<dyn CpuByteLease>,
}

impl fmt::Debug for PreparedRequirementUnion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedRequirementUnion")
            .field("requirements", &self.requirements.len())
            .field("charged_bytes", &self.charge.reserved_bytes())
            .finish()
    }
}

/// Worker-prepared retained-union replacement. `previous` is the exact Arc
/// identity observed when the request was submitted; the UI may apply the
/// removal delta only while that identity is still installed.
#[derive(Clone)]
pub(crate) struct PreparedRendererRequirementUpdate {
    pub(crate) previous: RetainedRequirementHandle,
    pub(crate) next: PreparedRequirementUnion,
    pub(crate) removals: Arc<[BrickKey]>,
    /// Transient ownership for the removal array; it is released immediately
    /// after the UI's O(delta) commit.
    pub(crate) removal_charge: Arc<dyn CpuByteLease>,
}

impl fmt::Debug for PreparedRendererRequirementUpdate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedRendererRequirementUpdate")
            .field("previous", &self.previous.requirements.len())
            .field(
                "previous_charge_bytes",
                &self
                    .previous
                    .charge
                    .as_ref()
                    .map(|charge| charge.reserved_bytes()),
            )
            .field("next", &self.next)
            .field("removals", &self.removals.len())
            .field(
                "removal_charge_bytes",
                &self.removal_charge.reserved_bytes(),
            )
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedScopeDemand {
    pub(crate) requirements: PreparedDemandRequirements,
    pub(crate) render_requirements: Option<PreparedRenderRequirements>,
    pub(crate) render_payload_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedNavigationCandidateDemand {
    pub(crate) render_requirements: PreparedRenderRequirements,
    pub(crate) selection_body: PreparedResourceBody,
    pub(crate) layer_scales: BTreeMap<LogicalLayerKey, ScaleLevel>,
    pub(crate) planned_payload_bytes: u64,
    pub(crate) resource_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedNavigationLadderDemand {
    pub(crate) candidates: Vec<PreparedNavigationCandidateDemand>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedCurrent3dDemand {
    pub(crate) plan: PreparedProgressiveDatasetDemandPlan,
    pub(crate) current: PreparedScopeDemand,
    pub(crate) refinement: PreparedScopeDemand,
    pub(crate) playback: PreparedScopeDemand,
    /// Number of ranked resources forming one complete future timepoint in
    /// `playback`. Warmup may require several such bodies, while the running
    /// clock gates only on the immediate successor body.
    pub(crate) playback_resources_per_timepoint: usize,
    pub(crate) navigation_ladder: PreparedNavigationLadderDemand,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedCrossSectionDemand {
    pub(crate) panel: PanelId,
    pub(crate) plan: PreparedDatasetDemandPlan,
    pub(crate) render_requirements: Option<PreparedRenderRequirements>,
    pub(crate) render_payload_bytes: Option<u64>,
    pub(crate) reuse_envelope: Option<SemanticPlaneReuseEnvelope>,
}

#[derive(Debug)]
/// Physical worker output for an explicitly requested temporal transaction.
///
/// This is deliberately a delta, not the logical frame. Omitted targets must
/// be assembled from compatible installed bodies at the composition root
/// before anything is committed or submitted to the renderer.
pub(crate) struct PreparedTemporalDelta {
    pub(crate) contract: PlaybackFrameContract,
    pub(crate) current_3d: Option<PreparedCurrent3dDemand>,
    pub(crate) cross_sections: Box<[PreparedCrossSectionDemand]>,
}

#[derive(Debug)]
pub(crate) enum PreparedVisibleTargets {
    Ordinary {
        current_3d: Option<PreparedCurrent3dDemand>,
        cross_sections: Box<[PreparedCrossSectionDemand]>,
    },
    Temporal(PreparedTemporalDelta),
}

impl PreparedVisibleTargets {
    pub(crate) fn current_3d(&self) -> Option<&PreparedCurrent3dDemand> {
        match self {
            Self::Ordinary { current_3d, .. } => current_3d.as_ref(),
            Self::Temporal(delta) => delta.current_3d.as_ref(),
        }
    }

    #[cfg(test)]
    pub(crate) fn cross_sections(&self) -> &[PreparedCrossSectionDemand] {
        match self {
            Self::Ordinary { cross_sections, .. } => cross_sections,
            Self::Temporal(delta) => &delta.cross_sections,
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        Option<PreparedCurrent3dDemand>,
        Box<[PreparedCrossSectionDemand]>,
        Option<PlaybackFrameContract>,
    ) {
        match self {
            Self::Ordinary {
                current_3d,
                cross_sections,
            } => (current_3d, cross_sections, None),
            Self::Temporal(delta) => (delta.current_3d, delta.cross_sections, Some(delta.contract)),
        }
    }
}

pub(crate) struct PreparedVisibleDemand {
    pub(crate) targets: PreparedVisibleTargets,
    pub(crate) renderer_requirement_update: PreparedRendererRequirementUpdate,
    /// Exact GPU payload bytes represented by
    /// `renderer_requirement_update.next`. This is prepared on the worker
    /// beside the canonical union so the UI can tighten adaptive selection
    /// after a physical-placement refusal without rescanning the catalog.
    pub(crate) renderer_requirement_payload_bytes: u64,
    pub(crate) post_refinement_promotion_update: Option<PreparedRendererRequirementUpdate>,
    pub(crate) candidates_visited: usize,
}

impl fmt::Debug for PreparedVisibleDemand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedVisibleDemand")
            .field("targets", &self.targets)
            .field(
                "renderer_requirement_union_len",
                &self.renderer_requirement_update.next.requirements.len(),
            )
            .field(
                "renderer_requirement_payload_bytes",
                &self.renderer_requirement_payload_bytes,
            )
            .field(
                "post_refinement_promotion_union_len",
                &self
                    .post_refinement_promotion_update
                    .as_ref()
                    .map(|update| update.next.requirements.len()),
            )
            .field("candidates_visited", &self.candidates_visited)
            .field(
                "renderer_requirement_update",
                &self.renderer_requirement_update,
            )
            .finish()
    }
}

impl PreparedVisibleDemand {
    pub(crate) fn current_3d(&self) -> Option<&PreparedCurrent3dDemand> {
        self.targets.current_3d()
    }

    #[cfg(test)]
    pub(crate) fn cross_sections(&self) -> &[PreparedCrossSectionDemand] {
        self.targets.cross_sections()
    }
}

#[derive(Debug)]
pub(crate) struct CameraDemandResult {
    pub(crate) revision: RenderIntentRevision,
    pub(crate) outcome: anyhow::Result<PreparedVisibleDemand>,
    pub(crate) planning_duration: Duration,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CameraDemandDiagnostics {
    pub(crate) submitted: u64,
    pub(crate) pending_replacements: u64,
    pub(crate) cancelled_running: u64,
    pub(crate) stale_results_suppressed: u64,
    pub(crate) completed: u64,
    pub(crate) completed_candidates_visited: u64,
    pub(crate) contained_reuses: u64,
    pub(crate) candidates_reused: u64,
    pub(crate) completed_guarded_rebuilds: u64,
    pub(crate) completed_exact_rebuilds: u64,
    pub(crate) last_primary_resources: u64,
    pub(crate) last_guard_resources: u64,
    pub(crate) completed_planning_time_ns: u64,
    pub(crate) cancelled_planning_time_ns: u64,
    pub(crate) stale_planning_time_ns: u64,
    pub(crate) last_completed_planning_time_ns: u64,
    /// Submission and polling never visit semantic candidates on the caller.
    pub(crate) ui_thread_candidates_visited: u64,
    pub(crate) maximum_pending_requests: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CameraDemandPlannerError {
    WorkerSpawnFailed(io::ErrorKind),
}

impl fmt::Display for CameraDemandPlannerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkerSpawnFailed(kind) => {
                write!(formatter, "failed to spawn camera-demand worker: {kind:?}")
            }
        }
    }
}

impl std::error::Error for CameraDemandPlannerError {}

struct QueuedRequest {
    submitted_at: Instant,
    request: CameraDemandRequest,
}

#[derive(Default)]
struct WorkerState {
    shutdown: bool,
    pending: Option<QueuedRequest>,
    result: Option<CameraDemandResult>,
    running_revision: Option<RenderIntentRevision>,
}

#[derive(Default)]
struct Counters {
    submitted: AtomicU64,
    pending_replacements: AtomicU64,
    cancelled_running: AtomicU64,
    stale_results_suppressed: AtomicU64,
    completed: AtomicU64,
    completed_candidates_visited: AtomicU64,
    contained_reuses: AtomicU64,
    candidates_reused: AtomicU64,
    completed_guarded_rebuilds: AtomicU64,
    completed_exact_rebuilds: AtomicU64,
    last_primary_resources: AtomicU64,
    last_guard_resources: AtomicU64,
    completed_planning_time_ns: AtomicU64,
    cancelled_planning_time_ns: AtomicU64,
    stale_planning_time_ns: AtomicU64,
    last_completed_planning_time_ns: AtomicU64,
}

struct Shared {
    state: Mutex<WorkerState>,
    wake: Condvar,
    shutdown: AtomicBool,
    latest_revision: AtomicU64,
    running_invalidated: AtomicBool,
    counters: Counters,
    #[cfg(test)]
    before_result_publication: Mutex<Option<TestPublicationBarrier>>,
}

#[cfg(test)]
struct TestPublicationBarrier {
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

pub(crate) struct CameraDemandPlanner {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl CameraDemandPlanner {
    pub(crate) fn new() -> Result<Self, CameraDemandPlannerError> {
        let shared = Arc::new(Shared {
            state: Mutex::new(WorkerState::default()),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
            latest_revision: AtomicU64::new(RenderIntentRevision::initial().get()),
            running_invalidated: AtomicBool::new(false),
            counters: Counters::default(),
            #[cfg(test)]
            before_result_publication: Mutex::new(None),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name(WORKER_NAME.to_owned())
            .spawn(move || run_worker(worker_shared))
            .map_err(|error| CameraDemandPlannerError::WorkerSpawnFailed(error.kind()))?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    /// Replaces any not-yet-started request and cancels a running older
    /// revision. The caller performs only one bounded slot replacement.
    pub(crate) fn submit(&mut self, request: CameraDemandRequest) -> bool {
        let revision = request.revision;
        if self
            .shared
            .latest_revision
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (revision.get() >= current).then_some(revision.get())
            })
            .is_err()
        {
            return false;
        }
        self.shared
            .counters
            .submitted
            .fetch_add(1, Ordering::Relaxed);

        let mut state = lock_state(&self.shared);
        if state.running_revision.is_some() {
            self.shared
                .running_invalidated
                .store(true, Ordering::Release);
        }
        if state
            .pending
            .replace(QueuedRequest {
                submitted_at: Instant::now(),
                request,
            })
            .is_some()
        {
            self.shared
                .counters
                .pending_replacements
                .fetch_add(1, Ordering::Relaxed);
        }
        if state.result.take().is_some() {
            self.shared
                .counters
                .stale_results_suppressed
                .fetch_add(1, Ordering::Relaxed);
        }
        drop(state);
        self.shared.wake.notify_one();
        true
    }

    /// Takes the sole result only when it still belongs to the latest request.
    pub(crate) fn take_result(&mut self) -> Option<CameraDemandResult> {
        let latest = self.shared.latest_revision.load(Ordering::Acquire);
        let mut state = lock_state(&self.shared);
        let result = state.result.take()?;
        if result.revision.get() == latest {
            Some(result)
        } else {
            self.shared
                .counters
                .stale_results_suppressed
                .fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    pub(crate) fn has_outstanding_request(&self) -> bool {
        let latest = self.shared.latest_revision.load(Ordering::Acquire);
        let state = lock_state(&self.shared);
        state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.request.revision.get() == latest)
            || state
                .result
                .as_ref()
                .is_some_and(|result| result.revision.get() == latest)
            || state
                .running_revision
                .is_some_and(|revision| revision.get() == latest)
    }

    /// Invalidates pending/running work during source replacement without
    /// manufacturing a revision. The revision is copied from the sole
    /// render-intent mailbox authority.
    pub(crate) fn invalidate(&mut self, revision: RenderIntentRevision) {
        let previous = self
            .shared
            .latest_revision
            .fetch_max(revision.get(), Ordering::AcqRel);
        debug_assert!(
            revision.get() >= previous,
            "camera-demand invalidation must observe the latest mailbox revision"
        );
        let mut state = lock_state(&self.shared);
        if state.running_revision.is_some() {
            self.shared
                .running_invalidated
                .store(true, Ordering::Release);
        }
        state.pending = None;
        state.result = None;
    }

    pub(crate) fn diagnostics(&self) -> CameraDemandDiagnostics {
        CameraDemandDiagnostics {
            submitted: self.shared.counters.submitted.load(Ordering::Relaxed),
            pending_replacements: self
                .shared
                .counters
                .pending_replacements
                .load(Ordering::Relaxed),
            cancelled_running: self
                .shared
                .counters
                .cancelled_running
                .load(Ordering::Relaxed),
            stale_results_suppressed: self
                .shared
                .counters
                .stale_results_suppressed
                .load(Ordering::Relaxed),
            completed: self.shared.counters.completed.load(Ordering::Relaxed),
            completed_candidates_visited: self
                .shared
                .counters
                .completed_candidates_visited
                .load(Ordering::Relaxed),
            contained_reuses: self
                .shared
                .counters
                .contained_reuses
                .load(Ordering::Relaxed),
            candidates_reused: self
                .shared
                .counters
                .candidates_reused
                .load(Ordering::Relaxed),
            completed_guarded_rebuilds: self
                .shared
                .counters
                .completed_guarded_rebuilds
                .load(Ordering::Relaxed),
            completed_exact_rebuilds: self
                .shared
                .counters
                .completed_exact_rebuilds
                .load(Ordering::Relaxed),
            last_primary_resources: self
                .shared
                .counters
                .last_primary_resources
                .load(Ordering::Relaxed),
            last_guard_resources: self
                .shared
                .counters
                .last_guard_resources
                .load(Ordering::Relaxed),
            completed_planning_time_ns: self
                .shared
                .counters
                .completed_planning_time_ns
                .load(Ordering::Relaxed),
            cancelled_planning_time_ns: self
                .shared
                .counters
                .cancelled_planning_time_ns
                .load(Ordering::Relaxed),
            stale_planning_time_ns: self
                .shared
                .counters
                .stale_planning_time_ns
                .load(Ordering::Relaxed),
            last_completed_planning_time_ns: self
                .shared
                .counters
                .last_completed_planning_time_ns
                .load(Ordering::Relaxed),
            ui_thread_candidates_visited: 0,
            maximum_pending_requests: 1,
        }
    }

    /// Records an O(1) installed-artifact reuse. No candidate is touched on
    /// the caller; the count is the prior traversal volume whose immutable
    /// classification and ranking remained authoritative.
    pub(crate) fn record_contained_reuse(&self, reusable_candidates: usize) {
        self.shared
            .counters
            .contained_reuses
            .fetch_add(1, Ordering::Relaxed);
        self.shared.counters.candidates_reused.fetch_add(
            u64::try_from(reusable_candidates).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    #[cfg(test)]
    pub(crate) fn block_next_result_publication(
        &self,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *self
            .shared
            .before_result_publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(TestPublicationBarrier {
            entered: entered_tx,
            release: release_rx,
        });
        (entered_rx, release_tx)
    }
}

impl Drop for CameraDemandPlanner {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        {
            let mut state = lock_state(&self.shared);
            state.shutdown = true;
            state.pending = None;
            state.result = None;
            self.shared
                .running_invalidated
                .store(true, Ordering::Release);
        }
        self.shared.wake.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_worker(shared: Arc<Shared>) {
    let mut coalesce_replacement = false;
    loop {
        let queued = {
            let mut state = lock_state(&shared);
            loop {
                if state.shutdown {
                    return;
                }
                let Some(pending) = state.pending.as_ref() else {
                    state = shared
                        .wake
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    continue;
                };
                if coalesce_replacement {
                    let remaining =
                        REPLACEMENT_QUIET_PERIOD.saturating_sub(pending.submitted_at.elapsed());
                    if !remaining.is_zero() {
                        let (next_state, _) = shared
                            .wake
                            .wait_timeout(state, remaining)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        state = next_state;
                        continue;
                    }
                }
                let queued = state
                    .pending
                    .take()
                    .expect("a checked pending request remains present");
                state.running_revision = Some(queued.request.revision);
                shared.running_invalidated.store(false, Ordering::Release);
                break queued;
            }
        };

        let revision = queued.request.revision;
        let started = Instant::now();
        let planning = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            plan_visible_demand(queued.request, || {
                shared.shutdown.load(Ordering::Acquire)
                    || shared.running_invalidated.load(Ordering::Acquire)
                    || shared.latest_revision.load(Ordering::Acquire) != revision.get()
            })
        }));
        let outcome = match planning {
            Ok(Ok(Some(planning))) => Ok(planning),
            Ok(Ok(None)) => {
                clear_running_revision(&shared, revision);
                shared
                    .counters
                    .cancelled_running
                    .fetch_add(1, Ordering::Relaxed);
                saturating_add_duration(
                    &shared.counters.cancelled_planning_time_ns,
                    started.elapsed(),
                );
                coalesce_replacement = true;
                continue;
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err(anyhow::anyhow!("camera-demand worker panicked")),
        };
        if shared.shutdown.load(Ordering::Acquire) {
            clear_running_revision(&shared, revision);
            return;
        }
        if shared.running_invalidated.load(Ordering::Acquire)
            || shared.latest_revision.load(Ordering::Acquire) != revision.get()
        {
            clear_running_revision(&shared, revision);
            shared
                .counters
                .stale_results_suppressed
                .fetch_add(1, Ordering::Relaxed);
            saturating_add_duration(&shared.counters.stale_planning_time_ns, started.elapsed());
            coalesce_replacement = true;
            continue;
        }

        let completed_candidates = outcome
            .as_ref()
            .map_or(0, |planning| planning.candidates_visited as u64);
        let completed_3d = outcome.as_ref().ok().and_then(|prepared| {
            prepared.current_3d().map(|current| {
                let plans = std::iter::once(&current.plan.target).chain(current.plan.coarse.iter());
                let (primary, total) = plans.fold((0_usize, 0_usize), |(primary, total), plan| {
                    (
                        primary.saturating_add(plan.primary_resource_count),
                        total.saturating_add(plan.requirements.canonical().len()),
                    )
                });
                (primary, total, current.plan.reuse_envelope.is_some())
            })
        });
        let planning_duration = started.elapsed();
        let result = CameraDemandResult {
            revision,
            outcome,
            planning_duration,
        };
        #[cfg(test)]
        if let Some(barrier) = shared
            .before_result_publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = barrier.entered.send(());
            let _ = barrier.release.recv();
        }
        let mut state = lock_state(&shared);
        if state.shutdown
            || shared.running_invalidated.load(Ordering::Acquire)
            || shared.latest_revision.load(Ordering::Acquire) != revision.get()
        {
            if state.running_revision == Some(revision) {
                state.running_revision = None;
            }
            shared
                .counters
                .stale_results_suppressed
                .fetch_add(1, Ordering::Relaxed);
            saturating_add_duration(&shared.counters.stale_planning_time_ns, started.elapsed());
            coalesce_replacement = true;
            continue;
        }
        state.result = Some(result);
        // Keep the revision visibly running until the result is installed
        // under the same lock observed by `has_outstanding_request`. Readers
        // therefore see either running work or a publishable result, never an
        // idle gap that can consume the final repaint wakeup.
        if state.running_revision == Some(revision) {
            state.running_revision = None;
        }
        shared.counters.completed.fetch_add(1, Ordering::Relaxed);
        shared
            .counters
            .completed_candidates_visited
            .fetch_add(completed_candidates, Ordering::Relaxed);
        if let Some((primary, total, guarded)) = completed_3d {
            shared.counters.last_primary_resources.store(
                u64::try_from(primary).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
            shared.counters.last_guard_resources.store(
                u64::try_from(total.saturating_sub(primary)).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
            let counter = if guarded {
                &shared.counters.completed_guarded_rebuilds
            } else {
                &shared.counters.completed_exact_rebuilds
            };
            counter.fetch_add(1, Ordering::Relaxed);
        }
        saturating_add_duration(
            &shared.counters.completed_planning_time_ns,
            planning_duration,
        );
        shared
            .counters
            .last_completed_planning_time_ns
            .store(duration_ns(planning_duration), Ordering::Relaxed);
        coalesce_replacement = false;
    }
}

fn plan_visible_demand(
    request: CameraDemandRequest,
    mut cancelled: impl FnMut() -> bool,
) -> anyhow::Result<Option<PreparedVisibleDemand>> {
    let CameraDemandRequest {
        revision: _,
        catalog,
        cpu_ledger,
        view,
        global_limits,
        current_3d,
        cross_sections,
        previous_renderer_requirement_union,
        preserve_previous_renderer_requirement_union,
        unchanged_renderer_requirement_bodies,
        post_refinement_promotion_unchanged_bodies,
        fixed_playback_layer_scales,
        temporal_frame_contract,
    } = request;
    let mut candidates_visited = 0_usize;
    let linked_navigation_floor = if cross_sections.is_empty() {
        None
    } else {
        let Some(floor) = plan_cross_section_navigation_floor_cancellable(
            catalog.as_ref(),
            &view,
            global_limits,
            Some(cpu_ledger.as_ref()),
            &mut cancelled,
        )?
        else {
            return Ok(None);
        };
        candidates_visited = candidates_visited.saturating_add(floor.candidates_visited);
        Some(floor)
    };
    let linked_navigation_floor_body = linked_navigation_floor.as_ref().map(|floor| {
        let mut canonical = floor.plan.resources.clone();
        canonical.sort_unstable();
        canonical.dedup();
        Arc::<[BrickKey]>::from(canonical)
    });
    let mut transition_obligation_bodies = unchanged_renderer_requirement_bodies
        .iter()
        .map(|body| Arc::clone(body.canonical()))
        .collect::<Vec<_>>();
    let preserved_previous_renderer_requirement_union =
        preserve_previous_renderer_requirement_union
            .then(|| Arc::clone(&previous_renderer_requirement_union.requirements));
    transition_obligation_bodies.extend(
        preserved_previous_renderer_requirement_union
            .iter()
            .cloned(),
    );
    if let Some(input) = current_3d.as_ref() {
        // The complete current front remains visible through replacement.
        // Refinement and playback scopes are latest-only work owned by this
        // same transaction; their predecessor bodies are cancelled/replaced
        // atomically and therefore are not permanent aggregate obligations.
        // Any genuinely in-flight GPU pins remain renderer-owned and are
        // checked by its transactional residency preflight.
        transition_obligation_bodies.push(Arc::clone(input.baselines.current.body.canonical()));
    }
    transition_obligation_bodies.extend(
        cross_sections
            .iter()
            .map(|input| Arc::clone(input.baseline.body.canonical())),
    );
    transition_obligation_bodies.extend(linked_navigation_floor_body.iter().cloned());
    let mut transition_obligation_reservations =
        PreparedAllocationReservations::new(cpu_ledger.as_ref());
    let Some(transition_obligations) = merge_requirement_union(
        &transition_obligation_bodies,
        &mut transition_obligation_reservations,
        &mut cancelled,
    )?
    else {
        return Ok(None);
    };
    let _transition_obligation_charge = transition_obligation_reservations.finish();
    let transition_payload_bytes =
        requirement_payload_bytes(catalog.as_ref(), &transition_obligations)?;
    if transition_obligations.len() > global_limits.max_resources
        || transition_payload_bytes > global_limits.max_gpu_payload_bytes
    {
        return Err(DatasetDemandPlanCapacityError::for_global_usage(
            global_limits,
            "visible transition",
            None,
            transition_obligations.len(),
            transition_payload_bytes,
        )
        .into());
    }
    let transition_current_front_body = current_3d
        .as_ref()
        .map(|input| Arc::clone(input.baselines.current.body.canonical()));
    let transition_cross_front_bodies = cross_sections
        .iter()
        .map(|input| Arc::clone(input.baseline.body.canonical()))
        .collect::<Vec<_>>();
    let current_3d = if let Some(input) = current_3d {
        let mut reservations = PreparedAllocationReservations::new(cpu_ledger.as_ref());
        let planning = plan_guarded_progressive_current_3d_with_obligations_cancellable(
            catalog.as_ref(),
            &view,
            input.presentation,
            input.viewport,
            input.limits,
            input.playback_active,
            &input.playback_timepoints,
            input.playback_required_timepoint_count,
            input.fixed_playback_layer_scales.as_deref(),
            transition_obligations.as_ref(),
            input.navigation_ladder_baseline.as_ref(),
            Some(cpu_ledger.as_ref()),
            &mut cancelled,
        )?;
        let Some(planning) = planning else {
            return Ok(None);
        };
        if cancelled() {
            return Ok(None);
        }
        let (mut plan, work) = PreparedProgressiveDatasetDemandPlan::from_planning_accounted(
            planning,
            &mut reservations,
        )?;
        candidates_visited = candidates_visited.saturating_add(work);
        let empty = PreparedDemandRequirements::empty_accounted(&mut reservations)?;
        let target_requirements = std::mem::replace(&mut plan.target.requirements, empty.clone());
        let (current_requirements, refinement_requirements) = if input
            .preserve_complete_presentation
        {
            let refinement = target_requirements.into_preserving_admitted_prefix_accounted(
                input.baselines.refinement.admitted_prefix(),
                &reservations,
            )?;
            plan.target.requirements = refinement.clone();
            (empty.clone(), refinement)
        } else if let Some(coarse) = plan.coarse.as_mut() {
            let coarse_requirements = std::mem::replace(&mut coarse.requirements, empty.clone());
            let current = coarse_requirements.into_preserving_admitted_prefix_accounted(
                input.baselines.current.admitted_prefix(),
                &reservations,
            )?;
            coarse.requirements = current.clone();
            let refinement = target_requirements.into_preserving_admitted_prefix_accounted(
                input.baselines.refinement.admitted_prefix(),
                &reservations,
            )?;
            plan.target.requirements = refinement.clone();
            (current, refinement)
        } else {
            let current = target_requirements.into_preserving_admitted_prefix_accounted(
                input.baselines.current.admitted_prefix(),
                &reservations,
            )?;
            plan.target.requirements = current.clone();
            (current, empty.clone())
        };
        let playback_timepoint_count = plan
            .playback_timepoint_count
            .min(input.playback_timepoints.len());
        let playback_timepoints = &input.playback_timepoints[..playback_timepoint_count];
        let playback_requirements = if input.playback_active && !playback_timepoints.is_empty() {
            let per_timepoint_resource_count = plan
                .target
                .playback_resource_count
                .min(plan.target.requirements.ranked().len());
            let playback_resource_count = per_timepoint_resource_count
                .checked_mul(playback_timepoints.len())
                .ok_or_else(|| anyhow::anyhow!("playback-window resource count overflow"))?;
            let required_prefix_len = per_timepoint_resource_count
                .checked_mul(
                    input
                        .playback_required_timepoint_count
                        .min(playback_timepoints.len()),
                )
                .ok_or_else(|| anyhow::anyhow!("playback-runway prefix overflow"))?;
            let temporary_charge =
                reservations.reserve_temporary(key_array_bytes(playback_resource_count)?)?;
            let source = &plan.target.requirements.ranked()[..per_timepoint_resource_count];
            let mut ranked = Vec::with_capacity(playback_resource_count);
            for &timepoint in playback_timepoints.iter() {
                ranked.extend(
                    source
                        .iter()
                        .copied()
                        .map(|key| rebind_timepoint(key, timepoint)),
                );
            }
            let prepared = PreparedDemandRequirements::from_ranked_accounted(
                ranked,
                required_prefix_len,
                &mut reservations,
            )?;
            drop(temporary_charge);
            prepared.into_preserving_admitted_prefix_accounted(
                input.baselines.playback.admitted_prefix(),
                &reservations,
            )?
        } else {
            empty
        };
        if cancelled() {
            return Ok(None);
        }
        let navigation_residency_requirements = plan
            .coarse
            .as_ref()
            .map(|coarse| coarse.requirements.clone())
            .unwrap_or_else(|| plan.target.requirements.clone());
        let navigation_candidates = std::mem::take(&mut plan.navigation_candidates);
        let mut prepared_navigation_candidates = Vec::with_capacity(navigation_candidates.len());
        let target_is_empty = plan.target.layer_scales.is_empty();
        if target_is_empty {
            if !plan.ideal_layer_scales.is_empty()
                || !plan.target.requirements.canonical().is_empty()
                || plan.target.primary_resource_count != 0
                || plan.target.playback_resource_count != 0
                || plan.coarse.as_ref().is_some_and(|coarse| {
                    !coarse.layer_scales.is_empty()
                        || !coarse.requirements.canonical().is_empty()
                        || coarse.primary_resource_count != 0
                })
                || navigation_candidates.iter().any(|candidate| {
                    !candidate.layer_scales.is_empty()
                        || !candidate.requirements.canonical().is_empty()
                        || candidate.primary_resource_count != 0
                })
            {
                anyhow::bail!("an empty visible-layer target owns non-empty 3D demand");
            }
        } else {
            for candidate in navigation_candidates {
                let render_requirements = prepare_navigation_render_requirements(
                    catalog.as_ref(),
                    &view,
                    &candidate.layer_scales,
                    &candidate.requirements,
                    &navigation_residency_requirements,
                    &mut reservations,
                )?
                .ok_or_else(|| anyhow::anyhow!("a full-volume navigation candidate is empty"))?;
                prepared_navigation_candidates.push(PreparedNavigationCandidateDemand {
                    render_requirements,
                    selection_body: candidate.requirements.body().clone(),
                    layer_scales: candidate.layer_scales,
                    planned_payload_bytes: candidate.payload_bytes,
                    resource_count: candidate.primary_resource_count,
                });
            }
        }
        if !target_is_empty && prepared_navigation_candidates.is_empty() {
            anyhow::bail!("the full-volume navigation ladder is empty");
        }
        let navigation_ladder = PreparedNavigationLadderDemand {
            candidates: prepared_navigation_candidates,
        };
        if cancelled() {
            return Ok(None);
        }
        let current_layer_scales = plan
            .coarse
            .as_ref()
            .map(|coarse| &coarse.layer_scales)
            .unwrap_or(&plan.target.layer_scales);
        let current_source_prefix_len = plan
            .coarse
            .as_ref()
            .map(|coarse| coarse.playback_resource_count)
            .unwrap_or(plan.target.playback_resource_count);
        let current_render_requirements = prepare_primary_render_requirements(
            catalog.as_ref(),
            &view,
            current_layer_scales,
            &current_requirements,
            current_source_prefix_len,
            &mut reservations,
        )?;
        if cancelled() {
            return Ok(None);
        }
        let refinement_render_requirements = prepare_primary_render_requirements(
            catalog.as_ref(),
            &view,
            &plan.target.layer_scales,
            &refinement_requirements,
            plan.target.playback_resource_count,
            &mut reservations,
        )?;
        let current_render_payload_bytes =
            prepared_render_payload_bytes(catalog.as_ref(), current_render_requirements.as_ref())?;
        let refinement_render_payload_bytes = prepared_render_payload_bytes(
            catalog.as_ref(),
            refinement_render_requirements.as_ref(),
        )?;
        if cancelled() {
            return Ok(None);
        }
        let prepared = PreparedCurrent3dDemand {
            current: PreparedScopeDemand {
                requirements: current_requirements,
                render_requirements: current_render_requirements,
                render_payload_bytes: current_render_payload_bytes,
            },
            refinement: PreparedScopeDemand {
                requirements: refinement_requirements,
                render_requirements: refinement_render_requirements,
                render_payload_bytes: refinement_render_payload_bytes,
            },
            playback: PreparedScopeDemand {
                requirements: playback_requirements,
                render_requirements: None,
                render_payload_bytes: None,
            },
            playback_resources_per_timepoint: if input.playback_active
                && !playback_timepoints.is_empty()
            {
                plan.target
                    .playback_resource_count
                    .min(plan.target.requirements.ranked().len())
            } else {
                0
            },
            navigation_ladder,
            plan,
        };
        let cohort_charge = reservations.finish();
        for body in [
            prepared.current.requirements.body(),
            prepared.refinement.requirements.body(),
            prepared.playback.requirements.body(),
        ] {
            body.attach_charge(Arc::clone(&cohort_charge))?;
        }
        for candidate in &prepared.navigation_ladder.candidates {
            candidate
                .render_requirements
                .body()
                .attach_charge(Arc::clone(&cohort_charge))?;
            candidate
                .selection_body
                .attach_charge(Arc::clone(&cohort_charge))?;
        }
        for render_requirements in [
            prepared.current.render_requirements.as_ref(),
            prepared.refinement.render_requirements.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            render_requirements
                .body()
                .attach_charge(Arc::clone(&cohort_charge))?;
        }
        Some(prepared)
    } else {
        None
    };

    let mut selection_obligation_bodies = vec![Arc::clone(&transition_obligations)];
    if let Some(current) = current_3d.as_ref() {
        selection_obligation_bodies.extend([
            Arc::clone(current.current.requirements.canonical()),
            Arc::clone(current.refinement.requirements.canonical()),
            Arc::clone(current.playback.requirements.canonical()),
        ]);
    }
    let mut selection_obligation_reservations =
        PreparedAllocationReservations::new(cpu_ledger.as_ref());
    let Some(mut selection_obligations) = merge_requirement_union(
        &selection_obligation_bodies,
        &mut selection_obligation_reservations,
        &mut cancelled,
    )?
    else {
        return Ok(None);
    };
    let mut _selection_obligation_charge = selection_obligation_reservations.finish();

    let playback_cross_section_scales = fixed_playback_layer_scales.as_deref().or_else(|| {
        current_3d.as_ref().and_then(|current| {
            (!current.playback.requirements.canonical().is_empty())
                .then_some(&current.plan.target.layer_scales)
        })
    });
    let mut prepared_cross_sections = Vec::with_capacity(cross_sections.len());
    for input in cross_sections {
        if cancelled() {
            return Ok(None);
        }
        let prepared = prepare_adaptive_cross_section_panel(
            catalog.as_ref(),
            &view,
            &input,
            CrossSectionPlanningObligations {
                navigation_floor: &linked_navigation_floor
                    .as_ref()
                    .expect("a requested cross-section owns one navigation floor")
                    .plan,
                obligated_resources: &selection_obligations,
                playback_layer_scales: playback_cross_section_scales,
            },
            cpu_ledger.as_ref(),
            &mut cancelled,
        )?;
        let Some((prepared, work)) = prepared else {
            return Ok(None);
        };
        candidates_visited = candidates_visited.saturating_add(work);
        prepared_cross_sections.push(prepared);
        selection_obligation_bodies.push(Arc::clone(
            prepared_cross_sections
                .last()
                .expect("the prepared panel was just appended")
                .plan
                .requirements
                .canonical(),
        ));
        let mut reservations = PreparedAllocationReservations::new(cpu_ledger.as_ref());
        let Some(next_obligations) = merge_requirement_union(
            &selection_obligation_bodies,
            &mut reservations,
            &mut cancelled,
        )?
        else {
            return Ok(None);
        };
        selection_obligations = next_obligations;
        _selection_obligation_charge = reservations.finish();
    }
    if cancelled() {
        return Ok(None);
    }

    let unchanged_renderer_requirement_bodies = unchanged_renderer_requirement_bodies.into_vec();
    let current_target_is_empty = current_3d
        .as_ref()
        .is_some_and(|current| current.plan.target.layer_scales.is_empty());
    let cross_bodies = prepared_cross_sections
        .iter()
        .map(|cross| Arc::clone(cross.plan.requirements.canonical()))
        .collect::<Vec<_>>();
    let promotion_bodies = post_refinement_promotion_unchanged_bodies.map_or_else(
        || {
            current_3d.as_ref().and_then(|current| {
                (!current.refinement.requirements.canonical().is_empty()).then(|| {
                    let mut bodies = unchanged_renderer_requirement_bodies.to_vec();
                    bodies.push(current.refinement.requirements.body().clone());
                    bodies.push(current.playback.requirements.body().clone());
                    bodies
                })
            })
        },
        |bodies| Some(bodies.into_vec()),
    );
    let post_refinement_promotion_union = if let Some(promotion_bodies) = promotion_bodies {
        let mut reservations = PreparedAllocationReservations::new(cpu_ledger.as_ref());
        let mut bodies = promotion_bodies
            .iter()
            .map(|body| Arc::clone(body.canonical()))
            .collect::<Vec<_>>();
        bodies.extend(cross_bodies.iter().cloned());
        let Some(union) = merge_requirement_union(&bodies, &mut reservations, &mut cancelled)?
        else {
            return Ok(None);
        };
        Some(PreparedRequirementUnion {
            requirements: union,
            charge: reservations.finish(),
        })
    } else {
        None
    };
    let mut union_bodies = unchanged_renderer_requirement_bodies
        .iter()
        .map(|body| Arc::clone(body.canonical()))
        .collect::<Vec<_>>();
    if !current_target_is_empty {
        union_bodies.extend(
            preserved_previous_renderer_requirement_union
                .iter()
                .cloned(),
        );
    }
    if let Some(current) = current_3d.as_ref() {
        union_bodies.push(Arc::clone(current.current.requirements.canonical()));
        union_bodies.push(Arc::clone(current.refinement.requirements.canonical()));
        union_bodies.push(Arc::clone(current.playback.requirements.canonical()));
    }
    if !current_target_is_empty && let Some(current_front) = transition_current_front_body {
        union_bodies.push(current_front);
    }
    debug_assert_eq!(
        transition_cross_front_bodies.len(),
        prepared_cross_sections.len()
    );
    union_bodies.extend(
        transition_cross_front_bodies
            .into_iter()
            .zip(prepared_cross_sections.iter())
            .filter_map(|(front, cross)| {
                (!cross.plan.requirements.canonical().is_empty()).then_some(front)
            }),
    );
    union_bodies.extend(cross_bodies);
    let mut union_reservations = PreparedAllocationReservations::new(cpu_ledger.as_ref());
    let Some(renderer_requirement_union) =
        merge_requirement_union(&union_bodies, &mut union_reservations, &mut cancelled)?
    else {
        return Ok(None);
    };
    let renderer_union_payload_bytes =
        requirement_payload_bytes(catalog.as_ref(), &renderer_requirement_union)?;
    if renderer_requirement_union.len() > global_limits.max_resources
        || renderer_union_payload_bytes > global_limits.max_gpu_payload_bytes
    {
        return Err(DatasetDemandPlanCapacityError::for_global_usage(
            global_limits,
            "visible aggregate",
            None,
            renderer_requirement_union.len(),
            renderer_union_payload_bytes,
        )
        .into());
    }
    let renderer_union_charge = union_reservations.finish();
    let mut removal_reservations = PreparedAllocationReservations::new(cpu_ledger.as_ref());
    let Some(removals) = prepare_requirement_removals(
        &previous_renderer_requirement_union.requirements,
        &renderer_requirement_union,
        &mut removal_reservations,
        &mut cancelled,
    )?
    else {
        return Ok(None);
    };
    let removal_charge = removal_reservations.finish();
    let post_refinement_promotion_update = if let Some(promotion) = post_refinement_promotion_union
    {
        let mut promotion_removal_reservations =
            PreparedAllocationReservations::new(cpu_ledger.as_ref());
        let Some(removals) = prepare_requirement_removals(
            &renderer_requirement_union,
            &promotion.requirements,
            &mut promotion_removal_reservations,
            &mut cancelled,
        )?
        else {
            return Ok(None);
        };
        Some(PreparedRendererRequirementUpdate {
            previous: RetainedRequirementHandle {
                requirements: Arc::clone(&renderer_requirement_union),
                charge: Some(Arc::clone(&renderer_union_charge)),
            },
            next: promotion,
            removals,
            removal_charge: promotion_removal_reservations.finish(),
        })
    } else {
        None
    };
    if cancelled() {
        return Ok(None);
    }
    let cross_sections = prepared_cross_sections.into_boxed_slice();
    let targets = if let Some(contract) = temporal_frame_contract {
        if contract.timepoint() != view.timepoint() {
            anyhow::bail!("a prepared temporal frame changed timepoint while planning");
        }
        if current_3d.as_ref().is_some_and(|current| {
            current.plan.target.layer_scales != *contract.layer_scales().as_ref()
        }) {
            anyhow::bail!("a prepared temporal 3D delta escaped its fixed scale map");
        }
        match contract.target_set() {
            PlaybackTargetSet::ThreeD if !cross_sections.is_empty() => {
                anyhow::bail!("a standalone temporal delta contains linked targets");
            }
            PlaybackTargetSet::FullLayout
                if cross_sections
                    .iter()
                    .any(|cross| cross.plan.layer_scales != *contract.layer_scales().as_ref()) =>
            {
                anyhow::bail!("a prepared temporal linked delta escaped its fixed scale map");
            }
            PlaybackTargetSet::ThreeD | PlaybackTargetSet::FullLayout => {}
        }
        PreparedVisibleTargets::Temporal(PreparedTemporalDelta {
            contract,
            current_3d,
            cross_sections,
        })
    } else {
        PreparedVisibleTargets::Ordinary {
            current_3d,
            cross_sections,
        }
    };
    Ok(Some(PreparedVisibleDemand {
        targets,
        renderer_requirement_update: PreparedRendererRequirementUpdate {
            previous: previous_renderer_requirement_union,
            next: PreparedRequirementUnion {
                requirements: renderer_requirement_union,
                charge: renderer_union_charge,
            },
            removals,
            removal_charge,
        },
        renderer_requirement_payload_bytes: renderer_union_payload_bytes,
        post_refinement_promotion_update,
        candidates_visited,
    }))
}

fn requirement_payload_bytes(
    catalog: &DatasetCatalog,
    requirements: &[BrickKey],
) -> anyhow::Result<u64> {
    requirements.iter().try_fold(0_u64, |total, key| {
        let descriptor = catalog.resource_payload_descriptor(*key)?;
        let bytes = mirante4d_render_wgpu::payload_allocation_bytes(descriptor)?;
        total
            .checked_add(bytes)
            .ok_or_else(|| anyhow::anyhow!("global GPU payload accounting overflow"))
    })
}

struct CrossSectionPlanningObligations<'a> {
    navigation_floor: &'a DatasetDemandPlan,
    obligated_resources: &'a [BrickKey],
    playback_layer_scales: Option<&'a BTreeMap<LogicalLayerKey, ScaleLevel>>,
}

fn prepare_adaptive_cross_section_panel(
    catalog: &DatasetCatalog,
    view: &ViewState,
    input: &CrossSectionDemandRequest,
    obligations: CrossSectionPlanningObligations<'_>,
    cpu_ledger: &dyn CpuByteLedger,
    cancelled: &mut impl FnMut() -> bool,
) -> anyhow::Result<Option<(PreparedCrossSectionDemand, usize)>> {
    if let Some(playback_layer_scales) = obligations.playback_layer_scales {
        let planning = plan_cross_section_playback_body_cancellable(
            catalog,
            view,
            input.panel,
            playback_layer_scales,
            input.limits,
            obligations.obligated_resources,
            Some(cpu_ledger),
            cancelled,
        )?;
        let Some(planning) = planning else {
            return Ok(None);
        };
        return prepare_cross_section_planning(
            catalog,
            view,
            input,
            obligations.navigation_floor,
            planning,
            cpu_ledger,
            cancelled,
        );
    }
    let planning = plan_adaptive_cross_section_panel_with_obligations_cancellable(
        catalog,
        view,
        input.panel,
        input.presentation,
        input.viewport,
        input.limits,
        Some(cpu_ledger),
        obligations.playback_layer_scales,
        obligations.obligated_resources,
        true,
        cancelled,
    )?;
    let Some(planning) = planning else {
        return Ok(None);
    };
    let guarded = planning.plane_reuse_envelope.is_some();
    match prepare_cross_section_planning(
        catalog,
        view,
        input,
        obligations.navigation_floor,
        planning,
        cpu_ledger,
        cancelled,
    ) {
        Ok(prepared) => Ok(prepared),
        Err(error) if guarded && plane_guard_attempt_should_retry_exact(&error) => {
            let planning = plan_adaptive_cross_section_panel_with_obligations_cancellable(
                catalog,
                view,
                input.panel,
                input.presentation,
                input.viewport,
                input.limits,
                Some(cpu_ledger),
                obligations.playback_layer_scales,
                obligations.obligated_resources,
                false,
                cancelled,
            )?;
            let Some(planning) = planning else {
                return Ok(None);
            };
            prepare_cross_section_planning(
                catalog,
                view,
                input,
                obligations.navigation_floor,
                planning,
                cpu_ledger,
                cancelled,
            )
        }
        Err(error) => Err(error),
    }
}

fn prepare_cross_section_planning(
    catalog: &DatasetCatalog,
    view: &ViewState,
    input: &CrossSectionDemandRequest,
    navigation_floor: &DatasetDemandPlan,
    planning: crate::dataset_demand_plan::DatasetDemandPlanning,
    cpu_ledger: &dyn CpuByteLedger,
    cancelled: &mut impl FnMut() -> bool,
) -> anyhow::Result<Option<(PreparedCrossSectionDemand, usize)>> {
    let mut reservations = PreparedAllocationReservations::new(cpu_ledger);
    let (plan, work, reuse_envelope) = planning.prepare_accounted(&mut reservations)?;
    let PreparedDatasetDemandPlan {
        layer_scales,
        requirements,
        payload_bytes,
        playback_downshifted,
        covers_full_volume,
        primary_resource_count,
        playback_resource_count,
    } = plan;
    let requirements = requirements.into_preserving_admitted_prefix_accounted(
        input.baseline.admitted_prefix(),
        &reservations,
    )?;
    let mut plan = PreparedDatasetDemandPlan {
        layer_scales,
        requirements,
        payload_bytes,
        playback_downshifted,
        covers_full_volume,
        primary_resource_count,
        playback_resource_count,
    };
    // A plane with no selected target data is a terminal transparent result.
    // It needs neither fallback samples nor a synthetic nonempty render body;
    // preserving that explicit empty path also avoids calling coarse data
    // scientific coverage outside the dataset.
    if plan.primary_resource_count != 0 {
        merge_linked_navigation_floor(catalog, navigation_floor, &mut plan, &mut reservations)?;
    }
    let render_requirements = prepare_render_requirements(
        catalog,
        view,
        &plan.layer_scales,
        &plan.requirements,
        true,
        &mut reservations,
    )?;
    let render_payload_bytes =
        prepared_render_payload_bytes(catalog, render_requirements.as_ref())?;
    if cancelled() {
        return Ok(None);
    }
    let prepared = PreparedCrossSectionDemand {
        panel: input.panel,
        plan,
        render_requirements,
        render_payload_bytes,
        reuse_envelope,
    };
    let panel_charge = reservations.finish();
    prepared
        .plan
        .requirements
        .body()
        .attach_charge(panel_charge)?;
    Ok(Some((prepared, work)))
}

fn merge_linked_navigation_floor(
    catalog: &DatasetCatalog,
    navigation_floor: &DatasetDemandPlan,
    target: &mut PreparedDatasetDemandPlan,
    reservations: &mut PreparedAllocationReservations<'_>,
) -> anyhow::Result<()> {
    if navigation_floor.resources.is_empty() {
        anyhow::bail!("the linked navigation floor is empty");
    }
    let temporary_count = navigation_floor
        .resources
        .len()
        .checked_add(target.requirements.ranked().len())
        .ok_or_else(|| anyhow::anyhow!("linked navigation requirement count overflow"))?;
    let temporary_charge = reservations.reserve_temporary(key_array_bytes(temporary_count)?)?;
    let mut floor_membership = navigation_floor.resources.clone();
    floor_membership.sort_unstable();
    floor_membership.dedup();
    if floor_membership.len() != navigation_floor.resources.len() {
        anyhow::bail!("the linked navigation floor contains duplicate resources");
    }

    let target_required = target.requirements.required_prefix_len();
    let mut ranked = Vec::with_capacity(temporary_count);
    ranked.extend(navigation_floor.resources.iter().copied());
    let mut target_required_unique = 0_usize;
    for key in target.requirements.ranked()[..target_required]
        .iter()
        .copied()
    {
        if floor_membership.binary_search(&key).is_err() {
            ranked.push(key);
            target_required_unique = target_required_unique.saturating_add(1);
        }
    }
    for key in target.requirements.ranked()[target_required..]
        .iter()
        .copied()
    {
        if floor_membership.binary_search(&key).is_err() {
            ranked.push(key);
        }
    }
    let first_useful_prefix_len = navigation_floor.resources.len();
    let required_prefix_len = first_useful_prefix_len
        .checked_add(target_required_unique)
        .ok_or_else(|| anyhow::anyhow!("linked required-prefix count overflow"))?;
    let target_requirements = std::mem::replace(
        &mut target.requirements,
        PreparedDemandRequirements::empty(),
    );
    let merged = target_requirements.into_merged_ranked_with_prefixes_accounted(
        ranked,
        first_useful_prefix_len,
        required_prefix_len,
        reservations,
    )?;
    let payload_bytes = requirement_payload_bytes(catalog, merged.canonical())?;
    target.requirements = merged;
    target.payload_bytes = payload_bytes;
    target.primary_resource_count = required_prefix_len;
    drop(temporary_charge);
    Ok(())
}

fn prepare_render_requirements(
    catalog: &DatasetCatalog,
    view: &ViewState,
    layer_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
    requirements: &PreparedDemandRequirements,
    progressive_plane: bool,
    reservations: &mut PreparedAllocationReservations<'_>,
) -> anyhow::Result<Option<PreparedRenderRequirements>> {
    let Some(first) = requirements.canonical().first().copied() else {
        return Ok(None);
    };
    let layer_count = view.layers().iter().filter(|layer| layer.visible()).count();
    let mut scale_chains = Vec::with_capacity(layer_count);
    for view_layer in view.layers().iter().filter(|layer| layer.visible()) {
        let layer = view_layer.layer_key();
        let target = layer_scales.get(&layer).copied().ok_or_else(|| {
            anyhow::anyhow!(
                "visible layer {} has no prepared render target scale",
                layer.ordinal()
            )
        })?;
        let catalog_layer = catalog.layer(layer).ok_or_else(|| {
            anyhow::anyhow!(
                "visible layer {} disappeared from the catalog",
                layer.ordinal()
            )
        })?;
        let scales = if progressive_plane {
            catalog_layer
                .scales()
                .map(|scale| scale.level())
                .filter(|scale| *scale >= target)
                .collect::<Vec<_>>()
        } else {
            vec![target]
        };
        scale_chains.push(RenderLayerScaleChain::new(layer, scales)?);
    }
    let scale_count = scale_chains.iter().map(|chain| chain.scales().len()).sum();
    reservations.reserve_result(
        PreparedRenderRequirements::preflight_host_allocation_bytes_with_scale_count(
            layer_count,
            scale_count,
            requirements.canonical().len(),
        )?,
    )?;
    let layer_bytes = layer_count
        .checked_mul(std::mem::size_of::<LogicalLayerKey>())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| anyhow::anyhow!("prepared render-layer byte overflow"))?;
    let layer_scratch = reservations.reserve_temporary(layer_bytes)?;
    let layers = view
        .layers()
        .iter()
        .filter(|layer| layer.visible())
        .map(|layer| layer.layer_key())
        .collect::<Vec<LogicalLayerKey>>();
    let first_useful_prefix_len = if progressive_plane {
        requirements.first_useful_prefix_len()
    } else {
        requirements.required_prefix_len()
    };
    let render_requirements =
        PreparedRenderRequirements::new_with_required_prefix_and_scale_chains(
            catalog.resource_identity(),
            first.timepoint(),
            layers,
            scale_chains,
            requirements.body().clone(),
            first_useful_prefix_len,
            requirements.required_prefix_len(),
        )?;
    drop(layer_scratch);
    Ok(Some(render_requirements))
}

fn prepare_navigation_render_requirements(
    catalog: &DatasetCatalog,
    view: &ViewState,
    layer_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
    selection_requirements: &PreparedDemandRequirements,
    residency_requirements: &PreparedDemandRequirements,
    reservations: &mut PreparedAllocationReservations<'_>,
) -> anyhow::Result<Option<PreparedRenderRequirements>> {
    let Some(first) = residency_requirements.canonical().first().copied() else {
        return Ok(None);
    };
    if selection_requirements.canonical().is_empty() {
        return Ok(None);
    }
    if selection_requirements.canonical().iter().any(|key| {
        residency_requirements
            .canonical()
            .binary_search(key)
            .is_err()
    }) {
        anyhow::bail!(
            "a full-volume navigation candidate is not contained by its aggregate residency body"
        );
    }

    let layer_count = view.layers().iter().filter(|layer| layer.visible()).count();
    let mut scale_chains = Vec::with_capacity(layer_count);
    for view_layer in view.layers().iter().filter(|layer| layer.visible()) {
        let layer = view_layer.layer_key();
        let target = layer_scales.get(&layer).copied().ok_or_else(|| {
            anyhow::anyhow!(
                "visible layer {} has no navigation target scale",
                layer.ordinal()
            )
        })?;
        let _catalog_layer = catalog.layer(layer).ok_or_else(|| {
            anyhow::anyhow!(
                "visible layer {} disappeared from the catalog",
                layer.ordinal()
            )
        })?;
        // A volume frame remains one truthful uniform scale. Other complete
        // ladder rungs belong only to this wrapper's dormant residency suffix
        // and therefore must never enter the active render chain.
        scale_chains.push(RenderLayerScaleChain::new(layer, vec![target])?);
    }

    // Rank this candidate's exact coherent rung first, followed by every
    // other aggregate ladder resource in the planner's terminal-to-fine
    // order. The canonical residency set remains common to all wrappers,
    // while only this first prefix contributes to coverage and pixels.
    let aggregate_len = residency_requirements.canonical().len();
    reservations.reserve_result(PreparedResourceBody::preflight_host_allocation_bytes(
        aggregate_len,
        aggregate_len,
    )?)?;
    let ranked_scratch = reservations.reserve_temporary(key_array_bytes(aggregate_len)?)?;
    let mut ranked = Vec::with_capacity(aggregate_len);
    ranked.extend(selection_requirements.ranked().iter().copied());
    ranked.extend(
        residency_requirements
            .ranked()
            .iter()
            .copied()
            .filter(|key| {
                selection_requirements
                    .canonical()
                    .binary_search(key)
                    .is_err()
            }),
    );
    if ranked.len() != aggregate_len {
        anyhow::bail!("navigation residency wrapper lost or duplicated aggregate resources");
    }
    let residency_body = PreparedResourceBody::new(
        Arc::clone(residency_requirements.canonical()),
        ranked.into(),
        None,
    )?;
    drop(ranked_scratch);

    let scale_count = scale_chains.iter().map(|chain| chain.scales().len()).sum();
    reservations.reserve_result(
        PreparedRenderRequirements::preflight_host_allocation_bytes_with_scale_count(
            layer_count,
            scale_count,
            residency_body.canonical().len(),
        )?,
    )?;
    let layer_bytes = layer_count
        .checked_mul(std::mem::size_of::<LogicalLayerKey>())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| anyhow::anyhow!("prepared navigation-layer byte overflow"))?;
    let layer_scratch = reservations.reserve_temporary(layer_bytes)?;
    let layers = view
        .layers()
        .iter()
        .filter(|layer| layer.visible())
        .map(|layer| layer.layer_key())
        .collect::<Vec<_>>();
    let render_requirements =
        PreparedRenderRequirements::new_with_dormant_residency_suffix_and_scale_chains(
            catalog.resource_identity(),
            first.timepoint(),
            layers,
            scale_chains,
            residency_body,
            selection_requirements.ranked().len(),
            selection_requirements.ranked().len(),
        )?;
    drop(layer_scratch);
    Ok(Some(render_requirements))
}

fn prepare_primary_render_requirements(
    catalog: &DatasetCatalog,
    view: &ViewState,
    layer_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
    requirements: &PreparedDemandRequirements,
    source_prefix_len: usize,
    reservations: &mut PreparedAllocationReservations<'_>,
) -> anyhow::Result<Option<PreparedRenderRequirements>> {
    let required_prefix_len = requirements.required_prefix_len();
    if required_prefix_len == 0 {
        return Ok(None);
    }
    let source_prefix_len = source_prefix_len.min(requirements.ranked().len());
    if source_prefix_len < required_prefix_len {
        anyhow::bail!(
            "selected-scale render source prefix {source_prefix_len} is shorter than its required prefix {required_prefix_len}"
        );
    }
    if required_prefix_len == requirements.ranked().len()
        && source_prefix_len == requirements.ranked().len()
    {
        return prepare_render_requirements(
            catalog,
            view,
            layer_scales,
            requirements,
            false,
            reservations,
        );
    }
    let temporary_charge = reservations.reserve_temporary(key_array_bytes(source_prefix_len)?)?;
    let target = requirements
        .ranked()
        .iter()
        .take(source_prefix_len)
        .copied()
        .filter(|key| layer_scales.get(&key.layer()) == Some(&key.scale()))
        .collect::<Vec<_>>();
    if target.len() < required_prefix_len {
        anyhow::bail!(
            "selected-scale render body lost required resources while excluding its independent navigation ladder"
        );
    }
    let target = PreparedDemandRequirements::from_ranked_accounted(
        target,
        required_prefix_len,
        reservations,
    )?;
    drop(temporary_charge);
    prepare_render_requirements(catalog, view, layer_scales, &target, false, reservations)
}

fn prepared_render_payload_bytes(
    catalog: &DatasetCatalog,
    requirements: Option<&PreparedRenderRequirements>,
) -> anyhow::Result<Option<u64>> {
    requirements
        .map(|requirements| {
            requirements
                .body()
                .canonical()
                .iter()
                .try_fold(0_u64, |total, key| {
                    let descriptor = catalog.resource_payload_descriptor(*key)?;
                    total
                        .checked_add(mirante4d_render_wgpu::payload_allocation_bytes(descriptor)?)
                        .ok_or_else(|| {
                            anyhow::anyhow!("prepared render payload accounting overflow")
                        })
                })
        })
        .transpose()
}

pub(crate) fn merge_requirement_union(
    bodies: &[Arc<[BrickKey]>],
    reservations: &mut PreparedAllocationReservations<'_>,
    cancelled: &mut impl FnMut() -> bool,
) -> anyhow::Result<Option<Arc<[BrickKey]>>> {
    let cursor_bytes = bodies
        .len()
        .checked_mul(std::mem::size_of::<usize>())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| anyhow::anyhow!("requirement-union cursor byte overflow"))?;
    let cursor_charge = reservations.reserve_temporary(cursor_bytes)?;
    let mut cursors = vec![0_usize; bodies.len()];
    let mut union_len = 0_usize;
    while next_union_key(bodies, &mut cursors).is_some() {
        if union_len.is_multiple_of(256) && cancelled() {
            return Ok(None);
        }
        union_len = union_len.saturating_add(1);
        if union_len > mirante4d_render_api::MAX_RENDER_REQUIREMENTS {
            anyhow::bail!(
                "prepared renderer requirement union exceeds {} resources",
                mirante4d_render_api::MAX_RENDER_REQUIREMENTS
            );
        }
    }
    let key_bytes = key_array_bytes(union_len)?;
    reservations.reserve_result(key_bytes)?;
    let construction_charge = reservations.reserve_temporary(key_bytes)?;
    cursors.fill(0);
    let mut union = Vec::with_capacity(union_len);
    while let Some(key) = next_union_key(bodies, &mut cursors) {
        if union.len().is_multiple_of(256) && cancelled() {
            return Ok(None);
        }
        union.push(key);
    }
    let union = Arc::from(union);
    drop(construction_charge);
    drop(cursor_charge);
    Ok(Some(union))
}

fn next_union_key(bodies: &[Arc<[BrickKey]>], cursors: &mut [usize]) -> Option<BrickKey> {
    let next = bodies
        .iter()
        .zip(cursors.iter())
        .filter_map(|(body, cursor)| body.get(*cursor).copied())
        .min()?;
    for (body, cursor) in bodies.iter().zip(cursors) {
        if body.get(*cursor).copied() == Some(next) {
            *cursor += 1;
        }
    }
    Some(next)
}

pub(crate) fn prepare_requirement_removals(
    previous: &[BrickKey],
    next: &[BrickKey],
    reservations: &mut PreparedAllocationReservations<'_>,
    cancelled: &mut impl FnMut() -> bool,
) -> anyhow::Result<Option<Arc<[BrickKey]>>> {
    let mut previous_index = 0_usize;
    let mut next_index = 0_usize;
    let mut removal_len = 0_usize;
    let mut visits = 0_usize;
    while previous_index < previous.len() && next_index < next.len() {
        if visits.is_multiple_of(256) && cancelled() {
            return Ok(None);
        }
        match previous[previous_index].cmp(&next[next_index]) {
            std::cmp::Ordering::Less => {
                removal_len += 1;
                previous_index += 1;
            }
            std::cmp::Ordering::Greater => next_index += 1,
            std::cmp::Ordering::Equal => {
                previous_index += 1;
                next_index += 1;
            }
        }
        visits += 1;
    }
    removal_len = removal_len
        .checked_add(previous.len().saturating_sub(previous_index))
        .ok_or_else(|| anyhow::anyhow!("requirement-removal count overflow"))?;
    let removal_bytes = key_array_bytes(removal_len)?;
    reservations.reserve_result(removal_bytes)?;
    let construction_charge = reservations.reserve_temporary(removal_bytes)?;

    previous_index = 0;
    next_index = 0;
    let mut removals = Vec::with_capacity(removal_len);
    while previous_index < previous.len() && next_index < next.len() {
        if removals.len().is_multiple_of(256) && cancelled() {
            return Ok(None);
        }
        match previous[previous_index].cmp(&next[next_index]) {
            std::cmp::Ordering::Less => {
                removals.push(previous[previous_index]);
                previous_index += 1;
            }
            std::cmp::Ordering::Greater => next_index += 1,
            std::cmp::Ordering::Equal => {
                previous_index += 1;
                next_index += 1;
            }
        }
    }
    removals.extend_from_slice(&previous[previous_index..]);
    debug_assert_eq!(removals.len(), removal_len);
    let removals = Arc::from(removals);
    drop(construction_charge);
    Ok(Some(removals))
}

fn rebind_timepoint(key: BrickKey, timepoint: TimeIndex) -> BrickKey {
    BrickKey::new(
        key.identity(),
        key.layer(),
        timepoint,
        key.scale(),
        key.region(),
    )
}

fn saturating_add_duration(counter: &AtomicU64, duration: Duration) {
    let amount = duration_ns(duration);
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(amount))
    });
}

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn lock_state(shared: &Shared) -> std::sync::MutexGuard<'_, WorkerState> {
    shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn clear_running_revision(shared: &Shared, revision: RenderIntentRevision) {
    let mut state = lock_state(shared);
    if state.running_revision == Some(revision) {
        state.running_revision = None;
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    use glam::{DMat4, DQuat, DVec3};
    use mirante4d_application::{
        PlaybackFps, RenderIntentFamily, RenderIntentMailbox, SourceSessionGeneration,
    };
    use mirante4d_dataset::{
        ContentAddressStatus, CpuByteLease, CpuLedgerCategory, CpuLedgerError, DatasetLayer,
        DatasetResourceIdentity, DatasetSourceId, ResourceRegion, ResourceValidity,
    };
    use mirante4d_domain::{
        CameraView, CrossSectionView, DisplayWindow, GridToWorld, IntensityDType, IsoLightState,
        LayerTransfer, LogicalLayerKey, Opacity, Projection, RenderState, RgbColor, SamplingPolicy,
        Shape3D, Shape4D, TimeIndex, TransferCurve, UnitQuaternion, ViewerLayout, WorldPoint3,
    };
    use mirante4d_project_model::LayerViewState;

    use super::*;

    struct TestCpuLease {
        category: CpuLedgerCategory,
        bytes: u64,
    }

    impl CpuByteLease for TestCpuLease {
        fn category(&self) -> CpuLedgerCategory {
            self.category
        }

        fn reserved_bytes(&self) -> u64 {
            self.bytes
        }
    }

    struct TestCpuLedger;

    impl CpuByteLedger for TestCpuLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            if bytes == 0 {
                return Err(CpuLedgerError::ZeroByteReservation);
            }
            Ok(Box::new(TestCpuLease { category, bytes }))
        }
    }

    #[test]
    fn temporal_planning_returns_one_fixed_scale_target_bundle() {
        let (catalog, view) = temporal_fixture(
            Shape3D::new(64, 64, 64).unwrap(),
            3,
            TimeIndex::new(1),
            ViewerLayout::Single3d,
        );
        let scales = BTreeMap::from([(LogicalLayerKey::new(0), ScaleLevel::BASE)]);
        let mut session = crate::playback_session::PlaybackSession::new();
        let source = SourceSessionGeneration::new(7);
        let fps = PlaybackFps::new(24).unwrap();
        session.begin_warmup(source, fps, ViewerLayout::Single3d);
        assert!(session.admit_contract(
            source,
            fps,
            ViewerLayout::Single3d,
            scales.clone(),
            2,
            1,
            1 << 20,
            1 << 20,
            TimeIndex::new(0),
            &[TimeIndex::new(1), TimeIndex::new(2)],
        ));
        let frame = session
            .contract()
            .unwrap()
            .frame_contract(TimeIndex::new(1));
        let limits = DatasetDemandPlanLimits::new(4_096, 65_536, u64::MAX);
        let request = CameraDemandRequest::new(
            RenderIntentRevision::initial(),
            Arc::clone(&catalog),
            test_cpu_ledger(),
            view,
            limits,
            Some(
                Current3dDemandRequest::new(
                    PresentationViewport::new(64.0, 64.0).unwrap(),
                    RenderExtent::new(64, 64).unwrap(),
                    limits,
                    Current3dDemandBaselines::empty(),
                )
                .with_playback(
                    true,
                    vec![TimeIndex::new(2)],
                    1,
                    Some(Arc::new(scales.clone())),
                ),
            ),
            Vec::new(),
            unaccounted_requirement_handle(Arc::from([])),
            Vec::new(),
            None,
        )
        .with_temporal_frame_contract(Some(frame.clone()));

        let prepared = plan_visible_demand(request, || false).unwrap().unwrap();
        let PreparedVisibleTargets::Temporal(bundle) = prepared.targets else {
            panic!("temporal planning returned an ordinary split target set");
        };
        assert_eq!(bundle.contract, frame);
        assert_eq!(bundle.current_3d.unwrap().plan.target.layer_scales, scales);
        assert!(bundle.cross_sections.is_empty());
    }

    #[test]
    fn temporal_planning_accepts_a_partial_four_panel_physical_delta() {
        let (catalog, view) = temporal_fixture(
            Shape3D::new(64, 64, 64).unwrap(),
            3,
            TimeIndex::new(1),
            ViewerLayout::FourPanel,
        );
        let scales = BTreeMap::from([(LogicalLayerKey::new(0), ScaleLevel::BASE)]);
        let source = SourceSessionGeneration::new(7);
        let fps = PlaybackFps::new(24).unwrap();
        let mut session = crate::playback_session::PlaybackSession::new();
        session.begin_warmup(source, fps, ViewerLayout::FourPanel);
        assert!(session.admit_contract(
            source,
            fps,
            ViewerLayout::FourPanel,
            scales.clone(),
            2,
            1,
            1 << 20,
            1 << 20,
            TimeIndex::new(0),
            &[TimeIndex::new(1), TimeIndex::new(2)],
        ));
        let limits = DatasetDemandPlanLimits::new(4_096, 65_536, u64::MAX);
        let request = CameraDemandRequest::new(
            RenderIntentRevision::initial(),
            catalog,
            test_cpu_ledger(),
            view,
            limits,
            Some(
                Current3dDemandRequest::new(
                    PresentationViewport::new(64.0, 64.0).unwrap(),
                    RenderExtent::new(64, 64).unwrap(),
                    limits,
                    Current3dDemandBaselines::empty(),
                )
                .with_playback(
                    true,
                    vec![TimeIndex::new(2)],
                    1,
                    Some(Arc::new(scales)),
                ),
            ),
            Vec::new(),
            unaccounted_requirement_handle(Arc::from([])),
            Vec::new(),
            None,
        )
        .with_temporal_frame_contract(Some(
            session
                .contract()
                .unwrap()
                .frame_contract(TimeIndex::new(1)),
        ));

        let prepared = plan_visible_demand(request, || false).unwrap().unwrap();
        let PreparedVisibleTargets::Temporal(delta) = prepared.targets else {
            panic!("temporal planning returned an ordinary target delta");
        };
        assert!(delta.current_3d.is_some());
        assert!(delta.cross_sections.is_empty());
    }

    #[test]
    fn active_playback_linked_only_planning_keeps_geometry_independent_full_volume_bodies() {
        let (catalog, view) = temporal_fixture(
            Shape3D::new(64, 64, 64).unwrap(),
            3,
            TimeIndex::new(1),
            ViewerLayout::FourPanel,
        );
        let scales = BTreeMap::from([(LogicalLayerKey::new(0), ScaleLevel::BASE)]);
        let limits = DatasetDemandPlanLimits::new(4_096, 65_536, u64::MAX);
        let presentation = PresentationViewport::new(64.0, 64.0).unwrap();
        let extent = RenderExtent::new(64, 64).unwrap();
        let cross_sections = [PanelId::Xy, PanelId::Xz, PanelId::Yz]
            .into_iter()
            .map(|panel| {
                CrossSectionDemandRequest::new(
                    panel,
                    presentation,
                    extent,
                    limits,
                    ScopeDemandBaseline::empty(),
                )
            })
            .collect();
        let request = CameraDemandRequest::new(
            RenderIntentRevision::initial(),
            catalog,
            test_cpu_ledger(),
            view,
            limits,
            None,
            cross_sections,
            unaccounted_requirement_handle(Arc::from([])),
            Vec::new(),
            None,
        )
        .with_fixed_playback_layer_scales(Some(Arc::new(scales.clone())));

        let prepared = plan_visible_demand(request, || false).unwrap().unwrap();
        let panels = prepared.cross_sections();
        assert_eq!(panels.len(), 3);
        let canonical = Arc::clone(panels[0].plan.requirements.canonical());
        assert!(!canonical.is_empty());
        for panel in panels {
            assert!(panel.plan.covers_full_volume);
            assert_eq!(panel.plan.layer_scales, scales);
            assert_eq!(
                panel.plan.requirements.canonical().as_ref(),
                canonical.as_ref()
            );
            assert_eq!(
                panel.plan.requirements.required_prefix_len(),
                panel.plan.requirements.ranked().len()
            );
        }
    }

    fn test_cpu_ledger() -> Arc<dyn CpuByteLedger> {
        Arc::new(TestCpuLedger)
    }

    struct QueueCapLedger {
        live_result_bytes: Arc<AtomicU64>,
        capacity_bytes: u64,
        rejected_result_reservations: Arc<AtomicU64>,
    }

    struct QueueCapLease {
        category: CpuLedgerCategory,
        bytes: u64,
        live_result_bytes: Option<Arc<AtomicU64>>,
    }

    impl CpuByteLease for QueueCapLease {
        fn category(&self) -> CpuLedgerCategory {
            self.category
        }

        fn reserved_bytes(&self) -> u64 {
            self.bytes
        }
    }

    impl Drop for QueueCapLease {
        fn drop(&mut self) {
            if let Some(live) = &self.live_result_bytes {
                live.fetch_sub(self.bytes, Ordering::AcqRel);
            }
        }
    }

    impl CpuByteLedger for QueueCapLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            if bytes == 0 {
                return Err(CpuLedgerError::ZeroByteReservation);
            }
            if category != CpuLedgerCategory::QueuesAndResults {
                return Ok(Box::new(QueueCapLease {
                    category,
                    bytes,
                    live_result_bytes: None,
                }));
            }
            let mut live = self.live_result_bytes.load(Ordering::Acquire);
            loop {
                let Some(next) = live.checked_add(bytes) else {
                    self.rejected_result_reservations
                        .fetch_add(1, Ordering::Relaxed);
                    return Err(CpuLedgerError::CapacityExceeded {
                        category,
                        requested_bytes: bytes,
                        available_bytes: self.capacity_bytes.saturating_sub(live),
                    });
                };
                if next > self.capacity_bytes {
                    self.rejected_result_reservations
                        .fetch_add(1, Ordering::Relaxed);
                    return Err(CpuLedgerError::CapacityExceeded {
                        category,
                        requested_bytes: bytes,
                        available_bytes: self.capacity_bytes.saturating_sub(live),
                    });
                }
                match self.live_result_bytes.compare_exchange_weak(
                    live,
                    next,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(current) => live = current,
                }
            }
            Ok(Box::new(QueueCapLease {
                category,
                bytes,
                live_result_bytes: Some(Arc::clone(&self.live_result_bytes)),
            }))
        }
    }

    struct TrackingCpuLedger {
        live_bytes: Arc<AtomicU64>,
        capacity_bytes: Arc<AtomicU64>,
    }

    struct TrackingCpuLease {
        category: CpuLedgerCategory,
        bytes: u64,
        live_bytes: Arc<AtomicU64>,
    }

    impl CpuByteLease for TrackingCpuLease {
        fn category(&self) -> CpuLedgerCategory {
            self.category
        }

        fn reserved_bytes(&self) -> u64 {
            self.bytes
        }
    }

    impl Drop for TrackingCpuLease {
        fn drop(&mut self) {
            self.live_bytes.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }

    impl CpuByteLedger for TrackingCpuLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            if bytes == 0 {
                return Err(CpuLedgerError::ZeroByteReservation);
            }
            let mut live = self.live_bytes.load(Ordering::Acquire);
            loop {
                let capacity = self.capacity_bytes.load(Ordering::Acquire);
                let Some(next) = live.checked_add(bytes) else {
                    return Err(CpuLedgerError::CapacityExceeded {
                        category,
                        requested_bytes: bytes,
                        available_bytes: capacity.saturating_sub(live),
                    });
                };
                if next > capacity {
                    return Err(CpuLedgerError::CapacityExceeded {
                        category,
                        requested_bytes: bytes,
                        available_bytes: capacity.saturating_sub(live),
                    });
                }
                match self.live_bytes.compare_exchange_weak(
                    live,
                    next,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(current) => live = current,
                }
            }
            Ok(Box::new(TrackingCpuLease {
                category,
                bytes,
                live_bytes: Arc::clone(&self.live_bytes),
            }))
        }
    }

    fn unaccounted_requirement_handle(requirements: Arc<[BrickKey]>) -> RetainedRequirementHandle {
        RetainedRequirementHandle {
            requirements,
            charge: None,
        }
    }

    #[test]
    fn latest_slot_exposes_only_the_current_revision_and_never_visits_candidates_on_caller() {
        let (catalog, view) = fixture(Shape3D::new(128, 128, 128).unwrap());
        let mut planner = CameraDemandPlanner::new().unwrap();
        let mut mailbox = RenderIntentMailbox::new();
        let first = mailbox
            .observe_durable_intent(RenderIntentFamily::Both)
            .unwrap();
        assert!(planner.submit(request(first, Arc::clone(&catalog), view.clone(), 4_096,)));
        let latest = mailbox
            .observe_durable_intent(RenderIntentFamily::Both)
            .unwrap();
        assert!(planner.submit(request(latest, Arc::clone(&catalog), view, 4_096,)));
        assert!(latest > first);

        let result = wait_for_result(&mut planner, Duration::from_secs(5));
        assert_eq!(result.revision, latest);
        let planning = result.outcome.unwrap();
        assert!(
            !planning
                .current_3d()
                .expect("a volume request returns volume demand")
                .plan
                .target
                .requirements
                .ranked()
                .is_empty()
        );
        assert!(planning.candidates_visited > 0);

        let diagnostics = planner.diagnostics();
        assert_eq!(diagnostics.submitted, 2);
        assert_eq!(diagnostics.completed, 1);
        assert_eq!(diagnostics.ui_thread_candidates_visited, 0);
        assert_eq!(diagnostics.maximum_pending_requests, 1);
        assert!(diagnostics.pending_replacements + diagnostics.cancelled_running >= 1);
        assert!(!planner.has_outstanding_request());
    }

    #[test]
    fn contained_reuse_diagnostics_separate_reused_from_reevaluated_candidates() {
        let planner = CameraDemandPlanner::new().unwrap();
        planner.record_contained_reuse(50_321);
        planner.record_contained_reuse(50_321);

        let diagnostics = planner.diagnostics();
        assert_eq!(diagnostics.contained_reuses, 2);
        assert_eq!(diagnostics.candidates_reused, 100_642);
        assert_eq!(diagnostics.completed_candidates_visited, 0);
        assert_eq!(diagnostics.ui_thread_candidates_visited, 0);
    }

    #[test]
    fn completed_plan_remains_outstanding_until_result_publication() {
        let (catalog, view) = fixture(Shape3D::new(64, 64, 64).unwrap());
        let mut planner = CameraDemandPlanner::new().unwrap();
        let (entered_rx, release_tx) = planner.block_next_result_publication();
        assert!(planner.submit(request(
            RenderIntentRevision::initial(),
            catalog,
            view,
            4_096,
        )));

        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker reaches the result-publication boundary");
        let remained_outstanding = planner.has_outstanding_request();
        release_tx.send(()).unwrap();

        assert!(
            remained_outstanding,
            "the repaint authority must remain armed before result publication"
        );
        let result = wait_for_result(&mut planner, Duration::from_secs(5));
        assert!(result.outcome.is_ok());
        assert!(!planner.has_outstanding_request());
    }

    #[test]
    fn invalidation_cancels_retired_source_work_without_exposing_a_result() {
        let (catalog, view) = fixture(Shape3D::new(128, 128, 128).unwrap());
        let mut planner = CameraDemandPlanner::new().unwrap();
        let revision = RenderIntentRevision::initial();
        assert!(planner.submit(request(revision, catalog, view, 4_096)));
        planner.invalidate(revision);

        let deadline = Instant::now() + Duration::from_millis(100);
        while Instant::now() < deadline && planner.has_outstanding_request() {
            thread::yield_now();
        }
        assert!(planner.take_result().is_none());
        assert!(!planner.has_outstanding_request());
    }

    #[test]
    fn cross_section_planning_and_render_validation_share_one_worker_built_body() {
        let (catalog, view) = fixture(Shape3D::new(128, 128, 128).unwrap());
        let request = CameraDemandRequest::new(
            RenderIntentRevision::initial(),
            Arc::clone(&catalog),
            test_cpu_ledger(),
            view,
            DatasetDemandPlanLimits::new(4_096, 65_536, u64::MAX),
            None,
            vec![CrossSectionDemandRequest::new(
                PanelId::Xy,
                PresentationViewport::new(64.0, 64.0).unwrap(),
                RenderExtent::new(64, 64).unwrap(),
                DatasetDemandPlanLimits::new(4_096, 65_536, u64::MAX),
                ScopeDemandBaseline::empty(),
            )],
            unaccounted_requirement_handle(Arc::from([])),
            Vec::new(),
            None,
        );
        let mut planner = CameraDemandPlanner::new().unwrap();
        assert!(planner.submit(request));

        let result = wait_for_result(&mut planner, Duration::from_secs(5));
        let planning = result.outcome.unwrap();
        assert!(planning.current_3d().is_none());
        assert_eq!(planning.cross_sections().len(), 1);
        let cross = &planning.cross_sections()[0];
        assert_eq!(cross.panel, PanelId::Xy);
        assert!(!cross.plan.requirements.canonical().is_empty());
        assert!(
            cross
                .render_requirements
                .as_ref()
                .expect("a non-empty plane owns render requirements")
                .body()
                .shares_storage_with(cross.plan.requirements.body())
        );
        assert_eq!(
            planning
                .renderer_requirement_update
                .next
                .requirements
                .as_ref(),
            cross.plan.requirements.canonical().as_ref()
        );
    }

    #[test]
    fn coarsest_floor_deduplicates_same_scale_guard_without_false_retry() {
        let (catalog, view) = fixture(Shape3D::new(512, 512, 512).unwrap());
        let input = CrossSectionDemandRequest::new(
            PanelId::Xy,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(4_096, 4_096, u64::MAX),
            ScopeDemandBaseline::empty(),
        );
        let exact = plan_cross_section_panel_attempt(
            catalog.as_ref(),
            &view,
            input.panel,
            input.presentation,
            input.viewport,
            input.limits,
            Some(&TestCpuLedger),
            None,
            false,
            &mut || false,
        )
        .unwrap()
        .unwrap();
        let guarded = plan_cross_section_panel_attempt(
            catalog.as_ref(),
            &view,
            input.panel,
            input.presentation,
            input.viewport,
            input.limits,
            Some(&TestCpuLedger),
            None,
            true,
            &mut || false,
        )
        .unwrap()
        .unwrap();
        let exact_count = exact.plan.resources.len();
        let guarded_count = guarded.plan.resources.len();
        assert!(
            guarded_count > exact_count,
            "the fixture must successfully plan a real guard before preparation fails"
        );
        let floor = plan_cross_section_navigation_floor_cancellable(
            catalog.as_ref(),
            &view,
            input.limits,
            Some(&TestCpuLedger),
            &mut || false,
        )
        .unwrap()
        .unwrap();
        let floor_count = floor.plan.resources.len();
        let final_body_bytes =
            PreparedResourceBody::preflight_host_allocation_bytes(floor_count, floor_count)
                .unwrap();
        let final_render_bytes =
            PreparedRenderRequirements::preflight_host_allocation_bytes_with_scale_count(
                1,
                1,
                floor_count,
            )
            .unwrap();
        let final_result_bytes = final_body_bytes.checked_add(final_render_bytes).unwrap();

        let live_result_bytes = Arc::new(AtomicU64::new(0));
        let rejected_result_reservations = Arc::new(AtomicU64::new(0));
        let ledger = QueueCapLedger {
            live_result_bytes: Arc::clone(&live_result_bytes),
            capacity_bytes: final_result_bytes,
            rejected_result_reservations: Arc::clone(&rejected_result_reservations),
        };
        let (prepared, _work) = prepare_adaptive_cross_section_panel(
            catalog.as_ref(),
            &view,
            &input,
            CrossSectionPlanningObligations {
                navigation_floor: &floor.plan,
                obligated_resources: &[],
                playback_layer_scales: None,
            },
            &ledger,
            &mut || false,
        )
        .unwrap()
        .expect("the floor-deduplicated guarded body fits its exact retained-result budget");

        assert_eq!(rejected_result_reservations.load(Ordering::Acquire), 0);
        assert_eq!(prepared.plan.requirements.ranked().len(), floor_count);
        assert_eq!(prepared.plan.primary_resource_count, floor_count);
        assert_eq!(prepared.plan.layer_scales, guarded.plan.layer_scales);
        assert!(
            prepared.reuse_envelope.is_some(),
            "a same-scale guard contained by the mandatory floor remains a valid zero-cost reuse proof"
        );
        drop(prepared);
        assert_eq!(
            live_result_bytes.load(Ordering::Acquire),
            0,
            "the successful exact body owns and releases precisely its fresh reservations"
        );
    }

    #[test]
    fn full_envelope_union_delta_is_prepared_on_worker_with_two_removals() {
        let identity = DatasetResourceIdentity::SessionLocal(DatasetSourceId::new(77));
        let key = |x| {
            BrickKey::new(
                identity,
                LogicalLayerKey::new(0),
                TimeIndex::new(0),
                mirante4d_domain::ScaleLevel::BASE,
                ResourceRegion::new([0, 0, x], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
            )
        };
        let previous = (0..65_536).map(key).collect::<Arc<[_]>>();
        let expected = [previous[19], previous[65_019]];
        let next = previous
            .iter()
            .copied()
            .filter(|candidate| expected.binary_search(candidate).is_err())
            .collect::<Arc<[_]>>();
        let ledger = TestCpuLedger;
        let mut reservations = PreparedAllocationReservations::new(&ledger);

        let removals =
            prepare_requirement_removals(&previous, &next, &mut reservations, &mut || false)
                .unwrap()
                .unwrap();
        let charge = reservations.finish();

        assert_eq!(removals.as_ref(), expected.as_slice());
        assert_eq!(
            charge.reserved_bytes(),
            u64::try_from(expected.len() * std::mem::size_of::<BrickKey>()).unwrap()
        );
    }

    #[test]
    fn partial_scope_replacement_keeps_old_body_and_new_union_bytes_charged_by_their_owners() {
        let live_bytes = Arc::new(AtomicU64::new(0));
        let capacity_bytes = Arc::new(AtomicU64::new(u64::MAX));
        let ledger: Arc<dyn CpuByteLedger> = Arc::new(TrackingCpuLedger {
            live_bytes: Arc::clone(&live_bytes),
            capacity_bytes,
        });
        let (catalog, view) = fixture(Shape3D::new(128, 128, 128).unwrap());
        let mut planner = CameraDemandPlanner::new().unwrap();
        let mut mailbox = RenderIntentMailbox::new();
        let first_revision = mailbox
            .observe_durable_intent(RenderIntentFamily::Both)
            .unwrap();
        assert!(planner.submit(volume_request_with_ledger(
            first_revision,
            Arc::clone(&catalog),
            Arc::clone(&ledger),
            view.clone(),
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(4_096, 65_536, u64::MAX),
        )));
        let first = wait_for_result(&mut planner, Duration::from_secs(5))
            .outcome
            .unwrap();
        let installed_volume_body = first
            .current_3d()
            .unwrap()
            .current
            .requirements
            .body()
            .clone();
        drop(first);
        let old_scope_bytes = live_bytes.load(Ordering::Acquire);
        assert!(old_scope_bytes > 0, "the installed body retains its charge");

        let replacement_revision = mailbox
            .observe_durable_intent(RenderIntentFamily::Both)
            .unwrap();
        assert!(planner.submit(CameraDemandRequest::new(
            replacement_revision,
            Arc::clone(&catalog),
            Arc::clone(&ledger),
            view,
            DatasetDemandPlanLimits::new(4_096, 65_536, u64::MAX),
            None,
            vec![CrossSectionDemandRequest::new(
                PanelId::Xy,
                PresentationViewport::new(64.0, 64.0).unwrap(),
                RenderExtent::new(64, 64).unwrap(),
                DatasetDemandPlanLimits::new(4_096, 65_536, u64::MAX),
                ScopeDemandBaseline::empty(),
            )],
            unaccounted_requirement_handle(Arc::from([])),
            vec![installed_volume_body.clone()],
            None,
        )));
        let replacement = wait_for_result(&mut planner, Duration::from_secs(5))
            .outcome
            .unwrap();
        let installed_union =
            Arc::clone(&replacement.renderer_requirement_update.next.requirements);
        let installed_union_charge =
            Arc::clone(&replacement.renderer_requirement_update.next.charge);
        drop(replacement);
        drop(installed_volume_body);
        assert!(
            live_bytes.load(Ordering::Acquire) > 0,
            "the replacement state retains its explicit union charge"
        );
        drop(installed_union);
        assert!(live_bytes.load(Ordering::Acquire) > 0);
        drop(installed_union_charge);
        assert_eq!(live_bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn pending_replacement_retains_the_accounted_old_union_until_update_retirement() {
        let live_bytes = Arc::new(AtomicU64::new(0));
        let capacity_bytes = Arc::new(AtomicU64::new(u64::MAX));
        let ledger: Arc<dyn CpuByteLedger> = Arc::new(TrackingCpuLedger {
            live_bytes: Arc::clone(&live_bytes),
            capacity_bytes,
        });
        let (catalog, view) = fixture(Shape3D::new(128, 128, 128).unwrap());
        let old_requirement = BrickKey::new(
            catalog.resource_identity(),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            mirante4d_domain::ScaleLevel::BASE,
            ResourceRegion::new([9_999, 0, 0], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
        );
        let old_requirements: Arc<[BrickKey]> = Arc::from([old_requirement]);
        let old_bytes = u64::try_from(std::mem::size_of::<BrickKey>()).unwrap();
        let old_charge: Arc<dyn CpuByteLease> = Arc::from(
            ledger
                .try_acquire(CpuLedgerCategory::QueuesAndResults, old_bytes)
                .unwrap(),
        );
        let weak_requirements = Arc::downgrade(&old_requirements);
        let weak_charge = Arc::downgrade(&old_charge);
        let installed_owner = RetainedRequirementHandle {
            requirements: Arc::clone(&old_requirements),
            charge: Some(Arc::clone(&old_charge)),
        };
        let request = CameraDemandRequest::new(
            RenderIntentRevision::initial(),
            Arc::clone(&catalog),
            Arc::clone(&ledger),
            view,
            DatasetDemandPlanLimits::new(4_096, 65_536, u64::MAX),
            None,
            vec![CrossSectionDemandRequest::new(
                PanelId::Xy,
                PresentationViewport::new(64.0, 64.0).unwrap(),
                RenderExtent::new(64, 64).unwrap(),
                DatasetDemandPlanLimits::new(4_096, 65_536, u64::MAX),
                ScopeDemandBaseline::empty(),
            )],
            installed_owner.clone(),
            Vec::new(),
            None,
        );
        let mut planner = CameraDemandPlanner::new().unwrap();
        assert!(planner.submit(request));
        drop(installed_owner);
        drop(old_requirements);
        drop(old_charge);

        assert!(weak_requirements.upgrade().is_some());
        assert!(weak_charge.upgrade().is_some());
        assert!(live_bytes.load(Ordering::Acquire) >= old_bytes);

        let prepared = wait_for_result(&mut planner, Duration::from_secs(5))
            .outcome
            .unwrap();
        let retained_requirements = weak_requirements
            .upgrade()
            .expect("the prepared update keeps the old union body alive");
        let retained_charge = weak_charge
            .upgrade()
            .expect("the prepared update keeps the old union charge alive");
        assert!(Arc::ptr_eq(
            &prepared.renderer_requirement_update.previous.requirements,
            &retained_requirements
        ));
        assert!(Arc::ptr_eq(
            prepared
                .renderer_requirement_update
                .previous
                .charge
                .as_ref()
                .expect("production replacement carries an accounted old union"),
            &retained_charge
        ));
        drop(retained_requirements);
        drop(retained_charge);
        drop(prepared);

        assert!(weak_requirements.upgrade().is_none());
        assert!(weak_charge.upgrade().is_none());
        assert_eq!(live_bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn linked_floor_charge_does_not_pin_replaced_current_or_create_false_capacity() {
        let live_bytes = Arc::new(AtomicU64::new(0));
        let capacity_bytes = Arc::new(AtomicU64::new(u64::MAX));
        let ledger: Arc<dyn CpuByteLedger> = Arc::new(TrackingCpuLedger {
            live_bytes: Arc::clone(&live_bytes),
            capacity_bytes: Arc::clone(&capacity_bytes),
        });
        let (catalog, view) = fixture(Shape3D::new(512, 512, 512).unwrap());
        let request = CameraDemandRequest::new(
            RenderIntentRevision::initial(),
            catalog,
            Arc::clone(&ledger),
            view,
            DatasetDemandPlanLimits::new(4_096, 65_536, u64::MAX),
            Some(Current3dDemandRequest::new(
                PresentationViewport::new(320.0, 320.0).unwrap(),
                RenderExtent::new(320, 320).unwrap(),
                DatasetDemandPlanLimits::new(4_096, 65_536, u64::MAX),
                Current3dDemandBaselines::empty(),
            )),
            vec![CrossSectionDemandRequest::new(
                PanelId::Xy,
                PresentationViewport::new(1.0, 1.0).unwrap(),
                RenderExtent::new(1, 1).unwrap(),
                DatasetDemandPlanLimits::new(4_096, 65_536, u64::MAX),
                ScopeDemandBaseline::empty(),
            )],
            unaccounted_requirement_handle(Arc::from([])),
            Vec::new(),
            None,
        );
        let prepared = plan_visible_demand(request, || false).unwrap().unwrap();
        let current_charge = prepared
            .current_3d()
            .unwrap()
            .current
            .requirements
            .body()
            .charged_bytes()
            .unwrap();
        let cross_body = prepared.cross_sections()[0]
            .plan
            .requirements
            .body()
            .clone();
        let cross_charge = cross_body.charged_bytes().unwrap();
        assert!(current_charge != 0);
        assert!(cross_charge != 0);

        drop(prepared);
        assert_eq!(
            live_bytes.load(Ordering::Acquire),
            cross_charge,
            "the surviving linked panel must own only its independently charged cohort"
        );
        capacity_bytes.store(
            cross_charge.checked_add(current_charge).unwrap(),
            Ordering::Release,
        );
        let replacement = ledger
            .try_acquire(CpuLedgerCategory::QueuesAndResults, current_charge)
            .expect("released large-current bytes must be immediately reusable");
        drop(replacement);
        assert_eq!(live_bytes.load(Ordering::Acquire), cross_charge);
        drop(cross_body);
        assert_eq!(live_bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn asymmetric_four_target_union_borrows_the_renderer_global_capacity() {
        let (catalog, single_view) = fixture(Shape3D::new(2_048, 2_048, 2_048).unwrap());
        let view = ViewState::new(
            single_view.layers().to_vec(),
            single_view.active_layer(),
            single_view.timepoint(),
            *single_view.camera(),
            ViewerLayout::FourPanel,
            *single_view.cross_section(),
            *single_view.iso_light(),
        )
        .unwrap();
        let limits = DatasetDemandPlanLimits::new(131_072, 65_536, u64::MAX);
        let presentation = PresentationViewport::new(320.0, 320.0).unwrap();
        let viewport = RenderExtent::new(320, 320).unwrap();
        let cross_sections = [PanelId::Xy, PanelId::Xz, PanelId::Yz]
            .into_iter()
            .map(|panel| {
                CrossSectionDemandRequest::new(
                    panel,
                    presentation,
                    viewport,
                    limits,
                    ScopeDemandBaseline::empty(),
                )
            })
            .collect();
        let request = CameraDemandRequest::new(
            RenderIntentRevision::initial(),
            Arc::clone(&catalog),
            test_cpu_ledger(),
            view,
            limits,
            Some(Current3dDemandRequest::new(
                presentation,
                viewport,
                limits,
                Current3dDemandBaselines::empty(),
            )),
            cross_sections,
            unaccounted_requirement_handle(Arc::from([])),
            Vec::new(),
            None,
        );

        let prepared = plan_visible_demand(request, || false).unwrap().unwrap();
        let current_count = prepared
            .current_3d()
            .expect("the four-target request includes 3D")
            .plan
            .target
            .requirements
            .canonical()
            .len();
        let old_equal_share = limits.max_resources / 4;
        assert!(
            current_count > old_equal_share,
            "the active 3D target must need more than the deleted one-quarter quota"
        );
        assert_eq!(prepared.cross_sections().len(), 3);
        assert!(
            prepared.renderer_requirement_update.next.requirements.len() <= limits.max_resources,
            "the exact deduplicated four-target union must fit the one global renderer limit"
        );
    }

    #[test]
    fn alternating_current_and_cross_replacements_release_only_replaced_cohorts() {
        let live_bytes = Arc::new(AtomicU64::new(0));
        let capacity_bytes = Arc::new(AtomicU64::new(u64::MAX));
        let ledger: Arc<dyn CpuByteLedger> = Arc::new(TrackingCpuLedger {
            live_bytes: Arc::clone(&live_bytes),
            capacity_bytes,
        });
        let (catalog, view) = fixture(Shape3D::new(256, 256, 256).unwrap());
        let limits = DatasetDemandPlanLimits::new(4_096, 65_536, u64::MAX);
        let first = plan_visible_demand(
            CameraDemandRequest::new(
                RenderIntentRevision::initial(),
                Arc::clone(&catalog),
                Arc::clone(&ledger),
                view.clone(),
                limits,
                Some(Current3dDemandRequest::new(
                    PresentationViewport::new(128.0, 128.0).unwrap(),
                    RenderExtent::new(128, 128).unwrap(),
                    limits,
                    Current3dDemandBaselines::empty(),
                )),
                vec![CrossSectionDemandRequest::new(
                    PanelId::Xy,
                    PresentationViewport::new(1.0, 1.0).unwrap(),
                    RenderExtent::new(1, 1).unwrap(),
                    limits,
                    ScopeDemandBaseline::empty(),
                )],
                unaccounted_requirement_handle(Arc::from([])),
                Vec::new(),
                None,
            ),
            || false,
        )
        .unwrap()
        .unwrap();
        let first_3d = first.current_3d().unwrap();
        let first_current = first_3d.current.requirements.body().clone();
        let first_refinement = first_3d.refinement.requirements.body().clone();
        let first_playback = first_3d.playback.requirements.body().clone();
        let first_3d_charge = first_current.charged_bytes().unwrap();
        let first_cross = first.cross_sections()[0].plan.requirements.body().clone();
        let first_cross_charge = first_cross.charged_bytes().unwrap();
        let first_union = Arc::clone(&first.renderer_requirement_update.next.requirements);
        let first_union_charge = Arc::clone(&first.renderer_requirement_update.next.charge);
        let first_union_bytes = first_union_charge.reserved_bytes();
        drop(first);
        assert_eq!(
            live_bytes.load(Ordering::Acquire),
            first_3d_charge + first_cross_charge + first_union_bytes
        );

        let second = plan_visible_demand(
            CameraDemandRequest::new(
                RenderIntentRevision::initial(),
                Arc::clone(&catalog),
                Arc::clone(&ledger),
                view.clone(),
                limits,
                Some(Current3dDemandRequest::new(
                    PresentationViewport::new(96.0, 96.0).unwrap(),
                    RenderExtent::new(96, 96).unwrap(),
                    limits,
                    Current3dDemandBaselines::new(
                        ScopeDemandBaseline::new(first_current.clone(), 0),
                        ScopeDemandBaseline::new(first_refinement.clone(), 0),
                        ScopeDemandBaseline::new(first_playback.clone(), 0),
                    ),
                )),
                Vec::new(),
                RetainedRequirementHandle {
                    requirements: Arc::clone(&first_union),
                    charge: Some(Arc::clone(&first_union_charge)),
                },
                vec![first_cross.clone()],
                None,
            ),
            || false,
        )
        .unwrap()
        .unwrap();
        let second_3d = second.current_3d().unwrap();
        let second_current = second_3d.current.requirements.body().clone();
        let second_refinement = second_3d.refinement.requirements.body().clone();
        let second_playback = second_3d.playback.requirements.body().clone();
        let second_3d_charge = second_current.charged_bytes().unwrap();
        let second_union = Arc::clone(&second.renderer_requirement_update.next.requirements);
        let second_union_charge = Arc::clone(&second.renderer_requirement_update.next.charge);
        let second_union_bytes = second_union_charge.reserved_bytes();
        drop(second);
        drop(first_current);
        drop(first_refinement);
        drop(first_playback);
        drop(first_union);
        drop(first_union_charge);
        assert_eq!(
            live_bytes.load(Ordering::Acquire),
            first_cross_charge + second_3d_charge + second_union_bytes,
            "a current replacement must not retain the old current cohort"
        );

        let third = plan_visible_demand(
            CameraDemandRequest::new(
                RenderIntentRevision::initial(),
                catalog,
                Arc::clone(&ledger),
                view,
                limits,
                None,
                vec![CrossSectionDemandRequest::new(
                    PanelId::Xy,
                    PresentationViewport::new(2.0, 2.0).unwrap(),
                    RenderExtent::new(2, 2).unwrap(),
                    limits,
                    ScopeDemandBaseline::new(first_cross.clone(), 0),
                )],
                RetainedRequirementHandle {
                    requirements: Arc::clone(&second_union),
                    charge: Some(Arc::clone(&second_union_charge)),
                },
                vec![
                    second_current.clone(),
                    second_refinement.clone(),
                    second_playback.clone(),
                ],
                None,
            ),
            || false,
        )
        .unwrap()
        .unwrap();
        let third_cross = third.cross_sections()[0].plan.requirements.body().clone();
        let third_cross_charge = third_cross.charged_bytes().unwrap();
        let third_union_charge = Arc::clone(&third.renderer_requirement_update.next.charge);
        let third_union_bytes = third_union_charge.reserved_bytes();
        drop(third);
        drop(first_cross);
        drop(second_union);
        drop(second_union_charge);
        assert_eq!(
            live_bytes.load(Ordering::Acquire),
            second_3d_charge + third_cross_charge + third_union_bytes,
            "a cross replacement must not retain the old cross cohort"
        );

        drop(second_current);
        drop(second_refinement);
        drop(second_playback);
        drop(third_cross);
        drop(third_union_charge);
        assert_eq!(live_bytes.load(Ordering::Acquire), 0);
    }

    /// Development-only structural/timing evidence. Run in release mode and
    /// report the printed CPU/build/workload facts; this is not a product
    /// performance threshold or qualification.
    #[test]
    #[ignore = "release-only large camera-planning diagnostic"]
    // The constant assertion is intentional: this ignored diagnostic is
    // meaningful only with release optimizations enabled.
    #[allow(clippy::assertions_on_constants)]
    fn large_oblique_grid_moves_full_planning_off_the_caller() {
        assert!(
            !cfg!(debug_assertions),
            "run this diagnostic in release mode"
        );
        let (catalog, view) = large_oblique_fixture();
        let presentation = PresentationViewport::new(2.0, 2.0).unwrap();
        let viewport = RenderExtent::new(2, 2).unwrap();
        let limits = DatasetDemandPlanLimits::new(131_072, 65_536, u64::MAX);

        let mut planner = CameraDemandPlanner::new().unwrap();
        let mut synchronous_samples = Vec::with_capacity(5);
        let mut enqueue_samples = Vec::with_capacity(5);
        let mut worker_samples = Vec::with_capacity(5);
        let mut accepted = None;
        let mut mailbox = RenderIntentMailbox::new();
        for _ in 0..5 {
            let synchronous_started = Instant::now();
            let synchronous = plan_progressive_current_3d_cancellable(
                catalog.as_ref(),
                &view,
                presentation,
                viewport,
                limits,
                false,
                None,
                || false,
            )
            .unwrap();
            let synchronous = synchronous.unwrap();
            synchronous_samples.push(synchronous_started.elapsed());
            assert!(synchronous.candidates_visited >= 100_000);

            let enqueue_started = Instant::now();
            let revision = mailbox
                .observe_durable_intent(RenderIntentFamily::Both)
                .unwrap();
            assert!(planner.submit(volume_request(
                revision,
                Arc::clone(&catalog),
                view.clone(),
                presentation,
                viewport,
                limits,
            )));
            enqueue_samples.push(enqueue_started.elapsed());
            let result = wait_for_result(&mut planner, Duration::from_secs(30));
            worker_samples.push(result.planning_duration);
            let planning = result.outcome.unwrap();
            assert_eq!(result.revision, revision);
            assert_eq!(
                planning
                    .current_3d()
                    .expect("a volume request returns volume demand")
                    .plan
                    .target
                    .requirements
                    .ranked()
                    .as_ref(),
                synchronous.plan.target.resources.as_slice()
            );
            assert_eq!(planning.candidates_visited, synchronous.candidates_visited);
            accepted = Some(planning);
        }
        let planning = accepted.expect("five trials produced a current plan");
        assert_eq!(planner.diagnostics().ui_thread_candidates_visited, 0);

        synchronous_samples.sort_unstable();
        enqueue_samples.sort_unstable();
        worker_samples.sort_unstable();
        let median = |samples: &[Duration]| samples[samples.len() / 2].as_nanos();
        let p95 = |samples: &[Duration]| samples[samples.len() - 1].as_nanos();

        let cpu = std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|contents| {
                contents
                    .lines()
                    .find_map(|line| line.strip_prefix("model name\t: ").map(str::to_owned))
            })
            .unwrap_or_else(|| "unreported".to_owned());
        eprintln!(
            "camera-demand diagnostic: cpu={cpu:?} build=release sampling=five sequential warm in-process trials workload=one u16 all-valid 48^3-brick affine grid, orthographic 2x2 viewport, 131072 candidate cap; synchronous_full_plan_p50_ns={} synchronous_full_plan_p95_ns={} caller_enqueue_p50_ns={} caller_enqueue_p95_ns={} worker_full_plan_p50_ns={} worker_full_plan_p95_ns={} candidates_visited={} resources={} ui_thread_candidates_visited=0",
            median(&synchronous_samples),
            p95(&synchronous_samples),
            median(&enqueue_samples),
            p95(&enqueue_samples),
            median(&worker_samples),
            p95(&worker_samples),
            planning.candidates_visited,
            planning
                .current_3d()
                .expect("a volume request returns volume demand")
                .plan
                .target
                .requirements
                .ranked()
                .len(),
        );
    }

    fn wait_for_result(planner: &mut CameraDemandPlanner, timeout: Duration) -> CameraDemandResult {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(result) = planner.take_result() {
                return result;
            }
            assert!(Instant::now() < deadline, "camera-demand result timed out");
            thread::yield_now();
        }
    }

    fn request(
        revision: RenderIntentRevision,
        catalog: Arc<DatasetCatalog>,
        view: ViewState,
        max_candidates: usize,
    ) -> CameraDemandRequest {
        volume_request(
            revision,
            catalog,
            view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(max_candidates, 65_536, u64::MAX),
        )
    }

    fn volume_request(
        revision: RenderIntentRevision,
        catalog: Arc<DatasetCatalog>,
        view: ViewState,
        presentation: PresentationViewport,
        viewport: RenderExtent,
        limits: DatasetDemandPlanLimits,
    ) -> CameraDemandRequest {
        volume_request_with_ledger(
            revision,
            catalog,
            test_cpu_ledger(),
            view,
            presentation,
            viewport,
            limits,
        )
    }

    fn volume_request_with_ledger(
        revision: RenderIntentRevision,
        catalog: Arc<DatasetCatalog>,
        cpu_ledger: Arc<dyn CpuByteLedger>,
        view: ViewState,
        presentation: PresentationViewport,
        viewport: RenderExtent,
        limits: DatasetDemandPlanLimits,
    ) -> CameraDemandRequest {
        CameraDemandRequest::new(
            revision,
            catalog,
            cpu_ledger,
            view,
            limits,
            Some(Current3dDemandRequest::new(
                presentation,
                viewport,
                limits,
                Current3dDemandBaselines::empty(),
            )),
            Vec::new(),
            unaccounted_requirement_handle(Arc::from([])),
            Vec::new(),
            None,
        )
    }

    fn fixture(shape: Shape3D) -> (Arc<DatasetCatalog>, ViewState) {
        fixture_with_transform(shape, GridToWorld::identity(), shape_center(shape))
    }

    fn temporal_fixture(
        shape: Shape3D,
        timepoint_count: u64,
        timepoint: TimeIndex,
        layout: ViewerLayout,
    ) -> (Arc<DatasetCatalog>, ViewState) {
        let active = LogicalLayerKey::new(0);
        let catalog = Arc::new(
            DatasetCatalog::new(
                "temporal-camera-demand-fixture",
                ContentAddressStatus::SessionLocal(DatasetSourceId::new(1)),
                vec![
                    DatasetLayer::new(
                        active,
                        "layer",
                        Shape4D::new(timepoint_count, shape.z(), shape.y(), shape.x()).unwrap(),
                        IntensityDType::Uint16,
                        GridToWorld::identity(),
                        ResourceValidity::AllValid,
                    )
                    .unwrap(),
                ],
            )
            .unwrap(),
        );
        let target = shape_center(shape);
        let camera = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::new(target.x, target.y, target.z).unwrap(),
            UnitQuaternion::identity(),
            64.0,
            320.0,
            shape.z() as f64 * 2.0,
        )
        .unwrap();
        let view = ViewState::new(
            vec![LayerViewState::new(
                active,
                true,
                LayerTransfer::new(
                    DisplayWindow::new(0.0, 65_535.0).unwrap(),
                    RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
                    Opacity::new(1.0).unwrap(),
                    TransferCurve::linear(),
                    false,
                ),
                RenderState::mip(SamplingPolicy::VoxelExact),
            )],
            active,
            timepoint,
            camera,
            layout,
            CrossSectionView::new(
                WorldPoint3::new(target.x, target.y, target.z).unwrap(),
                UnitQuaternion::identity(),
                1.0,
                1.0,
            )
            .unwrap(),
            IsoLightState::attached_camera(),
        )
        .unwrap();
        (catalog, view)
    }

    fn large_oblique_fixture() -> (Arc<DatasetCatalog>, ViewState) {
        let shape = Shape3D::new(48 * 64, 48 * 64, 48 * 64).unwrap();
        let diagonal = DVec3::ONE.normalize();
        let rotation = DQuat::from_rotation_arc(diagonal, DVec3::Z);
        let matrix = DMat4::from_quat(rotation);
        let columns = matrix.to_cols_array();
        let mut row_major = [0.0; 16];
        for row in 0..4 {
            for column in 0..4 {
                row_major[row * 4 + column] = columns[column * 4 + row];
            }
        }
        let transform = GridToWorld::from_row_major(row_major).unwrap();
        let center = matrix.transform_point3(DVec3::splat(shape.x() as f64 * 0.5 - 0.5));
        fixture_with_transform(shape, transform, center)
    }

    fn fixture_with_transform(
        shape: Shape3D,
        transform: GridToWorld,
        camera_target: DVec3,
    ) -> (Arc<DatasetCatalog>, ViewState) {
        let active = LogicalLayerKey::new(0);
        let catalog = Arc::new(
            DatasetCatalog::new(
                "camera-demand-fixture",
                ContentAddressStatus::SessionLocal(DatasetSourceId::new(1)),
                vec![
                    DatasetLayer::new(
                        active,
                        "layer",
                        Shape4D::new(1, shape.z(), shape.y(), shape.x()).unwrap(),
                        IntensityDType::Uint16,
                        transform,
                        ResourceValidity::AllValid,
                    )
                    .unwrap(),
                ],
            )
            .unwrap(),
        );
        let camera = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::new(camera_target.x, camera_target.y, camera_target.z).unwrap(),
            UnitQuaternion::identity(),
            64.0,
            320.0,
            shape.z() as f64 * 2.0,
        )
        .unwrap();
        let view = ViewState::new(
            vec![LayerViewState::new(
                active,
                true,
                LayerTransfer::new(
                    DisplayWindow::new(0.0, 65_535.0).unwrap(),
                    RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
                    Opacity::new(1.0).unwrap(),
                    TransferCurve::linear(),
                    false,
                ),
                RenderState::mip(SamplingPolicy::VoxelExact),
            )],
            active,
            TimeIndex::new(0),
            camera,
            ViewerLayout::Single3d,
            CrossSectionView::new(
                WorldPoint3::new(camera_target.x, camera_target.y, camera_target.z).unwrap(),
                UnitQuaternion::identity(),
                1.0,
                1.0,
            )
            .unwrap(),
            IsoLightState::attached_camera(),
        )
        .unwrap();
        (catalog, view)
    }

    fn shape_center(shape: Shape3D) -> DVec3 {
        DVec3::new(
            shape.x() as f64 * 0.5 - 0.5,
            shape.y() as f64 * 0.5 - 0.5,
            shape.z() as f64 * 0.5 - 0.5,
        )
    }
}
