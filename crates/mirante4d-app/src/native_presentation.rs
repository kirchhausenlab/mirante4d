//! Native egui/WGPU presentation owned by the process composition root.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::Instant,
};

use eframe::egui;
use mirante4d_dataset::DatasetCatalog;
use mirante4d_render_api::{FrameIdentity, PresentationTarget, PresentedFrame};
use mirante4d_render_wgpu::{
    CoordinatedValidationCaptureTicket, CpuFrameTiming, GpuTimingTicket, PipelineCapability,
    PipelineReadiness, RendererDeviceGeneration, TargetTextureRevision, ValidationCapture,
    WgpuRenderRuntime, WgpuRenderRuntimeError,
};
use mirante4d_ui_egui::EguiPresentationPaint;

use crate::viewer_layout::PanelId;

const MAX_PRESENTED_FRAME_INTERVAL_SAMPLES: usize = 256;

pub(crate) fn texture_revision_is_current(
    current: Option<(u64, u64)>,
    device_generation: u64,
    texture_revision: u64,
) -> bool {
    current == Some((device_generation, texture_revision))
}

pub(crate) fn install_if_current_texture_revision<T>(
    current: Option<(u64, u64)>,
    device_generation: u64,
    texture_revision: u64,
    destination: &mut Option<T>,
    candidate: T,
) -> bool {
    if !texture_revision_is_current(current, device_generation, texture_revision) {
        return false;
    }
    *destination = Some(candidate);
    true
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ProductLayerRequirementFacts {
    pub(crate) displayed_scale_level: Option<u32>,
    pub(crate) target_scale_level: Option<u32>,
    pub(crate) finest_fallback_scale_level: Option<u32>,
    pub(crate) fallback_scale_level: Option<u32>,
    pub(crate) target_available_requirements: u64,
    pub(crate) target_total_requirements: u64,
    pub(crate) available_requirements: u64,
    pub(crate) total_requirements: u64,
    pub(crate) mixed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PresentedFrameIntervalSample {
    pub(crate) sequence: u64,
    pub(crate) panel: PanelId,
    pub(crate) frame: FrameIdentity,
    pub(crate) interval_ns: Option<u64>,
    pub(crate) cpu_planning_ns: Option<u64>,
    pub(crate) cpu_queue_submit_ns: Option<u64>,
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
        });
        self.previous_by_panel.insert(panel, now);
    }

    fn retire_dataset_generation(&mut self) {
        self.previous_by_panel.clear();
        self.samples.clear();
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PendingCoordinatedValidationCapture {
    pub(crate) frame: PresentedFrame,
    pub(crate) ticket: CoordinatedValidationCaptureTicket,
}

#[derive(Debug, Clone)]
pub(crate) struct CompletedCoordinatedValidationCapture {
    pub(crate) frame: PresentedFrame,
    pub(crate) ticket: CoordinatedValidationCaptureTicket,
    pub(crate) capture: ValidationCapture,
}

pub(crate) struct ProductGpuRenderRuntime {
    pub(crate) renderer: WgpuRenderRuntime,
    pub(crate) current_partial_frames_presented: u64,
    pub(crate) partial_to_settled_transitions: u64,
    pub(crate) stale_frames_rejected: u64,
    pub(crate) last_coordinated_cpu_timing: Option<CpuFrameTiming>,
    pub(crate) last_coordinated_recorded_targets: Box<[PresentationTarget]>,
    pub(crate) last_coordinated_color_submissions: u32,
    pub(crate) total_coordinated_color_submissions: u64,
    pub(crate) last_coordinated_residency_submissions: u32,
    pub(crate) pending_gpu_timings: VecDeque<GpuTimingTicket>,
    pub(crate) pending_validation_captures: VecDeque<PendingCoordinatedValidationCapture>,
    pub(crate) completed_validation_captures: [Option<CompletedCoordinatedValidationCapture>; 4],
    presented_frame_intervals: PresentedFrameIntervalDiagnostics,
}

impl ProductGpuRenderRuntime {
    pub(crate) fn new(renderer: WgpuRenderRuntime) -> Self {
        let frame_timing_enabled = renderer.diagnostics().gpu_timing_enabled();
        Self {
            renderer,
            current_partial_frames_presented: 0,
            partial_to_settled_transitions: 0,
            stale_frames_rejected: 0,
            last_coordinated_cpu_timing: None,
            last_coordinated_recorded_targets: Box::new([]),
            last_coordinated_color_submissions: 0,
            total_coordinated_color_submissions: 0,
            last_coordinated_residency_submissions: 0,
            pending_gpu_timings: VecDeque::new(),
            pending_validation_captures: VecDeque::new(),
            completed_validation_captures: std::array::from_fn(|_| None),
            presented_frame_intervals: PresentedFrameIntervalDiagnostics::new(frame_timing_enabled),
        }
    }

    pub(crate) fn record_presented_frame_interval(
        &mut self,
        panel: PanelId,
        frame: FrameIdentity,
        cpu_timing: Option<CpuFrameTiming>,
    ) {
        if self.presented_frame_intervals.enabled {
            self.presented_frame_intervals
                .observe(panel, frame, cpu_timing, Instant::now());
        }
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

    pub(crate) fn clear_validation_capture(&mut self, target: PresentationTarget) {
        self.pending_validation_captures
            .retain(|pending| pending.ticket.target() != target);
        self.completed_validation_captures[target.index()] = None;
    }

    pub(crate) fn queue_validation_capture(
        &mut self,
        frame: PresentedFrame,
        ticket: CoordinatedValidationCaptureTicket,
    ) {
        self.clear_validation_capture(ticket.target());
        self.pending_validation_captures
            .push_back(PendingCoordinatedValidationCapture { frame, ticket });
    }

    pub(crate) fn completed_validation_capture(
        &self,
        target: PresentationTarget,
    ) -> Option<&CompletedCoordinatedValidationCapture> {
        self.completed_validation_captures[target.index()].as_ref()
    }

    /// Keeps the native device and fixed-target allocations while removing
    /// every source-scoped renderer and readback fact.
    pub(crate) fn retire_dataset_generation(&mut self) {
        self.renderer.retire_dataset_generation();
        self.pending_validation_captures.clear();
        self.completed_validation_captures = std::array::from_fn(|_| None);
        self.last_coordinated_cpu_timing = None;
        self.last_coordinated_recorded_targets = Box::new([]);
        self.last_coordinated_color_submissions = 0;
        self.total_coordinated_color_submissions = 0;
        self.last_coordinated_residency_submissions = 0;
        self.presented_frame_intervals.retire_dataset_generation();
    }

    pub(crate) fn activate_dataset_generation(
        &mut self,
        catalog: &DatasetCatalog,
    ) -> Result<(), WgpuRenderRuntimeError> {
        self.renderer.activate_dataset_generation(catalog)
    }
}

pub(crate) struct NativePresentationBridge {
    texture_renderer: Option<Arc<egui::mutex::RwLock<eframe::egui_wgpu::Renderer>>>,
    device: Option<eframe::wgpu::Device>,
    textures: BTreeMap<PresentationTarget, BoundTargetTexture>,
    pub(crate) product_gpu: Option<ProductGpuRenderRuntime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BoundTargetTexture {
    device_generation: u64,
    texture_revision: u64,
    texture_id: egui::TextureId,
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

    #[cfg(test)]
    pub(crate) fn with_headless_product_renderer(product_renderer: WgpuRenderRuntime) -> Self {
        Self {
            texture_renderer: None,
            device: None,
            textures: BTreeMap::new(),
            product_gpu: Some(ProductGpuRenderRuntime::new(product_renderer)),
        }
    }

    pub(crate) fn texture_id(&self, target: PresentationTarget) -> Option<egui::TextureId> {
        self.textures.get(&target).map(|binding| binding.texture_id)
    }

    pub(crate) fn texture_binding_identity(
        &self,
        target: PresentationTarget,
    ) -> Option<(u64, u64)> {
        self.textures
            .get(&target)
            .map(|binding| (binding.device_generation, binding.texture_revision))
    }

    /// Projects the renderer-owned fixed-pipeline state without retaining a
    /// second readiness flag in the composition layer.
    pub(crate) fn product_pipeline_readiness(
        &self,
    ) -> Option<Result<PipelineReadiness, WgpuRenderRuntimeError>> {
        self.product_gpu
            .as_ref()
            .map(|product| product.renderer.pipeline_readiness())
    }

    pub(crate) fn initial_render_pipeline_is_ready(&self) -> Result<bool, WgpuRenderRuntimeError> {
        self.product_gpu.as_ref().map_or(Ok(false), |product| {
            product
                .renderer
                .pipeline_capability_is_ready(PipelineCapability::InitialRender)
        })
    }

    pub(crate) fn pick_pipeline_is_ready(&self) -> Result<bool, WgpuRenderRuntimeError> {
        self.product_gpu.as_ref().map_or(Ok(false), |product| {
            product
                .renderer
                .pipeline_capability_is_ready(PipelineCapability::Pick)
        })
    }

    pub(crate) fn paint(
        &self,
        ui: &mut egui::Ui,
        paint: EguiPresentationPaint,
    ) -> anyhow::Result<()> {
        let target = paint.request().target();
        let texture_id = self
            .texture_id(target)
            .ok_or_else(|| anyhow::anyhow!("{target:?} has no native texture binding"))?;
        if let Some((_device_generation, texture_revision)) = self.texture_binding_identity(target)
        {
            crate::real_interaction_trace::record_egui_texture_paint_queued(
                target,
                texture_revision,
            );
        }
        egui::Image::from_texture((texture_id, paint.rect().size()))
            .fit_to_exact_size(paint.rect().size())
            .paint_at(ui, paint.rect());
        Ok(())
    }

    pub(crate) fn bind_texture(
        &mut self,
        target: PresentationTarget,
        device_generation: RendererDeviceGeneration,
        texture_revision: TargetTextureRevision,
        view: &eframe::wgpu::TextureView,
    ) {
        let existing = self.textures.get(&target).copied();
        if existing.is_some_and(|binding| {
            binding.device_generation == device_generation.get()
                && binding.texture_revision == texture_revision.get()
        }) {
            return;
        }
        let Some(texture_renderer) = self.texture_renderer.as_ref() else {
            #[cfg(test)]
            if self.product_gpu.is_some() {
                // App-level renderer tests exercise the real offscreen WGPU
                // target without constructing an unrelated egui paint pass.
                self.textures.insert(
                    target,
                    BoundTargetTexture {
                        device_generation: device_generation.get(),
                        texture_revision: texture_revision.get(),
                        texture_id: egui::TextureId::User(
                            u64::try_from(target.index() + 1)
                                .expect("the fixed target index fits a texture id"),
                        ),
                    },
                );
                return;
            }
            panic!("an installed product renderer has no egui or headless texture binding");
        };
        let device = self
            .device
            .as_ref()
            .expect("the egui texture renderer and WGPU device are installed together");
        let mut texture_renderer = texture_renderer.write();
        let texture_id = if let Some(existing) = existing {
            texture_renderer.update_egui_texture_from_wgpu_texture(
                device,
                view,
                display_texture_filter(),
                existing.texture_id,
            );
            existing.texture_id
        } else {
            texture_renderer.register_native_texture(device, view, display_texture_filter())
        };
        self.textures.insert(
            target,
            BoundTargetTexture {
                device_generation: device_generation.get(),
                texture_revision: texture_revision.get(),
                texture_id,
            },
        );
    }

    /// Applies the complete renderer-owned target set to the egui binding
    /// layer. Omitted targets are retired here as well as in the renderer so
    /// an old native view cannot keep a deactivated allocation alive.
    pub(crate) fn retain_texture_bindings(&mut self, retained: &[PresentationTarget]) {
        let omitted = self
            .textures
            .keys()
            .copied()
            .filter(|target| !retained.contains(target))
            .collect::<Vec<_>>();
        for target in omitted {
            let binding = self
                .textures
                .remove(&target)
                .expect("the omitted target came from the texture map");
            if let Some(texture_renderer) = self.texture_renderer.as_ref() {
                texture_renderer.write().free_texture(&binding.texture_id);
            }
        }
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

        assert_eq!(bridge.texture_id(PresentationTarget::ThreeD), None);
    }

    #[test]
    fn complete_layout_omission_retires_the_binding_identity() {
        let mut bridge = NativePresentationBridge::unavailable();
        bridge.textures.insert(
            PresentationTarget::ThreeD,
            BoundTargetTexture {
                device_generation: 4,
                texture_revision: 8,
                texture_id: egui::TextureId::User(1),
            },
        );
        bridge.textures.insert(
            PresentationTarget::Xy,
            BoundTargetTexture {
                device_generation: 4,
                texture_revision: 9,
                texture_id: egui::TextureId::User(2),
            },
        );

        bridge.retain_texture_bindings(&[PresentationTarget::ThreeD]);

        assert_eq!(
            bridge.texture_binding_identity(PresentationTarget::ThreeD),
            Some((4, 8))
        );
        assert_eq!(
            bridge.texture_binding_identity(PresentationTarget::Xy),
            None
        );
        assert_eq!(bridge.texture_id(PresentationTarget::Xy), None);
    }

    #[test]
    fn presented_frame_intervals_are_opt_in_and_keep_the_newest_bounded_samples() {
        let origin = Instant::now();
        let mut disabled = PresentedFrameIntervalDiagnostics::new(false);
        disabled.observe(PanelId::ThreeD, FrameIdentity::new(1), None, origin);
        disabled.observe(
            PanelId::ThreeD,
            FrameIdentity::new(2),
            Some(CpuFrameTiming::new(4, 2)),
            origin + Duration::from_nanos(7),
        );
        assert!(disabled.samples.is_empty());
        assert_eq!(disabled.samples.capacity(), 0);

        let mut enabled = PresentedFrameIntervalDiagnostics::new(true);
        enabled.observe(
            PanelId::ThreeD,
            FrameIdentity::new(1),
            Some(CpuFrameTiming::new(11, 3)),
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
                Some(CpuFrameTiming::new(index as u64, 1)),
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
    fn presented_frame_intervals_use_independent_panel_clocks() {
        let origin = Instant::now();
        let mut diagnostics = PresentedFrameIntervalDiagnostics::new(true);
        diagnostics.observe(PanelId::ThreeD, FrameIdentity::new(1), None, origin);
        diagnostics.observe(
            PanelId::Xy,
            FrameIdentity::new(2),
            None,
            origin + Duration::from_nanos(5),
        );
        diagnostics.observe(
            PanelId::ThreeD,
            FrameIdentity::new(3),
            None,
            origin + Duration::from_nanos(11),
        );
        diagnostics.observe(
            PanelId::Xy,
            FrameIdentity::new(4),
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
        diagnostics.observe(PanelId::ThreeD, FrameIdentity::new(1), None, origin);
        diagnostics.observe(
            PanelId::ThreeD,
            FrameIdentity::new(2),
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
            origin + Duration::from_nanos(100),
        );
        assert_eq!(diagnostics.samples[0].sequence, 3);
        assert_eq!(diagnostics.samples[0].interval_ns, None);
    }
}
