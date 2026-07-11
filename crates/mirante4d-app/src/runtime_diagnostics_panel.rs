use eframe::egui;

use crate::{
    MiranteWorkbenchApp, application_view,
    cross_section_runtime::{
        CrossSectionChunkState, CrossSectionPanelRuntime, CrossSectionRuntime,
    },
    cross_section_streaming::CROSS_SECTION_CHUNK_READ_SUBMISSIONS_PER_REFRESH,
    current_egui_shell_bridge, current_physical_layer_id,
    fidelity::show_frame_fidelity_property_rows,
    scene_extraction::{SceneViewInput, scene_draw_list},
    ui_kit::{self, StatusTone},
    viewer_layout::PanelId,
};

pub(crate) fn show_runtime_diagnostics_body(app: &MiranteWorkbenchApp, ui: &mut egui::Ui) {
    let snapshot = current_egui_shell_bridge::snapshot(&app.application);
    let view = application_view(&snapshot);
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
    if let Ok(diagnostics) = app.dataset_runtime.dataset.diagnostics() {
        let config = diagnostics.config;
        let stats = diagnostics.stats;
        ui_kit::property_row(
            ui,
            "volume cache budget",
            format!("{} bytes", config.volume_cache_budget_bytes),
        );
        ui_kit::property_row(
            ui,
            "volume cache",
            format!(
                "{} bytes used, {} hit, {} miss",
                stats.volume_cache_bytes, stats.volume_cache_hits, stats.volume_cache_misses
            ),
        );
        ui_kit::property_row(
            ui,
            "brick cache budget",
            format!("{} bytes", config.brick_cache_budget_bytes),
        );
        ui_kit::property_row(
            ui,
            "brick cache",
            format!(
                "{} bytes used, {} hit, {} miss, {} read",
                stats.brick_cache_bytes,
                stats.brick_cache_hits,
                stats.brick_cache_misses,
                stats.brick_reads
            ),
        );
        ui_kit::property_row(
            ui,
            "upload staging budget",
            format!("{} bytes", config.upload_staging_budget_bytes),
        );
        ui_kit::property_row(
            ui,
            "decoded in-flight budget",
            format!("{} bytes", config.max_in_flight_decoded_bytes),
        );
        ui_kit::property_row(
            ui,
            "brick requests",
            format!(
                "{} queued, {} done, {} cancel, {} stale, {} fail",
                stats.brick_requests_queued,
                stats.brick_requests_completed,
                stats.brick_requests_cancelled,
                stats.brick_requests_stale,
                stats.brick_requests_failed
            ),
        );
        ui_kit::property_row(
            ui,
            "payload bytes",
            format!(
                "{} encoded read, {} decoded, {} brick decoded",
                stats.encoded_payload_bytes_read, stats.decoded_bytes, stats.decoded_brick_bytes
            ),
        );
    }
    if let Some(pool) = &app.dataset_runtime.brick_read_pool {
        ui_kit::property_row(ui, "brick workers", pool.worker_count().to_string());
        ui_kit::property_row(
            ui,
            "brick queue capacity",
            pool.queue_capacity().to_string(),
        );
        if let Ok(queue) = pool.queue_diagnostics() {
            ui_kit::property_row(
                ui,
                "brick queue depth",
                format!(
                    "{}/{} queued (current {}, prefetch {}, warm {}, closed {})",
                    queue.queued_total,
                    queue.capacity,
                    queue.queued_current_frame,
                    queue.queued_prefetch,
                    queue.queued_warm,
                    queue.closed
                ),
            );
        }
    }
    ui_kit::property_row(
        ui,
        "visible bricks",
        format!(
            "{} @ stride {}, scale s{} {:?}",
            app.render_runtime.visible_brick_count,
            app.render_runtime.visible_brick_plan_stride,
            app.dataset_runtime.brick_stream_scale_level,
            app.dataset_runtime.brick_stream_scale_shape
        ),
    );
    ui_kit::property_row(
        ui,
        "LOD schedule",
        format!(
            "shown {:?}, target s{}, fallback {:?}, pending {:?}, replan {}",
            app.render_runtime.lod_schedule.displayed_scale_level,
            app.render_runtime.lod_schedule.target_scale_level,
            app.render_runtime.lod_schedule.fallback_scale_level,
            app.render_runtime.lod_schedule.pending_scale_level,
            app.render_runtime.lod_replan_pending
        ),
    );
    ui_kit::property_row(
        ui,
        "stream",
        format!(
            "gen {}: {}/{} done, {} stale, {} fail, complete {}",
            app.dataset_runtime.brick_stream_generation,
            app.dataset_runtime.brick_stream_completed,
            app.dataset_runtime.brick_stream_requested,
            app.dataset_runtime.brick_stream_stale,
            app.dataset_runtime.brick_stream_failed,
            app.dataset_runtime.brick_stream_complete
        ),
    );
    ui_kit::property_row(
        ui,
        "active 2D panel",
        snapshot
            .transient()
            .active_cross_section_panel()
            .map(PanelId::from_application_panel)
            .map(|panel_id| panel_id.label().to_owned())
            .unwrap_or_else(|| "none".to_owned()),
    );
    ui_kit::property_row(
        ui,
        "2D interaction",
        format!(
            "recent {}, last age {} ms",
            app.dataset_runtime
                .cross_section_last_interaction_at
                .is_some_and(|instant| {
                    instant.elapsed() < crate::CROSS_SECTION_INTERACTION_SETTLE_DURATION
                }),
            app.dataset_runtime
                .cross_section_last_interaction_at
                .map(|instant| instant.elapsed().as_millis().to_string())
                .unwrap_or_else(|| "none".to_owned())
        ),
    );
    ui_kit::property_row(
        ui,
        "2D read budget",
        format!(
            "{} chunk submissions/refresh",
            CROSS_SECTION_CHUNK_READ_SUBMISSIONS_PER_REFRESH
        ),
    );
    for panel in app.render_runtime.cross_section_runtime.panels() {
        if panel.panel_id.cross_section_panel().is_some() {
            ui_kit::property_row(
                ui,
                format!("2D panel {}", panel.panel_id.label()),
                cross_section_panel_lod_summary(panel),
            );
        }
    }
    ui_kit::property_row(
        ui,
        "2D global runtime",
        cross_section_global_runtime_summary(&app.render_runtime.cross_section_runtime),
    );
    let state_counts =
        cross_section_runtime_state_counts(&app.render_runtime.cross_section_runtime);
    ui_kit::property_row(
        ui,
        "2D chunk states",
        format!(
            "absent {}, queued {}, decoding {}, CPU {}, upload {}, GPU {}, failed {}, evicted {}",
            state_counts.absent,
            state_counts.queued,
            state_counts.decoding,
            state_counts.cpu_resident,
            state_counts.upload_queued,
            state_counts.gpu_resident,
            state_counts.failed,
            state_counts.evicted
        ),
    );
    for (panel_id, panel) in &app.render_runtime.cross_section_runtime.panels {
        ui_kit::property_row(
            ui,
            format!("2D global {}", panel_id.label()),
            cross_section_global_panel_summary(panel),
        );
    }
    for (panel_id, stream) in &app.render_runtime.cross_section_runtime.panel_streams {
        ui_kit::property_row(
            ui,
            format!("2D stream {}", panel_id.label()),
            format!(
                "{:?}: {}/{} done, visible {}, occupied {}, deferred {}, queued current {}, queued prefetch {}, fairness {}, active {:?}, {} cancel, {} stale, {} fail, {} empty, complete {}",
                stream.priority,
                stream.completed,
                stream.requested,
                stream.visible_chunks,
                stream.occupied_visible_chunks,
                stream.deferred,
                stream.queued_current_frame,
                stream.queued_prefetch,
                stream.fairness_promoted,
                stream.active_panel_at_submission.map(|panel| panel.label()),
                stream.cancelled,
                stream.stale,
                stream.failed,
                stream.materialized_empty,
                stream.complete
            ),
        );
    }
    for (panel_id, displayed) in &app.render_runtime.cross_section_gpu_display_frames {
        let diagnostics = displayed.frame.diagnostics;
        ui_kit::property_row(
            ui,
            format!("2D frame {}", panel_id.label()),
            format!(
                "{} channel(s), {} output bytes, {} draw calls, {} vertices",
                diagnostics.channels,
                diagnostics.output_bytes,
                diagnostics.draw_calls,
                diagnostics.vertex_count
            ),
        );
    }
    ui_kit::property_row(
        ui,
        "prefetch",
        format!(
            "{:?}: {}/{} done, {} cancel, {} stale, {} fail, {} skip",
            app.dataset_runtime.brick_prefetch_timepoints,
            app.dataset_runtime.brick_prefetch_completed,
            app.dataset_runtime.brick_prefetch_requested,
            app.dataset_runtime.brick_prefetch_cancelled,
            app.dataset_runtime.brick_prefetch_stale,
            app.dataset_runtime.brick_prefetch_failed,
            app.dataset_runtime.brick_prefetch_skipped
        ),
    );
    ui_kit::property_row(
        ui,
        "warm",
        format!(
            "{} candidates: {}/{} done, {} cancel, {} stale, {} fail, {} skip",
            app.dataset_runtime.brick_warm_brick_count,
            app.dataset_runtime.brick_warm_completed,
            app.dataset_runtime.brick_warm_requested,
            app.dataset_runtime.brick_warm_cancelled,
            app.dataset_runtime.brick_warm_stale,
            app.dataset_runtime.brick_warm_failed,
            app.dataset_runtime.brick_warm_skipped
        ),
    );
    if let Some(renderer) = app.render_runtime.gpu_renderer.as_ref()
        && let Ok(stats) = renderer.stats()
    {
        ui_kit::property_row(
            ui,
            "gpu brick atlas",
            format!(
                "{} / {} bytes, {} upload ({} bytes), {} hit, {} miss, {} evict",
                stats.brick_atlas_resident_bytes,
                stats.brick_atlas_cache_budget_bytes,
                stats.brick_atlas_uploads,
                stats.brick_atlas_uploaded_bytes,
                stats.brick_atlas_cache_hits,
                stats.brick_atlas_cache_misses,
                stats.brick_atlas_evictions
            ),
        );
        ui_kit::property_row(
            ui,
            "gpu display resources",
            format!(
                "{} bytes, {} hit, {} miss, {} recreate",
                stats.display_resource_resident_bytes,
                stats.display_resource_cache_hits,
                stats.display_resource_cache_misses,
                stats.display_resource_recreations
            ),
        );
    }
    if let Some(timing) = app.render_runtime.last_display_refresh_timing {
        ui_kit::property_row(
            ui,
            "display timing",
            if let Some(gpu_compute_ms) = timing.gpu_compute_ms {
                format!(
                    "{}: render {:.2} ms, GPU upload {}, GPU compute {:.2} ms, egui texture {:.2} ms, brick request {:.2} ms, CPU texture {:.2} ms, total {:.2} ms",
                    timing.path.label(),
                    timing.render_ms,
                    display_gpu_upload_ms(timing.gpu_upload_ms),
                    gpu_compute_ms,
                    timing.egui_texture_ms,
                    timing.visible_brick_request_ms,
                    timing.cpu_texture_update_ms,
                    timing.total_ms
                )
            } else {
                format!(
                    "{}: render {:.2} ms, GPU upload {}, GPU compute unavailable, egui texture {:.2} ms, brick request {:.2} ms, CPU texture {:.2} ms, total {:.2} ms",
                    timing.path.label(),
                    timing.render_ms,
                    display_gpu_upload_ms(timing.gpu_upload_ms),
                    timing.egui_texture_ms,
                    timing.visible_brick_request_ms,
                    timing.cpu_texture_update_ms,
                    timing.total_ms
                )
            },
        );
    }
    show_frame_fidelity_property_rows(ui, &app.render_runtime.frame_fidelity);
    let active_layer_id = match current_physical_layer_id(&app.dataset_runtime, view.active_layer())
    {
        Ok(layer_id) => layer_id,
        Err(err) => {
            ui_kit::status_badge(ui, StatusTone::Warning, format!("scene extraction: {err}"));
            return;
        }
    };
    match scene_draw_list(
        &app.analysis_runtime,
        &app.ui_runtime,
        SceneViewInput {
            active_layer_id: &active_layer_id,
            active_timepoint: view.timepoint(),
            active_source_grid_to_world: snapshot
                .catalog()
                .layer(view.active_layer())
                .expect("application view closes over the dataset catalog")
                .grid_to_world(),
            camera: *view.camera(),
        },
    ) {
        Ok(draw_list) => {
            ui_kit::property_row(ui, "scene draw items", draw_list.len().to_string());
        }
        Err(err) => {
            ui_kit::status_badge(ui, StatusTone::Warning, format!("scene extraction: {err}"));
        }
    }
}

