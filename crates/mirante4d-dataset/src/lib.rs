//! Immutable, framework-neutral dataset and resource contracts.
//!
//! This crate describes scientific layers and the semantic resources exposed
//! by a dataset source. It owns no filesystem access, serialization, storage
//! layout, codec, scheduling, cache, lease issuance, or runtime behavior.

#![forbid(unsafe_code)]

use std::{collections::BTreeMap, sync::Arc};

use mirante4d_domain::{
    GridToWorld, IntensityDType, LogicalLayerKey, ScaleLevel, Shape3D, Shape4D, ShapeError,
    TimeIndex,
};
use mirante4d_identity::ScientificContentId;
use thiserror::Error;

pub const MAX_DATASET_LABEL_BYTES: usize = 256;
pub const MAX_LAYER_LABEL_BYTES: usize = 256;
pub const MAX_DATASET_LAYERS: usize = 4_096;
pub const MAX_SCALES_PER_LAYER: usize = 64;

/// Opaque identity for one opened source before its scientific content has
/// been verified.
///
/// The composition owner assigns a fresh value for every open. It is neither a
/// path, package identity, cache key, nor substitute scientific identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DatasetSourceId(u64);

impl DatasetSourceId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Identity carried by a semantic resource request.
///
/// Unverified sources are addressable only within their exact open session.
/// Verification hard-cuts that provisional identity to the stable scientific
/// content identity; the two variants never compare equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DatasetResourceIdentity {
    Unverified(DatasetSourceId),
    Verified(ScientificContentId),
}

/// A non-empty, axis-aligned `z,y,x` region at one multiscale level.
///
/// Coordinates are semantic grid coordinates, never storage chunk, shard, or
/// object coordinates. The exclusive end is validated at construction so
/// downstream bounds checks cannot overflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResourceRegion {
    origin: [u64; 3],
    shape: Shape3D,
}

impl PartialOrd for ResourceRegion {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ResourceRegion {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.origin, self.shape.dimensions()).cmp(&(other.origin, other.shape.dimensions()))
    }
}

impl ResourceRegion {
    pub fn new(origin: [u64; 3], shape: Shape3D) -> Result<Self, ResourceContractError> {
        let dimensions = shape.dimensions();
        let mut end_exclusive = [0; 3];
        for (axis, ((start, length), end)) in origin
            .into_iter()
            .zip(dimensions)
            .zip(&mut end_exclusive)
            .enumerate()
        {
            *end = start
                .checked_add(length)
                .ok_or(ResourceContractError::RegionEndOverflow { axis })?;
        }
        Ok(Self { origin, shape })
    }

    pub const fn origin(self) -> [u64; 3] {
        self.origin
    }

    pub const fn shape(self) -> Shape3D {
        self.shape
    }

    pub fn end_exclusive(self) -> [u64; 3] {
        let dimensions = self.shape.dimensions();
        std::array::from_fn(|axis| {
            self.origin[axis]
                .checked_add(dimensions[axis])
                .expect("ResourceRegion construction validates its exclusive end")
        })
    }

    pub fn fits_within(self, shape: Shape3D) -> bool {
        self.end_exclusive()
            .into_iter()
            .zip(shape.dimensions())
            .all(|(end, dimension)| end <= dimension)
    }
}

/// Semantic identity for one decoded multiscale resource.
///
/// Before verification the key is stable only within its exact opened source;
/// afterward it is rooted in scientific content. Physical package identity,
/// paths, arrays, chunks, shards, and codec details are intentionally absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DatasetResourceKey {
    identity: DatasetResourceIdentity,
    layer: LogicalLayerKey,
    timepoint: TimeIndex,
    scale: ScaleLevel,
    region: ResourceRegion,
}

impl DatasetResourceKey {
    pub const fn new(
        identity: DatasetResourceIdentity,
        layer: LogicalLayerKey,
        timepoint: TimeIndex,
        scale: ScaleLevel,
        region: ResourceRegion,
    ) -> Self {
        Self {
            identity,
            layer,
            timepoint,
            scale,
            region,
        }
    }

    pub const fn identity(self) -> DatasetResourceIdentity {
        self.identity
    }

    pub const fn layer(self) -> LogicalLayerKey {
        self.layer
    }

    pub const fn timepoint(self) -> TimeIndex {
        self.timepoint
    }

    pub const fn scale(self) -> ScaleLevel {
        self.scale
    }

    pub const fn region(self) -> ResourceRegion {
        self.region
    }
}

/// Effective validity representation for one semantic resource.
///
/// `AllValid` has no validity allocation. `BitMask` carries exactly one bit
/// per sample in canonical `z,y,x` order, least-significant bit first within
/// each byte. Unused high bits in the final byte must be zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceValidity {
    AllValid,
    BitMask,
}

/// Dtype-neutral metadata for one decoded payload reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResourcePayloadDescriptor {
    dtype: IntensityDType,
    shape: Shape3D,
    validity: ResourceValidity,
    sample_count: u64,
    value_byte_len: u64,
    validity_byte_len: u64,
    byte_len: u64,
}

impl ResourcePayloadDescriptor {
    pub fn new(
        dtype: IntensityDType,
        shape: Shape3D,
        validity: ResourceValidity,
    ) -> Result<Self, ResourceContractError> {
        let sample_count = shape
            .element_count()
            .map_err(|_| ResourceContractError::PayloadByteLengthOverflow)?;
        let value_byte_len = sample_count
            .checked_mul(u64::from(dtype.bytes_per_sample()))
            .ok_or(ResourceContractError::PayloadByteLengthOverflow)?;
        let validity_byte_len = match validity {
            ResourceValidity::AllValid => 0,
            ResourceValidity::BitMask => sample_count / 8 + u64::from(sample_count % 8 != 0),
        };
        let byte_len = value_byte_len
            .checked_add(validity_byte_len)
            .ok_or(ResourceContractError::PayloadByteLengthOverflow)?;
        Ok(Self {
            dtype,
            shape,
            validity,
            sample_count,
            value_byte_len,
            validity_byte_len,
            byte_len,
        })
    }

    pub const fn dtype(self) -> IntensityDType {
        self.dtype
    }

    pub const fn shape(self) -> Shape3D {
        self.shape
    }

    pub const fn validity(self) -> ResourceValidity {
        self.validity
    }

    pub const fn sample_count(self) -> u64 {
        self.sample_count
    }

    pub const fn value_byte_len(self) -> u64 {
        self.value_byte_len
    }

    pub const fn validity_byte_len(self) -> u64 {
        self.validity_byte_len
    }

    /// Total reservation size: decoded values plus any validity bitmask.
    pub const fn byte_len(self) -> u64 {
        self.byte_len
    }

    pub fn view<'a>(
        self,
        value_bytes: &'a [u8],
        validity_bits: Option<&'a [u8]>,
    ) -> Result<ResourcePayloadView<'a>, ResourceContractError> {
        ResourcePayloadView::from_descriptor(self, value_bytes, validity_bits)
    }
}

/// Immutable decoded samples and their explicit validity in canonical
/// little-endian `z,y,x` order.
///
/// The view borrows its value and optional validity allocations from its owner
/// and therefore cannot transfer or duplicate either allocation. `Uint8` is
/// byte-order independent; multi-byte values use little-endian representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourcePayloadView<'a> {
    descriptor: ResourcePayloadDescriptor,
    value_bytes: &'a [u8],
    validity_bits: Option<&'a [u8]>,
}

impl<'a> ResourcePayloadView<'a> {
    pub fn new(
        dtype: IntensityDType,
        shape: Shape3D,
        validity: ResourceValidity,
        value_bytes: &'a [u8],
        validity_bits: Option<&'a [u8]>,
    ) -> Result<Self, ResourceContractError> {
        ResourcePayloadDescriptor::new(dtype, shape, validity)?.view(value_bytes, validity_bits)
    }

