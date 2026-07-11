use std::time::Instant;

use mirante4d_domain::{GridToWorld, IsoLightState, Shape3D};
use mirante4d_render_api::{CameraAxes, CameraFrame};
use wgpu::util::DeviceExt;

use super::{
    AdapterDiagnostics, GpuRenderError, GpuRenderTimings, GpuRenderer, WORKGROUP_SIZE_X,
    WORKGROUP_SIZE_Y, add_gpu_render_timings,
    atlas::{GpuBrickAtlasF32Resource, GpuBrickAtlasResource},
    buffers::{
        checked_buffer_byte_count, checked_u32, checked_u64_buffer_byte_count,
        validate_general_buffer_bytes, validate_storage_buffer_bytes,
        validate_uniform_buffer_bytes,
    },
    decode::{GPU_SURFACE_OUTPUT_F32_FIELDS, GPU_SURFACE_OUTPUT_U16_FIELDS},
    duration_ns_u64,
    params::{
        GPU_CAMERA_PARAM_F32_COUNT, GPU_CAMERA_PARAM_F32_UNIFORM_COUNT, apply_gpu_quality_params,
        camera_grid_params_for_transform, projection_code,
    },
};
use crate::{
    CameraRenderMode, CameraRenderModeF32, CameraRenderQuality, DvrRenderParameters,
    IntensitySamplingPolicy, IntensityTransfer, RenderError, RenderViewport, ResidentBrickSetF32,
    ResidentBrickSetU8, ResidentBrickSetU16,
};

const DVR_CHANNEL_U32_STRIDE: usize = 18;
const DVR_CHANNEL_F32_STRIDE: usize = 11;
const ISO_CHANNEL_U32_STRIDE: usize = 5;
const ISO_CHANNEL_F32_STRIDE: usize = 5;
const INTEGER_BRICK_METADATA_WORDS: u64 = 4;
const DVR_CHANNEL_DTYPE_U8: u32 = 0;
const DVR_CHANNEL_DTYPE_U16: u32 = 1;
const DVR_CHANNEL_DTYPE_F32: u32 = 2;

mod dvr_buffers;
use dvr_buffers::*;

fn display_mode_code(mode: CameraRenderMode) -> u32 {
    match mode {
        CameraRenderMode::Mip => 0,
        CameraRenderMode::Isosurface { .. } => 1,
        CameraRenderMode::Dvr { .. } => 2,
    }
}

