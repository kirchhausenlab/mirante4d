use std::path::Path;

use eframe::egui;

use crate::BACKGROUND_WORK_REPAINT_INTERVAL;

pub(crate) fn request_background_work_repaint(ctx: &egui::Context) {
    ctx.request_repaint();
    request_background_work_repaint_after(ctx);
}

pub(crate) fn request_background_work_repaint_after(ctx: &egui::Context) {
    ctx.request_repaint_after(BACKGROUND_WORK_REPAINT_INTERVAL);
}

pub(crate) fn dataset_path_status_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("dataset")
        .to_owned()
}
