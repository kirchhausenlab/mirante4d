//! Construction of one current source behind the unified dataset runtime.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use mirante4d_application::{ContentAddressOrigin, RenderCoordinationState, UnboundWorkspace};
use mirante4d_dataset::{
    CpuByteLedger, CpuLedgerCategory, DatasetCatalog, DatasetSource, DatasetSourceId,
};
use mirante4d_dataset_runtime::{
    DatasetRuntime, DatasetRuntimeConfig, ProcessCpuBroker, RuntimeFault, RuntimeFaultCode,
};
use mirante4d_domain::{
    CrossSectionView, DisplayWindow, IntensityDType, IsoLightState, LayerTransfer, Opacity,
    RenderState, RgbColor, SamplingPolicy, TransferCurve, UnitQuaternion, ViewerLayout,
};
use mirante4d_project_model::{
    DatasetLocatorHint, DatasetReference, LayerViewState, ProjectId, ViewState,
};
use mirante4d_settings::ResourcePolicy;
use mirante4d_storage::{
    ExactPackageValidationProgress, LocalDatasetSource, LocalDatasetSourceOpenError,
    LocalPackageCatalog, PACKAGE_VALIDATION_WORKING_BYTES, PackageOpenError,
    PackageValidationError, ScientificPackageValidationError, ScientificValidationProgress,
    ScientificValidationProgressStage, SelfConsistentPackageCapability,
};

use crate::{
    FrameCompleteness, FrameFidelityStatus, LodDecisionReason, StartupDiagnostics,
    analysis_session::AnalysisProductRuntime,
    camera_demand_cache::CameraDemandPlannerError,
    collect_startup_diagnostics,
    dataset_requests::DatasetDemandState,
    default_camera_for_shape,
    transfer_presets::default_channel_presets,
    viewport::{default_presentation_viewport, default_render_viewport_for_shape},
};

const REQUEST_QUEUE_LIMIT: usize = 1_024;
const COMPLETION_QUEUE_LIMIT: usize = 1_024;
const MAX_DATASET_WORKERS: usize = 8;
// One closed-profile interactive decode path: at most 1 MiB retained payload,
// less than 3 MiB of component/range retention, the 8 MiB codec authority,
// and less than 4 MiB of bounded control/staging state.
const MAX_INTERACTIVE_DECODE_PATH_BYTES: u64 = 16 * 1024 * 1024;
const RUNTIME_REQUEST_RECORD_BYTES: u64 = 512;
const RUNTIME_CACHE_RECORD_BYTES: u64 = 192;
const RUNTIME_SCOPE_RECORD_BYTES: u64 = 128;

pub(crate) struct UnifiedOpenedSource {
    pub(crate) dataset: DatasetDemandState,
    pub(crate) catalog: Arc<DatasetCatalog>,
    pub(crate) workspace: UnboundWorkspace,
    pub(crate) render_coordination: RenderCoordinationState,
    pub(crate) analysis_runtime: AnalysisProductRuntime,
    pub(crate) startup_diagnostics: StartupDiagnostics,
    pub(crate) source_reference: DatasetReference,
    pub(crate) content_address_origin: ContentAddressOrigin,
}

pub(crate) struct UnifiedPublishedSource {
    pub(crate) dataset: DatasetDemandState,
    pub(crate) catalog: Arc<DatasetCatalog>,
    pub(crate) source_reference: DatasetReference,
}

