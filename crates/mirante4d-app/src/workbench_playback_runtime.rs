use eframe::egui;
use mirante4d_application::{
    ApplicationCommand, ApplicationSnapshot, CrossSectionPanelScheduleStatus, OperationKind,
    PlaybackPhase, PresentationSlot,
};
use mirante4d_domain::ViewerLayout;
use mirante4d_render_api::PresentationTarget;

use crate::{
    BACKGROUND_WORK_REPAINT_INTERVAL, RenderCoordinationState,
    dataset_requests::{DatasetDemandState, SCOPE_PLAYBACK},
    display_refresh::{RenderAttemptCoordinator, RenderAttemptState},
    import_worker_service::ImportWorkerService,
    native_presentation::NativePresentationBridge,
    playback::{playback_frame_interval, playback_tick_for_ui_time},
    workbench_brick_runtime::{
        playback_successor_complete_with_renderer, scope_complete_with_renderer,
    },
};

pub(crate) fn background_work_active(
    snapshot: &ApplicationSnapshot,
    import: &ImportWorkerService,
    dataset: &DatasetDemandState,
    render: &RenderCoordinationState,
    presentation: &NativePresentationBridge,
    render_attempt: &RenderAttemptCoordinator,
    progressive_render_required: bool,
) -> bool {
    application_service_work_active(snapshot)
        || import.status().is_active()
        || snapshot.transient().playback_active()
        || dataset.dispatcher().has_pending_work()
        || (!matches!(
            render_attempt.state(),
            RenderAttemptState::RendererTerminal { .. }
        ) && presentation.product_gpu.as_ref().is_some_and(|product| {
            !product.pending_validation_captures.is_empty()
                || !product.pending_gpu_timings.is_empty()
        }))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaybackReadinessAction {
    Idle,
    Wait,
    MarkPrepared,
    RunClock,
}

const fn playback_readiness_action(
    phase: PlaybackPhase,
    future_ready: bool,
    coherent_predecessor_presented: bool,
    requested_temporal_front_presented: bool,
) -> PlaybackReadinessAction {
    match phase {
        PlaybackPhase::Stopped => PlaybackReadinessAction::Idle,
        PlaybackPhase::Warming if future_ready && coherent_predecessor_presented => {
            PlaybackReadinessAction::MarkPrepared
        }
        PlaybackPhase::Warming => PlaybackReadinessAction::Wait,
        PlaybackPhase::Playing if future_ready && requested_temporal_front_presented => {
            PlaybackReadinessAction::RunClock
        }
        PlaybackPhase::Playing => PlaybackReadinessAction::Wait,
    }
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
    attempt: &RenderAttemptCoordinator,
) -> ProgressiveRenderSubmissionWork {
    ProgressiveRenderSubmissionWork {
        three_d_required: attempt.target_ready(PresentationTarget::ThreeD),
        any_required: PresentationTarget::ALL
            .into_iter()
            .any(|target| attempt.target_ready(target)),
    }
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
                    | OperationKind::PackageIntegrityAudit
                    | OperationKind::ProjectOpen
                    | OperationKind::ProjectSave
                    | OperationKind::Analysis
            )
        })
}

pub(crate) fn enqueue_playback_command_if_due(
    snapshot: &ApplicationSnapshot,
    session: &mut crate::playback_session::PlaybackSession,
    dataset: &DatasetDemandState,
    render: &RenderCoordinationState,
    presentation: &NativePresentationBridge,
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

    let interval = playback_frame_interval(snapshot.transient().playback_fps());
    let future_ready = match snapshot.transient().playback_phase() {
        PlaybackPhase::Warming => {
            scope_complete_with_renderer(dataset, presentation, SCOPE_PLAYBACK)
        }
        PlaybackPhase::Playing => playback_successor_complete_with_renderer(dataset, presentation),
        PlaybackPhase::Stopped => false,
    };
    // This predicate is temporal-only. It deliberately ignores display-input
    // generations and spatial currentness; continuous camera/plane samples
    // may remain unsettled while the requested temporal front commits.
    let requested_temporal_front_presented =
        requested_timepoint_coherently_presented(snapshot, render);
    session.observe_readiness(
        crate::application_view(snapshot).timepoint(),
        timepoint_count,
        requested_temporal_front_presented,
        future_ready,
    );
    let contract_ready = session.contract().is_some();
    match playback_readiness_action(
        snapshot.transient().playback_phase(),
        future_ready && contract_ready,
        requested_temporal_front_presented,
        requested_temporal_front_presented,
    ) {
        PlaybackReadinessAction::Idle => return,
        PlaybackReadinessAction::Wait => {
            ctx.request_repaint_after(BACKGROUND_WORK_REPAINT_INTERVAL);
            return;
        }
        PlaybackReadinessAction::MarkPrepared => {
            commands.push(ApplicationCommand::MarkPlaybackPrepared);
            ctx.request_repaint_after(BACKGROUND_WORK_REPAINT_INTERVAL);
            return;
        }
        PlaybackReadinessAction::RunClock => {}
    }

    let tick = ctx
        .input(|input| playback_tick_for_ui_time(input.time, snapshot.transient().playback_fps()));
    if snapshot
        .transient()
        .last_playback_tick()
        .is_none_or(|last| tick > last)
    {
        commands.push(ApplicationCommand::AdvancePlaybackTick(tick));
    }
    ctx.request_repaint_after(interval);
}

