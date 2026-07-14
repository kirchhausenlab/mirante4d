//! Bounded, zero-copy ownership of dataset leases retained by the product.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

use mirante4d_dataset::{
    DatasetResourceIdentity, DatasetResourceKey, ResourceLease, ResourcePayloadView,
};
use mirante4d_domain::{IntensityDType, LogicalLayerKey, ScaleLevel, TimeIndex};
use mirante4d_render_api::MAX_RENDER_REQUIREMENTS;

pub(crate) const MAX_RETAINED_LEASE_REQUIREMENTS: usize = MAX_RENDER_REQUIREMENTS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetainedLeaseError {
    TooManyRequirements { actual: usize, maximum: usize },
    ResourceNotRequired { key: DatasetResourceKey },
    ConflictingLeaseAllocation { key: DatasetResourceKey },
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
            Self::ConflictingLeaseAllocation { .. } => formatter.write_str(
                "one semantic resource was delivered by two different lease allocations",
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
    requirements: BTreeSet<DatasetResourceKey>,
    leases: BTreeMap<DatasetResourceKey, Arc<dyn ResourceLease>>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RetainedLeaseResource<'a> {
    key: DatasetResourceKey,
    payload: ResourcePayloadView<'a>,
}

impl<'a> RetainedLeaseResource<'a> {
    pub(crate) const fn key(self) -> DatasetResourceKey {
        self.key
    }

    pub(crate) const fn payload(self) -> ResourcePayloadView<'a> {
        self.payload
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RetainedLeaseCohort<'a> {
    leases: &'a RetainedLeases,
    requirements: Option<&'a [DatasetResourceKey]>,
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
            .finish()
    }
}

impl RetainedLeases {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Atomically replaces the union of current semantic requirements.
    pub(crate) fn replace_requirements(
        &mut self,
        requirements: impl IntoIterator<Item = DatasetResourceKey>,
    ) -> Result<usize, RetainedLeaseError> {
        self.replace_requirements_with_limit(requirements, MAX_RETAINED_LEASE_REQUIREMENTS)
    }

