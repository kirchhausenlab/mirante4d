#![forbid(unsafe_code)]

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, VecDeque},
    mem::size_of,
    num::NonZeroU64,
    ops::Bound::{Excluded, Unbounded},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

use mirante4d_dataset::{
    DatasetCatalog, DatasetResourceKey, ResourceLease, ResourcePayloadFacts, ResourcePayloadView,
};
use mirante4d_render_api::{
    CameraFrame, FrameCompleteness, FrameCoverage, FrameIdentity, FrameLimitation, FrameProgress,
    GpuLedgerCategory, IsoShadingPolicy, LogicalLayerKey, MAX_RENDER_REQUIREMENTS,
    PreparedRenderRequirements, PreparedResourceBody, PresentationRegistration,
    PresentationRetirement, PresentationToken, PresentedFrame, Projection, RenderExtent,
    RenderIntent, RenderRequirement, RenderRequirements, RenderViewIntent, SamplingPolicy,
    TimeIndex, VolumePickCompleteness, VolumePickPolicy, VolumePickQuery, VolumePickResult,
    VolumePickTicket, VolumePickValue, WorldPoint3,
};

use super::{
    CpuFrameTiming, FrameExecutionReport, GpuFrameTiming, GpuTimingTicket,
    MAX_CONTROL_UPLOAD_BYTES, MAX_PAYLOAD_UPLOAD_BYTES, MAX_RENDER_HEIGHT_PIXELS,
    MAX_RENDER_WIDTH_PIXELS, MAX_UPLOADS, MAX_VISITS, RetainedFrameRenderPolicy, ValidationCapture,
    ValidationCaptureTicket, WgpuRenderRuntimeConfig, WgpuRenderRuntimeDiagnostics,
    WgpuRenderRuntimeError, payload_allocation_bytes,
};

// The metadata ceiling admits a full budget-sized s0 working set on the
// accepted 1--8 GiB configurations. It is deliberately independent of the
// old 128/256 traversal fixture and remains count bounded.
const MAX_ACTIVE_PRESENTATION_TARGETS: usize = 4;
// One inactive target may retain the last complete display texture while a
// replacement is streamed into the fourth active target. This is the atomic
// staged-promotion path; inactive targets own no residency or page pins.
const MAX_REGISTERED_PRESENTATION_TARGETS: usize = MAX_ACTIVE_PRESENTATION_TARGETS + 1;
const MAX_FRAME_LEASES: usize = MAX_RENDER_REQUIREMENTS;
// Metadata-only empty pages do not consume payload-arena bytes, but their CPU
// records are still count bounded. The ceiling keeps every page pinned by the
// four active presentations plus one complete requirement set of reuse. When
// full, only inactive records are eligible for LRU removal; the replacing
// frame's new body is protected before it becomes presentation state.
const MAX_EMPTY_RESIDENTS: usize = MAX_RENDER_REQUIREMENTS * (MAX_ACTIVE_PRESENTATION_TARGETS + 1);
// One key, one resident value, and a deliberately conservative sixteen-word
// BTree node/index/allocator allowance, rounded to a cache-line. This is a host
// metadata ledger charge, not a claim about one std implementation's private
// node layout.
const EMPTY_RESIDENT_INDEX_ALLOWANCE_BYTES: usize = 16 * size_of::<usize>();
const EMPTY_RESIDENT_METADATA_BYTES_PER_RECORD: u64 = host_metadata_record_bytes(
    size_of::<DatasetResourceKey>()
        + size_of::<ResidentResource>()
        + EMPTY_RESIDENT_INDEX_ALLOWANCE_BYTES,
);
const EMPTY_RESIDENT_METADATA_CAPACITY_BYTES: u64 =
    MAX_EMPTY_RESIDENTS as u64 * EMPTY_RESIDENT_METADATA_BYTES_PER_RECORD;
const MIN_BUFFER_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
const MIN_STORAGE_BINDING_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
const MIN_STORAGE_BUFFERS_PER_STAGE: u32 = 8;
const CONTROL_BUFFER_BYTES: u64 = 8 * 1024 * 1024;
const MIN_CONTROL_BUFFER_BYTES: u64 = 256 * 1024;
const INITIAL_CONTROL_BYTES: u64 = 256 * 1024;
const COPY_ALIGNMENT: u64 = wgpu::COPY_BUFFER_ALIGNMENT;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const FACT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg8Uint;
const COLOR_BYTES_PER_PIXEL: u32 = 4;
const FACT_BYTES_PER_PIXEL: u32 = 2;
const HEADER_WORDS: usize = 32;
const LAYER_WORDS: usize = 64;
const RESOURCE_WORDS: usize = 16;
// Queue::write_buffer owns one staging allocation/copy per call. Keep sparse
// availability updates cheap, but switch to one dense record publication before
// a fragmented update can turn into hundreds or thousands of tiny queue writes.
const MAX_CONTROL_PUBLICATION_WRITES_PER_FRAME: usize = 64;
const MAX_IN_FLIGHT_SUBMISSIONS: usize = 3;
const TIMING_SLOT_COUNT: usize = MAX_IN_FLIGHT_SUBMISSIONS;
const TIMING_QUERY_WORDS: u32 = 6;
const TIMING_QUERY_BYTES: u64 = TIMING_QUERY_WORDS as u64 * 8;
const TIMING_RESOLVE_STRIDE: u64 = wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT;
const MAX_COMPLETED_TIMINGS: usize = 64;
const PICK_SLOT_COUNT: usize = MAX_IN_FLIGHT_SUBMISSIONS;
const PICK_QUERY_WORDS: usize = 8;
const PICK_QUERY_BYTES: u64 = PICK_QUERY_WORDS as u64 * 4;
const PICK_OUTPUT_WORDS: usize = 12;
const PICK_OUTPUT_BYTES: u64 = PICK_OUTPUT_WORDS as u64 * 4;
const PICK_OUTPUT_MAGIC: u32 = 0x4d34_504b;
const MAX_PAYLOAD_SEGMENTS: usize = 4;
const MAX_SHADER_SEGMENT_BYTES: u64 = u32::MAX as u64 + 1;

const fn host_metadata_record_bytes(bytes: usize) -> u64 {
    bytes.div_ceil(64) as u64 * 64
}

const fn empty_resident_metadata_bytes(records: usize) -> u64 {
    (records as u64).saturating_mul(EMPTY_RESIDENT_METADATA_BYTES_PER_RECORD)
}

fn validate_empty_resident_metadata_capacity(records: usize) -> Result<(), WgpuRenderRuntimeError> {
    if records > MAX_EMPTY_RESIDENTS {
        return Err(WgpuRenderRuntimeError::ResidentMetadataCapacityExceeded {
            requested_records: records,
            maximum_records: MAX_EMPTY_RESIDENTS,
            requested_bytes: empty_resident_metadata_bytes(records),
            available_bytes: EMPTY_RESIDENT_METADATA_CAPACITY_BYTES,
        });
    }
    Ok(())
}

fn oldest_unprotected_keys<K: Copy>(
    age_order: impl IntoIterator<Item = (u64, K)>,
    excess: usize,
    mut is_protected: impl FnMut(K) -> bool,
) -> Vec<K> {
    if excess == 0 {
        return Vec::new();
    }
    age_order
        .into_iter()
        .filter(|(_, key)| !is_protected(*key))
        .take(excess)
        .map(|(_, key)| key)
        .collect()
}

