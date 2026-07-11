use super::*;
use crate::brick_streaming::{current_resident_frame_ready, stream_layer_ids_for_state};
use crate::cross_section_scheduler::{
    CROSS_SECTION_PANEL_RENDER_SUBMISSIONS_PER_PANEL_REFRESH, cross_section_panel_refinement_due,
    mark_cross_section_panel_render_failed, mark_cross_section_panel_rendered,
    schedule_cross_section_panel_for_state,
};
use crate::cross_section_streaming::submit_cross_section_visible_chunks_to_read_queue;
use crate::image_compositing::color_image_for_state;

#[derive(Clone)]
pub(crate) enum ViewportDisplayImage {
    Cpu(egui::TextureHandle),
    Gpu {
        texture_id: egui::TextureId,
        size: egui::Vec2,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisplayRefreshPath {
    GpuResidentDisplay,
    CpuTexture,
}

impl DisplayRefreshPath {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::GpuResidentDisplay => "gpu display",
            Self::CpuTexture => "cpu texture",
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
pub(crate) struct DisplayRefreshTiming {
    pub(crate) path: DisplayRefreshPath,
    pub(crate) render_ms: f64,
    pub(crate) gpu_upload_ms: Option<f64>,
    pub(crate) gpu_compute_ms: Option<f64>,
    pub(crate) egui_texture_ms: f64,
    pub(crate) visible_brick_request_ms: f64,
    pub(crate) cpu_texture_update_ms: f64,
    pub(crate) total_ms: f64,
}

#[derive(Clone)]
struct DisplayStateSnapshot {
    frame: MipImageU16,
    frame_f32: Option<MipImageF32>,
    diagnostics: FrameDiagnostics,
    diagnostics_f32: Option<FrameDiagnosticsF32>,
    render_backend: RenderBackend,
    frame_fidelity: FrameFidelityStatus,
    channel_fidelity: Vec<ChannelFidelityStatus>,
    lod_schedule: LodScheduleState,
    rendered_channels: Vec<RenderedIntensityChannel>,
}

impl DisplayStateSnapshot {
    fn capture(state: &AppState) -> Self {
        Self {
            frame: state.frame.clone(),
            frame_f32: state.frame_f32.clone(),
            diagnostics: state.diagnostics,
            diagnostics_f32: state.diagnostics_f32,
            render_backend: state.render_backend,
            frame_fidelity: state.frame_fidelity.clone(),
            channel_fidelity: state.channel_fidelity.clone(),
            lod_schedule: state.lod_schedule,
            rendered_channels: state.rendered_channels.clone(),
        }
    }

    fn restore(self, state: &mut AppState) {
        state.frame = self.frame;
        state.frame_f32 = self.frame_f32;
        state.diagnostics = self.diagnostics;
        state.diagnostics_f32 = self.diagnostics_f32;
        state.render_backend = self.render_backend;
        state.frame_fidelity = self.frame_fidelity;
        state.channel_fidelity = self.channel_fidelity;
        state.lod_schedule = self.lod_schedule;
        state.rendered_channels = self.rendered_channels;
    }
}

impl ViewportDisplayImage {
    pub(crate) fn size_vec2(&self) -> egui::Vec2 {
        match self {
            Self::Cpu(texture) => texture.size_vec2(),
            Self::Gpu { size, .. } => *size,
        }
    }
}

pub(crate) fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

impl MiranteWorkbenchApp {
    pub(crate) fn ensure_texture(&mut self, ctx: &egui::Context) -> &egui::TextureHandle {
        self.texture.get_or_insert_with(|| {
            ctx.load_texture(
                "mirante4d-mip",
                color_image_for_state(&self.state),
                egui::TextureOptions::NEAREST,
            )
        })
    }

    pub(crate) fn viewport_display_image(&mut self, ctx: &egui::Context) -> ViewportDisplayImage {
        if let (Some(frame), Some(texture_id)) =
            (self.gpu_display_frame.as_ref(), self.gpu_display_texture_id)
        {
            return ViewportDisplayImage::Gpu {
                texture_id,
                size: egui::vec2(frame.viewport.width as f32, frame.viewport.height as f32),
            };
        }
        ViewportDisplayImage::Cpu(self.ensure_texture(ctx).clone())
    }

    pub(crate) fn cross_section_panel_display_image(
        &self,
        panel_id: PanelId,
    ) -> Option<ViewportDisplayImage> {
        let displayed = self.cross_section_gpu_display_frames.get(&panel_id)?;
        Some(ViewportDisplayImage::Gpu {
            texture_id: displayed.texture_id,
            size: egui::vec2(
                displayed.frame.viewport.width as f32,
                displayed.frame.viewport.height as f32,
            ),
        })
    }

    pub(crate) fn clear_gpu_display_frame(&mut self) {
        self.gpu_display_frame = None;
        self.gpu_display_frame_identity = None;
        self.state.frame_fidelity.display_freshness = DisplayedFrameFreshness::Unknown;
    }

    pub(crate) fn retire_gpu_display_texture_id(&mut self) {
        if let Some(texture_id) = self.gpu_display_texture_id.take() {
            self.retired_gpu_display_texture_ids.push(texture_id);
        }
    }

    pub(crate) fn retire_cross_section_gpu_display_texture_ids(&mut self) {
        for displayed in self.cross_section_gpu_display_frames.values_mut() {
            self.retired_gpu_display_texture_ids
                .push(displayed.texture_id);
        }
        self.cross_section_gpu_display_frames.clear();
    }

    pub(crate) fn invalidate_cross_section_panel_display_frames(&mut self) {
        self.state.viewer_layout.mark_cross_section_panels_dirty();
    }

    pub(crate) fn free_retired_gpu_display_textures(&mut self) {
        if self.retired_gpu_display_texture_ids.is_empty() {
            return;
        }
        let Some(texture_renderer) = self.wgpu_texture_renderer.as_ref() else {
            self.retired_gpu_display_texture_ids.clear();
            return;
        };
        let mut texture_renderer = texture_renderer.write();
        for texture_id in self.retired_gpu_display_texture_ids.drain(..) {
            texture_renderer.free_texture(&texture_id);
        }
    }

    fn ensure_gpu_display_path_ready(&self) -> anyhow::Result<()> {
        if !resident_brick_render_supported(self.state.active_render_mode) {
            anyhow::bail!(
                "GPU resident display does not support the active mode {:?}",
                self.state.active_render_mode
            );
        }
        if !self.state.brick_stream_complete || !current_resident_frame_ready(&self.state) {
            anyhow::bail!("resident brick set is incomplete for GPU resident display");
        }
        scene_draw_list_for_state(&self.state)?;

        let layer_ids = stream_layer_ids_for_state(&self.state)?;
        if layer_ids.is_empty() {
            anyhow::bail!("GPU resident display requires at least one visible resident layer");
        }
        for layer_id in &layer_ids {
            let layer_id = layer_id.to_string();
            if !self.state.layers.iter().any(|layer| layer.id == layer_id) {
                anyhow::bail!("visible resident layer {layer_id} is not loaded in app state");
            }
        }
        Ok(())
    }

    fn register_or_update_gpu_display_texture(
        &mut self,
        frame: &GpuDisplayFrame,
    ) -> anyhow::Result<egui::TextureId> {
        let texture_id =
            self.register_or_update_gpu_texture_id(frame, self.gpu_display_texture_id)?;
        self.gpu_display_texture_id = Some(texture_id);
        Ok(texture_id)
    }

    fn register_or_update_gpu_texture_id(
        &self,
        frame: &GpuDisplayFrame,
        existing_texture_id: Option<egui::TextureId>,
    ) -> anyhow::Result<egui::TextureId> {
        let gpu_renderer = self
            .gpu_renderer
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("GPU renderer is unavailable"))?;
        let Some(texture_renderer) = self.wgpu_texture_renderer.as_ref() else {
            #[cfg(test)]
            if let Some(texture_id) = existing_texture_id {
                return Ok(texture_id);
            }
            anyhow::bail!("wgpu texture renderer is unavailable");
        };
        let mut texture_renderer = texture_renderer.write();
        let texture_filter = gpu_display_texture_filter();
        if let Some(texture_id) = existing_texture_id {
            texture_renderer.update_egui_texture_from_wgpu_texture(
                gpu_renderer.device(),
                frame.texture_view(),
                texture_filter,
                texture_id,
            );
            Ok(texture_id)
        } else {
            let texture_id = texture_renderer.register_native_texture(
                gpu_renderer.device(),
                frame.texture_view(),
                texture_filter,
            );
            Ok(texture_id)
        }
    }

    fn cross_section_panel_needs_display_render(&self, panel_id: PanelId) -> bool {
        let Some(runtime) = self.state.viewer_layout.four_panel_runtime() else {
            return false;
        };
        let Some(panel) = runtime.panel(panel_id) else {
            return false;
        };
        if panel_id.cross_section_panel().is_none() {
            return false;
        }
        let stale_or_missing = match self.cross_section_gpu_display_frames.get(&panel_id) {
            Some(displayed) => displayed.generation != panel.generation || !panel.display_current(),
            None => true,
        };
        let incomplete_current = panel.display_current()
            && panel
                .cross_section_schedule
                .is_some_and(|schedule| schedule.missing_occupied_bricks > 0);
        stale_or_missing
            || incomplete_current
            || cross_section_panel_refinement_due(&self.state, panel_id)
    }

    pub(crate) fn render_cross_section_panel_for_display_if_needed(
        &mut self,
        panel_id: PanelId,
    ) -> anyhow::Result<Option<DisplayRenderTiming>> {
        if !self.cross_section_panel_needs_display_render(panel_id) {
            return Ok(None);
        }
        if CROSS_SECTION_PANEL_RENDER_SUBMISSIONS_PER_PANEL_REFRESH == 0 {
            return Ok(None);
        }
        let gpu_display_available =
            self.gpu_renderer.is_some() && self.wgpu_texture_renderer.is_some();
        let schedule = schedule_cross_section_panel_for_state(
            &mut self.state,
            panel_id,
            gpu_display_available,
        )?
        .schedule;
        if !schedule.is_renderable() {
            if gpu_display_available && let Some(pool) = &self.cross_section_read_pool {
                let submission =
                    submit_cross_section_visible_chunks_to_read_queue(&mut self.state, pool)?;
                if submission.queued || submission.resident_changed {
                    self.state.brick_result_drain_last_repaint_reason =
                        Some("cross_section_panel_loading".to_owned());
                }
            }
            return Ok(None);
        }
        if schedule.missing_occupied_bricks > 0
            && gpu_display_available
            && let Some(pool) = &self.cross_section_read_pool
        {
            let submission =
                submit_cross_section_visible_chunks_to_read_queue(&mut self.state, pool)?;
            if submission.queued || submission.resident_changed {
                self.state.brick_result_drain_last_repaint_reason =
                    Some("cross_section_panel_loading".to_owned());
            }
        }
        let gpu_renderer = self
            .gpu_renderer
            .clone()
            .ok_or_else(|| anyhow::anyhow!("GPU renderer is unavailable"))?;
        let render_generation = schedule.generation;
        self.state
            .cross_section_runtime
            .mark_panel_resident_chunks_upload_queued(panel_id, render_generation);
        let render_start = Instant::now();
        let rendered = match render_gpu_cross_section_panel_frame_from_global_runtime(
            &self.state,
            gpu_renderer.as_ref(),
            panel_id,
        ) {
            Ok(rendered) => rendered,
            Err(err) => {
                self.state
                    .cross_section_runtime
                    .restore_panel_upload_queued_chunks_to_cpu_resident(
                        panel_id,
                        render_generation,
                    );
                mark_cross_section_panel_render_failed(&mut self.state, panel_id, schedule);
                return Err(err);
            }
        };
        self.state
            .cross_section_runtime
            .reconcile_panel_chunks_with_renderer_gpu_residency(
                rendered.panel_id,
                rendered.generation,
                &rendered.renderer_gpu_resident_chunks,
            );
        let render_ms = duration_ms(render_start.elapsed());
        let texture_start = Instant::now();
        let existing_texture_id = self
            .cross_section_gpu_display_frames
            .get(&panel_id)
            .map(|displayed| displayed.texture_id);
        let texture_id =
            self.register_or_update_gpu_texture_id(&rendered.frame, existing_texture_id)?;
        let egui_texture_ms = duration_ms(texture_start.elapsed());
        if !self
            .state
            .viewer_layout
            .mark_panel_displayed(rendered.panel_id, rendered.generation)
        {
            let mut stale_schedule = schedule;
            stale_schedule.reason =
                crate::viewer_layout::CrossSectionPanelScheduleReason::StaleGeneration;
            self.state
                .viewer_layout
                .set_cross_section_panel_schedule(rendered.panel_id, stale_schedule);
            if existing_texture_id != Some(texture_id) {
                self.retired_gpu_display_texture_ids.push(texture_id);
            }
            anyhow::bail!(
                "stale {} cross-section frame generation {} was not displayed",
                rendered.panel_id.label(),
                rendered.generation
            );
        }
        mark_cross_section_panel_rendered(&mut self.state, rendered.panel_id, schedule);
        let gpu_upload_ms = rendered.frame.timings.upload_ms();
        let gpu_compute_ms = rendered.frame.timings.gpu_compute_ms();
        self.cross_section_gpu_display_frames.insert(
            rendered.panel_id,
            CrossSectionPanelGpuDisplayFrame {
                generation: rendered.generation,
                frame: rendered.frame,
                texture_id,
            },
        );
        Ok(Some(DisplayRenderTiming {
            path: DisplayRefreshPath::GpuResidentDisplay,
            render_ms,
            gpu_upload_ms: Some(gpu_upload_ms),
            gpu_compute_ms,
            egui_texture_ms,
        }))
    }

    pub(crate) fn render_gpu_display_frame_for_current_state(
        &mut self,
    ) -> anyhow::Result<DisplayRenderTiming> {
        self.ensure_gpu_display_path_ready()?;
        let gpu_renderer = self
            .gpu_renderer
            .clone()
            .ok_or_else(|| anyhow::anyhow!("GPU renderer is unavailable"))?;
        let previous_display_state = DisplayStateSnapshot::capture(&self.state);
        let render_start = Instant::now();
        let mut frame = match render_gpu_display_frame_from_resident_bricks(
            &mut self.state,
            gpu_renderer.as_ref(),
        ) {
            Ok(frame) => frame,
            Err(err) => {
                previous_display_state.restore(&mut self.state);
                return Err(err);
            }
        };
        if self.state.viewer_layout.layout() == ViewerLayout::FourPanel {
            frame = match gpu_renderer.detach_display_frame_texture(frame) {
                Ok(frame) => frame,
                Err(err) => {
                    previous_display_state.restore(&mut self.state);
                    return Err(err.into());
                }
            };
        }
        let render_ms = duration_ms(render_start.elapsed());
        let texture_start = Instant::now();
        if let Err(err) = self.register_or_update_gpu_display_texture(&frame) {
            previous_display_state.restore(&mut self.state);
            return Err(err);
        }
        let egui_texture_ms = duration_ms(texture_start.elapsed());
        let gpu_upload_ms = frame.timings.upload_ms();
        let gpu_compute_ms = frame.timings.gpu_compute_ms();
        let display_identity = GpuDisplayedFrameIdentity::from_state(&self.state);
        self.state.frame_fidelity.display_freshness =
            display_identity.display_freshness_for_state(&self.state);
        self.gpu_display_frame_identity = Some(display_identity);
        self.gpu_display_frame = Some(frame);
        self.texture = None;
        Ok(DisplayRenderTiming {
            path: DisplayRefreshPath::GpuResidentDisplay,
            render_ms,
            gpu_upload_ms: Some(gpu_upload_ms),
            gpu_compute_ms,
            egui_texture_ms,
        })
    }

    fn render_current_resident_frame_to_cpu_texture(
        &mut self,
    ) -> anyhow::Result<DisplayRenderTiming> {
        self.clear_gpu_display_frame();
        let render_start = Instant::now();
        render_state_from_resident_bricks_with_backend(
            &mut self.state,
            self.gpu_renderer.as_deref(),
        )?;
        self.texture = None;
        Ok(DisplayRenderTiming {
            path: DisplayRefreshPath::CpuTexture,
            render_ms: duration_ms(render_start.elapsed()),
            gpu_upload_ms: None,
            gpu_compute_ms: None,
            egui_texture_ms: 0.0,
        })
    }

    pub(crate) fn render_current_resident_frame_for_display(
        &mut self,
    ) -> anyhow::Result<DisplayRenderTiming> {
        if self.gpu_renderer.is_none() || self.wgpu_texture_renderer.is_none() {
            return self.render_current_resident_frame_to_cpu_texture();
        }
        self.render_gpu_display_frame_for_current_state()
    }

    pub(crate) fn rerender_display_state(&mut self) -> anyhow::Result<DisplayRenderTiming> {
        update_visible_brick_plan(&mut self.state);
        if current_resident_frame_ready(&self.state)
            && resident_brick_render_supported(self.state.active_render_mode)
        {
            return self.render_current_resident_frame_for_display();
        }
        if self.can_preserve_gpu_presented_frame_for_pending_request() {
            self.mark_target_pending_while_preserving_gpu_frame();
            return Ok(DisplayRenderTiming {
                path: DisplayRefreshPath::GpuResidentDisplay,
                render_ms: 0.0,
                gpu_upload_ms: None,
                gpu_compute_ms: None,
                egui_texture_ms: 0.0,
            });
        }
        self.clear_gpu_display_frame();
        let render_start = Instant::now();
        rerender_state_with_backend(&mut self.state, self.gpu_renderer.as_deref())?;
        Ok(DisplayRenderTiming {
            path: DisplayRefreshPath::CpuTexture,
            render_ms: duration_ms(render_start.elapsed()),
            gpu_upload_ms: None,
            gpu_compute_ms: None,
            egui_texture_ms: 0.0,
        })
    }

    fn can_preserve_gpu_presented_frame_for_pending_request(&self) -> bool {
        if self.gpu_display_frame.is_none() {
            return false;
        }
        gpu_presented_frame_compatible_for_pending_request(
            self.gpu_display_frame_identity.as_ref(),
            self.gpu_display_texture_id.is_some(),
            &GpuDisplayedFrameIdentity::from_state(&self.state),
        )
    }

    fn mark_target_pending_while_preserving_gpu_frame(&mut self) {
        self.state.frame_fidelity.target_scale_level = self.state.lod_schedule.target_scale_level;
        self.state.frame_fidelity.viewport = self.state.render_viewport;
        self.state.frame_fidelity.presentation_viewport = self.state.presentation_viewport;
        if self.state.frame_fidelity.completeness != FrameCompleteness::BudgetLimited {
            self.state.frame_fidelity.completeness = FrameCompleteness::Loading;
            self.state.frame_fidelity.reason = LodDecisionReason::LoadingTargetScale;
        }
        self.state.frame_fidelity.last_failure_kind = None;
        self.state.frame_fidelity.last_capacity_error = None;
        self.state.frame_fidelity.display_freshness = self
            .gpu_display_frame_identity
            .as_ref()
            .map(|identity| identity.display_freshness_for_state(&self.state))
            .unwrap_or(DisplayedFrameFreshness::Unknown);
        refresh_fidelity_resource_stats(&mut self.state, self.gpu_renderer.as_deref());
        update_channel_fidelity_status(&mut self.state);
    }

    pub(crate) fn record_display_refresh_timing(
        &mut self,
        render: DisplayRenderTiming,
        visible_brick_request_ms: f64,
        cpu_texture_update_ms: f64,
        total_ms: f64,
    ) {
        self.last_display_refresh_timing = Some(DisplayRefreshTiming {
            path: render.path,
            render_ms: render.render_ms,
            gpu_upload_ms: render.gpu_upload_ms,
            gpu_compute_ms: render.gpu_compute_ms,
            egui_texture_ms: render.egui_texture_ms,
            visible_brick_request_ms,
            cpu_texture_update_ms,
            total_ms,
        });
    }

    pub(crate) fn record_preserved_display_refresh_timing(
        &mut self,
        visible_brick_request_ms: f64,
        cpu_texture_update_ms: f64,
        total_ms: f64,
    ) {
        self.last_display_refresh_timing = Some(DisplayRefreshTiming {
            path: if self.gpu_display_frame.is_some() {
                DisplayRefreshPath::GpuResidentDisplay
            } else {
                DisplayRefreshPath::CpuTexture
            },
            render_ms: 0.0,
            gpu_upload_ms: None,
            gpu_compute_ms: None,
            egui_texture_ms: 0.0,
            visible_brick_request_ms,
            cpu_texture_update_ms,
            total_ms,
        });
    }

    pub(crate) fn update_cpu_texture_if_needed(&mut self) -> f64 {
        let cpu_texture_start = Instant::now();
        if self.gpu_display_frame.is_none()
            && let Some(texture) = self.texture.as_mut()
        {
            texture.set(
                color_image_for_state(&self.state),
                egui::TextureOptions::NEAREST,
            );
        }
        duration_ms(cpu_texture_start.elapsed())
    }

    pub(crate) fn refresh_frame(&mut self, ctx: &egui::Context) {
        let total_start = Instant::now();
        let mut visible_brick_request_ms = 0.0;
        match self.rerender_display_state() {
            Ok(render_timing) => {
                self.state.last_render_error = None;
                let brick_request_start = Instant::now();
                self.request_visible_bricks();
                visible_brick_request_ms += duration_ms(brick_request_start.elapsed());
                let cpu_texture_update_ms = self.update_cpu_texture_if_needed();
                self.record_display_refresh_timing(
                    render_timing,
                    visible_brick_request_ms,
                    cpu_texture_update_ms,
                    duration_ms(total_start.elapsed()),
                );
                ctx.request_repaint();
            }
            Err(err) => {
                self.state.last_render_error = Some(err.to_string());
                tracing::error!(error = %err, "camera render failed");
            }
        }
    }

    pub(crate) fn refresh_texture_only(&mut self, ctx: &egui::Context) {
        let total_start = Instant::now();
        self.invalidate_cross_section_panel_display_frames();
        if self.gpu_display_frame.is_some() {
            match self.rerender_display_state() {
                Ok(render_timing) => {
                    self.state.last_render_error = None;
                    if self.gpu_display_frame.is_some() {
                        self.record_display_refresh_timing(
                            render_timing,
                            0.0,
                            0.0,
                            duration_ms(total_start.elapsed()),
                        );
                        ctx.request_repaint();
                        return;
                    }
                }
                Err(err) => {
                    self.state.last_render_error = Some(err.to_string());
                    tracing::error!(error = %err, "texture refresh render failed");
                    return;
                }
            }
        }
        self.clear_gpu_display_frame();
        let cpu_texture_update_ms = self.update_cpu_texture_if_needed();
        self.last_display_refresh_timing = Some(DisplayRefreshTiming {
            path: DisplayRefreshPath::CpuTexture,
            render_ms: 0.0,
            gpu_upload_ms: None,
            gpu_compute_ms: None,
            egui_texture_ms: 0.0,
            visible_brick_request_ms: 0.0,
            cpu_texture_update_ms,
            total_ms: duration_ms(total_start.elapsed()),
        });
        ctx.request_repaint();
    }
}

fn gpu_display_texture_filter() -> eframe::wgpu::FilterMode {
    eframe::wgpu::FilterMode::Linear
}

fn gpu_presented_frame_compatible_for_pending_request(
    frame_identity: Option<&GpuDisplayedFrameIdentity>,
    texture_registered: bool,
    requested_identity: &GpuDisplayedFrameIdentity,
) -> bool {
    let Some(frame_identity) = frame_identity else {
        return false;
    };
    texture_registered
        && resident_brick_render_supported(requested_identity.mode)
        && frame_identity.compatible_with_pending_request(requested_identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChannelRenderState, display_graph::DisplayChannelModeIdentity};

    fn test_display_identity(
        mode: RenderMode,
        viewport: RenderViewport,
    ) -> GpuDisplayedFrameIdentity {
        GpuDisplayedFrameIdentity {
            mode,
            channel_modes: vec![DisplayChannelModeIdentity {
                layer_id: "layer".to_owned(),
                render_state: ChannelRenderState::for_mode(
                    mode,
                    RenderSamplingPolicy::default(),
                    RenderIsoShadingPolicy::default(),
                    DEFAULT_ISO_DISPLAY_LEVEL,
                    DvrOpacityTransfer::new(
                        DisplayWindow::new(0.0, 1.0).unwrap(),
                        TransferCurve::Linear,
                    )
                    .unwrap(),
                    DEFAULT_DVR_DENSITY_SCALE,
                ),
            }],
            viewport,
            presentation_viewport: crate::viewport::default_presentation_viewport(),
            camera: CameraView::default_for_bounds(16.0, 16.0, 16.0),
            timepoint: TimeIndex(0),
            displayed_scale_level: Some(0),
            brick_stream_generation: 7,
            layer_ids: vec!["layer".to_owned()],
        }
    }

    #[test]
    fn gpu_display_texture_handoff_uses_linear_filtering() {
        assert_eq!(
            gpu_display_texture_filter(),
            eframe::wgpu::FilterMode::Linear
        );
    }

    #[test]
    fn presented_gpu_frame_compatibility_ignores_brick_generation_for_resident_modes() {
        let viewport = RenderViewport::new(16, 16).unwrap();
        let frame_identity = test_display_identity(RenderMode::Mip, viewport);
        let mut requested_identity = frame_identity.clone();

        assert!(gpu_presented_frame_compatible_for_pending_request(
            Some(&frame_identity),
            true,
            &requested_identity,
        ));

        requested_identity.brick_stream_generation =
            requested_identity.brick_stream_generation.saturating_add(1);

        assert!(gpu_presented_frame_compatible_for_pending_request(
            Some(&frame_identity),
            true,
            &requested_identity,
        ));
    }

    #[test]
    fn presented_gpu_frame_camera_metadata_is_content_identity() {
        let viewport = RenderViewport::new(16, 16).unwrap();
        let frame_identity = test_display_identity(RenderMode::Mip, viewport);
        let mut requested_identity = frame_identity.clone();
        requested_identity.camera.orbit_by(0.25, -0.1);

        assert_ne!(frame_identity.camera, requested_identity.camera);
        assert!(!gpu_presented_frame_compatible_for_pending_request(
            Some(&frame_identity),
            true,
            &requested_identity,
        ));
        assert_eq!(
            frame_identity.display_freshness_for_camera(
                requested_identity.camera,
                requested_identity.presentation_viewport
            ),
            DisplayedFrameFreshness::Stale
        );
        assert_eq!(
            requested_identity.display_freshness_for_camera(
                requested_identity.camera,
                requested_identity.presentation_viewport
            ),
            DisplayedFrameFreshness::Current
        );
    }

    #[test]
    fn presented_gpu_frame_presentation_metadata_is_content_identity() {
        let viewport = RenderViewport::new(16, 16).unwrap();
        let frame_identity = test_display_identity(RenderMode::Mip, viewport);
        let mut requested_identity = frame_identity.clone();
        requested_identity.presentation_viewport = PresentationViewport::new(640.0, 512.0).unwrap();

        assert!(!gpu_presented_frame_compatible_for_pending_request(
            Some(&frame_identity),
            true,
            &requested_identity,
        ));
        assert_eq!(
            frame_identity.display_freshness_for_camera(
                requested_identity.camera,
                requested_identity.presentation_viewport
            ),
            DisplayedFrameFreshness::Stale
        );
    }
}
