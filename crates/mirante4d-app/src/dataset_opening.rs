use std::{path::Path, sync::Arc};

use mirante4d_analysis::{summarize_u8_volume, summarize_u16_volume};
use mirante4d_application::UnboundWorkspace;
use mirante4d_data::{
    DataRuntimeConfig, DatasetHandle, DenseVolumeF32, DenseVolumeU8, DenseVolumeU16,
};
use mirante4d_dataset::{
    DatasetCatalog, DatasetLayer, DatasetSourceId, ResourceValidity, ScientificIdentityStatus,
};
use mirante4d_domain::{
    CameraView, CrossSectionView, GridToWorld, IntensityDType, IsoLightState, LayerTransfer,
    LogicalLayerKey, RenderMode as CanonicalRenderMode, RenderState, RgbColor, SamplingPolicy,
    Shape3D, TimeIndex, TransferCurve, UnitQuaternion, ViewerLayout,
};
use mirante4d_format::{LayerDisplay, LayerId};
use mirante4d_project_model::{LayerViewState, ProjectId, ViewState};
use mirante4d_render_api::PresentationViewport;
use mirante4d_renderer::{
    FrameDiagnostics, FrameDiagnosticsF32, IntensitySamplingPolicy, IntensityTransfer,
    IsoShadingMode, MipImageF32, MipImageU16, RenderViewport,
};
use mirante4d_settings::ResourcePolicy;

use crate::{
    FrameCompleteness, FrameFidelityStatus, IntensitySummary, LodDecisionReason, LodScheduleState,
    RenderBackend, RenderedIntensityChannel, StartupDiagnostics, collect_startup_diagnostics,
    cross_section_runtime::CrossSectionRuntime,
    current_runtime::{
        analysis::CurrentAnalysisRuntime, dataset::CurrentDatasetRuntime,
        render::CurrentRenderRuntime,
    },
    render_state::{
        dense_startup_allowed, f32_frame_to_display_u16_for_mode, f32_values_to_display_u16,
        metadata_intensity_summary, placeholder_frame_for_mode, render_app_frame,
        render_f32_app_frame, render_u8_app_frame,
    },
    transfer_presets::default_channel_presets,
    viewport::{
        default_camera_for_shape, default_presentation_viewport, default_render_viewport_for_shape,
    },
};

pub(crate) struct OpenedScalarLayer {
    pub(crate) source_shape: Shape3D,
    pub(crate) source_grid_to_world: GridToWorld,
    pub(crate) active_volume_u8: Option<DenseVolumeU8>,
    pub(crate) active_volume: Option<DenseVolumeU16>,
    pub(crate) active_volume_f32: Option<DenseVolumeF32>,
    pub(crate) camera: CameraView,
    pub(crate) presentation_viewport: PresentationViewport,
    pub(crate) render_viewport: RenderViewport,
    pub(crate) frame: MipImageU16,
    pub(crate) frame_f32: Option<MipImageF32>,
    pub(crate) diagnostics: FrameDiagnostics,
    pub(crate) diagnostics_f32: Option<FrameDiagnosticsF32>,
    pub(crate) active_intensity_summary: IntensitySummary,
}

#[derive(Debug, Clone, Copy, Default)]
struct InitialScalarLayerOverrides {
    render_viewport: Option<RenderViewport>,
    dense_startup_voxel_limit: Option<u64>,
}

#[cfg(test)]
pub(crate) const TEST_INITIAL_RENDER_VIEWPORT_SIDE: u64 = 32;
#[cfg(test)]
pub(crate) const TEST_DENSE_STARTUP_VOXEL_LIMIT: u64 = 16 * 16 * 16;

fn placeholder_open_frame(
    source_shape: Shape3D,
    render_viewport: RenderViewport,
    mode: CanonicalRenderMode,
) -> anyhow::Result<(MipImageU16, FrameDiagnostics)> {
    let frame = placeholder_frame_for_mode(render_viewport, mode);
    let diagnostics =
        mirante4d_renderer::frame_diagnostics(source_shape.element_count()?, frame.pixels());
    Ok((frame, diagnostics))
}

#[derive(Debug, Clone)]
pub(crate) struct ScalarLayerOpenOptions {
    pub(crate) display: LayerDisplay,
    pub(crate) transfer: IntensityTransfer,
    pub(crate) presentation_viewport: PresentationViewport,
    pub(crate) timepoint: TimeIndex,
    pub(crate) render_state: RenderState,
}