fn next_age_entry(
    index: &BTreeSet<(u64, DatasetResourceKey)>,
    after: Option<(u64, DatasetResourceKey)>,
) -> Option<(u64, DatasetResourceKey)> {
    match after {
        Some(entry) => index.range((Excluded(entry), Unbounded)).next().copied(),
        None => index.first().copied(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrameState {
    frame: FrameIdentity,
    requirements: RenderRequirements,
    cursor: usize,
}

struct PayloadLayout {
    allocation_bytes: u64,
    validity_offset: Option<u64>,
}

/// Exact-byte best-fit allocator for the persistent payload arena.
///
/// Dual offset/size indexes make allocation, exact reservation, release, and
/// neighbor coalescing O(log ranges), including adversarially fragmented
/// arenas. Frame planning uses a bounded undo log and never clones either
/// index or the resident map.
#[derive(Debug, Clone)]
struct ArenaAllocator {
    by_offset: BTreeMap<u64, u64>,
    by_size: BTreeSet<(u64, u64)>,
    available: u64,
}

struct PayloadSegment {
    buffer: wgpu::Buffer,
    capacity: u64,
    allocator: ArenaAllocator,
}

impl ArenaAllocator {
    fn new(capacity: u64) -> Self {
        let mut allocator = Self {
            by_offset: BTreeMap::new(),
            by_size: BTreeSet::new(),
            available: 0,
        };
        allocator.insert_range(0, capacity);
        allocator
    }

    fn insert_range(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        let bytes = end - start;
        assert!(self.by_offset.insert(start, end).is_none());
        assert!(self.by_size.insert((bytes, start)));
        self.available += bytes;
    }

    fn remove_range(&mut self, start: u64) -> Option<u64> {
        let end = self.by_offset.remove(&start)?;
        let bytes = end - start;
        assert!(self.by_size.remove(&(bytes, start)));
        self.available -= bytes;
        Some(end)
    }

    fn allocate(&mut self, bytes: u64) -> Option<u64> {
        let bytes = align_copy(bytes);
        let (_, start) = self.by_size.range((bytes, 0)..).next().copied()?;
        let end = self
            .remove_range(start)
            .expect("size index points to an offset range");
        self.insert_range(start + bytes, end);
        Some(start)
    }

    fn release(&mut self, offset: u64, bytes: u64) {
        let mut start = offset;
        let mut end = offset.saturating_add(align_copy(bytes));
        if let Some((previous_start, previous_end)) = self
            .by_offset
            .range(..=start)
            .next_back()
            .map(|(start, end)| (*start, *end))
            && previous_end == start
        {
            self.remove_range(previous_start);
            start = previous_start;
        }
        if let Some((next_start, next_end)) = self
            .by_offset
            .range(start..)
            .next()
            .map(|(start, end)| (*start, *end))
            && next_start == end
        {
            self.remove_range(next_start);
            end = next_end;
        }
        self.insert_range(start, end);
    }

    fn available_bytes(&self) -> u64 {
        self.available
    }

    fn reserve_exact(&mut self, offset: u64, bytes: u64) -> bool {
        let bytes = align_copy(bytes);
        let end = offset.saturating_add(bytes);
        let Some((range_start, range_end)) = self
            .by_offset
            .range(..=offset)
            .next_back()
            .map(|(start, end)| (*start, *end))
        else {
            return false;
        };
        if end > range_end {
            return false;
        }
        self.remove_range(range_start);
        self.insert_range(range_start, offset);
        self.insert_range(end, range_end);
        true
    }

    #[cfg(test)]
    fn ranges(&self) -> Vec<(u64, u64)> {
        self.by_offset
            .iter()
            .map(|(start, end)| (*start, *end))
            .collect()
    }
}

#[derive(Debug, Clone, Copy)]
enum ArenaOperation {
    Allocate {
        segment: usize,
        offset: u64,
        bytes: u64,
    },
    Release {
        segment: usize,
        offset: u64,
        bytes: u64,
    },
}

fn rollback_arena_operations(segments: &mut [PayloadSegment], operations: &[ArenaOperation]) {
    for operation in operations.iter().rev() {
        match *operation {
            ArenaOperation::Allocate {
                segment,
                offset,
                bytes,
            } => segments[segment].allocator.release(offset, bytes),
            ArenaOperation::Release {
                segment,
                offset,
                bytes,
            } => {
                assert!(segments[segment].allocator.reserve_exact(offset, bytes));
            }
        }
    }
}

fn rollback_arena_operations_from(
    segments: &mut [PayloadSegment],
    operations: &mut Vec<ArenaOperation>,
    start: usize,
) {
    rollback_arena_operations(segments, &operations[start..]);
    operations.truncate(start);
}

fn apply_arena_operations(segments: &mut [PayloadSegment], operations: &[ArenaOperation]) {
    for operation in operations {
        match *operation {
            ArenaOperation::Allocate {
                segment,
                offset,
                bytes,
            } => {
                assert!(segments[segment].allocator.reserve_exact(offset, bytes));
            }
            ArenaOperation::Release {
                segment,
                offset,
                bytes,
            } => segments[segment].allocator.release(offset, bytes),
        }
    }
}

fn payload_layout(
    payload: ResourcePayloadView<'_>,
) -> Result<PayloadLayout, WgpuRenderRuntimeError> {
    let value_len = payload.value_byte_len();
    let validity_offset = payload.validity_bits().map(|_| align_copy(value_len));
    Ok(PayloadLayout {
        allocation_bytes: payload_allocation_bytes(payload.descriptor())?,
        validity_offset,
    })
}

/// Clears only bytes copied into the GPU arena that are not replaced by the
/// immutable value or validity payload. Persistent mapped slots retain prior
/// contents, so alignment gaps must be deterministic; clearing value spans
/// first would merely double every upload's CPU memory traffic.
fn upload_padding_ranges(
    layout: &PayloadLayout,
    value_len: usize,
    validity_len: usize,
) -> [std::ops::Range<usize>; 2] {
    let validity_start = layout
        .validity_offset
        .map_or(value_len, |offset| offset as usize);
    debug_assert!(value_len <= validity_start);
    let payload_end = if layout.validity_offset.is_some() {
        validity_start + validity_len
    } else {
        debug_assert_eq!(validity_len, 0);
        value_len
    };
    debug_assert!(payload_end <= layout.allocation_bytes as usize);
    [
        value_len..validity_start,
        payload_end..layout.allocation_bytes as usize,
    ]
}

fn build_progress(
    coverage: FrameCoverage,
    budget_limited: bool,
    shader_capacity_limited: bool,
) -> Result<Option<FrameProgress>, WgpuRenderRuntimeError> {
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

const fn retained_frame_policy_allows_render(
    render_policy: RetainedFrameRenderPolicy,
    completeness: Option<FrameCompleteness>,
) -> bool {
    match render_policy {
        RetainedFrameRenderPolicy::EveryUsefulFrame => completeness.is_some(),
        RetainedFrameRenderPolicy::ExactFrameOnly => {
            matches!(completeness, Some(FrameCompleteness::Exact))
        }
    }
}

fn validate_requirement_contract(
    requirements: &RenderRequirements,
) -> Result<(), WgpuRenderRuntimeError> {
    validate_requirement_iter(requirements.resources())
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

fn validate_requirement_iter(
    resources: impl ExactSizeIterator<Item = RenderRequirement>,
) -> Result<(), WgpuRenderRuntimeError> {
    if resources.len() > MAX_RENDER_REQUIREMENTS {
        return Err(WgpuRenderRuntimeError::RequirementCapacityExceeded {
            actual: resources.len(),
            maximum: MAX_RENDER_REQUIREMENTS,
        });
    }

    let mut scales_by_layer = BTreeMap::new();
    for requirement in resources {
        let key = requirement.key();
        match scales_by_layer.entry(key.layer()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(key.scale());
            }
            std::collections::btree_map::Entry::Occupied(entry) if *entry.get() != key.scale() => {
                return Err(WgpuRenderRuntimeError::MixedScaleRequirements);
            }
            std::collections::btree_map::Entry::Occupied(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
fn validate_requirement_slice(
    resources: &[RenderRequirement],
) -> Result<(), WgpuRenderRuntimeError> {
    validate_requirement_iter(resources.iter().copied())
}

fn display_allocation_bytes(
    extent: RenderExtent,
    validation_facts: bool,
) -> Result<u64, WgpuRenderRuntimeError> {
    let pixels = u64::from(extent.width_pixels())
        .checked_mul(u64::from(extent.height_pixels()))
        .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
    let bytes_per_pixel = COLOR_BYTES_PER_PIXEL
        + if validation_facts {
            FACT_BYTES_PER_PIXEL
        } else {
            0
        };
    pixels
        .checked_mul(u64::from(bytes_per_pixel))
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
    validation_facts: bool,
) -> Result<DisplayTarget, WgpuRenderRuntimeError> {
    let allocated_bytes = display_allocation_bytes(extent, validation_facts)?;
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
    let fact_texture = validation_facts.then(|| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mirante4d-wp09a-fact-target"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FACT_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    });
    let fact_view = fact_texture
        .as_ref()
        .map(|texture| texture.create_view(&wgpu::TextureViewDescriptor::default()));
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
    timestamps: Option<(&wgpu::QuerySet, u32, u32)>,
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
        display
            .fact_view
            .as_ref()
            .map(|fact_view| wgpu::RenderPassColorAttachment {
                view: fact_view,
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
        timestamp_writes: timestamps.map(|(query_set, beginning, end)| {
            wgpu::RenderPassTimestampWrites {
                query_set,
                beginning_of_pass_write_index: Some(beginning),
                end_of_pass_write_index: Some(end),
            }
        }),
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
            texture: display
                .fact_texture
                .as_ref()
                .ok_or(WgpuRenderRuntimeError::ValidationCaptureFailed)?,
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

#[derive(Debug)]
struct LayerPageLayout {
    origin_zyx: [u64; 3],
    cell_zyx: [u64; 3],
    capacity: u32,
    seed: u32,
    slots: Vec<[u32; 4]>,
    occupied_cells: u32,
    transient_host_bytes: u64,
}

const PAGE_TOMBSTONE: u32 = u32::MAX;
// Body replacements larger than this are rebuilt through the ordinary full
// preparation path. Keeping the delta authority this small bounds worker
// scratch, queue writes, and patch lookup without creating a second layout
// strategy.
pub const MAX_INCREMENTAL_STATIC_KEY_CHANGES: usize = 32;
const MAX_INCREMENTAL_STATIC_PAGE_CHANGES: usize = MAX_CONTROL_PUBLICATION_WRITES_PER_FRAME - 2;
// Once a long-lived layout has accumulated material changes, compact it at a
// normal body boundary. Until then, a small replacement shares the immutable
// base table and owns only its sorted final-word overlay. Compaction also
// releases the deliberately conservative base-anchor/immediate-predecessor
// cohort charges, so accounting retention cannot grow with generations.
const MAX_STATIC_PAGE_PATCHES: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PageTablePatch {
    /// Four-word page entry offset relative to `base_page_table`.
    word_offset: u32,
    words: [u32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResourceSlotChange {
    slot: u32,
    key: Option<DatasetResourceKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StaticRequirementDelta {
    previous_requirements: PreparedResourceBody,
    previous_version: Arc<()>,
    removed: Arc<[DatasetResourceKey]>,
    added: Arc<[DatasetResourceKey]>,
    resource_slots: Arc<[ResourceSlotChange]>,
    page_patches: Arc<[PageTablePatch]>,
}

/// Immutable renderer-specific page/control layout produced on the demand
/// worker. The large canonical/ranked requirement arrays remain owned by the
/// render API; clones of this value share both those arrays and the static
/// page-table bytes by `Arc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedStaticPresentationLayout {
    requirements: PreparedResourceBody,
    /// The body whose attached cohort lease owns the one shared immutable
    /// base-table/layer allocation for this lineage. Incremental descendants
    /// retain this anchor instead of charging the base again. The app's
    /// current cohort lease is coarser than this artifact, so at most the base
    /// cohort and immediate predecessor cohort are conservatively retained;
    /// `MAX_STATIC_PAGE_PATCHES` forces bounded compaction.
    base_accounting_anchor: PreparedResourceBody,
    layers: Arc<[LogicalLayerKey]>,
    scale_keys: Arc<[DatasetResourceKey]>,
    layer_page_fields: Arc<[[u32; 7]]>,
    /// Immutable table constructed by the most recent full preparation.
    base_page_table: Arc<[u32]>,
    /// Sorted final values for page entries changed since the base table.
    page_table_patches: Arc<[PageTablePatch]>,
    occupied_cells: Arc<[u32]>,
    free_record_slots: Arc<[u32]>,
    next_record_slot: u32,
    record_capacity: u32,
    page_table_offset: u64,
    logical_control_bytes: u64,
    renderer_host_allocation_bytes: u64,
    preparation_peak_host_allocation_bytes: u64,
    version: Arc<()>,
    delta: Option<Arc<StaticRequirementDelta>>,
}

/// Exact requested-byte ledger facts available before any large renderer
/// artifact allocation. Demand workers reserve the retained result and the
/// temporary construction scratch independently, then release scratch as soon
/// as preparation completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticPresentationLayoutPreflight {
    renderer_host_allocation_bytes: u64,
    construction_scratch_allocation_bytes: u64,
    logical_control_bytes: u64,
}

impl StaticPresentationLayoutPreflight {
    pub const fn renderer_host_allocation_bytes(self) -> u64 {
        self.renderer_host_allocation_bytes
    }

    pub const fn construction_scratch_allocation_bytes(self) -> u64 {
        self.construction_scratch_allocation_bytes
    }

    pub const fn construction_peak_host_allocation_bytes(self) -> u64 {
        self.renderer_host_allocation_bytes
            .saturating_add(self.construction_scratch_allocation_bytes)
    }

    pub const fn logical_control_bytes(self) -> u64 {
        self.logical_control_bytes
    }
}

impl PreparedStaticPresentationLayout {
    /// Persistent renderer-owned host bytes. Shared render-requirement bytes
    /// are reported separately and are never duplicated by this artifact.
    pub const fn renderer_host_allocation_bytes(&self) -> u64 {
        self.renderer_host_allocation_bytes
    }

    pub fn shared_requirement_host_allocation_bytes(&self) -> u64 {
        self.requirements.host_allocation_bytes()
    }

    /// Conservative exact-requested-byte peak while constructing this layout
    /// on the worker, including the retained result and the largest temporary
    /// flat occupancy/hash vectors. It excludes the shared requirement body.
    pub const fn preparation_peak_host_allocation_bytes(&self) -> u64 {
        self.preparation_peak_host_allocation_bytes
    }

    pub const fn logical_control_bytes(&self) -> u64 {
        self.logical_control_bytes
    }

    pub fn resource_count(&self) -> usize {
        self.requirements.canonical().len()
    }

    pub const fn is_incremental_body_update(&self) -> bool {
        self.delta.is_some()
    }

    pub fn incremental_removed_resources(&self) -> &[DatasetResourceKey] {
        self.delta
            .as_ref()
            .map_or(&[], |delta| delta.removed.as_ref())
    }

    pub fn incremental_added_resources(&self) -> &[DatasetResourceKey] {
        self.delta
            .as_ref()
            .map_or(&[], |delta| delta.added.as_ref())
    }

    pub fn incremental_preparation_key_visits(&self) -> usize {
        self.delta
            .as_ref()
            .map_or(0, |delta| delta.removed.len() + delta.added.len())
    }

    pub fn incremental_resource_slot_changes(&self) -> usize {
        self.delta
            .as_ref()
            .map_or(0, |delta| delta.resource_slots.len())
    }

    pub fn incremental_page_entry_changes(&self) -> usize {
        self.delta
            .as_ref()
            .map_or(0, |delta| delta.page_patches.len())
    }

    pub fn shares_base_page_storage_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.base_page_table, &other.base_page_table)
            && self
                .base_accounting_anchor
                .shares_storage_with(&other.base_accounting_anchor)
    }

    #[cfg(test)]
    pub(crate) fn initial_page_hash_slot(&self, key: DatasetResourceKey) -> Option<u32> {
        let layer_index = self.layers.iter().position(|layer| *layer == key.layer())?;
        let fields = self.layer_page_fields.get(layer_index)?;
        let coordinate = page_coordinate_for_key(fields, key)?;
        let page_start = layer_page_relative_offset(self, fields)?;
        let capacity = *self.base_page_table.get(page_start as usize)?;
        let seed = *self.base_page_table.get(page_start as usize + 1)?;
        (capacity != 0).then(|| page_hash(coordinate, seed) & (capacity - 1))
    }

    fn is_incremental_from(
        &self,
        previous_layout: &Self,
        previous_requirements: &RenderRequirements,
    ) -> bool {
        self.delta.as_ref().is_some_and(|delta| {
            delta
                .previous_requirements
                .shares_storage_with(previous_requirements.prepared_body())
                && Arc::ptr_eq(&delta.previous_version, &previous_layout.version)
        })
    }

    fn effective_page_entry(&self, word_offset: u32) -> Option<[u32; 4]> {
        if let Ok(index) = self
            .page_table_patches
            .binary_search_by_key(&word_offset, |patch| patch.word_offset)
        {
            return Some(self.page_table_patches[index].words);
        }
        let start = usize::try_from(word_offset).ok()?;
        self.base_page_table
            .get(start..start.checked_add(4)?)?
            .try_into()
            .ok()
    }

    fn resource_slot(&self, key: DatasetResourceKey) -> Option<u32> {
        let layer_index = self.layers.iter().position(|layer| *layer == key.layer())?;
        let fields = self.layer_page_fields.get(layer_index)?;
        let coordinate = page_coordinate_for_key(fields, key)?;
        lookup_page_entry(self, fields, coordinate).map(|(_, entry)| entry[3] - 1)
    }

    fn matches(&self, intent: &RenderIntent, requirements: &RenderRequirements) -> bool {
        self.requirements
            .shares_storage_with(requirements.prepared_body())
            && self
                .layers
                .iter()
                .copied()
                .eq(intent.layers().iter().map(|layer| layer.layer()))
    }
}

fn patch_host_bytes(count: usize) -> u64 {
    (count as u64).saturating_mul(size_of::<PageTablePatch>() as u64)
}

fn key_host_bytes(count: usize) -> u64 {
    (count as u64).saturating_mul(size_of::<DatasetResourceKey>() as u64)
}

fn incremental_static_layout_host_bytes(
    layout: &PreparedStaticPresentationLayout,
    delta: &StaticRequirementDelta,
) -> u64 {
    (size_of::<PreparedStaticPresentationLayout>() as u64)
        .saturating_add(patch_host_bytes(layout.page_table_patches.len()))
        .saturating_add(
            (layout.occupied_cells.len() as u64).saturating_mul(size_of::<u32>() as u64),
        )
        .saturating_add(
            (layout.free_record_slots.len() as u64).saturating_mul(size_of::<u32>() as u64),
        )
        .saturating_add(key_host_bytes(delta.removed.len()))
        .saturating_add(key_host_bytes(delta.added.len()))
        .saturating_add(
            (delta.resource_slots.len() as u64)
                .saturating_mul(size_of::<ResourceSlotChange>() as u64),
        )
        .saturating_add(patch_host_bytes(delta.page_patches.len()))
        .saturating_add(size_of::<StaticRequirementDelta>() as u64)
        .saturating_add(size_of::<()>() as u64)
}

const MAX_PAGE_HASH_PROBES: usize = 32;
const MAX_PAGE_HASH_SEEDS: u32 = 256;

fn record_slot_capacity(requirement_count: usize) -> Result<usize, WgpuRenderRuntimeError> {
    requirement_count
        .checked_next_power_of_two()
        .filter(|capacity| *capacity <= MAX_RENDER_REQUIREMENTS)
        .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)
}

/// Computes exact requested persistent/control capacities without allocating
/// the page table or a full-size key/index structure.
pub fn preflight_static_presentation_layout(
    catalog: &DatasetCatalog,
    requirements: &PreparedRenderRequirements,
) -> Result<StaticPresentationLayoutPreflight, WgpuRenderRuntimeError> {
    preflight_static_presentation_layout_from_parts(
        catalog,
        requirements.body(),
        requirements.layers(),
    )
}

fn preflight_static_presentation_layout_from_parts(
    catalog: &DatasetCatalog,
    body: &PreparedResourceBody,
    layers: &[LogicalLayerKey],
) -> Result<StaticPresentationLayoutPreflight, WgpuRenderRuntimeError> {
    let keys = body.canonical().as_ref();
    if keys.is_empty() || keys.len() > MAX_RENDER_REQUIREMENTS {
        return Err(WgpuRenderRuntimeError::RequirementCapacityExceeded {
            actual: keys.len(),
            maximum: MAX_RENDER_REQUIREMENTS,
        });
    }
    let mut page_table_words = 0_usize;
    let mut scale_key_count = 0_usize;
    let mut maximum_layer_transient_bytes = 0_u64;
    for layer in layers.iter().copied() {
        let catalog_layer = catalog
            .layer(layer)
            .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
        let start = keys.partition_point(|key| key.layer() < layer);
        let end = keys.partition_point(|key| key.layer() <= layer);
        let layer_keys = &keys[start..end];
        if let Some(first) = layer_keys.first().copied() {
            if layer_keys.iter().any(|key| key.scale() != first.scale()) {
                return Err(WgpuRenderRuntimeError::MixedScaleRequirements);
            }
            if catalog_layer.scale(first.scale()).is_none() {
                return Err(WgpuRenderRuntimeError::PayloadContractMismatch);
            }
            scale_key_count += 1;
        } else if catalog_layer.scales().next().is_none() {
            return Err(WgpuRenderRuntimeError::PayloadContractMismatch);
        }
        let occupied_count = page_layout_geometry(layer_keys)?.2;
        let capacity = if occupied_count == 0 {
            0
        } else {
            occupied_count
                .checked_mul(2)
                .and_then(usize::checked_next_power_of_two)
                .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?
        };
        page_table_words = page_table_words
            .checked_add(2)
            .and_then(|words| words.checked_add(capacity.checked_mul(4)?))
            .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
        let transient = (occupied_count as u64)
            .saturating_mul(size_of::<([u32; 3], u32)>() as u64)
            .saturating_add((capacity as u64).saturating_mul(size_of::<[u32; 4]>() as u64));
        maximum_layer_transient_bytes = maximum_layer_transient_bytes.max(transient);
    }
    let prefix_words = HEADER_WORDS
        .checked_add(
            layers
                .len()
                .checked_mul(LAYER_WORDS)
                .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?,
        )
        .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    let record_capacity = record_slot_capacity(keys.len())?;
    let page_table_offset_words = prefix_words
        .checked_add(
            record_capacity
                .checked_mul(RESOURCE_WORDS)
                .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?,
        )
        .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    let logical_control_words = page_table_offset_words
        .checked_add(page_table_words)
        .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    let logical_control_bytes = u64::try_from(logical_control_words)
        .ok()
        .and_then(|words| words.checked_mul(4))
        .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    if logical_control_bytes > MAX_CONTROL_UPLOAD_BYTES {
        return Err(WgpuRenderRuntimeError::ControlCapacityExceeded);
    }
    let page_table_bytes = (page_table_words as u64).saturating_mul(4);
    let scale_key_bytes =
        (scale_key_count as u64).saturating_mul(size_of::<DatasetResourceKey>() as u64);
    let layer_field_bytes = (layers.len() as u64).saturating_mul(size_of::<[u32; 7]>() as u64);
    let layer_key_bytes = (layers.len() as u64).saturating_mul(size_of::<LogicalLayerKey>() as u64);
    let occupied_cell_bytes = (layers.len() as u64).saturating_mul(size_of::<u32>() as u64);
    let renderer_host_allocation_bytes = (size_of::<PreparedStaticPresentationLayout>() as u64)
        .saturating_add(page_table_bytes)
        .saturating_add(scale_key_bytes)
        .saturating_add(layer_field_bytes)
        .saturating_add(layer_key_bytes)
        .saturating_add(occupied_cell_bytes);
    // The result arrays are built in exact-capacity Vecs before Arc transfer.
    // Count those temporary arrays plus the largest one-layer hash workspace.
    let construction_scratch_allocation_bytes = page_table_bytes
        .saturating_add(
            (layers.len() as u64).saturating_mul(size_of::<DatasetResourceKey>() as u64),
        )
        .saturating_add(layer_field_bytes)
        .saturating_add(occupied_cell_bytes)
        .saturating_add((layers.len() as u64).saturating_mul(size_of::<(usize, usize)>() as u64))
        .saturating_add(maximum_layer_transient_bytes);
    Ok(StaticPresentationLayoutPreflight {
        renderer_host_allocation_bytes,
        construction_scratch_allocation_bytes,
        logical_control_bytes,
    })
}

/// Builds the renderer's immutable sparse page layout on the same worker that
/// owns demand preparation. It never creates another full requirement-key
/// vector or membership map: canonical indices come directly from the shared,
/// sorted render-API body.
pub fn prepare_static_presentation_layout(
    catalog: &DatasetCatalog,
    requirements: &PreparedRenderRequirements,
) -> Result<PreparedStaticPresentationLayout, WgpuRenderRuntimeError> {
    prepare_static_presentation_layout_from_parts(
        catalog,
        requirements.body(),
        requirements.layers(),
    )
}

/// Preflights the single static-layout authority while allowing a compatible
/// predecessor to retain stable record/hash slots. The compatibility walk is
/// over the already-sorted immutable bodies; it never reconstructs the full
/// page table. A rejected delta uses the ordinary full-layout preflight.
pub fn preflight_static_presentation_layout_update(
    catalog: &DatasetCatalog,
    requirements: &PreparedRenderRequirements,
    previous: Option<&PreparedStaticPresentationLayout>,
    additions: &[DatasetResourceKey],
    removals: &[DatasetResourceKey],
) -> Result<StaticPresentationLayoutPreflight, WgpuRenderRuntimeError> {
    if let Some(previous) = previous
        && let Some(layout) = try_prepare_incremental_static_layout(
            catalog,
            requirements,
            previous,
            additions,
            removals,
        )?
    {
        return Ok(StaticPresentationLayoutPreflight {
            renderer_host_allocation_bytes: layout.renderer_host_allocation_bytes,
            construction_scratch_allocation_bytes: layout
                .preparation_peak_host_allocation_bytes
                .saturating_sub(layout.renderer_host_allocation_bytes),
            logical_control_bytes: layout.logical_control_bytes,
        });
    }
    preflight_static_presentation_layout(catalog, requirements)
}

/// Prepares a body replacement against stable slots when its exact predecessor
/// and page geometry remain compatible. This is still the same sparse hash
/// table and control-buffer format; incompatible or material changes are
/// compacted through the sole full builder.
pub fn prepare_static_presentation_layout_update(
    catalog: &DatasetCatalog,
    requirements: &PreparedRenderRequirements,
    previous: Option<&PreparedStaticPresentationLayout>,
    additions: &[DatasetResourceKey],
    removals: &[DatasetResourceKey],
) -> Result<PreparedStaticPresentationLayout, WgpuRenderRuntimeError> {
    if let Some(previous) = previous
        && let Some(layout) = try_prepare_incremental_static_layout(
            catalog,
            requirements,
            previous,
            additions,
            removals,
        )?
    {
        return Ok(layout);
    }
    prepare_static_presentation_layout(catalog, requirements)
}

fn prepare_static_presentation_layout_from_parts(
    catalog: &DatasetCatalog,
    body: &PreparedResourceBody,
    layers: &[LogicalLayerKey],
) -> Result<PreparedStaticPresentationLayout, WgpuRenderRuntimeError> {
    let preflight = preflight_static_presentation_layout_from_parts(catalog, body, layers)?;
    let keys = body.canonical().as_ref();
    if keys.is_empty() || keys.len() > MAX_RENDER_REQUIREMENTS {
        return Err(WgpuRenderRuntimeError::RequirementCapacityExceeded {
            actual: keys.len(),
            maximum: MAX_RENDER_REQUIREMENTS,
        });
    }

    let mut scale_keys = Vec::with_capacity(layers.len());
    let mut layer_ranges = Vec::with_capacity(layers.len());
    for layer in layers.iter().copied() {
        let catalog_layer = catalog
            .layer(layer)
            .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
        let start = keys.partition_point(|key| key.layer() < layer);
        let end = keys.partition_point(|key| key.layer() <= layer);
        let layer_keys = &keys[start..end];
        if let Some(first) = layer_keys.first().copied() {
            if layer_keys.iter().any(|key| key.scale() != first.scale()) {
                return Err(WgpuRenderRuntimeError::MixedScaleRequirements);
            }
            if catalog_layer.scale(first.scale()).is_none() {
                return Err(WgpuRenderRuntimeError::PayloadContractMismatch);
            }
            scale_keys.push(first);
        } else if catalog_layer.scales().next().is_none() {
            return Err(WgpuRenderRuntimeError::PayloadContractMismatch);
        }
        layer_ranges.push((start, end));
    }

    let prefix_words = HEADER_WORDS
        .checked_add(
            layers
                .len()
                .checked_mul(LAYER_WORDS)
                .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?,
        )
        .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    let record_capacity = record_slot_capacity(keys.len())?;
    let page_table_offset_words = prefix_words
        .checked_add(
            record_capacity
                .checked_mul(RESOURCE_WORDS)
                .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?,
        )
        .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    let page_table_offset = u64::try_from(page_table_offset_words)
        .ok()
        .and_then(|words| words.checked_mul(4))
        .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    let exact_page_table_words = usize::try_from(
        preflight
            .logical_control_bytes()
            .saturating_sub(page_table_offset)
            / 4,
    )
    .map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    let mut page_table = Vec::<u32>::with_capacity(exact_page_table_words);
    let mut layer_page_fields = Vec::with_capacity(layers.len());
    let mut occupied_cells = Vec::with_capacity(layers.len());
    let mut maximum_transient_bytes = 0_u64;
    for (start, end) in layer_ranges {
        let layout = build_page_layout(&keys[start..end], start)?;
        maximum_transient_bytes = maximum_transient_bytes.max(layout.transient_host_bytes);
        let absolute_offset = page_table_offset_words
            .checked_add(page_table.len())
            .and_then(|offset| u32::try_from(offset).ok())
            .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
        layer_page_fields.push([
            absolute_offset,
            u64_to_u32(layout.origin_zyx[2])?,
            u64_to_u32(layout.origin_zyx[1])?,
            u64_to_u32(layout.origin_zyx[0])?,
            u64_to_u32(layout.cell_zyx[2])?,
            u64_to_u32(layout.cell_zyx[1])?,
            u64_to_u32(layout.cell_zyx[0])?,
        ]);
        occupied_cells.push(layout.occupied_cells);
        page_table.extend([layout.capacity, layout.seed]);
        page_table.extend(layout.slots.into_iter().flatten());
    }
    let page_table_bytes = (page_table.len() as u64)
        .checked_mul(4)
        .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    let logical_control_bytes = page_table_offset
        .checked_add(page_table_bytes)
        .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    if logical_control_bytes > MAX_CONTROL_UPLOAD_BYTES {
        return Err(WgpuRenderRuntimeError::ControlCapacityExceeded);
    }
    debug_assert_eq!(page_table.len(), exact_page_table_words);
    debug_assert_eq!(logical_control_bytes, preflight.logical_control_bytes());
    let renderer_host_allocation_bytes = preflight.renderer_host_allocation_bytes();
    let preparation_peak_host_allocation_bytes =
        preflight.construction_peak_host_allocation_bytes().max(
            renderer_host_allocation_bytes
                .saturating_add(page_table_bytes)
                .saturating_add(maximum_transient_bytes),
        );
    Ok(PreparedStaticPresentationLayout {
        requirements: body.clone(),
        base_accounting_anchor: body.clone(),
        layers: layers.into(),
        scale_keys: scale_keys.into(),
        layer_page_fields: layer_page_fields.into(),
        base_page_table: page_table.into(),
        page_table_patches: Arc::from([]),
        occupied_cells: occupied_cells.into(),
        free_record_slots: Arc::from([]),
        next_record_slot: u32::try_from(keys.len())
            .map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?,
        record_capacity: u32::try_from(record_capacity)
            .map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?,
        page_table_offset,
        logical_control_bytes,
        renderer_host_allocation_bytes,
        preparation_peak_host_allocation_bytes,
        version: Arc::new(()),
        delta: None,
    })
}

fn page_table_base_entry(
    table: &[u32],
    word_offset: u32,
) -> Result<[u32; 4], WgpuRenderRuntimeError> {
    let start = usize::try_from(word_offset)
        .map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    table
        .get(start..start.saturating_add(4))
        .and_then(|words| words.try_into().ok())
        .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)
}

fn effective_page_entry_from(
    base: &[u32],
    patches: &[PageTablePatch],
    word_offset: u32,
) -> Result<[u32; 4], WgpuRenderRuntimeError> {
    if let Ok(index) = patches.binary_search_by_key(&word_offset, |patch| patch.word_offset) {
        Ok(patches[index].words)
    } else {
        page_table_base_entry(base, word_offset)
    }
}

fn set_page_entry_patch(
    base: &[u32],
    patches: &mut Vec<PageTablePatch>,
    word_offset: u32,
    words: [u32; 4],
) -> Result<(), WgpuRenderRuntimeError> {
    let base_words = page_table_base_entry(base, word_offset)?;
    match patches.binary_search_by_key(&word_offset, |patch| patch.word_offset) {
        Ok(index) if words == base_words => {
            patches.remove(index);
        }
        Ok(index) => patches[index].words = words,
        Err(_) if words == base_words => {}
        Err(index) => patches.insert(index, PageTablePatch { word_offset, words }),
    }
    Ok(())
}

fn page_coordinate_for_key(fields: &[u32; 7], key: DatasetResourceKey) -> Option<[u32; 3]> {
    let origin_xyz = [
        u64::from(fields[1]),
        u64::from(fields[2]),
        u64::from(fields[3]),
    ];
    let cell_xyz = [
        u64::from(fields[4]),
        u64::from(fields[5]),
        u64::from(fields[6]),
    ];
    let origin = key.region().origin();
    let key_xyz = [origin[2], origin[1], origin[0]];
    let mut coordinate = [0_u32; 3];
    for axis in 0..3 {
        let relative = key_xyz[axis].checked_sub(origin_xyz[axis])?;
        if relative % cell_xyz[axis] != 0 {
            return None;
        }
        coordinate[axis] = u32::try_from(relative / cell_xyz[axis]).ok()?;
    }
    Some(coordinate)
}

fn page_coordinate_ranges(
    fields: &[u32; 7],
    key: DatasetResourceKey,
) -> Option<([u32; 3], [u32; 3])> {
    let start = page_coordinate_for_key(fields, key)?;
    let shape = key.region().shape().dimensions();
    let shape_xyz = [shape[2], shape[1], shape[0]];
    let cell_xyz = [
        u64::from(fields[4]),
        u64::from(fields[5]),
        u64::from(fields[6]),
    ];
    let mut end = [0_u32; 3];
    for axis in 0..3 {
        end[axis] = u32::try_from(
            u64::from(start[axis]).checked_add(shape_xyz[axis].div_ceil(cell_xyz[axis]))?,
        )
        .ok()?;
    }
    Some((start, end))
}

fn layer_page_relative_offset(
    layout: &PreparedStaticPresentationLayout,
    fields: &[u32; 7],
) -> Option<u32> {
    fields[0].checked_sub(u32::try_from(layout.page_table_offset / 4).ok()?)
}

fn lookup_page_entry(
    layout: &PreparedStaticPresentationLayout,
    fields: &[u32; 7],
    coordinate: [u32; 3],
) -> Option<(u32, [u32; 4])> {
    let page_start = layer_page_relative_offset(layout, fields)?;
    let capacity = *layout.base_page_table.get(page_start as usize)?;
    let seed = *layout.base_page_table.get(page_start as usize + 1)?;
    if capacity == 0 {
        return None;
    }
    let mut slot = page_hash(coordinate, seed) & (capacity - 1);
    for _ in 0..MAX_PAGE_HASH_PROBES {
        let word_offset = page_start
            .checked_add(2)?
            .checked_add(slot.checked_mul(4)?)?;
        let entry = layout.effective_page_entry(word_offset)?;
        if entry[3] == 0 {
            return None;
        }
        if entry[3] != PAGE_TOMBSTONE && entry[..3] == coordinate {
            return Some((word_offset, entry));
        }
        slot = (slot + 1) & (capacity - 1);
    }
    None
}

fn lookup_page_entry_from(
    layout: &PreparedStaticPresentationLayout,
    patches: &[PageTablePatch],
    fields: &[u32; 7],
    coordinate: [u32; 3],
) -> Result<Option<(u32, [u32; 4])>, WgpuRenderRuntimeError> {
    let page_start = layer_page_relative_offset(layout, fields)
        .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
    let capacity = *layout
        .base_page_table
        .get(page_start as usize)
        .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
    let seed = *layout
        .base_page_table
        .get(page_start as usize + 1)
        .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
    if capacity == 0 {
        return Ok(None);
    }
    let mut slot = page_hash(coordinate, seed) & (capacity - 1);
    for _ in 0..MAX_PAGE_HASH_PROBES {
        let word_offset = page_start
            .checked_add(2)
            .and_then(|offset| offset.checked_add(slot.checked_mul(4)?))
            .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
        let entry = effective_page_entry_from(&layout.base_page_table, patches, word_offset)?;
        if entry[3] == 0 {
            return Ok(None);
        }
        if entry[3] != PAGE_TOMBSTONE && entry[..3] == coordinate {
            return Ok(Some((word_offset, entry)));
        }
        slot = (slot + 1) & (capacity - 1);
    }
    Ok(None)
}

fn insertion_page_word_offset(
    layout: &PreparedStaticPresentationLayout,
    patches: &[PageTablePatch],
    fields: &[u32; 7],
    coordinate: [u32; 3],
) -> Result<Option<u32>, WgpuRenderRuntimeError> {
    let page_start = layer_page_relative_offset(layout, fields)
        .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
    let capacity = *layout
        .base_page_table
        .get(page_start as usize)
        .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
    let seed = *layout
        .base_page_table
        .get(page_start as usize + 1)
        .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
    if capacity == 0 {
        return Ok(None);
    }
    let mut slot = page_hash(coordinate, seed) & (capacity - 1);
    let mut tombstone = None;
    for _ in 0..MAX_PAGE_HASH_PROBES {
        let word_offset = page_start
            .checked_add(2)
            .and_then(|offset| offset.checked_add(slot.checked_mul(4)?))
            .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
        let entry = effective_page_entry_from(&layout.base_page_table, patches, word_offset)?;
        if entry[3] == PAGE_TOMBSTONE {
            tombstone.get_or_insert(word_offset);
        } else if entry[3] == 0 {
            return Ok(Some(tombstone.unwrap_or(word_offset)));
        } else if entry[..3] == coordinate {
            return Ok(None);
        }
        slot = (slot + 1) & (capacity - 1);
    }
    Ok(tombstone)
}

fn try_prepare_incremental_static_layout(
    catalog: &DatasetCatalog,
    requirements: &PreparedRenderRequirements,
    previous: &PreparedStaticPresentationLayout,
    additions: &[DatasetResourceKey],
    removals: &[DatasetResourceKey],
) -> Result<Option<PreparedStaticPresentationLayout>, WgpuRenderRuntimeError> {
    if previous.layers.as_ref() != requirements.layers()
        || previous.page_table_patches.len() >= MAX_STATIC_PAGE_PATCHES
    {
        return Ok(None);
    }
    if additions.len().saturating_add(removals.len()) > MAX_INCREMENTAL_STATIC_KEY_CHANGES
        || (additions.is_empty()
            && removals.is_empty()
            && !previous
                .requirements
                .shares_storage_with(requirements.body()))
        || previous
            .requirements
            .canonical()
            .len()
            .saturating_sub(removals.len())
            .saturating_add(additions.len())
            != requirements.body().canonical().len()
        || !additions.is_sorted()
        || !removals.is_sorted()
        || additions.windows(2).any(|pair| pair[0] == pair[1])
        || removals.windows(2).any(|pair| pair[0] == pair[1])
        || additions.iter().any(|key| {
            requirements.body().resource_index(*key).is_none()
                || previous.requirements.resource_index(*key).is_some()
        })
        || removals.iter().any(|key| {
            previous.requirements.resource_index(*key).is_none()
                || requirements.body().resource_index(*key).is_some()
        })
    {
        return Ok(None);
    }
    let removed = removals;
    let added = additions;

    for key in added {
        let Some(layer_index) = previous
            .layers
            .iter()
            .position(|layer| *layer == key.layer())
        else {
            return Ok(None);
        };
        let Some(catalog_layer) = catalog.layer(key.layer()) else {
            return Err(WgpuRenderRuntimeError::PayloadContractMismatch);
        };
        if catalog_layer.scale(key.scale()).is_none()
            || previous
                .scale_keys
                .iter()
                .find(|candidate| candidate.layer() == key.layer())
                .is_none_or(|candidate| candidate.scale() != key.scale())
            || page_coordinate_ranges(&previous.layer_page_fields[layer_index], *key).is_none()
        {
            return Ok(None);
        }
    }

    let mut patches = previous.page_table_patches.to_vec();
    let mut touched_before = BTreeMap::<u32, [u32; 4]>::new();
    let mut record_changes = BTreeMap::<u32, Option<DatasetResourceKey>>::new();
    let mut free_record_slots = previous.free_record_slots.to_vec();
    let mut occupied_cells = previous.occupied_cells.to_vec();

    for key in removed {
        let Some(layer_index) = previous
            .layers
            .iter()
            .position(|layer| *layer == key.layer())
        else {
            return Ok(None);
        };
        let fields = &previous.layer_page_fields[layer_index];
        let Some((start, end)) = page_coordinate_ranges(fields, *key) else {
            return Ok(None);
        };
        let mut resource_slot = None;
        for z in start[2]..end[2] {
            for y in start[1]..end[1] {
                for x in start[0]..end[0] {
                    let coordinate = [x, y, z];
                    let Some((word_offset, entry)) =
                        lookup_page_entry_from(previous, &patches, fields, coordinate)?
                    else {
                        return Ok(None);
                    };
                    let slot = entry[3] - 1;
                    if resource_slot
                        .replace(slot)
                        .is_some_and(|current| current != slot)
                    {
                        return Ok(None);
                    }
                    touched_before.entry(word_offset).or_insert(entry);
                    set_page_entry_patch(
                        &previous.base_page_table,
                        &mut patches,
                        word_offset,
                        [0, 0, 0, PAGE_TOMBSTONE],
                    )?;
                    occupied_cells[layer_index] = occupied_cells[layer_index].saturating_sub(1);
                }
            }
        }
        let Some(slot) = resource_slot else {
            return Ok(None);
        };
        if !free_record_slots.contains(&slot) {
            free_record_slots.push(slot);
        }
        record_changes.insert(slot, None);
    }
    free_record_slots.sort_unstable();
    free_record_slots.dedup();

    let mut next_record_slot = previous.next_record_slot;
    for key in added {
        let slot = if let Some(slot) = free_record_slots.pop() {
            slot
        } else if next_record_slot < previous.record_capacity {
            let slot = next_record_slot;
            next_record_slot += 1;
            slot
        } else {
            return Ok(None);
        };
        let layer_index = previous
            .layers
            .iter()
            .position(|layer| *layer == key.layer())
            .expect("added-key layers were preflighted");
        let fields = &previous.layer_page_fields[layer_index];
        let page_start = layer_page_relative_offset(previous, fields)
            .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
        let capacity = *previous
            .base_page_table
            .get(page_start as usize)
            .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
        let (start, end) =
            page_coordinate_ranges(fields, *key).expect("added-key geometry was preflighted");
        let added_cells = usize::try_from(end[0] - start[0])
            .ok()
            .and_then(|x| {
                usize::try_from(end[1] - start[1])
                    .ok()
                    .and_then(|y| x.checked_mul(y))
            })
            .and_then(|xy| {
                usize::try_from(end[2] - start[2])
                    .ok()
                    .and_then(|z| xy.checked_mul(z))
            })
            .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
        if u64::from(occupied_cells[layer_index]).saturating_add(added_cells as u64)
            > u64::from(capacity / 2)
        {
            return Ok(None);
        }
        for z in start[2]..end[2] {
            for y in start[1]..end[1] {
                for x in start[0]..end[0] {
                    let coordinate = [x, y, z];
                    let Some(word_offset) =
                        insertion_page_word_offset(previous, &patches, fields, coordinate)?
                    else {
                        return Ok(None);
                    };
                    let before = effective_page_entry_from(
                        &previous.base_page_table,
                        &patches,
                        word_offset,
                    )?;
                    touched_before.entry(word_offset).or_insert(before);
                    set_page_entry_patch(
                        &previous.base_page_table,
                        &mut patches,
                        word_offset,
                        [coordinate[0], coordinate[1], coordinate[2], slot + 1],
                    )?;
                    occupied_cells[layer_index] += 1;
                }
            }
        }
        record_changes.insert(slot, Some(*key));
    }

    if patches.len() > MAX_STATIC_PAGE_PATCHES {
        return Ok(None);
    }
    let immediate_page_patches = touched_before
        .into_iter()
        .filter_map(|(word_offset, before)| {
            let after =
                effective_page_entry_from(&previous.base_page_table, &patches, word_offset).ok()?;
            (after != before).then_some(PageTablePatch {
                word_offset,
                words: after,
            })
        })
        .collect::<Vec<_>>();
    if immediate_page_patches.len() > MAX_INCREMENTAL_STATIC_PAGE_CHANGES {
        return Ok(None);
    }
    let resource_slots = record_changes
        .into_iter()
        .map(|(slot, key)| ResourceSlotChange { slot, key })
        .collect::<Vec<_>>();
    if incremental_control_write_count(
        previous.layers.len(),
        previous.page_table_offset,
        &resource_slots,
        &immediate_page_patches,
    ) > MAX_CONTROL_PUBLICATION_WRITES_PER_FRAME
    {
        return Ok(None);
    }
    let delta = Arc::new(StaticRequirementDelta {
        previous_requirements: previous.requirements.clone(),
        previous_version: Arc::clone(&previous.version),
        removed: removed.into(),
        added: added.into(),
        resource_slots: resource_slots.into(),
        page_patches: immediate_page_patches.into(),
    });
    let mut layout = PreparedStaticPresentationLayout {
        requirements: requirements.body().clone(),
        base_accounting_anchor: previous.base_accounting_anchor.clone(),
        layers: Arc::clone(&previous.layers),
        scale_keys: Arc::clone(&previous.scale_keys),
        layer_page_fields: Arc::clone(&previous.layer_page_fields),
        base_page_table: Arc::clone(&previous.base_page_table),
        page_table_patches: patches.into(),
        occupied_cells: occupied_cells.into(),
        free_record_slots: free_record_slots.into(),
        next_record_slot,
        record_capacity: previous.record_capacity,
        page_table_offset: previous.page_table_offset,
        logical_control_bytes: previous.logical_control_bytes,
        renderer_host_allocation_bytes: 0,
        preparation_peak_host_allocation_bytes: 0,
        version: Arc::new(()),
        delta: Some(delta),
    };
    layout.renderer_host_allocation_bytes = incremental_static_layout_host_bytes(
        &layout,
        layout
            .delta
            .as_deref()
            .expect("an incremental layout owns one exact delta"),
    );
    let delta_scratch = key_host_bytes(
        layout
            .delta
            .as_ref()
            .map_or(0, |delta| delta.removed.len() + delta.added.len()),
    )
    .saturating_add(patch_host_bytes(
        layout
            .delta
            .as_ref()
            .map_or(0, |delta| delta.page_patches.len()),
    ))
    .saturating_add(
        (layout
            .delta
            .as_ref()
            .map_or(0, |delta| delta.resource_slots.len()) as u64)
            .saturating_mul(size_of::<ResourceSlotChange>() as u64),
    );
    layout.preparation_peak_host_allocation_bytes = layout
        .renderer_host_allocation_bytes
        .saturating_add(delta_scratch);
    Ok(Some(layout))
}

fn page_hash(coordinate_xyz: [u32; 3], seed: u32) -> u32 {
    let mut hash = coordinate_xyz[0]
        .wrapping_mul(0x9e37_79b1)
        .wrapping_add(coordinate_xyz[1].wrapping_mul(0x85eb_ca77))
        .wrapping_add(coordinate_xyz[2].wrapping_mul(0xc2b2_ae3d))
        ^ seed.wrapping_mul(0x27d4_eb2d);
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x7feb_352d);
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(0x846c_a68b);
    hash ^ (hash >> 16)
}

fn greatest_common_divisor(mut first: u64, mut second: u64) -> u64 {
    while second != 0 {
        let remainder = first % second;
        first = second;
        second = remainder;
    }
    first
}

fn page_cell_extent(keys: &[DatasetResourceKey], axis: usize) -> u64 {
    let maximum_shape = keys
        .iter()
        .map(|key| key.region().shape().dimensions()[axis])
        .max()
        .unwrap_or(1);
    let origin_gcd = keys
        .iter()
        .map(|key| key.region().origin()[axis])
        .fold(0, greatest_common_divisor);
    // Prefer the semantic brick extent, but retain a finer observed grid when
    // a layer genuinely contains multiple sub-bricks inside that extent. A
    // zero/global anchor makes ordinary sliding cohorts independent of the
    // body's current minimum coordinate.
    match origin_gcd {
        0 => maximum_shape,
        observed => observed.min(maximum_shape).max(1),
    }
}

fn page_layout_geometry(
    keys: &[DatasetResourceKey],
) -> Result<([u64; 3], [u64; 3], usize), WgpuRenderRuntimeError> {
    if keys.is_empty() {
        return Ok(([0; 3], [1; 3], 0));
    }
    let origin_zyx = [0; 3];
    let cell_zyx = std::array::from_fn(|axis| page_cell_extent(keys, axis));
    let mut occupied_count = 0_usize;
    for key in keys {
        let start: [u64; 3] = std::array::from_fn(|axis| {
            key.region().origin()[axis].saturating_sub(origin_zyx[axis]) / cell_zyx[axis]
        });
        let end: [u64; 3] = std::array::from_fn(|axis| {
            key.region().end_exclusive()[axis]
                .saturating_sub(origin_zyx[axis])
                .div_ceil(cell_zyx[axis])
        });
        let cells = usize::try_from(end[0].saturating_sub(start[0]))
            .ok()
            .and_then(|z| {
                usize::try_from(end[1].saturating_sub(start[1]))
                    .ok()
                    .and_then(|y| z.checked_mul(y))
            })
            .and_then(|zy| {
                usize::try_from(end[2].saturating_sub(start[2]))
                    .ok()
                    .and_then(|x| zy.checked_mul(x))
            })
            .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
        occupied_count = occupied_count
            .checked_add(cells)
            .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
        if occupied_count > MAX_RENDER_REQUIREMENTS {
            return Err(WgpuRenderRuntimeError::ControlCapacityExceeded);
        }
    }
    Ok((origin_zyx, cell_zyx, occupied_count))
}

fn build_page_layout(
    keys: &[DatasetResourceKey],
    canonical_index_base: usize,
) -> Result<LayerPageLayout, WgpuRenderRuntimeError> {
    if keys.is_empty() {
        return Ok(LayerPageLayout {
            origin_zyx: [0; 3],
            cell_zyx: [1; 3],
            capacity: 0,
            seed: 0,
            slots: Vec::new(),
            occupied_cells: 0,
            transient_host_bytes: 0,
        });
    }
    let (origin_zyx, cell_zyx, occupied_count) = page_layout_geometry(keys)?;
    let mut resident_entries = Vec::with_capacity(occupied_count);
    for (local_index, key) in keys.iter().enumerate() {
        let resource_index = canonical_index_base
            .checked_add(local_index)
            .and_then(|index| u32::try_from(index).ok())
            .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
        let start: [u64; 3] = std::array::from_fn(|axis| {
            key.region().origin()[axis].saturating_sub(origin_zyx[axis]) / cell_zyx[axis]
        });
        let end: [u64; 3] = std::array::from_fn(|axis| {
            key.region().end_exclusive()[axis]
                .saturating_sub(origin_zyx[axis])
                .div_ceil(cell_zyx[axis])
        });
        for z in start[0]..end[0] {
            for y in start[1]..end[1] {
                for x in start[2]..end[2] {
                    resident_entries.push((
                        [u64_to_u32(x)?, u64_to_u32(y)?, u64_to_u32(z)?],
                        resource_index,
                    ));
                }
            }
        }
    }
    resident_entries.sort_unstable_by_key(|(coordinate, _)| *coordinate);
    if resident_entries
        .windows(2)
        .any(|pair| pair[0].0 == pair[1].0)
    {
        return Err(WgpuRenderRuntimeError::OverlappingResources);
    }
    if resident_entries.is_empty() {
        return Ok(LayerPageLayout {
            origin_zyx,
            cell_zyx,
            capacity: 0,
            seed: 0,
            slots: Vec::new(),
            occupied_cells: 0,
            transient_host_bytes: 0,
        });
    }
    let capacity = resident_entries
        .len()
        .checked_mul(2)
        .and_then(usize::checked_next_power_of_two)
        .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    let capacity_u32 =
        u32::try_from(capacity).map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    let mut selected = None;
    for seed in 0..MAX_PAGE_HASH_SEEDS {
        let mut slots = vec![[0_u32; 4]; capacity];
        let mut fits = true;
        for (coordinate, resource_index) in &resident_entries {
            let mut slot = page_hash(*coordinate, seed) as usize & (capacity - 1);
            let mut inserted = false;
            for _ in 0..MAX_PAGE_HASH_PROBES {
                if slots[slot][3] == 0 {
                    slots[slot] = [
                        coordinate[0],
                        coordinate[1],
                        coordinate[2],
                        resource_index
                            .checked_add(1)
                            .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?,
                    ];
                    inserted = true;
                    break;
                }
                slot = (slot + 1) & (capacity - 1);
            }
            if !inserted {
                fits = false;
                break;
            }
        }
        if fits {
            selected = Some((seed, slots));
            break;
        }
    }
    let (seed, slots) = selected.ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    let transient_host_bytes = (resident_entries.capacity() as u64)
        .saturating_mul(size_of::<([u32; 3], u32)>() as u64)
        .saturating_add((slots.capacity() as u64).saturating_mul(size_of::<[u32; 4]>() as u64));
    Ok(LayerPageLayout {
        origin_zyx,
        cell_zyx,
        capacity: capacity_u32,
        seed,
        slots,
        occupied_cells: u32::try_from(resident_entries.len())
            .map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?,
        transient_host_bytes,
    })
}

fn inverse_affine_rows(matrix: [f64; 16]) -> Result<[f64; 12], WgpuRenderRuntimeError> {
    let a = matrix[0];
    let b = matrix[1];
    let c = matrix[2];
    let d = matrix[4];
    let e = matrix[5];
    let f = matrix[6];
    let g = matrix[8];
    let h = matrix[9];
    let i = matrix[10];
    let determinant = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if !determinant.is_finite() || determinant.abs() <= f64::EPSILON {
        return Err(WgpuRenderRuntimeError::UnsupportedView);
    }
    let inverse_determinant = determinant.recip();
    let linear = [
        (e * i - f * h) * inverse_determinant,
        (c * h - b * i) * inverse_determinant,
        (b * f - c * e) * inverse_determinant,
        (f * g - d * i) * inverse_determinant,
        (a * i - c * g) * inverse_determinant,
        (c * d - a * f) * inverse_determinant,
        (d * h - e * g) * inverse_determinant,
        (b * g - a * h) * inverse_determinant,
        (a * e - b * d) * inverse_determinant,
    ];
    if !linear.iter().all(|value| value.is_finite()) {
        return Err(WgpuRenderRuntimeError::UnsupportedView);
    }
    let translation = [matrix[3], matrix[7], matrix[11]];
    Ok([
        linear[0],
        linear[1],
        linear[2],
        -(linear[0] * translation[0] + linear[1] * translation[1] + linear[2] * translation[2]),
        linear[3],
        linear[4],
        linear[5],
        -(linear[3] * translation[0] + linear[4] * translation[1] + linear[5] * translation[2]),
        linear[6],
        linear[7],
        linear[8],
        -(linear[6] * translation[0] + linear[7] * translation[1] + linear[8] * translation[2]),
    ])
}

fn fused_dvr_compatible(words: &[u32], layer_count: usize) -> bool {
    if layer_count == 0 {
        return false;
    }
    let first = HEADER_WORDS;
    words[first + 18] == 1
        && words[first + 56] == 0
        && (1..layer_count).all(|layer_index| {
            let current = HEADER_WORDS + layer_index * LAYER_WORDS;
            words[current + 18] == 1
                && words[current + 56] == 0
                && words[current + 1..current + 4] == words[first + 1..first + 4]
                && words[current + 24] == words[first + 24]
                && words[current + 32..current + 56] == words[first + 32..first + 56]
        })
}

fn build_control(
    catalog: &DatasetCatalog,
    intent: &RenderIntent,
    demand_keys: &[DatasetResourceKey],
    resource_keys: &[DatasetResourceKey],
    resident: &BTreeMap<DatasetResourceKey, ResidentResource>,
) -> Result<(Vec<u8>, usize), WgpuRenderRuntimeError> {
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
    let mut demand_keys_by_layer: BTreeMap<LogicalLayerKey, Vec<DatasetResourceKey>> =
        BTreeMap::new();
    for key in demand_keys {
        demand_keys_by_layer
            .entry(key.layer())
            .or_default()
            .push(*key);
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
        let inverse_transform = inverse_affine_rows(transform)?;
        let shape = scale.shape().dimensions();
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
        record[4] = f64_to_f32(inverse_transform[0])?.to_bits();
        record[5] = f64_to_f32(inverse_transform[5])?.to_bits();
        record[6] = f64_to_f32(inverse_transform[10])?.to_bits();
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
        for (index, value) in inverse_transform.into_iter().enumerate() {
            record[32 + index] = f64_to_f32(value)?.to_bits();
        }
        for row in 0..3 {
            for column in 0..4 {
                record[44 + row * 4 + column] = f64_to_f32(transform[row * 4 + column])?.to_bits();
            }
        }
        record[56] = match state.sampling_policy() {
            SamplingPolicy::VoxelExact => 0,
            SamplingPolicy::SmoothLinear => 1,
        };
        record[57] = state.iso_parameters().map_or(0, |parameters| {
            u32::from(parameters.shading_policy() == IsoShadingPolicy::GradientLighting)
        });
        if let RenderViewIntent::Volume { camera, iso_light } = intent.view()
            && let Some([screen_x, screen_y]) = iso_light.detached_screen_position()
        {
            let axes = CameraFrame::new(camera, intent.presentation())
                .map_err(|_| WgpuRenderRuntimeError::UnsupportedView)?
                .axes();
            let disc_z = (1.0_f64
                - f64::from(screen_x) * f64::from(screen_x)
                - f64::from(screen_y) * f64::from(screen_y))
            .max(0.0)
            .sqrt();
            let direction: [f64; 3] = std::array::from_fn(|axis| {
                axes.right()[axis] * f64::from(screen_x) + axes.up()[axis] * f64::from(screen_y)
                    - axes.forward()[axis] * disc_z
            });
            record[58] = 1;
            for (axis, value) in direction.into_iter().enumerate() {
                record[59 + axis] = f64_to_f32(value)?.to_bits();
            }
        }
        words.extend_from_slice(&record);
    }
    words[27] = u32::from(fused_dvr_compatible(&words, intent.layers().len()));

    // Camera/transfer-only updates need exactly this header/layer prefix.
    // Returning before resource occupancy construction keeps retained
    // navigation O(layer-count) and prevents rebuilding even a tiny throwaway
    // page hash merely to discard it below.
    if resource_keys.is_empty() {
        return Ok((bytemuck::cast_slice::<u32, u8>(&words).to_vec(), 0));
    }

    words[7] =
        u32::try_from(words.len()).map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    // Requirement preflight permits exactly one scale per layer, so every key
    // named in progress is encoded into the submitted control buffer. Never
    // count a cached-but-omitted scale as covered.
    words[1] = u32::try_from(resource_keys.len())
        .map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    for key in resource_keys {
        let record = resource_record(*key, resident.get(key))?;
        words.extend_from_slice(&record);
    }
    let page_table_offset = words.len();
    words[26] = u32::try_from(page_table_offset)
        .map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    let mut page_layouts = Vec::with_capacity(intent.layers().len());
    for layer_intent in intent.layers() {
        let layer_keys = demand_keys_by_layer
            .get(&layer_intent.layer())
            .map_or(&[][..], Vec::as_slice);
        let canonical_index_base = layer_keys.first().map_or(0, |first| {
            resource_keys
                .binary_search(first)
                .expect("demand keys are the canonical resource keys")
        });
        let layout = build_page_layout(layer_keys, canonical_index_base)?;
        let layer_index = page_layouts.len();
        let record_start = HEADER_WORDS + layer_index * LAYER_WORDS;
        words[record_start + 26] = u64_to_u32(layout.origin_zyx[2])?;
        words[record_start + 27] = u64_to_u32(layout.origin_zyx[1])?;
        words[record_start + 28] = u64_to_u32(layout.origin_zyx[0])?;
        words[record_start + 29] = u64_to_u32(layout.cell_zyx[2])?;
        words[record_start + 30] = u64_to_u32(layout.cell_zyx[1])?;
        words[record_start + 31] = u64_to_u32(layout.cell_zyx[0])?;
        page_layouts.push(layout);
    }
    // Each layer points to one bounded sparse hash: capacity, seed, then
    // exact (x,y,z,resource+1) slots. Its size is O(resident demand), never
    // the volume of a sparse demand bounding box.
    for (layer_index, layout) in page_layouts.into_iter().enumerate() {
        let record_start = HEADER_WORDS + layer_index * LAYER_WORDS;
        let offset = words.len();
        words[record_start + 25] =
            u32::try_from(offset).map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;
        words.extend([layout.capacity, layout.seed]);
        words.extend(layout.slots.into_iter().flatten());
    }
    let bytes = bytemuck::cast_slice::<u32, u8>(&words).to_vec();
    if bytes.len() as u64 > MAX_CONTROL_UPLOAD_BYTES {
        return Err(WgpuRenderRuntimeError::ControlCapacityExceeded);
    }
    Ok((bytes, intent.layers().len()))
}

fn resource_record(
    key: DatasetResourceKey,
    resource: Option<&ResidentResource>,
) -> Result<[u32; RESOURCE_WORDS], WgpuRenderRuntimeError> {
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
    record[11] = key.scale().get();
    if let Some(resource) = resource {
        record[7] = u64_to_u32(resource.offset)?;
        record[8] = resource.validity_offset.map_or(Ok(u32::MAX), u64_to_u32)?;
        record[9] = resource.dtype_bytes;
        record[10] = resource.segment;
        record[12] = resource.minimum.to_bits();
        record[13] = resource.maximum.to_bits();
        record[14] = u32::from(resource.any_valid);
        record[15] = u32::from(resource.all_valid);
    } else {
        // Dtype zero is the stable-slot missing sentinel. Empty/all-invalid
        // resident resources retain their real dtype and remain distinct.
        record[8] = u32::MAX;
    }
    Ok(record)
}

/// Re-encodes only the camera, viewport, transfer, and mode prefix while
/// retaining the resource records and page layouts already resident in the
/// presentation-owned control buffer. Work is bounded by the layer count;
/// it never traverses the (potentially 65k-entry) requirement set.
fn build_dynamic_control_prefix(
    layout: &PreparedStaticPresentationLayout,
    catalog: &DatasetCatalog,
    intent: &RenderIntent,
) -> Result<Vec<u8>, WgpuRenderRuntimeError> {
    let prefix_bytes = (HEADER_WORDS + intent.layers().len() * LAYER_WORDS) * 4;
    if layout.layer_page_fields.len() != intent.layers().len() {
        return Err(WgpuRenderRuntimeError::PayloadContractMismatch);
    }
    let (generated, constructed_layouts) =
        build_control(catalog, intent, &layout.scale_keys, &[], &BTreeMap::new())?;
    debug_assert_eq!(constructed_layouts, 0);
    let mut prefix = generated
        .get(..prefix_bytes)
        .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?
        .to_vec();
    let prefix_words = bytemuck::try_cast_slice_mut::<u8, u32>(&mut prefix)
        .map_err(|_| WgpuRenderRuntimeError::PayloadContractMismatch)?;

    prefix_words[1] = u32::try_from(layout.resource_count())
        .map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    prefix_words[7] = u32::try_from(HEADER_WORDS + intent.layers().len() * LAYER_WORDS)
        .map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    prefix_words[26] = u32::try_from(layout.page_table_offset / 4)
        .map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    for (layer_index, fields) in layout.layer_page_fields.iter().enumerate() {
        let start = HEADER_WORDS + layer_index * LAYER_WORDS;
        prefix_words[start + 25..start + 32].copy_from_slice(fields);
    }
    Ok(prefix)
}

fn set_control_full_coverage(control: &mut [u8], full: bool) -> Result<(), WgpuRenderRuntimeError> {
    let words = bytemuck::try_cast_slice_mut::<u8, u32>(control)
        .map_err(|_| WgpuRenderRuntimeError::PayloadContractMismatch)?;
    let flag = words
        .get_mut(28)
        .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
    *flag = u32::from(full);
    Ok(())
}

struct ControlWrite {
    offset: u64,
    bytes: Vec<u8>,
}

struct BodyDeltaControlWrites {
    writes: Vec<ControlWrite>,
    empty_keys: Vec<DatasetResourceKey>,
}

fn resource_record_offset(layer_count: usize, resource_index: u32) -> u64 {
    ((HEADER_WORDS + layer_count * LAYER_WORDS) as u64
        + u64::from(resource_index) * RESOURCE_WORDS as u64)
        * 4
}

fn build_incremental_control_writes(
    prefix: Option<&[u8]>,
    layer_count: usize,
    layout: &PreparedStaticPresentationLayout,
    changed_keys: &BTreeSet<DatasetResourceKey>,
    resident: &BTreeMap<DatasetResourceKey, ResidentResource>,
) -> Result<Option<Vec<ControlWrite>>, WgpuRenderRuntimeError> {
    let mut writes = prefix.map_or_else(Vec::new, |prefix| {
        vec![ControlWrite {
            offset: 0,
            bytes: prefix.to_vec(),
        }]
    });
    for key in changed_keys.iter().copied() {
        let Some(index) = layout.resource_slot(key) else {
            continue;
        };
        let offset = resource_record_offset(layer_count, index);
        let record = resource_record(key, resident.get(&key))?;
        let bytes = bytemuck::cast_slice::<u32, u8>(&record);
        let merges_previous = writes
            .last()
            .is_some_and(|previous| previous.offset + previous.bytes.len() as u64 == offset);
        if merges_previous {
            writes
                .last_mut()
                .expect("a mergeable control write exists")
                .bytes
                .extend_from_slice(bytes);
        } else {
            if writes.len() == MAX_CONTROL_PUBLICATION_WRITES_PER_FRAME {
                return Ok(None);
            }
            writes.push(ControlWrite {
                offset,
                bytes: bytes.to_vec(),
            });
        }
    }
    Ok(Some(writes))
}

fn append_control_write(writes: &mut Vec<ControlWrite>, offset: u64, bytes: &[u8]) -> bool {
    if writes
        .last_mut()
        .is_some_and(|previous| previous.offset + previous.bytes.len() as u64 == offset)
    {
        writes
            .last_mut()
            .expect("a contiguous control write exists")
            .bytes
            .extend_from_slice(bytes);
        return true;
    }
    if writes.len() == MAX_CONTROL_PUBLICATION_WRITES_PER_FRAME {
        return false;
    }
    writes.push(ControlWrite {
        offset,
        bytes: bytes.to_vec(),
    });
    true
}

fn incremental_control_write_count(
    layer_count: usize,
    page_table_offset: u64,
    resource_slots: &[ResourceSlotChange],
    page_patches: &[PageTablePatch],
) -> usize {
    let mut writes = 1_usize;
    let mut previous_end = 0_u64;
    for (offset, bytes) in resource_slots
        .iter()
        .map(|change| (resource_record_offset(layer_count, change.slot), 64_u64))
        .chain(
            page_patches
                .iter()
                .map(|patch| (page_table_offset + u64::from(patch.word_offset) * 4, 16_u64)),
        )
    {
        if offset != previous_end {
            writes += 1;
        }
        previous_end = offset + bytes;
    }
    writes
}

fn build_body_delta_control_writes(
    prefix: &[u8],
    layer_count: usize,
    layout: &PreparedStaticPresentationLayout,
    changed_keys: &BTreeSet<DatasetResourceKey>,
    resident: &BTreeMap<DatasetResourceKey, ResidentResource>,
) -> Result<Option<BodyDeltaControlWrites>, WgpuRenderRuntimeError> {
    let Some(delta) = layout.delta.as_ref() else {
        return Ok(None);
    };
    let mut slot_changes = delta
        .resource_slots
        .iter()
        .map(|change| (change.slot, change.key))
        .collect::<BTreeMap<_, _>>();
    for key in changed_keys {
        if let Some(slot) = layout.resource_slot(*key) {
            slot_changes.insert(slot, Some(*key));
        }
    }
    let mut writes = vec![ControlWrite {
        offset: 0,
        bytes: prefix.to_vec(),
    }];
    let mut empty_keys = Vec::new();
    for (slot, key) in slot_changes {
        let record = if let Some(key) = key {
            if resident
                .get(&key)
                .is_some_and(|resource| !resource.any_valid)
            {
                empty_keys.push(key);
            }
            resource_record(key, resident.get(&key))?
        } else {
            [0; RESOURCE_WORDS]
        };
        if !append_control_write(
            &mut writes,
            resource_record_offset(layer_count, slot),
            bytemuck::cast_slice::<u32, u8>(&record),
        ) {
            return Ok(None);
        }
    }
    for patch in delta.page_patches.iter() {
        let offset = layout
            .page_table_offset
            .checked_add(u64::from(patch.word_offset).saturating_mul(4))
            .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
        if !append_control_write(
            &mut writes,
            offset,
            bytemuck::cast_slice::<u32, u8>(&patch.words),
        ) {
            return Ok(None);
        }
    }
    Ok(Some(BodyDeltaControlWrites { writes, empty_keys }))
}

fn build_static_control_writes(
    prefix: Vec<u8>,
    layer_count: usize,
    layout: &PreparedStaticPresentationLayout,
    base_resident: &BTreeMap<DatasetResourceKey, ResidentResource>,
    evicted: &BTreeSet<DatasetResourceKey>,
    uploaded: &BTreeMap<DatasetResourceKey, ResidentResource>,
    empty: &BTreeMap<DatasetResourceKey, ResidentResource>,
) -> Result<(Vec<ControlWrite>, Vec<DatasetResourceKey>), WgpuRenderRuntimeError> {
    let mut writes = vec![ControlWrite {
        offset: 0,
        bytes: prefix,
    }];
    let requirement_keys = layout.requirements.canonical();
    let record_bytes = usize::try_from(layout.record_capacity)
        .map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?
        .checked_mul(RESOURCE_WORDS)
        .and_then(|words| words.checked_mul(size_of::<u32>()))
        .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    let mut dense_records = vec![0_u8; record_bytes];
    let mut empty_keys = Vec::new();
    let mut base_iter = base_resident.iter().peekable();
    let mut evicted_iter = evicted.iter().peekable();
    let mut uploaded_iter = uploaded.iter().peekable();
    let mut empty_iter = empty.iter().peekable();

    debug_assert!(requirement_keys.windows(2).all(|pair| pair[0] < pair[1]));

    // The immutable requirement body and all residency sources are sorted.
    // Advancing each source monotonically intersects them in O(N + R + U + E)
    // time without a temporary resident tree or a binary search per record.
    for key in requirement_keys.iter().copied() {
        while base_iter
            .peek()
            .is_some_and(|(candidate, _)| **candidate < key)
        {
            base_iter.next();
        }
        while evicted_iter
            .peek()
            .is_some_and(|candidate| **candidate < key)
        {
            evicted_iter.next();
        }
        while uploaded_iter
            .peek()
            .is_some_and(|(candidate, _)| **candidate < key)
        {
            uploaded_iter.next();
        }
        while empty_iter
            .peek()
            .is_some_and(|(candidate, _)| **candidate < key)
        {
            empty_iter.next();
        }

        let uploaded_resource = uploaded_iter
            .peek()
            .filter(|(candidate, _)| **candidate == key)
            .map(|(_, resource)| *resource);
        let empty_resource = empty_iter
            .peek()
            .filter(|(candidate, _)| **candidate == key)
            .map(|(_, resource)| *resource);
        let base_is_evicted = evicted_iter
            .peek()
            .is_some_and(|candidate| **candidate == key);
        let base_resource = (!base_is_evicted)
            .then(|| {
                base_iter
                    .peek()
                    .filter(|(candidate, _)| **candidate == key)
                    .map(|(_, resource)| *resource)
            })
            .flatten();
        let Some(resource) = uploaded_resource.or(empty_resource).or(base_resource) else {
            continue;
        };

        if !resource.any_valid {
            empty_keys.push(key);
        }
        let index = layout
            .resource_slot(key)
            .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
        let record = resource_record(key, Some(resource))?;
        let bytes = bytemuck::cast_slice::<u32, u8>(&record);
        let byte_offset = index as usize * RESOURCE_WORDS * size_of::<u32>();
        dense_records[byte_offset..byte_offset + bytes.len()].copy_from_slice(bytes);
    }
    // Missing records remain zero-filled (dtype zero is the shader sentinel).
    // Keeping the prefix separate lets presentation state retain only its small
    // dynamic bytes while the complete record body is one bounded queue write.
    writes.push(ControlWrite {
        offset: resource_record_offset(layer_count, 0),
        bytes: dense_records,
    });
    Ok((writes, empty_keys))
}

fn encode_view(words: &mut [u32], intent: &RenderIntent) -> Result<(), WgpuRenderRuntimeError> {
    match intent.view() {
        RenderViewIntent::Volume { camera, .. } => {
            words[3] = 0;
            let frame = CameraFrame::new(camera, intent.presentation())
                .map_err(|_| WgpuRenderRuntimeError::UnsupportedView)?;
            let axes = frame.axes();
            let width = f64::from(intent.extent().width_pixels());
            let height = f64::from(intent.extent().height_pixels());
            let presentation = intent.presentation();
            let screen_x = (0.5 / width - 0.5) * presentation.width_points();
            let screen_y = (0.5 - 0.5 / height) * presentation.height_points();
            let screen_dx = presentation.width_points() / width;
            let screen_dy = -presentation.height_points() / height;
            let forward = axes.forward();
            let right = axes.right();
            let up = axes.up();
            let (origin, origin_dx, origin_dy, direction, direction_dx, direction_dy) = match camera
                .projection()
            {
                Projection::Orthographic => {
                    let scale = camera.orthographic_world_per_screen_point();
                    let eye = frame.eye().components();
                    (
                        std::array::from_fn(|axis| {
                            eye[axis] + right[axis] * screen_x * scale + up[axis] * screen_y * scale
                        }),
                        right.map(|value| value * screen_dx * scale),
                        up.map(|value| value * screen_dy * scale),
                        forward,
                        [0.0; 3],
                        [0.0; 3],
                    )
                }
                Projection::Perspective => {
                    let inverse_focal = camera.perspective_focal_length_screen_points().recip();
                    (
                        frame.eye().components(),
                        [0.0; 3],
                        [0.0; 3],
                        std::array::from_fn(|axis| {
                            forward[axis]
                                + right[axis] * screen_x * inverse_focal
                                + up[axis] * screen_y * inverse_focal
                        }),
                        right.map(|value| value * screen_dx * inverse_focal),
                        up.map(|value| value * screen_dy * inverse_focal),
                    )
                }
            };
            write_vec3(words, 8, origin)?;
            write_vec3(words, 11, origin_dx)?;
            write_vec3(words, 14, origin_dy)?;
            write_vec3(words, 17, direction)?;
            write_vec3(words, 20, direction_dx)?;
            write_vec3(words, 23, direction_dy)?;
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
    segment: u32,
    offset: u64,
    allocated_bytes: u64,
    validity_offset: Option<u64>,
    dtype_bytes: u32,
    minimum: f32,
    maximum: f32,
    any_valid: bool,
    all_valid: bool,
    last_used_frame: u64,
}

fn empty_resident_resource(
    payload: ResourcePayloadView<'_>,
    facts: ResourcePayloadFacts,
    last_used_frame: u64,
) -> Option<ResidentResource> {
    (!facts.any_valid()).then(|| ResidentResource {
        // Empty pages are represented entirely by control-buffer facts. The
        // shader tests `any_valid` before touching the payload arena, so no
        // dummy slot, value bytes, or validity mask need to be uploaded.
        segment: 0,
        offset: 0,
        allocated_bytes: 0,
        validity_offset: None,
        dtype_bytes: u32::from(payload.dtype().bytes_per_sample()),
        minimum: facts.minimum(),
        maximum: facts.maximum(),
        any_valid: false,
        all_valid: false,
        last_used_frame,
    })
}

struct DisplayTarget {
    color_texture: wgpu::Texture,
    color_view: wgpu::TextureView,
    fact_texture: Option<wgpu::Texture>,
    fact_view: Option<wgpu::TextureView>,
    extent: RenderExtent,
    allocated_bytes: u64,
}

/// One fully uploadable retained cohort in exact first-seen ranking order.
///
/// The map owns/deduplicates leases while the deque is the scheduling
/// authority. Both count and bytes that can actually enter transfer staging
/// are bounded before mutation, so repeated calls during GPU backpressure
/// cannot grow an unbounded upload cohort or silently fall back to semantic-key
/// order. Metadata-only all-invalid pages remain count bounded but consume no
/// transfer bytes, matching frame planning and the staging allocator.
struct PendingLeaseQueue {
    by_key: BTreeMap<DatasetResourceKey, Arc<dyn ResourceLease>>,
    order: VecDeque<DatasetResourceKey>,
    transfer_bytes: u64,
}

fn pending_lease_transfer_bytes(lease: &dyn ResourceLease) -> u64 {
    if lease.payload_facts().any_valid() {
        lease.payload().byte_len()
    } else {
        0
    }
}

impl PendingLeaseQueue {
    fn new() -> Self {
        Self {
            by_key: BTreeMap::new(),
            order: VecDeque::new(),
            transfer_bytes: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    fn len(&self) -> usize {
        self.order.len()
    }

    fn clear(&mut self) {
        self.by_key.clear();
        self.order.clear();
        self.transfer_bytes = 0;
    }

    fn remove(&mut self, key: DatasetResourceKey) -> bool {
        let Some(lease) = self.by_key.remove(&key) else {
            return false;
        };
        self.transfer_bytes = self
            .transfer_bytes
            .saturating_sub(pending_lease_transfer_bytes(lease.as_ref()));
        if let Some(index) = self.order.iter().position(|pending| *pending == key) {
            self.order.remove(index);
        } else {
            debug_assert!(false, "pending lease map/order must remain coherent");
        }
        true
    }

    fn retain(&mut self, mut keep: impl FnMut(DatasetResourceKey) -> bool) {
        let removed = self
            .order
            .iter()
            .copied()
            .filter(|key| !keep(*key))
            .collect::<Vec<_>>();
        for key in removed {
            self.remove(key);
        }
    }

    /// Atomically admits unique, nonresident updates in caller order. `false`
    /// means the existing cohort remains byte-for-byte unchanged and the
    /// caller must retry the same ranked updates after it drains.
    fn try_extend(
        &mut self,
        updates: &[Arc<dyn ResourceLease>],
    ) -> Result<bool, WgpuRenderRuntimeError> {
        let mut batch_keys = BTreeSet::new();
        let mut additional = Vec::new();
        let mut additional_bytes = 0_u64;
        for lease in updates {
            let key = lease.key();
            if self.by_key.contains_key(&key) || !batch_keys.insert(key) {
                continue;
            }
            let bytes = pending_lease_transfer_bytes(lease.as_ref());
            additional_bytes = additional_bytes.checked_add(bytes).ok_or(
                WgpuRenderRuntimeError::CapacityExceeded {
                    category: GpuLedgerCategory::TransferStaging,
                    requested_bytes: u64::MAX,
                    available_bytes: MAX_PAYLOAD_UPLOAD_BYTES,
                },
            )?;
            additional.push(Arc::clone(lease));
        }

        if additional.len() > MAX_UPLOADS {
            return Err(WgpuRenderRuntimeError::LeaseCapacityExceeded {
                actual: additional.len(),
                maximum: MAX_UPLOADS,
            });
        }
        if additional_bytes > MAX_PAYLOAD_UPLOAD_BYTES {
            return Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::TransferStaging,
                requested_bytes: additional_bytes,
                available_bytes: MAX_PAYLOAD_UPLOAD_BYTES,
            });
        }
        let combined_count = self.len().saturating_add(additional.len());
        let combined_bytes = self.transfer_bytes.saturating_add(additional_bytes);
        if combined_count > MAX_UPLOADS || combined_bytes > MAX_PAYLOAD_UPLOAD_BYTES {
            return Ok(false);
        }

        for lease in additional {
            let key = lease.key();
            self.transfer_bytes += pending_lease_transfer_bytes(lease.as_ref());
            let previous = self.by_key.insert(key, lease);
            debug_assert!(previous.is_none());
            self.order.push_back(key);
        }
        Ok(true)
    }

    fn ordered_leases(&self) -> Vec<Arc<dyn ResourceLease>> {
        self.order
            .iter()
            .map(|key| {
                Arc::clone(
                    self.by_key
                        .get(key)
                        .expect("pending lease order points to owned lease"),
                )
            })
            .collect()
    }
}

fn validate_retained_frame_preflight(
    current_frame_state: Option<&FrameState>,
    intent: &RenderIntent,
    requirements: &RenderRequirements,
) -> Result<(), WgpuRenderRuntimeError> {
    if intent.frame() != requirements.frame() {
        return Err(WgpuRenderRuntimeError::FrameContractMismatch);
    }
    if let Some(current) = current_frame_state {
        if intent.frame() < current.frame {
            return Err(WgpuRenderRuntimeError::StaleFrame {
                actual: intent.frame(),
                current: current.frame,
            });
        }
        if intent.frame() == current.frame
            && !current.requirements.shares_resources_with(requirements)
        {
            return Err(WgpuRenderRuntimeError::RequirementSetChanged);
        }
    }
    Ok(())
}

struct PresentationState {
    frame_state: Option<FrameState>,
    last_rendered_frame: Option<FrameIdentity>,
    last_rendered_timepoint: Option<TimeIndex>,
    last_rendered_volume: bool,
    last_rendered_layers: Vec<LogicalLayerKey>,
    last_rendered_modes: Vec<u32>,
    last_rendered_sampling: Vec<SamplingPolicy>,
    availability: Option<FrameCoverage>,
    last_progress: Option<FrameProgress>,
    /// Exact subset of this presentation's retained control keys whose global
    /// residency changed since its last rendered image. Keeping the subset at
    /// mutation time makes navigation and pick preflight O(1) instead of
    /// replaying a process-wide change log.
    residency_dirty_keys: BTreeSet<DatasetResourceKey>,
    control_buffer: wgpu::Buffer,
    control_capacity: u64,
    control_prefix: Vec<u8>,
    control_layout: Option<PreparedStaticPresentationLayout>,
    control_requirements: Option<RenderRequirements>,
    pending_leases: PendingLeaseQueue,
    render_bind_group: wgpu::BindGroup,
    pick_bind_groups: Vec<wgpu::BindGroup>,
    display: DisplayTarget,
    pending_capture: Option<PendingCapture>,
}

fn uses_mip_pipeline(intent: &RenderIntent) -> bool {
    matches!(intent.view(), RenderViewIntent::Volume { .. })
        && intent
            .layers()
            .iter()
            .all(|layer| layer.render_state().mip_parameters().is_some())
}

type MapState = Arc<Mutex<Option<Result<(), ()>>>>;

enum UploadStagingState {
    Mapped,
    Encoding,
    Pending(MapState),
}

struct UploadStagingSlot {
    buffer: wgpu::Buffer,
    capacity: u64,
    state: UploadStagingState,
}

struct UploadStagingPool {
    slots: Vec<UploadStagingSlot>,
    capacity: u64,
    reserved: u64,
}

impl UploadStagingPool {
    fn new(capacity: u64) -> Self {
        Self {
            slots: Vec::new(),
            capacity,
            reserved: 0,
        }
    }

    fn refresh(&mut self) -> Result<(), WgpuRenderRuntimeError> {
        for slot in &mut self.slots {
            let UploadStagingState::Pending(mapped) = &slot.state else {
                continue;
            };
            let status = mapped
                .lock()
                .map_err(|_| WgpuRenderRuntimeError::BackendValidation)?
                .to_owned();
            match status {
                None => {}
                Some(Ok(())) => slot.state = UploadStagingState::Mapped,
                Some(Err(())) => return Err(WgpuRenderRuntimeError::BackendValidation),
            }
        }
        Ok(())
    }

    /// Returns the slot and any newly reserved allocation bytes. `None` is a
    /// transient staging backpressure condition, never an invitation to grow
    /// beyond the configured transfer ledger.
    fn acquire(
        &mut self,
        device: &wgpu::Device,
        required: u64,
    ) -> Result<Option<(usize, u64)>, WgpuRenderRuntimeError> {
        let required = align_copy(required);
        if required > self.capacity {
            return Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::TransferStaging,
                requested_bytes: required,
                available_bytes: self.capacity,
            });
        }
        if let Some(index) = self.slots.iter().position(|slot| {
            matches!(slot.state, UploadStagingState::Mapped) && slot.capacity >= required
        }) {
            return Ok(Some((index, 0)));
        }

        // Prefer replacing an idle undersized slot over accumulating buffers.
        if let Some(index) = self.slots.iter().position(|slot| {
            matches!(slot.state, UploadStagingState::Mapped)
                && self
                    .reserved
                    .saturating_sub(slot.capacity)
                    .saturating_add(required)
                    <= self.capacity
        }) {
            let old = self.slots[index].capacity;
            self.slots[index] = UploadStagingSlot {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("mirante4d-upload-staging-slot"),
                    size: required,
                    usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: true,
                }),
                capacity: required,
                state: UploadStagingState::Mapped,
            };
            self.reserved = self.reserved - old + required;
            return Ok(Some((index, required)));
        }

        if self.slots.len() == MAX_IN_FLIGHT_SUBMISSIONS
            || self.reserved.saturating_add(required) > self.capacity
        {
            return Ok(None);
        }
        let index = self.slots.len();
        self.slots.push(UploadStagingSlot {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mirante4d-upload-staging-slot"),
                size: required,
                usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: true,
            }),
            capacity: required,
            state: UploadStagingState::Mapped,
        });
        self.reserved += required;
        Ok(Some((index, required)))
    }

    fn stage(&mut self, index: usize, uploads: &[UploadPlan<'_>]) -> u64 {
        let slot = &mut self.slots[index];
        debug_assert!(matches!(slot.state, UploadStagingState::Mapped));
        let required = uploads
            .iter()
            .map(|upload| upload.layout.allocation_bytes)
            .sum::<u64>();
        let mut mapped = slot.buffer.slice(..required).get_mapped_range_mut();
        let mut staging_offset = 0_u64;
        let mut padding_zero_bytes = 0_u64;
        for upload in uploads {
            let start = staging_offset as usize;
            let value_bytes = upload.payload.value_bytes();
            let validity_bytes = upload.payload.validity_bits();
            for padding in upload_padding_ranges(
                &upload.layout,
                value_bytes.len(),
                validity_bytes.map_or(0, <[u8]>::len),
            ) {
                if !padding.is_empty() {
                    padding_zero_bytes = padding_zero_bytes
                        .saturating_add(u64::try_from(padding.len()).unwrap_or(u64::MAX));
                    mapped
                        .slice(start + padding.start..start + padding.end)
                        .fill(0);
                }
            }
            mapped
                .slice(start..start + value_bytes.len())
                .copy_from_slice(value_bytes);
            if let (Some(relative), Some(validity)) =
                (upload.layout.validity_offset, validity_bytes)
            {
                let validity_start = start + relative as usize;
                mapped
                    .slice(validity_start..validity_start + validity.len())
                    .copy_from_slice(validity);
            }
            staging_offset += upload.layout.allocation_bytes;
        }
        drop(mapped);
        slot.buffer.unmap();
        slot.state = UploadStagingState::Encoding;
        padding_zero_bytes
    }

    fn encode_copies(
        &self,
        index: usize,
        encoder: &mut wgpu::CommandEncoder,
        payload_segments: &[PayloadSegment],
        uploads: &[UploadPlan<'_>],
    ) {
        let slot = &self.slots[index];
        debug_assert!(matches!(slot.state, UploadStagingState::Encoding));
        let mut staging_offset = 0_u64;
        for upload in uploads {
            encoder.copy_buffer_to_buffer(
                &slot.buffer,
                staging_offset,
                &payload_segments[upload.segment].buffer,
                upload.offset,
                upload.layout.allocation_bytes,
            );
            staging_offset += upload.layout.allocation_bytes;
        }
    }

    fn submitted(&mut self, index: usize) {
        let slot = &mut self.slots[index];
        debug_assert!(matches!(slot.state, UploadStagingState::Encoding));
        let mapped = Arc::new(Mutex::new(None));
        let callback = Arc::clone(&mapped);
        slot.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Write, move |result| {
                if let Ok(mut status) = callback.lock() {
                    *status = Some(result.map_err(|_| ()));
                }
            });
        slot.state = UploadStagingState::Pending(mapped);
    }
}

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

struct UploadPlan<'a> {
    key: DatasetResourceKey,
    victims: Vec<DatasetResourceKey>,
    segment: usize,
    offset: u64,
    payload: ResourcePayloadView<'a>,
    layout: PayloadLayout,
    resident: ResidentResource,
}

#[derive(Clone, Copy)]
struct FrameExecutionSetup<'a> {
    prepared_layout: Option<&'a PreparedStaticPresentationLayout>,
    nonblocking_progress_collected: bool,
    cpu_timing_start: Option<Instant>,
    display_generation: u64,
    render_policy: RetainedFrameRenderPolicy,
}

struct TimingResources {
    query_set: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    timestamp_period_ns: f32,
    encoder_copy_timestamps: bool,
    slots: Vec<TimingSlot>,
    completed: BTreeMap<GpuTimingTicket, GpuFrameTiming>,
    completed_order: VecDeque<GpuTimingTicket>,
}

struct TimingSlot {
    readback: wgpu::Buffer,
    state: TimingSlotState,
}

enum TimingSlotState {
    Free,
    Pending {
        ticket: GpuTimingTicket,
        mapped: MapState,
        batch_envelope_timestamps: bool,
        payload_copy_timestamps: bool,
        render_pass_timestamps: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimingQueryLayout {
    batch_envelope: Option<(u32, u32)>,
    payload_copy: Option<(u32, u32)>,
    render_pass: Option<(u32, u32)>,
    query_count: u32,
}

impl TimingQueryLayout {
    fn new(query_base: u32, batch_envelope: bool, payload_copy: bool, render_pass: bool) -> Self {
        let mut next = query_base;
        let batch_envelope = batch_envelope.then(|| {
            let pair = (next, next + 1);
            next += 2;
            pair
        });
        let payload_copy = payload_copy.then(|| {
            let pair = (next, next + 1);
            next += 2;
            pair
        });
        let render_pass = render_pass.then(|| {
            let pair = (next, next + 1);
            next += 2;
            pair
        });
        Self {
            batch_envelope,
            payload_copy,
            render_pass,
            query_count: next - query_base,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimingPlan {
    slot: usize,
    ticket: GpuTimingTicket,
    queries: TimingQueryLayout,
}

struct PickResources {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
    slots: Vec<PickSlot>,
}

struct PickSlot {
    query_buffer: wgpu::Buffer,
    output_buffer: wgpu::Buffer,
    readback: wgpu::Buffer,
    state: PickSlotState,
}

fn record_product_creation(creations: &mut u64) {
    *creations = creations.saturating_add(1);
}

fn create_counted_render_pipeline(
    device: &wgpu::Device,
    creations: &mut u64,
    descriptor: &wgpu::RenderPipelineDescriptor<'_>,
) -> wgpu::RenderPipeline {
    record_product_creation(creations);
    device.create_render_pipeline(descriptor)
}

fn create_counted_compute_pipeline(
    device: &wgpu::Device,
    creations: &mut u64,
    descriptor: &wgpu::ComputePipelineDescriptor<'_>,
) -> wgpu::ComputePipeline {
    record_product_creation(creations);
    device.create_compute_pipeline(descriptor)
}

fn create_counted_bind_group(
    device: &wgpu::Device,
    creations: &mut u64,
    descriptor: &wgpu::BindGroupDescriptor<'_>,
) -> wgpu::BindGroup {
    record_product_creation(creations);
    device.create_bind_group(descriptor)
}

enum PickSlotState {
    Free,
    Pending {
        ticket: VolumePickTicket,
        query: VolumePickQuery,
        mapped: MapState,
    },
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

fn create_timing_resources(device: &wgpu::Device, queue: &wgpu::Queue) -> TimingResources {
    let query_count =
        u32::try_from(TIMING_SLOT_COUNT).expect("timing slot count fits u32") * TIMING_QUERY_WORDS;
    let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
        label: Some("mirante4d-vp00-frame-timestamps"),
        ty: wgpu::QueryType::Timestamp,
        count: query_count,
    });
    let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mirante4d-vp00-timestamp-resolve"),
        size: TIMING_RESOLVE_STRIDE * TIMING_SLOT_COUNT as u64,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let slots = (0..TIMING_SLOT_COUNT)
        .map(|_| TimingSlot {
            readback: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mirante4d-vp00-timestamp-readback"),
                size: TIMING_QUERY_BYTES,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            state: TimingSlotState::Free,
        })
        .collect();
    TimingResources {
        query_set,
        resolve_buffer,
        timestamp_period_ns: queue.get_timestamp_period(),
        encoder_copy_timestamps: device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS),
        slots,
        completed: BTreeMap::new(),
        completed_order: VecDeque::new(),
    }
}

fn create_pick_resources(
    device: &wgpu::Device,
    segment_capacities: &[u64; MAX_PAYLOAD_SEGMENTS],
    pipeline_creations: &mut u64,
) -> PickResources {
    let mut layout_entries = vec![storage_layout_entry_for_stage(
        0,
        INITIAL_CONTROL_BYTES,
        true,
        wgpu::ShaderStages::COMPUTE,
    )];
    layout_entries.extend(
        segment_capacities
            .iter()
            .enumerate()
            .map(|(index, capacity)| {
                storage_layout_entry_for_stage(
                    1 + index as u32,
                    (*capacity).max(COPY_ALIGNMENT),
                    true,
                    wgpu::ShaderStages::COMPUTE,
                )
            }),
    );
    layout_entries.push(storage_layout_entry_for_stage(
        5,
        PICK_QUERY_BYTES,
        true,
        wgpu::ShaderStages::COMPUTE,
    ));
    layout_entries.push(storage_layout_entry_for_stage(
        6,
        PICK_OUTPUT_BYTES,
        false,
        wgpu::ShaderStages::COMPUTE,
    ));
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("mirante4d-vp05-pick-bind-group-layout"),
        entries: &layout_entries,
    });
    let shader_source = format!(
        "{}\n{}",
        include_str!("shader.wgsl"),
        include_str!("pick_shader.wgsl")
    );
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("mirante4d-vp05-pick-shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Owned(shader_source)),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("mirante4d-vp05-pick-pipeline-layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = create_counted_compute_pipeline(
        device,
        pipeline_creations,
        &wgpu::ComputePipelineDescriptor {
            label: Some("mirante4d-vp05-pick-pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("pick_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        },
    );
    let slots = create_pick_slots(device);
    PickResources {
        layout,
        pipeline,
        slots,
    }
}

fn create_pick_slots(device: &wgpu::Device) -> Vec<PickSlot> {
    (0..PICK_SLOT_COUNT)
        .map(|_| {
            let query_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mirante4d-vp05-pick-query"),
                size: PICK_QUERY_BYTES,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mirante4d-vp05-pick-output"),
                size: PICK_OUTPUT_BYTES,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mirante4d-vp05-pick-readback"),
                size: PICK_OUTPUT_BYTES,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            PickSlot {
                query_buffer,
                output_buffer,
                readback,
                state: PickSlotState::Free,
            }
        })
        .collect()
}

fn create_presentation_pick_bind_groups(
    device: &wgpu::Device,
    pick: &PickResources,
    payload_segments: &[PayloadSegment],
    control_buffer: &wgpu::Buffer,
    bind_group_creations: &mut u64,
) -> Vec<wgpu::BindGroup> {
    pick.slots
        .iter()
        .map(|slot| {
            let mut entries = vec![wgpu::BindGroupEntry {
                binding: 0,
                resource: control_buffer.as_entire_binding(),
            }];
            entries.extend((0..MAX_PAYLOAD_SEGMENTS).map(|index| {
                let segment = payload_segments.get(index).unwrap_or(&payload_segments[0]);
                wgpu::BindGroupEntry {
                    binding: 1 + index as u32,
                    resource: segment.buffer.as_entire_binding(),
                }
            }));
            entries.push(wgpu::BindGroupEntry {
                binding: 5,
                resource: slot.query_buffer.as_entire_binding(),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: 6,
                resource: slot.output_buffer.as_entire_binding(),
            });
            create_counted_bind_group(
                device,
                bind_group_creations,
                &wgpu::BindGroupDescriptor {
                    label: Some("mirante4d-vp05-presented-pick-bind-group"),
                    layout: &pick.layout,
                    entries: &entries,
                },
            )
        })
        .collect()
}

fn create_presentation_control_resources(
    device: &wgpu::Device,
    render_layout: &wgpu::BindGroupLayout,
    pick: &PickResources,
    payload_segments: &[PayloadSegment],
    control_capacity: u64,
    bind_group_creations: &mut u64,
) -> (wgpu::Buffer, wgpu::BindGroup, Vec<wgpu::BindGroup>) {
    let control_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mirante4d-vp01-presented-control"),
        size: control_capacity,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut render_entries = vec![wgpu::BindGroupEntry {
        binding: 0,
        resource: control_buffer.as_entire_binding(),
    }];
    render_entries.extend((0..MAX_PAYLOAD_SEGMENTS).map(|index| {
        let segment = payload_segments.get(index).unwrap_or(&payload_segments[0]);
        wgpu::BindGroupEntry {
            binding: 1 + index as u32,
            resource: segment.buffer.as_entire_binding(),
        }
    }));
    let render_bind_group = create_counted_bind_group(
        device,
        bind_group_creations,
        &wgpu::BindGroupDescriptor {
            label: Some("mirante4d-vp01-presented-render-bind-group"),
            layout: render_layout,
            entries: &render_entries,
        },
    );
    let bind_groups = create_presentation_pick_bind_groups(
        device,
        pick,
        payload_segments,
        &control_buffer,
        bind_group_creations,
    );
    (control_buffer, render_bind_group, bind_groups)
}

pub(super) struct Runtime {
    _instance: Option<wgpu::Instance>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    mip_pipeline: wgpu::RenderPipeline,
    render_bind_group_layout: wgpu::BindGroupLayout,
    payload_segments: Vec<PayloadSegment>,
    upload_staging: UploadStagingPool,
    control_capacity_bytes: u64,
    resident: BTreeMap<DatasetResourceKey, ResidentResource>,
    payload_resident_lru: [BTreeSet<(u64, DatasetResourceKey)>; MAX_PAYLOAD_SEGMENTS],
    /// Nonempty residents not pinned by any active presentation. Allocation
    /// misses walk this exact age index, so victim discovery is proportional
    /// to victims rather than to the whole residency map.
    payload_evictable_lru: [BTreeSet<(u64, DatasetResourceKey)>; MAX_PAYLOAD_SEGMENTS],
    empty_resident_count: usize,
    empty_resident_lru: BTreeSet<(u64, DatasetResourceKey)>,
    recently_evicted: BTreeMap<DatasetResourceKey, u64>,
    recently_evicted_order: VecDeque<(u64, DatasetResourceKey)>,
    next_eviction_sequence: u64,
    residency_epoch: u64,
    residency_invalidation_epoch: u64,
    presentations: BTreeMap<PresentationToken, PresentationState>,
    next_presentation: u64,
    next_capture: u64,
    next_timing: u64,
    next_pick: u64,
    timing: Option<TimingResources>,
    pick: PickResources,
    in_flight_submissions: Arc<AtomicUsize>,
    validation_error_count: Arc<AtomicUsize>,
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
        let device_descriptor = renderer_device_descriptor(&adapter, "mirante4d-wp09a-device")?;
        let (device, queue) = adapter
            .request_device(&device_descriptor)
            .await
            .map_err(|_| WgpuRenderRuntimeError::DeviceCreationFailed)?;

        Self::from_device_parts(Some(instance), &adapter, device, queue, config, None)
    }

    #[cfg(test)]
    pub(super) async fn new_with_payload_segment_limit(
        config: WgpuRenderRuntimeConfig,
        segment_limit: u64,
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
        let descriptor = renderer_device_descriptor(&adapter, "mirante4d-segment-fixture-device")?;
        let (device, queue) = adapter
            .request_device(&descriptor)
            .await
            .map_err(|_| WgpuRenderRuntimeError::DeviceCreationFailed)?;
        Self::from_device_parts(
            Some(instance),
            &adapter,
            device,
            queue,
            config,
            Some(segment_limit),
        )
    }

    pub(super) fn from_existing_device(
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: WgpuRenderRuntimeConfig,
    ) -> Result<Self, WgpuRenderRuntimeError> {
        validate_adapter(adapter)?;
        validate_device_limits(&device.limits())?;
        Self::from_device_parts(None, adapter, device, queue, config, None)
    }

    fn from_device_parts(
        instance: Option<wgpu::Instance>,
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: WgpuRenderRuntimeConfig,
        segment_limit_override: Option<u64>,
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
        let control_capacity_bytes = CONTROL_BUFFER_BYTES.min(
            other_capacity_bytes.saturating_sub(INITIAL_CONTROL_BYTES + 256 * 1024)
                / COPY_ALIGNMENT
                * COPY_ALIGNMENT,
        );
        if control_capacity_bytes < MIN_CONTROL_BUFFER_BYTES {
            return Err(WgpuRenderRuntimeError::InvalidConfiguration);
        }
        let mut payload_limits = device.limits();
        if let Some(limit) = segment_limit_override {
            payload_limits.max_buffer_size = payload_limits.max_buffer_size.min(limit);
            payload_limits.max_storage_buffer_binding_size =
                payload_limits.max_storage_buffer_binding_size.min(limit);
        }
        let segment_capacities = payload_segment_capacities(payload_ledger_bytes, &payload_limits)?;
        let payload_capacity_bytes = segment_capacities.iter().sum::<u64>();
        let payload_segment_count = segment_capacities
            .iter()
            .filter(|capacity| **capacity != 0)
            .count();

        let validation_error_count = Arc::new(AtomicUsize::new(0));
        let error_count = Arc::clone(&validation_error_count);
        device.on_uncaptured_error(Arc::new(move |error| {
            #[cfg(test)]
            eprintln!("mirante4d uncaptured WGPU error: {error}");
            #[cfg(not(test))]
            let _ = error;
            error_count.fetch_add(1, Ordering::Relaxed);
        }));

        let payload_segments = segment_capacities
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, capacity)| *capacity != 0)
            .map(|(index, capacity)| PayloadSegment {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(match index {
                        0 => "mirante4d-payload-segment-0",
                        1 => "mirante4d-payload-segment-1",
                        2 => "mirante4d-payload-segment-2",
                        _ => "mirante4d-payload-segment-3",
                    }),
                    size: capacity,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                capacity,
                allocator: ArenaAllocator::new(capacity),
            })
            .collect::<Vec<_>>();
        let mut render_layout_entries = vec![storage_layout_entry(0, INITIAL_CONTROL_BYTES)];
        render_layout_entries.extend(segment_capacities.iter().enumerate().map(
            |(index, capacity)| {
                storage_layout_entry(1 + index as u32, (*capacity).max(COPY_ALIGNMENT))
            },
        ));
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mirante4d-wp09a-bind-group-layout"),
            entries: &render_layout_entries,
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
        let fragment_targets = [
            Some(wgpu::ColorTargetState {
                format: COLOR_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            }),
            config
                .validation_capture()
                .then_some(wgpu::ColorTargetState {
                    format: FACT_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                }),
        ];
        let mut pipeline_creations = 0_u64;
        let pipeline = create_counted_render_pipeline(
            &device,
            &mut pipeline_creations,
            &wgpu::RenderPipelineDescriptor {
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
                    entry_point: Some(if config.validation_capture() {
                        "fs_validation"
                    } else {
                        "fs_color"
                    }),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &fragment_targets,
                }),
                multiview_mask: None,
                cache: None,
            },
        );
        let mip_pipeline = create_counted_render_pipeline(
            &device,
            &mut pipeline_creations,
            &wgpu::RenderPipelineDescriptor {
                label: Some("mirante4d-mip-pipeline"),
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
                    entry_point: Some(if config.validation_capture() {
                        "fs_mip_validation"
                    } else {
                        "fs_mip_color"
                    }),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &fragment_targets,
                }),
                multiview_mask: None,
                cache: None,
            },
        );
        let driver = if info.driver_info.trim().is_empty() {
            info.driver.clone()
        } else {
            info.driver_info.clone()
        };
        let timestamp_supported = device.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        let upload_timestamp_supported = device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS);
        let timing = (timestamp_supported && config.gpu_timing())
            .then(|| create_timing_resources(&device, &queue));
        let gpu_timing_enabled = timing.is_some();
        let pick = create_pick_resources(&device, &segment_capacities, &mut pipeline_creations);
        let upload_staging = UploadStagingPool::new(transfer_capacity_bytes);
        Ok(Self {
            _instance: instance,
            device,
            queue,
            pipeline,
            mip_pipeline,
            render_bind_group_layout: bind_group_layout,
            payload_segments,
            upload_staging,
            control_capacity_bytes,
            resident: BTreeMap::new(),
            payload_resident_lru: std::array::from_fn(|_| BTreeSet::new()),
            payload_evictable_lru: std::array::from_fn(|_| BTreeSet::new()),
            empty_resident_count: 0,
            empty_resident_lru: BTreeSet::new(),
            recently_evicted: BTreeMap::new(),
            recently_evicted_order: VecDeque::new(),
            next_eviction_sequence: 1,
            residency_epoch: 1,
            residency_invalidation_epoch: 1,
            presentations: BTreeMap::new(),
            next_presentation: 1,
            next_capture: 1,
            next_timing: 1,
            next_pick: 1,
            timing,
            pick,
            in_flight_submissions: Arc::new(AtomicUsize::new(0)),
            validation_error_count,
            config,
            diagnostics: WgpuRenderRuntimeDiagnostics {
                adapter_name: info.name,
                backend: format!("{:?}", info.backend),
                driver,
                vendor_id: info.vendor,
                device_id: info.device,
                device_type: format!("{:?}", info.device_type),
                driver_name: info.driver,
                driver_info: info.driver_info,
                max_buffer_size_bytes: adapter_limits.max_buffer_size,
                max_storage_buffer_binding_size_bytes: adapter_limits
                    .max_storage_buffer_binding_size,
                max_storage_buffers_per_shader_stage: adapter_limits
                    .max_storage_buffers_per_shader_stage,
                gpu_budget_bytes: config.gpu_budget_bytes(),
                payload_capacity_bytes,
                payload_segment_count,
                payload_segment_capacity_bytes: segment_capacities,
                transfer_capacity_bytes,
                other_capacity_bytes,
                payload_arena_allocated_bytes: payload_capacity_bytes,
                resident_payload_used_bytes: 0,
                peak_resident_payload_used_bytes: 0,
                empty_resident_metadata_capacity_records: MAX_EMPTY_RESIDENTS,
                empty_resident_metadata_bytes_per_record: EMPTY_RESIDENT_METADATA_BYTES_PER_RECORD,
                empty_resident_metadata_records: 0,
                empty_resident_metadata_bytes: 0,
                peak_empty_resident_metadata_bytes: 0,
                peak_transfer_bytes: 0,
                peak_display_target_bytes: 0,
                peak_page_table_bytes: 0,
                peak_scratch_bytes: 0,
                frames_executed: 0,
                queue_submissions: 0,
                current_in_flight_submissions: 0,
                peak_in_flight_submissions: 0,
                backpressure_deferrals: 0,
                residency_hits: 0,
                residency_misses: 0,
                residency_evictions: 0,
                residency_epoch_reuploads: 0,
                uploaded_resources: 0,
                uploaded_payload_bytes: 0,
                upload_staging_padding_zero_bytes: 0,
                render_thread_payload_fact_scan_bytes: 0,
                control_static_rebuilds: 0,
                control_static_rebuild_bytes: 0,
                page_layout_constructions: 0,
                control_dynamic_updates: 0,
                control_dynamic_upload_bytes: 0,
                control_publication_writes: 0,
                peak_control_publication_writes_per_frame: 0,
                control_dense_fallbacks: 0,
                control_body_delta_updates: 0,
                control_body_delta_keys: 0,
                control_body_delta_page_entries: 0,
                body_delta_pin_lru_keys: 0,
                control_buffer_allocations: 0,
                control_buffer_allocation_bytes: 0,
                bind_group_creations: 0,
                pipeline_creations,
                explicit_staging_allocations: 0,
                explicit_staging_bytes: 0,
                peak_explicit_staging_bytes: 0,
                allocator_plans: 0,
                retained_navigation_frames: 0,
                cold_coverage_membership_checks: 0,
                gpu_timestamps_supported: timestamp_supported,
                gpu_timing_enabled,
                gpu_payload_copy_timestamps_supported: upload_timestamp_supported,
                gpu_timing_prelude_submissions: 0,
                completed_gpu_timings: 0,
                gpu_timing_failures: 0,
                last_gpu_batch_envelope_ns: None,
                last_gpu_payload_copy_ns: None,
                last_gpu_render_pass_ns: None,
                completed_cpu_timings: 0,
                last_cpu_timing_frame: None,
                last_cpu_planning_ns: None,
                last_cpu_control_publication_ns: None,
                last_cpu_payload_staging_ns: None,
                last_cpu_queue_submit_ns: None,
                total_cpu_planning_ns: 0,
                total_cpu_control_publication_ns: 0,
                total_cpu_payload_staging_ns: 0,
                total_cpu_queue_submit_ns: 0,
                pick_submissions: 0,
                completed_picks: 0,
                pick_backpressure_deferrals: 0,
                validation_error_count: 0,
            },
        })
    }

    pub(super) const fn diagnostics(&self) -> &WgpuRenderRuntimeDiagnostics {
        &self.diagnostics
    }

    pub(super) const fn residency_epoch(&self) -> u64 {
        self.residency_epoch
    }

    pub(super) const fn residency_invalidation_epoch(&self) -> u64 {
        self.residency_invalidation_epoch
    }

    pub(super) fn resource_is_resident(&self, key: DatasetResourceKey) -> bool {
        self.resident.contains_key(&key)
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
            .checked_add(display_allocation_bytes(
                extent,
                self.config.validation_capture(),
            )?)
            .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
        let control_bytes = self
            .active_control_bytes()
            .checked_add(INITIAL_CONTROL_BYTES)
            .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
        self.validate_other_capacity(control_bytes, display_bytes, self.active_capture_bytes())?;
        let display = create_display(&self.device, extent, self.config.validation_capture())?;
        let (control_buffer, render_bind_group, pick_bind_groups) =
            create_presentation_control_resources(
                &self.device,
                &self.render_bind_group_layout,
                &self.pick,
                &self.payload_segments,
                INITIAL_CONTROL_BYTES,
                &mut self.diagnostics.bind_group_creations,
            );
        self.presentations.insert(
            token,
            PresentationState {
                frame_state: None,
                last_rendered_frame: None,
                last_rendered_timepoint: None,
                last_rendered_volume: false,
                last_rendered_layers: Vec::new(),
                last_rendered_modes: Vec::new(),
                last_rendered_sampling: Vec::new(),
                availability: None,
                last_progress: None,
                residency_dirty_keys: BTreeSet::new(),
                control_buffer,
                control_capacity: INITIAL_CONTROL_BYTES,
                control_prefix: Vec::new(),
                control_layout: None,
                control_requirements: None,
                pending_leases: PendingLeaseQueue::new(),
                render_bind_group,
                pick_bind_groups,
                display,
                pending_capture: None,
            },
        );
        self.next_presentation = next_presentation;
        self.diagnostics.control_buffer_allocations = self
            .diagnostics
            .control_buffer_allocations
            .saturating_add(1);
        self.diagnostics.control_buffer_allocation_bytes = self
            .diagnostics
            .control_buffer_allocation_bytes
            .saturating_add(INITIAL_CONTROL_BYTES);
        self.diagnostics.peak_display_target_bytes = self
            .diagnostics
            .peak_display_target_bytes
            .max(display_bytes);
        self.diagnostics.peak_page_table_bytes =
            self.diagnostics.peak_page_table_bytes.max(control_bytes);
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
        let retired = self
            .presentations
            .remove(&token)
            .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered { token })?;
        let released_requirements = retired
            .frame_state
            .as_ref()
            .map(|state| state.requirements.clone());
        drop(retired);
        if let Some(requirements) = released_requirements.as_ref() {
            self.refresh_payload_pin_states(requirements);
        }
        Ok(PresentationRetirement::new(token))
    }

    pub(super) fn deactivate_presentation(
        &mut self,
        token: PresentationToken,
    ) -> Result<(), WgpuRenderRuntimeError> {
        let released_requirements = {
            let presentation = self
                .presentations
                .get_mut(&token)
                .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered { token })?;
            let released = presentation
                .frame_state
                .as_ref()
                .map(|state| state.requirements.clone());
            presentation.frame_state = None;
            presentation.availability = None;
            presentation.pending_leases.clear();
            presentation.last_rendered_frame = None;
            presentation.last_rendered_timepoint = None;
            presentation.last_rendered_volume = false;
            presentation.last_rendered_layers.clear();
            presentation.last_rendered_modes.clear();
            presentation.last_rendered_sampling.clear();
            presentation.last_progress = None;
            presentation.residency_dirty_keys.clear();
            presentation.control_prefix.clear();
            presentation.control_layout = None;
            presentation.control_requirements = None;
            presentation.pending_capture = None;
            released
        };
        if let Some(requirements) = released_requirements.as_ref() {
            self.refresh_payload_pin_states(requirements);
        }
        Ok(())
    }

    /// Retires every GPU fact scoped to the current dataset generation while
    /// preserving the device, pipelines, registered presentation targets,
    /// and lifetime diagnostics. Source replacement is a hard boundary: no
    /// resident key, prepared control state, or asynchronous result from the
    /// predecessor may be observed by the successor.
    pub(super) fn retire_dataset_generation(&mut self) {
        let presentation_tokens = self.presentations.keys().copied().collect::<Vec<_>>();
        for token in presentation_tokens {
            self.deactivate_presentation(token)
                .expect("registered presentation remains valid during dataset retirement");
        }

        self.resident.clear();
        for segment in &mut self.payload_segments {
            segment.allocator = ArenaAllocator::new(segment.capacity);
        }
        for lru in &mut self.payload_resident_lru {
            lru.clear();
        }
        for lru in &mut self.payload_evictable_lru {
            lru.clear();
        }
        self.empty_resident_count = 0;
        self.empty_resident_lru.clear();
        self.recently_evicted.clear();
        self.recently_evicted_order.clear();
        self.next_eviction_sequence = 1;

        // Pending map callbacks retain their old WGPU resources until the
        // queue finishes, while replacement resources make every old ticket
        // immediately unknown to the successor generation.
        if self.timing.is_some() {
            self.timing = Some(create_timing_resources(&self.device, &self.queue));
        }
        self.pick.slots = create_pick_slots(&self.device);
        let (device, pick, payload_segments, presentations, diagnostics) = (
            &self.device,
            &self.pick,
            &self.payload_segments,
            &mut self.presentations,
            &mut self.diagnostics,
        );
        for presentation in presentations.values_mut() {
            presentation.pick_bind_groups = create_presentation_pick_bind_groups(
                device,
                pick,
                payload_segments,
                &presentation.control_buffer,
                &mut diagnostics.bind_group_creations,
            );
        }

        self.residency_epoch = self.residency_epoch.saturating_add(1);
        self.residency_invalidation_epoch = self.residency_invalidation_epoch.saturating_add(1);
        self.diagnostics.resident_payload_used_bytes = 0;
        self.diagnostics.empty_resident_metadata_records = 0;
        self.diagnostics.empty_resident_metadata_bytes = 0;
        self.diagnostics.last_gpu_batch_envelope_ns = None;
        self.diagnostics.last_gpu_payload_copy_ns = None;
        self.diagnostics.last_gpu_render_pass_ns = None;
        self.diagnostics.last_cpu_timing_frame = None;
        self.diagnostics.last_cpu_planning_ns = None;
        self.diagnostics.last_cpu_control_publication_ns = None;
        self.diagnostics.last_cpu_payload_staging_ns = None;
        self.diagnostics.last_cpu_queue_submit_ns = None;
    }

    pub(super) fn preflight_deactivate_presentation(
        &self,
        token: PresentationToken,
    ) -> Result<(), WgpuRenderRuntimeError> {
        self.presentations
            .contains_key(&token)
            .then_some(())
            .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered { token })
    }

    fn active_display_bytes(&self) -> u64 {
        self.presentations
            .values()
            .map(|presentation| presentation.display.allocated_bytes)
            .sum()
    }

    fn active_control_bytes(&self) -> u64 {
        self.presentations
            .values()
            .map(|presentation| presentation.control_capacity)
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
        control_bytes: u64,
        display_bytes: u64,
        capture_bytes: u64,
    ) -> Result<(), WgpuRenderRuntimeError> {
        if control_bytes > self.diagnostics.other_capacity_bytes {
            return Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::PageTable,
                requested_bytes: control_bytes,
                available_bytes: self.diagnostics.other_capacity_bytes,
            });
        }
        let after_control = self
            .diagnostics
            .other_capacity_bytes
            .saturating_sub(control_bytes);
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

    /// Executes a camera/transfer-only frame from retained presentation state.
    /// The proof is an unchanged requirement-body identity plus an unchanged
    /// renderer residency epoch, so this path performs no requirement walk,
    /// lease validation, allocator clone, eviction planning, or payload copy.
    fn try_execute_resident_navigation_frame(
        &mut self,
        presentation_token: PresentationToken,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        setup: FrameExecutionSetup<'_>,
    ) -> Result<Option<FrameExecutionReport>, WgpuRenderRuntimeError> {
        let FrameExecutionSetup {
            cpu_timing_start,
            display_generation,
            render_policy,
            ..
        } = setup;
        let Some(presentation) = self.presentations.get(&presentation_token) else {
            return Err(WgpuRenderRuntimeError::PresentationNotRegistered {
                token: presentation_token,
            });
        };
        let Some(frame_state) = presentation.frame_state.as_ref() else {
            return Ok(None);
        };
        let requirement_body_retained =
            frame_state.requirements.shares_resources_with(requirements)
                && frame_state.requirements.prefetch_promoted() == requirements.prefetch_promoted()
                && presentation
                    .control_requirements
                    .as_ref()
                    .is_some_and(|control| {
                        control.shares_resources_with(requirements)
                            && control.prefetch_promoted() == requirements.prefetch_promoted()
                    });
        let layers_match = presentation
            .last_rendered_layers
            .iter()
            .copied()
            .eq(intent.layers().iter().map(|layer| layer.layer()));
        if !requirement_body_retained
            || !layers_match
            || presentation.control_layout.is_none()
            || presentation.last_progress.is_none()
            || self.presentation_has_relevant_residency_changes(presentation)
        {
            return Ok(None);
        }

        let progress = presentation
            .last_progress
            .as_ref()
            .expect("fast-path preflight checked retained progress")
            .rebind(requirements)
            .map_err(|_| WgpuRenderRuntimeError::FrameProgressContract)?;
        let policy_allows_render =
            retained_frame_policy_allows_render(render_policy, Some(progress.completeness()));
        let replaces_display =
            policy_allows_render && presentation.display.extent != intent.extent();
        let render = policy_allows_render
            && (presentation.last_rendered_frame != Some(intent.frame()) || replaces_display);
        if !render {
            return Ok(Some(FrameExecutionReport {
                presentation: None,
                frame: intent.frame(),
                progress: Some(progress),
                visited_resources: 0,
                uploaded_resources: 0,
                payload_upload_bytes: 0,
                control_upload_bytes: 0,
                command_buffers: 0,
                queue_submissions: 0,
                deferred_by_backpressure: false,
                retained_updates_accepted: true,
                cpu_timing: None,
                gpu_timing: None,
                validation_capture: None,
                newly_resident_keys: Box::new([]),
                evicted_keys: Box::new([]),
            }));
        }

        let control = build_dynamic_control_prefix(
            presentation
                .control_layout
                .as_ref()
                .expect("fast-path preflight checked the static layout"),
            catalog,
            intent,
        )?;
        let mut control = control;
        set_control_full_coverage(&mut control, progress.coverage().is_full())?;
        let control_bytes = control.len() as u64;
        let capture_bytes = if self.config.validation_capture() {
            capture_allocation_bytes(intent.extent())?
        } else {
            0
        };
        if self.config.validation_capture()
            && presentation.pending_capture.is_some()
            && frame_state.frame == intent.frame()
        {
            return Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::Scratch,
                requested_bytes: capture_bytes,
                available_bytes: 0,
            });
        }
        let display_peak_bytes = self
            .active_display_bytes()
            .saturating_add(if replaces_display {
                display_allocation_bytes(intent.extent(), self.config.validation_capture())?
            } else {
                0
            });
        let control_bytes_peak = self.active_control_bytes();
        let capture_peak_bytes = self.active_capture_bytes().saturating_add(capture_bytes);
        self.validate_other_capacity(control_bytes_peak, display_peak_bytes, capture_peak_bytes)?;

        let mut new_display = if replaces_display {
            Some(create_display(
                &self.device,
                intent.extent(),
                self.config.validation_capture(),
            )?)
        } else {
            None
        };
        let timing_plan = self.timing.as_ref().and_then(|timing| {
            timing
                .slots
                .iter()
                .position(|slot| matches!(slot.state, TimingSlotState::Free))
                .map(|slot| {
                    let ticket = GpuTimingTicket {
                        id: self.next_timing,
                        target: presentation_token,
                        generation: intent.frame(),
                        display_generation,
                        pass_kind: intent.view().pass_kind(),
                    };
                    let query_base = u32::try_from(slot).expect("bounded timing slot fits u32")
                        * TIMING_QUERY_WORDS;
                    TimingPlan {
                        slot,
                        ticket,
                        queries: TimingQueryLayout::new(
                            query_base,
                            timing.encoder_copy_timestamps,
                            false,
                            true,
                        ),
                    }
                })
        });
        let gpu_timing_ticket = timing_plan.map(|plan| plan.ticket);
        let timing_prelude_submit_ns =
            timing_plan.and_then(|plan| self.submit_timing_batch_prelude(plan));
        let timing_prelude_submitted = timing_prelude_submit_ns.is_some();
        if timing_prelude_submitted {
            self.diagnostics.gpu_timing_prelude_submissions = self
                .diagnostics
                .gpu_timing_prelude_submissions
                .saturating_add(1);
        }
        let control_publication_start = cpu_timing_start.as_ref().map(|_| Instant::now());
        self.queue.write_buffer(
            &self
                .presentations
                .get(&presentation_token)
                .expect("fast-path presentation remains registered")
                .control_buffer,
            0,
            &control,
        );
        let control_publication_ns = control_publication_start.as_ref().map(elapsed_nanoseconds);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-vp01-retained-navigation-frame"),
            });
        let display = new_display.as_ref().unwrap_or_else(|| {
            &self
                .presentations
                .get(&presentation_token)
                .expect("fast-path presentation remains registered")
                .display
        });
        let pass_timestamps = timing_plan.and_then(|plan| {
            plan.queries.render_pass.map(|(beginning, end)| {
                (
                    &self
                        .timing
                        .as_ref()
                        .expect("timing plan has resources")
                        .query_set,
                    beginning,
                    end,
                )
            })
        });
        encode_render_pass(
            &mut encoder,
            if uses_mip_pipeline(intent) {
                &self.mip_pipeline
            } else {
                &self.pipeline
            },
            &self
                .presentations
                .get(&presentation_token)
                .expect("fast-path presentation remains registered")
                .render_bind_group,
            display,
            pass_timestamps,
        );
        if let Some((_, end)) = timing_plan.and_then(|plan| plan.queries.batch_envelope) {
            encoder.write_timestamp(
                &self
                    .timing
                    .as_ref()
                    .expect("timing plan has resources")
                    .query_set,
                end,
            );
        }
        let mut pending_capture = None;
        let capture_ticket = if self.config.validation_capture() {
            let pending = encode_capture(
                &self.device,
                &mut encoder,
                self.next_capture,
                presentation_token,
                intent.frame(),
                display,
            )?;
            let ticket = pending.ticket;
            pending_capture = Some(pending);
            Some(ticket)
        } else {
            None
        };
        if let Some(plan) = timing_plan {
            let timing = self.timing.as_ref().expect("timing plan has resources");
            let base = u32::try_from(plan.slot).expect("bounded timing slot fits u32")
                * TIMING_QUERY_WORDS;
            let resolve_offset = plan.slot as u64 * TIMING_RESOLVE_STRIDE;
            encoder.resolve_query_set(
                &timing.query_set,
                base..base + plan.queries.query_count,
                &timing.resolve_buffer,
                resolve_offset,
            );
            encoder.copy_buffer_to_buffer(
                &timing.resolve_buffer,
                resolve_offset,
                &timing.slots[plan.slot].readback,
                0,
                u64::from(plan.queries.query_count) * 8,
            );
        }
        let command_buffer = encoder.finish();
        let cpu_planning_ns = cpu_timing_start.as_ref().map(|start| {
            elapsed_nanoseconds(start).saturating_sub(timing_prelude_submit_ns.unwrap_or(0))
        });
        let cpu_submit_start = cpu_timing_start.as_ref().map(|_| Instant::now());
        self.queue.submit([command_buffer]);
        let cpu_queue_submit_ns = cpu_submit_start.as_ref().map(|start| {
            elapsed_nanoseconds(start).saturating_add(timing_prelude_submit_ns.unwrap_or(0))
        });
        let cpu_timing =
            cpu_planning_ns
                .zip(cpu_queue_submit_ns)
                .map(|(planning_ns, queue_submit_ns)| {
                    CpuFrameTiming::new(planning_ns, control_publication_ns, None, queue_submit_ns)
                });
        if let Some(timing) = cpu_timing {
            self.record_cpu_frame_timing(intent.frame(), timing);
        }
        let in_flight = Arc::clone(&self.in_flight_submissions);
        let submitted = in_flight.fetch_add(1, Ordering::AcqRel) + 1;
        self.queue.on_submitted_work_done(move || {
            in_flight.fetch_sub(1, Ordering::AcqRel);
        });
        if let Some(pending) = pending_capture.as_ref() {
            pending.start_map();
        }
        if let Some(plan) = timing_plan {
            let mapped = Arc::new(Mutex::new(None));
            let callback = Arc::clone(&mapped);
            let timing = self.timing.as_mut().expect("timing plan has resources");
            timing.slots[plan.slot].readback.slice(..).map_async(
                wgpu::MapMode::Read,
                move |result| {
                    if let Ok(mut status) = callback.lock() {
                        *status = Some(result.map_err(|_| ()));
                    }
                },
            );
            timing.slots[plan.slot].state = TimingSlotState::Pending {
                ticket: plan.ticket,
                mapped,
                batch_envelope_timestamps: plan.queries.batch_envelope.is_some(),
                payload_copy_timestamps: plan.queries.payload_copy.is_some(),
                render_pass_timestamps: plan.queries.render_pass.is_some(),
            };
            self.next_timing = self.next_timing.saturating_add(1);
        }

        let current_cursor = self
            .presentations
            .get(&presentation_token)
            .and_then(|presentation| presentation.frame_state.as_ref())
            .map_or(0, |state| state.cursor);
        let presentation = self
            .presentations
            .get_mut(&presentation_token)
            .expect("fast-path presentation remains registered");
        presentation.frame_state = Some(FrameState {
            frame: intent.frame(),
            requirements: requirements.clone(),
            cursor: current_cursor,
        });
        if let Some(display) = new_display.take() {
            presentation.display = display;
        }
        presentation.control_prefix = control;
        presentation.last_rendered_frame = Some(intent.frame());
        presentation.last_rendered_timepoint = Some(intent.timepoint());
        presentation.last_rendered_volume =
            matches!(intent.view(), RenderViewIntent::Volume { .. });
        presentation.last_rendered_modes.clear();
        presentation
            .last_rendered_modes
            .extend(intent.layers().iter().map(|layer| {
                let state = layer.render_state();
                if state.mip_parameters().is_some() {
                    0
                } else if state.dvr_parameters().is_some() {
                    1
                } else {
                    2
                }
            }));
        presentation.last_rendered_sampling.clear();
        presentation.last_rendered_sampling.extend(
            intent
                .layers()
                .iter()
                .map(|layer| layer.render_state().sampling_policy()),
        );
        presentation.last_progress = Some(progress.clone());
        presentation.residency_dirty_keys.clear();
        if let Some(pending) = pending_capture {
            presentation.pending_capture = Some(pending);
            self.next_capture = self.next_capture.saturating_add(1);
        }
        self.diagnostics.current_in_flight_submissions = submitted;
        self.diagnostics.peak_in_flight_submissions =
            self.diagnostics.peak_in_flight_submissions.max(submitted);
        self.diagnostics.control_dynamic_updates =
            self.diagnostics.control_dynamic_updates.saturating_add(1);
        self.diagnostics.control_dynamic_upload_bytes = self
            .diagnostics
            .control_dynamic_upload_bytes
            .saturating_add(control_bytes);
        self.diagnostics.retained_navigation_frames = self
            .diagnostics
            .retained_navigation_frames
            .saturating_add(1);
        self.record_control_publication_writes(1);
        self.refresh_diagnostics(
            control_bytes,
            control_bytes_peak,
            display_peak_bytes,
            capture_peak_bytes,
            true,
            1 + u32::from(timing_prelude_submitted),
        );

        Ok(Some(FrameExecutionReport {
            presentation: Some(PresentedFrame::new(
                presentation_token,
                intent.extent(),
                progress.clone(),
            )),
            frame: intent.frame(),
            progress: Some(progress),
            visited_resources: 0,
            uploaded_resources: 0,
            payload_upload_bytes: 0,
            control_upload_bytes: control_bytes,
            command_buffers: 1 + u32::from(timing_prelude_submitted),
            queue_submissions: 1 + u32::from(timing_prelude_submitted),
            deferred_by_backpressure: false,
            retained_updates_accepted: true,
            cpu_timing,
            gpu_timing: gpu_timing_ticket,
            validation_capture: capture_ticket,
            newly_resident_keys: Box::new([]),
            evicted_keys: Box::new([]),
        }))
    }

    pub(super) fn execute_retained_frame(
        &mut self,
        presentation_token: PresentationToken,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        lease_updates: &[Arc<dyn ResourceLease>],
    ) -> Result<FrameExecutionReport, WgpuRenderRuntimeError> {
        let cpu_timing_start = self.cpu_timing_start();
        self.execute_retained_frame_with_layout(
            presentation_token,
            catalog,
            intent,
            requirements,
            FrameExecutionSetup {
                prepared_layout: None,
                nonblocking_progress_collected: true,
                cpu_timing_start,
                display_generation: 0,
                render_policy: RetainedFrameRenderPolicy::EveryUsefulFrame,
            },
            lease_updates,
        )
    }

    pub(super) fn execute_prepared_retained_frame(
        &mut self,
        presentation_token: PresentationToken,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        layout: &PreparedStaticPresentationLayout,
        lease_updates: &[Arc<dyn ResourceLease>],
    ) -> Result<FrameExecutionReport, WgpuRenderRuntimeError> {
        self.execute_prepared_retained_frame_with_policy(
            presentation_token,
            catalog,
            intent,
            requirements,
            layout,
            lease_updates,
            0,
            RetainedFrameRenderPolicy::EveryUsefulFrame,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn execute_prepared_retained_frame_with_policy(
        &mut self,
        presentation_token: PresentationToken,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        layout: &PreparedStaticPresentationLayout,
        lease_updates: &[Arc<dyn ResourceLease>],
        display_generation: u64,
        render_policy: RetainedFrameRenderPolicy,
    ) -> Result<FrameExecutionReport, WgpuRenderRuntimeError> {
        let cpu_timing_start = self.cpu_timing_start();
        self.execute_retained_frame_with_layout(
            presentation_token,
            catalog,
            intent,
            requirements,
            FrameExecutionSetup {
                prepared_layout: Some(layout),
                nonblocking_progress_collected: true,
                cpu_timing_start,
                display_generation,
                render_policy,
            },
            lease_updates,
        )
    }

    fn execute_retained_frame_with_layout(
        &mut self,
        presentation_token: PresentationToken,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        setup: FrameExecutionSetup<'_>,
        lease_updates: &[Arc<dyn ResourceLease>],
    ) -> Result<FrameExecutionReport, WgpuRenderRuntimeError> {
        let layout = setup.prepared_layout;
        debug_assert!(setup.nonblocking_progress_collected);
        self.validate_presentation_activation(presentation_token)?;
        validate_lease_capacity(lease_updates.len())?;
        let current_frame_state = self
            .presentations
            .get(&presentation_token)
            .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered {
                token: presentation_token,
            })?
            .frame_state
            .clone();
        // Reject stale or internally inconsistent work before it can filter or
        // append the retained ranked cohort.
        validate_retained_frame_preflight(current_frame_state.as_ref(), intent, requirements)?;
        if layout.is_some_and(|layout| !layout.matches(intent, requirements)) {
            return Err(WgpuRenderRuntimeError::PreparedStaticLayoutMismatch);
        }
        for lease in lease_updates {
            if !requirements.contains_resource(lease.key()) {
                return Err(WgpuRenderRuntimeError::UnexpectedLease);
            }
        }

        let requirement_body_changed = current_frame_state
            .as_ref()
            .is_none_or(|state| !state.requirements.shares_resources_with(requirements));
        let nonresident_updates = lease_updates
            .iter()
            .filter(|lease| !self.resident.contains_key(&lease.key()))
            .cloned()
            .collect::<Vec<_>>();
        let mut overflow_updates = None;
        {
            let resident = &self.resident;
            let presentation = self
                .presentations
                .get_mut(&presentation_token)
                .expect("retained-frame presentation was checked");
            // Requirement replacement is rare; filter only the retained
            // backlog, never construct or scan a 65k-key allowed set. Entries
            // made resident by another presentation are retired at the same
            // bounded pending-cohort cost.
            if !presentation.pending_leases.is_empty() {
                presentation.pending_leases.retain(|key| {
                    (!requirement_body_changed || requirements.contains_resource(key))
                        && !resident.contains_key(&key)
                });
            }
            if !presentation
                .pending_leases
                .try_extend(&nonresident_updates)?
            {
                overflow_updates = Some(nonresident_updates);
            }
        }
        let mut updates_accepted = overflow_updates.is_none();

        // Queue updates first, then honor backpressure before payload
        // validation or backlog traversal. The next admitted frame consumes
        // the same retained updates; no scheduler work is lost.
        let _ = self.device.poll(wgpu::PollType::Poll);
        self.upload_staging.refresh()?;
        self.collect_gpu_timings()?;
        let in_flight = self.in_flight_submissions.load(Ordering::Acquire);
        self.diagnostics.current_in_flight_submissions = in_flight;
        if in_flight >= MAX_IN_FLIGHT_SUBMISSIONS {
            self.diagnostics.backpressure_deferrals =
                self.diagnostics.backpressure_deferrals.saturating_add(1);
            return Ok(FrameExecutionReport {
                presentation: None,
                frame: intent.frame(),
                progress: None,
                visited_resources: 0,
                uploaded_resources: 0,
                payload_upload_bytes: 0,
                control_upload_bytes: 0,
                command_buffers: 0,
                queue_submissions: 0,
                deferred_by_backpressure: true,
                retained_updates_accepted: overflow_updates.is_none(),
                cpu_timing: None,
                gpu_timing: None,
                validation_capture: None,
                newly_resident_keys: Box::new([]),
                evicted_keys: Box::new([]),
            });
        }

        // The pending authority is already one atomic, fully uploadable cohort;
        // materialize it in exact first-seen order without any key sort.
        let selected = self
            .presentations
            .get(&presentation_token)
            .expect("retained-frame presentation remains registered")
            .pending_leases
            .ordered_leases();
        let selected_keys = selected.iter().map(|lease| lease.key()).collect::<Vec<_>>();
        let lease_refs = selected.iter().map(Arc::as_ref).collect::<Vec<_>>();
        let result = self.execute_frame_with_layout(
            presentation_token,
            catalog,
            intent,
            requirements,
            setup,
            &lease_refs,
        );
        drop(lease_refs);
        if let Ok(report) = result.as_ref() {
            let resident = &self.resident;
            let pending = &mut self
                .presentations
                .get_mut(&presentation_token)
                .expect("retained-frame presentation remains registered")
                .pending_leases;
            for key in selected_keys {
                if resident.contains_key(&key) {
                    pending.remove(key);
                }
            }
            // If the prior cohort drained in this submission, retain the next
            // caller-ranked cohort for the following frame. Never hold both at
            // once, and never do this after a transient staging deferral.
            if !report.deferred_by_backpressure()
                && pending.is_empty()
                && let Some(updates) = overflow_updates.as_ref()
            {
                let admitted = pending.try_extend(updates)?;
                debug_assert!(admitted, "an individually bounded cohort fits after drain");
                updates_accepted = admitted;
            }
        }
        result.map(|mut report| {
            report.retained_updates_accepted = updates_accepted;
            report
        })
    }

    pub(super) fn execute_frame(
        &mut self,
        presentation_token: PresentationToken,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        leases: &[&dyn ResourceLease],
    ) -> Result<FrameExecutionReport, WgpuRenderRuntimeError> {
        let cpu_timing_start = self.cpu_timing_start();
        self.execute_frame_with_layout(
            presentation_token,
            catalog,
            intent,
            requirements,
            FrameExecutionSetup {
                prepared_layout: None,
                nonblocking_progress_collected: false,
                cpu_timing_start,
                display_generation: 0,
                render_policy: RetainedFrameRenderPolicy::EveryUsefulFrame,
            },
            leases,
        )
    }

    pub(super) fn execute_prepared_frame(
        &mut self,
        presentation_token: PresentationToken,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        layout: &PreparedStaticPresentationLayout,
        leases: &[&dyn ResourceLease],
    ) -> Result<FrameExecutionReport, WgpuRenderRuntimeError> {
        let cpu_timing_start = self.cpu_timing_start();
        self.execute_frame_with_layout(
            presentation_token,
            catalog,
            intent,
            requirements,
            FrameExecutionSetup {
                prepared_layout: Some(layout),
                nonblocking_progress_collected: false,
                cpu_timing_start,
                display_generation: 0,
                render_policy: RetainedFrameRenderPolicy::EveryUsefulFrame,
            },
            leases,
        )
    }

    fn execute_frame_with_layout(
        &mut self,
        presentation_token: PresentationToken,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        setup: FrameExecutionSetup<'_>,
        leases: &[&dyn ResourceLease],
    ) -> Result<FrameExecutionReport, WgpuRenderRuntimeError> {
        let retained_navigation_setup = setup;
        let FrameExecutionSetup {
            prepared_layout,
            nonblocking_progress_collected,
            cpu_timing_start,
            display_generation,
            render_policy,
        } = setup;
        self.validate_presentation_activation(presentation_token)?;
        // Drive completion callbacks without waiting. This keeps the explicit
        // submission bound honest while never stalling the UI thread. The
        // retained-frame entry already performs this before materializing its
        // pending cohort, so it must not repeat the device/staging/timing walk.
        if !nonblocking_progress_collected {
            let _ = self.device.poll(wgpu::PollType::Poll);
            self.upload_staging.refresh()?;
            self.collect_gpu_timings()?;
        }
        let current_frame_state = self
            .presentations
            .get(&presentation_token)
            .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered {
                token: presentation_token,
            })?
            .frame_state
            .clone();
        if intent.frame() != requirements.frame() {
            return Err(WgpuRenderRuntimeError::FrameContractMismatch);
        }
        if prepared_layout.is_some_and(|layout| !layout.matches(intent, requirements)) {
            return Err(WgpuRenderRuntimeError::PreparedStaticLayoutMismatch);
        }
        validate_extent(intent.extent())?;
        if let Some(current) = current_frame_state.as_ref()
            && intent.frame() < current.frame
        {
            return Err(WgpuRenderRuntimeError::StaleFrame {
                actual: intent.frame(),
                current: current.frame,
            });
        }
        let in_flight = self.in_flight_submissions.load(Ordering::Acquire);
        self.diagnostics.current_in_flight_submissions = in_flight;
        if in_flight >= MAX_IN_FLIGHT_SUBMISSIONS {
            self.diagnostics.backpressure_deferrals =
                self.diagnostics.backpressure_deferrals.saturating_add(1);
            return Ok(FrameExecutionReport {
                presentation: None,
                frame: intent.frame(),
                progress: None,
                visited_resources: 0,
                uploaded_resources: 0,
                payload_upload_bytes: 0,
                control_upload_bytes: 0,
                command_buffers: 0,
                queue_submissions: 0,
                deferred_by_backpressure: true,
                retained_updates_accepted: true,
                cpu_timing: None,
                gpu_timing: None,
                validation_capture: None,
                newly_resident_keys: Box::new([]),
                evicted_keys: Box::new([]),
            });
        }
        let lease_by_key = self.validate_inputs(
            current_frame_state.as_ref(),
            catalog,
            intent,
            requirements,
            leases,
        )?;
        if lease_by_key.is_empty()
            && let Some(report) = self.try_execute_resident_navigation_frame(
                presentation_token,
                catalog,
                intent,
                requirements,
                retained_navigation_setup,
            )?
        {
            return Ok(report);
        }
        let (planned_frame, scheduled) = Self::plan_frame(
            current_frame_state.as_ref(),
            requirements,
            lease_by_key.is_empty(),
        )?;
        // Progressive upload work is driven by the bounded lease-update
        // batch, not by rescanning a 32k requirement window until its cursor
        // happens to intersect those updates. Requirement traversal remains
        // only for a no-update cold presentation bootstrap.
        let visited = if lease_by_key.is_empty() {
            scheduled
        } else {
            lease_by_key.keys().copied().collect()
        };
        let frame_changed = current_frame_state
            .as_ref()
            .is_none_or(|current| current.frame != planned_frame.frame);
        let requirement_body_changed = current_frame_state.as_ref().is_none_or(|current| {
            !current
                .requirements
                .shares_resources_with(&planned_frame.requirements)
        });
        let replacement_release_candidates = self.replacement_release_candidates(
            presentation_token,
            current_frame_state.as_ref(),
            &planned_frame.requirements,
            prepared_layout,
        );

        let mut uploads = Vec::new();
        let mut empty_residents = BTreeMap::new();
        let mut raw_upload_bytes = 0_u64;
        let mut budget_limited = planned_frame.requirements.resources().len() > visited.len();
        // Plan directly against the compact allocator with a bounded undo log.
        // This avoids cloning even a heavily fragmented free-range index. The
        // exact inverse is applied before later preflight, then replayed only
        // after the submission succeeds.
        let mut allocator_operations = Vec::new();
        let mut victim_age_cursors = [None; MAX_PAYLOAD_SEGMENTS];

        for key in &visited {
            if self.resident.contains_key(key) {
                continue;
            }
            let Some(lease) = lease_by_key.get(key).copied() else {
                continue;
            };
            let payload = lease.payload();
            let facts = lease.payload_facts();
            if let Some(resident) = empty_resident_resource(payload, facts, intent.frame().get()) {
                empty_residents.insert(*key, resident);
                continue;
            }
            let raw_bytes = payload.byte_len();
            if uploads.len() == MAX_UPLOADS
                || raw_upload_bytes
                    .checked_add(raw_bytes)
                    .is_none_or(|total| total > MAX_PAYLOAD_UPLOAD_BYTES)
            {
                budget_limited = true;
                continue;
            }
            let layout = match payload_layout(payload) {
                Ok(layout) => layout,
                Err(error) => {
                    rollback_arena_operations(&mut self.payload_segments, &allocator_operations);
                    return Err(error);
                }
            };
            let allocated_bytes = layout.allocation_bytes;
            if let Err(error) = validate_single_payload_segment_fit(
                allocated_bytes,
                self.payload_segments.iter().map(|segment| segment.capacity),
            ) {
                rollback_arena_operations(&mut self.payload_segments, &allocator_operations);
                return Err(error);
            }
            let mut upload_victims = Vec::new();
            let mut allocation = None;
            for (segment_index, segment) in self.payload_segments.iter_mut().enumerate() {
                if let Some(offset) = segment.allocator.allocate(allocated_bytes) {
                    allocator_operations.push(ArenaOperation::Allocate {
                        segment: segment_index,
                        offset,
                        bytes: allocated_bytes,
                    });
                    allocation = Some((segment_index, offset));
                    break;
                }
            }
            if allocation.is_none() {
                let mut segment_order = (0..self.payload_segments.len())
                    .filter(|segment| self.payload_segments[*segment].capacity >= allocated_bytes)
                    .filter_map(|segment| {
                        self.next_payload_eviction_candidate(
                            segment,
                            victim_age_cursors[segment],
                            &replacement_release_candidates[segment],
                            &planned_frame.requirements,
                        )
                        .map(|candidate| (candidate.0, candidate.1, segment))
                    })
                    .collect::<Vec<_>>();
                segment_order.sort();
                for (_, _, segment_index) in segment_order {
                    let operation_start = allocator_operations.len();
                    let victim_start = upload_victims.len();
                    let original_cursor = victim_age_cursors[segment_index];
                    loop {
                        if let Some(offset) = self.payload_segments[segment_index]
                            .allocator
                            .allocate(allocated_bytes)
                        {
                            allocator_operations.push(ArenaOperation::Allocate {
                                segment: segment_index,
                                offset,
                                bytes: allocated_bytes,
                            });
                            allocation = Some((segment_index, offset));
                            break;
                        }
                        let Some((age, victim, offset, bytes)) = self
                            .next_payload_eviction_candidate(
                                segment_index,
                                victim_age_cursors[segment_index],
                                &replacement_release_candidates[segment_index],
                                &planned_frame.requirements,
                            )
                        else {
                            rollback_arena_operations_from(
                                &mut self.payload_segments,
                                &mut allocator_operations,
                                operation_start,
                            );
                            upload_victims.truncate(victim_start);
                            victim_age_cursors[segment_index] = original_cursor;
                            break;
                        };
                        victim_age_cursors[segment_index] = Some((age, victim));
                        self.payload_segments[segment_index]
                            .allocator
                            .release(offset, bytes);
                        allocator_operations.push(ArenaOperation::Release {
                            segment: segment_index,
                            offset,
                            bytes,
                        });
                        upload_victims.push(victim);
                    }
                    if allocation.is_some() {
                        break;
                    }
                }
            }
            let Some((segment, offset)) = allocation else {
                let available_bytes = self
                    .payload_segments
                    .iter()
                    .map(|segment| segment.allocator.available_bytes())
                    .sum();
                rollback_arena_operations(&mut self.payload_segments, &allocator_operations);
                return Err(WgpuRenderRuntimeError::CapacityExceeded {
                    category: GpuLedgerCategory::PayloadResidency,
                    requested_bytes: allocated_bytes,
                    available_bytes,
                });
            };
            let resident = ResidentResource {
                segment: segment as u32,
                offset,
                allocated_bytes,
                validity_offset: layout.validity_offset.map(|relative| offset + relative),
                dtype_bytes: u32::from(payload.dtype().bytes_per_sample()),
                minimum: facts.minimum(),
                maximum: facts.maximum(),
                any_valid: facts.any_valid(),
                all_valid: facts.all_valid(),
                last_used_frame: intent.frame().get(),
            };
            raw_upload_bytes += raw_bytes;
            uploads.push(UploadPlan {
                key: *key,
                victims: upload_victims,
                segment,
                offset,
                payload,
                layout,
                resident,
            });
        }
        rollback_arena_operations(&mut self.payload_segments, &allocator_operations);
        let empty_evictions = self.plan_empty_resident_evictions(
            presentation_token,
            &planned_frame.requirements,
            empty_residents.len(),
        )?;

        let evicted = uploads
            .iter()
            .flat_map(|upload| upload.victims.iter().copied())
            .chain(empty_evictions.iter().copied())
            .collect::<BTreeSet<_>>();
        let uploaded = uploads
            .iter()
            .map(|upload| upload.key)
            .collect::<BTreeSet<_>>();
        let upload_resources = uploads
            .iter()
            .map(|upload| (upload.key, upload.resident.clone()))
            .collect::<BTreeMap<_, _>>();
        let is_planned_resident = |key: &DatasetResourceKey| {
            upload_resources.contains_key(key)
                || empty_residents.contains_key(key)
                || (self.resident.contains_key(key) && !evicted.contains(key))
        };
        let presentation = self
            .presentations
            .get(&presentation_token)
            .expect("presentation registration was checked before frame planning");
        let requirement_body_retained = current_frame_state
            .as_ref()
            .is_some_and(|state| state.requirements.shares_resources_with(requirements))
            && presentation.availability.is_some();
        let mut cold_coverage_membership_checks = 0_u64;
        let base_coverage = if requirement_body_retained {
            presentation
                .availability
                .as_ref()
                .expect("retained requirement body has an availability bitmap")
                .rebind(requirements)
                .map_err(|_| WgpuRenderRuntimeError::FrameProgressContract)?
        } else {
            let initially_available = if requirements.resources().len() <= self.resident.len() {
                cold_coverage_membership_checks = requirements.resources().len() as u64;
                requirements
                    .resources()
                    .map(|requirement| requirement.key())
                    .filter(|key| self.resident.contains_key(key))
                    .collect::<Vec<_>>()
            } else {
                cold_coverage_membership_checks = self.resident.len() as u64;
                self.resident
                    .keys()
                    .copied()
                    .filter(|key| requirements.contains_resource(*key))
                    .collect::<Vec<_>>()
            };
            FrameCoverage::from_available(requirements, &initially_available)
                .map_err(|_| WgpuRenderRuntimeError::FrameProgressContract)?
        };
        let mut availability_changes = BTreeMap::new();
        if requirement_body_retained {
            for key in &presentation.residency_dirty_keys {
                if requirements.contains_resource(*key) {
                    availability_changes.insert(*key, is_planned_resident(key));
                }
            }
        }
        for key in &evicted {
            if requirements.contains_resource(*key) {
                availability_changes.insert(*key, false);
            }
        }
        for key in upload_resources.keys().chain(empty_residents.keys()) {
            availability_changes.insert(*key, true);
        }
        let availability_change_list = availability_changes
            .iter()
            .map(|(key, available)| (*key, *available))
            .collect::<Vec<_>>();
        let coverage = base_coverage
            .with_availability_changes(requirements, &availability_change_list)
            .map_err(|_| WgpuRenderRuntimeError::FrameProgressContract)?;
        let progress = build_progress(coverage.clone(), budget_limited, false)?;
        let required_availability_changed = !requirement_body_retained
            || availability_changes
                .keys()
                .any(|key| requirements.is_required_resource(*key));
        let regresses_same_frame = presentation.last_rendered_frame == Some(intent.frame())
            && availability_changes
                .iter()
                .any(|(key, available)| !available && requirements.is_required_resource(*key));
        let policy_allows_render = retained_frame_policy_allows_render(
            render_policy,
            progress.as_ref().map(FrameProgress::completeness),
        );
        let replaces_display =
            policy_allows_render && presentation.display.extent != intent.extent();
        let render = policy_allows_render
            && !regresses_same_frame
            && (presentation.last_rendered_frame != Some(intent.frame())
                || required_availability_changed
                || replaces_display);
        let intent_layers = intent
            .layers()
            .iter()
            .map(|layer| layer.layer())
            .collect::<Vec<_>>();
        let static_control_changed = render
            && (presentation.control_layout.is_none()
                || presentation
                    .control_requirements
                    .as_ref()
                    .is_none_or(|current| {
                        !current.shares_resources_with(&planned_frame.requirements)
                    })
                || presentation.last_rendered_layers != intent_layers);
        // A guard upload must prime its already-installed record slot without
        // repainting the unchanged view. Keeping this publication separate
        // prevents both per-cohort render passes and one O(guard) dense record
        // rebuild on the first contained camera move.
        let control_only_candidate = !render
            && !availability_changes.is_empty()
            && availability_changes
                .keys()
                .all(|key| !requirements.is_required_resource(*key))
            && presentation.last_rendered_frame == Some(intent.frame())
            && presentation.control_layout.is_some()
            && presentation
                .control_requirements
                .as_ref()
                .is_some_and(|current| current.shares_resources_with(requirements))
            && presentation.last_rendered_layers == intent_layers;
        let exact_predecessor_delta = prepared_layout.is_some_and(|layout| {
            presentation
                .control_layout
                .as_ref()
                .zip(presentation.control_requirements.as_ref())
                .is_some_and(|(previous_layout, previous_requirements)| {
                    layout.is_incremental_from(previous_layout, previous_requirements)
                })
        });
        let incremental_body_update = static_control_changed
            && exact_predecessor_delta
            && presentation.last_rendered_layers == intent_layers;
        let mut incremental_resident = BTreeMap::new();
        let mut frame_empty_keys = Vec::new();
        if (render || control_only_candidate)
            && (!static_control_changed || incremental_body_update)
        {
            let mut retain_resource = |key: DatasetResourceKey| {
                if !is_planned_resident(&key) {
                    return Ok::<(), WgpuRenderRuntimeError>(());
                }
                let resource = upload_resources
                    .get(&key)
                    .cloned()
                    .or_else(|| empty_residents.get(&key).cloned())
                    .or_else(|| self.resident.get(&key).cloned())
                    .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
                if !resource.any_valid {
                    frame_empty_keys.push(key);
                }
                incremental_resident.insert(key, resource);
                Ok(())
            };
            for key in availability_changes.keys().copied() {
                retain_resource(key)?;
            }
            if incremental_body_update
                && let Some(delta) = prepared_layout.and_then(|layout| layout.delta.as_ref())
            {
                for key in delta.added.iter().copied() {
                    retain_resource(key)?;
                }
            }
        }
        let mut replacement_control_layout = None;
        let mut incremental_control_layout = None;
        let mut control_writes = Vec::new();
        let mut dense_control_fallback = false;
        let mut page_layout_constructions = 0_usize;
        let mut control_synced_keys = BTreeSet::new();
        if static_control_changed {
            let layout = if let Some(layout) = prepared_layout {
                layout.clone()
            } else {
                let layer_keys = intent
                    .layers()
                    .iter()
                    .map(|layer| layer.layer())
                    .collect::<Vec<_>>();
                prepare_static_presentation_layout_from_parts(
                    catalog,
                    planned_frame.requirements.prepared_body(),
                    &layer_keys,
                )?
            };
            let mut prefix = build_dynamic_control_prefix(&layout, catalog, intent)?;
            set_control_full_coverage(&mut prefix, coverage.is_full())?;
            if incremental_body_update {
                let changed_keys = availability_changes
                    .keys()
                    .copied()
                    .collect::<BTreeSet<_>>();
                let Some(delta_writes) = build_body_delta_control_writes(
                    &prefix,
                    intent.layers().len(),
                    &layout,
                    &changed_keys,
                    &incremental_resident,
                )?
                else {
                    return Err(WgpuRenderRuntimeError::ControlCapacityExceeded);
                };
                control_writes = delta_writes.writes;
                frame_empty_keys.extend(delta_writes.empty_keys);
                incremental_control_layout = Some(layout);
            } else {
                if layout.delta.is_some() {
                    return Err(WgpuRenderRuntimeError::PreparedStaticLayoutMismatch);
                }
                (control_writes, frame_empty_keys) = build_static_control_writes(
                    prefix,
                    intent.layers().len(),
                    &layout,
                    &self.resident,
                    &evicted,
                    &upload_resources,
                    &empty_residents,
                )?;
                page_layout_constructions = layout.layer_page_fields.len();
                replacement_control_layout = Some(layout);
            }
        } else if render {
            let layout = presentation
                .control_layout
                .as_ref()
                .expect("unchanged static control retains its prepared layout");
            let mut prefix = build_dynamic_control_prefix(layout, catalog, intent)?;
            set_control_full_coverage(&mut prefix, coverage.is_full())?;
            let changed_keys = availability_changes
                .keys()
                .copied()
                .collect::<BTreeSet<_>>();
            if let Some(writes) = build_incremental_control_writes(
                Some(&prefix),
                intent.layers().len(),
                layout,
                &changed_keys,
                &incremental_resident,
            )? {
                control_writes = writes;
            } else {
                // A fragmented delta would exceed the hard queue-write cap.
                // Publish the complete record slab in one write; the retained
                // sparse page table is unchanged and does not need publication.
                (control_writes, frame_empty_keys) = build_static_control_writes(
                    prefix,
                    intent.layers().len(),
                    layout,
                    &self.resident,
                    &evicted,
                    &upload_resources,
                    &empty_residents,
                )?;
                dense_control_fallback = true;
            }
        } else if control_only_candidate {
            let layout = presentation
                .control_layout
                .as_ref()
                .expect("a control-only publication retains its prepared layout");
            let changed_keys = availability_changes
                .keys()
                .copied()
                .collect::<BTreeSet<_>>();
            if let Some(writes) = build_incremental_control_writes(
                None,
                intent.layers().len(),
                layout,
                &changed_keys,
                &incremental_resident,
            )? {
                control_writes = writes;
                control_synced_keys = changed_keys;
            }
        }
        let publishes_control = render || !control_writes.is_empty();
        let control_publication_writes =
            control_writes.len() + usize::from(render && replacement_control_layout.is_some());
        debug_assert!(
            !static_control_changed || incremental_body_update || control_publication_writes <= 3,
            "a static control publication is prefix + dense records + page table"
        );
        if control_publication_writes > MAX_CONTROL_PUBLICATION_WRITES_PER_FRAME {
            return Err(WgpuRenderRuntimeError::ControlCapacityExceeded);
        }
        let control_bytes = control_writes
            .iter()
            .map(|write| write.bytes.len() as u64)
            .sum::<u64>()
            .saturating_add(replacement_control_layout.as_ref().map_or(0, |layout| {
                (layout.base_page_table.len() as u64).saturating_mul(4)
            }));
        if control_bytes > MAX_CONTROL_UPLOAD_BYTES {
            return Err(WgpuRenderRuntimeError::ControlCapacityExceeded);
        }
        if control_bytes > self.control_capacity_bytes {
            return Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::PageTable,
                requested_bytes: control_bytes,
                available_bytes: self.control_capacity_bytes,
            });
        }
        let transfer_bytes = uploads.iter().try_fold(control_bytes, |total, upload| {
            total
                .checked_add(upload.layout.allocation_bytes)
                .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)
        })?;
        if transfer_bytes > self.diagnostics.transfer_capacity_bytes {
            return Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::TransferStaging,
                requested_bytes: transfer_bytes,
                available_bytes: self.diagnostics.transfer_capacity_bytes,
            });
        }

        let capture_bytes = if render && self.config.validation_capture() {
            capture_allocation_bytes(intent.extent())?
        } else {
            0
        };
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
        let retained_control_bytes = replacement_control_layout
            .as_ref()
            .or(incremental_control_layout.as_ref())
            .map_or_else(
                || {
                    presentation
                        .control_layout
                        .as_ref()
                        .map_or(0, PreparedStaticPresentationLayout::logical_control_bytes)
                },
                PreparedStaticPresentationLayout::logical_control_bytes,
            );
        let required_control = control_buffer_capacity(retained_control_bytes)?;
        if required_control > self.control_capacity_bytes {
            return Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::PageTable,
                requested_bytes: required_control,
                available_bytes: self.control_capacity_bytes,
            });
        }
        // A full static-body replacement gets a fresh zero-initialized buffer.
        // A predecessor-bound stable-slot delta overwrites every removed or
        // reassigned record/page before the one submission, so it can retain
        // the existing buffer without exposing mixed body state.
        let replaces_control = render
            && ((!incremental_body_update && static_control_changed)
                || required_control > presentation.control_capacity);
        let display_peak_bytes = self
            .active_display_bytes()
            .saturating_add(if replaces_display {
                display_allocation_bytes(intent.extent(), self.config.validation_capture())?
            } else {
                0
            });
        let control_peak_bytes = self
            .active_control_bytes()
            .saturating_add(if replaces_control {
                required_control
            } else {
                0
            });
        let capture_peak_bytes = self.active_capture_bytes().saturating_add(capture_bytes);
        self.validate_other_capacity(control_peak_bytes, display_peak_bytes, capture_peak_bytes)?;

        let needs_submission = !uploads.is_empty() || render || publishes_control;
        let payload_staging_bytes = transfer_bytes.saturating_sub(control_bytes);
        let staging_slot = if uploads.is_empty() {
            None
        } else {
            let Some((slot, newly_allocated)) = self
                .upload_staging
                .acquire(&self.device, payload_staging_bytes)?
            else {
                self.diagnostics.backpressure_deferrals =
                    self.diagnostics.backpressure_deferrals.saturating_add(1);
                return Ok(FrameExecutionReport {
                    presentation: None,
                    frame: intent.frame(),
                    progress: None,
                    visited_resources: 0,
                    uploaded_resources: 0,
                    payload_upload_bytes: 0,
                    control_upload_bytes: 0,
                    command_buffers: 0,
                    queue_submissions: 0,
                    deferred_by_backpressure: true,
                    retained_updates_accepted: true,
                    cpu_timing: None,
                    gpu_timing: None,
                    validation_capture: None,
                    newly_resident_keys: Box::new([]),
                    evicted_keys: Box::new([]),
                });
            };
            if newly_allocated != 0 {
                self.diagnostics.explicit_staging_allocations = self
                    .diagnostics
                    .explicit_staging_allocations
                    .saturating_add(1);
                self.diagnostics.explicit_staging_bytes = self.upload_staging.reserved;
                self.diagnostics.peak_explicit_staging_bytes = self
                    .diagnostics
                    .peak_explicit_staging_bytes
                    .max(self.upload_staging.reserved);
            }
            Some(slot)
        };
        let mut command_buffers = 0_u32;
        let mut queue_submissions = 0_u32;
        let mut cpu_timing = None;
        let mut capture_ticket = None;
        let mut new_display = if replaces_display {
            Some(create_display(
                &self.device,
                intent.extent(),
                self.config.validation_capture(),
            )?)
        } else {
            None
        };
        let mut new_control = replaces_control.then(|| {
            let (buffer, render_bind_group, pick_bind_groups) =
                create_presentation_control_resources(
                    &self.device,
                    &self.render_bind_group_layout,
                    &self.pick,
                    &self.payload_segments,
                    required_control,
                    &mut self.diagnostics.bind_group_creations,
                );
            (
                buffer,
                render_bind_group,
                pick_bind_groups,
                required_control,
            )
        });
        let mut pending_capture = None;
        let timing_plan = self.timing.as_ref().and_then(|timing| {
            let payload_copy_timestamps = staging_slot.is_some() && timing.encoder_copy_timestamps;
            let render_pass_timestamps = render;
            let batch_envelope_timestamps = timing.encoder_copy_timestamps && needs_submission;
            (batch_envelope_timestamps || payload_copy_timestamps || render_pass_timestamps)
                .then_some(())?;
            timing
                .slots
                .iter()
                .position(|slot| matches!(slot.state, TimingSlotState::Free))
                .map(|slot| {
                    let ticket = GpuTimingTicket {
                        id: self.next_timing,
                        target: presentation_token,
                        generation: intent.frame(),
                        display_generation,
                        pass_kind: intent.view().pass_kind(),
                    };
                    let query_base = u32::try_from(slot).expect("bounded timing slot fits u32")
                        * TIMING_QUERY_WORDS;
                    TimingPlan {
                        slot,
                        ticket,
                        queries: TimingQueryLayout::new(
                            query_base,
                            batch_envelope_timestamps,
                            payload_copy_timestamps,
                            render_pass_timestamps,
                        ),
                    }
                })
        });
        let gpu_timing_ticket = timing_plan.map(|plan| plan.ticket);
        let mut control_publication_ns = None;
        let mut payload_staging_ns = None;
        let timing_prelude_submit_ns =
            timing_plan.and_then(|plan| self.submit_timing_batch_prelude(plan));
        let timing_prelude_submitted = timing_prelude_submit_ns.is_some();
        if timing_prelude_submitted {
            self.diagnostics.gpu_timing_prelude_submissions = self
                .diagnostics
                .gpu_timing_prelude_submissions
                .saturating_add(1);
        }

        if needs_submission {
            if publishes_control {
                let control_publication_start = cpu_timing_start.as_ref().map(|_| Instant::now());
                let control_buffer = new_control.as_ref().map_or_else(
                    || {
                        &self
                            .presentations
                            .get(&presentation_token)
                            .expect("presentation registration was checked before submission")
                            .control_buffer
                    },
                    |(buffer, _, _, _)| buffer,
                );
                // `write_buffer` uses WGPU's bounded staging belt and avoids a
                // fresh explicit staging buffer on the camera-only hot path.
                // Static tails are uploaded only when their structure changes.
                for write in &control_writes {
                    self.queue
                        .write_buffer(control_buffer, write.offset, &write.bytes);
                }
                if let Some(layout) = replacement_control_layout.as_ref() {
                    self.queue.write_buffer(
                        control_buffer,
                        layout.page_table_offset,
                        bytemuck::cast_slice::<u32, u8>(&layout.base_page_table),
                    );
                }
                control_publication_ns =
                    control_publication_start.as_ref().map(elapsed_nanoseconds);
            }
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mirante4d-wp09a-frame"),
                });
            if let Some(slot) = staging_slot {
                let payload_staging_start = cpu_timing_start.as_ref().map(|_| Instant::now());
                let padding_zero_bytes = self.upload_staging.stage(slot, &uploads);
                payload_staging_ns = payload_staging_start.as_ref().map(elapsed_nanoseconds);
                self.diagnostics.upload_staging_padding_zero_bytes = self
                    .diagnostics
                    .upload_staging_padding_zero_bytes
                    .saturating_add(padding_zero_bytes);
            }
            if let Some((beginning, _)) = timing_plan.and_then(|plan| plan.queries.payload_copy) {
                encoder.write_timestamp(
                    &self
                        .timing
                        .as_ref()
                        .expect("timing plan has resources")
                        .query_set,
                    beginning,
                );
            }
            if let Some(slot) = staging_slot {
                self.upload_staging.encode_copies(
                    slot,
                    &mut encoder,
                    &self.payload_segments,
                    &uploads,
                );
            }
            if let Some((_, end)) = timing_plan.and_then(|plan| plan.queries.payload_copy) {
                encoder.write_timestamp(
                    &self
                        .timing
                        .as_ref()
                        .expect("timing plan has resources")
                        .query_set,
                    end,
                );
            }
            if render {
                let display = new_display.as_ref().unwrap_or_else(|| {
                    &self
                        .presentations
                        .get(&presentation_token)
                        .expect("presentation registration was checked before submission")
                        .display
                });
                let pass_timestamps = timing_plan.and_then(|plan| {
                    plan.queries.render_pass.map(|(beginning, end)| {
                        (
                            &self
                                .timing
                                .as_ref()
                                .expect("timing plan has resources")
                                .query_set,
                            beginning,
                            end,
                        )
                    })
                });
                encode_render_pass(
                    &mut encoder,
                    if uses_mip_pipeline(intent) {
                        &self.mip_pipeline
                    } else {
                        &self.pipeline
                    },
                    new_control.as_ref().map_or_else(
                        || {
                            &self
                                .presentations
                                .get(&presentation_token)
                                .expect("presentation registration was checked before submission")
                                .render_bind_group
                        },
                        |(_, bind_group, _, _)| bind_group,
                    ),
                    display,
                    pass_timestamps,
                );
                if let Some((_, end)) = timing_plan.and_then(|plan| plan.queries.batch_envelope) {
                    encoder.write_timestamp(
                        &self
                            .timing
                            .as_ref()
                            .expect("timing plan has resources")
                            .query_set,
                        end,
                    );
                }
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

            if !render
                && let Some((_, end)) = timing_plan.and_then(|plan| plan.queries.batch_envelope)
            {
                encoder.write_timestamp(
                    &self
                        .timing
                        .as_ref()
                        .expect("timing plan has resources")
                        .query_set,
                    end,
                );
            }

            if let Some(plan) = timing_plan {
                let timing = self.timing.as_ref().expect("timing plan has resources");
                let base = u32::try_from(plan.slot).expect("bounded timing slot fits u32")
                    * TIMING_QUERY_WORDS;
                let resolve_offset = plan.slot as u64 * TIMING_RESOLVE_STRIDE;
                encoder.resolve_query_set(
                    &timing.query_set,
                    base..base + plan.queries.query_count,
                    &timing.resolve_buffer,
                    resolve_offset,
                );
                encoder.copy_buffer_to_buffer(
                    &timing.resolve_buffer,
                    resolve_offset,
                    &timing.slots[plan.slot].readback,
                    0,
                    u64::from(plan.queries.query_count) * 8,
                );
            }

            let command_buffer = encoder.finish();
            let cpu_planning_ns = cpu_timing_start.as_ref().map(|start| {
                elapsed_nanoseconds(start).saturating_sub(timing_prelude_submit_ns.unwrap_or(0))
            });
            let cpu_submit_start = cpu_timing_start.as_ref().map(|_| Instant::now());
            self.queue.submit([command_buffer]);
            let cpu_queue_submit_ns = cpu_submit_start.as_ref().map(|start| {
                elapsed_nanoseconds(start).saturating_add(timing_prelude_submit_ns.unwrap_or(0))
            });
            cpu_timing =
                cpu_planning_ns
                    .zip(cpu_queue_submit_ns)
                    .map(|(planning_ns, queue_submit_ns)| {
                        CpuFrameTiming::new(
                            planning_ns,
                            control_publication_ns,
                            payload_staging_ns,
                            queue_submit_ns,
                        )
                    });
            if let Some(timing) = cpu_timing {
                self.record_cpu_frame_timing(intent.frame(), timing);
            }
            if let Some(slot) = staging_slot {
                self.upload_staging.submitted(slot);
            }
            command_buffers = 1 + u32::from(timing_prelude_submitted);
            queue_submissions = 1 + u32::from(timing_prelude_submitted);
            let in_flight = Arc::clone(&self.in_flight_submissions);
            let submitted = in_flight.fetch_add(1, Ordering::AcqRel) + 1;
            self.queue.on_submitted_work_done(move || {
                in_flight.fetch_sub(1, Ordering::AcqRel);
            });
            self.diagnostics.current_in_flight_submissions = submitted;
            self.diagnostics.peak_in_flight_submissions =
                self.diagnostics.peak_in_flight_submissions.max(submitted);
            if let Some(pending) = pending_capture.as_ref() {
                pending.start_map();
            }
            if let Some(plan) = timing_plan {
                let mapped = Arc::new(Mutex::new(None));
                let callback = Arc::clone(&mapped);
                let timing = self.timing.as_mut().expect("timing plan has resources");
                timing.slots[plan.slot].readback.slice(..).map_async(
                    wgpu::MapMode::Read,
                    move |result| {
                        if let Ok(mut status) = callback.lock() {
                            *status = Some(result.map_err(|_| ()));
                        }
                    },
                );
                timing.slots[plan.slot].state = TimingSlotState::Pending {
                    ticket: plan.ticket,
                    mapped,
                    batch_envelope_timestamps: plan.queries.batch_envelope.is_some(),
                    payload_copy_timestamps: plan.queries.payload_copy.is_some(),
                    render_pass_timestamps: plan.queries.render_pass.is_some(),
                };
                self.next_timing = self.next_timing.saturating_add(1);
            }
        }

        // Commit only after every capacity/control/view preflight and the one
        // allowed submission have succeeded. Only the compact free-range
        // index and touched keys commit; payloads and the resident map are
        // never transaction-cloned.
        if !allocator_operations.is_empty() {
            apply_arena_operations(&mut self.payload_segments, &allocator_operations);
        }
        for upload in &uploads {
            for victim in &upload.victims {
                if let Some(resource) = self.resident.remove(victim) {
                    let age_entry = (resource.last_used_frame, *victim);
                    let segment = resource.segment as usize;
                    let removed_from_resident_age =
                        self.payload_resident_lru[segment].remove(&age_entry);
                    debug_assert!(removed_from_resident_age);
                    self.payload_evictable_lru[segment].remove(&age_entry);
                    self.diagnostics.resident_payload_used_bytes = self
                        .diagnostics
                        .resident_payload_used_bytes
                        .saturating_sub(resource.allocated_bytes);
                }
                self.record_eviction(*victim);
                self.diagnostics.residency_evictions =
                    self.diagnostics.residency_evictions.saturating_add(1);
            }
            if self.recently_evicted.remove(&upload.key).is_some() {
                self.diagnostics.residency_epoch_reuploads =
                    self.diagnostics.residency_epoch_reuploads.saturating_add(1);
            }
            self.resident.insert(upload.key, upload.resident.clone());
            let inserted = self.payload_resident_lru[upload.segment]
                .insert((upload.resident.last_used_frame, upload.key));
            debug_assert!(inserted);
            self.diagnostics.resident_payload_used_bytes = self
                .diagnostics
                .resident_payload_used_bytes
                .saturating_add(upload.resident.allocated_bytes);
        }
        for (key, resource) in &empty_residents {
            // An immutable resource key cannot change from empty to nonempty;
            // remove stale epoch bookkeeping without calling this an upload.
            self.recently_evicted.remove(key);
            let previous = self.resident.insert(*key, resource.clone());
            debug_assert!(previous.is_none());
            self.empty_resident_count += 1;
            let inserted = self
                .empty_resident_lru
                .insert((resource.last_used_frame, *key));
            debug_assert!(inserted);
        }
        self.commit_empty_resident_evictions(&empty_evictions);
        if !requirement_body_retained {
            self.touch_empty_keys(&frame_empty_keys, planned_frame.frame);
        }
        self.record_residency_changes(
            uploads
                .iter()
                .map(|upload| upload.key)
                .chain(empty_residents.keys().copied()),
            evicted.iter().copied(),
        );
        let presentation = self
            .presentations
            .get_mut(&presentation_token)
            .expect("presentation registration was checked before commit");
        presentation.frame_state = Some(planned_frame.clone());
        presentation.availability = Some(coverage);
        if let Some(display) = new_display.take() {
            presentation.display = display;
        }
        if let Some((buffer, render_bind_group, pick_bind_groups, capacity)) = new_control.take() {
            presentation.control_buffer = buffer;
            presentation.render_bind_group = render_bind_group;
            presentation.pick_bind_groups = pick_bind_groups;
            presentation.control_capacity = capacity;
            self.diagnostics.control_buffer_allocations = self
                .diagnostics
                .control_buffer_allocations
                .saturating_add(1);
            self.diagnostics.control_buffer_allocation_bytes = self
                .diagnostics
                .control_buffer_allocation_bytes
                .saturating_add(capacity);
        }
        if render {
            presentation.control_prefix = control_writes
                .first()
                .filter(|write| write.offset == 0)
                .map(|write| write.bytes.clone())
                .expect("every rendered frame preflights one dynamic prefix write");
            if let Some(layout) = replacement_control_layout.take() {
                presentation.control_layout = Some(layout);
            } else if let Some(layout) = incremental_control_layout.take() {
                presentation.control_layout = Some(layout);
            }
            presentation.control_requirements = Some(planned_frame.requirements.clone());
            presentation.last_rendered_frame = Some(intent.frame());
            presentation.last_rendered_timepoint = Some(intent.timepoint());
            presentation.last_progress = progress.clone();
            presentation.residency_dirty_keys.clear();
            presentation.last_rendered_volume =
                matches!(intent.view(), RenderViewIntent::Volume { .. });
            presentation.last_rendered_layers.clear();
            presentation
                .last_rendered_layers
                .extend(intent.layers().iter().map(|layer| layer.layer()));
            presentation.last_rendered_modes.clear();
            presentation
                .last_rendered_modes
                .extend(intent.layers().iter().map(|layer| {
                    let state = layer.render_state();
                    if state.mip_parameters().is_some() {
                        0
                    } else if state.dvr_parameters().is_some() {
                        1
                    } else {
                        2
                    }
                }));
            presentation.last_rendered_sampling.clear();
            presentation.last_rendered_sampling.extend(
                intent
                    .layers()
                    .iter()
                    .map(|layer| layer.render_state().sampling_policy()),
            );
        } else if !control_synced_keys.is_empty() {
            // Prefetch residency changes do not alter the displayed pixels,
            // but retaining their latest bitmap makes later O(1) promotion
            // rebind the already-proved coverage instead of a stale snapshot.
            presentation.last_progress = progress.clone();
            presentation
                .residency_dirty_keys
                .retain(|key| !control_synced_keys.contains(key));
        }
        if frame_changed {
            presentation.pending_capture = None;
        }
        if let Some(pending) = pending_capture {
            presentation.pending_capture = Some(pending);
            self.next_capture = self.next_capture.saturating_add(1);
        }
        if requirement_body_changed {
            if exact_predecessor_delta {
                let delta = prepared_layout
                    .and_then(|layout| layout.delta.as_ref())
                    .expect("an exact predecessor update has one body delta");
                for key in delta.removed.iter().copied() {
                    self.refresh_payload_pin_state(key);
                }
                for key in delta.added.iter().copied() {
                    if render {
                        self.touch_payload_key(key, planned_frame.frame);
                    } else {
                        self.refresh_payload_pin_state(key);
                    }
                }
                self.diagnostics.body_delta_pin_lru_keys = self
                    .diagnostics
                    .body_delta_pin_lru_keys
                    .saturating_add((delta.removed.len() + delta.added.len()) as u64);
            } else {
                if let Some(previous) = current_frame_state.as_ref() {
                    self.refresh_payload_pin_states(&previous.requirements);
                }
                if render {
                    self.touch_payload_keys(&planned_frame.requirements, planned_frame.frame);
                } else {
                    self.refresh_payload_pin_states(&planned_frame.requirements);
                }
            }
        }
        self.record_control_publication_writes(control_publication_writes);
        self.refresh_diagnostics(
            transfer_bytes,
            control_peak_bytes,
            display_peak_bytes,
            capture_peak_bytes,
            render,
            queue_submissions,
        );
        let hits = visited
            .iter()
            .filter(|key| self.resident.contains_key(key) && !uploaded.contains(key))
            .count() as u64;
        self.diagnostics.residency_hits = self.diagnostics.residency_hits.saturating_add(hits);
        self.diagnostics.residency_misses = self
            .diagnostics
            .residency_misses
            .saturating_add(visited.len() as u64 - hits);
        self.diagnostics.uploaded_resources = self
            .diagnostics
            .uploaded_resources
            .saturating_add(uploads.len() as u64);
        self.diagnostics.uploaded_payload_bytes = self
            .diagnostics
            .uploaded_payload_bytes
            .saturating_add(raw_upload_bytes);
        self.diagnostics.allocator_plans = self.diagnostics.allocator_plans.saturating_add(1);
        self.diagnostics.cold_coverage_membership_checks = self
            .diagnostics
            .cold_coverage_membership_checks
            .saturating_add(cold_coverage_membership_checks);
        self.diagnostics.page_layout_constructions = self
            .diagnostics
            .page_layout_constructions
            .saturating_add(page_layout_constructions as u64);
        if render {
            self.diagnostics.control_dense_fallbacks = self
                .diagnostics
                .control_dense_fallbacks
                .saturating_add(u64::from(dense_control_fallback));
            if static_control_changed && !incremental_body_update {
                self.diagnostics.control_static_rebuilds =
                    self.diagnostics.control_static_rebuilds.saturating_add(1);
                self.diagnostics.control_static_rebuild_bytes = self
                    .diagnostics
                    .control_static_rebuild_bytes
                    .saturating_add(control_bytes);
            } else {
                self.diagnostics.control_dynamic_updates =
                    self.diagnostics.control_dynamic_updates.saturating_add(1);
                self.diagnostics.control_dynamic_upload_bytes = self
                    .diagnostics
                    .control_dynamic_upload_bytes
                    .saturating_add(control_bytes);
            }
            if incremental_body_update {
                let delta = prepared_layout
                    .and_then(|layout| layout.delta.as_ref())
                    .expect("an incremental body update has one exact delta");
                self.diagnostics.control_body_delta_updates = self
                    .diagnostics
                    .control_body_delta_updates
                    .saturating_add(1);
                self.diagnostics.control_body_delta_keys = self
                    .diagnostics
                    .control_body_delta_keys
                    .saturating_add((delta.removed.len() + delta.added.len()) as u64);
                self.diagnostics.control_body_delta_page_entries = self
                    .diagnostics
                    .control_body_delta_page_entries
                    .saturating_add(delta.page_patches.len() as u64);
            }
        } else if publishes_control {
            self.diagnostics.control_dynamic_updates =
                self.diagnostics.control_dynamic_updates.saturating_add(1);
            self.diagnostics.control_dynamic_upload_bytes = self
                .diagnostics
                .control_dynamic_upload_bytes
                .saturating_add(control_bytes);
        }

        Ok(FrameExecutionReport {
            presentation: render.then(|| {
                PresentedFrame::new(
                    presentation_token,
                    intent.extent(),
                    progress.clone().expect("a render has progress"),
                )
            }),
            frame: intent.frame(),
            progress,
            visited_resources: visited.len(),
            uploaded_resources: uploads.len(),
            payload_upload_bytes: raw_upload_bytes,
            control_upload_bytes: control_bytes,
            command_buffers,
            queue_submissions,
            deferred_by_backpressure: false,
            retained_updates_accepted: true,
            cpu_timing,
            gpu_timing: gpu_timing_ticket,
            validation_capture: capture_ticket,
            newly_resident_keys: uploads
                .iter()
                .map(|upload| upload.key)
                .chain(empty_residents.keys().copied())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            evicted_keys: evicted
                .iter()
                .copied()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        })
    }

    fn payload_is_pinned(&self, key: DatasetResourceKey) -> bool {
        self.presentations.values().any(|presentation| {
            presentation
                .frame_state
                .as_ref()
                .is_some_and(|state| state.requirements.contains_resource(key))
        })
    }

    fn payload_is_pinned_except(
        &self,
        key: DatasetResourceKey,
        excluded: PresentationToken,
    ) -> bool {
        self.presentations.iter().any(|(token, presentation)| {
            *token != excluded
                && presentation
                    .frame_state
                    .as_ref()
                    .is_some_and(|state| state.requirements.contains_resource(key))
        })
    }

    fn refresh_payload_pin_state(&mut self, key: DatasetResourceKey) {
        let Some(resource) = self.resident.get(&key) else {
            return;
        };
        if resource.allocated_bytes == 0 {
            return;
        }
        let segment = resource.segment as usize;
        let entry = (resource.last_used_frame, key);
        if self.payload_is_pinned(key) {
            self.payload_evictable_lru[segment].remove(&entry);
        } else {
            self.payload_evictable_lru[segment].insert(entry);
        }
    }

    fn refresh_payload_pin_states(&mut self, requirements: &RenderRequirements) {
        for requirement in requirements.resources() {
            self.refresh_payload_pin_state(requirement.key());
        }
    }

    fn touch_payload_keys(&mut self, requirements: &RenderRequirements, frame: FrameIdentity) {
        for requirement in requirements.resources() {
            self.touch_payload_key(requirement.key(), frame);
        }
    }

    fn touch_payload_key(&mut self, key: DatasetResourceKey, frame: FrameIdentity) {
        let frame = frame.get();
        let Some(resource) = self.resident.get(&key) else {
            return;
        };
        if resource.allocated_bytes == 0 {
            return;
        }
        let segment = resource.segment as usize;
        let previous = (resource.last_used_frame, key);
        if resource.last_used_frame != frame {
            let removed = self.payload_resident_lru[segment].remove(&previous);
            debug_assert!(removed);
            self.payload_evictable_lru[segment].remove(&previous);
            self.resident
                .get_mut(&key)
                .expect("resident key was checked before age update")
                .last_used_frame = frame;
            let inserted = self.payload_resident_lru[segment].insert((frame, key));
            debug_assert!(inserted);
        }
        self.refresh_payload_pin_state(key);
    }

    fn replacement_release_candidates(
        &self,
        presentation_token: PresentationToken,
        current: Option<&FrameState>,
        planned: &RenderRequirements,
        prepared_layout: Option<&PreparedStaticPresentationLayout>,
    ) -> [BTreeSet<(u64, DatasetResourceKey)>; MAX_PAYLOAD_SEGMENTS] {
        let mut candidates = std::array::from_fn(|_| BTreeSet::new());
        let Some(current) = current else {
            return candidates;
        };
        if current.requirements.shares_resources_with(planned) {
            return candidates;
        }
        let exact_removed = prepared_layout.and_then(|layout| {
            let previous_layout = self
                .presentations
                .get(&presentation_token)?
                .control_layout
                .as_ref()?;
            layout
                .is_incremental_from(previous_layout, &current.requirements)
                .then(|| layout.incremental_removed_resources())
        });
        let removed = exact_removed.map_or_else(
            || {
                current
                    .requirements
                    .resources()
                    .map(|requirement| requirement.key())
                    .filter(|key| !planned.contains_resource(*key))
                    .collect::<Vec<_>>()
            },
            <[DatasetResourceKey]>::to_vec,
        );
        for key in removed
            .into_iter()
            .filter(|key| !self.payload_is_pinned_except(*key, presentation_token))
        {
            let Some(resource) = self.resident.get(&key) else {
                continue;
            };
            if resource.allocated_bytes != 0 {
                candidates[resource.segment as usize].insert((resource.last_used_frame, key));
            }
        }
        candidates
    }

    fn next_payload_eviction_candidate(
        &self,
        segment: usize,
        after: Option<(u64, DatasetResourceKey)>,
        replacement_releases: &BTreeSet<(u64, DatasetResourceKey)>,
        planned: &RenderRequirements,
    ) -> Option<(u64, DatasetResourceKey, u64, u64)> {
        let mut cursor = after;
        loop {
            let globally_evictable = next_age_entry(&self.payload_evictable_lru[segment], cursor);
            let replacement_release = next_age_entry(replacement_releases, cursor);
            let entry = match (globally_evictable, replacement_release) {
                (Some(left), Some(right)) => left.min(right),
                (Some(entry), None) | (None, Some(entry)) => entry,
                (None, None) => return None,
            };
            cursor = Some(entry);
            if planned.contains_resource(entry.1) {
                continue;
            }
            let resource = self
                .resident
                .get(&entry.1)
                .expect("payload age indexes point to resident resources");
            debug_assert_ne!(resource.allocated_bytes, 0);
            debug_assert_eq!(resource.segment as usize, segment);
            return Some((entry.0, entry.1, resource.offset, resource.allocated_bytes));
        }
    }

    fn record_eviction(&mut self, key: DatasetResourceKey) {
        let sequence = self.next_eviction_sequence;
        self.next_eviction_sequence = self.next_eviction_sequence.saturating_add(1);
        self.recently_evicted.insert(key, sequence);
        self.recently_evicted_order.push_back((sequence, key));
        while self.recently_evicted.len() > MAX_RENDER_REQUIREMENTS
            || self.recently_evicted_order.len() > MAX_RENDER_REQUIREMENTS * 2
        {
            if let Some((expired_sequence, expired)) = self.recently_evicted_order.pop_front()
                && self.recently_evicted.get(&expired) == Some(&expired_sequence)
            {
                self.recently_evicted.remove(&expired);
            }
        }
    }

    fn validate_presentation_activation(
        &self,
        token: PresentationToken,
    ) -> Result<(), WgpuRenderRuntimeError> {
        let target_active = self
            .presentations
            .get(&token)
            .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered { token })?
            .frame_state
            .is_some();
        let active = self
            .presentations
            .values()
            .filter(|presentation| presentation.frame_state.is_some())
            .count();
        validate_active_presentation_capacity(active, target_active)
    }

    fn record_residency_changes<I, R>(&mut self, additions: I, removals: R)
    where
        I: IntoIterator<Item = DatasetResourceKey>,
        R: IntoIterator<Item = DatasetResourceKey>,
    {
        let removals = removals.into_iter().collect::<BTreeSet<_>>();
        let keys = additions
            .into_iter()
            .chain(removals.iter().copied())
            .collect::<BTreeSet<_>>();
        if keys.is_empty() {
            return;
        }
        self.residency_epoch = self.residency_epoch.saturating_add(1);
        if !removals.is_empty() {
            self.residency_invalidation_epoch = self.residency_invalidation_epoch.saturating_add(1);
        }
        for presentation in self.presentations.values_mut() {
            for key in &keys {
                if presentation
                    .control_requirements
                    .as_ref()
                    .is_some_and(|requirements| requirements.contains_resource(*key))
                {
                    presentation.residency_dirty_keys.insert(*key);
                }
            }
        }
    }

    fn presentation_has_relevant_residency_changes(
        &self,
        presentation: &PresentationState,
    ) -> bool {
        !presentation.residency_dirty_keys.is_empty()
    }

    fn plan_empty_resident_evictions(
        &self,
        replacing_presentation: PresentationToken,
        planned_requirements: &RenderRequirements,
        incoming_records: usize,
    ) -> Result<Vec<DatasetResourceKey>, WgpuRenderRuntimeError> {
        let predicted_records = self
            .empty_resident_count
            .checked_add(incoming_records)
            .ok_or(WgpuRenderRuntimeError::ResidentMetadataCapacityExceeded {
                requested_records: usize::MAX,
                maximum_records: MAX_EMPTY_RESIDENTS,
                requested_bytes: u64::MAX,
                available_bytes: EMPTY_RESIDENT_METADATA_CAPACITY_BYTES,
            })?;
        let excess = predicted_records.saturating_sub(MAX_EMPTY_RESIDENTS);
        if excess == 0 {
            return Ok(Vec::new());
        }
        let evictions =
            oldest_unprotected_keys(self.empty_resident_lru.iter().copied(), excess, |key| {
                planned_requirements.contains_resource(key)
                    || self.presentations.iter().any(|(token, presentation)| {
                        *token != replacing_presentation
                            && presentation
                                .frame_state
                                .as_ref()
                                .is_some_and(|state| state.requirements.contains_resource(key))
                    })
            });
        let minimum_retained = predicted_records.saturating_sub(evictions.len());
        validate_empty_resident_metadata_capacity(minimum_retained)?;
        Ok(evictions)
    }

    fn commit_empty_resident_evictions(&mut self, evictions: &[DatasetResourceKey]) {
        for key in evictions {
            let removed = self
                .resident
                .remove(key)
                .expect("planned empty metadata eviction remains resident until commit");
            debug_assert!(!removed.any_valid);
            let removed_from_lru = self
                .empty_resident_lru
                .remove(&(removed.last_used_frame, *key));
            debug_assert!(removed_from_lru);
            self.empty_resident_count = self.empty_resident_count.saturating_sub(1);
            self.record_eviction(*key);
            self.diagnostics.residency_evictions =
                self.diagnostics.residency_evictions.saturating_add(1);
        }
        debug_assert!(validate_empty_resident_metadata_capacity(self.empty_resident_count).is_ok());
    }

    fn touch_empty_keys(&mut self, keys: &[DatasetResourceKey], frame: FrameIdentity) {
        let frame = frame.get();
        for key in keys {
            let Some(resource) = self.resident.get_mut(key) else {
                continue;
            };
            if resource.any_valid || resource.last_used_frame == frame {
                continue;
            }
            let removed = self
                .empty_resident_lru
                .remove(&(resource.last_used_frame, *key));
            debug_assert!(removed);
            resource.last_used_frame = frame;
            let inserted = self.empty_resident_lru.insert((frame, *key));
            debug_assert!(inserted);
        }
    }

    fn collect_gpu_timings(&mut self) -> Result<(), WgpuRenderRuntimeError> {
        let Some(timing) = self.timing.as_mut() else {
            return Ok(());
        };
        let mut ready = Vec::new();
        let mut failures = 0_u64;
        for slot in &mut timing.slots {
            let (
                ticket,
                mapped,
                batch_envelope_timestamps,
                payload_copy_timestamps,
                render_pass_timestamps,
            ) = match &slot.state {
                TimingSlotState::Free => continue,
                TimingSlotState::Pending {
                    ticket,
                    mapped,
                    batch_envelope_timestamps,
                    payload_copy_timestamps,
                    render_pass_timestamps,
                } => (
                    *ticket,
                    Arc::clone(mapped),
                    *batch_envelope_timestamps,
                    *payload_copy_timestamps,
                    *render_pass_timestamps,
                ),
            };
            let status = match mapped.lock() {
                Ok(status) => status.to_owned(),
                Err(_) => {
                    slot.readback.unmap();
                    slot.state = TimingSlotState::Free;
                    failures = failures.saturating_add(1);
                    continue;
                }
            };
            match status {
                None => continue,
                Some(Err(())) => {
                    slot.readback.unmap();
                    slot.state = TimingSlotState::Free;
                    failures = failures.saturating_add(1);
                }
                Some(Ok(())) => {
                    let mapped_bytes = slot.readback.slice(..).get_mapped_range();
                    let read_u64 = |offset: usize| {
                        u64::from_le_bytes(
                            mapped_bytes[offset..offset + 8]
                                .try_into()
                                .expect("timestamp word is eight bytes"),
                        )
                    };
                    let elapsed = |begin: u64, end: u64| {
                        if end < begin {
                            return Err(WgpuRenderRuntimeError::GpuTimingFailed);
                        }
                        let nanoseconds =
                            (end - begin) as f64 * f64::from(timing.timestamp_period_ns);
                        if !nanoseconds.is_finite() || nanoseconds < 0.0 {
                            return Err(WgpuRenderRuntimeError::GpuTimingFailed);
                        }
                        Ok(nanoseconds.round() as u64)
                    };
                    let mut byte_offset = 0_usize;
                    let batch_gpu_envelope_ns = if batch_envelope_timestamps {
                        let duration = elapsed(read_u64(byte_offset), read_u64(byte_offset + 8));
                        byte_offset += 16;
                        Some(duration)
                    } else {
                        None
                    };
                    let payload_copy_ns = if payload_copy_timestamps {
                        let duration = elapsed(read_u64(byte_offset), read_u64(byte_offset + 8));
                        byte_offset += 16;
                        Some(duration)
                    } else {
                        None
                    };
                    let render_pass_ns = if render_pass_timestamps {
                        Some(elapsed(read_u64(byte_offset), read_u64(byte_offset + 8)))
                    } else {
                        None
                    };
                    let result =
                        batch_gpu_envelope_ns
                            .transpose()
                            .and_then(|batch_gpu_envelope_ns| {
                                payload_copy_ns.transpose().and_then(|payload_copy_ns| {
                                    render_pass_ns.transpose().map(|render_pass_ns| {
                                        (batch_gpu_envelope_ns, payload_copy_ns, render_pass_ns)
                                    })
                                })
                            });
                    drop(mapped_bytes);
                    slot.readback.unmap();
                    slot.state = TimingSlotState::Free;
                    match result {
                        Ok(result) => ready.push((ticket, result)),
                        Err(_) => failures = failures.saturating_add(1),
                    }
                }
            }
        }
        for (ticket, (batch_gpu_envelope_ns, payload_copy_ns, render_pass_ns)) in ready {
            let result = GpuFrameTiming {
                ticket,
                batch_gpu_envelope_ns,
                payload_copy_ns,
                render_pass_ns,
            };
            timing.completed.insert(ticket, result);
            timing.completed_order.push_back(ticket);
            while timing.completed_order.len() > MAX_COMPLETED_TIMINGS {
                if let Some(expired) = timing.completed_order.pop_front() {
                    timing.completed.remove(&expired);
                }
            }
            self.diagnostics.completed_gpu_timings =
                self.diagnostics.completed_gpu_timings.saturating_add(1);
            let timing_is_current =
                self.presentations
                    .get(&ticket.target)
                    .is_some_and(|presentation| {
                        presentation.last_rendered_frame == Some(ticket.generation)
                    });
            if timing_is_current {
                self.diagnostics.last_gpu_batch_envelope_ns = result.batch_gpu_envelope_ns;
                self.diagnostics.last_gpu_payload_copy_ns = result.payload_copy_ns;
                self.diagnostics.last_gpu_render_pass_ns = result.render_pass_ns;
            }
        }
        self.diagnostics.gpu_timing_failures = self
            .diagnostics
            .gpu_timing_failures
            .saturating_add(failures);
        if failures == 0 {
            Ok(())
        } else {
            Err(WgpuRenderRuntimeError::GpuTimingFailed)
        }
    }

    pub(super) fn poll_gpu_timing(
        &mut self,
        ticket: GpuTimingTicket,
    ) -> Result<Option<GpuFrameTiming>, WgpuRenderRuntimeError> {
        if self.timing.is_none() {
            return Err(WgpuRenderRuntimeError::UnknownGpuTiming);
        }
        self.device
            .poll(wgpu::PollType::Poll)
            .map_err(|_| WgpuRenderRuntimeError::GpuTimingFailed)?;
        self.collect_gpu_timings()?;
        let timing = self.timing.as_mut().expect("timing support was checked");
        if let Some(result) = timing.completed.remove(&ticket) {
            timing.completed_order.retain(|current| *current != ticket);
            return Ok(Some(result));
        }
        if timing.slots.iter().any(|slot| {
            matches!(
                slot.state,
                TimingSlotState::Pending {
                    ticket: pending,
                    ..
                } if pending == ticket
            )
        }) {
            return Ok(None);
        }
        Err(WgpuRenderRuntimeError::UnknownGpuTiming)
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

    pub(super) fn request_pick(
        &mut self,
        presentation_token: PresentationToken,
        query: VolumePickQuery,
    ) -> Result<VolumePickTicket, WgpuRenderRuntimeError> {
        if query.token() != presentation_token {
            return Err(WgpuRenderRuntimeError::PickQueryMismatch);
        }
        self.device
            .poll(wgpu::PollType::Poll)
            .map_err(|_| WgpuRenderRuntimeError::VolumePickFailed)?;
        self.diagnostics.current_in_flight_submissions =
            self.in_flight_submissions.load(Ordering::Acquire);
        self.sync_validation_errors();
        if self.diagnostics.validation_error_count != 0 {
            return Err(WgpuRenderRuntimeError::BackendValidation);
        }
        let in_flight = self.in_flight_submissions.load(Ordering::Acquire);
        if in_flight >= MAX_IN_FLIGHT_SUBMISSIONS {
            self.diagnostics.backpressure_deferrals =
                self.diagnostics.backpressure_deferrals.saturating_add(1);
            self.diagnostics.pick_backpressure_deferrals = self
                .diagnostics
                .pick_backpressure_deferrals
                .saturating_add(1);
            return Err(WgpuRenderRuntimeError::PickBackpressure);
        }
        let slot_index = self
            .pick
            .slots
            .iter()
            .position(|slot| matches!(slot.state, PickSlotState::Free))
            .ok_or(WgpuRenderRuntimeError::PickCapacityExceeded)?;

        {
            let presentation = self.presentations.get(&presentation_token).ok_or(
                WgpuRenderRuntimeError::PresentationNotRegistered {
                    token: presentation_token,
                },
            )?;
            let layer_index = presentation
                .last_rendered_layers
                .iter()
                .position(|layer| *layer == query.layer())
                .ok_or(WgpuRenderRuntimeError::PickFrameUnavailable)?;
            let mode = presentation
                .last_rendered_modes
                .get(layer_index)
                .copied()
                .ok_or(WgpuRenderRuntimeError::PickFrameUnavailable)?;
            if presentation
                .last_rendered_sampling
                .get(layer_index)
                .is_none()
            {
                return Err(WgpuRenderRuntimeError::PickFrameUnavailable);
            }
            let policy_matches_mode = matches!(
                (mode, query.policy()),
                (0, VolumePickPolicy::MipArgmax)
                    | (1, VolumePickPolicy::MaximumOpacityContribution)
                    | (2, VolumePickPolicy::FirstThresholdHit)
            );
            if !policy_matches_mode {
                return Err(WgpuRenderRuntimeError::PickQueryMismatch);
            }
            if presentation.last_rendered_frame != Some(query.frame())
                || presentation.last_rendered_timepoint != Some(query.timepoint())
                || presentation.display.extent != query.extent()
                || !presentation.last_rendered_volume
            {
                return Err(WgpuRenderRuntimeError::PickFrameUnavailable);
            }
            if self.presentation_has_relevant_residency_changes(presentation) {
                return Err(WgpuRenderRuntimeError::PickFrameUnavailable);
            }
        }
        let policy = match query.policy() {
            VolumePickPolicy::FirstThresholdHit => 0_u32,
            VolumePickPolicy::MipArgmax => 1,
            VolumePickPolicy::MaximumOpacityContribution => 2,
        };
        let render_pixel = query.render_pixel().map(|value| value as f32);
        if render_pixel.into_iter().any(|value| !value.is_finite()) {
            return Err(WgpuRenderRuntimeError::PickQueryMismatch);
        }
        let query_words = [
            render_pixel[0].to_bits(),
            render_pixel[1].to_bits(),
            query.layer().ordinal(),
            policy,
            0,
            0,
            0,
            0,
        ];
        let staging_bytes = bytemuck::cast_slice(&query_words);
        let transfer_bytes = staging_bytes.len() as u64;
        if transfer_bytes > self.diagnostics.transfer_capacity_bytes {
            return Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::TransferStaging,
                requested_bytes: transfer_bytes,
                available_bytes: self.diagnostics.transfer_capacity_bytes,
            });
        }

        let ticket = VolumePickTicket::new(self.next_pick, presentation_token, query.frame())
            .map_err(|_| WgpuRenderRuntimeError::PickTicketExhausted)?;
        let next_pick = self
            .next_pick
            .checked_add(1)
            .ok_or(WgpuRenderRuntimeError::PickTicketExhausted)?;
        let staging =
            mapped_staging_buffer(&self.device, "mirante4d-vp05-pick-staging", staging_bytes);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-vp05-pick"),
            });
        let slot = &mut self.pick.slots[slot_index];
        encoder.copy_buffer_to_buffer(&staging, 0, &slot.query_buffer, 0, PICK_QUERY_BYTES);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-vp05-pick-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pick.pipeline);
            pass.set_bind_group(
                0,
                &self
                    .presentations
                    .get(&presentation_token)
                    .expect("presentation registration was checked before pick submission")
                    .pick_bind_groups[slot_index],
                &[],
            );
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&slot.output_buffer, 0, &slot.readback, 0, PICK_OUTPUT_BYTES);
        self.queue.submit([encoder.finish()]);
        let in_flight_counter = Arc::clone(&self.in_flight_submissions);
        let submitted = in_flight_counter.fetch_add(1, Ordering::AcqRel) + 1;
        self.queue.on_submitted_work_done(move || {
            in_flight_counter.fetch_sub(1, Ordering::AcqRel);
        });
        let mapped = Arc::new(Mutex::new(None));
        let callback = Arc::clone(&mapped);
        slot.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                if let Ok(mut status) = callback.lock() {
                    *status = Some(result.map_err(|_| ()));
                }
            });
        slot.state = PickSlotState::Pending {
            ticket,
            query,
            mapped,
        };
        self.next_pick = next_pick;
        self.diagnostics.current_in_flight_submissions = submitted;
        self.diagnostics.peak_in_flight_submissions =
            self.diagnostics.peak_in_flight_submissions.max(submitted);
        self.diagnostics.queue_submissions = self.diagnostics.queue_submissions.saturating_add(1);
        self.diagnostics.pick_submissions = self.diagnostics.pick_submissions.saturating_add(1);
        self.diagnostics.peak_transfer_bytes =
            self.diagnostics.peak_transfer_bytes.max(transfer_bytes);
        drop(staging);
        Ok(ticket)
    }

    pub(super) fn poll_pick(
        &mut self,
        ticket: VolumePickTicket,
    ) -> Result<Option<VolumePickResult>, WgpuRenderRuntimeError> {
        self.device
            .poll(wgpu::PollType::Poll)
            .map_err(|_| WgpuRenderRuntimeError::VolumePickFailed)?;
        self.diagnostics.current_in_flight_submissions =
            self.in_flight_submissions.load(Ordering::Acquire);
        self.sync_validation_errors();
        if self.diagnostics.validation_error_count != 0 {
            return Err(WgpuRenderRuntimeError::BackendValidation);
        }
        let slot_index = self
            .pick
            .slots
            .iter()
            .position(|slot| {
                matches!(
                    slot.state,
                    PickSlotState::Pending {
                        ticket: pending,
                        ..
                    } if pending == ticket
                )
            })
            .ok_or(WgpuRenderRuntimeError::UnknownVolumePick)?;
        let slot = &mut self.pick.slots[slot_index];
        let PickSlotState::Pending { query, mapped, .. } = &slot.state else {
            return Err(WgpuRenderRuntimeError::UnknownVolumePick);
        };
        let query = *query;
        let status = mapped
            .lock()
            .map_err(|_| WgpuRenderRuntimeError::VolumePickFailed)?
            .to_owned();
        match status {
            None => Ok(None),
            Some(Err(())) => {
                slot.state = PickSlotState::Free;
                Err(WgpuRenderRuntimeError::VolumePickFailed)
            }
            Some(Ok(())) => {
                let mapped_bytes = slot.readback.slice(..).get_mapped_range();
                let result = decode_pick_result(query, &mapped_bytes);
                drop(mapped_bytes);
                slot.readback.unmap();
                slot.state = PickSlotState::Free;
                let result = result?;
                self.diagnostics.completed_picks =
                    self.diagnostics.completed_picks.saturating_add(1);
                Ok(Some(result))
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
        let requirement_keys_match = current_frame_state
            .is_some_and(|current| current.requirements.shares_resources_with(requirements));
        // Scale/identity facts are properties of the immutable key body. Reuse
        // them across progressive passes and camera-only frames. Page
        // occupancy/hash construction happens exactly once in the transactional
        // static-control build below, rather than once here and once there.
        if !requirement_keys_match {
            validate_requirement_contract(requirements)?;
        }
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
        if leases.is_empty() {
            return Ok(BTreeMap::new());
        }
        let mut by_key = BTreeMap::new();
        for lease in leases {
            let key = lease.key();
            if !requirements.contains_resource(key) {
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
        visit_requirements: bool,
    ) -> Result<(FrameState, Vec<DatasetResourceKey>), WgpuRenderRuntimeError> {
        let mut state = match current_frame_state {
            Some(current) if current.requirements.shares_resources_with(requirements) => {
                let mut current = current.clone();
                current.frame = requirements.frame();
                current.requirements = requirements.clone();
                current
            }
            Some(current) if current.frame == requirements.frame() => {
                return Err(WgpuRenderRuntimeError::RequirementSetChanged);
            }
            _ => FrameState {
                frame: requirements.frame(),
                requirements: requirements.clone(),
                cursor: 0,
            },
        };
        if !visit_requirements {
            return Ok((state, Vec::new()));
        }
        let resource_count = state.requirements.len();
        let remaining = resource_count.saturating_sub(state.cursor);
        let count = remaining.min(MAX_VISITS);
        let visited =
            state.requirements.resource_keys()[state.cursor..state.cursor + count].to_vec();
        state.cursor += count;
        if state.cursor == resource_count {
            state.cursor = 0;
        }
        Ok((state, visited))
    }

    fn refresh_diagnostics(
        &mut self,
        transfer_bytes: u64,
        control_bytes: u64,
        display_bytes: u64,
        capture_bytes: u64,
        rendered: bool,
        submissions: u32,
    ) {
        let empty_metadata_bytes = empty_resident_metadata_bytes(self.empty_resident_count);
        self.diagnostics.empty_resident_metadata_records = self.empty_resident_count;
        self.diagnostics.empty_resident_metadata_bytes = empty_metadata_bytes;
        self.diagnostics.peak_empty_resident_metadata_bytes = self
            .diagnostics
            .peak_empty_resident_metadata_bytes
            .max(empty_metadata_bytes);
        self.diagnostics.peak_resident_payload_used_bytes = self
            .diagnostics
            .peak_resident_payload_used_bytes
            .max(self.diagnostics.resident_payload_used_bytes);
        self.diagnostics.peak_transfer_bytes =
            self.diagnostics.peak_transfer_bytes.max(transfer_bytes);
        self.diagnostics.peak_page_table_bytes =
            self.diagnostics.peak_page_table_bytes.max(control_bytes);
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
        self.diagnostics.current_in_flight_submissions =
            self.in_flight_submissions.load(Ordering::Acquire);
        self.sync_validation_errors();
    }

    fn cpu_timing_start(&self) -> Option<Instant> {
        self.diagnostics.gpu_timing_enabled.then(Instant::now)
    }

    /// Places the diagnostic batch-envelope start before any queue-write
    /// publication. WGPU has no timestamp command on `Queue`, so the opt-in
    /// qualification path needs one explicitly counted prelude submission.
    fn submit_timing_batch_prelude(&self, plan: TimingPlan) -> Option<u64> {
        let (beginning, _) = plan.queries.batch_envelope?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-timing-batch-envelope-prelude"),
            });
        encoder.write_timestamp(
            &self
                .timing
                .as_ref()
                .expect("a timing plan retains timing resources")
                .query_set,
            beginning,
        );
        let started = Instant::now();
        self.queue.submit([encoder.finish()]);
        Some(elapsed_nanoseconds(&started))
    }

    fn record_cpu_frame_timing(&mut self, frame: FrameIdentity, timing: CpuFrameTiming) {
        let planning_ns = timing.planning_ns();
        let control_publication_ns = timing.control_publication_ns();
        let payload_staging_ns = timing.payload_staging_ns();
        let queue_submit_ns = timing.queue_submit_ns();
        self.diagnostics.completed_cpu_timings =
            self.diagnostics.completed_cpu_timings.saturating_add(1);
        self.diagnostics.last_cpu_timing_frame = Some(frame.get());
        self.diagnostics.last_cpu_planning_ns = Some(planning_ns);
        self.diagnostics.last_cpu_control_publication_ns = control_publication_ns;
        self.diagnostics.last_cpu_payload_staging_ns = payload_staging_ns;
        self.diagnostics.last_cpu_queue_submit_ns = Some(queue_submit_ns);
        self.diagnostics.total_cpu_planning_ns = self
            .diagnostics
            .total_cpu_planning_ns
            .saturating_add(planning_ns);
        self.diagnostics.total_cpu_control_publication_ns = self
            .diagnostics
            .total_cpu_control_publication_ns
            .saturating_add(control_publication_ns.unwrap_or(0));
        self.diagnostics.total_cpu_payload_staging_ns = self
            .diagnostics
            .total_cpu_payload_staging_ns
            .saturating_add(payload_staging_ns.unwrap_or(0));
        self.diagnostics.total_cpu_queue_submit_ns = self
            .diagnostics
            .total_cpu_queue_submit_ns
            .saturating_add(queue_submit_ns);
    }

    fn record_control_publication_writes(&mut self, writes: usize) {
        self.diagnostics.control_publication_writes = self
            .diagnostics
            .control_publication_writes
            .saturating_add(writes as u64);
        self.diagnostics.peak_control_publication_writes_per_frame = self
            .diagnostics
            .peak_control_publication_writes_per_frame
            .max(writes);
    }

    fn sync_validation_errors(&mut self) {
        self.diagnostics.validation_error_count = self
            .validation_error_count
            .load(Ordering::Relaxed)
            .try_into()
            .unwrap_or(u64::MAX);
    }
}

fn elapsed_nanoseconds(start: &Instant) -> u64 {
    start.elapsed().as_nanos().try_into().unwrap_or(u64::MAX)
}

fn decode_pick_result(
    query: VolumePickQuery,
    bytes: &[u8],
) -> Result<VolumePickResult, WgpuRenderRuntimeError> {
    if bytes.len() < PICK_OUTPUT_BYTES as usize {
        return Err(WgpuRenderRuntimeError::VolumePickFailed);
    }
    let words = bytes
        .chunks_exact(4)
        .take(PICK_OUTPUT_WORDS)
        .map(|word| u32::from_le_bytes(word.try_into().expect("pick word is four bytes")))
        .collect::<Vec<_>>();
    if words[8] != PICK_OUTPUT_MAGIC {
        return Err(WgpuRenderRuntimeError::VolumePickFailed);
    }
    let completeness = match words[1] {
        0 => VolumePickCompleteness::Exact,
        1 => VolumePickCompleteness::Approximate,
        2 => VolumePickCompleteness::Incomplete,
        _ => return Err(WgpuRenderRuntimeError::VolumePickFailed),
    };
    match words[0] {
        0 => Ok(VolumePickResult::empty(query, completeness)),
        1 | 2 => {
            let value = match words[2] {
                1 => VolumePickValue::IntensityU8(
                    u8::try_from(words[3]).map_err(|_| WgpuRenderRuntimeError::VolumePickFailed)?,
                ),
                2 => VolumePickValue::IntensityU16(
                    u16::try_from(words[3])
                        .map_err(|_| WgpuRenderRuntimeError::VolumePickFailed)?,
                ),
                4 => VolumePickValue::IntensityF32(f32::from_bits(words[3])),
                _ => return Err(WgpuRenderRuntimeError::VolumePickFailed),
            };
            let world = WorldPoint3::new(
                f64::from(f32::from_bits(words[4])),
                f64::from(f32::from_bits(words[5])),
                f64::from(f32::from_bits(words[6])),
            )
            .map_err(|_| WgpuRenderRuntimeError::VolumePickFailed)?;
            let distance = f64::from(f32::from_bits(words[7]));
            let result = if words[0] == 1 {
                VolumePickResult::voxel(query, world, value, distance, completeness)
            } else {
                VolumePickResult::interpolated_sample(query, world, value, distance, completeness)
            };
            result.map_err(|_| WgpuRenderRuntimeError::VolumePickFailed)
        }
        _ => Err(WgpuRenderRuntimeError::VolumePickFailed),
    }
}

pub(super) fn validate_adapter(adapter: &wgpu::Adapter) -> Result<(), WgpuRenderRuntimeError> {
    let info = adapter.get_info();
    validate_adapter_facts(info.device_type, info.backend, &adapter.limits())
}

pub(super) fn renderer_device_descriptor(
    adapter: &wgpu::Adapter,
    label: &'static str,
) -> Result<wgpu::DeviceDescriptor<'static>, WgpuRenderRuntimeError> {
    validate_adapter(adapter)?;
    let available = adapter.features();
    let mut required_features = wgpu::Features::empty();
    if available.contains(wgpu::Features::TIMESTAMP_QUERY) {
        required_features |= wgpu::Features::TIMESTAMP_QUERY;
    }
    if available.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS) {
        required_features |= wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    }
    Ok(wgpu::DeviceDescriptor {
        label: Some(label),
        required_features,
        required_limits: renderer_required_limits(adapter),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
    })
}

