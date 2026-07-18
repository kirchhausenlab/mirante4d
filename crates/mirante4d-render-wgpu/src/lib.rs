//! Progressive WGPU product-rendering runtime.
//!
//! This crate owns product GPU resources and consumes only semantic dataset
//! leases and backend-neutral render contracts.

#![forbid(unsafe_code)]

mod runtime;

#[cfg(test)]
mod shader_audit;

pub use runtime::{
    MAX_INCREMENTAL_STATIC_KEY_CHANGES, PreparedStaticPresentationLayout,
    StaticPresentationLayoutPreflight, preflight_static_presentation_layout,
    preflight_static_presentation_layout_update, prepare_static_presentation_layout,
    prepare_static_presentation_layout_update,
};

use std::sync::Arc;

use mirante4d_dataset::{
    DatasetCatalog, DatasetResourceKey, ResourceLease, ResourcePayloadDescriptor,
};
use mirante4d_render_api::{
    FrameIdentity, FrameProgress, GpuLedgerCategory, PresentationRegistration,
    PresentationRetirement, PresentationToken, PresentedFrame, RenderExtent, RenderExtentEnvelope,
    RenderIntent, RenderPassKind, RenderRequirements, VolumePickQuery, VolumePickResult,
    VolumePickTicket,
};
use thiserror::Error;

const MAX_VISITS: usize = mirante4d_render_api::MAX_RENDER_REQUIREMENTS;
// 512 x 16-KiB tiles saturate the 8-MiB byte envelope in one submission.
// A count guard remains to avoid pathological command-buffer fan-out for
// tiny payloads without throttling normal 2D brick streams to 128 KiB/frame.
const MAX_UPLOADS: usize = 512;
const MAX_PAYLOAD_UPLOAD_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CONTROL_UPLOAD_BYTES: u64 = 8 * 1024 * 1024;
const UNKNOWN_GPU_BUDGET_BYTES: u64 = 1024 * 1024 * 1024;

pub const MAX_RENDER_WIDTH_PIXELS: u32 = 1_920;
pub const MAX_RENDER_HEIGHT_PIXELS: u32 = 1_080;

/// Controls when retained residency progress produces a volume pass.
///
/// Both policies use the same upload arena, page-table authority, and frame
/// transaction. `ExactFrameOnly` is for hidden atomic refinement: incomplete
/// cohorts become resident without raymarching pixels that cannot be shown,
/// and the cohort that completes exact coverage renders once in the same GPU
/// submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainedFrameRenderPolicy {
    EveryUsefulFrame,
    ExactFrameOnly,
}

/// Exact persistent-arena reservation for one decoded payload, including the
/// renderer's independent value/validity copy alignment. Demand planning must
/// use this value rather than raw decoded bytes so a bounded cohort is also
/// guaranteed to fit the GPU allocation ledger.
pub fn payload_allocation_bytes(
    descriptor: ResourcePayloadDescriptor,
) -> Result<u64, WgpuRenderRuntimeError> {
    let validity_offset = (descriptor.validity_byte_len() != 0)
        .then(|| align_payload_copy(descriptor.value_byte_len()))
        .transpose()?;
    let logical_end = validity_offset
        .unwrap_or(descriptor.value_byte_len())
        .checked_add(descriptor.validity_byte_len())
        .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)?;
    align_payload_copy(logical_end)
}

fn align_payload_copy(bytes: u64) -> Result<u64, WgpuRenderRuntimeError> {
    let bytes = bytes.max(1);
    let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
    bytes
        .checked_add(alignment - 1)
        .map(|rounded| rounded / alignment * alignment)
        .ok_or(WgpuRenderRuntimeError::CoordinateLimitExceeded)
}

/// The exact per-frame interactive renderer work ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameBudget {
    resident_resources_visited: usize,
    new_resources_uploaded: usize,
    payload_upload_bytes: u64,
    control_upload_bytes: u64,
    command_buffers: u32,
    queue_submissions: u32,
}

/// Monotonic identity for the exact global GPU payload-residency set.
/// Applications cache readiness against this value and only rescan semantic
/// keys after an upload or eviction changes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GpuResidencyEpoch(u64);

impl GpuResidencyEpoch {
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Monotonic epoch for destructive residency changes only. Additions do not
/// advance it, so an application can retain a large satisfied-key set across
/// progressive upload batches and rescan only after eviction/removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GpuResidencyInvalidationEpoch(u64);

impl GpuResidencyInvalidationEpoch {
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl FrameBudget {
    /// Returns the fixed interactive budget. It is not caller-expandable.
    pub const fn interactive() -> Self {
        Self {
            resident_resources_visited: MAX_VISITS,
            new_resources_uploaded: MAX_UPLOADS,
            payload_upload_bytes: MAX_PAYLOAD_UPLOAD_BYTES,
            control_upload_bytes: MAX_CONTROL_UPLOAD_BYTES,
            command_buffers: 1,
            queue_submissions: 1,
        }
    }

    pub const fn resident_resources_visited(self) -> usize {
        self.resident_resources_visited
    }

    pub const fn new_resources_uploaded(self) -> usize {
        self.new_resources_uploaded
    }

    pub const fn payload_upload_bytes(self) -> u64 {
        self.payload_upload_bytes
    }

    pub const fn control_upload_bytes(self) -> u64 {
        self.control_upload_bytes
    }

    pub const fn command_buffers(self) -> u32 {
        self.command_buffers
    }

    pub const fn queue_submissions(self) -> u32 {
        self.queue_submissions
    }
}

/// Configuration for the product GPU runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WgpuRenderRuntimeConfig {
    gpu_budget_bytes: u64,
    validation_capture: bool,
    gpu_timing: bool,
}

impl WgpuRenderRuntimeConfig {
    pub fn new(gpu_budget_bytes: u64) -> Result<Self, WgpuRenderRuntimeError> {
        if gpu_budget_bytes < 1024 * 1024 {
            return Err(WgpuRenderRuntimeError::InvalidConfiguration);
        }
        Ok(Self {
            gpu_budget_bytes,
            validation_capture: false,
            gpu_timing: false,
        })
    }

