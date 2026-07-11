use super::*;
use crate::{
    cross_section_scheduler::{
        CROSS_SECTION_PANEL_RENDER_SUBMISSIONS_PER_PANEL_REFRESH, CrossSectionScheduleInput,
        mark_cross_section_panel_render_failed, mark_cross_section_panel_rendered,
        schedule_cross_section_panel,
    },
    dataset_requests::SCOPE_CURRENT_3D,
    image_compositing::color_image_for_snapshot,
    viewer_layout::PanelId,
};
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
            Self::CpuTexture => "loading texture",
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

impl ViewportDisplayImage {
    pub(crate) fn size_vec2(&self) -> egui::Vec2 {
        match self {
            Self::Cpu(texture) => texture.size_vec2(),
            Self::Gpu { size, .. } => *size,
        }
    }
}

pub(crate) fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

impl MiranteWorkbenchApp {
    pub(crate) fn ensure_texture(&mut self, ctx: &egui::Context) -> &egui::TextureHandle {
        if self.render_runtime.texture.is_none() {
            let snapshot = current_egui_shell_bridge::snapshot(&self.application);
            let image = color_image_for_snapshot(&snapshot, &self.render_runtime);
            self.render_runtime.texture = Some(ctx.load_texture(
                "mirante4d-loading-frame",
                image,
                egui::TextureOptions::NEAREST,
            ));
        }
        self.render_runtime
            .texture
            .as_ref()
            .expect("the loading texture was initialized")
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
        let panel = self.render_runtime.cross_section_runtime.panel(panel_id)?;
        let displayed = self
            .render_runtime
            .cross_section_gpu_display_frames
            .get(&panel_id)?;
        if displayed.generation != panel.generation || !panel.display_current() {
            return None;
        }
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
            .values()
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
        if self.render_runtime.gpu_renderer.is_none() {
            anyhow::bail!("GPU renderer is unavailable");
        }
        if !self
            .dataset
            .scope_complete(SCOPE_CURRENT_3D, &self.render_runtime.lease_bridge)
        {
            anyhow::bail!("current semantic lease set is incomplete");
        }
        let snapshot = current_egui_shell_bridge::snapshot(&self.application);
        if application_view(&snapshot)
            .layers()
            .iter()
            .all(|layer| !layer.visible())
        {
            anyhow::bail!("GPU display requires at least one visible layer");
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
        if let Some(texture_id) = existing_texture_id {
            texture_renderer.update_egui_texture_from_wgpu_texture(
                gpu_renderer.device(),
                frame.texture_view(),
                gpu_display_texture_filter(),
                texture_id,
            );
            Ok(texture_id)
        } else {
            Ok(texture_renderer.register_native_texture(
                gpu_renderer.device(),
                frame.texture_view(),
                gpu_display_texture_filter(),
            ))
        }
    }

    fn cross_section_panel_needs_display_render(&self, panel_id: PanelId) -> bool {
        let Some(panel) = self.render_runtime.cross_section_runtime.panel(panel_id) else {
            return false;
        };
        panel_id.cross_section_panel().is_some()
            && panel.render_failure.is_none()
            && self
                .render_runtime
                .cross_section_gpu_display_frames
                .get(&panel_id)
                .is_none_or(|displayed| {
                    displayed.generation != panel.generation || !panel.display_current()
                })
    }

    pub(crate) fn render_cross_section_panel_for_display_if_needed(
        &mut self,
        panel_id: PanelId,
    ) -> anyhow::Result<Option<DisplayRenderTiming>> {
        if !self.cross_section_panel_needs_display_render(panel_id)
            || CROSS_SECTION_PANEL_RENDER_SUBMISSIONS_PER_PANEL_REFRESH == 0
        {
            return Ok(None);
        }
        let snapshot = current_egui_shell_bridge::snapshot(&self.application);
        let view = application_view(&snapshot);
        let scope = match panel_id {
            PanelId::Xy => crate::dataset_requests::SCOPE_CROSS_SECTION_XY,
            PanelId::Xz => crate::dataset_requests::SCOPE_CROSS_SECTION_XZ,
            PanelId::Yz => crate::dataset_requests::SCOPE_CROSS_SECTION_YZ,
            PanelId::ThreeD => return Ok(None),
        };
        let requirements = self.dataset.scope_requirements(scope);
        let gpu_display_available = self.render_runtime.gpu_renderer.is_some()
            && self.ui_runtime.wgpu_texture_renderer.is_some();
        let schedule = schedule_cross_section_panel(
            &mut self.render_runtime,
            CrossSectionScheduleInput {
                catalog: snapshot.catalog(),
                view,
                active_layer: view.active_layer(),
                requirements,
                render_scale: self.dataset.current_scale(),
                dataset_failed: self.dataset.dispatcher().scope_failure(scope).is_some(),
            },
            panel_id,
            gpu_display_available,
        )?
        .schedule;
        if !schedule.is_renderable() {
            return Ok(None);
        }
        let gpu_renderer = self
            .render_runtime
            .gpu_renderer
            .clone()
            .ok_or_else(|| anyhow::anyhow!("GPU renderer is unavailable"))?;
        let render_start = Instant::now();
        let rendered = match render_gpu_cross_section_panel_frame_from_global_runtime(
            &snapshot,
            &self.dataset,
            &self.render_runtime,
            gpu_renderer.as_ref(),
            panel_id,
        ) {
            Ok(rendered) => rendered,
            Err(error) => {
                let failure = render_state::render_failure_status(&error);
                mark_cross_section_panel_render_failed(
                    &mut self.render_runtime,
                    panel_id,
                    schedule,
                    failure,
                );
                return Err(error);
            }
        };
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
            if existing_texture_id != Some(texture_id) {
                self.ui_runtime
                    .retired_gpu_display_texture_ids
                    .push(texture_id);
            }
            anyhow::bail!("stale cross-section frame was suppressed");
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
        let snapshot = current_egui_shell_bridge::snapshot(&self.application);
        let render_start = Instant::now();
        let mut frame = render_gpu_display_frame_from_resident_bricks(
            &snapshot,
            &self.dataset,
            &mut self.render_runtime,
            gpu_renderer.as_ref(),
        )?;
        if application_view(&snapshot).layout() == ViewerLayout::FourPanel {
            frame = gpu_renderer.detach_display_frame_texture(frame)?;
        }
        let render_ms = duration_ms(render_start.elapsed());
        let texture_start = Instant::now();
        self.register_or_update_gpu_display_texture(&frame)?;
        let egui_texture_ms = duration_ms(texture_start.elapsed());
        let gpu_upload_ms = frame.timings.upload_ms();
        let gpu_compute_ms = frame.timings.gpu_compute_ms();
        let display_identity = GpuDisplayedFrameIdentity::from_snapshot(
            &snapshot,
            &self.dataset,
            &self.render_runtime,
        )?;
        self.render_runtime.frame_fidelity.display_freshness = display_identity
            .display_freshness_for_snapshot(&snapshot, &self.dataset, &self.render_runtime)?;
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

    pub(crate) fn render_current_resident_frame_for_display(
        &mut self,
    ) -> anyhow::Result<DisplayRenderTiming> {
        self.render_gpu_display_frame_for_current_state()
    }

    pub(crate) fn rerender_display_state(&mut self) -> anyhow::Result<DisplayRenderTiming> {
        self.request_visible_bricks();
        if self.dataset.scope_is_empty(SCOPE_CURRENT_3D) {
            self.clear_gpu_display_frame();
            self.retire_gpu_display_texture_id();
            self.render_runtime.render_backend = RenderBackend::Empty;
            self.render_runtime.frame_fidelity.backend = RenderBackend::Empty;
            self.render_runtime.frame_fidelity.completeness = FrameCompleteness::Complete;
            self.render_runtime.frame_fidelity.reason = LodDecisionReason::NoVisibleData;
            self.render_runtime.frame_fidelity.display_freshness = DisplayedFrameFreshness::Current;
            self.render_runtime.texture = None;
            return Ok(DisplayRenderTiming {
                path: DisplayRefreshPath::CpuTexture,
                render_ms: 0.0,
                gpu_upload_ms: None,
                gpu_compute_ms: None,
                egui_texture_ms: 0.0,
            });
        }
        if self
            .dataset
            .scope_complete(SCOPE_CURRENT_3D, &self.render_runtime.lease_bridge)
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
        self.render_runtime.render_backend = RenderBackend::Loading;
        self.render_runtime.frame_fidelity.backend = RenderBackend::Loading;
        self.render_runtime.frame_fidelity.completeness = FrameCompleteness::Loading;
        self.render_runtime.frame_fidelity.reason = LodDecisionReason::LoadingTargetScale;
        self.render_runtime.texture = None;
        Ok(DisplayRenderTiming {
            path: DisplayRefreshPath::CpuTexture,
            render_ms: 0.0,
            gpu_upload_ms: None,
            gpu_compute_ms: None,
            egui_texture_ms: 0.0,
        })
    }

    fn can_preserve_gpu_presented_frame_for_pending_request(&self) -> bool {
        if self.render_runtime.gpu_display_frame.is_none() {
            return false;
        }
        let Ok(requested) = GpuDisplayedFrameIdentity::from_snapshot(
            &current_egui_shell_bridge::snapshot(&self.application),
            &self.dataset,
            &self.render_runtime,
        ) else {
            return false;
        };
        gpu_presented_frame_compatible_for_pending_request(
            self.render_runtime.gpu_display_frame_identity.as_ref(),
            self.ui_runtime.gpu_display_texture_id.is_some(),
            &requested,
        )
    }

    fn mark_target_pending_while_preserving_gpu_frame(&mut self) {
        self.render_runtime.frame_fidelity.completeness = FrameCompleteness::Loading;
        self.render_runtime.frame_fidelity.reason = LodDecisionReason::LoadingTargetScale;
        self.render_runtime.frame_fidelity.display_freshness = self
            .render_runtime
            .gpu_display_frame_identity
            .as_ref()
            .map(|identity| {
                identity
                    .display_freshness_for_snapshot(
                        &current_egui_shell_bridge::snapshot(&self.application),
                        &self.dataset,
                        &self.render_runtime,
                    )
                    .unwrap_or(DisplayedFrameFreshness::Unknown)
            })
            .unwrap_or(DisplayedFrameFreshness::Unknown);
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
        let started = Instant::now();
        if self.render_runtime.gpu_display_frame.is_none() {
            let image = color_image_for_snapshot(
                &current_egui_shell_bridge::snapshot(&self.application),
                &self.render_runtime,
            );
            if let Some(texture) = self.render_runtime.texture.as_mut() {
                texture.set(image, egui::TextureOptions::NEAREST);
            }
        }
        duration_ms(started.elapsed())
    }

    pub(crate) fn refresh_frame(&mut self, ctx: &egui::Context) {
        let total_start = Instant::now();
        match self.rerender_display_state() {
            Ok(render_timing) => {
                let cpu_texture_update_ms = self.update_cpu_texture_if_needed();
                self.record_display_refresh_timing(
                    render_timing,
                    0.0,
                    cpu_texture_update_ms,
                    duration_ms(total_start.elapsed()),
                );
            }
            Err(error) => {
                tracing::error!(%error, "GPU display refresh failed");
                let failure = render_state::render_failure_status(&error);
                self.render_runtime.frame_fidelity.last_failure_kind = Some(failure.kind);
                self.render_runtime.frame_fidelity.last_capacity_error = Some(failure.message);
                self.render_runtime.frame_fidelity.completeness = FrameCompleteness::Incomplete;
            }
        }
        ctx.request_repaint();
    }

    pub(crate) fn refresh_texture_only(&mut self, ctx: &egui::Context) {
        self.invalidate_cross_section_panel_display_frames();
        self.refresh_frame(ctx);
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
    frame_identity.is_some_and(|identity| {
        texture_registered && identity.compatible_with_pending_request(requested_identity)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_display_texture_handoff_uses_linear_filtering() {
        assert_eq!(
            gpu_display_texture_filter(),
            eframe::wgpu::FilterMode::Linear
        );
    }
}
