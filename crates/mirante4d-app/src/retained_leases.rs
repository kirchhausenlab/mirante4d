//! Bounded, zero-copy ownership of dataset leases retained by the product.

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    sync::Arc,
};

use crate::product_render_intent::PRODUCT_RENDER_RESOURCE_LIMIT;
use crate::semantic_tiles::SEMANTIC_TILE_SIDE;
use mirante4d_dataset::{
    BrickKey, CpuByteLease, DatasetResourceIdentity, ResourceLease, ResourcePayloadView,
};
use mirante4d_domain::{IntensityDType, LogicalLayerKey, ScaleLevel, TimeIndex};

pub(crate) const MAX_RETAINED_LEASE_REQUIREMENTS: usize = PRODUCT_RENDER_RESOURCE_LIMIT;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetainedLeaseError {
    TooManyRequirements { actual: usize, maximum: usize },
    ResourceNotRequired { key: BrickKey },
    PreparedRequirementsChanged,
}

impl fmt::Display for RetainedLeaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyRequirements { actual, maximum } => write!(
                formatter,
                "retained lease requirements contain {actual} unique resources, exceeding the limit of {maximum}"
            ),
            Self::ResourceNotRequired { .. } => formatter.write_str(
                "a runtime lease was delivered for a resource that is not currently required",
            ),
            Self::PreparedRequirementsChanged => formatter.write_str(
                "the retained requirement union changed after its worker delta was prepared",
            ),
        }
    }
}

impl std::error::Error for RetainedLeaseError {}

/// Runtime-issued lease handles for the product's current semantic demand.
///
/// Replacing requirements immediately drops obsolete handles. Payload values
/// and validity masks stay owned by their leases and are only borrowed here.
#[derive(Default)]
pub(crate) struct RetainedLeases {
    requirements: Arc<[BrickKey]>,
    requirement_charge: Option<Arc<dyn CpuByteLease>>,
    leases: BTreeMap<BrickKey, Arc<dyn ResourceLease>>,
    generation: u64,
    spatial_index: HashMap<RetainedSpatialKey, Vec<BrickKey>>,
    #[cfg(test)]
    spatial_lookup_visits: std::cell::Cell<u64>,
    #[cfg(test)]
    prepared_requirement_swaps: u64,
    #[cfg(test)]
    prepared_requirement_key_visits: u64,
    #[cfg(test)]
    prepared_removal_delta_visits: u64,
}

/// Indivisible immutable retained-union identity and its ledger lifetime.
/// Legacy/test unions may be uncharged; worker-installed unions always carry
/// `Some`, and cloning this handle keeps bytes charged while a latest-only
/// request or prepared result still references the old key body.
#[derive(Clone)]
pub(crate) struct RetainedRequirementHandle {
    pub(crate) requirements: Arc<[BrickKey]>,
    pub(crate) charge: Option<Arc<dyn CpuByteLease>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RetainedSpatialKey {
    identity: DatasetResourceIdentity,
    layer: LogicalLayerKey,
    timepoint: TimeIndex,
    scale: ScaleLevel,
    tile: [u64; 3],
}

impl RetainedSpatialKey {
    fn for_resource(key: BrickKey) -> Self {
        Self {
            identity: key.identity(),
            layer: key.layer(),
            timepoint: key.timepoint(),
            scale: key.scale(),
            tile: key
                .region()
                .origin()
                .map(|value| value / SEMANTIC_TILE_SIDE),
        }
    }

