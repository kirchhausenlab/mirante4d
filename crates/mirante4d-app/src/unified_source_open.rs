//! Construction of one current source behind the unified dataset runtime.

use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use mirante4d_application::UnboundWorkspace;
use mirante4d_data::{
    CurrentDatasetSource, CurrentSourceVerification, CurrentSourceVerificationError,
    CurrentSourceVerificationProgress,
};
use mirante4d_dataset::{CpuByteLedger, DatasetCatalog, DatasetSource, DatasetSourceId};
use mirante4d_dataset_runtime::{
    DatasetRuntime, DatasetRuntimeConfig, RuntimeFault, RuntimeFaultCode,
};
use mirante4d_domain::{
    CrossSectionView, DisplayWindow, IntensityDType, IsoLightState, LayerTransfer, Opacity,
    RenderState, RgbColor, SamplingPolicy, TransferCurve, UnitQuaternion, ViewerLayout,
};
use mirante4d_project_model::{LayerViewState, ProjectId, ViewState};
use mirante4d_settings::ResourcePolicy;

use crate::{
    CrossSectionRuntime, FrameCompleteness, FrameFidelityStatus, LodDecisionReason,
    LodScheduleState, StartupDiagnostics, collect_startup_diagnostics,
    current_runtime::{analysis::CurrentAnalysisRuntime, render::CurrentRenderRuntime},
    dataset_requests::DatasetDemandState,
    render_state::{metadata_intensity_summary, placeholder_frame_for_mode},
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
    pub(crate) analysis_runtime: CurrentAnalysisRuntime,
    pub(crate) startup_diagnostics: StartupDiagnostics,
}

pub(crate) struct UnifiedVerifiedSource {
    pub(crate) dataset: DatasetDemandState,
    pub(crate) catalog: Arc<DatasetCatalog>,
}

#[derive(Debug)]
pub(crate) enum UnifiedVerifiedSourceOpenError {
    RuntimeConfiguration(RuntimeFaultCode),
    Verification(CurrentSourceVerificationError),
    Runtime(RuntimeFault),
    MissingCpuLedger,
}

pub(crate) enum UnifiedCurrentSourceVerificationError {
    Open(mirante4d_data::CurrentDatasetSourceOpenError),
    Verification(CurrentSourceVerificationError),
}

pub(crate) fn open(
    path: impl AsRef<Path>,
    resource_policy: ResourcePolicy,
    source_id: DatasetSourceId,
) -> anyhow::Result<UnifiedOpenedSource> {
    let selected_path = path.as_ref().to_path_buf();
    let config = runtime_config(resource_policy)
        .map_err(|code| anyhow::anyhow!("unified dataset runtime configuration failed: {code}"))?;

    let source_error = Arc::new(Mutex::new(None));
    let worker_error = Arc::clone(&source_error);
    let captured_ledger = Arc::new(Mutex::new(None));
    let worker_ledger = Arc::clone(&captured_ledger);
    let source_path = selected_path.clone();
    let (runtime, catalog) = <dyn DatasetRuntime>::start(config, move |ledger| {
        *worker_ledger
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(Arc::clone(&ledger));
        match open_current_source(source_path, source_id, ledger) {
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
            .map(anyhow::Error::new)
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

pub(crate) fn verify_current_source(
    path: impl AsRef<Path>,
    source_id: DatasetSourceId,
    scan_ledger: Arc<dyn CpuByteLedger>,
    is_cancelled: impl Fn() -> bool,
    report_progress: impl FnMut(CurrentSourceVerificationProgress),
) -> Result<CurrentSourceVerification, UnifiedCurrentSourceVerificationError> {
    let source = open_current_source(path, source_id, scan_ledger)
        .map_err(UnifiedCurrentSourceVerificationError::Open)?;
    source
        .verify_scientific_content(is_cancelled, report_progress)
        .map_err(UnifiedCurrentSourceVerificationError::Verification)
}

fn open_current_source(
    path: impl AsRef<Path>,
    source_id: DatasetSourceId,
    ledger: Arc<dyn CpuByteLedger>,
) -> Result<Arc<CurrentDatasetSource>, mirante4d_data::CurrentDatasetSourceOpenError> {
    CurrentDatasetSource::open(path, source_id, ledger)
}

pub(crate) fn open_verified(
    path: impl AsRef<Path>,
    resource_policy: ResourcePolicy,
    verification: CurrentSourceVerification,
    cancellation: &AtomicBool,
    report_progress: impl Fn(CurrentSourceVerificationProgress) + Send + Sync + 'static,
) -> Result<UnifiedVerifiedSource, UnifiedVerifiedSourceOpenError> {
    let selected_path = path.as_ref().to_path_buf();
    let config = runtime_config(resource_policy)
        .map_err(UnifiedVerifiedSourceOpenError::RuntimeConfiguration)?;
    let source_error = Arc::new(Mutex::new(None));
    let worker_error = Arc::clone(&source_error);
    let captured_ledger = Arc::new(Mutex::new(None));
    let worker_ledger = Arc::clone(&captured_ledger);
    let source_path = selected_path.clone();
    let (runtime, catalog) = <dyn DatasetRuntime>::start(config, move |ledger| {
        *worker_ledger
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(Arc::clone(&ledger));
        match CurrentDatasetSource::open_verified(
            source_path,
            &verification,
            ledger,
            || cancellation.load(Ordering::Acquire),
            report_progress,
        ) {
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
            .map(UnifiedVerifiedSourceOpenError::Verification)
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
) -> anyhow::Result<(CurrentRenderRuntime, CurrentAnalysisRuntime)> {
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
    let analysis =
        CurrentAnalysisRuntime::empty(metadata_intensity_summary(active.shape().spatial())?);
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
