use std::collections::BTreeMap;

use mirante4d_identity::{ExactBytesDigest, IdentityHashError, PackageId};
use thiserror::Error;

use crate::package_read::{LocalBrickRead, PackageReadError, read_local_brick};
use crate::package_structure::{PackageStructureError, PackageStructureReport};
use crate::range_io::{LocalObjectHashError, LocalObjectSnapshot};
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
        self.proof
            .revalidate_authority(self.catalog.reader(), &mut is_cancelled)
            .map_err(map_snapshot_read_error)?;
        if is_cancelled() {
            return Err(PackageReadError::Cancelled);
        }

        let plan = self.catalog.plan_brick_storage(coordinates)?;
        let read = read_local_brick(self.catalog.reader(), self.catalog.descriptors(), plan)?;
        for snapshot in read.object_snapshots() {
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
            self.catalog.reader().revalidate_snapshot(snapshot)?;
        }
        self.proof
            .revalidate_authority(self.catalog.reader(), &mut is_cancelled)
            .map_err(map_snapshot_read_error)?;
        Ok(read)
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

pub(crate) fn validate_package_integrity(
    input: PackageIntegrityInput<'_>,
    mut is_cancelled: impl FnMut() -> bool,
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
