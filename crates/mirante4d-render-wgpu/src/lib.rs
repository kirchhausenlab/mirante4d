//! Progressive WGPU product-rendering runtime.
//!
//! This crate owns product GPU resources and consumes only semantic dataset
//! leases and backend-neutral render contracts.

#![forbid(unsafe_code)]

mod global_residency;
mod runtime;

use std::sync::Arc;

use mirante4d_dataset::{BrickKey, DatasetCatalog, ResourceLease, ResourcePayloadDescriptor};
#[cfg(test)]
use mirante4d_render_api::RenderResourceGridCatalog;
use mirante4d_render_api::{
    FrameIdentity, FrameProgress, GpuLedgerCategory, PreparedRenderRequirements,
    PresentationTarget, RenderExtent, RenderExtentEnvelope, RenderIntent, RenderPassKind,
    RenderRequirements, VolumePickQuery, VolumePickResult, VolumePickTicket,
};
use thiserror::Error;

// 512 x 16-KiB tiles saturate the 8-MiB byte envelope in one submission.
// A count guard remains to avoid pathological command-buffer fan-out for
// tiny payloads without throttling normal 2D brick streams to 128 KiB/frame.
const MAX_UPLOADS: usize = 512;
const MAX_PAYLOAD_UPLOAD_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CONTROL_UPLOAD_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_RENDER_WIDTH_PIXELS: u32 = 1_920;
pub const MAX_RENDER_HEIGHT_PIXELS: u32 = 1_080;

/// Controls when retained residency progress produces a volume pass.
///
/// Both policies use the same upload arena, global residency/control metadata,
/// and frame transaction. `ExactFrameOnly` is for hidden atomic refinement:
/// incomplete cohorts become resident without raymarching pixels that cannot
/// be shown, and the cohort that completes exact coverage renders once in the
/// same GPU submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainedFrameRenderPolicy {
    EveryUsefulFrame,
    ExactFrameOnly,
}

/// Scheduling policy for one coordinated 3D color request.
///
/// `InteractivePreview` renders a complete provisional volume at the physical
/// `output_extent`. Reduced internal preview extents are not supported.
/// `AtomicRefinement` renders the exact native-size private candidate in
/// bounded horizontal strips; partial strips never become visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeColorSchedule {
    Direct,
    InteractivePreview,
    AtomicRefinement { strip_height_pixels: u32 },
}

/// Hidden exact-volume construction progress for one coordinated cutoff.
///
/// The historically named fields now carry completed and total screen rows,
/// not dataset resources or visible presentation frames. A report with
/// `completed_strips < total_strips` must also report `presented == false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeRefinementProgress {
    completed_strips: u32,
    total_strips: u32,
}

impl VolumeRefinementProgress {
    const fn new(completed_strips: u32, total_strips: u32) -> Self {
        Self {
            completed_strips,
            total_strips,
        }
    }

    pub const fn completed_strips(self) -> u32 {
        self.completed_strips
    }

    pub const fn total_strips(self) -> u32 {
        self.total_strips
    }

    pub const fn is_complete(self) -> bool {
        self.completed_strips == self.total_strips
    }
}

/// Monotone identity of one renderer-owned logical target texture allocation.
///
/// The value changes only when the renderer replaces the allocation backing a
/// fixed coordinated target. Presentation code can therefore refresh its
/// borrowed texture registration without owning allocation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TargetTextureRevision(u64);

impl TargetTextureRevision {
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Generation of the renderer-owned WGPU device authority.
///
/// Mirante4D does not recover a failed device in place. A generation is
/// therefore stable for one runtime and changes only with construction of a
/// replacement renderer root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RendererDeviceGeneration(u64);

impl RendererDeviceGeneration {
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Desired physical allocation for one fixed logical presentation target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoordinatedTargetLayout {
    target: PresentationTarget,
    extent: RenderExtent,
}

impl CoordinatedTargetLayout {
    pub const fn new(target: PresentationTarget, extent: RenderExtent) -> Self {
        Self { target, extent }
    }

    pub const fn target(self) -> PresentationTarget {
        self.target
    }

    pub const fn extent(self) -> RenderExtent {
        self.extent
    }
}

/// Current renderer-owned allocation for one desired logical target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoordinatedTargetLayoutState {
    target: PresentationTarget,
    extent: RenderExtent,
    device_generation: RendererDeviceGeneration,
    texture_revision: TargetTextureRevision,
}

impl CoordinatedTargetLayoutState {
    pub const fn target(self) -> PresentationTarget {
        self.target
    }

    pub const fn extent(self) -> RenderExtent {
        self.extent
    }

    pub const fn device_generation(self) -> RendererDeviceGeneration {
        self.device_generation
    }

    pub const fn texture_revision(self) -> TargetTextureRevision {
        self.texture_revision
    }
}

/// Exact fixed-target result of one desired-layout request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatedLayoutReport {
    targets: Box<[CoordinatedTargetLayoutState]>,
}

impl CoordinatedLayoutReport {
    pub fn targets(&self) -> &[CoordinatedTargetLayoutState] {
        &self.targets
    }

    pub fn target(&self, target: PresentationTarget) -> Option<CoordinatedTargetLayoutState> {
        self.targets
            .iter()
            .copied()
            .find(|state| state.target == target)
    }
}

