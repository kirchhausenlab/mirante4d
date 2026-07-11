use super::*;
fn show_iso_light_controls(
    ui: &mut egui::Ui,
    light_state: IsoLightState,
    workbench_commands: &mut Vec<WorkbenchCommand>,
) {
    let attached = light_state.mode == IsoLightMode::AttachedCamera;
    ui.horizontal(|ui| {
        let mut attached_toggle = attached;
        if ui
            .checkbox(&mut attached_toggle, "Attached light")
            .changed()
        {
            workbench_commands.push(WorkbenchCommand::SetIsoLightAttached {
                attached: attached_toggle,
            });
        }
        if ui.button("Reset").clicked() {
            workbench_commands.push(WorkbenchCommand::ResetIsoLight);
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
    let position = light_state.detached_screen_position;
    let marker = egui::pos2(
        center.x + position.x * radius,
        center.y - position.y * radius,
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
        let x = ((pointer.x - center.x) / radius).clamp(-1.0, 1.0);
        let y = (-(pointer.y - center.y) / radius).clamp(-1.0, 1.0);
        workbench_commands.push(WorkbenchCommand::SetIsoLightDetachedPosition { x, y });
    }
}

fn layout_selector(
    ui: &mut egui::Ui,
    current: ViewerLayout,
    workbench_commands: &mut Vec<WorkbenchCommand>,
) {
    ui_kit::muted_label(ui, "Layout");
    if ui
        .selectable_label(current == ViewerLayout::Single3d, "3D")
        .clicked()
    {
        workbench_commands.push(WorkbenchCommand::SetViewerLayout(ViewerLayout::Single3d));
    }
    if ui
        .selectable_label(current == ViewerLayout::FourPanel, "4 Panel")
        .clicked()
    {
        workbench_commands.push(WorkbenchCommand::SetViewerLayout(ViewerLayout::FourPanel));
    }
}

const CROSS_SECTION_SCROLL_POINTS_PER_NOTCH: f32 = 120.0;
const CROSS_SECTION_SCROLL_ZOOM_FACTOR_SCALE: f32 = 0.001;

impl MiranteWorkbenchApp {
    fn show_viewer_layout(
        &mut self,
        ui: &mut egui::Ui,
        workbench_commands: &mut Vec<WorkbenchCommand>,
        rerender_requested: &mut bool,
        texture_refresh_requested: &mut bool,
    ) {
        match self.state.viewer_layout.layout() {
            ViewerLayout::Single3d => self.show_single_3d_viewport(
                ui,
                workbench_commands,
                rerender_requested,
                texture_refresh_requested,
            ),
            ViewerLayout::FourPanel => self.show_four_panel_viewport(
                ui,
                workbench_commands,
                rerender_requested,
                texture_refresh_requested,
            ),
        }
    }

    fn sync_3d_viewport_for_display_size(
        &mut self,
        ctx: &egui::Context,
        display_size_points: egui::Vec2,
    ) {
        let max_texture_side = ctx.input(|input| input.max_texture_side);
        let mut viewport_changed = false;
        if let Some(presentation_viewport) =
            presentation_viewport_for_display_size(display_size_points)
        {
            viewport_changed |= set_presentation_viewport(&mut self.state, presentation_viewport);
        }
        if let Some(render_viewport) = render_viewport_for_display_size(
            display_size_points,
            ctx.pixels_per_point(),
            max_texture_side,
        ) {
            viewport_changed |= set_render_viewport(&mut self.state, render_viewport);
        }
        if viewport_changed {
            self.refresh_frame(ctx);
        }
    }

    fn record_four_panel_viewport(
        &mut self,
        ctx: &egui::Context,
        panel_id: PanelId,
        display_size_points: egui::Vec2,
    ) -> Option<PresentationViewport> {
        let Some(presentation_viewport) =
            presentation_viewport_for_display_size(display_size_points)
        else {
            return None;
        };
        let Some(render_viewport) = render_viewport_for_display_size(
            display_size_points,
            ctx.pixels_per_point(),
            ctx.input(|input| input.max_texture_side),
        ) else {
            return None;
        };
        self.state.viewer_layout.record_panel_viewports(
            panel_id,
            presentation_viewport,
            render_viewport,
        );
        Some(presentation_viewport)
    }

    fn show_single_3d_viewport(
        &mut self,
        ui: &mut egui::Ui,
        workbench_commands: &mut Vec<WorkbenchCommand>,
        rerender_requested: &mut bool,
        texture_refresh_requested: &mut bool,
    ) {
        let available = ui.available_size();
        let ctx = ui.ctx().clone();
        self.sync_3d_viewport_for_display_size(&ctx, available);
        let display_image = self.viewport_display_image(&ctx);
        let image_size = fit_size(display_image.size_vec2(), available);
        ui.centered_and_justified(|ui| {
            self.show_3d_viewport_image(
                ui,
                display_image,
                image_size,
                workbench_commands,
                rerender_requested,
                texture_refresh_requested,
            );
        });
    }

    fn show_four_panel_viewport(
        &mut self,
        ui: &mut egui::Ui,
        workbench_commands: &mut Vec<WorkbenchCommand>,
        rerender_requested: &mut bool,
        texture_refresh_requested: &mut bool,
    ) {
        let available = ui.available_size();
        let gap = 6.0;
        let cell_size = egui::vec2(
            ((available.x - gap) * 0.5).max(1.0),
            ((available.y - gap) * 0.5).max(1.0),
        );
        let panels = self
            .state
            .viewer_layout
            .four_panel_runtime()
            .map(|runtime| {
                let panels = runtime.panels();
                [
                    panels[0].panel_id,
                    panels[1].panel_id,
                    panels[2].panel_id,
                    panels[3].panel_id,
                ]
            })
            .unwrap_or([PanelId::Xy, PanelId::Xz, PanelId::ThreeD, PanelId::Yz]);

        self.state.hovered_pixel = None;
        self.state.hovered_source_readout = None;

        ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
        ui.vertical(|ui| {
            for row in panels.chunks_exact(2) {
                ui.horizontal(|ui| {
                    for panel_id in row {
                        self.show_four_panel_cell(
                            ui,
                            *panel_id,
                            cell_size,
                            workbench_commands,
                            rerender_requested,
                            texture_refresh_requested,
                        );
                    }
                });
            }
        });
    }

    fn show_four_panel_cell(
        &mut self,
        ui: &mut egui::Ui,
        panel_id: PanelId,
        cell_size: egui::Vec2,
        workbench_commands: &mut Vec<WorkbenchCommand>,
        rerender_requested: &mut bool,
        texture_refresh_requested: &mut bool,
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
                        PanelId::ThreeD => self.show_embedded_3d_panel(
                            ui,
                            workbench_commands,
                            rerender_requested,
                            texture_refresh_requested,
                        ),
                        PanelId::Xy | PanelId::Xz | PanelId::Yz => {
                            self.show_cross_section_panel(ui, panel_id, workbench_commands);
                        }
                    }
                });
        });
    }

    fn show_embedded_3d_panel(
        &mut self,
        ui: &mut egui::Ui,
        workbench_commands: &mut Vec<WorkbenchCommand>,
        rerender_requested: &mut bool,
        texture_refresh_requested: &mut bool,
    ) {
        let available = ui.available_size();
        let ctx = ui.ctx().clone();
        self.record_four_panel_viewport(&ctx, PanelId::ThreeD, available);
        self.sync_3d_viewport_for_display_size(&ctx, available);
        let display_image = self.viewport_display_image(&ctx);
        let image_size = fit_size(display_image.size_vec2(), available);
        ui.centered_and_justified(|ui| {
            self.show_3d_viewport_image(
                ui,
                display_image,
                image_size,
                workbench_commands,
                rerender_requested,
                texture_refresh_requested,
            );
        });
    }

    fn show_3d_viewport_image(
        &mut self,
        ui: &mut egui::Ui,
        display_image: ViewportDisplayImage,
        image_size: egui::Vec2,
        workbench_commands: &mut Vec<WorkbenchCommand>,
        rerender_requested: &mut bool,
        texture_refresh_requested: &mut bool,
    ) {
        if image_size == egui::Vec2::ZERO {
            return;
        }
        let image = match display_image {
            ViewportDisplayImage::Cpu(texture) => egui::Image::new(&texture),
            ViewportDisplayImage::Gpu { texture_id, size } => {
                egui::Image::from_texture((texture_id, size))
            }
        };
        let response = ui.add(
            image
                .fit_to_exact_size(image_size)
                .sense(egui::Sense::click_and_drag()),
        );
        let hover = viewport_hover_from_response(&self.state, &response);
        if response.hovered() || self.state.viewer_layout.layout() == ViewerLayout::Single3d {
            self.state.hovered_pixel = hover;
            self.state.hovered_source_readout = None;
        }
        match apply_viewport_tool_response(&mut self.state, &response, hover) {
            Ok(outcome) => {
                *texture_refresh_requested |= outcome.texture_refresh_requested;
                *rerender_requested |= outcome.rerender_requested;
            }
            Err(err) => {
                self.state.last_render_error = Some(err.to_string());
            }
        }
        if matches!(
            self.state.viewer_tools.active_tool,
            ViewerTool::Navigate | ViewerTool::Inspect
        ) {
            workbench_commands.extend(viewport_interaction_commands(
                &mut self.state,
                &response,
                image_size,
            ));
        }
    }

    fn show_cross_section_panel(
        &mut self,
        ui: &mut egui::Ui,
        panel_id: PanelId,
        workbench_commands: &mut Vec<WorkbenchCommand>,
    ) {
        let available = ui.available_size();
        let ctx = ui.ctx().clone();
        let presentation_viewport = self.record_four_panel_viewport(&ctx, panel_id, available);
        if let Err(err) = self.render_cross_section_panel_for_display_if_needed(panel_id) {
            self.state.last_render_error = Some(err.to_string());
            tracing::error!(
                error = %err,
                panel = panel_id.label(),
                "cross-section panel render failed"
            );
        }
        let response =
            if let Some(display_image) = self.cross_section_panel_display_image(panel_id) {
                let image_size = fit_size(display_image.size_vec2(), available);
                if image_size != egui::Vec2::ZERO {
                    Some(
                        ui.centered_and_justified(|ui| {
                            self.show_cross_section_panel_image(
                                ui,
                                display_image,
                                image_size,
                                panel_id,
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
            .unwrap_or_else(|| self.show_cross_section_panel_placeholder(ui, panel_id, available));

        if let Some(presentation_viewport) = presentation_viewport
            && let Some(readout) = cross_section_hover_readout_for_response(
                &self.state,
                panel_id,
                presentation_viewport,
                &response,
            )
        {
            self.state.hovered_pixel = None;
            self.state.hovered_source_readout = Some(readout.text);
        }

        if matches!(
            self.state.viewer_tools.active_tool,
            ViewerTool::Navigate | ViewerTool::Inspect
        ) && let Some(presentation_viewport) = presentation_viewport
        {
            workbench_commands.extend(cross_section_interaction_commands(
                panel_id,
                presentation_viewport,
                &response,
            ));
        }
    }

    fn show_cross_section_panel_image(
        &mut self,
        ui: &mut egui::Ui,
        display_image: ViewportDisplayImage,
        image_size: egui::Vec2,
        panel_id: PanelId,
    ) -> egui::Response {
        let image = match display_image {
            ViewportDisplayImage::Cpu(texture) => egui::Image::new(&texture),
            ViewportDisplayImage::Gpu { texture_id, size } => {
                egui::Image::from_texture((texture_id, size))
            }
        };
        let response = ui.add(
            image
                .fit_to_exact_size(image_size)
                .sense(egui::Sense::click_and_drag()),
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Other,
                ui.is_enabled(),
                format!("{} cross-section panel", panel_id.label()),
            )
        });
        response
    }

    fn show_cross_section_panel_placeholder(
        &mut self,
        ui: &mut egui::Ui,
        panel_id: PanelId,
        available: egui::Vec2,
    ) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(available, egui::Sense::click_and_drag());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Other,
                ui.is_enabled(),
                format!("{} cross-section panel", panel_id.label()),
            )
        });
        let status = self
            .state
            .viewer_layout
            .four_panel_runtime()
            .and_then(|runtime| runtime.panel(panel_id))
            .and_then(|panel| panel.cross_section_schedule)
            .map(|schedule| schedule.status_label());
        let text = status
            .map(|status| format!("{}\n{}", panel_id.label(), status))
            .unwrap_or_else(|| panel_id.label().to_owned());
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(18.0),
            ui.visuals().weak_text_color(),
        );
        response
    }
}

