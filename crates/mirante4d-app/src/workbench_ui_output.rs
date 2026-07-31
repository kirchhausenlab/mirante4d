//! Native resolution of typed workbench UI output.

use super::*;
use crate::viewer_layout::PanelId;

pub(crate) fn apply_viewport_observations(
    render_coordination: &mut RenderCoordinationState,
    render_intent_mailbox: &mut RenderIntentMailbox,
    observations: impl IntoIterator<Item = ViewportObservation>,
) -> Result<Option<CoordinatedPresentationGroup>, RenderIntentMailboxError> {
    let mut three_d_changed = false;
    let mut linked_2d_changed = false;
    for observation in observations {
        let slot = observation.slot();
        if !render_coordination.record_viewports(
            observation.slot(),
            observation.presentation(),
            observation.render(),
        ) {
            continue;
        }
        if slot == PresentationSlot::ThreeD {
            three_d_changed = true;
        } else {
            linked_2d_changed = true;
        }
    }
    let required_group = match (three_d_changed, linked_2d_changed) {
        (true, true) => Some(CoordinatedPresentationGroup::FullLayout),
        (true, false) => Some(CoordinatedPresentationGroup::ThreeD),
        (false, true) => Some(CoordinatedPresentationGroup::Linked2d),
        (false, false) => None,
    };
    if let Some(group) = required_group {
        // A render extent participates in the requirement body. Give the
        // affected coalesced family one new frame identity before planning
        // can bind that changed body.
        let family = match group {
            CoordinatedPresentationGroup::ThreeD => RenderIntentFamily::ThreeD,
            CoordinatedPresentationGroup::Linked2d => RenderIntentFamily::Linked2d,
            CoordinatedPresentationGroup::FullLayout => RenderIntentFamily::Both,
        };
        render_intent_mailbox.observe_durable_intent(family)?;
        render_coordination.request_refresh();
    }
    Ok(required_group)
}

impl MiranteWorkbenchApp {
    pub(crate) fn request_repaint_for_queued_display_refresh(&self, ctx: &egui::Context) {
        if self.render_coordination.refresh_requested() {
            ctx.request_repaint();
        }
    }

