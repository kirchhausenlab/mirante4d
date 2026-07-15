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
            native_actions,
            viewport_observations,
            render_requests,
            presentation_paints,
            mut rerender_requested,
            texture_refresh_requested,
        } = output;

        let command_apply_started = Instant::now();
        for action in native_actions {
            match action {
                NativeWorkbenchAction::OpenDatasetDialog => {
                    self.open_native_from_dialog(ui.ctx());
                }
                NativeWorkbenchAction::NewProject => self.new_current_project(),
                NativeWorkbenchAction::OpenProjectDialog => {
                    self.open_session_from_dialog(ui.ctx());
                }
                NativeWorkbenchAction::SaveProject => {
                    self.save_current_project();
                }
                NativeWorkbenchAction::SaveProjectAs => {
                    self.save_current_project_as();
                }
                NativeWorkbenchAction::OpenProjectRecovery => {
                    self.open_project_recovery_panel();
                }
                NativeWorkbenchAction::ImportTiffDirectoryDialog => {
                    self.import_tiff_directory_from_dialog(ui.ctx());
                }
                NativeWorkbenchAction::ImportTiffFileDialog => {
                    self.import_tiff_file_from_dialog(ui.ctx());
                }
                NativeWorkbenchAction::CopySelectedAnalysisCsv => {
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
