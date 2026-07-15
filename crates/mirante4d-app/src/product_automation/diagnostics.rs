use mirante4d_dataset::CpuLedgerCategory;
use mirante4d_dataset_runtime::DatasetRuntimeDiagnostics;
use mirante4d_render_wgpu::WgpuRenderRuntimeDiagnostics;
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

pub(crate) fn gpu_adapter_diagnostics_json(adapter: &WgpuRenderRuntimeDiagnostics) -> Value {
    json!({
        "name": adapter.adapter_name(),
        "backend": adapter.backend(),
        "driver": adapter.driver(),
        "limits": {
            "max_buffer_size": adapter.max_buffer_size_bytes(),
            "max_storage_buffer_binding_size": adapter.max_storage_buffer_binding_size_bytes(),
            "max_storage_buffers_per_shader_stage": adapter.max_storage_buffers_per_shader_stage(),
        },
        "gpu_budget_bytes": adapter.gpu_budget_bytes(),
        "payload_capacity_bytes": adapter.payload_capacity_bytes(),
        "transfer_capacity_bytes": adapter.transfer_capacity_bytes(),
        "other_capacity_bytes": adapter.other_capacity_bytes(),
        "resident_payload_bytes": adapter.resident_payload_bytes(),
        "frames_executed": adapter.frames_executed(),
        "queue_submissions": adapter.queue_submissions(),
        "validation_error_count": adapter.validation_error_count(),
    })
}
