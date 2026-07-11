use wgpu::util::DeviceExt;

use super::{
    GpuRenderError, GpuRenderer,
    atlas::{GpuBrickAtlasF32Resource, GpuBrickAtlasResource},
};
use crate::{RenderError, RenderViewport};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct GpuDisplayResourceStats {
    pub(super) cache_hits: u64,
    pub(super) cache_misses: u64,
    pub(super) recreations: u64,
    pub(super) resident_bytes: u64,
}

#[derive(Debug, Default)]
pub(super) struct GpuDisplayResourceCache {
    resources: Option<GpuDisplayResources>,
    stats: GpuDisplayResourceStats,
}

impl GpuDisplayResourceCache {
    pub(super) fn stats(&self) -> GpuDisplayResourceStats {
        self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource_key(width: u64, height: u64) -> GpuDisplayResourceKey {
        GpuDisplayResourceKey {
            viewport: RenderViewport::new(width, height).unwrap(),
            format: wgpu::TextureFormat::Rgba8Unorm,
        }
    }

    #[test]
    fn display_resource_allocation_reuses_larger_accumulator_for_smaller_request() {
        assert!(display_resource_allocation_compatible(
            resource_key(64, 64),
            64 * 64 * 16,
            64 * 64 * 4,
            resource_key(64, 64),
            16,
            64 * 64 * 4,
        ));
    }

    #[test]
    fn display_resource_allocation_rejects_smaller_accumulator_for_larger_request() {
        assert!(!display_resource_allocation_compatible(
            resource_key(64, 64),
            16,
            64 * 64 * 4,
            resource_key(64, 64),
            64 * 64 * 16,
            64 * 64 * 4,
        ));
    }

    #[test]
    fn display_resource_allocation_rejects_different_viewport_or_texture_bytes() {
        assert!(!display_resource_allocation_compatible(
            resource_key(64, 64),
            64 * 64 * 16,
            64 * 64 * 4,
            resource_key(32, 64),
            32 * 64 * 16,
            32 * 64 * 4,
        ));
        assert!(!display_resource_allocation_compatible(
            resource_key(64, 64),
            64 * 64 * 16,
            64 * 64 * 4,
            resource_key(64, 64),
            64 * 64 * 16,
            64 * 64 * 8,
        ));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GpuDisplayResourceKey {
    viewport: RenderViewport,
    format: wgpu::TextureFormat,
}

fn display_resource_allocation_compatible(
    cached_key: GpuDisplayResourceKey,
    cached_accumulator_bytes: u64,
    cached_texture_bytes: u64,
    requested_key: GpuDisplayResourceKey,
    requested_accumulator_bytes: u64,
    requested_texture_bytes: u64,
) -> bool {
    cached_key == requested_key
        && cached_accumulator_bytes >= requested_accumulator_bytes
        && cached_texture_bytes == requested_texture_bytes
}

#[derive(Debug)]
struct GpuDisplayResources {
    key: GpuDisplayResourceKey,
    accumulator: wgpu::Buffer,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    scene_overlay_texture: wgpu::Texture,
    scene_overlay_view: wgpu::TextureView,
    _final_params_buffer: wgpu::Buffer,
    final_bind_group: wgpu::BindGroup,
    accumulator_bytes: u64,
    texture_bytes: u64,
    scene_overlay_texture_bytes: u64,
    integer_camera_outputs: Vec<GpuIntegerCameraDisplayBuffers>,
    f32_camera_outputs: Vec<GpuF32CameraDisplayBuffers>,
    channel_composites: Vec<GpuDisplayChannelCompositeBuffers>,
    iso_multi_channel: Option<GpuIsoMultiChannelDisplayBuffers>,
    dvr_multi_channel: Option<GpuDvrMultiChannelDisplayBuffers>,
    scene_overlay: Option<GpuSceneOverlayDisplayBuffers>,
}

#[derive(Debug, Clone)]
pub(super) struct GpuDisplayResourceHandles {
    pub(super) accumulator: wgpu::Buffer,
    pub(super) texture: wgpu::Texture,
    pub(super) view: wgpu::TextureView,
    pub(super) scene_overlay_texture: wgpu::Texture,
    pub(super) scene_overlay_view: wgpu::TextureView,
    pub(super) final_bind_group: wgpu::BindGroup,
    pub(super) accumulator_bytes: u64,
    pub(super) texture_bytes: u64,
    pub(super) scene_overlay_texture_bytes: u64,
}

#[derive(Debug)]
struct GpuIntegerCameraDisplayBuffers {
    output_buffer: wgpu::Buffer,
    params_u32_buffer: wgpu::Buffer,
    params_f32_buffer: wgpu::Buffer,
    skip_diagnostics_buffer: wgpu::Buffer,
    output_bytes: u64,
    params_u32_bytes: u64,
    params_f32_bytes: u64,
    skip_diagnostics_bytes: u64,
    bind_group_key: Option<GpuCameraDisplayBindGroupKey>,
    bind_group: Option<wgpu::BindGroup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GpuIntegerCameraDisplayBufferSpec {
    pub(super) output_bytes: u64,
    pub(super) params_u32_bytes: u64,
    pub(super) params_f32_bytes: u64,
    pub(super) skip_diagnostics_bytes: u64,
}

#[derive(Debug, Clone)]
pub(super) struct GpuIntegerCameraDisplayBufferHandles {
    pub(super) output_buffer: wgpu::Buffer,
    pub(super) params_u32_buffer: wgpu::Buffer,
    pub(super) params_f32_buffer: wgpu::Buffer,
    pub(super) skip_diagnostics_buffer: wgpu::Buffer,
    pub(super) bind_group: wgpu::BindGroup,
    pub(super) output_bytes: u64,
}

#[derive(Debug)]
struct GpuF32CameraDisplayBuffers {
    output_buffer: wgpu::Buffer,
    params_u32_buffer: wgpu::Buffer,
    params_f32_buffer: wgpu::Buffer,
    output_bytes: u64,
    params_u32_bytes: u64,
    params_f32_bytes: u64,
    bind_group_key: Option<GpuCameraDisplayBindGroupKey>,
    bind_group: Option<wgpu::BindGroup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GpuF32CameraDisplayBufferSpec {
    pub(super) output_bytes: u64,
    pub(super) params_u32_bytes: u64,
    pub(super) params_f32_bytes: u64,
}

#[derive(Debug, Clone)]
pub(super) struct GpuF32CameraDisplayBufferHandles {
    pub(super) output_buffer: wgpu::Buffer,
    pub(super) params_u32_buffer: wgpu::Buffer,
    pub(super) params_f32_buffer: wgpu::Buffer,
    pub(super) bind_group: wgpu::BindGroup,
    pub(super) output_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GpuCameraDisplayBindGroupKey {
    atlas_generation: u64,
    binding_contract: GpuCameraDisplayBindingContract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GpuCameraDisplayBindingContract {
    Standard,
    IntegerChunkDrawList,
}

#[derive(Debug)]
struct GpuDisplayChannelCompositeBuffers {
    params_u32_buffer: wgpu::Buffer,
    params_f32_buffer: wgpu::Buffer,
    params_u32_bytes: u64,
    params_f32_bytes: u64,
}

#[derive(Debug, Clone)]
pub(super) struct GpuDisplayChannelCompositeBufferHandles {
    pub(super) params_u32_buffer: wgpu::Buffer,
    pub(super) params_f32_buffer: wgpu::Buffer,
}

#[derive(Debug)]
struct GpuIsoMultiChannelDisplayBuffers {
    combined_output_buffer: wgpu::Buffer,
    channel_params_u32_buffer: wgpu::Buffer,
    channel_params_f32_buffer: wgpu::Buffer,
    frame_params_u32_buffer: wgpu::Buffer,
    frame_params_f32_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    output_bytes: u64,
    channel_params_u32_bytes: u64,
    channel_params_f32_bytes: u64,
    frame_params_u32_bytes: u64,
    frame_params_f32_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GpuIsoMultiChannelDisplayBufferSizes {
    output_bytes: u64,
    channel_params_u32_bytes: u64,
    channel_params_f32_bytes: u64,
    frame_params_u32_bytes: u64,
    frame_params_f32_bytes: u64,
}

#[derive(Debug, Clone)]
pub(super) struct GpuIsoMultiChannelDisplayBufferHandles {
    pub(super) combined_output_buffer: wgpu::Buffer,
    pub(super) channel_params_u32_buffer: wgpu::Buffer,
    pub(super) channel_params_f32_buffer: wgpu::Buffer,
    pub(super) frame_params_u32_buffer: wgpu::Buffer,
    pub(super) frame_params_f32_buffer: wgpu::Buffer,
    pub(super) bind_group: wgpu::BindGroup,
}

#[derive(Debug)]
struct GpuDvrMultiChannelDisplayBuffers {
    packed_values_buffer: wgpu::Buffer,
    validity_buffer: wgpu::Buffer,
    f32_values_buffer: wgpu::Buffer,
    page_table_buffer: wgpu::Buffer,
    metadata_buffer: wgpu::Buffer,
    channel_params_u32_buffer: wgpu::Buffer,
    channel_params_f32_buffer: wgpu::Buffer,
    frame_params_u32_buffer: wgpu::Buffer,
    frame_params_f32_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    packed_values_bytes: u64,
    validity_bytes: u64,
    f32_values_bytes: u64,
    page_table_bytes: u64,
    metadata_bytes: u64,
    channel_params_u32_bytes: u64,
    channel_params_f32_bytes: u64,
    frame_params_u32_bytes: u64,
    frame_params_f32_bytes: u64,
}

#[derive(Debug, Clone)]
pub(super) struct GpuDvrMultiChannelDisplayBufferHandles {
    pub(super) packed_values_buffer: wgpu::Buffer,
    pub(super) validity_buffer: wgpu::Buffer,
    pub(super) f32_values_buffer: wgpu::Buffer,
    pub(super) page_table_buffer: wgpu::Buffer,
    pub(super) metadata_buffer: wgpu::Buffer,
    pub(super) channel_params_u32_buffer: wgpu::Buffer,
    pub(super) channel_params_f32_buffer: wgpu::Buffer,
    pub(super) frame_params_u32_buffer: wgpu::Buffer,
    pub(super) frame_params_f32_buffer: wgpu::Buffer,
    pub(super) bind_group: wgpu::BindGroup,
}

#[derive(Debug)]
struct GpuSceneOverlayDisplayBuffers {
    params_u32_buffer: wgpu::Buffer,
    command_u32_buffer: wgpu::Buffer,
    command_f32_buffer: wgpu::Buffer,
    params_u32_bytes: u64,
    command_u32_bytes: u64,
    command_f32_bytes: u64,
}

#[derive(Debug, Clone)]
pub(super) struct GpuSceneOverlayDisplayBufferHandles {
    pub(super) params_u32_buffer: wgpu::Buffer,
    pub(super) command_u32_buffer: wgpu::Buffer,
    pub(super) command_f32_buffer: wgpu::Buffer,
}

fn storage_copy_dst_buffer(device: &wgpu::Device, label: &'static str, bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn uniform_copy_dst_buffer(device: &wgpu::Device, label: &'static str, bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

impl GpuDisplayResources {
    fn handles(&self) -> GpuDisplayResourceHandles {
        GpuDisplayResourceHandles {
            accumulator: self.accumulator.clone(),
            texture: self.texture.clone(),
            view: self.view.clone(),
            scene_overlay_texture: self.scene_overlay_texture.clone(),
            scene_overlay_view: self.scene_overlay_view.clone(),
            final_bind_group: self.final_bind_group.clone(),
            accumulator_bytes: self.accumulator_bytes,
            texture_bytes: self.texture_bytes,
            scene_overlay_texture_bytes: self.scene_overlay_texture_bytes,
        }
    }

    fn resident_bytes(&self) -> u64 {
        let final_params_bytes = std::mem::size_of::<[u32; 2]>() as u64;
        self.accumulator_bytes
            .saturating_add(self.texture_bytes)
            .saturating_add(self.scene_overlay_texture_bytes)
            .saturating_add(final_params_bytes)
            .saturating_add(
                self.integer_camera_outputs
                    .iter()
                    .fold(0_u64, |total, buffers| {
                        total.saturating_add(buffers.resident_bytes())
                    }),
            )
            .saturating_add(
                self.f32_camera_outputs
                    .iter()
                    .fold(0_u64, |total, buffers| {
                        total.saturating_add(buffers.resident_bytes())
                    }),
            )
            .saturating_add(
                self.channel_composites
                    .iter()
                    .fold(0_u64, |total, buffers| {
                        total.saturating_add(buffers.resident_bytes())
                    }),
            )
            .saturating_add(
                self.iso_multi_channel
                    .as_ref()
                    .map(GpuIsoMultiChannelDisplayBuffers::resident_bytes)
                    .unwrap_or(0),
            )
            .saturating_add(
                self.dvr_multi_channel
                    .as_ref()
                    .map(GpuDvrMultiChannelDisplayBuffers::resident_bytes)
                    .unwrap_or(0),
            )
            .saturating_add(
                self.scene_overlay
                    .as_ref()
                    .map(GpuSceneOverlayDisplayBuffers::resident_bytes)
                    .unwrap_or(0),
            )
    }

    fn integer_camera_output_handles(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        atlas: &GpuBrickAtlasResource,
        slot: usize,
        spec: GpuIntegerCameraDisplayBufferSpec,
        binding_contract: GpuCameraDisplayBindingContract,
    ) -> GpuIntegerCameraDisplayBufferHandles {
        while self.integer_camera_outputs.len() <= slot {
            self.integer_camera_outputs
                .push(GpuIntegerCameraDisplayBuffers::new(
                    device,
                    spec.output_bytes,
                    spec.params_u32_bytes,
                    spec.params_f32_bytes,
                    spec.skip_diagnostics_bytes,
                ));
        }
        let buffers = &mut self.integer_camera_outputs[slot];
        if !buffers.matches(
            spec.output_bytes,
            spec.params_u32_bytes,
            spec.params_f32_bytes,
            spec.skip_diagnostics_bytes,
        ) {
            *buffers = GpuIntegerCameraDisplayBuffers::new(
                device,
                spec.output_bytes,
                spec.params_u32_bytes,
                spec.params_f32_bytes,
                spec.skip_diagnostics_bytes,
            );
        }
        buffers.handles(device, layout, atlas, binding_contract)
    }

    fn f32_camera_output_handles(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        atlas: &GpuBrickAtlasF32Resource,
        slot: usize,
        spec: GpuF32CameraDisplayBufferSpec,
    ) -> GpuF32CameraDisplayBufferHandles {
        while self.f32_camera_outputs.len() <= slot {
            self.f32_camera_outputs
                .push(GpuF32CameraDisplayBuffers::new(
                    device,
                    spec.output_bytes,
                    spec.params_u32_bytes,
                    spec.params_f32_bytes,
                ));
        }
        let buffers = &mut self.f32_camera_outputs[slot];
        if !buffers.matches(
            spec.output_bytes,
            spec.params_u32_bytes,
            spec.params_f32_bytes,
        ) {
            *buffers = GpuF32CameraDisplayBuffers::new(
                device,
                spec.output_bytes,
                spec.params_u32_bytes,
                spec.params_f32_bytes,
            );
        }
        buffers.handles(device, layout, atlas)
    }

    fn channel_composite_handles(
        &mut self,
        device: &wgpu::Device,
        slot: usize,
        params_u32_bytes: u64,
        params_f32_bytes: u64,
    ) -> GpuDisplayChannelCompositeBufferHandles {
        while self.channel_composites.len() <= slot {
            self.channel_composites
                .push(GpuDisplayChannelCompositeBuffers::new(
                    device,
                    params_u32_bytes,
                    params_f32_bytes,
                ));
        }
        let buffers = &mut self.channel_composites[slot];
        if !buffers.matches(params_u32_bytes, params_f32_bytes) {
            *buffers =
                GpuDisplayChannelCompositeBuffers::new(device, params_u32_bytes, params_f32_bytes);
        }
        buffers.handles()
    }

    fn iso_multi_channel_handles(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sizes: GpuIsoMultiChannelDisplayBufferSizes,
    ) -> GpuIsoMultiChannelDisplayBufferHandles {
        let needs_create = self
            .iso_multi_channel
            .as_ref()
            .map(|buffers| !buffers.matches(sizes))
            .unwrap_or(true);
        if needs_create {
            self.iso_multi_channel = Some(GpuIsoMultiChannelDisplayBuffers::new(
                device, layout, &self.view, sizes,
            ));
        }
        self.iso_multi_channel
            .as_ref()
            .expect("ISO display resources were created")
            .handles()
    }

    #[allow(clippy::too_many_arguments)]
    fn dvr_multi_channel_handles(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        packed_values_bytes: u64,
        validity_bytes: u64,
        f32_values_bytes: u64,
        page_table_bytes: u64,
        metadata_bytes: u64,
        channel_params_u32_bytes: u64,
        channel_params_f32_bytes: u64,
        frame_params_u32_bytes: u64,
        frame_params_f32_bytes: u64,
    ) -> GpuDvrMultiChannelDisplayBufferHandles {
        let needs_create = self
            .dvr_multi_channel
            .as_ref()
            .map(|buffers| {
                !buffers.matches(
                    packed_values_bytes,
                    validity_bytes,
                    f32_values_bytes,
                    page_table_bytes,
                    metadata_bytes,
                    channel_params_u32_bytes,
                    channel_params_f32_bytes,
                    frame_params_u32_bytes,
                    frame_params_f32_bytes,
                )
            })
            .unwrap_or(true);
        if needs_create {
            self.dvr_multi_channel = Some(GpuDvrMultiChannelDisplayBuffers::new(
                device,
                layout,
                &self.view,
                packed_values_bytes,
                validity_bytes,
                f32_values_bytes,
                page_table_bytes,
                metadata_bytes,
                channel_params_u32_bytes,
                channel_params_f32_bytes,
                frame_params_u32_bytes,
                frame_params_f32_bytes,
            ));
        }
        self.dvr_multi_channel
            .as_ref()
            .expect("DVR display resources were created")
            .handles()
    }

    fn scene_overlay_handles(
        &mut self,
        device: &wgpu::Device,
        params_u32_bytes: u64,
        command_u32_bytes: u64,
        command_f32_bytes: u64,
    ) -> GpuSceneOverlayDisplayBufferHandles {
        let needs_create = self
            .scene_overlay
            .as_ref()
            .map(|buffers| !buffers.matches(params_u32_bytes, command_u32_bytes, command_f32_bytes))
            .unwrap_or(true);
        if needs_create {
            self.scene_overlay = Some(GpuSceneOverlayDisplayBuffers::new(
                device,
                params_u32_bytes,
                command_u32_bytes,
                command_f32_bytes,
            ));
        }
        self.scene_overlay
            .as_ref()
            .expect("scene overlay display resources were created")
            .handles()
    }
}

impl GpuIntegerCameraDisplayBuffers {
    fn new(
        device: &wgpu::Device,
        output_bytes: u64,
        params_u32_bytes: u64,
        params_f32_bytes: u64,
        skip_diagnostics_bytes: u64,
    ) -> Self {
        let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-bricked-camera-display-output-u32"),
            size: output_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let params_u32_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-bricked-camera-display-params-u32"),
            size: params_u32_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let params_f32_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-bricked-camera-display-params-f32"),
            size: params_f32_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let skip_diagnostics_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-bricked-camera-skip-diagnostics"),
            size: skip_diagnostics_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            output_buffer,
            params_u32_buffer,
            params_f32_buffer,
            skip_diagnostics_buffer,
            output_bytes,
            params_u32_bytes,
            params_f32_bytes,
            skip_diagnostics_bytes,
            bind_group_key: None,
            bind_group: None,
        }
    }

    fn matches(
        &self,
        output_bytes: u64,
        params_u32_bytes: u64,
        params_f32_bytes: u64,
        skip_diagnostics_bytes: u64,
    ) -> bool {
        self.output_bytes == output_bytes
            && self.params_u32_bytes == params_u32_bytes
            && self.params_f32_bytes == params_f32_bytes
            && self.skip_diagnostics_bytes == skip_diagnostics_bytes
    }

    fn handles(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        atlas: &GpuBrickAtlasResource,
        binding_contract: GpuCameraDisplayBindingContract,
    ) -> GpuIntegerCameraDisplayBufferHandles {
        let key = GpuCameraDisplayBindGroupKey {
            atlas_generation: atlas.generation,
            binding_contract,
        };
        if self.bind_group_key != Some(key) {
            self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mirante4d-bricked-camera-display-bind-group"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: atlas.packed_values_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.output_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.params_u32_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.params_f32_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: atlas.page_table_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: atlas.validity_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: atlas.metadata_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: self.skip_diagnostics_buffer.as_entire_binding(),
                    },
                ],
            }));
            self.bind_group_key = Some(key);
        }
        GpuIntegerCameraDisplayBufferHandles {
            output_buffer: self.output_buffer.clone(),
            params_u32_buffer: self.params_u32_buffer.clone(),
            params_f32_buffer: self.params_f32_buffer.clone(),
            skip_diagnostics_buffer: self.skip_diagnostics_buffer.clone(),
            bind_group: self
                .bind_group
                .as_ref()
                .expect("integer display camera bind group exists")
                .clone(),
            output_bytes: self.output_bytes,
        }
    }

    fn resident_bytes(&self) -> u64 {
        self.output_bytes
            .saturating_add(self.params_u32_bytes)
            .saturating_add(self.params_f32_bytes)
            .saturating_add(self.skip_diagnostics_bytes)
    }
}

impl GpuF32CameraDisplayBuffers {
    fn new(
        device: &wgpu::Device,
        output_bytes: u64,
        params_u32_bytes: u64,
        params_f32_bytes: u64,
    ) -> Self {
        let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-bricked-camera-f32-display-output"),
            size: output_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let params_u32_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-bricked-camera-f32-display-params-u32"),
            size: params_u32_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let params_f32_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-bricked-camera-f32-display-params-f32"),
            size: params_f32_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            output_buffer,
            params_u32_buffer,
            params_f32_buffer,
            output_bytes,
            params_u32_bytes,
            params_f32_bytes,
            bind_group_key: None,
            bind_group: None,
        }
    }

    fn matches(&self, output_bytes: u64, params_u32_bytes: u64, params_f32_bytes: u64) -> bool {
        self.output_bytes == output_bytes
            && self.params_u32_bytes == params_u32_bytes
            && self.params_f32_bytes == params_f32_bytes
    }

    fn handles(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        atlas: &GpuBrickAtlasF32Resource,
    ) -> GpuF32CameraDisplayBufferHandles {
        let key = GpuCameraDisplayBindGroupKey {
            atlas_generation: atlas.generation,
            binding_contract: GpuCameraDisplayBindingContract::Standard,
        };
        if self.bind_group_key != Some(key) {
            self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mirante4d-bricked-camera-f32-display-bind-group"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: atlas.values_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.output_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.params_u32_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.params_f32_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: atlas.page_table_buffer.as_entire_binding(),
                    },
                ],
            }));
            self.bind_group_key = Some(key);
        }
        GpuF32CameraDisplayBufferHandles {
            output_buffer: self.output_buffer.clone(),
            params_u32_buffer: self.params_u32_buffer.clone(),
            params_f32_buffer: self.params_f32_buffer.clone(),
            bind_group: self
                .bind_group
                .as_ref()
                .expect("float32 display camera bind group exists")
                .clone(),
            output_bytes: self.output_bytes,
        }
    }

    fn resident_bytes(&self) -> u64 {
        self.output_bytes
            .saturating_add(self.params_u32_bytes)
            .saturating_add(self.params_f32_bytes)
    }
}

