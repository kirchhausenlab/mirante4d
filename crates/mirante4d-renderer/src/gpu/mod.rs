use std::{borrow::Cow, sync::Mutex, time::Duration};

use std::collections::HashSet;

use mirante4d_data::SpatialBrickIndex;
use mirante4d_domain::Shape3D;
use thiserror::Error;

use crate::{
    BrickFrameDiagnostics, BrickFrameDiagnosticsF32, FrameDiagnostics, FrameDiagnosticsF32,
    MipImageF32, MipImageU16, RenderError,
};

mod adapter;
mod atlas;
mod buffers;
mod cross_section;
mod decode;
mod dense_camera;
mod display;
mod display_resources;
mod params;
mod readback;
mod resident_f32;
mod resident_u16;
mod scene;
mod shaders;
mod summary;
mod volume_cache;
mod z_mip;

pub use adapter::{
    AdapterDiagnostics, GPU_ADAPTER_ENV, GPU_TIMESTAMPS_ENV, GpuLimitDiagnostics,
    REQUIRED_MAX_BUFFER_SIZE, REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE,
    REQUIRED_MAX_STORAGE_BUFFERS_PER_SHADER_STAGE, adapter_info_matches_name, adapter_info_summary,
    adapter_preference_score, renderer_device_descriptor, renderer_required_limits_for_adapter,
};
pub use atlas::GpuBrickAtlasPagePriority;
pub use cross_section::{GpuCrossSectionChunkDisplayChannel, GpuCrossSectionChunkDraw};
pub use display::{
    GpuDisplayFrame, GpuDisplayFrameBlendMode, GpuDisplayFrameDiagnostics,
    GpuResidentDisplayChannel, GpuResidentDisplayRequest,
};
pub use scene::{GpuScenePickOutput, GpuSceneRenderOutput};
pub use summary::{GpuIntensitySummaryF32, GpuIntensitySummaryU16};
pub use z_mip::{
    render_camera_mip_wgpu_blocking, render_camera_wgpu_blocking, render_mip_z_wgpu,
    render_mip_z_wgpu_blocking,
};

use adapter::{diagnostics_for_existing_device, request_device, timestamp_queries_requested};
use atlas::{GpuBrickAtlasCache, GpuBrickAtlasF32Cache};
#[cfg(test)]
use atlas::{build_gpu_brick_atlas, build_gpu_brick_atlas_u8, pack_brick_f32_for_slot};
use buffers::{storage_entry, storage_texture_entry, texture_entry, uniform_entry};
use display_resources::GpuDisplayResourceCache;
use shaders::{
    BRICKED_CAMERA_F32_SHADER, CAMERA_MIP_SHADER, CROSS_SECTION_CHUNK_DISPLAY_F32_SHADER,
    CROSS_SECTION_CHUNK_DISPLAY_INTEGER_SHADER, DISPLAY_COMPOSITE_F32_SHADER,
    DISPLAY_COMPOSITE_SHADER, DISPLAY_DVR_MULTI_CHANNEL_SHADER, DISPLAY_FRAME_BLEND_SHADER,
    DISPLAY_ISO_MULTI_CHANNEL_SHADER, INTENSITY_SUMMARY_F32_SHADER, INTENSITY_SUMMARY_SHADER,
    SCENE_PICK_SHADER, SCENE_RENDER_SHADER, SCENE_RENDER_TEXTURE_SHADER,
    bricked_camera_shader_source,
};
use volume_cache::{DEFAULT_GPU_VOLUME_CACHE_BYTES, GpuVolumeCache};

