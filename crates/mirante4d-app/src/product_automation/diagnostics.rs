use mirante4d_dataset::CpuLedgerCategory;
use mirante4d_dataset_runtime::DatasetRuntimeDiagnostics;
use mirante4d_renderer::gpu::{AdapterDiagnostics, GpuLimitDiagnostics};
use serde_json::{Value, json};

pub(crate) fn dataset_runtime_diagnostics_json(diagnostics: DatasetRuntimeDiagnostics) -> Value {
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
        },
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

pub(crate) fn gpu_adapter_diagnostics_json(adapter: &AdapterDiagnostics) -> Value {
    json!({
        "name": adapter.name.as_str(),
        "backend": adapter.backend.as_str(),
        "device_type": adapter.device_type.as_str(),
        "driver": adapter.driver.as_str(),
        "driver_info": adapter.driver_info.as_str(),
        "timestamp_queries_supported": adapter.timestamp_queries_supported,
        "timestamp_queries_requested": adapter.timestamp_queries_requested,
        "timestamp_queries_enabled": adapter.timestamp_queries_enabled,
        "adapter_limits": gpu_limit_diagnostics_json(&adapter.adapter_limits),
        "requested_limits": gpu_limit_diagnostics_json(&adapter.requested_limits),
    })
}

fn gpu_limit_diagnostics_json(limits: &GpuLimitDiagnostics) -> Value {
    json!({
        "max_buffer_size": limits.max_buffer_size,
        "max_storage_buffer_binding_size": limits.max_storage_buffer_binding_size,
        "max_storage_buffers_per_shader_stage": limits.max_storage_buffers_per_shader_stage,
    })
}

pub(crate) fn gpu_timestamp_timing_json(adapter: &AdapterDiagnostics) -> Value {
    json!({
        "kind": "gpu_timestamp_timing",
        "taxonomy_version": 1,
        "status": gpu_timestamp_timing_status(adapter),
        "env_var": "MIRANTE4D_GPU_TIMESTAMPS",
        "measurement_scope": "renderer_compute_pass_elapsed_time_from_wgpu_timestamp_queries",
        "sample_field": "gpu_compute_ms",
        "unit": "milliseconds",
        "adapter_timestamp_queries_supported": adapter.timestamp_queries_supported,
        "timestamp_queries_requested": adapter.timestamp_queries_requested,
        "timestamp_queries_enabled": adapter.timestamp_queries_enabled,
    })
}

fn gpu_timestamp_timing_status(adapter: &AdapterDiagnostics) -> &'static str {
    match (
        adapter.timestamp_queries_supported,
        adapter.timestamp_queries_requested,
        adapter.timestamp_queries_enabled,
    ) {
        (_, _, true) => "enabled",
        (true, true, false) => "requested_but_device_feature_missing",
        (false, true, false) => "requested_but_unsupported",
        (true, false, false) => "supported_not_requested",
        (false, false, false) => "unsupported_not_requested",
    }
}
