use std::collections::BTreeMap;

use mirante4d_identity::{ExactBytesDigest, IdentityHashError, PackageId};
use thiserror::Error;

use crate::package_read::{
    DirectPayloadFactsAuthority, LocalBrickRead, LocalDirectBrickRead, LocalDirectBrickReadError,
    PackageReadError, read_local_brick_reusing, read_local_brick_reusing_for_scientific_scan,
    read_local_brick_reusing_in_transaction, read_local_brick_reusing_into_sink_in_transaction,
};
use crate::package_structure::{PackageStructureError, PackageStructureReport};
use crate::range_io::{LocalCurrentnessBatch, LocalObjectHashError, LocalObjectSnapshot};
use crate::{
    DatasetProfileAdmission, DirectoryInventoryError, LocalPackageCatalog, LocalPackageReader,
    ManifestRoot, PackageObjectDescriptor, PackagePath, PackedIndexCoordinates, RangeReadError,
};

/// A local package snapshot whose complete manifest closure passed exact
/// validation.
///
/// The capability owns the catalog that was validated, so it cannot be paired
/// with another package root. It is deliberately not `Clone`. PackageId-
/// attributed brick reads revalidate the manifest authority and compare every
/// shard actually used by the read with the snapshot captured during full
/// validation.
#[derive(Debug)]
pub struct ExactPackageCapability {
    catalog: LocalPackageCatalog,
    admission: DatasetProfileAdmission,
    proof: PackageIntegrityProof,
}

/// Truthful cumulative work emitted after each encoded object has been
/// completely hashed and matched against the package manifest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExactPackageValidationProgress {
    objects_hashed: u64,
    bytes_hashed: u64,
}

impl ExactPackageValidationProgress {
    pub const fn objects_hashed(self) -> u64 {
        self.objects_hashed
    }

    pub const fn bytes_hashed(self) -> u64 {
        self.bytes_hashed
    }
}

impl ExactPackageCapability {
    pub(crate) const fn new(
        catalog: LocalPackageCatalog,
        admission: DatasetProfileAdmission,
        proof: PackageIntegrityProof,
    ) -> Self {
        Self {
            catalog,
            admission,
            proof,
        }
    }

    /// Returns the PackageId proved by the complete exact-byte closure.
    pub const fn package_id(&self) -> PackageId {
        self.proof.package_id
    }

    pub const fn admission(&self) -> DatasetProfileAdmission {
        self.admission
    }

    pub const fn catalog(&self) -> &LocalPackageCatalog {
        &self.catalog
    }

    pub(crate) const fn catalog_mut(&mut self) -> &mut LocalPackageCatalog {
        &mut self.catalog
    }

    pub const fn objects_hashed(&self) -> u64 {
        self.proof.objects_hashed
    }

    pub const fn bytes_hashed(&self) -> u64 {
        self.proof.bytes_hashed
    }

    /// Sequentially revalidates every finalized package-object snapshot.
    ///
    /// This is an explicit O(object-count) freshness check. Normal brick reads
    /// avoid that package-wide cost and instead check manifest authority plus
    /// every shard actually consumed by that read. This does not turn a
    /// mutable directory into an atomic filesystem snapshot.
    pub fn revalidate_complete(
        &self,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), PackageValidationError> {
        self.proof
            .revalidate_all(self.catalog.reader(), &mut is_cancelled)
    }

