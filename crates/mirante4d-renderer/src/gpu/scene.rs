use std::time::Instant;

use mirante4d_render_api::CameraFrame;
use wgpu::util::DeviceExt;

use super::{
    GpuDisplayFrame, GpuDisplayFrameDiagnostics, GpuRenderError, GpuRenderTimings, GpuRenderer,
    WORKGROUP_SIZE_X, WORKGROUP_SIZE_Y, add_gpu_render_timings, duration_ns_u64,
};
use crate::{
    PickCompleteness, PickHit, PickHitKind, PickPolicy, PickQuery, RenderError, RenderViewport,
    SceneLayerKind, ScreenPosition, empty_pick_hit,
    scene_render::{
        SceneRenderCommandList, SceneRenderOutput, SceneRgbaImage, build_scene_render_commands,
        scene_render_diagnostics,
    },
};

#[derive(Debug, Clone, PartialEq)]
pub struct GpuSceneRenderOutput {
    pub output: SceneRenderOutput,
    pub timings: Option<GpuRenderTimings>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GpuScenePickOutput {
    pub hit: PickHit,
    pub timings: Option<GpuRenderTimings>,
}

impl GpuRenderer {
    pub fn render_scene_layers_to_display_texture(
        &self,
        frame: GpuDisplayFrame,
        draw_list: &crate::SceneDrawList,
        camera: CameraFrame,
        viewport: RenderViewport,
    ) -> Result<GpuDisplayFrame, GpuRenderError> {
        if frame.viewport != viewport {
            return Err(RenderError::InvalidRgbaImageBuffer {
                width: viewport.width,
                height: viewport.height,
                expected: (viewport.width as usize) * (viewport.height as usize),
                actual: (frame.viewport.width as usize) * (frame.viewport.height as usize),
            }
            .into());
        }
        let commands = build_scene_render_commands(draw_list, camera, viewport);
        if commands.commands().is_empty() {
            return Ok(frame);
        }

        let viewport_width = super::buffers::checked_u32("viewport_width", viewport.width)?;
        let viewport_height = super::buffers::checked_u32("viewport_height", viewport.height)?;
        let command_count =
            super::buffers::checked_u32("scene_command_count", commands.commands().len() as u64)?;
        let pixel_count = (u64::from(viewport_width) * u64::from(viewport_height)) as usize;
        let texture_bytes =
            super::buffers::checked_buffer_byte_count("GPU display texture", pixel_count, 4)?;
        let resources = self.display_resources_for_viewport(
            frame.viewport,
            viewport_width,
            viewport_height,
            frame.diagnostics.accumulator_bytes,
            texture_bytes,
        )?;
        let upload_started = Instant::now();
        let params_u32 = [viewport_width, viewport_height, command_count, 0];
        let mut command_u32 = Vec::with_capacity(commands.commands().len() * 4);
        let mut command_f32 = Vec::with_capacity(commands.commands().len() * 6);
        for command in commands.commands() {
            command_u32.extend_from_slice(&command.shader_u32_fields());
            command_f32.extend_from_slice(&command.shader_f32_fields());
        }
        let params_u32_bytes = super::buffers::checked_buffer_byte_count(
            "GPU scene overlay parameters",
            params_u32.len(),
            std::mem::size_of::<u32>(),
        )?;
        let command_u32_bytes = super::buffers::checked_buffer_byte_count(
            "GPU scene overlay u32 commands",
            command_u32.len(),
            std::mem::size_of::<u32>(),
        )?;
        let command_f32_bytes = super::buffers::checked_buffer_byte_count(
            "GPU scene overlay f32 commands",
            command_f32.len(),
            std::mem::size_of::<f32>(),
        )?;
        super::buffers::validate_storage_buffer_bytes(
            &self.device.limits(),
            "GPU scene overlay parameters",
            params_u32_bytes,
        )?;
        super::buffers::validate_storage_buffer_bytes(
            &self.device.limits(),
            "GPU scene overlay u32 commands",
            command_u32_bytes,
        )?;
        super::buffers::validate_storage_buffer_bytes(
            &self.device.limits(),
            "GPU scene overlay f32 commands",
            command_f32_bytes,
        )?;
        let overlay_resources = self.scene_overlay_display_resources(
            frame.viewport,
            params_u32_bytes,
            command_u32_bytes,
            command_f32_bytes,
        )?;
        self.queue.write_buffer(
            &overlay_resources.params_u32_buffer,
            0,
            bytemuck::cast_slice(&params_u32),
        );
        self.queue.write_buffer(
            &overlay_resources.command_u32_buffer,
            0,
            bytemuck::cast_slice(&command_u32),
        );
        self.queue.write_buffer(
            &overlay_resources.command_f32_buffer,
            0,
            bytemuck::cast_slice(&command_f32),
        );
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mirante4d-scene-render-texture-bind-group"),
            layout: &self.scene_render_texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(frame.texture_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&resources.scene_overlay_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: overlay_resources.params_u32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: overlay_resources.command_u32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: overlay_resources.command_f32_buffer.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-scene-render-texture-command-encoder"),
            });
        let timestamp = self.timestamp_query_pair("mirante4d-scene-render-texture-timestamps");
        let timestamp_writes = timestamp
            .as_ref()
            .map(super::readback::GpuTimestampQueryPair::compute_pass_writes);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-scene-render-texture-compute-pass"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.scene_render_texture_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                viewport_width.div_ceil(WORKGROUP_SIZE_X),
                viewport_height.div_ceil(WORKGROUP_SIZE_Y),
                1,
            );
        }
        let upload_ns = duration_ns_u64(upload_started.elapsed());
        let gpu_compute_ns = self.submit_with_optional_timestamp(encoder, timestamp)?;
        let timings = add_gpu_render_timings(
            frame.timings,
            GpuRenderTimings {
                upload_ns,
                gpu_compute_ns,
            },
        );
        Ok(GpuDisplayFrame {
            viewport: frame.viewport,
            diagnostics: GpuDisplayFrameDiagnostics {
                channels: frame.diagnostics.channels,
                output_bytes: frame.diagnostics.output_bytes,
                accumulator_bytes: frame.diagnostics.accumulator_bytes,
                texture_bytes: resources
                    .texture_bytes
                    .saturating_add(resources.scene_overlay_texture_bytes),
                draw_calls: frame.diagnostics.draw_calls,
                vertex_count: frame.diagnostics.vertex_count,
            },
            timings,
            adapter: frame.adapter,
            texture: resources.scene_overlay_texture,
            view: resources.scene_overlay_view,
        })
    }

    pub fn render_scene_layers_rgba(
        &self,
        base: &SceneRgbaImage,
        draw_list: &crate::SceneDrawList,
        camera: CameraFrame,
        viewport: RenderViewport,
    ) -> Result<SceneRenderOutput, GpuRenderError> {
        Ok(self
            .render_scene_layers_rgba_with_timings(base, draw_list, camera, viewport)?
            .output)
    }

    pub fn render_scene_layers_rgba_with_timings(
        &self,
        base: &SceneRgbaImage,
        draw_list: &crate::SceneDrawList,
        camera: CameraFrame,
        viewport: RenderViewport,
    ) -> Result<GpuSceneRenderOutput, GpuRenderError> {
        if base.width != viewport.width || base.height != viewport.height {
            let expected = (viewport.width as usize) * (viewport.height as usize);
            return Err(RenderError::InvalidRgbaImageBuffer {
                width: viewport.width,
                height: viewport.height,
                expected,
                actual: base.pixels().len(),
            }
            .into());
        }

        let commands = build_scene_render_commands(draw_list, camera, viewport);
        if commands.commands().is_empty() {
            let output_pixels = base.pixels().to_vec();
            let diagnostics = scene_render_diagnostics(base, &commands, &output_pixels);
            return Ok(GpuSceneRenderOutput {
                output: SceneRenderOutput {
                    image: SceneRgbaImage::new(base.width, base.height, output_pixels)?,
                    diagnostics,
                },
                timings: None,
            });
        }

        let (output_pixels, timings) =
            self.render_scene_commands_rgba_with_timings(base, &commands, viewport)?;
        let diagnostics = scene_render_diagnostics(base, &commands, &output_pixels);
        Ok(GpuSceneRenderOutput {
            output: SceneRenderOutput {
                image: SceneRgbaImage::new(viewport.width, viewport.height, output_pixels)?,
                diagnostics,
            },
            timings,
        })
    }

    pub fn pick_scene_object_id(
        &self,
        draw_list: &crate::SceneDrawList,
        camera: CameraFrame,
        viewport: RenderViewport,
        query: PickQuery,
    ) -> Result<PickHit, GpuRenderError> {
        Ok(self
            .pick_scene_object_id_with_timings(draw_list, camera, viewport, query)?
            .hit)
    }

    pub fn pick_scene_object_id_with_timings(
        &self,
        draw_list: &crate::SceneDrawList,
        camera: CameraFrame,
        viewport: RenderViewport,
        query: PickQuery,
    ) -> Result<GpuScenePickOutput, GpuRenderError> {
        if !query.screen_position.x.is_finite()
            || !query.screen_position.y.is_finite()
            || query.screen_position.x < 0.0
            || query.screen_position.y < 0.0
            || query.screen_position.x >= viewport.width as f32
            || query.screen_position.y >= viewport.height as f32
        {
            return Ok(GpuScenePickOutput {
                hit: empty_pick_hit(query),
                timings: None,
            });
        }

        let commands = build_scene_render_commands(draw_list, camera, viewport);
        if commands.commands().is_empty() {
            return Ok(GpuScenePickOutput {
                hit: empty_pick_hit(query),
                timings: None,
            });
        }

        let (pick_id, timings) =
            self.pick_scene_command_id_with_timings(&commands, query.screen_position)?;
        let Some(record) = commands.pick_record(pick_id) else {
            return Ok(GpuScenePickOutput {
                hit: empty_pick_hit(query),
                timings,
            });
        };

        Ok(GpuScenePickOutput {
            hit: PickHit {
                kind: pick_hit_kind_for_scene_layer_kind(record.layer_kind),
                layer_id: Some(record.layer_id.clone()),
                object_id: Some(record.object_id.clone()),
                source_layer_id: record.source_layer_id.clone(),
                timepoint: query.timepoint,
                world_position: None,
                grid_position: None,
                screen_position: Some(query.screen_position),
                value: None,
                policy: PickPolicy::SceneObject,
                completeness: PickCompleteness::Exact,
            },
            timings,
        })
    }

    fn render_scene_commands_rgba_with_timings(
        &self,
        base: &SceneRgbaImage,
        commands: &SceneRenderCommandList,
        viewport: RenderViewport,
    ) -> Result<(Vec<u32>, Option<GpuRenderTimings>), GpuRenderError> {
        let viewport_width = super::buffers::checked_u32("viewport_width", viewport.width)?;
        let viewport_height = super::buffers::checked_u32("viewport_height", viewport.height)?;
        let command_count =
            super::buffers::checked_u32("scene_command_count", commands.commands().len() as u64)?;
        let output_len = (u64::from(viewport_width) * u64::from(viewport_height)) as usize;
        let output_bytes = (output_len * std::mem::size_of::<u32>()) as wgpu::BufferAddress;

        let upload_started = Instant::now();
        let base_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mirante4d-scene-render-base-rgba"),
                contents: bytemuck::cast_slice(base.pixels()),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-scene-render-output-rgba"),
            size: output_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-scene-render-readback-rgba"),
            size: output_bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let params_u32 = [viewport_width, viewport_height, command_count, 0];
        let params_u32_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mirante4d-scene-render-params-u32"),
                contents: bytemuck::cast_slice(&params_u32),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let mut command_u32 = Vec::with_capacity(commands.commands().len() * 4);
        let mut command_f32 = Vec::with_capacity(commands.commands().len() * 6);
        for command in commands.commands() {
            command_u32.extend_from_slice(&command.shader_u32_fields());
            command_f32.extend_from_slice(&command.shader_f32_fields());
        }
        let command_u32_buffer =
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("mirante4d-scene-render-commands-u32"),
                    contents: bytemuck::cast_slice(&command_u32),
                    usage: wgpu::BufferUsages::STORAGE,
                });
        let command_f32_buffer =
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("mirante4d-scene-render-commands-f32"),
                    contents: bytemuck::cast_slice(&command_f32),
                    usage: wgpu::BufferUsages::STORAGE,
                });

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mirante4d-scene-render-bind-group"),
            layout: &self.scene_render_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: base_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: output_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params_u32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: command_u32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: command_f32_buffer.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-scene-render-command-encoder"),
            });
        let timestamp = self.timestamp_query_pair("mirante4d-scene-render-readback-timestamp");
        let timestamp_writes = timestamp
            .as_ref()
            .map(super::readback::GpuTimestampQueryPair::compute_pass_writes);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-scene-render-compute-pass"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.scene_render_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                viewport_width.div_ceil(WORKGROUP_SIZE_X),
                viewport_height.div_ceil(WORKGROUP_SIZE_Y),
                1,
            );
        }
        encoder.copy_buffer_to_buffer(&output_buffer, 0, &readback_buffer, 0, output_bytes);
        let upload_ns = duration_ns_u64(upload_started.elapsed());
        let (pixels, gpu_compute_ns) =
            self.submit_and_read_u32_with_optional_timestamp(encoder, readback_buffer, timestamp)?;
        Ok((
            pixels,
            Some(GpuRenderTimings {
                upload_ns,
                gpu_compute_ns,
            }),
        ))
    }

    fn pick_scene_command_id_with_timings(
        &self,
        commands: &SceneRenderCommandList,
        screen_position: ScreenPosition,
    ) -> Result<(u32, Option<GpuRenderTimings>), GpuRenderError> {
        let command_count =
            super::buffers::checked_u32("scene_command_count", commands.commands().len() as u64)?;
        if command_count == 0 {
            return Ok((0, None));
        }
        let output_bytes = std::mem::size_of::<u32>() as wgpu::BufferAddress;
        let upload_started = Instant::now();
        let output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-scene-pick-output-u32"),
            size: output_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-scene-pick-readback-u32"),
            size: output_bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let params_u32 = [command_count, 0, 0, 0];
        let params_f32 = [screen_position.x, screen_position.y, 0.0, 0.0];
        let params_u32_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mirante4d-scene-pick-params-u32"),
                contents: bytemuck::cast_slice(&params_u32),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let params_f32_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mirante4d-scene-pick-params-f32"),
                contents: bytemuck::cast_slice(&params_f32),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let mut command_u32 = Vec::with_capacity(commands.commands().len() * 4);
        let mut command_f32 = Vec::with_capacity(commands.commands().len() * 6);
        for command in commands.commands() {
            command_u32.extend_from_slice(&command.shader_u32_fields());
            command_f32.extend_from_slice(&command.shader_f32_fields());
        }
        let command_u32_buffer =
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("mirante4d-scene-pick-commands-u32"),
                    contents: bytemuck::cast_slice(&command_u32),
                    usage: wgpu::BufferUsages::STORAGE,
                });
        let command_f32_buffer =
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("mirante4d-scene-pick-commands-f32"),
                    contents: bytemuck::cast_slice(&command_f32),
                    usage: wgpu::BufferUsages::STORAGE,
                });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mirante4d-scene-pick-bind-group"),
            layout: &self.scene_pick_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_u32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: params_f32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: command_u32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: command_f32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: output_buffer.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-scene-pick-command-encoder"),
            });
        let timestamp = self.timestamp_query_pair("mirante4d-scene-pick-timestamp");
        let timestamp_writes = timestamp
            .as_ref()
            .map(super::readback::GpuTimestampQueryPair::compute_pass_writes);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-scene-pick-compute-pass"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.scene_pick_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output_buffer, 0, &readback_buffer, 0, output_bytes);
        let upload_ns = duration_ns_u64(upload_started.elapsed());
        let (output, gpu_compute_ns) =
            self.submit_and_read_u32_with_optional_timestamp(encoder, readback_buffer, timestamp)?;
        let pick_id = output.first().copied().unwrap_or(0);
        Ok((
            pick_id,
            Some(GpuRenderTimings {
                upload_ns,
                gpu_compute_ns,
            }),
        ))
    }
}

fn pick_hit_kind_for_scene_layer_kind(layer_kind: SceneLayerKind) -> PickHitKind {
    match layer_kind {
        SceneLayerKind::Track => PickHitKind::Track,
        SceneLayerKind::Measurement => PickHitKind::Measurement,
        SceneLayerKind::Annotation => PickHitKind::Annotation,
        SceneLayerKind::Interaction | SceneLayerKind::Reference => PickHitKind::Annotation,
    }
}
