pub(crate) const MIB: u64 = 1024 * 1024;
pub(crate) const GIB: u64 = 1024 * MIB;

pub(crate) const DEFAULT_VOLUME_CACHE_BYTES: u64 = 512 * MIB;
pub(crate) const DEFAULT_BRICK_CACHE_BYTES: u64 = 2 * GIB;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataRuntimeConfig {
    pub volume_cache_budget_bytes: u64,
    pub brick_cache_budget_bytes: u64,
    pub upload_staging_budget_bytes: u64,
    pub max_in_flight_decoded_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataEngineDiagnostics {
    pub config: DataRuntimeConfig,
    pub stats: DataEngineStats,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DataEngineStats {
    pub volume_cache_hits: u64,
    pub volume_cache_misses: u64,
    pub volume_cache_evictions: u64,
    pub volume_cache_bytes: u64,
    pub brick_cache_hits: u64,
    pub brick_cache_misses: u64,
    pub brick_cache_evictions: u64,
    pub brick_cache_bytes: u64,
    pub brick_cache_u8_bytes: u64,
    pub brick_cache_u16_bytes: u64,
    pub brick_cache_f32_bytes: u64,
    pub brick_reads: u64,
    pub decoded_brick_values: u64,
    pub brick_requests_queued: u64,
    pub brick_requests_completed: u64,
    pub brick_requests_cancelled: u64,
    pub brick_requests_stale: u64,
    pub brick_requests_failed: u64,
    pub brick_queue_full: u64,
    pub subset_reads: u64,
    pub decoded_values: u64,
    pub decoded_bytes: u64,
    pub decoded_brick_bytes: u64,
    pub encoded_payload_bytes_read: u64,
    pub encoded_shard_payloads_read: u64,
    pub shard_index_cache_hits: u64,
    pub shard_index_cache_misses: u64,
    pub shard_index_cache_entries: u64,
}

impl DataRuntimeConfig {
    pub fn from_cache_budgets(
        volume_cache_budget_bytes: u64,
        brick_cache_budget_bytes: u64,
    ) -> Self {
        Self {
            volume_cache_budget_bytes,
            brick_cache_budget_bytes,
            upload_staging_budget_bytes: upload_staging_budget_bytes(brick_cache_budget_bytes),
            max_in_flight_decoded_bytes: in_flight_decoded_budget_bytes(brick_cache_budget_bytes),
        }
    }
}

impl Default for DataRuntimeConfig {
    fn default() -> Self {
        Self::from_cache_budgets(DEFAULT_VOLUME_CACHE_BYTES, DEFAULT_BRICK_CACHE_BYTES)
    }
}

fn upload_staging_budget_bytes(cpu_decoded_cache_budget_bytes: u64) -> u64 {
    GIB.min(cpu_decoded_cache_budget_bytes / 4)
}

fn in_flight_decoded_budget_bytes(cpu_decoded_cache_budget_bytes: u64) -> u64 {
    (2 * GIB).min(cpu_decoded_cache_budget_bytes / 4)
}