const WORKGROUP_SIZE_X: u32 = 8;
const WORKGROUP_SIZE_Y: u32 = 8;
const DEFAULT_GPU_BRICK_ATLAS_CACHE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct GpuMipOutput {
    pub image: MipImageU16,
    pub frame: FrameDiagnostics,
    pub brick_frame: Option<BrickFrameDiagnostics>,
    pub timings: Option<GpuRenderTimings>,
    pub adapter: AdapterDiagnostics,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GpuMipOutputF32 {
    pub image: MipImageF32,
    pub frame: FrameDiagnosticsF32,
    pub brick_frame: Option<BrickFrameDiagnosticsF32>,
    pub timings: Option<GpuRenderTimings>,
    pub adapter: AdapterDiagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuResidentBrickGeometry {
    pub brick_shape: Shape3D,
    pub brick_grid_shape: Shape3D,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GpuRenderTimings {
    pub upload_ns: u64,
    pub gpu_compute_ns: Option<u64>,
}

impl GpuRenderTimings {
    pub fn upload_ms(self) -> f64 {
        self.upload_ns as f64 / 1_000_000.0
    }

    pub fn gpu_compute_ms(self) -> Option<f64> {
        self.gpu_compute_ns.map(|ns| ns as f64 / 1_000_000.0)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GpuRendererStats {
    pub volume_cache_budget_bytes: u64,
    pub brick_atlas_cache_budget_bytes: u64,
    pub volume_cache_hits: u64,
    pub volume_cache_misses: u64,
    pub volume_uploads: u64,
    pub volume_uploaded_bytes: u64,
    pub volume_evictions: u64,
    pub volume_resident_bytes: u64,
    pub brick_atlas_cache_hits: u64,
    pub brick_atlas_cache_misses: u64,
    pub brick_atlas_uploads: u64,
    pub brick_atlas_uploaded_bytes: u64,
    pub brick_atlas_u8_uploaded_bytes: u64,
    pub brick_atlas_u16_uploaded_bytes: u64,
    pub brick_atlas_f32_uploaded_bytes: u64,
    pub brick_atlas_evictions: u64,
    pub brick_atlas_page_table_rebuilds: u64,
    pub brick_atlas_page_table_bytes_written: u64,
    pub brick_atlas_resident_bytes: u64,
    pub brick_atlas_u8_resident_bytes: u64,
    pub brick_atlas_u16_resident_bytes: u64,
    pub brick_atlas_f32_resident_bytes: u64,
    pub upload_ready_brick_cache_budget_bytes: u64,
    pub upload_ready_brick_cache_hits: u64,
    pub upload_ready_brick_cache_misses: u64,
    pub upload_ready_brick_cache_evictions: u64,
    pub upload_ready_brick_cache_resident_bytes: u64,
    pub display_resource_cache_hits: u64,
    pub display_resource_cache_misses: u64,
    pub display_resource_recreations: u64,
    pub display_resource_resident_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuBrickAtlasResidencySnapshot {
    pub retained: bool,
    pub generation: Option<u64>,
    pub resident_pages: HashSet<SpatialBrickIndex>,
    pub active_pages: HashSet<SpatialBrickIndex>,
    pub bytes: u64,
    pub slot_count: usize,
}

pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter: AdapterDiagnostics,
    timestamp_queries_enabled: bool,
    camera_mip_bind_group_layout: wgpu::BindGroupLayout,
    camera_mip_pipeline: wgpu::ComputePipeline,
    bricked_camera_bind_group_layout: wgpu::BindGroupLayout,
    bricked_camera_pipeline: wgpu::ComputePipeline,
    cross_section_integer_chunk_display_bind_group_layout: wgpu::BindGroupLayout,
    cross_section_integer_chunk_display_pipeline: wgpu::ComputePipeline,
    cross_section_f32_chunk_display_bind_group_layout: wgpu::BindGroupLayout,
    cross_section_f32_chunk_display_pipeline: wgpu::ComputePipeline,
    bricked_camera_f32_bind_group_layout: wgpu::BindGroupLayout,
    bricked_camera_f32_pipeline: wgpu::ComputePipeline,
    scene_render_bind_group_layout: wgpu::BindGroupLayout,
    scene_render_pipeline: wgpu::ComputePipeline,
    scene_render_texture_bind_group_layout: wgpu::BindGroupLayout,
    scene_render_texture_pipeline: wgpu::ComputePipeline,
    scene_pick_bind_group_layout: wgpu::BindGroupLayout,
    scene_pick_pipeline: wgpu::ComputePipeline,
    intensity_summary_bind_group_layout: wgpu::BindGroupLayout,
    intensity_summary_pipeline: wgpu::ComputePipeline,
    intensity_summary_f32_pipeline: wgpu::ComputePipeline,
    display_composite_bind_group_layout: wgpu::BindGroupLayout,
    display_composite_pipeline: wgpu::ComputePipeline,
    display_composite_f32_pipeline: wgpu::ComputePipeline,
    display_iso_multi_channel_bind_group_layout: wgpu::BindGroupLayout,
    display_iso_multi_channel_pipeline: wgpu::ComputePipeline,
    display_dvr_multi_channel_bind_group_layout: wgpu::BindGroupLayout,
    display_dvr_multi_channel_pipeline: wgpu::ComputePipeline,
    display_frame_blend_bind_group_layout: wgpu::BindGroupLayout,
    display_frame_blend_pipeline: wgpu::ComputePipeline,
    display_finalize_bind_group_layout: wgpu::BindGroupLayout,
    display_finalize_pipeline: wgpu::ComputePipeline,
    volume_cache: Mutex<GpuVolumeCache>,
    brick_atlas_cache: Mutex<GpuBrickAtlasCache>,
    brick_atlas_f32_cache: Mutex<GpuBrickAtlasF32Cache>,
    display_resources: Mutex<GpuDisplayResourceCache>,
}

#[derive(Debug, Error)]
pub enum GpuRenderError {
    #[error(transparent)]
    Render(#[from] RenderError),
    #[error("no compatible GPU adapter was found: {0}")]
    AdapterUnavailable(String),
    #[error("selected adapter is CPU/software only, not an acceptable renderer adapter: {0}")]
    CpuAdapterOnly(String),
    #[error(
        "selected adapter supports {supported} for {limit}, below required renderer limit {required}"
    )]
    RequiredLimitUnsupported {
        limit: &'static str,
        required: u64,
        supported: u64,
    },
    #[error(
        "GPU device was created with {actual} for {limit}, below required renderer limit {required}"
    )]
    DeviceLimitTooLow {
        limit: &'static str,
        required: u64,
        actual: u64,
    },
    #[error("failed to request GPU device: {0}")]
    RequestDevice(String),
    #[error("GPU buffer map failed: {0}")]
    MapFailed(String),
    #[error("GPU poll failed: {0}")]
    PollFailed(String),
    #[error("GPU readback callback did not return a result")]
    ReadbackChannelClosed,
    #[error("GPU camera renderer does not support {0} mode yet")]
    UnsupportedCameraMode(&'static str),
    #[error(
        "GPU {resource} buffer requires {required_bytes} bytes, exceeding device limit {limit_bytes} bytes"
    )]
    BufferTooLarge {
        resource: &'static str,
        required_bytes: u64,
        limit_bytes: u64,
    },
    #[error(
        "GPU {resource} requires {required_bytes} bytes, exceeding renderer budget {budget_bytes} bytes"
    )]
    BudgetExceeded {
        resource: &'static str,
        required_bytes: u64,
        budget_bytes: u64,
    },
    #[error("GPU {resource} buffer byte size overflowed")]
    BufferSizeOverflow { resource: &'static str },
    #[error("GPU renderer cache lock is poisoned")]
    CachePoisoned,
}

