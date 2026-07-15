use super::*;
use mirante4d_application::CrossSectionPanelId;
use mirante4d_domain::ToolKind;
use mirante4d_render_api::{CameraFrame, RenderExtent};

use crate::viewer_layout::{PanelId, cross_section_schedule_status_label};

const CROSS_SECTION_SCROLL_POINTS_PER_NOTCH: f32 = 120.0;
const CROSS_SECTION_SCROLL_ZOOM_FACTOR_SCALE: f32 = 0.001;

#[derive(Clone)]
struct ViewerUiSnapshot {
    presentation_viewport: PresentationViewport,
    render_viewport: RenderExtent,
    frame_fidelity: FrameFidelityStatus,
    composite_fidelity: String,
    dataset_path: String,
    messages: Vec<String>,
    three_d_display: ViewportDisplayImage,
    xy_display: Option<ViewportDisplayImage>,
    xz_display: Option<ViewportDisplayImage>,
    yz_display: Option<ViewportDisplayImage>,
    xy_placeholder: String,
    xz_placeholder: String,
    yz_placeholder: String,
    test_render_viewport_max_side: Option<usize>,
    automation_render_target: Option<RenderExtent>,
}

impl ViewerUiSnapshot {
    fn display_for_panel(&self, panel_id: PanelId) -> Option<ViewportDisplayImage> {
        match panel_id {
            PanelId::Xy => self.xy_display.clone(),
            PanelId::Xz => self.xz_display.clone(),
            PanelId::Yz => self.yz_display.clone(),
            PanelId::ThreeD => Some(self.three_d_display.clone()),
        }
    }

    fn placeholder_for_panel(&self, panel_id: PanelId) -> &str {
        match panel_id {
            PanelId::Xy => &self.xy_placeholder,
            PanelId::Xz => &self.xz_placeholder,
            PanelId::Yz => &self.yz_placeholder,
            PanelId::ThreeD => "3D",
        }
    }

    fn render_viewport_max_side(&self, context_max: usize) -> usize {
        self.test_render_viewport_max_side
            .map_or(context_max, |test_max| context_max.min(test_max))
    }
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
            three_d_display: self.viewport_display_image(snapshot),
            xy_display: self.cross_section_panel_display_image(PanelId::Xy, snapshot),
            xz_display: self.cross_section_panel_display_image(PanelId::Xz, snapshot),
            yz_display: self.cross_section_panel_display_image(PanelId::Yz, snapshot),
            xy_placeholder: panel_placeholder(PanelId::Xy),
            xz_placeholder: panel_placeholder(PanelId::Xz),
            yz_placeholder: panel_placeholder(PanelId::Yz),
            test_render_viewport_max_side: self.validation_runtime.test_render_viewport_max_side,
            automation_render_target: self
                .validation_runtime
                .product_automation
                .as_ref()
                .and_then(ProductAutomationController::render_target_override),
        }
    }
}

fn show_viewer_layout(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    viewer: &ViewerUiSnapshot,
    egui_ui: &mut ui_kit::EguiUiState,
    output: &mut WorkbenchUiOutput,
) {
    match view.layout() {
        CanonicalViewerLayout::Single3d => {
            show_single_3d_viewport(ui, snapshot, view, viewer, egui_ui, output)
        }
        CanonicalViewerLayout::FourPanel => {
            show_four_panel_viewport(ui, snapshot, view, viewer, egui_ui, output)
        }
    }
}

fn observe_3d_viewport_for_display_size(
    ctx: &egui::Context,
    display_size_points: egui::Vec2,
    viewer: &ViewerUiSnapshot,
    output: &mut WorkbenchUiOutput,
) {
    let max_texture_side =
        viewer.render_viewport_max_side(ctx.input(|input| input.max_texture_side));
    let Some(presentation_viewport) = presentation_viewport_for_display_size(display_size_points)
    else {
        return;
    };
    let Some(render_viewport) = viewer.automation_render_target.or_else(|| {
        render_viewport_for_display_size(
            display_size_points,
            ctx.pixels_per_point(),
            max_texture_side,
        )
    }) else {
        return;
    };
    output.viewport_observations.push(ViewportObservation::new(
        PresentationSlot::ThreeD,
        presentation_viewport,
        render_viewport,
    ));
}

