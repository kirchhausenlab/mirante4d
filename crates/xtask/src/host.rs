use std::{env, fs, path::Path, process::Command};

use mirante4d_data::DataEngineStats;
use mirante4d_format::NativeDatasetProvenanceKind;
use mirante4d_renderer::gpu::GpuRendererStats;
use serde_json::{Value, json};

pub(crate) fn benchmark_host_context() -> Value {
    json!({
        "name": benchmark_hardware_name(),
        "build_profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "git_commit": git_commit_hash(),
        "dirty_worktree": git_dirty_worktree(),
        "os": env::consts::OS,
        "arch": env::consts::ARCH,
        "cpu_model": linux_cpu_model(),
        "mem_total_kib": linux_mem_total_kib(),
    })
}

pub(crate) fn benchmark_hardware_name() -> String {
    env::var("MIRANTE4D_BENCH_HARDWARE_NAME").unwrap_or_else(|_| "local-dev-machine".to_owned())
}

pub(crate) fn benchmark_hardware_class() -> String {
    env::var("MIRANTE4D_BENCH_HARDWARE_CLASS").unwrap_or_else(|_| benchmark_hardware_name())
}

pub(crate) fn benchmark_baseline_class() -> String {
    env::var("MIRANTE4D_BENCH_BASELINE_CLASS").unwrap_or_else(|_| "local_gpu".to_owned())
}

pub(crate) fn benchmark_native_package_dataset_class(
    package: &Path,
    provenance_kind: NativeDatasetProvenanceKind,
) -> String {
    env::var("MIRANTE4D_BENCH_DATASET_CLASS")
        .ok()
        .and_then(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        })
        .unwrap_or_else(|| classify_native_package_dataset_class(package, provenance_kind))
}

fn classify_native_package_dataset_class(
    package: &Path,
    provenance_kind: NativeDatasetProvenanceKind,
) -> String {
    let normalized = package.to_string_lossy().replace('\\', "/");
    if normalized.contains("target/mirante4d/benchmarks/import-sample/") {
        return "imported_real_sample_package".to_owned();
    }
    if normalized.contains("target/mirante4d/fixtures/")
        || provenance_kind == NativeDatasetProvenanceKind::Generated
    {
        return "synthetic_fixture_native_package".to_owned();
    }
    "native_package".to_owned()
}

pub(crate) fn data_stats_json(stats: DataEngineStats) -> Value {
    json!({
        "subset_reads": stats.subset_reads,
        "decoded_values": stats.decoded_values,
        "decoded_bytes": stats.decoded_bytes,
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
        "decoded_brick_bytes": stats.decoded_brick_bytes,
        "encoded_payload_bytes_read": stats.encoded_payload_bytes_read,
        "encoded_shard_payloads_read": stats.encoded_shard_payloads_read,
        "shard_index_cache_hits": stats.shard_index_cache_hits,
        "shard_index_cache_misses": stats.shard_index_cache_misses,
        "shard_index_cache_entries": stats.shard_index_cache_entries,
        "brick_requests_queued": stats.brick_requests_queued,
        "brick_requests_completed": stats.brick_requests_completed,
        "brick_requests_cancelled": stats.brick_requests_cancelled,
        "brick_requests_stale": stats.brick_requests_stale,
        "brick_requests_failed": stats.brick_requests_failed,
        "brick_queue_full": stats.brick_queue_full,
    })
}

