use mirante4d_dataset::CpuLedgerCategory;
use mirante4d_dataset_runtime::DatasetRuntimeDiagnostics;
use mirante4d_render_wgpu::WgpuRenderRuntimeDiagnostics;
use mirante4d_storage::{LocalDatasetSourceDiagnostics, LocalPackageReadDiagnostics};
use serde_json::{Value, json};

pub(crate) fn dataset_runtime_diagnostics_json(diagnostics: DatasetRuntimeDiagnostics) -> Value {
    let performance = diagnostics.performance();
    json!({
        "capacity": {
            "total_cpu_bytes": diagnostics.total_cap_bytes(),
            "worker_limit": diagnostics.worker_limit(),
            "request_queue_limit": diagnostics.request_queue_limit(),
            "completion_queue_limit": diagnostics.completion_queue_limit(),
            "category_bytes": category_bytes_json(diagnostics, true),
        },
        "used": {
            "total_cpu_bytes": diagnostics.total_used_bytes(),
            "category_bytes": category_bytes_json(diagnostics, false),
        },
        "work": {
            "queued_requests": diagnostics.queued_requests(),
            "in_flight_decodes": diagnostics.in_flight_decodes(),
            "pending_completions": diagnostics.pending_completions(),
            "resident_resources": diagnostics.resident_resources(),
        },
        "counters": {
            "submitted_requests": diagnostics.submitted_requests(),
            "started_decodes": diagnostics.started_decodes(),
            "completed_decodes": diagnostics.completed_decodes(),
            "ready_requests": diagnostics.ready_requests(),
            "cancelled_requests": diagnostics.cancelled_requests(),
            "failed_requests": diagnostics.failed_requests(),
            "decode_cohorts": performance.decode_cohorts(),
            "decode_cohort_members": performance.decode_cohort_members(),
            "peak_decode_cohort_members": performance.peak_decode_cohort_members(),
        },
        "performance": {
            "cache_hits": performance.cache_hits(),
            "cache_misses": performance.cache_misses(),
            "cache_evictions": performance.cache_evictions(),
            "progress_updates": performance.progress_updates(),
            "queue_wait_ns": performance.queue_wait_ns(),
            "decode_time_ns": performance.decode_time_ns(),
            "decoded_output_bytes": performance.decoded_output_bytes(),
            "cancelled_decode_executions": performance.cancelled_decode_executions(),
            "cancelled_decode_time_ns": performance.cancelled_decode_time_ns(),
            "cancelled_decode_bytes": performance.cancelled_decode_bytes(),
        },
    })
}

