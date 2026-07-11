use super::*;
use crate::brick_streaming::{current_resident_frame_ready, stream_layer_ids_for_snapshot};
use crate::cross_section_runtime::CrossSectionLayerInput;
use crate::cross_section_scheduler::{
    CROSS_SECTION_PANEL_RENDER_SUBMISSIONS_PER_PANEL_REFRESH, CrossSectionScheduleInput,
    cross_section_panel_refinement_due, mark_cross_section_panel_render_failed,
    mark_cross_section_panel_rendered, schedule_cross_section_panel,
};
use crate::cross_section_streaming::{
    CrossSectionStreamingInput, submit_cross_section_visible_chunks_to_read_queue,
};
use crate::image_compositing::color_image_for_snapshot;
use crate::scene_extraction::{SceneViewInput, scene_draw_list};
use crate::viewer_layout::PanelId;
use mirante4d_domain::ViewerLayout;

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
    fn capture(render: &current_runtime::render::CurrentRenderRuntime) -> Self {
        Self {
            frame: render.frame.clone(),
            frame_f32: render.frame_f32.clone(),
            diagnostics: render.diagnostics,
            diagnostics_f32: render.diagnostics_f32,
            render_backend: render.render_backend,
            frame_fidelity: render.frame_fidelity.clone(),
            channel_fidelity: render.channel_fidelity.clone(),
            lod_schedule: render.lod_schedule,
            rendered_channels: render.rendered_channels.clone(),
        }
    }

    fn restore(self, render: &mut current_runtime::render::CurrentRenderRuntime) {
        render.frame = self.frame;
        render.frame_f32 = self.frame_f32;
        render.diagnostics = self.diagnostics;
        render.diagnostics_f32 = self.diagnostics_f32;
        render.render_backend = self.render_backend;
        render.frame_fidelity = self.frame_fidelity;
        render.channel_fidelity = self.channel_fidelity;
        render.lod_schedule = self.lod_schedule;
        render.rendered_channels = self.rendered_channels;
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
        if self.render_runtime.texture.is_none() {
            let snapshot = current_egui_shell_bridge::snapshot(&self.application);
            let image = color_image_for_snapshot(
                &snapshot,
                &self.dataset_runtime,
                &self.analysis_runtime,
                &self.ui_runtime,
                &self.render_runtime,
            );
            self.render_runtime.texture =
                Some(ctx.load_texture("mirante4d-mip", image, egui::TextureOptions::NEAREST));
        }
        self.render_runtime
            .texture
            .as_ref()
            .expect("texture was initialized")
    }

    pub(crate) fn viewport_display_image(&mut self, ctx: &egui::Context) -> ViewportDisplayImage {
        if let (Some(frame), Some(texture_id)) = (
            self.render_runtime.gpu_display_frame.as_ref(),
            self.ui_runtime.gpu_display_texture_id,
        ) {
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
        let displayed = self
            .render_runtime
            .cross_section_gpu_display_frames
            .get(&panel_id)?;
        Some(ViewportDisplayImage::Gpu {
            texture_id: displayed.texture_id,
            size: egui::vec2(
                displayed.frame.viewport.width as f32,
                displayed.frame.viewport.height as f32,
            ),
        })
    }

    pub(crate) fn clear_gpu_display_frame(&mut self) {
        self.render_runtime.gpu_display_frame = None;
        self.render_runtime.gpu_display_frame_identity = None;
        self.render_runtime.frame_fidelity.display_freshness = DisplayedFrameFreshness::Unknown;
    }

    pub(crate) fn retire_gpu_display_texture_id(&mut self) {
        if let Some(texture_id) = self.ui_runtime.gpu_display_texture_id.take() {
            self.ui_runtime
                .retired_gpu_display_texture_ids
                .push(texture_id);
        }
    }

    pub(crate) fn retire_cross_section_gpu_display_texture_ids(&mut self) {
        for displayed in self
            .render_runtime
            .cross_section_gpu_display_frames
            .values_mut()
        {
            self.ui_runtime
                .retired_gpu_display_texture_ids
                .push(displayed.texture_id);
        }
        self.render_runtime.cross_section_gpu_display_frames.clear();
    }

    pub(crate) fn invalidate_cross_section_panel_display_frames(&mut self) {
        self.render_runtime
            .cross_section_runtime
            .mark_cross_section_panels_dirty();
    }

    pub(crate) fn free_retired_gpu_display_textures(&mut self) {
        if self.ui_runtime.retired_gpu_display_texture_ids.is_empty() {
            return;
        }
        let Some(texture_renderer) = self.ui_runtime.wgpu_texture_renderer.as_ref() else {
            self.ui_runtime.retired_gpu_display_texture_ids.clear();
            return;
        };
        let mut texture_renderer = texture_renderer.write();
        for texture_id in self.ui_runtime.retired_gpu_display_texture_ids.drain(..) {
            texture_renderer.free_texture(&texture_id);
        }
    }

    fn ensure_gpu_display_path_ready(&self) -> anyhow::Result<()> {
        let snapshot = current_egui_shell_bridge::snapshot(&self.application);
        let view = application_view(&snapshot);
        let active_mode = view
            .layer(view.active_layer())
            .expect("application view has an active layer")
            .render_state()
            .mode();
        if !resident_brick_render_supported(active_mode) {
            anyhow::bail!(
                "GPU resident display does not support the active mode {:?}",
                active_mode
            );
        }
        if !self.dataset_runtime.brick_stream_complete
            || !current_resident_frame_ready(&snapshot, &self.dataset_runtime, &self.render_runtime)
        {
            anyhow::bail!("resident brick set is incomplete for GPU resident display");
        }
        let active_layer_id =
            current_physical_layer_id(&self.dataset_runtime, view.active_layer())?;
        scene_draw_list(
            &self.analysis_runtime,
            &self.ui_runtime,
            SceneViewInput {
                active_layer_id: &active_layer_id,
                active_timepoint: view.timepoint(),
                active_source_grid_to_world: snapshot
                    .catalog()
                    .layer(view.active_layer())
                    .expect("application view closes over the dataset catalog")
                    .grid_to_world(),
                camera: *view.camera(),
            },
        )?;

        let layer_ids = stream_layer_ids_for_snapshot(&snapshot, &self.dataset_runtime)?;
        if layer_ids.is_empty() {
            anyhow::bail!("GPU resident display requires at least one visible resident layer");
        }
        for layer_id in &layer_ids {
            if !self
                .dataset_runtime
                .dataset
                .manifest()
                .layers
                .iter()
                .any(|layer| layer.id == layer_id.as_str())
            {
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
            self.register_or_update_gpu_texture_id(frame, self.ui_runtime.gpu_display_texture_id)?;
        self.ui_runtime.gpu_display_texture_id = Some(texture_id);
        Ok(texture_id)
    }

    fn register_or_update_gpu_texture_id(
        &self,
        frame: &GpuDisplayFrame,
        existing_texture_id: Option<egui::TextureId>,
    ) -> anyhow::Result<egui::TextureId> {
        let gpu_renderer = self
            .render_runtime
            .gpu_renderer
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("GPU renderer is unavailable"))?;
        let Some(texture_renderer) = self.ui_runtime.wgpu_texture_renderer.as_ref() else {
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
        let Some(panel) = self.render_runtime.cross_section_runtime.panel(panel_id) else {
            return false;
        };
        if panel_id.cross_section_panel().is_none() {
            return false;
        }
        if panel.render_failure.is_some() {
            return false;
        }
        let stale_or_missing = match self
            .render_runtime
            .cross_section_gpu_display_frames
            .get(&panel_id)
        {
            Some(displayed) => displayed.generation != panel.generation || !panel.display_current(),
            None => true,
        };
        let incomplete_current = panel.display_current()
            && panel
                .cross_section_schedule
                .is_some_and(|schedule| schedule.missing_occupied_bricks > 0);
        stale_or_missing
            || incomplete_current
            || cross_section_panel_refinement_due(
                &self.dataset_runtime,
                &self.render_runtime,
                panel_id,
            )
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
        let gpu_display_available = self.render_runtime.gpu_renderer.is_some()
            && self.ui_runtime.wgpu_texture_renderer.is_some();
        let snapshot = current_egui_shell_bridge::snapshot(&self.application);
        let view = application_view(&snapshot);
        let active_layer_id =
            current_physical_layer_id(&self.dataset_runtime, view.active_layer())?;
        let layer_ids = stream_layer_ids_for_snapshot(&snapshot, &self.dataset_runtime)?;
        let visible_layers = view
            .layers()
            .iter()
            .filter(|layer| layer.visible())
            .collect::<Vec<_>>();
        if layer_ids.len() != visible_layers.len() {
            anyhow::bail!("visible logical and physical layer sets are inconsistent");
        }
        let layers = layer_ids
            .iter()
            .zip(visible_layers)
            .map(|(id, layer)| CrossSectionLayerInput {
                id,
                dtype: snapshot
                    .catalog()
                    .layer(layer.layer_key())
                    .expect("application view closes over the dataset catalog")
                    .dtype(),
            })
            .collect::<Vec<_>>();
        let active_panel = snapshot.transient().active_cross_section_panel();
        let gpu_budget_bytes = snapshot.resource_policy().gpu_budget_bytes();
        let schedule = schedule_cross_section_panel(
            &self.dataset_runtime,
            &mut self.render_runtime,
            CrossSectionScheduleInput {
                view,
                active_layer_id: &active_layer_id,
                layers: &layers,
                active_panel,
                gpu_budget_bytes,
            },
            panel_id,
            gpu_display_available,
        )?
        .schedule;
        if !schedule.is_renderable() {
            if gpu_display_available
                && let Some(pool) = &self.dataset_runtime.cross_section_read_pool
            {
                let submission = submit_cross_section_visible_chunks_to_read_queue(
                    &self.dataset_runtime,
                    &mut self.render_runtime,
                    CrossSectionStreamingInput {
                        view,
                        active_layer_id: &active_layer_id,
                        layers: &layers,
                        active_panel,
                        gpu_budget_bytes,
                    },
                    pool,
                )?;
                if submission.queued || submission.resident_changed {
                    self.dataset_runtime.brick_result_drain_last_repaint_reason =
                        Some("cross_section_panel_loading".to_owned());
                }
            }
            return Ok(None);
        }
        if schedule.missing_occupied_bricks > 0
            && gpu_display_available
            && let Some(pool) = &self.dataset_runtime.cross_section_read_pool
        {
            let submission = submit_cross_section_visible_chunks_to_read_queue(
                &self.dataset_runtime,
                &mut self.render_runtime,
                CrossSectionStreamingInput {
                    view,
                    active_layer_id: &active_layer_id,
                    layers: &layers,
                    active_panel,
                    gpu_budget_bytes,
                },
                pool,
            )?;
            if submission.queued || submission.resident_changed {
                self.dataset_runtime.brick_result_drain_last_repaint_reason =
                    Some("cross_section_panel_loading".to_owned());
            }
        }
        let gpu_renderer = self
            .render_runtime
            .gpu_renderer
            .clone()
            .ok_or_else(|| anyhow::anyhow!("GPU renderer is unavailable"))?;
        let render_generation = schedule.generation;
        self.render_runtime
            .cross_section_runtime
            .mark_panel_resident_chunks_upload_queued(panel_id, render_generation);
        let render_start = Instant::now();
        let rendered = match render_gpu_cross_section_panel_frame_from_global_runtime(
            &snapshot,
            &self.dataset_runtime,
            &self.render_runtime,
            gpu_renderer.as_ref(),
            panel_id,
        ) {
            Ok(rendered) => rendered,
            Err(err) => {
                let failure = render_state::render_failure_status(&err);
                self.render_runtime
                    .cross_section_runtime
                    .restore_panel_upload_queued_chunks_to_cpu_resident(
                        panel_id,
                        render_generation,
                    );
                mark_cross_section_panel_render_failed(
                    &mut self.render_runtime,
                    panel_id,
                    schedule,
                    failure,
                );
                return Err(err);
            }
        };
        self.render_runtime
            .cross_section_runtime
            .reconcile_panel_chunks_with_renderer_gpu_residency(
                rendered.panel_id,
                rendered.generation,
                &rendered.renderer_gpu_resident_chunks,
            );
        let render_ms = duration_ms(render_start.elapsed());
        let texture_start = Instant::now();
        let existing_texture_id = self
            .render_runtime
            .cross_section_gpu_display_frames
            .get(&panel_id)
            .map(|displayed| displayed.texture_id);
        let texture_id =
            self.register_or_update_gpu_texture_id(&rendered.frame, existing_texture_id)?;
        let egui_texture_ms = duration_ms(texture_start.elapsed());
        if !self
            .render_runtime
            .cross_section_runtime
            .mark_panel_displayed(rendered.panel_id, rendered.generation)
        {
            let mut stale_schedule = schedule;
            stale_schedule.reason =
                crate::viewer_layout::CrossSectionPanelScheduleReason::StaleGeneration;
            self.render_runtime
                .cross_section_runtime
                .set_panel_schedule(rendered.panel_id, stale_schedule);
            if existing_texture_id != Some(texture_id) {
                self.ui_runtime
                    .retired_gpu_display_texture_ids
                    .push(texture_id);
            }
            anyhow::bail!(
                "stale {} cross-section frame generation {} was not displayed",
                rendered.panel_id.label(),
                rendered.generation
            );
        }
        mark_cross_section_panel_rendered(&mut self.render_runtime, rendered.panel_id, schedule);
        let gpu_upload_ms = rendered.frame.timings.upload_ms();
        let gpu_compute_ms = rendered.frame.timings.gpu_compute_ms();
        self.render_runtime.cross_section_gpu_display_frames.insert(
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
            .render_runtime
            .gpu_renderer
            .clone()
            .ok_or_else(|| anyhow::anyhow!("GPU renderer is unavailable"))?;
        let previous_display_state = DisplayStateSnapshot::capture(&self.render_runtime);
        let render_start = Instant::now();
        let snapshot = current_egui_shell_bridge::snapshot(&self.application);
        let mut frame = match render_gpu_display_frame_from_resident_bricks(
            &snapshot,
            &self.dataset_runtime,
            &mut self.render_runtime,
            &self.analysis_runtime,
            &self.ui_runtime,
            gpu_renderer.as_ref(),
        ) {
            Ok(frame) => frame,
            Err(err) => {
                previous_display_state.restore(&mut self.render_runtime);
                return Err(err);
            }
        };
        if application_view(&snapshot).layout() == ViewerLayout::FourPanel {
            frame = match gpu_renderer.detach_display_frame_texture(frame) {
                Ok(frame) => frame,
                Err(err) => {
                    previous_display_state.restore(&mut self.render_runtime);
                    return Err(err.into());
                }
            };
        }
        let render_ms = duration_ms(render_start.elapsed());
        let texture_start = Instant::now();
        if let Err(err) = self.register_or_update_gpu_display_texture(&frame) {
            previous_display_state.restore(&mut self.render_runtime);
            return Err(err);
        }
        let egui_texture_ms = duration_ms(texture_start.elapsed());
        let gpu_upload_ms = frame.timings.upload_ms();
        let gpu_compute_ms = frame.timings.gpu_compute_ms();
        let display_identity = GpuDisplayedFrameIdentity::from_snapshot(
            &snapshot,
            &self.dataset_runtime,
            &self.render_runtime,
        )?;
        self.render_runtime.frame_fidelity.display_freshness = display_identity
            .display_freshness_for_snapshot(
                &snapshot,
                &self.dataset_runtime,
                &self.render_runtime,
            )?;
        self.render_runtime.gpu_display_frame_identity = Some(display_identity);
        self.render_runtime.gpu_display_frame = Some(frame);
        self.render_runtime.texture = None;
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
        let gpu_renderer = self.render_runtime.gpu_renderer.clone();
        render_state_from_resident_bricks_with_backend(
            &current_egui_shell_bridge::snapshot(&self.application),
            &self.dataset_runtime,
            &mut self.render_runtime,
            &self.analysis_runtime,
            &self.ui_runtime,
            gpu_renderer.as_deref(),
        )?;
        self.render_runtime.texture = None;
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
        if self.render_runtime.gpu_renderer.is_none()
            || self.ui_runtime.wgpu_texture_renderer.is_none()
        {
            return self.render_current_resident_frame_to_cpu_texture();
        }
        self.render_gpu_display_frame_for_current_state()
    }

    pub(crate) fn rerender_display_state(&mut self) -> anyhow::Result<DisplayRenderTiming> {
        let snapshot = current_egui_shell_bridge::snapshot(&self.application);
        update_visible_brick_plan(
            &snapshot,
            &mut self.dataset_runtime,
            &mut self.render_runtime,
        );
        if current_resident_frame_ready(&snapshot, &self.dataset_runtime, &self.render_runtime)
            && resident_brick_render_supported(
                application_view(&snapshot)
                    .layer(application_view(&snapshot).active_layer())
                    .expect("application view has an active layer")
                    .render_state()
                    .mode(),
            )
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
        let gpu_renderer = self.render_runtime.gpu_renderer.clone();
        rerender_state_with_backend(
            &snapshot,
            &mut self.dataset_runtime,
            &self.analysis_runtime,
            &self.ui_runtime,
            &mut self.render_runtime,
            gpu_renderer.as_deref(),
        )?;
        Ok(DisplayRenderTiming {
            path: DisplayRefreshPath::CpuTexture,
            render_ms: duration_ms(render_start.elapsed()),
            gpu_upload_ms: None,
            gpu_compute_ms: None,
            egui_texture_ms: 0.0,
        })
    }

    fn can_preserve_gpu_presented_frame_for_pending_request(&self) -> bool {
        if self.render_runtime.gpu_display_frame.is_none() {
            return false;
        }
        GpuDisplayedFrameIdentity::from_snapshot(
            &current_egui_shell_bridge::snapshot(&self.application),
            &self.dataset_runtime,
            &self.render_runtime,
        )
        .is_ok_and(|requested_identity| {
            gpu_presented_frame_compatible_for_pending_request(
                self.render_runtime.gpu_display_frame_identity.as_ref(),
                self.ui_runtime.gpu_display_texture_id.is_some(),
                &requested_identity,
            )
        })
    }

    fn mark_target_pending_while_preserving_gpu_frame(&mut self) {
        self.render_runtime.frame_fidelity.target_scale_level =
            self.render_runtime.lod_schedule.target_scale_level;
        self.render_runtime.frame_fidelity.viewport = self.render_runtime.render_viewport;
        self.render_runtime.frame_fidelity.presentation_viewport =
            self.render_runtime.presentation_viewport;
        if self.render_runtime.frame_fidelity.completeness != FrameCompleteness::BudgetLimited {
            self.render_runtime.frame_fidelity.completeness = FrameCompleteness::Loading;
            self.render_runtime.frame_fidelity.reason = LodDecisionReason::LoadingTargetScale;
        }
        // Keep the typed failure visible while the coarser/current request is
        // still pending. A successfully presented frame clears it.
        self.render_runtime.frame_fidelity.display_freshness = self
            .render_runtime
            .gpu_display_frame_identity
            .as_ref()
            .map(|identity| {
                identity
                    .display_freshness_for_snapshot(
                        &current_egui_shell_bridge::snapshot(&self.application),
                        &self.dataset_runtime,
                        &self.render_runtime,
                    )
                    .unwrap_or(DisplayedFrameFreshness::Unknown)
            })
            .unwrap_or(DisplayedFrameFreshness::Unknown);
        let snapshot = current_egui_shell_bridge::snapshot(&self.application);
        let gpu_renderer = self.render_runtime.gpu_renderer.clone();
        refresh_fidelity_resource_stats(
            &snapshot,
            &self.dataset_runtime,
            &mut self.render_runtime,
            gpu_renderer.as_deref(),
        );
        update_channel_fidelity_status(&snapshot, &self.dataset_runtime, &mut self.render_runtime);
    }

    pub(crate) fn record_display_refresh_timing(
        &mut self,
        render: DisplayRenderTiming,
        visible_brick_request_ms: f64,
        cpu_texture_update_ms: f64,
        total_ms: f64,
    ) {
        self.render_runtime.last_display_refresh_timing = Some(DisplayRefreshTiming {
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

    pub(crate) fn update_cpu_texture_if_needed(&mut self) -> f64 {
        let cpu_texture_start = Instant::now();
        if self.render_runtime.gpu_display_frame.is_none() && self.render_runtime.texture.is_some()
        {
            let image = color_image_for_snapshot(
                &current_egui_shell_bridge::snapshot(&self.application),
                &self.dataset_runtime,
                &self.analysis_runtime,
                &self.ui_runtime,
                &self.render_runtime,
            );
            if let Some(texture) = self.render_runtime.texture.as_mut() {
                texture.set(image, egui::TextureOptions::NEAREST);
            }
        }
        duration_ms(cpu_texture_start.elapsed())
    }

    pub(crate) fn refresh_frame(&mut self, ctx: &egui::Context) {
        let total_start = Instant::now();
        let mut visible_brick_request_ms = 0.0;
        match self.rerender_display_state() {
            Ok(render_timing) => {
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
                tracing::error!(error = %err, "camera render failed");
                let snapshot = current_egui_shell_bridge::snapshot(&self.application);
                if render_state::record_render_failure(
                    &snapshot,
                    &mut self.dataset_runtime,
                    &mut self.render_runtime,
                    &err,
                ) {
                    ctx.request_repaint();
                }
            }
        }
    }

    pub(crate) fn refresh_texture_only(&mut self, ctx: &egui::Context) {
        let total_start = Instant::now();
        self.invalidate_cross_section_panel_display_frames();
        if self.render_runtime.gpu_display_frame.is_some() {
            match self.rerender_display_state() {
                Ok(render_timing) => {
                    if self.render_runtime.gpu_display_frame.is_some() {
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
                    tracing::error!(error = %err, "texture refresh render failed");
                    let snapshot = current_egui_shell_bridge::snapshot(&self.application);
                    if render_state::record_render_failure(
                        &snapshot,
                        &mut self.dataset_runtime,
                        &mut self.render_runtime,
                        &err,
                    ) {
                        ctx.request_repaint();
                    }
                    return;
                }
            }
        }
        self.clear_gpu_display_frame();
        let cpu_texture_update_ms = self.update_cpu_texture_if_needed();
        self.render_runtime.last_display_refresh_timing = Some(DisplayRefreshTiming {
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
    use crate::display_graph::DisplayChannelModeIdentity;
    use mirante4d_domain::{
        DisplayWindow, DvrOpacityTransfer, GridToWorld, IsoShadingPolicy, RenderMode, RenderState,
        SamplingPolicy, Shape3D, TimeIndex, TransferCurve,
    };
    use mirante4d_format::LayerId;

    fn test_render_state(mode: RenderMode) -> RenderState {
        match mode {
            RenderMode::Mip => RenderState::mip(SamplingPolicy::SmoothLinear),
            RenderMode::Isosurface => RenderState::iso(
                SamplingPolicy::SmoothLinear,
                IsoShadingPolicy::GradientLighting,
                0.5,
            )
            .unwrap(),
            RenderMode::Dvr => RenderState::dvr(
                SamplingPolicy::SmoothLinear,
                DvrOpacityTransfer::new(
                    DisplayWindow::new(0.0, 1.0).unwrap(),
                    TransferCurve::linear(),
                ),
                12.0,
            )
            .unwrap(),
        }
    }

    fn test_display_identity(
        mode: RenderMode,
        viewport: RenderViewport,
    ) -> GpuDisplayedFrameIdentity {
        GpuDisplayedFrameIdentity {
            mode,
            channel_modes: vec![DisplayChannelModeIdentity {
                layer_id: LayerId::new("layer").unwrap(),
                render_state: test_render_state(mode),
            }],
            viewport,
            presentation_viewport: crate::viewport::default_presentation_viewport(),
            camera: crate::viewport::default_camera_for_shape(
                Shape3D::new(16, 16, 16).unwrap(),
                GridToWorld::identity(),
            ),
            timepoint: TimeIndex::new(0),
            displayed_scale_level: Some(0),
            brick_stream_generation: 7,
            layer_ids: vec![LayerId::new("layer").unwrap()],
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
        requested_identity.camera = crate::viewport::default_camera_for_shape(
            Shape3D::new(32, 16, 16).unwrap(),
            GridToWorld::identity(),
        );

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
