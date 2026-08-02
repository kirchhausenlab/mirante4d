//! Target-package adapter for the storage-independent dataset contract.

#![allow(
    clippy::result_large_err,
    reason = "the frozen DatasetSource contract requires context-rich typed faults"
)]

use std::{
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use mirante4d_dataset::{
    BrickKey, ContentAddressStatus, CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError,
    DatasetCatalog, DatasetCatalogError, DatasetLayer, DatasetResourceIdentity, DatasetScale,
    DatasetSource, DatasetSourceFault, DatasetSourceId, DecodeSinkError, ReservedDecodeSink,
    ResourceContractError, ResourcePayloadDescriptor, ResourcePayloadFacts, ResourceValidity,
};
use mirante4d_domain::{GridToWorld, IntensityDType, ScaleLevel, Shape3D};
use mirante4d_identity::PackageId;
use thiserror::Error;

use crate::package_read::{
    DirectPayloadFactsAuthority, LocalDirectBrickRead, LocalDirectBrickReadError,
    read_local_brick_reusing_in_transaction, read_local_brick_reusing_into_sink_in_transaction,
};
use crate::range_io::{
    LOCAL_OBJECT_CACHE_ACCOUNTED_BYTES_MAX, LocalCurrentnessBatch, LocalPackageReadDiagnostics,
    LocalPackageReader,
};
use crate::{
    INNER_CODEC_WORKING_BYTES_MAX, LocalBrickRead, LocalPackageCatalog, OmeLevelTransform,
    PackageAdmissionError, PackagePath, PackageReadError, PackedIndexCoordinates,
    ProfileValidityMode, RangeReadError, SelfConsistentPackageCapability, ShardProfileKind,
};

const METADATA_MIN_BYTES: u64 = 64 * 1024;
const METADATA_ENCODED_MULTIPLIER: u64 = 8;
const SINK_WRITE_CHUNK_BYTES: usize = 8 * 1024;
const PHYSICAL_BRICK_CACHE_ENTRIES_MAX: usize = 8;
const PHYSICAL_CACHE_WAIT_POLL: Duration = Duration::from_millis(5);
const PHYSICAL_BRICK_OBJECT_SNAPSHOTS_MAX: usize = 3;

/// Conservative caller reservation for one exact-plus-scientific validation.
///
/// The accepted validators retain bounded metadata, one 64 KiB hash buffer,
/// four fixed scientific tiles, one physical brick, the shared native codec
/// workspace, and a bounded tile-digest slab. Explicit integrity audit and
/// import publication acquire this from `InFlightDecode` before invoking the
/// validators.
pub const PACKAGE_VALIDATION_WORKING_BYTES: u64 = 64 * 1024 * 1024;

const _: () = assert!(PACKAGE_VALIDATION_WORKING_BYTES >= crate::INNER_CODEC_WORKING_BYTES_MAX);

/// Failure while binding an opened target package to the dataset contract.
#[derive(Debug, Error)]
pub enum LocalDatasetSourceOpenError {
    #[error("target-package metadata accounting overflowed")]
    MetadataAccountingOverflow,
    #[error("target-package metadata admission failed: {0}")]
    MetadataAdmission(#[source] CpuLedgerError),
    #[error("the CPU byte ledger returned an invalid metadata lease")]
    InvalidMetadataLease,
    #[error("target package does not fit a supported dataset profile: {0}")]
    Admission(#[source] PackageAdmissionError),
    #[error(transparent)]
    Catalog(#[from] DatasetCatalogError),
    #[error("target-package metadata is inconsistent: {reason}")]
    MetadataInvariant { reason: &'static str },
}

enum LocalPackageAccess {
    Admitted(Box<LocalPackageCatalog>),
    Published(Box<SelfConsistentPackageCapability>),
}

impl LocalPackageAccess {
    const fn storage_catalog(&self) -> &LocalPackageCatalog {
        match self {
            Self::Admitted(catalog) => catalog,
            Self::Published(capability) => capability.catalog(),
        }
    }

    const fn package_id(&self) -> Option<PackageId> {
        match self {
            Self::Admitted(_) => None,
            Self::Published(capability) => Some(capability.package_id()),
        }
    }

    fn runtime_read_object_capacity(&self) -> usize {
        match self {
            Self::Admitted(catalog) => catalog.runtime_read_object_capacity(),
            Self::Published(capability) => capability.runtime_read_object_capacity(),
        }
    }

    fn read_brick(
        &self,
        coordinates: PackedIndexCoordinates,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<LocalBrickRead, PackageReadError> {
        if is_cancelled() {
            return Err(PackageReadError::Cancelled);
        }
        let read = match self {
            Self::Admitted(catalog) => catalog.read_brick_admitted(coordinates),
            Self::Published(capability) => capability.read_brick(coordinates, &mut is_cancelled),
        }?;
        if is_cancelled() {
            Err(PackageReadError::Cancelled)
        } else {
            Ok(read)
        }
    }

    fn begin_runtime_read_cohort(
        &self,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<LocalCurrentnessBatch<'_>, PackageReadError> {
        if is_cancelled() {
            return Err(PackageReadError::Cancelled);
        }
        match self {
            Self::Admitted(catalog) => catalog
                .reader()
                .begin_cached_read_transaction_with_capacity(self.runtime_read_object_capacity())
                .map_err(PackageReadError::from),
            Self::Published(capability) => capability.begin_runtime_read_cohort(is_cancelled),
        }
    }

    fn read_brick_into_sink_in_cohort(
        &self,
        coordinates: PackedIndexCoordinates,
        sink: &mut dyn ReservedDecodeSink,
        transaction: &mut LocalCurrentnessBatch<'_>,
    ) -> Result<LocalDirectBrickRead, LocalDirectBrickReadError> {
        if sink.is_cancelled() {
            return Err(LocalDirectBrickReadError::Package(
                PackageReadError::Cancelled,
            ));
        }
        let read = match self {
            Self::Admitted(catalog) => {
                let plan = catalog.plan_brick_storage(coordinates)?;
                read_local_brick_reusing_into_sink_in_transaction(
                    catalog.reader(),
                    catalog.descriptors(),
                    plan,
                    sink,
                    transaction,
                    DirectPayloadFactsAuthority::ScanDecoded,
                )
            }
            Self::Published(capability) => {
                capability.read_brick_into_sink_in_cohort(coordinates, sink, transaction)
            }
        }?;
        if sink.is_cancelled() {
            Err(LocalDirectBrickReadError::Package(
                PackageReadError::Cancelled,
            ))
        } else {
            Ok(read)
        }
    }

    fn read_brick_in_cohort(
        &self,
        coordinates: PackedIndexCoordinates,
        transaction: &mut LocalCurrentnessBatch<'_>,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<LocalBrickRead, PackageReadError> {
        if is_cancelled() {
            return Err(PackageReadError::Cancelled);
        }
        match self {
            Self::Admitted(catalog) => {
                let plan = catalog.plan_brick_storage(coordinates)?;
                read_local_brick_reusing_in_transaction(
                    catalog.reader(),
                    catalog.descriptors(),
                    plan,
                    transaction,
                    DirectPayloadFactsAuthority::ScanDecoded,
                )
            }
            Self::Published(capability) => {
                capability.read_brick_in_cohort(coordinates, transaction, is_cancelled)
            }
        }
    }

    fn validate_cached_brick_in_cohort(
        &self,
        brick: &LocalBrickRead,
        transaction: &mut LocalCurrentnessBatch<'_>,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), PackageReadError> {
        if is_cancelled() {
            return Err(PackageReadError::Cancelled);
        }
        match self {
            Self::Admitted(_) => {
                for snapshot in brick.object_snapshots() {
                    if is_cancelled() {
                        return Err(PackageReadError::Cancelled);
                    }
                    transaction.validate_snapshot(snapshot)?;
                }
                Ok(())
            }
            Self::Published(capability) => {
                capability.validate_cached_brick_in_cohort(brick, transaction, is_cancelled)
            }
        }
    }

    const fn reader(&self) -> &LocalPackageReader {
        self.storage_catalog().reader()
    }

    fn revalidate_cached_brick(
        &self,
        brick: &LocalBrickRead,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), PackageReadError> {
        if is_cancelled() {
            return Err(PackageReadError::Cancelled);
        }
        match self {
            Self::Admitted(catalog) => catalog
                .reader()
                .revalidate_cached_snapshots(brick.object_snapshots())
                .map_err(PackageReadError::from),
            Self::Published(capability) => {
                capability.revalidate_cached_brick(brick, &mut is_cancelled)
            }
        }
    }
}

#[derive(Clone)]
struct LocalPackageAccessSnapshot {
    access: Arc<LocalPackageAccess>,
}

#[derive(Clone, Copy)]
struct LayerStorageMapping {
    image: u32,
    physical_channel: u32,
    brick_shape: [u64; 3],
}

#[derive(Clone, Copy)]
struct AlignedCohortMember {
    sink_index: usize,
    key: BrickKey,
    descriptor: ResourcePayloadDescriptor,
    mapping: LayerStorageMapping,
    coordinates: PackedIndexCoordinates,
}

#[derive(Clone, Copy)]
struct UnalignedCohortMember {
    sink_index: usize,
    key: BrickKey,
    descriptor: ResourcePayloadDescriptor,
    mapping: LayerStorageMapping,
}

struct UnalignedMemberState {
    member: UnalignedCohortMember,
    staging: Vec<u8>,
    facts: PayloadFactsAccumulator,
    _staging_lease: Box<dyn CpuByteLease>,
}

#[derive(Clone, Copy)]
struct UnalignedPhysicalWork {
    coordinates: PackedIndexCoordinates,
    brick_coordinates: [u64; 3],
    state_index: usize,
}

/// Cancellation view for one physical read shared by several semantic sinks.
///
/// A member-local cancellation must not abort a decode still needed by a live
/// sibling. The physical cache's loading interest is therefore cancelled only
/// when every semantic consumer in this coordinate group has cancelled.
struct PhysicalFanoutCancellationSink<'sinks, 'sink> {
    sinks: &'sinks [&'sink mut dyn ReservedDecodeSink],
    sink_indices: &'sinks [usize],
    key: BrickKey,
    descriptor: ResourcePayloadDescriptor,
}

impl ReservedDecodeSink for PhysicalFanoutCancellationSink<'_, '_> {
    fn resource_key(&self) -> BrickKey {
        self.key
    }

    fn payload_descriptor(&self) -> ResourcePayloadDescriptor {
        self.descriptor
    }

    fn written_bytes(&self) -> u64 {
        0
    }

    fn is_cancelled(&self) -> bool {
        self.sink_indices
            .iter()
            .all(|&index| self.sinks[index].is_cancelled())
    }

    fn write(&mut self, _bytes: &[u8]) -> Result<(), DecodeSinkError> {
        Err(DecodeSinkError::DirectWriteUnsupported)
    }

    fn finish(&mut self) -> Result<(), DecodeSinkError> {
        Err(DecodeSinkError::DirectWriteUnsupported)
    }

    fn is_finished(&self) -> bool {
        false
    }
}

/// Cumulative storage-source work and current bounded physical reuse state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalDatasetSourceDiagnostics {
    pub physical_brick_requests: u64,
    pub physical_brick_cache_hits: u64,
    pub physical_brick_cache_misses: u64,
    pub physical_brick_cache_waits: u64,
    pub physical_brick_cache_evictions: u64,
    pub physical_brick_cache_capacity_bypasses: u64,
    pub physical_brick_unique_decodes: u64,
    /// Pixel and validity payload bytes produced by unique physical decodes.
    ///
    /// Actual codec output, including decoded shard indexes, is reported by
    /// `reader.codec_decoded_bytes`.
    pub physical_brick_unique_decoded_bytes: u64,
    /// Full 3D bricks decoded through the bounded streaming sink path without
    /// decoded physical residency or a payload-sized intermediate allocation.
    pub aligned_direct_deliveries: u64,
    pub aligned_direct_streamed_bytes: u64,
    /// Codec/zero-fill bytes committed directly in runtime-owned writable
    /// spans with no decoded payload-sized copy.
    pub aligned_direct_sink_span_bytes: u64,
    /// Already-decoded aligned bytes copied into a sink after decode. The
    /// common fact-checked all-valid 3D path is expected to remain zero.
    pub aligned_direct_post_decode_copy_bytes: u64,
    pub physical_brick_cache_entries: u64,
    pub physical_brick_cache_bytes: u64,
    pub physical_brick_cache_peak_bytes: u64,
    pub contiguous_copy_bytes: u64,
    pub scalar_copy_samples: u64,
    pub sink_write_bytes: u64,
    pub reader: LocalPackageReadDiagnostics,
}

#[derive(Default)]
struct LocalDatasetSourceCounters {
    physical_brick_requests: AtomicU64,
    physical_brick_cache_hits: AtomicU64,
    physical_brick_cache_misses: AtomicU64,
    physical_brick_cache_waits: AtomicU64,
    physical_brick_cache_evictions: AtomicU64,
    physical_brick_cache_capacity_bypasses: AtomicU64,
    physical_brick_unique_decodes: AtomicU64,
    physical_brick_unique_decoded_bytes: AtomicU64,
    aligned_direct_deliveries: AtomicU64,
    aligned_direct_streamed_bytes: AtomicU64,
    aligned_direct_sink_span_bytes: AtomicU64,
    aligned_direct_post_decode_copy_bytes: AtomicU64,
    contiguous_copy_bytes: AtomicU64,
    scalar_copy_samples: AtomicU64,
    sink_write_bytes: AtomicU64,
}

struct PhysicalBrickCache {
    state: Mutex<PhysicalBrickCacheState>,
    wake: Condvar,
}

#[derive(Default)]
struct PhysicalBrickCacheState {
    entries: Vec<PhysicalBrickCacheEntry>,
    next_touch: u64,
    next_generation: u64,
    retained_bytes: u64,
    peak_retained_bytes: u64,
}

struct PhysicalBrickCacheEntry {
    coordinates: PackedIndexCoordinates,
    generation: u64,
    last_touch: u64,
    value: PhysicalBrickCacheValue,
}

enum PhysicalBrickCacheValue {
    Loading {
        interests: usize,
        cancellation: Arc<AtomicBool>,
    },
    /// A completed physical decode which could not enter decoded residency.
    ///
    /// Every interest which joined the corresponding loading generation gets
    /// the same result. The shared owner keeps the one in-flight lease alive
    /// until the last semantic caller has finished copying from the brick.
    TransientReady {
        shared: Arc<TransientPhysicalBrick>,
        remaining_interests: usize,
    },
    Ready {
        brick: Arc<LocalBrickRead>,
        _lease: Box<dyn CpuByteLease>,
        bytes: u64,
    },
}

#[derive(Clone)]
struct PhysicalLoadingTicket {
    generation: u64,
    cancellation: Arc<AtomicBool>,
}

struct TransientPhysicalBrick {
    brick: Arc<LocalBrickRead>,
    _retention_lease: Box<dyn CpuByteLease>,
}

struct PhysicalDecodeScratch {
    transient_retention: Box<dyn CpuByteLease>,
    workspace: Box<dyn CpuByteLease>,
}

impl PhysicalDecodeScratch {
    fn into_transient_retention(self) -> Box<dyn CpuByteLease> {
        let Self {
            transient_retention,
            workspace,
        } = self;
        // Encoded buffers, shard-index tails, and the native codec workspace
        // are dead once decode finishes. Only the bounded decoded brick may
        // outlive this point while joined readers fan it out.
        drop(workspace);
        transient_retention
    }
}

/// A physical brick borrowed by one semantic decode. Cache hits are charged
/// by the cache entry; capacity-bypass results retain their in-flight charge
/// until the semantic caller has finished copying from the brick.
struct PhysicalBrickRead {
    brick: Arc<LocalBrickRead>,
    _temporary_owner: Option<Arc<TransientPhysicalBrick>>,
}

struct PhysicalReadCohort<'access, 'transaction, 'reader> {
    accepted: &'access LocalPackageAccessSnapshot,
    transaction: &'transaction mut LocalCurrentnessBatch<'reader>,
}

impl std::ops::Deref for PhysicalBrickRead {
    type Target = LocalBrickRead;

    fn deref(&self) -> &Self::Target {
        &self.brick
    }
}

impl Default for PhysicalBrickCache {
    fn default() -> Self {
        Self {
            state: Mutex::new(PhysicalBrickCacheState::default()),
            wake: Condvar::new(),
        }
    }
}

/// One target-package source for the shared dataset scheduler.
///
/// Ordinary admitted construction uses a caller-assigned opaque resource ID
/// while exposing the content address declared by the manifest. Published
/// construction accepts only the self-consistency capability handed across
/// the import publication boundary. The caller supplies the human display
/// label; it is UI metadata, not a persisted identity or content address.
pub struct LocalDatasetSource {
    access: Arc<LocalPackageAccess>,
    catalog: Arc<DatasetCatalog>,
    mappings: Vec<LayerStorageMapping>,
    ledger: Arc<dyn CpuByteLedger>,
    physical_cache: PhysicalBrickCache,
    reader_diagnostics_baseline: LocalPackageReadDiagnostics,
    counters: LocalDatasetSourceCounters,
    _metadata_lease: Box<dyn CpuByteLease>,
}

impl LocalDatasetSource {
    pub fn from_admitted(
        storage: LocalPackageCatalog,
        source_id: DatasetSourceId,
        display_label: impl AsRef<str>,
        ledger: Arc<dyn CpuByteLedger>,
    ) -> Result<Arc<Self>, LocalDatasetSourceOpenError> {
        let content_id = storage.science().scientific_content_id();
        Self::new(
            LocalPackageAccess::Admitted(Box::new(storage)),
            ContentAddressStatus::ContentAddress(content_id),
            DatasetResourceIdentity::SessionLocal(source_id),
            display_label.as_ref(),
            ledger,
        )
    }

    pub fn from_published(
        capability: SelfConsistentPackageCapability,
        display_label: impl AsRef<str>,
        ledger: Arc<dyn CpuByteLedger>,
    ) -> Result<Arc<Self>, LocalDatasetSourceOpenError> {
        let scientific_content_id = capability.scientific_content_id();
        Self::new(
            LocalPackageAccess::Published(Box::new(capability)),
            ContentAddressStatus::ContentAddress(scientific_content_id),
            DatasetResourceIdentity::ContentAddress(scientific_content_id),
            display_label.as_ref(),
            ledger,
        )
    }

    fn new(
        access: LocalPackageAccess,
        content_address_status: ContentAddressStatus,
        resource_identity: DatasetResourceIdentity,
        display_label: &str,
        ledger: Arc<dyn CpuByteLedger>,
    ) -> Result<Arc<Self>, LocalDatasetSourceOpenError> {
        let metadata_bytes = access
            .storage_catalog()
            .metadata_bytes_read()
            .checked_mul(METADATA_ENCODED_MULTIPLIER)
            .ok_or(LocalDatasetSourceOpenError::MetadataAccountingOverflow)?
            .max(METADATA_MIN_BYTES)
            .checked_add(LOCAL_OBJECT_CACHE_ACCOUNTED_BYTES_MAX)
            .ok_or(LocalDatasetSourceOpenError::MetadataAccountingOverflow)?;
        let metadata_lease = ledger
            .try_acquire(CpuLedgerCategory::MetadataAndIndexes, metadata_bytes)
            .map_err(LocalDatasetSourceOpenError::MetadataAdmission)?;
        if metadata_lease.category() != CpuLedgerCategory::MetadataAndIndexes
            || metadata_lease.reserved_bytes() != metadata_bytes
        {
            return Err(LocalDatasetSourceOpenError::InvalidMetadataLease);
        }
        if matches!(&access, LocalPackageAccess::Admitted(_)) {
            access
                .storage_catalog()
                .admit_supported_dataset_profile(|| false)
                .map_err(LocalDatasetSourceOpenError::Admission)?;
        }

        let (catalog, mappings) = build_dataset_catalog(
            access.storage_catalog(),
            content_address_status,
            resource_identity,
            display_label,
        )?;
        let access = Arc::new(access);
        let reader_diagnostics_baseline = access.reader().read_diagnostics();
        Ok(Arc::new(Self {
            access,
            catalog: Arc::new(catalog),
            mappings,
            ledger,
            physical_cache: PhysicalBrickCache::default(),
            reader_diagnostics_baseline,
            counters: LocalDatasetSourceCounters::default(),
            _metadata_lease: metadata_lease,
        }))
    }

    /// Returns the exact package identity only for an importer-published
    /// source whose publication capability already owns that address.
    pub fn package_id(&self) -> Option<PackageId> {
        self.access.package_id()
    }

    pub fn diagnostics(&self) -> LocalDatasetSourceDiagnostics {
        let cache = lock_unpoisoned(&self.physical_cache.state);
        let current_reader = self.access.reader().read_diagnostics();
        let mut reader =
            subtract_reader_diagnostics(current_reader, self.reader_diagnostics_baseline);
        // Gauges describe the live reader and must never be
        // baseline-subtracted as though they were event counters.
        reader.open_object_handles_current = current_reader.open_object_handles_current;
        reader.open_object_handles_peak = current_reader.open_object_handles_peak;
        reader.object_handle_cache_entries = current_reader.object_handle_cache_entries;
        reader.object_handle_cache_peak_entries = current_reader.object_handle_cache_peak_entries;
        LocalDatasetSourceDiagnostics {
            physical_brick_requests: self
                .counters
                .physical_brick_requests
                .load(Ordering::Relaxed),
            physical_brick_cache_hits: self
                .counters
                .physical_brick_cache_hits
                .load(Ordering::Relaxed),
            physical_brick_cache_misses: self
                .counters
                .physical_brick_cache_misses
                .load(Ordering::Relaxed),
            physical_brick_cache_waits: self
                .counters
                .physical_brick_cache_waits
                .load(Ordering::Relaxed),
            physical_brick_cache_evictions: self
                .counters
                .physical_brick_cache_evictions
                .load(Ordering::Relaxed),
            physical_brick_cache_capacity_bypasses: self
                .counters
                .physical_brick_cache_capacity_bypasses
                .load(Ordering::Relaxed),
            physical_brick_unique_decodes: self
                .counters
                .physical_brick_unique_decodes
                .load(Ordering::Relaxed),
            physical_brick_unique_decoded_bytes: self
                .counters
                .physical_brick_unique_decoded_bytes
                .load(Ordering::Relaxed),
            aligned_direct_deliveries: self
                .counters
                .aligned_direct_deliveries
                .load(Ordering::Relaxed),
            aligned_direct_streamed_bytes: self
                .counters
                .aligned_direct_streamed_bytes
                .load(Ordering::Relaxed),
            aligned_direct_sink_span_bytes: self
                .counters
                .aligned_direct_sink_span_bytes
                .load(Ordering::Relaxed),
            aligned_direct_post_decode_copy_bytes: self
                .counters
                .aligned_direct_post_decode_copy_bytes
                .load(Ordering::Relaxed),
            physical_brick_cache_entries: cache
                .entries
                .iter()
                .filter(|entry| matches!(entry.value, PhysicalBrickCacheValue::Ready { .. }))
                .count() as u64,
            physical_brick_cache_bytes: cache.retained_bytes,
            physical_brick_cache_peak_bytes: cache.peak_retained_bytes,
            contiguous_copy_bytes: self.counters.contiguous_copy_bytes.load(Ordering::Relaxed),
            scalar_copy_samples: self.counters.scalar_copy_samples.load(Ordering::Relaxed),
            sink_write_bytes: self.counters.sink_write_bytes.load(Ordering::Relaxed),
            reader,
        }
    }

    fn access_snapshot(&self) -> LocalPackageAccessSnapshot {
        LocalPackageAccessSnapshot {
            access: Arc::clone(&self.access),
        }
    }

    fn finish_physical_delivery(
        &self,
        accepted: LocalPackageAccessSnapshot,
        brick: &LocalBrickRead,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), PackageReadError> {
        self.finish_snapshot_delivery(accepted, brick.object_snapshots(), is_cancelled)
    }

    fn finish_snapshot_delivery(
        &self,
        _accepted: LocalPackageAccessSnapshot,
        _snapshots: &[crate::range_io::LocalObjectSnapshot],
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), PackageReadError> {
        if is_cancelled() {
            Err(PackageReadError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn revalidate_physical_delivery(
        &self,
        brick: &LocalBrickRead,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), PackageReadError> {
        let access = self.access_snapshot();
        access
            .access
            .revalidate_cached_brick(brick, &mut is_cancelled)?;
        self.finish_physical_delivery(access, brick, is_cancelled)
    }

    fn mapping(&self, key: BrickKey) -> Result<LayerStorageMapping, DatasetSourceFault> {
        let index = usize::try_from(key.layer().ordinal())
            .map_err(|_| DatasetSourceFault::DecodeFailed { key })?;
        self.mappings
            .get(index)
            .copied()
            .ok_or(DatasetSourceFault::DecodeFailed { key })
    }

    fn checkpoint(sink: &dyn ReservedDecodeSink, key: BrickKey) -> Result<(), DatasetSourceFault> {
        if sink.is_cancelled() {
            Err(DatasetSourceFault::Cancelled { key })
        } else {
            Ok(())
        }
    }

    fn acquire_semantic_staging(
        &self,
        key: BrickKey,
        descriptor: ResourcePayloadDescriptor,
    ) -> Result<Box<dyn CpuByteLease>, DatasetSourceFault> {
        let bytes = descriptor.byte_len();
        let lease = self
            .ledger
            .try_acquire(CpuLedgerCategory::InFlightDecode, bytes)
            .map_err(|error| map_ledger_error(key, bytes, error))?;
        if lease.category() != CpuLedgerCategory::InFlightDecode || lease.reserved_bytes() != bytes
        {
            return Err(DatasetSourceFault::DecodeFailed { key });
        }
        Ok(lease)
    }

    fn acquire_unaligned_control(
        &self,
        key: BrickKey,
        bytes: u64,
    ) -> Result<Box<dyn CpuByteLease>, DatasetSourceFault> {
        let lease = self
            .ledger
            .try_acquire(CpuLedgerCategory::InFlightDecode, bytes)
            .map_err(|error| map_ledger_error(key, bytes, error))?;
        if lease.category() != CpuLedgerCategory::InFlightDecode || lease.reserved_bytes() != bytes
        {
            return Err(DatasetSourceFault::DecodeFailed { key });
        }
        Ok(lease)
    }

    fn acquire_physical_decode_scratch(
        &self,
        key: BrickKey,
        descriptor: ResourcePayloadDescriptor,
        mapping: LayerStorageMapping,
    ) -> Result<PhysicalDecodeScratch, DatasetSourceFault> {
        let total = physical_brick_working_bytes(
            key,
            descriptor.dtype(),
            descriptor.validity(),
            mapping.brick_shape[0] == 1,
        )?
        .checked_add(INNER_CODEC_WORKING_BYTES_MAX)
        .ok_or(DatasetSourceFault::DecodeFailed { key })?;
        let transient_bytes = physical_brick_retention_bytes_max(
            key,
            descriptor.dtype(),
            descriptor.validity(),
            mapping.brick_shape[0] == 1,
        )?;
        let workspace_bytes = total
            .checked_sub(transient_bytes)
            .filter(|bytes| *bytes != 0)
            .ok_or(DatasetSourceFault::DecodeFailed { key })?;
        let transient_retention = self
            .ledger
            .try_acquire(CpuLedgerCategory::InFlightDecode, transient_bytes)
            .map_err(|error| map_ledger_error(key, transient_bytes, error))?;
        if transient_retention.category() != CpuLedgerCategory::InFlightDecode
            || transient_retention.reserved_bytes() != transient_bytes
        {
            return Err(DatasetSourceFault::DecodeFailed { key });
        }
        let workspace = self
            .ledger
            .try_acquire(CpuLedgerCategory::InFlightDecode, workspace_bytes)
            .map_err(|error| map_ledger_error(key, workspace_bytes, error))?;
        if workspace.category() != CpuLedgerCategory::InFlightDecode
            || workspace.reserved_bytes() != workspace_bytes
        {
            return Err(DatasetSourceFault::DecodeFailed { key });
        }
        Ok(PhysicalDecodeScratch {
            transient_retention,
            workspace,
        })
    }

    fn acquire_aligned_cohort_scratch(
        &self,
        members: &[AlignedCohortMember],
    ) -> Result<Box<dyn CpuByteLease>, DatasetSourceFault> {
        let first = members
            .first()
            .ok_or(DatasetSourceFault::CatalogUnavailable)?;
        // Members decode sequentially inside one source transaction, so one
        // maximum-sized native codec/range workspace is the exact live bound;
        // retaining one workspace per reserved sink would multiply idle bytes.
        let bytes = members.iter().try_fold(0_u64, |maximum, member| {
            aligned_direct_working_bytes(
                member.key,
                member.descriptor.dtype(),
                member.descriptor.validity(),
                member.mapping.brick_shape[0] == 1,
            )
            .map(|bytes| maximum.max(bytes))
        })?;
        let bytes = bytes
            .checked_add(INNER_CODEC_WORKING_BYTES_MAX)
            .ok_or(DatasetSourceFault::DecodeFailed { key: first.key })?;
        let lease = self
            .ledger
            .try_acquire(CpuLedgerCategory::InFlightDecode, bytes)
            .map_err(|error| map_ledger_error(first.key, bytes, error))?;
        if lease.category() != CpuLedgerCategory::InFlightDecode || lease.reserved_bytes() != bytes
        {
            return Err(DatasetSourceFault::DecodeFailed { key: first.key });
        }
        Ok(lease)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn read_physical_brick(
        &self,
        sink: &dyn ReservedDecodeSink,
        key: BrickKey,
        coordinates: PackedIndexCoordinates,
        descriptor: ResourcePayloadDescriptor,
        mapping: LayerStorageMapping,
    ) -> Result<PhysicalBrickRead, DatasetSourceFault> {
        self.read_physical_brick_with_cohort(sink, key, coordinates, descriptor, mapping, None)
    }

    fn read_physical_brick_with_cohort(
        &self,
        sink: &dyn ReservedDecodeSink,
        key: BrickKey,
        coordinates: PackedIndexCoordinates,
        descriptor: ResourcePayloadDescriptor,
        mapping: LayerStorageMapping,
        mut cohort: Option<&mut PhysicalReadCohort<'_, '_, '_>>,
    ) -> Result<PhysicalBrickRead, DatasetSourceFault> {
        self.counters
            .physical_brick_requests
            .fetch_add(1, Ordering::Relaxed);
        let mut joined_loading: Option<PhysicalLoadingTicket> = None;
        loop {
            if sink.is_cancelled() {
                if let Some(ticket) = joined_loading.take() {
                    self.leave_physical_interest(coordinates, &ticket);
                }
                return Err(DatasetSourceFault::Cancelled { key });
            }
            let mut cache = lock_unpoisoned(&self.physical_cache.state);
            cache.next_touch = cache.next_touch.saturating_add(1);
            let touch = cache.next_touch;
            if let Some(index) = cache
                .entries
                .iter()
                .position(|entry| entry.coordinates == coordinates)
            {
                let generation = cache.entries[index].generation;
                cache.entries[index].last_touch = touch;
                match &mut cache.entries[index].value {
                    PhysicalBrickCacheValue::Ready { brick, .. } => {
                        let brick = brick.clone();
                        self.counters
                            .physical_brick_cache_hits
                            .fetch_add(1, Ordering::Relaxed);
                        drop(cache);
                        let validation = if let Some(cohort) = cohort.as_deref_mut() {
                            cohort.accepted.access.validate_cached_brick_in_cohort(
                                &brick,
                                cohort.transaction,
                                || sink.is_cancelled(),
                            )
                        } else {
                            self.revalidate_physical_delivery(&brick, || sink.is_cancelled())
                        };
                        match validation {
                            Ok(()) => {}
                            Err(PackageReadError::Cancelled) => {
                                // Caller cancellation is not evidence that a
                                // current, decoded cache entry became stale.
                                return Err(DatasetSourceFault::Cancelled { key });
                            }
                            Err(error) => {
                                self.remove_physical_ready(coordinates, &brick);
                                return Err(map_read_error(key, error));
                            }
                        }
                        Self::checkpoint(sink, key)?;
                        return Ok(PhysicalBrickRead {
                            brick,
                            _temporary_owner: None,
                        });
                    }
                    PhysicalBrickCacheValue::Loading {
                        interests,
                        cancellation,
                    } => {
                        let already_joined = joined_loading.as_ref().is_some_and(|ticket| {
                            ticket.generation == generation
                                && Arc::ptr_eq(&ticket.cancellation, cancellation)
                        });
                        if !already_joined {
                            *interests = interests
                                .checked_add(1)
                                .ok_or(DatasetSourceFault::DecodeFailed { key })?;
                            joined_loading = Some(PhysicalLoadingTicket {
                                generation,
                                cancellation: Arc::clone(cancellation),
                            });
                        }
                        self.counters
                            .physical_brick_cache_waits
                            .fetch_add(1, Ordering::Relaxed);
                        let (next, _) = self
                            .physical_cache
                            .wake
                            .wait_timeout(cache, PHYSICAL_CACHE_WAIT_POLL)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        drop(next);
                        continue;
                    }
                    PhysicalBrickCacheValue::TransientReady {
                        shared,
                        remaining_interests,
                    } => {
                        let joined_this_generation = joined_loading
                            .as_ref()
                            .is_some_and(|ticket| ticket.generation == generation);
                        let shared = Arc::clone(shared);
                        let remove = if joined_this_generation {
                            let Some(remaining) = remaining_interests.checked_sub(1) else {
                                return Err(DatasetSourceFault::DecodeFailed { key });
                            };
                            *remaining_interests = remaining;
                            remaining == 0
                        } else {
                            false
                        };
                        if remove {
                            cache.entries.remove(index);
                        }
                        self.counters
                            .physical_brick_cache_hits
                            .fetch_add(1, Ordering::Relaxed);
                        drop(cache);
                        if remove {
                            self.physical_cache.wake.notify_all();
                        }
                        let validation = if let Some(cohort) = cohort.as_deref_mut() {
                            cohort.accepted.access.validate_cached_brick_in_cohort(
                                &shared.brick,
                                cohort.transaction,
                                || sink.is_cancelled(),
                            )
                        } else {
                            self.revalidate_physical_delivery(&shared.brick, || sink.is_cancelled())
                        };
                        match validation {
                            Ok(()) => {}
                            Err(PackageReadError::Cancelled) => {
                                return Err(DatasetSourceFault::Cancelled { key });
                            }
                            Err(error) => return Err(map_read_error(key, error)),
                        }
                        Self::checkpoint(sink, key)?;
                        return Ok(PhysicalBrickRead {
                            brick: Arc::clone(&shared.brick),
                            _temporary_owner: Some(shared),
                        });
                    }
                }
            }

            // A completed or failed generation removes its entry. Any ticket
            // left from that generation no longer represents cache state.
            joined_loading = None;

            if cache.entries.len() == PHYSICAL_BRICK_CACHE_ENTRIES_MAX {
                let victim = cache
                    .entries
                    .iter()
                    .enumerate()
                    .filter(|(_, entry)| {
                        matches!(entry.value, PhysicalBrickCacheValue::Ready { .. })
                    })
                    .min_by_key(|(_, entry)| entry.last_touch)
                    .map(|(index, _)| index);
                let Some(victim) = victim else {
                    self.counters
                        .physical_brick_cache_waits
                        .fetch_add(1, Ordering::Relaxed);
                    let (next, _) = self
                        .physical_cache
                        .wake
                        .wait_timeout(cache, PHYSICAL_CACHE_WAIT_POLL)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    drop(next);
                    continue;
                };
                let removed = cache.entries.remove(victim);
                if let PhysicalBrickCacheValue::Ready { bytes, .. } = removed.value {
                    cache.retained_bytes = cache.retained_bytes.saturating_sub(bytes);
                }
                self.counters
                    .physical_brick_cache_evictions
                    .fetch_add(1, Ordering::Relaxed);
            }

            let generation = cache.next_generation;
            cache.next_generation = cache
                .next_generation
                .checked_add(1)
                .ok_or(DatasetSourceFault::DecodeFailed { key })?;
            let shared_cancellation = Arc::new(AtomicBool::new(false));
            let loading_ticket = PhysicalLoadingTicket {
                generation,
                cancellation: Arc::clone(&shared_cancellation),
            };
            cache.entries.push(PhysicalBrickCacheEntry {
                coordinates,
                generation,
                last_touch: touch,
                value: PhysicalBrickCacheValue::Loading {
                    interests: 1,
                    cancellation: Arc::clone(&shared_cancellation),
                },
            });
            joined_loading = Some(loading_ticket.clone());
            self.counters
                .physical_brick_cache_misses
                .fetch_add(1, Ordering::Relaxed);
            drop(cache);

            // Snapshot the immutable package access before any potentially
            // blocking allocation or filesystem work. The Arc owns the one
            // source identity through the complete delivery.
            let read_access = cohort
                .as_deref()
                .map_or_else(|| self.access_snapshot(), |cohort| cohort.accepted.clone());
            let physical_scratch =
                match self.acquire_physical_decode_scratch(key, descriptor, mapping) {
                    Ok(lease) => lease,
                    Err(error) => {
                        self.remove_physical_loading(coordinates, &loading_ticket);
                        return Err(error);
                    }
                };
            let loaded = if let Some(cohort) = cohort.as_deref_mut() {
                read_access
                    .access
                    .read_brick_in_cohort(coordinates, cohort.transaction, || {
                        if joined_loading.is_some() && sink.is_cancelled() {
                            self.leave_physical_interest(coordinates, &loading_ticket);
                            joined_loading = None;
                        }
                        shared_cancellation.load(Ordering::Acquire)
                    })
            } else {
                read_access.access.read_brick(coordinates, || {
                    if joined_loading.is_some() && sink.is_cancelled() {
                        self.leave_physical_interest(coordinates, &loading_ticket);
                        joined_loading = None;
                    }
                    shared_cancellation.load(Ordering::Acquire)
                })
            };
            let brick = match loaded {
                Ok(brick) if brick.payload_facts().is_some() => Arc::new(brick),
                Ok(_) => {
                    self.remove_physical_loading(coordinates, &loading_ticket);
                    return Err(DatasetSourceFault::CorruptResource { key });
                }
                Err(error) => {
                    self.remove_physical_loading(coordinates, &loading_ticket);
                    return Err(map_read_error(key, error));
                }
            };
            if cohort.is_none()
                && let Err(error) = self.finish_physical_delivery(read_access, &brick, || {
                    if joined_loading.is_some() && sink.is_cancelled() {
                        self.leave_physical_interest(coordinates, &loading_ticket);
                        joined_loading = None;
                    }
                    shared_cancellation.load(Ordering::Acquire)
                })
            {
                self.remove_physical_loading(coordinates, &loading_ticket);
                return Err(map_read_error(key, error));
            }
            let retained_bytes = match brick.retained_payload_bytes() {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.remove_physical_loading(coordinates, &loading_ticket);
                    return Err(map_read_error(key, error));
                }
            };
            // Codec and encoded-range workspace is no longer live. Releasing
            // it before residency admission avoids a false cache bypass caused
            // only by dead scratch still counting against the total CPU cap.
            let transient_retention = physical_scratch.into_transient_retention();
            self.counters
                .physical_brick_unique_decodes
                .fetch_add(1, Ordering::Relaxed);
            let decoded_payload_bytes = retained_bytes.saturating_sub(512);
            let _ = self
                .counters
                .physical_brick_unique_decoded_bytes
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |bytes| {
                    Some(bytes.saturating_add(decoded_payload_bytes))
                });
            let lease = match self
                .ledger
                .try_acquire(CpuLedgerCategory::DecodedResidency, retained_bytes)
            {
                Ok(lease)
                    if lease.category() == CpuLedgerCategory::DecodedResidency
                        && lease.reserved_bytes() == retained_bytes =>
                {
                    Some(lease)
                }
                Ok(_) | Err(CpuLedgerError::ZeroByteReservation) => {
                    self.remove_physical_loading(coordinates, &loading_ticket);
                    return Err(DatasetSourceFault::DecodeFailed { key });
                }
                Err(CpuLedgerError::CapacityExceeded { .. }) => {
                    self.counters
                        .physical_brick_cache_capacity_bypasses
                        .fetch_add(1, Ordering::Relaxed);
                    None
                }
                Err(CpuLedgerError::ShuttingDown) => {
                    self.remove_physical_loading(coordinates, &loading_ticket);
                    return Err(DatasetSourceFault::ShuttingDown {
                        key,
                        category: CpuLedgerCategory::DecodedResidency,
                        requested_bytes: retained_bytes,
                    });
                }
            };

            let Some(lease) = lease else {
                let shared = Arc::new(TransientPhysicalBrick {
                    brick,
                    _retention_lease: transient_retention,
                });
                if !self.publish_physical_transient(coordinates, &loading_ticket, shared) {
                    return if sink.is_cancelled() {
                        Err(DatasetSourceFault::Cancelled { key })
                    } else {
                        Err(DatasetSourceFault::DecodeFailed { key })
                    };
                }
                if let Err(error) = Self::checkpoint(sink, key) {
                    if joined_loading.take().is_some() {
                        self.leave_physical_interest(coordinates, &loading_ticket);
                    }
                    return Err(error);
                }
                let Some(shared) = self.take_physical_transient(coordinates, &loading_ticket)
                else {
                    return Err(DatasetSourceFault::DecodeFailed { key });
                };
                return Ok(PhysicalBrickRead {
                    brick: Arc::clone(&shared.brick),
                    _temporary_owner: Some(shared),
                });
            };
            drop(transient_retention);
            if !self.publish_physical_ready(
                coordinates,
                &loading_ticket,
                Arc::clone(&brick),
                lease,
                retained_bytes,
            ) {
                return if sink.is_cancelled() {
                    Err(DatasetSourceFault::Cancelled { key })
                } else {
                    Err(DatasetSourceFault::DecodeFailed { key })
                };
            }
            Self::checkpoint(sink, key)?;
            return Ok(PhysicalBrickRead {
                brick,
                _temporary_owner: None,
            });
        }
    }

    fn remove_physical_ready(
        &self,
        coordinates: PackedIndexCoordinates,
        expected: &Arc<LocalBrickRead>,
    ) {
        let mut cache = lock_unpoisoned(&self.physical_cache.state);
        let found = cache.entries.iter().position(|entry| {
            entry.coordinates == coordinates
                && match &entry.value {
                    PhysicalBrickCacheValue::Ready { brick, .. } => {
                        Arc::as_ptr(brick) == Arc::as_ptr(expected)
                    }
                    PhysicalBrickCacheValue::Loading { .. }
                    | PhysicalBrickCacheValue::TransientReady { .. } => false,
                }
        });
        if let Some(index) = found {
            let removed = cache.entries.remove(index);
            if let PhysicalBrickCacheValue::Ready { bytes, .. } = removed.value {
                cache.retained_bytes = cache.retained_bytes.saturating_sub(bytes);
            }
        }
        drop(cache);
        self.physical_cache.wake.notify_all();
    }

    fn remove_physical_loading(
        &self,
        coordinates: PackedIndexCoordinates,
        ticket: &PhysicalLoadingTicket,
    ) {
        let mut cache = lock_unpoisoned(&self.physical_cache.state);
        let found = cache.entries.iter().position(|entry| {
            entry.coordinates == coordinates
                && entry.generation == ticket.generation
                && matches!(
                    &entry.value,
                    PhysicalBrickCacheValue::Loading { cancellation, .. }
                        if Arc::ptr_eq(cancellation, &ticket.cancellation)
                )
        });
        if let Some(index) = found {
            ticket.cancellation.store(true, Ordering::Release);
            cache.entries.remove(index);
        }
        drop(cache);
        self.physical_cache.wake.notify_all();
    }

    fn leave_physical_interest(
        &self,
        coordinates: PackedIndexCoordinates,
        ticket: &PhysicalLoadingTicket,
    ) {
        let mut cache = lock_unpoisoned(&self.physical_cache.state);
        let Some(index) = cache.entries.iter().position(|entry| {
            entry.coordinates == coordinates && entry.generation == ticket.generation
        }) else {
            return;
        };
        let remove = match &mut cache.entries[index].value {
            PhysicalBrickCacheValue::Loading {
                interests,
                cancellation,
            } if Arc::ptr_eq(cancellation, &ticket.cancellation) => {
                let Some(remaining) = interests.checked_sub(1) else {
                    return;
                };
                *interests = remaining;
                if remaining == 0 {
                    cancellation.store(true, Ordering::Release);
                    true
                } else {
                    false
                }
            }
            PhysicalBrickCacheValue::TransientReady {
                remaining_interests,
                ..
            } => {
                let Some(remaining) = remaining_interests.checked_sub(1) else {
                    return;
                };
                *remaining_interests = remaining;
                remaining == 0
            }
            PhysicalBrickCacheValue::Loading { .. } | PhysicalBrickCacheValue::Ready { .. } => {
                return;
            }
        };
        if remove {
            cache.entries.remove(index);
        }
        drop(cache);
        if remove {
            self.physical_cache.wake.notify_all();
        }
    }

    fn publish_physical_transient(
        &self,
        coordinates: PackedIndexCoordinates,
        ticket: &PhysicalLoadingTicket,
        shared: Arc<TransientPhysicalBrick>,
    ) -> bool {
        let mut cache = lock_unpoisoned(&self.physical_cache.state);
        let Some(entry) = cache.entries.iter_mut().find(|entry| {
            entry.coordinates == coordinates && entry.generation == ticket.generation
        }) else {
            return false;
        };
        let PhysicalBrickCacheValue::Loading {
            interests,
            cancellation,
        } = &entry.value
        else {
            return false;
        };
        if *interests == 0 || !Arc::ptr_eq(cancellation, &ticket.cancellation) {
            return false;
        }
        let remaining_interests = *interests;
        entry.value = PhysicalBrickCacheValue::TransientReady {
            shared,
            remaining_interests,
        };
        drop(cache);
        self.physical_cache.wake.notify_all();
        true
    }

    fn take_physical_transient(
        &self,
        coordinates: PackedIndexCoordinates,
        ticket: &PhysicalLoadingTicket,
    ) -> Option<Arc<TransientPhysicalBrick>> {
        let mut cache = lock_unpoisoned(&self.physical_cache.state);
        let index = cache.entries.iter().position(|entry| {
            entry.coordinates == coordinates && entry.generation == ticket.generation
        })?;
        let PhysicalBrickCacheValue::TransientReady {
            shared,
            remaining_interests,
        } = &mut cache.entries[index].value
        else {
            return None;
        };
        let shared = Arc::clone(shared);
        *remaining_interests = remaining_interests.checked_sub(1)?;
        let remove = *remaining_interests == 0;
        if remove {
            cache.entries.remove(index);
        }
        drop(cache);
        if remove {
            self.physical_cache.wake.notify_all();
        }
        Some(shared)
    }

    fn publish_physical_ready(
        &self,
        coordinates: PackedIndexCoordinates,
        ticket: &PhysicalLoadingTicket,
        brick: Arc<LocalBrickRead>,
        lease: Box<dyn CpuByteLease>,
        bytes: u64,
    ) -> bool {
        let mut cache = lock_unpoisoned(&self.physical_cache.state);
        let Some(entry) = cache.entries.iter_mut().find(|entry| {
            entry.coordinates == coordinates && entry.generation == ticket.generation
        }) else {
            return false;
        };
        if !matches!(
            &entry.value,
            PhysicalBrickCacheValue::Loading { cancellation, .. }
                if Arc::ptr_eq(cancellation, &ticket.cancellation)
        ) {
            return false;
        }
        entry.value = PhysicalBrickCacheValue::Ready {
            brick,
            _lease: lease,
            bytes,
        };
        cache.retained_bytes = cache.retained_bytes.saturating_add(bytes);
        cache.peak_retained_bytes = cache.peak_retained_bytes.max(cache.retained_bytes);
        drop(cache);
        self.physical_cache.wake.notify_all();
        true
    }

    fn aligned_physical_coordinates(
        &self,
        key: BrickKey,
        mapping: LayerStorageMapping,
    ) -> Result<Option<PackedIndexCoordinates>, DatasetSourceFault> {
        let origin = key.region().origin();
        if mapping.brick_shape != [64, 64, 64]
            || key.region().shape().dimensions() != mapping.brick_shape
            || origin
                .into_iter()
                .zip(mapping.brick_shape)
                .any(|(origin, brick)| origin % brick != 0)
        {
            return Ok(None);
        }
        let timepoint = u32::try_from(key.timepoint().get())
            .map_err(|_| DatasetSourceFault::CorruptResource { key })?;
        let brick_coordinates = [
            origin[0] / mapping.brick_shape[0],
            origin[1] / mapping.brick_shape[1],
            origin[2] / mapping.brick_shape[2],
        ];
        Ok(Some(PackedIndexCoordinates::new(
            mapping.image,
            key.scale().get(),
            timepoint,
            mapping.physical_channel,
            coordinate_u32(key, brick_coordinates[0])?,
            coordinate_u32(key, brick_coordinates[1])?,
            coordinate_u32(key, brick_coordinates[2])?,
        )))
    }

    fn decode_unaligned_cohort(
        &self,
        sinks: &mut [&mut dyn ReservedDecodeSink],
        members: &[UnalignedCohortMember],
        outcomes: &mut [Option<Result<(), DatasetSourceFault>>],
    ) {
        if members.is_empty() {
            return;
        }
        let total_work = members.iter().try_fold(0_usize, |total, member| {
            unaligned_member_work_count(*member).and_then(|count| {
                total
                    .checked_add(count)
                    .ok_or(DatasetSourceFault::DecodeFailed { key: member.key })
            })
        });
        let total_work = match total_work {
            Ok(total) => total,
            Err(error) => {
                let kind = SharedSourceFault::from_dataset_fault(&error);
                for member in members {
                    outcomes[member.sink_index] = Some(Err(kind.for_key(member.key)));
                }
                return;
            }
        };
        let read_access = self.access_snapshot();
        let descriptor_count = read_access.access.storage_catalog().descriptors().len();
        let transaction_object_capacity = read_access.access.runtime_read_object_capacity();
        let snapshot_capacity = total_work
            .saturating_mul(PHYSICAL_BRICK_OBJECT_SNAPSHOTS_MAX)
            .min(descriptor_count);
        let control_bytes = match unaligned_control_bytes(
            members[0].key,
            members.len(),
            total_work,
            snapshot_capacity,
            transaction_object_capacity,
        ) {
            Ok(bytes) => bytes,
            Err(error) => {
                let kind = SharedSourceFault::from_dataset_fault(&error);
                for member in members {
                    outcomes[member.sink_index] = Some(Err(kind.for_key(member.key)));
                }
                return;
            }
        };
        let _control_lease = match self.acquire_unaligned_control(members[0].key, control_bytes) {
            Ok(lease) => lease,
            Err(error) => {
                let kind = SharedSourceFault::from_dataset_fault(&error);
                for member in members {
                    outcomes[member.sink_index] = Some(Err(kind.for_key(member.key)));
                }
                return;
            }
        };
        let mut states = Vec::with_capacity(members.len());
        let mut work = Vec::with_capacity(total_work);

        for &member in members {
            let sink = &*sinks[member.sink_index];
            if sink.is_cancelled() {
                outcomes[member.sink_index] =
                    Some(Err(DatasetSourceFault::Cancelled { key: member.key }));
                continue;
            }
            let staging_lease = match self.acquire_semantic_staging(member.key, member.descriptor) {
                Ok(lease) => lease,
                Err(error) => {
                    outcomes[member.sink_index] = Some(Err(error));
                    continue;
                }
            };
            let staging_len = match usize::try_from(member.descriptor.byte_len()) {
                Ok(bytes) => bytes,
                Err(_) => {
                    outcomes[member.sink_index] =
                        Some(Err(DatasetSourceFault::DecodeFailed { key: member.key }));
                    continue;
                }
            };
            let state_index = states.len();
            states.push(UnalignedMemberState {
                member,
                staging: vec![0_u8; staging_len],
                facts: PayloadFactsAccumulator::default(),
                _staging_lease: staging_lease,
            });
            let previous_work = work.len();
            if let Err(error) = append_unaligned_member_work(member, state_index, &mut work) {
                work.truncate(previous_work);
                states.pop();
                outcomes[member.sink_index] = Some(Err(error));
            }
        }

        // Every cohort advances in the same physical order. All semantic
        // siblings interested in one physical brick consume that one decoded
        // result before the bounded eight-entry cache can evict it. Concurrent
        // cohorts use the same order and therefore rendezvous on Loading.
        work.sort_unstable_by_key(|item| packed_coordinates_sort_key(item.coordinates));

        if states.is_empty() {
            return;
        }
        let mut transaction = match read_access.access.begin_runtime_read_cohort(|| {
            states
                .iter()
                .all(|state| sinks[state.member.sink_index].is_cancelled())
        }) {
            Ok(transaction) => transaction,
            Err(error) => {
                let kind = SharedSourceFault::from_read_error(&error);
                for state in states {
                    let member = state.member;
                    outcomes[member.sink_index] = Some(Err(kind.for_key(member.key)));
                }
                return;
            }
        };
        let mut snapshots = Vec::with_capacity(snapshot_capacity);
        let mut used_bricks = Vec::with_capacity(work.len());
        let mut active_sink_indices = Vec::with_capacity(members.len());

        let mut group_start = 0;
        while group_start < work.len() {
            let coordinates = work[group_start].coordinates;
            let mut group_end = group_start + 1;
            while group_end < work.len() && work[group_end].coordinates == coordinates {
                group_end += 1;
            }

            let group = &work[group_start..group_end];
            active_sink_indices.clear();
            let mut representative = None;
            for item in group {
                let member = states[item.state_index].member;
                if outcomes[member.sink_index].is_none() {
                    representative.get_or_insert(member);
                    active_sink_indices.push(member.sink_index);
                }
            }
            let Some(representative) = representative else {
                group_start = group_end;
                continue;
            };
            let brick = {
                let cancellation = PhysicalFanoutCancellationSink {
                    sinks: &*sinks,
                    sink_indices: &active_sink_indices,
                    key: representative.key,
                    descriptor: representative.descriptor,
                };
                let mut read_cohort = PhysicalReadCohort {
                    accepted: &read_access,
                    transaction: &mut transaction,
                };
                self.read_physical_brick_with_cohort(
                    &cancellation,
                    representative.key,
                    coordinates,
                    representative.descriptor,
                    representative.mapping,
                    Some(&mut read_cohort),
                )
            };
            let brick = match brick {
                Ok(brick) => brick,
                Err(error) => {
                    let kind = SharedSourceFault::from_dataset_fault(&error);
                    for item in group {
                        let member = states[item.state_index].member;
                        if outcomes[member.sink_index].is_some() {
                            continue;
                        }
                        outcomes[member.sink_index] =
                            Some(if sinks[member.sink_index].is_cancelled() {
                                Err(DatasetSourceFault::Cancelled { key: member.key })
                            } else {
                                Err(kind.for_key(member.key))
                            });
                    }
                    group_start = group_end;
                    continue;
                }
            };
            for snapshot in brick.object_snapshots() {
                if !snapshots
                    .iter()
                    .any(|existing: &crate::range_io::LocalObjectSnapshot| {
                        existing.path() == snapshot.path()
                    })
                {
                    snapshots.push(snapshot.clone());
                }
            }
            used_bricks.push((coordinates, Arc::downgrade(&brick.brick)));

            for item in group {
                let state = &mut states[item.state_index];
                let member = state.member;
                if outcomes[member.sink_index].is_some() {
                    continue;
                }
                let sink = &*sinks[member.sink_index];
                let copied = Self::checkpoint(sink, member.key).and_then(|()| {
                    copy_brick_intersection(
                        sink,
                        member.key,
                        member.descriptor,
                        member.mapping.brick_shape,
                        item.brick_coordinates,
                        &brick,
                        &mut state.staging,
                        &mut state.facts,
                        &self.counters,
                    )
                });
                if let Err(error) = copied {
                    outcomes[member.sink_index] = Some(Err(error));
                }
            }
            group_start = group_end;
        }

        let shared_delivery = transaction
            .finish_read()
            .map_err(PackageReadError::from)
            .and_then(|()| {
                self.finish_snapshot_delivery(read_access, &snapshots, || {
                    states
                        .iter()
                        .filter(|state| outcomes[state.member.sink_index].is_none())
                        .all(|state| sinks[state.member.sink_index].is_cancelled())
                })
            });
        if let Err(error) = shared_delivery {
            if !matches!(error, PackageReadError::Cancelled) {
                for (coordinates, brick) in used_bricks {
                    if let Some(brick) = brick.upgrade() {
                        self.remove_physical_ready(coordinates, &brick);
                    }
                }
            }
            let kind = SharedSourceFault::from_read_error(&error);
            for state in states {
                let member = state.member;
                if outcomes[member.sink_index].is_none() {
                    outcomes[member.sink_index] = Some(Err(kind.for_key(member.key)));
                }
            }
            return;
        }

        for state in states {
            let member = state.member;
            if outcomes[member.sink_index].is_some() {
                continue;
            }
            let facts = match state.facts.finish(member.key, member.descriptor) {
                Ok(facts) => facts,
                Err(error) => {
                    outcomes[member.sink_index] = Some(Err(error));
                    continue;
                }
            };
            let sink = &mut *sinks[member.sink_index];
            let result = Self::checkpoint(sink, member.key)
                .and_then(|()| write_sink_bytes(sink, member.key, &state.staging, &self.counters))
                .and_then(|()| Self::checkpoint(sink, member.key))
                .and_then(|()| {
                    sink.finish_with_facts(facts)
                        .map_err(|reason| map_sink_error(member.key, reason))
                });
            outcomes[member.sink_index] = Some(result);
        }
    }

    fn decode_aligned_cohort(
        &self,
        sinks: &mut [&mut dyn ReservedDecodeSink],
        members: &[AlignedCohortMember],
        outcomes: &mut [Option<Result<(), DatasetSourceFault>>],
    ) {
        let active = members
            .iter()
            .copied()
            .filter(|member| !sinks[member.sink_index].is_cancelled())
            .collect::<Vec<_>>();
        for member in members {
            if sinks[member.sink_index].is_cancelled() {
                outcomes[member.sink_index] =
                    Some(Err(DatasetSourceFault::Cancelled { key: member.key }));
            }
        }
        if active.is_empty() {
            return;
        }

        let _scratch = match self.acquire_aligned_cohort_scratch(&active) {
            Ok(scratch) => scratch,
            Err(error) => {
                let kind = SharedSourceFault::from_dataset_fault(&error);
                for member in active {
                    outcomes[member.sink_index] = Some(Err(kind.for_key(member.key)));
                }
                return;
            }
        };
        let read_access = self.access_snapshot();
        let mut transaction = match read_access.access.begin_runtime_read_cohort(|| {
            active
                .iter()
                .all(|member| sinks[member.sink_index].is_cancelled())
        }) {
            Ok(transaction) => transaction,
            Err(error) => {
                let kind = SharedSourceFault::from_read_error(&error);
                for member in active {
                    outcomes[member.sink_index] = Some(Err(kind.for_key(member.key)));
                }
                return;
            }
        };

        let mut pending = Vec::with_capacity(active.len());
        for member in active {
            let sink = &mut *sinks[member.sink_index];
            if sink.is_cancelled() {
                outcomes[member.sink_index] =
                    Some(Err(DatasetSourceFault::Cancelled { key: member.key }));
                continue;
            }
            self.counters
                .physical_brick_requests
                .fetch_add(1, Ordering::Relaxed);
            let written_before = sink.written_bytes();
            let direct = read_access.access.read_brick_into_sink_in_cohort(
                member.coordinates,
                sink,
                &mut transaction,
            );
            let written = sink.written_bytes().saturating_sub(written_before);
            saturating_counter_add(&self.counters.sink_write_bytes, written);
            match direct {
                Ok(direct) => {
                    self.counters
                        .physical_brick_unique_decodes
                        .fetch_add(1, Ordering::Relaxed);
                    saturating_counter_add(
                        &self.counters.physical_brick_unique_decoded_bytes,
                        member.descriptor.byte_len(),
                    );
                    saturating_counter_add(
                        &self.counters.aligned_direct_sink_span_bytes,
                        direct.direct_span_bytes(),
                    );
                    saturating_counter_add(
                        &self.counters.aligned_direct_post_decode_copy_bytes,
                        direct.post_decode_copy_bytes(),
                    );
                    pending.push((member, direct));
                }
                Err(error) => {
                    outcomes[member.sink_index] =
                        Some(Err(map_direct_read_error(member.key, error)));
                }
            }
        }

        let post = transaction.finish_read().map_err(PackageReadError::from);
        if let Err(error) = post {
            let kind = SharedSourceFault::from_read_error(&error);
            for (member, _) in pending {
                outcomes[member.sink_index] = Some(Err(kind.for_key(member.key)));
            }
            return;
        }

        let mut snapshots = Vec::new();
        for (_, direct) in &pending {
            for snapshot in direct.object_snapshots() {
                if !snapshots
                    .iter()
                    .any(|existing: &crate::range_io::LocalObjectSnapshot| {
                        existing.path() == snapshot.path()
                    })
                {
                    snapshots.push(snapshot.clone());
                }
            }
        }
        let shared_delivery = self.finish_snapshot_delivery(read_access, &snapshots, || {
            pending
                .iter()
                .all(|(member, _)| sinks[member.sink_index].is_cancelled())
        });
        if let Err(error) = shared_delivery {
            let kind = SharedSourceFault::from_read_error(&error);
            for (member, _) in pending {
                outcomes[member.sink_index] = Some(Err(kind.for_key(member.key)));
            }
            return;
        }

        for (member, direct) in pending {
            let sink = &mut *sinks[member.sink_index];
            let result = Self::checkpoint(sink, member.key).and_then(|()| {
                let facts = resource_payload_facts(member.key, direct.payload_facts())?;
                sink.finish_with_facts(facts)
                    .map_err(|reason| map_sink_error(member.key, reason))
            });
            if result.is_ok() {
                self.counters
                    .aligned_direct_deliveries
                    .fetch_add(1, Ordering::Relaxed);
                saturating_counter_add(
                    &self.counters.aligned_direct_streamed_bytes,
                    member.descriptor.byte_len(),
                );
            }
            outcomes[member.sink_index] = Some(result);
        }
    }
}

impl DatasetSource for LocalDatasetSource {
    fn catalog(&self) -> Result<Arc<DatasetCatalog>, DatasetSourceFault> {
        Ok(Arc::clone(&self.catalog))
    }

    fn minimum_decode_working_bytes(
        &self,
        key: BrickKey,
        descriptor: ResourcePayloadDescriptor,
    ) -> Result<u64, DatasetSourceFault> {
        let expected = self
            .catalog
            .resource_payload_descriptor(key)
            .map_err(|reason| invalid_resource(key, reason))?;
        if expected != descriptor {
            return Err(DatasetSourceFault::DecodeFailed { key });
        }
        let mapping = self.mapping(key)?;
        if self.aligned_physical_coordinates(key, mapping)?.is_some() {
            return aligned_direct_working_bytes(
                key,
                descriptor.dtype(),
                descriptor.validity(),
                mapping.brick_shape[0] == 1,
            )?
            .checked_add(INNER_CODEC_WORKING_BYTES_MAX)
            .ok_or(DatasetSourceFault::DecodeFailed { key });
        }

        let member = UnalignedCohortMember {
            sink_index: 0,
            key,
            descriptor,
            mapping,
        };
        let work = unaligned_member_work_count(member)?;
        let access = self.access_snapshot();
        let descriptor_count = access.access.storage_catalog().descriptors().len();
        // The catalog already contains the immutable root/page closure used by
        // per-read currentness checks. This bound is identical for ordinary
        // admitted and importer-published sources.
        let transaction_object_capacity = access.access.runtime_read_object_capacity();
        let snapshots = work
            .saturating_mul(PHYSICAL_BRICK_OBJECT_SNAPSHOTS_MAX)
            .min(descriptor_count);
        let control =
            unaligned_control_bytes(key, 1, work, snapshots, transaction_object_capacity)?;
        descriptor
            .byte_len()
            .checked_add(physical_brick_working_bytes(
                key,
                descriptor.dtype(),
                descriptor.validity(),
                mapping.brick_shape[0] == 1,
            )?)
            .and_then(|bytes| bytes.checked_add(INNER_CODEC_WORKING_BYTES_MAX))
            .and_then(|bytes| bytes.checked_add(control))
            .ok_or(DatasetSourceFault::DecodeFailed { key })
    }

    fn decode_cohort_into(
        &self,
        sinks: &mut [&mut dyn ReservedDecodeSink],
    ) -> Vec<Result<(), DatasetSourceFault>> {
        let mut outcomes = vec![None; sinks.len()];
        let mut aligned = Vec::new();
        let mut unaligned = Vec::new();
        for (sink_index, sink) in sinks.iter_mut().enumerate() {
            let sink = &mut **sink;
            let key = sink.resource_key();
            let prepared = (|| {
                Self::checkpoint(sink, key)?;
                let descriptor = self
                    .catalog
                    .validate_decode_reservation(sink)
                    .map_err(|reason| invalid_resource(key, reason))?;
                let mapping = self.mapping(key)?;
                Ok::<_, DatasetSourceFault>((descriptor, mapping))
            })();
            let (descriptor, mapping) = match prepared {
                Ok(prepared) => prepared,
                Err(error) => {
                    outcomes[sink_index] = Some(Err(error));
                    continue;
                }
            };
            match self.aligned_physical_coordinates(key, mapping) {
                Ok(Some(coordinates)) => aligned.push(AlignedCohortMember {
                    sink_index,
                    key,
                    descriptor,
                    mapping,
                    coordinates,
                }),
                Ok(None) => unaligned.push(UnalignedCohortMember {
                    sink_index,
                    key,
                    descriptor,
                    mapping,
                }),
                Err(error) => outcomes[sink_index] = Some(Err(error)),
            }
        }
        self.decode_unaligned_cohort(sinks, &unaligned, &mut outcomes);
        self.decode_aligned_cohort(sinks, &aligned, &mut outcomes);
        outcomes
            .into_iter()
            .enumerate()
            .map(|(index, outcome)| {
                outcome.unwrap_or_else(|| {
                    Err(DatasetSourceFault::DecodeFailed {
                        key: sinks[index].resource_key(),
                    })
                })
            })
            .collect()
    }
}

fn build_dataset_catalog(
    storage: &LocalPackageCatalog,
    content_address_status: ContentAddressStatus,
    resource_identity: DatasetResourceIdentity,
    display_label: &str,
) -> Result<(DatasetCatalog, Vec<LayerStorageMapping>), LocalDatasetSourceOpenError> {
    let mut layers = Vec::with_capacity(storage.science().layers().len());
    let mut mappings = Vec::with_capacity(storage.science().layers().len());

    for science in storage.science().layers() {
        let logical_layer = science.logical_layer();
        let (image, physical_channel) = storage
            .profile()
            .images()
            .iter()
            .find_map(|image| {
                image
                    .logical_layers()
                    .iter()
                    .find(|mapping| mapping.logical_layer() == logical_layer)
                    .map(|mapping| (image, mapping.physical_channel()))
            })
            .ok_or(LocalDatasetSourceOpenError::MetadataInvariant {
                reason: "a scientific layer has no physical image/channel mapping",
            })?;
        let ome_path = metadata_path(image.image_group_path())?;
        let ome =
            storage
                .ome_image(&ome_path)
                .ok_or(LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "a physical image has no opened OME metadata",
                })?;
        let mut scales = Vec::with_capacity(image.levels().len());
        let mut brick_shape = None;
        for (ordinal, level) in image.levels().iter().enumerate() {
            let array_path = metadata_path(level.pixel_path())?;
            let array = storage.zarr_array(&array_path).ok_or(
                LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "a profile pixel level has no opened Zarr metadata",
                },
            )?;
            let shape = array.shape();
            if shape.len() != 5 {
                return Err(LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "a profile pixel array is not t,c,z,y,x",
                });
            }
            let shape = Shape3D::new(shape[2], shape[3], shape[4]).map_err(|_| {
                LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "a profile pixel level has an invalid spatial shape",
                }
            })?;
            let current_brick = pixel_brick_shape(array.kind()).ok_or(
                LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "a profile pixel level uses a non-pixel storage kind",
                },
            )?;
            if brick_shape
                .replace(current_brick)
                .is_some_and(|prior| prior != current_brick)
            {
                return Err(LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "one logical layer mixes 2D and 3D physical bricks",
                });
            }
            let transform = ome.level_transforms().get(ordinal).copied().ok_or(
                LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "OME transform count differs from the profile level count",
                },
            )?;
            scales.push(DatasetScale::new(
                ScaleLevel::new(level.scale_ordinal()),
                shape,
                dataset_transform(
                    science.grid_to_world_micrometer_f64_bits(),
                    transform,
                    ordinal,
                )?,
                match level.validity_mode() {
                    ProfileValidityMode::AllValid => ResourceValidity::AllValid,
                    ProfileValidityMode::Explicit => ResourceValidity::BitMask,
                },
            ));
        }
        let layer_label = storage
            .display_defaults()
            .layers()
            .iter()
            .find(|display| display.logical_layer() == logical_layer)
            .and_then(crate::DisplayLayerDefaults::label)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("Layer {}", logical_layer.ordinal() + 1));
        layers.push(DatasetLayer::new_multiscale(
            logical_layer,
            layer_label,
            science.base_shape().t(),
            science.dtype(),
            scales,
        )?);
        mappings.push(LayerStorageMapping {
            image: image.image_ordinal(),
            physical_channel,
            brick_shape: brick_shape.ok_or(LocalDatasetSourceOpenError::MetadataInvariant {
                reason: "a logical layer has no physical scale",
            })?,
        });
    }
    Ok((
        DatasetCatalog::new_with_resource_identity(
            display_label,
            content_address_status,
            resource_identity,
            layers,
        )?,
        mappings,
    ))
}

