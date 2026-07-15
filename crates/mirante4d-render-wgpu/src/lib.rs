//! Progressive WGPU product-rendering runtime.
//!
//! This crate owns product GPU resources and consumes only semantic dataset
//! leases and backend-neutral render contracts.

#![forbid(unsafe_code)]

mod runtime;

use mirante4d_dataset::{DatasetCatalog, ResourceLease};
use mirante4d_render_api::{
    FrameIdentity, FrameProgress, GpuLedgerCategory, PresentationRegistration,
    PresentationRetirement, PresentationToken, PresentedFrame, RenderExtent, RenderIntent,
    RenderRequirements,
};
use thiserror::Error;

const MAX_VISITS: usize = 128;
const MAX_UPLOADS: usize = 8;
const MAX_PAYLOAD_UPLOAD_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CONTROL_UPLOAD_BYTES: u64 = 64 * 1024;
const UNKNOWN_GPU_BUDGET_BYTES: u64 = 1024 * 1024 * 1024;

/// The exact per-frame WP-09A work ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameBudget {
    resident_resources_visited: usize,
    new_resources_uploaded: usize,
    payload_upload_bytes: u64,
    control_upload_bytes: u64,
    command_buffers: u32,
    queue_submissions: u32,
}

impl FrameBudget {
    /// Returns the accepted WP-09A budget. It is not caller-expandable.
    pub const fn wp09a() -> Self {
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

/// Configuration for one off-product GPU authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WgpuRenderRuntimeConfig {
    gpu_budget_bytes: u64,
    validation_capture: bool,
}

impl WgpuRenderRuntimeConfig {
    pub fn new(gpu_budget_bytes: u64) -> Result<Self, WgpuRenderRuntimeError> {
        if gpu_budget_bytes < 1024 * 1024 {
            return Err(WgpuRenderRuntimeError::InvalidConfiguration);
        }
        Ok(Self {
            gpu_budget_bytes,
            validation_capture: false,
        })
    }

    pub const fn unknown_capacity() -> Self {
        Self {
            gpu_budget_bytes: UNKNOWN_GPU_BUDGET_BYTES,
            validation_capture: false,
        }
    }

    pub const fn with_validation_capture(mut self, enabled: bool) -> Self {
        self.validation_capture = enabled;
        self
    }

    pub const fn gpu_budget_bytes(self) -> u64 {
        self.gpu_budget_bytes
    }

    pub const fn validation_capture(self) -> bool {
        self.validation_capture
    }
}

/// Stable counters and sanitized adapter facts for the successor runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WgpuRenderRuntimeDiagnostics {
    adapter_name: String,
    backend: String,
    driver: String,
    max_buffer_size_bytes: u64,
    max_storage_buffer_binding_size_bytes: u64,
    max_storage_buffers_per_shader_stage: u32,
    gpu_budget_bytes: u64,
    payload_capacity_bytes: u64,
    transfer_capacity_bytes: u64,
    other_capacity_bytes: u64,
    payload_arena_allocated_bytes: u64,
    resident_payload_used_bytes: u64,
    peak_resident_payload_used_bytes: u64,
    peak_transfer_bytes: u64,
    peak_display_target_bytes: u64,
    peak_page_table_bytes: u64,
    peak_scratch_bytes: u64,
    frames_executed: u64,
    queue_submissions: u64,
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