    fn for_sample(
        identity: DatasetResourceIdentity,
        layer: LogicalLayerKey,
        timepoint: TimeIndex,
        scale: ScaleLevel,
        index: [u64; 3],
    ) -> Self {
        Self {
            identity,
            layer,
            timepoint,
            scale,
            tile: index.map(|value| value / SEMANTIC_TILE_SIDE),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RetainedLeaseResource<'a> {
    payload: ResourcePayloadView<'a>,
}

impl<'a> RetainedLeaseResource<'a> {
    pub(crate) const fn payload(self) -> ResourcePayloadView<'a> {
        self.payload
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RetainedLeaseCohort<'a> {
    leases: &'a RetainedLeases,
    requirements: Option<&'a [BrickKey]>,
    identity: DatasetResourceIdentity,
    layer: LogicalLayerKey,
    timepoint: TimeIndex,
    scale: ScaleLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetainedLeaseStatus {
    pub(crate) required: usize,
    pub(crate) retained: usize,
    pub(crate) missing: usize,
}

impl RetainedLeaseStatus {
    pub(crate) const fn is_complete(self) -> bool {
        self.required != 0 && self.missing == 0
    }
}

/// A sample borrowed directly from a retained payload.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RetainedLeaseSample {
    Uint8(u8),
    Uint16(u16),
    Float32(f32),
    InvalidNoData,
    Missing,
}

impl fmt::Debug for RetainedLeases {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetainedLeases")
            .field("required_resources", &self.requirements.len())
            .field("retained_leases", &self.leases.len())
            .field("missing_resources", &self.missing_len())
            .field("generation", &self.generation)
            .finish()
    }
}

impl RetainedLeases {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Monotonic identity for the exact CPU lease cohort visible to the
    /// renderer. Deterministic render failures may be suppressed only while
    /// this input generation remains unchanged.
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    /// Atomically replaces the union of current semantic requirements.
    /// Atomically replaces the requirement set and returns the number of
    /// previously retained lease handles retired by that replacement.
    #[cfg(test)]
    pub(crate) fn replace_requirements(
        &mut self,
        requirements: impl IntoIterator<Item = BrickKey>,
    ) -> Result<usize, RetainedLeaseError> {
        self.replace_requirements_with_limit(requirements, MAX_RETAINED_LEASE_REQUIREMENTS)
    }

    #[cfg(test)]
    fn replace_requirements_with_limit(
        &mut self,
        requirements: impl IntoIterator<Item = BrickKey>,
        maximum: usize,
    ) -> Result<usize, RetainedLeaseError> {
        let mut next = requirements.into_iter().collect::<Vec<_>>();
        next.sort_unstable();
        next.dedup();
        self.replace_prepared_requirements_with_limit(next.into(), maximum)
    }

    /// Installs an immutable canonical union prepared off the UI thread.
    /// Swapping the large requirement body is O(1); only the much smaller set
    /// of currently CPU-retained handles is checked for retirement.
    #[cfg(test)]
    pub(crate) fn replace_prepared_requirements(
        &mut self,
        requirements: Arc<[BrickKey]>,
    ) -> Result<usize, RetainedLeaseError> {
        self.replace_prepared_requirements_with_limit(requirements, MAX_RETAINED_LEASE_REQUIREMENTS)
    }

    #[cfg(test)]
    fn replace_prepared_requirements_with_limit(
        &mut self,
        next: Arc<[BrickKey]>,
        maximum: usize,
    ) -> Result<usize, RetainedLeaseError> {
        debug_assert!(next.is_sorted());
        debug_assert!(next.windows(2).all(|pair| pair[0] != pair[1]));
        Self::preflight_prepared_requirements_with_limit(&next, maximum)?;

        let changed = !Arc::ptr_eq(&self.requirements, &next);
        let retained_before = self.leases.len();
        self.leases.retain(|key, _| next.binary_search(key).is_ok());
        let leases = &self.leases;
        self.spatial_index.retain(|_, keys| {
            keys.retain(|key| leases.contains_key(key));
            !keys.is_empty()
        });
        let retired = retained_before.saturating_sub(self.leases.len());
        self.requirements = next;
        self.requirement_charge = None;
        #[cfg(test)]
        {
            self.prepared_requirement_swaps = self.prepared_requirement_swaps.saturating_add(1);
            self.prepared_requirement_key_visits =
                self.prepared_requirement_key_visits.saturating_add(0);
        }
        if changed {
            self.generation = self.generation.saturating_add(1);
        }
        Ok(retired)
    }

    /// Validates a worker-prepared replacement without visiting either large
    /// key body. Arc identity binds the removal delta to the exact old union.
    pub(crate) fn preflight_prepared_requirement_update(
        &self,
        previous: &Arc<[BrickKey]>,
        next: &Arc<[BrickKey]>,
    ) -> Result<(), RetainedLeaseError> {
        Self::preflight_prepared_requirements(next)?;
        if !Arc::ptr_eq(&self.requirements, previous) {
            return Err(RetainedLeaseError::PreparedRequirementsChanged);
        }
        Ok(())
    }