fn metadata_path(base: &PackagePath) -> Result<PackagePath, LocalDatasetSourceOpenError> {
    PackagePath::parse(&format!("{base}/zarr.json")).map_err(|_| {
        LocalDatasetSourceOpenError::MetadataInvariant {
            reason: "a profile metadata path violates the package path grammar",
        }
    })
}

fn dataset_transform(
    base: &[crate::F64Bits; 16],
    ome: OmeLevelTransform,
    level: usize,
) -> Result<GridToWorld, LocalDatasetSourceOpenError> {
    let row_major = match ome {
        OmeLevelTransform::DiagonalMicrometer {
            scale_zyx: [z, y, x],
            translation_zyx: [tz, ty, tx],
        } => [
            x.value(),
            0.0,
            0.0,
            tx.value(),
            0.0,
            y.value(),
            0.0,
            ty.value(),
            0.0,
            0.0,
            z.value(),
            tz.value(),
            0.0,
            0.0,
            0.0,
            1.0,
        ],
        OmeLevelTransform::UnitlessIdentity => {
            let exponent = u32::try_from(level).map_err(|_| {
                LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "a scale level cannot be represented",
                }
            })?;
            let factor = 2_u64.checked_pow(exponent).ok_or(
                LocalDatasetSourceOpenError::MetadataInvariant {
                    reason: "a scale transform factor overflowed",
                },
            )? as f64;
            let mut row_major = base.map(crate::F64Bits::value);
            for row in 0..4 {
                for column in 0..3 {
                    row_major[row * 4 + column] *= factor;
                }
            }
            row_major
        }
    };
    GridToWorld::from_row_major(row_major).map_err(|_| {
        LocalDatasetSourceOpenError::MetadataInvariant {
            reason: "a scale transform is not finite affine metadata",
        }
    })
}

