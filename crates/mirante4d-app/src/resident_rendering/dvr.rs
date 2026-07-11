use std::time::Instant;

use mirante4d_core::{ChannelTransferFunction, IntensityDType};
use mirante4d_renderer::{
    DvrResidentChannel, ResidentBrickSetF32, ResidentBrickSetU8, ResidentBrickSetU16,
    gpu::GpuRenderer, render_dvr_channels_from_bricks_with_quality,
};

use crate::{
    AppLayerSummary, AppState, ChannelRenderState, DvrOpacityTransfer, FrameFailureKind,
    LodDecisionReason, RenderBackend, RenderMode, RenderedIntensityChannel,
    render_state::{
        dvr_render_parameters, frame_completeness_for_rendered_scale, placeholder_frame_for_mode,
        record_completed_frame_time, refresh_fidelity_resource_stats,
        resident_render_failure_error, update_channel_fidelity_status,
    },
    viewport::camera_render_quality,
};

use super::{
    dvr_opacity_transfer_for_layer, render_state_for_layer, resident_f32_set_for_layer,
    resident_u8_set_for_layer, resident_u16_set_for_layer, transfer_for_layer,
};

enum ResidentDvrLayerSet {
    U8(ResidentBrickSetU8),
    U16(ResidentBrickSetU16),
    F32(ResidentBrickSetF32),
}

struct ResidentDvrLayerInput {
    layer_id: String,
    transfer: ChannelTransferFunction,
    dvr_opacity_transfer: DvrOpacityTransfer,
    dvr_density_scale: f64,
    set: ResidentDvrLayerSet,
}

pub(super) fn render_dvr_state_from_resident_bricks(
    state: &mut AppState,
    render_start: Instant,
    gpu_renderer: Option<&GpuRenderer>,
    layers: Vec<AppLayerSummary>,
) -> anyhow::Result<()> {
    let mut inputs = Vec::with_capacity(layers.len());
    for layer in layers {
        let render_state = render_state_for_layer(state, &layer);
        let transfer = transfer_for_layer(state, &layer);
        let dvr_opacity_transfer = dvr_opacity_transfer_for_layer(state, &layer, render_state);
        let set = match layer.dtype {
            IntensityDType::Float32 => {
                ResidentDvrLayerSet::F32(resident_f32_set_for_layer(state, &layer)?)
            }
            IntensityDType::Uint8 => {
                ResidentDvrLayerSet::U8(resident_u8_set_for_layer(state, &layer)?)
            }
            IntensityDType::Uint16 => {
                ResidentDvrLayerSet::U16(resident_u16_set_for_layer(state, &layer)?)
            }
        };
        inputs.push(ResidentDvrLayerInput {
            layer_id: layer.id,
            transfer,
            dvr_opacity_transfer,
            dvr_density_scale: render_state.dvr_density_scale(),
            set,
        });
    }

    let channels = inputs
        .iter()
        .map(|input| match &input.set {
            ResidentDvrLayerSet::U8(resident) => DvrResidentChannel::u8(
                resident,
                dvr_render_parameters(
                    &input.transfer,
                    input.dvr_opacity_transfer,
                    input.dvr_density_scale,
                ),
            ),
            ResidentDvrLayerSet::U16(resident) => DvrResidentChannel::u16(
                resident,
                dvr_render_parameters(
                    &input.transfer,
                    input.dvr_opacity_transfer,
                    input.dvr_density_scale,
                ),
            ),
            ResidentDvrLayerSet::F32(resident) => DvrResidentChannel::f32(
                resident,
                dvr_render_parameters(
                    &input.transfer,
                    input.dvr_opacity_transfer,
                    input.dvr_density_scale,
                ),
            ),
        })
        .collect::<Vec<_>>();
    let (frame, diagnostics) = render_dvr_channels_from_bricks_with_quality(
        &channels,
        state.camera.to_camera_state(state.presentation_viewport),
        state.render_viewport,
        camera_render_quality(state),
    )?;
    if !diagnostics.complete {
        return Err(resident_render_failure_error(
            FrameFailureKind::IncompleteResidency,
            format!(
                "resident same-ray DVR renderer reported incomplete frame with {} missing samples",
                diagnostics.missing_voxel_samples
            ),
        ));
    }

    state.frame = frame.clone();
    state.frame_f32 = None;
    state.diagnostics = diagnostics.frame;
    state.diagnostics_f32 = None;
    state.render_backend = RenderBackend::CpuResidentBricks;
    state.rendered_channels = inputs
        .iter()
        .map(|input| RenderedIntensityChannel {
            layer_id: input.layer_id.clone(),
            render_state: ChannelRenderState::for_mode(
                RenderMode::Dvr,
                state.render_sampling_policy,
                state.render_iso_shading_policy,
                state.iso_display_level,
                input.dvr_opacity_transfer,
                input.dvr_density_scale,
            ),
            transfer: input.transfer.clone(),
            frame: placeholder_frame_for_mode(state.render_viewport, RenderMode::Dvr),
            frame_f32: None,
        })
        .collect();
    state.frame_fidelity.displayed_scale_level = Some(state.brick_stream_scale_level);
    state.lod_schedule.displayed_scale_level = Some(state.brick_stream_scale_level);
    state.lod_schedule.pending_scale_level = None;
    state.frame_fidelity.completeness = frame_completeness_for_rendered_scale(
        state.brick_stream_scale_level,
        state.frame_fidelity.target_scale_level,
        state.frame_fidelity.reason,
    );
    state.frame_fidelity.reason = if state.brick_stream_scale_level == 0 {
        LodDecisionReason::ExactS0
    } else {
        state.frame_fidelity.reason
    };
    state.frame_fidelity.backend = state.render_backend;
    record_completed_frame_time(state, render_start);
    state.frame_fidelity.last_failure_kind = None;
    state.frame_fidelity.last_capacity_error = None;
    refresh_fidelity_resource_stats(state, gpu_renderer);
    update_channel_fidelity_status(state);
    Ok(())
}
