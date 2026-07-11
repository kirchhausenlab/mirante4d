use std::time::Instant;

use mirante4d_application::ApplicationSnapshot;
use mirante4d_domain::{DvrOpacityTransfer, IntensityDType, RenderMode, RenderState};
use mirante4d_format::LayerId;
use mirante4d_render_api::CameraFrame;
use mirante4d_renderer::{
    DvrResidentChannel, IntensityTransfer, ResidentBrickSetF32, ResidentBrickSetU8,
    ResidentBrickSetU16, gpu::GpuRenderer, render_dvr_channels_from_bricks_with_quality,
};

use crate::{
    FrameFailureKind, LodDecisionReason, RenderBackend, RenderedIntensityChannel, application_view,
    current_runtime::{dataset::CurrentDatasetRuntime, render::CurrentRenderRuntime},
    render_state::{
        dvr_render_parameters, frame_completeness_for_rendered_scale, placeholder_frame_for_mode,
        record_completed_frame_time, refresh_fidelity_resource_stats,
        resident_render_failure_error, update_channel_fidelity_status,
    },
    viewport::camera_render_quality_for_render_state,
};

use super::{
    ResidentLayer, render_state_for_layer, resident_f32_set_for_layer, resident_u8_set_for_layer,
    resident_u16_set_for_layer, transfer_for_layer,
};

enum ResidentDvrLayerSet {
    U8(ResidentBrickSetU8),
    U16(ResidentBrickSetU16),
    F32(ResidentBrickSetF32),
}

struct ResidentDvrLayerInput {
    layer_id: LayerId,
    render_state: RenderState,
    transfer: IntensityTransfer,
    dvr_opacity_transfer: DvrOpacityTransfer,
    dvr_density_scale: f64,
    set: ResidentDvrLayerSet,
}

pub(super) fn render_dvr_state_from_resident_bricks(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    render_start: Instant,
    gpu_renderer: Option<&GpuRenderer>,
    layers: Vec<ResidentLayer>,
) -> anyhow::Result<()> {
    let mut inputs = Vec::with_capacity(layers.len());
    for layer in layers {
        let render_state = render_state_for_layer(&layer);
        let parameters = render_state.dvr_parameters().ok_or_else(|| {
            resident_render_failure_error(
                FrameFailureKind::InvalidModeParameter,
                format!("layer {} is not configured for DVR", layer.id),
            )
        })?;
        let transfer = transfer_for_layer(&layer);
        let set = match layer.dtype {
            IntensityDType::Float32 => {
                ResidentDvrLayerSet::F32(resident_f32_set_for_layer(snapshot, dataset, &layer)?)
            }
            IntensityDType::Uint8 => {
                ResidentDvrLayerSet::U8(resident_u8_set_for_layer(snapshot, dataset, &layer)?)
            }
            IntensityDType::Uint16 => {
                ResidentDvrLayerSet::U16(resident_u16_set_for_layer(snapshot, dataset, &layer)?)
            }
        };
        inputs.push(ResidentDvrLayerInput {
            layer_id: layer.id,
            render_state,
            transfer,
            dvr_opacity_transfer: parameters.opacity_transfer(),
            dvr_density_scale: parameters.density_scale(),
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
    let view = application_view(snapshot);
    let active_render_state = *view
        .layer(view.active_layer())
        .expect("application view has an active layer")
        .render_state();
    let (frame, diagnostics) = render_dvr_channels_from_bricks_with_quality(
        &channels,
        CameraFrame::new(*view.camera(), render.presentation_viewport)?,
        render.render_viewport,
        camera_render_quality_for_render_state(active_render_state),
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

    render.frame = frame;
    render.frame_f32 = None;
    render.diagnostics = diagnostics.frame;
    render.diagnostics_f32 = None;
    render.render_backend = RenderBackend::CpuResidentBricks;
    render.rendered_channels = inputs
        .iter()
        .map(|input| RenderedIntensityChannel {
            layer_id: input.layer_id.clone(),
            render_state: input.render_state,
            transfer: input.transfer,
            frame: placeholder_frame_for_mode(render.render_viewport, RenderMode::Dvr),
            frame_f32: None,
        })
        .collect();
    render.frame_fidelity.displayed_scale_level = Some(dataset.brick_stream_scale_level);
    render.lod_schedule.displayed_scale_level = Some(dataset.brick_stream_scale_level);
    render.lod_schedule.pending_scale_level = None;
    render.frame_fidelity.completeness = frame_completeness_for_rendered_scale(
        dataset.brick_stream_scale_level,
        render.frame_fidelity.target_scale_level,
        render.frame_fidelity.reason,
    );
    render.frame_fidelity.reason = if dataset.brick_stream_scale_level == 0 {
        LodDecisionReason::ExactS0
    } else {
        render.frame_fidelity.reason
    };
    render.frame_fidelity.backend = render.render_backend;
    record_completed_frame_time(render, render_start);
    render.frame_fidelity.last_failure_kind = None;
    render.frame_fidelity.last_capacity_error = None;
    refresh_fidelity_resource_stats(snapshot, dataset, render, gpu_renderer);
    update_channel_fidelity_status(snapshot, dataset, render);
    Ok(())
}
