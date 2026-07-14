#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU64,
    ops::Range,
    sync::{Arc, Mutex},
};

use mirante4d_dataset::{DatasetCatalog, DatasetResourceKey, ResourceLease};
use mirante4d_render_api::{
    CameraFrame, FrameCompleteness, FrameCoverage, FrameIdentity, FrameLimitation, FrameProgress,
    GpuLedgerCategory, PresentationRegistration, PresentationRetirement, PresentationToken,
    PresentedFrame, RenderExtent, RenderIntent, RenderRequirement, RenderRequirementRole,
    RenderRequirements, RenderViewIntent,
};

use super::{
    FrameExecutionReport, MAX_CONTROL_UPLOAD_BYTES, MAX_PAYLOAD_UPLOAD_BYTES, MAX_UPLOADS,
    MAX_VISITS, ValidationCapture, ValidationCaptureTicket, WgpuRenderRuntimeConfig,
    WgpuRenderRuntimeDiagnostics, WgpuRenderRuntimeError,
};

const MAX_WIDTH: u32 = 1_920;
const MAX_HEIGHT: u32 = 1_080;
// The inherited render API permits larger generic plans, but this deliberately
// small successor slice admits only bounded view-local metadata. The 256-entry
// input ceiling preserves the accepted 129-resource traversal fixture. The
// resident metadata table has the same fixed ceiling, while each traversal,
// supplied-lease window, and submitted shader table stays at 128 resources.
const MAX_FRAME_REQUIREMENTS: usize = 256;
const MAX_FRAME_LEASES: usize = MAX_VISITS;
const MAX_RESIDENT_RESOURCES: usize = MAX_FRAME_REQUIREMENTS;
const MAX_SHADER_RESOURCES: usize = MAX_VISITS;
const MAX_PRESENTATION_TARGETS: usize = 4;
const MAX_RAY_SAMPLES: u64 = 16_384;
const MIN_BUFFER_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
const MIN_STORAGE_BINDING_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
const MIN_STORAGE_BUFFERS_PER_STAGE: u32 = 8;
const CONTROL_BUFFER_BYTES: u64 = 64 * 1024;
const COPY_ALIGNMENT: u64 = wgpu::COPY_BUFFER_ALIGNMENT;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const FACT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg8Uint;
const COLOR_BYTES_PER_PIXEL: u32 = 4;
const FACT_BYTES_PER_PIXEL: u32 = 2;
const HEADER_WORDS: usize = 32;
const LAYER_WORDS: usize = 32;
const RESOURCE_WORDS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrameState {
    frame: FrameIdentity,
    requirements: Vec<DatasetResourceKey>,
    cursor: usize,
}

struct PackedPayload {
    bytes: Vec<u8>,
    validity_offset: Option<u64>,
}

fn pack_payload(
    payload: mirante4d_dataset::ResourcePayloadView<'_>,
) -> Result<PackedPayload, WgpuRenderRuntimeError> {
    // Scientific float validation is deliberately inside the upload path:
    // callers may offer more leases than this frame visits, while uploaded
    // payload bytes are already bounded by MAX_PAYLOAD_UPLOAD_BYTES.
    if payload.dtype().bytes_per_sample() == 4
        && payload.value_bytes().chunks_exact(4).any(|bytes| {
            !f32::from_le_bytes(bytes.try_into().expect("four-byte chunk")).is_finite()
        })
    {
        return Err(WgpuRenderRuntimeError::PayloadContractMismatch);
    }
    let value_len = payload.value_byte_len();
    let validity_offset = payload.validity_bits().map(|_| align_copy(value_len));
    let logical_end = validity_offset
        .unwrap_or(value_len)
        .checked_add(payload.validity_byte_len())
        .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
    let allocation = align_copy(logical_end);
    let allocation =
        usize::try_from(allocation).map_err(|_| WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
    let mut bytes = vec![0_u8; allocation];
    let value_end =
        usize::try_from(value_len).map_err(|_| WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
    bytes[..value_end].copy_from_slice(payload.value_bytes());
    if let (Some(offset), Some(validity)) = (validity_offset, payload.validity_bits()) {
        let start =
            usize::try_from(offset).map_err(|_| WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
        bytes[start..start + validity.len()].copy_from_slice(validity);
    }
    Ok(PackedPayload {
        bytes,
        validity_offset,
    })
}

fn allocate_transactional(
    allocator: &mut ArenaAllocator,
    resident: &mut BTreeMap<DatasetResourceKey, ResidentResource>,
    required: &BTreeSet<DatasetResourceKey>,
    bytes: u64,
) -> Result<u64, WgpuRenderRuntimeError> {
    if bytes > allocator.capacity {
        return Err(WgpuRenderRuntimeError::CapacityExceeded {
            category: GpuLedgerCategory::PayloadResidency,
            requested_bytes: bytes,
            available_bytes: allocator.capacity,
        });
    }
    if resident.len() < MAX_RESIDENT_RESOURCES
        && let Some(offset) = allocator.allocate(bytes)
    {
        return Ok(offset);
    }
    let mut victims = resident
        .iter()
        .filter(|(key, _)| !required.contains(key))
        .map(|(key, value)| (*key, value.last_used_frame))
        .collect::<Vec<_>>();
    victims.sort_by_key(|(key, frame)| (*frame, *key));
    for (key, _) in victims {
        if let Some(resource) = resident.remove(&key) {
            allocator.release(resource.offset, resource.allocated_bytes);
        }
        if resident.len() < MAX_RESIDENT_RESOURCES
            && let Some(offset) = allocator.allocate(bytes)
        {
            return Ok(offset);
        }
    }
    Err(WgpuRenderRuntimeError::CapacityExceeded {
        category: GpuLedgerCategory::PayloadResidency,
        requested_bytes: bytes,
        available_bytes: allocator.available_bytes(),
    })
}

fn build_progress(
    requirements: &RenderRequirements,
    available: &[DatasetResourceKey],
    budget_limited: bool,
    shader_capacity_limited: bool,
) -> Result<Option<FrameProgress>, WgpuRenderRuntimeError> {
    let coverage = FrameCoverage::from_available(requirements, available)
        .map_err(|_| WgpuRenderRuntimeError::FrameProgressContract)?;
    if !coverage.is_first_useful() {
        return Ok(None);
    }
    let (completeness, limitation) = if coverage.is_full() {
        (FrameCompleteness::Exact, None)
    } else if shader_capacity_limited {
        (
            FrameCompleteness::Progressive,
            Some(FrameLimitation::CapacityLimited),
        )
    } else if budget_limited {
        (
            FrameCompleteness::Progressive,
            Some(FrameLimitation::BudgetLimited),
        )
    } else {
        (
            FrameCompleteness::Progressive,
            Some(FrameLimitation::MissingResources),
        )
    };
    FrameProgress::new(coverage, completeness, limitation)
        .map(Some)
        .map_err(|_| WgpuRenderRuntimeError::FrameProgressContract)
}

fn validate_requirement_contract(
    requirements: &RenderRequirements,
) -> Result<(), WgpuRenderRuntimeError> {
    validate_requirement_slice(requirements.resources())
}

fn validate_lease_capacity(lease_count: usize) -> Result<(), WgpuRenderRuntimeError> {
    if lease_count > MAX_FRAME_LEASES {
        return Err(WgpuRenderRuntimeError::LeaseCapacityExceeded {
            actual: lease_count,
            maximum: MAX_FRAME_LEASES,
        });
    }
    Ok(())
}

fn validate_requirement_slice(
    resources: &[RenderRequirement],
) -> Result<(), WgpuRenderRuntimeError> {
    if resources.len() > MAX_FRAME_REQUIREMENTS {
        return Err(WgpuRenderRuntimeError::RequirementCapacityExceeded {
            actual: resources.len(),
            maximum: MAX_FRAME_REQUIREMENTS,
        });
    }

    let mut scale_by_layer = BTreeMap::new();
    for requirement in resources {
        let key = requirement.key();
        if let Some(scale) = scale_by_layer.insert(key.layer(), key.scale())
            && scale != key.scale()
        {
            return Err(WgpuRenderRuntimeError::MixedScaleRequirements);
        }
    }

    // The generic RenderRequirements contract permits arbitrary semantic
    // regions. This successor must reject ambiguous overlap before the shader,
    // matching the independent CPU oracle. The enclosing 256-entry cap makes
    // this pairwise validation a fixed, small metadata bound.
    for (index, first) in resources.iter().enumerate() {
        let first = first.key();
        for second in &resources[index + 1..] {
            let second = second.key();
            if first.layer() == second.layer()
                && first.scale() == second.scale()
                && regions_overlap(first, second)
            {
                return Err(WgpuRenderRuntimeError::OverlappingResources);
            }
        }
    }
    Ok(())
}

fn regions_overlap(first: DatasetResourceKey, second: DatasetResourceKey) -> bool {
    let first_start = first.region().origin();
    let first_end = first.region().end_exclusive();
    let second_start = second.region().origin();
    let second_end = second.region().end_exclusive();
    (0..3).all(|axis| first_start[axis] < second_end[axis] && second_start[axis] < first_end[axis])
}

fn display_allocation_bytes(extent: RenderExtent) -> Result<u64, WgpuRenderRuntimeError> {
    let pixels = u64::from(extent.width_pixels())
        .checked_mul(u64::from(extent.height_pixels()))
        .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
    pixels
        .checked_mul(u64::from(COLOR_BYTES_PER_PIXEL + FACT_BYTES_PER_PIXEL))
        .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)
}

fn capture_layout(extent: RenderExtent) -> Result<(u32, u64, u32, u64), WgpuRenderRuntimeError> {
    let color_unpadded = extent
        .width_pixels()
        .checked_mul(COLOR_BYTES_PER_PIXEL)
        .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
    let color_padded = color_unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let color_bytes = u64::from(color_padded)
        .checked_mul(u64::from(extent.height_pixels()))
        .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
    let fact_unpadded = extent
        .width_pixels()
        .checked_mul(FACT_BYTES_PER_PIXEL)
        .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
    let fact_padded = fact_unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let fact_offset = color_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64)
        * u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let fact_bytes = u64::from(fact_padded)
        .checked_mul(u64::from(extent.height_pixels()))
        .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
    Ok((
        color_padded,
        fact_offset,
        fact_padded,
        fact_offset + fact_bytes,
    ))
}

fn capture_allocation_bytes(extent: RenderExtent) -> Result<u64, WgpuRenderRuntimeError> {
    capture_layout(extent).map(|(_, _, _, total)| total)
}

fn create_display(
    device: &wgpu::Device,
    extent: RenderExtent,
) -> Result<DisplayTarget, WgpuRenderRuntimeError> {
    let allocated_bytes = display_allocation_bytes(extent)?;
    let size = wgpu::Extent3d {
        width: extent.width_pixels(),
        height: extent.height_pixels(),
        depth_or_array_layers: 1,
    };
    let color_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mirante4d-wp09a-color-target"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let color_view = color_texture.create_view(&wgpu::TextureViewDescriptor::default());
    let fact_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mirante4d-wp09a-fact-target"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FACT_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let fact_view = fact_texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(DisplayTarget {
        color_texture,
        color_view,
        fact_texture,
        fact_view,
        extent,
        allocated_bytes,
    })
}