fn open_initial_scalar_layer_with_overrides(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    stored_dtype: IntensityDType,
    options: ScalarLayerOpenOptions,
    overrides: InitialScalarLayerOverrides,
) -> anyhow::Result<OpenedScalarLayer> {
    let source_shape = dataset.scale_shape(layer_id, 0)?;
    let source_grid_to_world = dataset.scale_grid_to_world(layer_id, 0)?;
    let camera = default_camera_for_shape(source_shape, source_grid_to_world);
    let presentation_viewport = options.presentation_viewport;
    let render_viewport = overrides
        .render_viewport
        .map(Ok)
        .unwrap_or_else(|| default_render_viewport_for_shape(source_shape))?;
    let quality = mirante4d_renderer::CameraRenderQuality {
        intensity_sampling: IntensitySamplingPolicy::SmoothLinear,
        iso_shading: IsoShadingMode::GradientLighting,
    };
    let active_intensity_summary = metadata_intensity_summary(source_shape)?;
    let dense_startup_is_allowed = overrides
        .dense_startup_voxel_limit
        .map(|limit| {
            source_shape
                .element_count()
                .is_ok_and(|voxels| voxels <= limit)
        })
        .unwrap_or_else(|| dense_startup_allowed(source_shape));
    if !dense_startup_is_allowed {
        let (frame, diagnostics) =
            placeholder_open_frame(source_shape, render_viewport, options.render_state.mode())?;
        return Ok(OpenedScalarLayer {
            source_shape,
            source_grid_to_world,
            active_volume_u8: None,
            active_volume: None,
            active_volume_f32: None,
            camera,
            presentation_viewport,
            render_viewport,
            frame,
            frame_f32: None,
            diagnostics,
            diagnostics_f32: None,
            active_intensity_summary,
        });
    }

    match stored_dtype {
        IntensityDType::Float32 => {
            let volume_f32 = dataset.read_f32_volume(layer_id, options.timepoint)?;
            let display_values =
                f32_values_to_display_u16(volume_f32.values(), options.display.window());
            let active_volume = DenseVolumeU16::new(
                volume_f32.dataset_id.clone(),
                volume_f32.layer_id.clone(),
                volume_f32.scale_level,
                volume_f32.timepoint,
                volume_f32.shape,
                volume_f32.grid_to_world,
                display_values,
            )?;
            let (frame_f32, diagnostics_f32) = render_f32_app_frame(
                &volume_f32,
                camera,
                presentation_viewport,
                render_viewport,
                options.render_state,
                &options.transfer,
                quality,
            )?;
            let frame = f32_frame_to_display_u16_for_mode(
                &frame_f32,
                options.render_state.mode(),
                options.display.window(),
            )?;
            let diagnostics = mirante4d_renderer::frame_diagnostics(
                active_volume.shape.element_count()?,
                frame.pixels(),
            );
            let active_intensity_summary = summarize_u16_volume(&active_volume);
            Ok(OpenedScalarLayer {
                source_shape,
                source_grid_to_world,
                active_volume_u8: None,
                active_volume: Some(active_volume),
                active_volume_f32: Some(volume_f32),
                camera,
                presentation_viewport,
                render_viewport,
                frame,
                frame_f32: Some(frame_f32),
                diagnostics,
                diagnostics_f32: Some(diagnostics_f32),
                active_intensity_summary,
            })
        }
        IntensityDType::Uint8 => {
            let volume = dataset.read_u8_volume(layer_id, options.timepoint)?;
            let (frame, diagnostics) = render_u8_app_frame(
                &volume,
                camera,
                presentation_viewport,
                render_viewport,
                options.render_state,
                &options.transfer,
                quality,
            )?;
            let active_intensity_summary = summarize_u8_volume(&volume);
            Ok(OpenedScalarLayer {
                source_shape,
                source_grid_to_world,
                active_volume_u8: Some(volume),
                active_volume: None,
                active_volume_f32: None,
                camera,
                presentation_viewport,
                render_viewport,
                frame,
                frame_f32: None,
                diagnostics,
                diagnostics_f32: None,
                active_intensity_summary,
            })
        }
        IntensityDType::Uint16 => {
            let volume = dataset.read_u16_volume(layer_id, options.timepoint)?;
            let (frame, diagnostics) = render_app_frame(
                &volume,
                camera,
                presentation_viewport,
                render_viewport,
                options.render_state,
                &options.transfer,
                quality,
            )?;
            let active_intensity_summary = summarize_u16_volume(&volume);
            Ok(OpenedScalarLayer {
                source_shape,
                source_grid_to_world,
                active_volume_u8: None,
                active_volume: Some(volume),
                active_volume_f32: None,
                camera,
                presentation_viewport,
                render_viewport,
                frame,
                frame_f32: None,
                diagnostics,
                diagnostics_f32: None,
                active_intensity_summary,
            })
        }
    }
}