impl GpuDisplayChannelCompositeBuffers {
    fn new(device: &wgpu::Device, params_u32_bytes: u64, params_f32_bytes: u64) -> Self {
        let params_u32_buffer = storage_copy_dst_buffer(
            device,
            "mirante4d-display-channel-params-u32",
            params_u32_bytes,
        );
        let params_f32_buffer = storage_copy_dst_buffer(
            device,
            "mirante4d-display-channel-params-f32",
            params_f32_bytes,
        );
        Self {
            params_u32_buffer,
            params_f32_buffer,
            params_u32_bytes,
            params_f32_bytes,
        }
    }

    fn matches(&self, params_u32_bytes: u64, params_f32_bytes: u64) -> bool {
        self.params_u32_bytes == params_u32_bytes && self.params_f32_bytes == params_f32_bytes
    }

    fn handles(&self) -> GpuDisplayChannelCompositeBufferHandles {
        GpuDisplayChannelCompositeBufferHandles {
            params_u32_buffer: self.params_u32_buffer.clone(),
            params_f32_buffer: self.params_f32_buffer.clone(),
        }
    }

    fn resident_bytes(&self) -> u64 {
        self.params_u32_bytes.saturating_add(self.params_f32_bytes)
    }
}

impl GpuIsoMultiChannelDisplayBuffers {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        display_view: &wgpu::TextureView,
        sizes: GpuIsoMultiChannelDisplayBufferSizes,
    ) -> Self {
        let combined_output_buffer = storage_copy_dst_buffer(
            device,
            "mirante4d-display-iso-multi-channel-output",
            sizes.output_bytes,
        );
        let channel_params_u32_buffer = storage_copy_dst_buffer(
            device,
            "mirante4d-display-iso-channel-params-u32",
            sizes.channel_params_u32_bytes,
        );
        let channel_params_f32_buffer = storage_copy_dst_buffer(
            device,
            "mirante4d-display-iso-channel-params-f32",
            sizes.channel_params_f32_bytes,
        );
        let frame_params_u32_buffer = storage_copy_dst_buffer(
            device,
            "mirante4d-display-iso-frame-params-u32",
            sizes.frame_params_u32_bytes,
        );
        let frame_params_f32_buffer = storage_copy_dst_buffer(
            device,
            "mirante4d-display-iso-frame-params-f32",
            sizes.frame_params_f32_bytes,
        );
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mirante4d-display-iso-multi-channel-bind-group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: combined_output_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: channel_params_u32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: channel_params_f32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: frame_params_u32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: frame_params_f32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(display_view),
                },
            ],
        });
        Self {
            combined_output_buffer,
            channel_params_u32_buffer,
            channel_params_f32_buffer,
            frame_params_u32_buffer,
            frame_params_f32_buffer,
            bind_group,
            output_bytes: sizes.output_bytes,
            channel_params_u32_bytes: sizes.channel_params_u32_bytes,
            channel_params_f32_bytes: sizes.channel_params_f32_bytes,
            frame_params_u32_bytes: sizes.frame_params_u32_bytes,
            frame_params_f32_bytes: sizes.frame_params_f32_bytes,
        }
    }

    fn matches(&self, sizes: GpuIsoMultiChannelDisplayBufferSizes) -> bool {
        self.output_bytes == sizes.output_bytes
            && self.channel_params_u32_bytes == sizes.channel_params_u32_bytes
            && self.channel_params_f32_bytes == sizes.channel_params_f32_bytes
            && self.frame_params_u32_bytes == sizes.frame_params_u32_bytes
            && self.frame_params_f32_bytes == sizes.frame_params_f32_bytes
    }

    fn handles(&self) -> GpuIsoMultiChannelDisplayBufferHandles {
        GpuIsoMultiChannelDisplayBufferHandles {
            combined_output_buffer: self.combined_output_buffer.clone(),
            channel_params_u32_buffer: self.channel_params_u32_buffer.clone(),
            channel_params_f32_buffer: self.channel_params_f32_buffer.clone(),
            frame_params_u32_buffer: self.frame_params_u32_buffer.clone(),
            frame_params_f32_buffer: self.frame_params_f32_buffer.clone(),
            bind_group: self.bind_group.clone(),
        }
    }

    fn resident_bytes(&self) -> u64 {
        self.output_bytes
            .saturating_add(self.channel_params_u32_bytes)
            .saturating_add(self.channel_params_f32_bytes)
            .saturating_add(self.frame_params_u32_bytes)
            .saturating_add(self.frame_params_f32_bytes)
    }
}