#[derive(Debug)]
pub(crate) enum UnifiedPublishedSourceOpenError {
    RuntimeConfiguration(RuntimeFaultCode),
    Adapter(LocalDatasetSourceOpenError),
    Runtime(RuntimeFault),
    DemandPlanner(CameraDemandPlannerError),
    MissingCpuLedger,
    MissingLocalSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetPackageAuditProgress {
    Exact(ExactPackageValidationProgress),
    Scientific(ScientificValidationProgress),
}

#[derive(Debug)]
pub(crate) enum TargetPackageAuditError {
    Cancelled,
    Open(PackageOpenError),
    Reservation,
    InvalidReservation,
    Exact(PackageValidationError),
    Scientific {
        stage: ScientificValidationProgressStage,
        source: ScientificPackageValidationError,
    },
}

pub(crate) fn open(
    path: impl AsRef<Path>,
    resource_policy: ResourcePolicy,
    source_id: DatasetSourceId,
) -> anyhow::Result<UnifiedOpenedSource> {
    let broker = ProcessCpuBroker::new(resource_policy.cpu_dataset_budget_bytes())
        .map_err(|code| anyhow::anyhow!("process CPU broker configuration failed: {code}"))?;
    open_with_broker(path, resource_policy, source_id, broker)
}

pub(crate) fn open_with_broker(
    path: impl AsRef<Path>,
    resource_policy: ResourcePolicy,
    source_id: DatasetSourceId,
    broker: ProcessCpuBroker,
) -> anyhow::Result<UnifiedOpenedSource> {
    let selected_path = path.as_ref().to_path_buf();
    let config = runtime_config(resource_policy)
        .map_err(|code| anyhow::anyhow!("unified dataset runtime configuration failed: {code}"))?;
    broker
        .set_foreground_reserve(interactive_foreground_reserve(config)?)
        .map_err(|error| {
            anyhow::anyhow!("interactive CPU reserve could not be installed: {error}")
        })?;

    let source_error = Arc::new(Mutex::new(None::<anyhow::Error>));
    let worker_error = Arc::clone(&source_error);
    let captured_ledger = Arc::new(Mutex::new(None));
    let worker_ledger = Arc::clone(&captured_ledger);
    let captured_source = Arc::new(Mutex::new(None));
    let worker_source = Arc::clone(&captured_source);
    let captured_reference = Arc::new(Mutex::new(None));
    let worker_reference = Arc::clone(&captured_reference);
    let source_path = selected_path.clone();
    let display_label = dataset_display_label(&selected_path);
    let (runtime, catalog) =
        <dyn DatasetRuntime>::start_with_broker(config, broker, move |ledger| {
            *worker_ledger
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = Some(Arc::clone(&ledger));
            let source = LocalPackageCatalog::open(&source_path)
                .map_err(SourceConstructionError::Open)
                .and_then(|catalog| {
                    let locator_hint = source_path
                        .to_str()
                        .and_then(|path| DatasetLocatorHint::new(path).ok());
                    *worker_reference
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner()) =
                        Some(DatasetReference::new(
                            catalog.science().scientific_content_id(),
                            Some(catalog.declared_package_id()),
                            None,
                            locator_hint,
                        ));
                    LocalDatasetSource::from_admitted(catalog, source_id, &display_label, ledger)
                        .map_err(SourceConstructionError::Adapter)
                });
            match source {
                Ok(source) => {
                    *worker_source
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner()) = Some(Arc::clone(&source));
                    let source_contract: Arc<dyn DatasetSource> = source;
                    Ok(source_contract)
                }
                Err(error) => {
                    *worker_error
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner()) = Some(error.into());
                    Err(RuntimeFault::new(RuntimeFaultCode::SourceRejected))
                }
            }
        })
        .map_err(|runtime_error| {
            source_error
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .take()
                .unwrap_or_else(|| anyhow::Error::new(runtime_error))
        })?;
    let cpu_ledger = captured_ledger
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take()
        .ok_or_else(|| anyhow::anyhow!("unified runtime did not supply its CPU ledger"))?;
    let local_source = captured_source
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take()
        .ok_or_else(|| anyhow::anyhow!("unified runtime did not retain its local source"))?;
    let source_reference = captured_reference
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take()
        .ok_or_else(|| anyhow::anyhow!("unified runtime did not retain its source reference"))?;

    let workspace = workspace_from_catalog(catalog.as_ref())?;
    let (render_coordination, analysis_runtime) =
        initial_runtime_state(catalog.as_ref(), &workspace)?;
    let resource_identity = catalog.resource_identity();
    let dataset = DatasetDemandState::new_local(
        runtime,
        cpu_ledger,
        resource_identity,
        selected_path,
        local_source,
    )?;
    Ok(UnifiedOpenedSource {
        dataset,
        catalog,
        workspace,
        render_coordination,
        analysis_runtime,
        startup_diagnostics: collect_startup_diagnostics(),
        source_reference,
        content_address_origin: ContentAddressOrigin::DeclaredByPackage,
    })
}