fn mapped_staging_buffer(device: &wgpu::Device, label: &'static str, bytes: &[u8]) -> wgpu::Buffer {
    debug_assert!(!bytes.is_empty() && bytes.len().is_multiple_of(COPY_ALIGNMENT as usize));
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: true,
    });
    buffer
        .slice(..)
        .get_mapped_range_mut()
        .copy_from_slice(bytes);
    buffer.unmap();
    buffer
}

fn encode_render_pass(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
    display: &DisplayTarget,
) {
    let attachments = [
        Some(wgpu::RenderPassColorAttachment {
            view: &display.color_view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        }),
        Some(wgpu::RenderPassColorAttachment {
            view: &display.fact_view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        }),
    ];
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("mirante4d-wp09a-semantic-pass"),
        color_attachments: &attachments,
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[]);
    pass.draw(0..3, 0..1);
}

fn encode_capture(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    id: u64,
    presentation: PresentationToken,
    frame: FrameIdentity,
    display: &DisplayTarget,
) -> Result<PendingCapture, WgpuRenderRuntimeError> {
    let (color_padded_row, fact_offset, fact_padded_row, allocated_bytes) =
        capture_layout(display.extent)?;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mirante4d-wp09a-validation-readback"),
        size: allocated_bytes,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let copy_size = wgpu::Extent3d {
        width: display.extent.width_pixels(),
        height: display.extent.height_pixels(),
        depth_or_array_layers: 1,
    };
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &display.color_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(color_padded_row),
                rows_per_image: Some(display.extent.height_pixels()),
            },
        },
        copy_size,
    );
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &display.fact_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: fact_offset,
                bytes_per_row: Some(fact_padded_row),
                rows_per_image: Some(display.extent.height_pixels()),
            },
        },
        copy_size,
    );
    Ok(PendingCapture {
        ticket: ValidationCaptureTicket {
            id,
            presentation,
            frame,
            extent: display.extent,
        },
        buffer,
        color_offset: 0,
        color_padded_row,
        fact_offset,
        fact_padded_row,
        allocated_bytes,
        state: Arc::new(Mutex::new(None)),
    })
}