impl GpuDvrMultiChannelDisplayBuffers {
    #[allow(clippy::too_many_arguments)]
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        display_view: &wgpu::TextureView,
        packed_values_bytes: u64,
        validity_bytes: u64,
        f32_values_bytes: u64,
        page_table_bytes: u64,
        metadata_bytes: u64,
        channel_params_u32_bytes: u64,
        channel_params_f32_bytes: u64,
        frame_params_u32_bytes: u64,
        frame_params_f32_bytes: u64,
    ) -> Self {
        let packed_values_buffer = storage_copy_dst_buffer(
            device,
            "mirante4d-display-dvr-packed-values",
            packed_values_bytes,
        );
        let validity_buffer =
            storage_copy_dst_buffer(device, "mirante4d-display-dvr-validity", validity_bytes);
        let f32_values_buffer =
            storage_copy_dst_buffer(device, "mirante4d-display-dvr-f32-values", f32_values_bytes);
        let page_table_buffer = storage_copy_dst_buffer(
            device,
            "mirante4d-display-dvr-page-tables",
            page_table_bytes,
        );
        let metadata_buffer =
            storage_copy_dst_buffer(device, "mirante4d-display-dvr-metadata", metadata_bytes);
        let channel_params_u32_buffer = storage_copy_dst_buffer(
            device,
            "mirante4d-display-dvr-channel-params-u32",
            channel_params_u32_bytes,
        );
        let channel_params_f32_buffer = storage_copy_dst_buffer(
            device,
            "mirante4d-display-dvr-channel-params-f32",
            channel_params_f32_bytes,
        );
        let frame_params_u32_buffer = uniform_copy_dst_buffer(
            device,
            "mirante4d-display-dvr-frame-params-u32",
            frame_params_u32_bytes,
        );
        let frame_params_f32_buffer = uniform_copy_dst_buffer(
            device,
            "mirante4d-display-dvr-frame-params-f32",
            frame_params_f32_bytes,
        );
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mirante4d-display-dvr-multi-channel-bind-group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: packed_values_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: validity_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: page_table_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: metadata_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: f32_values_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: channel_params_u32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: channel_params_f32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: frame_params_u32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: frame_params_f32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: wgpu::BindingResource::TextureView(display_view),
                },
            ],
        });
        Self {
            packed_values_buffer,
            validity_buffer,
            f32_values_buffer,
            page_table_buffer,
            metadata_buffer,
            channel_params_u32_buffer,
            channel_params_f32_buffer,
            frame_params_u32_buffer,
            frame_params_f32_buffer,
            bind_group,
            packed_values_bytes,
            validity_bytes,
            f32_values_bytes,
            page_table_bytes,
            metadata_bytes,
            channel_params_u32_bytes,
            channel_params_f32_bytes,
            frame_params_u32_bytes,
            frame_params_f32_bytes,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn matches(
        &self,
        packed_values_bytes: u64,
        validity_bytes: u64,
        f32_values_bytes: u64,
        page_table_bytes: u64,
        metadata_bytes: u64,
        channel_params_u32_bytes: u64,
        channel_params_f32_bytes: u64,
        frame_params_u32_bytes: u64,
        frame_params_f32_bytes: u64,
    ) -> bool {
        self.packed_values_bytes == packed_values_bytes
            && self.validity_bytes == validity_bytes
            && self.f32_values_bytes == f32_values_bytes
            && self.page_table_bytes == page_table_bytes
            && self.metadata_bytes == metadata_bytes
            && self.channel_params_u32_bytes == channel_params_u32_bytes
            && self.channel_params_f32_bytes == channel_params_f32_bytes
            && self.frame_params_u32_bytes == frame_params_u32_bytes
            && self.frame_params_f32_bytes == frame_params_f32_bytes
    }

    fn handles(&self) -> GpuDvrMultiChannelDisplayBufferHandles {
        GpuDvrMultiChannelDisplayBufferHandles {
            packed_values_buffer: self.packed_values_buffer.clone(),
            validity_buffer: self.validity_buffer.clone(),
            f32_values_buffer: self.f32_values_buffer.clone(),
            page_table_buffer: self.page_table_buffer.clone(),
            metadata_buffer: self.metadata_buffer.clone(),
            channel_params_u32_buffer: self.channel_params_u32_buffer.clone(),
            channel_params_f32_buffer: self.channel_params_f32_buffer.clone(),
            frame_params_u32_buffer: self.frame_params_u32_buffer.clone(),
            frame_params_f32_buffer: self.frame_params_f32_buffer.clone(),
            bind_group: self.bind_group.clone(),
        }
    }

    fn resident_bytes(&self) -> u64 {
        self.packed_values_bytes
            .saturating_add(self.validity_bytes)
            .saturating_add(self.f32_values_bytes)
            .saturating_add(self.page_table_bytes)
            .saturating_add(self.metadata_bytes)
            .saturating_add(self.channel_params_u32_bytes)
            .saturating_add(self.channel_params_f32_bytes)
            .saturating_add(self.frame_params_u32_bytes)
            .saturating_add(self.frame_params_f32_bytes)
    }
}