pub(crate) fn audit_target_package(
    path: impl AsRef<Path>,
    scan_ledger: Arc<dyn CpuByteLedger>,
    mut is_cancelled: impl FnMut() -> bool,
    mut report_progress: impl FnMut(TargetPackageAuditProgress),
) -> Result<SelfConsistentPackageCapability, TargetPackageAuditError> {
    if is_cancelled() {
        return Err(TargetPackageAuditError::Cancelled);
    }
    let validation_lease = scan_ledger
        .try_acquire(
            CpuLedgerCategory::InFlightDecode,
            PACKAGE_VALIDATION_WORKING_BYTES,
        )
        .map_err(|_| TargetPackageAuditError::Reservation)?;
    if validation_lease.category() != CpuLedgerCategory::InFlightDecode
        || validation_lease.reserved_bytes() != PACKAGE_VALIDATION_WORKING_BYTES
    {
        return Err(TargetPackageAuditError::InvalidReservation);
    }

    let catalog = LocalPackageCatalog::open(path).map_err(TargetPackageAuditError::Open)?;
    if is_cancelled() {
        return Err(TargetPackageAuditError::Cancelled);
    }
    let exact = catalog
        .validate_exact_supported_package_with_progress(&mut is_cancelled, |progress| {
            report_progress(TargetPackageAuditProgress::Exact(progress));
        })
        .map_err(TargetPackageAuditError::Exact)?;
    if is_cancelled() {
        return Err(TargetPackageAuditError::Cancelled);
    }
    let mut scientific_stage = ScientificValidationProgressStage::CanonicalBaseContent;
    exact
        .validate_scientific_content_with_progress(&mut is_cancelled, |progress| {
            scientific_stage = progress.stage();
            report_progress(TargetPackageAuditProgress::Scientific(progress));
        })
        .map_err(|source| TargetPackageAuditError::Scientific {
            stage: scientific_stage,
            source,
        })
}

pub(crate) fn open_published_with_broker(
    resource_policy: ResourcePolicy,
    capability: SelfConsistentPackageCapability,
    broker: ProcessCpuBroker,
) -> Result<UnifiedPublishedSource, UnifiedPublishedSourceOpenError> {
    let selected_path = capability.root_path().to_path_buf();
    let locator_hint = selected_path
        .to_str()
        .and_then(|path| DatasetLocatorHint::new(path).ok());
    let source_reference = DatasetReference::new(
        capability.scientific_content_id(),
        Some(capability.package_id()),
        None,
        locator_hint,
    );
    let config = runtime_config(resource_policy)
        .map_err(UnifiedPublishedSourceOpenError::RuntimeConfiguration)?;
    broker
        .set_foreground_reserve(
            interactive_foreground_reserve(config)
                .map_err(UnifiedPublishedSourceOpenError::RuntimeConfiguration)?,
        )
        .map_err(|_| {
            UnifiedPublishedSourceOpenError::RuntimeConfiguration(
                RuntimeFaultCode::InvalidConfiguration,
            )
        })?;
    let source_error = Arc::new(Mutex::new(None));
    let worker_error = Arc::clone(&source_error);
    let captured_ledger = Arc::new(Mutex::new(None));
    let worker_ledger = Arc::clone(&captured_ledger);
    let captured_source = Arc::new(Mutex::new(None));
    let worker_source = Arc::clone(&captured_source);
    let display_label = dataset_display_label(&selected_path);
    let (runtime, catalog) =
        <dyn DatasetRuntime>::start_with_broker(config, broker, move |ledger| {
            *worker_ledger
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = Some(Arc::clone(&ledger));
            match LocalDatasetSource::from_published(capability, &display_label, ledger) {
                Ok(source) => {
                    *worker_source
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner()) = Some(Arc::clone(&source));
                    let source_contract: Arc<dyn DatasetSource> = source;
                    Ok(source_contract)
                }
                Err(error) => {
                    *worker_error
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner()) = Some(error);
                    Err(RuntimeFault::new(RuntimeFaultCode::SourceRejected))
                }
            }
        })
        .map_err(|runtime_error| {
            source_error
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .take()
                .map(UnifiedPublishedSourceOpenError::Adapter)
                .unwrap_or(UnifiedPublishedSourceOpenError::Runtime(runtime_error))
        })?;
    let cpu_ledger = captured_ledger
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take()
        .ok_or(UnifiedPublishedSourceOpenError::MissingCpuLedger)?;
    let local_source = captured_source
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take()
        .ok_or(UnifiedPublishedSourceOpenError::MissingLocalSource)?;
    let resource_identity = catalog.resource_identity();
    let dataset = DatasetDemandState::new_local(
        runtime,
        cpu_ledger,
        resource_identity,
        selected_path,
        local_source,
    )
    .map_err(UnifiedPublishedSourceOpenError::DemandPlanner)?;
    Ok(UnifiedPublishedSource {
        dataset,
        catalog,
        source_reference,
    })
}