/// Borrowed scientific request for one fixed target in a coordinated cutoff.
#[derive(Debug, Clone, Copy)]
pub struct CoordinatedTargetRequest<'a> {
    target: PresentationTarget,
    intent: &'a RenderIntent,
    requirements: &'a RenderRequirements,
    display_generation: u64,
    render_policy: RetainedFrameRenderPolicy,
    output_extent: RenderExtent,
    volume_schedule: VolumeColorSchedule,
    measure_gpu_timing: bool,
    hidden_promotion_authorized: bool,
}

impl<'a> CoordinatedTargetRequest<'a> {
    pub const fn new(
        target: PresentationTarget,
        intent: &'a RenderIntent,
        requirements: &'a RenderRequirements,
        display_generation: u64,
        render_policy: RetainedFrameRenderPolicy,
    ) -> Self {
        Self {
            target,
            intent,
            requirements,
            display_generation,
            render_policy,
            output_extent: intent.extent(),
            volume_schedule: VolumeColorSchedule::Direct,
            measure_gpu_timing: false,
            hidden_promotion_authorized: true,
        }
    }

    /// Configures the one 3D presentation schedule for this request.
    ///
    /// The renderer validates that previews and strips are used only for the
    /// 3D volume target. Native previews require `intent.extent()` and
    /// `output_extent` to be the same physical panel extent.
    pub const fn with_volume_schedule(
        mut self,
        output_extent: RenderExtent,
        schedule: VolumeColorSchedule,
        measure_gpu_timing: bool,
    ) -> Self {
        self.output_extent = output_extent;
        self.volume_schedule = schedule;
        self.measure_gpu_timing = measure_gpu_timing;
        self
    }

    /// Controls whether a complete private exact-volume candidate may replace
    /// the visible preview during this cutoff.
    ///
    /// Product staging uses this as the final transaction boundary: GPU work
    /// may complete at any time, but publication waits until the application
    /// has preflighted the matching dataset/residency promotion.
    pub const fn with_hidden_promotion_authorized(mut self, authorized: bool) -> Self {
        self.hidden_promotion_authorized = authorized;
        self
    }

    pub const fn target(self) -> PresentationTarget {
        self.target
    }

    pub const fn intent(self) -> &'a RenderIntent {
        self.intent
    }

    pub const fn requirements(self) -> &'a RenderRequirements {
        self.requirements
    }

    pub const fn display_generation(self) -> u64 {
        self.display_generation
    }

    pub const fn render_policy(self) -> RetainedFrameRenderPolicy {
        self.render_policy
    }

    pub const fn output_extent(self) -> RenderExtent {
        self.output_extent
    }

    pub const fn volume_schedule(self) -> VolumeColorSchedule {
        self.volume_schedule
    }

    pub const fn measure_gpu_timing(self) -> bool {
        self.measure_gpu_timing
    }

    pub const fn hidden_promotion_authorized(self) -> bool {
        self.hidden_promotion_authorized
    }
}

/// Consequences for one logical target in a coordinated cutoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatedTargetExecutionReport {
    target: PresentationTarget,
    device_generation: RendererDeviceGeneration,
    texture_revision: TargetTextureRevision,
    frame: FrameIdentity,
    progress: Option<FrameProgress>,
    presented: bool,
    visited_resources: usize,
    uploaded_resources: usize,
    payload_upload_bytes: u64,
    control_upload_bytes: u64,
    residency_command_buffers: u32,
    residency_queue_submissions: u32,
    deferred_by_backpressure: bool,
    volume_refinement: Option<VolumeRefinementProgress>,
    validation_capture: Option<CoordinatedValidationCaptureTicket>,
    newly_resident_keys: Box<[BrickKey]>,
    evicted_keys: Box<[BrickKey]>,
}

impl CoordinatedTargetExecutionReport {
    pub const fn target(&self) -> PresentationTarget {
        self.target
    }

    pub const fn texture_revision(&self) -> TargetTextureRevision {
        self.texture_revision
    }

    pub const fn device_generation(&self) -> RendererDeviceGeneration {
        self.device_generation
    }

    pub const fn frame(&self) -> FrameIdentity {
        self.frame
    }

    pub const fn progress(&self) -> Option<&FrameProgress> {
        self.progress.as_ref()
    }

    pub const fn presented(&self) -> bool {
        self.presented
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

    pub const fn residency_command_buffers(&self) -> u32 {
        self.residency_command_buffers
    }

    pub const fn residency_queue_submissions(&self) -> u32 {
        self.residency_queue_submissions
    }

    pub const fn deferred_by_backpressure(&self) -> bool {
        self.deferred_by_backpressure
    }

    pub const fn volume_refinement(&self) -> Option<VolumeRefinementProgress> {
        self.volume_refinement
    }

    pub const fn validation_capture(&self) -> Option<CoordinatedValidationCaptureTicket> {
        self.validation_capture
    }

    pub fn newly_resident_keys(&self) -> &[BrickKey] {
        &self.newly_resident_keys
    }

    pub fn evicted_keys(&self) -> &[BrickKey] {
        &self.evicted_keys
    }
}

/// Opaque capture identity for one fixed coordinated target.
///
/// Private allocation and dataset-generation facts remain renderer-internal
/// while the fixed target/revision facts are the public identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CoordinatedValidationCaptureTicket {
    target: PresentationTarget,
    device_generation: RendererDeviceGeneration,
    texture_revision: TargetTextureRevision,
    residency_generation: u64,
    inner: ValidationCaptureTicket,
}