/// One source-derived construction result. It is consumed immediately by the
/// composition root or by the bounded source-open actor and is never retained
/// as another live runtime aggregate.
pub(crate) struct OpenedCurrentSource {
    pub(crate) startup_diagnostics: StartupDiagnostics,
    pub(crate) catalog: Arc<DatasetCatalog>,
    pub(crate) workspace: UnboundWorkspace,
    pub(crate) dataset_runtime: CurrentDatasetRuntime,
    pub(crate) render_runtime: CurrentRenderRuntime,
    pub(crate) analysis_runtime: CurrentAnalysisRuntime,
}

pub(crate) fn open_dataset_with_resource_policy_and_render_first_frame(
    path: impl AsRef<Path>,
    resource_policy: ResourcePolicy,
    source_id: DatasetSourceId,
) -> anyhow::Result<OpenedCurrentSource> {
    #[cfg(test)]
    {
        open_test_dataset_with_resource_policy_and_render_first_frame(
            path,
            resource_policy,
            source_id,
            RenderViewport::new(
                TEST_INITIAL_RENDER_VIEWPORT_SIDE,
                TEST_INITIAL_RENDER_VIEWPORT_SIDE,
            )
            .expect("the fixed test viewport is valid"),
            TEST_DENSE_STARTUP_VOXEL_LIMIT,
        )
    }

    #[cfg(not(test))]
    open_dataset_with_resource_policy_and_overrides(
        path,
        resource_policy,
        source_id,
        // The interactive product always enters through the progressive brick
        // path. Dense whole-volume startup is retained only by the explicit
        // test/reference override below.
        InitialScalarLayerOverrides {
            render_viewport: None,
            dense_startup_voxel_limit: Some(0),
        },
    )
}

#[cfg(test)]
pub(crate) fn open_test_dataset_with_resource_policy_and_render_first_frame(
    path: impl AsRef<Path>,
    resource_policy: ResourcePolicy,
    source_id: DatasetSourceId,
    render_viewport: RenderViewport,
    dense_startup_voxel_limit: u64,
) -> anyhow::Result<OpenedCurrentSource> {
    open_dataset_with_resource_policy_and_overrides(
        path,
        resource_policy,
        source_id,
        InitialScalarLayerOverrides {
            render_viewport: Some(render_viewport),
            dense_startup_voxel_limit: Some(dense_startup_voxel_limit),
        },
    )
}