    pub const fn unknown_capacity() -> Self {
        Self {
            gpu_budget_bytes: UNKNOWN_GPU_BUDGET_BYTES,
            validation_capture: false,
            gpu_timing: false,
        }
    }

    pub const fn with_validation_capture(mut self, enabled: bool) -> Self {
        self.validation_capture = enabled;
        self
    }

    /// Enables per-frame timestamp resolve/readback for diagnostics and
    /// performance qualification. It is off on the product hot path.
    pub const fn with_gpu_timing(mut self, enabled: bool) -> Self {
        self.gpu_timing = enabled;
        self
    }

    pub const fn gpu_budget_bytes(self) -> u64 {
        self.gpu_budget_bytes
    }

    pub const fn validation_capture(self) -> bool {
        self.validation_capture
    }

    pub const fn gpu_timing(self) -> bool {
        self.gpu_timing
    }
}

/// Stable counters and sanitized adapter facts for the product runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WgpuRenderRuntimeDiagnostics {
    adapter_name: String,
    backend: String,
    driver: String,
    vendor_id: u32,
    device_id: u32,
    device_type: String,
    driver_name: String,
    driver_info: String,
    max_buffer_size_bytes: u64,
    max_storage_buffer_binding_size_bytes: u64,
    max_storage_buffers_per_shader_stage: u32,
    gpu_budget_bytes: u64,
    payload_capacity_bytes: u64,
    payload_segment_count: usize,
    payload_segment_capacity_bytes: [u64; 4],
    transfer_capacity_bytes: u64,
    other_capacity_bytes: u64,
    payload_arena_allocated_bytes: u64,
    resident_payload_used_bytes: u64,
    peak_resident_payload_used_bytes: u64,
    empty_resident_metadata_capacity_records: usize,
    empty_resident_metadata_bytes_per_record: u64,
    empty_resident_metadata_records: usize,
    empty_resident_metadata_bytes: u64,
    peak_empty_resident_metadata_bytes: u64,
    peak_transfer_bytes: u64,
    peak_display_target_bytes: u64,
    peak_page_table_bytes: u64,
    peak_scratch_bytes: u64,
    frames_executed: u64,
    queue_submissions: u64,
    current_in_flight_submissions: usize,
    peak_in_flight_submissions: usize,
    backpressure_deferrals: u64,
    residency_hits: u64,
    residency_misses: u64,
    residency_evictions: u64,
    residency_epoch_reuploads: u64,
    uploaded_resources: u64,
    uploaded_payload_bytes: u64,
    upload_staging_padding_zero_bytes: u64,
    render_thread_payload_fact_scan_bytes: u64,
    control_static_rebuilds: u64,
    control_static_rebuild_bytes: u64,
    page_layout_constructions: u64,
    control_dynamic_updates: u64,
    control_dynamic_upload_bytes: u64,
    control_publication_writes: u64,
    peak_control_publication_writes_per_frame: usize,
    control_dense_fallbacks: u64,
    control_body_delta_updates: u64,
    control_body_delta_keys: u64,
    control_body_delta_page_entries: u64,
    body_delta_pin_lru_keys: u64,
    control_buffer_allocations: u64,
    control_buffer_allocation_bytes: u64,
    bind_group_creations: u64,
    pipeline_creations: u64,
    explicit_staging_allocations: u64,
    explicit_staging_bytes: u64,
    peak_explicit_staging_bytes: u64,
    allocator_plans: u64,
    retained_navigation_frames: u64,
    cold_coverage_membership_checks: u64,
    gpu_timestamps_supported: bool,
    gpu_timing_enabled: bool,
    gpu_payload_copy_timestamps_supported: bool,
    gpu_timing_prelude_submissions: u64,
    completed_gpu_timings: u64,
    gpu_timing_failures: u64,
    last_gpu_batch_envelope_ns: Option<u64>,
    last_gpu_payload_copy_ns: Option<u64>,
    last_gpu_render_pass_ns: Option<u64>,
    completed_cpu_timings: u64,
    last_cpu_timing_frame: Option<u64>,
    last_cpu_planning_ns: Option<u64>,
    last_cpu_control_publication_ns: Option<u64>,
    last_cpu_payload_staging_ns: Option<u64>,
    last_cpu_queue_submit_ns: Option<u64>,
    total_cpu_planning_ns: u64,
    total_cpu_control_publication_ns: u64,
    total_cpu_payload_staging_ns: u64,
    total_cpu_queue_submit_ns: u64,
    pick_submissions: u64,
    completed_picks: u64,
    pick_backpressure_deferrals: u64,
    validation_error_count: u64,
}

impl WgpuRenderRuntimeDiagnostics {
    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    pub fn backend(&self) -> &str {
        &self.backend
    }

    pub fn driver(&self) -> &str {
        &self.driver
    }

    pub const fn vendor_id(&self) -> u32 {
        self.vendor_id
    }

    pub const fn device_id(&self) -> u32 {
        self.device_id
    }

    pub fn device_type(&self) -> &str {
        &self.device_type
    }

    pub fn driver_name(&self) -> &str {
        &self.driver_name
    }

    pub fn driver_info(&self) -> &str {
        &self.driver_info
    }

    pub const fn max_buffer_size_bytes(&self) -> u64 {
        self.max_buffer_size_bytes
    }

    pub const fn max_storage_buffer_binding_size_bytes(&self) -> u64 {
        self.max_storage_buffer_binding_size_bytes
    }

    pub const fn max_storage_buffers_per_shader_stage(&self) -> u32 {
        self.max_storage_buffers_per_shader_stage
    }

    pub const fn gpu_budget_bytes(&self) -> u64 {
        self.gpu_budget_bytes
    }

    pub const fn payload_capacity_bytes(&self) -> u64 {
        self.payload_capacity_bytes
    }

    pub const fn payload_segment_count(&self) -> usize {
        self.payload_segment_count
    }

