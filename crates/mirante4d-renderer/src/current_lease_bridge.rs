//! Temporary zero-copy bridge from unified dataset leases to the predecessor
//! renderer.
//!
//! The bridge owns no decoded allocation. It retains runtime-issued lease
//! handles only while their semantic keys are part of the current render
//! requirements and exposes borrowed payload views to the still-live renderer.
//! WP-09B deletes this module with the predecessor renderer.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

use mirante4d_dataset::{
    DatasetResourceIdentity, DatasetResourceKey, ResourceLease, ResourcePayloadView,
};
use mirante4d_domain::{
    GridToWorld, IntensityDType, LogicalLayerKey, ScaleLevel, Shape3D, TimeIndex,
};
use mirante4d_render_api::MAX_RENDER_REQUIREMENTS;
use thiserror::Error;

/// Global bound across the predecessor renderer's simultaneously current 3D
/// and cross-section semantic requirements.
pub const MAX_CURRENT_LEASE_REQUIREMENTS: usize = MAX_RENDER_REQUIREMENTS;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum CurrentLeaseBridgeError {
    #[error(
        "current renderer requirements contain {actual} unique resources, exceeding the limit of {maximum}"
    )]
    TooManyRequirements { actual: usize, maximum: usize },
    #[error("a runtime lease was delivered for a resource that is not currently required")]
    ResourceNotRequired { key: DatasetResourceKey },
    #[error("one semantic resource was delivered by two different lease allocations")]
    ConflictingLeaseAllocation { key: DatasetResourceKey },
}

/// The sole temporary decoded-resource holder in the predecessor renderer.
///
/// This map stores lease handles rather than payload values. Updating
/// requirements drops obsolete handles immediately. Installing the same lease
/// twice is idempotent; a different allocation for the same immutable semantic
/// key is an invariant failure.
#[derive(Default)]
pub struct CurrentLeaseBridge {
    requirements: BTreeSet<DatasetResourceKey>,
    leases: BTreeMap<DatasetResourceKey, Arc<dyn ResourceLease>>,
}

/// One semantic resource borrowed from a runtime-issued lease.
#[derive(Debug, Clone, Copy)]
pub struct CurrentLeaseResource<'a> {
    key: DatasetResourceKey,
    payload: ResourcePayloadView<'a>,
}

impl<'a> CurrentLeaseResource<'a> {
    pub const fn key(self) -> DatasetResourceKey {
        self.key
    }

    pub const fn payload(self) -> ResourcePayloadView<'a> {
        self.payload
    }
}

/// Zero-copy view of one layer/timepoint/scale cohort retained by the bridge.
///
/// The predecessor GPU 3D and cross-section entry points migrate to this
/// semantic cohort in the product cutover. It contains no `VolumeBrick*`,
/// current-format ID, storage index, or owning pixel allocation.
#[derive(Debug, Clone, Copy)]
pub struct CurrentLeaseResidentSet<'a> {
    bridge: &'a CurrentLeaseBridge,
    requirements: Option<&'a [DatasetResourceKey]>,
    identity: DatasetResourceIdentity,
    layer: LogicalLayerKey,
    timepoint: TimeIndex,
    scale: ScaleLevel,
}

/// Geometry plus one semantic lease cohort consumed by the predecessor GPU
/// atlas during the WP-08B/WP-09B bridge window.
///
/// The decoded allocations remain owned by the dataset runtime. The resource
/// shape is the semantic demand tile shape; clipped edge resources keep their
/// exact smaller shape in each [`DatasetResourceKey`].
#[derive(Debug, Clone, Copy)]
pub struct CurrentLeaseVolume<'a> {
    resident: CurrentLeaseResidentSet<'a>,
    volume_shape: Shape3D,
    resource_shape: Shape3D,
    grid_to_world: GridToWorld,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrentLeaseCohortStatus {
    pub required: usize,
    pub retained: usize,
    pub missing: usize,
}

impl CurrentLeaseCohortStatus {
    pub const fn is_complete(self) -> bool {
        self.required != 0 && self.missing == 0
    }
}

/// Exact retained sample state. Missing data is distinct from explicit
/// invalid/no-data and from a scientifically valid zero value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CurrentLeaseSample {
    Uint8(u8),
    Uint16(u16),
    Float32(f32),
    InvalidNoData,
    Missing,
}