    pub(crate) fn apply_workbench_ui_output(
        &mut self,
        ui: &mut egui::Ui,
        output: WorkbenchUiOutput,
    ) {
        let WorkbenchUiOutput {
            application_commands,
            render_intent_interactions,
            import_commands,
            actions,
            viewport_observations,
            cross_section_readout_requests,
            viewer_pick_request,
            render_requests,
            presentation_paints,
            mut rerender_requested,
            texture_refresh_requested,
            repaint_after,
        } = output;

        if self
            .viewer_pick_queue
            .observe_ui_request(viewer_pick_request)
        {
            self.egui_ui.hovered_pixel = None;
            self.egui_ui.viewer_tools.clear_hover();
        }

        if !cross_section_readout_requests.is_empty() {
            let snapshot = self.application.snapshot();
            let view = application_view(&snapshot);
            let base = RenderIntentBase::from_snapshot(&snapshot);
            let durable_cross_section = *view.cross_section();
            let active_target = self.render_intent_mailbox.active_target(base);
            let demand_currentness = self.visible_demand_plan_currentness();
            for request in cross_section_readout_requests {
                let panel_id = PanelId::from_application_panel(request.panel());
                let presentation = request.presentation();
                let [normalized_x, normalized_y] = request.normalized_point();
                let cross_section = if active_target
                    == Some(RenderIntentTarget::CrossSection(request.panel()))
                    && demand_currentness.cross_section(panel_id)
                    && self
                        .render_coordination
                        .surface(panel_id.presentation_slot())
                        .display_current()
                {
                    self.render_intent_mailbox
                        .effective_cross_section(base, durable_cross_section)
                } else {
                    durable_cross_section
                };
                if let Some(readout) = cross_section_hover_readout_for_panel_point(
                    &self.render_coordination,
                    self.dataset.retained_leases(),
                    cross_section_readout::CrossSectionReadoutInput {
                        view,
                        cross_section,
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
                    match recommended_for_current_system(
                        self.selected_adapter_memory.recommended_capacity_bytes(),
                    ) {
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
                    if self.pending_dataset_open.is_some() {
                        if let Err(error) =
                            self.continue_pending_dataset_open_after_project_decision(None)
                        {
                            self.project_status_message =
                                Some(format!("Dataset open could not begin: {error}"));
                        }
                    } else {
                        self.egui_ui.allow_close_without_prompt = true;
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                WorkbenchUiAction::CancelDirtyProjectClose => {
                    self.egui_ui.close_prompt_open = false;
                    self.egui_ui.allow_close_without_prompt = false;
                    self.close_after_project_save = false;
                    self.cancel_pending_dataset_open();
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
                WorkbenchUiAction::CopyDiagnostics => {
                    ui.ctx().copy_text(self.diagnostics_summary_text());
                }
            }
        }
        if let Some(delay) = repaint_after {
            ui.ctx().request_repaint_after(delay);
        }

        match apply_viewport_observations(
            &mut self.render_coordination,
            &mut self.render_intent_mailbox,
            viewport_observations,
        ) {
            Ok(Some(required_group)) => {
                self.begin_display_input_generation(required_group);
                ui.ctx().request_repaint();
            }
            Ok(None) => {}
            Err(error) => {
                tracing::error!(%error, "viewport render-intent revision could not advance");
            }
        }
        if !render_requests.is_empty() && !self.render_coordination.refresh_requested() {
            // Per-panel widgets report dirty visibility, but the composition
            // root resolves every visible target through one coordinated
            // layout and one renderer cutoff below.
            rerender_requested = true;
        }

        // Resolve snapshot-built paints before a same-frame application
        // command can retire or replace their fixed-target texture revision.
        for paint in presentation_paints {
            let slot = paint.slot();
            real_interaction_trace::record_presentation_target(
                slot,
                paint.rect(),
                ui.ctx().pixels_per_point(),
            );
            if let Err(error) = self.native_presentation.paint(ui, paint) {
                tracing::warn!(%error, ?slot, "native presentation request was rejected");
            }
        }

        for interaction in render_intent_interactions {
            if let Err(error) = self.apply_render_intent_interaction(interaction, ui.ctx()) {
                tracing::warn!(%error, "render interaction was rejected");
            }
        }
        for command in application_commands {
            if let Err(fault) = self.apply_application_command(command, ui.ctx()) {
                tracing::warn!(?fault, "UI application command rejected");
            }
        }
        rerender_requested |= self.render_coordination.take_refresh_request();
        let published_after_paint = if rerender_requested {
            // Resident interactions may already have taken the immediate
            // low-latency path above. P3 keeps that responsiveness contract:
            // this remains a second coordinated observation, while clean
            // renderer targets guarantee it records no second color cutoff.
            self.refresh_frame()
        } else if texture_refresh_requested {
            self.refresh_texture_only()
        } else {
            false
        };
        if published_after_paint {
            // Presentation paints were resolved earlier in this UI turn.
            // Guarantee one composition turn that can actually expose the
            // renderer publication on the mapped window.
            ui.ctx().request_repaint();
        }
        // A refresh can publish the cold coarse predecessor and enqueue the
        // renderer-owned exact successor during this UI turn. The queued bit is the
        // next display transaction, not merely bookkeeping; make one future
        // event-loop turn inevitable even when dataset work is already idle.
        self.request_repaint_for_queued_display_refresh(ui.ctx());
        for command in import_commands {
            self.apply_import_command(command, ui.ctx());
        }
    }

    pub(crate) fn apply_render_intent_interaction(
        &mut self,
        interaction: RenderIntentInteraction,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        match interaction {
            RenderIntentInteraction::Sample(sample) => {
                let snapshot = self.application.snapshot();
                let base = RenderIntentBase::from_snapshot(&snapshot);
                let resident_camera = match sample.payload() {
                    RenderIntentPayload::Camera(camera) => {
                        real_interaction_trace::record_camera_sample(
                            camera,
                            base.currentness().get(),
                        );
                        self.prepare_resident_camera_intent(camera)
                    }
                    RenderIntentPayload::CrossSection(_) => false,
                };
                let now_ns = self.display_instrumentation_now_ns();
                let revision = self
                    .render_intent_mailbox
                    .sample(base, sample, now_ns, resident_camera)
                    .map_err(|error| error.to_string())?;
                let resident_plane = match (sample.target(), sample.payload()) {
                    (
                        RenderIntentTarget::CrossSection(panel),
                        RenderIntentPayload::CrossSection(cross_section),
                    ) => self.prepare_resident_cross_section_intent(
                        revision,
                        PanelId::from_application_panel(panel),
                        cross_section,
                    ),
                    _ => false,
                };
                if resident_plane {
                    self.render_intent_mailbox.mark_renderable(base, revision);
                }
                let group = match sample.target() {
                    RenderIntentTarget::ThreeD => CoordinatedPresentationGroup::ThreeD,
                    RenderIntentTarget::CrossSection(_) => CoordinatedPresentationGroup::Linked2d,
                };
                self.begin_display_input_generation(group);
                if sample.target() == RenderIntentTarget::ThreeD {
                    self.render_coordination.mark_3d_display_stale();
                }
                let demand_plan_required = match sample.target() {
                    RenderIntentTarget::ThreeD => !resident_camera,
                    RenderIntentTarget::CrossSection(_) => {
                        !resident_plane || self.resident_cross_section_requires_planning()
                    }
                };
                if demand_plan_required {
                    self.request_transient_visible_demand(revision, sample.target());
                }
                let published_after_paint =
                    (resident_camera || resident_plane) && self.refresh_frame();
                if published_after_paint {
                    ctx.request_repaint();
                }
                // Nonresident input needs an immediate turn while
                // asynchronous demand starts. A resident publication also
                // needs exactly one later composition turn because the paint
                // list for this turn was resolved before the texture changed.
                if !resident_camera && !resident_plane {
                    ctx.request_repaint();
                }
                Ok(())
            }
            RenderIntentInteraction::Finish(target) => {
                let base = RenderIntentBase::from_snapshot(&self.application.snapshot());
                let Some(intent) = self.render_intent_mailbox.finish(base, target) else {
                    return Ok(());
                };
                self.commit_render_intent(intent, ctx)
            }
        }
    }

    pub(crate) fn commit_settled_render_intent_if_due(&mut self, ctx: &egui::Context) {
        let base = RenderIntentBase::from_snapshot(&self.application.snapshot());
        let now_ns = self.display_instrumentation_now_ns();
        let settle_ns =
            u64::try_from(CROSS_SECTION_INTERACTION_SETTLE_DURATION.as_nanos()).unwrap_or(u64::MAX);
        if let Some(intent) = self
            .render_intent_mailbox
            .finish_due(base, now_ns, settle_ns)
        {
            if let Err(error) = self.commit_render_intent(intent, ctx) {
                tracing::warn!(%error, "settled render interaction was rejected");
            }
            return;
        }
        if let Some(remaining_ns) = self
            .render_intent_mailbox
            .scroll_settle_remaining_ns(base, now_ns, settle_ns)
            && remaining_ns > 0
        {
            ctx.request_repaint_after(Duration::from_nanos(remaining_ns));
        }
    }

    fn commit_render_intent(
        &mut self,
        intent: CompletedRenderIntent,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let completed_frame = intent.revision();
        match (intent.target(), intent.payload()) {
            (RenderIntentTarget::ThreeD, RenderIntentPayload::Camera(camera)) => self
                .apply_application_command_with_revision_policy(
                    ApplicationCommand::SetCamera(camera),
                    ctx,
                    false,
                )
                .map(|_| ())
                .map_err(|fault| format!("camera interaction commit was rejected: {fault:?}")),
            (
                RenderIntentTarget::CrossSection(panel),
                RenderIntentPayload::CrossSection(cross_section),
            ) => {
                self.apply_application_command(
                    ApplicationCommand::SetActiveCrossSectionPanel(Some(panel)),
                    ctx,
                )
                .map_err(|fault| {
                    format!("cross-section interaction focus was rejected: {fault:?}")
                })?;
                let layout = self.application.snapshot().view().layout();
                self.apply_application_command_with_revision_policy(
                    ApplicationCommand::SetLayout {
                        layout,
                        cross_section,
                    },
                    ctx,
                    false,
                )
                .map_err(|fault| {
                    format!("cross-section interaction commit was rejected: {fault:?}")
                })?;
                match self.adopt_completed_exact_cross_sections(completed_frame, cross_section) {
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::debug!(
                            ?panel,
                            ?completed_frame,
                            "completed cross-section requires normal durable rendering"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            ?panel,
                            ?completed_frame,
                            "completed exact cross-section could not be adopted"
                        );
                    }
                }
                Ok(())
            }
            _ => Err("completed render interaction target/payload mismatch".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use mirante4d_application::RenderExtent;
    use mirante4d_render_api::{PresentationPaintRequest, PresentationTarget};

    use super::*;

    #[test]
    fn workbench_output_returns_backend_neutral_presentation_paints() {
        let target = PresentationTarget::ThreeD;
        let request =
            PresentationPaintRequest::new(target, PresentationViewport::new(320.0, 240.0).unwrap());
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
        assert_eq!(output.presentation_paints[0].request().target(), target);
        assert_eq!(
            output.presentation_paints[0].slot(),
            PresentationSlot::ThreeD
        );
    }

    #[test]
    fn viewport_observations_allocate_exactly_one_frame_identity_for_one_changed_batch() {
        let initial_presentation = PresentationViewport::new(320.0, 240.0).unwrap();
        let initial_render = RenderExtent::new(320, 240).unwrap();
        let mut coordination = RenderCoordinationState::new(
            FrameFidelityStatus::new_with_presentation(initial_render, initial_presentation),
        );
        let mut mailbox = RenderIntentMailbox::new();
        let initial_revision = mailbox.snapshot().latest_revision;
        let presentation = PresentationViewport::new(640.0, 360.0).unwrap();
        let render = RenderExtent::new(1280, 720).unwrap();
        let observations = [
            ViewportObservation::new(PresentationSlot::ThreeD, presentation, render),
            ViewportObservation::new(PresentationSlot::Xy, presentation, render),
            ViewportObservation::new(PresentationSlot::Xz, presentation, render),
            ViewportObservation::new(PresentationSlot::Yz, presentation, render),
        ];

        assert_eq!(
            apply_viewport_observations(&mut coordination, &mut mailbox, observations),
            Ok(Some(CoordinatedPresentationGroup::FullLayout))
        );
        assert_eq!(
            mailbox.snapshot().latest_revision.get(),
            initial_revision.get() + 1
        );
        for slot in PresentationSlot::ALL {
            assert_eq!(
                coordination.surface(slot).presentation_viewport(),
                Some(presentation)
            );
            assert_eq!(coordination.surface(slot).render_viewport(), Some(render));
        }
        assert!(coordination.take_refresh_request());
        assert!(!coordination.take_refresh_request());

        assert_eq!(
            apply_viewport_observations(&mut coordination, &mut mailbox, observations),
            Ok(None)
        );
        assert_eq!(
            mailbox.snapshot().latest_revision.get(),
            initial_revision.get() + 1
        );
        assert!(!coordination.take_refresh_request());
    }

    #[test]
    fn viewport_observations_classify_mixed_targets_as_full_layout() {
        let initial_presentation = PresentationViewport::new(320.0, 240.0).unwrap();
        let initial_render = RenderExtent::new(320, 240).unwrap();
        let mut coordination = RenderCoordinationState::new(
            FrameFidelityStatus::new_with_presentation(initial_render, initial_presentation),
        );
        let mut mailbox = RenderIntentMailbox::new();
        let initial_revision = mailbox.snapshot().latest_revision;
        let presentation = PresentationViewport::new(640.0, 360.0).unwrap();
        let render = RenderExtent::new(1280, 720).unwrap();

        assert_eq!(
            apply_viewport_observations(
                &mut coordination,
                &mut mailbox,
                [
                    ViewportObservation::new(PresentationSlot::ThreeD, presentation, render),
                    ViewportObservation::new(PresentationSlot::Xy, presentation, render),
                ],
            ),
            Ok(Some(CoordinatedPresentationGroup::FullLayout))
        );
        assert_eq!(
            mailbox.snapshot().latest_revision.get(),
            initial_revision.get() + 1
        );
    }
}
