//! Latest-only native coordination for bounded asynchronous 3D picks.

use mirante4d_application::{
    ApplicationSnapshot, PresentationSlot,
    viewer_tools::{
        ActiveVolumeGeometry, PickHit, PickValue, ViewerTool, ViewerToolCommand, ViewerToolContext,
        ViewerToolEvent,
    },
};
use mirante4d_render_api::{FrameIdentity, PresentationTarget, RenderExtent, VolumePickResult};
use mirante4d_ui_egui::{
    ViewerPickPurpose, ViewerPickRequest, ViewportHover, ViewportIntensity, ViewportSampleKind,
};

use crate::{BACKGROUND_WORK_REPAINT_INTERVAL, MiranteWorkbenchApp, application_view};

const PICK_POLL_REPAINT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PresentedPickIdentity {
    target: PresentationTarget,
    frame: FrameIdentity,
    extent: RenderExtent,
}

/// Small immutable facts used to suppress a result after source, time, layer,
/// tool, presentation, frame, or extent changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ViewerPickCurrentness {
    context: ViewerToolContext,
    tool: ViewerTool,
    presented: Option<PresentedPickIdentity>,
}

impl ViewerPickCurrentness {
    pub(crate) fn from_snapshot(snapshot: &ApplicationSnapshot) -> Self {
        let view = snapshot.view();
        let presented = snapshot
            .presentations()
            .get(PresentationSlot::ThreeD)
            .and_then(|surface| surface.current_frame())
            .map(|frame| PresentedPickIdentity {
                target: frame.target(),
                frame: frame.frame(),
                extent: frame.extent(),
            });
        Self {
            context: ViewerToolContext::new(
                snapshot.source_generation(),
                view.timepoint(),
                view.active_layer(),
            ),
            tool: ViewerTool::from(snapshot.transient().active_tool()),
            presented,
        }
    }

