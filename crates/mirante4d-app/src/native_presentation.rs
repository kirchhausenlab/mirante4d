//! Native egui/WGPU presentation owned by the process composition root.

use std::{
    cell::Cell,
    collections::{BTreeMap, HashSet, VecDeque},
    sync::Arc,
    time::Instant,
};

use eframe::egui;
use mirante4d_dataset::DatasetResourceKey;
use mirante4d_domain::LogicalLayerKey;
use mirante4d_render_api::{
    FrameIdentity, PresentationToken, PresentedFrame, RenderExtent, RenderPassKind,
};
use mirante4d_render_wgpu::{
    CpuFrameTiming, GpuFrameTiming, GpuResidencyInvalidationEpoch, GpuTimingTicket,
    ValidationCapture, ValidationCaptureTicket, WgpuRenderRuntime,
};
use mirante4d_ui_egui::EguiPresentationPaint;

use crate::{product_render_intent::ProductRenderRequest, viewer_layout::PanelId};

const MAX_PRESENTED_FRAME_INTERVAL_SAMPLES: usize = 256;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ProductLayerRequirementFacts {
    pub(crate) displayed_scale_level: Option<u32>,
    pub(crate) available_requirements: u64,
    pub(crate) total_requirements: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ProductTargetDiagnosticCounters {
    pub(crate) renderer_calls: u64,
    pub(crate) command_buffers: u64,
    pub(crate) queue_submissions: u64,
    pub(crate) color_passes: u64,
    pub(crate) completion_notifications: u64,
    pub(crate) encoded_display_batches: u64,
    pub(crate) backpressure_deferrals: u64,
    pub(crate) control_static_rebuilds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProductGpuExecutionIdentity {
    pub(crate) execution_id: u64,
    pub(crate) target: PresentationToken,
    pub(crate) renderer_frame: FrameIdentity,
    pub(crate) display_generation: u64,
    pub(crate) pass_kind: RenderPassKind,
}

impl ProductGpuExecutionIdentity {
    pub(crate) fn from_ticket(ticket: GpuTimingTicket) -> Self {
        Self {
            execution_id: ticket.execution_id(),
            target: ticket.target(),
            renderer_frame: ticket.generation(),
            display_generation: ticket.display_generation(),
            pass_kind: ticket.pass_kind(),
        }
    }

    fn matches_ticket(self, ticket: GpuTimingTicket) -> bool {
        self.execution_id == ticket.execution_id()
            && self.target == ticket.target()
            && self.renderer_frame == ticket.generation()
            && self.display_generation == ticket.display_generation()
            && self.pass_kind == ticket.pass_kind()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProductGpuExecutionTiming {
    pub(crate) batch_gpu_envelope_ns: Option<u64>,
    pub(crate) payload_copy_ns: Option<u64>,
    pub(crate) render_pass_ns: Option<u64>,
}

impl From<GpuFrameTiming> for ProductGpuExecutionTiming {
    fn from(timing: GpuFrameTiming) -> Self {
        Self {
            batch_gpu_envelope_ns: timing.batch_gpu_envelope_ns(),
            payload_copy_ns: timing.payload_copy_ns(),
            render_pass_ns: timing.render_pass_ns(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PresentedFrameIntervalSample {
    pub(crate) sequence: u64,
    pub(crate) panel: PanelId,
    pub(crate) frame: FrameIdentity,
    pub(crate) interval_ns: Option<u64>,
    pub(crate) cpu_planning_ns: Option<u64>,
    pub(crate) cpu_queue_submit_ns: Option<u64>,
    pub(crate) gpu_execution: Option<ProductGpuExecutionIdentity>,
    pub(crate) gpu_timing: Option<ProductGpuExecutionTiming>,
}

struct PresentedFrameIntervalDiagnostics {
    enabled: bool,
    previous_by_panel: BTreeMap<PanelId, Instant>,
    samples: VecDeque<PresentedFrameIntervalSample>,
    total_publications: u64,
    dropped_samples: u64,
}

impl PresentedFrameIntervalDiagnostics {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            previous_by_panel: BTreeMap::new(),
            samples: VecDeque::new(),
            total_publications: 0,
            dropped_samples: 0,
        }
    }

    fn observe(
        &mut self,
        panel: PanelId,
        frame: FrameIdentity,
        cpu_timing: Option<CpuFrameTiming>,
        gpu_execution: Option<ProductGpuExecutionIdentity>,
        gpu_timing: Option<ProductGpuExecutionTiming>,
        now: Instant,
    ) {
        if !self.enabled {
            return;
        }
        let interval_ns = self.previous_by_panel.get(&panel).and_then(|previous| {
            now.checked_duration_since(*previous)
                .map(|elapsed| elapsed.as_nanos().try_into().unwrap_or(u64::MAX))
        });
        self.total_publications = self.total_publications.saturating_add(1);
        if self.samples.len() == MAX_PRESENTED_FRAME_INTERVAL_SAMPLES {
            self.samples.pop_front();
            self.dropped_samples = self.dropped_samples.saturating_add(1);
        }
        self.samples.push_back(PresentedFrameIntervalSample {
            sequence: self.total_publications,
            panel,
            frame,
            interval_ns,
            cpu_planning_ns: cpu_timing.map(CpuFrameTiming::planning_ns),
            cpu_queue_submit_ns: cpu_timing.map(CpuFrameTiming::queue_submit_ns),
            gpu_execution,
            gpu_timing,
        });
        self.previous_by_panel.insert(panel, now);
    }

    fn retire_dataset_generation(&mut self) {
        self.previous_by_panel.clear();
        self.samples.clear();
    }

    /// Completes the exact publication that owns this renderer execution.
    /// Returns true only when that publication is still the newest one, which
    /// prevents an older progressive submission for the same FrameIdentity
    /// from overwriting the current display timing.
    fn complete_gpu_timing(&mut self, ticket: GpuTimingTicket, timing: GpuFrameTiming) -> bool {
        if !self.enabled {
            return false;
        }
        let latest_sequence = self.total_publications;
        let Some(sample) = self.samples.iter_mut().find(|sample| {
            sample
                .gpu_execution
                .is_some_and(|execution| execution.matches_ticket(ticket))
        }) else {
            return false;
        };
        sample.gpu_timing = Some(timing.into());
        sample.sequence == latest_sequence
    }

    #[cfg(test)]
    fn complete_gpu_timing_value(
        &mut self,
        execution: ProductGpuExecutionIdentity,
        timing: ProductGpuExecutionTiming,
    ) -> bool {
        if !self.enabled {
            return false;
        }
        let latest_sequence = self.total_publications;
        let Some(sample) = self
            .samples
            .iter_mut()
            .find(|sample| sample.gpu_execution == Some(execution))
        else {
            return false;
        };
        sample.gpu_timing = Some(timing);
        sample.sequence == latest_sequence
    }

    #[cfg(test)]
    fn complete_gpu_timing_for_test(
        &mut self,
        execution: ProductGpuExecutionIdentity,
        timing: ProductGpuExecutionTiming,
    ) -> bool {
        self.complete_gpu_timing_value(execution, timing)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProductFrameExecutionTiming {
    pub(crate) target: PresentationToken,
    pub(crate) frame: FrameIdentity,
    pub(crate) display_generation: u64,
    pub(crate) pass_kind: RenderPassKind,
    pub(crate) cpu: Option<CpuFrameTiming>,
    pub(crate) gpu_ticket: Option<GpuTimingTicket>,
    pub(crate) gpu: Option<GpuFrameTiming>,
}

impl ProductFrameExecutionTiming {
    pub(crate) fn new(
        target: PresentationToken,
        frame: FrameIdentity,
        display_generation: u64,
        pass_kind: RenderPassKind,
        cpu: Option<CpuFrameTiming>,
        gpu_ticket: Option<GpuTimingTicket>,
    ) -> Self {
        assert!(
            gpu_ticket.is_none_or(|ticket| ticket.display_generation() == display_generation),
            "renderer timing ticket must retain the exact display generation"
        );
        Self {
            target,
            frame,
            display_generation,
            pass_kind,
            cpu,
            gpu_ticket,
            gpu: None,
        }
    }

    pub(crate) fn complete_gpu(&mut self, ticket: GpuTimingTicket, timing: GpuFrameTiming) {
        if self.gpu_ticket == Some(ticket) {
            self.gpu = Some(timing);
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProgressiveLeaseProbeState {
    pub(crate) next_requirement: usize,
    pub(crate) requirements_remaining: usize,
    pub(crate) render_requested: bool,
}

pub(crate) struct ProductPresentationTarget {
    pub(crate) token: PresentationToken,
    pub(crate) extent: RenderExtent,
    pub(crate) request: Option<Arc<ProductRenderRequest>>,
    pub(crate) requirement_keys: Arc<[DatasetResourceKey]>,
    pub(crate) lease_priority_keys: Arc<[DatasetResourceKey]>,
    pub(crate) satisfied_requirement_keys: HashSet<DatasetResourceKey>,
    pub(crate) next_unsatisfied_requirement: usize,
    pub(crate) last_residency_invalidation_epoch: Option<GpuResidencyInvalidationEpoch>,
    /// Physical resources proved available by the latest renderer report,
    /// including dormant prefetch. Unlike `presented`, this advances on
    /// control-only and exact-policy executions that intentionally paint no
    /// frame, so background polling can remain O(1) and settle truthfully.
    pub(crate) last_renderer_available_resources: u64,
    pub(crate) presented: Option<PresentedFrame>,
    pub(crate) pending_capture: Option<(PresentedFrame, ValidationCaptureTicket)>,
    pub(crate) completed_capture: Option<(PresentedFrame, ValidationCapture)>,
    pub(crate) pending_gpu_timings: VecDeque<GpuTimingTicket>,
    pub(crate) last_execution_timing: Option<ProductFrameExecutionTiming>,
    pub(crate) partial_seen: bool,
    pub(crate) layer_requirement_facts: BTreeMap<LogicalLayerKey, ProductLayerRequirementFacts>,
    pub(crate) presented_layer_requirement_facts:
        BTreeMap<LogicalLayerKey, ProductLayerRequirementFacts>,
    pub(crate) diagnostic_counters: ProductTargetDiagnosticCounters,
    progressive_lease_probe: Cell<ProgressiveLeaseProbeState>,
}

impl ProductPresentationTarget {
    pub(crate) fn new(token: PresentationToken, extent: RenderExtent) -> Self {
        Self {
            token,
            extent,
            request: None,
            requirement_keys: Arc::from([]),
            lease_priority_keys: Arc::from([]),
            satisfied_requirement_keys: HashSet::new(),
            next_unsatisfied_requirement: 0,
            last_residency_invalidation_epoch: None,
            last_renderer_available_resources: 0,
            presented: None,
            pending_capture: None,
            completed_capture: None,
            pending_gpu_timings: VecDeque::new(),
            last_execution_timing: None,
            partial_seen: false,
            layer_requirement_facts: BTreeMap::new(),
            presented_layer_requirement_facts: BTreeMap::new(),
            diagnostic_counters: ProductTargetDiagnosticCounters::default(),
            progressive_lease_probe: Cell::new(ProgressiveLeaseProbeState {
                next_requirement: 0,
                requirements_remaining: 0,
                render_requested: false,
            }),
        }
    }

    pub(crate) fn reset(&mut self) {
        let token = self.token;
        let extent = self.extent;
        let diagnostic_counters = self.diagnostic_counters;
        *self = Self::new(token, extent);
        self.diagnostic_counters = diagnostic_counters;
    }

    pub(crate) fn reset_layer_requirement_facts(&mut self) {
        self.layer_requirement_facts.clear();
        for key in self.requirement_keys.iter().copied() {
            let facts = self.layer_requirement_facts.entry(key.layer()).or_default();
            facts.displayed_scale_level = Some(key.scale().get());
            facts.total_requirements = facts.total_requirements.saturating_add(1);
        }
    }

    pub(crate) fn mark_layer_requirement_available(&mut self, key: DatasetResourceKey) {
        if let Some(facts) = self.layer_requirement_facts.get_mut(&key.layer()) {
            facts.available_requirements = facts.available_requirements.saturating_add(1);
        }
    }

    pub(crate) fn mark_layer_requirement_unavailable(&mut self, key: DatasetResourceKey) {
        if let Some(facts) = self.layer_requirement_facts.get_mut(&key.layer()) {
            facts.available_requirements = facts.available_requirements.saturating_sub(1);
        }
    }

    /// Opens one finite membership pass after an event that can make another
    /// retained update useful. Ordinary UI polling never reopens the pass.
    pub(crate) fn dirty_progressive_lease_probe(&self) {
        let requirement_count = self.lease_priority_keys.len();
        let next_requirement = if requirement_count == 0 {
            0
        } else {
            self.next_unsatisfied_requirement % requirement_count
        };
        self.progressive_lease_probe
            .set(ProgressiveLeaseProbeState {
                next_requirement,
                requirements_remaining: requirement_count,
                render_requested: false,
            });
    }

    pub(crate) fn progressive_lease_probe_state(&self) -> ProgressiveLeaseProbeState {
        self.progressive_lease_probe.get()
    }

    pub(crate) fn set_progressive_lease_probe_state(&self, state: ProgressiveLeaseProbeState) {
        self.progressive_lease_probe.set(state);
    }

    pub(crate) fn progressive_render_requested(&self) -> bool {
        self.progressive_lease_probe.get().render_requested
    }

    pub(crate) fn clear_progressive_render_request(&self) {
        let mut state = self.progressive_lease_probe.get();
        state.render_requested = false;
        self.progressive_lease_probe.set(state);
    }
}

pub(crate) struct ProductGpuRenderRuntime {
    pub(crate) renderer: WgpuRenderRuntime,
    pub(crate) targets: BTreeMap<PanelId, ProductPresentationTarget>,
    pub(crate) staging_3d: Option<ProductPresentationTarget>,
    next_frame_identity: u64,
    pub(crate) current_partial_frames_presented: u64,
    pub(crate) partial_to_settled_transitions: u64,
    pub(crate) stale_frames_rejected: u64,
    presented_frame_intervals: PresentedFrameIntervalDiagnostics,
}

impl ProductGpuRenderRuntime {
    pub(crate) fn new(renderer: WgpuRenderRuntime) -> Self {
        let frame_timing_enabled = renderer.diagnostics().gpu_timing_enabled();
        Self {
            renderer,
            targets: BTreeMap::new(),
            staging_3d: None,
            next_frame_identity: 1,
            current_partial_frames_presented: 0,
            partial_to_settled_transitions: 0,
            stale_frames_rejected: 0,
            presented_frame_intervals: PresentedFrameIntervalDiagnostics::new(frame_timing_enabled),
        }
    }

    pub(crate) fn record_presented_frame_interval(
        &mut self,
        panel: PanelId,
        frame: FrameIdentity,
        cpu_timing: Option<CpuFrameTiming>,
        gpu_execution: Option<ProductGpuExecutionIdentity>,
        gpu_timing: Option<ProductGpuExecutionTiming>,
    ) {
        if self.presented_frame_intervals.enabled {
            self.presented_frame_intervals.observe(
                panel,
                frame,
                cpu_timing,
                gpu_execution,
                gpu_timing,
                Instant::now(),
            );
        }
    }

    pub(crate) fn complete_presented_frame_gpu_timing(
        &mut self,
        ticket: GpuTimingTicket,
        timing: GpuFrameTiming,
    ) -> bool {
        self.presented_frame_intervals
            .complete_gpu_timing(ticket, timing)
    }

    pub(crate) const fn presented_frame_interval_timing_enabled(&self) -> bool {
        self.presented_frame_intervals.enabled
    }

    pub(crate) const fn presented_frame_interval_samples(
        &self,
    ) -> &VecDeque<PresentedFrameIntervalSample> {
        &self.presented_frame_intervals.samples
    }

    pub(crate) const fn total_presented_frame_publications(&self) -> u64 {
        self.presented_frame_intervals.total_publications
    }

    pub(crate) const fn dropped_presented_frame_interval_samples(&self) -> u64 {
        self.presented_frame_intervals.dropped_samples
    }

    pub(crate) fn allocate_frame_identity(&mut self) -> FrameIdentity {
        let frame = FrameIdentity::new(self.next_frame_identity);
        self.next_frame_identity = self.next_frame_identity.saturating_add(1);
        frame
    }

    /// Keeps the native device and registered targets while removing every
    /// source-scoped renderer and presentation fact before a replacement
    /// dataset can submit work.
    pub(crate) fn retire_dataset_generation(&mut self) {
        self.renderer.retire_dataset_generation();
        for target in self.targets.values_mut().chain(self.staging_3d.iter_mut()) {
            target.reset();
        }
        self.presented_frame_intervals.retire_dataset_generation();
    }

    /// Dirties only targets whose canonical requirement body contains one of
    /// the exact resources that became newly actionable.
    pub(crate) fn dirty_progressive_lease_probes_for_keys(&self, keys: &[DatasetResourceKey]) {
        if keys.is_empty() {
            return;
        }
        for target in self.targets.values().chain(self.staging_3d.iter()) {
            if target.request.is_some()
                && keys
                    .iter()
                    .any(|key| target.requirement_keys.binary_search(key).is_ok())
            {
                target.dirty_progressive_lease_probe();
            }
        }
    }
}

pub(crate) struct NativePresentationBridge {
    texture_renderer: Option<Arc<egui::mutex::RwLock<eframe::egui_wgpu::Renderer>>>,
    device: Option<eframe::wgpu::Device>,
    textures: BTreeMap<PresentationToken, egui::TextureId>,
    pub(crate) product_gpu: Option<ProductGpuRenderRuntime>,
}

impl NativePresentationBridge {
    pub(crate) fn new(
        texture_renderer: Arc<egui::mutex::RwLock<eframe::egui_wgpu::Renderer>>,
        device: eframe::wgpu::Device,
        product_renderer: WgpuRenderRuntime,
    ) -> Self {
        Self {
            texture_renderer: Some(texture_renderer),
            device: Some(device),
            textures: BTreeMap::new(),
            product_gpu: Some(ProductGpuRenderRuntime::new(product_renderer)),
        }
    }

    #[cfg(test)]
    pub(crate) fn unavailable() -> Self {
        Self {
            texture_renderer: None,
            device: None,
            textures: BTreeMap::new(),
            product_gpu: None,
        }
    }

    pub(crate) fn texture_id(&self, token: PresentationToken) -> Option<egui::TextureId> {
        self.textures.get(&token).copied()
    }

    pub(crate) fn dirty_progressive_lease_probes_for_keys(&self, keys: &[DatasetResourceKey]) {
        if let Some(product) = self.product_gpu.as_ref() {
            product.dirty_progressive_lease_probes_for_keys(keys);
        }
    }

    pub(crate) fn paint(
        &self,
        ui: &mut egui::Ui,
        paint: EguiPresentationPaint,
    ) -> anyhow::Result<()> {
        let texture_id = self
            .texture_id(paint.request().token())
            .ok_or_else(|| anyhow::anyhow!("presentation token has no native texture"))?;
        egui::Image::from_texture((texture_id, paint.rect().size()))
            .fit_to_exact_size(paint.rect().size())
            .paint_at(ui, paint.rect());
        Ok(())
    }

    pub(crate) fn bind_texture(
        &mut self,
        token: PresentationToken,
        view: &eframe::wgpu::TextureView,
        extent_changed: bool,
    ) -> anyhow::Result<()> {
        let existing = self.texture_id(token);
        let Some(texture_renderer) = self.texture_renderer.as_ref() else {
            #[cfg(test)]
            if existing.is_some() {
                return Ok(());
            }
            anyhow::bail!("wgpu texture renderer is unavailable");
        };
        let device = self
            .device
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("wgpu device is unavailable"))?;
        let mut texture_renderer = texture_renderer.write();
        let texture_id = if let Some(texture_id) = existing {
            if extent_changed {
                texture_renderer.update_egui_texture_from_wgpu_texture(
                    device,
                    view,
                    display_texture_filter(),
                    texture_id,
                );
            }
            texture_id
        } else {
            texture_renderer.register_native_texture(device, view, display_texture_filter())
        };
        self.textures.insert(token, texture_id);
        Ok(())
    }
}

fn display_texture_filter() -> eframe::wgpu::FilterMode {
    eframe::wgpu::FilterMode::Linear
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn display_texture_handoff_uses_linear_filtering() {
        assert_eq!(display_texture_filter(), eframe::wgpu::FilterMode::Linear);
    }

    #[test]
    fn unavailable_bridge_has_no_native_texture_mapping() {
        let bridge = NativePresentationBridge::unavailable();
        let token = PresentationToken::new(1).unwrap();

        assert_eq!(bridge.texture_id(token), None);
    }

    #[test]
    fn presented_frame_intervals_are_opt_in_and_keep_the_newest_bounded_samples() {
        let origin = Instant::now();
        let mut disabled = PresentedFrameIntervalDiagnostics::new(false);
        disabled.observe(
            PanelId::ThreeD,
            FrameIdentity::new(1),
            None,
            None,
            None,
            origin,
        );
        disabled.observe(
            PanelId::ThreeD,
            FrameIdentity::new(2),
            Some(CpuFrameTiming::new(4, None, None, 2)),
            None,
            None,
            origin + Duration::from_nanos(7),
        );
        assert!(disabled.samples.is_empty());
        assert_eq!(disabled.samples.capacity(), 0);

        let mut enabled = PresentedFrameIntervalDiagnostics::new(true);
        enabled.observe(
            PanelId::ThreeD,
            FrameIdentity::new(1),
            Some(CpuFrameTiming::new(11, None, None, 3)),
            None,
            None,
            origin,
        );
        assert_eq!(enabled.samples.front().unwrap().interval_ns, None);
        assert_eq!(
            enabled.samples.front().unwrap().frame,
            FrameIdentity::new(1)
        );
        assert_eq!(enabled.samples.front().unwrap().cpu_planning_ns, Some(11));
        assert_eq!(
            enabled.samples.front().unwrap().cpu_queue_submit_ns,
            Some(3)
        );
        for index in 1..=MAX_PRESENTED_FRAME_INTERVAL_SAMPLES {
            enabled.observe(
                PanelId::ThreeD,
                FrameIdentity::new(index as u64 + 1),
                Some(CpuFrameTiming::new(index as u64, None, None, 1)),
                None,
                None,
                origin + Duration::from_nanos(index as u64),
            );
        }
        assert_eq!(enabled.samples.len(), MAX_PRESENTED_FRAME_INTERVAL_SAMPLES);
        assert_eq!(enabled.total_publications, 257);
        assert_eq!(enabled.dropped_samples, 1);
        assert_eq!(enabled.samples.front().unwrap().sequence, 2);
        assert_eq!(enabled.samples.back().unwrap().sequence, 257);
        assert!(
            enabled
                .samples
                .iter()
                .all(|sample| sample.interval_ns == Some(1))
        );
    }

    #[test]
    fn gpu_timing_completion_uses_execution_not_reused_progressive_frame_identity() {
        let origin = Instant::now();
        let frame = FrameIdentity::new(9);
        let presentation = PresentationToken::new(3).unwrap();
        let older = ProductGpuExecutionIdentity {
            execution_id: 41,
            target: presentation,
            renderer_frame: frame,
            display_generation: 7,
            pass_kind: RenderPassKind::Volume,
        };
        let newer = ProductGpuExecutionIdentity {
            execution_id: 42,
            target: presentation,
            renderer_frame: frame,
            display_generation: 7,
            pass_kind: RenderPassKind::Volume,
        };
        let mut diagnostics = PresentedFrameIntervalDiagnostics::new(true);
        diagnostics.observe(PanelId::ThreeD, frame, None, Some(older), None, origin);
        diagnostics.observe(
            PanelId::ThreeD,
            frame,
            None,
            Some(newer),
            None,
            origin + Duration::from_nanos(1),
        );

        let older_timing = ProductGpuExecutionTiming {
            batch_gpu_envelope_ns: Some(17),
            payload_copy_ns: None,
            render_pass_ns: Some(17),
        };
        let newer_timing = ProductGpuExecutionTiming {
            batch_gpu_envelope_ns: Some(22),
            payload_copy_ns: Some(3),
            render_pass_ns: Some(19),
        };
        assert!(!diagnostics.complete_gpu_timing_for_test(older, older_timing));
        assert_eq!(diagnostics.samples[0].gpu_timing, Some(older_timing));
        assert_eq!(diagnostics.samples[1].gpu_timing, None);
        assert!(diagnostics.complete_gpu_timing_for_test(newer, newer_timing));
        assert_eq!(diagnostics.samples[1].gpu_timing, Some(newer_timing));
    }

    #[test]
    fn presented_frame_intervals_use_independent_panel_clocks() {
        let origin = Instant::now();
        let mut diagnostics = PresentedFrameIntervalDiagnostics::new(true);
        diagnostics.observe(
            PanelId::ThreeD,
            FrameIdentity::new(1),
            None,
            None,
            None,
            origin,
        );
        diagnostics.observe(
            PanelId::Xy,
            FrameIdentity::new(2),
            None,
            None,
            None,
            origin + Duration::from_nanos(5),
        );
        diagnostics.observe(
            PanelId::ThreeD,
            FrameIdentity::new(3),
            None,
            None,
            None,
            origin + Duration::from_nanos(11),
        );
        diagnostics.observe(
            PanelId::Xy,
            FrameIdentity::new(4),
            None,
            None,
            None,
            origin + Duration::from_nanos(18),
        );

        assert_eq!(diagnostics.samples[0].interval_ns, None);
        assert_eq!(diagnostics.samples[1].interval_ns, None);
        assert_eq!(diagnostics.samples[2].interval_ns, Some(11));
        assert_eq!(diagnostics.samples[3].interval_ns, Some(13));
    }

    #[test]
    fn dataset_retirement_breaks_interval_and_timing_authority_across_sources() {
        let origin = Instant::now();
        let mut diagnostics = PresentedFrameIntervalDiagnostics::new(true);
        diagnostics.observe(
            PanelId::ThreeD,
            FrameIdentity::new(1),
            None,
            None,
            None,
            origin,
        );
        diagnostics.observe(
            PanelId::ThreeD,
            FrameIdentity::new(2),
            None,
            None,
            None,
            origin + Duration::from_nanos(7),
        );

        diagnostics.retire_dataset_generation();

        assert!(diagnostics.samples.is_empty());
        assert!(diagnostics.previous_by_panel.is_empty());
        assert_eq!(diagnostics.total_publications, 2);
        diagnostics.observe(
            PanelId::ThreeD,
            FrameIdentity::new(3),
            None,
            None,
            None,
            origin + Duration::from_nanos(100),
        );
        assert_eq!(diagnostics.samples[0].sequence, 3);
        assert_eq!(diagnostics.samples[0].interval_ns, None);
    }
}