impl fmt::Debug for CurrentLeaseBridge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CurrentLeaseBridge")
            .field("required_resources", &self.requirements.len())
            .field("retained_leases", &self.leases.len())
            .field("missing_resources", &self.missing_len())
            .finish()
    }
}

impl CurrentLeaseBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Atomically replaces the union of current semantic render requirements.
    ///
    /// Duplicate keys from linked views intentionally coalesce. If the new
    /// union exceeds the bound, existing requirements and leases are unchanged.
    pub fn replace_current_requirements(
        &mut self,
        requirements: impl IntoIterator<Item = DatasetResourceKey>,
    ) -> Result<usize, CurrentLeaseBridgeError> {
        self.replace_current_requirements_with_limit(requirements, MAX_CURRENT_LEASE_REQUIREMENTS)
    }

    fn replace_current_requirements_with_limit(
        &mut self,
        requirements: impl IntoIterator<Item = DatasetResourceKey>,
        maximum: usize,
    ) -> Result<usize, CurrentLeaseBridgeError> {
        let mut next = BTreeSet::new();
        for key in requirements {
            next.insert(key);
            if next.len() > maximum {
                return Err(CurrentLeaseBridgeError::TooManyRequirements {
                    actual: next.len(),
                    maximum,
                });
            }
        }

        let retained_before = self.leases.len();
        self.leases.retain(|key, _| next.contains(key));
        let retired = retained_before.saturating_sub(self.leases.len());
        self.requirements = next;
        Ok(retired)
    }

    /// Retains one runtime-issued lease without copying its payload.
    ///
    /// Returns `true` when newly installed and `false` when the exact same
    /// `Arc` was already retained.
    pub fn install(
        &mut self,
        lease: Arc<dyn ResourceLease>,
    ) -> Result<bool, CurrentLeaseBridgeError> {
        let key = lease.key();
        if !self.requirements.contains(&key) {
            return Err(CurrentLeaseBridgeError::ResourceNotRequired { key });
        }
        if let Some(current) = self.leases.get(&key) {
            return if same_payload_allocation(current.as_ref(), lease.as_ref()) {
                Ok(false)
            } else {
                Err(CurrentLeaseBridgeError::ConflictingLeaseAllocation { key })
            };
        }
        self.leases.insert(key, lease);
        Ok(true)
    }

    pub fn required_len(&self) -> usize {
        self.requirements.len()
    }

    pub fn required_keys(&self) -> impl ExactSizeIterator<Item = DatasetResourceKey> + '_ {
        self.requirements.iter().copied()
    }

    pub fn retained_len(&self) -> usize {
        self.leases.len()
    }

    pub fn missing_len(&self) -> usize {
        self.requirements.len().saturating_sub(self.leases.len())
    }

    pub fn is_complete(&self) -> bool {
        !self.requirements.is_empty() && self.missing_len() == 0
    }

    pub fn requires(&self, key: DatasetResourceKey) -> bool {
        self.requirements.contains(&key)
    }

    #[cfg(test)]
    fn retained_lease(&self, key: DatasetResourceKey) -> Option<&Arc<dyn ResourceLease>> {
        self.leases.get(&key)
    }

    /// Borrows the runtime-owned values and validity representation in place.
    pub fn payload(&self, key: DatasetResourceKey) -> Option<ResourcePayloadView<'_>> {
        self.leases.get(&key).map(|lease| lease.payload())
    }

    pub fn retained_payloads(
        &self,
    ) -> impl ExactSizeIterator<Item = (DatasetResourceKey, ResourcePayloadView<'_>)> + '_ {
        self.leases
            .iter()
            .map(|(key, lease)| (*key, lease.payload()))
    }

    pub fn resident_set(
        &self,
        identity: DatasetResourceIdentity,
        layer: LogicalLayerKey,
        timepoint: TimeIndex,
        scale: ScaleLevel,
    ) -> CurrentLeaseResidentSet<'_> {
        CurrentLeaseResidentSet {
            bridge: self,
            requirements: None,
            identity,
            layer,
            timepoint,
            scale,
        }
    }

    /// Borrows only the resources required by one linked view while retaining
    /// the bridge's single global lease authority.
    pub fn resident_subset<'a>(
        &'a self,
        requirements: &'a [DatasetResourceKey],
        identity: DatasetResourceIdentity,
        layer: LogicalLayerKey,
        timepoint: TimeIndex,
        scale: ScaleLevel,
    ) -> CurrentLeaseResidentSet<'a> {
        CurrentLeaseResidentSet {
            bridge: self,
            requirements: Some(requirements),
            identity,
            layer,
            timepoint,
            scale,
        }
    }

    /// Counts only one semantic layer/timepoint/scale cohort. Unrelated view
    /// requirements cannot keep a completed histogram or panel falsely pending.
    pub fn cohort_status(
        &self,
        identity: DatasetResourceIdentity,
        layer: LogicalLayerKey,
        timepoint: TimeIndex,
        scale: ScaleLevel,
    ) -> CurrentLeaseCohortStatus {
        let matches = |key: &&DatasetResourceKey| {
            key.identity() == identity
                && key.layer() == layer
                && key.timepoint() == timepoint
                && key.scale() == scale
        };
        let required = self.requirements.iter().filter(matches).count();
        let retained = self.leases.keys().filter(matches).count();
        CurrentLeaseCohortStatus {
            required,
            retained,
            missing: required.saturating_sub(retained),
        }
    }
}