    fn from_descriptor(
        descriptor: ResourcePayloadDescriptor,
        value_bytes: &'a [u8],
        validity_bits: Option<&'a [u8]>,
    ) -> Result<Self, ResourceContractError> {
        let expected = descriptor.value_byte_len();
        let actual = u64::try_from(value_bytes.len())
            .map_err(|_| ResourceContractError::PayloadByteLengthOverflow)?;
        if actual != expected {
            return Err(ResourceContractError::PayloadValueByteLengthMismatch { expected, actual });
        }

        match (descriptor.validity(), validity_bits) {
            (ResourceValidity::AllValid, None) => {}
            (ResourceValidity::AllValid, Some(_)) => {
                return Err(ResourceContractError::UnexpectedValidityBits);
            }
            (ResourceValidity::BitMask, None) => {
                return Err(ResourceContractError::MissingValidityBits);
            }
            (ResourceValidity::BitMask, Some(bits)) => {
                let expected = descriptor.validity_byte_len();
                let actual = u64::try_from(bits.len())
                    .map_err(|_| ResourceContractError::PayloadByteLengthOverflow)?;
                if actual != expected {
                    return Err(ResourceContractError::PayloadValidityByteLengthMismatch {
                        expected,
                        actual,
                    });
                }

                let used_bits = u8::try_from(descriptor.sample_count() % 8)
                    .expect("a remainder modulo eight fits in u8");
                if used_bits != 0 {
                    let used_mask = (1_u8 << used_bits) - 1;
                    let last = bits
                        .last()
                        .copied()
                        .expect("a nonzero sample count has a nonempty validity bitmask");
                    if last & !used_mask != 0 {
                        return Err(ResourceContractError::ValidityPaddingBitsNonZero {
                            used_bits,
                            final_byte: last,
                        });
                    }
                }
            }
        }

        Ok(Self {
            descriptor,
            value_bytes,
            validity_bits,
        })
    }

    pub const fn descriptor(self) -> ResourcePayloadDescriptor {
        self.descriptor
    }

    pub const fn dtype(self) -> IntensityDType {
        self.descriptor.dtype()
    }

    pub const fn shape(self) -> Shape3D {
        self.descriptor.shape()
    }

    pub const fn validity(self) -> ResourceValidity {
        self.descriptor.validity()
    }

    pub const fn value_bytes(self) -> &'a [u8] {
        self.value_bytes
    }

    pub const fn validity_bits(self) -> Option<&'a [u8]> {
        self.validity_bits
    }

    pub const fn sample_count(self) -> u64 {
        self.descriptor.sample_count()
    }

    pub const fn value_byte_len(self) -> u64 {
        self.descriptor.value_byte_len()
    }

    pub const fn validity_byte_len(self) -> u64 {
        self.descriptor.validity_byte_len()
    }

    pub const fn byte_len(self) -> u64 {
        self.descriptor.byte_len()
    }

    /// Returns whether the sample at `index` is scientifically valid.
    ///
    /// Bitmask samples use least-significant-bit-first ordering within each
    /// byte. The index is checked before the bitmask is accessed.
    pub fn sample_is_valid(self, index: u64) -> Result<bool, ResourceContractError> {
        if index >= self.sample_count() {
            return Err(ResourceContractError::SampleIndexOutOfBounds {
                index,
                sample_count: self.sample_count(),
            });
        }
        let Some(bits) = self.validity_bits else {
            return Ok(true);
        };
        let byte_index = usize::try_from(index / 8)
            .map_err(|_| ResourceContractError::PayloadByteLengthOverflow)?;
        let bit_index = u8::try_from(index % 8).expect("a remainder modulo eight fits in u8");
        Ok(bits[byte_index] & (1_u8 << bit_index) != 0)
    }
}

/// Categories shared by every producer that acquires capacity from the sole
/// CPU dataset ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CpuLedgerCategory {
    DecodedResidency,
    UploadStaging,
    InFlightDecode,
    MetadataAndIndexes,
    QueuesAndResults,
    Prefetch,
    ImportWorkingSet,
}

impl CpuLedgerCategory {
    pub const fn contract_name(self) -> &'static str {
        match self {
            Self::DecodedResidency => "cpu.decoded-residency",
            Self::UploadStaging => "cpu.upload-staging",
            Self::InFlightDecode => "cpu.in-flight-decode",
            Self::MetadataAndIndexes => "cpu.metadata-and-indexes",
            Self::QueuesAndResults => "cpu.queues-and-results",
            Self::Prefetch => "cpu.prefetch",
            Self::ImportWorkingSet => "cpu.import-working-set",
        }
    }
}

/// Framework-neutral admission failures returned by the CPU byte authority.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum CpuLedgerError {
    #[error("a CPU byte reservation must be nonzero")]
    ZeroByteReservation,
    #[error(
        "CPU capacity in {category:?} cannot satisfy {requested_bytes} bytes with {available_bytes} bytes available"
    )]
    CapacityExceeded {
        category: CpuLedgerCategory,
        requested_bytes: u64,
        available_bytes: u64,
    },
    #[error("the CPU byte authority is shutting down")]
    ShuttingDown,
}

/// Inspection-only lifetime token for one admitted CPU allocation.
///
/// The allocation owner retains this token for at least as long as the
/// charged bytes. The dataset runtime is the sole production implementation
/// and issuer; storage, import, and analysis receive the ledger by injection.
pub trait CpuByteLease: Send + Sync {
    fn category(&self) -> CpuLedgerCategory;
    fn reserved_bytes(&self) -> u64;
}