const fn pixel_brick_shape(kind: ShardProfileKind) -> Option<[u64; 3]> {
    match kind {
        ShardProfileKind::Pixel3dUint8
        | ShardProfileKind::Pixel3dUint16
        | ShardProfileKind::Pixel3dFloat32 => Some([64, 64, 64]),
        ShardProfileKind::Pixel2dUint8
        | ShardProfileKind::Pixel2dUint16
        | ShardProfileKind::Pixel2dFloat32 => Some([1, 256, 256]),
        ShardProfileKind::Validity3d
        | ShardProfileKind::Validity2d
        | ShardProfileKind::PackedIndex => None,
    }
}

fn physical_brick_working_bytes(
    key: BrickKey,
    dtype: IntensityDType,
    validity: ResourceValidity,
    two_dimensional: bool,
) -> Result<u64, DatasetSourceFault> {
    let pixel = match (dtype, two_dimensional) {
        (IntensityDType::Uint8, false) => ShardProfileKind::Pixel3dUint8,
        (IntensityDType::Uint16, false) => ShardProfileKind::Pixel3dUint16,
        (IntensityDType::Float32, false) => ShardProfileKind::Pixel3dFloat32,
        (IntensityDType::Uint8, true) => ShardProfileKind::Pixel2dUint8,
        (IntensityDType::Uint16, true) => ShardProfileKind::Pixel2dUint16,
        (IntensityDType::Float32, true) => ShardProfileKind::Pixel2dFloat32,
    };
    let mut total = component_working_bytes(key, ShardProfileKind::PackedIndex)?
        .checked_add(component_working_bytes(key, pixel)?)
        .ok_or(DatasetSourceFault::DecodeFailed { key })?;
    if validity == ResourceValidity::BitMask {
        let kind = if two_dimensional {
            ShardProfileKind::Validity2d
        } else {
            ShardProfileKind::Validity3d
        };
        total = total
            .checked_add(component_working_bytes(key, kind)?)
            .ok_or(DatasetSourceFault::DecodeFailed { key })?;
    }
    Ok(total)
}

fn physical_brick_retention_bytes_max(
    key: BrickKey,
    dtype: IntensityDType,
    validity: ResourceValidity,
    two_dimensional: bool,
) -> Result<u64, DatasetSourceFault> {
    let pixel = match (dtype, two_dimensional) {
        (IntensityDType::Uint8, false) => ShardProfileKind::Pixel3dUint8,
        (IntensityDType::Uint16, false) => ShardProfileKind::Pixel3dUint16,
        (IntensityDType::Float32, false) => ShardProfileKind::Pixel3dFloat32,
        (IntensityDType::Uint8, true) => ShardProfileKind::Pixel2dUint8,
        (IntensityDType::Uint16, true) => ShardProfileKind::Pixel2dUint16,
        (IntensityDType::Float32, true) => ShardProfileKind::Pixel2dFloat32,
    };
    let pixel = u64::try_from(pixel.decoded_inner_bytes())
        .map_err(|_| DatasetSourceFault::DecodeFailed { key })?;
    let validity = if validity == ResourceValidity::BitMask {
        let kind = if two_dimensional {
            ShardProfileKind::Validity2d
        } else {
            ShardProfileKind::Validity3d
        };
        u64::try_from(kind.decoded_inner_bytes())
            .map_err(|_| DatasetSourceFault::DecodeFailed { key })?
    } else {
        0
    };
    pixel
        .checked_add(validity)
        // Match LocalBrickRead::retained_payload_bytes accounting for the
        // record, snapshots, vector headers, and Arc/cache ownership.
        .and_then(|bytes| bytes.checked_add(512))
        .ok_or(DatasetSourceFault::DecodeFailed { key })
}

