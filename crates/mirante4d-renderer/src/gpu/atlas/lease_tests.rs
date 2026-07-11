use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use mirante4d_dataset::{
    CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError, DatasetResourceIdentity,
    DatasetResourceKey, DatasetSourceId, ResourceLease, ResourcePayloadDescriptor,
    ResourcePayloadView, ResourceRegion, ResourceValidity,
};
use mirante4d_domain::{
    GridToWorld, IntensityDType, LogicalLayerKey, ScaleLevel, Shape3D, TimeIndex,
};

use crate::{CurrentLeaseBridge, CurrentLeaseVolume};

use super::{
    F32UploadBytes, IntegerAtlasDType, UploadReadyIntegerBrickCache, validate_lease_pages,
};

struct FixtureLease {
    key: DatasetResourceKey,
    descriptor: ResourcePayloadDescriptor,
    values: Box<[u8]>,
    validity: Option<Box<[u8]>>,
}

impl ResourceLease for FixtureLease {
    fn key(&self) -> DatasetResourceKey {
        self.key
    }

    fn payload(&self) -> ResourcePayloadView<'_> {
        self.descriptor
            .view(&self.values, self.validity.as_deref())
            .unwrap()
    }
}

#[derive(Clone, Default)]
struct TestLedger {
    used: Arc<AtomicU64>,
}

struct TestCharge {
    used: Arc<AtomicU64>,
    bytes: u64,
}

impl CpuByteLedger for TestLedger {
    fn try_acquire(
        &self,
        _category: CpuLedgerCategory,
        bytes: u64,
    ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
        if bytes == 0 {
            return Err(CpuLedgerError::ZeroByteReservation);
        }
        self.used.fetch_add(bytes, Ordering::SeqCst);
        Ok(Box::new(TestCharge {
            used: Arc::clone(&self.used),
            bytes,
        }))
    }
}

impl CpuByteLease for TestCharge {
    fn category(&self) -> CpuLedgerCategory {
        CpuLedgerCategory::UploadStaging
    }

    fn reserved_bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for TestCharge {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}

#[test]
fn semantic_integer_pages_pack_validity_and_hold_exact_staging_charge() {
    let identity = DatasetResourceIdentity::Unverified(DatasetSourceId::new(19));
    let layer = LogicalLayerKey::new(2);
    let first = key(identity, layer, [0, 0, 0], [2, 2, 2]);
    let edge = key(identity, layer, [0, 0, 2], [2, 2, 1]);
    let first_lease = u16_lease(first, &[0, 1, 2, 3, 4, 5, 6, 7], Some(&[0b1111_1101]));
    let edge_lease = u16_lease(edge, &[8, 9, 10, 11], None);
    let mut bridge = CurrentLeaseBridge::new();
    bridge.replace_current_requirements([first, edge]).unwrap();
    bridge.install(first_lease).unwrap();
    bridge.install(edge_lease).unwrap();
    let volume = CurrentLeaseVolume::new(
        bridge.resident_set(identity, layer, TimeIndex::new(0), ScaleLevel::BASE),
        Shape3D::new(2, 2, 3).unwrap(),
        Shape3D::new(2, 2, 2).unwrap(),
        GridToWorld::identity(),
    );

    let pages = validate_lease_pages(volume, IntensityDType::Uint16, None).unwrap();
    assert_eq!(pages.len(), 2);
    assert_eq!((pages[0].brick_index.x, pages[1].brick_index.x), (0, 1));

    let ledger = TestLedger::default();
    let mut cache = UploadReadyIntegerBrickCache::new(1_024);
    let packed = cache
        .get_or_pack(
            pages[0],
            volume.resource_shape(),
            4,
            1,
            IntegerAtlasDType::U16,
            &ledger,
        )
        .unwrap();
    assert_eq!(ledger.used.load(Ordering::SeqCst), 20);
    assert_eq!(packed.values[0] & 0xffff, 0);
    assert_eq!((packed.values[0] >> 16) & 0xffff, 0);
    assert_eq!(packed.validity_bits[0] & 0b11, 0b01);
    assert_eq!(packed.valid_voxel_count, 7);
    assert_eq!((packed.min_value, packed.max_value), (0, 7));
    drop(packed);
    assert_eq!(ledger.used.load(Ordering::SeqCst), 20);
    drop(cache);
    assert_eq!(ledger.used.load(Ordering::SeqCst), 0);
}

#[test]
fn float32_upload_borrows_all_valid_bytes_and_charges_only_validity_rewrite() {
    let ledger = TestLedger::default();
    let values = [1.5_f32.to_le_bytes(), 2.5_f32.to_le_bytes()].concat();
    let shape = Shape3D::new(1, 1, 2).unwrap();
    let direct = ResourcePayloadView::new(
        IntensityDType::Float32,
        shape,
        ResourceValidity::AllValid,
        &values,
        None,
    )
    .unwrap();
    let upload = F32UploadBytes::new(direct, &ledger).unwrap();
    assert_eq!(upload.bytes().as_ptr(), values.as_ptr());
    assert_eq!(ledger.used.load(Ordering::SeqCst), 0);

    let validity = [0b0000_0001];
    let masked = ResourcePayloadView::new(
        IntensityDType::Float32,
        shape,
        ResourceValidity::BitMask,
        &values,
        Some(&validity),
    )
    .unwrap();
    let staged = F32UploadBytes::new(masked, &ledger).unwrap();
    assert_eq!(ledger.used.load(Ordering::SeqCst), 8);
    assert_eq!(&staged.bytes()[..4], &1.5_f32.to_le_bytes());
    assert!(f32::from_le_bytes(staged.bytes()[4..8].try_into().unwrap()).is_nan());
    drop(staged);
    assert_eq!(ledger.used.load(Ordering::SeqCst), 0);
}

fn key(
    identity: DatasetResourceIdentity,
    layer: LogicalLayerKey,
    origin: [u64; 3],
    shape: [u64; 3],
) -> DatasetResourceKey {
    DatasetResourceKey::new(
        identity,
        layer,
        TimeIndex::new(0),
        ScaleLevel::BASE,
        ResourceRegion::new(origin, Shape3D::new(shape[0], shape[1], shape[2]).unwrap()).unwrap(),
    )
}

fn u16_lease(
    key: DatasetResourceKey,
    values: &[u16],
    validity: Option<&[u8]>,
) -> Arc<dyn ResourceLease> {
    let descriptor = ResourcePayloadDescriptor::new(
        IntensityDType::Uint16,
        key.region().shape(),
        if validity.is_some() {
            ResourceValidity::BitMask
        } else {
            ResourceValidity::AllValid
        },
    )
    .unwrap();
    Arc::new(FixtureLease {
        key,
        descriptor,
        values: values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        validity: validity.map(|bits| bits.to_vec().into_boxed_slice()),
    })
}
