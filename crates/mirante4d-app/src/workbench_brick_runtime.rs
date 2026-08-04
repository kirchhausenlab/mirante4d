//! Unified interactive dataset demand and completion delivery.

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui;
use mirante4d_application::{
    PresentationSlot, RenderIntentBase, RenderIntentRevision, RenderIntentTarget,
};
use mirante4d_dataset::{BrickKey, CpuLedgerCategory, CpuLedgerError};
use mirante4d_dataset_runtime::{RuntimeFault, RuntimeFaultCode};
use mirante4d_domain::{CameraView, LogicalLayerKey, ScaleLevel, TimeIndex, ViewerLayout};
use mirante4d_render_api::{
    MAX_RENDER_REQUIREMENTS, PreparedRenderRequirements, PresentationTarget,
};

use crate::{
    BACKGROUND_WORK_REPAINT_INTERVAL, DeterministicFailureLatch, DisplayedFrameFreshness,
    FrameCompleteness, FrameFailureKind, LodDecisionReason, MiranteWorkbenchApp, RenderBackend,
    application_view,
    camera_demand_cache::{
        CameraDemandRequest, CrossSectionDemandRequest, Current3dDemandBaselines,
        Current3dDemandRequest, PreparedCurrent3dDemand, PreparedRendererRequirementUpdate,
        PreparedVisibleDemand, ScopeDemandBaseline,
    },
    dataset_demand_plan::{
        DatasetDemandPlanCapacityError, DatasetDemandPlanLimits, NavigationCandidateBaseline,
        NavigationLadderBaseline, cross_section_projected_layer_scales,
        current_3d_projected_layer_scales, sampling_footprint_class, uniform_layer_scale,
    },
    dataset_requests::{
        DatasetDemandState, RendererEvictionDisposition, SCOPE_ANALYSIS, SCOPE_CROSS_SECTION_XY,
        SCOPE_CROSS_SECTION_XZ, SCOPE_CROSS_SECTION_YZ, SCOPE_CURRENT_3D,
        SCOPE_CURRENT_3D_REFINEMENT, SCOPE_PLAYBACK, ScopeReconciliationTargets,
    },
    display_refresh::{RenderAttemptCoordinator, render_backend_for_view},
    native_presentation::NativePresentationBridge,
    playback_session::PlaybackFrameContract,
    presentation_scheduler::MissingLogicalTarget,
    product_render_intent::PRODUCT_RENDER_RESOURCE_LIMIT,
    retained_leases::RetainedRequirementHandle,
    viewer_layout::PanelId,
};

fn dataset_fidelity_completeness(
    empty: bool,
    displayed_scale_level: Option<u32>,
    display_freshness: DisplayedFrameFreshness,
    presented_completeness: Option<mirante4d_render_api::FrameCompleteness>,
) -> FrameCompleteness {
    if empty {
        FrameCompleteness::Complete
    } else if displayed_scale_level == Some(0)
        && matches!(display_freshness, DisplayedFrameFreshness::Current)
        && matches!(
            presented_completeness,
            Some(mirante4d_render_api::FrameCompleteness::Exact)
        )
    {
        // Dataset readiness must not downgrade the stronger renderer-owned
        // fact already published for the current full S0 presentation.
        FrameCompleteness::Exact
    } else {
        match presented_completeness {
            Some(mirante4d_render_api::FrameCompleteness::Progressive) => {
                FrameCompleteness::Incomplete
            }
            Some(
                mirante4d_render_api::FrameCompleteness::Complete
                | mirante4d_render_api::FrameCompleteness::Exact,
            ) => FrameCompleteness::Complete,
            None => FrameCompleteness::Loading,
        }
    }
}

fn pump_interactive_admission_with_renderer(
    dataset: &mut DatasetDemandState,
    native: &mut NativePresentationBridge,
    render_attempt: &mut RenderAttemptCoordinator,
) -> Result<usize, RuntimeFault> {
    if render_attempt.renderer_is_terminal() {
        return dataset.pump_interactive_admission();
    }
    let Some(product) = native.product_gpu.as_mut() else {
        return dataset.pump_interactive_admission();
    };
    dataset.begin_submission_pass();
    let events = product.renderer.pending_residency_evictions(256);
    let mut acknowledge_through = None;
    for event in events.iter().copied() {
        match dataset.resolve_renderer_eviction(event.key())? {
            RendererEvictionDisposition::Reoffer(lease) => {
                let key = lease.key();
                product
                    .renderer
                    .offer_residency_leases(&[lease])
                    .map_err(|error| {
                        tracing::error!(%error, "renderer rejected an exact eviction reoffer lease");
                        RuntimeFault::new(RuntimeFaultCode::InvariantViolation)
                    })?;
                render_attempt.observe_relevant_residency_offer(key);
                acknowledge_through = Some(event.sequence());
            }
            RendererEvictionDisposition::Submitted
            | RendererEvictionDisposition::NoLongerDemanded => {
                acknowledge_through = Some(event.sequence());
            }
            RendererEvictionDisposition::Deferred => break,
        }
    }
    if let Some(sequence) = acknowledge_through {
        product.renderer.acknowledge_residency_evictions(sequence);
        render_attempt.observe_eviction_acknowledgement(sequence);
    }
    dataset
        .retire_released_cpu_authority_payloads(|key| product.renderer.resource_is_resident(key));
    dataset.pump_interactive_admission_after_begin_with_gpu_residency(|key| {
        product.renderer.resource_is_resident(key)
    })
}

pub(crate) fn scope_complete_with_renderer(
    dataset: &DatasetDemandState,
    native: &NativePresentationBridge,
    scope: u64,
) -> bool {
    let Some(product) = native.product_gpu.as_ref() else {
        return dataset.scope_complete(scope);
    };
    dataset
        .scope_complete_with_gpu_residency(scope, |key| product.renderer.resource_is_resident(key))
}

pub(crate) fn playback_successor_complete_with_renderer(
    dataset: &DatasetDemandState,
    native: &NativePresentationBridge,
) -> bool {
    let Some(product) = native.product_gpu.as_ref() else {
        return dataset.playback_successor_complete_with_gpu_residency(|_| false);
    };
    dataset.playback_successor_complete_with_gpu_residency(|key| {
        product.renderer.resource_is_resident(key)
    })
}

pub(crate) fn scope_resources_complete_with_renderer(
    dataset: &DatasetDemandState,
    native: &NativePresentationBridge,
    scope: u64,
) -> bool {
    let Some(product) = native.product_gpu.as_ref() else {
        return dataset.scope_resources_complete(scope);
    };
    dataset.scope_resources_complete_with_gpu_residency(scope, |key| {
        product.renderer.resource_is_resident(key)
    })
}

pub(crate) fn scope_all_resources_complete_with_renderer(
    dataset: &DatasetDemandState,
    native: &NativePresentationBridge,
    scope: u64,
) -> bool {
    let Some(product) = native.product_gpu.as_ref() else {
        return dataset.scope_all_resources_complete(scope);
    };
    dataset.scope_all_resources_complete_with_gpu_residency(scope, |key| {
        product.renderer.resource_is_resident(key)
    })
}

pub(crate) fn first_useful_resources_complete_with_renderer(
    dataset: &DatasetDemandState,
    native: &NativePresentationBridge,
    plan: &PreparedScopeRenderPlan,
) -> bool {
    let first_useful = plan.requirements.first_useful_prefix_len();
    if first_useful == 0 || first_useful > plan.requirements.body().ranked().len() {
        return false;
    }
    let keys = &plan.requirements.body().ranked()[..first_useful];
    if let Some(product) = native.product_gpu.as_ref() {
        keys.iter()
            .copied()
            .all(|key| product.renderer.resource_is_resident(key))
    } else {
        keys.iter()
            .all(|key| dataset.retained_leases().payload(*key).is_some())
    }
}

pub(crate) fn first_useful_resources_uploadable_with_renderer(
    dataset: &DatasetDemandState,
    native: &NativePresentationBridge,
    plan: &PreparedScopeRenderPlan,
) -> bool {
    let first_useful = plan.requirements.first_useful_prefix_len();
    if first_useful == 0 || first_useful > plan.requirements.body().ranked().len() {
        return false;
    }
    let keys = &plan.requirements.body().ranked()[..first_useful];
    keys.iter().copied().all(|key| {
        dataset.retained_leases().payload(key).is_some()
            || native
                .product_gpu
                .as_ref()
                .is_some_and(|product| product.renderer.resource_is_resident(key))
    })
}

/// Promotes every installed camera-guard tail covered by the reuse envelope.
/// A progressive plan can own both a visible coarse scope and a staged exact
/// scope; preserve-complete planning simply leaves the coarse scope absent.
/// Dataset and renderer boundaries share immutable bodies and change together.
fn promote_installed_camera_guards(
    dataset: &mut DatasetDemandState,
    render_plans: &mut std::collections::BTreeMap<u64, PreparedScopeRenderPlan>,
) -> bool {
    promote_installed_camera_guards_for_scopes(
        dataset,
        render_plans,
        &[SCOPE_CURRENT_3D, SCOPE_CURRENT_3D_REFINEMENT],
    )
}

fn promote_installed_camera_guards_for_scopes(
    dataset: &mut DatasetDemandState,
    render_plans: &mut std::collections::BTreeMap<u64, PreparedScopeRenderPlan>,
    scopes: &[u64],
) -> bool {
    for &scope in scopes {
        if !dataset.scope_is_installed(scope) {
            continue;
        }
        let Some(plan) = render_plans.get(&scope) else {
            return false;
        };
        let render_prefetch_count = plan.requirements.prefetch_resource_count();
        if render_prefetch_count == 0 {
            // The dataset scope may still own a coarse navigation-floor tail.
            // That tail has its own render plan and is not a same-scale camera
            // guard to promote into this target body.
            continue;
        }
        let dataset_has_unpromoted_guard =
            dataset.scope_required_prefix_len(scope) < dataset.scope_requirements(scope).len();
        let render_has_unpromoted_guard = !plan.requirements.prefetch_promoted();
        if dataset_has_unpromoted_guard != render_has_unpromoted_guard {
            return false;
        }
        if render_has_unpromoted_guard && !dataset.can_commit_complete_scope_prefetch_tail(scope) {
            return false;
        }
    }

    for &scope in scopes {
        if !dataset.scope_is_installed(scope) {
            continue;
        }
        let plan = render_plans
            .get_mut(&scope)
            .expect("a preflighted camera scope owns render requirements");
        if plan.requirements.prefetch_resource_count() == 0 || plan.requirements.prefetch_promoted()
        {
            continue;
        }
        dataset.commit_complete_scope_prefetch_tail(scope);
        plan.requirements = plan.requirements.promote_prefetch();
    }
    true
}

pub(crate) fn installed_camera_guard_bodies_are_complete(
    dataset: &DatasetDemandState,
    native: &NativePresentationBridge,
) -> bool {
    installed_camera_guard_scopes_are_complete(
        dataset,
        native,
        &[SCOPE_CURRENT_3D, SCOPE_CURRENT_3D_REFINEMENT],
    )
}

fn installed_camera_guard_scopes_are_complete(
    dataset: &DatasetDemandState,
    native: &NativePresentationBridge,
    scopes: &[u64],
) -> bool {
    let mut installed = false;
    for &scope in scopes {
        if !dataset.scope_is_installed(scope) {
            continue;
        }
        installed = true;
        if !scope_all_resources_complete_with_renderer(dataset, native, scope) {
            return false;
        }
    }
    installed
}

fn commit_preflighted_installed_plane_guard(
    dataset: &mut DatasetDemandState,
    render_plans: &mut BTreeMap<u64, PreparedScopeRenderPlan>,
    scope: u64,
) {
    debug_assert!(installed_plane_guard_is_promotable(
        dataset,
        render_plans,
        scope
    ));
    let plan = render_plans
        .get_mut(&scope)
        .expect("a preflighted plane guard owns render requirements");
    let dataset_has_unpromoted_guard =
        dataset.scope_required_prefix_len(scope) < dataset.scope_requirements(scope).len();
    dataset.commit_complete_scope_prefetch_tail(scope);
    if dataset_has_unpromoted_guard {
        plan.requirements = plan.requirements.promote_prefetch();
    }
}

