//! Backend-neutral render, progressive-frame, and presentation contracts.
//!
//! This crate describes what to render and how a rendered frame is identified
//! and presented. It owns no dataset payload, scheduler, GPU resource,
//! presentation backend, UI object, serialization, or I/O behavior.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, OnceLock},
};

use mirante4d_dataset::{CpuByteLease, DatasetResourceIdentity, DatasetResourceKey};
use mirante4d_domain::{
    CameraView, CrossSectionView, IsoLightState, LayerTransfer, RenderState, UnitQuaternion,
};
pub use mirante4d_domain::{
    IsoShadingPolicy, LogicalLayerKey, Projection, SamplingPolicy, TimeIndex, WorldPoint3,
};
use thiserror::Error;

pub const MAX_RENDER_LAYERS: usize = 64;
pub const MAX_RENDER_REQUIREMENTS: usize = 65_536;
pub const MAX_PRESENTATION_TARGETS: usize = 64;
pub const DEFAULT_PRESENTATION_VIEWPORT: PresentationViewport =
    PresentationViewport::new_unchecked(512.0, 512.0);

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RenderApiError {
    #[error("presentation viewport dimensions must be finite and positive")]
    InvalidPresentationViewport,
    #[error("screen-point coordinates must be finite")]
    NonFiniteScreenPoint,
    #[error("render extent dimensions must be nonzero")]
    InvalidRenderExtent,
    #[error("render extent envelope dimensions must be nonzero")]
    InvalidRenderExtentEnvelope,
    #[error("render-pixel coordinates must be finite")]
    NonFiniteRenderPixel,
    #[error("volume-pick pixel lies outside its presented render extent")]
    PickPixelOutsideExtent,
    #[error("volume-pick result fields are inconsistent")]
    InvalidVolumePickResult,
    #[error("volume-pick ticket sequence must be nonzero")]
    InvalidVolumePickTicket,
    #[error("camera projection math produced a non-finite value")]
    CameraMathNotFinite,
    #[error("camera projection math produced a zero-length direction")]
    DegenerateViewDirection,
    #[error("a render intent must contain at least one visible layer")]
    EmptyRenderLayers,
    #[error("render intent contains {actual} layers, exceeding the limit of {maximum}")]
    TooManyRenderLayers { actual: usize, maximum: usize },
    #[error("logical layer key {ordinal} occurs more than once in one render intent")]
    DuplicateRenderLayer { ordinal: u32 },
    #[error("a render requirement set must not be empty")]
    EmptyRenderRequirements,
    #[error("render requirement set contains {actual} entries, exceeding the limit of {maximum}")]
    TooManyRenderRequirements { actual: usize, maximum: usize },
    #[error("one dataset resource occurs more than once in a render requirement set")]
    DuplicateRenderRequirement,
    #[error("requirement layer {ordinal} is absent from the render intent")]
    RequirementLayerNotInIntent { ordinal: u32 },
    #[error("requirement timepoint {actual} differs from render-intent timepoint {expected}")]
    RequirementTimepointMismatch { expected: u64, actual: u64 },
    #[error("requirement dataset/source identity differs from the render intent")]
    RequirementIdentityMismatch,
    #[error("a render requirement set must contain at least one first-useful-frame resource")]
    MissingFirstUsefulRequirement,
    #[error("prepared render requirement accounting charge is smaller than its host body")]
    PreparedRequirementChargeTooSmall,
    #[error("prepared render requirement accounting charge was already attached")]
    PreparedRequirementChargeAlreadyAttached,
    #[error("prepared render requirement host allocation size overflowed")]
    PreparedRequirementHostAllocationOverflow,
    #[error("one covered dataset resource occurs more than once")]
    DuplicateCoveredResource,
    #[error("frame coverage contains {actual} entries, exceeding its {maximum} requirements")]
    TooManyCoveredResources { actual: usize, maximum: usize },
    #[error("a covered dataset resource does not belong to the frame requirements")]
    CoveredResourceNotRequired,
    #[error("frame completeness, coverage, and limitation are inconsistent")]
    InvalidFrameProgress,
    #[error("presentation tokens must be nonzero")]
    InvalidPresentationToken,
}

/// A monotonically assigned identity used to suppress stale render results.
///
/// The assigning application/runtime decides when an intent is superseded;
/// render backends compare this value but do not reinterpret it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FrameIdentity(u64);

impl FrameIdentity {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A backend-neutral target size in physical render pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderExtent {
    width_pixels: u32,
    height_pixels: u32,
}

/// Backend capability envelope used by presentation code to negotiate HiDPI
/// render sizes before submitting them to a renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderExtentEnvelope {
    max_width_pixels: u32,
    max_height_pixels: u32,
}

impl RenderExtentEnvelope {
    pub fn new(max_width_pixels: u32, max_height_pixels: u32) -> Result<Self, RenderApiError> {
        if max_width_pixels == 0 || max_height_pixels == 0 {
            return Err(RenderApiError::InvalidRenderExtentEnvelope);
        }
        Ok(Self {
            max_width_pixels,
            max_height_pixels,
        })
    }

    pub const fn max_width_pixels(self) -> u32 {
        self.max_width_pixels
    }

    pub const fn max_height_pixels(self) -> u32 {
        self.max_height_pixels
    }
}

impl RenderExtent {
    pub fn new(width_pixels: u32, height_pixels: u32) -> Result<Self, RenderApiError> {
        if width_pixels == 0 || height_pixels == 0 {
            return Err(RenderApiError::InvalidRenderExtent);
        }
        Ok(Self {
            width_pixels,
            height_pixels,
        })
    }

    pub const fn width_pixels(self) -> u32 {
        self.width_pixels
    }

    pub const fn height_pixels(self) -> u32 {
        self.height_pixels
    }
}

/// The framework-neutral view for one render target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RenderViewIntent {
    Volume {
        camera: CameraView,
        iso_light: IsoLightState,
    },
    CrossSection(CrossSectionView),
}

/// Geometry family of the render pass required by one target.
///
/// This identity is deliberately independent of the GPU backend and volume
/// mode. A mixed-channel volume pass remains one volume pass, while every
/// arbitrary cross-section uses the plane family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RenderPassKind {
    Plane,
    Volume,
}

impl RenderViewIntent {
    pub const fn volume(camera: CameraView, iso_light: IsoLightState) -> Self {
        Self::Volume { camera, iso_light }
    }

    pub const fn cross_section(view: CrossSectionView) -> Self {
        Self::CrossSection(view)
    }

    pub const fn pass_kind(self) -> RenderPassKind {
        match self {
            Self::Volume { .. } => RenderPassKind::Volume,
            Self::CrossSection(_) => RenderPassKind::Plane,
        }
    }
}

/// One visible logical layer and its validated scientific display intent.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerRenderIntent {
    layer: LogicalLayerKey,
    transfer: LayerTransfer,
    render_state: RenderState,
}

impl LayerRenderIntent {
    pub fn new(layer: LogicalLayerKey, transfer: LayerTransfer, render_state: RenderState) -> Self {
        Self {
            layer,
            transfer,
            render_state,
        }
    }

    pub const fn layer(&self) -> LogicalLayerKey {
        self.layer
    }

    pub const fn transfer(&self) -> &LayerTransfer {
        &self.transfer
    }

    pub const fn render_state(&self) -> &RenderState {
        &self.render_state
    }
}

/// One immutable, bounded request to produce a current frame.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderIntent {
    frame: FrameIdentity,
    resource_identity: DatasetResourceIdentity,
    timepoint: TimeIndex,
    view: RenderViewIntent,
    presentation: PresentationViewport,
    extent: RenderExtent,
    layers: Vec<LayerRenderIntent>,
}

impl RenderIntent {
    pub fn new(
        frame: FrameIdentity,
        resource_identity: DatasetResourceIdentity,
        timepoint: TimeIndex,
        view: RenderViewIntent,
        presentation: PresentationViewport,
        extent: RenderExtent,
        layers: Vec<LayerRenderIntent>,
    ) -> Result<Self, RenderApiError> {
        if layers.is_empty() {
            return Err(RenderApiError::EmptyRenderLayers);
        }
        if layers.len() > MAX_RENDER_LAYERS {
            return Err(RenderApiError::TooManyRenderLayers {
                actual: layers.len(),
                maximum: MAX_RENDER_LAYERS,
            });
        }
        let mut seen = HashSet::with_capacity(layers.len());
        for layer in &layers {
            if !seen.insert(layer.layer()) {
                return Err(RenderApiError::DuplicateRenderLayer {
                    ordinal: layer.layer().ordinal(),
                });
            }
        }
        Ok(Self {
            frame,
            resource_identity,
            timepoint,
            view,
            presentation,
            extent,
            layers,
        })
    }

    pub const fn frame(&self) -> FrameIdentity {
        self.frame
    }

    /// Rebinds an already validated owned intent to a new frame identity.
    /// Frame identity does not participate in layer/view validation, so this
    /// avoids rebuilding and revalidating the small layer cohort merely to
    /// compare a candidate before allocating its final frame number.
    pub fn with_frame(mut self, frame: FrameIdentity) -> Self {
        self.frame = frame;
        self
    }

    pub const fn resource_identity(&self) -> DatasetResourceIdentity {
        self.resource_identity
    }

    pub const fn timepoint(&self) -> TimeIndex {
        self.timepoint
    }

    pub const fn view(&self) -> RenderViewIntent {
        self.view
    }

    pub const fn presentation(&self) -> PresentationViewport {
        self.presentation
    }

    pub const fn extent(&self) -> RenderExtent {
        self.extent
    }

    pub fn layers(&self) -> &[LayerRenderIntent] {
        &self.layers
    }
}

/// How a semantic dataset resource contributes to progressive rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderRequirementRole {
    FirstUsefulFrame,
    Refinement,
    /// Resident navigation guard. It is uploaded and retained but does not
    /// affect current-frame coverage until explicitly promoted.
    Prefetch,
}

/// One semantic dataset resource needed by a render intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderRequirement {
    key: DatasetResourceKey,
    role: RenderRequirementRole,
}

impl RenderRequirement {
    pub const fn new(key: DatasetResourceKey, role: RenderRequirementRole) -> Self {
        Self { key, role }
    }

    pub const fn key(self) -> DatasetResourceKey {
        self.key
    }

    pub const fn role(self) -> RenderRequirementRole {
        self.role
    }
}

/// The bounded, deduplicated semantic resources for one frame identity.
///
/// Input order is preserved so a planner can emit a deterministic traversal;
/// runtime request priority remains owned by the dataset runtime.
struct PreparedResourceBodyData {
    canonical: Arc<[DatasetResourceKey]>,
    ranked: Arc<[DatasetResourceKey]>,
    charge: OnceLock<Arc<dyn CpuByteLease>>,
    host_allocation_bytes: u64,
}

/// One immutable, accounted authority for a planned semantic cohort.
/// Dataset admission, render binding, and backend static preparation share
/// these exact Arcs; no consumer clones the up-to-65,536-key bodies or index.
#[derive(Clone)]
pub struct PreparedResourceBody {
    body: Arc<PreparedResourceBodyData>,
}

impl std::fmt::Debug for PreparedResourceBody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedResourceBody")
            .field("requirements", &self.body.canonical.len())
            .field("host_allocation_bytes", &self.body.host_allocation_bytes)
            .field(
                "charged_bytes",
                &self.body.charge.get().map(|charge| charge.reserved_bytes()),
            )
            .finish()
    }
}

impl PartialEq for PreparedResourceBody {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.body, &other.body)
    }
}

impl Eq for PreparedResourceBody {}

impl PreparedResourceBody {
    pub fn new(
        canonical: Arc<[DatasetResourceKey]>,
        ranked: Arc<[DatasetResourceKey]>,
        charge: Option<Arc<dyn CpuByteLease>>,
    ) -> Result<Self, RenderApiError> {
        if canonical.len() > MAX_RENDER_REQUIREMENTS {
            return Err(RenderApiError::TooManyRenderRequirements {
                actual: canonical.len(),
                maximum: MAX_RENDER_REQUIREMENTS,
            });
        }
        if !canonical.is_sorted()
            || canonical.windows(2).any(|pair| pair[0] == pair[1])
            || canonical.len() != ranked.len()
        {
            return Err(RenderApiError::DuplicateRenderRequirement);
        }
        for key in ranked.iter() {
            if canonical.binary_search(key).is_err() {
                return Err(RenderApiError::DuplicateRenderRequirement);
            }
        }
        let host_allocation_bytes =
            Self::preflight_host_allocation_bytes(canonical.len(), ranked.len())?;
        let prepared = Self {
            body: Arc::new(PreparedResourceBodyData {
                canonical,
                ranked,
                charge: OnceLock::new(),
                host_allocation_bytes,
            }),
        };
        if let Some(charge) = charge {
            prepared.attach_charge(charge)?;
        }
        Ok(prepared)
    }

