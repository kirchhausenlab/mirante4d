use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use mirante4d_data::CurrentDatasetSource;
use mirante4d_dataset::{
    CpuLedgerCategory, DatasetCatalog, DatasetResourceKey, DatasetSource, DatasetSourceId,
    ResourceRegion,
};
use mirante4d_dataset_runtime::{
    CancellationGeneration, DatasetRuntime, DatasetRuntimeConfig, DatasetRuntimeDiagnostics,
    RequestPriority, ResourceRequest, RuntimeFault, RuntimeFaultCode, RuntimeOutcome,
    ShutdownState,
};
use mirante4d_domain::{ScaleLevel, Shape3D, Shape4D, TimeIndex};
use mirante4d_format::{
    ChannelMetadata, DenseU16Layer, ExistingPackagePolicy, NativeU16Dataset, WorldSpace, WorldUnit,
    default_u16_display, grid_to_world_scale_um, write_native_u16_dataset,
};
use serde_json::{Value, json};

use crate::{host::benchmark_host_context, stable_id_from_name};

const DIAGNOSTIC_CPU_BYTES: u64 = 1024 * 1024 * 1024;
const NATIVE_WORKERS: usize = 2;
const STRESS_WORKERS: usize = 4;
const MAX_REQUESTS: usize = 16;
const POLL_BATCH: usize = 8;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const SEMANTIC_TILE_EDGE: u64 = 32;
const DIAGNOSTIC_SCOPE: u64 = 90;