fn validate_adapter_facts(
    device_type: wgpu::DeviceType,
    backend: wgpu::Backend,
    limits: &wgpu::Limits,
) -> Result<(), WgpuRenderRuntimeError> {
    if matches!(device_type, wgpu::DeviceType::Cpu) {
        return Err(WgpuRenderRuntimeError::SoftwareAdapter);
    }
    if backend != wgpu::Backend::Vulkan {
        return Err(WgpuRenderRuntimeError::UnsupportedBackend);
    }
    if limits.max_buffer_size < MIN_BUFFER_LIMIT_BYTES
        || limits.max_storage_buffer_binding_size < MIN_STORAGE_BINDING_LIMIT_BYTES
        || limits.max_storage_buffers_per_shader_stage < MIN_STORAGE_BUFFERS_PER_STAGE
    {
        return Err(WgpuRenderRuntimeError::AdapterLimitsInsufficient);
    }
    Ok(())
}

fn renderer_required_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    let available = adapter.limits();
    wgpu::Limits {
        // Request the adapter's actual payload limits so a caller-selected GPU
        // budget can translate into payload residency instead of retaining
        // the old 256-MiB ceiling.
        max_buffer_size: available.max_buffer_size,
        max_storage_buffer_binding_size: available.max_storage_buffer_binding_size,
        max_storage_buffers_per_shader_stage: MIN_STORAGE_BUFFERS_PER_STAGE,
        ..wgpu::Limits::default()
    }
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