fn aligned_direct_working_bytes(
    key: BrickKey,
    dtype: IntensityDType,
    validity: ResourceValidity,
    two_dimensional: bool,
) -> Result<u64, DatasetSourceFault> {
    let pixel = match (dtype, two_dimensional) {
        (IntensityDType::Uint8, false) => ShardProfileKind::Pixel3dUint8,
        (IntensityDType::Uint16, false) => ShardProfileKind::Pixel3dUint16,
        (IntensityDType::Float32, false) => ShardProfileKind::Pixel3dFloat32,
        (IntensityDType::Uint8, true) => ShardProfileKind::Pixel2dUint8,
        (IntensityDType::Uint16, true) => ShardProfileKind::Pixel2dUint16,
        (IntensityDType::Float32, true) => ShardProfileKind::Pixel2dFloat32,
    };
    physical_brick_working_bytes(key, dtype, validity, two_dimensional)?
        .checked_sub(
            u64::try_from(pixel.decoded_inner_bytes())
                .map_err(|_| DatasetSourceFault::DecodeFailed { key })?,
        )
        .ok_or(DatasetSourceFault::DecodeFailed { key })
}

fn component_working_bytes(
    key: BrickKey,
    kind: ShardProfileKind,
) -> Result<u64, DatasetSourceFault> {
    u64::try_from(kind.decoded_inner_bytes())
        .ok()
        .and_then(|bytes| bytes.checked_add(u64::try_from(kind.encoded_inner_bytes_max()).ok()?))
        .and_then(|bytes| bytes.checked_add(u64::try_from(kind.index_tail_bytes()).ok()?))
        .ok_or(DatasetSourceFault::DecodeFailed { key })
}

#[derive(Default)]
struct PayloadFactsAccumulator {
    minimum: f32,
    maximum: f32,
    valid_samples: u64,
    initialized: bool,
}

impl PayloadFactsAccumulator {
    fn include_value(&mut self, key: BrickKey, value: f32) -> Result<(), DatasetSourceFault> {
        if !value.is_finite() {
            return Err(DatasetSourceFault::CorruptResource { key });
        }
        if self.initialized {
            self.minimum = self.minimum.min(value);
            self.maximum = self.maximum.max(value);
        } else {
            self.minimum = value;
            self.maximum = value;
            self.initialized = true;
        }
        self.valid_samples = self
            .valid_samples
            .checked_add(1)
            .ok_or(DatasetSourceFault::DecodeFailed { key })?;
        Ok(())
    }

    fn include_bytes(
        &mut self,
        key: BrickKey,
        dtype: IntensityDType,
        bytes: &[u8],
    ) -> Result<(), DatasetSourceFault> {
        match dtype {
            IntensityDType::Uint8 => {
                for value in bytes {
                    self.include_value(key, f32::from(*value))?;
                }
            }
            IntensityDType::Uint16 => {
                for bytes in bytes.chunks_exact(2) {
                    self.include_value(key, f32::from(u16::from_le_bytes([bytes[0], bytes[1]])))?;
                }
            }
            IntensityDType::Float32 => {
                for bytes in bytes.chunks_exact(4) {
                    self.include_value(
                        key,
                        f32::from_le_bytes(bytes.try_into().expect("a float sample is four bytes")),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn include_zeros(&mut self, key: BrickKey, count: usize) -> Result<(), DatasetSourceFault> {
        if count == 0 {
            return Ok(());
        }
        if self.initialized {
            self.minimum = self.minimum.min(0.0);
            self.maximum = self.maximum.max(0.0);
        } else {
            self.minimum = 0.0;
            self.maximum = 0.0;
            self.initialized = true;
        }
        self.valid_samples = self
            .valid_samples
            .checked_add(
                u64::try_from(count).map_err(|_| DatasetSourceFault::DecodeFailed { key })?,
            )
            .ok_or(DatasetSourceFault::DecodeFailed { key })?;
        Ok(())
    }

    fn finish(
        self,
        key: BrickKey,
        descriptor: ResourcePayloadDescriptor,
    ) -> Result<ResourcePayloadFacts, DatasetSourceFault> {
        let any_valid = self.valid_samples != 0;
        ResourcePayloadFacts::from_validated_range(
            self.minimum,
            self.maximum,
            any_valid,
            self.valid_samples == descriptor.sample_count(),
        )
        .map_err(|_| DatasetSourceFault::CorruptResource { key })
    }
}

fn unaligned_member_work_count(member: UnalignedCohortMember) -> Result<usize, DatasetSourceFault> {
    let key = member.key;
    let region_start = key.region().origin();
    let region_end = key.region().end_exclusive();
    let first: [u64; 3] =
        std::array::from_fn(|axis| region_start[axis] / member.mapping.brick_shape[axis]);
    let last: [u64; 3] =
        std::array::from_fn(|axis| (region_end[axis] - 1) / member.mapping.brick_shape[axis]);
    coordinate_u32(key, last[0])?;
    coordinate_u32(key, last[1])?;
    coordinate_u32(key, last[2])?;
    u32::try_from(key.timepoint().get())
        .map_err(|_| DatasetSourceFault::CorruptResource { key })?;
    let count = last
        .into_iter()
        .zip(first)
        .try_fold(1_u64, |count, (last, first)| {
            last.checked_sub(first)
                .and_then(|span| span.checked_add(1))
                .and_then(|span| count.checked_mul(span))
        })
        .ok_or(DatasetSourceFault::DecodeFailed { key })?;
    usize::try_from(count).map_err(|_| DatasetSourceFault::DecodeFailed { key })
}

fn append_unaligned_member_work(
    member: UnalignedCohortMember,
    state_index: usize,
    work: &mut Vec<UnalignedPhysicalWork>,
) -> Result<(), DatasetSourceFault> {
    let key = member.key;
    let region_start = key.region().origin();
    let region_end = key.region().end_exclusive();
    let brick_shape = member.mapping.brick_shape;
    let first: [u64; 3] = std::array::from_fn(|axis| region_start[axis] / brick_shape[axis]);
    let last: [u64; 3] = std::array::from_fn(|axis| (region_end[axis] - 1) / brick_shape[axis]);
    let timepoint = u32::try_from(key.timepoint().get())
        .map_err(|_| DatasetSourceFault::CorruptResource { key })?;
    for z in first[0]..=last[0] {
        for y in first[1]..=last[1] {
            for x in first[2]..=last[2] {
                work.push(UnalignedPhysicalWork {
                    coordinates: PackedIndexCoordinates::new(
                        member.mapping.image,
                        key.scale().get(),
                        timepoint,
                        member.mapping.physical_channel,
                        coordinate_u32(key, z)?,
                        coordinate_u32(key, y)?,
                        coordinate_u32(key, x)?,
                    ),
                    brick_coordinates: [z, y, x],
                    state_index,
                });
            }
        }
    }
    Ok(())
}

fn unaligned_control_bytes(
    key: BrickKey,
    members: usize,
    physical_work: usize,
    snapshot_capacity: usize,
    transaction_object_capacity: usize,
) -> Result<u64, DatasetSourceFault> {
    let member_state = members
        .checked_mul(std::mem::size_of::<UnalignedMemberState>())
        .and_then(|bytes| bytes.checked_add(members.checked_mul(std::mem::size_of::<usize>())?));
    let physical = physical_work
        .checked_mul(std::mem::size_of::<UnalignedPhysicalWork>())
        .and_then(|bytes| {
            bytes.checked_add(physical_work.checked_mul(std::mem::size_of::<(
                PackedIndexCoordinates,
                std::sync::Weak<LocalBrickRead>,
            )>())?)
        });
    let snapshot_slot = std::mem::size_of::<crate::range_io::LocalObjectSnapshot>()
        .checked_add(crate::MAX_RELATIVE_PATH_BYTES);
    let snapshots = snapshot_slot.and_then(|slot| snapshot_capacity.checked_mul(slot));
    let transaction = transaction_object_capacity.checked_mul(std::mem::size_of::<Arc<()>>());
    member_state
        .and_then(|members| members.checked_add(physical?))
        .and_then(|bytes| bytes.checked_add(snapshots?))
        .and_then(|bytes| bytes.checked_add(transaction?))
        .and_then(|bytes| u64::try_from(bytes).ok())
        .filter(|bytes| *bytes != 0)
        .ok_or(DatasetSourceFault::DecodeFailed { key })
}

const fn packed_coordinates_sort_key(
    coordinates: PackedIndexCoordinates,
) -> (u32, u32, u32, u32, u32, u32, u32) {
    (
        coordinates.image_ordinal(),
        coordinates.scale(),
        coordinates.t(),
        coordinates.c(),
        coordinates.z_chunk(),
        coordinates.y_chunk(),
        coordinates.x_chunk(),
    )
}

#[allow(clippy::too_many_arguments)]
fn copy_brick_intersection(
    sink: &dyn ReservedDecodeSink,
    key: BrickKey,
    descriptor: ResourcePayloadDescriptor,
    brick_shape: [u64; 3],
    brick_coordinates: [u64; 3],
    brick: &LocalBrickRead,
    staging: &mut [u8],
    facts: &mut PayloadFactsAccumulator,
    counters: &LocalDatasetSourceCounters,
) -> Result<(), DatasetSourceFault> {
    let sample_bytes = usize::from(descriptor.dtype().bytes_per_sample());
    let brick_start = [
        checked_mul(key, brick_coordinates[0], brick_shape[0])?,
        checked_mul(key, brick_coordinates[1], brick_shape[1])?,
        checked_mul(key, brick_coordinates[2], brick_shape[2])?,
    ];
    let brick_end = checked_end(key, brick_start, brick.logical_extent_zyx())?;
    let region_start = key.region().origin();
    let region_end = key.region().end_exclusive();
    let start: [u64; 3] = std::array::from_fn(|axis| region_start[axis].max(brick_start[axis]));
    let end: [u64; 3] = std::array::from_fn(|axis| region_end[axis].min(brick_end[axis]));
    let region_shape = key.region().shape().dimensions();
    let validity_offset = usize::try_from(descriptor.value_byte_len())
        .map_err(|_| DatasetSourceFault::DecodeFailed { key })?;
    let row_samples = usize::try_from(end[2].saturating_sub(start[2]))
        .map_err(|_| DatasetSourceFault::DecodeFailed { key })?;
    let row_bytes = row_samples
        .checked_mul(sample_bytes)
        .ok_or(DatasetSourceFault::DecodeFailed { key })?;
    let all_valid =
        descriptor.validity() == ResourceValidity::AllValid || brick.record().all_voxels_valid();
    let all_invalid = brick.record().all_voxels_invalid();
    let validity = brick.validity_payload();
    let pixel = brick.pixel_payload();

    for z in start[0]..end[0] {
        for y in start[1]..end[1] {
            LocalDatasetSource::checkpoint(sink, key)?;
            if all_invalid {
                continue;
            }
            let source = linear_3d(
                key,
                [
                    z - brick_start[0],
                    y - brick_start[1],
                    start[2] - brick_start[2],
                ],
                brick_shape,
            )?;
            let target = linear_3d(
                key,
                [
                    z - region_start[0],
                    y - region_start[1],
                    start[2] - region_start[2],
                ],
                region_shape,
            )?;
            let source_byte = source
                .checked_mul(sample_bytes)
                .ok_or(DatasetSourceFault::DecodeFailed { key })?;
            let target_byte = target
                .checked_mul(sample_bytes)
                .ok_or(DatasetSourceFault::DecodeFailed { key })?;

            if all_valid {
                if let Some(pixel) = pixel {
                    let source_end = source_byte
                        .checked_add(row_bytes)
                        .ok_or(DatasetSourceFault::DecodeFailed { key })?;
                    let target_end = target_byte
                        .checked_add(row_bytes)
                        .ok_or(DatasetSourceFault::DecodeFailed { key })?;
                    staging[target_byte..target_end]
                        .copy_from_slice(&pixel[source_byte..source_end]);
                    facts.include_bytes(
                        key,
                        descriptor.dtype(),
                        &pixel[source_byte..source_end],
                    )?;
                    let _ = counters.contiguous_copy_bytes.fetch_update(
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                        |bytes| Some(bytes.saturating_add(row_bytes as u64)),
                    );
                } else {
                    facts.include_zeros(key, row_samples)?;
                }
                if descriptor.validity() == ResourceValidity::BitMask {
                    set_validity_range(key, &mut staging[validity_offset..], target, row_samples)?;
                }
                continue;
            }

            let bits = validity.ok_or(DatasetSourceFault::CorruptResource { key })?;
            for offset in 0..row_samples {
                let source = source + offset;
                if bits[source / 8] & (1 << (source % 8)) == 0 {
                    continue;
                }
                let target = target + offset;
                if let Some(pixel) = pixel {
                    let source_byte = source_byte + offset * sample_bytes;
                    let target_byte = target_byte + offset * sample_bytes;
                    staging[target_byte..target_byte + sample_bytes]
                        .copy_from_slice(&pixel[source_byte..source_byte + sample_bytes]);
                    facts.include_bytes(
                        key,
                        descriptor.dtype(),
                        &pixel[source_byte..source_byte + sample_bytes],
                    )?;
                } else {
                    facts.include_value(key, 0.0)?;
                }
                staging[validity_offset + target / 8] |= 1 << (target % 8);
                counters.scalar_copy_samples.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    Ok(())
}

fn set_validity_range(
    key: BrickKey,
    bits: &mut [u8],
    start: usize,
    count: usize,
) -> Result<(), DatasetSourceFault> {
    if count == 0 {
        return Ok(());
    }
    let end = start
        .checked_add(count)
        .ok_or(DatasetSourceFault::DecodeFailed { key })?;
    let first_byte = start / 8;
    let last_byte = (end - 1) / 8;
    if last_byte >= bits.len() {
        return Err(DatasetSourceFault::DecodeFailed { key });
    }

    let first_bit = start % 8;
    let end_bit = end % 8;
    if first_byte == last_byte {
        let upper = if end_bit == 0 {
            u8::MAX
        } else {
            (1_u8 << end_bit) - 1
        };
        bits[first_byte] |= upper & (u8::MAX << first_bit);
        return Ok(());
    }

    let full_start = if first_bit == 0 {
        first_byte
    } else {
        bits[first_byte] |= u8::MAX << first_bit;
        first_byte + 1
    };
    let full_end = end / 8;
    bits[full_start..full_end].fill(u8::MAX);
    if end_bit != 0 {
        bits[full_end] |= (1_u8 << end_bit) - 1;
    }
    Ok(())
}

fn resource_payload_facts(
    key: BrickKey,
    facts: crate::package_read::LocalBrickPayloadFacts,
) -> Result<ResourcePayloadFacts, DatasetSourceFault> {
    ResourcePayloadFacts::from_validated_range(
        facts.minimum(),
        facts.maximum(),
        facts.any_valid(),
        facts.all_valid(),
    )
    .map_err(|_| DatasetSourceFault::CorruptResource { key })
}

fn coordinate_u32(key: BrickKey, value: u64) -> Result<u32, DatasetSourceFault> {
    u32::try_from(value).map_err(|_| DatasetSourceFault::CorruptResource { key })
}

fn checked_mul(key: BrickKey, left: u64, right: u64) -> Result<u64, DatasetSourceFault> {
    left.checked_mul(right)
        .ok_or(DatasetSourceFault::DecodeFailed { key })
}

fn checked_end(
    key: BrickKey,
    start: [u64; 3],
    extent: [u64; 3],
) -> Result<[u64; 3], DatasetSourceFault> {
    Ok([
        start[0]
            .checked_add(extent[0])
            .ok_or(DatasetSourceFault::DecodeFailed { key })?,
        start[1]
            .checked_add(extent[1])
            .ok_or(DatasetSourceFault::DecodeFailed { key })?,
        start[2]
            .checked_add(extent[2])
            .ok_or(DatasetSourceFault::DecodeFailed { key })?,
    ])
}

fn linear_3d(
    key: BrickKey,
    coordinate: [u64; 3],
    shape: [u64; 3],
) -> Result<usize, DatasetSourceFault> {
    let ordinal = coordinate[0]
        .checked_mul(shape[1])
        .and_then(|value| value.checked_add(coordinate[1]))
        .and_then(|value| value.checked_mul(shape[2]))
        .and_then(|value| value.checked_add(coordinate[2]))
        .ok_or(DatasetSourceFault::DecodeFailed { key })?;
    usize::try_from(ordinal).map_err(|_| DatasetSourceFault::DecodeFailed { key })
}

fn write_sink_bytes(
    sink: &mut dyn ReservedDecodeSink,
    key: BrickKey,
    bytes: &[u8],
    counters: &LocalDatasetSourceCounters,
) -> Result<(), DatasetSourceFault> {
    for chunk in bytes.chunks(SINK_WRITE_CHUNK_BYTES) {
        LocalDatasetSource::checkpoint(sink, key)?;
        sink.write(chunk)
            .map_err(|reason| map_sink_error(key, reason))?;
        let _ =
            counters
                .sink_write_bytes
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |bytes| {
                    Some(bytes.saturating_add(chunk.len() as u64))
                });
        LocalDatasetSource::checkpoint(sink, key)?;
    }
    Ok(())
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn subtract_reader_diagnostics(
    current: LocalPackageReadDiagnostics,
    baseline: LocalPackageReadDiagnostics,
) -> LocalPackageReadDiagnostics {
    LocalPackageReadDiagnostics {
        object_open_operations: current
            .object_open_operations
            .saturating_sub(baseline.object_open_operations),
        object_open_time_ns: current
            .object_open_time_ns
            .saturating_sub(baseline.object_open_time_ns),
        open_object_handles_current: current.open_object_handles_current,
        open_object_handles_peak: current.open_object_handles_peak,
        object_handle_cache_entries: current
            .object_handle_cache_entries
            .saturating_sub(baseline.object_handle_cache_entries),
        object_handle_cache_peak_entries: current
            .object_handle_cache_peak_entries
            .saturating_sub(baseline.object_handle_cache_peak_entries),
        object_handle_cache_hits: current
            .object_handle_cache_hits
            .saturating_sub(baseline.object_handle_cache_hits),
        object_handle_cache_misses: current
            .object_handle_cache_misses
            .saturating_sub(baseline.object_handle_cache_misses),
        object_handle_cache_evictions: current
            .object_handle_cache_evictions
            .saturating_sub(baseline.object_handle_cache_evictions),
        object_handle_cache_lock_acquisitions: current
            .object_handle_cache_lock_acquisitions
            .saturating_sub(baseline.object_handle_cache_lock_acquisitions),
        object_handle_cache_lock_contentions: current
            .object_handle_cache_lock_contentions
            .saturating_sub(baseline.object_handle_cache_lock_contentions),
        object_handle_cache_lock_wait_time_ns: current
            .object_handle_cache_lock_wait_time_ns
            .saturating_sub(baseline.object_handle_cache_lock_wait_time_ns),
        shard_index_cache_hits: current
            .shard_index_cache_hits
            .saturating_sub(baseline.shard_index_cache_hits),
        shard_index_cache_misses: current
            .shard_index_cache_misses
            .saturating_sub(baseline.shard_index_cache_misses),
        shard_index_decode_operations: current
            .shard_index_decode_operations
            .saturating_sub(baseline.shard_index_decode_operations),
        packed_inner_cache_hits: current
            .packed_inner_cache_hits
            .saturating_sub(baseline.packed_inner_cache_hits),
        packed_inner_cache_misses: current
            .packed_inner_cache_misses
            .saturating_sub(baseline.packed_inner_cache_misses),
        currentness_pre_use_batches: current
            .currentness_pre_use_batches
            .saturating_sub(baseline.currentness_pre_use_batches),
        currentness_post_use_batches: current
            .currentness_post_use_batches
            .saturating_sub(baseline.currentness_post_use_batches),
        currentness_snapshot_batches: current
            .currentness_snapshot_batches
            .saturating_sub(baseline.currentness_snapshot_batches),
        currentness_root_metadata_checks: current
            .currentness_root_metadata_checks
            .saturating_sub(baseline.currentness_root_metadata_checks),
        currentness_named_object_resolutions: current
            .currentness_named_object_resolutions
            .saturating_sub(baseline.currentness_named_object_resolutions),
        currentness_object_fd_metadata_checks: current
            .currentness_object_fd_metadata_checks
            .saturating_sub(baseline.currentness_object_fd_metadata_checks),
        currentness_time_ns: current
            .currentness_time_ns
            .saturating_sub(baseline.currentness_time_ns),
        physical_range_read_operations: current
            .physical_range_read_operations
            .saturating_sub(baseline.physical_range_read_operations),
        physical_encoded_bytes_read: current
            .physical_encoded_bytes_read
            .saturating_sub(baseline.physical_encoded_bytes_read),
        physical_range_read_time_ns: current
            .physical_range_read_time_ns
            .saturating_sub(baseline.physical_range_read_time_ns),
        codec_decode_operations: current
            .codec_decode_operations
            .saturating_sub(baseline.codec_decode_operations),
        codec_decoded_bytes: current
            .codec_decoded_bytes
            .saturating_sub(baseline.codec_decoded_bytes),
        codec_decode_time_ns: current
            .codec_decode_time_ns
            .saturating_sub(baseline.codec_decode_time_ns),
    }
}

fn invalid_resource(key: BrickKey, reason: ResourceContractError) -> DatasetSourceFault {
    DatasetSourceFault::InvalidResource {
        key,
        reason: Box::new(reason),
    }
}

fn map_ledger_error(
    key: BrickKey,
    requested_bytes: u64,
    error: CpuLedgerError,
) -> DatasetSourceFault {
    match error {
        CpuLedgerError::CapacityExceeded {
            category,
            requested_bytes,
            available_bytes,
        } => DatasetSourceFault::CapacityExceeded {
            key,
            category,
            requested_bytes,
            available_bytes,
        },
        CpuLedgerError::ShuttingDown => DatasetSourceFault::ShuttingDown {
            key,
            category: CpuLedgerCategory::InFlightDecode,
            requested_bytes,
        },
        CpuLedgerError::ZeroByteReservation => DatasetSourceFault::DecodeFailed { key },
    }
}

fn map_sink_error(key: BrickKey, reason: DecodeSinkError) -> DatasetSourceFault {
    match reason {
        DecodeSinkError::Cancelled => DatasetSourceFault::Cancelled { key },
        reason => DatasetSourceFault::SinkRejected {
            key,
            reason: Box::new(reason),
        },
    }
}

fn map_read_error(key: BrickKey, error: PackageReadError) -> DatasetSourceFault {
    match error {
        PackageReadError::Cancelled => DatasetSourceFault::Cancelled { key },
        PackageReadError::Range(RangeReadError::Io {
            kind: std::io::ErrorKind::NotFound,
            ..
        }) => DatasetSourceFault::ResourceUnavailable { key },
        _ => DatasetSourceFault::CorruptResource { key },
    }
}

#[derive(Clone, Copy)]
enum SharedSourceFault {
    Cancelled,
    ResourceUnavailable,
    CapacityExceeded {
        category: CpuLedgerCategory,
        requested_bytes: u64,
        available_bytes: u64,
    },
    ShuttingDown {
        category: CpuLedgerCategory,
        requested_bytes: u64,
    },
    DecodeFailed,
    Corrupt,
}

impl SharedSourceFault {
    fn from_read_error(error: &PackageReadError) -> Self {
        match error {
            PackageReadError::Cancelled => Self::Cancelled,
            PackageReadError::Range(RangeReadError::Io {
                kind: std::io::ErrorKind::NotFound,
                ..
            }) => Self::ResourceUnavailable,
            _ => Self::Corrupt,
        }
    }

    fn from_dataset_fault(error: &DatasetSourceFault) -> Self {
        match error {
            DatasetSourceFault::Cancelled { .. } => Self::Cancelled,
            DatasetSourceFault::ResourceUnavailable { .. } => Self::ResourceUnavailable,
            DatasetSourceFault::CapacityExceeded {
                category,
                requested_bytes,
                available_bytes,
                ..
            } => Self::CapacityExceeded {
                category: *category,
                requested_bytes: *requested_bytes,
                available_bytes: *available_bytes,
            },
            DatasetSourceFault::ShuttingDown {
                category,
                requested_bytes,
                ..
            } => Self::ShuttingDown {
                category: *category,
                requested_bytes: *requested_bytes,
            },
            DatasetSourceFault::DecodeFailed { .. } | DatasetSourceFault::CatalogUnavailable => {
                Self::DecodeFailed
            }
            _ => Self::Corrupt,
        }
    }

    fn for_key(self, key: BrickKey) -> DatasetSourceFault {
        match self {
            Self::Cancelled => DatasetSourceFault::Cancelled { key },
            Self::ResourceUnavailable => DatasetSourceFault::ResourceUnavailable { key },
            Self::CapacityExceeded {
                category,
                requested_bytes,
                available_bytes,
            } => DatasetSourceFault::CapacityExceeded {
                key,
                category,
                requested_bytes,
                available_bytes,
            },
            Self::ShuttingDown {
                category,
                requested_bytes,
            } => DatasetSourceFault::ShuttingDown {
                key,
                category,
                requested_bytes,
            },
            Self::DecodeFailed => DatasetSourceFault::DecodeFailed { key },
            Self::Corrupt => DatasetSourceFault::CorruptResource { key },
        }
    }
}

fn saturating_counter_add(counter: &AtomicU64, value: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(value))
    });
}

fn map_direct_read_error(key: BrickKey, error: LocalDirectBrickReadError) -> DatasetSourceFault {
    match error {
        LocalDirectBrickReadError::Package(error) => map_read_error(key, error),
        LocalDirectBrickReadError::Sink(error) => map_sink_error(key, error),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        io::{Read, Seek, SeekFrom, Write},
        path::{Component, Path, PathBuf},
        sync::{
            Arc, Barrier, Condvar, Mutex,
            atomic::{AtomicU64, Ordering},
            mpsc,
        },
        thread,
        time::{Duration, Instant},
    };

    use mirante4d_dataset::{
        CpuByteLease, DatasetResourceIdentity, ResourcePayloadDescriptor, ResourceRegion,
    };
    use mirante4d_dataset_runtime::{
        CancellationGeneration, DatasetRuntime, DatasetRuntimeConfig, RequestPriority,
        ResourceRequest, RuntimeFault, RuntimeFaultCode, RuntimeOutcome,
    };
    use mirante4d_domain::{LogicalLayerKey, Shape4D, TimeIndex};
    use mirante4d_identity::{
        ScientificContentId, ScientificDatasetHasher, ScientificLayerDescriptor,
        ScientificLayerHasher, ScientificTemporalCalibration as IdentityTemporalCalibration,
        ScientificTile,
    };

    use super::*;

    const TEST_CAPACITY_BYTES: u64 = 16 * 1024 * 1024;
    const TAR_BLOCK_BYTES: usize = 512;

    #[derive(Debug)]
    struct TestLedgerState {
        used: Mutex<[u64; 7]>,
        peak: Mutex<[u64; 7]>,
    }

    #[derive(Clone, Debug)]
    struct TestLedger {
        state: Arc<TestLedgerState>,
        capacity_bytes: u64,
    }

    impl Default for TestLedger {
        fn default() -> Self {
            Self {
                state: Arc::new(TestLedgerState {
                    used: Mutex::new([0; 7]),
                    peak: Mutex::new([0; 7]),
                }),
                capacity_bytes: TEST_CAPACITY_BYTES,
            }
        }
    }

    #[derive(Debug)]
    struct TestLease {
        state: Arc<TestLedgerState>,
        category: CpuLedgerCategory,
        bytes: u64,
    }

    impl Drop for TestLease {
        fn drop(&mut self) {
            self.state.used.lock().unwrap()[category_index(self.category)] -= self.bytes;
        }
    }

    impl CpuByteLease for TestLease {
        fn category(&self) -> CpuLedgerCategory {
            self.category
        }

        fn reserved_bytes(&self) -> u64 {
            self.bytes
        }
    }

    impl CpuByteLedger for TestLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            if bytes == 0 {
                return Err(CpuLedgerError::ZeroByteReservation);
            }
            let index = category_index(category);
            let mut used = self.state.used.lock().unwrap();
            let available = self.capacity_bytes.saturating_sub(used[index]);
            if bytes > available {
                return Err(CpuLedgerError::CapacityExceeded {
                    category,
                    requested_bytes: bytes,
                    available_bytes: available,
                });
            }
            used[index] += bytes;
            let current = used[index];
            drop(used);
            let mut peak = self.state.peak.lock().unwrap();
            peak[index] = peak[index].max(current);
            Ok(Box::new(TestLease {
                state: Arc::clone(&self.state),
                category,
                bytes,
            }))
        }
    }

    impl TestLedger {
        fn with_capacity(capacity_bytes: u64) -> Self {
            Self {
                capacity_bytes,
                ..Self::default()
            }
        }

        fn used(&self, category: CpuLedgerCategory) -> u64 {
            self.state.used.lock().unwrap()[category_index(category)]
        }

        fn peak(&self, category: CpuLedgerCategory) -> u64 {
            self.state.peak.lock().unwrap()[category_index(category)]
        }
    }

    #[derive(Debug, Default)]
    struct PhysicalDecodeGate {
        state: Mutex<(bool, bool)>,
        wake: Condvar,
    }

    impl PhysicalDecodeGate {
        fn wait_until_entered(&self) {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut state = self.state.lock().unwrap();
            while !state.0 {
                let timeout = deadline.saturating_duration_since(Instant::now());
                assert!(!timeout.is_zero(), "physical decode never reached its gate");
                let (next, result) = self.wake.wait_timeout(state, timeout).unwrap();
                state = next;
                assert!(
                    state.0 || !result.timed_out(),
                    "physical decode never reached its gate"
                );
            }
        }

        fn enter_and_wait(&self) {
            let mut state = self.state.lock().unwrap();
            state.0 = true;
            self.wake.notify_all();
            while !state.1 {
                state = self.wake.wait(state).unwrap();
            }
        }

        fn release(&self) {
            let mut state = self.state.lock().unwrap();
            state.1 = true;
            drop(state);
            self.wake.notify_all();
        }
    }

    #[derive(Clone, Debug)]
    struct GatedBypassLedger {
        inner: TestLedger,
        physical_decode: Arc<PhysicalDecodeGate>,
    }

    impl Default for GatedBypassLedger {
        fn default() -> Self {
            Self {
                inner: TestLedger::default(),
                physical_decode: Arc::new(PhysicalDecodeGate::default()),
            }
        }
    }

    impl CpuByteLedger for GatedBypassLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            if category == CpuLedgerCategory::DecodedResidency {
                return Err(CpuLedgerError::CapacityExceeded {
                    category,
                    requested_bytes: bytes,
                    available_bytes: 0,
                });
            }
            if category == CpuLedgerCategory::InFlightDecode {
                self.physical_decode.enter_and_wait();
            }
            self.inner.try_acquire(category, bytes)
        }
    }

    impl GatedBypassLedger {
        fn used(&self, category: CpuLedgerCategory) -> u64 {
            self.inner.used(category)
        }
    }

    #[derive(Debug, Default)]
    struct RuntimeStorageGateState {
        blocker_members_entered: usize,
        target_members_entered: usize,
        target_cohort_sizes: Vec<usize>,
        active_target_cohorts: usize,
        peak_active_target_cohorts: usize,
        blockers_released: bool,
        targets_released: bool,
    }

    #[derive(Debug)]
    struct RuntimeStorageGate {
        state: Mutex<RuntimeStorageGateState>,
        changed: Condvar,
        blocker_finish: Barrier,
    }

    impl RuntimeStorageGate {
        fn new(workers: usize) -> Self {
            Self {
                state: Mutex::new(RuntimeStorageGateState::default()),
                changed: Condvar::new(),
                blocker_finish: Barrier::new(workers),
            }
        }

        fn wait_for(
            &self,
            mut predicate: impl FnMut(&RuntimeStorageGateState) -> bool,
            timeout_message: &str,
        ) {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut state = self.state.lock().unwrap();
            while !predicate(&state) {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "{timeout_message}");
                let (next, timeout) = self.changed.wait_timeout(state, remaining).unwrap();
                state = next;
                assert!(
                    predicate(&state) || !timeout.timed_out(),
                    "{timeout_message}"
                );
            }
        }

        fn release_blockers(&self) {
            let mut state = self.state.lock().unwrap();
            state.blockers_released = true;
            drop(state);
            self.changed.notify_all();
        }

        fn release_targets(&self) {
            let mut state = self.state.lock().unwrap();
            state.targets_released = true;
            drop(state);
            self.changed.notify_all();
        }
    }