pub(crate) fn bench_native_package(package: &Path) -> anyhow::Result<PathBuf> {
    bench_native_package_with_overrides(package, NativePackageBenchmarkOverrides::default())
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NativePackageBenchmarkOverrides {
    pub(crate) viewport_width: Option<u64>,
    pub(crate) viewport_height: Option<u64>,
    pub(crate) brick_pixel_stride: Option<u64>,
}

pub(crate) fn bench_native_package_with_overrides(
    package: &Path,
    overrides: NativePackageBenchmarkOverrides,
) -> anyhow::Result<PathBuf> {
    validate_package_path(package)?;
    let request_limit = request_limit_from_overrides(overrides)?;
    let report = run_runtime_diagnostic(
        package,
        DatasetSourceId::new(1),
        NATIVE_WORKERS,
        request_limit,
        overrides,
        "caller-supplied-current-package",
    )?;

    let package_name = package
        .file_stem()
        .and_then(|name| name.to_str())
        .map(stable_id_from_name)
        .unwrap_or_else(|| "native-package".to_owned());
    write_report(
        PathBuf::from("target/mirante4d/benchmarks")
            .join(format!("bench-native-package-{package_name}.json")),
        report_with_name("bench-native-package", report),
    )
}

pub(crate) fn bench_runtime_stress() -> anyhow::Result<PathBuf> {
    let output_root = PathBuf::from("target/mirante4d/benchmarks/runtime-stress");
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let package = output_root.join("bounded-runtime-diagnostic.m4d");
    write_bounded_stress_package(&package)?;

    let report = run_runtime_diagnostic(
        &package,
        DatasetSourceId::new(2),
        STRESS_WORKERS,
        MAX_REQUESTS,
        NativePackageBenchmarkOverrides::default(),
        "bounded-generated-current-package",
    )?;
    write_report(
        output_root.join("bench-runtime-stress.json"),
        report_with_name("bench-runtime-stress", report),
    )
}

fn validate_package_path(package: &Path) -> anyhow::Result<()> {
    if !package.is_dir() {
        bail!(
            "native package path does not exist or is not a directory: {}",
            package.display()
        );
    }
    Ok(())
}

fn run_runtime_diagnostic(
    package: &Path,
    source_id: DatasetSourceId,
    worker_limit: usize,
    request_limit: usize,
    overrides: NativePackageBenchmarkOverrides,
    source_kind: &'static str,
) -> anyhow::Result<Value> {
    let total_started = Instant::now();
    let config = DatasetRuntimeConfig::new(
        DIAGNOSTIC_CPU_BYTES,
        worker_limit,
        MAX_REQUESTS,
        MAX_REQUESTS,
    )
    .map_err(|code| anyhow::anyhow!("invalid bounded runtime diagnostic config: {code}"))?;

    let source_error = Arc::new(Mutex::new(None));
    let source_error_for_factory = Arc::clone(&source_error);
    let source_path = package.to_path_buf();
    let open_started = Instant::now();
    let (runtime, catalog) = <dyn DatasetRuntime>::start(config, move |ledger| {
        match CurrentDatasetSource::open(source_path, source_id, ledger) {
            Ok(source) => {
                let source: Arc<dyn DatasetSource> = source;
                Ok(source)
            }
            Err(error) => {
                *source_error_for_factory
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = Some(error.to_string());
                Err(RuntimeFault::new(RuntimeFaultCode::SourceRejected))
            }
        }
    })
    .map_err(|runtime_error| {
        let source_error = source_error
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        anyhow::anyhow!(
            "unified runtime could not open the diagnostic source: {}",
            source_error.unwrap_or_else(|| runtime_error.to_string())
        )
    })?;
    let open_ms = open_started.elapsed().as_secs_f64() * 1000.0;

    let resources = plan_semantic_resources(catalog.as_ref(), request_limit)?;
    let selected_layer = resources
        .first()
        .expect("a non-empty catalog and non-empty scale produce one semantic resource")
        .layer();
    let selected_scale = resources[0].scale();
    let generation = CancellationGeneration::for_scope(DIAGNOSTIC_SCOPE, 1);
    for resource in &resources {
        if let Err(error) = runtime.submit(ResourceRequest::new(
            *resource,
            RequestPriority::CurrentView,
            generation,
        )) {
            let _ = runtime.request_shutdown();
            return Err(anyhow::Error::new(error).context("runtime diagnostic request admission"));
        }
    }

    let request_started = Instant::now();
    let outcome = wait_for_completions(runtime.as_ref(), resources.len(), REQUEST_TIMEOUT);
    let request_ms = request_started.elapsed().as_secs_f64() * 1000.0;
    let diagnostics = runtime
        .diagnostics()
        .context("runtime diagnostic snapshot")?;
    let _ = runtime.request_shutdown();
    wait_for_shutdown(runtime.as_ref(), SHUTDOWN_TIMEOUT);
    let outcome = outcome?;

    if !outcome.failures.is_empty() || outcome.cancelled != 0 {
        bail!(
            "runtime diagnostic did not complete cleanly: {} failed, {} cancelled",
            outcome.failures.len(),
            outcome.cancelled
        );
    }

    Ok(json!({
        "runtime_diagnostic_schema_version": 1,
        "authority": "diagnostic-only; no performance, conformance, or product-validation claim",
        "authoritative_performance_claim": false,
        "source_kind": source_kind,
        "hardware": benchmark_host_context(),
        "request_hints": {
            "viewport_width": overrides.viewport_width,
            "viewport_height": overrides.viewport_height,
            "pixel_stride": overrides.brick_pixel_stride,
        },
        "semantic_work": {
            "layer_ordinal": selected_layer.ordinal(),
            "scale_level": selected_scale.get(),
            "requested_resources": resources.len(),
            "ready_resources": outcome.ready,
            "cancelled_resources": outcome.cancelled,
            "failed_resources": outcome.failures,
            "ready_accounted_bytes": outcome.ready_accounted_bytes,
            "tile_edge_upper_bound": SEMANTIC_TILE_EDGE,
        },
        "observed_wall_ms": {
            "open_and_runtime_start": open_ms,
            "requests_until_terminal": request_ms,
            "total": total_started.elapsed().as_secs_f64() * 1000.0,
        },
        "runtime": diagnostics_json(diagnostics),
    }))
}

#[derive(Debug, Default)]
struct CompletionSummary {
    ready: usize,
    cancelled: usize,
    failures: Vec<String>,
    ready_accounted_bytes: u64,
}

fn wait_for_completions(
    runtime: &dyn DatasetRuntime,
    expected: usize,
    timeout: Duration,
) -> anyhow::Result<CompletionSummary> {
    let deadline = Instant::now() + timeout;
    let mut summary = CompletionSummary::default();
    while summary.ready + summary.cancelled + summary.failures.len() < expected {
        if Instant::now() >= deadline {
            bail!(
                "timed out after {:.1}s waiting for bounded unified-runtime diagnostic work",
                timeout.as_secs_f64()
            );
        }
        let completions = runtime
            .poll(POLL_BATCH)
            .context("poll runtime diagnostic")?;
        if completions.is_empty() {
            thread::sleep(Duration::from_millis(1));
            continue;
        }
        for completion in completions {
            match completion.outcome() {
                RuntimeOutcome::Ready(lease) => {
                    summary.ready += 1;
                    summary.ready_accounted_bytes = summary
                        .ready_accounted_bytes
                        .checked_add(lease.accounted_bytes())
                        .context("runtime diagnostic ready-byte count overflowed")?;
                }
                RuntimeOutcome::Cancelled => summary.cancelled += 1,
                RuntimeOutcome::Failed(fault) => summary.failures.push(fault.code().to_string()),
            }
        }
    }
    Ok(summary)
}

fn wait_for_shutdown(runtime: &dyn DatasetRuntime, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while runtime.shutdown_state() != ShutdownState::Stopped && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
}

fn plan_semantic_resources(
    catalog: &DatasetCatalog,
    limit: usize,
) -> anyhow::Result<Vec<DatasetResourceKey>> {
    let layer = catalog
        .layers()
        .next()
        .context("runtime diagnostic catalog has no logical layers")?;
    let scale = layer
        .scale(ScaleLevel::BASE)
        .context("runtime diagnostic first layer has no base scale")?;
    let shape = scale.shape();
    let mut resources = Vec::with_capacity(limit);

    'timepoints: for timepoint in 0..layer.shape().t().min(2) {
        for z in (0..shape.z()).step_by(SEMANTIC_TILE_EDGE as usize) {
            for y in (0..shape.y()).step_by(SEMANTIC_TILE_EDGE as usize) {
                for x in (0..shape.x()).step_by(SEMANTIC_TILE_EDGE as usize) {
                    let region_shape = Shape3D::new(
                        SEMANTIC_TILE_EDGE.min(shape.z() - z),
                        SEMANTIC_TILE_EDGE.min(shape.y() - y),
                        SEMANTIC_TILE_EDGE.min(shape.x() - x),
                    )?;
                    let region = ResourceRegion::new([z, y, x], region_shape)?;
                    resources.push(DatasetResourceKey::new(
                        catalog.scientific_identity().resource_identity(),
                        layer.key(),
                        TimeIndex::new(timepoint),
                        ScaleLevel::BASE,
                        region,
                    ));
                    if resources.len() == limit {
                        break 'timepoints;
                    }
                }
            }
        }
    }
    if resources.is_empty() {
        bail!("runtime diagnostic planned no semantic resources");
    }
    Ok(resources)
}