    /// Fixed shader-visible payload bindings in priority/fill order. Entries
    /// after [`Self::payload_segment_count`] are zero-capacity dummy bindings.
    pub const fn payload_segment_capacity_bytes(&self) -> &[u64; 4] {
        &self.payload_segment_capacity_bytes
    }

    pub const fn transfer_capacity_bytes(&self) -> u64 {
        self.transfer_capacity_bytes
    }

    pub const fn other_capacity_bytes(&self) -> u64 {
        self.other_capacity_bytes
    }

    pub const fn payload_arena_allocated_bytes(&self) -> u64 {
        self.payload_arena_allocated_bytes
    }

    pub const fn resident_payload_bytes(&self) -> u64 {
        self.resident_payload_used_bytes
    }

    pub const fn peak_resident_payload_bytes(&self) -> u64 {
        self.peak_resident_payload_used_bytes
    }

    /// Maximum number of metadata-only empty page records retained by the
    /// renderer. This covers the complete four-presentation active union plus
    /// one full requirement set of inactive reuse.
    pub const fn empty_resident_metadata_capacity_records(&self) -> usize {
        self.empty_resident_metadata_capacity_records
    }

    /// Conservative host-memory ledger charge for each metadata-only empty
    /// page record, including its key/value and map-node/index allowance.
    pub const fn empty_resident_metadata_bytes_per_record(&self) -> u64 {
        self.empty_resident_metadata_bytes_per_record
    }

    pub const fn empty_resident_metadata_records(&self) -> usize {
        self.empty_resident_metadata_records
    }

    pub const fn empty_resident_metadata_bytes(&self) -> u64 {
        self.empty_resident_metadata_bytes
    }

    pub const fn peak_empty_resident_metadata_bytes(&self) -> u64 {
        self.peak_empty_resident_metadata_bytes
    }

    pub const fn peak_transfer_bytes(&self) -> u64 {
        self.peak_transfer_bytes
    }

    pub const fn peak_display_target_bytes(&self) -> u64 {
        self.peak_display_target_bytes
    }

    pub const fn peak_page_table_bytes(&self) -> u64 {
        self.peak_page_table_bytes
    }

    pub const fn peak_scratch_bytes(&self) -> u64 {
        self.peak_scratch_bytes
    }

    pub const fn frames_executed(&self) -> u64 {
        self.frames_executed
    }

    pub const fn queue_submissions(&self) -> u64 {
        self.queue_submissions
    }

    pub const fn current_in_flight_submissions(&self) -> usize {
        self.current_in_flight_submissions
    }

    pub const fn peak_in_flight_submissions(&self) -> usize {
        self.peak_in_flight_submissions
    }

    pub const fn backpressure_deferrals(&self) -> u64 {
        self.backpressure_deferrals
    }

    pub const fn residency_hits(&self) -> u64 {
        self.residency_hits
    }

    pub const fn residency_misses(&self) -> u64 {
        self.residency_misses
    }

    pub const fn residency_evictions(&self) -> u64 {
        self.residency_evictions
    }

    pub const fn residency_epoch_reuploads(&self) -> u64 {
        self.residency_epoch_reuploads
    }

    pub const fn uploaded_resources(&self) -> u64 {
        self.uploaded_resources
    }

    pub const fn uploaded_payload_bytes(&self) -> u64 {
        self.uploaded_payload_bytes
    }

    /// Bytes explicitly cleared in persistent mapped upload slots because
    /// they are alignment padding. Value and validity spans are never included.
    pub const fn upload_staging_padding_zero_bytes(&self) -> u64 {
        self.upload_staging_padding_zero_bytes
    }

    /// Bytes rescanned on the render thread to derive min/max/validity facts.
    /// Runtime-issued leases carry these facts, so the accepted value is zero.
    pub const fn render_thread_payload_fact_scan_bytes(&self) -> u64 {
        self.render_thread_payload_fact_scan_bytes
    }

    pub const fn control_static_rebuilds(&self) -> u64 {
        self.control_static_rebuilds
    }

    pub const fn control_static_rebuild_bytes(&self) -> u64 {
        self.control_static_rebuild_bytes
    }

    /// Number of per-layer sparse page layouts constructed for committed
    /// static requirement bodies. One new body constructs each layer once.
    pub const fn page_layout_constructions(&self) -> u64 {
        self.page_layout_constructions
    }

    pub const fn control_dynamic_updates(&self) -> u64 {
        self.control_dynamic_updates
    }

    pub const fn control_dynamic_upload_bytes(&self) -> u64 {
        self.control_dynamic_upload_bytes
    }

    /// Total `Queue::write_buffer` operations used to publish render-control
    /// data. The per-frame peak is hard-capped before any writes are issued.
    pub const fn control_publication_writes(&self) -> u64 {
        self.control_publication_writes
    }

    pub const fn peak_control_publication_writes_per_frame(&self) -> usize {
        self.peak_control_publication_writes_per_frame
    }

    /// Fragmented incremental updates replaced by one dense record-slab write.
    pub const fn control_dense_fallbacks(&self) -> u64 {
        self.control_dense_fallbacks
    }

    /// Exact predecessor-bound body replacements applied through stable
    /// record slots and sparse page-entry patches.
    pub const fn control_body_delta_updates(&self) -> u64 {
        self.control_body_delta_updates
    }

    pub const fn control_body_delta_keys(&self) -> u64 {
        self.control_body_delta_keys
    }

    pub const fn control_body_delta_page_entries(&self) -> u64 {
        self.control_body_delta_page_entries
    }

    /// Exact added/removed keys visited to reconcile payload pin and age
    /// indexes after a predecessor-bound body replacement.
    pub const fn body_delta_pin_lru_keys(&self) -> u64 {
        self.body_delta_pin_lru_keys
    }

    /// Presentation control buffers allocated since runtime creation,
    /// including each registration's initial buffer.
    pub const fn control_buffer_allocations(&self) -> u64 {
        self.control_buffer_allocations
    }

    pub const fn control_buffer_allocation_bytes(&self) -> u64 {
        self.control_buffer_allocation_bytes
    }