fn payload_segment_capacities(
    requested_payload_bytes: u64,
    limits: &wgpu::Limits,
) -> Result<[u64; MAX_PAYLOAD_SEGMENTS], WgpuRenderRuntimeError> {
    let requested = requested_payload_bytes / COPY_ALIGNMENT * COPY_ALIGNMENT;
    let segment_limit = limits
        .max_storage_buffer_binding_size
        .min(limits.max_buffer_size)
        .min(MAX_SHADER_SEGMENT_BYTES)
        / COPY_ALIGNMENT
        * COPY_ALIGNMENT;
    let maximum = segment_limit.saturating_mul(MAX_PAYLOAD_SEGMENTS as u64);
    if requested < COPY_ALIGNMENT {
        return Err(WgpuRenderRuntimeError::InvalidConfiguration);
    }
    if segment_limit < COPY_ALIGNMENT || requested > maximum {
        return Err(WgpuRenderRuntimeError::CapacityExceeded {
            category: GpuLedgerCategory::PayloadResidency,
            requested_bytes: requested,
            available_bytes: maximum,
        });
    }
    let mut capacities = [0_u64; MAX_PAYLOAD_SEGMENTS];
    let mut remaining = requested;
    for capacity in &mut capacities {
        *capacity = remaining.min(segment_limit);
        remaining -= *capacity;
    }
    debug_assert_eq!(remaining, 0);
    Ok(capacities)
}

