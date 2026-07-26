use std::{collections::BTreeSet, sync::Arc};

use super::*;
use crate::{
    cross_section_scheduler::{
        CROSS_SECTION_PANEL_RENDER_SUBMISSIONS_PER_PANEL_REFRESH, CrossSectionScheduleInput,
        schedule_cross_section_panel,
    },
    dataset_requests::{
        SCOPE_CROSS_SECTION_XY, SCOPE_CROSS_SECTION_XZ, SCOPE_CROSS_SECTION_YZ, SCOPE_CURRENT_3D,
        SCOPE_CURRENT_3D_REFINEMENT, ScopeReconciliationTargets,
    },
    native_presentation::{
        ProductFrameExecutionTiming, ProductGpuExecutionIdentity, ProductGpuExecutionTiming,
        ProductLayerRequirementFacts, ProductPresentationTarget,
    },
    product_render_intent::{ProductRenderRequest, cross_section_intent, volume_intent},
    viewer_layout::PanelId,
};
use mirante4d_dataset::DatasetResourceKey;
use mirante4d_domain::RenderMode;
use mirante4d_render_api::{
    FrameCompleteness as RenderFrameCompleteness, FrameIdentity, FrameLimitation,
    MAX_RENDER_LAYERS, PresentationToken, RenderExtent,
};
use mirante4d_render_wgpu::{GpuFrameTiming, RetainedFrameRenderPolicy, WgpuRenderRuntimeError};

const MAX_REQUIREMENT_VISITS_PER_RENDER: usize = 1_024;
const MAX_LEASE_UPDATES_PER_RENDER: usize = 512;
const MAX_LEASE_UPDATE_PAYLOAD_BYTES: u64 =
    mirante4d_render_wgpu::FrameBudget::interactive().payload_upload_bytes();

#[derive(Debug, Clone, Copy)]
pub(crate) struct PerformanceMilestoneObservation {
    pub(crate) first_useful: bool,
    pub(crate) complete_replacement: bool,
    pub(crate) complete_coarse: bool,
    pub(crate) target_current: bool,
    pub(crate) foreground_idle: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LayerPerformanceMilestones {
    layer_ordinal: Option<u32>,
    first_current_presented_ms: Option<f64>,
    first_useful_frame_ms: Option<f64>,
    complete_coarse_ms: Option<f64>,
    complete_replacement_ms: Option<f64>,
    target_settled_ms: Option<f64>,
}

impl LayerPerformanceMilestones {
    pub(crate) const fn layer_ordinal(self) -> Option<u32> {
        self.layer_ordinal
    }

    pub(crate) const fn first_current_presented_ms(self) -> Option<f64> {
        self.first_current_presented_ms
    }

    pub(crate) const fn first_useful_frame_ms(self) -> Option<f64> {
        self.first_useful_frame_ms
    }

    pub(crate) const fn complete_coarse_ms(self) -> Option<f64> {
        self.complete_coarse_ms
    }

    pub(crate) const fn complete_replacement_ms(self) -> Option<f64> {
        self.complete_replacement_ms
    }