fn cross_section_interaction_commands(
    panel_id: PanelId,
    presentation_viewport: PresentationViewport,
    response: &egui::Response,
) -> Vec<WorkbenchCommand> {
    let mut commands = Vec::new();
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
                commands.push(WorkbenchCommand::CrossSectionRotateDrag {
                    panel_id,
                    motion_points,
                });
            } else {
                commands.push(WorkbenchCommand::CrossSectionPanDrag {
                    panel_id,
                    motion_points,
                });
            }
        }
    }
    if response.hovered() {
        if modifiers.ctrl || modifiers.command {
            let zoom_delta = response.ctx.input(|input| input.zoom_delta());
            if let Some(scroll_y) = scroll_y_points_from_zoom_delta(zoom_delta)
                && let Some(pointer) = response.hover_pos()
            {
                let local = pointer - response.rect.min.to_vec2();
                commands.push(WorkbenchCommand::CrossSectionZoom {
                    panel_id,
                    presentation_viewport,
                    pointer_position_points: local,
                    scroll_y_points: scroll_y,
                });
            }
        } else {
            let scroll_y = response.ctx.input(|input| input.smooth_scroll_delta().y);
            if scroll_y.is_finite() && scroll_y != 0.0 {
                commands.push(WorkbenchCommand::CrossSectionSliceStep {
                    panel_id,
                    notches: f64::from(scroll_y / CROSS_SECTION_SCROLL_POINTS_PER_NOTCH),
                    fast: modifiers.shift,
                });
            }
        }
    }
    commands
}