fn display_mode_code_f32(mode: CameraRenderModeF32) -> u32 {
    match mode {
        CameraRenderModeF32::Mip => 0,
        CameraRenderModeF32::Isosurface { .. } => 1,
        CameraRenderModeF32::Dvr { .. } => 2,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuDisplayFrameDiagnostics {
    pub channels: usize,
    pub output_bytes: u64,
    pub accumulator_bytes: u64,
    pub texture_bytes: u64,
    pub draw_calls: usize,
    pub vertex_count: u64,
}

#[derive(Debug)]
pub struct GpuDisplayFrame {
    pub viewport: RenderViewport,
    pub diagnostics: GpuDisplayFrameDiagnostics,
    pub timings: GpuRenderTimings,
    pub adapter: AdapterDiagnostics,
    pub(super) texture: wgpu::Texture,
    pub(super) view: wgpu::TextureView,
}

impl GpuDisplayFrame {
    pub fn texture_view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuDisplayFrameBlendMode {
    Additive,
    SourceOver,
}

pub enum GpuResidentDisplayChannel<'a> {
    U8 {
        resident: &'a ResidentBrickSetU8,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        mode: CameraRenderMode,
        transfer: IntensityTransfer,
    },
    U16 {
        resident: &'a ResidentBrickSetU16,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        mode: CameraRenderMode,
        transfer: IntensityTransfer,
    },
    F32 {
        resident: &'a ResidentBrickSetF32,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        mode: CameraRenderModeF32,
        transfer: IntensityTransfer,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct GpuResidentDisplayRequest {
    pub camera: CameraFrame,
    pub viewport: RenderViewport,
    pub quality: CameraRenderQuality,
    pub iso_light_state: IsoLightState,
    pub camera_axes: CameraAxes,
}

impl GpuResidentDisplayChannel<'_> {
    fn is_iso(&self) -> bool {
        matches!(
            self,
            Self::U8 {
                mode: CameraRenderMode::Isosurface { .. },
                ..
            } | Self::U16 {
                mode: CameraRenderMode::Isosurface { .. },
                ..
            } | Self::F32 {
                mode: CameraRenderModeF32::Isosurface { .. },
                ..
            }
        )
    }

    fn is_dvr(&self) -> bool {
        matches!(
            self,
            Self::U8 {
                mode: CameraRenderMode::Dvr { .. },
                ..
            } | Self::U16 {
                mode: CameraRenderMode::Dvr { .. },
                ..
            } | Self::F32 {
                mode: CameraRenderModeF32::Dvr { .. },
                ..
            }
        )
    }

    fn dvr_parameters(&self) -> Option<DvrRenderParameters> {
        match self {
            Self::U8 {
                mode: CameraRenderMode::Dvr { parameters },
                ..
            }
            | Self::U16 {
                mode: CameraRenderMode::Dvr { parameters },
                ..
            } => Some(*parameters),
            Self::F32 {
                mode: CameraRenderModeF32::Dvr { parameters },
                ..
            } => Some(*parameters),
            Self::U8 { .. } | Self::U16 { .. } | Self::F32 { .. } => None,
        }
    }

    fn transfer(&self) -> &IntensityTransfer {
        match self {
            Self::U8 { transfer, .. } | Self::U16 { transfer, .. } | Self::F32 { transfer, .. } => {
                transfer
            }
        }
    }

    fn volume_shape(&self) -> Shape3D {
        match self {
            Self::U8 { resident, .. } => resident.volume_shape,
            Self::U16 { resident, .. } => resident.volume_shape,
            Self::F32 { resident, .. } => resident.volume_shape,
        }
    }

    fn grid_to_world(&self) -> GridToWorld {
        match self {
            Self::U8 { resident, .. } => resident.grid_to_world,
            Self::U16 { resident, .. } => resident.grid_to_world,
            Self::F32 { resident, .. } => resident.grid_to_world,
        }
    }
}

enum GpuDvrDisplayAtlas {
    Integer(GpuBrickAtlasResource),
    F32(GpuBrickAtlasF32Resource),
}

struct GpuDvrDisplayAtlasChannel {
    atlas: GpuDvrDisplayAtlas,
    parameters: DvrRenderParameters,
    display_visible: bool,
}

#[derive(Debug, Clone, Copy)]
struct GpuDvrCombinedOffsets {
    packed_values_words: u64,
    validity_words: u64,
    f32_values_words: u64,
    page_table_words: u64,
    metadata_words: u64,
}

#[derive(Debug, Clone, Copy)]
struct GpuDvrAtlasBufferWords {
    packed_values: u64,
    validity: u64,
    f32_values: u64,
    page_table: u64,
    metadata: u64,
}

#[derive(Debug, Clone, Copy)]
struct GpuDvrCombinedBufferSet<'a> {
    packed_values_buffer: &'a wgpu::Buffer,
    validity_buffer: &'a wgpu::Buffer,
    f32_values_buffer: &'a wgpu::Buffer,
    page_table_buffer: &'a wgpu::Buffer,
    metadata_buffer: &'a wgpu::Buffer,
}

struct GpuIsoDisplayOutputChannel {
    output_buffer: wgpu::Buffer,
    output_bytes: u64,
    timings: GpuRenderTimings,
    dtype: u32,
    record_stride_words: u32,
    transfer: IntensityTransfer,
}

impl GpuRenderer {
    pub fn detach_display_frame_texture(
        &self,
        frame: GpuDisplayFrame,
    ) -> Result<GpuDisplayFrame, GpuRenderError> {
        let viewport_width = checked_u32("viewport_width", frame.viewport.width)?;
        let viewport_height = checked_u32("viewport_height", frame.viewport.height)?;
        let pixel_count = (u64::from(viewport_width) * u64::from(viewport_height)) as usize;
        let (texture, view, texture_bytes) = self.create_owned_display_texture(
            "mirante4d-detached-display-rgba-texture",
            viewport_width,
            viewport_height,
            pixel_count,
        )?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-detached-display-frame-command-encoder"),
            });
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: frame.texture(),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: viewport_width,
                height: viewport_height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));
        Ok(GpuDisplayFrame {
            viewport: frame.viewport,
            diagnostics: GpuDisplayFrameDiagnostics {
                texture_bytes: frame
                    .diagnostics
                    .texture_bytes
                    .saturating_add(texture_bytes),
                ..frame.diagnostics
            },
            timings: frame.timings,
            adapter: frame.adapter,
            texture,
            view,
        })
    }

    pub fn blend_display_frames_to_texture(
        &self,
        base: GpuDisplayFrame,
        overlay: &GpuDisplayFrame,
        mode: GpuDisplayFrameBlendMode,
    ) -> Result<GpuDisplayFrame, GpuRenderError> {
        if base.viewport != overlay.viewport {
            return Err(RenderError::InvalidChannelComposite(
                "GPU display frame blending requires matching viewports",
            )
            .into());
        }
        let viewport_width = checked_u32("viewport_width", base.viewport.width)?;
        let viewport_height = checked_u32("viewport_height", base.viewport.height)?;
        let pixel_count = (u64::from(viewport_width) * u64::from(viewport_height)) as usize;
        let (texture, view, texture_bytes) = self.create_owned_display_texture(
            "mirante4d-blended-display-rgba-texture",
            viewport_width,
            viewport_height,
            pixel_count,
        )?;
        let params = [
            viewport_width,
            viewport_height,
            match mode {
                GpuDisplayFrameBlendMode::Additive => 0,
                GpuDisplayFrameBlendMode::SourceOver => 1,
            },
            0,
        ];
        let params_bytes = checked_buffer_byte_count(
            "GPU display frame blend parameters",
            params.len(),
            std::mem::size_of::<u32>(),
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "GPU display frame blend parameters",
            params_bytes,
        )?;
        let upload_started = Instant::now();
        let params_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mirante4d-display-frame-blend-params-u32"),
                contents: bytemuck::cast_slice(&params),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mirante4d-display-frame-blend-bind-group"),
            layout: &self.display_frame_blend_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(base.texture_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(overlay.texture_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: params_buffer.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-display-frame-blend-command-encoder"),
            });
        let timestamp = self.timestamp_query_pair("mirante4d-display-frame-blend-timestamps");
        let timestamp_writes = timestamp
            .as_ref()
            .map(super::readback::GpuTimestampQueryPair::compute_pass_writes);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-display-frame-blend-pass"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.display_frame_blend_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                viewport_width.div_ceil(WORKGROUP_SIZE_X),
                viewport_height.div_ceil(WORKGROUP_SIZE_Y),
                1,
            );
        }
        let blend_timing = GpuRenderTimings {
            upload_ns: duration_ns_u64(upload_started.elapsed()),
            gpu_compute_ns: self.submit_with_optional_timestamp(encoder, timestamp)?,
        };
        let timings = add_gpu_render_timings(
            add_gpu_render_timings(base.timings, overlay.timings),
            blend_timing,
        );
        Ok(GpuDisplayFrame {
            viewport: base.viewport,
            diagnostics: GpuDisplayFrameDiagnostics {
                channels: base
                    .diagnostics
                    .channels
                    .saturating_add(overlay.diagnostics.channels),
                output_bytes: base
                    .diagnostics
                    .output_bytes
                    .saturating_add(overlay.diagnostics.output_bytes),
                accumulator_bytes: base
                    .diagnostics
                    .accumulator_bytes
                    .saturating_add(overlay.diagnostics.accumulator_bytes),
                texture_bytes: base
                    .diagnostics
                    .texture_bytes
                    .saturating_add(overlay.diagnostics.texture_bytes)
                    .saturating_add(texture_bytes),
                draw_calls: base
                    .diagnostics
                    .draw_calls
                    .saturating_add(overlay.diagnostics.draw_calls)
                    .saturating_add(1),
                vertex_count: base
                    .diagnostics
                    .vertex_count
                    .saturating_add(overlay.diagnostics.vertex_count),
            },
            timings,
            adapter: base.adapter,
            texture,
            view,
        })
    }

    fn create_owned_display_texture(
        &self,
        label: &'static str,
        width: u32,
        height: u32,
        pixel_count: usize,
    ) -> Result<(wgpu::Texture, wgpu::TextureView, u64), GpuRenderError> {
        let texture_bytes = checked_buffer_byte_count("GPU display texture", pixel_count, 4)?;
        validate_general_buffer_bytes(&self.device.limits(), "GPU display texture", texture_bytes)?;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok((texture, view, texture_bytes))
    }

    fn render_iso_channels_to_display_texture(
        &self,
        channels: &[GpuResidentDisplayChannel<'_>],
        camera: CameraFrame,
        viewport: RenderViewport,
        quality: CameraRenderQuality,
        iso_light_state: IsoLightState,
        camera_axes: CameraAxes,
    ) -> Result<GpuDisplayFrame, GpuRenderError> {
        if channels.is_empty() {
            return Err(RenderError::InvalidChannelComposite(
                "GPU display ISO rendering requires at least one channel",
            )
            .into());
        }
        if !channels.iter().all(GpuResidentDisplayChannel::is_iso) {
            return Err(GpuRenderError::UnsupportedCameraMode(
                "multi-channel GPU display ISO requires ISO channels",
            ));
        }

        let viewport_width = checked_u32("viewport_width", viewport.width)?;
        let viewport_height = checked_u32("viewport_height", viewport.height)?;
        let channel_count = checked_u32("ISO channel count", channels.len() as u64)?;
        let pixel_count = (u64::from(viewport_width) * u64::from(viewport_height)) as usize;
        let accumulator_bytes =
            checked_buffer_byte_count("GPU display accumulator", pixel_count, 16)?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "GPU display accumulator",
            accumulator_bytes,
        )?;
        let texture_bytes = checked_buffer_byte_count("GPU display texture", pixel_count, 4)?;
        validate_general_buffer_bytes(&self.device.limits(), "GPU display texture", texture_bytes)?;
        let display_resources = self.display_resources_for_viewport(
            viewport,
            viewport_width,
            viewport_height,
            accumulator_bytes,
            texture_bytes,
        )?;

        let mut rendered_channels = Vec::with_capacity(channels.len());
        let mut timings = GpuRenderTimings::default();
        let mut output_bytes_total = 0_u64;
        let mut integer_output_slot = 0_usize;
        let mut f32_output_slot = 0_usize;
        for channel in channels {
            let channel_started = Instant::now();
            let rendered = match channel {
                GpuResidentDisplayChannel::U8 {
                    resident,
                    brick_shape,
                    brick_grid_shape,
                    mode,
                    transfer,
                } => {
                    let CameraRenderMode::Isosurface { .. } = mode else {
                        return Err(GpuRenderError::UnsupportedCameraMode(
                            "multi-channel GPU display ISO requires ISO channels",
                        ));
                    };
                    let output_slot = integer_output_slot;
                    integer_output_slot = integer_output_slot.checked_add(1).ok_or(
                        GpuRenderError::BufferSizeOverflow {
                            resource: "integer ISO output slots",
                        },
                    )?;
                    let output = self.render_camera_u8_from_bricks_output_buffer(
                        resident,
                        *brick_shape,
                        *brick_grid_shape,
                        camera,
                        viewport,
                        *mode,
                        quality,
                        output_slot,
                    )?;
                    GpuIsoDisplayOutputChannel {
                        output_buffer: output.output_buffer,
                        output_bytes: output.output_bytes,
                        timings: output.timings,
                        dtype: 0,
                        record_stride_words: checked_u32(
                            "integer ISO output record stride",
                            GPU_SURFACE_OUTPUT_U16_FIELDS as u64,
                        )?,
                        transfer: *transfer,
                    }
                }
                GpuResidentDisplayChannel::U16 {
                    resident,
                    brick_shape,
                    brick_grid_shape,
                    mode,
                    transfer,
                } => {
                    let CameraRenderMode::Isosurface { .. } = mode else {
                        return Err(GpuRenderError::UnsupportedCameraMode(
                            "multi-channel GPU display ISO requires ISO channels",
                        ));
                    };
                    let output_slot = integer_output_slot;
                    integer_output_slot = integer_output_slot.checked_add(1).ok_or(
                        GpuRenderError::BufferSizeOverflow {
                            resource: "integer ISO output slots",
                        },
                    )?;
                    let output = self.render_camera_from_bricks_output_buffer(
                        resident,
                        *brick_shape,
                        *brick_grid_shape,
                        camera,
                        viewport,
                        *mode,
                        quality,
                        output_slot,
                    )?;
                    GpuIsoDisplayOutputChannel {
                        output_buffer: output.output_buffer,
                        output_bytes: output.output_bytes,
                        timings: output.timings,
                        dtype: 0,
                        record_stride_words: checked_u32(
                            "integer ISO output record stride",
                            GPU_SURFACE_OUTPUT_U16_FIELDS as u64,
                        )?,
                        transfer: *transfer,
                    }
                }
                GpuResidentDisplayChannel::F32 {
                    resident,
                    brick_shape,
                    brick_grid_shape,
                    mode,
                    transfer,
                } => {
                    let CameraRenderModeF32::Isosurface { .. } = mode else {
                        return Err(GpuRenderError::UnsupportedCameraMode(
                            "multi-channel GPU display ISO requires ISO channels",
                        ));
                    };
                    let output_slot = f32_output_slot;
                    f32_output_slot = f32_output_slot.checked_add(1).ok_or(
                        GpuRenderError::BufferSizeOverflow {
                            resource: "float32 ISO output slots",
                        },
                    )?;
                    let output = self.render_camera_f32_from_bricks_output_buffer(
                        resident,
                        *brick_shape,
                        *brick_grid_shape,
                        camera,
                        viewport,
                        *mode,
                        quality,
                        output_slot,
                    )?;
                    GpuIsoDisplayOutputChannel {
                        output_buffer: output.output_buffer,
                        output_bytes: output.output_bytes,
                        timings: output.timings,
                        dtype: 1,
                        record_stride_words: checked_u32(
                            "float32 ISO output record stride",
                            GPU_SURFACE_OUTPUT_F32_FIELDS as u64,
                        )?,
                        transfer: *transfer,
                    }
                }
            };
            timings = add_gpu_render_timings(timings, rendered.timings);
            timings.upload_ns = timings
                .upload_ns
                .saturating_add(duration_ns_u64(channel_started.elapsed()));
            output_bytes_total = output_bytes_total
                .checked_add(rendered.output_bytes)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel ISO outputs",
                })?;
            rendered_channels.push(rendered);
        }

        validate_storage_buffer_bytes(
            &self.device.limits(),
            "multi-channel ISO outputs",
            output_bytes_total,
        )?;

        let channel_u32_len = channels.len().checked_mul(ISO_CHANNEL_U32_STRIDE).ok_or(
            GpuRenderError::BufferSizeOverflow {
                resource: "multi-channel ISO u32 parameters",
            },
        )?;
        let channel_f32_len = channels.len().checked_mul(ISO_CHANNEL_F32_STRIDE).ok_or(
            GpuRenderError::BufferSizeOverflow {
                resource: "multi-channel ISO f32 parameters",
            },
        )?;
        let mut channel_params_u32 = Vec::with_capacity(channel_u32_len);
        let mut channel_params_f32 = Vec::with_capacity(channel_f32_len);
        let mut output_offset_words = 0_u64;
        for channel in &rendered_channels {
            let transfer = &channel.transfer;
            channel_params_u32.extend_from_slice(&[
                channel.dtype,
                u32::from(transfer.visible()),
                0,
                checked_u32("ISO output record offset", output_offset_words)?,
                channel.record_stride_words,
            ]);
            channel_params_f32.extend_from_slice(&[
                transfer.opacity().get(),
                transfer.color_rgba()[0],
                transfer.color_rgba()[1],
                transfer.color_rgba()[2],
                transfer.color_rgba()[3],
            ]);
            output_offset_words = output_offset_words
                .checked_add(channel.output_bytes / std::mem::size_of::<u32>() as u64)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel ISO output word offsets",
                })?;
        }
        debug_assert_eq!(channel_params_u32.len(), channel_u32_len);
        debug_assert_eq!(channel_params_f32.len(), channel_f32_len);
        let channel_u32_bytes = checked_buffer_byte_count(
            "multi-channel ISO u32 parameters",
            channel_params_u32.len(),
            std::mem::size_of::<u32>(),
        )?;
        let channel_f32_bytes = checked_buffer_byte_count(
            "multi-channel ISO f32 parameters",
            channel_params_f32.len(),
            std::mem::size_of::<f32>(),
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "multi-channel ISO u32 parameters",
            channel_u32_bytes,
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "multi-channel ISO f32 parameters",
            channel_f32_bytes,
        )?;

        let light_world = crate::current_camera::iso_light_direction(iso_light_state, camera_axes);
        let (camera_forward, camera_right, camera_up) =
            crate::current_camera::axes_vectors(camera_axes);
        let frame_params_u32 = [
            viewport_width,
            viewport_height,
            channel_count,
            projection_code(crate::current_camera::projection(camera)),
        ];
        let frame_params_f32 = [
            light_world.x as f32,
            light_world.y as f32,
            light_world.z as f32,
            camera_forward.x as f32,
            camera_forward.y as f32,
            camera_forward.z as f32,
            camera_right.x as f32,
            camera_right.y as f32,
            camera_right.z as f32,
            camera_up.x as f32,
            camera_up.y as f32,
            camera_up.z as f32,
            crate::current_camera::perspective_focal_length_screen_points(camera) as f32,
            crate::current_camera::presentation_width_points(camera) as f32,
            crate::current_camera::presentation_height_points(camera) as f32,
        ];
        let frame_u32_bytes = checked_buffer_byte_count(
            "multi-channel ISO frame u32 parameters",
            frame_params_u32.len(),
            std::mem::size_of::<u32>(),
        )?;
        let frame_f32_bytes = checked_buffer_byte_count(
            "multi-channel ISO frame f32 parameters",
            frame_params_f32.len(),
            std::mem::size_of::<f32>(),
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "multi-channel ISO frame u32 parameters",
            frame_u32_bytes,
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "multi-channel ISO frame f32 parameters",
            frame_f32_bytes,
        )?;
        let iso_resources = self.iso_multi_channel_display_resources(
            viewport,
            output_bytes_total,
            channel_u32_bytes,
            channel_f32_bytes,
            frame_u32_bytes,
            frame_f32_bytes,
        )?;
        self.queue.write_buffer(
            &iso_resources.channel_params_u32_buffer,
            0,
            bytemuck::cast_slice(&channel_params_u32),
        );
        self.queue.write_buffer(
            &iso_resources.channel_params_f32_buffer,
            0,
            bytemuck::cast_slice(&channel_params_f32),
        );
        self.queue.write_buffer(
            &iso_resources.frame_params_u32_buffer,
            0,
            bytemuck::cast_slice(&frame_params_u32),
        );
        self.queue.write_buffer(
            &iso_resources.frame_params_f32_buffer,
            0,
            bytemuck::cast_slice(&frame_params_f32),
        );

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-display-iso-multi-channel-command-encoder"),
            });
        let mut output_offset_bytes = 0_u64;
        for channel in &rendered_channels {
            encoder.copy_buffer_to_buffer(
                &channel.output_buffer,
                0,
                &iso_resources.combined_output_buffer,
                output_offset_bytes as wgpu::BufferAddress,
                channel.output_bytes as wgpu::BufferAddress,
            );
            output_offset_bytes = output_offset_bytes
                .checked_add(channel.output_bytes)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel ISO output byte offsets",
                })?;
        }
        let timestamp = self.timestamp_query_pair("mirante4d-display-iso-multi-channel-timestamp");
        let timestamp_writes = timestamp
            .as_ref()
            .map(|timestamp| timestamp.compute_pass_writes());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-display-iso-multi-channel-pass"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.display_iso_multi_channel_pipeline);
            pass.set_bind_group(0, &iso_resources.bind_group, &[]);
            pass.dispatch_workgroups(
                viewport_width.div_ceil(WORKGROUP_SIZE_X),
                viewport_height.div_ceil(WORKGROUP_SIZE_Y),
                1,
            );
        }
        let compositor_timing = GpuRenderTimings {
            gpu_compute_ns: self.submit_with_optional_timestamp(encoder, timestamp)?,
            ..Default::default()
        };
        timings = add_gpu_render_timings(timings, compositor_timing);

        Ok(GpuDisplayFrame {
            viewport,
            diagnostics: GpuDisplayFrameDiagnostics {
                channels: channels.len(),
                output_bytes: output_bytes_total.saturating_mul(2),
                accumulator_bytes: display_resources.accumulator_bytes,
                texture_bytes: display_resources.texture_bytes,
                draw_calls: 0,
                vertex_count: 0,
            },
            timings,
            adapter: self.adapter.clone(),
            texture: display_resources.texture,
            view: display_resources.view,
        })
    }

    fn dvr_display_atlas_channel(
        &self,
        channel: &GpuResidentDisplayChannel<'_>,
    ) -> Result<GpuDvrDisplayAtlasChannel, GpuRenderError> {
        let Some(parameters) = channel.dvr_parameters() else {
            return Err(GpuRenderError::UnsupportedCameraMode(
                "multi-channel GPU display DVR requires DVR channels",
            ));
        };
        let display_visible = channel.transfer().visible();
        let atlas = match channel {
            GpuResidentDisplayChannel::U8 {
                resident,
                brick_shape,
                brick_grid_shape,
                ..
            } => {
                if resident.bricks().is_empty() {
                    return Err(RenderError::InvalidBrickAtlas(
                        "resident uint8 brick set is empty",
                    )
                    .into());
                }
                GpuDvrDisplayAtlas::Integer(self.cached_brick_atlas_u8(
                    resident,
                    *brick_shape,
                    *brick_grid_shape,
                )?)
            }
            GpuResidentDisplayChannel::U16 {
                resident,
                brick_shape,
                brick_grid_shape,
                ..
            } => {
                if resident.bricks().is_empty() {
                    return Err(
                        RenderError::InvalidBrickAtlas("resident brick set is empty").into(),
                    );
                }
                GpuDvrDisplayAtlas::Integer(self.cached_brick_atlas(
                    resident,
                    *brick_shape,
                    *brick_grid_shape,
                )?)
            }
            GpuResidentDisplayChannel::F32 {
                resident,
                brick_shape,
                brick_grid_shape,
                ..
            } => {
                if resident.bricks().is_empty() {
                    return Err(RenderError::InvalidBrickAtlas(
                        "resident float32 brick set is empty",
                    )
                    .into());
                }
                GpuDvrDisplayAtlas::F32(self.cached_brick_atlas_f32(
                    resident,
                    *brick_shape,
                    *brick_grid_shape,
                )?)
            }
        };
        Ok(GpuDvrDisplayAtlasChannel {
            atlas,
            parameters,
            display_visible,
        })
    }

    fn render_dvr_channels_to_display_texture(
        &self,
        channels: &[GpuResidentDisplayChannel<'_>],
        camera: CameraFrame,
        viewport: RenderViewport,
        quality: CameraRenderQuality,
    ) -> Result<GpuDisplayFrame, GpuRenderError> {
        let Some(first) = channels.first() else {
            return Err(RenderError::InvalidDvrChannelSet(
                "at least one visible resident channel is required",
            )
            .into());
        };
        let volume_shape = first.volume_shape();
        if volume_shape.element_count().map_err(RenderError::from)? == 0 {
            return Err(RenderError::EmptyVolume.into());
        }
        let grid_to_world = first.grid_to_world();
        for channel in channels {
            if channel.volume_shape() != volume_shape {
                return Err(RenderError::InvalidDvrChannelSet(
                    "all resident DVR channels must share one grid shape",
                )
                .into());
            }
            if channel.grid_to_world() != grid_to_world {
                return Err(RenderError::InvalidDvrChannelSet(
                    "all resident DVR channels must share one grid transform",
                )
                .into());
            }
            if !channel.is_dvr() {
                return Err(GpuRenderError::UnsupportedCameraMode(
                    "multi-channel GPU display DVR requires DVR channels",
                ));
            }
        }

        let viewport_width = checked_u32("viewport_width", viewport.width)?;
        let viewport_height = checked_u32("viewport_height", viewport.height)?;
        let shape_x = checked_u32("x", volume_shape.x())?;
        let shape_y = checked_u32("y", volume_shape.y())?;
        let shape_z = checked_u32("z", volume_shape.z())?;
        let channel_count = checked_u32("DVR channel count", channels.len() as u64)?;
        let pixel_count = (u64::from(viewport_width) * u64::from(viewport_height)) as usize;
        let accumulator_bytes =
            checked_buffer_byte_count("GPU display accumulator", pixel_count, 16)?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "GPU display accumulator",
            accumulator_bytes,
        )?;
        let texture_bytes = checked_buffer_byte_count("GPU display texture", pixel_count, 4)?;
        validate_general_buffer_bytes(&self.device.limits(), "GPU display texture", texture_bytes)?;
        let display_resources = self.display_resources_for_viewport(
            viewport,
            viewport_width,
            viewport_height,
            accumulator_bytes,
            texture_bytes,
        )?;

        let upload_started = Instant::now();
        let atlas_channels = channels
            .iter()
            .map(|channel| self.dvr_display_atlas_channel(channel))
            .collect::<Result<Vec<_>, _>>()?;
        let mut timings = GpuRenderTimings {
            upload_ns: duration_ns_u64(upload_started.elapsed()),
            ..Default::default()
        };

        let mut buffer_words = Vec::with_capacity(atlas_channels.len());
        let mut total_packed_values_words = 0_u64;
        let mut total_validity_words = 0_u64;
        let mut total_f32_values_words = 0_u64;
        let mut total_page_table_words = 0_u64;
        let mut total_metadata_words = 0_u64;
        for channel in &atlas_channels {
            let words = dvr_atlas_buffer_words(channel)?;
            total_packed_values_words = total_packed_values_words
                .checked_add(words.packed_values)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel DVR packed values",
                })?;
            total_validity_words = total_validity_words.checked_add(words.validity).ok_or(
                GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel DVR validity",
                },
            )?;
            total_f32_values_words = total_f32_values_words.checked_add(words.f32_values).ok_or(
                GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel DVR float32 values",
                },
            )?;
            total_page_table_words = total_page_table_words.checked_add(words.page_table).ok_or(
                GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel DVR page tables",
                },
            )?;
            total_metadata_words = total_metadata_words.checked_add(words.metadata).ok_or(
                GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel DVR metadata",
                },
            )?;
            buffer_words.push(words);
        }
        let allocated_packed_values_words = total_packed_values_words.max(1);
        let allocated_validity_words = total_validity_words.max(1);
        let allocated_f32_values_words = total_f32_values_words.max(1);
        let allocated_page_table_words = total_page_table_words.max(1);
        let allocated_metadata_words = total_metadata_words.max(1);
        let packed_values_bytes = checked_u64_buffer_byte_count(
            "multi-channel DVR packed values",
            allocated_packed_values_words,
            std::mem::size_of::<u32>() as u64,
        )?;
        let validity_bytes = checked_u64_buffer_byte_count(
            "multi-channel DVR validity",
            allocated_validity_words,
            std::mem::size_of::<u32>() as u64,
        )?;
        let f32_values_bytes = checked_u64_buffer_byte_count(
            "multi-channel DVR float32 values",
            allocated_f32_values_words,
            std::mem::size_of::<f32>() as u64,
        )?;
        let page_table_bytes = checked_u64_buffer_byte_count(
            "multi-channel DVR page tables",
            allocated_page_table_words,
            std::mem::size_of::<u32>() as u64,
        )?;
        let metadata_bytes = checked_u64_buffer_byte_count(
            "multi-channel DVR metadata",
            allocated_metadata_words,
            std::mem::size_of::<u32>() as u64,
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "multi-channel DVR packed values",
            packed_values_bytes,
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "multi-channel DVR validity",
            validity_bytes,
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "multi-channel DVR float32 values",
            f32_values_bytes,
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "multi-channel DVR page tables",
            page_table_bytes,
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "multi-channel DVR metadata",
            metadata_bytes,
        )?;

        let channel_u32_len = channels.len().checked_mul(DVR_CHANNEL_U32_STRIDE).ok_or(
            GpuRenderError::BufferSizeOverflow {
                resource: "multi-channel DVR u32 parameters",
            },
        )?;
        let channel_f32_len = channels.len().checked_mul(DVR_CHANNEL_F32_STRIDE).ok_or(
            GpuRenderError::BufferSizeOverflow {
                resource: "multi-channel DVR f32 parameters",
            },
        )?;
        let mut channel_params_u32 = Vec::with_capacity(channel_u32_len);
        let mut channel_params_f32 = Vec::with_capacity(channel_f32_len);
        let mut offsets = GpuDvrCombinedOffsets {
            packed_values_words: 0,
            validity_words: 0,
            f32_values_words: 0,
            page_table_words: 0,
            metadata_words: 0,
        };
        for (channel, words) in atlas_channels.iter().zip(buffer_words.iter().copied()) {
            push_dvr_channel_descriptors(
                &mut channel_params_u32,
                &mut channel_params_f32,
                channel,
                offsets,
            )?;
            offsets.packed_values_words = offsets
                .packed_values_words
                .checked_add(words.packed_values)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel DVR packed values",
                })?;
            offsets.validity_words = offsets.validity_words.checked_add(words.validity).ok_or(
                GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel DVR validity",
                },
            )?;
            offsets.f32_values_words = offsets
                .f32_values_words
                .checked_add(words.f32_values)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel DVR float32 values",
                })?;
            offsets.page_table_words = offsets
                .page_table_words
                .checked_add(words.page_table)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel DVR page tables",
                })?;
            offsets.metadata_words = offsets.metadata_words.checked_add(words.metadata).ok_or(
                GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel DVR metadata",
                },
            )?;
        }
        debug_assert_eq!(channel_params_u32.len(), channel_u32_len);
        debug_assert_eq!(channel_params_f32.len(), channel_f32_len);
        let channel_u32_bytes = checked_buffer_byte_count(
            "multi-channel DVR u32 parameters",
            channel_params_u32.len(),
            std::mem::size_of::<u32>(),
        )?;
        let channel_f32_bytes = checked_buffer_byte_count(
            "multi-channel DVR f32 parameters",
            channel_params_f32.len(),
            std::mem::size_of::<f32>(),
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "multi-channel DVR u32 parameters",
            channel_u32_bytes,
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "multi-channel DVR f32 parameters",
            channel_f32_bytes,
        )?;

        let mut camera_params = camera_grid_params_for_transform(grid_to_world, camera, viewport)?;
        apply_gpu_quality_params(&mut camera_params, quality);
        let mut frame_params_f32 = [0.0; GPU_CAMERA_PARAM_F32_UNIFORM_COUNT];
        frame_params_f32[..GPU_CAMERA_PARAM_F32_COUNT].copy_from_slice(&camera_params);
        let sampling_policy = match quality.intensity_sampling {
            IntensitySamplingPolicy::VoxelExact => 0,
            IntensitySamplingPolicy::SmoothLinear => 1,
        };
        let frame_params_u32 = [
            viewport_width,
            viewport_height,
            shape_x,
            shape_y,
            shape_z,
            projection_code(crate::current_camera::projection(camera)),
            channel_count,
            sampling_policy,
        ];
        let frame_u32_bytes = checked_buffer_byte_count(
            "multi-channel DVR frame u32 parameters",
            frame_params_u32.len(),
            std::mem::size_of::<u32>(),
        )?;
        let frame_f32_bytes = checked_buffer_byte_count(
            "multi-channel DVR frame f32 parameters",
            frame_params_f32.len(),
            std::mem::size_of::<f32>(),
        )?;
        validate_uniform_buffer_bytes(
            &self.device.limits(),
            "multi-channel DVR frame u32 parameters",
            frame_u32_bytes,
        )?;
        validate_uniform_buffer_bytes(
            &self.device.limits(),
            "multi-channel DVR frame f32 parameters",
            frame_f32_bytes,
        )?;
        let dvr_resources = self.dvr_multi_channel_display_resources(
            viewport,
            packed_values_bytes,
            validity_bytes,
            f32_values_bytes,
            page_table_bytes,
            metadata_bytes,
            channel_u32_bytes,
            channel_f32_bytes,
            frame_u32_bytes,
            frame_f32_bytes,
        )?;
        self.queue.write_buffer(
            &dvr_resources.channel_params_u32_buffer,
            0,
            bytemuck::cast_slice(&channel_params_u32),
        );
        self.queue.write_buffer(
            &dvr_resources.channel_params_f32_buffer,
            0,
            bytemuck::cast_slice(&channel_params_f32),
        );
        self.queue.write_buffer(
            &dvr_resources.frame_params_u32_buffer,
            0,
            bytemuck::cast_slice(&frame_params_u32),
        );
        self.queue.write_buffer(
            &dvr_resources.frame_params_f32_buffer,
            0,
            bytemuck::cast_slice(&frame_params_f32),
        );

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-display-dvr-multi-channel-command-encoder"),
            });
        let mut copy_offsets = GpuDvrCombinedOffsets {
            packed_values_words: 0,
            validity_words: 0,
            f32_values_words: 0,
            page_table_words: 0,
            metadata_words: 0,
        };
        let combined_buffers = GpuDvrCombinedBufferSet {
            packed_values_buffer: &dvr_resources.packed_values_buffer,
            validity_buffer: &dvr_resources.validity_buffer,
            f32_values_buffer: &dvr_resources.f32_values_buffer,
            page_table_buffer: &dvr_resources.page_table_buffer,
            metadata_buffer: &dvr_resources.metadata_buffer,
        };
        for (channel, words) in atlas_channels.iter().zip(buffer_words.iter().copied()) {
            copy_dvr_channel_atlas_buffers(
                &mut encoder,
                channel,
                words,
                copy_offsets,
                combined_buffers,
            )?;
            copy_offsets.packed_values_words = copy_offsets
                .packed_values_words
                .checked_add(words.packed_values)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel DVR packed values",
                })?;
            copy_offsets.validity_words = copy_offsets
                .validity_words
                .checked_add(words.validity)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                resource: "multi-channel DVR validity",
            })?;
            copy_offsets.f32_values_words = copy_offsets
                .f32_values_words
                .checked_add(words.f32_values)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel DVR float32 values",
                })?;
            copy_offsets.page_table_words = copy_offsets
                .page_table_words
                .checked_add(words.page_table)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel DVR page tables",
                })?;
            copy_offsets.metadata_words = copy_offsets
                .metadata_words
                .checked_add(words.metadata)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                resource: "multi-channel DVR metadata",
            })?;
        }
        let timestamp = self.timestamp_query_pair("mirante4d-display-dvr-multi-channel-timestamp");
        let timestamp_writes = timestamp
            .as_ref()
            .map(|timestamp| timestamp.compute_pass_writes());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-display-dvr-multi-channel-pass"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.display_dvr_multi_channel_pipeline);
            pass.set_bind_group(0, &dvr_resources.bind_group, &[]);
            pass.dispatch_workgroups(
                viewport_width.div_ceil(WORKGROUP_SIZE_X),
                viewport_height.div_ceil(WORKGROUP_SIZE_Y),
                1,
            );
        }
        timings.gpu_compute_ns = self.submit_with_optional_timestamp(encoder, timestamp)?;

        Ok(GpuDisplayFrame {
            viewport,
            diagnostics: GpuDisplayFrameDiagnostics {
                channels: channels.len(),
                output_bytes: 0,
                accumulator_bytes: display_resources.accumulator_bytes,
                texture_bytes: display_resources.texture_bytes,
                draw_calls: 0,
                vertex_count: 0,
            },
            timings,
            adapter: self.adapter.clone(),
            texture: display_resources.texture,
            view: display_resources.view,
        })
    }

    pub fn render_resident_channels_to_display_texture(
        &self,
        channels: &[GpuResidentDisplayChannel<'_>],
        request: GpuResidentDisplayRequest,
    ) -> Result<GpuDisplayFrame, GpuRenderError> {
        if channels.is_empty() {
            return Err(RenderError::InvalidChannelComposite(
                "GPU display rendering requires at least one channel",
            )
            .into());
        }
        let GpuResidentDisplayRequest {
            camera,
            viewport,
            quality,
            iso_light_state,
            camera_axes,
        } = request;
        if channels.len() > 1 && channels.iter().all(GpuResidentDisplayChannel::is_iso) {
            return self.render_iso_channels_to_display_texture(
                channels,
                camera,
                viewport,
                quality,
                iso_light_state,
                camera_axes,
            );
        }
        if channels.len() > 1 && channels.iter().all(GpuResidentDisplayChannel::is_dvr) {
            return self
                .render_dvr_channels_to_display_texture(channels, camera, viewport, quality);
        }
        let viewport_width = checked_u32("viewport_width", viewport.width)?;
        let viewport_height = checked_u32("viewport_height", viewport.height)?;
        let pixel_count = (u64::from(viewport_width) * u64::from(viewport_height)) as usize;
        let accumulator_bytes =
            checked_buffer_byte_count("GPU display accumulator", pixel_count, 16)?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "GPU display accumulator",
            accumulator_bytes,
        )?;
        let texture_bytes = checked_buffer_byte_count("GPU display texture", pixel_count, 4)?;
        validate_general_buffer_bytes(&self.device.limits(), "GPU display texture", texture_bytes)?;
        let display_resources = self.display_resources_for_viewport(
            viewport,
            viewport_width,
            viewport_height,
            accumulator_bytes,
            texture_bytes,
        )?;

        let light_world = crate::current_camera::iso_light_direction(iso_light_state, camera_axes);
        let (camera_forward, camera_right, camera_up) =
            crate::current_camera::axes_vectors(camera_axes);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-display-command-encoder"),
            });
        encoder.clear_buffer(&display_resources.accumulator, 0, None);

        let mut timings = GpuRenderTimings::default();
        let mut output_bytes_total = 0_u64;
        let mut integer_output_slot = 0_usize;
        let mut f32_output_slot = 0_usize;
        let mut composite_slot = 0_usize;
        let display_timestamp =
            self.timestamp_query_pair("mirante4d-display-composite-finalize-timestamp");
        let mut display_timestamp_begin_written = false;
        for channel in channels {
            let channel_started = Instant::now();
            let channel_composite_slot = composite_slot;
            composite_slot =
                composite_slot
                    .checked_add(1)
                    .ok_or(GpuRenderError::BufferSizeOverflow {
                        resource: "display composite slots",
                    })?;
            let (
                output_buffer,
                output_bytes,
                output_timings,
                mode_code,
                transfer,
                composite_pipeline,
            ) = match channel {
                GpuResidentDisplayChannel::U8 {
                    resident,
                    brick_shape,
                    brick_grid_shape,
                    mode,
                    transfer,
                } => {
                    let output_slot = integer_output_slot;
                    integer_output_slot = integer_output_slot.checked_add(1).ok_or(
                        GpuRenderError::BufferSizeOverflow {
                            resource: "integer display output slots",
                        },
                    )?;
                    let output = self.render_camera_u8_from_bricks_output_buffer(
                        resident,
                        *brick_shape,
                        *brick_grid_shape,
                        camera,
                        viewport,
                        *mode,
                        quality,
                        output_slot,
                    )?;
                    (
                        output.output_buffer,
                        output.output_bytes,
                        output.timings,
                        display_mode_code(*mode),
                        transfer,
                        &self.display_composite_pipeline,
                    )
                }
                GpuResidentDisplayChannel::U16 {
                    resident,
                    brick_shape,
                    brick_grid_shape,
                    mode,
                    transfer,
                } => {
                    let output_slot = integer_output_slot;
                    integer_output_slot = integer_output_slot.checked_add(1).ok_or(
                        GpuRenderError::BufferSizeOverflow {
                            resource: "integer display output slots",
                        },
                    )?;
                    let output = self.render_camera_from_bricks_output_buffer(
                        resident,
                        *brick_shape,
                        *brick_grid_shape,
                        camera,
                        viewport,
                        *mode,
                        quality,
                        output_slot,
                    )?;
                    (
                        output.output_buffer,
                        output.output_bytes,
                        output.timings,
                        display_mode_code(*mode),
                        transfer,
                        &self.display_composite_pipeline,
                    )
                }
                GpuResidentDisplayChannel::F32 {
                    resident,
                    brick_shape,
                    brick_grid_shape,
                    mode,
                    transfer,
                } => {
                    let output_slot = f32_output_slot;
                    f32_output_slot = f32_output_slot.checked_add(1).ok_or(
                        GpuRenderError::BufferSizeOverflow {
                            resource: "float32 display output slots",
                        },
                    )?;
                    let output = self.render_camera_f32_from_bricks_output_buffer(
                        resident,
                        *brick_shape,
                        *brick_grid_shape,
                        camera,
                        viewport,
                        *mode,
                        quality,
                        output_slot,
                    )?;
                    (
                        output.output_buffer,
                        output.output_bytes,
                        output.timings,
                        display_mode_code_f32(*mode),
                        transfer,
                        &self.display_composite_f32_pipeline,
                    )
                }
            };
            timings = add_gpu_render_timings(timings, output_timings);
            timings.upload_ns = timings
                .upload_ns
                .saturating_add(duration_ns_u64(channel_started.elapsed()));
            output_bytes_total = output_bytes_total.saturating_add(output_bytes);
            let params_u32 = [
                viewport_width,
                viewport_height,
                mode_code,
                u32::from(transfer.visible()),
                u32::from(transfer.invert()),
                projection_code(crate::current_camera::projection(camera)),
            ];
            let params_f32 = [
                transfer.window().low(),
                transfer.window().high(),
                transfer.curve().gamma_value(),
                transfer.opacity().get(),
                transfer.color_rgba()[0],
                transfer.color_rgba()[1],
                transfer.color_rgba()[2],
                transfer.color_rgba()[3],
                light_world.x as f32,
                light_world.y as f32,
                light_world.z as f32,
                camera_forward.x as f32,
                camera_forward.y as f32,
                camera_forward.z as f32,
                camera_right.x as f32,
                camera_right.y as f32,
                camera_right.z as f32,
                camera_up.x as f32,
                camera_up.y as f32,
                camera_up.z as f32,
                crate::current_camera::perspective_focal_length_screen_points(camera) as f32,
                crate::current_camera::presentation_width_points(camera) as f32,
                crate::current_camera::presentation_height_points(camera) as f32,
            ];
            let params_u32_bytes = checked_buffer_byte_count(
                "GPU display channel u32 parameters",
                params_u32.len(),
                std::mem::size_of::<u32>(),
            )?;
            let params_f32_bytes = checked_buffer_byte_count(
                "GPU display channel f32 parameters",
                params_f32.len(),
                std::mem::size_of::<f32>(),
            )?;
            validate_storage_buffer_bytes(
                &self.device.limits(),
                "GPU display channel u32 parameters",
                params_u32_bytes,
            )?;
            validate_storage_buffer_bytes(
                &self.device.limits(),
                "GPU display channel f32 parameters",
                params_f32_bytes,
            )?;
            let composite_resources = self.display_channel_composite_resources(
                viewport,
                channel_composite_slot,
                params_u32_bytes,
                params_f32_bytes,
            )?;
            self.queue.write_buffer(
                &composite_resources.params_u32_buffer,
                0,
                bytemuck::cast_slice(&params_u32),
            );
            self.queue.write_buffer(
                &composite_resources.params_f32_buffer,
                0,
                bytemuck::cast_slice(&params_f32),
            );
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mirante4d-display-channel-bind-group"),
                layout: &self.display_composite_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: output_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: display_resources.accumulator.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: composite_resources.params_u32_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: composite_resources.params_f32_buffer.as_entire_binding(),
                    },
                ],
            });
            {
                let timestamp_writes = if display_timestamp_begin_written {
                    None
                } else {
                    display_timestamp_begin_written = true;
                    display_timestamp
                        .as_ref()
                        .map(|timestamp| timestamp.compute_pass_begin_write())
                };
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("mirante4d-display-composite-channel-pass"),
                    timestamp_writes,
                });
                pass.set_pipeline(composite_pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.dispatch_workgroups(
                    viewport_width.div_ceil(WORKGROUP_SIZE_X),
                    viewport_height.div_ceil(WORKGROUP_SIZE_Y),
                    1,
                );
            }
        }
        {
            let timestamp_writes = display_timestamp
                .as_ref()
                .map(|timestamp| timestamp.compute_pass_end_write());
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-display-finalize-pass"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.display_finalize_pipeline);
            pass.set_bind_group(0, &display_resources.final_bind_group, &[]);
            pass.dispatch_workgroups(
                viewport_width.div_ceil(WORKGROUP_SIZE_X),
                viewport_height.div_ceil(WORKGROUP_SIZE_Y),
                1,
            );
        }
        let display_timing = GpuRenderTimings {
            gpu_compute_ns: self.submit_with_optional_timestamp(encoder, display_timestamp)?,
            ..Default::default()
        };
        timings = add_gpu_render_timings(timings, display_timing);

        Ok(GpuDisplayFrame {
            viewport,
            diagnostics: GpuDisplayFrameDiagnostics {
                channels: channels.len(),
                output_bytes: output_bytes_total,
                accumulator_bytes: display_resources.accumulator_bytes,
                texture_bytes: display_resources.texture_bytes,
                draw_calls: 0,
                vertex_count: 0,
            },
            timings,
            adapter: self.adapter.clone(),
            texture: display_resources.texture,
            view: display_resources.view,
        })
    }
}