    /// Product bind groups created since this runtime was constructed. This
    /// includes presentation registration, control-buffer replacement, and
    /// resources created for work that is subsequently abandoned.
    pub const fn bind_group_creations(&self) -> u64 {
        self.bind_group_creations
    }

    /// Product render and compute pipelines created since this runtime was
    /// constructed. The fixed initial set is the general render pipeline, the
    /// dedicated MIP pipeline, and the asynchronous pick pipeline.
    pub const fn pipeline_creations(&self) -> u64 {
        self.pipeline_creations
    }

    /// Explicit mapped staging buffers allocated by the runtime. Camera-only
    /// retained navigation uses a small queue write and leaves this unchanged.
    pub const fn explicit_staging_allocations(&self) -> u64 {
        self.explicit_staging_allocations
    }

    pub const fn explicit_staging_bytes(&self) -> u64 {
        self.explicit_staging_bytes
    }

    pub const fn peak_explicit_staging_bytes(&self) -> u64 {
        self.peak_explicit_staging_bytes
    }

    pub const fn allocator_plans(&self) -> u64 {
        self.allocator_plans
    }

    pub const fn retained_navigation_frames(&self) -> u64 {
        self.retained_navigation_frames
    }

    /// Exact membership probes used to seed availability for new immutable
    /// requirement bodies. The runtime probes the smaller of body/residency.
    pub const fn cold_coverage_membership_checks(&self) -> u64 {
        self.cold_coverage_membership_checks
    }

    pub const fn gpu_timestamps_supported(&self) -> bool {
        self.gpu_timestamps_supported
    }

    pub const fn gpu_timing_enabled(&self) -> bool {
        self.gpu_timing_enabled
    }

    pub const fn gpu_payload_copy_timestamps_supported(&self) -> bool {
        self.gpu_payload_copy_timestamps_supported
    }

    /// Qualification-only timestamp prelude submissions used to place the
    /// batch-envelope start before queue-write control publication.
    pub const fn gpu_timing_prelude_submissions(&self) -> u64 {
        self.gpu_timing_prelude_submissions
    }

    pub const fn completed_gpu_timings(&self) -> u64 {
        self.completed_gpu_timings
    }

    /// Terminal timestamp-map or timestamp-validation failures. Failed slots
    /// are released immediately so one bad result cannot permanently consume
    /// the bounded timing ring.
    pub const fn gpu_timing_failures(&self) -> u64 {
        self.gpu_timing_failures
    }

    pub const fn last_gpu_batch_envelope_ns(&self) -> Option<u64> {
        self.last_gpu_batch_envelope_ns
    }

    pub const fn last_gpu_payload_copy_ns(&self) -> Option<u64> {
        self.last_gpu_payload_copy_ns
    }

    pub const fn last_gpu_render_pass_ns(&self) -> Option<u64> {
        self.last_gpu_render_pass_ns
    }

    /// CPU renderer timings are collected only with the same opt-in
    /// qualification switch as GPU timestamps. Planning covers renderer
    /// validation, residency/control preparation, and command encoding up to
    /// (but not including) `Queue::submit`; submission is measured separately.
    pub const fn completed_cpu_timings(&self) -> u64 {
        self.completed_cpu_timings
    }

    pub const fn last_cpu_timing_frame(&self) -> Option<u64> {
        self.last_cpu_timing_frame
    }

    pub const fn last_cpu_planning_ns(&self) -> Option<u64> {
        self.last_cpu_planning_ns
    }

    /// Host time spent in queue-write control publication. This is not a GPU
    /// transfer duration; queue-visible interference remains outside the
    /// encoder-bracketed payload-copy interval.
    pub const fn last_cpu_control_publication_ns(&self) -> Option<u64> {
        self.last_cpu_control_publication_ns
    }

    /// Host time spent copying decoded payloads into mapped staging storage.
    /// Encoder command construction and GPU copy execution are outside it.
    pub const fn last_cpu_payload_staging_ns(&self) -> Option<u64> {
        self.last_cpu_payload_staging_ns
    }

    pub const fn last_cpu_queue_submit_ns(&self) -> Option<u64> {
        self.last_cpu_queue_submit_ns
    }

    pub const fn total_cpu_planning_ns(&self) -> u64 {
        self.total_cpu_planning_ns
    }

    pub const fn total_cpu_control_publication_ns(&self) -> u64 {
        self.total_cpu_control_publication_ns
    }

    pub const fn total_cpu_payload_staging_ns(&self) -> u64 {
        self.total_cpu_payload_staging_ns
    }

    pub const fn total_cpu_queue_submit_ns(&self) -> u64 {
        self.total_cpu_queue_submit_ns
    }

    pub const fn pick_submissions(&self) -> u64 {
        self.pick_submissions
    }

    pub const fn completed_picks(&self) -> u64 {
        self.completed_picks
    }

    pub const fn pick_backpressure_deferrals(&self) -> u64 {
        self.pick_backpressure_deferrals
    }

    pub const fn validation_error_count(&self) -> u64 {
        self.validation_error_count
    }
}

/// Opaque identity for one asynchronous GPU timestamp result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GpuTimingTicket {
    id: u64,
    target: PresentationToken,
    generation: FrameIdentity,
    display_generation: u64,
    pass_kind: RenderPassKind,
}

impl GpuTimingTicket {
    /// Monotonic renderer-execution identity. Frame identity alone is not an
    /// execution identity because progressive publications may reuse one
    /// input frame while residency improves.
    pub const fn execution_id(self) -> u64 {
        self.id
    }

    pub const fn target(self) -> PresentationToken {
        self.target
    }

    pub const fn generation(self) -> FrameIdentity {
        self.generation
    }

    /// Application input/display generation supplied at renderer execution.
    /// This is part of the asynchronous ticket rather than out-of-band
    /// metadata so a completed timestamp cannot be rebound to a newer input.
    pub const fn display_generation(self) -> u64 {
        self.display_generation
    }

    pub const fn pass_kind(self) -> RenderPassKind {
        self.pass_kind
    }
}

