//! Native resolution of typed workbench UI output.

use super::*;
use crate::viewer_layout::PanelId;
use crate::workbench_ui::viewer_tool_for_kind;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct WorkbenchUiApplyTiming {
    pub(crate) command_apply_ms: f64,
    pub(crate) display_refresh_trigger_ms: f64,
    pub(crate) import_action_ms: f64,
}

fn apply_viewport_observations(
    render_coordination: &mut RenderCoordinationState,
    observations: impl IntoIterator<Item = ViewportObservation>,
) -> bool {
    let mut changed = false;
    for observation in observations {
        changed |= render_coordination.record_viewports(
            observation.slot(),
            observation.presentation(),
            observation.render(),
        );
    }
    if changed {
        render_coordination.request_refresh();
    }
    changed
}

impl MiranteWorkbenchApp {
    pub(crate) fn apply_workbench_ui_output(
        &mut self,
        ui: &mut egui::Ui,
        output: WorkbenchUiOutput,
    ) -> WorkbenchUiApplyTiming {
        let WorkbenchUiOutput {
            application_commands,
            import_commands,
            actions,
            viewport_observations,
            cross_section_readout_requests,
            render_requests,
            presentation_paints,
            mut rerender_requested,
            texture_refresh_requested,
        } = output;

        let command_apply_started = Instant::now();
        if !cross_section_readout_requests.is_empty() {
            let snapshot = self.application.snapshot();
            let view = application_view(&snapshot);
            for request in cross_section_readout_requests {
                let panel_id = PanelId::from_application_panel(request.panel());
                let presentation = request.presentation();
                let [normalized_x, normalized_y] = request.normalized_point();
                if let Some(readout) = cross_section_hover_readout_for_panel_point(
                    &self.render_coordination,
                    self.dataset.retained_leases(),
                    cross_section_readout::CrossSectionReadoutInput {
                        view,
                        catalog: snapshot.catalog(),
                    },
                    panel_id,
                    normalized_x * presentation.width_points(),
                    normalized_y * presentation.height_points(),
                    presentation,
                ) {
                    self.egui_ui.hovered_pixel = None;
                    self.egui_ui.hovered_source_readout = Some(readout.text);
                }
            }
        }
        for action in actions {
            match action {
                WorkbenchUiAction::OpenDatasetDialog => {
                    self.open_native_from_dialog(ui.ctx());
                }
                WorkbenchUiAction::NewProject => self.new_current_project(),
                WorkbenchUiAction::OpenProjectDialog => {
                    self.open_session_from_dialog(ui.ctx());
                }
                WorkbenchUiAction::SaveProject => {
                    self.save_current_project();
                }
                WorkbenchUiAction::SaveProjectAs => {
                    self.save_current_project_as();
                }
                WorkbenchUiAction::OpenProjectRecovery => {
                    self.open_project_recovery_panel();
                }
                WorkbenchUiAction::ImportTiffDirectoryDialog => {
                    self.import_tiff_directory_from_dialog(ui.ctx());
                }
                WorkbenchUiAction::ImportTiffFileDialog => {
                    self.import_tiff_file_from_dialog(ui.ctx());
                }
                WorkbenchUiAction::CopySelectedAnalysisCsv => {
                    let snapshot = self.application.snapshot();
                    let transient = snapshot.transient();
                    match export_selected_analysis_table(
                        &mut self.analysis_runtime,
                        AnalysisTableExportInput {
                            table_descriptors: transient.analysis_tables(),
                            selected_table: transient.selected_analysis_table(),
                        },
                    ) {
                        Ok(_) => {
                            if let Some(csv) = self.analysis_runtime.last_export_csv() {
                                ui.ctx().copy_text(csv.to_owned());
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "analysis table export rejected");
                        }
                    }
                }
                WorkbenchUiAction::CancelAnalysis => {
                    if let Err(error) = self.request_analysis_cancel() {
                        self.project_status_message =
                            Some(format!("Analysis could not be cancelled: {error}"));
                    }
                }
                WorkbenchUiAction::SetAnalysisRoi { origin, shape } => {
                    if let Err(error) = self.analysis_runtime.set_roi(origin, shape) {
                        tracing::warn!(%error, "analysis box was rejected");
                    }
                }
                WorkbenchUiAction::StartAnalysis(kind) => {
                    let scope = match kind {
                        WorkbenchAnalysisKind::FullTimeTrace => {
                            analysis_product::ProductAnalysisScope::FullTimeTrace
                        }
                        WorkbenchAnalysisKind::CurrentTimepointBox => {
                            analysis_product::ProductAnalysisScope::CurrentTimepointBox
                        }
                    };
                    if let Err(error) = self.start_product_analysis(scope) {
                        self.project_status_message =
                            Some(format!("Analysis could not start: {error}"));
                    }
                }
                WorkbenchUiAction::SaveSettings(draft) => {
                    match ResourcePolicy::new(
                        draft.cpu_dataset_budget_bytes,
                        draft.gpu_budget_bytes,
                    ) {
                        Ok(policy) => self.request_resource_policy_change(
                            policy,
                            RejectedFileDisposition::Preserve,
                        ),
                        Err(error) => tracing::warn!(
                            ?error,
                            "valid settings draft was rejected after widget construction"
                        ),
                    }
                }
                WorkbenchUiAction::ReplaceRejectedSettings(draft) => {
                    match ResourcePolicy::new(
                        draft.cpu_dataset_budget_bytes,
                        draft.gpu_budget_bytes,
                    ) {
                        Ok(policy) => self.request_resource_policy_change(
                            policy,
                            RejectedFileDisposition::ReplaceExplicitly,
                        ),
                        Err(error) => tracing::warn!(
                            ?error,
                            "valid settings draft was rejected after widget construction"
                        ),
                    }
                }
                WorkbenchUiAction::UseRecommendedSettings => {
                    match recommended_for_current_system(None) {
                        Ok(policy) => {
                            self.egui_ui.settings_runtime_draft = ui_kit::ResourcePolicyDraft {
                                cpu_dataset_budget_bytes: policy.cpu_dataset_budget_bytes(),
                                gpu_budget_bytes: policy.gpu_budget_bytes(),
                            };
                            ui.ctx().request_repaint();
                        }
                        Err(error) => {
                            tracing::warn!(?error, "recommended resource policy is unavailable")
                        }
                    }
                }
                WorkbenchUiAction::SaveDirtyProject | WorkbenchUiAction::SaveDirtyProjectAs => {
                    self.close_after_project_save = true;
                    let started = if action == WorkbenchUiAction::SaveDirtyProjectAs {
                        self.save_current_project_as()
                    } else {
                        self.save_current_project()
                    };
                    if started && self.close_after_project_save {
                        self.egui_ui.close_prompt_open = false;
                    } else if !started {
                        self.close_after_project_save = false;
                    }
                }
                WorkbenchUiAction::DiscardDirtyProject => {
                    self.egui_ui.close_prompt_open = false;
                    self.close_after_project_save = false;
                    if let Some(path) = self.pending_dataset_open_path.take() {
                        if let Err(error) = self.replace_state_from_dataset_path(path, None) {
                            self.project_status_message =
                                Some(format!("Dataset open could not start: {error}"));
                        }
                    } else {
                        self.request_project_store_close_for_exit();
                    }
                }
                WorkbenchUiAction::CancelDirtyProjectClose => {
                    self.egui_ui.close_prompt_open = false;
                    self.egui_ui.allow_close_without_prompt = false;
                    self.close_after_project_save = false;
                    self.pending_dataset_open_path = None;
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
                WorkbenchUiAction::RecoverReviewedAutosave(generation_id) => {
                    self.recover_project_candidate(generation_id);
                }
                WorkbenchUiAction::AcceptSavedProjectAfterRecoveryReview => {
                    self.accept_saved_project_after_recovery_review();
                }
                WorkbenchUiAction::CloseProjectRecoveryPanel => {
                    self.project_recovery_panel_open = false;
                }
                WorkbenchUiAction::OpenRecoveryCandidate(generation_id) => {
                    self.project_recovery_panel_open = false;
                    self.recover_project_candidate(generation_id);
                }
                WorkbenchUiAction::OpenRecoveryLocator(project_id) => {
                    self.project_recovery_panel_open = false;
                    self.open_recovery_locator(project_id);
                }
            }
        }

        if apply_viewport_observations(&mut self.render_coordination, viewport_observations) {
            ui.ctx().request_repaint();
        }

        for request in render_requests {
            if self.render_coordination.refresh_requested() {
                continue;
            }
            match request {
                RenderUiRequest::EnsureCrossSectionCurrent { panel } => {
                    let panel_id = PanelId::from_application_panel(panel);
                    let slot = panel_id.presentation_slot();
                    let before = self.render_coordination.surface(slot).clone();
                    match self.render_cross_section_panel_for_display_if_needed(panel_id) {
                        Ok(timing) => {
                            if timing.is_some() || self.render_coordination.surface(slot) != &before
                            {
                                ui.ctx().request_repaint();
                            }
                        }
                        Err(error) => {
                            if self.render_coordination.surface(slot) != &before {
                                ui.ctx().request_repaint();
                            }
                            tracing::error!(
                                %error,
                                panel = panel_id.label(),
                                "cross-section panel render failed"
                            );
                        }
                    }
                }
            }
        }

        // Resolve snapshot-built paints before a same-frame application
        // command can retire or replace their presentation token.
        for paint in presentation_paints {
            let slot = paint.slot();
            if let Err(error) = self.native_presentation.paint(ui, paint) {
                tracing::warn!(%error, ?slot, "native presentation request was rejected");
            }
        }

        for command in application_commands {
            if let Err(fault) = self.apply_application_command(command, ui.ctx()) {
                tracing::warn!(?fault, "UI application command rejected");
            }
        }
        let accepted_snapshot = self.application.snapshot();
        let accepted_tool = viewer_tool_for_kind(accepted_snapshot.transient().active_tool());
        if self.egui_ui.viewer_tools.active_tool != accepted_tool {
            self.egui_ui.viewer_tools.set_active_tool(accepted_tool);
        }
        let command_apply_ms = duration_ms(command_apply_started.elapsed());

        let display_refresh_trigger_started = Instant::now();
        rerender_requested |= self.render_coordination.take_refresh_request();
        if rerender_requested {
            self.refresh_frame(ui.ctx());
        } else if texture_refresh_requested {
            self.refresh_texture_only(ui.ctx());
        }
        let display_refresh_trigger_ms = duration_ms(display_refresh_trigger_started.elapsed());

        let import_action_started = Instant::now();
        for command in import_commands {
            self.apply_import_command(command, ui.ctx());
        }
        let import_action_ms = duration_ms(import_action_started.elapsed());

        WorkbenchUiApplyTiming {
            command_apply_ms,
            display_refresh_trigger_ms,
            import_action_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use mirante4d_application::RenderExtent;
    use mirante4d_render_api::{PresentationPaintRequest, PresentationToken};

    use super::*;

    #[test]
    fn workbench_output_returns_backend_neutral_presentation_paints() {
        let token = PresentationToken::new(17).unwrap();
        let request =
            PresentationPaintRequest::new(token, PresentationViewport::new(320.0, 240.0).unwrap());
        let paint = ui_kit::EguiPresentationPaint::new(
            PresentationSlot::ThreeD,
            request,
            egui::Rect::from_min_size(egui::pos2(4.0, 8.0), egui::vec2(320.0, 240.0)),
        );
        let output = WorkbenchUiOutput {
            presentation_paints: vec![paint],
            ..WorkbenchUiOutput::default()
        };

        assert_eq!(output.presentation_paints, vec![paint]);
        assert_eq!(output.presentation_paints[0].request().token(), token);
        assert_eq!(
            output.presentation_paints[0].slot(),
            PresentationSlot::ThreeD
        );
    }

    #[test]
    fn viewport_observations_coalesce_one_refresh_request() {
        let initial_presentation = PresentationViewport::new(320.0, 240.0).unwrap();
        let initial_render = RenderExtent::new(320, 240).unwrap();
        let mut coordination = RenderCoordinationState::new(
            FrameFidelityStatus::new_with_presentation(initial_render, initial_presentation),
        );
        let presentation = PresentationViewport::new(640.0, 360.0).unwrap();
        let render = RenderExtent::new(1280, 720).unwrap();
        let observation = ViewportObservation::new(PresentationSlot::Xy, presentation, render);

        assert!(apply_viewport_observations(
            &mut coordination,
            [observation, observation]
        ));
        assert_eq!(
            coordination
                .surface(PresentationSlot::Xy)
                .presentation_viewport(),
            Some(presentation)
        );
        assert_eq!(
            coordination.surface(PresentationSlot::Xy).render_viewport(),
            Some(render)
        );
        assert!(coordination.take_refresh_request());
        assert!(!coordination.take_refresh_request());

        assert!(!apply_viewport_observations(
            &mut coordination,
            [observation]
        ));
        assert!(!coordination.take_refresh_request());
    }
}
