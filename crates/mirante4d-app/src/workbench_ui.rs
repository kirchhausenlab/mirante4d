use super::*;
use mirante4d_render_api::{CameraFrame, RenderExtent};

use crate::viewer_layout::{PanelId, cross_section_schedule_status_label};

#[derive(Clone)]
struct ViewerUiSnapshot {
    presentation_viewport: PresentationViewport,
    render_viewport: RenderExtent,
    frame_fidelity: FrameFidelityStatus,
    linked_2d_fidelity: ui_kit::Linked2dFidelityStatus,
    renderer_initializing: bool,
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
    fn linked_2d_fidelity_status(
        &self,
        snapshot: &ApplicationSnapshot,
    ) -> ui_kit::Linked2dFidelityStatus {
        let view = application_view(snapshot);
        let active_layer = view.active_layer();
        let demand_currentness = self.visible_demand_plan_currentness();
        let demand_renderability = self.visible_demand_renderability();
        let planning_or_data_work = self.pending_visible_demand_plan.is_some()
            || self.dataset.visible_demand_plan_outstanding()
            || self.dataset.dispatcher().has_pending_work();
        let panels = [PanelId::Xy, PanelId::Xz, PanelId::Yz].map(|panel| {
            let surface = self.render_coordination.surface(panel.presentation_slot());
            let ideal_scale_level = surface
                .presentation_viewport()
                .zip(surface.render_viewport())
                .and_then(|(presentation, extent)| {
                    crate::dataset_demand_plan::cross_section_projected_layer_scales(
                        snapshot.catalog(),
                        view,
                        panel,
                        presentation,
                        extent,
                    )
                    .ok()
                })
                .and_then(|scales| scales.get(&active_layer).copied())
                .map(ScaleLevel::get);
            let schedule = surface.cross_section_schedule();
            let selected_scale_level = schedule.and_then(|schedule| schedule.target_scale_level);
            let displayed_scale_level = surface
                .layer_presentations()
                .iter()
                .find(|layer| layer.layer_ordinal == active_layer.ordinal())
                .and_then(|layer| layer.displayed_scale_level);
            let layer_presentation = surface
                .layer_presentations()
                .iter()
                .find(|layer| layer.layer_ordinal == active_layer.ordinal());
            let exact = demand_currentness.cross_section(panel);
            let provisional = schedule.is_some_and(|schedule| {
                schedule.status
                    == mirante4d_application::CrossSectionPanelScheduleStatus::Provisional
            }) || (demand_renderability.cross_section(panel) && !exact);
            let refining = !exact
                && (planning_or_data_work
                    || layer_presentation.is_some_and(|layer| layer.mixed)
                    || layer_presentation.is_some_and(|layer| {
                        layer.target_available_requirements < layer.target_total_requirements
                    })
                    || selected_scale_level != displayed_scale_level
                    || selected_scale_level != ideal_scale_level);
            ui_kit::LinkedPanelFidelityStatus {
                ideal_scale_level,
                selected_scale_level,
                displayed_scale_level,
                finest_fallback_scale_level: layer_presentation
                    .and_then(|layer| layer.finest_fallback_scale_level),
                fallback_scale_level: layer_presentation
                    .and_then(|layer| layer.fallback_scale_level),
                target_available_requirements: layer_presentation
                    .map_or(0, |layer| layer.target_available_requirements),
                target_total_requirements: layer_presentation
                    .map_or(0, |layer| layer.target_total_requirements),
                mixed: layer_presentation.is_some_and(|layer| layer.mixed),
                exact,
                provisional,
                refining,
                display_current: surface.display_current(),
            }
        });
        ui_kit::Linked2dFidelityStatus { panels }
    }

