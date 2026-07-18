//! Bounded latest-only planning for camera-dependent 3D dataset demand.
//!
//! Exact candidate testing and contribution sorting can touch the complete
//! 131,072-brick planning envelope. This owner keeps that work off the UI
//! thread. It has one running job, one replaceable pending job, and one
//! replaceable result; source/view currentness is represented by a monotonic
//! generation and checked before a result is exposed.

use std::{
    fmt, io,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use mirante4d_dataset::{CpuByteLease, CpuByteLedger, DatasetCatalog, DatasetResourceKey};
use mirante4d_domain::{LogicalLayerKey, TimeIndex};
use mirante4d_project_model::ViewState;
use mirante4d_render_api::{
    PreparedRenderRequirements, PreparedResourceBody, PresentationViewport, RenderExtent,
};
use mirante4d_render_wgpu::{
    MAX_INCREMENTAL_STATIC_KEY_CHANGES, PreparedStaticPresentationLayout,
    preflight_static_presentation_layout_update, prepare_static_presentation_layout_update,
};

#[cfg(test)]
use crate::dataset_demand_plan::plan_progressive_current_3d_cancellable;
use crate::dataset_demand_plan::{
    DatasetDemandPlanLimits, PreparedAllocationReservations, PreparedDatasetDemandPlan,
    PreparedDemandRequirements, PreparedProgressiveDatasetDemandPlan,
    bounded_requirement_delta_scratch_bytes, key_array_bytes, plan_cross_section_panel_cancellable,
    plan_guarded_progressive_current_3d_cancellable,
};
use crate::retained_leases::RetainedRequirementHandle;
use crate::viewer_layout::PanelId;

const WORKER_NAME: &str = "mirante4d-camera-demand";
/// After cancellation, wait briefly for an input burst to settle before
/// starting the sole replacement. This turns sustained orbit traffic into
/// one latest request rather than repeatedly traversing stale candidates.
const REPLACEMENT_QUIET_PERIOD: Duration = Duration::from_millis(4);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CameraDemandGeneration(u64);

impl CameraDemandGeneration {
    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

pub(crate) struct CameraDemandRequest {
    catalog: Arc<DatasetCatalog>,
    cpu_ledger: Arc<dyn CpuByteLedger>,
    view: ViewState,
    current_3d: Option<Current3dDemandRequest>,
    cross_sections: Box<[CrossSectionDemandRequest]>,
    previous_renderer_requirement_union: RetainedRequirementHandle,
    unchanged_renderer_requirement_bodies: Box<[PreparedResourceBody]>,
    post_refinement_promotion_unchanged_bodies: Option<Box<[PreparedResourceBody]>>,
}

pub(crate) struct Current3dDemandRequest {
    presentation: PresentationViewport,
    viewport: RenderExtent,
    limits: DatasetDemandPlanLimits,
    playback_active: bool,
    next_playback_timepoint: Option<TimeIndex>,
    preserve_complete_presentation: bool,
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
    static_layout: Option<PreparedStaticPresentationLayout>,
}

impl ScopeDemandBaseline {
    pub(crate) fn new(body: PreparedResourceBody, admitted_prefix_len: usize) -> Self {
        debug_assert!(admitted_prefix_len <= body.ranked().len());
        Self {
            body,
            admitted_prefix_len,
            static_layout: None,
        }
    }

    pub(crate) fn with_static_layout(
        mut self,
        static_layout: Option<PreparedStaticPresentationLayout>,
    ) -> Self {
        self.static_layout = static_layout;
        self
    }

    fn admitted_prefix(&self) -> &[DatasetResourceKey] {
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
        playback_active: bool,
        next_playback_timepoint: Option<TimeIndex>,
        preserve_complete_presentation: bool,
        baselines: Current3dDemandBaselines,
    ) -> Self {
        Self {
            presentation,
            viewport,
            limits,
            playback_active,
            next_playback_timepoint,
            preserve_complete_presentation,
            baselines,
        }
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
        catalog: Arc<DatasetCatalog>,
        cpu_ledger: Arc<dyn CpuByteLedger>,
        view: ViewState,
        current_3d: Option<Current3dDemandRequest>,
        cross_sections: Vec<CrossSectionDemandRequest>,
        previous_renderer_requirement_union: RetainedRequirementHandle,
        unchanged_renderer_requirement_bodies: Vec<PreparedResourceBody>,
        post_refinement_promotion_unchanged_bodies: Option<Vec<PreparedResourceBody>>,
    ) -> Self {
        Self {
            catalog,
            cpu_ledger,
            view,
            current_3d,
            cross_sections: cross_sections.into_boxed_slice(),
            previous_renderer_requirement_union,
            unchanged_renderer_requirement_bodies: unchanged_renderer_requirement_bodies
                .into_boxed_slice(),
            post_refinement_promotion_unchanged_bodies: post_refinement_promotion_unchanged_bodies
                .map(Vec::into_boxed_slice),
        }
    }
}

/// One canonical union and the exact ledger authority for only that union's
/// immutable key array. Keeping the pair indivisible prevents an Arc from
/// escaping its accounting lifetime.
#[derive(Clone)]
pub(crate) struct PreparedRequirementUnion {
    pub(crate) requirements: Arc<[DatasetResourceKey]>,
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
    pub(crate) removals: Arc<[DatasetResourceKey]>,
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
    pub(crate) static_layout: Option<PreparedStaticPresentationLayout>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedCurrent3dDemand {
    pub(crate) plan: PreparedProgressiveDatasetDemandPlan,
    pub(crate) current: PreparedScopeDemand,
    pub(crate) refinement: PreparedScopeDemand,
    pub(crate) playback: PreparedScopeDemand,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedCrossSectionDemand {
    pub(crate) panel: PanelId,
    pub(crate) plan: PreparedDatasetDemandPlan,
    pub(crate) render_requirements: PreparedRenderRequirements,
    pub(crate) static_layout: PreparedStaticPresentationLayout,
}

pub(crate) struct PreparedVisibleDemand {
    pub(crate) current_3d: Option<PreparedCurrent3dDemand>,
    pub(crate) cross_sections: Box<[PreparedCrossSectionDemand]>,
    pub(crate) renderer_requirement_update: PreparedRendererRequirementUpdate,
    pub(crate) post_refinement_promotion_update: Option<PreparedRendererRequirementUpdate>,
    pub(crate) candidates_visited: usize,
}

impl fmt::Debug for PreparedVisibleDemand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedVisibleDemand")
            .field("current_3d", &self.current_3d)
            .field("cross_sections", &self.cross_sections)
            .field(
                "renderer_requirement_union_len",
                &self.renderer_requirement_update.next.requirements.len(),
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

#[derive(Debug)]
pub(crate) struct CameraDemandResult {
    pub(crate) generation: CameraDemandGeneration,
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
    pub(crate) renderer_static_preparations: u64,
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
    GenerationExhausted,
}

impl fmt::Display for CameraDemandPlannerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkerSpawnFailed(kind) => {
                write!(formatter, "failed to spawn camera-demand worker: {kind:?}")
            }
            Self::GenerationExhausted => {
                formatter.write_str("camera-demand generation space is exhausted")
            }
        }
    }
}

impl std::error::Error for CameraDemandPlannerError {}

struct QueuedRequest {
    generation: CameraDemandGeneration,
    submitted_at: Instant,
    request: CameraDemandRequest,
}

#[derive(Default)]
struct WorkerState {
    shutdown: bool,
    pending: Option<QueuedRequest>,
    result: Option<CameraDemandResult>,
}

#[derive(Default)]
struct Counters {
    submitted: AtomicU64,
    pending_replacements: AtomicU64,
    cancelled_running: AtomicU64,
    stale_results_suppressed: AtomicU64,
    completed: AtomicU64,
    completed_candidates_visited: AtomicU64,
    renderer_static_preparations: AtomicU64,
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
    latest_generation: AtomicU64,
    running_generation: AtomicU64,
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
    next_generation: u64,
}

impl CameraDemandPlanner {
    pub(crate) fn new() -> Result<Self, CameraDemandPlannerError> {
        let shared = Arc::new(Shared {
            state: Mutex::new(WorkerState::default()),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
            latest_generation: AtomicU64::new(0),
            running_generation: AtomicU64::new(0),
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
            next_generation: 1,
        })
    }

    /// Replaces any not-yet-started request and cancels a running older
    /// generation. The caller performs only one bounded slot replacement.
    pub(crate) fn submit(
        &mut self,
        request: CameraDemandRequest,
    ) -> Result<CameraDemandGeneration, CameraDemandPlannerError> {
        let generation = CameraDemandGeneration(self.next_generation);
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or(CameraDemandPlannerError::GenerationExhausted)?;
        self.shared
            .latest_generation
            .store(generation.get(), Ordering::Release);
        self.shared
            .counters
            .submitted
            .fetch_add(1, Ordering::Relaxed);

        let mut state = lock_state(&self.shared);
        if state
            .pending
            .replace(QueuedRequest {
                generation,
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
        Ok(generation)
    }

    /// Takes the sole result only when it still belongs to the latest request.
    pub(crate) fn take_result(&mut self) -> Option<CameraDemandResult> {
        let latest = self.shared.latest_generation.load(Ordering::Acquire);
        let mut state = lock_state(&self.shared);
        let result = state.result.take()?;
        if result.generation.get() == latest {
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
        let latest = self.shared.latest_generation.load(Ordering::Acquire);
        if latest == 0 {
            return false;
        }
        let state = lock_state(&self.shared);
        state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.generation.get() == latest)
            || state
                .result
                .as_ref()
                .is_some_and(|result| result.generation.get() == latest)
            || self.shared.running_generation.load(Ordering::Acquire) == latest
    }

    /// Invalidates pending/running work during source replacement without
    /// manufacturing a request for the retired source.
    pub(crate) fn invalidate(&mut self) -> Result<(), CameraDemandPlannerError> {
        let generation = self.next_generation;
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or(CameraDemandPlannerError::GenerationExhausted)?;
        self.shared
            .latest_generation
            .store(generation, Ordering::Release);
        let mut state = lock_state(&self.shared);
        state.pending = None;
        state.result = None;
        Ok(())
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
            renderer_static_preparations: self
                .shared
                .counters
                .renderer_static_preparations
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
}

impl Drop for CameraDemandPlanner {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.latest_generation.fetch_add(1, Ordering::AcqRel);
        {
            let mut state = lock_state(&self.shared);
            state.shutdown = true;
            state.pending = None;
            state.result = None;
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
                break state
                    .pending
                    .take()
                    .expect("a checked pending request remains present");
            }
        };

        let generation = queued.generation;
        shared
            .running_generation
            .store(generation.get(), Ordering::Release);
        let started = Instant::now();
        let planning = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            plan_visible_demand(
                queued.request,
                || {
                    shared.shutdown.load(Ordering::Acquire)
                        || shared.latest_generation.load(Ordering::Acquire) != generation.get()
                },
                || {
                    shared
                        .counters
                        .renderer_static_preparations
                        .fetch_add(1, Ordering::Relaxed);
                },
            )
        }));
        let outcome = match planning {
            Ok(Ok(Some(planning))) => Ok(planning),
            Ok(Ok(None)) => {
                shared.running_generation.store(0, Ordering::Release);
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
            shared.running_generation.store(0, Ordering::Release);
            return;
        }
        if shared.latest_generation.load(Ordering::Acquire) != generation.get() {
            shared.running_generation.store(0, Ordering::Release);
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
            prepared.current_3d.as_ref().map(|current| {
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
            generation,
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
        if state.shutdown || shared.latest_generation.load(Ordering::Acquire) != generation.get() {
            shared.running_generation.store(0, Ordering::Release);
            shared
                .counters
                .stale_results_suppressed
                .fetch_add(1, Ordering::Relaxed);
            saturating_add_duration(&shared.counters.stale_planning_time_ns, started.elapsed());
            coalesce_replacement = true;
            continue;
        }
        state.result = Some(result);
        // Keep the generation visibly running until the result is installed
        // under the same lock observed by `has_outstanding_request`. Readers
        // therefore see either running work or a publishable result, never an
        // idle gap that can consume the final repaint wakeup.
        shared.running_generation.store(0, Ordering::Release);
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
    mut record_static_preparation: impl FnMut(),
) -> anyhow::Result<Option<PreparedVisibleDemand>> {
    let CameraDemandRequest {
        catalog,
        cpu_ledger,
        view,
        current_3d,
        cross_sections,
        previous_renderer_requirement_union,
        unchanged_renderer_requirement_bodies,
        post_refinement_promotion_unchanged_bodies,
    } = request;
    let mut candidates_visited = 0_usize;
    let current_3d = if let Some(input) = current_3d {
        let mut reservations = PreparedAllocationReservations::new(cpu_ledger.as_ref());
        let planning = plan_guarded_progressive_current_3d_cancellable(
            catalog.as_ref(),
            &view,
            input.presentation,
            input.viewport,
            input.limits,
            input.playback_active,
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
        let playback_requirements = if input.playback_active
            && let Some(timepoint) = input.next_playback_timepoint
        {
            let required_prefix_len = plan.target.requirements.required_prefix_len();
            let temporary_charge = reservations
                .reserve_temporary(key_array_bytes(plan.target.requirements.ranked().len())?)?;
            let ranked = plan
                .target
                .requirements
                .ranked()
                .iter()
                .copied()
                .map(|key| rebind_timepoint(key, timepoint))
                .collect::<Vec<_>>();
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
        let current_artifacts = prepare_presentation_artifacts(
            catalog.as_ref(),
            &view,
            &current_requirements,
            Some(&input.baselines.current),
            &mut reservations,
            &mut record_static_preparation,
        )?;
        if cancelled() {
            return Ok(None);
        }
        let refinement_artifacts = prepare_presentation_artifacts(
            catalog.as_ref(),
            &view,
            &refinement_requirements,
            Some(&input.baselines.refinement),
            &mut reservations,
            &mut record_static_preparation,
        )?;
        if cancelled() {
            return Ok(None);
        }
        let prepared = PreparedCurrent3dDemand {
            current: PreparedScopeDemand {
                requirements: current_requirements,
                render_requirements: current_artifacts
                    .as_ref()
                    .map(|(requirements, _)| requirements.clone()),
                static_layout: current_artifacts.map(|(_, layout)| layout),
            },
            refinement: PreparedScopeDemand {
                requirements: refinement_requirements,
                render_requirements: refinement_artifacts
                    .as_ref()
                    .map(|(requirements, _)| requirements.clone()),
                static_layout: refinement_artifacts.map(|(_, layout)| layout),
            },
            playback: PreparedScopeDemand {
                requirements: playback_requirements,
                render_requirements: None,
                static_layout: None,
            },
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
        Some(prepared)
    } else {
        None
    };

    let mut prepared_cross_sections = Vec::with_capacity(cross_sections.len());
    for input in cross_sections {
        let mut reservations = PreparedAllocationReservations::new(cpu_ledger.as_ref());
        if cancelled() {
            return Ok(None);
        }
        let planning = plan_cross_section_panel_cancellable(
            catalog.as_ref(),
            &view,
            input.panel,
            input.presentation,
            input.viewport,
            input.limits,
            Some(cpu_ledger.as_ref()),
            &mut cancelled,
        )?;
        let Some(planning) = planning else {
            return Ok(None);
        };
        let (plan, work) = planning.prepare_accounted(&mut reservations)?;
        candidates_visited = candidates_visited.saturating_add(work);
        let PreparedDatasetDemandPlan {
            scale,
            layer_scales,
            requirements,
            payload_bytes,
            playback_downshifted,
            covers_full_volume,
            primary_resource_count,
        } = plan;
        let requirements = requirements.into_preserving_admitted_prefix_accounted(
            input.baseline.admitted_prefix(),
            &reservations,
        )?;
        let plan = PreparedDatasetDemandPlan {
            scale,
            layer_scales,
            requirements,
            payload_bytes,
            playback_downshifted,
            covers_full_volume,
            primary_resource_count,
        };
        let (render_requirements, static_layout) = prepare_presentation_artifacts(
            catalog.as_ref(),
            &view,
            &plan.requirements,
            Some(&input.baseline),
            &mut reservations,
            &mut record_static_preparation,
        )?
        .ok_or_else(|| anyhow::anyhow!("a planned cross section has no resources"))?;
        if cancelled() {
            return Ok(None);
        }
        let prepared = PreparedCrossSectionDemand {
            panel: input.panel,
            plan,
            render_requirements,
            static_layout,
        };
        let panel_charge = reservations.finish();
        prepared
            .plan
            .requirements
            .body()
            .attach_charge(panel_charge)?;
        prepared_cross_sections.push(prepared);
    }
    if cancelled() {
        return Ok(None);
    }

    let unchanged_renderer_requirement_bodies = unchanged_renderer_requirement_bodies.into_vec();
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
    if let Some(current) = current_3d.as_ref() {
        union_bodies.push(Arc::clone(current.current.requirements.canonical()));
        union_bodies.push(Arc::clone(current.refinement.requirements.canonical()));
        union_bodies.push(Arc::clone(current.playback.requirements.canonical()));
    }
    union_bodies.extend(cross_bodies);
    let mut union_reservations = PreparedAllocationReservations::new(cpu_ledger.as_ref());
    let Some(renderer_requirement_union) =
        merge_requirement_union(&union_bodies, &mut union_reservations, &mut cancelled)?
    else {
        return Ok(None);
    };
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
    Ok(Some(PreparedVisibleDemand {
        current_3d,
        cross_sections: prepared_cross_sections.into_boxed_slice(),
        renderer_requirement_update: PreparedRendererRequirementUpdate {
            previous: previous_renderer_requirement_union,
            next: PreparedRequirementUnion {
                requirements: renderer_requirement_union,
                charge: renderer_union_charge,
            },
            removals,
            removal_charge,
        },
        post_refinement_promotion_update,
        candidates_visited,
    }))
}

fn prepare_presentation_artifacts(
    catalog: &DatasetCatalog,
    view: &ViewState,
    requirements: &PreparedDemandRequirements,
    baseline: Option<&ScopeDemandBaseline>,
    reservations: &mut PreparedAllocationReservations<'_>,
    record_static_preparation: &mut impl FnMut(),
) -> anyhow::Result<Option<(PreparedRenderRequirements, PreparedStaticPresentationLayout)>> {
    let Some(first) = requirements.canonical().first().copied() else {
        return Ok(None);
    };
    let layer_count = view.layers().iter().filter(|layer| layer.visible()).count();
    reservations.reserve_result(PreparedRenderRequirements::preflight_host_allocation_bytes(
        layer_count,
        requirements.canonical().len(),
    )?)?;
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
    let render_requirements = PreparedRenderRequirements::new_with_required_prefix(
        catalog.resource_identity(),
        first.timepoint(),
        layers,
        requirements.body().clone(),
        1,
        requirements.required_prefix_len(),
    )?;
    drop(layer_scratch);
    let previous_layout = baseline.and_then(|baseline| baseline.static_layout.as_ref());
    let delta_scratch = if previous_layout.is_some() {
        Some(
            reservations.reserve_temporary(bounded_requirement_delta_scratch_bytes(
                MAX_INCREMENTAL_STATIC_KEY_CHANGES,
            )?)?,
        )
    } else {
        None
    };
    let delta = previous_layout.and_then(|_| {
        requirements.bounded_delta_from(
            baseline
                .expect("a previous layout belongs to a baseline")
                .body
                .canonical(),
            MAX_INCREMENTAL_STATIC_KEY_CHANGES,
        )
    });
    // A material replacement deliberately drops the predecessor: the one
    // full builder then owns both preflight and construction.
    let incremental_previous = delta.as_ref().and(previous_layout);
    let additions = delta
        .as_ref()
        .map_or(&[][..], |delta| delta.additions().as_ref());
    let removals = delta
        .as_ref()
        .map_or(&[][..], |delta| delta.removals().as_ref());
    let preflight = preflight_static_presentation_layout_update(
        catalog,
        &render_requirements,
        incremental_previous,
        additions,
        removals,
    )?;
    reservations.reserve_result(preflight.renderer_host_allocation_bytes())?;
    let construction_scratch =
        reservations.reserve_temporary(preflight.construction_scratch_allocation_bytes())?;
    let static_layout = prepare_static_presentation_layout_update(
        catalog,
        &render_requirements,
        incremental_previous,
        additions,
        removals,
    )?;
    record_static_preparation();
    drop(construction_scratch);
    drop(delta_scratch);
    Ok(Some((render_requirements, static_layout)))
}

pub(crate) fn merge_requirement_union(
    bodies: &[Arc<[DatasetResourceKey]>],
    reservations: &mut PreparedAllocationReservations<'_>,
    cancelled: &mut impl FnMut() -> bool,
) -> anyhow::Result<Option<Arc<[DatasetResourceKey]>>> {
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

fn next_union_key(
    bodies: &[Arc<[DatasetResourceKey]>],
    cursors: &mut [usize],
) -> Option<DatasetResourceKey> {
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
    previous: &[DatasetResourceKey],
    next: &[DatasetResourceKey],
    reservations: &mut PreparedAllocationReservations<'_>,
    cancelled: &mut impl FnMut() -> bool,
) -> anyhow::Result<Option<Arc<[DatasetResourceKey]>>> {
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

fn rebind_timepoint(key: DatasetResourceKey, timepoint: TimeIndex) -> DatasetResourceKey {
    DatasetResourceKey::new(
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

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    use glam::{DMat4, DQuat, DVec3};
    use mirante4d_dataset::{
        CpuByteLease, CpuLedgerCategory, CpuLedgerError, DatasetLayer, DatasetResourceIdentity,
        DatasetSourceId, ResourceRegion, ResourceValidity, ScientificIdentityStatus,
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

    fn test_cpu_ledger() -> Arc<dyn CpuByteLedger> {
        Arc::new(TestCpuLedger)
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

    fn unaccounted_requirement_handle(
        requirements: Arc<[DatasetResourceKey]>,
    ) -> RetainedRequirementHandle {
        RetainedRequirementHandle {
            requirements,
            charge: None,
        }
    }

    #[test]
    fn latest_slot_exposes_only_the_current_generation_and_never_visits_candidates_on_caller() {
        let (catalog, view) = fixture(Shape3D::new(128, 128, 128).unwrap());
        let mut planner = CameraDemandPlanner::new().unwrap();
        let first = planner.submit(request(Arc::clone(&catalog), view.clone(), 4_096));
        let first = first.unwrap();
        let latest = planner
            .submit(request(Arc::clone(&catalog), view, 4_096))
            .unwrap();
        assert!(latest > first);

        let result = wait_for_result(&mut planner, Duration::from_secs(5));
        assert_eq!(result.generation, latest);
        let planning = result.outcome.unwrap();
        assert!(
            !planning
                .current_3d
                .as_ref()
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
        assert_eq!(diagnostics.renderer_static_preparations, 1);
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
        assert_eq!(diagnostics.renderer_static_preparations, 0);
        assert_eq!(diagnostics.ui_thread_candidates_visited, 0);
    }

    #[test]
    fn completed_plan_remains_outstanding_until_result_publication() {
        let (catalog, view) = fixture(Shape3D::new(64, 64, 64).unwrap());
        let mut planner = CameraDemandPlanner::new().unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *planner
            .shared
            .before_result_publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(TestPublicationBarrier {
            entered: entered_tx,
            release: release_rx,
        });
        planner.submit(request(catalog, view, 4_096)).unwrap();

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
        planner.submit(request(catalog, view, 4_096)).unwrap();
        planner.invalidate().unwrap();

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
            Arc::clone(&catalog),
            test_cpu_ledger(),
            view,
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
        planner.submit(request).unwrap();

        let result = wait_for_result(&mut planner, Duration::from_secs(5));
        let planning = result.outcome.unwrap();
        assert!(planning.current_3d.is_none());
        assert_eq!(planning.cross_sections.len(), 1);
        let cross = &planning.cross_sections[0];
        assert_eq!(cross.panel, PanelId::Xy);
        assert!(!cross.plan.requirements.canonical().is_empty());
        assert!(
            cross
                .render_requirements
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
    fn full_envelope_union_delta_is_prepared_on_worker_with_two_removals() {
        let identity = DatasetResourceIdentity::Unverified(DatasetSourceId::new(77));
        let key = |x| {
            DatasetResourceKey::new(
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
            u64::try_from(expected.len() * std::mem::size_of::<DatasetResourceKey>()).unwrap()
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
        planner
            .submit(volume_request_with_ledger(
                Arc::clone(&catalog),
                Arc::clone(&ledger),
                view.clone(),
                PresentationViewport::new(64.0, 64.0).unwrap(),
                RenderExtent::new(64, 64).unwrap(),
                DatasetDemandPlanLimits::new(4_096, 65_536, u64::MAX),
            ))
            .unwrap();
        let first = wait_for_result(&mut planner, Duration::from_secs(5))
            .outcome
            .unwrap();
        let installed_volume_body = first
            .current_3d
            .as_ref()
            .unwrap()
            .current
            .requirements
            .body()
            .clone();
        drop(first);
        let old_scope_bytes = live_bytes.load(Ordering::Acquire);
        assert!(old_scope_bytes > 0, "the installed body retains its charge");

        planner
            .submit(CameraDemandRequest::new(
                Arc::clone(&catalog),
                Arc::clone(&ledger),
                view,
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
            ))
            .unwrap();
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
        let old_requirement = DatasetResourceKey::new(
            catalog.resource_identity(),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            mirante4d_domain::ScaleLevel::BASE,
            ResourceRegion::new([9_999, 0, 0], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
        );
        let old_requirements: Arc<[DatasetResourceKey]> = Arc::from([old_requirement]);
        let old_bytes = u64::try_from(std::mem::size_of::<DatasetResourceKey>()).unwrap();
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
            Arc::clone(&catalog),
            Arc::clone(&ledger),
            view,
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
        planner.submit(request).unwrap();
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
    fn tiny_cross_panel_does_not_pin_large_current_or_create_false_capacity() {
        let live_bytes = Arc::new(AtomicU64::new(0));
        let capacity_bytes = Arc::new(AtomicU64::new(u64::MAX));
        let ledger: Arc<dyn CpuByteLedger> = Arc::new(TrackingCpuLedger {
            live_bytes: Arc::clone(&live_bytes),
            capacity_bytes: Arc::clone(&capacity_bytes),
        });
        let (catalog, view) = fixture(Shape3D::new(512, 512, 512).unwrap());
        let request = CameraDemandRequest::new(
            catalog,
            Arc::clone(&ledger),
            view,
            Some(Current3dDemandRequest::new(
                PresentationViewport::new(320.0, 320.0).unwrap(),
                RenderExtent::new(320, 320).unwrap(),
                DatasetDemandPlanLimits::new(4_096, 65_536, u64::MAX),
                false,
                None,
                false,
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
        let prepared = plan_visible_demand(request, || false, || {})
            .unwrap()
            .unwrap();
        let current_charge = prepared
            .current_3d
            .as_ref()
            .unwrap()
            .current
            .requirements
            .body()
            .charged_bytes()
            .unwrap();
        let cross_body = prepared.cross_sections[0].plan.requirements.body().clone();
        let cross_charge = cross_body.charged_bytes().unwrap();
        assert!(current_charge > cross_charge);

        drop(prepared);
        assert_eq!(
            live_bytes.load(Ordering::Acquire),
            cross_charge,
            "the surviving tiny panel must own only its panel cohort"
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
                Arc::clone(&catalog),
                Arc::clone(&ledger),
                view.clone(),
                Some(Current3dDemandRequest::new(
                    PresentationViewport::new(128.0, 128.0).unwrap(),
                    RenderExtent::new(128, 128).unwrap(),
                    limits,
                    false,
                    None,
                    false,
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
            || {},
        )
        .unwrap()
        .unwrap();
        let first_3d = first.current_3d.as_ref().unwrap();
        let first_current = first_3d.current.requirements.body().clone();
        let first_refinement = first_3d.refinement.requirements.body().clone();
        let first_playback = first_3d.playback.requirements.body().clone();
        let first_3d_charge = first_current.charged_bytes().unwrap();
        let first_cross = first.cross_sections[0].plan.requirements.body().clone();
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
                Arc::clone(&catalog),
                Arc::clone(&ledger),
                view.clone(),
                Some(Current3dDemandRequest::new(
                    PresentationViewport::new(96.0, 96.0).unwrap(),
                    RenderExtent::new(96, 96).unwrap(),
                    limits,
                    false,
                    None,
                    false,
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
            || {},
        )
        .unwrap()
        .unwrap();
        let second_3d = second.current_3d.as_ref().unwrap();
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
                catalog,
                Arc::clone(&ledger),
                view,
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
            || {},
        )
        .unwrap()
        .unwrap();
        let third_cross = third.cross_sections[0].plan.requirements.body().clone();
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
            let generation = planner
                .submit(volume_request(
                    Arc::clone(&catalog),
                    view.clone(),
                    presentation,
                    viewport,
                    limits,
                ))
                .unwrap();
            enqueue_samples.push(enqueue_started.elapsed());
            let result = wait_for_result(&mut planner, Duration::from_secs(30));
            worker_samples.push(result.planning_duration);
            let planning = result.outcome.unwrap();
            assert_eq!(result.generation, generation);
            assert_eq!(
                planning
                    .current_3d
                    .as_ref()
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
                .current_3d
                .as_ref()
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
        catalog: Arc<DatasetCatalog>,
        view: ViewState,
        max_candidates: usize,
    ) -> CameraDemandRequest {
        volume_request(
            catalog,
            view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(max_candidates, 65_536, u64::MAX),
        )
    }

    fn volume_request(
        catalog: Arc<DatasetCatalog>,
        view: ViewState,
        presentation: PresentationViewport,
        viewport: RenderExtent,
        limits: DatasetDemandPlanLimits,
    ) -> CameraDemandRequest {
        volume_request_with_ledger(
            catalog,
            test_cpu_ledger(),
            view,
            presentation,
            viewport,
            limits,
        )
    }

    fn volume_request_with_ledger(
        catalog: Arc<DatasetCatalog>,
        cpu_ledger: Arc<dyn CpuByteLedger>,
        view: ViewState,
        presentation: PresentationViewport,
        viewport: RenderExtent,
        limits: DatasetDemandPlanLimits,
    ) -> CameraDemandRequest {
        CameraDemandRequest::new(
            catalog,
            cpu_ledger,
            view,
            Some(Current3dDemandRequest::new(
                presentation,
                viewport,
                limits,
                false,
                None,
                false,
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
                ScientificIdentityStatus::Unverified(DatasetSourceId::new(1)),
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
