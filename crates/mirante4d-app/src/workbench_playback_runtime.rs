use eframe::egui;
use mirante4d_application::{ApplicationCommand, ApplicationSnapshot, OperationKind};
use mirante4d_project_model::ViewState;

use crate::{
    BACKGROUND_WORK_REPAINT_INTERVAL,
    brick_streaming::{brick_runtime_work_active, playback_timepoint_finished_loading},
    cross_section_scheduler::cross_section_refinement_work_pending,
    cross_section_streaming::cross_section_runtime_work_active,
    current_runtime::{
        analysis::CurrentAnalysisRuntime, dataset::CurrentDatasetRuntime,
        import::CurrentImportRuntime, render::CurrentRenderRuntime,
    },
    playback::{PLAYBACK_FRAME_INTERVAL, playback_tick_for_ui_time},
};

pub(crate) fn background_work_active(
    snapshot: &ApplicationSnapshot,
    import: &CurrentImportRuntime,
    analysis: &CurrentAnalysisRuntime,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
) -> bool {
    application_service_work_active(snapshot)
        || import.tiff_import_setup_task.is_some()
        || import.import_task.is_some()
        || analysis.analysis_task.is_some()
        || snapshot.transient().playback_active()
        || brick_runtime_work_active(dataset)
        || cross_section_runtime_work_active(&render.cross_section_runtime)
        || cross_section_refinement_work_pending(dataset, render)
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
                    | OperationKind::ProjectOpen
                    | OperationKind::ProjectSave
            )
        })
}

pub(crate) fn enqueue_playback_command_if_due(
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    dataset: &CurrentDatasetRuntime,
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
        && !playback_timepoint_finished_loading(snapshot, dataset, render, view.timepoint())
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
    fn source_and_project_operations_keep_application_services_polling() {
        for kind in [
            OperationKind::DatasetOpen,
            OperationKind::ProjectOpen,
            OperationKind::ProjectSave,
        ] {
            assert!(pending_application_service_work([kind], false));
        }

        assert!(!pending_application_service_work(
            [OperationKind::Analysis, OperationKind::Import],
            false,
        ));
    }

    #[test]
    fn pending_settings_keep_application_services_polling_without_an_operation() {
        assert!(pending_application_service_work([], true));
        assert!(!pending_application_service_work([], false));
    }
}