pub(super) struct GpuIntegerCameraOutputBuffer {
    pub(super) output_buffer: wgpu::Buffer,
    pub(super) output_bytes: u64,
    pub(super) timings: GpuRenderTimings,
}

pub(super) struct GpuF32CameraOutputBuffer {
    pub(super) output_buffer: wgpu::Buffer,
    pub(super) output_bytes: u64,
    pub(super) timings: GpuRenderTimings,
}

pub(super) fn integer_camera_output_bytes(
    viewport: RenderViewport,
) -> Result<(usize, u64), GpuRenderError> {
    let output_len = (viewport.width as usize)
        .checked_mul(viewport.height as usize)
        .ok_or(GpuRenderError::BufferSizeOverflow {
            resource: "GPU display output",
        })?;
    let output_bytes = checked_buffer_byte_count(
        "GPU display output",
        output_len,
        GPU_SURFACE_OUTPUT_U16_FIELDS * std::mem::size_of::<u32>(),
    )?;
    Ok((output_len, output_bytes))
}

pub(super) fn f32_camera_output_bytes(
    viewport: RenderViewport,
) -> Result<(usize, u64), GpuRenderError> {
    let output_len = (viewport.width as usize)
        .checked_mul(viewport.height as usize)
        .ok_or(GpuRenderError::BufferSizeOverflow {
            resource: "GPU display f32 output",
        })?;
    let output_bytes = checked_buffer_byte_count(
        "GPU display f32 output",
        output_len,
        super::decode::GPU_SURFACE_OUTPUT_F32_FIELDS * std::mem::size_of::<u32>(),
    )?;
    Ok((output_len, output_bytes))
}