    /// Reads one brick whose returned bytes are attributable to `package_id()`.
    pub fn read_brick(
        &self,
        coordinates: PackedIndexCoordinates,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<LocalBrickRead, PackageReadError> {
        if is_cancelled() {
            return Err(PackageReadError::Cancelled);
        }

        let plan = self.catalog.plan_brick_storage(coordinates)?;
        let read =
            read_local_brick_reusing(self.catalog.reader(), self.catalog.descriptors(), plan)?;
        self.validate_cached_brick_snapshots(&read, &mut is_cancelled)?;
        self.proof
            .revalidate_authority_cached(self.catalog.reader(), &mut is_cancelled)
            .map_err(map_snapshot_read_error)?;
        Ok(read)
    }

    pub(crate) fn begin_runtime_read_cohort(
        &self,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<LocalCurrentnessBatch<'_>, PackageReadError> {
        let mut transaction = self
            .catalog
            .reader()
            .begin_cached_read_transaction_with_capacity(self.runtime_read_object_capacity())?;
        for snapshot in &self.proof.authority_snapshots {
            if is_cancelled() {
                return Err(PackageReadError::Cancelled);
            }
            transaction.validate_snapshot(snapshot)?;
        }
        Ok(transaction)
    }

    pub(crate) fn runtime_read_object_capacity(&self) -> usize {
        self.catalog.runtime_read_object_capacity()
    }

    pub(crate) fn read_brick_into_sink_in_cohort(
        &self,
        coordinates: PackedIndexCoordinates,
        sink: &mut dyn mirante4d_dataset::ReservedDecodeSink,
        transaction: &mut LocalCurrentnessBatch<'_>,
        facts_authority: DirectPayloadFactsAuthority,
    ) -> Result<LocalDirectBrickRead, LocalDirectBrickReadError> {
        if sink.is_cancelled() {
            return Err(LocalDirectBrickReadError::Package(
                PackageReadError::Cancelled,
            ));
        }
        let plan = self.catalog.plan_brick_storage(coordinates)?;
        let read = read_local_brick_reusing_into_sink_in_transaction(
            self.catalog.reader(),
            self.catalog.descriptors(),
            plan,
            sink,
            transaction,
            facts_authority,
        )?;
        self.validate_cached_snapshots(read.object_snapshots(), &mut || sink.is_cancelled())
            .map_err(LocalDirectBrickReadError::Package)?;
        Ok(read)
    }

    pub(crate) fn read_brick_in_cohort(
        &self,
        coordinates: PackedIndexCoordinates,
        transaction: &mut LocalCurrentnessBatch<'_>,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<LocalBrickRead, PackageReadError> {
        if is_cancelled() {
            return Err(PackageReadError::Cancelled);
        }
        let plan = self.catalog.plan_brick_storage(coordinates)?;
        let read = read_local_brick_reusing_in_transaction(
            self.catalog.reader(),
            self.catalog.descriptors(),
            plan,
            transaction,
            DirectPayloadFactsAuthority::PublishedPackedRecord,
        )?;
        self.validate_cached_brick_snapshots(&read, &mut is_cancelled)?;
        Ok(read)
    }

    pub(crate) fn validate_cached_brick_in_cohort(
        &self,
        read: &LocalBrickRead,
        transaction: &mut LocalCurrentnessBatch<'_>,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), PackageReadError> {
        self.validate_cached_brick_snapshots(read, &mut is_cancelled)?;
        for snapshot in read.object_snapshots() {
            if is_cancelled() {
                return Err(PackageReadError::Cancelled);
            }
            transaction.validate_snapshot(snapshot)?;
        }
        Ok(())
    }

    pub(crate) fn revalidate_cached_brick(
        &self,
        read: &LocalBrickRead,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), PackageReadError> {
        self.revalidate_cached_snapshots(read.object_snapshots(), &mut is_cancelled)
    }

    pub(crate) fn revalidate_cached_snapshots(
        &self,
        snapshots: &[LocalObjectSnapshot],
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), PackageReadError> {
        self.catalog
            .reader()
            .revalidate_cached_snapshots(snapshots)?;
        self.validate_cached_snapshots(snapshots, &mut is_cancelled)?;
        self.proof
            .revalidate_authority_cached(self.catalog.reader(), &mut is_cancelled)
            .map_err(map_snapshot_read_error)
    }

    fn validate_cached_brick_snapshots(
        &self,
        read: &LocalBrickRead,
        is_cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), PackageReadError> {
        self.validate_cached_snapshots(read.object_snapshots(), is_cancelled)
    }

    fn validate_cached_snapshots(
        &self,
        snapshots: &[LocalObjectSnapshot],
        is_cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), PackageReadError> {
        for snapshot in snapshots {
            if is_cancelled() {
                return Err(PackageReadError::Cancelled);
            }
            let Some(expected) = self.proof.object_snapshots.get(snapshot.path()) else {
                return Err(RangeReadError::ObjectChanged {
                    path: snapshot.path().to_string(),
                }
                .into());
            };
            if snapshot != expected {
                return Err(RangeReadError::ObjectChanged {
                    path: snapshot.path().to_string(),
                }
                .into());
            }
        }
        Ok(())
    }

    pub(crate) fn begin_scientific_scan(
        &self,
        is_cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), PackageValidationError> {
        self.proof
            .revalidate_authority(self.catalog.reader(), is_cancelled)?;
        Ok(())
    }

    /// Reads one brick during a package-wide scientific scan.
    ///
    /// The scan authenticates the manifest authority once around the whole
    /// operation. Each consumed shard is still compared with the exact-object
    /// snapshot captured by full package validation, avoiding an
    /// `O(bricks * manifest_pages)` authority check.
    pub(crate) fn read_brick_for_scientific_scan(
        &self,
        coordinates: PackedIndexCoordinates,
    ) -> Result<LocalBrickRead, PackageReadError> {
        let plan = self.catalog.plan_brick_storage(coordinates)?;
        // The scan already authenticates the complete authority/object set at
        // its boundaries. Reuse generation-bound handles and decoded indexes
        // between bricks; each returned component is still revalidated and
        // compared with the exact proof below.
        let read = read_local_brick_reusing_for_scientific_scan(
            self.catalog.reader(),
            self.catalog.descriptors(),
            plan,
        )?;
        for snapshot in read.object_snapshots() {
            let Some(expected) = self.proof.object_snapshots.get(snapshot.path()) else {
                return Err(RangeReadError::ObjectChanged {
                    path: snapshot.path().to_string(),
                }
                .into());
            };
            if snapshot != expected {
                return Err(RangeReadError::ObjectChanged {
                    path: snapshot.path().to_string(),
                }
                .into());
            }
        }
        Ok(read)
    }

    pub(crate) fn finish_scientific_scan(
        &self,
        is_cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), PackageValidationError> {
        self.proof
            .revalidate_authority(self.catalog.reader(), is_cancelled)?;
        self.proof
            .revalidate_all(self.catalog.reader(), is_cancelled)
    }
}

/// A typed failure before an exact package capability can be issued.
#[derive(Debug, Error)]
pub enum PackageValidationError {
    #[error(transparent)]
    Structure(#[from] PackageStructureError),
    #[error(transparent)]
    Inventory(#[from] DirectoryInventoryError),
    #[error(transparent)]
    Range(#[from] RangeReadError),
    #[error(transparent)]
    Identity(#[from] IdentityHashError),
    #[error("exact package validation was cancelled")]
    Cancelled,
    #[error("object {path} has {actual} bytes; manifest declares {expected}")]
    ObjectLengthMismatch {
        path: String,
        expected: u64,
        actual: u64,
    },
    #[error("object {path} does not match its manifest SHA-256")]
    ObjectDigestMismatch { path: String },
    #[error("structurally inspected shard {path} is absent from the manifest closure")]
    StructuralObjectMissing { path: String },
    #[error("exact package {metric} accounting overflowed")]
    AccountingOverflow { metric: &'static str },
}

pub(crate) struct PackageIntegrityInput<'a> {
    pub(crate) reader: &'a LocalPackageReader,
    pub(crate) manifest_root_path: &'a PackagePath,
    pub(crate) manifest_root_bytes: u64,
    pub(crate) manifest_root: &'a ManifestRoot,
    pub(crate) package_id: PackageId,
    pub(crate) descriptors: &'a [PackageObjectDescriptor],
    pub(crate) structure: &'a PackageStructureReport,
}

#[derive(Debug)]
pub(crate) struct PackageIntegrityProof {
    package_id: PackageId,
    authority_snapshots: Vec<LocalObjectSnapshot>,
    object_snapshots: BTreeMap<PackagePath, LocalObjectSnapshot>,
    objects_hashed: u64,
    bytes_hashed: u64,
}

impl PackageIntegrityProof {
    pub(crate) fn revalidate_all(
        &self,
        reader: &LocalPackageReader,
        is_cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), PackageValidationError> {
        for snapshot in self.object_snapshots.values() {
            if is_cancelled() {
                return Err(PackageValidationError::Cancelled);
            }
            reader.revalidate_snapshot(snapshot)?;
        }
        self.revalidate_authority(reader, is_cancelled)?;
        Ok(())
    }

    fn revalidate_authority(
        &self,
        reader: &LocalPackageReader,
        is_cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), SnapshotValidationError> {
        for snapshot in &self.authority_snapshots {
            if is_cancelled() {
                return Err(SnapshotValidationError::Cancelled);
            }
            reader
                .revalidate_snapshot(snapshot)
                .map_err(SnapshotValidationError::Range)?;
        }
        if is_cancelled() {
            Err(SnapshotValidationError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn revalidate_authority_cached(
        &self,
        reader: &LocalPackageReader,
        is_cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), SnapshotValidationError> {
        let mut batch = reader
            .begin_cached_snapshot_revalidation()
            .map_err(SnapshotValidationError::Range)?;
        for snapshot in &self.authority_snapshots {
            if is_cancelled() {
                return Err(SnapshotValidationError::Cancelled);
            }
            batch
                .validate_snapshot(snapshot)
                .map_err(SnapshotValidationError::Range)?;
        }
        batch.finish_snapshot();
        if is_cancelled() {
            Err(SnapshotValidationError::Cancelled)
        } else {
            Ok(())
        }
    }
}

enum SnapshotValidationError {
    Cancelled,
    Range(RangeReadError),
}

impl From<SnapshotValidationError> for PackageValidationError {
    fn from(error: SnapshotValidationError) -> Self {
        match error {
            SnapshotValidationError::Cancelled => Self::Cancelled,
            SnapshotValidationError::Range(error) => Self::Range(error),
        }
    }
}

#[cfg(test)]
pub(crate) fn validate_package_integrity(
    input: PackageIntegrityInput<'_>,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<PackageIntegrityProof, PackageValidationError> {
    validate_package_integrity_with_progress(input, &mut is_cancelled, &mut |_| {})
}

pub(crate) fn validate_package_integrity_with_progress(
    input: PackageIntegrityInput<'_>,
    mut is_cancelled: impl FnMut() -> bool,
    mut report_progress: impl FnMut(ExactPackageValidationProgress),
) -> Result<PackageIntegrityProof, PackageValidationError> {
    let mut structural = BTreeMap::new();
    for snapshot in input.structure.snapshots() {
        if let Some(previous) = structural.insert(snapshot.path().clone(), snapshot.clone())
            && previous != *snapshot
        {
            return Err(RangeReadError::ObjectChanged {
                path: snapshot.path().to_string(),
            }
            .into());
        }
    }

    let mut proof = PackageIntegrityProof {
        package_id: input.package_id,
        authority_snapshots: Vec::with_capacity(input.manifest_root.pages().len() + 1),
        object_snapshots: BTreeMap::new(),
        objects_hashed: 0,
        bytes_hashed: 0,
    };

    let root_digest = ExactBytesDigest::from_digest(input.package_id.digest());
    let root = hash_expected_object(
        input.reader,
        input.manifest_root_path,
        input.manifest_root_bytes,
        root_digest,
        &mut proof,
        &mut is_cancelled,
        &mut report_progress,
    )?;
    proof.authority_snapshots.push(root);

    for page in input.manifest_root.pages() {
        let snapshot = hash_expected_object(
            input.reader,
            page.path(),
            page.byte_length(),
            page.digest(),
            &mut proof,
            &mut is_cancelled,
            &mut report_progress,
        )?;
        proof.authority_snapshots.push(snapshot);
    }

    for descriptor in input.descriptors {
        let snapshot = hash_expected_object(
            input.reader,
            descriptor.path(),
            descriptor.raw().byte_length(),
            descriptor.raw().digest(),
            &mut proof,
            &mut is_cancelled,
            &mut report_progress,
        )?;
        if let Some(structural_snapshot) = structural.remove(descriptor.path())
            && structural_snapshot != snapshot
        {
            return Err(RangeReadError::ObjectChanged {
                path: descriptor.path().to_string(),
            }
            .into());
        }
        proof
            .object_snapshots
            .insert(descriptor.path().clone(), snapshot);
    }

    if let Some((path, _)) = structural.into_iter().next() {
        return Err(PackageValidationError::StructuralObjectMissing {
            path: path.to_string(),
        });
    }
    if is_cancelled() {
        Err(PackageValidationError::Cancelled)
    } else {
        Ok(proof)
    }
}

fn hash_expected_object(
    reader: &LocalPackageReader,
    path: &PackagePath,
    expected_bytes: u64,
    expected_digest: ExactBytesDigest,
    proof: &mut PackageIntegrityProof,
    is_cancelled: &mut impl FnMut() -> bool,
    report_progress: &mut impl FnMut(ExactPackageValidationProgress),
) -> Result<LocalObjectSnapshot, PackageValidationError> {
    let hashed = reader
        .hash_object_with_snapshot(path, expected_bytes, &mut *is_cancelled)
        .map_err(|error| map_hash_error(path, error))?;
    if hashed.facts.digest() != expected_digest {
        return Err(PackageValidationError::ObjectDigestMismatch {
            path: path.to_string(),
        });
    }
    proof.objects_hashed =
        proof
            .objects_hashed
            .checked_add(1)
            .ok_or(PackageValidationError::AccountingOverflow {
                metric: "object count",
            })?;
    proof.bytes_hashed = proof
        .bytes_hashed
        .checked_add(hashed.facts.byte_length())
        .ok_or(PackageValidationError::AccountingOverflow {
            metric: "byte count",
        })?;
    report_progress(ExactPackageValidationProgress {
        objects_hashed: proof.objects_hashed,
        bytes_hashed: proof.bytes_hashed,
    });
    Ok(hashed.snapshot)
}

fn map_hash_error(path: &PackagePath, error: LocalObjectHashError) -> PackageValidationError {
    match error {
        LocalObjectHashError::Range(error) => error.into(),
        LocalObjectHashError::Identity(error) => error.into(),
        LocalObjectHashError::Cancelled => PackageValidationError::Cancelled,
        LocalObjectHashError::DeclaredLengthMismatch { expected, actual } => {
            PackageValidationError::ObjectLengthMismatch {
                path: path.to_string(),
                expected,
                actual,
            }
        }
    }
}

fn map_snapshot_read_error(error: SnapshotValidationError) -> PackageReadError {
    match error {
        SnapshotValidationError::Cancelled => PackageReadError::Cancelled,
        SnapshotValidationError::Range(error) => PackageReadError::Range(error),
    }
}
