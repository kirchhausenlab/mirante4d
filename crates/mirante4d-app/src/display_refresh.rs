use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use super::*;
use crate::{
    cross_section_scheduler::{CrossSectionScheduleInput, schedule_cross_section_panel},
    dataset_demand_plan::cross_section_projected_layer_scales,
    dataset_requests::{
        SCOPE_CROSS_SECTION_XY, SCOPE_CROSS_SECTION_XZ, SCOPE_CROSS_SECTION_YZ, SCOPE_CURRENT_3D,
        SCOPE_CURRENT_3D_REFINEMENT, ScopeReconciliationTargets,
    },
    native_presentation::{
        CompletedCoordinatedValidationCapture, ProductLayerRequirementFacts,
        install_if_current_texture_revision,
    },
    product_render_intent::{ProductRenderRequest, cross_section_intent, volume_intent},
    viewer_layout::PanelId,
    volume_presentation::{
        VolumePreviewCandidate, VolumePreviewCandidateDisposition, VolumeWorkloadProfile,
    },
    workbench_brick_runtime::{
        first_useful_resources_complete_with_renderer,
        first_useful_resources_uploadable_with_renderer, scope_resources_complete_with_renderer,
    },
};
use mirante4d_domain::{CrossSectionView, RenderMode};
use mirante4d_render_api::{
    FrameCompleteness as RenderFrameCompleteness, FrameIdentity, FrameLimitation,
    MAX_RENDER_LAYERS, PreparedRenderRequirements, PresentationTarget, PresentedFrame,
    RenderExtent, RenderRequirements, RenderViewIntent,
};
use mirante4d_render_wgpu::{
    CoordinatedFrameExecutionReport, CoordinatedTargetLayout, CoordinatedTargetRequest,
    CpuFrameTiming, RetainedFrameRenderPolicy, VolumeColorSchedule, WgpuRenderRuntimeError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VolumeRenderPlanSource {
    Scope(u64),
    Navigation(usize),
}

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
/// same keys. Frame identity covers every render-intent change, while retained
/// CPU payload generation and dataset-runtime epoch cover replaceable inputs.
#[derive(Debug, Clone)]
pub(crate) struct ProductRenderFailureSignature {
    frames: [Option<FrameIdentity>; 4],
    requirements: [Option<RenderRequirements>; 4],
    retained_lease_generation: u64,
    dataset_runtime_epoch: u64,
}

impl ProductRenderFailureSignature {
    fn new(
        requests: &[CoordinatedOwnedRequest],
        retained_lease_generation: u64,
        dataset_runtime_epoch: u64,
    ) -> Self {
        let mut frames = [None; 4];
        let mut requirements = std::array::from_fn(|_| None);
        for request in requests {
            let index = request.target.index();
            frames[index] = Some(request.request.intent.frame());
            requirements[index] = Some(request.request.requirements.clone());
        }
        Self {
            frames,
            requirements,
            retained_lease_generation,
            dataset_runtime_epoch,
        }
    }

    fn matches_current(&self, current: &Self) -> bool {
        self.frames == current.frames
            && self
                .requirements
                .iter()
                .zip(&current.requirements)
                .all(|(left, right)| match (left, right) {
                    (Some(left), Some(right)) => {
                        left.shares_resources_with(right)
                            && left.prefetch_promoted() == right.prefetch_promoted()
                    }
                    (None, None) => true,
                    _ => false,
                })
            && self.retained_lease_generation == current.retained_lease_generation
            && self.dataset_runtime_epoch == current.dataset_runtime_epoch
    }
}

/// Only outcomes with a concrete asynchronous completion path bypass the
/// latch. Eviction-log capacity is backpressure rather than a terminal
/// configuration failure because normal event acknowledgement can clear it
/// without changing any render-signature input.
fn product_render_failure_is_deterministic(error: WgpuRenderRuntimeError) -> bool {
    !matches!(
        error,
        WgpuRenderRuntimeError::StaleFrame { .. }
            | WgpuRenderRuntimeError::PipelineNotReady { .. }
            | WgpuRenderRuntimeError::PickCapacityExceeded
            | WgpuRenderRuntimeError::PickBackpressure
            | WgpuRenderRuntimeError::PayloadPlacementUnavailable { .. }
            | WgpuRenderRuntimeError::PayloadRecoveryDeferred
            | WgpuRenderRuntimeError::ResidencyEvictionEventCapacityExceeded { .. }
    )
}

pub(crate) const fn display_refresh_path_label(path: DisplayRefreshPath) -> &'static str {
    match path {
        DisplayRefreshPath::GpuResidentDisplay => "gpu display",
        DisplayRefreshPath::UiBackground => "ui background",
    }
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

struct CoordinatedOwnedRequest {
    target: PresentationTarget,
    panel: PanelId,
    layer_scales: Arc<BTreeMap<mirante4d_domain::LogicalLayerKey, mirante4d_domain::ScaleLevel>>,
    surface_generation: u64,
    request: ProductRenderRequest,
    output_extent: RenderExtent,
    volume_schedule: VolumeColorSchedule,
    volume_profile: Option<VolumeWorkloadProfile>,
    render_policy: RetainedFrameRenderPolicy,
    cross_section: Option<(u64, CrossSectionPanelScheduleState)>,
    staged_3d_refinement: bool,
}

struct PreparedCoordinatedStagedPromotion {
    renderer_update: crate::camera_demand_cache::PreparedRendererRequirementUpdate,
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
        let demand_currentness = self.visible_demand_plan_currentness();
        let three_d = Some(self.presentation_surface(
            PanelId::ThreeD,
            self.render_coordination.presentation_viewport,
            self.render_coordination.frame_fidelity.display_freshness
                == DisplayedFrameFreshness::Current
                && demand_currentness.current_3d,
        ));
        let (xy, xz, yz) =
            if application_view(&snapshot).layout() == CanonicalViewerLayout::FourPanel {
                (
                    self.cross_section_presentation_surface(
                        PanelId::Xy,
                        demand_currentness.cross_section(PanelId::Xy),
                    ),
                    self.cross_section_presentation_surface(
                        PanelId::Xz,
                        demand_currentness.cross_section(PanelId::Xz),
                    ),
                    self.cross_section_presentation_surface(
                        PanelId::Yz,
                        demand_currentness.cross_section(PanelId::Yz),
                    ),
                )
            } else {
                (None, None, None)
            };
        snapshot
            .with_presentations(PresentationSnapshot::new(three_d, xy, xz, yz))
            .with_import_workflow(self.import.snapshot())
    }

    fn cross_section_presentation_surface(
        &self,
        panel_id: PanelId,
        demand_current: bool,
    ) -> Option<PresentationSurface> {
        let panel = self
            .render_coordination
            .surface(panel_id.presentation_slot());
        Some(self.presentation_surface(
            panel_id,
            panel.presentation_viewport()?,
            panel.display_current() && demand_current,
        ))
    }

    fn presentation_surface(
        &self,
        panel_id: PanelId,
        viewport: PresentationViewport,
        frame_is_current: bool,
    ) -> PresentationSurface {
        let frame = self
            .render_coordination
            .surface(panel_id.presentation_slot())
            .presented_frame()
            .cloned();
        PresentationSurface::with_frame_currentness(viewport, frame, frame_is_current)
    }

    pub(crate) fn clear_3d_product_presentation(&mut self) {
        if let Some(product) = self.native_presentation.product_gpu.as_mut() {
            product.clear_validation_capture(PresentationTarget::ThreeD);
        }
        self.render_coordination
            .clear_presented_frame(PresentationSlot::ThreeD);
        self.viewer_render_failure_latch = None;
        self.render_coordination.frame_fidelity.display_freshness =
            DisplayedFrameFreshness::Unknown;
        self.render_coordination.frame_fidelity.three_d_preview = false;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_candidate_count = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_resident_candidate_count = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_safe_candidate_count = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_is_finest_safe = false;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_uses_emergency_floor = false;
        self.render_coordination
            .frame_fidelity
            .three_d_render_viewport = self.render_coordination.render_viewport;
        self.render_coordination
            .frame_fidelity
            .three_d_refinement_strips_completed = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_refinement_strips_total = 0;
    }

    fn record_current_empty_3d_presentation(&mut self) {
        self.clear_3d_product_presentation();
        let generation = self
            .render_coordination
            .surface(PresentationSlot::ThreeD)
            .generation();
        assert!(
            self.render_coordination
                .record_empty_3d_presentation(generation),
            "a synchronous empty publication must match the current 3D surface generation"
        );
        self.render_coordination.frame_fidelity.backend = RenderBackend::Empty;
        self.render_coordination.frame_fidelity.completeness = FrameCompleteness::Complete;
        self.render_coordination.frame_fidelity.reason = LodDecisionReason::NoVisibleData;
        self.render_coordination.frame_fidelity.display_freshness =
            DisplayedFrameFreshness::Current;
        self.render_coordination
            .frame_fidelity
            .displayed_scale_level = None;
        self.render_coordination.frame_fidelity.resident_bricks = 0;
        self.render_coordination
            .frame_fidelity
            .missing_occupied_bricks = 0;
        self.render_coordination.frame_fidelity.three_d_preview = false;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_candidate_count = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_resident_candidate_count = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_safe_candidate_count = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_is_finest_safe = false;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_uses_emergency_floor = false;
        self.render_coordination
            .frame_fidelity
            .three_d_refinement_strips_completed = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_refinement_strips_total = 0;
    }

    pub(crate) fn clear_cross_section_product_presentations(&mut self) {
        if let Some(product) = self.native_presentation.product_gpu.as_mut() {
            for target in [
                PresentationTarget::Xy,
                PresentationTarget::Xz,
                PresentationTarget::Yz,
            ] {
                product.clear_validation_capture(target);
            }
        }
        for slot in [
            PresentationSlot::Xy,
            PresentationSlot::Xz,
            PresentationSlot::Yz,
        ] {
            self.render_coordination.clear_presented_frame(slot);
        }
        self.viewer_render_failure_latch = None;
    }

    fn clear_cross_section_product_presentation(&mut self, panel_id: PanelId) {
        let target = panel_id.presentation_slot();
        if let Some(product) = self.native_presentation.product_gpu.as_mut() {
            product.clear_validation_capture(target);
        }
        self.render_coordination.clear_presented_frame(target);
        self.viewer_render_failure_latch = None;
    }

    pub(crate) fn invalidate_cross_section_panel_display_frames(&mut self) {
        self.render_coordination.invalidate_cross_sections();
    }

    pub(crate) fn clear_product_presentations(&mut self) {
        if let Some(product) = self.native_presentation.product_gpu.as_mut() {
            for target in PresentationTarget::ALL {
                product.clear_validation_capture(target);
            }
        }
        for slot in PresentationSlot::ALL {
            self.render_coordination.clear_presented_frame(slot);
        }
        self.viewer_render_failure_latch = None;
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
        self.volume_presentation.retire_dataset();
        self.viewer_pick_queue.retire_source_generation();
        for slot in PresentationSlot::ALL {
            self.render_coordination.clear_presented_frame(slot);
        }
        self.viewer_render_failure_latch = None;
        self.render_coordination.frame_fidelity.display_freshness =
            DisplayedFrameFreshness::Unknown;
    }

    fn cross_section_panel_needs_display_render(
        &self,
        panel_id: PanelId,
        requirements: &PreparedRenderRequirements,
    ) -> bool {
        if panel_id.cross_section_panel().is_none() {
            return false;
        }
        let panel = self
            .render_coordination
            .surface(panel_id.presentation_slot());
        let renderer_execution_required =
            self.native_presentation
                .product_gpu
                .as_ref()
                .map(|product| {
                    let Some(extent) = panel.render_viewport() else {
                        return true;
                    };
                    let frame = self.render_intent_mailbox.snapshot().linked_2d_revision;
                    match product
                        .renderer
                        .coordinated_target_requires_prepared_presentation(
                            panel_id.presentation_slot(),
                            frame,
                            extent,
                            requirements,
                        ) {
                        Ok(required) => required,
                        Err(WgpuRenderRuntimeError::CoordinatedTargetNotConfigured { .. })
                            if panel.display_current()
                                && panel.cross_section_schedule().is_some_and(|schedule| {
                                    schedule.status == CrossSectionPanelScheduleStatus::Empty
                                }) =>
                        {
                            false
                        }
                        Err(_) => true,
                    }
                });
        panel.render_failure().is_none()
            && (!panel.display_current() || renderer_execution_required == Some(true))
    }

    fn volume_profile_for_render_plan(
        &self,
        snapshot: &ApplicationSnapshot,
        source: VolumeRenderPlanSource,
        camera: mirante4d_domain::CameraView,
        extent: RenderExtent,
    ) -> anyhow::Result<VolumeWorkloadProfile> {
        let prepared = match source {
            VolumeRenderPlanSource::Scope(scope) => self
                .prepared_scope_render_plans
                .get(&scope)
                .ok_or_else(|| {
                    anyhow::anyhow!("scope {scope} has no prepared 3D render requirements")
                })?,
            VolumeRenderPlanSource::Navigation(index) => {
                self.navigation_render_plans.get(index).ok_or_else(|| {
                    anyhow::anyhow!("3D preview has no prepared navigation candidate {index}")
                })?
            }
        };
        VolumeWorkloadProfile::from_view(
            snapshot.catalog(),
            application_view(snapshot),
            camera,
            self.render_coordination.presentation_viewport,
            prepared.layer_scales.as_ref(),
            extent,
        )
    }

    fn select_volume_preview(
        &mut self,
        target_scope: u64,
        target_profile: &VolumeWorkloadProfile,
        target_available: bool,
        snapshot: &ApplicationSnapshot,
        gesture: Option<mirante4d_application::RenderGestureId>,
    ) -> anyhow::Result<(VolumeRenderPlanSource, VolumeWorkloadProfile)> {
        let mut candidates =
            Vec::with_capacity(self.navigation_render_plans.len().saturating_add(1));
        let active_layer = application_view(snapshot).active_layer();
        for (index, plan) in self.navigation_render_plans.iter().enumerate() {
            let profile = VolumeWorkloadProfile::from_view(
                snapshot.catalog(),
                application_view(snapshot),
                target_profile.camera(),
                self.render_coordination.presentation_viewport,
                plan.layer_scales.as_ref(),
                target_profile.extent(),
            )?;
            let available = first_useful_resources_complete_with_renderer(
                &self.dataset,
                &self.native_presentation,
                plan,
            );
            // Only the guaranteed terminal rung may break the cold-start
            // residency cycle. It still needs every first-useful payload
            // retained on the CPU (or already resident), and the renderer
            // cannot present it until those resources finish uploading.
            let cold_bootstrap = index == 0
                && first_useful_resources_uploadable_with_renderer(
                    &self.dataset,
                    &self.native_presentation,
                    plan,
                );
            let active_scale = plan
                .layer_scales
                .get(&active_layer)
                .copied()
                .ok_or_else(|| {
                    anyhow::anyhow!("3D navigation candidate {index} has no active-layer scale")
                })?;
            candidates.push((
                VolumeRenderPlanSource::Navigation(index),
                VolumePreviewCandidate::navigation(
                    profile,
                    available,
                    cold_bootstrap,
                    active_scale,
                    plan.primary_resource_count,
                    plan.render_payload_bytes,
                ),
            ));
        }
        let target_duplicates_navigation = candidates.iter().any(|(_, candidate)| {
            candidate.profile() == target_profile && (!target_available || candidate.available())
        });
        if !target_duplicates_navigation {
            let target_plan = self
                .prepared_scope_render_plans
                .get(&target_scope)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "scope {target_scope} has no prepared exact 3D render requirements"
                    )
                })?;
            let active_scale = target_plan
                .layer_scales
                .get(&active_layer)
                .copied()
                .ok_or_else(|| anyhow::anyhow!("3D target has no active-layer scale"))?;
            candidates.push((
                VolumeRenderPlanSource::Scope(target_scope),
                VolumePreviewCandidate::target(
                    target_profile.clone(),
                    target_available,
                    active_scale,
                    target_plan.requirements.body().canonical().len(),
                    target_plan.render_payload_bytes,
                ),
            ));
        }
        let preview_candidates = candidates
            .iter()
            .map(|(_, candidate)| candidate.clone())
            .collect::<Vec<_>>();
        let choice = self
            .volume_presentation
            .select_preview_profile(target_profile, &preview_candidates, gesture)
            .ok_or_else(|| anyhow::anyhow!("3D preview candidate selection was empty"))?;
        let facts = self.volume_presentation.latest_candidate_facts();
        self.render_coordination
            .frame_fidelity
            .three_d_preview_candidate_count = u32::try_from(facts.len()).unwrap_or(u32::MAX);
        self.render_coordination
            .frame_fidelity
            .three_d_preview_resident_candidate_count = u32::try_from(
            facts
                .iter()
                .filter(|candidate| candidate.complete_and_resident)
                .count(),
        )
        .unwrap_or(u32::MAX);
        self.render_coordination
            .frame_fidelity
            .three_d_preview_safe_candidate_count = u32::try_from(
            facts
                .iter()
                .filter(|candidate| {
                    candidate.complete_and_resident
                        && candidate.target_quality_eligible
                        && candidate.interaction_safe
                })
                .count(),
        )
        .unwrap_or(u32::MAX);
        self.render_coordination
            .frame_fidelity
            .three_d_preview_is_finest_safe = facts
            .iter()
            .any(|candidate| candidate.disposition == VolumePreviewCandidateDisposition::Selected);
        self.render_coordination
            .frame_fidelity
            .three_d_preview_uses_emergency_floor = facts.iter().any(|candidate| {
            candidate.disposition == VolumePreviewCandidateDisposition::SelectedTerminalEmergency
        });
        let (source, _) = candidates
            .get(choice.candidate_index())
            .expect("a selected preview index belongs to its bounded candidate set");
        Ok((*source, choice.into_profile()))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the request boundary keeps independent target, geometry, and refinement facts explicit"
    )]
    fn build_coordinated_request(
        &self,
        panel: PanelId,
        scope: u64,
        snapshot: &ApplicationSnapshot,
        presentation: PresentationViewport,
        extent: RenderExtent,
        output_extent: RenderExtent,
        volume_schedule: VolumeColorSchedule,
        volume_profile: Option<VolumeWorkloadProfile>,
        cross_section: Option<(u64, CrossSectionPanelScheduleState)>,
        staged_3d_refinement: bool,
        navigation_candidate: Option<usize>,
    ) -> anyhow::Result<Option<CoordinatedOwnedRequest>> {
        if staged_3d_refinement && navigation_candidate.is_some() {
            anyhow::bail!("a staged fine frame cannot also bind the navigation ladder");
        }
        if panel == PanelId::ThreeD {
            if volume_profile.is_none() {
                anyhow::bail!("a 3D request requires a bounded workload profile");
            }
            if volume_schedule == VolumeColorSchedule::InteractivePreview && extent != output_extent
            {
                anyhow::bail!("a 3D navigation preview must render at the physical output extent");
            }
        } else if output_extent != extent
            || volume_schedule != VolumeColorSchedule::Direct
            || volume_profile.is_some()
        {
            anyhow::bail!("a linked-plane request cannot use a 3D presentation schedule");
        }
        let base = RenderIntentBase::from_snapshot(snapshot);
        let active_intent_target = self.render_intent_mailbox.active_target(base);
        let durable_camera = *application_view(snapshot).camera();
        let resident_camera = (panel == PanelId::ThreeD)
            .then(|| self.render_intent_mailbox.renderable_camera(base))
            .flatten();
        let camera_override = resident_camera.or_else(|| {
            (panel == PanelId::ThreeD
                && staged_3d_refinement
                && active_intent_target == Some(RenderIntentTarget::ThreeD))
            .then(|| {
                self.render_intent_mailbox
                    .effective_camera(base, durable_camera)
            })
        });
        let prepared = if let Some(index) = navigation_candidate {
            self.navigation_render_plans.get(index).ok_or_else(|| {
                anyhow::anyhow!("resident camera has no prepared navigation candidate {index}")
            })?
        } else {
            let prepared = self
                .prepared_scope_render_plans
                .get(&scope)
                .ok_or_else(|| {
                    anyhow::anyhow!("scope {scope} has no prepared render requirements")
                })?;
            let resources = self.dataset.scope_requirement_handle(scope);
            if !Arc::ptr_eq(&prepared.scope_requirements, &resources) {
                anyhow::bail!(
                    "scope {scope} render requirements do not match its current residency union"
                );
            }
            prepared
        };
        let durable_cross_section = *application_view(snapshot).cross_section();
        let linked_interaction_active = active_intent_target
            .is_some_and(|target| matches!(target, RenderIntentTarget::CrossSection(_)));
        let cross_section_view = panel.cross_section_panel().map(|_| {
            if linked_interaction_active {
                self.render_intent_mailbox
                    .effective_cross_section(base, durable_cross_section)
            } else {
                durable_cross_section
            }
        });
        let mailbox = self.render_intent_mailbox.snapshot();
        let frame = if panel == PanelId::ThreeD {
            mailbox.three_d_revision
        } else {
            mailbox.linked_2d_revision
        };
        let Some(intent) = build_product_intent(
            snapshot,
            frame,
            panel.cross_section_panel().map(|_| panel),
            cross_section_view,
            presentation,
            extent,
            camera_override,
        )?
        else {
            return Ok(None);
        };
        let requirements = prepared.requirements.bind(&intent)?;
        let existing_presentation = self
            .render_coordination
            .surface(panel.presentation_slot())
            .presented_frame()
            .map(|frame| frame.progress().completeness());
        let render_policy = if panel.cross_section_panel().is_some() {
            // Every Plane body owns a complete navigation floor as its
            // first-useful prefix. A useful frame is therefore current
            // geometry with valid fallback fidelity, never a hole-filled
            // partial frame.
            RetainedFrameRenderPolicy::EveryUsefulFrame
        } else if staged_3d_refinement
            || camera_override.is_some()
            || volume_schedule != VolumeColorSchedule::Direct
        {
            RetainedFrameRenderPolicy::ExactFrameOnly
        } else {
            retained_frame_render_policy(true, existing_presentation)
        };
        Ok(Some(CoordinatedOwnedRequest {
            target: panel.presentation_slot(),
            panel,
            layer_scales: Arc::clone(&prepared.layer_scales),
            surface_generation: self
                .render_coordination
                .surface(panel.presentation_slot())
                .generation(),
            request: ProductRenderRequest {
                intent,
                requirements,
            },
            output_extent,
            volume_schedule,
            volume_profile,
            render_policy,
            cross_section,
            staged_3d_refinement,
        }))
    }

    fn retained_exact_frame_for_request(
        &self,
        request: &CoordinatedOwnedRequest,
    ) -> Option<PresentedFrame> {
        self.render_coordination
            .surface(request.target)
            .presented_frame()
            .filter(|frame| {
                frame.target() == request.target
                    && frame.frame() == request.request.intent.frame()
                    && frame.extent() == request.output_extent
                    && frame.progress().completeness() == RenderFrameCompleteness::Exact
            })
            .cloned()
    }

    fn coordinated_active_target(
        &self,
        snapshot: &ApplicationSnapshot,
        requests: &[CoordinatedOwnedRequest],
    ) -> Option<PresentationTarget> {
        let base = RenderIntentBase::from_snapshot(snapshot);
        let transient = self
            .render_intent_mailbox
            .active_target(base)
            .map(|target| match target {
                RenderIntentTarget::ThreeD => PresentationTarget::ThreeD,
                RenderIntentTarget::CrossSection(panel) => {
                    PanelId::from_application_panel(panel).presentation_slot()
                }
            });
        let durable = snapshot
            .transient()
            .active_cross_section_panel()
            .map(PanelId::from_application_panel)
            .map(PanelId::presentation_slot)
            .filter(|_| application_view(snapshot).layout() == CanonicalViewerLayout::FourPanel);
        transient
            .or(durable)
            .filter(|target| requests.iter().any(|request| request.target == *target))
            .or_else(|| {
                requests
                    .iter()
                    .find(|request| request.target == PresentationTarget::ThreeD)
                    .map(|request| request.target)
            })
            .or_else(|| requests.first().map(|request| request.target))
    }

    fn apply_coordinated_layout(
        &mut self,
        desired: &[CoordinatedTargetLayout],
    ) -> anyhow::Result<f64> {
        let started = Instant::now();
        let bindings = {
            let product = self
                .native_presentation
                .product_gpu
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
            let report = product.renderer.request_coordinated_layout(desired)?;
            report
                .targets()
                .iter()
                .map(|state| {
                    Ok::<_, WgpuRenderRuntimeError>((
                        state.target(),
                        state.device_generation(),
                        state.texture_revision(),
                        product
                            .renderer
                            .coordinated_target_texture_view(state.target())?
                            .clone(),
                    ))
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        let retained = bindings
            .iter()
            .map(|(target, _, _, _)| *target)
            .collect::<Vec<_>>();
        self.native_presentation.retain_texture_bindings(&retained);
        for (target, device_generation, texture_revision, view) in bindings {
            self.native_presentation.bind_texture(
                target,
                device_generation,
                texture_revision,
                &view,
            );
        }
        Ok(duration_ms(started.elapsed()))
    }

    fn coordinated_target_needs_execution(&self, target: PresentationTarget) -> bool {
        self.native_presentation
            .product_gpu
            .as_ref()
            .is_none_or(|product| {
                product
                    .renderer
                    .coordinated_target_requires_execution(target)
                    .unwrap_or(true)
            })
    }

    fn record_presented_volume_schedule(&mut self, request: &CoordinatedOwnedRequest) {
        if request.panel != PanelId::ThreeD {
            return;
        }
        let preview = request.volume_schedule == VolumeColorSchedule::InteractivePreview;
        if let Some(profile) = request.volume_profile.clone() {
            if preview {
                self.volume_presentation.note_presented_preview(
                    request.request.intent.frame(),
                    profile,
                    request.output_extent,
                );
            } else {
                self.volume_presentation
                    .note_presented_exact(request.request.intent.frame());
            }
        }
        self.render_coordination
            .frame_fidelity
            .three_d_render_viewport = request.request.intent.extent();
        self.render_coordination.frame_fidelity.three_d_preview = preview;
        self.render_coordination
            .frame_fidelity
            .three_d_refinement_strips_completed = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_refinement_strips_total = 0;
        if preview {
            self.render_coordination.frame_fidelity.reason = LodDecisionReason::FrameBudgetLimited;
            self.render_coordination.frame_fidelity.refinement_pending = true;
            self.render_coordination.request_refresh();
        }
    }

    /// True once every required staged-refinement payload is either retained
    /// by the app or already resident in the renderer. Before this point the
    /// refinement scope can still own live requests needed by the private
    /// candidate, so promotion reconciliation must not cancel them.
    fn staged_3d_promotion_payloads_ready(&self) -> bool {
        let Some(product) = self.native_presentation.product_gpu.as_ref() else {
            return false;
        };
        self.dataset
            .scope_resources_complete_with_gpu_residency(SCOPE_CURRENT_3D_REFINEMENT, |key| {
                product.renderer.resource_is_resident(key)
            })
    }

    fn adopt_retained_exact_request(
        &mut self,
        request: &CoordinatedOwnedRequest,
    ) -> anyhow::Result<bool> {
        let request_execution_required =
            self.native_presentation
                .product_gpu
                .as_ref()
                .is_none_or(|product| {
                    product
                        .renderer
                        .coordinated_target_requires_render_presentation(
                            request.target,
                            request.request.intent.extent(),
                            &request.request.requirements,
                            request.volume_schedule,
                        )
                        .unwrap_or(true)
                });
        if request.staged_3d_refinement || request_execution_required {
            return Ok(false);
        }
        let Some(frame) = self.retained_exact_frame_for_request(request) else {
            return Ok(false);
        };
        if let Some((generation, schedule)) = request.cross_section {
            let coverage = frame.progress().coverage();
            let schedule = cross_section_schedule_for_presented_coverage(
                schedule,
                frame.progress().completeness(),
                coverage.available_requirements(),
                coverage.total_requirements(),
            );
            if !self.render_coordination.record_cross_section_presentation(
                request.target,
                generation,
                schedule,
            ) {
                return Ok(false);
            }
        }
        self.record_coordinated_product_frame(
            request.panel,
            request.surface_generation,
            &frame,
            None,
        );
        self.record_presented_volume_schedule(request);
        self.record_coordinated_layer_presentations_with_scales(
            request.panel,
            Some(request.layer_scales.as_ref()),
        );
        Ok(true)
    }

    /// Atomically rebinds the already-rendered exact transient planes into
    /// the new durable linked generation without manufacturing replacement
    /// pixels. All three panels displayed the same transient geometry and
    /// therefore cross the durable boundary as one retained presentation.
    pub(crate) fn adopt_completed_exact_cross_sections(
        &mut self,
        completed_frame: FrameIdentity,
        completed_view: CrossSectionView,
    ) -> anyhow::Result<bool> {
        let snapshot = self.application.snapshot();
        if application_view(&snapshot).layout() != CanonicalViewerLayout::FourPanel
            || *application_view(&snapshot).cross_section() != completed_view
            || ![PanelId::Xy, PanelId::Xz, PanelId::Yz]
                .into_iter()
                .all(|panel| self.visible_demand_plan_currentness().cross_section(panel))
        {
            return Ok(false);
        }

        let view = application_view(&snapshot);
        let mut adoptions = Vec::with_capacity(3);
        for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
            let target = panel.presentation_slot();
            let scope = cross_section_scope(panel)?;
            let surface = self.render_coordination.surface(target);
            let Some(presentation) = surface.presentation_viewport() else {
                return Ok(false);
            };
            let Some(extent) = surface.render_viewport() else {
                return Ok(false);
            };
            let projected_layer_scales = cross_section_projected_layer_scales(
                snapshot.catalog(),
                view,
                panel,
                presentation,
                extent,
            )?;
            if self.dataset.scope_layer_scales(scope) != Some(&projected_layer_scales) {
                // A transient interaction LOD is truthful while the gesture
                // is active, but it is not an exact durable result after an
                // orientation-driven LOD threshold. The ordinary settled
                // plan must promote the screen-selected target instead.
                return Ok(false);
            }
            let Some(prepared) = self.prepared_scope_render_plans.get(&scope) else {
                return Ok(false);
            };
            let canonical_resources = self.dataset.scope_requirement_handle(scope);
            if !Arc::ptr_eq(
                prepared.requirements.body().canonical(),
                &canonical_resources,
            ) || self.coordinated_target_needs_execution(target)
                || self.native_presentation.texture_id(target).is_none()
            {
                return Ok(false);
            }
            let Some(frame) = self
                .render_coordination
                .surface(target)
                .presented_frame()
                .filter(|frame| {
                    frame.target() == target
                        && frame.frame() == completed_frame
                        && frame.progress().completeness() == RenderFrameCompleteness::Exact
                        && self.render_coordination.surface(target).render_viewport()
                            == Some(frame.extent())
                })
                .cloned()
            else {
                return Ok(false);
            };

            let active_layer_target = self
                .dataset
                .scope_layer_scales(scope)
                .and_then(|scales| scales.get(&view.active_layer()))
                .copied();
            let priority = self.dataset.scope_gpu_priority_handle(scope);
            let required = self.dataset.scope_required_prefix_len(scope);
            let requirements = &priority[..required];
            let all_requirements_available = scope_resources_complete_with_renderer(
                &self.dataset,
                &self.native_presentation,
                scope,
            );
            let prepared = self
                .prepared_scope_render_plans
                .get(&scope)
                .ok_or_else(|| anyhow::anyhow!("linked scope has no prepared render plan"))?;
            let schedule = schedule_cross_section_panel(
                &mut self.render_coordination,
                CrossSectionScheduleInput {
                    view,
                    active_layer_target,
                    requirements,
                    first_useful_requirements: prepared.requirements.first_useful_prefix_len(),
                    first_useful_available: first_useful_resources_complete_with_renderer(
                        &self.dataset,
                        &self.native_presentation,
                        prepared,
                    ),
                    retained_leases: self.dataset.retained_leases(),
                    all_requirements_available,
                    dataset_failed: self.dataset.dispatcher().scope_failure(scope).is_some(),
                    #[cfg(test)]
                    requirement_visit_counter: None,
                },
                panel,
                true,
            )?
            .schedule;
            if !schedule.is_renderable() {
                return Ok(false);
            }
            let coverage = frame.progress().coverage();
            let schedule = cross_section_schedule_for_presented_coverage(
                schedule,
                frame.progress().completeness(),
                coverage.available_requirements(),
                coverage.total_requirements(),
            );
            adoptions.push((
                panel,
                target,
                scope,
                self.render_coordination.surface(target).generation(),
                frame,
                schedule,
            ));
        }
        if !adoptions.iter().all(|(_, target, _, generation, _, _)| {
            self.render_coordination.surface(*target).generation() == *generation
        }) {
            return Ok(false);
        }
        for (panel, target, scope, generation, frame, schedule) in adoptions {
            assert!(
                self.render_coordination
                    .record_cross_section_presentation(target, generation, schedule),
                "a preflighted linked adoption must retain its surface generation"
            );
            self.record_coordinated_product_frame(panel, generation, &frame, None);
            self.record_coordinated_layer_presentations(panel, scope);
        }
        Ok(true)
    }

    fn desired_coordinated_layout(
        &self,
        snapshot: &ApplicationSnapshot,
        requests: &[CoordinatedOwnedRequest],
    ) -> Vec<CoordinatedTargetLayout> {
        PresentationTarget::ALL
            .into_iter()
            .filter(|target| {
                *target == PresentationTarget::ThreeD
                    || application_view(snapshot).layout() == CanonicalViewerLayout::FourPanel
            })
            .filter_map(|target| {
                let surface = self.render_coordination.surface(target);
                let requested = requests.iter().find(|request| request.target == target);
                (requested.is_some() || surface.presented_frame().is_some())
                    .then(|| {
                        requested.map_or_else(
                            || {
                                (target == PresentationTarget::ThreeD)
                                    .then_some(
                                        self.render_coordination
                                            .frame_fidelity
                                            .three_d_render_viewport,
                                    )
                                    .or_else(|| surface.render_viewport())
                            },
                            |request| Some(request.request.intent.extent()),
                        )
                    })
                    .flatten()
                    .map(|extent| CoordinatedTargetLayout::new(target, extent))
            })
            .collect()
    }

    pub(crate) fn rerender_coordinated_display_state(
        &mut self,
    ) -> anyhow::Result<DisplayRefreshWorkTiming> {
        let demand_started = Instant::now();
        self.request_visible_bricks();
        let visible_brick_request_ms = duration_ms(demand_started.elapsed());
        let demand_currentness = self.visible_demand_plan_currentness();
        let demand_renderability = self.visible_demand_renderability();
        let current_3d_is_empty =
            demand_currentness.current_3d && self.dataset.scope_is_empty(SCOPE_CURRENT_3D);
        if current_3d_is_empty {
            // Empty is a terminal semantic result, not GPU work. Publish it
            // even while the renderer is unavailable or still compiling.
            self.record_current_empty_3d_presentation();
            self.record_current_layout_presentation_if_complete();
        }
        if !self
            .native_presentation
            .initial_render_pipeline_is_ready()?
        {
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

        let snapshot = self.application.snapshot();
        let mut requests = Vec::with_capacity(4);
        let base = RenderIntentBase::from_snapshot(&snapshot);
        let resident_camera = self.render_intent_mailbox.renderable_camera(base);
        let resident_target_body = resident_camera
            .is_some_and(|camera| self.resident_camera_target_body_is_complete(camera));
        let resident_navigation_intent = resident_camera.is_some()
            && (resident_target_body
                || (self.dataset.scope_is_installed(SCOPE_CURRENT_3D)
                    && self.navigation_render_plans.first().is_some_and(|plan| {
                        first_useful_resources_complete_with_renderer(
                            &self.dataset,
                            &self.native_presentation,
                            plan,
                        )
                    })));
        if (demand_currentness.current_3d || resident_navigation_intent) && !current_3d_is_empty {
            let retained_complete = self
                .render_coordination
                .surface(PresentationTarget::ThreeD)
                .presented_frame()
                .is_some_and(|frame| {
                    frame.progress().completeness() != RenderFrameCompleteness::Progressive
                });
            let transient_camera_staging = self.dataset.staging_current_refinement()
                && self.render_intent_mailbox.active_target(base)
                    == Some(RenderIntentTarget::ThreeD);
            // A progressive transaction has two complete-frame candidates:
            // the navigation scope for the latest intent, then the finer
            // hidden successor. Whenever input has made the display stale,
            // submit the navigation scope first. Once that exact frame is
            // current, the next refresh may work on the refinement without
            // making interaction wait behind it.
            let navigation_frame_pending = !resident_target_body
                && self.dataset.staging_current_refinement()
                && self.dataset.scope_is_installed(SCOPE_CURRENT_3D)
                && self.render_coordination.frame_fidelity.display_freshness
                    != DisplayedFrameFreshness::Current;
            let staged_3d_refinement = !navigation_frame_pending
                && self.dataset.staging_current_refinement()
                && (transient_camera_staging
                    || self.dataset.holding_previous_presentation()
                    || retained_complete);
            let scope = if staged_3d_refinement {
                SCOPE_CURRENT_3D_REFINEMENT
            } else {
                SCOPE_CURRENT_3D
            };
            let output_extent = self.render_coordination.render_viewport;
            let mailbox = self.render_intent_mailbox.snapshot();
            let frame = mailbox.three_d_revision;
            let preview_gesture = matches!(mailbox.active_target, Some(RenderIntentTarget::ThreeD))
                .then_some(mailbox.active_gesture)
                .flatten();
            let durable_camera = *application_view(&snapshot).camera();
            let workload_camera = resident_camera.unwrap_or_else(|| {
                if self.render_intent_mailbox.active_target(base)
                    == Some(RenderIntentTarget::ThreeD)
                {
                    self.render_intent_mailbox
                        .effective_camera(base, durable_camera)
                } else {
                    durable_camera
                }
            });
            let target_uses_navigation_ladder = !resident_target_body
                && (navigation_frame_pending
                    || (resident_camera.is_some() && !staged_3d_refinement));

            let (
                request_source,
                request_extent,
                request_schedule,
                request_profile,
                request_staged_refinement,
                preview_replacement_needed,
            ) = if target_uses_navigation_ladder {
                let target_profile_source = if self
                    .prepared_scope_render_plans
                    .contains_key(&SCOPE_CURRENT_3D_REFINEMENT)
                {
                    VolumeRenderPlanSource::Scope(SCOPE_CURRENT_3D_REFINEMENT)
                } else {
                    VolumeRenderPlanSource::Scope(SCOPE_CURRENT_3D)
                };
                let target_profile = self.volume_profile_for_render_plan(
                    &snapshot,
                    target_profile_source,
                    workload_camera,
                    output_extent,
                )?;
                let (preview_source, preview_profile) = self.select_volume_preview(
                    match target_profile_source {
                        VolumeRenderPlanSource::Scope(scope) => scope,
                        VolumeRenderPlanSource::Navigation(_) => SCOPE_CURRENT_3D,
                    },
                    &target_profile,
                    false,
                    &snapshot,
                    preview_gesture,
                )?;
                let replace = self.volume_presentation.preview_needs_replacement(
                    frame,
                    &preview_profile,
                    output_extent,
                );
                (
                    preview_source,
                    output_extent,
                    VolumeColorSchedule::InteractivePreview,
                    preview_profile,
                    false,
                    replace,
                )
            } else {
                let target_profile = self.volume_profile_for_render_plan(
                    &snapshot,
                    VolumeRenderPlanSource::Scope(scope),
                    workload_camera,
                    output_extent,
                )?;
                if preview_gesture.is_none()
                    && self.volume_presentation.direct_is_safe(&target_profile)
                {
                    (
                        VolumeRenderPlanSource::Scope(scope),
                        output_extent,
                        VolumeColorSchedule::Direct,
                        target_profile,
                        staged_3d_refinement,
                        false,
                    )
                } else {
                    let (preview_source, preview_profile) = self.select_volume_preview(
                        scope,
                        &target_profile,
                        true,
                        &snapshot,
                        preview_gesture,
                    )?;
                    let replace = self.volume_presentation.preview_needs_replacement(
                        frame,
                        &preview_profile,
                        output_extent,
                    );
                    if replace {
                        (
                            preview_source,
                            output_extent,
                            VolumeColorSchedule::InteractivePreview,
                            preview_profile,
                            false,
                            replace,
                        )
                    } else {
                        let strip_height = self
                            .volume_presentation
                            .refinement_strip_height(frame, &target_profile);
                        (
                            VolumeRenderPlanSource::Scope(scope),
                            output_extent,
                            VolumeColorSchedule::AtomicRefinement {
                                strip_height_pixels: strip_height,
                            },
                            target_profile,
                            staged_3d_refinement,
                            false,
                        )
                    }
                }
            };

            let exact_frame_already_presented = !self.dataset.staging_current_refinement()
                && self.render_coordination.frame_fidelity.display_freshness
                    == DisplayedFrameFreshness::Current
                && self
                    .render_coordination
                    .frame_fidelity
                    .displayed_scale_level
                    == Some(self.render_coordination.frame_fidelity.target_scale_level)
                && !self.coordinated_target_needs_execution(PresentationTarget::ThreeD)
                && self
                    .render_coordination
                    .surface(PresentationTarget::ThreeD)
                    .presented_frame()
                    .is_some_and(|presented| {
                        presented.frame() == frame
                            && presented.extent() == output_extent
                            && presented.progress().completeness() == RenderFrameCompleteness::Exact
                    })
                && !self.render_coordination.frame_fidelity.three_d_preview;
            let needs_execution = !exact_frame_already_presented
                && (request_staged_refinement
                    || matches!(
                        request_schedule,
                        VolumeColorSchedule::AtomicRefinement { .. }
                    )
                    || (request_schedule == VolumeColorSchedule::InteractivePreview
                        && preview_replacement_needed)
                    || (request_schedule == VolumeColorSchedule::Direct
                        && self.render_coordination.frame_fidelity.three_d_preview)
                    || self.render_coordination.frame_fidelity.display_freshness
                        != DisplayedFrameFreshness::Current
                    || self.coordinated_target_needs_execution(PresentationTarget::ThreeD));
            let (request_scope, navigation_candidate) = match request_source {
                VolumeRenderPlanSource::Scope(scope) => (scope, None),
                VolumeRenderPlanSource::Navigation(index) => (SCOPE_CURRENT_3D, Some(index)),
            };
            if needs_execution
                && let Some(request) = self.build_coordinated_request(
                    PanelId::ThreeD,
                    request_scope,
                    &snapshot,
                    self.render_coordination.presentation_viewport,
                    request_extent,
                    output_extent,
                    request_schedule,
                    Some(request_profile),
                    None,
                    request_staged_refinement,
                    navigation_candidate,
                )?
                && !self.adopt_retained_exact_request(&request)?
            {
                requests.push(request);
            }
        }

        if application_view(&snapshot).layout() == CanonicalViewerLayout::FourPanel {
            for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
                if !demand_renderability.cross_section(panel) {
                    continue;
                }
                let scope = cross_section_scope(panel)?;
                let prepared = self
                    .prepared_scope_render_plans
                    .get(&scope)
                    .ok_or_else(|| anyhow::anyhow!("linked scope has no prepared render plan"))?;
                if !self.cross_section_panel_needs_display_render(panel, &prepared.requirements) {
                    continue;
                }
                let priority = self.dataset.scope_gpu_priority_handle(scope);
                let required = self.dataset.scope_required_prefix_len(scope);
                let requirements = &priority[..required];
                let view = application_view(&snapshot);
                let active_layer_target = self
                    .dataset
                    .scope_layer_scales(scope)
                    .and_then(|scales| scales.get(&view.active_layer()))
                    .copied();
                let all_requirements_available = scope_resources_complete_with_renderer(
                    &self.dataset,
                    &self.native_presentation,
                    scope,
                );
                let mut schedule = schedule_cross_section_panel(
                    &mut self.render_coordination,
                    CrossSectionScheduleInput {
                        view,
                        active_layer_target,
                        requirements,
                        first_useful_requirements: prepared.requirements.first_useful_prefix_len(),
                        first_useful_available: first_useful_resources_complete_with_renderer(
                            &self.dataset,
                            &self.native_presentation,
                            prepared,
                        ),
                        retained_leases: self.dataset.retained_leases(),
                        all_requirements_available,
                        dataset_failed: self.dataset.dispatcher().scope_failure(scope).is_some(),
                        #[cfg(test)]
                        requirement_visit_counter: None,
                    },
                    panel,
                    true,
                )?
                .schedule;
                if !demand_currentness.cross_section(panel) {
                    schedule = schedule.provisional();
                }
                if schedule.status == CrossSectionPanelScheduleStatus::Empty {
                    self.clear_cross_section_product_presentation(panel);
                    if !self
                        .render_coordination
                        .record_empty_cross_section_presentation(
                            panel.presentation_slot(),
                            schedule.generation,
                            schedule,
                        )
                    {
                        anyhow::bail!("stale empty cross-section presentation was suppressed");
                    }
                    continue;
                }
                if !schedule.is_renderable() {
                    continue;
                }
                let surface = self.render_coordination.surface(panel.presentation_slot());
                let presentation = surface.presentation_viewport().ok_or_else(|| {
                    anyhow::anyhow!("cross-section presentation viewport is unavailable")
                })?;
                let extent = surface.render_viewport().ok_or_else(|| {
                    anyhow::anyhow!("cross-section render viewport is unavailable")
                })?;
                if let Some(request) = self.build_coordinated_request(
                    panel,
                    scope,
                    &snapshot,
                    presentation,
                    extent,
                    extent,
                    VolumeColorSchedule::Direct,
                    None,
                    Some((surface.generation(), schedule)),
                    false,
                    None,
                )? && !self.adopt_retained_exact_request(&request)?
                {
                    requests.push(request);
                }
            }
        }

        let desired = self.desired_coordinated_layout(&snapshot, &requests);
        let egui_texture_ms = self.apply_coordinated_layout(&desired)?;
        let Some(active_target) = self.coordinated_active_target(&snapshot, &requests) else {
            self.observe_coordinated_display_milestones(false);
            self.record_current_layout_presentation_if_complete();
            return Ok(DisplayRefreshWorkTiming::new(
                DisplayRenderTiming {
                    path: DisplayRefreshPath::UiBackground,
                    render_ms: 0.0,
                    gpu_upload_ms: None,
                    gpu_compute_ms: None,
                    egui_texture_ms,
                },
                visible_brick_request_ms,
            ));
        };

        let failure_signature = ProductRenderFailureSignature::new(
            &requests,
            self.dataset.retained_leases().generation(),
            self.dataset_runtime_epoch,
        );
        if self
            .viewer_render_failure_latch
            .as_ref()
            .is_some_and(|latch| {
                latch.blocks(|failure| failure.matches_current(&failure_signature))
            })
        {
            return Ok(DisplayRefreshWorkTiming::new(
                DisplayRenderTiming {
                    path: DisplayRefreshPath::UiBackground,
                    render_ms: 0.0,
                    gpu_upload_ms: None,
                    gpu_compute_ms: None,
                    egui_texture_ms,
                },
                visible_brick_request_ms,
            ));
        }
        self.viewer_render_failure_latch = None;
        let display_generation = self
            .render_coordination
            .display_generation()
            .input_generation;
        let staged_candidate_ready = {
            let product = self
                .native_presentation
                .product_gpu
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
            let mut ready = false;
            for request in requests
                .iter()
                .filter(|request| request.staged_3d_refinement)
            {
                ready |= match request.volume_schedule {
                    VolumeColorSchedule::Direct => true,
                    VolumeColorSchedule::InteractivePreview => false,
                    VolumeColorSchedule::AtomicRefinement { .. } => {
                        let borrowed = CoordinatedTargetRequest::new(
                            request.target,
                            &request.request.intent,
                            &request.request.requirements,
                            display_generation,
                            request.render_policy,
                        )
                        .with_volume_schedule(request.output_extent, request.volume_schedule, false)
                        .with_hidden_promotion_authorized(false);
                        product
                            .renderer
                            .poll_coordinated_hidden_refinement_ready(borrowed)?
                    }
                };
            }
            ready
        };
        let prepared_staged_promotion = (staged_candidate_ready
            && self.staged_3d_promotion_payloads_ready())
        .then(|| self.prepare_coordinated_staged_current_promotion())
        .transpose()?;
        self.note_viewer_render_submission();
        #[cfg(test)]
        {
            self.product_render_attempts = self.product_render_attempts.saturating_add(1);
        }
        let borrowed = requests
            .iter()
            .map(|request| {
                CoordinatedTargetRequest::new(
                    request.target,
                    &request.request.intent,
                    &request.request.requirements,
                    self.render_coordination
                        .display_generation()
                        .input_generation,
                    request.render_policy,
                )
                .with_volume_schedule(
                    request.output_extent,
                    request.volume_schedule,
                    request.panel == PanelId::ThreeD,
                )
                .with_hidden_promotion_authorized(
                    !request.staged_3d_refinement || prepared_staged_promotion.is_some(),
                )
            })
            .collect::<Vec<_>>();
        let placement_target_group = match (
            requests
                .iter()
                .any(|request| request.panel == PanelId::ThreeD),
            requests
                .iter()
                .any(|request| request.panel != PanelId::ThreeD),
        ) {
            (false, true) => "linked 2D",
            (true, false) => "3D",
            (true, true) | (false, false) => "visible aggregate",
        };
        let render_started = Instant::now();
        let report = {
            let product = self
                .native_presentation
                .product_gpu
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
            match product.renderer.execute_coordinated_frame(
                snapshot.catalog(),
                active_target,
                &borrowed,
            ) {
                Ok(report) => report,
                Err(error @ WgpuRenderRuntimeError::StaleFrame { .. }) => {
                    product.stale_frames_rejected = product.stale_frames_rejected.saturating_add(1);
                    tracing::debug!(%error, "stale coordinated frame was rejected");
                    return Ok(DisplayRefreshWorkTiming::new(
                        DisplayRenderTiming {
                            path: DisplayRefreshPath::UiBackground,
                            render_ms: duration_ms(render_started.elapsed()),
                            gpu_upload_ms: None,
                            gpu_compute_ms: None,
                            egui_texture_ms,
                        },
                        visible_brick_request_ms,
                    ));
                }
                Err(error @ WgpuRenderRuntimeError::PayloadPlacementUnavailable { .. }) => {
                    match product.renderer.recover_payload_fragmentation() {
                        Ok(true) => {
                            tracing::info!(
                                %error,
                                "compacted fragmented GPU payload residency; coordinated frame will retry"
                            );
                            self.render_coordination.request_refresh();
                            return Ok(DisplayRefreshWorkTiming::new(
                                DisplayRenderTiming {
                                    path: DisplayRefreshPath::UiBackground,
                                    render_ms: duration_ms(render_started.elapsed()),
                                    gpu_upload_ms: None,
                                    gpu_compute_ms: None,
                                    egui_texture_ms,
                                },
                                visible_brick_request_ms,
                            ));
                        }
                        Err(WgpuRenderRuntimeError::PayloadRecoveryDeferred) => {
                            self.render_coordination.request_refresh();
                            return Ok(DisplayRefreshWorkTiming::new(
                                DisplayRenderTiming {
                                    path: DisplayRefreshPath::UiBackground,
                                    render_ms: duration_ms(render_started.elapsed()),
                                    gpu_upload_ms: None,
                                    gpu_compute_ms: None,
                                    egui_texture_ms,
                                },
                                visible_brick_request_ms,
                            ));
                        }
                        Err(recovery_error) => return Err(recovery_error.into()),
                        Ok(false) => {}
                    }
                    if self
                        .recover_from_coordinated_payload_placement(placement_target_group, error)
                    {
                        tracing::info!(
                            %error,
                            target_group = placement_target_group,
                            "residual GPU placement refusal will retry through adaptive visible-demand selection"
                        );
                        return Ok(DisplayRefreshWorkTiming::new(
                            DisplayRenderTiming {
                                path: DisplayRefreshPath::UiBackground,
                                render_ms: duration_ms(render_started.elapsed()),
                                gpu_upload_ms: None,
                                gpu_compute_ms: None,
                                egui_texture_ms,
                            },
                            visible_brick_request_ms,
                        ));
                    }
                    return Err(error.into());
                }
                Err(error) => {
                    if product_render_failure_is_deterministic(error) {
                        self.viewer_render_failure_latch =
                            Some(DeterministicFailureLatch::new(failure_signature));
                    }
                    return Err(error.into());
                }
            }
        };
        self.viewer_render_failure_latch = None;
        self.apply_coordinated_execution_report(
            &snapshot,
            &requests,
            &report,
            prepared_staged_promotion,
        );
        let refill_admission = self.retire_coordinated_gpu_resident_payloads(&report) > 0;
        if refill_admission {
            self.dataset.defer_interactive_admission_refill(true);
        }
        // A bounded in-flight deferral produces no strip-progress record, but
        // the renderer still owns an executable hidden candidate. Preserve
        // that renderer fact as the next immediate display transaction rather
        // than waiting for the generic background-work poll. This is also the
        // single continuation rule for ordinary cold uploads and linked
        // targets: if a request's fixed target still reports work, the next UI
        // turn must observe it.
        if requests
            .iter()
            .any(|request| self.coordinated_target_needs_execution(request.target))
        {
            self.render_coordination.request_refresh();
        }
        self.observe_coordinated_display_milestones(false);
        self.record_current_layout_presentation_if_complete();
        Ok(DisplayRefreshWorkTiming::new(
            DisplayRenderTiming {
                path: if report.color_queue_submissions() > 0
                    || PresentationTarget::ALL.into_iter().any(|target| {
                        self.render_coordination
                            .surface(target)
                            .presented_frame()
                            .is_some()
                    }) {
                    DisplayRefreshPath::GpuResidentDisplay
                } else {
                    DisplayRefreshPath::UiBackground
                },
                render_ms: duration_ms(render_started.elapsed()),
                gpu_upload_ms: None,
                gpu_compute_ms: None,
                egui_texture_ms,
            },
            visible_brick_request_ms,
        ))
    }

    fn apply_coordinated_execution_report(
        &mut self,
        snapshot: &ApplicationSnapshot,
        requests: &[CoordinatedOwnedRequest],
        report: &CoordinatedFrameExecutionReport,
        prepared_staged_promotion: Option<PreparedCoordinatedStagedPromotion>,
    ) {
        let any_target_presented = report.targets().iter().any(|target| target.presented());
        if let Some(timing) = report.cpu_timing() {
            real_interaction_trace::record_renderer_cpu_timing(timing);
        }
        if let Some(ticket) = report.gpu_timing()
            && let Some(request) = requests
                .iter()
                .find(|request| request.target == ticket.target())
            && let Some(profile) = request.volume_profile.clone()
        {
            self.volume_presentation.track_timing(
                ticket,
                profile,
                request.volume_schedule,
                request.request.intent.frame(),
                report
                    .target(request.target)
                    .and_then(|target| target.volume_refinement()),
            );
        }
        let staged_presented = requests.iter().any(|request| {
            request.staged_3d_refinement
                && report
                    .target(request.target)
                    .is_some_and(|target| target.presented())
        });
        if staged_presented {
            assert!(
                requests
                    .iter()
                    .filter(|request| request.staged_3d_refinement)
                    .all(|request| {
                        report.target(request.target).is_some_and(|target| {
                            target.progress().is_some_and(|progress| {
                                progress.completeness() == RenderFrameCompleteness::Exact
                            })
                        })
                    }),
                "the renderer may publish a staged 3D candidate only at exact coverage"
            );
            self.commit_coordinated_staged_current_promotion(
                snapshot,
                prepared_staged_promotion
                    .expect(
                        "an exact staged candidate passed payload readiness and promotion preflight before submission",
                    ),
            );
        }
        let bindings = {
            let product = self
                .native_presentation
                .product_gpu
                .as_mut()
                .expect("the renderer that produced the coordinated report remains installed");
            product.last_coordinated_cpu_timing = report.cpu_timing();
            product.last_coordinated_recorded_targets =
                report.recorded_targets().to_vec().into_boxed_slice();
            product.last_coordinated_color_submissions = report.color_queue_submissions();
            product.total_coordinated_color_submissions = product
                .total_coordinated_color_submissions
                .saturating_add(u64::from(report.color_queue_submissions()));
            product.last_coordinated_residency_submissions = report.residency_queue_submissions();
            if let Some(ticket) = report.gpu_timing() {
                product.pending_gpu_timings.push_back(ticket);
            }
            report
                .targets()
                .iter()
                .filter(|target_report| target_report.presented())
                .map(|target_report| {
                    (
                        target_report.target(),
                        target_report.device_generation(),
                        target_report.texture_revision(),
                        product
                            .renderer
                            .coordinated_target_texture_view(target_report.target())
                            .expect(
                                "a presented coordinated target retains its current texture view",
                            )
                            .clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        for (target, device_generation, texture_revision, view) in bindings {
            self.native_presentation.bind_texture(
                target,
                device_generation,
                texture_revision,
                &view,
            );
        }

        for request in requests {
            let target_report = report
                .target(request.target)
                .expect("a coordinated report contains every requested target");
            real_interaction_trace::record_coordinated_execution(
                request.target,
                request.request.intent.frame(),
                match request.request.intent.view() {
                    RenderViewIntent::CrossSection(view) => {
                        Some(view.scale_world_per_screen_point())
                    }
                    RenderViewIntent::Volume { .. } => None,
                },
                target_report.presented(),
                report.color_queue_submissions(),
            );
            if request.panel == PanelId::ThreeD
                && let Some(refinement) = target_report.volume_refinement()
            {
                self.render_coordination
                    .frame_fidelity
                    .three_d_refinement_strips_completed = refinement.completed_strips();
                self.render_coordination
                    .frame_fidelity
                    .three_d_refinement_strips_total = refinement.total_strips();
                self.render_coordination.frame_fidelity.refinement_pending =
                    !refinement.is_complete();
            }
            if !target_report.presented() {
                continue;
            }
            let progress = target_report
                .progress()
                .cloned()
                .expect("a presented coordinated target contains progress");
            let frame = PresentedFrame::new(request.target, request.output_extent, progress);
            if request.staged_3d_refinement {
                debug_assert_eq!(
                    frame.progress().completeness(),
                    RenderFrameCompleteness::Exact,
                    "the renderer publishes an ExactFrameOnly candidate only after exact coverage"
                );
            }

            let previous_partial = self
                .render_coordination
                .surface(request.target)
                .presented_frame()
                .is_some_and(|previous| {
                    previous.progress().completeness() == RenderFrameCompleteness::Progressive
                });
            let current_partial =
                frame.progress().completeness() == RenderFrameCompleteness::Progressive;
            if let Some(product) = self.native_presentation.product_gpu.as_mut() {
                if current_partial && !previous_partial {
                    product.current_partial_frames_presented =
                        product.current_partial_frames_presented.saturating_add(1);
                }
                if !current_partial && previous_partial {
                    product.partial_to_settled_transitions =
                        product.partial_to_settled_transitions.saturating_add(1);
                }
                if let Some(ticket) = target_report.validation_capture() {
                    product.queue_validation_capture(frame.clone(), ticket);
                }
            }

            if let Some((generation, schedule)) = request.cross_section {
                let coverage = frame.progress().coverage();
                let schedule = cross_section_schedule_for_presented_coverage(
                    schedule,
                    frame.progress().completeness(),
                    coverage.available_requirements(),
                    coverage.total_requirements(),
                );
                assert!(
                    self.render_coordination.record_cross_section_presentation(
                        request.target,
                        generation,
                        schedule,
                    ),
                    "the synchronously submitted cross-section generation remains current"
                );
            }
            // Cutoff CPU timing is recorded once on ProductGpuRenderRuntime;
            // a four-target cutoff is not four independent CPU executions.
            self.record_coordinated_product_frame(
                request.panel,
                request.surface_generation,
                &frame,
                None,
            );
            self.record_presented_volume_schedule(request);
            self.record_coordinated_layer_presentations_with_scales(
                request.panel,
                Some(request.layer_scales.as_ref()),
            );
        }
        if any_target_presented && self.dataset.last_plan_error().is_none() {
            self.render_coordination.frame_fidelity.last_failure_kind = None;
            self.render_coordination.frame_fidelity.last_capacity_error = None;
        }
    }

    fn retire_coordinated_gpu_resident_payloads(
        &mut self,
        report: &CoordinatedFrameExecutionReport,
    ) -> usize {
        let newly_resident = report
            .targets()
            .iter()
            .flat_map(|target| target.newly_resident_keys().iter().copied())
            .collect::<BTreeSet<_>>();
        let (dataset, native_presentation) = (&mut self.dataset, &self.native_presentation);
        let Some(product) = native_presentation.product_gpu.as_ref() else {
            return 0;
        };
        dataset.retire_gpu_resident_current_payloads(newly_resident, |key| {
            product.renderer.resource_is_resident(key)
        })
    }

    fn prepare_coordinated_staged_current_promotion(
        &mut self,
    ) -> anyhow::Result<PreparedCoordinatedStagedPromotion> {
        if !self.staged_3d_promotion_payloads_ready() {
            anyhow::bail!(
                "staged promotion reconciliation requires every primary payload to be retained or GPU-resident"
            );
        }
        let mut scope_targets = ScopeReconciliationTargets::default();
        if !self
            .dataset
            .prepare_staged_current_promotion_scope_targets(&mut scope_targets)
        {
            anyhow::bail!("the complete coordinated candidate has no staged dataset plan");
        }
        let post_promotion_update = self
            .staged_post_promotion_renderer_update
            .clone()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "the complete coordinated candidate has no prepared promotion union"
                )
            })?;
        self.dataset
            .preflight_prepared_renderer_requirement_update(
                &post_promotion_update.previous.requirements,
                &post_promotion_update.next.requirements,
            )?;
        let prepared_scope_reconciliation =
            self.dataset.prepare_scope_reconciliation(scope_targets)?;
        // This is the final fallible pre-submit boundary. The readiness gate
        // proves no missing primary payload depends on a cancelled waiter;
        // arrived payloads remain protected by retained/GPU leases. This
        // scheduling cleanup does not publish the staged dataset plan.
        self.dataset
            .commit_prepared_scope_reconciliation(prepared_scope_reconciliation)?;
        Ok(PreparedCoordinatedStagedPromotion {
            renderer_update: post_promotion_update,
        })
    }

    fn commit_coordinated_staged_current_promotion(
        &mut self,
        snapshot: &ApplicationSnapshot,
        prepared: PreparedCoordinatedStagedPromotion,
    ) {
        self.dataset
            .commit_reconciled_gpu_prefilled_staged_current_plan();
        let refinement_render_plan = self
            .prepared_scope_render_plans
            .remove(&SCOPE_CURRENT_3D_REFINEMENT)
            .expect("a preflighted coordinated candidate retains refinement requirements");
        for navigation_candidate in &mut self.navigation_render_plans {
            navigation_candidate.scope_requirements =
                Arc::clone(&refinement_render_plan.scope_requirements);
        }
        self.prepared_scope_render_plans
            .insert(SCOPE_CURRENT_3D, refinement_render_plan);
        let crate::camera_demand_cache::PreparedRendererRequirementUpdate {
            previous,
            next,
            removals,
            removal_charge: _,
        } = prepared.renderer_update;
        self.dataset.commit_preflighted_renderer_requirement_update(
            previous.requirements,
            next.requirements,
            &removals,
            next.charge,
        );
        self.native_presentation
            .product_gpu
            .as_mut()
            .expect("the submitted coordinated renderer remains installed")
            .renderer
            .retire_residency_offers(&removals);
        self.staged_post_promotion_renderer_update = None;
        let base = RenderIntentBase::from_snapshot(snapshot);
        if self.render_intent_mailbox.active_target(base) == Some(RenderIntentTarget::ThreeD)
            && self.visible_demand_plan_currentness().current_3d
            && let Some(revision) = self.render_intent_mailbox.active_revision(base)
        {
            self.render_intent_mailbox.mark_renderable(base, revision);
        }
    }

    fn record_coordinated_product_frame(
        &mut self,
        panel: PanelId,
        surface_generation: u64,
        frame: &PresentedFrame,
        cpu_timing: Option<CpuFrameTiming>,
    ) {
        if let Some(product) = self
            .native_presentation
            .product_gpu
            .as_mut()
            .filter(|product| product.presented_frame_interval_timing_enabled())
        {
            product.record_presented_frame_interval(panel, frame.frame(), cpu_timing);
        }
        assert!(
            self.render_coordination.record_presented_frame(
                panel.presentation_slot(),
                surface_generation,
                frame.clone(),
            ),
            "a coordinated report must match its semantic application surface generation and extent"
        );
        if panel != PanelId::ThreeD {
            return;
        }
        if self.dataset.staging_current_refinement()
            && frame.progress().completeness() != RenderFrameCompleteness::Progressive
        {
            // A settled coarse front can be published after the last dataset
            // completion wake was consumed by this cutoff. Queue the private
            // exact successor explicitly so an otherwise idle UI cannot
            // strand the staged candidate.
            self.render_coordination.request_refresh();
        }
        let snapshot = self.application.snapshot();
        let view = application_view(&snapshot);
        let displayed_scale_level = frame
            .progress()
            .coverage()
            .layer_coverages()
            .find(|coverage| coverage.layer() == view.active_layer())
            .and_then(|coverage| coverage.scale())
            .map(|scale| scale.get());
        let target_settled = !self.dataset.staging_current_refinement()
            && displayed_scale_level
                == Some(self.render_coordination.frame_fidelity.target_scale_level);
        self.display_performance_milestones.observe_three_d(
            frame,
            displayed_scale_level.unwrap_or(self.dataset.current_scale().get()),
            self.render_coordination.frame_fidelity.target_scale_level,
            target_settled,
        );
        let progress = frame.progress();
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
                if displayed_scale_level == Some(0) {
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
            None if self.dataset.current_capacity_constrained() => {
                LodDecisionReason::AdaptiveCapacity
            }
            None if self.dataset.current_playback_downshifted() => {
                LodDecisionReason::PlaybackDownshift
            }
            None if displayed_scale_level == Some(0) => LodDecisionReason::ExactS0,
            None => LodDecisionReason::ScreenEquivalentCoarserScale,
        };
        let mode = view
            .layer(view.active_layer())
            .expect("the current view contains its active layer")
            .render_state()
            .mode();
        self.render_coordination.frame_fidelity.backend = render_backend_for_mode(mode);
        self.render_coordination.frame_fidelity.display_freshness =
            DisplayedFrameFreshness::Current;
        self.render_coordination.frame_fidelity.refinement_pending =
            self.dataset.staging_current_refinement();
        self.render_coordination
            .frame_fidelity
            .displayed_scale_level = displayed_scale_level;
    }

    fn record_coordinated_layer_presentations(&mut self, panel: PanelId, scope: u64) {
        let expected = self.dataset.scope_layer_scales(scope).cloned();
        self.record_coordinated_layer_presentations_with_scales(panel, expected.as_ref());
    }

    fn record_coordinated_layer_presentations_with_scales(
        &mut self,
        panel: PanelId,
        expected: Option<
            &BTreeMap<mirante4d_domain::LogicalLayerKey, mirante4d_domain::ScaleLevel>,
        >,
    ) {
        let presented = self
            .render_coordination
            .surface(panel.presentation_slot())
            .presented_frame();
        let facts = presented
            .into_iter()
            .flat_map(|frame| frame.progress().coverage().layer_coverages())
            .map(|layer| {
                (
                    layer.layer(),
                    ProductLayerRequirementFacts {
                        displayed_scale_level: layer.scale().map(|scale| scale.get()),
                        target_scale_level: Some(layer.target_scale().get()),
                        finest_fallback_scale_level: layer
                            .finest_fallback_scale()
                            .map(|scale| scale.get()),
                        fallback_scale_level: layer.fallback_scale().map(|scale| scale.get()),
                        target_available_requirements: layer.available_target_requirements(),
                        target_total_requirements: layer.total_target_requirements(),
                        available_requirements: layer.available_requirements(),
                        total_requirements: layer.total_requirements(),
                        mixed: layer.is_mixed(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let current = if panel == PanelId::ThreeD {
            self.render_coordination.frame_fidelity.display_freshness
                == DisplayedFrameFreshness::Current
        } else {
            self.render_coordination
                .surface(panel.presentation_slot())
                .display_current()
                && self.visible_demand_plan_currentness().cross_section(panel)
        };
        let layers = expected
            .into_iter()
            .flat_map(|scales| scales.keys())
            .chain(facts.keys())
            .copied()
            .collect::<BTreeSet<_>>();
        let presentations = layers
            .into_iter()
            .map(|layer| {
                product_layer_presentation_status(
                    layer.ordinal(),
                    expected
                        .and_then(|scales| scales.get(&layer))
                        .map(|scale| scale.get()),
                    facts.get(&layer).copied(),
                    current,
                )
            })
            .collect::<Vec<_>>();
        if let Err(overflow) = self
            .render_coordination
            .set_layer_presentations(panel.presentation_slot(), presentations)
        {
            tracing::error!(
                panel = panel.label(),
                actual = overflow.actual,
                maximum = overflow.maximum,
                "layer presentation instrumentation exceeded the renderer layer bound"
            );
        }
    }

    pub(crate) fn poll_product_validation_captures(&mut self) -> anyhow::Result<()> {
        let pending_count = self
            .native_presentation
            .product_gpu
            .as_ref()
            .map_or(0, |product| product.pending_validation_captures.len());
        let mut first_error = None;
        for _ in 0..pending_count {
            let pending = self
                .native_presentation
                .product_gpu
                .as_mut()
                .and_then(|product| product.pending_validation_captures.pop_front())
                .expect("the bounded pending count came from the same queue");
            let result = self
                .native_presentation
                .product_gpu
                .as_mut()
                .expect("a pending capture belongs to the product renderer")
                .renderer
                .poll_coordinated_validation_capture(pending.ticket);
            match result {
                Ok(None) => self
                    .native_presentation
                    .product_gpu
                    .as_mut()
                    .expect("a pending capture belongs to the product renderer")
                    .pending_validation_captures
                    .push_back(pending),
                Ok(Some(capture)) => {
                    let target = pending.ticket.target();
                    let frame_is_current =
                        self.render_coordination.surface(target).presented_frame()
                            == Some(&pending.frame);
                    let binding = self.native_presentation.texture_binding_identity(target);
                    if frame_is_current {
                        let destination = &mut self
                            .native_presentation
                            .product_gpu
                            .as_mut()
                            .expect("a completed capture belongs to the product renderer")
                            .completed_validation_captures[target.index()];
                        install_if_current_texture_revision(
                            binding,
                            pending.ticket.device_generation().get(),
                            pending.ticket.texture_revision().get(),
                            destination,
                            CompletedCoordinatedValidationCapture {
                                frame: pending.frame,
                                ticket: pending.ticket,
                                capture,
                            },
                        );
                    }
                }
                Err(WgpuRenderRuntimeError::StaleValidationCapture) => {
                    // A newer frame may replace the captured presentation
                    // before its asynchronous readback is mapped. The old
                    // observation is then intentionally unusable, but this is
                    // not a renderer failure: discard it and let the current
                    // frame's capture (or the next render) establish evidence.
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
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

    /// Executes one coordinated renderer observation and reports whether it
    /// published new color content into a native presentation texture.
    ///
    /// The egui paint list for the current UI turn is built before this
    /// method is called. A `true` result therefore requires one subsequent
    /// composition wake; otherwise renderer currentness can advance while
    /// the mapped window remains indefinitely on its previous pixels.
    pub(crate) fn refresh_frame(&mut self) -> bool {
        #[cfg(test)]
        {
            self.refresh_frame_calls = self.refresh_frame_calls.saturating_add(1);
        }
        self.poll_product_gpu_timings();
        let color_submissions_before = self
            .native_presentation
            .product_gpu
            .as_ref()
            .map_or(0, |product| product.total_coordinated_color_submissions);
        let total_start = Instant::now();
        let completed_work = match self.rerender_coordinated_display_state() {
            Ok(work) => Some(work),
            Err(error) => {
                tracing::error!(%error, "GPU display refresh failed");
                self.record_product_render_failure(&error);
                None
            }
        };
        if let Some(work) = completed_work {
            self.record_display_refresh_timing(
                work.render,
                work.visible_brick_request_ms,
                duration_ms(total_start.elapsed()),
            );
        }
        self.native_presentation
            .product_gpu
            .as_ref()
            .is_some_and(|product| {
                product.total_coordinated_color_submissions > color_submissions_before
            })
    }

    fn poll_product_gpu_timings(&mut self) {
        let Some(pending_count) = self
            .native_presentation
            .product_gpu
            .as_ref()
            .map(|product| product.pending_gpu_timings.len())
        else {
            return;
        };
        for _ in 0..pending_count {
            let outcome = {
                let product = self
                    .native_presentation
                    .product_gpu
                    .as_mut()
                    .expect("the timing runtime remains installed during one bounded poll");
                let Some(ticket) = product.pending_gpu_timings.front().copied() else {
                    break;
                };
                (ticket, product.renderer.poll_gpu_timing(ticket))
            };
            match outcome {
                (ticket, Ok(Some(timing))) => {
                    self.native_presentation
                        .product_gpu
                        .as_mut()
                        .expect("the timing runtime remains installed")
                        .pending_gpu_timings
                        .pop_front();
                    debug_assert_eq!(timing.ticket(), ticket);
                    if self.volume_presentation.observe_timing(timing) {
                        self.render_coordination.request_refresh();
                    }
                    real_interaction_trace::record_gpu_timing(timing);
                }
                (_, Ok(None)) => break,
                (_, Err(error)) => {
                    let ticket = self
                        .native_presentation
                        .product_gpu
                        .as_mut()
                        .expect("the timing runtime remains installed")
                        .pending_gpu_timings
                        .pop_front()
                        .expect("the failed front timing remains pending");
                    self.volume_presentation.discard_timing(ticket);
                    tracing::warn!(%error, "adaptive GPU timing could not be collected");
                }
            }
        }
    }

    pub(crate) fn refresh_texture_only(&mut self) -> bool {
        self.invalidate_cross_section_panel_display_frames();
        self.refresh_frame()
    }

    fn record_product_render_failure(&mut self, error: &anyhow::Error) {
        let failure = render_state::render_failure_status(error);
        self.render_coordination.frame_fidelity.last_failure_kind = Some(failure.kind());
        self.render_coordination.frame_fidelity.last_capacity_error =
            Some(failure.message().to_owned());
        self.render_coordination.frame_fidelity.completeness = FrameCompleteness::Incomplete;
    }

    /// Projects one renderer-owned terminal startup failure through the same
    /// product failure vocabulary used by ordinary display execution. Returns
    /// true only for the first visible projection of that exact failure.
    pub(crate) fn record_terminal_product_renderer_failure(
        &mut self,
        error: &anyhow::Error,
    ) -> bool {
        let failure = render_state::render_failure_status(error);
        let changed = self.render_coordination.frame_fidelity.last_failure_kind
            != Some(failure.kind())
            || self
                .render_coordination
                .frame_fidelity
                .last_capacity_error
                .as_deref()
                != Some(failure.message());
        if !changed {
            return false;
        }
        let retained_pixels = PresentationTarget::ALL.into_iter().any(|target| {
            self.render_coordination
                .surface(target)
                .presented_frame()
                .is_some()
        });
        self.record_product_render_failure(error);
        self.render_coordination.frame_fidelity.display_freshness = if retained_pixels {
            DisplayedFrameFreshness::Stale
        } else {
            DisplayedFrameFreshness::Unknown
        };
        self.render_coordination.invalidate_cross_sections();
        true
    }
}

fn build_product_intent(
    snapshot: &ApplicationSnapshot,
    frame: FrameIdentity,
    cross_section: Option<PanelId>,
    cross_section_view: Option<CrossSectionView>,
    presentation: PresentationViewport,
    extent: RenderExtent,
    camera_override: Option<CameraView>,
) -> anyhow::Result<Option<mirante4d_render_api::RenderIntent>> {
    match cross_section {
        Some(panel) => cross_section_intent(
            snapshot,
            frame,
            panel,
            cross_section_view.ok_or_else(|| {
                anyhow::anyhow!("a cross-section target requires explicit plane geometry")
            })?,
            presentation,
            extent,
        ),
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
    let target_scale_level = presented_facts.and_then(|facts| facts.target_scale_level);
    let mixed = presented_facts.is_some_and(|facts| facts.mixed);
    mirante4d_application::LayerPresentationStatus {
        layer_ordinal,
        expected_scale_level,
        displayed_scale_level,
        target_scale_level,
        finest_fallback_scale_level: presented_facts
            .and_then(|facts| facts.finest_fallback_scale_level),
        fallback_scale_level: presented_facts.and_then(|facts| facts.fallback_scale_level),
        target_available_requirements: presented_facts
            .map_or(0, |facts| facts.target_available_requirements),
        target_total_requirements: presented_facts
            .map_or(0, |facts| facts.target_total_requirements),
        available_requirements: presented_facts.map_or(0, |facts| facts.available_requirements),
        total_requirements: presented_facts.map_or(0, |facts| facts.total_requirements),
        mixed,
        current: panel_current
            && !mixed
            && expected_scale_level == displayed_scale_level
            && expected_scale_level == target_scale_level,
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
    use super::*;

    #[test]
    fn layer_evidence_uses_the_presented_resource_scale() {
        let status = product_layer_presentation_status(
            7,
            Some(1),
            Some(ProductLayerRequirementFacts {
                displayed_scale_level: Some(0),
                target_scale_level: Some(0),
                finest_fallback_scale_level: Some(1),
                fallback_scale_level: Some(3),
                target_available_requirements: 3,
                target_total_requirements: 4,
                available_requirements: 3,
                total_requirements: 4,
                mixed: false,
            }),
            true,
        );
        assert_eq!(status.layer_ordinal, 7);
        assert_eq!(status.expected_scale_level, Some(1));
        assert_eq!(status.displayed_scale_level, Some(0));
        assert_eq!(status.target_scale_level, Some(0));
        assert_eq!(status.finest_fallback_scale_level, Some(1));
        assert_eq!(status.fallback_scale_level, Some(3));
        assert_eq!(status.target_available_requirements, 3);
        assert_eq!(status.target_total_requirements, 4);
        assert_eq!(status.available_requirements, 3);
        assert_eq!(status.total_requirements, 4);
        assert!(!status.current);
    }

    #[test]
    fn mixed_layer_evidence_never_claims_one_shown_scale_or_exact_currentness() {
        let status = product_layer_presentation_status(
            7,
            Some(0),
            Some(ProductLayerRequirementFacts {
                displayed_scale_level: None,
                target_scale_level: Some(0),
                finest_fallback_scale_level: Some(1),
                fallback_scale_level: Some(3),
                target_available_requirements: 7,
                target_total_requirements: 20,
                available_requirements: 8,
                total_requirements: 21,
                mixed: true,
            }),
            true,
        );
        assert_eq!(status.displayed_scale_level, None);
        assert_eq!(status.target_scale_level, Some(0));
        assert_eq!(status.finest_fallback_scale_level, Some(1));
        assert_eq!(status.fallback_scale_level, Some(3));
        assert!(status.mixed);
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

    fn render_failure_signature(
        frame: u64,
        retained_lease_generation: u64,
        dataset_runtime_epoch: u64,
    ) -> ProductRenderFailureSignature {
        ProductRenderFailureSignature {
            frames: [Some(FrameIdentity::new(frame)), None, None, None],
            requirements: std::array::from_fn(|_| None),
            retained_lease_generation,
            dataset_runtime_epoch,
        }
    }

    #[test]
    fn terminal_positive_lease_failure_executes_once_until_an_exact_input_changes() {
        use mirante4d_render_api::GpuLedgerCategory;

        let current = render_failure_signature(7, 13, 17);
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

        let changed = [
            render_failure_signature(8, 13, 17),
            render_failure_signature(7, 14, 17),
            render_failure_signature(7, 13, 18),
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
        assert!(!product_render_failure_is_deterministic(
            WgpuRenderRuntimeError::PayloadPlacementUnavailable {
                requested_bytes: 256,
                total_free_bytes: 512,
                largest_contiguous_bytes: 128,
            }
        ));
        assert!(!product_render_failure_is_deterministic(
            WgpuRenderRuntimeError::PayloadRecoveryDeferred
        ));
    }

    #[test]
    fn eviction_event_backpressure_does_not_latch_an_unchanged_render_signature() {
        let current = render_failure_signature(7, 13, 17);
        let error = WgpuRenderRuntimeError::ResidencyEvictionEventCapacityExceeded {
            actual: 2,
            maximum: 1,
        };
        let mut latch = None;
        let mut executions = 0_u64;
        for _ in 0..2 {
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

        assert_eq!(
            executions, 2,
            "eviction acknowledgement must permit retry without a render-signature change"
        );
        assert!(latch.is_none());
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
}
