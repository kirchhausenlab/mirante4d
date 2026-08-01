//! Sole composition-side owner of unified dataset demand.
//!
//! It owns bounded ticket correlation, cancellation generations, and the
//! runtime-issued lease handles retained for current interactive demand.

use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use mirante4d_application::RenderIntentRevision;
use mirante4d_dataset::{
    BrickKey, CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError,
    DatasetResourceIdentity, ResourceLease,
};
use mirante4d_dataset_runtime::{
    AccountedCpuLease, CancellationGeneration, DatasetRuntime, DatasetRuntimeConfig,
    DatasetRuntimeDiagnostics, RequestPriority, RequestTicket, ResourceRequest, RuntimeFault,
    RuntimeFaultCode, RuntimeOutcome, RuntimeRequestId,
};
use mirante4d_domain::ScaleLevel;
use mirante4d_render_api::PreparedResourceBody;
use mirante4d_storage::{LocalDatasetSource, LocalDatasetSourceDiagnostics};

use crate::{
    camera_demand_cache::{
        CameraDemandDiagnostics, CameraDemandPlanner, CameraDemandPlannerError,
        CameraDemandRequest, CameraDemandResult,
    },
    dataset_demand_plan::{
        PreparedDatasetDemandPlan, PreparedDemandRequirements, PreparedProgressiveDatasetDemandPlan,
    },
    retained_leases::{RetainedLeaseError, RetainedLeases, RetainedRequirementHandle},
};

#[cfg(test)]
use crate::dataset_demand_plan::{DatasetDemandPlan, PreparedAllocationReservations};

pub(crate) const SCOPE_CURRENT_3D: u64 = 1;
pub(crate) const SCOPE_CROSS_SECTION_XY: u64 = 2;
pub(crate) const SCOPE_CROSS_SECTION_XZ: u64 = 3;
pub(crate) const SCOPE_CROSS_SECTION_YZ: u64 = 4;
pub(crate) const SCOPE_PLAYBACK: u64 = 5;
pub(crate) const SCOPE_ANALYSIS: u64 = 6;
pub(crate) const SCOPE_CURRENT_3D_REFINEMENT: u64 = 7;