fn requested_timepoint_coherently_presented(
    snapshot: &ApplicationSnapshot,
    render: &RenderCoordinationState,
) -> bool {
    let view = crate::application_view(snapshot);
    requested_timepoint_coherently_presented_for_layout(view.layout(), view.timepoint(), render)
}

fn requested_timepoint_coherently_presented_for_layout(
    layout: ViewerLayout,
    expected_timepoint: mirante4d_domain::TimeIndex,
    render: &RenderCoordinationState,
) -> bool {
    let required = match layout {
        ViewerLayout::Single3d => &PresentationSlot::ALL[..1],
        ViewerLayout::FourPanel => &PresentationSlot::ALL,
    };
    required.iter().copied().all(|slot| {
        render
            .surface(slot)
            .presented_frame()
            .is_some_and(|frame| frame.timepoint() == expected_timepoint)
    })
}

pub(crate) fn catalog_timepoint_count(snapshot: &ApplicationSnapshot) -> u64 {
    snapshot.timepoint_count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_waits_for_temporal_pixels_but_never_for_spatial_settlement() {
        for (future_ready, temporal_front) in [(false, false), (false, true), (true, false)] {
            assert_eq!(
                playback_readiness_action(
                    PlaybackPhase::Warming,
                    future_ready,
                    temporal_front,
                    temporal_front,
                ),
                PlaybackReadinessAction::Wait
            );
            assert_eq!(
                playback_readiness_action(
                    PlaybackPhase::Playing,
                    future_ready,
                    temporal_front,
                    temporal_front,
                ),
                PlaybackReadinessAction::Wait
            );
        }
        assert_eq!(
            playback_readiness_action(PlaybackPhase::Warming, true, true, false),
            PlaybackReadinessAction::MarkPrepared
        );
        assert_eq!(
            playback_readiness_action(PlaybackPhase::Playing, true, false, true),
            PlaybackReadinessAction::RunClock
        );
        assert_eq!(
            playback_readiness_action(PlaybackPhase::Stopped, true, true, true),
            PlaybackReadinessAction::Idle
        );
    }

    #[test]
    fn temporal_readiness_ignores_spatial_generation_but_requires_every_visible_timepoint() {
        let initial_presentation =
            mirante4d_render_api::PresentationViewport::new(64.0, 64.0).unwrap();
        let initial_extent = mirante4d_render_api::RenderExtent::new(64, 64).unwrap();
        let mut render = RenderCoordinationState::new(
            crate::FrameFidelityStatus::new_with_presentation(initial_extent, initial_presentation),
        );
        for slot in [
            PresentationSlot::Xy,
            PresentationSlot::Xz,
            PresentationSlot::Yz,
        ] {
            assert!(render.record_viewports(slot, initial_presentation, initial_extent));
        }
        for slot in PresentationSlot::ALL {
            let generation = render.surface(slot).generation();
            assert!(render.record_presented_frame(
                slot,
                generation,
                crate::tests::synthetic_presented_frame(slot, initial_extent),
            ));
        }

        let changed_presentation =
            mirante4d_render_api::PresentationViewport::new(80.0, 72.0).unwrap();
        let changed_extent = mirante4d_render_api::RenderExtent::new(80, 72).unwrap();
        for slot in PresentationSlot::ALL {
            assert!(render.record_viewports(slot, changed_presentation, changed_extent));
            assert!(!render.surface(slot).display_current());
        }

        assert!(requested_timepoint_coherently_presented_for_layout(
            ViewerLayout::FourPanel,
            mirante4d_domain::TimeIndex::new(0),
            &render,
        ));
        assert!(!requested_timepoint_coherently_presented_for_layout(
            ViewerLayout::FourPanel,
            mirante4d_domain::TimeIndex::new(1),
            &render,
        ));
        render.clear_presented_frame(PresentationSlot::Yz);
        assert!(!requested_timepoint_coherently_presented_for_layout(
            ViewerLayout::FourPanel,
            mirante4d_domain::TimeIndex::new(0),
            &render,
        ));
    }

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
            OperationKind::PackageIntegrityAudit,
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
}