/// Expands an importer-published runtime into the complete state required for a
/// current-source replacement.
///
/// Imported packages use this after consuming their one-shot publication
/// transfer. No path is accepted here: the selected path already came from the
/// destination-bound publication capability.
pub(crate) fn prepare_published_current_source(
    opened: UnifiedPublishedSource,
) -> anyhow::Result<UnifiedOpenedSource> {
    let UnifiedPublishedSource {
        dataset,
        catalog,
        source_reference,
    } = opened;
    let workspace = workspace_from_catalog(catalog.as_ref())?;
    let (render_coordination, analysis_runtime) =
        initial_runtime_state(catalog.as_ref(), &workspace)?;
    Ok(UnifiedOpenedSource {
        dataset,
        catalog,
        workspace,
        render_coordination,
        analysis_runtime,
        startup_diagnostics: collect_startup_diagnostics(),
        source_reference,
        content_address_origin: ContentAddressOrigin::ComputedDuringImport,
    })
}

#[derive(Debug)]
enum SourceConstructionError {
    Open(PackageOpenError),
    Adapter(LocalDatasetSourceOpenError),
}

impl From<PackageOpenError> for SourceConstructionError {
    fn from(error: PackageOpenError) -> Self {
        Self::Open(error)
    }
}

impl std::fmt::Display for SourceConstructionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(error) => write!(formatter, "target package open failed: {error}"),
            Self::Adapter(error) => {
                write!(formatter, "target package runtime binding failed: {error}")
            }
        }
    }
}

impl std::error::Error for SourceConstructionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Open(error) => Some(error),
            Self::Adapter(error) => Some(error),
        }
    }
}

fn dataset_display_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Dataset")
        .to_owned()
}

fn runtime_config(
    resource_policy: ResourcePolicy,
) -> Result<DatasetRuntimeConfig, RuntimeFaultCode> {
    let worker_limit = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
        .clamp(1, MAX_DATASET_WORKERS);
    DatasetRuntimeConfig::new(
        resource_policy.cpu_dataset_budget_bytes(),
        worker_limit,
        REQUEST_QUEUE_LIMIT,
        COMPLETION_QUEUE_LIMIT,
    )
}

fn interactive_foreground_reserve(config: DatasetRuntimeConfig) -> Result<u64, RuntimeFaultCode> {
    let workers =
        u64::try_from(config.worker_limit()).map_err(|_| RuntimeFaultCode::InvalidConfiguration)?;
    let queue = u64::try_from(config.request_queue_limit())
        .map_err(|_| RuntimeFaultCode::InvalidConfiguration)?;
    let completion = u64::try_from(config.completion_queue_limit())
        .map_err(|_| RuntimeFaultCode::InvalidConfiguration)?;
    let decode_cohort = workers
        .checked_mul(MAX_INTERACTIVE_DECODE_PATH_BYTES)
        .ok_or(RuntimeFaultCode::InvalidConfiguration)?;
    let request_records = queue
        .checked_mul(RUNTIME_REQUEST_RECORD_BYTES + RUNTIME_SCOPE_RECORD_BYTES)
        .ok_or(RuntimeFaultCode::InvalidConfiguration)?;
    let retained_results = completion
        .checked_mul(RUNTIME_CACHE_RECORD_BYTES)
        .ok_or(RuntimeFaultCode::InvalidConfiguration)?;
    decode_cohort
        .checked_add(request_records)
        .and_then(|bytes| bytes.checked_add(retained_results))
        .filter(|bytes| *bytes <= config.total_cpu_bytes())
        .ok_or(RuntimeFaultCode::MinimumWorkUnitExceedsBudget)
}