fn build_control(
    catalog: &DatasetCatalog,
    intent: &RenderIntent,
    resource_keys: &[DatasetResourceKey],
    resident: &BTreeMap<DatasetResourceKey, ResidentResource>,
) -> Result<Vec<u8>, WgpuRenderRuntimeError> {
    let mut words = vec![0_u32; HEADER_WORDS];
    words[0] = 0x4d34_5739;
    words[2] = u32::try_from(intent.layers().len())
        .map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    words[4] = intent.extent().width_pixels();
    words[5] = intent.extent().height_pixels();
    words[6] =
        u32::try_from(HEADER_WORDS).map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;

    encode_view(&mut words, intent)?;

    let mut selected_scales = BTreeMap::new();
    for key in resource_keys {
        selected_scales
            .entry(key.layer())
            .and_modify(|scale| {
                if key.scale() < *scale {
                    *scale = key.scale();
                }
            })
            .or_insert(key.scale());
    }

    for layer_intent in intent.layers() {
        let layer_key = layer_intent.layer();
        let catalog_layer = catalog
            .layer(layer_key)
            .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
        let base_level = catalog_layer
            .scales()
            .next()
            .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?
            .level();
        let scale_level = selected_scales
            .get(&layer_key)
            .copied()
            .unwrap_or(base_level);
        let scale = catalog_layer
            .scale(scale_level)
            .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
        let transform = scale.grid_to_world().row_major();
        if [1, 2, 4, 6, 8, 9]
            .into_iter()
            .any(|index| transform[index] != 0.0)
            || [transform[0], transform[5], transform[10]]
                .into_iter()
                .any(|value| !value.is_finite() || value == 0.0)
        {
            return Err(WgpuRenderRuntimeError::UnsupportedView);
        }
        if format!("{:?}", layer_intent.render_state().sampling_policy()) != "VoxelExact" {
            return Err(WgpuRenderRuntimeError::UnsupportedSampling);
        }
        let shape = scale.shape().dimensions();
        if words[3] == 0
            && shape
                .into_iter()
                .any(|dimension| dimension > MAX_RAY_SAMPLES)
        {
            return Err(WgpuRenderRuntimeError::RaySampleLimitExceeded);
        }
        let transfer = layer_intent.transfer();
        let state = layer_intent.render_state();
        let (mode, iso_level, dvr_low, dvr_high, dvr_gamma, dvr_density) =
            if state.mip_parameters().is_some() {
                (0_u32, 0.0_f32, 0.0, 1.0, 1.0, 1.0)
            } else if let Some(parameters) = state.dvr_parameters() {
                let opacity = parameters.opacity_transfer();
                (
                    1,
                    0.0,
                    opacity.window().low(),
                    opacity.window().high(),
                    opacity.curve().gamma_value(),
                    f64_to_f32(parameters.density_scale())?,
                )
            } else if let Some(parameters) = state.iso_parameters() {
                if format!("{:?}", parameters.shading_policy()) != "Flat" {
                    return Err(WgpuRenderRuntimeError::UnsupportedIsoShading);
                }
                (2, parameters.display_level(), 0.0, 1.0, 1.0, 1.0)
            } else {
                return Err(WgpuRenderRuntimeError::UnsupportedView);
            };
        let dimensions = [
            u64_to_u32(shape[2])?,
            u64_to_u32(shape[1])?,
            u64_to_u32(shape[0])?,
        ];
        let color = transfer.color().rgb();
        let mut record = [0_u32; LAYER_WORDS];
        record[0] = layer_key.ordinal();
        record[1..4].copy_from_slice(&dimensions);
        record[4] = f64_to_f32(transform[0].recip())?.to_bits();
        record[5] = f64_to_f32(transform[5].recip())?.to_bits();
        record[6] = f64_to_f32(transform[10].recip())?.to_bits();
        record[7] = f64_to_f32(transform[3])?.to_bits();
        record[8] = f64_to_f32(transform[7])?.to_bits();
        record[9] = f64_to_f32(transform[11])?.to_bits();
        record[10] = transfer.window().low().to_bits();
        record[11] = transfer.window().high().to_bits();
        record[12] = color[0].to_bits();
        record[13] = color[1].to_bits();
        record[14] = color[2].to_bits();
        record[15] = transfer.opacity().get().to_bits();
        record[16] = transfer.curve().gamma_value().to_bits();
        record[17] = u32::from(transfer.invert());
        record[18] = mode;
        record[19] = iso_level.to_bits();
        record[20] = dvr_low.to_bits();
        record[21] = dvr_high.to_bits();
        record[22] = dvr_gamma.to_bits();
        record[23] = dvr_density.to_bits();
        record[24] = scale_level.get();
        words.extend_from_slice(&record);
    }

    words[7] =
        u32::try_from(words.len()).map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    // Requirement preflight permits exactly one scale per layer, so every key
    // named in progress is encoded into the submitted control buffer. Never
    // count a cached-but-omitted scale as covered.
    words[1] = u32::try_from(resource_keys.len())
        .map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    for key in resource_keys {
        let resource = resident
            .get(key)
            .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
        let origin = key.region().origin();
        let shape = key.region().shape().dimensions();
        let mut record = [0_u32; RESOURCE_WORDS];
        record[0] = key.layer().ordinal();
        record[1] = u64_to_u32(origin[2])?;
        record[2] = u64_to_u32(origin[1])?;
        record[3] = u64_to_u32(origin[0])?;
        record[4] = u64_to_u32(shape[2])?;
        record[5] = u64_to_u32(shape[1])?;
        record[6] = u64_to_u32(shape[0])?;
        record[7] = u64_to_u32(resource.offset)?;
        record[8] = resource.validity_offset.map_or(Ok(u32::MAX), u64_to_u32)?;
        record[9] = resource.dtype_bytes;
        record[10] = u64_to_u32(resource.value_bytes)?;
        record[11] = key.scale().get();
        words.extend_from_slice(&record);
    }
    let bytes = bytemuck::cast_slice::<u32, u8>(&words).to_vec();
    if bytes.len() as u64 > MAX_CONTROL_UPLOAD_BYTES {
        return Err(WgpuRenderRuntimeError::ControlCapacityExceeded);
    }
    Ok(bytes)
}

fn encode_view(words: &mut [u32], intent: &RenderIntent) -> Result<(), WgpuRenderRuntimeError> {
    match intent.view() {
        RenderViewIntent::Volume { camera, .. } => {
            words[3] = 0;
            let frame = CameraFrame::new(camera, intent.presentation())
                .map_err(|_| WgpuRenderRuntimeError::UnsupportedView)?;
            let ray = frame
                .ray_for_render_pixel(
                    0.0,
                    0.0,
                    intent.extent().width_pixels(),
                    intent.extent().height_pixels(),
                )
                .map_err(|_| WgpuRenderRuntimeError::UnsupportedView)?;
            let ray_x = frame
                .ray_for_render_pixel(
                    1.0,
                    0.0,
                    intent.extent().width_pixels(),
                    intent.extent().height_pixels(),
                )
                .map_err(|_| WgpuRenderRuntimeError::UnsupportedView)?;
            let ray_y = frame
                .ray_for_render_pixel(
                    0.0,
                    1.0,
                    intent.extent().width_pixels(),
                    intent.extent().height_pixels(),
                )
                .map_err(|_| WgpuRenderRuntimeError::UnsupportedView)?;
            if !vectors_close(ray.direction(), ray_x.direction())
                || !vectors_close(ray.direction(), ray_y.direction())
            {
                return Err(WgpuRenderRuntimeError::UnsupportedView);
            }
            let origin = ray.origin().components();
            let origin_x = ray_x.origin().components();
            let origin_y = ray_y.origin().components();
            write_vec3(words, 8, origin)?;
            write_vec3(words, 11, subtract(origin_x, origin))?;
            write_vec3(words, 14, subtract(origin_y, origin))?;
            write_vec3(words, 17, ray.direction())?;
        }
        RenderViewIntent::CrossSection(view) => {
            words[3] = 1;
            let [right, up] = cross_section_axes(view.orientation().xyzw());
            write_vec3(words, 8, view.center_world().components())?;
            write_vec3(words, 11, right)?;
            write_vec3(words, 14, up)?;
            words[17] = f64_to_f32(view.scale_world_per_screen_point())?.to_bits();
            words[18] = f64_to_f32(intent.presentation().width_points())?.to_bits();
            words[19] = f64_to_f32(intent.presentation().height_points())?.to_bits();
        }
    }
    Ok(())
}

fn vectors_close(first: [f64; 3], second: [f64; 3]) -> bool {
    first
        .into_iter()
        .zip(second)
        .all(|(a, b)| (a - b).abs() <= 1.0e-10)
}

fn subtract(first: [f64; 3], second: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|axis| first[axis] - second[axis])
}