pub(crate) fn gpu_stats_json(stats: GpuRendererStats) -> Value {
    json!({
        "volume_cache_hits": stats.volume_cache_hits,
        "volume_cache_misses": stats.volume_cache_misses,
        "volume_uploads": stats.volume_uploads,
        "volume_uploaded_bytes": stats.volume_uploaded_bytes,
        "volume_evictions": stats.volume_evictions,
        "volume_resident_bytes": stats.volume_resident_bytes,
        "volume_cache_budget_bytes": stats.volume_cache_budget_bytes,
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
        "brick_atlas_cache_budget_bytes": stats.brick_atlas_cache_budget_bytes,
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

pub(crate) fn gpu_stats_delta_json(
    before: Option<GpuRendererStats>,
    after: Option<GpuRendererStats>,
) -> Value {
    let Some(before) = before else {
        return json!({
            "available": false,
            "error": "renderer stats unavailable before render",
        });
    };
    let Some(after) = after else {
        return json!({
            "available": false,
            "error": "renderer stats unavailable after render",
        });
    };

    json!({
        "available": true,
        "volume_cache_hits": after.volume_cache_hits.saturating_sub(before.volume_cache_hits),
        "volume_cache_misses": after.volume_cache_misses.saturating_sub(before.volume_cache_misses),
        "volume_uploads": after.volume_uploads.saturating_sub(before.volume_uploads),
        "volume_uploaded_bytes": after.volume_uploaded_bytes.saturating_sub(before.volume_uploaded_bytes),
        "volume_evictions": after.volume_evictions.saturating_sub(before.volume_evictions),
        "volume_resident_bytes_after": after.volume_resident_bytes,
        "volume_cache_budget_bytes": after.volume_cache_budget_bytes,
        "brick_atlas_cache_hits": after.brick_atlas_cache_hits.saturating_sub(before.brick_atlas_cache_hits),
        "brick_atlas_cache_misses": after.brick_atlas_cache_misses.saturating_sub(before.brick_atlas_cache_misses),
        "brick_atlas_uploads": after.brick_atlas_uploads.saturating_sub(before.brick_atlas_uploads),
        "brick_atlas_uploaded_bytes": after.brick_atlas_uploaded_bytes.saturating_sub(before.brick_atlas_uploaded_bytes),
        "brick_atlas_u8_uploaded_bytes": after.brick_atlas_u8_uploaded_bytes.saturating_sub(before.brick_atlas_u8_uploaded_bytes),
        "brick_atlas_u16_uploaded_bytes": after.brick_atlas_u16_uploaded_bytes.saturating_sub(before.brick_atlas_u16_uploaded_bytes),
        "brick_atlas_f32_uploaded_bytes": after.brick_atlas_f32_uploaded_bytes.saturating_sub(before.brick_atlas_f32_uploaded_bytes),
        "brick_atlas_evictions": after.brick_atlas_evictions.saturating_sub(before.brick_atlas_evictions),
        "brick_atlas_page_table_rebuilds": after.brick_atlas_page_table_rebuilds.saturating_sub(before.brick_atlas_page_table_rebuilds),
        "brick_atlas_page_table_bytes_written": after
            .brick_atlas_page_table_bytes_written
            .saturating_sub(before.brick_atlas_page_table_bytes_written),
        "brick_atlas_resident_bytes_after": after.brick_atlas_resident_bytes,
        "brick_atlas_u8_resident_bytes_after": after.brick_atlas_u8_resident_bytes,
        "brick_atlas_u16_resident_bytes_after": after.brick_atlas_u16_resident_bytes,
        "brick_atlas_f32_resident_bytes_after": after.brick_atlas_f32_resident_bytes,
        "brick_atlas_cache_budget_bytes": after.brick_atlas_cache_budget_bytes,
        "upload_ready_brick_cache_budget_bytes": after.upload_ready_brick_cache_budget_bytes,
        "upload_ready_brick_cache_hits": after
            .upload_ready_brick_cache_hits
            .saturating_sub(before.upload_ready_brick_cache_hits),
        "upload_ready_brick_cache_misses": after
            .upload_ready_brick_cache_misses
            .saturating_sub(before.upload_ready_brick_cache_misses),
        "upload_ready_brick_cache_evictions": after
            .upload_ready_brick_cache_evictions
            .saturating_sub(before.upload_ready_brick_cache_evictions),
        "upload_ready_brick_cache_resident_bytes_after": after
            .upload_ready_brick_cache_resident_bytes,
        "display_resource_cache_hits": after.display_resource_cache_hits.saturating_sub(before.display_resource_cache_hits),
        "display_resource_cache_misses": after.display_resource_cache_misses.saturating_sub(before.display_resource_cache_misses),
        "display_resource_recreations": after.display_resource_recreations.saturating_sub(before.display_resource_recreations),
        "display_resource_resident_bytes_after": after.display_resource_resident_bytes,
        "pending_upload_bytes": 0,
        "pending_upload_bytes_source": "phase13_benchmark_uses_synchronous_wgpu_queue_writes",
    })
}

pub(crate) fn linux_process_peak_rss_kib() -> Option<u64> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|line| line.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
}

fn git_commit_hash() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!hash.is_empty()).then_some(hash)
}

fn git_dirty_worktree() -> Option<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(!output.stdout.is_empty())
}

fn linux_cpu_model() -> Option<String> {
    fs::read_to_string("/proc/cpuinfo")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("model name")
                .or_else(|| line.strip_prefix("Hardware"))
        })
        .and_then(|line| {
            line.split_once(':')
                .map(|(_, value)| value.trim().to_owned())
        })
}

