//! Sole composition-side dispatcher for the unified dataset runtime.
//!
//! It owns only bounded ticket correlation and cancellation generations. Pixel
//! payloads move directly from runtime completions into the renderer bridge.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
};

use mirante4d_dataset::{CpuByteLedger, DatasetResourceIdentity, DatasetResourceKey};
use mirante4d_dataset_runtime::{
    AccountedCpuLease, CancellationGeneration, DatasetRuntime, DatasetRuntimeDiagnostics,
    RequestPriority, RequestTicket, ResourceRequest, RuntimeFault, RuntimeFaultCode,
    RuntimeOutcome, RuntimeRequestId,
};
use mirante4d_domain::ScaleLevel;
use mirante4d_renderer::CurrentLeaseBridge;

use crate::dataset_demand_plan::DatasetDemandPlan;

pub(crate) const SCOPE_CURRENT_3D: u64 = 1;
pub(crate) const SCOPE_CROSS_SECTION_XY: u64 = 2;
pub(crate) const SCOPE_CROSS_SECTION_XZ: u64 = 3;
pub(crate) const SCOPE_CROSS_SECTION_YZ: u64 = 4;
pub(crate) const SCOPE_PLAYBACK: u64 = 5;
pub(crate) const SCOPE_ANALYSIS: u64 = 6;