    /// Infallible commit after `preflight_prepared_requirement_update`.
    /// Requirement ownership swaps by Arc and only worker-proven removals
    /// touch the retained map and their directly addressed spatial buckets.
    pub(crate) fn commit_prepared_requirement_update(
        &mut self,
        previous: Arc<[BrickKey]>,
        next: Arc<[BrickKey]>,
        removals: &[BrickKey],
        charge: Arc<dyn CpuByteLease>,
    ) -> usize {
        debug_assert!(Arc::ptr_eq(&self.requirements, &previous));
        debug_assert!(next.is_sorted());
        debug_assert!(removals.is_sorted());
        debug_assert!(removals.windows(2).all(|pair| pair[0] != pair[1]));
        debug_assert!(removals.iter().all(|key| {
            previous.binary_search(key).is_ok() && next.binary_search(key).is_err()
        }));
        let changed = !Arc::ptr_eq(&previous, &next);
        let mut retired = 0_usize;
        for key in removals {
            if self.leases.remove(key).is_some() {
                retired += 1;
                self.remove_spatial_key(*key);
            }
        }
        self.requirements = next;
        self.requirement_charge = Some(charge);
        #[cfg(test)]
        {
            self.prepared_requirement_swaps = self.prepared_requirement_swaps.saturating_add(1);
            self.prepared_removal_delta_visits = self
                .prepared_removal_delta_visits
                .saturating_add(removals.len() as u64);
        }
        if changed {
            self.generation = self.generation.saturating_add(1);
        }
        retired
    }

    pub(crate) fn preflight_prepared_requirements(
        requirements: &Arc<[BrickKey]>,
    ) -> Result<(), RetainedLeaseError> {
        Self::preflight_prepared_requirements_with_limit(
            requirements,
            MAX_RETAINED_LEASE_REQUIREMENTS,
        )
    }

    fn preflight_prepared_requirements_with_limit(
        requirements: &Arc<[BrickKey]>,
        maximum: usize,
    ) -> Result<(), RetainedLeaseError> {
        if requirements.len() > maximum {
            return Err(RetainedLeaseError::TooManyRequirements {
                actual: requirements.len(),
                maximum,
            });
        }
        Ok(())
    }

    /// Retains a runtime lease without copying its payload.
    ///
    /// Returns `true` for a new handle and `false` when this immutable
    /// semantic resource already has a retained allocation.
    pub(crate) fn install(
        &mut self,
        lease: Arc<dyn ResourceLease>,
    ) -> Result<bool, RetainedLeaseError> {
        let key = lease.key();
        if self.requirements.binary_search(&key).is_err() {
            return Err(RetainedLeaseError::ResourceNotRequired { key });
        }
        if self.leases.contains_key(&key) {
            // BrickKey is the immutable scientific-content identity. The
            // runtime cache may evict its own handle while this owner still
            // retains one, then validly decode the same key into a different
            // physical allocation for a raced waiter. Keep the first retained
            // lease and release the redundant delivery without changing
            // readiness, generation, or renderer authority.
            return Ok(false);
        }
        self.leases.insert(key, lease);
        let spatial = RetainedSpatialKey::for_resource(key);
        let bucket = self.spatial_index.entry(spatial).or_default();
        if !bucket.contains(&key) {
            bucket.push(key);
        }
        self.generation = self.generation.saturating_add(1);
        Ok(true)
    }

    pub(crate) fn required_len(&self) -> usize {
        self.requirements.len()
    }

    #[cfg(test)]
    pub(crate) fn required_keys(&self) -> impl ExactSizeIterator<Item = BrickKey> + '_ {
        self.requirements.iter().copied()
    }

    #[cfg(test)]
    pub(crate) fn requirement_handle(&self) -> Arc<[BrickKey]> {
        Arc::clone(&self.requirements)
    }

