//! Unified interactive dataset demand and completion delivery.

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui;
use mirante4d_application::ApplicationCommand;
use mirante4d_dataset::{CpuLedgerCategory, CpuLedgerError};
use mirante4d_dataset_runtime::{RuntimeFault, RuntimeFaultCode};
use mirante4d_domain::{CameraView, TimeIndex, ViewerLayout};
use mirante4d_render_api::{MAX_RENDER_REQUIREMENTS, PreparedRenderRequirements};
use mirante4d_render_wgpu::PreparedStaticPresentationLayout;

use crate::{
    BACKGROUND_WORK_REPAINT_INTERVAL, DeterministicFailureLatch, FrameCompleteness,
    FrameFailureKind, LodDecisionReason, MiranteWorkbenchApp, RenderBackend, application_view,
    camera_demand_cache::{
        CameraDemandRequest, CrossSectionDemandRequest, Current3dDemandBaselines,
        Current3dDemandRequest, PreparedRendererRequirementUpdate, PreparedVisibleDemand,
        ScopeDemandBaseline,
    },
    dataset_demand_plan::{
        DatasetDemandPlanCapacityError, DatasetDemandPlanLimits, current_3d_projected_layer_scales,
        sampling_footprint_class,
    },
    dataset_requests::{
        DatasetDemandState, SCOPE_ANALYSIS, SCOPE_CROSS_SECTION_XY, SCOPE_CROSS_SECTION_XZ,
        SCOPE_CROSS_SECTION_YZ, SCOPE_CURRENT_3D, SCOPE_CURRENT_3D_REFINEMENT, SCOPE_PLAYBACK,
        ScopeReconciliationTargets,
    },
    display_refresh::render_backend_for_mode,
    native_presentation::NativePresentationBridge,
    product_render_intent::PRODUCT_RENDER_RESOURCE_LIMIT,
    viewer_layout::PanelId,
};

fn pump_interactive_admission_with_renderer(
    dataset: &mut DatasetDemandState,
    native: &NativePresentationBridge,
) -> Result<usize, RuntimeFault> {
    let Some(product) = native.product_gpu.as_ref() else {
        return dataset.pump_interactive_admission();
    };
    let epoch = product.renderer.residency_invalidation_epoch();
    dataset.reconcile_gpu_residency_invalidation(epoch.get(), |key| {
        product.renderer.resource_is_resident(key)
    });
    dataset
        .retire_released_cpu_authority_payloads(|key| product.renderer.resource_is_resident(key));
    let submitted = dataset.pump_interactive_admission_with_gpu_residency(|key| {
        product.renderer.resource_is_resident(key)
    })?;
    debug_assert_eq!(product.renderer.residency_invalidation_epoch(), epoch);
    Ok(submitted)
}

fn scope_complete_with_renderer(
    dataset: &DatasetDemandState,
    native: &NativePresentationBridge,
    scope: u64,
) -> bool {
    let Some(product) = native.product_gpu.as_ref() else {
        return dataset.scope_complete(scope);
    };
    let epoch = product.renderer.residency_invalidation_epoch();
    let complete = dataset.scope_complete_with_gpu_residency_at_invalidation_epoch(
        scope,
        epoch.get(),
        |key| product.renderer.resource_is_resident(key),
    );
    complete && product.renderer.residency_invalidation_epoch() == epoch
}

fn scope_resources_complete_with_renderer(
    dataset: &DatasetDemandState,
    native: &NativePresentationBridge,
    scope: u64,
) -> bool {
    let Some(product) = native.product_gpu.as_ref() else {
        return dataset.scope_resources_complete(scope);
    };
    let epoch = product.renderer.residency_invalidation_epoch();
    let complete = dataset.scope_resources_complete_with_gpu_residency_at_invalidation_epoch(
        scope,
        epoch.get(),
        |key| product.renderer.resource_is_resident(key),
    );
    complete && product.renderer.residency_invalidation_epoch() == epoch
}

/// Promotes every installed camera-guard tail covered by the reuse envelope.
/// A progressive plan can own both a visible coarse scope and a hidden target
/// scope; preserve-complete planning simply leaves the coarse scope absent.
/// Dataset and renderer boundaries share immutable bodies and change together.
fn promote_installed_camera_guards(
    dataset: &mut DatasetDemandState,
    render_plans: &mut std::collections::BTreeMap<u64, PreparedScopeRenderPlan>,
) -> bool {
    let scopes = [SCOPE_CURRENT_3D, SCOPE_CURRENT_3D_REFINEMENT];
    for scope in scopes {
        let dataset_has_unpromoted_guard =
            dataset.scope_required_prefix_len(scope) < dataset.scope_requirements(scope).len();
        let render_has_unpromoted_guard = render_plans.get(&scope).is_some_and(|plan| {
            !plan.requirements.prefetch_promoted()
                && plan.requirements.prefetch_resource_count() != 0
        });
        if dataset_has_unpromoted_guard != render_has_unpromoted_guard {
            return false;
        }
    }

    for scope in scopes {
        if dataset.scope_required_prefix_len(scope) >= dataset.scope_requirements(scope).len() {
            continue;
        }
        let promoted_dataset = dataset.promote_scope_prefetch_tail(scope);
        let plan = render_plans
            .get_mut(&scope)
            .expect("a camera-guard dataset scope owns matching render requirements");
        plan.requirements = plan.requirements.promote_prefetch();
        debug_assert!(promoted_dataset);
    }
    true
}

fn installed_camera_guard_bodies_are_complete(
    dataset: &DatasetDemandState,
    native: &NativePresentationBridge,
) -> bool {
    [SCOPE_CURRENT_3D, SCOPE_CURRENT_3D_REFINEMENT]
        .into_iter()
        .all(|scope| {
            let requirements = dataset.scope_requirements(scope);
            native.product_gpu.as_ref().map_or_else(
                || {
                    requirements
                        .iter()
                        .all(|key| dataset.retained_leases().payload(*key).is_some())
                },
                |product| {
                    requirements
                        .iter()
                        .all(|key| product.renderer.resource_is_resident(*key))
                },
            )
        })
}

fn promote_ready_staged_plan_with_renderer(
    dataset: &mut DatasetDemandState,
    native: &NativePresentationBridge,
    post_promotion_update: &mut Option<PreparedRendererRequirementUpdate>,
) -> Result<bool, RuntimeFault> {
    if native.product_gpu.is_none() {
        if !dataset.staging_current_refinement()
            || !dataset.scope_resources_complete(SCOPE_CURRENT_3D_REFINEMENT)
        {
            return Ok(false);
        }
        let update = post_promotion_update
            .as_ref()
            .ok_or_else(|| RuntimeFault::new(RuntimeFaultCode::InvariantViolation))?;
        let mut targets = ScopeReconciliationTargets::default();
        if !dataset.prepare_staged_current_promotion_scope_targets(&mut targets) {
            return Err(RuntimeFault::new(RuntimeFaultCode::InvariantViolation));
        }
        dataset
            .preflight_prepared_renderer_requirement_update(
                &update.previous.requirements,
                &update.next.requirements,
            )
            .map_err(|_| RuntimeFault::new(RuntimeFaultCode::InvariantViolation))?;
        let prepared_reconciliation = dataset.prepare_scope_reconciliation(targets)?;

        // The exact cancellation batch is the final fallible boundary. Scope
        // ownership, staged-plan identity, and the worker-prepared retained
        // union publish together after it succeeds.
        dataset.commit_prepared_scope_reconciliation(prepared_reconciliation)?;
        dataset.commit_reconciled_gpu_prefilled_staged_current_plan();
        let PreparedRendererRequirementUpdate {
            previous,
            next,
            removals,
            removal_charge: _removal_charge,
        } = post_promotion_update
            .take()
            .expect("the checked headless promotion union remains installed");
        dataset.commit_preflighted_renderer_requirement_update(
            previous.requirements,
            next.requirements,
            &removals,
            next.charge,
        );
        return Ok(true);
    }
    // Product rendering pre-fills a hidden presentation token and performs
    // the dataset promotion together with the atomic texture-token swap.
    // Promoting here on residency alone would expose the target request before
    // its complete texture exists.
    Ok(false)
}

/// Candidate traversal and renderer output are different bounds. Accepted
/// storage profiles contain up to 109,196 logical bricks; the conservative
/// oblique/frustum AABB may visit that envelope before exact rejection yields
/// at most `MAX_RENDER_REQUIREMENTS` output resources.
const SEMANTIC_PLAN_CANDIDATES_PER_LAYER: usize = 131_072;

/// Caps incremental upload/render submissions at the display cadence while
/// still allowing the first useful lease to become visible immediately.
/// Completion of the cohort is never delayed by this pacer.
const PROGRESSIVE_DISPLAY_REFRESH_INTERVAL: Duration = Duration::from_millis(16);

/// Completion ingestion is independent of presentation pacing. Poll in
/// cache-friendly batches, but allow one UI turn to absorb a full renderer
/// requirement envelope when processing remains inside its time slice.
const RESULT_DRAIN_BATCH: usize = 256;
const RESULT_DRAIN_COUNT_ENVELOPE: usize = MAX_RENDER_REQUIREMENTS;
const RESULT_DRAIN_TIME_BUDGET: Duration = Duration::from_millis(3);

#[derive(Debug, Default)]
pub(crate) struct ProgressiveDisplayRefreshPacer {
    last_requested_at: Option<Instant>,
    useful_change_pending: bool,
}

impl ProgressiveDisplayRefreshPacer {
    fn reset(&mut self) {
        self.last_requested_at = None;
        self.useful_change_pending = false;
    }

