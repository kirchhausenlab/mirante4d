//! Construction of one current source behind the unified dataset runtime.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use mirante4d_application::UnboundWorkspace;
use mirante4d_dataset::{
    CpuByteLedger, CpuLedgerCategory, CpuLedgerError, DatasetCatalog, DatasetSource,
    DatasetSourceId,
};
use mirante4d_dataset_runtime::{
    DatasetRuntime, DatasetRuntimeConfig, RuntimeFault, RuntimeFaultCode,
};
use mirante4d_domain::{
    CrossSectionView, DisplayWindow, IntensityDType, IsoLightState, LayerTransfer, Opacity,
    RenderState, RgbColor, SamplingPolicy, TransferCurve, UnitQuaternion, ViewerLayout,
};
use mirante4d_project_model::{LayerViewState, ProjectId, ViewState};
use mirante4d_settings::ResourcePolicy;
use mirante4d_storage::{
    LocalDatasetSource, LocalDatasetSourceOpenError, LocalPackageCatalog,
    PACKAGE_VALIDATION_WORKING_BYTES, PackageOpenError, PackageValidationError,
    ScientificPackageValidationError, VerifiedScientificPackageCapability,
};

use crate::{
    CrossSectionRuntime, FrameCompleteness, FrameFidelityStatus, LodDecisionReason,
    LodScheduleState, StartupDiagnostics, collect_startup_diagnostics,
    current_runtime::{analysis::AnalysisProductRuntime, render::CurrentRenderRuntime},
    dataset_requests::DatasetDemandState,
    render_state::placeholder_frame_for_mode,
    transfer_presets::default_channel_presets,
    viewport::{
        default_camera_for_shape, default_presentation_viewport, default_render_viewport_for_shape,
    },
};

const REQUEST_QUEUE_LIMIT: usize = 1_024;
const COMPLETION_QUEUE_LIMIT: usize = 1_024;
const MAX_DATASET_WORKERS: usize = 8;

pub(crate) struct UnifiedOpenedSource {
    pub(crate) dataset: DatasetDemandState,
    pub(crate) catalog: Arc<DatasetCatalog>,
    pub(crate) workspace: UnboundWorkspace,
    pub(crate) render_runtime: CurrentRenderRuntime,
    pub(crate) analysis_runtime: AnalysisProductRuntime,
    pub(crate) startup_diagnostics: StartupDiagnostics,
}

pub(crate) struct UnifiedVerifiedSource {
    pub(crate) dataset: DatasetDemandState,
    pub(crate) catalog: Arc<DatasetCatalog>,
}