impl GpuSceneOverlayDisplayBuffers {
    fn new(
        device: &wgpu::Device,
        params_u32_bytes: u64,
        command_u32_bytes: u64,
        command_f32_bytes: u64,
    ) -> Self {
        let params_u32_buffer = storage_copy_dst_buffer(
            device,
            "mirante4d-scene-render-texture-params-u32",
            params_u32_bytes,
        );
        let command_u32_buffer = storage_copy_dst_buffer(
            device,
            "mirante4d-scene-render-texture-commands-u32",
            command_u32_bytes,
        );
        let command_f32_buffer = storage_copy_dst_buffer(
            device,
            "mirante4d-scene-render-texture-commands-f32",
            command_f32_bytes,
        );
        Self {
            params_u32_buffer,
            command_u32_buffer,
            command_f32_buffer,
            params_u32_bytes,
            command_u32_bytes,
            command_f32_bytes,
        }
    }

    fn matches(
        &self,
        params_u32_bytes: u64,
        command_u32_bytes: u64,
        command_f32_bytes: u64,
    ) -> bool {
        self.params_u32_bytes == params_u32_bytes
            && self.command_u32_bytes == command_u32_bytes
            && self.command_f32_bytes == command_f32_bytes
    }

    fn handles(&self) -> GpuSceneOverlayDisplayBufferHandles {
        GpuSceneOverlayDisplayBufferHandles {
            params_u32_buffer: self.params_u32_buffer.clone(),
            command_u32_buffer: self.command_u32_buffer.clone(),
            command_f32_buffer: self.command_f32_buffer.clone(),
        }
    }