fn validate_single_payload_segment_fit(
    requested_bytes: u64,
    segment_capacities: impl IntoIterator<Item = u64>,
) -> Result<(), WgpuRenderRuntimeError> {
    let maximum = segment_capacities.into_iter().max().unwrap_or(0);
    if requested_bytes > maximum {
        return Err(WgpuRenderRuntimeError::CapacityExceeded {
            category: GpuLedgerCategory::PayloadResidency,
            requested_bytes,
            available_bytes: maximum,
        });
    }
    Ok(())
}

fn validate_extent(extent: RenderExtent) -> Result<(), WgpuRenderRuntimeError> {
    if extent.width_pixels() > MAX_RENDER_WIDTH_PIXELS
        || extent.height_pixels() > MAX_RENDER_HEIGHT_PIXELS
    {
        return Err(WgpuRenderRuntimeError::ExtentExceeded);
    }
    Ok(())
}

fn validate_presentation_capacity(registered: usize) -> Result<(), WgpuRenderRuntimeError> {
    if registered >= MAX_REGISTERED_PRESENTATION_TARGETS {
        return Err(WgpuRenderRuntimeError::PresentationCapacityExceeded {
            maximum: MAX_REGISTERED_PRESENTATION_TARGETS,
        });
    }
    Ok(())
}

