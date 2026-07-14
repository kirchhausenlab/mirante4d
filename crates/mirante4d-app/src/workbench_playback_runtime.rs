use eframe::egui;
use mirante4d_application::{ApplicationCommand, ApplicationSnapshot, OperationKind};
use mirante4d_domain::ViewerLayout;
use mirante4d_project_model::ViewState;

use crate::{
    BACKGROUND_WORK_REPAINT_INTERVAL,
    current_runtime::{
        analysis::AnalysisProductRuntime, import::ImportRuntime, render::CurrentRenderRuntime,
    },
    dataset_requests::{DatasetDemandState, SCOPE_CURRENT_3D},
    playback::{PLAYBACK_FRAME_INTERVAL, playback_tick_for_ui_time},
    viewer_layout::CrossSectionPanelScheduleStatus,
};

pub(crate) fn background_work_active(
    snapshot: &ApplicationSnapshot,
    import: &ImportRuntime,
    _analysis: &AnalysisProductRuntime,
    dataset: &DatasetDemandState,
    render: &CurrentRenderRuntime,
) -> bool {
    application_service_work_active(snapshot)
        || import.tiff_import_setup_task.is_some()
        || import.import_task.is_some()
        || snapshot.transient().playback_active()
        || dataset.dispatcher().has_pending_work()
        || (crate::application_view(snapshot).layout() == ViewerLayout::FourPanel
            && render.cross_section_runtime.panels().any(|panel| {
                panel.cross_section_schedule.is_some_and(|schedule| {
                    matches!(
                        schedule.status,
                        CrossSectionPanelScheduleStatus::Loading
                            | CrossSectionPanelScheduleStatus::Coarse
                    )
                })
            }))
}

fn application_service_work_active(snapshot: &ApplicationSnapshot) -> bool {
    pending_application_service_work(
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
    _view: &ViewState,
    dataset: &DatasetDemandState,
    render: &mut CurrentRenderRuntime,
    commands: &mut Vec<ApplicationCommand>,
    ctx: &egui::Context,
) {
    if !snapshot.transient().playback_active() {
        return;
    }

    let timepoint_count = catalog_timepoint_count(snapshot);
    if timepoint_count <= 1 {
        render.playback_lod_downshift_active = false;
        commands.push(ApplicationCommand::SetPlaybackActive(false));
        return;
    }

    if snapshot.transient().last_playback_tick().is_some()
        && !dataset.scope_complete(SCOPE_CURRENT_3D, &render.retained_leases)
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
    snapshot
        .catalog()
        .layers()
        .map(|layer| layer.shape().t())
        .min()
        .expect("DatasetCatalog is non-empty by construction")
}

#[cfg(test)]
mod tests {
    use super::*;

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