fn request_limit_from_overrides(
    overrides: NativePackageBenchmarkOverrides,
) -> anyhow::Result<usize> {
    let width = overrides.viewport_width.unwrap_or(256).clamp(1, 4096);
    let height = overrides.viewport_height.unwrap_or(256).clamp(1, 4096);
    let stride = overrides.brick_pixel_stride.unwrap_or(64).clamp(1, 4096);
    let columns = width.div_ceil(stride);
    let rows = height.div_ceil(stride);
    let requested = columns
        .checked_mul(rows)
        .context("runtime diagnostic request hint overflowed")?
        .clamp(1, MAX_REQUESTS as u64);
    usize::try_from(requested).context("runtime diagnostic request limit does not fit usize")
}

fn diagnostics_json(diagnostics: DatasetRuntimeDiagnostics) -> Value {
    let category = |category| {
        json!({
            "used_bytes": diagnostics.category_used_bytes(category),
            "cap_bytes": diagnostics.category_cap_bytes(category),
        })
    };
    json!({
        "cpu": {
            "used_bytes": diagnostics.total_used_bytes(),
            "cap_bytes": diagnostics.total_cap_bytes(),
            "categories": {
                "decoded_residency": category(CpuLedgerCategory::DecodedResidency),
                "upload_staging": category(CpuLedgerCategory::UploadStaging),
                "in_flight_decode": category(CpuLedgerCategory::InFlightDecode),
                "metadata_and_indexes": category(CpuLedgerCategory::MetadataAndIndexes),
                "queues_and_results": category(CpuLedgerCategory::QueuesAndResults),
                "prefetch": category(CpuLedgerCategory::Prefetch),
                "import_working_set": category(CpuLedgerCategory::ImportWorkingSet),
            },
        },
        "bounds": {
            "queued_requests": diagnostics.queued_requests(),
            "request_queue_limit": diagnostics.request_queue_limit(),
            "in_flight_decodes": diagnostics.in_flight_decodes(),
            "worker_limit": diagnostics.worker_limit(),
            "pending_completions": diagnostics.pending_completions(),
            "completion_queue_limit": diagnostics.completion_queue_limit(),
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

fn report_with_name(name: &'static str, report: Value) -> Value {
    let mut report = report
        .as_object()
        .cloned()
        .expect("runtime diagnostic report is an object");
    report.insert("benchmark".to_owned(), Value::String(name.to_owned()));
    Value::Object(report)
}

fn write_report(path: PathBuf, report: Value) -> anyhow::Result<PathBuf> {
    let parent = path
        .parent()
        .context("runtime diagnostic report path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&report)?),
    )
    .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn write_bounded_stress_package(package: &Path) -> anyhow::Result<()> {
    let shape = Shape4D::new(3, 64, 128, 128)?;
    let brick_shape = Shape4D::new(1, 16, 16, 16)?;
    let element_count = usize::try_from(shape.element_count()?)
        .context("bounded stress fixture element count does not fit usize")?;
    let mut values = Vec::with_capacity(element_count);
    for t in 0..shape.t() {
        for z in 0..shape.z() {
            for y in 0..shape.y() {
                for x in 0..shape.x() {
                    let pattern = t * 8191 + z * 251 + y * 37 + x * 11;
                    values.push((pattern % u64::from(u16::MAX)) as u16);
                }
            }
        }
    }
    write_native_u16_dataset(
        package,
        NativeU16Dataset {
            id: "bounded-runtime-diagnostic".to_owned(),
            name: "Bounded runtime diagnostic".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: WorldUnit::Micrometer,
            },
            layers: vec![DenseU16Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape,
                brick_shape,
                grid_to_world: grid_to_world_scale_um(0.2, 0.2, 0.2),
                display: default_u16_display(),
                values_tzyx: values,
            }],
        },
        ExistingPackagePolicy::Replace,
    )?;
    Ok(())
}