    fn viewer_ui_snapshot(&self, snapshot: &ApplicationSnapshot) -> ViewerUiSnapshot {
        let renderer_initializing = matches!(
            self.native_presentation.product_pipeline_readiness(),
            Some(Ok(
                mirante4d_render_wgpu::PipelineReadiness::CompilingInitial
            ))
        );
        let panel_placeholder = |panel_id: PanelId| {
            let panel = self
                .render_coordination
                .surface(panel_id.presentation_slot());
            if renderer_initializing {
                format!("{}\nRenderer initializing…", panel_id.label())
            } else if panel.render_failure().is_some() {
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
        if renderer_initializing {
            messages.push("Renderer initializing…".to_owned());
        }
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
        let mut frame_fidelity = self.render_coordination.frame_fidelity.clone();
        if renderer_initializing {
            // Dataset demand and decoded leases remain live while the device
            // compiles fixed pipelines. Do not project those data-plane facts
            // as a rendered frame before color execution is possible.
            frame_fidelity.completeness = FrameCompleteness::Loading;
            frame_fidelity.backend = RenderBackend::Loading;
            frame_fidelity.displayed_scale_level = None;
            frame_fidelity.display_freshness = DisplayedFrameFreshness::Unknown;
            frame_fidelity.frame_time_ms = None;
        }
        ViewerUiSnapshot {
            presentation_viewport: self.render_coordination.presentation_viewport,
            render_viewport: self.render_coordination.render_viewport,
            frame_fidelity,
            linked_2d_fidelity: self.linked_2d_fidelity_status(snapshot),
            renderer_initializing,
            composite_fidelity: if renderer_initializing {
                "Renderer initializing".to_owned()
            } else {
                composite_fidelity_label(snapshot, &self.render_coordination)
            },
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

    /// Consumes at most one renderer-owned compiler event per UI turn. The
    /// application observes transitions only to schedule product work; it
    /// keeps no parallel pipeline-readiness state.
    fn poll_product_renderer_pipeline_readiness(&mut self, ctx: &egui::Context) {
        let Some(product) = self.native_presentation.product_gpu.as_mut() else {
            return;
        };
        let before = product.renderer.pipeline_readiness();
        if let Err(error) = before {
            self.viewer_pick_queue.clear_unsubmitted();
            let error = anyhow::Error::new(error);
            if self.record_terminal_product_renderer_failure(&error) {
                ctx.request_repaint();
            }
            return;
        }
        let after = product.renderer.poll_pipeline_readiness();
        match (before, after) {
            (
                Ok(mirante4d_render_wgpu::PipelineReadiness::CompilingInitial),
                Ok(mirante4d_render_wgpu::PipelineReadiness::InitialRenderReady),
            ) => {
                // Refresh bits may have been consumed while demand and lease
                // offers continued during compilation. Render the latest
                // source and intent now that color capability exists.
                self.render_coordination.request_refresh();
                ctx.request_repaint();
            }
            (
                Ok(mirante4d_render_wgpu::PipelineReadiness::InitialRenderReady),
                Ok(mirante4d_render_wgpu::PipelineReadiness::Ready),
            ) => {
                if self.viewer_pick_queue.has_work() {
                    ctx.request_repaint();
                }
            }
            (_, Err(error)) => {
                self.viewer_pick_queue.clear_unsubmitted();
                let error = anyhow::Error::new(error);
                if self.record_terminal_product_renderer_failure(&error) {
                    ctx.request_repaint();
                }
            }
            _ => {}
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
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        real_interaction_trace::record_raw_input(raw_input);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_process_termination_request(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.handle_process_termination_request(ui.ctx()) {
            return;
        }
        self.poll_product_renderer_pipeline_readiness(ui.ctx());
        let update_started = Instant::now();
        let generation_at_start = self.render_coordination.display_generation();
        let active_input_at_start = generation_at_start.input_generation > 0
            && generation_at_start.current_presentation_generation
                != Some(generation_at_start.input_generation);
        let snapshot_at_start = self.application.snapshot();
        let loading_work_at_start = workbench_playback_runtime::background_work_active(
            &snapshot_at_start,
            &self.import.workers,
            &self.dataset,
            &self.render_coordination,
            &self.native_presentation,
            false,
        ) || self.dataset.visible_demand_plan_outstanding()
            || self.pending_visible_demand_plan.is_some();
        let heartbeat_at_ns = self.display_instrumentation_now_ns();
        self.render_coordination
            .record_main_loop_heartbeat(heartbeat_at_ns);
        self.pump_application_services();
        self.handle_close_request(ui.ctx());

        self.drain_tiff_import_setup_results(ui.ctx());
        self.drain_import_results(ui.ctx());
        self.commit_settled_render_intent_if_due(ui.ctx());
        // Scripted input must precede the immutable workbench snapshot so it
        // enters this turn's normal widget-to-render-root path.
        ProductAutomationController::drive(self, ui.ctx());

        let application_snapshot = self.application_snapshot_for_ui();
        let render_intent_base = RenderIntentBase::from_snapshot(&application_snapshot);
        self.render_intent_mailbox
            .synchronize_base(render_intent_base);
        let viewer_ui_snapshot = self.viewer_ui_snapshot(&application_snapshot);
        let view = application_view(&application_snapshot);
        let effective_camera = self
            .render_intent_mailbox
            .effective_camera(render_intent_base, *view.camera());
        let effective_cross_section = self
            .render_intent_mailbox
            .effective_cross_section(render_intent_base, *view.cross_section());
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
        let dataset_open_pending = self.pending_dataset_open.is_some();
        let project_status_message = self.project_status_message.clone();
        let source_verification_available = self
            .source_verification_service
            .as_ref()
            .is_some_and(|service| service.active_token().is_none());
        let runtime_diagnostics_view =
            runtime_diagnostics_panel::runtime_diagnostics_view_if_requested(
                self,
                self.egui_ui.runtime_diagnostics_open,
            );
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
            CameraFrame::new(effective_camera, viewer_ui_snapshot.presentation_viewport).ok();
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
                    linked_2d_fidelity: viewer_ui_snapshot.linked_2d_fidelity,
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
                    runtime_diagnostics: runtime_diagnostics_view.as_ref(),
                    camera: camera_inspector_view,
                    messages: &viewer_ui_snapshot.messages,
                },
                viewer: ui_kit::ViewerWorkbenchView {
                    application: &application_snapshot,
                    effective_camera,
                    effective_cross_section,
                    frame_fidelity: &viewer_ui_snapshot.frame_fidelity,
                    renderer_initializing: viewer_ui_snapshot.renderer_initializing,
                    fallback_render_extent: viewer_ui_snapshot.render_viewport,
                    render_extent_envelope: WgpuRenderRuntime::extent_envelope(),
                    xy_placeholder: &viewer_ui_snapshot.xy_placeholder,
                    xz_placeholder: &viewer_ui_snapshot.xz_placeholder,
                    yz_placeholder: &viewer_ui_snapshot.yz_placeholder,
                    test_render_viewport_max_side: viewer_ui_snapshot.test_render_viewport_max_side,
                    automation_render_target: viewer_ui_snapshot.automation_render_target,
                    interaction: ui_kit::ViewerInteractionConfig {
                        camera_settle_duration: CROSS_SECTION_INTERACTION_SETTLE_DURATION,
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
        self.pump_viewer_pick(ui.ctx());
        self.update_source_verification_interactive_busy();

        let snapshot = self.application.snapshot();
        let project_store_pending = self
            .project_store
            .as_ref()
            .is_some_and(ProjectStoreApplicationService::has_pending_work);
        let progressive_render_work =
            workbench_playback_runtime::progressive_render_submission_work(
                &self.native_presentation,
            );
        let background_work_active = workbench_playback_runtime::background_work_active(
            &snapshot,
            &self.import.workers,
            &self.dataset,
            &self.render_coordination,
            &self.native_presentation,
            progressive_render_work.any_required,
        );
        if workbench_playback_runtime::renderer_progress_refresh_required(
            progressive_render_work,
            self.dataset.dispatcher().has_pending_work(),
        ) {
            self.render_coordination.request_refresh();
            // Renderer-owned work is already CPU/GPU ready. In particular,
            // an atomic 3D successor can be waiting behind an earlier
            // interaction turn even when no dataset worker remains active.
            // Schedule the transaction immediately; the 50 ms background
            // cadence is for external/work-queue polling, not ready color
            // continuation.
            ui.ctx().request_repaint();
        }
        let foreground_cross_section_loading = application_view(&snapshot).layout()
            == CanonicalViewerLayout::FourPanel
            && [
                dataset_requests::SCOPE_CROSS_SECTION_XY,
                dataset_requests::SCOPE_CROSS_SECTION_XZ,
                dataset_requests::SCOPE_CROSS_SECTION_YZ,
            ]
            .into_iter()
            .any(|scope| {
                self.dataset.scope_is_installed(scope)
                    && !workbench_brick_runtime::scope_resources_complete_with_renderer(
                        &self.dataset,
                        &self.native_presentation,
                        scope,
                    )
            });
        let visible_demand_transaction_pending = self.pending_visible_demand_plan.is_some()
            || self.dataset.visible_demand_plan_outstanding();
        let active_render_interaction = self
            .render_intent_mailbox
            .active_target(render_intent_base)
            .is_some();
        if active_render_interaction {
            ui.ctx().request_repaint();
        } else if foreground_cross_section_loading || visible_demand_transaction_pending {
            ui.ctx()
                .request_repaint_after(FOREGROUND_VISIBLE_WORK_REPAINT_INTERVAL);
        } else if background_work_active
            || workbench_playback_runtime::source_verification_polling_required(
                self.pending_automatic_source_verification.is_some(),
                self.source_verification_service
                    .as_ref()
                    .is_some_and(|service| service.active_token().is_some()),
            )
            || project_store_pending
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

        let foreground_idle = !background_work_active
            && !self.dataset.visible_demand_plan_outstanding()
            && self.pending_visible_demand_plan.is_none();
        self.observe_coordinated_display_milestones(foreground_idle);

        let final_snapshot = self.application.snapshot();
        let four_panel =
            application_view(&final_snapshot).layout() == CanonicalViewerLayout::FourPanel;
        let source_verified = matches!(
            final_snapshot.source(),
            SourceVerificationSnapshot::Verified(_)
        );
        let complete =
            self.render_coordination.frame_fidelity.completeness == FrameCompleteness::Complete;
        let current = self.render_coordination.frame_fidelity.display_freshness
            == DisplayedFrameFreshness::Current;
        let all_presentations = PresentationSlot::ALL.into_iter().all(|slot| {
            final_snapshot
                .presentations()
                .get(slot)
                .is_some_and(|presentation| presentation.frame().is_some())
        });
        let displayed_scale = self
            .render_coordination
            .frame_fidelity
            .displayed_scale_level;
        let selected_scale = self.render_coordination.frame_fidelity.target_scale_level;
        let ideal_scale = self.render_coordination.frame_fidelity.ideal_scale_level;
        // Source verification and hidden refinement are deliberately not
        // readiness prerequisites. The owner-visible failure occurs while the
        // normal app is usable, and those background services must not turn
        // this finite interaction check into another idle launch protocol.
        let ready = four_panel && complete && current && displayed_scale == Some(3);
        let state_flags = u64::from(four_panel)
            | (u64::from(source_verified) << 1)
            | (u64::from(complete) << 2)
            | (u64::from(current) << 3)
            | (u64::from(all_presentations) << 4)
            | (u64::from(foreground_idle) << 5)
            | (u64::from(
                self.render_coordination
                    .frame_fidelity
                    .adaptive_capacity_limited,
            ) << 6)
            | (u64::from(
                self.render_coordination
                    .frame_fidelity
                    .last_capacity_error
                    .is_some(),
            ) << 7)
            | (u64::from(self.render_coordination.frame_fidelity.refinement_pending) << 8);
        real_interaction_trace::record_ui_end(
            ready,
            displayed_scale,
            selected_scale,
            ideal_scale,
            state_flags,
        );
        if real_interaction_trace::enabled() && four_panel {
            let demand_currentness = self.visible_demand_plan_currentness();
            let demand_renderability = self.visible_demand_renderability();
            let view = application_view(&final_snapshot);
            let active_layer = view.active_layer();
            let linked = [PanelId::Xy, PanelId::Xz, PanelId::Yz].map(|panel| {
                let surface = self.render_coordination.surface(panel.presentation_slot());
                let ideal = surface
                    .presentation_viewport()
                    .zip(surface.render_viewport())
                    .and_then(|(presentation, extent)| {
                        crate::dataset_demand_plan::cross_section_projected_layer_scales(
                            final_snapshot.catalog(),
                            view,
                            panel,
                            presentation,
                            extent,
                        )
                        .ok()
                    })
                    .and_then(|scales| scales.get(&active_layer).copied())
                    .map(ScaleLevel::get);
                let scope = match panel {
                    PanelId::Xy => crate::dataset_requests::SCOPE_CROSS_SECTION_XY,
                    PanelId::Xz => crate::dataset_requests::SCOPE_CROSS_SECTION_XZ,
                    PanelId::Yz => crate::dataset_requests::SCOPE_CROSS_SECTION_YZ,
                    PanelId::ThreeD => unreachable!(),
                };
                let installed = self
                    .dataset
                    .scope_layer_scales(scope)
                    .and_then(|scales| scales.get(&active_layer))
                    .copied()
                    .map(ScaleLevel::get);
                let displayed = surface
                    .layer_presentations()
                    .iter()
                    .find(|layer| layer.layer_ordinal == active_layer.ordinal())
                    .and_then(|layer| layer.displayed_scale_level);
                let exact = demand_currentness.cross_section(panel);
                let provisional = demand_renderability.cross_section(panel) && !exact;
                (
                    ideal,
                    installed,
                    displayed,
                    exact,
                    provisional,
                    surface.display_current(),
                )
            });
            real_interaction_trace::record_linked_lod_status(linked);
            let base = mirante4d_application::RenderIntentBase::from_snapshot(&final_snapshot);
            let active_linked = matches!(
                self.render_intent_mailbox.active_target(base),
                Some(mirante4d_application::RenderIntentTarget::CrossSection(_))
            );
            let scopes = [
                crate::dataset_requests::SCOPE_CROSS_SECTION_XY,
                crate::dataset_requests::SCOPE_CROSS_SECTION_XZ,
                crate::dataset_requests::SCOPE_CROSS_SECTION_YZ,
            ];
            let complete = scopes.map(|scope| self.dataset.scope_all_resources_complete(scope));
            let runtime_status = u64::from(active_linked)
                | (u64::from(self.resident_cross_section_coverage.is_some()) << 1)
                | (u64::from(self.pending_visible_demand_plan.is_some()) << 2)
                | (u64::from(self.dataset.visible_demand_plan_outstanding()) << 3)
                | (u64::from(self.dataset.dispatcher().has_pending_work()) << 4)
                | (u64::from(self.dataset.last_plan_error().is_some()) << 5)
                | (u64::from(complete[0]) << 6)
                | (u64::from(complete[1]) << 7)
                | (u64::from(complete[2]) << 8)
                | (u64::from(demand_currentness.cross_sections) << 9)
                | (u64::from(demand_renderability.cross_sections) << 10);
            if real_interaction_trace::record_linked_runtime_status(runtime_status) {
                real_interaction_trace::record_linked_runtime_detail(&format!(
                    "plan_error={:?}\nplanner={:?}\nrenderer={:?}\n",
                    self.dataset.last_plan_error(),
                    self.dataset.visible_demand_diagnostics(),
                    self.native_presentation
                        .product_gpu
                        .as_ref()
                        .map(|product| (
                            product.last_coordinated_recorded_targets.as_ref(),
                            product.last_coordinated_color_submissions,
                            product.total_coordinated_color_submissions,
                            product.last_coordinated_residency_submissions,
                        )),
                ));
            }
            let planner = self.dataset.visible_demand_diagnostics();
            for (kind, value) in [
                ("demand_plans_submitted", planner.submitted),
                ("demand_plans_completed", planner.completed),
                ("demand_plans_cancelled", planner.cancelled_running),
                (
                    "demand_planning_completed_ns",
                    planner.completed_planning_time_ns,
                ),
            ] {
                real_interaction_trace::record_boundary_counter(kind, value);
            }
            if let Ok(runtime) = self.dataset.dispatcher().diagnostics() {
                let performance = runtime.performance();
                for (kind, value) in [
                    ("dataset_requests_submitted", runtime.submitted_requests()),
                    ("dataset_decodes_started", runtime.started_decodes()),
                    ("dataset_decodes_completed", runtime.completed_decodes()),
                    ("dataset_requests_ready", runtime.ready_requests()),
                    ("dataset_requests_cancelled", runtime.cancelled_requests()),
                    ("dataset_requests_failed", runtime.failed_requests()),
                    ("dataset_queue_wait_ns", performance.queue_wait_ns()),
                    ("dataset_decode_time_ns", performance.decode_time_ns()),
                    (
                        "dataset_decoded_output_bytes",
                        performance.decoded_output_bytes(),
                    ),
                ] {
                    real_interaction_trace::record_boundary_counter(kind, value);
                }
            }
            if let Some(source) = self.dataset.local_source_diagnostics() {
                for (kind, value) in [
                    (
                        "source_physical_range_reads",
                        source.reader.physical_range_read_operations,
                    ),
                    (
                        "source_physical_encoded_bytes",
                        source.reader.physical_encoded_bytes_read,
                    ),
                    (
                        "source_codec_decodes",
                        source.reader.codec_decode_operations,
                    ),
                    (
                        "source_codec_decoded_bytes",
                        source.reader.codec_decoded_bytes,
                    ),
                    (
                        "source_codec_decode_time_ns",
                        source.reader.codec_decode_time_ns,
                    ),
                ] {
                    real_interaction_trace::record_boundary_counter(kind, value);
                }
            }
            if let Some(product) = self.native_presentation.product_gpu.as_ref() {
                let renderer = product.renderer.diagnostics();
                for (kind, value) in [
                    ("renderer_frames_executed", renderer.frames_executed()),
                    ("renderer_queue_submissions", renderer.queue_submissions()),
                    ("renderer_uploaded_resources", renderer.uploaded_resources()),
                    (
                        "renderer_uploaded_payload_bytes",
                        renderer.uploaded_payload_bytes(),
                    ),
                    (
                        "renderer_color_submissions",
                        product.total_coordinated_color_submissions,
                    ),
                ] {
                    real_interaction_trace::record_boundary_counter(kind, value);
                }
            }
        }

        let generation_at_end = self.render_coordination.display_generation();
        let active_input_at_end = generation_at_end.input_generation > 0
            && generation_at_end.current_presentation_generation
                != Some(generation_at_end.input_generation);
        let loading_work_at_end = !foreground_idle;
        if active_input_at_start
            || active_input_at_end
            || loading_work_at_start
            || loading_work_at_end
        {
            let duration_ns =
                u64::try_from(update_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            self.render_coordination
                .record_active_ui_update_duration(duration_ns);
            real_interaction_trace::record_ui_update_duration(duration_ns);
        }
    }

    fn on_exit(&mut self) {
        real_interaction_trace::finish();
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