fn cross_section_axes(quaternion: [f64; 4]) -> [[f64; 3]; 2] {
    let [x, y, z, w] = quaternion;
    let rotate = |vector: [f64; 3]| {
        let cross = [
            y * vector[2] - z * vector[1],
            z * vector[0] - x * vector[2],
            x * vector[1] - y * vector[0],
        ];
        let twice = cross.map(|value| 2.0 * value);
        let second = [
            y * twice[2] - z * twice[1],
            z * twice[0] - x * twice[2],
            x * twice[1] - y * twice[0],
        ];
        std::array::from_fn(|axis| vector[axis] + w * twice[axis] + second[axis])
    };
    [rotate([1.0, 0.0, 0.0]), rotate([0.0, 1.0, 0.0])]
}

fn write_vec3(
    words: &mut [u32],
    start: usize,
    values: [f64; 3],
) -> Result<(), WgpuRenderRuntimeError> {
    for (index, value) in values.into_iter().enumerate() {
        words[start + index] = f64_to_f32(value)?.to_bits();
    }
    Ok(())
}

fn f64_to_f32(value: f64) -> Result<f32, WgpuRenderRuntimeError> {
    let converted = value as f32;
    if converted.is_finite() {
        Ok(if converted == 0.0 { 0.0 } else { converted })
    } else {
        Err(WgpuRenderRuntimeError::UnsupportedView)
    }
}

fn u64_to_u32(value: u64) -> Result<u32, WgpuRenderRuntimeError> {
    u32::try_from(value).map_err(|_| WgpuRenderRuntimeError::CoordinateLimitExceeded)
}

#[derive(Debug, Clone)]
struct ResidentResource {
    offset: u64,
    allocated_bytes: u64,
    value_bytes: u64,
    validity_offset: Option<u64>,
    dtype_bytes: u32,
    last_used_frame: u64,
}

#[derive(Debug, Clone)]
struct ArenaAllocator {
    capacity: u64,
    free: Vec<Range<u64>>,
}

impl ArenaAllocator {
    fn new(capacity: u64) -> Self {
        Self {
            capacity,
            free: std::iter::once(0..capacity).collect(),
        }
    }

    fn allocate(&mut self, bytes: u64) -> Option<u64> {
        let bytes = align_copy(bytes);
        let index = self
            .free
            .iter()
            .position(|range| range.end.saturating_sub(range.start) >= bytes)?;
        let offset = self.free[index].start;
        self.free[index].start += bytes;
        if self.free[index].is_empty() {
            self.free.remove(index);
        }
        Some(offset)
    }

    fn release(&mut self, offset: u64, bytes: u64) {
        self.free.push(offset..offset.saturating_add(bytes));
        self.free.sort_by_key(|range| range.start);
        let mut merged: Vec<Range<u64>> = Vec::with_capacity(self.free.len());
        for range in self.free.drain(..) {
            if let Some(previous) = merged.last_mut()
                && range.start <= previous.end
            {
                previous.end = previous.end.max(range.end);
            } else {
                merged.push(range);
            }
        }
        self.free = merged;
    }

    fn available_bytes(&self) -> u64 {
        self.free
            .iter()
            .map(|range| range.end.saturating_sub(range.start))
            .sum()
    }
}

struct DisplayTarget {
    color_texture: wgpu::Texture,
    color_view: wgpu::TextureView,
    fact_texture: wgpu::Texture,
    fact_view: wgpu::TextureView,
    extent: RenderExtent,
    allocated_bytes: u64,
}

struct PresentationState {
    frame_state: Option<FrameState>,
    display: DisplayTarget,
    pending_capture: Option<PendingCapture>,
}

type MapState = Arc<Mutex<Option<Result<(), ()>>>>;

struct PendingCapture {
    ticket: ValidationCaptureTicket,
    buffer: wgpu::Buffer,
    color_offset: u64,
    color_padded_row: u32,
    fact_offset: u64,
    fact_padded_row: u32,
    allocated_bytes: u64,
    state: MapState,
}

struct UploadPlan {
    offset: u64,
    bytes: Vec<u8>,
    resident: ResidentResource,
}

impl PendingCapture {
    fn start_map(&self) {
        let state = Arc::clone(&self.state);
        self.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                if let Ok(mut status) = state.lock() {
                    *status = Some(result.map_err(|_| ()));
                }
            });
    }
}

pub(super) struct Runtime {
    _instance: Option<wgpu::Instance>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    arena_buffer: wgpu::Buffer,
    control_buffer: wgpu::Buffer,
    allocator: ArenaAllocator,
    resident: BTreeMap<DatasetResourceKey, ResidentResource>,
    presentations: BTreeMap<PresentationToken, PresentationState>,
    next_presentation: u64,
    next_capture: u64,
    validation_errors: Arc<Mutex<Vec<String>>>,
    config: WgpuRenderRuntimeConfig,
    diagnostics: WgpuRenderRuntimeDiagnostics,
}

impl Runtime {
    pub(super) async fn new(
        config: WgpuRenderRuntimeConfig,
    ) -> Result<Self, WgpuRenderRuntimeError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
            .map_err(|_| WgpuRenderRuntimeError::DeviceUnavailable)?;
        validate_adapter(&adapter)?;