    pub(crate) fn accounted_requirement_handle(&self) -> RetainedRequirementHandle {
        RetainedRequirementHandle {
            requirements: Arc::clone(&self.requirements),
            charge: self.requirement_charge.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) const fn prepared_requirement_swap_work(&self) -> (u64, u64) {
        (
            self.prepared_requirement_swaps,
            self.prepared_requirement_key_visits,
        )
    }

    #[cfg(test)]
    pub(crate) const fn prepared_removal_delta_visits(&self) -> u64 {
        self.prepared_removal_delta_visits
    }

    pub(crate) fn retained_len(&self) -> usize {
        self.leases.len()
    }

    pub(crate) fn missing_len(&self) -> usize {
        self.requirements.len().saturating_sub(self.leases.len())
    }

    #[cfg(test)]
    pub(crate) fn is_complete(&self) -> bool {
        !self.requirements.is_empty() && self.missing_len() == 0
    }

    pub(crate) fn requires(&self, key: BrickKey) -> bool {
        self.requirements.binary_search(&key).is_ok()
    }

    #[cfg(test)]
    fn retained_lease(&self, key: BrickKey) -> Option<&Arc<dyn ResourceLease>> {
        self.leases.get(&key)
    }

    pub(crate) fn payload(&self, key: BrickKey) -> Option<ResourcePayloadView<'_>> {
        self.leases.get(&key).map(|lease| lease.payload())
    }

    pub(crate) fn lease_handle(&self, requirement: BrickKey) -> Option<Arc<dyn ResourceLease>> {
        self.leases.get(&requirement).cloned()
    }

    /// Drops one CPU payload handle after the renderer has committed the
    /// exact immutable resource to GPU residency. The semantic requirement
    /// remains installed so an eviction can re-admit the same key without
    /// replanning or changing renderer slot identity.
    pub(crate) fn retire_payload_handle(&mut self, key: BrickKey) -> bool {
        if self.leases.remove(&key).is_none() {
            return false;
        }
        self.remove_spatial_key(key);
        self.generation = self.generation.saturating_add(1);
        true
    }

    fn remove_spatial_key(&mut self, key: BrickKey) {
        let spatial = RetainedSpatialKey::for_resource(key);
        if let Some(bucket) = self.spatial_index.get_mut(&spatial) {
            bucket.retain(|candidate| *candidate != key);
            if bucket.is_empty() {
                self.spatial_index.remove(&spatial);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn spatial_lookup_visits(&self) -> u64 {
        self.spatial_lookup_visits.get()
    }

    pub(crate) fn retained_payloads(
        &self,
    ) -> impl ExactSizeIterator<Item = (BrickKey, ResourcePayloadView<'_>)> + '_ {
        self.leases
            .iter()
            .map(|(key, lease)| (*key, lease.payload()))
    }

    pub(crate) fn resident_set(
        &self,
        identity: DatasetResourceIdentity,
        layer: LogicalLayerKey,
        timepoint: TimeIndex,
        scale: ScaleLevel,
    ) -> RetainedLeaseCohort<'_> {
        RetainedLeaseCohort {
            leases: self,
            requirements: None,
            identity,
            layer,
            timepoint,
            scale,
        }
    }

    pub(crate) fn resident_subset<'a>(
        &'a self,
        requirements: &'a [BrickKey],
        identity: DatasetResourceIdentity,
        layer: LogicalLayerKey,
        timepoint: TimeIndex,
        scale: ScaleLevel,
    ) -> RetainedLeaseCohort<'a> {
        RetainedLeaseCohort {
            leases: self,
            requirements: Some(requirements),
            identity,
            layer,
            timepoint,
            scale,
        }
    }

    pub(crate) fn cohort_status(
        &self,
        identity: DatasetResourceIdentity,
        layer: LogicalLayerKey,
        timepoint: TimeIndex,
        scale: ScaleLevel,
    ) -> RetainedLeaseStatus {
        let matches = |key: &&BrickKey| {
            key.identity() == identity
                && key.layer() == layer
                && key.timepoint() == timepoint
                && key.scale() == scale
        };
        let required = self.requirements.iter().filter(matches).count();
        let retained = self.leases.keys().filter(matches).count();
        RetainedLeaseStatus {
            required,
            retained,
            missing: required.saturating_sub(retained),
        }
    }
}

impl<'a> RetainedLeaseCohort<'a> {
    pub(crate) fn resources(&self) -> impl Iterator<Item = RetainedLeaseResource<'a>> + 'a {
        let leases = self.leases;
        let requirements = self.requirements;
        let identity = self.identity;
        let layer = self.layer;
        let timepoint = self.timepoint;
        let scale = self.scale;
        let matches_cohort = move |key: BrickKey| {
            key.identity() == identity
                && key.layer() == layer
                && key.timepoint() == timepoint
                && key.scale() == scale
        };

        let mut selected = requirements.map(|requirements| requirements.iter().copied());
        let mut retained = leases.leases.iter();

        std::iter::from_fn(move || {
            if let Some(selected) = selected.as_mut() {
                loop {
                    let key = selected.next()?;
                    if matches_cohort(key)
                        && let Some(lease) = leases.leases.get(&key)
                    {
                        return Some(RetainedLeaseResource {
                            payload: lease.payload(),
                        });
                    }
                }
            }

            loop {
                let (key, lease) = retained.next()?;
                if matches_cohort(*key) {
                    return Some(RetainedLeaseResource {
                        payload: lease.payload(),
                    });
                }
            }
        })
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.resources().count()
    }

