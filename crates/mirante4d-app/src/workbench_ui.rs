use super::*;
use mirante4d_render_api::{CameraFrame, RenderExtent};

use crate::viewer_layout::{PanelId, cross_section_schedule_status_label};

#[derive(Clone)]
struct ViewerUiSnapshot {
    presentation_viewport: PresentationViewport,
    render_viewport: RenderExtent,
    frame_fidelity: FrameFidelityStatus,
    composite_fidelity: String,
    dataset_path: String,
    messages: Vec<String>,
    xy_placeholder: String,
    xz_placeholder: String,
    yz_placeholder: String,
    test_render_viewport_max_side: Option<usize>,
    automation_render_target: Option<RenderExtent>,
}

impl MiranteWorkbenchApp {
    fn viewer_ui_snapshot(&self, snapshot: &ApplicationSnapshot) -> ViewerUiSnapshot {
        let panel_placeholder = |panel_id: PanelId| {
            let panel = self
                .render_coordination
                .surface(panel_id.presentation_slot());
            if panel.render_failure().is_some() {
                format!("{}\nrender failed", panel_id.label())
            } else {
                panel
                    .cross_section_schedule()
                    .map(cross_section_schedule_status_label)
                    .map(|status| format!("{}\n{status}", panel_id.label()))
                    .unwrap_or_else(|| panel_id.label().to_owned())
            }
        };
        let dataset_plan_error = self.dataset.last_plan_error().map(str::to_owned);
        let mut messages = dataset_plan_error.iter().cloned().collect::<Vec<_>>();
        if let Some(error) = &self.render_coordination.frame_fidelity.last_capacity_error
            && dataset_plan_error.as_deref() != Some(error.as_str())
        {
            messages.push(error.clone());
        }
        for (slot, panel) in self.render_coordination.iter() {
            if let Some(failure) = panel.render_failure() {
                let panel_id = PanelId::from_presentation_slot(slot);
                messages.push(format!(
                    "{} cross-section failed ({:?}): {}",
                    panel_id.label(),
                    failure.kind(),
                    failure.message()
                ));
            }
        }
        ViewerUiSnapshot {
            presentation_viewport: self.render_coordination.presentation_viewport,
            render_viewport: self.render_coordination.render_viewport,
            frame_fidelity: self.render_coordination.frame_fidelity.clone(),
            composite_fidelity: composite_fidelity_label(snapshot, &self.render_coordination),
            dataset_path: dataset_path_status_label(self.dataset.selected_path()),
            messages,
            xy_placeholder: panel_placeholder(PanelId::Xy),
            xz_placeholder: panel_placeholder(PanelId::Xz),
            yz_placeholder: panel_placeholder(PanelId::Yz),
            test_render_viewport_max_side: {
                #[cfg(test)]
                {
                    self.test_render_viewport_max_side
                }
                #[cfg(not(test))]
                {
                    None
                }
            },
            automation_render_target: self
                .product_automation
                .as_ref()
                .and_then(ProductAutomationController::render_target_override),
        }
    }
}

fn active_layer_no_data_policy_label(snapshot: &ApplicationSnapshot) -> Option<&'static str> {
    let active_layer = match snapshot.workspace() {
        WorkspaceSnapshot::Unbound { workspace } => workspace.view().active_layer(),
        WorkspaceSnapshot::Bound { project, .. } => project.view().active_layer(),
    };
    match snapshot
        .catalog()
        .layer(active_layer)
        .and_then(|layer| layer.validity(ScaleLevel::BASE))
    {
        Some(ResourceValidity::BitMask) => Some("explicit per-sample validity mask"),
        Some(ResourceValidity::AllValid) | None => None,
    }
}