pub(crate) fn local_dataset_source_diagnostics_json(
    diagnostics: LocalDatasetSourceDiagnostics,
) -> Value {
    json!({
        "physical_bricks": {
            "requests": diagnostics.physical_brick_requests,
            "cache_hits": diagnostics.physical_brick_cache_hits,
            "cache_misses": diagnostics.physical_brick_cache_misses,
            "cache_waits": diagnostics.physical_brick_cache_waits,
            "cache_evictions": diagnostics.physical_brick_cache_evictions,
            "cache_capacity_bypasses": diagnostics.physical_brick_cache_capacity_bypasses,
            "unique_decodes": diagnostics.physical_brick_unique_decodes,
            "unique_decoded_bytes": diagnostics.physical_brick_unique_decoded_bytes,
            "cache_entries": diagnostics.physical_brick_cache_entries,
            "cache_bytes": diagnostics.physical_brick_cache_bytes,
            "cache_peak_bytes": diagnostics.physical_brick_cache_peak_bytes,
        },
        "copy": {
            "contiguous_bytes": diagnostics.contiguous_copy_bytes,
            "scalar_samples": diagnostics.scalar_copy_samples,
            "sink_write_bytes": diagnostics.sink_write_bytes,
        },
        "aligned_direct": {
            "deliveries": diagnostics.aligned_direct_deliveries,
            "streamed_bytes": diagnostics.aligned_direct_streamed_bytes,
            "sink_span_bytes": diagnostics.aligned_direct_sink_span_bytes,
            "post_decode_copy_bytes": diagnostics.aligned_direct_post_decode_copy_bytes,
        },
        "reader": {
            "current_open_object_handles": diagnostics.reader.open_object_handles_current,
            "peak_open_object_handles": diagnostics.reader.open_object_handles_peak,
            "open_object_handle_gauge": {
                "available": true,
                "scope": "active_reader_root_cached_and_transient_object_descriptors",
                "current": diagnostics.reader.open_object_handles_current,
                "peak": diagnostics.reader.open_object_handles_peak,
                "retained_cache_current": diagnostics.reader.object_handle_cache_entries,
                "retained_cache_peak": diagnostics.reader.object_handle_cache_peak_entries,
                "operation_counts_used_as_concurrency": false,
            },
            "object_open_operations": diagnostics.reader.object_open_operations,
            "object_open_time_ns": diagnostics.reader.object_open_time_ns,
            "object_handle_cache_hits": diagnostics.reader.object_handle_cache_hits,
            "object_handle_cache_misses": diagnostics.reader.object_handle_cache_misses,
            "object_handle_cache_evictions": diagnostics.reader.object_handle_cache_evictions,
            "object_handle_cache_lock_acquisitions": diagnostics.reader.object_handle_cache_lock_acquisitions,
            "object_handle_cache_lock_contentions": diagnostics.reader.object_handle_cache_lock_contentions,
            "object_handle_cache_lock_wait_time_ns": diagnostics.reader.object_handle_cache_lock_wait_time_ns,
            "shard_index_cache_hits": diagnostics.reader.shard_index_cache_hits,
            "shard_index_cache_misses": diagnostics.reader.shard_index_cache_misses,
            "shard_index_decode_operations": diagnostics.reader.shard_index_decode_operations,
            "packed_inner_cache_hits": diagnostics.reader.packed_inner_cache_hits,
            "packed_inner_cache_misses": diagnostics.reader.packed_inner_cache_misses,
            "currentness": {
                "pre_use_batches": diagnostics.reader.currentness_pre_use_batches,
                "post_use_batches": diagnostics.reader.currentness_post_use_batches,
                "snapshot_batches": diagnostics.reader.currentness_snapshot_batches,
                "root_metadata_checks": diagnostics.reader.currentness_root_metadata_checks,
                "named_object_resolutions": diagnostics.reader.currentness_named_object_resolutions,
                "object_fd_metadata_checks": diagnostics.reader.currentness_object_fd_metadata_checks,
                "time_ns": diagnostics.reader.currentness_time_ns,
            },
            "physical_range_read_operations": diagnostics.reader.physical_range_read_operations,
            "physical_encoded_bytes_read": diagnostics.reader.physical_encoded_bytes_read,
            "physical_range_read_time_ns": diagnostics.reader.physical_range_read_time_ns,
            "cancelled_encoded_bytes": {
                "available": false,
                "reason": "physical_range_cohort_has_no_per_sink_cancellation_ownership",
            },
            "codec_decode_operations": diagnostics.reader.codec_decode_operations,
            "codec_decoded_bytes": diagnostics.reader.codec_decoded_bytes,
            "codec_decode_time_ns": diagnostics.reader.codec_decode_time_ns,
        },
    })
}

pub(crate) fn local_package_read_diagnostics_json(
    diagnostics: LocalPackageReadDiagnostics,
) -> Value {
    json!({
        "current_open_object_handles": diagnostics.open_object_handles_current,
        "peak_open_object_handles": diagnostics.open_object_handles_peak,
        "object_handle_cache_current": diagnostics.object_handle_cache_entries,
        "object_handle_cache_peak": diagnostics.object_handle_cache_peak_entries,
        "object_open_operations": diagnostics.object_open_operations,
        "object_open_time_ns": diagnostics.object_open_time_ns,
        "object_handle_cache_hits": diagnostics.object_handle_cache_hits,
        "object_handle_cache_misses": diagnostics.object_handle_cache_misses,
        "object_handle_cache_evictions": diagnostics.object_handle_cache_evictions,
        "object_handle_cache_lock_acquisitions": diagnostics.object_handle_cache_lock_acquisitions,
        "object_handle_cache_lock_contentions": diagnostics.object_handle_cache_lock_contentions,
        "object_handle_cache_lock_wait_time_ns": diagnostics.object_handle_cache_lock_wait_time_ns,
        "shard_index_cache_hits": diagnostics.shard_index_cache_hits,
        "shard_index_cache_misses": diagnostics.shard_index_cache_misses,
        "shard_index_decode_operations": diagnostics.shard_index_decode_operations,
        "packed_inner_cache_hits": diagnostics.packed_inner_cache_hits,
        "packed_inner_cache_misses": diagnostics.packed_inner_cache_misses,
        "currentness": {
            "pre_use_batches": diagnostics.currentness_pre_use_batches,
            "post_use_batches": diagnostics.currentness_post_use_batches,
            "snapshot_batches": diagnostics.currentness_snapshot_batches,
            "root_metadata_checks": diagnostics.currentness_root_metadata_checks,
            "named_object_resolutions": diagnostics.currentness_named_object_resolutions,
            "object_fd_metadata_checks": diagnostics.currentness_object_fd_metadata_checks,
            "time_ns": diagnostics.currentness_time_ns,
        },
        "physical_range_read_operations": diagnostics.physical_range_read_operations,
        "physical_encoded_bytes_read": diagnostics.physical_encoded_bytes_read,
        "physical_range_read_time_ns": diagnostics.physical_range_read_time_ns,
        "codec_decode_operations": diagnostics.codec_decode_operations,
        "codec_decoded_bytes": diagnostics.codec_decoded_bytes,
        "codec_decode_time_ns": diagnostics.codec_decode_time_ns,
    })
}