/// Dependency-inverted admission boundary for the sole CPU byte ledger.
///
/// Implementations must reject zero-byte requests, enforce the category and
/// total caps, and return a lease whose category and byte count exactly match
/// the request. Dropping the lease releases the charge.
pub trait CpuByteLedger: Send + Sync {
    fn try_acquire(
        &self,
        category: CpuLedgerCategory,
        bytes: u64,
    ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError>;
}

/// Inspection-only contract for a runtime-issued, byte-accounted lease.
///
/// This crate deliberately provides no lease constructor, issuer, owned
/// payload accessor, or accounting bypass. The dataset runtime owns concrete
/// lease issuance and lifetime; consumers only borrow the immutable view.
pub trait ResourceLease: Send + Sync {
    fn key(&self) -> DatasetResourceKey;
    fn payload(&self) -> ResourcePayloadView<'_>;
}

/// A decoded-buffer reservation owned by the dataset runtime.
///
/// A source receives this sink from its caller and writes sequential decoded
/// bytes into the already-reserved capacity. The layout is exactly
/// `descriptor.value_byte_len()` canonical value bytes followed by
/// `descriptor.validity_byte_len()` packed validity bytes; an all-valid
/// descriptor therefore has no trailing validity bytes. Implementors must
/// reject writes beyond `reserved_bytes`, writes after completion, incomplete
/// completion, and cancellation. Storage never returns an owning decoded
/// buffer.
pub trait ReservedDecodeSink {
    fn resource_key(&self) -> DatasetResourceKey;
    fn payload_descriptor(&self) -> ResourcePayloadDescriptor;
    fn reserved_bytes(&self) -> u64 {
        self.payload_descriptor().byte_len()
    }
    fn written_bytes(&self) -> u64;
    /// Returns whether the caller has cancelled this exact reservation.
    ///
    /// Sources must checkpoint this before starting decode and throughout
    /// every long read/decode stage; relying only on a rejected write is not a
    /// cancellation checkpoint.
    fn is_cancelled(&self) -> bool;
    fn write(&mut self, bytes: &[u8]) -> Result<(), DecodeSinkError>;
    fn finish(&mut self) -> Result<(), DecodeSinkError>;
    fn is_finished(&self) -> bool;
}

/// A storage-independent source that exposes one immutable catalog and
/// decodes semantic resources into caller-owned reservations.
///
/// Calls are synchronous by design: WP-08B owns the worker/scheduler context
/// in which they run. Implementations own storage discovery and codecs, but
/// may not expose their physical layout through this interface. A successful
/// decode must fill the exact descriptor and call `ReservedDecodeSink::finish`;
/// `Float32` samples must already be validated as finite.
pub trait DatasetSource: Send + Sync {
    fn catalog(&self) -> Result<Arc<DatasetCatalog>, DatasetSourceFault>;
    fn decode_into(&self, sink: &mut dyn ReservedDecodeSink) -> Result<(), DatasetSourceFault>;
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ResourceContractError {
    #[error("resource region end overflows axis {axis}")]
    RegionEndOverflow { axis: usize },
    #[error("decoded payload byte length overflows")]
    PayloadByteLengthOverflow,
    #[error("decoded value payload has {actual} bytes; expected exactly {expected}")]
    PayloadValueByteLengthMismatch { expected: u64, actual: u64 },
    #[error("an all-valid payload must not carry validity bits")]
    UnexpectedValidityBits,
    #[error("a bitmask-validity payload is missing its validity bits")]
    MissingValidityBits,
    #[error("validity bitmask has {actual} bytes; expected exactly {expected}")]
    PayloadValidityByteLengthMismatch { expected: u64, actual: u64 },
    #[error(
        "validity bitmask final byte {final_byte:#04x} has nonzero padding above its {used_bits} used bits"
    )]
    ValidityPaddingBitsNonZero { used_bits: u8, final_byte: u8 },
    #[error("sample index {index} is outside the payload's {sample_count} samples")]
    SampleIndexOutOfBounds { index: u64, sample_count: u64 },
    #[error("decode reservation descriptor does not match the catalog resource")]
    PayloadDescriptorMismatch,
    #[error("the resource key belongs to a different source identity")]
    ResourceIdentityMismatch,
    #[error("logical layer {ordinal} is absent from the catalog")]
    UnknownLayer { ordinal: u32 },
    #[error("timepoint {index} is outside the layer's {timepoints} timepoints")]
    TimepointOutOfBounds { index: u64, timepoints: u64 },
    #[error("multiscale level {level} is absent from the logical layer")]
    UnknownScale { level: u32 },
    #[error("resource region exceeds multiscale level {level} bounds")]
    RegionOutOfBounds { level: u32 },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DecodeSinkError {
    #[error("decode reservation is cancelled")]
    Cancelled,
    #[error("decoded write length overflows the byte counter")]
    ByteCountOverflow,
    #[error("decoded write would use {attempted} bytes from a {reserved}-byte reservation")]
    ReservationExceeded { reserved: u64, attempted: u64 },
    #[error("decode sink has already been completed")]
    AlreadyFinished,
    #[error("decode completed with {written} of {reserved} reserved bytes written")]
    Incomplete { reserved: u64, written: u64 },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DatasetSourceFault {
    #[error("dataset catalog is unavailable")]
    CatalogUnavailable,
    #[error("invalid semantic resource request: {reason}")]
    InvalidResource {
        key: DatasetResourceKey,
        reason: Box<ResourceContractError>,
    },
    #[error("semantic resource is unavailable")]
    ResourceUnavailable { key: DatasetResourceKey },
    #[error("semantic resource is corrupt")]
    CorruptResource { key: DatasetResourceKey },
    #[error("semantic resource uses an unsupported representation")]
    UnsupportedResource { key: DatasetResourceKey },
    #[error("semantic resource decoding was cancelled")]
    Cancelled { key: DatasetResourceKey },
    #[error("CPU capacity cannot satisfy the semantic resource reservation")]
    CapacityExceeded {
        key: DatasetResourceKey,
        category: CpuLedgerCategory,
        requested_bytes: u64,
        available_bytes: u64,
    },
    #[error("the dataset resource authority is shutting down")]
    ShuttingDown {
        key: DatasetResourceKey,
        category: CpuLedgerCategory,
        requested_bytes: u64,
    },
    #[error("semantic resource decoding failed")]
    DecodeFailed { key: DatasetResourceKey },
    #[error("decode sink rejected semantic resource: {reason}")]
    SinkRejected {
        key: DatasetResourceKey,
        reason: Box<DecodeSinkError>,
    },
}

/// Whether the catalog has been bound to verified scientific content.
///
/// `Unverified` carries only the opaque identity of this exact open. In
/// particular, a package slug, path, manifest value, or cache digest cannot be
/// represented as a verified scientific identity through this type.
///
/// This checkpoint-A value records a classification; constructing it is not a
/// verifier capability and does not authorize application attachment. The
/// verifier-owned admission route is introduced by WP-08, while the WP-07B
/// application boundary intentionally exposes no public verification command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScientificIdentityStatus {
    Unverified(DatasetSourceId),
    Verified(ScientificContentId),
}

impl ScientificIdentityStatus {
    pub const fn is_verified(&self) -> bool {
        matches!(self, Self::Verified(_))
    }

    pub const fn verified_id(&self) -> Option<&ScientificContentId> {
        match self {
            Self::Unverified(_) => None,
            Self::Verified(identity) => Some(identity),
        }
    }

    pub const fn source_id(&self) -> Option<DatasetSourceId> {
        match self {
            Self::Unverified(source_id) => Some(*source_id),
            Self::Verified(_) => None,
        }
    }

    pub const fn resource_identity(&self) -> DatasetResourceIdentity {
        match self {
            Self::Unverified(source_id) => DatasetResourceIdentity::Unverified(*source_id),
            Self::Verified(identity) => DatasetResourceIdentity::Verified(*identity),
        }
    }
}

/// Shape and transform for one semantic multiscale level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DatasetScale {
    level: ScaleLevel,
    shape: Shape3D,
    grid_to_world: GridToWorld,
    validity: ResourceValidity,
}

impl DatasetScale {
    pub const fn new(
        level: ScaleLevel,
        shape: Shape3D,
        grid_to_world: GridToWorld,
        validity: ResourceValidity,
    ) -> Self {
        Self {
            level,
            shape,
            grid_to_world,
            validity,
        }
    }

    pub const fn level(self) -> ScaleLevel {
        self.level
    }

    pub const fn shape(self) -> Shape3D {
        self.shape
    }

    pub const fn grid_to_world(self) -> GridToWorld {
        self.grid_to_world
    }

    pub const fn validity(self) -> ResourceValidity {
        self.validity
    }
}

/// Immutable scientific and display-label facts for one logical layer.
#[derive(Debug, Clone, PartialEq)]
pub struct DatasetLayer {
    key: LogicalLayerKey,
    label: String,
    shape: Shape4D,
    dtype: IntensityDType,
    grid_to_world: GridToWorld,
    scales: BTreeMap<ScaleLevel, DatasetScale>,
}

impl DatasetLayer {
    pub fn new(
        key: LogicalLayerKey,
        label: impl AsRef<str>,
        shape: Shape4D,
        dtype: IntensityDType,
        grid_to_world: GridToWorld,
        validity: ResourceValidity,
    ) -> Result<Self, DatasetCatalogError> {
        Self::new_multiscale(
            key,
            label,
            shape.t(),
            dtype,
            vec![DatasetScale::new(
                ScaleLevel::BASE,
                shape.spatial(),
                grid_to_world,
                validity,
            )],
        )
    }

    pub fn new_multiscale(
        key: LogicalLayerKey,
        label: impl AsRef<str>,
        timepoints: u64,
        dtype: IntensityDType,
        scales: Vec<DatasetScale>,
    ) -> Result<Self, DatasetCatalogError> {
        let label = validate_label("layer label", label.as_ref(), MAX_LAYER_LABEL_BYTES)?;
        if scales.is_empty() {
            return Err(DatasetCatalogError::EmptyScaleCatalog);
        }
        if scales.len() > MAX_SCALES_PER_LAYER {
            return Err(DatasetCatalogError::TooManyScales {
                actual: scales.len(),
                maximum: MAX_SCALES_PER_LAYER,
            });
        }

        let mut by_level = BTreeMap::new();
        for scale in scales {
            let level = scale.level();
            if by_level.insert(level, scale).is_some() {
                return Err(DatasetCatalogError::DuplicateScaleLevel { level: level.get() });
            }
        }

        let base = by_level
            .get(&ScaleLevel::BASE)
            .copied()
            .ok_or(DatasetCatalogError::MissingBaseScale)?;
        let shape = Shape4D::new(
            timepoints,
            base.shape().z(),
            base.shape().y(),
            base.shape().x(),
        )
        .map_err(|reason| DatasetCatalogError::InvalidLayerShape { reason })?;

        for scale in by_level.values() {
            if !ResourceRegion::new([0; 3], scale.shape())
                .expect("a validated shape at the origin cannot overflow")
                .fits_within(base.shape())
            {
                return Err(DatasetCatalogError::ScaleExceedsBaseShape {
                    level: scale.level().get(),
                });
            }
        }

        Ok(Self {
            key,
            label,
            shape,
            dtype,
            grid_to_world: base.grid_to_world(),
            scales: by_level,
        })
    }