    pub fn canonical(&self) -> &Arc<[DatasetResourceKey]> {
        &self.body.canonical
    }

    pub fn ranked(&self) -> &Arc<[DatasetResourceKey]> {
        &self.body.ranked
    }

    pub fn host_allocation_bytes(&self) -> u64 {
        self.body.host_allocation_bytes
    }

    pub fn charged_bytes(&self) -> Option<u64> {
        self.body.charge.get().map(|charge| charge.reserved_bytes())
    }

    /// Attaches the one shared ledger lifetime after all worker-prepared
    /// backend artifacts have reported their exact host-byte contribution.
    /// The immutable body identity is preserved.
    pub fn attach_charge(&self, charge: Arc<dyn CpuByteLease>) -> Result<(), RenderApiError> {
        if charge.reserved_bytes() < self.body.host_allocation_bytes {
            return Err(RenderApiError::PreparedRequirementChargeTooSmall);
        }
        if let Some(current) = self.body.charge.get() {
            return if Arc::ptr_eq(current, &charge) {
                Ok(())
            } else {
                Err(RenderApiError::PreparedRequirementChargeAlreadyAttached)
            };
        }
        self.body
            .charge
            .set(charge)
            .map_err(|_| RenderApiError::PreparedRequirementChargeAlreadyAttached)
    }

    pub fn shares_storage_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.body, &other.body)
    }

    pub fn resource_index(&self, key: DatasetResourceKey) -> Option<usize> {
        self.body.canonical.binary_search(&key).ok()
    }

    pub fn preflight_host_allocation_bytes(
        canonical_len: usize,
        ranked_len: usize,
    ) -> Result<u64, RenderApiError> {
        let key_count = canonical_len
            .checked_add(ranked_len)
            .ok_or(RenderApiError::PreparedRequirementHostAllocationOverflow)?;
        let keys = key_count
            .checked_mul(std::mem::size_of::<DatasetResourceKey>())
            .ok_or(RenderApiError::PreparedRequirementHostAllocationOverflow)?;
        let bytes = keys
            .checked_add(std::mem::size_of::<PreparedResourceBodyData>())
            .ok_or(RenderApiError::PreparedRequirementHostAllocationOverflow)?;
        u64::try_from(bytes).map_err(|_| RenderApiError::PreparedRequirementHostAllocationOverflow)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RenderRequirementResources {
    resource_identity: DatasetResourceIdentity,
    timepoint: TimeIndex,
    layers: Box<[LogicalLayerKey]>,
    body: PreparedResourceBody,
    first_useful_words: Arc<[u64]>,
    required_words: Arc<[u64]>,
    total_first_useful: u64,
    total_required: u64,
}

impl RenderRequirementResources {
    fn base_role_at(&self, index: usize) -> RenderRequirementRole {
        if self.first_useful_words[index / 64] & (1_u64 << (index % 64)) != 0 {
            RenderRequirementRole::FirstUsefulFrame
        } else if self.required_words[index / 64] & (1_u64 << (index % 64)) != 0 {
            RenderRequirementRole::Refinement
        } else {
            RenderRequirementRole::Prefetch
        }
    }

    fn role_at(&self, index: usize, prefetch_promoted: bool) -> RenderRequirementRole {
        match self.base_role_at(index) {
            RenderRequirementRole::Prefetch if prefetch_promoted => {
                RenderRequirementRole::Refinement
            }
            role => role,
        }
    }

    fn is_required(&self, index: usize, prefetch_promoted: bool) -> bool {
        prefetch_promoted || self.required_words[index / 64] & (1_u64 << (index % 64)) != 0
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RenderRequirementSet {
    frame: FrameIdentity,
    resources: Arc<RenderRequirementResources>,
    prefetch_promoted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderRequirements {
    set: Arc<RenderRequirementSet>,
}

/// A worker-built semantic body validated independently of a frame/camera.
/// Binding it to a `RenderIntent` touches only the at-most-64 layer list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRenderRequirements {
    resources: Arc<RenderRequirementResources>,
    prefetch_promoted: bool,
}

impl PreparedRenderRequirements {
    pub fn new(
        resource_identity: DatasetResourceIdentity,
        timepoint: TimeIndex,
        layers: Vec<LogicalLayerKey>,
        body: PreparedResourceBody,
        first_useful_prefix_len: usize,
    ) -> Result<Self, RenderApiError> {
        let required_prefix_len = body.ranked().len();
        Self::new_with_required_prefix(
            resource_identity,
            timepoint,
            layers,
            body,
            first_useful_prefix_len,
            required_prefix_len,
        )
    }

    /// Prepares one body whose ranked suffix is a resident-navigation guard.
    /// Guard resources share the canonical body and backend page layout, but
    /// do not affect current-frame completeness until O(1) promotion.
    pub fn new_with_required_prefix(
        resource_identity: DatasetResourceIdentity,
        timepoint: TimeIndex,
        layers: Vec<LogicalLayerKey>,
        body: PreparedResourceBody,
        first_useful_prefix_len: usize,
        required_prefix_len: usize,
    ) -> Result<Self, RenderApiError> {
        if first_useful_prefix_len == 0
            || first_useful_prefix_len > required_prefix_len
            || required_prefix_len > body.canonical().len()
        {
            return Err(RenderApiError::MissingFirstUsefulRequirement);
        }
        if layers.is_empty() {
            return Err(RenderApiError::EmptyRenderLayers);
        }
        if layers.len() > MAX_RENDER_LAYERS {
            return Err(RenderApiError::TooManyRenderLayers {
                actual: layers.len(),
                maximum: MAX_RENDER_LAYERS,
            });
        }
        Self::preflight_host_allocation_bytes(layers.len(), body.canonical().len())?;
        let layer_set = layers.iter().copied().collect::<HashSet<_>>();
        if layer_set.len() != layers.len() {
            let duplicate = layers
                .iter()
                .copied()
                .find(|layer| {
                    layers
                        .iter()
                        .filter(|candidate| **candidate == *layer)
                        .count()
                        > 1
                })
                .expect("a shorter unique set proves a duplicate layer");
            return Err(RenderApiError::DuplicateRenderLayer {
                ordinal: duplicate.ordinal(),
            });
        }
        for key in body.canonical().iter().copied() {
            if key.identity() != resource_identity {
                return Err(RenderApiError::RequirementIdentityMismatch);
            }
            if key.timepoint() != timepoint {
                return Err(RenderApiError::RequirementTimepointMismatch {
                    expected: timepoint.get(),
                    actual: key.timepoint().get(),
                });
            }
            if !layer_set.contains(&key.layer()) {
                return Err(RenderApiError::RequirementLayerNotInIntent {
                    ordinal: key.layer().ordinal(),
                });
            }
        }
        let mut first_useful_words = vec![0_u64; body.canonical().len().div_ceil(64)];
        for key in body.ranked()[..first_useful_prefix_len].iter().copied() {
            let index = body
                .resource_index(key)
                .expect("a prepared ranked key belongs to its canonical body");
            first_useful_words[index / 64] |= 1_u64 << (index % 64);
        }
        let mut required_words = vec![0_u64; body.canonical().len().div_ceil(64)];
        for key in body.ranked()[..required_prefix_len].iter().copied() {
            let index = body
                .resource_index(key)
                .expect("a prepared ranked key belongs to its canonical body");
            required_words[index / 64] |= 1_u64 << (index % 64);
        }
        Ok(Self {
            resources: Arc::new(RenderRequirementResources {
                resource_identity,
                timepoint,
                layers: layers.into(),
                body,
                first_useful_words: first_useful_words.into(),
                required_words: required_words.into(),
                total_first_useful: first_useful_prefix_len as u64,
                total_required: required_prefix_len as u64,
            }),
            prefetch_promoted: false,
        })
    }

    pub fn bind(&self, intent: &RenderIntent) -> Result<RenderRequirements, RenderApiError> {
        validate_prepared_render_binding(&self.resources, intent)?;
        Ok(RenderRequirements {
            set: Arc::new(RenderRequirementSet {
                frame: intent.frame(),
                resources: Arc::clone(&self.resources),
                prefetch_promoted: self.prefetch_promoted,
            }),
        })
    }

    /// Promotes the resident guard to required refinement without touching a
    /// key, bitmap, or backend layout. The next frame remains truthful when a
    /// promoted guard resource has not arrived yet.
    pub fn promote_prefetch(&self) -> Self {
        Self {
            resources: Arc::clone(&self.resources),
            prefetch_promoted: true,
        }
    }

    pub const fn prefetch_promoted(&self) -> bool {
        self.prefetch_promoted
    }

    pub fn required_prefix_len(&self) -> usize {
        if self.prefetch_promoted {
            self.resources.body.ranked().len()
        } else {
            self.resources.total_required as usize
        }
    }

    pub fn prefetch_resource_count(&self) -> usize {
        self.resources
            .body
            .ranked()
            .len()
            .saturating_sub(self.resources.total_required as usize)
    }

    pub fn body(&self) -> &PreparedResourceBody {
        &self.resources.body
    }

    pub fn resource_identity(&self) -> DatasetResourceIdentity {
        self.resources.resource_identity
    }

    pub fn timepoint(&self) -> TimeIndex {
        self.resources.timepoint
    }

    pub fn layers(&self) -> &[LogicalLayerKey] {
        &self.resources.layers
    }

    pub fn first_useful_prefix_len(&self) -> usize {
        self.resources.total_first_useful as usize
    }

    /// Host allocation owned by this render wrapper, excluding the shared
    /// `PreparedResourceBody` reported separately.
    pub fn host_allocation_bytes(&self) -> u64 {
        Self::preflight_host_allocation_bytes(
            self.resources.layers.len(),
            self.resources.body.canonical().len(),
        )
        .expect("a constructed prepared render wrapper has representable host bytes")
    }

    /// Exact retained bytes known before the wrapper allocates its layer body
    /// and compact role bitmaps. The shared resource body is accounted
    /// independently by [`PreparedResourceBody`].
    pub fn preflight_host_allocation_bytes(
        layer_count: usize,
        requirement_count: usize,
    ) -> Result<u64, RenderApiError> {
        let layer_bytes = layer_count
            .checked_mul(std::mem::size_of::<LogicalLayerKey>())
            .ok_or(RenderApiError::PreparedRequirementHostAllocationOverflow)?;
        let first_useful_words = requirement_count
            .checked_add(63)
            .ok_or(RenderApiError::PreparedRequirementHostAllocationOverflow)?
            / 64;
        let role_bitmap_bytes = first_useful_words
            .checked_mul(std::mem::size_of::<u64>())
            .and_then(|bytes| bytes.checked_mul(2))
            .ok_or(RenderApiError::PreparedRequirementHostAllocationOverflow)?;
        let bytes = std::mem::size_of::<RenderRequirementResources>()
            .checked_add(layer_bytes)
            .and_then(|bytes| bytes.checked_add(role_bitmap_bytes))
            .ok_or(RenderApiError::PreparedRequirementHostAllocationOverflow)?;
        u64::try_from(bytes).map_err(|_| RenderApiError::PreparedRequirementHostAllocationOverflow)
    }

    pub fn shares_body_with(&self, other: &Self) -> bool {
        self.resources
            .body
            .shares_storage_with(&other.resources.body)
    }
}

fn validate_prepared_render_binding(
    resources: &RenderRequirementResources,
    intent: &RenderIntent,
) -> Result<(), RenderApiError> {
    if resources.resource_identity != intent.resource_identity() {
        return Err(RenderApiError::RequirementIdentityMismatch);
    }
    if resources.timepoint != intent.timepoint() {
        return Err(RenderApiError::RequirementTimepointMismatch {
            expected: intent.timepoint().get(),
            actual: resources.timepoint.get(),
        });
    }
    let intent_layers = intent
        .layers()
        .iter()
        .map(LayerRenderIntent::layer)
        .collect::<HashSet<_>>();
    if resources.layers.len() != intent_layers.len() {
        let missing = resources
            .layers
            .iter()
            .find(|layer| !intent_layers.contains(layer))
            .copied()
            .unwrap_or_else(|| resources.layers[0]);
        return Err(RenderApiError::RequirementLayerNotInIntent {
            ordinal: missing.ordinal(),
        });
    }
    if let Some(missing) = resources
        .layers
        .iter()
        .find(|layer| !intent_layers.contains(layer))
    {
        return Err(RenderApiError::RequirementLayerNotInIntent {
            ordinal: missing.ordinal(),
        });
    }
    Ok(())
}

impl RenderRequirements {
    pub fn new(
        intent: &RenderIntent,
        resources: Vec<RenderRequirement>,
    ) -> Result<Self, RenderApiError> {
        if resources.is_empty() {
            return Err(RenderApiError::EmptyRenderRequirements);
        }
        if resources.len() > MAX_RENDER_REQUIREMENTS {
            return Err(RenderApiError::TooManyRenderRequirements {
                actual: resources.len(),
                maximum: MAX_RENDER_REQUIREMENTS,
            });
        }
        let total_first_useful = resources
            .iter()
            .filter(|requirement| requirement.role() == RenderRequirementRole::FirstUsefulFrame)
            .count();
        if total_first_useful == 0 {
            return Err(RenderApiError::MissingFirstUsefulRequirement);
        }
        let mut canonical = resources
            .iter()
            .map(|requirement| requirement.key())
            .collect::<Vec<_>>();
        canonical.sort_unstable();
        canonical.dedup();
        if canonical.len() != resources.len() {
            return Err(RenderApiError::DuplicateRenderRequirement);
        }
        let ranked = resources
            .iter()
            .filter(|requirement| requirement.role() == RenderRequirementRole::FirstUsefulFrame)
            .chain(
                resources
                    .iter()
                    .filter(|requirement| requirement.role() == RenderRequirementRole::Refinement),
            )
            .chain(
                resources
                    .iter()
                    .filter(|requirement| requirement.role() == RenderRequirementRole::Prefetch),
            )
            .map(|requirement| requirement.key())
            .collect::<Vec<_>>();
        let body = PreparedResourceBody::new(canonical.into(), ranked.into(), None)?;
        let total_required = resources
            .iter()
            .filter(|requirement| requirement.role() != RenderRequirementRole::Prefetch)
            .count();
        PreparedRenderRequirements::new_with_required_prefix(
            intent.resource_identity(),
            intent.timepoint(),
            intent
                .layers()
                .iter()
                .map(LayerRenderIntent::layer)
                .collect(),
            body,
            total_first_useful,
            total_required,
        )?
        .bind(intent)
    }

    /// Rebinds an immutable semantic requirement body to a new camera/frame
    /// intent in O(layer-count) time. This is the camera-navigation path: the
    /// up-to-65,536 resource records remain shared and are not cloned or
    /// revalidated.
    pub fn rebind(&self, intent: &RenderIntent) -> Result<Self, RenderApiError> {
        let resources = &self.set.resources;
        validate_prepared_render_binding(resources, intent)?;
        Ok(Self {
            set: Arc::new(RenderRequirementSet {
                frame: intent.frame(),
                resources: Arc::clone(resources),
                prefetch_promoted: self.set.prefetch_promoted,
            }),
        })
    }

    /// True when two frame-bound handles share the exact validated ordered
    /// resource body. This comparison is O(1).
    pub fn shares_resources_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.set.resources, &other.set.resources)
    }

    pub fn prefetch_promoted(&self) -> bool {
        self.set.prefetch_promoted
    }

    pub fn is_required_resource(&self, key: DatasetResourceKey) -> bool {
        self.resource_index(key).is_some_and(|index| {
            self.set
                .resources
                .is_required(index, self.set.prefetch_promoted)
        })
    }

    pub fn frame(&self) -> FrameIdentity {
        self.set.frame
    }

    pub fn resources(&self) -> RenderRequirementIter<'_> {
        RenderRequirementIter {
            resources: &self.set.resources,
            prefetch_promoted: self.set.prefetch_promoted,
            index: 0,
        }
    }

    pub fn resource_keys(&self) -> &[DatasetResourceKey] {
        self.set.resources.body.canonical().as_ref()
    }

    pub fn len(&self) -> usize {
        self.set.resources.body.canonical().len()
    }

    pub fn is_empty(&self) -> bool {
        self.set.resources.body.canonical().is_empty()
    }

    pub fn prepared_body(&self) -> &PreparedResourceBody {
        &self.set.resources.body
    }

    pub fn contains_resource(&self, key: DatasetResourceKey) -> bool {
        self.set.resources.body.resource_index(key).is_some()
    }

    pub fn resource_index(&self, key: DatasetResourceKey) -> Option<usize> {
        self.set.resources.body.resource_index(key)
    }

    pub fn requirement(&self, index: usize) -> Option<RenderRequirement> {
        let key = self.resource_keys().get(index).copied()?;
        Some(RenderRequirement::new(
            key,
            self.set
                .resources
                .role_at(index, self.set.prefetch_promoted),
        ))
    }
}