fn same_payload_allocation(left: &dyn ResourceLease, right: &dyn ResourceLease) -> bool {
    if left.key() != right.key() {
        return false;
    }
    let left = left.payload();
    let right = right.payload();
    left.descriptor() == right.descriptor()
        && left.value_bytes().len() == right.value_bytes().len()
        && std::ptr::eq(left.value_bytes().as_ptr(), right.value_bytes().as_ptr())
        && match (left.validity_bits(), right.validity_bits()) {
            (None, None) => true,
            (Some(left), Some(right)) => {
                left.len() == right.len() && std::ptr::eq(left.as_ptr(), right.as_ptr())
            }
            (None, Some(_)) | (Some(_), None) => false,
        }
}

impl<'a> CurrentLeaseResidentSet<'a> {
    pub const fn identity(&self) -> DatasetResourceIdentity {
        self.identity
    }

    pub const fn layer(&self) -> LogicalLayerKey {
        self.layer
    }

    pub const fn timepoint(&self) -> TimeIndex {
        self.timepoint
    }

    pub const fn scale(&self) -> ScaleLevel {
        self.scale
    }

    pub fn resources(&self) -> impl Iterator<Item = CurrentLeaseResource<'a>> + 'a {
        let bridge = self.bridge;
        let requirements = self.requirements;
        let identity = self.identity;
        let layer = self.layer;
        let timepoint = self.timepoint;
        let scale = self.scale;
        let matches_cohort = move |key: DatasetResourceKey| {
            key.identity() == identity
                && key.layer() == layer
                && key.timepoint() == timepoint
                && key.scale() == scale
        };

        // A linked-view subset is normally much smaller than the bridge's
        // global retained set. Index each requested key through the lease map
        // instead of linearly searching the request slice once per retained
        // lease. The selection contains keys only, is bounded by the bridge's
        // retained-lease bound, and keeps payload ownership in the leases.
        let mut selected = requirements.map(|requirements| {
            requirements
                .iter()
                .copied()
                .filter(|key| matches_cohort(*key) && bridge.leases.contains_key(key))
                .collect::<BTreeSet<_>>()
        });
        let mut retained = bridge.leases.iter();

        std::iter::from_fn(move || {
            if let Some(selected) = selected.as_mut() {
                let key = selected.pop_first()?;
                let lease = bridge
                    .leases
                    .get(&key)
                    .expect("selected lease keys remain retained for the borrowed view");
                return Some(CurrentLeaseResource {
                    key,
                    payload: lease.payload(),
                });
            }

            loop {
                let (key, lease) = retained.next()?;
                if matches_cohort(*key) {
                    return Some(CurrentLeaseResource {
                        key: *key,
                        payload: lease.payload(),
                    });
                }
            }
        })
    }

    pub fn len(&self) -> usize {
        self.resources().count()
    }

    pub fn is_empty(&self) -> bool {
        self.resources().next().is_none()
    }

