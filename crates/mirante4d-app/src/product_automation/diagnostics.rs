use mirante4d_data::{
    BrickReadQueueDiagnostics, DataEngineDiagnostics, DataEngineStats, DataRuntimeConfig,
};
use mirante4d_renderer::gpu::{AdapterDiagnostics, GpuLimitDiagnostics, GpuRendererStats};
use serde_json::{Value, json};

pub(crate) fn data_engine_diagnostics_json(diagnostics: DataEngineDiagnostics) -> Value {
    let config = diagnostics.config;
    let stats = diagnostics.stats;
    json!({
        "config": data_runtime_config_json(config),
        "stats": data_engine_stats_json(stats),
    })
}

fn data_runtime_config_json(config: DataRuntimeConfig) -> Value {
    json!({
        "volume_cache_budget_bytes": config.volume_cache_budget_bytes,
        "brick_cache_budget_bytes": config.brick_cache_budget_bytes,
        "upload_staging_budget_bytes": config.upload_staging_budget_bytes,
        "max_in_flight_decoded_bytes": config.max_in_flight_decoded_bytes,
    })
}

fn data_engine_stats_json(stats: DataEngineStats) -> Value {
    json!({
        "volume_cache_hits": stats.volume_cache_hits,
        "volume_cache_misses": stats.volume_cache_misses,
        "volume_cache_evictions": stats.volume_cache_evictions,
        "volume_cache_bytes": stats.volume_cache_bytes,
        "brick_cache_hits": stats.brick_cache_hits,
        "brick_cache_misses": stats.brick_cache_misses,
        "brick_cache_evictions": stats.brick_cache_evictions,
        "brick_cache_bytes": stats.brick_cache_bytes,
        "brick_cache_u8_bytes": stats.brick_cache_u8_bytes,
        "brick_cache_u16_bytes": stats.brick_cache_u16_bytes,
        "brick_cache_f32_bytes": stats.brick_cache_f32_bytes,
        "brick_reads": stats.brick_reads,
        "decoded_brick_values": stats.decoded_brick_values,
        "brick_requests_queued": stats.brick_requests_queued,
        "brick_requests_completed": stats.brick_requests_completed,
        "brick_requests_cancelled": stats.brick_requests_cancelled,
        "brick_requests_stale": stats.brick_requests_stale,
        "brick_requests_failed": stats.brick_requests_failed,
        "brick_queue_full": stats.brick_queue_full,
        "subset_reads": stats.subset_reads,
        "decoded_values": stats.decoded_values,
        "decoded_bytes": stats.decoded_bytes,
        "decoded_brick_bytes": stats.decoded_brick_bytes,
        "encoded_payload_bytes_read": stats.encoded_payload_bytes_read,
        "encoded_shard_payloads_read": stats.encoded_shard_payloads_read,
        "shard_index_cache_hits": stats.shard_index_cache_hits,
        "shard_index_cache_misses": stats.shard_index_cache_misses,
        "shard_index_cache_entries": stats.shard_index_cache_entries,
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

pub(crate) fn gpu_renderer_stats_json(stats: GpuRendererStats) -> Value {
    json!({
        "volume_cache_budget_bytes": stats.volume_cache_budget_bytes,
        "brick_atlas_cache_budget_bytes": stats.brick_atlas_cache_budget_bytes,
        "volume_cache_hits": stats.volume_cache_hits,
        "volume_cache_misses": stats.volume_cache_misses,
        "volume_uploads": stats.volume_uploads,
        "volume_uploaded_bytes": stats.volume_uploaded_bytes,
        "volume_evictions": stats.volume_evictions,
        "volume_resident_bytes": stats.volume_resident_bytes,
        "brick_atlas_cache_hits": stats.brick_atlas_cache_hits,
        "brick_atlas_cache_misses": stats.brick_atlas_cache_misses,
        "brick_atlas_uploads": stats.brick_atlas_uploads,
        "brick_atlas_uploaded_bytes": stats.brick_atlas_uploaded_bytes,
        "brick_atlas_u8_uploaded_bytes": stats.brick_atlas_u8_uploaded_bytes,
        "brick_atlas_u16_uploaded_bytes": stats.brick_atlas_u16_uploaded_bytes,
        "brick_atlas_f32_uploaded_bytes": stats.brick_atlas_f32_uploaded_bytes,
        "brick_atlas_evictions": stats.brick_atlas_evictions,
        "brick_atlas_page_table_rebuilds": stats.brick_atlas_page_table_rebuilds,
        "brick_atlas_page_table_bytes_written": stats.brick_atlas_page_table_bytes_written,
        "brick_atlas_resident_bytes": stats.brick_atlas_resident_bytes,
        "brick_atlas_u8_resident_bytes": stats.brick_atlas_u8_resident_bytes,
        "brick_atlas_u16_resident_bytes": stats.brick_atlas_u16_resident_bytes,
        "brick_atlas_f32_resident_bytes": stats.brick_atlas_f32_resident_bytes,
        "upload_ready_brick_cache_budget_bytes": stats.upload_ready_brick_cache_budget_bytes,
        "upload_ready_brick_cache_hits": stats.upload_ready_brick_cache_hits,
        "upload_ready_brick_cache_misses": stats.upload_ready_brick_cache_misses,
        "upload_ready_brick_cache_evictions": stats.upload_ready_brick_cache_evictions,
        "upload_ready_brick_cache_resident_bytes": stats.upload_ready_brick_cache_resident_bytes,
        "display_resource_cache_hits": stats.display_resource_cache_hits,
        "display_resource_cache_misses": stats.display_resource_cache_misses,
        "display_resource_recreations": stats.display_resource_recreations,
        "display_resource_resident_bytes": stats.display_resource_resident_bytes,
    })
}

pub(crate) fn brick_queue_diagnostics_json(queue: BrickReadQueueDiagnostics) -> Value {
    json!({
        "capacity": queue.capacity,
        "queued_total": queue.queued_total,
        "queued_current_frame": queue.queued_current_frame,
        "queued_prefetch": queue.queued_prefetch,
        "queued_warm": queue.queued_warm,
        "purged_stale_requests": queue.purged_stale_requests,
        "closed": queue.closed,
    })
}