pub struct RenderRequirementIter<'a> {
    resources: &'a RenderRequirementResources,
    prefetch_promoted: bool,
    index: usize,
}

impl Iterator for RenderRequirementIter<'_> {
    type Item = RenderRequirement;

    fn next(&mut self) -> Option<Self::Item> {
        let key = self.resources.body.canonical().get(self.index).copied()?;
        let role = self.resources.role_at(self.index, self.prefetch_promoted);
        self.index += 1;
        Some(RenderRequirement::new(key, role))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.resources.body.canonical().len() - self.index;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for RenderRequirementIter<'_> {}

/// Requirement-bound availability for one progressive frame.
///
/// Coverage can only be constructed by matching semantic resource keys back
/// to one validated requirement set. It separately preserves the
/// first-useful and refinement roles; it is not pixel coverage and cannot
/// classify uncovered pixels as scientifically empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameCoverage {
    frame: FrameIdentity,
    requirements: Arc<RenderRequirementResources>,
    available_words: Arc<[u64]>,
    available_first_useful: u64,
    total_first_useful: u64,
    /// Base required refinement, excluding the dormant prefetch suffix.
    available_refinement: u64,
    total_refinement: u64,
    available_prefetch: u64,
    total_prefetch: u64,
    prefetch_promoted: bool,
}

impl FrameCoverage {
    /// Creates an empty bitmap for an already validated requirement body.
    pub fn empty(requirements: &RenderRequirements) -> Self {
        let resources = &requirements.set.resources;
        Self {
            frame: requirements.frame(),
            requirements: Arc::clone(resources),
            available_words: vec![0_u64; resources.body.canonical().len().div_ceil(64)].into(),
            available_first_useful: 0,
            total_first_useful: resources.total_first_useful,
            available_refinement: 0,
            total_refinement: resources.total_required - resources.total_first_useful,
            available_prefetch: 0,
            total_prefetch: resources.body.canonical().len() as u64 - resources.total_required,
            prefetch_promoted: requirements.set.prefetch_promoted,
        }
    }

    pub fn from_available(
        requirements: &RenderRequirements,
        available: &[DatasetResourceKey],
    ) -> Result<Self, RenderApiError> {
        if available.len() > requirements.resources().len() {
            return Err(RenderApiError::TooManyCoveredResources {
                actual: available.len(),
                maximum: requirements.resources().len(),
            });
        }
        let mut seen = HashSet::with_capacity(available.len());
        let mut available_words = vec![0_u64; requirements.resources().len().div_ceil(64)];
        let mut available_first_useful = 0_u64;
        let mut available_refinement = 0_u64;
        let mut available_prefetch = 0_u64;
        for key in available {
            if !seen.insert(*key) {
                return Err(RenderApiError::DuplicateCoveredResource);
            }
            match requirements.resource_index(*key) {
                Some(index)
                    if requirements.set.resources.base_role_at(index)
                        == RenderRequirementRole::FirstUsefulFrame =>
                {
                    available_words[index / 64] |= 1_u64 << (index % 64);
                    available_first_useful += 1;
                }
                Some(index)
                    if requirements.set.resources.base_role_at(index)
                        == RenderRequirementRole::Refinement =>
                {
                    available_words[index / 64] |= 1_u64 << (index % 64);
                    available_refinement += 1;
                }
                Some(index) => {
                    available_words[index / 64] |= 1_u64 << (index % 64);
                    available_prefetch += 1;
                }
                None => return Err(RenderApiError::CoveredResourceNotRequired),
            }
        }

        let total_first_useful = requirements.set.resources.total_first_useful;
        let total_refinement = requirements.set.resources.total_required - total_first_useful;
        let total_prefetch =
            requirements.resources().len() as u64 - requirements.set.resources.total_required;
        Ok(Self {
            frame: requirements.frame(),
            requirements: Arc::clone(&requirements.set.resources),
            available_words: available_words.into(),
            available_first_useful,
            total_first_useful,
            available_refinement,
            total_refinement,
            available_prefetch,
            total_prefetch,
            prefetch_promoted: requirements.set.prefetch_promoted,
        })
    }

    /// Applies a bounded list of residency additions/removals to a retained
    /// bitmap. The bitmap clone is N/64 words and each changed key is an O(1)
    /// lookup, so a complete progressive stream is O(N) rather than O(N²).
    pub fn with_availability_changes(
        &self,
        requirements: &RenderRequirements,
        changes: &[(DatasetResourceKey, bool)],
    ) -> Result<Self, RenderApiError> {
        if !Arc::ptr_eq(&self.requirements, &requirements.set.resources) {
            return Err(RenderApiError::CoveredResourceNotRequired);
        }
        let mut words = self.available_words.to_vec();
        let mut available_first_useful = self.available_first_useful;
        let mut available_refinement = self.available_refinement;
        let mut available_prefetch = self.available_prefetch;
        for (key, available) in changes {
            let Some(index) = self.requirements.body.resource_index(*key) else {
                return Err(RenderApiError::CoveredResourceNotRequired);
            };
            let mask = 1_u64 << (index % 64);
            let word = &mut words[index / 64];
            let was_available = *word & mask != 0;
            if was_available == *available {
                continue;
            }
            if *available {
                *word |= mask;
            } else {
                *word &= !mask;
            }
            let count = match self.requirements.base_role_at(index) {
                RenderRequirementRole::FirstUsefulFrame => &mut available_first_useful,
                RenderRequirementRole::Refinement => &mut available_refinement,
                RenderRequirementRole::Prefetch => &mut available_prefetch,
            };
            if *available {
                *count += 1;
            } else {
                *count -= 1;
            }
        }
        Ok(Self {
            frame: requirements.frame(),
            requirements: Arc::clone(&self.requirements),
            available_words: words.into(),
            available_first_useful,
            total_first_useful: self.total_first_useful,
            available_refinement,
            total_refinement: self.total_refinement,
            available_prefetch,
            total_prefetch: self.total_prefetch,
            prefetch_promoted: requirements.set.prefetch_promoted,
        })
    }

    pub fn frame(&self) -> FrameIdentity {
        self.frame
    }

    pub const fn available_requirements(&self) -> u64 {
        self.available_first_useful
            + self.available_refinement
            + if self.prefetch_promoted {
                self.available_prefetch
            } else {
                0
            }
    }

    pub const fn total_requirements(&self) -> u64 {
        self.total_first_useful
            + self.total_refinement
            + if self.prefetch_promoted {
                self.total_prefetch
            } else {
                0
            }
    }

    pub const fn available_first_useful(&self) -> u64 {
        self.available_first_useful
    }

    pub const fn total_first_useful(&self) -> u64 {
        self.total_first_useful
    }

    pub const fn available_refinement(&self) -> u64 {
        self.available_refinement
            + if self.prefetch_promoted {
                self.available_prefetch
            } else {
                0
            }
    }

    pub const fn total_refinement(&self) -> u64 {
        self.total_refinement
            + if self.prefetch_promoted {
                self.total_prefetch
            } else {
                0
            }
    }

    pub const fn available_prefetch(&self) -> u64 {
        self.available_prefetch
    }

    pub const fn total_prefetch(&self) -> u64 {
        self.total_prefetch
    }

    pub const fn prefetch_promoted(&self) -> bool {
        self.prefetch_promoted
    }

    pub const fn is_first_useful(&self) -> bool {
        self.available_first_useful == self.total_first_useful
    }

    pub const fn is_full(&self) -> bool {
        self.is_first_useful() && self.available_refinement() == self.total_refinement()
    }

    pub fn fraction(&self) -> f64 {
        self.available_requirements() as f64 / self.total_requirements() as f64
    }