    pub const fn validation_error_count(&self) -> u64 {
        self.validation_error_count
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
    validation_capture: Option<ValidationCaptureTicket>,
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

    pub const fn validation_capture(&self) -> Option<ValidationCaptureTicket> {
        self.validation_capture
    }
}

/// Typed, backend-neutral failures from the successor GPU runtime.
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
    #[error("the adapter does not satisfy the accepted WP-09A limits")]
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
    #[error(
        "the render requirement set contains {actual} resources, exceeding the successor limit of {maximum}"
    )]
    RequirementCapacityExceeded { actual: usize, maximum: usize },
    #[error("the successor renderer reached its limit of {maximum} presentation targets")]
    PresentationCapacityExceeded { maximum: usize },
    #[error("presentation token {token:?} is not registered in this renderer")]
    PresentationNotRegistered { token: PresentationToken },
    #[error("the successor renderer exhausted its presentation token space")]
    PresentationTokenExhausted,
    #[error(
        "the supplied lease set contains {actual} resources, exceeding the successor limit of {maximum}"
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
    #[error("the accepted qualification renderer does not support this view transform")]
    UnsupportedView,
    #[error("the accepted qualification renderer supports only voxel-exact sampling")]
    UnsupportedSampling,
    #[error("the accepted qualification renderer supports only flat ISO shading")]
    UnsupportedIsoShading,
    #[error("semantic coordinates exceed the bounded GPU metadata representation")]
    CoordinateLimitExceeded,
    #[error("one qualification ray may exceed the 16,384-sample WP-09A ceiling")]
    RaySampleLimitExceeded,
    #[error("frame control metadata exceeds its 64-KiB ceiling")]
    ControlCapacityExceeded,
    #[error(
        "GPU capacity in {category:?} cannot satisfy {requested_bytes} bytes with {available_bytes} bytes available"
    )]
    CapacityExceeded {
        category: GpuLedgerCategory,
        requested_bytes: u64,
        available_bytes: u64,
    },
    #[error("backend validation rejected successor render work")]
    BackendValidation,
    #[error("validation capture does not belong to this runtime")]
    UnknownValidationCapture,
    #[error("validation capture belongs to a stale frame generation")]
    StaleValidationCapture,
    #[error("validation capture mapping failed")]
    ValidationCaptureFailed,
    #[error("backend-neutral frame progress construction failed")]
    FrameProgressContract,
}

/// Checks whether an adapter can run the interactive successor renderer.
///
/// Product startup uses this before asking WGPU to create the window device,
/// so unsupported software, non-Vulkan, and undersized adapters fail before
/// the viewer opens.
pub fn qualify_adapter(adapter: &wgpu::Adapter) -> Result<(), WgpuRenderRuntimeError> {
    runtime::validate_adapter(adapter)
}

/// Builds the device request used by the interactive successor renderer.
///
/// The requested features and limits are the same fixed set used by
/// [`WgpuRenderRuntime::new`].
pub fn renderer_device_descriptor(
    adapter: &wgpu::Adapter,
    label: &'static str,
) -> Result<wgpu::DeviceDescriptor<'static>, WgpuRenderRuntimeError> {
    runtime::renderer_device_descriptor(adapter, label)
}

/// Sole off-product owner of successor WGPU resources.
pub struct WgpuRenderRuntime {
    inner: runtime::Runtime,
}

impl WgpuRenderRuntime {
    /// Creates a real Vulkan device and the offscreen successor pipeline.
    pub async fn new(config: WgpuRenderRuntimeConfig) -> Result<Self, WgpuRenderRuntimeError> {
        Ok(Self {
            inner: runtime::Runtime::new(config).await?,
        })
    }

    /// Builds the successor around the device already selected for the native
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
        FrameBudget::wp09a()
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

    /// Polls once without waiting. `None` means the GPU/map callback has not
    /// completed yet and the caller should poll on a later event-loop turn.
    pub fn poll_validation_capture(
        &mut self,
        ticket: ValidationCaptureTicket,
    ) -> Result<Option<ValidationCapture>, WgpuRenderRuntimeError> {
        self.inner.poll_validation_capture(ticket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_frame_budget_is_exact() {
        let budget = FrameBudget::wp09a();
        assert_eq!(budget.resident_resources_visited(), 128);
        assert_eq!(budget.new_resources_uploaded(), 8);
        assert_eq!(budget.payload_upload_bytes(), 8 * 1024 * 1024);
        assert_eq!(budget.control_upload_bytes(), 64 * 1024);
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
}

#[cfg(test)]
mod gpu_tests;