fn display_gpu_upload_ms(value: Option<f64>) -> String {
    value
        .map(|ms| format!("{ms:.2} ms"))
        .unwrap_or_else(|| "unavailable".to_owned())
}

pub(crate) fn diagnostics_summary_text(app: &MiranteWorkbenchApp) -> String {
    let snapshot = current_egui_shell_bridge::snapshot(&app.application);
    let mut text = app.startup_diagnostics.summary_text(
        Some(app.dataset_runtime.dataset.root()),
        app.startup_diagnostics.gpu_adapter.as_deref(),
    );
    text.push_str(&format!(
        "visible_bricks: {}\n\
         visible_brick_plan_stride: {}\n\
         brick_stream_scale_level: {}\n\
         brick_stream_generation: {}\n\
         brick_stream_requested: {}\n\
         brick_stream_completed: {}\n\
         brick_stream_cancelled: {}\n\
         brick_stream_stale: {}\n\
         brick_stream_failed: {}\n\
         brick_stream_complete: {}\n\
         brick_prefetch_requested: {}\n\
         brick_prefetch_completed: {}\n\
         brick_prefetch_cancelled: {}\n\
         brick_prefetch_stale: {}\n\
         brick_prefetch_failed: {}\n\
         brick_prefetch_skipped: {}\n\
         brick_warm_brick_count: {}\n\
         brick_warm_requested: {}\n\
         brick_warm_completed: {}\n\
         brick_warm_cancelled: {}\n\
         brick_warm_stale: {}\n\
         brick_warm_failed: {}\n\
         brick_warm_skipped: {}\n",
        app.render_runtime.visible_brick_count,
        app.render_runtime.visible_brick_plan_stride,
        app.dataset_runtime.brick_stream_scale_level,
        app.dataset_runtime.brick_stream_generation,
        app.dataset_runtime.brick_stream_requested,
        app.dataset_runtime.brick_stream_completed,
        app.dataset_runtime.brick_stream_cancelled,
        app.dataset_runtime.brick_stream_stale,
        app.dataset_runtime.brick_stream_failed,
        app.dataset_runtime.brick_stream_complete,
        app.dataset_runtime.brick_prefetch_requested,
        app.dataset_runtime.brick_prefetch_completed,
        app.dataset_runtime.brick_prefetch_cancelled,
        app.dataset_runtime.brick_prefetch_stale,
        app.dataset_runtime.brick_prefetch_failed,
        app.dataset_runtime.brick_prefetch_skipped,
        app.dataset_runtime.brick_warm_brick_count,
        app.dataset_runtime.brick_warm_requested,
        app.dataset_runtime.brick_warm_completed,
        app.dataset_runtime.brick_warm_cancelled,
        app.dataset_runtime.brick_warm_stale,
        app.dataset_runtime.brick_warm_failed,
        app.dataset_runtime.brick_warm_skipped
    ));
    text.push_str(&format!(
        "cross_section_active_panel: {}\n",
        snapshot
            .transient()
            .active_cross_section_panel()
            .map(PanelId::from_application_panel)
            .map(|panel_id| panel_id.label().to_owned())
            .unwrap_or_else(|| "none".to_owned())
    ));
    text.push_str(&format!(
        "cross_section_read_submission_budget_per_refresh: {}\n",
        CROSS_SECTION_CHUNK_READ_SUBMISSIONS_PER_REFRESH
    ));
    text.push_str(&format!(
        "cross_section_interaction_recent: {}\n\
         cross_section_last_interaction_age_ms: {}\n",
        app.dataset_runtime
            .cross_section_last_interaction_at
            .is_some_and(|instant| {
                instant.elapsed() < crate::CROSS_SECTION_INTERACTION_SETTLE_DURATION
            }),
        app.dataset_runtime
            .cross_section_last_interaction_at
            .map(|instant| instant.elapsed().as_millis().to_string())
            .unwrap_or_else(|| "none".to_owned())
    ));
    for panel in app.render_runtime.cross_section_runtime.panels() {
        if panel.panel_id.cross_section_panel().is_some() {
            append_cross_section_panel_diagnostics(&mut text, panel);
        }
    }
    append_cross_section_global_runtime_diagnostics(
        &mut text,
        &app.render_runtime.cross_section_runtime,
    );
    for (panel_id, stream) in &app.render_runtime.cross_section_runtime.panel_streams {
        text.push_str(&format!(
            "cross_section_stream_{}_priority: {:?}\n\
             cross_section_stream_{}_requested: {}\n\
             cross_section_stream_{}_completed: {}\n\
             cross_section_stream_{}_deferred: {}\n\
             cross_section_stream_{}_queued_current_frame: {}\n\
             cross_section_stream_{}_queued_prefetch: {}\n\
             cross_section_stream_{}_fairness_promoted: {}\n\
             cross_section_stream_{}_active_panel_at_submission: {}\n\
             cross_section_stream_{}_cancelled: {}\n\
             cross_section_stream_{}_stale: {}\n\
             cross_section_stream_{}_failed: {}\n\
             cross_section_stream_{}_materialized_empty: {}\n\
             cross_section_stream_{}_visible_chunks: {}\n\
             cross_section_stream_{}_occupied_visible_chunks: {}\n\
             cross_section_stream_{}_decoded_bytes: {}\n\
             cross_section_stream_{}_encoded_payload_bytes_read: {}\n\
             cross_section_stream_{}_complete: {}\n",
            panel_id.label(),
            stream.priority,
            panel_id.label(),
            stream.requested,
            panel_id.label(),
            stream.completed,
            panel_id.label(),
            stream.deferred,
            panel_id.label(),
            stream.queued_current_frame,
            panel_id.label(),
            stream.queued_prefetch,
            panel_id.label(),
            stream.fairness_promoted,
            panel_id.label(),
            stream
                .active_panel_at_submission
                .map(|panel| panel.label().to_owned())
                .unwrap_or_else(|| "none".to_owned()),
            panel_id.label(),
            stream.cancelled,
            panel_id.label(),
            stream.stale,
            panel_id.label(),
            stream.failed,
            panel_id.label(),
            stream.materialized_empty,
            panel_id.label(),
            stream.visible_chunks,
            panel_id.label(),
            stream.occupied_visible_chunks,
            panel_id.label(),
            stream.decoded_bytes,
            panel_id.label(),
            stream.encoded_payload_bytes_read,
            panel_id.label(),
            stream.complete
        ));
    }
    for (panel_id, displayed) in &app.render_runtime.cross_section_gpu_display_frames {
        let diagnostics = displayed.frame.diagnostics;
        text.push_str(&format!(
            "cross_section_frame_{}_channels: {}\n\
             cross_section_frame_{}_output_bytes: {}\n\
             cross_section_frame_{}_draw_calls: {}\n\
             cross_section_frame_{}_vertex_count: {}\n",
            panel_id.label(),
            diagnostics.channels,
            panel_id.label(),
            diagnostics.output_bytes,
            panel_id.label(),
            diagnostics.draw_calls,
            panel_id.label(),
            diagnostics.vertex_count
        ));
    }
    if let Some(pool) = &app.dataset_runtime.brick_read_pool
        && let Ok(queue) = pool.queue_diagnostics()
    {
        text.push_str(&format!(
            "brick_queue_capacity: {}\n\
             brick_queue_total: {}\n\
             brick_queue_current_frame: {}\n\
             brick_queue_prefetch: {}\n\
             brick_queue_warm: {}\n\
             brick_queue_closed: {}\n",
            queue.capacity,
            queue.queued_total,
            queue.queued_current_frame,
            queue.queued_prefetch,
            queue.queued_warm,
            queue.closed
        ));
    }
    text
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CrossSectionRuntimeStateCounts {
    absent: usize,
    queued: usize,
    decoding: usize,
    cpu_resident: usize,
    upload_queued: usize,
    gpu_resident: usize,
    failed: usize,
    evicted: usize,
}

fn cross_section_global_runtime_summary(runtime: &CrossSectionRuntime) -> String {
    format!(
        "{} global chunks, {} panels, visible work {}, CPU payload {} / {} bytes, queues dl {} gpu+ {} cpu- {} gpu- {}",
        runtime.chunks.len(),
        runtime.panels.len(),
        runtime.has_visible_work(),
        runtime.resident_payload_bytes(),
        runtime.cpu_payload_budget_bytes,
        runtime.queues.download_promotions.entries().len(),
        runtime.queues.gpu_promotions.entries().len(),
        runtime.queues.cpu_evictions.entries().len(),
        runtime.queues.gpu_evictions.entries().len()
    )
}

fn cross_section_global_panel_summary(panel: &CrossSectionPanelRuntime) -> String {
    format!(
        "gen {}, s{}, {:?}, {} visible, {} geometry, {} candidates",
        panel.generation,
        panel.scale_level,
        panel.priority_tier,
        panel.visible_chunks.len(),
        panel.visible_chunk_geometries.len(),
        panel.candidate_chunks
    )
}

fn cross_section_runtime_state_counts(
    runtime: &CrossSectionRuntime,
) -> CrossSectionRuntimeStateCounts {
    let mut counts = CrossSectionRuntimeStateCounts::default();
    for entry in runtime.chunks.values() {
        match entry.state {
            CrossSectionChunkState::Absent => counts.absent += 1,
            CrossSectionChunkState::Queued => counts.queued += 1,
            CrossSectionChunkState::Decoding => counts.decoding += 1,
            CrossSectionChunkState::CpuResident => counts.cpu_resident += 1,
            CrossSectionChunkState::UploadQueued => counts.upload_queued += 1,
            CrossSectionChunkState::GpuResident => counts.gpu_resident += 1,
            CrossSectionChunkState::Failed => counts.failed += 1,
            CrossSectionChunkState::Evicted => counts.evicted += 1,
        }
    }
    counts
}

fn append_cross_section_global_runtime_diagnostics(
    text: &mut String,
    runtime: &CrossSectionRuntime,
) {
    let counts = cross_section_runtime_state_counts(runtime);
    text.push_str(&format!(
        "cross_section_global_chunks: {}\n\
         cross_section_global_panels: {}\n\
         cross_section_global_visible_work: {}\n\
         cross_section_global_state_absent: {}\n\
         cross_section_global_state_queued: {}\n\
         cross_section_global_state_decoding: {}\n\
         cross_section_global_state_cpu_resident: {}\n\
         cross_section_global_state_upload_queued: {}\n\
         cross_section_global_state_gpu_resident: {}\n\
         cross_section_global_state_failed: {}\n\
         cross_section_global_state_evicted: {}\n\
         cross_section_global_cpu_payload_budget_bytes: {}\n\
         cross_section_global_cpu_payload_resident_bytes: {}\n\
         cross_section_global_cpu_payload_eviction_passes: {}\n\
         cross_section_global_cpu_payload_evicted_chunks: {}\n\
         cross_section_global_cpu_payload_evicted_bytes: {}\n\
         cross_section_global_cpu_payload_last_over_budget_after: {}\n\
         cross_section_global_queue_revision: {}\n\
         cross_section_global_queue_download_promotions: {}\n\
         cross_section_global_queue_gpu_promotions: {}\n\
         cross_section_global_queue_cpu_evictions: {}\n\
         cross_section_global_queue_gpu_evictions: {}\n",
        runtime.chunks.len(),
        runtime.panels.len(),
        runtime.has_visible_work(),
        counts.absent,
        counts.queued,
        counts.decoding,
        counts.cpu_resident,
        counts.upload_queued,
        counts.gpu_resident,
        counts.failed,
        counts.evicted,
        runtime.cpu_payload_budget_bytes,
        runtime.resident_payload_bytes(),
        runtime.cpu_payload_eviction_passes,
        runtime.cpu_payload_evicted_chunks,
        runtime.cpu_payload_evicted_bytes,
        runtime.cpu_payload_last_eviction.over_budget_after,
        runtime.queues.revision,
        runtime.queues.download_promotions.entries().len(),
        runtime.queues.gpu_promotions.entries().len(),
        runtime.queues.cpu_evictions.entries().len(),
        runtime.queues.gpu_evictions.entries().len()
    ));
    for (panel_id, panel) in &runtime.panels {
        text.push_str(&format!(
            "cross_section_global_panel_{}_generation: {}\n\
             cross_section_global_panel_{}_scale_level: {}\n\
             cross_section_global_panel_{}_priority_tier: {:?}\n\
             cross_section_global_panel_{}_candidate_chunks: {}\n\
             cross_section_global_panel_{}_visible_chunks: {}\n\
             cross_section_global_panel_{}_geometry_chunks: {}\n",
            panel_id.label(),
            panel.generation,
            panel_id.label(),
            panel.scale_level,
            panel_id.label(),
            panel.priority_tier,
            panel_id.label(),
            panel.candidate_chunks,
            panel_id.label(),
            panel.visible_chunks.len(),
            panel_id.label(),
            panel.visible_chunk_geometries.len()
        ));
    }
}

fn cross_section_panel_lod_summary(panel: &CrossSectionPanelRuntime) -> String {
    let Some(schedule) = panel.cross_section_schedule else {
        return format!(
            "target none, render none, fallback none, display {}, gen {}/{}",
            display_current_label(panel.display_current()),
            displayed_generation_label(panel.displayed_generation),
            panel.generation
        );
    };
    format!(
        "target {}, render {}, fallback {}, display {}, gen {}/{}, status {:?}, bricks {}/{}, missing {}",
        scale_level_label(schedule.target_scale_level),
        scale_level_label(schedule.render_scale_level),
        scale_level_label(schedule.fallback_scale_level),
        display_current_label(panel.display_current()),
        displayed_generation_label(panel.displayed_generation),
        panel.generation,
        schedule.status,
        schedule.occupied_selected_bricks,
        schedule.selected_bricks,
        schedule.missing_occupied_bricks
    )
}

fn append_cross_section_panel_diagnostics(text: &mut String, panel: &CrossSectionPanelRuntime) {
    if let Some(schedule) = panel.cross_section_schedule {
        text.push_str(&format!(
            "cross_section_panel_{}_target_scale_level: {}\n\
             cross_section_panel_{}_render_scale_level: {}\n\
             cross_section_panel_{}_fallback_scale_level: {}\n\
             cross_section_panel_{}_display_current: {}\n\
             cross_section_panel_{}_generation: {}\n\
             cross_section_panel_{}_displayed_generation: {}\n\
             cross_section_panel_{}_status: {:?}\n\
             cross_section_panel_{}_selected_bricks: {}\n\
             cross_section_panel_{}_occupied_selected_bricks: {}\n\
             cross_section_panel_{}_missing_occupied_bricks: {}\n",
            panel.panel_id.label(),
            optional_scale_level_value(schedule.target_scale_level),
            panel.panel_id.label(),
            optional_scale_level_value(schedule.render_scale_level),
            panel.panel_id.label(),
            optional_scale_level_value(schedule.fallback_scale_level),
            panel.panel_id.label(),
            panel.display_current(),
            panel.panel_id.label(),
            panel.generation,
            panel.panel_id.label(),
            optional_generation_value(panel.displayed_generation),
            panel.panel_id.label(),
            schedule.status,
            panel.panel_id.label(),
            schedule.selected_bricks,
            panel.panel_id.label(),
            schedule.occupied_selected_bricks,
            panel.panel_id.label(),
            schedule.missing_occupied_bricks
        ));
    } else {
        text.push_str(&format!(
            "cross_section_panel_{}_target_scale_level: none\n\
             cross_section_panel_{}_render_scale_level: none\n\
             cross_section_panel_{}_fallback_scale_level: none\n\
             cross_section_panel_{}_display_current: {}\n\
             cross_section_panel_{}_generation: {}\n\
             cross_section_panel_{}_displayed_generation: {}\n\
             cross_section_panel_{}_status: none\n\
             cross_section_panel_{}_selected_bricks: none\n\
             cross_section_panel_{}_occupied_selected_bricks: none\n\
             cross_section_panel_{}_missing_occupied_bricks: none\n",
            panel.panel_id.label(),
            panel.panel_id.label(),
            panel.panel_id.label(),
            panel.panel_id.label(),
            panel.display_current(),
            panel.panel_id.label(),
            panel.generation,
            panel.panel_id.label(),
            optional_generation_value(panel.displayed_generation),
            panel.panel_id.label(),
            panel.panel_id.label(),
            panel.panel_id.label(),
            panel.panel_id.label()
        ));
    }
}

fn scale_level_label(scale_level: Option<u32>) -> String {
    scale_level
        .map(|scale| format!("s{scale}"))
        .unwrap_or_else(|| "none".to_owned())
}

fn optional_scale_level_value(scale_level: Option<u32>) -> String {
    scale_level
        .map(|scale| scale.to_string())
        .unwrap_or_else(|| "none".to_owned())
}

fn optional_generation_value(generation: Option<u64>) -> String {
    generation
        .map(|generation| generation.to_string())
        .unwrap_or_else(|| "none".to_owned())
}

fn displayed_generation_label(generation: Option<u64>) -> String {
    generation
        .map(|generation| generation.to_string())
        .unwrap_or_else(|| "-".to_owned())
}

fn display_current_label(display_current: bool) -> &'static str {
    if display_current { "current" } else { "stale" }
}