    pub(crate) fn status(&self) -> RetainedLeaseStatus {
        if let Some(requirements) = self.requirements {
            let matches = |key: &&BrickKey| {
                key.identity() == self.identity
                    && key.layer() == self.layer
                    && key.timepoint() == self.timepoint
                    && key.scale() == self.scale
            };
            let required = requirements.iter().filter(matches).count();
            let retained = requirements
                .iter()
                .filter(matches)
                .filter(|key| self.leases.leases.contains_key(key))
                .count();
            RetainedLeaseStatus {
                required,
                retained,
                missing: required.saturating_sub(retained),
            }
        } else {
            self.leases
                .cohort_status(self.identity, self.layer, self.timepoint, self.scale)
        }
    }

    pub(crate) fn sample(&self, index: [u64; 3]) -> RetainedLeaseSample {
        #[cfg(test)]
        self.leases
            .spatial_lookup_visits
            .set(self.leases.spatial_lookup_visits.get().saturating_add(1));
        let spatial = RetainedSpatialKey::for_sample(
            self.identity,
            self.layer,
            self.timepoint,
            self.scale,
            index,
        );
        let Some(key) = self.leases.spatial_index.get(&spatial).and_then(|keys| {
            keys.iter().copied().find(|key| {
                region_contains(key.region(), index)
                    && self
                        .requirements
                        .is_none_or(|requirements| requirements.binary_search(key).is_ok())
            })
        }) else {
            return RetainedLeaseSample::Missing;
        };
        let Some(lease) = self.leases.leases.get(&key) else {
            return RetainedLeaseSample::Missing;
        };
        let region = key.region();
        let origin = region.origin();
        let shape = region.shape();
        let local: [u64; 3] = std::array::from_fn(|axis| index[axis] - origin[axis]);
        let sample_index = local[0]
            .checked_mul(shape.y())
            .and_then(|value| value.checked_add(local[1]))
            .and_then(|value| value.checked_mul(shape.x()))
            .and_then(|value| value.checked_add(local[2]))
            .expect("validated resource shapes and regions preserve sample indexing");
        let payload = lease.payload();
        if !payload
            .sample_is_valid(sample_index)
            .expect("the resource region indexes its validated payload")
        {
            return RetainedLeaseSample::InvalidNoData;
        }
        let byte_offset = usize::try_from(
            sample_index
                .checked_mul(u64::from(payload.dtype().bytes_per_sample()))
                .expect("a validated payload byte length preserves sample offsets"),
        )
        .expect("a resident payload has an addressable byte slice");
        let bytes = payload.value_bytes();
        match payload.dtype() {
            IntensityDType::Uint8 => RetainedLeaseSample::Uint8(bytes[byte_offset]),
            IntensityDType::Uint16 => RetainedLeaseSample::Uint16(u16::from_le_bytes(
                bytes[byte_offset..byte_offset + 2]
                    .try_into()
                    .expect("a validated uint16 payload contains a complete sample"),
            )),
            IntensityDType::Float32 => RetainedLeaseSample::Float32(f32::from_le_bytes(
                bytes[byte_offset..byte_offset + 4]
                    .try_into()
                    .expect("a validated float32 payload contains a complete sample"),
            )),
        }
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
mod tests {
    use super::*;
    use mirante4d_dataset::{
        DatasetSourceId, ResourceContractError, ResourcePayloadDescriptor, ResourceRegion,
        ResourceValidity,
    };
    use mirante4d_domain::Shape3D;

    #[derive(Debug)]
    struct FixtureLease {
        key: BrickKey,
        descriptor: ResourcePayloadDescriptor,
        values: Box<[u8]>,
        validity: Option<Box<[u8]>>,
    }

    impl FixtureLease {
        fn u16(
            key: BrickKey,
            values: &[u16],
            validity: Option<&[u8]>,
        ) -> Result<Self, ResourceContractError> {
            let descriptor = ResourcePayloadDescriptor::new(
                IntensityDType::Uint16,
                key.region().shape(),
                if validity.is_some() {
                    ResourceValidity::BitMask
                } else {
                    ResourceValidity::AllValid
                },
            )?;
            let values = values
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>()
                .into_boxed_slice();
            let validity = validity.map(|bits| bits.to_vec().into_boxed_slice());
            descriptor.view(&values, validity.as_deref())?;
            Ok(Self {
                key,
                descriptor,
                values,
                validity,
            })
        }
    }

    impl ResourceLease for FixtureLease {
        fn key(&self) -> BrickKey {
            self.key
        }

        fn payload(&self) -> ResourcePayloadView<'_> {
            self.descriptor
                .view(&self.values, self.validity.as_deref())
                .expect("fixture lease preserves its validated payload")
        }

        fn payload_facts(&self) -> mirante4d_dataset::ResourcePayloadFacts {
            mirante4d_dataset::ResourcePayloadFacts::from_payload(self.payload())
                .expect("fixture facts are valid")
        }
    }

    #[derive(Debug)]
    struct FixtureCpuCharge(u64);

    impl CpuByteLease for FixtureCpuCharge {
        fn category(&self) -> mirante4d_dataset::CpuLedgerCategory {
            mirante4d_dataset::CpuLedgerCategory::QueuesAndResults
        }

        fn reserved_bytes(&self) -> u64 {
            self.0
        }
    }

    fn key(x: u64) -> BrickKey {
        BrickKey::new(
            DatasetResourceIdentity::SessionLocal(DatasetSourceId::new(7)),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, x], Shape3D::new(1, 1, 2).unwrap()).unwrap(),
        )
    }

