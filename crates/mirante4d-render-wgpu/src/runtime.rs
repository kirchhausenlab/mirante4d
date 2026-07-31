#![forbid(unsafe_code)]

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    mem::size_of,
    num::NonZeroU64,
    ops::Bound::{Excluded, Unbounded},
    panic::{AssertUnwindSafe, catch_unwind},
    pin::pin,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
        mpsc::{Receiver, SyncSender, TryRecvError, sync_channel},
    },
    task::{Context, Poll, Wake, Waker},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use mirante4d_dataset::{
    BrickKey, DatasetCatalog, ResourceLease, ResourcePayloadFacts, ResourcePayloadView,
};
use mirante4d_render_api::{
    CameraFrame, FrameCompleteness, FrameCoverage, FrameIdentity, FrameLimitation, FrameProgress,
    GpuLedgerCategory, IsoShadingPolicy, LogicalLayerKey, MAX_RENDER_REQUIREMENTS,
    PreparedRenderRequirements, PresentationTarget, Projection, RenderExtent, RenderIntent,
    RenderRequirement, RenderRequirements, RenderResourceGrid, RenderResourceGridCatalog,
    RenderViewIntent, SamplingPolicy, ScaleLevel, TimeIndex, VolumePickCompleteness,
    VolumePickPolicy, VolumePickQuery, VolumePickResult, VolumePickTicket, VolumePickValue,
    WorldPoint3, shader_control_world_to_grid_rows,
};

use super::{
    CoordinatedFrameExecutionReport, CoordinatedLayoutReport, CoordinatedTargetExecutionReport,
    CoordinatedTargetLayout, CoordinatedTargetLayoutState, CoordinatedTargetRequest,
    CoordinatedValidationCaptureTicket, CpuFrameTiming, GpuFrameTiming, GpuResidencyEvictionEvent,
    GpuTimingTicket, MAX_CONTROL_UPLOAD_BYTES, MAX_PAYLOAD_UPLOAD_BYTES, MAX_RENDER_HEIGHT_PIXELS,
    MAX_RENDER_WIDTH_PIXELS, MAX_UPLOADS, PipelineCapability, PipelineCompilationFailureCause,
    PipelineReadiness, RendererDeviceGeneration, RetainedFrameRenderPolicy, TargetTextureRevision,
    ValidationCapture, ValidationCaptureTicket, VolumeColorSchedule, VolumeRefinementProgress,
    WgpuRenderRuntimeConfig, WgpuRenderRuntimeDiagnostics, WgpuRenderRuntimeError,
    payload_allocation_bytes,
};
use crate::global_residency::{
    DirectoryAdmission, DirectoryPublication, DirectoryRemoval, GlobalResidencyDirectory,
    GlobalResidencyError, PAGE_RECORD_BYTES, PreparedDirectoryBatch, compact_cell_keys,
};

// The metadata ceiling admits a full budget-sized s0 working set on the
// accepted 1--8 GiB configurations. It is deliberately independent of the
// old 128/256 traversal fixture and remains count bounded.
const MAX_ACTIVE_PRESENTATION_TARGETS: usize = 4;
// The four fixed fronts plus one private eager 3D candidate are the complete
// renderer-owned presentation allocation set. The candidate may retain its
// opaque frame lease only while staging an atomic exact replacement.
const MAX_REGISTERED_PRESENTATION_TARGETS: usize = MAX_ACTIVE_PRESENTATION_TARGETS + 1;
const MAX_FRAME_LEASES: usize = MAX_RENDER_REQUIREMENTS;
// Metadata-only empty pages do not consume payload-arena bytes, but their CPU
// records are still count bounded. The ceiling keeps every page pinned by the
// four active presentations plus one complete requirement set of reuse. When
// full, only inactive records are eligible for LRU removal; the replacing
// frame's new body is protected before it becomes presentation state.
const MAX_EMPTY_RESIDENTS: usize = MAX_RENDER_REQUIREMENTS * (MAX_ACTIVE_PRESENTATION_TARGETS + 1);
const MAX_GLOBAL_RESIDENT_PAGES: usize = MAX_EMPTY_RESIDENTS;
pub(crate) const GLOBAL_DIRECTORY_SLOTS: usize = 1_048_576;
const GLOBAL_DIRECTORY_BYTES: u64 = GLOBAL_DIRECTORY_SLOTS as u64 * 32;
const GLOBAL_PAGE_RECORD_BYTES: u64 = MAX_GLOBAL_RESIDENT_PAGES as u64 * 64;
const GLOBAL_RESIDENCY_METADATA_BYTES: u64 = GLOBAL_DIRECTORY_BYTES + GLOBAL_PAGE_RECORD_BYTES;
// One key, one resident value, and a deliberately conservative sixteen-word
// BTree node/index/allocator allowance, rounded to a cache-line. This is a host
// metadata ledger charge, not a claim about one std implementation's private
// node layout.
const EMPTY_RESIDENT_INDEX_ALLOWANCE_BYTES: usize = 16 * size_of::<usize>();
const EMPTY_RESIDENT_METADATA_BYTES_PER_RECORD: u64 = host_metadata_record_bytes(
    size_of::<BrickKey>() + size_of::<ResidentResource>() + EMPTY_RESIDENT_INDEX_ALLOWANCE_BYTES,
);
const EMPTY_RESIDENT_METADATA_CAPACITY_BYTES: u64 =
    MAX_EMPTY_RESIDENTS as u64 * EMPTY_RESIDENT_METADATA_BYTES_PER_RECORD;
const MIN_BUFFER_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
const MIN_STORAGE_BINDING_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
const MIN_STORAGE_BUFFERS_PER_STAGE: u32 = 8;
const INITIAL_CONTROL_BYTES: u64 = 256 * 1024;
const COPY_ALIGNMENT: u64 = wgpu::COPY_BUFFER_ALIGNMENT;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const FACT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg8Uint;
const COLOR_BYTES_PER_PIXEL: u32 = 4;
const FACT_BYTES_PER_PIXEL: u32 = 2;
const HEADER_WORDS: usize = 32;
const LAYER_WORDS: usize = 64;
const PLANE_SCALE_WORDS: usize = 19;
const RESOURCE_WORDS: usize = 16;
const MAX_IN_FLIGHT_SUBMISSIONS: usize = 3;
const MAX_IN_FLIGHT_COLOR_CUTOFFS: usize = 2;
const TIMING_SLOT_COUNT: usize = MAX_IN_FLIGHT_SUBMISSIONS;
const TIMING_QUERY_WORDS: u32 = 4;
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
/// Enough startup commitment for the complete S3 Cell body while remaining
/// tiny relative to a multi-gigabyte logical arena. Later demand grows
/// geometrically under the configured hard maximum without speculating more
/// than a bounded amount beyond the proven per-segment placement high-watermark.
const INITIAL_PAYLOAD_COMMITMENT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PAYLOAD_GROWTH_HEADROOM_BYTES: u64 = 128 * 1024 * 1024;
static NEXT_RENDERER_DEVICE_GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct PrivatePresentationId(u64);

impl PrivatePresentationId {
    fn new(value: u64) -> Result<Self, ()> {
        (value != 0).then_some(Self(value)).ok_or(())
    }

    const fn get(self) -> u64 {
        self.0
    }
}

fn allocate_renderer_device_generation() -> Result<RendererDeviceGeneration, WgpuRenderRuntimeError>
{
    let mut current = NEXT_RENDERER_DEVICE_GENERATION.load(Ordering::Relaxed);
    loop {
        let next = current
            .checked_add(1)
            .ok_or(WgpuRenderRuntimeError::RendererDeviceGenerationExhausted)?;
        match NEXT_RENDERER_DEVICE_GENERATION.compare_exchange_weak(
            current,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return Ok(RendererDeviceGeneration(current)),
            Err(observed) => current = observed,
        }
    }
}

#[derive(Debug, Default)]
struct GpuFailureLatch {
    first: OnceLock<WgpuRenderRuntimeError>,
}

impl GpuFailureLatch {
    fn record_device_loss(&self, reason: wgpu::DeviceLostReason) {
        let error = match reason {
            wgpu::DeviceLostReason::Unknown | wgpu::DeviceLostReason::Destroyed => {
                WgpuRenderRuntimeError::DeviceLost
            }
        };
        let _ = self.first.set(error);
    }

    fn record_uncaptured_error(&self, error: &wgpu::Error) {
        let error = match error {
            wgpu::Error::OutOfMemory { .. } => WgpuRenderRuntimeError::DeviceOutOfMemory,
            wgpu::Error::Internal { .. } => WgpuRenderRuntimeError::BackendInternal,
            wgpu::Error::Validation { .. } => WgpuRenderRuntimeError::BackendValidation,
        };
        let _ = self.first.set(error);
    }

    fn ensure_available(&self) -> Result<(), WgpuRenderRuntimeError> {
        self.first.get().copied().map_or(Ok(()), Err)
    }
}

const fn host_metadata_record_bytes(bytes: usize) -> u64 {
    bytes.div_ceil(64) as u64 * 64
}

const fn empty_resident_metadata_bytes(records: usize) -> u64 {
    (records as u64).saturating_mul(EMPTY_RESIDENT_METADATA_BYTES_PER_RECORD)
}

fn map_global_residency_error(error: GlobalResidencyError) -> WgpuRenderRuntimeError {
    match error {
        GlobalResidencyError::PageCapacity | GlobalResidencyError::DirectoryCapacity => {
            WgpuRenderRuntimeError::ResidentMetadataCapacityExceeded {
                requested_records: MAX_GLOBAL_RESIDENT_PAGES.saturating_add(1),
                maximum_records: MAX_GLOBAL_RESIDENT_PAGES,
                requested_bytes: GLOBAL_PAGE_RECORD_BYTES.saturating_add(64),
                available_bytes: GLOBAL_PAGE_RECORD_BYTES,
            }
        }
        _ => WgpuRenderRuntimeError::PayloadContractMismatch,
    }
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
    index: &BTreeSet<(u64, BrickKey)>,
    after: Option<(u64, BrickKey)>,
) -> Option<(u64, BrickKey)> {
    match after {
        Some(entry) => index.range((Excluded(entry), Unbounded)).next().copied(),
        None => index.first().copied(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ResidentFrameLease(u64);

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrameState {
    frame: FrameIdentity,
    residency: ResidentFrameLease,
}

#[derive(Debug, Clone)]
struct PlannedFrame {
    frame: FrameIdentity,
    requirements: RenderRequirements,
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
    /// Hard shader-visible/configuration ceiling for this segment.
    logical_capacity: u64,
    /// Usable bytes currently backed by `buffer` and owned by `allocator`.
    capacity: u64,
    /// Physical buffer allocation. Empty logical segments retain one minimum
    /// binding-sized dummy buffer while committing zero usable payload bytes.
    allocated_bytes: u64,
    allocator: ArenaAllocator,
}

#[derive(Debug, Clone, Copy)]
struct PayloadRelocation {
    key: BrickKey,
    segment: usize,
    source_offset: u64,
    destination_offset: u64,
    bytes: u64,
    destination_validity_offset: Option<u64>,
}

struct PayloadCompactionPlan {
    relocations: Vec<PayloadRelocation>,
    allocators: Vec<ArenaAllocator>,
}

fn prepare_segment_payload_compaction(
    segment: usize,
    capacity: u64,
    mut resources: Vec<(BrickKey, u64, u64, Option<u64>)>,
) -> (Vec<PayloadRelocation>, ArenaAllocator) {
    resources.sort_by_key(|(key, offset, _, _)| (*offset, *key));
    let mut allocator = ArenaAllocator::new(capacity);
    let mut relocations = Vec::new();
    for (key, source_offset, bytes, validity_offset) in resources {
        let destination_offset = allocator
            .allocate(bytes)
            .expect("packing the existing segment cannot exceed its capacity");
        if destination_offset == source_offset {
            continue;
        }
        let destination_validity_offset = validity_offset
            .map(|validity| destination_offset + validity.saturating_sub(source_offset));
        relocations.push(PayloadRelocation {
            key,
            segment,
            source_offset,
            destination_offset,
            bytes,
            destination_validity_offset,
        });
    }
    (relocations, allocator)
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

    fn largest_contiguous_bytes(&self) -> u64 {
        self.by_size.last().map_or(0, |(bytes, _)| *bytes)
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

    fn grow(&mut self, previous_capacity: u64, new_capacity: u64) {
        assert!(new_capacity >= previous_capacity);
        self.release(previous_capacity, new_capacity - previous_capacity);
    }

    fn remove_free_tail(&mut self, previous_capacity: u64, new_capacity: u64) {
        assert!(new_capacity >= previous_capacity);
        assert!(self.reserve_exact(
            previous_capacity,
            new_capacity.saturating_sub(previous_capacity)
        ));
    }

    #[cfg(test)]
    fn ranges(&self) -> Vec<(u64, u64)> {
        self.by_offset
            .iter()
            .map(|(start, end)| (*start, *end))
            .collect()
    }
}

fn payload_allocation_failure(
    segments: &[PayloadSegment],
    requested_bytes: u64,
) -> WgpuRenderRuntimeError {
    let total_free_bytes = segments
        .iter()
        .map(|segment| segment.allocator.available_bytes())
        .sum();
    let largest_contiguous_bytes = segments
        .iter()
        .map(|segment| segment.allocator.largest_contiguous_bytes())
        .max()
        .unwrap_or(0);
    classify_payload_allocation_failure(requested_bytes, total_free_bytes, largest_contiguous_bytes)
}

fn payload_segment_allocation_order(
    segment_count: usize,
    resident: &BTreeMap<BrickKey, ResidentResource>,
) -> Vec<usize> {
    let mut preserved_prefixes = vec![0_u64; segment_count];
    for resource in resident
        .values()
        .filter(|resource| resource.allocated_bytes != 0)
    {
        let segment = resource.segment as usize;
        if segment < segment_count {
            preserved_prefixes[segment] = preserved_prefixes[segment]
                .max(resource.offset.saturating_add(resource.allocated_bytes));
        }
    }
    let mut order = (0..segment_count).collect::<Vec<_>>();
    order.sort_by_key(|segment| {
        (
            preserved_prefixes[*segment] != 0,
            preserved_prefixes[*segment],
            *segment,
        )
    });
    order
}

fn classify_payload_allocation_failure(
    requested_bytes: u64,
    total_free_bytes: u64,
    largest_contiguous_bytes: u64,
) -> WgpuRenderRuntimeError {
    let requested_bytes = align_copy(requested_bytes);
    if total_free_bytes >= requested_bytes && largest_contiguous_bytes < requested_bytes {
        WgpuRenderRuntimeError::PayloadPlacementUnavailable {
            requested_bytes,
            total_free_bytes,
            largest_contiguous_bytes,
        }
    } else {
        WgpuRenderRuntimeError::CapacityExceeded {
            category: GpuLedgerCategory::PayloadResidency,
            requested_bytes,
            available_bytes: total_free_bytes,
        }
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
    SimulatedGrowth {
        segment: usize,
        previous_capacity: u64,
        new_capacity: u64,
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
            ArenaOperation::SimulatedGrowth {
                segment,
                previous_capacity,
                new_capacity,
            } => segments[segment]
                .allocator
                .remove_free_tail(previous_capacity, new_capacity),
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
            ArenaOperation::SimulatedGrowth { .. } => {
                unreachable!("simulated arena growth is never committed")
            }
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
        label: Some("mirante4d-render-target-color"),
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
            label: Some("mirante4d-render-target-validation"),
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

fn mapped_staging_buffer(
    device: &wgpu::Device,
    gpu_failure: &GpuFailureLatch,
    label: &'static str,
    bytes: &[u8],
) -> Result<wgpu::Buffer, WgpuRenderRuntimeError> {
    debug_assert!(!bytes.is_empty() && bytes.len().is_multiple_of(COPY_ALIGNMENT as usize));
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: true,
    });
    gpu_failure.ensure_available()?;
    buffer
        .slice(..)
        .get_mapped_range_mut()
        .copy_from_slice(bytes);
    buffer.unmap();
    gpu_failure.ensure_available()?;
    Ok(buffer)
}

fn encode_render_pass(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
    display: &DisplayTarget,
    region: ColorRenderRegion,
    timestamps: Option<(&wgpu::QuerySet, u32, u32)>,
) {
    encode_render_views(
        encoder,
        pipeline,
        bind_group,
        &display.color_view,
        display.fact_view.as_ref(),
        display.extent,
        region,
        timestamps,
    );
}

#[allow(clippy::too_many_arguments)]
fn encode_render_views(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
    color_view: &wgpu::TextureView,
    fact_view: Option<&wgpu::TextureView>,
    extent: RenderExtent,
    region: ColorRenderRegion,
    timestamps: Option<(&wgpu::QuerySet, u32, u32)>,
) {
    let color_load = if region.clear {
        wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
    } else {
        wgpu::LoadOp::Load
    };
    let attachments = [
        Some(wgpu::RenderPassColorAttachment {
            view: color_view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: color_load,
                store: wgpu::StoreOp::Store,
            },
        }),
        fact_view.map(|fact_view| wgpu::RenderPassColorAttachment {
            view: fact_view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: color_load,
                store: wgpu::StoreOp::Store,
            },
        }),
    ];
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("mirante4d-color-pass"),
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
    pass.set_scissor_rect(0, region.y, extent.width_pixels(), region.height);
    pass.draw(0..3, 0..1);
}

fn encode_capture(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    id: u64,
    presentation: PrivatePresentationId,
    frame: FrameIdentity,
    display: &DisplayTarget,
) -> Result<PendingCapture, WgpuRenderRuntimeError> {
    let (color_padded_row, fact_offset, fact_padded_row, allocated_bytes) =
        capture_layout(display.extent)?;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mirante4d-validation-readback"),
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
            private_presentation: presentation.get(),
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

fn control_layer_indices(intent: &RenderIntent) -> Vec<usize> {
    let mut indices = (0..intent.layers().len()).collect::<Vec<_>>();
    if ColorKernel::for_intent(intent) == ColorKernel::Dvr {
        indices.sort_unstable_by_key(|index| intent.layers()[*index].layer());
    }
    indices
}

fn build_control(
    catalog: &DatasetCatalog,
    intent: &RenderIntent,
    selected_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
    layer_indices: &[usize],
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

    for &layer_index in layer_indices {
        let layer_intent = &intent.layers()[layer_index];
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
        let grid_to_world = scale.grid_to_world();
        let transform = grid_to_world.row_major();
        let inverse_transform = shader_control_world_to_grid_rows(grid_to_world)
            .map_err(|_| WgpuRenderRuntimeError::UnsupportedView)?;
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
        record[4] = inverse_transform[0][0].to_bits();
        record[5] = inverse_transform[1][1].to_bits();
        record[6] = inverse_transform[2][2].to_bits();
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
        for (index, value) in inverse_transform.into_iter().flatten().enumerate() {
            record[32 + index] = value.to_bits();
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
    let bytes = bytemuck::cast_slice::<u32, u8>(&words).to_vec();
    if bytes.len() as u64 > MAX_CONTROL_UPLOAD_BYTES {
        return Err(WgpuRenderRuntimeError::ControlCapacityExceeded);
    }
    Ok(bytes)
}

fn build_target_control(
    catalog: &DatasetCatalog,
    grids: &RenderResourceGridCatalog,
    intent: &RenderIntent,
    requirements: &RenderRequirements,
    directory_capacity: u32,
) -> Result<Vec<u8>, WgpuRenderRuntimeError> {
    let selected_scales = requirements
        .scale_chains()
        .iter()
        .map(|chain| (chain.layer(), chain.target()))
        .collect::<BTreeMap<_, _>>();
    let layer_indices = control_layer_indices(intent);
    let mut control = build_control(catalog, intent, &selected_scales, &layer_indices)?;
    let base_word_len = control.len() / std::mem::size_of::<u32>();
    let mut plane_scale_words = Vec::new();
    let timepoint = intent.timepoint().get();
    {
        let words = bytemuck::try_cast_slice_mut::<u8, u32>(&mut control)
            .map_err(|_| WgpuRenderRuntimeError::PayloadContractMismatch)?;
        words[7] = directory_capacity;
        for (layer_index, &intent_layer_index) in layer_indices.iter().enumerate() {
            let layer = &intent.layers()[intent_layer_index];
            let chain = requirements
                .scale_chain(layer.layer())
                .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
            let grid = grids
                .grid(layer.layer(), chain.target())
                .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
            let cell = grid.cell_shape().dimensions();
            let record = HEADER_WORDS + layer_index * LAYER_WORDS;
            words[record + 25] = timepoint as u32;
            words[record + 26] = (timepoint >> 32) as u32;
            words[record + 27] = u64_to_u32(cell[2])?;
            words[record + 28] = u64_to_u32(cell[1])?;
            words[record + 29] = u64_to_u32(cell[0])?;
            words[record + 30] = 0;
            words[record + 31] = 0;
            if intent.view().pass_kind() == mirante4d_render_api::RenderPassKind::Plane {
                words[record + 30] = u32::try_from(
                    base_word_len
                        .checked_add(plane_scale_words.len())
                        .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?,
                )
                .map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;
                words[record + 31] = u32::try_from(chain.scales().len())
                    .map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;
                let catalog_layer = catalog
                    .layer(layer.layer())
                    .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
                for scale_level in chain.scales().iter().copied() {
                    let scale = catalog_layer
                        .scale(scale_level)
                        .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
                    let scale_grid = grids
                        .grid(layer.layer(), scale_level)
                        .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
                    let shape = scale.shape().dimensions();
                    let cell = scale_grid.cell_shape().dimensions();
                    let inverse_transform =
                        shader_control_world_to_grid_rows(scale.grid_to_world())
                            .map_err(|_| WgpuRenderRuntimeError::UnsupportedView)?;
                    let mut scale_record = [0_u32; PLANE_SCALE_WORDS];
                    scale_record[0] = scale_level.get();
                    scale_record[1] = u64_to_u32(shape[2])?;
                    scale_record[2] = u64_to_u32(shape[1])?;
                    scale_record[3] = u64_to_u32(shape[0])?;
                    scale_record[4] = u64_to_u32(cell[2])?;
                    scale_record[5] = u64_to_u32(cell[1])?;
                    scale_record[6] = u64_to_u32(cell[0])?;
                    for (index, value) in inverse_transform.into_iter().flatten().enumerate() {
                        scale_record[7 + index] = value.to_bits();
                    }
                    plane_scale_words.extend_from_slice(&scale_record);
                }
            }
        }
    }
    if !plane_scale_words.is_empty() {
        control.extend_from_slice(bytemuck::cast_slice(&plane_scale_words));
        if control.len() as u64 > MAX_CONTROL_UPLOAD_BYTES {
            return Err(WgpuRenderRuntimeError::ControlCapacityExceeded);
        }
    }
    Ok(control)
}

fn resource_record(
    key: BrickKey,
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

fn set_control_full_coverage(control: &mut [u8], full: bool) -> Result<(), WgpuRenderRuntimeError> {
    let words = bytemuck::try_cast_slice_mut::<u8, u32>(control)
        .map_err(|_| WgpuRenderRuntimeError::PayloadContractMismatch)?;
    let flag = words
        .get_mut(28)
        .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
    *flag = u32::from(full);
    Ok(())
}

/// Publishes a renderer-resident whole-volume page directly into each layer
/// record. Field 62 stores `page_record_index + 1`; zero retains the ordinary
/// sparse-directory path.
///
/// This is deliberately derived after residency preparation. A preflight
/// control body cannot name a page that is admitted by the same cutoff, while
/// this point has one stable globally-owned page record for every resident
/// requirement.
fn full_volume_requirement_key(
    catalog: &DatasetCatalog,
    intent: &RenderIntent,
    requirements: &RenderRequirements,
    layer: LogicalLayerKey,
) -> Result<Option<BrickKey>, WgpuRenderRuntimeError> {
    let chain = requirements
        .scale_chain(layer)
        .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
    let scale = chain.target();
    let volume_shape = catalog
        .layer(layer)
        .and_then(|catalog_layer| catalog_layer.scale(scale))
        .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?
        .shape()
        .dimensions();
    let mut matching = requirements
        .resources()
        .map(|requirement| requirement.key())
        .filter(|key| {
            key.layer() == layer && key.timepoint() == intent.timepoint() && key.scale() == scale
        });
    let Some(key) = matching.next() else {
        return Ok(None);
    };
    Ok((matching.next().is_none()
        && key.region().origin() == [0, 0, 0]
        && key.region().shape().dimensions() == volume_shape)
        .then_some(key))
}

fn set_control_full_resource_fast_paths(
    control: &mut [u8],
    catalog: &DatasetCatalog,
    intent: &RenderIntent,
    requirements: &RenderRequirements,
    residency: &ResidencyOwner,
) -> Result<(), WgpuRenderRuntimeError> {
    let words = bytemuck::try_cast_slice_mut::<u8, u32>(control)
        .map_err(|_| WgpuRenderRuntimeError::PayloadContractMismatch)?;
    let layer_indices = control_layer_indices(intent);
    for (control_layer_index, intent_layer_index) in layer_indices.into_iter().enumerate() {
        let intent_layer = &intent.layers()[intent_layer_index];
        let layer = intent_layer.layer();
        let Some(key) = full_volume_requirement_key(catalog, intent, requirements, layer)? else {
            continue;
        };
        let Some(resource) = residency.resident_resource(key) else {
            continue;
        };
        let page_record_plus_one = resource.page_record_index.checked_add(1).ok_or(
            WgpuRenderRuntimeError::ResidentMetadataCapacityExceeded {
                requested_records: MAX_GLOBAL_RESIDENT_PAGES.saturating_add(1),
                maximum_records: MAX_GLOBAL_RESIDENT_PAGES,
                requested_bytes: GLOBAL_PAGE_RECORD_BYTES.saturating_add(PAGE_RECORD_BYTES),
                available_bytes: GLOBAL_PAGE_RECORD_BYTES,
            },
        )?;
        let record = HEADER_WORDS
            .checked_add(
                control_layer_index
                    .checked_mul(LAYER_WORDS)
                    .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?,
            )
            .ok_or(WgpuRenderRuntimeError::ControlCapacityExceeded)?;
        *words
            .get_mut(record + 62)
            .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)? = page_record_plus_one;
    }
    Ok(())
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
    grid: Option<RenderResourceGrid>,
    page_record_index: u32,
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
        grid: None,
        page_record_index: u32::MAX,
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

/// Renderer-global decoded-payload ingress in exact first-seen order.
///
/// The inbox is bounded by the renderer's canonical requirement ceiling.
/// Per-execution selection applies the smaller 512-resource/8-MiB transfer
/// envelope without removing deferred entries, so polling a dataset result
/// never requires an application-owned retry queue. The map owns and
/// deduplicates CPU-ledger-accounted leases while the deque is the sole
/// scheduling order.
struct PendingLeaseQueue {
    by_key: BTreeMap<BrickKey, Arc<dyn ResourceLease>>,
    order: VecDeque<BrickKey>,
}

struct PendingLeaseSelection {
    leases: Vec<Arc<dyn ResourceLease>>,
    #[cfg(test)]
    visited_offers: usize,
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
        }
    }

    fn len(&self) -> usize {
        self.order.len()
    }

    fn clear(&mut self) {
        self.by_key.clear();
        self.order.clear();
    }

    fn relevant_order(&self, requirements: &RenderRequirements) -> VecDeque<BrickKey> {
        self.order
            .iter()
            .copied()
            .filter(|key| requirements.contains_resource(*key))
            .collect()
    }

    fn remove_batch(&mut self, keys: &BTreeSet<BrickKey>) {
        if keys.is_empty() {
            return;
        }
        for key in keys {
            self.by_key.remove(key);
        }
        self.order.retain(|key| !keys.contains(key));
        debug_assert_eq!(self.by_key.len(), self.order.len());
    }

    /// Atomically admits unique, nonresident offers in caller order.
    ///
    /// Resident, already-pending, and duplicate keys are idempotent no-ops.
    /// A capacity error leaves the existing inbox byte-for-byte unchanged.
    fn offer(
        &mut self,
        offers: &[Arc<dyn ResourceLease>],
        resident: &BTreeMap<BrickKey, ResidentResource>,
    ) -> Result<Vec<BrickKey>, WgpuRenderRuntimeError> {
        let mut batch_keys = BTreeSet::new();
        let mut additional = Vec::new();
        for lease in offers {
            let key = lease.key();
            if resident.contains_key(&key)
                || self.by_key.contains_key(&key)
                || !batch_keys.insert(key)
            {
                continue;
            }
            additional.push(Arc::clone(lease));
        }

        let combined_count = self.len().checked_add(additional.len()).ok_or(
            WgpuRenderRuntimeError::LeaseCapacityExceeded {
                actual: usize::MAX,
                maximum: MAX_RENDER_REQUIREMENTS,
            },
        )?;
        if combined_count > MAX_RENDER_REQUIREMENTS {
            return Err(WgpuRenderRuntimeError::LeaseCapacityExceeded {
                actual: combined_count,
                maximum: MAX_RENDER_REQUIREMENTS,
            });
        }

        let mut admitted = Vec::with_capacity(additional.len());
        for lease in additional {
            let key = lease.key();
            let previous = self.by_key.insert(key, lease);
            debug_assert!(previous.is_none());
            self.order.push_back(key);
            admitted.push(key);
        }
        Ok(admitted)
    }

    /// Selects the oldest relevant offers that fit one transfer execution.
    ///
    /// Irrelevant offers remain ordered for another target. An individually
    /// oversized payload fails visibly rather than becoming a permanently
    /// stranded head entry.
    fn select_for_execution(
        &self,
        resident: &BTreeMap<BrickKey, ResidentResource>,
        relevant: impl FnMut(BrickKey) -> bool,
    ) -> Result<PendingLeaseSelection, WgpuRenderRuntimeError> {
        self.select_ordered_for_execution(resident, self.order.iter().copied(), relevant)
    }

    fn select_indexed_for_execution(
        &self,
        resident: &BTreeMap<BrickKey, ResidentResource>,
        relevant_order: &VecDeque<BrickKey>,
    ) -> Result<PendingLeaseSelection, WgpuRenderRuntimeError> {
        self.select_ordered_for_execution(resident, relevant_order.iter().copied(), |_| true)
    }

    fn select_ordered_for_execution(
        &self,
        resident: &BTreeMap<BrickKey, ResidentResource>,
        order: impl IntoIterator<Item = BrickKey>,
        mut relevant: impl FnMut(BrickKey) -> bool,
    ) -> Result<PendingLeaseSelection, WgpuRenderRuntimeError> {
        let mut selected = Vec::new();
        let mut selected_bytes = 0_u64;
        #[cfg(test)]
        let mut visited_offers = 0;
        for key in order {
            if selected.len() == MAX_UPLOADS {
                break;
            }
            #[cfg(test)]
            {
                visited_offers += 1;
            }
            if resident.contains_key(&key) || !relevant(key) {
                continue;
            }
            let lease = self
                .by_key
                .get(&key)
                .expect("pending lease order points to owned lease");
            let bytes = pending_lease_transfer_bytes(lease.as_ref());
            if bytes > MAX_PAYLOAD_UPLOAD_BYTES {
                return Err(WgpuRenderRuntimeError::CapacityExceeded {
                    category: GpuLedgerCategory::TransferStaging,
                    requested_bytes: bytes,
                    available_bytes: MAX_PAYLOAD_UPLOAD_BYTES,
                });
            }
            if selected_bytes
                .checked_add(bytes)
                .is_none_or(|total| total > MAX_PAYLOAD_UPLOAD_BYTES)
            {
                break;
            }
            selected_bytes += bytes;
            selected.push(Arc::clone(lease));
        }
        Ok(PendingLeaseSelection {
            leases: selected,
            #[cfg(test)]
            visited_offers,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderedResidencyFreshness {
    Current,
    RequiredAddition,
    RequiredRemoval,
}

impl RenderedResidencyFreshness {
    fn record_required_change(&mut self, available: bool) {
        if !available {
            *self = Self::RequiredRemoval;
        } else if *self == Self::Current {
            *self = Self::RequiredAddition;
        }
    }

    const fn requires_refresh(self) -> bool {
        !matches!(self, Self::Current)
    }

    const fn required_resource_was_removed(self) -> bool {
        matches!(self, Self::RequiredRemoval)
    }
}

struct PresentationState {
    logical_target: PresentationTarget,
    texture_revision: TargetTextureRevision,
    frame_state: Option<FrameState>,
    last_rendered_frame: Option<FrameIdentity>,
    /// Scheduling authority of the currently exposed 3D volume pixels.
    ///
    /// A full-size interactive preview can be pixel-identical to a direct
    /// pass, but it remains provisional: it owns no exact validation capture
    /// and cannot satisfy an exact request without one real exact render.
    last_rendered_volume_schedule: Option<VolumeColorSchedule>,
    last_rendered_timepoint: Option<TimeIndex>,
    last_rendered_volume: bool,
    last_rendered_layers: Vec<LogicalLayerKey>,
    last_rendered_modes: Vec<u32>,
    last_rendered_sampling: Vec<SamplingPolicy>,
    availability: Option<FrameCoverage>,
    last_progress: Option<FrameProgress>,
    /// Exact semantic body from which the currently exposed pixels were
    /// recorded. Frame identity alone is insufficient because one accepted
    /// frame may replace its prepared body before a color cutoff is admitted.
    last_rendered_requirements: Option<RenderRequirements>,
    /// O(1) currentness of the last pixel-producing frame relative to the
    /// global residency directory. Required removals dominate additions until
    /// a real color render publishes pixels from the current directory.
    rendered_residency_freshness: RenderedResidencyFreshness,
    control_buffer: wgpu::Buffer,
    control_capacity: u64,
    render_bind_group: wgpu::BindGroup,
    pick_bind_groups: Vec<wgpu::BindGroup>,
    display: DisplayTarget,
    pending_capture: Option<PendingCapture>,
    hidden_volume_refinement: Option<HiddenVolumeRefinementState>,
}

#[derive(Clone)]
struct HiddenVolumeRefinementState {
    job_id: u64,
    frame: FrameIdentity,
    requirements: RenderRequirements,
    extent: RenderExtent,
    completed_rows: Arc<AtomicU32>,
    _control_buffer: wgpu::Buffer,
    _render_bind_group: wgpu::BindGroup,
    control_capacity: u64,
    status: HiddenRefinementStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HiddenRefinementStatus {
    Running,
    Complete {
        batches: u32,
        elapsed_ns: u64,
        last_batch_rows: u32,
    },
    Failed,
}

impl HiddenVolumeRefinementState {
    fn matches(&self, request: CoordinatedTargetRequest<'_>) -> bool {
        self.frame == request.intent().frame()
            && self.extent == request.intent().extent()
            && self
                .requirements
                .shares_resources_with(request.requirements())
            && self.requirements.prefetch_promoted() == request.requirements().prefetch_promoted()
            && matches!(
                request.volume_schedule(),
                VolumeColorSchedule::AtomicRefinement { .. }
            )
    }

    fn progress(&self) -> VolumeRefinementProgress {
        VolumeRefinementProgress::new(
            self.completed_rows
                .load(Ordering::Acquire)
                .min(self.extent.height_pixels()),
            self.extent.height_pixels(),
        )
    }

    const fn is_actionable(&self) -> bool {
        !matches!(self.status, HiddenRefinementStatus::Running)
    }
}

const HIDDEN_REFINEMENT_TARGET_NS: u64 = 3_000_000;
const HIDDEN_REFINEMENT_MAX_BATCH_ROWS: u32 = 256;
const HIDDEN_REFINEMENT_BATCH_TIMEOUT: Duration = Duration::from_secs(2);

type HiddenRefinementWake = Arc<dyn Fn() + Send + Sync + 'static>;

struct HiddenRefinementJob {
    id: u64,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    color_view: wgpu::TextureView,
    fact_view: Option<wgpu::TextureView>,
    extent: RenderExtent,
    initial_batch_rows: u32,
    completed_rows: Arc<AtomicU32>,
}

struct HiddenRefinementMailbox {
    latest_job: u64,
    pending: Option<HiddenRefinementJob>,
    shutdown: bool,
}

#[derive(Debug, Clone, Copy)]
enum HiddenRefinementWorkerOutcome {
    Complete {
        batches: u32,
        rows: u32,
        elapsed_ns: u64,
        last_batch_rows: u32,
    },
    Cancelled {
        batches: u32,
        rows: u32,
        elapsed_ns: u64,
    },
    Failed {
        batches: u32,
        rows: u32,
        elapsed_ns: u64,
    },
}

#[derive(Debug, Clone, Copy)]
struct HiddenRefinementWorkerResult {
    job_id: u64,
    outcome: HiddenRefinementWorkerOutcome,
}

struct HiddenRefinementScheduler {
    mailbox: Arc<(Mutex<HiddenRefinementMailbox>, Condvar)>,
    results: Arc<Mutex<VecDeque<HiddenRefinementWorkerResult>>>,
    wake: Arc<Mutex<Option<HiddenRefinementWake>>>,
    worker: Option<JoinHandle<()>>,
    next_job: u64,
}

impl HiddenRefinementScheduler {
    fn spawn(
        device: wgpu::Device,
        queue: wgpu::Queue,
        gpu_failure: Arc<GpuFailureLatch>,
    ) -> Result<Self, WgpuRenderRuntimeError> {
        let mailbox = Arc::new((
            Mutex::new(HiddenRefinementMailbox {
                latest_job: 0,
                pending: None,
                shutdown: false,
            }),
            Condvar::new(),
        ));
        let results = Arc::new(Mutex::new(VecDeque::with_capacity(2)));
        let wake = Arc::new(Mutex::new(None::<HiddenRefinementWake>));
        let worker_mailbox = Arc::clone(&mailbox);
        let worker_results = Arc::clone(&results);
        let worker_wake = Arc::clone(&wake);
        let worker = thread::Builder::new()
            .name("mirante4d-hidden-refinement".to_owned())
            .spawn(move || {
                hidden_refinement_worker(
                    device,
                    queue,
                    gpu_failure,
                    worker_mailbox,
                    worker_results,
                    worker_wake,
                );
            })
            .map_err(|_| WgpuRenderRuntimeError::HiddenRefinementWorkerSpawnFailed)?;
        Ok(Self {
            mailbox,
            results,
            wake,
            worker: Some(worker),
            next_job: 1,
        })
    }

    fn set_wake(&self, wake: HiddenRefinementWake) {
        *self
            .wake
            .lock()
            .expect("the hidden-refinement wake slot is never poisoned") = Some(wake);
    }

    fn allocate_job(&mut self) -> Result<u64, WgpuRenderRuntimeError> {
        let id = self.next_job;
        self.next_job = self
            .next_job
            .checked_add(1)
            .ok_or(WgpuRenderRuntimeError::HiddenRefinementIdentityExhausted)?;
        Ok(id)
    }

    fn replace(&self, job: HiddenRefinementJob) {
        let (mailbox, ready) = &*self.mailbox;
        let mut mailbox = mailbox
            .lock()
            .expect("the hidden-refinement mailbox is never poisoned");
        mailbox.latest_job = job.id;
        mailbox.pending = Some(job);
        ready.notify_one();
    }

    fn cancel(&self, job_id: u64) {
        let (mailbox, ready) = &*self.mailbox;
        let mut mailbox = mailbox
            .lock()
            .expect("the hidden-refinement mailbox is never poisoned");
        if mailbox.latest_job == job_id {
            mailbox.latest_job = 0;
            mailbox.pending = None;
            ready.notify_one();
        }
    }

    fn has_result(&self) -> bool {
        !self
            .results
            .lock()
            .expect("the hidden-refinement result queue is never poisoned")
            .is_empty()
    }

    fn drain_results(&self) -> Vec<HiddenRefinementWorkerResult> {
        self.results
            .lock()
            .map(|mut results| results.drain(..).collect())
            .unwrap_or_default()
    }
}

impl Drop for HiddenRefinementScheduler {
    fn drop(&mut self) {
        let (mailbox, ready) = &*self.mailbox;
        if let Ok(mut mailbox) = mailbox.lock() {
            mailbox.shutdown = true;
            mailbox.pending = None;
            mailbox.latest_job = 0;
            ready.notify_one();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn hidden_refinement_worker(
    device: wgpu::Device,
    queue: wgpu::Queue,
    gpu_failure: Arc<GpuFailureLatch>,
    mailbox: Arc<(Mutex<HiddenRefinementMailbox>, Condvar)>,
    results: Arc<Mutex<VecDeque<HiddenRefinementWorkerResult>>>,
    wake: Arc<Mutex<Option<HiddenRefinementWake>>>,
) {
    loop {
        let job = {
            let (mailbox, ready) = &*mailbox;
            let mut state = mailbox
                .lock()
                .expect("the hidden-refinement mailbox is never poisoned");
            while state.pending.is_none() && !state.shutdown {
                state = ready
                    .wait(state)
                    .expect("the hidden-refinement mailbox is never poisoned");
            }
            if state.shutdown {
                return;
            }
            state
                .pending
                .take()
                .expect("a woken refinement worker owns one pending job")
        };
        let job_id = job.id;
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            execute_hidden_refinement_job(&device, &queue, &gpu_failure, &mailbox, job)
        }))
        .ok()
        .unwrap_or(HiddenRefinementWorkerOutcome::Failed {
            batches: 0,
            rows: 0,
            elapsed_ns: 0,
        });
        if let Ok(mut queue) = results.lock() {
            if queue.len() == 2 {
                queue.pop_front();
            }
            queue.push_back(HiddenRefinementWorkerResult { job_id, outcome });
        }
        if !matches!(outcome, HiddenRefinementWorkerOutcome::Cancelled { .. })
            && let Ok(wake) = wake.lock()
            && let Some(wake) = wake.as_ref()
        {
            wake();
        }
    }
}

fn execute_hidden_refinement_job(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    gpu_failure: &GpuFailureLatch,
    mailbox: &Arc<(Mutex<HiddenRefinementMailbox>, Condvar)>,
    job: HiddenRefinementJob,
) -> HiddenRefinementWorkerOutcome {
    let mut next_y = 0_u32;
    let mut batch_rows = job
        .initial_batch_rows
        .clamp(1, HIDDEN_REFINEMENT_MAX_BATCH_ROWS)
        .min(job.extent.height_pixels());
    let started = Instant::now();
    let mut batches = 0_u32;
    let mut last_batch_rows = batch_rows;
    while next_y < job.extent.height_pixels() {
        let current = mailbox
            .0
            .lock()
            .map(|mailbox| mailbox.latest_job)
            .unwrap_or(0);
        if current != job.id {
            return HiddenRefinementWorkerOutcome::Cancelled {
                batches,
                rows: next_y,
                elapsed_ns: elapsed_nanoseconds(&started),
            };
        }
        let rows = batch_rows.min(job.extent.height_pixels() - next_y);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mirante4d-hidden-exact-refinement"),
        });
        encode_render_views(
            &mut encoder,
            &job.pipeline,
            &job.bind_group,
            &job.color_view,
            job.fact_view.as_ref(),
            job.extent,
            ColorRenderRegion {
                y: next_y,
                height: rows,
                clear: next_y == 0,
            },
            None,
        );
        let batch_started = Instant::now();
        let submission = queue.submit([encoder.finish()]);
        if device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(HIDDEN_REFINEMENT_BATCH_TIMEOUT),
            })
            .is_err()
            || gpu_failure.ensure_available().is_err()
        {
            return HiddenRefinementWorkerOutcome::Failed {
                batches,
                rows: next_y,
                elapsed_ns: elapsed_nanoseconds(&started),
            };
        }
        let elapsed_ns = elapsed_nanoseconds(&batch_started);
        next_y = next_y.saturating_add(rows);
        last_batch_rows = rows;
        batches = batches.saturating_add(1);
        job.completed_rows.store(next_y, Ordering::Release);
        if next_y == job.extent.height_pixels() {
            break;
        }
        batch_rows = adapted_hidden_batch_rows(
            rows,
            elapsed_ns,
            job.extent.height_pixels().saturating_sub(next_y),
        );
    }
    let still_current = mailbox
        .0
        .lock()
        .map(|mailbox| mailbox.latest_job == job.id)
        .unwrap_or(false);
    if !still_current {
        return HiddenRefinementWorkerOutcome::Cancelled {
            batches,
            rows: next_y,
            elapsed_ns: elapsed_nanoseconds(&started),
        };
    }
    HiddenRefinementWorkerOutcome::Complete {
        batches,
        rows: job.extent.height_pixels(),
        elapsed_ns: elapsed_nanoseconds(&started),
        last_batch_rows,
    }
}

fn adapted_hidden_batch_rows(current_rows: u32, elapsed_ns: u64, remaining_rows: u32) -> u32 {
    let proportional = if elapsed_ns == 0 {
        u64::from(current_rows).saturating_mul(4)
    } else {
        u64::from(current_rows)
            .saturating_mul(HIDDEN_REFINEMENT_TARGET_NS)
            .checked_div(elapsed_ns)
            .unwrap_or(1)
    };
    let minimum = u64::from((current_rows / 2).max(1));
    let maximum = u64::from(current_rows.saturating_mul(4).max(1));
    u32::try_from(proportional.clamp(minimum, maximum))
        .unwrap_or(HIDDEN_REFINEMENT_MAX_BATCH_ROWS)
        .clamp(1, HIDDEN_REFINEMENT_MAX_BATCH_ROWS)
        .min(remaining_rows.max(1))
}

#[derive(Debug, Clone, Copy)]
struct CoordinatedSlot {
    desired: bool,
    desired_extent: Option<RenderExtent>,
    front: Option<PrivatePresentationId>,
    candidate: Option<PrivatePresentationId>,
    /// A color-eligible cutoff for this fixed logical target was explicitly
    /// deferred. Retry authority follows the logical target across private
    /// front/candidate swaps and never belongs to either allocation.
    color_retry_pending: bool,
}

impl CoordinatedSlot {
    const fn empty() -> Self {
        Self {
            desired: false,
            desired_extent: None,
            front: None,
            candidate: None,
            color_retry_pending: false,
        }
    }
}

/// Sole owner of fixed-target allocation/binding, texture revisions,
/// color-pass ordering, current pixel facts, and color-completion leases.
struct FrameCoordinator {
    device_generation: RendererDeviceGeneration,
    next_texture_revision: u64,
    next_private_presentation: u64,
    slots: [CoordinatedSlot; 4],
    presentations: BTreeMap<PrivatePresentationId, PresentationState>,
    pending_residency_targets: BTreeSet<PrivatePresentationId>,
    last_recorded_targets: Vec<PresentationTarget>,
    latest_observed_frames: [Option<FrameIdentity>; 4],
    residency_generation: u64,
    next_color_cutoff: u64,
    in_flight_color_leases: BTreeMap<u64, Vec<ResidentFrameLease>>,
    in_flight_color_cutoffs: Arc<AtomicUsize>,
    completed_color_cutoffs: Arc<Mutex<Vec<CompletedColorCutoff>>>,
}

impl FrameCoordinator {
    fn new() -> Result<Self, WgpuRenderRuntimeError> {
        Ok(Self {
            device_generation: allocate_renderer_device_generation()?,
            next_texture_revision: 1,
            next_private_presentation: 1,
            slots: [CoordinatedSlot::empty(); 4],
            presentations: BTreeMap::new(),
            pending_residency_targets: BTreeSet::new(),
            last_recorded_targets: Vec::with_capacity(PresentationTarget::ALL.len()),
            latest_observed_frames: [None; 4],
            residency_generation: 1,
            next_color_cutoff: 1,
            in_flight_color_leases: BTreeMap::new(),
            in_flight_color_cutoffs: Arc::new(AtomicUsize::new(0)),
            completed_color_cutoffs: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn allocate_color_cutoff(&mut self) -> u64 {
        loop {
            let cutoff = self.next_color_cutoff;
            self.next_color_cutoff = self.next_color_cutoff.wrapping_add(1).max(1);
            if !self.in_flight_color_leases.contains_key(&cutoff) {
                return cutoff;
            }
        }
    }

    fn allocate_texture_revision(
        &mut self,
    ) -> Result<TargetTextureRevision, WgpuRenderRuntimeError> {
        let revision = TargetTextureRevision(self.next_texture_revision);
        self.next_texture_revision = self
            .next_texture_revision
            .checked_add(1)
            .ok_or(WgpuRenderRuntimeError::TextureRevisionExhausted)?;
        Ok(revision)
    }

    fn allocate_private_presentation(
        &mut self,
    ) -> Result<PrivatePresentationId, WgpuRenderRuntimeError> {
        let token = PrivatePresentationId::new(self.next_private_presentation)
            .map_err(|_| WgpuRenderRuntimeError::PrivatePresentationIdExhausted)?;
        self.next_private_presentation = self
            .next_private_presentation
            .checked_add(1)
            .ok_or(WgpuRenderRuntimeError::PrivatePresentationIdExhausted)?;
        Ok(token)
    }

    fn slot(&self, target: PresentationTarget) -> &CoordinatedSlot {
        &self.slots[target.index()]
    }

    fn slot_mut(&mut self, target: PresentationTarget) -> &mut CoordinatedSlot {
        &mut self.slots[target.index()]
    }

    fn front_token(
        &self,
        target: PresentationTarget,
    ) -> Result<PrivatePresentationId, WgpuRenderRuntimeError> {
        let slot = self.slot(target);
        if !slot.desired {
            return Err(WgpuRenderRuntimeError::CoordinatedTargetNotConfigured { target });
        }
        slot.front
            .ok_or(WgpuRenderRuntimeError::CoordinatedTargetNotConfigured { target })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColorKernel {
    Plane,
    Mip,
    Dvr,
    Iso,
    Mixed,
}

impl ColorKernel {
    fn for_intent(intent: &RenderIntent) -> Self {
        if matches!(intent.view(), RenderViewIntent::CrossSection(_)) {
            return Self::Plane;
        }

        let mut all_mip = true;
        let mut all_dvr = true;
        let mut all_iso = true;
        for layer in intent.layers() {
            let state = layer.render_state();
            all_mip &= state.mip_parameters().is_some();
            all_dvr &= state.dvr_parameters().is_some();
            all_iso &= state.iso_parameters().is_some();
        }
        if all_mip {
            Self::Mip
        } else if all_dvr {
            Self::Dvr
        } else if all_iso {
            Self::Iso
        } else {
            Self::Mixed
        }
    }
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

    const fn capacity(&self) -> u64 {
        self.capacity
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

    fn stage(
        &mut self,
        index: usize,
        uploads: &[UploadPlan<'_>],
        directory: Option<&PreparedDirectoryBatch>,
        removals: &[DirectoryRemoval<'_>],
        admitted_page_records: &[[u32; RESOURCE_WORDS]],
        gpu_failure: &GpuFailureLatch,
    ) -> Result<u64, WgpuRenderRuntimeError> {
        gpu_failure.ensure_available()?;
        let slot = &mut self.slots[index];
        debug_assert!(matches!(slot.state, UploadStagingState::Mapped));
        let payload_bytes = uploads
            .iter()
            .map(|upload| upload.layout.allocation_bytes)
            .sum::<u64>();
        let metadata_bytes = directory.map_or(0, |prepared| {
            let directory_bytes = match prepared.publication() {
                DirectoryPublication::Incremental {
                    removal_writes,
                    insertion_writes,
                } => removal_writes.len().saturating_add(insertion_writes.len()) as u64 * 32,
                DirectoryPublication::Rebuilt { slots } => slots.len() as u64 * 32,
            };
            directory_bytes.saturating_add(
                removals.len().saturating_add(admitted_page_records.len()) as u64 * 64,
            )
        });
        let required = payload_bytes.saturating_add(metadata_bytes);
        let mut mapped = slot.buffer.slice(..required).get_mapped_range_mut();
        let mut staging_offset = 0_u64;
        let mut padding_zero_bytes = 0_u64;
        if let Some(prepared) = directory
            && let DirectoryPublication::Incremental { removal_writes, .. } = prepared.publication()
        {
            for write in removal_writes {
                let bytes = bytemuck::bytes_of(write.slot());
                let start = staging_offset as usize;
                mapped
                    .slice(start..start + bytes.len())
                    .copy_from_slice(bytes);
                staging_offset += bytes.len() as u64;
            }
        }
        for _ in removals {
            let start = staging_offset as usize;
            mapped
                .slice(start..start + RESOURCE_WORDS * size_of::<u32>())
                .fill(0);
            staging_offset += (RESOURCE_WORDS * size_of::<u32>()) as u64;
        }
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
        for record in admitted_page_records {
            let bytes = bytemuck::cast_slice(record);
            let start = staging_offset as usize;
            mapped
                .slice(start..start + bytes.len())
                .copy_from_slice(bytes);
            staging_offset += bytes.len() as u64;
        }
        if let Some(prepared) = directory {
            match prepared.publication() {
                DirectoryPublication::Incremental {
                    insertion_writes, ..
                } => {
                    for write in insertion_writes {
                        let bytes = bytemuck::bytes_of(write.slot());
                        let start = staging_offset as usize;
                        mapped
                            .slice(start..start + bytes.len())
                            .copy_from_slice(bytes);
                        staging_offset += bytes.len() as u64;
                    }
                }
                DirectoryPublication::Rebuilt { slots } => {
                    let bytes = bytemuck::cast_slice(slots);
                    let start = staging_offset as usize;
                    mapped
                        .slice(start..start + bytes.len())
                        .copy_from_slice(bytes);
                    staging_offset += bytes.len() as u64;
                }
            }
        }
        debug_assert_eq!(staging_offset, required);
        drop(mapped);
        slot.buffer.unmap();
        gpu_failure.ensure_available()?;
        slot.state = UploadStagingState::Encoding;
        Ok(padding_zero_bytes)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the transfer encoder keeps independent bounded buffers and prepared batches borrowed"
    )]
    fn encode_copies(
        &self,
        index: usize,
        encoder: &mut wgpu::CommandEncoder,
        payload_segments: &[PayloadSegment],
        uploads: &[UploadPlan<'_>],
        directory_buffer: &wgpu::Buffer,
        page_record_buffer: &wgpu::Buffer,
        directory: Option<&PreparedDirectoryBatch>,
        removals: &[DirectoryRemoval<'_>],
        admitted_page_records: &[[u32; RESOURCE_WORDS]],
    ) {
        let slot = &self.slots[index];
        debug_assert!(matches!(slot.state, UploadStagingState::Encoding));
        let mut staging_offset = 0_u64;
        if let Some(prepared) = directory
            && let DirectoryPublication::Incremental { removal_writes, .. } = prepared.publication()
        {
            for write in removal_writes {
                encoder.copy_buffer_to_buffer(
                    &slot.buffer,
                    staging_offset,
                    directory_buffer,
                    write.byte_offset(),
                    32,
                );
                staging_offset += 32;
            }
        }
        for removal in removals {
            encoder.copy_buffer_to_buffer(
                &slot.buffer,
                staging_offset,
                page_record_buffer,
                u64::from(removal.page_index()) * 64,
                64,
            );
            staging_offset += 64;
        }
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
        if let Some(prepared) = directory {
            for (page_index, _) in prepared
                .admission_page_indices()
                .iter()
                .copied()
                .zip(admitted_page_records)
            {
                encoder.copy_buffer_to_buffer(
                    &slot.buffer,
                    staging_offset,
                    page_record_buffer,
                    u64::from(page_index) * 64,
                    64,
                );
                staging_offset += 64;
            }
            match prepared.publication() {
                DirectoryPublication::Incremental {
                    insertion_writes, ..
                } => {
                    for write in insertion_writes {
                        encoder.copy_buffer_to_buffer(
                            &slot.buffer,
                            staging_offset,
                            directory_buffer,
                            write.byte_offset(),
                            32,
                        );
                        staging_offset += 32;
                    }
                }
                DirectoryPublication::Rebuilt { slots } => {
                    let bytes = slots.len() as u64 * 32;
                    encoder.copy_buffer_to_buffer(
                        &slot.buffer,
                        staging_offset,
                        directory_buffer,
                        0,
                        bytes,
                    );
                    staging_offset += bytes;
                }
            }
        }
        debug_assert!(staging_offset <= slot.capacity);
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
    key: BrickKey,
    victims: Vec<BrickKey>,
    segment: usize,
    offset: u64,
    payload: ResourcePayloadView<'a>,
    layout: PayloadLayout,
    resident: ResidentResource,
}

/// Private result of preparing one coordinated target's residency state.
///
/// Color recording, capture, timing, and presentation publication remain
/// solely owned by `execute_coordinated_frame`.
struct ResidencyPreparationReport {
    frame: FrameIdentity,
    progress: Option<FrameProgress>,
    visited_resources: usize,
    uploaded_resources: usize,
    payload_upload_bytes: u64,
    command_buffers: u32,
    queue_submissions: u32,
    deferred_by_backpressure: bool,
    newly_resident_keys: Box<[BrickKey]>,
    evicted_keys: Box<[BrickKey]>,
}

#[derive(Clone, Copy)]
struct CoordinatedExecutionPlan<'a> {
    request: CoordinatedTargetRequest<'a>,
    token: PrivatePresentationId,
    candidate: bool,
}

struct CoordinatedColorPass<'a> {
    plan: CoordinatedExecutionPlan<'a>,
    report_index: usize,
    progress: FrameProgress,
    control: Vec<u8>,
    new_display: Option<DisplayTarget>,
    new_texture_revision: Option<TargetTextureRevision>,
    pending_capture: Option<PendingCapture>,
    capture_ticket: Option<CoordinatedValidationCaptureTicket>,
    render_region: ColorRenderRegion,
    volume_refinement: Option<VolumeRefinementProgress>,
    publishes_frame: bool,
    captures_validation: bool,
}

struct CoordinatedHiddenPass<'a> {
    plan: CoordinatedExecutionPlan<'a>,
    report_index: usize,
    control: Vec<u8>,
    new_display: Option<DisplayTarget>,
    new_texture_revision: Option<TargetTextureRevision>,
    initial_batch_rows: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ColorRenderRegion {
    y: u32,
    height: u32,
    clear: bool,
}

enum PlannedColorWork {
    Main {
        region: ColorRenderRegion,
        progress: Option<VolumeRefinementProgress>,
        publishes_frame: bool,
    },
    StartHidden {
        initial_batch_rows: u32,
        progress: VolumeRefinementProgress,
    },
    HiddenRunning {
        progress: VolumeRefinementProgress,
    },
    HiddenFailed,
}

fn planned_color_work(
    presentation: &PresentationState,
    plan: CoordinatedExecutionPlan<'_>,
) -> PlannedColorWork {
    let extent = plan.request.intent().extent();
    let full = ColorRenderRegion {
        y: 0,
        height: extent.height_pixels(),
        clear: true,
    };
    let VolumeColorSchedule::AtomicRefinement {
        strip_height_pixels,
    } = plan.request.volume_schedule()
    else {
        return PlannedColorWork::Main {
            region: full,
            progress: None,
            publishes_frame: true,
        };
    };
    let Some(refinement) = presentation
        .hidden_volume_refinement
        .as_ref()
        .filter(|state| state.matches(plan.request))
    else {
        return PlannedColorWork::StartHidden {
            initial_batch_rows: strip_height_pixels,
            progress: VolumeRefinementProgress::new(0, extent.height_pixels()),
        };
    };
    match refinement.status {
        HiddenRefinementStatus::Running => PlannedColorWork::HiddenRunning {
            progress: refinement.progress(),
        },
        HiddenRefinementStatus::Complete { .. } => {
            if plan.request.hidden_promotion_authorized() {
                PlannedColorWork::Main {
                    region: ColorRenderRegion {
                        y: 0,
                        height: 0,
                        clear: false,
                    },
                    progress: Some(refinement.progress()),
                    publishes_frame: true,
                }
            } else {
                PlannedColorWork::HiddenRunning {
                    progress: refinement.progress(),
                }
            }
        }
        HiddenRefinementStatus::Failed => PlannedColorWork::HiddenFailed,
    }
}

const fn volume_schedule_requires_exact_promotion(
    rendered: Option<VolumeColorSchedule>,
    requested: VolumeColorSchedule,
) -> bool {
    matches!(rendered, Some(VolumeColorSchedule::InteractivePreview))
        && !matches!(requested, VolumeColorSchedule::InteractivePreview)
}

const fn volume_schedule_requires_color_continuation(
    rendered: Option<VolumeColorSchedule>,
    unfinished_atomic_strips_match: bool,
    requested: VolumeColorSchedule,
) -> bool {
    unfinished_atomic_strips_match || volume_schedule_requires_exact_promotion(rendered, requested)
}

struct PreparedPresentationAllocation {
    token: PrivatePresentationId,
    presentation: PresentationState,
    bind_group_creations: u64,
}

struct PreparedDisplayReplacement {
    token: PrivatePresentationId,
    display: DisplayTarget,
    texture_revision: TargetTextureRevision,
}

struct CompletedColorCutoff {
    residency_generation: u64,
    cutoff: u64,
}

struct TimingResources {
    query_set: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    timestamp_period_ns: f32,
    encoder_timestamps: bool,
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
        render_pass_timestamps: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimingQueryLayout {
    batch_envelope: Option<(u32, u32)>,
    render_pass: Option<(u32, u32)>,
    query_count: u32,
}

impl TimingQueryLayout {
    fn new(query_base: u32, batch_envelope: bool, render_pass: bool) -> Self {
        let mut next = query_base;
        let batch_envelope = batch_envelope.then(|| {
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

const PIPELINE_COMPILE_EVENT_CAPACITY: usize = 2;

#[derive(Debug)]
enum PipelineCompileEvent<Initial, Pick> {
    InitialRenderReady(Initial),
    Ready(Pick),
    Failed {
        capability: PipelineCapability,
        cause: PipelineCompilationFailureCause,
    },
}

struct PipelineCompiler<Initial, Pick> {
    receiver: Receiver<PipelineCompileEvent<Initial, Pick>>,
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl<Initial: Send + 'static, Pick: Send + 'static> PipelineCompiler<Initial, Pick> {
    fn spawn(
        compile_initial: impl FnOnce() -> Result<Initial, PipelineCompilationFailureCause>
        + Send
        + 'static,
        compile_pick: impl FnOnce() -> Result<Pick, PipelineCompilationFailureCause> + Send + 'static,
    ) -> Result<Self, WgpuRenderRuntimeError> {
        let (sender, receiver) = sync_channel(PIPELINE_COMPILE_EVENT_CAPACITY);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let worker = thread::Builder::new()
            .name("mirante4d-pipeline-compiler".to_owned())
            .spawn(move || {
                run_pipeline_compiler(
                    sender,
                    worker_cancelled.as_ref(),
                    compile_initial,
                    compile_pick,
                );
            })
            .map_err(|_| WgpuRenderRuntimeError::PipelineCompilerSpawnFailed)?;
        Ok(Self {
            receiver,
            cancelled,
            worker: Some(worker),
        })
    }
}

impl<Initial, Pick> PipelineCompiler<Initial, Pick> {
    fn try_next(&self) -> Result<Option<PipelineCompileEvent<Initial, Pick>>, TryRecvError> {
        match self.receiver.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(error @ TryRecvError::Disconnected) => Err(error),
        }
    }

    fn join(&mut self) -> Result<(), ()> {
        self.worker
            .take()
            .map_or(Ok(()), |worker| worker.join().map_err(|_| ()))
    }

    fn cancel_and_detach(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        // Driver pipeline creation is not cancellable. Detaching lets cold
        // shutdown return immediately; the worker owns cloned GPU handles for
        // every resource it may still touch.
        let _ = self.worker.take();
    }
}

impl<Initial, Pick> Drop for PipelineCompiler<Initial, Pick> {
    fn drop(&mut self) {
        self.cancel_and_detach();
    }
}

fn run_pipeline_compiler<Initial, Pick>(
    sender: SyncSender<PipelineCompileEvent<Initial, Pick>>,
    cancelled: &AtomicBool,
    compile_initial: impl FnOnce() -> Result<Initial, PipelineCompilationFailureCause>,
    compile_pick: impl FnOnce() -> Result<Pick, PipelineCompilationFailureCause>,
) {
    let initial_result = catch_unwind(AssertUnwindSafe(compile_initial));
    if cancelled.load(Ordering::Acquire) {
        return;
    }
    let initial = match initial_result {
        Ok(Ok(initial)) => initial,
        Ok(Err(cause)) => {
            let _ = sender.send(PipelineCompileEvent::Failed {
                capability: PipelineCapability::InitialRender,
                cause,
            });
            return;
        }
        Err(_) => {
            let _ = sender.send(PipelineCompileEvent::Failed {
                capability: PipelineCapability::InitialRender,
                cause: PipelineCompilationFailureCause::WorkerPanicked,
            });
            return;
        }
    };
    if sender
        .send(PipelineCompileEvent::InitialRenderReady(initial))
        .is_err()
    {
        return;
    }
    if cancelled.load(Ordering::Acquire) {
        return;
    }

    let pick_result = catch_unwind(AssertUnwindSafe(compile_pick));
    if cancelled.load(Ordering::Acquire) {
        return;
    }
    let pick = match pick_result {
        Ok(Ok(pick)) => pick,
        Ok(Err(cause)) => {
            let _ = sender.send(PipelineCompileEvent::Failed {
                capability: PipelineCapability::Pick,
                cause,
            });
            return;
        }
        Err(_) => {
            let _ = sender.send(PipelineCompileEvent::Failed {
                capability: PipelineCapability::Pick,
                cause: PipelineCompilationFailureCause::WorkerPanicked,
            });
            return;
        }
    };
    let _ = sender.send(PipelineCompileEvent::Ready(pick));
}

struct CurrentThreadWake(thread::Thread);

impl Wake for CurrentThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

fn wait_for_device_future<Output>(
    device: &wgpu::Device,
    future: impl Future<Output = Output>,
) -> Result<Output, PipelineCompilationFailureCause> {
    let waker = Waker::from(Arc::new(CurrentThreadWake(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return Ok(output);
        }
        device
            .poll(wgpu::PollType::Poll)
            .map_err(|_| PipelineCompilationFailureCause::BackendInternal)?;
        // The future's waker unparks this thread. An unpark racing ahead of
        // this call leaves a token, so the worker neither spins nor loses the
        // completion notification.
        thread::park();
    }
}

fn pipeline_failure_cause(error: &wgpu::Error) -> PipelineCompilationFailureCause {
    match error {
        wgpu::Error::OutOfMemory { .. } => PipelineCompilationFailureCause::DeviceOutOfMemory,
        wgpu::Error::Internal { .. } => PipelineCompilationFailureCause::BackendInternal,
        wgpu::Error::Validation { .. } => PipelineCompilationFailureCause::Validation,
    }
}

fn compile_with_pipeline_error_scopes<Output>(
    device: &wgpu::Device,
    compile: impl FnOnce() -> Output,
) -> Result<Output, PipelineCompilationFailureCause> {
    let out_of_memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let output = compile();
    let validation_error = wait_for_device_future(device, validation.pop())?;
    let internal_error = wait_for_device_future(device, internal.pop())?;
    let out_of_memory_error = wait_for_device_future(device, out_of_memory.pop())?;
    if let Some(error) = validation_error.or(internal_error).or(out_of_memory_error) {
        return Err(pipeline_failure_cause(&error));
    }
    Ok(output)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the ordered compiler keeps each shader and pipeline operation separately fallible"
)]
fn compile_initial_pipeline_program<
    PlaneShader,
    Plane,
    MipShader,
    Mip,
    DvrShader,
    Dvr,
    IsoShader,
    Iso,
    MixedShader,
    Mixed,
>(
    create_plane_shader: impl FnOnce() -> Result<PlaneShader, PipelineCompilationFailureCause>,
    create_plane: impl FnOnce(&PlaneShader) -> Result<Plane, PipelineCompilationFailureCause>,
    create_mip_shader: impl FnOnce() -> Result<MipShader, PipelineCompilationFailureCause>,
    create_mip: impl FnOnce(&MipShader) -> Result<Mip, PipelineCompilationFailureCause>,
    create_dvr_shader: impl FnOnce() -> Result<DvrShader, PipelineCompilationFailureCause>,
    create_dvr: impl FnOnce(&DvrShader) -> Result<Dvr, PipelineCompilationFailureCause>,
    create_iso_shader: impl FnOnce() -> Result<IsoShader, PipelineCompilationFailureCause>,
    create_iso: impl FnOnce(&IsoShader) -> Result<Iso, PipelineCompilationFailureCause>,
    create_mixed_shader: impl FnOnce() -> Result<MixedShader, PipelineCompilationFailureCause>,
    create_mixed: impl FnOnce(&MixedShader) -> Result<Mixed, PipelineCompilationFailureCause>,
) -> Result<(Plane, Mip, Dvr, Iso, Mixed), PipelineCompilationFailureCause> {
    let plane_shader = create_plane_shader()?;
    let plane = create_plane(&plane_shader)?;
    let mip_shader = create_mip_shader()?;
    let mip = create_mip(&mip_shader)?;
    let dvr_shader = create_dvr_shader()?;
    let dvr = create_dvr(&dvr_shader)?;
    let iso_shader = create_iso_shader()?;
    let iso = create_iso(&iso_shader)?;
    let mixed_shader = create_mixed_shader()?;
    let mixed = create_mixed(&mixed_shader)?;
    Ok((plane, mip, dvr, iso, mixed))
}

fn compile_pick_pipeline_program<Shader, Pick>(
    create_shader: impl FnOnce() -> Result<Shader, PipelineCompilationFailureCause>,
    create_pick: impl FnOnce(&Shader) -> Result<Pick, PipelineCompilationFailureCause>,
) -> Result<Pick, PipelineCompilationFailureCause> {
    let shader = create_shader()?;
    create_pick(&shader)
}

struct InitialPipelines {
    plane: wgpu::RenderPipeline,
    mip: wgpu::RenderPipeline,
    dvr: wgpu::RenderPipeline,
    iso: wgpu::RenderPipeline,
    mixed: wgpu::RenderPipeline,
}

impl InitialPipelines {
    fn for_intent(&self, intent: &RenderIntent) -> &wgpu::RenderPipeline {
        match ColorKernel::for_intent(intent) {
            ColorKernel::Plane => &self.plane,
            ColorKernel::Mip => &self.mip,
            ColorKernel::Dvr => &self.dvr,
            ColorKernel::Iso => &self.iso,
            ColorKernel::Mixed => &self.mixed,
        }
    }
}

struct PickResources {
    layout: wgpu::BindGroupLayout,
    slots: Vec<PickSlot>,
}

struct PipelineState<Initial = InitialPipelines, Pick = wgpu::ComputePipeline> {
    readiness: PipelineReadiness,
    initial: Option<Initial>,
    pick: Option<Pick>,
    first_failure: Option<WgpuRenderRuntimeError>,
    compiler: Option<PipelineCompiler<Initial, Pick>>,
}

impl<Initial, Pick> PipelineState<Initial, Pick> {
    fn compiling(compiler: PipelineCompiler<Initial, Pick>) -> Self {
        Self {
            readiness: PipelineReadiness::CompilingInitial,
            initial: None,
            pick: None,
            first_failure: None,
            compiler: Some(compiler),
        }
    }

    fn readiness(&self) -> Result<PipelineReadiness, WgpuRenderRuntimeError> {
        self.first_failure.map_or(Ok(self.readiness), Err)
    }

    fn capability_is_ready(
        &self,
        capability: PipelineCapability,
    ) -> Result<bool, WgpuRenderRuntimeError> {
        if let Some(failure) = self.first_failure {
            return Err(failure);
        }
        Ok(match capability {
            PipelineCapability::InitialRender => self.initial.is_some(),
            PipelineCapability::Pick => self.pick.is_some(),
        })
    }

    fn ensure_capability(
        &self,
        capability: PipelineCapability,
    ) -> Result<(), WgpuRenderRuntimeError> {
        self.capability_is_ready(capability)?
            .then_some(())
            .ok_or(WgpuRenderRuntimeError::PipelineNotReady { capability })
    }

    fn poll(&mut self) -> Result<PipelineReadiness, WgpuRenderRuntimeError> {
        if let Some(failure) = self.first_failure {
            return Err(failure);
        }
        let event = match self.compiler.as_ref().map(PipelineCompiler::try_next) {
            None => return Ok(self.readiness),
            Some(Ok(None)) => return Ok(self.readiness),
            Some(Ok(Some(event))) => event,
            Some(Err(TryRecvError::Disconnected)) => {
                let capability = match self.readiness {
                    PipelineReadiness::CompilingInitial => PipelineCapability::InitialRender,
                    PipelineReadiness::InitialRenderReady => PipelineCapability::Pick,
                    PipelineReadiness::Ready => {
                        unreachable!("the terminal Ready event consumes the compiler")
                    }
                };
                return Err(self.latch_detached_failure(
                    capability,
                    PipelineCompilationFailureCause::WorkerStopped,
                ));
            }
            Some(Err(TryRecvError::Empty)) => unreachable!("empty was handled above"),
        };
        match event {
            PipelineCompileEvent::InitialRenderReady(initial) => {
                if self.readiness != PipelineReadiness::CompilingInitial {
                    return Err(self.latch_detached_failure(
                        PipelineCapability::InitialRender,
                        PipelineCompilationFailureCause::WorkerStopped,
                    ));
                }
                self.initial = Some(initial);
                self.readiness = PipelineReadiness::InitialRenderReady;
            }
            PipelineCompileEvent::Ready(pick) => {
                if self.readiness != PipelineReadiness::InitialRenderReady {
                    return Err(self.latch_detached_failure(
                        PipelineCapability::Pick,
                        PipelineCompilationFailureCause::WorkerStopped,
                    ));
                }
                self.pick = Some(pick);
                self.readiness = PipelineReadiness::Ready;
                self.join_terminal_compiler();
            }
            PipelineCompileEvent::Failed { capability, cause } => {
                return Err(self.latch_terminal_failure(capability, cause));
            }
        }
        Ok(self.readiness)
    }

    fn latch_terminal_failure(
        &mut self,
        capability: PipelineCapability,
        cause: PipelineCompilationFailureCause,
    ) -> WgpuRenderRuntimeError {
        let error = WgpuRenderRuntimeError::PipelineCompilationFailed { capability, cause };
        if self.first_failure.is_none() {
            self.first_failure = Some(error);
        }
        self.join_terminal_compiler();
        self.first_failure
            .expect("the first pipeline compilation failure was latched")
    }

    fn latch_detached_failure(
        &mut self,
        capability: PipelineCapability,
        cause: PipelineCompilationFailureCause,
    ) -> WgpuRenderRuntimeError {
        let error = WgpuRenderRuntimeError::PipelineCompilationFailed { capability, cause };
        if self.first_failure.is_none() {
            self.first_failure = Some(error);
        }
        self.cancel_and_detach_compiler();
        self.first_failure
            .expect("the first pipeline compilation failure was latched")
    }

    fn join_terminal_compiler(&mut self) {
        if let Some(mut compiler) = self.compiler.take() {
            let _ = compiler.join();
        }
    }

    fn cancel_and_detach_compiler(&mut self) {
        if let Some(mut compiler) = self.compiler.take() {
            compiler.cancel_and_detach();
        }
    }
}

impl PipelineState {
    fn initial(&self) -> Result<&InitialPipelines, WgpuRenderRuntimeError> {
        self.ensure_capability(PipelineCapability::InitialRender)?;
        Ok(self
            .initial
            .as_ref()
            .expect("an available initial-render capability owns both color pipelines"))
    }

    fn pick(&self) -> Result<&wgpu::ComputePipeline, WgpuRenderRuntimeError> {
        self.ensure_capability(PipelineCapability::Pick)?;
        Ok(self
            .pick
            .as_ref()
            .expect("an available pick capability owns its compute pipeline"))
    }
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
        label: Some("mirante4d-render-timestamps"),
        ty: wgpu::QueryType::Timestamp,
        count: query_count,
    });
    let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mirante4d-timing-resolve"),
        size: TIMING_RESOLVE_STRIDE * TIMING_SLOT_COUNT as u64,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let slots = (0..TIMING_SLOT_COUNT)
        .map(|_| TimingSlot {
            readback: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mirante4d-timing-readback"),
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
        encoder_timestamps: device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS),
        slots,
        completed: BTreeMap::new(),
        completed_order: VecDeque::new(),
    }
}

fn create_pick_fixed_resources(
    device: &wgpu::Device,
    directory_bytes: u64,
    page_record_bytes: u64,
) -> (PickResources, wgpu::PipelineLayout) {
    let mut layout_entries = vec![storage_layout_entry_for_stage(
        0,
        INITIAL_CONTROL_BYTES,
        true,
        wgpu::ShaderStages::COMPUTE,
    )];
    layout_entries.extend((0..MAX_PAYLOAD_SEGMENTS).map(|index| {
        storage_layout_entry_for_stage(
            1 + index as u32,
            COPY_ALIGNMENT,
            true,
            wgpu::ShaderStages::COMPUTE,
        )
    }));
    layout_entries.push(storage_layout_entry_for_stage(
        5,
        directory_bytes,
        true,
        wgpu::ShaderStages::COMPUTE,
    ));
    layout_entries.push(storage_layout_entry_for_stage(
        6,
        page_record_bytes,
        true,
        wgpu::ShaderStages::COMPUTE,
    ));
    layout_entries.push(uniform_layout_entry_for_stage(
        7,
        PICK_QUERY_BYTES,
        wgpu::ShaderStages::COMPUTE,
    ));
    layout_entries.push(storage_layout_entry_for_stage(
        8,
        PICK_OUTPUT_BYTES,
        false,
        wgpu::ShaderStages::COMPUTE,
    ));
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("mirante4d-pick-bind-group-layout"),
        entries: &layout_entries,
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("mirante4d-pick-pipeline-layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let slots = create_pick_slots(device);
    (PickResources { layout, slots }, pipeline_layout)
}

fn compile_pick_pipeline(
    device: &wgpu::Device,
    pipeline_layout: &wgpu::PipelineLayout,
) -> Result<wgpu::ComputePipeline, PipelineCompilationFailureCause> {
    let shader_source = format!(
        "{}\n{}\n{}\n{}\n{}",
        include_str!("shader_common.wgsl"),
        include_str!("volume_common.wgsl"),
        include_str!("dvr_optics.wgsl"),
        include_str!("iso_gradient.wgsl"),
        include_str!("pick_shader.wgsl")
    );
    compile_pick_pipeline_program(
        || {
            compile_with_pipeline_error_scopes(device, || {
                device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("mirante4d-pick-shader"),
                    source: wgpu::ShaderSource::Wgsl(Cow::Owned(shader_source)),
                })
            })
        },
        |shader| {
            compile_with_pipeline_error_scopes(device, || {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("mirante4d-pick-pipeline"),
                    layout: Some(pipeline_layout),
                    module: shader,
                    entry_point: Some("pick_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    cache: None,
                })
            })
        },
    )
}

fn compile_initial_pipelines(
    device: &wgpu::Device,
    pipeline_layout: &wgpu::PipelineLayout,
    validation_capture: bool,
) -> Result<InitialPipelines, PipelineCompilationFailureCause> {
    let fragment_targets = [
        Some(wgpu::ColorTargetState {
            format: COLOR_FORMAT,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        }),
        validation_capture.then_some(wgpu::ColorTargetState {
            format: FACT_FORMAT,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        }),
    ];
    let plane_source = format!(
        "{}\n{}\n{}",
        include_str!("shader_common.wgsl"),
        include_str!("plane_sampling.wgsl"),
        include_str!("plane_shader.wgsl")
    );
    let mip_source = format!(
        "{}\n{}\n{}\n{}",
        include_str!("shader_common.wgsl"),
        include_str!("volume_common.wgsl"),
        include_str!("mip_core.wgsl"),
        include_str!("mip_shader.wgsl")
    );
    let dvr_source = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        include_str!("shader_common.wgsl"),
        include_str!("volume_common.wgsl"),
        include_str!("volume_composite.wgsl"),
        include_str!("dvr_optics.wgsl"),
        include_str!("dvr_core.wgsl"),
        include_str!("dvr_shader.wgsl")
    );
    let iso_source = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        include_str!("shader_common.wgsl"),
        include_str!("volume_common.wgsl"),
        include_str!("volume_composite.wgsl"),
        include_str!("iso_gradient.wgsl"),
        include_str!("iso_core.wgsl"),
        include_str!("iso_shader.wgsl")
    );
    let mixed_source = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
        include_str!("shader_common.wgsl"),
        include_str!("volume_common.wgsl"),
        include_str!("volume_composite.wgsl"),
        include_str!("mip_core.wgsl"),
        include_str!("dvr_optics.wgsl"),
        include_str!("dvr_core.wgsl"),
        include_str!("iso_gradient.wgsl"),
        include_str!("iso_core.wgsl"),
        include_str!("mixed_shader.wgsl")
    );
    let (plane, mip, dvr, iso, mixed) = compile_initial_pipeline_program(
        || {
            compile_with_pipeline_error_scopes(device, || {
                device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("mirante4d-plane-shader"),
                    source: wgpu::ShaderSource::Wgsl(Cow::Owned(plane_source)),
                })
            })
        },
        |shader| {
            compile_with_pipeline_error_scopes(device, || {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("mirante4d-plane-pipeline"),
                    layout: Some(pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: shader,
                        entry_point: Some("vs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: shader,
                        entry_point: Some(if validation_capture {
                            "fs_plane_validation"
                        } else {
                            "fs_plane_color"
                        }),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &fragment_targets,
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            })
        },
        || {
            compile_with_pipeline_error_scopes(device, || {
                device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("mirante4d-mip-shader"),
                    source: wgpu::ShaderSource::Wgsl(Cow::Owned(mip_source)),
                })
            })
        },
        |shader| {
            compile_with_pipeline_error_scopes(device, || {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("mirante4d-mip-pipeline"),
                    layout: Some(pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: shader,
                        entry_point: Some("vs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: shader,
                        entry_point: Some(if validation_capture {
                            "fs_mip_validation"
                        } else {
                            "fs_mip_color"
                        }),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &fragment_targets,
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            })
        },
        || {
            compile_with_pipeline_error_scopes(device, || {
                device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("mirante4d-dvr-shader"),
                    source: wgpu::ShaderSource::Wgsl(Cow::Owned(dvr_source)),
                })
            })
        },
        |shader| {
            compile_with_pipeline_error_scopes(device, || {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("mirante4d-dvr-pipeline"),
                    layout: Some(pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: shader,
                        entry_point: Some("vs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: shader,
                        entry_point: Some(if validation_capture {
                            "fs_dvr_validation"
                        } else {
                            "fs_dvr_color"
                        }),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &fragment_targets,
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            })
        },
        || {
            compile_with_pipeline_error_scopes(device, || {
                device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("mirante4d-iso-shader"),
                    source: wgpu::ShaderSource::Wgsl(Cow::Owned(iso_source)),
                })
            })
        },
        |shader| {
            compile_with_pipeline_error_scopes(device, || {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("mirante4d-iso-pipeline"),
                    layout: Some(pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: shader,
                        entry_point: Some("vs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: shader,
                        entry_point: Some(if validation_capture {
                            "fs_iso_validation"
                        } else {
                            "fs_iso_color"
                        }),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &fragment_targets,
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            })
        },
        || {
            compile_with_pipeline_error_scopes(device, || {
                device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("mirante4d-mixed-shader"),
                    source: wgpu::ShaderSource::Wgsl(Cow::Owned(mixed_source)),
                })
            })
        },
        |shader| {
            compile_with_pipeline_error_scopes(device, || {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("mirante4d-mixed-pipeline"),
                    layout: Some(pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: shader,
                        entry_point: Some("vs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: shader,
                        entry_point: Some(if validation_capture {
                            "fs_mixed_validation"
                        } else {
                            "fs_mixed_color"
                        }),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &fragment_targets,
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            })
        },
    )?;
    Ok(InitialPipelines {
        plane,
        mip,
        dvr,
        iso,
        mixed,
    })
}

fn create_pick_slots(device: &wgpu::Device) -> Vec<PickSlot> {
    (0..PICK_SLOT_COUNT)
        .map(|_| {
            let query_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mirante4d-pick-query"),
                size: PICK_QUERY_BYTES,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mirante4d-pick-output"),
                size: PICK_OUTPUT_BYTES,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mirante4d-pick-readback"),
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
    directory_buffer: &wgpu::Buffer,
    page_record_buffer: &wgpu::Buffer,
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
                resource: directory_buffer.as_entire_binding(),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: 6,
                resource: page_record_buffer.as_entire_binding(),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: 7,
                resource: slot.query_buffer.as_entire_binding(),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: 8,
                resource: slot.output_buffer.as_entire_binding(),
            });
            create_counted_bind_group(
                device,
                bind_group_creations,
                &wgpu::BindGroupDescriptor {
                    label: Some("mirante4d-target-pick-bind-group"),
                    layout: &pick.layout,
                    entries: &entries,
                },
            )
        })
        .collect()
}

fn create_presentation_render_bind_group(
    device: &wgpu::Device,
    render_layout: &wgpu::BindGroupLayout,
    payload_segments: &[PayloadSegment],
    directory_buffer: &wgpu::Buffer,
    page_record_buffer: &wgpu::Buffer,
    control_buffer: &wgpu::Buffer,
    bind_group_creations: &mut u64,
) -> wgpu::BindGroup {
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
        resource: directory_buffer.as_entire_binding(),
    });
    entries.push(wgpu::BindGroupEntry {
        binding: 6,
        resource: page_record_buffer.as_entire_binding(),
    });
    create_counted_bind_group(
        device,
        bind_group_creations,
        &wgpu::BindGroupDescriptor {
            label: Some("mirante4d-target-render-bind-group"),
            layout: render_layout,
            entries: &entries,
        },
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "one construction boundary wires the fixed control and global-residency bindings"
)]
fn create_presentation_control_resources(
    device: &wgpu::Device,
    render_layout: &wgpu::BindGroupLayout,
    pick: &PickResources,
    payload_segments: &[PayloadSegment],
    directory_buffer: &wgpu::Buffer,
    page_record_buffer: &wgpu::Buffer,
    control_capacity: u64,
    bind_group_creations: &mut u64,
) -> (wgpu::Buffer, wgpu::BindGroup, Vec<wgpu::BindGroup>) {
    let control_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mirante4d-target-control"),
        size: control_capacity,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let render_bind_group = create_presentation_render_bind_group(
        device,
        render_layout,
        payload_segments,
        directory_buffer,
        page_record_buffer,
        &control_buffer,
        bind_group_creations,
    );
    let bind_groups = create_presentation_pick_bind_groups(
        device,
        pick,
        payload_segments,
        directory_buffer,
        page_record_buffer,
        &control_buffer,
        bind_group_creations,
    );
    (control_buffer, render_bind_group, bind_groups)
}

fn collect_pending_residency_evictions(
    recently_evicted: &BTreeMap<BrickKey, u64>,
    recently_evicted_order: &VecDeque<(u64, BrickKey)>,
    maximum: usize,
) -> Box<[GpuResidencyEvictionEvent]> {
    let mut pending = Vec::with_capacity(maximum.min(recently_evicted.len()));
    for (sequence, key) in recently_evicted_order {
        if pending.len() == maximum {
            break;
        }
        if recently_evicted.get(key) == Some(sequence) {
            pending.push(GpuResidencyEvictionEvent::new(*key, *sequence));
        }
    }
    pending.into_boxed_slice()
}

fn acknowledge_residency_evictions_through(
    recently_evicted: &mut BTreeMap<BrickKey, u64>,
    recently_evicted_order: &mut VecDeque<(u64, BrickKey)>,
    through_sequence: u64,
) {
    while recently_evicted_order
        .front()
        .is_some_and(|(sequence, _)| *sequence <= through_sequence)
    {
        let (sequence, key) = recently_evicted_order
            .pop_front()
            .expect("the eviction order was checked as nonempty");
        if recently_evicted.get(&key) == Some(&sequence) {
            recently_evicted.remove(&key);
        }
    }
}

fn preflight_residency_eviction_events(
    recently_evicted: &BTreeMap<BrickKey, u64>,
    planned: &BTreeSet<BrickKey>,
    admitted: &BTreeSet<BrickKey>,
    maximum: usize,
) -> Result<(), WgpuRenderRuntimeError> {
    let retained = recently_evicted
        .keys()
        .filter(|key| !admitted.contains(key))
        .count();
    let additional = planned
        .iter()
        .filter(|key| !admitted.contains(key) && !recently_evicted.contains_key(key))
        .count();
    let actual = retained.checked_add(additional).ok_or(
        WgpuRenderRuntimeError::ResidencyEvictionEventCapacityExceeded {
            actual: usize::MAX,
            maximum,
        },
    )?;
    if actual > maximum {
        return Err(
            WgpuRenderRuntimeError::ResidencyEvictionEventCapacityExceeded { actual, maximum },
        );
    }
    Ok(())
}

fn compact_residency_eviction_order(
    recently_evicted: &BTreeMap<BrickKey, u64>,
    recently_evicted_order: &mut VecDeque<(u64, BrickKey)>,
) {
    recently_evicted_order.retain(|(sequence, key)| recently_evicted.get(key) == Some(sequence));
    debug_assert_eq!(recently_evicted.len(), recently_evicted_order.len());
}

fn record_residency_eviction(
    recently_evicted: &mut BTreeMap<BrickKey, u64>,
    recently_evicted_order: &mut VecDeque<(u64, BrickKey)>,
    next_eviction_sequence: &mut u64,
    key: BrickKey,
) {
    let sequence = *next_eviction_sequence;
    *next_eviction_sequence = next_eviction_sequence.saturating_add(1);
    recently_evicted.insert(key, sequence);
    recently_evicted_order.push_back((sequence, key));
    debug_assert!(recently_evicted.len() <= MAX_RENDER_REQUIREMENTS);
    if recently_evicted_order.len() > MAX_RENDER_REQUIREMENTS * 2 {
        compact_residency_eviction_order(recently_evicted, recently_evicted_order);
    }
}

/// Sole owner of renderer-global GPU residency and the frame leases that pin
/// it. Presentation state can retain only an opaque [`ResidentFrameLease`];
/// it cannot enumerate or mutate pins, slots, allocation state, or transfer
/// work.
struct ResidentFrameLeaseRecord {
    requirements: RenderRequirements,
    relevant_pending: Option<VecDeque<BrickKey>>,
    cohort: ResidentBodyCohort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ResidentBodyCohort(u64);

struct ResidentBodyPinCohort {
    requirements: RenderRequirements,
    lease_count: u32,
    latest_used_frame: u64,
}

struct ReleasedBodyPinCohort {
    keys: Vec<(BrickKey, bool)>,
    latest_used_frame: u64,
}

#[derive(Default)]
struct FrameLeaseMutation {
    pin_state_changes: Vec<BrickKey>,
    released_body: Option<ReleasedBodyPinCohort>,
}

struct ResidentFrameLeases {
    by_id: BTreeMap<ResidentFrameLease, ResidentFrameLeaseRecord>,
    cohorts: BTreeMap<ResidentBodyCohort, ResidentBodyPinCohort>,
    pin_counts: BTreeMap<BrickKey, u32>,
    next: u64,
    next_cohort: u64,
}

impl ResidentFrameLeases {
    fn new() -> Self {
        Self {
            by_id: BTreeMap::new(),
            cohorts: BTreeMap::new(),
            pin_counts: BTreeMap::new(),
            next: 1,
            next_cohort: 1,
        }
    }

    fn requirements(&self, lease: ResidentFrameLease) -> &RenderRequirements {
        &self
            .by_id
            .get(&lease)
            .expect("every presentation frame lease belongs to the residency owner")
            .requirements
    }

    fn owned_requirements(&self, lease: ResidentFrameLease) -> RenderRequirements {
        self.requirements(lease).clone()
    }

    fn relevant_pending(&self, lease: ResidentFrameLease) -> Option<&VecDeque<BrickKey>> {
        self.by_id
            .get(&lease)
            .expect("every presentation frame lease belongs to the residency owner")
            .relevant_pending
            .as_ref()
    }

    fn has_relevant_pending(&self, lease: ResidentFrameLease) -> bool {
        self.relevant_pending(lease)
            .is_some_and(|pending| !pending.is_empty())
    }

    fn append_offers(&mut self, admitted: &[BrickKey]) {
        if admitted.is_empty() {
            return;
        }
        for record in self.by_id.values_mut() {
            if let Some(relevant_pending) = record.relevant_pending.as_mut() {
                relevant_pending.extend(
                    admitted
                        .iter()
                        .copied()
                        .filter(|key| record.requirements.contains_resource(*key)),
                );
            }
        }
    }

    fn remove_offers(&mut self, removed: &BTreeSet<BrickKey>) {
        if removed.is_empty() {
            return;
        }
        for record in self.by_id.values_mut() {
            if let Some(relevant_pending) = record.relevant_pending.as_mut() {
                relevant_pending.retain(|key| !removed.contains(key));
            }
        }
    }

    fn allocate(&mut self) -> ResidentFrameLease {
        loop {
            let lease = ResidentFrameLease(self.next);
            self.next = self.next.wrapping_add(1);
            if !self.by_id.contains_key(&lease) {
                return lease;
            }
        }
    }

    fn allocate_cohort(&mut self) -> ResidentBodyCohort {
        loop {
            let cohort = ResidentBodyCohort(self.next_cohort);
            self.next_cohort = self.next_cohort.wrapping_add(1);
            if !self.cohorts.contains_key(&cohort) {
                return cohort;
            }
        }
    }

    fn body_cohort(&self, requirements: &RenderRequirements) -> Option<ResidentBodyCohort> {
        self.cohorts.iter().find_map(|(cohort, record)| {
            record
                .requirements
                .shares_resources_with(requirements)
                .then_some(*cohort)
        })
    }

    fn body_is_known(&self, requirements: &RenderRequirements) -> bool {
        self.body_cohort(requirements).is_some()
    }

    fn increment_pin(&mut self, key: BrickKey) -> bool {
        let count = self.pin_counts.entry(key).or_default();
        let became_pinned = *count == 0;
        *count = count
            .checked_add(1)
            .expect("the bounded presentation count cannot overflow a pin count");
        became_pinned
    }

    fn decrement_pin(&mut self, key: BrickKey) -> bool {
        let count = self
            .pin_counts
            .get_mut(&key)
            .expect("every released frame key has a matching pin count");
        if *count == 1 {
            self.pin_counts.remove(&key);
            true
        } else {
            *count -= 1;
            false
        }
    }

    fn retain_body(
        &mut self,
        requirements: &RenderRequirements,
    ) -> (ResidentBodyCohort, Vec<BrickKey>) {
        if let Some(cohort) = self.body_cohort(requirements) {
            let record = self
                .cohorts
                .get_mut(&cohort)
                .expect("a found body cohort remains present");
            record.lease_count = record
                .lease_count
                .checked_add(1)
                .expect("the bounded coordinated cutoff count fits u32");
            return (cohort, Vec::new());
        }
        let cohort = self.allocate_cohort();
        let changed = requirements
            .resource_keys()
            .iter()
            .copied()
            .filter(|key| self.increment_pin(*key))
            .collect();
        let previous = self.cohorts.insert(
            cohort,
            ResidentBodyPinCohort {
                requirements: requirements.clone(),
                lease_count: 1,
                latest_used_frame: requirements.frame().get(),
            },
        );
        debug_assert!(previous.is_none());
        (cohort, changed)
    }

    fn release_body(&mut self, cohort: ResidentBodyCohort) -> Option<ReleasedBodyPinCohort> {
        let record = self
            .cohorts
            .get_mut(&cohort)
            .expect("every live lease refers to one body cohort");
        if record.lease_count > 1 {
            record.lease_count -= 1;
            return None;
        }
        let record = self
            .cohorts
            .remove(&cohort)
            .expect("the final body lease removes its cohort");
        let keys = record
            .requirements
            .resource_keys()
            .iter()
            .copied()
            .map(|key| {
                let became_unpinned = self.decrement_pin(key);
                (key, became_unpinned)
            })
            .collect();
        Some(ReleasedBodyPinCohort {
            keys,
            latest_used_frame: record.latest_used_frame,
        })
    }

    fn acquire(
        &mut self,
        requirements: RenderRequirements,
        pending: &PendingLeaseQueue,
    ) -> (ResidentFrameLease, FrameLeaseMutation) {
        let lease = self.allocate();
        let relevant_pending = pending.relevant_order(&requirements);
        let (cohort, pin_state_changes) = self.retain_body(&requirements);
        let previous = self.by_id.insert(
            lease,
            ResidentFrameLeaseRecord {
                requirements,
                relevant_pending: Some(relevant_pending),
                cohort,
            },
        );
        debug_assert!(previous.is_none());
        (
            lease,
            FrameLeaseMutation {
                pin_state_changes,
                released_body: None,
            },
        )
    }

    fn clone_for_completion(&mut self, source: ResidentFrameLease) -> ResidentFrameLease {
        let (requirements, cohort) = {
            let source = self
                .by_id
                .get(&source)
                .expect("a completion snapshot clones one live frame lease");
            (source.requirements.clone(), source.cohort)
        };
        let cohort_record = self
            .cohorts
            .get_mut(&cohort)
            .expect("a live frame lease retains its body cohort");
        cohort_record.lease_count = cohort_record
            .lease_count
            .checked_add(1)
            .expect("the bounded coordinated cutoff count fits u32");
        let lease = self.allocate();
        let previous = self.by_id.insert(
            lease,
            ResidentFrameLeaseRecord {
                requirements,
                relevant_pending: None,
                cohort,
            },
        );
        debug_assert!(previous.is_none());
        lease
    }

    /// Creates a presentation-owned lease for a same-body resident rebind.
    ///
    /// The caller proves that the source has no relevant pending offers. The
    /// new relevance index can therefore start empty and future global offers
    /// will update it normally, avoiding a requirement-body or inbox walk.
    fn clone_for_presentation_rebind(
        &mut self,
        source: ResidentFrameLease,
        requirements: RenderRequirements,
    ) -> ResidentFrameLease {
        let (source_requirements, cohort, source_pending_is_empty) = {
            let source = self
                .by_id
                .get(&source)
                .expect("a resident presentation rebind clones one live frame lease");
            (
                &source.requirements,
                source.cohort,
                source
                    .relevant_pending
                    .as_ref()
                    .is_some_and(VecDeque::is_empty),
            )
        };
        assert!(
            source_requirements.shares_resources_with(&requirements)
                && source_requirements.prefetch_promoted() == requirements.prefetch_promoted(),
            "a resident presentation rebind must preserve the exact requirement body"
        );
        assert!(
            source_pending_is_empty,
            "a resident presentation rebind cannot skip relevant pending offers"
        );
        let cohort_record = self
            .cohorts
            .get_mut(&cohort)
            .expect("a live frame lease retains its body cohort");
        cohort_record.lease_count = cohort_record
            .lease_count
            .checked_add(1)
            .expect("the bounded coordinated cutoff count fits u32");
        let lease = self.allocate();
        let previous = self.by_id.insert(
            lease,
            ResidentFrameLeaseRecord {
                requirements,
                relevant_pending: Some(VecDeque::new()),
                cohort,
            },
        );
        debug_assert!(previous.is_none());
        lease
    }

    fn replace(
        &mut self,
        current: ResidentFrameLease,
        requirements: RenderRequirements,
        pending: &PendingLeaseQueue,
    ) -> FrameLeaseMutation {
        let previous = self.owned_requirements(current);
        if previous.shares_resources_with(&requirements) {
            self.by_id
                .get_mut(&current)
                .expect("a replaced frame lease belongs to the residency owner")
                .requirements = requirements;
            return FrameLeaseMutation::default();
        }

        let relevant_pending = pending.relevant_order(&requirements);
        let old_cohort = self
            .by_id
            .get(&current)
            .expect("a replaced frame lease belongs to the residency owner")
            .cohort;
        // Retain the replacement before releasing its predecessor. Overlap
        // keys therefore remain continuously pinned and do not manufacture a
        // false unpin/repin transition in the payload owner.
        let (cohort, mut pin_state_changes) = self.retain_body(&requirements);
        let released_body = self.release_body(old_cohort);
        if let Some(released) = released_body.as_ref() {
            pin_state_changes.extend(
                released
                    .keys
                    .iter()
                    .filter_map(|(key, changed)| changed.then_some(*key)),
            );
        }
        pin_state_changes.sort_unstable();
        debug_assert!(
            pin_state_changes.windows(2).all(|pair| pair[0] != pair[1]),
            "retain-before-release produces one exact transition per key"
        );
        self.by_id.insert(
            current,
            ResidentFrameLeaseRecord {
                requirements,
                relevant_pending: Some(relevant_pending),
                cohort,
            },
        );
        FrameLeaseMutation {
            pin_state_changes,
            released_body,
        }
    }

    fn release(&mut self, lease: ResidentFrameLease) -> FrameLeaseMutation {
        let record = self
            .by_id
            .remove(&lease)
            .expect("a live presentation releases its residency lease exactly once");
        let released_body = self.release_body(record.cohort);
        let pin_state_changes = released_body
            .as_ref()
            .into_iter()
            .flat_map(|released| &released.keys)
            .filter_map(|(key, changed)| changed.then_some(*key))
            .collect();
        FrameLeaseMutation {
            pin_state_changes,
            released_body,
        }
    }

    fn touch(&mut self, lease: ResidentFrameLease, frame: FrameIdentity) {
        let cohort = self
            .by_id
            .get(&lease)
            .expect("a touched frame lease belongs to the residency owner")
            .cohort;
        let body = self
            .cohorts
            .get_mut(&cohort)
            .expect("a touched frame lease retains its body cohort");
        body.latest_used_frame = body.latest_used_frame.max(frame.get());
    }

    fn is_pinned(&self, key: BrickKey) -> bool {
        self.pin_counts.contains_key(&key)
    }

    fn is_pinned_excluding(&self, key: BrickKey, lease: ResidentFrameLease) -> bool {
        let record = self
            .by_id
            .get(&lease)
            .expect("an excluded frame lease belongs to the residency owner");
        let current_owns_only_body_pin = self
            .cohorts
            .get(&record.cohort)
            .is_some_and(|cohort| cohort.lease_count == 1)
            && record.requirements.contains_resource(key);
        self.pin_counts
            .get(&key)
            .copied()
            .unwrap_or(0)
            .saturating_sub(u32::from(current_owns_only_body_pin))
            != 0
    }

    fn is_empty(&self) -> bool {
        self.by_id.is_empty() && self.cohorts.is_empty() && self.pin_counts.is_empty()
    }
}

struct ResidencyOwner {
    payload_capacity_bytes: u64,
    payload_segments: Vec<PayloadSegment>,
    compaction_scratch: Option<wgpu::Buffer>,
    compaction_scratch_capacity: u64,
    directory_buffer: wgpu::Buffer,
    page_record_buffer: wgpu::Buffer,
    upload_staging: UploadStagingPool,
    resource_grids: Option<RenderResourceGridCatalog>,
    global_residency: GlobalResidencyDirectory,
    resident: BTreeMap<BrickKey, ResidentResource>,
    resident_payload_bytes: u64,
    payload_resident_lru: [BTreeSet<(u64, BrickKey)>; MAX_PAYLOAD_SEGMENTS],
    payload_evictable_lru: [BTreeSet<(u64, BrickKey)>; MAX_PAYLOAD_SEGMENTS],
    empty_resident_count: usize,
    empty_resident_lru: BTreeSet<(u64, BrickKey)>,
    recently_evicted: BTreeMap<BrickKey, u64>,
    recently_evicted_order: VecDeque<(u64, BrickKey)>,
    next_eviction_sequence: u64,
    pending_leases: PendingLeaseQueue,
    frame_leases: ResidentFrameLeases,
    retired_frame_leases: BTreeMap<u64, ResidentFrameLeases>,
}

struct ResidencyChangeSet {
    changes: BTreeMap<BrickKey, bool>,
}

struct ResidencyBindings<'a> {
    payload_segments: &'a [PayloadSegment],
    directory_buffer: &'a wgpu::Buffer,
    page_record_buffer: &'a wgpu::Buffer,
}

struct ResidencyTransferPlanning<'a> {
    payload_segments: &'a mut [PayloadSegment],
    resident: &'a BTreeMap<BrickKey, ResidentResource>,
    payload_evictable_lru: &'a [BTreeSet<(u64, BrickKey)>; MAX_PAYLOAD_SEGMENTS],
    resource_grids: &'a RenderResourceGridCatalog,
}

impl ResidencyTransferPlanning<'_> {
    fn next_payload_eviction_candidate(
        &self,
        segment: usize,
        after: Option<(u64, BrickKey)>,
        replacement_releases: &BTreeSet<(u64, BrickKey)>,
        planned: &RenderRequirements,
    ) -> Option<(u64, BrickKey, u64, u64)> {
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
}

struct ResidencyCommitOutcome {
    changes: ResidencyChangeSet,
    evicted_resources: u64,
    reuploaded_resources: u64,
    resident_payload_bytes: u64,
}

impl ResidencyOwner {
    fn new(
        payload_segments: Vec<PayloadSegment>,
        compaction_scratch: Option<wgpu::Buffer>,
        compaction_scratch_capacity: u64,
        directory_buffer: wgpu::Buffer,
        page_record_buffer: wgpu::Buffer,
        upload_staging: UploadStagingPool,
        global_residency: GlobalResidencyDirectory,
    ) -> Self {
        let payload_capacity_bytes = payload_segments
            .iter()
            .map(|segment| segment.logical_capacity)
            .sum();
        Self {
            payload_capacity_bytes,
            payload_segments,
            compaction_scratch,
            compaction_scratch_capacity,
            directory_buffer,
            page_record_buffer,
            upload_staging,
            resource_grids: None,
            global_residency,
            resident: BTreeMap::new(),
            resident_payload_bytes: 0,
            payload_resident_lru: std::array::from_fn(|_| BTreeSet::new()),
            payload_evictable_lru: std::array::from_fn(|_| BTreeSet::new()),
            empty_resident_count: 0,
            empty_resident_lru: BTreeSet::new(),
            recently_evicted: BTreeMap::new(),
            recently_evicted_order: VecDeque::new(),
            next_eviction_sequence: 1,
            pending_leases: PendingLeaseQueue::new(),
            frame_leases: ResidentFrameLeases::new(),
            retired_frame_leases: BTreeMap::new(),
        }
    }

    fn requirements(&self, lease: ResidentFrameLease) -> &RenderRequirements {
        self.frame_leases.requirements(lease)
    }

    fn bindings(&self) -> ResidencyBindings<'_> {
        ResidencyBindings {
            payload_segments: &self.payload_segments,
            directory_buffer: &self.directory_buffer,
            page_record_buffer: &self.page_record_buffer,
        }
    }

    fn resource_grids(&self) -> Result<&RenderResourceGridCatalog, WgpuRenderRuntimeError> {
        self.resource_grids
            .as_ref()
            .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)
    }

    fn catalog_is_active(&self, catalog: &DatasetCatalog) -> bool {
        self.resource_grids
            .as_ref()
            .is_some_and(|grids| grids.resource_identity() == catalog.resource_identity())
    }

    fn begin_transfer_plan(
        &mut self,
    ) -> Result<ResidencyTransferPlanning<'_>, WgpuRenderRuntimeError> {
        let resource_grids = self
            .resource_grids
            .as_ref()
            .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
        Ok(ResidencyTransferPlanning {
            payload_segments: &mut self.payload_segments,
            resident: &self.resident,
            payload_evictable_lru: &self.payload_evictable_lru,
            resource_grids,
        })
    }

    fn resource_is_resident(&self, key: BrickKey) -> bool {
        self.resident.contains_key(&key)
    }

    const fn payload_capacity_bytes(&self) -> u64 {
        self.payload_capacity_bytes
    }

    const fn resident_payload_bytes(&self) -> u64 {
        self.resident_payload_bytes
    }

    fn payload_free_bytes(&self) -> u64 {
        self.payload_segments
            .iter()
            .map(|segment| segment.allocator.available_bytes())
            .sum()
    }

    fn payload_committed_capacity_bytes(&self) -> u64 {
        self.payload_segments
            .iter()
            .map(|segment| segment.capacity)
            .sum()
    }

    fn payload_allocated_bytes(&self) -> u64 {
        self.payload_segments
            .iter()
            .map(|segment| segment.allocated_bytes)
            .sum()
    }

    fn payload_segment_committed_capacity_bytes(&self) -> [u64; MAX_PAYLOAD_SEGMENTS] {
        let mut committed = [0; MAX_PAYLOAD_SEGMENTS];
        for (destination, segment) in committed.iter_mut().zip(&self.payload_segments) {
            *destination = segment.capacity;
        }
        committed
    }

    fn payload_largest_contiguous_bytes(&self) -> u64 {
        self.payload_segments
            .iter()
            .map(|segment| segment.allocator.largest_contiguous_bytes())
            .max()
            .unwrap_or(0)
    }

    fn payload_segment_free_bytes(&self) -> [u64; MAX_PAYLOAD_SEGMENTS] {
        let mut free = [0; MAX_PAYLOAD_SEGMENTS];
        for (destination, segment) in free.iter_mut().zip(&self.payload_segments) {
            *destination = segment.allocator.available_bytes();
        }
        free
    }

    fn payload_segment_largest_contiguous_bytes(&self) -> [u64; MAX_PAYLOAD_SEGMENTS] {
        let mut largest = [0; MAX_PAYLOAD_SEGMENTS];
        for (destination, segment) in largest.iter_mut().zip(&self.payload_segments) {
            *destination = segment.allocator.largest_contiguous_bytes();
        }
        largest
    }

    /// Prepares a stable within-segment packing. Source order is preserved, so
    /// every destination ends before the next not-yet-copied source. One
    /// bounded scratch buffer is therefore sufficient even when a move
    /// overlaps its own old range.
    fn prepare_payload_compaction(&self) -> PayloadCompactionPlan {
        let mut relocations = Vec::new();
        let mut allocators = Vec::with_capacity(self.payload_segments.len());
        for segment in 0..self.payload_segments.len() {
            let resources = self
                .resident
                .iter()
                .filter(|(_, resource)| {
                    resource.allocated_bytes != 0 && resource.segment as usize == segment
                })
                .map(|(key, resource)| {
                    (
                        *key,
                        resource.offset,
                        resource.allocated_bytes,
                        resource.validity_offset,
                    )
                })
                .collect::<Vec<_>>();
            let (segment_relocations, allocator) = prepare_segment_payload_compaction(
                segment,
                self.payload_segments[segment].capacity,
                resources,
            );
            relocations.extend(segment_relocations);
            allocators.push(allocator);
        }
        PayloadCompactionPlan {
            relocations,
            allocators,
        }
    }

    fn commit_payload_compaction(&mut self, plan: PayloadCompactionPlan) {
        for relocation in &plan.relocations {
            let resource = self
                .resident
                .get_mut(&relocation.key)
                .expect("a compacted resource remains resident until commit");
            debug_assert_eq!(resource.segment as usize, relocation.segment);
            debug_assert_eq!(resource.offset, relocation.source_offset);
            resource.offset = relocation.destination_offset;
            resource.validity_offset = relocation.destination_validity_offset;
        }
        for (segment, allocator) in self.payload_segments.iter_mut().zip(plan.allocators) {
            segment.allocator = allocator;
        }
    }

    /// Proves that one exact aggregate requirement union can occupy the
    /// renderer-owned segmented arena without publishing slots, evictions, or
    /// allocator state. The caller must include every published front; hidden
    /// or completed predecessors outside the proposed steady-state union are
    /// treated as replaceable. Every tentative release/allocation is undone
    /// before this method returns, including fragmentation failures.
    fn preflight_global_requirement_union(
        &mut self,
        catalog: &DatasetCatalog,
        requirements: &[BrickKey],
    ) -> Result<Vec<u64>, WgpuRenderRuntimeError> {
        if !self.catalog_is_active(catalog) {
            return Err(WgpuRenderRuntimeError::PayloadContractMismatch);
        }
        if requirements.len() > MAX_GLOBAL_RESIDENT_PAGES {
            return Err(WgpuRenderRuntimeError::ResidentMetadataCapacityExceeded {
                requested_records: requirements.len(),
                maximum_records: MAX_GLOBAL_RESIDENT_PAGES,
                requested_bytes: u64::try_from(requirements.len())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(PAGE_RECORD_BYTES),
                available_bytes: GLOBAL_PAGE_RECORD_BYTES,
            });
        }
        if !requirements.is_sorted() || requirements.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(WgpuRenderRuntimeError::RequirementSetChanged);
        }

        let required = requirements.iter().copied().collect::<BTreeSet<_>>();
        let mut missing = requirements
            .iter()
            .copied()
            .filter(|key| !self.resident.contains_key(key))
            .map(|key| {
                let descriptor = catalog
                    .resource_payload_descriptor(key)
                    .map_err(|_| WgpuRenderRuntimeError::PayloadContractMismatch)?;
                Ok((key, payload_allocation_bytes(descriptor)?))
            })
            .collect::<Result<Vec<_>, WgpuRenderRuntimeError>>()?;
        // Largest-first makes the proof deterministic and avoids manufacturing
        // fragmentation from the traversal order of otherwise equivalent
        // catalog keys.
        missing.sort_by_key(|(key, bytes)| (std::cmp::Reverse(*bytes), *key));

        let releasable = self
            .resident
            .iter()
            .filter(|(key, resident)| resident.allocated_bytes != 0 && !required.contains(key))
            .map(|(key, resident)| {
                (
                    *key,
                    resident.segment as usize,
                    resident.offset,
                    resident.allocated_bytes,
                )
            })
            .collect::<Vec<_>>();
        let mut operations = Vec::with_capacity(releasable.len().saturating_add(missing.len()));
        let segment_order =
            payload_segment_allocation_order(self.payload_segments.len(), &self.resident);
        let mut required_ends = vec![0_u64; self.payload_segments.len()];
        for key in requirements {
            if let Some(resident) = self
                .resident
                .get(key)
                .filter(|resident| resident.allocated_bytes != 0)
            {
                let segment = resident.segment as usize;
                required_ends[segment] = required_ends[segment]
                    .max(resident.offset.saturating_add(resident.allocated_bytes));
            }
        }
        // Feasibility is governed by the configured logical maximum, not by
        // how many backing bytes happen to be committed at this instant.
        // Temporarily expose each uncommitted tail to the same exact allocator
        // proof and remove it during the existing bounded rollback.
        for (segment, arena) in self.payload_segments.iter_mut().enumerate() {
            if arena.capacity < arena.logical_capacity {
                arena.allocator.grow(arena.capacity, arena.logical_capacity);
                operations.push(ArenaOperation::SimulatedGrowth {
                    segment,
                    previous_capacity: arena.capacity,
                    new_capacity: arena.logical_capacity,
                });
            }
        }
        for (_, segment, offset, bytes) in &releasable {
            self.payload_segments[*segment]
                .allocator
                .release(*offset, *bytes);
            operations.push(ArenaOperation::Release {
                segment: *segment,
                offset: *offset,
                bytes: *bytes,
            });
        }

        for (_, bytes) in missing {
            if let Err(error) = validate_single_payload_segment_fit(
                bytes,
                self.payload_segments
                    .iter()
                    .map(|segment| segment.logical_capacity),
            ) {
                rollback_arena_operations(&mut self.payload_segments, &operations);
                return Err(error);
            }
            let allocation = segment_order.iter().copied().find_map(|segment| {
                self.payload_segments[segment]
                    .allocator
                    .allocate(bytes)
                    .map(|offset| (segment, offset))
            });
            let Some((segment, offset)) = allocation else {
                let error = payload_allocation_failure(&self.payload_segments, bytes);
                rollback_arena_operations(&mut self.payload_segments, &operations);
                return Err(error);
            };
            operations.push(ArenaOperation::Allocate {
                segment,
                offset,
                bytes,
            });
            required_ends[segment] = required_ends[segment].max(offset.saturating_add(bytes));
        }
        rollback_arena_operations(&mut self.payload_segments, &operations);
        self.payload_segments
            .iter()
            .zip(required_ends)
            .map(|(segment, required_end)| {
                bounded_geometric_payload_commitment(
                    segment.capacity,
                    segment.logical_capacity,
                    required_end,
                )
            })
            .collect()
    }

    fn resident_resource(&self, key: BrickKey) -> Option<&ResidentResource> {
        self.resident.get(&key)
    }

    fn initial_available_keys(&self, requirements: &RenderRequirements) -> (Vec<BrickKey>, u64) {
        if requirements.resources().len() <= self.resident.len() {
            let available = requirements
                .resources()
                .map(|requirement| requirement.key())
                .filter(|key| self.resident.contains_key(key))
                .collect();
            (available, requirements.resources().len() as u64)
        } else {
            let available = self
                .resident
                .keys()
                .copied()
                .filter(|key| requirements.contains_resource(*key))
                .collect();
            (available, self.resident.len() as u64)
        }
    }

    fn resident_hit_count(&self, visited: &[BrickKey], uploaded: &BTreeSet<BrickKey>) -> u64 {
        visited
            .iter()
            .filter(|key| self.resident.contains_key(key) && !uploaded.contains(key))
            .count() as u64
    }

    fn empty_resident_count(&self) -> usize {
        self.empty_resident_count
    }

    fn pending_evictions(&self, maximum: usize) -> Box<[GpuResidencyEvictionEvent]> {
        collect_pending_residency_evictions(
            &self.recently_evicted,
            &self.recently_evicted_order,
            maximum,
        )
    }

    fn has_pending_evictions(&self) -> bool {
        !self.recently_evicted.is_empty()
    }

    fn acknowledge_evictions(&mut self, through_sequence: u64) {
        acknowledge_residency_evictions_through(
            &mut self.recently_evicted,
            &mut self.recently_evicted_order,
            through_sequence,
        );
    }

    fn offer_leases(
        &mut self,
        leases: &[Arc<dyn ResourceLease>],
    ) -> Result<Vec<BrickKey>, WgpuRenderRuntimeError> {
        if leases.is_empty() {
            return Ok(Vec::new());
        }
        let grids = self
            .resource_grids
            .as_ref()
            .ok_or(WgpuRenderRuntimeError::PayloadContractMismatch)?;
        for lease in leases {
            grids
                .validate_key(lease.key())
                .map_err(|_| WgpuRenderRuntimeError::PayloadContractMismatch)?;
        }
        let admitted = self.pending_leases.offer(leases, &self.resident)?;
        self.frame_leases.append_offers(&admitted);
        Ok(admitted)
    }

    fn retire_offers(&mut self, retired: &BTreeSet<BrickKey>) {
        self.pending_leases.remove_batch(retired);
        self.frame_leases.remove_offers(retired);
    }

    fn has_relevant_offer(&self, lease: ResidentFrameLease) -> bool {
        self.frame_leases.has_relevant_pending(lease)
    }

    fn select_offers_for_execution(
        &self,
        current: Option<ResidentFrameLease>,
        requirements: &RenderRequirements,
    ) -> Result<PendingLeaseSelection, WgpuRenderRuntimeError> {
        if let Some(current) = current
            && self
                .frame_leases
                .requirements(current)
                .shares_resources_with(requirements)
        {
            return self.pending_leases.select_indexed_for_execution(
                &self.resident,
                self.frame_leases
                    .relevant_pending(current)
                    .expect("a presentation lease owns a relevant-offer index"),
            );
        }
        self.pending_leases
            .select_for_execution(&self.resident, |key| requirements.contains_resource(key))
    }

    fn complete_resident_offers(
        &mut self,
        selected: impl IntoIterator<Item = BrickKey>,
    ) -> BTreeSet<BrickKey> {
        let completed = selected
            .into_iter()
            .filter(|key| self.resident.contains_key(key))
            .collect::<BTreeSet<_>>();
        self.pending_leases.remove_batch(&completed);
        self.frame_leases.remove_offers(&completed);
        completed
    }

    fn can_activate_dataset(&self) -> bool {
        self.resident.is_empty() && self.pending_leases.len() == 0 && self.frame_leases.is_empty()
    }

    fn activate_dataset(
        &mut self,
        catalog: &DatasetCatalog,
        grids: &RenderResourceGridCatalog,
    ) -> Result<(), WgpuRenderRuntimeError> {
        if !self.can_activate_dataset() {
            return Err(WgpuRenderRuntimeError::RequirementSetChanged);
        }
        grids
            .validate_catalog(catalog)
            .map_err(|_| WgpuRenderRuntimeError::InvalidResourceGridCatalog)?;
        self.resource_grids = Some(grids.clone());
        Ok(())
    }

    fn owned_requirements(&self, lease: ResidentFrameLease) -> RenderRequirements {
        self.frame_leases.owned_requirements(lease)
    }

    fn body_is_known(&self, requirements: &RenderRequirements) -> bool {
        self.frame_leases.body_is_known(requirements)
    }

    fn acquire_frame_lease(&mut self, requirements: RenderRequirements) -> ResidentFrameLease {
        let (lease, mutation) = self
            .frame_leases
            .acquire(requirements, &self.pending_leases);
        self.apply_frame_lease_mutation(mutation);
        lease
    }

    fn clone_frame_lease_for_completion(
        &mut self,
        source: ResidentFrameLease,
    ) -> ResidentFrameLease {
        self.frame_leases.clone_for_completion(source)
    }

    fn clone_frame_lease_for_presentation_rebind(
        &mut self,
        source: ResidentFrameLease,
        requirements: RenderRequirements,
    ) -> ResidentFrameLease {
        self.frame_leases
            .clone_for_presentation_rebind(source, requirements)
    }

    /// Rebinds one committed frame lease without exposing a transient
    /// unpinned state. Same-body rebinds update the shared requirements record
    /// with unchanged pin counts; body replacement applies only the exact
    /// key-set delta.
    fn replace_frame_lease(
        &mut self,
        current: ResidentFrameLease,
        requirements: RenderRequirements,
    ) -> ResidentFrameLease {
        let mutation = self
            .frame_leases
            .replace(current, requirements, &self.pending_leases);
        self.apply_frame_lease_mutation(mutation);
        current
    }

    fn commit_frame_lease(
        &mut self,
        current: Option<ResidentFrameLease>,
        requirements: RenderRequirements,
    ) -> ResidentFrameLease {
        match current {
            Some(current) => self.replace_frame_lease(current, requirements),
            None => self.acquire_frame_lease(requirements),
        }
    }

    fn release_frame_lease(&mut self, lease: ResidentFrameLease) {
        let mutation = self.frame_leases.release(lease);
        self.apply_frame_lease_mutation(mutation);
    }

    fn apply_frame_lease_mutation(&mut self, mutation: FrameLeaseMutation) {
        if let Some(released) = mutation.released_body {
            for (key, _) in released.keys {
                self.touch_payload_key(key, FrameIdentity::new(released.latest_used_frame));
            }
        }
        for key in mutation.pin_state_changes {
            self.refresh_payload_pin_state(key);
        }
    }

    fn touch_frame_lease(&mut self, lease: ResidentFrameLease, frame: FrameIdentity) {
        self.frame_leases.touch(lease, frame);
    }

    fn detach_frame_leases_for_generation(&mut self, generation: u64) {
        let detached = std::mem::replace(&mut self.frame_leases, ResidentFrameLeases::new());
        if detached.is_empty() {
            return;
        }
        let previous = self.retired_frame_leases.insert(generation, detached);
        debug_assert!(
            previous.is_none(),
            "one residency generation is detached at most once"
        );
    }

    fn release_completed_frame_leases(
        &mut self,
        generation: u64,
        current_generation: u64,
        leases: Vec<ResidentFrameLease>,
    ) {
        if generation == current_generation {
            for lease in leases {
                self.release_frame_lease(lease);
            }
            return;
        }
        let retired = self
            .retired_frame_leases
            .get_mut(&generation)
            .expect("an old in-flight cutoff retains its detached lease owner");
        for lease in leases {
            let _ = retired.release(lease);
        }
        if retired.is_empty() {
            self.retired_frame_leases.remove(&generation);
        }
    }

    fn is_pinned(&self, key: BrickKey) -> bool {
        self.frame_leases.is_pinned(key)
    }

    fn is_pinned_excluding(&self, key: BrickKey, lease: ResidentFrameLease) -> bool {
        self.frame_leases.is_pinned_excluding(key, lease)
    }

    fn refresh_payload_pin_state(&mut self, key: BrickKey) {
        let Some(resource) = self.resident.get(&key) else {
            return;
        };
        if resource.allocated_bytes == 0 {
            return;
        }
        let segment = resource.segment as usize;
        let entry = (resource.last_used_frame, key);
        if self.is_pinned(key) {
            self.payload_evictable_lru[segment].remove(&entry);
        } else {
            self.payload_evictable_lru[segment].insert(entry);
        }
    }

    fn replacement_release_candidates(
        &self,
        current: Option<ResidentFrameLease>,
        planned: &RenderRequirements,
    ) -> [BTreeSet<(u64, BrickKey)>; MAX_PAYLOAD_SEGMENTS] {
        let mut candidates = std::array::from_fn(|_| BTreeSet::new());
        let Some(current) = current else {
            return candidates;
        };
        let previous = self.requirements(current);
        if previous.shares_resources_with(planned) {
            return candidates;
        }
        for key in previous
            .resources()
            .map(|requirement| requirement.key())
            .filter(|key| !planned.contains_resource(*key))
            .filter(|key| !self.is_pinned_excluding(*key, current))
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

    fn touch_payload_key(&mut self, key: BrickKey, frame: FrameIdentity) {
        let frame = self.resident.get(&key).map_or(frame.get(), |resource| {
            resource.last_used_frame.max(frame.get())
        });
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

    fn protected_during_replacement(
        &self,
        key: BrickKey,
        current: Option<ResidentFrameLease>,
        planned: &RenderRequirements,
    ) -> bool {
        planned.contains_resource(key)
            || current.map_or_else(
                || self.is_pinned(key),
                |current| self.is_pinned_excluding(key, current),
            )
    }

    fn plan_global_page_capacity_evictions(
        &self,
        current: Option<ResidentFrameLease>,
        planned: &RenderRequirements,
        already_evicted: &BTreeSet<BrickKey>,
        incoming_records: usize,
    ) -> Result<Vec<BrickKey>, WgpuRenderRuntimeError> {
        let retained_records =
            (self.global_residency.live_pages() as usize).saturating_sub(already_evicted.len());
        let predicted_records = retained_records.checked_add(incoming_records).ok_or(
            WgpuRenderRuntimeError::ResidentMetadataCapacityExceeded {
                requested_records: usize::MAX,
                maximum_records: MAX_GLOBAL_RESIDENT_PAGES,
                requested_bytes: u64::MAX,
                available_bytes: GLOBAL_PAGE_RECORD_BYTES,
            },
        )?;
        let excess = predicted_records.saturating_sub(MAX_GLOBAL_RESIDENT_PAGES);
        if excess == 0 {
            return Ok(Vec::new());
        }
        let candidates = self
            .resident
            .iter()
            .filter(|(key, _)| !already_evicted.contains(key))
            .map(|(key, resource)| (resource.last_used_frame, *key))
            .collect::<BTreeSet<_>>();
        let evictions = oldest_unprotected_keys(candidates, excess, |key| {
            self.protected_during_replacement(key, current, planned)
        });
        let remaining = predicted_records.saturating_sub(evictions.len());
        if remaining > MAX_GLOBAL_RESIDENT_PAGES {
            return Err(WgpuRenderRuntimeError::ResidentMetadataCapacityExceeded {
                requested_records: remaining,
                maximum_records: MAX_GLOBAL_RESIDENT_PAGES,
                requested_bytes: (remaining as u64).saturating_mul(64),
                available_bytes: GLOBAL_PAGE_RECORD_BYTES,
            });
        }
        Ok(evictions)
    }

    fn plan_empty_resident_evictions(
        &self,
        current: Option<ResidentFrameLease>,
        planned: &RenderRequirements,
        incoming_records: usize,
    ) -> Result<Vec<BrickKey>, WgpuRenderRuntimeError> {
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
                self.protected_during_replacement(key, current, planned)
            });
        let minimum_retained = predicted_records.saturating_sub(evictions.len());
        validate_empty_resident_metadata_capacity(minimum_retained)?;
        Ok(evictions)
    }

    fn touch_empty_keys(&mut self, keys: &[BrickKey], frame: FrameIdentity) {
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

    fn prepare_directory_batch(
        &self,
        removals: &[DirectoryRemoval<'_>],
        admissions: &[DirectoryAdmission<'_>],
    ) -> Result<PreparedDirectoryBatch, WgpuRenderRuntimeError> {
        self.global_residency
            .prepare_batch(removals, admissions)
            .map_err(map_global_residency_error)
    }

    fn preflight_eviction_events(
        &self,
        planned: &BTreeSet<BrickKey>,
        admitted: &BTreeSet<BrickKey>,
    ) -> Result<(), WgpuRenderRuntimeError> {
        preflight_residency_eviction_events(
            &self.recently_evicted,
            planned,
            admitted,
            MAX_RENDER_REQUIREMENTS,
        )
    }

    fn refresh_transfers(&mut self) -> Result<(), WgpuRenderRuntimeError> {
        self.upload_staging.refresh()
    }

    fn acquire_transfer_slot(
        &mut self,
        device: &wgpu::Device,
        required: u64,
    ) -> Result<Option<(usize, u64, u64)>, WgpuRenderRuntimeError> {
        Ok(self
            .upload_staging
            .acquire(device, required)?
            .map(|(slot, newly_allocated)| (slot, newly_allocated, self.upload_staging.reserved)))
    }

    fn stage_transfer(
        &mut self,
        slot: usize,
        uploads: &[UploadPlan<'_>],
        directory: Option<&PreparedDirectoryBatch>,
        removals: &[DirectoryRemoval<'_>],
        admitted_page_records: &[[u32; RESOURCE_WORDS]],
        gpu_failure: &GpuFailureLatch,
    ) -> Result<u64, WgpuRenderRuntimeError> {
        self.upload_staging.stage(
            slot,
            uploads,
            directory,
            removals,
            admitted_page_records,
            gpu_failure,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_transfer(
        &self,
        slot: usize,
        encoder: &mut wgpu::CommandEncoder,
        uploads: &[UploadPlan<'_>],
        directory: Option<&PreparedDirectoryBatch>,
        removals: &[DirectoryRemoval<'_>],
        admitted_page_records: &[[u32; RESOURCE_WORDS]],
    ) {
        self.upload_staging.encode_copies(
            slot,
            encoder,
            &self.payload_segments,
            uploads,
            &self.directory_buffer,
            &self.page_record_buffer,
            directory,
            removals,
            admitted_page_records,
        );
    }

    fn transfer_submitted(&mut self, slot: usize) {
        self.upload_staging.submitted(slot);
    }

    fn commit_transfer(
        &mut self,
        prepared_directory: Option<PreparedDirectoryBatch>,
        allocator_operations: &[ArenaOperation],
        evicted: &BTreeSet<BrickKey>,
        uploads: &[UploadPlan<'_>],
        empty_residents: &BTreeMap<BrickKey, ResidentResource>,
    ) -> ResidencyCommitOutcome {
        if let Some(prepared) = prepared_directory {
            self.global_residency
                .commit(prepared)
                .expect("an exclusively owned prepared directory batch cannot become stale");
        }
        if !allocator_operations.is_empty() {
            apply_arena_operations(&mut self.payload_segments, allocator_operations);
        }

        // Same-transaction admissions cancel their current destructive event
        // before new evictions are recorded. This makes the committed event
        // count match the capacity proof `(current ∪ evicted) - admitted`
        // without a transient over-capacity state.
        let mut reuploaded_resources = 0_u64;
        for upload in uploads {
            if self.recently_evicted.remove(&upload.key).is_some() {
                reuploaded_resources = reuploaded_resources.saturating_add(1);
            }
        }
        for key in empty_residents.keys() {
            self.recently_evicted.remove(key);
        }

        for victim in evicted {
            let resource = self
                .resident
                .remove(victim)
                .expect("every planned global eviction remains resident until commit");
            let age_entry = (resource.last_used_frame, *victim);
            if resource.allocated_bytes == 0 {
                let removed = self.empty_resident_lru.remove(&age_entry);
                debug_assert!(removed);
                self.empty_resident_count = self.empty_resident_count.saturating_sub(1);
            } else {
                let segment = resource.segment as usize;
                let removed = self.payload_resident_lru[segment].remove(&age_entry);
                debug_assert!(removed);
                self.payload_evictable_lru[segment].remove(&age_entry);
                self.resident_payload_bytes = self
                    .resident_payload_bytes
                    .saturating_sub(resource.allocated_bytes);
            }
            self.record_eviction(*victim);
        }

        for upload in uploads {
            let previous = self.resident.insert(upload.key, upload.resident.clone());
            debug_assert!(previous.is_none());
            let inserted = self.payload_resident_lru[upload.segment]
                .insert((upload.resident.last_used_frame, upload.key));
            debug_assert!(inserted);
            self.resident_payload_bytes = self
                .resident_payload_bytes
                .saturating_add(upload.resident.allocated_bytes);
            self.refresh_payload_pin_state(upload.key);
        }
        for (key, resource) in empty_residents {
            let previous = self.resident.insert(*key, resource.clone());
            debug_assert!(previous.is_none());
            self.empty_resident_count += 1;
            let inserted = self
                .empty_resident_lru
                .insert((resource.last_used_frame, *key));
            debug_assert!(inserted);
        }

        let changes = self.change_set(
            uploads
                .iter()
                .map(|upload| upload.key)
                .chain(empty_residents.keys().copied()),
            evicted.iter().copied(),
        );
        ResidencyCommitOutcome {
            changes,
            evicted_resources: evicted.len() as u64,
            reuploaded_resources,
            resident_payload_bytes: self.resident_payload_bytes,
        }
    }

    fn record_eviction(&mut self, key: BrickKey) {
        record_residency_eviction(
            &mut self.recently_evicted,
            &mut self.recently_evicted_order,
            &mut self.next_eviction_sequence,
            key,
        );
    }

    fn change_set<I, R>(&self, additions: I, removals: R) -> ResidencyChangeSet
    where
        I: IntoIterator<Item = BrickKey>,
        R: IntoIterator<Item = BrickKey>,
    {
        let mut changes = BTreeMap::new();
        for key in additions {
            let previous = changes.insert(key, true);
            debug_assert!(previous.is_none(), "one residency batch adds each key once");
        }
        for key in removals {
            let previous = changes.insert(key, false);
            debug_assert!(
                previous.is_none(),
                "one residency batch cannot add and remove the same key"
            );
        }
        ResidencyChangeSet { changes }
    }

    const fn transfer_capacity_bytes(&self) -> u64 {
        self.upload_staging.capacity()
    }

    fn reset_generation(&mut self) {
        assert!(
            self.frame_leases.is_empty(),
            "dataset retirement releases every opaque frame lease before residency reset"
        );
        self.resource_grids = None;
        self.global_residency = GlobalResidencyDirectory::new(
            u32::try_from(MAX_GLOBAL_RESIDENT_PAGES)
                .expect("the renderer-global page ceiling fits u32"),
        )
        .expect("the already-qualified fixed global residency capacity remains valid");
        self.resident.clear();
        self.resident_payload_bytes = 0;
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
        self.pending_leases.clear();
    }
}

pub(super) struct Runtime {
    pipelines: PipelineState,
    device: wgpu::Device,
    queue: wgpu::Queue,
    gpu_failure: Arc<GpuFailureLatch>,
    render_bind_group_layout: wgpu::BindGroupLayout,
    residency: ResidencyOwner,
    frame_coordinator: FrameCoordinator,
    next_capture: u64,
    next_timing: u64,
    next_pick: u64,
    timing: Option<TimingResources>,
    pick: PickResources,
    hidden_refinement: HiddenRefinementScheduler,
    in_flight_submissions: Arc<AtomicUsize>,
    validation_error_count: Arc<AtomicUsize>,
    config: WgpuRenderRuntimeConfig,
    diagnostics: WgpuRenderRuntimeDiagnostics,
}

impl Runtime {
    pub(super) fn set_hidden_refinement_wake(
        &mut self,
        wake: Arc<dyn Fn() + Send + Sync + 'static>,
    ) {
        self.hidden_refinement.set_wake(wake);
    }

    #[cfg(test)]
    pub(super) fn from_existing_device_with_payload_segment_limit(
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: WgpuRenderRuntimeConfig,
        segment_limit: u64,
    ) -> Result<Self, WgpuRenderRuntimeError> {
        Self::from_existing_device_parts(adapter, device, queue, config, Some(segment_limit), None)
    }

    #[cfg(test)]
    pub(super) fn from_existing_device_with_payload_test_limits(
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: WgpuRenderRuntimeConfig,
        segment_limit: u64,
        initial_commitment: u64,
    ) -> Result<Self, WgpuRenderRuntimeError> {
        Self::from_existing_device_parts(
            adapter,
            device,
            queue,
            config,
            Some(segment_limit),
            Some(initial_commitment),
        )
    }

    pub(super) fn from_existing_device(
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: WgpuRenderRuntimeConfig,
    ) -> Result<Self, WgpuRenderRuntimeError> {
        Self::from_existing_device_parts(adapter, device, queue, config, None, None)
    }

    fn from_existing_device_parts(
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: WgpuRenderRuntimeConfig,
        segment_limit_override: Option<u64>,
        initial_commitment_override: Option<u64>,
    ) -> Result<Self, WgpuRenderRuntimeError> {
        validate_adapter(adapter)?;
        let info = adapter.get_info();
        let adapter_limits = adapter.limits();
        validate_device_limits(&device.limits())?;
        // Global residency metadata is a fixed renderer-wide prerequisite,
        // not presentation scratch. Reserve it once before dividing the
        // remaining configurable budget so a valid modest runtime is not
        // forced to hide 52 MiB inside its 15% "other" share.
        let configurable_bytes = config
            .gpu_budget_bytes()
            .saturating_sub(GLOBAL_RESIDENCY_METADATA_BYTES);
        let payload_ledger_bytes = configurable_bytes.saturating_mul(75) / 100;
        let transfer_capacity_bytes = configurable_bytes.saturating_mul(10) / 100;
        let (upload_staging_capacity, compaction_scratch_capacity) =
            transfer_capacity_partition(transfer_capacity_bytes)?;
        let other_capacity_bytes = config
            .gpu_budget_bytes()
            .saturating_sub(payload_ledger_bytes)
            .saturating_sub(transfer_capacity_bytes);
        let mut payload_limits = device.limits();
        if let Some(limit) = segment_limit_override {
            payload_limits.max_buffer_size = payload_limits.max_buffer_size.min(limit);
            payload_limits.max_storage_buffer_binding_size =
                payload_limits.max_storage_buffer_binding_size.min(limit);
        }
        let segment_capacities = payload_segment_capacities(payload_ledger_bytes, &payload_limits)?;
        let initial_segment_commitments = initial_payload_segment_commitments(
            &segment_capacities,
            initial_commitment_override.unwrap_or(INITIAL_PAYLOAD_COMMITMENT_BYTES),
        );
        let payload_capacity_bytes = segment_capacities.iter().sum::<u64>();
        let payload_segment_count = segment_capacities
            .iter()
            .filter(|capacity| **capacity != 0)
            .count();

        let gpu_failure = Arc::new(GpuFailureLatch::default());
        let device_loss = Arc::clone(&gpu_failure);
        device.set_device_lost_callback(move |reason, _message| {
            device_loss.record_device_loss(reason);
        });
        let validation_error_count = Arc::new(AtomicUsize::new(0));
        let error_count = Arc::clone(&validation_error_count);
        let uncaptured_failure = Arc::clone(&gpu_failure);
        device.on_uncaptured_error(Arc::new(move |error| {
            #[cfg(test)]
            eprintln!("mirante4d uncaptured WGPU error: {error}");
            uncaptured_failure.record_uncaptured_error(&error);
            error_count.fetch_add(1, Ordering::Relaxed);
        }));

        let payload_segments = segment_capacities
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, capacity)| *capacity != 0)
            .map(|(index, logical_capacity)| {
                let capacity = initial_segment_commitments[index];
                let allocated_bytes = capacity.max(COPY_ALIGNMENT);
                PayloadSegment {
                    buffer: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some(match index {
                            0 => "mirante4d-payload-segment-0",
                            1 => "mirante4d-payload-segment-1",
                            2 => "mirante4d-payload-segment-2",
                            _ => "mirante4d-payload-segment-3",
                        }),
                        size: allocated_bytes,
                        usage: wgpu::BufferUsages::STORAGE
                            | wgpu::BufferUsages::COPY_SRC
                            | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }),
                    logical_capacity,
                    capacity,
                    allocated_bytes,
                    allocator: ArenaAllocator::new(capacity),
                }
            })
            .collect::<Vec<_>>();
        let compaction_scratch = (compaction_scratch_capacity != 0).then(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mirante4d-payload-compaction-scratch"),
                size: compaction_scratch_capacity,
                usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let directory_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-global-residency-directory"),
            size: GLOBAL_DIRECTORY_BYTES,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let page_record_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-global-page-records"),
            size: GLOBAL_PAGE_RECORD_BYTES,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut render_layout_entries = vec![storage_layout_entry(0, INITIAL_CONTROL_BYTES)];
        render_layout_entries.extend(
            (0..MAX_PAYLOAD_SEGMENTS)
                .map(|index| storage_layout_entry(1 + index as u32, COPY_ALIGNMENT)),
        );
        render_layout_entries.push(storage_layout_entry(5, GLOBAL_DIRECTORY_BYTES));
        render_layout_entries.push(storage_layout_entry(6, GLOBAL_PAGE_RECORD_BYTES));
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mirante4d-target-bind-group-layout"),
            entries: &render_layout_entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mirante4d-color-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let (pick, pick_pipeline_layout) =
            create_pick_fixed_resources(&device, GLOBAL_DIRECTORY_BYTES, GLOBAL_PAGE_RECORD_BYTES);
        let driver = if info.driver_info.trim().is_empty() {
            info.driver.clone()
        } else {
            info.driver_info.clone()
        };
        let timestamp_supported = device.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        let encoder_timestamp_supported = device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS);
        let timing = (timestamp_supported && config.gpu_timing())
            .then(|| create_timing_resources(&device, &queue));
        let gpu_timing_enabled = timing.is_some();
        let upload_staging = UploadStagingPool::new(upload_staging_capacity);
        let global_residency = GlobalResidencyDirectory::new(
            u32::try_from(MAX_GLOBAL_RESIDENT_PAGES)
                .map_err(|_| WgpuRenderRuntimeError::InvalidConfiguration)?,
        )
        .map_err(map_global_residency_error)?;
        debug_assert_eq!(
            global_residency.directory_buffer_bytes(),
            GLOBAL_DIRECTORY_BYTES
        );
        debug_assert_eq!(
            global_residency.page_record_buffer_bytes(),
            GLOBAL_PAGE_RECORD_BYTES
        );
        let initial_device = device.clone();
        let initial_layout = pipeline_layout.clone();
        let validation_capture = config.validation_capture();
        let pick_device = device.clone();
        let pick_layout = pick_pipeline_layout.clone();
        let compiler = PipelineCompiler::spawn(
            move || compile_initial_pipelines(&initial_device, &initial_layout, validation_capture),
            move || compile_pick_pipeline(&pick_device, &pick_layout),
        )?;
        let pipelines = PipelineState::compiling(compiler);
        let hidden_refinement = HiddenRefinementScheduler::spawn(
            device.clone(),
            queue.clone(),
            Arc::clone(&gpu_failure),
        )?;
        gpu_failure.ensure_available()?;
        let payload_arena_allocated_bytes = payload_segments
            .iter()
            .map(|segment| segment.allocated_bytes)
            .sum();
        let payload_committed_capacity_bytes = payload_segments
            .iter()
            .map(|segment| segment.capacity)
            .sum();
        let mut payload_segment_committed_capacity_bytes = [0; MAX_PAYLOAD_SEGMENTS];
        for (destination, segment) in payload_segment_committed_capacity_bytes
            .iter_mut()
            .zip(&payload_segments)
        {
            *destination = segment.capacity;
        }
        Ok(Self {
            pipelines,
            device,
            queue,
            gpu_failure,
            render_bind_group_layout: bind_group_layout,
            residency: ResidencyOwner::new(
                payload_segments,
                compaction_scratch,
                compaction_scratch_capacity,
                directory_buffer,
                page_record_buffer,
                upload_staging,
                global_residency,
            ),
            frame_coordinator: FrameCoordinator::new()?,
            next_capture: 1,
            next_timing: 1,
            next_pick: 1,
            timing,
            pick,
            hidden_refinement,
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
                payload_arena_allocated_bytes,
                payload_committed_capacity_bytes,
                payload_uncommitted_capacity_bytes: payload_capacity_bytes
                    .saturating_sub(payload_committed_capacity_bytes),
                payload_segment_committed_capacity_bytes,
                payload_growths: 0,
                payload_growth_copy_bytes: 0,
                resident_payload_used_bytes: 0,
                peak_resident_payload_used_bytes: 0,
                payload_free_bytes: payload_committed_capacity_bytes,
                payload_largest_contiguous_bytes: initial_segment_commitments
                    .iter()
                    .copied()
                    .max()
                    .unwrap_or(0),
                payload_segment_free_bytes: initial_segment_commitments,
                payload_segment_largest_contiguous_bytes: initial_segment_commitments,
                payload_placeability_failures: 0,
                payload_compactions: 0,
                payload_compaction_resources_moved: 0,
                payload_compaction_bytes_moved: 0,
                empty_resident_metadata_capacity_records: MAX_EMPTY_RESIDENTS,
                empty_resident_metadata_bytes_per_record: EMPTY_RESIDENT_METADATA_BYTES_PER_RECORD,
                empty_resident_metadata_records: 0,
                empty_resident_metadata_bytes: 0,
                peak_empty_resident_metadata_bytes: 0,
                peak_transfer_bytes: compaction_scratch_capacity,
                peak_display_target_bytes: 0,
                peak_control_and_residency_metadata_bytes: 0,
                peak_scratch_bytes: 0,
                frames_executed: 0,
                queue_submissions: 0,
                current_in_flight_submissions: 0,
                peak_in_flight_submissions: 0,
                peak_in_flight_color_cutoffs: 0,
                backpressure_deferrals: 0,
                residency_hits: 0,
                residency_misses: 0,
                residency_evictions: 0,
                residency_epoch_reuploads: 0,
                uploaded_resources: 0,
                uploaded_payload_bytes: 0,
                upload_staging_padding_zero_bytes: 0,
                render_thread_payload_fact_scan_bytes: 0,
                directory_publications: 0,
                directory_mutations: 0,
                directory_rebuilds: 0,
                directory_slot_writes: 0,
                page_record_writes: 0,
                target_control_updates: 0,
                target_control_upload_bytes: 0,
                control_buffer_allocations: 0,
                control_buffer_allocation_bytes: 0,
                bind_group_creations: 0,
                usable_pipeline_handles: 0,
                explicit_staging_allocations: 0,
                explicit_staging_bytes: 0,
                peak_explicit_staging_bytes: 0,
                allocator_plans: 0,
                retained_navigation_frames: 0,
                cold_coverage_membership_checks: 0,
                cold_coverage_resident_matches: 0,
                gpu_timestamps_supported: timestamp_supported,
                gpu_timing_enabled,
                gpu_encoder_timestamps_supported: encoder_timestamp_supported,
                completed_gpu_timings: 0,
                gpu_timing_failures: 0,
                last_gpu_batch_envelope_ns: None,
                last_gpu_render_pass_ns: None,
                completed_cpu_timings: 0,
                last_cpu_timing_frame: None,
                last_cpu_planning_ns: None,
                last_cpu_queue_submit_ns: None,
                total_cpu_planning_ns: 0,
                total_cpu_queue_submit_ns: 0,
                hidden_refinement_jobs_started: 0,
                hidden_refinement_jobs_completed: 0,
                hidden_refinement_jobs_cancelled: 0,
                hidden_refinement_jobs_failed: 0,
                hidden_refinement_batches: 0,
                hidden_refinement_rows: 0,
                hidden_refinement_elapsed_ns: 0,
                hidden_refinement_last_batch_rows: None,
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

    pub(super) fn pipeline_readiness(&self) -> Result<PipelineReadiness, WgpuRenderRuntimeError> {
        self.ensure_device_available()?;
        self.pipelines.readiness()
    }

    pub(super) fn poll_pipeline_readiness(
        &mut self,
    ) -> Result<PipelineReadiness, WgpuRenderRuntimeError> {
        self.ensure_device_available()?;
        let before = self.pipelines.readiness;
        let readiness = self.pipelines.poll()?;
        self.ensure_device_available()?;
        match (before, readiness) {
            (PipelineReadiness::CompilingInitial, PipelineReadiness::InitialRenderReady) => {
                self.diagnostics.usable_pipeline_handles =
                    self.diagnostics.usable_pipeline_handles.saturating_add(5);
            }
            (PipelineReadiness::InitialRenderReady, PipelineReadiness::Ready) => {
                self.diagnostics.usable_pipeline_handles =
                    self.diagnostics.usable_pipeline_handles.saturating_add(1);
            }
            _ => {}
        }
        Ok(readiness)
    }

    pub(super) fn pipeline_capability_is_ready(
        &self,
        capability: PipelineCapability,
    ) -> Result<bool, WgpuRenderRuntimeError> {
        self.ensure_device_available()?;
        self.pipelines.capability_is_ready(capability)
    }

    pub(super) const fn payload_capacity_bytes(&self) -> u64 {
        self.residency.payload_capacity_bytes()
    }

    pub(super) const fn resident_payload_bytes(&self) -> u64 {
        self.residency.resident_payload_bytes()
    }

    pub(super) fn ensure_global_requirement_union(
        &mut self,
        catalog: &DatasetCatalog,
        requirements: &[BrickKey],
    ) -> Result<(), WgpuRenderRuntimeError> {
        self.ensure_device_available()?;
        self.collect_completed_color_leases();
        self.validate_published_fronts_in_union(requirements)?;
        let commitments = match self
            .residency
            .preflight_global_requirement_union(catalog, requirements)
        {
            Ok(commitments) => Ok(commitments),
            Err(WgpuRenderRuntimeError::PayloadPlacementUnavailable { .. }) => {
                self.diagnostics.payload_placeability_failures = self
                    .diagnostics
                    .payload_placeability_failures
                    .saturating_add(1);
                if !self.compact_payload_residency()? {
                    return self
                        .residency
                        .preflight_global_requirement_union(catalog, requirements)
                        .and_then(|commitments| self.ensure_payload_commitment(&commitments));
                }
                let result = self
                    .residency
                    .preflight_global_requirement_union(catalog, requirements);
                if matches!(
                    result,
                    Err(WgpuRenderRuntimeError::PayloadPlacementUnavailable { .. })
                ) {
                    self.diagnostics.payload_placeability_failures = self
                        .diagnostics
                        .payload_placeability_failures
                        .saturating_add(1);
                }
                result
            }
            Err(error) => Err(error),
        };
        self.ensure_payload_commitment(&commitments?)
    }

    fn ensure_payload_commitment(&mut self, targets: &[u64]) -> Result<(), WgpuRenderRuntimeError> {
        if targets
            .iter()
            .zip(&self.residency.payload_segments)
            .all(|(target, segment)| *target <= segment.capacity)
        {
            return Ok(());
        }

        let copy_bytes = targets
            .iter()
            .enumerate()
            .filter(|(segment, target)| {
                **target > self.residency.payload_segments[*segment].capacity
            })
            .map(|(segment, _)| {
                self.residency
                    .resident
                    .values()
                    .filter(|resource| {
                        resource.allocated_bytes != 0 && resource.segment as usize == segment
                    })
                    .map(|resource| resource.offset.saturating_add(resource.allocated_bytes))
                    .max()
                    .unwrap_or(0)
            })
            .sum::<u64>();
        if copy_bytes != 0 {
            let in_flight = self.in_flight_submissions.load(Ordering::Acquire);
            self.diagnostics.current_in_flight_submissions = in_flight;
            if in_flight >= MAX_IN_FLIGHT_SUBMISSIONS {
                self.diagnostics.backpressure_deferrals =
                    self.diagnostics.backpressure_deferrals.saturating_add(1);
                return Err(WgpuRenderRuntimeError::PayloadRecoveryDeferred);
            }
        }

        let mut encoder = (copy_bytes != 0).then(|| {
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mirante4d-payload-growth"),
                })
        });
        let mut replacements = Vec::new();
        let mut copied_bytes = 0_u64;
        for (segment_index, target) in targets.iter().copied().enumerate() {
            let segment = &self.residency.payload_segments[segment_index];
            if target <= segment.capacity {
                continue;
            }
            debug_assert!(target <= segment.logical_capacity);
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(match segment_index {
                    0 => "mirante4d-payload-segment-0-growth",
                    1 => "mirante4d-payload-segment-1-growth",
                    2 => "mirante4d-payload-segment-2-growth",
                    _ => "mirante4d-payload-segment-3-growth",
                }),
                size: target.max(COPY_ALIGNMENT),
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let preserved = self
                .residency
                .resident
                .values()
                .filter(|resource| {
                    resource.allocated_bytes != 0 && resource.segment as usize == segment_index
                })
                .map(|resource| resource.offset.saturating_add(resource.allocated_bytes))
                .max()
                .unwrap_or(0);
            if preserved != 0 {
                encoder
                    .as_mut()
                    .expect("a populated growth prepared one command encoder")
                    .copy_buffer_to_buffer(&segment.buffer, 0, &buffer, 0, preserved);
                copied_bytes = copied_bytes.saturating_add(preserved);
            }
            replacements.push((segment_index, target, buffer));
        }
        self.ensure_device_available()?;

        if let Some(encoder) = encoder {
            self.queue.submit([encoder.finish()]);
            let in_flight = Arc::clone(&self.in_flight_submissions);
            let submitted = in_flight.fetch_add(1, Ordering::AcqRel) + 1;
            self.queue.on_submitted_work_done(move || {
                in_flight.fetch_sub(1, Ordering::AcqRel);
            });
            self.diagnostics.queue_submissions =
                self.diagnostics.queue_submissions.saturating_add(1);
            self.diagnostics.current_in_flight_submissions = submitted;
            self.diagnostics.peak_in_flight_submissions =
                self.diagnostics.peak_in_flight_submissions.max(submitted);
        }
        for (segment_index, target, buffer) in replacements {
            let segment = &mut self.residency.payload_segments[segment_index];
            let previous = segment.capacity;
            segment.allocator.grow(previous, target);
            segment.buffer = buffer;
            segment.capacity = target;
            segment.allocated_bytes = target.max(COPY_ALIGNMENT);
        }
        self.rebuild_payload_bind_groups();
        self.ensure_device_available()?;

        self.diagnostics.payload_growths = self.diagnostics.payload_growths.saturating_add(1);
        self.diagnostics.payload_growth_copy_bytes = self
            .diagnostics
            .payload_growth_copy_bytes
            .saturating_add(copied_bytes);
        self.refresh_payload_allocator_diagnostics();
        Ok(())
    }

    fn rebuild_payload_bind_groups(&mut self) {
        let mut creations = 0_u64;
        for presentation in self.frame_coordinator.presentations.values_mut() {
            presentation.render_bind_group = create_presentation_render_bind_group(
                &self.device,
                &self.render_bind_group_layout,
                &self.residency.payload_segments,
                &self.residency.directory_buffer,
                &self.residency.page_record_buffer,
                &presentation.control_buffer,
                &mut creations,
            );
            presentation.pick_bind_groups = create_presentation_pick_bind_groups(
                &self.device,
                &self.pick,
                &self.residency.payload_segments,
                &self.residency.directory_buffer,
                &self.residency.page_record_buffer,
                &presentation.control_buffer,
                &mut creations,
            );
        }
        self.diagnostics.bind_group_creations = self
            .diagnostics
            .bind_group_creations
            .saturating_add(creations);
    }

    fn validate_published_fronts_in_union(
        &self,
        requirements: &[BrickKey],
    ) -> Result<(), WgpuRenderRuntimeError> {
        // Published fronts are non-negotiable transition obligations. A
        // hidden candidate can be transactionally replaced, and an in-flight
        // predecessor can merely defer reuse until its bounded cutoff
        // completes; neither should become a permanent LOD refusal.
        for front in self
            .frame_coordinator
            .slots
            .iter()
            .filter_map(|slot| slot.front)
        {
            let state = self
                .frame_coordinator
                .presentations
                .get(&front)
                .and_then(|presentation| presentation.frame_state.as_ref());
            if let Some(state) = state {
                let front_requirements = self.residency.requirements(state.residency);
                if front_requirements
                    .resources()
                    .any(|requirement| requirements.binary_search(&requirement.key()).is_err())
                {
                    return Err(WgpuRenderRuntimeError::RequirementSetChanged);
                }
            }
        }
        Ok(())
    }

    pub(super) fn recover_payload_fragmentation(&mut self) -> Result<bool, WgpuRenderRuntimeError> {
        self.ensure_device_available()?;
        self.collect_completed_color_leases();
        self.compact_payload_residency()
    }

    fn compact_payload_residency(&mut self) -> Result<bool, WgpuRenderRuntimeError> {
        let plan = self.residency.prepare_payload_compaction();
        if plan.relocations.is_empty() {
            return Ok(false);
        }
        let maximum_move = plan
            .relocations
            .iter()
            .map(|relocation| relocation.bytes)
            .max()
            .unwrap_or(0);
        if maximum_move > self.residency.compaction_scratch_capacity {
            // Compaction is an optional physical recovery, not a new
            // feasibility requirement. Leave residency untouched and let the
            // caller project the original placement refusal into adaptive LOD
            // selection when the bounded scratch cannot move this payload.
            return Ok(false);
        }
        let in_flight = self.in_flight_submissions.load(Ordering::Acquire);
        self.diagnostics.current_in_flight_submissions = in_flight;
        if in_flight >= MAX_IN_FLIGHT_SUBMISSIONS {
            self.diagnostics.backpressure_deferrals =
                self.diagnostics.backpressure_deferrals.saturating_add(1);
            return Err(WgpuRenderRuntimeError::PayloadRecoveryDeferred);
        }

        let page_records = plan
            .relocations
            .iter()
            .map(|relocation| {
                let mut resource = self
                    .residency
                    .resident
                    .get(&relocation.key)
                    .expect("a planned relocation names one resident payload")
                    .clone();
                resource.offset = relocation.destination_offset;
                resource.validity_offset = relocation.destination_validity_offset;
                Ok((
                    resource.page_record_index,
                    resource_record(relocation.key, Some(&resource))?,
                ))
            })
            .collect::<Result<Vec<_>, WgpuRenderRuntimeError>>()?;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-payload-compaction"),
            });
        let scratch = self
            .residency
            .compaction_scratch
            .as_ref()
            .expect("a move that fits the nonzero scratch has its fixed buffer");
        for relocation in &plan.relocations {
            let segment = &self.residency.payload_segments[relocation.segment].buffer;
            encoder.copy_buffer_to_buffer(
                segment,
                relocation.source_offset,
                scratch,
                0,
                relocation.bytes,
            );
            encoder.copy_buffer_to_buffer(
                scratch,
                0,
                segment,
                relocation.destination_offset,
                relocation.bytes,
            );
        }
        for (page_index, record) in &page_records {
            debug_assert_ne!(*page_index, u32::MAX);
            self.queue.write_buffer(
                &self.residency.page_record_buffer,
                u64::from(*page_index) * PAGE_RECORD_BYTES,
                bytemuck::bytes_of(record),
            );
        }
        self.ensure_device_available()?;
        self.queue.submit([encoder.finish()]);
        let in_flight = Arc::clone(&self.in_flight_submissions);
        let submitted = in_flight.fetch_add(1, Ordering::AcqRel) + 1;
        self.queue.on_submitted_work_done(move || {
            in_flight.fetch_sub(1, Ordering::AcqRel);
        });

        let moved_resources = plan.relocations.len() as u64;
        let moved_bytes = plan
            .relocations
            .iter()
            .map(|relocation| relocation.bytes)
            .sum::<u64>();
        self.residency.commit_payload_compaction(plan);
        self.diagnostics.payload_compactions =
            self.diagnostics.payload_compactions.saturating_add(1);
        self.diagnostics.payload_compaction_resources_moved = self
            .diagnostics
            .payload_compaction_resources_moved
            .saturating_add(moved_resources);
        self.diagnostics.payload_compaction_bytes_moved = self
            .diagnostics
            .payload_compaction_bytes_moved
            .saturating_add(moved_bytes);
        self.diagnostics.page_record_writes = self
            .diagnostics
            .page_record_writes
            .saturating_add(moved_resources);
        self.diagnostics.queue_submissions = self.diagnostics.queue_submissions.saturating_add(1);
        self.diagnostics.current_in_flight_submissions = submitted;
        self.diagnostics.peak_in_flight_submissions =
            self.diagnostics.peak_in_flight_submissions.max(submitted);
        self.refresh_payload_allocator_diagnostics();
        Ok(true)
    }

    fn refresh_payload_allocator_diagnostics(&mut self) {
        let committed = self.residency.payload_committed_capacity_bytes();
        self.diagnostics.payload_arena_allocated_bytes = self.residency.payload_allocated_bytes();
        self.diagnostics.payload_committed_capacity_bytes = committed;
        self.diagnostics.payload_uncommitted_capacity_bytes = self
            .residency
            .payload_capacity_bytes()
            .saturating_sub(committed);
        self.diagnostics.payload_segment_committed_capacity_bytes =
            self.residency.payload_segment_committed_capacity_bytes();
        self.diagnostics.payload_free_bytes = self.residency.payload_free_bytes();
        self.diagnostics.payload_largest_contiguous_bytes =
            self.residency.payload_largest_contiguous_bytes();
        self.diagnostics.payload_segment_free_bytes = self.residency.payload_segment_free_bytes();
        self.diagnostics.payload_segment_largest_contiguous_bytes =
            self.residency.payload_segment_largest_contiguous_bytes();
    }

    fn ensure_device_available(&self) -> Result<(), WgpuRenderRuntimeError> {
        self.gpu_failure.ensure_available()
    }

    /// Releases residency snapshots only after WGPU reports completion of the
    /// coordinated color cutoff that consumed them. The callback owns only
    /// opaque lease identities; the residency subowner remains the sole place
    /// that can mutate pin counts.
    fn collect_completed_color_leases(&mut self) {
        let completed = {
            let mut completed = self
                .frame_coordinator
                .completed_color_cutoffs
                .lock()
                .expect("the color-completion queue is never poisoned");
            std::mem::take(&mut *completed)
        };
        for completed in completed {
            let leases = self
                .frame_coordinator
                .in_flight_color_leases
                .remove(&completed.cutoff)
                .expect("one completed color cutoff retains one lease record");
            self.residency.release_completed_frame_leases(
                completed.residency_generation,
                self.frame_coordinator.residency_generation,
                leases,
            );
        }
    }

    fn collect_hidden_refinement_results(&mut self) {
        for result in self.hidden_refinement.drain_results() {
            let matching =
                self.frame_coordinator
                    .presentations
                    .iter()
                    .find_map(|(token, presentation)| {
                        presentation
                            .hidden_volume_refinement
                            .as_ref()
                            .filter(|refinement| refinement.job_id == result.job_id)
                            .map(|_| *token)
                    });
            match result.outcome {
                HiddenRefinementWorkerOutcome::Cancelled {
                    batches,
                    rows,
                    elapsed_ns,
                } => {
                    self.diagnostics.hidden_refinement_jobs_cancelled = self
                        .diagnostics
                        .hidden_refinement_jobs_cancelled
                        .saturating_add(1);
                    self.record_hidden_refinement_work(batches, rows, elapsed_ns, None);
                }
                HiddenRefinementWorkerOutcome::Complete {
                    batches,
                    rows,
                    elapsed_ns,
                    last_batch_rows,
                } => {
                    self.diagnostics.hidden_refinement_jobs_completed = self
                        .diagnostics
                        .hidden_refinement_jobs_completed
                        .saturating_add(1);
                    self.record_hidden_refinement_work(
                        batches,
                        rows,
                        elapsed_ns,
                        Some(last_batch_rows),
                    );
                    if let Some(token) = matching {
                        let target = {
                            let presentation = self
                                .frame_coordinator
                                .presentations
                                .get_mut(&token)
                                .expect("a matched hidden result retains its presentation");
                            let refinement = presentation
                                .hidden_volume_refinement
                                .as_mut()
                                .expect("a matched hidden result retains its job state");
                            refinement
                                .completed_rows
                                .store(refinement.extent.height_pixels(), Ordering::Release);
                            refinement.status = HiddenRefinementStatus::Complete {
                                batches,
                                elapsed_ns,
                                last_batch_rows,
                            };
                            presentation.logical_target
                        };
                        self.frame_coordinator.slot_mut(target).color_retry_pending = true;
                    }
                }
                HiddenRefinementWorkerOutcome::Failed {
                    batches,
                    rows,
                    elapsed_ns,
                } => {
                    self.diagnostics.hidden_refinement_jobs_failed = self
                        .diagnostics
                        .hidden_refinement_jobs_failed
                        .saturating_add(1);
                    self.record_hidden_refinement_work(batches, rows, elapsed_ns, None);
                    if let Some(token) = matching {
                        let target = {
                            let presentation = self
                                .frame_coordinator
                                .presentations
                                .get_mut(&token)
                                .expect("a matched hidden failure retains its presentation");
                            presentation
                                .hidden_volume_refinement
                                .as_mut()
                                .expect("a matched hidden failure retains its job state")
                                .status = HiddenRefinementStatus::Failed;
                            presentation.logical_target
                        };
                        self.frame_coordinator.slot_mut(target).color_retry_pending = true;
                    }
                }
            }
        }
    }

    fn record_hidden_refinement_work(
        &mut self,
        batches: u32,
        rows: u32,
        elapsed_ns: u64,
        last_batch_rows: Option<u32>,
    ) {
        self.diagnostics.hidden_refinement_batches = self
            .diagnostics
            .hidden_refinement_batches
            .saturating_add(u64::from(batches));
        self.diagnostics.hidden_refinement_rows = self
            .diagnostics
            .hidden_refinement_rows
            .saturating_add(u64::from(rows));
        self.diagnostics.hidden_refinement_elapsed_ns = self
            .diagnostics
            .hidden_refinement_elapsed_ns
            .saturating_add(elapsed_ns);
        if last_batch_rows.is_some() {
            self.diagnostics.hidden_refinement_last_batch_rows = last_batch_rows;
        }
        self.diagnostics.queue_submissions = self
            .diagnostics
            .queue_submissions
            .saturating_add(u64::from(batches));
    }

    pub(super) fn resource_is_resident(&self, key: BrickKey) -> bool {
        self.residency.resource_is_resident(key)
    }

    pub(super) fn pending_residency_evictions(
        &self,
        maximum: usize,
    ) -> Box<[GpuResidencyEvictionEvent]> {
        self.residency.pending_evictions(maximum)
    }

    pub(super) fn has_pending_residency_evictions(&self) -> bool {
        self.residency.has_pending_evictions()
    }

    pub(super) fn acknowledge_residency_evictions(&mut self, through_sequence: u64) {
        self.residency.acknowledge_evictions(through_sequence);
    }

    pub(super) fn offer_residency_leases(
        &mut self,
        leases: &[Arc<dyn ResourceLease>],
    ) -> Result<(), WgpuRenderRuntimeError> {
        let admitted = self.residency.offer_leases(leases)?;
        if !admitted.is_empty() {
            let active = self
                .frame_coordinator
                .presentations
                .keys()
                .copied()
                .collect::<Vec<_>>();
            for token in active {
                self.refresh_pending_residency_target(token);
            }
        }
        Ok(())
    }

    pub(super) fn retire_residency_offers(&mut self, keys: &[BrickKey]) {
        if keys.is_empty() {
            return;
        }
        let retired = keys.iter().copied().collect::<BTreeSet<_>>();
        self.residency.retire_offers(&retired);
        let active = self
            .frame_coordinator
            .presentations
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for token in active {
            self.refresh_pending_residency_target(token);
        }
    }

    pub(super) fn has_pending_residency_work(&self) -> bool {
        !self.frame_coordinator.pending_residency_targets.is_empty()
    }

    fn refresh_pending_residency_target(&mut self, token: PrivatePresentationId) {
        let requires_execution = self
            .frame_coordinator
            .presentations
            .get(&token)
            .and_then(|presentation| presentation.frame_state.as_ref())
            .is_some_and(|state| self.residency.has_relevant_offer(state.residency));
        if requires_execution {
            self.frame_coordinator
                .pending_residency_targets
                .insert(token);
        } else {
            self.frame_coordinator
                .pending_residency_targets
                .remove(&token);
        }
    }

    pub(super) fn activate_dataset_generation(
        &mut self,
        catalog: &DatasetCatalog,
    ) -> Result<(), WgpuRenderRuntimeError> {
        let grids = RenderResourceGridCatalog::current(catalog);
        self.activate_dataset_generation_with_resource_grids(catalog, &grids)
    }

    pub(super) fn activate_dataset_generation_with_resource_grids(
        &mut self,
        catalog: &DatasetCatalog,
        grids: &RenderResourceGridCatalog,
    ) -> Result<(), WgpuRenderRuntimeError> {
        self.ensure_device_available()?;
        self.residency.activate_dataset(catalog, grids)
    }

    pub(super) fn request_coordinated_layout(
        &mut self,
        desired: &[CoordinatedTargetLayout],
    ) -> Result<CoordinatedLayoutReport, WgpuRenderRuntimeError> {
        self.ensure_device_available()?;
        let mut desired_extents = [None; 4];
        for layout in desired {
            validate_extent(layout.extent())?;
            let slot = &mut desired_extents[layout.target().index()];
            if slot.replace(layout.extent()).is_some() {
                return Err(WgpuRenderRuntimeError::DuplicateCoordinatedTarget {
                    target: layout.target(),
                });
            }
        }

        let mut allocation_count = 0_usize;
        let mut allocation_display_bytes = 0_u64;
        let mut replacement_display_bytes = 0_u64;
        for target in PresentationTarget::ALL {
            let Some(extent) = desired_extents[target.index()] else {
                continue;
            };
            let slot = self.frame_coordinator.slot(target);
            if slot.front.is_none() {
                allocation_count = allocation_count
                    .checked_add(1)
                    .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
                allocation_display_bytes = allocation_display_bytes
                    .checked_add(display_allocation_bytes(
                        extent,
                        self.config.validation_capture(),
                    )?)
                    .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
            }
            if target == PresentationTarget::ThreeD {
                match slot.candidate {
                    None => {
                        allocation_count = allocation_count
                            .checked_add(1)
                            .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
                        allocation_display_bytes = allocation_display_bytes
                            .checked_add(display_allocation_bytes(
                                extent,
                                self.config.validation_capture(),
                            )?)
                            .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
                    }
                    Some(token) => {
                        let candidate = self
                            .frame_coordinator
                            .presentations
                            .get(&token)
                            .expect("a coordinated candidate slot owns one presentation");
                        if candidate.display.extent != extent {
                            replacement_display_bytes = replacement_display_bytes
                                .checked_add(display_allocation_bytes(
                                    extent,
                                    self.config.validation_capture(),
                                )?)
                                .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
                        }
                    }
                }
            }
        }
        let registered_after_staging = self
            .frame_coordinator
            .presentations
            .len()
            .checked_add(allocation_count)
            .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
        if registered_after_staging > MAX_REGISTERED_PRESENTATION_TARGETS {
            return Err(WgpuRenderRuntimeError::PresentationCapacityExceeded {
                maximum: MAX_REGISTERED_PRESENTATION_TARGETS,
            });
        }
        let control_peak_bytes = self
            .active_control_bytes()
            .checked_add(
                (allocation_count as u64)
                    .checked_mul(INITIAL_CONTROL_BYTES)
                    .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?,
            )
            .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
        let display_peak_bytes = self
            .active_display_bytes()
            .checked_add(allocation_display_bytes)
            .and_then(|bytes| bytes.checked_add(replacement_display_bytes))
            .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
        self.validate_other_capacity(
            control_peak_bytes,
            display_peak_bytes,
            self.active_capture_bytes(),
        )?;

        // Build the complete successor layout without changing any live slot,
        // texture, frame lease, or app-visible binding. Resource creation and
        // monotone ID reservation may be discarded on failure; the published
        // renderer layout remains byte-for-byte unchanged.
        let old_slots = self.frame_coordinator.slots;
        let mut next_slots = old_slots;
        let mut prepared_presentations = Vec::with_capacity(allocation_count);
        let mut prepared_replacements = Vec::new();
        for target in PresentationTarget::ALL {
            let Some(extent) = desired_extents[target.index()] else {
                next_slots[target.index()] = CoordinatedSlot::empty();
                continue;
            };

            let front = match old_slots[target.index()].front {
                Some(token) => token,
                None => {
                    let prepared = self.prepare_presentation_allocation(target, extent)?;
                    let token = prepared.token;
                    prepared_presentations.push(prepared);
                    token
                }
            };
            let candidate = if target == PresentationTarget::ThreeD {
                match old_slots[target.index()].candidate {
                    Some(token) => {
                        let current_extent = self
                            .frame_coordinator
                            .presentations
                            .get(&token)
                            .expect("a coordinated candidate slot owns one presentation")
                            .display
                            .extent;
                        if current_extent != extent {
                            prepared_replacements.push(PreparedDisplayReplacement {
                                token,
                                display: create_display(
                                    &self.device,
                                    extent,
                                    self.config.validation_capture(),
                                )?,
                                texture_revision: self
                                    .frame_coordinator
                                    .allocate_texture_revision()?,
                            });
                        }
                        Some(token)
                    }
                    None => {
                        let prepared = self.prepare_presentation_allocation(target, extent)?;
                        let token = prepared.token;
                        prepared_presentations.push(prepared);
                        Some(token)
                    }
                }
            } else {
                None
            };
            next_slots[target.index()] = CoordinatedSlot {
                desired: true,
                desired_extent: Some(extent),
                front: Some(front),
                candidate,
                color_retry_pending: old_slots[target.index()].desired
                    && old_slots[target.index()].color_retry_pending,
            };
        }
        self.ensure_device_available()?;

        // Every operation below is infallible over prevalidated owned tokens.
        // This is the single publication point for the desired layout.
        let omitted_tokens = PresentationTarget::ALL
            .into_iter()
            .filter(|target| desired_extents[target.index()].is_none())
            .flat_map(|target| {
                let slot = old_slots[target.index()];
                [slot.front, slot.candidate].into_iter().flatten()
            })
            .collect::<Vec<_>>();
        for replacement in prepared_replacements {
            self.deactivate_presentation(replacement.token)
                .expect("a prepared coordinated replacement retains its private allocation");
            let presentation = self
                .frame_coordinator
                .presentations
                .get_mut(&replacement.token)
                .expect("a prepared coordinated replacement retains its private allocation");
            presentation.texture_revision = replacement.texture_revision;
            presentation.display = replacement.display;
        }
        for prepared in prepared_presentations {
            let previous = self
                .frame_coordinator
                .presentations
                .insert(prepared.token, prepared.presentation);
            debug_assert!(previous.is_none());
            self.diagnostics.bind_group_creations = self
                .diagnostics
                .bind_group_creations
                .saturating_add(prepared.bind_group_creations);
            self.diagnostics.control_buffer_allocations = self
                .diagnostics
                .control_buffer_allocations
                .saturating_add(1);
            self.diagnostics.control_buffer_allocation_bytes = self
                .diagnostics
                .control_buffer_allocation_bytes
                .saturating_add(INITIAL_CONTROL_BYTES);
        }
        self.frame_coordinator.slots = next_slots;
        debug_assert!(
            PresentationTarget::ALL.into_iter().all(|target| {
                let slot = self.frame_coordinator.slot(target);
                target != PresentationTarget::ThreeD
                    || !slot.desired
                    || slot.candidate.is_some_and(|candidate| {
                        self.frame_coordinator
                            .presentations
                            .get(&candidate)
                            .is_some_and(|presentation| {
                                Some(presentation.display.extent) == slot.desired_extent
                            })
                    })
            }),
            "the desired 3D layout must resize its private candidate transactionally"
        );
        for token in omitted_tokens {
            self.retire_presentation(token)
                .expect("an omitted coordinated slot retains its private allocation");
        }
        self.diagnostics.peak_display_target_bytes = self
            .diagnostics
            .peak_display_target_bytes
            .max(display_peak_bytes);
        self.diagnostics.peak_control_and_residency_metadata_bytes = self
            .diagnostics
            .peak_control_and_residency_metadata_bytes
            .max(control_peak_bytes);
        let targets = PresentationTarget::ALL
            .into_iter()
            .filter(|target| desired_extents[target.index()].is_some())
            .map(|target| self.coordinated_layout_state(target))
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        Ok(CoordinatedLayoutReport { targets })
    }

    fn coordinated_layout_state(
        &self,
        target: PresentationTarget,
    ) -> Result<CoordinatedTargetLayoutState, WgpuRenderRuntimeError> {
        let token = self.frame_coordinator.front_token(target)?;
        let presentation = self
            .frame_coordinator
            .presentations
            .get(&token)
            .expect("a coordinated front slot owns one presentation");
        Ok(CoordinatedTargetLayoutState {
            target,
            extent: presentation.display.extent,
            device_generation: self.frame_coordinator.device_generation,
            texture_revision: presentation.texture_revision,
        })
    }

    #[cfg(test)]
    pub(super) fn coordinated_3d_private_extents_for_test(
        &self,
    ) -> (RenderExtent, RenderExtent, RenderExtent) {
        let slot = self.frame_coordinator.slot(PresentationTarget::ThreeD);
        let front = slot.front.expect("the 3D test front is configured");
        let candidate = slot.candidate.expect("the 3D test candidate is configured");
        (
            self.frame_coordinator.presentations[&front].display.extent,
            self.frame_coordinator.presentations[&candidate]
                .display
                .extent,
            slot.desired_extent
                .expect("the 3D test desired extent is configured"),
        )
    }

    pub(super) fn coordinated_target_texture_view(
        &self,
        target: PresentationTarget,
    ) -> Result<&wgpu::TextureView, WgpuRenderRuntimeError> {
        self.ensure_device_available()?;
        let token = self.frame_coordinator.front_token(target)?;
        Ok(&self
            .frame_coordinator
            .presentations
            .get(&token)
            .expect("a coordinated front slot owns one presentation")
            .display
            .color_view)
    }

    pub(super) fn coordinated_target_requires_execution(
        &self,
        target: PresentationTarget,
    ) -> Result<bool, WgpuRenderRuntimeError> {
        let slot = self.frame_coordinator.slot(target);
        if !slot.desired {
            return Err(WgpuRenderRuntimeError::CoordinatedTargetNotConfigured { target });
        }
        Ok(slot.color_retry_pending
            || [slot.front, slot.candidate]
                .into_iter()
                .flatten()
                .any(|token| {
                    self.coordinated_presentation_has_actionable_body_work(token)
                        || self
                            .frame_coordinator
                            .presentations
                            .get(&token)
                            .expect("a coordinated slot owns one presentation")
                            .hidden_volume_refinement
                            .as_ref()
                            .is_some_and(HiddenVolumeRefinementState::is_actionable)
                })
            || (target == PresentationTarget::ThreeD && self.hidden_refinement.has_result())
            || slot.front.is_some_and(|token| {
                let front_extent = self
                    .frame_coordinator
                    .presentations
                    .get(&token)
                    .expect("a coordinated front slot owns one presentation")
                    .display
                    .extent;
                slot.desired_extent != Some(front_extent)
            }))
    }

    pub(super) fn poll_coordinated_hidden_refinement_ready(
        &mut self,
        request: CoordinatedTargetRequest<'_>,
    ) -> Result<bool, WgpuRenderRuntimeError> {
        self.ensure_device_available()?;
        let slot = self.frame_coordinator.slot(request.target());
        if !slot.desired {
            return Err(WgpuRenderRuntimeError::CoordinatedTargetNotConfigured {
                target: request.target(),
            });
        }
        self.collect_hidden_refinement_results();
        let slot = self.frame_coordinator.slot(request.target());
        Ok([slot.front, slot.candidate]
            .into_iter()
            .flatten()
            .any(|token| {
                self.frame_coordinator
                    .presentations
                    .get(&token)
                    .and_then(|presentation| presentation.hidden_volume_refinement.as_ref())
                    .is_some_and(|refinement| {
                        refinement.matches(request)
                            && matches!(refinement.status, HiddenRefinementStatus::Complete { .. })
                    })
            }))
    }

    pub(super) fn coordinated_target_requires_prepared_presentation(
        &self,
        target: PresentationTarget,
        frame: FrameIdentity,
        extent: RenderExtent,
        requirements: &PreparedRenderRequirements,
    ) -> Result<bool, WgpuRenderRuntimeError> {
        self.coordinated_target_requires_presentation_matching(target, frame, extent, |rendered| {
            requirements.shares_resources_with(rendered)
                && requirements.prefetch_promoted() == rendered.prefetch_promoted()
        })
    }

    pub(super) fn coordinated_target_requires_render_presentation(
        &self,
        target: PresentationTarget,
        extent: RenderExtent,
        requirements: &RenderRequirements,
        volume_schedule: VolumeColorSchedule,
    ) -> Result<bool, WgpuRenderRuntimeError> {
        let body_requires_presentation = self.coordinated_target_requires_presentation_matching(
            target,
            requirements.frame(),
            extent,
            |rendered| {
                requirements.shares_resources_with(rendered)
                    && requirements.prefetch_promoted() == rendered.prefetch_promoted()
            },
        )?;
        if body_requires_presentation {
            return Ok(true);
        }
        let front = self.frame_coordinator.front_token(target)?;
        let presentation = self
            .frame_coordinator
            .presentations
            .get(&front)
            .expect("a coordinated front slot owns one presentation");
        Ok(volume_schedule_requires_exact_promotion(
            presentation.last_rendered_volume_schedule,
            volume_schedule,
        ))
    }

    fn coordinated_target_requires_presentation_matching(
        &self,
        target: PresentationTarget,
        frame: FrameIdentity,
        extent: RenderExtent,
        rendered_requirements_match: impl FnOnce(&RenderRequirements) -> bool,
    ) -> Result<bool, WgpuRenderRuntimeError> {
        let front = self.frame_coordinator.front_token(target)?;
        let presentation = self
            .frame_coordinator
            .presentations
            .get(&front)
            .expect("a coordinated front slot owns one presentation");
        let rendered_matches = presentation.last_rendered_frame == Some(frame)
            && presentation.display.extent == extent
            && presentation.last_progress.is_some()
            && !presentation.rendered_residency_freshness.requires_refresh()
            && presentation
                .last_rendered_requirements
                .as_ref()
                .is_some_and(rendered_requirements_match);
        // This request-aware query asks whether the visible front pixels
        // satisfy one presentation identity. Work owned only by the private
        // 3D candidate must not invalidate a matching preview front: the app
        // first adopts that front, then issues the exact continuation
        // schedule. Conflating the two leaves both sides waiting forever.
        Ok(!rendered_matches || self.coordinated_presentation_has_actionable_body_work(front))
    }

    fn coordinated_presentation_has_actionable_body_work(
        &self,
        token: PrivatePresentationId,
    ) -> bool {
        let presentation = self
            .frame_coordinator
            .presentations
            .get(&token)
            .expect("a coordinated slot owns every queried presentation");
        let relevant_offer = presentation
            .frame_state
            .as_ref()
            .is_some_and(|state| self.residency.has_relevant_offer(state.residency));
        let full_dirty_color = presentation
            .availability
            .as_ref()
            .is_some_and(FrameCoverage::is_full)
            && self.coordinated_presentation_is_dirty(token);
        relevant_offer || full_dirty_color
    }

    fn coordinated_presentation_is_dirty(&self, token: PrivatePresentationId) -> bool {
        let presentation = self
            .frame_coordinator
            .presentations
            .get(&token)
            .expect("a coordinated slot owns every queried presentation");
        presentation.frame_state.as_ref().is_some_and(|state| {
            presentation.last_rendered_frame != Some(state.frame)
                || presentation
                    .last_rendered_requirements
                    .as_ref()
                    .is_none_or(|rendered| {
                        let current = self.residency.requirements(state.residency);
                        !rendered.shares_resources_with(current)
                            || rendered.prefetch_promoted() != current.prefetch_promoted()
                    })
        }) || presentation.last_progress.is_none() && presentation.frame_state.is_some()
            || presentation.rendered_residency_freshness.requires_refresh()
    }

    fn coordinated_schedule_requires_render(
        &self,
        token: PrivatePresentationId,
        request: CoordinatedTargetRequest<'_>,
    ) -> bool {
        let presentation = self
            .frame_coordinator
            .presentations
            .get(&token)
            .expect("a coordinated slot owns every queried presentation");
        request.target() == PresentationTarget::ThreeD
            && matches!(request.intent().view(), RenderViewIntent::Volume { .. })
            && volume_schedule_requires_color_continuation(
                presentation.last_rendered_volume_schedule,
                presentation
                    .hidden_volume_refinement
                    .as_ref()
                    .is_some_and(|refinement| {
                        refinement.matches(request) && refinement.is_actionable()
                    }),
                request.volume_schedule(),
            )
    }

    fn prepare_presentation_allocation(
        &mut self,
        logical_target: PresentationTarget,
        extent: RenderExtent,
    ) -> Result<PreparedPresentationAllocation, WgpuRenderRuntimeError> {
        let token = self.frame_coordinator.allocate_private_presentation()?;
        let texture_revision = self.frame_coordinator.allocate_texture_revision()?;
        let display = create_display(&self.device, extent, self.config.validation_capture())?;
        let bindings = self.residency.bindings();
        let mut bind_group_creations = 0;
        let (control_buffer, render_bind_group, pick_bind_groups) =
            create_presentation_control_resources(
                &self.device,
                &self.render_bind_group_layout,
                &self.pick,
                bindings.payload_segments,
                bindings.directory_buffer,
                bindings.page_record_buffer,
                INITIAL_CONTROL_BYTES,
                &mut bind_group_creations,
            );
        Ok(PreparedPresentationAllocation {
            token,
            presentation: PresentationState {
                logical_target,
                texture_revision,
                frame_state: None,
                last_rendered_frame: None,
                last_rendered_volume_schedule: None,
                last_rendered_timepoint: None,
                last_rendered_volume: false,
                last_rendered_layers: Vec::new(),
                last_rendered_modes: Vec::new(),
                last_rendered_sampling: Vec::new(),
                availability: None,
                last_progress: None,
                last_rendered_requirements: None,
                rendered_residency_freshness: RenderedResidencyFreshness::Current,
                control_buffer,
                control_capacity: INITIAL_CONTROL_BYTES,
                render_bind_group,
                pick_bind_groups,
                display,
                pending_capture: None,
                hidden_volume_refinement: None,
            },
            bind_group_creations,
        })
    }

    fn retire_presentation(
        &mut self,
        token: PrivatePresentationId,
    ) -> Result<PrivatePresentationId, WgpuRenderRuntimeError> {
        if let Some(job_id) = self
            .frame_coordinator
            .presentations
            .get(&token)
            .and_then(|presentation| presentation.hidden_volume_refinement.as_ref())
            .map(|refinement| refinement.job_id)
        {
            self.hidden_refinement.cancel(job_id);
        }
        let retired = self
            .frame_coordinator
            .presentations
            .remove(&token)
            .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered)?;
        self.frame_coordinator
            .pending_residency_targets
            .remove(&token);
        let released_lease = retired.frame_state.as_ref().map(|state| state.residency);
        drop(retired);
        if let Some(lease) = released_lease {
            self.residency.release_frame_lease(lease);
        }
        Ok(token)
    }

    fn deactivate_presentation(
        &mut self,
        token: PrivatePresentationId,
    ) -> Result<(), WgpuRenderRuntimeError> {
        if let Some(job_id) = self
            .frame_coordinator
            .presentations
            .get(&token)
            .and_then(|presentation| presentation.hidden_volume_refinement.as_ref())
            .map(|refinement| refinement.job_id)
        {
            self.hidden_refinement.cancel(job_id);
        }
        let released_lease = {
            let presentation = self
                .frame_coordinator
                .presentations
                .get_mut(&token)
                .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered)?;
            let released = presentation.frame_state.take().map(|state| state.residency);
            presentation.availability = None;
            presentation.last_rendered_frame = None;
            presentation.last_rendered_volume_schedule = None;
            presentation.last_rendered_timepoint = None;
            presentation.last_rendered_volume = false;
            presentation.last_rendered_layers.clear();
            presentation.last_rendered_modes.clear();
            presentation.last_rendered_sampling.clear();
            presentation.last_progress = None;
            presentation.last_rendered_requirements = None;
            presentation.rendered_residency_freshness = RenderedResidencyFreshness::Current;
            presentation.pending_capture = None;
            presentation.hidden_volume_refinement = None;
            released
        };
        if let Some(lease) = released_lease {
            self.residency.release_frame_lease(lease);
        }
        self.frame_coordinator
            .pending_residency_targets
            .remove(&token);
        Ok(())
    }

    /// Retires every GPU fact scoped to the current dataset generation while
    /// preserving the device, pipelines, registered presentation targets,
    /// and lifetime diagnostics. Source replacement is a hard boundary: no
    /// resident key, prepared control state, or asynchronous result from the
    /// predecessor may be observed by the successor.
    pub(super) fn retire_dataset_generation(&mut self) {
        let _ = self.device.poll(wgpu::PollType::Poll);
        self.collect_completed_color_leases();
        self.frame_coordinator.latest_observed_frames = [None; 4];
        for slot in &mut self.frame_coordinator.slots {
            slot.color_retry_pending = false;
        }
        let presentation_tokens = self
            .frame_coordinator
            .presentations
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for token in presentation_tokens {
            self.deactivate_presentation(token)
                .expect("registered presentation remains valid during dataset retirement");
        }

        let mut clear_encoder =
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mirante4d-global-residency-retirement"),
                });
        let bindings = self.residency.bindings();
        clear_encoder.clear_buffer(bindings.directory_buffer, 0, None);
        clear_encoder.clear_buffer(bindings.page_record_buffer, 0, None);
        self.queue.submit([clear_encoder.finish()]);
        self.diagnostics.queue_submissions = self.diagnostics.queue_submissions.saturating_add(1);
        let retired_generation = self.frame_coordinator.residency_generation;
        self.residency
            .detach_frame_leases_for_generation(retired_generation);
        self.frame_coordinator.residency_generation = self
            .frame_coordinator
            .residency_generation
            .checked_add(1)
            .expect("one renderer cannot exhaust dataset generations");
        self.residency.reset_generation();
        self.refresh_payload_allocator_diagnostics();
        self.frame_coordinator.pending_residency_targets.clear();

        // Pending map callbacks retain their old WGPU resources until the
        // queue finishes, while replacement resources make every old ticket
        // immediately unknown to the successor generation.
        if self.timing.is_some() {
            self.timing = Some(create_timing_resources(&self.device, &self.queue));
        }
        self.pick.slots = create_pick_slots(&self.device);
        let bindings = self.residency.bindings();
        let (device, pick, presentations, diagnostics) = (
            &self.device,
            &self.pick,
            &mut self.frame_coordinator.presentations,
            &mut self.diagnostics,
        );
        for presentation in presentations.values_mut() {
            presentation.pick_bind_groups = create_presentation_pick_bind_groups(
                device,
                pick,
                bindings.payload_segments,
                bindings.directory_buffer,
                bindings.page_record_buffer,
                &presentation.control_buffer,
                &mut diagnostics.bind_group_creations,
            );
        }

        self.diagnostics.resident_payload_used_bytes = 0;
        self.diagnostics.empty_resident_metadata_records = 0;
        self.diagnostics.empty_resident_metadata_bytes = 0;
        self.diagnostics.last_gpu_batch_envelope_ns = None;
        self.diagnostics.last_gpu_render_pass_ns = None;
        self.diagnostics.last_cpu_timing_frame = None;
        self.diagnostics.last_cpu_planning_ns = None;
        self.diagnostics.last_cpu_queue_submit_ns = None;
    }

    fn active_display_bytes(&self) -> u64 {
        self.frame_coordinator
            .presentations
            .values()
            .map(|presentation| presentation.display.allocated_bytes)
            .sum()
    }

    fn active_control_bytes(&self) -> u64 {
        GLOBAL_RESIDENCY_METADATA_BYTES.saturating_add(
            self.frame_coordinator
                .presentations
                .values()
                .map(|presentation| {
                    presentation.control_capacity.saturating_add(
                        presentation
                            .hidden_volume_refinement
                            .as_ref()
                            .map_or(0, |refinement| refinement.control_capacity),
                    )
                })
                .sum::<u64>(),
        )
    }

    fn active_capture_bytes(&self) -> u64 {
        self.frame_coordinator
            .presentations
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
                category: GpuLedgerCategory::ControlAndResidencyMetadata,
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
    /// The proof is an unchanged requirement-body identity plus target-local
    /// residency freshness; unrelated global-directory changes may have
    /// occurred. This path performs no requirement walk, lease validation,
    /// allocator clone, eviction planning, or payload copy.
    fn coordinated_request_order<'a>(
        &self,
        active_target: PresentationTarget,
        requests: &[CoordinatedTargetRequest<'a>],
    ) -> Result<Vec<CoordinatedTargetRequest<'a>>, WgpuRenderRuntimeError> {
        let mut by_target = [None; 4];
        for request in requests {
            let target = request.target();
            if by_target[target.index()].replace(*request).is_some() {
                return Err(WgpuRenderRuntimeError::DuplicateCoordinatedTarget { target });
            }
            let slot = self.frame_coordinator.slot(target);
            if !slot.desired {
                return Err(WgpuRenderRuntimeError::CoordinatedTargetNotConfigured { target });
            }
            let view_matches = matches!(
                (target, request.intent().view()),
                (PresentationTarget::ThreeD, RenderViewIntent::Volume { .. })
                    | (
                        PresentationTarget::Xy | PresentationTarget::Xz | PresentationTarget::Yz,
                        RenderViewIntent::CrossSection(_)
                    )
            );
            if !view_matches {
                return Err(WgpuRenderRuntimeError::CoordinatedTargetViewMismatch { target });
            }
            let desired = slot
                .desired_extent
                .expect("a desired coordinated slot retains its requested extent");
            if request.intent().extent() != desired {
                return Err(WgpuRenderRuntimeError::CoordinatedTargetExtentMismatch {
                    target,
                    requested: request.intent().extent(),
                    desired,
                });
            }
        }

        let mut ordered = Vec::with_capacity(requests.len());
        if let Some(active) = by_target[active_target.index()] {
            ordered.push(active);
        }
        for target in [
            PresentationTarget::Xy,
            PresentationTarget::Xz,
            PresentationTarget::Yz,
        ] {
            if target != active_target
                && let Some(request) = by_target[target.index()]
            {
                ordered.push(request);
            }
        }
        if active_target != PresentationTarget::ThreeD
            && let Some(request) = by_target[PresentationTarget::ThreeD.index()]
        {
            ordered.push(request);
        }
        debug_assert_eq!(ordered.len(), requests.len());
        Ok(ordered)
    }

    fn presentation_matches_coordinated_request(
        &self,
        token: PrivatePresentationId,
        request: CoordinatedTargetRequest<'_>,
    ) -> bool {
        self.frame_coordinator
            .presentations
            .get(&token)
            .and_then(|presentation| presentation.frame_state.as_ref())
            .is_some_and(|state| {
                let current = self.residency.owned_requirements(state.residency);
                state.frame == request.intent().frame()
                    && current.shares_resources_with(request.requirements())
                    && current.prefetch_promoted() == request.requirements().prefetch_promoted()
            })
    }

    fn coordinated_execution_token(
        &mut self,
        request: CoordinatedTargetRequest<'_>,
    ) -> Result<(PrivatePresentationId, bool), WgpuRenderRuntimeError> {
        let target = request.target();
        let front = self.frame_coordinator.front_token(target)?;
        let front_is_provisional = self
            .frame_coordinator
            .presentations
            .get(&front)
            .expect("a coordinated front slot owns one presentation")
            .last_rendered_volume_schedule
            == Some(VolumeColorSchedule::InteractivePreview);
        let atomic_request_needs_private_exact_candidate = front_is_provisional
            && matches!(
                request.volume_schedule(),
                VolumeColorSchedule::AtomicRefinement { .. }
            );
        if target != PresentationTarget::ThreeD
            || (self.presentation_matches_coordinated_request(front, request)
                && !atomic_request_needs_private_exact_candidate)
            || self
                .frame_coordinator
                .presentations
                .get(&front)
                .expect("a coordinated front slot owns one presentation")
                .last_rendered_frame
                .is_none()
        {
            return Ok((front, false));
        }

        let slot = *self.frame_coordinator.slot(target);
        let desired_extent = slot
            .desired_extent
            .expect("a desired 3D target retains one extent");
        let candidate = slot
            .candidate
            .expect("a desired 3D layout eagerly owns one private candidate");
        assert_eq!(
            self.frame_coordinator
                .presentations
                .get(&candidate)
                .expect("the private 3D candidate remains allocated")
                .display
                .extent,
            desired_extent,
            "the transactional layout resizes the hidden 3D candidate eagerly; slot={slot:?}"
        );
        if !self.presentation_matches_coordinated_request(candidate, request) {
            let candidate_has_state = self
                .frame_coordinator
                .presentations
                .get(&candidate)
                .expect("the private 3D candidate remains allocated")
                .frame_state
                .is_some();
            if candidate_has_state {
                self.deactivate_presentation(candidate)?;
            }
        }
        Ok((candidate, true))
    }

    fn validate_coordinated_request_contract(
        &self,
        catalog: &DatasetCatalog,
        request: CoordinatedTargetRequest<'_>,
    ) -> Result<(), WgpuRenderRuntimeError> {
        if request.intent().frame() != request.requirements().frame() {
            return Err(WgpuRenderRuntimeError::FrameContractMismatch);
        }
        validate_extent(request.intent().extent())?;
        validate_extent(request.output_extent())?;
        let valid_schedule = match (
            request.target(),
            request.intent().view(),
            request.volume_schedule(),
        ) {
            (
                PresentationTarget::ThreeD,
                RenderViewIntent::Volume { .. },
                VolumeColorSchedule::Direct,
            ) => request.output_extent() == request.intent().extent(),
            (
                PresentationTarget::ThreeD,
                RenderViewIntent::Volume { .. },
                VolumeColorSchedule::InteractivePreview,
            ) => request.output_extent() == request.intent().extent(),
            (
                PresentationTarget::ThreeD,
                RenderViewIntent::Volume { .. },
                VolumeColorSchedule::AtomicRefinement {
                    strip_height_pixels,
                },
            ) => {
                strip_height_pixels != 0
                    && request.output_extent() == request.intent().extent()
                    && request.render_policy() == RetainedFrameRenderPolicy::ExactFrameOnly
            }
            (
                PresentationTarget::Xy | PresentationTarget::Xz | PresentationTarget::Yz,
                RenderViewIntent::CrossSection(_),
                VolumeColorSchedule::Direct,
            ) => request.output_extent() == request.intent().extent(),
            _ => false,
        };
        if !valid_schedule {
            return Err(WgpuRenderRuntimeError::InvalidVolumeColorSchedule {
                target: request.target(),
            });
        }
        let _ = catalog;
        if let Some(latest) =
            self.frame_coordinator.latest_observed_frames[request.target().index()]
            && request.intent().frame() < latest
        {
            return Err(WgpuRenderRuntimeError::StaleFrame {
                actual: request.intent().frame(),
                current: latest,
            });
        }
        Ok(())
    }

    /// Prepares an exact fully resident coordinated target without entering
    /// transfer or allocator planning.
    ///
    /// An identical retained requirement body, including a fully available
    /// prefetch promotion, reuses its proven coverage in O(1). A genuinely
    /// new body takes an instrumented linear fallback: validate it, prove every
    /// required key is already globally resident, and commit only the exact
    /// frame-lease pin delta. That fallback is not the steady interaction
    /// path, and any missing key returns to ordinary cold planning. The
    /// coordinator remains solely responsible for control publication, color
    /// recording, submission, and the atomic 3D front/candidate swap.
    fn try_prepare_resident_coordinated_frame(
        &mut self,
        presentation_token: PrivatePresentationId,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
    ) -> Result<Option<ResidencyPreparationReport>, WgpuRenderRuntimeError> {
        self.validate_presentation_activation(presentation_token)?;
        let current_frame_state = self
            .frame_coordinator
            .presentations
            .get(&presentation_token)
            .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered)?
            .frame_state
            .clone();
        let current_requirements = current_frame_state
            .as_ref()
            .map(|state| self.residency.owned_requirements(state.residency));
        let current_residency = current_frame_state.as_ref().map(|state| state.residency);

        // Reuse the same scientific and transition validator as the cold path.
        // A retained body makes its potentially linear body checks O(1), and
        // the empty lease slice proves that no payload contract is skipped.
        self.validate_inputs(
            current_frame_state.as_ref(),
            current_requirements.as_ref(),
            catalog,
            intent,
            requirements,
            &[],
        )?;

        if let Some((state, current)) = current_frame_state
            .as_ref()
            .zip(current_requirements.as_ref())
            && current.shares_resources_with(requirements)
            && self.residency.has_relevant_offer(state.residency)
        {
            return Ok(None);
        }

        let retained = self
            .frame_coordinator
            .presentations
            .values()
            .find_map(|presentation| {
                let state = presentation.frame_state.as_ref()?;
                let retained = self.residency.requirements(state.residency);
                if !retained.shares_resources_with(requirements)
                    || (retained.prefetch_promoted() != requirements.prefetch_promoted()
                        && current_residency != Some(state.residency))
                    || self.residency.has_relevant_offer(state.residency)
                {
                    return None;
                }
                let coverage = presentation
                    .availability
                    .as_ref()?
                    .rebind(requirements)
                    .ok()?;
                if !coverage.is_full() {
                    return None;
                }
                let retained_progress = presentation
                    .last_rendered_requirements
                    .as_ref()
                    .filter(|rendered| {
                        rendered.shares_resources_with(requirements)
                            && rendered.prefetch_promoted() == requirements.prefetch_promoted()
                    })
                    .and(presentation.last_progress.as_ref())
                    .cloned();
                Some((state.residency, coverage, retained_progress))
            });
        let (source_residency, coverage, retained_progress) = match retained {
            Some((source, coverage, progress)) => (Some(source), coverage, progress),
            None => {
                let (available, checks) = self.residency.initial_available_keys(requirements);
                self.diagnostics.cold_coverage_membership_checks = self
                    .diagnostics
                    .cold_coverage_membership_checks
                    .saturating_add(checks);
                self.diagnostics.cold_coverage_resident_matches = self
                    .diagnostics
                    .cold_coverage_resident_matches
                    .saturating_add(available.len() as u64);
                let coverage = FrameCoverage::from_available(requirements, &available)
                    .map_err(|_| WgpuRenderRuntimeError::FrameProgressContract)?;
                if !coverage.is_full() {
                    return Ok(None);
                }
                (None, coverage, None)
            }
        };

        // Preserve a truthful full retained limitation when one exists.
        // Progressive pixels cannot describe newly complete availability, so
        // that case is rebuilt as exact from the proven full bitmap.
        let progress = match retained_progress {
            Some(progress) => {
                let rebound = progress
                    .rebind(requirements)
                    .map_err(|_| WgpuRenderRuntimeError::FrameProgressContract)?;
                if rebound.coverage().is_full() {
                    rebound
                } else {
                    build_progress(coverage.clone(), false, false)?
                        .expect("full retained availability is always useful")
                }
            }
            None => build_progress(coverage.clone(), false, false)?
                .expect("full retained availability is always useful"),
        };

        // Everything that can return an ordinary error has completed. Commit
        // only opaque lease/frame/coverage facts; no queue or allocator state
        // is touched here.
        let residency = match current_frame_state {
            Some(current) => self
                .residency
                .replace_frame_lease(current.residency, requirements.clone()),
            None => match source_residency {
                Some(source) => self
                    .residency
                    .clone_frame_lease_for_presentation_rebind(source, requirements.clone()),
                None => self
                    .residency
                    .commit_frame_lease(None, requirements.clone()),
            },
        };
        let presentation = self
            .frame_coordinator
            .presentations
            .get_mut(&presentation_token)
            .expect("resident preparation retains its validated presentation");
        presentation.frame_state = Some(FrameState {
            frame: requirements.frame(),
            residency,
        });
        presentation.availability = Some(coverage);

        Ok(Some(ResidencyPreparationReport {
            frame: intent.frame(),
            progress: Some(progress),
            visited_resources: 0,
            uploaded_resources: 0,
            payload_upload_bytes: 0,
            command_buffers: 0,
            queue_submissions: 0,
            deferred_by_backpressure: false,
            newly_resident_keys: Box::new([]),
            evicted_keys: Box::new([]),
        }))
    }

    pub(super) fn execute_coordinated_frame(
        &mut self,
        catalog: &DatasetCatalog,
        active_target: PresentationTarget,
        targets: &[CoordinatedTargetRequest<'_>],
    ) -> Result<CoordinatedFrameExecutionReport, WgpuRenderRuntimeError> {
        self.pipelines
            .ensure_capability(PipelineCapability::InitialRender)?;
        self.ensure_device_available()?;
        self.collect_hidden_refinement_results();
        if !self.residency.catalog_is_active(catalog) {
            return Err(WgpuRenderRuntimeError::PayloadContractMismatch);
        }
        let ordered = self.coordinated_request_order(active_target, targets)?;
        for request in &ordered {
            self.validate_coordinated_request_contract(catalog, *request)?;
        }
        if ordered.is_empty() {
            self.frame_coordinator.last_recorded_targets.clear();
            return Ok(CoordinatedFrameExecutionReport {
                targets: Box::new([]),
                recorded_targets: Box::new([]),
                residency_queue_submissions: 0,
                color_queue_submissions: 0,
                cpu_timing: None,
                gpu_timing: None,
            });
        }
        for request in &ordered {
            let latest =
                &mut self.frame_coordinator.latest_observed_frames[request.target().index()];
            if latest.is_none_or(|current| request.intent().frame() > current) {
                *latest = Some(request.intent().frame());
            }
        }

        let mut plans = Vec::with_capacity(ordered.len());
        for request in ordered {
            let (token, candidate) = self.coordinated_execution_token(request)?;
            if matches!(
                request.volume_schedule(),
                VolumeColorSchedule::AtomicRefinement { .. }
            ) && !candidate
            {
                let presentation = self
                    .frame_coordinator
                    .presentations
                    .get(&token)
                    .expect("a coordinated execution token retains its presentation");
                let already_presented = presentation.last_rendered_frame
                    == Some(request.intent().frame())
                    && presentation.display.extent == request.intent().extent()
                    && presentation.last_progress.as_ref().is_some_and(|progress| {
                        progress.completeness() == FrameCompleteness::Exact
                    })
                    && presentation
                        .last_rendered_requirements
                        .as_ref()
                        .is_some_and(|rendered| {
                            rendered.shares_resources_with(request.requirements())
                                && rendered.prefetch_promoted()
                                    == request.requirements().prefetch_promoted()
                        });
                if !already_presented {
                    return Err(WgpuRenderRuntimeError::InvalidVolumeColorSchedule {
                        target: request.target(),
                    });
                }
            }
            plans.push(CoordinatedExecutionPlan {
                request,
                token,
                candidate,
            });
        }

        // Materialize every fallible color contract before the one permitted
        // cold residency submission. Requirement validation below prepares
        // every non-selected target first and the selected target last, so no
        // later target can turn a published transfer into an outer error.
        self.pipelines.initial()?;
        let directory_slots = u32::try_from(GLOBAL_DIRECTORY_SLOTS)
            .map_err(|_| WgpuRenderRuntimeError::ControlCapacityExceeded)?;
        let mut preflight_controls = Vec::with_capacity(plans.len());
        let mut replaces_displays = Vec::with_capacity(plans.len());
        let mut preflight_control_bytes = 0_u64;
        let mut replacement_display_bytes = 0_u64;
        let mut potential_capture_bytes = 0_u64;
        for plan in &plans {
            let presentation = self
                .frame_coordinator
                .presentations
                .get(&plan.token)
                .expect("a coordinated execution plan retains its allocation");
            let control = build_target_control(
                catalog,
                self.residency.resource_grids()?,
                plan.request.intent(),
                plan.request.requirements(),
                directory_slots,
            )?;
            if control.len() as u64 > presentation.control_capacity {
                return Err(WgpuRenderRuntimeError::ControlCapacityExceeded);
            }
            preflight_control_bytes = preflight_control_bytes
                .checked_add(control.len() as u64)
                .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
            let replaces_display = presentation.display.extent != plan.request.intent().extent();
            if replaces_display {
                replacement_display_bytes = replacement_display_bytes
                    .checked_add(display_allocation_bytes(
                        plan.request.intent().extent(),
                        self.config.validation_capture(),
                    )?)
                    .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
            }
            let potentially_dirty = self.coordinated_presentation_is_dirty(plan.token)
                || self.coordinated_schedule_requires_render(plan.token, plan.request)
                || self
                    .frame_coordinator
                    .pending_residency_targets
                    .contains(&plan.token)
                || !self.presentation_matches_coordinated_request(plan.token, plan.request)
                || replaces_display;
            if potentially_dirty
                && self.config.validation_capture()
                && plan.request.volume_schedule() != VolumeColorSchedule::InteractivePreview
            {
                potential_capture_bytes = potential_capture_bytes
                    .checked_add(capture_allocation_bytes(plan.request.intent().extent())?)
                    .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
            }
            preflight_controls.push(Some(control));
            replaces_displays.push(replaces_display);
        }
        if preflight_control_bytes > MAX_CONTROL_UPLOAD_BYTES {
            return Err(WgpuRenderRuntimeError::ControlCapacityExceeded);
        }
        let preflight_display_peak_bytes = self
            .active_display_bytes()
            .checked_add(replacement_display_bytes)
            .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
        let preflight_capture_peak_bytes = self
            .active_capture_bytes()
            .checked_add(potential_capture_bytes)
            .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
        self.validate_other_capacity(
            self.active_control_bytes(),
            preflight_display_peak_bytes,
            preflight_capture_peak_bytes,
        )?;
        let mut preflight_displays = Vec::with_capacity(plans.len());
        let mut preflight_texture_revisions = Vec::with_capacity(plans.len());
        for (plan, replaces_display) in plans.iter().zip(replaces_displays) {
            preflight_displays.push(
                replaces_display
                    .then(|| {
                        create_display(
                            &self.device,
                            plan.request.intent().extent(),
                            self.config.validation_capture(),
                        )
                    })
                    .transpose()?,
            );
            preflight_texture_revisions.push(
                replaces_display
                    .then(|| self.frame_coordinator.allocate_texture_revision())
                    .transpose()?,
            );
        }

        // One nonblocking progress collection per coordinated cutoff.
        let poll_result = self.device.poll(wgpu::PollType::Poll);
        self.ensure_device_available()?;
        poll_result.map_err(|_| WgpuRenderRuntimeError::BackendValidation)?;
        self.collect_completed_color_leases();
        let staging_refresh = self.residency.refresh_transfers();
        self.ensure_device_available()?;
        staging_refresh?;
        self.collect_gpu_timings()?;

        // Select at most the highest-priority target with useful cold work.
        let mut cold_selection = None;
        for (index, plan) in plans.iter().enumerate() {
            let current = self
                .frame_coordinator
                .presentations
                .get(&plan.token)
                .and_then(|presentation| presentation.frame_state.as_ref())
                .map(|state| state.residency);
            let selected = self
                .residency
                .select_offers_for_execution(current, plan.request.requirements())?;
            if !selected.leases.is_empty() {
                cold_selection = Some((index, selected.leases));
                break;
            }
        }
        let color_slot_available = self
            .frame_coordinator
            .in_flight_color_cutoffs
            .load(Ordering::Acquire)
            < MAX_IN_FLIGHT_COLOR_CUTOFFS;
        let reserved_submission_slots = usize::from(cold_selection.is_some());
        let current_in_flight = self.in_flight_submissions.load(Ordering::Acquire);
        if current_in_flight
            .checked_add(reserved_submission_slots)
            .is_none_or(|required| required > MAX_IN_FLIGHT_SUBMISSIONS)
        {
            self.diagnostics.backpressure_deferrals =
                self.diagnostics.backpressure_deferrals.saturating_add(1);
            for plan in &plans {
                self.frame_coordinator
                    .slot_mut(plan.request.target())
                    .color_retry_pending = true;
            }
            let reports = plans
                .into_iter()
                .map(|plan| {
                    let state = self
                        .coordinated_layout_state(plan.request.target())
                        .expect("a validated coordinated plan retains its desired front");
                    CoordinatedTargetExecutionReport {
                        target: plan.request.target(),
                        device_generation: state.device_generation,
                        texture_revision: state.texture_revision,
                        frame: plan.request.intent().frame(),
                        progress: None,
                        presented: false,
                        visited_resources: 0,
                        uploaded_resources: 0,
                        payload_upload_bytes: 0,
                        control_upload_bytes: 0,
                        residency_command_buffers: 0,
                        residency_queue_submissions: 0,
                        deferred_by_backpressure: true,
                        volume_refinement: None,
                        validation_capture: None,
                        newly_resident_keys: Box::new([]),
                        evicted_keys: Box::new([]),
                    }
                })
                .collect::<Vec<_>>()
                .into_boxed_slice();
            self.frame_coordinator.last_recorded_targets.clear();
            return Ok(CoordinatedFrameExecutionReport {
                targets: reports,
                recorded_targets: Box::new([]),
                residency_queue_submissions: 0,
                color_queue_submissions: 0,
                cpu_timing: None,
                gpu_timing: None,
            });
        }
        for plan in &plans {
            self.frame_coordinator
                .slot_mut(plan.request.target())
                .color_retry_pending = false;
        }

        let selected_index = cold_selection
            .as_ref()
            .map(|(selected_index, _)| *selected_index);
        let mut prepared_reports = (0..plans.len()).map(|_| None).collect::<Vec<_>>();
        let mut resident_prepared = vec![false; plans.len()];
        let mut selected_keys = Vec::new();
        let preparation_order = (0..plans.len())
            .filter(|index| Some(*index) != selected_index)
            .chain(selected_index);
        for index in preparation_order {
            let plan = plans[index];
            let selected = cold_selection
                .as_ref()
                .filter(|(selected_index, _)| *selected_index == index)
                .map(|(_, leases)| leases.as_slice())
                .unwrap_or(&[]);
            if !selected.is_empty() {
                selected_keys.extend(selected.iter().map(|lease| lease.key()));
            }
            let lease_refs = selected.iter().map(Arc::as_ref).collect::<Vec<_>>();
            let report = if selected.is_empty() {
                match self.try_prepare_resident_coordinated_frame(
                    plan.token,
                    catalog,
                    plan.request.intent(),
                    plan.request.requirements(),
                )? {
                    Some(report) => {
                        resident_prepared[index] = true;
                        report
                    }
                    None => self.prepare_coordinated_residency(
                        plan.token,
                        catalog,
                        plan.request.intent(),
                        plan.request.requirements(),
                        &lease_refs,
                    )?,
                }
            } else {
                self.prepare_coordinated_residency(
                    plan.token,
                    catalog,
                    plan.request.intent(),
                    plan.request.requirements(),
                    &lease_refs,
                )?
            };
            prepared_reports[index] = Some(report);
        }
        let mut prepared = plans
            .iter()
            .copied()
            .zip(prepared_reports)
            .map(|(plan, report)| {
                (
                    plan,
                    report.expect("every coordinated plan is prepared exactly once"),
                )
            })
            .collect::<Vec<_>>();
        let selected_completed = cold_selection
            .as_ref()
            .is_some_and(|(index, _)| !prepared[*index].1.deferred_by_backpressure);
        if selected_completed {
            self.residency.complete_resident_offers(selected_keys);
            let active = self
                .frame_coordinator
                .presentations
                .keys()
                .copied()
                .collect::<Vec<_>>();
            for token in active {
                self.refresh_pending_residency_target(token);
            }
        }

        let color_cpu_start = self.cpu_timing_start();
        let mut color_passes = Vec::with_capacity(prepared.len());
        let mut hidden_passes = Vec::with_capacity(1);
        let mut volume_refinement_by_report = vec![None; prepared.len()];
        let mut control_bytes = 0_u64;
        let mut replacement_display_bytes = 0_u64;
        let mut capture_bytes = 0_u64;
        for (report_index, (plan, report)) in prepared.iter().enumerate() {
            if report.deferred_by_backpressure {
                continue;
            }
            let Some(progress) = report.progress.clone() else {
                continue;
            };
            let policy_allows = retained_frame_policy_allows_render(
                plan.request.render_policy(),
                Some(progress.completeness()),
            );
            if !policy_allows
                || (plan.candidate && progress.completeness() != FrameCompleteness::Exact)
            {
                continue;
            }
            let presentation = self
                .frame_coordinator
                .presentations
                .get(&plan.token)
                .expect("a coordinated execution plan retains its allocation");
            let same_rendered_body = presentation
                .last_rendered_requirements
                .as_ref()
                .is_some_and(|rendered| {
                    rendered.shares_resources_with(plan.request.requirements())
                        && rendered.prefetch_promoted()
                            == plan.request.requirements().prefetch_promoted()
                });
            let regresses_same_frame = presentation.last_rendered_frame
                == Some(plan.request.intent().frame())
                && same_rendered_body
                && !progress.coverage().is_full()
                && presentation
                    .rendered_residency_freshness
                    .required_resource_was_removed();
            let dirty = presentation.last_rendered_frame != Some(plan.request.intent().frame())
                || presentation.last_progress.is_none()
                || presentation
                    .last_rendered_requirements
                    .as_ref()
                    .is_none_or(|rendered| {
                        !rendered.shares_resources_with(plan.request.requirements())
                            || rendered.prefetch_promoted()
                                != plan.request.requirements().prefetch_promoted()
                    })
                || presentation.rendered_residency_freshness.requires_refresh()
                || presentation.display.extent != plan.request.intent().extent()
                || self.coordinated_schedule_requires_render(plan.token, plan.request);
            if regresses_same_frame || !dirty {
                continue;
            }
            let mut control = preflight_controls[report_index]
                .take()
                .expect("every coordinated target has one preflighted control body");
            set_control_full_coverage(&mut control, progress.coverage().is_full())
                .expect("the preflighted target-control ABI retains its coverage word");
            set_control_full_resource_fast_paths(
                &mut control,
                catalog,
                plan.request.intent(),
                plan.request.requirements(),
                &self.residency,
            )?;
            control_bytes = control_bytes.saturating_add(control.len() as u64);
            let replaces_display = presentation.display.extent != plan.request.intent().extent();
            if replaces_display {
                replacement_display_bytes = replacement_display_bytes.saturating_add(
                    display_allocation_bytes(
                        plan.request.intent().extent(),
                        self.config.validation_capture(),
                    )
                    .expect("coordinated display bytes were preflighted"),
                );
            }
            let new_display = preflight_displays[report_index].take();
            let new_texture_revision = preflight_texture_revisions[report_index].take();
            debug_assert_eq!(new_display.is_some(), new_texture_revision.is_some());
            match planned_color_work(presentation, *plan) {
                PlannedColorWork::Main {
                    region,
                    progress: volume_refinement,
                    publishes_frame,
                } => {
                    let captures_validation = publishes_frame
                        && plan.request.volume_schedule()
                            != VolumeColorSchedule::InteractivePreview;
                    if self.config.validation_capture() && captures_validation {
                        capture_bytes = capture_bytes.saturating_add(
                            capture_allocation_bytes(plan.request.intent().extent())
                                .expect("coordinated capture bytes were preflighted"),
                        );
                    }
                    color_passes.push(CoordinatedColorPass {
                        plan: *plan,
                        report_index,
                        progress,
                        control,
                        new_display,
                        new_texture_revision,
                        pending_capture: None,
                        capture_ticket: None,
                        render_region: region,
                        volume_refinement,
                        publishes_frame,
                        captures_validation,
                    });
                }
                PlannedColorWork::StartHidden {
                    initial_batch_rows,
                    progress: volume_refinement,
                } => {
                    volume_refinement_by_report[report_index] = Some(volume_refinement);
                    hidden_passes.push(CoordinatedHiddenPass {
                        plan: *plan,
                        report_index,
                        control,
                        new_display,
                        new_texture_revision,
                        initial_batch_rows,
                    });
                }
                PlannedColorWork::HiddenRunning {
                    progress: volume_refinement,
                } => {
                    volume_refinement_by_report[report_index] = Some(volume_refinement);
                }
                PlannedColorWork::HiddenFailed => {
                    return Err(WgpuRenderRuntimeError::HiddenRefinementFailed);
                }
            }
        }
        let display_peak_bytes = self
            .active_display_bytes()
            .checked_add(replacement_display_bytes)
            .expect("coordinated display peak was preflighted");
        let capture_peak_bytes = self
            .active_capture_bytes()
            .checked_add(capture_bytes)
            .expect("coordinated capture peak was preflighted");

        let color_submission_available = color_slot_available
            && self.in_flight_submissions.load(Ordering::Acquire) < MAX_IN_FLIGHT_SUBMISSIONS;
        if !color_submission_available {
            for pass in &color_passes {
                prepared[pass.report_index].1.deferred_by_backpressure = true;
                self.frame_coordinator
                    .slot_mut(pass.plan.request.target())
                    .color_retry_pending = true;
            }
            if !color_passes.is_empty() {
                self.diagnostics.backpressure_deferrals =
                    self.diagnostics.backpressure_deferrals.saturating_add(1);
            }
            color_passes.clear();
        }

        let mut color_control_bytes_by_report = vec![0_u64; prepared.len()];
        for pass in &color_passes {
            color_control_bytes_by_report[pass.report_index] = pass.control.len() as u64;
        }
        if !hidden_passes.is_empty() {
            let hidden_control_bytes = hidden_passes
                .iter()
                .map(|pass| align_copy(pass.control.len() as u64).max(INITIAL_CONTROL_BYTES))
                .sum::<u64>();
            self.validate_other_capacity(
                self.active_control_bytes()
                    .checked_add(hidden_control_bytes)
                    .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?,
                display_peak_bytes,
                capture_peak_bytes,
            )?;
            let pipelines = self
                .pipelines
                .initial()
                .expect("coordinated pipelines were preflighted");
            for mut pass in hidden_passes {
                let job_id = self.hidden_refinement.allocate_job()?;
                let control_capacity =
                    align_copy(pass.control.len() as u64).max(INITIAL_CONTROL_BYTES);
                let control_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("mirante4d-hidden-refinement-control"),
                    size: control_capacity,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let mut bind_group_creations = 0_u64;
                let render_bind_group = create_presentation_render_bind_group(
                    &self.device,
                    &self.render_bind_group_layout,
                    &self.residency.payload_segments,
                    &self.residency.directory_buffer,
                    &self.residency.page_record_buffer,
                    &control_buffer,
                    &mut bind_group_creations,
                );
                self.queue
                    .write_buffer(&control_buffer, 0, pass.control.as_slice());
                self.ensure_device_available()?;

                let completed_rows = Arc::new(AtomicU32::new(0));
                let presentation = self
                    .frame_coordinator
                    .presentations
                    .get_mut(&pass.plan.token)
                    .expect("a coordinated hidden pass retains its allocation");
                if let Some(previous) = presentation.hidden_volume_refinement.take() {
                    self.hidden_refinement.cancel(previous.job_id);
                }
                if let Some(display) = pass.new_display.take() {
                    presentation.display = display;
                }
                if let Some(revision) = pass.new_texture_revision {
                    presentation.texture_revision = revision;
                }
                presentation.hidden_volume_refinement = Some(HiddenVolumeRefinementState {
                    job_id,
                    frame: pass.plan.request.intent().frame(),
                    requirements: pass.plan.request.requirements().clone(),
                    extent: pass.plan.request.intent().extent(),
                    completed_rows: Arc::clone(&completed_rows),
                    _control_buffer: control_buffer,
                    _render_bind_group: render_bind_group.clone(),
                    control_capacity,
                    status: HiddenRefinementStatus::Running,
                });
                let job = HiddenRefinementJob {
                    id: job_id,
                    pipeline: pipelines.for_intent(pass.plan.request.intent()).clone(),
                    bind_group: render_bind_group,
                    color_view: presentation.display.color_view.clone(),
                    fact_view: presentation.display.fact_view.clone(),
                    extent: presentation.display.extent,
                    initial_batch_rows: pass.initial_batch_rows,
                    completed_rows,
                };
                self.hidden_refinement.replace(job);
                self.frame_coordinator
                    .slot_mut(pass.plan.request.target())
                    .color_retry_pending = false;
                self.diagnostics.hidden_refinement_jobs_started = self
                    .diagnostics
                    .hidden_refinement_jobs_started
                    .saturating_add(1);
                self.diagnostics.control_buffer_allocations = self
                    .diagnostics
                    .control_buffer_allocations
                    .saturating_add(1);
                self.diagnostics.control_buffer_allocation_bytes = self
                    .diagnostics
                    .control_buffer_allocation_bytes
                    .saturating_add(control_capacity);
                self.diagnostics.bind_group_creations = self
                    .diagnostics
                    .bind_group_creations
                    .saturating_add(bind_group_creations);
                color_control_bytes_by_report[pass.report_index] = pass.control.len() as u64;
            }
        }
        let mut color_cpu_timing = None;
        let mut color_gpu_timing = None;
        if !color_passes.is_empty() {
            let timed_pass = color_passes
                .iter()
                .position(|pass| {
                    pass.render_region.height != 0 && pass.plan.request.measure_gpu_timing()
                })
                .or_else(|| {
                    color_passes.iter().position(|pass| {
                        pass.render_region.height != 0
                            && pass.plan.request.target() == active_target
                    })
                })
                .or_else(|| {
                    color_passes
                        .iter()
                        .position(|pass| pass.render_region.height != 0)
                });
            let timing_plan = timed_pass.and_then(|timed_pass| {
                self.timing.as_ref().and_then(|timing| {
                    timing
                        .slots
                        .iter()
                        .position(|slot| matches!(slot.state, TimingSlotState::Free))
                        .map(|slot| {
                            let request = color_passes[timed_pass].plan.request;
                            let ticket = GpuTimingTicket {
                                id: self.next_timing,
                                target: request.target(),
                                generation: request.intent().frame(),
                                display_generation: request.display_generation(),
                                pass_kind: request.intent().view().pass_kind(),
                            };
                            let query_base = u32::try_from(slot)
                                .expect("bounded timing slot fits u32")
                                * TIMING_QUERY_WORDS;
                            TimingPlan {
                                slot,
                                ticket,
                                queries: TimingQueryLayout::new(
                                    query_base,
                                    timing.encoder_timestamps,
                                    true,
                                ),
                            }
                        })
                })
            });
            color_gpu_timing = timing_plan.map(|plan| plan.ticket);
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mirante4d-coordinated-color-cutoff"),
                });
            let pipelines = self
                .pipelines
                .initial()
                .expect("coordinated pipelines were preflighted");
            if let Some((beginning, _)) = timing_plan.and_then(|plan| plan.queries.batch_envelope) {
                encoder.write_timestamp(
                    &self
                        .timing
                        .as_ref()
                        .expect("a timing plan retains timing resources")
                        .query_set,
                    beginning,
                );
            }
            for (pass_index, pass) in color_passes.iter_mut().enumerate() {
                let presentation = self
                    .frame_coordinator
                    .presentations
                    .get(&pass.plan.token)
                    .expect("a coordinated color pass retains its allocation");
                let display = pass.new_display.as_ref().unwrap_or(&presentation.display);
                if pass.render_region.height != 0 {
                    encode_render_pass(
                        &mut encoder,
                        pipelines.for_intent(pass.plan.request.intent()),
                        &presentation.render_bind_group,
                        display,
                        pass.render_region,
                        if Some(pass_index) == timed_pass {
                            timing_plan.and_then(|plan| plan.queries.render_pass).map(
                                |(beginning, end)| {
                                    (
                                        &self
                                            .timing
                                            .as_ref()
                                            .expect("a timing plan retains timing resources")
                                            .query_set,
                                        beginning,
                                        end,
                                    )
                                },
                            )
                        } else {
                            None
                        },
                    );
                }
                if self.config.validation_capture() && pass.captures_validation {
                    let pending = encode_capture(
                        &self.device,
                        &mut encoder,
                        self.next_capture,
                        pass.plan.token,
                        pass.plan.request.intent().frame(),
                        display,
                    )
                    .expect("coordinated capture layout and fact target were preflighted");
                    let texture_revision = pass
                        .new_texture_revision
                        .unwrap_or(presentation.texture_revision);
                    pass.capture_ticket = Some(CoordinatedValidationCaptureTicket {
                        target: pass.plan.request.target(),
                        device_generation: self.frame_coordinator.device_generation,
                        texture_revision,
                        residency_generation: self.frame_coordinator.residency_generation,
                        inner: pending.ticket,
                    });
                    pass.pending_capture = Some(pending);
                    self.next_capture = self.next_capture.saturating_add(1);
                }
            }
            if let Some((_, end)) = timing_plan.and_then(|plan| plan.queries.batch_envelope) {
                encoder.write_timestamp(
                    &self
                        .timing
                        .as_ref()
                        .expect("a timing plan retains timing resources")
                        .query_set,
                    end,
                );
            }
            if let Some(plan) = timing_plan {
                let timing = self
                    .timing
                    .as_ref()
                    .expect("a timing plan retains timing resources");
                let query_base = u32::try_from(plan.slot).expect("bounded timing slot fits u32")
                    * TIMING_QUERY_WORDS;
                let resolve_offset = plan.slot as u64 * TIMING_RESOLVE_STRIDE;
                encoder.resolve_query_set(
                    &timing.query_set,
                    query_base..query_base + plan.queries.query_count,
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
            self.ensure_device_available()?;
            for pass in &color_passes {
                self.queue.write_buffer(
                    &self
                        .frame_coordinator
                        .presentations
                        .get(&pass.plan.token)
                        .expect("a coordinated color pass retains its allocation")
                        .control_buffer,
                    0,
                    &pass.control,
                );
            }
            let completion_leases = color_passes
                .iter()
                .map(|pass| {
                    let source = self
                        .frame_coordinator
                        .presentations
                        .get(&pass.plan.token)
                        .and_then(|presentation| presentation.frame_state.as_ref())
                        .expect("a coordinated color pass retains one frame lease")
                        .residency;
                    self.residency.clone_frame_lease_for_completion(source)
                })
                .collect::<Vec<_>>();
            let color_cutoff = self.frame_coordinator.allocate_color_cutoff();
            let previous = self
                .frame_coordinator
                .in_flight_color_leases
                .insert(color_cutoff, completion_leases);
            debug_assert!(previous.is_none());
            let cpu_planning_ns = color_cpu_start.as_ref().map(elapsed_nanoseconds);
            let cpu_submit_start = color_cpu_start.as_ref().map(|_| Instant::now());
            self.queue.submit([command_buffer]);
            let resident_navigation_frames = color_passes
                .iter()
                .filter(|pass| resident_prepared[pass.report_index])
                .count() as u64;
            self.diagnostics.retained_navigation_frames = self
                .diagnostics
                .retained_navigation_frames
                .saturating_add(resident_navigation_frames);
            let cpu_queue_submit_ns = cpu_submit_start.as_ref().map(elapsed_nanoseconds);
            color_cpu_timing = cpu_planning_ns
                .zip(cpu_queue_submit_ns)
                .map(|(planning, queue_submit)| CpuFrameTiming::new(planning, queue_submit));
            let in_flight = Arc::clone(&self.in_flight_submissions);
            let in_flight_color = Arc::clone(&self.frame_coordinator.in_flight_color_cutoffs);
            let completed_color_cutoffs =
                Arc::clone(&self.frame_coordinator.completed_color_cutoffs);
            let residency_generation = self.frame_coordinator.residency_generation;
            let submitted = in_flight.fetch_add(1, Ordering::AcqRel) + 1;
            let submitted_color = in_flight_color.fetch_add(1, Ordering::AcqRel) + 1;
            debug_assert!(submitted_color <= MAX_IN_FLIGHT_COLOR_CUTOFFS);
            self.diagnostics.peak_in_flight_color_cutoffs = self
                .diagnostics
                .peak_in_flight_color_cutoffs
                .max(submitted_color);
            self.queue.on_submitted_work_done(move || {
                completed_color_cutoffs
                    .lock()
                    .expect("the color-completion queue is never poisoned")
                    .push(CompletedColorCutoff {
                        residency_generation,
                        cutoff: color_cutoff,
                    });
                in_flight.fetch_sub(1, Ordering::AcqRel);
                in_flight_color.fetch_sub(1, Ordering::AcqRel);
            });
            self.diagnostics.current_in_flight_submissions = submitted;
            self.diagnostics.peak_in_flight_submissions =
                self.diagnostics.peak_in_flight_submissions.max(submitted);
            for pass in &color_passes {
                if let Some(pending) = pass.pending_capture.as_ref() {
                    pending.start_map();
                }
            }
            if let Some(plan) = timing_plan {
                let mapped = Arc::new(Mutex::new(None));
                let callback = Arc::clone(&mapped);
                let timing = self
                    .timing
                    .as_mut()
                    .expect("a timing plan retains timing resources");
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
                    render_pass_timestamps: true,
                };
                self.next_timing = self.next_timing.saturating_add(1);
            }
        }

        let submitted_targets = color_passes
            .iter()
            .map(|pass| pass.plan.request.target())
            .collect::<Vec<_>>();
        let mut presented_by_report = vec![false; prepared.len()];
        let mut capture_by_report = vec![None; prepared.len()];
        for mut pass in color_passes {
            let frame_lease = self
                .frame_coordinator
                .presentations
                .get(&pass.plan.token)
                .and_then(|presentation| presentation.frame_state.as_ref())
                .expect("a submitted coordinated pass retains one frame lease")
                .residency;
            self.residency
                .touch_frame_lease(frame_lease, pass.plan.request.intent().frame());
            let presentation = self
                .frame_coordinator
                .presentations
                .get_mut(&pass.plan.token)
                .expect("a submitted coordinated pass retains its allocation");
            if let Some(display) = pass.new_display.take() {
                presentation.display = display;
            }
            if let Some(revision) = pass.new_texture_revision {
                presentation.texture_revision = revision;
            }
            volume_refinement_by_report[pass.report_index] = pass.volume_refinement;
            debug_assert!(pass.publishes_frame);
            presentation.hidden_volume_refinement = None;
            presentation.last_rendered_frame = Some(pass.plan.request.intent().frame());
            presentation.last_rendered_volume_schedule = matches!(
                pass.plan.request.intent().view(),
                RenderViewIntent::Volume { .. }
            )
            .then_some(pass.plan.request.volume_schedule());
            presentation.last_rendered_timepoint = Some(pass.plan.request.intent().timepoint());
            presentation.last_rendered_volume = matches!(
                pass.plan.request.intent().view(),
                RenderViewIntent::Volume { .. }
            );
            presentation.last_rendered_layers.clear();
            presentation.last_rendered_layers.extend(
                pass.plan
                    .request
                    .intent()
                    .layers()
                    .iter()
                    .map(|layer| layer.layer()),
            );
            presentation.last_rendered_modes.clear();
            presentation.last_rendered_modes.extend(
                pass.plan.request.intent().layers().iter().map(|layer| {
                    let state = layer.render_state();
                    if state.mip_parameters().is_some() {
                        0
                    } else if state.dvr_parameters().is_some() {
                        1
                    } else {
                        2
                    }
                }),
            );
            presentation.last_rendered_sampling.clear();
            presentation.last_rendered_sampling.extend(
                pass.plan
                    .request
                    .intent()
                    .layers()
                    .iter()
                    .map(|layer| layer.render_state().sampling_policy()),
            );
            presentation.last_progress = Some(pass.progress.clone());
            presentation.last_rendered_requirements =
                Some(pass.plan.request.requirements().clone());
            if presentation.availability.as_ref() == Some(pass.progress.coverage()) {
                presentation.rendered_residency_freshness = RenderedResidencyFreshness::Current;
            }
            // A different target can upload resources after this pass's
            // progress snapshot was prepared but before the coordinated color
            // cutoff is recorded. `apply_residency_changes` marks that newer
            // required availability above. Do not erase the mark merely
            // because this pass published its older useful pixels; the next
            // cutoff must consume the newer coverage.
            if let Some(pending) = pass.pending_capture.take() {
                presentation.pending_capture = Some(pending);
            }
            capture_by_report[pass.report_index] = pass.capture_ticket;
            presented_by_report[pass.report_index] = true;

            if pass.plan.candidate {
                debug_assert_eq!(
                    pass.progress.completeness(),
                    FrameCompleteness::Exact,
                    "an incomplete 3D candidate cannot replace the front"
                );
                let slot = *self.frame_coordinator.slot(PresentationTarget::ThreeD);
                let old_front = slot
                    .front
                    .expect("a submitted 3D candidate retains a visible predecessor");
                debug_assert_eq!(slot.candidate, Some(pass.plan.token));
                self.deactivate_presentation(old_front)
                    .expect("the replaced 3D front remains privately allocated");
                let slot = self.frame_coordinator.slot_mut(PresentationTarget::ThreeD);
                slot.front = Some(pass.plan.token);
                slot.candidate = Some(old_front);
            }
        }

        self.frame_coordinator.last_recorded_targets.clear();
        self.frame_coordinator
            .last_recorded_targets
            .extend(submitted_targets);
        let recorded_targets = self
            .frame_coordinator
            .last_recorded_targets
            .clone()
            .into_boxed_slice();
        let color_queue_submissions = u32::from(!recorded_targets.is_empty());
        if color_queue_submissions != 0 {
            self.record_target_control_publication(recorded_targets.len(), control_bytes);
            self.refresh_diagnostics(
                control_bytes,
                self.active_control_bytes(),
                display_peak_bytes,
                capture_peak_bytes,
                true,
                1,
            );
            if let Some(timing) = color_cpu_timing {
                let frame = prepared
                    .iter()
                    .find(|(plan, _)| plan.request.target() == active_target)
                    .or_else(|| prepared.first())
                    .expect("a nonempty coordinated cutoff has one request")
                    .0
                    .request
                    .intent()
                    .frame();
                self.record_cpu_frame_timing(frame, timing);
            }
        }
        for (plan, _) in &prepared {
            self.refresh_pending_residency_target(plan.token);
        }

        let residency_queue_submissions = prepared
            .iter()
            .map(|(_, report)| report.queue_submissions)
            .sum();
        let reports = prepared
            .into_iter()
            .enumerate()
            .map(|(index, (plan, report))| {
                let state = self
                    .coordinated_layout_state(plan.request.target())
                    .expect("a submitted coordinated cutoff retains every desired front");
                CoordinatedTargetExecutionReport {
                    target: plan.request.target(),
                    device_generation: state.device_generation,
                    texture_revision: state.texture_revision,
                    frame: report.frame,
                    progress: report.progress,
                    presented: presented_by_report[index],
                    visited_resources: report.visited_resources,
                    uploaded_resources: report.uploaded_resources,
                    payload_upload_bytes: report.payload_upload_bytes,
                    control_upload_bytes: color_control_bytes_by_report[index],
                    residency_command_buffers: report.command_buffers,
                    residency_queue_submissions: report.queue_submissions,
                    deferred_by_backpressure: report.deferred_by_backpressure,
                    volume_refinement: volume_refinement_by_report[index],
                    validation_capture: capture_by_report[index],
                    newly_resident_keys: report.newly_resident_keys,
                    evicted_keys: report.evicted_keys,
                }
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(CoordinatedFrameExecutionReport {
            targets: reports,
            recorded_targets,
            residency_queue_submissions,
            color_queue_submissions,
            cpu_timing: color_cpu_timing,
            gpu_timing: color_gpu_timing,
        })
    }

    #[allow(
        clippy::drop_non_drop,
        reason = "the explicit drop ends the first mutable residency plan before the second preflight"
    )]
    fn prepare_coordinated_residency(
        &mut self,
        presentation_token: PrivatePresentationId,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        leases: &[&dyn ResourceLease],
    ) -> Result<ResidencyPreparationReport, WgpuRenderRuntimeError> {
        self.ensure_device_available()?;
        if !self.residency.catalog_is_active(catalog) {
            return Err(WgpuRenderRuntimeError::PayloadContractMismatch);
        }
        self.validate_presentation_activation(presentation_token)?;
        // The coordinated owner collects device, transfer, and timing
        // completions once before preparing any target in the cutoff.
        let current_frame_state = self
            .frame_coordinator
            .presentations
            .get(&presentation_token)
            .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered)?
            .frame_state
            .clone();
        let current_requirements = current_frame_state
            .as_ref()
            .map(|state| self.residency.owned_requirements(state.residency));
        if intent.frame() != requirements.frame() {
            return Err(WgpuRenderRuntimeError::FrameContractMismatch);
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
            return Ok(ResidencyPreparationReport {
                frame: intent.frame(),
                progress: None,
                visited_resources: 0,
                uploaded_resources: 0,
                payload_upload_bytes: 0,
                command_buffers: 0,
                queue_submissions: 0,
                deferred_by_backpressure: true,
                newly_resident_keys: Box::new([]),
                evicted_keys: Box::new([]),
            });
        }
        let lease_by_key = self.validate_inputs(
            current_frame_state.as_ref(),
            current_requirements.as_ref(),
            catalog,
            intent,
            requirements,
            leases,
        )?;
        let planned_frame = PlannedFrame {
            frame: requirements.frame(),
            requirements: requirements.clone(),
        };
        let visited = lease_by_key.keys().copied().collect::<Vec<_>>();
        let replacement_release_candidates = self.residency.replacement_release_candidates(
            current_frame_state
                .as_ref()
                .map(|current| current.residency),
            &planned_frame.requirements,
        );

        let mut uploads = Vec::new();
        let mut empty_residents = BTreeMap::new();
        let mut raw_upload_bytes = 0_u64;
        let mut budget_limited =
            !visited.is_empty() && planned_frame.requirements.resources().len() > visited.len();
        // Plan directly against the compact allocator with a bounded undo log.
        // This avoids cloning even a heavily fragmented free-range index. The
        // exact inverse is applied before later preflight, then replayed only
        // after the submission succeeds.
        let mut allocator_operations = Vec::new();
        let mut victim_age_cursors = [None; MAX_PAYLOAD_SEGMENTS];
        let transfer = self.residency.begin_transfer_plan()?;
        let segment_allocation_order =
            payload_segment_allocation_order(transfer.payload_segments.len(), transfer.resident);

        for key in &visited {
            if transfer.resident.contains_key(key) {
                continue;
            }
            let Some(lease) = lease_by_key.get(key).copied() else {
                continue;
            };
            let payload = lease.payload();
            let facts = lease.payload_facts();
            let grid = transfer
                .resource_grids
                .validate_key(*key)
                .map_err(|_| WgpuRenderRuntimeError::PayloadContractMismatch)?;
            if let Some(mut resident) =
                empty_resident_resource(payload, facts, intent.frame().get())
            {
                resident.grid = Some(grid);
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
                    rollback_arena_operations(
                        &mut *transfer.payload_segments,
                        &allocator_operations,
                    );
                    return Err(error);
                }
            };
            let allocated_bytes = layout.allocation_bytes;
            if let Err(error) = validate_single_payload_segment_fit(
                allocated_bytes,
                transfer
                    .payload_segments
                    .iter()
                    .map(|segment| segment.logical_capacity),
            ) {
                rollback_arena_operations(&mut *transfer.payload_segments, &allocator_operations);
                return Err(error);
            }
            let mut upload_victims = Vec::new();
            let mut allocation = None;
            for segment_index in segment_allocation_order.iter().copied() {
                if let Some(offset) = transfer.payload_segments[segment_index]
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
            }
            if allocation.is_none() {
                let mut segment_order = (0..transfer.payload_segments.len())
                    .filter(|segment| {
                        transfer.payload_segments[*segment].capacity >= allocated_bytes
                    })
                    .filter_map(|segment| {
                        transfer
                            .next_payload_eviction_candidate(
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
                        if let Some(offset) = transfer.payload_segments[segment_index]
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
                        let Some((age, victim, offset, bytes)) = transfer
                            .next_payload_eviction_candidate(
                                segment_index,
                                victim_age_cursors[segment_index],
                                &replacement_release_candidates[segment_index],
                                &planned_frame.requirements,
                            )
                        else {
                            rollback_arena_operations_from(
                                &mut *transfer.payload_segments,
                                &mut allocator_operations,
                                operation_start,
                            );
                            upload_victims.truncate(victim_start);
                            victim_age_cursors[segment_index] = original_cursor;
                            break;
                        };
                        victim_age_cursors[segment_index] = Some((age, victim));
                        transfer.payload_segments[segment_index]
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
                let error = payload_allocation_failure(transfer.payload_segments, allocated_bytes);
                rollback_arena_operations(&mut *transfer.payload_segments, &allocator_operations);
                return Err(error);
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
                grid: Some(grid),
                page_record_index: u32::MAX,
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
        rollback_arena_operations(&mut *transfer.payload_segments, &allocator_operations);
        drop(transfer);
        let current_residency = current_frame_state
            .as_ref()
            .map(|current| current.residency);
        let empty_evictions = self.residency.plan_empty_resident_evictions(
            current_residency,
            &planned_frame.requirements,
            empty_residents.len(),
        )?;

        let mut evicted = uploads
            .iter()
            .flat_map(|upload| upload.victims.iter().copied())
            .chain(empty_evictions.iter().copied())
            .collect::<BTreeSet<_>>();
        let additional_page_evictions = self.residency.plan_global_page_capacity_evictions(
            current_residency,
            &planned_frame.requirements,
            &evicted,
            uploads.len().saturating_add(empty_residents.len()),
        )?;
        for key in additional_page_evictions {
            let resource = self
                .residency
                .resident_resource(key)
                .expect("planned global page eviction remains resident until commit");
            if resource.allocated_bytes != 0 {
                allocator_operations.push(ArenaOperation::Release {
                    segment: resource.segment as usize,
                    offset: resource.offset,
                    bytes: resource.allocated_bytes,
                });
            }
            evicted.insert(key);
        }
        let admitted_keys = uploads
            .iter()
            .map(|upload| upload.key)
            .chain(empty_residents.keys().copied())
            .collect::<BTreeSet<_>>();
        debug_assert!(
            evicted.is_disjoint(&admitted_keys),
            "victim planning protects every requirement key admitted by this frame"
        );
        self.residency
            .preflight_eviction_events(&evicted, &admitted_keys)?;
        let removal_cells = evicted
            .iter()
            .map(|key| {
                let resident = self
                    .residency
                    .resident_resource(*key)
                    .expect("planned evictions remain resident until commit");
                compact_cell_keys(
                    *key,
                    resident
                        .grid
                        .expect("every globally resident resource retains its canonical grid"),
                )
                .map_err(map_global_residency_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let directory_removals = evicted
            .iter()
            .zip(&removal_cells)
            .map(|(key, cells)| {
                let page_index = self
                    .residency
                    .resident_resource(*key)
                    .expect("planned evictions remain resident until commit")
                    .page_record_index;
                DirectoryRemoval::new(page_index, cells)
            })
            .collect::<Vec<_>>();
        let admission_cells = uploads
            .iter()
            .map(|upload| {
                compact_cell_keys(
                    upload.key,
                    upload
                        .resident
                        .grid
                        .expect("planned uploads retain their canonical grid"),
                )
                .map_err(map_global_residency_error)
            })
            .chain(empty_residents.iter().map(|(key, resident)| {
                compact_cell_keys(
                    *key,
                    resident
                        .grid
                        .expect("planned empty residents retain their canonical grid"),
                )
                .map_err(map_global_residency_error)
            }))
            .collect::<Result<Vec<_>, _>>()?;
        let directory_admissions = admission_cells
            .iter()
            .map(|cells| DirectoryAdmission::new(cells))
            .collect::<Vec<_>>();
        let mut prepared_directory = (!directory_removals.is_empty()
            || !directory_admissions.is_empty())
        .then(|| {
            self.residency
                .prepare_directory_batch(&directory_removals, &directory_admissions)
        })
        .transpose()?;
        if let Some(prepared) = prepared_directory.as_ref() {
            let pages = prepared.admission_page_indices();
            let (upload_pages, empty_pages) = pages.split_at(uploads.len());
            for (upload, page_index) in uploads.iter_mut().zip(upload_pages.iter().copied()) {
                upload.resident.page_record_index = page_index;
            }
            for ((_, resident), page_index) in
                empty_residents.iter_mut().zip(empty_pages.iter().copied())
            {
                resident.page_record_index = page_index;
            }
        }
        let admitted_page_records = uploads
            .iter()
            .map(|upload| resource_record(upload.key, Some(&upload.resident)))
            .chain(
                empty_residents
                    .iter()
                    .map(|(key, resident)| resource_record(*key, Some(resident))),
            )
            .collect::<Result<Vec<_>, _>>()?;
        let uploaded = uploads
            .iter()
            .map(|upload| upload.key)
            .collect::<BTreeSet<_>>();
        let upload_resources = uploads
            .iter()
            .map(|upload| (upload.key, upload.resident.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut cold_coverage_membership_checks = 0_u64;
        let mut cold_coverage_resident_matches = 0_u64;
        let retained_availability =
            self.frame_coordinator
                .presentations
                .values()
                .find_map(|candidate| {
                    let state = candidate.frame_state.as_ref()?;
                    self.residency
                        .requirements(state.residency)
                        .shares_resources_with(requirements)
                        .then_some(candidate.availability.as_ref())
                        .flatten()
                });
        let requirement_body_retained = retained_availability.is_some();
        let base_coverage = if let Some(availability) = retained_availability {
            availability
                .rebind(requirements)
                .map_err(|_| WgpuRenderRuntimeError::FrameProgressContract)?
        } else {
            let (initially_available, checks) = self.residency.initial_available_keys(requirements);
            cold_coverage_membership_checks = checks;
            cold_coverage_resident_matches = initially_available.len() as u64;
            FrameCoverage::from_available(requirements, &initially_available)
                .map_err(|_| WgpuRenderRuntimeError::FrameProgressContract)?
        };
        let mut availability_changes = BTreeMap::new();
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
        let frame_empty_keys = empty_residents.keys().copied().collect::<Vec<_>>();
        let (
            directory_publications,
            directory_mutations,
            directory_rebuilds,
            directory_slot_writes,
            page_record_writes,
        ) = prepared_directory
            .as_ref()
            .map(|prepared| {
                let directory_mutations = directory_removals
                    .iter()
                    .map(|removal| removal.keys().len() as u64)
                    .chain(
                        directory_admissions
                            .iter()
                            .map(|admission| admission.keys().len() as u64),
                    )
                    .fold(0_u64, u64::saturating_add);
                let (directory_rebuilds, directory_slot_writes) = match prepared.publication() {
                    DirectoryPublication::Incremental {
                        removal_writes,
                        insertion_writes,
                    } => (
                        0,
                        removal_writes.len().saturating_add(insertion_writes.len()) as u64,
                    ),
                    DirectoryPublication::Rebuilt { slots } => (1, slots.len() as u64),
                };
                (
                    1,
                    directory_mutations,
                    directory_rebuilds,
                    directory_slot_writes,
                    directory_removals
                        .len()
                        .saturating_add(admitted_page_records.len()) as u64,
                )
            })
            .unwrap_or((0, 0, 0, 0, 0));
        let residency_publication_bytes = prepared_directory
            .as_ref()
            .map(|prepared| {
                let directory_bytes = match prepared.publication() {
                    DirectoryPublication::Incremental {
                        removal_writes,
                        insertion_writes,
                    } => removal_writes.len().saturating_add(insertion_writes.len()) as u64 * 32,
                    DirectoryPublication::Rebuilt { slots } => slots.len() as u64 * 32,
                };
                directory_bytes.saturating_add(
                    evicted.len().saturating_add(admitted_page_records.len()) as u64 * 64,
                )
            })
            .unwrap_or(0);
        let transfer_bytes =
            uploads
                .iter()
                .try_fold(residency_publication_bytes, |total, upload| {
                    total
                        .checked_add(upload.layout.allocation_bytes)
                        .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)
                })?;
        let transfer_capacity_bytes = self.residency.transfer_capacity_bytes();
        if transfer_bytes > transfer_capacity_bytes {
            return Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::TransferStaging,
                requested_bytes: transfer_bytes,
                available_bytes: transfer_capacity_bytes,
            });
        }

        let control_peak_bytes = self.active_control_bytes();
        let display_peak_bytes = self.active_display_bytes();
        let capture_peak_bytes = self.active_capture_bytes();

        let needs_submission = prepared_directory.is_some();
        let residency_staging_bytes = transfer_bytes;
        let staging_slot = if residency_staging_bytes == 0 {
            None
        } else {
            let Some((slot, newly_allocated, reserved)) = self
                .residency
                .acquire_transfer_slot(&self.device, residency_staging_bytes)?
            else {
                self.diagnostics.backpressure_deferrals =
                    self.diagnostics.backpressure_deferrals.saturating_add(1);
                return Ok(ResidencyPreparationReport {
                    frame: intent.frame(),
                    progress: None,
                    visited_resources: 0,
                    uploaded_resources: 0,
                    payload_upload_bytes: 0,
                    command_buffers: 0,
                    queue_submissions: 0,
                    deferred_by_backpressure: true,
                    newly_resident_keys: Box::new([]),
                    evicted_keys: Box::new([]),
                });
            };
            if newly_allocated != 0 {
                self.diagnostics.explicit_staging_allocations = self
                    .diagnostics
                    .explicit_staging_allocations
                    .saturating_add(1);
                self.diagnostics.explicit_staging_bytes = reserved;
                self.diagnostics.peak_explicit_staging_bytes =
                    self.diagnostics.peak_explicit_staging_bytes.max(reserved);
            }
            Some(slot)
        };
        let mut command_buffers = 0_u32;
        let mut queue_submissions = 0_u32;
        self.ensure_device_available()?;

        if needs_submission {
            if let Some(slot) = staging_slot {
                let padding_zero_bytes = self.residency.stage_transfer(
                    slot,
                    &uploads,
                    prepared_directory.as_ref(),
                    &directory_removals,
                    &admitted_page_records,
                    self.gpu_failure.as_ref(),
                )?;
                self.diagnostics.upload_staging_padding_zero_bytes = self
                    .diagnostics
                    .upload_staging_padding_zero_bytes
                    .saturating_add(padding_zero_bytes);
            }
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mirante4d-residency-transfer"),
                });
            if let Some(slot) = staging_slot {
                self.residency.encode_transfer(
                    slot,
                    &mut encoder,
                    &uploads,
                    prepared_directory.as_ref(),
                    &directory_removals,
                    &admitted_page_records,
                );
            }
            let command_buffer = encoder.finish();
            self.ensure_device_available()?;
            self.queue.submit([command_buffer]);
            if let Some(slot) = staging_slot {
                self.residency.transfer_submitted(slot);
            }
            command_buffers = 1;
            queue_submissions = 1;
            let in_flight = Arc::clone(&self.in_flight_submissions);
            let submitted = in_flight.fetch_add(1, Ordering::AcqRel) + 1;
            self.queue.on_submitted_work_done(move || {
                in_flight.fetch_sub(1, Ordering::AcqRel);
            });
            self.diagnostics.current_in_flight_submissions = submitted;
            self.diagnostics.peak_in_flight_submissions =
                self.diagnostics.peak_in_flight_submissions.max(submitted);
        }
        let residency_commit = self.residency.commit_transfer(
            prepared_directory.take(),
            &allocator_operations,
            &evicted,
            &uploads,
            &empty_residents,
        );
        self.refresh_payload_allocator_diagnostics();
        self.record_directory_publication(
            directory_publications,
            directory_mutations,
            directory_rebuilds,
            directory_slot_writes,
            page_record_writes,
        );

        self.diagnostics.residency_evictions = self
            .diagnostics
            .residency_evictions
            .saturating_add(residency_commit.evicted_resources);
        self.diagnostics.residency_epoch_reuploads = self
            .diagnostics
            .residency_epoch_reuploads
            .saturating_add(residency_commit.reuploaded_resources);
        self.diagnostics.resident_payload_used_bytes = residency_commit.resident_payload_bytes;
        if !requirement_body_retained {
            self.residency
                .touch_empty_keys(&frame_empty_keys, planned_frame.frame);
        }
        self.apply_residency_changes(residency_commit.changes);
        let residency = self.residency.commit_frame_lease(
            current_frame_state
                .as_ref()
                .map(|current| current.residency),
            planned_frame.requirements.clone(),
        );
        let presentation = self
            .frame_coordinator
            .presentations
            .get_mut(&presentation_token)
            .expect("presentation registration was checked before commit");
        presentation.frame_state = Some(FrameState {
            frame: planned_frame.frame,
            residency,
        });
        presentation.availability = Some(coverage);
        self.refresh_diagnostics(
            transfer_bytes,
            control_peak_bytes,
            display_peak_bytes,
            capture_peak_bytes,
            false,
            queue_submissions,
        );
        let hits = self.residency.resident_hit_count(&visited, &uploaded);
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
        self.diagnostics.cold_coverage_resident_matches = self
            .diagnostics
            .cold_coverage_resident_matches
            .saturating_add(cold_coverage_resident_matches);
        Ok(ResidencyPreparationReport {
            frame: intent.frame(),
            progress,
            visited_resources: visited.len(),
            uploaded_resources: uploads.len(),
            payload_upload_bytes: raw_upload_bytes,
            command_buffers,
            queue_submissions,
            deferred_by_backpressure: false,
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

    fn validate_presentation_activation(
        &self,
        token: PrivatePresentationId,
    ) -> Result<(), WgpuRenderRuntimeError> {
        let target_active = self
            .frame_coordinator
            .presentations
            .get(&token)
            .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered)?
            .frame_state
            .is_some();
        let active = self
            .frame_coordinator
            .presentations
            .values()
            .filter(|presentation| presentation.frame_state.is_some())
            .count();
        if target_active || active < MAX_ACTIVE_PRESENTATION_TARGETS {
            return Ok(());
        }
        let private_3d_candidate = self
            .frame_coordinator
            .slot(PresentationTarget::ThreeD)
            .candidate;
        if active == MAX_ACTIVE_PRESENTATION_TARGETS
            && private_3d_candidate.is_some_and(|candidate| {
                candidate == token
                    || self
                        .frame_coordinator
                        .presentations
                        .get(&candidate)
                        .is_some_and(|presentation| presentation.frame_state.is_some())
            })
        {
            // Exactly four fixed fronts plus their one private 3D replacement
            // are legal regardless of which member is activated last.
            return Ok(());
        }
        Err(WgpuRenderRuntimeError::PresentationCapacityExceeded {
            maximum: MAX_ACTIVE_PRESENTATION_TARGETS,
        })
    }

    fn apply_residency_changes(&mut self, change_set: ResidencyChangeSet) {
        let ResidencyChangeSet { changes } = change_set;
        if changes.is_empty() {
            return;
        }
        let requirements_by_target = self
            .frame_coordinator
            .presentations
            .iter()
            .filter_map(|(token, presentation)| {
                presentation
                    .frame_state
                    .as_ref()
                    .map(|state| (*token, self.residency.owned_requirements(state.residency)))
            })
            .collect::<BTreeMap<_, _>>();
        for (token, presentation) in &mut self.frame_coordinator.presentations {
            let Some(requirements) = requirements_by_target.get(token) else {
                continue;
            };
            let applicable = changes
                .iter()
                .filter(|(key, _)| requirements.contains_resource(**key))
                .map(|(key, available)| (*key, *available))
                .collect::<Vec<_>>();
            if applicable.is_empty() {
                continue;
            }
            let coverage = presentation
                .availability
                .as_ref()
                .expect("an active presentation retains exact availability")
                .with_availability_changes(requirements, &applicable)
                .expect("retained availability shares the active requirement body");
            for (key, available) in &applicable {
                if requirements.is_required_resource(*key) {
                    presentation
                        .rendered_residency_freshness
                        .record_required_change(*available);
                }
            }
            presentation.availability = Some(coverage);
        }
    }

    fn collect_gpu_timings(&mut self) -> Result<(), WgpuRenderRuntimeError> {
        let gpu_failure = Arc::clone(&self.gpu_failure);
        gpu_failure.ensure_available()?;
        let Some(timing) = self.timing.as_mut() else {
            return Ok(());
        };
        let mut ready = Vec::new();
        let mut failures = 0_u64;
        for slot in &mut timing.slots {
            let (ticket, mapped, batch_envelope_timestamps, render_pass_timestamps) =
                match &slot.state {
                    TimingSlotState::Free => continue,
                    TimingSlotState::Pending {
                        ticket,
                        mapped,
                        batch_envelope_timestamps,
                        render_pass_timestamps,
                    } => (
                        *ticket,
                        Arc::clone(mapped),
                        *batch_envelope_timestamps,
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
                    gpu_failure.ensure_available()?;
                    slot.readback.unmap();
                    slot.state = TimingSlotState::Free;
                    failures = failures.saturating_add(1);
                }
                Some(Ok(())) => {
                    gpu_failure.ensure_available()?;
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
                    let render_pass_ns = if render_pass_timestamps {
                        Some(elapsed(read_u64(byte_offset), read_u64(byte_offset + 8)))
                    } else {
                        None
                    };
                    let result =
                        batch_gpu_envelope_ns
                            .transpose()
                            .and_then(|batch_gpu_envelope_ns| {
                                render_pass_ns
                                    .transpose()
                                    .map(|render_pass_ns| (batch_gpu_envelope_ns, render_pass_ns))
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
        for (ticket, (batch_gpu_envelope_ns, render_pass_ns)) in ready {
            let result = GpuFrameTiming {
                ticket,
                batch_gpu_envelope_ns,
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
                self.frame_coordinator
                    .presentations
                    .values()
                    .any(|presentation| {
                        presentation.logical_target == ticket.target
                            && presentation.last_rendered_frame == Some(ticket.generation)
                    });
            if timing_is_current {
                self.diagnostics.last_gpu_batch_envelope_ns = result.batch_gpu_envelope_ns;
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
        self.ensure_device_available()?;
        if self.timing.is_none() {
            return Err(WgpuRenderRuntimeError::UnknownGpuTiming);
        }
        let poll_result = self.device.poll(wgpu::PollType::Poll);
        self.ensure_device_available()?;
        poll_result.map_err(|_| WgpuRenderRuntimeError::GpuTimingFailed)?;
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

    fn poll_coordinated_validation_capture_inner(
        &mut self,
        ticket: ValidationCaptureTicket,
    ) -> Result<Option<ValidationCapture>, WgpuRenderRuntimeError> {
        let gpu_failure = Arc::clone(&self.gpu_failure);
        gpu_failure.ensure_available()?;
        let private_presentation = PrivatePresentationId::new(ticket.private_presentation)
            .expect("renderer-created capture tickets retain a nonzero private identity");
        if self
            .frame_coordinator
            .presentations
            .get(&private_presentation)
            .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered)?
            .frame_state
            .as_ref()
            .is_some_and(|current| ticket.frame != current.frame)
        {
            return Err(WgpuRenderRuntimeError::StaleValidationCapture);
        }
        let poll_result = self.device.poll(wgpu::PollType::Poll);
        gpu_failure.ensure_available()?;
        poll_result.map_err(|_| WgpuRenderRuntimeError::ValidationCaptureFailed)?;
        self.sync_validation_errors();
        if self.diagnostics.validation_error_count != 0 {
            return Err(WgpuRenderRuntimeError::BackendValidation);
        }
        let presentation = self
            .frame_coordinator
            .presentations
            .get_mut(&private_presentation)
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
                gpu_failure.ensure_available()?;
                presentation.pending_capture = None;
                Err(WgpuRenderRuntimeError::ValidationCaptureFailed)
            }
            Some(Ok(())) => {
                gpu_failure.ensure_available()?;
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

    pub(super) fn poll_coordinated_validation_capture(
        &mut self,
        ticket: CoordinatedValidationCaptureTicket,
    ) -> Result<Option<ValidationCapture>, WgpuRenderRuntimeError> {
        if ticket.device_generation != self.frame_coordinator.device_generation
            || ticket.residency_generation != self.frame_coordinator.residency_generation
        {
            return Err(WgpuRenderRuntimeError::StaleValidationCapture);
        }
        let slot = self.frame_coordinator.slot(ticket.target);
        if !slot.desired {
            return Err(WgpuRenderRuntimeError::StaleValidationCapture);
        }
        let Some(front) = slot.front else {
            return Err(WgpuRenderRuntimeError::StaleValidationCapture);
        };
        let presentation = self
            .frame_coordinator
            .presentations
            .get(&front)
            .ok_or(WgpuRenderRuntimeError::StaleValidationCapture)?;
        if front.get() != ticket.inner.private_presentation
            || presentation.texture_revision != ticket.texture_revision
            || presentation
                .pending_capture
                .as_ref()
                .is_none_or(|pending| pending.ticket != ticket.inner)
        {
            return Err(WgpuRenderRuntimeError::StaleValidationCapture);
        }
        self.poll_coordinated_validation_capture_inner(ticket.inner)
    }

    fn request_coordinated_pick_inner(
        &mut self,
        presentation_token: PrivatePresentationId,
        query: VolumePickQuery,
    ) -> Result<VolumePickTicket, WgpuRenderRuntimeError> {
        self.pipelines.ensure_capability(PipelineCapability::Pick)?;
        let gpu_failure = Arc::clone(&self.gpu_failure);
        gpu_failure.ensure_available()?;
        let presentation_target = self
            .frame_coordinator
            .presentations
            .get(&presentation_token)
            .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered)?
            .logical_target;
        if query.target() != presentation_target {
            return Err(WgpuRenderRuntimeError::PickQueryMismatch);
        }
        let poll_result = self.device.poll(wgpu::PollType::Poll);
        gpu_failure.ensure_available()?;
        poll_result.map_err(|_| WgpuRenderRuntimeError::VolumePickFailed)?;
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
            let presentation = self
                .frame_coordinator
                .presentations
                .get(&presentation_token)
                .ok_or(WgpuRenderRuntimeError::PresentationNotRegistered)?;
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
            if presentation.rendered_residency_freshness.requires_refresh() {
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
        let transfer_capacity_bytes = self.residency.transfer_capacity_bytes();
        if transfer_bytes > transfer_capacity_bytes {
            return Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::TransferStaging,
                requested_bytes: transfer_bytes,
                available_bytes: transfer_capacity_bytes,
            });
        }

        let ticket = VolumePickTicket::new(self.next_pick, presentation_target, query.frame())
            .map_err(|_| WgpuRenderRuntimeError::PickTicketExhausted)?;
        let next_pick = self
            .next_pick
            .checked_add(1)
            .ok_or(WgpuRenderRuntimeError::PickTicketExhausted)?;
        let staging = mapped_staging_buffer(
            &self.device,
            gpu_failure.as_ref(),
            "mirante4d-pick-query-staging",
            staging_bytes,
        )?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-pick-command-encoder"),
            });
        let pick_pipeline = self.pipelines.pick()?;
        let slot = &mut self.pick.slots[slot_index];
        encoder.copy_buffer_to_buffer(&staging, 0, &slot.query_buffer, 0, PICK_QUERY_BYTES);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-pick-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(pick_pipeline);
            pass.set_bind_group(
                0,
                &self
                    .frame_coordinator
                    .presentations
                    .get(&presentation_token)
                    .expect("presentation registration was checked before pick submission")
                    .pick_bind_groups[slot_index],
                &[],
            );
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&slot.output_buffer, 0, &slot.readback, 0, PICK_OUTPUT_BYTES);
        let command_buffer = encoder.finish();
        gpu_failure.ensure_available()?;
        let in_flight_counter = Arc::clone(&self.in_flight_submissions);
        let submitted = in_flight_counter.fetch_add(1, Ordering::AcqRel) + 1;
        let mapped = Arc::new(Mutex::new(None));
        let callback = Arc::clone(&mapped);
        self.queue.submit([command_buffer]);
        self.queue.on_submitted_work_done(move || {
            in_flight_counter.fetch_sub(1, Ordering::AcqRel);
        });
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
        self.diagnostics.peak_transfer_bytes = self.diagnostics.peak_transfer_bytes.max(
            self.residency
                .compaction_scratch_capacity
                .saturating_add(transfer_bytes),
        );
        drop(staging);
        Ok(ticket)
    }

    pub(super) fn request_coordinated_pick(
        &mut self,
        target: PresentationTarget,
        query: VolumePickQuery,
    ) -> Result<VolumePickTicket, WgpuRenderRuntimeError> {
        if target != PresentationTarget::ThreeD || query.target() != target {
            return Err(WgpuRenderRuntimeError::PickQueryMismatch);
        }
        let front = self.frame_coordinator.front_token(target)?;
        self.request_coordinated_pick_inner(front, query)
    }

    pub(super) fn poll_pick(
        &mut self,
        ticket: VolumePickTicket,
    ) -> Result<Option<VolumePickResult>, WgpuRenderRuntimeError> {
        let gpu_failure = Arc::clone(&self.gpu_failure);
        gpu_failure.ensure_available()?;
        let poll_result = self.device.poll(wgpu::PollType::Poll);
        gpu_failure.ensure_available()?;
        poll_result.map_err(|_| WgpuRenderRuntimeError::VolumePickFailed)?;
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
                gpu_failure.ensure_available()?;
                slot.state = PickSlotState::Free;
                Err(WgpuRenderRuntimeError::VolumePickFailed)
            }
            Some(Ok(())) => {
                gpu_failure.ensure_available()?;
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
        current_requirements: Option<&RenderRequirements>,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        leases: &'a [&'a dyn ResourceLease],
    ) -> Result<BTreeMap<BrickKey, &'a dyn ResourceLease>, WgpuRenderRuntimeError> {
        if intent.frame() != requirements.frame() {
            return Err(WgpuRenderRuntimeError::FrameContractMismatch);
        }
        let requirement_keys_match =
            current_requirements.is_some_and(|current| current.shares_resources_with(requirements));
        // Scale/identity facts are properties of the immutable key body. Reuse
        // them across progressive passes and camera-only frames. Page
        // occupancy/hash construction happens exactly once in the transactional
        // static-control build below, rather than once here and once there.
        if !requirement_keys_match && !self.residency.body_is_known(requirements) {
            validate_requirement_contract(requirements)?;
            let grids = self.residency.resource_grids()?;
            for key in requirements.resource_keys() {
                grids
                    .validate_key(*key)
                    .map_err(|_| WgpuRenderRuntimeError::PayloadContractMismatch)?;
            }
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
        // Frame identity names the accepted semantic input, while the
        // immutable requirement body names the resources prepared for that
        // input. Latest-only asynchronous planning may legitimately replace a
        // provisional body under the same frame. Coverage, lease relevance,
        // and presentation dirty state are all body-aware below, so no second
        // frame identity is needed for that cutover.
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

    fn refresh_diagnostics(
        &mut self,
        transfer_bytes: u64,
        control_bytes: u64,
        display_bytes: u64,
        capture_bytes: u64,
        rendered: bool,
        submissions: u32,
    ) {
        let empty_resident_count = self.residency.empty_resident_count();
        let empty_metadata_bytes = empty_resident_metadata_bytes(empty_resident_count);
        self.diagnostics.empty_resident_metadata_records = empty_resident_count;
        self.diagnostics.empty_resident_metadata_bytes = empty_metadata_bytes;
        self.diagnostics.peak_empty_resident_metadata_bytes = self
            .diagnostics
            .peak_empty_resident_metadata_bytes
            .max(empty_metadata_bytes);
        self.diagnostics.peak_resident_payload_used_bytes = self
            .diagnostics
            .peak_resident_payload_used_bytes
            .max(self.diagnostics.resident_payload_used_bytes);
        self.diagnostics.peak_transfer_bytes = self.diagnostics.peak_transfer_bytes.max(
            self.residency
                .compaction_scratch_capacity
                .saturating_add(transfer_bytes),
        );
        self.diagnostics.peak_control_and_residency_metadata_bytes = self
            .diagnostics
            .peak_control_and_residency_metadata_bytes
            .max(control_bytes);
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

    fn record_cpu_frame_timing(&mut self, frame: FrameIdentity, timing: CpuFrameTiming) {
        let planning_ns = timing.planning_ns();
        let queue_submit_ns = timing.queue_submit_ns();
        self.diagnostics.completed_cpu_timings =
            self.diagnostics.completed_cpu_timings.saturating_add(1);
        self.diagnostics.last_cpu_timing_frame = Some(frame.get());
        self.diagnostics.last_cpu_planning_ns = Some(planning_ns);
        self.diagnostics.last_cpu_queue_submit_ns = Some(queue_submit_ns);
        self.diagnostics.total_cpu_planning_ns = self
            .diagnostics
            .total_cpu_planning_ns
            .saturating_add(planning_ns);
        self.diagnostics.total_cpu_queue_submit_ns = self
            .diagnostics
            .total_cpu_queue_submit_ns
            .saturating_add(queue_submit_ns);
    }

    fn record_directory_publication(
        &mut self,
        publications: u64,
        mutations: u64,
        rebuilds: u64,
        slot_writes: u64,
        page_record_writes: u64,
    ) {
        self.diagnostics.directory_publications = self
            .diagnostics
            .directory_publications
            .saturating_add(publications);
        self.diagnostics.directory_mutations = self
            .diagnostics
            .directory_mutations
            .saturating_add(mutations);
        self.diagnostics.directory_rebuilds =
            self.diagnostics.directory_rebuilds.saturating_add(rebuilds);
        self.diagnostics.directory_slot_writes = self
            .diagnostics
            .directory_slot_writes
            .saturating_add(slot_writes);
        self.diagnostics.page_record_writes = self
            .diagnostics
            .page_record_writes
            .saturating_add(page_record_writes);
    }

    fn record_target_control_publication(&mut self, updates: usize, bytes: u64) {
        self.diagnostics.target_control_updates = self
            .diagnostics
            .target_control_updates
            .saturating_add(updates as u64);
        self.diagnostics.target_control_upload_bytes = self
            .diagnostics
            .target_control_upload_bytes
            .saturating_add(bytes);
    }

    fn sync_validation_errors(&mut self) {
        self.diagnostics.validation_error_count = self
            .validation_error_count
            .load(Ordering::Relaxed)
            .try_into()
            .unwrap_or(u64::MAX);
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // A driver compile is not cancellable. Signal the sole worker and
        // detach instead of making cold application shutdown wait for it.
        // Cloned device/layout handles keep its remaining work race-free.
        self.pipelines.cancel_and_detach_compiler();
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

fn initial_payload_segment_commitments(
    logical_capacities: &[u64; MAX_PAYLOAD_SEGMENTS],
    requested_commitment: u64,
) -> [u64; MAX_PAYLOAD_SEGMENTS] {
    let mut commitments = [0_u64; MAX_PAYLOAD_SEGMENTS];
    let mut remaining = requested_commitment;
    for (commitment, logical) in commitments.iter_mut().zip(logical_capacities) {
        if *logical == 0 || remaining == 0 {
            break;
        }
        *commitment = remaining.min(*logical) / COPY_ALIGNMENT * COPY_ALIGNMENT;
        remaining = remaining.saturating_sub(*commitment);
    }
    commitments
}

fn bounded_geometric_payload_commitment(
    current: u64,
    logical_maximum: u64,
    required_end: u64,
) -> Result<u64, WgpuRenderRuntimeError> {
    if current > logical_maximum || required_end > logical_maximum {
        return Err(WgpuRenderRuntimeError::CapacityExceeded {
            category: GpuLedgerCategory::PayloadResidency,
            requested_bytes: required_end,
            available_bytes: logical_maximum,
        });
    }
    if required_end <= current {
        return Ok(current);
    }
    let geometric = current
        .max(COPY_ALIGNMENT)
        .checked_mul(2)
        .unwrap_or(logical_maximum)
        .min(logical_maximum);
    let headroom_ceiling = align_copy(
        required_end
            .saturating_add(MAX_PAYLOAD_GROWTH_HEADROOM_BYTES)
            .min(logical_maximum),
    )
    .min(logical_maximum);
    Ok(geometric.clamp(required_end, headroom_ceiling))
}

fn transfer_capacity_partition(
    transfer_capacity_bytes: u64,
) -> Result<(u64, u64), WgpuRenderRuntimeError> {
    let compaction_scratch_capacity =
        if transfer_capacity_bytes >= MAX_PAYLOAD_UPLOAD_BYTES.saturating_mul(2) {
            MAX_PAYLOAD_UPLOAD_BYTES
        } else {
            0
        };
    let upload_staging_capacity =
        transfer_capacity_bytes.saturating_sub(compaction_scratch_capacity);
    if upload_staging_capacity < COPY_ALIGNMENT {
        return Err(WgpuRenderRuntimeError::InvalidConfiguration);
    }
    Ok((upload_staging_capacity, compaction_scratch_capacity))
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

fn uniform_layout_entry_for_stage(
    binding: u32,
    bytes: u64,
    visibility: wgpu::ShaderStages,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
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

#[cfg(test)]
mod tests {
    use mirante4d_dataset::{
        DatasetLayer, DatasetResourceIdentity, DatasetScale, DatasetSourceId,
        ResourcePayloadDescriptor, ResourcePayloadFacts, ResourceRegion, ResourceValidity,
        ScientificIdentityStatus,
    };
    use mirante4d_domain::{
        CameraView, CrossSectionView, DisplayWindow, DvrOpacityTransfer, GridToWorld,
        IntensityDType, IsoLightState, IsoShadingPolicy, LayerTransfer, LogicalLayerKey, Opacity,
        RenderState, RgbColor, SamplingPolicy, ScaleLevel, Shape3D, TimeIndex, TransferCurve,
        UnitQuaternion, WorldPoint3,
    };
    use mirante4d_render_api::{
        LayerRenderIntent, PreparedRenderRequirements, PreparedResourceBody, PresentationViewport,
        RenderRequirementRole,
    };

    use super::*;

    fn key(layer: u32, scale: u32, origin_x: u64, width: u64) -> BrickKey {
        BrickKey::new(
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

    fn requirement(key: BrickKey) -> RenderRequirement {
        RenderRequirement::new(key, mirante4d_render_api::RenderRequirementRole::Refinement)
    }

    fn frame_intent(frame: u64, layer: LogicalLayerKey) -> RenderIntent {
        RenderIntent::new(
            FrameIdentity::new(frame),
            DatasetResourceIdentity::Unverified(DatasetSourceId::new(1)),
            TimeIndex::new(0),
            RenderViewIntent::cross_section(
                CrossSectionView::new(
                    WorldPoint3::new(0.0, 0.0, 0.0).expect("test center is finite"),
                    UnitQuaternion::identity(),
                    1.0,
                    1.0,
                )
                .expect("test cross-section is valid"),
            ),
            PresentationViewport::new(64.0, 64.0).expect("test viewport is valid"),
            RenderExtent::new(64, 64).expect("test extent is valid"),
            vec![LayerRenderIntent::new(
                layer,
                LayerTransfer::new(
                    DisplayWindow::new(0.0, 1.0).expect("test window is valid"),
                    RgbColor::new([1.0, 1.0, 1.0]).expect("test color is valid"),
                    Opacity::new(1.0).expect("test opacity is valid"),
                    TransferCurve::linear(),
                    false,
                ),
                RenderState::mip(SamplingPolicy::VoxelExact),
            )],
        )
        .expect("test render intent is valid")
    }

    fn test_layer_intent(ordinal: u32, state: RenderState) -> LayerRenderIntent {
        LayerRenderIntent::new(
            LogicalLayerKey::new(ordinal),
            LayerTransfer::new(
                DisplayWindow::new(0.0, 1.0).expect("test window is valid"),
                RgbColor::new([1.0, 1.0, 1.0]).expect("test color is valid"),
                Opacity::new(1.0).expect("test opacity is valid"),
                TransferCurve::linear(),
                false,
            ),
            state,
        )
    }

    fn volume_intent_with_layers(layers: Vec<LayerRenderIntent>) -> RenderIntent {
        RenderIntent::new(
            FrameIdentity::new(1),
            DatasetResourceIdentity::Unverified(DatasetSourceId::new(1)),
            TimeIndex::new(0),
            RenderViewIntent::volume(
                CameraView::new(
                    Projection::Orthographic,
                    WorldPoint3::new(0.0, 0.0, 0.0).expect("test target is finite"),
                    UnitQuaternion::identity(),
                    1.0,
                    64.0,
                    8.0,
                )
                .expect("test camera is valid"),
                IsoLightState::attached_camera(),
            ),
            PresentationViewport::new(64.0, 64.0).expect("test viewport is valid"),
            RenderExtent::new(64, 64).expect("test extent is valid"),
            layers,
        )
        .expect("test volume intent is valid")
    }

    fn volume_intent(states: &[RenderState]) -> RenderIntent {
        volume_intent_with_layers(
            states
                .iter()
                .copied()
                .enumerate()
                .map(|(ordinal, state)| {
                    test_layer_intent(u32::try_from(ordinal).expect("test ordinal fits"), state)
                })
                .collect(),
        )
    }

    #[test]
    fn hidden_batch_adaptation_grows_and_shrinks_bidirectionally() {
        assert_eq!(adapted_hidden_batch_rows(1, 100_000, 946), 4);
        assert_eq!(adapted_hidden_batch_rows(4, 100_000, 946), 16);
        assert_eq!(adapted_hidden_batch_rows(64, 12_000_000, 946), 32);
        assert_eq!(
            adapted_hidden_batch_rows(256, 100_000, 946),
            HIDDEN_REFINEMENT_MAX_BATCH_ROWS
        );
        assert_eq!(adapted_hidden_batch_rows(64, 1_000_000, 7), 7);
    }

    #[test]
    fn provisional_volume_schedule_never_satisfies_exact_presentation() {
        assert!(volume_schedule_requires_exact_promotion(
            Some(VolumeColorSchedule::InteractivePreview),
            VolumeColorSchedule::Direct,
        ));
        assert!(volume_schedule_requires_exact_promotion(
            Some(VolumeColorSchedule::InteractivePreview),
            VolumeColorSchedule::AtomicRefinement {
                strip_height_pixels: 8,
            },
        ));
        assert!(!volume_schedule_requires_exact_promotion(
            Some(VolumeColorSchedule::InteractivePreview),
            VolumeColorSchedule::InteractivePreview,
        ));
        assert!(!volume_schedule_requires_exact_promotion(
            Some(VolumeColorSchedule::Direct),
            VolumeColorSchedule::Direct,
        ));
        assert!(!volume_schedule_requires_exact_promotion(
            None,
            VolumeColorSchedule::Direct,
        ));
        assert!(
            volume_schedule_requires_color_continuation(
                None,
                true,
                VolumeColorSchedule::AtomicRefinement {
                    strip_height_pixels: 8,
                },
            ),
            "an unfinished hidden candidate must record its next strip before it owns a published schedule"
        );
        assert!(
            volume_schedule_requires_color_continuation(
                Some(VolumeColorSchedule::Direct),
                true,
                VolumeColorSchedule::AtomicRefinement {
                    strip_height_pixels: 8,
                },
            ),
            "an unfinished hidden candidate must not be mistaken for an already-rendered exact frame"
        );
        assert!(!volume_schedule_requires_color_continuation(
            Some(VolumeColorSchedule::Direct),
            false,
            VolumeColorSchedule::AtomicRefinement {
                strip_height_pixels: 8,
            },
        ));
    }

    #[test]
    fn color_kernel_selection_has_no_volume_fallback() {
        let layer = LogicalLayerKey::new(0);
        assert_eq!(
            ColorKernel::for_intent(&frame_intent(1, layer)),
            ColorKernel::Plane
        );

        let mip = RenderState::mip(SamplingPolicy::VoxelExact);
        let dvr = RenderState::dvr(
            SamplingPolicy::VoxelExact,
            DvrOpacityTransfer::new(
                DisplayWindow::new(0.0, 1.0).expect("test opacity window is valid"),
                TransferCurve::linear(),
            ),
            1.0,
        )
        .expect("test DVR state is valid");
        let iso = RenderState::iso(SamplingPolicy::VoxelExact, IsoShadingPolicy::Flat, 0.5)
            .expect("test ISO state is valid");

        assert_eq!(
            ColorKernel::for_intent(&volume_intent(&[mip, mip])),
            ColorKernel::Mip
        );
        assert_eq!(
            ColorKernel::for_intent(&volume_intent(&[dvr, dvr])),
            ColorKernel::Dvr
        );
        assert_eq!(
            ColorKernel::for_intent(&volume_intent(&[iso, iso])),
            ColorKernel::Iso
        );
        assert_eq!(
            ColorKernel::for_intent(&volume_intent(&[mip, dvr])),
            ColorKernel::Mixed
        );
    }

    #[test]
    fn whole_volume_fast_path_requires_one_exact_full_scale_resource() {
        let layer = LogicalLayerKey::new(0);
        let shape = Shape3D::new(8, 8, 8).unwrap();
        let catalog = DatasetCatalog::new(
            "whole-volume-fast-path",
            ScientificIdentityStatus::Unverified(DatasetSourceId::new(1)),
            vec![
                DatasetLayer::new(
                    layer,
                    "volume",
                    mirante4d_domain::Shape4D::new(1, 8, 8, 8).unwrap(),
                    IntensityDType::Uint8,
                    GridToWorld::identity(),
                    ResourceValidity::AllValid,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let intent = volume_intent(&[RenderState::mip(SamplingPolicy::VoxelExact)]);
        let full = BrickKey::new(
            catalog.resource_identity(),
            layer,
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0; 3], shape).unwrap(),
        );
        let full_requirements = RenderRequirements::new(
            &intent,
            vec![RenderRequirement::new(
                full,
                RenderRequirementRole::FirstUsefulFrame,
            )],
        )
        .unwrap();
        assert_eq!(
            full_volume_requirement_key(&catalog, &intent, &full_requirements, layer).unwrap(),
            Some(full)
        );

        let left = BrickKey::new(
            catalog.resource_identity(),
            layer,
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 0], Shape3D::new(8, 8, 4).unwrap()).unwrap(),
        );
        let right = BrickKey::new(
            catalog.resource_identity(),
            layer,
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 4], Shape3D::new(8, 8, 4).unwrap()).unwrap(),
        );
        let split_requirements = RenderRequirements::new(
            &intent,
            vec![
                RenderRequirement::new(left, RenderRequirementRole::FirstUsefulFrame),
                RenderRequirement::new(right, RenderRequirementRole::Refinement),
            ],
        )
        .unwrap();
        assert_eq!(
            full_volume_requirement_key(&catalog, &intent, &split_requirements, layer).unwrap(),
            None
        );
    }

    #[test]
    fn only_homogeneous_dvr_control_records_are_canonicalized_once_on_the_cpu() {
        let dvr = RenderState::dvr(
            SamplingPolicy::SmoothLinear,
            DvrOpacityTransfer::new(
                DisplayWindow::new(0.0, 1.0).expect("test opacity window is valid"),
                TransferCurve::linear(),
            ),
            1.0,
        )
        .expect("test DVR state is valid");
        let mip = RenderState::mip(SamplingPolicy::VoxelExact);

        let homogeneous = volume_intent_with_layers(vec![
            test_layer_intent(9, dvr),
            test_layer_intent(3, dvr),
            test_layer_intent(7, dvr),
        ]);
        assert_eq!(control_layer_indices(&homogeneous), vec![1, 2, 0]);

        let authored_mip =
            volume_intent_with_layers(vec![test_layer_intent(9, mip), test_layer_intent(3, mip)]);
        assert_eq!(control_layer_indices(&authored_mip), vec![0, 1]);

        let authored_mixed =
            volume_intent_with_layers(vec![test_layer_intent(9, dvr), test_layer_intent(3, mip)]);
        assert_eq!(control_layer_indices(&authored_mixed), vec![0, 1]);

        let authored_cross_section = RenderIntent::new(
            FrameIdentity::new(1),
            DatasetResourceIdentity::Unverified(DatasetSourceId::new(1)),
            TimeIndex::new(0),
            RenderViewIntent::cross_section(
                CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 1.0, 1.0)
                    .expect("test cross-section is valid"),
            ),
            PresentationViewport::new(64.0, 64.0).expect("test viewport is valid"),
            RenderExtent::new(64, 64).expect("test extent is valid"),
            vec![test_layer_intent(9, dvr), test_layer_intent(3, dvr)],
        )
        .expect("test cross-section intent is valid");
        assert_eq!(control_layer_indices(&authored_cross_section), vec![0, 1]);
    }

    #[test]
    fn canonical_dvr_control_keeps_each_record_aligned_with_its_logical_layer() {
        let layer_three_transform = GridToWorld::from_row_major([
            2.0, 0.0, 0.0, 5.0, 0.0, 3.0, 0.0, 6.0, 0.0, 0.0, 4.0, 7.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .expect("test transform is affine");
        let layer_nine_base_transform =
            GridToWorld::scale(4.0, 4.0, 4.0).expect("test base transform is valid");
        let layer_nine_scale_two_transform = GridToWorld::from_row_major([
            8.0, 0.0, 0.0, 11.0, 0.0, 9.0, 0.0, 12.0, 0.0, 0.0, 10.0, 13.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .expect("test transform is affine");
        let layer_three_shape = Shape3D::new(10, 12, 14).expect("test shape is valid");
        let layer_nine_base_shape = Shape3D::new(18, 20, 22).expect("test shape is valid");
        let layer_nine_scale_two_shape = Shape3D::new(9, 10, 11).expect("test shape is valid");
        let layer_three_cell = Shape3D::new(2, 3, 7).expect("test cell shape is valid");
        let layer_nine_base_cell = Shape3D::new(6, 5, 11).expect("test cell shape is valid");
        let layer_nine_scale_two_cell = Shape3D::new(3, 2, 11).expect("test cell shape is valid");
        let scale_two = ScaleLevel::new(2);

        let catalog = DatasetCatalog::new(
            "canonical-DVR-control",
            ScientificIdentityStatus::Unverified(DatasetSourceId::new(1)),
            vec![
                DatasetLayer::new_multiscale(
                    LogicalLayerKey::new(3),
                    "three",
                    1,
                    IntensityDType::Uint8,
                    vec![DatasetScale::new(
                        ScaleLevel::BASE,
                        layer_three_shape,
                        layer_three_transform,
                        ResourceValidity::AllValid,
                    )],
                )
                .expect("test layer is valid"),
                DatasetLayer::new_multiscale(
                    LogicalLayerKey::new(9),
                    "nine",
                    1,
                    IntensityDType::Uint8,
                    vec![
                        DatasetScale::new(
                            ScaleLevel::BASE,
                            layer_nine_base_shape,
                            layer_nine_base_transform,
                            ResourceValidity::AllValid,
                        ),
                        DatasetScale::new(
                            scale_two,
                            layer_nine_scale_two_shape,
                            layer_nine_scale_two_transform,
                            ResourceValidity::AllValid,
                        ),
                    ],
                )
                .expect("test layer is valid"),
            ],
        )
        .expect("test catalog is valid");
        let grids = RenderResourceGridCatalog::new(
            &catalog,
            vec![
                RenderResourceGrid::new(
                    LogicalLayerKey::new(3),
                    ScaleLevel::BASE,
                    layer_three_shape,
                    layer_three_cell,
                ),
                RenderResourceGrid::new(
                    LogicalLayerKey::new(9),
                    ScaleLevel::BASE,
                    layer_nine_base_shape,
                    layer_nine_base_cell,
                ),
                RenderResourceGrid::new(
                    LogicalLayerKey::new(9),
                    scale_two,
                    layer_nine_scale_two_shape,
                    layer_nine_scale_two_cell,
                ),
            ],
        )
        .expect("test grids cover every catalog scale");

        let dvr_layer = |ordinal| {
            let (window, color, opacity, curve, invert, opacity_window, density) = match ordinal {
                3 => (
                    [0.1, 0.7],
                    [0.2, 0.3, 0.4],
                    0.5,
                    1.5,
                    false,
                    [0.15, 0.65],
                    2.0,
                ),
                9 => (
                    [0.25, 0.95],
                    [0.6, 0.7, 0.8],
                    0.9,
                    2.5,
                    true,
                    [0.35, 0.85],
                    3.0,
                ),
                _ => panic!("unexpected test layer"),
            };
            LayerRenderIntent::new(
                LogicalLayerKey::new(ordinal),
                LayerTransfer::new(
                    DisplayWindow::new(window[0], window[1]).expect("test window is valid"),
                    RgbColor::new(color).expect("test color is valid"),
                    Opacity::new(opacity).expect("test opacity is valid"),
                    TransferCurve::gamma(curve).expect("test curve is valid"),
                    invert,
                ),
                RenderState::dvr(
                    SamplingPolicy::SmoothLinear,
                    DvrOpacityTransfer::new(
                        DisplayWindow::new(opacity_window[0], opacity_window[1])
                            .expect("test opacity window is valid"),
                        TransferCurve::linear(),
                    ),
                    density,
                )
                .expect("test DVR state is valid"),
            )
        };
        let forward = volume_intent_with_layers(vec![dvr_layer(3), dvr_layer(9)]);
        let reversed = volume_intent_with_layers(vec![dvr_layer(9), dvr_layer(3)]);
        let layer_three_key = BrickKey::new(
            catalog.resource_identity(),
            LogicalLayerKey::new(3),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0; 3], layer_three_cell).expect("test region is valid"),
        );
        let layer_nine_key = BrickKey::new(
            catalog.resource_identity(),
            LogicalLayerKey::new(9),
            TimeIndex::new(0),
            scale_two,
            ResourceRegion::new([0; 3], layer_nine_scale_two_cell).expect("test region is valid"),
        );
        let requirements_for = |intent: &RenderIntent| {
            RenderRequirements::new(
                intent,
                vec![
                    RenderRequirement::new(layer_nine_key, RenderRequirementRole::FirstUsefulFrame),
                    RenderRequirement::new(
                        layer_three_key,
                        RenderRequirementRole::FirstUsefulFrame,
                    ),
                ],
            )
            .expect("test requirements are valid")
        };
        let forward_control =
            build_target_control(&catalog, &grids, &forward, &requirements_for(&forward), 17)
                .expect("forward control is valid");
        let reversed_control = build_target_control(
            &catalog,
            &grids,
            &reversed,
            &requirements_for(&reversed),
            17,
        )
        .expect("reversed control is valid");
        let forward_words = bytemuck::cast_slice::<u8, u32>(&forward_control);
        let reversed_words = bytemuck::cast_slice::<u8, u32>(&reversed_control);

        assert_eq!(
            forward_words, reversed_words,
            "authored DVR order must not change canonical control records"
        );
        assert_eq!(forward_words.len(), HEADER_WORDS + 2 * LAYER_WORDS);
        let layer_three_record = &forward_words[HEADER_WORDS..HEADER_WORDS + LAYER_WORDS];
        let layer_nine_record =
            &forward_words[HEADER_WORDS + LAYER_WORDS..HEADER_WORDS + 2 * LAYER_WORDS];

        assert_eq!(layer_three_record[0], 3);
        assert_eq!(&layer_three_record[1..4], &[14, 12, 10]);
        assert_eq!(layer_three_record[10], 0.1_f32.to_bits());
        assert_eq!(layer_three_record[12], 0.2_f32.to_bits());
        assert_eq!(&layer_three_record[24..30], &[0, 0, 0, 7, 3, 2]);
        assert_eq!(layer_three_record[35], (-2.5_f32).to_bits());
        assert_eq!(layer_three_record[47], 5.0_f32.to_bits());

        assert_eq!(layer_nine_record[0], 9);
        assert_eq!(&layer_nine_record[1..4], &[11, 10, 9]);
        assert_eq!(layer_nine_record[10], 0.25_f32.to_bits());
        assert_eq!(layer_nine_record[12], 0.6_f32.to_bits());
        assert_eq!(&layer_nine_record[24..30], &[2, 0, 0, 11, 2, 3]);
        assert_eq!(layer_nine_record[35], (-1.375_f32).to_bits());
        assert_eq!(layer_nine_record[47], 11.0_f32.to_bits());
    }

    fn frame_requirements(frame: u64, keys: &[BrickKey]) -> RenderRequirements {
        let layer = keys
            .first()
            .expect("test requirements are nonempty")
            .layer();
        assert!(keys.iter().all(|key| key.layer() == layer));
        RenderRequirements::new(
            &frame_intent(frame, layer),
            keys.iter()
                .enumerate()
                .map(|(index, key)| {
                    RenderRequirement::new(
                        *key,
                        if index == 0 {
                            RenderRequirementRole::FirstUsefulFrame
                        } else {
                            RenderRequirementRole::Refinement
                        },
                    )
                })
                .collect(),
        )
        .expect("test render requirements are valid")
    }

    #[test]
    fn resident_frame_leases_count_rebind_preview_replace_and_release_real_pins() {
        let a = key(7, 0, 0, 1);
        let shared = key(7, 0, 1, 1);
        let b = key(7, 0, 2, 1);
        let c = key(7, 0, 3, 1);
        let first_requirements = frame_requirements(1, &[a, shared]);
        let rebound_requirements = first_requirements
            .rebind(&frame_intent(3, LogicalLayerKey::new(7)))
            .expect("same-body frame rebind is valid");
        assert!(first_requirements.shares_resources_with(&rebound_requirements));

        let mut leases = ResidentFrameLeases::new();
        let pending = PendingLeaseQueue::new();
        let (first_lease, first_transitions) = leases.acquire(first_requirements, &pending);
        let (second_lease, second_transitions) =
            leases.acquire(frame_requirements(2, &[shared, b]), &pending);

        assert_eq!(first_transitions.pin_state_changes, vec![a, shared]);
        assert_eq!(second_transitions.pin_state_changes, vec![b]);
        assert!(leases.is_pinned(a));
        assert!(leases.is_pinned(shared));
        assert!(leases.is_pinned(b));
        assert!(!leases.is_pinned(c));
        assert!(!leases.is_pinned_excluding(a, first_lease));
        assert!(leases.is_pinned_excluding(shared, first_lease));
        assert!(leases.is_pinned_excluding(b, first_lease));

        assert!(
            leases
                .replace(first_lease, rebound_requirements, &pending)
                .pin_state_changes
                .is_empty()
        );
        assert_eq!(
            leases.requirements(first_lease).frame(),
            FrameIdentity::new(3)
        );
        assert!(leases.is_pinned(a));
        assert!(leases.is_pinned(shared));
        assert!(leases.is_pinned(b));

        assert!(!leases.is_pinned_excluding(a, first_lease));
        assert!(leases.is_pinned(a));
        assert!(!leases.is_pinned(c));
        assert_eq!(
            leases
                .replace(first_lease, frame_requirements(4, &[c]), &pending)
                .pin_state_changes,
            vec![a, c]
        );
        assert!(!leases.is_pinned(a));
        assert!(leases.is_pinned(shared));
        assert!(leases.is_pinned(b));
        assert!(leases.is_pinned(c));

        assert_eq!(
            leases
                .replace(first_lease, frame_requirements(5, &[c, a]), &pending)
                .pin_state_changes,
            vec![a],
            "an overlap key stays continuously pinned while only the new key transitions"
        );
        assert_eq!(
            leases
                .replace(first_lease, frame_requirements(6, &[c]), &pending)
                .pin_state_changes,
            vec![a],
            "removing the extra key must not manufacture a transition for the overlap key"
        );
        assert!(!leases.is_pinned(a));
        assert!(leases.is_pinned(c));

        assert_eq!(
            leases.release(second_lease).pin_state_changes,
            vec![shared, b]
        );
        assert!(!leases.is_pinned(shared));
        assert!(!leases.is_pinned(b));
        assert!(leases.is_pinned(c));
        assert_eq!(leases.release(first_lease).pin_state_changes, vec![c]);
        assert!(!leases.is_pinned(c));
        assert!(leases.is_empty());
    }

    #[test]
    fn same_frame_body_replacement_retains_overlap_before_releasing_predecessor() {
        let retired = key(18, 0, 0, 1);
        let overlap = key(18, 0, 1, 1);
        let admitted = key(18, 0, 2, 1);
        let frame = 41;
        let current = frame_requirements(frame, &[retired, overlap]);
        let successor = frame_requirements(frame, &[overlap, admitted]);
        assert!(!current.shares_resources_with(&successor));

        let pending = PendingLeaseQueue::new();
        let mut leases = ResidentFrameLeases::new();
        let (lease, initial) = leases.acquire(current, &pending);
        assert_eq!(initial.pin_state_changes, vec![retired, overlap]);

        let mutation = leases.replace(lease, successor.clone(), &pending);
        assert_eq!(
            mutation.pin_state_changes,
            vec![retired, admitted],
            "only the true set difference changes pin state; the overlap never becomes unpinned"
        );
        assert!(mutation.released_body.is_some());
        assert_eq!(
            leases.requirements(lease).frame(),
            FrameIdentity::new(frame)
        );
        assert!(leases.requirements(lease).shares_resources_with(&successor));
        assert!(!leases.is_pinned(retired));
        assert!(leases.is_pinned(overlap));
        assert!(leases.is_pinned(admitted));
        assert_eq!(
            leases.release(lease).pin_state_changes,
            vec![overlap, admitted]
        );
        assert!(leases.is_empty());
    }

    #[test]
    fn same_body_completion_and_rebind_reuse_one_pin_cohort() {
        let layer = LogicalLayerKey::new(19);
        let keys = (0..4_096)
            .map(|index| key(layer.ordinal(), 0, index, 1))
            .collect::<Vec<_>>();
        let requirements = frame_requirements(1, &keys);
        let rebound = requirements
            .rebind(&frame_intent(2, layer))
            .expect("same-body frame rebind is valid");
        let pending = PendingLeaseQueue::new();
        let mut leases = ResidentFrameLeases::new();
        let (presentation, initial) = leases.acquire(requirements, &pending);
        assert_eq!(initial.pin_state_changes.len(), keys.len());

        let pins_after_first_body_scan = leases.pin_counts.clone();
        let completion = leases.clone_for_completion(presentation);
        assert_eq!(leases.pin_counts, pins_after_first_body_scan);
        assert!(
            leases.relevant_pending(completion).is_none(),
            "a completion-held clone cannot own or scan a pending-offer index"
        );
        assert_eq!(leases.cohorts.len(), 1);
        assert_eq!(
            leases
                .cohorts
                .values()
                .next()
                .expect("one shared body cohort exists")
                .lease_count,
            2
        );

        let rebound_mutation = leases.replace(presentation, rebound, &pending);
        assert!(rebound_mutation.pin_state_changes.is_empty());
        assert!(rebound_mutation.released_body.is_none());
        assert_eq!(leases.pin_counts, pins_after_first_body_scan);

        let completion_release = leases.release(completion);
        assert!(completion_release.pin_state_changes.is_empty());
        assert!(completion_release.released_body.is_none());
        assert_eq!(leases.pin_counts, pins_after_first_body_scan);
        assert_eq!(
            leases.release(presentation).pin_state_changes.len(),
            keys.len()
        );
        assert!(leases.is_empty());
    }

    #[test]
    fn same_body_presentation_rebind_clones_pins_and_empty_offer_index_in_constant_work() {
        let layer = LogicalLayerKey::new(20);
        let keys = (0..4_096)
            .map(|index| key(layer.ordinal(), 0, index, 1))
            .collect::<Vec<_>>();
        let requirements = frame_requirements(1, &keys);
        let rebound = requirements
            .rebind(&frame_intent(2, layer))
            .expect("same-body frame rebind is valid");
        let pending = PendingLeaseQueue::new();
        let mut leases = ResidentFrameLeases::new();
        let (source, initial) = leases.acquire(requirements, &pending);
        assert_eq!(initial.pin_state_changes.len(), keys.len());

        let pins_after_first_body_scan = leases.pin_counts.clone();
        let cloned = leases.clone_for_presentation_rebind(source, rebound);
        assert_eq!(leases.pin_counts, pins_after_first_body_scan);
        assert_eq!(
            leases.relevant_pending(cloned),
            Some(&VecDeque::new()),
            "the cloned presentation owns an empty index for future offers"
        );
        assert_eq!(leases.requirements(cloned).frame(), FrameIdentity::new(2));
        assert_eq!(leases.cohorts.len(), 1);
        assert_eq!(
            leases
                .cohorts
                .values()
                .next()
                .expect("one shared body cohort exists")
                .lease_count,
            2
        );

        let cloned_release = leases.release(cloned);
        assert!(cloned_release.pin_state_changes.is_empty());
        assert!(cloned_release.released_body.is_none());
        assert_eq!(leases.pin_counts, pins_after_first_body_scan);
        assert_eq!(leases.release(source).pin_state_changes.len(), keys.len());
        assert!(leases.is_empty());
    }

    #[test]
    fn same_frame_prefetch_promotion_updates_the_owned_body_without_pin_churn() {
        let first = key(8, 0, 0, 1);
        let guard = key(8, 0, 1, 1);
        let mut canonical = vec![first, guard];
        canonical.sort_unstable();
        let body = PreparedResourceBody::new(canonical.into(), vec![first, guard].into(), None)
            .expect("test prepared body is valid");
        let prepared = PreparedRenderRequirements::new_with_required_prefix(
            DatasetResourceIdentity::Unverified(DatasetSourceId::new(1)),
            TimeIndex::new(0),
            vec![LogicalLayerKey::new(8)],
            body,
            1,
            1,
        )
        .expect("test prepared requirements are valid");
        let same_frame_intent = frame_intent(10, LogicalLayerKey::new(8));
        let current = prepared
            .bind(&same_frame_intent)
            .expect("unpromoted test requirements bind");
        let promoted_same_frame = prepared
            .promote_prefetch()
            .bind(&same_frame_intent)
            .expect("promoted test requirements bind");
        assert!(current.shares_resources_with(&promoted_same_frame));
        assert!(!current.prefetch_promoted());
        assert!(promoted_same_frame.prefetch_promoted());

        let pending = PendingLeaseQueue::new();
        let mut leases = ResidentFrameLeases::new();
        let (lease, acquired) = leases.acquire(current, &pending);
        assert_eq!(acquired.pin_state_changes, vec![first, guard]);
        let promoted = leases.replace(lease, promoted_same_frame, &pending);
        assert!(promoted.pin_state_changes.is_empty());
        assert!(promoted.released_body.is_none());
        assert!(leases.requirements(lease).prefetch_promoted());
        assert_eq!(leases.release(lease).pin_state_changes, vec![first, guard]);
    }

    #[test]
    fn gpu_failure_latch_preserves_the_first_typed_cause() {
        let latch = GpuFailureLatch::default();
        assert_eq!(latch.ensure_available(), Ok(()));

        latch.record_device_loss(wgpu::DeviceLostReason::Unknown);
        let derivative = wgpu::Error::Validation {
            source: Box::new(std::io::Error::other("derivative validation")),
            description: "derivative validation".to_owned(),
        };
        latch.record_uncaptured_error(&derivative);

        assert_eq!(
            latch.ensure_available(),
            Err(WgpuRenderRuntimeError::DeviceLost)
        );
    }

    #[test]
    fn gpu_failure_latch_classifies_wgpu_terminal_causes() {
        for reason in [
            wgpu::DeviceLostReason::Unknown,
            wgpu::DeviceLostReason::Destroyed,
        ] {
            let latch = GpuFailureLatch::default();
            latch.record_device_loss(reason);
            assert_eq!(
                latch.ensure_available(),
                Err(WgpuRenderRuntimeError::DeviceLost)
            );
        }

        let cases = [
            (
                wgpu::Error::OutOfMemory {
                    source: Box::new(std::io::Error::other("oom detail")),
                },
                WgpuRenderRuntimeError::DeviceOutOfMemory,
            ),
            (
                wgpu::Error::Internal {
                    source: Box::new(std::io::Error::other("internal detail")),
                    description: "internal detail".to_owned(),
                },
                WgpuRenderRuntimeError::BackendInternal,
            ),
            (
                wgpu::Error::Validation {
                    source: Box::new(std::io::Error::other("validation detail")),
                    description: "validation detail".to_owned(),
                },
                WgpuRenderRuntimeError::BackendValidation,
            ),
        ];
        for (error, expected) in cases {
            let latch = GpuFailureLatch::default();
            latch.record_uncaptured_error(&error);
            assert_eq!(latch.ensure_available(), Err(expected));
        }
    }

    #[test]
    fn terminal_device_rejects_upload_staging_before_mapped_access() {
        let mut staging = UploadStagingPool::new(COPY_ALIGNMENT);
        let latch = GpuFailureLatch::default();
        latch.record_device_loss(wgpu::DeviceLostReason::Destroyed);

        assert_eq!(
            staging.stage(usize::MAX, &[], None, &[], &[], &latch),
            Err(WgpuRenderRuntimeError::DeviceLost)
        );
    }

    #[test]
    fn timing_query_layout_distinguishes_envelope_render_and_absent_work() {
        let complete_envelope = TimingQueryLayout::new(8, true, true);
        assert_eq!(complete_envelope.batch_envelope, Some((8, 9)));
        assert_eq!(complete_envelope.render_pass, Some((10, 11)));
        assert_eq!(complete_envelope.query_count, 4);

        let render_only = TimingQueryLayout::new(8, false, true);
        assert_eq!(render_only.batch_envelope, None);
        assert_eq!(render_only.render_pass, Some((8, 9)));
        assert_eq!(render_only.query_count, 2);

        let absent = TimingQueryLayout::new(8, false, false);
        assert_eq!(absent.batch_envelope, None);
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
    fn bounded_pipeline_compiler_publishes_initial_then_pick_on_one_worker() {
        assert_eq!(PIPELINE_COMPILE_EVENT_CAPACITY, 2);
        let caller = thread::current().id();
        let worker_threads = Arc::new(Mutex::new(Vec::new()));
        let initial_threads = Arc::clone(&worker_threads);
        let pick_threads = Arc::clone(&worker_threads);
        let mut compiler = PipelineCompiler::spawn(
            move || {
                initial_threads
                    .lock()
                    .expect("worker-thread ledger is available")
                    .push(thread::current().id());
                Ok::<_, PipelineCompilationFailureCause>(11_u8)
            },
            move || {
                pick_threads
                    .lock()
                    .expect("worker-thread ledger is available")
                    .push(thread::current().id());
                Ok::<_, PipelineCompilationFailureCause>(22_u8)
            },
        )
        .expect("the bounded compiler worker starts");

        assert!(matches!(
            compiler
                .receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("initial readiness is published"),
            PipelineCompileEvent::InitialRenderReady(11)
        ));
        assert!(matches!(
            compiler
                .receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("pick readiness is published"),
            PipelineCompileEvent::Ready(22)
        ));
        compiler.join().expect("the sole worker exits cleanly");

        let threads = worker_threads
            .lock()
            .expect("worker-thread ledger is available");
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0], threads[1]);
        assert_ne!(threads[0], caller);
        assert!(matches!(
            compiler.receiver.try_recv(),
            Err(TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn bounded_pipeline_compiler_latches_initial_failure_and_skips_pick() {
        let pick_calls = Arc::new(AtomicUsize::new(0));
        let observed_pick_calls = Arc::clone(&pick_calls);
        let mut compiler = PipelineCompiler::<u8, u8>::spawn(
            || Err(PipelineCompilationFailureCause::Validation),
            move || {
                observed_pick_calls.fetch_add(1, Ordering::SeqCst);
                Err(PipelineCompilationFailureCause::DeviceOutOfMemory)
            },
        )
        .expect("the bounded compiler worker starts");

        assert!(matches!(
            compiler
                .receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("the first compilation failure is published"),
            PipelineCompileEvent::Failed {
                capability: PipelineCapability::InitialRender,
                cause: PipelineCompilationFailureCause::Validation,
            }
        ));
        compiler.join().expect("the sole worker exits cleanly");
        assert_eq!(pick_calls.load(Ordering::SeqCst), 0);
        assert!(matches!(
            compiler.receiver.try_recv(),
            Err(TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn bounded_pipeline_compiler_reports_pick_failure_after_initial_readiness() {
        let mut compiler = PipelineCompiler::spawn(
            || Ok::<_, PipelineCompilationFailureCause>(7_u8),
            || Err::<u8, _>(PipelineCompilationFailureCause::BackendInternal),
        )
        .expect("the bounded compiler worker starts");

        assert!(matches!(
            compiler
                .receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("initial readiness is published"),
            PipelineCompileEvent::InitialRenderReady(7)
        ));
        assert!(matches!(
            compiler
                .receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("the pick failure is published"),
            PipelineCompileEvent::Failed {
                capability: PipelineCapability::Pick,
                cause: PipelineCompilationFailureCause::BackendInternal,
            }
        ));
        compiler.join().expect("the sole worker exits cleanly");
    }

    #[test]
    fn pipeline_state_exposes_initial_render_before_pick_when_both_are_buffered() {
        let compiler = PipelineCompiler::spawn(
            || Ok::<_, PipelineCompilationFailureCause>(31_u8),
            || Ok::<_, PipelineCompilationFailureCause>(47_u8),
        )
        .expect("the bounded compiler worker starts");
        let mut state = PipelineState::compiling(compiler);
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match state.poll().expect("initial compilation succeeds") {
                PipelineReadiness::CompilingInitial => {
                    assert!(Instant::now() < deadline);
                    thread::sleep(std::time::Duration::from_millis(1));
                }
                PipelineReadiness::InitialRenderReady => break,
                PipelineReadiness::Ready => {
                    panic!("one poll must not consume both readiness events")
                }
            }
        }
        assert_eq!(state.initial, Some(31));
        assert_eq!(state.pick, None);
        assert!(
            state
                .capability_is_ready(PipelineCapability::InitialRender)
                .expect("the initial capability query succeeds")
        );
        assert!(
            !state
                .capability_is_ready(PipelineCapability::Pick)
                .expect("the pick capability query succeeds")
        );

        loop {
            match state.poll().expect("pick compilation succeeds") {
                PipelineReadiness::InitialRenderReady => {
                    assert!(Instant::now() < deadline);
                    thread::sleep(std::time::Duration::from_millis(1));
                }
                PipelineReadiness::Ready => break,
                PipelineReadiness::CompilingInitial => {
                    panic!("pipeline readiness cannot regress")
                }
            }
        }
        assert_eq!(state.initial, Some(31));
        assert_eq!(state.pick, Some(47));
    }

    #[test]
    fn pipeline_state_preserves_the_first_typed_compilation_failure() {
        let compiler = PipelineCompiler::<u8, u8>::spawn(
            || Err(PipelineCompilationFailureCause::DeviceOutOfMemory),
            || Err(PipelineCompilationFailureCause::BackendInternal),
        )
        .expect("the bounded compiler worker starts");
        let mut state = PipelineState::compiling(compiler);
        let expected = WgpuRenderRuntimeError::PipelineCompilationFailed {
            capability: PipelineCapability::InitialRender,
            cause: PipelineCompilationFailureCause::DeviceOutOfMemory,
        };
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match state.poll() {
                Ok(PipelineReadiness::CompilingInitial) => {
                    assert!(Instant::now() < deadline);
                    thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(error) => {
                    assert_eq!(error, expected);
                    break;
                }
                Ok(readiness) => panic!("failed compilation published {readiness:?}"),
            }
        }
        assert_eq!(state.poll(), Err(expected));
        assert_eq!(state.readiness(), Err(expected));
        assert_eq!(
            state.capability_is_ready(PipelineCapability::InitialRender),
            Err(expected)
        );
    }

    #[test]
    fn dropping_pipeline_compiler_cancels_without_waiting_for_blocked_initial_work() {
        let (entered_sender, entered_receiver) = sync_channel(1);
        let (release_sender, release_receiver) = sync_channel(1);
        let (initial_returned_sender, initial_returned_receiver) = sync_channel(1);
        let pick_calls = Arc::new(AtomicUsize::new(0));
        let observed_pick_calls = Arc::clone(&pick_calls);
        let compiler = PipelineCompiler::spawn(
            move || {
                entered_sender.send(()).expect("the test observes entry");
                release_receiver
                    .recv()
                    .expect("the test releases initial work");
                initial_returned_sender
                    .send(())
                    .expect("the test observes initial completion");
                Ok::<_, PipelineCompilationFailureCause>(())
            },
            move || {
                observed_pick_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, PipelineCompilationFailureCause>(())
            },
        )
        .expect("the bounded compiler worker starts");
        entered_receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("initial work is blocked inside the worker");
        let (drop_returned_sender, drop_returned_receiver) = sync_channel(1);
        let dropper = thread::spawn(move || {
            drop(compiler);
            drop_returned_sender
                .send(())
                .expect("the test observes nonblocking drop");
        });

        drop_returned_receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("drop must detach instead of waiting for driver compilation");
        release_sender
            .send(())
            .expect("the blocked initial operation is released");
        initial_returned_receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("the cancelled initial operation returns");
        dropper.join().expect("the drop observer exits");
        assert_eq!(pick_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cancelled_pipeline_compiler_suppresses_publication_and_pick() {
        let (entered_sender, entered_receiver) = sync_channel(1);
        let (release_sender, release_receiver) = sync_channel(1);
        let pick_calls = Arc::new(AtomicUsize::new(0));
        let observed_pick_calls = Arc::clone(&pick_calls);
        let mut compiler = PipelineCompiler::spawn(
            move || {
                entered_sender.send(()).expect("the test observes entry");
                release_receiver
                    .recv()
                    .expect("the test releases initial work");
                Ok::<_, PipelineCompilationFailureCause>(5_u8)
            },
            move || {
                observed_pick_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, PipelineCompilationFailureCause>(9_u8)
            },
        )
        .expect("the bounded compiler worker starts");
        entered_receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("initial work is blocked inside the worker");

        compiler.cancel_and_detach();
        release_sender
            .send(())
            .expect("the blocked initial operation is released");
        assert!(matches!(
            compiler
                .receiver
                .recv_timeout(std::time::Duration::from_secs(2)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        ));
        assert_eq!(pick_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn initial_pipeline_program_stops_at_the_first_failing_operation() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let plane_shader_calls = Arc::clone(&calls);
        let plane_calls = Arc::clone(&calls);
        let mip_shader_calls = Arc::clone(&calls);
        let mip_calls = Arc::clone(&calls);
        let dvr_shader_calls = Arc::clone(&calls);
        let dvr_calls = Arc::clone(&calls);
        let iso_shader_calls = Arc::clone(&calls);
        let iso_calls = Arc::clone(&calls);
        let mixed_shader_calls = Arc::clone(&calls);
        let mixed_calls = Arc::clone(&calls);
        let result = compile_initial_pipeline_program(
            move || {
                plane_shader_calls
                    .lock()
                    .expect("call ledger is available")
                    .push(1);
                Ok::<_, PipelineCompilationFailureCause>(())
            },
            move |_| {
                plane_calls
                    .lock()
                    .expect("call ledger is available")
                    .push(2);
                Ok::<_, PipelineCompilationFailureCause>(())
            },
            move || {
                mip_shader_calls
                    .lock()
                    .expect("call ledger is available")
                    .push(3);
                Ok::<_, PipelineCompilationFailureCause>(())
            },
            move |_| {
                mip_calls.lock().expect("call ledger is available").push(4);
                Ok::<_, PipelineCompilationFailureCause>(())
            },
            move || {
                dvr_shader_calls
                    .lock()
                    .expect("call ledger is available")
                    .push(5);
                Ok::<_, PipelineCompilationFailureCause>(())
            },
            move |_| {
                dvr_calls.lock().expect("call ledger is available").push(6);
                Ok::<_, PipelineCompilationFailureCause>(())
            },
            move || {
                iso_shader_calls
                    .lock()
                    .expect("call ledger is available")
                    .push(7);
                Ok::<_, PipelineCompilationFailureCause>(())
            },
            move |_| {
                iso_calls.lock().expect("call ledger is available").push(8);
                Ok::<_, PipelineCompilationFailureCause>(())
            },
            move || {
                mixed_shader_calls
                    .lock()
                    .expect("call ledger is available")
                    .push(9);
                Err::<(), _>(PipelineCompilationFailureCause::Validation)
            },
            move |_| {
                mixed_calls
                    .lock()
                    .expect("call ledger is available")
                    .push(10);
                Err::<u8, _>(PipelineCompilationFailureCause::DeviceOutOfMemory)
            },
        );

        assert_eq!(result, Err(PipelineCompilationFailureCause::Validation));
        assert_eq!(
            *calls.lock().expect("call ledger is available"),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9]
        );
    }

    struct QueueFixtureLease {
        key: BrickKey,
        descriptor: ResourcePayloadDescriptor,
        values: Box<[u8]>,
        validity: Option<Box<[u8]>>,
        facts: ResourcePayloadFacts,
    }

    impl QueueFixtureLease {
        fn arc(key: BrickKey) -> Arc<dyn ResourceLease> {
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

        fn all_invalid_arc(key: BrickKey) -> Arc<dyn ResourceLease> {
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

        fn large_valid_arc(key: BrickKey) -> Arc<dyn ResourceLease> {
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
        fn key(&self) -> BrickKey {
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
    fn global_residency_inbox_preserves_first_seen_order_across_bounded_drains() {
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
        let resident = BTreeMap::new();
        assert_eq!(queue.offer(&cohort_a, &resident).unwrap(), keys(&cohort_a));
        assert_eq!(queue.offer(&cohort_b, &resident).unwrap(), keys(&cohort_b));
        assert_eq!(queue.offer(&cohort_c, &resident).unwrap(), keys(&cohort_c));
        assert!(queue.offer(&cohort_a, &resident).unwrap().is_empty());
        assert_eq!(queue.len(), MAX_UPLOADS * 2 + 1);

        let first = queue
            .select_for_execution(&resident, |_| true)
            .expect("the first transfer cohort fits")
            .leases;
        assert_eq!(keys(&first), keys(&cohort_a));
        queue.remove_batch(&keys(&first).into_iter().collect());

        let second = queue
            .select_for_execution(&resident, |_| true)
            .expect("the second transfer cohort fits")
            .leases;
        assert_eq!(keys(&second), keys(&cohort_b));
        queue.remove_batch(&keys(&second).into_iter().collect());

        let third = queue
            .select_for_execution(&resident, |_| true)
            .expect("the final transfer cohort fits")
            .leases;
        assert_eq!(keys(&third), keys(&cohort_c));
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn global_residency_inbox_drains_all_invalid_pages_as_zero_transfer_bytes() {
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
        let resident = BTreeMap::new();
        assert_eq!(queue.offer(&cohort, &resident).unwrap().len(), cohort.len());
        assert_eq!(queue.len(), cohort.len());
        assert_eq!(
            queue
                .select_for_execution(&resident, |_| true)
                .expect("metadata-only pages consume no transfer bytes")
                .leases
                .len(),
            cohort.len()
        );

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
        assert_eq!(queue.offer(&mixed, &resident).unwrap().len(), mixed.len());
        assert_eq!(queue.len(), mixed.len());
        assert_eq!(
            queue
                .select_for_execution(&resident, |_| true)
                .expect("the invalid tail fits after an exact 8-MiB valid prefix")
                .leases
                .len(),
            mixed.len()
        );
    }

    #[test]
    fn global_residency_inbox_retirement_is_idempotent_and_preserves_survivor_order() {
        let offered = [9, 1, 5]
            .into_iter()
            .map(|origin| QueueFixtureLease::arc(key(0, 0, origin, 1)))
            .collect::<Vec<_>>();
        let resident = BTreeMap::new();
        let mut queue = PendingLeaseQueue::new();
        queue
            .offer(&offered, &resident)
            .expect("the ordered offers fit the global inbox");

        let retired = [offered[1].key(), key(0, 0, 99, 1)]
            .into_iter()
            .collect::<BTreeSet<_>>();
        queue.remove_batch(&retired);
        queue.remove_batch(&retired);

        let remaining = queue
            .select_for_execution(&resident, |_| true)
            .expect("the surviving offers fit one execution")
            .leases
            .into_iter()
            .map(|lease| lease.key())
            .collect::<Vec<_>>();
        assert_eq!(remaining, vec![offered[0].key(), offered[2].key()]);
    }

    #[test]
    fn frame_lease_index_skips_65k_unrelated_offers_and_preserves_reoffer_order() {
        let unrelated_count = MAX_RENDER_REQUIREMENTS - 2;
        let mut offered = (0..unrelated_count)
            .map(|origin| QueueFixtureLease::arc(key(0, 0, origin as u64, 1)))
            .collect::<Vec<_>>();
        let relevant_first = key(1, 0, 9, 1);
        let relevant_second = key(1, 0, 1, 1);
        offered.push(QueueFixtureLease::arc(relevant_first));
        offered.push(QueueFixtureLease::arc(relevant_second));
        let rebuilt_body_key = offered[123].key();

        let resident = BTreeMap::new();
        let mut queue = PendingLeaseQueue::new();
        assert_eq!(
            queue
                .offer(&offered, &resident)
                .expect("the maximum-size global inbox is admitted")
                .len(),
            MAX_RENDER_REQUIREMENTS
        );

        let first_requirements = frame_requirements(20, &[relevant_first, relevant_second]);
        let rebound_requirements = first_requirements
            .rebind(&frame_intent(21, LogicalLayerKey::new(1)))
            .expect("same-body frame rebind is valid");
        let mut leases = ResidentFrameLeases::new();
        let (lease, _) = leases.acquire(first_requirements, &queue);

        let selection = queue
            .select_indexed_for_execution(&resident, leases.relevant_pending(lease).unwrap())
            .expect("the relevant indexed offers fit");
        assert_eq!(selection.visited_offers, 2);
        assert_eq!(
            selection
                .leases
                .iter()
                .map(|lease| lease.key())
                .collect::<Vec<_>>(),
            vec![relevant_first, relevant_second]
        );
        assert_eq!(
            selection
                .visited_offers
                .saturating_sub(selection.leases.len()),
            0,
            "indexed selection must not visit any of the 65k unrelated offers"
        );

        assert!(
            leases
                .replace(lease, rebound_requirements, &queue)
                .pin_state_changes
                .is_empty()
        );
        assert_eq!(
            leases.relevant_pending(lease),
            Some(&VecDeque::from([relevant_first, relevant_second])),
            "same-body rebind preserves the existing relevance index"
        );

        let removed = BTreeSet::from([relevant_first]);
        queue.remove_batch(&removed);
        leases.remove_offers(&removed);
        let reoffer = vec![QueueFixtureLease::arc(relevant_first)];
        let admitted = queue
            .offer(&reoffer, &resident)
            .expect("a retired relevant offer can be admitted again");
        leases.append_offers(&admitted);
        let reordered = queue
            .select_indexed_for_execution(&resident, leases.relevant_pending(lease).unwrap())
            .expect("the reoffered indexed cohort fits");
        assert_eq!(reordered.visited_offers, 2);
        assert_eq!(
            reordered
                .leases
                .iter()
                .map(|lease| lease.key())
                .collect::<Vec<_>>(),
            vec![relevant_second, relevant_first],
            "batch removal eliminates the stale entry and reoffer appends exactly once"
        );

        leases.replace(lease, frame_requirements(22, &[rebuilt_body_key]), &queue);
        let rebuilt = queue
            .select_indexed_for_execution(&resident, leases.relevant_pending(lease).unwrap())
            .expect("changed-body relevance rebuild fits");
        assert_eq!(rebuilt.visited_offers, 1);
        assert_eq!(rebuilt.leases[0].key(), rebuilt_body_key);
        leases.release(lease);
        assert!(leases.is_empty());
    }

    #[test]
    fn residency_eviction_retry_and_ack_preserve_a_later_re_eviction() {
        let first = key(0, 0, 11, 1);
        let second = key(0, 0, 12, 1);
        let mut recently_evicted = BTreeMap::new();
        let mut recently_evicted_order = VecDeque::new();
        let mut next_sequence = 1;

        record_residency_eviction(
            &mut recently_evicted,
            &mut recently_evicted_order,
            &mut next_sequence,
            first,
        );
        record_residency_eviction(
            &mut recently_evicted,
            &mut recently_evicted_order,
            &mut next_sequence,
            second,
        );

        let first_retry =
            collect_pending_residency_evictions(&recently_evicted, &recently_evicted_order, 1);
        assert_eq!(
            first_retry.as_ref(),
            &[GpuResidencyEvictionEvent::new(first, 1)]
        );
        assert_eq!(
            collect_pending_residency_evictions(&recently_evicted, &recently_evicted_order, 1),
            first_retry,
            "an unacknowledged event must be retried identically"
        );
        assert!(
            collect_pending_residency_evictions(&recently_evicted, &recently_evicted_order, 0)
                .is_empty(),
            "the query allocation and result must honor the caller's bound"
        );

        // Becoming resident cancels the current event but deliberately leaves
        // its order entry stale; the query must filter that stale entry.
        recently_evicted.remove(&first);
        record_residency_eviction(
            &mut recently_evicted,
            &mut recently_evicted_order,
            &mut next_sequence,
            first,
        );
        acknowledge_residency_evictions_through(
            &mut recently_evicted,
            &mut recently_evicted_order,
            1,
        );
        assert_eq!(
            collect_pending_residency_evictions(
                &recently_evicted,
                &recently_evicted_order,
                usize::MAX,
            )
            .as_ref(),
            &[
                GpuResidencyEvictionEvent::new(second, 2),
                GpuResidencyEvictionEvent::new(first, 3),
            ],
            "acknowledging the old event must not erase the later re-eviction"
        );

        acknowledge_residency_evictions_through(
            &mut recently_evicted,
            &mut recently_evicted_order,
            2,
        );
        assert_eq!(
            collect_pending_residency_evictions(
                &recently_evicted,
                &recently_evicted_order,
                usize::MAX,
            )
            .as_ref(),
            &[GpuResidencyEvictionEvent::new(first, 3)]
        );
        acknowledge_residency_evictions_through(
            &mut recently_evicted,
            &mut recently_evicted_order,
            3,
        );
        assert!(recently_evicted.is_empty());
        assert!(recently_evicted_order.is_empty());
    }

    #[test]
    fn eviction_event_capacity_refusal_and_stale_compaction_preserve_current_events() {
        let mut recently_evicted = BTreeMap::new();
        let mut recently_evicted_order = VecDeque::new();
        let mut next_sequence = 1;
        for origin in 0..MAX_RENDER_REQUIREMENTS {
            record_residency_eviction(
                &mut recently_evicted,
                &mut recently_evicted_order,
                &mut next_sequence,
                key(2, 0, origin as u64, 1),
            );
        }
        let first_before_refusal =
            collect_pending_residency_evictions(&recently_evicted, &recently_evicted_order, 1);
        let overflow = key(2, 0, MAX_RENDER_REQUIREMENTS as u64, 1);
        assert_eq!(
            preflight_residency_eviction_events(
                &recently_evicted,
                &BTreeSet::from([overflow]),
                &BTreeSet::new(),
                MAX_RENDER_REQUIREMENTS,
            ),
            Err(
                WgpuRenderRuntimeError::ResidencyEvictionEventCapacityExceeded {
                    actual: MAX_RENDER_REQUIREMENTS + 1,
                    maximum: MAX_RENDER_REQUIREMENTS,
                }
            )
        );
        assert_eq!(recently_evicted.len(), MAX_RENDER_REQUIREMENTS);
        assert_eq!(
            collect_pending_residency_evictions(&recently_evicted, &recently_evicted_order, 1,),
            first_before_refusal,
            "capacity refusal leaves every existing unacknowledged event untouched"
        );

        let repeated = key(2, 0, 0, 1);
        for _ in 0..=MAX_RENDER_REQUIREMENTS {
            record_residency_eviction(
                &mut recently_evicted,
                &mut recently_evicted_order,
                &mut next_sequence,
                repeated,
            );
        }
        assert!(recently_evicted_order.len() <= MAX_RENDER_REQUIREMENTS * 2);
        let current = collect_pending_residency_evictions(
            &recently_evicted,
            &recently_evicted_order,
            usize::MAX,
        );
        assert_eq!(
            current.len(),
            MAX_RENDER_REQUIREMENTS,
            "stale-order compaction retains one current version for every pending key"
        );
        assert!(
            current
                .iter()
                .all(|event| { recently_evicted.get(&event.key()) == Some(&event.sequence()) })
        );
    }

    #[test]
    fn eviction_event_preflight_accepts_net_neutral_readmission_and_refuses_net_growth() {
        let reuploaded = key(3, 0, 0, 1);
        let retained = key(3, 0, 1, 1);
        let newly_evicted = key(3, 0, 2, 1);
        let additional_eviction = key(3, 0, 3, 1);
        let recently_evicted = BTreeMap::from([(reuploaded, 1), (retained, 2)]);
        let original = recently_evicted.clone();
        let admitted = BTreeSet::from([reuploaded]);

        assert_eq!(
            preflight_residency_eviction_events(
                &recently_evicted,
                &BTreeSet::from([newly_evicted]),
                &admitted,
                2,
            ),
            Ok(()),
            "reuploading A while evicting new B keeps a full event ledger net neutral"
        );
        assert_eq!(recently_evicted, original);

        assert_eq!(
            preflight_residency_eviction_events(
                &recently_evicted,
                &BTreeSet::from([newly_evicted, additional_eviction]),
                &admitted,
                2,
            ),
            Err(
                WgpuRenderRuntimeError::ResidencyEvictionEventCapacityExceeded {
                    actual: 3,
                    maximum: 2,
                }
            ),
            "one cancelled current event cannot admit two distinct new events"
        );
        assert_eq!(
            recently_evicted, original,
            "both preflight outcomes leave the current event ledger untouched"
        );
    }

    #[test]
    fn requirement_preflight_admits_a_scale_s0_working_set() {
        let resources = (0..32_768)
            .map(|x| requirement(key(0, 0, x, 1)))
            .collect::<Vec<_>>();
        assert_eq!(validate_requirement_slice(&resources), Ok(()));
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
    fn initial_payload_commitment_is_small_bounded_and_follows_logical_segments() {
        let gib = 1024_u64 * 1024 * 1024;
        assert_eq!(
            initial_payload_segment_commitments(&[gib, gib, gib, 0], 64 * 1024 * 1024),
            [64 * 1024 * 1024, 0, 0, 0]
        );
        assert_eq!(
            initial_payload_segment_commitments(
                &[4 * 1024 * 1024, 4 * 1024 * 1024, 256 * 1024, 0],
                64 * 1024 * 1024,
            ),
            [4 * 1024 * 1024, 4 * 1024 * 1024, 256 * 1024, 0]
        );
    }

    #[test]
    fn allocator_growth_adds_only_the_new_tail_and_rolls_back_exactly() {
        let mut allocator = ArenaAllocator::new(1024);
        assert_eq!(allocator.allocate(256), Some(0));
        let before = allocator.ranges();
        allocator.grow(1024, 4096);
        assert_eq!(allocator.available_bytes(), 4096 - 256);
        assert_eq!(allocator.largest_contiguous_bytes(), 4096 - 256);
        allocator.remove_free_tail(1024, 4096);
        assert_eq!(allocator.ranges(), before);
    }

    #[test]
    fn bounded_geometric_commitment_preserves_each_segment_without_large_overshoot() {
        let logical = 4 * 1024;
        let required_ends = [1536, 1536];
        let targets = required_ends.map(|required| {
            bounded_geometric_payload_commitment(256, logical, required)
                .expect("the independently planned segment extent fits")
        });
        assert_eq!(targets, required_ends);
        assert!(
            targets
                .into_iter()
                .zip(required_ends)
                .all(|(target, required)| target >= required),
            "aggregate byte sufficiency cannot substitute for per-segment placement"
        );
        assert_eq!(
            bounded_geometric_payload_commitment(2048, logical, 1024),
            Ok(2048),
            "ordinary interaction never shrinks committed payload buffers"
        );
        assert_eq!(
            bounded_geometric_payload_commitment(256, logical, logical + COPY_ALIGNMENT),
            Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::PayloadResidency,
                requested_bytes: logical + COPY_ALIGNMENT,
                available_bytes: logical,
            })
        );

        let gib = 1024_u64 * 1024 * 1024;
        let required = gib + COPY_ALIGNMENT;
        let target = bounded_geometric_payload_commitment(gib, 3 * gib, required)
            .expect("the larger logical arena admits a small incremental increase");
        assert!(target >= required);
        assert!(target <= required + MAX_PAYLOAD_GROWTH_HEADROOM_BYTES);
        assert_ne!(
            target,
            2 * gib,
            "doubling may not manufacture a nearly one-GiB commitment overshoot"
        );
    }

    #[test]
    fn allocation_order_opens_empty_segments_before_copying_populated_prefixes() {
        let mut resident = BTreeMap::new();
        resident.insert(
            key(0, 0, 0, 1),
            ResidentResource {
                segment: 0,
                offset: 0,
                allocated_bytes: 4096,
                validity_offset: None,
                dtype_bytes: 1,
                minimum: 0.0,
                maximum: 1.0,
                any_valid: true,
                all_valid: true,
                last_used_frame: 1,
                grid: None,
                page_record_index: 0,
            },
        );
        resident.insert(
            key(0, 0, 1, 1),
            ResidentResource {
                segment: 2,
                offset: 0,
                allocated_bytes: 2048,
                validity_offset: None,
                dtype_bytes: 1,
                minimum: 0.0,
                maximum: 1.0,
                any_valid: true,
                all_valid: true,
                last_used_frame: 1,
                grid: None,
                page_record_index: 1,
            },
        );
        assert_eq!(
            payload_segment_allocation_order(3, &resident),
            vec![1, 2, 0],
            "an unused segment has zero preservation copy cost, followed by the smaller prefix"
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
    fn compaction_scratch_never_steals_the_small_runtime_upload_envelope() {
        let eight_mib = 8 * 1024 * 1024;
        assert_eq!(transfer_capacity_partition(eight_mib), Ok((eight_mib, 0)));
        assert_eq!(
            transfer_capacity_partition(eight_mib * 2),
            Ok((eight_mib, eight_mib))
        );
        assert_eq!(
            transfer_capacity_partition(COPY_ALIGNMENT - 1),
            Err(WgpuRenderRuntimeError::InvalidConfiguration)
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
    fn fragmented_payload_failure_reports_placeability_not_aggregate_exhaustion() {
        let mut allocator = ArenaAllocator::new(1024);
        let offsets = (0..4)
            .map(|_| allocator.allocate(256).unwrap())
            .collect::<Vec<_>>();
        allocator.release(offsets[0], 256);
        allocator.release(offsets[2], 256);

        assert_eq!(allocator.available_bytes(), 512);
        assert_eq!(allocator.largest_contiguous_bytes(), 256);
        assert_eq!(allocator.allocate(384), None);
        assert_eq!(
            classify_payload_allocation_failure(
                384,
                allocator.available_bytes(),
                allocator.largest_contiguous_bytes(),
            ),
            WgpuRenderRuntimeError::PayloadPlacementUnavailable {
                requested_bytes: 384,
                total_free_bytes: 512,
                largest_contiguous_bytes: 256,
            }
        );
        assert_eq!(
            classify_payload_allocation_failure(
                768,
                allocator.available_bytes(),
                allocator.largest_contiguous_bytes(),
            ),
            WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::PayloadResidency,
                requested_bytes: 768,
                available_bytes: 512,
            }
        );
    }

    #[test]
    fn segment_compaction_stably_packs_payload_and_preserves_validity_delta() {
        let first = key(0, 0, 0, 1);
        let second = key(0, 0, 1, 1);
        let (relocations, allocator) = prepare_segment_payload_compaction(
            2,
            1024,
            vec![(second, 512, 256, Some(704)), (first, 0, 256, None)],
        );

        assert_eq!(relocations.len(), 1);
        let moved = relocations[0];
        assert_eq!(moved.key, second);
        assert_eq!(moved.segment, 2);
        assert_eq!(moved.source_offset, 512);
        assert_eq!(moved.destination_offset, 256);
        assert_eq!(moved.bytes, 256);
        assert_eq!(moved.destination_validity_offset, Some(448));
        assert_eq!(allocator.available_bytes(), 512);
        assert_eq!(allocator.largest_contiguous_bytes(), 512);
        assert_eq!(allocator.ranges(), vec![(512, 1024)]);
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
        let entry_body_bytes = size_of::<BrickKey>()
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
    fn requirement_preflight_accepts_api_validated_multiscale_resources() {
        assert_eq!(
            validate_requirement_slice(&[
                requirement(key(0, 0, 0, 1)),
                requirement(key(0, 1, 0, 1)),
            ]),
            Ok(())
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
    fn shader_contract_uses_direct_pages_and_bounded_brick_traversal() {
        let mip_core = include_str!("mip_core.wgsl");
        let shader = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            include_str!("shader_common.wgsl"),
            include_str!("volume_common.wgsl"),
            include_str!("volume_composite.wgsl"),
            include_str!("dvr_optics.wgsl"),
            include_str!("dvr_core.wgsl"),
            include_str!("dvr_shader.wgsl")
        );
        let pick_shader = include_str!("pick_shader.wgsl");

        assert!(shader.contains("fn lookup_page"));
        assert!(shader.contains("fn page_exit_distance"));
        assert!(mip_core.contains("resource_f32(page.resource_index, 13u) <= maximum"));
        assert!(shader.contains("const ALPHA_TERMINATION: f32 = 0.999"));
        assert!(shader.contains("alpha >= ALPHA_TERMINATION && control[28u] != 0u"));
        assert!(!shader.contains("for (var resource_index"));
        assert!(!shader.contains("resource_index += 1u"));
        assert!(!shader.contains("MAX_RAY_SAMPLES"));
        assert!(!pick_shader.contains("MAX_RAY_SAMPLES"));
        assert!(pick_shader.contains("transmittance <= best_score"));
        assert!(pick_shader.contains("control[28u] != 0u"));
        assert!(shader.contains("let full_resource_plus_one = layer_word(layer_index, 62u)"));
        assert!(shader.contains("fn sample_grid_linear_in_resolved_page"));
        assert!(shader.contains("sample_resource_at(address, coordinate)"));
        assert!(shader.contains("sample_linear_tap"));
        let general_dvr = shader
            .split_once("fn render_general_dvr")
            .and_then(|(_, tail)| tail.split_once("fn render_dvr_fragment"))
            .map(|(body, _)| body)
            .expect("general DVR function is present in the dedicated DVR contract");
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
    fn plane_kernel_is_structurally_isolated_from_volume_traversal() {
        let plane = format!(
            "{}\n{}\n{}",
            include_str!("shader_common.wgsl"),
            include_str!("plane_sampling.wgsl"),
            include_str!("plane_shader.wgsl")
        );

        for required in [
            "@group(0) @binding(0)",
            "@group(0) @binding(6)",
            "fn render_plane_layer",
            "fn render_plane_fragment",
            "fn fs_plane_color",
            "fn fs_plane_validation",
            "fn sample_plane_multiscale",
        ] {
            assert!(
                plane.contains(required),
                "the dedicated Plane module is missing {required}"
            );
        }
        for forbidden in [
            "VolumeRay",
            "volume_ray",
            "intersect_grid",
            "page_exit_distance",
            "render_mip",
            "render_dvr",
            "render_iso",
            "ALPHA_TERMINATION",
            "pick_main",
            "control[3u]",
        ] {
            assert!(
                !plane.contains(forbidden),
                "the dedicated Plane module still contains {forbidden}"
            );
        }
    }

    #[test]
    fn mip_kernel_is_structurally_isolated_from_other_volume_modes() {
        let mip_core = include_str!("mip_core.wgsl");
        let mixed_shader = include_str!("mixed_shader.wgsl");
        let mip = format!(
            "{}\n{}\n{}\n{}",
            include_str!("shader_common.wgsl"),
            include_str!("volume_common.wgsl"),
            mip_core,
            include_str!("mip_shader.wgsl")
        );

        for required in [
            "fn page_exit_distance",
            "fn segment_end_index",
            "fn sample_in_page",
            "fn render_mip",
            "fn render_mip_fragment",
            "fn fs_mip_color",
            "fn fs_mip_validation",
        ] {
            assert!(
                mip.contains(required),
                "the dedicated MIP module is missing {required}"
            );
        }
        for forbidden in [
            "ALPHA_TERMINATION",
            "dvr_",
            "render_dvr",
            "iso_",
            "render_iso",
            "render_volume_layer",
            "fn render_fragment",
            "pick_main",
            "control[3u]",
            "control[27u]",
            "render_plane",
            "fs_plane",
            "composite_over",
            "layer_word(layer_index, 18u)",
            "PickQuery",
            "@binding(7)",
            "@binding(8)",
            "array<u32, 64>",
            "array<PixelResult, 64>",
            "mip_maximum_is_terminal",
            "publish_",
            "fidelity",
            "descriptor_evidence",
        ] {
            assert!(
                !mip.contains(forbidden),
                "the dedicated MIP module still contains {forbidden}"
            );
        }
        assert!(!mixed_shader.contains("fn render_mip("));
        assert!(!mixed_shader.contains("render_mip_fragment"));
        assert!(!mixed_shader.contains("fs_mip_"));
        assert_eq!(mip_core.matches("fn render_mip(").count(), 1);
        assert!(
            mip_core
                .find("transfer_value(layer_index, maximum)")
                .is_some_and(|transfer| {
                    mip_core
                        .find("maximum = select")
                        .is_some_and(|maximum| transfer > maximum)
                }),
            "MIP must take the raw maximum during traversal and transfer it once afterward"
        );
        let mixed = format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            include_str!("shader_common.wgsl"),
            include_str!("volume_common.wgsl"),
            include_str!("volume_composite.wgsl"),
            mip_core,
            include_str!("dvr_optics.wgsl"),
            include_str!("dvr_core.wgsl"),
            include_str!("iso_gradient.wgsl"),
            include_str!("iso_core.wgsl"),
            mixed_shader
        );
        assert_eq!(mixed.matches("fn render_mip(").count(), 1);
        assert!(
            mixed.contains("return render_mip("),
            "Mixed must call the same canonical MIP core"
        );
    }

    #[test]
    fn dvr_kernel_is_structurally_isolated_and_uses_physical_joint_integration() {
        let dvr_optics = include_str!("dvr_optics.wgsl");
        let dvr_core = include_str!("dvr_core.wgsl");
        let dvr_shader = include_str!("dvr_shader.wgsl");
        let mixed_shader = include_str!("mixed_shader.wgsl");
        let dvr = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            include_str!("shader_common.wgsl"),
            include_str!("volume_common.wgsl"),
            include_str!("volume_composite.wgsl"),
            dvr_optics,
            dvr_core,
            dvr_shader
        );

        for required in [
            "fn dvr_effective_alpha",
            "fn dvr_effective_tau",
            "fn dvr_can_terminate",
            "fn render_dvr_layer",
            "fn render_fused_dvr",
            "fn render_general_dvr",
            "fn render_dvr_fragment",
            "fn fs_dvr_color",
            "fn fs_dvr_validation",
            "step * length(world_direction)",
            "tau_total += tau",
            "weighted_rgb += color * tau",
            "control[28u] != 0u",
        ] {
            assert!(
                dvr.contains(required),
                "the dedicated DVR module is missing {required}"
            );
        }
        for forbidden in [
            "render_plane",
            "fs_plane",
            "render_mip",
            "fs_mip",
            "iso_",
            "render_iso",
            "render_volume_layer",
            "layer_word(layer_index, 18u)",
            "fn render_fragment",
            "pick_main",
            "@binding(7)",
            "@binding(8)",
            "publish_",
            "fidelity",
            "descriptor_evidence",
        ] {
            assert!(
                !dvr.contains(forbidden),
                "the dedicated DVR module still contains {forbidden}"
            );
        }
        assert!(!mixed_shader.contains("fn render_dvr_layer"));
        assert!(!mixed_shader.contains("fn render_fused_dvr"));
        assert!(!mixed_shader.contains("fn render_general_dvr"));
        assert!(!mixed_shader.contains("fs_dvr_"));
        assert_eq!(dvr_core.matches("fn render_dvr_layer").count(), 1);
        assert!(!dvr_core.contains("fn dvr_effective_alpha"));
        assert!(!dvr_core.contains("fn dvr_effective_tau"));
        assert!(!dvr_core.contains("fn render_fused_dvr"));
        assert!(!dvr_core.contains("fn render_general_dvr"));
        assert!(dvr_optics.contains("fn dvr_effective_alpha"));
        assert!(dvr_shader.contains("fn dvr_effective_tau"));
        assert!(dvr_shader.contains("fn render_fused_dvr"));
        assert!(dvr_shader.contains("fn render_general_dvr"));
        assert!(
            mixed_shader.contains("return render_dvr_layer("),
            "Mixed must call the same canonical single-layer DVR core"
        );
        let mixed = format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            include_str!("shader_common.wgsl"),
            include_str!("volume_common.wgsl"),
            include_str!("volume_composite.wgsl"),
            include_str!("mip_core.wgsl"),
            dvr_optics,
            dvr_core,
            include_str!("iso_gradient.wgsl"),
            include_str!("iso_core.wgsl"),
            mixed_shader
        );
        assert!(!mixed.contains("fn render_fused_dvr"));
        assert!(!mixed.contains("fn render_general_dvr"));
    }

    #[test]
    fn iso_and_mixed_kernels_have_distinct_explicit_semantics() {
        let iso_gradient = include_str!("iso_gradient.wgsl");
        let iso_core = include_str!("iso_core.wgsl");
        let iso_shader = include_str!("iso_shader.wgsl");
        let mixed_shader = include_str!("mixed_shader.wgsl");
        let iso = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            include_str!("shader_common.wgsl"),
            include_str!("volume_common.wgsl"),
            include_str!("volume_composite.wgsl"),
            iso_gradient,
            iso_core,
            iso_shader
        );

        for required in [
            "fn iso_gradient",
            "fn iso_lighting",
            "fn render_iso(",
            "fn render_iso_layer",
            "fn render_iso_stack",
            "fn render_iso_fragment",
            "fn fs_iso_color",
            "fn fs_iso_validation",
            "sample_distance * length(world_direction)",
            "hits[insertion - 1u].depth <= hit.depth",
            "result = composite_over(hits[index], result)",
        ] {
            assert!(
                iso.contains(required),
                "the dedicated ISO module is missing {required}"
            );
        }
        for forbidden in [
            "render_plane",
            "fs_plane",
            "render_mip",
            "fs_mip",
            "render_dvr",
            "fs_dvr",
            "dvr_",
            "layer_word(layer_index, 18u)",
            "render_mixed",
            "fs_mixed",
            "pick_main",
            "@binding(7)",
            "@binding(8)",
            "publish_",
            "fidelity",
            "descriptor_evidence",
        ] {
            assert!(
                !iso.contains(forbidden),
                "the dedicated ISO module still contains {forbidden}"
            );
        }
        assert_eq!(iso_gradient.matches("fn iso_gradient").count(), 1);
        assert!(!iso_gradient.contains("fn iso_lighting"));
        assert_eq!(iso_core.matches("fn render_iso(").count(), 1);
        assert!(!iso_core.contains("fn render_iso_stack"));
        assert!(!iso_shader.contains("layer_word(layer_index, 18u)"));

        let mixed = format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            include_str!("shader_common.wgsl"),
            include_str!("volume_common.wgsl"),
            include_str!("volume_composite.wgsl"),
            include_str!("mip_core.wgsl"),
            include_str!("dvr_optics.wgsl"),
            include_str!("dvr_core.wgsl"),
            iso_gradient,
            iso_core,
            mixed_shader
        );
        for required in [
            "fn render_mixed_layer",
            "fn render_mixed_fragment",
            "fn fs_mixed_color",
            "fn fs_mixed_validation",
            "if mode == 0u",
            "if mode == 1u",
            "if mode == 2u",
            "return render_mip(",
            "return render_dvr_layer(",
            "return render_iso(",
            "invalid.covered = 0u",
            "pixel = composite_over(",
            "pixel,\n            render_mixed_layer",
        ] {
            assert!(
                mixed.contains(required),
                "the dedicated Mixed module is missing {required}"
            );
        }
        for forbidden in [
            "render_fused_dvr",
            "render_general_dvr",
            "render_dvr_fragment",
            "render_iso_stack",
            "render_iso_fragment",
            "fs_dvr",
            "fs_iso",
            "fs_mip",
            "render_plane",
            "pick_main",
            "array<PixelResult, 64>",
            "array<u32, 64>",
            "all_iso",
            "all_dvr",
            "publish_",
            "fidelity",
            "descriptor_evidence",
        ] {
            assert!(
                !mixed.contains(forbidden),
                "the dedicated Mixed module still contains {forbidden}"
            );
        }
        assert_eq!(mixed.matches("fn render_mip(").count(), 1);
        assert_eq!(mixed.matches("fn render_dvr_layer(").count(), 1);
        assert_eq!(mixed.matches("fn render_iso(").count(), 1);
    }

    #[test]
    fn pick_kernel_is_structurally_isolated_from_color_kernels() {
        let pick = format!(
            "{}\n{}\n{}\n{}\n{}",
            include_str!("shader_common.wgsl"),
            include_str!("volume_common.wgsl"),
            include_str!("dvr_optics.wgsl"),
            include_str!("iso_gradient.wgsl"),
            include_str!("pick_shader.wgsl")
        );

        for required in [
            "@group(0) @binding(0)",
            "@group(0) @binding(6)",
            "@group(0) @binding(7)",
            "@group(0) @binding(8)",
            "@compute @workgroup_size(1)",
            "fn pick_main",
            "fn volume_ray",
            "fn page_exit_distance",
            "fn sample_in_page",
            "fn dvr_effective_alpha",
            "fn iso_gradient",
            "policy == PICK_FIRST_THRESHOLD",
            "policy == PICK_MIP_ARGMAX",
            "policy == PICK_DVR_MAX_CONTRIBUTION",
            "control[28u] != 0u",
            "transmittance <= best_score",
        ] {
            assert!(
                pick.contains(required),
                "the dedicated Pick module is missing {required}"
            );
        }
        for forbidden in [
            "@fragment",
            "fn render_plane",
            "fn render_mip(",
            "fn render_dvr_layer",
            "fn render_fused_dvr",
            "fn render_general_dvr",
            "fn render_iso(",
            "fn render_mixed",
            "fn composite_over",
            "fs_plane",
            "fs_mip",
            "fs_dvr",
            "fs_iso",
            "fs_mixed",
            "publish_",
            "fidelity",
            "descriptor_evidence",
        ] {
            assert!(
                !pick.contains(forbidden),
                "the dedicated Pick module still contains {forbidden}"
            );
        }
        assert_eq!(pick.matches("@compute").count(), 1);
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
