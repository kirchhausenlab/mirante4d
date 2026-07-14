use std::path::Path;

use eframe::egui;
use mirante4d_application::{ApplicationCommand, ApplicationSnapshot};
use mirante4d_domain::{
    CameraView, DvrOpacityTransfer, IsoShadingPolicy, Projection, RenderMode, RenderState,
    SamplingPolicy, TimeIndex, TransferCurve,
};
use mirante4d_project_model::{LayerViewState, ViewState};

use crate::{
    BACKGROUND_WORK_REPAINT_INTERVAL, playback::stepped_timepoint, ui_kit,
    workbench_playback_runtime::catalog_timepoint_count,
};

const DEFAULT_ISO_DISPLAY_LEVEL: f32 = 0.5;
const DEFAULT_DVR_DENSITY_SCALE: f64 = 12.0;
const DEFAULT_DVR_OPACITY_GAMMA: f32 = 0.25;

pub(crate) fn request_background_work_repaint(ctx: &egui::Context) {
    ctx.request_repaint();
    request_background_work_repaint_after(ctx);
}

pub(crate) fn request_background_work_repaint_after(ctx: &egui::Context) {
    ctx.request_repaint_after(BACKGROUND_WORK_REPAINT_INTERVAL);
}

pub(crate) fn show_playback_controls(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    workflow_busy: bool,
    application_commands: &mut Vec<ApplicationCommand>,
) {
    let active_timepoint = view.timepoint();
    let timepoint_count = catalog_timepoint_count(snapshot);
    if timepoint_count <= 1 {
        ui_kit::muted_label(ui, "time t 1/1");
        return;
    }

    if ui_kit::toolbar_button(ui, "First", !workflow_busy).clicked() {
        application_commands.push(ApplicationCommand::SetTimepoint(TimeIndex::new(0)));
    }
    if ui_kit::toolbar_button(ui, "Prev", !workflow_busy).clicked() {
        application_commands.push(ApplicationCommand::SetTimepoint(stepped_timepoint(
            active_timepoint,
            timepoint_count,
            -1,
        )));
    }
    let playback_active = snapshot.transient().playback_active();
    let playback_label = if playback_active { "Pause" } else { "Play" };
    if ui_kit::toolbar_button(ui, playback_label, !workflow_busy).clicked() {
        application_commands.push(ApplicationCommand::SetPlaybackActive(!playback_active));
    }
    if ui_kit::toolbar_button(ui, "Next", !workflow_busy).clicked() {
        application_commands.push(ApplicationCommand::SetTimepoint(stepped_timepoint(
            active_timepoint,
            timepoint_count,
            1,
        )));
    }
    if ui_kit::toolbar_button(ui, "Last", !workflow_busy).clicked() {
        application_commands.push(ApplicationCommand::SetTimepoint(TimeIndex::new(
            timepoint_count - 1,
        )));
    }

    let mut timepoint = active_timepoint.get().min(timepoint_count - 1);
    let slider_width = ui.available_width().clamp(120.0, 360.0);
    let response = ui.add_sized(
        [
            slider_width,
            ui_kit::UiTokens::default().spacing.control_height,
        ],
        egui::Slider::new(&mut timepoint, 0..=timepoint_count - 1).text("t"),
    );
    if response.changed() {
        application_commands.push(ApplicationCommand::SetTimepoint(TimeIndex::new(timepoint)));
    }
    ui_kit::muted_label(
        ui,
        format!("t {}/{}", active_timepoint.get() + 1, timepoint_count),
    );
}

pub(crate) fn dataset_path_status_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("dataset")
        .to_owned()
}

pub(crate) fn render_mode_selector(
    ui: &mut egui::Ui,
    view: &ViewState,
) -> Option<ApplicationCommand> {
    let current_layer = view
        .layer(view.active_layer())
        .expect("ViewState contains its active layer by construction");
    let current_mode = current_layer.render_state().mode();
    let mut mode = current_mode;
    egui::ComboBox::from_id_salt("render-mode-selector")
        .selected_text(render_mode_label(mode))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut mode, RenderMode::Mip, "MIP");
            ui.selectable_value(&mut mode, RenderMode::Isosurface, "ISO");
            ui.selectable_value(&mut mode, RenderMode::Dvr, "DVR");
        });
    if mode != current_mode {
        Some(ApplicationCommand::SetLayerView(LayerViewState::new(
            current_layer.layer_key(),
            current_layer.visible(),
            current_layer.transfer().clone(),
            render_state_for_mode(current_layer, mode),
        )))
    } else {
        None
    }
}

pub(crate) fn render_mode_label(mode: RenderMode) -> &'static str {
    match mode {
        RenderMode::Mip => "MIP",
        RenderMode::Isosurface => "ISO",
        RenderMode::Dvr => "DVR",
    }
}

pub(crate) fn projection_selector(
    ui: &mut egui::Ui,
    camera: CameraView,
) -> Option<ApplicationCommand> {
    let current_projection = camera.projection();
    let mut projection = current_projection;
    egui::ComboBox::from_id_salt("projection-selector")
        .selected_text(format!("{projection:?}"))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut projection, Projection::Orthographic, "Orthographic");
            ui.selectable_value(&mut projection, Projection::Perspective, "Perspective");
        });
    if projection != current_projection {
        Some(ApplicationCommand::SetCamera(
            CameraView::new(
                projection,
                camera.target(),
                camera.orientation(),
                camera.orthographic_world_per_screen_point(),
                camera.perspective_focal_length_screen_points(),
                camera.perspective_view_distance_world(),
            )
            .expect("changing projection preserves validated camera invariants"),
        ))
    } else {
        None
    }
}

fn render_state_for_mode(layer: &LayerViewState, mode: RenderMode) -> RenderState {
    let current = *layer.render_state();
    let sampling = SamplingPolicy::VoxelExact;
    match mode {
        RenderMode::Mip => RenderState::mip(sampling),
        RenderMode::Isosurface => {
            let display_level = current
                .iso_parameters()
                .map(|parameters| parameters.display_level())
                .unwrap_or(DEFAULT_ISO_DISPLAY_LEVEL);
            RenderState::iso(sampling, IsoShadingPolicy::Flat, display_level)
                .expect("the retained or default ISO parameters are valid")
        }
        RenderMode::Dvr => {
            let (opacity_transfer, density_scale) = current
                .dvr_parameters()
                .map(|parameters| (parameters.opacity_transfer(), parameters.density_scale()))
                .unwrap_or_else(|| {
                    (
                        DvrOpacityTransfer::new(
                            layer.transfer().window(),
                            TransferCurve::gamma(DEFAULT_DVR_OPACITY_GAMMA)
                                .expect("the default DVR opacity gamma is valid"),
                        ),
                        DEFAULT_DVR_DENSITY_SCALE,
                    )
                });
            RenderState::dvr(sampling, opacity_transfer, density_scale)
                .expect("the retained or default DVR parameters are valid")
        }
    }
}