    struct GatedRuntimeStorageSource {
        inner: Arc<LocalDatasetSource>,
        gate: Arc<RuntimeStorageGate>,
    }

    impl DatasetSource for GatedRuntimeStorageSource {
        fn catalog(&self) -> Result<Arc<DatasetCatalog>, DatasetSourceFault> {
            self.inner.catalog()
        }

        fn minimum_decode_working_bytes(
            &self,
            key: BrickKey,
            descriptor: ResourcePayloadDescriptor,
        ) -> Result<u64, DatasetSourceFault> {
            self.inner.minimum_decode_working_bytes(key, descriptor)
        }

        fn decode_cohort_into(
            &self,
            sinks: &mut [&mut dyn ReservedDecodeSink],
        ) -> Vec<Result<(), DatasetSourceFault>> {
            let target = sinks[0].resource_key().region().shape().dimensions() == [1, 257, 1_025];
            if target {
                let mut state = self.gate.state.lock().unwrap();
                state.target_members_entered += sinks.len();
                state.target_cohort_sizes.push(sinks.len());
                state.active_target_cohorts += 1;
                state.peak_active_target_cohorts = state
                    .peak_active_target_cohorts
                    .max(state.active_target_cohorts);
                self.gate.changed.notify_all();
                while !state.targets_released {
                    state = self.gate.changed.wait(state).unwrap();
                }
                drop(state);

                let outcomes = self.inner.decode_cohort_into(sinks);
                let mut state = self.gate.state.lock().unwrap();
                state.active_target_cohorts -= 1;
                drop(state);
                self.gate.changed.notify_all();
                outcomes
            } else {
                let mut state = self.gate.state.lock().unwrap();
                state.blocker_members_entered += sinks.len();
                self.gate.changed.notify_all();
                while !state.blockers_released {
                    state = self.gate.changed.wait(state).unwrap();
                }
                drop(state);

                let outcomes = self.inner.decode_cohort_into(sinks);
                // No worker may claim visible work until every lower-priority
                // singleton has retired, making the ensuing scheduler burst
                // deterministic without changing production scheduling.
                self.gate.blocker_finish.wait();
                outcomes
            }
        }
    }

    const fn category_index(category: CpuLedgerCategory) -> usize {
        match category {
            CpuLedgerCategory::DecodedResidency => 0,
            CpuLedgerCategory::UploadStaging => 1,
            CpuLedgerCategory::InFlightDecode => 2,
            CpuLedgerCategory::MetadataAndIndexes => 3,
            CpuLedgerCategory::QueuesAndResults => 4,
            CpuLedgerCategory::Prefetch => 5,
            CpuLedgerCategory::ImportWorkingSet => 6,
        }
    }

    struct TestSink {
        key: BrickKey,
        descriptor: ResourcePayloadDescriptor,
        bytes: Vec<u8>,
        finished: bool,
        supplied_facts: Option<ResourcePayloadFacts>,
        write_calls: u64,
        largest_write: usize,
        committed: usize,
        offered_span: usize,
        direct_committed_bytes: u64,
    }

    impl TestSink {
        fn new(key: BrickKey, descriptor: ResourcePayloadDescriptor) -> Self {
            Self {
                key,
                descriptor,
                bytes: Vec::with_capacity(usize::try_from(descriptor.byte_len()).unwrap()),
                finished: false,
                supplied_facts: None,
                write_calls: 0,
                largest_write: 0,
                committed: 0,
                offered_span: 0,
                direct_committed_bytes: 0,
            }
        }
    }

    impl ReservedDecodeSink for TestSink {
        fn resource_key(&self) -> BrickKey {
            self.key
        }

        fn payload_descriptor(&self) -> ResourcePayloadDescriptor {
            self.descriptor
        }

        fn written_bytes(&self) -> u64 {
            u64::try_from(self.committed).unwrap()
        }

        fn is_cancelled(&self) -> bool {
            false
        }

        fn writable_span(&mut self, maximum_bytes: usize) -> Result<&mut [u8], DecodeSinkError> {
            if self.finished {
                return Err(DecodeSinkError::AlreadyFinished);
            }
            if self.offered_span != 0 {
                return Err(DecodeSinkError::WritableSpanOutstanding);
            }
            let reserved = usize::try_from(self.descriptor.byte_len()).unwrap();
            let remaining = reserved.saturating_sub(self.committed);
            if maximum_bytes == 0 || remaining == 0 {
                return Err(DecodeSinkError::InvalidWritableSpanRequest);
            }
            self.offered_span = remaining.min(maximum_bytes);
            self.bytes.resize(self.committed + self.offered_span, 0);
            Ok(&mut self.bytes[self.committed..])
        }

        fn commit_written(&mut self, bytes: usize) -> Result<(), DecodeSinkError> {
            let offered = self.offered_span;
            if offered == 0 {
                return Err(DecodeSinkError::WritableCommitWithoutSpan);
            }
            if bytes > offered {
                return Err(DecodeSinkError::WritableCommitExceeded {
                    offered,
                    attempted: bytes,
                });
            }
            self.committed = self
                .committed
                .checked_add(bytes)
                .ok_or(DecodeSinkError::ByteCountOverflow)?;
            self.bytes.truncate(self.committed);
            self.offered_span = 0;
            self.direct_committed_bytes = self
                .direct_committed_bytes
                .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
            Ok(())
        }

        fn write(&mut self, bytes: &[u8]) -> Result<(), DecodeSinkError> {
            if self.finished {
                return Err(DecodeSinkError::AlreadyFinished);
            }
            if self.offered_span != 0 {
                return Err(DecodeSinkError::WritableSpanOutstanding);
            }
            let attempted = self
                .bytes
                .len()
                .checked_add(bytes.len())
                .ok_or(DecodeSinkError::ByteCountOverflow)?;
            if u64::try_from(attempted).unwrap_or(u64::MAX) > self.descriptor.byte_len() {
                return Err(DecodeSinkError::ReservationExceeded {
                    reserved: self.descriptor.byte_len(),
                    attempted: u64::try_from(attempted).unwrap_or(u64::MAX),
                });
            }
            self.bytes.extend_from_slice(bytes);
            self.committed = attempted;
            self.write_calls += 1;
            self.largest_write = self.largest_write.max(bytes.len());
            Ok(())
        }

        fn finish(&mut self) -> Result<(), DecodeSinkError> {
            if self.finished {
                return Err(DecodeSinkError::AlreadyFinished);
            }
            if self.offered_span != 0 {
                return Err(DecodeSinkError::WritableSpanOutstanding);
            }
            if self.written_bytes() != self.descriptor.byte_len() {
                return Err(DecodeSinkError::Incomplete {
                    reserved: self.descriptor.byte_len(),
                    written: self.written_bytes(),
                });
            }
            self.finished = true;
            Ok(())
        }

        fn finish_with_facts(
            &mut self,
            facts: ResourcePayloadFacts,
        ) -> Result<(), DecodeSinkError> {
            self.finish()?;
            self.supplied_facts = Some(facts);
            Ok(())
        }

        fn is_finished(&self) -> bool {
            self.finished
        }
    }

    fn decode_one(
        source: &dyn DatasetSource,
        sink: &mut dyn ReservedDecodeSink,
    ) -> Result<(), DatasetSourceFault> {
        let mut cohort = [sink];
        let mut outcomes = source.decode_cohort_into(&mut cohort);
        assert_eq!(outcomes.len(), 1);
        outcomes.remove(0)
    }

    fn assert_supplied_facts_match_payload(sink: &TestSink) {
        let value_len = usize::try_from(sink.descriptor.value_byte_len()).unwrap();
        let (values, validity) = sink.bytes.split_at(value_len);
        let payload = sink
            .descriptor
            .view(
                values,
                (sink.descriptor.validity() == ResourceValidity::BitMask).then_some(validity),
            )
            .unwrap();
        assert_eq!(
            sink.supplied_facts,
            Some(ResourcePayloadFacts::from_payload(payload).unwrap())
        );
    }

    fn aligned_nan_target(valid_nan: bool) -> TargetFixture {
        aligned_explicit_target(valid_nan, true)
    }

    fn aligned_finite_explicit_validity_target() -> TargetFixture {
        aligned_explicit_target(false, false)
    }

