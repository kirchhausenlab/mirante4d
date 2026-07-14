use eframe::egui;
use mirante4d_dataset::CpuLedgerCategory;

use crate::{
    MiranteWorkbenchApp, current_egui_shell_bridge, fidelity::show_frame_fidelity_property_rows,
    ui_kit, viewer_layout::PanelId,
};

const CPU_CATEGORIES: [(CpuLedgerCategory, &str); 7] = [
    (CpuLedgerCategory::DecodedResidency, "decoded residency"),
    (CpuLedgerCategory::UploadStaging, "upload staging"),
    (CpuLedgerCategory::InFlightDecode, "in-flight decode"),
    (CpuLedgerCategory::MetadataAndIndexes, "metadata/indexes"),
    (CpuLedgerCategory::QueuesAndResults, "queues/results"),
    (CpuLedgerCategory::Prefetch, "prefetch"),
    (CpuLedgerCategory::ImportWorkingSet, "import working set"),
];

pub(crate) fn show_runtime_diagnostics_body(app: &MiranteWorkbenchApp, ui: &mut egui::Ui) {
    let snapshot = current_egui_shell_bridge::snapshot(&app.application);
    if ui_kit::toolbar_button(ui, "Copy Diagnostics", true).clicked() {
        ui.ctx().copy_text(app.diagnostics_summary_text());
    }
    ui_kit::property_row(
        ui,
        "logs",
        app.startup_diagnostics
            .logs_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "stderr/stdout".to_owned()),
    );
    ui_kit::property_row(
        ui,
        "source",
        app.dataset.selected_path().display().to_string(),
    );

    match app.dataset.dispatcher().diagnostics() {
        Ok(diagnostics) => {
            ui_kit::property_row(
                ui,
                "dataset CPU",
                format!(
                    "{} / {} bytes",
                    diagnostics.total_used_bytes(),
                    diagnostics.total_cap_bytes()
                ),
            );
            for (category, label) in CPU_CATEGORIES {
                ui_kit::property_row(
                    ui,
                    label,
                    format!(
                        "{} / {} bytes",
                        diagnostics.category_used_bytes(category),
                        diagnostics.category_cap_bytes(category)
                    ),
                );
            }
            ui_kit::property_row(
                ui,
                "requests",
                format!(
                    "{} queued, {} decoding, {} completions; {} submitted, {} ready, {} cancelled, {} failed",
                    diagnostics.queued_requests(),
                    diagnostics.in_flight_decodes(),
                    diagnostics.pending_completions(),
                    diagnostics.submitted_requests(),
                    diagnostics.ready_requests(),
                    diagnostics.cancelled_requests(),
                    diagnostics.failed_requests()
                ),
            );
            ui_kit::property_row(
                ui,
                "queue bounds",
                format!(
                    "requests {}, completions {}, workers {}",
                    diagnostics.request_queue_limit(),
                    diagnostics.completion_queue_limit(),
                    diagnostics.worker_limit()
                ),
            );
            ui_kit::property_row(
                ui,
                "resident resources",
                diagnostics.resident_resources().to_string(),
            );
        }
        Err(error) => ui_kit::property_row(ui, "dataset runtime", error.to_string()),
    }

    ui_kit::property_row(
        ui,
        "renderer leases",
        format!(
            "{} / {} retained, {} missing",
            app.dataset.retained_leases().retained_len(),
            app.dataset.retained_leases().required_len(),
            app.dataset.retained_leases().missing_len()
        ),
    );
    ui_kit::property_row(
        ui,
        "LOD",
        format!(
            "shown {:?}, target s{}",
            app.render_runtime.frame_fidelity.displayed_scale_level,
            app.render_runtime.frame_fidelity.target_scale_level,
        ),
    );
    ui_kit::property_row(
        ui,
        "active 2D panel",
        snapshot
            .transient()
            .active_cross_section_panel()
            .map(PanelId::from_application_panel)
            .map(|panel| panel.label().to_owned())
            .unwrap_or_else(|| "none".to_owned()),
    );
    for panel in app.render_runtime.cross_section_runtime.panels() {
        if panel.panel_id.cross_section_panel().is_some() {
            ui_kit::property_row(
                ui,
                format!("2D {}", panel.panel_id.label()),
                panel_summary(panel),
            );
        }
    }
    if let Some(product) = app.native_presentation.product_gpu.as_ref() {
        let diagnostics = product.renderer.diagnostics();
        ui_kit::property_row(
            ui,
            "GPU residency",
            format!(
                "{} / {} bytes, {} frames, {} submissions",
                diagnostics.resident_payload_bytes(),
                diagnostics.payload_capacity_bytes(),
                diagnostics.frames_executed(),
                diagnostics.queue_submissions(),
            ),
        );
        ui_kit::property_row(
            ui,
            "progressive frames",
            format!(
                "{} partial, {} settled, {} stale rejected",
                product.current_partial_frames_presented,
                product.partial_to_settled_transitions,
                product.stale_frames_rejected,
            ),
        );
    }
    if let Some(timing) = app.render_runtime.last_display_refresh_timing {
        ui_kit::property_row(
            ui,
            "display timing",
            format!(
                "{}: render {:.2} ms, GPU upload {}, GPU compute {}, total {:.2} ms",
                timing.path.label(),
                timing.render_ms,
                optional_ms(timing.gpu_upload_ms),
                optional_ms(timing.gpu_compute_ms),
                timing.total_ms
            ),
        );
    }
    show_frame_fidelity_property_rows(ui, &app.render_runtime.frame_fidelity);
}