    pub const fn key(&self) -> LogicalLayerKey {
        self.key
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub const fn shape(&self) -> Shape4D {
        self.shape
    }

    pub const fn dtype(&self) -> IntensityDType {
        self.dtype
    }

    pub const fn grid_to_world(&self) -> GridToWorld {
        self.grid_to_world
    }

    pub fn scale(&self, level: ScaleLevel) -> Option<&DatasetScale> {
        self.scales.get(&level)
    }

    pub fn validity(&self, level: ScaleLevel) -> Option<ResourceValidity> {
        self.scale(level).map(|scale| scale.validity())
    }

    pub fn scales(&self) -> impl ExactSizeIterator<Item = &DatasetScale> {
        self.scales.values()
    }
}

/// A bounded catalog keyed only by canonical logical-layer keys.
#[derive(Debug, Clone, PartialEq)]
pub struct DatasetCatalog {
    label: String,
    scientific_identity: ScientificIdentityStatus,
    layers: BTreeMap<LogicalLayerKey, DatasetLayer>,
}

impl DatasetCatalog {
    pub fn new(
        label: impl AsRef<str>,
        scientific_identity: ScientificIdentityStatus,
        layers: Vec<DatasetLayer>,
    ) -> Result<Self, DatasetCatalogError> {
        let label = validate_label("dataset label", label.as_ref(), MAX_DATASET_LABEL_BYTES)?;
        if layers.is_empty() {
            return Err(DatasetCatalogError::EmptyCatalog);
        }
        if layers.len() > MAX_DATASET_LAYERS {
            return Err(DatasetCatalogError::TooManyLayers {
                actual: layers.len(),
                maximum: MAX_DATASET_LAYERS,
            });
        }

        let mut by_key = BTreeMap::new();
        for layer in layers {
            let key = layer.key();
            if by_key.insert(key, layer).is_some() {
                return Err(DatasetCatalogError::DuplicateLayerKey {
                    ordinal: key.ordinal(),
                });
            }
        }

        Ok(Self {
            label,
            scientific_identity,
            layers: by_key,
        })
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub const fn scientific_identity(&self) -> &ScientificIdentityStatus {
        &self.scientific_identity
    }

    pub fn len(&self) -> usize {
        self.layers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    pub fn layer(&self, key: LogicalLayerKey) -> Option<&DatasetLayer> {
        self.layers.get(&key)
    }

    /// Validates a semantic key against this immutable catalog and returns the
    /// layer dtype needed to reserve and interpret its decoded payload.
    pub fn validate_resource_key(
        &self,
        key: DatasetResourceKey,
    ) -> Result<IntensityDType, ResourceContractError> {
        if self.scientific_identity.resource_identity() != key.identity() {
            return Err(ResourceContractError::ResourceIdentityMismatch);
        }

        let layer = self
            .layer(key.layer())
            .ok_or(ResourceContractError::UnknownLayer {
                ordinal: key.layer().ordinal(),
            })?;
        if key.timepoint().get() >= layer.shape().t() {
            return Err(ResourceContractError::TimepointOutOfBounds {
                index: key.timepoint().get(),
                timepoints: layer.shape().t(),
            });
        }
        let scale = layer
            .scale(key.scale())
            .ok_or(ResourceContractError::UnknownScale {
                level: key.scale().get(),
            })?;
        if !key.region().fits_within(scale.shape()) {
            return Err(ResourceContractError::RegionOutOfBounds {
                level: key.scale().get(),
            });
        }
        Ok(layer.dtype())
    }

    pub fn resource_payload_descriptor(
        &self,
        key: DatasetResourceKey,
    ) -> Result<ResourcePayloadDescriptor, ResourceContractError> {
        let dtype = self.validate_resource_key(key)?;
        let validity = self
            .layer(key.layer())
            .and_then(|layer| layer.validity(key.scale()))
            .expect("resource-key validation proves the layer and scale exist");
        ResourcePayloadDescriptor::new(dtype, key.region().shape(), validity)
    }

    pub fn resource_validity(
        &self,
        key: DatasetResourceKey,
    ) -> Result<ResourceValidity, ResourceContractError> {
        self.validate_resource_key(key)?;
        Ok(self
            .layer(key.layer())
            .and_then(|layer| layer.validity(key.scale()))
            .expect("resource-key validation proves the layer and scale exist"))
    }

    pub fn validate_decode_reservation(
        &self,
        sink: &dyn ReservedDecodeSink,
    ) -> Result<ResourcePayloadDescriptor, ResourceContractError> {
        let expected = self.resource_payload_descriptor(sink.resource_key())?;
        if expected != sink.payload_descriptor() {
            return Err(ResourceContractError::PayloadDescriptorMismatch);
        }
        Ok(expected)
    }

    /// Iterates in ascending `LogicalLayerKey` order, independent of input
    /// order or duplicate human-readable labels.
    pub fn layers(&self) -> impl ExactSizeIterator<Item = &DatasetLayer> {
        self.layers.values()
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DatasetCatalogError {
    #[error("{kind} must not be empty")]
    EmptyLabel { kind: &'static str },
    #[error("{kind} exceeds {maximum} UTF-8 bytes")]
    LabelTooLong { kind: &'static str, maximum: usize },
    #[error("{kind} contains a control character")]
    LabelContainsControl { kind: &'static str },
    #[error("a dataset catalog must contain at least one logical layer")]
    EmptyCatalog,
    #[error("dataset catalog contains {actual} layers, exceeding the limit of {maximum}")]
    TooManyLayers { actual: usize, maximum: usize },
    #[error("logical layer key {ordinal} occurs more than once")]
    DuplicateLayerKey { ordinal: u32 },
    #[error("a logical layer must describe at least its base scale")]
    EmptyScaleCatalog,
    #[error("logical layer contains {actual} scales, exceeding the limit of {maximum}")]
    TooManyScales { actual: usize, maximum: usize },
    #[error("multiscale level {level} occurs more than once")]
    DuplicateScaleLevel { level: u32 },
    #[error("logical layer is missing multiscale level zero")]
    MissingBaseScale,
    #[error("logical layer shape is invalid: {reason}")]
    InvalidLayerShape { reason: ShapeError },
    #[error("multiscale level {level} exceeds the base-scale shape")]
    ScaleExceedsBaseShape { level: u32 },
}

fn validate_label(
    kind: &'static str,
    value: &str,
    maximum: usize,
) -> Result<String, DatasetCatalogError> {
    if value.trim().is_empty() {
        return Err(DatasetCatalogError::EmptyLabel { kind });
    }
    if value.len() > maximum {
        return Err(DatasetCatalogError::LabelTooLong { kind, maximum });
    }
    if value.chars().any(char::is_control) {
        return Err(DatasetCatalogError::LabelContainsControl { kind });
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZERO_SCIENTIFIC_ID: &str =
        "m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000";

    const fn source_id() -> DatasetSourceId {
        DatasetSourceId::new(1)
    }

    fn layer(key: u32, label: &str) -> DatasetLayer {
        DatasetLayer::new(
            LogicalLayerKey::new(key),
            label,
            Shape4D::new(3, 5, 7, 11).unwrap(),
            IntensityDType::Uint16,
            GridToWorld::scale(0.5, 0.75, 2.0).unwrap(),
            ResourceValidity::AllValid,
        )
        .unwrap()
    }

    #[test]
    fn catalog_is_keyed_and_iterated_by_logical_layer_key() {
        let catalog = DatasetCatalog::new(
            "experiment",
            ScientificIdentityStatus::Unverified(source_id()),
            vec![layer(7, "green"), layer(2, "red"), layer(5, "green")],
        )
        .unwrap();

        assert_eq!(catalog.len(), 3);
        assert_eq!(
            catalog.layer(LogicalLayerKey::new(2)).unwrap().label(),
            "red"
        );
        assert_eq!(
            catalog.layers().map(DatasetLayer::key).collect::<Vec<_>>(),
            vec![
                LogicalLayerKey::new(2),
                LogicalLayerKey::new(5),
                LogicalLayerKey::new(7),
            ]
        );
    }

    #[test]
    fn duplicate_human_labels_are_not_identity_but_duplicate_keys_reject() {
        assert!(
            DatasetCatalog::new(
                "experiment",
                ScientificIdentityStatus::Unverified(source_id()),
                vec![layer(0, "channel"), layer(1, "channel")],
            )
            .is_ok()
        );

        assert_eq!(
            DatasetCatalog::new(
                "experiment",
                ScientificIdentityStatus::Unverified(source_id()),
                vec![layer(3, "first"), layer(3, "second")],
            ),
            Err(DatasetCatalogError::DuplicateLayerKey { ordinal: 3 })
        );
    }

    #[test]
    fn identity_status_cannot_confuse_unverified_catalogs_with_verified_content() {
        let unverified = DatasetCatalog::new(
            "experiment",
            ScientificIdentityStatus::Unverified(source_id()),
            vec![layer(0, "channel")],
        )
        .unwrap();
        assert!(!unverified.scientific_identity().is_verified());
        assert_eq!(unverified.scientific_identity().verified_id(), None);

        let identity = ScientificContentId::parse(ZERO_SCIENTIFIC_ID).unwrap();
        let verified = DatasetCatalog::new(
            "experiment",
            ScientificIdentityStatus::Verified(identity),
            vec![layer(0, "channel")],
        )
        .unwrap();
        assert_eq!(
            verified.scientific_identity().verified_id(),
            Some(&identity)
        );
    }

    #[test]
    fn layer_preserves_canonical_scientific_facts() {
        let layer = layer(4, "channel");
        assert_eq!(layer.key(), LogicalLayerKey::new(4));
        assert_eq!(layer.shape().dimensions(), [3, 5, 7, 11]);
        assert_eq!(layer.dtype(), IntensityDType::Uint16);
        assert_eq!(
            layer.validity(ScaleLevel::BASE),
            Some(ResourceValidity::AllValid)
        );
        assert_eq!(
            layer.grid_to_world().row_major(),
            GridToWorld::scale(0.5, 0.75, 2.0).unwrap().row_major()
        );
    }

    #[test]
    fn catalog_and_labels_are_bounded_before_collection() {
        assert_eq!(
            DatasetCatalog::new(
                "experiment",
                ScientificIdentityStatus::Unverified(source_id()),
                Vec::new(),
            ),
            Err(DatasetCatalogError::EmptyCatalog)
        );
        assert_eq!(
            DatasetCatalog::new(
                " ",
                ScientificIdentityStatus::Unverified(source_id()),
                vec![layer(0, "channel")],
            ),
            Err(DatasetCatalogError::EmptyLabel {
                kind: "dataset label"
            })
        );
        assert_eq!(
            DatasetLayer::new(
                LogicalLayerKey::new(0),
                "bad\nlabel",
                Shape4D::new(1, 1, 1, 1).unwrap(),
                IntensityDType::Uint8,
                GridToWorld::identity(),
                ResourceValidity::AllValid,
            ),
            Err(DatasetCatalogError::LabelContainsControl {
                kind: "layer label"
            })
        );

        let oversized = "x".repeat(MAX_DATASET_LABEL_BYTES + 1);
        assert_eq!(
            DatasetCatalog::new(
                oversized,
                ScientificIdentityStatus::Unverified(source_id()),
                vec![layer(0, "channel")],
            ),
            Err(DatasetCatalogError::LabelTooLong {
                kind: "dataset label",
                maximum: MAX_DATASET_LABEL_BYTES,
            })
        );

        let layers = (0..=MAX_DATASET_LAYERS)
            .map(|key| layer(u32::try_from(key).unwrap(), "channel"))
            .collect();
        assert_eq!(
            DatasetCatalog::new(
                "experiment",
                ScientificIdentityStatus::Unverified(source_id()),
                layers,
            ),
            Err(DatasetCatalogError::TooManyLayers {
                actual: MAX_DATASET_LAYERS + 1,
                maximum: MAX_DATASET_LAYERS,
            })
        );
    }

    fn scientific_id(fill: char) -> ScientificContentId {
        ScientificContentId::parse(&format!("m4d-sc-v1-sha256:{}", fill.to_string().repeat(64)))
            .unwrap()
    }

    fn multiscale_catalog(identity: ScientificContentId) -> Arc<DatasetCatalog> {
        let layer = DatasetLayer::new_multiscale(
            LogicalLayerKey::new(3),
            "channel",
            3,
            IntensityDType::Uint16,
            vec![
                DatasetScale::new(
                    ScaleLevel::BASE,
                    Shape3D::new(4, 6, 8).unwrap(),
                    GridToWorld::identity(),
                    ResourceValidity::AllValid,
                ),
                DatasetScale::new(
                    ScaleLevel::new(1),
                    Shape3D::new(2, 3, 4).unwrap(),
                    GridToWorld::scale(2.0, 2.0, 2.0).unwrap(),
                    ResourceValidity::BitMask,
                ),
            ],
        )
        .unwrap();
        Arc::new(
            DatasetCatalog::new(
                "experiment",
                ScientificIdentityStatus::Verified(identity),
                vec![layer],
            )
            .unwrap(),
        )
    }

    fn resource_key(identity: ScientificContentId) -> DatasetResourceKey {
        DatasetResourceKey::new(
            DatasetResourceIdentity::Verified(identity),
            LogicalLayerKey::new(3),
            TimeIndex::new(1),
            ScaleLevel::new(1),
            ResourceRegion::new([0, 1, 1], Shape3D::new(1, 1, 2).unwrap()).unwrap(),
        )
    }

    #[test]
    fn semantic_resource_keys_are_stable_hashable_and_storage_independent() {
        use std::collections::HashSet;

        let identity = scientific_id('1');
        let key = resource_key(identity);
        let equal = resource_key(identity);
        let different_scale = DatasetResourceKey::new(
            DatasetResourceIdentity::Verified(identity),
            key.layer(),
            key.timepoint(),
            ScaleLevel::BASE,
            key.region(),
        );

        assert_eq!(key, equal);
        assert_ne!(key, different_scale);
        assert_eq!(HashSet::from([key, equal]).len(), 1);
        assert_eq!(key.identity(), DatasetResourceIdentity::Verified(identity));
        assert_eq!(key.region().origin(), [0, 1, 1]);
        assert_eq!(key.region().end_exclusive(), [1, 2, 3]);
    }

    #[test]
    fn resource_regions_reject_overflowing_exclusive_ends() {
        assert_eq!(
            ResourceRegion::new([0, 0, u64::MAX], Shape3D::new(1, 1, 2).unwrap(),),
            Err(ResourceContractError::RegionEndOverflow { axis: 2 })
        );
    }

    #[test]
    fn payload_descriptor_bounds_value_validity_and_total_bytes_exactly() {
        let nine_samples = Shape3D::new(1, 1, 9).unwrap();
        let all_valid = ResourcePayloadDescriptor::new(
            IntensityDType::Uint16,
            nine_samples,
            ResourceValidity::AllValid,
        )
        .unwrap();
        assert_eq!(all_valid.sample_count(), 9);
        assert_eq!(all_valid.value_byte_len(), 18);
        assert_eq!(all_valid.validity_byte_len(), 0);
        assert_eq!(all_valid.byte_len(), 18);

        let bitmask = ResourcePayloadDescriptor::new(
            IntensityDType::Uint16,
            nine_samples,
            ResourceValidity::BitMask,
        )
        .unwrap();
        assert_eq!(bitmask.value_byte_len(), 18);
        assert_eq!(bitmask.validity_byte_len(), 2);
        assert_eq!(bitmask.byte_len(), 20);

        let eight_samples = ResourcePayloadDescriptor::new(
            IntensityDType::Uint8,
            Shape3D::new(1, 1, 8).unwrap(),
            ResourceValidity::BitMask,
        )
        .unwrap();
        assert_eq!(eight_samples.validity_byte_len(), 1);
        assert_eq!(eight_samples.byte_len(), 9);

        let maximum_samples = Shape3D::new(1, 1, u64::MAX).unwrap();
        assert_eq!(
            ResourcePayloadDescriptor::new(
                IntensityDType::Uint16,
                maximum_samples,
                ResourceValidity::AllValid,
            ),
            Err(ResourceContractError::PayloadByteLengthOverflow)
        );
        assert_eq!(
            ResourcePayloadDescriptor::new(
                IntensityDType::Uint8,
                maximum_samples,
                ResourceValidity::BitMask,
            ),
            Err(ResourceContractError::PayloadByteLengthOverflow)
        );
    }

    #[test]
    fn all_valid_payload_keeps_valid_zero_and_rejects_any_mask() {
        let values = [0_u8, 17];
        let view = ResourcePayloadView::new(
            IntensityDType::Uint8,
            Shape3D::new(1, 1, 2).unwrap(),
            ResourceValidity::AllValid,
            &values,
            None,
        )
        .unwrap();
        assert_eq!(view.dtype(), IntensityDType::Uint8);
        assert_eq!(view.validity(), ResourceValidity::AllValid);
        assert_eq!(view.value_bytes().as_ptr(), values.as_ptr());
        assert_eq!(view.validity_bits(), None);
        assert_eq!(view.value_byte_len(), 2);
        assert_eq!(view.validity_byte_len(), 0);
        assert_eq!(view.byte_len(), 2);
        assert_eq!(view.sample_is_valid(0), Ok(true));
        assert_eq!(values[0], 0, "a valid zero remains scientific data");
        assert_eq!(view.sample_is_valid(1), Ok(true));
        assert_eq!(
            view.sample_is_valid(2),
            Err(ResourceContractError::SampleIndexOutOfBounds {
                index: 2,
                sample_count: 2,
            })
        );

        assert_eq!(
            ResourcePayloadView::new(
                IntensityDType::Uint8,
                Shape3D::new(1, 1, 2).unwrap(),
                ResourceValidity::AllValid,
                &values,
                Some(&[]),
            ),
            Err(ResourceContractError::UnexpectedValidityBits)
        );
    }

    #[test]
    fn bitmask_payload_supports_mixed_and_all_invalid_samples_lsb_first() {
        let values = [0_u8; 9];
        let mixed_bits = [0b1000_0101, 0b0000_0001];
        let mixed = ResourcePayloadView::new(
            IntensityDType::Uint8,
            Shape3D::new(1, 1, 9).unwrap(),
            ResourceValidity::BitMask,
            &values,
            Some(&mixed_bits),
        )
        .unwrap();
        assert_eq!(mixed.validity_bits(), Some(mixed_bits.as_slice()));
        assert_eq!(mixed.value_byte_len(), 9);
        assert_eq!(mixed.validity_byte_len(), 2);
        assert_eq!(mixed.byte_len(), 11);
        assert_eq!(mixed.sample_is_valid(0), Ok(true));
        assert_eq!(values[0], 0, "the explicitly valid sample may be zero");
        assert_eq!(mixed.sample_is_valid(1), Ok(false));
        assert_eq!(mixed.sample_is_valid(2), Ok(true));
        assert_eq!(mixed.sample_is_valid(7), Ok(true));
        assert_eq!(mixed.sample_is_valid(8), Ok(true));

        let all_invalid_bits = [0_u8; 2];
        let all_invalid = ResourcePayloadView::new(
            IntensityDType::Uint8,
            Shape3D::new(1, 1, 9).unwrap(),
            ResourceValidity::BitMask,
            &values,
            Some(&all_invalid_bits),
        )
        .unwrap();
        assert!((0..9).all(|index| all_invalid.sample_is_valid(index) == Ok(false)));
    }

    #[test]
    fn payload_view_rejects_noncanonical_masks_and_inexact_lengths() {
        let shape = Shape3D::new(1, 1, 10).unwrap();
        let values = [0_u8; 10];

        assert_eq!(
            ResourcePayloadView::new(
                IntensityDType::Uint8,
                shape,
                ResourceValidity::BitMask,
                &values,
                Some(&[0, 0b0000_0100]),
            ),
            Err(ResourceContractError::ValidityPaddingBitsNonZero {
                used_bits: 2,
                final_byte: 0b0000_0100,
            })
        );
        assert_eq!(
            ResourcePayloadView::new(
                IntensityDType::Uint8,
                shape,
                ResourceValidity::BitMask,
                &values,
                None,
            ),
            Err(ResourceContractError::MissingValidityBits)
        );
        assert_eq!(
            ResourcePayloadView::new(
                IntensityDType::Uint8,
                shape,
                ResourceValidity::BitMask,
                &values,
                Some(&[0]),
            ),
            Err(ResourceContractError::PayloadValidityByteLengthMismatch {
                expected: 2,
                actual: 1,
            })
        );
        assert_eq!(
            ResourcePayloadView::new(
                IntensityDType::Uint8,
                shape,
                ResourceValidity::BitMask,
                &values[..9],
                Some(&[0, 0]),
            ),
            Err(ResourceContractError::PayloadValueByteLengthMismatch {
                expected: 10,
                actual: 9,
            })
        );
    }

    #[test]
    fn multiscale_catalog_bounds_and_validates_semantic_requests() {
        let identity = scientific_id('2');
        let catalog = multiscale_catalog(identity);
        let key = resource_key(identity);
        assert_eq!(
            catalog.validate_resource_key(key),
            Ok(IntensityDType::Uint16)
        );
        assert_eq!(
            catalog.resource_validity(key),
            Ok(ResourceValidity::BitMask)
        );
        let descriptor = catalog.resource_payload_descriptor(key).unwrap();
        assert_eq!(descriptor.validity(), ResourceValidity::BitMask);
        assert_eq!(descriptor.value_byte_len(), 4);
        assert_eq!(descriptor.validity_byte_len(), 1);
        assert_eq!(descriptor.byte_len(), 5);
        assert_eq!(
            catalog
                .layer(LogicalLayerKey::new(3))
                .unwrap()
                .scales()
                .map(|scale| scale.level())
                .collect::<Vec<_>>(),
            vec![ScaleLevel::BASE, ScaleLevel::new(1)]
        );

        let base_key = DatasetResourceKey::new(
            DatasetResourceIdentity::Verified(identity),
            key.layer(),
            key.timepoint(),
            ScaleLevel::BASE,
            key.region(),
        );
        assert_eq!(
            catalog.resource_validity(base_key),
            Ok(ResourceValidity::AllValid)
        );

        let wrong_identity = DatasetResourceKey::new(
            DatasetResourceIdentity::Verified(scientific_id('3')),
            key.layer(),
            key.timepoint(),
            key.scale(),
            key.region(),
        );
        assert_eq!(
            catalog.validate_resource_key(wrong_identity),
            Err(ResourceContractError::ResourceIdentityMismatch)
        );

        let unverified = DatasetCatalog::new(
            "experiment",
            ScientificIdentityStatus::Unverified(source_id()),
            vec![layer(3, "channel")],
        )
        .unwrap();
        assert_eq!(
            unverified.validate_resource_key(key),
            Err(ResourceContractError::ResourceIdentityMismatch)
        );

        let late = DatasetResourceKey::new(
            DatasetResourceIdentity::Verified(identity),
            key.layer(),
            TimeIndex::new(3),
            key.scale(),
            key.region(),
        );
        assert_eq!(
            catalog.validate_resource_key(late),
            Err(ResourceContractError::TimepointOutOfBounds {
                index: 3,
                timepoints: 3,
            })
        );

        let unknown_layer = DatasetResourceKey::new(
            DatasetResourceIdentity::Verified(identity),
            LogicalLayerKey::new(99),
            key.timepoint(),
            key.scale(),
            key.region(),
        );
        assert_eq!(
            catalog.validate_resource_key(unknown_layer),
            Err(ResourceContractError::UnknownLayer { ordinal: 99 })
        );

        let unknown_scale = DatasetResourceKey::new(
            DatasetResourceIdentity::Verified(identity),
            key.layer(),
            key.timepoint(),
            ScaleLevel::new(99),
            key.region(),
        );
        assert_eq!(
            catalog.validate_resource_key(unknown_scale),
            Err(ResourceContractError::UnknownScale { level: 99 })
        );

        let outside = DatasetResourceKey::new(
            DatasetResourceIdentity::Verified(identity),
            key.layer(),
            key.timepoint(),
            key.scale(),
            ResourceRegion::new([1, 2, 3], Shape3D::new(2, 2, 2).unwrap()).unwrap(),
        );
        assert_eq!(
            catalog.validate_resource_key(outside),
            Err(ResourceContractError::RegionOutOfBounds { level: 1 })
        );
    }

    #[test]
    fn multiscale_layer_validation_is_bounded_before_collection() {
        assert_eq!(
            DatasetLayer::new_multiscale(
                LogicalLayerKey::new(0),
                "channel",
                1,
                IntensityDType::Uint8,
                Vec::new(),
            ),
            Err(DatasetCatalogError::EmptyScaleCatalog)
        );
        assert_eq!(
            DatasetLayer::new_multiscale(
                LogicalLayerKey::new(0),
                "channel",
                1,
                IntensityDType::Uint8,
                vec![DatasetScale::new(
                    ScaleLevel::new(1),
                    Shape3D::new(1, 1, 1).unwrap(),
                    GridToWorld::identity(),
                    ResourceValidity::AllValid,
                )],
            ),
            Err(DatasetCatalogError::MissingBaseScale)
        );

        let base = DatasetScale::new(
            ScaleLevel::BASE,
            Shape3D::new(2, 2, 2).unwrap(),
            GridToWorld::identity(),
            ResourceValidity::AllValid,
        );
        assert_eq!(
            DatasetLayer::new_multiscale(
                LogicalLayerKey::new(0),
                "channel",
                1,
                IntensityDType::Uint8,
                vec![base, base],
            ),
            Err(DatasetCatalogError::DuplicateScaleLevel { level: 0 })
        );
        assert_eq!(
            DatasetLayer::new_multiscale(
                LogicalLayerKey::new(0),
                "channel",
                1,
                IntensityDType::Uint8,
                vec![
                    base,
                    DatasetScale::new(
                        ScaleLevel::new(1),
                        Shape3D::new(3, 2, 2).unwrap(),
                        GridToWorld::identity(),
                        ResourceValidity::AllValid,
                    ),
                ],
            ),
            Err(DatasetCatalogError::ScaleExceedsBaseShape { level: 1 })
        );

        let scales = (0..=MAX_SCALES_PER_LAYER)
            .map(|level| {
                DatasetScale::new(
                    ScaleLevel::new(u32::try_from(level).unwrap()),
                    Shape3D::new(1, 1, 1).unwrap(),
                    GridToWorld::identity(),
                    ResourceValidity::AllValid,
                )
            })
            .collect();
        assert_eq!(
            DatasetLayer::new_multiscale(
                LogicalLayerKey::new(0),
                "channel",
                1,
                IntensityDType::Uint8,
                scales,
            ),
            Err(DatasetCatalogError::TooManyScales {
                actual: MAX_SCALES_PER_LAYER + 1,
                maximum: MAX_SCALES_PER_LAYER,
            })
        );
    }

    struct TestSink {
        key: DatasetResourceKey,
        descriptor: ResourcePayloadDescriptor,
        bytes: Box<[u8]>,
        written: usize,
        write_calls: usize,
        finished: bool,
        cancelled: bool,
    }

    impl TestSink {
        fn new(key: DatasetResourceKey, descriptor: ResourcePayloadDescriptor) -> Self {
            let byte_len = usize::try_from(descriptor.byte_len()).unwrap();
            Self {
                key,
                descriptor,
                bytes: vec![0; byte_len].into_boxed_slice(),
                written: 0,
                write_calls: 0,
                finished: false,
                cancelled: false,
            }
        }

        fn payload_view(&self) -> Result<ResourcePayloadView<'_>, ResourceContractError> {
            let value_end = usize::try_from(self.descriptor.value_byte_len())
                .expect("the test reservation fits in addressable memory");
            let (value_bytes, validity_bytes) = self.bytes.split_at(value_end);
            let validity_bits = match self.descriptor.validity() {
                ResourceValidity::AllValid => None,
                ResourceValidity::BitMask => Some(validity_bytes),
            };
            self.descriptor.view(value_bytes, validity_bits)
        }
    }

    impl ReservedDecodeSink for TestSink {
        fn resource_key(&self) -> DatasetResourceKey {
            self.key
        }

        fn payload_descriptor(&self) -> ResourcePayloadDescriptor {
            self.descriptor
        }

        fn written_bytes(&self) -> u64 {
            u64::try_from(self.written).unwrap()
        }

        fn is_cancelled(&self) -> bool {
            self.cancelled
        }

        fn write(&mut self, bytes: &[u8]) -> Result<(), DecodeSinkError> {
            self.write_calls += 1;
            if self.cancelled {
                return Err(DecodeSinkError::Cancelled);
            }
            if self.finished {
                return Err(DecodeSinkError::AlreadyFinished);
            }
            let byte_count =
                u64::try_from(bytes.len()).map_err(|_| DecodeSinkError::ByteCountOverflow)?;
            let attempted = self
                .written_bytes()
                .checked_add(byte_count)
                .ok_or(DecodeSinkError::ByteCountOverflow)?;
            if attempted > self.reserved_bytes() {
                return Err(DecodeSinkError::ReservationExceeded {
                    reserved: self.reserved_bytes(),
                    attempted,
                });
            }
            let end = usize::try_from(attempted).map_err(|_| DecodeSinkError::ByteCountOverflow)?;
            self.bytes[self.written..end].copy_from_slice(bytes);
            self.written = end;
            Ok(())
        }

        fn finish(&mut self) -> Result<(), DecodeSinkError> {
            if self.cancelled {
                return Err(DecodeSinkError::Cancelled);
            }
            if self.finished {
                return Err(DecodeSinkError::AlreadyFinished);
            }
            let written = self.written_bytes();
            if written != self.reserved_bytes() {
                return Err(DecodeSinkError::Incomplete {
                    reserved: self.reserved_bytes(),
                    written,
                });
            }
            self.finished = true;
            Ok(())
        }

        fn is_finished(&self) -> bool {
            self.finished
        }
    }

    fn sink_fault(key: DatasetResourceKey, reason: DecodeSinkError) -> DatasetSourceFault {
        match reason {
            DecodeSinkError::Cancelled => DatasetSourceFault::Cancelled { key },
            reason => DatasetSourceFault::SinkRejected {
                key,
                reason: Box::new(reason),
            },
        }
    }

    struct TableSource {
        catalog: Arc<DatasetCatalog>,
        resources: BTreeMap<DatasetResourceKey, Arc<[u8]>>,
    }

    impl DatasetSource for TableSource {
        fn catalog(&self) -> Result<Arc<DatasetCatalog>, DatasetSourceFault> {
            Ok(Arc::clone(&self.catalog))
        }

        fn decode_into(&self, sink: &mut dyn ReservedDecodeSink) -> Result<(), DatasetSourceFault> {
            let key = sink.resource_key();
            if sink.is_cancelled() {
                return Err(DatasetSourceFault::Cancelled { key });
            }
            self.catalog
                .validate_decode_reservation(sink)
                .map_err(|reason| DatasetSourceFault::InvalidResource {
                    key,
                    reason: Box::new(reason),
                })?;
            let bytes = self
                .resources
                .get(&key)
                .ok_or(DatasetSourceFault::ResourceUnavailable { key })?;
            for part in bytes.chunks(3) {
                if sink.is_cancelled() {
                    return Err(DatasetSourceFault::Cancelled { key });
                }
                sink.write(part).map_err(|reason| sink_fault(key, reason))?;
            }
            if sink.is_cancelled() {
                return Err(DatasetSourceFault::Cancelled { key });
            }
            sink.finish().map_err(|reason| sink_fault(key, reason))
        }
    }

    struct FormulaSource {
        catalog: Arc<DatasetCatalog>,
        fill: u8,
    }

    impl DatasetSource for FormulaSource {
        fn catalog(&self) -> Result<Arc<DatasetCatalog>, DatasetSourceFault> {
            Ok(Arc::clone(&self.catalog))
        }

        fn decode_into(&self, sink: &mut dyn ReservedDecodeSink) -> Result<(), DatasetSourceFault> {
            let key = sink.resource_key();
            if sink.is_cancelled() {
                return Err(DatasetSourceFault::Cancelled { key });
            }
            let descriptor = self
                .catalog
                .validate_decode_reservation(sink)
                .map_err(|reason| DatasetSourceFault::InvalidResource {
                    key,
                    reason: Box::new(reason),
                })?;
            let mut remaining = descriptor.value_byte_len();
            let block = [self.fill; 64];
            while remaining > 0 {
                if sink.is_cancelled() {
                    return Err(DatasetSourceFault::Cancelled { key });
                }
                let count = usize::try_from(remaining.min(block.len() as u64))
                    .map_err(|_| DatasetSourceFault::DecodeFailed { key })?;
                sink.write(&block[..count])
                    .map_err(|reason| sink_fault(key, reason))?;
                remaining -=
                    u64::try_from(count).map_err(|_| DatasetSourceFault::DecodeFailed { key })?;
            }

            let mut remaining = descriptor.validity_byte_len();
            let full_block = [u8::MAX; 64];
            while remaining > 0 {
                if sink.is_cancelled() {
                    return Err(DatasetSourceFault::Cancelled { key });
                }
                let count = usize::try_from(remaining.min(full_block.len() as u64))
                    .map_err(|_| DatasetSourceFault::DecodeFailed { key })?;
                let is_final_write = remaining == u64::try_from(count).unwrap();
                if is_final_write && descriptor.sample_count() % 8 != 0 {
                    let mut final_block = [u8::MAX; 64];
                    let used_bits = u8::try_from(descriptor.sample_count() % 8)
                        .expect("a remainder modulo eight fits in u8");
                    final_block[count - 1] = (1_u8 << used_bits) - 1;
                    sink.write(&final_block[..count])
                        .map_err(|reason| sink_fault(key, reason))?;
                } else {
                    sink.write(&full_block[..count])
                        .map_err(|reason| sink_fault(key, reason))?;
                }
                remaining -=
                    u64::try_from(count).map_err(|_| DatasetSourceFault::DecodeFailed { key })?;
            }
            if sink.is_cancelled() {
                return Err(DatasetSourceFault::Cancelled { key });
            }
            sink.finish().map_err(|reason| sink_fault(key, reason))
        }
    }

    fn decode_from(
        source: &dyn DatasetSource,
        key: DatasetResourceKey,
    ) -> (ResourcePayloadDescriptor, Vec<u8>) {
        let catalog = source.catalog().unwrap();
        let descriptor = catalog.resource_payload_descriptor(key).unwrap();
        let mut sink = TestSink::new(key, descriptor);
        source.decode_into(&mut sink).unwrap();
        assert!(sink.is_finished());
        assert_eq!(sink.written_bytes(), sink.reserved_bytes());
        let view = sink.payload_view().unwrap();
        assert_eq!(view.descriptor(), descriptor);
        (descriptor, sink.bytes.into_vec())
    }

    #[test]
    fn source_contract_is_substitutable_across_two_in_memory_implementations() {
        let identity = scientific_id('5');
        let catalog = multiscale_catalog(identity);
        let key = resource_key(identity);
        let table = TableSource {
            catalog: Arc::clone(&catalog),
            resources: BTreeMap::from([(key, Arc::from([10_u8, 11, 12, 13, 0b0000_0011]))]),
        };
        let formula = FormulaSource { catalog, fill: 7 };

        let (table_descriptor, table_bytes) = decode_from(&table, key);
        assert_eq!(table_descriptor.value_byte_len(), 4);
        assert_eq!(table_descriptor.validity_byte_len(), 1);
        assert_eq!(table_bytes, vec![10, 11, 12, 13, 0b0000_0011]);

        let (formula_descriptor, formula_bytes) = decode_from(&formula, key);
        assert_eq!(formula_descriptor, table_descriptor);
        assert_eq!(formula_bytes, vec![7, 7, 7, 7, 0b0000_0011]);
    }

    #[test]
    fn unverified_source_identity_supports_bootstrap_decode_without_a_fake_scientific_id() {
        let source_id = DatasetSourceId::new(17);
        let template = multiscale_catalog(scientific_id('4'));
        let catalog = Arc::new(
            DatasetCatalog::new(
                "unverified",
                ScientificIdentityStatus::Unverified(source_id),
                template.layers().cloned().collect(),
            )
            .unwrap(),
        );
        let verified_key = resource_key(scientific_id('4'));
        let key = DatasetResourceKey::new(
            DatasetResourceIdentity::Unverified(source_id),
            verified_key.layer(),
            verified_key.timepoint(),
            verified_key.scale(),
            verified_key.region(),
        );
        let source = FormulaSource { catalog, fill: 9 };

        assert_eq!(decode_from(&source, key).1, vec![9, 9, 9, 9, 0b0000_0011]);
    }

    #[test]
    fn source_and_sink_failures_are_typed_and_reservation_bound() {
        let identity = scientific_id('6');
        let catalog = multiscale_catalog(identity);
        let key = resource_key(identity);
        let source = TableSource {
            catalog: Arc::clone(&catalog),
            resources: BTreeMap::new(),
        };
        let wrong_descriptor = ResourcePayloadDescriptor::new(
            IntensityDType::Uint8,
            key.region().shape(),
            ResourceValidity::BitMask,
        )
        .unwrap();
        let mut mismatched = TestSink::new(key, wrong_descriptor);
        assert_eq!(
            source.decode_into(&mut mismatched),
            Err(DatasetSourceFault::InvalidResource {
                key,
                reason: Box::new(ResourceContractError::PayloadDescriptorMismatch),
            })
        );

        let descriptor = catalog.resource_payload_descriptor(key).unwrap();
        let mut sink = TestSink::new(key, descriptor);
        assert_eq!(
            source.decode_into(&mut sink),
            Err(DatasetSourceFault::ResourceUnavailable { key })
        );
        let safe_message = DatasetSourceFault::ResourceUnavailable { key }.to_string();
        assert_eq!(safe_message, "semantic resource is unavailable");
        assert!(!safe_message.contains(&identity.to_string()));

        let capacity = DatasetSourceFault::CapacityExceeded {
            key,
            category: CpuLedgerCategory::InFlightDecode,
            requested_bytes: 5,
            available_bytes: 4,
        };
        assert_eq!(
            capacity,
            DatasetSourceFault::CapacityExceeded {
                key,
                category: CpuLedgerCategory::InFlightDecode,
                requested_bytes: 5,
                available_bytes: 4,
            }
        );
        assert!(!capacity.to_string().contains(&identity.to_string()));
        let shutdown = DatasetSourceFault::ShuttingDown {
            key,
            category: CpuLedgerCategory::InFlightDecode,
            requested_bytes: 5,
        };
        assert!(!shutdown.to_string().contains(&identity.to_string()));

        assert_eq!(
            sink.write(&[0; 6]),
            Err(DecodeSinkError::ReservationExceeded {
                reserved: 5,
                attempted: 6,
            })
        );
        sink.write(&[0; 2]).unwrap();
        assert_eq!(
            sink.finish(),
            Err(DecodeSinkError::Incomplete {
                reserved: 5,
                written: 2,
            })
        );
        sink.cancelled = true;
        assert_eq!(sink.write(&[0]), Err(DecodeSinkError::Cancelled));
    }

    #[test]
    fn source_checks_cancellation_before_reading_or_writing() {
        let identity = scientific_id('7');
        let catalog = multiscale_catalog(identity);
        let key = resource_key(identity);
        let source = TableSource {
            catalog: Arc::clone(&catalog),
            resources: BTreeMap::from([(key, Arc::from([1_u8, 2, 3, 4, 0b0000_0011]))]),
        };
        let descriptor = catalog.resource_payload_descriptor(key).unwrap();
        let mut sink = TestSink::new(key, descriptor);
        sink.cancelled = true;

        assert_eq!(
            source.decode_into(&mut sink),
            Err(DatasetSourceFault::Cancelled { key })
        );
        assert_eq!(sink.write_calls, 0);
        assert_eq!(sink.written_bytes(), 0);
        assert!(!sink.is_finished());
    }
}
