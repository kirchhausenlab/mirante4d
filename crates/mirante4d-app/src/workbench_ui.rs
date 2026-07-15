use super::*;
use mirante4d_application::CrossSectionPanelId;
use mirante4d_domain::{Opacity, ToolKind};
use mirante4d_render_api::{CameraFrame, RenderExtent};

use crate::viewer_layout::{PanelId, cross_section_schedule_status_label};

fn show_iso_light_controls(
    ui: &mut egui::Ui,
    light_state: IsoLightState,
    application_commands: &mut Vec<ApplicationCommand>,
) {
    let attached = light_state.is_attached_camera();
    ui.horizontal(|ui| {
        let mut attached_toggle = attached;
        if ui
            .checkbox(&mut attached_toggle, "Attached light")
            .changed()
        {
            let next = if attached_toggle {
                IsoLightState::attached_camera()
            } else {
                let [x, y] = light_state.detached_screen_position().unwrap_or([0.0, 0.0]);
                IsoLightState::detached_screen(x, y)
                    .expect("the retained detached ISO light position is valid")
            };
            application_commands.push(ApplicationCommand::SetIsoLight(next));
        }
        if ui.button("Reset").clicked() {
            application_commands.push(ApplicationCommand::SetIsoLight(
                IsoLightState::attached_camera(),
            ));
        }
    });

    let side = 72.0;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click_and_drag());
    let response = response.on_hover_text("ISO light direction");
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Other,
            ui.is_enabled(),
            "ISO light direction",
        )
    });
    let center = rect.center();
    let radius = rect.width().min(rect.height()) * 0.5 - 4.0;
    let visuals = ui.visuals();
    let stroke = if attached {
        visuals.widgets.noninteractive.fg_stroke
    } else {
        visuals.widgets.inactive.fg_stroke
    };
    let painter = ui.painter_at(rect);
    painter.circle_stroke(center, radius, stroke);
    painter.line_segment(
        [
            egui::pos2(center.x - radius, center.y),
            egui::pos2(center.x + radius, center.y),
        ],
        egui::Stroke::new(1.0, stroke.color.linear_multiply(0.4)),
    );
    painter.line_segment(
        [
            egui::pos2(center.x, center.y - radius),
            egui::pos2(center.x, center.y + radius),
        ],
        egui::Stroke::new(1.0, stroke.color.linear_multiply(0.4)),
    );
    let position = light_state.detached_screen_position().unwrap_or([0.0, 0.0]);
    let marker = egui::pos2(
        center.x + position[0] * radius,
        center.y - position[1] * radius,
    );
    painter.circle_filled(
        marker,
        4.0,
        if attached {
            stroke.color.linear_multiply(0.45)
        } else {
            visuals.selection.bg_fill
        },
    );

    if !attached
        && (response.dragged() || response.clicked())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let mut x = ((pointer.x - center.x) / radius).clamp(-1.0, 1.0);
        let mut y = (-(pointer.y - center.y) / radius).clamp(-1.0, 1.0);
        let length = x.hypot(y);
        if length > 1.0 {
            x /= length;
            y /= length;
        }
        if let Ok(next) = IsoLightState::detached_screen(x, y) {
            application_commands.push(ApplicationCommand::SetIsoLight(next));
        }
    }
}