fn installed_plane_guard_is_promotable(
    dataset: &DatasetDemandState,
    render_plans: &BTreeMap<u64, PreparedScopeRenderPlan>,
    scope: u64,
) -> bool {
    let Some(plan) = render_plans.get(&scope) else {
        return false;
    };
    if plan.plane_reuse_envelope.is_none() {
        return false;
    }
    if !dataset.can_commit_complete_scope_prefetch_tail(scope) {
        return false;
    }
    let dataset_has_unpromoted_guard =
        dataset.scope_required_prefix_len(scope) < dataset.scope_requirements(scope).len();
    let render_has_unpromoted_guard =
        !plan.requirements.prefetch_promoted() && plan.requirements.prefetch_resource_count() != 0;
    dataset_has_unpromoted_guard == render_has_unpromoted_guard
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
    // Product rendering streams the renderer-owned private 3D candidate and
    // performs dataset promotion with its atomic fixed-target revision swap.
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
    pub(crate) cross_section_plan_installed: bool,
    pub(crate) current_changed: bool,
    pub(crate) resident_changed: bool,
    pub(crate) current_frame_ready: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct VisibleDemandPlanCurrentness {
    pub(crate) current_3d: bool,
    pub(crate) cross_sections: bool,
    cross_section_panels: [bool; 3],
}

impl VisibleDemandPlanCurrentness {
    pub(crate) fn cross_section(self, panel: PanelId) -> bool {
        self.cross_section_panels[cross_section_panel_index(panel)]
    }
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
    /// Playback's retained pre-cutover target ordering uses analysis focus as
    /// its priority input. Static demand deliberately stores `None` so an
    /// analysis-only focus change cannot invalidate rendered membership.
    playback_priority_layer: Option<mirante4d_domain::LogicalLayerKey>,
    timepoint: TimeIndex,
    camera: mirante4d_domain::CameraView,
    layout: ViewerLayout,
    layers: Box<[DemandPlanningLayerSignature]>,
    presentation_viewport: mirante4d_render_api::PresentationViewport,
    render_viewport: mirante4d_render_api::RenderExtent,
    playback_active: bool,
    playback_fps: mirante4d_application::PlaybackFps,
    gpu_payload_capacity: u64,
}

impl Current3dDemandPlanningSignature {
    fn same_non_camera_demand(&self, other: &Self) -> bool {
        self.resource_identity == other.resource_identity
            && self.playback_priority_layer == other.playback_priority_layer
            && self.timepoint == other.timepoint
            && self.layout == other.layout
            && self.layers == other.layers
            && self.presentation_viewport == other.presentation_viewport
            && self.render_viewport == other.render_viewport
            && self.playback_active == other.playback_active
            && self.playback_fps == other.playback_fps
            && self.gpu_payload_capacity == other.gpu_payload_capacity
    }
}

#[cfg(test)]
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
struct CrossSectionPanelDemandPlanningSignature {
    cross_section: mirante4d_domain::CrossSectionView,
    presentation_viewport: Option<mirante4d_render_api::PresentationViewport>,
    render_extent: Option<mirante4d_render_api::RenderExtent>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CrossSectionDemandPlanningSignature {
    resource_identity: mirante4d_dataset::DatasetResourceIdentity,
    playback_priority_layer: Option<mirante4d_domain::LogicalLayerKey>,
    timepoint: TimeIndex,
    layout: ViewerLayout,
    layers: Box<[DemandPlanningLayerSignature]>,
    panels: [CrossSectionPanelDemandPlanningSignature; 3],
    playback_active: bool,
    playback_fps: mirante4d_application::PlaybackFps,
    gpu_payload_capacity: u64,
}

impl CrossSectionDemandPlanningSignature {
    fn same_common_demand(&self, other: &Self) -> bool {
        self.resource_identity == other.resource_identity
            && self.playback_priority_layer == other.playback_priority_layer
            && self.timepoint == other.timepoint
            && self.layout == other.layout
            && self.layers == other.layers
            && self.playback_active == other.playback_active
            && self.playback_fps == other.playback_fps
            && self.gpu_payload_capacity == other.gpu_payload_capacity
    }

    fn panel(&self, panel: PanelId) -> &CrossSectionPanelDemandPlanningSignature {
        &self.panels[cross_section_panel_index(panel)]
    }

    fn panel_mut(&mut self, panel: PanelId) -> &mut CrossSectionPanelDemandPlanningSignature {
        &mut self.panels[cross_section_panel_index(panel)]
    }

    fn same_non_geometry_demand(&self, other: &Self) -> bool {
        self.same_common_demand(other)
            && self
                .panels
                .iter()
                .zip(other.panels.iter())
                .all(|(planned, current)| {
                    planned.presentation_viewport == current.presentation_viewport
                        && planned.render_extent == current.render_extent
                })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CrossSectionPlanMask([bool; 3]);

impl CrossSectionPlanMask {
    const ALL: Self = Self([true; 3]);

    fn contains(self, panel: PanelId) -> bool {
        self.0[cross_section_panel_index(panel)]
    }

    fn insert(&mut self, panel: PanelId) {
        self.0[cross_section_panel_index(panel)] = true;
    }

    fn any(self) -> bool {
        self.0.into_iter().any(|required| required)
    }
}

#[derive(Debug, Clone)]
struct PendingCrossSectionDemandPlan {
    target: CrossSectionDemandPlanningSignature,
    panels: CrossSectionPlanMask,
    exact: bool,
}

#[derive(Clone)]
pub(crate) struct PendingVisibleDemandPlan {
    revision: RenderIntentRevision,
    planning: VisibleDemandPlanningSignature,
    current_3d: Option<Current3dDemandPlanningSignature>,
    cross_sections: Option<PendingCrossSectionDemandPlan>,
    union_plan_required: bool,
    preserve_complete_presentation: bool,
    unchanged_scope_handles: Vec<(u64, Arc<[mirante4d_dataset::BrickKey]>)>,
    renderer_requirement_base: RetainedRequirementHandle,
    cpu_capacity_epoch: u64,
    dataset_runtime_epoch: u64,
    temporal_frame_contract: Option<PlaybackFrameContract>,
}

impl PendingVisibleDemandPlan {
    fn target_group(&self) -> &'static str {
        match (self.current_3d.is_some(), self.cross_sections.is_some()) {
            (false, true) => "linked 2D",
            (true, false) => "3D",
            (true, true) | (false, false) => "visible aggregate",
        }
    }
}

fn pending_temporal_plan_accepts_spatial_supersession(
    pending: &PendingVisibleDemandPlan,
    current: &VisibleDemandPlanningSignature,
    active_target: Option<RenderIntentTarget>,
) -> bool {
    if pending.temporal_frame_contract.is_none() {
        return false;
    }
    let camera_only = pending.current_3d.as_ref().is_some_and(|planned| {
        temporal_3d_body_matches_camera_only_change(planned, &current.current_3d, active_target)
    }) && pending.planning.cross_sections == current.cross_sections
        && pending
            .cross_sections
            .as_ref()
            .is_none_or(|cross| cross.target == current.cross_sections);
    let linked_only = matches!(active_target, Some(RenderIntentTarget::CrossSection(_)))
        && pending.current_3d.as_ref() == Some(&current.current_3d)
        && pending
            .planning
            .cross_sections
            .same_non_geometry_demand(&current.cross_sections)
        && pending.cross_sections.as_ref().is_some_and(|cross| {
            cross.panels == CrossSectionPlanMask::ALL
                && cross
                    .target
                    .same_non_geometry_demand(&current.cross_sections)
        });
    camera_only || linked_only
}

fn temporal_3d_body_matches_camera_only_change(
    planned: &Current3dDemandPlanningSignature,
    current: &Current3dDemandPlanningSignature,
    active_target: Option<RenderIntentTarget>,
) -> bool {
    active_target == Some(RenderIntentTarget::ThreeD)
        && planned.playback_active
        && planned.same_non_camera_demand(current)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PayloadPlacementFacts {
    requested_bytes: u64,
    total_free_bytes: u64,
    largest_contiguous_bytes: u64,
}

impl PayloadPlacementFacts {
    fn from_runtime_error(error: mirante4d_render_wgpu::WgpuRenderRuntimeError) -> Option<Self> {
        match error {
            mirante4d_render_wgpu::WgpuRenderRuntimeError::PayloadPlacementUnavailable {
                requested_bytes,
                total_free_bytes,
                largest_contiguous_bytes,
            } => Some(Self {
                requested_bytes,
                total_free_bytes,
                largest_contiguous_bytes,
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisibleDemandPlaceabilityRetry {
    target_group: &'static str,
    failed_union_payload_bytes: u64,
    placement: PayloadPlacementFacts,
}

impl std::fmt::Display for VisibleDemandPlaceabilityRetry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} candidate with {} aggregate payload bytes needs one contiguous \
             {}-byte allocation; {} bytes are free in aggregate but the largest \
             contiguous range is {} bytes",
            self.target_group,
            self.failed_union_payload_bytes,
            self.placement.requested_bytes,
            self.placement.total_free_bytes,
            self.placement.largest_contiguous_bytes,
        )
    }
}

impl std::error::Error for VisibleDemandPlaceabilityRetry {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisibleDemandMinimumPlacementError {
    target_group: &'static str,
    placement: PayloadPlacementFacts,
}

impl std::fmt::Display for VisibleDemandMinimumPlacementError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} minimum navigation demand remains physically unplaceable after \
             bounded compaction: one {}-byte payload requires a contiguous range; \
             {} bytes are free in aggregate and the largest contiguous range is \
             {} bytes",
            self.target_group,
            self.placement.requested_bytes,
            self.placement.total_free_bytes,
            self.placement.largest_contiguous_bytes,
        )
    }
}

impl std::error::Error for VisibleDemandMinimumPlacementError {}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VisibleDemandPlaceabilityLimit {
    planning: VisibleDemandPlanningSignature,
    max_gpu_payload_bytes: u64,
    target_group: &'static str,
    placement: PayloadPlacementFacts,
}

impl VisibleDemandPlaceabilityLimit {
    fn tightened(
        previous: Option<&Self>,
        planning: VisibleDemandPlanningSignature,
        retry: VisibleDemandPlaceabilityRetry,
    ) -> Self {
        let rejected_body_cap = retry.failed_union_payload_bytes.saturating_sub(1);
        let max_gpu_payload_bytes = previous
            .filter(|limit| limit.applies_to(&planning))
            .map_or(rejected_body_cap, |limit| {
                limit.max_gpu_payload_bytes.min(rejected_body_cap)
            });
        Self {
            planning,
            max_gpu_payload_bytes,
            target_group: retry.target_group,
            placement: retry.placement,
        }
    }

    fn applies_to(&self, planning: &VisibleDemandPlanningSignature) -> bool {
        self.planning == *planning
    }

    fn terminal_error(&self) -> VisibleDemandMinimumPlacementError {
        VisibleDemandMinimumPlacementError {
            target_group: self.target_group,
            placement: self.placement,
        }
    }
}

#[derive(Clone)]
pub(crate) struct VisibleDemandFailureSignature {
    revision: RenderIntentRevision,
    planning: VisibleDemandPlanningSignature,
    current_plan_required: bool,
    cross_plan_required: CrossSectionPlanMask,
    union_plan_required: bool,
    unchanged_scope_handles: Vec<(u64, Arc<[mirante4d_dataset::BrickKey]>)>,
    renderer_requirement_base: RetainedRequirementHandle,
    cpu_capacity_epoch: u64,
    dataset_runtime_epoch: u64,
}

impl VisibleDemandFailureSignature {
    #[allow(
        clippy::too_many_arguments,
        reason = "all independent retry-latch identity dimensions must remain explicit"
    )]
    fn new(
        revision: RenderIntentRevision,
        planning: VisibleDemandPlanningSignature,
        current_plan_required: bool,
        cross_plan_required: CrossSectionPlanMask,
        union_plan_required: bool,
        unchanged_scope_handles: Vec<(u64, Arc<[mirante4d_dataset::BrickKey]>)>,
        renderer_requirement_base: RetainedRequirementHandle,
        cpu_capacity_epoch: u64,
        dataset_runtime_epoch: u64,
    ) -> Self {
        Self {
            revision,
            planning,
            current_plan_required,
            cross_plan_required,
            union_plan_required,
            unchanged_scope_handles,
            renderer_requirement_base,
            cpu_capacity_epoch,
            dataset_runtime_epoch,
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "all independent retry-latch identity dimensions must remain explicit"
    )]
    fn matches_current(
        &self,
        revision: RenderIntentRevision,
        planning: &VisibleDemandPlanningSignature,
        current_plan_required: bool,
        cross_plan_required: CrossSectionPlanMask,
        union_plan_required: bool,
        dataset: &DatasetDemandState,
        cpu_capacity_epoch: u64,
        dataset_runtime_epoch: u64,
    ) -> bool {
        self.matches_inputs(
            revision,
            planning,
            current_plan_required,
            cross_plan_required,
            union_plan_required,
            cpu_capacity_epoch,
            dataset_runtime_epoch,
            scope_handles_are_current(dataset, &self.unchanged_scope_handles),
            renderer_requirement_base_is_current(dataset, &self.renderer_requirement_base),
        )
    }

    // Keeping the eight independent identity dimensions visible prevents a
    // partial comparison from silently reopening terminal work.
    #[allow(clippy::too_many_arguments)]
    fn matches_inputs(
        &self,
        revision: RenderIntentRevision,
        planning: &VisibleDemandPlanningSignature,
        current_plan_required: bool,
        cross_plan_required: CrossSectionPlanMask,
        union_plan_required: bool,
        cpu_capacity_epoch: u64,
        dataset_runtime_epoch: u64,
        scope_handles_are_current: bool,
        renderer_requirement_base_is_current: bool,
    ) -> bool {
        self.revision == revision
            && self.planning == *planning
            && self.current_plan_required == current_plan_required
            && self.cross_plan_required == cross_plan_required
            && self.union_plan_required == union_plan_required
            && self.cpu_capacity_epoch == cpu_capacity_epoch
            && self.dataset_runtime_epoch == dataset_runtime_epoch
            && scope_handles_are_current
            && renderer_requirement_base_is_current
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedScopeRenderPlan {
    pub(crate) requirements: PreparedRenderRequirements,
    /// Exact semantic body used to decide whether this candidate is selectable.
    /// A navigation wrapper may carry a larger dormant residency-prefetch body
    /// in `requirements`.
    pub(crate) selection_body: mirante4d_render_api::PreparedResourceBody,
    pub(crate) selection_required_prefix_len: usize,
    /// Exact installed dataset body which owns this render body's resources.
    /// The renderer body may be a single-scale prefix of that residency union.
    pub(crate) scope_requirements: Arc<[BrickKey]>,
    pub(crate) layer_scales: Arc<BTreeMap<LogicalLayerKey, ScaleLevel>>,
    /// Payload bytes referenced by this exact render body, excluding any
    /// independent navigation-tail resources retained by its owning scope.
    pub(crate) render_payload_bytes: u64,
    pub(crate) planned_payload_bytes: u64,
    pub(crate) primary_resource_count: usize,
    /// The selected scales contain every brick of every visible layer. Plane
    /// geometry can therefore change without changing this immutable body.
    pub(crate) covers_full_selected_volumes: bool,
    pub(crate) plane_reuse_envelope: Option<crate::semantic_demand::SemanticPlaneReuseEnvelope>,
}

/// Concrete per-view caches. Installed dataset scope requirements are the
/// cached planner outputs; these signatures independently decide which scope
/// needs to replace its output.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VisibleDemandPlanningSignature {
    current_3d: Current3dDemandPlanningSignature,
    cross_sections: CrossSectionDemandPlanningSignature,
}

/// A complete installed linked-plane body that may be reprojected for one
/// active mailbox revision. This is deliberately separate from installed
/// exact-demand identity: coverage can keep interaction responsive, but it
/// cannot make a different LOD/body current or settled.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ResidentCrossSectionCoverage {
    revision: RenderIntentRevision,
    cross_sections: CrossSectionDemandPlanningSignature,
    exact_body_reuse: bool,
    rolling_replan: bool,
    /// True only while the resident body is a continuity fallback or its
    /// rolling window is near exhaustion. A worker-installed latest body has
    /// already satisfied planning even while its target resources continue
    /// arriving asynchronously.
    planning_required: bool,
}

/// Immutable dataset body bound to the durable exact-demand signature for one
/// linked panel. A transient worker replacement clears this binding; only an
/// exact worker plan or a fully preflighted durable resident promotion can
/// establish it again.
#[derive(Debug, Clone)]
pub(crate) struct InstalledCrossSectionExactBody {
    requirements: Arc<[BrickKey]>,
    layer_scales: Arc<BTreeMap<LogicalLayerKey, ScaleLevel>>,
}

fn installed_visible_demand_plan_currentness(
    installed: Option<&VisibleDemandPlanningSignature>,
    current: &VisibleDemandPlanningSignature,
    installed_cross_section_bodies_are_current: [bool; 3],
) -> VisibleDemandPlanCurrentness {
    let Some(installed) = installed else {
        return VisibleDemandPlanCurrentness::default();
    };
    let cross_section_panels = [PanelId::Xy, PanelId::Xz, PanelId::Yz].map(|panel| {
        installed
            .cross_sections
            .same_common_demand(&current.cross_sections)
            && installed.cross_sections.panel(panel) == current.cross_sections.panel(panel)
            && installed_cross_section_bodies_are_current[cross_section_panel_index(panel)]
    });
    VisibleDemandPlanCurrentness {
        current_3d: installed.current_3d == current.current_3d,
        cross_sections: cross_section_panels.into_iter().all(|current| current),
        cross_section_panels,
    }
}

fn required_cross_section_plan(
    installed: Option<&CrossSectionDemandPlanningSignature>,
    current: &CrossSectionDemandPlanningSignature,
    installed_bodies_are_current: [bool; 3],
) -> CrossSectionPlanMask {
    if current.layout != ViewerLayout::FourPanel {
        return CrossSectionPlanMask::default();
    }
    let Some(installed) = installed else {
        return CrossSectionPlanMask::ALL;
    };
    if !installed.same_common_demand(current) {
        return CrossSectionPlanMask::ALL;
    }
    let mut required = CrossSectionPlanMask::default();
    for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        if installed.panel(panel) != current.panel(panel)
            || !installed_bodies_are_current[cross_section_panel_index(panel)]
        {
            required.insert(panel);
        }
    }
    required
}

impl MiranteWorkbenchApp {
    fn installed_temporal_3d_body_matches(&self, frame: &PlaybackFrameContract) -> bool {
        [SCOPE_CURRENT_3D, SCOPE_CURRENT_3D_REFINEMENT]
            .into_iter()
            .any(|scope| {
                let Some(plan) = self.prepared_scope_render_plans.get(&scope) else {
                    return false;
                };
                let requirements = self.dataset.scope_requirement_handle(scope);
                self.dataset.scope_is_installed(scope)
                    && Arc::ptr_eq(&requirements, &plan.scope_requirements)
                    && plan.covers_full_selected_volumes
                    && plan.requirements.timepoint() == frame.timepoint()
                    && plan.layer_scales.as_ref() == frame.layer_scales().as_ref()
            })
    }

    fn installed_temporal_cross_section_body_matches(
        &self,
        panel: PanelId,
        frame: &PlaybackFrameContract,
    ) -> bool {
        let scope = cross_section_scope_id(panel);
        self.installed_cross_section_full_volume_body_matches_scales(
            panel,
            frame.layer_scales().as_ref(),
        ) && self
            .prepared_scope_render_plans
            .get(&scope)
            .is_some_and(|plan| plan.requirements.timepoint() == frame.timepoint())
    }

    fn installed_cross_section_body_matches_prepared(&self, panel: PanelId) -> bool {
        let scope = cross_section_scope_id(panel);
        if !self.dataset.scope_is_installed(scope)
            || self.dataset.scope_layer_scales(scope).is_none()
        {
            return false;
        }
        let scope_requirements = self.dataset.scope_requirement_handle(scope);
        let Some(plan) = self.prepared_scope_render_plans.get(&scope) else {
            return scope_requirements.is_empty();
        };
        !scope_requirements.is_empty()
            && Arc::ptr_eq(&scope_requirements, &plan.scope_requirements)
            && self.dataset.scope_layer_scales(scope) == Some(plan.layer_scales.as_ref())
    }