    fn can_replace(&self, current: &Self) -> bool {
        self.frame == current.frame
            && Arc::ptr_eq(&self.requirements, &current.requirements)
            && self.prefetch_promoted == current.prefetch_promoted
            && self
                .available_words
                .iter()
                .zip(current.available_words.iter())
                .all(|(next, previous)| next & previous == *previous)
    }

    pub fn rebind(&self, requirements: &RenderRequirements) -> Result<Self, RenderApiError> {
        if !Arc::ptr_eq(&self.requirements, &requirements.set.resources) {
            return Err(RenderApiError::CoveredResourceNotRequired);
        }
        Ok(Self {
            frame: requirements.frame(),
            requirements: Arc::clone(&self.requirements),
            available_words: Arc::clone(&self.available_words),
            available_first_useful: self.available_first_useful,
            total_first_useful: self.total_first_useful,
            available_refinement: self.available_refinement,
            total_refinement: self.total_refinement,
            available_prefetch: self.available_prefetch,
            total_prefetch: self.total_prefetch,
            prefetch_promoted: requirements.set.prefetch_promoted,
        })
    }
}

/// Whether the presented frame is still progressive, complete with an
/// explicit limitation, or exact for its current intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameCompleteness {
    Progressive,
    Complete,
    Exact,
}

/// Why a frame cannot yet or cannot ever be exact for its current intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameLimitation {
    CoarserScale,
    BudgetLimited,
    CapacityLimited,
    MissingResources,
}

/// Truthful progressive status for one presented frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameProgress {
    coverage: FrameCoverage,
    completeness: FrameCompleteness,
    limitation: Option<FrameLimitation>,
}

impl FrameProgress {
    pub fn new(
        coverage: FrameCoverage,
        completeness: FrameCompleteness,
        limitation: Option<FrameLimitation>,
    ) -> Result<Self, RenderApiError> {
        let valid = coverage.is_first_useful()
            && match completeness {
                FrameCompleteness::Progressive => !coverage.is_full(),
                FrameCompleteness::Complete => coverage.is_full() && limitation.is_some(),
                FrameCompleteness::Exact => coverage.is_full() && limitation.is_none(),
            };
        if !valid {
            return Err(RenderApiError::InvalidFrameProgress);
        }
        Ok(Self {
            coverage,
            completeness,
            limitation,
        })
    }

    pub const fn coverage(&self) -> &FrameCoverage {
        &self.coverage
    }

    pub const fn completeness(&self) -> FrameCompleteness {
        self.completeness
    }

    pub const fn limitation(&self) -> Option<FrameLimitation> {
        self.limitation
    }

    /// Reuses an unchanged coverage bitmap for a new frame-bound requirement
    /// handle that shares the same validated semantic resource body.
    pub fn rebind(&self, requirements: &RenderRequirements) -> Result<Self, RenderApiError> {
        Self::new(
            self.coverage.rebind(requirements)?,
            self.completeness,
            self.limitation,
        )
    }

    fn can_replace(&self, current: &Self) -> bool {
        self.coverage.can_replace(&current.coverage)
            && completeness_rank(self.completeness) >= completeness_rank(current.completeness)
    }
}

const fn completeness_rank(completeness: FrameCompleteness) -> u8 {
    match completeness {
        FrameCompleteness::Progressive => 0,
        FrameCompleteness::Complete => 1,
        FrameCompleteness::Exact => 2,
    }
}

/// The sole categories to which large production GPU allocations are charged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuLedgerCategory {
    PayloadResidency,
    TransferStaging,
    DisplayTarget,
    PageTable,
    Scratch,
}

/// A framework-neutral identifier for a renderer-owned presentation resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PresentationToken(u64);

impl PresentationToken {
    pub fn new(value: u64) -> Result<Self, RenderApiError> {
        if value == 0 {
            return Err(RenderApiError::InvalidPresentationToken);
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The current renderer-owned frame facts safe to carry in an application
/// snapshot. The token never transfers ownership of the backend resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentedFrame {
    token: PresentationToken,
    extent: RenderExtent,
    progress: FrameProgress,
}

impl PresentedFrame {
    pub const fn new(
        token: PresentationToken,
        extent: RenderExtent,
        progress: FrameProgress,
    ) -> Self {
        Self {
            token,
            extent,
            progress,
        }
    }

    pub const fn token(&self) -> PresentationToken {
        self.token
    }

    pub fn frame(&self) -> FrameIdentity {
        self.progress.coverage().frame()
    }

    pub const fn extent(&self) -> RenderExtent {
        self.extent
    }

    pub const fn progress(&self) -> &FrameProgress {
        &self.progress
    }
}

/// Mode-specific scientific selection made by one volume-pick ray.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VolumePickPolicy {
    /// The nearest sample that first crosses the ISO display threshold.
    FirstThresholdHit,
    /// The nearest sample among equal raw-intensity maxima.
    MipArgmax,
    /// The nearest sample among equal maxima of
    /// `transmittance_before * sample_alpha`.
    MaximumOpacityContribution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VolumePickHitKind {
    Voxel,
    InterpolatedSample,
    Empty,
}

/// Completeness of the scientific volume result, independent of asynchronous
/// request status. A result can contain a hit and still be incomplete when a
/// missing page occurred earlier along the ray.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VolumePickCompleteness {
    Exact,
    Approximate,
    Incomplete,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VolumePickValue {
    IntensityU8(u8),
    IntensityU16(u16),
    IntensityF32(f32),
}

/// Opaque monotonic identity for one bounded asynchronous volume pick.
/// Presentation and frame facts make accidental cross-target polling visible
/// without transferring ownership of any backend resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VolumePickTicket {
    sequence: u64,
    presentation: PresentationToken,
    frame: FrameIdentity,
}

impl VolumePickTicket {
    pub fn new(
        sequence: u64,
        presentation: PresentationToken,
        frame: FrameIdentity,
    ) -> Result<Self, RenderApiError> {
        if sequence == 0 {
            return Err(RenderApiError::InvalidVolumePickTicket);
        }
        Ok(Self {
            sequence,
            presentation,
            frame,
        })
    }

    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    pub const fn presentation(self) -> PresentationToken {
        self.presentation
    }

    pub const fn frame(self) -> FrameIdentity {
        self.frame
    }
}

/// One bounded pick bound to the exact presented resource, frame, extent,
/// timepoint, and active layer from which it was requested.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolumePickQuery {
    token: PresentationToken,
    frame: FrameIdentity,
    extent: RenderExtent,
    timepoint: TimeIndex,
    layer: LogicalLayerKey,
    render_pixel: [f64; 2],
    policy: VolumePickPolicy,
}

impl VolumePickQuery {
    pub fn new(
        presented: &PresentedFrame,
        timepoint: TimeIndex,
        layer: LogicalLayerKey,
        render_pixel: [f64; 2],
        policy: VolumePickPolicy,
    ) -> Result<Self, RenderApiError> {
        if !render_pixel.into_iter().all(f64::is_finite) {
            return Err(RenderApiError::NonFiniteRenderPixel);
        }
        let extent = presented.extent();
        if render_pixel[0] < 0.0
            || render_pixel[1] < 0.0
            || render_pixel[0] >= f64::from(extent.width_pixels())
            || render_pixel[1] >= f64::from(extent.height_pixels())
        {
            return Err(RenderApiError::PickPixelOutsideExtent);
        }
        Ok(Self {
            token: presented.token(),
            frame: presented.frame(),
            extent,
            timepoint,
            layer,
            render_pixel: render_pixel.map(canonical_zero),
            policy,
        })
    }

    pub const fn token(self) -> PresentationToken {
        self.token
    }

    pub const fn frame(self) -> FrameIdentity {
        self.frame
    }

    pub const fn extent(self) -> RenderExtent {
        self.extent
    }

    pub const fn timepoint(self) -> TimeIndex {
        self.timepoint
    }

    pub const fn layer(self) -> LogicalLayerKey {
        self.layer
    }

    pub const fn render_pixel(self) -> [f64; 2] {
        self.render_pixel
    }

    pub const fn policy(self) -> VolumePickPolicy {
        self.policy
    }
}

/// Small asynchronous readback payload. It contains no resource lease,
/// framebuffer copy, backend handle, or unbounded collection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolumePickResult {
    query: VolumePickQuery,
    kind: VolumePickHitKind,
    world_position: Option<WorldPoint3>,
    value: Option<VolumePickValue>,
    ray_distance_world: Option<f64>,
    completeness: VolumePickCompleteness,
}

impl VolumePickResult {
    pub const fn empty(query: VolumePickQuery, completeness: VolumePickCompleteness) -> Self {
        Self {
            query,
            kind: VolumePickHitKind::Empty,
            world_position: None,
            value: None,
            ray_distance_world: None,
            completeness,
        }
    }

    pub fn voxel(
        query: VolumePickQuery,
        world_position: WorldPoint3,
        value: VolumePickValue,
        ray_distance_world: f64,
        completeness: VolumePickCompleteness,
    ) -> Result<Self, RenderApiError> {
        Self::sample(
            query,
            VolumePickHitKind::Voxel,
            world_position,
            value,
            ray_distance_world,
            completeness,
        )
    }

    pub fn interpolated_sample(
        query: VolumePickQuery,
        world_position: WorldPoint3,
        value: VolumePickValue,
        ray_distance_world: f64,
        completeness: VolumePickCompleteness,
    ) -> Result<Self, RenderApiError> {
        Self::sample(
            query,
            VolumePickHitKind::InterpolatedSample,
            world_position,
            value,
            ray_distance_world,
            completeness,
        )
    }

    fn sample(
        query: VolumePickQuery,
        kind: VolumePickHitKind,
        world_position: WorldPoint3,
        value: VolumePickValue,
        ray_distance_world: f64,
        completeness: VolumePickCompleteness,
    ) -> Result<Self, RenderApiError> {
        let value_is_finite = match value {
            VolumePickValue::IntensityF32(value) => value.is_finite(),
            VolumePickValue::IntensityU8(_) | VolumePickValue::IntensityU16(_) => true,
        };
        if !value_is_finite || !ray_distance_world.is_finite() || ray_distance_world < 0.0 {
            return Err(RenderApiError::InvalidVolumePickResult);
        }
        Ok(Self {
            query,
            kind,
            world_position: Some(world_position),
            value: Some(value),
            ray_distance_world: Some(canonical_zero(ray_distance_world)),
            completeness,
        })
    }

    pub const fn query(self) -> VolumePickQuery {
        self.query
    }

    pub const fn kind(self) -> VolumePickHitKind {
        self.kind
    }

    pub const fn world_position(self) -> Option<WorldPoint3> {
        self.world_position
    }

    pub const fn value(self) -> Option<VolumePickValue> {
        self.value
    }

    pub const fn ray_distance_world(self) -> Option<f64> {
        self.ray_distance_world
    }

    pub const fn completeness(self) -> VolumePickCompleteness {
        self.completeness
    }
}

/// Registers one opaque presentation resource retained by the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PresentationRegistration {
    token: PresentationToken,
    extent: RenderExtent,
}

impl PresentationRegistration {
    pub const fn new(token: PresentationToken, extent: RenderExtent) -> Self {
        Self { token, extent }
    }

    pub const fn token(self) -> PresentationToken {
        self.token
    }

    pub const fn extent(self) -> RenderExtent {
        self.extent
    }
}

/// Publishes a current frame for an already registered presentation token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentationUpdate(PresentedFrame);

impl PresentationUpdate {
    pub const fn new(frame: PresentedFrame) -> Self {
        Self(frame)
    }

    pub const fn frame(&self) -> &PresentedFrame {
        &self.0
    }

    pub const fn token(&self) -> PresentationToken {
        self.0.token()
    }

    fn into_frame(self) -> PresentedFrame {
        self.0
    }
}

/// Retires the renderer-owned resource associated with an opaque token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PresentationRetirement(PresentationToken);

impl PresentationRetirement {
    pub const fn new(token: PresentationToken) -> Self {
        Self(token)
    }

    pub const fn token(self) -> PresentationToken {
        self.0
    }
}

/// A UI-originated request to paint a registered token in logical points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresentationPaintRequest {
    token: PresentationToken,
    viewport: PresentationViewport,
}

impl PresentationPaintRequest {
    pub const fn new(token: PresentationToken, viewport: PresentationViewport) -> Self {
        Self { token, viewport }
    }

    pub const fn token(self) -> PresentationToken {
        self.token
    }