    fn lease(key: BrickKey, values: &[u16], validity: Option<&[u8]>) -> Arc<dyn ResourceLease> {
        Arc::new(FixtureLease::u16(key, values, validity).unwrap())
    }

    fn semantic_key(layer: u32, tile_x: u64) -> BrickKey {
        BrickKey::new(
            DatasetResourceIdentity::SessionLocal(DatasetSourceId::new(17)),
            LogicalLayerKey::new(layer),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new(
                [0, 0, tile_x * SEMANTIC_TILE_SIDE],
                Shape3D::new(1, 1, 1).unwrap(),
            )
            .unwrap(),
        )
    }

    #[test]
    fn retains_lease_payload_without_copying() {
        let key = key(0);
        let lease = lease(key, &[0, 41], Some(&[0b0000_0001]));
        let original = lease.payload();
        let original_values = original.value_bytes().as_ptr();
        let original_validity = original.validity_bits().unwrap().as_ptr();

        let mut retained = RetainedLeases::new();
        assert_eq!(retained.replace_requirements([key]), Ok(0));
        assert_eq!(retained.install(Arc::clone(&lease)), Ok(true));
        assert!(Arc::ptr_eq(retained.retained_lease(key).unwrap(), &lease));

        let payload = retained.payload(key).unwrap();
        assert_eq!(payload.value_bytes().as_ptr(), original_values);
        assert_eq!(payload.validity_bits().unwrap().as_ptr(), original_validity);
        assert!(payload.sample_is_valid(0).unwrap());
        assert!(!payload.sample_is_valid(1).unwrap());
        assert_eq!(retained.install(Arc::clone(&lease)), Ok(false));
    }