fn observe_four_panel_viewport(
    ctx: &egui::Context,
    panel_id: PanelId,
    display_size_points: egui::Vec2,
    viewer: &ViewerUiSnapshot,
    output: &mut WorkbenchUiOutput,
) -> Option<PresentationViewport> {
    let presentation_viewport = presentation_viewport_for_display_size(display_size_points)?;
    let max_texture_side =
        viewer.render_viewport_max_side(ctx.input(|input| input.max_texture_side));
    let render_viewport = render_viewport_for_display_size(
        display_size_points,
        ctx.pixels_per_point(),
        max_texture_side,
    )?;
    output.viewport_observations.push(ViewportObservation::new(
        panel_id.presentation_slot(),
        presentation_viewport,
        render_viewport,
    ));
    Some(presentation_viewport)
}

fn show_single_3d_viewport(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    viewer: &ViewerUiSnapshot,
    egui_ui: &mut ui_kit::EguiUiState,
    output: &mut WorkbenchUiOutput,
) {
    let available = ui.available_size();
    let ctx = ui.ctx().clone();
    observe_3d_viewport_for_display_size(&ctx, available, viewer, output);
    let display_image = viewer.three_d_display.clone();
    let image_size = fit_size(display_image.size_vec2(), available);
    ui.centered_and_justified(|ui| {
        show_3d_viewport_image(
            ui,
            display_image,
            image_size,
            snapshot,
            view,
            viewer,
            egui_ui,
            output,
        );
    });
}

