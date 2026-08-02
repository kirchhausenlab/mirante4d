use mirante4d_application::RenderSurfaceState;
use mirante4d_dataset::CpuLedgerCategory;
use mirante4d_ui_egui::RuntimeDiagnosticsView;

use crate::{MiranteWorkbenchApp, viewer_layout::PanelId};

const CPU_CATEGORIES: [(CpuLedgerCategory, &str); 7] = [
    (CpuLedgerCategory::DecodedResidency, "decoded residency"),
    (CpuLedgerCategory::UploadStaging, "upload staging"),
    (CpuLedgerCategory::InFlightDecode, "in-flight decode"),
    (CpuLedgerCategory::MetadataAndIndexes, "metadata/indexes"),
    (CpuLedgerCategory::QueuesAndResults, "queues/results"),
    (CpuLedgerCategory::Prefetch, "prefetch"),
    (CpuLedgerCategory::ImportWorkingSet, "import working set"),
];

pub(crate) fn runtime_diagnostics_view(app: &MiranteWorkbenchApp) -> RuntimeDiagnosticsView {
    #[cfg(test)]
    app.runtime_diagnostics_collections
        .set(app.runtime_diagnostics_collections.get().saturating_add(1));
    let snapshot = app.application.snapshot();
    let mut rows = Vec::new();
    rows.push((
        "logs".to_owned(),
        app.startup_diagnostics
            .logs_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "stderr/stdout".to_owned()),
    ));
    rows.push((
        "source".to_owned(),
        app.dataset.selected_path().display().to_string(),
    ));
    rows.push((
        "selected GPU memory".to_owned(),
        format!(
            "{} {:?} {:?}, {} {}, device-local {}, driver budget {}, driver usage {}, failure {}",
            app.selected_adapter_memory.adapter_name(),
            app.selected_adapter_memory.backend(),
            app.selected_adapter_memory.device_type(),
            app.selected_adapter_memory.memory_model(),
            app.selected_adapter_memory.source(),
            app.selected_adapter_memory
                .device_local_bytes()
                .map_or_else(|| "unknown".to_owned(), |bytes| bytes.to_string()),
            app.selected_adapter_memory
                .driver_budget_bytes()
                .map_or_else(|| "unknown".to_owned(), |bytes| bytes.to_string()),
            app.selected_adapter_memory
                .driver_usage_bytes()
                .map_or_else(|| "unknown".to_owned(), |bytes| bytes.to_string()),
            app.selected_adapter_memory
                .failure()
                .map_or_else(|| "none".to_owned(), ToString::to_string),
        ),
    ));
    let camera_demand = app.dataset.visible_demand_diagnostics();
    rows.push((
        "camera demand".to_owned(),
        format!(
            "{} submitted, {} completed, {} guarded / {} exact rebuilds, {} contained reuses, {} pending replacements, {} running cancellations, {} stale suppressed; {} reevaluated / {} reused candidates, {} candidates on UI, last exact {} + guard {} resources, pending bound {}",
            camera_demand.submitted,
            camera_demand.completed,
            camera_demand.completed_guarded_rebuilds,
            camera_demand.completed_exact_rebuilds,
            camera_demand.contained_reuses,
            camera_demand.pending_replacements,
            camera_demand.cancelled_running,
            camera_demand.stale_results_suppressed,
            camera_demand.completed_candidates_visited,
            camera_demand.candidates_reused,
            camera_demand.ui_thread_candidates_visited,
            camera_demand.last_primary_resources,
            camera_demand.last_guard_resources,
            camera_demand.maximum_pending_requests,
        ),
    ));
    rows.push((
        "camera demand time".to_owned(),
        format!(
            "{:.2} ms completed total / {:.2} ms last; {:.2} ms cancelled, {:.2} ms stale",
            camera_demand.completed_planning_time_ns as f64 / 1_000_000.0,
            camera_demand.last_completed_planning_time_ns as f64 / 1_000_000.0,
            camera_demand.cancelled_planning_time_ns as f64 / 1_000_000.0,
            camera_demand.stale_planning_time_ns as f64 / 1_000_000.0,
        ),
    ));

    match app.dataset.dispatcher().diagnostics() {
        Ok(diagnostics) => {
            rows.push((
                "dataset CPU".to_owned(),
                format!(
                    "{} / {} bytes",
                    diagnostics.total_used_bytes(),
                    diagnostics.total_cap_bytes()
                ),
            ));
            for (category, label) in CPU_CATEGORIES {
                rows.push((
                    label.to_owned(),
                    format!(
                        "{} / {} bytes",
                        diagnostics.category_used_bytes(category),
                        diagnostics.category_cap_bytes(category)
                    ),
                ));
            }
            rows.push((
                "requests".to_owned(),
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
            ));
            rows.push((
                "queue bounds".to_owned(),
                format!(
                    "requests {}, completions {}, workers {}",
                    diagnostics.request_queue_limit(),
                    diagnostics.completion_queue_limit(),
                    diagnostics.worker_limit()
                ),
            ));
            rows.push((
                "resident resources".to_owned(),
                diagnostics.resident_resources().to_string(),
            ));
            let performance = diagnostics.performance();
            rows.push((
                "dataset performance".to_owned(),
                format!(
                    "cache {} hit / {} miss / {} evicted; {} cohorts / {} members / {} peak; queue {:.2} ms, decode {:.2} ms / {} bytes, cancelled waste {} decodes / {:.2} ms / {} bytes, {} progress updates",
                    performance.cache_hits(),
                    performance.cache_misses(),
                    performance.cache_evictions(),
                    performance.decode_cohorts(),
                    performance.decode_cohort_members(),
                    performance.peak_decode_cohort_members(),
                    performance.queue_wait_ns() as f64 / 1_000_000.0,
                    performance.decode_time_ns() as f64 / 1_000_000.0,
                    performance.decoded_output_bytes(),
                    performance.cancelled_decode_executions(),
                    performance.cancelled_decode_time_ns() as f64 / 1_000_000.0,
                    performance.cancelled_decode_bytes(),
                    performance.progress_updates(),
                ),
            ));
        }
        Err(error) => rows.push(("dataset runtime".to_owned(), error.to_string())),
    }
    if let Some(diagnostics) = app.dataset.local_source_diagnostics() {
        rows.push((
            "physical brick cache".to_owned(),
            format!(
                "{} hit / {} miss / {} wait / {} evicted; {} entries, {} / {} peak bytes",
                diagnostics.physical_brick_cache_hits,
                diagnostics.physical_brick_cache_misses,
                diagnostics.physical_brick_cache_waits,
                diagnostics.physical_brick_cache_evictions,
                diagnostics.physical_brick_cache_entries,
                diagnostics.physical_brick_cache_bytes,
                diagnostics.physical_brick_cache_peak_bytes,
            ),
        ));
        rows.push((
            "source I/O".to_owned(),
            format!(
                "{} ranges / {} encoded bytes; {} codec decodes / {} bytes / {:.2} ms; currentness {} pre / {} post / {:.2} ms; {} unique bricks / {} bytes",
                diagnostics.reader.physical_range_read_operations,
                diagnostics.reader.physical_encoded_bytes_read,
                diagnostics.reader.codec_decode_operations,
                diagnostics.reader.codec_decoded_bytes,
                diagnostics.reader.codec_decode_time_ns as f64 / 1_000_000.0,
                diagnostics.reader.currentness_pre_use_batches,
                diagnostics.reader.currentness_post_use_batches,
                diagnostics.reader.currentness_time_ns as f64 / 1_000_000.0,
                diagnostics.physical_brick_unique_decodes,
                diagnostics.physical_brick_unique_decoded_bytes,
            ),
        ));
        rows.push((
            "aligned direct decode".to_owned(),
            format!(
                "{} deliveries / {} streamed bytes; {} direct-span bytes; {} post-decode copy bytes",
                diagnostics.aligned_direct_deliveries,
                diagnostics.aligned_direct_streamed_bytes,
                diagnostics.aligned_direct_sink_span_bytes,
                diagnostics.aligned_direct_post_decode_copy_bytes,
            ),
        ));
    }

    rows.push((
        "renderer leases".to_owned(),
        format!(
            "{} / {} CPU-retained, {} CPU-absent",
            app.dataset.retained_leases().retained_len(),
            app.dataset.retained_leases().required_len(),
            app.dataset.retained_leases().missing_len(),
        ),
    ));
    rows.push((
        "LOD".to_owned(),
        format!(
            "shown {:?}, uniform target {:?}",
            app.render_coordination.frame_fidelity.displayed_scale_level,
            app.render_coordination.frame_fidelity.target_scale_level,
        ),
    ));
    for (index, candidate) in app
        .volume_presentation
        .latest_candidate_facts()
        .iter()
        .enumerate()
    {
        let layer_scales = candidate
            .layer_scales
            .iter()
            .map(|(layer, scale)| format!("l{}=s{}", layer.ordinal(), scale.get()))
            .collect::<Vec<_>>()
            .join(", ");
        rows.push((
            format!("3D candidate {index} {}", candidate.kind.label()),
            format!(
                "[{layer_scales}]; {}; {} shared / {} total schedule work units; {} resources / {} payload bytes; resident {}; target-eligible {}; interaction-safe {}; full-volume {}; {}",
                candidate.kernel.label(),
                candidate.shared_work_units,
                candidate.schedule_work_units,
                candidate.resource_count,
                candidate.payload_bytes,
                candidate.complete_and_resident,
                candidate.target_quality_eligible,
                candidate.interaction_safe,
                candidate.full_volume,
                candidate.disposition.label(),
            ),
        ));
        for layer in &candidate.layer_work {
            rows.push((
                format!(
                    "3D candidate {index} layer {} s{}",
                    layer.layer.ordinal(),
                    layer.scale.get()
                ),
                format!(
                    "{:?}/{:?}; projected {} px × {} steps; scheduled {} px × {} steps; {} taps/step + {} gradient taps/ray; ray {} + schedule {} + terminal {} = {} work units",
                    layer.mode,
                    layer.sampling,
                    layer.projected_pixels,
                    layer.traversal_step_bound,
                    layer.scheduled_pixels,
                    layer.scheduled_step_bound,
                    layer.sample_taps_per_step,
                    layer.gradient_taps_per_ray,
                    layer.ray_setup_work_units,
                    layer.scheduled_work_units,
                    layer.terminal_work_units,
                    layer.total_work_units(),
                ),
            ));
        }
    }
    let milestones = app.display_performance_milestones;
    rows.push((
        "display milestones".to_owned(),
        format!(
            "generation {}; current {}, useful {}, replacement {}, settled {}",
            milestones.generation(),
            optional_ms(milestones.first_current_presented_ms()),
            optional_ms(milestones.first_useful_frame_ms()),
            optional_ms(milestones.complete_replacement_ms()),
            optional_ms(milestones.target_settled_ms()),
        ),
    ));
    rows.push((
        "active 2D panel".to_owned(),
        snapshot
            .transient()
            .active_cross_section_panel()
            .map(PanelId::from_application_panel)
            .map(|panel| panel.label().to_owned())
            .unwrap_or_else(|| "none".to_owned()),
    ));
    for (slot, panel) in app.render_coordination.iter() {
        if slot.is_cross_section() {
            let panel_id = PanelId::from_presentation_slot(slot);
            rows.push((format!("2D {}", panel_id.label()), panel_summary(panel)));
        }
    }
    if let Some(product) = app.native_presentation.product_gpu.as_ref() {
        rows.push((
            "renderer pipelines".to_owned(),
            product.renderer.pipeline_readiness().map_or_else(
                |error| format!("failed: {error}"),
                |state| format!("{state:?}"),
            ),
        ));
        let diagnostics = product.renderer.diagnostics();
        rows.push((
            "GPU residency".to_owned(),
            format!(
                "{} resident / {} committed / {} logical bytes ({} physical buffer bytes, {} uncommitted), {} growths / {} copied bytes; {} frames, {} submissions",
                diagnostics.resident_payload_bytes(),
                diagnostics.payload_committed_capacity_bytes(),
                diagnostics.payload_capacity_bytes(),
                diagnostics.payload_arena_allocated_bytes(),
                diagnostics.payload_uncommitted_capacity_bytes(),
                diagnostics.payload_growths(),
                diagnostics.payload_growth_copy_bytes(),
                diagnostics.frames_executed(),
                diagnostics.queue_submissions(),
            ),
        ));
        rows.push((
            "hidden exact".to_owned(),
            format!(
                "{} started / {} completed / {} cancelled / {} failed; {} batches / {} rows / {:.2} ms, last batch {} rows",
                diagnostics.hidden_refinement_jobs_started(),
                diagnostics.hidden_refinement_jobs_completed(),
                diagnostics.hidden_refinement_jobs_cancelled(),
                diagnostics.hidden_refinement_jobs_failed(),
                diagnostics.hidden_refinement_batches(),
                diagnostics.hidden_refinement_rows(),
                diagnostics.hidden_refinement_elapsed_ns() as f64 / 1_000_000.0,
                diagnostics
                    .hidden_refinement_last_batch_rows()
                    .map_or_else(|| "none".to_owned(), |rows| rows.to_string()),
            ),
        ));
        rows.push((
            "GPU placeability".to_owned(),
            format!(
                "{} aggregate free / {} largest contiguous bytes; {} refusals, {} compactions ({} resources / {} bytes moved)",
                diagnostics.payload_free_bytes(),
                diagnostics.payload_largest_contiguous_bytes(),
                diagnostics.payload_placeability_failures(),
                diagnostics.payload_compactions(),
                diagnostics.payload_compaction_resources_moved(),
                diagnostics.payload_compaction_bytes_moved(),
            ),
        ));
        rows.push((
            "GPU transfers".to_owned(),
            format!(
                "{} resources / {} payload bytes; {} render-thread fact-scan bytes; {} padding-zero bytes",
                diagnostics.uploaded_resources(),
                diagnostics.uploaded_payload_bytes(),
                diagnostics.render_thread_payload_fact_scan_bytes(),
                diagnostics.upload_staging_padding_zero_bytes(),
            ),
        ));
        rows.push((
            "GPU directory".to_owned(),
            format!(
                "{} publications / {} mutations / {} rebuilds; {} slot writes / {} page-record writes",
                diagnostics.directory_publications(),
                diagnostics.directory_mutations(),
                diagnostics.directory_rebuilds(),
                diagnostics.directory_slot_writes(),
                diagnostics.page_record_writes(),
            ),
        ));
        rows.push((
            "GPU target control".to_owned(),
            format!(
                "{} updates / {} bytes",
                diagnostics.target_control_updates(),
                diagnostics.target_control_upload_bytes(),
            ),
        ));
        rows.push((
            "progressive frames".to_owned(),
            format!(
                "{} partial, {} settled, {} stale rejected",
                product.current_partial_frames_presented,
                product.partial_to_settled_transitions,
                product.stale_frames_rejected,
            ),
        ));
    }
    if let Some(timing) = app.render_coordination.last_display_refresh_timing {
        rows.push((
            "display timing".to_owned(),
            format!(
                "{}: 3D render {:.2} ms, GPU upload {}, GPU compute {}, whole refresh {:.2} ms",
                crate::display_refresh::display_refresh_path_label(timing.path),
                timing.render_ms,
                optional_ms(timing.gpu_upload_ms),
                optional_ms(timing.gpu_compute_ms),
                timing.total_ms
            ),
        ));
    }
    RuntimeDiagnosticsView::new(rows, app.render_coordination.frame_fidelity.clone())
}

