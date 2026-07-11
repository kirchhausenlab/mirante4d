use mirante4d_core::{
    ChannelColor, ChannelTransferFunction, LayerDisplay, LayerId, TimeIndex, TransferCurve,
    TransferPresetId,
};

use crate::{
    AppState, ChannelRenderState, DvrOpacityTransfer, FrameCompleteness, LodDecisionReason,
    brick_streaming::reset_prefetch_state,
    dataset_opening::{OpenedScalarLayer, ScalarLayerOpenOptions, open_initial_scalar_layer},
    render_state::{
        metadata_intensity_summary, rerender_state_with_backend, set_single_rendered_channel,
        update_channel_fidelity_status,
    },
    transfer_presets::transfer_from_layer_summary,
    update_visible_brick_plan,
};

pub(crate) fn active_layer_render_state_from_runtime(state: &AppState) -> ChannelRenderState {
    ChannelRenderState::for_mode(
        state.active_render_mode,
        state.render_sampling_policy,
        state.render_iso_shading_policy,
        state.iso_display_level,
        state.active_dvr_opacity_transfer,
        state.dvr_density_scale,
    )
}

pub(crate) fn sync_active_layer_render_state_from_runtime(state: &mut AppState) {
    let render_state = active_layer_render_state_from_runtime(state);
    if let Some(layer) = state.layers.get_mut(state.active_layer_index) {
        layer.render_state = render_state;
        if let ChannelRenderState::Dvr(parameters) = render_state {
            layer.dvr_opacity_transfer = parameters.opacity_transfer;
        }
    }
}

fn apply_render_state_to_runtime(
    state: &mut AppState,
    render_state: ChannelRenderState,
    default_dvr_opacity_transfer: DvrOpacityTransfer,
) {
    state.active_render_mode = render_state.mode();
    state.render_sampling_policy = render_state.sampling_policy();
    state.render_iso_shading_policy = render_state.iso_shading_policy();
    state.iso_display_level = render_state.iso_display_level();
    state.active_dvr_opacity_transfer =
        render_state.dvr_opacity_transfer(default_dvr_opacity_transfer);
    state.dvr_density_scale = render_state.dvr_density_scale();
}

pub(crate) fn set_layer_render_state(
    state: &mut AppState,
    layer_index: usize,
    render_state: ChannelRenderState,
) -> anyhow::Result<bool> {
    let default_dvr_opacity_transfer = {
        let layer = state
            .layers
            .get_mut(layer_index)
            .ok_or_else(|| anyhow::anyhow!("layer index {layer_index} is out of range"))?;
        let changed = layer.render_state != render_state;
        if !changed {
            return Ok(false);
        }
        layer.render_state = render_state;
        if let ChannelRenderState::Dvr(parameters) = render_state {
            layer.dvr_opacity_transfer = parameters.opacity_transfer;
        }
        layer.dvr_opacity_transfer
    };
    if layer_index == state.active_layer_index {
        apply_render_state_to_runtime(state, render_state, default_dvr_opacity_transfer);
    }
    Ok(true)
}

pub(crate) fn layer_render_state_for_mode(
    state: &AppState,
    layer_index: usize,
    mode: crate::RenderMode,
) -> anyhow::Result<ChannelRenderState> {
    let layer = state
        .layers
        .get(layer_index)
        .ok_or_else(|| anyhow::anyhow!("layer index {layer_index} is out of range"))?;
    let current = if layer_index == state.active_layer_index {
        active_layer_render_state_from_runtime(state)
    } else {
        layer.render_state
    };
    Ok(ChannelRenderState::for_mode(
        mode,
        current.sampling_policy(),
        current.iso_shading_policy(),
        current.iso_display_level(),
        current.dvr_opacity_transfer(layer.dvr_opacity_transfer),
        current.dvr_density_scale(),
    ))
}

pub(crate) fn set_layer_display_state(
    state: &mut AppState,
    layer_index: usize,
    display: LayerDisplay,
    color: ChannelColor,
) -> anyhow::Result<bool> {
    let transfer = state
        .layers
        .get(layer_index)
        .map(|layer| {
            ChannelTransferFunction::new(display, color, layer.curve, layer.preset.clone())
                .map(|transfer| transfer.with_invert(layer.invert))
        })
        .ok_or_else(|| anyhow::anyhow!("layer index {layer_index} is out of range"))??;
    set_layer_transfer_state(state, layer_index, transfer)
}