    pub(crate) const fn target_settled_ms(self) -> Option<f64> {
        self.target_settled_ms
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PanelPerformanceMilestones {
    first_current_presented_ms: Option<f64>,
    first_useful_frame_ms: Option<f64>,
    complete_coarse_ms: Option<f64>,
    complete_replacement_ms: Option<f64>,
    target_settled_ms: Option<f64>,
    visible_layers: [LayerPerformanceMilestones; MAX_RENDER_LAYERS],
    visible_layer_overflow: bool,
}

impl Default for PanelPerformanceMilestones {
    fn default() -> Self {
        Self {
            first_current_presented_ms: None,
            first_useful_frame_ms: None,
            complete_coarse_ms: None,
            complete_replacement_ms: None,
            target_settled_ms: None,
            visible_layers: [LayerPerformanceMilestones::default(); MAX_RENDER_LAYERS],
            visible_layer_overflow: false,
        }
    }
}

impl PanelPerformanceMilestones {
    pub(crate) const fn first_current_presented_ms(self) -> Option<f64> {
        self.first_current_presented_ms
    }

    pub(crate) const fn first_useful_frame_ms(self) -> Option<f64> {
        self.first_useful_frame_ms
    }

    pub(crate) const fn complete_coarse_ms(self) -> Option<f64> {
        self.complete_coarse_ms
    }

    pub(crate) const fn complete_replacement_ms(self) -> Option<f64> {
        self.complete_replacement_ms
    }

    pub(crate) const fn target_settled_ms(self) -> Option<f64> {
        self.target_settled_ms
    }

    pub(crate) const fn visible_layers(&self) -> &[LayerPerformanceMilestones; MAX_RENDER_LAYERS] {
        &self.visible_layers
    }

    pub(crate) const fn visible_layer_overflow(self) -> bool {
        self.visible_layer_overflow
    }
}

/// Exact immutable inputs for one terminal product-render failure.
///
/// The requirement body deliberately uses allocation identity: replacing a
/// prepared body is a real planning event even when it happens to contain the
/// same keys. Frame identity covers every render-intent change, while the two
/// input epochs cover residency and retained CPU payload changes.
#[derive(Debug, Clone)]
pub(crate) struct ProductRenderFailureSignature {
    token: PresentationToken,
    frame: FrameIdentity,
    requirement_body: Arc<[DatasetResourceKey]>,
    residency_invalidation_epoch: u64,
    retained_lease_generation: u64,
    runtime_recovery_epoch: u64,
}

impl ProductRenderFailureSignature {
    fn new(
        token: PresentationToken,
        frame: FrameIdentity,
        requirement_body: Arc<[DatasetResourceKey]>,
        residency_invalidation_epoch: u64,
        retained_lease_generation: u64,
        runtime_recovery_epoch: u64,
    ) -> Self {
        Self {
            token,
            frame,
            requirement_body,
            residency_invalidation_epoch,
            retained_lease_generation,
            runtime_recovery_epoch,
        }
    }

    fn matches_current(&self, current: &Self) -> bool {
        self.token == current.token
            && self.frame == current.frame
            && Arc::ptr_eq(&self.requirement_body, &current.requirement_body)
            && self.residency_invalidation_epoch == current.residency_invalidation_epoch
            && self.retained_lease_generation == current.retained_lease_generation
            && self.runtime_recovery_epoch == current.runtime_recovery_epoch
    }
}

/// Only outcomes with a concrete asynchronous completion path bypass the
/// latch. Capacity/configuration failures are terminal for the exact immutable
/// signature; a residency, payload, body, intent, or source/runtime-recovery
/// epoch change naturally produces a different signature and permits one new
/// try.
fn product_render_failure_is_deterministic(error: WgpuRenderRuntimeError) -> bool {
    !matches!(
        error,
        WgpuRenderRuntimeError::StaleFrame { .. }
            | WgpuRenderRuntimeError::PickCapacityExceeded
            | WgpuRenderRuntimeError::PickBackpressure
    )
}

pub(crate) const fn display_refresh_path_label(path: DisplayRefreshPath) -> &'static str {
    match path {
        DisplayRefreshPath::GpuResidentDisplay => "gpu display",
        DisplayRefreshPath::UiBackground => "ui background",
    }
}

fn hidden_target_ready_for_atomic_swap(
    presented_frame: FrameIdentity,
    presented_extent: RenderExtent,
    request_frame: FrameIdentity,
    request_extent: RenderExtent,
    completeness: RenderFrameCompleteness,
    total_requirements: u64,
    staged_requirement_count: usize,
) -> bool {
    presented_frame == request_frame
        && presented_extent == request_extent
        && matches!(completeness, RenderFrameCompleteness::Exact)
        && u64::try_from(staged_requirement_count).ok() == Some(total_requirements)
}

fn retained_frame_render_policy(
    publish_to_display: bool,
    existing_presentation: Option<RenderFrameCompleteness>,
) -> RetainedFrameRenderPolicy {
    let replacing_settled_pixels = existing_presentation
        .is_some_and(|completeness| completeness != RenderFrameCompleteness::Progressive);
    if publish_to_display && !replacing_settled_pixels {
        RetainedFrameRenderPolicy::EveryUsefulFrame
    } else {
        RetainedFrameRenderPolicy::ExactFrameOnly
    }
}

fn cross_section_schedule_for_presented_coverage(
    mut schedule: CrossSectionPanelScheduleState,
    completeness: RenderFrameCompleteness,
    available_requirements: u64,
    total_requirements: u64,
) -> CrossSectionPanelScheduleState {
    debug_assert!(available_requirements <= total_requirements);
    schedule.selected_bricks = usize::try_from(total_requirements).unwrap_or(usize::MAX);
    schedule.occupied_selected_bricks =
        usize::try_from(available_requirements).unwrap_or(usize::MAX);
    schedule.missing_occupied_bricks = schedule
        .selected_bricks
        .saturating_sub(schedule.occupied_selected_bricks);
    if completeness == RenderFrameCompleteness::Progressive {
        schedule.status = CrossSectionPanelScheduleStatus::Incomplete;
    }
    schedule
}

const fn cross_section_render_allowed(
    matching_demand_plan_installed: bool,
    panel_needs_render: bool,
) -> bool {
    matching_demand_plan_installed
        && panel_needs_render
        && CROSS_SECTION_PANEL_RENDER_SUBMISSIONS_PER_PANEL_REFRESH > 0
}

fn requirement_binding_is_reusable(
    same_body: bool,
    current_prefetch_promoted: Option<bool>,
    prepared_prefetch_promoted: bool,
) -> bool {
    same_body
        && current_prefetch_promoted.is_none_or(|current| current == prepared_prefetch_promoted)
}

/// Finds the CPU leases that have not already been forwarded to, or observed
/// resident in, one presentation target. Once every exact requirement is
/// satisfied at an unchanged residency epoch, the hot navigation path returns
/// before touching the requirement slice or the retained-lease map.
// The callbacks deliberately keep this bounded scan generic and allocation
// free; a parameter object would obscure the separately borrowed state.
#[allow(clippy::too_many_arguments)]
fn collect_requirement_lease_updates<K, E, T>(
    requirements: &[K],
    requirement_key: impl Fn(&K) -> DatasetResourceKey,
    satisfied_keys: &mut std::collections::HashSet<DatasetResourceKey>,
    next_unsatisfied_requirement: &mut usize,
    last_residency_invalidation_epoch: &mut Option<E>,
    residency_invalidation_epoch: E,
    mut resource_is_resident: impl FnMut(DatasetResourceKey) -> bool,
    mut lease_handle: impl FnMut(DatasetResourceKey) -> Option<T>,
    mut lease_upload_bytes: impl FnMut(&T) -> u64,
) -> RequirementLeaseUpdateWork<T>
where
    E: Copy + PartialEq,
{
    let mut work = RequirementLeaseUpdateWork::default();
    if *last_residency_invalidation_epoch == Some(residency_invalidation_epoch)
        && satisfied_keys.len() == requirements.len()
    {
        return work;
    }
    if *last_residency_invalidation_epoch != Some(residency_invalidation_epoch) {
        satisfied_keys.retain(|key| {
            let resident = resource_is_resident(*key);
            if !resident {
                work.removed_satisfied_keys.push(*key);
            }
            resident
        });
        *next_unsatisfied_requirement = 0;
        *last_residency_invalidation_epoch = Some(residency_invalidation_epoch);
    }
    if satisfied_keys.len() == requirements.len() {
        return work;
    }
    work.starting_requirement_cursor = *next_unsatisfied_requirement;
    let visit_limit = requirements.len().min(MAX_REQUIREMENT_VISITS_PER_RENDER);
    while work.requirements_visited < visit_limit
        && work.lease_updates.len() < MAX_LEASE_UPDATES_PER_RENDER
    {
        if *next_unsatisfied_requirement == requirements.len() {
            *next_unsatisfied_requirement = 0;
        }
        let requirement_index = *next_unsatisfied_requirement;
        let key = requirement_key(&requirements[requirement_index]);
        work.requirements_visited = work.requirements_visited.saturating_add(1);
        *next_unsatisfied_requirement += 1;
        if satisfied_keys.contains(&key) {
            continue;
        }
        if resource_is_resident(key) {
            satisfied_keys.insert(key);
            work.observed_resident_keys.push(key);
            continue;
        }
        work.lease_lookups = work.lease_lookups.saturating_add(1);
        if let Some(lease) = lease_handle(key) {
            let upload_bytes = lease_upload_bytes(&lease);
            if upload_bytes > 0
                && work
                    .lease_payload_bytes
                    .checked_add(upload_bytes)
                    .is_none_or(|total| total > MAX_LEASE_UPDATE_PAYLOAD_BYTES)
            {
                // Revisit the first byte-limited key before continuing the
                // circular scan on the next frame.
                *next_unsatisfied_requirement = requirement_index;
                break;
            }
            // Selection is not commitment. The renderer explicitly reports
            // whether it retained the complete ordered cohort; failure or
            // rejection rewinds this pass to preserve exact priority.
            work.selected_lease_keys.push(key);
            work.lease_updates.push(lease);
            work.lease_payload_bytes += upload_bytes;
        }
    }
    work
}

fn commit_or_rewind_retained_updates(
    satisfied_keys: &mut std::collections::HashSet<DatasetResourceKey>,
    next_unsatisfied_requirement: &mut usize,
    starting_requirement_cursor: usize,
    selected_lease_keys: &[DatasetResourceKey],
    retained_updates_accepted: bool,
) {
    if retained_updates_accepted {
        satisfied_keys.extend(selected_lease_keys.iter().copied());
    } else {
        *next_unsatisfied_requirement = starting_requirement_cursor;
    }
}

/// Applies a renderer-reported exact eviction delta to every presentation
/// cache. Work is proportional to targets × evictions; the 65k satisfied-key
/// bodies are never retained/scanned on the ordinary arena-eviction path.
fn apply_exact_residency_delta<'a, E: Copy + 'a>(
    targets: impl IntoIterator<
        Item = (
            &'a [DatasetResourceKey],
            &'a mut std::collections::HashSet<DatasetResourceKey>,
            &'a mut usize,
            &'a mut Option<E>,
        ),
    >,
    invalidation_epoch: E,
    evicted_keys: &[DatasetResourceKey],
) -> usize {
    let mut exact_membership_probes = 0_usize;
    for (requirements, satisfied, cursor, epoch) in targets {
        for key in evicted_keys {
            exact_membership_probes = exact_membership_probes.saturating_add(1);
            if satisfied.remove(key)
                && let Ok(index) = requirements.binary_search(key)
            {
                *cursor = (*cursor).min(index);
            }
        }
        *epoch = Some(invalidation_epoch);
    }
    exact_membership_probes
}

struct RequirementLeaseUpdateWork<T> {
    lease_updates: Vec<T>,
    selected_lease_keys: Vec<DatasetResourceKey>,
    /// Exact renderer-resident keys observed during this bounded membership
    /// pass. Unlike the renderer's upload delta this also includes resources
    /// uploaded earlier by another presentation target.
    observed_resident_keys: Vec<DatasetResourceKey>,
    removed_satisfied_keys: Vec<DatasetResourceKey>,
    starting_requirement_cursor: usize,
    requirements_visited: usize,
    lease_lookups: usize,
    lease_payload_bytes: u64,
}

impl<T> Default for RequirementLeaseUpdateWork<T> {
    fn default() -> Self {
        Self {
            lease_updates: Vec::new(),
            selected_lease_keys: Vec::new(),
            observed_resident_keys: Vec::new(),
            removed_satisfied_keys: Vec::new(),
            starting_requirement_cursor: 0,
            requirements_visited: 0,
            lease_lookups: 0,
            lease_payload_bytes: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DisplayRenderTiming {
    pub(crate) path: DisplayRefreshPath,
    pub(crate) render_ms: f64,
    pub(crate) gpu_upload_ms: Option<f64>,
    pub(crate) gpu_compute_ms: Option<f64>,
    pub(crate) egui_texture_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DisplayRefreshWorkTiming {
    render: DisplayRenderTiming,
    visible_brick_request_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StagedCurrentProductFrameResult {
    rendered: bool,
    published_gpu_timing: Option<GpuFrameTiming>,
}

impl DisplayRefreshWorkTiming {
    const fn new(render: DisplayRenderTiming, visible_brick_request_ms: f64) -> Self {
        Self {
            render,
            visible_brick_request_ms,
        }
    }
}

/// Allocation-free, generation-bound clocks for the milestones that matter
/// to interactive latency. Values are monotonic elapsed milliseconds from the
/// input generation that invalidated the prior display.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DisplayPerformanceMilestones {
    generation: u64,
    started_at: Instant,
    first_current_presented_ms: Option<f64>,
    first_useful_frame_ms: Option<f64>,
    complete_coarse_ms: Option<f64>,
    complete_replacement_ms: Option<f64>,
    target_settled_ms: Option<f64>,
    three_d_first_useful_frame_ms: Option<f64>,
    three_d_complete_coarse_ms: Option<f64>,
    three_d_complete_replacement_ms: Option<f64>,
    three_d_target_settled_ms: Option<f64>,
    panels: [PanelPerformanceMilestones; 4],
}

impl Default for DisplayPerformanceMilestones {
    fn default() -> Self {
        Self {
            generation: 0,
            started_at: Instant::now(),
            first_current_presented_ms: None,
            first_useful_frame_ms: None,
            complete_coarse_ms: None,
            complete_replacement_ms: None,
            target_settled_ms: None,
            three_d_first_useful_frame_ms: None,
            three_d_complete_coarse_ms: None,
            three_d_complete_replacement_ms: None,
            three_d_target_settled_ms: None,
            panels: [PanelPerformanceMilestones::default(); 4],
        }
    }
}

impl DisplayPerformanceMilestones {
    pub(crate) fn begin_generation(&mut self, generation: u64) {
        self.generation = generation;
        self.started_at = Instant::now();
        self.first_current_presented_ms = None;
        self.first_useful_frame_ms = None;
        self.complete_coarse_ms = None;
        self.complete_replacement_ms = None;
        self.target_settled_ms = None;
        self.three_d_first_useful_frame_ms = None;
        self.three_d_complete_coarse_ms = None;
        self.three_d_complete_replacement_ms = None;
        self.three_d_target_settled_ms = None;
        self.panels = [PanelPerformanceMilestones::default(); 4];
    }

    const fn panel_index(slot: PresentationSlot) -> usize {
        match slot {
            PresentationSlot::ThreeD => 0,
            PresentationSlot::Xy => 1,
            PresentationSlot::Xz => 2,
            PresentationSlot::Yz => 3,
        }
    }

    pub(crate) fn observe_panel(
        &mut self,
        slot: PresentationSlot,
        observation: PerformanceMilestoneObservation,
    ) {
        let elapsed_ms = duration_ms(self.started_at.elapsed());
        let panel = &mut self.panels[Self::panel_index(slot)];
        if observation.first_useful {
            panel.first_useful_frame_ms.get_or_insert(elapsed_ms);
        }
        if observation.complete_replacement {
            panel.complete_replacement_ms.get_or_insert(elapsed_ms);
        }
        if observation.complete_coarse {
            panel.complete_coarse_ms.get_or_insert(elapsed_ms);
        }
        if observation.target_current {
            panel.first_current_presented_ms.get_or_insert(elapsed_ms);
            if observation.foreground_idle {
                panel.target_settled_ms.get_or_insert(elapsed_ms);
            }
        }
    }

    pub(crate) fn observe_visible_layer(
        &mut self,
        slot: PresentationSlot,
        layer_ordinal: u32,
        observation: PerformanceMilestoneObservation,
    ) {
        let elapsed_ms = duration_ms(self.started_at.elapsed());
        let panel = &mut self.panels[Self::panel_index(slot)];
        let layer_index = panel
            .visible_layers
            .iter()
            .position(|milestones| milestones.layer_ordinal == Some(layer_ordinal))
            .or_else(|| {
                panel
                    .visible_layers
                    .iter()
                    .position(|milestones| milestones.layer_ordinal.is_none())
            });
        let Some(layer_index) = layer_index else {
            panel.visible_layer_overflow = true;
            return;
        };
        let layer = &mut panel.visible_layers[layer_index];
        layer.layer_ordinal = Some(layer_ordinal);
        if observation.first_useful {
            layer.first_useful_frame_ms.get_or_insert(elapsed_ms);
        }
        if observation.complete_replacement {
            layer.complete_replacement_ms.get_or_insert(elapsed_ms);
        }
        if observation.complete_coarse {
            layer.complete_coarse_ms.get_or_insert(elapsed_ms);
        }
        if observation.target_current {
            layer.first_current_presented_ms.get_or_insert(elapsed_ms);
            if observation.foreground_idle {
                layer.target_settled_ms.get_or_insert(elapsed_ms);
            }
        }
    }

    fn observe_three_d(
        &mut self,
        frame: &mirante4d_render_api::PresentedFrame,
        displayed_scale_level: u32,
        target_scale_level: u32,
        target_settled: bool,
    ) {
        let elapsed_ms = duration_ms(self.started_at.elapsed());
        if frame.progress().coverage().available_requirements() > 0 {
            self.three_d_first_useful_frame_ms.get_or_insert(elapsed_ms);
        }
        let coverage = frame.progress().coverage();
        let complete_coverage = frame.progress().completeness()
            != RenderFrameCompleteness::Progressive
            && coverage.available_requirements() == coverage.total_requirements();
        if complete_coverage {
            self.three_d_complete_replacement_ms
                .get_or_insert(elapsed_ms);
            if displayed_scale_level > target_scale_level {
                self.three_d_complete_coarse_ms.get_or_insert(elapsed_ms);
            }
            if target_settled {
                self.three_d_target_settled_ms.get_or_insert(elapsed_ms);
            }
        }
    }

    pub(crate) fn observe_coordinated_layout(
        &mut self,
        first_useful: bool,
        complete_replacement: bool,
        complete_coarse: bool,
        target_current: bool,
        foreground_idle: bool,
    ) {
        let elapsed_ms = duration_ms(self.started_at.elapsed());
        if first_useful {
            self.first_useful_frame_ms.get_or_insert(elapsed_ms);
        }
        if complete_replacement {
            self.complete_replacement_ms.get_or_insert(elapsed_ms);
        }
        if complete_coarse {
            self.complete_coarse_ms.get_or_insert(elapsed_ms);
        }
        if target_current {
            self.first_current_presented_ms.get_or_insert(elapsed_ms);
            if foreground_idle {
                self.target_settled_ms.get_or_insert(elapsed_ms);
            }
        }
    }

    pub(crate) const fn generation(self) -> u64 {
        self.generation
    }

    pub(crate) const fn first_current_presented_ms(self) -> Option<f64> {
        self.first_current_presented_ms
    }

    pub(crate) const fn first_useful_frame_ms(self) -> Option<f64> {
        self.first_useful_frame_ms
    }

    pub(crate) const fn complete_replacement_ms(self) -> Option<f64> {
        self.complete_replacement_ms
    }

    pub(crate) const fn complete_coarse_ms(self) -> Option<f64> {
        self.complete_coarse_ms
    }

    pub(crate) const fn target_settled_ms(self) -> Option<f64> {
        self.target_settled_ms
    }

    pub(crate) const fn three_d_first_useful_frame_ms(self) -> Option<f64> {
        self.three_d_first_useful_frame_ms
    }

    pub(crate) const fn three_d_complete_coarse_ms(self) -> Option<f64> {
        self.three_d_complete_coarse_ms
    }

    pub(crate) const fn three_d_complete_replacement_ms(self) -> Option<f64> {
        self.three_d_complete_replacement_ms
    }

    pub(crate) const fn three_d_target_settled_ms(self) -> Option<f64> {
        self.three_d_target_settled_ms
    }

    pub(crate) const fn panel(&self, slot: PresentationSlot) -> &PanelPerformanceMilestones {
        &self.panels[Self::panel_index(slot)]
    }
}

pub(crate) fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

impl MiranteWorkbenchApp {
    pub(crate) fn application_snapshot_for_ui(&self) -> ApplicationSnapshot {
        let snapshot = self.application.snapshot();
        let three_d = Some(self.presentation_surface(
            PanelId::ThreeD,
            self.render_coordination.presentation_viewport,
            self.render_coordination.frame_fidelity.display_freshness
                == DisplayedFrameFreshness::Current,
        ));
        let (xy, xz, yz) =
            if application_view(&snapshot).layout() == CanonicalViewerLayout::FourPanel {
                (
                    self.cross_section_presentation_surface(PanelId::Xy),
                    self.cross_section_presentation_surface(PanelId::Xz),
                    self.cross_section_presentation_surface(PanelId::Yz),
                )
            } else {
                (None, None, None)
            };
        snapshot
            .with_presentations(PresentationSnapshot::new(three_d, xy, xz, yz))
            .with_import_workflow(self.import.snapshot())
    }

    fn cross_section_presentation_surface(&self, panel_id: PanelId) -> Option<PresentationSurface> {
        let panel = self
            .render_coordination
            .surface(panel_id.presentation_slot());
        Some(self.presentation_surface(
            panel_id,
            panel.presentation_viewport()?,
            panel.display_current(),
        ))
    }

    fn presentation_surface(
        &self,
        panel_id: PanelId,
        viewport: PresentationViewport,
        frame_is_current: bool,
    ) -> PresentationSurface {
        let frame = self
            .native_presentation
            .product_gpu
            .as_ref()
            .and_then(|product| product.targets.get(&panel_id))
            .and_then(|target| target.presented.clone());
        PresentationSurface::with_frame_currentness(viewport, frame, frame_is_current)
    }

    fn product_display(
        &self,
        snapshot: &ApplicationSnapshot,
        slot: PresentationSlot,
    ) -> Option<RenderExtent> {
        let frame = snapshot.presentations().get(slot)?.frame()?;
        Some(frame.extent())
    }

    pub(crate) fn clear_3d_product_presentation(&mut self) {
        if let Some(product) = self.native_presentation.product_gpu.as_mut() {
            if let Some(staging) = product.staging_3d.as_mut() {
                if let Err(error) = product.renderer.deactivate_presentation(staging.token) {
                    tracing::error!(%error, "hidden 3D presentation deactivation failed");
                } else {
                    staging.reset();
                }
            }
            if let Some(token) = product
                .targets
                .get(&PanelId::ThreeD)
                .map(|target| target.token)
            {
                if let Err(error) = product.renderer.deactivate_presentation(token) {
                    tracing::error!(%error, "3D presentation deactivation failed");
                } else {
                    product
                        .targets
                        .get_mut(&PanelId::ThreeD)
                        .expect("the deactivated 3D target remains registered")
                        .reset();
                }
            }
        }
        self.viewer_render_failure_latches.clear();
        self.render_coordination.frame_fidelity.display_freshness =
            DisplayedFrameFreshness::Unknown;
    }

    pub(crate) fn clear_cross_section_product_presentations(&mut self) {
        if let Some(product) = self.native_presentation.product_gpu.as_mut() {
            for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
                let Some(token) = product.targets.get(&panel).map(|target| target.token) else {
                    continue;
                };
                if let Err(error) = product.renderer.deactivate_presentation(token) {
                    tracing::error!(%error, ?panel, "cross-section presentation deactivation failed");
                    continue;
                }
                if let Some(target) = product.targets.get_mut(&panel) {
                    target.request = None;
                    target.requirement_keys = Arc::from([]);
                    target.lease_priority_keys = Arc::from([]);
                    target.satisfied_requirement_keys.clear();
                    target.next_unsatisfied_requirement = 0;
                    target.last_residency_invalidation_epoch = None;
                    target.last_renderer_available_resources = 0;
                    target.presented = None;
                    target.pending_capture = None;
                    target.completed_capture = None;
                    target.pending_gpu_timings.clear();
                    target.last_execution_timing = None;
                    target.partial_seen = false;
                    target.reset_layer_requirement_facts();
                    target.presented_layer_requirement_facts.clear();
                    target.set_progressive_lease_probe_state(Default::default());
                }
            }
        }
        self.viewer_render_failure_latches.clear();
    }

    pub(crate) fn invalidate_cross_section_panel_display_frames(&mut self) {
        self.render_coordination.invalidate_cross_sections();
    }

    pub(crate) fn clear_product_presentations(&mut self) {
        if let Some(product) = self.native_presentation.product_gpu.as_mut() {
            if let Some(staging) = product.staging_3d.as_mut() {
                if let Err(error) = product.renderer.deactivate_presentation(staging.token) {
                    tracing::error!(%error, "hidden 3D presentation deactivation failed");
                } else {
                    staging.reset();
                }
            }
            let targets = product
                .targets
                .iter()
                .map(|(panel, target)| (*panel, target.token))
                .collect::<Vec<_>>();
            for (panel, token) in targets {
                if let Err(error) = product.renderer.deactivate_presentation(token) {
                    tracing::error!(%error, ?panel, "presentation deactivation failed");
                    continue;
                }
                let target = product
                    .targets
                    .get_mut(&panel)
                    .expect("the deactivated target remains registered");
                target.request = None;
                target.requirement_keys = Arc::from([]);
                target.lease_priority_keys = Arc::from([]);
                target.satisfied_requirement_keys.clear();
                target.next_unsatisfied_requirement = 0;
                target.last_residency_invalidation_epoch = None;
                target.last_renderer_available_resources = 0;
                target.presented = None;
                target.pending_capture = None;
                target.completed_capture = None;
                target.pending_gpu_timings.clear();
                target.last_execution_timing = None;
                target.partial_seen = false;
                target.reset_layer_requirement_facts();
                target.presented_layer_requirement_facts.clear();
                target.set_progressive_lease_probe_state(Default::default());
            }
        }
        self.viewer_render_failure_latches.clear();
        self.render_coordination.frame_fidelity.display_freshness =
            DisplayedFrameFreshness::Unknown;
    }

    /// Hard source-generation boundary. Unlike ordinary presentation
    /// deactivation, this also retires shared GPU residency and asynchronous
    /// source-scoped tickets before the replacement runtime is installed.
    pub(crate) fn retire_product_dataset_generation(&mut self) {
        if let Some(product) = self.native_presentation.product_gpu.as_mut() {
            product.retire_dataset_generation();
        }
        self.viewer_pick_queue.retire_source_generation();
        self.viewer_render_failure_latches.clear();
        self.render_coordination.frame_fidelity.display_freshness =
            DisplayedFrameFreshness::Unknown;
    }

    fn cross_section_panel_needs_display_render(&self, panel_id: PanelId) -> bool {
        if panel_id.cross_section_panel().is_none() {
            return false;
        }
        let panel = self
            .render_coordination
            .surface(panel_id.presentation_slot());
        let progressive_render_requested = self
            .native_presentation
            .product_gpu
            .as_ref()
            .and_then(|product| product.targets.get(&panel_id))
            .is_some_and(|target| {
                target.presented.as_ref().is_some_and(|frame| {
                    frame.progress().completeness() == RenderFrameCompleteness::Progressive
                }) && target.progressive_render_requested()
            });
        panel.render_failure().is_none()
            && (!panel.display_current() || progressive_render_requested)
    }

    pub(crate) fn render_cross_section_panel_for_display_if_needed(
        &mut self,
        panel_id: PanelId,
    ) -> anyhow::Result<Option<DisplayRenderTiming>> {
        // Cross-section requirements depend on the plane and panel viewport.
        // Keep the previous pixels stale until the installed signature proves
        // that the plane and viewport match the requirement body. Pending,
        // failed, and rejected planning all deliberately fail this proof.
        if !cross_section_render_allowed(
            self.visible_demand_plan_currentness().cross_sections,
            self.cross_section_panel_needs_display_render(panel_id),
        ) {
            return Ok(None);
        }
        let snapshot = self.application.snapshot();
        let view = application_view(&snapshot);
        let scope = cross_section_scope(panel_id)?;
        let requirements = self.dataset.scope_requirement_handle(scope);
        let lease_priority = self.dataset.scope_gpu_priority_handle(scope);
        let active_layer_target = self
            .dataset
            .scope_layer_scales(scope)
            .and_then(|scales| scales.get(&view.active_layer()))
            .copied();
        let gpu_available = self.native_presentation.product_gpu.is_some();
        let schedule = schedule_cross_section_panel(
            &mut self.render_coordination,
            CrossSectionScheduleInput {
                view,
                active_layer_target,
                requirements: requirements.as_ref(),
                retained_leases: self.dataset.retained_leases(),
                dataset_failed: self.dataset.dispatcher().scope_failure(scope).is_some(),
            },
            panel_id,
            gpu_available,
        )?
        .schedule;
        if !schedule.is_renderable() {
            return Ok(None);
        }
        let panel = self
            .render_coordination
            .surface(panel_id.presentation_slot());
        let presentation = panel
            .presentation_viewport()
            .ok_or_else(|| anyhow::anyhow!("cross-section presentation viewport is unavailable"))?;
        let extent = panel
            .render_viewport()
            .ok_or_else(|| anyhow::anyhow!("cross-section render viewport is unavailable"))?;
        let generation = panel.generation();
        let render_start = Instant::now();
        let rendered = match self.render_product_target(
            panel_id,
            scope,
            Some(panel_id),
            &snapshot,
            presentation,
            extent,
            requirements,
            lease_priority,
            true,
        ) {
            Ok(rendered) => rendered,
            Err(error) => {
                let failure = render_state::render_failure_status(&error);
                self.render_coordination.record_cross_section_failure(
                    panel_id.presentation_slot(),
                    schedule,
                    failure,
                );
                return Err(error);
            }
        };
        if !rendered {
            return Ok(None);
        }
        let presented = self
            .native_presentation
            .product_gpu
            .as_ref()
            .and_then(|product| product.targets.get(&panel_id))
            .and_then(|target| target.presented.clone())
            .expect("a rendered cross-section target retains its presented frame");
        let coverage = presented.progress().coverage();
        let schedule = cross_section_schedule_for_presented_coverage(
            schedule,
            presented.progress().completeness(),
            coverage.available_requirements(),
            coverage.total_requirements(),
        );
        if !self.render_coordination.record_cross_section_presentation(
            panel_id.presentation_slot(),
            generation,
            schedule,
        ) {
            anyhow::bail!("stale cross-section frame was suppressed");
        }
        let _ = self.record_product_frame(panel_id, &presented);
        self.record_product_layer_presentations(panel_id, scope);
        self.observe_coordinated_display_milestones(false);
        self.record_current_layout_presentation_if_complete();
        Ok(Some(DisplayRenderTiming {
            path: DisplayRefreshPath::GpuResidentDisplay,
            render_ms: duration_ms(render_start.elapsed()),
            gpu_upload_ms: None,
            gpu_compute_ms: None,
            egui_texture_ms: 0.0,
        }))
    }

    // This is the single render cutover boundary. Its borrowed/scalar inputs
    // make frame ownership visible without constructing a transient request
    // wrapper that the renderer would immediately unpack.
    #[allow(clippy::too_many_arguments)]
    fn render_product_target(
        &mut self,
        target_id: PanelId,
        scope: u64,
        cross_section: Option<PanelId>,
        snapshot: &ApplicationSnapshot,
        presentation: PresentationViewport,
        extent: RenderExtent,
        resources: Arc<[DatasetResourceKey]>,
        lease_priority: Arc<[DatasetResourceKey]>,
        publish_to_display: bool,
    ) -> anyhow::Result<bool> {
        self.ensure_product_target(target_id, extent)?;
        let prepared = self
            .prepared_scope_render_plans
            .get(&scope)
            .cloned()
            .ok_or(WgpuRenderRuntimeError::PreparedStaticLayoutMismatch)?;
        if !Arc::ptr_eq(prepared.requirements.body().canonical(), &resources) {
            return Err(WgpuRenderRuntimeError::PreparedStaticLayoutMismatch.into());
        }
        let target = self
            .native_presentation
            .product_gpu
            .as_ref()
            .and_then(|product| product.targets.get(&target_id))
            .expect("the product target was registered before request construction");
        let current_frame = target
            .request
            .as_ref()
            .map_or(FrameIdentity::new(1), |request| request.intent.frame());
        let existing_presentation = target
            .presented
            .as_ref()
            .map(|frame| frame.progress().completeness());
        let same_requirement_body = Arc::ptr_eq(&target.requirement_keys, &resources);
        let reusable_requirement_binding = requirement_binding_is_reusable(
            same_requirement_body,
            target
                .request
                .as_ref()
                .map(|request| request.requirements.prefetch_promoted()),
            prepared.requirements.prefetch_promoted(),
        );
        let current_intent = target.request.as_ref().map(|request| &request.intent);
        let camera_override = cross_section
            .is_none()
            .then_some(self.camera_preview)
            .flatten();
        let candidate_intent = build_product_intent(
            snapshot,
            current_frame,
            cross_section,
            presentation,
            extent,
            camera_override,
        )?;
        // A transient camera must never replace the last complete texture
        // with an arriving-brick mosaic. Exact-frame-only execution can still
        // hot-rebind and publish immediately when the installed body is fully
        // resident.
        let publish_to_display = publish_to_display && camera_override.is_none();
        let changed = !reusable_requirement_binding || current_intent != candidate_intent.as_ref();
        let priority_changed = !Arc::ptr_eq(&target.lease_priority_keys, &lease_priority);
        if changed {
            let frame = self
                .native_presentation
                .product_gpu
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?
                .allocate_frame_identity();
            let intent = candidate_intent.map(|intent| intent.with_frame(frame));
            let previous = self
                .native_presentation
                .product_gpu
                .as_ref()
                .and_then(|product| product.targets.get(&target_id))
                .and_then(|target| target.request.clone());
            let next = if reusable_requirement_binding {
                match (previous, intent) {
                    (Some(previous), Some(intent)) => Some(Arc::new(previous.rebind(intent)?)),
                    (_, None) => None,
                    (None, Some(intent)) => Some(Arc::new(ProductRenderRequest {
                        requirements: prepared.requirements.bind(&intent)?,
                        intent,
                    })),
                }
            } else {
                intent
                    .map(|intent| {
                        Ok::<_, mirante4d_render_api::RenderApiError>(Arc::new(
                            ProductRenderRequest {
                                requirements: prepared.requirements.bind(&intent)?,
                                intent,
                            },
                        ))
                    })
                    .transpose()?
            };
            let target = self
                .native_presentation
                .product_gpu
                .as_mut()
                .and_then(|product| product.targets.get_mut(&target_id))
                .expect("the product target was registered before request construction");
            if !same_requirement_body {
                target.satisfied_requirement_keys.clear();
                target.next_unsatisfied_requirement = 0;
                target.last_residency_invalidation_epoch = None;
            }
            target.requirement_keys = Arc::clone(&resources);
            target.lease_priority_keys = Arc::clone(&lease_priority);
            target.request = next;
            target.last_renderer_available_resources = 0;
            target.pending_capture = None;
            target.completed_capture = None;
            // A request rebind supersedes the current execution but does not
            // retire the renderer target. Keep tickets for frames that were
            // already published so their exact asynchronous result can still
            // complete publication history. Full-ticket matching prevents an
            // older result from attaching to this newer request.
            target.last_execution_timing = None;
            target.partial_seen = false;
            if !same_requirement_body {
                target.reset_layer_requirement_facts();
            }
            if !same_requirement_body || priority_changed {
                target.dirty_progressive_lease_probe();
            }
        } else if priority_changed {
            let target = self
                .native_presentation
                .product_gpu
                .as_mut()
                .and_then(|product| product.targets.get_mut(&target_id))
                .expect("the product target was registered before priority refresh");
            target.lease_priority_keys = Arc::clone(&lease_priority);
            target.next_unsatisfied_requirement = 0;
            target.dirty_progressive_lease_probe();
        } else if !self.poll_product_target_validation_capture(target_id)? {
            if target_id == PanelId::ThreeD {
                self.render_coordination.request_refresh();
            }
            return Ok(false);
        }
        self.record_product_layer_presentations(target_id, scope);
        let Some(request) = self
            .native_presentation
            .product_gpu
            .as_ref()
            .and_then(|product| product.targets.get(&target_id))
            .and_then(|target| target.request.clone())
        else {
            return Ok(false);
        };
        let render_failure_signature = {
            let product = self
                .native_presentation
                .product_gpu
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
            let target = product
                .targets
                .get(&target_id)
                .expect("the product target was registered");
            ProductRenderFailureSignature::new(
                target.token,
                request.intent.frame(),
                Arc::clone(&resources),
                product.renderer.residency_invalidation_epoch().get(),
                self.dataset.retained_leases().generation(),
                self.viewer_runtime_recovery_epoch,
            )
        };
        let token = render_failure_signature.token;
        let terminal_failure_is_current = self
            .viewer_render_failure_latches
            .get(&token)
            .is_some_and(|latch| {
                latch.blocks(|failure| failure.matches_current(&render_failure_signature))
            });
        if terminal_failure_is_current {
            self.native_presentation
                .product_gpu
                .as_ref()
                .and_then(|product| product.targets.get(&target_id))
                .expect("the product target was registered")
                .set_progressive_lease_probe_state(Default::default());
            return Ok(false);
        }
        // A changed frame, prepared body, residency generation, retained
        // payload cohort, or source/runtime recovery makes the old failure
        // irrelevant.
        self.viewer_render_failure_latches.remove(&token);
        self.note_viewer_render_submission();
        let display_generation = self
            .render_coordination
            .display_generation()
            .input_generation;
        let (dataset, native_presentation, render_failure_latches) = (
            &mut self.dataset,
            &mut self.native_presentation,
            &mut self.viewer_render_failure_latches,
        );
        let product = native_presentation
            .product_gpu
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
        let residency_invalidation_epoch = product.renderer.residency_invalidation_epoch();
        let target = product
            .targets
            .get_mut(&target_id)
            .expect("the product target was registered");
        target.clear_progressive_render_request();
        let lease_work = collect_requirement_lease_updates(
            target.lease_priority_keys.as_ref(),
            |key| *key,
            &mut target.satisfied_requirement_keys,
            &mut target.next_unsatisfied_requirement,
            &mut target.last_residency_invalidation_epoch,
            residency_invalidation_epoch,
            |key| product.renderer.resource_is_resident(key),
            |key| dataset.retained_leases().lease_handle(key),
            |lease| {
                if lease.payload_facts().any_valid() {
                    lease.payload().byte_len()
                } else {
                    0
                }
            },
        );
        let observed_resident_keys = lease_work.observed_resident_keys;
        let removed_satisfied_keys = lease_work.removed_satisfied_keys;
        let selected_lease_keys = lease_work.selected_lease_keys;
        let starting_requirement_cursor = lease_work.starting_requirement_cursor;
        let lease_updates = lease_work.lease_updates;
        let render_policy = retained_frame_render_policy(publish_to_display, existing_presentation);
        let report = match product
            .renderer
            .execute_prepared_retained_frame_with_policy(
                token,
                snapshot.catalog(),
                &request.intent,
                &request.requirements,
                &prepared.static_layout,
                &lease_updates,
                display_generation,
                render_policy,
            ) {
            Ok(report) => report,
            Err(error @ WgpuRenderRuntimeError::StaleFrame { .. }) => {
                let target = product
                    .targets
                    .get_mut(&target_id)
                    .expect("the product target was registered");
                target.next_unsatisfied_requirement = starting_requirement_cursor;
                target.dirty_progressive_lease_probe();
                product.stale_frames_rejected = product.stale_frames_rejected.saturating_add(1);
                tracing::debug!(%error, "stale product frame was rejected");
                return Ok(false);
            }
            Err(error) => {
                let target = product
                    .targets
                    .get_mut(&target_id)
                    .expect("the product target was registered");
                target.next_unsatisfied_requirement = starting_requirement_cursor;
                if product_render_failure_is_deterministic(error) {
                    target.set_progressive_lease_probe_state(Default::default());
                    render_failure_latches.insert(
                        token,
                        DeterministicFailureLatch::new(render_failure_signature),
                    );
                } else {
                    target.dirty_progressive_lease_probe();
                }
                return Err(error.into());
            }
        };
        drop(lease_updates);
        let invalidation_epoch = product.renderer.residency_invalidation_epoch();
        {
            let target = product
                .targets
                .get_mut(&target_id)
                .expect("the product target was registered");
            commit_or_rewind_retained_updates(
                &mut target.satisfied_requirement_keys,
                &mut target.next_unsatisfied_requirement,
                starting_requirement_cursor,
                &selected_lease_keys,
                report.retained_updates_accepted(),
            );
            for key in removed_satisfied_keys.iter().copied() {
                target.mark_layer_requirement_unavailable(key);
            }
            for key in observed_resident_keys.iter().copied() {
                target.mark_layer_requirement_available(key);
            }
            if report.retained_updates_accepted() {
                for key in selected_lease_keys.iter().copied() {
                    target.mark_layer_requirement_available(key);
                }
            }
            for key in report.newly_resident_keys().iter().copied() {
                if target.requirement_keys.binary_search(&key).is_ok()
                    && target.satisfied_requirement_keys.insert(key)
                {
                    target.mark_layer_requirement_available(key);
                }
            }
            if let Some(progress) = report.progress() {
                let coverage = progress.coverage();
                target.last_renderer_available_resources = coverage
                    .available_requirements()
                    .saturating_add(if coverage.prefetch_promoted() {
                        0
                    } else {
                        coverage.available_prefetch()
                    });
            }
        }
        if !report.evicted_keys().is_empty() {
            for target in product
                .targets
                .values_mut()
                .chain(product.staging_3d.iter_mut())
            {
                for key in report.evicted_keys().iter().copied() {
                    if target.satisfied_requirement_keys.contains(&key) {
                        target.mark_layer_requirement_unavailable(key);
                    }
                }
            }
            apply_exact_residency_delta(
                product
                    .targets
                    .values_mut()
                    .map(|target| {
                        (
                            target.requirement_keys.as_ref(),
                            &mut target.satisfied_requirement_keys,
                            &mut target.next_unsatisfied_requirement,
                            &mut target.last_residency_invalidation_epoch,
                        )
                    })
                    .chain(product.staging_3d.iter_mut().map(|target| {
                        (
                            target.requirement_keys.as_ref(),
                            &mut target.satisfied_requirement_keys,
                            &mut target.next_unsatisfied_requirement,
                            &mut target.last_residency_invalidation_epoch,
                        )
                    })),
                invalidation_epoch,
                report.evicted_keys(),
            );
            product.dirty_progressive_lease_probes_for_keys(report.evicted_keys());
        }
        let evicted = dataset.reconcile_exact_gpu_evictions(
            invalidation_epoch.get(),
            report.evicted_keys().iter().copied(),
        );
        let mut retired = 0;
        if target_id == PanelId::ThreeD {
            retired = dataset.retire_gpu_resident_current_payloads(
                observed_resident_keys
                    .iter()
                    .copied()
                    .chain(report.newly_resident_keys().iter().copied()),
                |key| product.renderer.resource_is_resident(key),
            );
        }
        let refill_admission = retired > 0 || evicted > 0;
        if let Some(ticket) = report.gpu_timing() {
            product
                .targets
                .get_mut(&target_id)
                .expect("the product target was registered")
                .pending_gpu_timings
                .push_back(ticket);
        }
        let Some(presented) = report.presentation().cloned() else {
            if refill_admission {
                dataset.defer_interactive_admission_refill(false);
            }
            return Ok(false);
        };
        let execution_timing = Some(ProductFrameExecutionTiming::new(
            token,
            report.frame(),
            display_generation,
            request.intent.view().pass_kind(),
            report.cpu_timing(),
            report.gpu_timing(),
        ));
        product
            .targets
            .get_mut(&target_id)
            .expect("the product target was registered")
            .last_execution_timing = execution_timing;
        let partial_seen = product
            .targets
            .get(&target_id)
            .is_some_and(|target| target.partial_seen);
        let current_is_partial =
            presented.progress().completeness() == RenderFrameCompleteness::Progressive;
        if publish_to_display && current_is_partial && !partial_seen {
            product.current_partial_frames_presented =
                product.current_partial_frames_presented.saturating_add(1);
        }
        if publish_to_display && !current_is_partial && partial_seen {
            product.partial_to_settled_transitions =
                product.partial_to_settled_transitions.saturating_add(1);
        }
        let target = product
            .targets
            .get_mut(&target_id)
            .expect("the product target was registered");
        let extent_changed = target.extent != presented.extent();
        target.extent = presented.extent();
        target.presented = Some(presented.clone());
        target.presented_layer_requirement_facts = target.layer_requirement_facts.clone();
        target.partial_seen = current_is_partial;
        if let Some(ticket) = report.validation_capture() {
            target.pending_capture = Some((presented.clone(), ticket));
            target.completed_capture = None;
        }
        self.bind_product_texture(target_id, extent_changed)?;
        if publish_to_display && target_id == PanelId::ThreeD {
            let _ = self.record_product_frame(target_id, &presented);
            self.record_product_layer_presentations(target_id, scope);
            self.observe_coordinated_display_milestones(false);
            self.record_current_layout_presentation_if_complete();
        }
        // Renderer execution has already committed this texture and frame.
        // Publish the matching application metadata before the unrelated,
        // fallible queue refill so a refill fault cannot strand or invalidate
        // a frame that the renderer will treat as already rendered on retry.
        // The normal dataset-service path observes admission_blocked on the
        // next turn, performs the refill outside this presentation transaction,
        // and reports any dataset fault through its own state machine.
        if refill_admission {
            self.dataset.defer_interactive_admission_refill(true);
        }
        Ok(true)
    }

    fn ensure_product_target(
        &mut self,
        target_id: PanelId,
        extent: RenderExtent,
    ) -> anyhow::Result<()> {
        let product = self
            .native_presentation
            .product_gpu
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
        if product.targets.contains_key(&target_id) {
            return Ok(());
        }
        let registration = product.renderer.register_presentation(extent)?;
        product.targets.insert(
            target_id,
            ProductPresentationTarget::new(registration.token(), extent),
        );
        Ok(())
    }

    fn ensure_staging_3d_target(&mut self, extent: RenderExtent) -> anyhow::Result<()> {
        let product = self
            .native_presentation
            .product_gpu
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
        if product.staging_3d.is_none() {
            let registration = product.renderer.register_presentation(extent)?;
            product.staging_3d = Some(ProductPresentationTarget::new(registration.token(), extent));
        }
        let visible_token = product
            .targets
            .get(&PanelId::ThreeD)
            .ok_or_else(|| anyhow::anyhow!("the visible 3D target is unavailable"))?
            .token;
        // Deactivation releases residency pins, controls and pending uploads,
        // but leaves the immutable display texture registered and paintable.
        // The hidden token can therefore own the active target cohort without
        // replacing the last complete visible pixels.
        product.renderer.deactivate_presentation(visible_token)?;
        let visible = product
            .targets
            .get_mut(&PanelId::ThreeD)
            .expect("the deactivated visible target remains registered");
        // Renderer deactivation terminally discards asynchronous work for the
        // token. Preserve any completed capture of the still-paintable pixels,
        // but do not leave an application ticket that the renderer no longer
        // owns for automation/background polling.
        visible.pending_capture = None;
        visible.pending_gpu_timings.clear();
        Ok(())
    }

    fn render_staged_current_product_frame(
        &mut self,
    ) -> anyhow::Result<StagedCurrentProductFrameResult> {
        let snapshot = self.application.snapshot();
        let requirements = self
            .dataset
            .scope_requirement_handle(SCOPE_CURRENT_3D_REFINEMENT);
        let lease_priority = self
            .dataset
            .scope_gpu_priority_handle(SCOPE_CURRENT_3D_REFINEMENT);
        let presentation = self.render_coordination.presentation_viewport;
        let extent = self.render_coordination.render_viewport;
        self.ensure_staging_3d_target(extent)?;

        let visible_target = {
            let product = self
                .native_presentation
                .product_gpu
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
            let visible = product
                .targets
                .remove(&PanelId::ThreeD)
                .ok_or_else(|| anyhow::anyhow!("the visible 3D target is unavailable"))?;
            let staging = product
                .staging_3d
                .take()
                .ok_or_else(|| anyhow::anyhow!("the hidden 3D target is unavailable"))?;
            product.targets.insert(PanelId::ThreeD, staging);
            visible
        };
        let render_result = self.render_product_target(
            PanelId::ThreeD,
            SCOPE_CURRENT_3D_REFINEMENT,
            None,
            &snapshot,
            presentation,
            extent,
            requirements,
            lease_priority,
            false,
        );
        {
            let product = self
                .native_presentation
                .product_gpu
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
            let staging = product
                .targets
                .remove(&PanelId::ThreeD)
                .ok_or_else(|| anyhow::anyhow!("the hidden 3D target disappeared"))?;
            product.targets.insert(PanelId::ThreeD, visible_target);
            product.staging_3d = Some(staging);
        }
        let rendered = render_result?;
        let target_complete = self
            .native_presentation
            .product_gpu
            .as_ref()
            .and_then(|product| product.staging_3d.as_ref())
            .and_then(|target| target.presented.as_ref().zip(target.request.as_ref()))
            .is_some_and(|(frame, request)| {
                hidden_target_ready_for_atomic_swap(
                    frame.frame(),
                    frame.extent(),
                    request.intent.frame(),
                    request.intent.extent(),
                    frame.progress().completeness(),
                    frame.progress().coverage().total_requirements(),
                    self.dataset
                        .scope_required_prefix_len(SCOPE_CURRENT_3D_REFINEMENT),
                )
            });
        if !target_complete {
            return Ok(StagedCurrentProductFrameResult {
                rendered,
                published_gpu_timing: None,
            });
        }

        let mut scope_targets = ScopeReconciliationTargets::default();
        if !self
            .dataset
            .prepare_staged_current_promotion_scope_targets(&mut scope_targets)
        {
            anyhow::bail!("the complete hidden target has no staged dataset plan");
        }
        let post_promotion_update = self
            .staged_post_promotion_renderer_update
            .clone()
            .ok_or_else(|| {
                anyhow::anyhow!("the complete hidden target has no worker-prepared promotion union")
            })?;
        self.dataset
            .preflight_prepared_renderer_requirement_update(
                &post_promotion_update.previous.requirements,
                &post_promotion_update.next.requirements,
            )?;
        let prepared_scope_reconciliation =
            self.dataset.prepare_scope_reconciliation(scope_targets)?;
        let (old_visible_token, presented) = {
            let product = self
                .native_presentation
                .product_gpu
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
            let old_visible_token = product
                .targets
                .get(&PanelId::ThreeD)
                .ok_or_else(|| anyhow::anyhow!("the visible 3D target is unavailable"))?
                .token;
            let presented = product
                .staging_3d
                .as_ref()
                .and_then(|target| target.presented.clone())
                .ok_or_else(|| anyhow::anyhow!("the complete hidden target has no frame"))?;
            (old_visible_token, presented)
        };
        // Registration validation is read-only. Atomic cancel-many remains the
        // final fallible boundary, so either failure leaves renderer, ticket,
        // scope, retained-union, and target ownership unchanged.
        self.native_presentation
            .product_gpu
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?
            .renderer
            .preflight_deactivate_presentation(old_visible_token)?;
        self.dataset
            .commit_prepared_scope_reconciliation(prepared_scope_reconciliation)?;
        {
            let product = self
                .native_presentation
                .product_gpu
                .as_mut()
                .expect("the checked product renderer remains installed");
            product
                .renderer
                .commit_preflighted_deactivate_presentation(old_visible_token);
            let mut old_visible = product
                .targets
                .remove(&PanelId::ThreeD)
                .expect("the checked visible target remains installed");
            old_visible.pending_capture = None;
            old_visible.pending_gpu_timings.clear();
            let new_visible = product
                .staging_3d
                .take()
                .expect("the checked complete hidden target remains installed");
            product.targets.insert(PanelId::ThreeD, new_visible);
            product.staging_3d = Some(old_visible);
        }
        self.dataset
            .commit_reconciled_gpu_prefilled_staged_current_plan();
        let refinement_render_plan = self
            .prepared_scope_render_plans
            .remove(&SCOPE_CURRENT_3D_REFINEMENT)
            .expect("a complete hidden target owns its prepared refinement layout");
        self.prepared_scope_render_plans
            .insert(SCOPE_CURRENT_3D, refinement_render_plan);
        let crate::camera_demand_cache::PreparedRendererRequirementUpdate {
            previous,
            next,
            removals,
            removal_charge: _removal_charge,
        } = post_promotion_update;
        self.dataset.commit_preflighted_renderer_requirement_update(
            previous.requirements,
            next.requirements,
            &removals,
            next.charge,
        );
        self.staged_post_promotion_renderer_update = None;
        let published_gpu_timing = self.record_product_frame(PanelId::ThreeD, &presented);
        self.record_product_layer_presentations(PanelId::ThreeD, SCOPE_CURRENT_3D);
        self.observe_coordinated_display_milestones(false);
        self.record_current_layout_presentation_if_complete();
        Ok(StagedCurrentProductFrameResult {
            rendered: true,
            published_gpu_timing,
        })
    }

    /// Polls renderer timestamp readbacks without waiting on the UI thread.
    /// Completed values are bound to the exact presented frame that produced
    /// them, so CPU submission time is never mislabeled as GPU work.
    pub(crate) fn poll_product_gpu_timings(&mut self) -> anyhow::Result<()> {
        let pending = self
            .native_presentation
            .product_gpu
            .as_ref()
            .map(|product| {
                product
                    .targets
                    .iter()
                    .flat_map(|(panel, target)| {
                        target
                            .pending_gpu_timings
                            .iter()
                            .copied()
                            .map(|ticket| (Some(*panel), ticket))
                    })
                    .chain(product.staging_3d.iter().flat_map(|target| {
                        target
                            .pending_gpu_timings
                            .iter()
                            .copied()
                            .map(|ticket| (None, ticket))
                    }))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut first_error = None;
        for (panel, ticket) in pending {
            let product = self
                .native_presentation
                .product_gpu
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
            let polled = product.renderer.poll_gpu_timing(ticket);
            if let Err(error) = polled {
                // Unknown/failed timing tickets are terminal. Retaining one
                // here would keep background repainting forever and prevent
                // every later ticket from being observed.
                let target = match panel {
                    Some(panel) => product
                        .targets
                        .get_mut(&panel)
                        .expect("a pending GPU timing belongs to a registered target"),
                    None => product
                        .staging_3d
                        .as_mut()
                        .expect("a pending GPU timing belongs to the hidden 3D target"),
                };
                target
                    .pending_gpu_timings
                    .retain(|pending| *pending != ticket);
                if first_error.is_none() {
                    first_error = Some(error);
                }
                continue;
            }
            let Some(timing) = polled.expect("the error case continued above") else {
                continue;
            };
            {
                let target = match panel {
                    Some(panel) => product
                        .targets
                        .get_mut(&panel)
                        .expect("a pending GPU timing belongs to a registered target"),
                    None => product
                        .staging_3d
                        .as_mut()
                        .expect("a pending GPU timing belongs to the hidden 3D target"),
                };
                target
                    .pending_gpu_timings
                    .retain(|pending| *pending != ticket);
                if let Some(execution) = target.last_execution_timing.as_mut() {
                    execution.complete_gpu(ticket, timing);
                }
            }
            let completed_latest_publication =
                product.complete_presented_frame_gpu_timing(ticket, timing);
            if panel == Some(PanelId::ThreeD)
                && completed_latest_publication
                && let Some(refresh) = self
                    .render_coordination
                    .last_display_refresh_timing
                    .as_mut()
            {
                refresh.gpu_upload_ms = timing.payload_copy_ns().map(nanoseconds_ms);
                refresh.gpu_compute_ms = timing.render_pass_ns().map(nanoseconds_ms);
            }
        }
        match first_error {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
    }

    pub(crate) fn poll_product_validation_captures(&mut self) -> anyhow::Result<()> {
        let pending = self
            .native_presentation
            .product_gpu
            .as_ref()
            .map(|product| {
                product
                    .targets
                    .iter()
                    .filter_map(|(panel, target)| {
                        target.pending_capture.as_ref().map(|_| Some(*panel))
                    })
                    .chain(
                        product
                            .staging_3d
                            .iter()
                            .filter_map(|target| target.pending_capture.as_ref().map(|_| None)),
                    )
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for panel in pending {
            match panel {
                Some(panel) => {
                    self.poll_product_target_validation_capture(panel)?;
                }
                None => {
                    self.poll_staging_3d_validation_capture()?;
                }
            }
        }
        Ok(())
    }

    fn poll_staging_3d_validation_capture(&mut self) -> anyhow::Result<bool> {
        let pending = self
            .native_presentation
            .product_gpu
            .as_ref()
            .and_then(|product| product.staging_3d.as_ref())
            .and_then(|target| target.pending_capture.clone());
        let Some((presentation, ticket)) = pending else {
            return Ok(true);
        };
        let product = self
            .native_presentation
            .product_gpu
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
        let Some(capture) = product.renderer.poll_validation_capture(ticket)? else {
            return Ok(false);
        };
        let target = product
            .staging_3d
            .as_mut()
            .expect("a pending capture belongs to the hidden 3D target");
        target.pending_capture = None;
        if target.presented.as_ref() == Some(&presentation) {
            target.completed_capture = Some((presentation, capture));
        }
        Ok(true)
    }

    fn poll_product_target_validation_capture(&mut self, panel: PanelId) -> anyhow::Result<bool> {
        let pending = self
            .native_presentation
            .product_gpu
            .as_ref()
            .and_then(|product| product.targets.get(&panel))
            .and_then(|target| target.pending_capture.clone());
        let Some((presentation, ticket)) = pending else {
            return Ok(true);
        };
        let product = self
            .native_presentation
            .product_gpu
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
        let Some(capture) = product.renderer.poll_validation_capture(ticket)? else {
            return Ok(false);
        };
        let target = product
            .targets
            .get_mut(&panel)
            .expect("a pending capture belongs to a registered target");
        target.pending_capture = None;
        if target.presented.as_ref() == Some(&presentation) {
            target.completed_capture = Some((presentation, capture));
        }
        Ok(true)
    }

    fn bind_product_texture(
        &mut self,
        target_id: PanelId,
        extent_changed: bool,
    ) -> anyhow::Result<()> {
        let product = self
            .native_presentation
            .product_gpu
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
        let target = product
            .targets
            .get(&target_id)
            .ok_or_else(|| anyhow::anyhow!("product presentation target is unavailable"))?;
        let token = target.token;
        let view = product.renderer.presentation_texture_view(token)?.clone();
        self.native_presentation
            .bind_texture(token, &view, extent_changed)?;
        Ok(())
    }

    fn record_product_frame(
        &mut self,
        panel_id: PanelId,
        frame: &mirante4d_render_api::PresentedFrame,
    ) -> Option<GpuFrameTiming> {
        let published_gpu_timing = self
            .native_presentation
            .product_gpu
            .as_mut()
            .filter(|product| product.presented_frame_interval_timing_enabled())
            .and_then(|product| {
                // Keep the disabled product path ahead of the target lookup and
                // clock. Progressive submissions can reuse FrameIdentity, so
                // the GPU side is additionally bound by its execution ticket.
                let execution = product
                    .targets
                    .get(&panel_id)
                    .and_then(|target| target.last_execution_timing)
                    .filter(|timing| {
                        timing.frame == frame.frame()
                            && timing.gpu_ticket.is_none_or(|ticket| {
                                ticket.display_generation() == timing.display_generation
                            })
                    });
                let cpu_timing = execution.and_then(|timing| timing.cpu);
                let gpu_execution = execution.and_then(|timing| {
                    timing
                        .gpu_ticket
                        .map(ProductGpuExecutionIdentity::from_ticket)
                });
                let gpu_timing = execution.and_then(|timing| timing.gpu);
                product.record_presented_frame_interval(
                    panel_id,
                    frame.frame(),
                    cpu_timing,
                    gpu_execution,
                    gpu_timing.map(ProductGpuExecutionTiming::from),
                );
                gpu_timing
            });
        if panel_id != PanelId::ThreeD {
            return published_gpu_timing;
        }
        let progress = frame.progress();
        let target_settled = !self.dataset.staging_current_refinement()
            && self.dataset.current_scale().get()
                == self.render_coordination.frame_fidelity.target_scale_level;
        self.display_performance_milestones.observe_three_d(
            frame,
            self.dataset.current_scale().get(),
            self.render_coordination.frame_fidelity.target_scale_level,
            target_settled,
        );
        let coverage = progress.coverage();
        self.render_coordination.frame_fidelity.resident_bricks =
            usize::try_from(coverage.available_requirements()).unwrap_or(usize::MAX);
        self.render_coordination
            .frame_fidelity
            .missing_occupied_bricks = usize::try_from(
            coverage
                .total_requirements()
                .saturating_sub(coverage.available_requirements()),
        )
        .unwrap_or(usize::MAX);
        self.render_coordination.frame_fidelity.completeness = match progress.completeness() {
            RenderFrameCompleteness::Progressive => FrameCompleteness::Incomplete,
            RenderFrameCompleteness::Complete => FrameCompleteness::Complete,
            RenderFrameCompleteness::Exact => {
                if self.dataset.current_scale().get() == 0 {
                    FrameCompleteness::Exact
                } else {
                    FrameCompleteness::Complete
                }
            }
        };
        self.render_coordination.frame_fidelity.reason = match progress.limitation() {
            Some(FrameLimitation::BudgetLimited | FrameLimitation::CapacityLimited) => {
                LodDecisionReason::GpuBudgetLimited
            }
            Some(FrameLimitation::CoarserScale) => LodDecisionReason::ScreenEquivalentCoarserScale,
            Some(FrameLimitation::MissingResources) => LodDecisionReason::IncompleteResidency,
            None if self.dataset.current_playback_downshifted() => {
                LodDecisionReason::PlaybackDownshift
            }
            None if self.dataset.current_scale().get() == 0 => LodDecisionReason::ExactS0,
            None => LodDecisionReason::ScreenEquivalentCoarserScale,
        };
        let snapshot = self.application.snapshot();
        let view = application_view(&snapshot);
        let mode = view
            .layer(view.active_layer())
            .expect("the current view contains its active layer")
            .render_state()
            .mode();
        self.render_coordination.frame_fidelity.backend = render_backend_for_mode(mode);
        self.render_coordination.frame_fidelity.display_freshness =
            DisplayedFrameFreshness::Current;
        self.render_coordination
            .frame_fidelity
            .displayed_scale_level = Some(self.dataset.current_scale().get());
        published_gpu_timing
    }

    fn record_product_layer_presentations(&mut self, target_id: PanelId, scope: u64) {
        let expected_scope = if target_id == PanelId::ThreeD
            && self.dataset.staging_current_refinement()
            && self
                .dataset
                .scope_layer_scales(SCOPE_CURRENT_3D_REFINEMENT)
                .is_some()
        {
            SCOPE_CURRENT_3D_REFINEMENT
        } else {
            scope
        };
        let expected = self.dataset.scope_layer_scales(expected_scope);
        let Some(target) = self
            .native_presentation
            .product_gpu
            .as_ref()
            .and_then(|product| product.targets.get(&target_id))
        else {
            return;
        };
        let current = if target_id == PanelId::ThreeD {
            self.render_coordination.frame_fidelity.display_freshness
                == DisplayedFrameFreshness::Current
        } else {
            self.render_coordination
                .surface(target_id.presentation_slot())
                .display_current()
        };
        let layers = expected
            .into_iter()
            .flat_map(|scales| scales.keys())
            .chain(target.presented_layer_requirement_facts.keys())
            .copied()
            .collect::<BTreeSet<_>>();
        let presentations = layers
            .into_iter()
            .map(|layer| {
                let expected_scale_level = expected
                    .and_then(|scales| scales.get(&layer))
                    .map(|scale| scale.get());
                let facts = target
                    .presented_layer_requirement_facts
                    .get(&layer)
                    .copied();
                product_layer_presentation_status(
                    layer.ordinal(),
                    expected_scale_level,
                    facts,
                    current,
                )
            })
            .collect::<Vec<_>>();
        if let Err(overflow) = self
            .render_coordination
            .set_layer_presentations(target_id.presentation_slot(), presentations)
        {
            tracing::error!(
                panel = target_id.label(),
                actual = overflow.actual,
                maximum = overflow.maximum,
                "layer presentation instrumentation exceeded the renderer layer bound"
            );
        }
    }

    pub(crate) fn render_current_product_frame(&mut self) -> anyhow::Result<DisplayRenderTiming> {
        let snapshot = self.application.snapshot();
        let requirements = self.dataset.scope_requirement_handle(SCOPE_CURRENT_3D);
        let lease_priority = self.dataset.scope_gpu_priority_handle(SCOPE_CURRENT_3D);
        let presentation = self.render_coordination.presentation_viewport;
        let extent = self.render_coordination.render_viewport;
        let started = Instant::now();
        let rendered = self.render_product_target(
            PanelId::ThreeD,
            SCOPE_CURRENT_3D,
            None,
            &snapshot,
            presentation,
            extent,
            requirements,
            lease_priority,
            true,
        )?;
        let displayed = self.product_display(
            &self.application_snapshot_for_ui(),
            PresentationSlot::ThreeD,
        );
        Ok(DisplayRenderTiming {
            path: if rendered || displayed.is_some() {
                DisplayRefreshPath::GpuResidentDisplay
            } else {
                DisplayRefreshPath::UiBackground
            },
            render_ms: duration_ms(started.elapsed()),
            gpu_upload_ms: None,
            gpu_compute_ms: None,
            egui_texture_ms: 0.0,
        })
    }

    pub(crate) fn rerender_display_state(&mut self) -> anyhow::Result<DisplayRefreshWorkTiming> {
        let demand_started = Instant::now();
        self.request_visible_bricks();
        let visible_brick_request_ms = duration_ms(demand_started.elapsed());
        if !self.visible_demand_plan_currentness().current_3d {
            return Ok(DisplayRefreshWorkTiming::new(
                DisplayRenderTiming {
                    path: DisplayRefreshPath::UiBackground,
                    render_ms: 0.0,
                    gpu_upload_ms: None,
                    gpu_compute_ms: None,
                    egui_texture_ms: 0.0,
                },
                visible_brick_request_ms,
            ));
        }
        let complete_presentation_retained = self
            .native_presentation
            .product_gpu
            .as_ref()
            .and_then(|product| product.targets.get(&PanelId::ThreeD))
            .and_then(|target| target.presented.as_ref())
            .is_some_and(|frame| {
                frame.progress().completeness() != RenderFrameCompleteness::Progressive
            });
        if self.dataset.staging_current_refinement()
            && (self.dataset.holding_previous_presentation() || complete_presentation_retained)
        {
            // Stream and render into a hidden token. The visible target stays
            // immutable until the hidden frame proves exact GPU coverage; the
            // linked panels continue through their independent dirty/render
            // path instead of being gated by 3D refinement.
            let render_started = Instant::now();
            #[cfg(test)]
            {
                self.product_render_attempts = self.product_render_attempts.saturating_add(1);
            }
            let outcome = self.render_staged_current_product_frame()?;
            return Ok(DisplayRefreshWorkTiming::new(
                DisplayRenderTiming {
                    path: if outcome.rendered {
                        DisplayRefreshPath::GpuResidentDisplay
                    } else {
                        DisplayRefreshPath::UiBackground
                    },
                    render_ms: duration_ms(render_started.elapsed()),
                    gpu_upload_ms: outcome
                        .published_gpu_timing
                        .and_then(GpuFrameTiming::payload_copy_ns)
                        .map(nanoseconds_ms),
                    gpu_compute_ms: outcome
                        .published_gpu_timing
                        .and_then(GpuFrameTiming::render_pass_ns)
                        .map(nanoseconds_ms),
                    egui_texture_ms: 0.0,
                },
                visible_brick_request_ms,
            ));
        }
        if self.dataset.scope_is_empty(SCOPE_CURRENT_3D) {
            self.clear_3d_product_presentation();
            self.render_coordination.frame_fidelity.backend = RenderBackend::Empty;
            self.render_coordination.frame_fidelity.completeness = FrameCompleteness::Complete;
            self.render_coordination.frame_fidelity.reason = LodDecisionReason::NoVisibleData;
            self.render_coordination.frame_fidelity.display_freshness =
                DisplayedFrameFreshness::Current;
            return Ok(DisplayRefreshWorkTiming::new(
                DisplayRenderTiming {
                    path: DisplayRefreshPath::UiBackground,
                    render_ms: 0.0,
                    gpu_upload_ms: None,
                    gpu_compute_ms: None,
                    egui_texture_ms: 0.0,
                },
                visible_brick_request_ms,
            ));
        }
        #[cfg(test)]
        {
            self.product_render_attempts = self.product_render_attempts.saturating_add(1);
        }
        self.render_current_product_frame()
            .map(|render| DisplayRefreshWorkTiming::new(render, visible_brick_request_ms))
    }

    pub(crate) fn record_display_refresh_timing(
        &mut self,
        render: DisplayRenderTiming,
        visible_brick_request_ms: f64,
        total_ms: f64,
    ) {
        self.render_coordination.last_display_refresh_timing = Some(DisplayRefreshTiming {
            path: render.path,
            render_ms: render.render_ms,
            gpu_upload_ms: render.gpu_upload_ms,
            gpu_compute_ms: render.gpu_compute_ms,
            egui_texture_ms: render.egui_texture_ms,
            visible_brick_request_ms,
            total_ms,
        });
    }

    pub(crate) fn refresh_frame(&mut self) {
        let total_start = Instant::now();
        let completed_work = match self.rerender_display_state() {
            Ok(work) => Some(work),
            Err(error) => {
                tracing::error!(%error, "GPU display refresh failed");
                let failure = render_state::render_failure_status(&error);
                self.render_coordination.frame_fidelity.last_failure_kind = Some(failure.kind());
                self.render_coordination.frame_fidelity.last_capacity_error =
                    Some(failure.message().to_owned());
                self.render_coordination.frame_fidelity.completeness =
                    FrameCompleteness::Incomplete;
                None
            }
        };
        if application_view(&self.application.snapshot()).layout()
            == CanonicalViewerLayout::FourPanel
        {
            // A global 3D refresh request suppresses the UI's per-panel
            // ensure-current callbacks for this turn. Run the three linked
            // scopes here as independent dirty decisions so hidden 3D prefill
            // can never starve them across repeated upload bursts.
            for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
                if let Err(error) = self.render_cross_section_panel_for_display_if_needed(panel) {
                    tracing::error!(%error, panel = panel.label(), "linked panel refresh failed");
                }
            }
        }
        if let Some(work) = completed_work {
            // `render_ms` remains the 3D renderer call. The total is the whole
            // display refresh, including linked FourPanel targets.
            self.record_display_refresh_timing(
                work.render,
                work.visible_brick_request_ms,
                duration_ms(total_start.elapsed()),
            );
        }
    }

    pub(crate) fn refresh_texture_only(&mut self) {
        self.invalidate_cross_section_panel_display_frames();
        self.refresh_frame();
    }
}

fn nanoseconds_ms(nanoseconds: u64) -> f64 {
    nanoseconds as f64 / 1_000_000.0
}

fn build_product_intent(
    snapshot: &ApplicationSnapshot,
    frame: FrameIdentity,
    cross_section: Option<PanelId>,
    presentation: PresentationViewport,
    extent: RenderExtent,
    camera_override: Option<CameraView>,
) -> anyhow::Result<Option<mirante4d_render_api::RenderIntent>> {
    match cross_section {
        Some(panel) => cross_section_intent(snapshot, frame, panel, presentation, extent),
        None => volume_intent(snapshot, frame, presentation, extent, camera_override),
    }
}

fn product_layer_presentation_status(
    layer_ordinal: u32,
    expected_scale_level: Option<u32>,
    presented_facts: Option<ProductLayerRequirementFacts>,
    panel_current: bool,
) -> mirante4d_application::LayerPresentationStatus {
    let displayed_scale_level = presented_facts.and_then(|facts| facts.displayed_scale_level);
    mirante4d_application::LayerPresentationStatus {
        layer_ordinal,
        expected_scale_level,
        displayed_scale_level,
        available_requirements: presented_facts.map_or(0, |facts| facts.available_requirements),
        total_requirements: presented_facts.map_or(0, |facts| facts.total_requirements),
        current: panel_current && expected_scale_level == displayed_scale_level,
    }
}

fn cross_section_scope(panel_id: PanelId) -> anyhow::Result<u64> {
    match panel_id {
        PanelId::Xy => Ok(SCOPE_CROSS_SECTION_XY),
        PanelId::Xz => Ok(SCOPE_CROSS_SECTION_XZ),
        PanelId::Yz => Ok(SCOPE_CROSS_SECTION_YZ),
        PanelId::ThreeD => anyhow::bail!("the 3D panel has no cross-section demand scope"),
    }
}

pub(crate) fn render_backend_for_mode(mode: RenderMode) -> RenderBackend {
    match mode {
        RenderMode::Mip => RenderBackend::GpuCameraMip,
        RenderMode::Isosurface => RenderBackend::GpuCameraIso,
        RenderMode::Dvr => RenderBackend::GpuCameraDvr,
    }
}

#[cfg(test)]
mod requirement_lease_update_tests {
    use std::{cell::Cell, collections::HashSet};

    use mirante4d_dataset::{DatasetResourceIdentity, DatasetSourceId, ResourceRegion};
    use mirante4d_domain::{LogicalLayerKey, ScaleLevel, Shape3D, TimeIndex};

    use super::*;

    #[test]
    fn layer_evidence_uses_the_presented_resource_scale() {
        let status = product_layer_presentation_status(
            7,
            Some(1),
            Some(ProductLayerRequirementFacts {
                displayed_scale_level: Some(0),
                available_requirements: 3,
                total_requirements: 4,
            }),
            true,
        );
        assert_eq!(status.layer_ordinal, 7);
        assert_eq!(status.expected_scale_level, Some(1));
        assert_eq!(status.displayed_scale_level, Some(0));
        assert_eq!(status.available_requirements, 3);
        assert_eq!(status.total_requirements, 4);
        assert!(!status.current);
    }

    #[test]
    fn coordinated_milestones_require_the_visible_layout_and_foreground_idle() {
        let mut milestones = DisplayPerformanceMilestones::default();
        milestones.begin_generation(1);

        milestones.observe_coordinated_layout(true, true, true, false, false);
        assert!(milestones.first_useful_frame_ms().is_some());
        assert!(milestones.complete_coarse_ms().is_some());
        assert!(milestones.complete_replacement_ms().is_some());
        assert!(milestones.first_current_presented_ms().is_none());
        assert!(milestones.target_settled_ms().is_none());

        milestones.observe_coordinated_layout(true, true, false, true, false);
        assert!(milestones.first_current_presented_ms().is_some());
        assert!(milestones.target_settled_ms().is_none());

        milestones.observe_coordinated_layout(true, true, false, true, true);
        assert!(milestones.target_settled_ms().is_some());

        let exact_idle = PerformanceMilestoneObservation {
            first_useful: true,
            complete_replacement: true,
            complete_coarse: true,
            target_current: true,
            foreground_idle: true,
        };
        milestones.observe_panel(PresentationSlot::Xy, exact_idle);
        milestones.observe_visible_layer(PresentationSlot::Xy, 17, exact_idle);
        let xy = milestones.panel(PresentationSlot::Xy);
        assert!(xy.first_useful_frame_ms().is_some());
        assert!(xy.target_settled_ms().is_some());
        let layer = xy
            .visible_layers()
            .iter()
            .find(|layer| layer.layer_ordinal() == Some(17))
            .expect("the visible layer has generation-bound milestones");
        assert!(layer.complete_coarse_ms().is_some());
        assert!(layer.target_settled_ms().is_some());
    }

    fn key(x: u64) -> DatasetResourceKey {
        DatasetResourceKey::new(
            DatasetResourceIdentity::Unverified(DatasetSourceId::new(1)),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([x, 0, 0], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
        )
    }

    fn render_failure_signature(
        requirement_body: Arc<[DatasetResourceKey]>,
        frame: u64,
        residency_epoch: u64,
        retained_lease_generation: u64,
        runtime_recovery_epoch: u64,
    ) -> ProductRenderFailureSignature {
        ProductRenderFailureSignature::new(
            PresentationToken::new(1).unwrap(),
            FrameIdentity::new(frame),
            requirement_body,
            residency_epoch,
            retained_lease_generation,
            runtime_recovery_epoch,
        )
    }

    #[test]
    fn terminal_positive_lease_failure_executes_once_until_an_exact_input_changes() {
        use mirante4d_render_api::GpuLedgerCategory;

        let body: Arc<[DatasetResourceKey]> = Arc::from([key(0)]);
        let current = render_failure_signature(Arc::clone(&body), 7, 11, 13, 17);
        let terminal_errors = [
            WgpuRenderRuntimeError::BackendValidation,
            WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::PayloadResidency,
                requested_bytes: 2,
                available_bytes: 1,
            },
            WgpuRenderRuntimeError::RequirementCapacityExceeded {
                actual: 2,
                maximum: 1,
            },
        ];
        let mut final_latch = None;
        for error in terminal_errors {
            let mut latch = None;
            let mut executions = 0_u64;
            for _ in 0..128 {
                let blocked = latch.as_ref().is_some_and(
                    |latch: &DeterministicFailureLatch<ProductRenderFailureSignature>| {
                        latch.blocks(|failure| failure.matches_current(&current))
                    },
                );
                if blocked {
                    continue;
                }
                executions = executions.saturating_add(1);
                if product_render_failure_is_deterministic(error) {
                    latch = Some(DeterministicFailureLatch::new(current.clone()));
                }
            }
            assert_eq!(executions, 1, "unchanged UI polls must stay idle");
            final_latch = latch;
        }

        // Equal keys in a newly prepared allocation are an intentional retry
        // event; each remaining field is also an independent input epoch.
        let changed = [
            render_failure_signature(Arc::from([key(0)]), 7, 11, 13, 17),
            render_failure_signature(Arc::clone(&body), 8, 11, 13, 17),
            render_failure_signature(Arc::clone(&body), 7, 12, 13, 17),
            render_failure_signature(Arc::clone(&body), 7, 11, 14, 17),
            render_failure_signature(Arc::clone(&body), 7, 11, 13, 18),
        ];
        for signature in changed {
            assert!(
                !final_latch
                    .as_ref()
                    .expect("the first deterministic failure was latched")
                    .blocks(|failure| failure.matches_current(&signature)),
                "every exact input change must permit another execution"
            );
        }
    }

    #[test]
    fn render_capacity_is_latched_but_stale_and_async_backpressure_remain_retryable() {
        use mirante4d_render_api::GpuLedgerCategory;

        assert!(product_render_failure_is_deterministic(
            WgpuRenderRuntimeError::BackendValidation
        ));
        assert!(!product_render_failure_is_deterministic(
            WgpuRenderRuntimeError::StaleFrame {
                actual: FrameIdentity::new(1),
                current: FrameIdentity::new(2),
            }
        ));
        assert!(product_render_failure_is_deterministic(
            WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::PayloadResidency,
                requested_bytes: 2,
                available_bytes: 1,
            }
        ));
        assert!(product_render_failure_is_deterministic(
            WgpuRenderRuntimeError::RequirementCapacityExceeded {
                actual: 2,
                maximum: 1,
            }
        ));
        assert!(product_render_failure_is_deterministic(
            WgpuRenderRuntimeError::ControlCapacityExceeded
        ));
        assert!(!product_render_failure_is_deterministic(
            WgpuRenderRuntimeError::PickBackpressure
        ));
    }

    #[test]
    fn hidden_target_waits_across_multiple_upload_bursts_then_swaps_once_exact() {
        let request_frame = FrameIdentity::new(2);
        let request_extent = RenderExtent::new(640, 480).expect("test extent");
        let required_primary_count = 1_024;
        assert!(!hidden_target_ready_for_atomic_swap(
            request_frame,
            request_extent,
            request_frame,
            request_extent,
            RenderFrameCompleteness::Progressive,
            512,
            required_primary_count,
        ));
        assert!(!hidden_target_ready_for_atomic_swap(
            request_frame,
            request_extent,
            request_frame,
            request_extent,
            RenderFrameCompleteness::Progressive,
            1_024,
            required_primary_count,
        ));
        assert!(hidden_target_ready_for_atomic_swap(
            request_frame,
            request_extent,
            request_frame,
            request_extent,
            RenderFrameCompleteness::Exact,
            1_024,
            required_primary_count,
        ));

        // A recycled hidden target may still hold an exact frame from the
        // preceding request. It is promotable only when both request identity
        // and extent still describe those completed pixels.
        assert!(!hidden_target_ready_for_atomic_swap(
            FrameIdentity::new(1),
            request_extent,
            request_frame,
            request_extent,
            RenderFrameCompleteness::Exact,
            1_024,
            required_primary_count,
        ));
        assert!(!hidden_target_ready_for_atomic_swap(
            request_frame,
            RenderExtent::new(320, 240).expect("stale test extent"),
            request_frame,
            request_extent,
            RenderFrameCompleteness::Exact,
            1_024,
            required_primary_count,
        ));
        assert!(hidden_target_ready_for_atomic_swap(
            request_frame,
            request_extent,
            request_frame,
            request_extent,
            RenderFrameCompleteness::Exact,
            1_024,
            required_primary_count,
        ));

        // A contained move promotes the 512-key guard. Exactness must then be
        // proved against the enlarged scalar boundary before atomic publish.
        let promoted_requirement_count = 1_536;
        assert!(!hidden_target_ready_for_atomic_swap(
            request_frame,
            request_extent,
            request_frame,
            request_extent,
            RenderFrameCompleteness::Exact,
            1_024,
            promoted_requirement_count,
        ));
        assert!(hidden_target_ready_for_atomic_swap(
            request_frame,
            request_extent,
            request_frame,
            request_extent,
            RenderFrameCompleteness::Exact,
            1_536,
            promoted_requirement_count,
        ));
    }

    #[test]
    fn visible_target_retains_settled_pixels_until_exact_replacement() {
        for settled in [
            RenderFrameCompleteness::Complete,
            RenderFrameCompleteness::Exact,
        ] {
            assert_eq!(
                retained_frame_render_policy(true, Some(settled)),
                RetainedFrameRenderPolicy::ExactFrameOnly,
                "linked panels and visible 3D retain settled pixels on every refresh until replacement"
            );
        }
        assert_eq!(
            retained_frame_render_policy(false, None),
            RetainedFrameRenderPolicy::ExactFrameOnly,
            "hidden and transient targets remain exact-only"
        );
    }

    #[test]
    fn initial_and_already_progressive_targets_can_publish_useful_progress() {
        assert_eq!(
            retained_frame_render_policy(true, None),
            RetainedFrameRenderPolicy::EveryUsefulFrame
        );
        assert_eq!(
            retained_frame_render_policy(true, Some(RenderFrameCompleteness::Progressive),),
            RetainedFrameRenderPolicy::EveryUsefulFrame
        );
    }

    #[test]
    fn cross_section_currentness_uses_presented_gpu_coverage() {
        let cpu_ready = CrossSectionPanelScheduleState {
            generation: 7,
            target_scale_level: Some(0),
            render_scale_level: Some(0),
            fallback_scale_level: None,
            selected_bricks: 4,
            occupied_selected_bricks: 4,
            missing_occupied_bricks: 0,
            estimated_decoded_bytes: 0,
            decoded_budget_bytes: 0,
            status: CrossSectionPanelScheduleStatus::Ready,
            reason: CrossSectionPanelScheduleReason::TargetScaleReady,
        };

        let progressive = cross_section_schedule_for_presented_coverage(
            cpu_ready,
            RenderFrameCompleteness::Progressive,
            2,
            4,
        )
        .rendered();
        assert_eq!(
            progressive.status,
            CrossSectionPanelScheduleStatus::Incomplete
        );
        assert_eq!(progressive.occupied_selected_bricks, 2);
        assert_eq!(progressive.missing_occupied_bricks, 2);

        let defensive_progressive = cross_section_schedule_for_presented_coverage(
            cpu_ready,
            RenderFrameCompleteness::Progressive,
            4,
            4,
        )
        .rendered();
        assert_eq!(
            defensive_progressive.status,
            CrossSectionPanelScheduleStatus::Incomplete,
            "progressive metadata is never promoted to Current from counters alone"
        );

        let exact = cross_section_schedule_for_presented_coverage(
            cpu_ready,
            RenderFrameCompleteness::Exact,
            4,
            4,
        )
        .rendered();
        assert_eq!(exact.status, CrossSectionPanelScheduleStatus::Current);
        assert_eq!(exact.occupied_selected_bricks, 4);
        assert_eq!(exact.missing_occupied_bricks, 0);
    }

    #[test]
    fn cross_section_render_waits_for_its_matching_demand_body() {
        assert!(!cross_section_render_allowed(false, true));
        assert!(cross_section_render_allowed(true, true));
        assert!(!cross_section_render_allowed(true, false));
    }

    #[test]
    fn prefetch_promotion_rebinds_prepared_semantics_on_the_same_body() {
        assert!(requirement_binding_is_reusable(true, Some(false), false));
        assert!(requirement_binding_is_reusable(true, Some(true), true));
        assert!(requirement_binding_is_reusable(true, None, true));
        assert!(
            !requirement_binding_is_reusable(true, Some(false), true),
            "promotion must bind the prepared wrapper instead of rebinding stale exact semantics"
        );
        assert!(!requirement_binding_is_reusable(false, Some(true), true));
    }

    #[test]
    fn settled_camera_frame_skips_requirements_and_lease_map() {
        let requirements = (0..4).map(key).collect::<Vec<_>>();
        let mut satisfied = requirements.iter().copied().collect::<HashSet<_>>();
        let mut cursor = requirements.len();
        let mut epoch = Some(7_u64);
        let residency_queries = Cell::new(0_usize);
        let lease_queries = Cell::new(0_usize);

        let work = collect_requirement_lease_updates(
            &requirements,
            |key| *key,
            &mut satisfied,
            &mut cursor,
            &mut epoch,
            7,
            |_| {
                residency_queries.set(residency_queries.get() + 1);
                true
            },
            |_| {
                lease_queries.set(lease_queries.get() + 1);
                Some(())
            },
            |_| 0,
        );

        assert_eq!(work.requirements_visited, 0);
        assert_eq!(work.lease_lookups, 0);
        assert!(work.lease_updates.is_empty());
        assert_eq!(residency_queries.get(), 0);
        assert_eq!(lease_queries.get(), 0);
    }

    #[test]
    fn exact_eviction_delta_invalidates_all_targets_without_scanning_satisfied_bodies() {
        const REQUIREMENT_COUNT: u64 = 65_536;
        let requirements = (0..REQUIREMENT_COUNT).map(key).collect::<Vec<_>>();
        let evicted = [requirements[123], requirements[64_000]];
        let mut first_satisfied = requirements.iter().copied().collect::<HashSet<_>>();
        let mut second_satisfied = first_satisfied.clone();
        let mut first_cursor = requirements.len();
        let mut second_cursor = requirements.len();
        let mut first_epoch = Some(4_u64);
        let mut second_epoch = Some(4_u64);

        let probes = apply_exact_residency_delta(
            [
                (
                    requirements.as_slice(),
                    &mut first_satisfied,
                    &mut first_cursor,
                    &mut first_epoch,
                ),
                (
                    requirements.as_slice(),
                    &mut second_satisfied,
                    &mut second_cursor,
                    &mut second_epoch,
                ),
            ],
            5,
            &evicted,
        );

        assert_eq!(probes, 4, "work is targets times exact evictions");
        assert_eq!(first_satisfied.len(), requirements.len() - evicted.len());
        assert_eq!(second_satisfied.len(), requirements.len() - evicted.len());
        for key in evicted {
            assert!(!first_satisfied.contains(&key));
            assert!(!second_satisfied.contains(&key));
        }
        assert_eq!(first_cursor, 123);
        assert_eq!(second_cursor, 123);
        assert_eq!(first_epoch, Some(5));
        assert_eq!(second_epoch, Some(5));
    }

    #[test]
    fn second_panel_gpu_reuse_becomes_zero_visit_after_one_membership_pass() {
        let requirements = (0..4).map(key).collect::<Vec<_>>();
        let mut second_panel_satisfied = HashSet::new();
        let mut second_panel_cursor = 0;
        let mut second_panel_epoch = None;
        let lease_queries = Cell::new(0_usize);

        let first = collect_requirement_lease_updates(
            &requirements,
            |key| *key,
            &mut second_panel_satisfied,
            &mut second_panel_cursor,
            &mut second_panel_epoch,
            9_u64,
            |_| true,
            |_| {
                lease_queries.set(lease_queries.get() + 1);
                Some(())
            },
            |_| 0,
        );
        assert_eq!(first.requirements_visited, requirements.len());
        assert_eq!(first.lease_lookups, 0);
        assert_eq!(second_panel_satisfied.len(), requirements.len());

        let residency_queries = Cell::new(0_usize);
        let settled = collect_requirement_lease_updates(
            &requirements,
            |key| *key,
            &mut second_panel_satisfied,
            &mut second_panel_cursor,
            &mut second_panel_epoch,
            9,
            |_| {
                residency_queries.set(residency_queries.get() + 1);
                true
            },
            |_| {
                lease_queries.set(lease_queries.get() + 1);
                Some(())
            },
            |_| 0,
        );
        assert_eq!(settled.requirements_visited, 0);
        assert_eq!(settled.lease_lookups, 0);
        assert_eq!(residency_queries.get(), 0);
        assert_eq!(lease_queries.get(), 0);
    }

    #[test]
    fn missing_first_requirement_does_not_block_later_ready_handoffs() {
        let requirements = (0..4).map(key).collect::<Vec<_>>();
        let first_key = requirements[0];
        let mut satisfied = HashSet::new();
        let mut cursor = 0;
        let mut epoch = None;

        let first = collect_requirement_lease_updates(
            &requirements,
            |key| *key,
            &mut satisfied,
            &mut cursor,
            &mut epoch,
            1_u64,
            |_| false,
            |key| (key != first_key).then_some(key),
            |_| 0,
        );
        assert_eq!(first.requirements_visited, requirements.len());
        assert_eq!(first.lease_updates.len(), requirements.len() - 1);
        assert!(first.observed_resident_keys.is_empty());
        assert!(!satisfied.contains(&first_key));
        satisfied.extend(first.selected_lease_keys.iter().copied());

        let second = collect_requirement_lease_updates(
            &requirements,
            |key| *key,
            &mut satisfied,
            &mut cursor,
            &mut epoch,
            1,
            |_| false,
            Some,
            |_| 0,
        );
        assert_eq!(second.requirements_visited, requirements.len());
        assert_eq!(second.lease_updates, vec![first_key]);
        satisfied.extend(second.selected_lease_keys.iter().copied());
        assert_eq!(satisfied.len(), requirements.len());
    }

    #[test]
    fn failed_renderer_execution_retries_selected_cpu_lease() {
        let requirements = vec![key(0), key(1)];
        let mut satisfied = HashSet::new();
        let mut cursor = 0;
        let mut epoch = None;

        let failed_attempt = collect_requirement_lease_updates(
            &requirements,
            |key| *key,
            &mut satisfied,
            &mut cursor,
            &mut epoch,
            1_u64,
            |_| false,
            Some,
            |_| MAX_LEASE_UPDATE_PAYLOAD_BYTES,
        );
        assert_eq!(failed_attempt.selected_lease_keys, vec![requirements[0]]);
        assert!(satisfied.is_empty(), "selection must not commit residency");

        // Every renderer error follows the same pre-acceptance rollback path.
        commit_or_rewind_retained_updates(
            &mut satisfied,
            &mut cursor,
            failed_attempt.starting_requirement_cursor,
            &failed_attempt.selected_lease_keys,
            false,
        );
        let retry = collect_requirement_lease_updates(
            &requirements,
            |key| *key,
            &mut satisfied,
            &mut cursor,
            &mut epoch,
            1_u64,
            |_| false,
            Some,
            |_| MAX_LEASE_UPDATE_PAYLOAD_BYTES,
        );
        assert_eq!(retry.selected_lease_keys, vec![requirements[0]]);
    }

    #[test]
    fn two_renderer_deferrals_cannot_advance_b_behind_c() {
        let requirements = vec![key(0), key(1), key(2)];
        let mut satisfied = HashSet::new();
        let mut cursor = 0;
        let mut epoch = None;
        let mut collect = |satisfied: &mut HashSet<_>, cursor: &mut usize| {
            collect_requirement_lease_updates(
                &requirements,
                |key| *key,
                satisfied,
                cursor,
                &mut epoch,
                1_u64,
                |_| false,
                Some,
                |_| MAX_LEASE_UPDATE_PAYLOAD_BYTES,
            )
        };

        let a = collect(&mut satisfied, &mut cursor);
        assert_eq!(a.selected_lease_keys, vec![requirements[0]]);
        commit_or_rewind_retained_updates(
            &mut satisfied,
            &mut cursor,
            a.starting_requirement_cursor,
            &a.selected_lease_keys,
            true,
        );

        for _ in 0..2 {
            let deferred_b = collect(&mut satisfied, &mut cursor);
            assert_eq!(deferred_b.selected_lease_keys, vec![requirements[1]]);
            commit_or_rewind_retained_updates(
                &mut satisfied,
                &mut cursor,
                deferred_b.starting_requirement_cursor,
                &deferred_b.selected_lease_keys,
                false,
            );
        }
        let accepted_b = collect(&mut satisfied, &mut cursor);
        assert_eq!(accepted_b.selected_lease_keys, vec![requirements[1]]);
        commit_or_rewind_retained_updates(
            &mut satisfied,
            &mut cursor,
            accepted_b.starting_requirement_cursor,
            &accepted_b.selected_lease_keys,
            true,
        );
        let c = collect(&mut satisfied, &mut cursor);
        assert_eq!(c.selected_lease_keys, vec![requirements[2]]);
    }

    #[test]
    fn resident_keys_uploaded_by_another_target_are_exact_observations() {
        let requirements = (0..4).map(key).collect::<Vec<_>>();
        let mut satisfied = HashSet::new();
        let mut cursor = 0;
        let mut epoch = None;

        let work = collect_requirement_lease_updates(
            &requirements,
            |key| *key,
            &mut satisfied,
            &mut cursor,
            &mut epoch,
            1_u64,
            |_| true,
            |_| -> Option<()> { panic!("a renderer-resident key needs no CPU lease lookup") },
            |_: &()| 0,
        );

        assert_eq!(work.observed_resident_keys, requirements);
        assert!(work.lease_updates.is_empty());
        assert!(work.observed_resident_keys.len() <= MAX_REQUIREMENT_VISITS_PER_RENDER);
    }

    #[test]
    fn mixed_dtype_and_validity_payloads_obey_the_renderer_byte_budget() {
        let requirements = (0..13).map(key).collect::<Vec<_>>();
        // Representative 64^3 u8/u16/f32 pages, including packed validity
        // masks and two metadata-only empty pages.
        let mask = 64_u64.pow(3).div_ceil(8);
        let bytes = [
            0,
            64_u64.pow(3),
            2 * 64_u64.pow(3) + mask,
            4 * 64_u64.pow(3) + mask,
            4 * 64_u64.pow(3),
            2 * 64_u64.pow(3),
            64_u64.pow(3) + mask,
            4 * 64_u64.pow(3) + mask,
            0,
            4 * 64_u64.pow(3) + mask,
            4 * 64_u64.pow(3) + mask,
            4 * 64_u64.pow(3) + mask,
            4 * 64_u64.pow(3) + mask,
        ];
        let mut satisfied = HashSet::new();
        let mut cursor = 0;
        let mut epoch = None;

        let work = collect_requirement_lease_updates(
            &requirements,
            |key| *key,
            &mut satisfied,
            &mut cursor,
            &mut epoch,
            1_u64,
            |_| false,
            |key| Some(bytes[key.region().origin()[0] as usize]),
            |bytes| *bytes,
        );

        assert!(work.lease_payload_bytes <= MAX_LEASE_UPDATE_PAYLOAD_BYTES);
        assert!(work.lease_updates.len() <= MAX_LEASE_UPDATES_PER_RENDER);
        assert_eq!(
            work.lease_payload_bytes,
            work.lease_updates.iter().sum::<u64>()
        );
        assert!(work.lease_updates.len() < requirements.len());
        assert_eq!(cursor, work.lease_updates.len());
    }

    #[test]
    fn byte_limited_forwarding_preserves_rank_order_opposed_to_key_order() {
        let canonical = (0..10).map(key).collect::<Vec<_>>();
        let ranked = canonical.iter().rev().copied().collect::<Vec<_>>();
        let mut satisfied = HashSet::new();
        let mut cursor = 0;
        let mut epoch = None;

        let work = collect_requirement_lease_updates(
            &ranked,
            |key| *key,
            &mut satisfied,
            &mut cursor,
            &mut epoch,
            1_u64,
            |_| false,
            Some,
            |_| 1024 * 1024,
        );

        assert_eq!(work.selected_lease_keys, ranked[..8]);
        assert_eq!(work.lease_payload_bytes, MAX_LEASE_UPDATE_PAYLOAD_BYTES);
        assert_ne!(work.selected_lease_keys, canonical[..8]);
    }
}