        let required_limits = wgpu::Limits {
            max_buffer_size: MIN_BUFFER_LIMIT_BYTES,
            max_storage_buffer_binding_size: MIN_STORAGE_BINDING_LIMIT_BYTES,
            max_storage_buffers_per_shader_stage: MIN_STORAGE_BUFFERS_PER_STAGE,
            ..wgpu::Limits::default()
        };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("mirante4d-wp09a-device"),
                required_features: wgpu::Features::empty(),
                required_limits,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|_| WgpuRenderRuntimeError::DeviceCreationFailed)?;

        Self::from_device_parts(Some(instance), &adapter, device, queue, config)
    }

    pub(super) fn from_existing_device(
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: WgpuRenderRuntimeConfig,
    ) -> Result<Self, WgpuRenderRuntimeError> {
        validate_adapter(adapter)?;
        validate_device_limits(&device.limits())?;
        Self::from_device_parts(None, adapter, device, queue, config)
    }

    fn from_device_parts(
        instance: Option<wgpu::Instance>,
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: WgpuRenderRuntimeConfig,
    ) -> Result<Self, WgpuRenderRuntimeError> {
        let info = adapter.get_info();
        let adapter_limits = adapter.limits();
        validate_device_limits(&device.limits())?;
        let payload_ledger_bytes = config.gpu_budget_bytes().saturating_mul(75) / 100;
        let transfer_capacity_bytes = config.gpu_budget_bytes().saturating_mul(10) / 100;
        let other_capacity_bytes = config
            .gpu_budget_bytes()
            .saturating_sub(payload_ledger_bytes)
            .saturating_sub(transfer_capacity_bytes);
        let arena_capacity = payload_ledger_bytes
            .min(device.limits().max_storage_buffer_binding_size)
            .min(device.limits().max_buffer_size)
            .min(MIN_STORAGE_BINDING_LIMIT_BYTES)
            / COPY_ALIGNMENT
            * COPY_ALIGNMENT;
        if arena_capacity < COPY_ALIGNMENT {
            return Err(WgpuRenderRuntimeError::InvalidConfiguration);
        }

        let validation_errors = Arc::new(Mutex::new(Vec::new()));
        let error_sink = Arc::clone(&validation_errors);
        device.on_uncaptured_error(Arc::new(move |error| {
            if let Ok(mut errors) = error_sink.lock() {
                errors.push(error.to_string());
            }
        }));

        let arena_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-wp09a-payload-arena"),
            size: arena_capacity,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let control_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-wp09a-control"),
            size: CONTROL_BUFFER_BYTES,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mirante4d-wp09a-bind-group-layout"),
            entries: &[
                storage_layout_entry(0, CONTROL_BUFFER_BYTES),
                storage_layout_entry(1, arena_capacity),
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mirante4d-wp09a-bind-group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: control_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: arena_buffer.as_entire_binding(),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mirante4d-wp09a-semantic-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mirante4d-wp09a-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mirante4d-wp09a-semantic-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[
                    Some(wgpu::ColorTargetState {
                        format: COLOR_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                    Some(wgpu::ColorTargetState {
                        format: FACT_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                ],
            }),
            multiview_mask: None,
            cache: None,
        });
        let driver = if info.driver_info.trim().is_empty() {
            info.driver.clone()
        } else {
            info.driver_info.clone()
        };

        Ok(Self {
            _instance: instance,
            device,
            queue,
            pipeline,
            bind_group,
            arena_buffer,
            control_buffer,
            allocator: ArenaAllocator::new(arena_capacity),
            resident: BTreeMap::new(),
            presentations: BTreeMap::new(),
            next_presentation: 1,
            next_capture: 1,
            validation_errors,
            config,
            diagnostics: WgpuRenderRuntimeDiagnostics {
                adapter_name: info.name,
                backend: format!("{:?}", info.backend),
                driver,
                max_buffer_size_bytes: adapter_limits.max_buffer_size,
                max_storage_buffer_binding_size_bytes: adapter_limits
                    .max_storage_buffer_binding_size,
                max_storage_buffers_per_shader_stage: adapter_limits
                    .max_storage_buffers_per_shader_stage,
                gpu_budget_bytes: config.gpu_budget_bytes(),
                payload_capacity_bytes: payload_ledger_bytes,
                transfer_capacity_bytes,
                other_capacity_bytes,
                payload_arena_allocated_bytes: arena_capacity,
                resident_payload_used_bytes: 0,
                peak_resident_payload_used_bytes: 0,
                peak_transfer_bytes: 0,
                peak_display_target_bytes: 0,
                peak_page_table_bytes: CONTROL_BUFFER_BYTES,
                peak_scratch_bytes: 0,
                frames_executed: 0,
                queue_submissions: 0,
                validation_error_count: 0,
            },
        })
    }

    pub(super) const fn diagnostics(&self) -> &WgpuRenderRuntimeDiagnostics {
        &self.diagnostics
    }

    pub(super) fn register_presentation(
        &mut self,
        extent: RenderExtent,
    ) -> Result<PresentationRegistration, WgpuRenderRuntimeError> {
        validate_extent(extent)?;
        validate_presentation_capacity(self.presentations.len())?;
        let token = PresentationToken::new(self.next_presentation)
            .map_err(|_| WgpuRenderRuntimeError::PresentationTokenExhausted)?;
        let next_presentation = self
            .next_presentation
            .checked_add(1)
            .ok_or(WgpuRenderRuntimeError::PresentationTokenExhausted)?;
        let display_bytes = self
            .active_display_bytes()
            .checked_add(display_allocation_bytes(extent)?)
            .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
        self.validate_other_capacity(display_bytes, self.active_capture_bytes())?;
        let display = create_display(&self.device, extent)?;
        self.presentations.insert(
            token,
            PresentationState {
                frame_state: None,
                display,
                pending_capture: None,
            },
        );
        self.next_presentation = next_presentation;
        self.diagnostics.peak_display_target_bytes = self
            .diagnostics
            .peak_display_target_bytes
            .max(display_bytes);
        Ok(PresentationRegistration::new(token, extent))
    }

    pub(super) fn presentation_texture_view(
        &self,
        token: PresentationToken,
    ) -> Result<&wgpu::TextureView, WgpuRenderRuntimeError> {
        self.presentations
            .get(&token)
            .map(|presentation| &presentation.display.color_view)
            .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered { token })
    }

    pub(super) fn retire_presentation(
        &mut self,
        token: PresentationToken,
    ) -> Result<PresentationRetirement, WgpuRenderRuntimeError> {
        self.presentations
            .remove(&token)
            .map(|_| PresentationRetirement::new(token))
            .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered { token })
    }

    fn active_display_bytes(&self) -> u64 {
        self.presentations
            .values()
            .map(|presentation| presentation.display.allocated_bytes)
            .sum()
    }

    fn active_capture_bytes(&self) -> u64 {
        self.presentations
            .values()
            .filter_map(|presentation| presentation.pending_capture.as_ref())
            .map(|capture| capture.allocated_bytes)
            .sum()
    }

    fn validate_other_capacity(
        &self,
        display_bytes: u64,
        capture_bytes: u64,
    ) -> Result<(), WgpuRenderRuntimeError> {
        if CONTROL_BUFFER_BYTES > self.diagnostics.other_capacity_bytes {
            return Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::PageTable,
                requested_bytes: CONTROL_BUFFER_BYTES,
                available_bytes: self.diagnostics.other_capacity_bytes,
            });
        }
        let after_control = self
            .diagnostics
            .other_capacity_bytes
            .saturating_sub(CONTROL_BUFFER_BYTES);
        if display_bytes > after_control {
            return Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::DisplayTarget,
                requested_bytes: display_bytes,
                available_bytes: after_control,
            });
        }
        let after_display = after_control.saturating_sub(display_bytes);
        if capture_bytes > after_display {
            return Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::Scratch,
                requested_bytes: capture_bytes,
                available_bytes: after_display,
            });
        }
        Ok(())
    }

    pub(super) fn execute_frame(
        &mut self,
        presentation_token: PresentationToken,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        leases: &[&dyn ResourceLease],
    ) -> Result<FrameExecutionReport, WgpuRenderRuntimeError> {
        let current_frame_state = self
            .presentations
            .get(&presentation_token)
            .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered {
                token: presentation_token,
            })?
            .frame_state
            .clone();
        let lease_by_key = self.validate_inputs(
            current_frame_state.as_ref(),
            catalog,
            intent,
            requirements,
            leases,
        )?;
        let (planned_frame, visited) =
            Self::plan_frame(current_frame_state.as_ref(), requirements)?;
        let frame_changed = current_frame_state
            .as_ref()
            .is_none_or(|current| current.frame != planned_frame.frame);

        let required = planned_frame
            .requirements
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let mut planned_allocator = self.allocator.clone();
        let mut planned_resident = self.resident.clone();
        let mut uploads = Vec::new();
        let mut raw_upload_bytes = 0_u64;
        let mut budget_limited = planned_frame.requirements.len() > visited.len();

        for key in &visited {
            if planned_resident.contains_key(key) {
                continue;
            }
            let Some(lease) = lease_by_key.get(key).copied() else {
                continue;
            };
            let payload = lease.payload();
            let raw_bytes = payload.byte_len();
            if uploads.len() == MAX_UPLOADS
                || raw_upload_bytes
                    .checked_add(raw_bytes)
                    .is_none_or(|total| total > MAX_PAYLOAD_UPLOAD_BYTES)
            {
                budget_limited = true;
                continue;
            }
            let packed = pack_payload(payload)?;
            let allocated_bytes = u64::try_from(packed.bytes.len())
                .map_err(|_| WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
            let offset = allocate_transactional(
                &mut planned_allocator,
                &mut planned_resident,
                &required,
                allocated_bytes,
            )?;
            let resident = ResidentResource {
                offset,
                allocated_bytes,
                value_bytes: payload.value_byte_len(),
                validity_offset: packed.validity_offset.map(|relative| offset + relative),
                dtype_bytes: u32::from(payload.dtype().bytes_per_sample()),
                last_used_frame: intent.frame().get(),
            };
            planned_resident.insert(*key, resident.clone());
            raw_upload_bytes += raw_bytes;
            uploads.push(UploadPlan {
                offset,
                bytes: packed.bytes,
                resident,
            });
        }
        for key in &planned_frame.requirements {
            if let Some(resource) = planned_resident.get_mut(key) {
                resource.last_used_frame = intent.frame().get();
            }
        }

        let available = planned_frame
            .requirements
            .iter()
            .copied()
            .filter(|key| planned_resident.contains_key(key))
            .collect::<Vec<_>>();
        let shader_keys = requirements
            .resources()
            .iter()
            .filter(|requirement| requirement.role() == RenderRequirementRole::FirstUsefulFrame)
            .chain(
                requirements
                    .resources()
                    .iter()
                    .filter(|requirement| requirement.role() == RenderRequirementRole::Refinement),
            )
            .map(|requirement| requirement.key())
            .filter(|key| planned_resident.contains_key(key))
            .take(MAX_SHADER_RESOURCES)
            .collect::<Vec<_>>();
        let shader_capacity_limited = shader_keys.len() != available.len();
        let progress = build_progress(
            requirements,
            &shader_keys,
            budget_limited,
            shader_capacity_limited,
        )?;

        let control = if progress.is_some() {
            build_control(catalog, intent, &shader_keys, &planned_resident)?
        } else {
            Vec::new()
        };
        let control_bytes = u64::try_from(control.len())
            .map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;
        if control_bytes > MAX_CONTROL_UPLOAD_BYTES {
            return Err(WgpuRenderRuntimeError::ControlCapacityExceeded);
        }
        let transfer_bytes = uploads
            .iter()
            .map(|upload| u64::try_from(upload.bytes.len()).unwrap_or(u64::MAX))
            .sum::<u64>()
            .saturating_add(control_bytes);
        if transfer_bytes > self.diagnostics.transfer_capacity_bytes {
            return Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::TransferStaging,
                requested_bytes: transfer_bytes,
                available_bytes: self.diagnostics.transfer_capacity_bytes,
            });
        }

        let render = progress.is_some();
        let capture_bytes = if render && self.config.validation_capture() {
            capture_allocation_bytes(intent.extent())?
        } else {
            0
        };
        let presentation = self
            .presentations
            .get(&presentation_token)
            .expect("presentation registration was checked before frame planning");
        let replaces_display = render && presentation.display.extent != intent.extent();
        if render
            && self.config.validation_capture()
            && presentation.pending_capture.is_some()
            && !frame_changed
        {
            return Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::Scratch,
                requested_bytes: capture_bytes,
                available_bytes: 0,
            });
        }
        let display_peak_bytes = if replaces_display {
            self.active_display_bytes()
                .saturating_add(display_allocation_bytes(intent.extent())?)
        } else {
            self.active_display_bytes()
        };
        let capture_peak_bytes = self.active_capture_bytes().saturating_add(capture_bytes);
        self.validate_other_capacity(display_peak_bytes, capture_peak_bytes)?;

        let needs_submission = !uploads.is_empty() || render;
        let mut command_buffers = 0_u32;
        let mut queue_submissions = 0_u32;
        let mut capture_ticket = None;
        let mut new_display = if replaces_display {
            Some(create_display(&self.device, intent.extent())?)
        } else {
            None
        };
        let mut pending_capture = None;

        if needs_submission {
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mirante4d-wp09a-frame"),
                });
            let mut staging_buffers = Vec::with_capacity(uploads.len() + usize::from(render));
            for upload in &uploads {
                let staging = mapped_staging_buffer(
                    &self.device,
                    "mirante4d-wp09a-payload-staging",
                    &upload.bytes,
                );
                encoder.copy_buffer_to_buffer(
                    &staging,
                    0,
                    &self.arena_buffer,
                    upload.offset,
                    upload.resident.allocated_bytes,
                );
                staging_buffers.push(staging);
            }

            if render {
                let staging = mapped_staging_buffer(
                    &self.device,
                    "mirante4d-wp09a-control-staging",
                    &control,
                );
                encoder.copy_buffer_to_buffer(&staging, 0, &self.control_buffer, 0, control_bytes);
                staging_buffers.push(staging);
                let display = new_display.as_ref().unwrap_or_else(|| {
                    &self
                        .presentations
                        .get(&presentation_token)
                        .expect("presentation registration was checked before submission")
                        .display
                });
                encode_render_pass(&mut encoder, &self.pipeline, &self.bind_group, display);
                if self.config.validation_capture() {
                    let pending = encode_capture(
                        &self.device,
                        &mut encoder,
                        self.next_capture,
                        presentation_token,
                        intent.frame(),
                        display,
                    )?;
                    capture_ticket = Some(pending.ticket);
                    pending_capture = Some(pending);
                }
            }

            self.queue.submit([encoder.finish()]);
            command_buffers = 1;
            queue_submissions = 1;
            if let Some(pending) = pending_capture.as_ref() {
                pending.start_map();
            }
            drop(staging_buffers);
        }

        // Commit only after every capacity/control/view preflight and the one
        // allowed submission have succeeded.
        self.allocator = planned_allocator;
        self.resident = planned_resident;
        let presentation = self
            .presentations
            .get_mut(&presentation_token)
            .expect("presentation registration was checked before commit");
        presentation.frame_state = Some(planned_frame.clone());
        if let Some(display) = new_display.take() {
            presentation.display = display;
        }
        if frame_changed {
            presentation.pending_capture = None;
        }
        if let Some(pending) = pending_capture {
            presentation.pending_capture = Some(pending);
            self.next_capture = self.next_capture.saturating_add(1);
        }
        self.refresh_diagnostics(
            transfer_bytes,
            display_peak_bytes,
            capture_peak_bytes,
            render,
            queue_submissions,
        );

        Ok(FrameExecutionReport {
            presentation: progress
                .clone()
                .map(|progress| PresentedFrame::new(presentation_token, intent.extent(), progress)),
            frame: intent.frame(),
            progress,
            visited_resources: visited.len(),
            uploaded_resources: uploads.len(),
            payload_upload_bytes: raw_upload_bytes,
            control_upload_bytes: control_bytes,
            command_buffers,
            queue_submissions,
            validation_capture: capture_ticket,
        })
    }

    pub(super) fn poll_validation_capture(
        &mut self,
        ticket: ValidationCaptureTicket,
    ) -> Result<Option<ValidationCapture>, WgpuRenderRuntimeError> {
        if self
            .presentations
            .get(&ticket.presentation)
            .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered {
                token: ticket.presentation,
            })?
            .frame_state
            .as_ref()
            .is_some_and(|current| ticket.frame != current.frame)
        {
            return Err(WgpuRenderRuntimeError::StaleValidationCapture);
        }
        self.device
            .poll(wgpu::PollType::Poll)
            .map_err(|_| WgpuRenderRuntimeError::ValidationCaptureFailed)?;
        self.sync_validation_errors();
        if self.diagnostics.validation_error_count != 0 {
            return Err(WgpuRenderRuntimeError::BackendValidation);
        }
        let presentation = self
            .presentations
            .get_mut(&ticket.presentation)
            .expect("presentation registration was checked before capture polling");
        let Some(pending) = presentation.pending_capture.as_ref() else {
            return Err(WgpuRenderRuntimeError::UnknownValidationCapture);
        };
        if pending.ticket != ticket {
            return Err(WgpuRenderRuntimeError::UnknownValidationCapture);
        }
        let status = pending
            .state
            .lock()
            .map_err(|_| WgpuRenderRuntimeError::ValidationCaptureFailed)?
            .to_owned();
        match status {
            None => Ok(None),
            Some(Err(())) => {
                presentation.pending_capture = None;
                Err(WgpuRenderRuntimeError::ValidationCaptureFailed)
            }
            Some(Ok(())) => {
                let pending = presentation
                    .pending_capture
                    .take()
                    .ok_or(WgpuRenderRuntimeError::UnknownValidationCapture)?;
                let mapped = pending.buffer.slice(..).get_mapped_range();
                let width = usize::try_from(ticket.extent.width_pixels())
                    .map_err(|_| WgpuRenderRuntimeError::ValidationCaptureFailed)?;
                let height = usize::try_from(ticket.extent.height_pixels())
                    .map_err(|_| WgpuRenderRuntimeError::ValidationCaptureFailed)?;
                let mut rgba8 = Vec::with_capacity(width.saturating_mul(height).saturating_mul(4));
                let color_start = usize::try_from(pending.color_offset)
                    .map_err(|_| WgpuRenderRuntimeError::ValidationCaptureFailed)?;
                let color_padded = usize::try_from(pending.color_padded_row)
                    .map_err(|_| WgpuRenderRuntimeError::ValidationCaptureFailed)?;
                for row in mapped[color_start..]
                    .chunks_exact(color_padded)
                    .take(height)
                {
                    rgba8.extend_from_slice(&row[..width * 4]);
                }
                let mut coverage = Vec::with_capacity(width.saturating_mul(height));
                let mut validity = Vec::with_capacity(width.saturating_mul(height));
                let fact_start = usize::try_from(pending.fact_offset)
                    .map_err(|_| WgpuRenderRuntimeError::ValidationCaptureFailed)?;
                let fact_padded = usize::try_from(pending.fact_padded_row)
                    .map_err(|_| WgpuRenderRuntimeError::ValidationCaptureFailed)?;
                for row in mapped[fact_start..].chunks_exact(fact_padded).take(height) {
                    for pair in row[..width * 2].chunks_exact(2) {
                        coverage.push(pair[0]);
                        validity.push(pair[1]);
                    }
                }
                drop(mapped);
                pending.buffer.unmap();
                Ok(Some(ValidationCapture {
                    frame: ticket.frame,
                    extent: ticket.extent,
                    rgba8: rgba8.into_boxed_slice(),
                    coverage: coverage.into_boxed_slice(),
                    validity: validity.into_boxed_slice(),
                }))
            }
        }
    }

    fn validate_inputs<'a>(
        &self,
        current_frame_state: Option<&FrameState>,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        leases: &'a [&'a dyn ResourceLease],
    ) -> Result<BTreeMap<DatasetResourceKey, &'a dyn ResourceLease>, WgpuRenderRuntimeError> {
        if intent.frame() != requirements.frame() {
            return Err(WgpuRenderRuntimeError::FrameContractMismatch);
        }
        validate_requirement_contract(requirements)?;
        validate_lease_capacity(leases.len())?;
        validate_extent(intent.extent())?;
        if let Some(current) = current_frame_state
            && intent.frame() < current.frame
        {
            return Err(WgpuRenderRuntimeError::StaleFrame {
                actual: intent.frame(),
                current: current.frame,
            });
        }
        let allowed = requirements
            .resources()
            .iter()
            .map(|requirement| requirement.key())
            .collect::<BTreeSet<_>>();
        let mut by_key = BTreeMap::new();
        for lease in leases {
            let key = lease.key();
            if !allowed.contains(&key) {
                return Err(WgpuRenderRuntimeError::UnexpectedLease);
            }
            if by_key.insert(key, *lease).is_some() {
                return Err(WgpuRenderRuntimeError::DuplicateLease);
            }
            let expected = catalog
                .resource_payload_descriptor(key)
                .map_err(|_| WgpuRenderRuntimeError::PayloadContractMismatch)?;
            let payload = lease.payload();
            if payload.descriptor() != expected || payload.shape() != key.region().shape() {
                return Err(WgpuRenderRuntimeError::PayloadContractMismatch);
            }
        }
        Ok(by_key)
    }

    fn plan_frame(
        current_frame_state: Option<&FrameState>,
        requirements: &RenderRequirements,
    ) -> Result<(FrameState, Vec<DatasetResourceKey>), WgpuRenderRuntimeError> {
        let keys = requirements
            .resources()
            .iter()
            .map(|requirement| requirement.key())
            .collect::<Vec<_>>();
        Self::plan_frame_keys(current_frame_state, requirements.frame(), keys)
    }

    fn plan_frame_keys(
        current_frame_state: Option<&FrameState>,
        frame: FrameIdentity,
        keys: Vec<DatasetResourceKey>,
    ) -> Result<(FrameState, Vec<DatasetResourceKey>), WgpuRenderRuntimeError> {
        let mut state = match current_frame_state {
            Some(current) if current.frame == frame => {
                if current.requirements != keys {
                    return Err(WgpuRenderRuntimeError::RequirementSetChanged);
                }
                current.clone()
            }
            _ => FrameState {
                frame,
                requirements: keys,
                cursor: 0,
            },
        };
        let remaining = state.requirements.len().saturating_sub(state.cursor);
        let count = remaining.min(MAX_VISITS);
        let visited = state.requirements[state.cursor..state.cursor + count].to_vec();
        state.cursor += count;
        if state.cursor == state.requirements.len() {
            state.cursor = 0;
        }
        Ok((state, visited))
    }

    fn refresh_diagnostics(
        &mut self,
        transfer_bytes: u64,
        display_bytes: u64,
        capture_bytes: u64,
        rendered: bool,
        submissions: u32,
    ) {
        let used = self
            .resident
            .values()
            .map(|resource| resource.allocated_bytes)
            .sum::<u64>();
        self.diagnostics.resident_payload_used_bytes = used;
        self.diagnostics.peak_resident_payload_used_bytes =
            self.diagnostics.peak_resident_payload_used_bytes.max(used);
        self.diagnostics.peak_transfer_bytes =
            self.diagnostics.peak_transfer_bytes.max(transfer_bytes);
        self.diagnostics.peak_display_target_bytes = self
            .diagnostics
            .peak_display_target_bytes
            .max(display_bytes);
        self.diagnostics.peak_scratch_bytes =
            self.diagnostics.peak_scratch_bytes.max(capture_bytes);
        self.diagnostics.frames_executed = self
            .diagnostics
            .frames_executed
            .saturating_add(u64::from(rendered));
        self.diagnostics.queue_submissions = self
            .diagnostics
            .queue_submissions
            .saturating_add(u64::from(submissions));
        self.sync_validation_errors();
    }

    fn sync_validation_errors(&mut self) {
        self.diagnostics.validation_error_count = self
            .validation_errors
            .lock()
            .map_or(u64::MAX, |errors| errors.len() as u64);
    }
}