pub(crate) fn set_layer_transfer_curve(
    state: &mut AppState,
    layer_index: usize,
    curve: TransferCurve,
    preset: TransferPresetId,
) -> anyhow::Result<bool> {
    let layer = state
        .layers
        .get(layer_index)
        .ok_or_else(|| anyhow::anyhow!("layer index {layer_index} is out of range"))?;
    let transfer = ChannelTransferFunction::new(layer.display, layer.color, curve, preset)?
        .with_invert(layer.invert);
    set_layer_transfer_state(state, layer_index, transfer)
}

pub(crate) fn set_layer_transfer_invert(
    state: &mut AppState,
    layer_index: usize,
    invert: bool,
) -> anyhow::Result<bool> {
    let layer = state
        .layers
        .get(layer_index)
        .ok_or_else(|| anyhow::anyhow!("layer index {layer_index} is out of range"))?;
    let transfer = ChannelTransferFunction::new(
        layer.display,
        layer.color,
        layer.curve,
        layer.preset.clone(),
    )?
    .with_invert(invert);
    set_layer_transfer_state(state, layer_index, transfer)
}

pub(crate) fn set_layer_transfer_state(
    state: &mut AppState,
    layer_index: usize,
    transfer: ChannelTransferFunction,
) -> anyhow::Result<bool> {
    let transfer = ChannelTransferFunction::new(
        transfer.display,
        transfer.color,
        transfer.curve,
        transfer.preset,
    )?
    .with_invert(transfer.invert);
    let layer = state
        .layers
        .get_mut(layer_index)
        .ok_or_else(|| anyhow::anyhow!("layer index {layer_index} is out of range"))?;
    let changed = transfer_from_layer_summary(layer) != transfer;
    if !changed {
        return Ok(false);
    }
    layer.display = transfer.display;
    layer.color = transfer.color;
    layer.curve = transfer.curve;
    layer.preset = transfer.preset.clone();
    layer.invert = transfer.invert;
    if layer_index == state.active_layer_index {
        state.active_layer_display = transfer.display;
        state.active_layer_color = transfer.color;
        state.active_layer_transfer = transfer.clone();
    }
    for rendered in &mut state.rendered_channels {
        if rendered.layer_id == layer.id {
            rendered.transfer = transfer.clone();
        }
    }
    state.last_workflow_message = Some(format!("Updated display for {}", layer.name));
    update_channel_fidelity_status(state);
    Ok(true)
}

pub(crate) fn layer_dvr_opacity_transfer(
    state: &AppState,
    layer_index: usize,
) -> anyhow::Result<DvrOpacityTransfer> {
    state
        .layers
        .get(layer_index)
        .map(|layer| {
            if layer_index == state.active_layer_index {
                state.active_dvr_opacity_transfer
            } else {
                layer.dvr_opacity_transfer
            }
        })
        .ok_or_else(|| anyhow::anyhow!("layer index {layer_index} is out of range"))
}

pub(crate) fn set_layer_dvr_opacity_transfer_state(
    state: &mut AppState,
    layer_index: usize,
    transfer: DvrOpacityTransfer,
) -> anyhow::Result<bool> {
    let transfer = DvrOpacityTransfer::new(transfer.window, transfer.curve)?;
    let layer = state
        .layers
        .get_mut(layer_index)
        .ok_or_else(|| anyhow::anyhow!("layer index {layer_index} is out of range"))?;
    if layer.dvr_opacity_transfer == transfer
        && (layer_index != state.active_layer_index
            || state.active_dvr_opacity_transfer == transfer)
    {
        return Ok(false);
    }
    layer.dvr_opacity_transfer = transfer;
    if let ChannelRenderState::Dvr(mut parameters) = layer.render_state {
        parameters.opacity_transfer = transfer;
        layer.render_state = ChannelRenderState::Dvr(parameters);
    }
    if layer_index == state.active_layer_index {
        state.active_dvr_opacity_transfer = transfer;
    }
    state.last_workflow_message = Some(format!("Updated DVR opacity for {}", layer.name));
    Ok(true)
}

pub(crate) fn default_dvr_opacity_transfer(display: LayerDisplay) -> DvrOpacityTransfer {
    DvrOpacityTransfer::new(
        display.window,
        TransferCurve::gamma(crate::DEFAULT_DVR_OPACITY_GAMMA)
            .expect("default DVR opacity gamma is valid"),
    )
    .expect("dataset display window is valid")
}

#[cfg(test)]
pub(crate) fn activate_layer_timepoint(
    state: &mut AppState,
    layer_index: usize,
    timepoint: TimeIndex,
) -> anyhow::Result<()> {
    activate_layer_timepoint_state_only(state, layer_index, timepoint)?;
    rerender_state_with_backend(state, None)?;
    state.last_render_error = None;
    Ok(())
}