fn show_four_panel_viewport(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    viewer: &ViewerUiSnapshot,
    egui_ui: &mut ui_kit::EguiUiState,
    output: &mut WorkbenchUiOutput,
) {
    let available = ui.available_size();
    let gap = 6.0;
    let cell_size = egui::vec2(
        ((available.x - gap) * 0.5).max(1.0),
        ((available.y - gap) * 0.5).max(1.0),
    );
    let panels = [PanelId::Xy, PanelId::Xz, PanelId::ThreeD, PanelId::Yz];

    egui_ui.hovered_pixel = None;
    egui_ui.hovered_source_readout = None;

    ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
    ui.vertical(|ui| {
        for row in panels.chunks_exact(2) {
            ui.horizontal(|ui| {
                for panel_id in row {
                    show_four_panel_cell(
                        ui, *panel_id, cell_size, snapshot, view, viewer, egui_ui, output,
                    );
                }
            });
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn show_four_panel_cell(
    ui: &mut egui::Ui,
    panel_id: PanelId,
    cell_size: egui::Vec2,
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    viewer: &ViewerUiSnapshot,
    egui_ui: &mut ui_kit::EguiUiState,
    output: &mut WorkbenchUiOutput,
) {
    let tokens = ui_kit::UiTokens::default();
    ui.allocate_ui_with_layout(cell_size, egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_min_size(cell_size);
        egui::Frame::new()
            .fill(tokens.colors.viewport_background)
            .stroke(egui::Stroke::new(1.0, tokens.colors.border))
            .corner_radius(egui::CornerRadius::same(3))
            .inner_margin(egui::Margin::same(6))
            .show(ui, |ui| {
                ui.set_min_size((cell_size - egui::vec2(12.0, 12.0)).max(egui::Vec2::ZERO));
                ui.label(egui::RichText::new(panel_id.label()).strong());
                ui.add_space(4.0);
                match panel_id {
                    PanelId::ThreeD => {
                        show_embedded_3d_panel(ui, snapshot, view, viewer, egui_ui, output)
                    }
                    PanelId::Xy | PanelId::Xz | PanelId::Yz => {
                        show_cross_section_panel(
                            ui, panel_id, snapshot, view, viewer, egui_ui, output,
                        );
                    }
                }
            });
    });
}

fn show_embedded_3d_panel(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    viewer: &ViewerUiSnapshot,
    egui_ui: &mut ui_kit::EguiUiState,
    output: &mut WorkbenchUiOutput,
) {
    let available = ui.available_size();
    let ctx = ui.ctx().clone();
    observe_3d_viewport_for_display_size(&ctx, available, viewer, output);
    let display_image = viewer.three_d_display.clone();
    let image_size = fit_size(display_image.size_vec2(), available);
    ui.centered_and_justified(|ui| {
        show_3d_viewport_image(
            ui,
            display_image,
            image_size,
            snapshot,
            view,
            viewer,
            egui_ui,
            output,
        );
    });
}

#[allow(clippy::too_many_arguments)]
fn show_3d_viewport_image(
    ui: &mut egui::Ui,
    display_image: ViewportDisplayImage,
    image_size: egui::Vec2,
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    viewer: &ViewerUiSnapshot,
    egui_ui: &mut ui_kit::EguiUiState,
    output: &mut WorkbenchUiOutput,
) {
    if image_size == egui::Vec2::ZERO {
        return;
    }
    let response = match display_image {
        ViewportDisplayImage::UiBackground { .. } => {
            let (rect, response) =
                ui.allocate_exact_size(image_size, egui::Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
            let label = if viewer.frame_fidelity.backend == RenderBackend::Empty {
                "No visible data"
            } else {
                "Loading…"
            };
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(18.0),
                ui.visuals().weak_text_color(),
            );
            response
        }
        ViewportDisplayImage::Presentation { slot, .. } => show_presentation(
            ui,
            snapshot,
            slot,
            image_size,
            egui::Sense::click_and_drag(),
            output,
        ),
    };
    let hover = viewport_hover_from_response(snapshot, view, &response);
    if response.hovered() || view.layout() == CanonicalViewerLayout::Single3d {
        egui_ui.hovered_pixel = hover;
        egui_ui.hovered_source_readout = None;
    }
    match apply_viewport_tool_response(
        snapshot,
        egui_ui,
        viewer.frame_fidelity.completeness,
        &response,
        hover,
    ) {
        Ok(outcome) => {
            output.texture_refresh_requested |= outcome.texture_refresh_requested;
            output.rerender_requested |= outcome.rerender_requested;
        }
        Err(err) => {
            tracing::warn!(%err, "viewer tool interaction rejected");
        }
    }
    if matches!(
        egui_ui.viewer_tools.active_tool,
        ViewerTool::Navigate | ViewerTool::Inspect
    ) {
        output
            .application_commands
            .extend(viewport_interaction_commands(
                egui_ui, view, &response, image_size,
            ));
    }
}

fn show_cross_section_panel(
    ui: &mut egui::Ui,
    panel_id: PanelId,
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    viewer: &ViewerUiSnapshot,
    egui_ui: &mut ui_kit::EguiUiState,
    output: &mut WorkbenchUiOutput,
) {
    let available = ui.available_size();
    let ctx = ui.ctx().clone();
    let presentation_viewport =
        observe_four_panel_viewport(&ctx, panel_id, available, viewer, output);
    let application_panel = application_cross_section_panel_id(panel_id)
        .expect("a cross-section widget has a cross-section panel ID");
    output
        .render_requests
        .push(RenderUiRequest::EnsureCrossSectionCurrent {
            panel: application_panel,
        });
    let response = if let Some(display_image) = viewer.display_for_panel(panel_id) {
        let image_size = fit_size(display_image.size_vec2(), available);
        if image_size != egui::Vec2::ZERO {
            Some(
                ui.centered_and_justified(|ui| {
                    show_cross_section_panel_image(
                        ui,
                        display_image,
                        image_size,
                        panel_id,
                        snapshot,
                        output,
                    )
                })
                .inner,
            )
        } else {
            None
        }
    } else {
        None
    }
    .unwrap_or_else(|| show_cross_section_panel_placeholder(ui, panel_id, available, viewer));

    if let Some(presentation_viewport) = presentation_viewport
        && let Some(request) = CrossSectionReadoutRequest::from_response(
            application_panel,
            presentation_viewport,
            &response,
        )
    {
        output.cross_section_readout_requests.push(request);
    }

    if matches!(
        egui_ui.viewer_tools.active_tool,
        ViewerTool::Navigate | ViewerTool::Inspect
    ) && let Some(presentation_viewport) = presentation_viewport
    {
        match cross_section_interaction_commands(
            snapshot,
            view,
            panel_id,
            presentation_viewport,
            &response,
        ) {
            Ok(commands) if !commands.is_empty() => {
                output.application_commands.extend(commands);
                output.request_repaint_after(CROSS_SECTION_INTERACTION_SETTLE_DURATION);
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "cross-section interaction rejected"),
        }
    }
}

fn show_cross_section_panel_image(
    ui: &mut egui::Ui,
    display_image: ViewportDisplayImage,
    image_size: egui::Vec2,
    panel_id: PanelId,
    snapshot: &ApplicationSnapshot,
    output: &mut WorkbenchUiOutput,
) -> egui::Response {
    let response = match display_image {
        ViewportDisplayImage::UiBackground { .. } => {
            let (rect, response) =
                ui.allocate_exact_size(image_size, egui::Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
            response
        }
        ViewportDisplayImage::Presentation { slot, .. } => show_presentation(
            ui,
            snapshot,
            slot,
            image_size,
            egui::Sense::click_and_drag(),
            output,
        ),
    };
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Other,
            ui.is_enabled(),
            format!("{} cross-section panel", panel_id.label()),
        )
    });
    response
}

fn show_presentation(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    slot: PresentationSlot,
    image_size: egui::Vec2,
    sense: egui::Sense,
    output: &mut WorkbenchUiOutput,
) -> egui::Response {
    let surface = snapshot
        .presentations()
        .get(slot)
        .expect("a displayed presentation belongs to a projected surface");
    let (response, paint) = ui_kit::reserve_presentation(ui, slot, surface, image_size, sense);
    ui.painter()
        .rect_filled(response.rect, 0.0, ui.visuals().extreme_bg_color);
    if let Some(paint) = paint {
        output.presentation_paints.push(paint);
    }
    response
}

fn show_cross_section_panel_placeholder(
    ui: &mut egui::Ui,
    panel_id: PanelId,
    available: egui::Vec2,
    viewer: &ViewerUiSnapshot,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(available, egui::Sense::click_and_drag());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Other,
            ui.is_enabled(),
            format!("{} cross-section panel", panel_id.label()),
        )
    });
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        viewer.placeholder_for_panel(panel_id),
        egui::FontId::proportional(18.0),
        ui.visuals().weak_text_color(),
    );
    response
}