fn validate_active_presentation_capacity(
    active: usize,
    target_active: bool,
) -> Result<(), WgpuRenderRuntimeError> {
    if target_active {
        return Ok(());
    }
    if active >= MAX_ACTIVE_PRESENTATION_TARGETS {
        return Err(WgpuRenderRuntimeError::PresentationCapacityExceeded {
            maximum: MAX_ACTIVE_PRESENTATION_TARGETS,
        });
    }
    Ok(())
}

fn storage_layout_entry(binding: u32, bytes: u64) -> wgpu::BindGroupLayoutEntry {
    storage_layout_entry_for_stage(binding, bytes, true, wgpu::ShaderStages::FRAGMENT)
}

fn storage_layout_entry_for_stage(
    binding: u32,
    bytes: u64,
    read_only: bool,
    visibility: wgpu::ShaderStages,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: NonZeroU64::new(bytes),
        },
        count: None,
    }
}

const fn align_copy(bytes: u64) -> u64 {
    let bytes = if bytes < 1 { 1 } else { bytes };
    bytes.div_ceil(COPY_ALIGNMENT) * COPY_ALIGNMENT
}

fn control_buffer_capacity(required_bytes: u64) -> Result<u64, WgpuRenderRuntimeError> {
    let required = required_bytes.max(INITIAL_CONTROL_BYTES);
    let capacity = required
        .checked_next_power_of_two()
        .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
    if capacity > CONTROL_BUFFER_BYTES {
        return Err(WgpuRenderRuntimeError::ControlCapacityExceeded);
    }
    Ok(capacity)
}