fn validate_adapter(adapter: &wgpu::Adapter) -> Result<(), WgpuRenderRuntimeError> {
    let info = adapter.get_info();
    if matches!(info.device_type, wgpu::DeviceType::Cpu) {
        return Err(WgpuRenderRuntimeError::SoftwareAdapter);
    }
    if info.backend != wgpu::Backend::Vulkan {
        return Err(WgpuRenderRuntimeError::UnsupportedBackend);
    }
    let limits = adapter.limits();
    if limits.max_buffer_size < MIN_BUFFER_LIMIT_BYTES
        || limits.max_storage_buffer_binding_size < MIN_STORAGE_BINDING_LIMIT_BYTES
        || limits.max_storage_buffers_per_shader_stage < MIN_STORAGE_BUFFERS_PER_STAGE
    {
        return Err(WgpuRenderRuntimeError::AdapterLimitsInsufficient);
    }
    Ok(())
}

fn validate_device_limits(limits: &wgpu::Limits) -> Result<(), WgpuRenderRuntimeError> {
    if limits.max_buffer_size < MIN_BUFFER_LIMIT_BYTES
        || limits.max_storage_buffer_binding_size < MIN_STORAGE_BINDING_LIMIT_BYTES
        || limits.max_storage_buffers_per_shader_stage < MIN_STORAGE_BUFFERS_PER_STAGE
    {
        return Err(WgpuRenderRuntimeError::DeviceLimitsInsufficient);
    }
    Ok(())
}