fn cross_section_interaction_commands(
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    panel_id: PanelId,
    presentation_viewport: PresentationViewport,
    response: &egui::Response,
) -> Result<Vec<ApplicationCommand>, String> {
    let panel = panel_id
        .cross_section_panel()
        .ok_or_else(|| "3D is not a cross-section interaction target".to_owned())?;
    let application_panel = application_cross_section_panel_id(panel_id)
        .ok_or_else(|| "3D is not a cross-section interaction target".to_owned())?;
    let mut cross_section =
        mirante4d_application::viewport_interaction::CrossSectionViewState::from_canonical(
            *view.cross_section(),
        );
    let mut edited = false;
    let modifiers = response.ctx.input(|input| input.modifiers);
    if response.dragged() {
        let primary_down = response.ctx.input(|input| input.pointer.primary_down());
        let motion_points = response.drag_motion();
        if primary_down
            && motion_points.x.is_finite()
            && motion_points.y.is_finite()
            && motion_points != egui::Vec2::ZERO
        {
            if modifiers.shift {
                cross_section.rotate_oblique_by_panel_drag(
                    panel,
                    f64::from(motion_points.x),
                    f64::from(motion_points.y),
                    CROSS_SECTION_ROTATE_RADIANS_PER_POINT,
                );
            } else {
                cross_section.pan_by_panel_points(
                    panel,
                    f64::from(motion_points.x),
                    f64::from(motion_points.y),
                );
            }
            edited = true;
        }
    }
    if response.hovered() {
        if modifiers.ctrl || modifiers.command {
            let zoom_delta = response.ctx.input(|input| input.zoom_delta());
            if let Some(scroll_y) = scroll_y_points_from_zoom_delta(zoom_delta)
                && let Some(pointer) = response.hover_pos()
            {
                let local = pointer - response.rect.min.to_vec2();
                let factor =
                    (-f64::from(scroll_y) * CROSS_SECTION_SCROLL_ZOOM_FACTOR_SCALE as f64).exp();
                cross_section.zoom_around_panel_point(
                    panel,
                    presentation_viewport,
                    f64::from(local.x),
                    f64::from(local.y),
                    factor,
                );
                edited = true;
            }
        } else {
            let scroll_y = response.ctx.input(|input| input.smooth_scroll_delta().y);
            if scroll_y.is_finite() && scroll_y != 0.0 {
                let layer = snapshot
                    .catalog()
                    .layer(view.active_layer())
                    .ok_or_else(|| "active layer is absent from the dataset catalog".to_owned())?;
                let voxel_size =
                    mirante4d_application::viewport_interaction::representative_voxel_world_size(
                        layer.grid_to_world(),
                    );
                let multiplier = if modifiers.shift {
                    CROSS_SECTION_FAST_SLICE_MULTIPLIER
                } else {
                    1.0
                };
                let notches = f64::from(scroll_y / CROSS_SECTION_SCROLL_POINTS_PER_NOTCH);
                cross_section.slice_by_world_distance(panel, notches * voxel_size * multiplier);
                edited = true;
            }
        }
    }
    if !edited {
        return Ok(Vec::new());
    }
    let cross_section = cross_section
        .into_canonical()
        .map_err(|error| error.to_string())?;
    Ok(vec![
        ApplicationCommand::SetActiveCrossSectionPanel(Some(application_panel)),
        ApplicationCommand::SetLayout {
            layout: view.layout(),
            cross_section,
        },
    ])
}