    #[test]
    fn spatial_sample_lookup_is_constant_with_full_unrelated_envelope() {
        const UNRELATED: u64 = 65_000;
        let target = semantic_key(1, 0);
        let mut requirements = (0..UNRELATED)
            .map(|x| semantic_key(0, x))
            .collect::<Vec<_>>();
        requirements.push(target);
        let mut retained = RetainedLeases::new();
        retained
            .replace_requirements(requirements.iter().copied())
            .unwrap();
        for key in requirements {
            retained.install(lease(key, &[1], None)).unwrap();
        }
        let before = retained.spatial_lookup_visits();
        let sample = retained
            .resident_set(
                target.identity(),
                target.layer(),
                target.timepoint(),
                target.scale(),
            )
            .sample([0, 0, 0]);

        assert_eq!(sample, RetainedLeaseSample::Uint16(1));
        assert_eq!(retained.spatial_lookup_visits() - before, 1);
    }

    #[test]
    fn requirements_coalesce_and_retire_obsolete_leases() {
        let first = key(0);
        let shared = key(2);
        let next = key(4);
        let shared_lease = lease(shared, &[3, 4], None);

        let mut retained = RetainedLeases::new();
        assert_eq!(
            retained.replace_requirements([first, shared, shared]),
            Ok(0)
        );
        assert_eq!(retained.required_len(), 2);
        retained.install(lease(first, &[1, 2], None)).unwrap();
        retained.install(Arc::clone(&shared_lease)).unwrap();
        assert!(retained.is_complete());

        assert_eq!(retained.replace_requirements([shared, next]), Ok(1));
        assert!(retained.payload(first).is_none());
        assert!(Arc::ptr_eq(
            retained.retained_lease(shared).unwrap(),
            &shared_lease
        ));
        assert_eq!(
            retained.required_keys().collect::<Vec<_>>(),
            vec![shared, next]
        );
        assert_eq!(retained.retained_len(), 1);
        assert_eq!(retained.missing_len(), 1);
    }

    #[test]
    fn full_envelope_prepared_swap_retires_only_the_two_key_worker_delta() {
        const REQUIREMENTS: u64 = 65_536;
        let previous = (0..REQUIREMENTS)
            .map(|index| semantic_key(0, index))
            .collect::<Arc<[_]>>();
        let removed = [previous[17], previous[65_000]];
        let next = previous
            .iter()
            .copied()
            .filter(|key| removed.binary_search(key).is_err())
            .collect::<Arc<[_]>>();
        let kept = previous[31];
        let mut retained = RetainedLeases::new();
        retained
            .replace_prepared_requirements(Arc::clone(&previous))
            .unwrap();
        for key in [removed[0], kept, removed[1]] {
            retained.install(lease(key, &[1], None)).unwrap();
        }

        retained
            .preflight_prepared_requirement_update(&previous, &next)
            .unwrap();
        let visits_before = retained.prepared_removal_delta_visits();
        let charge: Arc<dyn CpuByteLease> = Arc::new(FixtureCpuCharge(
            u64::try_from(next.len() * std::mem::size_of::<BrickKey>()).unwrap(),
        ));
        assert_eq!(
            retained.commit_prepared_requirement_update(
                Arc::clone(&previous),
                Arc::clone(&next),
                &removed,
                charge,
            ),
            2
        );

        assert_eq!(
            retained.prepared_removal_delta_visits() - visits_before,
            2,
            "the UI commit must not scan the 65,534-key retained target"
        );
        assert!(Arc::ptr_eq(&retained.requirement_handle(), &next));
        assert!(retained.payload(removed[0]).is_none());
        assert!(retained.payload(removed[1]).is_none());
        assert!(retained.payload(kept).is_some());
    }

    #[test]
    fn install_rejects_unrequired_and_coalesces_redecoded_allocations() {
        let required = key(0);
        let unrequired = key(2);
        let first = lease(required, &[7, 9], None);

        let mut retained = RetainedLeases::new();
        retained.replace_requirements([required]).unwrap();
        assert_eq!(retained.install(Arc::clone(&first)), Ok(true));
        assert_eq!(retained.install(Arc::clone(&first)), Ok(false));
        assert_eq!(
            retained.install(lease(unrequired, &[3, 4], None)),
            Err(RetainedLeaseError::ResourceNotRequired { key: unrequired })
        );
        assert_eq!(
            retained.install(lease(required, &[7, 9], None)),
            Ok(false),
            "an immutable semantic duplicate may use another physical allocation"
        );
        assert!(Arc::ptr_eq(
            retained.retained_lease(required).unwrap(),
            &first
        ));
    }