const DATASET_DEMAND_SCOPES: [u64; 6] = [
    SCOPE_CURRENT_3D,
    SCOPE_CURRENT_3D_REFINEMENT,
    SCOPE_CROSS_SECTION_XY,
    SCOPE_CROSS_SECTION_XZ,
    SCOPE_CROSS_SECTION_YZ,
    SCOPE_PLAYBACK,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveVisibleDemand {
    revision: RenderIntentRevision,
    scope: u64,
    installed: bool,
}

/// A small deterministic sample per current layer remains CPU-authoritative
/// for histogram/readout consumers. Full 3D display cohorts stream through
/// CPU residency into the larger GPU arena.
const HISTOGRAM_CPU_RESOURCES_PER_LAYER: usize = 8;

fn histogram_quota_complete(
    histogram_by_layer: &BTreeMap<mirante4d_domain::LogicalLayerKey, Vec<BrickKey>>,
    selected_layers: &BTreeSet<mirante4d_domain::LogicalLayerKey>,
) -> bool {
    selected_layers.iter().all(|layer| {
        histogram_by_layer
            .get(layer)
            .is_some_and(|resources| resources.len() >= HISTOGRAM_CPU_RESOURCES_PER_LAYER)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PendingKey {
    scope: u64,
    resource: BrickKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingRequest {
    key: PendingKey,
    retry_cursor: Option<usize>,
}

#[derive(Default)]
pub(crate) struct ScopeReconciliationTargets {
    requirements_by_scope: BTreeMap<u64, Arc<[BrickKey]>>,
    rewind_cancelled_scopes: BTreeSet<u64>,
}

impl ScopeReconciliationTargets {
    pub(crate) fn replace(&mut self, scope: u64, requirements: &PreparedDemandRequirements) {
        self.requirements_by_scope
            .insert(scope, Arc::clone(requirements.canonical()));
    }

    pub(crate) fn remove(&mut self, scope: u64) {
        self.requirements_by_scope.insert(scope, Arc::default());
    }

    fn replace_handle(&mut self, scope: u64, requirements: Arc<[BrickKey]>) {
        self.requirements_by_scope.insert(scope, requirements);
    }

    fn rewind_cancelled_scope(&mut self, scope: u64) {
        self.rewind_cancelled_scopes.insert(scope);
    }
}

/// Pure, exact cancellation plan for every scope in one visible-demand
/// transaction. The ticket array is charged before allocation and is bounded
/// by the runtime request queue. No dispatcher or runtime state changes until
/// `commit_prepared_scope_reconciliation` invokes atomic `cancel_many`.
pub(crate) struct PreparedScopeReconciliation {
    targets: ScopeReconciliationTargets,
    cancellation_tickets: Box<[RequestTicket]>,
    admission_cursor_rewinds: BTreeMap<u64, usize>,
    _ticket_charge: Option<Box<dyn CpuByteLease>>,
}

/// One bounded poll owner. No other application service may call
/// `DatasetRuntime::poll` directly.
pub(crate) struct DatasetRequestDispatcher {
    runtime: Arc<dyn DatasetRuntime>,
    generations: BTreeMap<u64, CancellationGeneration>,
    pending_by_id: HashMap<RuntimeRequestId, PendingRequest>,
    pending_by_key: HashMap<PendingKey, RequestTicket>,
    failed_by_scope: BTreeMap<u64, BTreeMap<BrickKey, RuntimeFault>>,
    admission_blocked: bool,
    last_fault: Option<RuntimeFault>,
    #[cfg(test)]
    submitted_requests: Vec<(u64, BrickKey, RequestPriority)>,
    #[cfg(test)]
    promoted_requests: Vec<(u64, BrickKey, RequestPriority)>,
}

/// Bounded demand and retained-resource state for one opened source.
pub(crate) struct DatasetDemandState {
    dispatcher: DatasetRequestDispatcher,
    visible_demand_planner: CameraDemandPlanner,
    latest_visible_intent_revision: RenderIntentRevision,
    active_visible_demand: Option<ActiveVisibleDemand>,
    retained_leases: RetainedLeases,
    cpu_ledger: Arc<dyn CpuByteLedger>,
    resource_identity: DatasetResourceIdentity,
    selected_path: PathBuf,
    local_source: Option<Arc<LocalDatasetSource>>,
    requirements_by_scope: BTreeMap<u64, Arc<[BrickKey]>>,
    gpu_priority_order_by_scope: BTreeMap<u64, Arc<[BrickKey]>>,
    /// Required current-view prefix within each contribution-ranked body.
    /// The suffix is admitted only by the final speculative-prefetch pass.
    required_prefix_len_by_scope: BTreeMap<u64, usize>,
    prepared_body_by_scope: BTreeMap<u64, PreparedResourceBody>,
    layer_scales_by_scope: BTreeMap<u64, BTreeMap<mirante4d_domain::LogicalLayerKey, ScaleLevel>>,
    admission_cursor_by_scope: BTreeMap<u64, usize>,
    readiness_by_scope: BTreeMap<u64, ScopeReadiness>,
    /// Ranked prefix length of one complete future playback timepoint. This is
    /// distinct from the longer startup-runway prefix retained by the scope.
    playback_resources_per_timepoint: usize,
    retained_requirements_dirty: bool,
    linked_cpu_authoritative_keys: HashSet<BrickKey>,
    linked_cpu_authoritative_keys_dirty: bool,
    histogram_cpu_authoritative_keys: HashSet<BrickKey>,
    histogram_cpu_authoritative_keys_dirty: bool,
    histogram_requirements_by_layer: BTreeMap<mirante4d_domain::LogicalLayerKey, Arc<[BrickKey]>>,
    histogram_generation_by_layer: BTreeMap<mirante4d_domain::LogicalLayerKey, u64>,
    /// CPU-authority removals that may now be released immediately when the
    /// exact payload is already resident through another presentation target.
    released_cpu_authority_candidates: HashSet<BrickKey>,
    current_scale: ScaleLevel,
    current_ideal_scale: ScaleLevel,
    current_ideal_layer_scales: BTreeMap<mirante4d_domain::LogicalLayerKey, ScaleLevel>,
    current_capacity_constrained: bool,
    current_covers_full_volume: bool,
    current_playback_downshifted: bool,
    four_panel: bool,
    last_plan_error: Option<String>,
    preserve_presentation_for_deferred_refill_fault: bool,
    staged_current_plan: Option<PreparedDatasetDemandPlan>,
    holding_previous_presentation: bool,
    source_quarantined: bool,
    #[cfg(test)]
    admission_requirement_visits: u64,
    #[cfg(test)]
    readiness_requirement_visits: Cell<u64>,
    #[cfg(test)]
    staged_promotion_commits: u64,
    #[cfg(test)]
    staged_promotion_requirement_visits: u64,
    #[cfg(test)]
    linked_cpu_authority_requirement_visits: u64,
    #[cfg(test)]
    histogram_cpu_authority_requirement_visits: u64,
    #[cfg(test)]
    prepared_scope_commits: u64,
    #[cfg(test)]
    prepared_scope_commit_requirement_visits: u64,
}

#[derive(Debug, Default)]
struct ScopeReadiness {
    /// Monotone positions into the contribution-ranked body. The visible
    /// readiness boundary and the full camera-guard body are independent
    /// proofs; neither stores renderer residency membership.
    required_cursor: Cell<usize>,
    all_resources_cursor: Cell<usize>,
}

#[derive(Default)]
pub(crate) struct InstalledLeaseChanges {
    scopes: BTreeSet<u64>,
    leases: Vec<Arc<dyn ResourceLease>>,
}

impl InstalledLeaseChanges {
    pub(crate) fn in_scope(&self, scope: u64) -> bool {
        self.scopes.contains(&scope)
    }

    pub(crate) fn any(&self) -> bool {
        !self.scopes.is_empty()
    }

    pub(crate) fn leases(&self) -> &[Arc<dyn ResourceLease>] {
        &self.leases
    }
}

pub(crate) enum RendererEvictionDisposition {
    Reoffer(Arc<dyn ResourceLease>),
    Submitted,
    NoLongerDemanded,
    Deferred,
}

impl DatasetRequestDispatcher {
    pub(crate) fn new(runtime: Arc<dyn DatasetRuntime>) -> Self {
        Self {
            runtime,
            generations: BTreeMap::new(),
            pending_by_id: HashMap::new(),
            pending_by_key: HashMap::new(),
            failed_by_scope: BTreeMap::new(),
            admission_blocked: false,
            last_fault: None,
            #[cfg(test)]
            submitted_requests: Vec::new(),
            #[cfg(test)]
            promoted_requests: Vec::new(),
        }
    }

    pub(crate) fn generation(&mut self, scope: u64) -> CancellationGeneration {
        *self
            .generations
            .entry(scope)
            .or_insert_with(|| CancellationGeneration::for_scope(scope, 0))
    }

    /// Cancels only older waiters in this scope. Shared work remains live when
    /// another scope still needs the same semantic resource.
    pub(crate) fn advance_scope(
        &mut self,
        scope: u64,
    ) -> Result<CancellationGeneration, RuntimeFault> {
        let current = self.generation(scope);
        let next = current.checked_next().map_err(RuntimeFault::new)?;
        self.runtime.cancel_before(next)?;
        self.generations.insert(scope, next);
        self.pending_by_key.retain(|key, _| key.scope != scope);
        // Keep request IDs until their cancellation completions are drained.
        // Otherwise idle polling can stop while the runtime still owns
        // charged request/completion records for the retired generation.
        self.failed_by_scope.remove(&scope);
        Ok(next)
    }

    /// Submits at most one waiter for a `(scope, resource)` pair. Runtime-level
    /// deduplication still merges identical resources demanded by other scopes.
    pub(crate) fn submit_if_missing(
        &mut self,
        scope: u64,
        resource: BrickKey,
        priority: RequestPriority,
        already_resident: bool,
        retry_cursor: Option<usize>,
    ) -> Result<Option<RequestTicket>, RuntimeFault> {
        let pending_key = PendingKey { scope, resource };
        if already_resident
            || self
                .failed_by_scope
                .get(&scope)
                .is_some_and(|failed| failed.contains_key(&resource))
        {
            return Ok(None);
        }
        if let Some(ticket) = self.pending_by_key.get(&pending_key).copied() {
            if self.runtime.promote_priority(ticket, priority)? {
                #[cfg(test)]
                self.promoted_requests.push((scope, resource, priority));
            }
            return Ok(None);
        }
        let generation = self.generation(scope);
        let ticket = self
            .runtime
            .submit(ResourceRequest::new(resource, priority, generation))?;
        #[cfg(test)]
        self.submitted_requests.push((scope, resource, priority));
        self.pending_by_id.insert(
            ticket.id(),
            PendingRequest {
                key: pending_key,
                retry_cursor,
            },
        );
        self.pending_by_key.insert(pending_key, ticket);
        Ok(Some(ticket))
    }

    pub(crate) fn drain(
        &mut self,
        maximum: usize,
        mut accept: impl FnMut(RequestTicket, RuntimeOutcome),
    ) -> Result<usize, RuntimeFault> {
        self.drain_with_retry_cursor(maximum, |ticket, outcome, _| accept(ticket, outcome))
    }

    fn drain_with_retry_cursor(
        &mut self,
        maximum: usize,
        mut accept: impl FnMut(RequestTicket, RuntimeOutcome, Option<usize>),
    ) -> Result<usize, RuntimeFault> {
        let completions = self.runtime.poll(maximum)?;
        let count = completions.len();
        for completion in completions {
            let ticket = completion.ticket();
            let pending_request = self.pending_by_id.remove(&ticket.id());
            let waiter_was_live = pending_request.is_some_and(|request| {
                self.pending_by_key
                    .get(&request.key)
                    .is_some_and(|pending| pending.id() == ticket.id())
            });
            if waiter_was_live {
                self.pending_by_key.remove(
                    &pending_request
                        .expect("a live waiter has its pending correlation key")
                        .key,
                );
            }
            let current = self.generation(ticket.generation().scope());
            if !waiter_was_live || !ticket.is_current(current).map_err(RuntimeFault::new)? {
                // Individual retirement keeps the scope generation stable so
                // overlapping waiters and shared decode survive. Its own
                // eventual completion is nevertheless stale and must not
                // install a lease or poison a reintroduced key with a raced
                // terminal failure.
                continue;
            }
            let outcome = completion.outcome().clone();
            match &outcome {
                RuntimeOutcome::Ready(_) | RuntimeOutcome::Cancelled => {}
                RuntimeOutcome::Failed(fault) => {
                    if runtime_failure_is_sticky(fault.code()) {
                        self.failed_by_scope
                            .entry(ticket.generation().scope())
                            .or_default()
                            .insert(ticket.resource(), fault.clone());
                    } else if runtime_failure_needs_capacity_retry(fault.code()) {
                        // A source-side staging reservation can lose a race
                        // after runtime admission. Keep the key retryable and
                        // keep bounded UI polling alive until a later
                        // submission can acquire the released capacity.
                        self.admission_blocked = true;
                    }
                    if ticket.generation().scope() != SCOPE_ANALYSIS {
                        self.last_fault = Some(fault.clone());
                    }
                }
            }
            accept(
                ticket,
                outcome,
                pending_request.and_then(|request| request.retry_cursor),
            );
        }
        Ok(count)
    }

    pub(crate) fn try_acquire_analysis_bytes(
        &self,
        bytes: u64,
    ) -> Result<AccountedCpuLease, RuntimeFault> {
        self.runtime.try_acquire_analysis_bytes(bytes)
    }

    pub(crate) fn diagnostics(&self) -> Result<DatasetRuntimeDiagnostics, RuntimeFault> {
        self.runtime.diagnostics()
    }

    /// Immutable runtime limits are configuration, not diagnostics. Reading
    /// them must never scan live jobs on the demand-planning hot path.
    pub(crate) fn config(&self) -> DatasetRuntimeConfig {
        self.runtime.config()
    }

    pub(crate) fn has_pending_work(&self) -> bool {
        self.admission_blocked || !self.pending_by_id.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn pending_ticket(&self, scope: u64, resource: BrickKey) -> Option<RequestTicket> {
        self.pending_by_key
            .get(&PendingKey { scope, resource })
            .copied()
    }

    #[cfg(test)]
    pub(crate) fn submitted_request_records(&self) -> &[(u64, BrickKey, RequestPriority)] {
        &self.submitted_requests
    }

    pub(crate) const fn admission_blocked(&self) -> bool {
        self.admission_blocked
    }

    fn begin_submission_pass(&mut self) {
        self.admission_blocked = false;
    }

    fn mark_admission_blocked(&mut self) {
        self.admission_blocked = true;
    }

    fn has_submission_capacity(&self) -> bool {
        let config = self.runtime.config();
        let maximum = config
            .request_queue_limit()
            .min(config.completion_queue_limit());
        self.pending_by_id.len() < maximum
    }

    pub(crate) fn last_fault(&self) -> Option<&RuntimeFault> {
        self.last_fault.as_ref()
    }

    pub(crate) fn take_last_fault(&mut self) -> Option<RuntimeFault> {
        self.last_fault.take()
    }

    pub(crate) fn scope_failure(&self, scope: u64) -> Option<&RuntimeFault> {
        self.failed_by_scope
            .get(&scope)
            .and_then(|failed| failed.values().next())
    }

    pub(crate) fn request_shutdown(&self) -> Result<(), RuntimeFault> {
        self.runtime.request_shutdown()
    }
}

impl DatasetDemandState {
    pub(crate) fn new_local(
        runtime: Arc<dyn DatasetRuntime>,
        cpu_ledger: Arc<dyn CpuByteLedger>,
        resource_identity: DatasetResourceIdentity,
        selected_path: PathBuf,
        local_source: Arc<LocalDatasetSource>,
    ) -> Result<Self, CameraDemandPlannerError> {
        Self::new_with_local_source(
            runtime,
            cpu_ledger,
            resource_identity,
            selected_path,
            Some(local_source),
        )
    }

    fn new_with_local_source(
        runtime: Arc<dyn DatasetRuntime>,
        cpu_ledger: Arc<dyn CpuByteLedger>,
        resource_identity: DatasetResourceIdentity,
        selected_path: PathBuf,
        local_source: Option<Arc<LocalDatasetSource>>,
    ) -> Result<Self, CameraDemandPlannerError> {
        Ok(Self {
            dispatcher: DatasetRequestDispatcher::new(runtime),
            visible_demand_planner: CameraDemandPlanner::new()?,
            latest_visible_intent_revision: RenderIntentRevision::initial(),
            active_visible_demand: None,
            retained_leases: RetainedLeases::new(),
            cpu_ledger,
            resource_identity,
            selected_path,
            local_source,
            requirements_by_scope: BTreeMap::new(),
            gpu_priority_order_by_scope: BTreeMap::new(),
            required_prefix_len_by_scope: BTreeMap::new(),
            prepared_body_by_scope: BTreeMap::new(),
            layer_scales_by_scope: BTreeMap::new(),
            admission_cursor_by_scope: BTreeMap::new(),
            readiness_by_scope: BTreeMap::new(),
            playback_resources_per_timepoint: 0,
            retained_requirements_dirty: true,
            linked_cpu_authoritative_keys: HashSet::new(),
            linked_cpu_authoritative_keys_dirty: true,
            histogram_cpu_authoritative_keys: HashSet::new(),
            histogram_cpu_authoritative_keys_dirty: true,
            histogram_requirements_by_layer: BTreeMap::new(),
            histogram_generation_by_layer: BTreeMap::new(),
            released_cpu_authority_candidates: HashSet::new(),
            current_scale: ScaleLevel::BASE,
            current_ideal_scale: ScaleLevel::BASE,
            current_ideal_layer_scales: BTreeMap::new(),
            current_capacity_constrained: false,
            current_covers_full_volume: false,
            current_playback_downshifted: false,
            four_panel: false,
            last_plan_error: None,
            preserve_presentation_for_deferred_refill_fault: false,
            staged_current_plan: None,
            holding_previous_presentation: false,
            source_quarantined: false,
            #[cfg(test)]
            admission_requirement_visits: 0,
            #[cfg(test)]
            readiness_requirement_visits: Cell::new(0),
            #[cfg(test)]
            staged_promotion_commits: 0,
            #[cfg(test)]
            staged_promotion_requirement_visits: 0,
            #[cfg(test)]
            linked_cpu_authority_requirement_visits: 0,
            #[cfg(test)]
            histogram_cpu_authority_requirement_visits: 0,
            #[cfg(test)]
            prepared_scope_commits: 0,
            #[cfg(test)]
            prepared_scope_commit_requirement_visits: 0,
        })
    }

    pub(crate) fn selected_path(&self) -> &Path {
        &self.selected_path
    }

    pub(crate) fn dispatcher(&self) -> &DatasetRequestDispatcher {
        &self.dispatcher
    }

    /// Observes the mailbox-owned revision and its currently interactive
    /// scope. Advancing the revision invalidates planner work before a
    /// replacement can publish; the data plane copies but never allocates the
    /// revision.
    pub(crate) fn observe_visible_intent(
        &mut self,
        revision: RenderIntentRevision,
        active_scope: Option<u64>,
    ) -> bool {
        if !self.can_observe_visible_intent(revision, active_scope) {
            return false;
        }
        self.commit_preflighted_visible_intent(revision, active_scope);
        true
    }

    pub(crate) fn can_observe_visible_intent(
        &self,
        revision: RenderIntentRevision,
        active_scope: Option<u64>,
    ) -> bool {
        revision >= self.latest_visible_intent_revision
            && active_scope.is_none_or(|scope| DATASET_DEMAND_SCOPES.contains(&scope))
    }

    /// Infallible mutation half of visible-intent observation. Callers that
    /// coordinate this with other scope state preflight every participant
    /// before entering this commit.
    pub(crate) fn commit_preflighted_visible_intent(
        &mut self,
        revision: RenderIntentRevision,
        active_scope: Option<u64>,
    ) {
        debug_assert!(self.can_observe_visible_intent(revision, active_scope));
        if revision > self.latest_visible_intent_revision {
            self.visible_demand_planner.invalidate(revision);
            self.latest_visible_intent_revision = revision;
        }
        let previous_active = self.active_visible_demand;
        self.active_visible_demand = active_scope.map(|scope| {
            let installed = previous_active.is_some_and(|active| {
                active.revision == revision && active.scope == scope && active.installed
            });
            ActiveVisibleDemand {
                revision,
                scope,
                installed,
            }
        });
    }

    pub(crate) fn can_install_visible_intent(
        &self,
        revision: RenderIntentRevision,
        scope: u64,
    ) -> bool {
        self.can_observe_visible_intent(revision, Some(scope)) && self.scope_is_installed(scope)
    }

    /// Atomically observes and marks one already-installed interactive scope
    /// active after the app has preflighted the matching signature/body.
    pub(crate) fn commit_preflighted_installed_visible_intent(
        &mut self,
        revision: RenderIntentRevision,
        scope: u64,
    ) {
        debug_assert!(self.can_install_visible_intent(revision, scope));
        self.commit_preflighted_visible_intent(revision, Some(scope));
        self.active_visible_demand
            .as_mut()
            .expect("the preflighted active scope was just installed")
            .installed = true;
    }

    /// Enables foreground admission only after the app has proved that the
    /// active mailbox revision's panel signature owns the installed scope
    /// body. A newer sample therefore cannot promote its predecessor's pending
    /// keys while replacement planning is still in flight.
    pub(crate) fn activate_installed_visible_intent(
        &mut self,
        revision: RenderIntentRevision,
        scope: u64,
    ) -> bool {
        let Some(active) = self.active_visible_demand.as_mut() else {
            return false;
        };
        if revision != self.latest_visible_intent_revision
            || active.revision != revision
            || active.scope != scope
        {
            return false;
        }
        active.installed = true;
        true
    }

    pub(crate) fn submit_visible_demand_plan(&mut self, request: CameraDemandRequest) -> bool {
        if request.revision() != self.latest_visible_intent_revision {
            return false;
        }
        self.visible_demand_planner.submit(request)
    }

    pub(crate) fn take_visible_demand_plan_result(&mut self) -> Option<CameraDemandResult> {
        self.visible_demand_planner
            .take_result()
            .filter(|result| result.revision == self.latest_visible_intent_revision)
    }

    pub(crate) fn visible_demand_plan_outstanding(&self) -> bool {
        self.visible_demand_planner.has_outstanding_request()
    }

    pub(crate) fn invalidate_visible_demand_plan(&mut self, revision: RenderIntentRevision) {
        if revision < self.latest_visible_intent_revision {
            return;
        }
        self.latest_visible_intent_revision = revision;
        self.active_visible_demand = None;
        self.visible_demand_planner.invalidate(revision);
    }

    pub(crate) fn visible_demand_diagnostics(&self) -> CameraDemandDiagnostics {
        self.visible_demand_planner.diagnostics()
    }

    pub(crate) fn record_contained_visible_demand_reuse(&self, reusable_candidates: usize) {
        self.visible_demand_planner
            .record_contained_reuse(reusable_candidates);
    }

    #[cfg(test)]
    pub(crate) fn block_next_visible_demand_result_publication(
        &self,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        self.visible_demand_planner.block_next_result_publication()
    }

    pub(crate) fn cpu_ledger_arc(&self) -> Arc<dyn CpuByteLedger> {
        Arc::clone(&self.cpu_ledger)
    }

    pub(crate) fn cpu_capacity_epoch(&self) -> u64 {
        self.cpu_ledger.capacity_epoch()
    }

    pub(crate) fn local_source_diagnostics(&self) -> Option<LocalDatasetSourceDiagnostics> {
        self.local_source
            .as_ref()
            .map(|source| source.diagnostics())
    }

    pub(crate) const fn resource_identity(&self) -> DatasetResourceIdentity {
        self.resource_identity
    }

    pub(crate) const fn source_quarantined(&self) -> bool {
        self.source_quarantined
    }

    pub(crate) fn dispatcher_mut(&mut self) -> &mut DatasetRequestDispatcher {
        &mut self.dispatcher
    }

    pub(crate) fn retained_leases(&self) -> &RetainedLeases {
        &self.retained_leases
    }

    #[cfg(test)]
    pub(crate) fn retire_cpu_payload_for_foreground_test(&mut self, key: BrickKey) -> bool {
        self.retained_leases.retire_payload_handle(key)
    }

    fn refresh_cpu_authoritative_keys(&mut self) {
        if !self.linked_cpu_authoritative_keys_dirty && !self.histogram_cpu_authoritative_keys_dirty
        {
            return;
        }

        if self.linked_cpu_authoritative_keys_dirty {
            let mut next_linked = HashSet::new();
            for scope in [
                SCOPE_CROSS_SECTION_XY,
                SCOPE_CROSS_SECTION_XZ,
                SCOPE_CROSS_SECTION_YZ,
                SCOPE_PLAYBACK,
                SCOPE_ANALYSIS,
            ] {
                if let Some(requirements) = self.requirements_by_scope.get(&scope) {
                    #[cfg(test)]
                    {
                        self.linked_cpu_authority_requirement_visits = self
                            .linked_cpu_authority_requirement_visits
                            .saturating_add(requirements.len() as u64);
                    }
                    next_linked.extend(requirements.iter().copied());
                }
            }
            let released = self
                .linked_cpu_authoritative_keys
                .difference(&next_linked)
                .copied()
                .filter(|key| !self.histogram_cpu_authoritative_keys.contains(key))
                .filter(|key| self.current_demands_key(*key))
                .collect::<Vec<_>>();
            self.released_cpu_authority_candidates.extend(released);
            self.linked_cpu_authoritative_keys = next_linked;
            self.linked_cpu_authoritative_keys_dirty = false;
        }

        if self.histogram_cpu_authoritative_keys_dirty {
            let selected_layers = [SCOPE_CURRENT_3D, SCOPE_CURRENT_3D_REFINEMENT]
                .into_iter()
                .filter_map(|scope| self.layer_scales_by_scope.get(&scope))
                .flat_map(BTreeMap::keys)
                .copied()
                .collect::<BTreeSet<_>>();
            let mut next_histogram = HashSet::with_capacity(
                selected_layers
                    .len()
                    .saturating_mul(HISTOGRAM_CPU_RESOURCES_PER_LAYER),
            );
            let mut histogram_by_layer =
                BTreeMap::<mirante4d_domain::LogicalLayerKey, Vec<BrickKey>>::new();
            for scope in [SCOPE_CURRENT_3D, SCOPE_CURRENT_3D_REFINEMENT] {
                if histogram_quota_complete(&histogram_by_layer, &selected_layers) {
                    break;
                }
                if let Some(requirements) = self.gpu_priority_order_by_scope.get(&scope) {
                    for key in requirements.iter().copied() {
                        #[cfg(test)]
                        {
                            self.histogram_cpu_authority_requirement_visits = self
                                .histogram_cpu_authority_requirement_visits
                                .saturating_add(1);
                        }
                        let histogram = histogram_by_layer.entry(key.layer()).or_default();
                        if histogram.len() < HISTOGRAM_CPU_RESOURCES_PER_LAYER
                            && !histogram.contains(&key)
                        {
                            next_histogram.insert(key);
                            histogram.push(key);
                        }
                        if histogram_quota_complete(&histogram_by_layer, &selected_layers) {
                            break;
                        }
                    }
                }
            }
            let changed_layers = self
                .histogram_requirements_by_layer
                .keys()
                .chain(histogram_by_layer.keys())
                .copied()
                .collect::<BTreeSet<_>>();
            for layer in changed_layers {
                let current = self
                    .histogram_requirements_by_layer
                    .get(&layer)
                    .map(|requirements| requirements.as_ref())
                    .unwrap_or(&[]);
                let next = histogram_by_layer
                    .get(&layer)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                if current != next {
                    let generation = self.histogram_generation_by_layer.entry(layer).or_default();
                    *generation = generation.saturating_add(1);
                }
            }
            self.histogram_requirements_by_layer = histogram_by_layer
                .into_iter()
                .map(|(layer, requirements)| (layer, requirements.into()))
                .collect();
            let released = self
                .histogram_cpu_authoritative_keys
                .difference(&next_histogram)
                .copied()
                .filter(|key| !self.linked_cpu_authoritative_keys.contains(key))
                .filter(|key| self.current_demands_key(*key))
                .collect::<Vec<_>>();
            self.released_cpu_authority_candidates.extend(released);
            self.histogram_cpu_authoritative_keys = next_histogram;
            self.histogram_cpu_authoritative_keys_dirty = false;
        }
    }

    fn cpu_authoritative(&self, key: BrickKey) -> bool {
        self.linked_cpu_authoritative_keys.contains(&key)
            || self.histogram_cpu_authoritative_keys.contains(&key)
    }

    fn current_demands_key(&self, key: BrickKey) -> bool {
        [SCOPE_CURRENT_3D, SCOPE_CURRENT_3D_REFINEMENT]
            .into_iter()
            .any(|scope| {
                self.requirements_by_scope
                    .get(&scope)
                    .is_some_and(|requirements| requirements.binary_search(&key).is_ok())
            })
    }

    /// Releases CPU handles whose protection disappeared after the payload
    /// had already become GPU-resident through a linked presentation target.
    /// The candidate body is the exact authority-set delta, never the full
    /// current requirement cohort.
    pub(crate) fn retire_released_cpu_authority_payloads(
        &mut self,
        mut gpu_resident: impl FnMut(BrickKey) -> bool,
    ) -> usize {
        self.refresh_cpu_authoritative_keys();
        let candidates = std::mem::take(&mut self.released_cpu_authority_candidates);
        self.retire_gpu_resident_current_payloads(candidates, &mut gpu_resident)
    }

    /// Releases only display-only CPU handles whose exact immutable payload
    /// has committed to GPU residency. CPU-authoritative linked views,
    /// playback, analysis, and the bounded histogram sample remain retained.
    pub(crate) fn retire_gpu_resident_current_payloads(
        &mut self,
        candidates: impl IntoIterator<Item = BrickKey>,
        mut gpu_resident: impl FnMut(BrickKey) -> bool,
    ) -> usize {
        self.refresh_cpu_authoritative_keys();
        let mut retired = 0_usize;
        for key in candidates {
            if !self.current_demands_key(key) || self.cpu_authoritative(key) || !gpu_resident(key) {
                continue;
            }
            if self.retained_leases.retire_payload_handle(key) {
                retired = retired.saturating_add(1);
            }
        }
        retired
    }

    pub(crate) fn histogram_requirements(
        &self,
        layer: mirante4d_domain::LogicalLayerKey,
    ) -> &[BrickKey] {
        self.histogram_requirements_by_layer
            .get(&layer)
            .map_or(&[], |requirements| requirements.as_ref())
    }

    pub(crate) fn histogram_generation(&self, layer: mirante4d_domain::LogicalLayerKey) -> u64 {
        self.histogram_generation_by_layer
            .get(&layer)
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn take_retained_leases(&mut self) -> RetainedLeases {
        self.retained_requirements_dirty = true;
        std::mem::take(&mut self.retained_leases)
    }

    pub(crate) const fn current_scale(&self) -> ScaleLevel {
        self.current_scale
    }

    pub(crate) const fn current_ideal_scale(&self) -> ScaleLevel {
        self.current_ideal_scale
    }

    pub(crate) fn current_ideal_layer_scales(
        &self,
    ) -> &BTreeMap<mirante4d_domain::LogicalLayerKey, ScaleLevel> {
        &self.current_ideal_layer_scales
    }

    pub(crate) const fn current_capacity_constrained(&self) -> bool {
        self.current_capacity_constrained
    }

    pub(crate) const fn current_playback_downshifted(&self) -> bool {
        self.current_playback_downshifted
    }

    pub(crate) const fn current_covers_full_volume(&self) -> bool {
        self.current_covers_full_volume
    }

    /// Adds every 3D scope replacement to a pure aggregate reconciliation.
    /// This selects the same branch as the later infallible commit but neither
    /// calls the runtime nor changes installed demand.
    pub(crate) fn prepare_progressive_current_scope_targets(
        &self,
        plan: &PreparedProgressiveDatasetDemandPlan,
        four_panel: bool,
        preserve_complete_presentation: bool,
        targets: &mut ScopeReconciliationTargets,
    ) {
        let target = &plan.target;
        if self.current_prepared_plan_matches(target) {
            targets.remove(SCOPE_CURRENT_3D_REFINEMENT);
        } else if target.requirements.canonical().is_empty() {
            targets.replace(SCOPE_CURRENT_3D, &target.requirements);
            targets.remove(SCOPE_CURRENT_3D_REFINEMENT);
        } else if preserve_complete_presentation {
            // The installed complete scope is the navigation front. Keep it
            // admitted and globally accounted while the finer replacement is
            // prepared privately; later camera samples can rebind this body
            // instead of waiting behind cold refinement work.
            targets.replace(SCOPE_CURRENT_3D_REFINEMENT, &target.requirements);
        } else if let Some(coarse) = plan.coarse.as_ref() {
            targets.replace(SCOPE_CURRENT_3D, &coarse.requirements);
            targets.replace(SCOPE_CURRENT_3D_REFINEMENT, &target.requirements);
        } else {
            targets.replace(SCOPE_CURRENT_3D, &target.requirements);
            targets.remove(SCOPE_CURRENT_3D_REFINEMENT);
        }
        if !four_panel {
            for scope in [
                SCOPE_CROSS_SECTION_XY,
                SCOPE_CROSS_SECTION_XZ,
                SCOPE_CROSS_SECTION_YZ,
            ] {
                targets.remove(scope);
            }
        }
    }

    /// Infallible commit half of `preflight_prepared_progressive_current_plan`.
    /// The branch is recomputed against still-unmodified state, so the exact
    /// preflighted bodies move into all related 3D maps as one UI transaction.
    pub(crate) fn commit_preflighted_progressive_current_plan(
        &mut self,
        plan: PreparedProgressiveDatasetDemandPlan,
        four_panel: bool,
        preserve_complete_presentation: bool,
    ) -> bool {
        let PreparedProgressiveDatasetDemandPlan {
            ideal_scale,
            ideal_layer_scales,
            target,
            coarse,
            navigation_candidates: _,
            reuse_envelope: _,
            playback_timepoint_count: _,
        } = plan;
        self.current_ideal_scale = ideal_scale;
        self.current_capacity_constrained = target.layer_scales != ideal_layer_scales;
        self.current_ideal_layer_scales = ideal_layer_scales;
        if self.current_prepared_plan_matches(&target) {
            self.commit_clear_staged_current_plan();
            let changed = self.commit_four_panel_state(four_panel);
            self.last_plan_error = None;
            return changed;
        }

        if target.requirements.canonical().is_empty() {
            self.commit_prepared_display_plan(target);
            self.commit_clear_staged_current_plan();
            self.commit_four_panel_state(four_panel);
            self.last_plan_error = None;
            return true;
        }

        let presentation_changed = if preserve_complete_presentation {
            self.holding_previous_presentation = true;
            false
        } else if let Some(coarse) = coarse {
            self.commit_prepared_display_plan(coarse);
            self.holding_previous_presentation = false;
            true
        } else {
            self.commit_prepared_display_plan(target);
            self.commit_clear_staged_current_plan();
            self.holding_previous_presentation = false;
            self.commit_four_panel_state(four_panel);
            self.last_plan_error = None;
            return true;
        };

        self.commit_preflighted_scope_replacement(
            SCOPE_CURRENT_3D_REFINEMENT,
            target.requirements.clone(),
            target.layer_scales.clone(),
        );
        self.staged_current_plan = Some(target);
        let panels_changed = self.commit_four_panel_state(four_panel);
        self.last_plan_error = None;
        presentation_changed || panels_changed
    }

    /// Prepares one aggregate cancellation list without changing dispatcher
    /// or runtime state. Pending waiters are scanned twice: once to obtain an
    /// exact byte reservation and once to fill the exact-capacity ticket body;
    /// no semantic requirement body is traversed.
    pub(crate) fn prepare_scope_reconciliation(
        &self,
        targets: ScopeReconciliationTargets,
    ) -> Result<PreparedScopeReconciliation, RuntimeFault> {
        let should_cancel = |key: &PendingKey| {
            targets
                .requirements_by_scope
                .get(&key.scope)
                .is_some_and(|requirements| requirements.binary_search(&key.resource).is_err())
        };
        let cancellation_count = self
            .dispatcher
            .pending_by_key
            .keys()
            .filter(|key| should_cancel(key))
            .count();
        let ticket_bytes = cancellation_count
            .checked_mul(std::mem::size_of::<RequestTicket>())
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or_else(|| RuntimeFault::new(RuntimeFaultCode::InvariantViolation))?;
        let ticket_charge = if ticket_bytes == 0 {
            None
        } else {
            Some(
                self.cpu_ledger
                    .try_acquire(CpuLedgerCategory::QueuesAndResults, ticket_bytes)
                    .map_err(|error| RuntimeFault::new(map_cpu_ledger_error(error)))?,
            )
        };
        let mut cancellation_tickets = Vec::with_capacity(cancellation_count);
        let mut admission_cursor_rewinds = BTreeMap::<u64, usize>::new();
        for (key, ticket) in self
            .dispatcher
            .pending_by_key
            .iter()
            .filter(|(key, _)| should_cancel(key))
        {
            cancellation_tickets.push(*ticket);
            if targets.rewind_cancelled_scopes.contains(&key.scope) {
                let retry_cursor = self
                    .dispatcher
                    .pending_by_id
                    .get(&ticket.id())
                    .and_then(|pending| pending.retry_cursor)
                    .unwrap_or(0);
                admission_cursor_rewinds
                    .entry(key.scope)
                    .and_modify(|cursor| *cursor = (*cursor).min(retry_cursor))
                    .or_insert(retry_cursor);
            }
        }
        debug_assert_eq!(cancellation_tickets.len(), cancellation_count);
        Ok(PreparedScopeReconciliation {
            targets,
            cancellation_tickets: cancellation_tickets.into_boxed_slice(),
            admission_cursor_rewinds,
            _ticket_charge: ticket_charge,
        })
    }

    /// The sole fallible boundary immediately before publication. Runtime
    /// `cancel_many` validates every ticket under one lock and mutates none on
    /// error; after it succeeds, dispatcher cleanup is local and infallible.
    pub(crate) fn commit_prepared_scope_reconciliation(
        &mut self,
        prepared: PreparedScopeReconciliation,
    ) -> Result<(), RuntimeFault> {
        let PreparedScopeReconciliation {
            targets,
            cancellation_tickets,
            admission_cursor_rewinds,
            _ticket_charge,
        } = prepared;
        if !cancellation_tickets.is_empty() {
            self.dispatcher.runtime.cancel_many(&cancellation_tickets)?;
        }
        self.dispatcher.pending_by_key.retain(|key, _| {
            targets
                .requirements_by_scope
                .get(&key.scope)
                .is_none_or(|requirements| requirements.binary_search(&key.resource).is_ok())
        });
        for (scope, rewind) in admission_cursor_rewinds {
            let cursor = self.admission_cursor_by_scope.entry(scope).or_default();
            *cursor = (*cursor).min(rewind);
        }
        for (scope, requirements) in targets.requirements_by_scope {
            if let Some(failed) = self.dispatcher.failed_by_scope.get_mut(&scope) {
                failed.retain(|resource, _| requirements.binary_search(resource).is_ok());
            }
            if self
                .dispatcher
                .failed_by_scope
                .get(&scope)
                .is_some_and(BTreeMap::is_empty)
            {
                self.dispatcher.failed_by_scope.remove(&scope);
            }
        }
        drop(_ticket_charge);
        Ok(())
    }

    fn commit_prepared_display_plan(&mut self, plan: PreparedDatasetDemandPlan) {
        self.current_playback_downshifted = plan.playback_downshifted;
        self.current_covers_full_volume = plan.covers_full_volume;
        self.current_scale = plan.scale;
        self.commit_preflighted_scope_replacement(
            SCOPE_CURRENT_3D,
            plan.requirements,
            plan.layer_scales,
        );
    }

    fn commit_clear_staged_current_plan(&mut self) {
        self.staged_current_plan = None;
        self.holding_previous_presentation = false;
        self.commit_preflighted_scope_removal(SCOPE_CURRENT_3D_REFINEMENT);
    }

    fn commit_four_panel_state(&mut self, four_panel: bool) -> bool {
        let changed = self.four_panel != four_panel;
        if !four_panel {
            for scope in [
                SCOPE_CROSS_SECTION_XY,
                SCOPE_CROSS_SECTION_XZ,
                SCOPE_CROSS_SECTION_YZ,
            ] {
                self.commit_preflighted_scope_removal(scope);
            }
        }
        self.four_panel = four_panel;
        changed
    }

    fn current_prepared_plan_matches(&self, plan: &PreparedDatasetDemandPlan) -> bool {
        self.prepared_body_by_scope
            .get(&SCOPE_CURRENT_3D)
            .is_some_and(|current| current.shares_storage_with(plan.requirements.body()))
            && self
                .required_prefix_len_by_scope
                .get(&SCOPE_CURRENT_3D)
                .copied()
                == Some(plan.requirements.required_prefix_len())
            && self.layer_scales_by_scope.get(&SCOPE_CURRENT_3D) == Some(&plan.layer_scales)
            && self.current_scale == plan.scale
            && self.current_covers_full_volume == plan.covers_full_volume
            && self.current_playback_downshifted == plan.playback_downshifted
            && self.staged_current_plan.is_none()
    }

    /// Pure scope-target preparation for renderer-candidate promotion. Ticket
    /// cancellation is aggregated with the retained-union and renderer
    /// preflights by the display transaction.
    pub(crate) fn prepare_staged_current_promotion_scope_targets(
        &self,
        targets: &mut ScopeReconciliationTargets,
    ) -> bool {
        if self.staged_current_plan.is_none() {
            return false;
        }
        let staged_requirements = self
            .requirements_by_scope
            .get(&SCOPE_CURRENT_3D_REFINEMENT)
            .cloned()
            .expect("a staged plan owns canonical refinement requirements");
        targets.replace_handle(SCOPE_CURRENT_3D, staged_requirements);
        targets.remove(SCOPE_CURRENT_3D_REFINEMENT);
        // Refinement tickets cannot change cancellation scope when their
        // immutable body becomes current. Preserve only the earliest exact
        // canceled cursor so promotion can re-admit those holes without a
        // full body rescan.
        targets.rewind_cancelled_scope(SCOPE_CURRENT_3D_REFINEMENT);
        true
    }

    fn promote_staged_current_plan(&mut self) -> bool {
        if self.staged_current_plan.is_none() {
            return false;
        }
        let plan = self
            .staged_current_plan
            .take()
            .expect("the checked staged plan remains installed");
        self.current_playback_downshifted = plan.playback_downshifted;
        self.current_covers_full_volume = plan.covers_full_volume;
        self.current_scale = plan.scale;

        self.requirements_by_scope.remove(&SCOPE_CURRENT_3D);
        let requirements = self
            .requirements_by_scope
            .remove(&SCOPE_CURRENT_3D_REFINEMENT)
            .expect("a staged plan owns canonical refinement requirements");
        self.requirements_by_scope
            .insert(SCOPE_CURRENT_3D, requirements);
        self.gpu_priority_order_by_scope.remove(&SCOPE_CURRENT_3D);
        let gpu_priority = self
            .gpu_priority_order_by_scope
            .remove(&SCOPE_CURRENT_3D_REFINEMENT)
            .expect("a staged plan owns GPU-priority refinement requirements");
        self.gpu_priority_order_by_scope
            .insert(SCOPE_CURRENT_3D, gpu_priority);
        self.required_prefix_len_by_scope.remove(&SCOPE_CURRENT_3D);
        let required_prefix_len = self
            .required_prefix_len_by_scope
            .remove(&SCOPE_CURRENT_3D_REFINEMENT)
            .expect("a staged plan owns a required-prefix boundary");
        self.required_prefix_len_by_scope
            .insert(SCOPE_CURRENT_3D, required_prefix_len);
        self.prepared_body_by_scope.remove(&SCOPE_CURRENT_3D);
        if let Some(body) = self
            .prepared_body_by_scope
            .remove(&SCOPE_CURRENT_3D_REFINEMENT)
        {
            self.prepared_body_by_scope.insert(SCOPE_CURRENT_3D, body);
        }
        self.layer_scales_by_scope.remove(&SCOPE_CURRENT_3D);
        let layer_scales = self
            .layer_scales_by_scope
            .remove(&SCOPE_CURRENT_3D_REFINEMENT)
            .expect("a staged plan owns per-layer scales");
        self.layer_scales_by_scope
            .insert(SCOPE_CURRENT_3D, layer_scales);
        self.admission_cursor_by_scope.remove(&SCOPE_CURRENT_3D);
        let admission_cursor = self
            .admission_cursor_by_scope
            .remove(&SCOPE_CURRENT_3D_REFINEMENT)
            .expect("a staged plan owns an admission cursor");
        self.admission_cursor_by_scope
            .insert(SCOPE_CURRENT_3D, admission_cursor);
        self.readiness_by_scope.remove(&SCOPE_CURRENT_3D);
        let readiness = self
            .readiness_by_scope
            .remove(&SCOPE_CURRENT_3D_REFINEMENT)
            .expect("a staged plan owns readiness state");
        self.readiness_by_scope.insert(SCOPE_CURRENT_3D, readiness);
        self.retained_requirements_dirty = true;
        self.histogram_cpu_authoritative_keys_dirty = true;
        self.holding_previous_presentation = false;
        #[cfg(test)]
        {
            self.staged_promotion_commits = self.staged_promotion_commits.saturating_add(1);
            // Deliberately remains zero: all large bodies above move by
            // ownership or Arc pointer, never by per-requirement traversal.
            self.staged_promotion_requirement_visits =
                self.staged_promotion_requirement_visits.saturating_add(0);
        }
        true
    }

    pub(crate) fn commit_reconciled_gpu_prefilled_staged_current_plan(&mut self) {
        assert!(
            self.promote_staged_current_plan(),
            "a reconciled renderer-candidate promotion retains its staged plan until commit"
        );
    }

    pub(crate) fn record_plan_error(&mut self, error: impl Into<String>) {
        self.last_plan_error = Some(error.into());
    }

    pub(crate) fn clear_plan_error(&mut self) {
        self.last_plan_error = None;
    }

    pub(crate) fn last_plan_error(&self) -> Option<&str> {
        self.last_plan_error.as_deref()
    }

    pub(crate) fn scope_layer_scales(
        &self,
        scope: u64,
    ) -> Option<&BTreeMap<mirante4d_domain::LogicalLayerKey, ScaleLevel>> {
        self.layer_scales_by_scope.get(&scope)
    }

    /// Infallible commit half used only after all scopes and the retained
    /// union have passed preflight. It performs no fallible runtime calls.
    pub(crate) fn commit_preflighted_scope_replacement(
        &mut self,
        scope: u64,
        requirements: PreparedDemandRequirements,
        layer_scales: BTreeMap<mirante4d_domain::LogicalLayerKey, ScaleLevel>,
    ) -> bool {
        let (body, admitted_prefix_len, required_prefix_len) =
            requirements.into_body_and_prefixes();
        let canonical = Arc::clone(body.canonical());
        let ranked = Arc::clone(body.ranked());
        let body_unchanged = self
            .prepared_body_by_scope
            .get(&scope)
            .is_some_and(|current| current.shares_storage_with(&body));
        let required_prefix_unchanged =
            self.required_prefix_len_by_scope.get(&scope).copied() == Some(required_prefix_len);
        if body_unchanged
            && required_prefix_unchanged
            && self.layer_scales_by_scope.get(&scope) == Some(&layer_scales)
        {
            return false;
        }

        let canonical_unchanged = self
            .requirements_by_scope
            .get(&scope)
            .is_some_and(|current| Arc::ptr_eq(current, &canonical));
        self.prepared_body_by_scope.insert(scope, body);
        self.requirements_by_scope
            .insert(scope, Arc::clone(&canonical));
        self.gpu_priority_order_by_scope.insert(scope, ranked);
        self.required_prefix_len_by_scope
            .insert(scope, required_prefix_len);
        self.layer_scales_by_scope.insert(scope, layer_scales);
        self.admission_cursor_by_scope
            .insert(scope, admitted_prefix_len.min(canonical.len()));
        if !canonical_unchanged {
            self.retained_requirements_dirty = true;
        }
        if !body_unchanged {
            self.readiness_by_scope
                .insert(scope, ScopeReadiness::default());
        } else if !required_prefix_unchanged
            && let Some(readiness) = self.readiness_by_scope.get(&scope)
        {
            readiness
                .required_cursor
                .set(readiness.required_cursor.get().min(required_prefix_len));
        }
        if matches!(scope, SCOPE_CURRENT_3D | SCOPE_CURRENT_3D_REFINEMENT) {
            self.histogram_cpu_authoritative_keys_dirty = true;
        } else if matches!(
            scope,
            SCOPE_CROSS_SECTION_XY
                | SCOPE_CROSS_SECTION_XZ
                | SCOPE_CROSS_SECTION_YZ
                | SCOPE_PLAYBACK
                | SCOPE_ANALYSIS
        ) {
            self.linked_cpu_authoritative_keys_dirty = true;
        }
        #[cfg(test)]
        {
            self.prepared_scope_commits = self.prepared_scope_commits.saturating_add(1);
            self.prepared_scope_commit_requirement_visits = self
                .prepared_scope_commit_requirement_visits
                .saturating_add(0);
        }
        true
    }

    pub(crate) fn commit_preflighted_playback_scope_replacement(
        &mut self,
        requirements: PreparedDemandRequirements,
        layer_scales: BTreeMap<mirante4d_domain::LogicalLayerKey, ScaleLevel>,
        resources_per_timepoint: usize,
    ) -> bool {
        debug_assert!(
            resources_per_timepoint == 0
                || (requirements.required_prefix_len() >= resources_per_timepoint
                    && requirements
                        .ranked()
                        .len()
                        .is_multiple_of(resources_per_timepoint)),
            "a playback frame body must partition the ranked temporal window"
        );
        self.playback_resources_per_timepoint =
            resources_per_timepoint.min(requirements.ranked().len());
        self.commit_preflighted_scope_replacement(SCOPE_PLAYBACK, requirements, layer_scales)
    }

    /// Removes one preflighted scope without allocating an empty replacement
    /// body. All scope getters already define absence as the canonical empty
    /// state.
    pub(crate) fn commit_preflighted_scope_removal(&mut self, scope: u64) -> bool {
        let removed = self
            .requirements_by_scope
            .remove(&scope)
            .is_some_and(|requirements| !requirements.is_empty());
        self.gpu_priority_order_by_scope.remove(&scope);
        self.required_prefix_len_by_scope.remove(&scope);
        self.prepared_body_by_scope.remove(&scope);
        self.layer_scales_by_scope.remove(&scope);
        self.admission_cursor_by_scope.remove(&scope);
        self.readiness_by_scope.remove(&scope);
        if scope == SCOPE_PLAYBACK {
            self.playback_resources_per_timepoint = 0;
        }
        self.retained_requirements_dirty |= removed;
        if matches!(scope, SCOPE_CURRENT_3D | SCOPE_CURRENT_3D_REFINEMENT) {
            self.histogram_cpu_authoritative_keys_dirty |= removed;
        } else if matches!(
            scope,
            SCOPE_CROSS_SECTION_XY
                | SCOPE_CROSS_SECTION_XZ
                | SCOPE_CROSS_SECTION_YZ
                | SCOPE_PLAYBACK
                | SCOPE_ANALYSIS
        ) {
            self.linked_cpu_authoritative_keys_dirty |= removed;
        }
        removed
    }

    pub(crate) const fn renderer_requirement_union_needs_preparation(&self) -> bool {
        self.retained_requirements_dirty
    }

    pub(crate) fn preflight_prepared_renderer_requirement_union(
        &self,
        requirements: &Arc<[BrickKey]>,
    ) -> Result<(), RetainedLeaseError> {
        RetainedLeases::preflight_prepared_requirements(requirements)
    }

    pub(crate) fn preflight_prepared_renderer_requirement_update(
        &self,
        previous: &Arc<[BrickKey]>,
        next: &Arc<[BrickKey]>,
    ) -> Result<(), RetainedLeaseError> {
        self.retained_leases
            .preflight_prepared_requirement_update(previous, next)
    }

    /// Infallible half of the prepared retained-union transaction. All
    /// capacity and old-Arc identity checks must precede any scope mutation.
    pub(crate) fn commit_preflighted_renderer_requirement_update(
        &mut self,
        previous: Arc<[BrickKey]>,
        next: Arc<[BrickKey]>,
        removals: &[BrickKey],
        charge: Arc<dyn CpuByteLease>,
    ) -> usize {
        let retired = self
            .retained_leases
            .commit_prepared_requirement_update(previous, next, removals, charge);
        self.retained_requirements_dirty = false;
        retired
    }

    #[cfg(test)]
    pub(crate) fn renderer_requirements(&self) -> Vec<BrickKey> {
        let mut resources = self
            .requirements_by_scope
            .values()
            .flat_map(|resources| resources.iter().copied())
            .collect::<Vec<_>>();
        resources.sort_unstable();
        resources.dedup();
        resources
    }

    pub(crate) fn scope_requirements(&self, scope: u64) -> &[BrickKey] {
        self.requirements_by_scope
            .get(&scope)
            .map_or(&[], |resources| resources.as_ref())
    }

    /// O(1) immutable handle used to preserve renderer requirement identity
    /// across camera-only frames without cloning the semantic key body.
    pub(crate) fn scope_requirement_handle(&self, scope: u64) -> Arc<[BrickKey]> {
        self.requirements_by_scope
            .get(&scope)
            .cloned()
            .unwrap_or_default()
    }

    /// Exact Arc identity used by the worker to bind a prepared retained-union
    /// removal delta to the state from which it was computed.
    pub(crate) fn renderer_requirement_handle(&self) -> RetainedRequirementHandle {
        self.retained_leases.accounted_requirement_handle()
    }

    /// Ranked/interleaved upload order is independent of the canonical
    /// renderer requirement body. Keeping both preserves stable GPU slots
    /// while the first useful upload cohort follows screen contribution.
    pub(crate) fn scope_gpu_priority_handle(&self, scope: u64) -> Arc<[BrickKey]> {
        self.gpu_priority_order_by_scope
            .get(&scope)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn scope_required_prefix_len(&self, scope: u64) -> usize {
        self.required_prefix_len_by_scope
            .get(&scope)
            .copied()
            .unwrap_or(0)
            .min(
                self.gpu_priority_order_by_scope
                    .get(&scope)
                    .map_or(0, |requirements| requirements.len()),
            )
    }

    /// Promotes a suffix only after its complete body has independently been
    /// proven resident. No admission scan is needed: every ranked key is
    /// already ready, so both scalar frontiers advance directly to the end.
    pub(crate) fn can_commit_complete_scope_prefetch_tail(&self, scope: u64) -> bool {
        let total = self
            .gpu_priority_order_by_scope
            .get(&scope)
            .map_or(0, |requirements| requirements.len());
        self.required_prefix_len_by_scope.contains_key(&scope)
            && self
                .readiness_by_scope
                .get(&scope)
                .is_some_and(|readiness| readiness.all_resources_cursor.get() == total)
    }

    pub(crate) fn commit_complete_scope_prefetch_tail(&mut self, scope: u64) {
        debug_assert!(self.can_commit_complete_scope_prefetch_tail(scope));
        let total = self
            .gpu_priority_order_by_scope
            .get(&scope)
            .map_or(0, |requirements| requirements.len());
        let required = self
            .required_prefix_len_by_scope
            .get_mut(&scope)
            .expect("a preflighted complete scope owns a required-prefix boundary");
        *required = total;
        self.admission_cursor_by_scope.insert(scope, total);
        let readiness = self
            .readiness_by_scope
            .get(&scope)
            .expect("a preflighted complete scope owns readiness cursors");
        debug_assert_eq!(readiness.all_resources_cursor.get(), total);
        readiness.required_cursor.set(total);
    }

    /// Exact immutable planning authority for one installed scope. Prepared
    /// scopes retain their attached CPU-ledger charge while a latest-only
    /// worker reads the baseline. Absent/empty scopes are wrapped without
    /// cloning either large key body.
    pub(crate) fn scope_prepared_body_handle(&self, scope: u64) -> PreparedResourceBody {
        self.prepared_body_by_scope
            .get(&scope)
            .cloned()
            .unwrap_or_else(|| {
                PreparedResourceBody::new(
                    self.scope_requirement_handle(scope),
                    self.scope_gpu_priority_handle(scope),
                    None,
                )
                .expect("installed scope key bodies satisfy prepared-body invariants")
            })
    }

    pub(crate) fn scope_admitted_prefix_len(&self, scope: u64) -> usize {
        self.admission_cursor_by_scope
            .get(&scope)
            .copied()
            .unwrap_or(0)
            .min(
                self.gpu_priority_order_by_scope
                    .get(&scope)
                    .map_or(0, |requirements| requirements.len()),
            )
    }

    #[cfg(test)]
    pub(crate) fn submit_scope(
        &mut self,
        scope: u64,
        priority: RequestPriority,
    ) -> Result<usize, RuntimeFault> {
        self.submit_scope_with_gpu_residency(scope, priority, |_| false)
    }

    pub(crate) fn submit_scope_with_gpu_residency(
        &mut self,
        scope: u64,
        priority: RequestPriority,
        mut gpu_resident: impl FnMut(BrickKey) -> bool,
    ) -> Result<usize, RuntimeFault> {
        let required_prefix_len = self.scope_required_prefix_len(scope);
        self.submit_scope_until_with_gpu_residency(
            scope,
            priority,
            required_prefix_len,
            &mut gpu_resident,
        )
    }

    fn submit_scope_until_with_gpu_residency(
        &mut self,
        scope: u64,
        priority: RequestPriority,
        end: usize,
        mut gpu_resident: impl FnMut(BrickKey) -> bool,
    ) -> Result<usize, RuntimeFault> {
        if let Some(fault) = self.dispatcher.scope_failure(scope) {
            return Err(fault.clone());
        }
        self.refresh_cpu_authoritative_keys();
        let resources = self
            .gpu_priority_order_by_scope
            .get(&scope)
            .cloned()
            .unwrap_or_default();
        let end = end.min(resources.len());
        let linked_cpu_authoritative_keys = &self.linked_cpu_authoritative_keys;
        let histogram_cpu_authoritative_keys = &self.histogram_cpu_authoritative_keys;
        let cursor = self.admission_cursor_by_scope.entry(scope).or_default();
        let mut submitted = 0;
        while *cursor < end {
            let resource = resources[*cursor];
            let pending = self
                .dispatcher
                .pending_by_key
                .contains_key(&PendingKey { scope, resource });
            // A live waiter can be promoted in place even when every bounded
            // admission slot is occupied. New waiters still stop before the
            // runtime's hard queue boundary.
            if !pending && !self.dispatcher.has_submission_capacity() {
                self.dispatcher.mark_admission_blocked();
                break;
            }
            #[cfg(test)]
            {
                self.admission_requirement_visits =
                    self.admission_requirement_visits.saturating_add(1);
            }
            let cpu_ready = self.retained_leases.payload(resource).is_some();
            let gpu_ready = !linked_cpu_authoritative_keys.contains(&resource)
                && !histogram_cpu_authoritative_keys.contains(&resource)
                && !cpu_ready
                && gpu_resident(resource);
            let already_ready = cpu_ready || gpu_ready;
            match self.dispatcher.submit_if_missing(
                scope,
                resource,
                priority,
                already_ready,
                Some(*cursor),
            ) {
                Ok(Some(_)) => {
                    submitted += 1;
                    *cursor += 1;
                }
                Ok(None) => *cursor += 1,
                Err(fault) if fault.code() == RuntimeFaultCode::QueueFull => {
                    self.dispatcher.mark_admission_blocked();
                    break;
                }
                Err(fault) => return Err(fault),
            }
        }
        Ok(submitted)
    }

    #[cfg(test)]
    pub(crate) const fn admission_requirement_visits(&self) -> u64 {
        self.admission_requirement_visits
    }

    #[cfg(test)]
    pub(crate) fn readiness_requirement_visits(&self) -> u64 {
        self.readiness_requirement_visits.get()
    }

    #[cfg(test)]
    pub(crate) const fn staged_promotion_work(&self) -> (u64, u64) {
        (
            self.staged_promotion_commits,
            self.staged_promotion_requirement_visits,
        )
    }

    #[cfg(test)]
    pub(crate) const fn cpu_authority_requirement_visits(&self) -> (u64, u64) {
        (
            self.linked_cpu_authority_requirement_visits,
            self.histogram_cpu_authority_requirement_visits,
        )
    }

    #[cfg(test)]
    pub(crate) const fn prepared_scope_commit_work(&self) -> (u64, u64) {
        (
            self.prepared_scope_commits,
            self.prepared_scope_commit_requirement_visits,
        )
    }

    /// Resolves one exact renderer eviction through the CPU data plane without
    /// retaining a second GPU-residency set. The renderer's
    /// sequenced event remains the retry authority until this method returns
    /// an acknowledgeable disposition.
    pub(crate) fn resolve_renderer_eviction(
        &mut self,
        resource: BrickKey,
    ) -> Result<RendererEvictionDisposition, RuntimeFault> {
        if !self.retained_leases.requires(resource) {
            return Ok(RendererEvictionDisposition::NoLongerDemanded);
        }
        if let Some(lease) = self.retained_leases.lease_handle(resource) {
            return Ok(RendererEvictionDisposition::Reoffer(lease));
        }

        let active_cross_scope = self.active_visible_demand.and_then(|active| {
            (active.installed
                && active.revision == self.latest_visible_intent_revision
                && matches!(
                    active.scope,
                    SCOPE_CROSS_SECTION_XY | SCOPE_CROSS_SECTION_XZ | SCOPE_CROSS_SECTION_YZ
                ))
            .then_some(active.scope)
        });
        let mut ordered_scopes = Vec::with_capacity(DATASET_DEMAND_SCOPES.len());
        if let Some(scope) = active_cross_scope {
            ordered_scopes.push((scope, RequestPriority::CurrentView));
        }
        for (scope, priority) in [
            (SCOPE_CURRENT_3D, RequestPriority::CurrentView),
            (
                SCOPE_CURRENT_3D_REFINEMENT,
                RequestPriority::VisibleRefinement,
            ),
            (SCOPE_CROSS_SECTION_XY, RequestPriority::LinkedView),
            (SCOPE_CROSS_SECTION_XZ, RequestPriority::LinkedView),
            (SCOPE_CROSS_SECTION_YZ, RequestPriority::LinkedView),
            (SCOPE_PLAYBACK, RequestPriority::Playback),
        ] {
            if Some(scope) != active_cross_scope {
                ordered_scopes.push((scope, priority));
            }
        }

        let mut owning_scopes = Vec::new();
        for (scope, priority) in ordered_scopes {
            if self
                .requirements_by_scope
                .get(&scope)
                .is_none_or(|requirements| requirements.binary_search(&resource).is_err())
            {
                continue;
            }
            let index = self
                .gpu_priority_order_by_scope
                .get(&scope)
                .and_then(|requirements| {
                    requirements
                        .iter()
                        .position(|candidate| *candidate == resource)
                })
                .expect("canonical and ranked scope bodies contain the same resources");
            let cursor = self.admission_cursor_by_scope.entry(scope).or_insert(index);
            *cursor = (*cursor).min(index);
            if let Some(readiness) = self.readiness_by_scope.get(&scope) {
                readiness
                    .required_cursor
                    .set(readiness.required_cursor.get().min(index));
                readiness
                    .all_resources_cursor
                    .set(readiness.all_resources_cursor.get().min(index));
            }
            owning_scopes.push((scope, priority, index));
        }
        let Some((scope, priority, retry_cursor)) = owning_scopes.first().copied() else {
            debug_assert!(
                false,
                "a retained renderer requirement belongs to an interactive scope"
            );
            return Ok(RendererEvictionDisposition::NoLongerDemanded);
        };
        let already_pending = self
            .dispatcher
            .pending_by_key
            .contains_key(&PendingKey { scope, resource });
        if !already_pending && !self.dispatcher.has_submission_capacity() {
            self.dispatcher.mark_admission_blocked();
            return Ok(RendererEvictionDisposition::Deferred);
        }
        match self.dispatcher.submit_if_missing(
            scope,
            resource,
            priority,
            false,
            Some(retry_cursor),
        ) {
            Ok(_) => Ok(RendererEvictionDisposition::Submitted),
            Err(fault) if fault.code() == RuntimeFaultCode::QueueFull => {
                self.dispatcher.mark_admission_blocked();
                Ok(RendererEvictionDisposition::Deferred)
            }
            Err(fault) => Err(fault),
        }
    }

    /// Refills the bounded runtime queue from persistent per-scope cursors.
    /// This is deliberately independent of semantic demand planning: draining
    /// one batch of completions must not rebuild the view/frustum plan or
    /// rescan already-admitted prefixes of a large requirement set.
    pub(crate) fn pump_interactive_admission(&mut self) -> Result<usize, RuntimeFault> {
        self.pump_interactive_admission_with_gpu_residency(|_| false)
    }

    pub(crate) fn pump_interactive_admission_with_gpu_residency(
        &mut self,
        gpu_resident: impl FnMut(BrickKey) -> bool,
    ) -> Result<usize, RuntimeFault> {
        self.begin_submission_pass();
        self.pump_interactive_admission_after_begin_with_gpu_residency(gpu_resident)
    }

    pub(crate) fn pump_interactive_admission_after_begin_with_gpu_residency(
        &mut self,
        mut gpu_resident: impl FnMut(BrickKey) -> bool,
    ) -> Result<usize, RuntimeFault> {
        self.refresh_cpu_authoritative_keys();
        let mut submitted = 0_usize;
        let active_cross_scope = self.active_visible_demand.and_then(|active| {
            (active.installed
                && active.revision == self.latest_visible_intent_revision
                && matches!(
                    active.scope,
                    SCOPE_CROSS_SECTION_XY | SCOPE_CROSS_SECTION_XZ | SCOPE_CROSS_SECTION_YZ
                ))
            .then_some(active.scope)
        });
        if let Some(scope) = active_cross_scope {
            submitted = submitted.saturating_add(self.submit_scope_with_gpu_residency(
                scope,
                RequestPriority::CurrentView,
                &mut gpu_resident,
            )?);
        }
        for (scope, priority) in [
            (SCOPE_CURRENT_3D, RequestPriority::CurrentView),
            (
                SCOPE_CURRENT_3D_REFINEMENT,
                RequestPriority::VisibleRefinement,
            ),
            (SCOPE_CROSS_SECTION_XY, RequestPriority::LinkedView),
            (SCOPE_CROSS_SECTION_XZ, RequestPriority::LinkedView),
            (SCOPE_CROSS_SECTION_YZ, RequestPriority::LinkedView),
            (SCOPE_PLAYBACK, RequestPriority::Playback),
        ] {
            if self.dispatcher.admission_blocked() {
                break;
            }
            if Some(scope) == active_cross_scope {
                continue;
            }
            submitted = submitted.saturating_add(self.submit_scope_with_gpu_residency(
                scope,
                priority,
                &mut gpu_resident,
            )?);
            if self.dispatcher.admission_blocked() {
                break;
            }
        }
        // Only spare queue capacity reaches camera guards. Keeping this as a
        // final pass prevents speculative I/O from occupying slots needed by
        // 3D primary coverage, staged refinement, linked panels, or playback.
        if !self.dispatcher.admission_blocked() {
            for scope in [
                SCOPE_CURRENT_3D,
                SCOPE_CURRENT_3D_REFINEMENT,
                SCOPE_CROSS_SECTION_XY,
                SCOPE_CROSS_SECTION_XZ,
                SCOPE_CROSS_SECTION_YZ,
                SCOPE_PLAYBACK,
            ] {
                let total = self
                    .gpu_priority_order_by_scope
                    .get(&scope)
                    .map_or(0, |requirements| requirements.len());
                submitted = submitted.saturating_add(self.submit_scope_until_with_gpu_residency(
                    scope,
                    RequestPriority::Prefetch,
                    total,
                    &mut gpu_resident,
                )?);
                if self.dispatcher.admission_blocked() {
                    break;
                }
            }
        }
        Ok(submitted)
    }

    pub(crate) fn defer_interactive_admission_refill(
        &mut self,
        preserve_presentation_on_fault: bool,
    ) {
        self.dispatcher.mark_admission_blocked();
        self.preserve_presentation_for_deferred_refill_fault |= preserve_presentation_on_fault;
    }

    pub(crate) fn take_deferred_refill_presentation_preservation(&mut self) -> bool {
        std::mem::take(&mut self.preserve_presentation_for_deferred_refill_fault)
    }

    pub(crate) const fn deferred_refill_preserves_presentation(&self) -> bool {
        self.preserve_presentation_for_deferred_refill_fault
    }

    pub(crate) fn finish_deferred_refill_if_unblocked(&mut self) {
        if !self.dispatcher.admission_blocked() {
            self.preserve_presentation_for_deferred_refill_fault = false;
        }
    }

    pub(crate) fn drain_runtime_results(
        &mut self,
        maximum: usize,
        mut accept_analysis: impl FnMut(RequestTicket, RuntimeOutcome),
    ) -> Result<(usize, InstalledLeaseChanges), RuntimeFault> {
        let dispatcher = &mut self.dispatcher;
        let retained_leases = &mut self.retained_leases;
        let admission_cursor_by_scope = &mut self.admission_cursor_by_scope;
        let histogram_requirements_by_layer = &self.histogram_requirements_by_layer;
        let histogram_generation_by_layer = &mut self.histogram_generation_by_layer;
        let mut installed = InstalledLeaseChanges::default();
        let drained =
            dispatcher.drain_with_retry_cursor(maximum, |ticket, outcome, retry_cursor| {
                if ticket.generation().scope() == SCOPE_ANALYSIS {
                    accept_analysis(ticket, outcome);
                } else {
                    if matches!(
                        &outcome,
                        RuntimeOutcome::Failed(fault)
                            if runtime_failure_needs_capacity_retry(fault.code())
                    ) && let Some(index) = retry_cursor
                    {
                        let cursor = admission_cursor_by_scope
                            .entry(ticket.generation().scope())
                            .or_insert(index);
                        *cursor = (*cursor).min(index);
                    }
                    if let RuntimeOutcome::Ready(lease) = outcome
                        && retained_leases.requires(ticket.resource())
                    {
                        let lease: Arc<dyn ResourceLease> = Arc::new(lease);
                        match retained_leases.install(Arc::clone(&lease)) {
                            Ok(true) => {
                                installed.scopes.insert(ticket.generation().scope());
                                let resource = ticket.resource();
                                installed.leases.push(lease);
                                if histogram_requirements_by_layer
                                    .get(&resource.layer())
                                    .is_some_and(|requirements| requirements.contains(&resource))
                                {
                                    let generation = histogram_generation_by_layer
                                        .entry(resource.layer())
                                        .or_default();
                                    *generation = generation.saturating_add(1);
                                }
                            }
                            Ok(false) => {}
                            Err(error) => tracing::error!(
                                %error,
                                "runtime lease delivery violated retained-demand requirements"
                            ),
                        }
                    }
                }
            })?;
        Ok((drained, installed))
    }

    pub(crate) fn begin_submission_pass(&mut self) {
        self.dispatcher.begin_submission_pass();
    }

    pub(crate) fn scope_complete(&self, scope: u64) -> bool {
        self.scope_complete_with_gpu_residency(scope, |_| false)
    }

    pub(crate) fn scope_complete_with_gpu_residency(
        &self,
        scope: u64,
        gpu_resident: impl FnMut(BrickKey) -> bool,
    ) -> bool {
        if scope == SCOPE_CURRENT_3D && self.staged_current_plan.is_some() {
            return false;
        }
        self.scope_resources_complete_with_gpu_residency(scope, gpu_resident)
    }

    pub(crate) fn scope_resources_complete(&self, scope: u64) -> bool {
        self.scope_resources_complete_with_gpu_residency(scope, |_| false)
    }

    pub(crate) fn scope_resources_complete_with_gpu_residency(
        &self,
        scope: u64,
        gpu_resident: impl FnMut(BrickKey) -> bool,
    ) -> bool {
        let Some(resources) = self.gpu_priority_order_by_scope.get(&scope) else {
            return false;
        };
        let required_prefix_len = self
            .required_prefix_len_by_scope
            .get(&scope)
            .copied()
            .unwrap_or(0)
            .min(resources.len());
        let Some(readiness) = self.readiness_by_scope.get(&scope) else {
            debug_assert!(false, "every installed scope has readiness state");
            return false;
        };
        self.advance_scope_readiness_cursor(
            resources,
            required_prefix_len,
            &readiness.required_cursor,
            gpu_resident,
        )
    }

    pub(crate) fn playback_successor_complete_with_gpu_residency(
        &self,
        gpu_resident: impl FnMut(BrickKey) -> bool,
    ) -> bool {
        if self.playback_resources_per_timepoint == 0 {
            return false;
        }
        let Some(resources) = self.gpu_priority_order_by_scope.get(&SCOPE_PLAYBACK) else {
            return false;
        };
        let Some(readiness) = self.readiness_by_scope.get(&SCOPE_PLAYBACK) else {
            debug_assert!(false, "every installed playback scope has readiness state");
            return false;
        };
        self.advance_scope_readiness_cursor(
            resources,
            self.playback_resources_per_timepoint,
            &readiness.required_cursor,
            gpu_resident,
        )
    }

    pub(crate) fn scope_all_resources_complete(&self, scope: u64) -> bool {
        self.scope_all_resources_complete_with_gpu_residency(scope, |_| false)
    }

    pub(crate) fn scope_all_resources_complete_with_gpu_residency(
        &self,
        scope: u64,
        gpu_resident: impl FnMut(BrickKey) -> bool,
    ) -> bool {
        let Some(resources) = self.gpu_priority_order_by_scope.get(&scope) else {
            return false;
        };
        let Some(readiness) = self.readiness_by_scope.get(&scope) else {
            debug_assert!(false, "every installed scope has readiness state");
            return false;
        };
        self.advance_scope_readiness_cursor(
            resources,
            resources.len(),
            &readiness.all_resources_cursor,
            gpu_resident,
        )
    }

    fn advance_scope_readiness_cursor(
        &self,
        resources: &[BrickKey],
        end: usize,
        readiness_cursor: &Cell<usize>,
        mut gpu_resident: impl FnMut(BrickKey) -> bool,
    ) -> bool {
        let end = end.min(resources.len());
        let mut cursor = readiness_cursor.get().min(end);
        while cursor < end {
            #[cfg(test)]
            self.readiness_requirement_visits
                .set(self.readiness_requirement_visits.get().saturating_add(1));
            let resource = resources[cursor];
            if self.retained_leases.payload(resource).is_none() && !gpu_resident(resource) {
                readiness_cursor.set(cursor);
                return false;
            }
            cursor += 1;
        }
        readiness_cursor.set(cursor);
        true
    }

    pub(crate) const fn staging_current_refinement(&self) -> bool {
        self.staged_current_plan.is_some()
    }

    pub(crate) const fn holding_previous_presentation(&self) -> bool {
        self.holding_previous_presentation
    }

    pub(crate) fn scope_is_empty(&self, scope: u64) -> bool {
        self.requirements_by_scope
            .get(&scope)
            .is_some_and(|resources| resources.is_empty())
    }

    pub(crate) fn scope_is_installed(&self, scope: u64) -> bool {
        self.requirements_by_scope.contains_key(&scope)
    }

    pub(crate) fn scope_ready_prefix(&self, scope: u64) -> usize {
        self.readiness_by_scope
            .get(&scope)
            .map_or(0, |readiness| readiness.required_cursor.get())
    }

    pub(crate) fn request_shutdown(&self) -> Result<(), RuntimeFault> {
        self.dispatcher.request_shutdown()
    }
}

const fn map_cpu_ledger_error(error: CpuLedgerError) -> RuntimeFaultCode {
    match error {
        CpuLedgerError::ZeroByteReservation => RuntimeFaultCode::InvariantViolation,
        CpuLedgerError::CapacityExceeded {
            category,
            requested_bytes,
            available_bytes,
        } => RuntimeFaultCode::CapacityExceeded {
            category,
            requested_bytes,
            available_bytes,
        },
        CpuLedgerError::ShuttingDown => RuntimeFaultCode::ShuttingDown,
    }
}

const fn runtime_failure_is_sticky(code: RuntimeFaultCode) -> bool {
    !matches!(
        code,
        RuntimeFaultCode::CapacityExceeded { .. }
            | RuntimeFaultCode::QueueFull
            | RuntimeFaultCode::Cancelled
            | RuntimeFaultCode::StaleGeneration
    )
}

const fn runtime_failure_needs_capacity_retry(code: RuntimeFaultCode) -> bool {
    matches!(code, RuntimeFaultCode::CapacityExceeded { .. })
}

#[cfg(test)]
fn scope_requirements_complete(
    resources: Option<&[BrickKey]>,
    mut ready: impl FnMut(BrickKey) -> bool,
) -> bool {
    resources.is_some_and(|resources| resources.iter().copied().all(&mut ready))
}

#[cfg(test)]
fn requirement_layer_scales(
    resources: &[BrickKey],
) -> Result<BTreeMap<mirante4d_domain::LogicalLayerKey, ScaleLevel>, RuntimeFault> {
    let mut scales = BTreeMap::new();
    for resource in resources {
        match scales.insert(resource.layer(), resource.scale()) {
            Some(previous) if previous != resource.scale() => {
                return Err(RuntimeFault::new(RuntimeFaultCode::InvariantViolation));
            }
            Some(_) | None => {}
        }
    }
    Ok(scales)
}

#[cfg(test)]
fn prepare_test_renderer_requirement_update(
    state: &DatasetDemandState,
    bodies: &[Arc<[BrickKey]>],
) -> anyhow::Result<crate::camera_demand_cache::PreparedRendererRequirementUpdate> {
    let previous = state.renderer_requirement_handle();
    let mut union_reservations = PreparedAllocationReservations::new(state.cpu_ledger.as_ref());
    let next = crate::camera_demand_cache::merge_requirement_union(
        bodies,
        &mut union_reservations,
        &mut || false,
    )?
    .expect("a non-cancellable test union completes");
    let next_charge = union_reservations.finish();
    let mut removal_reservations = PreparedAllocationReservations::new(state.cpu_ledger.as_ref());
    let removals = crate::camera_demand_cache::prepare_requirement_removals(
        &previous.requirements,
        &next,
        &mut removal_reservations,
        &mut || false,
    )?
    .expect("a non-cancellable test delta completes");
    Ok(
        crate::camera_demand_cache::PreparedRendererRequirementUpdate {
            previous,
            next: crate::camera_demand_cache::PreparedRequirementUnion {
                requirements: next,
                charge: next_charge,
            },
            removals,
            removal_charge: removal_reservations.finish(),
        },
    )
}

#[cfg(test)]
fn final_test_union_bodies(
    state: &DatasetDemandState,
    targets: &ScopeReconciliationTargets,
) -> Vec<Arc<[BrickKey]>> {
    state
        .requirements_by_scope
        .iter()
        .filter(|(scope, _)| !targets.requirements_by_scope.contains_key(scope))
        .map(|(_, requirements)| Arc::clone(requirements))
        .chain(
            targets
                .requirements_by_scope
                .values()
                .filter(|requirements| !requirements.is_empty())
                .cloned(),
        )
        .collect()
}

/// Test-fixture composition of the production prepared transaction. It keeps
/// setup on the same Arc-swap, exact-delta, and atomic-cancellation authority
/// as camera results without retaining a synchronous product installer.
#[cfg(test)]
pub(crate) fn install_prepared_scope_test_fixture(
    state: &mut DatasetDemandState,
    scope: u64,
    resources: Vec<BrickKey>,
) -> anyhow::Result<bool> {
    let required_prefix_len = resources.len();
    install_prepared_scope_test_fixture_with_required_prefix(
        state,
        scope,
        resources,
        required_prefix_len,
    )
}

#[cfg(test)]
fn remove_prepared_scope_test_fixture(
    state: &mut DatasetDemandState,
    scope: u64,
) -> anyhow::Result<bool> {
    let was_installed = state.scope_is_installed(scope);
    let mut targets = ScopeReconciliationTargets::default();
    targets.remove(scope);
    let update =
        prepare_test_renderer_requirement_update(state, &final_test_union_bodies(state, &targets))?;
    state.preflight_prepared_renderer_requirement_update(
        &update.previous.requirements,
        &update.next.requirements,
    )?;
    let reconciliation = state.prepare_scope_reconciliation(targets)?;
    state.commit_prepared_scope_reconciliation(reconciliation)?;
    state.commit_preflighted_scope_removal(scope);
    let crate::camera_demand_cache::PreparedRendererRequirementUpdate {
        previous,
        next,
        removals,
        removal_charge: _removal_charge,
    } = update;
    state.commit_preflighted_renderer_requirement_update(
        previous.requirements,
        next.requirements,
        &removals,
        next.charge,
    );
    Ok(was_installed)
}

#[cfg(test)]
fn install_prepared_scope_test_fixture_with_required_prefix(
    state: &mut DatasetDemandState,
    scope: u64,
    resources: Vec<BrickKey>,
    required_prefix_len: usize,
) -> anyhow::Result<bool> {
    let layer_scales = requirement_layer_scales(&resources)?;
    let mut scope_reservations = PreparedAllocationReservations::new(state.cpu_ledger.as_ref());
    let requirements = PreparedDemandRequirements::from_ranked_accounted(
        resources,
        required_prefix_len,
        &mut scope_reservations,
    )?;
    requirements
        .body()
        .attach_charge(scope_reservations.finish())?;
    let mut targets = ScopeReconciliationTargets::default();
    targets.replace(scope, &requirements);
    let update =
        prepare_test_renderer_requirement_update(state, &final_test_union_bodies(state, &targets))?;
    state.preflight_prepared_renderer_requirement_update(
        &update.previous.requirements,
        &update.next.requirements,
    )?;
    let reconciliation = state.prepare_scope_reconciliation(targets)?;
    state.commit_prepared_scope_reconciliation(reconciliation)?;
    let changed = state.commit_preflighted_scope_replacement(scope, requirements, layer_scales);
    let crate::camera_demand_cache::PreparedRendererRequirementUpdate {
        previous,
        next,
        removals,
        removal_charge: _removal_charge,
    } = update;
    state.commit_preflighted_renderer_requirement_update(
        previous.requirements,
        next.requirements,
        &removals,
        next.charge,
    );
    Ok(changed)
}

#[cfg(test)]
fn install_prepared_playback_scope_test_fixture(
    state: &mut DatasetDemandState,
    resources: Vec<BrickKey>,
    required_prefix_len: usize,
    resources_per_timepoint: usize,
) -> anyhow::Result<bool> {
    let layer_scales = requirement_layer_scales(&resources)?;
    let mut scope_reservations = PreparedAllocationReservations::new(state.cpu_ledger.as_ref());
    let requirements = PreparedDemandRequirements::from_ranked_accounted(
        resources,
        required_prefix_len,
        &mut scope_reservations,
    )?;
    requirements
        .body()
        .attach_charge(scope_reservations.finish())?;
    let mut targets = ScopeReconciliationTargets::default();
    targets.replace(SCOPE_PLAYBACK, &requirements);
    let update =
        prepare_test_renderer_requirement_update(state, &final_test_union_bodies(state, &targets))?;
    state.preflight_prepared_renderer_requirement_update(
        &update.previous.requirements,
        &update.next.requirements,
    )?;
    let reconciliation = state.prepare_scope_reconciliation(targets)?;
    state.commit_prepared_scope_reconciliation(reconciliation)?;
    let changed = state.commit_preflighted_playback_scope_replacement(
        requirements,
        layer_scales,
        resources_per_timepoint,
    );
    let crate::camera_demand_cache::PreparedRendererRequirementUpdate {
        previous,
        next,
        removals,
        removal_charge: _removal_charge,
    } = update;
    state.commit_preflighted_renderer_requirement_update(
        previous.requirements,
        next.requirements,
        &removals,
        next.charge,
    );
    Ok(changed)
}

#[cfg(test)]
fn install_prepared_progressive_test_fixture(
    state: &mut DatasetDemandState,
    plan: PreparedProgressiveDatasetDemandPlan,
    four_panel: bool,
    preserve_complete_presentation: bool,
) -> anyhow::Result<bool> {
    let mut targets = ScopeReconciliationTargets::default();
    state.prepare_progressive_current_scope_targets(
        &plan,
        four_panel,
        preserve_complete_presentation,
        &mut targets,
    );
    let update =
        prepare_test_renderer_requirement_update(state, &final_test_union_bodies(state, &targets))?;
    state.preflight_prepared_renderer_requirement_update(
        &update.previous.requirements,
        &update.next.requirements,
    )?;
    let reconciliation = state.prepare_scope_reconciliation(targets)?;
    state.commit_prepared_scope_reconciliation(reconciliation)?;
    let changed = state.commit_preflighted_progressive_current_plan(
        plan,
        four_panel,
        preserve_complete_presentation,
    );
    let crate::camera_demand_cache::PreparedRendererRequirementUpdate {
        previous,
        next,
        removals,
        removal_charge: _removal_charge,
    } = update;
    state.commit_preflighted_renderer_requirement_update(
        previous.requirements,
        next.requirements,
        &removals,
        next.charge,
    );
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Condvar, Mutex},
        time::{Duration, Instant},
    };

    use mirante4d_dataset::{
        ContentAddressStatus, CpuByteLease, CpuLedgerCategory, CpuLedgerError, DatasetCatalog,
        DatasetLayer, DatasetSource, DatasetSourceFault, DatasetSourceId, ReservedDecodeSink,
        ResourcePayloadFacts, ResourceRegion, ResourceValidity,
    };
    use mirante4d_dataset_runtime::DatasetRuntimeConfig;
    use mirante4d_domain::{GridToWorld, IntensityDType, Shape3D, Shape4D, TimeIndex};

    use super::*;

    struct ZeroSource {
        catalog: Arc<DatasetCatalog>,
    }

    #[derive(Default)]
    struct DecodeGate {
        state: Mutex<(usize, bool)>,
        changed: Condvar,
    }

    impl DecodeGate {
        fn enter_and_wait(&self) {
            let mut state = self.state.lock().unwrap();
            state.0 += 1;
            self.changed.notify_all();
            while !state.1 {
                state = self.changed.wait(state).unwrap();
            }
        }

        fn wait_until_entered(&self) {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut state = self.state.lock().unwrap();
            while state.0 == 0 {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "the gated decode did not start");
                let (next, timeout) = self.changed.wait_timeout(state, remaining).unwrap();
                state = next;
                assert!(!timeout.timed_out() || state.0 != 0);
            }
        }

        fn release(&self) {
            let mut state = self.state.lock().unwrap();
            state.1 = true;
            self.changed.notify_all();
        }
    }

    struct GatedZeroSource {
        catalog: Arc<DatasetCatalog>,
        gate: Arc<DecodeGate>,
    }

    #[allow(
        clippy::result_large_err,
        reason = "the source trait fixes the per-sink scientific source-fault result type"
    )]
    fn write_zero_sinks(
        sinks: &mut [&mut dyn ReservedDecodeSink],
    ) -> Vec<Result<(), DatasetSourceFault>> {
        sinks
            .iter_mut()
            .map(|sink| {
                let sink = &mut **sink;
                let key = sink.resource_key();
                sink.write(&[0])
                    .map_err(|reason| DatasetSourceFault::SinkRejected {
                        key,
                        reason: Box::new(reason),
                    })?;
                sink.finish_with_facts(
                    ResourcePayloadFacts::from_validated_range(0.0, 0.0, true, true).unwrap(),
                )
                .map_err(|reason| DatasetSourceFault::SinkRejected {
                    key,
                    reason: Box::new(reason),
                })
            })
            .collect()
    }

    impl DatasetSource for ZeroSource {
        fn catalog(&self) -> Result<Arc<DatasetCatalog>, DatasetSourceFault> {
            Ok(Arc::clone(&self.catalog))
        }

        #[allow(
            clippy::result_large_err,
            reason = "the trait fixes the per-sink scientific source-fault result type"
        )]
        fn decode_cohort_into(
            &self,
            sinks: &mut [&mut dyn ReservedDecodeSink],
        ) -> Vec<Result<(), DatasetSourceFault>> {
            write_zero_sinks(sinks)
        }
    }

    impl DatasetSource for GatedZeroSource {
        fn catalog(&self) -> Result<Arc<DatasetCatalog>, DatasetSourceFault> {
            Ok(Arc::clone(&self.catalog))
        }

        #[allow(
            clippy::result_large_err,
            reason = "the trait fixes the per-sink scientific source-fault result type"
        )]
        fn decode_cohort_into(
            &self,
            sinks: &mut [&mut dyn ReservedDecodeSink],
        ) -> Vec<Result<(), DatasetSourceFault>> {
            self.gate.enter_and_wait();
            write_zero_sinks(sinks)
        }
    }

    struct NoopLedger;
    struct NoopLease {
        category: CpuLedgerCategory,
        bytes: u64,
    }

    impl CpuByteLease for NoopLease {
        fn category(&self) -> CpuLedgerCategory {
            self.category
        }

        fn reserved_bytes(&self) -> u64 {
            self.bytes
        }
    }

    impl CpuByteLedger for NoopLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            if bytes == 0 {
                return Err(CpuLedgerError::ZeroByteReservation);
            }
            Ok(Box::new(NoopLease { category, bytes }))
        }
    }

    #[test]
    fn explicit_empty_scope_is_complete_but_absent_scope_is_not() {
        assert!(scope_requirements_complete(Some(&[]), |_| false));
        assert!(!scope_requirements_complete(None, |_| true));
    }

    #[test]
    fn playing_can_consume_one_complete_successor_while_the_startup_runway_refills() {
        let source_id = DatasetSourceId::new(111);
        let layer_key = mirante4d_domain::LogicalLayerKey::new(0);
        let layer = DatasetLayer::new(
            layer_key,
            "playback-successor-readiness",
            Shape4D::new(4, 1, 1, 2).unwrap(),
            IntensityDType::Uint8,
            GridToWorld::identity(),
            ResourceValidity::AllValid,
        )
        .unwrap();
        let catalog = Arc::new(
            DatasetCatalog::new(
                "playback-successor-readiness",
                ContentAddressStatus::SessionLocal(source_id),
                vec![layer],
            )
            .unwrap(),
        );
        let source: Arc<dyn DatasetSource> = Arc::new(ZeroSource {
            catalog: Arc::clone(&catalog),
        });
        let config = DatasetRuntimeConfig::new(1 << 20, 1, 8, 8).unwrap();
        let (runtime, _) = <dyn DatasetRuntime>::start(config, move |_| Ok(source)).unwrap();
        let identity = DatasetResourceIdentity::SessionLocal(source_id);
        let resources = (1..=3)
            .flat_map(|timepoint| {
                (0..2).map(move |x| {
                    BrickKey::new(
                        identity,
                        layer_key,
                        TimeIndex::new(timepoint),
                        ScaleLevel::BASE,
                        ResourceRegion::new([0, 0, x], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
                    )
                })
            })
            .collect::<Vec<_>>();
        let mut state = DatasetDemandState::new_with_local_source(
            runtime,
            Arc::new(NoopLedger),
            identity,
            PathBuf::from("playback-successor-readiness"),
            None,
        )
        .unwrap();
        install_prepared_playback_scope_test_fixture(
            &mut state,
            resources.clone(),
            resources.len(),
            2,
        )
        .unwrap();

        assert!(
            !state.scope_resources_complete_with_gpu_residency(SCOPE_PLAYBACK, |key| {
                key.timepoint() == TimeIndex::new(1)
            }),
            "warmup must still wait for the complete multi-frame startup runway"
        );
        assert!(
            state.playback_successor_complete_with_gpu_residency(|key| {
                key.timepoint() == TimeIndex::new(1)
            }),
            "steady-state playback may consume its complete immediate successor"
        );
        state.request_shutdown().unwrap();
    }

    #[test]
    fn transient_capacity_failure_does_not_poison_an_unchanged_scope() {
        let capacity = RuntimeFaultCode::CapacityExceeded {
            category: mirante4d_dataset::CpuLedgerCategory::DecodedResidency,
            requested_bytes: 8,
            available_bytes: 0,
        };
        assert!(!runtime_failure_is_sticky(capacity));
        assert!(runtime_failure_needs_capacity_retry(capacity));
        assert!(runtime_failure_is_sticky(RuntimeFaultCode::DecodeFailed));
        assert!(!runtime_failure_needs_capacity_retry(
            RuntimeFaultCode::DecodeFailed
        ));
    }

    #[test]
    fn completion_leases_support_lossless_renderer_eviction_resolution() {
        let source_id = DatasetSourceId::new(110);
        let layer_key = mirante4d_domain::LogicalLayerKey::new(0);
        let layer = DatasetLayer::new(
            layer_key,
            "renderer-eviction-resolution",
            Shape4D::new(1, 1, 1, 9).unwrap(),
            IntensityDType::Uint8,
            GridToWorld::identity(),
            ResourceValidity::AllValid,
        )
        .unwrap();
        let catalog = Arc::new(
            DatasetCatalog::new(
                "renderer-eviction-resolution",
                ContentAddressStatus::SessionLocal(source_id),
                vec![layer],
            )
            .unwrap(),
        );
        let source: Arc<dyn DatasetSource> = Arc::new(ZeroSource {
            catalog: Arc::clone(&catalog),
        });
        let config = DatasetRuntimeConfig::new(1 << 20, 2, 16, 16).unwrap();
        let (runtime, _) = <dyn DatasetRuntime>::start(config, move |_| Ok(source)).unwrap();
        let identity = DatasetResourceIdentity::SessionLocal(source_id);
        let resources = (0..9)
            .map(|x| {
                BrickKey::new(
                    identity,
                    layer_key,
                    TimeIndex::new(0),
                    ScaleLevel::BASE,
                    ResourceRegion::new([0, 0, x], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let mut state = DatasetDemandState::new_with_local_source(
            runtime,
            Arc::new(NoopLedger),
            identity,
            PathBuf::from("renderer-eviction-resolution"),
            None,
        )
        .unwrap();
        install_prepared_scope_test_fixture_with_required_prefix(
            &mut state,
            SCOPE_CURRENT_3D,
            resources.clone(),
            HISTOGRAM_CPU_RESOURCES_PER_LAYER,
        )
        .unwrap();

        assert_eq!(state.pump_interactive_admission().unwrap(), resources.len());
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut delivered = Vec::new();
        while !state.scope_all_resources_complete(SCOPE_CURRENT_3D) {
            let (drained, installed) = state.drain_runtime_results(16, |_, _| {}).unwrap();
            delivered.extend(installed.leases().iter().map(|lease| lease.key()));
            assert!(
                Instant::now() < deadline,
                "the eviction-resolution fixture did not finish decoding"
            );
            if drained == 0 {
                std::thread::yield_now();
            }
        }
        delivered.sort_unstable();
        assert_eq!(
            delivered, resources,
            "every newly installed CPU lease must cross the completion boundary exactly once"
        );
        assert!(state.scope_all_resources_complete(SCOPE_CURRENT_3D));
        let headless_presentation =
            crate::native_presentation::NativePresentationBridge::unavailable();
        assert!(state.scope_is_installed(SCOPE_CURRENT_3D));
        assert!(!state.scope_is_installed(SCOPE_CURRENT_3D_REFINEMENT));
        assert!(
            crate::workbench_brick_runtime::installed_camera_guard_bodies_are_complete(
                &state,
                &headless_presentation,
            ),
            "a complete current-only progressive state must pass the guard gate"
        );

        let gpu_only = *resources
            .last()
            .expect("the fixture has one key beyond the CPU histogram quota");
        match state.resolve_renderer_eviction(gpu_only).unwrap() {
            RendererEvictionDisposition::Reoffer(lease) => assert_eq!(lease.key(), gpu_only),
            _ => panic!("a retained CPU handle must be reoffered without another decode"),
        }
        assert_eq!(
            state.retire_gpu_resident_current_payloads([gpu_only], |key| key == gpu_only),
            1,
            "the ninth current brick is display-only and may become GPU-authoritative"
        );

        let submissions_before_refetch = state.dispatcher.submitted_requests.len();
        assert!(matches!(
            state.resolve_renderer_eviction(gpu_only).unwrap(),
            RendererEvictionDisposition::Submitted
        ));
        assert_eq!(
            state.dispatcher.submitted_requests.len(),
            submissions_before_refetch + 1
        );
        assert_eq!(
            state.dispatcher.submitted_requests.last().copied(),
            Some((SCOPE_CURRENT_3D, gpu_only, RequestPriority::CurrentView))
        );

        assert!(matches!(
            state.resolve_renderer_eviction(gpu_only).unwrap(),
            RendererEvictionDisposition::Submitted
        ));
        assert_eq!(
            state.dispatcher.submitted_requests.len(),
            submissions_before_refetch + 1,
            "replaying an unacknowledged eviction must reuse the pending decode"
        );
        assert!(
            !state.scope_all_resources_complete_with_gpu_residency(SCOPE_CURRENT_3D, |_| false,),
            "the exact eviction must rewind the full-body readiness proof"
        );
        assert!(
            state.scope_resources_complete(SCOPE_CURRENT_3D),
            "the visible prefix remains complete while its guard tail is missing"
        );
        assert!(
            !crate::workbench_brick_runtime::installed_camera_guard_bodies_are_complete(
                &state,
                &headless_presentation,
            ),
            "headless product readiness must inspect the full CPU guard body"
        );
        let promoted_while_guard_missing = false;
        assert!(!promoted_while_guard_missing);
        assert_eq!(
            state.scope_required_prefix_len(SCOPE_CURRENT_3D),
            HISTOGRAM_CPU_RESOURCES_PER_LAYER,
            "the product gate must leave a missing guard tail unpromoted"
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        while state.retained_leases().payload(gpu_only).is_none() {
            let (drained, installed) = state.drain_runtime_results(1, |_, _| {}).unwrap();
            if drained != 0 {
                assert_eq!(installed.leases().len(), 1);
                assert_eq!(installed.leases()[0].key(), gpu_only);
            }
            assert!(
                Instant::now() < deadline,
                "the exact eviction refetch did not complete"
            );
            if drained == 0 {
                std::thread::yield_now();
            }
        }
        assert!(
            state.scope_all_resources_complete(SCOPE_CURRENT_3D),
            "full-body readiness must resume only after the missing payload returns"
        );
        assert!(
            crate::workbench_brick_runtime::installed_camera_guard_bodies_are_complete(
                &state,
                &headless_presentation,
            ),
            "the product guard gate must reopen after the exact CPU payload returns"
        );

        install_prepared_scope_test_fixture(
            &mut state,
            SCOPE_CURRENT_3D_REFINEMENT,
            resources.clone(),
        )
        .unwrap();
        assert!(
            crate::workbench_brick_runtime::installed_camera_guard_bodies_are_complete(
                &state,
                &headless_presentation,
            ),
            "two installed complete 3D bodies must pass the guard gate"
        );

        assert!(remove_prepared_scope_test_fixture(&mut state, SCOPE_CURRENT_3D).unwrap());
        assert!(!state.scope_is_installed(SCOPE_CURRENT_3D));
        assert!(state.scope_is_installed(SCOPE_CURRENT_3D_REFINEMENT));
        assert!(
            crate::workbench_brick_runtime::installed_camera_guard_bodies_are_complete(
                &state,
                &headless_presentation,
            ),
            "a complete refinement-only progressive state must pass the guard gate"
        );

        install_prepared_scope_test_fixture(&mut state, SCOPE_CURRENT_3D_REFINEMENT, Vec::new())
            .unwrap();
        assert!(state.scope_is_installed(SCOPE_CURRENT_3D_REFINEMENT));
        assert!(state.scope_is_empty(SCOPE_CURRENT_3D_REFINEMENT));
        assert!(
            crate::workbench_brick_runtime::installed_camera_guard_bodies_are_complete(
                &state,
                &headless_presentation,
            ),
            "an installed empty 3D scope is complete rather than absent"
        );
        assert!(matches!(
            state.resolve_renderer_eviction(gpu_only).unwrap(),
            RendererEvictionDisposition::NoLongerDemanded
        ));
        assert!(
            remove_prepared_scope_test_fixture(&mut state, SCOPE_CURRENT_3D_REFINEMENT).unwrap()
        );
        assert!(
            !crate::workbench_brick_runtime::installed_camera_guard_bodies_are_complete(
                &state,
                &headless_presentation,
            ),
            "no installed 3D body cannot authorize resident camera reuse"
        );
        state.request_shutdown().unwrap();
    }

    #[test]
    fn camera_guard_is_admitted_after_visible_scopes_and_promotes_in_constant_time() {
        let source_id = DatasetSourceId::new(93);
        let layer_key = mirante4d_domain::LogicalLayerKey::new(0);
        let layer = DatasetLayer::new(
            layer_key,
            "guard-priority",
            Shape4D::new(1, 1, 1, 8).unwrap(),
            IntensityDType::Uint8,
            GridToWorld::identity(),
            ResourceValidity::AllValid,
        )
        .unwrap();
        let catalog = Arc::new(
            DatasetCatalog::new(
                "guard-priority",
                ContentAddressStatus::SessionLocal(source_id),
                vec![layer],
            )
            .unwrap(),
        );
        let gate = Arc::new(DecodeGate::default());
        let source: Arc<dyn DatasetSource> = Arc::new(GatedZeroSource {
            catalog: Arc::clone(&catalog),
            gate: Arc::clone(&gate),
        });
        let config = DatasetRuntimeConfig::new(1 << 20, 1, 8, 8).unwrap();
        let (runtime, _) = <dyn DatasetRuntime>::start(config, move |_| Ok(source)).unwrap();
        let identity = DatasetResourceIdentity::SessionLocal(source_id);
        let resource = |x| {
            BrickKey::new(
                identity,
                layer_key,
                TimeIndex::new(0),
                ScaleLevel::BASE,
                ResourceRegion::new([0, 0, x], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
            )
        };
        let primary = [resource(0), resource(1)];
        let guard = [resource(2), resource(3)];
        let linked = resource(4);
        let mut state = DatasetDemandState::new_with_local_source(
            runtime,
            Arc::new(NoopLedger),
            identity,
            PathBuf::from("guard-priority"),
            None,
        )
        .unwrap();
        install_prepared_scope_test_fixture_with_required_prefix(
            &mut state,
            SCOPE_CURRENT_3D,
            primary.into_iter().chain(guard).collect(),
            primary.len(),
        )
        .unwrap();
        install_prepared_scope_test_fixture(&mut state, SCOPE_CROSS_SECTION_XY, vec![linked])
            .unwrap();

        assert!(
            state.scope_resources_complete_with_gpu_residency(SCOPE_CURRENT_3D, |key| primary
                .contains(&key),)
        );
        assert!(
            !state.scope_all_resources_complete_with_gpu_residency(SCOPE_CURRENT_3D, |key| primary
                .contains(&key),),
            "a complete visible prefix must not hide a missing camera-guard tail"
        );
        assert!(
            state.scope_all_resources_complete_with_gpu_residency(SCOPE_CURRENT_3D, |_| true,),
            "the full body becomes reusable only when the guard tail is resident"
        );
        let readiness_after_full_body = state.readiness_requirement_visits();
        assert!(
            state.scope_all_resources_complete_with_gpu_residency(SCOPE_CURRENT_3D, |_| panic!(
                "a completed full-body proof must not rescan resources"
            ),)
        );
        assert_eq!(
            state.readiness_requirement_visits(),
            readiness_after_full_body,
            "repeated full-body readiness checks must perform zero additional key visits"
        );
        let readiness_before_promotion = state.readiness_requirement_visits();
        let admission_before_promotion = state.admission_requirement_visits();
        assert!(state.can_commit_complete_scope_prefetch_tail(SCOPE_CURRENT_3D));
        state.commit_complete_scope_prefetch_tail(SCOPE_CURRENT_3D);
        assert_eq!(
            state.readiness_requirement_visits(),
            readiness_before_promotion,
            "promotion advances already-proven cursors without visiting keys"
        );
        assert!(
            state.scope_resources_complete_with_gpu_residency(SCOPE_CURRENT_3D, |_| panic!(
                "the promoted required cursor must already be complete"
            ),)
        );
        assert!(
            state.scope_all_resources_complete_with_gpu_residency(SCOPE_CURRENT_3D, |_| panic!(
                "the full-body cursor must remain complete"
            ),)
        );
        assert_eq!(
            state.readiness_requirement_visits(),
            readiness_before_promotion
        );
        assert_eq!(
            state.admission_requirement_visits(),
            admission_before_promotion
        );
        assert_eq!(
            state.scope_admitted_prefix_len(SCOPE_CURRENT_3D),
            primary.len() + guard.len()
        );

        // Restore an unpromoted boundary to exercise admission ordering in
        // the same compact fixture; this is test setup, not a product path.
        state
            .required_prefix_len_by_scope
            .insert(SCOPE_CURRENT_3D, primary.len());
        state.admission_cursor_by_scope.insert(SCOPE_CURRENT_3D, 0);
        state
            .admission_cursor_by_scope
            .insert(SCOPE_CROSS_SECTION_XY, 0);
        state.dispatcher.submitted_requests.clear();
        assert_eq!(state.pump_interactive_admission().unwrap(), 5);
        assert_eq!(
            state.dispatcher.submitted_requests,
            vec![
                (SCOPE_CURRENT_3D, primary[0], RequestPriority::CurrentView),
                (SCOPE_CURRENT_3D, primary[1], RequestPriority::CurrentView),
                (SCOPE_CROSS_SECTION_XY, linked, RequestPriority::LinkedView),
                (SCOPE_CURRENT_3D, guard[0], RequestPriority::Prefetch),
                (SCOPE_CURRENT_3D, guard[1], RequestPriority::Prefetch),
            ]
        );
        gate.wait_until_entered();
        gate.release();

        let previous_body = state.scope_prepared_body_handle(SCOPE_CURRENT_3D);
        let canonical = Arc::clone(previous_body.canonical());
        let mut reranked = previous_body.ranked().to_vec();
        reranked.reverse();
        let replacement = PreparedDemandRequirements::from_prepared_body_for_test(
            PreparedResourceBody::new(canonical, reranked.into(), None).unwrap(),
            state.scope_admitted_prefix_len(SCOPE_CURRENT_3D),
            state.scope_required_prefix_len(SCOPE_CURRENT_3D),
        );
        let layer_scales = state
            .scope_layer_scales(SCOPE_CURRENT_3D)
            .cloned()
            .expect("the installed current scope has per-layer scales");
        assert!(!state.renderer_requirement_union_needs_preparation());
        let visits_before_rerank = state.readiness_requirement_visits();
        assert!(state.commit_preflighted_scope_replacement(
            SCOPE_CURRENT_3D,
            replacement,
            layer_scales,
        ));
        assert!(
            !state.renderer_requirement_union_needs_preparation(),
            "a ranked-only replacement must not dirty the canonical renderer union"
        );
        assert!(state.scope_all_resources_complete_with_gpu_residency(SCOPE_CURRENT_3D, |_| true,));
        assert_eq!(
            state.readiness_requirement_visits() - visits_before_rerank,
            (primary.len() + guard.len()) as u64,
            "ranked-body replacement must reset the position-based full readiness cursor"
        );
        state.request_shutdown().unwrap();
    }

    #[test]
    fn replacement_plane_does_not_promote_its_predecessor_before_install() {
        let source_id = DatasetSourceId::new(94);
        let layer_key = mirante4d_domain::LogicalLayerKey::new(0);
        let layer = DatasetLayer::new(
            layer_key,
            "replacement-plane-priority",
            Shape4D::new(1, 1, 1, 2).unwrap(),
            IntensityDType::Uint8,
            GridToWorld::identity(),
            ResourceValidity::AllValid,
        )
        .unwrap();
        let catalog = Arc::new(
            DatasetCatalog::new(
                "replacement-plane-priority",
                ContentAddressStatus::SessionLocal(source_id),
                vec![layer],
            )
            .unwrap(),
        );
        let gate = Arc::new(DecodeGate::default());
        let source: Arc<dyn DatasetSource> = Arc::new(GatedZeroSource {
            catalog: Arc::clone(&catalog),
            gate: Arc::clone(&gate),
        });
        let config = DatasetRuntimeConfig::new(1 << 20, 1, 1, 1).unwrap();
        let (runtime, _) = <dyn DatasetRuntime>::start(config, move |_| Ok(source)).unwrap();
        let identity = DatasetResourceIdentity::SessionLocal(source_id);
        let predecessor = |x| {
            BrickKey::new(
                identity,
                layer_key,
                TimeIndex::new(0),
                ScaleLevel::BASE,
                ResourceRegion::new([0, 0, x], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
            )
        };
        let first = predecessor(0);
        let obsolete_successor = predecessor(1);
        let mut state = DatasetDemandState::new_with_local_source(
            runtime,
            Arc::new(NoopLedger),
            identity,
            PathBuf::from("replacement-plane-priority"),
            None,
        )
        .unwrap();
        install_prepared_scope_test_fixture(
            &mut state,
            SCOPE_CROSS_SECTION_XY,
            vec![first, obsolete_successor],
        )
        .unwrap();

        assert_eq!(state.pump_interactive_admission().unwrap(), 1);
        assert_eq!(
            state.dispatcher.submitted_requests,
            vec![(SCOPE_CROSS_SECTION_XY, first, RequestPriority::LinkedView)]
        );
        gate.wait_until_entered();

        let revision = RenderIntentRevision::initial();
        assert!(state.observe_visible_intent(revision, Some(SCOPE_CROSS_SECTION_XY)));
        let foreground_submissions_before = state
            .dispatcher
            .submitted_requests
            .iter()
            .filter(|(_, _, priority)| *priority == RequestPriority::CurrentView)
            .count();
        assert_eq!(state.pump_interactive_admission().unwrap(), 0);
        gate.release();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let (drained, _) = state.drain_runtime_results(1, |_, _| {}).unwrap();
            if drained == 1 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the predecessor decode did not free the bounded queue"
            );
            std::thread::yield_now();
        }
        assert_eq!(state.pump_interactive_admission().unwrap(), 1);
        assert_eq!(
            state.dispatcher.submitted_requests.last().copied(),
            Some((
                SCOPE_CROSS_SECTION_XY,
                obsolete_successor,
                RequestPriority::LinkedView,
            )),
            "the old body's next key must not become foreground while its replacement plan is pending"
        );
        assert_eq!(
            state
                .dispatcher
                .submitted_requests
                .iter()
                .filter(|(_, _, priority)| *priority == RequestPriority::CurrentView)
                .count(),
            foreground_submissions_before
        );
        state.request_shutdown().unwrap();
    }

    #[test]
    fn bounded_queue_refill_visits_large_requirements_linearly() {
        let source_id = DatasetSourceId::new(91);
        let layer = DatasetLayer::new(
            mirante4d_domain::LogicalLayerKey::new(0),
            "linear-admission",
            Shape4D::new(1, 1, 1, 100).unwrap(),
            IntensityDType::Uint8,
            GridToWorld::identity(),
            ResourceValidity::AllValid,
        )
        .unwrap();
        let catalog = Arc::new(
            DatasetCatalog::new(
                "linear-admission",
                ContentAddressStatus::SessionLocal(source_id),
                vec![layer],
            )
            .unwrap(),
        );
        let source: Arc<dyn DatasetSource> = Arc::new(ZeroSource {
            catalog: Arc::clone(&catalog),
        });
        let config = DatasetRuntimeConfig::new(1 << 20, 2, 8, 8).unwrap();
        let (runtime, opened_catalog) = <dyn DatasetRuntime>::start(config, move |_| Ok(source))
            .expect("the bounded production runtime starts");
        assert!(Arc::ptr_eq(&opened_catalog, &catalog));
        let identity = DatasetResourceIdentity::SessionLocal(source_id);
        let resources = (0..100)
            .map(|x| {
                BrickKey::new(
                    identity,
                    mirante4d_domain::LogicalLayerKey::new(0),
                    TimeIndex::new(0),
                    ScaleLevel::BASE,
                    ResourceRegion::new([0, 0, x], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let mut state = DatasetDemandState::new_with_local_source(
            runtime,
            Arc::new(NoopLedger),
            identity,
            PathBuf::from("linear-admission"),
            None,
        )
        .unwrap();
        install_prepared_scope_test_fixture(&mut state, SCOPE_CURRENT_3D, resources.clone())
            .unwrap();
        let mut reranked = resources;
        reranked.reverse();
        let expected_gpu_priority = reranked.clone();
        install_prepared_scope_test_fixture(&mut state, SCOPE_CURRENT_3D, reranked).unwrap();
        let canonical = state.scope_requirement_handle(SCOPE_CURRENT_3D);
        assert_eq!(
            state.scope_gpu_priority_handle(SCOPE_CURRENT_3D).as_ref(),
            expected_gpu_priority.as_slice(),
            "GPU upload priority must follow the worker-prepared contribution order"
        );
        let retained_union_work = state.retained_leases().prepared_requirement_swap_work();
        state.pump_interactive_admission().unwrap();
        let authority_work_after_current = state.cpu_authority_requirement_visits();
        assert_eq!(authority_work_after_current.0, 0);
        assert!(
            authority_work_after_current.1 <= HISTOGRAM_CPU_RESOURCES_PER_LAYER as u64,
            "histogram authority must stop at its per-layer sample quota"
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut complete = false;
        while !complete {
            let (drained, _) = state.drain_runtime_results(1, |_, _| {}).unwrap();
            if state.dispatcher().admission_blocked() {
                state.pump_interactive_admission().unwrap();
            }
            if drained > 0 {
                complete = state.scope_complete(SCOPE_CURRENT_3D);
            }
            assert!(Instant::now() < deadline, "bounded queue refill timed out");
            if drained == 0 {
                std::thread::yield_now();
            }
        }

        assert_eq!(
            state
                .dispatcher()
                .diagnostics()
                .unwrap()
                .submitted_requests(),
            100
        );
        assert!(
            state.admission_requirement_visits() <= 126,
            "cursor refill revisited too many admitted requirements: {}",
            state.admission_requirement_visits()
        );
        assert_eq!(
            state.retained_leases().prepared_requirement_swap_work(),
            retained_union_work,
            "unchanged completion pumping must not publish another retained union"
        );
        assert!(
            state.readiness_requirement_visits() <= 201,
            "monotonic readiness revisited too many requirements: {}",
            state.readiness_requirement_visits()
        );

        // A linked panel may upload a key before the 3D target observes it.
        // When linked CPU authority later disappears, retire that shared
        // handle directly from the exact authority delta without re-I/O.
        let shared = canonical[51];
        install_prepared_scope_test_fixture(&mut state, SCOPE_CROSS_SECTION_XY, vec![shared])
            .unwrap();
        assert_eq!(
            state.retire_gpu_resident_current_payloads([shared], |_| true),
            0,
            "linked-view CPU authority protects the shared handle"
        );
        assert!(state.retained_leases().payload(shared).is_some());
        let authority_work_after_linked = state.cpu_authority_requirement_visits();
        assert_eq!(authority_work_after_linked.0, 1);
        assert_eq!(
            authority_work_after_linked.1, authority_work_after_current.1,
            "linked-panel changes must not rescan the 3D histogram authority"
        );
        install_prepared_scope_test_fixture(&mut state, SCOPE_CROSS_SECTION_XY, Vec::new())
            .unwrap();
        let submissions_before_release = state
            .dispatcher()
            .diagnostics()
            .unwrap()
            .submitted_requests();
        assert_eq!(
            state.retire_released_cpu_authority_payloads(|key| key == shared),
            1
        );
        assert!(state.retained_leases().payload(shared).is_none());
        assert_eq!(
            state
                .dispatcher()
                .diagnostics()
                .unwrap()
                .submitted_requests(),
            submissions_before_release,
            "resident linked-panel reuse must not submit another read/decode"
        );

        let staged_resources = canonical[..99].to_vec();
        let mut staged_reservations =
            PreparedAllocationReservations::new(state.cpu_ledger.as_ref());
        let (staged_plan, _) =
            crate::dataset_demand_plan::PreparedProgressiveDatasetDemandPlan::from_planning_accounted(
                crate::dataset_demand_plan::ProgressiveDatasetDemandPlanning {
                    plan: crate::dataset_demand_plan::ProgressiveDatasetDemandPlan {
                        ideal_scale: ScaleLevel::BASE,
                        ideal_layer_scales: BTreeMap::from([(
                            mirante4d_domain::LogicalLayerKey::new(0),
                            ScaleLevel::BASE,
                        )]),
                        target: DatasetDemandPlan {
                            scale: ScaleLevel::BASE,
                            layer_scales: BTreeMap::from([(
                                mirante4d_domain::LogicalLayerKey::new(0),
                                ScaleLevel::BASE,
                            )]),
                            resources: staged_resources,
                            payload_bytes: mirante4d_render_wgpu::FrameBudget::interactive()
                                .payload_upload_bytes()
                                .saturating_add(1),
                            playback_downshifted: false,
                            covers_full_volume: false,
                            primary_resource_count: 98,
                            playback_resource_count: 99,
                        },
                        coarse: None,
                        navigation_candidates: Vec::new(),
                    },
                    playback_timepoint_count: 0,
                    candidates_visited: 99,
                    reuse_envelope: None,
                    scratch_charges: Vec::new(),
                },
                &mut staged_reservations,
            )
            .unwrap();
        staged_plan
            .target
            .requirements
            .body()
            .attach_charge(staged_reservations.finish())
            .unwrap();
        let current_changed =
            install_prepared_progressive_test_fixture(&mut state, staged_plan, false, true)
                .unwrap();
        assert!(
            !current_changed,
            "preserving a complete presentation must not publish the staged cohort as current"
        );
        assert!(
            state.staging_current_refinement(),
            "a CPU-ready refinement larger than one upload burst must remain staged until the hidden GPU target proves an exact frame"
        );
        assert!(!state.scope_complete(SCOPE_CURRENT_3D));
        assert_eq!(
            state.scope_requirements(SCOPE_CURRENT_3D_REFINEMENT).len(),
            99
        );
        let staged_canonical = state.scope_requirement_handle(SCOPE_CURRENT_3D_REFINEMENT);
        let staged_priority = state.scope_gpu_priority_handle(SCOPE_CURRENT_3D_REFINEMENT);
        assert!(
            state.scope_all_resources_complete_with_gpu_residency(
                SCOPE_CURRENT_3D_REFINEMENT,
                |key| key == shared,
            ),
            "the staged full-body proof includes the one GPU-only shared brick"
        );
        let readiness_visits_before_staged_promotion = state.readiness_requirement_visits();
        let pending_guard = staged_priority[98];
        state
            .admission_cursor_by_scope
            .insert(SCOPE_CURRENT_3D_REFINEMENT, staged_priority.len());
        state
            .dispatcher
            .submit_if_missing(
                SCOPE_CURRENT_3D_REFINEMENT,
                pending_guard,
                RequestPriority::Prefetch,
                false,
                Some(98),
            )
            .unwrap()
            .expect("the staged guard owns one pending waiter before atomic promotion");
        let submissions_before_swap = state
            .dispatcher()
            .diagnostics()
            .unwrap()
            .submitted_requests();
        let mut promotion_targets = ScopeReconciliationTargets::default();
        assert!(state.prepare_staged_current_promotion_scope_targets(&mut promotion_targets));
        let prepared_promotion = state
            .prepare_scope_reconciliation(promotion_targets)
            .unwrap();
        state
            .commit_prepared_scope_reconciliation(prepared_promotion)
            .unwrap();
        state.commit_reconciled_gpu_prefilled_staged_current_plan();
        assert_eq!(
            state.staged_promotion_work(),
            (1, 0),
            "atomic staged promotion must move prepared map state without visiting or rebuilding its requirement body"
        );
        assert!(Arc::ptr_eq(
            &staged_canonical,
            &state.scope_requirement_handle(SCOPE_CURRENT_3D)
        ));
        assert!(Arc::ptr_eq(
            &staged_priority,
            &state.scope_gpu_priority_handle(SCOPE_CURRENT_3D)
        ));
        assert!(
            state.scope_all_resources_complete_with_gpu_residency(SCOPE_CURRENT_3D, |_| panic!(
                "staged promotion must move the completed full-body cursor"
            ),)
        );
        assert_eq!(
            state.readiness_requirement_visits(),
            readiness_visits_before_staged_promotion,
            "staged promotion must move readiness without rescanning the ranked body"
        );
        assert_eq!(
            state.scope_admitted_prefix_len(SCOPE_CURRENT_3D),
            98,
            "a canceled staged guard waiter must be exactly re-admissible after scope promotion"
        );
        state
            .pump_interactive_admission_with_gpu_residency(|_| true)
            .unwrap();
        assert_eq!(
            state
                .dispatcher()
                .diagnostics()
                .unwrap()
                .submitted_requests(),
            submissions_before_swap,
            "the atomic target swap must reuse staged bodies without another read/decode"
        );

        let mut prepared_ranked = canonical[..98].to_vec();
        prepared_ranked.reverse();
        let mut prepared_reservations =
            PreparedAllocationReservations::new(state.cpu_ledger.as_ref());
        let (prepared, _) =
            crate::dataset_demand_plan::PreparedProgressiveDatasetDemandPlan::from_planning_accounted(
                crate::dataset_demand_plan::ProgressiveDatasetDemandPlanning {
                    plan: crate::dataset_demand_plan::ProgressiveDatasetDemandPlan {
                        ideal_scale: ScaleLevel::BASE,
                        ideal_layer_scales: BTreeMap::from([(
                            mirante4d_domain::LogicalLayerKey::new(0),
                            ScaleLevel::BASE,
                        )]),
                        target: DatasetDemandPlan {
                            scale: ScaleLevel::BASE,
                            layer_scales: BTreeMap::from([(
                                mirante4d_domain::LogicalLayerKey::new(0),
                                ScaleLevel::BASE,
                            )]),
                            resources: prepared_ranked,
                            payload_bytes: 98,
                            playback_downshifted: false,
                            covers_full_volume: false,
                            primary_resource_count: 98,
                            playback_resource_count: 98,
                        },
                        coarse: None,
                        navigation_candidates: Vec::new(),
                    },
                    playback_timepoint_count: 0,
                    candidates_visited: 98,
                    reuse_envelope: None,
                    scratch_charges: Vec::new(),
                },
                &mut prepared_reservations,
            )
            .unwrap();
        prepared
            .target
            .requirements
            .body()
            .attach_charge(prepared_reservations.finish())
            .unwrap();
        let prepared_canonical = Arc::clone(prepared.target.requirements.canonical());
        let prepared_priority = Arc::clone(prepared.target.requirements.ranked());
        let prepared_scope_work_before = state.prepared_scope_commit_work();
        let retained_swap_work_before = state.retained_leases().prepared_requirement_swap_work();
        install_prepared_progressive_test_fixture(&mut state, prepared, false, false).unwrap();
        let prepared_scope_work_after = state.prepared_scope_commit_work();
        assert_eq!(
            prepared_scope_work_after.0 - prepared_scope_work_before.0,
            1
        );
        assert_eq!(
            prepared_scope_work_after.1 - prepared_scope_work_before.1,
            0
        );
        assert!(Arc::ptr_eq(
            &prepared_canonical,
            &state.scope_requirement_handle(SCOPE_CURRENT_3D)
        ));
        assert!(Arc::ptr_eq(
            &prepared_priority,
            &state.scope_gpu_priority_handle(SCOPE_CURRENT_3D)
        ));
        let retained_swap_work_after = state.retained_leases().prepared_requirement_swap_work();
        assert_eq!(retained_swap_work_after.0 - retained_swap_work_before.0, 1);
        assert_eq!(
            retained_swap_work_after.1 - retained_swap_work_before.1,
            0,
            "prepared retained-union commit must compare Arc identity without visiting target keys"
        );
        assert_eq!(
            prepared_canonical.as_ref(),
            state.renderer_requirement_handle().requirements.as_ref()
        );
        state.request_shutdown().unwrap();
    }

    #[test]
    fn rejected_union_after_multi_scope_preparation_preserves_old_work_and_maps() {
        let source_id = DatasetSourceId::new(109);
        let layer = DatasetLayer::new(
            mirante4d_domain::LogicalLayerKey::new(0),
            "atomic-reconciliation",
            Shape4D::new(1, 1, 1, 2).unwrap(),
            IntensityDType::Uint8,
            GridToWorld::identity(),
            ResourceValidity::AllValid,
        )
        .unwrap();
        let catalog = Arc::new(
            DatasetCatalog::new(
                "atomic-reconciliation",
                ContentAddressStatus::SessionLocal(source_id),
                vec![layer],
            )
            .unwrap(),
        );
        let source: Arc<dyn DatasetSource> = Arc::new(ZeroSource {
            catalog: Arc::clone(&catalog),
        });
        let config = DatasetRuntimeConfig::new(1 << 20, 2, 8, 8).unwrap();
        let (runtime, _) = <dyn DatasetRuntime>::start(config, move |_| Ok(source)).unwrap();
        let identity = DatasetResourceIdentity::SessionLocal(source_id);
        let resource = |x| {
            BrickKey::new(
                identity,
                mirante4d_domain::LogicalLayerKey::new(0),
                TimeIndex::new(0),
                ScaleLevel::BASE,
                ResourceRegion::new([0, 0, x], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
            )
        };
        let current = resource(0);
        let cross = resource(1);
        let mut state = DatasetDemandState::new_with_local_source(
            runtime,
            Arc::new(NoopLedger),
            identity,
            PathBuf::from("atomic-reconciliation"),
            None,
        )
        .unwrap();
        install_prepared_scope_test_fixture(&mut state, SCOPE_CURRENT_3D, vec![current]).unwrap();
        install_prepared_scope_test_fixture(&mut state, SCOPE_CROSS_SECTION_XY, vec![cross])
            .unwrap();
        state
            .dispatcher
            .submit_if_missing(
                SCOPE_CURRENT_3D,
                current,
                RequestPriority::CurrentView,
                false,
                None,
            )
            .unwrap();
        state
            .dispatcher
            .submit_if_missing(
                SCOPE_CROSS_SECTION_XY,
                cross,
                RequestPriority::LinkedView,
                false,
                None,
            )
            .unwrap();
        let old_current = state.scope_requirement_handle(SCOPE_CURRENT_3D);
        let old_cross = state.scope_requirement_handle(SCOPE_CROSS_SECTION_XY);
        let old_union = state.renderer_requirement_handle();
        let old_pending = state.dispatcher.pending_by_key.clone();

        let mut targets = ScopeReconciliationTargets::default();
        targets.remove(SCOPE_CURRENT_3D);
        targets.remove(SCOPE_CROSS_SECTION_XY);
        let prepared = state.prepare_scope_reconciliation(targets).unwrap();
        assert_eq!(prepared.cancellation_tickets.len(), 2);
        let wrong_previous = Arc::from(old_union.requirements.to_vec());
        let empty = Arc::default();
        assert_eq!(
            state.preflight_prepared_renderer_requirement_update(&wrong_previous, &empty),
            Err(RetainedLeaseError::PreparedRequirementsChanged)
        );
        drop(prepared);

        assert_eq!(state.dispatcher.pending_by_key, old_pending);
        assert!(Arc::ptr_eq(
            &state.scope_requirement_handle(SCOPE_CURRENT_3D),
            &old_current
        ));
        assert!(Arc::ptr_eq(
            &state.scope_requirement_handle(SCOPE_CROSS_SECTION_XY),
            &old_cross
        ));
        assert!(Arc::ptr_eq(
            &state.renderer_requirement_handle().requirements,
            &old_union.requirements
        ));
        assert_eq!(
            state
                .dispatcher()
                .diagnostics()
                .unwrap()
                .cancelled_requests(),
            0,
            "a later union rejection must not cancel useful old waiters"
        );
        state.request_shutdown().unwrap();
    }
}