    fn observe_useful_change(&mut self) {
        self.useful_change_pending = true;
    }

    fn cancel_pending(&mut self) {
        self.useful_change_pending = false;
    }

    fn take_due(&mut self, now: Instant) -> bool {
        if !self.useful_change_pending
            || self.last_requested_at.is_some_and(|last| {
                now.saturating_duration_since(last) < PROGRESSIVE_DISPLAY_REFRESH_INTERVAL
            })
        {
            return false;
        }
        self.useful_change_pending = false;
        self.last_requested_at = Some(now);
        true
    }

    fn next_due_in(&self, now: Instant) -> Option<Duration> {
        self.useful_change_pending.then(|| {
            self.last_requested_at.map_or(Duration::ZERO, |last| {
                PROGRESSIVE_DISPLAY_REFRESH_INTERVAL
                    .saturating_sub(now.saturating_duration_since(last))
            })
        })
    }

    fn record_unpaced_refresh(&mut self, now: Instant) {
        self.useful_change_pending = false;
        self.last_requested_at = Some(now);
    }
}

fn next_completion_drain_batch(processed: usize, elapsed: Duration) -> usize {
    if processed >= RESULT_DRAIN_COUNT_ENVELOPE
        || (processed > 0 && elapsed >= RESULT_DRAIN_TIME_BUDGET)
    {
        return 0;
    }
    RESULT_DRAIN_BATCH.min(RESULT_DRAIN_COUNT_ENVELOPE - processed)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VisibleBrickRequestOutcome {
    /// A newly prepared current-view transaction was atomically published.
    /// This is distinct from changed resource membership: viewport or camera
    /// changes can need a new frame while reusing the exact same resident set.
    pub(crate) current_plan_installed: bool,
    pub(crate) current_changed: bool,
    pub(crate) resident_changed: bool,
    pub(crate) current_frame_ready: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct VisibleDemandPlanCurrentness {
    pub(crate) current_3d: bool,
    pub(crate) cross_sections: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct DemandPlanningLayerSignature {
    key: mirante4d_domain::LogicalLayerKey,
    visible: bool,
    sampling_footprint: Option<crate::dataset_demand_plan::SamplingFootprintClass>,
}

#[derive(Debug, Clone, PartialEq)]
struct Current3dDemandPlanningSignature {
    resource_identity: mirante4d_dataset::DatasetResourceIdentity,
    active_layer: mirante4d_domain::LogicalLayerKey,
    timepoint: TimeIndex,
    camera: mirante4d_domain::CameraView,
    layout: ViewerLayout,
    layers: Box<[DemandPlanningLayerSignature]>,
    presentation_viewport: mirante4d_render_api::PresentationViewport,
    render_viewport: mirante4d_render_api::RenderExtent,
    playback_active: bool,
    gpu_payload_capacity: u64,
}

impl Current3dDemandPlanningSignature {
    fn same_non_camera_demand(&self, other: &Self) -> bool {
        self.resource_identity == other.resource_identity
            && self.active_layer == other.active_layer
            && self.timepoint == other.timepoint
            && self.layout == other.layout
            && self.layers == other.layers
            && self.presentation_viewport == other.presentation_viewport
            && self.render_viewport == other.render_viewport
            && self.playback_active == other.playback_active
            && self.gpu_payload_capacity == other.gpu_payload_capacity
    }
}

fn installed_target_layer_scales<'a>(
    staging_refinement: bool,
    current: Option<&'a BTreeMap<mirante4d_domain::LogicalLayerKey, mirante4d_domain::ScaleLevel>>,
    refinement: Option<
        &'a BTreeMap<mirante4d_domain::LogicalLayerKey, mirante4d_domain::ScaleLevel>,
    >,
) -> Option<&'a BTreeMap<mirante4d_domain::LogicalLayerKey, mirante4d_domain::ScaleLevel>> {
    if staging_refinement {
        refinement
    } else {
        current
    }
}

#[derive(Debug, Clone, PartialEq)]
struct CrossSectionDemandPlanningSignature {
    resource_identity: mirante4d_dataset::DatasetResourceIdentity,
    active_layer: mirante4d_domain::LogicalLayerKey,
    timepoint: TimeIndex,
    layout: ViewerLayout,
    cross_section: mirante4d_domain::CrossSectionView,
    layers: Box<[DemandPlanningLayerSignature]>,
    presentation_viewports: [Option<mirante4d_render_api::PresentationViewport>; 3],
    render_extents: [Option<mirante4d_render_api::RenderExtent>; 3],
    playback_active: bool,
    gpu_payload_capacity: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingVisibleDemandPlan {
    generation: crate::camera_demand_cache::CameraDemandGeneration,
    current_3d: Option<Current3dDemandPlanningSignature>,
    cross_sections: Option<CrossSectionDemandPlanningSignature>,
    union_plan_required: bool,
    preserve_complete_presentation: bool,
    unchanged_scope_handles: Vec<(u64, Arc<[mirante4d_dataset::DatasetResourceKey]>)>,
    cpu_capacity_epoch: u64,
    runtime_recovery_epoch: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct VisibleDemandFailureSignature {
    planning: VisibleDemandPlanningSignature,
    current_plan_required: bool,
    cross_plan_required: bool,
    union_plan_required: bool,
    unchanged_scope_handles: Vec<(u64, Arc<[mirante4d_dataset::DatasetResourceKey]>)>,
    cpu_capacity_epoch: u64,
    runtime_recovery_epoch: u64,
}

impl VisibleDemandFailureSignature {
    fn new(
        planning: VisibleDemandPlanningSignature,
        current_plan_required: bool,
        cross_plan_required: bool,
        union_plan_required: bool,
        unchanged_scope_handles: Vec<(u64, Arc<[mirante4d_dataset::DatasetResourceKey]>)>,
        cpu_capacity_epoch: u64,
        runtime_recovery_epoch: u64,
    ) -> Self {
        Self {
            planning,
            current_plan_required,
            cross_plan_required,
            union_plan_required,
            unchanged_scope_handles,
            cpu_capacity_epoch,
            runtime_recovery_epoch,
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "all independent retry-latch identity dimensions must remain explicit"
    )]
    fn matches_current(
        &self,
        planning: &VisibleDemandPlanningSignature,
        current_plan_required: bool,
        cross_plan_required: bool,
        union_plan_required: bool,
        dataset: &DatasetDemandState,
        cpu_capacity_epoch: u64,
        runtime_recovery_epoch: u64,
    ) -> bool {
        self.matches_inputs(
            planning,
            current_plan_required,
            cross_plan_required,
            union_plan_required,
            cpu_capacity_epoch,
            runtime_recovery_epoch,
            scope_handles_are_current(dataset, &self.unchanged_scope_handles),
        )
    }

    // Keeping the seven independent identity dimensions visible prevents a
    // partial comparison from silently reopening terminal work.
    #[allow(clippy::too_many_arguments)]
    fn matches_inputs(
        &self,
        planning: &VisibleDemandPlanningSignature,
        current_plan_required: bool,
        cross_plan_required: bool,
        union_plan_required: bool,
        cpu_capacity_epoch: u64,
        runtime_recovery_epoch: u64,
        scope_handles_are_current: bool,
    ) -> bool {
        self.planning == *planning
            && self.current_plan_required == current_plan_required
            && self.cross_plan_required == cross_plan_required
            && self.union_plan_required == union_plan_required
            && self.cpu_capacity_epoch == cpu_capacity_epoch
            && self.runtime_recovery_epoch == runtime_recovery_epoch
            && scope_handles_are_current
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedScopeRenderPlan {
    pub(crate) requirements: PreparedRenderRequirements,
    pub(crate) static_layout: PreparedStaticPresentationLayout,
    pub(crate) planned_payload_bytes: u64,
    pub(crate) primary_resource_count: usize,
}

/// Concrete per-view caches. Installed dataset scope requirements are the
/// cached planner outputs; these signatures independently decide which scope
/// needs to replace its output.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VisibleDemandPlanningSignature {
    current_3d: Current3dDemandPlanningSignature,
    cross_sections: CrossSectionDemandPlanningSignature,
}

fn installed_visible_demand_plan_currentness(
    installed: Option<&VisibleDemandPlanningSignature>,
    current: &VisibleDemandPlanningSignature,
) -> VisibleDemandPlanCurrentness {
    let Some(installed) = installed else {
        return VisibleDemandPlanCurrentness::default();
    };
    VisibleDemandPlanCurrentness {
        current_3d: installed.current_3d == current.current_3d,
        cross_sections: installed.cross_sections == current.cross_sections,
    }
}

impl MiranteWorkbenchApp {
    pub(crate) fn visible_demand_plan_currentness(&self) -> VisibleDemandPlanCurrentness {
        let snapshot = self.application.snapshot();
        let current = self.visible_demand_planning_signature(&snapshot);
        installed_visible_demand_plan_currentness(
            self.visible_demand_planning_signature.as_ref(),
            &current,
        )
    }

    /// Prepares the existing immutable demand body for a transient camera.
    ///
    /// This path never plans or changes application/project state. It accepts
    /// only full-volume coverage or a camera proven inside the installed guard
    /// envelope, and it promotes a guard only after its complete body is
    /// already resident. The caller can therefore rebind the current request
    /// without exposing an arriving-brick mosaic.
    pub(crate) fn prepare_resident_camera_preview(&mut self, camera: CameraView) -> bool {
        let inside_reuse_envelope =
            self.current_camera_reuse_envelope
                .as_ref()
                .is_some_and(|envelope| {
                    mirante4d_render_api::CameraFrame::new(
                        camera,
                        self.render_coordination.presentation_viewport,
                    )
                    .ok()
                    .and_then(|camera| {
                        envelope
                            .contains(camera, self.render_coordination.render_viewport)
                            .ok()
                    })
                    .unwrap_or(false)
                });
        if !self.dataset.current_covers_full_volume() && !inside_reuse_envelope {
            return false;
        }
        if !installed_camera_guard_bodies_are_complete(&self.dataset, &self.native_presentation) {
            return false;
        }
        promote_installed_camera_guards(&mut self.dataset, &mut self.prepared_scope_render_plans)
    }

    fn effective_gpu_payload_capacity(&self) -> u64 {
        self.native_presentation
            .product_gpu
            .as_ref()
            .map(|product| product.renderer.payload_capacity_bytes())
            .unwrap_or_else(|| {
                // Headless app fixtures have no product renderer. Their
                // decoded-residency cap remains a conservative stand-in;
                // production semantic planning is always GPU-budgeted.
                self.dataset
                    .dispatcher()
                    .config()
                    .category_cap(CpuLedgerCategory::DecodedResidency)
            })
    }

    fn visible_demand_planning_signature(
        &self,
        snapshot: &mirante4d_application::ApplicationSnapshot,
    ) -> VisibleDemandPlanningSignature {
        let view = application_view(snapshot);
        let panels = [PanelId::Xy, PanelId::Xz, PanelId::Yz];
        let layers = view
            .layers()
            .iter()
            .map(|layer| DemandPlanningLayerSignature {
                key: layer.layer_key(),
                visible: layer.visible(),
                sampling_footprint: layer
                    .visible()
                    .then(|| sampling_footprint_class(*layer.render_state())),
            })
            .collect::<Box<[_]>>();
        let presentation_viewports = panels.map(|panel| {
            self.render_coordination
                .surface(panel.presentation_slot())
                .presentation_viewport()
        });
        let render_extents = panels.map(|panel| {
            self.render_coordination
                .surface(panel.presentation_slot())
                .render_viewport()
        });
        let gpu_payload_capacity = self.effective_gpu_payload_capacity();
        VisibleDemandPlanningSignature {
            current_3d: Current3dDemandPlanningSignature {
                resource_identity: snapshot.catalog().resource_identity(),
                active_layer: view.active_layer(),
                timepoint: view.timepoint(),
                camera: *view.camera(),
                layout: view.layout(),
                layers: layers.clone(),
                presentation_viewport: self.render_coordination.presentation_viewport,
                render_viewport: self.render_coordination.render_viewport,
                playback_active: snapshot.transient().playback_active(),
                gpu_payload_capacity,
            },
            cross_sections: CrossSectionDemandPlanningSignature {
                resource_identity: snapshot.catalog().resource_identity(),
                active_layer: view.active_layer(),
                timepoint: view.timepoint(),
                layout: view.layout(),
                cross_section: *view.cross_section(),
                layers,
                presentation_viewports,
                render_extents,
                playback_active: snapshot.transient().playback_active(),
                gpu_payload_capacity,
            },
        }
    }

    fn pump_unchanged_visible_demand(&mut self) -> VisibleBrickRequestOutcome {
        let promoted = match promote_ready_staged_plan_with_renderer(
            &mut self.dataset,
            &self.native_presentation,
            &mut self.staged_post_promotion_renderer_update,
        ) {
            Ok(promoted) => promoted,
            Err(fault) => {
                self.record_dataset_fault(&fault);
                return VisibleBrickRequestOutcome::default();
            }
        };
        let preserve_presentation = self.dataset.deferred_refill_preserves_presentation();
        match pump_interactive_admission_with_renderer(&mut self.dataset, &self.native_presentation)
        {
            Ok(_) => {
                if let Some(fault) = self.dataset.dispatcher_mut().take_last_fault() {
                    self.dataset
                        .take_deferred_refill_presentation_preservation();
                    self.record_dataset_admission_fault(&fault, preserve_presentation);
                } else {
                    self.dataset.finish_deferred_refill_if_unblocked();
                }
            }
            Err(fault) => {
                self.dataset
                    .take_deferred_refill_presentation_preservation();
                self.record_dataset_admission_fault(&fault, preserve_presentation);
            }
        }
        let ready = scope_complete_with_renderer(
            &self.dataset,
            &self.native_presentation,
            SCOPE_CURRENT_3D,
        );
        self.update_dataset_fidelity(ready);
        VisibleBrickRequestOutcome {
            current_plan_installed: false,
            current_changed: promoted,
            resident_changed: false,
            current_frame_ready: ready,
        }
    }

    pub(crate) fn request_visible_bricks(&mut self) -> VisibleBrickRequestOutcome {
        let snapshot = self.application.snapshot();
        if self.dataset.source_quarantined()
            || self.dataset.resource_identity() != snapshot.catalog().resource_identity()
        {
            if (self.pending_visible_demand_plan.is_some()
                || self.camera_demand_planner.has_outstanding_request())
                && let Err(error) = self.camera_demand_planner.invalidate()
            {
                self.dataset.record_plan_error(error.to_string());
            }
            self.pending_visible_demand_plan = None;
            self.render_coordination.frame_fidelity.completeness = FrameCompleteness::Loading;
            self.render_coordination.frame_fidelity.reason = LodDecisionReason::LoadingTargetScale;
            self.render_coordination.frame_fidelity.backend = RenderBackend::Loading;
            return VisibleBrickRequestOutcome::default();
        }

        let planning_signature = self.visible_demand_planning_signature(&snapshot);
        let mut accepted = VisibleBrickRequestOutcome::default();
        if let Some(result) = self.camera_demand_planner.take_result() {
            self.last_camera_demand_planning_duration = Some(result.planning_duration);
            let pending = self.pending_visible_demand_plan.take();
            let result_is_current = pending
                .as_ref()
                .is_some_and(|pending| pending.generation == result.generation)
                && pending
                    .as_ref()
                    .and_then(|pending| pending.current_3d.as_ref())
                    .is_none_or(|current| current == &planning_signature.current_3d)
                && pending
                    .as_ref()
                    .and_then(|pending| pending.cross_sections.as_ref())
                    .is_none_or(|cross| cross == &planning_signature.cross_sections)
                && pending.as_ref().is_some_and(|pending| {
                    unchanged_scope_handles_are_current(&self.dataset, pending)
                });
            if result_is_current {
                match result.outcome {
                    Ok(prepared) => {
                        let pending = pending.expect("a current result has pending identity");
                        match self.install_prepared_visible_demand(prepared, &pending) {
                            Ok(outcome) => {
                                self.visible_demand_failure_latch = None;
                                accepted = outcome;
                            }
                            Err(error) => {
                                if visible_demand_failure_is_deterministic(&error) {
                                    let signature = visible_demand_failure_signature(
                                        planning_signature.clone(),
                                        pending,
                                    );
                                    self.visible_demand_failure_latch =
                                        Some(DeterministicFailureLatch::new(signature));
                                }
                                self.record_dataset_plan_error(&error);
                                return VisibleBrickRequestOutcome::default();
                            }
                        }
                    }
                    Err(error) => {
                        let pending = pending.expect("a current result has pending identity");
                        if visible_demand_failure_is_deterministic(&error) {
                            let signature = visible_demand_failure_signature(
                                planning_signature.clone(),
                                pending,
                            );
                            self.visible_demand_failure_latch =
                                Some(DeterministicFailureLatch::new(signature));
                        }
                        self.record_dataset_plan_error(&error);
                        return VisibleBrickRequestOutcome::default();
                    }
                }
            }
        }

        let mut current_plan_required = self
            .visible_demand_planning_signature
            .as_ref()
            .is_none_or(|previous| previous.current_3d != planning_signature.current_3d);
        let camera_is_inside_reuse_envelope = self
            .current_camera_reuse_envelope
            .as_ref()
            .is_some_and(|envelope| {
                mirante4d_render_api::CameraFrame::new(
                    planning_signature.current_3d.camera,
                    planning_signature.current_3d.presentation_viewport,
                )
                .ok()
                .and_then(|camera| {
                    envelope
                        .contains(camera, planning_signature.current_3d.render_viewport)
                        .ok()
                })
                .unwrap_or(false)
            });
        let camera_only_change = current_plan_required
            && self
                .visible_demand_planning_signature
                .as_ref()
                .is_some_and(|previous| {
                    previous
                        .current_3d
                        .same_non_camera_demand(&planning_signature.current_3d)
                });
        let installed_target_scales = installed_target_layer_scales(
            self.dataset.staging_current_refinement(),
            self.dataset.scope_layer_scales(SCOPE_CURRENT_3D),
            self.dataset.scope_layer_scales(SCOPE_CURRENT_3D_REFINEMENT),
        );
        let projected_lod_is_unchanged = camera_only_change
            && current_3d_projected_layer_scales(
                snapshot.catalog(),
                application_view(&snapshot),
                planning_signature.current_3d.presentation_viewport,
                planning_signature.current_3d.render_viewport,
                planning_signature.current_3d.playback_active,
            )
            .is_ok_and(|selected| installed_target_scales == Some(&selected));
        let current_plan_is_reusable = projected_lod_is_unchanged
            && (self.dataset.current_covers_full_volume() || camera_is_inside_reuse_envelope);
        let camera_guard_is_reusable = !current_plan_is_reusable
            || (installed_camera_guard_bodies_are_complete(
                &self.dataset,
                &self.native_presentation,
            ) && promote_installed_camera_guards(
                &mut self.dataset,
                &mut self.prepared_scope_render_plans,
            ));
        if current_plan_is_reusable && camera_guard_is_reusable {
            current_plan_required = false;
            let reusable_candidates = self.current_camera_reuse_envelope.as_ref().map_or_else(
                || self.dataset.scope_requirements(SCOPE_CURRENT_3D).len(),
                |envelope| envelope.reusable_candidates(),
            );
            self.camera_demand_planner
                .record_contained_reuse(reusable_candidates);
            if let Some(installed) = self.visible_demand_planning_signature.as_mut() {
                installed.current_3d = planning_signature.current_3d.clone();
            }
        }
        let cross_plan_required = self
            .visible_demand_planning_signature
            .as_ref()
            .is_none_or(|previous| previous.cross_sections != planning_signature.cross_sections);
        let union_plan_required = self.dataset.renderer_requirement_union_needs_preparation();
        if !current_plan_required && !cross_plan_required && !union_plan_required {
            self.visible_demand_failure_latch = None;
            if self.pending_visible_demand_plan.is_some() {
                if let Err(error) = self.camera_demand_planner.invalidate() {
                    self.dataset.record_plan_error(error.to_string());
                }
                self.pending_visible_demand_plan = None;
            }
            let mut pumped = self.pump_unchanged_visible_demand();
            pumped.current_plan_installed |= accepted.current_plan_installed;
            pumped.current_changed |= accepted.current_changed;
            return pumped;
        }

        let cpu_capacity_epoch = self.dataset.cpu_capacity_epoch();
        let failure_is_unchanged =
            self.visible_demand_failure_latch
                .as_ref()
                .is_some_and(|latch| {
                    latch.blocks(|failed| {
                        failed.matches_current(
                            &planning_signature,
                            current_plan_required,
                            cross_plan_required,
                            union_plan_required,
                            &self.dataset,
                            cpu_capacity_epoch,
                            self.viewer_runtime_recovery_epoch,
                        )
                    })
                });
        if failure_is_unchanged {
            return VisibleBrickRequestOutcome::default();
        }
        self.visible_demand_failure_latch = None;

        let pending_matches = self
            .pending_visible_demand_plan
            .as_ref()
            .is_some_and(|pending| {
                pending.current_3d.as_ref()
                    == current_plan_required.then_some(&planning_signature.current_3d)
                    && pending.cross_sections.as_ref()
                        == cross_plan_required.then_some(&planning_signature.cross_sections)
                    && unchanged_scope_handles_are_current(&self.dataset, pending)
            });
        if !pending_matches {
            let view = application_view(&snapshot);
            let four_panel = view.layout() == ViewerLayout::FourPanel;
            let playback_active = snapshot.transient().playback_active();
            let demand_cohorts = 1 + usize::from(playback_active) + if four_panel { 3 } else { 0 };
            let gpu_payload_capacity = self.effective_gpu_payload_capacity();
            let mut unchanged_scopes = Vec::new();
            if !current_plan_required {
                unchanged_scopes.extend([
                    SCOPE_CURRENT_3D,
                    SCOPE_CURRENT_3D_REFINEMENT,
                    SCOPE_PLAYBACK,
                ]);
            }
            if !cross_plan_required {
                unchanged_scopes.extend(cross_section_scopes().map(|(scope, _)| scope));
            }
            unchanged_scopes.push(SCOPE_ANALYSIS);
            let unchanged_scope_handles = unchanged_scopes
                .iter()
                .copied()
                .map(|scope| (scope, self.dataset.scope_requirement_handle(scope)))
                .collect::<Vec<_>>();
            let unchanged_bodies = unchanged_scopes
                .iter()
                .copied()
                .map(|scope| self.dataset.scope_prepared_body_handle(scope))
                .collect::<Vec<_>>();

            let mut preserve_complete_presentation = false;
            let current_3d = if current_plan_required {
                #[cfg(test)]
                {
                    self.visible_demand_plan_calls =
                        self.visible_demand_plan_calls.saturating_add(1);
                }
                let current_share_numerator = if playback_active { 2 } else { 1 };
                preserve_complete_presentation = self
                    .native_presentation
                    .product_gpu
                    .as_ref()
                    .and_then(|product| product.targets.get(&PanelId::ThreeD))
                    .and_then(|target| target.presented.as_ref())
                    .is_some_and(|frame| {
                        frame.progress().completeness()
                            != mirante4d_render_api::FrameCompleteness::Progressive
                    });
                Some(Current3dDemandRequest::new(
                    self.render_coordination.presentation_viewport,
                    self.render_coordination.render_viewport,
                    DatasetDemandPlanLimits::new(
                        SEMANTIC_PLAN_CANDIDATES_PER_LAYER,
                        budget_share_usize(
                            PRODUCT_RENDER_RESOURCE_LIMIT,
                            current_share_numerator,
                            demand_cohorts,
                        ),
                        budget_share_u64(
                            gpu_payload_capacity,
                            current_share_numerator,
                            demand_cohorts,
                        ),
                    ),
                    playback_active,
                    next_playback_timepoint(&snapshot),
                    preserve_complete_presentation,
                    Current3dDemandBaselines::new(
                        scope_baseline(
                            &self.dataset,
                            &self.prepared_scope_render_plans,
                            SCOPE_CURRENT_3D,
                        ),
                        scope_baseline(
                            &self.dataset,
                            &self.prepared_scope_render_plans,
                            SCOPE_CURRENT_3D_REFINEMENT,
                        ),
                        scope_baseline(
                            &self.dataset,
                            &self.prepared_scope_render_plans,
                            SCOPE_PLAYBACK,
                        ),
                    ),
                ))
            } else {
                None
            };

            let mut cross_sections = Vec::new();
            if cross_plan_required && four_panel {
                for (scope, panel) in cross_section_scopes() {
                    let surface = self.render_coordination.surface(panel.presentation_slot());
                    let (Some(presentation), Some(extent)) =
                        (surface.presentation_viewport(), surface.render_viewport())
                    else {
                        continue;
                    };
                    #[cfg(test)]
                    {
                        self.cross_section_demand_plan_calls =
                            self.cross_section_demand_plan_calls.saturating_add(1);
                    }
                    cross_sections.push(CrossSectionDemandRequest::new(
                        panel,
                        presentation,
                        extent,
                        DatasetDemandPlanLimits::new(
                            SEMANTIC_PLAN_CANDIDATES_PER_LAYER,
                            budget_share_usize(PRODUCT_RENDER_RESOURCE_LIMIT, 1, demand_cohorts),
                            budget_share_u64(gpu_payload_capacity, 1, demand_cohorts),
                        ),
                        scope_baseline(&self.dataset, &self.prepared_scope_render_plans, scope),
                    ));
                }
            }
            let promotion_bodies =
                (current_3d.is_none() && self.dataset.staging_current_refinement()).then(|| {
                    let mut bodies = vec![
                        self.dataset
                            .scope_prepared_body_handle(SCOPE_CURRENT_3D_REFINEMENT),
                        self.dataset.scope_prepared_body_handle(SCOPE_PLAYBACK),
                        self.dataset.scope_prepared_body_handle(SCOPE_ANALYSIS),
                    ];
                    if !cross_plan_required {
                        bodies.extend(
                            cross_section_scopes()
                                .map(|(scope, _)| self.dataset.scope_prepared_body_handle(scope)),
                        );
                    }
                    bodies
                });
            let request = CameraDemandRequest::new(
                Arc::clone(snapshot.catalog()),
                self.dataset.cpu_ledger_arc(),
                view.clone(),
                current_3d,
                cross_sections,
                self.dataset.renderer_requirement_handle(),
                unchanged_bodies,
                promotion_bodies,
            );
            let generation = match self.camera_demand_planner.submit(request) {
                Ok(generation) => generation,
                Err(error) => {
                    let signature = VisibleDemandFailureSignature::new(
                        planning_signature.clone(),
                        current_plan_required,
                        cross_plan_required,
                        union_plan_required,
                        unchanged_scope_handles,
                        cpu_capacity_epoch,
                        self.viewer_runtime_recovery_epoch,
                    );
                    self.visible_demand_failure_latch =
                        Some(DeterministicFailureLatch::new(signature));
                    self.dataset.record_plan_error(error.to_string());
                    self.render_coordination.frame_fidelity.completeness =
                        FrameCompleteness::Incomplete;
                    return VisibleBrickRequestOutcome::default();
                }
            };
            self.pending_visible_demand_plan = Some(PendingVisibleDemandPlan {
                generation,
                current_3d: current_plan_required.then(|| planning_signature.current_3d.clone()),
                cross_sections: cross_plan_required
                    .then(|| planning_signature.cross_sections.clone()),
                union_plan_required,
                preserve_complete_presentation,
                unchanged_scope_handles,
                cpu_capacity_epoch,
                runtime_recovery_epoch: self.viewer_runtime_recovery_epoch,
            });
        }

        let mut pumped = self.pump_unchanged_visible_demand();
        pumped.current_plan_installed |= accepted.current_plan_installed;
        pumped.current_changed |= accepted.current_changed;
        if self.pending_current_3d_demand() {
            self.render_coordination.frame_fidelity.completeness = FrameCompleteness::Loading;
            self.render_coordination.frame_fidelity.reason = LodDecisionReason::LoadingTargetScale;
            self.render_coordination.frame_fidelity.backend = RenderBackend::Loading;
            pumped.current_frame_ready = false;
        }
        pumped
    }

    pub(crate) fn pending_current_3d_demand(&self) -> bool {
        self.pending_visible_demand_plan
            .as_ref()
            .is_some_and(|pending| pending.current_3d.is_some())
    }

    pub(crate) fn install_prepared_visible_demand(
        &mut self,
        prepared: PreparedVisibleDemand,
        pending: &PendingVisibleDemandPlan,
    ) -> anyhow::Result<VisibleBrickRequestOutcome> {
        let PreparedVisibleDemand {
            current_3d,
            cross_sections,
            renderer_requirement_update,
            post_refinement_promotion_update,
            candidates_visited,
        } = prepared;
        if current_3d.is_some() != pending.current_3d.is_some() {
            anyhow::bail!("prepared 3D scope set does not match its pending transaction");
        }
        if pending.cross_sections.is_none() && !cross_sections.is_empty() {
            anyhow::bail!("prepared cross-section scope set was not requested");
        }

        // Build and validate the complete transaction before the first scope
        // map changes. Everything below the preflight block is infallible.
        let four_panel = pending
            .current_3d
            .as_ref()
            .is_some_and(|signature| signature.layout == ViewerLayout::FourPanel);
        let current_install = current_3d
            .map(|current| {
                let target_scale = current.plan.target.scale;
                let target_playback_downshifted = current.plan.target.playback_downshifted;
                let target_visible_count = current.plan.target.primary_resource_count;
                let reuse_envelope = current.plan.reuse_envelope.clone();
                let playback_layer_scales = current.plan.target.layer_scales.clone();
                let current_accounting = if current.current.render_requirements.is_some() {
                    if current.refinement.render_requirements.is_some() {
                        current.plan.coarse.as_ref()
                    } else {
                        Some(&current.plan.target)
                    }
                } else {
                    None
                }
                .map(|plan| (plan.payload_bytes, plan.primary_resource_count));
                let refinement_accounting =
                    current.refinement.render_requirements.is_some().then_some((
                        current.plan.target.payload_bytes,
                        current.plan.target.primary_resource_count,
                    ));
                let current_render = match (
                    current.current.render_requirements,
                    current.current.static_layout,
                ) {
                    (Some(requirements), Some(static_layout)) => Some(PreparedScopeRenderPlan {
                        requirements,
                        static_layout,
                        planned_payload_bytes: current_accounting
                            .expect("a current render owns plan accounting")
                            .0,
                        primary_resource_count: current_accounting
                            .expect("a current render owns plan accounting")
                            .1,
                    }),
                    (None, None) => None,
                    _ => anyhow::bail!("prepared current render artifacts are incomplete"),
                };
                let refinement_render = match (
                    current.refinement.render_requirements,
                    current.refinement.static_layout,
                ) {
                    (Some(requirements), Some(static_layout)) => Some(PreparedScopeRenderPlan {
                        requirements,
                        static_layout,
                        planned_payload_bytes: refinement_accounting
                            .expect("a refinement render owns target accounting")
                            .0,
                        primary_resource_count: refinement_accounting
                            .expect("a refinement render owns target accounting")
                            .1,
                    }),
                    (None, None) => None,
                    _ => anyhow::bail!("prepared refinement render artifacts are incomplete"),
                };
                if current.playback.render_requirements.is_some()
                    || current.playback.static_layout.is_some()
                {
                    anyhow::bail!("prepared playback scope unexpectedly owns render artifacts");
                }
                Ok::<_, anyhow::Error>((
                    current.plan,
                    current.playback.requirements,
                    playback_layer_scales,
                    current_render,
                    refinement_render,
                    target_scale,
                    target_playback_downshifted,
                    target_visible_count,
                    reuse_envelope,
                ))
            })
            .transpose()?;

        let mut installed_panels = std::collections::BTreeSet::new();
        let mut cross_installs = Vec::with_capacity(cross_sections.len());
        if pending.cross_sections.is_some() {
            for cross in cross_sections {
                if !installed_panels.insert(cross.panel) {
                    anyhow::bail!("prepared cross-section transaction contains a duplicate panel");
                }
                cross_installs.push((
                    cross_section_scope_id(cross.panel),
                    cross.panel,
                    cross.plan.requirements,
                    cross.plan.layer_scales,
                    PreparedScopeRenderPlan {
                        requirements: cross.render_requirements,
                        static_layout: cross.static_layout,
                        planned_payload_bytes: cross.plan.payload_bytes,
                        primary_resource_count: cross.plan.primary_resource_count,
                    },
                ));
            }
        }

        let next_signature = match self.visible_demand_planning_signature.as_ref() {
            Some(installed) => {
                let mut next = installed.clone();
                if let Some(current) = pending.current_3d.as_ref() {
                    next.current_3d = current.clone();
                }
                if let Some(cross) = pending.cross_sections.as_ref() {
                    next.cross_sections = cross.clone();
                }
                next
            }
            None => VisibleDemandPlanningSignature {
                current_3d: pending
                    .current_3d
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("initial demand result omitted 3D identity"))?,
                cross_sections: pending.cross_sections.clone().ok_or_else(|| {
                    anyhow::anyhow!("initial demand result omitted cross-section identity")
                })?,
            },
        };

        let mut scope_targets = ScopeReconciliationTargets::default();
        if let Some((plan, playback, _, _, _, _, _, _, _)) = current_install.as_ref() {
            self.dataset.prepare_progressive_current_scope_targets(
                plan,
                four_panel,
                pending.preserve_complete_presentation,
                &mut scope_targets,
            );
            scope_targets.replace(SCOPE_PLAYBACK, playback);
        }
        for (scope, _, requirements, _, _) in &cross_installs {
            scope_targets.replace(*scope, requirements);
        }
        if pending.cross_sections.is_some() {
            for (scope, panel) in cross_section_scopes() {
                if !installed_panels.contains(&panel) {
                    scope_targets.remove(scope);
                }
            }
        }
        self.dataset
            .preflight_prepared_renderer_requirement_update(
                &renderer_requirement_update.previous.requirements,
                &renderer_requirement_update.next.requirements,
            )?;
        if let Some(promotion) = post_refinement_promotion_update.as_ref() {
            if !Arc::ptr_eq(
                &promotion.previous.requirements,
                &renderer_requirement_update.next.requirements,
            ) {
                anyhow::bail!("promotion union is not bound to the installed worker union");
            }
            self.dataset
                .preflight_prepared_renderer_requirement_union(&promotion.next.requirements)?;
        }
        let prepared_scope_reconciliation =
            self.dataset.prepare_scope_reconciliation(scope_targets)?;

        // Atomic runtime cancellation is the last fallible boundary. It
        // prevalidates every ticket under one lock and changes nothing on
        // error; every operation after success is an infallible publication.
        self.dataset
            .commit_prepared_scope_reconciliation(prepared_scope_reconciliation)?;
        self.last_visible_demand_candidates_visited = Some(candidates_visited);

        let current_plan_installed = current_install.is_some();
        let mut current_changed = false;
        if let Some((
            plan,
            playback,
            playback_layer_scales,
            current_render,
            refinement_render,
            target_scale,
            target_playback_downshifted,
            target_visible_count,
            reuse_envelope,
        )) = current_install
        {
            current_changed = self.dataset.commit_preflighted_progressive_current_plan(
                plan,
                four_panel,
                pending.preserve_complete_presentation,
            );
            self.dataset.commit_preflighted_scope_replacement(
                SCOPE_PLAYBACK,
                playback,
                playback_layer_scales,
            );
            install_scope_render_plan(
                &mut self.prepared_scope_render_plans,
                SCOPE_CURRENT_3D,
                current_render,
            );
            install_scope_render_plan(
                &mut self.prepared_scope_render_plans,
                SCOPE_CURRENT_3D_REFINEMENT,
                refinement_render,
            );
            self.render_coordination.frame_fidelity.target_scale_level = target_scale.get();
            self.render_coordination.frame_fidelity.visible_bricks = target_visible_count;
            self.current_camera_reuse_envelope = reuse_envelope;
            self.render_coordination.frame_fidelity.reason = if target_playback_downshifted {
                LodDecisionReason::PlaybackDownshift
            } else if target_scale == mirante4d_domain::ScaleLevel::BASE {
                LodDecisionReason::ExactS0
            } else {
                LodDecisionReason::ScreenEquivalentCoarserScale
            };
        }

        let mut cross_changed = false;
        if pending.cross_sections.is_some() {
            for (scope, _, requirements, layer_scales, render_plan) in cross_installs {
                cross_changed |= self.dataset.commit_preflighted_scope_replacement(
                    scope,
                    requirements,
                    layer_scales,
                );
                self.prepared_scope_render_plans.insert(scope, render_plan);
            }
            for (scope, panel) in cross_section_scopes() {
                if !installed_panels.contains(&panel) {
                    cross_changed |= self.dataset.commit_preflighted_scope_removal(scope);
                    self.prepared_scope_render_plans.remove(&scope);
                }
            }
            if cross_changed {
                self.render_coordination.invalidate_cross_sections();
            }
        }

        let crate::camera_demand_cache::PreparedRendererRequirementUpdate {
            previous,
            next,
            removals,
            removal_charge: _removal_charge,
        } = renderer_requirement_update;
        self.dataset.commit_preflighted_renderer_requirement_update(
            previous.requirements,
            next.requirements,
            &removals,
            next.charge,
        );
        self.staged_post_promotion_renderer_update = if self.dataset.staging_current_refinement() {
            post_refinement_promotion_update
        } else {
            None
        };
        self.visible_demand_planning_signature = Some(next_signature);
        if current_changed {
            self.progressive_display_pacer.reset();
        }
        Ok(VisibleBrickRequestOutcome {
            current_plan_installed,
            current_changed,
            resident_changed: false,
            current_frame_ready: false,
        })
    }

    pub(crate) fn drain_brick_results(&mut self, ctx: &egui::Context) {
        let started = Instant::now();
        let mut current_plan_installed = false;
        let snapshot = self.application.snapshot();
        if !self.dataset.source_quarantined()
            && self.dataset.resource_identity() == snapshot.catalog().resource_identity()
        {
            // Result ingestion is also a bounded wake-up point for the
            // latest-only planner. This keeps slow I/O/background frames
            // progressing even when no display refresh was otherwise due.
            let visible = self.request_visible_bricks();
            current_plan_installed = visible.current_plan_installed;
        }
        let snapshot = self.application.snapshot();
        if self.dataset.source_quarantined()
            || self.dataset.resource_identity() != snapshot.catalog().resource_identity()
        {
            let drain_started = Instant::now();
            let mut drained = 0;
            loop {
                let batch = next_completion_drain_batch(drained, drain_started.elapsed());
                if batch == 0 {
                    break;
                }
                match self
                    .dataset
                    .dispatcher_mut()
                    .drain(batch, |_ticket, _outcome| {})
                {
                    Ok(batch_drained) => {
                        drained = drained.saturating_add(batch_drained);
                        if batch_drained < batch {
                            break;
                        }
                    }
                    Err(fault) => {
                        self.dataset.record_plan_error(fault.to_string());
                        return;
                    }
                }
            }
            let _ = self.dataset.dispatcher_mut().take_last_fault();
            if drained > 0 {
                ctx.request_repaint();
            } else if self.dataset.dispatcher().has_pending_work() {
                ctx.request_repaint_after(BACKGROUND_WORK_REPAINT_INTERVAL);
            }
            return;
        }
        let (dataset, analysis, native_presentation) = (
            &mut self.dataset,
            &mut self.analysis_runtime,
            &self.native_presentation,
        );
        let mut analysis_events = Vec::new();
        let mut analysis_errors = Vec::new();
        let drain_started = Instant::now();
        let mut drained = 0;
        let mut installed_current = false;
        let mut installed_any = false;
        let mut drain_fault = None;
        loop {
            let batch = next_completion_drain_batch(drained, drain_started.elapsed());
            if batch == 0 {
                break;
            }
            let outcome = dataset.drain_runtime_results(batch, |ticket, outcome| {
                debug_assert_eq!(ticket.generation().scope(), SCOPE_ANALYSIS);
                if let Some(token) = analysis.active_token().cloned() {
                    match analysis.accept_completion(ticket, outcome) {
                        Ok(event) => analysis_events.push((token, event)),
                        Err(error) => analysis_errors.push((token, error)),
                    }
                }
            });
            let (batch_drained, batch_installed) = match outcome {
                Ok(outcome) => outcome,
                Err(fault) => {
                    let preserve_presentation =
                        dataset.take_deferred_refill_presentation_preservation();
                    drain_fault = Some((fault, preserve_presentation));
                    break;
                }
            };
            drained = drained.saturating_add(batch_drained);
            native_presentation.dirty_progressive_lease_probes_for_keys(batch_installed.keys());
            installed_current |= batch_installed.in_scope(SCOPE_CURRENT_3D);
            installed_any |= batch_installed.any();
            if dataset.dispatcher().admission_blocked() {
                let preserve_presentation = dataset.deferred_refill_preserves_presentation();
                match pump_interactive_admission_with_renderer(dataset, native_presentation) {
                    Ok(_) => {}
                    Err(fault) => {
                        dataset.take_deferred_refill_presentation_preservation();
                        drain_fault = Some((fault, preserve_presentation));
                        break;
                    }
                }
            }
            if batch_drained < batch {
                break;
            }
        }
        if let Some((fault, preserve_presentation)) = drain_fault {
            self.record_dataset_admission_fault(&fault, preserve_presentation);
            return;
        }
        if let Some(fault) = self.dataset.dispatcher_mut().take_last_fault() {
            let preserve_presentation = self
                .dataset
                .take_deferred_refill_presentation_preservation();
            let retry_after_capacity =
                matches!(fault.code(), RuntimeFaultCode::CapacityExceeded { .. });
            self.record_dataset_admission_fault(&fault, preserve_presentation);
            if !retry_after_capacity {
                return;
            }
        } else {
            self.dataset.finish_deferred_refill_if_unblocked();
        }

        let promoted_current = match promote_ready_staged_plan_with_renderer(
            &mut self.dataset,
            &self.native_presentation,
            &mut self.staged_post_promotion_renderer_update,
        ) {
            Ok(promoted) => promoted,
            Err(fault) => {
                self.record_dataset_fault(&fault);
                return;
            }
        };
        if self.dataset.dispatcher().admission_blocked() {
            let preserve_presentation = self.dataset.deferred_refill_preserves_presentation();
            match pump_interactive_admission_with_renderer(
                &mut self.dataset,
                &self.native_presentation,
            ) {
                Ok(_) => self.dataset.finish_deferred_refill_if_unblocked(),
                Err(fault) => {
                    self.dataset
                        .take_deferred_refill_presentation_preservation();
                    self.record_dataset_admission_fault(&fault, preserve_presentation);
                    return;
                }
            }
        }

        for (token, error) in analysis_errors {
            self.abort_running_analysis(
                &token,
                mirante4d_application::OperationFailureCode::AnalysisExecutionFailed,
                &error,
            );
        }
        let analysis_changed = !analysis_events.is_empty();
        for (token, event) in analysis_events {
            self.handle_analysis_runtime_event(token, event);
        }
        if self.analysis_runtime.active_token().is_some()
            && let Err(error) = self.pump_analysis_requests()
            && let Some(token) = self.analysis_runtime.active_token().cloned()
        {
            self.abort_running_analysis(
                &token,
                mirante4d_application::OperationFailureCode::AnalysisExecutionFailed,
                &error,
            );
        }

        let ready = scope_complete_with_renderer(
            &self.dataset,
            &self.native_presentation,
            SCOPE_CURRENT_3D,
        );
        self.update_dataset_fidelity(ready);
        let refresh_observed_at = Instant::now();
        let complete_current_cohort_installed = installed_current
            && scope_resources_complete_with_renderer(
                &self.dataset,
                &self.native_presentation,
                SCOPE_CURRENT_3D,
            );
        // Every accepted current-view transaction owns exactly one renderer
        // wake. In the preserve-complete path SCOPE_CURRENT_3D is deliberately
        // not "ready" while the new cohort lives in the refinement scope; this
        // wake is what enters the hidden render and atomic-promotion path.
        let complete_refresh = current_plan_installed
            || completion_drain_needs_display_refresh(
                promoted_current || complete_current_cohort_installed,
            );
        let complete_presentation_held = self
            .native_presentation
            .product_gpu
            .as_ref()
            .and_then(|product| product.targets.get(&PanelId::ThreeD))
            .and_then(|target| target.presented.as_ref())
            .is_some_and(|frame| {
                frame.progress().completeness()
                    != mirante4d_render_api::FrameCompleteness::Progressive
            });
        let atomic_replacement_held =
            self.dataset.holding_previous_presentation() || complete_presentation_held;
        if installed_current && !complete_current_cohort_installed && !atomic_replacement_held {
            self.progressive_display_pacer.observe_useful_change();
        } else if atomic_replacement_held {
            // Refinement leases must not replace a complete prior/coarse
            // presentation with a key-ordered subset.
            self.progressive_display_pacer.cancel_pending();
        }
        let partial_refresh =
            !complete_refresh && self.progressive_display_pacer.take_due(refresh_observed_at);
        if complete_refresh || partial_refresh {
            if complete_refresh {
                self.progressive_display_pacer
                    .record_unpaced_refresh(refresh_observed_at);
            }
            self.render_coordination.request_refresh();
            self.render_coordination.frame_fidelity.frame_time_ms =
                Some(started.elapsed().as_secs_f64() * 1_000.0);
            ctx.request_repaint();
        } else if let Some(delay) = self
            .progressive_display_pacer
            .next_due_in(refresh_observed_at)
        {
            // A batch that arrived inside the frame budget is coalesced into
            // the next deadline even if the runtime has no other completion
            // available to wake the UI.
            ctx.request_repaint_after(delay);
        } else if analysis_changed || installed_any || drained > 0 {
            ctx.request_repaint();
        } else if self.dataset.dispatcher().has_pending_work() {
            // A slow read/decode must not turn the UI into a busy poll loop.
            // Fresh completions repaint immediately; an unchanged pending
            // state is sampled at the bounded background cadence.
            ctx.request_repaint_after(BACKGROUND_WORK_REPAINT_INTERVAL);
        }
    }

    fn update_dataset_fidelity(&mut self, ready: bool) {
        let required = self.dataset.scope_required_prefix_len(SCOPE_CURRENT_3D);
        // Exact presented coverage is recorded by `record_product_frame`.
        // Between submissions, the monotonic ready prefix is a conservative
        // O(1) lower bound; never rescan 65k keys merely to repaint a label.
        let resident = if ready {
            required
        } else {
            self.render_coordination
                .frame_fidelity
                .resident_bricks
                .max(self.dataset.scope_ready_prefix(SCOPE_CURRENT_3D))
                .min(required)
        };
        self.render_coordination.frame_fidelity.resident_bricks = resident;
        self.render_coordination
            .frame_fidelity
            .missing_occupied_bricks = required.saturating_sub(resident);
        // CPU ledger diagnostics are collected lazily by diagnostics and
        // automation surfaces, never by the interactive demand pump.
        let empty = self.dataset.scope_is_empty(SCOPE_CURRENT_3D);
        let ready_backend = ready.then(|| {
            let snapshot = self.application.snapshot();
            let view = application_view(&snapshot);
            let mode = view
                .layer(view.active_layer())
                .expect("the current view contains its active layer")
                .render_state()
                .mode();
            render_backend_for_mode(mode)
        });
        self.render_coordination.frame_fidelity.completeness = if empty || ready {
            FrameCompleteness::Complete
        } else {
            FrameCompleteness::Loading
        };
        self.render_coordination.frame_fidelity.backend = if empty {
            RenderBackend::Empty
        } else if let Some(backend) = ready_backend {
            backend
        } else {
            RenderBackend::Loading
        };
        self.render_coordination
            .frame_fidelity
            .displayed_scale_level = (empty || ready).then_some(self.dataset.current_scale().get());
        if empty {
            self.render_coordination.frame_fidelity.reason = LodDecisionReason::NoVisibleData;
        }
    }

    pub(crate) fn record_dataset_fault(&mut self, fault: &RuntimeFault) {
        self.record_dataset_fault_with_presentation_effect(fault, true);
    }

    fn record_dataset_admission_fault(
        &mut self,
        fault: &RuntimeFault,
        preserve_presentation: bool,
    ) {
        self.record_dataset_fault_with_presentation_effect(
            fault,
            admission_fault_invalidates_presentation(preserve_presentation),
        );
    }

    fn record_dataset_fault_with_presentation_effect(
        &mut self,
        fault: &RuntimeFault,
        invalidate_presentation: bool,
    ) {
        let source_binding_invalidated = if runtime_fault_invalidates_verified_source(fault.code())
        {
            let snapshot = self.application.snapshot();
            if snapshot.catalog().scientific_identity().is_verified() {
                match self
                    .application
                    .dispatch(ApplicationCommand::InvalidateSourceVerification {
                        source_generation: snapshot.source_generation(),
                    }) {
                    Ok(_) => true,
                    Err(application_fault) => {
                        tracing::warn!(
                            ?application_fault,
                            "observed source fault could not invalidate the verified binding"
                        );
                        false
                    }
                }
            } else {
                false
            }
        } else {
            false
        };
        let message = fault.to_string();
        self.dataset.record_plan_error(message.clone());
        self.render_coordination.frame_fidelity.last_capacity_error = Some(message);
        if invalidate_presentation {
            self.render_coordination.frame_fidelity.completeness = FrameCompleteness::Incomplete;
        }
        if source_binding_invalidated {
            // The reducer event is consumed at the start of the next UI turn.
            // Quarantine after recording the terminal fault so the runtime's
            // fail-closed loading state remains the final visible state.
            self.retire_invalidated_source_runtime();
        }
    }

    fn record_dataset_plan_error(&mut self, error: &anyhow::Error) {
        let message = error.to_string();
        let capacity = error
            .downcast_ref::<DatasetDemandPlanCapacityError>()
            .is_some();
        self.dataset.record_plan_error(message.clone());
        self.render_coordination.frame_fidelity.last_failure_kind = Some(if capacity {
            FrameFailureKind::BudgetExceeded
        } else {
            FrameFailureKind::InvalidModeParameter
        });
        self.render_coordination.frame_fidelity.last_capacity_error = Some(message);
        self.render_coordination.frame_fidelity.completeness = if capacity {
            FrameCompleteness::BudgetLimited
        } else {
            FrameCompleteness::Incomplete
        };
        self.render_coordination.frame_fidelity.reason = if capacity {
            LodDecisionReason::CpuBudgetLimited
        } else {
            LodDecisionReason::BackendLimit
        };
    }
}

const fn admission_fault_invalidates_presentation(preserve_presentation: bool) -> bool {
    !preserve_presentation
}

pub(crate) const fn runtime_fault_invalidates_verified_source(code: RuntimeFaultCode) -> bool {
    matches!(
        code,
        RuntimeFaultCode::SourceRejected
            | RuntimeFaultCode::CorruptResource
            | RuntimeFaultCode::UnsupportedResource
            | RuntimeFaultCode::DecodeFailed
    )
}

fn scope_baseline(
    dataset: &DatasetDemandState,
    render_plans: &std::collections::BTreeMap<u64, PreparedScopeRenderPlan>,
    scope: u64,
) -> ScopeDemandBaseline {
    let body = dataset.scope_prepared_body_handle(scope);
    let static_layout = render_plans
        .get(&scope)
        .filter(|plan| plan.requirements.body().shares_storage_with(&body))
        .map(|plan| plan.static_layout.clone());
    ScopeDemandBaseline::new(body, dataset.scope_admitted_prefix_len(scope))
        .with_static_layout(static_layout)
}

fn unchanged_scope_handles_are_current(
    dataset: &DatasetDemandState,
    pending: &PendingVisibleDemandPlan,
) -> bool {
    scope_handles_are_current(dataset, &pending.unchanged_scope_handles)
}

fn visible_demand_failure_signature(
    planning: VisibleDemandPlanningSignature,
    pending: PendingVisibleDemandPlan,
) -> VisibleDemandFailureSignature {
    VisibleDemandFailureSignature::new(
        planning,
        pending.current_3d.is_some(),
        pending.cross_sections.is_some(),
        pending.union_plan_required,
        pending.unchanged_scope_handles,
        pending.cpu_capacity_epoch,
        pending.runtime_recovery_epoch,
    )
}

fn visible_demand_failure_is_deterministic(error: &anyhow::Error) -> bool {
    if error
        .downcast_ref::<CpuLedgerError>()
        .is_some_and(|error| matches!(error, CpuLedgerError::ShuttingDown))
    {
        return false;
    }
    !error.downcast_ref::<RuntimeFault>().is_some_and(|fault| {
        matches!(
            fault.code(),
            RuntimeFaultCode::QueueFull
                | RuntimeFaultCode::Cancelled
                | RuntimeFaultCode::StaleGeneration
                | RuntimeFaultCode::ShuttingDown
        )
    })
}

fn scope_handles_are_current(
    dataset: &DatasetDemandState,
    handles: &[(u64, Arc<[mirante4d_dataset::DatasetResourceKey]>)],
) -> bool {
    handles
        .iter()
        .all(|(scope, baseline)| Arc::ptr_eq(baseline, &dataset.scope_requirement_handle(*scope)))
}

fn cross_section_scopes() -> std::array::IntoIter<(u64, PanelId), 3> {
    [
        (SCOPE_CROSS_SECTION_XY, PanelId::Xy),
        (SCOPE_CROSS_SECTION_XZ, PanelId::Xz),
        (SCOPE_CROSS_SECTION_YZ, PanelId::Yz),
    ]
    .into_iter()
}

fn cross_section_scope_id(panel: PanelId) -> u64 {
    match panel {
        PanelId::Xy => SCOPE_CROSS_SECTION_XY,
        PanelId::Xz => SCOPE_CROSS_SECTION_XZ,
        PanelId::Yz => SCOPE_CROSS_SECTION_YZ,
        PanelId::ThreeD => unreachable!("the 3D panel has no cross-section demand scope"),
    }
}

fn install_scope_render_plan(
    plans: &mut std::collections::BTreeMap<u64, PreparedScopeRenderPlan>,
    scope: u64,
    plan: Option<PreparedScopeRenderPlan>,
) {
    if let Some(plan) = plan {
        plans.insert(scope, plan);
    } else {
        plans.remove(&scope);
    }
}

fn next_playback_timepoint(
    snapshot: &mirante4d_application::ApplicationSnapshot,
) -> Option<TimeIndex> {
    if !snapshot.transient().playback_active() {
        return None;
    }
    let timepoints = snapshot
        .catalog()
        .layers()
        .map(|layer| layer.shape().t())
        .min()
        .unwrap_or(1);
    (timepoints > 1)
        .then(|| TimeIndex::new((application_view(snapshot).timepoint().get() + 1) % timepoints))
}

fn budget_share_usize(total: usize, numerator: usize, denominator: usize) -> usize {
    total.checked_div(denominator).unwrap_or(0) * numerator
}

const fn completion_drain_needs_display_refresh(presentation_changed: bool) -> bool {
    presentation_changed
}

fn budget_share_u64(total: u64, numerator: usize, denominator: usize) -> u64 {
    total.checked_div(denominator as u64).unwrap_or(0) * numerator as u64
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        time::{Duration, Instant},
    };

    use mirante4d_dataset::{
        CpuLedgerCategory, CpuLedgerError, DatasetResourceIdentity, DatasetSourceId,
    };
    use mirante4d_dataset_runtime::{RuntimeFault, RuntimeFaultCode};
    use mirante4d_domain::{
        CameraView, CrossSectionView, LogicalLayerKey, Projection, ScaleLevel, TimeIndex,
        UnitQuaternion, ViewerLayout, WorldPoint3,
    };
    use mirante4d_render_api::{PresentationViewport, RenderExtent};

    use super::{
        CrossSectionDemandPlanningSignature, Current3dDemandPlanningSignature,
        PROGRESSIVE_DISPLAY_REFRESH_INTERVAL, ProgressiveDisplayRefreshPacer, RESULT_DRAIN_BATCH,
        RESULT_DRAIN_COUNT_ENVELOPE, RESULT_DRAIN_TIME_BUDGET, VisibleDemandFailureSignature,
        VisibleDemandPlanningSignature, admission_fault_invalidates_presentation,
        completion_drain_needs_display_refresh, installed_target_layer_scales,
        installed_visible_demand_plan_currentness, next_completion_drain_batch,
        runtime_fault_invalidates_verified_source, visible_demand_failure_is_deterministic,
    };
    use crate::DeterministicFailureLatch;

    fn planning_signature(source_id: u64) -> VisibleDemandPlanningSignature {
        let resource_identity =
            DatasetResourceIdentity::Unverified(DatasetSourceId::new(source_id));
        let active_layer = LogicalLayerKey::new(0);
        let camera = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            1.0,
            1.0,
            1.0,
        )
        .unwrap();
        let cross_section =
            CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 1.0, 1.0)
                .unwrap();
        let presentation_viewport = PresentationViewport::new(16.0, 16.0).unwrap();
        let render_extent = RenderExtent::new(16, 16).unwrap();
        VisibleDemandPlanningSignature {
            current_3d: Current3dDemandPlanningSignature {
                resource_identity,
                active_layer,
                timepoint: TimeIndex::new(0),
                camera,
                layout: ViewerLayout::Single3d,
                layers: Box::new([]),
                presentation_viewport,
                render_viewport: render_extent,
                playback_active: false,
                gpu_payload_capacity: 1,
            },
            cross_sections: CrossSectionDemandPlanningSignature {
                resource_identity,
                active_layer,
                timepoint: TimeIndex::new(0),
                layout: ViewerLayout::Single3d,
                cross_section,
                layers: Box::new([]),
                presentation_viewports: [None; 3],
                render_extents: [None; 3],
                playback_active: false,
                gpu_payload_capacity: 1,
            },
        }
    }

    #[test]
    fn staged_refinement_is_the_installed_lod_target_until_promotion() {
        let layer = LogicalLayerKey::new(0);
        let current = BTreeMap::from([(layer, ScaleLevel::new(2))]);
        let refinement = BTreeMap::from([(layer, ScaleLevel::BASE)]);

        assert_eq!(
            installed_target_layer_scales(true, Some(&current), Some(&refinement)),
            Some(&refinement)
        );
        assert_eq!(
            installed_target_layer_scales(false, Some(&current), Some(&refinement)),
            Some(&current)
        );
        assert_eq!(
            installed_target_layer_scales(true, Some(&current), None),
            None
        );
    }

    #[test]
    fn render_eligibility_requires_the_installed_signature_for_each_view_family() {
        let installed = planning_signature(1);
        assert_eq!(
            installed_visible_demand_plan_currentness(Some(&installed), &installed),
            super::VisibleDemandPlanCurrentness {
                current_3d: true,
                cross_sections: true,
            }
        );
        assert_eq!(
            installed_visible_demand_plan_currentness(None, &installed),
            super::VisibleDemandPlanCurrentness::default()
        );

        let mut changed_3d = installed.clone();
        changed_3d.current_3d.render_viewport = RenderExtent::new(32, 16).unwrap();
        assert_eq!(
            installed_visible_demand_plan_currentness(Some(&installed), &changed_3d),
            super::VisibleDemandPlanCurrentness {
                current_3d: false,
                cross_sections: true,
            }
        );

        let mut changed_cross = installed.clone();
        changed_cross.cross_sections.render_extents[0] = Some(RenderExtent::new(32, 16).unwrap());
        assert_eq!(
            installed_visible_demand_plan_currentness(Some(&installed), &changed_cross),
            super::VisibleDemandPlanCurrentness {
                current_3d: true,
                cross_sections: false,
            }
        );
    }

    #[test]
    fn deterministic_planning_failure_runs_once_until_signature_or_capacity_changes() {
        let planning = planning_signature(1);
        let mut latch = None;
        let mut attempts = 0_u64;
        for _ in 0..128 {
            let blocked = latch.as_ref().is_some_and(
                |latch: &DeterministicFailureLatch<VisibleDemandFailureSignature>| {
                    latch.blocks(|failure| {
                        failure.matches_inputs(&planning, true, false, true, 7, 11, true)
                    })
                },
            );
            if blocked {
                continue;
            }
            attempts = attempts.saturating_add(1);
            latch = Some(DeterministicFailureLatch::new(
                VisibleDemandFailureSignature::new(
                    planning.clone(),
                    true,
                    false,
                    true,
                    Vec::new(),
                    7,
                    11,
                ),
            ));
        }
        assert_eq!(attempts, 1, "unchanged UI polls must stay idle");

        let failed = latch.expect("the deterministic failure was latched");
        assert!(!failed.blocks(|failure| failure.matches_inputs(
            &planning_signature(2),
            true,
            false,
            true,
            7,
            11,
            true,
        )));
        assert!(
            !failed.blocks(
                |failure| failure.matches_inputs(&planning, true, false, true, 8, 11, true,)
            )
        );
        assert!(
            !failed.blocks(
                |failure| failure.matches_inputs(&planning, true, false, true, 7, 12, true,)
            )
        );
        assert!(
            !failed.blocks(
                |failure| failure.matches_inputs(&planning, true, false, true, 7, 11, false,)
            )
        );
    }

    #[test]
    fn planning_capacity_is_terminal_but_busy_cancel_and_shutdown_are_retryable() {
        assert!(visible_demand_failure_is_deterministic(
            &anyhow::Error::new(CpuLedgerError::CapacityExceeded {
                category: CpuLedgerCategory::MetadataAndIndexes,
                requested_bytes: 2,
                available_bytes: 1,
            })
        ));
        assert!(!visible_demand_failure_is_deterministic(
            &anyhow::Error::new(RuntimeFault::new(RuntimeFaultCode::QueueFull))
        ));
        assert!(!visible_demand_failure_is_deterministic(
            &anyhow::Error::new(RuntimeFault::new(RuntimeFaultCode::Cancelled))
        ));
        assert!(!visible_demand_failure_is_deterministic(
            &anyhow::Error::new(CpuLedgerError::ShuttingDown)
        ));
    }

    #[test]
    fn draining_cancelled_backlog_retries_queue_blocked_demand() {
        assert!(!completion_drain_needs_display_refresh(false));
        assert!(completion_drain_needs_display_refresh(true));
    }

    #[test]
    fn slow_completion_stream_presents_first_useful_data_before_final_decode_and_is_paced() {
        let started = Instant::now();
        let completion_spacing = Duration::from_millis(2);
        let completion_count = 64_u32;
        let mut pacer = ProgressiveDisplayRefreshPacer::default();
        let mut partial_presentations = Vec::new();

        for completion in 0..completion_count - 1 {
            let now = started + completion_spacing * completion;
            pacer.observe_useful_change();
            if pacer.take_due(now) {
                partial_presentations.push(completion);
            }
        }

        assert_eq!(partial_presentations.first(), Some(&0));
        assert!(
            partial_presentations[0] < completion_count - 1,
            "the first useful presentation must precede the final decode"
        );
        let stream_duration = completion_spacing * (completion_count - 1);
        let maximum_paced_partials =
            stream_duration.as_nanos() / PROGRESSIVE_DISPLAY_REFRESH_INTERVAL.as_nanos() + 1;
        assert!(partial_presentations.len() as u128 <= maximum_paced_partials);

        // Cohort completion is submitted immediately and clears any pending
        // partial, even if it lands inside the pacing interval.
        let final_at = started + stream_duration;
        pacer.observe_useful_change();
        pacer.record_unpaced_refresh(final_at);
        assert!(!pacer.take_due(final_at));
    }

    #[test]
    fn held_complete_presentation_cancels_partial_refresh_until_atomic_promotion() {
        let now = Instant::now();
        let mut pacer = ProgressiveDisplayRefreshPacer::default();
        pacer.observe_useful_change();
        pacer.cancel_pending();
        assert!(!pacer.take_due(now));

        pacer.record_unpaced_refresh(now);
        assert_eq!(pacer.next_due_in(now), None);
    }

    #[test]
    fn ready_burst_ingestion_uses_the_full_bounded_envelope_not_one_presentation_batch() {
        let mut processed = 0;
        let mut polls = 0;
        loop {
            let batch = next_completion_drain_batch(processed, Duration::ZERO);
            if batch == 0 {
                break;
            }
            assert!(batch <= RESULT_DRAIN_BATCH);
            processed += batch;
            polls += 1;
        }

        assert_eq!(processed, RESULT_DRAIN_COUNT_ENVELOPE);
        assert!(processed > 32);
        assert_eq!(
            polls,
            RESULT_DRAIN_COUNT_ENVELOPE.div_ceil(RESULT_DRAIN_BATCH)
        );
        assert_eq!(
            next_completion_drain_batch(RESULT_DRAIN_BATCH, RESULT_DRAIN_TIME_BUDGET),
            0,
            "the UI time budget bounds a burst independently of its count envelope"
        );
    }

    #[test]
    fn only_observed_source_integrity_faults_invalidate_a_verified_binding() {
        assert!(!admission_fault_invalidates_presentation(true));
        assert!(admission_fault_invalidates_presentation(false));
        for code in [
            RuntimeFaultCode::SourceRejected,
            RuntimeFaultCode::CorruptResource,
            RuntimeFaultCode::UnsupportedResource,
            RuntimeFaultCode::DecodeFailed,
        ] {
            assert!(runtime_fault_invalidates_verified_source(code));
        }
        for code in [
            RuntimeFaultCode::QueueFull,
            RuntimeFaultCode::Cancelled,
            RuntimeFaultCode::ShuttingDown,
            RuntimeFaultCode::InvariantViolation,
        ] {
            assert!(!runtime_fault_invalidates_verified_source(code));
        }
    }
}