#[derive(Debug)]
pub(crate) enum UnifiedVerifiedSourceOpenError {
    RuntimeConfiguration(RuntimeFaultCode),
    Adapter(LocalDatasetSourceOpenError),
    Runtime(RuntimeFault),
    MissingCpuLedger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetPackageVerificationStage {
    MetadataOpened,
    ExactPackageVerified,
    ScientificContentVerified,
}

#[derive(Debug)]
pub(crate) enum TargetPackageVerificationError {
    Cancelled,
    Open(PackageOpenError),
    Reservation(CpuLedgerError),
    InvalidReservation,
    Exact(PackageValidationError),
    Scientific(ScientificPackageValidationError),
}

pub(crate) fn open(
    path: impl AsRef<Path>,
    resource_policy: ResourcePolicy,
    source_id: DatasetSourceId,
) -> anyhow::Result<UnifiedOpenedSource> {
    let selected_path = path.as_ref().to_path_buf();
    let config = runtime_config(resource_policy)
        .map_err(|code| anyhow::anyhow!("unified dataset runtime configuration failed: {code}"))?;

    let source_error = Arc::new(Mutex::new(None::<anyhow::Error>));
    let worker_error = Arc::clone(&source_error);
    let captured_ledger = Arc::new(Mutex::new(None));
    let worker_ledger = Arc::clone(&captured_ledger);
    let source_path = selected_path.clone();
    let display_label = dataset_display_label(&selected_path);
    let (runtime, catalog) = <dyn DatasetRuntime>::start(config, move |ledger| {
        *worker_ledger
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(Arc::clone(&ledger));
        let source = LocalPackageCatalog::open(&source_path)
            .map_err(SourceConstructionError::Open)
            .and_then(|catalog| {
                LocalDatasetSource::from_provisional(catalog, source_id, &display_label, ledger)
                    .map_err(SourceConstructionError::Adapter)
            });
        match source {
            Ok(source) => {
                let source: Arc<dyn DatasetSource> = source;
                Ok(source)
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

    let workspace = workspace_from_catalog(catalog.as_ref())?;
    let (render_runtime, analysis_runtime) = initial_runtime_state(catalog.as_ref(), &workspace)?;
    let resource_identity = catalog.scientific_identity().resource_identity();
    let dataset = DatasetDemandState::new(runtime, cpu_ledger, resource_identity, selected_path);
    Ok(UnifiedOpenedSource {
        dataset,
        catalog,
        workspace,
        render_runtime,
        analysis_runtime,
        startup_diagnostics: collect_startup_diagnostics(),
    })
}

pub(crate) fn verify_target_package(
    path: impl AsRef<Path>,
    scan_ledger: Arc<dyn CpuByteLedger>,
    mut is_cancelled: impl FnMut() -> bool,
    mut report_stage: impl FnMut(TargetPackageVerificationStage),
) -> Result<VerifiedScientificPackageCapability, TargetPackageVerificationError> {
    if is_cancelled() {
        return Err(TargetPackageVerificationError::Cancelled);
    }
    let validation_lease = scan_ledger
        .try_acquire(
            CpuLedgerCategory::InFlightDecode,
            PACKAGE_VALIDATION_WORKING_BYTES,
        )
        .map_err(TargetPackageVerificationError::Reservation)?;
    if validation_lease.category() != CpuLedgerCategory::InFlightDecode
        || validation_lease.reserved_bytes() != PACKAGE_VALIDATION_WORKING_BYTES
    {
        return Err(TargetPackageVerificationError::InvalidReservation);
    }

    let catalog = LocalPackageCatalog::open(path).map_err(TargetPackageVerificationError::Open)?;
    report_stage(TargetPackageVerificationStage::MetadataOpened);
    if is_cancelled() {
        return Err(TargetPackageVerificationError::Cancelled);
    }
    let exact = catalog
        .validate_exact_supported_package(&mut is_cancelled)
        .map_err(TargetPackageVerificationError::Exact)?;
    report_stage(TargetPackageVerificationStage::ExactPackageVerified);
    if is_cancelled() {
        return Err(TargetPackageVerificationError::Cancelled);
    }
    let verified = exact
        .validate_scientific_content(&mut is_cancelled)
        .map_err(TargetPackageVerificationError::Scientific)?;
    report_stage(TargetPackageVerificationStage::ScientificContentVerified);
    Ok(verified)
}

pub(crate) fn open_verified(
    path: impl AsRef<Path>,
    resource_policy: ResourcePolicy,
    capability: VerifiedScientificPackageCapability,
) -> Result<UnifiedVerifiedSource, UnifiedVerifiedSourceOpenError> {
    let selected_path = path.as_ref().to_path_buf();
    let config = runtime_config(resource_policy)
        .map_err(UnifiedVerifiedSourceOpenError::RuntimeConfiguration)?;
    let source_error = Arc::new(Mutex::new(None));
    let worker_error = Arc::clone(&source_error);
    let captured_ledger = Arc::new(Mutex::new(None));
    let worker_ledger = Arc::clone(&captured_ledger);
    let display_label = dataset_display_label(&selected_path);
    let (runtime, catalog) = <dyn DatasetRuntime>::start(config, move |ledger| {
        *worker_ledger
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(Arc::clone(&ledger));
        match LocalDatasetSource::from_verified(capability, &display_label, ledger) {
            Ok(source) => {
                let source: Arc<dyn DatasetSource> = source;
                Ok(source)
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
            .map(UnifiedVerifiedSourceOpenError::Adapter)
            .unwrap_or(UnifiedVerifiedSourceOpenError::Runtime(runtime_error))
    })?;
    let cpu_ledger = captured_ledger
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take()
        .ok_or(UnifiedVerifiedSourceOpenError::MissingCpuLedger)?;
    let resource_identity = catalog.scientific_identity().resource_identity();
    let dataset = DatasetDemandState::new(runtime, cpu_ledger, resource_identity, selected_path);
    Ok(UnifiedVerifiedSource { dataset, catalog })
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

fn initial_runtime_state(
    catalog: &DatasetCatalog,
    workspace: &UnboundWorkspace,
) -> anyhow::Result<(CurrentRenderRuntime, AnalysisProductRuntime)> {
    let view = workspace.view();
    let active = catalog
        .layer(view.active_layer())
        .expect("the initial view closes over the catalog");
    let presentation = default_presentation_viewport();
    let viewport = default_render_viewport_for_shape(active.shape().spatial())?;
    let mode = view
        .layer(view.active_layer())
        .expect("the initial view has an active layer")
        .render_state()
        .mode();
    let frame = placeholder_frame_for_mode(viewport, mode);
    let diagnostics = mirante4d_renderer::frame_diagnostics(0, frame.pixels());
    let mut fidelity = FrameFidelityStatus::new_with_presentation(viewport, presentation);
    fidelity.completeness = FrameCompleteness::Loading;
    fidelity.reason = LodDecisionReason::ExactS0;
    let render = CurrentRenderRuntime::opened(
        presentation,
        viewport,
        fidelity,
        LodScheduleState::new(None),
        diagnostics,
        CrossSectionRuntime::default(),
        frame,
        None,
    );
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
            RenderState::mip(SamplingPolicy::SmoothLinear),
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