fn category_bytes_json(diagnostics: DatasetRuntimeDiagnostics, capacity: bool) -> Value {
    let bytes = |category| {
        if capacity {
            diagnostics.category_cap_bytes(category)
        } else {
            diagnostics.category_used_bytes(category)
        }
    };
    json!({
        "decoded_residency": bytes(CpuLedgerCategory::DecodedResidency),
        "upload_staging": bytes(CpuLedgerCategory::UploadStaging),
        "in_flight_decode": bytes(CpuLedgerCategory::InFlightDecode),
        "metadata_and_indexes": bytes(CpuLedgerCategory::MetadataAndIndexes),
        "queues_and_results": bytes(CpuLedgerCategory::QueuesAndResults),
        "prefetch": bytes(CpuLedgerCategory::Prefetch),
        "import_working_set": bytes(CpuLedgerCategory::ImportWorkingSet),
    })
}

pub(crate) fn gpu_adapter_diagnostics_json(adapter: &WgpuRenderRuntimeDiagnostics) -> Value {
    let mut requested_features = Vec::new();
    if adapter.gpu_timestamps_supported() {
        requested_features.push("TIMESTAMP_QUERY");
    }
    if adapter.gpu_payload_copy_timestamps_supported() {
        requested_features.push("TIMESTAMP_QUERY_INSIDE_ENCODERS");
    }
    let timing = json!({
        "timestamps_supported": adapter.gpu_timestamps_supported(),
        "enabled": adapter.gpu_timing_enabled(),
        "payload_copy_timestamps_supported": adapter.gpu_payload_copy_timestamps_supported(),
        "completed": adapter.completed_gpu_timings(),
        "failures": adapter.gpu_timing_failures(),
        "gpu_timing_prelude_submissions": adapter.gpu_timing_prelude_submissions(),
        "last_batch_gpu_envelope_ns": adapter.last_gpu_batch_envelope_ns(),
        "last_payload_copy_ns": adapter.last_gpu_payload_copy_ns(),
        "last_render_pass_ns": adapter.last_gpu_render_pass_ns(),
        "cpu": {
            "measurement_scope": "retained_cohort_preflight_validation_residency_control_and_command_encoding_excluding_queue_submit",
            "completed": adapter.completed_cpu_timings(),
            "last_frame": adapter.last_cpu_timing_frame(),
            "last_planning_ns": adapter.last_cpu_planning_ns(),
            "last_control_publication_ns": adapter.last_cpu_control_publication_ns(),
            "last_payload_staging_ns": adapter.last_cpu_payload_staging_ns(),
            "last_queue_submit_ns": adapter.last_cpu_queue_submit_ns(),
            "total_planning_ns": adapter.total_cpu_planning_ns(),
            "total_control_publication_ns": adapter.total_cpu_control_publication_ns(),
            "total_payload_staging_ns": adapter.total_cpu_payload_staging_ns(),
            "total_queue_submit_ns": adapter.total_cpu_queue_submit_ns(),
        },
    });
    json!({
        "name": adapter.adapter_name(),
        "backend": adapter.backend(),
        "driver": adapter.driver(),
        "identity": {
            "adapter_name": adapter.adapter_name(),
            "backend": adapter.backend(),
            "vendor_id": adapter.vendor_id(),
            "device_id": adapter.device_id(),
            "device_type": adapter.device_type(),
            "driver_name": adapter.driver_name(),
            "driver_info": adapter.driver_info(),
            "source": "wgpu_adapter_info_for_exact_product_device",
        },
        "device_contract": {
            "requested_features": requested_features,
            "memory_hint": "MemoryUsage",
            "source": "enabled_device_features_and_fixed_renderer_device_descriptor",
        },
        "limits": {
            "max_buffer_size": adapter.max_buffer_size_bytes(),
            "max_storage_buffer_binding_size": adapter.max_storage_buffer_binding_size_bytes(),
            "max_storage_buffers_per_shader_stage": adapter.max_storage_buffers_per_shader_stage(),
        },
        "gpu_budget_bytes": adapter.gpu_budget_bytes(),
        "payload_capacity_bytes": adapter.payload_capacity_bytes(),
        "transfer_capacity_bytes": adapter.transfer_capacity_bytes(),
        "other_capacity_bytes": adapter.other_capacity_bytes(),
        "payload_arena_allocated_bytes": adapter.payload_arena_allocated_bytes(),
        "resident_payload_bytes": adapter.resident_payload_bytes(),
        "peak_resident_payload_bytes": adapter.peak_resident_payload_bytes(),
        "resident_metadata": {
            "records": adapter.empty_resident_metadata_records(),
            "capacity_records": adapter.empty_resident_metadata_capacity_records(),
            "bytes": adapter.empty_resident_metadata_bytes(),
            "bytes_per_record": adapter.empty_resident_metadata_bytes_per_record(),
            "peak_bytes": adapter.peak_empty_resident_metadata_bytes(),
        },
        "frames_executed": adapter.frames_executed(),
        "queue_submissions": adapter.queue_submissions(),
        "current_in_flight_submissions": adapter.current_in_flight_submissions(),
        "peak_in_flight_submissions": adapter.peak_in_flight_submissions(),
        "backpressure_deferrals": adapter.backpressure_deferrals(),
        "residency": {
            "hits": adapter.residency_hits(),
            "misses": adapter.residency_misses(),
            "evictions": adapter.residency_evictions(),
            "epoch_reuploads": adapter.residency_epoch_reuploads(),
        },
        "uploads": {
            "resources": adapter.uploaded_resources(),
            "payload_bytes": adapter.uploaded_payload_bytes(),
            "cancelled_payload_bytes": {
                "available": false,
                "reason": "renderer_uploads_have_no_sealed_generation_cancellation_outcome",
            },
            "render_thread_payload_fact_scan_bytes": adapter.render_thread_payload_fact_scan_bytes(),
        },
        "control": {
            "static_rebuilds": adapter.control_static_rebuilds(),
            "static_rebuild_bytes": adapter.control_static_rebuild_bytes(),
            "dynamic_updates": adapter.control_dynamic_updates(),
            "dynamic_upload_bytes": adapter.control_dynamic_upload_bytes(),
            "publication_writes": adapter.control_publication_writes(),
            "peak_publication_writes_per_frame": adapter.peak_control_publication_writes_per_frame(),
            "dense_fallbacks": adapter.control_dense_fallbacks(),
            "buffer_allocations": adapter.control_buffer_allocations(),
            "bind_group_creations": adapter.bind_group_creations(),
            "pipeline_creations": adapter.pipeline_creations(),
            "residency_directory_updates": adapter.control_body_delta_updates(),
            "page_layout_constructions": adapter.page_layout_constructions(),
            "page_table_updates": adapter.control_body_delta_page_entries(),
            "allocator_plans": adapter.allocator_plans(),
        },
        "staging": {
            "explicit_allocations": adapter.explicit_staging_allocations(),
            "explicit_bytes": adapter.explicit_staging_bytes(),
            "peak_explicit_bytes": adapter.peak_explicit_staging_bytes(),
            "peak_transfer_bytes": adapter.peak_transfer_bytes(),
            "padding_zero_bytes": adapter.upload_staging_padding_zero_bytes(),
        },
        "retained_navigation_frames": adapter.retained_navigation_frames(),
        "timing": timing,
        "picks": {
            "submissions": adapter.pick_submissions(),
            "completed": adapter.completed_picks(),
            "backpressure_deferrals": adapter.pick_backpressure_deferrals(),
        },
        "validation_error_count": adapter.validation_error_count(),
    })
}