    pub(crate) fn accepts(self, request: ViewerPickRequest) -> bool {
        let query = request.query();
        request.context() == self.context
            && request.tool() == self.tool
            && self.presented.is_some_and(|presented| {
                query.target() == presented.target
                    && query.frame() == presented.frame
                    && query.extent() == presented.extent
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PendingPick<Ticket> {
    ticket: Ticket,
    request: ViewerPickRequest,
}

/// Exactly one GPU request may be pending and exactly one latest replacement
/// may be queued. Primary clicks cannot be overwritten by hover traffic.
#[derive(Debug)]
pub(crate) struct ViewerPickQueue<Ticket> {
    pending: Option<PendingPick<Ticket>>,
    queued: Option<ViewerPickRequest>,
    last_completed: Option<ViewerPickRequest>,
    hover_interest: Option<ViewerPickRequest>,
    automation_interest: Option<ViewerPickRequest>,
    automation_completion: Option<(ViewerPickRequest, PickHit)>,
}

impl<Ticket> Default for ViewerPickQueue<Ticket> {
    fn default() -> Self {
        Self {
            pending: None,
            queued: None,
            last_completed: None,
            hover_interest: None,
            automation_interest: None,
            automation_completion: None,
        }
    }
}

impl<Ticket: Copy + Eq> ViewerPickQueue<Ticket> {
    /// Records the sole pick request produced by one UI frame. `None` means
    /// the pointer is no longer over an eligible 3D viewport; queued and
    /// completed hover work must then stop influencing visible tool state.
    pub(crate) fn observe_ui_request(&mut self, request: Option<ViewerPickRequest>) -> bool {
        let visible_hover_invalidated = self
            .last_completed
            .is_some_and(|completed| !same_pick_target(completed, request));
        if visible_hover_invalidated {
            self.last_completed = None;
        }
        self.hover_interest =
            request.filter(|request| request.purpose() == ViewerPickPurpose::Hover);
        match request {
            Some(request) => self.enqueue(request),
            None => {
                if self.queued.is_some_and(|request| {
                    request.purpose() == ViewerPickPurpose::Hover
                        && self.automation_interest != Some(request)
                }) {
                    self.queued = None;
                }
                if self
                    .last_completed
                    .is_some_and(|request| request.purpose() == ViewerPickPurpose::Hover)
                {
                    self.last_completed = None;
                }
            }
        }
        visible_hover_invalidated
    }

    /// Records a product-automation request without making it a second pick
    /// path. UI pointer departure may clear UI hover interest, but it must not
    /// erase this independently awaited request before the normal bounded GPU
    /// queue has submitted and drained it.
    pub(crate) fn enqueue_automation(&mut self, request: ViewerPickRequest) {
        if self.automation_interest != Some(request) {
            self.automation_interest = Some(request);
            self.automation_completion = None;
        }
        // A prior UI hover retained only the deduplication identity, not the
        // scientific result automation must report. Force one real completion
        // for the automation waiter even when both targets are identical.
        if self.last_completed == Some(request) {
            self.last_completed = None;
        }
        self.enqueue(request);
    }

    pub(crate) fn take_automation_completion_for(
        &mut self,
        purpose: ViewerPickPurpose,
    ) -> Option<(ViewerPickRequest, PickHit)> {
        let matches = self
            .automation_completion
            .as_ref()
            .is_some_and(|(completed, _)| completed.purpose() == purpose);
        if !matches {
            return None;
        }
        self.automation_interest = None;
        self.automation_completion.take()
    }

    pub(crate) fn enqueue(&mut self, request: ViewerPickRequest) {
        if self
            .pending
            .is_some_and(|pending| pending.request == request)
            || self.queued == Some(request)
            || (request.purpose() == ViewerPickPurpose::Hover
                && self.last_completed == Some(request))
        {
            return;
        }
        if self
            .queued
            .is_some_and(|queued| queued.purpose() == ViewerPickPurpose::PrimaryClick)
        {
            return;
        }
        self.queued = Some(request);
    }

    pub(crate) fn next_request(&self) -> Option<ViewerPickRequest> {
        if self.pending.is_none() {
            self.queued
        } else {
            None
        }
    }

    pub(crate) fn mark_submitted(&mut self, ticket: Ticket) -> bool {
        if self.pending.is_some() {
            return false;
        }
        let Some(request) = self.queued.take() else {
            return false;
        };
        self.pending = Some(PendingPick { ticket, request });
        true
    }

    pub(crate) fn pending(&self) -> Option<(Ticket, ViewerPickRequest)> {
        self.pending
            .map(|pending| (pending.ticket, pending.request))
    }

    pub(crate) fn finish_pending(&mut self, ticket: Ticket) -> Option<ViewerPickRequest> {
        if self.pending.is_some_and(|pending| pending.ticket == ticket) {
            self.pending.take().map(|pending| pending.request)
        } else {
            None
        }
    }

    pub(crate) fn record_completed(&mut self, request: ViewerPickRequest) {
        if request.purpose() == ViewerPickPurpose::Hover {
            self.last_completed = Some(request);
        }
    }

    pub(crate) fn automation_completion_is_wanted(&self, request: ViewerPickRequest) -> bool {
        self.automation_interest == Some(request)
    }

    pub(crate) fn record_automation_completion(
        &mut self,
        request: ViewerPickRequest,
        hit: PickHit,
    ) {
        if self.automation_completion_is_wanted(request) {
            self.automation_completion = Some((request, hit));
        }
    }

    pub(crate) fn discard_pending(&mut self, ticket: Ticket) -> Option<ViewerPickRequest> {
        if self.pending.is_some_and(|pending| pending.ticket == ticket) {
            self.pending.take().map(|pending| pending.request)
        } else {
            None
        }
    }

    pub(crate) fn retain_current(&mut self, currentness: ViewerPickCurrentness) {
        if self
            .queued
            .is_some_and(|request| !currentness.accepts(request))
        {
            self.queued = None;
        }
        if self
            .last_completed
            .is_some_and(|request| !currentness.accepts(request))
        {
            self.last_completed = None;
        }
        if self
            .hover_interest
            .is_some_and(|request| !currentness.accepts(request))
        {
            self.hover_interest = None;
        }
        if self.automation_completion.is_none()
            && self
                .automation_interest
                .is_some_and(|request| !currentness.accepts(request))
        {
            self.automation_interest = None;
        }
        // Pending GPU work cannot be cancelled. It remains bounded and is
        // drained later; completion currentness decides whether it is used.
    }

    pub(crate) fn hover_completion_is_wanted(&self, request: ViewerPickRequest) -> bool {
        request.purpose() == ViewerPickPurpose::Hover
            && (self.hover_interest == Some(request) || self.automation_interest == Some(request))
    }

    pub(crate) fn discard_queued(&mut self, request: ViewerPickRequest) -> bool {
        if self.queued == Some(request) {
            self.queued = None;
            true
        } else {
            false
        }
    }

    pub(crate) fn clear_unsubmitted(&mut self) {
        self.queued = None;
        self.last_completed = None;
        self.hover_interest = None;
        self.automation_interest = None;
        self.automation_completion = None;
    }

    /// Dataset replacement invalidates even already-submitted GPU tickets.
    /// The renderer retires those source-scoped slots at the same boundary,
    /// so retaining a pending queue entry would only poll an unknown ticket.
    pub(crate) fn retire_source_generation(&mut self) {
        self.pending = None;
        self.clear_unsubmitted();
    }

    pub(crate) const fn has_work(&self) -> bool {
        self.pending.is_some() || self.queued.is_some()
    }
}

fn same_pick_target(completed: ViewerPickRequest, request: Option<ViewerPickRequest>) -> bool {
    request.is_some_and(|request| {
        completed.query() == request.query()
            && completed.context() == request.context()
            && completed.tool() == request.tool()
            && completed.screen_position() == request.screen_position()
    })
}

pub(crate) fn accepted_pick_hit(
    currentness: ViewerPickCurrentness,
    request: ViewerPickRequest,
    result: VolumePickResult,
) -> Option<PickHit> {
    (currentness.accepts(request) && result.query() == request.query()).then(|| {
        PickHit::from_volume_result(
            request.context().source_generation,
            request.screen_position(),
            result,
        )
    })
}

impl MiranteWorkbenchApp {
    /// Polls and submits at most one bounded asynchronous volume pick without
    /// ever waiting for a map callback on the UI thread.
    pub(crate) fn pump_viewer_pick(&mut self, context: &eframe::egui::Context) {
        if !self.viewer_pick_queue.has_work() {
            return;
        }
        let snapshot = self.application_snapshot_for_ui();
        let currentness = ViewerPickCurrentness::from_snapshot(&snapshot);
        self.viewer_pick_queue.retain_current(currentness);
        match self.native_presentation.pick_pipeline_is_ready() {
            Ok(true) => {}
            Ok(false) => {
                // Keep the latest current request queued while the renderer's
                // one bounded compiler worker finishes the pick capability.
                // Compiler polling owns the slower cadence; do not turn this
                // into the 8-ms mapped-result poll loop.
                context.request_repaint_after(BACKGROUND_WORK_REPAINT_INTERVAL);
                return;
            }
            Err(error) => {
                self.viewer_pick_queue.clear_unsubmitted();
                tracing::warn!(%error, "viewer picking stopped after renderer initialization failed");
                return;
            }
        }

        let completion = self
            .viewer_pick_queue
            .pending()
            .and_then(|(ticket, request)| {
                let polled = self
                    .native_presentation
                    .product_gpu
                    .as_mut()
                    .map(|product| product.renderer.poll_pick(ticket));
                match polled {
                    Some(Ok(Some(result))) => {
                        self.viewer_pick_queue.finish_pending(ticket);
                        Some((request, result))
                    }
                    Some(Ok(None)) => None,
                    Some(Err(error)) => {
                        self.viewer_pick_queue.discard_pending(ticket);
                        tracing::warn!(%error, ?ticket, "asynchronous viewer pick failed");
                        None
                    }
                    None => {
                        self.viewer_pick_queue.discard_pending(ticket);
                        None
                    }
                }
            });

        if let Some((request, result)) = completion {
            let hover_wanted = self.viewer_pick_queue.hover_completion_is_wanted(request);
            let geometry = snapshot
                .catalog()
                .layer(snapshot.view().active_layer())
                .map(|layer| {
                    ActiveVolumeGeometry::new(layer.grid_to_world(), layer.shape().spatial())
                });
            if let Some(hit) = accepted_pick_hit(currentness, request, result)
                && (request.purpose() == ViewerPickPurpose::PrimaryClick || hover_wanted)
                && geometry.is_some_and(|geometry| {
                    self.apply_viewer_pick_hit(request, hit.clone(), geometry, context)
                })
            {
                self.viewer_pick_queue.record_completed(request);
                self.viewer_pick_queue
                    .record_automation_completion(request, hit);
            }
        }

        if self.native_presentation.product_gpu.is_none() {
            self.viewer_pick_queue.clear_unsubmitted();
            return;
        }
        if let Some(request) = self.viewer_pick_queue.next_request() {
            let query = request.query();
            let submission = self
                .native_presentation
                .product_gpu
                .as_mut()
                .expect("the renderer availability was checked above")
                .renderer
                .request_coordinated_pick(query.target(), query);
            match submission {
                Ok(ticket) => {
                    if ticket.target() != query.target() || ticket.frame() != query.frame() {
                        tracing::error!(
                            ?ticket,
                            ?query,
                            "renderer returned a mismatched volume-pick ticket"
                        );
                    }
                    self.viewer_pick_queue.mark_submitted(ticket);
                }
                Err(
                    mirante4d_render_wgpu::WgpuRenderRuntimeError::PickBackpressure
                    | mirante4d_render_wgpu::WgpuRenderRuntimeError::PickCapacityExceeded,
                ) => {}
                Err(mirante4d_render_wgpu::WgpuRenderRuntimeError::PickFrameUnavailable) => {
                    self.viewer_pick_queue.discard_queued(request);
                }
                Err(error) => {
                    self.viewer_pick_queue.discard_queued(request);
                    tracing::warn!(%error, "viewer-pick submission rejected");
                }
            }
        }

        if self.viewer_pick_queue.has_work() {
            context.request_repaint_after(PICK_POLL_REPAINT_INTERVAL);
        }
    }

    fn apply_viewer_pick_hit(
        &mut self,
        request: ViewerPickRequest,
        hit: PickHit,
        geometry: ActiveVolumeGeometry,
        context: &eframe::egui::Context,
    ) -> bool {
        let event = match request.purpose() {
            ViewerPickPurpose::Hover => {
                self.egui_ui.hovered_pixel = viewport_hover(request, &hit);
                self.egui_ui.hovered_source_readout = None;
                ViewerToolEvent::Hover(Some(hit))
            }
            ViewerPickPurpose::PrimaryClick => ViewerToolEvent::PrimaryClick(hit),
        };
        let commands = match self.egui_ui.viewer_tools.handle_event(event, geometry) {
            Ok(commands) => commands,
            Err(error) => {
                tracing::warn!(%error, "viewer-tool pick was rejected");
                return false;
            }
        };
        for command in commands {
            match command {
                ViewerToolCommand::CommitRoi(roi) => {
                    if let Err(error) = self
                        .analysis_runtime
                        .set_roi(roi.origin_zyx(), roi.shape_zyx())
                    {
                        tracing::warn!(%error, "viewer ROI was rejected by analysis");
                    }
                }
                ViewerToolCommand::SetCrosshair(hit) => {
                    let Some(world_position) = hit.world_position else {
                        continue;
                    };
                    let snapshot = self.application.snapshot();
                    let view = application_view(&snapshot);
                    let cross_section =
                        match cross_section_centered_at(*view.cross_section(), world_position) {
                            Ok(cross_section) => cross_section,
                            Err(error) => {
                                tracing::warn!(%error, "crosshair world position was rejected");
                                continue;
                            }
                        };
                    if let Err(fault) = self.apply_application_command(
                        mirante4d_application::ApplicationCommand::SetLayout {
                            layout: view.layout(),
                            cross_section,
                        },
                        context,
                    ) {
                        tracing::warn!(?fault, "crosshair could not update linked slice authority");
                    }
                }
                ViewerToolCommand::SetDistanceMeasurement(_) => {}
            }
        }
        true
    }
}

fn cross_section_centered_at(
    current: mirante4d_domain::CrossSectionView,
    center: mirante4d_domain::WorldPoint3,
) -> Result<mirante4d_domain::CrossSectionView, mirante4d_domain::ViewError> {
    mirante4d_domain::CrossSectionView::new(
        center,
        current.orientation(),
        current.scale_world_per_screen_point(),
        current.depth_world(),
    )
}

fn viewport_hover(request: ViewerPickRequest, hit: &PickHit) -> Option<ViewportHover> {
    let intensity = match hit.value? {
        PickValue::IntensityU8(value) => ViewportIntensity::U8(value),
        PickValue::IntensityU16(value) => ViewportIntensity::U16(value),
        PickValue::IntensityF32(value) => ViewportIntensity::F32(value),
    };
    let screen = request.screen_position();
    Some(ViewportHover {
        x: screen.x.max(0.0).round() as u64,
        y: screen.y.max(0.0).round() as u64,
        intensity,
        sample_kind: match hit.kind {
            mirante4d_application::viewer_tools::PickHitKind::Voxel => ViewportSampleKind::Voxel,
            mirante4d_application::viewer_tools::PickHitKind::InterpolatedSample => {
                ViewportSampleKind::Interpolated
            }
            mirante4d_application::viewer_tools::PickHitKind::Empty => return None,
        },
    })
}

#[cfg(test)]
mod tests {
    use mirante4d_application::{
        SourceSessionGeneration,
        viewer_tools::{ScreenPosition, ViewerToolContext},
    };
    use mirante4d_dataset::{BrickKey, DatasetResourceIdentity, ResourceRegion};
    use mirante4d_domain::{LogicalLayerKey, ScaleLevel, Shape3D, TimeIndex, WorldPoint3};
    use mirante4d_render_api::{
        FrameCompleteness, FrameCoverage, FrameProgress, PresentationTarget, PresentedFrame,
        RenderRequirement, RenderRequirementRole, RenderRequirements, VolumePickCompleteness,
        VolumePickPolicy, VolumePickQuery, VolumePickValue,
    };

    use super::*;

    fn identity() -> DatasetResourceIdentity {
        DatasetResourceIdentity::ContentAddress(
            "m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
        )
    }

    fn presented() -> PresentedFrame {
        let layer = mirante4d_render_api::LayerRenderIntent::new(
            LogicalLayerKey::new(3),
            mirante4d_domain::LayerTransfer::new(
                mirante4d_domain::DisplayWindow::new(0.0, 1.0).unwrap(),
                mirante4d_domain::RgbColor::new([1.0; 3]).unwrap(),
                mirante4d_domain::Opacity::new(1.0).unwrap(),
                mirante4d_domain::TransferCurve::linear(),
                false,
            ),
            mirante4d_domain::RenderState::mip(mirante4d_domain::SamplingPolicy::VoxelExact),
        );
        let intent = mirante4d_render_api::RenderIntent::new(
            FrameIdentity::new(7),
            identity(),
            TimeIndex::new(5),
            mirante4d_render_api::RenderViewIntent::volume(
                mirante4d_domain::CameraView::new(
                    mirante4d_domain::Projection::Orthographic,
                    WorldPoint3::origin(),
                    mirante4d_domain::UnitQuaternion::identity(),
                    1.0,
                    1.0,
                    1.0,
                )
                .unwrap(),
                mirante4d_domain::IsoLightState::attached_camera(),
            ),
            mirante4d_render_api::PresentationViewport::new(64.0, 32.0).unwrap(),
            RenderExtent::new(64, 32).unwrap(),
            vec![layer],
        )
        .unwrap();
        let key = BrickKey::new(
            identity(),
            LogicalLayerKey::new(3),
            TimeIndex::new(5),
            ScaleLevel::BASE,
            ResourceRegion::new([0; 3], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
        );
        let requirements = RenderRequirements::new(
            &intent,
            vec![RenderRequirement::new(
                key,
                RenderRequirementRole::FirstUsefulFrame,
            )],
        )
        .unwrap();
        let coverage = FrameCoverage::from_available(&requirements, &[key]).unwrap();
        PresentedFrame::new(
            PresentationTarget::ThreeD,
            RenderExtent::new(64, 32).unwrap(),
            FrameProgress::new(coverage, FrameCompleteness::Exact, None).unwrap(),
        )
    }

    fn request(purpose: ViewerPickPurpose, x: f64) -> ViewerPickRequest {
        let presented = presented();
        let context = ViewerToolContext::new(
            SourceSessionGeneration::new(2),
            TimeIndex::new(5),
            LogicalLayerKey::new(3),
        );
        ViewerPickRequest::new(
            VolumePickQuery::new(
                &presented,
                context.timepoint,
                context.layer,
                [x, 4.0],
                VolumePickPolicy::MipArgmax,
            )
            .unwrap(),
            context,
            ViewerTool::RoiBox,
            purpose,
            ScreenPosition::new(x as f32, 4.0),
        )
        .unwrap()
    }

    fn currentness(request: ViewerPickRequest) -> ViewerPickCurrentness {
        let query = request.query();
        ViewerPickCurrentness {
            context: request.context(),
            tool: request.tool(),
            presented: Some(PresentedPickIdentity {
                target: query.target(),
                frame: query.frame(),
                extent: query.extent(),
            }),
        }
    }

    #[test]
    fn queue_is_one_pending_plus_one_latest_and_clicks_survive_hover_traffic() {
        let first_hover = request(ViewerPickPurpose::Hover, 1.0);
        let next_hover = request(ViewerPickPurpose::Hover, 2.0);
        let click = request(ViewerPickPurpose::PrimaryClick, 3.0);
        let later_hover = request(ViewerPickPurpose::Hover, 4.0);
        let mut queue = ViewerPickQueue::<u64>::default();

        queue.enqueue(first_hover);
        assert_eq!(queue.next_request(), Some(first_hover));
        assert!(queue.mark_submitted(7));
        queue.enqueue(next_hover);
        queue.enqueue(click);
        queue.enqueue(later_hover);
        assert_eq!(queue.pending(), Some((7, first_hover)));
        assert_eq!(queue.finish_pending(7), Some(first_hover));
        assert_eq!(queue.next_request(), Some(click));
    }

    #[test]
    fn completed_stationary_hover_is_not_resubmitted() {
        let hover = request(ViewerPickPurpose::Hover, 1.0);
        let click = request(ViewerPickPurpose::PrimaryClick, 1.0);
        let mut queue = ViewerPickQueue::<u64>::default();
        queue.enqueue(hover);
        assert!(queue.mark_submitted(1));
        assert_eq!(queue.finish_pending(1), Some(hover));
        queue.record_completed(hover);
        queue.enqueue(hover);
        assert_eq!(queue.next_request(), None);
        queue.enqueue(click);
        assert_eq!(queue.next_request(), Some(click));
    }

    #[test]
    fn leaving_the_viewport_invalidates_hover_completion_and_deduplication() {
        let hover = request(ViewerPickPurpose::Hover, 1.0);
        let mut queue = ViewerPickQueue::<u64>::default();
        queue.observe_ui_request(Some(hover));
        assert!(queue.mark_submitted(1));
        assert!(queue.hover_completion_is_wanted(hover));

        queue.observe_ui_request(None);
        assert!(!queue.hover_completion_is_wanted(hover));
        assert_eq!(queue.finish_pending(1), Some(hover));

        queue.observe_ui_request(Some(hover));
        assert_eq!(queue.next_request(), Some(hover));
    }

    #[test]
    fn ui_pointer_departure_does_not_erase_an_awaited_automation_pick() {
        let hover = request(ViewerPickPurpose::Hover, 1.0);
        let result = VolumePickResult::voxel(
            hover.query(),
            WorldPoint3::new(1.0, 2.0, 3.0).unwrap(),
            VolumePickValue::IntensityU16(42),
            5.0,
            VolumePickCompleteness::Exact,
        )
        .unwrap();
        let hit = accepted_pick_hit(currentness(hover), hover, result).unwrap();
        let mut queue = ViewerPickQueue::<u64>::default();

        queue.enqueue_automation(hover);
        queue.observe_ui_request(None);
        assert_eq!(queue.next_request(), Some(hover));
        assert!(queue.hover_completion_is_wanted(hover));
        assert!(queue.mark_submitted(1));
        assert_eq!(queue.finish_pending(1), Some(hover));
        queue.record_automation_completion(hover, hit.clone());

        assert_eq!(
            queue.take_automation_completion_for(ViewerPickPurpose::Hover),
            Some((hover, hit))
        );
        assert_eq!(
            queue.take_automation_completion_for(ViewerPickPurpose::Hover),
            None
        );
    }

    #[test]
    fn automation_click_completion_keeps_its_submitted_identity_after_the_click_effect() {
        let click = request(ViewerPickPurpose::PrimaryClick, 1.0);
        let next_frame_click = request(ViewerPickPurpose::PrimaryClick, 2.0);
        let result = VolumePickResult::voxel(
            click.query(),
            WorldPoint3::new(1.0, 2.0, 3.0).unwrap(),
            VolumePickValue::IntensityU16(42),
            5.0,
            VolumePickCompleteness::Exact,
        )
        .unwrap();
        let hit = accepted_pick_hit(currentness(click), click, result).unwrap();
        let mut queue = ViewerPickQueue::<u64>::default();

        queue.enqueue_automation(click);
        assert!(queue.mark_submitted(1));
        assert_eq!(queue.finish_pending(1), Some(click));
        queue.record_automation_completion(click, hit.clone());

        // Applying a click may synchronously produce a new presentation frame.
        // The automation result still belongs to the command that submitted
        // the accepted pick, not a newly reconstructed request identity.
        assert_ne!(click, next_frame_click);
        queue.retain_current(currentness(next_frame_click));
        assert_eq!(
            queue.take_automation_completion_for(ViewerPickPurpose::PrimaryClick),
            Some((click, hit))
        );
    }

    #[test]
    fn automation_repeats_an_identical_ui_hover_to_obtain_its_own_result() {
        let hover = request(ViewerPickPurpose::Hover, 1.0);
        let mut queue = ViewerPickQueue::<u64>::default();
        queue.record_completed(hover);

        queue.enqueue_automation(hover);

        assert_eq!(queue.next_request(), Some(hover));
    }

    #[test]
    fn moving_then_returning_resubmits_a_previously_completed_hover() {
        let first = request(ViewerPickPurpose::Hover, 1.0);
        let moved = request(ViewerPickPurpose::Hover, 2.0);
        let mut queue = ViewerPickQueue::<u64>::default();
        queue.observe_ui_request(Some(first));
        assert!(queue.mark_submitted(1));
        assert_eq!(queue.finish_pending(1), Some(first));
        queue.record_completed(first);

        assert!(queue.observe_ui_request(Some(moved)));
        assert_eq!(queue.next_request(), Some(moved));
        assert!(queue.discard_queued(moved));

        assert!(!queue.observe_ui_request(Some(first)));
        assert_eq!(queue.next_request(), Some(first));
    }

    #[test]
    fn pointer_departure_drops_only_hover_and_preserves_a_queued_click() {
        let hover = request(ViewerPickPurpose::Hover, 1.0);
        let click = request(ViewerPickPurpose::PrimaryClick, 2.0);
        let mut queue = ViewerPickQueue::<u64>::default();
        queue.observe_ui_request(Some(hover));
        assert!(queue.mark_submitted(1));
        queue.observe_ui_request(Some(click));
        queue.observe_ui_request(None);

        assert_eq!(queue.pending(), Some((1, hover)));
        assert_eq!(queue.finish_pending(1), Some(hover));
        assert_eq!(queue.next_request(), Some(click));
    }

    #[test]
    fn stale_queued_work_is_dropped_but_pending_work_remains_drainable() {
        let initial_request = request(ViewerPickPurpose::Hover, 1.0);
        let mut queue = ViewerPickQueue::<u64>::default();
        queue.enqueue(initial_request);
        assert!(queue.mark_submitted(5));
        queue.enqueue(request(ViewerPickPurpose::Hover, 2.0));
        let mut stale = currentness(initial_request);
        stale.tool = ViewerTool::Inspect;
        queue.retain_current(stale);
        assert_eq!(queue.next_request(), None);
        assert_eq!(queue.pending(), Some((5, initial_request)));
    }

    #[test]
    fn source_retirement_drops_pending_queued_and_completed_pick_authority() {
        let pending = request(ViewerPickPurpose::Hover, 1.0);
        let queued = request(ViewerPickPurpose::PrimaryClick, 2.0);
        let mut queue = ViewerPickQueue::<u64>::default();
        queue.enqueue_automation(pending);
        assert!(queue.mark_submitted(5));
        queue.enqueue(queued);
        queue.record_completed(pending);

        queue.retire_source_generation();

        assert!(!queue.has_work());
        assert_eq!(queue.pending(), None);
        assert_eq!(queue.next_request(), None);
        assert_eq!(
            queue.take_automation_completion_for(ViewerPickPurpose::Hover),
            None
        );
        assert!(!queue.hover_completion_is_wanted(pending));
    }

    #[test]
    fn completion_requires_exact_query_and_current_display_identity() {
        let request = request(ViewerPickPurpose::PrimaryClick, 3.0);
        let result = VolumePickResult::voxel(
            request.query(),
            WorldPoint3::new(1.0, 2.0, 3.0).unwrap(),
            VolumePickValue::IntensityU16(42),
            5.0,
            VolumePickCompleteness::Exact,
        )
        .unwrap();
        assert!(accepted_pick_hit(currentness(request), request, result).is_some());

        let mut stale = currentness(request);
        stale.presented = None;
        assert_eq!(accepted_pick_hit(stale, request, result), None);
    }

    #[test]
    fn accepted_volume_value_becomes_the_existing_viewport_readout() {
        let request = request(ViewerPickPurpose::Hover, 3.4);
        let result = VolumePickResult::voxel(
            request.query(),
            WorldPoint3::new(1.0, 2.0, 3.0).unwrap(),
            VolumePickValue::IntensityU16(42),
            5.0,
            VolumePickCompleteness::Exact,
        )
        .unwrap();
        let hit = accepted_pick_hit(currentness(request), request, result).unwrap();

        assert_eq!(
            viewport_hover(request, &hit),
            Some(ViewportHover {
                x: 3,
                y: 4,
                intensity: ViewportIntensity::U16(42),
                sample_kind: ViewportSampleKind::Voxel,
            })
        );
    }

    #[test]
    fn exact_3d_crosshair_pick_recenters_linked_slice_authority() {
        let current = mirante4d_domain::CrossSectionView::new(
            WorldPoint3::origin(),
            mirante4d_domain::UnitQuaternion::identity(),
            2.0,
            3.0,
        )
        .unwrap();
        let picked = WorldPoint3::new(11.0, 12.0, 13.0).unwrap();

        let linked = cross_section_centered_at(current, picked).unwrap();

        assert_eq!(linked.center_world(), picked);
        assert_eq!(linked.orientation(), current.orientation());
        assert_eq!(
            linked.scale_world_per_screen_point(),
            current.scale_world_per_screen_point()
        );
        assert_eq!(linked.depth_world(), current.depth_world());
    }
}