pub(crate) fn runtime_diagnostics_view_if_requested(
    app: &MiranteWorkbenchApp,
    requested: bool,
) -> Option<RuntimeDiagnosticsView> {
    collect_if_requested(requested, || runtime_diagnostics_view(app))
}

fn collect_if_requested<T>(requested: bool, collect: impl FnOnce() -> T) -> Option<T> {
    requested.then(collect)
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
            let performance = diagnostics.performance();
            text.push_str(&format!(
                "dataset_cache_hits: {}\n\
                 dataset_cache_misses: {}\n\
                 dataset_cache_evictions: {}\n\
                 dataset_progress_updates: {}\n\
                 dataset_decode_cohorts: {}\n\
                 dataset_decode_cohort_members: {}\n\
                 dataset_peak_decode_cohort_members: {}\n\
                 dataset_queue_wait_ns: {}\n\
                 dataset_decode_time_ns: {}\n\
                 dataset_decoded_output_bytes: {}\n\
                 dataset_cancelled_decode_executions: {}\n\
                 dataset_cancelled_decode_time_ns: {}\n\
                 dataset_cancelled_decode_bytes: {}\n",
                performance.cache_hits(),
                performance.cache_misses(),
                performance.cache_evictions(),
                performance.progress_updates(),
                performance.decode_cohorts(),
                performance.decode_cohort_members(),
                performance.peak_decode_cohort_members(),
                performance.queue_wait_ns(),
                performance.decode_time_ns(),
                performance.decoded_output_bytes(),
                performance.cancelled_decode_executions(),
                performance.cancelled_decode_time_ns(),
                performance.cancelled_decode_bytes(),
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
    if let Some(diagnostics) = app.dataset.local_source_diagnostics() {
        text.push_str(&format!(
            "source_physical_brick_requests: {}\n\
             source_physical_brick_cache_hits: {}\n\
             source_physical_brick_cache_misses: {}\n\
             source_physical_brick_cache_waits: {}\n\
             source_physical_brick_cache_evictions: {}\n\
             source_physical_brick_cache_capacity_bypasses: {}\n\
             source_physical_brick_unique_decodes: {}\n\
             source_physical_brick_unique_decoded_bytes: {}\n\
             source_aligned_direct_deliveries: {}\n\
             source_aligned_direct_streamed_bytes: {}\n\
             source_aligned_direct_sink_span_bytes: {}\n\
             source_aligned_direct_post_decode_copy_bytes: {}\n\
             source_physical_brick_cache_entries: {}\n\
             source_physical_brick_cache_bytes: {}\n\
             source_physical_brick_cache_peak_bytes: {}\n\
             source_contiguous_copy_bytes: {}\n\
             source_scalar_copy_samples: {}\n\
             source_sink_write_bytes: {}\n\
             source_object_open_operations: {}\n\
             source_object_open_time_ns: {}\n\
             source_object_handle_cache_hits: {}\n\
             source_object_handle_cache_misses: {}\n\
             source_object_handle_cache_evictions: {}\n\
             source_shard_index_cache_hits: {}\n\
             source_shard_index_cache_misses: {}\n\
             source_shard_index_decode_operations: {}\n\
             source_packed_inner_cache_hits: {}\n\
             source_packed_inner_cache_misses: {}\n\
             source_currentness_pre_use_batches: {}\n\
             source_currentness_post_use_batches: {}\n\
             source_currentness_snapshot_batches: {}\n\
             source_currentness_root_metadata_checks: {}\n\
             source_currentness_named_object_resolutions: {}\n\
             source_currentness_object_fd_metadata_checks: {}\n\
             source_currentness_time_ns: {}\n\
             source_physical_range_read_operations: {}\n\
             source_physical_encoded_bytes_read: {}\n\
             source_physical_range_read_time_ns: {}\n\
             source_codec_decode_operations: {}\n\
             source_codec_decoded_bytes: {}\n\
             source_codec_decode_time_ns: {}\n",
            diagnostics.physical_brick_requests,
            diagnostics.physical_brick_cache_hits,
            diagnostics.physical_brick_cache_misses,
            diagnostics.physical_brick_cache_waits,
            diagnostics.physical_brick_cache_evictions,
            diagnostics.physical_brick_cache_capacity_bypasses,
            diagnostics.physical_brick_unique_decodes,
            diagnostics.physical_brick_unique_decoded_bytes,
            diagnostics.aligned_direct_deliveries,
            diagnostics.aligned_direct_streamed_bytes,
            diagnostics.aligned_direct_sink_span_bytes,
            diagnostics.aligned_direct_post_decode_copy_bytes,
            diagnostics.physical_brick_cache_entries,
            diagnostics.physical_brick_cache_bytes,
            diagnostics.physical_brick_cache_peak_bytes,
            diagnostics.contiguous_copy_bytes,
            diagnostics.scalar_copy_samples,
            diagnostics.sink_write_bytes,
            diagnostics.reader.object_open_operations,
            diagnostics.reader.object_open_time_ns,
            diagnostics.reader.object_handle_cache_hits,
            diagnostics.reader.object_handle_cache_misses,
            diagnostics.reader.object_handle_cache_evictions,
            diagnostics.reader.shard_index_cache_hits,
            diagnostics.reader.shard_index_cache_misses,
            diagnostics.reader.shard_index_decode_operations,
            diagnostics.reader.packed_inner_cache_hits,
            diagnostics.reader.packed_inner_cache_misses,
            diagnostics.reader.currentness_pre_use_batches,
            diagnostics.reader.currentness_post_use_batches,
            diagnostics.reader.currentness_snapshot_batches,
            diagnostics.reader.currentness_root_metadata_checks,
            diagnostics.reader.currentness_named_object_resolutions,
            diagnostics.reader.currentness_object_fd_metadata_checks,
            diagnostics.reader.currentness_time_ns,
            diagnostics.reader.physical_range_read_operations,
            diagnostics.reader.physical_encoded_bytes_read,
            diagnostics.reader.physical_range_read_time_ns,
            diagnostics.reader.codec_decode_operations,
            diagnostics.reader.codec_decoded_bytes,
            diagnostics.reader.codec_decode_time_ns,
        ));
    }
    text.push_str(&format!(
        "renderer_required_leases: {}\n\
         renderer_retained_leases: {}\n\
         renderer_cpu_absent_leases: {}\n\
         current_uniform_scale_level: {:?}\n",
        app.dataset.retained_leases().required_len(),
        app.dataset.retained_leases().retained_len(),
        app.dataset.retained_leases().missing_len(),
        app.dataset
            .current_uniform_scale()
            .map(mirante4d_domain::ScaleLevel::get),
    ));
    for (slot, panel) in app.render_coordination.iter() {
        if let Some(schedule) = panel.cross_section_schedule() {
            let panel_id = PanelId::from_presentation_slot(slot);
            text.push_str(&format!(
                "cross_section_{}_generation: {}\n\
                 cross_section_{}_displayed_generation: {}\n\
                 cross_section_{}_status: {:?}\n\
                 cross_section_{}_required: {}\n\
                 cross_section_{}_retained: {}\n\
                 cross_section_{}_missing: {}\n",
                panel_id.label(),
                panel.generation(),
                panel_id.label(),
                panel
                    .displayed_generation()
                    .map_or_else(|| "none".to_owned(), |value| value.to_string()),
                panel_id.label(),
                schedule.status,
                panel_id.label(),
                schedule.selected_bricks,
                panel_id.label(),
                schedule.occupied_selected_bricks,
                panel_id.label(),
                schedule.missing_occupied_bricks,
            ));
        }
    }
    let milestones = app.display_performance_milestones;
    text.push_str(&format!(
        "display_input_generation: {}\n\
         display_first_current_presented_ms: {}\n\
         display_first_useful_frame_ms: {}\n\
         display_complete_replacement_ms: {}\n\
         display_target_settled_ms: {}\n",
        milestones.generation(),
        optional_ms(milestones.first_current_presented_ms()),
        optional_ms(milestones.first_useful_frame_ms()),
        optional_ms(milestones.complete_replacement_ms()),
        optional_ms(milestones.target_settled_ms()),
    ));
    if let Some(product) = app.native_presentation.product_gpu.as_ref() {
        let diagnostics = product.renderer.diagnostics();
        text.push_str(&format!(
            "renderer_pipeline_readiness: {}\n\
             gpu_uploaded_resources: {}\n\
             gpu_uploaded_payload_bytes: {}\n\
             gpu_render_thread_payload_fact_scan_bytes: {}\n\
             gpu_upload_staging_padding_zero_bytes: {}\n\
             gpu_directory_publications: {}\n\
             gpu_directory_mutations: {}\n\
             gpu_directory_rebuilds: {}\n\
             gpu_directory_slot_writes: {}\n\
             gpu_page_record_writes: {}\n\
             gpu_target_control_updates: {}\n\
             gpu_target_control_upload_bytes: {}\n",
            product.renderer.pipeline_readiness().map_or_else(
                |error| format!("failed: {error}"),
                |state| format!("{state:?}")
            ),
            diagnostics.uploaded_resources(),
            diagnostics.uploaded_payload_bytes(),
            diagnostics.render_thread_payload_fact_scan_bytes(),
            diagnostics.upload_staging_padding_zero_bytes(),
            diagnostics.directory_publications(),
            diagnostics.directory_mutations(),
            diagnostics.directory_rebuilds(),
            diagnostics.directory_slot_writes(),
            diagnostics.page_record_writes(),
            diagnostics.target_control_updates(),
            diagnostics.target_control_upload_bytes(),
        ));
    }
    text
}

fn panel_summary(panel: &RenderSurfaceState) -> String {
    let Some(schedule) = panel.cross_section_schedule() else {
        return format!("generation {}, no schedule", panel.generation());
    };
    format!(
        "{:?}, s{:?}, {}/{} retained, {} missing, generation {}/{}",
        schedule.status,
        schedule.render_scale_level,
        schedule.occupied_selected_bricks,
        schedule.selected_bricks,
        schedule.missing_occupied_bricks,
        panel
            .displayed_generation()
            .map_or_else(|| "none".to_owned(), |value| value.to_string()),
        panel.generation(),
    )
}

fn optional_ms(value: Option<f64>) -> String {
    value
        .map(|milliseconds| format!("{milliseconds:.2} ms"))
        .unwrap_or_else(|| "unavailable".to_owned())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::collect_if_requested;

    #[test]
    fn collapsed_frames_collect_zero_runtime_diagnostics() {
        let collections = Cell::new(0_u64);
        for _ in 0..256 {
            assert_eq!(
                collect_if_requested(false, || {
                    collections.set(collections.get() + 1);
                    1
                }),
                None
            );
        }
        assert_eq!(collections.get(), 0);

        assert_eq!(
            collect_if_requested(true, || {
                collections.set(collections.get() + 1);
                1
            }),
            Some(1)
        );
        assert_eq!(collections.get(), 1);
    }
}
