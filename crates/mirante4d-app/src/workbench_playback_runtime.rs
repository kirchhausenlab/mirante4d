use eframe::egui;
use mirante4d_application::{
    ApplicationCommand, ApplicationSnapshot, CrossSectionPanelScheduleStatus, OperationKind,
};
use mirante4d_domain::ViewerLayout;
use mirante4d_render_api::PresentationTarget;

use crate::{
    BACKGROUND_WORK_REPAINT_INTERVAL, RenderCoordinationState,
    dataset_requests::{DatasetDemandState, SCOPE_CURRENT_3D},
    import_worker_service::ImportWorkerService,
    native_presentation::NativePresentationBridge,
    playback::{PLAYBACK_FRAME_INTERVAL, playback_tick_for_ui_time},
};

pub(crate) fn background_work_active(
    snapshot: &ApplicationSnapshot,
    import: &ImportWorkerService,
    dataset: &DatasetDemandState,
    render: &RenderCoordinationState,
    presentation: &NativePresentationBridge,
    progressive_render_required: bool,
) -> bool {
    application_service_work_active(snapshot)
        || import.status().is_active()
        || snapshot.transient().playback_active()
        || dataset.dispatcher().has_pending_work()
        || presentation.product_gpu.as_ref().is_some_and(|product| {
            product
                .renderer
                .pipeline_readiness()
                .is_ok_and(|readiness| readiness != mirante4d_render_wgpu::PipelineReadiness::Ready)
                || product.renderer.has_pending_residency_work()
                || product.renderer.has_pending_residency_evictions()
                || !product.pending_validation_captures.is_empty()
        })
        || progressive_render_required
        || (crate::application_view(snapshot).layout() == ViewerLayout::FourPanel
            && render.iter().any(|(_, panel)| {
                panel.cross_section_schedule().is_some_and(|schedule| {
                    cross_section_schedule_requires_polling(schedule.status)
                })
            }))
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProgressiveRenderSubmissionWork {
    pub(crate) three_d_required: bool,
    pub(crate) any_required: bool,
}

/// Renderer-owned progress must not wait for unrelated dataset work to become
/// idle. This applies equally to 3D and linked targets: once newly offered
/// resources make a target executable, one coordinated refresh must consume
/// that work instead of stranding a `Ready` linked schedule.
pub(crate) const fn renderer_progress_refresh_required(
    work: ProgressiveRenderSubmissionWork,
    _dispatcher_has_pending_work: bool,
) -> bool {
    work.any_required
}

const fn cross_section_schedule_requires_polling(status: CrossSectionPanelScheduleStatus) -> bool {
    // `Coarse` is a settled, honest display at the selected current scale; no
    // background work can improve it until view/demand changes. Only missing
    // CPU resources require periodic polling.
    matches!(status, CrossSectionPanelScheduleStatus::Loading)
}

/// Returns true only when another renderer execution can admit data that is
/// already CPU-resident. Dataset I/O completion owns refreshes while resources
/// are still pending, so slow I/O cannot cause identical full-frame renders.
pub(crate) fn progressive_render_submission_work(
    presentation: &NativePresentationBridge,
) -> ProgressiveRenderSubmissionWork {
    let Some(product) = presentation.product_gpu.as_ref() else {
        return ProgressiveRenderSubmissionWork::default();
    };
    if !product
        .renderer
        .pipeline_capability_is_ready(mirante4d_render_wgpu::PipelineCapability::InitialRender)
        .unwrap_or(false)
    {
        return ProgressiveRenderSubmissionWork::default();
    }
    let mut work = ProgressiveRenderSubmissionWork::default();
    for target in PresentationTarget::ALL {
        let required = match product
            .renderer
            .coordinated_target_requires_execution(target)
        {
            Ok(required) => required,
            Err(
                mirante4d_render_wgpu::WgpuRenderRuntimeError::CoordinatedTargetNotConfigured {
                    ..
                },
            ) => false,
            Err(error) => {
                tracing::error!(%error, ?target, "renderer target refresh query failed");
                true
            }
        };
        work.any_required |= required;
        if target == PresentationTarget::ThreeD {
            work.three_d_required = required;
        }
    }
    work
}

fn application_service_work_active(snapshot: &ApplicationSnapshot) -> bool {
    // Events emitted after the update's initial service pump still own one
    // subsequent turn. In particular, a terminal source fault can leave the
    // dataset dispatcher idle while its invalidation event is waiting to
    // retire composition-root state.
    snapshot.pending_event_count() != 0
        || pending_application_service_work(
            snapshot
                .active_operations()
                .iter()
                .map(|operation| operation.kind()),
            snapshot.pending_settings_change().is_some(),
        )
}

fn pending_application_service_work(
    operation_kinds: impl IntoIterator<Item = OperationKind>,
    settings_change_pending: bool,
) -> bool {
    settings_change_pending
        || operation_kinds.into_iter().any(|kind| {
            matches!(
                kind,
                OperationKind::DatasetOpen
                    | OperationKind::SourceVerification
                    | OperationKind::ProjectOpen
                    | OperationKind::ProjectSave
                    | OperationKind::Analysis
            )
        })
}

pub(crate) const fn source_verification_polling_required(
    automatic_request_pending: bool,
    worker_active: bool,
) -> bool {
    automatic_request_pending || worker_active
}

pub(crate) fn enqueue_playback_command_if_due(
    snapshot: &ApplicationSnapshot,
    dataset: &DatasetDemandState,
    commands: &mut Vec<ApplicationCommand>,
    ctx: &egui::Context,
) {
    if !snapshot.transient().playback_active() {
        return;
    }

    let timepoint_count = catalog_timepoint_count(snapshot);
    if timepoint_count <= 1 {
        commands.push(ApplicationCommand::SetPlaybackActive(false));
        return;
    }

    if snapshot.transient().last_playback_tick().is_some()
        && !dataset.scope_complete(SCOPE_CURRENT_3D)
    {
        ctx.request_repaint_after(BACKGROUND_WORK_REPAINT_INTERVAL);
        return;
    }

    let tick = ctx.input(|input| playback_tick_for_ui_time(input.time));
    if snapshot
        .transient()
        .last_playback_tick()
        .is_none_or(|last| tick > last)
    {
        commands.push(ApplicationCommand::AdvancePlaybackTick(tick));
    }
    ctx.request_repaint_after(PLAYBACK_FRAME_INTERVAL);
}

pub(crate) fn catalog_timepoint_count(snapshot: &ApplicationSnapshot) -> u64 {
    snapshot.timepoint_count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loading_cross_sections_keep_background_polling() {
        assert!(cross_section_schedule_requires_polling(
            CrossSectionPanelScheduleStatus::Loading,
        ));
        assert!(!cross_section_schedule_requires_polling(
            CrossSectionPanelScheduleStatus::Coarse,
        ));
        assert!(!cross_section_schedule_requires_polling(
            CrossSectionPanelScheduleStatus::Ready,
        ));
    }

    #[test]
    fn linked_renderer_progress_requests_a_refresh_without_waiting_for_dataset_idle() {
        let linked_only = ProgressiveRenderSubmissionWork {
            three_d_required: false,
            any_required: true,
        };
        assert!(renderer_progress_refresh_required(linked_only, true));
        assert!(renderer_progress_refresh_required(linked_only, false));
        assert!(!renderer_progress_refresh_required(
            ProgressiveRenderSubmissionWork::default(),
            false,
        ));
    }

    #[test]
    fn source_project_and_analysis_operations_keep_application_services_polling() {
        for kind in [
            OperationKind::DatasetOpen,
            OperationKind::SourceVerification,
            OperationKind::ProjectOpen,
            OperationKind::ProjectSave,
            OperationKind::Analysis,
        ] {
            assert!(pending_application_service_work([kind], false));
        }

        assert!(!pending_application_service_work(
            [OperationKind::Import],
            false,
        ));
    }

    #[test]
    fn pending_settings_keep_application_services_polling_without_an_operation() {
        assert!(pending_application_service_work([], true));
        assert!(!pending_application_service_work([], false));
    }

    #[test]
    fn deferred_or_retiring_source_verification_keeps_ui_polling() {
        assert!(source_verification_polling_required(true, false));
        assert!(source_verification_polling_required(false, true));
        assert!(source_verification_polling_required(true, true));
        assert!(!source_verification_polling_required(false, false));
    }
}
