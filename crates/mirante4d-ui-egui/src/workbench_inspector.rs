use eframe::egui;
use mirante4d_application::{
    AnalysisWorkspaceSnapshot, ApplicationCommand, ApplicationSnapshot, DEFAULT_DVR_OPACITY_GAMMA,
    DisplayWindow, DvrOpacityTransfer, FrameFidelityStatus, IsoLightState, IsoShadingPolicy,
    LayerHistogramSummary, LayerTransfer, LayerViewState, Opacity, RenderExtent, RenderMode,
    RenderState, RgbColor, SamplingPolicy, TRANSFER_GAMMA_MAX, TRANSFER_GAMMA_MIN, ToolKind,
    TransferCurve, auto_dense_window_from_histogram, auto_dvr_opacity_transfer_from_histogram,
    auto_signal_window_from_histogram, histogram_can_auto_window,
    import_workflow::ImportWorkflowSnapshot,
};

use crate::{
    EguiUiState, RuntimeDiagnosticsView, SettingsUiView, StatusTone, WorkbenchAnalysisKind,
    WorkbenchLayoutSpec, WorkbenchUiAction, WorkbenchUiOutput, application_problem_message,
    iso_shading_policy_label, panel_scroll, property_row, render_sampling_policy_label, section,
    show_analysis_workspace, show_frame_fidelity_property_rows, show_runtime_diagnostics_body,
    show_settings_body, status_badge, toolbar_button,
};

#[derive(Debug, Clone, Copy)]
pub struct AnalysisControlsView<'a> {
    pub start_unavailable_reason: Option<&'a str>,
    pub active: bool,
    pub roi_origin: [u64; 3],
    pub roi_shape: [u64; 3],
    pub workspace: &'a AnalysisWorkspaceSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraInspectorView {
    pub forward: Option<[f64; 3]>,
    pub world_per_screen_point: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub struct InspectorWorkbenchView<'a> {
    pub application: &'a ApplicationSnapshot,
    pub histogram: &'a LayerHistogramSummary,
    pub frame_fidelity: &'a FrameFidelityStatus,
    pub render_viewport: RenderExtent,
    pub dvr_density_scale_range: [f64; 2],
    pub no_data_policy_label: Option<&'a str>,
    pub analysis: AnalysisControlsView<'a>,
    pub settings: &'a SettingsUiView,
    pub runtime_diagnostics: &'a RuntimeDiagnosticsView,
    pub camera: CameraInspectorView,
    pub messages: &'a [String],
}

pub(crate) fn show_workbench_inspector(
    ui: &mut egui::Ui,
    view: InspectorWorkbenchView<'_>,
    state: &mut EguiUiState,
    layout: WorkbenchLayoutSpec,
    output: &mut WorkbenchUiOutput,
) {
    egui::Panel::right("inspector-panel")
        .resizable(true)
        .default_size(layout.right_panel_width)
        .size_range(layout.right_width_range())
        .show_inside(ui, |ui| {
            panel_scroll(ui, "inspector-panel-scroll", |ui| {
                section(ui, "Inspector", |ui| {
                    show_channel_inspector(
                        ui,
                        view.application,
                        view.histogram,
                        view.no_data_policy_label,
                        output,
                    );
                });
                section(ui, "Frame", |ui| {
                    show_frame_inspector(ui, view.frame_fidelity, view.render_viewport);
                });
                section(ui, "Viewer Tools", |ui| {
                    show_viewer_tools(ui, view.application, state, output);
                });
                section(ui, "Analysis", |ui| {
                    show_analysis_controls(ui, view.application, view.analysis, state, output);
                });
                section(ui, "Settings", |ui| {
                    show_settings_body(
                        ui,
                        &mut state.settings_runtime_draft,
                        view.settings,
                        &mut output.actions,
                    );
                });
                egui::CollapsingHeader::new("Runtime Diagnostics")
                    .default_open(false)
                    .show(ui, |ui| {
                        show_runtime_diagnostics_body(
                            view.runtime_diagnostics,
                            ui,
                            &mut output.actions,
                        );
                    });
                section(ui, "Render Settings", |ui| {
                    show_render_settings(
                        ui,
                        view.application,
                        view.histogram,
                        view.dvr_density_scale_range,
                        output,
                    );
                });
                section(ui, "Camera", |ui| {
                    show_camera_inspector(ui, view.application, view.camera);
                });
                section(ui, "Messages", |ui| {
                    show_messages(ui, view.application, view.messages, state);
                });
            });
        });
}