impl CoordinatedValidationCaptureTicket {
    pub const fn target(self) -> PresentationTarget {
        self.target
    }

    pub const fn device_generation(self) -> RendererDeviceGeneration {
        self.device_generation
    }

    pub const fn texture_revision(self) -> TargetTextureRevision {
        self.texture_revision
    }

    pub const fn frame(self) -> FrameIdentity {
        self.inner.frame()
    }

    pub const fn extent(self) -> RenderExtent {
        self.inner.extent()
    }
}

/// One renderer cutoff over the fixed four-target layout.
///
/// `recorded_targets` is the exact color-pass order. Residency-only transfer
/// submissions, when present, remain separately accounted by the per-target
/// execution reports and never inflate `color_queue_submissions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatedFrameExecutionReport {
    targets: Box<[CoordinatedTargetExecutionReport]>,
    recorded_targets: Box<[PresentationTarget]>,
    residency_queue_submissions: u32,
    color_queue_submissions: u32,
    cpu_timing: Option<CpuFrameTiming>,
    gpu_timing: Option<GpuTimingTicket>,
}

impl CoordinatedFrameExecutionReport {
    pub fn targets(&self) -> &[CoordinatedTargetExecutionReport] {
        &self.targets
    }

    pub fn target(&self, target: PresentationTarget) -> Option<&CoordinatedTargetExecutionReport> {
        self.targets.iter().find(|report| report.target == target)
    }

    pub fn recorded_targets(&self) -> &[PresentationTarget] {
        &self.recorded_targets
    }

    pub const fn residency_queue_submissions(&self) -> u32 {
        self.residency_queue_submissions
    }

    pub const fn color_queue_submissions(&self) -> u32 {
        self.color_queue_submissions
    }

    pub const fn cpu_timing(&self) -> Option<CpuFrameTiming> {
        self.cpu_timing
    }

    /// Timestamp ticket for the active target's color pass when timing is
    /// enabled and a coordinated color cutoff was submitted.
    pub const fn gpu_timing(&self) -> Option<GpuTimingTicket> {
        self.gpu_timing
    }
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

/// One exact destructive change in renderer-global GPU residency.
///
/// Events remain queryable until acknowledged, superseded by a later
/// re-eviction of the same key, or cancelled by that key becoming resident
/// again. Sequence numbers are renderer-local and monotonically increasing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuResidencyEvictionEvent {
    key: BrickKey,
    sequence: u64,
}

impl GpuResidencyEvictionEvent {
    const fn new(key: BrickKey, sequence: u64) -> Self {
        Self { key, sequence }
    }

    pub const fn key(self) -> BrickKey {
        self.key
    }

    pub const fn sequence(self) -> u64 {
        self.sequence
    }
}