    #[test]
    fn over_limit_requirement_update_is_atomic() {
        let retained_key = key(0);
        let retained_lease = lease(retained_key, &[1, 2], None);
        let mut retained = RetainedLeases::new();
        retained.replace_requirements([retained_key]).unwrap();
        retained.install(Arc::clone(&retained_lease)).unwrap();

        assert_eq!(
            retained.replace_requirements_with_limit([key(2), key(4), key(6)], 2),
            Err(RetainedLeaseError::TooManyRequirements {
                actual: 3,
                maximum: 2,
            })
        );
        assert!(retained.requires(retained_key));
        assert!(Arc::ptr_eq(
            retained.retained_lease(retained_key).unwrap(),
            &retained_lease
        ));

        let too_many = (0..=MAX_RETAINED_LEASE_REQUIREMENTS as u64).map(key);
        assert_eq!(
            retained.replace_requirements(too_many),
            Err(RetainedLeaseError::TooManyRequirements {
                actual: MAX_RETAINED_LEASE_REQUIREMENTS + 1,
                maximum: MAX_RETAINED_LEASE_REQUIREMENTS,
            })
        );
        assert!(retained.requires(retained_key));
    }

    #[test]
    fn atlas_scale_requirement_set_is_retained_without_the_old_128_cap() {
        let mut retained = RetainedLeases::new();
        let resources = (0..32_768).map(key).collect::<Vec<_>>();

        // The return value is the number of obsolete retained lease handles
        // retired, not the number of newly required resources.
        assert_eq!(retained.replace_requirements(resources.clone()), Ok(0));
        assert_eq!(retained.required_len(), 32_768);
        assert!(
            resources
                .into_iter()
                .all(|resource| retained.requires(resource))
        );
    }

    #[test]
    fn cohort_filters_requirements_and_samples_validity() {
        let first = key(0);
        let second = key(2);
        let missing = key(4);
        let other_timepoint = BrickKey::new(
            first.identity(),
            first.layer(),
            TimeIndex::new(1),
            first.scale(),
            first.region(),
        );

        let mut retained = RetainedLeases::new();
        retained
            .replace_requirements([first, second, missing, other_timepoint])
            .unwrap();
        retained
            .install(lease(first, &[0, 11], Some(&[0b0000_0001])))
            .unwrap();
        retained.install(lease(second, &[22, 33], None)).unwrap();
        retained
            .install(lease(other_timepoint, &[44, 55], None))
            .unwrap();

        let cohort = retained.resident_set(
            first.identity(),
            first.layer(),
            first.timepoint(),
            first.scale(),
        );
        assert_eq!(
            cohort.status(),
            RetainedLeaseStatus {
                required: 3,
                retained: 2,
                missing: 1,
            }
        );
        assert!(!cohort.status().is_complete());
        assert_eq!(cohort.len(), 2);
        assert_eq!(cohort.sample([0, 0, 0]), RetainedLeaseSample::Uint16(0));
        assert_eq!(cohort.sample([0, 0, 1]), RetainedLeaseSample::InvalidNoData);
        assert_eq!(cohort.sample([0, 0, 2]), RetainedLeaseSample::Uint16(22));

        let subset_requirements = [second, missing];
        let subset = retained.resident_subset(
            &subset_requirements,
            first.identity(),
            first.layer(),
            first.timepoint(),
            first.scale(),
        );
        assert_eq!(subset.resources().count(), 1);
        assert_eq!(
            subset.status(),
            RetainedLeaseStatus {
                required: 2,
                retained: 1,
                missing: 1,
            }
        );
        assert_eq!(subset.sample([0, 0, 0]), RetainedLeaseSample::Missing);
        assert_eq!(
            retained.retained_payloads().count(),
            retained.retained_len()
        );
    }
}
