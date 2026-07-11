use std::sync::Arc;

use mirante4d_dataset::{
    DatasetResourceIdentity, DatasetResourceKey, DatasetSourceId, ResourceContractError,
    ResourceLease, ResourcePayloadDescriptor, ResourcePayloadView, ResourceRegion,
    ResourceValidity,
};
use mirante4d_domain::{IntensityDType, LogicalLayerKey, ScaleLevel, Shape3D, TimeIndex};

use super::{CurrentLeaseBridge, CurrentLeaseBridgeError, CurrentLeaseSample};

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
fn retains_exact_lease_and_borrows_value_and_validity_allocations() {
    let key = key(0);
    let lease = lease(key, &[0, 41], Some(&[0b0000_0001]));
    let original = lease.payload();
    let original_values = original.value_bytes().as_ptr();
    let original_validity = original.validity_bits().unwrap().as_ptr();

    let mut bridge = CurrentLeaseBridge::new();
    assert_eq!(bridge.replace_current_requirements([key]), Ok(0));
    assert_eq!(bridge.install(Arc::clone(&lease)), Ok(true));
    assert!(Arc::ptr_eq(bridge.retained_lease(key).unwrap(), &lease));

    let bridged = bridge.payload(key).unwrap();
    assert_eq!(bridged.value_bytes().as_ptr(), original_values);
    assert_eq!(bridged.validity_bits().unwrap().as_ptr(), original_validity);
    assert!(bridged.sample_is_valid(0).unwrap());
    assert!(!bridged.sample_is_valid(1).unwrap());
    assert_eq!(&bridged.value_bytes()[..2], &[0, 0]);

    assert_eq!(bridge.install(Arc::clone(&lease)), Ok(false));
    drop(lease);
    assert_eq!(
        bridge.payload(key).unwrap().value_bytes().as_ptr(),
        original_values
    );
}

#[test]
fn requirements_coalesce_and_retire_obsolete_leases() {
    let first = key(0);
    let shared = key(2);
    let next = key(4);
    let first_lease = lease(first, &[1, 2], None);
    let shared_lease = lease(shared, &[3, 4], None);

    let mut bridge = CurrentLeaseBridge::new();
    assert_eq!(
        bridge.replace_current_requirements([first, shared, shared]),
        Ok(0)
    );
    assert_eq!(bridge.required_len(), 2);
    bridge.install(first_lease).unwrap();
    bridge.install(Arc::clone(&shared_lease)).unwrap();
    assert!(bridge.is_complete());

    assert_eq!(bridge.replace_current_requirements([shared, next]), Ok(1));
    assert!(bridge.payload(first).is_none());
    assert!(Arc::ptr_eq(
        bridge.retained_lease(shared).unwrap(),
        &shared_lease
    ));
    assert_eq!(bridge.retained_len(), 1);
    assert_eq!(bridge.missing_len(), 1);
    assert!(!bridge.is_complete());
}

#[test]
fn fresh_outer_arcs_for_one_payload_allocation_are_idempotent() {
    let key = key(0);
    let inner = Arc::new(FixtureLease::u16(key, &[7, 9], None).unwrap());
    let first: Arc<dyn ResourceLease> = Arc::new(OuterLease(Arc::clone(&inner)));
    let second: Arc<dyn ResourceLease> = Arc::new(OuterLease(inner));
    assert!(!Arc::ptr_eq(&first, &second));

    let mut bridge = CurrentLeaseBridge::new();
    bridge.replace_current_requirements([key]).unwrap();
    assert_eq!(bridge.install(Arc::clone(&first)), Ok(true));
    assert_eq!(bridge.install(second), Ok(false));
    assert!(Arc::ptr_eq(bridge.retained_lease(key).unwrap(), &first));
}

#[test]
fn rejects_unrequired_or_conflicting_allocations_without_mutation() {
    let required = key(0);
    let unrequired = key(2);
    let retained = lease(required, &[1, 2], None);
    let conflict = lease(required, &[1, 2], None);

    let mut bridge = CurrentLeaseBridge::new();
    bridge.replace_current_requirements([required]).unwrap();
    assert_eq!(bridge.install(Arc::clone(&retained)), Ok(true));
    assert_eq!(
        bridge.install(lease(unrequired, &[3, 4], None)),
        Err(CurrentLeaseBridgeError::ResourceNotRequired { key: unrequired })
    );
    assert_eq!(
        bridge.install(conflict),
        Err(CurrentLeaseBridgeError::ConflictingLeaseAllocation { key: required })
    );
    assert!(Arc::ptr_eq(
        bridge.retained_lease(required).unwrap(),
        &retained
    ));
    assert_eq!(bridge.retained_len(), 1);
}

