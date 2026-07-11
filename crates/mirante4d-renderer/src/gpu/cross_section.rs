use std::{collections::HashMap, time::Instant};

use bytemuck::cast_slice;
use mirante4d_data::SpatialBrickIndex;
use mirante4d_domain::Shape3D;
use mirante4d_format::CurrentGridToWorldExt;
use mirante4d_render_api::PresentationViewport;
use wgpu::util::DeviceExt;

use super::{
    GpuRenderError, GpuRenderTimings, GpuRenderer, WORKGROUP_SIZE_X, WORKGROUP_SIZE_Y,
    add_gpu_render_timings,
    atlas::{GpuBrickAtlasF32Resource, GpuBrickAtlasPagePriority, GpuBrickAtlasResource},
    buffers::{
        checked_buffer_byte_count, checked_u32, validate_general_buffer_bytes,
        validate_storage_buffer_bytes,
    },
    display::{
        GpuDisplayFrame, GpuDisplayFrameDiagnostics, f32_camera_output_bytes,
        integer_camera_output_bytes,
    },
    display_resources::{GpuF32CameraDisplayBufferSpec, GpuIntegerCameraDisplayBufferSpec},
    duration_ns_u64,
};
use crate::{
    CrossSectionPanelBounds, CrossSectionView, IntensityTransfer, RenderError, RenderViewport,
    ResidentBrickSetF32, ResidentBrickSetU8, ResidentBrickSetU16,
};

const CROSS_SECTION_PARAMS_U32_WORDS: usize = 17;
const CROSS_SECTION_PARAMS_F32_WORDS: usize = 11;
const CROSS_SECTION_F32_PARAMS_U32_WORDS: usize = 11;
const CROSS_SECTION_F32_PARAMS_F32_WORDS: usize = 11;
const CROSS_SECTION_CHUNK_DRAW_WORDS: usize = 8;

struct CrossSectionIntegerParams {
    params_u32: [u32; CROSS_SECTION_PARAMS_U32_WORDS],
    params_f32: [f32; CROSS_SECTION_PARAMS_F32_WORDS],
}