/// GPU-domain elapsed times from timestamp queries.
///
/// Payload-copy time is available only when copy commands were encoded and the
/// adapter supports timestamp writes inside command encoders. Render-pass time
/// is available only when this execution encoded the target's pass. Queue
/// writes and CPU staging are deliberately excluded from both intervals. The
/// batch GPU envelope spans an explicitly counted diagnostic prelude through
/// the end of the render/copy commands; it includes queue-write interference
/// but is not a claimed control-copy duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuFrameTiming {
    ticket: GpuTimingTicket,
    batch_gpu_envelope_ns: Option<u64>,
    payload_copy_ns: Option<u64>,
    render_pass_ns: Option<u64>,
}

/// CPU-domain work for one exact renderer execution.
///
/// Planning includes the retained-cohort preflight, validation, residency and
/// control preparation, and command encoding. The synchronous
/// `Queue::submit` call is measured separately. Collection is opt-in with the
/// qualification timing switch and never performs a GPU wait or readback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuFrameTiming {
    planning_ns: u64,
    control_publication_ns: Option<u64>,
    payload_staging_ns: Option<u64>,
    queue_submit_ns: u64,
}

impl CpuFrameTiming {
    pub const fn new(
        planning_ns: u64,
        control_publication_ns: Option<u64>,
        payload_staging_ns: Option<u64>,
        queue_submit_ns: u64,
    ) -> Self {
        Self {
            planning_ns,
            control_publication_ns,
            payload_staging_ns,
            queue_submit_ns,
        }
    }

    pub const fn planning_ns(self) -> u64 {
        self.planning_ns
    }

    pub const fn control_publication_ns(self) -> Option<u64> {
        self.control_publication_ns
    }

    pub const fn payload_staging_ns(self) -> Option<u64> {
        self.payload_staging_ns
    }

    pub const fn queue_submit_ns(self) -> u64 {
        self.queue_submit_ns
    }
}

impl GpuFrameTiming {
    pub const fn ticket(self) -> GpuTimingTicket {
        self.ticket
    }

    pub const fn target(self) -> PresentationToken {
        self.ticket.target()
    }

    pub const fn generation(self) -> FrameIdentity {
        self.ticket.generation()
    }

    pub const fn pass_kind(self) -> RenderPassKind {
        self.ticket.pass_kind()
    }

    pub const fn batch_gpu_envelope_ns(self) -> Option<u64> {
        self.batch_gpu_envelope_ns
    }

    pub const fn payload_copy_ns(self) -> Option<u64> {
        self.payload_copy_ns
    }

    pub const fn render_pass_ns(self) -> Option<u64> {
        self.render_pass_ns
    }
}

/// Opaque identity for one asynchronous validation readback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValidationCaptureTicket {
    id: u64,
    presentation: PresentationToken,
    frame: FrameIdentity,
    extent: RenderExtent,
}

impl ValidationCaptureTicket {
    pub const fn presentation(self) -> PresentationToken {
        self.presentation
    }

    pub const fn frame(self) -> FrameIdentity {
        self.frame
    }

    pub const fn extent(self) -> RenderExtent {
        self.extent
    }
}

/// Completed tightly packed RGBA8 pixels and exact per-pixel facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationCapture {
    frame: FrameIdentity,
    extent: RenderExtent,
    rgba8: Box<[u8]>,
    coverage: Box<[u8]>,
    validity: Box<[u8]>,
}

impl ValidationCapture {
    pub const fn frame(&self) -> FrameIdentity {
        self.frame
    }

    pub const fn extent(&self) -> RenderExtent {
        self.extent
    }

    pub fn rgba8(&self) -> &[u8] {
        &self.rgba8
    }

    pub fn coverage(&self) -> &[u8] {
        &self.coverage
    }

    pub fn validity(&self) -> &[u8] {
        &self.validity
    }
}

/// One bounded execution result. No report implies more coverage than its
/// `FrameProgress` proves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameExecutionReport {
    presentation: Option<PresentedFrame>,
    frame: FrameIdentity,
    progress: Option<FrameProgress>,
    visited_resources: usize,
    uploaded_resources: usize,
    payload_upload_bytes: u64,
    control_upload_bytes: u64,
    command_buffers: u32,
    queue_submissions: u32,
    deferred_by_backpressure: bool,
    retained_updates_accepted: bool,
    cpu_timing: Option<CpuFrameTiming>,
    gpu_timing: Option<GpuTimingTicket>,
    validation_capture: Option<ValidationCaptureTicket>,
    newly_resident_keys: Box<[DatasetResourceKey]>,
    evicted_keys: Box<[DatasetResourceKey]>,
}

impl FrameExecutionReport {
    pub const fn presentation(&self) -> Option<&PresentedFrame> {
        self.presentation.as_ref()
    }

    pub const fn frame(&self) -> FrameIdentity {
        self.frame
    }

    pub const fn progress(&self) -> Option<&FrameProgress> {
        self.progress.as_ref()
    }

    pub const fn visited_resources(&self) -> usize {
        self.visited_resources
    }

    pub const fn uploaded_resources(&self) -> usize {
        self.uploaded_resources
    }

    pub const fn payload_upload_bytes(&self) -> u64 {
        self.payload_upload_bytes
    }

    pub const fn control_upload_bytes(&self) -> u64 {
        self.control_upload_bytes
    }

    pub const fn command_buffers(&self) -> u32 {
        self.command_buffers
    }

    pub const fn queue_submissions(&self) -> u32 {
        self.queue_submissions
    }

    pub const fn deferred_by_backpressure(&self) -> bool {
        self.deferred_by_backpressure
    }

    /// Whether every lease update supplied to `execute_retained_frame` was
    /// either already known/resident or retained in exact caller order. When
    /// false, the caller must retry the same ranked cohort before advancing.
    /// Direct `execute_frame` reports always return true.
    pub const fn retained_updates_accepted(&self) -> bool {
        self.retained_updates_accepted
    }

    /// CPU planning and queue-submission durations bound to this report's
    /// exact frame. `None` means timing was disabled or no submission occurred.
    pub const fn cpu_timing(&self) -> Option<CpuFrameTiming> {
        self.cpu_timing
    }

