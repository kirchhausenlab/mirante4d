use std::path::Path;

use eframe::egui;
use mirante4d_core::{Projection, TimeIndex};

use crate::{
    BACKGROUND_WORK_REPAINT_INTERVAL, RenderMode, commands::WorkbenchCommand,
    playback::PlaybackState, ui_kit,
};

pub(crate) fn request_background_work_repaint(ctx: &egui::Context) {
    ctx.request_repaint();
    request_background_work_repaint_after(ctx);
}

pub(crate) fn request_background_work_repaint_after(ctx: &egui::Context) {
    ctx.request_repaint_after(BACKGROUND_WORK_REPAINT_INTERVAL);
}

pub(crate) fn show_playback_controls(
    ui: &mut egui::Ui,
    playback: PlaybackState,
    active_timepoint: TimeIndex,
    timepoint_count: u64,
    workflow_busy: bool,
    workbench_commands: &mut Vec<WorkbenchCommand>,
) {
    if timepoint_count <= 1 {
        ui_kit::muted_label(ui, "time t 1/1");
        return;
    }

    if ui_kit::toolbar_button(ui, "First", !workflow_busy).clicked() {
        workbench_commands.push(WorkbenchCommand::SetTimepoint(TimeIndex(0)));
    }
    if ui_kit::toolbar_button(ui, "Prev", !workflow_busy).clicked() {
        workbench_commands.push(WorkbenchCommand::StepTimepoint { delta: -1 });
    }
    let playback_label = if playback.playing { "Pause" } else { "Play" };
    if ui_kit::toolbar_button(ui, playback_label, !workflow_busy).clicked() {
        workbench_commands.push(WorkbenchCommand::SetPlayback {
            playing: !playback.playing,
        });
    }
    if ui_kit::toolbar_button(ui, "Next", !workflow_busy).clicked() {
        workbench_commands.push(WorkbenchCommand::StepTimepoint { delta: 1 });
    }
    if ui_kit::toolbar_button(ui, "Last", !workflow_busy).clicked() {
        workbench_commands.push(WorkbenchCommand::SetTimepoint(TimeIndex(
            timepoint_count - 1,
        )));
    }

    let mut timepoint = active_timepoint.0.min(timepoint_count - 1);
    let slider_width = ui.available_width().clamp(120.0, 360.0);
    let response = ui.add_sized(
        [
            slider_width,
            ui_kit::UiTokens::default().spacing.control_height,
        ],
        egui::Slider::new(&mut timepoint, 0..=timepoint_count - 1).text("t"),
    );
    if response.changed() {
        workbench_commands.push(WorkbenchCommand::SetTimepoint(TimeIndex(timepoint)));
    }
    ui_kit::muted_label(
        ui,
        format!("t {}/{}", active_timepoint.0 + 1, timepoint_count),
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
    current_mode: RenderMode,
) -> Option<WorkbenchCommand> {
    let mut mode = current_mode;
    egui::ComboBox::from_id_salt("render-mode-selector")
        .selected_text(render_mode_label(mode))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut mode, RenderMode::Mip, "MIP");
            ui.selectable_value(&mut mode, RenderMode::Isosurface, "ISO");
            ui.selectable_value(&mut mode, RenderMode::Dvr, "DVR");
        });
    if mode != current_mode {
        Some(WorkbenchCommand::SetRenderMode(mode))
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
    current_projection: Projection,
) -> Option<WorkbenchCommand> {
    let mut projection = current_projection;
    egui::ComboBox::from_id_salt("projection-selector")
        .selected_text(format!("{projection:?}"))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut projection, Projection::Orthographic, "Orthographic");
            ui.selectable_value(&mut projection, Projection::Perspective, "Perspective");
        });
    if projection != current_projection {
        Some(WorkbenchCommand::SetProjection(projection))
    } else {
        None
    }
}