struct CrossSectionF32Params {
    params_u32: [u32; CROSS_SECTION_F32_PARAMS_U32_WORDS],
    params_f32: [f32; CROSS_SECTION_F32_PARAMS_F32_WORDS],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpuCrossSectionChunkDraw {
    pub brick_index: SpatialBrickIndex,
    pub panel_bounds: CrossSectionPanelBounds,
    pub vertex_count: u32,
    pub cache_priority: GpuBrickAtlasPagePriority,
}

pub enum GpuCrossSectionChunkDisplayChannel<'a> {
    U8 {
        resident: &'a ResidentBrickSetU8,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        transfer: IntensityTransfer,
        chunks: &'a [GpuCrossSectionChunkDraw],
    },
    U16 {
        resident: &'a ResidentBrickSetU16,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        transfer: IntensityTransfer,
        chunks: &'a [GpuCrossSectionChunkDraw],
    },
    F32 {
        resident: &'a ResidentBrickSetF32,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        transfer: IntensityTransfer,
        chunks: &'a [GpuCrossSectionChunkDraw],
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CrossSectionChunkPixelBounds {
    min_x: u32,
    min_y: u32,
    max_x: u32,
    max_y: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CrossSectionChunkDispatchPlan {
    words: Vec<u32>,
    max_width: u32,
    max_height: u32,
    draw_calls: u64,
    vertex_count: u64,
}

struct GpuChunkedIntegerCameraOutputBuffer {
    output_buffer: wgpu::Buffer,
    output_bytes: u64,
    timings: GpuRenderTimings,
    draw_calls: u64,
    vertex_count: u64,
}

struct GpuChunkedF32CameraOutputBuffer {
    output_buffer: wgpu::Buffer,
    output_bytes: u64,
    timings: GpuRenderTimings,
    draw_calls: u64,
    vertex_count: u64,
}

fn cross_section_chunk_page_priorities(
    chunks: &[GpuCrossSectionChunkDraw],
) -> HashMap<SpatialBrickIndex, GpuBrickAtlasPagePriority> {
    let mut priorities = HashMap::with_capacity(chunks.len());
    for chunk in chunks {
        priorities
            .entry(chunk.brick_index)
            .and_modify(|existing| {
                if page_priority_is_better(chunk.cache_priority, *existing) {
                    *existing = chunk.cache_priority;
                }
            })
            .or_insert(chunk.cache_priority);
    }
    priorities
}

fn page_priority_is_better(
    left: GpuBrickAtlasPagePriority,
    right: GpuBrickAtlasPagePriority,
) -> bool {
    left.tier_rank < right.tier_rank
        || (left.tier_rank == right.tier_rank && left.score > right.score)
}

fn cross_section_integer_params(
    volume_shape: mirante4d_domain::Shape3D,
    grid_to_world: mirante4d_domain::GridToWorld,
    atlas: &GpuBrickAtlasResource,
    view: CrossSectionView,
    presentation_viewport: PresentationViewport,
    render_viewport: RenderViewport,
) -> Result<CrossSectionIntegerParams, GpuRenderError> {
    let viewport_width = checked_u32("viewport_width", render_viewport.width)?;
    let viewport_height = checked_u32("viewport_height", render_viewport.height)?;
    let shape_x = checked_u32("x", volume_shape.x())?;
    let shape_y = checked_u32("y", volume_shape.y())?;
    let shape_z = checked_u32("z", volume_shape.z())?;
    let brick_x = checked_u32("brick_x", atlas.brick_shape.x())?;
    let brick_y = checked_u32("brick_y", atlas.brick_shape.y())?;
    let brick_z = checked_u32("brick_z", atlas.brick_shape.z())?;
    let grid_x = checked_u32("grid_x", atlas.brick_grid_shape.x())?;
    let grid_y = checked_u32("grid_y", atlas.brick_grid_shape.y())?;
    let grid_z = checked_u32("grid_z", atlas.brick_grid_shape.z())?;
    let brick_voxel_count = checked_u32("brick_voxel_count", atlas.brick_voxel_count)?;
    let packed_u32_per_brick = checked_u32("packed_u32_per_brick", atlas.packed_u32_per_brick)?;
    let valid_u32_per_brick = checked_u32("valid_u32_per_brick", atlas.valid_u32_per_brick)?;
    let world_to_grid = grid_to_world.inverse().map_err(RenderError::from)?;
    let center_grid = world_to_grid.transform_point(view.center_world);
    let right_grid_per_point =
        world_to_grid.transform_vector(view.basis.right_world * view.scale_world_per_screen_point);
    let down_grid_per_point =
        world_to_grid.transform_vector(view.basis.down_world * view.scale_world_per_screen_point);
    let params_u32 = [
        viewport_width,
        viewport_height,
        shape_x,
        shape_y,
        shape_z,
        brick_x,
        brick_y,
        brick_z,
        grid_x,
        grid_y,
        grid_z,
        brick_voxel_count,
        packed_u32_per_brick,
        atlas.values_per_word,
        atlas.bits_per_value,
        atlas.value_mask,
        valid_u32_per_brick,
    ];
    let params_f32 = [
        center_grid.x as f32,
        center_grid.y as f32,
        center_grid.z as f32,
        right_grid_per_point.x as f32,
        right_grid_per_point.y as f32,
        right_grid_per_point.z as f32,
        down_grid_per_point.x as f32,
        down_grid_per_point.y as f32,
        down_grid_per_point.z as f32,
        presentation_viewport.width_points() as f32,
        presentation_viewport.height_points() as f32,
    ];
    Ok(CrossSectionIntegerParams {
        params_u32,
        params_f32,
    })
}

fn cross_section_f32_params(
    volume_shape: Shape3D,
    grid_to_world: mirante4d_domain::GridToWorld,
    atlas: &GpuBrickAtlasF32Resource,
    view: CrossSectionView,
    presentation_viewport: PresentationViewport,
    render_viewport: RenderViewport,
) -> Result<CrossSectionF32Params, GpuRenderError> {
    let viewport_width = checked_u32("viewport_width", render_viewport.width)?;
    let viewport_height = checked_u32("viewport_height", render_viewport.height)?;
    let shape_x = checked_u32("x", volume_shape.x())?;
    let shape_y = checked_u32("y", volume_shape.y())?;
    let shape_z = checked_u32("z", volume_shape.z())?;
    let brick_x = checked_u32("brick_x", atlas.brick_shape.x())?;
    let brick_y = checked_u32("brick_y", atlas.brick_shape.y())?;
    let brick_z = checked_u32("brick_z", atlas.brick_shape.z())?;
    let grid_x = checked_u32("grid_x", atlas.brick_grid_shape.x())?;
    let grid_y = checked_u32("grid_y", atlas.brick_grid_shape.y())?;
    let grid_z = checked_u32("grid_z", atlas.brick_grid_shape.z())?;
    let world_to_grid = grid_to_world.inverse().map_err(RenderError::from)?;
    let center_grid = world_to_grid.transform_point(view.center_world);
    let right_grid_per_point =
        world_to_grid.transform_vector(view.basis.right_world * view.scale_world_per_screen_point);
    let down_grid_per_point =
        world_to_grid.transform_vector(view.basis.down_world * view.scale_world_per_screen_point);
    let params_u32 = [
        viewport_width,
        viewport_height,
        shape_x,
        shape_y,
        shape_z,
        brick_x,
        brick_y,
        brick_z,
        grid_x,
        grid_y,
        grid_z,
    ];
    let params_f32 = [
        center_grid.x as f32,
        center_grid.y as f32,
        center_grid.z as f32,
        right_grid_per_point.x as f32,
        right_grid_per_point.y as f32,
        right_grid_per_point.z as f32,
        down_grid_per_point.x as f32,
        down_grid_per_point.y as f32,
        down_grid_per_point.z as f32,
        presentation_viewport.width_points() as f32,
        presentation_viewport.height_points() as f32,
    ];
    Ok(CrossSectionF32Params {
        params_u32,
        params_f32,
    })
}

fn cross_section_chunk_dispatch_plan(
    chunks: &[GpuCrossSectionChunkDraw],
    presentation_viewport: PresentationViewport,
    render_viewport: RenderViewport,
) -> Result<CrossSectionChunkDispatchPlan, GpuRenderError> {
    let mut words = Vec::with_capacity(chunks.len().saturating_mul(CROSS_SECTION_CHUNK_DRAW_WORDS));
    let mut max_width = 0_u32;
    let mut max_height = 0_u32;
    let mut draw_calls = 0_u64;
    let mut vertex_count = 0_u64;
    for chunk in chunks {
        let Some(bounds) = cross_section_chunk_pixel_bounds(
            chunk.panel_bounds,
            presentation_viewport,
            render_viewport,
        ) else {
            continue;
        };
        let z = checked_u32("chunk_z", chunk.brick_index.z)?;
        let y = checked_u32("chunk_y", chunk.brick_index.y)?;
        let x = checked_u32("chunk_x", chunk.brick_index.x)?;
        words.extend_from_slice(&[
            z,
            y,
            x,
            bounds.min_x,
            bounds.min_y,
            bounds.max_x,
            bounds.max_y,
            chunk.vertex_count,
        ]);
        max_width = max_width.max(bounds.max_x.saturating_sub(bounds.min_x));
        max_height = max_height.max(bounds.max_y.saturating_sub(bounds.min_y));
        draw_calls = draw_calls.saturating_add(1);
        vertex_count = vertex_count.saturating_add(u64::from(chunk.vertex_count));
    }
    Ok(CrossSectionChunkDispatchPlan {
        words,
        max_width,
        max_height,
        draw_calls,
        vertex_count,
    })
}

fn cross_section_chunk_pixel_bounds(
    panel_bounds: CrossSectionPanelBounds,
    presentation_viewport: PresentationViewport,
    render_viewport: RenderViewport,
) -> Option<CrossSectionChunkPixelBounds> {
    if !panel_bounds.min_points.is_finite()
        || !panel_bounds.max_points.is_finite()
        || panel_bounds.max_points.x < panel_bounds.min_points.x
        || panel_bounds.max_points.y < panel_bounds.min_points.y
    {
        return None;
    }
    let width_points = presentation_viewport.width_points();
    let height_points = presentation_viewport.height_points();
    if width_points <= 0.0 || height_points <= 0.0 {
        return None;
    }
    let width = render_viewport.width as f64;
    let height = render_viewport.height as f64;
    let min_x = ((panel_bounds.min_points.x / width_points) * width).floor() as i64 - 1;
    let min_y = ((panel_bounds.min_points.y / height_points) * height).floor() as i64 - 1;
    let max_x = ((panel_bounds.max_points.x / width_points) * width).ceil() as i64 + 1;
    let max_y = ((panel_bounds.max_points.y / height_points) * height).ceil() as i64 + 1;
    let min_x = min_x.clamp(0, render_viewport.width as i64) as u32;
    let min_y = min_y.clamp(0, render_viewport.height as i64) as u32;
    let max_x = max_x.clamp(0, render_viewport.width as i64) as u32;
    let max_y = max_y.clamp(0, render_viewport.height as i64) as u32;
    if min_x >= max_x || min_y >= max_y {
        return None;
    }
    Some(CrossSectionChunkPixelBounds {
        min_x,
        min_y,
        max_x,
        max_y,
    })
}

impl GpuRenderer {
    pub fn render_cross_section_chunked_channels_to_display_texture(
        &self,
        channels: &[GpuCrossSectionChunkDisplayChannel<'_>],
        view: CrossSectionView,
        presentation_viewport: PresentationViewport,
        render_viewport: RenderViewport,
    ) -> Result<GpuDisplayFrame, GpuRenderError> {
        if channels.is_empty() {
            return Err(RenderError::InvalidChannelComposite(
                "chunked cross-section display rendering requires at least one channel",
            )
            .into());
        }
        let viewport_width = checked_u32("viewport_width", render_viewport.width)?;
        let viewport_height = checked_u32("viewport_height", render_viewport.height)?;
        let pixel_count = (u64::from(viewport_width) * u64::from(viewport_height)) as usize;
        let accumulator_bytes = checked_buffer_byte_count(
            "chunked cross-section display accumulator",
            pixel_count,
            16,
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "chunked cross-section display accumulator",
            accumulator_bytes,
        )?;
        let texture_bytes =
            checked_buffer_byte_count("chunked cross-section display texture", pixel_count, 4)?;
        validate_general_buffer_bytes(
            &self.device.limits(),
            "chunked cross-section display texture",
            texture_bytes,
        )?;
        let display_resources = self.display_resources_for_viewport(
            render_viewport,
            viewport_width,
            viewport_height,
            accumulator_bytes,
            texture_bytes,
        )?;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-chunked-cross-section-display-command-encoder"),
            });
        encoder.clear_buffer(&display_resources.accumulator, 0, None);

        let mut timings = GpuRenderTimings::default();
        let mut output_bytes_total = 0_u64;
        let mut output_slot = 0_usize;
        let mut composite_slot = 0_usize;
        let mut draw_calls = 0_u64;
        let mut vertex_count = 0_u64;
        let display_timestamp =
            self.timestamp_query_pair("mirante4d-chunked-cross-section-display-timestamp");
        let mut display_timestamp_begin_written = false;

        for channel in channels {
            let channel_composite_slot = composite_slot;
            composite_slot =
                composite_slot
                    .checked_add(1)
                    .ok_or(GpuRenderError::BufferSizeOverflow {
                        resource: "chunked cross-section display composite slots",
                    })?;

            let channel_output_slot = output_slot;
            output_slot = output_slot
                .checked_add(1)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                    resource: "chunked cross-section display output slots",
                })?;

            let (
                output_buffer,
                output_bytes,
                output_timings,
                transfer,
                composite_pipeline,
                channel_draws,
                channel_vertices,
            ) = match channel {
                GpuCrossSectionChunkDisplayChannel::U8 {
                    resident,
                    brick_shape,
                    brick_grid_shape,
                    transfer,
                    chunks,
                } => {
                    if resident.bricks().is_empty() {
                        return Err(RenderError::InvalidBrickAtlas(
                                "resident uint8 brick set is empty for chunked cross-section display render",
                            )
                        .into());
                    }
                    let upload_started = Instant::now();
                    let page_priorities = cross_section_chunk_page_priorities(chunks);
                    let atlas = self.cached_brick_atlas_u8_with_page_priorities(
                        resident,
                        *brick_shape,
                        *brick_grid_shape,
                        &page_priorities,
                    )?;
                    let upload_timings = GpuRenderTimings {
                        upload_ns: duration_ns_u64(upload_started.elapsed()),
                        ..Default::default()
                    };
                    let output = self.render_cross_section_integer_chunk_atlas_output_buffer(
                        resident.volume_shape,
                        resident.grid_to_world,
                        atlas,
                        upload_timings,
                        view,
                        presentation_viewport,
                        render_viewport,
                        channel_output_slot,
                        chunks,
                    )?;
                    (
                        output.output_buffer,
                        output.output_bytes,
                        output.timings,
                        transfer,
                        &self.display_composite_pipeline,
                        output.draw_calls,
                        output.vertex_count,
                    )
                }
                GpuCrossSectionChunkDisplayChannel::U16 {
                    resident,
                    brick_shape,
                    brick_grid_shape,
                    transfer,
                    chunks,
                } => {
                    if resident.bricks().is_empty() {
                        return Err(RenderError::InvalidBrickAtlas(
                                "resident uint16 brick set is empty for chunked cross-section display render",
                            )
                        .into());
                    }
                    let upload_started = Instant::now();
                    let page_priorities = cross_section_chunk_page_priorities(chunks);
                    let atlas = self.cached_brick_atlas_with_page_priorities(
                        resident,
                        *brick_shape,
                        *brick_grid_shape,
                        &page_priorities,
                    )?;
                    let upload_timings = GpuRenderTimings {
                        upload_ns: duration_ns_u64(upload_started.elapsed()),
                        ..Default::default()
                    };
                    let output = self.render_cross_section_integer_chunk_atlas_output_buffer(
                        resident.volume_shape,
                        resident.grid_to_world,
                        atlas,
                        upload_timings,
                        view,
                        presentation_viewport,
                        render_viewport,
                        channel_output_slot,
                        chunks,
                    )?;
                    (
                        output.output_buffer,
                        output.output_bytes,
                        output.timings,
                        transfer,
                        &self.display_composite_pipeline,
                        output.draw_calls,
                        output.vertex_count,
                    )
                }
                GpuCrossSectionChunkDisplayChannel::F32 {
                    resident,
                    brick_shape,
                    brick_grid_shape,
                    transfer,
                    chunks,
                } => {
                    if resident.bricks().is_empty() {
                        return Err(RenderError::InvalidBrickAtlas(
                            "resident float32 brick set is empty for chunked cross-section display render",
                        )
                        .into());
                    }
                    let upload_started = Instant::now();
                    let page_priorities = cross_section_chunk_page_priorities(chunks);
                    let atlas = self.cached_brick_atlas_f32_with_page_priorities(
                        resident,
                        *brick_shape,
                        *brick_grid_shape,
                        &page_priorities,
                    )?;
                    let upload_timings = GpuRenderTimings {
                        upload_ns: duration_ns_u64(upload_started.elapsed()),
                        ..Default::default()
                    };
                    let output = self.render_cross_section_f32_chunk_atlas_output_buffer(
                        resident.volume_shape,
                        resident.grid_to_world,
                        atlas,
                        upload_timings,
                        view,
                        presentation_viewport,
                        render_viewport,
                        channel_output_slot,
                        chunks,
                    )?;
                    (
                        output.output_buffer,
                        output.output_bytes,
                        output.timings,
                        transfer,
                        &self.display_composite_f32_pipeline,
                        output.draw_calls,
                        output.vertex_count,
                    )
                }
            };
            timings = add_gpu_render_timings(timings, output_timings);
            output_bytes_total = output_bytes_total.saturating_add(output_bytes);
            draw_calls = draw_calls.saturating_add(channel_draws);
            vertex_count = vertex_count.saturating_add(channel_vertices);

            let params_u32 = [
                viewport_width,
                viewport_height,
                0,
                u32::from(transfer.visible()),
                u32::from(transfer.invert()),
                0,
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
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                presentation_viewport.width_points() as f32,
                presentation_viewport.height_points() as f32,
            ];
            let params_u32_bytes = checked_buffer_byte_count(
                "chunked cross-section display composite u32 parameters",
                params_u32.len(),
                std::mem::size_of::<u32>(),
            )?;
            let params_f32_bytes = checked_buffer_byte_count(
                "chunked cross-section display composite f32 parameters",
                params_f32.len(),
                std::mem::size_of::<f32>(),
            )?;
            validate_storage_buffer_bytes(
                &self.device.limits(),
                "chunked cross-section display composite u32 parameters",
                params_u32_bytes,
            )?;
            validate_storage_buffer_bytes(
                &self.device.limits(),
                "chunked cross-section display composite f32 parameters",
                params_f32_bytes,
            )?;
            let composite_resources = self.display_channel_composite_resources(
                render_viewport,
                channel_composite_slot,
                params_u32_bytes,
                params_f32_bytes,
            )?;
            self.queue.write_buffer(
                &composite_resources.params_u32_buffer,
                0,
                cast_slice(&params_u32),
            );
            self.queue.write_buffer(
                &composite_resources.params_f32_buffer,
                0,
                cast_slice(&params_f32),
            );
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mirante4d-chunked-cross-section-display-composite-bind-group"),
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
            let timestamp_writes = if display_timestamp_begin_written {
                None
            } else {
                display_timestamp_begin_written = true;
                display_timestamp
                    .as_ref()
                    .map(|timestamp| timestamp.compute_pass_begin_write())
            };
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-chunked-cross-section-display-composite-pass"),
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
        {
            let timestamp_writes = display_timestamp
                .as_ref()
                .map(|timestamp| timestamp.compute_pass_end_write());
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-chunked-cross-section-display-finalize-pass"),
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
        let diagnostics_draw_calls =
            usize::try_from(draw_calls).map_err(|_| GpuRenderError::BufferSizeOverflow {
                resource: "chunked cross-section display draw calls",
            })?;

        Ok(GpuDisplayFrame {
            viewport: render_viewport,
            diagnostics: GpuDisplayFrameDiagnostics {
                channels: channels.len(),
                output_bytes: output_bytes_total,
                accumulator_bytes: display_resources.accumulator_bytes,
                texture_bytes: display_resources.texture_bytes,
                draw_calls: diagnostics_draw_calls,
                vertex_count,
            },
            timings,
            adapter: self.adapter.clone(),
            texture: display_resources.texture,
            view: display_resources.view,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn render_cross_section_integer_chunk_atlas_output_buffer(
        &self,
        volume_shape: mirante4d_domain::Shape3D,
        grid_to_world: mirante4d_domain::GridToWorld,
        atlas: GpuBrickAtlasResource,
        mut timings: GpuRenderTimings,
        view: CrossSectionView,
        presentation_viewport: PresentationViewport,
        render_viewport: RenderViewport,
        output_slot: usize,
        chunks: &[GpuCrossSectionChunkDraw],
    ) -> Result<GpuChunkedIntegerCameraOutputBuffer, GpuRenderError> {
        let (_, output_bytes) = integer_camera_output_bytes(render_viewport)?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "chunked cross-section integer display output",
            output_bytes,
        )?;
        let dispatch_plan =
            cross_section_chunk_dispatch_plan(chunks, presentation_viewport, render_viewport)?;
        if dispatch_plan.draw_calls == 0 {
            return Err(RenderError::InvalidBrickAtlas(
                "chunked cross-section display has no visible chunk draw bounds",
            )
            .into());
        }
        let max_workgroups = self.device.limits().max_compute_workgroups_per_dimension;
        let draw_workgroups = checked_u32("chunk_draw_count", dispatch_plan.draw_calls)?;
        if draw_workgroups > max_workgroups {
            return Err(GpuRenderError::BufferTooLarge {
                resource: "chunked cross-section chunk dispatch count",
                required_bytes: u64::from(draw_workgroups),
                limit_bytes: u64::from(max_workgroups),
            });
        }
        let CrossSectionIntegerParams {
            params_u32,
            params_f32,
        } = cross_section_integer_params(
            volume_shape,
            grid_to_world,
            &atlas,
            view,
            presentation_viewport,
            render_viewport,
        )?;
        let params_u32_bytes = checked_buffer_byte_count(
            "chunked cross-section display u32 parameters",
            params_u32.len(),
            std::mem::size_of::<u32>(),
        )?;
        let params_f32_bytes = checked_buffer_byte_count(
            "chunked cross-section display f32 parameters",
            params_f32.len(),
            std::mem::size_of::<f32>(),
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "chunked cross-section display u32 parameters",
            params_u32_bytes,
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "chunked cross-section display f32 parameters",
            params_f32_bytes,
        )?;
        let chunk_draw_bytes = checked_buffer_byte_count(
            "chunked cross-section chunk draw buffer",
            dispatch_plan.words.len(),
            std::mem::size_of::<u32>(),
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "chunked cross-section chunk draw buffer",
            chunk_draw_bytes,
        )?;
        let display_buffers = self.integer_camera_chunk_display_output_resources(
            render_viewport,
            &atlas,
            output_slot,
            GpuIntegerCameraDisplayBufferSpec {
                output_bytes,
                params_u32_bytes,
                params_f32_bytes,
                skip_diagnostics_bytes: chunk_draw_bytes,
            },
        )?;
        self.queue.write_buffer(
            &display_buffers.params_u32_buffer,
            0,
            cast_slice(&params_u32),
        );
        self.queue.write_buffer(
            &display_buffers.params_f32_buffer,
            0,
            cast_slice(&params_f32),
        );
        self.queue.write_buffer(
            &display_buffers.skip_diagnostics_buffer,
            0,
            cast_slice(&dispatch_plan.words),
        );
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-chunked-cross-section-display-output-command-encoder"),
            });
        encoder.clear_buffer(&display_buffers.output_buffer, 0, None);
        let timestamp =
            self.timestamp_query_pair("mirante4d-chunked-cross-section-display-output-timestamp");
        let timestamp_writes = timestamp
            .as_ref()
            .map(|timestamp| timestamp.compute_pass_writes());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-chunked-cross-section-display-output-compute-pass"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.cross_section_integer_chunk_display_pipeline);
            pass.set_bind_group(0, &display_buffers.bind_group, &[]);
            pass.dispatch_workgroups(
                dispatch_plan.max_width.div_ceil(WORKGROUP_SIZE_X),
                dispatch_plan.max_height.div_ceil(WORKGROUP_SIZE_Y),
                draw_workgroups,
            );
        }
        timings.gpu_compute_ns = self.submit_with_optional_timestamp(encoder, timestamp)?;
        Ok(GpuChunkedIntegerCameraOutputBuffer {
            output_buffer: display_buffers.output_buffer,
            output_bytes: display_buffers.output_bytes,
            timings,
            draw_calls: dispatch_plan.draw_calls,
            vertex_count: dispatch_plan.vertex_count,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn render_cross_section_f32_chunk_atlas_output_buffer(
        &self,
        volume_shape: Shape3D,
        grid_to_world: mirante4d_domain::GridToWorld,
        atlas: GpuBrickAtlasF32Resource,
        mut timings: GpuRenderTimings,
        view: CrossSectionView,
        presentation_viewport: PresentationViewport,
        render_viewport: RenderViewport,
        output_slot: usize,
        chunks: &[GpuCrossSectionChunkDraw],
    ) -> Result<GpuChunkedF32CameraOutputBuffer, GpuRenderError> {
        let (_, output_bytes) = f32_camera_output_bytes(render_viewport)?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "chunked cross-section f32 display output",
            output_bytes,
        )?;
        let dispatch_plan =
            cross_section_chunk_dispatch_plan(chunks, presentation_viewport, render_viewport)?;
        if dispatch_plan.draw_calls == 0 {
            return Err(RenderError::InvalidBrickAtlas(
                "chunked float32 cross-section display has no visible chunk draw bounds",
            )
            .into());
        }
        let max_workgroups = self.device.limits().max_compute_workgroups_per_dimension;
        let draw_workgroups = checked_u32("f32_chunk_draw_count", dispatch_plan.draw_calls)?;
        if draw_workgroups > max_workgroups {
            return Err(GpuRenderError::BufferTooLarge {
                resource: "chunked float32 cross-section chunk dispatch count",
                required_bytes: u64::from(draw_workgroups),
                limit_bytes: u64::from(max_workgroups),
            });
        }
        let CrossSectionF32Params {
            params_u32,
            params_f32,
        } = cross_section_f32_params(
            volume_shape,
            grid_to_world,
            &atlas,
            view,
            presentation_viewport,
            render_viewport,
        )?;
        let params_u32_bytes = checked_buffer_byte_count(
            "chunked cross-section f32 display u32 parameters",
            params_u32.len(),
            std::mem::size_of::<u32>(),
        )?;
        let params_f32_bytes = checked_buffer_byte_count(
            "chunked cross-section f32 display f32 parameters",
            params_f32.len(),
            std::mem::size_of::<f32>(),
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "chunked cross-section f32 display u32 parameters",
            params_u32_bytes,
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "chunked cross-section f32 display f32 parameters",
            params_f32_bytes,
        )?;
        let chunk_draw_bytes = checked_buffer_byte_count(
            "chunked cross-section f32 chunk draw buffer",
            dispatch_plan.words.len(),
            std::mem::size_of::<u32>(),
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "chunked cross-section f32 chunk draw buffer",
            chunk_draw_bytes,
        )?;
        let display_buffers = self.f32_camera_display_output_resources(
            render_viewport,
            &atlas,
            output_slot,
            GpuF32CameraDisplayBufferSpec {
                output_bytes,
                params_u32_bytes,
                params_f32_bytes,
            },
        )?;
        self.queue.write_buffer(
            &display_buffers.params_u32_buffer,
            0,
            cast_slice(&params_u32),
        );
        self.queue.write_buffer(
            &display_buffers.params_f32_buffer,
            0,
            cast_slice(&params_f32),
        );
        let chunk_draw_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mirante4d-chunked-cross-section-f32-draws"),
                contents: cast_slice(&dispatch_plan.words),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mirante4d-chunked-cross-section-f32-display-bind-group"),
            layout: &self.cross_section_f32_chunk_display_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: atlas.values_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: display_buffers.output_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: display_buffers.params_u32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: display_buffers.params_f32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: atlas.page_table_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: chunk_draw_buffer.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-chunked-cross-section-f32-display-output-command-encoder"),
            });
        encoder.clear_buffer(&display_buffers.output_buffer, 0, None);
        let timestamp = self
            .timestamp_query_pair("mirante4d-chunked-cross-section-f32-display-output-timestamp");
        let timestamp_writes = timestamp
            .as_ref()
            .map(|timestamp| timestamp.compute_pass_writes());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-chunked-cross-section-f32-display-output-compute-pass"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.cross_section_f32_chunk_display_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                dispatch_plan.max_width.div_ceil(WORKGROUP_SIZE_X),
                dispatch_plan.max_height.div_ceil(WORKGROUP_SIZE_Y),
                draw_workgroups,
            );
        }
        timings.gpu_compute_ns = self.submit_with_optional_timestamp(encoder, timestamp)?;
        Ok(GpuChunkedF32CameraOutputBuffer {
            output_buffer: display_buffers.output_buffer,
            output_bytes: display_buffers.output_bytes,
            timings,
            draw_calls: dispatch_plan.draw_calls,
            vertex_count: dispatch_plan.vertex_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use glam::{DQuat, DVec2, DVec3};
    use mirante4d_data::{
        DenseVolumeF32, DenseVolumeU8, SpatialBrickIndex, VolumeBrickF32, VolumeBrickU8,
        VolumeRegion,
    };
    use mirante4d_domain::{
        DisplayWindow, GridToWorld, LayerTransfer, Opacity, RgbColor, Shape3D, TimeIndex,
        TransferCurve,
    };
    use mirante4d_format::{BrickIndex, DatasetId, LayerId};
    use mirante4d_render_api::PresentationViewport;

    use super::*;
    use crate::{CrossSectionPanel, ResidentBrickSetF32, ResidentBrickSetU8};

    fn linear_color_transfer(color_rgba: [f32; 4], window_high: f32) -> IntensityTransfer {
        let [red, green, blue, _alpha] = color_rgba;
        IntensityTransfer::new(
            true,
            LayerTransfer::new(
                DisplayWindow::new(0.0, window_high).unwrap(),
                RgbColor::new([red, green, blue]).unwrap(),
                Opacity::new(1.0).unwrap(),
                TransferCurve::linear(),
                false,
            ),
        )
    }

    fn linear_white_transfer(window_high: f32) -> IntensityTransfer {
        linear_color_transfer([1.0, 1.0, 1.0, 1.0], window_high)
    }

    fn assert_rgba_abs_diff_le(actual: &[u8], expected: &[u8], max_diff: u8) {
        assert_eq!(actual.len(), expected.len());
        for (index, (&actual, &expected)) in actual.iter().zip(expected).enumerate() {
            let diff = actual.abs_diff(expected);
            assert!(
                diff <= max_diff,
                "RGBA byte {index} differs by {diff}: actual={actual}, expected={expected}, max_diff={max_diff}"
            );
        }
    }

    #[test]
    fn chunk_pixel_bounds_clamp_and_expand_panel_bounds() {
        let bounds = CrossSectionPanelBounds {
            min_points: DVec2::new(1.0, 2.0),
            max_points: DVec2::new(5.0, 7.0),
        };

        let pixel_bounds = cross_section_chunk_pixel_bounds(
            bounds,
            PresentationViewport::new(10.0, 10.0).unwrap(),
            RenderViewport::new(100, 50).unwrap(),
        )
        .unwrap();

        assert_eq!(
            pixel_bounds,
            CrossSectionChunkPixelBounds {
                min_x: 9,
                min_y: 9,
                max_x: 51,
                max_y: 36,
            }
        );
    }

    #[test]
    fn chunk_dispatch_plan_serializes_visible_draws() {
        let chunks = [
            GpuCrossSectionChunkDraw {
                brick_index: SpatialBrickIndex::new(2, 3, 4),
                panel_bounds: CrossSectionPanelBounds {
                    min_points: DVec2::new(1.0, 1.0),
                    max_points: DVec2::new(3.0, 3.0),
                },
                vertex_count: 4,
                cache_priority: GpuBrickAtlasPagePriority::default(),
            },
            GpuCrossSectionChunkDraw {
                brick_index: SpatialBrickIndex::new(5, 6, 7),
                panel_bounds: CrossSectionPanelBounds {
                    min_points: DVec2::new(8.0, 8.0),
                    max_points: DVec2::new(30.0, 30.0),
                },
                vertex_count: 6,
                cache_priority: GpuBrickAtlasPagePriority::default(),
            },
        ];

        let plan = cross_section_chunk_dispatch_plan(
            &chunks,
            PresentationViewport::new(10.0, 10.0).unwrap(),
            RenderViewport::new(20, 20).unwrap(),
        )
        .unwrap();

        assert_eq!(plan.draw_calls, 2);
        assert_eq!(plan.vertex_count, 10);
        assert_eq!(plan.words.len(), CROSS_SECTION_CHUNK_DRAW_WORDS * 2);
        assert_eq!(&plan.words[0..8], &[2, 3, 4, 1, 1, 7, 7, 4]);
        assert_eq!(&plan.words[8..16], &[5, 6, 7, 15, 15, 20, 20, 6]);
        assert_eq!(plan.max_width, 6);
        assert_eq!(plan.max_height, 6);
    }

    fn full_panel_chunk_draw(width_points: f64, height_points: f64) -> GpuCrossSectionChunkDraw {
        GpuCrossSectionChunkDraw {
            brick_index: SpatialBrickIndex::new(0, 0, 0),
            panel_bounds: CrossSectionPanelBounds {
                min_points: DVec2::ZERO,
                max_points: DVec2::new(width_points, height_points),
            },
            vertex_count: 4,
            cache_priority: GpuBrickAtlasPagePriority::default(),
        }
    }

    #[test]
    #[ignore = "requires a usable non-CPU GPU adapter"]
    fn gpu_chunked_cross_section_u8_writes_display_texture() {
        let layer_id = LayerId::new("ch0").unwrap();
        let shape = Shape3D::new(1, 1, 4).unwrap();
        let grid_to_world = GridToWorld::identity();
        let volume = DenseVolumeU8::new(
            DatasetId::new("gpu-cross-section-u8-display").unwrap(),
            layer_id.clone(),
            0,
            TimeIndex::new(0),
            shape,
            grid_to_world,
            vec![10, 20, 30, 40],
        )
        .unwrap();
        let brick = VolumeBrickU8 {
            scale_level: 0,
            brick_index: SpatialBrickIndex::new(0, 0, 0),
            chunk_index: BrickIndex {
                t: 0,
                z: 0,
                y: 0,
                x: 0,
            },
            region: VolumeRegion::new(0, 0, 0, 1, 1, 4).unwrap(),
            occupied: true,
            valid_voxel_count: 4,
            min: 10.0,
            max: 40.0,
            volume,
        };
        let resident = ResidentBrickSetU8::new(
            layer_id,
            TimeIndex::new(0),
            shape,
            grid_to_world,
            vec![brick],
        );
        let renderer = GpuRenderer::new_blocking().unwrap();
        let chunks = [full_panel_chunk_draw(4.0, 1.0)];
        let channels = [GpuCrossSectionChunkDisplayChannel::U8 {
            resident: &resident,
            brick_shape: shape,
            brick_grid_shape: Shape3D::new(1, 1, 1).unwrap(),
            transfer: linear_white_transfer(40.0),
            chunks: &chunks,
        }];
        let frame = renderer
            .render_cross_section_chunked_channels_to_display_texture(
                &channels,
                CrossSectionView::new(
                    DVec3::new(1.5, 0.0, 0.0),
                    CrossSectionPanel::Xy,
                    DQuat::IDENTITY,
                    1.0,
                    1.0,
                ),
                PresentationViewport::new(4.0, 1.0).unwrap(),
                RenderViewport::new(4, 1).unwrap(),
            )
            .unwrap();

        assert_eq!(frame.viewport, RenderViewport::new(4, 1).unwrap());
        assert_eq!(frame.diagnostics.channels, 1);
        assert_eq!(frame.diagnostics.texture_bytes, 16);
        assert_eq!(frame.diagnostics.output_bytes, 96);
        assert_eq!(frame.diagnostics.draw_calls, 1);
        assert_eq!(frame.diagnostics.vertex_count, 4);
        let rgba = renderer
            .read_display_frame_rgba_for_diagnostics(&frame)
            .unwrap();
        assert_rgba_abs_diff_le(
            &rgba,
            &[
                64, 64, 64, 255, 128, 128, 128, 255, 191, 191, 191, 255, 255, 255, 255, 255,
            ],
            1,
        );
    }

    #[test]
    #[ignore = "requires a usable non-CPU GPU adapter"]
    fn gpu_chunked_cross_section_u8_composites_multiple_display_channels() {
        let layer_id = LayerId::new("ch0").unwrap();
        let shape = Shape3D::new(1, 1, 4).unwrap();
        let grid_to_world = GridToWorld::identity();
        let volume = DenseVolumeU8::new(
            DatasetId::new("gpu-cross-section-u8-multi-display").unwrap(),
            layer_id.clone(),
            0,
            TimeIndex::new(0),
            shape,
            grid_to_world,
            vec![10, 20, 30, 40],
        )
        .unwrap();
        let brick = VolumeBrickU8 {
            scale_level: 0,
            brick_index: SpatialBrickIndex::new(0, 0, 0),
            chunk_index: BrickIndex {
                t: 0,
                z: 0,
                y: 0,
                x: 0,
            },
            region: VolumeRegion::new(0, 0, 0, 1, 1, 4).unwrap(),
            occupied: true,
            valid_voxel_count: 4,
            min: 10.0,
            max: 40.0,
            volume,
        };
        let resident = ResidentBrickSetU8::new(
            layer_id,
            TimeIndex::new(0),
            shape,
            grid_to_world,
            vec![brick],
        );
        let renderer = GpuRenderer::new_blocking().unwrap();
        let chunks = [full_panel_chunk_draw(4.0, 1.0)];
        let channels = [
            GpuCrossSectionChunkDisplayChannel::U8 {
                resident: &resident,
                brick_shape: shape,
                brick_grid_shape: Shape3D::new(1, 1, 1).unwrap(),
                transfer: linear_color_transfer([1.0, 0.0, 0.0, 1.0], 40.0),
                chunks: &chunks,
            },
            GpuCrossSectionChunkDisplayChannel::U8 {
                resident: &resident,
                brick_shape: shape,
                brick_grid_shape: Shape3D::new(1, 1, 1).unwrap(),
                transfer: linear_color_transfer([0.0, 1.0, 0.0, 1.0], 40.0),
                chunks: &chunks,
            },
        ];
        let frame = renderer
            .render_cross_section_chunked_channels_to_display_texture(
                &channels,
                CrossSectionView::new(
                    DVec3::new(1.5, 0.0, 0.0),
                    CrossSectionPanel::Xy,
                    DQuat::IDENTITY,
                    1.0,
                    1.0,
                ),
                PresentationViewport::new(4.0, 1.0).unwrap(),
                RenderViewport::new(4, 1).unwrap(),
            )
            .unwrap();

        assert_eq!(frame.viewport, RenderViewport::new(4, 1).unwrap());
        assert_eq!(frame.diagnostics.channels, 2);
        assert_eq!(frame.diagnostics.texture_bytes, 16);
        assert_eq!(frame.diagnostics.output_bytes, 192);
        assert_eq!(frame.diagnostics.draw_calls, 2);
        assert_eq!(frame.diagnostics.vertex_count, 8);
        let rgba = renderer
            .read_display_frame_rgba_for_diagnostics(&frame)
            .unwrap();
        assert_rgba_abs_diff_le(
            &rgba,
            &[
                64, 64, 0, 255, 128, 128, 0, 255, 191, 191, 0, 255, 255, 255, 0, 255,
            ],
            1,
        );
    }

    #[test]
    #[ignore = "requires a usable non-CPU GPU adapter"]
    fn gpu_chunked_cross_section_f32_writes_display_texture() {
        let layer_id = LayerId::new("ch0").unwrap();
        let shape = Shape3D::new(1, 1, 4).unwrap();
        let grid_to_world = GridToWorld::identity();
        let volume = DenseVolumeF32::new(
            DatasetId::new("gpu-cross-section-f32-display").unwrap(),
            layer_id.clone(),
            0,
            TimeIndex::new(0),
            shape,
            grid_to_world,
            vec![0.25, 0.5, 0.75, 1.0],
        )
        .unwrap();
        let brick = VolumeBrickF32 {
            scale_level: 0,
            brick_index: SpatialBrickIndex::new(0, 0, 0),
            chunk_index: BrickIndex {
                t: 0,
                z: 0,
                y: 0,
                x: 0,
            },
            region: VolumeRegion::new(0, 0, 0, 1, 1, 4).unwrap(),
            occupied: true,
            valid_voxel_count: 4,
            min: 0.25,
            max: 1.0,
            volume,
        };
        let resident = ResidentBrickSetF32::new(
            layer_id,
            TimeIndex::new(0),
            shape,
            grid_to_world,
            vec![brick],
        );
        let renderer = GpuRenderer::new_blocking().unwrap();
        let chunks = [full_panel_chunk_draw(4.0, 1.0)];
        let channels = [GpuCrossSectionChunkDisplayChannel::F32 {
            resident: &resident,
            brick_shape: shape,
            brick_grid_shape: Shape3D::new(1, 1, 1).unwrap(),
            transfer: linear_white_transfer(1.0),
            chunks: &chunks,
        }];
        let frame = renderer
            .render_cross_section_chunked_channels_to_display_texture(
                &channels,
                CrossSectionView::new(
                    DVec3::new(1.5, 0.0, 0.0),
                    CrossSectionPanel::Xy,
                    DQuat::IDENTITY,
                    1.0,
                    1.0,
                ),
                PresentationViewport::new(4.0, 1.0).unwrap(),
                RenderViewport::new(4, 1).unwrap(),
            )
            .unwrap();

        assert_eq!(frame.viewport, RenderViewport::new(4, 1).unwrap());
        assert_eq!(frame.diagnostics.channels, 1);
        assert_eq!(frame.diagnostics.texture_bytes, 16);
        assert_eq!(frame.diagnostics.output_bytes, 128);
        assert_eq!(frame.diagnostics.draw_calls, 1);
        assert_eq!(frame.diagnostics.vertex_count, 4);
        let rgba = renderer
            .read_display_frame_rgba_for_diagnostics(&frame)
            .unwrap();
        assert_rgba_abs_diff_le(
            &rgba,
            &[
                64, 64, 64, 255, 128, 128, 128, 255, 191, 191, 191, 255, 255, 255, 255, 255,
            ],
            1,
        );
    }
}