    fn aligned_explicit_target(valid_nan: bool, nan_pixel: bool) -> TargetFixture {
        let scientific_id = if valid_nan {
            ScientificContentId::parse(
                "m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap()
        } else {
            aligned_invalid_nan_scientific_id()
        };
        let one = crate::F64Bits::parse("3ff0000000000000").unwrap();
        let zero = crate::F64Bits::parse("0000000000000000").unwrap();
        let temporal = crate::ScienceTemporalCalibration::regular(one).unwrap();
        let level = crate::ProfileLevel::new(0, 0, ProfileValidityMode::Explicit).unwrap();
        let image = crate::ProfileImage::new(
            0,
            vec![crate::ProfileLogicalLayer::new(LogicalLayerKey::new(0), 0)],
            vec![level.clone()],
        )
        .unwrap();
        let profile = crate::ProfileHeader::new(
            scientific_id,
            vec![image.clone()],
            0,
            crate::OmeInteroperabilityBase::Io1,
        )
        .unwrap();
        let identity = [
            one, zero, zero, zero, zero, one, zero, zero, zero, zero, one, zero, zero, zero, zero,
            one,
        ];
        let science = crate::ScienceDescriptor::new(
            scientific_id,
            vec![
                crate::ScienceLayer::new(
                    LogicalLayerKey::new(0),
                    Shape4D::new(1, 64, 64, 64).unwrap(),
                    IntensityDType::Float32,
                    temporal.clone(),
                    identity,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let display = crate::DisplayDefaults::new(vec![
            crate::DisplayLayerDefaults::new(
                LogicalLayerKey::new(0),
                true,
                crate::Rgb24::parse("ffffff").unwrap(),
                crate::F32Bits::parse("00000000").unwrap(),
                crate::F32Bits::parse("3f800000").unwrap(),
            )
            .unwrap(),
        ])
        .unwrap();
        let ome = crate::OmeImageGroupMetadata::new(
            &image,
            &temporal,
            vec![OmeLevelTransform::DiagonalMicrometer {
                scale_zyx: [one; 3],
                translation_zyx: [zero; 3],
            }],
        )
        .unwrap();
        let arrays = vec![
            crate::PackageArrayInput::new(
                level.pixel_path().clone(),
                crate::ZarrArrayMetadata::new(
                    ShardProfileKind::Pixel3dFloat32,
                    vec![1, 1, 64, 64, 64],
                )
                .unwrap(),
            ),
            crate::PackageArrayInput::new(
                level.validity_path().unwrap().clone(),
                crate::ZarrArrayMetadata::new(ShardProfileKind::Validity3d, vec![1, 1, 64, 64, 8])
                    .unwrap(),
            ),
            crate::PackageArrayInput::new(
                level.packed_index_path().clone(),
                crate::ZarrArrayMetadata::new(ShardProfileKind::PackedIndex, vec![1, 64]).unwrap(),
            ),
        ];

        let capacity = 64_u64 * 64 * 64;
        let valid_samples = if valid_nan { capacity } else { capacity - 1 };
        let record = crate::PackedIndexRecord::new(
            PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0),
            crate::PackedIndexStatistics::new(valid_samples, 0, Some((0, 0))),
            true,
            true,
            IntensityDType::Float32,
            capacity,
        )
        .unwrap();
        let pixel_kind = ShardProfileKind::Pixel3dFloat32;
        let mut pixel = vec![0; pixel_kind.decoded_inner_bytes()];
        if nan_pixel {
            pixel[..4].copy_from_slice(&f32::NAN.to_le_bytes());
        }
        let mut pixel_chunks = missing_test_chunks(pixel_kind);
        pixel_chunks[0] = Some(pixel);
        let validity_kind = ShardProfileKind::Validity3d;
        let mut validity = vec![u8::MAX; validity_kind.decoded_inner_bytes()];
        if !valid_nan {
            validity[0] &= !1;
        }
        let mut validity_chunks = missing_test_chunks(validity_kind);
        validity_chunks[0] = Some(validity);
        let packed_kind = ShardProfileKind::PackedIndex;
        let mut packed = vec![0; packed_kind.decoded_inner_bytes()];
        packed[..crate::PACKED_INDEX_RECORD_BYTES as usize].copy_from_slice(&record.encode());
        let mut packed_chunks = missing_test_chunks(packed_kind);
        packed_chunks[0] = Some(packed);
        let shards = vec![
            crate::PackageShardInput::new(
                level.pixel_path().clone(),
                vec![0, 0, 0, 0, 0],
                pixel_chunks,
            ),
            crate::PackageShardInput::new(
                level.validity_path().unwrap().clone(),
                vec![0, 0, 0, 0, 0],
                validity_chunks,
            ),
            crate::PackageShardInput::new(
                level.packed_index_path().clone(),
                vec![0, 0],
                packed_chunks,
            ),
        ];
        let fixture = TargetFixture::empty(match (nan_pixel, valid_nan) {
            (true, true) => "aligned-valid-nan",
            (true, false) => "aligned-invalid-nan",
            (false, _) => "aligned-finite-explicit-validity",
        });
        crate::LocalPackageWriter::write_new(
            fixture.path(),
            crate::PackageWriteInput::new(
                crate::ProfileKind::Current,
                profile,
                science,
                display,
                Vec::new(),
                vec![ome],
                arrays,
                shards,
            ),
            || false,
        )
        .unwrap();
        fixture
    }

    fn aligned_invalid_nan_scientific_id() -> ScientificContentId {
        let shape = Shape4D::new(1, 64, 64, 64).unwrap();
        let descriptor = ScientificLayerDescriptor::new(
            LogicalLayerKey::new(0),
            IntensityDType::Float32,
            shape,
            IdentityTemporalCalibration::Regular { step_seconds: 1.0 },
            GridToWorld::identity(),
        )
        .unwrap();
        let tile_samples = 16_usize * 64 * 64;
        let values =
            vec![0_u8; tile_samples * usize::from(IntensityDType::Float32.bytes_per_sample())];
        let mut validity = vec![u8::MAX; tile_samples / 8];
        let mut layer = ScientificLayerHasher::new(descriptor).unwrap();
        for z in [0_u64, 16, 32, 48] {
            validity[0] = if z == 0 { 0xfe } else { u8::MAX };
            layer
                .push_tile(ScientificTile::new(
                    [0, z, 0, 0],
                    [1, 16, 64, 64],
                    &validity,
                    &values,
                ))
                .unwrap();
        }
        let mut dataset = ScientificDatasetHasher::new(1).unwrap();
        dataset.push_layer(layer.finalize().unwrap()).unwrap();
        dataset.finalize().unwrap()
    }

    fn one_zero_u8_scientific_id() -> ScientificContentId {
        let descriptor = ScientificLayerDescriptor::new(
            LogicalLayerKey::new(0),
            IntensityDType::Uint8,
            Shape4D::new(1, 1, 1, 1).unwrap(),
            IdentityTemporalCalibration::Regular { step_seconds: 1.0 },
            GridToWorld::identity(),
        )
        .unwrap();
        let mut layer = ScientificLayerHasher::new(descriptor).unwrap();
        layer
            .push_tile(ScientificTile::new([0; 4], [1; 4], &[1], &[0]))
            .unwrap();
        let mut dataset = ScientificDatasetHasher::new(1).unwrap();
        dataset.push_layer(layer.finalize().unwrap()).unwrap();
        dataset.finalize().unwrap()
    }

    /// Builds an exact, base-scientifically-correct package whose s1 packed
    /// record lies about one zero sample. The writer authenticates those bytes
    /// normally, so only the all-scale scientific fact proof can reject it.
    fn coarse_scale_lied_statistics_target() -> TargetFixture {
        let scientific_id = one_zero_u8_scientific_id();
        let one = crate::F64Bits::parse("3ff0000000000000").unwrap();
        let two = crate::F64Bits::parse("4000000000000000").unwrap();
        let zero = crate::F64Bits::parse("0000000000000000").unwrap();
        let temporal = crate::ScienceTemporalCalibration::regular(one).unwrap();
        let levels = [
            crate::ProfileLevel::new(0, 0, ProfileValidityMode::AllValid).unwrap(),
            crate::ProfileLevel::new(0, 1, ProfileValidityMode::AllValid).unwrap(),
        ];
        let image = crate::ProfileImage::new(
            0,
            vec![crate::ProfileLogicalLayer::new(LogicalLayerKey::new(0), 0)],
            levels.to_vec(),
        )
        .unwrap();
        let profile = crate::ProfileHeader::new(
            scientific_id,
            vec![image.clone()],
            0,
            crate::OmeInteroperabilityBase::Io2,
        )
        .unwrap();
        let identity = [
            one, zero, zero, zero, zero, one, zero, zero, zero, zero, one, zero, zero, zero, zero,
            one,
        ];
        let science = crate::ScienceDescriptor::new(
            scientific_id,
            vec![
                crate::ScienceLayer::new(
                    LogicalLayerKey::new(0),
                    Shape4D::new(1, 1, 1, 1).unwrap(),
                    IntensityDType::Uint8,
                    temporal.clone(),
                    identity,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let display = crate::DisplayDefaults::new(vec![
            crate::DisplayLayerDefaults::new(
                LogicalLayerKey::new(0),
                true,
                crate::Rgb24::parse("ffffff").unwrap(),
                crate::F32Bits::parse("00000000").unwrap(),
                crate::F32Bits::parse("3f800000").unwrap(),
            )
            .unwrap(),
        ])
        .unwrap();
        let ome = crate::OmeImageGroupMetadata::new(
            &image,
            &temporal,
            vec![
                OmeLevelTransform::DiagonalMicrometer {
                    scale_zyx: [one; 3],
                    translation_zyx: [zero; 3],
                },
                OmeLevelTransform::DiagonalMicrometer {
                    scale_zyx: [two; 3],
                    translation_zyx: [zero; 3],
                },
            ],
        )
        .unwrap();

        let mut arrays = Vec::new();
        let mut shards = Vec::new();
        for (ordinal, level) in levels.iter().enumerate() {
            arrays.push(crate::PackageArrayInput::new(
                level.pixel_path().clone(),
                crate::ZarrArrayMetadata::new(ShardProfileKind::Pixel2dUint8, vec![1, 1, 1, 1, 1])
                    .unwrap(),
            ));
            arrays.push(crate::PackageArrayInput::new(
                level.packed_index_path().clone(),
                crate::ZarrArrayMetadata::new(ShardProfileKind::PackedIndex, vec![1, 64]).unwrap(),
            ));

            let pixel_kind = ShardProfileKind::Pixel2dUint8;
            let mut pixel_chunks = missing_test_chunks(pixel_kind);
            pixel_chunks[0] = Some(vec![0; pixel_kind.decoded_inner_bytes()]);
            shards.push(crate::PackageShardInput::new(
                level.pixel_path().clone(),
                vec![0, 0, 0, 0, 0],
                pixel_chunks,
            ));

            let statistics = if ordinal == 0 {
                crate::PackedIndexStatistics::new(1, 0, Some((0, 0)))
            } else {
                crate::PackedIndexStatistics::new(1, 1, Some((1, 1)))
            };
            let record = crate::PackedIndexRecord::new(
                PackedIndexCoordinates::new(0, ordinal as u32, 0, 0, 0, 0, 0),
                statistics,
                true,
                false,
                IntensityDType::Uint8,
                1,
            )
            .unwrap();
            let packed_kind = ShardProfileKind::PackedIndex;
            let mut packed = vec![0; packed_kind.decoded_inner_bytes()];
            packed[..crate::PACKED_INDEX_RECORD_BYTES as usize].copy_from_slice(&record.encode());
            let mut packed_chunks = missing_test_chunks(packed_kind);
            packed_chunks[0] = Some(packed);
            shards.push(crate::PackageShardInput::new(
                level.packed_index_path().clone(),
                vec![0, 0],
                packed_chunks,
            ));
        }

        let fixture = TargetFixture::empty("coarse-lied-statistics");
        crate::LocalPackageWriter::write_new(
            fixture.path(),
            crate::PackageWriteInput::new(
                crate::ProfileKind::Current,
                profile,
                science,
                display,
                Vec::new(),
                vec![ome],
                arrays,
                shards,
            ),
            || false,
        )
        .unwrap();
        fixture
    }

    fn wide_2d_overlap_target() -> TargetFixture {
        const HEIGHT: u64 = 1_025;
        const WIDTH: u64 = 2_049;
        const Y_BRICKS: u32 = 5;
        const X_BRICKS: u32 = 9;

        let scientific_id = ScientificContentId::parse(
            "m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        let one = crate::F64Bits::parse("3ff0000000000000").unwrap();
        let zero = crate::F64Bits::parse("0000000000000000").unwrap();
        let temporal = crate::ScienceTemporalCalibration::regular(one).unwrap();
        let level = crate::ProfileLevel::new(0, 0, ProfileValidityMode::AllValid).unwrap();
        let image = crate::ProfileImage::new(
            0,
            vec![crate::ProfileLogicalLayer::new(LogicalLayerKey::new(0), 0)],
            vec![level.clone()],
        )
        .unwrap();
        let profile = crate::ProfileHeader::new(
            scientific_id,
            vec![image.clone()],
            0,
            crate::OmeInteroperabilityBase::Io1,
        )
        .unwrap();
        let identity = [
            one, zero, zero, zero, zero, one, zero, zero, zero, zero, one, zero, zero, zero, zero,
            one,
        ];
        let science = crate::ScienceDescriptor::new(
            scientific_id,
            vec![
                crate::ScienceLayer::new(
                    LogicalLayerKey::new(0),
                    Shape4D::new(1, 1, HEIGHT, WIDTH).unwrap(),
                    IntensityDType::Uint8,
                    temporal.clone(),
                    identity,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let display = crate::DisplayDefaults::new(vec![
            crate::DisplayLayerDefaults::new(
                LogicalLayerKey::new(0),
                true,
                crate::Rgb24::parse("ffffff").unwrap(),
                crate::F32Bits::parse("00000000").unwrap(),
                crate::F32Bits::parse("3f800000").unwrap(),
            )
            .unwrap(),
        ])
        .unwrap();
        let ome = crate::OmeImageGroupMetadata::new(
            &image,
            &temporal,
            vec![OmeLevelTransform::DiagonalMicrometer {
                scale_zyx: [one; 3],
                translation_zyx: [zero; 3],
            }],
        )
        .unwrap();
        let arrays = vec![
            crate::PackageArrayInput::new(
                level.pixel_path().clone(),
                crate::ZarrArrayMetadata::new(
                    ShardProfileKind::Pixel2dUint8,
                    vec![1, 1, 1, HEIGHT, WIDTH],
                )
                .unwrap(),
            ),
            crate::PackageArrayInput::new(
                level.packed_index_path().clone(),
                crate::ZarrArrayMetadata::new(
                    ShardProfileKind::PackedIndex,
                    vec![u64::from(Y_BRICKS * X_BRICKS), 64],
                )
                .unwrap(),
            ),
        ];

        let pixel_kind = ShardProfileKind::Pixel2dUint8;
        let one_brick = vec![1_u8; pixel_kind.decoded_inner_bytes()];
        let mut first_pixel_shard = missing_test_chunks(pixel_kind);
        let mut second_pixel_shard = missing_test_chunks(pixel_kind);
        for y in 0..2_u32 {
            for x in 0..5_u32 {
                let slot = usize::try_from((y % 4) * 4 + (x % 4)).unwrap();
                if x < 4 {
                    first_pixel_shard[slot] = Some(one_brick.clone());
                } else {
                    second_pixel_shard[slot] = Some(one_brick.clone());
                }
            }
        }

        let packed_kind = ShardProfileKind::PackedIndex;
        let mut packed = vec![0_u8; packed_kind.decoded_inner_bytes()];
        for y in 0..Y_BRICKS {
            for x in 0..X_BRICKS {
                let extent_y = (HEIGHT - u64::from(y) * 256).min(256);
                let extent_x = (WIDTH - u64::from(x) * 256).min(256);
                let capacity = extent_y * extent_x;
                let present = y < 2 && x < 5;
                let statistics = if present {
                    crate::PackedIndexStatistics::new(capacity, capacity, Some((1, 1)))
                } else {
                    crate::PackedIndexStatistics::new(capacity, 0, Some((0, 0)))
                };
                let record = crate::PackedIndexRecord::new(
                    PackedIndexCoordinates::new(0, 0, 0, 0, 0, y, x),
                    statistics,
                    present,
                    false,
                    IntensityDType::Uint8,
                    capacity,
                )
                .unwrap();
                let ordinal = usize::try_from(y * X_BRICKS + x).unwrap();
                let start = ordinal * crate::PACKED_INDEX_RECORD_BYTES as usize;
                let end = start + crate::PACKED_INDEX_RECORD_BYTES as usize;
                packed[start..end].copy_from_slice(&record.encode());
            }
        }
        let mut packed_chunks = missing_test_chunks(packed_kind);
        packed_chunks[0] = Some(packed);
        let shards = vec![
            crate::PackageShardInput::new(
                level.pixel_path().clone(),
                vec![0, 0, 0, 0, 0],
                first_pixel_shard,
            ),
            crate::PackageShardInput::new(
                level.pixel_path().clone(),
                vec![0, 0, 0, 0, 1],
                second_pixel_shard,
            ),
            crate::PackageShardInput::new(
                level.packed_index_path().clone(),
                vec![0, 0],
                packed_chunks,
            ),
        ];
        let fixture = TargetFixture::empty("wide-2d-overlap");
        crate::LocalPackageWriter::write_new(
            fixture.path(),
            crate::PackageWriteInput::new(
                crate::ProfileKind::Current,
                profile,
                science,
                display,
                Vec::new(),
                vec![ome],
                arrays,
                shards,
            ),
            || false,
        )
        .unwrap();
        fixture
    }

    fn missing_test_chunks(kind: ShardProfileKind) -> Vec<Option<Vec<u8>>> {
        std::iter::repeat_with(|| None)
            .take(kind.chunks_per_shard())
            .collect()
    }

    struct CancelOnSecondCheckSink {
        inner: TestSink,
        checks: AtomicU64,
    }

    struct CancelAfterBytesSink {
        inner: TestSink,
        cancel_after: u64,
    }

    struct RejectDirectSpanSink {
        inner: TestSink,
    }

    struct MutateAfterFullDirectSink {
        inner: TestSink,
        path: PathBuf,
        mutated: bool,
    }

    struct MutateAfterPhysicalDecodeSink {
        inner: TestSink,
        source: Arc<LocalDatasetSource>,
        path: PathBuf,
        mutate_after_decodes: u64,
        mutated: AtomicBool,
    }

    impl ReservedDecodeSink for RejectDirectSpanSink {
        fn resource_key(&self) -> BrickKey {
            self.inner.resource_key()
        }

        fn payload_descriptor(&self) -> ResourcePayloadDescriptor {
            self.inner.payload_descriptor()
        }

        fn written_bytes(&self) -> u64 {
            self.inner.written_bytes()
        }

        fn is_cancelled(&self) -> bool {
            false
        }

        fn writable_span(&mut self, _maximum_bytes: usize) -> Result<&mut [u8], DecodeSinkError> {
            Err(DecodeSinkError::DirectWriteUnsupported)
        }

        fn commit_written(&mut self, _bytes: usize) -> Result<(), DecodeSinkError> {
            Err(DecodeSinkError::WritableCommitWithoutSpan)
        }

        fn write(&mut self, bytes: &[u8]) -> Result<(), DecodeSinkError> {
            self.inner.write(bytes)
        }

        fn finish(&mut self) -> Result<(), DecodeSinkError> {
            self.inner.finish()
        }

        fn finish_with_facts(
            &mut self,
            facts: ResourcePayloadFacts,
        ) -> Result<(), DecodeSinkError> {
            self.inner.finish_with_facts(facts)
        }

        fn is_finished(&self) -> bool {
            self.inner.is_finished()
        }
    }

    impl ReservedDecodeSink for MutateAfterFullDirectSink {
        fn resource_key(&self) -> BrickKey {
            self.inner.resource_key()
        }

        fn payload_descriptor(&self) -> ResourcePayloadDescriptor {
            self.inner.payload_descriptor()
        }

        fn written_bytes(&self) -> u64 {
            self.inner.written_bytes()
        }

        fn is_cancelled(&self) -> bool {
            false
        }

        fn writable_span(&mut self, maximum_bytes: usize) -> Result<&mut [u8], DecodeSinkError> {
            self.inner.writable_span(maximum_bytes)
        }

        fn commit_written(&mut self, bytes: usize) -> Result<(), DecodeSinkError> {
            self.inner.commit_written(bytes)?;
            if !self.mutated && self.inner.written_bytes() == self.inner.descriptor.byte_len() {
                let mut file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&self.path)
                    .unwrap();
                let mut first = [0_u8; 1];
                file.read_exact(&mut first).unwrap();
                file.seek(SeekFrom::Start(0)).unwrap();
                first[0] ^= 1;
                file.write_all(&first).unwrap();
                file.sync_data().unwrap();
                self.mutated = true;
            }
            Ok(())
        }

        fn write(&mut self, bytes: &[u8]) -> Result<(), DecodeSinkError> {
            self.inner.write(bytes)
        }

        fn finish(&mut self) -> Result<(), DecodeSinkError> {
            self.inner.finish()
        }

        fn finish_with_facts(
            &mut self,
            facts: ResourcePayloadFacts,
        ) -> Result<(), DecodeSinkError> {
            self.inner.finish_with_facts(facts)
        }

        fn is_finished(&self) -> bool {
            self.inner.is_finished()
        }
    }

    impl ReservedDecodeSink for MutateAfterPhysicalDecodeSink {
        fn resource_key(&self) -> BrickKey {
            self.inner.resource_key()
        }

        fn payload_descriptor(&self) -> ResourcePayloadDescriptor {
            self.inner.payload_descriptor()
        }

        fn written_bytes(&self) -> u64 {
            self.inner.written_bytes()
        }

        fn is_cancelled(&self) -> bool {
            if self
                .source
                .counters
                .physical_brick_unique_decodes
                .load(Ordering::Acquire)
                >= self.mutate_after_decodes
                && !self.mutated.swap(true, Ordering::AcqRel)
            {
                let mut file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&self.path)
                    .unwrap();
                let mut first = [0_u8; 1];
                file.read_exact(&mut first).unwrap();
                file.seek(SeekFrom::Start(0)).unwrap();
                first[0] ^= 1;
                file.write_all(&first).unwrap();
                file.sync_data().unwrap();
            }
            false
        }

        fn writable_span(&mut self, maximum_bytes: usize) -> Result<&mut [u8], DecodeSinkError> {
            self.inner.writable_span(maximum_bytes)
        }

        fn commit_written(&mut self, bytes: usize) -> Result<(), DecodeSinkError> {
            self.inner.commit_written(bytes)
        }

        fn write(&mut self, bytes: &[u8]) -> Result<(), DecodeSinkError> {
            self.inner.write(bytes)
        }

        fn finish(&mut self) -> Result<(), DecodeSinkError> {
            self.inner.finish()
        }

        fn finish_with_facts(
            &mut self,
            facts: ResourcePayloadFacts,
        ) -> Result<(), DecodeSinkError> {
            self.inner.finish_with_facts(facts)
        }

        fn is_finished(&self) -> bool {
            self.inner.is_finished()
        }
    }

    impl ReservedDecodeSink for CancelAfterBytesSink {
        fn resource_key(&self) -> BrickKey {
            self.inner.resource_key()
        }

        fn payload_descriptor(&self) -> ResourcePayloadDescriptor {
            self.inner.payload_descriptor()
        }

        fn written_bytes(&self) -> u64 {
            self.inner.written_bytes()
        }

        fn is_cancelled(&self) -> bool {
            self.written_bytes() >= self.cancel_after
        }

        fn writable_span(&mut self, maximum_bytes: usize) -> Result<&mut [u8], DecodeSinkError> {
            if self.is_cancelled() {
                return Err(DecodeSinkError::Cancelled);
            }
            self.inner.writable_span(maximum_bytes)
        }

        fn commit_written(&mut self, bytes: usize) -> Result<(), DecodeSinkError> {
            self.inner.commit_written(bytes)
        }

        fn write(&mut self, bytes: &[u8]) -> Result<(), DecodeSinkError> {
            self.inner.write(bytes)
        }

        fn finish(&mut self) -> Result<(), DecodeSinkError> {
            self.inner.finish()
        }

        fn finish_with_facts(
            &mut self,
            facts: ResourcePayloadFacts,
        ) -> Result<(), DecodeSinkError> {
            self.inner.finish_with_facts(facts)
        }

        fn is_finished(&self) -> bool {
            self.inner.is_finished()
        }
    }

    impl CancelOnSecondCheckSink {
        fn new(key: BrickKey, descriptor: ResourcePayloadDescriptor) -> Self {
            Self {
                inner: TestSink::new(key, descriptor),
                checks: AtomicU64::new(0),
            }
        }
    }

    impl ReservedDecodeSink for CancelOnSecondCheckSink {
        fn resource_key(&self) -> BrickKey {
            self.inner.resource_key()
        }

        fn payload_descriptor(&self) -> ResourcePayloadDescriptor {
            self.inner.payload_descriptor()
        }

        fn written_bytes(&self) -> u64 {
            self.inner.written_bytes()
        }

        fn is_cancelled(&self) -> bool {
            self.checks.fetch_add(1, Ordering::Relaxed) >= 1
        }

        fn writable_span(&mut self, maximum_bytes: usize) -> Result<&mut [u8], DecodeSinkError> {
            if self.is_cancelled() {
                return Err(DecodeSinkError::Cancelled);
            }
            self.inner.writable_span(maximum_bytes)
        }

        fn commit_written(&mut self, bytes: usize) -> Result<(), DecodeSinkError> {
            self.inner.commit_written(bytes)
        }

        fn write(&mut self, bytes: &[u8]) -> Result<(), DecodeSinkError> {
            self.inner.write(bytes)
        }

        fn finish(&mut self) -> Result<(), DecodeSinkError> {
            self.inner.finish()
        }

        fn is_finished(&self) -> bool {
            self.inner.is_finished()
        }
    }

    #[test]
    fn contiguous_validity_ranges_match_scalar_bit_setting() {
        let key = BrickKey::new(
            DatasetResourceIdentity::SessionLocal(DatasetSourceId::new(1)),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 0], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
        );
        for start in 0..64 {
            for count in 0..=64 - start {
                let mut expected = [0b0101_0101; 8];
                for sample in start..start + count {
                    expected[sample / 8] |= 1 << (sample % 8);
                }
                let mut actual = [0b0101_0101; 8];
                set_validity_range(key, &mut actual, start, count).unwrap();
                assert_eq!(actual, expected, "start={start}, count={count}");
            }
        }
    }

    #[test]
    fn provisional_2d_source_decodes_one_region_across_physical_bricks() {
        let fixture = TargetFixture::extract("m4d-t1-u8-2d-sparse");
        let ledger = Arc::new(TestLedger::default());
        let injected: Arc<dyn CpuByteLedger> = ledger.clone();
        let source_id = DatasetSourceId::new(41);
        let storage = LocalPackageCatalog::open(fixture.path()).unwrap();
        let declared_content_id = storage.science().scientific_content_id();
        let source =
            LocalDatasetSource::from_admitted(storage, source_id, "Sparse target", injected)
                .unwrap();

        assert_eq!(source.package_id(), None);
        let catalog = source.catalog().unwrap();
        assert_eq!(catalog.label(), "Sparse target");
        assert_eq!(
            catalog.content_address_status(),
            &ContentAddressStatus::ContentAddress(declared_content_id)
        );
        let region = ResourceRegion::new([0, 256, 255], Shape3D::new(1, 1, 770).unwrap()).unwrap();
        let key = BrickKey::new(
            DatasetResourceIdentity::SessionLocal(source_id),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            region,
        );
        let descriptor = catalog.resource_payload_descriptor(key).unwrap();
        let mut sink = TestSink::new(key, descriptor);
        decode_one(source.as_ref(), &mut sink).unwrap();

        assert!(sink.finished);
        assert_supplied_facts_match_payload(&sink);
        assert_eq!(sink.bytes.len(), 770);
        let nonzero = sink
            .bytes
            .iter()
            .enumerate()
            .filter_map(|(index, value)| (*value != 0).then_some((index, *value)))
            .collect::<Vec<_>>();
        assert_eq!(nonzero, vec![(1, 7), (257, 8), (513, 9), (769, 10)]);
        assert!(ledger.peak(CpuLedgerCategory::InFlightDecode) > descriptor.byte_len());
        assert_eq!(ledger.used(CpuLedgerCategory::InFlightDecode), 0);
        assert!(ledger.used(CpuLedgerCategory::MetadataAndIndexes) > 0);
        drop(source);
        assert_eq!(ledger.used(CpuLedgerCategory::MetadataAndIndexes), 0);
    }

    #[test]
    fn two_dimensional_siblings_share_one_bounded_physical_decode_sequentially_and_concurrently() {
        fn source_and_identity() -> (
            Arc<LocalDatasetSource>,
            Arc<TestLedger>,
            DatasetSourceId,
            TargetFixture,
        ) {
            let fixture = TargetFixture::extract("m4d-t1-u8-2d-sparse");
            let ledger = Arc::new(TestLedger::default());
            let injected: Arc<dyn CpuByteLedger> = ledger.clone();
            let source_id = DatasetSourceId::new(77);
            let source = LocalDatasetSource::from_admitted(
                LocalPackageCatalog::open(fixture.path()).unwrap(),
                source_id,
                "Coalesced target",
                injected,
            )
            .unwrap();
            (source, ledger, source_id, fixture)
        }

        fn quadrant_key(source_id: DatasetSourceId, y: u64, x: u64) -> BrickKey {
            BrickKey::new(
                DatasetResourceIdentity::SessionLocal(source_id),
                LogicalLayerKey::new(0),
                TimeIndex::new(0),
                ScaleLevel::BASE,
                ResourceRegion::new([0, y * 64, x * 64], Shape3D::new(1, 64, 64).unwrap()).unwrap(),
            )
        }

        let (source, ledger, source_id, _fixture) = source_and_identity();
        let catalog = source.catalog().unwrap();
        for y in 0..4 {
            for x in 0..4 {
                let key = quadrant_key(source_id, y, x);
                let descriptor = catalog.resource_payload_descriptor(key).unwrap();
                decode_one(source.as_ref(), &mut TestSink::new(key, descriptor)).unwrap();
            }
        }
        let sequential = source.diagnostics();
        assert_eq!(sequential.physical_brick_requests, 16);
        assert_eq!(sequential.physical_brick_cache_misses, 1);
        assert_eq!(sequential.physical_brick_cache_hits, 15);
        assert_eq!(sequential.physical_brick_unique_decodes, 1);
        assert_eq!(sequential.physical_brick_cache_entries, 1);
        assert!(sequential.physical_brick_cache_bytes > 0);
        assert_eq!(sequential.reader.shard_index_cache_misses, 2);
        assert_eq!(sequential.reader.shard_index_decode_operations, 2);
        assert!(ledger.used(CpuLedgerCategory::DecodedResidency) > 0);

        let (source, _ledger, source_id, _fixture) = source_and_identity();
        let catalog = source.catalog().unwrap();
        let barrier = Arc::new(Barrier::new(16));
        let mut workers = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                let source = Arc::clone(&source);
                let catalog = Arc::clone(&catalog);
                let barrier = Arc::clone(&barrier);
                workers.push(thread::spawn(move || {
                    let key = quadrant_key(source_id, y, x);
                    let descriptor = catalog.resource_payload_descriptor(key).unwrap();
                    barrier.wait();
                    decode_one(source.as_ref(), &mut TestSink::new(key, descriptor)).unwrap();
                }));
            }
        }
        for worker in workers {
            worker.join().unwrap();
        }
        let concurrent = source.diagnostics();
        assert_eq!(concurrent.physical_brick_requests, 16);
        assert_eq!(concurrent.physical_brick_cache_misses, 1);
        assert_eq!(concurrent.physical_brick_unique_decodes, 1);
        assert_eq!(concurrent.physical_brick_cache_hits, 15);
        assert!(concurrent.physical_brick_cache_waits > 0);
    }

    #[test]
    fn unaligned_cohort_fans_out_before_a_ten_brick_working_set_can_thrash_eight_cache_slots() {
        const MEMBERS: usize = 8;

        let fixture = wide_2d_overlap_target();
        let source_id = DatasetSourceId::new(177);
        let source = LocalDatasetSource::from_admitted(
            LocalPackageCatalog::open(fixture.path()).unwrap(),
            source_id,
            "Physical-order cohort target",
            Arc::new(TestLedger::default()),
        )
        .unwrap();
        let catalog = source.catalog().unwrap();
        let mut owned = (0..MEMBERS)
            .map(|x| {
                let key = BrickKey::new(
                    DatasetResourceIdentity::SessionLocal(source_id),
                    LogicalLayerKey::new(0),
                    TimeIndex::new(0),
                    ScaleLevel::BASE,
                    ResourceRegion::new(
                        [0, 0, u64::try_from(x).unwrap()],
                        Shape3D::new(1, 257, 1_025).unwrap(),
                    )
                    .unwrap(),
                );
                let descriptor = catalog.resource_payload_descriptor(key).unwrap();
                TestSink::new(key, descriptor)
            })
            .collect::<Vec<_>>();
        let mut sinks = owned
            .iter_mut()
            .map(|sink| sink as &mut dyn ReservedDecodeSink)
            .collect::<Vec<_>>();
        let outcomes = source.decode_cohort_into(&mut sinks);

        assert!(outcomes.into_iter().all(|outcome| outcome.is_ok()));
        assert!(owned.iter().all(TestSink::is_finished));
        let diagnostics = source.diagnostics();
        assert_eq!(diagnostics.physical_brick_requests, 10);
        assert_eq!(diagnostics.physical_brick_unique_decodes, 10);
        assert_eq!(diagnostics.physical_brick_cache_misses, 10);
        assert_eq!(diagnostics.physical_brick_cache_entries, 8);
        assert_eq!(diagnostics.physical_brick_cache_evictions, 2);
        assert_eq!(diagnostics.reader.currentness_pre_use_batches, 1);
        assert_eq!(diagnostics.reader.currentness_post_use_batches, 1);
    }

    #[test]
    fn production_runtime_sixteen_distinct_keys_decode_each_physical_brick_once() {
        const WORKERS: usize = 8;
        const TARGETS: usize = 16;
        const COMPLETIONS: usize = WORKERS + TARGETS;

        let fixture = wide_2d_overlap_target();
        let source_id = DatasetSourceId::new(179);
        let gate = Arc::new(RuntimeStorageGate::new(WORKERS));
        let source_slot = Arc::new(Mutex::new(None::<Arc<LocalDatasetSource>>));
        let source_path = fixture.path().to_path_buf();
        let factory_gate = Arc::clone(&gate);
        let factory_slot = Arc::clone(&source_slot);
        let config = DatasetRuntimeConfig::new(512 * 1024 * 1024, WORKERS, 64, 64).unwrap();
        let (runtime, catalog) = <dyn DatasetRuntime>::start(config, move |ledger| {
            let package = LocalPackageCatalog::open(&source_path)
                .map_err(|_| RuntimeFault::new(RuntimeFaultCode::InvariantViolation))?;
            let inner = LocalDatasetSource::from_admitted(
                package,
                source_id,
                "Runtime physical-order cohort target",
                ledger,
            )
            .map_err(|_| RuntimeFault::new(RuntimeFaultCode::InvariantViolation))?;
            *factory_slot.lock().unwrap() = Some(Arc::clone(&inner));
            let source: Arc<dyn DatasetSource> = Arc::new(GatedRuntimeStorageSource {
                inner,
                gate: factory_gate,
            });
            Ok(source)
        })
        .unwrap();
        let source = source_slot.lock().unwrap().take().unwrap();

        let mut blocker_tickets = Vec::with_capacity(WORKERS);
        for index in 0..WORKERS {
            let key = BrickKey::new(
                DatasetResourceIdentity::SessionLocal(source_id),
                LogicalLayerKey::new(0),
                TimeIndex::new(0),
                ScaleLevel::BASE,
                ResourceRegion::new(
                    [0, 0, 1_800 + u64::try_from(index).unwrap()],
                    Shape3D::new(1, 1, 1).unwrap(),
                )
                .unwrap(),
            );
            blocker_tickets.push(
                runtime
                    .submit(ResourceRequest::new(
                        key,
                        RequestPriority::Analysis,
                        CancellationGeneration::for_scope(100 + u64::try_from(index).unwrap(), 0),
                    ))
                    .unwrap(),
            );
        }
        gate.wait_for(
            |state| state.blocker_members_entered == WORKERS,
            "lower-priority runtime blockers did not occupy every worker",
        );

        for index in 0..TARGETS {
            let key = BrickKey::new(
                catalog.resource_identity(),
                LogicalLayerKey::new(0),
                TimeIndex::new(0),
                ScaleLevel::BASE,
                ResourceRegion::new(
                    [0, 0, u64::try_from(index).unwrap()],
                    Shape3D::new(1, 257, 1_025).unwrap(),
                )
                .unwrap(),
            );
            runtime
                .submit(ResourceRequest::new(
                    key,
                    RequestPriority::CurrentView,
                    CancellationGeneration::for_scope(1_000 + u64::try_from(index).unwrap(), 0),
                ))
                .unwrap();
        }
        gate.release_blockers();
        gate.wait_for(
            |state| state.target_members_entered == TARGETS,
            "the sixteen-key visible burst did not enter source cohorts",
        );
        let (target_cohort_sizes, peak_active_target_cohorts) = {
            let state = gate.state.lock().unwrap();
            (
                state.target_cohort_sizes.clone(),
                state.peak_active_target_cohorts,
            )
        };
        for ticket in blocker_tickets {
            runtime.cancel(ticket).unwrap();
        }
        let before_targets = source.diagnostics();
        gate.release_targets();
        // Which worker wins each first claim is intentionally unspecified.
        // The contract is a concurrent, bounded, multi-member burst covering
        // all sixteen distinct keys; exact 8x2 partitioning would turn thread
        // timing into a false production requirement.
        assert_eq!(target_cohort_sizes.iter().sum::<usize>(), TARGETS);
        assert_eq!(peak_active_target_cohorts, target_cohort_sizes.len());
        assert!(target_cohort_sizes.len() >= 2);
        assert!(target_cohort_sizes.len() <= WORKERS);
        assert!(
            target_cohort_sizes
                .iter()
                .all(|members| (1..=WORKERS).contains(members))
        );
        assert!(target_cohort_sizes.iter().any(|members| *members > 1));

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut completions = Vec::with_capacity(COMPLETIONS);
        while completions.len() < COMPLETIONS {
            completions.extend(runtime.poll(COMPLETIONS - completions.len()).unwrap());
            assert!(Instant::now() < deadline, "runtime completions timed out");
            if completions.len() < COMPLETIONS {
                thread::sleep(Duration::from_millis(1));
            }
        }
        assert_eq!(
            completions
                .iter()
                .filter(|completion| matches!(completion.outcome(), RuntimeOutcome::Ready(_)))
                .count(),
            TARGETS
        );
        assert_eq!(
            completions
                .iter()
                .filter(|completion| matches!(completion.outcome(), RuntimeOutcome::Cancelled))
                .count(),
            WORKERS
        );

        let after_targets = source.diagnostics();
        assert_eq!(
            after_targets.physical_brick_unique_decodes
                - before_targets.physical_brick_unique_decodes,
            10
        );
        assert_eq!(
            after_targets.physical_brick_cache_misses - before_targets.physical_brick_cache_misses,
            10
        );
        assert!(
            after_targets.physical_brick_cache_entries
                <= u64::try_from(PHYSICAL_BRICK_CACHE_ENTRIES_MAX).unwrap()
        );
        let performance = runtime.diagnostics().unwrap().performance();
        assert_eq!(performance.decode_cohort_members(), COMPLETIONS as u64);
        assert!(performance.peak_decode_cohort_members() >= 2);
    }

    #[test]
    fn unaligned_cohort_postvalidation_rejects_mutation_before_any_sink_publication() {
        let fixture = TargetFixture::extract("m4d-t1-u8-2d-sparse");
        let source_id = DatasetSourceId::new(178);
        let source = LocalDatasetSource::from_admitted(
            LocalPackageCatalog::open(fixture.path()).unwrap(),
            source_id,
            "Late mutation target",
            Arc::new(TestLedger::default()),
        )
        .unwrap();
        let key = BrickKey::new(
            DatasetResourceIdentity::SessionLocal(source_id),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 0], Shape3D::new(1, 64, 64).unwrap()).unwrap(),
        );
        let descriptor = source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(key)
            .unwrap();
        let mut sink = MutateAfterPhysicalDecodeSink {
            inner: TestSink::new(key, descriptor),
            source: Arc::clone(&source),
            path: fixture.path().join("images/i00000000/s00/c/0/0/0/0/0"),
            mutate_after_decodes: 1,
            mutated: AtomicBool::new(false),
        };

        assert_eq!(
            decode_one(source.as_ref(), &mut sink),
            Err(DatasetSourceFault::CorruptResource { key })
        );
        assert!(sink.mutated.load(Ordering::Acquire));
        assert_eq!(sink.written_bytes(), 0);
        assert!(!sink.is_finished());
        let diagnostics = source.diagnostics();
        assert_eq!(diagnostics.reader.currentness_pre_use_batches, 1);
        assert_eq!(diagnostics.reader.currentness_post_use_batches, 1);
        assert_eq!(diagnostics.physical_brick_cache_entries, 0);
    }

    #[test]
    fn physical_loading_cancellation_is_interest_and_generation_scoped() {
        let fixture = TargetFixture::extract("m4d-t1-u8-2d-sparse");
        let ledger: Arc<dyn CpuByteLedger> = Arc::new(TestLedger::default());
        let source = LocalDatasetSource::from_admitted(
            LocalPackageCatalog::open(fixture.path()).unwrap(),
            DatasetSourceId::new(79),
            "Generation target",
            ledger,
        )
        .unwrap();
        let coordinates = PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0);
        let old_cancellation = Arc::new(AtomicBool::new(false));
        let old_ticket = PhysicalLoadingTicket {
            generation: 17,
            cancellation: Arc::clone(&old_cancellation),
        };
        {
            let mut cache = lock_unpoisoned(&source.physical_cache.state);
            cache.entries.push(PhysicalBrickCacheEntry {
                coordinates,
                generation: old_ticket.generation,
                last_touch: 0,
                value: PhysicalBrickCacheValue::Loading {
                    interests: 2,
                    cancellation: Arc::clone(&old_cancellation),
                },
            });
        }

        source.leave_physical_interest(coordinates, &old_ticket);
        assert!(!old_cancellation.load(Ordering::Acquire));
        {
            let cache = lock_unpoisoned(&source.physical_cache.state);
            assert!(matches!(
                cache.entries[0].value,
                PhysicalBrickCacheValue::Loading { interests: 1, .. }
            ));
        }

        source.leave_physical_interest(coordinates, &old_ticket);
        assert!(old_cancellation.load(Ordering::Acquire));
        assert!(
            lock_unpoisoned(&source.physical_cache.state)
                .entries
                .is_empty()
        );

        let new_cancellation = Arc::new(AtomicBool::new(false));
        let new_ticket = PhysicalLoadingTicket {
            generation: 18,
            cancellation: Arc::clone(&new_cancellation),
        };
        {
            let mut cache = lock_unpoisoned(&source.physical_cache.state);
            cache.entries.push(PhysicalBrickCacheEntry {
                coordinates,
                generation: new_ticket.generation,
                last_touch: 1,
                value: PhysicalBrickCacheValue::Loading {
                    interests: 1,
                    cancellation: Arc::clone(&new_cancellation),
                },
            });
        }

        // Cleanup arriving late from the abandoned generation cannot erase or
        // cancel a restarted read at the same physical coordinates.
        source.remove_physical_loading(coordinates, &old_ticket);
        source.leave_physical_interest(coordinates, &old_ticket);
        assert!(!new_cancellation.load(Ordering::Acquire));
        {
            let cache = lock_unpoisoned(&source.physical_cache.state);
            assert_eq!(cache.entries.len(), 1);
            assert_eq!(cache.entries[0].generation, new_ticket.generation);
        }

        source.leave_physical_interest(coordinates, &new_ticket);
        assert!(new_cancellation.load(Ordering::Acquire));
        assert!(
            lock_unpoisoned(&source.physical_cache.state)
                .entries
                .is_empty()
        );
    }

    #[test]
    fn cancellation_during_ready_revalidation_does_not_evict_current_residency() {
        let fixture = TargetFixture::extract("m4d-t1-u8-2d-sparse");
        let ledger = Arc::new(TestLedger::default());
        let injected: Arc<dyn CpuByteLedger> = ledger.clone();
        let source_id = DatasetSourceId::new(81);
        let source = LocalDatasetSource::from_admitted(
            LocalPackageCatalog::open(fixture.path()).unwrap(),
            source_id,
            "Cancellation target",
            injected,
        )
        .unwrap();
        let key = BrickKey::new(
            DatasetResourceIdentity::SessionLocal(source_id),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 0], Shape3D::new(1, 64, 64).unwrap()).unwrap(),
        );
        let descriptor = source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(key)
            .unwrap();
        decode_one(source.as_ref(), &mut TestSink::new(key, descriptor)).unwrap();
        let before = source.diagnostics();
        assert_eq!(before.physical_brick_cache_entries, 1);
        assert!(before.physical_brick_cache_bytes > 0);
        let resident_bytes = ledger.used(CpuLedgerCategory::DecodedResidency);
        assert!(resident_bytes > 0);

        let mapping = source.mapping(key).unwrap();
        let coordinates = PackedIndexCoordinates::new(
            mapping.image,
            key.scale().get(),
            u32::try_from(key.timepoint().get()).unwrap(),
            mapping.physical_channel,
            0,
            0,
            0,
        );
        let error = match source.read_physical_brick(
            &CancelOnSecondCheckSink::new(key, descriptor),
            key,
            coordinates,
            descriptor,
            mapping,
        ) {
            Ok(_) => panic!("cancellation during revalidation must stop this caller"),
            Err(error) => error,
        };
        assert!(matches!(error, DatasetSourceFault::Cancelled { .. }));

        let after = source.diagnostics();
        assert_eq!(after.physical_brick_cache_entries, 1);
        assert_eq!(
            after.physical_brick_cache_bytes,
            before.physical_brick_cache_bytes
        );
        assert_eq!(
            ledger.used(CpuLedgerCategory::DecodedResidency),
            resident_bytes
        );
    }