fn layout_selector(
    ui: &mut egui::Ui,
    current: CanonicalViewerLayout,
    cross_section: CrossSectionView,
    application_commands: &mut Vec<ApplicationCommand>,
) {
    ui_kit::muted_label(ui, "Layout");
    if ui
        .selectable_label(current == CanonicalViewerLayout::Single3d, "3D")
        .clicked()
    {
        application_commands.push(ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::Single3d,
            cross_section,
        });
    }
    if ui
        .selectable_label(current == CanonicalViewerLayout::FourPanel, "4 Panel")
        .clicked()
    {
        application_commands.push(ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section,
        });
    }
}

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

    fn show_viewer_layout(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &ApplicationSnapshot,
        view: &ViewState,
        viewer: &ViewerUiSnapshot,
        output: &mut WorkbenchUiOutput,
    ) {
        match view.layout() {
            CanonicalViewerLayout::Single3d => {
                self.show_single_3d_viewport(ui, snapshot, view, viewer, output)
            }
            CanonicalViewerLayout::FourPanel => {
                self.show_four_panel_viewport(ui, snapshot, view, viewer, output)
            }
        }
    }

    fn observe_3d_viewport_for_display_size(
        &self,
        ctx: &egui::Context,
        display_size_points: egui::Vec2,
        viewer: &ViewerUiSnapshot,
        output: &mut WorkbenchUiOutput,
    ) {
        let max_texture_side =
            viewer.render_viewport_max_side(ctx.input(|input| input.max_texture_side));
        let Some(presentation_viewport) =
            presentation_viewport_for_display_size(display_size_points)
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
        &self,
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
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &ApplicationSnapshot,
        view: &ViewState,
        viewer: &ViewerUiSnapshot,
        output: &mut WorkbenchUiOutput,
    ) {
        let available = ui.available_size();
        let ctx = ui.ctx().clone();
        self.observe_3d_viewport_for_display_size(&ctx, available, viewer, output);
        let display_image = viewer.three_d_display.clone();
        let image_size = fit_size(display_image.size_vec2(), available);
        ui.centered_and_justified(|ui| {
            self.show_3d_viewport_image(
                ui,
                display_image,
                image_size,
                snapshot,
                view,
                viewer,
                output,
            );
        });
    }

    fn show_four_panel_viewport(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &ApplicationSnapshot,
        view: &ViewState,
        viewer: &ViewerUiSnapshot,
        output: &mut WorkbenchUiOutput,
    ) {
        let available = ui.available_size();
        let gap = 6.0;
        let cell_size = egui::vec2(
            ((available.x - gap) * 0.5).max(1.0),
            ((available.y - gap) * 0.5).max(1.0),
        );
        let panels = [PanelId::Xy, PanelId::Xz, PanelId::ThreeD, PanelId::Yz];

        self.egui_ui.hovered_pixel = None;
        self.egui_ui.hovered_source_readout = None;

        ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
        ui.vertical(|ui| {
            for row in panels.chunks_exact(2) {
                ui.horizontal(|ui| {
                    for panel_id in row {
                        self.show_four_panel_cell(
                            ui, *panel_id, cell_size, snapshot, view, viewer, output,
                        );
                    }
                });
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn show_four_panel_cell(
        &mut self,
        ui: &mut egui::Ui,
        panel_id: PanelId,
        cell_size: egui::Vec2,
        snapshot: &ApplicationSnapshot,
        view: &ViewState,
        viewer: &ViewerUiSnapshot,
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
                            self.show_embedded_3d_panel(ui, snapshot, view, viewer, output)
                        }
                        PanelId::Xy | PanelId::Xz | PanelId::Yz => {
                            self.show_cross_section_panel(
                                ui, panel_id, snapshot, view, viewer, output,
                            );
                        }
                    }
                });
        });
    }

    fn show_embedded_3d_panel(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &ApplicationSnapshot,
        view: &ViewState,
        viewer: &ViewerUiSnapshot,
        output: &mut WorkbenchUiOutput,
    ) {
        let available = ui.available_size();
        let ctx = ui.ctx().clone();
        self.observe_3d_viewport_for_display_size(&ctx, available, viewer, output);
        let display_image = viewer.three_d_display.clone();
        let image_size = fit_size(display_image.size_vec2(), available);
        ui.centered_and_justified(|ui| {
            self.show_3d_viewport_image(
                ui,
                display_image,
                image_size,
                snapshot,
                view,
                viewer,
                output,
            );
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn show_3d_viewport_image(
        &mut self,
        ui: &mut egui::Ui,
        display_image: ViewportDisplayImage,
        image_size: egui::Vec2,
        snapshot: &ApplicationSnapshot,
        view: &ViewState,
        viewer: &ViewerUiSnapshot,
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
            ViewportDisplayImage::Presentation { slot, .. } => self.show_presentation(
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
            self.egui_ui.hovered_pixel = hover;
            self.egui_ui.hovered_source_readout = None;
        }
        match apply_viewport_tool_response(
            snapshot,
            &mut self.egui_ui,
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
            self.egui_ui.viewer_tools.active_tool,
            ViewerTool::Navigate | ViewerTool::Inspect
        ) {
            output
                .application_commands
                .extend(viewport_interaction_commands(
                    &mut self.egui_ui,
                    view,
                    &response,
                    image_size,
                ));
        }
    }

    fn show_cross_section_panel(
        &mut self,
        ui: &mut egui::Ui,
        panel_id: PanelId,
        snapshot: &ApplicationSnapshot,
        view: &ViewState,
        viewer: &ViewerUiSnapshot,
        output: &mut WorkbenchUiOutput,
    ) {
        let available = ui.available_size();
        let ctx = ui.ctx().clone();
        let presentation_viewport =
            self.observe_four_panel_viewport(&ctx, panel_id, available, viewer, output);
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
                        self.show_cross_section_panel_image(
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
        .unwrap_or_else(|| {
            self.show_cross_section_panel_placeholder(ui, panel_id, available, viewer)
        });

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
            self.egui_ui.viewer_tools.active_tool,
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
        &mut self,
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
            ViewportDisplayImage::Presentation { slot, .. } => self.show_presentation(
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
        &mut self,
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
        &mut self,
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
    let mut cross_section = viewer_layout::render_cross_section_view_state(*view.cross_section());
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
                    lod_scheduler::representative_voxel_world_size(layer.grid_to_world());
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
    let cross_section = cross_section.into_canonical()?;
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

fn workspace_channel_presets(snapshot: &ApplicationSnapshot) -> &[ChannelPreset] {
    match snapshot.workspace() {
        WorkspaceSnapshot::Unbound { workspace } => workspace.channel_presets(),
        WorkspaceSnapshot::Bound { project, .. } => project.channel_presets(),
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

fn layer_view_command(
    layer: &LayerViewState,
    visible: bool,
    transfer: LayerTransfer,
    render_state: CanonicalRenderState,
) -> ApplicationCommand {
    ApplicationCommand::SetLayerView(LayerViewState::new(
        layer.layer_key(),
        visible,
        transfer,
        render_state,
    ))
}

fn layer_render_state_for_mode(
    layer: &LayerViewState,
    mode: mirante4d_domain::RenderMode,
) -> Result<CanonicalRenderState, String> {
    let current = *layer.render_state();
    let sampling = SamplingPolicy::VoxelExact;
    match mode {
        mirante4d_domain::RenderMode::Mip => Ok(CanonicalRenderState::mip(sampling)),
        mirante4d_domain::RenderMode::Isosurface => {
            let display_level = current
                .iso_parameters()
                .map(|parameters| parameters.display_level())
                .unwrap_or(DEFAULT_ISO_DISPLAY_LEVEL);
            CanonicalRenderState::iso(sampling, IsoShadingPolicy::Flat, display_level)
                .map_err(|error| error.to_string())
        }
        mirante4d_domain::RenderMode::Dvr => {
            let (opacity_transfer, density_scale) = current
                .dvr_parameters()
                .map(|parameters| (parameters.opacity_transfer(), parameters.density_scale()))
                .unwrap_or((
                    CanonicalDvrOpacityTransfer::new(
                        layer.transfer().window(),
                        layer.transfer().curve(),
                    ),
                    DEFAULT_DVR_DENSITY_SCALE,
                ));
            CanonicalRenderState::dvr(sampling, opacity_transfer, density_scale)
                .map_err(|error| error.to_string())
        }
    }
}

fn render_state_with_sampling(
    current: CanonicalRenderState,
    sampling: SamplingPolicy,
) -> Result<CanonicalRenderState, String> {
    match current.mode() {
        mirante4d_domain::RenderMode::Mip => Ok(CanonicalRenderState::mip(sampling)),
        mirante4d_domain::RenderMode::Isosurface => {
            let parameters = current
                .iso_parameters()
                .expect("ISO mode has ISO parameters");
            CanonicalRenderState::iso(
                sampling,
                parameters.shading_policy(),
                parameters.display_level(),
            )
            .map_err(|error| error.to_string())
        }
        mirante4d_domain::RenderMode::Dvr => {
            let parameters = current
                .dvr_parameters()
                .expect("DVR mode has DVR parameters");
            CanonicalRenderState::dvr(
                sampling,
                parameters.opacity_transfer(),
                parameters.density_scale(),
            )
            .map_err(|error| error.to_string())
        }
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

fn reset_view_command(
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    presentation_viewport: PresentationViewport,
) -> Result<ApplicationCommand, String> {
    let layer = snapshot
        .catalog()
        .layer(view.active_layer())
        .ok_or_else(|| "active layer is absent from the dataset catalog".to_owned())?;
    let default_camera = default_camera_for_shape(layer.shape().spatial(), layer.grid_to_world());
    let camera = CameraView::new(
        view.camera().projection(),
        default_camera.target(),
        default_camera.orientation(),
        default_camera.orthographic_world_per_screen_point(),
        default_camera.perspective_focal_length_screen_points(),
        default_camera.perspective_view_distance_world(),
    )
    .map_err(|error| error.to_string())?;
    let camera = fit_camera_to_shape_preserving_view(
        camera,
        layer.shape().spatial(),
        layer.grid_to_world(),
        presentation_viewport,
    );
    let cross_section = CrossSectionView::new(
        camera.target(),
        UnitQuaternion::identity(),
        camera.orthographic_world_per_screen_point(),
        lod_scheduler::representative_voxel_world_size(layer.grid_to_world()),
    )
    .map_err(|error| error.to_string())?;
    let next = ViewState::new(
        view.layers().to_vec(),
        view.active_layer(),
        view.timepoint(),
        camera,
        view.layout(),
        cross_section,
        *view.iso_light(),
    )
    .map_err(|error| error.to_string())?;
    Ok(ApplicationCommand::ReplaceView(next))
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
        let analysis_workspace_view = AnalysisWorkspaceView::new(
            &self.analysis_runtime,
            AnalysisWorkspaceViewInput {
                table_descriptors: transient.analysis_tables(),
                plot_descriptors: transient.analysis_plots(),
                selected_table: transient.selected_analysis_table(),
                selected_plot: transient.selected_analysis_plot(),
                selected_plot_point: transient.selected_analysis_plot_point(),
            },
        );
        let dirty_project_close_ui = self.dirty_project_close_ui();
        let settings_ui_view = self.settings_ui_view();
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
        let import_snapshot = application_snapshot.import_workflow();
        let import_active = matches!(import_snapshot, ImportWorkflowSnapshot::Importing(_));
        let dataset_open_active = application_snapshot
            .active_operations()
            .iter()
            .any(|token| token.kind() == OperationKind::DatasetOpen);
        let workflow_busy =
            !matches!(import_snapshot, ImportWorkflowSnapshot::Idle) || dataset_open_active;
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
            || !project_recovery_ui.candidates.is_empty()
            || !project_recovery_ui.locators.is_empty();
        let active_layer = view
            .layer(view.active_layer())
            .expect("application view contains its active layer");
        let active_catalog_layer = application_snapshot
            .catalog()
            .layer(view.active_layer())
            .expect("application view closes over the dataset catalog");
        let timepoint_count =
            workbench_playback_runtime::catalog_timepoint_count(&application_snapshot);

        egui::Panel::top("top-toolbar").show_inside(ui, |ui| {
            ui.vertical(|ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.heading("Mirante4D");
                    ui.separator();
                    if ui_kit::toolbar_button(
                        ui,
                        "Open",
                        !workflow_busy
                            && project_store_idle
                            && self.pending_dataset_open_path.is_none(),
                    )
                    .clicked()
                    {
                        actions.push(WorkbenchUiAction::OpenDatasetDialog);
                    }
                    if ui_kit::toolbar_button(ui, "New Project", !workflow_busy && can_new_project)
                        .on_hover_text("Start an unsaved project for the verified dataset")
                        .clicked()
                    {
                        actions.push(WorkbenchUiAction::NewProject);
                    }
                    if ui_kit::toolbar_button(
                        ui,
                        "Open Project",
                        !workflow_busy && can_open_project,
                    )
                    .on_hover_text("Requires a verified scientific dataset identity")
                    .clicked()
                    {
                        actions.push(WorkbenchUiAction::OpenProjectDialog);
                    }
                    if ui_kit::toolbar_button(
                        ui,
                        "Save Project",
                        !workflow_busy && can_save_project,
                    )
                    .on_hover_text("Requires a verified scientific dataset identity")
                    .clicked()
                    {
                        actions.push(WorkbenchUiAction::SaveProject);
                    }
                    if ui_kit::toolbar_button(
                        ui,
                        "Save Project As",
                        !workflow_busy && can_save_project_as,
                    )
                    .on_hover_text("Save a new project identity with exact fork provenance")
                    .clicked()
                    {
                        actions.push(WorkbenchUiAction::SaveProjectAs);
                    }
                    if ui_kit::toolbar_button(
                        ui,
                        "Recovery",
                        !workflow_busy && project_recovery_available,
                    )
                    .on_hover_text("List validated autosave and manual recovery branches")
                    .clicked()
                    {
                        actions.push(WorkbenchUiAction::OpenProjectRecovery);
                    }
                    if ui_kit::toolbar_button(ui, "Import Dir", !workflow_busy)
                        .on_hover_text("Import TIFF directory")
                        .clicked()
                    {
                        actions.push(WorkbenchUiAction::ImportTiffDirectoryDialog);
                    }
                    if ui_kit::toolbar_button(ui, "Import File", !workflow_busy)
                        .on_hover_text("Import TIFF file")
                        .clicked()
                    {
                        actions.push(WorkbenchUiAction::ImportTiffFileDialog);
                    }
                    if import_active {
                        let cancellation_pending = matches!(
                            import_snapshot,
                            ImportWorkflowSnapshot::Importing(execution)
                                if execution.cancellation_requested
                        );
                        if ui_kit::toolbar_button(
                            ui,
                            if cancellation_pending {
                                "Stopping Import"
                            } else {
                                "Cancel Import"
                            },
                            !cancellation_pending,
                        )
                        .clicked()
                        {
                            import_commands.push(ImportCommand::CancelImport);
                        }
                    }
                    ui.separator();
                    ui_kit::elided_label(ui, application_snapshot.catalog().label(), 42);
                });
                if let Some(message) = self.project_status_message.as_deref() {
                    ui_kit::muted_label(ui, message);
                }
                ui.horizontal_wrapped(|ui| {
                    layout_selector(
                        ui,
                        view.layout(),
                        *view.cross_section(),
                        &mut application_commands,
                    );
                    ui.separator();
                    ui_kit::muted_label(ui, "Render");
                    if let Some(command) = render_mode_selector(ui, view) {
                        application_commands.push(command);
                    }
                    ui.separator();
                    ui_kit::muted_label(ui, "Camera");
                    if let Some(command) = projection_selector(ui, *view.camera()) {
                        application_commands.push(command);
                    }
                    if ui.button("Fit Data").clicked() {
                        application_commands.push(ApplicationCommand::SetCamera(
                            fit_camera_to_shape_preserving_view(
                                *view.camera(),
                                active_catalog_layer.shape().spatial(),
                                active_catalog_layer.grid_to_world(),
                                viewer_ui_snapshot.presentation_viewport,
                            ),
                        ));
                    }
                    if ui.button("Reset View").clicked() {
                        match reset_view_command(
                            &application_snapshot,
                            view,
                            viewer_ui_snapshot.presentation_viewport,
                        ) {
                            Ok(command) => application_commands.push(command),
                            Err(error) => tracing::warn!(%error, "view reset rejected"),
                        }
                    }
                });
            });
        });

        egui::Panel::left("layers-panel")
            .resizable(true)
            .default_size(layout.left_panel_width)
            .size_range(layout.left_width_range())
            .show_inside(ui, |ui| {
                ui_kit::panel_scroll(ui, "layers-panel-scroll", |ui| {
                    ui_kit::section(ui, "Dataset", |ui| {
                        ui_kit::property_row(ui, "name", application_snapshot.catalog().label());
                        ui_kit::property_row(
                            ui,
                            "layers",
                            application_snapshot.catalog().len().to_string(),
                        );
                        ui_kit::property_row(
                            ui,
                            "timepoints",
                            timepoint_count.to_string(),
                        );
                        ui_kit::property_row(
                            ui,
                            "scientific identity",
                            match application_snapshot.source() {
                                SourceVerificationSnapshot::Required => {
                                    "verification required; project open/save unavailable"
                                        .to_owned()
                                }
                                SourceVerificationSnapshot::Verifying {
                                    completed_work,
                                    total_work,
                                    ..
                                } => (*completed_work)
                                    .saturating_mul(100)
                                    .checked_div(*total_work)
                                    .map_or_else(
                                        || "verifying".to_owned(),
                                        |percent| format!("verifying ({percent}%)"),
                                    ),
                                SourceVerificationSnapshot::Verified(_) => "verified".to_owned(),
                            },
                        );
                        if let SourceVerificationSnapshot::Verifying { operation_id, .. } =
                            application_snapshot.source()
                            && ui_kit::toolbar_button(ui, "Cancel Verification", true).clicked()
                        {
                            application_commands
                                .push(ApplicationCommand::CancelOperation(*operation_id));
                        }
                        if matches!(
                            application_snapshot.source(),
                            SourceVerificationSnapshot::Required
                        ) && ui_kit::toolbar_button(
                            ui,
                            "Verify Source",
                            source_verification_available,
                        )
                        .clicked()
                        {
                            application_commands
                                .push(ApplicationCommand::RequestSourceVerification);
                        }
                    });
                    ui_kit::section(ui, "Status", |ui| {
                        match import_snapshot {
                            ImportWorkflowSnapshot::Importing(execution) => {
                                ui_kit::status_badge(
                                    ui,
                                    StatusTone::Warning,
                                    if execution.cancellation_requested {
                                        "stopping TIFF import"
                                    } else {
                                        "importing TIFF"
                                    },
                                );
                            }
                            ImportWorkflowSnapshot::Inspecting(_) => {
                                ui_kit::status_badge(
                                    ui,
                                    StatusTone::Warning,
                                    "inspecting TIFF input",
                                );
                            }
                            ImportWorkflowSnapshot::Review(_) => {
                                ui_kit::status_badge(
                                    ui,
                                    StatusTone::Warning,
                                    "review TIFF import settings",
                                );
                            }
                            ImportWorkflowSnapshot::Failed(_) => {
                                ui_kit::status_badge(
                                    ui,
                                    StatusTone::Error,
                                    "TIFF import needs attention",
                                );
                            }
                            ImportWorkflowSnapshot::Idle => {
                                ui_kit::status_badge(ui, StatusTone::Ready, "ready");
                            }
                        }
                        ui_kit::property_row(
                            ui,
                            "fidelity",
                            &viewer_ui_snapshot.composite_fidelity,
                        );
                        if let Some(hover) = self.egui_ui.hovered_pixel {
                            ui_kit::property_row(ui, "hover", viewport_hover_status_label(hover));
                        }
                        if let Some(readout) = &self.egui_ui.hovered_source_readout {
                            ui_kit::property_row(ui, "readout", readout);
                        }
                        ui_kit::property_row(
                            ui,
                            "playback",
                            playback_status_label(
                                application_snapshot.transient().playback_active(),
                                view.timepoint(),
                                timepoint_count,
                            ),
                        );
                        ui_kit::property_row(
                            ui,
                            "path",
                            &viewer_ui_snapshot.dataset_path,
                        );
                        ui.horizontal_wrapped(|ui| {
                            show_playback_controls(
                                ui,
                                &application_snapshot,
                                view,
                                workflow_busy,
                                &mut application_commands,
                            );
                        });
                    });
                    ui_kit::section(ui, "Layers", |ui| {
                        for layer in view.layers() {
                            let catalog_layer = application_snapshot
                                .catalog()
                                .layer(layer.layer_key())
                                .expect("application view closes over the dataset catalog");
                            let selected = layer.layer_key() == view.active_layer();
                            let detail = format!(
                                "{} {:?} t{} z{} y{} x{}",
                                layer.layer_key().ordinal(),
                                catalog_layer.dtype(),
                                catalog_layer.shape().t(),
                                catalog_layer.shape().z(),
                                catalog_layer.shape().y(),
                                catalog_layer.shape().x()
                            );
                            ui.horizontal(|ui| {
                                let mut visible = layer.visible();
                                if ui
                                    .checkbox(&mut visible, "")
                                    .on_hover_text(format!("Show {}", catalog_layer.label()))
                                    .changed()
                                {
                                    application_commands.push(layer_view_command(
                                        layer,
                                        visible,
                                        layer.transfer().clone(),
                                        *layer.render_state(),
                                    ));
                                }
                                if ui_kit::layer_row(
                                    ui,
                                    selected,
                                    layer.visible(),
                                    catalog_layer.label(),
                                    &detail,
                                )
                                .clicked()
                                    && !selected
                                {
                                    application_commands.push(ApplicationCommand::SetActiveLayer(
                                        layer.layer_key(),
                                    ));
                                }
                                let mut mode = layer.render_state().mode();
                                egui::ComboBox::from_id_salt(format!(
                                    "layer-render-mode-{}",
                                    layer.layer_key().ordinal()
                                ))
                                .selected_text(render_mode_label(mode))
                                .width(72.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut mode,
                                        mirante4d_domain::RenderMode::Mip,
                                        "MIP",
                                    );
                                    ui.selectable_value(
                                        &mut mode,
                                        mirante4d_domain::RenderMode::Isosurface,
                                        "ISO",
                                    );
                                    ui.selectable_value(
                                        &mut mode,
                                        mirante4d_domain::RenderMode::Dvr,
                                        "DVR",
                                    );
                                });
                                if mode != layer.render_state().mode() {
                                    match layer_render_state_for_mode(layer, mode) {
                                        Ok(render_state) => application_commands.push(
                                            layer_view_command(
                                                layer,
                                                layer.visible(),
                                                layer.transfer().clone(),
                                                render_state,
                                            ),
                                        ),
                                        Err(error) => tracing::warn!(%error, "render mode change rejected"),
                                    }
                                }
                            });
                        }
                        let active_id = view.active_layer().ordinal().to_string();
                        ui_kit::property_row(ui, "active ID", active_id);
                    });
                    ui_kit::section(ui, "Channel Presets", |ui| {
                        let presets = workspace_channel_presets(&application_snapshot);
                        if presets.is_empty() {
                            ui_kit::status_badge(ui, StatusTone::Warning, "no channel presets");
                        } else {
                            let selected = application_snapshot
                                .transient()
                                .selected_channel_preset()
                                .and_then(|id| presets.iter().find(|preset| preset.id() == id))
                                .unwrap_or(&presets[0]);
                            egui::ComboBox::from_label("channel preset")
                                .selected_text(selected.label())
                                .show_ui(ui, |ui| {
                                    for preset in presets {
                                        if ui
                                            .selectable_label(
                                                preset.id() == selected.id(),
                                                preset.label(),
                                            )
                                            .clicked()
                                        {
                                            application_commands.push(
                                                ApplicationCommand::ApplyChannelPreset(
                                                    preset.id().clone(),
                                                ),
                                            );
                                        }
                                    }
                                });
                            ui.horizontal_wrapped(|ui| {
                                if ui_kit::toolbar_button(ui, "Apply", true).clicked() {
                                    application_commands.push(
                                        ApplicationCommand::ApplyChannelPreset(
                                            selected.id().clone(),
                                        ),
                                    );
                                }
                                if ui_kit::toolbar_button(ui, "Save Current", true).clicked() {
                                    let id = next_user_channel_preset_id(presets);
                                    let label = format!("Display {}", presets.len() + 1);
                                    match channel_preset_from_current_view(view, id, label) {
                                        Ok(preset) => application_commands
                                            .push(ApplicationCommand::UpsertChannelPreset(preset)),
                                        Err(error) => tracing::warn!(%error, "channel preset creation rejected"),
                                    }
                                }
                                if ui_kit::toolbar_button(ui, "Update", true).clicked() {
                                    match channel_preset_from_current_view(
                                        view,
                                        selected.id().clone(),
                                        selected.label(),
                                    ) {
                                        Ok(preset) => application_commands
                                            .push(ApplicationCommand::UpsertChannelPreset(preset)),
                                        Err(error) => tracing::warn!(%error, "channel preset update rejected"),
                                    }
                                }
                            });
                        }
                    });
                });
            });

        egui::Panel::right("inspector-panel")
            .resizable(true)
            .default_size(layout.right_panel_width)
            .size_range(layout.right_width_range())
            .show_inside(ui, |ui| {
                ui_kit::panel_scroll(ui, "inspector-panel-scroll", |ui| {
                    ui_kit::section(ui, "Inspector", |ui| {
                        ui_kit::property_row(
                            ui,
                            "dtype",
                            format!("{:?}", active_catalog_layer.dtype()),
                        );
                        if let Some(label) =
                            active_layer_no_data_policy_label(&application_snapshot)
                        {
                            ui_kit::property_row(ui, "no-data", label);
                        }
                        let mut visible = active_layer.visible();
                        if ui.checkbox(&mut visible, "channel visible").changed() {
                            application_commands.push(layer_view_command(
                                active_layer,
                                visible,
                                active_layer.transfer().clone(),
                                *active_layer.render_state(),
                            ));
                        }
                        let mut opacity = active_layer.transfer().opacity().get();
                        ui.horizontal(|ui| {
                            ui.label("channel opacity");
                            if ui
                                .add(egui::Slider::new(&mut opacity, 0.0..=1.0).show_value(true))
                                .changed()
                                && let Ok(opacity) = Opacity::new(opacity)
                            {
                                let current = active_layer.transfer();
                                let transfer = LayerTransfer::new(
                                    current.window(),
                                    current.color(),
                                    opacity,
                                    current.curve(),
                                    current.invert(),
                                );
                                application_commands.push(layer_view_command(
                                    active_layer,
                                    active_layer.visible(),
                                    transfer,
                                    *active_layer.render_state(),
                                ));
                            }
                        });
                        let mut window_low = active_layer.transfer().window().low();
                        let mut window_high = active_layer.transfer().window().high();
                        ui.horizontal(|ui| {
                            ui.label("display window");
                            let low_changed = ui
                                .add(
                                    egui::DragValue::new(&mut window_low)
                                        .speed(1.0)
                                        .prefix("low "),
                                )
                                .changed();
                            let high_changed = ui
                                .add(
                                    egui::DragValue::new(&mut window_high)
                                        .speed(1.0)
                                        .prefix("high "),
                                )
                                .changed();
                            if (low_changed || high_changed)
                                && let Ok(window) = DisplayWindow::new(window_low, window_high)
                            {
                                let current = active_layer.transfer();
                                let transfer = LayerTransfer::new(
                                    window,
                                    current.color(),
                                    current.opacity(),
                                    current.curve(),
                                    current.invert(),
                                );
                                application_commands.push(layer_view_command(
                                    active_layer,
                                    active_layer.visible(),
                                    transfer,
                                    *active_layer.render_state(),
                                ));
                            }
                        });
                        let [red, green, blue] = active_layer.transfer().color().rgb();
                        let mut color_rgba = [red, green, blue, 1.0];
                        ui.horizontal(|ui| {
                            ui.label("channel color");
                            if ui
                                .color_edit_button_rgba_unmultiplied(&mut color_rgba)
                                .changed()
                                && let Ok(color) =
                                    RgbColor::new([color_rgba[0], color_rgba[1], color_rgba[2]])
                            {
                                let current = active_layer.transfer();
                                let transfer = LayerTransfer::new(
                                    current.window(),
                                    color,
                                    current.opacity(),
                                    current.curve(),
                                    current.invert(),
                                );
                                application_commands.push(layer_view_command(
                                    active_layer,
                                    active_layer.visible(),
                                    transfer,
                                    *active_layer.render_state(),
                                ));
                            }
                        });
                        let mut gamma = active_layer.transfer().curve().gamma_value();
                        ui.horizontal(|ui| {
                            ui.label("transfer gamma");
                            if ui
                                .add(
                                    egui::Slider::new(
                                        &mut gamma,
                                        TRANSFER_GAMMA_MIN..=TRANSFER_GAMMA_MAX,
                                    )
                                    .show_value(true),
                                )
                                .changed()
                                && let Ok(curve) = TransferCurve::gamma(gamma)
                            {
                                let current = active_layer.transfer();
                                let transfer = LayerTransfer::new(
                                    current.window(),
                                    current.color(),
                                    current.opacity(),
                                    curve,
                                    current.invert(),
                                );
                                application_commands.push(layer_view_command(
                                    active_layer,
                                    active_layer.visible(),
                                    transfer,
                                    *active_layer.render_state(),
                                ));
                            }
                        });
                        let mut invert = active_layer.transfer().invert();
                        if ui.checkbox(&mut invert, "invert LUT").changed() {
                            let current = active_layer.transfer();
                            let transfer = LayerTransfer::new(
                                current.window(),
                                current.color(),
                                current.opacity(),
                                current.curve(),
                                invert,
                            );
                            application_commands.push(layer_view_command(
                                active_layer,
                                active_layer.visible(),
                                transfer,
                                *active_layer.render_state(),
                            ));
                        }
                        let active_curve = active_layer.transfer().curve();
                        let active_preset_label = built_in_transfer_presets()
                            .into_iter()
                            .find(|preset| built_in_transfer_preset_curve(*preset) == active_curve)
                            .map(built_in_transfer_preset_label)
                            .unwrap_or("Custom");
                        egui::ComboBox::from_label("transfer preset")
                            .selected_text(active_preset_label)
                            .show_ui(ui, |ui| {
                                for preset in built_in_transfer_presets() {
                                    if ui
                                        .selectable_label(
                                            active_curve == built_in_transfer_preset_curve(preset),
                                            built_in_transfer_preset_label(preset),
                                        )
                                        .clicked()
                                    {
                                        let current = active_layer.transfer();
                                        let transfer = LayerTransfer::new(
                                            current.window(),
                                            current.color(),
                                            current.opacity(),
                                            built_in_transfer_preset_curve(preset),
                                            current.invert(),
                                        );
                                        application_commands.push(layer_view_command(
                                            active_layer,
                                            active_layer.visible(),
                                            transfer,
                                            *active_layer.render_state(),
                                        ));
                                    }
                                }
                            });
                        let histogram = &active_layer_histogram_for_ui;
                        ui_kit::property_row(ui, "histogram", histogram_status_label(histogram));
                        ui_kit::property_row(ui, "histogram bins", histogram_bins_label(histogram));
                        ui.horizontal_wrapped(|ui| {
                            if ui_kit::toolbar_button(
                                ui,
                                "Auto Dense",
                                histogram_can_auto_window(histogram),
                            )
                            .clicked()
                            {
                                match auto_dense_window_from_histogram(histogram) {
                                    Ok(window) => {
                                        let current = active_layer.transfer();
                                        let transfer = LayerTransfer::new(
                                            window,
                                            current.color(),
                                            current.opacity(),
                                            current.curve(),
                                            current.invert(),
                                        );
                                        application_commands.push(layer_view_command(
                                            active_layer,
                                            active_layer.visible(),
                                            transfer,
                                            *active_layer.render_state(),
                                        ));
                                    }
                                    Err(error) => {
                                        tracing::warn!(%error, "auto dense window rejected")
                                    }
                                }
                            }
                            if ui_kit::toolbar_button(
                                ui,
                                "Auto Signal",
                                histogram_can_auto_window(histogram),
                            )
                            .clicked()
                            {
                                match auto_signal_window_from_histogram(histogram) {
                                    Ok(window) => {
                                        let current = active_layer.transfer();
                                        let transfer = LayerTransfer::new(
                                            window,
                                            current.color(),
                                            current.opacity(),
                                            current.curve(),
                                            current.invert(),
                                        );
                                        application_commands.push(layer_view_command(
                                            active_layer,
                                            active_layer.visible(),
                                            transfer,
                                            *active_layer.render_state(),
                                        ));
                                    }
                                    Err(error) => {
                                        tracing::warn!(%error, "auto signal window rejected")
                                    }
                                }
                            }
                        });
                        ui_kit::property_row(
                            ui,
                            "shape",
                            format!(
                                "t{} z{} y{} x{}",
                                active_catalog_layer.shape().t(),
                                active_catalog_layer.shape().z(),
                                active_catalog_layer.shape().y(),
                                active_catalog_layer.shape().x()
                            ),
                        );
                        ui_kit::property_row(
                            ui,
                            "timepoint",
                            format!("{}/{}", view.timepoint().get() + 1, timepoint_count),
                        );
                    });
                    ui_kit::section(ui, "Frame", |ui| {
                        ui_kit::show_frame_fidelity_property_rows(
                            ui,
                            &viewer_ui_snapshot.frame_fidelity,
                        );
                        ui_kit::property_row(
                            ui,
                            "pixels",
                            viewer_ui_snapshot
                                .render_viewport
                                .width_pixels()
                                .saturating_mul(viewer_ui_snapshot.render_viewport.height_pixels())
                                .to_string(),
                        );
                        ui_kit::property_row(ui, "nonzero", "unavailable");
                        ui_kit::property_row(ui, "max", "unavailable");
                        ui_kit::property_row(ui, "mean", "unavailable");
                    });
                    ui_kit::section(ui, "Viewer Tools", |ui| {
                        let mut active_tool = application_snapshot.transient().active_tool();
                        ui.horizontal_wrapped(|ui| {
                            ui.selectable_value(&mut active_tool, ToolKind::Navigate, "Navigate");
                            ui.selectable_value(&mut active_tool, ToolKind::Inspect, "Inspect");
                            ui.selectable_value(&mut active_tool, ToolKind::Crosshair, "Crosshair");
                        });
                        if active_tool != application_snapshot.transient().active_tool() {
                            application_commands
                                .push(ApplicationCommand::SetActiveTool(active_tool));
                        }
                        if let Some(crosshair) = &self.egui_ui.viewer_tools.crosshair
                            && let Some(screen) = crosshair.screen_position
                        {
                            ui_kit::property_row(
                                ui,
                                "crosshair",
                                format!("x{:.0} y{:.0} {:?}", screen.x, screen.y, crosshair.kind),
                            );
                        }
                        if let Some(selection) = &self.egui_ui.viewer_tools.selection {
                            ui_kit::property_row(ui, "selection", format!("{selection:?}"));
                        }
                    });
                    ui_kit::section(ui, "Analysis", |ui| {
                        let transient = application_snapshot.transient();
                        let can_start = analysis_start_unavailable_reason.is_none();
                        let mut start_time = false;
                        let mut start_box = false;
                        ui.horizontal_wrapped(|ui| {
                            start_time =
                                ui_kit::toolbar_button(ui, "Analyze Time", can_start).clicked();
                            start_box =
                                ui_kit::toolbar_button(ui, "Analyze Box", can_start).clicked();
                            if ui_kit::toolbar_button(ui, "Cancel", analysis_active).clicked() {
                                actions.push(WorkbenchUiAction::CancelAnalysis);
                            }
                            if ui_kit::toolbar_button(ui, "Workspace", true).clicked() {
                                self.egui_ui.analysis_workspace_open = true;
                            }
                            if ui_kit::toolbar_button(
                                ui,
                                "Copy CSV",
                                transient.selected_analysis_table().is_some(),
                            )
                            .clicked()
                            {
                                actions.push(WorkbenchUiAction::CopySelectedAnalysisCsv);
                            }
                        });
                        let layer_shape = application_snapshot
                            .catalog()
                            .layer(view.active_layer())
                            .expect("the active layer closes over the catalog")
                            .shape()
                            .spatial()
                            .dimensions();
                        let mut roi_origin = analysis_roi_origin;
                        let mut roi_shape = analysis_roi_shape;
                        ui.label("Box coordinates (z, y, x voxels)");
                        ui.small("Analyze Box uses the current timepoint.");
                        ui.horizontal_wrapped(|ui| {
                            for axis in 0..3 {
                                ui.add(
                                    egui::DragValue::new(&mut roi_origin[axis])
                                        .range(0..=layer_shape[axis].saturating_sub(1))
                                        .prefix(format!("{} ", ["z", "y", "x"][axis])),
                                );
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            for axis in 0..3 {
                                let maximum = layer_shape[axis].saturating_sub(roi_origin[axis]);
                                ui.add(
                                    egui::DragValue::new(&mut roi_shape[axis])
                                        .range(1..=maximum.max(1))
                                        .prefix(format!("{} size ", ["z", "y", "x"][axis])),
                                );
                            }
                        });
                        for axis in 0..3 {
                            roi_origin[axis] =
                                roi_origin[axis].min(layer_shape[axis].saturating_sub(1));
                            roi_shape[axis] = roi_shape[axis]
                                .clamp(1, layer_shape[axis].saturating_sub(roi_origin[axis]));
                        }
                        if roi_origin != analysis_roi_origin || roi_shape != analysis_roi_shape {
                            actions.push(WorkbenchUiAction::SetAnalysisRoi {
                                origin: roi_origin,
                                shape: roi_shape,
                            });
                        }
                        if start_time {
                            actions.push(WorkbenchUiAction::StartAnalysis(
                                WorkbenchAnalysisKind::FullTimeTrace,
                            ));
                        }
                        if start_box {
                            actions.push(WorkbenchUiAction::StartAnalysis(
                                WorkbenchAnalysisKind::CurrentTimepointBox,
                            ));
                        }
                        if let Some(reason) = analysis_start_unavailable_reason.as_deref() {
                            ui_kit::status_badge(ui, StatusTone::Warning, reason);
                        }
                        let commands = show_analysis_workspace(
                            ui,
                            &analysis_workspace_view,
                            &mut self.egui_ui,
                        );
                        application_commands.extend(commands);
                    });
                    ui_kit::section(ui, "Settings", |ui| {
                        ui_kit::show_settings_body(
                            ui,
                            &mut self.egui_ui.settings_runtime_draft,
                            &settings_ui_view,
                            &mut actions,
                        );
                    });
                    egui::CollapsingHeader::new("Runtime Diagnostics")
                        .default_open(false)
                        .show(ui, |ui| {
                            runtime_diagnostics_panel::show_runtime_diagnostics_body(
                                &runtime_diagnostics_view,
                                ui,
                                &mut actions,
                            );
                        });
                    ui_kit::section(ui, "Render Settings", |ui| {
                        let current_render = *active_layer.render_state();
                        let mut sampling_policy = current_render.sampling_policy();
                        egui::ComboBox::from_label("sampling")
                            .selected_text(render_sampling_policy_label(sampling_policy))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut sampling_policy,
                                    SamplingPolicy::VoxelExact,
                                    render_sampling_policy_label(SamplingPolicy::VoxelExact),
                                );
                            });
                        if sampling_policy != current_render.sampling_policy() {
                            match render_state_with_sampling(current_render, sampling_policy) {
                                Ok(render_state) => application_commands.push(layer_view_command(
                                    active_layer,
                                    active_layer.visible(),
                                    active_layer.transfer().clone(),
                                    render_state,
                                )),
                                Err(error) => tracing::warn!(%error, "sampling change rejected"),
                            }
                        }
                        ui_kit::property_row(
                            ui,
                            "sampling policy",
                            render_sampling_policy_label(current_render.sampling_policy()),
                        );
                        match current_render.mode() {
                            mirante4d_domain::RenderMode::Mip => {
                                ui_kit::property_row(
                                    ui,
                                    "MIP projection",
                                    "maximum intensity along the camera ray",
                                );
                                ui_kit::property_row(
                                    ui,
                                    "MIP display mapping",
                                    "active channel window and color",
                                );
                            }
                            mirante4d_domain::RenderMode::Isosurface => {
                                let parameters = current_render
                                    .iso_parameters()
                                    .expect("ISO mode has ISO parameters");
                                let mut display_level = parameters.display_level();
                                if ui
                                    .add(
                                        egui::Slider::new(&mut display_level, 0.0..=1.0)
                                            .text("ISO display level"),
                                    )
                                    .changed()
                                    && let Ok(render_state) = CanonicalRenderState::iso(
                                        current_render.sampling_policy(),
                                        parameters.shading_policy(),
                                        display_level,
                                    )
                                {
                                    application_commands.push(layer_view_command(
                                        active_layer,
                                        active_layer.visible(),
                                        active_layer.transfer().clone(),
                                        render_state,
                                    ));
                                }
                                let mut iso_shading_policy = parameters.shading_policy();
                                egui::ComboBox::from_label("ISO shading")
                                    .selected_text(iso_shading_policy_label(iso_shading_policy))
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut iso_shading_policy,
                                            IsoShadingPolicy::Flat,
                                            "Flat threshold hit",
                                        );
                                    });
                                if iso_shading_policy != parameters.shading_policy()
                                    && let Ok(render_state) = CanonicalRenderState::iso(
                                        current_render.sampling_policy(),
                                        iso_shading_policy,
                                        parameters.display_level(),
                                    )
                                {
                                    application_commands.push(layer_view_command(
                                        active_layer,
                                        active_layer.visible(),
                                        active_layer.transfer().clone(),
                                        render_state,
                                    ));
                                }
                                show_iso_light_controls(
                                    ui,
                                    *view.iso_light(),
                                    &mut application_commands,
                                );
                                ui_kit::property_row(
                                    ui,
                                    "ISO surface rule",
                                    "first display-level crossing along the camera ray",
                                );
                                ui_kit::property_row(ui, "ISO pick policy", "display-level hit");
                            }
                            mirante4d_domain::RenderMode::Dvr => {
                                let parameters = current_render
                                    .dvr_parameters()
                                    .expect("DVR mode has DVR parameters");
                                let mut density_scale = parameters.density_scale();
                                if ui
                                    .add(
                                        egui::Slider::new(
                                            &mut density_scale,
                                            DVR_DENSITY_SCALE_MIN..=DVR_DENSITY_SCALE_MAX,
                                        )
                                        .text("DVR density scale"),
                                    )
                                    .changed()
                                    && let Ok(render_state) = CanonicalRenderState::dvr(
                                        current_render.sampling_policy(),
                                        parameters.opacity_transfer(),
                                        density_scale,
                                    )
                                {
                                    application_commands.push(layer_view_command(
                                        active_layer,
                                        active_layer.visible(),
                                        active_layer.transfer().clone(),
                                        render_state,
                                    ));
                                }
                                let opacity = parameters.opacity_transfer();
                                let mut opacity_low = opacity.window().low();
                                let mut opacity_high = opacity.window().high();
                                let mut opacity_window_changed = false;
                                ui.horizontal(|ui| {
                                    ui.label("opacity low");
                                    if ui.add(egui::DragValue::new(&mut opacity_low)).changed() {
                                        opacity_window_changed = true;
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("opacity high");
                                    if ui.add(egui::DragValue::new(&mut opacity_high)).changed() {
                                        opacity_window_changed = true;
                                    }
                                });
                                if opacity_window_changed
                                    && let Ok(window) =
                                        DisplayWindow::new(opacity_low, opacity_high)
                                {
                                    let opacity_transfer =
                                        CanonicalDvrOpacityTransfer::new(window, opacity.curve());
                                    if let Ok(render_state) = CanonicalRenderState::dvr(
                                        current_render.sampling_policy(),
                                        opacity_transfer,
                                        parameters.density_scale(),
                                    ) {
                                        application_commands.push(layer_view_command(
                                            active_layer,
                                            active_layer.visible(),
                                            active_layer.transfer().clone(),
                                            render_state,
                                        ));
                                    }
                                }
                                let mut opacity_gamma = opacity.curve().gamma_value();
                                ui.horizontal(|ui| {
                                    ui.label("opacity gamma");
                                    if ui
                                        .add(
                                            egui::Slider::new(
                                                &mut opacity_gamma,
                                                TRANSFER_GAMMA_MIN..=TRANSFER_GAMMA_MAX,
                                            )
                                            .show_value(true),
                                        )
                                        .changed()
                                        && let Ok(curve) = TransferCurve::gamma(opacity_gamma)
                                        && let Ok(render_state) = CanonicalRenderState::dvr(
                                            current_render.sampling_policy(),
                                            CanonicalDvrOpacityTransfer::new(
                                                opacity.window(),
                                                curve,
                                            ),
                                            parameters.density_scale(),
                                        )
                                    {
                                        application_commands.push(layer_view_command(
                                            active_layer,
                                            active_layer.visible(),
                                            active_layer.transfer().clone(),
                                            render_state,
                                        ));
                                    }
                                });
                                let histogram = &active_layer_histogram_for_ui;
                                ui.horizontal_wrapped(|ui| {
                                    if ui_kit::toolbar_button(
                                        ui,
                                        "Auto Opacity",
                                        histogram_can_auto_window(histogram),
                                    )
                                    .clicked()
                                    {
                                        match auto_dvr_opacity_transfer_from_histogram(histogram) {
                                            Ok(transfer) => {
                                                let opacity_transfer =
                                                    CanonicalDvrOpacityTransfer::new(
                                                        transfer.window(),
                                                        transfer.curve(),
                                                    );
                                                if let Ok(render_state) = CanonicalRenderState::dvr(
                                                    current_render.sampling_policy(),
                                                    opacity_transfer,
                                                    parameters.density_scale(),
                                                ) {
                                                    application_commands.push(layer_view_command(
                                                        active_layer,
                                                        active_layer.visible(),
                                                        active_layer.transfer().clone(),
                                                        render_state,
                                                    ));
                                                }
                                            }
                                            Err(error) => {
                                                tracing::warn!(%error, "auto DVR opacity rejected")
                                            }
                                        }
                                    }
                                    if ui_kit::toolbar_button(ui, "Reset Opacity", true).clicked() {
                                        let opacity_transfer = CanonicalDvrOpacityTransfer::new(
                                            active_layer.transfer().window(),
                                            TransferCurve::gamma(DEFAULT_DVR_OPACITY_GAMMA)
                                                .expect("default DVR opacity gamma is valid"),
                                        );
                                        if let Ok(render_state) = CanonicalRenderState::dvr(
                                            current_render.sampling_policy(),
                                            opacity_transfer,
                                            parameters.density_scale(),
                                        ) {
                                            application_commands.push(layer_view_command(
                                                active_layer,
                                                active_layer.visible(),
                                                active_layer.transfer().clone(),
                                                render_state,
                                            ));
                                        }
                                    }
                                });
                                ui_kit::property_row(
                                    ui,
                                    "DVR opacity transfer",
                                    "source window plus independent opacity gamma",
                                );
                                ui_kit::property_row(
                                    ui,
                                    "DVR termination",
                                    "front-to-back alpha, stops near opaque",
                                );
                            }
                        }
                    });
                    ui_kit::section(ui, "Camera", |ui| {
                        ui_kit::property_row(
                            ui,
                            "projection",
                            format!("{:?}", view.camera().projection()),
                        );
                        if let Ok(camera_frame) = CameraFrame::new(
                            *view.camera(),
                            viewer_ui_snapshot.presentation_viewport,
                        ) {
                            let camera_forward = camera_frame.axes().forward();
                            ui_kit::property_row(
                                ui,
                                "forward",
                                format!(
                                    "{:.2}, {:.2}, {:.2}",
                                    camera_forward[0], camera_forward[1], camera_forward[2]
                                ),
                            );
                            if let Ok(scale) = camera_frame.world_per_screen_point_at_target() {
                                ui_kit::property_row(ui, "scale", format!("{scale:.4} world/pt"));
                            }
                        }
                    });
                    ui_kit::section(ui, "Messages", |ui| {
                        if let Some(message) = ui_kit::application_problem_message(
                            application_snapshot.latest_problem(),
                        ) {
                            ui_kit::status_badge(ui, StatusTone::Error, message);
                        }
                        if let ImportWorkflowSnapshot::Failed(failure) = import_snapshot {
                            ui_kit::status_badge(ui, StatusTone::Error, &failure.message);
                        }
                        for message in &viewer_ui_snapshot.messages {
                            ui_kit::status_badge(ui, StatusTone::Error, message);
                        }
                        if let Some(hover) = self.egui_ui.hovered_pixel {
                            ui_kit::property_row(
                                ui,
                                "hover",
                                format!("x{} y{} intensity {}", hover.x, hover.y, hover.intensity),
                            );
                        }
                    });
                });
            });

        let mut viewer_output = WorkbenchUiOutput::default();
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.show_viewer_layout(
                ui,
                &application_snapshot,
                view,
                &viewer_ui_snapshot,
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
        Self::show_project_recovery_ui(ui.ctx(), &project_recovery_ui, &mut actions);
        Self::show_dirty_project_close_prompt(ui.ctx(), dirty_project_close_ui, &mut actions);
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

pub(crate) fn viewport_hover_status_label(hover: ViewportHover) -> String {
    format!(
        "hover x{} y{} intensity {}",
        hover.x, hover.y, hover.intensity
    )
}

#[cfg(test)]
mod tests {
    use egui_kittest::{Harness, kittest::Queryable};

    use super::*;

    struct DirtyCloseHarnessState {
        input: DirtyProjectCloseUi,
        actions: Vec<WorkbenchUiAction>,
    }

    struct ProjectRecoveryHarnessState {
        input: ProjectRecoveryUi,
        actions: Vec<WorkbenchUiAction>,
    }

    #[test]
    fn viewport_hover_status_label_exposes_pixel_intensity() {
        assert_eq!(
            viewport_hover_status_label(ViewportHover {
                x: 12,
                y: 34,
                intensity: ViewportIntensity::U16(567),
            }),
            "hover x12 y34 intensity 567"
        );
    }

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

    #[test]
    fn dirty_project_prompt_returns_ordered_choices_without_app_side_effects() {
        let mut harness = Harness::builder().build_ui_state(
            |ui, state: &mut DirtyCloseHarnessState| {
                state.actions.clear();
                MiranteWorkbenchApp::show_dirty_project_close_prompt(
                    ui.ctx(),
                    state.input,
                    &mut state.actions,
                );
            },
            DirtyCloseHarnessState {
                input: DirtyProjectCloseUi {
                    open: true,
                    pending_dataset_open: true,
                    save_action: DirtyProjectSaveAction::SaveAs,
                },
                actions: Vec::new(),
            },
        );

        harness.get_by_label("Save As").click();
        harness.step();
        assert_eq!(
            harness.state().actions,
            vec![WorkbenchUiAction::SaveDirtyProjectAs]
        );

        harness.get_by_label("Discard").click();
        harness.step();
        assert_eq!(
            harness.state().actions,
            vec![WorkbenchUiAction::DiscardDirtyProject]
        );

        harness.get_by_label("Cancel").click();
        harness.step();
        assert_eq!(
            harness.state().actions,
            vec![WorkbenchUiAction::CancelDirtyProjectClose]
        );
    }

    #[test]
    fn project_recovery_windows_return_exact_selected_identities() {
        let generation_id = ProjectGenerationId::parse(&format!(
            "{}{}",
            ProjectGenerationId::PREFIX,
            "11".repeat(32)
        ))
        .unwrap();
        let mut review_harness = Harness::builder().build_ui_state(
            |ui, state: &mut ProjectRecoveryHarnessState| {
                state.actions.clear();
                MiranteWorkbenchApp::show_project_recovery_ui(
                    ui.ctx(),
                    &state.input,
                    &mut state.actions,
                );
            },
            ProjectRecoveryHarnessState {
                input: ProjectRecoveryUi {
                    review_generation: Some(generation_id),
                    panel_open: false,
                    candidates: Vec::new(),
                    locators: Vec::new(),
                    can_open_locator: false,
                },
                actions: Vec::new(),
            },
        );

        review_harness.get_by_label("Recover Autosave").click();
        review_harness.step();
        assert_eq!(
            review_harness.state().actions,
            vec![WorkbenchUiAction::RecoverReviewedAutosave(generation_id)]
        );

        let project_id = ProjectId::from_bytes([7; 16]);
        let mut locator_harness = Harness::builder().build_ui_state(
            |ui, state: &mut ProjectRecoveryHarnessState| {
                state.actions.clear();
                MiranteWorkbenchApp::show_project_recovery_ui(
                    ui.ctx(),
                    &state.input,
                    &mut state.actions,
                );
            },
            ProjectRecoveryHarnessState {
                input: ProjectRecoveryUi {
                    review_generation: None,
                    panel_open: true,
                    candidates: Vec::new(),
                    locators: vec![project_id],
                    can_open_locator: true,
                },
                actions: Vec::new(),
            },
        );

        locator_harness.get_by_label("Inspect and Recover").click();
        locator_harness.step();
        assert_eq!(
            locator_harness.state().actions,
            vec![WorkbenchUiAction::OpenRecoveryLocator(project_id)]
        );
    }
}
