use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

use mirante4d_analysis::{SceneArtifactStore, summarize_u8_volume, summarize_u16_volume};
use mirante4d_core::{
    CameraView, ChannelColor, ChannelTransferFunction, GridToWorld, IntensityDType, IsoLightState,
    LayerDisplay, LayerId, PresentationViewport, Projection, Shape3D, TimeIndex, TransferCurve,
    TransferPresetId,
};
use mirante4d_data::{DatasetHandle, DenseVolumeF32, DenseVolumeU8, DenseVolumeU16};
use mirante4d_renderer::{
    FrameDiagnostics, FrameDiagnosticsF32, IntensitySamplingPolicy, IsoShadingMode, MipImageF32,
    MipImageU16, RenderViewport,
};

use crate::{
    AppLayerSummary, AppPreferences, AppState, ChannelRenderState, DEFAULT_DVR_DENSITY_SCALE,
    DEFAULT_ISO_DISPLAY_LEVEL, DvrOpacityTransfer, FrameCompleteness, FrameFidelityStatus,
    IntensitySummary, LodDecisionReason, LodScheduleState, RenderBackend, RenderIsoShadingPolicy,
    RenderMode, RenderSamplingPolicy, RenderedIntensityChannel, ViewerToolState,
    collect_startup_diagnostics,
    cross_section_runtime::CrossSectionRuntime,
    default_channel_presets_from_layers,
    layer_state::default_dvr_opacity_transfer,
    render_state::{
        dense_startup_allowed, f32_frame_to_display_u16_for_mode, f32_values_to_display_u16,
        metadata_intensity_summary, placeholder_frame_for_mode, render_app_frame,
        render_f32_app_frame, render_u8_app_frame, update_channel_fidelity_status,
    },
    update_visible_brick_plan,
    viewer_layout::ViewerLayoutState,
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

fn placeholder_open_frame(
    source_shape: Shape3D,
    render_viewport: RenderViewport,
    mode: RenderMode,
) -> anyhow::Result<(MipImageU16, FrameDiagnostics)> {
    let frame = placeholder_frame_for_mode(render_viewport, mode);
    let diagnostics =
        mirante4d_renderer::frame_diagnostics(source_shape.element_count()?, frame.pixels());
    Ok((frame, diagnostics))
}

#[derive(Debug, Clone)]
pub(crate) struct ScalarLayerOpenOptions {
    pub(crate) display: LayerDisplay,
    pub(crate) transfer: ChannelTransferFunction,
    pub(crate) dvr_opacity_transfer: DvrOpacityTransfer,
    pub(crate) presentation_viewport: PresentationViewport,
    pub(crate) timepoint: TimeIndex,
    pub(crate) mode: RenderMode,
    pub(crate) iso_display_level: f32,
    pub(crate) dvr_density_scale: f64,
}

pub(crate) fn open_initial_scalar_layer(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    stored_dtype: IntensityDType,
    options: ScalarLayerOpenOptions,
) -> anyhow::Result<OpenedScalarLayer> {
    let source_shape = dataset.scale_shape(layer_id, 0)?;
    let source_grid_to_world = dataset.scale_grid_to_world(layer_id, 0)?;
    let camera = default_camera_for_shape(source_shape, source_grid_to_world);
    let presentation_viewport = options.presentation_viewport;
    let render_viewport = default_render_viewport_for_shape(source_shape)?;
    let quality = mirante4d_renderer::CameraRenderQuality {
        intensity_sampling: IntensitySamplingPolicy::SmoothLinear,
        iso_shading: IsoShadingMode::GradientLighting,
    };
    let active_intensity_summary = metadata_intensity_summary(source_shape)?;
    if !dense_startup_allowed(source_shape) {
        let (frame, diagnostics) =
            placeholder_open_frame(source_shape, render_viewport, options.mode)?;
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
            let display_values = f32_values_to_display_u16(volume_f32.values(), options.display);
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
                options.mode,
                &options.transfer,
                options.display,
                options.dvr_opacity_transfer,
                options.iso_display_level,
                options.dvr_density_scale,
                quality,
            )?;
            let frame =
                f32_frame_to_display_u16_for_mode(&frame_f32, options.mode, options.display)?;
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
                options.mode,
                &options.transfer,
                options.dvr_opacity_transfer,
                options.iso_display_level,
                options.dvr_density_scale,
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
                options.mode,
                &options.transfer,
                options.dvr_opacity_transfer,
                options.iso_display_level,
                options.dvr_density_scale,
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

pub fn open_dataset_and_render_first_frame(path: impl AsRef<Path>) -> anyhow::Result<AppState> {
    open_dataset_with_preferences_and_render_first_frame(path, &AppPreferences::default())
}

pub fn open_dataset_with_preferences_and_render_first_frame(
    path: impl AsRef<Path>,
    preferences: &AppPreferences,
) -> anyhow::Result<AppState> {
    preferences.validate()?;
    let dataset = DatasetHandle::open_with_runtime_config(&path, preferences.runtime_config())?;
    let layer_id = dataset.first_layer_id()?;
    let layer = dataset
        .layer(&layer_id)
        .expect("first layer id comes from manifest")
        .clone();
    let layers = dataset
        .manifest()
        .layers
        .iter()
        .map(|layer| {
            let color = ChannelColor::new(layer.channel.color_rgba)
                .expect("dataset validation rejects invalid channel colors");
            AppLayerSummary {
                id: layer.id.clone(),
                name: layer.name.clone(),
                shape: layer.shape,
                dtype: layer.dtype.stored,
                display: layer.display,
                color,
                curve: TransferCurve::Linear,
                preset: TransferPresetId::linear(),
                invert: false,
                dvr_opacity_transfer: default_dvr_opacity_transfer(layer.display),
                render_state: ChannelRenderState::mip(),
            }
        })
        .collect::<Vec<_>>();
    let active_render_mode = RenderMode::Mip;
    let iso_display_level = DEFAULT_ISO_DISPLAY_LEVEL;
    let iso_light_state = IsoLightState::attached_camera();
    let dvr_density_scale = DEFAULT_DVR_DENSITY_SCALE;
    let active_dvr_opacity_transfer = default_dvr_opacity_transfer(layer.display);
    let active_color = ChannelColor::new(layer.channel.color_rgba)
        .expect("dataset validation rejects invalid channel colors");
    let active_transfer = ChannelTransferFunction::linear(layer.display, active_color);
    let opened = open_initial_scalar_layer(
        &dataset,
        &layer_id,
        layer.dtype.stored,
        ScalarLayerOpenOptions {
            display: layer.display,
            transfer: active_transfer.clone(),
            dvr_opacity_transfer: active_dvr_opacity_transfer,
            presentation_viewport: default_presentation_viewport(),
            timepoint: TimeIndex(0),
            mode: active_render_mode,
            iso_display_level,
            dvr_density_scale,
        },
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
    let render_sampling_policy = RenderSamplingPolicy::default();
    let render_iso_shading_policy = RenderIsoShadingPolicy::default();
    let rendered_channels = vec![RenderedIntensityChannel {
        layer_id: layer_id.to_string(),
        render_state: ChannelRenderState::mip(),
        transfer: active_transfer.clone(),
        frame: frame.clone(),
        frame_f32: frame_f32.clone(),
    }];
    let mut state = AppState {
        startup_diagnostics: collect_startup_diagnostics(),
        dataset_name: dataset.dataset_name().to_owned(),
        dataset_path: path.as_ref().to_path_buf(),
        layer_count: dataset.layer_count(),
        layers,
        active_layer_index: 0,
        active_layer_name: layer.name,
        active_layer_id: layer_id.to_string(),
        active_layer_shape: layer.shape,
        active_layer_dtype: layer.dtype.stored,
        active_layer_display: layer.display,
        active_layer_color: active_color,
        active_layer_transfer: active_transfer,
        active_dvr_opacity_transfer,
        active_source_shape: source_shape,
        active_source_grid_to_world: source_grid_to_world,
        active_timepoint: TimeIndex(0),
        timepoint_count: layer.shape.t,
        active_projection: Projection::Orthographic,
        active_render_mode,
        render_sampling_policy,
        render_iso_shading_policy,
        iso_display_level,
        iso_light_state,
        dvr_density_scale,
        presentation_viewport,
        render_viewport,
        viewer_layout: ViewerLayoutState::single_3d_for_dataset(
            source_shape,
            source_grid_to_world,
            presentation_viewport,
        ),
        render_backend: RenderBackend::CpuReference,
        frame_fidelity: FrameFidelityStatus::new_with_presentation(
            render_viewport,
            presentation_viewport,
        ),
        channel_fidelity: Vec::new(),
        lod_schedule: LodScheduleState::new(
            (active_volume_u8.is_some() || active_volume.is_some() || active_volume_f32.is_some())
                .then_some(0),
        ),
        lod_replan_pending: false,
        playback_lod_downshift_active: false,
        renderer_gpu_brick_budget_bytes: preferences.runtime.gpu_brick_cache_budget_bytes,
        visible_brick_count: 0,
        visible_brick_plan_stride: 1,
        visible_brick_plan_error: None,
        visible_bricks: Vec::new(),
        brick_stream_scale_level: 0,
        brick_stream_scale_shape: source_shape,
        brick_stream_generation: 0,
        brick_stream_requested: 0,
        brick_stream_completed: 0,
        brick_stream_cancelled: 0,
        brick_stream_stale: 0,
        brick_stream_failed: 0,
        brick_stream_last_error: None,
        brick_stream_complete: false,
        brick_result_drain_limit: 0,
        brick_result_drain_time_budget_ms: 0.0,
        brick_result_drain_last_count: 0,
        brick_result_drain_last_budget_limited: false,
        brick_result_drain_last_repaint_reason: None,
        brick_result_drain_budget_hit_count: 0,
        brick_result_drain_total_drained: 0,
        brick_prefetch_timepoints: Vec::new(),
        brick_prefetch_requested: 0,
        brick_prefetch_completed: 0,
        brick_prefetch_cancelled: 0,
        brick_prefetch_stale: 0,
        brick_prefetch_failed: 0,
        brick_prefetch_skipped: 0,
        brick_prefetch_last_error: None,
        brick_warm_brick_count: 0,
        brick_warm_requested: 0,
        brick_warm_completed: 0,
        brick_warm_cancelled: 0,
        brick_warm_stale: 0,
        brick_warm_failed: 0,
        brick_warm_skipped: 0,
        brick_warm_last_error: None,
        resident_bricks_u8: Vec::new(),
        resident_bricks_u8_by_layer: BTreeMap::new(),
        resident_bricks: Vec::new(),
        resident_bricks_by_layer: BTreeMap::new(),
        resident_bricks_f32: Vec::new(),
        resident_bricks_f32_by_layer: BTreeMap::new(),
        cross_section_runtime: CrossSectionRuntime::default(),
        cross_section_last_interaction_at: None,
        resident_histogram_generation: 0,
        resident_histogram_samples: HashMap::new(),
        prefetched_brick_payloads: Vec::new(),
        adapter_summary: None,
        camera,
        viewport_orbit_drag: None,
        frame,
        frame_f32,
        diagnostics,
        diagnostics_f32,
        active_intensity_summary,
        channel_presets: Vec::new(),
        selected_channel_preset_index: None,
        channel_preset_warnings: Vec::new(),
        analysis_tables: Vec::new(),
        analysis_plots: Vec::new(),
        analysis_operations: Vec::new(),
        last_analysis_export_csv: None,
        selected_analysis_table_index: None,
        selected_analysis_plot_index: None,
        selected_analysis_plot_point: None,
        analysis_plot_view: None,
        analysis_filter: String::new(),
        analysis_sort: None,
        rendered_channels,
        scene_artifacts: SceneArtifactStore::default(),
        viewer_tools: ViewerToolState::default(),
        hovered_pixel: None,
        hovered_source_readout: None,
        last_render_error: None,
        last_workflow_message: None,
        dataset,
        active_volume_u8,
        active_volume,
        active_volume_f32,
        active_histogram_cache: None,
        brick_stream_request_key: None,
        brick_prefetch_request_key: None,
        brick_warm_request_key: None,
        #[cfg(test)]
        fixed_frame_time_ms_for_snapshots: None,
    };
    update_visible_brick_plan(&mut state);
    state.channel_presets = default_channel_presets_from_layers(&state.layers);
    update_channel_fidelity_status(&mut state);
    if state.active_volume_u8.is_some()
        || state.active_volume.is_some()
        || state.active_volume_f32.is_some()
    {
        state.frame_fidelity.displayed_scale_level = Some(0);
        state.lod_schedule.displayed_scale_level = Some(0);
        state.frame_fidelity.completeness = FrameCompleteness::Exact;
        state.frame_fidelity.reason = LodDecisionReason::ExactS0;
        state.frame_fidelity.backend = state.render_backend;
    }
    Ok(state)
}