    pub const fn viewport(self) -> PresentationViewport {
        self.viewport
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegisteredPresentation {
    extent: RenderExtent,
    frame: Option<PresentedFrame>,
}

/// Backend-neutral lifecycle authority for opaque presentation tokens.
///
/// This registry retains only validated scalar metadata. A render backend
/// remains the sole owner of textures and other presentation resources and
/// applies the same accepted register/update/retire operations to them.
#[derive(Debug, Default)]
pub struct PresentationRegistry {
    entries: BTreeMap<PresentationToken, RegisteredPresentation>,
}

impl PresentationRegistry {
    pub const fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    pub fn register(&mut self, registration: PresentationRegistration) -> Result<(), RenderFault> {
        if self.entries.contains_key(&registration.token()) {
            return Err(RenderFault::PresentationAlreadyRegistered {
                token: registration.token(),
            });
        }
        if self.entries.len() >= MAX_PRESENTATION_TARGETS {
            return Err(RenderFault::PresentationCapacityExceeded {
                maximum: MAX_PRESENTATION_TARGETS,
            });
        }
        self.entries.insert(
            registration.token(),
            RegisteredPresentation {
                extent: registration.extent(),
                frame: None,
            },
        );
        Ok(())
    }

    pub fn update(&mut self, update: PresentationUpdate) -> Result<(), RenderFault> {
        let token = update.token();
        let entry = self
            .entries
            .get_mut(&token)
            .ok_or(RenderFault::PresentationNotRegistered { token })?;
        let next = update.into_frame();
        if let Some(current) = entry.frame.as_ref() {
            if next.frame() < current.frame() {
                return Err(RenderFault::StaleFrame {
                    actual: next.frame(),
                    current: current.frame(),
                });
            }
            if next.frame() == current.frame() && !next.progress().can_replace(current.progress()) {
                return Err(RenderFault::FrameProgressRegressed {
                    frame: next.frame(),
                });
            }
        }
        entry.extent = next.extent();
        entry.frame = Some(next);
        Ok(())
    }

    pub fn retire(&mut self, retirement: PresentationRetirement) -> Result<(), RenderFault> {
        let token = retirement.token();
        self.entries
            .remove(&token)
            .map(|_| ())
            .ok_or(RenderFault::PresentationNotRegistered { token })
    }

    /// Resolves the current metadata for a paint request without exposing or
    /// transferring the renderer-owned presentation resource.
    pub fn resolve_paint(
        &self,
        request: PresentationPaintRequest,
    ) -> Result<Option<PresentedFrame>, RenderFault> {
        self.entries
            .get(&request.token())
            .map(|entry| entry.frame.clone())
            .ok_or(RenderFault::PresentationNotRegistered {
                token: request.token(),
            })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Stable, actionable render failures with no backend strings or private paths.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RenderFault {
    #[error("no qualifying GPU device is available")]
    DeviceUnavailable,
    #[error("the active GPU device was lost")]
    DeviceLost,
    #[error(
        "GPU capacity in {category:?} cannot satisfy {requested_bytes} bytes with {available_bytes} bytes available"
    )]
    CapacityExceeded {
        category: GpuLedgerCategory,
        requested_bytes: u64,
        available_bytes: u64,
    },
    #[error("a required semantic dataset resource is unavailable")]
    ResourceUnavailable { key: DatasetResourceKey },
    #[error("presentation token {token:?} is already registered")]
    PresentationAlreadyRegistered { token: PresentationToken },
    #[error("presentation registry reached its limit of {maximum} targets")]
    PresentationCapacityExceeded { maximum: usize },
    #[error("presentation token {token:?} is not registered")]
    PresentationNotRegistered { token: PresentationToken },
    #[error("render result {actual:?} is stale; the current frame is {current:?}")]
    StaleFrame {
        actual: FrameIdentity,
        current: FrameIdentity,
    },
    #[error("progress for current frame {frame:?} regressed")]
    FrameProgressRegressed { frame: FrameIdentity },
    #[error("the render runtime is shutting down")]
    ShuttingDown,
}

/// The logical presentation size in UI-independent screen points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresentationViewport {
    width_points: f64,
    height_points: f64,
}

impl PresentationViewport {
    const fn new_unchecked(width_points: f64, height_points: f64) -> Self {
        Self {
            width_points,
            height_points,
        }
    }

    pub fn new(width_points: f64, height_points: f64) -> Result<Self, RenderApiError> {
        if !is_finite_positive(width_points) || !is_finite_positive(height_points) {
            return Err(RenderApiError::InvalidPresentationViewport);
        }
        Ok(Self::new_unchecked(width_points, height_points))
    }

    pub const fn width_points(self) -> f64 {
        self.width_points
    }

    pub const fn height_points(self) -> f64 {
        self.height_points
    }
}

/// Orthonormal world-space axes derived from a canonical camera orientation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraAxes {
    forward: [f64; 3],
    right: [f64; 3],
    up: [f64; 3],
}

impl CameraAxes {
    pub const fn forward(self) -> [f64; 3] {
        self.forward
    }

    pub const fn right(self) -> [f64; 3] {
        self.right
    }

    pub const fn up(self) -> [f64; 3] {
        self.up
    }
}

/// A finite world-space ray with a unit-length direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewRay {
    origin: WorldPoint3,
    direction: [f64; 3],
}

/// A visible world point projected into camera-centered presentation points.
///
/// Positive `screen_x_points` points along camera right and positive
/// `screen_y_points` points along camera up. `depth_world` is the positive
/// distance along camera forward from the eye plane. Points on or behind that
/// plane are not projectable and are reported as `None`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProjectedWorldPoint {
    screen_x_points: f64,
    screen_y_points: f64,
    depth_world: f64,
}

impl ProjectedWorldPoint {
    pub const fn screen_x_points(self) -> f64 {
        self.screen_x_points
    }

    pub const fn screen_y_points(self) -> f64 {
        self.screen_y_points
    }

    pub const fn depth_world(self) -> f64 {
        self.depth_world
    }
}

impl ViewRay {
    pub const fn origin(self) -> WorldPoint3 {
        self.origin
    }

    pub const fn direction(self) -> [f64; 3] {
        self.direction
    }
}

/// Operational projection facts derived from one canonical durable view.
///
/// The canonical `CameraView` remains the authority. This value only combines
/// it with the current presentation extent and provides deterministic math.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraFrame {
    view: CameraView,
    presentation: PresentationViewport,
    axes: CameraAxes,
    eye: WorldPoint3,
}

impl CameraFrame {
    pub fn new(
        view: CameraView,
        presentation: PresentationViewport,
    ) -> Result<Self, RenderApiError> {
        let axes = axes_from_orientation(view.orientation())?;
        let target = Vec3::from_array(view.target().components());
        let eye = target.checked_sub(
            Vec3::from_array(axes.forward).checked_mul(view.perspective_view_distance_world())?,
        )?;
        Ok(Self {
            view,
            presentation,
            axes,
            eye: eye.to_world_point()?,
        })
    }

    pub const fn view(self) -> CameraView {
        self.view
    }

    pub const fn presentation(self) -> PresentationViewport {
        self.presentation
    }

    pub const fn axes(self) -> CameraAxes {
        self.axes
    }

    pub const fn eye(self) -> WorldPoint3 {
        self.eye
    }

    pub fn ray_for_screen_point(
        self,
        screen_x_points: f64,
        screen_y_points: f64,
    ) -> Result<ViewRay, RenderApiError> {
        if !screen_x_points.is_finite() || !screen_y_points.is_finite() {
            return Err(RenderApiError::NonFiniteScreenPoint);
        }

        let forward = Vec3::from_array(self.axes.forward);
        let right = Vec3::from_array(self.axes.right);
        let up = Vec3::from_array(self.axes.up);
        match self.view.projection() {
            Projection::Perspective => {
                let focal_length = self.view.perspective_focal_length_screen_points();
                let direction = forward
                    .checked_add(right.checked_mul(screen_x_points / focal_length)?)?
                    .checked_add(up.checked_mul(screen_y_points / focal_length)?)?
                    .normalized()?;
                Ok(ViewRay {
                    origin: self.eye,
                    direction: direction.0,
                })
            }
            Projection::Orthographic => {
                let scale = self.view.orthographic_world_per_screen_point();
                let origin = Vec3::from_array(self.eye.components())
                    .checked_add(right.checked_mul(screen_x_points * scale)?)?
                    .checked_add(up.checked_mul(screen_y_points * scale)?)?
                    .to_world_point()?;
                Ok(ViewRay {
                    origin,
                    direction: forward.0,
                })
            }
        }
    }

    /// Maps a physical render pixel center into presentation points before
    /// deriving its ray. Pixel coordinates may be outside the render extent so
    /// callers can deliberately evaluate border samples; they must be finite.
    pub fn ray_for_render_pixel(
        self,
        pixel_x: f64,
        pixel_y: f64,
        render_width: u32,
        render_height: u32,
    ) -> Result<ViewRay, RenderApiError> {
        if render_width == 0 || render_height == 0 {
            return Err(RenderApiError::InvalidRenderExtent);
        }
        if !pixel_x.is_finite() || !pixel_y.is_finite() {
            return Err(RenderApiError::NonFiniteRenderPixel);
        }
        let screen_x_points =
            (((pixel_x + 0.5) / f64::from(render_width)) - 0.5) * self.presentation.width_points;
        let screen_y_points =
            (0.5 - ((pixel_y + 0.5) / f64::from(render_height))) * self.presentation.height_points;
        if !screen_x_points.is_finite() || !screen_y_points.is_finite() {
            return Err(RenderApiError::CameraMathNotFinite);
        }
        self.ray_for_screen_point(screen_x_points, screen_y_points)
    }

    /// Projects a finite world point into camera-centered presentation points.
    ///
    /// The returned point is deliberately not clipped to the presentation
    /// rectangle. Overlay owners can let their UI painter clip line segments
    /// while retaining correct geometry at the viewport boundary.
    pub fn project_world_point(
        self,
        world: WorldPoint3,
    ) -> Result<Option<ProjectedWorldPoint>, RenderApiError> {
        let relative = Vec3::from_array(world.components())
            .checked_sub(Vec3::from_array(self.eye.components()))?;
        let forward = Vec3::from_array(self.axes.forward);
        let right = Vec3::from_array(self.axes.right);
        let up = Vec3::from_array(self.axes.up);
        let depth_world = relative.dot(forward)?;
        if depth_world <= 0.0 {
            return Ok(None);
        }

        let right_world = relative.dot(right)?;
        let up_world = relative.dot(up)?;
        let (screen_x_points, screen_y_points) = match self.view.projection() {
            Projection::Perspective => {
                let scale = self.view.perspective_focal_length_screen_points() / depth_world;
                (
                    checked_scalar(right_world * scale)?,
                    checked_scalar(up_world * scale)?,
                )
            }
            Projection::Orthographic => {
                let scale = self.view.orthographic_world_per_screen_point();
                (
                    checked_scalar(right_world / scale)?,
                    checked_scalar(up_world / scale)?,
                )
            }
        };
        Ok(Some(ProjectedWorldPoint {
            screen_x_points,
            screen_y_points,
            depth_world: checked_scalar(depth_world)?,
        }))
    }

    pub fn orthographic_world_span_width(self) -> Result<f64, RenderApiError> {
        checked_scalar(
            self.presentation.width_points * self.view.orthographic_world_per_screen_point(),
        )
    }

    pub fn orthographic_world_span_height(self) -> Result<f64, RenderApiError> {
        checked_scalar(
            self.presentation.height_points * self.view.orthographic_world_per_screen_point(),
        )
    }

    pub fn perspective_vertical_fov_radians(self) -> Result<f64, RenderApiError> {
        let ratio = (self.presentation.height_points * 0.5)
            / self.view.perspective_focal_length_screen_points();
        checked_scalar(2.0 * ratio.atan())
    }