#[cfg(test)]
mod tests {
    use mirante4d_dataset::{
        DatasetCatalog, DatasetLayer, DatasetResourceIdentity, DatasetSourceId,
        ResourcePayloadDescriptor, ResourcePayloadFacts, ResourceRegion, ResourceValidity,
        ScientificIdentityStatus,
    };
    use mirante4d_domain::{
        GridToWorld, IntensityDType, LogicalLayerKey, ScaleLevel, Shape3D, Shape4D, TimeIndex,
    };

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
        RenderRequirement::new(key, mirante4d_render_api::RenderRequirementRole::Refinement)
    }

    #[test]
    fn timing_query_layout_distinguishes_copy_render_and_absent_work() {
        let complete_envelope = TimingQueryLayout::new(8, true, true, true);
        assert_eq!(complete_envelope.batch_envelope, Some((8, 9)));
        assert_eq!(complete_envelope.payload_copy, Some((10, 11)));
        assert_eq!(complete_envelope.render_pass, Some((12, 13)));
        assert_eq!(complete_envelope.query_count, 6);

        let known_copy_and_pass = TimingQueryLayout::new(8, false, true, true);
        assert_eq!(known_copy_and_pass.batch_envelope, None);
        assert_eq!(known_copy_and_pass.payload_copy, Some((8, 9)));
        assert_eq!(known_copy_and_pass.render_pass, Some((10, 11)));
        assert_eq!(known_copy_and_pass.query_count, 4);

        let empty_copy_control = TimingQueryLayout::new(8, false, false, true);
        assert_eq!(empty_copy_control.payload_copy, None);
        assert_eq!(empty_copy_control.render_pass, Some((8, 9)));
        assert_eq!(empty_copy_control.query_count, 2);

        let empty_pass_control = TimingQueryLayout::new(8, false, true, false);
        assert_eq!(empty_pass_control.payload_copy, Some((8, 9)));
        assert_eq!(empty_pass_control.render_pass, None);
        assert_eq!(empty_pass_control.query_count, 2);

        let absent = TimingQueryLayout::new(8, false, false, false);
        assert_eq!(absent.batch_envelope, None);
        assert_eq!(absent.payload_copy, None);
        assert_eq!(absent.render_pass, None);
        assert_eq!(absent.query_count, 0);
    }

    #[test]
    fn exact_only_retained_frames_wait_for_exact_coverage() {
        assert!(retained_frame_policy_allows_render(
            RetainedFrameRenderPolicy::EveryUsefulFrame,
            Some(FrameCompleteness::Progressive),
        ));
        assert!(!retained_frame_policy_allows_render(
            RetainedFrameRenderPolicy::ExactFrameOnly,
            Some(FrameCompleteness::Progressive),
        ));
        assert!(retained_frame_policy_allows_render(
            RetainedFrameRenderPolicy::ExactFrameOnly,
            Some(FrameCompleteness::Exact),
        ));
        assert!(!retained_frame_policy_allows_render(
            RetainedFrameRenderPolicy::ExactFrameOnly,
            None,
        ));
    }

    #[test]
    fn product_creation_counter_is_monotonic_and_saturating() {
        let mut creations = 0;
        record_product_creation(&mut creations);
        record_product_creation(&mut creations);
        assert_eq!(creations, 2);

        creations = u64::MAX;
        record_product_creation(&mut creations);
        assert_eq!(creations, u64::MAX);
    }

    #[test]
    fn product_bind_group_and_pipeline_calls_use_counted_helpers() {
        let source = include_str!("runtime.rs");
        for api in ["render_pipeline", "compute_pipeline", "bind_group"] {
            let direct_call = [".create_", api, "("].concat();
            assert_eq!(
                source.matches(&direct_call).count(),
                1,
                "the sole `{direct_call}` must remain inside its counted helper"
            );
        }

        let initialization = source
            .split_once("fn from_device_parts(")
            .and_then(|(_, tail)| tail.split_once("pub(super) const fn diagnostics"))
            .map(|(body, _)| body)
            .expect("runtime initialization remains inspectable");
        assert_eq!(
            initialization
                .matches("create_counted_render_pipeline(")
                .count(),
            2,
            "runtime creation has exactly the general and dedicated MIP render pipelines"
        );
        assert_eq!(
            initialization.matches("create_pick_resources(").count(),
            1,
            "runtime creation has exactly one asynchronous pick pipeline"
        );
    }

    fn test_static_layout(keys: &[DatasetResourceKey]) -> PreparedStaticPresentationLayout {
        let body = PreparedResourceBody::new(keys.to_vec().into(), keys.to_vec().into(), None)
            .expect("test keys are canonical");
        let record_capacity = record_slot_capacity(keys.len()).unwrap();
        let page_table_offset_words = HEADER_WORDS + LAYER_WORDS + record_capacity * RESOURCE_WORDS;
        let page = build_page_layout(keys, 0).unwrap();
        let fields = [[
            u32::try_from(page_table_offset_words).unwrap(),
            u64_to_u32(page.origin_zyx[2]).unwrap(),
            u64_to_u32(page.origin_zyx[1]).unwrap(),
            u64_to_u32(page.origin_zyx[0]).unwrap(),
            u64_to_u32(page.cell_zyx[2]).unwrap(),
            u64_to_u32(page.cell_zyx[1]).unwrap(),
            u64_to_u32(page.cell_zyx[0]).unwrap(),
        ]];
        let occupied_cells = page.occupied_cells;
        let mut table = vec![page.capacity, page.seed];
        table.extend(page.slots.into_iter().flatten());
        let logical_control_bytes = ((page_table_offset_words + table.len()) * 4) as u64;
        PreparedStaticPresentationLayout {
            requirements: body.clone(),
            base_accounting_anchor: body,
            layers: Arc::from([LogicalLayerKey::new(0)]),
            scale_keys: Arc::from([keys[0]]),
            layer_page_fields: Arc::from(fields),
            base_page_table: table.into(),
            page_table_patches: Arc::from([]),
            occupied_cells: Arc::from([occupied_cells]),
            free_record_slots: Arc::from([]),
            next_record_slot: keys.len() as u32,
            record_capacity: record_capacity as u32,
            page_table_offset: (page_table_offset_words * 4) as u64,
            logical_control_bytes,
            renderer_host_allocation_bytes: 0,
            preparation_peak_host_allocation_bytes: 0,
            version: Arc::new(()),
            delta: None,
        }
    }

    fn test_catalog(width: u64) -> DatasetCatalog {
        DatasetCatalog::new(
            "incremental control fixture",
            ScientificIdentityStatus::Unverified(DatasetSourceId::new(1)),
            vec![
                DatasetLayer::new(
                    LogicalLayerKey::new(0),
                    "layer",
                    Shape4D::new(1, 1, 1, width).unwrap(),
                    IntensityDType::Uint8,
                    GridToWorld::identity(),
                    ResourceValidity::AllValid,
                )
                .unwrap(),
            ],
        )
        .unwrap()
    }

    struct QueueFixtureLease {
        key: DatasetResourceKey,
        descriptor: ResourcePayloadDescriptor,
        values: Box<[u8]>,
        validity: Option<Box<[u8]>>,
        facts: ResourcePayloadFacts,
    }

    impl QueueFixtureLease {
        fn arc(key: DatasetResourceKey) -> Arc<dyn ResourceLease> {
            let descriptor = ResourcePayloadDescriptor::new(
                IntensityDType::Uint8,
                Shape3D::new(1, 1, 1).unwrap(),
                ResourceValidity::AllValid,
            )
            .unwrap();
            let values: Box<[u8]> = vec![1].into_boxed_slice();
            let facts = ResourcePayloadFacts::from_payload(descriptor.view(&values, None).unwrap())
                .unwrap();
            Arc::new(Self {
                key,
                descriptor,
                values,
                validity: None,
                facts,
            })
        }

        fn all_invalid_arc(key: DatasetResourceKey) -> Arc<dyn ResourceLease> {
            let shape = Shape3D::new(64, 64, 64).unwrap();
            let descriptor = ResourcePayloadDescriptor::new(
                IntensityDType::Float32,
                shape,
                ResourceValidity::BitMask,
            )
            .unwrap();
            let values = vec![0; shape.element_count().unwrap() as usize * 4].into_boxed_slice();
            let validity = vec![0; shape.element_count().unwrap() as usize / 8].into_boxed_slice();
            let facts = ResourcePayloadFacts::from_validated_range(0.0, 0.0, false, false).unwrap();
            Arc::new(Self {
                key,
                descriptor,
                values,
                validity: Some(validity),
                facts,
            })
        }

        fn large_valid_arc(key: DatasetResourceKey) -> Arc<dyn ResourceLease> {
            let shape = Shape3D::new(64, 64, 64).unwrap();
            let descriptor = ResourcePayloadDescriptor::new(
                IntensityDType::Float32,
                shape,
                ResourceValidity::AllValid,
            )
            .unwrap();
            let values = vec![0; shape.element_count().unwrap() as usize * 4].into_boxed_slice();
            let facts = ResourcePayloadFacts::from_validated_range(0.0, 0.0, true, true).unwrap();
            Arc::new(Self {
                key,
                descriptor,
                values,
                validity: None,
                facts,
            })
        }
    }

    impl ResourceLease for QueueFixtureLease {
        fn key(&self) -> DatasetResourceKey {
            self.key
        }

        fn payload(&self) -> ResourcePayloadView<'_> {
            self.descriptor
                .view(&self.values, self.validity.as_deref())
                .unwrap()
        }

        fn payload_facts(&self) -> ResourcePayloadFacts {
            self.facts
        }
    }

    #[test]
    fn retained_queue_preserves_rank_order_across_two_atomic_deferrals() {
        let cohort = |base: u64, descending: bool| {
            (0..MAX_UPLOADS)
                .map(|index| {
                    let rank = if descending {
                        MAX_UPLOADS - 1 - index
                    } else {
                        index
                    };
                    QueueFixtureLease::arc(key(0, 0, base + rank as u64, 1))
                })
                .collect::<Vec<_>>()
        };
        let cohort_a = cohort(10_000, true);
        let cohort_b = cohort(1_000, false);
        let cohort_c = vec![QueueFixtureLease::arc(key(0, 0, 1, 1))];
        let keys = |leases: &[Arc<dyn ResourceLease>]| {
            leases.iter().map(|lease| lease.key()).collect::<Vec<_>>()
        };

        let mut queue = PendingLeaseQueue::new();
        assert_eq!(queue.try_extend(&cohort_a), Ok(true));
        assert_eq!(keys(&queue.ordered_leases()), keys(&cohort_a));
        let a_snapshot = keys(&queue.ordered_leases());
        assert_eq!(queue.try_extend(&cohort_b), Ok(false));
        assert_eq!(keys(&queue.ordered_leases()), a_snapshot);
        assert!(queue.transfer_bytes <= MAX_PAYLOAD_UPLOAD_BYTES);

        for key in a_snapshot {
            assert!(queue.remove(key));
        }
        assert_eq!(queue.try_extend(&cohort_b), Ok(true));
        assert_eq!(keys(&queue.ordered_leases()), keys(&cohort_b));
        let b_snapshot = keys(&queue.ordered_leases());
        assert_eq!(queue.try_extend(&cohort_c), Ok(false));
        assert_eq!(keys(&queue.ordered_leases()), b_snapshot);

        for key in b_snapshot {
            assert!(queue.remove(key));
        }
        assert_eq!(queue.try_extend(&cohort_c), Ok(true));
        assert_eq!(keys(&queue.ordered_leases()), keys(&cohort_c));
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.transfer_bytes, 1);
    }

    #[test]
    fn retained_queue_charges_all_invalid_pages_as_zero_transfer_bytes() {
        let cohort = (0..9)
            .map(|index| QueueFixtureLease::all_invalid_arc(key(0, 0, index, 1)))
            .collect::<Vec<_>>();
        assert!(
            cohort
                .iter()
                .map(|lease| lease.payload().byte_len())
                .sum::<u64>()
                > MAX_PAYLOAD_UPLOAD_BYTES
        );

        let mut queue = PendingLeaseQueue::new();
        assert_eq!(queue.try_extend(&cohort), Ok(true));
        assert_eq!(queue.len(), cohort.len());
        assert_eq!(queue.transfer_bytes, 0);

        queue.clear();
        let mut mixed = (0..8)
            .map(|index| QueueFixtureLease::large_valid_arc(key(0, 0, 100 + index, 1)))
            .collect::<Vec<_>>();
        mixed.push(QueueFixtureLease::all_invalid_arc(key(0, 0, 200, 1)));
        assert_eq!(
            mixed[..8]
                .iter()
                .map(|lease| lease.payload().byte_len())
                .sum::<u64>(),
            MAX_PAYLOAD_UPLOAD_BYTES
        );
        assert_eq!(queue.try_extend(&mixed), Ok(true));
        assert_eq!(queue.len(), mixed.len());
        assert_eq!(queue.transfer_bytes, MAX_PAYLOAD_UPLOAD_BYTES);
    }

    #[test]
    fn requirement_preflight_preserves_the_129_resource_cursor_fixture() {
        let resources = (0..129)
            .map(|x| requirement(key(0, 0, x, 1)))
            .collect::<Vec<_>>();
        assert_eq!(validate_requirement_slice(&resources), Ok(()));
    }

    #[test]
    fn requirement_preflight_admits_a_scale_s0_working_set() {
        let resources = (0..32_768)
            .map(|x| requirement(key(0, 0, x, 1)))
            .collect::<Vec<_>>();
        assert_eq!(validate_requirement_slice(&resources), Ok(()));
    }

    #[test]
    fn static_control_merge_applies_overlays_and_evictions_in_canonical_order() {
        let resident = |offset: u64, any_valid: bool| ResidentResource {
            segment: 0,
            offset,
            allocated_bytes: u64::from(any_valid) * 256,
            validity_offset: None,
            dtype_bytes: 1,
            minimum: 0.0,
            maximum: 1.0,
            any_valid,
            all_valid: any_valid,
            last_used_frame: 1,
        };
        let requirement_keys = (0..7).map(|x| key(0, 0, x, 1)).collect::<Vec<_>>();
        let base = BTreeMap::from([
            (requirement_keys[1], resident(101, true)),
            (requirement_keys[2], resident(102, true)),
            (requirement_keys[3], resident(103, true)),
            (requirement_keys[5], resident(105, true)),
            (key(0, 0, 100, 1), resident(200, true)),
        ]);
        let evicted = BTreeSet::from([requirement_keys[2], requirement_keys[3]]);
        let uploaded = BTreeMap::from([(requirement_keys[2], resident(202, true))]);
        let empty = BTreeMap::from([(requirement_keys[4], resident(0, false))]);
        let prefix = vec![7_u8; resource_record_offset(1, 0) as usize];
        let layout = test_static_layout(&requirement_keys);

        let (writes, empty_keys) = build_static_control_writes(
            prefix.clone(),
            1,
            &layout,
            &base,
            &evicted,
            &uploaded,
            &empty,
        )
        .unwrap();

        assert_eq!(empty_keys, vec![requirement_keys[4]]);
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].offset, 0);
        assert_eq!(writes[0].bytes, prefix);
        assert_eq!(writes[1].offset, resource_record_offset(1, 0));
        assert_eq!(
            writes[1].bytes.len(),
            layout.record_capacity as usize * RESOURCE_WORDS * size_of::<u32>()
        );

        let mut expected = [[0_u32; RESOURCE_WORDS]; 8];
        expected[1] = resource_record(requirement_keys[1], base.get(&requirement_keys[1])).unwrap();
        expected[2] =
            resource_record(requirement_keys[2], uploaded.get(&requirement_keys[2])).unwrap();
        expected[4] =
            resource_record(requirement_keys[4], empty.get(&requirement_keys[4])).unwrap();
        expected[5] = resource_record(requirement_keys[5], base.get(&requirement_keys[5])).unwrap();
        assert_eq!(
            writes[1].bytes,
            bytemuck::cast_slice::<[u32; RESOURCE_WORDS], u8>(&expected)
        );
    }

    #[test]
    fn static_control_publication_has_constant_writes_for_alternating_65k_availability() {
        let resident = |offset: u64| ResidentResource {
            segment: 0,
            offset,
            allocated_bytes: 256,
            validity_offset: None,
            dtype_bytes: 1,
            minimum: 0.0,
            maximum: 1.0,
            any_valid: true,
            all_valid: true,
            last_used_frame: 1,
        };
        let requirement_keys = (0..MAX_RENDER_REQUIREMENTS as u64)
            .map(|x| key(0, 0, x, 1))
            .collect::<Vec<_>>();
        let base = requirement_keys
            .iter()
            .copied()
            .enumerate()
            .filter(|(index, _)| index % 2 == 0)
            .map(|(index, key)| (key, resident(index as u64 * 256)))
            .collect::<BTreeMap<_, _>>();
        let prefix = vec![0_u8; resource_record_offset(1, 0) as usize];
        let layout = test_static_layout(&requirement_keys);

        let (writes, empty_keys) = build_static_control_writes(
            prefix,
            1,
            &layout,
            &base,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();

        assert!(empty_keys.is_empty());
        assert_eq!(writes.len(), 2);
        assert_eq!(
            writes[1].bytes.len(),
            MAX_RENDER_REQUIREMENTS * RESOURCE_WORDS * size_of::<u32>()
        );
        // A static publication adds exactly one retained page-table write.
        assert_eq!(writes.len() + 1, 3);
        assert!(writes.len() < MAX_CONTROL_PUBLICATION_WRITES_PER_FRAME);

        for index in [
            0,
            1,
            MAX_RENDER_REQUIREMENTS - 2,
            MAX_RENDER_REQUIREMENTS - 1,
        ] {
            let start = index * RESOURCE_WORDS * size_of::<u32>();
            let end = start + RESOURCE_WORDS * size_of::<u32>();
            if index % 2 == 0 {
                let expected =
                    resource_record(requirement_keys[index], base.get(&requirement_keys[index]))
                        .unwrap();
                assert_eq!(&writes[1].bytes[start..end], bytemuck::bytes_of(&expected));
            } else {
                assert!(writes[1].bytes[start..end].iter().all(|byte| *byte == 0));
            }
        }
    }

    #[test]
    fn one_key_65k_body_replacement_reuses_base_slots_and_accounts_only_delta() {
        let previous_keys = (0..MAX_RENDER_REQUIREMENTS as u64)
            .map(|x| key(0, 0, x, 1))
            .collect::<Vec<_>>();
        let previous = test_static_layout(&previous_keys);
        let added = key(0, 0, MAX_RENDER_REQUIREMENTS as u64, 1);
        let removed = previous_keys[0];
        let mut next_keys = previous_keys[1..].to_vec();
        next_keys.push(added);
        let next_body =
            PreparedResourceBody::new(next_keys.clone().into(), next_keys.clone().into(), None)
                .unwrap();
        let next_requirements = PreparedRenderRequirements::new(
            DatasetResourceIdentity::Unverified(DatasetSourceId::new(1)),
            TimeIndex::new(0),
            vec![LogicalLayerKey::new(0)],
            next_body,
            1,
        )
        .unwrap();
        let catalog = test_catalog(MAX_RENDER_REQUIREMENTS as u64 + 1);

        let preflight = preflight_static_presentation_layout_update(
            &catalog,
            &next_requirements,
            Some(&previous),
            &[added],
            &[removed],
        )
        .unwrap();
        let next = prepare_static_presentation_layout_update(
            &catalog,
            &next_requirements,
            Some(&previous),
            &[added],
            &[removed],
        )
        .unwrap();

        assert!(next.is_incremental_body_update());
        assert!(next.shares_base_page_storage_with(&previous));
        assert_eq!(next.incremental_preparation_key_visits(), 2);
        assert_eq!(next.incremental_resource_slot_changes(), 1);
        assert!(next.incremental_page_entry_changes() <= 2);
        assert_eq!(
            next.logical_control_bytes(),
            previous.logical_control_bytes()
        );
        assert_eq!(
            preflight.renderer_host_allocation_bytes(),
            next.renderer_host_allocation_bytes()
        );
        assert!(
            next.renderer_host_allocation_bytes() * 100 < previous.base_page_table.len() as u64 * 4,
            "one-key preparation must retain the base allocation and charge only proportional delta state"
        );
        assert!(next.resource_slot(removed).is_none());
        assert!(next.resource_slot(added).is_some());
        let delta = next.delta.as_ref().unwrap();
        assert!(
            incremental_control_write_count(
                next.layers.len(),
                next.page_table_offset,
                &delta.resource_slots,
                &delta.page_patches,
            ) <= 4
        );

        let mut saturated = previous.clone();
        saturated.page_table_patches = vec![
            PageTablePatch {
                word_offset: 2,
                words: [0, 0, 0, PAGE_TOMBSTONE],
            };
            MAX_STATIC_PAGE_PATCHES
        ]
        .into();
        let compacted = prepare_static_presentation_layout_update(
            &catalog,
            &next_requirements,
            Some(&saturated),
            &[added],
            &[removed],
        )
        .unwrap();
        assert!(!compacted.is_incremental_body_update());
        assert!(!compacted.shares_base_page_storage_with(&saturated));
    }

    #[test]
    fn alternating_max_delta_falls_back_before_the_control_write_cap() {
        let resident = |offset: u64| ResidentResource {
            segment: 0,
            offset,
            allocated_bytes: 256,
            validity_offset: None,
            dtype_bytes: 1,
            minimum: 0.0,
            maximum: 1.0,
            any_valid: true,
            all_valid: true,
            last_used_frame: 1,
        };
        let requirement_keys = (0..(MAX_UPLOADS * 2) as u64)
            .map(|x| key(0, 0, x, 1))
            .collect::<Vec<_>>();
        let changed_keys = requirement_keys
            .iter()
            .step_by(2)
            .copied()
            .collect::<BTreeSet<_>>();
        let base = changed_keys
            .iter()
            .copied()
            .enumerate()
            .map(|(index, key)| (key, resident(index as u64 * 256)))
            .collect::<BTreeMap<_, _>>();
        let prefix = vec![0_u8; resource_record_offset(1, 0) as usize];
        let layout = test_static_layout(&requirement_keys);

        assert_eq!(changed_keys.len(), MAX_UPLOADS);
        assert!(
            build_incremental_control_writes(Some(&prefix), 1, &layout, &changed_keys, &base,)
                .unwrap()
                .is_none(),
            "fragmented sparse writes must request the dense fallback"
        );
        let (dense_writes, _) = build_static_control_writes(
            prefix,
            1,
            &layout,
            &base,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(dense_writes.len(), 2);
        assert!(dense_writes.len() <= MAX_CONTROL_PUBLICATION_WRITES_PER_FRAME);
    }

    #[test]
    fn exact_byte_arena_preserves_dtype_specific_one_gib_capacity() {
        let arena_bytes = 1024_u64 * 1024 * 1024 * 75 / 100;
        let allocation_count = |bytes| {
            let mut allocator = ArenaAllocator::new(arena_bytes);
            let mut count = 0;
            while allocator.allocate(bytes).is_some() {
                count += 1;
            }
            count
        };

        assert_eq!(allocation_count(64 * 64 * 64), 3_072);
        assert_eq!(allocation_count(64 * 64 * 64 * 2), 1_536);
        assert_eq!(allocation_count(64 * 64 * 64 * 4), 768);
    }

    #[test]
    fn payload_category_is_partitioned_into_maximal_fixed_segments() {
        let two_gib = 2_u64 * 1024 * 1024 * 1024;
        let limits = wgpu::Limits {
            max_buffer_size: two_gib,
            max_storage_buffer_binding_size: two_gib,
            ..wgpu::Limits::default()
        };
        let capacities = payload_segment_capacities(3 * 1024 * 1024 * 1024, &limits).unwrap();
        assert_eq!(capacities, [two_gib, 1024 * 1024 * 1024, 0, 0]);
        assert_eq!(capacities.iter().sum::<u64>(), 3 * 1024 * 1024 * 1024);
        assert_eq!(
            validate_single_payload_segment_fit(two_gib, capacities),
            Ok(())
        );
        assert_eq!(
            validate_single_payload_segment_fit(two_gib + COPY_ALIGNMENT, capacities),
            Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::PayloadResidency,
                requested_bytes: two_gib + COPY_ALIGNMENT,
                available_bytes: two_gib,
            })
        );
    }

    #[test]
    fn payload_segment_count_has_a_typed_total_capacity_ceiling() {
        let one_gib = 1024_u64 * 1024 * 1024;
        let limits = wgpu::Limits {
            max_buffer_size: one_gib,
            max_storage_buffer_binding_size: one_gib,
            ..wgpu::Limits::default()
        };
        assert_eq!(
            payload_segment_capacities(4 * one_gib + COPY_ALIGNMENT, &limits),
            Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::PayloadResidency,
                requested_bytes: 4 * one_gib + COPY_ALIGNMENT,
                available_bytes: 4 * one_gib,
            })
        );
    }

    #[test]
    fn persistent_upload_staging_zeros_padding_without_rewriting_payload_spans() {
        let exact = PayloadLayout {
            allocation_bytes: 8,
            validity_offset: None,
        };
        let mut exact_bytes = [0xa5_u8; 8];
        let exact_padding = upload_padding_ranges(&exact, 8, 0);
        assert_eq!(exact_padding, [8..8, 8..8]);
        for range in exact_padding {
            exact_bytes[range].fill(0);
        }
        assert_eq!(exact_bytes, [0xa5; 8]);

        let split = PayloadLayout {
            allocation_bytes: 12,
            validity_offset: Some(8),
        };
        let mut split_bytes = [0xa5_u8; 12];
        let split_padding = upload_padding_ranges(&split, 5, 2);
        assert_eq!(split_padding, [5..8, 10..12]);
        for range in split_padding {
            split_bytes[range].fill(0);
        }
        assert_eq!(&split_bytes[..5], &[0xa5; 5]);
        assert_eq!(&split_bytes[5..8], &[0; 3]);
        assert_eq!(&split_bytes[8..10], &[0xa5; 2]);
        assert_eq!(&split_bytes[10..], &[0; 2]);

        let trailing = PayloadLayout {
            allocation_bytes: 4,
            validity_offset: None,
        };
        let mut trailing_bytes = [0xa5_u8; 4];
        let trailing_padding = upload_padding_ranges(&trailing, 1, 0);
        assert_eq!(trailing_padding, [1..1, 1..4]);
        for range in trailing_padding {
            trailing_bytes[range].fill(0);
        }
        assert_eq!(trailing_bytes, [0xa5, 0, 0, 0]);
    }

    #[test]
    fn fragmented_arena_planning_uses_exact_undo_without_snapshot_clone() {
        let mut allocator = ArenaAllocator::new(4096);
        let first = allocator.allocate(512).unwrap();
        let second = allocator.allocate(512).unwrap();
        let third = allocator.allocate(512).unwrap();
        allocator.release(second, 512);
        let original = allocator.ranges();

        let planned = allocator.allocate(256).unwrap();
        allocator.release(planned, 256);
        assert_eq!(allocator.ranges(), original);

        assert!(allocator.reserve_exact(planned, 256));
        assert!(
            !allocator
                .ranges()
                .iter()
                .any(|(start, end)| *start <= planned && planned < *end)
        );
        assert_eq!(first, 0);
        assert_eq!(third, 1024);
    }

    #[test]
    fn allocator_undo_is_bounded_at_65536_range_fragmentation() {
        const BLOCKS: usize = 65_536;
        let mut allocator = ArenaAllocator::new(BLOCKS as u64 * 256);
        let offsets = (0..BLOCKS)
            .map(|_| allocator.allocate(256).unwrap())
            .collect::<Vec<_>>();
        for offset in offsets.iter().step_by(2) {
            allocator.release(*offset, 256);
        }
        assert_eq!(allocator.ranges().len(), BLOCKS / 2);
        let original = allocator.ranges();
        let planned = (0..MAX_UPLOADS)
            .map(|_| allocator.allocate(256).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(planned.len(), MAX_UPLOADS);
        for offset in planned.into_iter().rev() {
            allocator.release(offset, 256);
        }
        assert_eq!(allocator.ranges(), original);
    }

    #[test]
    fn all_invalid_payload_is_a_zero_byte_resident_page() {
        let descriptor = ResourcePayloadDescriptor::new(
            IntensityDType::Float32,
            Shape3D::new(64, 64, 64).unwrap(),
            ResourceValidity::BitMask,
        )
        .unwrap();
        let values = vec![17_u8; usize::try_from(descriptor.value_byte_len()).unwrap()];
        let validity = vec![0_u8; usize::try_from(descriptor.validity_byte_len()).unwrap()];
        let payload = descriptor.view(&values, Some(&validity)).unwrap();
        let facts = ResourcePayloadFacts::from_payload(payload).unwrap();
        let resident = empty_resident_resource(payload, facts, 7).unwrap();

        assert_eq!(resident.offset, 0);
        assert_eq!(resident.allocated_bytes, 0);
        assert_eq!(resident.validity_offset, None);
        assert!(!resident.any_valid);
        assert!(!resident.all_valid);
    }

    #[test]
    fn empty_metadata_ledger_has_an_explicit_host_byte_formula() {
        let entry_body_bytes = size_of::<DatasetResourceKey>()
            + size_of::<ResidentResource>()
            + EMPTY_RESIDENT_INDEX_ALLOWANCE_BYTES;
        assert_eq!(EMPTY_RESIDENT_METADATA_BYTES_PER_RECORD % 64, 0);
        assert!(EMPTY_RESIDENT_METADATA_BYTES_PER_RECORD >= entry_body_bytes as u64);
        assert_eq!(MAX_EMPTY_RESIDENTS, 327_680);
        assert_eq!(
            EMPTY_RESIDENT_METADATA_CAPACITY_BYTES,
            empty_resident_metadata_bytes(MAX_EMPTY_RESIDENTS)
        );
    }

    #[test]
    fn four_panel_empty_union_plus_one_frame_reuse_is_bounded() {
        let active_union = MAX_ACTIVE_PRESENTATION_TARGETS * MAX_RENDER_REQUIREMENTS;
        let inactive_reuse = MAX_RENDER_REQUIREMENTS;
        assert_eq!(active_union + inactive_reuse, MAX_EMPTY_RESIDENTS);

        // Model one page arriving beyond the admitted four-panel active union
        // and one full inactive frame. The maintained age order includes all
        // records; the active-union predicate must bypass 262,144 older pinned
        // keys and select only the first inactive record.
        let predicted = active_union + inactive_reuse + 1;
        let excess = predicted - MAX_EMPTY_RESIDENTS;
        let age_order = (0_u32..predicted as u32).map(|key| (u64::from(key), key));
        let mut protection_checks = 0_usize;
        let evictions = oldest_unprotected_keys(age_order, excess, |key| {
            protection_checks += 1;
            key < active_union as u32
        });
        assert_eq!(evictions, vec![active_union as u32]);
        assert_eq!(protection_checks, active_union + 1);
        assert_eq!(predicted - evictions.len(), MAX_EMPTY_RESIDENTS);
        assert!(validate_empty_resident_metadata_capacity(predicted - evictions.len()).is_ok());
    }

    #[test]
    fn protected_empty_metadata_overflow_is_a_typed_capacity_failure() {
        let requested = MAX_EMPTY_RESIDENTS + 1;
        assert_eq!(
            validate_empty_resident_metadata_capacity(requested),
            Err(WgpuRenderRuntimeError::ResidentMetadataCapacityExceeded {
                requested_records: requested,
                maximum_records: MAX_EMPTY_RESIDENTS,
                requested_bytes: empty_resident_metadata_bytes(requested),
                available_bytes: EMPTY_RESIDENT_METADATA_CAPACITY_BYTES,
            })
        );
    }

    #[test]
    fn requirement_preflight_rejects_multiscale_and_static_layout_rejects_overlap() {
        assert_eq!(
            validate_requirement_slice(&[
                requirement(key(0, 0, 0, 1)),
                requirement(key(0, 1, 0, 1)),
            ]),
            Err(WgpuRenderRuntimeError::MixedScaleRequirements)
        );
        let overlapping = vec![key(0, 0, 0, 2), key(0, 0, 1, 2)];
        assert_eq!(
            validate_requirement_slice(
                &overlapping
                    .iter()
                    .copied()
                    .map(requirement)
                    .collect::<Vec<_>>()
            ),
            Ok(())
        );
        assert!(matches!(
            build_page_layout(&overlapping, 0),
            Err(WgpuRenderRuntimeError::OverlappingResources)
        ));
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
        let resources = (0..=MAX_RENDER_REQUIREMENTS)
            .map(|x| requirement(key(0, 0, x as u64, 1)))
            .collect::<Vec<_>>();
        assert_eq!(
            validate_requirement_slice(&resources),
            Err(WgpuRenderRuntimeError::RequirementCapacityExceeded {
                actual: MAX_RENDER_REQUIREMENTS + 1,
                maximum: MAX_RENDER_REQUIREMENTS,
            })
        );
    }

    #[test]
    fn sparse_page_layout_preplaces_missing_resources_for_incremental_patching() {
        let keys = vec![key(0, 0, 0, 64), key(0, 0, 64, 64), key(0, 0, 128, 64)];
        let layout = build_page_layout(&keys, 0).unwrap();

        assert_eq!(layout.origin_zyx, [0, 0, 0]);
        assert_eq!(layout.cell_zyx, [1, 1, 64]);
        assert_eq!(layout.capacity, 8);
        assert!(layout.slots.contains(&[0, 0, 0, 1]));
        assert!(layout.slots.contains(&[1, 0, 0, 2]));
        assert!(layout.slots.contains(&[2, 0, 0, 3]));
    }

    #[test]
    fn sparse_page_layout_is_bounded_by_demand_not_global_grid_volume() {
        let keys = vec![
            key(0, 0, 0, 1),
            key(0, 0, 1, 1),
            key(0, 0, u32::MAX as u64 - 1, 1),
        ];
        let layout = build_page_layout(&keys, 0).unwrap();

        assert_eq!(layout.capacity, 8);
        assert_eq!(layout.slots.len(), 8);
    }

    #[test]
    fn shader_contract_uses_direct_pages_and_bounded_brick_traversal() {
        let shader = include_str!("shader.wgsl");
        let pick_shader = include_str!("pick_shader.wgsl");

        assert!(shader.contains("fn lookup_page"));
        assert!(shader.contains("fn page_exit_distance"));
        assert!(shader.contains("resource_f32(page.resource_index, 13u) <= maximum"));
        assert!(shader.contains("const ALPHA_TERMINATION: f32 = 0.999"));
        assert!(shader.contains("alpha >= ALPHA_TERMINATION && control[28u] != 0u"));
        assert!(!shader.contains("for (var resource_index"));
        assert!(!shader.contains("resource_index += 1u"));
        assert!(!shader.contains("MAX_RAY_SAMPLES"));
        assert!(!pick_shader.contains("MAX_RAY_SAMPLES"));
        assert!(pick_shader.contains("transmittance <= best_score"));
        assert!(pick_shader.contains("control[28u] != 0u"));
        assert!(shader.contains("footprint_in_page"));
        assert!(shader.contains("sample_linear_tap"));
        let general_dvr = shader
            .split_once("fn render_general_dvr")
            .and_then(|(_, tail)| tail.split_once("fn render_cross_section_layer"))
            .map(|(body, _)| body)
            .expect("general DVR function remains in the shader contract");
        assert!(general_dvr.contains("page_exit_distance"));
        assert!(general_dvr.contains("segment_end_index"));
        assert!(general_dvr.contains("sample_in_page"));
        assert!(
            !general_dvr.contains("sample_grid("),
            "mixed-grid DVR must not redo sparse lookup at every world sample"
        );
        assert!(
            shader.contains("resources[layer_index] = 0xffffffffu;"),
            "fused DVR must sentinel-disable extrema-skipped pages"
        );
    }

    #[test]
    fn fused_dvr_requires_the_complete_affine_not_only_diagonal_terms() {
        let mut words = vec![0_u32; HEADER_WORDS + 2 * LAYER_WORDS];
        for layer in 0..2 {
            let start = HEADER_WORDS + layer * LAYER_WORDS;
            words[start + 18] = 1;
            words[start + 1..start + 4].copy_from_slice(&[64, 64, 64]);
            words[start + 24] = 0;
            for field in 32..56 {
                words[start + field] = (field as f32 * 0.125).to_bits();
            }
        }
        assert!(fused_dvr_compatible(&words, 2));

        // Off-diagonal inverse term: diagonal-only compatibility would have
        // fused these distinct sheared grids and sampled the wrong channel.
        words[HEADER_WORDS + LAYER_WORDS + 33] ^= 1;
        assert!(!fused_dvr_compatible(&words, 2));
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
    fn presentation_capacity_allows_one_inactive_atomic_staging_target() {
        assert_eq!(validate_presentation_capacity(0), Ok(()));
        assert_eq!(validate_presentation_capacity(4), Ok(()));
        assert_eq!(
            validate_presentation_capacity(5),
            Err(WgpuRenderRuntimeError::PresentationCapacityExceeded { maximum: 5 })
        );
        assert_eq!(validate_active_presentation_capacity(3, false), Ok(()));
        assert_eq!(validate_active_presentation_capacity(4, true), Ok(()));
        assert_eq!(
            validate_active_presentation_capacity(4, false),
            Err(WgpuRenderRuntimeError::PresentationCapacityExceeded { maximum: 4 })
        );
    }

    #[test]
    fn existing_device_preflight_requires_the_renderer_limits() {
        let mut limits = wgpu::Limits {
            max_buffer_size: MIN_BUFFER_LIMIT_BYTES,
            max_storage_buffer_binding_size: MIN_STORAGE_BINDING_LIMIT_BYTES,
            max_storage_buffers_per_shader_stage: MIN_STORAGE_BUFFERS_PER_STAGE,
            ..wgpu::Limits::default()
        };
        assert_eq!(validate_device_limits(&limits), Ok(()));

        limits.max_storage_buffer_binding_size = MIN_STORAGE_BINDING_LIMIT_BYTES - 1;
        assert_eq!(
            validate_device_limits(&limits),
            Err(WgpuRenderRuntimeError::DeviceLimitsInsufficient)
        );
    }

    #[test]
    fn adapter_preflight_is_vulkan_hardware_with_the_accepted_limits() {
        let limits = wgpu::Limits {
            max_buffer_size: MIN_BUFFER_LIMIT_BYTES,
            max_storage_buffer_binding_size: MIN_STORAGE_BINDING_LIMIT_BYTES,
            max_storage_buffers_per_shader_stage: MIN_STORAGE_BUFFERS_PER_STAGE,
            ..wgpu::Limits::default()
        };
        assert_eq!(
            validate_adapter_facts(
                wgpu::DeviceType::DiscreteGpu,
                wgpu::Backend::Vulkan,
                &limits,
            ),
            Ok(())
        );
        assert_eq!(
            validate_adapter_facts(wgpu::DeviceType::Cpu, wgpu::Backend::Vulkan, &limits),
            Err(WgpuRenderRuntimeError::SoftwareAdapter)
        );
        assert_eq!(
            validate_adapter_facts(wgpu::DeviceType::DiscreteGpu, wgpu::Backend::Gl, &limits),
            Err(WgpuRenderRuntimeError::UnsupportedBackend)
        );

        let mut undersized = limits;
        undersized.max_buffer_size = MIN_BUFFER_LIMIT_BYTES - 1;
        assert_eq!(
            validate_adapter_facts(
                wgpu::DeviceType::DiscreteGpu,
                wgpu::Backend::Vulkan,
                &undersized,
            ),
            Err(WgpuRenderRuntimeError::AdapterLimitsInsufficient)
        );
    }
}