    fn replace_requirements_with_limit(
        &mut self,
        requirements: impl IntoIterator<Item = DatasetResourceKey>,
        maximum: usize,
    ) -> Result<usize, RetainedLeaseError> {
        let mut next = BTreeSet::new();
        for key in requirements {
            next.insert(key);
            if next.len() > maximum {
                return Err(RetainedLeaseError::TooManyRequirements {
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

    /// Retains a runtime lease without copying its payload.
    ///
    /// Returns `true` for a new handle and `false` when the same underlying
    /// allocation was already retained.
    pub(crate) fn install(
        &mut self,
        lease: Arc<dyn ResourceLease>,
    ) -> Result<bool, RetainedLeaseError> {
        let key = lease.key();
        if !self.requirements.contains(&key) {
            return Err(RetainedLeaseError::ResourceNotRequired { key });
        }
        if let Some(current) = self.leases.get(&key) {
            return if same_payload_allocation(current.as_ref(), lease.as_ref()) {
                Ok(false)
            } else {
                Err(RetainedLeaseError::ConflictingLeaseAllocation { key })
            };
        }
        self.leases.insert(key, lease);
        Ok(true)
    }

    pub(crate) fn required_len(&self) -> usize {
        self.requirements.len()
    }

    pub(crate) fn required_keys(&self) -> impl ExactSizeIterator<Item = DatasetResourceKey> + '_ {
        self.requirements.iter().copied()
    }

    pub(crate) fn retained_len(&self) -> usize {
        self.leases.len()
    }

    pub(crate) fn missing_len(&self) -> usize {
        self.requirements.len().saturating_sub(self.leases.len())
    }

    pub(crate) fn is_complete(&self) -> bool {
        !self.requirements.is_empty() && self.missing_len() == 0
    }

    pub(crate) fn requires(&self, key: DatasetResourceKey) -> bool {
        self.requirements.contains(&key)
    }

    #[cfg(test)]
    fn retained_lease(&self, key: DatasetResourceKey) -> Option<&Arc<dyn ResourceLease>> {
        self.leases.get(&key)
    }

    pub(crate) fn payload(&self, key: DatasetResourceKey) -> Option<ResourcePayloadView<'_>> {
        self.leases.get(&key).map(|lease| lease.payload())
    }

    pub(crate) fn lease_refs<'a>(
        &'a self,
        requirements: &[DatasetResourceKey],
    ) -> Vec<&'a dyn ResourceLease> {
        requirements
            .iter()
            .filter_map(|key| self.leases.get(key).map(Arc::as_ref))
            .collect()
    }

    pub(crate) fn lease_handles(
        &self,
        requirements: &[DatasetResourceKey],
    ) -> Vec<Arc<dyn ResourceLease>> {
        requirements
            .iter()
            .filter_map(|key| self.leases.get(key).cloned())
            .collect()
    }

    pub(crate) fn retained_payloads(
        &self,
    ) -> impl ExactSizeIterator<Item = (DatasetResourceKey, ResourcePayloadView<'_>)> + '_ {
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
        requirements: &'a [DatasetResourceKey],
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
        let matches = |key: &&DatasetResourceKey| {
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
        let matches_cohort = move |key: DatasetResourceKey| {
            key.identity() == identity
                && key.layer() == layer
                && key.timepoint() == timepoint
                && key.scale() == scale
        };

        let mut selected = requirements.map(|requirements| {
            requirements
                .iter()
                .copied()
                .filter(|key| matches_cohort(*key) && leases.leases.contains_key(key))
                .collect::<BTreeSet<_>>()
        });
        let mut retained = leases.leases.iter();

        std::iter::from_fn(move || {
            if let Some(selected) = selected.as_mut() {
                let key = selected.pop_first()?;
                let lease = leases
                    .leases
                    .get(&key)
                    .expect("selected lease keys remain retained for the borrowed view");
                return Some(RetainedLeaseResource {
                    key,
                    payload: lease.payload(),
                });
            }

            loop {
                let (key, lease) = retained.next()?;
                if matches_cohort(*key) {
                    return Some(RetainedLeaseResource {
                        key: *key,
                        payload: lease.payload(),
                    });
                }
            }
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.resources().count()
    }

    pub(crate) fn status(&self) -> RetainedLeaseStatus {
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
        let Some(resource) = self
            .resources()
            .find(|resource| region_contains(resource.key().region(), index))
        else {
            return RetainedLeaseSample::Missing;
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
        key: DatasetResourceKey,
        descriptor: ResourcePayloadDescriptor,
        values: Box<[u8]>,
        validity: Option<Box<[u8]>>,
    }

    impl FixtureLease {
        fn u16(
            key: DatasetResourceKey,
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
        fn key(&self) -> DatasetResourceKey {
            self.key
        }

        fn payload(&self) -> ResourcePayloadView<'_> {
            self.descriptor
                .view(&self.values, self.validity.as_deref())
                .expect("fixture lease preserves its validated payload")
        }
    }

    #[derive(Debug)]
    struct OuterLease(Arc<FixtureLease>);

    impl ResourceLease for OuterLease {
        fn key(&self) -> DatasetResourceKey {
            self.0.key()
        }

        fn payload(&self) -> ResourcePayloadView<'_> {
            self.0.payload()
        }
    }

    fn key(x: u64) -> DatasetResourceKey {
        DatasetResourceKey::new(
            DatasetResourceIdentity::Unverified(DatasetSourceId::new(7)),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, x], Shape3D::new(1, 1, 2).unwrap()).unwrap(),
        )
    }

    fn lease(
        key: DatasetResourceKey,
        values: &[u16],
        validity: Option<&[u8]>,
    ) -> Arc<dyn ResourceLease> {
        Arc::new(FixtureLease::u16(key, values, validity).unwrap())
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
    fn install_rejects_unrequired_and_conflicting_allocations() {
        let required = key(0);
        let unrequired = key(2);
        let inner = Arc::new(FixtureLease::u16(required, &[7, 9], None).unwrap());
        let first: Arc<dyn ResourceLease> = Arc::new(OuterLease(Arc::clone(&inner)));
        let same_payload: Arc<dyn ResourceLease> = Arc::new(OuterLease(inner));

        let mut retained = RetainedLeases::new();
        retained.replace_requirements([required]).unwrap();
        assert_eq!(retained.install(Arc::clone(&first)), Ok(true));
        assert_eq!(retained.install(same_payload), Ok(false));
        assert_eq!(
            retained.install(lease(unrequired, &[3, 4], None)),
            Err(RetainedLeaseError::ResourceNotRequired { key: unrequired })
        );
        assert_eq!(
            retained.install(lease(required, &[7, 9], None)),
            Err(RetainedLeaseError::ConflictingLeaseAllocation { key: required })
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
    }

    #[test]
    fn cohort_filters_requirements_and_samples_validity() {
        let first = key(0);
        let second = key(2);
        let missing = key(4);
        let other_timepoint = DatasetResourceKey::new(
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
        assert_eq!(
            subset
                .resources()
                .map(RetainedLeaseResource::key)
                .collect::<Vec<_>>(),
            vec![second]
        );
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