fn linux_mem_total_kib() -> Option<u64> {
    fs::read_to_string("/proc/meminfo")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|line| line.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_package_dataset_class_distinguishes_curated_baseline_inputs() {
        assert_eq!(
            classify_native_package_dataset_class(
                Path::new("target/mirante4d/fixtures/time-multichannel-u16-8cube-3t-2c.m4d"),
                NativeDatasetProvenanceKind::Generated,
            ),
            "synthetic_fixture_native_package"
        );
        assert_eq!(
            classify_native_package_dataset_class(
                Path::new("target/mirante4d/benchmarks/import-sample/t5_qual_003.m4d"),
                NativeDatasetProvenanceKind::Imported,
            ),
            "imported_real_sample_package"
        );
        assert_eq!(
            classify_native_package_dataset_class(
                Path::new("/private-qualification/T5-QUAL-001.m4d"),
                NativeDatasetProvenanceKind::Imported,
            ),
            "native_package"
        );
    }

    #[test]
    fn gpu_stats_delta_reports_phase13_resource_counters() {
        let before = GpuRendererStats {
            volume_cache_hits: 2,
            volume_cache_misses: 3,
            volume_uploads: 5,
            volume_uploaded_bytes: 7,
            volume_evictions: 11,
            volume_resident_bytes: 13,
            volume_cache_budget_bytes: 17,
            brick_atlas_cache_hits: 19,
            brick_atlas_cache_misses: 23,
            brick_atlas_uploads: 29,
            brick_atlas_uploaded_bytes: 31,
            brick_atlas_u8_uploaded_bytes: 3,
            brick_atlas_u16_uploaded_bytes: 5,
            brick_atlas_f32_uploaded_bytes: 7,
            brick_atlas_evictions: 37,
            brick_atlas_page_table_rebuilds: 41,
            brick_atlas_page_table_bytes_written: 43,
            brick_atlas_resident_bytes: 47,
            brick_atlas_u8_resident_bytes: 11,
            brick_atlas_u16_resident_bytes: 13,
            brick_atlas_f32_resident_bytes: 17,
            brick_atlas_cache_budget_bytes: 53,
            upload_ready_brick_cache_budget_bytes: 59,
            upload_ready_brick_cache_hits: 61,
            upload_ready_brick_cache_misses: 67,
            upload_ready_brick_cache_evictions: 71,
            upload_ready_brick_cache_resident_bytes: 73,
            display_resource_cache_hits: 2,
            display_resource_cache_misses: 3,
            display_resource_recreations: 5,
            display_resource_resident_bytes: 7,
        };
        let after = GpuRendererStats {
            volume_cache_hits: 4,
            volume_cache_misses: 7,
            volume_uploads: 11,
            volume_uploaded_bytes: 20,
            volume_evictions: 18,
            volume_resident_bytes: 99,
            volume_cache_budget_bytes: 17,
            brick_atlas_cache_hits: 30,
            brick_atlas_cache_misses: 36,
            brick_atlas_uploads: 46,
            brick_atlas_uploaded_bytes: 50,
            brick_atlas_u8_uploaded_bytes: 13,
            brick_atlas_u16_uploaded_bytes: 19,
            brick_atlas_f32_uploaded_bytes: 23,
            brick_atlas_evictions: 60,
            brick_atlas_page_table_rebuilds: 70,
            brick_atlas_page_table_bytes_written: 82,
            brick_atlas_resident_bytes: 100,
            brick_atlas_u8_resident_bytes: 29,
            brick_atlas_u16_resident_bytes: 31,
            brick_atlas_f32_resident_bytes: 37,
            brick_atlas_cache_budget_bytes: 53,
            upload_ready_brick_cache_budget_bytes: 59,
            upload_ready_brick_cache_hits: 83,
            upload_ready_brick_cache_misses: 89,
            upload_ready_brick_cache_evictions: 97,
            upload_ready_brick_cache_resident_bytes: 101,
            display_resource_cache_hits: 11,
            display_resource_cache_misses: 17,
            display_resource_recreations: 23,
            display_resource_resident_bytes: 103,
        };

        let delta = gpu_stats_delta_json(Some(before), Some(after));

        assert_eq!(delta["available"], true);
        assert_eq!(delta["volume_uploads"], 6);
        assert_eq!(delta["volume_uploaded_bytes"], 13);
        assert_eq!(delta["volume_resident_bytes_after"], 99);
        assert_eq!(delta["brick_atlas_uploads"], 17);
        assert_eq!(delta["brick_atlas_uploaded_bytes"], 19);
        assert_eq!(delta["brick_atlas_u8_uploaded_bytes"], 10);
        assert_eq!(delta["brick_atlas_u16_uploaded_bytes"], 14);
        assert_eq!(delta["brick_atlas_f32_uploaded_bytes"], 16);
        assert_eq!(delta["brick_atlas_evictions"], 23);
        assert_eq!(delta["brick_atlas_page_table_rebuilds"], 29);
        assert_eq!(delta["brick_atlas_page_table_bytes_written"], 39);
        assert_eq!(delta["brick_atlas_resident_bytes_after"], 100);
        assert_eq!(delta["brick_atlas_u8_resident_bytes_after"], 29);
        assert_eq!(delta["brick_atlas_u16_resident_bytes_after"], 31);
        assert_eq!(delta["brick_atlas_f32_resident_bytes_after"], 37);
        assert_eq!(delta["upload_ready_brick_cache_budget_bytes"], 59);
        assert_eq!(delta["upload_ready_brick_cache_hits"], 22);
        assert_eq!(delta["upload_ready_brick_cache_misses"], 22);
        assert_eq!(delta["upload_ready_brick_cache_evictions"], 26);
        assert_eq!(delta["upload_ready_brick_cache_resident_bytes_after"], 101);
        assert_eq!(delta["display_resource_cache_hits"], 9);
        assert_eq!(delta["display_resource_cache_misses"], 14);
        assert_eq!(delta["display_resource_recreations"], 18);
        assert_eq!(delta["display_resource_resident_bytes_after"], 103);
        assert_eq!(delta["pending_upload_bytes"], 0);
        assert_eq!(
            delta["pending_upload_bytes_source"],
            "phase13_benchmark_uses_synchronous_wgpu_queue_writes"
        );
    }
}