fn show_channel_inspector(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    histogram: &LayerHistogramSummary,
    no_data_policy_label: Option<&str>,
    output: &mut WorkbenchUiOutput,
) {
    let view = snapshot.view();
    let active_layer = active_layer(snapshot);
    let active_catalog_layer = snapshot
        .catalog()
        .layer(view.active_layer())
        .expect("application view closes over the dataset catalog");

    property_row(ui, "dtype", format!("{:?}", active_catalog_layer.dtype()));
    if let Some(label) = no_data_policy_label {
        property_row(ui, "no-data", label);
    }

    let mut visible = active_layer.visible();
    if ui.checkbox(&mut visible, "channel visible").changed() {
        output.application_commands.push(layer_view_command(
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
            output.application_commands.push(layer_view_command(
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
            output.application_commands.push(layer_view_command(
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
            && let Ok(color) = RgbColor::new([color_rgba[0], color_rgba[1], color_rgba[2]])
        {
            let current = active_layer.transfer();
            let transfer = LayerTransfer::new(
                current.window(),
                color,
                current.opacity(),
                current.curve(),
                current.invert(),
            );
            output.application_commands.push(layer_view_command(
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
                egui::Slider::new(&mut gamma, TRANSFER_GAMMA_MIN..=TRANSFER_GAMMA_MAX)
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
            output.application_commands.push(layer_view_command(
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
        output.application_commands.push(layer_view_command(
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
                    output.application_commands.push(layer_view_command(
                        active_layer,
                        active_layer.visible(),
                        transfer,
                        *active_layer.render_state(),
                    ));
                }
            }
        });

    property_row(ui, "histogram", histogram_status_label(histogram));
    property_row(ui, "histogram bins", histogram_bins_label(histogram));
    ui.horizontal_wrapped(|ui| {
        if toolbar_button(ui, "Auto Dense", histogram_can_auto_window(histogram)).clicked() {
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
                    output.application_commands.push(layer_view_command(
                        active_layer,
                        active_layer.visible(),
                        transfer,
                        *active_layer.render_state(),
                    ));
                }
                Err(error) => tracing::warn!(%error, "auto dense window rejected"),
            }
        }
        if toolbar_button(ui, "Auto Signal", histogram_can_auto_window(histogram)).clicked() {
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
                    output.application_commands.push(layer_view_command(
                        active_layer,
                        active_layer.visible(),
                        transfer,
                        *active_layer.render_state(),
                    ));
                }
                Err(error) => tracing::warn!(%error, "auto signal window rejected"),
            }
        }
    });

    property_row(
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
    property_row(
        ui,
        "timepoint",
        format!(
            "{}/{}",
            view.timepoint().get() + 1,
            snapshot.timepoint_count()
        ),
    );
}

fn show_frame_inspector(
    ui: &mut egui::Ui,
    fidelity: &FrameFidelityStatus,
    render_viewport: RenderExtent,
) {
    show_frame_fidelity_property_rows(ui, fidelity);
    property_row(
        ui,
        "pixels",
        render_viewport
            .width_pixels()
            .saturating_mul(render_viewport.height_pixels())
            .to_string(),
    );
    property_row(ui, "nonzero", "unavailable");
    property_row(ui, "max", "unavailable");
    property_row(ui, "mean", "unavailable");
}

pub fn histogram_status_label(histogram: &LayerHistogramSummary) -> String {
    match &histogram.status {
        mirante4d_application::HistogramStatus::Exact => format!(
            "exact {}, {:.3}..{:.3}",
            histogram.sample_count, histogram.min_value, histogram.max_value
        ),
        mirante4d_application::HistogramStatus::Sampled { source } => format!(
            "sampled {}, {:.3}..{:.3} ({source})",
            histogram.sample_count, histogram.min_value, histogram.max_value
        ),
        mirante4d_application::HistogramStatus::Pending { reason } => {
            format!("pending: {reason}")
        }
        mirante4d_application::HistogramStatus::Unavailable { reason } => {
            format!("unavailable: {reason}")
        }
    }
}

pub fn histogram_bins_label(histogram: &LayerHistogramSummary) -> String {
    if histogram.bins.is_empty() {
        return "no bins".to_owned();
    }
    let max_bin = histogram.bins.iter().copied().max().unwrap_or(0).max(1);
    format!("{} bins, peak count {}", histogram.bin_count, max_bin)
}

fn show_viewer_tools(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    state: &EguiUiState,
    output: &mut WorkbenchUiOutput,
) {
    let mut active_tool = snapshot.transient().active_tool();
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(&mut active_tool, ToolKind::Navigate, "Navigate");
        ui.selectable_value(&mut active_tool, ToolKind::Inspect, "Inspect");
        ui.selectable_value(&mut active_tool, ToolKind::Crosshair, "Crosshair");
    });
    if active_tool != snapshot.transient().active_tool() {
        output
            .application_commands
            .push(ApplicationCommand::SetActiveTool(active_tool));
    }
    if let Some(crosshair) = &state.viewer_tools.crosshair
        && let Some(screen) = crosshair.screen_position
    {
        property_row(
            ui,
            "crosshair",
            format!("x{:.0} y{:.0} {:?}", screen.x, screen.y, crosshair.kind),
        );
    }
}

fn show_analysis_controls(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    view: AnalysisControlsView<'_>,
    state: &mut EguiUiState,
    output: &mut WorkbenchUiOutput,
) {
    let can_start = view.start_unavailable_reason.is_none();
    let mut start_time = false;
    let mut start_box = false;
    ui.horizontal_wrapped(|ui| {
        start_time = toolbar_button(ui, "Analyze Time", can_start).clicked();
        start_box = toolbar_button(ui, "Analyze Box", can_start).clicked();
        if toolbar_button(ui, "Cancel", view.active).clicked() {
            output.actions.push(WorkbenchUiAction::CancelAnalysis);
        }
        if toolbar_button(ui, "Workspace", true).clicked() {
            state.analysis_workspace_open = true;
        }
        if toolbar_button(
            ui,
            "Copy CSV",
            snapshot.transient().selected_analysis_table().is_some(),
        )
        .clicked()
        {
            output
                .actions
                .push(WorkbenchUiAction::CopySelectedAnalysisCsv);
        }
    });

    let layer_shape = snapshot
        .catalog()
        .layer(snapshot.view().active_layer())
        .expect("the active layer closes over the catalog")
        .shape()
        .spatial()
        .dimensions();
    let mut roi_origin = view.roi_origin;
    let mut roi_shape = view.roi_shape;
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
        roi_origin[axis] = roi_origin[axis].min(layer_shape[axis].saturating_sub(1));
        roi_shape[axis] =
            roi_shape[axis].clamp(1, layer_shape[axis].saturating_sub(roi_origin[axis]));
    }
    if roi_origin != view.roi_origin || roi_shape != view.roi_shape {
        output.actions.push(WorkbenchUiAction::SetAnalysisRoi {
            origin: roi_origin,
            shape: roi_shape,
        });
    }
    if start_time {
        output.actions.push(WorkbenchUiAction::StartAnalysis(
            WorkbenchAnalysisKind::FullTimeTrace,
        ));
    }
    if start_box {
        output.actions.push(WorkbenchUiAction::StartAnalysis(
            WorkbenchAnalysisKind::CurrentTimepointBox,
        ));
    }
    if let Some(reason) = view.start_unavailable_reason {
        status_badge(ui, StatusTone::Warning, reason);
    }
    output
        .application_commands
        .extend(show_analysis_workspace(ui, view.workspace, state));
}

fn show_render_settings(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    histogram: &LayerHistogramSummary,
    dvr_density_scale_range: [f64; 2],
    output: &mut WorkbenchUiOutput,
) {
    let active_layer = active_layer(snapshot);
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
            Ok(render_state) => output.application_commands.push(layer_view_command(
                active_layer,
                active_layer.visible(),
                active_layer.transfer().clone(),
                render_state,
            )),
            Err(error) => tracing::warn!(%error, "sampling change rejected"),
        }
    }
    property_row(
        ui,
        "sampling policy",
        render_sampling_policy_label(current_render.sampling_policy()),
    );

    match current_render.mode() {
        RenderMode::Mip => {
            property_row(
                ui,
                "MIP projection",
                "maximum intensity along the camera ray",
            );
            property_row(ui, "MIP display mapping", "active channel window and color");
        }
        RenderMode::Isosurface => {
            show_iso_render_settings(
                ui,
                active_layer,
                current_render,
                *snapshot.view().iso_light(),
                output,
            );
        }
        RenderMode::Dvr => {
            show_dvr_render_settings(
                ui,
                active_layer,
                current_render,
                histogram,
                dvr_density_scale_range,
                output,
            );
        }
    }
}

fn show_iso_render_settings(
    ui: &mut egui::Ui,
    active_layer: &LayerViewState,
    current_render: RenderState,
    light_state: IsoLightState,
    output: &mut WorkbenchUiOutput,
) {
    let parameters = current_render
        .iso_parameters()
        .expect("ISO mode has ISO parameters");
    let mut display_level = parameters.display_level();
    if ui
        .add(egui::Slider::new(&mut display_level, 0.0..=1.0).text("ISO display level"))
        .changed()
        && let Ok(render_state) = RenderState::iso(
            current_render.sampling_policy(),
            parameters.shading_policy(),
            display_level,
        )
    {
        output.application_commands.push(layer_view_command(
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
        && let Ok(render_state) = RenderState::iso(
            current_render.sampling_policy(),
            iso_shading_policy,
            parameters.display_level(),
        )
    {
        output.application_commands.push(layer_view_command(
            active_layer,
            active_layer.visible(),
            active_layer.transfer().clone(),
            render_state,
        ));
    }

    show_iso_light_controls(ui, light_state, output);
    property_row(
        ui,
        "ISO surface rule",
        "first display-level crossing along the camera ray",
    );
    property_row(ui, "ISO pick policy", "display-level hit");
}

fn show_dvr_render_settings(
    ui: &mut egui::Ui,
    active_layer: &LayerViewState,
    current_render: RenderState,
    histogram: &LayerHistogramSummary,
    dvr_density_scale_range: [f64; 2],
    output: &mut WorkbenchUiOutput,
) {
    let parameters = current_render
        .dvr_parameters()
        .expect("DVR mode has DVR parameters");
    let mut density_scale = parameters.density_scale();
    if ui
        .add(
            egui::Slider::new(
                &mut density_scale,
                dvr_density_scale_range[0]..=dvr_density_scale_range[1],
            )
            .text("DVR density scale"),
        )
        .changed()
        && let Ok(render_state) = RenderState::dvr(
            current_render.sampling_policy(),
            parameters.opacity_transfer(),
            density_scale,
        )
    {
        output.application_commands.push(layer_view_command(
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
    if opacity_window_changed && let Ok(window) = DisplayWindow::new(opacity_low, opacity_high) {
        let opacity_transfer = DvrOpacityTransfer::new(window, opacity.curve());
        if let Ok(render_state) = RenderState::dvr(
            current_render.sampling_policy(),
            opacity_transfer,
            parameters.density_scale(),
        ) {
            output.application_commands.push(layer_view_command(
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
                egui::Slider::new(&mut opacity_gamma, TRANSFER_GAMMA_MIN..=TRANSFER_GAMMA_MAX)
                    .show_value(true),
            )
            .changed()
            && let Ok(curve) = TransferCurve::gamma(opacity_gamma)
            && let Ok(render_state) = RenderState::dvr(
                current_render.sampling_policy(),
                DvrOpacityTransfer::new(opacity.window(), curve),
                parameters.density_scale(),
            )
        {
            output.application_commands.push(layer_view_command(
                active_layer,
                active_layer.visible(),
                active_layer.transfer().clone(),
                render_state,
            ));
        }
    });

    ui.horizontal_wrapped(|ui| {
        if toolbar_button(ui, "Auto Opacity", histogram_can_auto_window(histogram)).clicked() {
            match auto_dvr_opacity_transfer_from_histogram(histogram) {
                Ok(transfer) => {
                    let opacity_transfer =
                        DvrOpacityTransfer::new(transfer.window(), transfer.curve());
                    if let Ok(render_state) = RenderState::dvr(
                        current_render.sampling_policy(),
                        opacity_transfer,
                        parameters.density_scale(),
                    ) {
                        output.application_commands.push(layer_view_command(
                            active_layer,
                            active_layer.visible(),
                            active_layer.transfer().clone(),
                            render_state,
                        ));
                    }
                }
                Err(error) => tracing::warn!(%error, "auto DVR opacity rejected"),
            }
        }
        if toolbar_button(ui, "Reset Opacity", true).clicked() {
            let opacity_transfer = DvrOpacityTransfer::new(
                active_layer.transfer().window(),
                TransferCurve::gamma(DEFAULT_DVR_OPACITY_GAMMA)
                    .expect("default DVR opacity gamma is valid"),
            );
            if let Ok(render_state) = RenderState::dvr(
                current_render.sampling_policy(),
                opacity_transfer,
                parameters.density_scale(),
            ) {
                output.application_commands.push(layer_view_command(
                    active_layer,
                    active_layer.visible(),
                    active_layer.transfer().clone(),
                    render_state,
                ));
            }
        }
    });
    property_row(
        ui,
        "DVR opacity transfer",
        "source window plus independent opacity gamma",
    );
    property_row(
        ui,
        "DVR termination",
        "front-to-back alpha, stops near opaque",
    );
}

fn show_iso_light_controls(
    ui: &mut egui::Ui,
    light_state: IsoLightState,
    output: &mut WorkbenchUiOutput,
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
            output
                .application_commands
                .push(ApplicationCommand::SetIsoLight(next));
        }
        if ui.button("Reset").clicked() {
            output
                .application_commands
                .push(ApplicationCommand::SetIsoLight(
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
            output
                .application_commands
                .push(ApplicationCommand::SetIsoLight(next));
        }
    }
}

fn show_camera_inspector(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    camera: CameraInspectorView,
) {
    property_row(
        ui,
        "projection",
        format!("{:?}", snapshot.view().camera().projection()),
    );
    if let Some(forward) = camera.forward {
        property_row(
            ui,
            "forward",
            format!("{:.2}, {:.2}, {:.2}", forward[0], forward[1], forward[2]),
        );
    }
    if let Some(scale) = camera.world_per_screen_point {
        property_row(ui, "scale", format!("{scale:.4} world/pt"));
    }
}

fn show_messages(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    messages: &[String],
    state: &EguiUiState,
) {
    if let Some(message) = application_problem_message(snapshot.latest_problem()) {
        status_badge(ui, StatusTone::Error, message);
    }
    if let ImportWorkflowSnapshot::Failed(failure) = snapshot.import_workflow() {
        status_badge(ui, StatusTone::Error, &failure.message);
    }
    for message in messages {
        status_badge(ui, StatusTone::Error, message);
    }
    if let Some(hover) = state.hovered_pixel {
        property_row(
            ui,
            "hover",
            format!("x{} y{} intensity {}", hover.x, hover.y, hover.intensity),
        );
    }
}

fn active_layer(snapshot: &ApplicationSnapshot) -> &LayerViewState {
    let view = snapshot.view();
    view.layer(view.active_layer())
        .expect("application view contains its active layer")
}

fn layer_view_command(
    layer: &LayerViewState,
    visible: bool,
    transfer: LayerTransfer,
    render_state: RenderState,
) -> ApplicationCommand {
    ApplicationCommand::SetLayerView(LayerViewState::new(
        layer.layer_key(),
        visible,
        transfer,
        render_state,
    ))
}

fn render_state_with_sampling(
    current: RenderState,
    sampling: SamplingPolicy,
) -> Result<RenderState, String> {
    match current.mode() {
        RenderMode::Mip => Ok(RenderState::mip(sampling)),
        RenderMode::Isosurface => {
            let parameters = current
                .iso_parameters()
                .expect("ISO mode has ISO parameters");
            RenderState::iso(
                sampling,
                parameters.shading_policy(),
                parameters.display_level(),
            )
            .map_err(|error| error.to_string())
        }
        RenderMode::Dvr => {
            let parameters = current
                .dvr_parameters()
                .expect("DVR mode has DVR parameters");
            RenderState::dvr(
                sampling,
                parameters.opacity_transfer(),
                parameters.density_scale(),
            )
            .map_err(|error| error.to_string())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuiltInTransferPreset {
    Linear,
    BrightGamma,
    HighContrast,
}

#[cfg(test)]
fn built_in_transfer_preset_id(preset: BuiltInTransferPreset) -> &'static str {
    match preset {
        BuiltInTransferPreset::Linear => "linear",
        BuiltInTransferPreset::BrightGamma => "bright_gamma",
        BuiltInTransferPreset::HighContrast => "high_contrast",
    }
}

fn built_in_transfer_preset_curve(preset: BuiltInTransferPreset) -> TransferCurve {
    match preset {
        BuiltInTransferPreset::Linear => TransferCurve::linear(),
        BuiltInTransferPreset::BrightGamma => TransferCurve::gamma(2.0).unwrap(),
        BuiltInTransferPreset::HighContrast => TransferCurve::gamma(0.75).unwrap(),
    }
}

fn built_in_transfer_preset_label(preset: BuiltInTransferPreset) -> &'static str {
    match preset {
        BuiltInTransferPreset::Linear => "Linear",
        BuiltInTransferPreset::BrightGamma => "Bright gamma",
        BuiltInTransferPreset::HighContrast => "High contrast",
    }
}

fn built_in_transfer_presets() -> [BuiltInTransferPreset; 3] {
    [
        BuiltInTransferPreset::Linear,
        BuiltInTransferPreset::BrightGamma,
        BuiltInTransferPreset::HighContrast,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_transfer_presets_have_stable_ids_labels_and_curves() {
        let presets = built_in_transfer_presets();

        assert_eq!(
            presets
                .iter()
                .map(|preset| built_in_transfer_preset_id(*preset).to_owned())
                .collect::<Vec<_>>(),
            vec!["linear", "bright_gamma", "high_contrast"]
        );
        assert_eq!(
            presets
                .iter()
                .map(|preset| built_in_transfer_preset_label(*preset))
                .collect::<Vec<_>>(),
            vec!["Linear", "Bright gamma", "High contrast"]
        );
        assert_eq!(
            built_in_transfer_preset_curve(BuiltInTransferPreset::Linear),
            TransferCurve::linear()
        );
        assert_eq!(
            built_in_transfer_preset_curve(BuiltInTransferPreset::BrightGamma),
            TransferCurve::gamma(2.0).unwrap()
        );
    }
}