    pub fn status(&self) -> CurrentLeaseCohortStatus {
        if let Some(requirements) = self.requirements {
            let matches = |key: &&DatasetResourceKey| {
                key.identity() == self.identity
                    && key.layer() == self.layer
                    && key.timepoint() == self.timepoint
                    && key.scale() == self.scale
            };
            let required = requirements.iter().filter(matches).count();
            let retained = requirements
                .iter()
                .filter(matches)
                .filter(|key| self.bridge.leases.contains_key(key))
                .count();
            CurrentLeaseCohortStatus {
                required,
                retained,
                missing: required.saturating_sub(retained),
            }
        } else {
            self.bridge
                .cohort_status(self.identity, self.layer, self.timepoint, self.scale)
        }
    }

    /// Samples canonical `z,y,x` coordinates directly from retained payload
    /// bytes. This is the readout seam and the CPU semantic reference used by
    /// the predecessor renderer while its owning brick types are removed.
    pub fn sample(&self, index: [u64; 3]) -> CurrentLeaseSample {
        let Some(resource) = self
            .resources()
            .find(|resource| region_contains(resource.key().region(), index))
        else {
            return CurrentLeaseSample::Missing;
        };
        let region = resource.key().region();
        let origin = region.origin();
        let shape = region.shape();
        let local: [u64; 3] = std::array::from_fn(|axis| index[axis] - origin[axis]);
        let sample_index = local[0]
            .checked_mul(shape.y())
            .and_then(|value| value.checked_add(local[1]))
            .and_then(|value| value.checked_mul(shape.x()))
            .and_then(|value| value.checked_add(local[2]))
            .expect("validated resource shapes and regions preserve sample indexing");
        let payload = resource.payload();
        if !payload
            .sample_is_valid(sample_index)
            .expect("the resource region indexes its validated payload")
        {
            return CurrentLeaseSample::InvalidNoData;
        }
        let byte_offset = usize::try_from(
            sample_index
                .checked_mul(u64::from(payload.dtype().bytes_per_sample()))
                .expect("a validated payload byte length preserves sample offsets"),
        )
        .expect("a resident payload has an addressable byte slice");
        let bytes = payload.value_bytes();
        match payload.dtype() {
            IntensityDType::Uint8 => CurrentLeaseSample::Uint8(bytes[byte_offset]),
            IntensityDType::Uint16 => CurrentLeaseSample::Uint16(u16::from_le_bytes(
                bytes[byte_offset..byte_offset + 2]
                    .try_into()
                    .expect("a validated uint16 payload contains a complete sample"),
            )),
            IntensityDType::Float32 => CurrentLeaseSample::Float32(f32::from_le_bytes(
                bytes[byte_offset..byte_offset + 4]
                    .try_into()
                    .expect("a validated float32 payload contains a complete sample"),
            )),
        }
    }
}

impl<'a> CurrentLeaseVolume<'a> {
    pub const fn new(
        resident: CurrentLeaseResidentSet<'a>,
        volume_shape: Shape3D,
        resource_shape: Shape3D,
        grid_to_world: GridToWorld,
    ) -> Self {
        Self {
            resident,
            volume_shape,
            resource_shape,
            grid_to_world,
        }
    }

    pub const fn resident(self) -> CurrentLeaseResidentSet<'a> {
        self.resident
    }

    pub const fn volume_shape(self) -> Shape3D {
        self.volume_shape
    }

    pub const fn resource_shape(self) -> Shape3D {
        self.resource_shape
    }

    pub const fn grid_to_world(self) -> GridToWorld {
        self.grid_to_world
    }

    pub fn resource_grid_shape(self) -> Shape3D {
        Shape3D::new(
            self.volume_shape.z().div_ceil(self.resource_shape.z()),
            self.volume_shape.y().div_ceil(self.resource_shape.y()),
            self.volume_shape.x().div_ceil(self.resource_shape.x()),
        )
        .expect("nonempty volume and resource shapes produce a nonempty resource grid")
    }
}

fn region_contains(region: mirante4d_dataset::ResourceRegion, index: [u64; 3]) -> bool {
    region
        .origin()
        .into_iter()
        .zip(region.end_exclusive())
        .zip(index)
        .all(|((start, end), value)| start <= value && value < end)
}

#[cfg(test)]
mod tests;
