use std::sync::Arc;

use super::*;
use crate::{
    cross_section_scheduler::{
        CROSS_SECTION_PANEL_RENDER_SUBMISSIONS_PER_PANEL_REFRESH, CrossSectionScheduleInput,
        mark_cross_section_panel_render_failed, mark_cross_section_panel_rendered,
        schedule_cross_section_panel,
    },
    dataset_requests::{
        SCOPE_CROSS_SECTION_XY, SCOPE_CROSS_SECTION_XZ, SCOPE_CROSS_SECTION_YZ, SCOPE_CURRENT_3D,
    },
    native_presentation::ProductPresentationTarget,
    product_render_intent::{ProductRenderRequest, cross_section_request, volume_request},
    viewer_layout::PanelId,
};
use mirante4d_dataset::{DatasetResourceKey, ResourceLease};
use mirante4d_domain::RenderMode;
use mirante4d_render_api::{
    FrameCompleteness as RenderFrameCompleteness, FrameIdentity, FrameLimitation, RenderExtent,
};
use mirante4d_render_wgpu::WgpuRenderRuntimeError;

#[derive(Clone)]
pub(crate) enum ViewportDisplayImage {
    UiBackground {
        size: egui::Vec2,
    },
    Presentation {
        slot: PresentationSlot,
        size: egui::Vec2,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisplayRefreshPath {
    GpuResidentDisplay,
    UiBackground,
}

impl DisplayRefreshPath {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::GpuResidentDisplay => "gpu display",
            Self::UiBackground => "ui background",
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
    pub(crate) total_ms: f64,
}

impl ViewportDisplayImage {
    pub(crate) fn size_vec2(&self) -> egui::Vec2 {
        match self {
            Self::UiBackground { size } => *size,
            Self::Presentation { size, .. } => *size,
        }
    }
}

pub(crate) fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

impl MiranteWorkbenchApp {
    pub(crate) fn application_snapshot_for_ui(&self) -> ApplicationSnapshot {
        let snapshot = current_egui_shell_bridge::snapshot(&self.application);
        let three_d = Some(self.presentation_surface(
            PanelId::ThreeD,
            self.render_runtime.presentation_viewport,
            true,
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
        let panel = self.render_runtime.cross_section_runtime.panel(panel_id)?;
        Some(self.presentation_surface(
            panel_id,
            panel.presentation_viewport?,
            panel.display_current(),
        ))
    }

    fn presentation_surface(
        &self,
        panel_id: PanelId,
        viewport: PresentationViewport,
        frame_is_current: bool,
    ) -> PresentationSurface {
        let frame = frame_is_current
            .then(|| {
                self.native_presentation
                    .product_gpu
                    .as_ref()?
                    .targets
                    .get(&panel_id)?
                    .presented
                    .clone()
            })
            .flatten();
        PresentationSurface::new(viewport, frame)
    }

    pub(crate) fn viewport_display_image(
        &self,
        snapshot: &ApplicationSnapshot,
    ) -> ViewportDisplayImage {
        if let Some(extent) = self.product_display(snapshot, PresentationSlot::ThreeD) {
            return ViewportDisplayImage::Presentation {
                slot: PresentationSlot::ThreeD,
                size: extent_size(extent),
            };
        }
        ViewportDisplayImage::UiBackground {
            size: extent_size(self.render_runtime.render_viewport),
        }
    }

    pub(crate) fn cross_section_panel_display_image(
        &self,
        panel_id: PanelId,
        snapshot: &ApplicationSnapshot,
    ) -> Option<ViewportDisplayImage> {
        let slot = match panel_id {
            PanelId::Xy => PresentationSlot::Xy,
            PanelId::Xz => PresentationSlot::Xz,
            PanelId::Yz => PresentationSlot::Yz,
            PanelId::ThreeD => return None,
        };
        let extent = self.product_display(snapshot, slot)?;
        Some(ViewportDisplayImage::Presentation {
            slot,
            size: extent_size(extent),
        })
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
        if let Some(product) = self.native_presentation.product_gpu.as_mut()
            && let Some(target) = product.targets.get_mut(&PanelId::ThreeD)
        {
            target.request = None;
            target.presented = None;
            target.pending_capture = None;
            target.completed_capture = None;
            target.partial_seen = false;
        }
        self.render_runtime.frame_fidelity.display_freshness = DisplayedFrameFreshness::Unknown;
    }

    pub(crate) fn clear_cross_section_product_presentations(&mut self) {
        if let Some(product) = self.native_presentation.product_gpu.as_mut() {
            for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
                if let Some(target) = product.targets.get_mut(&panel) {
                    target.request = None;
                    target.presented = None;
                    target.pending_capture = None;
                    target.completed_capture = None;
                    target.partial_seen = false;
                }
            }
        }
    }

    pub(crate) fn invalidate_cross_section_panel_display_frames(&mut self) {
        self.render_runtime
            .cross_section_runtime
            .mark_cross_section_panels_dirty();
    }

    pub(crate) fn clear_product_presentations(&mut self) {
        if let Some(product) = self.native_presentation.product_gpu.as_mut() {
            for target in product.targets.values_mut() {
                target.request = None;
                target.presented = None;
                target.pending_capture = None;
                target.completed_capture = None;
                target.partial_seen = false;
            }
        }
        self.render_runtime.frame_fidelity.display_freshness = DisplayedFrameFreshness::Unknown;
    }

    fn cross_section_panel_needs_display_render(&self, panel_id: PanelId) -> bool {
        let Some(panel) = self.render_runtime.cross_section_runtime.panel(panel_id) else {
            return false;
        };
        let target_is_progressive = self
            .native_presentation
            .product_gpu
            .as_ref()
            .and_then(|product| product.targets.get(&panel_id))
            .and_then(|target| target.presented.as_ref())
            .is_some_and(|frame| {
                frame.progress().completeness() == RenderFrameCompleteness::Progressive
            });
        panel_id.cross_section_panel().is_some()
            && panel.render_failure.is_none()
            && (!panel.display_current() || target_is_progressive)
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
        let scope = cross_section_scope(panel_id)?;
        let requirements = self.dataset.scope_requirements(scope).to_vec();
        let gpu_available = self.native_presentation.product_gpu.is_some();
        let schedule = schedule_cross_section_panel(
            &mut self.render_runtime,
            CrossSectionScheduleInput {
                catalog: snapshot.catalog(),
                view,
                active_layer: view.active_layer(),
                requirements: &requirements,
                retained_leases: self.dataset.retained_leases(),
                render_scale: self.dataset.current_scale(),
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
            .render_runtime
            .cross_section_runtime
            .panel(panel_id)
            .ok_or_else(|| anyhow::anyhow!("cross-section panel state is unavailable"))?;
        let presentation = panel
            .presentation_viewport
            .ok_or_else(|| anyhow::anyhow!("cross-section presentation viewport is unavailable"))?;
        let extent = panel
            .render_viewport
            .ok_or_else(|| anyhow::anyhow!("cross-section render viewport is unavailable"))?;
        let generation = panel.generation;
        let render_start = Instant::now();
        let rendered = match self.render_product_target(
            panel_id,
            Some(panel_id),
            &snapshot,
            presentation,
            extent,
            &requirements,
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
        if !rendered {
            return Ok(None);
        }
        if !self
            .render_runtime
            .cross_section_runtime
            .mark_panel_displayed(panel_id, generation)
        {
            anyhow::bail!("stale cross-section frame was suppressed");
        }
        mark_cross_section_panel_rendered(&mut self.render_runtime, panel_id, schedule);
        Ok(Some(DisplayRenderTiming {
            path: DisplayRefreshPath::GpuResidentDisplay,
            render_ms: duration_ms(render_start.elapsed()),
            gpu_upload_ms: None,
            gpu_compute_ms: None,
            egui_texture_ms: 0.0,
        }))
    }

    fn render_product_target(
        &mut self,
        target_id: PanelId,
        cross_section: Option<PanelId>,
        snapshot: &ApplicationSnapshot,
        presentation: PresentationViewport,
        extent: RenderExtent,
        resources: &[DatasetResourceKey],
    ) -> anyhow::Result<bool> {
        self.ensure_product_target(target_id, extent)?;
        let current_frame = self
            .native_presentation
            .product_gpu
            .as_ref()
            .and_then(|product| product.targets.get(&target_id))
            .and_then(|target| target.request.as_ref())
            .map_or(FrameIdentity::new(1), |request| request.intent.frame());
        let mut next = build_product_request(
            snapshot,
            current_frame,
            cross_section,
            presentation,
            extent,
            resources,
        )?;
        let changed = self
            .native_presentation
            .product_gpu
            .as_ref()
            .and_then(|product| product.targets.get(&target_id))
            .and_then(|target| target.request.as_ref())
            != next.as_ref();
        if changed {
            let frame = self
                .native_presentation
                .product_gpu
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?
                .allocate_frame_identity();
            next = build_product_request(
                snapshot,
                frame,
                cross_section,
                presentation,
                extent,
                resources,
            )?;
            let target = self
                .native_presentation
                .product_gpu
                .as_mut()
                .and_then(|product| product.targets.get_mut(&target_id))
                .expect("the product target was registered before request construction");
            target.request = next;
            target.presented = None;
            target.pending_capture = None;
            target.completed_capture = None;
        } else if !self.poll_product_target_validation_capture(target_id)? {
            if target_id == PanelId::ThreeD {
                self.render_runtime.lod_replan_pending = true;
            }
            return Ok(false);
        }
        let Some(request) = self
            .native_presentation
            .product_gpu
            .as_ref()
            .and_then(|product| product.targets.get(&target_id))
            .and_then(|target| target.request.clone())
        else {
            return Ok(false);
        };
        let keys = request
            .requirements
            .resources()
            .iter()
            .map(|requirement| requirement.key())
            .collect::<Vec<_>>();
        let leases = self.dataset.retained_leases().lease_handles(&keys);
        let lease_refs = leases
            .iter()
            .map(|lease| Arc::as_ref(lease) as &dyn ResourceLease)
            .collect::<Vec<_>>();
        let product = self
            .native_presentation
            .product_gpu
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
        let token = product
            .targets
            .get(&target_id)
            .expect("the product target was registered")
            .token;
        let report = match product.renderer.execute_frame(
            token,
            snapshot.catalog(),
            &request.intent,
            &request.requirements,
            &lease_refs,
        ) {
            Ok(report) => report,
            Err(error @ WgpuRenderRuntimeError::StaleFrame { .. }) => {
                product.stale_frames_rejected = product.stale_frames_rejected.saturating_add(1);
                tracing::debug!(%error, "stale product frame was rejected");
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
        };
        let Some(presented) = report.presentation().cloned() else {
            if target_id == PanelId::ThreeD && report.uploaded_resources() > 0 {
                self.render_runtime.lod_replan_pending = true;
            }
            return Ok(false);
        };
        let partial_seen = product
            .targets
            .get(&target_id)
            .is_some_and(|target| target.partial_seen);
        let current_is_partial =
            presented.progress().completeness() == RenderFrameCompleteness::Progressive;
        if current_is_partial && !partial_seen {
            product.current_partial_frames_presented =
                product.current_partial_frames_presented.saturating_add(1);
        }
        if !current_is_partial && partial_seen {
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
        target.partial_seen = current_is_partial;
        if let Some(ticket) = report.validation_capture() {
            target.pending_capture = Some((presented.clone(), ticket));
            target.completed_capture = None;
        }
        if target_id == PanelId::ThreeD && current_is_partial {
            self.render_runtime.lod_replan_pending = true;
        }
        self.bind_product_texture(target_id, extent_changed)?;
        if target_id == PanelId::ThreeD {
            self.record_product_frame(&presented);
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
            ProductPresentationTarget {
                token: registration.token(),
                extent,
                request: None,
                presented: None,
                pending_capture: None,
                completed_capture: None,
                partial_seen: false,
            },
        );
        Ok(())
    }

    pub(crate) fn poll_product_validation_captures(&mut self) -> anyhow::Result<()> {
        let pending = self
            .native_presentation
            .product_gpu
            .as_ref()
            .into_iter()
            .flat_map(|product| product.targets.iter())
            .filter_map(|(panel, target)| target.pending_capture.as_ref().map(|_| *panel))
            .collect::<Vec<_>>();
        for panel in pending {
            self.poll_product_target_validation_capture(panel)?;
        }
        Ok(())
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

    fn record_product_frame(&mut self, frame: &mirante4d_render_api::PresentedFrame) {
        let progress = frame.progress();
        let coverage = progress.coverage();
        self.render_runtime.frame_fidelity.resident_bricks =
            usize::try_from(coverage.available_requirements()).unwrap_or(usize::MAX);
        self.render_runtime.frame_fidelity.missing_occupied_bricks = usize::try_from(
            coverage
                .total_requirements()
                .saturating_sub(coverage.available_requirements()),
        )
        .unwrap_or(usize::MAX);
        self.render_runtime.frame_fidelity.completeness = match progress.completeness() {
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
        self.render_runtime.frame_fidelity.reason = match progress.limitation() {
            Some(FrameLimitation::BudgetLimited | FrameLimitation::CapacityLimited) => {
                LodDecisionReason::GpuBudgetLimited
            }
            Some(FrameLimitation::CoarserScale) => LodDecisionReason::ScreenEquivalentCoarserScale,
            Some(FrameLimitation::MissingResources) => LodDecisionReason::IncompleteResidency,
            None if self.dataset.current_scale().get() == 0 => LodDecisionReason::ExactS0,
            None => LodDecisionReason::ScreenEquivalentCoarserScale,
        };
        let mode = application_view(&current_egui_shell_bridge::snapshot(&self.application))
            .layer(
                application_view(&current_egui_shell_bridge::snapshot(&self.application))
                    .active_layer(),
            )
            .expect("the current view contains its active layer")
            .render_state()
            .mode();
        self.render_runtime.frame_fidelity.backend = render_backend_for_mode(mode);
        self.render_runtime.frame_fidelity.display_freshness = DisplayedFrameFreshness::Current;
        self.render_runtime.lod_schedule.displayed_scale_level =
            Some(self.dataset.current_scale().get());
        self.render_runtime.frame_fidelity.displayed_scale_level =
            self.render_runtime.lod_schedule.displayed_scale_level;
    }

    pub(crate) fn render_current_product_frame(&mut self) -> anyhow::Result<DisplayRenderTiming> {
        let snapshot = current_egui_shell_bridge::snapshot(&self.application);
        let requirements = self.dataset.scope_requirements(SCOPE_CURRENT_3D).to_vec();
        let presentation = self.render_runtime.presentation_viewport;
        let extent = self.render_runtime.render_viewport;
        let started = Instant::now();
        let rendered = self.render_product_target(
            PanelId::ThreeD,
            None,
            &snapshot,
            presentation,
            extent,
            &requirements,
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

    pub(crate) fn rerender_display_state(&mut self) -> anyhow::Result<DisplayRenderTiming> {
        self.request_visible_bricks();
        if self.dataset.scope_is_empty(SCOPE_CURRENT_3D) {
            self.clear_3d_product_presentation();
            self.render_runtime.frame_fidelity.backend = RenderBackend::Empty;
            self.render_runtime.frame_fidelity.completeness = FrameCompleteness::Complete;
            self.render_runtime.frame_fidelity.reason = LodDecisionReason::NoVisibleData;
            self.render_runtime.frame_fidelity.display_freshness = DisplayedFrameFreshness::Current;
            return Ok(DisplayRenderTiming {
                path: DisplayRefreshPath::UiBackground,
                render_ms: 0.0,
                gpu_upload_ms: None,
                gpu_compute_ms: None,
                egui_texture_ms: 0.0,
            });
        }
        self.render_current_product_frame()
    }

    pub(crate) fn record_display_refresh_timing(
        &mut self,
        render: DisplayRenderTiming,
        visible_brick_request_ms: f64,
        total_ms: f64,
    ) {
        self.render_runtime.last_display_refresh_timing = Some(DisplayRefreshTiming {
            path: render.path,
            render_ms: render.render_ms,
            gpu_upload_ms: render.gpu_upload_ms,
            gpu_compute_ms: render.gpu_compute_ms,
            egui_texture_ms: render.egui_texture_ms,
            visible_brick_request_ms,
            total_ms,
        });
    }

    pub(crate) fn refresh_frame(&mut self, ctx: &egui::Context) {
        let total_start = Instant::now();
        match self.rerender_display_state() {
            Ok(render_timing) => {
                self.record_display_refresh_timing(
                    render_timing,
                    0.0,
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

fn build_product_request(
    snapshot: &ApplicationSnapshot,
    frame: FrameIdentity,
    cross_section: Option<PanelId>,
    presentation: PresentationViewport,
    extent: RenderExtent,
    resources: &[DatasetResourceKey],
) -> anyhow::Result<Option<ProductRenderRequest>> {
    match cross_section {
        Some(panel) => {
            cross_section_request(snapshot, frame, panel, presentation, extent, resources)
        }
        None => volume_request(snapshot, frame, presentation, extent, resources),
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

fn extent_size(extent: RenderExtent) -> egui::Vec2 {
    egui::vec2(extent.width_pixels() as f32, extent.height_pixels() as f32)
}

pub(crate) fn render_backend_for_mode(mode: RenderMode) -> RenderBackend {
    match mode {
        RenderMode::Mip => RenderBackend::GpuCameraMip,
        RenderMode::Isosurface => RenderBackend::GpuCameraIso,
        RenderMode::Dvr => RenderBackend::GpuCameraDvr,
    }
}