impl GpuRenderer {
    pub fn new_blocking() -> Result<Self, GpuRenderError> {
        pollster::block_on(Self::new())
    }

    pub fn new_with_volume_cache_budget_blocking(
        max_cache_bytes: u64,
    ) -> Result<Self, GpuRenderError> {
        pollster::block_on(Self::new_with_volume_cache_budget(max_cache_bytes))
    }

    pub async fn new() -> Result<Self, GpuRenderError> {
        Self::new_with_cache_budgets(
            DEFAULT_GPU_VOLUME_CACHE_BYTES,
            DEFAULT_GPU_BRICK_ATLAS_CACHE_BYTES,
        )
        .await
    }

    pub async fn new_with_volume_cache_budget(
        max_cache_bytes: u64,
    ) -> Result<Self, GpuRenderError> {
        Self::new_with_cache_budgets(max_cache_bytes, DEFAULT_GPU_BRICK_ATLAS_CACHE_BYTES).await
    }

    pub fn new_with_cache_budgets_blocking(
        max_volume_cache_bytes: u64,
        max_brick_atlas_cache_bytes: u64,
    ) -> Result<Self, GpuRenderError> {
        pollster::block_on(Self::new_with_cache_budgets(
            max_volume_cache_bytes,
            max_brick_atlas_cache_bytes,
        ))
    }

    pub async fn new_with_cache_budgets(
        max_volume_cache_bytes: u64,
        max_brick_atlas_cache_bytes: u64,
    ) -> Result<Self, GpuRenderError> {
        let (device, queue, adapter) = request_device("mirante4d-render-device").await?;
        Self::from_device_parts(
            device,
            queue,
            adapter,
            max_volume_cache_bytes,
            max_brick_atlas_cache_bytes,
        )
    }