    pub fn world_per_screen_point_at_target(self) -> Result<f64, RenderApiError> {
        match self.view.projection() {
            Projection::Orthographic => Ok(self.view.orthographic_world_per_screen_point()),
            Projection::Perspective => checked_scalar(
                self.view.perspective_view_distance_world()
                    / self.view.perspective_focal_length_screen_points(),
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Vec3([f64; 3]);

impl Vec3 {
    const X: Self = Self([1.0, 0.0, 0.0]);
    const Y: Self = Self([0.0, 1.0, 0.0]);
    const NEG_Z: Self = Self([0.0, 0.0, -1.0]);

    const fn from_array(value: [f64; 3]) -> Self {
        Self(value)
    }

    fn checked_add(self, other: Self) -> Result<Self, RenderApiError> {
        Self::checked([
            self.0[0] + other.0[0],
            self.0[1] + other.0[1],
            self.0[2] + other.0[2],
        ])
    }

    fn checked_sub(self, other: Self) -> Result<Self, RenderApiError> {
        Self::checked([
            self.0[0] - other.0[0],
            self.0[1] - other.0[1],
            self.0[2] - other.0[2],
        ])
    }

    fn checked_mul(self, scalar: f64) -> Result<Self, RenderApiError> {
        if !scalar.is_finite() {
            return Err(RenderApiError::CameraMathNotFinite);
        }
        Self::checked([self.0[0] * scalar, self.0[1] * scalar, self.0[2] * scalar])
    }

    fn checked(value: [f64; 3]) -> Result<Self, RenderApiError> {
        if value.iter().all(|component| component.is_finite()) {
            Ok(Self(value.map(canonical_zero)))
        } else {
            Err(RenderApiError::CameraMathNotFinite)
        }
    }

    fn cross(self, other: Self) -> Self {
        Self([
            self.0[1] * other.0[2] - self.0[2] * other.0[1],
            self.0[2] * other.0[0] - self.0[0] * other.0[2],
            self.0[0] * other.0[1] - self.0[1] * other.0[0],
        ])
    }

    fn dot(self, other: Self) -> Result<f64, RenderApiError> {
        checked_scalar(self.0[0] * other.0[0] + self.0[1] * other.0[1] + self.0[2] * other.0[2])
    }

    fn normalized(self) -> Result<Self, RenderApiError> {
        if !self.0.iter().all(|component| component.is_finite()) {
            return Err(RenderApiError::CameraMathNotFinite);
        }
        let scale = self.0.iter().map(|value| value.abs()).fold(0.0, f64::max);
        if scale == 0.0 {
            return Err(RenderApiError::DegenerateViewDirection);
        }
        let scaled = self.0.map(|value| value / scale);
        let length = scaled.iter().map(|value| value * value).sum::<f64>().sqrt();
        Self::checked(scaled.map(|value| value / length))
    }

    fn to_world_point(self) -> Result<WorldPoint3, RenderApiError> {
        WorldPoint3::new(self.0[0], self.0[1], self.0[2])
            .map_err(|_| RenderApiError::CameraMathNotFinite)
    }
}

fn axes_from_orientation(orientation: UnitQuaternion) -> Result<CameraAxes, RenderApiError> {
    let right = rotate(orientation, Vec3::X)?.normalized()?;
    let up = rotate(orientation, Vec3::Y)?.normalized()?;
    let forward = rotate(orientation, Vec3::NEG_Z)?.normalized()?;
    Ok(CameraAxes {
        forward: forward.0,
        right: right.0,
        up: up.0,
    })
}

fn rotate(quaternion: UnitQuaternion, vector: Vec3) -> Result<Vec3, RenderApiError> {
    let [x, y, z, w] = quaternion.xyzw();
    let imaginary = Vec3([x, y, z]);
    let twice_cross = imaginary.cross(vector).checked_mul(2.0)?;
    vector
        .checked_add(twice_cross.checked_mul(w)?)?
        .checked_add(imaginary.cross(twice_cross))
}

fn is_finite_positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

fn checked_scalar(value: f64) -> Result<f64, RenderApiError> {
    if value.is_finite() {
        Ok(canonical_zero(value))
    } else {
        Err(RenderApiError::CameraMathNotFinite)
    }
}

fn canonical_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

#[cfg(test)]
mod tests {
    use mirante4d_dataset::{
        CpuByteLease, CpuLedgerCategory, DatasetResourceIdentity, DatasetSourceId, ResourceRegion,
    };
    use mirante4d_domain::{
        DisplayWindow, Opacity, RgbColor, SamplingPolicy, ScaleLevel, Shape3D, TransferCurve,
    };

    use super::*;

    const EPSILON: f64 = 1.0e-12;

    struct TestCharge(u64);

    impl CpuByteLease for TestCharge {
        fn category(&self) -> CpuLedgerCategory {
            CpuLedgerCategory::QueuesAndResults
        }

        fn reserved_bytes(&self) -> u64 {
            self.0
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= EPSILON,
            "expected {expected}, got {actual}"
        );
    }

    fn camera(projection: Projection) -> CameraFrame {
        let view = CameraView::new(
            projection,
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            1.0,
            8.0,
            10.0,
        )
        .unwrap();
        CameraFrame::new(view, PresentationViewport::new(8.0, 8.0).unwrap()).unwrap()
    }

    #[test]
    fn render_pass_kind_is_bound_to_target_geometry() {
        assert_eq!(
            RenderViewIntent::volume(
                camera(Projection::Orthographic).view(),
                IsoLightState::attached_camera(),
            )
            .pass_kind(),
            RenderPassKind::Volume
        );
        assert_eq!(
            RenderViewIntent::cross_section(
                CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 1.0, 1.0,)
                    .unwrap(),
            )
            .pass_kind(),
            RenderPassKind::Plane
        );
    }

    fn layer(key: u32) -> LayerRenderIntent {
        LayerRenderIntent::new(
            LogicalLayerKey::new(key),
            LayerTransfer::new(
                DisplayWindow::new(0.0, 1.0).unwrap(),
                RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
                Opacity::new(1.0).unwrap(),
                TransferCurve::linear(),
                false,
            ),
            RenderState::mip(SamplingPolicy::VoxelExact),
        )
    }

    fn intent(layers: Vec<LayerRenderIntent>) -> Result<RenderIntent, RenderApiError> {
        RenderIntent::new(
            FrameIdentity::new(7),
            resource_identity(),
            TimeIndex::new(2),
            RenderViewIntent::volume(
                camera(Projection::Orthographic).view(),
                IsoLightState::attached_camera(),
            ),
            PresentationViewport::new(800.0, 600.0).unwrap(),
            RenderExtent::new(1600, 1200).unwrap(),
            layers,
        )
    }

    fn resource_key(x: u64) -> DatasetResourceKey {
        resource_key_at(3, 5, x)
    }

    fn resource_identity() -> DatasetResourceIdentity {
        DatasetResourceIdentity::Verified(
            "m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
        )
    }

    fn resource_key_at(layer: u32, timepoint: u64, x: u64) -> DatasetResourceKey {
        DatasetResourceKey::new(
            resource_identity(),
            LogicalLayerKey::new(layer),
            TimeIndex::new(timepoint),
            ScaleLevel::new(1),
            ResourceRegion::new([0, 0, x], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
        )
    }

    fn requirements_intent(frame: u64) -> RenderIntent {
        RenderIntent::new(
            FrameIdentity::new(frame),
            resource_identity(),
            TimeIndex::new(5),
            RenderViewIntent::volume(
                camera(Projection::Orthographic).view(),
                IsoLightState::attached_camera(),
            ),
            PresentationViewport::new(800.0, 600.0).unwrap(),
            RenderExtent::new(1600, 1200).unwrap(),
            vec![layer(3)],
        )
        .unwrap()
    }

    fn presented_frame(frame: u64, extent: RenderExtent) -> PresentedFrame {
        let intent = requirements_intent(frame);
        let key = resource_key_at(3, 5, 0);
        let requirements = RenderRequirements::new(
            &intent,
            vec![RenderRequirement::new(
                key,
                RenderRequirementRole::FirstUsefulFrame,
            )],
        )
        .unwrap();
        let coverage = FrameCoverage::from_available(&requirements, &[key]).unwrap();
        let progress = FrameProgress::new(coverage, FrameCompleteness::Exact, None).unwrap();
        PresentedFrame::new(PresentationToken::new(9).unwrap(), extent, progress)
    }

    #[test]
    fn render_extent_and_intent_are_validated_and_bounded() {
        assert_eq!(
            RenderExtent::new(0, 1),
            Err(RenderApiError::InvalidRenderExtent)
        );
        assert_eq!(
            RenderExtentEnvelope::new(1920, 0),
            Err(RenderApiError::InvalidRenderExtentEnvelope)
        );
        let envelope = RenderExtentEnvelope::new(1920, 1080).unwrap();
        assert_eq!(envelope.max_width_pixels(), 1920);
        assert_eq!(envelope.max_height_pixels(), 1080);
        assert_eq!(intent(Vec::new()), Err(RenderApiError::EmptyRenderLayers));
        assert_eq!(
            intent(vec![layer(2), layer(2)]),
            Err(RenderApiError::DuplicateRenderLayer { ordinal: 2 })
        );

        let too_many = (0..=MAX_RENDER_LAYERS)
            .map(|index| layer(u32::try_from(index).unwrap()))
            .collect();
        assert_eq!(
            intent(too_many),
            Err(RenderApiError::TooManyRenderLayers {
                actual: MAX_RENDER_LAYERS + 1,
                maximum: MAX_RENDER_LAYERS,
            })
        );

        let intent = intent(vec![layer(2), layer(9)]).unwrap();
        assert_eq!(intent.frame(), FrameIdentity::new(7));
        let reframed = intent.clone().with_frame(FrameIdentity::new(8));
        assert_eq!(reframed.frame(), FrameIdentity::new(8));
        assert_eq!(reframed.layers(), intent.layers());
        assert_eq!(intent.timepoint(), TimeIndex::new(2));
        assert_eq!(intent.extent().width_pixels(), 1600);
        assert_eq!(
            intent
                .layers()
                .iter()
                .map(LayerRenderIntent::layer)
                .collect::<Vec<_>>(),
            vec![LogicalLayerKey::new(2), LogicalLayerKey::new(9)]
        );
    }

    #[test]
    fn volume_pick_query_is_bound_to_presented_frame_and_pixel_extent() {
        let presented = presented_frame(17, RenderExtent::new(640, 360).unwrap());
        let query = VolumePickQuery::new(
            &presented,
            TimeIndex::new(5),
            LogicalLayerKey::new(3),
            [639.5, 359.5],
            VolumePickPolicy::MipArgmax,
        )
        .unwrap();
        assert_eq!(query.token(), presented.token());
        assert_eq!(query.frame(), FrameIdentity::new(17));
        assert_eq!(query.extent(), RenderExtent::new(640, 360).unwrap());
        assert_eq!(query.timepoint(), TimeIndex::new(5));
        assert_eq!(query.layer(), LogicalLayerKey::new(3));
        assert_eq!(query.render_pixel(), [639.5, 359.5]);

        assert_eq!(
            VolumePickQuery::new(
                &presented,
                TimeIndex::new(5),
                LogicalLayerKey::new(3),
                [640.0, 0.0],
                VolumePickPolicy::MipArgmax,
            ),
            Err(RenderApiError::PickPixelOutsideExtent)
        );
        assert_eq!(
            VolumePickQuery::new(
                &presented,
                TimeIndex::new(5),
                LogicalLayerKey::new(3),
                [f64::NAN, 0.0],
                VolumePickPolicy::MipArgmax,
            ),
            Err(RenderApiError::NonFiniteRenderPixel)
        );
    }

    #[test]
    fn volume_pick_result_is_fixed_size_and_scientifically_explicit() {
        let presented = presented_frame(18, RenderExtent::new(64, 32).unwrap());
        let query = VolumePickQuery::new(
            &presented,
            TimeIndex::new(5),
            LogicalLayerKey::new(3),
            [12.5, 4.5],
            VolumePickPolicy::MaximumOpacityContribution,
        )
        .unwrap();
        let hit = VolumePickResult::voxel(
            query,
            WorldPoint3::new(3.0, 4.0, 5.0).unwrap(),
            VolumePickValue::IntensityU16(42),
            7.5,
            VolumePickCompleteness::Incomplete,
        )
        .unwrap();
        assert_eq!(hit.kind(), VolumePickHitKind::Voxel);
        assert_eq!(hit.world_position().unwrap().components(), [3.0, 4.0, 5.0]);
        assert_eq!(hit.value(), Some(VolumePickValue::IntensityU16(42)));
        assert_eq!(hit.ray_distance_world(), Some(7.5));
        assert_eq!(hit.completeness(), VolumePickCompleteness::Incomplete);

        let empty = VolumePickResult::empty(query, VolumePickCompleteness::Exact);
        assert_eq!(empty.kind(), VolumePickHitKind::Empty);
        assert_eq!(empty.world_position(), None);
        assert_eq!(empty.value(), None);
        assert_eq!(empty.ray_distance_world(), None);

        assert_eq!(
            VolumePickResult::voxel(
                query,
                WorldPoint3::origin(),
                VolumePickValue::IntensityF32(f32::NAN),
                0.0,
                VolumePickCompleteness::Exact,
            ),
            Err(RenderApiError::InvalidVolumePickResult)
        );
        assert_eq!(
            VolumePickResult::voxel(
                query,
                WorldPoint3::origin(),
                VolumePickValue::IntensityU8(1),
                -1.0,
                VolumePickCompleteness::Exact,
            ),
            Err(RenderApiError::InvalidVolumePickResult)
        );
    }

    #[test]
    fn requirements_use_semantic_keys_and_reject_duplicate_or_unbounded_work() {
        let intent = requirements_intent(11);
        let first =
            RenderRequirement::new(resource_key(0), RenderRequirementRole::FirstUsefulFrame);
        let refinement = RenderRequirement::new(resource_key(1), RenderRequirementRole::Refinement);
        let requirements = RenderRequirements::new(&intent, vec![first, refinement]).unwrap();
        assert_eq!(requirements.frame(), FrameIdentity::new(11));
        assert_eq!(
            requirements.resources().collect::<Vec<_>>(),
            vec![first, refinement]
        );

        assert_eq!(
            RenderRequirements::new(&intent, Vec::new()),
            Err(RenderApiError::EmptyRenderRequirements)
        );
        assert_eq!(
            RenderRequirements::new(&intent, vec![first, first]),
            Err(RenderApiError::DuplicateRenderRequirement)
        );
        assert_eq!(
            RenderRequirements::new(&intent, vec![refinement]),
            Err(RenderApiError::MissingFirstUsefulRequirement)
        );
        assert_eq!(
            RenderRequirements::new(
                &intent,
                vec![RenderRequirement::new(
                    DatasetResourceKey::new(
                        DatasetResourceIdentity::Unverified(DatasetSourceId::new(99)),
                        LogicalLayerKey::new(3),
                        TimeIndex::new(5),
                        ScaleLevel::new(1),
                        ResourceRegion::new([0, 0, 2], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
                    ),
                    RenderRequirementRole::FirstUsefulFrame,
                )],
            ),
            Err(RenderApiError::RequirementIdentityMismatch)
        );
        assert_eq!(
            RenderRequirements::new(
                &intent,
                vec![RenderRequirement::new(
                    resource_key_at(4, 5, 2),
                    RenderRequirementRole::FirstUsefulFrame,
                )],
            ),
            Err(RenderApiError::RequirementLayerNotInIntent { ordinal: 4 })
        );
        assert_eq!(
            RenderRequirements::new(
                &intent,
                vec![RenderRequirement::new(
                    resource_key_at(3, 6, 2),
                    RenderRequirementRole::FirstUsefulFrame,
                )],
            ),
            Err(RenderApiError::RequirementTimepointMismatch {
                expected: 5,
                actual: 6,
            })
        );

        let too_many = (0..=MAX_RENDER_REQUIREMENTS)
            .map(|index| {
                RenderRequirement::new(
                    resource_key(u64::try_from(index).unwrap()),
                    RenderRequirementRole::Refinement,
                )
            })
            .collect();
        assert_eq!(
            RenderRequirements::new(&intent, too_many),
            Err(RenderApiError::TooManyRenderRequirements {
                actual: MAX_RENDER_REQUIREMENTS + 1,
                maximum: MAX_RENDER_REQUIREMENTS,
            })
        );
    }

    #[test]
    fn prepared_requirement_authority_shares_exact_bodies_and_has_linear_host_bound() {
        let canonical: Arc<[DatasetResourceKey]> = Arc::from([resource_key(0), resource_key(1)]);
        let ranked: Arc<[DatasetResourceKey]> = Arc::from([resource_key(1), resource_key(0)]);
        let body =
            PreparedResourceBody::new(Arc::clone(&canonical), Arc::clone(&ranked), None).unwrap();
        assert!(Arc::ptr_eq(body.canonical(), &canonical));
        assert!(Arc::ptr_eq(body.ranked(), &ranked));
        assert!(body.shares_storage_with(&body.clone()));
        assert_eq!(body.resource_index(resource_key(0)), Some(0));
        assert_eq!(body.resource_index(resource_key(1)), Some(1));

        let key_bytes = u64::try_from(
            (canonical.len() + ranked.len()) * std::mem::size_of::<DatasetResourceKey>(),
        )
        .unwrap();
        assert!(body.host_allocation_bytes() >= key_bytes);
        assert!(
            body.host_allocation_bytes()
                <= key_bytes
                    + u64::try_from(std::mem::size_of::<PreparedResourceBodyData>()).unwrap()
        );
        let charge: Arc<dyn CpuByteLease> = Arc::new(TestCharge(body.host_allocation_bytes()));
        body.attach_charge(Arc::clone(&charge)).unwrap();
        assert_eq!(body.charged_bytes(), Some(body.host_allocation_bytes()));
        assert!(matches!(
            body.attach_charge(Arc::new(TestCharge(body.host_allocation_bytes()))),
            Err(RenderApiError::PreparedRequirementChargeAlreadyAttached)
        ));

        let prepared = PreparedRenderRequirements::new(
            resource_identity(),
            TimeIndex::new(5),
            vec![LogicalLayerKey::new(3)],
            body.clone(),
            1,
        )
        .unwrap();
        let bound = prepared.bind(&requirements_intent(12)).unwrap();
        assert!(bound.prepared_body().shares_storage_with(&body));
        let requirements = bound.resources().collect::<Vec<_>>();
        assert_eq!(requirements[0].key(), resource_key(0));
        assert_eq!(requirements[0].role(), RenderRequirementRole::Refinement);
        assert_eq!(requirements[1].key(), resource_key(1));
        assert_eq!(
            requirements[1].role(),
            RenderRequirementRole::FirstUsefulFrame
        );
    }

    #[test]
    fn navigation_prefetch_is_resident_but_not_required_until_o1_promotion() {
        let first = resource_key(0);
        let refinement = resource_key(1);
        let guard = resource_key(2);
        let body = PreparedResourceBody::new(
            Arc::from([first, refinement, guard]),
            Arc::from([first, refinement, guard]),
            None,
        )
        .unwrap();
        let prepared = PreparedRenderRequirements::new_with_required_prefix(
            resource_identity(),
            TimeIndex::new(5),
            vec![LogicalLayerKey::new(3)],
            body,
            1,
            2,
        )
        .unwrap();
        assert_eq!(prepared.required_prefix_len(), 2);
        assert_eq!(prepared.prefetch_resource_count(), 1);

        let current = prepared.bind(&requirements_intent(20)).unwrap();
        assert_eq!(
            current.requirement(2).unwrap().role(),
            RenderRequirementRole::Prefetch
        );
        let current_coverage =
            FrameCoverage::from_available(&current, &[first, refinement]).unwrap();
        assert!(current_coverage.is_full());
        assert_eq!(current_coverage.fraction(), 1.0);
        assert_eq!(current_coverage.available_prefetch(), 0);
        assert_eq!(current_coverage.total_prefetch(), 1);

        let promoted_prepared = prepared.promote_prefetch();
        assert_eq!(promoted_prepared.required_prefix_len(), 3);
        let promoted = promoted_prepared.bind(&requirements_intent(21)).unwrap();
        assert!(current.shares_resources_with(&promoted));
        assert_eq!(
            promoted.requirement(2).unwrap().role(),
            RenderRequirementRole::Refinement
        );
        let promoted_coverage = current_coverage.rebind(&promoted).unwrap();
        assert!(!promoted_coverage.is_full());
        assert_eq!(promoted_coverage.available_requirements(), 2);
        assert_eq!(promoted_coverage.total_requirements(), 3);

        let full = promoted_coverage
            .with_availability_changes(&promoted, &[(guard, true)])
            .unwrap();
        assert!(full.is_full());
        assert_eq!(full.fraction(), 1.0);
    }

    #[test]
    fn progressive_frame_status_cannot_claim_uncovered_work_is_exact() {
        let first_a = resource_key(0);
        let first_b = resource_key(1);
        let refinement_a = resource_key(2);
        let refinement_b = resource_key(3);
        let intent = requirements_intent(12);
        let requirements = RenderRequirements::new(
            &intent,
            vec![
                RenderRequirement::new(first_a, RenderRequirementRole::FirstUsefulFrame),
                RenderRequirement::new(first_b, RenderRequirementRole::FirstUsefulFrame),
                RenderRequirement::new(refinement_a, RenderRequirementRole::Refinement),
                RenderRequirement::new(refinement_b, RenderRequirementRole::Refinement),
            ],
        )
        .unwrap();

        assert_eq!(
            FrameCoverage::from_available(&requirements, &[first_a, first_a]),
            Err(RenderApiError::DuplicateCoveredResource)
        );
        assert_eq!(
            FrameCoverage::from_available(
                &requirements,
                &[
                    first_a,
                    first_b,
                    refinement_a,
                    refinement_b,
                    resource_key(99)
                ],
            ),
            Err(RenderApiError::TooManyCoveredResources {
                actual: 5,
                maximum: 4,
            })
        );
        assert_eq!(
            FrameCoverage::from_available(&requirements, &[resource_key(99)]),
            Err(RenderApiError::CoveredResourceNotRequired)
        );

        let before_first_useful =
            FrameCoverage::from_available(&requirements, &[first_a, refinement_a]).unwrap();
        assert_eq!(before_first_useful.available_first_useful(), 1);
        assert_eq!(before_first_useful.total_first_useful(), 2);
        assert_eq!(
            FrameProgress::new(before_first_useful, FrameCompleteness::Progressive, None,),
            Err(RenderApiError::InvalidFrameProgress)
        );

        let partial =
            FrameCoverage::from_available(&requirements, &[first_a, first_b, refinement_a])
                .unwrap();
        let full = FrameCoverage::from_available(
            &requirements,
            &[first_a, first_b, refinement_a, refinement_b],
        )
        .unwrap();
        assert_eq!(partial.frame(), FrameIdentity::new(12));
        assert_eq!(partial.available_first_useful(), 2);
        assert_eq!(partial.available_refinement(), 1);
        assert_eq!(partial.total_refinement(), 2);
        assert_eq!(partial.fraction(), 0.75);
        assert!(partial.is_first_useful());
        assert!(!partial.is_full());
        assert_eq!(
            FrameProgress::new(partial.clone(), FrameCompleteness::Exact, None),
            Err(RenderApiError::InvalidFrameProgress)
        );
        assert_eq!(
            FrameProgress::new(full.clone(), FrameCompleteness::Complete, None),
            Err(RenderApiError::InvalidFrameProgress)
        );

        assert!(FrameProgress::new(partial, FrameCompleteness::Progressive, None).is_ok());
        assert!(
            FrameProgress::new(
                full.clone(),
                FrameCompleteness::Complete,
                Some(FrameLimitation::CoarserScale),
            )
            .is_ok()
        );
        assert!(FrameProgress::new(full, FrameCompleteness::Exact, None).is_ok());
    }

    #[test]
    fn presentation_lifecycle_carries_only_opaque_identity_and_frame_facts() {
        assert_eq!(
            PresentationToken::new(0),
            Err(RenderApiError::InvalidPresentationToken)
        );
        let token = PresentationToken::new(17).unwrap();
        let extent = RenderExtent::new(640, 480).unwrap();
        let registration = PresentationRegistration::new(token, extent);
        let viewport = PresentationViewport::new(320.0, 240.0).unwrap();
        let paint = PresentationPaintRequest::new(token, viewport);

        let first = resource_key(0);
        let refinement_a = resource_key(1);
        let refinement_b = resource_key(2);
        let alternate_a = resource_key(3);
        let alternate_b = resource_key(4);
        let requirements = |frame, refinements: [DatasetResourceKey; 2]| {
            let intent = requirements_intent(frame);
            RenderRequirements::new(
                &intent,
                vec![
                    RenderRequirement::new(first, RenderRequirementRole::FirstUsefulFrame),
                    RenderRequirement::new(refinements[0], RenderRequirementRole::Refinement),
                    RenderRequirement::new(refinements[1], RenderRequirementRole::Refinement),
                ],
            )
            .unwrap()
        };
        let current_requirements = requirements(8, [refinement_a, refinement_b]);
        let alternate_requirements = requirements(8, [alternate_a, alternate_b]);
        let stale_requirements = requirements(7, [refinement_a, refinement_b]);
        let progress =
            |requirements: &RenderRequirements, available: &[DatasetResourceKey], completeness| {
                FrameProgress::new(
                    FrameCoverage::from_available(requirements, available).unwrap(),
                    completeness,
                    None,
                )
                .unwrap()
            };

        let mut registry = PresentationRegistry::new();
        assert!(registry.is_empty());
        registry.register(registration).unwrap();
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.resolve_paint(paint).unwrap(), None);
        assert_eq!(
            registry.register(registration),
            Err(RenderFault::PresentationAlreadyRegistered { token })
        );

        let mut bounded = PresentationRegistry::new();
        for value in 1..=MAX_PRESENTATION_TARGETS {
            bounded
                .register(PresentationRegistration::new(
                    PresentationToken::new(u64::try_from(value).unwrap()).unwrap(),
                    extent,
                ))
                .unwrap();
        }
        assert_eq!(
            bounded.register(PresentationRegistration::new(
                PresentationToken::new(u64::try_from(MAX_PRESENTATION_TARGETS + 1).unwrap())
                    .unwrap(),
                extent,
            )),
            Err(RenderFault::PresentationCapacityExceeded {
                maximum: MAX_PRESENTATION_TARGETS,
            })
        );

        let partial_frame = PresentedFrame::new(
            token,
            extent,
            progress(
                &current_requirements,
                &[first, refinement_a],
                FrameCompleteness::Progressive,
            ),
        );
        registry
            .update(PresentationUpdate::new(partial_frame.clone()))
            .unwrap();
        assert_eq!(
            registry.resolve_paint(paint).unwrap(),
            Some(partial_frame.clone())
        );

        let swapped_same_set = PresentedFrame::new(
            token,
            extent,
            progress(
                &current_requirements,
                &[first, refinement_b],
                FrameCompleteness::Progressive,
            ),
        );
        assert_eq!(
            registry.update(PresentationUpdate::new(swapped_same_set)),
            Err(RenderFault::FrameProgressRegressed {
                frame: FrameIdentity::new(8)
            })
        );

        let crossed_requirement_set = PresentedFrame::new(
            token,
            extent,
            progress(
                &alternate_requirements,
                &[first, alternate_a],
                FrameCompleteness::Progressive,
            ),
        );
        assert_eq!(
            registry.update(PresentationUpdate::new(crossed_requirement_set)),
            Err(RenderFault::FrameProgressRegressed {
                frame: FrameIdentity::new(8)
            })
        );

        let exact_frame = PresentedFrame::new(
            token,
            extent,
            progress(
                &current_requirements,
                &[first, refinement_a, refinement_b],
                FrameCompleteness::Exact,
            ),
        );
        registry
            .update(PresentationUpdate::new(exact_frame.clone()))
            .unwrap();
        assert_eq!(registry.resolve_paint(paint).unwrap(), Some(exact_frame));
        assert_eq!(
            registry.update(PresentationUpdate::new(partial_frame)),
            Err(RenderFault::FrameProgressRegressed {
                frame: FrameIdentity::new(8)
            })
        );

        let stale = PresentedFrame::new(
            token,
            extent,
            progress(
                &stale_requirements,
                &[first, refinement_a, refinement_b],
                FrameCompleteness::Exact,
            ),
        );
        assert_eq!(
            registry.update(PresentationUpdate::new(stale)),
            Err(RenderFault::StaleFrame {
                actual: FrameIdentity::new(7),
                current: FrameIdentity::new(8),
            })
        );

        registry.retire(PresentationRetirement::new(token)).unwrap();
        assert!(registry.is_empty());
        assert_eq!(
            registry.resolve_paint(paint),
            Err(RenderFault::PresentationNotRegistered { token })
        );
    }

    #[test]
    fn volume_pick_ticket_rejects_zero_and_preserves_frame_identity() {
        let presentation = PresentationToken::new(17).unwrap();
        let frame = FrameIdentity::new(23);
        assert_eq!(
            VolumePickTicket::new(0, presentation, frame),
            Err(RenderApiError::InvalidVolumePickTicket)
        );

        let ticket = VolumePickTicket::new(41, presentation, frame).unwrap();
        assert_eq!(ticket.sequence(), 41);
        assert_eq!(ticket.presentation(), presentation);
        assert_eq!(ticket.frame(), frame);
    }

    #[test]
    fn render_faults_are_typed_and_backend_neutral() {
        let token = PresentationToken::new(2).unwrap();
        assert!(matches!(
            RenderFault::CapacityExceeded {
                category: GpuLedgerCategory::PayloadResidency,
                requested_bytes: 8,
                available_bytes: 4,
            },
            RenderFault::CapacityExceeded {
                category: GpuLedgerCategory::PayloadResidency,
                requested_bytes: 8,
                available_bytes: 4,
            }
        ));
        assert_eq!(
            RenderFault::PresentationNotRegistered { token },
            RenderFault::PresentationNotRegistered { token }
        );
        assert_eq!(
            RenderFault::ResourceUnavailable {
                key: resource_key(9)
            },
            RenderFault::ResourceUnavailable {
                key: resource_key(9)
            }
        );
    }

    #[test]
    fn presentation_viewport_rejects_nonpositive_or_nonfinite_dimensions() {
        assert_eq!(
            PresentationViewport::new(0.0, 1.0),
            Err(RenderApiError::InvalidPresentationViewport)
        );
        assert_eq!(
            PresentationViewport::new(1.0, f64::NAN),
            Err(RenderApiError::InvalidPresentationViewport)
        );
        assert_eq!(
            PresentationViewport::new(f64::INFINITY, 1.0),
            Err(RenderApiError::InvalidPresentationViewport)
        );
    }

    #[test]
    fn canonical_identity_orientation_defines_expected_axes_and_eye() {
        let camera = camera(Projection::Orthographic);
        assert_eq!(camera.axes().right(), [1.0, 0.0, 0.0]);
        assert_eq!(camera.axes().up(), [0.0, 1.0, 0.0]);
        assert_eq!(camera.axes().forward(), [0.0, 0.0, -1.0]);
        assert_eq!(camera.eye().components(), [0.0, 0.0, 10.0]);
    }

    #[test]
    fn quarter_turn_camera_orientation_has_known_axes_and_eye() {
        let half_angle = std::f64::consts::FRAC_PI_4;
        let orientation =
            UnitQuaternion::new_xyzw(0.0, half_angle.sin(), 0.0, half_angle.cos()).unwrap();
        let view = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::origin(),
            orientation,
            1.0,
            8.0,
            10.0,
        )
        .unwrap();
        let camera = CameraFrame::new(view, PresentationViewport::new(8.0, 8.0).unwrap()).unwrap();

        for (actual, expected) in camera.axes().right().into_iter().zip([0.0, 0.0, -1.0]) {
            assert_close(actual, expected);
        }
        for (actual, expected) in camera.axes().up().into_iter().zip([0.0, 1.0, 0.0]) {
            assert_close(actual, expected);
        }
        for (actual, expected) in camera.axes().forward().into_iter().zip([-1.0, 0.0, 0.0]) {
            assert_close(actual, expected);
        }
        for (actual, expected) in camera.eye().components().into_iter().zip([10.0, 0.0, 0.0]) {
            assert_close(actual, expected);
        }
    }

    #[test]
    fn orthographic_rays_are_parallel_with_screen_shifted_origins() {
        let camera = camera(Projection::Orthographic);
        let center = camera.ray_for_screen_point(0.0, 0.0).unwrap();
        let corner = camera.ray_for_screen_point(4.0, 4.0).unwrap();

        assert_eq!(center.direction(), [0.0, 0.0, -1.0]);
        assert_eq!(corner.direction(), center.direction());
        assert_eq!(center.origin().components(), [0.0, 0.0, 10.0]);
        assert_eq!(corner.origin().components(), [4.0, 4.0, 10.0]);
    }

    #[test]
    fn perspective_rays_diverge_from_one_eye() {
        let camera = camera(Projection::Perspective);
        let center = camera.ray_for_screen_point(0.0, 0.0).unwrap();
        let corner = camera.ray_for_screen_point(4.0, 4.0).unwrap();

        assert_eq!(center.origin(), corner.origin());
        assert_eq!(center.direction(), [0.0, 0.0, -1.0]);
        assert_ne!(center.direction(), corner.direction());
        let direction = corner.direction();
        assert_close(
            direction
                .iter()
                .map(|component| component * component)
                .sum::<f64>(),
            1.0,
        );
    }

    #[test]
    fn render_pixel_centers_map_y_opposite_camera_up() {
        let camera = camera(Projection::Orthographic);
        let top = camera.ray_for_render_pixel(1.0, 0.0, 4, 4).unwrap();
        let bottom = camera.ray_for_render_pixel(1.0, 3.0, 4, 4).unwrap();
        assert!(top.origin().y() > bottom.origin().y());
    }

    #[test]
    fn projection_measurements_use_canonical_view_values() {
        let orthographic = camera(Projection::Orthographic);
        assert_close(orthographic.orthographic_world_span_width().unwrap(), 8.0);
        assert_close(orthographic.orthographic_world_span_height().unwrap(), 8.0);
        assert_close(
            orthographic.world_per_screen_point_at_target().unwrap(),
            1.0,
        );

        let perspective = camera(Projection::Perspective);
        assert_close(
            perspective.perspective_vertical_fov_radians().unwrap(),
            2.0 * 0.5_f64.atan(),
        );
        assert_close(
            perspective.world_per_screen_point_at_target().unwrap(),
            1.25,
        );
    }

    #[test]
    fn orthographic_world_projection_has_analytic_screen_coordinates() {
        let camera = camera(Projection::Orthographic);
        let projected = camera
            .project_world_point(WorldPoint3::new(3.0, -2.0, 4.0).unwrap())
            .unwrap()
            .unwrap();

        assert_close(projected.screen_x_points(), 3.0);
        assert_close(projected.screen_y_points(), -2.0);
        assert_close(projected.depth_world(), 6.0);
    }

    #[test]
    fn perspective_world_projection_uses_depth_and_inverts_ray_construction() {
        let camera = camera(Projection::Perspective);
        let projected = camera
            .project_world_point(WorldPoint3::new(2.0, -1.0, 5.0).unwrap())
            .unwrap()
            .unwrap();

        // The identity camera eye is [0, 0, 10], focal length is 8 points,
        // and this point is five world units in front of the eye.
        assert_close(projected.screen_x_points(), 3.2);
        assert_close(projected.screen_y_points(), -1.6);
        assert_close(projected.depth_world(), 5.0);

        let ray = camera
            .ray_for_screen_point(projected.screen_x_points(), projected.screen_y_points())
            .unwrap();
        let from_eye = [2.0, -1.0, -5.0];
        let length = from_eye
            .iter()
            .map(|component| component * component)
            .sum::<f64>()
            .sqrt();
        for (actual, expected) in ray
            .direction()
            .into_iter()
            .zip(from_eye.map(|component| component / length))
        {
            assert_close(actual, expected);
        }
    }

    #[test]
    fn projection_rejects_the_eye_plane_and_points_behind_it() {
        for projection in [Projection::Orthographic, Projection::Perspective] {
            let camera = camera(projection);
            assert_eq!(
                camera
                    .project_world_point(WorldPoint3::new(1.0, 1.0, 10.0).unwrap())
                    .unwrap(),
                None
            );
            assert_eq!(
                camera
                    .project_world_point(WorldPoint3::new(1.0, 1.0, 11.0).unwrap())
                    .unwrap(),
                None
            );
        }
    }

    #[test]
    fn invalid_queries_and_nonfinite_results_fail_explicitly() {
        let camera = camera(Projection::Orthographic);
        assert_eq!(
            camera.ray_for_screen_point(f64::NAN, 0.0),
            Err(RenderApiError::NonFiniteScreenPoint)
        );
        assert_eq!(
            camera.ray_for_render_pixel(0.0, 0.0, 0, 4),
            Err(RenderApiError::InvalidRenderExtent)
        );
        assert_eq!(
            camera.ray_for_render_pixel(f64::INFINITY, 0.0, 4, 4),
            Err(RenderApiError::NonFiniteRenderPixel)
        );

        let extreme = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            f64::MAX,
            1.0,
            1.0,
        )
        .unwrap();
        let extreme =
            CameraFrame::new(extreme, PresentationViewport::new(f64::MAX, 1.0).unwrap()).unwrap();
        assert_eq!(
            extreme.orthographic_world_span_width(),
            Err(RenderApiError::CameraMathNotFinite)
        );
        assert_eq!(
            extreme.ray_for_screen_point(f64::MAX, 0.0),
            Err(RenderApiError::CameraMathNotFinite)
        );
    }
}
