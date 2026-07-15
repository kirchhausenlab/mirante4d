use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, Metadata},
    io::{self, Write},
    path::Path,
};

use mirante4d_identity::{ExactBytesHasher, IdentityHashError, PackageId};
use thiserror::Error;

use crate::local_publication::{LocalPublication, LocalPublicationError};
use crate::range_io::LocalObjectSnapshot;
use crate::shard::encode_shard_index_tail;
use crate::{
    ControlError, DatasetProfileAdmission, DisplayDefaults, LocalPackageCatalog, ManifestRoot,
    OmeImageGroupMetadata, PackageObjectDescriptor, PackageObjectKind, PackageOpenError,
    PackagePath, PackageStructureError, PackageValidationError, PortableRecord, ProfileHeader,
    ProfileKind, RangeReadError, ScienceDescriptor, ScientificPackageValidationError,
    ShardCodecError, ShardProfileKind, StorageProfileError, ZarrArrayMetadata, ZarrGroupMetadata,
    ZarrMetadataError, encode_inner_payload, manifest_page_path, pack_manifest_pages,
    profile_limits,
};

const PROFILE_PATH: &str = "m4d/profile.json";

/// One profile-addressed Zarr array whose metadata bytes are writer-owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageArrayInput {
    path: PackagePath,
    metadata: ZarrArrayMetadata,
}

impl PackageArrayInput {
    pub const fn new(path: PackagePath, metadata: ZarrArrayMetadata) -> Self {
        Self { path, metadata }
    }

    pub const fn path(&self) -> &PackagePath {
        &self.path
    }

    pub const fn metadata(&self) -> &ZarrArrayMetadata {
        &self.metadata
    }
}

/// One bounded outer shard supplied as decoded inner chunks in slot order.
///
/// A caller may generate these values lazily. The writer consumes and drops
/// each complete outer shard before requesting the next one.
#[derive(Debug, PartialEq, Eq)]
pub struct PackageShardInput {
    array_path: PackagePath,
    outer_coordinates: Vec<u64>,
    decoded_chunks: Vec<Option<Vec<u8>>>,
}

impl PackageShardInput {
    pub fn new(
        array_path: PackagePath,
        outer_coordinates: Vec<u64>,
        decoded_chunks: Vec<Option<Vec<u8>>>,
    ) -> Self {
        Self {
            array_path,
            outer_coordinates,
            decoded_chunks,
        }
    }

    pub const fn array_path(&self) -> &PackagePath {
        &self.array_path
    }

    pub fn outer_coordinates(&self) -> &[u64] {
        &self.outer_coordinates
    }

    pub fn decoded_chunks(&self) -> &[Option<Vec<u8>>] {
        &self.decoded_chunks
    }
}

/// Complete typed input to the deterministic package writer.
///
/// `shards` may be any lazy iterator. Encoded shards, descriptors, manifest
/// pages, roots, and their paths are deliberately not accepted as input.
pub struct PackageWriteInput<I> {
    profile_kind: ProfileKind,
    profile: ProfileHeader,
    science: ScienceDescriptor,
    display_defaults: DisplayDefaults,
    portable_records: Vec<PortableRecord>,
    ome_images: Vec<OmeImageGroupMetadata>,
    arrays: Vec<PackageArrayInput>,
    shards: I,
}

impl<I> PackageWriteInput<I> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        profile_kind: ProfileKind,
        profile: ProfileHeader,
        science: ScienceDescriptor,
        display_defaults: DisplayDefaults,
        portable_records: Vec<PortableRecord>,
        ome_images: Vec<OmeImageGroupMetadata>,
        arrays: Vec<PackageArrayInput>,
        shards: I,
    ) -> Self {
        Self {
            profile_kind,
            profile,
            science,
            display_defaults,
            portable_records,
            ome_images,
            arrays,
            shards,
        }
    }
}

/// Durable result of one create-only package publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackageWriteReceipt {
    package_id: PackageId,
    admission: DatasetProfileAdmission,
}

impl PackageWriteReceipt {
    pub const fn package_id(self) -> PackageId {
        self.package_id
    }

    pub const fn admission(self) -> DatasetProfileAdmission {
        self.admission
    }
}