impl FrameBudget {
    /// Returns the fixed interactive budget. It is not caller-expandable.
    pub const fn interactive() -> Self {
        Self {
            resident_resources_visited: MAX_UPLOADS,
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

/// Product pipeline capability compiled by the renderer's bounded startup
/// worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineCapability {
    /// All color pipelines required by the current renderer.
    InitialRender,
    /// Asynchronous volume picking.
    Pick,
}

/// Observable readiness of the product renderer's fixed pipeline set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineReadiness {
    /// The initial color pipelines are still compiling.
    CompilingInitial,
    /// Color rendering is available while the pick pipeline compiles.
    InitialRenderReady,
    /// Color rendering and picking are both available.
    Ready,
}

/// Stable classification of a pipeline-worker failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineCompilationFailureCause {
    Validation,
    DeviceOutOfMemory,
    BackendInternal,
    WorkerPanicked,
    WorkerStopped,
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
    payload_committed_capacity_bytes: u64,
    payload_uncommitted_capacity_bytes: u64,
    payload_segment_committed_capacity_bytes: [u64; 4],
    payload_growths: u64,
    payload_growth_copy_bytes: u64,
    resident_payload_used_bytes: u64,
    peak_resident_payload_used_bytes: u64,
    payload_free_bytes: u64,
    payload_largest_contiguous_bytes: u64,
    payload_segment_free_bytes: [u64; 4],
    payload_segment_largest_contiguous_bytes: [u64; 4],
    payload_placeability_failures: u64,
    payload_compactions: u64,
    payload_compaction_resources_moved: u64,
    payload_compaction_bytes_moved: u64,
    empty_resident_metadata_capacity_records: usize,
    empty_resident_metadata_bytes_per_record: u64,
    empty_resident_metadata_records: usize,
    empty_resident_metadata_bytes: u64,
    peak_empty_resident_metadata_bytes: u64,
    peak_transfer_bytes: u64,
    peak_display_target_bytes: u64,
    peak_control_and_residency_metadata_bytes: u64,
    peak_scratch_bytes: u64,
    frames_executed: u64,
    queue_submissions: u64,
    current_in_flight_submissions: usize,
    peak_in_flight_submissions: usize,
    peak_in_flight_color_cutoffs: usize,
    backpressure_deferrals: u64,
    residency_hits: u64,
    residency_misses: u64,
    residency_evictions: u64,
    residency_epoch_reuploads: u64,
    uploaded_resources: u64,
    uploaded_payload_bytes: u64,
    upload_staging_padding_zero_bytes: u64,
    render_thread_payload_fact_scan_bytes: u64,
    directory_publications: u64,
    directory_mutations: u64,
    directory_rebuilds: u64,
    directory_slot_writes: u64,
    page_record_writes: u64,
    target_control_updates: u64,
    target_control_upload_bytes: u64,
    control_buffer_allocations: u64,
    control_buffer_allocation_bytes: u64,
    bind_group_creations: u64,
    usable_pipeline_handles: u64,
    explicit_staging_allocations: u64,
    explicit_staging_bytes: u64,
    peak_explicit_staging_bytes: u64,
    allocator_plans: u64,
    retained_navigation_frames: u64,
    cold_coverage_membership_checks: u64,
    cold_coverage_resident_matches: u64,
    gpu_timestamps_supported: bool,
    gpu_timing_enabled: bool,
    gpu_encoder_timestamps_supported: bool,
    completed_gpu_timings: u64,
    gpu_timing_failures: u64,
    last_gpu_batch_envelope_ns: Option<u64>,
    last_gpu_render_pass_ns: Option<u64>,
    completed_cpu_timings: u64,
    last_cpu_timing_frame: Option<u64>,
    last_cpu_planning_ns: Option<u64>,
    last_cpu_queue_submit_ns: Option<u64>,
    total_cpu_planning_ns: u64,
    total_cpu_queue_submit_ns: u64,
    hidden_refinement_jobs_started: u64,
    hidden_refinement_jobs_completed: u64,
    hidden_refinement_jobs_cancelled: u64,
    hidden_refinement_jobs_failed: u64,
    hidden_refinement_batches: u64,
    hidden_refinement_rows: u64,
    hidden_refinement_elapsed_ns: u64,
    hidden_refinement_last_batch_rows: Option<u32>,
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

    /// Logical payload segment maxima in deterministic allocation order.
    /// Entries after [`Self::payload_segment_count`] have zero logical
    /// capacity; their required shader bindings alias the first valid segment.
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

    /// Usable payload bytes currently backed by physical buffers.
    pub const fn payload_committed_capacity_bytes(&self) -> u64 {
        self.payload_committed_capacity_bytes
    }

    /// Logical payload capacity not yet physically committed.
    pub const fn payload_uncommitted_capacity_bytes(&self) -> u64 {
        self.payload_uncommitted_capacity_bytes
    }

    pub const fn payload_segment_committed_capacity_bytes(&self) -> &[u64; 4] {
        &self.payload_segment_committed_capacity_bytes
    }

    pub const fn payload_growths(&self) -> u64 {
        self.payload_growths
    }

    pub const fn payload_growth_copy_bytes(&self) -> u64 {
        self.payload_growth_copy_bytes
    }

    pub const fn resident_payload_bytes(&self) -> u64 {
        self.resident_payload_used_bytes
    }

    pub const fn peak_resident_payload_bytes(&self) -> u64 {
        self.peak_resident_payload_used_bytes
    }

    /// Aggregate free payload bytes. This is not itself one allocatable range.
    pub const fn payload_free_bytes(&self) -> u64 {
        self.payload_free_bytes
    }

    /// Largest currently allocatable contiguous range in any payload segment.
    pub const fn payload_largest_contiguous_bytes(&self) -> u64 {
        self.payload_largest_contiguous_bytes
    }

    pub const fn payload_segment_free_bytes(&self) -> &[u64; 4] {
        &self.payload_segment_free_bytes
    }

    pub const fn payload_segment_largest_contiguous_bytes(&self) -> &[u64; 4] {
        &self.payload_segment_largest_contiguous_bytes
    }

    pub const fn payload_placeability_failures(&self) -> u64 {
        self.payload_placeability_failures
    }

    pub const fn payload_compactions(&self) -> u64 {
        self.payload_compactions
    }

    pub const fn payload_compaction_resources_moved(&self) -> u64 {
        self.payload_compaction_resources_moved
    }