pub(crate) fn activate_layer_timepoint_state_only(
    state: &mut AppState,
    layer_index: usize,
    timepoint: TimeIndex,
) -> anyhow::Result<()> {
    let layer = state
        .layers
        .get(layer_index)
        .ok_or_else(|| anyhow::anyhow!("layer index {layer_index} is out of range"))?
        .clone();
    let layer_id = LayerId::new(layer.id.clone())?;
    let transfer = transfer_from_layer_summary(&layer);
    let render_state = layer.render_state;
    let active_render_mode = render_state.mode();
    let active_dvr_opacity_transfer = render_state.dvr_opacity_transfer(layer.dvr_opacity_transfer);
    let iso_display_level = render_state.iso_display_level();
    let dvr_density_scale = render_state.dvr_density_scale();
    let opened = open_initial_scalar_layer(
        &state.dataset,
        &layer_id,
        layer.dtype,
        ScalarLayerOpenOptions {
            display: layer.display,
            transfer: transfer.clone(),
            dvr_opacity_transfer: active_dvr_opacity_transfer,
            presentation_viewport: state.presentation_viewport,
            timepoint,
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
    let reset_camera = state.active_layer_shape.spatial() != layer.shape.spatial();
    let layer_transfer = transfer_from_layer_summary(&layer);
    state.active_layer_index = layer_index;
    state.active_layer_name = layer.name;
    state.active_layer_id = layer.id;
    state.active_layer_shape = layer.shape;
    state.active_layer_dtype = layer.dtype;
    state.active_layer_display = layer.display;
    state.active_layer_color = layer.color;
    state.active_layer_transfer = layer_transfer;
    state.active_render_mode = active_render_mode;
    state.render_sampling_policy = render_state.sampling_policy();
    state.render_iso_shading_policy = render_state.iso_shading_policy();
    state.iso_display_level = iso_display_level;
    state.active_dvr_opacity_transfer = active_dvr_opacity_transfer;
    state.dvr_density_scale = dvr_density_scale;
    state.active_source_shape = source_shape;
    state.active_source_grid_to_world = source_grid_to_world;
    state.active_timepoint = timepoint;
    state.timepoint_count = layer.shape.t;
    state.active_volume_u8 = active_volume_u8;
    state.active_volume = active_volume;
    state.active_volume_f32 = active_volume_f32;
    state.frame = frame;
    state.frame_f32 = frame_f32;
    state.diagnostics = diagnostics;
    state.diagnostics_f32 = diagnostics_f32;
    state.active_intensity_summary = active_intensity_summary;
    set_single_rendered_channel(state);
    state.hovered_pixel = None;
    state.hovered_source_readout = None;
    state.viewer_tools.hover = None;
    state.viewer_tools.selection = None;
    if reset_camera {
        state.camera = camera;
        state.presentation_viewport = presentation_viewport;
        state.render_viewport = render_viewport;
        state.frame_fidelity.presentation_viewport = presentation_viewport;
        state.frame_fidelity.viewport = render_viewport;
    }
    state.active_projection = state.camera.projection;
    rerender_state_with_backend(state, None)?;
    Ok(())
}

pub(crate) fn activate_streaming_timepoint_preserving_frame(
    state: &mut AppState,
    timepoint: TimeIndex,
) -> anyhow::Result<()> {
    if timepoint.0 >= state.timepoint_count {
        anyhow::bail!(
            "timepoint {} is out of range for {} timepoint(s)",
            timepoint.0,
            state.timepoint_count
        );
    }
    if timepoint == state.active_timepoint {
        return Ok(());
    }
    state.active_timepoint = timepoint;
    state.active_volume_u8 = None;
    state.active_volume = None;
    state.active_volume_f32 = None;
    state.frame_f32 = None;
    state.diagnostics_f32 = None;
    state.active_intensity_summary = metadata_intensity_summary(state.active_source_shape)?;
    state.active_histogram_cache = None;
    state.brick_stream_requested = 0;
    state.brick_stream_completed = 0;
    state.brick_stream_cancelled = 0;
    state.brick_stream_stale = 0;
    state.brick_stream_failed = 0;
    state.brick_stream_last_error = None;
    state.brick_stream_complete = false;
    state.brick_stream_request_key = None;
    state.frame_fidelity.completeness = FrameCompleteness::Loading;
    state.frame_fidelity.reason = LodDecisionReason::LoadingTargetScale;
    state.frame_fidelity.frame_time_ms = None;
    reset_prefetch_state(state);
    state.hovered_pixel = None;
    state.hovered_source_readout = None;
    state.viewer_tools.hover = None;
    state.viewer_tools.selection = None;
    update_visible_brick_plan(state);
    update_channel_fidelity_status(state);
    Ok(())
}