/// Typed failure from deterministic package construction or publication.
#[derive(Debug, Error)]
pub enum PackageWriteError {
    #[error("package writing was cancelled before publication")]
    Cancelled,
    #[error("invalid package writer input: {reason}")]
    InvalidInput { reason: &'static str },
    #[error("the destination already exists and was not changed")]
    DestinationExists,
    #[error("atomic create-only directory publication is unsupported on this filesystem")]
    AtomicPublishUnsupported,
    #[error("package {package_id} became visible, but final directory durability is unknown")]
    CommitIndeterminate {
        package_id: PackageId,
        #[source]
        source: io::Error,
    },
    #[error("package filesystem operation {operation} failed")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error(transparent)]
    Identity(#[from] IdentityHashError),
    #[error(transparent)]
    Metadata(#[from] ZarrMetadataError),
    #[error(transparent)]
    Shard(#[from] ShardCodecError),
    #[error(transparent)]
    Open(#[from] PackageOpenError),
    #[error(transparent)]
    Structure(#[from] PackageStructureError),
    #[error(transparent)]
    ExactValidation(PackageValidationError),
    #[error(transparent)]
    ScientificValidation(ScientificPackageValidationError),
    #[error(transparent)]
    Range(#[from] RangeReadError),
    #[error(transparent)]
    Profile(#[from] StorageProfileError),
}

/// Sole writer for a new local target-profile package.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalPackageWriter;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StagedValidation {
    Structure,
    Scientific,
}

impl LocalPackageWriter {
    /// Writes, validates, and atomically publishes one previously absent
    /// package directory.
    pub fn write_new<I>(
        destination: impl AsRef<Path>,
        input: PackageWriteInput<I>,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<PackageWriteReceipt, PackageWriteError>
    where
        I: IntoIterator<Item = PackageShardInput>,
    {
        Self::write_new_with_validation(
            destination,
            input,
            &mut is_cancelled,
            StagedValidation::Structure,
        )
    }

    /// Writes a new package, proves its exact-byte closure and declared
    /// scientific identity while it is staged, and publishes it atomically
    /// only after both validations succeed.
    pub fn write_new_scientifically_validated<I>(
        destination: impl AsRef<Path>,
        input: PackageWriteInput<I>,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<PackageWriteReceipt, PackageWriteError>
    where
        I: IntoIterator<Item = PackageShardInput>,
    {
        Self::write_new_with_validation(
            destination,
            input,
            &mut is_cancelled,
            StagedValidation::Scientific,
        )
    }

    fn write_new_with_validation<I>(
        destination: impl AsRef<Path>,
        input: PackageWriteInput<I>,
        mut is_cancelled: &mut impl FnMut() -> bool,
        staged_validation: StagedValidation,
    ) -> Result<PackageWriteReceipt, PackageWriteError>
    where
        I: IntoIterator<Item = PackageShardInput>,
    {
        check_cancelled(&mut is_cancelled)?;
        let PackageWriteInput {
            profile_kind,
            profile,
            science,
            display_defaults,
            portable_records,
            ome_images,
            arrays,
            shards,
        } = input;
        let prepared = prepare_metadata(
            &profile,
            &science,
            &display_defaults,
            &portable_records,
            &ome_images,
            arrays,
        )?;
        drop((science, display_defaults, portable_records, ome_images));
        let PreparedMetadata { objects, arrays } = prepared;
        let limits = profile_limits(profile_kind);
        let shard_input_limit = limits
            .pixel_shards
            .checked_add(limits.validity_shards)
            .and_then(|value| value.checked_add(limits.packed_index_shards))
            .ok_or(PackageWriteError::InvalidInput {
                reason: "the selected profile shard-input bound overflowed",
            })?;

        check_cancelled(&mut is_cancelled)?;
        let mut publication =
            LocalPublication::begin(destination).map_err(map_publication_error_without_commit)?;
        let mut descriptors = Vec::new();
        let mut snapshots = Vec::new();
        let mut written_paths = BTreeSet::new();

        for object in objects {
            check_cancelled(&mut is_cancelled)?;
            require_descriptor_capacity(descriptors.len(), limits.total_physical_objects)?;
            require_new_path(&mut written_paths, &object.path)?;
            let (descriptor, snapshot) =
                write_object_bytes(&mut publication, object.path, object.kind, &object.bytes)?;
            descriptors.push(descriptor);
            snapshots.push(snapshot);
        }

        let mut shard_inputs_seen = 0_u64;
        let mut shards = shards.into_iter();
        // A `for` loop would consume and drop the iterator before validation.
        #[allow(clippy::while_let_on_iterator)]
        while let Some(shard) = shards.next() {
            check_cancelled(&mut is_cancelled)?;
            shard_inputs_seen =
                shard_inputs_seen
                    .checked_add(1)
                    .ok_or(PackageWriteError::InvalidInput {
                        reason: "the shard-input count overflowed",
                    })?;
            if shard_inputs_seen > shard_input_limit {
                return invalid_input("shard inputs exceed the selected profile bound");
            }
            let array = arrays
                .get(&shard.array_path)
                .ok_or(PackageWriteError::InvalidInput {
                    reason: "a shard names an array outside the profile",
                })?;
            let expected_coordinates = match array.shard_kind {
                PackageObjectKind::PackedIndexShard => 2,
                PackageObjectKind::PixelShard | PackageObjectKind::ValidityShard => 5,
                _ => unreachable!("prepared arrays have only shard object kinds"),
            };
            if shard.outer_coordinates.len() != expected_coordinates
                || (array.shard_kind == PackageObjectKind::PackedIndexShard
                    && shard.outer_coordinates[1] != 0)
            {
                return Err(PackageWriteError::InvalidInput {
                    reason: "a shard has the wrong outer-coordinate shape",
                });
            }
            if shard.decoded_chunks.len() != array.metadata.kind().chunks_per_shard() {
                return Err(ShardCodecError::ChunkCount {
                    expected: array.metadata.kind().chunks_per_shard(),
                    actual: shard.decoded_chunks.len(),
                }
                .into());
            }
            if shard.decoded_chunks.iter().all(Option::is_none) {
                if array.shard_kind == PackageObjectKind::PackedIndexShard {
                    return Err(PackageWriteError::InvalidInput {
                        reason: "a packed-index shard cannot contain only missing slots",
                    });
                }
                continue;
            }

            let shard_path = shard_path(&shard.array_path, &shard.outer_coordinates)?;
            require_descriptor_capacity(descriptors.len(), limits.total_physical_objects)?;
            require_new_path(&mut written_paths, &shard_path)?;
            let (descriptor, snapshot) = write_shard(
                &mut publication,
                shard_path,
                array.shard_kind,
                array.metadata.kind(),
                shard.decoded_chunks,
                &mut is_cancelled,
            )?;
            descriptors.push(descriptor);
            snapshots.push(snapshot);
        }

        check_cancelled(&mut is_cancelled)?;
        drop((arrays, written_paths));
        let pages = pack_manifest_pages(descriptors)?;
        let root = ManifestRoot::new(&pages)?;
        for (ordinal, page) in pages.iter().enumerate() {
            check_cancelled(&mut is_cancelled)?;
            let ordinal = u32::try_from(ordinal).map_err(|_| PackageWriteError::InvalidInput {
                reason: "manifest page ordinal exceeds u32",
            })?;
            let path = manifest_page_path(ordinal)?;
            let snapshot = write_authority_bytes(&mut publication, path, &page.canonical_bytes()?)?;
            snapshots.push(snapshot);
        }
        drop(pages);
        let root_path = profile.manifest_root_path().clone();
        drop(profile);
        let root_snapshot =
            write_authority_bytes(&mut publication, root_path, &root.canonical_bytes()?)?;
        snapshots.push(root_snapshot);
        let package_id = root.package_id()?;

        publication
            .sync_directories(&mut is_cancelled)
            .map_err(map_publication_error_without_commit)?;
        check_cancelled(&mut is_cancelled)?;

        let catalog = LocalPackageCatalog::open(publication.stage_path())?;
        if catalog.declared_package_id() != package_id {
            return Err(PackageWriteError::InvalidInput {
                reason: "the staged manifest root changed before validation",
            });
        }
        let admission = catalog
            .validate_package_structure(profile_kind, &mut is_cancelled)
            .map_err(map_structure_error)?;
        for snapshot in &snapshots {
            check_cancelled(&mut is_cancelled)?;
            catalog.reader().revalidate_snapshot(snapshot)?;
        }
        drop(snapshots);
        check_cancelled(&mut is_cancelled)?;

        if staged_validation == StagedValidation::Scientific {
            let exact = catalog
                .validate_exact_package(profile_kind, &mut is_cancelled)
                .map_err(map_exact_validation_error)?;
            if exact.package_id() != package_id || exact.admission() != admission {
                return invalid_input("staged exact validation disagrees with writer admission");
            }
            exact
                .validate_scientific_content(&mut is_cancelled)
                .map_err(map_scientific_validation_error)?;
            check_cancelled(&mut is_cancelled)?;
        }

        // Some lazy producers retain the final bounded-memory lease in their
        // iterator. Keep that lease alive through staged validation.
        drop(shards);

        publication
            .commit(package_id)
            .map_err(|error| map_publication_error(error, package_id))?;
        Ok(PackageWriteReceipt {
            package_id,
            admission,
        })
    }
}

struct PreparedObject {
    path: PackagePath,
    kind: PackageObjectKind,
    bytes: Vec<u8>,
}

struct PreparedArray {
    metadata: ZarrArrayMetadata,
    shard_kind: PackageObjectKind,
}

struct PreparedMetadata {
    objects: Vec<PreparedObject>,
    arrays: BTreeMap<PackagePath, PreparedArray>,
}

fn prepare_metadata(
    profile: &ProfileHeader,
    science: &ScienceDescriptor,
    display_defaults: &DisplayDefaults,
    portable_records: &[PortableRecord],
    ome_images: &[OmeImageGroupMetadata],
    arrays: Vec<PackageArrayInput>,
) -> Result<PreparedMetadata, PackageWriteError> {
    if portable_records.len() != profile.portable_record_paths().len() {
        return invalid_input("portable-record count does not match the profile");
    }
    if ome_images.len() != profile.images().len() {
        return invalid_input("OME image metadata count does not match the profile");
    }

    let group_bytes = ZarrGroupMetadata::new().deterministic_bytes()?;
    let mut objects = vec![
        prepared(
            "zarr.json",
            PackageObjectKind::ZarrRoot,
            group_bytes.clone(),
        )?,
        prepared(
            "images/zarr.json",
            PackageObjectKind::ZarrImagesGroup,
            group_bytes.clone(),
        )?,
        prepared(
            "validity/zarr.json",
            PackageObjectKind::ZarrValidityGroup,
            group_bytes.clone(),
        )?,
        prepared(
            "indexes/zarr.json",
            PackageObjectKind::ZarrIndexesGroup,
            group_bytes,
        )?,
        PreparedObject {
            path: PackagePath::parse(PROFILE_PATH)?,
            kind: PackageObjectKind::Profile,
            bytes: profile.canonical_bytes()?,
        },
        PreparedObject {
            path: profile.science_path().clone(),
            kind: PackageObjectKind::Science,
            bytes: science.canonical_bytes()?,
        },
        PreparedObject {
            path: profile.display_defaults_path().clone(),
            kind: PackageObjectKind::DisplayDefaults,
            bytes: display_defaults.canonical_bytes()?,
        },
    ];

    for ((record, path), ordinal) in portable_records
        .iter()
        .zip(profile.portable_record_paths())
        .zip(0_u64..)
    {
        if record.record_ordinal().get() != ordinal {
            return invalid_input("portable-record ordinals must match their profile paths");
        }
        objects.push(PreparedObject {
            path: path.clone(),
            kind: PackageObjectKind::PortableRecord,
            bytes: record.canonical_bytes()?,
        });
    }

    for (image, ome) in profile.images().iter().zip(ome_images) {
        objects.push(PreparedObject {
            path: metadata_path(image.image_group_path())?,
            kind: PackageObjectKind::ZarrImageGroup,
            bytes: ome.deterministic_bytes()?,
        });
    }

    let expected = expected_arrays(profile)?;
    let mut prepared_arrays = BTreeMap::new();
    for array in arrays {
        let metadata_kind =
            expected
                .get(&array.path)
                .copied()
                .ok_or(PackageWriteError::InvalidInput {
                    reason: "array metadata names a path outside the profile",
                })?;
        if !array_kind_matches(metadata_kind, array.metadata.kind()) {
            return invalid_input("array metadata uses the wrong storage-profile row");
        }
        let shard_kind = shard_kind(metadata_kind);
        let metadata_bytes = array.metadata.deterministic_bytes()?;
        let path = metadata_path(&array.path)?;
        if prepared_arrays
            .insert(
                array.path,
                PreparedArray {
                    metadata: array.metadata,
                    shard_kind,
                },
            )
            .is_some()
        {
            return invalid_input("array metadata paths must be unique");
        }
        objects.push(PreparedObject {
            path,
            kind: metadata_kind,
            bytes: metadata_bytes,
        });
    }
    if prepared_arrays.len() != expected.len()
        || expected
            .keys()
            .any(|path| !prepared_arrays.contains_key(path))
    {
        return invalid_input("array metadata does not exactly cover the profile");
    }

    objects.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    Ok(PreparedMetadata {
        objects,
        arrays: prepared_arrays,
    })
}

fn expected_arrays(
    profile: &ProfileHeader,
) -> Result<BTreeMap<PackagePath, PackageObjectKind>, PackageWriteError> {
    let mut expected = BTreeMap::new();
    for image in profile.images() {
        for level in image.levels() {
            insert_expected(
                &mut expected,
                level.pixel_path().clone(),
                PackageObjectKind::ZarrPixelArray,
            )?;
            insert_expected(
                &mut expected,
                level.packed_index_path().clone(),
                PackageObjectKind::ZarrPackedIndexArray,
            )?;
            if let Some(path) = level.validity_path() {
                insert_expected(
                    &mut expected,
                    path.clone(),
                    PackageObjectKind::ZarrValidityArray,
                )?;
            }
        }
    }
    Ok(expected)
}

fn insert_expected(
    expected: &mut BTreeMap<PackagePath, PackageObjectKind>,
    path: PackagePath,
    kind: PackageObjectKind,
) -> Result<(), PackageWriteError> {
    if expected.insert(path, kind).is_some() {
        return invalid_input("profile array paths must be unique");
    }
    Ok(())
}

fn array_kind_matches(kind: PackageObjectKind, storage: ShardProfileKind) -> bool {
    match kind {
        PackageObjectKind::ZarrPixelArray => matches!(
            storage,
            ShardProfileKind::Pixel3dUint8
                | ShardProfileKind::Pixel3dUint16
                | ShardProfileKind::Pixel3dFloat32
                | ShardProfileKind::Pixel2dUint8
                | ShardProfileKind::Pixel2dUint16
                | ShardProfileKind::Pixel2dFloat32
        ),
        PackageObjectKind::ZarrValidityArray => matches!(
            storage,
            ShardProfileKind::Validity3d | ShardProfileKind::Validity2d
        ),
        PackageObjectKind::ZarrPackedIndexArray => storage == ShardProfileKind::PackedIndex,
        _ => false,
    }
}

fn shard_kind(metadata_kind: PackageObjectKind) -> PackageObjectKind {
    match metadata_kind {
        PackageObjectKind::ZarrPixelArray => PackageObjectKind::PixelShard,
        PackageObjectKind::ZarrValidityArray => PackageObjectKind::ValidityShard,
        PackageObjectKind::ZarrPackedIndexArray => PackageObjectKind::PackedIndexShard,
        _ => unreachable!("only array metadata kinds are prepared"),
    }
}

fn prepared(
    path: &str,
    kind: PackageObjectKind,
    bytes: Vec<u8>,
) -> Result<PreparedObject, PackageWriteError> {
    Ok(PreparedObject {
        path: PackagePath::parse(path)?,
        kind,
        bytes,
    })
}

fn metadata_path(base: &PackagePath) -> Result<PackagePath, PackageWriteError> {
    Ok(PackagePath::parse(&format!("{base}/zarr.json"))?)
}

fn shard_path(array: &PackagePath, coordinates: &[u64]) -> Result<PackagePath, PackageWriteError> {
    let coordinates = coordinates
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join("/");
    Ok(PackagePath::parse(&format!("{array}/c/{coordinates}"))?)
}

fn require_new_path(
    written: &mut BTreeSet<PackagePath>,
    path: &PackagePath,
) -> Result<(), PackageWriteError> {
    if !written.insert(path.clone()) {
        return invalid_input("two writer inputs derive the same package path");
    }
    Ok(())
}

fn require_descriptor_capacity(current: usize, maximum: u64) -> Result<(), PackageWriteError> {
    if u64::try_from(current).map_or(true, |current| current >= maximum) {
        invalid_input("manifest descriptors exceed the selected profile bound")
    } else {
        Ok(())
    }
}

fn write_object_bytes(
    publication: &mut LocalPublication,
    path: PackagePath,
    kind: PackageObjectKind,
    bytes: &[u8],
) -> Result<(PackageObjectDescriptor, LocalObjectSnapshot), PackageWriteError> {
    let (facts, snapshot) = write_hashed_file(publication, path.clone(), |file, hasher| {
        write_hashed(file, hasher, bytes)
    })?;
    let descriptor = PackageObjectDescriptor::new(path, kind, facts.byte_length(), facts.digest())?;
    Ok((descriptor, snapshot))
}

fn write_authority_bytes(
    publication: &mut LocalPublication,
    path: PackagePath,
    bytes: &[u8],
) -> Result<LocalObjectSnapshot, PackageWriteError> {
    write_hashed_file(publication, path, |file, hasher| {
        write_hashed(file, hasher, bytes)
    })
    .map(|(_facts, snapshot)| snapshot)
}

fn write_shard(
    publication: &mut LocalPublication,
    path: PackagePath,
    object_kind: PackageObjectKind,
    codec_kind: ShardProfileKind,
    decoded_chunks: Vec<Option<Vec<u8>>>,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<(PackageObjectDescriptor, LocalObjectSnapshot), PackageWriteError> {
    let (facts, snapshot) = write_hashed_file(publication, path.clone(), |file, hasher| {
        let mut lengths = Vec::with_capacity(codec_kind.chunks_per_shard());
        for decoded in decoded_chunks {
            check_cancelled(is_cancelled)?;
            match decoded {
                Some(decoded) => {
                    let encoded = encode_inner_payload(codec_kind, &decoded)?;
                    let encoded_length = u64::try_from(encoded.len())
                        .map_err(|_| ShardCodecError::LengthOverflow)?;
                    write_hashed(file, hasher, &encoded)?;
                    lengths.push(Some(encoded_length));
                }
                None => lengths.push(None),
            }
        }
        let tail = encode_shard_index_tail(codec_kind, &lengths)?;
        write_hashed(file, hasher, &tail)
    })?;
    let descriptor =
        PackageObjectDescriptor::new(path, object_kind, facts.byte_length(), facts.digest())?;
    Ok((descriptor, snapshot))
}

fn write_hashed_file(
    publication: &mut LocalPublication,
    path: PackagePath,
    write_body: impl FnOnce(&mut File, &mut ExactBytesHasher) -> Result<(), PackageWriteError>,
) -> Result<(mirante4d_identity::ExactBytesFacts, LocalObjectSnapshot), PackageWriteError> {
    let mut file = publication
        .create_file(&path)
        .map_err(map_publication_error_without_commit)?;
    let mut hasher = ExactBytesHasher::new();
    write_body(&mut file, &mut hasher)?;
    file.sync_all().map_err(|source| PackageWriteError::Io {
        operation: "sync staged package object",
        source,
    })?;
    let metadata = file.metadata().map_err(|source| PackageWriteError::Io {
        operation: "inspect staged package object",
        source,
    })?;
    let facts = hasher.finalize()?;
    if facts.byte_length() != metadata.len() {
        return invalid_input("the staged object length changed while it was written");
    }
    let snapshot = snapshot(path, &metadata)?;
    Ok((facts, snapshot))
}

fn write_hashed(
    file: &mut File,
    hasher: &mut ExactBytesHasher,
    bytes: &[u8],
) -> Result<(), PackageWriteError> {
    file.write_all(bytes)
        .map_err(|source| PackageWriteError::Io {
            operation: "write staged package object",
            source,
        })?;
    hasher.update(bytes)?;
    Ok(())
}

#[cfg(unix)]
fn snapshot(
    path: PackagePath,
    metadata: &Metadata,
) -> Result<LocalObjectSnapshot, PackageWriteError> {
    Ok(LocalObjectSnapshot::from_metadata(path, metadata)?)
}

#[cfg(not(unix))]
fn snapshot(
    _path: PackagePath,
    _metadata: &Metadata,
) -> Result<LocalObjectSnapshot, PackageWriteError> {
    Err(PackageWriteError::Range(
        RangeReadError::UnsupportedPlatform,
    ))
}

fn check_cancelled(is_cancelled: &mut impl FnMut() -> bool) -> Result<(), PackageWriteError> {
    if is_cancelled() {
        Err(PackageWriteError::Cancelled)
    } else {
        Ok(())
    }
}

fn invalid_input<T>(reason: &'static str) -> Result<T, PackageWriteError> {
    Err(PackageWriteError::InvalidInput { reason })
}

fn map_publication_error_without_commit(error: LocalPublicationError) -> PackageWriteError {
    match error {
        LocalPublicationError::Cancelled => PackageWriteError::Cancelled,
        LocalPublicationError::DestinationExists => PackageWriteError::DestinationExists,
        LocalPublicationError::AtomicPublishUnsupported { .. } => {
            PackageWriteError::AtomicPublishUnsupported
        }
        LocalPublicationError::CommitIndeterminate { source } => PackageWriteError::Io {
            operation: "unexpected precommit durability state",
            source,
        },
        LocalPublicationError::Io { operation, source } => {
            PackageWriteError::Io { operation, source }
        }
    }
}

fn map_publication_error(error: LocalPublicationError, package_id: PackageId) -> PackageWriteError {
    match error {
        LocalPublicationError::CommitIndeterminate { source } => {
            PackageWriteError::CommitIndeterminate { package_id, source }
        }
        other => map_publication_error_without_commit(other),
    }
}

fn map_structure_error(error: PackageStructureError) -> PackageWriteError {
    if matches!(
        error,
        PackageStructureError::Cancelled
            | PackageStructureError::Admission(crate::PackageAdmissionError::Inventory(
                crate::DirectoryInventoryError::Cancelled
            ))
    ) {
        PackageWriteError::Cancelled
    } else {
        PackageWriteError::Structure(error)
    }
}

fn map_exact_validation_error(error: PackageValidationError) -> PackageWriteError {
    if matches!(error, PackageValidationError::Cancelled) {
        PackageWriteError::Cancelled
    } else {
        PackageWriteError::ExactValidation(error)
    }
}

fn map_scientific_validation_error(error: ScientificPackageValidationError) -> PackageWriteError {
    if matches!(error, ScientificPackageValidationError::Cancelled) {
        PackageWriteError::Cancelled
    } else {
        PackageWriteError::ScientificValidation(error)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        fs,
        path::{Path, PathBuf},
        rc::Rc,
        sync::atomic::{AtomicU64, Ordering},
    };

    use mirante4d_domain::{IntensityDType, LogicalLayerKey, Shape4D};
    use mirante4d_identity::ScientificContentId;

    use super::*;
    use crate::{
        DisplayLayerDefaults, F32Bits, F64Bits, OmeInteroperabilityBase, OmeLevelTransform,
        PackedIndexCoordinates, PackedIndexRecord, PackedIndexStatistics, ProfileImage,
        ProfileLevel, ProfileLogicalLayer, ProfileValidityMode, Rgb24, ScienceLayer,
        ScienceTemporalCalibration,
    };

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[derive(Clone, Copy)]
    enum BrickMode {
        PixelPresent,
        AllFill,
        ExplicitValidity,
        ExplicitAllInvalid,
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "mirante4d-package-writer-{label}-{}-{}",
                std::process::id(),
                TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn writes_byte_identical_exact_packages_independent_of_parent_and_input_order() {
        let root = TestDirectory::new("deterministic");
        let first_parent = root.0.join("first");
        let second_parent = root.0.join("second");
        fs::create_dir(&first_parent).unwrap();
        fs::create_dir(&second_parent).unwrap();
        let first = first_parent.join("data.m4d");
        let second = second_parent.join("renamed.m4d");

        let first_receipt = LocalPackageWriter::write_new(
            &first,
            fixture_input(BrickMode::PixelPresent, false),
            || false,
        )
        .unwrap();
        let second_receipt = LocalPackageWriter::write_new(
            &second,
            fixture_input(BrickMode::PixelPresent, true),
            || false,
        )
        .unwrap();

        assert_eq!(first_receipt, second_receipt);
        assert_eq!(tree_bytes(&first), tree_bytes(&second));
        let capability = LocalPackageCatalog::open(&first)
            .unwrap()
            .validate_exact_package(ProfileKind::Ds0, || false)
            .unwrap();
        assert_eq!(capability.package_id(), first_receipt.package_id());
        assert_eq!(capability.admission(), first_receipt.admission());
        let brick = capability
            .read_brick(PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0), || false)
            .unwrap();
        assert_eq!(brick.logical_extent_zyx(), [1, 2, 3]);
        assert!(brick.pixel_payload().is_some());
        assert!(brick.validity_payload().is_none());
    }

    #[test]
    fn writer_outputs_cover_fill_and_explicit_validity_modes() {
        for (label, mode, pixel, validity) in [
            ("all-fill", BrickMode::AllFill, false, false),
            ("explicit-validity", BrickMode::ExplicitValidity, true, true),
            (
                "explicit-all-invalid",
                BrickMode::ExplicitAllInvalid,
                false,
                false,
            ),
        ] {
            let root = TestDirectory::new(label);
            let destination = root.0.join("data.m4d");
            let receipt =
                LocalPackageWriter::write_new(&destination, fixture_input(mode, false), || false)
                    .unwrap();
            let capability = LocalPackageCatalog::open(&destination)
                .unwrap()
                .validate_exact_package(ProfileKind::Ds0, || false)
                .unwrap();
            assert_eq!(capability.package_id(), receipt.package_id());
            let brick = capability
                .read_brick(PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0), || false)
                .unwrap();
            assert_eq!(brick.pixel_payload().is_some(), pixel);
            assert_eq!(brick.validity_payload().is_some(), validity);
        }
    }

    #[test]
    fn cancellation_and_collision_never_touch_destination_or_source() {
        let root = TestDirectory::new("safety");
        let source = root.0.join("source.tif");
        fs::write(&source, b"immutable-source").unwrap();
        let checks = Cell::new(0_usize);
        LocalPackageWriter::write_new(
            root.0.join("count-checks.m4d"),
            fixture_input(BrickMode::PixelPresent, false),
            || {
                checks.set(checks.get() + 1);
                false
            },
        )
        .unwrap();
        let total_checks = checks.get();
        assert!(total_checks > 6);

        for (ordinal, cancel_at) in [
            0,
            total_checks / 3,
            (total_checks * 2) / 3,
            total_checks - 1,
        ]
        .into_iter()
        .enumerate()
        {
            let cancelled_destination = root.0.join(format!("cancelled-{ordinal}.m4d"));
            let checks = Cell::new(0_usize);
            let error = LocalPackageWriter::write_new(
                &cancelled_destination,
                fixture_input(BrickMode::PixelPresent, false),
                || {
                    let current = checks.get();
                    checks.set(current + 1);
                    current == cancel_at
                },
            )
            .unwrap_err();
            assert!(matches!(error, PackageWriteError::Cancelled));
            assert!(!cancelled_destination.exists());
            assert!(!fs::read_dir(&root.0).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".mirante4d-stage-")
            }));
        }

        let collision = root.0.join("collision.m4d");
        fs::write(&collision, b"keep-existing").unwrap();
        let error = LocalPackageWriter::write_new(
            &collision,
            fixture_input(BrickMode::PixelPresent, false),
            || false,
        )
        .unwrap_err();
        assert!(matches!(error, PackageWriteError::DestinationExists));
        assert_eq!(fs::read(collision).unwrap(), b"keep-existing");
        assert_eq!(fs::read(source).unwrap(), b"immutable-source");
    }

    #[test]
    fn scientific_writer_validates_the_stage_before_publication() {
        let root = TestDirectory::new("scientific-stage-validation");
        let mismatch = root.0.join("mismatch.m4d");
        let computed = match LocalPackageWriter::write_new_scientifically_validated(
            &mismatch,
            fixture_input(BrickMode::PixelPresent, false),
            || false,
        )
        .unwrap_err()
        {
            PackageWriteError::ScientificValidation(
                ScientificPackageValidationError::ScientificContentMismatch { computed, .. },
            ) => computed,
            other => panic!("unexpected staged-validation error: {other}"),
        };
        assert!(!mismatch.exists());

        let validated = root.0.join("validated.m4d");
        let receipt = LocalPackageWriter::write_new_scientifically_validated(
            &validated,
            fixture_input_with_scientific_id(BrickMode::PixelPresent, false, computed),
            || false,
        )
        .unwrap();
        let capability = LocalPackageCatalog::open(&validated)
            .unwrap()
            .validate_exact_package(ProfileKind::Ds0, || false)
            .unwrap()
            .validate_scientific_content(|| false)
            .unwrap();
        assert_eq!(capability.package_id(), receipt.package_id());
        assert_eq!(capability.scientific_content_id(), computed);

        let cancelled = root.0.join("cancelled.m4d");
        let error = LocalPackageWriter::write_new_scientifically_validated(
            &cancelled,
            fixture_input_with_scientific_id(BrickMode::PixelPresent, false, computed),
            || true,
        )
        .unwrap_err();
        assert!(matches!(error, PackageWriteError::Cancelled));
        assert!(!cancelled.exists());
        assert!(!fs::read_dir(&root.0).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".mirante4d-stage-")
        }));
    }

    #[test]
    fn lazy_shard_input_stops_at_the_selected_profile_bound() {
        let root = TestDirectory::new("input-bound");
        let destination = root.0.join("bounded.m4d");
        let template = fixture_input(BrickMode::AllFill, false);
        let PackageWriteInput {
            profile_kind,
            profile,
            science,
            display_defaults,
            portable_records,
            ome_images,
            arrays,
            shards: _,
        } = template;
        let pixel_path = profile.images()[0].levels()[0].pixel_path().clone();
        let limits = profile_limits(profile_kind);
        let maximum = limits.pixel_shards + limits.validity_shards + limits.packed_index_shards;
        let yielded = Rc::new(Cell::new(0_u64));
        let observed = Rc::clone(&yielded);
        let shards = std::iter::from_fn(move || {
            observed.set(observed.get() + 1);
            Some(PackageShardInput::new(
                pixel_path.clone(),
                vec![0, 0, 0, 0, 0],
                missing_chunks(ShardProfileKind::Pixel2dUint8),
            ))
        });
        let input = PackageWriteInput::new(
            profile_kind,
            profile,
            science,
            display_defaults,
            portable_records,
            ome_images,
            arrays,
            shards,
        );

        let error = LocalPackageWriter::write_new(&destination, input, || false).unwrap_err();
        assert!(matches!(
            error,
            PackageWriteError::InvalidInput {
                reason: "shard inputs exceed the selected profile bound"
            }
        ));
        assert_eq!(yielded.get(), maximum + 1);
        assert!(!destination.exists());
        assert!(!fs::read_dir(&root.0).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".mirante4d-stage-")
        }));
    }