fn application_cross_section_panel_id(panel_id: PanelId) -> Option<CrossSectionPanelId> {
    match panel_id {
        PanelId::Xy => Some(CrossSectionPanelId::Xy),
        PanelId::Xz => Some(CrossSectionPanelId::Xz),
        PanelId::Yz => Some(CrossSectionPanelId::Yz),
        PanelId::ThreeD => None,
    }
}

fn scroll_y_points_from_zoom_delta(zoom_delta: f32) -> Option<f32> {
    if !zoom_delta.is_finite() || zoom_delta <= 0.0 || zoom_delta == 1.0 {
        return None;
    }
    let scroll_y = -zoom_delta.ln() / CROSS_SECTION_SCROLL_ZOOM_FACTOR_SCALE;
    scroll_y.is_finite().then_some(scroll_y)
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

pub(crate) fn viewer_tool_for_kind(tool: ToolKind) -> ViewerTool {
    match tool {
        ToolKind::Navigate => ViewerTool::Navigate,
        ToolKind::Inspect => ViewerTool::Inspect,
        ToolKind::Crosshair => ViewerTool::Crosshair,
        ToolKind::RoiBox => ViewerTool::RoiBox,
        ToolKind::MeasureDistance => ViewerTool::MeasureDistance,
    }
}

impl eframe::App for MiranteWorkbenchApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let update_started = Instant::now();
        let setup_started = Instant::now();
        self.pump_application_services();
        self.handle_close_request(ui.ctx());
        let setup_ms = duration_ms(setup_started.elapsed());

        let task_drain_started = Instant::now();
        self.drain_tiff_import_setup_results(ui.ctx());
        self.drain_import_results(ui.ctx());
        let task_drain_ms = duration_ms(task_drain_started.elapsed());

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
        let canonical_tool = viewer_tool_for_kind(application_snapshot.transient().active_tool());
        if self.egui_ui.viewer_tools.active_tool != canonical_tool {
            self.egui_ui.viewer_tools.set_active_tool(canonical_tool);
        }

        let mut rerender_requested = false;
        let mut texture_refresh_requested = false;
        let mut repaint_after: Option<Duration> = None;
        let mut application_commands = Vec::new();
        let mut actions = Vec::new();
        let mut viewport_observations = Vec::new();
        let mut cross_section_readout_requests = Vec::new();
        let mut render_requests = Vec::new();
        let mut presentation_paints = Vec::new();
        let mut import_commands = Vec::new();
        let layout = WorkbenchLayoutSpec::default();
        let playback_started = Instant::now();
        workbench_playback_runtime::enqueue_playback_command_if_due(
            &application_snapshot,
            &self.dataset,
            &mut application_commands,
            ui.ctx(),
        );
        let playback_ms = duration_ms(playback_started.elapsed());

        let ui_build_started = Instant::now();
        let histogram_started = Instant::now();
        let active_layer_histogram_for_ui = self.active_histogram_summary(&application_snapshot);
        let histogram_ui_ms = duration_ms(histogram_started.elapsed());
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
        let mut chrome_output = WorkbenchUiOutput::default();
        ui_kit::show_top_toolbar(
            ui,
            ui_kit::TopToolbarView {
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
            &mut chrome_output,
        );
        ui_kit::show_left_workbench_panel(
            ui,
            ui_kit::LeftWorkbenchView {
                application: &application_snapshot,
                source_verification_available,
                composite_fidelity: &viewer_ui_snapshot.composite_fidelity,
                dataset_path: &viewer_ui_snapshot.dataset_path,
            },
            &self.egui_ui,
            layout,
            &mut chrome_output,
        );
        application_commands.append(&mut chrome_output.application_commands);
        import_commands.append(&mut chrome_output.import_commands);
        actions.append(&mut chrome_output.actions);
        viewport_observations.append(&mut chrome_output.viewport_observations);
        cross_section_readout_requests.append(&mut chrome_output.cross_section_readout_requests);
        render_requests.append(&mut chrome_output.render_requests);
        presentation_paints.append(&mut chrome_output.presentation_paints);
        rerender_requested |= chrome_output.rerender_requested;
        texture_refresh_requested |= chrome_output.texture_refresh_requested;
        if let Some(delay) = chrome_output.repaint_after {
            repaint_after = Some(repaint_after.map_or(delay, |current| current.min(delay)));
        }

        let no_data_policy_label = active_layer_no_data_policy_label(&application_snapshot);
        let camera_frame =
            CameraFrame::new(*view.camera(), viewer_ui_snapshot.presentation_viewport).ok();
        let camera_inspector_view = ui_kit::CameraInspectorView {
            forward: camera_frame.as_ref().map(|frame| frame.axes().forward()),
            world_per_screen_point: camera_frame
                .as_ref()
                .and_then(|frame| frame.world_per_screen_point_at_target().ok()),
        };
        let mut inspector_output = WorkbenchUiOutput::default();
        ui_kit::show_workbench_inspector(
            ui,
            ui_kit::InspectorWorkbenchView {
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
            &mut self.egui_ui,
            layout,
            &mut inspector_output,
        );
        application_commands.append(&mut inspector_output.application_commands);
        import_commands.append(&mut inspector_output.import_commands);
        actions.append(&mut inspector_output.actions);
        viewport_observations.append(&mut inspector_output.viewport_observations);
        cross_section_readout_requests.append(&mut inspector_output.cross_section_readout_requests);
        render_requests.append(&mut inspector_output.render_requests);
        presentation_paints.append(&mut inspector_output.presentation_paints);
        rerender_requested |= inspector_output.rerender_requested;
        texture_refresh_requested |= inspector_output.texture_refresh_requested;
        if let Some(delay) = inspector_output.repaint_after {
            repaint_after = Some(repaint_after.map_or(delay, |current| current.min(delay)));
        }

        let mut viewer_output = WorkbenchUiOutput::default();
        egui::CentralPanel::default().show_inside(ui, |ui| {
            show_viewer_layout(
                ui,
                &application_snapshot,
                view,
                &viewer_ui_snapshot,
                &mut self.egui_ui,
                &mut viewer_output,
            );
        });
        application_commands.append(&mut viewer_output.application_commands);
        import_commands.append(&mut viewer_output.import_commands);
        actions.append(&mut viewer_output.actions);
        viewport_observations.append(&mut viewer_output.viewport_observations);
        cross_section_readout_requests.append(&mut viewer_output.cross_section_readout_requests);
        render_requests.append(&mut viewer_output.render_requests);
        presentation_paints.append(&mut viewer_output.presentation_paints);
        rerender_requested |= viewer_output.rerender_requested;
        texture_refresh_requested |= viewer_output.texture_refresh_requested;
        if let Some(delay) = viewer_output.repaint_after {
            if let Some(current) = repaint_after {
                repaint_after = Some(current.min(delay));
            } else {
                repaint_after = Some(delay);
            }
        }

        let commands =
            show_analysis_workspace_window(ui.ctx(), &analysis_workspace_view, &mut self.egui_ui);
        application_commands.extend(commands);

        import_commands.extend(ui_kit::show_import_workflow_window(
            ui.ctx(),
            &mut self.egui_ui,
            application_snapshot.import_workflow(),
        ));
        show_project_recovery_ui(ui.ctx(), &project_recovery_ui, &mut actions);
        show_dirty_project_close_prompt(ui.ctx(), dirty_project_close_ui, &mut actions);
        let ui_build_ms = duration_ms(ui_build_started.elapsed());
        let apply_timing = self.apply_workbench_ui_output(
            ui,
            WorkbenchUiOutput {
                application_commands,
                import_commands,
                actions,
                viewport_observations,
                cross_section_readout_requests,
                render_requests,
                presentation_paints,
                rerender_requested,
                texture_refresh_requested,
                repaint_after,
            },
        );

        let brick_result_drain_started = Instant::now();
        self.drain_brick_results(ui.ctx());
        let brick_result_drain_ms = duration_ms(brick_result_drain_started.elapsed());

        let background_repaint_started = Instant::now();
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
        let background_repaint_request_ms = duration_ms(background_repaint_started.elapsed());

        ProductAutomationController::drive(
            self,
            ui.ctx(),
            ProductAutomationAppUpdateTiming {
                update_started,
                setup_ms,
                task_drain_ms,
                playback_ms,
                ui_build_ms,
                histogram_ui_ms,
                command_apply_ms: apply_timing.command_apply_ms,
                display_refresh_trigger_ms: apply_timing.display_refresh_trigger_ms,
                import_action_ms: apply_timing.import_action_ms,
                brick_result_drain_ms,
                background_repaint_request_ms,
            },
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_section_zoom_delta_converts_to_existing_scroll_units() {
        assert_eq!(scroll_y_points_from_zoom_delta(1.0), None);
        assert_eq!(scroll_y_points_from_zoom_delta(0.0), None);
        let zoom_in_scroll = scroll_y_points_from_zoom_delta(1.25).unwrap();
        let zoom_out_scroll = scroll_y_points_from_zoom_delta(0.8).unwrap();
        assert!(zoom_in_scroll < 0.0);
        assert!(zoom_out_scroll > 0.0);
        let reconstructed = (-zoom_in_scroll * CROSS_SECTION_SCROLL_ZOOM_FACTOR_SCALE).exp();
        assert!((reconstructed - 1.25).abs() < 1e-6);
    }
}