fn validate_extent(extent: RenderExtent) -> Result<(), WgpuRenderRuntimeError> {
    if extent.width_pixels() > MAX_WIDTH || extent.height_pixels() > MAX_HEIGHT {
        return Err(WgpuRenderRuntimeError::ExtentExceeded);
    }
    Ok(())
}

fn validate_presentation_capacity(registered: usize) -> Result<(), WgpuRenderRuntimeError> {
    if registered >= MAX_PRESENTATION_TARGETS {
        return Err(WgpuRenderRuntimeError::PresentationCapacityExceeded {
            maximum: MAX_PRESENTATION_TARGETS,
        });
    }
    Ok(())
}

fn storage_layout_entry(binding: u32, bytes: u64) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: NonZeroU64::new(bytes),
        },
        count: None,
    }
}

fn align_copy(bytes: u64) -> u64 {
    bytes.max(1).div_ceil(COPY_ALIGNMENT) * COPY_ALIGNMENT
}

#[cfg(test)]
mod tests {
    use mirante4d_dataset::{DatasetResourceIdentity, DatasetSourceId, ResourceRegion};
    use mirante4d_domain::{LogicalLayerKey, ScaleLevel, Shape3D, TimeIndex};

    use super::*;

    fn key(layer: u32, scale: u32, origin_x: u64, width: u64) -> DatasetResourceKey {
        DatasetResourceKey::new(
            DatasetResourceIdentity::Unverified(DatasetSourceId::new(1)),
            LogicalLayerKey::new(layer),
            TimeIndex::new(0),
            ScaleLevel::new(scale),
            ResourceRegion::new(
                [0, 0, origin_x],
                Shape3D::new(1, 1, width).expect("test shape is valid"),
            )
            .expect("test region is valid"),
        )
    }