fn scroll_y_points_from_zoom_delta(zoom_delta: f32) -> Option<f32> {
    if !zoom_delta.is_finite() || zoom_delta <= 0.0 || zoom_delta == 1.0 {
        return None;
    }
    let scroll_y = -zoom_delta.ln() / CROSS_SECTION_SCROLL_ZOOM_FACTOR_SCALE;
    scroll_y.is_finite().then_some(scroll_y)
}

impl eframe::App for MiranteWorkbenchApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let update_started = Instant::now();
        let setup_started = Instant::now();
        self.free_retired_gpu_display_textures();
        self.handle_close_request(ui.ctx());
        let setup_ms = duration_ms(setup_started.elapsed());

        let task_drain_started = Instant::now();
        self.drain_tiff_import_setup_results(ui.ctx());
        self.drain_import_results(ui.ctx());
        self.drain_analysis_results(ui.ctx());
        let task_drain_ms = duration_ms(task_drain_started.elapsed());

        let mut rerender_requested = false;
        let mut texture_refresh_requested = false;
        let mut workbench_commands = Vec::new();
        let import_active = self.import_task.is_some();
        let import_setup_active =
            self.tiff_import_setup_task.is_some() || self.pending_tiff_import.is_some();
        let workflow_busy = import_active || import_setup_active;
        let mut start_pending_tiff_import = false;
        let mut cancel_pending_tiff_import = false;
        let mut dismiss_tiff_import_setup_error = false;
        let layout = WorkbenchLayoutSpec::default();
        let playback_started = Instant::now();
        self.enqueue_playback_command_if_due(&mut workbench_commands, ui.ctx());
        let playback_ms = duration_ms(playback_started.elapsed());

        let ui_build_started = Instant::now();
        let mut histogram_ui_ms = 0.0;
        let mut active_layer_histogram_for_ui = None;
        egui::Panel::top("top-toolbar").show_inside(ui, |ui| {
            ui.vertical(|ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.heading("Mirante4D");
                    ui.separator();
                    if ui_kit::toolbar_button(ui, "Open", !workflow_busy).clicked() {
                        self.open_native_from_dialog(ui.ctx());
                    }
                    if ui_kit::toolbar_button(ui, "Open Project", !workflow_busy).clicked() {
                        self.open_session_from_dialog(ui.ctx());
                    }
                    if ui_kit::toolbar_button(ui, "Save Project", !workflow_busy).clicked() {
                        self.save_current_project();
                    }
                    if ui_kit::toolbar_button(ui, "Import Dir", !workflow_busy)
                        .on_hover_text("Import TIFF directory")
                        .clicked()
                    {
                        self.import_tiff_directory_from_dialog(ui.ctx());
                    }
                    if ui_kit::toolbar_button(ui, "Import File", !workflow_busy)
                        .on_hover_text("Import TIFF file")
                        .clicked()
                    {
                        self.import_tiff_file_from_dialog(ui.ctx());
                    }
                    if import_active && ui_kit::toolbar_button(ui, "Cancel Import", true).clicked()
                    {
                        self.cancel_import_task();
                    }
                    ui.separator();
                    ui_kit::elided_label(ui, &self.state.dataset_name, 42);
                });
                ui.horizontal_wrapped(|ui| {
                    layout_selector(
                        ui,
                        self.state.viewer_layout.layout(),
                        &mut workbench_commands,
                    );
                    ui.separator();
                    ui_kit::muted_label(ui, "Render");
                    if let Some(command) = render_mode_selector(ui, self.state.active_render_mode) {
                        workbench_commands.push(command);
                    }
                    ui.separator();
                    ui_kit::muted_label(ui, "Camera");
                    if let Some(command) = projection_selector(ui, self.state.camera.projection) {
                        workbench_commands.push(command);
                    }
                    if ui.button("Fit Data").clicked() {
                        workbench_commands.push(WorkbenchCommand::FitData);
                    }
                    if ui.button("Reset View").clicked() {
                        workbench_commands.push(WorkbenchCommand::ResetView);
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
                        ui_kit::property_row(ui, "name", &self.state.dataset_name);
                        ui_kit::property_row(ui, "layers", self.state.layer_count.to_string());
                        ui_kit::property_row(
                            ui,
                            "timepoints",
                            self.state.timepoint_count.to_string(),
                        );
                    });
                    ui_kit::section(ui, "Status", |ui| {
                        if let Some(task) = &self.import_task {
                            ui_kit::status_badge(
                                ui,
                                StatusTone::Warning,
                                import_task_status_text(task),
                            );
                        } else if self.tiff_import_setup_task.is_some() {
                            ui_kit::status_badge(ui, StatusTone::Warning, "inspecting TIFF input");
                        } else if self.pending_tiff_import.is_some() {
                            ui_kit::status_badge(
                                ui,
                                StatusTone::Warning,
                                "review TIFF import settings",
                            );
                        } else {
                            ui_kit::status_badge(ui, StatusTone::Ready, "ready");
                        }
                        ui_kit::property_row(ui, "fidelity", composite_fidelity_label(&self.state));
                        if let Some(hover) = self.state.hovered_pixel {
                            ui_kit::property_row(ui, "hover", viewport_hover_status_label(hover));
                        }
                        if let Some(readout) = &self.state.hovered_source_readout {
                            ui_kit::property_row(ui, "readout", readout);
                        }
                        ui_kit::property_row(
                            ui,
                            "playback",
                            playback_status_label(
                                self.playback,
                                self.state.active_timepoint,
                                self.state.timepoint_count,
                            ),
                        );
                        ui_kit::property_row(
                            ui,
                            "path",
                            dataset_path_status_label(&self.state.dataset_path),
                        );
                        ui.horizontal_wrapped(|ui| {
                            show_playback_controls(
                                ui,
                                self.playback,
                                self.state.active_timepoint,
                                self.state.timepoint_count,
                                workflow_busy,
                                &mut workbench_commands,
                            );
                        });
                    });
                    ui_kit::section(ui, "Layers", |ui| {
                        for (index, layer) in self.state.layers.iter().enumerate() {
                            let selected = index == self.state.active_layer_index;
                            let detail = format!(
                                "{} {:?} t{} z{} y{} x{}",
                                layer.id,
                                layer.dtype,
                                layer.shape.t,
                                layer.shape.z,
                                layer.shape.y,
                                layer.shape.x
                            );
                            ui.horizontal(|ui| {
                                let mut visible = layer.display.visible;
                                if ui
                                    .checkbox(&mut visible, "")
                                    .on_hover_text(format!("Show {}", layer.name))
                                    .changed()
                                {
                                    workbench_commands.push(WorkbenchCommand::SetLayerVisibility {
                                        layer_index: index,
                                        visible,
                                    });
                                }
                                if ui_kit::layer_row(
                                    ui,
                                    selected,
                                    layer.display.visible,
                                    &layer.name,
                                    &detail,
                                )
                                .clicked()
                                    && !selected
                                {
                                    workbench_commands.push(WorkbenchCommand::SelectLayer(index));
                                }
                                let mut mode = if selected {
                                    self.state.active_render_mode
                                } else {
                                    layer.render_state.mode()
                                };
                                egui::ComboBox::from_id_salt(format!(
                                    "layer-render-mode-{}",
                                    layer.id
                                ))
                                .selected_text(render_mode_label(mode))
                                .width(72.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut mode, RenderMode::Mip, "MIP");
                                    ui.selectable_value(&mut mode, RenderMode::Isosurface, "ISO");
                                    ui.selectable_value(&mut mode, RenderMode::Dvr, "DVR");
                                });
                                let current_mode = if selected {
                                    self.state.active_render_mode
                                } else {
                                    layer.render_state.mode()
                                };
                                if mode != current_mode {
                                    workbench_commands.push(WorkbenchCommand::SetLayerRenderMode {
                                        layer_index: index,
                                        mode,
                                    });
                                }
                            });
                        }
                        ui_kit::property_row(ui, "active ID", &self.state.active_layer_id);
                    });
                    ui_kit::section(ui, "Channel Presets", |ui| {
                        if self.state.channel_presets.is_empty() {
                            ui_kit::status_badge(ui, StatusTone::Warning, "no channel presets");
                        } else {
                            let selected = self
                                .state
                                .selected_channel_preset_index
                                .filter(|index| *index < self.state.channel_presets.len())
                                .unwrap_or(0);
                            let selected_name = self.state.channel_presets[selected].name.clone();
                            egui::ComboBox::from_label("channel preset")
                                .selected_text(selected_name)
                                .show_ui(ui, |ui| {
                                    for (index, preset) in
                                        self.state.channel_presets.iter().enumerate()
                                    {
                                        if ui
                                            .selectable_label(index == selected, &preset.name)
                                            .clicked()
                                        {
                                            workbench_commands.push(
                                                WorkbenchCommand::ApplyChannelPreset {
                                                    preset_index: index,
                                                },
                                            );
                                        }
                                    }
                                });
                            ui.horizontal_wrapped(|ui| {
                                if ui_kit::toolbar_button(ui, "Apply", true).clicked() {
                                    workbench_commands.push(WorkbenchCommand::ApplyChannelPreset {
                                        preset_index: selected,
                                    });
                                }
                                if ui_kit::toolbar_button(ui, "Save Current", true).clicked() {
                                    workbench_commands
                                        .push(WorkbenchCommand::SaveCurrentChannelPreset);
                                }
                                if ui_kit::toolbar_button(ui, "Update", true).clicked() {
                                    workbench_commands.push(
                                        WorkbenchCommand::UpdateChannelPreset {
                                            preset_index: selected,
                                        },
                                    );
                                }
                            });
                        }
                        for warning in &self.state.channel_preset_warnings {
                            ui_kit::status_badge(ui, StatusTone::Warning, warning);
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
                            format!("{:?}", self.state.active_layer_dtype),
                        );
                        if let Some(label) = active_layer_no_data_policy_label(&self.state) {
                            ui_kit::property_row(ui, "no-data", label);
                        }
                        let layer_index = self.state.active_layer_index;
                        let mut visible = self.state.active_layer_display.visible;
                        if ui.checkbox(&mut visible, "channel visible").changed() {
                            workbench_commands.push(WorkbenchCommand::SetLayerVisibility {
                                layer_index,
                                visible,
                            });
                        }
                        let mut opacity = self.state.active_layer_display.opacity;
                        ui.horizontal(|ui| {
                            ui.label("channel opacity");
                            if ui
                                .add(egui::Slider::new(&mut opacity, 0.0..=1.0).show_value(true))
                                .changed()
                            {
                                workbench_commands.push(WorkbenchCommand::SetLayerOpacity {
                                    layer_index,
                                    opacity,
                                });
                            }
                        });
                        let mut window_low = self.state.active_layer_display.window.low;
                        let mut window_high = self.state.active_layer_display.window.high;
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
                            if low_changed || high_changed {
                                workbench_commands.push(WorkbenchCommand::SetLayerWindow {
                                    layer_index,
                                    low: window_low,
                                    high: window_high,
                                });
                            }
                        });
                        let mut color_rgba = self.state.active_layer_color.color_rgba;
                        ui.horizontal(|ui| {
                            ui.label("channel color");
                            if ui
                                .color_edit_button_rgba_unmultiplied(&mut color_rgba)
                                .changed()
                            {
                                workbench_commands.push(WorkbenchCommand::SetLayerColor {
                                    layer_index,
                                    color_rgba,
                                });
                            }
                        });
                        let mut gamma = self.state.active_layer_transfer.curve.gamma_value();
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
                            {
                                workbench_commands
                                    .push(WorkbenchCommand::SetLayerGamma { layer_index, gamma });
                            }
                        });
                        let mut invert = self.state.active_layer_transfer.invert;
                        if ui.checkbox(&mut invert, "invert LUT").changed() {
                            workbench_commands.push(WorkbenchCommand::SetLayerInvert {
                                layer_index,
                                invert,
                            });
                        }
                        egui::ComboBox::from_label("transfer preset")
                            .selected_text(transfer_preset_label_for_id(
                                &self.state.active_layer_transfer.preset,
                            ))
                            .show_ui(ui, |ui| {
                                for preset in built_in_transfer_presets() {
                                    if ui
                                        .selectable_label(
                                            self.state.active_layer_transfer.preset
                                                == built_in_transfer_preset_id(preset),
                                            built_in_transfer_preset_label(preset),
                                        )
                                        .clicked()
                                    {
                                        workbench_commands.push(
                                            WorkbenchCommand::SetLayerTransferPreset {
                                                layer_index,
                                                preset,
                                            },
                                        );
                                    }
                                }
                            });
                        let histogram =
                            if let Some(histogram) = active_layer_histogram_for_ui.clone() {
                                histogram
                            } else {
                                let histogram_started = Instant::now();
                                let histogram = active_layer_histogram_summary(&mut self.state);
                                histogram_ui_ms += duration_ms(histogram_started.elapsed());
                                active_layer_histogram_for_ui = Some(histogram.clone());
                                histogram
                            };
                        ui_kit::property_row(ui, "histogram", histogram_status_label(&histogram));
                        ui_kit::property_row(
                            ui,
                            "histogram bins",
                            histogram_bins_label(&histogram),
                        );
                        ui.horizontal_wrapped(|ui| {
                            if ui_kit::toolbar_button(
                                ui,
                                "Auto Dense",
                                histogram_can_auto_window(&histogram),
                            )
                            .clicked()
                            {
                                match auto_dense_window_from_histogram(&histogram) {
                                    Ok(window) => {
                                        workbench_commands.push(WorkbenchCommand::SetLayerWindow {
                                            layer_index,
                                            low: window.low,
                                            high: window.high,
                                        });
                                    }
                                    Err(err) => {
                                        self.state.last_render_error = Some(err.to_string());
                                    }
                                }
                            }
                            if ui_kit::toolbar_button(
                                ui,
                                "Auto Signal",
                                histogram_can_auto_window(&histogram),
                            )
                            .clicked()
                            {
                                match auto_signal_window_from_histogram(&histogram) {
                                    Ok(window) => {
                                        workbench_commands.push(WorkbenchCommand::SetLayerWindow {
                                            layer_index,
                                            low: window.low,
                                            high: window.high,
                                        });
                                    }
                                    Err(err) => {
                                        self.state.last_render_error = Some(err.to_string());
                                    }
                                }
                            }
                        });
                        ui_kit::property_row(
                            ui,
                            "shape",
                            format!(
                                "t{} z{} y{} x{}",
                                self.state.active_layer_shape.t,
                                self.state.active_layer_shape.z,
                                self.state.active_layer_shape.y,
                                self.state.active_layer_shape.x
                            ),
                        );
                        ui_kit::property_row(
                            ui,
                            "timepoint",
                            format!(
                                "{}/{}",
                                self.state.active_timepoint.0 + 1,
                                self.state.timepoint_count
                            ),
                        );
                    });
                    if let Some(task) = &self.import_task {
                        ui_kit::section(ui, "Import", |ui| {
                            ui_kit::status_badge(
                                ui,
                                StatusTone::Warning,
                                import_task_status_text(task),
                            );
                            if let Some(progress) =
                                import_progress_fraction(task.latest_event.as_ref())
                            {
                                ui.add(egui::ProgressBar::new(progress).show_percentage());
                            }
                        });
                    } else if let Some(task) = &self.tiff_import_setup_task {
                        ui_kit::section(ui, "TIFF Import", |ui| {
                            ui_kit::status_badge(ui, StatusTone::Warning, "inspecting input");
                            ui_kit::property_row(ui, "source", task.source.path().display());
                            ui_kit::property_row(ui, "output", task.output_parent.display());
                        });
                    }
                    if let Some(pending_import) = &mut self.pending_tiff_import {
                        ui_kit::section(ui, "TIFF Import", |ui| {
                            ui_kit::property_row(
                                ui,
                                "source",
                                pending_import.options.source.path().display().to_string(),
                            );
                            ui_kit::property_row(
                                ui,
                                "output",
                                pending_import.options.output_package.display().to_string(),
                            );
                            ui_kit::property_row(
                                ui,
                                "files",
                                format!(
                                    "{} file(s), {} channel(s), {} timepoint(s)",
                                    pending_import.inspection.file_count,
                                    pending_import.inspection.channel_count,
                                    pending_import.inspection.timepoint_count
                                ),
                            );
                            ui_kit::property_row(
                                ui,
                                "source profile",
                                tiff_source_profile_label(pending_import.inspection.source_profile),
                            );
                            ui_kit::property_row(
                                ui,
                                "stack",
                                format!(
                                    "z{} y{} x{}",
                                    pending_import.inspection.shape.z,
                                    pending_import.inspection.shape.y,
                                    pending_import.inspection.shape.x
                                ),
                            );
                            ui_kit::property_row(
                                ui,
                                "source dtype",
                                format!("{:?}", pending_import.inspection.source_dtype),
                            );
                            show_tiff_no_data_controls(ui, pending_import);
                            ui_kit::property_row(
                                ui,
                                "spacing metadata",
                                tiff_voxel_spacing_metadata_label(&pending_import.inspection),
                            );
                            ui_kit::property_row(
                                ui,
                                "storage estimate",
                                tiff_import_storage_estimate_label(&pending_import.inspection),
                            );
                            ui.add(
                                egui::TextEdit::singleline(
                                    &mut pending_import.options.dataset_name,
                                )
                                .desired_width(180.0),
                            );
                            ui.horizontal(|ui| {
                                ui.label("voxel um");
                                ui.add(
                                    egui::DragValue::new(
                                        &mut pending_import.options.voxel_spacing_um[0],
                                    )
                                    .speed(0.01)
                                    .prefix("x "),
                                );
                                ui.add(
                                    egui::DragValue::new(
                                        &mut pending_import.options.voxel_spacing_um[1],
                                    )
                                    .speed(0.01)
                                    .prefix("y "),
                                );
                                ui.add(
                                    egui::DragValue::new(
                                        &mut pending_import.options.voxel_spacing_um[2],
                                    )
                                    .speed(0.01)
                                    .prefix("z "),
                                );
                            });
                            show_tiff_channel_metadata_controls(
                                ui,
                                &mut pending_import.options,
                                &pending_import.inspection,
                                140.0,
                            );
                            show_tiff_grouping_controls(ui, pending_import);
                            ui.checkbox(
                                &mut pending_import.voxel_spacing_confirmed,
                                "voxel spacing reviewed",
                            );
                            ui.horizontal(|ui| {
                                if ui_kit::toolbar_button(
                                    ui,
                                    "Run Import",
                                    pending_tiff_import_ready_to_start(pending_import),
                                )
                                .clicked()
                                {
                                    start_pending_tiff_import = true;
                                }
                                if ui_kit::toolbar_button(ui, "Cancel Setup", true).clicked() {
                                    cancel_pending_tiff_import = true;
                                }
                            });
                        });
                    }
                    ui_kit::section(ui, "Frame", |ui| {
                        show_frame_fidelity_property_rows(ui, &self.state.frame_fidelity);
                        if visible_channel_fidelity_is_mixed(&self.state.channel_fidelity) {
                            ui_kit::status_badge(ui, StatusTone::Warning, "mixed channel fidelity");
                        }
                        ui_kit::property_row(
                            ui,
                            "pixels",
                            self.state.diagnostics.output_pixels.to_string(),
                        );
                        if matches!(self.state.render_backend, RenderBackend::GpuResidentBricks) {
                            ui_kit::property_row(ui, "nonzero", "unavailable");
                            ui_kit::property_row(ui, "max", "unavailable");
                            ui_kit::property_row(ui, "mean", "unavailable");
                        } else {
                            ui_kit::property_row(
                                ui,
                                "nonzero",
                                self.state.diagnostics.nonzero_pixels.to_string(),
                            );
                            ui_kit::property_row(
                                ui,
                                "max",
                                self.state.diagnostics.max_value.to_string(),
                            );
                            ui_kit::property_row(
                                ui,
                                "mean",
                                format!("{:.2}", self.state.active_intensity_summary.mean),
                            );
                        }
                        for channel in &self.state.channel_fidelity {
                            ui_kit::property_row(
                                ui,
                                format!("{} fidelity", channel.layer_id),
                                channel_fidelity_label(channel),
                            );
                        }
                    });
                    ui_kit::section(ui, "Viewer Tools", |ui| {
                        let mut active_tool = self.state.viewer_tools.active_tool;
                        ui.horizontal_wrapped(|ui| {
                            ui.selectable_value(&mut active_tool, ViewerTool::Navigate, "Navigate");
                            ui.selectable_value(&mut active_tool, ViewerTool::Inspect, "Inspect");
                            ui.selectable_value(
                                &mut active_tool,
                                ViewerTool::Crosshair,
                                "Crosshair",
                            );
                            ui.selectable_value(&mut active_tool, ViewerTool::RoiBox, "ROI");
                            ui.selectable_value(
                                &mut active_tool,
                                ViewerTool::MeasureDistance,
                                "Measure",
                            );
                        });
                        if active_tool != self.state.viewer_tools.active_tool {
                            self.state.viewer_tools.set_active_tool(active_tool);
                        }
                        if let Some(crosshair) = &self.state.viewer_tools.crosshair
                            && let Some(screen) = crosshair.screen_position
                        {
                            ui_kit::property_row(
                                ui,
                                "crosshair",
                                format!("x{:.0} y{:.0} {:?}", screen.x, screen.y, crosshair.kind),
                            );
                        }
                        if let Some(selection) = &self.state.viewer_tools.selection {
                            ui_kit::property_row(ui, "selection", format!("{selection:?}"));
                        }
                    });
                    ui_kit::section(ui, "Scene Artifacts", |ui| {
                        rerender_requested |= show_scene_artifacts_editor(ui, &mut self.state);
                    });
                    ui_kit::section(ui, "Analysis", |ui| {
                        let analysis_running = self.analysis_task.is_some();
                        normalize_analysis_selection(&mut self.state);
                        ui.horizontal_wrapped(|ui| {
                            if ui_kit::toolbar_button(ui, "Analyze Time", !analysis_running)
                                .clicked()
                            {
                                self.start_analysis_task(AnalysisTaskKind::FullTimeSeries);
                            }
                            if ui_kit::toolbar_button(ui, "Analyze ROIs", !analysis_running)
                                .clicked()
                            {
                                self.start_analysis_task(AnalysisTaskKind::RoiIntensity);
                            }
                            if analysis_running
                                && ui_kit::toolbar_button(ui, "Cancel Analysis", true).clicked()
                            {
                                self.cancel_analysis_task();
                            }
                            if ui_kit::toolbar_button(ui, "Workspace", true).clicked() {
                                self.analysis_workspace_open = true;
                            }
                            if ui_kit::toolbar_button(
                                ui,
                                "Export CSV",
                                !analysis_running
                                    && self.state.selected_analysis_table_index.is_some(),
                            )
                            .clicked()
                            {
                                match export_selected_analysis_table(&mut self.state) {
                                    Ok(()) => {}
                                    Err(err) => {
                                        self.state.last_render_error = Some(err.to_string())
                                    }
                                }
                            }
                        });
                        if let Some(task) = &self.analysis_task {
                            ui_kit::status_badge(
                                ui,
                                StatusTone::Warning,
                                analysis_task_status_text(task),
                            );
                            if let Some(progress) =
                                analysis_progress_fraction(task.latest_progress.as_ref())
                            {
                                ui.add(egui::ProgressBar::new(progress).show_percentage());
                            }
                        }
                        show_analysis_workspace(ui, &mut self.state);
                    });
                    ui_kit::section(ui, "Settings", |ui| self.show_settings_body(ui));
                    egui::CollapsingHeader::new("Runtime Diagnostics")
                        .default_open(false)
                        .show(ui, |ui| self.show_runtime_diagnostics_body(ui));
                    ui_kit::section(ui, "Render Settings", |ui| {
                        let mut sampling_policy = self.state.render_sampling_policy;
                        egui::ComboBox::from_label("sampling")
                            .selected_text(render_sampling_policy_label(sampling_policy))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut sampling_policy,
                                    RenderSamplingPolicy::SmoothLinear,
                                    render_sampling_policy_label(
                                        RenderSamplingPolicy::SmoothLinear,
                                    ),
                                );
                                ui.selectable_value(
                                    &mut sampling_policy,
                                    RenderSamplingPolicy::VoxelExact,
                                    render_sampling_policy_label(RenderSamplingPolicy::VoxelExact),
                                );
                            });
                        if sampling_policy != self.state.render_sampling_policy {
                            workbench_commands
                                .push(WorkbenchCommand::SetRenderSamplingPolicy(sampling_policy));
                        }
                        ui_kit::property_row(
                            ui,
                            "sampling policy",
                            render_sampling_policy_label(self.state.render_sampling_policy),
                        );
                        match self.state.active_render_mode {
                            RenderMode::Mip => {
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
                            RenderMode::Isosurface => {
                                let mut display_level = self.state.iso_display_level;
                                if ui
                                    .add(
                                        egui::Slider::new(&mut display_level, 0.0..=1.0)
                                            .text("ISO display level"),
                                    )
                                    .changed()
                                {
                                    workbench_commands.push(WorkbenchCommand::SetIsoDisplayLevel {
                                        display_level,
                                    });
                                }
                                let mut iso_shading_policy = self.state.render_iso_shading_policy;
                                egui::ComboBox::from_label("ISO shading")
                                    .selected_text(iso_shading_policy_label(iso_shading_policy))
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut iso_shading_policy,
                                            RenderIsoShadingPolicy::GradientLighting,
                                            "Gradient lighting",
                                        );
                                        ui.selectable_value(
                                            &mut iso_shading_policy,
                                            RenderIsoShadingPolicy::Flat,
                                            "Flat threshold hit",
                                        );
                                    });
                                if iso_shading_policy != self.state.render_iso_shading_policy {
                                    workbench_commands.push(
                                        WorkbenchCommand::SetRenderIsoShadingPolicy(
                                            iso_shading_policy,
                                        ),
                                    );
                                }
                                show_iso_light_controls(
                                    ui,
                                    self.state.iso_light_state,
                                    &mut workbench_commands,
                                );
                                ui_kit::property_row(
                                    ui,
                                    "ISO surface rule",
                                    "first display-level crossing along the camera ray",
                                );
                                ui_kit::property_row(ui, "ISO pick policy", "display-level hit");
                            }
                            RenderMode::Dvr => {
                                let mut density_scale = self.state.dvr_density_scale;
                                if ui
                                    .add(
                                        egui::Slider::new(
                                            &mut density_scale,
                                            DVR_DENSITY_SCALE_MIN..=DVR_DENSITY_SCALE_MAX,
                                        )
                                        .text("DVR density scale"),
                                    )
                                    .changed()
                                {
                                    workbench_commands.push(WorkbenchCommand::SetDvrDensityScale {
                                        density_scale,
                                    });
                                }
                                let layer_index = self.state.active_layer_index;
                                let mut opacity_low =
                                    self.state.active_dvr_opacity_transfer.window.low;
                                let mut opacity_high =
                                    self.state.active_dvr_opacity_transfer.window.high;
                                ui.horizontal(|ui| {
                                    ui.label("opacity low");
                                    if ui.add(egui::DragValue::new(&mut opacity_low)).changed() {
                                        workbench_commands.push(
                                            WorkbenchCommand::SetLayerDvrOpacityWindow {
                                                layer_index,
                                                low: opacity_low,
                                                high: opacity_high,
                                            },
                                        );
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("opacity high");
                                    if ui.add(egui::DragValue::new(&mut opacity_high)).changed() {
                                        workbench_commands.push(
                                            WorkbenchCommand::SetLayerDvrOpacityWindow {
                                                layer_index,
                                                low: opacity_low,
                                                high: opacity_high,
                                            },
                                        );
                                    }
                                });
                                let mut opacity_gamma =
                                    self.state.active_dvr_opacity_transfer.curve.gamma_value();
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
                                    {
                                        workbench_commands.push(
                                            WorkbenchCommand::SetLayerDvrOpacityGamma {
                                                layer_index,
                                                gamma: opacity_gamma,
                                            },
                                        );
                                    }
                                });
                                let histogram = if let Some(histogram) =
                                    active_layer_histogram_for_ui.clone()
                                {
                                    histogram
                                } else {
                                    let histogram_started = Instant::now();
                                    let histogram = active_layer_histogram_summary(&mut self.state);
                                    histogram_ui_ms += duration_ms(histogram_started.elapsed());
                                    active_layer_histogram_for_ui = Some(histogram.clone());
                                    histogram
                                };
                                ui.horizontal_wrapped(|ui| {
                                    if ui_kit::toolbar_button(
                                        ui,
                                        "Auto Opacity",
                                        histogram_can_auto_window(&histogram),
                                    )
                                    .clicked()
                                    {
                                        workbench_commands.push(
                                            WorkbenchCommand::AutoLayerDvrOpacity { layer_index },
                                        );
                                    }
                                    if ui_kit::toolbar_button(ui, "Reset Opacity", true).clicked() {
                                        workbench_commands.push(
                                            WorkbenchCommand::ResetLayerDvrOpacity { layer_index },
                                        );
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
                            format!("{:?}", self.state.camera.projection),
                        );
                        let camera_forward = self.state.camera.axes().forward;
                        ui_kit::property_row(
                            ui,
                            "forward",
                            format!(
                                "{:.2}, {:.2}, {:.2}",
                                camera_forward.x, camera_forward.y, camera_forward.z
                            ),
                        );
                        ui_kit::property_row(
                            ui,
                            "scale",
                            format!(
                                "{:.4} world/pt",
                                self.state.camera.world_per_screen_point_at_target()
                            ),
                        );
                    });
                    ui_kit::section(ui, "Messages", |ui| {
                        if let Some(error) = &self.state.last_render_error {
                            ui_kit::status_badge(ui, StatusTone::Error, error);
                        }
                        if let Some(message) = &self.state.last_workflow_message {
                            ui_kit::property_row(ui, "workflow", message);
                        }
                        if let Some(hover) = self.state.hovered_pixel {
                            ui_kit::property_row(
                                ui,
                                "hover",
                                format!("x{} y{} intensity {}", hover.x, hover.y, hover.intensity),
                            );
                        }
                    });
                });
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.show_viewer_layout(
                ui,
                &mut workbench_commands,
                &mut rerender_requested,
                &mut texture_refresh_requested,
            );
        });

        show_analysis_workspace_window(
            ui.ctx(),
            &mut self.analysis_workspace_open,
            &mut self.state,
        );

        self.show_tiff_import_setup_window(
            ui.ctx(),
            &mut start_pending_tiff_import,
            &mut cancel_pending_tiff_import,
            &mut dismiss_tiff_import_setup_error,
        );
        self.show_dirty_project_close_prompt(ui.ctx());
        let ui_build_ms = duration_ms(ui_build_started.elapsed());

        let command_apply_started = Instant::now();
        for command in workbench_commands {
            let outcome = self.apply_workbench_command(command, ui.ctx());
            rerender_requested |= outcome.rerender_requested;
            texture_refresh_requested |= outcome.texture_refresh_requested;
        }
        let command_apply_ms = duration_ms(command_apply_started.elapsed());

        let display_refresh_trigger_started = Instant::now();
        rerender_requested |= take_lod_replan_pending(&mut self.state);
        if rerender_requested {
            self.refresh_frame(ui.ctx());
        } else if texture_refresh_requested {
            self.refresh_texture_only(ui.ctx());
        }
        let display_refresh_trigger_ms = duration_ms(display_refresh_trigger_started.elapsed());

        let import_action_started = Instant::now();
        if dismiss_tiff_import_setup_error {
            self.tiff_import_setup_error = None;
            self.state.last_render_error = None;
        }
        if cancel_pending_tiff_import {
            self.cancel_pending_tiff_import();
        }
        if start_pending_tiff_import {
            self.start_pending_tiff_import();
        }
        let import_action_ms = duration_ms(import_action_started.elapsed());

        let brick_result_drain_started = Instant::now();
        self.drain_brick_results(ui.ctx());
        let brick_result_drain_ms = duration_ms(brick_result_drain_started.elapsed());

        let background_repaint_started = Instant::now();
        if self.background_work_active() {
            request_background_work_repaint_after(ui.ctx());
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
                command_apply_ms,
                display_refresh_trigger_ms,
                import_action_ms,
                brick_result_drain_ms,
                background_repaint_request_ms,
            },
        );
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
    use super::*;

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
}