    fn installed_cross_section_exact_body_candidate(
        &self,
        panel: PanelId,
    ) -> Option<InstalledCrossSectionExactBody> {
        if !self.installed_cross_section_body_matches_prepared(panel) {
            return None;
        }
        let scope = cross_section_scope_id(panel);
        let requirements = self.dataset.scope_requirement_handle(scope);
        let layer_scales = self
            .prepared_scope_render_plans
            .get(&scope)
            .map(|plan| Arc::clone(&plan.layer_scales))
            .unwrap_or_else(|| {
                Arc::new(
                    self.dataset
                        .scope_layer_scales(scope)
                        .expect("a matched empty scope retains its selected scales")
                        .clone(),
                )
            });
        Some(InstalledCrossSectionExactBody {
            requirements,
            layer_scales,
        })
    }

    fn installed_cross_section_full_volume_body_matches_scales(
        &self,
        panel: PanelId,
        layer_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
    ) -> bool {
        if !self.installed_cross_section_body_matches_prepared(panel) {
            return false;
        }
        let scope = cross_section_scope_id(panel);
        self.prepared_scope_render_plans
            .get(&scope)
            .is_some_and(|plan| {
                plan.covers_full_selected_volumes
                    && plan.layer_scales.as_ref() == layer_scales
                    && plan.requirements.required_prefix_len()
                        == plan.requirements.body().ranked().len()
                    && self.dataset.scope_required_prefix_len(scope)
                        == self.dataset.scope_requirements(scope).len()
            })
    }

    fn installed_cross_section_exact_body_is_current(&self, panel: PanelId) -> bool {
        let scope = cross_section_scope_id(panel);
        let Some(exact) =
            self.installed_cross_section_exact_bodies[cross_section_panel_index(panel)].as_ref()
        else {
            return false;
        };
        if !self.dataset.scope_is_installed(scope)
            || !Arc::ptr_eq(
                &self.dataset.scope_requirement_handle(scope),
                &exact.requirements,
            )
            || self.dataset.scope_layer_scales(scope) != Some(exact.layer_scales.as_ref())
        {
            return false;
        }
        if exact.requirements.is_empty() {
            return !self.prepared_scope_render_plans.contains_key(&scope);
        }
        self.prepared_scope_render_plans
            .get(&scope)
            .is_some_and(|plan| {
                Arc::ptr_eq(&plan.scope_requirements, &exact.requirements)
                    && plan.layer_scales.as_ref() == exact.layer_scales.as_ref()
            })
    }

    fn installed_cross_section_exact_bodies_are_current(&self) -> [bool; 3] {
        [PanelId::Xy, PanelId::Xz, PanelId::Yz]
            .map(|panel| self.installed_cross_section_exact_body_is_current(panel))
    }

    fn resident_cross_section_coverage_matches_current(
        &self,
        snapshot: &mirante4d_application::ApplicationSnapshot,
        current: &VisibleDemandPlanningSignature,
    ) -> bool {
        let Some(coverage) = self.resident_cross_section_coverage.as_ref() else {
            return false;
        };
        let base = RenderIntentBase::from_snapshot(snapshot);
        self.render_intent_mailbox.active_composed_revision(base) == Some(coverage.revision)
            && matches!(
                self.render_intent_mailbox.active_target(base),
                Some(RenderIntentTarget::CrossSection(_))
            )
            && coverage.cross_sections == current.cross_sections
            && [PanelId::Xy, PanelId::Xz, PanelId::Yz]
                .into_iter()
                .all(|panel| self.installed_cross_section_body_matches_prepared(panel))
    }

    fn resident_cross_section_coverage_is_renderable(
        &self,
        snapshot: &mirante4d_application::ApplicationSnapshot,
        current: &VisibleDemandPlanningSignature,
    ) -> bool {
        self.resident_cross_section_coverage_matches_current(snapshot, current)
            && [PanelId::Xy, PanelId::Xz, PanelId::Yz]
                .into_iter()
                .all(|panel| {
                    self.prepared_scope_render_plans
                        .get(&cross_section_scope_id(panel))
                        .is_some_and(|plan| {
                            first_useful_resources_complete_with_renderer(
                                &self.dataset,
                                &self.native_presentation,
                                plan,
                            )
                        })
                })
    }

    pub(crate) fn visible_demand_plan_currentness(&self) -> VisibleDemandPlanCurrentness {
        let snapshot = self.application.snapshot();
        let (view, active_target, _) = self.effective_visible_demand_inputs(&snapshot);
        let current = self.visible_demand_planning_signature(&snapshot, &view, active_target);
        installed_visible_demand_plan_currentness(
            self.visible_demand_planning_signature.as_ref(),
            &current,
            self.installed_cross_section_exact_bodies_are_current(),
        )
    }

    /// True when the installed target-local planning result for `panel`
    /// belongs to the current effective geometry and is explicitly empty.
    ///
    /// This is weaker than durable exact-body currentness only in one useful
    /// way: an active transient interaction may own a current empty terminal
    /// result without installing a renderable `PreparedRenderRequirements`
    /// body. Empty has no GPU body to prepare, but it still needs the same
    /// planning-signature proof before old pixels may be cleared.
    pub(crate) fn current_cross_section_empty_result(&self, panel: PanelId) -> bool {
        let snapshot = self.application.snapshot();
        let (view, active_target, _) = self.effective_visible_demand_inputs(&snapshot);
        let current = self.visible_demand_planning_signature(&snapshot, &view, active_target);
        let Some(installed) = self.visible_demand_planning_signature.as_ref() else {
            return false;
        };
        let scope = cross_section_scope_id(panel);
        installed
            .cross_sections
            .same_common_demand(&current.cross_sections)
            && installed.cross_sections.panel(panel) == current.cross_sections.panel(panel)
            && self.installed_cross_section_body_matches_prepared(panel)
            && self.dataset.scope_is_empty(scope)
    }

    pub(crate) fn resident_cross_section_requires_planning(&self) -> bool {
        self.resident_cross_section_coverage
            .as_ref()
            .is_none_or(|coverage| coverage.planning_required)
    }

    /// Geometry/body eligibility for issuing a frame. Unlike exact
    /// currentness, this may accept a complete resident guard while its
    /// matching mailbox interaction remains active.
    pub(crate) fn visible_demand_renderability(&self) -> VisibleDemandPlanCurrentness {
        let snapshot = self.application.snapshot();
        let (view, active_target, _) = self.effective_visible_demand_inputs(&snapshot);
        let current = self.visible_demand_planning_signature(&snapshot, &view, active_target);
        let mut renderability = installed_visible_demand_plan_currentness(
            self.visible_demand_planning_signature.as_ref(),
            &current,
            self.installed_cross_section_exact_bodies_are_current(),
        );
        if self.resident_cross_section_coverage_is_renderable(&snapshot, &current) {
            renderability.cross_section_panels = [true; 3];
            renderability.cross_sections = true;
        }
        renderability
    }

    /// Starts or replaces production demand for the exact mailbox revision.
    /// Geometry remains owned by the mailbox; this method merely causes the
    /// existing data-plane authority to observe and plan its latest snapshot.
    pub(crate) fn request_transient_visible_demand(
        &mut self,
        revision: RenderIntentRevision,
        target: RenderIntentTarget,
    ) -> VisibleBrickRequestOutcome {
        let snapshot = self.application.snapshot();
        let base = RenderIntentBase::from_snapshot(&snapshot);
        if self.render_intent_mailbox.active_revision(base) != Some(revision)
            || self.render_intent_mailbox.active_target(base) != Some(target)
        {
            return VisibleBrickRequestOutcome::default();
        }
        self.request_visible_bricks()
    }

    /// Returns whether the installed selected 3D body can cover `camera`
    /// without changing scale or admitting any new resource.
    ///
    /// This is deliberately stricter than navigation-floor renderability:
    /// both the semantic camera envelope and the projected LOD must remain
    /// valid. This establishes data readiness only; the volume-presentation
    /// controller separately decides whether the next frame is direct,
    /// bounded-preview, or an atomic exact-refinement tile.
    pub(crate) fn resident_camera_target_body_is_complete(&self, camera: CameraView) -> bool {
        let snapshot = self.application.snapshot();
        let durable = application_view(&snapshot);
        let candidate_view = mirante4d_project_model::ViewState::new(
            durable.layers().to_vec(),
            durable.active_layer(),
            durable.timepoint(),
            camera,
            durable.layout(),
            *durable.cross_section(),
            *durable.iso_light(),
        )
        .expect("a validated camera and durable view form a valid transient view");
        let candidate = self.visible_demand_planning_signature(
            &snapshot,
            &candidate_view,
            Some(RenderIntentTarget::ThreeD),
        );
        let Some(installed) = self.visible_demand_planning_signature.as_ref() else {
            return false;
        };
        if !installed
            .current_3d
            .same_non_camera_demand(&candidate.current_3d)
        {
            return false;
        }
        let projected_lod_is_unchanged = self.playback_session.contract().is_some_and(|contract| {
            self.dataset
                .scope_layer_scales(SCOPE_CURRENT_3D_REFINEMENT)
                .or_else(|| self.dataset.scope_layer_scales(SCOPE_CURRENT_3D))
                == Some(contract.layer_scales().as_ref())
        }) || current_3d_projected_layer_scales(
            snapshot.catalog(),
            &candidate_view,
            candidate.current_3d.presentation_viewport,
            candidate.current_3d.render_viewport,
            candidate.current_3d.playback_active,
        )
        .is_ok_and(|ideal| self.dataset.current_ideal_layer_scales() == &ideal);
        let camera_is_contained = self.dataset.current_covers_full_volume()
            || self
                .current_camera_reuse_envelope
                .as_ref()
                .is_some_and(|envelope| {
                    mirante4d_render_api::CameraFrame::new(
                        camera,
                        candidate.current_3d.presentation_viewport,
                    )
                    .ok()
                    .and_then(|frame| {
                        envelope
                            .contains(frame, candidate.current_3d.render_viewport)
                            .ok()
                    })
                    .unwrap_or(false)
                });
        projected_lod_is_unchanged
            && camera_is_contained
            && installed_camera_guard_bodies_are_complete(&self.dataset, &self.native_presentation)
    }

    /// Prepares an already-resident immutable body for a transient camera.
    ///
    /// A complete selected body wins the readiness decision. Guard promotion
    /// only changes the role bitmap of already-resident resources; it performs
    /// no decode, upload, or allocation. The full-volume floor is used only
    /// when that body cannot truthfully cover the new camera. Frame-work
    /// scheduling remains the responsibility of the volume-presentation
    /// controller.
    pub(crate) fn prepare_resident_camera_intent(&mut self, camera: CameraView) -> bool {
        let target_body_complete = self.resident_camera_target_body_is_complete(camera);
        if target_body_complete
            && (self.dataset.current_covers_full_volume()
                || promote_installed_camera_guards(
                    &mut self.dataset,
                    &mut self.prepared_scope_render_plans,
                ))
        {
            return true;
        }
        if self.playback_session.contract().is_some() {
            // Playback owns one fixed, full-volume target body. A spatial
            // sample must wait for that body rather than escape through an
            // unrelated coarser navigation rung and visibly change LOD.
            return false;
        }
        let effective_timepoint = application_view(&self.application.snapshot()).timepoint();
        self.navigation_render_plans.first().is_some_and(|plan| {
            plan.requirements.timepoint() == effective_timepoint
                && first_useful_resources_complete_with_renderer(
                    &self.dataset,
                    &self.native_presentation,
                    plan,
                )
        })
    }