const INTERACTIVE_DEMAND_SCOPES: [u64; 5] = [
    SCOPE_CURRENT_3D,
    SCOPE_CROSS_SECTION_XY,
    SCOPE_CROSS_SECTION_XZ,
    SCOPE_CROSS_SECTION_YZ,
    SCOPE_PLAYBACK,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PendingKey {
    scope: u64,
    resource: DatasetResourceKey,
}

/// One bounded poll owner. No other application service may call
/// `DatasetRuntime::poll` directly.
pub(crate) struct DatasetRequestDispatcher {
    runtime: Arc<dyn DatasetRuntime>,
    generations: BTreeMap<u64, CancellationGeneration>,
    pending_by_id: HashMap<RuntimeRequestId, PendingKey>,
    pending_by_key: HashMap<PendingKey, RequestTicket>,
    failed_by_scope: BTreeMap<u64, BTreeMap<DatasetResourceKey, RuntimeFault>>,
    admission_blocked: bool,
    last_fault: Option<RuntimeFault>,
}

/// Small, payload-free composition state for one opened source.
pub(crate) struct DatasetDemandState {
    dispatcher: DatasetRequestDispatcher,
    cpu_ledger: Arc<dyn CpuByteLedger>,
    resource_identity: DatasetResourceIdentity,
    selected_path: PathBuf,
    requirements_by_scope: BTreeMap<u64, Arc<[DatasetResourceKey]>>,
    layer_scales_by_scope: BTreeMap<u64, BTreeMap<mirante4d_domain::LogicalLayerKey, ScaleLevel>>,
    current_scale: ScaleLevel,
    four_panel: bool,
    last_plan_error: Option<String>,
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
        self.pending_by_id.retain(|_, key| key.scope != scope);
        self.failed_by_scope.remove(&scope);
        Ok(next)
    }

    /// Submits at most one waiter for a `(scope, resource)` pair. Runtime-level
    /// deduplication still merges identical resources demanded by other scopes.
    pub(crate) fn submit_if_missing(
        &mut self,
        scope: u64,
        resource: DatasetResourceKey,
        priority: RequestPriority,
        already_resident: bool,
    ) -> Result<Option<RequestTicket>, RuntimeFault> {
        let pending_key = PendingKey { scope, resource };
        if already_resident
            || self.pending_by_key.contains_key(&pending_key)
            || self
                .failed_by_scope
                .get(&scope)
                .is_some_and(|failed| failed.contains_key(&resource))
        {
            return Ok(None);
        }
        let generation = self.generation(scope);
        let ticket = self
            .runtime
            .submit(ResourceRequest::new(resource, priority, generation))?;
        self.pending_by_id.insert(ticket.id(), pending_key);
        self.pending_by_key.insert(pending_key, ticket);
        Ok(Some(ticket))
    }

    pub(crate) fn drain(
        &mut self,
        maximum: usize,
        mut accept: impl FnMut(RequestTicket, RuntimeOutcome),
    ) -> Result<usize, RuntimeFault> {
        let completions = self.runtime.poll(maximum)?;
        let count = completions.len();
        for completion in completions {
            let ticket = completion.ticket();
            if let Some(key) = self.pending_by_id.remove(&ticket.id()) {
                self.pending_by_key.remove(&key);
            }
            let current = self.generation(ticket.generation().scope());
            if !ticket.is_current(current).map_err(RuntimeFault::new)? {
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
                    }
                    if ticket.generation().scope() != SCOPE_ANALYSIS {
                        self.last_fault = Some(fault.clone());
                    }
                }
            }
            accept(ticket, outcome);
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

    pub(crate) fn has_pending_work(&self) -> bool {
        self.admission_blocked || !self.pending_by_id.is_empty()
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
    pub(crate) fn new(
        runtime: Arc<dyn DatasetRuntime>,
        cpu_ledger: Arc<dyn CpuByteLedger>,
        resource_identity: DatasetResourceIdentity,
        selected_path: PathBuf,
    ) -> Self {
        Self {
            dispatcher: DatasetRequestDispatcher::new(runtime),
            cpu_ledger,
            resource_identity,
            selected_path,
            requirements_by_scope: BTreeMap::new(),
            layer_scales_by_scope: BTreeMap::new(),
            current_scale: ScaleLevel::BASE,
            four_panel: false,
            last_plan_error: None,
        }
    }

    pub(crate) fn selected_path(&self) -> &Path {
        &self.selected_path
    }

    pub(crate) fn dispatcher(&self) -> &DatasetRequestDispatcher {
        &self.dispatcher
    }

    pub(crate) fn cpu_ledger(&self) -> &dyn CpuByteLedger {
        self.cpu_ledger.as_ref()
    }

    pub(crate) fn cpu_ledger_arc(&self) -> Arc<dyn CpuByteLedger> {
        Arc::clone(&self.cpu_ledger)
    }

    pub(crate) const fn resource_identity(&self) -> DatasetResourceIdentity {
        self.resource_identity
    }

    pub(crate) fn dispatcher_mut(&mut self) -> &mut DatasetRequestDispatcher {
        &mut self.dispatcher
    }

    pub(crate) const fn current_scale(&self) -> ScaleLevel {
        self.current_scale
    }

    pub(crate) fn install_current_plan(
        &mut self,
        plan: DatasetDemandPlan,
        four_panel: bool,
    ) -> Result<bool, RuntimeFault> {
        let DatasetDemandPlan {
            scale,
            layer_scales,
            resources,
            decoded_bytes: _,
        } = plan;
        let changed = self
            .requirements_by_scope
            .get(&SCOPE_CURRENT_3D)
            .is_none_or(|current| current.as_ref() != resources.as_slice())
            || self.layer_scales_by_scope.get(&SCOPE_CURRENT_3D) != Some(&layer_scales)
            || self.current_scale != scale
            || self.four_panel != four_panel;
        if !changed {
            self.last_plan_error = None;
            return Ok(false);
        }
        let resources: Arc<[DatasetResourceKey]> = resources.into();
        self.dispatcher.advance_scope(SCOPE_CURRENT_3D)?;
        self.requirements_by_scope
            .insert(SCOPE_CURRENT_3D, Arc::clone(&resources));
        self.layer_scales_by_scope
            .insert(SCOPE_CURRENT_3D, layer_scales);
        if !four_panel {
            for scope in [
                SCOPE_CROSS_SECTION_XY,
                SCOPE_CROSS_SECTION_XZ,
                SCOPE_CROSS_SECTION_YZ,
            ] {
                self.dispatcher.advance_scope(scope)?;
                self.requirements_by_scope.remove(&scope);
                self.layer_scales_by_scope.remove(&scope);
            }
        }
        self.current_scale = scale;
        self.four_panel = four_panel;
        self.last_plan_error = None;
        Ok(true)
    }

    pub(crate) fn record_plan_error(&mut self, error: impl Into<String>) {
        self.last_plan_error = Some(error.into());
    }

    pub(crate) fn last_plan_error(&self) -> Option<&str> {
        self.last_plan_error.as_deref()
    }

    pub(crate) fn set_scope_requirements(
        &mut self,
        scope: u64,
        resources: Vec<DatasetResourceKey>,
    ) -> Result<bool, RuntimeFault> {
        let layer_scales = requirement_layer_scales(&resources)?;
        if self
            .requirements_by_scope
            .get(&scope)
            .is_some_and(|current| current.as_ref() == resources.as_slice())
        {
            return Ok(false);
        }
        self.dispatcher.advance_scope(scope)?;
        self.requirements_by_scope.insert(scope, resources.into());
        self.layer_scales_by_scope.insert(scope, layer_scales);
        Ok(true)
    }

    pub(crate) fn renderer_requirements(&self) -> Vec<DatasetResourceKey> {
        let mut resources = self
            .requirements_by_scope
            .values()
            .flat_map(|resources| resources.iter().copied())
            .collect::<Vec<_>>();
        resources.sort_unstable();
        resources.dedup();
        resources
    }

    pub(crate) fn scope_requirements(&self, scope: u64) -> &[DatasetResourceKey] {
        self.requirements_by_scope
            .get(&scope)
            .map_or(&[], |resources| resources.as_ref())
    }

    pub(crate) fn scope_layer_scale(
        &self,
        scope: u64,
        layer: mirante4d_domain::LogicalLayerKey,
    ) -> Option<ScaleLevel> {
        self.layer_scales_by_scope
            .get(&scope)
            .and_then(|scales| scales.get(&layer))
            .copied()
    }

    pub(crate) fn submit_scope(
        &mut self,
        scope: u64,
        priority: RequestPriority,
        leases: &CurrentLeaseBridge,
    ) -> Result<usize, RuntimeFault> {
        if let Some(fault) = self.dispatcher.scope_failure(scope) {
            return Err(fault.clone());
        }
        let resources = self
            .requirements_by_scope
            .get(&scope)
            .cloned()
            .unwrap_or_default();
        let mut submitted = 0;
        for resource in resources.iter().copied() {
            let already_ready = leases.payload(resource).is_some();
            match self
                .dispatcher
                .submit_if_missing(scope, resource, priority, already_ready)
            {
                Ok(Some(_)) => submitted += 1,
                Ok(None) => {}
                Err(fault) if fault.code() == RuntimeFaultCode::QueueFull => {
                    self.dispatcher.mark_admission_blocked();
                    break;
                }
                Err(fault) => return Err(fault),
            }
        }
        Ok(submitted)
    }

    pub(crate) fn begin_submission_pass(&mut self) {
        self.dispatcher.begin_submission_pass();
    }

    /// Quarantines every interactive demand owned by this source without
    /// shutting down the runtime. The CPU ledger must remain usable because
    /// current-source reverification scans against that same bounded ledger.
    pub(crate) fn cancel_and_clear_interactive_demand(&mut self) -> Result<(), RuntimeFault> {
        self.requirements_by_scope.clear();
        self.layer_scales_by_scope.clear();
        self.dispatcher.begin_submission_pass();
        self.last_plan_error = None;

        let mut first_fault = None;
        for scope in INTERACTIVE_DEMAND_SCOPES {
            if let Err(fault) = self.dispatcher.advance_scope(scope)
                && first_fault.is_none()
            {
                first_fault = Some(fault);
            }
        }
        let _ = self.dispatcher.take_last_fault();

        match first_fault {
            Some(fault) => Err(fault),
            None => Ok(()),
        }
    }

    pub(crate) fn scope_complete(&self, scope: u64, leases: &CurrentLeaseBridge) -> bool {
        scope_requirements_complete(
            self.requirements_by_scope
                .get(&scope)
                .map(|resources| resources.as_ref()),
            |resource| leases.payload(resource).is_some(),
        )
    }

    pub(crate) fn scope_is_empty(&self, scope: u64) -> bool {
        self.requirements_by_scope
            .get(&scope)
            .is_some_and(|resources| resources.is_empty())
    }

    pub(crate) fn request_shutdown(&self) -> Result<(), RuntimeFault> {
        self.dispatcher.request_shutdown()
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

fn scope_requirements_complete(
    resources: Option<&[DatasetResourceKey]>,
    mut ready: impl FnMut(DatasetResourceKey) -> bool,
) -> bool {
    resources.is_some_and(|resources| resources.iter().copied().all(&mut ready))
}

fn requirement_layer_scales(
    resources: &[DatasetResourceKey],
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
mod tests {
    use super::*;

    #[test]
    fn explicit_empty_scope_is_complete_but_absent_scope_is_not() {
        assert!(scope_requirements_complete(Some(&[]), |_| false));
        assert!(!scope_requirements_complete(None, |_| true));
    }

    #[test]
    fn transient_capacity_failure_does_not_poison_an_unchanged_scope() {
        assert!(!runtime_failure_is_sticky(
            RuntimeFaultCode::CapacityExceeded {
                category: mirante4d_dataset::CpuLedgerCategory::DecodedResidency,
                requested_bytes: 8,
                available_bytes: 0,
            }
        ));
        assert!(runtime_failure_is_sticky(RuntimeFaultCode::DecodeFailed));
    }
}