pub(crate) fn diagnostics_summary_text(app: &MiranteWorkbenchApp) -> String {
    let mut text = app.startup_diagnostics.summary_text(
        Some(app.dataset.selected_path()),
        app.startup_diagnostics.gpu_adapter.as_deref(),
    );
    match app.dataset.dispatcher().diagnostics() {
        Ok(diagnostics) => {
            text.push_str(&format!(
                "dataset_cpu_used_bytes: {}\n\
                 dataset_cpu_cap_bytes: {}\n\
                 dataset_queued_requests: {}\n\
                 dataset_in_flight_decodes: {}\n\
                 dataset_pending_completions: {}\n\
                 dataset_resident_resources: {}\n\
                 dataset_submitted_requests: {}\n\
                 dataset_ready_requests: {}\n\
                 dataset_cancelled_requests: {}\n\
                 dataset_failed_requests: {}\n",
                diagnostics.total_used_bytes(),
                diagnostics.total_cap_bytes(),
                diagnostics.queued_requests(),
                diagnostics.in_flight_decodes(),
                diagnostics.pending_completions(),
                diagnostics.resident_resources(),
                diagnostics.submitted_requests(),
                diagnostics.ready_requests(),
                diagnostics.cancelled_requests(),
                diagnostics.failed_requests(),
            ));
            for (category, label) in CPU_CATEGORIES {
                let key = label.replace([' ', '-'], "_");
                text.push_str(&format!(
                    "dataset_cpu_{key}_used_bytes: {}\ndataset_cpu_{key}_cap_bytes: {}\n",
                    diagnostics.category_used_bytes(category),
                    diagnostics.category_cap_bytes(category)
                ));
            }
        }
        Err(error) => text.push_str(&format!("dataset_runtime_error: {error}\n")),
    }
    text.push_str(&format!(
        "renderer_required_leases: {}\n\
         renderer_retained_leases: {}\n\
         renderer_missing_leases: {}\n\
         current_scale_level: {}\n",
        app.dataset.retained_leases().required_len(),
        app.dataset.retained_leases().retained_len(),
        app.dataset.retained_leases().missing_len(),
        app.dataset.current_scale().get(),
    ));
    for panel in app.render_runtime.cross_section_runtime.panels() {
        if let Some(schedule) = panel.cross_section_schedule {
            text.push_str(&format!(
                "cross_section_{}_generation: {}\n\
                 cross_section_{}_displayed_generation: {}\n\
                 cross_section_{}_status: {:?}\n\
                 cross_section_{}_required: {}\n\
                 cross_section_{}_retained: {}\n\
                 cross_section_{}_missing: {}\n",
                panel.panel_id.label(),
                panel.generation,
                panel.panel_id.label(),
                panel
                    .displayed_generation
                    .map_or_else(|| "none".to_owned(), |value| value.to_string()),
                panel.panel_id.label(),
                schedule.status,
                panel.panel_id.label(),
                schedule.selected_bricks,
                panel.panel_id.label(),
                schedule.occupied_selected_bricks,
                panel.panel_id.label(),
                schedule.missing_occupied_bricks,
            ));
        }
    }
    text
}

fn panel_summary(panel: &crate::cross_section_runtime::CrossSectionPanelRuntime) -> String {
    let Some(schedule) = panel.cross_section_schedule else {
        return format!("generation {}, no schedule", panel.generation);
    };
    format!(
        "{:?}, s{:?}, {}/{} retained, {} missing, generation {}/{}",
        schedule.status,
        schedule.render_scale_level,
        schedule.occupied_selected_bricks,
        schedule.selected_bricks,
        schedule.missing_occupied_bricks,
        panel
            .displayed_generation
            .map_or_else(|| "none".to_owned(), |value| value.to_string()),
        panel.generation,
    )
}

fn optional_ms(value: Option<f64>) -> String {
    value
        .map(|milliseconds| format!("{milliseconds:.2} ms"))
        .unwrap_or_else(|| "unavailable".to_owned())
}