fn open_dataset_with_resource_policy_and_overrides(
    path: impl AsRef<Path>,
    resource_policy: ResourcePolicy,
    source_id: DatasetSourceId,
    overrides: InitialScalarLayerOverrides,
) -> anyhow::Result<OpenedCurrentSource> {
    let adapter = resource_policy.current_runtime_adapter();
    let dataset = DatasetHandle::open_with_runtime_config(
        &path,
        DataRuntimeConfig::from_cache_budgets(
            adapter.cpu_whole_volume_cache_budget_bytes(),
            adapter.cpu_brick_cache_budget_bytes(),
        ),
    )?;
    let layer_id = dataset.first_layer_id()?;
    let layer = dataset
        .layer(&layer_id)
        .expect("first layer id comes from manifest")
        .clone();
    let mut catalog_layers = Vec::with_capacity(dataset.layer_count());
    let mut view_layers = Vec::with_capacity(dataset.layer_count());
    for (index, layer) in dataset.manifest().layers.iter().enumerate() {
        let physical_layer_id =
            LayerId::new(layer.id.clone()).expect("dataset validation rejects invalid layer ids");
        let layer_key = LogicalLayerKey::new(u32::try_from(index)?);
        let grid_to_world = dataset.scale_grid_to_world(&physical_layer_id, 0)?;
        let validity = if layer
            .scales
            .iter()
            .find(|scale| scale.level == 0)
            .expect("dataset validation requires the base scale")
            .validity
            .is_some()
        {
            ResourceValidity::BitMask
        } else {
            ResourceValidity::AllValid
        };
        catalog_layers.push(DatasetLayer::new(
            layer_key,
            &layer.name,
            layer.shape,
            layer.dtype.stored,
            grid_to_world,
            validity,
        )?);
        view_layers.push(LayerViewState::new(
            layer_key,
            layer.display.visible(),
            LayerTransfer::new(
                layer.display.window(),
                RgbColor::new(layer.channel.color_rgba[..3].try_into().unwrap())
                    .expect("dataset validation rejects invalid channel colors"),
                layer.display.opacity(),
                TransferCurve::linear(),
                false,
            ),
            RenderState::mip(SamplingPolicy::SmoothLinear),
        ));
    }
    let catalog = Arc::new(DatasetCatalog::new(
        dataset.dataset_name(),
        ScientificIdentityStatus::Unverified(source_id),
        catalog_layers,
    )?);
    let active_key = LogicalLayerKey::new(0);
    let active_layer_view = view_layers
        .first()
        .expect("the current dataset format requires at least one layer")
        .clone();
    let active_transfer = IntensityTransfer::new(
        active_layer_view.visible(),
        active_layer_view.transfer().clone(),
    );
    let active_render_state = *active_layer_view.render_state();
    let legacy_display = layer.display;
    let opened = open_initial_scalar_layer_with_overrides(
        &dataset,
        &layer_id,
        layer.dtype.stored,
        ScalarLayerOpenOptions {
            display: legacy_display,
            transfer: active_transfer,
            presentation_viewport: default_presentation_viewport(),
            timepoint: TimeIndex::new(0),
            render_state: active_render_state,
        },
        overrides,
    )?;
    let OpenedScalarLayer {
        source_shape,
        source_grid_to_world,
        active_volume_u8,
        active_volume,
        active_volume_f32,
        camera,
        presentation_viewport,
        render_viewport,
        frame,
        frame_f32,
        diagnostics,
        diagnostics_f32,
        active_intensity_summary,
    } = opened;
    let cross_section = CrossSectionView::new(
        camera.target(),
        UnitQuaternion::identity(),
        camera.orthographic_world_per_screen_point(),
        effective_voxel_world_step(source_grid_to_world),
    )?;
    let view = ViewState::new(
        view_layers,
        active_key,
        TimeIndex::new(0),
        camera,
        ViewerLayout::Single3d,
        cross_section,
        IsoLightState::attached_camera(),
    )?;
    let presets = default_channel_presets(&catalog, &view)?;
    let workspace = UnboundWorkspace::new(
        ProjectId::from_bytes(*uuid::Uuid::new_v4().as_bytes()),
        view,
        presets,
    )
    .map_err(|code| anyhow::anyhow!("initial application workspace rejected: {code:?}"))?;
    let dense_frame_is_exact =
        active_volume_u8.is_some() || active_volume.is_some() || active_volume_f32.is_some();
    let rendered_channels = vec![RenderedIntensityChannel {
        layer_id: layer_id.clone(),
        render_state: active_render_state,
        transfer: active_transfer,
        frame: frame.clone(),
        frame_f32: frame_f32.clone(),
    }];
    let dataset_runtime = CurrentDatasetRuntime::opened(
        dataset,
        source_shape,
        active_volume_u8,
        active_volume,
        active_volume_f32,
    );
    let mut render_runtime = CurrentRenderRuntime::opened(
        presentation_viewport,
        render_viewport,
        FrameFidelityStatus::new_with_presentation(render_viewport, presentation_viewport),
        LodScheduleState::new(dense_frame_is_exact.then_some(0)),
        diagnostics,
        diagnostics_f32,
        CrossSectionRuntime::default(),
        frame,
        frame_f32,
        rendered_channels,
    );
    if dense_frame_is_exact {
        render_runtime.frame_fidelity.displayed_scale_level = Some(0);
        render_runtime.frame_fidelity.completeness = FrameCompleteness::Exact;
        render_runtime.frame_fidelity.reason = LodDecisionReason::ExactS0;
        render_runtime.frame_fidelity.backend = RenderBackend::CpuReference;
        render_runtime.lod_schedule.displayed_scale_level = Some(0);
        render_runtime.lod_schedule.pending_scale_level = None;
    }

    Ok(OpenedCurrentSource {
        startup_diagnostics: collect_startup_diagnostics(),
        catalog,
        workspace,
        dataset_runtime,
        render_runtime,
        analysis_runtime: CurrentAnalysisRuntime::empty(active_intensity_summary),
    })
}

fn effective_voxel_world_step(grid_to_world: GridToWorld) -> f64 {
    let matrix = grid_to_world.row_major();
    let x = (matrix[0] * matrix[0] + matrix[4] * matrix[4] + matrix[8] * matrix[8]).sqrt();
    let y = (matrix[1] * matrix[1] + matrix[5] * matrix[5] + matrix[9] * matrix[9]).sqrt();
    let z = (matrix[2] * matrix[2] + matrix[6] * matrix[6] + matrix[10] * matrix[10]).sqrt();
    x.min(y).min(z).max(f64::EPSILON)
}