    /// Returns a ticket only when this execution submitted at least one
    /// timestamped payload-copy or render-pass interval. Each interval remains
    /// independently optional in the asynchronous result; `None` here is an
    /// explicit unavailable/not-submitted state for the whole execution.
    pub const fn gpu_timing(&self) -> Option<GpuTimingTicket> {
        self.gpu_timing
    }

    pub const fn validation_capture(&self) -> Option<ValidationCaptureTicket> {
        self.validation_capture
    }

    /// Exact bounded residency additions committed by this execution,
    /// including metadata-only empty resources drained from retained leases.
    pub fn newly_resident_keys(&self) -> &[DatasetResourceKey] {
        &self.newly_resident_keys
    }

    /// Exact payload/metadata removals committed by this execution.
    pub fn evicted_keys(&self) -> &[DatasetResourceKey] {
        &self.evicted_keys
    }
}

/// Typed, backend-neutral failures from the product GPU runtime.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum WgpuRenderRuntimeError {
    #[error("the WGPU runtime configuration is invalid")]
    InvalidConfiguration,
    #[error("no qualifying Vulkan GPU adapter is available")]
    DeviceUnavailable,
    #[error("a CPU or software adapter cannot run the interactive renderer")]
    SoftwareAdapter,
    #[error("the interactive renderer requires a Vulkan adapter")]
    UnsupportedBackend,
    #[error("the adapter does not satisfy the accepted renderer limits")]
    AdapterLimitsInsufficient,
    #[error("the existing WGPU device was created below the renderer limits")]
    DeviceLimitsInsufficient,
    #[error("the GPU device could not be created")]
    DeviceCreationFailed,
    #[error("render intent and requirements name different frame generations")]
    FrameContractMismatch,
    #[error("the requested render extent exceeds 1920x1080")]
    ExtentExceeded,
    #[error("render frame {actual:?} is stale relative to {current:?}")]
    StaleFrame {
        actual: FrameIdentity,
        current: FrameIdentity,
    },
    #[error("the requirement set changed within one frame generation")]
    RequirementSetChanged,
    #[error("the prepared static presentation layout does not match the render body/layers")]
    PreparedStaticLayoutMismatch,
    #[error(
        "the render requirement set contains {actual} resources, exceeding the renderer limit of {maximum}"
    )]
    RequirementCapacityExceeded { actual: usize, maximum: usize },
    #[error("the renderer reached its limit of {maximum} presentation targets")]
    PresentationCapacityExceeded { maximum: usize },
    #[error("presentation token {token:?} is not registered in this renderer")]
    PresentationNotRegistered { token: PresentationToken },
    #[error("the renderer exhausted its presentation token space")]
    PresentationTokenExhausted,
    #[error(
        "the supplied lease set contains {actual} resources, exceeding the renderer limit of {maximum}"
    )]
    LeaseCapacityExceeded { actual: usize, maximum: usize },
    #[error("one render layer requests more than one semantic scale in the same frame")]
    MixedScaleRequirements,
    #[error("same-layer resources overlap at one semantic scale")]
    OverlappingResources,
    #[error("one semantic resource lease occurs more than once")]
    DuplicateLease,
    #[error("a supplied lease is absent from the frame requirements")]
    UnexpectedLease,
    #[error("a supplied lease violates the catalog payload contract")]
    PayloadContractMismatch,
    #[error("the product renderer does not support this view transform")]
    UnsupportedView,
    #[error("semantic coordinates exceed the bounded GPU metadata representation")]
    CoordinateLimitExceeded,
    #[error("frame control metadata exceeds its 8-MiB ceiling")]
    ControlCapacityExceeded,
    #[error(
        "GPU capacity in {category:?} cannot satisfy {requested_bytes} bytes with {available_bytes} bytes available"
    )]
    CapacityExceeded {
        category: GpuLedgerCategory,
        requested_bytes: u64,
        available_bytes: u64,
    },
    #[error(
        "empty-page host metadata requires {requested_records} records/{requested_bytes} bytes, exceeding the bounded {maximum_records} records/{available_bytes} bytes"
    )]
    ResidentMetadataCapacityExceeded {
        requested_records: usize,
        maximum_records: usize,
        requested_bytes: u64,
        available_bytes: u64,
    },
    #[error("backend validation rejected render work")]
    BackendValidation,
    #[error("validation capture does not belong to this runtime")]
    UnknownValidationCapture,
    #[error("validation capture belongs to a stale frame generation")]
    StaleValidationCapture,
    #[error("validation capture mapping failed")]
    ValidationCaptureFailed,
    #[error("GPU timing does not belong to this runtime or has expired")]
    UnknownGpuTiming,
    #[error("GPU timing mapping failed")]
    GpuTimingFailed,
    #[error("the volume pick query does not match the requested presentation")]
    PickQueryMismatch,
    #[error(
        "the exact presented frame or its payload residency is no longer available for picking"
    )]
    PickFrameUnavailable,
    #[error("all bounded asynchronous volume-pick slots are occupied")]
    PickCapacityExceeded,
    #[error("the renderer exhausted its asynchronous volume-pick ticket space")]
    PickTicketExhausted,
    #[error("volume-pick submission was deferred by the renderer in-flight bound")]
    PickBackpressure,
    #[error("the volume pick does not belong to this runtime or has already been consumed")]
    UnknownVolumePick,
    #[error("asynchronous volume-pick execution or result validation failed")]
    VolumePickFailed,
    #[error("backend-neutral frame progress construction failed")]
    FrameProgressContract,
}

/// Checks whether an adapter can run the interactive product renderer.
///
/// Product startup uses this before asking WGPU to create the window device,
/// so unsupported software, non-Vulkan, and undersized adapters fail before
/// the viewer opens.
pub fn qualify_adapter(adapter: &wgpu::Adapter) -> Result<(), WgpuRenderRuntimeError> {
    runtime::validate_adapter(adapter)
}