    /// Rebinds a cross-section mailbox revision to the already-complete
    /// immutable bodies of all three linked panels. Geometry, common demand,
    /// the gesture LOD, and every body-derived containment proof are checked
    /// before the latest-only planner is invalidated.
    pub(crate) fn prepare_resident_cross_section_intent(
        &mut self,
        revision: RenderIntentRevision,
        panel: PanelId,
        cross_section: mirante4d_domain::CrossSectionView,
    ) -> bool {
        self.resident_cross_section_coverage = None;
        if panel.cross_section_panel().is_none() {
            return false;
        }
        let snapshot = self.application.snapshot();
        let base = RenderIntentBase::from_snapshot(&snapshot);
        if self.render_intent_mailbox.active_revision(base) != Some(revision)
            || !self
                .render_intent_mailbox
                .active_target(base)
                .is_some_and(|target| {
                    matches!(
                        target,
                        RenderIntentTarget::CrossSection(active)
                            if PanelId::from_application_panel(active) == panel
                    )
                })
            || self
                .render_intent_mailbox
                .effective_cross_section(base, *application_view(&snapshot).cross_section())
                != cross_section
        {
            return false;
        }
        let (effective_view, active_target, _) = self.effective_visible_demand_inputs(&snapshot);
        let current =
            self.visible_demand_planning_signature(&snapshot, &effective_view, active_target);
        let Some(installed) = self.visible_demand_planning_signature.as_ref() else {
            return false;
        };
        if !installed
            .cross_sections
            .same_common_demand(&current.cross_sections)
        {
            return false;
        }
        let active_scope = cross_section_scope_id(panel);
        if !self
            .dataset
            .can_observe_visible_intent(revision, Some(active_scope))
        {
            return false;
        }
        let mut reusable_candidates = 0_usize;
        let playback_scales = self
            .playback_session
            .contract()
            .map(|contract| contract.layer_scales().as_ref());
        let mut exact_body_reuse = true;
        let mut all_panels_renderable = true;
        let mut rolling_replan = false;
        for linked_panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
            let current_panel = current.cross_sections.panel(linked_panel);
            let (Some(presentation), Some(extent)) = (
                current_panel.presentation_viewport,
                current_panel.render_extent,
            ) else {
                return false;
            };
            let scope = cross_section_scope_id(linked_panel);
            let Some(plan) = self.prepared_scope_render_plans.get(&scope) else {
                return false;
            };
            let panel_renderable = first_useful_resources_complete_with_renderer(
                &self.dataset,
                &self.native_presentation,
                plan,
            );
            let full_volume_body = playback_scales.is_some_and(|scales| {
                self.installed_cross_section_full_volume_body_matches_scales(linked_panel, scales)
            });
            if !full_volume_body && !panel_renderable {
                return false;
            }
            all_panels_renderable &= panel_renderable;
            let semantic_panel = match linked_panel {
                PanelId::Xy => crate::semantic_demand::CrossSectionPlane::Xy,
                PanelId::Xz => crate::semantic_demand::CrossSectionPlane::Xz,
                PanelId::Yz => crate::semantic_demand::CrossSectionPlane::Yz,
                PanelId::ThreeD => unreachable!(),
            };
            let guarded_panel = !full_volume_body
                && plan.plane_reuse_envelope.as_ref().is_some_and(|envelope| {
                    envelope
                        .contains(cross_section, semantic_panel, presentation, extent)
                        .unwrap_or(false)
                        && scope_all_resources_complete_with_renderer(
                            &self.dataset,
                            &self.native_presentation,
                            scope,
                        )
                        && installed_plane_guard_is_promotable(
                            &self.dataset,
                            &self.prepared_scope_render_plans,
                            scope,
                        )
                });
            let exact_panel = full_volume_body || guarded_panel;
            exact_body_reuse &= exact_panel;
            if guarded_panel {
                let envelope = plan
                    .plane_reuse_envelope
                    .as_ref()
                    .expect("an exact guarded panel owns its envelope");
                rolling_replan |= envelope
                    .needs_rolling_replan(cross_section, semantic_panel, presentation, extent)
                    .unwrap_or(true);
                reusable_candidates =
                    reusable_candidates.saturating_add(envelope.reusable_candidates());
            } else if full_volume_body {
                reusable_candidates =
                    reusable_candidates.saturating_add(plan.primary_resource_count);
            }
        }
        if exact_body_reuse {
            if !self
                .dataset
                .can_install_visible_intent(revision, active_scope)
            {
                return false;
            }
            for linked_panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
                let scope = cross_section_scope_id(linked_panel);
                if self
                    .prepared_scope_render_plans
                    .get(&scope)
                    .is_some_and(|plan| plan.plane_reuse_envelope.is_some())
                {
                    commit_preflighted_installed_plane_guard(
                        &mut self.dataset,
                        &mut self.prepared_scope_render_plans,
                        scope,
                    );
                }
            }
            self.dataset
                .commit_preflighted_installed_visible_intent(revision, active_scope);
            self.pending_visible_demand_plan = None;
            self.visible_demand_failure_latch = None;
            self.dataset
                .record_contained_visible_demand_reuse(reusable_candidates);
            self.resident_plane_guard_reuses = self.resident_plane_guard_reuses.saturating_add(3);
        }
        self.resident_cross_section_coverage = Some(ResidentCrossSectionCoverage {
            revision: self
                .render_intent_mailbox
                .active_composed_revision(base)
                .unwrap_or(revision),
            cross_sections: current.cross_sections,
            exact_body_reuse,
            rolling_replan,
            planning_required: !exact_body_reuse || rolling_replan,
        });
        self.render_coordination.invalidate_cross_sections();
        all_panels_renderable
    }

    /// Rebinds all three linked panels after the one settled durable
    /// cross-section commit when every panel's installed guard proves the new
    /// geometry. This avoids turning gesture completion into two passive-panel
    /// worker rebuilds.
    pub(crate) fn prepare_resident_durable_cross_sections(&mut self) -> bool {
        self.resident_cross_section_coverage = None;
        let snapshot = self.application.snapshot();
        if application_view(&snapshot).layout() != ViewerLayout::FourPanel {
            return false;
        }
        let base = RenderIntentBase::from_snapshot(&snapshot);
        if self.render_intent_mailbox.active_target(base).is_some() {
            return false;
        }
        let effective_view = application_view(&snapshot).clone();
        let current = self.visible_demand_planning_signature(&snapshot, &effective_view, None);
        let Some(installed) = self.visible_demand_planning_signature.as_ref() else {
            return false;
        };
        if !installed
            .cross_sections
            .same_common_demand(&current.cross_sections)
        {
            return false;
        }

        let mut reusable_candidates = 0_usize;
        for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
            let signature = current.cross_sections.panel(panel);
            let (Some(presentation), Some(extent)) =
                (signature.presentation_viewport, signature.render_extent)
            else {
                return false;
            };
            let scope = cross_section_scope_id(panel);
            let expected_scales = self.playback_session.contract().map_or_else(
                || {
                    cross_section_projected_layer_scales(
                        snapshot.catalog(),
                        &effective_view,
                        panel,
                        presentation,
                        extent,
                    )
                    .ok()
                },
                |contract| Some(contract.layer_scales().as_ref().clone()),
            );
            if expected_scales
                .as_ref()
                .is_none_or(|selected| self.dataset.scope_layer_scales(scope) != Some(selected))
            {
                return false;
            }
            // Renderer residency is the authoritative completeness proof and
            // advances the cached all-resource cursor used by promotion.
            // Passive linked panels may not have observed that body since it
            // was uploaded, so this proof must precede the O(1) promotability
            // check.
            if !scope_all_resources_complete_with_renderer(
                &self.dataset,
                &self.native_presentation,
                scope,
            ) {
                return false;
            }
            let full_volume_body = expected_scales.as_ref().is_some_and(|scales| {
                self.installed_cross_section_full_volume_body_matches_scales(panel, scales)
            });
            let semantic_panel = match panel {
                PanelId::Xy => crate::semantic_demand::CrossSectionPlane::Xy,
                PanelId::Xz => crate::semantic_demand::CrossSectionPlane::Xz,
                PanelId::Yz => crate::semantic_demand::CrossSectionPlane::Yz,
                PanelId::ThreeD => unreachable!(),
            };
            if full_volume_body {
                reusable_candidates = reusable_candidates.saturating_add(
                    self.prepared_scope_render_plans
                        .get(&scope)
                        .expect("the full-volume scope owns a render plan")
                        .primary_resource_count,
                );
            } else {
                if !installed_plane_guard_is_promotable(
                    &self.dataset,
                    &self.prepared_scope_render_plans,
                    scope,
                ) {
                    return false;
                }
                let envelope = self
                    .prepared_scope_render_plans
                    .get(&scope)
                    .and_then(|plan| plan.plane_reuse_envelope.as_ref())
                    .expect("the checked guarded plane scope retains its envelope");
                if !envelope
                    .contains(
                        *effective_view.cross_section(),
                        semantic_panel,
                        presentation,
                        extent,
                    )
                    .unwrap_or(false)
                {
                    return false;
                }
                reusable_candidates =
                    reusable_candidates.saturating_add(envelope.reusable_candidates());
            }
        }

        let revision = self.render_intent_mailbox.snapshot().linked_2d_revision;
        if !self.dataset.can_observe_visible_intent(revision, None) {
            return false;
        }
        let exact_bodies = [PanelId::Xy, PanelId::Xz, PanelId::Yz]
            .map(|panel| self.installed_cross_section_exact_body_candidate(panel));
        if exact_bodies.iter().any(Option::is_none) {
            return false;
        }
        for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
            let scope = cross_section_scope_id(panel);
            if self
                .prepared_scope_render_plans
                .get(&scope)
                .is_some_and(|plan| plan.plane_reuse_envelope.is_some())
            {
                commit_preflighted_installed_plane_guard(
                    &mut self.dataset,
                    &mut self.prepared_scope_render_plans,
                    scope,
                );
            }
        }
        self.dataset
            .commit_preflighted_visible_intent(revision, None);
        self.pending_visible_demand_plan = None;
        self.visible_demand_failure_latch = None;
        self.visible_demand_planning_signature
            .as_mut()
            .expect("the checked installed signature remains present")
            .cross_sections = current.cross_sections;
        self.installed_cross_section_exact_bodies = exact_bodies;
        self.dataset
            .record_contained_visible_demand_reuse(reusable_candidates);
        self.resident_plane_guard_reuses = self.resident_plane_guard_reuses.saturating_add(3);
        true
    }

    fn physical_gpu_payload_capacity(&self) -> u64 {
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

    fn selected_gpu_payload_capacity(&self, planning: &VisibleDemandPlanningSignature) -> u64 {
        let physical = self.physical_gpu_payload_capacity();
        self.visible_demand_placeability_limit
            .as_ref()
            .filter(|limit| limit.applies_to(planning))
            .map_or(physical, |limit| physical.min(limit.max_gpu_payload_bytes))
    }

    fn activate_visible_demand_placeability_fallback(
        &mut self,
        planning: VisibleDemandPlanningSignature,
        retry: VisibleDemandPlaceabilityRetry,
    ) {
        let limit = VisibleDemandPlaceabilityLimit::tightened(
            self.visible_demand_placeability_limit.as_ref(),
            planning,
            retry,
        );
        tracing::info!(
            target_group = retry.target_group,
            failed_union_payload_bytes = retry.failed_union_payload_bytes,
            requested_allocation_bytes = retry.placement.requested_bytes,
            aggregate_free_bytes = retry.placement.total_free_bytes,
            largest_contiguous_bytes = retry.placement.largest_contiguous_bytes,
            next_payload_limit_bytes = limit.max_gpu_payload_bytes,
            "renderer placement refused one visible-demand candidate; retrying generic adaptive selection"
        );
        self.visible_demand_placeability_limit = Some(limit);
        self.visible_demand_failure_latch = None;
        self.render_coordination.request_refresh();
    }

    pub(crate) fn recover_from_coordinated_payload_placement(
        &mut self,
        target_group: &'static str,
        error: mirante4d_render_wgpu::WgpuRenderRuntimeError,
    ) -> bool {
        let Some(placement) = PayloadPlacementFacts::from_runtime_error(error) else {
            return false;
        };
        let Some(planning) = self.visible_demand_planning_signature.clone() else {
            return false;
        };
        let requirements = self.dataset.renderer_requirement_handle().requirements;
        let snapshot = self.application.snapshot();
        let Ok(failed_union_payload_bytes) = requirements.iter().try_fold(0_u64, |total, key| {
            let descriptor = snapshot.catalog().resource_payload_descriptor(*key)?;
            let bytes = mirante4d_render_wgpu::payload_allocation_bytes(descriptor)
                .map_err(anyhow::Error::from)?;
            total
                .checked_add(bytes)
                .ok_or_else(|| anyhow::anyhow!("GPU payload accounting overflow"))
        }) else {
            return false;
        };
        if failed_union_payload_bytes == 0 {
            return false;
        }
        self.activate_visible_demand_placeability_fallback(
            planning,
            VisibleDemandPlaceabilityRetry {
                target_group,
                failed_union_payload_bytes,
                placement,
            },
        );
        // The installed bodies and current complete fronts remain intact, but
        // their aggregate union has proved physically unusable. Removing only
        // the semantic-currentness certificate forces the ordinary planner to
        // replace that union under the stricter bound.
        self.visible_demand_planning_signature = None;
        self.resident_cross_section_coverage = None;
        self.current_camera_reuse_envelope = None;
        true
    }

    fn physical_minimum_error_if_limited(
        &self,
        planning: &VisibleDemandPlanningSignature,
        error: anyhow::Error,
    ) -> anyhow::Error {
        if error
            .downcast_ref::<DatasetDemandPlanCapacityError>()
            .is_some_and(|error| error.is_gpu_payload_capacity())
            && let Some(limit) = self
                .visible_demand_placeability_limit
                .as_ref()
                .filter(|limit| limit.applies_to(planning))
        {
            return anyhow::Error::new(limit.terminal_error());
        }
        error
    }

    pub(crate) fn effective_visible_demand_inputs(
        &self,
        snapshot: &mirante4d_application::ApplicationSnapshot,
    ) -> (
        mirante4d_project_model::ViewState,
        Option<RenderIntentTarget>,
        RenderIntentRevision,
    ) {
        let durable = application_view(snapshot);
        let base = RenderIntentBase::from_snapshot(snapshot);
        let active_target = self.render_intent_mailbox.active_target(base);
        let revision = self
            .render_intent_mailbox
            .active_composed_revision(base)
            .unwrap_or_else(|| self.render_intent_mailbox.snapshot().latest_revision);
        let camera = self
            .render_intent_mailbox
            .effective_camera(base, *durable.camera());
        let cross_section = self
            .render_intent_mailbox
            .effective_cross_section(base, *durable.cross_section());
        let view = mirante4d_project_model::ViewState::new(
            durable.layers().to_vec(),
            durable.active_layer(),
            durable.timepoint(),
            camera,
            durable.layout(),
            cross_section,
            *durable.iso_light(),
        )
        .expect("mailbox geometry and the durable view are already validated");
        (view, active_target, revision)
    }

    fn visible_demand_planning_signature(
        &self,
        snapshot: &mirante4d_application::ApplicationSnapshot,
        effective_view: &mirante4d_project_model::ViewState,
        active_target: Option<RenderIntentTarget>,
    ) -> VisibleDemandPlanningSignature {
        let durable_view = application_view(snapshot);
        let panels = [PanelId::Xy, PanelId::Xz, PanelId::Yz];
        let layers = effective_view
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
        let linked_interaction_active = active_target
            .is_some_and(|target| matches!(target, RenderIntentTarget::CrossSection(_)));
        let panel_signatures = panels.map(|panel| {
            let surface = self.render_coordination.surface(panel.presentation_slot());
            CrossSectionPanelDemandPlanningSignature {
                cross_section: if linked_interaction_active {
                    *effective_view.cross_section()
                } else {
                    *durable_view.cross_section()
                },
                presentation_viewport: surface.presentation_viewport(),
                render_extent: surface.render_viewport(),
            }
        });
        let gpu_payload_capacity = self.physical_gpu_payload_capacity();
        let playback_active = snapshot.transient().playback_active();
        let playback_priority_layer = playback_active.then_some(effective_view.active_layer());
        VisibleDemandPlanningSignature {
            current_3d: Current3dDemandPlanningSignature {
                resource_identity: snapshot.catalog().resource_identity(),
                playback_priority_layer,
                timepoint: effective_view.timepoint(),
                camera: *effective_view.camera(),
                layout: effective_view.layout(),
                layers: layers.clone(),
                presentation_viewport: self.render_coordination.presentation_viewport,
                render_viewport: self.render_coordination.render_viewport,
                playback_active,
                playback_fps: snapshot.transient().playback_fps(),
                gpu_payload_capacity,
            },
            cross_sections: CrossSectionDemandPlanningSignature {
                resource_identity: snapshot.catalog().resource_identity(),
                playback_priority_layer,
                timepoint: effective_view.timepoint(),
                layout: effective_view.layout(),
                layers,
                panels: panel_signatures,
                playback_active,
                playback_fps: snapshot.transient().playback_fps(),
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
        match pump_interactive_admission_with_renderer(
            &mut self.dataset,
            &mut self.native_presentation,
            &mut self.render_attempt,
        ) {
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
        if ready && self.visible_demand_plan_currentness().current_3d {
            let snapshot = self.application.snapshot();
            let base = RenderIntentBase::from_snapshot(&snapshot);
            if self.render_intent_mailbox.active_target(base) == Some(RenderIntentTarget::ThreeD)
                && let Some(revision) = self.render_intent_mailbox.active_revision(base)
            {
                self.render_intent_mailbox.mark_renderable(base, revision);
            }
        }
        self.update_dataset_fidelity(ready);
        VisibleBrickRequestOutcome {
            current_plan_installed: false,
            cross_section_plan_installed: false,
            current_changed: promoted,
            resident_changed: false,
            current_frame_ready: ready,
        }
    }

    pub(crate) fn request_visible_bricks(&mut self) -> VisibleBrickRequestOutcome {
        let snapshot = self.application.snapshot();
        let (effective_view, active_target, intent_revision) =
            self.effective_visible_demand_inputs(&snapshot);
        let active_scope = active_target.map(render_intent_scope);
        if self.dataset.source_quarantined()
            || self.dataset.resource_identity() != snapshot.catalog().resource_identity()
        {
            if self.pending_visible_demand_plan.is_some()
                || self.dataset.visible_demand_plan_outstanding()
            {
                self.dataset.invalidate_visible_demand_plan(intent_revision);
            }
            self.pending_visible_demand_plan = None;
            self.render_coordination.frame_fidelity.completeness = FrameCompleteness::Loading;
            self.render_coordination.frame_fidelity.reason = LodDecisionReason::LoadingTargetScale;
            self.render_coordination.frame_fidelity.backend = RenderBackend::Loading;
            return VisibleBrickRequestOutcome::default();
        }

        let planning_signature =
            self.visible_demand_planning_signature(&snapshot, &effective_view, active_target);
        let temporal_frame_contract = self
            .playback_session
            .pending_frame_contract(effective_view.timepoint());
        let retained_quality_handoff = self
            .presentation_scheduler
            .retained_quality_active(&snapshot, self.render_intent_mailbox.snapshot());
        // Playback installs geometry-independent full-volume bodies. Once a
        // timepoint replacement is in flight, camera or linked-plane samples
        // cannot change its resource selection and must not continually
        // cancel the worker. The completed immutable body is rebound to the
        // newest spatial revision below before it can become renderable. The CPU capacity
        // epoch is deliberately absent from this validity proof: it is an
        // advisory retry signal for a previously refused reservation, and
        // ordinary lease release while this successful request is in flight
        // cannot invalidate the request's owned ledger authority.
        let pending_temporal_spatial_rebase = self
            .pending_visible_demand_plan
            .as_ref()
            .is_some_and(|pending| {
                pending.revision < intent_revision
                    && pending_temporal_plan_accepts_spatial_supersession(
                        pending,
                        &planning_signature,
                        active_target,
                    )
                    && pending.dataset_runtime_epoch == self.dataset_runtime_epoch
                    && unchanged_scope_handles_are_current(&self.dataset, pending)
                    && renderer_requirement_base_is_current(
                        &self.dataset,
                        &pending.renderer_requirement_base,
                    )
            });
        if !pending_temporal_spatial_rebase
            && !self
                .dataset
                .observe_visible_intent(intent_revision, active_scope)
        {
            return VisibleBrickRequestOutcome::default();
        }
        if self
            .visible_demand_placeability_limit
            .as_ref()
            .is_some_and(|limit| !limit.applies_to(&planning_signature))
        {
            self.visible_demand_placeability_limit = None;
        }
        let mut accepted = VisibleBrickRequestOutcome::default();
        if let Some(result) = self.dataset.take_visible_demand_plan_result() {
            self.last_camera_demand_planning_duration = Some(result.planning_duration);
            let pending = self.pending_visible_demand_plan.take();
            let result_is_current = pending
                .as_ref()
                .is_some_and(|pending| pending.revision == result.revision)
                && (result.revision == intent_revision || pending_temporal_spatial_rebase)
                && pending
                    .as_ref()
                    .and_then(|pending| pending.current_3d.as_ref())
                    .is_none_or(|current| {
                        current == &planning_signature.current_3d || pending_temporal_spatial_rebase
                    })
                && pending
                    .as_ref()
                    .and_then(|pending| pending.cross_sections.as_ref())
                    .is_none_or(|cross| {
                        cross.target == planning_signature.cross_sections
                            || pending_temporal_spatial_rebase
                    })
                && pending.as_ref().is_some_and(|pending| {
                    unchanged_scope_handles_are_current(&self.dataset, pending)
                        && renderer_requirement_base_is_current(
                            &self.dataset,
                            &pending.renderer_requirement_base,
                        )
                });
            if result_is_current {
                match result.outcome {
                    Ok(prepared) => {
                        let pending = pending.expect("a current result has pending identity");
                        match self.install_prepared_visible_demand(prepared, &pending) {
                            Ok(outcome) => {
                                if pending_temporal_spatial_rebase {
                                    // The worker selected geometry-independent
                                    // full-volume bodies under the temporal
                                    // revision that launched it. Publication
                                    // is owned by the latest spatial revision.
                                    self.visible_demand_planning_signature =
                                        Some(planning_signature.clone());
                                    if !self
                                        .dataset
                                        .observe_visible_intent(intent_revision, active_scope)
                                    {
                                        self.dataset.record_plan_error(
                                            "playback spatial rebase rejected the latest render intent",
                                        );
                                        return VisibleBrickRequestOutcome::default();
                                    }
                                    if let Some(scope) = active_scope
                                        && self.dataset.scope_is_installed(scope)
                                    {
                                        debug_assert!(
                                            self.dataset.activate_installed_visible_intent(
                                                intent_revision,
                                                scope,
                                            )
                                        );
                                    }
                                    if matches!(
                                        active_target,
                                        Some(RenderIntentTarget::CrossSection(_))
                                    ) {
                                        let scales = self
                                            .playback_session
                                            .contract()
                                            .expect("a temporal linked rebase retains its session")
                                            .layer_scales()
                                            .as_ref();
                                        assert!(
                                            [PanelId::Xy, PanelId::Xz, PanelId::Yz]
                                                .into_iter()
                                                .all(|panel| self
                                                    .installed_cross_section_full_volume_body_matches_scales(
                                                        panel, scales,
                                                    )),
                                            "a rebased temporal linked bundle must be geometry-independent"
                                        );
                                        self.installed_cross_section_exact_bodies =
                                            [PanelId::Xy, PanelId::Xz, PanelId::Yz].map(|panel| {
                                                self.installed_cross_section_exact_body_candidate(
                                                    panel,
                                                )
                                            });
                                        self.resident_cross_section_coverage =
                                            Some(ResidentCrossSectionCoverage {
                                                revision: intent_revision,
                                                cross_sections: planning_signature
                                                    .cross_sections
                                                    .clone(),
                                                exact_body_reuse: true,
                                                rolling_replan: false,
                                                planning_required: false,
                                            });
                                    }
                                }
                                self.visible_demand_failure_latch = None;
                                accepted = outcome;
                            }
                            Err(error) => {
                                if let Some(retry) = error
                                    .downcast_ref::<VisibleDemandPlaceabilityRetry>()
                                    .copied()
                                {
                                    self.activate_visible_demand_placeability_fallback(
                                        pending.planning.clone(),
                                        retry,
                                    );
                                } else if visible_demand_failure_is_recovery_backpressure(&error) {
                                    self.visible_demand_failure_latch = None;
                                    self.render_coordination.request_refresh();
                                } else {
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
                    Err(error) => {
                        let pending = pending.expect("a current result has pending identity");
                        let error =
                            self.physical_minimum_error_if_limited(&planning_signature, error);
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
        let projected_lod_is_unchanged = camera_only_change
            && current_3d_projected_layer_scales(
                snapshot.catalog(),
                &effective_view,
                planning_signature.current_3d.presentation_viewport,
                planning_signature.current_3d.render_viewport,
                planning_signature.current_3d.playback_active,
            )
            .is_ok_and(|ideal| self.dataset.current_ideal_layer_scales() == &ideal);
        let playback_full_volume_reuse = camera_only_change
            && planning_signature.current_3d.playback_active
            && self.dataset.current_covers_full_volume();
        let current_plan_is_reusable = playback_full_volume_reuse
            || (projected_lod_is_unchanged
                && (self.dataset.current_covers_full_volume() || camera_is_inside_reuse_envelope));
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
            self.dataset
                .record_contained_visible_demand_reuse(reusable_candidates);
            if let Some(installed) = self.visible_demand_planning_signature.as_mut() {
                installed.current_3d = planning_signature.current_3d.clone();
            }
        }
        let resident_cross_sections_match =
            self.resident_cross_section_coverage_matches_current(&snapshot, &planning_signature);
        if self.resident_cross_section_coverage.is_some() && !resident_cross_sections_match {
            self.resident_cross_section_coverage = None;
        }
        let installed_cross_section_exact_bodies =
            self.installed_cross_section_exact_bodies_are_current();
        let resident_planning_satisfied = resident_cross_sections_match
            && self
                .resident_cross_section_coverage
                .as_ref()
                .is_some_and(|coverage| !coverage.planning_required);
        let cross_plan_required = if resident_planning_satisfied {
            CrossSectionPlanMask::default()
        } else {
            required_cross_section_plan(
                self.visible_demand_planning_signature
                    .as_ref()
                    .map(|previous| &previous.cross_sections),
                &planning_signature.cross_sections,
                installed_cross_section_exact_bodies,
            )
        };
        let preserve_previous_renderer_requirement_union = [PanelId::Xy, PanelId::Xz, PanelId::Yz]
            .into_iter()
            .any(|panel| {
                cross_plan_required.contains(panel)
                    && !installed_cross_section_exact_bodies[cross_section_panel_index(panel)]
            });
        if let Some(RenderIntentTarget::CrossSection(active_panel)) = active_target {
            let panel = PanelId::from_application_panel(active_panel);
            if !cross_plan_required.contains(panel) {
                let scope = cross_section_scope_id(panel);
                debug_assert!(
                    self.dataset
                        .activate_installed_visible_intent(intent_revision, scope),
                    "the current mailbox panel must own its active dataset scope"
                );
            }
        }
        let union_plan_required = self.dataset.renderer_requirement_union_needs_preparation();
        if !current_plan_required && !cross_plan_required.any() && !union_plan_required {
            self.visible_demand_failure_latch = None;
            if self.pending_visible_demand_plan.is_some() {
                self.dataset.invalidate_visible_demand_plan(intent_revision);
                self.pending_visible_demand_plan = None;
            }
            let mut pumped = self.pump_unchanged_visible_demand();
            pumped.current_plan_installed |= accepted.current_plan_installed;
            pumped.cross_section_plan_installed |= accepted.cross_section_plan_installed;
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
                            intent_revision,
                            &planning_signature,
                            current_plan_required,
                            cross_plan_required,
                            union_plan_required,
                            &self.dataset,
                            cpu_capacity_epoch,
                            self.dataset_runtime_epoch,
                        )
                    })
                });
        if failure_is_unchanged {
            return VisibleBrickRequestOutcome::default();
        }
        self.visible_demand_failure_latch = None;

        let pending_matches = pending_temporal_spatial_rebase
            || self
                .pending_visible_demand_plan
                .as_ref()
                .is_some_and(|pending| {
                    pending.revision == intent_revision
                        && pending.current_3d.as_ref()
                            == current_plan_required.then_some(&planning_signature.current_3d)
                        && pending_cross_sections_match(
                            pending.cross_sections.as_ref(),
                            &planning_signature.cross_sections,
                            cross_plan_required,
                        )
                        && unchanged_scope_handles_are_current(&self.dataset, pending)
                        && renderer_requirement_base_is_current(
                            &self.dataset,
                            &pending.renderer_requirement_base,
                        )
                });
        if !pending_matches {
            let view = &effective_view;
            let four_panel = view.layout() == ViewerLayout::FourPanel;
            let playback_active = snapshot.transient().playback_active();
            let playback_window =
                playback_demand_window(&snapshot, self.playback_session.contract());
            let gpu_payload_capacity = self.selected_gpu_payload_capacity(&planning_signature);
            let global_demand_limits = DatasetDemandPlanLimits::new(
                SEMANTIC_PLAN_CANDIDATES_PER_LAYER,
                PRODUCT_RENDER_RESOURCE_LIMIT,
                gpu_payload_capacity,
            )
            .with_playback_decoded_capacity(snapshot.resource_policy().cpu_dataset_budget_bytes());
            let mut unchanged_scopes = Vec::new();
            if !current_plan_required {
                unchanged_scopes.extend([
                    SCOPE_CURRENT_3D,
                    SCOPE_CURRENT_3D_REFINEMENT,
                    SCOPE_PLAYBACK,
                ]);
            }
            for (scope, panel) in cross_section_scopes() {
                if !cross_plan_required.contains(panel) {
                    unchanged_scopes.push(scope);
                }
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

            let mut preserve_complete_presentation = retained_quality_handoff;
            let current_3d = if current_plan_required {
                #[cfg(test)]
                {
                    self.visible_demand_plan_calls =
                        self.visible_demand_plan_calls.saturating_add(1);
                }
                let installed_navigation_ladder = installed_navigation_ladder_baseline(
                    &self.dataset,
                    &self.navigation_render_plans,
                );
                // Static viewing and playback deliberately own different
                // navigation-ladder policies. In particular, the static
                // max-min ladder advances one visible layer per rung while
                // playback retains its pre-cutover lockstep ladder. Never
                // infer compatibility from an installed suffix: a
                // terminal-only ladder has no suffix from which to infer its
                // policy. A mode transition therefore rebuilds the requested
                // ladder, while an unchanged mode may still adopt the
                // validated installed baseline.
                let navigation_ladder_baseline = self
                    .visible_demand_planning_signature
                    .as_ref()
                    .filter(|installed| installed.current_3d.playback_active == playback_active)
                    .and(installed_navigation_ladder.clone());
                preserve_complete_presentation |= self
                    .render_coordination
                    .surface(PresentationSlot::ThreeD)
                    .presented_frame()
                    .is_some_and(|frame| {
                        frame.progress().completeness()
                            != mirante4d_render_api::FrameCompleteness::Progressive
                    })
                    && installed_navigation_ladder.is_some()
                    && self.navigation_render_plans.first().is_some_and(|plan| {
                        first_useful_resources_complete_with_renderer(
                            &self.dataset,
                            &self.native_presentation,
                            plan,
                        )
                    });
                Some(
                    Current3dDemandRequest::new(
                        self.render_coordination.presentation_viewport,
                        self.render_coordination.render_viewport,
                        global_demand_limits,
                        Current3dDemandBaselines::new(
                            scope_baseline(&self.dataset, SCOPE_CURRENT_3D),
                            scope_baseline(&self.dataset, SCOPE_CURRENT_3D_REFINEMENT),
                            scope_baseline(&self.dataset, SCOPE_PLAYBACK),
                        ),
                    )
                    .with_playback(
                        playback_active,
                        playback_window.timepoints,
                        playback_window.required_timepoint_count,
                        self.playback_session
                            .contract()
                            .map(|contract| Arc::clone(contract.layer_scales())),
                    )
                    .with_complete_presentation_preserved(preserve_complete_presentation)
                    .with_navigation_ladder_baseline(navigation_ladder_baseline),
                )
            } else {
                None
            };

            let mut cross_sections = Vec::new();
            if cross_plan_required.any() && four_panel {
                let mut ordered_panels = cross_section_scopes().collect::<Vec<_>>();
                if let Some(RenderIntentTarget::CrossSection(active)) = active_target {
                    let active = PanelId::from_application_panel(active);
                    ordered_panels.sort_by_key(|(_, panel)| usize::from(*panel != active));
                }
                for (scope, panel) in ordered_panels {
                    if !cross_plan_required.contains(panel) {
                        continue;
                    }
                    let panel_signature = planning_signature.cross_sections.panel(panel);
                    let (Some(presentation), Some(extent)) = (
                        panel_signature.presentation_viewport,
                        panel_signature.render_extent,
                    ) else {
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
                        global_demand_limits,
                        scope_baseline(&self.dataset, scope),
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
                    for (scope, panel) in cross_section_scopes() {
                        if !cross_plan_required.contains(panel) {
                            bodies.push(self.dataset.scope_prepared_body_handle(scope));
                        }
                    }
                    bodies
                });
            let cross_section_plan_count = u64::try_from(cross_sections.len()).unwrap_or(u64::MAX);
            let renderer_requirement_base = self.dataset.renderer_requirement_handle();
            let request = CameraDemandRequest::new(
                intent_revision,
                Arc::clone(snapshot.catalog()),
                self.dataset.cpu_ledger_arc(),
                view.clone(),
                global_demand_limits,
                current_3d,
                cross_sections,
                renderer_requirement_base.clone(),
                unchanged_bodies,
                promotion_bodies,
            )
            .with_previous_renderer_requirement_union_preserved(
                preserve_previous_renderer_requirement_union
                    || preserve_complete_presentation
                    || temporal_frame_contract.is_some()
                    || retained_quality_handoff,
            )
            .with_fixed_playback_layer_scales(
                self.playback_session
                    .contract()
                    .map(|contract| Arc::clone(contract.layer_scales())),
            )
            .with_temporal_frame_contract(temporal_frame_contract.clone());
            if !self.dataset.submit_visible_demand_plan(request) {
                self.dataset.record_plan_error(
                    "visible-demand planning received a stale render-intent revision",
                );
                self.render_coordination.frame_fidelity.completeness =
                    FrameCompleteness::Incomplete;
                return VisibleBrickRequestOutcome::default();
            }
            self.resident_plane_async_plan_submissions = self
                .resident_plane_async_plan_submissions
                .saturating_add(cross_section_plan_count);
            self.pending_visible_demand_plan = Some(PendingVisibleDemandPlan {
                revision: intent_revision,
                planning: planning_signature.clone(),
                current_3d: current_plan_required.then(|| planning_signature.current_3d.clone()),
                cross_sections: cross_plan_required
                    .any()
                    .then(|| PendingCrossSectionDemandPlan {
                        target: planning_signature.cross_sections.clone(),
                        panels: cross_plan_required,
                        exact: !matches!(active_target, Some(RenderIntentTarget::CrossSection(_))),
                    }),
                union_plan_required,
                preserve_complete_presentation,
                unchanged_scope_handles,
                renderer_requirement_base,
                cpu_capacity_epoch,
                dataset_runtime_epoch: self.dataset_runtime_epoch,
                temporal_frame_contract,
            });
        }

        let mut pumped = self.pump_unchanged_visible_demand();
        pumped.current_plan_installed |= accepted.current_plan_installed;
        pumped.cross_section_plan_installed |= accepted.cross_section_plan_installed;
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
            targets,
            renderer_requirement_update,
            renderer_requirement_payload_bytes,
            post_refinement_promotion_update,
            candidates_visited,
        } = prepared;
        let (current_3d, cross_sections, temporal_frame_contract) = targets.into_parts();
        let previous_plan_error = self.dataset.last_plan_error().map(str::to_owned);
        if temporal_frame_contract != pending.temporal_frame_contract {
            anyhow::bail!(
                "prepared temporal-frame identity does not match its pending transaction"
            );
        }
        if let Some(frame) = temporal_frame_contract.as_ref() {
            let snapshot = self.application.snapshot();
            let session = self.playback_session.contract().ok_or_else(|| {
                anyhow::anyhow!("prepared temporal frame outlived its playback session")
            })?;
            if !session.admits_frame(frame)
                || frame.source_generation() != snapshot.source_generation()
                || frame.timepoint() != application_view(&snapshot).timepoint()
                || frame.target_set()
                    != crate::playback_session::playback_targets_for_layout(
                        application_view(&snapshot).layout(),
                    )
            {
                anyhow::bail!("prepared temporal frame is stale for the live playback contract");
            }
        }
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
                let PreparedCurrent3dDemand {
                    plan,
                    current,
                    refinement,
                    playback,
                    playback_resources_per_timepoint,
                    navigation_ladder,
                } = current;
                let ideal_scale = uniform_layer_scale(&plan.ideal_layer_scales);
                let playback_timepoint_count = plan.playback_timepoint_count;
                let target_scale = uniform_layer_scale(&plan.target.layer_scales);
                let target_is_empty = plan.target.layer_scales.is_empty();
                let adaptive_capacity_limited = plan.target.layer_scales != plan.ideal_layer_scales;
                let target_playback_downshifted = plan.target.playback_downshifted;
                let target_visible_count = plan.target.primary_resource_count;
                let reuse_envelope = plan.reuse_envelope.clone();
                let playback_layer_scales = plan.target.layer_scales.clone();
                let current_plan = if current.render_requirements.is_some() {
                    if refinement.render_requirements.is_some() {
                        plan.coarse.as_ref()
                    } else {
                        Some(&plan.target)
                    }
                } else {
                    None
                };
                let current_accounting =
                    current_plan.map(|plan| (plan.payload_bytes, plan.primary_resource_count));
                let current_covers_full_selected_volumes =
                    current_plan.is_some_and(|plan| plan.covers_full_volume);
                let current_layer_scales =
                    current_plan.map(|plan| Arc::new(plan.layer_scales.clone()));
                let refinement_accounting = refinement.render_requirements.is_some().then_some((
                    plan.target.payload_bytes,
                    plan.target.primary_resource_count,
                ));
                let current_scope_requirements = Arc::clone(current.requirements.canonical());
                let refinement_scope_requirements = Arc::clone(refinement.requirements.canonical());
                let navigation_scope_requirements = if pending.preserve_complete_presentation {
                    Arc::clone(&refinement_scope_requirements)
                } else {
                    Arc::clone(&current_scope_requirements)
                };
                if target_is_empty
                    && (!plan.ideal_layer_scales.is_empty()
                        || !plan.target.requirements.canonical().is_empty()
                        || !current.requirements.canonical().is_empty()
                        || current.render_requirements.is_some()
                        || !refinement.requirements.canonical().is_empty()
                        || refinement.render_requirements.is_some()
                        || !playback.requirements.canonical().is_empty()
                        || playback.render_requirements.is_some())
                {
                    anyhow::bail!("an empty visible-layer target owns prepared 3D artifacts");
                }
                let current_render =
                    current
                        .render_requirements
                        .map(|requirements| PreparedScopeRenderPlan {
                            selection_required_prefix_len: requirements.first_useful_prefix_len(),
                            selection_body: requirements.body().clone(),
                            requirements,
                            scope_requirements: Arc::clone(&current_scope_requirements),
                            layer_scales: current_layer_scales
                                .expect("a current render owns selected layer scales"),
                            render_payload_bytes: current
                                .render_payload_bytes
                                .expect("a current render owns exact payload accounting"),
                            planned_payload_bytes: current_accounting
                                .expect("a current render owns plan accounting")
                                .0,
                            primary_resource_count: current_accounting
                                .expect("a current render owns plan accounting")
                                .1,
                            covers_full_selected_volumes: current_covers_full_selected_volumes,
                            plane_reuse_envelope: None,
                        });
                let refinement_render =
                    refinement
                        .render_requirements
                        .map(|requirements| PreparedScopeRenderPlan {
                            selection_required_prefix_len: requirements.first_useful_prefix_len(),
                            selection_body: requirements.body().clone(),
                            requirements,
                            scope_requirements: Arc::clone(&refinement_scope_requirements),
                            layer_scales: Arc::new(plan.target.layer_scales.clone()),
                            render_payload_bytes: refinement
                                .render_payload_bytes
                                .expect("a refinement render owns exact payload accounting"),
                            planned_payload_bytes: refinement_accounting
                                .expect("a refinement render owns target accounting")
                                .0,
                            primary_resource_count: refinement_accounting
                                .expect("a refinement render owns target accounting")
                                .1,
                            covers_full_selected_volumes: plan.target.covers_full_volume,
                            plane_reuse_envelope: None,
                        });
                if playback.render_requirements.is_some() {
                    anyhow::bail!("prepared playback scope unexpectedly owns render artifacts");
                }
                let navigation_render_plans = navigation_ladder
                    .candidates
                    .into_iter()
                    .map(|candidate| PreparedScopeRenderPlan {
                        selection_required_prefix_len: candidate.selection_body.ranked().len(),
                        selection_body: candidate.selection_body,
                        requirements: candidate.render_requirements,
                        scope_requirements: Arc::clone(&navigation_scope_requirements),
                        layer_scales: Arc::new(candidate.layer_scales),
                        render_payload_bytes: candidate.planned_payload_bytes,
                        planned_payload_bytes: candidate.planned_payload_bytes,
                        primary_resource_count: candidate.resource_count,
                        covers_full_selected_volumes: true,
                        plane_reuse_envelope: None,
                    })
                    .collect::<Vec<_>>();
                if !target_is_empty && navigation_render_plans.is_empty() {
                    anyhow::bail!("prepared 3D navigation ladder is empty");
                }
                if target_is_empty && !navigation_render_plans.is_empty() {
                    anyhow::bail!("an empty visible-layer target owns a navigation ladder");
                }
                Ok::<_, anyhow::Error>((
                    plan,
                    playback.requirements,
                    playback_resources_per_timepoint,
                    playback_timepoint_count,
                    playback_layer_scales,
                    current_render,
                    refinement_render,
                    navigation_render_plans,
                    ideal_scale,
                    target_scale,
                    adaptive_capacity_limited,
                    target_playback_downshifted,
                    target_visible_count,
                    reuse_envelope,
                ))
            })
            .transpose()?;

        let mut installed_panels = std::collections::BTreeSet::new();
        let mut cross_installs = Vec::with_capacity(cross_sections.len());
        if let Some(pending_cross) = pending.cross_sections.as_ref() {
            for cross in cross_sections {
                if !pending_cross.panels.contains(cross.panel) {
                    anyhow::bail!(
                        "prepared cross-section transaction contains an unrequested panel"
                    );
                }
                if !installed_panels.insert(cross.panel) {
                    anyhow::bail!("prepared cross-section transaction contains a duplicate panel");
                }
                let render_plan = match cross.render_requirements {
                    Some(requirements) => Some(PreparedScopeRenderPlan {
                        selection_required_prefix_len: requirements.first_useful_prefix_len(),
                        selection_body: requirements.body().clone(),
                        requirements,
                        scope_requirements: Arc::clone(cross.plan.requirements.canonical()),
                        layer_scales: Arc::new(cross.plan.layer_scales.clone()),
                        render_payload_bytes: cross
                            .render_payload_bytes
                            .expect("a cross-section render owns exact payload accounting"),
                        planned_payload_bytes: cross.plan.payload_bytes,
                        primary_resource_count: cross.plan.primary_resource_count,
                        covers_full_selected_volumes: cross.plan.covers_full_volume,
                        plane_reuse_envelope: cross.reuse_envelope,
                    }),
                    None if cross.plan.requirements.canonical().is_empty() => None,
                    None => {
                        anyhow::bail!(
                            "a non-empty prepared cross section omitted render requirements"
                        )
                    }
                };
                cross_installs.push((
                    cross_section_scope_id(cross.panel),
                    cross.panel,
                    cross.plan.requirements,
                    cross.plan.layer_scales,
                    render_plan,
                ));
            }
        }

        // A worker result can replace only part of the installed immutable
        // body cohort. Before mutating any scope, prove that each temporal
        // target will have either its new body or the exact compatible body
        // already installed. Renderer-front reuse remains a later renderer-
        // owned decision over the complete requests built from that cohort.
        temporal_frame_contract
            .as_ref()
            .map(|frame| {
                let newly_prepared = |target| match target {
                    PresentationTarget::ThreeD => current_install.is_some(),
                    PresentationTarget::Xy | PresentationTarget::Xz | PresentationTarget::Yz => {
                        installed_panels
                            .iter()
                            .any(|panel| panel.presentation_slot() == target)
                    }
                };
                let installed_matches = |target| {
                    let Some(installed) = self.visible_demand_planning_signature.as_ref() else {
                        return false;
                    };
                    match target {
                        PresentationTarget::ThreeD => {
                            installed.current_3d == pending.planning.current_3d
                                && self.installed_temporal_3d_body_matches(frame)
                        }
                        PresentationTarget::Xy
                        | PresentationTarget::Xz
                        | PresentationTarget::Yz => {
                            let panel = match target {
                                PresentationTarget::Xy => PanelId::Xy,
                                PresentationTarget::Xz => PanelId::Xz,
                                PresentationTarget::Yz => PanelId::Yz,
                                PresentationTarget::ThreeD => unreachable!(),
                            };
                            installed
                                .cross_sections
                                .same_common_demand(&pending.planning.cross_sections)
                                && installed.cross_sections.panel(panel)
                                    == pending.planning.cross_sections.panel(panel)
                                && self.installed_temporal_cross_section_body_matches(panel, frame)
                        }
                    }
                };
                for target in PresentationTarget::ALL {
                    let required = frame.target_set().contains(target);
                    if required && !newly_prepared(target) && !installed_matches(target) {
                        return Err(anyhow::Error::new(MissingLogicalTarget(target)));
                    }
                }
                Ok(())
            })
            .transpose()?;

        let next_signature = match self.visible_demand_planning_signature.as_ref() {
            Some(installed) => {
                let mut next = installed.clone();
                if let Some(current) = pending.current_3d.as_ref() {
                    next.current_3d = current.clone();
                }
                if let Some(cross) = pending.cross_sections.as_ref() {
                    if !next.cross_sections.same_common_demand(&cross.target) {
                        debug_assert_eq!(cross.panels, CrossSectionPlanMask::ALL);
                        next.cross_sections = cross.target.clone();
                    } else {
                        for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
                            if cross.panels.contains(panel) {
                                *next.cross_sections.panel_mut(panel) =
                                    cross.target.panel(panel).clone();
                            }
                        }
                    }
                }
                next
            }
            None => pending.planning.clone(),
        };

        let mut scope_targets = ScopeReconciliationTargets::default();
        if let Some((plan, playback, _, _, _, _, _, _, _, _, _, _, _, _)) = current_install.as_ref()
        {
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
        if let Some(pending_cross) = pending.cross_sections.as_ref() {
            for (scope, panel) in cross_section_scopes() {
                if pending_cross.panels.contains(panel) && !installed_panels.contains(&panel) {
                    scope_targets.remove(scope);
                }
            }
        }
        self.dataset
            .preflight_prepared_renderer_requirement_update(
                &renderer_requirement_update.previous.requirements,
                &renderer_requirement_update.next.requirements,
            )?;
        if let Some(product) = self.native_presentation.product_gpu.as_mut() {
            let snapshot = self.application.snapshot();
            match product.renderer.ensure_global_requirement_union(
                snapshot.catalog(),
                &renderer_requirement_update.next.requirements,
            ) {
                Ok(()) => {}
                Err(error) => {
                    if let Some(placement) = PayloadPlacementFacts::from_runtime_error(error) {
                        return Err(anyhow::Error::new(VisibleDemandPlaceabilityRetry {
                            target_group: pending.target_group(),
                            failed_union_payload_bytes: renderer_requirement_payload_bytes,
                            placement,
                        }));
                    }
                    return Err(error.into());
                }
            }
        }
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
        let mut prepared_playback_session = if let Some((
            _,
            _,
            _,
            playback_timepoint_count,
            playback_layer_scales,
            _,
            _,
            _,
            _,
            _,
            _,
            _,
            _,
            _,
        )) = current_install.as_ref()
            && self.application.snapshot().transient().playback_active()
            && self.playback_session.contract().is_none()
        {
            let snapshot = self.application.snapshot();
            let window = playback_demand_window(&snapshot, None);
            Some(
                self.playback_session
                    .prepare_contract(
                        snapshot.source_generation(),
                        snapshot.transient().playback_fps(),
                        application_view(&snapshot).layout(),
                        playback_layer_scales.clone(),
                        *playback_timepoint_count,
                        window.required_timepoint_count,
                        snapshot.resource_policy().cpu_dataset_budget_bytes(),
                        self.physical_gpu_payload_capacity(),
                        application_view(&snapshot).timepoint(),
                        &window.timepoints,
                    )
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "prepared playback plan cannot form its immutable session contract"
                        )
                    })?,
            )
        } else {
            None
        };

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
            playback_resources_per_timepoint,
            _playback_timepoint_count,
            playback_layer_scales,
            current_render,
            refinement_render,
            navigation_render_plans,
            ideal_scale,
            target_scale,
            adaptive_capacity_limited,
            target_playback_downshifted,
            target_visible_count,
            reuse_envelope,
        )) = current_install
        {
            let target_is_empty = plan.target.layer_scales.is_empty();
            // Empty visibility is an immediate display state, never a quality
            // handoff. A predecessor retained for progressive replacement
            // must not keep either its scope render plan or its fidelity label
            // alive after the last visible layer is hidden.
            let preserve_complete_presentation =
                pending.preserve_complete_presentation && !target_is_empty;
            current_changed = self.dataset.commit_preflighted_progressive_current_plan(
                plan,
                four_panel,
                preserve_complete_presentation,
            );
            self.dataset.commit_preflighted_playback_scope_replacement(
                playback,
                playback_layer_scales.clone(),
                playback_resources_per_timepoint,
            );
            if let Some(prepared_session) = prepared_playback_session.take() {
                assert!(
                    self.playback_session.commit_prepared(prepared_session),
                    "a preflighted playback session must commit in the same UI transaction"
                );
            }
            if !preserve_complete_presentation {
                install_scope_render_plan(
                    &mut self.prepared_scope_render_plans,
                    SCOPE_CURRENT_3D,
                    current_render,
                );
            }
            install_scope_render_plan(
                &mut self.prepared_scope_render_plans,
                SCOPE_CURRENT_3D_REFINEMENT,
                refinement_render,
            );
            // A retained predecessor remains protected by its installed scope
            // and renderer front. Navigation metadata is semantic data for
            // future renders, so it must advance to the successor immediately;
            // retaining the predecessor's timepoint-bound ladder would make
            // camera samples repeatedly replan or render stale resources.
            self.navigation_render_plans = navigation_render_plans;
            self.render_coordination.frame_fidelity.ideal_scale_level =
                ideal_scale.map(ScaleLevel::get);
            self.render_coordination.frame_fidelity.target_scale_level =
                target_scale.map(ScaleLevel::get);
            self.render_coordination
                .frame_fidelity
                .adaptive_capacity_limited = adaptive_capacity_limited;
            self.render_coordination.frame_fidelity.refinement_pending =
                self.dataset.staging_current_refinement();
            self.render_coordination.frame_fidelity.visible_bricks = target_visible_count;
            self.current_camera_reuse_envelope = reuse_envelope;
            self.render_coordination.frame_fidelity.reason = if target_is_empty {
                LodDecisionReason::NoVisibleData
            } else if adaptive_capacity_limited {
                LodDecisionReason::AdaptiveCapacity
            } else if target_playback_downshifted {
                LodDecisionReason::PlaybackDownshift
            } else if target_scale == Some(mirante4d_domain::ScaleLevel::BASE) {
                LodDecisionReason::ExactS0
            } else {
                LodDecisionReason::ScreenEquivalentCoarserScale
            };
        }

        if let Some(pending_cross) = pending.cross_sections.as_ref() {
            for (scope, panel, requirements, layer_scales, render_plan) in cross_installs {
                let guarded = render_plan
                    .as_ref()
                    .is_some_and(|plan| plan.plane_reuse_envelope.is_some());
                if guarded {
                    self.resident_plane_guard_body_installs =
                        self.resident_plane_guard_body_installs.saturating_add(1);
                } else {
                    self.resident_plane_exact_body_installs =
                        self.resident_plane_exact_body_installs.saturating_add(1);
                }
                self.dataset.commit_preflighted_scope_replacement(
                    scope,
                    requirements,
                    layer_scales,
                );
                install_scope_render_plan(
                    &mut self.prepared_scope_render_plans,
                    scope,
                    render_plan,
                );
                let exact_body = pending_cross.exact.then(|| {
                    self.installed_cross_section_exact_body_candidate(panel)
                        .expect("an exact linked install binds its committed immutable body")
                });
                self.installed_cross_section_exact_bodies[cross_section_panel_index(panel)] =
                    exact_body;
            }
            for (scope, panel) in cross_section_scopes() {
                if pending_cross.panels.contains(panel) && !installed_panels.contains(&panel) {
                    self.dataset.commit_preflighted_scope_removal(scope);
                    self.prepared_scope_render_plans.remove(&scope);
                    self.installed_cross_section_exact_bodies[cross_section_panel_index(panel)] =
                        None;
                }
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
        if let Some(product) = self.native_presentation.product_gpu.as_mut() {
            product.renderer.retire_residency_offers(&removals);
        }
        self.staged_post_promotion_renderer_update = if self.dataset.staging_current_refinement() {
            post_refinement_promotion_update
        } else {
            None
        };
        if let Some(cross) = pending.cross_sections.as_ref() {
            self.resident_cross_section_coverage =
                (!cross.exact).then(|| ResidentCrossSectionCoverage {
                    revision: pending.revision,
                    cross_sections: cross.target.clone(),
                    exact_body_reuse: false,
                    rolling_replan: false,
                    planning_required: false,
                });
        }
        self.visible_demand_planning_signature = Some(next_signature);
        self.visible_demand_placeability_limit = None;
        self.dataset.clear_plan_error();
        if previous_plan_error.as_deref().is_some_and(|previous| {
            self.render_coordination
                .frame_fidelity
                .last_capacity_error
                .as_deref()
                == Some(previous)
        }) {
            self.render_coordination.frame_fidelity.last_failure_kind = None;
            self.render_coordination.frame_fidelity.last_capacity_error = None;
        }
        if current_changed {
            self.progressive_display_pacer.reset();
        }
        if pending.cross_sections.is_some()
            && [PanelId::Xy, PanelId::Xz, PanelId::Yz]
                .into_iter()
                .any(|panel| {
                    self.render_coordination
                        .surface(panel.presentation_slot())
                        .display_current()
                })
        {
            // A cold transient keeps the prior pixels visible while its
            // latest-only plan is prepared. The atomic plan cutover is the
            // first point at which those pixels become semantically stale.
            // Durable view changes already invalidate the linked generation,
            // so avoid manufacturing a second generation in that case.
            self.render_coordination.invalidate_cross_sections();
        }
        Ok(VisibleBrickRequestOutcome {
            current_plan_installed,
            cross_section_plan_installed: pending.cross_sections.is_some(),
            current_changed,
            resident_changed: false,
            current_frame_ready: false,
        })
    }

    pub(crate) fn drain_brick_results(&mut self, ctx: &egui::Context) {
        let started = Instant::now();
        let mut current_plan_installed = false;
        let mut cross_section_plan_installed = false;
        let snapshot = self.application.snapshot();
        if !self.dataset.source_quarantined()
            && self.dataset.resource_identity() == snapshot.catalog().resource_identity()
        {
            // Result ingestion is also a bounded wake-up point for the
            // latest-only planner. This keeps slow I/O/background frames
            // progressing even when no display refresh was otherwise due.
            let visible = self.request_visible_bricks();
            current_plan_installed = visible.current_plan_installed;
            cross_section_plan_installed = visible.cross_section_plan_installed;
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
        let (dataset, analysis, native_presentation, render_attempt) = (
            &mut self.dataset,
            &mut self.analysis_runtime,
            &mut self.native_presentation,
            &mut self.render_attempt,
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
            if !batch_installed.leases().is_empty()
                && !render_attempt.renderer_is_terminal()
                && let Some(product) = native_presentation.product_gpu.as_mut()
            {
                if let Err(error) = product
                    .renderer
                    .offer_residency_leases(batch_installed.leases())
                {
                    tracing::error!(%error, "renderer rejected a CPU completion lease batch");
                    drain_fault = Some((
                        RuntimeFault::new(RuntimeFaultCode::InvariantViolation),
                        dataset.deferred_refill_preserves_presentation(),
                    ));
                    break;
                }
                for lease in batch_installed.leases() {
                    render_attempt.observe_relevant_residency_offer(lease.key());
                }
            }
            installed_current |= batch_installed.in_scope(SCOPE_CURRENT_3D);
            installed_any |= batch_installed.any();
            if dataset.dispatcher().admission_blocked() {
                let preserve_presentation = dataset.deferred_refill_preserves_presentation();
                match pump_interactive_admission_with_renderer(
                    dataset,
                    native_presentation,
                    render_attempt,
                ) {
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
                &mut self.native_presentation,
                &mut self.render_attempt,
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
            || cross_section_plan_installed
            || completion_drain_needs_display_refresh(
                promoted_current || complete_current_cohort_installed,
            );
        let complete_presentation_held = self
            .render_coordination
            .surface(PresentationSlot::ThreeD)
            .presented_frame()
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
        self.render_coordination.frame_fidelity.ideal_scale_level = self
            .dataset
            .current_ideal_uniform_scale()
            .map(ScaleLevel::get);
        self.render_coordination
            .frame_fidelity
            .adaptive_capacity_limited = self.dataset.current_capacity_constrained();
        self.render_coordination.frame_fidelity.refinement_pending =
            self.dataset.staging_current_refinement();
        let presented_completeness = self
            .render_coordination
            .surface(PresentationSlot::ThreeD)
            .presented_frame()
            .map(|frame| frame.progress().completeness());
        let ready_backend = ready.then(|| {
            let snapshot = self.application.snapshot();
            let view = application_view(&snapshot);
            render_backend_for_view(view)
        });
        self.render_coordination.frame_fidelity.completeness = dataset_fidelity_completeness(
            empty,
            self.render_coordination
                .frame_fidelity
                .displayed_scale_level,
            self.render_coordination.frame_fidelity.display_freshness,
            presented_completeness,
        );
        self.render_coordination.frame_fidelity.backend = if empty {
            RenderBackend::Empty
        } else if let Some(backend) = ready_backend {
            backend
        } else {
            RenderBackend::Loading
        };
        if empty {
            self.render_coordination.frame_fidelity.reason = LodDecisionReason::NoVisibleData;
        } else if ready && self.dataset.current_capacity_constrained() {
            self.render_coordination.frame_fidelity.reason = LodDecisionReason::AdaptiveCapacity;
        }
    }

    pub(crate) fn record_dataset_fault(&mut self, fault: &RuntimeFault) {
        self.record_dataset_fault_with_presentation_effect(fault, false);
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
        let message = fault.to_string();
        self.dataset.record_plan_error(message.clone());
        self.render_coordination.frame_fidelity.last_capacity_error = Some(message);
        if invalidate_presentation {
            self.render_coordination.frame_fidelity.completeness = FrameCompleteness::Incomplete;
        }
    }

    fn record_dataset_plan_error(&mut self, error: &anyhow::Error) {
        tracing::warn!(%error, "visible dataset demand planning failed");
        let message = error.to_string();
        let aggregate_capacity = error
            .downcast_ref::<DatasetDemandPlanCapacityError>()
            .is_some();
        let physical_capacity = error
            .downcast_ref::<VisibleDemandMinimumPlacementError>()
            .is_some();
        let capacity = aggregate_capacity || physical_capacity;
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
            if physical_capacity {
                LodDecisionReason::GpuBudgetLimited
            } else {
                LodDecisionReason::CpuBudgetLimited
            }
        } else {
            LodDecisionReason::BackendLimit
        };
    }
}

const fn admission_fault_invalidates_presentation(preserve_presentation: bool) -> bool {
    !preserve_presentation
}

fn scope_baseline(dataset: &DatasetDemandState, scope: u64) -> ScopeDemandBaseline {
    let body = dataset.scope_prepared_body_handle(scope);
    ScopeDemandBaseline::new(body, dataset.scope_admitted_prefix_len(scope))
}

fn installed_navigation_ladder_baseline(
    dataset: &DatasetDemandState,
    navigation_ladder: &[PreparedScopeRenderPlan],
) -> Option<NavigationLadderBaseline> {
    if navigation_ladder.is_empty() {
        return None;
    }
    if !dataset.scope_is_installed(SCOPE_CURRENT_3D) {
        return None;
    }
    let installed = dataset.scope_prepared_body_handle(SCOPE_CURRENT_3D);
    if navigation_ladder
        .iter()
        .any(|candidate| !Arc::ptr_eq(&candidate.scope_requirements, installed.canonical()))
    {
        return None;
    }
    Some(NavigationLadderBaseline::new(
        navigation_ladder
            .iter()
            .map(|candidate| {
                NavigationCandidateBaseline::new(
                    candidate.selection_body.clone(),
                    Arc::clone(&candidate.layer_scales),
                    candidate.planned_payload_bytes,
                )
            })
            .collect(),
    ))
}

fn unchanged_scope_handles_are_current(
    dataset: &DatasetDemandState,
    pending: &PendingVisibleDemandPlan,
) -> bool {
    scope_handles_are_current(dataset, &pending.unchanged_scope_handles)
}

fn renderer_requirement_base_is_current(
    dataset: &DatasetDemandState,
    expected: &RetainedRequirementHandle,
) -> bool {
    let current = dataset.renderer_requirement_handle();
    Arc::ptr_eq(&current.requirements, &expected.requirements)
}

fn visible_demand_failure_signature(
    planning: VisibleDemandPlanningSignature,
    pending: PendingVisibleDemandPlan,
) -> VisibleDemandFailureSignature {
    VisibleDemandFailureSignature::new(
        pending.revision,
        planning,
        pending.current_3d.is_some(),
        pending
            .cross_sections
            .as_ref()
            .map_or_else(CrossSectionPlanMask::default, |cross| cross.panels),
        pending.union_plan_required,
        pending.unchanged_scope_handles,
        pending.renderer_requirement_base,
        pending.cpu_capacity_epoch,
        pending.dataset_runtime_epoch,
    )
}

fn visible_demand_failure_is_deterministic(error: &anyhow::Error) -> bool {
    if error
        .downcast_ref::<VisibleDemandPlaceabilityRetry>()
        .is_some()
        || error
            .downcast_ref::<mirante4d_render_wgpu::WgpuRenderRuntimeError>()
            .is_some_and(|error| {
                matches!(
                    error,
                    mirante4d_render_wgpu::WgpuRenderRuntimeError::PayloadPlacementUnavailable {
                        ..
                    } | mirante4d_render_wgpu::WgpuRenderRuntimeError::PayloadRecoveryDeferred
                )
            })
    {
        return false;
    }
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

fn visible_demand_failure_is_recovery_backpressure(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<mirante4d_render_wgpu::WgpuRenderRuntimeError>()
        .is_some_and(|error| {
            matches!(
                error,
                mirante4d_render_wgpu::WgpuRenderRuntimeError::PayloadRecoveryDeferred
            )
        })
}

fn scope_handles_are_current(
    dataset: &DatasetDemandState,
    handles: &[(u64, Arc<[mirante4d_dataset::BrickKey]>)],
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

fn cross_section_panel_index(panel: PanelId) -> usize {
    match panel {
        PanelId::Xy => 0,
        PanelId::Xz => 1,
        PanelId::Yz => 2,
        PanelId::ThreeD => unreachable!("the 3D panel has no cross-section demand index"),
    }
}

fn pending_cross_sections_match(
    pending: Option<&PendingCrossSectionDemandPlan>,
    target: &CrossSectionDemandPlanningSignature,
    panels: CrossSectionPlanMask,
) -> bool {
    match pending {
        Some(pending) => pending.panels == panels && pending.target == *target,
        None => !panels.any(),
    }
}

fn render_intent_scope(target: RenderIntentTarget) -> u64 {
    match target {
        RenderIntentTarget::ThreeD => SCOPE_CURRENT_3D,
        RenderIntentTarget::CrossSection(panel) => {
            cross_section_scope_id(PanelId::from_application_panel(panel))
        }
    }
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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct PlaybackDemandWindow {
    timepoints: Vec<TimeIndex>,
    required_timepoint_count: usize,
}

fn playback_demand_window(
    snapshot: &mirante4d_application::ApplicationSnapshot,
    contract: Option<&crate::playback_session::PlaybackSessionContract>,
) -> PlaybackDemandWindow {
    let mut window = playback_demand_window_for(
        application_view(snapshot).timepoint(),
        snapshot
            .catalog()
            .layers()
            .map(|layer| layer.shape().t())
            .min()
            .unwrap_or(1),
        snapshot.transient().playback_fps(),
        snapshot.transient().playback_active(),
    );
    if let Some(contract) = contract {
        window.timepoints.truncate(contract.slot_count());
        window.required_timepoint_count = contract.startup_runway().min(window.timepoints.len());
    }
    window
}

fn playback_demand_window_for(
    current: TimeIndex,
    timepoint_count: u64,
    fps: mirante4d_application::PlaybackFps,
    active: bool,
) -> PlaybackDemandWindow {
    if !active {
        return PlaybackDemandWindow::default();
    }
    if timepoint_count <= 1 {
        return PlaybackDemandWindow::default();
    }
    let desired_count =
        usize::from(fps.get()).min(usize::try_from(timepoint_count - 1).unwrap_or(usize::MAX));
    let required_timepoint_count = usize::from(fps.get().saturating_add(3) / 4)
        .max(1)
        .min(desired_count);
    let current = current.get();
    let timepoints = (1..=desired_count)
        .map(|offset| {
            let offset = u64::try_from(offset).expect("playback FPS fits u64");
            TimeIndex::new((current + offset) % timepoint_count)
        })
        .collect();
    PlaybackDemandWindow {
        timepoints,
        required_timepoint_count,
    }
}

const fn completion_drain_needs_display_refresh(presentation_changed: bool) -> bool {
    presentation_changed
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::Arc,
        time::{Duration, Instant},
    };

    use mirante4d_application::{RenderIntentRevision, RenderIntentTarget};
    use mirante4d_dataset::{
        CpuLedgerCategory, CpuLedgerError, DatasetResourceIdentity, DatasetSourceId,
    };
    use mirante4d_dataset_runtime::{RuntimeFault, RuntimeFaultCode};
    use mirante4d_domain::{
        CameraView, CrossSectionView, LogicalLayerKey, Projection, ScaleLevel, TimeIndex,
        UnitQuaternion, ViewerLayout, WorldPoint3,
    };
    use mirante4d_render_api::{
        FrameCoverage, FrameIdentity, FrameProgress, PresentationTarget, PresentationViewport,
        PresentedFrame, RenderExtent,
    };

    use crate::retained_leases::RetainedRequirementHandle;

    use super::{
        CrossSectionDemandPlanningSignature, CrossSectionPanelDemandPlanningSignature,
        CrossSectionPlanMask, Current3dDemandPlanningSignature,
        PROGRESSIVE_DISPLAY_REFRESH_INTERVAL, PayloadPlacementFacts, PlaybackDemandWindow,
        ProgressiveDisplayRefreshPacer, RESULT_DRAIN_BATCH, RESULT_DRAIN_COUNT_ENVELOPE,
        RESULT_DRAIN_TIME_BUDGET, VisibleDemandFailureSignature, VisibleDemandPlaceabilityLimit,
        VisibleDemandPlaceabilityRetry, VisibleDemandPlanningSignature,
        admission_fault_invalidates_presentation, completion_drain_needs_display_refresh,
        dataset_fidelity_completeness, installed_target_layer_scales,
        installed_visible_demand_plan_currentness, next_completion_drain_batch,
        playback_demand_window_for, temporal_3d_body_matches_camera_only_change,
        visible_demand_failure_is_deterministic, visible_demand_failure_is_recovery_backpressure,
    };
    use crate::{DeterministicFailureLatch, DisplayedFrameFreshness, FrameCompleteness};

    #[test]
    fn playback_window_is_fps_bounded_wraps_and_does_not_scale_with_total_time() {
        let fps_24 = mirante4d_application::PlaybackFps::new(24).unwrap();
        let long = playback_demand_window_for(TimeIndex::new(10), 365, fps_24, true);
        let arbitrarily_long =
            playback_demand_window_for(TimeIndex::new(10), 1_000_000, fps_24, true);
        assert_eq!(long.timepoints.len(), 24);
        assert_eq!(arbitrarily_long.timepoints, long.timepoints);
        assert_eq!(long.required_timepoint_count, 6);

        let short = playback_demand_window_for(TimeIndex::new(2), 3, fps_24, true);
        assert_eq!(short.timepoints, vec![TimeIndex::new(0), TimeIndex::new(1)]);
        assert_eq!(short.required_timepoint_count, 2);

        let fps_12 = mirante4d_application::PlaybackFps::new(12).unwrap();
        let lower_rate = playback_demand_window_for(TimeIndex::new(0), 365, fps_12, true);
        assert_eq!(lower_rate.timepoints.len(), 12);
        assert_eq!(lower_rate.required_timepoint_count, 3);
        assert_eq!(
            playback_demand_window_for(TimeIndex::new(0), 365, fps_12, false),
            PlaybackDemandWindow::default()
        );
    }

    #[test]
    fn playback_full_volume_planning_survives_only_active_camera_changes() {
        let mut planned = planning_signature(1).current_3d;
        planned.playback_active = true;
        let mut camera_changed = planned.clone();
        camera_changed.camera = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            2.0,
            1.0,
            1.0,
        )
        .unwrap();

        assert!(temporal_3d_body_matches_camera_only_change(
            &planned,
            &camera_changed,
            Some(RenderIntentTarget::ThreeD),
        ));
        assert!(!temporal_3d_body_matches_camera_only_change(
            &planned,
            &camera_changed,
            None,
        ));

        let mut time_changed = camera_changed.clone();
        time_changed.timepoint = TimeIndex::new(1);
        assert!(!temporal_3d_body_matches_camera_only_change(
            &planned,
            &time_changed,
            Some(RenderIntentTarget::ThreeD),
        ));
        planned.playback_active = false;
        assert!(!temporal_3d_body_matches_camera_only_change(
            &planned,
            &camera_changed,
            Some(RenderIntentTarget::ThreeD),
        ));
    }

    fn planning_signature(source_id: u64) -> VisibleDemandPlanningSignature {
        let resource_identity =
            DatasetResourceIdentity::SessionLocal(DatasetSourceId::new(source_id));
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
                playback_priority_layer: None,
                timepoint: TimeIndex::new(0),
                camera,
                layout: ViewerLayout::Single3d,
                layers: Box::new([]),
                presentation_viewport,
                render_viewport: render_extent,
                playback_active: false,
                playback_fps: mirante4d_application::PlaybackFps::DEFAULT,
                gpu_payload_capacity: 1,
            },
            cross_sections: CrossSectionDemandPlanningSignature {
                resource_identity,
                playback_priority_layer: None,
                timepoint: TimeIndex::new(0),
                layout: ViewerLayout::Single3d,
                layers: Box::new([]),
                panels: std::array::from_fn(|_| CrossSectionPanelDemandPlanningSignature {
                    cross_section,
                    presentation_viewport: None,
                    render_extent: None,
                }),
                playback_active: false,
                playback_fps: mirante4d_application::PlaybackFps::DEFAULT,
                gpu_payload_capacity: 1,
            },
        }
    }

    #[test]
    fn ready_dataset_refresh_preserves_current_exact_s0_presented_fidelity() {
        let temp = tempfile::tempdir().unwrap();
        let package = crate::tests::write_target_fixture(temp.path()).unwrap();
        let opened = crate::tests::open_dataset_and_render_first_frame(&package).unwrap();
        let mut app = crate::tests::test_workbench_app_without_background_runtime(opened);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !app
            .prepared_scope_render_plans
            .contains_key(&crate::dataset_requests::SCOPE_CURRENT_3D)
        {
            assert!(
                Instant::now() < deadline,
                "the exact S0 demand plan did not become available"
            );
            app.request_visible_bricks();
            std::thread::yield_now();
        }
        let context = eframe::egui::Context::default();
        let refinement_deadline = Instant::now() + Duration::from_secs(5);
        while app.dataset.staging_current_refinement() {
            assert!(
                Instant::now() < refinement_deadline,
                "the exact S0 demand plan did not become current"
            );
            app.drain_brick_results(&context);
            app.request_visible_bricks();
            std::thread::yield_now();
        }
        assert_eq!(app.dataset.current_uniform_scale(), Some(ScaleLevel::BASE));

        let snapshot = app.application.snapshot();
        let intent = crate::product_render_intent::volume_intent(
            &snapshot,
            FrameIdentity::new(17),
            app.render_coordination.presentation_viewport,
            app.render_coordination.render_viewport,
            None,
        )
        .unwrap()
        .expect("the fixture has a visible volume layer");
        let prepared = app
            .prepared_scope_render_plans
            .values()
            .find(|plan| {
                plan.requirements
                    .body()
                    .canonical()
                    .iter()
                    .all(|key| key.scale() == ScaleLevel::BASE)
            })
            .expect("the promoted current frame retains its prepared S0 body");
        let requirements = prepared.requirements.bind(&intent).unwrap();
        assert!(
            requirements
                .resource_keys()
                .iter()
                .all(|key| key.scale() == ScaleLevel::BASE),
            "the regression frame must be an S0 presentation"
        );
        let coverage =
            FrameCoverage::from_available(&requirements, requirements.resource_keys()).unwrap();
        assert!(coverage.is_full());
        let progress = FrameProgress::new(
            coverage,
            mirante4d_render_api::FrameCompleteness::Exact,
            None,
        )
        .unwrap();
        let generation = app
            .render_coordination
            .surface(PresentationTarget::ThreeD)
            .generation();
        let presented = PresentedFrame::new(
            PresentationTarget::ThreeD,
            intent.extent(),
            progress.clone(),
        );
        assert!(
            !app.render_coordination.record_presented_frame(
                PresentationTarget::ThreeD,
                generation.saturating_add(1),
                presented.clone(),
            ),
            "a frame built for another surface generation must remain stale"
        );
        assert!(
            !app.render_coordination.record_presented_frame(
                PresentationTarget::ThreeD,
                generation,
                PresentedFrame::new(
                    PresentationTarget::ThreeD,
                    RenderExtent::new(
                        intent.extent().width_pixels() + 1,
                        intent.extent().height_pixels(),
                    )
                    .unwrap(),
                    progress,
                ),
            ),
            "a frame built for another render extent must remain stale"
        );
        assert!(app.render_coordination.record_presented_frame(
            PresentationTarget::ThreeD,
            generation,
            presented,
        ));
        app.render_coordination.frame_fidelity.display_freshness = DisplayedFrameFreshness::Current;
        app.render_coordination.frame_fidelity.displayed_scale_level = Some(0);
        app.render_coordination.frame_fidelity.completeness = FrameCompleteness::Exact;

        app.update_dataset_fidelity(true);

        assert_eq!(
            app.render_coordination.frame_fidelity.completeness,
            FrameCompleteness::Exact,
            "a dataset-ready refresh must not downgrade the current full Exact S0 frame",
        );
        for (displayed, freshness, presented, expected) in [
            (
                Some(1),
                DisplayedFrameFreshness::Current,
                Some(mirante4d_render_api::FrameCompleteness::Exact),
                FrameCompleteness::Complete,
            ),
            (
                Some(0),
                DisplayedFrameFreshness::Stale,
                Some(mirante4d_render_api::FrameCompleteness::Exact),
                FrameCompleteness::Complete,
            ),
            (
                Some(0),
                DisplayedFrameFreshness::Current,
                Some(mirante4d_render_api::FrameCompleteness::Complete),
                FrameCompleteness::Complete,
            ),
            (
                None,
                DisplayedFrameFreshness::Current,
                None,
                FrameCompleteness::Loading,
            ),
        ] {
            assert_eq!(
                dataset_fidelity_completeness(false, displayed, freshness, presented),
                expected,
                "dataset readiness must not manufacture unpublished fidelity",
            );
        }
        app.dataset.request_shutdown().unwrap();
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
    fn exact_currentness_requires_geometry_and_installed_body_for_each_view_family() {
        let installed = planning_signature(1);
        assert_eq!(
            installed_visible_demand_plan_currentness(Some(&installed), &installed, [true; 3],),
            super::VisibleDemandPlanCurrentness {
                current_3d: true,
                cross_sections: true,
                cross_section_panels: [true; 3],
            }
        );
        assert_eq!(
            installed_visible_demand_plan_currentness(None, &installed, [true; 3]),
            super::VisibleDemandPlanCurrentness::default()
        );

        let mut changed_3d = installed.clone();
        changed_3d.current_3d.render_viewport = RenderExtent::new(32, 16).unwrap();
        assert_eq!(
            installed_visible_demand_plan_currentness(Some(&installed), &changed_3d, [true; 3],),
            super::VisibleDemandPlanCurrentness {
                current_3d: false,
                cross_sections: true,
                cross_section_panels: [true; 3],
            }
        );

        let mut changed_cross = installed.clone();
        changed_cross.cross_sections.panels[0].render_extent =
            Some(RenderExtent::new(32, 16).unwrap());
        assert_eq!(
            installed_visible_demand_plan_currentness(Some(&installed), &changed_cross, [true; 3],),
            super::VisibleDemandPlanCurrentness {
                current_3d: true,
                cross_sections: false,
                cross_section_panels: [false, true, true],
            }
        );

        assert_eq!(
            installed_visible_demand_plan_currentness(
                Some(&installed),
                &installed,
                [true, false, true],
            ),
            super::VisibleDemandPlanCurrentness {
                current_3d: true,
                cross_sections: false,
                cross_section_panels: [true, false, true],
            }
        );
    }

    #[test]
    fn deterministic_planning_failure_runs_once_until_signature_or_capacity_changes() {
        let planning = planning_signature(1);
        let revision = RenderIntentRevision::initial();
        let renderer_requirement_base = RetainedRequirementHandle {
            requirements: Arc::from([]),
            charge: None,
        };
        let mut latch = None;
        let mut attempts = 0_u64;
        for _ in 0..128 {
            let blocked = latch.as_ref().is_some_and(
                |latch: &DeterministicFailureLatch<VisibleDemandFailureSignature>| {
                    latch.blocks(|failure| {
                        failure.matches_inputs(
                            revision,
                            &planning,
                            true,
                            CrossSectionPlanMask::default(),
                            true,
                            7,
                            11,
                            true,
                            true,
                        )
                    })
                },
            );
            if blocked {
                continue;
            }
            attempts = attempts.saturating_add(1);
            latch = Some(DeterministicFailureLatch::new(
                VisibleDemandFailureSignature::new(
                    revision,
                    planning.clone(),
                    true,
                    CrossSectionPlanMask::default(),
                    true,
                    Vec::new(),
                    renderer_requirement_base.clone(),
                    7,
                    11,
                ),
            ));
        }
        assert_eq!(attempts, 1, "unchanged UI polls must stay idle");

        let failed = latch.expect("the deterministic failure was latched");
        assert!(!failed.blocks(|failure| failure.matches_inputs(
            revision,
            &planning_signature(2),
            true,
            CrossSectionPlanMask::default(),
            true,
            7,
            11,
            true,
            true,
        )));
        assert!(!failed.blocks(|failure| {
            failure.matches_inputs(
                revision,
                &planning,
                true,
                CrossSectionPlanMask::default(),
                true,
                8,
                11,
                true,
                true,
            )
        }));
        assert!(!failed.blocks(|failure| {
            failure.matches_inputs(
                revision,
                &planning,
                true,
                CrossSectionPlanMask::default(),
                true,
                7,
                12,
                true,
                true,
            )
        }));
        assert!(!failed.blocks(|failure| {
            failure.matches_inputs(
                revision,
                &planning,
                true,
                CrossSectionPlanMask::default(),
                true,
                7,
                11,
                false,
                true,
            )
        }));
        assert!(!failed.blocks(|failure| {
            failure.matches_inputs(
                revision,
                &planning,
                true,
                CrossSectionPlanMask::default(),
                true,
                7,
                11,
                true,
                false,
            )
        }));
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
        assert!(!visible_demand_failure_is_deterministic(
            &anyhow::Error::new(VisibleDemandPlaceabilityRetry {
                target_group: "linked 2D",
                failed_union_payload_bytes: 1024,
                placement: PayloadPlacementFacts {
                    requested_bytes: 256,
                    total_free_bytes: 512,
                    largest_contiguous_bytes: 128,
                },
            })
        ));
        let recovery_backpressure = anyhow::Error::new(
            mirante4d_render_wgpu::WgpuRenderRuntimeError::PayloadRecoveryDeferred,
        );
        assert!(!visible_demand_failure_is_deterministic(
            &recovery_backpressure
        ));
        assert!(visible_demand_failure_is_recovery_backpressure(
            &recovery_backpressure
        ));
    }

    #[test]
    fn physical_placeability_limit_tightens_monotonically_and_reopens_for_new_input() {
        let first_planning = planning_signature(17);
        let second_planning = planning_signature(18);
        let first_retry = VisibleDemandPlaceabilityRetry {
            target_group: "linked 2D",
            failed_union_payload_bytes: 1_000,
            placement: PayloadPlacementFacts {
                requested_bytes: 300,
                total_free_bytes: 800,
                largest_contiguous_bytes: 200,
            },
        };
        let first =
            VisibleDemandPlaceabilityLimit::tightened(None, first_planning.clone(), first_retry);
        assert_eq!(first.max_gpu_payload_bytes, 999);
        assert!(first.applies_to(&first_planning));
        assert!(!first.applies_to(&second_planning));

        let second = VisibleDemandPlaceabilityLimit::tightened(
            Some(&first),
            first_planning.clone(),
            VisibleDemandPlaceabilityRetry {
                failed_union_payload_bytes: 700,
                ..first_retry
            },
        );
        assert_eq!(second.max_gpu_payload_bytes, 699);
        assert_eq!(
            second.terminal_error().to_string(),
            "linked 2D minimum navigation demand remains physically unplaceable after bounded \
             compaction: one 300-byte payload requires a contiguous range; 800 bytes are free in \
             aggregate and the largest contiguous range is 200 bytes"
        );

        let reopened = VisibleDemandPlaceabilityLimit::tightened(
            Some(&second),
            second_planning.clone(),
            first_retry,
        );
        assert_eq!(reopened.max_gpu_payload_bytes, 999);
        assert!(reopened.applies_to(&second_planning));
    }

    #[test]
    fn residual_coordinated_placement_refusal_forces_adaptive_replan_without_red_error() {
        let temp = tempfile::tempdir().unwrap();
        let package = crate::tests::write_target_fixture(temp.path()).unwrap();
        let opened = crate::tests::open_dataset_and_render_first_frame(&package).unwrap();
        let mut app = crate::tests::test_workbench_app_without_background_runtime(opened);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.request_visible_bricks();
            if app.pending_visible_demand_plan.is_none()
                && app.visible_demand_planning_signature.is_some()
                && !app
                    .dataset
                    .renderer_requirement_handle()
                    .requirements
                    .is_empty()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the fixture did not install its initial visible requirement union"
            );
            std::thread::yield_now();
        }
        let planning = app
            .visible_demand_planning_signature
            .clone()
            .expect("the initial visible plan installed its signature");
        let failed_union_payload_bytes = app
            .dataset
            .renderer_requirement_handle()
            .requirements
            .iter()
            .try_fold(0_u64, |total, key| {
                let descriptor = app
                    .application
                    .snapshot()
                    .catalog()
                    .resource_payload_descriptor(*key)
                    .unwrap();
                total.checked_add(
                    mirante4d_render_wgpu::payload_allocation_bytes(descriptor).unwrap(),
                )
            })
            .unwrap();
        assert_ne!(failed_union_payload_bytes, 0);

        assert!(app.recover_from_coordinated_payload_placement(
            "linked 2D",
            mirante4d_render_wgpu::WgpuRenderRuntimeError::PayloadPlacementUnavailable {
                requested_bytes: 300,
                total_free_bytes: 800,
                largest_contiguous_bytes: 200,
            },
        ));

        assert!(app.visible_demand_planning_signature.is_none());
        let limit = app
            .visible_demand_placeability_limit
            .as_ref()
            .expect("the residual refusal installs a monotone selection bound");
        assert!(limit.applies_to(&planning));
        assert_eq!(limit.max_gpu_payload_bytes, failed_union_payload_bytes - 1);
        assert_eq!(app.dataset.last_plan_error(), None);
        assert_eq!(
            app.render_coordination.frame_fidelity.last_capacity_error,
            None
        );
        assert!(app.render_coordination.refresh_requested());
        app.dataset.request_shutdown().unwrap();
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
    fn scoped_runtime_faults_preserve_an_existing_presentation() {
        assert!(!admission_fault_invalidates_presentation(true));
        assert!(admission_fault_invalidates_presentation(false));
    }
}