    pub const fn payload_compaction_bytes_moved(&self) -> u64 {
        self.payload_compaction_bytes_moved
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

    pub const fn peak_control_and_residency_metadata_bytes(&self) -> u64 {
        self.peak_control_and_residency_metadata_bytes
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

    /// Maximum coordinated color cutoffs simultaneously owned by the GPU.
    /// Residency-transfer, pick, capture, and timing-only work do not count.
    pub const fn peak_in_flight_color_cutoffs(&self) -> usize {
        self.peak_in_flight_color_cutoffs
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

    /// Committed batches that changed the renderer-global residency directory.
    pub const fn directory_publications(&self) -> u64 {
        self.directory_publications
    }

    /// Exact compact-cell insertions and removals across committed directory
    /// batches.
    pub const fn directory_mutations(&self) -> u64 {
        self.directory_mutations
    }

    /// Bounded full-directory compactions among committed publications.
    pub const fn directory_rebuilds(&self) -> u64 {
        self.directory_rebuilds
    }

    /// Shader-visible directory slots copied to the GPU. Incremental batches
    /// count their touched slots; a rebuild counts its complete fixed image.
    pub const fn directory_slot_writes(&self) -> u64 {
        self.directory_slot_writes
    }

    /// Shader-visible page records written or cleared after successful
    /// residency publication.
    pub const fn page_record_writes(&self) -> u64 {
        self.page_record_writes
    }

    /// Target-local frame-control blocks published before color work.
    pub const fn target_control_updates(&self) -> u64 {
        self.target_control_updates
    }

    pub const fn target_control_upload_bytes(&self) -> u64 {
        self.target_control_upload_bytes
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

    /// Successfully adopted product pipeline handles that are usable by this
    /// runtime. Buffered worker results and failed or cancelled creation
    /// attempts are deliberately not counted.
    pub const fn usable_pipeline_handles(&self) -> u64 {
        self.usable_pipeline_handles
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

    pub const fn cold_coverage_resident_matches(&self) -> u64 {
        self.cold_coverage_resident_matches
    }

    pub const fn gpu_timestamps_supported(&self) -> bool {
        self.gpu_timestamps_supported
    }

    pub const fn gpu_timing_enabled(&self) -> bool {
        self.gpu_timing_enabled
    }

    pub const fn gpu_encoder_timestamps_supported(&self) -> bool {
        self.gpu_encoder_timestamps_supported
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

    pub const fn last_cpu_queue_submit_ns(&self) -> Option<u64> {
        self.last_cpu_queue_submit_ns
    }

    pub const fn total_cpu_planning_ns(&self) -> u64 {
        self.total_cpu_planning_ns
    }

    pub const fn total_cpu_queue_submit_ns(&self) -> u64 {
        self.total_cpu_queue_submit_ns
    }

    pub const fn hidden_refinement_jobs_started(&self) -> u64 {
        self.hidden_refinement_jobs_started
    }

    pub const fn hidden_refinement_jobs_completed(&self) -> u64 {
        self.hidden_refinement_jobs_completed
    }

    pub const fn hidden_refinement_jobs_cancelled(&self) -> u64 {
        self.hidden_refinement_jobs_cancelled
    }

    pub const fn hidden_refinement_jobs_failed(&self) -> u64 {
        self.hidden_refinement_jobs_failed
    }

    pub const fn hidden_refinement_batches(&self) -> u64 {
        self.hidden_refinement_batches
    }

    pub const fn hidden_refinement_rows(&self) -> u64 {
        self.hidden_refinement_rows
    }

    pub const fn hidden_refinement_elapsed_ns(&self) -> u64 {
        self.hidden_refinement_elapsed_ns
    }

    pub const fn hidden_refinement_last_batch_rows(&self) -> Option<u32> {
        self.hidden_refinement_last_batch_rows
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
    target: PresentationTarget,
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

    pub const fn target(self) -> PresentationTarget {
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
/// Render-pass time is available when this execution encoded the active
/// target's pass. When encoder timestamps are supported, the batch envelope
/// spans the coordinated color encoder's first timestamp through its final
/// timestamp. Queue writes and CPU staging are deliberately excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuFrameTiming {
    ticket: GpuTimingTicket,
    batch_gpu_envelope_ns: Option<u64>,
    render_pass_ns: Option<u64>,
}

/// CPU-domain work for one exact renderer execution.
///
/// Planning covers coordinated color-control preparation and command encoding.
/// The synchronous `Queue::submit` call is measured separately. Collection is
/// opt-in and never performs a GPU wait or readback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuFrameTiming {
    planning_ns: u64,
    queue_submit_ns: u64,
}

impl CpuFrameTiming {
    pub const fn new(planning_ns: u64, queue_submit_ns: u64) -> Self {
        Self {
            planning_ns,
            queue_submit_ns,
        }
    }

    pub const fn planning_ns(self) -> u64 {
        self.planning_ns
    }

    pub const fn queue_submit_ns(self) -> u64 {
        self.queue_submit_ns
    }
}

impl GpuFrameTiming {
    pub const fn ticket(self) -> GpuTimingTicket {
        self.ticket
    }

    pub const fn target(self) -> PresentationTarget {
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

    pub const fn render_pass_ns(self) -> Option<u64> {
        self.render_pass_ns
    }
}

/// Private identity embedded in a coordinated validation-capture ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ValidationCaptureTicket {
    id: u64,
    private_presentation: u64,
    frame: FrameIdentity,
    extent: RenderExtent,
}

impl ValidationCaptureTicket {
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

/// Typed, backend-neutral failures from the product GPU runtime.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum WgpuRenderRuntimeError {
    #[error("the WGPU runtime configuration is invalid")]
    InvalidConfiguration,
    #[error("a CPU or software adapter cannot run the interactive renderer")]
    SoftwareAdapter,
    #[error("the interactive renderer requires a Vulkan adapter")]
    UnsupportedBackend,
    #[error("the adapter does not satisfy the accepted renderer limits")]
    AdapterLimitsInsufficient,
    #[error("the existing WGPU device was created below the renderer limits")]
    DeviceLimitsInsufficient,
    #[error("the bounded GPU pipeline compiler worker could not be started")]
    PipelineCompilerSpawnFailed,
    #[error("the bounded hidden-refinement worker could not be started")]
    HiddenRefinementWorkerSpawnFailed,
    #[error("the renderer exhausted its hidden-refinement job identity space")]
    HiddenRefinementIdentityExhausted,
    #[error("hidden exact refinement failed before atomic promotion")]
    HiddenRefinementFailed,
    #[error("{capability:?} GPU pipeline compilation failed with first cause {cause:?}")]
    PipelineCompilationFailed {
        capability: PipelineCapability,
        cause: PipelineCompilationFailureCause,
    },
    #[error("{capability:?} GPU pipelines are not ready")]
    PipelineNotReady { capability: PipelineCapability },
    #[error("the active GPU device was lost")]
    DeviceLost,
    #[error("the GPU backend exhausted device memory")]
    DeviceOutOfMemory,
    #[error("the GPU backend reported an internal failure")]
    BackendInternal,
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
    #[error("the dataset generation's logical render-resource grid is invalid")]
    InvalidResourceGridCatalog,
    #[error(
        "the render requirement set contains {actual} resources, exceeding the renderer limit of {maximum}"
    )]
    RequirementCapacityExceeded { actual: usize, maximum: usize },
    #[error("the renderer reached its limit of {maximum} presentation targets")]
    PresentationCapacityExceeded { maximum: usize },
    #[error("the requested private renderer target is not allocated")]
    PresentationNotRegistered,
    #[error("the renderer exhausted its private target-slot identity space")]
    PrivatePresentationIdExhausted,
    #[error("the renderer exhausted its monotone target texture revision space")]
    TextureRevisionExhausted,
    #[error("the process exhausted its monotone renderer-device generation space")]
    RendererDeviceGenerationExhausted,
    #[error("coordinated target {target:?} occurs more than once in one request")]
    DuplicateCoordinatedTarget { target: PresentationTarget },
    #[error("coordinated target {target:?} has no desired renderer allocation")]
    CoordinatedTargetNotConfigured { target: PresentationTarget },
    #[error("coordinated target {target:?} received a mismatched view family")]
    CoordinatedTargetViewMismatch { target: PresentationTarget },
    #[error("coordinated target {target:?} received an invalid 3D color schedule")]
    InvalidVolumeColorSchedule { target: PresentationTarget },
    #[error(
        "coordinated target {target:?} requested extent {requested:?}, but its desired layout is {desired:?}"
    )]
    CoordinatedTargetExtentMismatch {
        target: PresentationTarget,
        requested: RenderExtent,
        desired: RenderExtent,
    },
    #[error(
        "the supplied lease set contains {actual} resources, exceeding the renderer limit of {maximum}"
    )]
    LeaseCapacityExceeded { actual: usize, maximum: usize },
    #[error(
        "unacknowledged residency evictions would require {actual} events, exceeding the renderer limit of {maximum}"
    )]
    ResidencyEvictionEventCapacityExceeded { actual: usize, maximum: usize },
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
        "GPU payload placement cannot fit one {requested_bytes}-byte allocation: \
         {total_free_bytes} bytes are free in aggregate, but the largest contiguous \
         range is {largest_contiguous_bytes} bytes"
    )]
    PayloadPlacementUnavailable {
        requested_bytes: u64,
        total_free_bytes: u64,
        largest_contiguous_bytes: u64,
    },
    #[error("GPU payload fragmentation recovery is deferred by bounded in-flight work")]
    PayloadRecoveryDeferred,
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
/// The requested features and limits are the fixed set required by
/// [`WgpuRenderRuntime::from_existing_device`].
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
    #[cfg(test)]
    fn from_existing_device_with_payload_segment_limit(
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: WgpuRenderRuntimeConfig,
        segment_limit: u64,
    ) -> Result<Self, WgpuRenderRuntimeError> {
        Ok(Self {
            inner: runtime::Runtime::from_existing_device_with_payload_segment_limit(
                adapter,
                device,
                queue,
                config,
                segment_limit,
            )?,
        })
    }

    #[cfg(test)]
    fn from_existing_device_with_payload_test_limits(
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: WgpuRenderRuntimeConfig,
        segment_limit: u64,
        initial_commitment: u64,
    ) -> Result<Self, WgpuRenderRuntimeError> {
        Ok(Self {
            inner: runtime::Runtime::from_existing_device_with_payload_test_limits(
                adapter,
                device,
                queue,
                config,
                segment_limit,
                initial_commitment,
            )?,
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

    /// Installs the native event-loop wake used only when hidden exact work
    /// reaches a terminal handoff. Hidden batches never use this callback as
    /// their scheduling clock.
    pub fn set_hidden_refinement_wake(&mut self, wake: Arc<dyn Fn() + Send + Sync + 'static>) {
        self.inner.set_hidden_refinement_wake(wake);
    }

    /// Returns the last readiness state observed by the runtime, or the first
    /// pipeline-compilation failure once one has been observed.
    pub fn pipeline_readiness(&self) -> Result<PipelineReadiness, WgpuRenderRuntimeError> {
        self.inner.pipeline_readiness()
    }

    /// Consumes at most one ordered event from the bounded compiler channel.
    ///
    /// The native application calls this once per UI turn. A cold runtime
    /// therefore exposes `InitialRenderReady` before `Ready` even when both
    /// events were produced between UI turns.
    pub fn poll_pipeline_readiness(&mut self) -> Result<PipelineReadiness, WgpuRenderRuntimeError> {
        self.inner.poll_pipeline_readiness()
    }

    /// Reports whether one capability is currently usable. A latched compiler
    /// failure is returned rather than hidden behind `false`.
    pub fn pipeline_capability_is_ready(
        &self,
        capability: PipelineCapability,
    ) -> Result<bool, WgpuRenderRuntimeError> {
        self.inner.pipeline_capability_is_ready(capability)
    }

    /// Applies the complete desired subset of the fixed four-target layout.
    ///
    /// Allocations are created lazily and replaced only when their physical
    /// extent changes. Omitted targets retain no active frame authority.
    pub fn request_coordinated_layout(
        &mut self,
        desired: &[CoordinatedTargetLayout],
    ) -> Result<CoordinatedLayoutReport, WgpuRenderRuntimeError> {
        self.inner.request_coordinated_layout(desired)
    }

    /// Borrows the current front color view for one fixed logical target.
    pub fn coordinated_target_texture_view(
        &self,
        target: PresentationTarget,
    ) -> Result<&wgpu::TextureView, WgpuRenderRuntimeError> {
        self.inner.coordinated_target_texture_view(target)
    }

    /// Reports whether either private allocation for one desired logical
    /// target has actionable residency or freshness work.
    pub fn coordinated_target_requires_execution(
        &self,
        target: PresentationTarget,
    ) -> Result<bool, WgpuRenderRuntimeError> {
        self.inner.coordinated_target_requires_execution(target)
    }

    /// Collects completed renderer-owned hidden work and reports whether the
    /// exact private candidate matching this request is ready for application
    /// preflight and atomic publication.
    pub fn poll_coordinated_hidden_refinement_ready(
        &mut self,
        request: CoordinatedTargetRequest<'_>,
    ) -> Result<bool, WgpuRenderRuntimeError> {
        self.inner.poll_coordinated_hidden_refinement_ready(request)
    }

    /// Reports whether a fixed target needs work for one exact prepared
    /// presentation identity.
    ///
    /// The ordinary target-level query can only observe the body already
    /// installed in the renderer. This request-aware form also detects an
    /// application-prepared successor body under the same semantic frame, so
    /// asynchronous planning cannot wait for another input revision to make
    /// the replacement visible.
    pub fn coordinated_target_requires_prepared_presentation(
        &self,
        target: PresentationTarget,
        frame: FrameIdentity,
        extent: RenderExtent,
        requirements: &PreparedRenderRequirements,
    ) -> Result<bool, WgpuRenderRuntimeError> {
        self.inner
            .coordinated_target_requires_prepared_presentation(target, frame, extent, requirements)
    }

    /// Request-bound counterpart of
    /// [`Self::coordinated_target_requires_prepared_presentation`].
    ///
    /// This is used at retained-presentation adoption boundaries, where frame
    /// and immutable body identity must both match before existing pixels can
    /// satisfy a new request. The volume schedule is also authoritative:
    /// provisional preview pixels, including a full-size preview, cannot
    /// satisfy a direct or atomic exact request. Work owned by the private 3D
    /// candidate does not invalidate an otherwise matching visible front.
    pub fn coordinated_target_requires_render_presentation(
        &self,
        target: PresentationTarget,
        extent: RenderExtent,
        requirements: &RenderRequirements,
        volume_schedule: VolumeColorSchedule,
    ) -> Result<bool, WgpuRenderRuntimeError> {
        self.inner.coordinated_target_requires_render_presentation(
            target,
            extent,
            requirements,
            volume_schedule,
        )
    }

    /// Executes one ordered cutoff over the desired fixed-target layout.
    ///
    /// Eligible resident color passes are recorded active-first into one
    /// command encoder and use exactly one uninstrumented color submission.
    pub fn execute_coordinated_frame(
        &mut self,
        catalog: &DatasetCatalog,
        active_target: PresentationTarget,
        targets: &[CoordinatedTargetRequest<'_>],
    ) -> Result<CoordinatedFrameExecutionReport, WgpuRenderRuntimeError> {
        self.inner
            .execute_coordinated_frame(catalog, active_target, targets)
    }

    /// Polls one target/revision-bound coordinated validation capture.
    pub fn poll_coordinated_validation_capture(
        &mut self,
        ticket: CoordinatedValidationCaptureTicket,
    ) -> Result<Option<ValidationCapture>, WgpuRenderRuntimeError> {
        self.inner.poll_coordinated_validation_capture(ticket)
    }

    /// Retires all dataset-generation-scoped residency, controls, and
    /// asynchronous results while retaining the selected device, pipelines,
    /// and registered presentation targets for the replacement source.
    pub fn retire_dataset_generation(&mut self) {
        self.inner.retire_dataset_generation();
    }

    /// Installs the one canonical logical resource grid for an opened dataset
    /// generation. Presentation bodies may select its layer/scales but never
    /// infer or redefine GPU brick coordinates.
    pub fn activate_dataset_generation(
        &mut self,
        catalog: &DatasetCatalog,
    ) -> Result<(), WgpuRenderRuntimeError> {
        self.inner.activate_dataset_generation(catalog)
    }

    #[cfg(test)]
    fn activate_dataset_generation_with_resource_grids(
        &mut self,
        catalog: &DatasetCatalog,
        grids: &RenderResourceGridCatalog,
    ) -> Result<(), WgpuRenderRuntimeError> {
        self.inner
            .activate_dataset_generation_with_resource_grids(catalog, grids)
    }

    /// Offers decoded payload leases to the renderer-global residency inbox.
    ///
    /// The batch is atomic and idempotent. Resident, already-pending, and
    /// duplicate keys are no-ops; newly admitted leases retain caller order
    /// until a relevant presentation drains them under the fixed per-execution
    /// transfer bounds.
    pub fn offer_residency_leases(
        &mut self,
        leases: &[Arc<dyn ResourceLease>],
    ) -> Result<(), WgpuRenderRuntimeError> {
        self.inner.offer_residency_leases(leases)
    }

    /// Retires pending CPU lease offers removed from the global retained
    /// resource union. This is idempotent and never evicts an already-resident
    /// GPU page.
    pub fn retire_residency_offers(&mut self, keys: &[BrickKey]) {
        self.inner.retire_residency_offers(keys);
    }

    /// Returns whether any active target has actionable offered residency
    /// work. Offers for a future or unrelated target remain retained without
    /// forcing idle repaint polling.
    pub fn has_pending_residency_work(&self) -> bool {
        self.inner.has_pending_residency_work()
    }

    /// Returns up to `maximum` unacknowledged destructive residency changes
    /// in eviction order. Repeated calls return the same events until they are
    /// acknowledged, superseded by a later eviction of the same key, or
    /// cancelled by that key becoming resident again.
    pub fn pending_residency_evictions(&self, maximum: usize) -> Box<[GpuResidencyEvictionEvent]> {
        self.inner.pending_residency_evictions(maximum)
    }

    pub fn has_pending_residency_evictions(&self) -> bool {
        self.inner.has_pending_residency_evictions()
    }

    /// Acknowledges every still-current eviction event through the supplied
    /// sequence. A later re-eviction of the same key is a distinct event and
    /// is never removed by acknowledging an older sequence.
    pub fn acknowledge_residency_evictions(&mut self, through_sequence: u64) {
        self.inner.acknowledge_residency_evictions(through_sequence);
    }

    /// Effective usable payload arena after device binding limits and the
    /// configured ledger are both applied.
    pub const fn payload_capacity_bytes(&self) -> u64 {
        self.inner.payload_capacity_bytes()
    }

    pub const fn resident_payload_bytes(&self) -> u64 {
        self.inner.resident_payload_bytes()
    }

    pub const fn available_payload_bytes(&self) -> u64 {
        self.payload_capacity_bytes()
            .saturating_sub(self.resident_payload_bytes())
    }

    /// Ensures that one exact aggregate visible/replacement union is physically
    /// placeable in the renderer's sole segmented arena and current frame pins.
    ///
    /// A fragmented arena is compacted once inside the residency owner before
    /// the exact transactional preflight is retried. Logical residency, pins,
    /// and requirement identity never leave the renderer.
    pub fn ensure_global_requirement_union(
        &mut self,
        catalog: &DatasetCatalog,
        requirements: &[BrickKey],
    ) -> Result<(), WgpuRenderRuntimeError> {
        self.inner
            .ensure_global_requirement_union(catalog, requirements)
    }

    /// Attempts one physical-only compaction after a transfer-time placement
    /// refusal. Returns whether any payload moved.
    pub fn recover_payload_fragmentation(&mut self) -> Result<bool, WgpuRenderRuntimeError> {
        self.inner.recover_payload_fragmentation()
    }

    /// Exact read-only membership query for the current residency epoch.
    pub fn resource_is_resident(&self, key: BrickKey) -> bool {
        self.inner.resource_is_resident(key)
    }

    /// Polls one asynchronous GPU-domain timing without waiting for the GPU.
    pub fn poll_gpu_timing(
        &mut self,
        ticket: GpuTimingTicket,
    ) -> Result<Option<GpuFrameTiming>, WgpuRenderRuntimeError> {
        self.inner.poll_gpu_timing(ticket)
    }

    /// Submits one pick against the exact visible 3D front allocation.
    ///
    /// An incomplete private refinement candidate is never resolved by this
    /// entrypoint and therefore cannot become accidental pick authority.
    pub fn request_coordinated_pick(
        &mut self,
        target: PresentationTarget,
        query: VolumePickQuery,
    ) -> Result<VolumePickTicket, WgpuRenderRuntimeError> {
        self.inner.request_coordinated_pick(target, query)
    }

    /// Polls once without waiting. A completed result preserves the exact
    /// query supplied to [`Self::request_coordinated_pick`].
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
        assert_eq!(budget.resident_resources_visited(), 512);
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