/// Builds the device request used by the interactive product renderer.
///
/// The requested features and limits are the same fixed set used by
/// [`WgpuRenderRuntime::new`].
pub fn renderer_device_descriptor(
    adapter: &wgpu::Adapter,
    label: &'static str,
) -> Result<wgpu::DeviceDescriptor<'static>, WgpuRenderRuntimeError> {
    runtime::renderer_device_descriptor(adapter, label)
}

/// Sole product owner of WGPU resources.
pub struct WgpuRenderRuntime {
    inner: runtime::Runtime,
}

impl WgpuRenderRuntime {
    /// Creates a real Vulkan device and the offscreen rendering pipeline.
    pub async fn new(config: WgpuRenderRuntimeConfig) -> Result<Self, WgpuRenderRuntimeError> {
        Ok(Self {
            inner: runtime::Runtime::new(config).await?,
        })
    }

    #[cfg(test)]
    async fn new_with_payload_segment_limit(
        config: WgpuRenderRuntimeConfig,
        segment_limit: u64,
    ) -> Result<Self, WgpuRenderRuntimeError> {
        Ok(Self {
            inner: runtime::Runtime::new_with_payload_segment_limit(config, segment_limit).await?,
        })
    }

    /// Builds the runtime around the device already selected for the native
    /// window. The runtime retains its own handles and remains the owner of all
    /// Mirante4D render resources created on that device.
    pub fn from_existing_device(
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: WgpuRenderRuntimeConfig,
    ) -> Result<Self, WgpuRenderRuntimeError> {
        Ok(Self {
            inner: runtime::Runtime::from_existing_device(adapter, device, queue, config)?,
        })
    }

    pub const fn frame_budget(&self) -> FrameBudget {
        FrameBudget::interactive()
    }

    /// Maximum accepted physical render target. Product HiDPI negotiation
    /// queries this instead of duplicating backend limits.
    pub fn maximum_extent() -> RenderExtent {
        RenderExtent::new(MAX_RENDER_WIDTH_PIXELS, MAX_RENDER_HEIGHT_PIXELS)
            .expect("the renderer maximum extent is positive")
    }

    pub fn extent_envelope() -> RenderExtentEnvelope {
        RenderExtentEnvelope::new(MAX_RENDER_WIDTH_PIXELS, MAX_RENDER_HEIGHT_PIXELS)
            .expect("the renderer extent envelope is positive")
    }

    pub const fn diagnostics(&self) -> &WgpuRenderRuntimeDiagnostics {
        self.inner.diagnostics()
    }

    /// Creates one renderer-owned target and returns only its opaque identity
    /// and scalar extent to the composition layer.
    pub fn register_presentation(
        &mut self,
        extent: RenderExtent,
    ) -> Result<PresentationRegistration, WgpuRenderRuntimeError> {
        self.inner.register_presentation(extent)
    }

    /// Borrows the color view for the sole native composition bridge. The
    /// token is checked by the renderer and the texture remains renderer-owned.
    pub fn presentation_texture_view(
        &self,
        token: PresentationToken,
    ) -> Result<&wgpu::TextureView, WgpuRenderRuntimeError> {
        self.inner.presentation_texture_view(token)
    }

    pub fn retire_presentation(
        &mut self,
        token: PresentationToken,
    ) -> Result<PresentationRetirement, WgpuRenderRuntimeError> {
        self.inner.retire_presentation(token)
    }

    /// Clears a target's retained frame, streaming backlog, and residency
    /// pins while preserving its registered texture/token for later reuse.
    pub fn deactivate_presentation(
        &mut self,
        token: PresentationToken,
    ) -> Result<(), WgpuRenderRuntimeError> {
        self.inner.deactivate_presentation(token)
    }

    /// Retires all dataset-generation-scoped residency, controls, and
    /// asynchronous results while retaining the selected device, pipelines,
    /// and registered presentation targets for the replacement source.
    pub fn retire_dataset_generation(&mut self) {
        self.inner.retire_dataset_generation();
    }

    /// Read-only half of an application-level multi-owner transaction.
    pub fn preflight_deactivate_presentation(
        &self,
        token: PresentationToken,
    ) -> Result<(), WgpuRenderRuntimeError> {
        self.inner.preflight_deactivate_presentation(token)
    }

    /// Infallible commit after `preflight_deactivate_presentation`. Renderer
    /// ownership is UI-thread confined, so no registration can change between
    /// the adjacent preflight and commit boundaries.
    pub fn commit_preflighted_deactivate_presentation(&mut self, token: PresentationToken) {
        self.inner
            .deactivate_presentation(token)
            .expect("a preflighted presentation remains registered until UI commit");
    }

    pub fn execute_frame(
        &mut self,
        presentation: PresentationToken,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        leases: &[&dyn ResourceLease],
    ) -> Result<FrameExecutionReport, WgpuRenderRuntimeError> {
        self.inner
            .execute_frame(presentation, catalog, intent, requirements, leases)
    }

    /// Executes a frame using a worker-prepared static sparse layout. The UI
    /// path performs only bounded layer-prefix work plus actual residency
    /// deltas; it never constructs or clones the up-to-65,536-entry layout.
    pub fn execute_prepared_frame(
        &mut self,
        presentation: PresentationToken,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        layout: &PreparedStaticPresentationLayout,
        leases: &[&dyn ResourceLease],
    ) -> Result<FrameExecutionReport, WgpuRenderRuntimeError> {
        self.inner.execute_prepared_frame(
            presentation,
            catalog,
            intent,
            requirements,
            layout,
            leases,
        )
    }

    /// Executes from presentation-retained requirement/control/lease state.
    /// Callers supply only newly available CPU leases; leases not consumed by
    /// this frame's byte budget remain retained for subsequent submissions.
    pub fn execute_retained_frame(
        &mut self,
        presentation: PresentationToken,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        lease_updates: &[Arc<dyn ResourceLease>],
    ) -> Result<FrameExecutionReport, WgpuRenderRuntimeError> {
        self.inner.execute_retained_frame(
            presentation,
            catalog,
            intent,
            requirements,
            lease_updates,
        )
    }