fn initial_runtime_state(
    catalog: &DatasetCatalog,
    workspace: &UnboundWorkspace,
) -> anyhow::Result<(RenderCoordinationState, AnalysisProductRuntime)> {
    let view = workspace.view();
    let active = catalog
        .layer(view.active_layer())
        .expect("the initial view closes over the catalog");
    let presentation = default_presentation_viewport();
    let viewport = default_render_viewport_for_shape(active.shape().spatial())?;
    let mut fidelity = FrameFidelityStatus::new_with_presentation(viewport, presentation);
    fidelity.completeness = FrameCompleteness::Loading;
    fidelity.reason = LodDecisionReason::ExactS0;
    let render = RenderCoordinationState::new(fidelity);
    let mut analysis = AnalysisProductRuntime::new();
    analysis.set_roi([0; 3], active.shape().spatial().dimensions())?;
    Ok((render, analysis))
}

fn workspace_from_catalog(catalog: &DatasetCatalog) -> anyhow::Result<UnboundWorkspace> {
    let first = catalog
        .layers()
        .next()
        .expect("DatasetCatalog is non-empty by construction");
    let camera = default_camera_for_shape(first.shape().spatial(), first.grid_to_world());
    let cross_section = CrossSectionView::new(
        camera.target(),
        UnitQuaternion::identity(),
        camera.orthographic_world_per_screen_point(),
        effective_voxel_world_step(first.grid_to_world()),
    )?;
    let mut layers = Vec::with_capacity(catalog.len());
    for (index, layer) in catalog.layers().enumerate() {
        layers.push(LayerViewState::new(
            layer.key(),
            true,
            default_transfer(layer.dtype(), index)?,
            RenderState::mip(SamplingPolicy::VoxelExact),
        ));
    }
    let active = first.key();
    let view = ViewState::new(
        layers,
        active,
        mirante4d_domain::TimeIndex::new(0),
        camera,
        ViewerLayout::Single3d,
        cross_section,
        IsoLightState::attached_camera(),
    )?;
    let presets = default_channel_presets(catalog, &view)?;
    UnboundWorkspace::new(
        ProjectId::from_bytes(*uuid::Uuid::new_v4().as_bytes()),
        view,
        presets,
    )
    .map_err(|code| anyhow::anyhow!("initial application workspace rejected: {code:?}"))
}

fn default_transfer(dtype: IntensityDType, index: usize) -> anyhow::Result<LayerTransfer> {
    const COLORS: [[f32; 3]; 6] = [
        [1.0, 1.0, 1.0],
        [1.0, 0.25, 0.25],
        [0.25, 1.0, 0.25],
        [0.25, 0.55, 1.0],
        [1.0, 0.4, 1.0],
        [0.25, 1.0, 1.0],
    ];
    let window = match dtype {
        IntensityDType::Uint8 => DisplayWindow::new(0.0, 255.0),
        IntensityDType::Uint16 => DisplayWindow::new(0.0, 65_535.0),
        IntensityDType::Float32 => DisplayWindow::new(0.0, 1.0),
    }?;
    Ok(LayerTransfer::new(
        window,
        RgbColor::new(COLORS[index % COLORS.len()])?,
        Opacity::new(1.0)?,
        TransferCurve::linear(),
        false,
    ))
}

fn effective_voxel_world_step(grid_to_world: mirante4d_domain::GridToWorld) -> f64 {
    let matrix = grid_to_world.row_major();
    let x = (matrix[0] * matrix[0] + matrix[4] * matrix[4] + matrix[8] * matrix[8]).sqrt();
    let y = (matrix[1] * matrix[1] + matrix[5] * matrix[5] + matrix[9] * matrix[9]).sqrt();
    let z = (matrix[2] * matrix[2] + matrix[6] * matrix[6] + matrix[10] * matrix[10]).sqrt();
    x.min(y).min(z).max(f64::EPSILON)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_transfer_is_finite_for_every_supported_dtype() {
        for dtype in [
            IntensityDType::Uint8,
            IntensityDType::Uint16,
            IntensityDType::Float32,
        ] {
            let transfer = default_transfer(dtype, 17).unwrap();
            assert!(transfer.window().low().is_finite());
            assert!(transfer.window().high().is_finite());
        }
    }
}