    fn resident_bytes(&self) -> u64 {
        self.params_u32_bytes
            .saturating_add(self.command_u32_bytes)
            .saturating_add(self.command_f32_bytes)
    }
}

impl GpuRenderer {
    pub(super) fn display_resources_for_viewport(
        &self,
        viewport: RenderViewport,
        viewport_width: u32,
        viewport_height: u32,
        accumulator_bytes: u64,
        texture_bytes: u64,
    ) -> Result<GpuDisplayResourceHandles, GpuRenderError> {
        let key = GpuDisplayResourceKey {
            viewport,
            format: wgpu::TextureFormat::Rgba8Unorm,
        };
        let mut cache = self
            .display_resources
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?;
        let cached_handles = cache
            .resources
            .as_ref()
            .filter(|resources| {
                display_resource_allocation_compatible(
                    resources.key,
                    resources.accumulator_bytes,
                    resources.texture_bytes,
                    key,
                    accumulator_bytes,
                    texture_bytes,
                )
            })
            .map(GpuDisplayResources::handles);
        if let Some(handles) = cached_handles {
            cache.stats.cache_hits = cache.stats.cache_hits.saturating_add(1);
            return Ok(handles);
        }
        if cache.resources.is_some() {
            cache.stats.recreations = cache.stats.recreations.saturating_add(1);
        } else {
            cache.stats.cache_misses = cache.stats.cache_misses.saturating_add(1);
        }

        let accumulator = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-display-rgba-accumulator"),
            size: accumulator_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mirante4d-display-rgba-texture"),
            size: wgpu::Extent3d {
                width: viewport_width,
                height: viewport_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let scene_overlay_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mirante4d-display-scene-overlay-rgba-texture"),
            size: wgpu::Extent3d {
                width: viewport_width,
                height: viewport_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let scene_overlay_view =
            scene_overlay_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let final_params = [viewport_width, viewport_height];
        let final_params_buffer =
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("mirante4d-display-final-params-u32"),
                    contents: bytemuck::cast_slice(&final_params),
                    usage: wgpu::BufferUsages::STORAGE,
                });
        let final_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mirante4d-display-final-bind-group"),
            layout: &self.display_finalize_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: accumulator.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: final_params_buffer.as_entire_binding(),
                },
            ],
        });
        let resources = GpuDisplayResources {
            key,
            accumulator,
            texture,
            view,
            scene_overlay_texture,
            scene_overlay_view,
            _final_params_buffer: final_params_buffer,
            final_bind_group,
            accumulator_bytes,
            texture_bytes,
            scene_overlay_texture_bytes: texture_bytes,
            integer_camera_outputs: Vec::new(),
            f32_camera_outputs: Vec::new(),
            channel_composites: Vec::new(),
            iso_multi_channel: None,
            dvr_multi_channel: None,
            scene_overlay: None,
        };
        cache.stats.resident_bytes = resources.resident_bytes();
        let handles = resources.handles();
        cache.resources = Some(resources);
        Ok(handles)
    }

    pub(super) fn integer_camera_display_output_resources(
        &self,
        viewport: RenderViewport,
        atlas: &GpuBrickAtlasResource,
        slot: usize,
        spec: GpuIntegerCameraDisplayBufferSpec,
    ) -> Result<GpuIntegerCameraDisplayBufferHandles, GpuRenderError> {
        let key = GpuDisplayResourceKey {
            viewport,
            format: wgpu::TextureFormat::Rgba8Unorm,
        };
        let mut cache = self
            .display_resources
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?;
        let resources = cache
            .resources
            .as_mut()
            .filter(|resources| resources.key == key)
            .ok_or(RenderError::InvalidChannelComposite(
                "GPU display camera output resources require initialized display viewport resources",
            ))?;
        let handles = resources.integer_camera_output_handles(
            &self.device,
            &self.bricked_camera_bind_group_layout,
            atlas,
            slot,
            spec,
            GpuCameraDisplayBindingContract::Standard,
        );
        cache.stats.resident_bytes = resources.resident_bytes();
        Ok(handles)
    }

    pub(super) fn integer_camera_chunk_display_output_resources(
        &self,
        viewport: RenderViewport,
        atlas: &GpuBrickAtlasResource,
        slot: usize,
        spec: GpuIntegerCameraDisplayBufferSpec,
    ) -> Result<GpuIntegerCameraDisplayBufferHandles, GpuRenderError> {
        let key = GpuDisplayResourceKey {
            viewport,
            format: wgpu::TextureFormat::Rgba8Unorm,
        };
        let mut cache = self
            .display_resources
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?;
        let resources = cache
            .resources
            .as_mut()
            .filter(|resources| resources.key == key)
            .ok_or(RenderError::InvalidChannelComposite(
                "GPU display camera output resources require initialized display viewport resources",
            ))?;
        let handles = resources.integer_camera_output_handles(
            &self.device,
            &self.cross_section_integer_chunk_display_bind_group_layout,
            atlas,
            slot,
            spec,
            GpuCameraDisplayBindingContract::IntegerChunkDrawList,
        );
        cache.stats.resident_bytes = resources.resident_bytes();
        Ok(handles)
    }

    pub(super) fn f32_camera_display_output_resources(
        &self,
        viewport: RenderViewport,
        atlas: &GpuBrickAtlasF32Resource,
        slot: usize,
        spec: GpuF32CameraDisplayBufferSpec,
    ) -> Result<GpuF32CameraDisplayBufferHandles, GpuRenderError> {
        let key = GpuDisplayResourceKey {
            viewport,
            format: wgpu::TextureFormat::Rgba8Unorm,
        };
        let mut cache = self
            .display_resources
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?;
        let resources = cache
            .resources
            .as_mut()
            .filter(|resources| resources.key == key)
            .ok_or(RenderError::InvalidChannelComposite(
                "GPU display camera output resources require initialized display viewport resources",
            ))?;
        let handles = resources.f32_camera_output_handles(
            &self.device,
            &self.bricked_camera_f32_bind_group_layout,
            atlas,
            slot,
            spec,
        );
        cache.stats.resident_bytes = resources.resident_bytes();
        Ok(handles)
    }

    pub(super) fn display_channel_composite_resources(
        &self,
        viewport: RenderViewport,
        slot: usize,
        params_u32_bytes: u64,
        params_f32_bytes: u64,
    ) -> Result<GpuDisplayChannelCompositeBufferHandles, GpuRenderError> {
        let key = GpuDisplayResourceKey {
            viewport,
            format: wgpu::TextureFormat::Rgba8Unorm,
        };
        let mut cache = self
            .display_resources
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?;
        let resources = cache
            .resources
            .as_mut()
            .filter(|resources| resources.key == key)
            .ok_or(RenderError::InvalidChannelComposite(
                "GPU display composite resources require initialized display viewport resources",
            ))?;
        let handles = resources.channel_composite_handles(
            &self.device,
            slot,
            params_u32_bytes,
            params_f32_bytes,
        );
        cache.stats.resident_bytes = resources.resident_bytes();
        Ok(handles)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn iso_multi_channel_display_resources(
        &self,
        viewport: RenderViewport,
        output_bytes: u64,
        channel_params_u32_bytes: u64,
        channel_params_f32_bytes: u64,
        frame_params_u32_bytes: u64,
        frame_params_f32_bytes: u64,
    ) -> Result<GpuIsoMultiChannelDisplayBufferHandles, GpuRenderError> {
        let key = GpuDisplayResourceKey {
            viewport,
            format: wgpu::TextureFormat::Rgba8Unorm,
        };
        let mut cache = self
            .display_resources
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?;
        let resources = cache
            .resources
            .as_mut()
            .filter(|resources| resources.key == key)
            .ok_or(RenderError::InvalidChannelComposite(
                "GPU display ISO resources require initialized display viewport resources",
            ))?;
        let handles = resources.iso_multi_channel_handles(
            &self.device,
            &self.display_iso_multi_channel_bind_group_layout,
            GpuIsoMultiChannelDisplayBufferSizes {
                output_bytes,
                channel_params_u32_bytes,
                channel_params_f32_bytes,
                frame_params_u32_bytes,
                frame_params_f32_bytes,
            },
        );
        cache.stats.resident_bytes = resources.resident_bytes();
        Ok(handles)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn dvr_multi_channel_display_resources(
        &self,
        viewport: RenderViewport,
        packed_values_bytes: u64,
        validity_bytes: u64,
        f32_values_bytes: u64,
        page_table_bytes: u64,
        metadata_bytes: u64,
        channel_params_u32_bytes: u64,
        channel_params_f32_bytes: u64,
        frame_params_u32_bytes: u64,
        frame_params_f32_bytes: u64,
    ) -> Result<GpuDvrMultiChannelDisplayBufferHandles, GpuRenderError> {
        let key = GpuDisplayResourceKey {
            viewport,
            format: wgpu::TextureFormat::Rgba8Unorm,
        };
        let mut cache = self
            .display_resources
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?;
        let resources = cache
            .resources
            .as_mut()
            .filter(|resources| resources.key == key)
            .ok_or(RenderError::InvalidChannelComposite(
                "GPU display DVR resources require initialized display viewport resources",
            ))?;
        let handles = resources.dvr_multi_channel_handles(
            &self.device,
            &self.display_dvr_multi_channel_bind_group_layout,
            packed_values_bytes,
            validity_bytes,
            f32_values_bytes,
            page_table_bytes,
            metadata_bytes,
            channel_params_u32_bytes,
            channel_params_f32_bytes,
            frame_params_u32_bytes,
            frame_params_f32_bytes,
        );
        cache.stats.resident_bytes = resources.resident_bytes();
        Ok(handles)
    }

    pub(super) fn scene_overlay_display_resources(
        &self,
        viewport: RenderViewport,
        params_u32_bytes: u64,
        command_u32_bytes: u64,
        command_f32_bytes: u64,
    ) -> Result<GpuSceneOverlayDisplayBufferHandles, GpuRenderError> {
        let key = GpuDisplayResourceKey {
            viewport,
            format: wgpu::TextureFormat::Rgba8Unorm,
        };
        let mut cache = self
            .display_resources
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?;
        let resources = cache
            .resources
            .as_mut()
            .filter(|resources| resources.key == key)
            .ok_or(RenderError::InvalidChannelComposite(
                "GPU scene overlay resources require initialized display viewport resources",
            ))?;
        let handles = resources.scene_overlay_handles(
            &self.device,
            params_u32_bytes,
            command_u32_bytes,
            command_f32_bytes,
        );
        cache.stats.resident_bytes = resources.resident_bytes();
        Ok(handles)
    }
}