    pub fn from_existing_device_with_cache_budgets(
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        max_volume_cache_bytes: u64,
        max_brick_atlas_cache_bytes: u64,
    ) -> Result<Self, GpuRenderError> {
        let adapter = diagnostics_for_existing_device(adapter, &device)?;
        Self::from_device_parts(
            device,
            queue,
            adapter,
            max_volume_cache_bytes,
            max_brick_atlas_cache_bytes,
        )
    }

    fn from_device_parts(
        device: wgpu::Device,
        queue: wgpu::Queue,
        adapter: AdapterDiagnostics,
        max_volume_cache_bytes: u64,
        max_brick_atlas_cache_bytes: u64,
    ) -> Result<Self, GpuRenderError> {
        let timestamp_features =
            wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES;
        let timestamp_queries_enabled =
            timestamp_queries_requested() && device.features().contains(timestamp_features);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mirante4d-camera-mip-wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(CAMERA_MIP_SHADER)),
        });
        let bricked_shader_source = bricked_camera_shader_source();
        let bricked_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mirante4d-bricked-camera-wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Owned(bricked_shader_source)),
        });
        let cross_section_integer_chunk_display_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("mirante4d-cross-section-integer-chunk-display-wgsl"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(
                    CROSS_SECTION_CHUNK_DISPLAY_INTEGER_SHADER,
                )),
            });
        let cross_section_f32_chunk_display_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("mirante4d-cross-section-f32-chunk-display-wgsl"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(
                    CROSS_SECTION_CHUNK_DISPLAY_F32_SHADER,
                )),
            });
        let bricked_f32_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mirante4d-bricked-camera-f32-wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(BRICKED_CAMERA_F32_SHADER)),
        });
        let scene_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mirante4d-scene-render-wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SCENE_RENDER_SHADER)),
        });
        let scene_texture_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mirante4d-scene-render-texture-wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SCENE_RENDER_TEXTURE_SHADER)),
        });
        let scene_pick_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mirante4d-scene-pick-wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SCENE_PICK_SHADER)),
        });
        let intensity_summary_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mirante4d-intensity-summary-wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(INTENSITY_SUMMARY_SHADER)),
        });
        let intensity_summary_f32_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("mirante4d-intensity-summary-f32-wgsl"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(INTENSITY_SUMMARY_F32_SHADER)),
            });
        let display_composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mirante4d-display-composite-wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(DISPLAY_COMPOSITE_SHADER)),
        });
        let display_composite_f32_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("mirante4d-display-composite-f32-wgsl"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(DISPLAY_COMPOSITE_F32_SHADER)),
            });
        let display_iso_multi_channel_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("mirante4d-display-iso-multi-channel-wgsl"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(DISPLAY_ISO_MULTI_CHANNEL_SHADER)),
            });
        let display_dvr_multi_channel_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("mirante4d-display-dvr-multi-channel-wgsl"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(DISPLAY_DVR_MULTI_CHANNEL_SHADER)),
            });
        let display_frame_blend_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("mirante4d-display-frame-blend-wgsl"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(DISPLAY_FRAME_BLEND_SHADER)),
            });
        let camera_mip_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mirante4d-camera-mip-bind-group-layout"),
                entries: &[
                    storage_entry(0, true),
                    storage_entry(1, false),
                    storage_entry(2, true),
                    storage_entry(3, true),
                ],
            });
        let bricked_camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mirante4d-bricked-camera-bind-group-layout"),
                entries: &[
                    storage_entry(0, true),
                    storage_entry(1, false),
                    storage_entry(2, true),
                    storage_entry(3, true),
                    storage_entry(4, true),
                    storage_entry(5, true),
                    storage_entry(6, true),
                    storage_entry(7, false),
                ],
            });
        let cross_section_integer_chunk_display_bind_group_layout = device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mirante4d-cross-section-integer-chunk-display-bind-group-layout"),
                entries: &[
                    storage_entry(0, true),
                    storage_entry(1, false),
                    storage_entry(2, true),
                    storage_entry(3, true),
                    storage_entry(4, true),
                    storage_entry(5, true),
                    storage_entry(6, true),
                    storage_entry(7, true),
                ],
            });
        let bricked_camera_f32_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mirante4d-bricked-camera-f32-bind-group-layout"),
                entries: &[
                    storage_entry(0, true),
                    storage_entry(1, false),
                    storage_entry(2, true),
                    storage_entry(3, true),
                    storage_entry(4, true),
                ],
            });
        let cross_section_f32_chunk_display_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mirante4d-cross-section-f32-chunk-display-bind-group-layout"),
                entries: &[
                    storage_entry(0, true),
                    storage_entry(1, false),
                    storage_entry(2, true),
                    storage_entry(3, true),
                    storage_entry(4, true),
                    storage_entry(5, true),
                ],
            });
        let scene_render_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mirante4d-scene-render-bind-group-layout"),
                entries: &[
                    storage_entry(0, true),
                    storage_entry(1, false),
                    storage_entry(2, true),
                    storage_entry(3, true),
                    storage_entry(4, true),
                ],
            });
        let scene_render_texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mirante4d-scene-render-texture-bind-group-layout"),
                entries: &[
                    texture_entry(0),
                    storage_texture_entry(1),
                    storage_entry(2, true),
                    storage_entry(3, true),
                    storage_entry(4, true),
                ],
            });
        let scene_pick_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mirante4d-scene-pick-bind-group-layout"),
                entries: &[
                    storage_entry(0, true),
                    storage_entry(1, true),
                    storage_entry(2, true),
                    storage_entry(3, true),
                    storage_entry(4, false),
                ],
            });
        let intensity_summary_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mirante4d-intensity-summary-bind-group-layout"),
                entries: &[
                    storage_entry(0, true),
                    storage_entry(1, false),
                    storage_entry(2, true),
                ],
            });
        let display_composite_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mirante4d-display-composite-bind-group-layout"),
                entries: &[
                    storage_entry(0, true),
                    storage_entry(1, false),
                    storage_entry(2, true),
                    storage_entry(3, true),
                ],
            });
        let display_iso_multi_channel_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mirante4d-display-iso-multi-channel-bind-group-layout"),
                entries: &[
                    storage_entry(0, true),
                    storage_entry(1, true),
                    storage_entry(2, true),
                    storage_entry(3, true),
                    storage_entry(4, true),
                    storage_texture_entry(5),
                ],
            });
        let display_dvr_multi_channel_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mirante4d-display-dvr-multi-channel-bind-group-layout"),
                entries: &[
                    storage_entry(0, true),
                    storage_entry(1, true),
                    storage_entry(2, true),
                    storage_entry(3, true),
                    storage_entry(4, true),
                    storage_entry(5, true),
                    storage_entry(6, true),
                    uniform_entry(7),
                    uniform_entry(8),
                    storage_texture_entry(9),
                ],
            });
        let display_frame_blend_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mirante4d-display-frame-blend-bind-group-layout"),
                entries: &[
                    texture_entry(0),
                    texture_entry(1),
                    storage_texture_entry(2),
                    storage_entry(3, true),
                ],
            });
        let display_finalize_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mirante4d-display-finalize-bind-group-layout"),
                entries: &[
                    storage_entry(0, true),
                    storage_texture_entry(1),
                    storage_entry(2, true),
                ],
            });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mirante4d-camera-mip-pipeline-layout"),
            bind_group_layouts: &[Some(&camera_mip_bind_group_layout)],
            immediate_size: 0,
        });
        let bricked_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mirante4d-bricked-camera-pipeline-layout"),
                bind_group_layouts: &[Some(&bricked_camera_bind_group_layout)],
                immediate_size: 0,
            });
        let bricked_f32_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mirante4d-bricked-camera-f32-pipeline-layout"),
                bind_group_layouts: &[Some(&bricked_camera_f32_bind_group_layout)],
                immediate_size: 0,
            });
        let cross_section_integer_chunk_display_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mirante4d-cross-section-integer-chunk-display-pipeline-layout"),
                bind_group_layouts: &[Some(&cross_section_integer_chunk_display_bind_group_layout)],
                immediate_size: 0,
            });
        let cross_section_f32_chunk_display_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mirante4d-cross-section-f32-chunk-display-pipeline-layout"),
                bind_group_layouts: &[Some(&cross_section_f32_chunk_display_bind_group_layout)],
                immediate_size: 0,
            });
        let scene_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mirante4d-scene-render-pipeline-layout"),
                bind_group_layouts: &[Some(&scene_render_bind_group_layout)],
                immediate_size: 0,
            });
        let scene_texture_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mirante4d-scene-render-texture-pipeline-layout"),
                bind_group_layouts: &[Some(&scene_render_texture_bind_group_layout)],
                immediate_size: 0,
            });
        let scene_pick_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mirante4d-scene-pick-pipeline-layout"),
                bind_group_layouts: &[Some(&scene_pick_bind_group_layout)],
                immediate_size: 0,
            });
        let intensity_summary_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mirante4d-intensity-summary-pipeline-layout"),
                bind_group_layouts: &[Some(&intensity_summary_bind_group_layout)],
                immediate_size: 0,
            });
        let display_composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mirante4d-display-composite-pipeline-layout"),
                bind_group_layouts: &[Some(&display_composite_bind_group_layout)],
                immediate_size: 0,
            });
        let display_iso_multi_channel_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mirante4d-display-iso-multi-channel-pipeline-layout"),
                bind_group_layouts: &[Some(&display_iso_multi_channel_bind_group_layout)],
                immediate_size: 0,
            });
        let display_dvr_multi_channel_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mirante4d-display-dvr-multi-channel-pipeline-layout"),
                bind_group_layouts: &[Some(&display_dvr_multi_channel_bind_group_layout)],
                immediate_size: 0,
            });
        let display_frame_blend_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mirante4d-display-frame-blend-pipeline-layout"),
                bind_group_layouts: &[Some(&display_frame_blend_bind_group_layout)],
                immediate_size: 0,
            });
        let display_finalize_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mirante4d-display-finalize-pipeline-layout"),
                bind_group_layouts: &[Some(&display_finalize_bind_group_layout)],
                immediate_size: 0,
            });
        let camera_mip_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("mirante4d-camera-mip-compute-pipeline"),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some("camera_mip_main"),
                compilation_options: Default::default(),
                cache: None,
            });
        let bricked_camera_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("mirante4d-bricked-camera-compute-pipeline"),
                layout: Some(&bricked_pipeline_layout),
                module: &bricked_shader,
                entry_point: Some("camera_mip_main"),
                compilation_options: Default::default(),
                cache: None,
            });
        let cross_section_integer_chunk_display_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("mirante4d-cross-section-integer-chunk-display-compute-pipeline"),
                layout: Some(&cross_section_integer_chunk_display_pipeline_layout),
                module: &cross_section_integer_chunk_display_shader,
                entry_point: Some("cross_section_chunk_display_integer_main"),
                compilation_options: Default::default(),
                cache: None,
            });
        let bricked_camera_f32_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("mirante4d-bricked-camera-f32-compute-pipeline"),
                layout: Some(&bricked_f32_pipeline_layout),
                module: &bricked_f32_shader,
                entry_point: Some("camera_f32_main"),
                compilation_options: Default::default(),
                cache: None,
            });
        let cross_section_f32_chunk_display_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("mirante4d-cross-section-f32-chunk-display-compute-pipeline"),
                layout: Some(&cross_section_f32_chunk_display_pipeline_layout),
                module: &cross_section_f32_chunk_display_shader,
                entry_point: Some("cross_section_chunk_display_f32_main"),
                compilation_options: Default::default(),
                cache: None,
            });
        let scene_render_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("mirante4d-scene-render-compute-pipeline"),
                layout: Some(&scene_pipeline_layout),
                module: &scene_shader,
                entry_point: Some("scene_render_main"),
                compilation_options: Default::default(),
                cache: None,
            });
        let scene_render_texture_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("mirante4d-scene-render-texture-compute-pipeline"),
                layout: Some(&scene_texture_pipeline_layout),
                module: &scene_texture_shader,
                entry_point: Some("scene_render_texture_main"),
                compilation_options: Default::default(),
                cache: None,
            });
        let scene_pick_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("mirante4d-scene-pick-compute-pipeline"),
                layout: Some(&scene_pick_pipeline_layout),
                module: &scene_pick_shader,
                entry_point: Some("scene_pick_main"),
                compilation_options: Default::default(),
                cache: None,
            });
        let intensity_summary_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("mirante4d-intensity-summary-compute-pipeline"),
                layout: Some(&intensity_summary_pipeline_layout),
                module: &intensity_summary_shader,
                entry_point: Some("intensity_summary_main"),
                compilation_options: Default::default(),
                cache: None,
            });
        let intensity_summary_f32_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("mirante4d-intensity-summary-f32-compute-pipeline"),
                layout: Some(&intensity_summary_pipeline_layout),
                module: &intensity_summary_f32_shader,
                entry_point: Some("intensity_summary_f32_main"),
                compilation_options: Default::default(),
                cache: None,
            });
        let display_composite_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("mirante4d-display-composite-compute-pipeline"),
                layout: Some(&display_composite_pipeline_layout),
                module: &display_composite_shader,
                entry_point: Some("display_composite_channel_main"),
                compilation_options: Default::default(),
                cache: None,
            });
        let display_composite_f32_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("mirante4d-display-composite-f32-compute-pipeline"),
                layout: Some(&display_composite_pipeline_layout),
                module: &display_composite_f32_shader,
                entry_point: Some("display_composite_f32_channel_main"),
                compilation_options: Default::default(),
                cache: None,
            });
        let display_iso_multi_channel_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("mirante4d-display-iso-multi-channel-compute-pipeline"),
                layout: Some(&display_iso_multi_channel_pipeline_layout),
                module: &display_iso_multi_channel_shader,
                entry_point: Some("display_iso_multi_channel_main"),
                compilation_options: Default::default(),
                cache: None,
            });
        let display_dvr_multi_channel_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("mirante4d-display-dvr-multi-channel-compute-pipeline"),
                layout: Some(&display_dvr_multi_channel_pipeline_layout),
                module: &display_dvr_multi_channel_shader,
                entry_point: Some("display_dvr_multi_channel_main"),
                compilation_options: Default::default(),
                cache: None,
            });
        let display_frame_blend_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("mirante4d-display-frame-blend-compute-pipeline"),
                layout: Some(&display_frame_blend_pipeline_layout),
                module: &display_frame_blend_shader,
                entry_point: Some("display_frame_blend_main"),
                compilation_options: Default::default(),
                cache: None,
            });
        let display_finalize_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("mirante4d-display-finalize-compute-pipeline"),
                layout: Some(&display_finalize_pipeline_layout),
                module: &display_composite_shader,
                entry_point: Some("display_finalize_main"),
                compilation_options: Default::default(),
                cache: None,
            });
        Ok(Self {
            device,
            queue,
            adapter,
            timestamp_queries_enabled,
            camera_mip_bind_group_layout,
            camera_mip_pipeline,
            bricked_camera_bind_group_layout,
            bricked_camera_pipeline,
            cross_section_integer_chunk_display_bind_group_layout,
            cross_section_integer_chunk_display_pipeline,
            cross_section_f32_chunk_display_bind_group_layout,
            cross_section_f32_chunk_display_pipeline,
            bricked_camera_f32_bind_group_layout,
            bricked_camera_f32_pipeline,
            scene_render_bind_group_layout,
            scene_render_pipeline,
            scene_render_texture_bind_group_layout,
            scene_render_texture_pipeline,
            scene_pick_bind_group_layout,
            scene_pick_pipeline,
            intensity_summary_bind_group_layout,
            intensity_summary_pipeline,
            intensity_summary_f32_pipeline,
            display_composite_bind_group_layout,
            display_composite_pipeline,
            display_composite_f32_pipeline,
            display_iso_multi_channel_bind_group_layout,
            display_iso_multi_channel_pipeline,
            display_dvr_multi_channel_bind_group_layout,
            display_dvr_multi_channel_pipeline,
            display_frame_blend_bind_group_layout,
            display_frame_blend_pipeline,
            display_finalize_bind_group_layout,
            display_finalize_pipeline,
            volume_cache: Mutex::new(GpuVolumeCache::new(max_volume_cache_bytes)),
            brick_atlas_cache: Mutex::new(GpuBrickAtlasCache::new(max_brick_atlas_cache_bytes)),
            brick_atlas_f32_cache: Mutex::new(GpuBrickAtlasF32Cache::new(
                max_brick_atlas_cache_bytes,
            )),
            display_resources: Mutex::new(GpuDisplayResourceCache::default()),
        })
    }

    pub fn adapter_diagnostics(&self) -> &AdapterDiagnostics {
        &self.adapter
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn stats(&self) -> Result<GpuRendererStats, GpuRenderError> {
        let (volume_budget, volume_stats) = self
            .volume_cache
            .lock()
            .map(|cache| (cache.max_bytes, cache.stats))
            .map_err(|_| GpuRenderError::CachePoisoned)?;
        let (brick_budget, brick_stats) = self
            .brick_atlas_cache
            .lock()
            .map(|cache| (cache.max_bytes, cache.stats))
            .map_err(|_| GpuRenderError::CachePoisoned)?;
        let brick_f32_stats = self
            .brick_atlas_f32_cache
            .lock()
            .map(|cache| cache.stats)
            .map_err(|_| GpuRenderError::CachePoisoned)?;
        let display_stats = self
            .display_resources
            .lock()
            .map(|cache| cache.stats())
            .map_err(|_| GpuRenderError::CachePoisoned)?;
        Ok(GpuRendererStats {
            volume_cache_budget_bytes: volume_budget,
            brick_atlas_cache_budget_bytes: brick_budget,
            volume_cache_hits: volume_stats.volume_cache_hits,
            volume_cache_misses: volume_stats.volume_cache_misses,
            volume_uploads: volume_stats.volume_uploads,
            volume_uploaded_bytes: volume_stats.volume_uploaded_bytes,
            volume_evictions: volume_stats.volume_evictions,
            volume_resident_bytes: volume_stats.volume_resident_bytes,
            brick_atlas_cache_hits: brick_stats.brick_atlas_cache_hits
                + brick_f32_stats.brick_atlas_cache_hits,
            brick_atlas_cache_misses: brick_stats.brick_atlas_cache_misses
                + brick_f32_stats.brick_atlas_cache_misses,
            brick_atlas_uploads: brick_stats.brick_atlas_uploads
                + brick_f32_stats.brick_atlas_uploads,
            brick_atlas_uploaded_bytes: brick_stats.brick_atlas_uploaded_bytes
                + brick_f32_stats.brick_atlas_uploaded_bytes,
            brick_atlas_u8_uploaded_bytes: brick_stats.brick_atlas_u8_uploaded_bytes,
            brick_atlas_u16_uploaded_bytes: brick_stats.brick_atlas_u16_uploaded_bytes,
            brick_atlas_f32_uploaded_bytes: brick_f32_stats.brick_atlas_f32_uploaded_bytes,
            brick_atlas_evictions: brick_stats.brick_atlas_evictions
                + brick_f32_stats.brick_atlas_evictions,
            brick_atlas_page_table_rebuilds: brick_stats.brick_atlas_page_table_rebuilds
                + brick_f32_stats.brick_atlas_page_table_rebuilds,
            brick_atlas_page_table_bytes_written: brick_stats.brick_atlas_page_table_bytes_written
                + brick_f32_stats.brick_atlas_page_table_bytes_written,
            brick_atlas_resident_bytes: brick_stats.brick_atlas_resident_bytes
                + brick_f32_stats.brick_atlas_resident_bytes,
            brick_atlas_u8_resident_bytes: brick_stats.brick_atlas_u8_resident_bytes,
            brick_atlas_u16_resident_bytes: brick_stats.brick_atlas_u16_resident_bytes,
            brick_atlas_f32_resident_bytes: brick_f32_stats.brick_atlas_f32_resident_bytes,
            upload_ready_brick_cache_budget_bytes: brick_stats
                .upload_ready_brick_cache_budget_bytes,
            upload_ready_brick_cache_hits: brick_stats.upload_ready_brick_cache_hits,
            upload_ready_brick_cache_misses: brick_stats.upload_ready_brick_cache_misses,
            upload_ready_brick_cache_evictions: brick_stats.upload_ready_brick_cache_evictions,
            upload_ready_brick_cache_resident_bytes: brick_stats
                .upload_ready_brick_cache_resident_bytes,
            display_resource_cache_hits: display_stats.cache_hits,
            display_resource_cache_misses: display_stats.cache_misses,
            display_resource_recreations: display_stats.recreations,
            display_resource_resident_bytes: display_stats.resident_bytes,
        })
    }
}

fn duration_ns_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn add_gpu_render_timings(left: GpuRenderTimings, right: GpuRenderTimings) -> GpuRenderTimings {
    let gpu_compute_ns = match (left.gpu_compute_ns, right.gpu_compute_ns) {
        (Some(left), Some(right)) => Some(left.saturating_add(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
    GpuRenderTimings {
        upload_ns: left.upload_ns.saturating_add(right.upload_ns),
        gpu_compute_ns,
    }
}

#[cfg(test)]
mod display_tests;
#[cfg(test)]
mod scene_tests;
#[cfg(test)]
mod shader_contract_tests;
#[cfg(test)]
mod summary_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