impl eframe::App for MiranteWorkbenchApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump_application_services();
        self.handle_close_request(ui.ctx());

        self.drain_tiff_import_setup_results(ui.ctx());
        self.drain_import_results(ui.ctx());

        let application_snapshot = self.application_snapshot_for_ui();
        let viewer_ui_snapshot = self.viewer_ui_snapshot(&application_snapshot);
        let view = application_view(&application_snapshot);
        let analysis_start_unavailable_reason = self.analysis_start_unavailable_reason();
        let analysis_active = self.analysis_runtime.active_token().is_some();
        let analysis_roi_origin = self.analysis_runtime.roi_origin();
        let analysis_roi_shape = self.analysis_runtime.roi_shape();
        let transient = application_snapshot.transient();
        let analysis_workspace_view = analysis_workspace_snapshot(
            &self.analysis_runtime,
            AnalysisWorkspaceSnapshotInput {
                table_descriptors: transient.analysis_tables(),
                plot_descriptors: transient.analysis_plots(),
                selected_table: transient.selected_analysis_table(),
                selected_plot: transient.selected_analysis_plot(),
                selected_plot_point: transient.selected_analysis_plot_point(),
            },
        );
        let dirty_project_close_ui = self.dirty_project_close_ui();
        let settings_ui_view = self.settings_ui_view();
        let dataset_open_pending = self.pending_dataset_open_path.is_some();
        let project_status_message = self.project_status_message.clone();
        let source_verification_available = self
            .source_verification_service
            .as_ref()
            .is_some_and(|service| service.active_token().is_none());
        let runtime_diagnostics_view = runtime_diagnostics_panel::runtime_diagnostics_view(self);
        let mut application_commands = Vec::new();
        workbench_playback_runtime::enqueue_playback_command_if_due(
            &application_snapshot,
            &self.dataset,
            &mut application_commands,
            ui.ctx(),
        );

        let active_layer_histogram_for_ui = self.active_histogram_summary(&application_snapshot);
        let project_actions_available = matches!(
            application_snapshot.source(),
            SourceVerificationSnapshot::Verified(_)
        );
        let project_is_bound = application_snapshot.is_bound();
        let project_store_status = self.project_store.as_ref().map(|service| service.status());
        let project_store_idle = project_store_status.as_ref().is_some_and(|status| {
            !status.foreground_active()
                && !status.autosave_active()
                && !matches!(
                    status.lifecycle(),
                    ProjectStoreLifecycle::Closing | ProjectStoreLifecycle::Closed
                )
        });
        let can_inspect_project_recovery = project_store_status.as_ref().is_some_and(|status| {
            !status.foreground_active()
                && !status.autosave_active()
                && matches!(
                    status.lifecycle(),
                    ProjectStoreLifecycle::Provisional
                        | ProjectStoreLifecycle::Established
                        | ProjectStoreLifecycle::RecoveryOnly
                        | ProjectStoreLifecycle::RecoverySelected
                )
        });
        let can_new_project = project_actions_available
            && !project_is_bound
            && self
                .project_store
                .as_ref()
                .is_some_and(ProjectStoreApplicationService::can_open);
        let can_open_project = project_actions_available
            && !project_is_bound
            && self
                .project_store
                .as_ref()
                .is_some_and(ProjectStoreApplicationService::can_open);
        let can_save_project = project_actions_available
            && project_is_bound
            && self
                .project_store
                .as_ref()
                .is_some_and(ProjectStoreApplicationService::can_save);
        let can_save_project_as = project_actions_available
            && project_is_bound
            && self
                .project_store
                .as_ref()
                .is_some_and(ProjectStoreApplicationService::can_save_as);
        let project_recovery_ui = self.project_recovery_ui();
        let project_recovery_available = can_inspect_project_recovery
            || project_recovery_ui.has_candidates()
            || project_recovery_ui.has_locators();
        let no_data_policy_label = active_layer_no_data_policy_label(&application_snapshot);
        let camera_frame =
            CameraFrame::new(*view.camera(), viewer_ui_snapshot.presentation_viewport).ok();
        let camera_inspector_view = ui_kit::CameraInspectorView {
            forward: camera_frame.as_ref().map(|frame| frame.axes().forward()),
            world_per_screen_point: camera_frame
                .as_ref()
                .and_then(|frame| frame.world_per_screen_point_at_target().ok()),
        };
        let mut workbench_output = ui_kit::show_workbench(
            ui,
            ui_kit::WorkbenchView {
                toolbar: ui_kit::TopToolbarView {
                    application: &application_snapshot,
                    project: ui_kit::ProjectControlsView {
                        status_message: project_status_message.as_deref(),
                        dataset_open_pending,
                        project_store_idle,
                        can_new: can_new_project,
                        can_open: can_open_project,
                        can_save: can_save_project,
                        can_save_as: can_save_project_as,
                        recovery_available: project_recovery_available,
                    },
                    presentation_viewport: viewer_ui_snapshot.presentation_viewport,
                },
                left: ui_kit::LeftWorkbenchView {
                    application: &application_snapshot,
                    source_verification_available,
                    composite_fidelity: &viewer_ui_snapshot.composite_fidelity,
                    dataset_path: &viewer_ui_snapshot.dataset_path,
                },
                inspector: ui_kit::InspectorWorkbenchView {
                    application: &application_snapshot,
                    histogram: &active_layer_histogram_for_ui,
                    frame_fidelity: &viewer_ui_snapshot.frame_fidelity,
                    render_viewport: viewer_ui_snapshot.render_viewport,
                    dvr_density_scale_range: [DVR_DENSITY_SCALE_MIN, DVR_DENSITY_SCALE_MAX],
                    no_data_policy_label,
                    analysis: ui_kit::AnalysisControlsView {
                        start_unavailable_reason: analysis_start_unavailable_reason.as_deref(),
                        active: analysis_active,
                        roi_origin: analysis_roi_origin,
                        roi_shape: analysis_roi_shape,
                        workspace: &analysis_workspace_view,
                    },
                    settings: &settings_ui_view,
                    runtime_diagnostics: &runtime_diagnostics_view,
                    camera: camera_inspector_view,
                    messages: &viewer_ui_snapshot.messages,
                },
                viewer: ui_kit::ViewerWorkbenchView {
                    application: &application_snapshot,
                    frame_fidelity: &viewer_ui_snapshot.frame_fidelity,
                    fallback_render_extent: viewer_ui_snapshot.render_viewport,
                    xy_placeholder: &viewer_ui_snapshot.xy_placeholder,
                    xz_placeholder: &viewer_ui_snapshot.xz_placeholder,
                    yz_placeholder: &viewer_ui_snapshot.yz_placeholder,
                    test_render_viewport_max_side: viewer_ui_snapshot.test_render_viewport_max_side,
                    automation_render_target: viewer_ui_snapshot.automation_render_target,
                    interaction: ui_kit::ViewerInteractionConfig {
                        cross_section_settle_duration: CROSS_SECTION_INTERACTION_SETTLE_DURATION,
                        cross_section_fast_slice_multiplier: CROSS_SECTION_FAST_SLICE_MULTIPLIER,
                        cross_section_rotate_radians_per_point:
                            CROSS_SECTION_ROTATE_RADIANS_PER_POINT,
                    },
                },
                analysis_workspace: &analysis_workspace_view,
                project_recovery: &project_recovery_ui,
                dirty_project_close: dirty_project_close_ui,
            },
            &mut self.egui_ui,
        );
        application_commands.append(&mut workbench_output.application_commands);
        workbench_output.application_commands = application_commands;
        self.apply_workbench_ui_output(ui, workbench_output);

        self.drain_brick_results(ui.ctx());

        let snapshot = self.application.snapshot();
        let project_store_pending = self
            .project_store
            .as_ref()
            .is_some_and(ProjectStoreApplicationService::has_pending_work);
        if workbench_playback_runtime::background_work_active(
            &snapshot,
            &self.import.workers,
            &self.analysis_runtime,
            &self.dataset,
            &self.render_coordination,
            &self.native_presentation,
        ) || workbench_playback_runtime::source_verification_polling_required(
            self.pending_automatic_source_verification.is_some(),
            self.source_verification_service
                .as_ref()
                .is_some_and(|service| service.active_token().is_some()),
        ) || project_store_pending
        {
            request_background_work_repaint_after(ui.ctx());
        }
        if let Some(delay) = self
            .project_store
            .as_ref()
            .and_then(ProjectStoreApplicationService::repaint_after)
        {
            ui.ctx().request_repaint_after(delay);
        }

        ProductAutomationController::drive(self, ui.ctx());
    }

    fn on_exit(&mut self) {
        self.import.workers.shutdown();
        if let Err(error) = self.dataset.request_shutdown() {
            tracing::warn!(%error, "dataset runtime shutdown request failed");
        }
        if let Some(source_open_service) = self.source_open_service.take()
            && let Err(error) = source_open_service.shutdown()
        {
            tracing::warn!(%error, "dataset open service shutdown failed");
        }
        if let Some(source_verification_service) = self.source_verification_service.take()
            && let Err(error) = source_verification_service.shutdown()
        {
            tracing::warn!(%error, "source-verification service shutdown failed");
        }
        if let Err(error) = self.settings_connection.shutdown() {
            tracing::warn!(%error, "settings actor shutdown failed");
        }
        if let Some(mut project_store) = self.project_store.take() {
            if let Err(error) = project_store.close()
                && !matches!(
                    error,
                    mirante4d_application::ProjectStoreServiceError::Closing
                )
            {
                tracing::warn!(?error, "project-store close request failed during exit");
            }
            if let Err(error) = project_store.join() {
                tracing::warn!(?error, "project-store actor join failed during exit");
            }
        }
    }
}