    /// Retained-lease variant of [`Self::execute_prepared_frame`].
    pub fn execute_prepared_retained_frame(
        &mut self,
        presentation: PresentationToken,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        layout: &PreparedStaticPresentationLayout,
        lease_updates: &[Arc<dyn ResourceLease>],
    ) -> Result<FrameExecutionReport, WgpuRenderRuntimeError> {
        self.inner.execute_prepared_retained_frame(
            presentation,
            catalog,
            intent,
            requirements,
            layout,
            lease_updates,
        )
    }

    /// Retained-lease execution with an explicit presentation cadence policy.
    /// Upload/residency progress is never delayed by the policy.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_prepared_retained_frame_with_policy(
        &mut self,
        presentation: PresentationToken,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        layout: &PreparedStaticPresentationLayout,
        lease_updates: &[Arc<dyn ResourceLease>],
        display_generation: u64,
        render_policy: RetainedFrameRenderPolicy,
    ) -> Result<FrameExecutionReport, WgpuRenderRuntimeError> {
        self.inner.execute_prepared_retained_frame_with_policy(
            presentation,
            catalog,
            intent,
            requirements,
            layout,
            lease_updates,
            display_generation,
            render_policy,
        )
    }

    pub const fn residency_epoch(&self) -> GpuResidencyEpoch {
        GpuResidencyEpoch(self.inner.residency_epoch())
    }

    pub const fn residency_invalidation_epoch(&self) -> GpuResidencyInvalidationEpoch {
        GpuResidencyInvalidationEpoch(self.inner.residency_invalidation_epoch())
    }

    /// Effective usable payload arena after device binding limits and the
    /// configured ledger are both applied.
    pub const fn payload_capacity_bytes(&self) -> u64 {
        self.inner.diagnostics().payload_capacity_bytes()
    }

    pub const fn resident_payload_bytes(&self) -> u64 {
        self.inner.diagnostics().resident_payload_bytes()
    }

    pub const fn available_payload_bytes(&self) -> u64 {
        self.payload_capacity_bytes()
            .saturating_sub(self.resident_payload_bytes())
    }

    /// Exact read-only membership query for the current residency epoch.
    pub fn resource_is_resident(&self, key: DatasetResourceKey) -> bool {
        self.inner.resource_is_resident(key)
    }

    /// Exact sorted snapshot source for qualification diagnostics. The
    /// iterator is read-only and allocation-free; callers that retain a
    /// snapshot must own and account for their bounded diagnostic storage.
    pub fn resident_keys(&self) -> impl ExactSizeIterator<Item = DatasetResourceKey> + '_ {
        self.inner.resident_keys()
    }

    /// Polls once without waiting. `None` means the GPU/map callback has not
    /// completed yet and the caller should poll on a later event-loop turn.
    pub fn poll_validation_capture(
        &mut self,
        ticket: ValidationCaptureTicket,
    ) -> Result<Option<ValidationCapture>, WgpuRenderRuntimeError> {
        self.inner.poll_validation_capture(ticket)
    }

    /// Polls one asynchronous GPU-domain timing without waiting for the GPU.
    pub fn poll_gpu_timing(
        &mut self,
        ticket: GpuTimingTicket,
    ) -> Result<Option<GpuFrameTiming>, WgpuRenderRuntimeError> {
        self.inner.poll_gpu_timing(ticket)
    }

    /// Submits one bounded compute pick over the exact page/payload snapshot
    /// associated with `query`. No framebuffer or synchronous readback is used.
    pub fn request_pick(
        &mut self,
        presentation: PresentationToken,
        query: VolumePickQuery,
    ) -> Result<VolumePickTicket, WgpuRenderRuntimeError> {
        self.inner.request_pick(presentation, query)
    }

    /// Polls once without waiting. A completed result preserves the exact
    /// query supplied to [`Self::request_pick`].
    pub fn poll_pick(
        &mut self,
        ticket: VolumePickTicket,
    ) -> Result<Option<VolumePickResult>, WgpuRenderRuntimeError> {
        self.inner.poll_pick(ticket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_frame_budget_is_exact() {
        let budget = FrameBudget::interactive();
        assert_eq!(
            budget.resident_resources_visited(),
            mirante4d_render_api::MAX_RENDER_REQUIREMENTS
        );
        assert_eq!(budget.new_resources_uploaded(), 512);
        assert_eq!(budget.payload_upload_bytes(), 8 * 1024 * 1024);
        assert_eq!(budget.control_upload_bytes(), 8 * 1024 * 1024);
        assert_eq!(budget.command_buffers(), 1);
        assert_eq!(budget.queue_submissions(), 1);
    }

    #[test]
    fn config_rejects_a_ledger_smaller_than_one_mebibyte() {
        assert_eq!(
            WgpuRenderRuntimeConfig::new(1024 * 1024 - 1),
            Err(WgpuRenderRuntimeError::InvalidConfiguration)
        );
    }

    #[test]
    fn maximum_extent_is_a_public_backend_fact() {
        assert_eq!(
            WgpuRenderRuntime::maximum_extent(),
            RenderExtent::new(1920, 1080).unwrap()
        );
        assert_eq!(
            WgpuRenderRuntime::extent_envelope(),
            RenderExtentEnvelope::new(1920, 1080).unwrap()
        );
    }

    #[test]
    fn public_payload_allocation_calculator_matches_split_copy_alignment() {
        let shape = mirante4d_domain::Shape3D::new(3, 3, 3).unwrap();
        let all_valid = ResourcePayloadDescriptor::new(
            mirante4d_domain::IntensityDType::Uint16,
            shape,
            mirante4d_dataset::ResourceValidity::AllValid,
        )
        .unwrap();
        let bitmask = ResourcePayloadDescriptor::new(
            mirante4d_domain::IntensityDType::Uint16,
            shape,
            mirante4d_dataset::ResourceValidity::BitMask,
        )
        .unwrap();
        assert_eq!(payload_allocation_bytes(all_valid), Ok(56));
        assert_eq!(payload_allocation_bytes(bitmask), Ok(60));
    }
}

#[cfg(test)]
mod gpu_tests;