    #[test]
    fn capacity_bypass_fans_one_decode_out_with_one_shared_accounting_lease() {
        const WORKERS: usize = 8;

        let fixture = TargetFixture::extract("m4d-t1-u8-2d-sparse");
        let ledger = Arc::new(GatedBypassLedger::default());
        let injected: Arc<dyn CpuByteLedger> = ledger.clone();
        let source_id = DatasetSourceId::new(80);
        let source = LocalDatasetSource::from_admitted(
            LocalPackageCatalog::open(fixture.path()).unwrap(),
            source_id,
            "Bypass target",
            injected,
        )
        .unwrap();
        let key = BrickKey::new(
            DatasetResourceIdentity::SessionLocal(source_id),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 0], Shape3D::new(1, 64, 64).unwrap()).unwrap(),
        );
        let descriptor = source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(key)
            .unwrap();
        let mapping = source.mapping(key).unwrap();
        let coordinates = PackedIndexCoordinates::new(
            mapping.image,
            key.scale().get(),
            u32::try_from(key.timepoint().get()).unwrap(),
            mapping.physical_channel,
            0,
            0,
            0,
        );
        let start = Arc::new(Barrier::new(WORKERS + 1));
        let hold_results = Arc::new((Mutex::new(false), Condvar::new()));
        let (ready_tx, ready_rx) = mpsc::channel();
        let mut joins = Vec::new();
        for _ in 0..WORKERS {
            let source = Arc::clone(&source);
            let start = Arc::clone(&start);
            let hold_results = Arc::clone(&hold_results);
            let ready_tx = ready_tx.clone();
            joins.push(thread::spawn(move || {
                let sink = TestSink::new(key, descriptor);
                start.wait();
                let brick = source
                    .read_physical_brick(&sink, key, coordinates, descriptor, mapping)
                    .unwrap();
                ready_tx.send(Arc::as_ptr(&brick.brick) as usize).unwrap();
                let (release, wake) = &*hold_results;
                let mut release = release.lock().unwrap();
                while !*release {
                    release = wake.wait(release).unwrap();
                }
                drop(brick);
            }));
        }
        drop(ready_tx);
        start.wait();
        ledger.physical_decode.wait_until_entered();

        let join_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let interests = {
                let cache = lock_unpoisoned(&source.physical_cache.state);
                cache.entries.iter().find_map(|entry| match entry.value {
                    PhysicalBrickCacheValue::Loading { interests, .. }
                        if entry.coordinates == coordinates =>
                    {
                        Some(interests)
                    }
                    _ => None,
                })
            };
            if interests == Some(WORKERS) {
                break;
            }
            if Instant::now() >= join_deadline {
                ledger.physical_decode.release();
                panic!("not every concurrent reader joined the loading generation");
            }
            thread::yield_now();
        }
        ledger.physical_decode.release();

        let mut brick_pointers = Vec::new();
        for _ in 0..WORKERS {
            brick_pointers.push(
                ready_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("a shared bypass result should arrive"),
            );
        }
        assert!(brick_pointers.windows(2).all(|pair| pair[0] == pair[1]));
        let diagnostics = source.diagnostics();
        assert_eq!(diagnostics.physical_brick_cache_misses, 1);
        assert_eq!(diagnostics.physical_brick_cache_hits, WORKERS as u64 - 1);
        assert_eq!(diagnostics.physical_brick_unique_decodes, 1);
        assert_eq!(diagnostics.physical_brick_cache_capacity_bypasses, 1);
        assert_eq!(diagnostics.physical_brick_cache_entries, 0);
        assert_eq!(diagnostics.physical_brick_cache_bytes, 0);
        assert!(
            lock_unpoisoned(&source.physical_cache.state)
                .entries
                .is_empty()
        );
        let transient_retention = physical_brick_retention_bytes_max(
            key,
            descriptor.dtype(),
            descriptor.validity(),
            mapping.brick_shape[0] == 1,
        )
        .unwrap();
        assert_eq!(
            ledger.used(CpuLedgerCategory::InFlightDecode),
            transient_retention
        );
        assert!(transient_retention < INNER_CODEC_WORKING_BYTES_MAX);
        assert_eq!(ledger.used(CpuLedgerCategory::DecodedResidency), 0);

        {
            let (release, wake) = &*hold_results;
            *release.lock().unwrap() = true;
            wake.notify_all();
        }
        for join in joins {
            join.join().unwrap();
        }
        assert_eq!(ledger.used(CpuLedgerCategory::InFlightDecode), 0);
    }

    #[test]
    fn aligned_three_dimensional_resource_writes_directly_into_the_reserved_sink() {
        let fixture = TargetFixture::extract("m4d-t1-u16-3d-multiscale");
        let ledger = Arc::new(TestLedger::default());
        let injected: Arc<dyn CpuByteLedger> = ledger.clone();
        let source_id = DatasetSourceId::new(78);
        let source = LocalDatasetSource::from_admitted(
            LocalPackageCatalog::open(fixture.path()).unwrap(),
            source_id,
            "Direct target",
            injected,
        )
        .unwrap();
        let key = BrickKey::new(
            DatasetResourceIdentity::SessionLocal(source_id),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 0], Shape3D::new(64, 64, 64).unwrap()).unwrap(),
        );
        let descriptor = source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(key)
            .unwrap();
        assert_eq!(descriptor.byte_len(), 64 * 64 * 64 * 2);
        let mut sink = TestSink::new(key, descriptor);
        decode_one(source.as_ref(), &mut sink).unwrap();
        assert!(sink.is_finished());
        assert_supplied_facts_match_payload(&sink);
        let diagnostics = source.diagnostics();
        assert_eq!(diagnostics.physical_brick_unique_decodes, 1);
        assert_eq!(diagnostics.aligned_direct_deliveries, 1);
        assert_eq!(
            diagnostics.aligned_direct_streamed_bytes,
            descriptor.byte_len()
        );
        assert_eq!(diagnostics.physical_brick_cache_misses, 0);
        assert_eq!(diagnostics.physical_brick_cache_entries, 0);
        assert_eq!(diagnostics.physical_brick_cache_bytes, 0);
        assert_eq!(diagnostics.contiguous_copy_bytes, 0);
        assert_eq!(diagnostics.scalar_copy_samples, 0);
        assert_eq!(diagnostics.sink_write_bytes, descriptor.byte_len());
        assert_eq!(sink.largest_write, 0);
        assert_eq!(sink.write_calls, 0);
        assert_eq!(sink.direct_committed_bytes, descriptor.byte_len());
        assert_eq!(
            diagnostics.aligned_direct_sink_span_bytes,
            descriptor.byte_len()
        );
        assert_eq!(diagnostics.aligned_direct_post_decode_copy_bytes, 0);
        let old_scratch =
            physical_brick_working_bytes(key, descriptor.dtype(), descriptor.validity(), false)
                .unwrap()
                + INNER_CODEC_WORKING_BYTES_MAX;
        let direct_scratch =
            aligned_direct_working_bytes(key, descriptor.dtype(), descriptor.validity(), false)
                .unwrap()
                + INNER_CODEC_WORKING_BYTES_MAX;
        assert_eq!(
            ledger.peak(CpuLedgerCategory::InFlightDecode),
            direct_scratch
        );
        assert_eq!(old_scratch - direct_scratch, 64 * 64 * 64 * 2);
        assert_eq!(ledger.used(CpuLedgerCategory::InFlightDecode), 0);
        assert_eq!(ledger.used(CpuLedgerCategory::DecodedResidency), 0);
    }

    #[test]
    fn aligned_direct_delivery_preserves_self_consistent_authority_batches() {
        let fixture = TargetFixture::extract("m4d-t1-u16-3d-multiscale");
        let self_consistent = LocalPackageCatalog::open(fixture.path())
            .unwrap()
            .validate_exact_supported_package(|| false)
            .unwrap()
            .validate_scientific_content(|| false)
            .unwrap();
        assert_eq!(
            self_consistent
                .validation_report()
                .pyramid_fact_brick_reads(),
            6
        );
        let scientific_id = self_consistent.scientific_content_id();
        let source = LocalDatasetSource::from_published(
            self_consistent,
            "Verified direct target",
            Arc::new(TestLedger::default()),
        )
        .unwrap();
        let key = BrickKey::new(
            DatasetResourceIdentity::ContentAddress(scientific_id),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 0], Shape3D::new(64, 64, 64).unwrap()).unwrap(),
        );
        let descriptor = source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(key)
            .unwrap();
        let mut sink = TestSink::new(key, descriptor);
        decode_one(source.as_ref(), &mut sink).unwrap();
        assert_supplied_facts_match_payload(&sink);
        let diagnostics = source.diagnostics();
        assert_eq!(diagnostics.aligned_direct_deliveries, 1);
        assert_eq!(diagnostics.reader.currentness_pre_use_batches, 1);
        assert_eq!(diagnostics.reader.currentness_post_use_batches, 1);
        assert_eq!(diagnostics.reader.currentness_snapshot_batches, 0);
    }

    #[test]
    fn self_consistent_aligned_cohort_has_constant_exact_currentness_and_one_scratch_lease() {
        const MEMBERS: usize = 8;
        let fixture = TargetFixture::extract("m4d-t1-u16-3d-multiscale");
        let self_consistent = LocalPackageCatalog::open(fixture.path())
            .unwrap()
            .validate_exact_supported_package(|| false)
            .unwrap()
            .validate_scientific_content(|| false)
            .unwrap();
        let scientific_id = self_consistent.scientific_content_id();
        let ledger = Arc::new(TestLedger::default());
        let injected: Arc<dyn CpuByteLedger> = ledger.clone();
        let source =
            LocalDatasetSource::from_published(self_consistent, "Self-consistent cohort", injected)
                .unwrap();
        let key = BrickKey::new(
            DatasetResourceIdentity::ContentAddress(scientific_id),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 0], Shape3D::new(64, 64, 64).unwrap()).unwrap(),
        );
        let descriptor = source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(key)
            .unwrap();
        let mut sinks = (0..MEMBERS)
            .map(|_| TestSink::new(key, descriptor))
            .collect::<Vec<_>>();
        let mut cohort = sinks
            .iter_mut()
            .map(|sink| sink as &mut dyn ReservedDecodeSink)
            .collect::<Vec<_>>();
        assert!(
            source
                .decode_cohort_into(&mut cohort)
                .into_iter()
                .all(|outcome| outcome.is_ok())
        );
        drop(cohort);
        assert!(sinks.iter().all(TestSink::is_finished));
        assert!(
            sinks
                .iter()
                .all(|sink| sink.direct_committed_bytes == descriptor.byte_len())
        );

        let diagnostics = source.diagnostics();
        assert_eq!(diagnostics.aligned_direct_deliveries, MEMBERS as u64);
        assert_eq!(diagnostics.reader.currentness_pre_use_batches, 1);
        assert_eq!(diagnostics.reader.currentness_post_use_batches, 1);
        assert_eq!(diagnostics.reader.currentness_snapshot_batches, 0);
        assert_eq!(diagnostics.reader.currentness_root_metadata_checks, 2);
        // One manifest root, one manifest page, one packed-index shard, and
        // one pixel shard, each checked once before and once after the cohort.
        assert_eq!(diagnostics.reader.currentness_named_object_resolutions, 8);
        assert_eq!(diagnostics.reader.currentness_object_fd_metadata_checks, 8);
        assert_eq!(diagnostics.aligned_direct_post_decode_copy_bytes, 0);
        assert_eq!(
            diagnostics.aligned_direct_sink_span_bytes,
            descriptor.byte_len() * MEMBERS as u64
        );
        let one_member_scratch =
            aligned_direct_working_bytes(key, descriptor.dtype(), descriptor.validity(), false)
                .unwrap()
                + INNER_CODEC_WORKING_BYTES_MAX;
        assert_eq!(
            ledger.peak(CpuLedgerCategory::InFlightDecode),
            one_member_scratch
        );
        assert_eq!(ledger.used(CpuLedgerCategory::InFlightDecode), 0);
    }

    #[test]
    fn aligned_cohort_keeps_cancellation_and_sink_errors_member_local() {
        let fixture = TargetFixture::extract("m4d-t1-u16-3d-multiscale");
        let source_id = DatasetSourceId::new(181);
        let source = LocalDatasetSource::from_admitted(
            LocalPackageCatalog::open(fixture.path()).unwrap(),
            source_id,
            "Independent cohort members",
            Arc::new(TestLedger::default()),
        )
        .unwrap();
        let key = BrickKey::new(
            DatasetResourceIdentity::SessionLocal(source_id),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 0], Shape3D::new(64, 64, 64).unwrap()).unwrap(),
        );
        let descriptor = source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(key)
            .unwrap();
        let mut cancelled = CancelAfterBytesSink {
            inner: TestSink::new(key, descriptor),
            cancel_after: 0,
        };
        let mut rejected = RejectDirectSpanSink {
            inner: TestSink::new(key, descriptor),
        };
        let mut healthy = TestSink::new(key, descriptor);
        let mut cohort: [&mut dyn ReservedDecodeSink; 3] =
            [&mut cancelled, &mut rejected, &mut healthy];
        let outcomes = source.decode_cohort_into(&mut cohort);
        assert_eq!(outcomes[0], Err(DatasetSourceFault::Cancelled { key }));
        assert!(matches!(
            &outcomes[1],
            Err(DatasetSourceFault::SinkRejected { key: actual, .. }) if *actual == key
        ));
        assert_eq!(outcomes[2], Ok(()));
        assert!(!cancelled.inner.is_finished());
        assert!(!rejected.inner.is_finished());
        assert!(healthy.is_finished());
    }

    #[test]
    fn cohort_postvalidation_mutation_fails_every_unfinished_success() {
        let fixture = TargetFixture::extract("m4d-t1-u16-3d-multiscale");
        let self_consistent = LocalPackageCatalog::open(fixture.path())
            .unwrap()
            .validate_exact_supported_package(|| false)
            .unwrap()
            .validate_scientific_content(|| false)
            .unwrap();
        let scientific_id = self_consistent.scientific_content_id();
        let source = LocalDatasetSource::from_published(
            self_consistent,
            "Mutation cohort",
            Arc::new(TestLedger::default()),
        )
        .unwrap();
        let key = BrickKey::new(
            DatasetResourceIdentity::ContentAddress(scientific_id),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 0], Shape3D::new(64, 64, 64).unwrap()).unwrap(),
        );
        let descriptor = source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(key)
            .unwrap();
        let mut first = TestSink::new(key, descriptor);
        let mut mutating = MutateAfterFullDirectSink {
            inner: TestSink::new(key, descriptor),
            path: fixture.path().join("images/i00000000/s00/c/0/0/0/0/0"),
            mutated: false,
        };
        let mut cohort: [&mut dyn ReservedDecodeSink; 2] = [&mut first, &mut mutating];
        let outcomes = source.decode_cohort_into(&mut cohort);
        assert!(outcomes
            .iter()
            .all(|outcome| matches!(outcome, Err(DatasetSourceFault::CorruptResource { key: actual }) if *actual == key)));
        assert!(mutating.mutated);
        assert_eq!(first.written_bytes(), descriptor.byte_len());
        assert_eq!(mutating.written_bytes(), descriptor.byte_len());
        assert!(!first.is_finished());
        assert!(!mutating.is_finished());
        let diagnostics = source.diagnostics();
        assert_eq!(diagnostics.reader.currentness_pre_use_batches, 1);
        assert_eq!(diagnostics.reader.currentness_post_use_batches, 1);
    }

    #[test]
    fn scientific_capability_rejects_lied_s1_facts_before_published_source_admission() {
        let fixture = coarse_scale_lied_statistics_target();
        let exact = LocalPackageCatalog::open(fixture.path())
            .unwrap()
            .validate_exact_supported_package(|| false)
            .expect("the adversarial bytes and their manifest are exact");
        assert!(matches!(
            exact.validate_scientific_content(|| false),
            Err(crate::ScientificPackageValidationError::Read(
                PackageReadError::PackedStatisticsMismatch
            ))
        ));
    }

    /// Release-only structural surrogate for 1,024 distinct aligned 3D
    /// bricks. The repository fixture has one complete 64-cubed brick, so the
    /// test repeats that normal source delivery to exercise the same secure
    /// currentness, range-read, streaming decode, sink-copy, and lock work per
    /// brick without checking in a 512-MiB package.
    #[test]
    #[ignore = "release-only 1,024-brick storage-scale diagnostic"]
    fn aligned_self_consistent_1024_brick_storage_scale_diagnostic() {
        const BRICKS: u64 = 1_024;

        let fixture = TargetFixture::extract("m4d-t1-u16-3d-multiscale");
        let self_consistent = LocalPackageCatalog::open(fixture.path())
            .unwrap()
            .validate_exact_supported_package(|| false)
            .unwrap()
            .validate_scientific_content(|| false)
            .unwrap();
        let scientific_id = self_consistent.scientific_content_id();
        let source = LocalDatasetSource::from_published(
            self_consistent,
            "Verified 1,024-brick diagnostic",
            Arc::new(TestLedger::default()),
        )
        .unwrap();
        let key = BrickKey::new(
            DatasetResourceIdentity::ContentAddress(scientific_id),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 0], Shape3D::new(64, 64, 64).unwrap()).unwrap(),
        );
        let descriptor = source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(key)
            .unwrap();

        let started = Instant::now();
        for _ in 0..BRICKS {
            let mut sink = TestSink::new(key, descriptor);
            decode_one(source.as_ref(), &mut sink).unwrap();
            assert!(sink.is_finished());
        }
        let wall_time_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let diagnostics = source.diagnostics();
        let delivered_bytes = descriptor.byte_len().checked_mul(BRICKS).unwrap();
        let currentness_syscalls = diagnostics
            .reader
            .currentness_root_metadata_checks
            .saturating_add(diagnostics.reader.currentness_named_object_resolutions)
            .saturating_add(diagnostics.reader.currentness_object_fd_metadata_checks);

        assert_eq!(diagnostics.physical_brick_requests, BRICKS);
        assert_eq!(diagnostics.physical_brick_unique_decodes, BRICKS);
        assert_eq!(diagnostics.aligned_direct_deliveries, BRICKS);
        assert_eq!(diagnostics.aligned_direct_streamed_bytes, delivered_bytes);
        assert_eq!(diagnostics.sink_write_bytes, delivered_bytes);
        assert_eq!(diagnostics.reader.currentness_pre_use_batches, BRICKS);
        assert_eq!(diagnostics.reader.currentness_post_use_batches, BRICKS);
        assert_eq!(diagnostics.reader.currentness_snapshot_batches, 0);
        assert_eq!(
            diagnostics.reader.currentness_root_metadata_checks,
            BRICKS * 2
        );
        assert_eq!(
            diagnostics.reader.currentness_named_object_resolutions,
            BRICKS * 8
        );
        assert_eq!(
            diagnostics.reader.currentness_object_fd_metadata_checks,
            BRICKS * 8
        );
        assert_eq!(currentness_syscalls, BRICKS * 18);
        assert_eq!(diagnostics.contiguous_copy_bytes, 0);
        assert_eq!(diagnostics.scalar_copy_samples, 0);
        assert!(diagnostics.reader.currentness_time_ns > 0);
        assert!(diagnostics.reader.physical_range_read_time_ns > 0);
        assert!(diagnostics.reader.codec_decode_time_ns > 0);

        eprintln!(
            "storage-scale-1024 wall_time_ns={wall_time_ns} delivered_bytes={delivered_bytes} \
             currentness_syscalls={currentness_syscalls} currentness_time_ns={} \
             object_opens={} object_open_time_ns={} range_reads={} encoded_bytes={} \
             range_read_time_ns={} codec_decodes={} codec_decoded_bytes={} \
             codec_decode_time_ns={} sink_write_bytes={} cache_lock_acquisitions={} \
             cache_lock_contentions={} cache_lock_wait_time_ns={}",
            diagnostics.reader.currentness_time_ns,
            diagnostics.reader.object_open_operations,
            diagnostics.reader.object_open_time_ns,
            diagnostics.reader.physical_range_read_operations,
            diagnostics.reader.physical_encoded_bytes_read,
            diagnostics.reader.physical_range_read_time_ns,
            diagnostics.reader.codec_decode_operations,
            diagnostics.reader.codec_decoded_bytes,
            diagnostics.reader.codec_decode_time_ns,
            diagnostics.sink_write_bytes,
            diagnostics.reader.object_handle_cache_lock_acquisitions,
            diagnostics.reader.object_handle_cache_lock_contentions,
            diagnostics.reader.object_handle_cache_lock_wait_time_ns,
        );
    }

    /// Eight-consumer companion to the sequential scale diagnostic. This is
    /// deliberately the same normal direct-delivery source and does not add a
    /// benchmark-only cache, batching path, or scheduler.
    #[test]
    #[ignore = "release-only 8-consumer storage-scale diagnostic"]
    fn aligned_self_consistent_1024_brick_eight_consumer_storage_scale_diagnostic() {
        const CONSUMERS: u64 = 8;
        const BRICKS_PER_CONSUMER: u64 = 128;
        const BRICKS: u64 = CONSUMERS * BRICKS_PER_CONSUMER;

        let fixture = TargetFixture::extract("m4d-t1-u16-3d-multiscale");
        let self_consistent = LocalPackageCatalog::open(fixture.path())
            .unwrap()
            .validate_exact_supported_package(|| false)
            .unwrap()
            .validate_scientific_content(|| false)
            .unwrap();
        let scientific_id = self_consistent.scientific_content_id();
        let ledger = Arc::new(TestLedger::with_capacity(128 * 1024 * 1024));
        let injected: Arc<dyn CpuByteLedger> = ledger.clone();
        let source = LocalDatasetSource::from_published(
            self_consistent,
            "Verified 8-consumer diagnostic",
            injected,
        )
        .unwrap();
        let key = BrickKey::new(
            DatasetResourceIdentity::ContentAddress(scientific_id),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 0], Shape3D::new(64, 64, 64).unwrap()).unwrap(),
        );
        let descriptor = source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(key)
            .unwrap();
        let ready = Arc::new(Barrier::new(usize::try_from(CONSUMERS + 1).unwrap()));
        let mut joins = Vec::new();
        for _ in 0..CONSUMERS {
            let source = Arc::clone(&source);
            let ready = Arc::clone(&ready);
            joins.push(thread::spawn(move || {
                ready.wait();
                for _ in 0..BRICKS_PER_CONSUMER {
                    let mut sink = TestSink::new(key, descriptor);
                    decode_one(source.as_ref(), &mut sink).unwrap();
                    assert!(sink.is_finished());
                }
            }));
        }

        let started = Instant::now();
        ready.wait();
        for join in joins {
            join.join().unwrap();
        }
        let wall_time_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let diagnostics = source.diagnostics();
        let delivered_bytes = descriptor.byte_len().checked_mul(BRICKS).unwrap();
        let currentness_syscalls = diagnostics
            .reader
            .currentness_root_metadata_checks
            .saturating_add(diagnostics.reader.currentness_named_object_resolutions)
            .saturating_add(diagnostics.reader.currentness_object_fd_metadata_checks);
        let direct_scratch =
            aligned_direct_working_bytes(key, descriptor.dtype(), descriptor.validity(), false)
                .unwrap()
                + INNER_CODEC_WORKING_BYTES_MAX;
        let caller_reserved_sink_bytes = descriptor.byte_len() * CONSUMERS;
        let combined_accounted_peak_bytes = ledger
            .peak(CpuLedgerCategory::InFlightDecode)
            .saturating_add(ledger.peak(CpuLedgerCategory::MetadataAndIndexes))
            .saturating_add(ledger.peak(CpuLedgerCategory::DecodedResidency))
            .saturating_add(caller_reserved_sink_bytes);

        assert_eq!(diagnostics.physical_brick_requests, BRICKS);
        assert_eq!(diagnostics.physical_brick_unique_decodes, BRICKS);
        assert_eq!(diagnostics.aligned_direct_deliveries, BRICKS);
        assert_eq!(diagnostics.aligned_direct_streamed_bytes, delivered_bytes);
        assert_eq!(diagnostics.sink_write_bytes, delivered_bytes);
        assert_eq!(diagnostics.reader.currentness_pre_use_batches, BRICKS);
        assert_eq!(diagnostics.reader.currentness_post_use_batches, BRICKS);
        assert_eq!(diagnostics.reader.currentness_snapshot_batches, 0);
        assert_eq!(
            diagnostics.reader.currentness_root_metadata_checks,
            BRICKS * 2
        );
        assert_eq!(
            diagnostics.reader.currentness_named_object_resolutions,
            BRICKS * 8
        );
        assert_eq!(
            diagnostics.reader.currentness_object_fd_metadata_checks,
            BRICKS * 8
        );
        assert_eq!(currentness_syscalls, BRICKS * 18);
        assert_eq!(diagnostics.contiguous_copy_bytes, 0);
        assert_eq!(diagnostics.scalar_copy_samples, 0);
        assert_eq!(ledger.used(CpuLedgerCategory::InFlightDecode), 0);
        assert!(ledger.peak(CpuLedgerCategory::InFlightDecode) >= direct_scratch);
        assert!(ledger.peak(CpuLedgerCategory::InFlightDecode) <= direct_scratch * CONSUMERS);

        eprintln!(
            "storage-scale-1024-8c wall_time_ns={wall_time_ns} delivered_bytes={delivered_bytes} \
             currentness_syscalls={currentness_syscalls} currentness_time_ns={} \
             object_opens={} object_cache_misses={} range_reads={} encoded_bytes={} \
             range_read_time_ns={} shard_index_decodes={} codec_decodes={} \
             codec_decoded_bytes={} codec_decode_time_ns={} sink_write_bytes={} \
             cache_lock_acquisitions={} cache_lock_contentions={} \
             cache_lock_wait_time_ns={} peak_in_flight_decode_bytes={} \
             peak_metadata_index_bytes={} peak_decoded_residency_bytes={} \
             caller_reserved_sink_bytes={caller_reserved_sink_bytes} \
             combined_accounted_peak_bytes={combined_accounted_peak_bytes}",
            diagnostics.reader.currentness_time_ns,
            diagnostics.reader.object_open_operations,
            diagnostics.reader.object_handle_cache_misses,
            diagnostics.reader.physical_range_read_operations,
            diagnostics.reader.physical_encoded_bytes_read,
            diagnostics.reader.physical_range_read_time_ns,
            diagnostics.reader.shard_index_decode_operations,
            diagnostics.reader.codec_decode_operations,
            diagnostics.reader.codec_decoded_bytes,
            diagnostics.reader.codec_decode_time_ns,
            diagnostics.sink_write_bytes,
            diagnostics.reader.object_handle_cache_lock_acquisitions,
            diagnostics.reader.object_handle_cache_lock_contentions,
            diagnostics.reader.object_handle_cache_lock_wait_time_ns,
            ledger.peak(CpuLedgerCategory::InFlightDecode),
            ledger.peak(CpuLedgerCategory::MetadataAndIndexes),
            ledger.peak(CpuLedgerCategory::DecodedResidency),
        );
    }

    #[test]
    fn aligned_direct_delivery_cancels_between_bounded_decode_chunks() {
        let fixture = TargetFixture::extract("m4d-t1-u16-3d-multiscale");
        let source_id = DatasetSourceId::new(181);
        let ledger = Arc::new(TestLedger::default());
        let source = LocalDatasetSource::from_admitted(
            LocalPackageCatalog::open(fixture.path()).unwrap(),
            source_id,
            "Cancelled direct target",
            ledger.clone(),
        )
        .unwrap();
        let key = BrickKey::new(
            DatasetResourceIdentity::SessionLocal(source_id),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 0], Shape3D::new(64, 64, 64).unwrap()).unwrap(),
        );
        let descriptor = source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(key)
            .unwrap();
        let mut sink = CancelAfterBytesSink {
            inner: TestSink::new(key, descriptor),
            cancel_after: 64 * 1024,
        };
        assert_eq!(
            decode_one(source.as_ref(), &mut sink),
            Err(DatasetSourceFault::Cancelled { key })
        );
        assert_eq!(sink.written_bytes(), 64 * 1024);
        assert!(!sink.is_finished());
        assert_eq!(source.diagnostics().aligned_direct_deliveries, 0);
        assert_eq!(ledger.used(CpuLedgerCategory::DecodedResidency), 0);
        assert_eq!(ledger.used(CpuLedgerCategory::InFlightDecode), 0);
    }

    fn assert_aligned_nan_source_rejected(valid_nan: bool) {
        let fixture = aligned_nan_target(valid_nan);
        let source_id = DatasetSourceId::new(if valid_nan { 179 } else { 180 });
        let source = LocalDatasetSource::from_admitted(
            LocalPackageCatalog::open(fixture.path()).unwrap(),
            source_id,
            "Non-finite aligned target",
            Arc::new(TestLedger::default()),
        )
        .unwrap();
        let key = BrickKey::new(
            DatasetResourceIdentity::SessionLocal(source_id),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 0], Shape3D::new(64, 64, 64).unwrap()).unwrap(),
        );
        let descriptor = source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(key)
            .unwrap();
        let mut sink = TestSink::new(key, descriptor);
        assert_eq!(
            decode_one(source.as_ref(), &mut sink),
            Err(DatasetSourceFault::CorruptResource { key })
        );
        assert_eq!(sink.written_bytes(), 0);
        assert!(sink.written_bytes() < descriptor.byte_len());
        assert!(!sink.is_finished());
        assert_eq!(source.diagnostics().physical_brick_unique_decodes, 0);
    }

    #[test]
    fn aligned_source_rejects_nan_in_a_valid_float_sample() {
        assert_aligned_nan_source_rejected(true);
    }

    #[test]
    fn aligned_source_rejects_nan_in_an_invalid_float_sample() {
        assert_aligned_nan_source_rejected(false);
    }

    #[test]
    fn self_consistent_f32_source_uses_proved_identity_and_compact_validity() {
        let fixture = TargetFixture::extract("m4d-t1-f32-3d-validity");
        let self_consistent = LocalPackageCatalog::open(fixture.path())
            .unwrap()
            .validate_exact_supported_package(|| false)
            .unwrap()
            .validate_scientific_content(|| false)
            .unwrap();
        let expected_package_id = self_consistent.package_id();
        let expected_scientific_id = self_consistent.scientific_content_id();
        let ledger = Arc::new(TestLedger::default());
        let injected: Arc<dyn CpuByteLedger> = ledger.clone();
        let source =
            LocalDatasetSource::from_published(self_consistent, "Finite f32 target", injected)
                .unwrap();

        assert_eq!(source.package_id(), Some(expected_package_id));
        let mut reader_diagnostics = source.diagnostics().reader;
        assert!(reader_diagnostics.open_object_handles_current >= 1);
        assert!(
            reader_diagnostics.open_object_handles_peak
                >= reader_diagnostics.open_object_handles_current
        );
        assert_eq!(
            reader_diagnostics.object_handle_cache_entries,
            reader_diagnostics
                .open_object_handles_current
                .saturating_sub(1)
        );
        reader_diagnostics.open_object_handles_current = 0;
        reader_diagnostics.open_object_handles_peak = 0;
        reader_diagnostics.object_handle_cache_entries = 0;
        reader_diagnostics.object_handle_cache_peak_entries = 0;
        assert_eq!(reader_diagnostics, LocalPackageReadDiagnostics::default());
        let catalog = source.catalog().unwrap();
        assert_eq!(
            catalog.content_address_status(),
            &ContentAddressStatus::ContentAddress(expected_scientific_id)
        );
        let region = ResourceRegion::new([0, 0, 0], Shape3D::new(1, 1, 16).unwrap()).unwrap();
        let key = BrickKey::new(
            DatasetResourceIdentity::ContentAddress(expected_scientific_id),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            region,
        );
        let descriptor = catalog.resource_payload_descriptor(key).unwrap();
        assert_eq!(descriptor.validity(), ResourceValidity::BitMask);
        let mut sink = TestSink::new(key, descriptor);
        decode_one(source.as_ref(), &mut sink).unwrap();

        let values = sink.bytes[..64]
            .chunks_exact(4)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            vec![
                0x0000_0000,
                0x8000_0000,
                0x0000_0001,
                0x8000_0001,
                0x007f_ffff,
                0x807f_ffff,
                0x0080_0000,
                0x8080_0000,
                0x3f7f_ffff,
                0x3f80_0000,
                0x3f80_0001,
                0x0000_0000,
                0xbf80_0000,
                0xbf80_0001,
                0x7f7f_ffff,
                0xff7f_ffff,
            ]
        );
        assert_eq!(&sink.bytes[64..], &[0b1111_1110, 0b1111_0111]);
        assert!(values.iter().all(|bits| f32::from_bits(*bits).is_finite()));
        assert_eq!(ledger.used(CpuLedgerCategory::InFlightDecode), 0);
    }

    #[test]
    fn self_consistent_explicit_validity_decodes_pixels_and_mask_into_final_spans() {
        let fixture = aligned_finite_explicit_validity_target();
        let self_consistent = LocalPackageCatalog::open(fixture.path())
            .unwrap()
            .validate_exact_supported_package(|| false)
            .unwrap()
            .validate_scientific_content(|| false)
            .unwrap();
        let scientific_id = self_consistent.scientific_content_id();
        let source = LocalDatasetSource::from_published(
            self_consistent,
            "Direct validity target",
            Arc::new(TestLedger::default()),
        )
        .unwrap();
        let key = BrickKey::new(
            DatasetResourceIdentity::ContentAddress(scientific_id),
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 0], Shape3D::new(64, 64, 64).unwrap()).unwrap(),
        );
        let descriptor = source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(key)
            .unwrap();
        assert_eq!(descriptor.validity(), ResourceValidity::BitMask);
        let mut sink = TestSink::new(key, descriptor);
        decode_one(source.as_ref(), &mut sink).unwrap();
        assert!(sink.is_finished());
        assert_eq!(sink.write_calls, 0);
        assert_eq!(sink.direct_committed_bytes, descriptor.byte_len());
        let diagnostics = source.diagnostics();
        assert_eq!(diagnostics.aligned_direct_deliveries, 1);
        assert_eq!(
            diagnostics.aligned_direct_sink_span_bytes,
            descriptor.byte_len()
        );
        assert_eq!(diagnostics.aligned_direct_post_decode_copy_bytes, 0);
    }

    struct TargetFixture(PathBuf);

    impl TargetFixture {
        fn empty(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            assert!(
                label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            );
            Self(std::env::temp_dir().join(format!(
                "mirante4d-dataset-source-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )))
        }

        fn extract(case: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            assert!(
                case.bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            );
            let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(Path::parent)
                .unwrap();
            let archive = fs::read(
                repository
                    .join("fixtures/target/archives")
                    .join(format!("{case}.tar")),
            )
            .unwrap();
            let path = std::env::temp_dir().join(format!(
                "mirante4d-dataset-source-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            extract_ustar(&archive, &path);
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TargetFixture {
        fn drop(&mut self) {
            if self.0.exists() {
                fs::remove_dir_all(&self.0).unwrap();
            }
        }
    }

    fn extract_ustar(archive: &[u8], root: &Path) {
        let mut offset = 0;
        while offset + TAR_BLOCK_BYTES <= archive.len() {
            let header = &archive[offset..offset + TAR_BLOCK_BYTES];
            if header.iter().all(|byte| *byte == 0) {
                break;
            }
            assert_eq!(&header[257..263], b"ustar\0");
            let name = tar_text(&header[..100]);
            let prefix = tar_text(&header[345..500]);
            let relative = if prefix.is_empty() {
                PathBuf::from(name)
            } else {
                PathBuf::from(prefix).join(name)
            };
            assert!(!relative.as_os_str().is_empty());
            assert!(
                relative
                    .components()
                    .all(|component| { matches!(component, Component::Normal(_)) })
            );
            let size = tar_octal(&header[124..136]);
            let body_start = offset + TAR_BLOCK_BYTES;
            let body_end = body_start.checked_add(size).unwrap();
            assert!(body_end <= archive.len());
            let destination = root.join(&relative);
            match header[156] {
                b'5' => fs::create_dir(&destination).unwrap(),
                0 | b'0' => {
                    fs::create_dir_all(destination.parent().unwrap()).unwrap();
                    let mut file = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&destination)
                        .unwrap();
                    file.write_all(&archive[body_start..body_end]).unwrap();
                }
                kind => panic!("unsupported fixture archive entry type {kind}"),
            }
            offset = body_start + size.div_ceil(TAR_BLOCK_BYTES) * TAR_BLOCK_BYTES;
        }
    }

    fn tar_text(bytes: &[u8]) -> &str {
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        std::str::from_utf8(&bytes[..end]).unwrap()
    }

    fn tar_octal(bytes: &[u8]) -> usize {
        let text = tar_text(bytes).trim();
        usize::from_str_radix(text, 8).unwrap()
    }
}