    fn fixture_input(mode: BrickMode, reverse: bool) -> PackageWriteInput<Vec<PackageShardInput>> {
        fixture_input_with_scientific_id(mode, reverse, scientific_id())
    }

    fn fixture_input_with_scientific_id(
        mode: BrickMode,
        reverse: bool,
        scientific_id: ScientificContentId,
    ) -> PackageWriteInput<Vec<PackageShardInput>> {
        let temporal = ScienceTemporalCalibration::regular(bits64("3ff0000000000000")).unwrap();
        let explicit = matches!(
            mode,
            BrickMode::ExplicitValidity | BrickMode::ExplicitAllInvalid
        );
        let validity_mode = if explicit {
            ProfileValidityMode::Explicit
        } else {
            ProfileValidityMode::AllValid
        };
        let level = ProfileLevel::new(0, 0, validity_mode).unwrap();
        let image = ProfileImage::new(
            0,
            vec![ProfileLogicalLayer::new(LogicalLayerKey::new(0), 0)],
            vec![level.clone()],
        )
        .unwrap();
        let profile = ProfileHeader::new(
            scientific_id,
            vec![image.clone()],
            0,
            if explicit {
                OmeInteroperabilityBase::Io1
            } else {
                OmeInteroperabilityBase::Io2
            },
        )
        .unwrap();
        let science = ScienceDescriptor::new(
            scientific_id,
            vec![
                ScienceLayer::new(
                    LogicalLayerKey::new(0),
                    Shape4D::new(1, 1, 2, 3).unwrap(),
                    IntensityDType::Uint8,
                    temporal.clone(),
                    identity_transform(),
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let display = DisplayDefaults::new(vec![
            DisplayLayerDefaults::new(
                LogicalLayerKey::new(0),
                true,
                Rgb24::parse("ffffff").unwrap(),
                F32Bits::parse("00000000").unwrap(),
                F32Bits::parse("3f800000").unwrap(),
            )
            .unwrap(),
        ])
        .unwrap();
        let ome = OmeImageGroupMetadata::new(
            &image,
            &temporal,
            vec![OmeLevelTransform::DiagonalMicrometer {
                scale_zyx: [bits64("3ff0000000000000"); 3],
                translation_zyx: [bits64("0000000000000000"); 3],
            }],
        )
        .unwrap();

        let mut arrays = vec![
            PackageArrayInput::new(
                level.pixel_path().clone(),
                ZarrArrayMetadata::new(ShardProfileKind::Pixel2dUint8, vec![1, 1, 1, 2, 3])
                    .unwrap(),
            ),
            PackageArrayInput::new(
                level.packed_index_path().clone(),
                ZarrArrayMetadata::new(ShardProfileKind::PackedIndex, vec![1, 64]).unwrap(),
            ),
        ];
        if let Some(path) = level.validity_path() {
            arrays.push(PackageArrayInput::new(
                path.clone(),
                ZarrArrayMetadata::new(ShardProfileKind::Validity2d, vec![1, 1, 1, 2, 1]).unwrap(),
            ));
        }

        let pixel_present = matches!(mode, BrickMode::PixelPresent | BrickMode::ExplicitValidity);
        let (statistics, explicit_validity) = match mode {
            BrickMode::PixelPresent => (PackedIndexStatistics::new(6, 2, Some((0, 2))), false),
            BrickMode::AllFill => (PackedIndexStatistics::new(6, 0, Some((0, 0))), false),
            BrickMode::ExplicitValidity => (PackedIndexStatistics::new(3, 2, Some((0, 2))), true),
            BrickMode::ExplicitAllInvalid => (PackedIndexStatistics::new(0, 0, None), true),
        };
        let record = PackedIndexRecord::new(
            PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0),
            statistics,
            pixel_present,
            explicit_validity,
            IntensityDType::Uint8,
            6,
        )
        .unwrap();

        let mut shards = Vec::new();
        if pixel_present {
            let kind = ShardProfileKind::Pixel2dUint8;
            let mut payload = vec![0; kind.decoded_inner_bytes()];
            payload[..6].copy_from_slice(&[0, 1, 2, 0, 0, 0]);
            let mut chunks = missing_chunks(kind);
            chunks[0] = Some(payload);
            shards.push(PackageShardInput::new(
                level.pixel_path().clone(),
                vec![0, 0, 0, 0, 0],
                chunks,
            ));
        }

        let packed_kind = ShardProfileKind::PackedIndex;
        let mut packed_payload = vec![0; packed_kind.decoded_inner_bytes()];
        packed_payload[..crate::PACKED_INDEX_RECORD_BYTES as usize]
            .copy_from_slice(&record.encode());
        let mut packed_chunks = missing_chunks(packed_kind);
        packed_chunks[0] = Some(packed_payload);
        shards.push(PackageShardInput::new(
            level.packed_index_path().clone(),
            vec![0, 0],
            packed_chunks,
        ));

        if matches!(mode, BrickMode::ExplicitValidity) {
            let kind = ShardProfileKind::Validity2d;
            let mut payload = vec![0; kind.decoded_inner_bytes()];
            payload[0] = 0b0000_0111;
            let mut chunks = missing_chunks(kind);
            chunks[0] = Some(payload);
            shards.push(PackageShardInput::new(
                level.validity_path().unwrap().clone(),
                vec![0, 0, 0, 0, 0],
                chunks,
            ));
        }
        if reverse {
            arrays.reverse();
            shards.reverse();
        }

        PackageWriteInput::new(
            ProfileKind::Ds0,
            profile,
            science,
            display,
            Vec::new(),
            vec![ome],
            arrays,
            shards,
        )
    }

    fn missing_chunks(kind: ShardProfileKind) -> Vec<Option<Vec<u8>>> {
        std::iter::repeat_with(|| None)
            .take(kind.chunks_per_shard())
            .collect()
    }

    fn scientific_id() -> ScientificContentId {
        ScientificContentId::parse(
            "m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap()
    }

    fn bits64(value: &str) -> F64Bits {
        F64Bits::parse(value).unwrap()
    }

    fn identity_transform() -> [F64Bits; 16] {
        [
            bits64("3ff0000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("3ff0000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("3ff0000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("3ff0000000000000"),
        ]
    }

    fn tree_bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
        fn visit(root: &Path, current: &Path, output: &mut BTreeMap<String, Vec<u8>>) {
            let mut entries = fs::read_dir(current)
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                if path.is_dir() {
                    visit(root, &path, output);
                } else {
                    let relative = path
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    output.insert(relative, fs::read(path).unwrap());
                }
            }
        }

        let mut output = BTreeMap::new();
        visit(root, root, &mut output);
        output
    }
}