    fn requirement(key: DatasetResourceKey) -> RenderRequirement {
        RenderRequirement::new(key, RenderRequirementRole::Refinement)
    }

    #[test]
    fn requirement_preflight_preserves_the_129_resource_cursor_fixture() {
        let resources = (0..129)
            .map(|x| requirement(key(0, 0, x, 1)))
            .collect::<Vec<_>>();
        assert_eq!(validate_requirement_slice(&resources), Ok(()));
    }

    #[test]
    fn requirement_preflight_rejects_ambiguous_multiscale_and_overlap() {
        assert_eq!(
            validate_requirement_slice(&[
                requirement(key(0, 0, 0, 1)),
                requirement(key(0, 1, 0, 1)),
            ]),
            Err(WgpuRenderRuntimeError::MixedScaleRequirements)
        );
        assert_eq!(
            validate_requirement_slice(&[
                requirement(key(0, 0, 0, 2)),
                requirement(key(0, 0, 1, 2)),
            ]),
            Err(WgpuRenderRuntimeError::OverlappingResources)
        );
        assert_eq!(
            validate_requirement_slice(&[
                requirement(key(0, 0, 0, 1)),
                requirement(key(0, 0, 1, 1)),
            ]),
            Ok(())
        );
    }

    #[test]
    fn requirement_preflight_has_a_fixed_metadata_ceiling() {
        let resources = (0..=MAX_FRAME_REQUIREMENTS)
            .map(|x| requirement(key(0, 0, x as u64, 1)))
            .collect::<Vec<_>>();
        assert_eq!(
            validate_requirement_slice(&resources),
            Err(WgpuRenderRuntimeError::RequirementCapacityExceeded {
                actual: MAX_FRAME_REQUIREMENTS + 1,
                maximum: MAX_FRAME_REQUIREMENTS,
            })
        );
    }

    #[test]
    fn lease_preflight_enforces_the_exact_frame_visit_ceiling() {
        assert_eq!(validate_lease_capacity(MAX_FRAME_LEASES), Ok(()));
        assert_eq!(
            validate_lease_capacity(MAX_FRAME_LEASES + 1),
            Err(WgpuRenderRuntimeError::LeaseCapacityExceeded {
                actual: MAX_FRAME_LEASES + 1,
                maximum: MAX_FRAME_LEASES,
            })
        );
    }

    #[test]
    fn presentation_capacity_allows_exactly_four_renderer_owned_targets() {
        assert_eq!(validate_presentation_capacity(0), Ok(()));
        assert_eq!(validate_presentation_capacity(3), Ok(()));
        assert_eq!(
            validate_presentation_capacity(4),
            Err(WgpuRenderRuntimeError::PresentationCapacityExceeded { maximum: 4 })
        );
    }

    #[test]
    fn equal_frame_numbers_keep_requirement_state_scoped_to_their_presentation() {
        let frame = FrameIdentity::new(7);
        let first_key = key(0, 0, 0, 1);
        let second_key = key(1, 0, 0, 1);
        let first_state = FrameState {
            frame,
            requirements: vec![first_key],
            cursor: 0,
        };

        assert_eq!(
            Runtime::plan_frame_keys(Some(&first_state), frame, vec![second_key]),
            Err(WgpuRenderRuntimeError::RequirementSetChanged)
        );
        let (second_state, visited) =
            Runtime::plan_frame_keys(None, frame, vec![second_key]).unwrap();
        assert_eq!(second_state.requirements, vec![second_key]);
        assert_eq!(visited, vec![second_key]);
    }

    #[test]
    fn existing_device_preflight_requires_the_renderer_limits() {
        let mut limits = wgpu::Limits::default();
        limits.max_buffer_size = MIN_BUFFER_LIMIT_BYTES;
        limits.max_storage_buffer_binding_size = MIN_STORAGE_BINDING_LIMIT_BYTES;
        limits.max_storage_buffers_per_shader_stage = MIN_STORAGE_BUFFERS_PER_STAGE;
        assert_eq!(validate_device_limits(&limits), Ok(()));

        limits.max_storage_buffer_binding_size = MIN_STORAGE_BINDING_LIMIT_BYTES - 1;
        assert_eq!(
            validate_device_limits(&limits),
            Err(WgpuRenderRuntimeError::DeviceLimitsInsufficient)
        );
    }
}