#[test]
fn over_limit_requirement_update_is_atomic() {
    let retained_key = key(0);
    let retained = lease(retained_key, &[1, 2], None);
    let mut bridge = CurrentLeaseBridge::new();
    bridge.replace_current_requirements([retained_key]).unwrap();
    bridge.install(Arc::clone(&retained)).unwrap();

    assert_eq!(
        bridge.replace_current_requirements_with_limit([key(2), key(4), key(6)], 2),
        Err(CurrentLeaseBridgeError::TooManyRequirements {
            actual: 3,
            maximum: 2,
        })
    );
    assert!(bridge.requires(retained_key));
    assert!(Arc::ptr_eq(
        bridge.retained_lease(retained_key).unwrap(),
        &retained
    ));
}

#[test]
fn resident_set_filters_semantic_cohort_and_samples_validity_without_copying() {
    let first = key(0);
    let second = key(2);
    let other_timepoint = DatasetResourceKey::new(
        first.identity(),
        first.layer(),
        TimeIndex::new(1),
        first.scale(),
        first.region(),
    );
    let first_lease = lease(first, &[0, 11], Some(&[0b0000_0001]));
    let second_lease = lease(second, &[22, 33], None);
    let other_lease = lease(other_timepoint, &[44, 55], None);
    let second_values = second_lease.payload().value_bytes().as_ptr();

    let mut bridge = CurrentLeaseBridge::new();
    bridge
        .replace_current_requirements([first, second, other_timepoint])
        .unwrap();
    bridge.install(first_lease).unwrap();
    bridge.install(second_lease).unwrap();
    bridge.install(other_lease).unwrap();

    let resident = bridge.resident_set(
        first.identity(),
        first.layer(),
        first.timepoint(),
        first.scale(),
    );
    assert_eq!(resident.status().required, 2);
    assert_eq!(resident.status().retained, 2);
    assert_eq!(resident.status().missing, 0);
    assert_eq!(resident.len(), 2);
    assert_eq!(resident.sample([0, 0, 0]), CurrentLeaseSample::Uint16(0));
    assert_eq!(
        resident.sample([0, 0, 1]),
        CurrentLeaseSample::InvalidNoData
    );
    assert_eq!(resident.sample([0, 0, 2]), CurrentLeaseSample::Uint16(22));
    assert_eq!(resident.sample([0, 0, 9]), CurrentLeaseSample::Missing);
    let second = resident
        .resources()
        .find(|resource| resource.key() == second)
        .unwrap();
    assert_eq!(second.payload().value_bytes().as_ptr(), second_values);

    let linked_requirements = [second.key()];
    let linked = bridge.resident_subset(
        &linked_requirements,
        first.identity(),
        first.layer(),
        first.timepoint(),
        first.scale(),
    );
    assert_eq!(linked.status().required, 1);
    assert_eq!(linked.status().retained, 1);
    assert_eq!(linked.len(), 1);
    assert_eq!(linked.sample([0, 0, 0]), CurrentLeaseSample::Missing);
    assert_eq!(linked.sample([0, 0, 2]), CurrentLeaseSample::Uint16(22));
}

#[test]
fn resident_subset_selects_requested_keys_from_many_retained_leases() {
    let retained_keys = (0..128).map(|index| key(index * 2)).collect::<Vec<_>>();
    let first = retained_keys[7];
    let second = retained_keys[103];
    let missing = key(512);
    let mut bridge = CurrentLeaseBridge::new();
    bridge
        .replace_current_requirements(
            retained_keys
                .iter()
                .copied()
                .chain(std::iter::once(missing)),
        )
        .unwrap();
    for (index, key) in retained_keys.iter().copied().enumerate() {
        bridge
            .install(lease(key, &[u16::try_from(index).unwrap(), 0], None))
            .unwrap();
    }

    let subset_requirements = [second, missing, first, second];
    let subset = bridge.resident_subset(
        &subset_requirements,
        first.identity(),
        first.layer(),
        first.timepoint(),
        first.scale(),
    );

    assert_eq!(
        subset
            .resources()
            .map(|resource| resource.key())
            .collect::<Vec<_>>(),
        vec![first, second]
    );
    assert_eq!(subset.len(), 2);
    assert_eq!(
        subset.sample(first.region().origin()),
        CurrentLeaseSample::Uint16(7)
    );
    assert_eq!(
        subset.sample(second.region().origin()),
        CurrentLeaseSample::Uint16(103)
    );
    assert_eq!(
        subset.sample(retained_keys[64].region().origin()),
        CurrentLeaseSample::Missing
    );
}
