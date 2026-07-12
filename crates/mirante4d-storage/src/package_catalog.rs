use std::{collections::BTreeMap, path::Path};

use mirante4d_domain::IntensityDType;
use mirante4d_identity::{ExactBytesHasher, IdentityHashError, PackageId};
use thiserror::Error;

use crate::brick_address::plan_local_brick_address;
use crate::directory_inventory::{ExpectedFile, ExpectedFileRole, inspect_directory_closure};
use crate::package_admission::{DatasetProfileAdmissionInput, admit_dataset_profile};
use crate::package_integrity::{
    ExactPackageCapability, PackageIntegrityInput, PackageValidationError,
    validate_package_integrity,
};
use crate::package_structure::{
    PackageStructureError, PackageStructureInput, PackageStructureReport,
    reconcile_package_structure,
};
use crate::{
    ControlError, DisplayDefaults, LocalPackageReader, MAX_PORTABLE_CONTROL_OBJECT_BYTES,
    MAX_PROFILE_HEADER_BYTES, MAX_ZARR_METADATA_BYTES, ManifestPage, ManifestRoot,
    OmeImageGroupMetadata, OmeInteroperabilityBase, OmeLevelTransform, PackageObjectDescriptor,
    PackageObjectKind, PackagePath, ProfileHeader, ProfileValidityMode, RangeReadError,
    ScienceDescriptor, ScienceLayer, ScienceTemporalKind, ShardProfileKind, StorageProfileError,
    ZarrArrayMetadata, ZarrGroupMetadata, ZarrMetadataError,
};

const PROFILE_PATH: &str = "m4d/profile.json";
const MANIFEST_ROOT_PATH: &str = "m4d/manifest/root.json";

/// Authenticated, bounded metadata catalog for one local target-profile package.
///
/// This proves the canonical manifest root/pages and the opening-critical
/// profile, science, display, Zarr, and OME objects. Portable records,
/// shard-byte verification, and rejection of unlisted filesystem objects remain
/// explicit later validation modes.
#[derive(Debug)]
pub struct LocalPackageCatalog {
    reader: LocalPackageReader,
    declared_package_id: PackageId,
    manifest_root: ManifestRoot,
    manifest_root_bytes: u64,
    profile: ProfileHeader,
    science: ScienceDescriptor,
    display_defaults: DisplayDefaults,
    descriptors: Vec<PackageObjectDescriptor>,
    ome_images: BTreeMap<PackagePath, OmeImageGroupMetadata>,
    zarr_arrays: BTreeMap<PackagePath, ZarrArrayMetadata>,
    metadata_bytes_read: u64,
}

impl LocalPackageCatalog {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, PackageOpenError> {
        let reader = LocalPackageReader::open(root)?;
        let profile_path = fixed_path(PROFILE_PATH)?;
        let profile_bytes = reader.read_object(&profile_path, MAX_PROFILE_HEADER_BYTES as u64)?;
        let profile = ProfileHeader::parse_canonical(&profile_bytes)?;

        let root_path = fixed_path(MANIFEST_ROOT_PATH)?;
        let root_bytes =
            reader.read_object(&root_path, MAX_PORTABLE_CONTROL_OBJECT_BYTES as u64)?;
        let manifest_root = ManifestRoot::parse_canonical(&root_bytes)?;
        let manifest_root_bytes = u64::try_from(root_bytes.len())
            .map_err(|_| PackageOpenError::MetadataByteCountOverflow)?;
        let declared_package_id = manifest_root.package_id()?;
        let mut metadata_bytes_read = checked_add_bytes(0, profile_bytes.len())?;
        metadata_bytes_read = checked_add_bytes(metadata_bytes_read, root_bytes.len())?;

        let mut pages = Vec::with_capacity(manifest_root.pages().len());
        for reference in manifest_root.pages() {
            let bytes =
                reader.read_object(reference.path(), MAX_PORTABLE_CONTROL_OBJECT_BYTES as u64)?;
            metadata_bytes_read = checked_add_bytes(metadata_bytes_read, bytes.len())?;
            if u64::try_from(bytes.len()).ok() != Some(reference.byte_length()) {
                return Err(PackageOpenError::ManifestPageLengthMismatch {
                    path: reference.path().to_string(),
                    expected: reference.byte_length(),
                    actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                });
            }
            let facts = ExactBytesHasher::hash(&bytes)?;
            if facts.digest() != reference.digest() {
                return Err(PackageOpenError::ManifestPageDigestMismatch {
                    path: reference.path().to_string(),
                });
            }
            pages.push(ManifestPage::parse_canonical(&bytes)?);
        }
        manifest_root.verify_pages(&pages)?;
        let descriptors = pages
            .into_iter()
            .flat_map(|page| page.entries().to_vec())
            .collect::<Vec<_>>();

        let expected = expected_metadata_objects(&profile)?;
        validate_metadata_descriptor_set(&descriptors, &expected)?;

        let profile_descriptor = descriptor(&descriptors, &profile_path)?;
        verify_exact_bytes(profile_descriptor, &profile_bytes)?;

        let mut science = None;
        let mut display_defaults = None;
        let mut ome_images = BTreeMap::new();
        let mut zarr_arrays = BTreeMap::new();
        for (path, kind) in &expected {
            if matches!(
                kind,
                PackageObjectKind::Profile | PackageObjectKind::PortableRecord
            ) {
                continue;
            }
            let descriptor = descriptor(&descriptors, path)?;
            let bytes = read_verified_metadata(&reader, descriptor)?;
            metadata_bytes_read = checked_add_bytes(metadata_bytes_read, bytes.len())?;
            match kind {
                PackageObjectKind::ZarrRoot
                | PackageObjectKind::ZarrImagesGroup
                | PackageObjectKind::ZarrValidityGroup
                | PackageObjectKind::ZarrIndexesGroup => {
                    ZarrGroupMetadata::parse(&bytes)?;
                }
                PackageObjectKind::ZarrImageGroup => {
                    ome_images.insert(path.clone(), OmeImageGroupMetadata::parse(&bytes)?);
                }
                PackageObjectKind::ZarrPixelArray
                | PackageObjectKind::ZarrValidityArray
                | PackageObjectKind::ZarrPackedIndexArray => {
                    zarr_arrays.insert(path.clone(), ZarrArrayMetadata::parse(&bytes)?);
                }
                PackageObjectKind::Science => {
                    science = Some(ScienceDescriptor::parse_canonical(&bytes)?);
                }
                PackageObjectKind::DisplayDefaults => {
                    display_defaults = Some(DisplayDefaults::parse_canonical(&bytes)?);
                }
                PackageObjectKind::Profile
                | PackageObjectKind::PortableRecord
                | PackageObjectKind::PixelShard
                | PackageObjectKind::ValidityShard
                | PackageObjectKind::PackedIndexShard => {
                    return Err(PackageOpenError::UnexpectedMetadataObject {
                        path: path.to_string(),
                    });
                }
            }
        }
        let science = science.ok_or_else(|| PackageOpenError::MissingMetadataObject {
            path: profile.science_path().to_string(),
        })?;
        let display_defaults =
            display_defaults.ok_or_else(|| PackageOpenError::MissingMetadataObject {
                path: profile.display_defaults_path().to_string(),
            })?;
        validate_cross_object_metadata(&profile, &science, &display_defaults, &ome_images)?;
        validate_storage_metadata(&profile, &science, &ome_images, &zarr_arrays)?;

        Ok(Self {
            reader,
            declared_package_id,
            manifest_root,
            manifest_root_bytes,
            profile,
            science,
            display_defaults,
            descriptors,
            ome_images,
            zarr_arrays,
            metadata_bytes_read,
        })
    }

    /// Returns the ID declared by the canonical manifest root.
    ///
    /// The actual package closure does not own this identity until every
    /// descriptor payload passes full SHA-256 validation.
    pub const fn declared_package_id(&self) -> PackageId {
        self.declared_package_id
    }

    pub const fn profile(&self) -> &ProfileHeader {
        &self.profile
    }

    pub const fn science(&self) -> &ScienceDescriptor {
        &self.science
    }

    pub const fn display_defaults(&self) -> &DisplayDefaults {
        &self.display_defaults
    }

    pub fn descriptors(&self) -> &[PackageObjectDescriptor] {
        &self.descriptors
    }

    pub fn descriptor(&self, path: &PackagePath) -> Option<&PackageObjectDescriptor> {
        self.descriptors
            .binary_search_by(|entry| entry.path().cmp(path))
            .ok()
            .map(|index| &self.descriptors[index])
    }

    pub fn ome_image(&self, path: &PackagePath) -> Option<&OmeImageGroupMetadata> {
        self.ome_images.get(path)
    }

    pub fn zarr_array(&self, path: &PackagePath) -> Option<&ZarrArrayMetadata> {
        self.zarr_arrays.get(path)
    }

    pub const fn metadata_bytes_read(&self) -> u64 {
        self.metadata_bytes_read
    }

    /// Applies one caller-selected DS profile to exact addressing and
    /// directory facts. This never infers a profile or validates payload SHA.
    pub fn admit_dataset_profile(
        &self,
        requested: crate::ProfileKind,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<crate::DatasetProfileAdmission, crate::PackageAdmissionError> {
        let inventory = self.inspect_directory_closure(&mut is_cancelled)?;
        admit_dataset_profile(
            DatasetProfileAdmissionInput {
                profile: &self.profile,
                science: &self.science,
                arrays: &self.zarr_arrays,
                descriptors: &self.descriptors,
                inventory,
            },
            requested,
            &mut is_cancelled,
        )
    }

    /// Consumes this catalog and issues the sole exact-package capability.
    ///
    /// Validation performs explicit DS admission, whole-package structural
    /// reconciliation, streaming SHA-256 closure over the manifest authority
    /// and every descriptor object, a fresh exact inventory, and a final
    /// identity sweep. No capability is returned after cancellation or
    /// detected drift; the sweep is sequential rather than an atomic
    /// filesystem snapshot.
    pub fn validate_exact_package(
        self,
        requested: crate::ProfileKind,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<ExactPackageCapability, PackageValidationError> {
        let admission = self
            .admit_dataset_profile(requested, &mut is_cancelled)
            .map_err(map_admission_validation_error)?;
        let report = reconcile_package_structure(
            PackageStructureInput {
                reader: &self.reader,
                profile: &self.profile,
                arrays: &self.zarr_arrays,
                descriptors: &self.descriptors,
                admission,
            },
            &mut is_cancelled,
        )
        .map_err(map_structure_validation_error)?;
        self.inspect_directory_closure(&mut is_cancelled)
            .map_err(map_inventory_validation_error)?;
        report
            .revalidate_snapshots(&self.reader, &mut is_cancelled)
            .map_err(map_structure_validation_error)?;

        let proof = validate_package_integrity(
            PackageIntegrityInput {
                reader: &self.reader,
                manifest_root_path: self.profile.manifest_root_path(),
                manifest_root_bytes: self.manifest_root_bytes,
                manifest_root: &self.manifest_root,
                package_id: self.declared_package_id,
                descriptors: &self.descriptors,
                structure: &report,
            },
            &mut is_cancelled,
        )?;
        self.inspect_directory_closure(&mut is_cancelled)
            .map_err(map_inventory_validation_error)?;
        proof.revalidate_all(&self.reader, &mut is_cancelled)?;
        Ok(ExactPackageCapability::new(self, admission, proof))
    }

    /// Derives the sole bounded storage address plan for one logical brick.
    pub fn plan_brick_storage(
        &self,
        coordinates: crate::PackedIndexCoordinates,
    ) -> Result<crate::LocalBrickAddressPlan, crate::BrickAddressError> {
        plan_local_brick_address(
            &self.profile,
            &self.zarr_arrays,
            &self.descriptors,
            coordinates,
        )
    }

    pub(crate) const fn reader(&self) -> &LocalPackageReader {
        &self.reader
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn validate_package_structure(
        &self,
        requested: crate::ProfileKind,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<crate::DatasetProfileAdmission, PackageStructureError> {
        self.validate_package_structure_with_report(requested, &mut is_cancelled)
            .map(|(admission, _report)| admission)
    }

    fn validate_package_structure_with_report(
        &self,
        requested: crate::ProfileKind,
        is_cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(crate::DatasetProfileAdmission, PackageStructureReport), PackageStructureError>
    {
        let admission = self
            .admit_dataset_profile(requested, &mut *is_cancelled)
            .map_err(PackageStructureError::from)?;
        let report = reconcile_package_structure(
            PackageStructureInput {
                reader: &self.reader,
                profile: &self.profile,
                arrays: &self.zarr_arrays,
                descriptors: &self.descriptors,
                admission,
            },
            &mut *is_cancelled,
        )?;
        self.inspect_directory_closure(&mut *is_cancelled)
            .map_err(crate::PackageAdmissionError::from)
            .map_err(PackageStructureError::from)?;
        report.revalidate_snapshots(&self.reader, &mut *is_cancelled)?;
        Ok((admission, report))
    }

    #[cfg(test)]
    fn reconcile_structure_for_test(
        &self,
        requested: crate::ProfileKind,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<PackageStructureReport, PackageStructureError> {
        self.validate_package_structure_with_report(requested, &mut is_cancelled)
            .map(|(_admission, report)| report)
    }

    #[cfg(test)]
    fn read_brick_core_for_test(
        &self,
        coordinates: crate::PackedIndexCoordinates,
    ) -> Result<crate::LocalBrickRead, crate::PackageReadError> {
        let plan = self.plan_brick_storage(coordinates)?;
        crate::package_read::read_local_brick(&self.reader, &self.descriptors, plan)
    }

    /// Inspects the exact finalized file and ancestor-directory closure.
    ///
    /// This operation opens no payload bytes, hashes no shards, and does not
    /// select a DS-specific admission profile. It is globally bounded and
    /// checks `is_cancelled` before and throughout filesystem enumeration.
    pub fn inspect_directory_closure(
        &self,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<crate::DirectoryInventory, crate::DirectoryInventoryError> {
        self.reauthenticate_manifest_authority(&mut is_cancelled)?;
        let mut expected = BTreeMap::new();
        for descriptor in &self.descriptors {
            insert_inventory_file(
                &mut expected,
                descriptor.path().clone(),
                ExpectedFile {
                    bytes: descriptor.raw().byte_length(),
                    role: ExpectedFileRole::Descriptor(descriptor.kind()),
                },
            )?;
        }
        insert_inventory_file(
            &mut expected,
            PackagePath::parse(MANIFEST_ROOT_PATH)?,
            ExpectedFile {
                bytes: self.manifest_root_bytes,
                role: ExpectedFileRole::ManifestRoot,
            },
        )?;
        for page in self.manifest_root.pages() {
            insert_inventory_file(
                &mut expected,
                page.path().clone(),
                ExpectedFile {
                    bytes: page.byte_length(),
                    role: ExpectedFileRole::ManifestPage,
                },
            )?;
        }
        let inventory = inspect_directory_closure(&self.reader, expected, &mut is_cancelled)?;
        self.reauthenticate_manifest_authority(&mut is_cancelled)?;
        Ok(inventory)
    }

    fn reauthenticate_manifest_authority(
        &self,
        is_cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), crate::DirectoryInventoryError> {
        self.reauthenticate_manifest_root(is_cancelled)?;
        for page in self.manifest_root.pages() {
            if is_cancelled() {
                return Err(crate::DirectoryInventoryError::Cancelled);
            }
            let bytes = self
                .reader
                .read_object(page.path(), MAX_PORTABLE_CONTROL_OBJECT_BYTES as u64)?;
            let length_matches = u64::try_from(bytes.len()).ok() == Some(page.byte_length());
            let digest_matches = ExactBytesHasher::hash(&bytes)
                .map(|facts| facts.digest() == page.digest())
                .unwrap_or(false);
            if !length_matches || !digest_matches {
                return Err(crate::DirectoryInventoryError::ManifestAuthorityChanged);
            }
        }
        self.reauthenticate_manifest_root(is_cancelled)
    }

    fn reauthenticate_manifest_root(
        &self,
        is_cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), crate::DirectoryInventoryError> {
        if is_cancelled() {
            return Err(crate::DirectoryInventoryError::Cancelled);
        }
        let path = PackagePath::parse(MANIFEST_ROOT_PATH)?;
        let bytes = self
            .reader
            .read_object(&path, MAX_PORTABLE_CONTROL_OBJECT_BYTES as u64)?;
        if u64::try_from(bytes.len()).ok() != Some(self.manifest_root_bytes)
            || ManifestRoot::parse_canonical(&bytes).ok().as_ref() != Some(&self.manifest_root)
        {
            return Err(crate::DirectoryInventoryError::ManifestAuthorityChanged);
        }
        Ok(())
    }
}

fn map_admission_validation_error(error: crate::PackageAdmissionError) -> PackageValidationError {
    if matches!(
        error,
        crate::PackageAdmissionError::Inventory(crate::DirectoryInventoryError::Cancelled)
    ) {
        PackageValidationError::Cancelled
    } else {
        PackageValidationError::Structure(PackageStructureError::Admission(error))
    }
}

fn map_structure_validation_error(error: PackageStructureError) -> PackageValidationError {
    if matches!(
        error,
        PackageStructureError::Cancelled
            | PackageStructureError::Admission(crate::PackageAdmissionError::Inventory(
                crate::DirectoryInventoryError::Cancelled
            ))
    ) {
        PackageValidationError::Cancelled
    } else {
        PackageValidationError::Structure(error)
    }
}

fn map_inventory_validation_error(error: crate::DirectoryInventoryError) -> PackageValidationError {
    if error == crate::DirectoryInventoryError::Cancelled {
        PackageValidationError::Cancelled
    } else {
        PackageValidationError::Inventory(error)
    }
}

fn insert_inventory_file(
    expected: &mut BTreeMap<PackagePath, ExpectedFile>,
    path: PackagePath,
    file: ExpectedFile,
) -> Result<(), crate::DirectoryInventoryError> {
    if expected.insert(path.clone(), file).is_some() {
        return Err(crate::DirectoryInventoryError::Path(
            StorageProfileError::DuplicatePath {
                path: path.to_string(),
            },
        ));
    }
    Ok(())
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PackageOpenError {
    #[error(transparent)]
    Path(#[from] StorageProfileError),
    #[error(transparent)]
    Range(#[from] RangeReadError),
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error(transparent)]
    Metadata(#[from] ZarrMetadataError),
    #[error(transparent)]
    IdentityHash(#[from] IdentityHashError),
    #[error("metadata byte accounting overflowed u64")]
    MetadataByteCountOverflow,
    #[error("manifest page {path} has {actual} bytes; expected {expected}")]
    ManifestPageLengthMismatch {
        path: String,
        expected: u64,
        actual: u64,
    },
    #[error("manifest page {path} does not match its authenticated digest")]
    ManifestPageDigestMismatch { path: String },
    #[error("required metadata object {path} is absent from the manifest")]
    MissingMetadataObject { path: String },
    #[error("manifest contains unexpected metadata object {path}")]
    UnexpectedMetadataObject { path: String },
    #[error("metadata object {path} has the wrong registered kind")]
    MetadataKindMismatch { path: String },
    #[error("object {path} has {actual} bytes; manifest declares {expected}")]
    ObjectLengthMismatch {
        path: String,
        expected: u64,
        actual: u64,
    },
    #[error("object {path} does not match its manifest digest")]
    ObjectDigestMismatch { path: String },
    #[error("package metadata is cross-object inconsistent: {reason}")]
    CrossObjectInconsistency { reason: &'static str },
}

fn expected_metadata_objects(
    profile: &ProfileHeader,
) -> Result<BTreeMap<PackagePath, PackageObjectKind>, PackageOpenError> {
    let mut expected = BTreeMap::new();
    for (path, kind) in [
        ("zarr.json", PackageObjectKind::ZarrRoot),
        ("images/zarr.json", PackageObjectKind::ZarrImagesGroup),
        ("validity/zarr.json", PackageObjectKind::ZarrValidityGroup),
        ("indexes/zarr.json", PackageObjectKind::ZarrIndexesGroup),
        (PROFILE_PATH, PackageObjectKind::Profile),
        ("m4d/science.json", PackageObjectKind::Science),
        ("m4d/display.json", PackageObjectKind::DisplayDefaults),
    ] {
        insert_expected(&mut expected, fixed_path(path)?, kind)?;
    }
    for path in profile.portable_record_paths() {
        insert_expected(
            &mut expected,
            path.clone(),
            PackageObjectKind::PortableRecord,
        )?;
    }
    for image in profile.images() {
        insert_expected(
            &mut expected,
            metadata_path(image.image_group_path())?,
            PackageObjectKind::ZarrImageGroup,
        )?;
        for level in image.levels() {
            insert_expected(
                &mut expected,
                metadata_path(level.pixel_path())?,
                PackageObjectKind::ZarrPixelArray,
            )?;
            if let Some(path) = level.validity_path() {
                insert_expected(
                    &mut expected,
                    metadata_path(path)?,
                    PackageObjectKind::ZarrValidityArray,
                )?;
            }
            insert_expected(
                &mut expected,
                metadata_path(level.packed_index_path())?,
                PackageObjectKind::ZarrPackedIndexArray,
            )?;
        }
    }
    Ok(expected)
}

fn insert_expected(
    expected: &mut BTreeMap<PackagePath, PackageObjectKind>,
    path: PackagePath,
    kind: PackageObjectKind,
) -> Result<(), PackageOpenError> {
    if expected.insert(path.clone(), kind).is_some() {
        return Err(PackageOpenError::CrossObjectInconsistency {
            reason: "profile maps two metadata roles to one package path",
        });
    }
    Ok(())
}

fn validate_metadata_descriptor_set(
    descriptors: &[PackageObjectDescriptor],
    expected: &BTreeMap<PackagePath, PackageObjectKind>,
) -> Result<(), PackageOpenError> {
    for descriptor in descriptors {
        if is_shard(descriptor.kind()) {
            continue;
        }
        match expected.get(descriptor.path()) {
            None => {
                return Err(PackageOpenError::UnexpectedMetadataObject {
                    path: descriptor.path().to_string(),
                });
            }
            Some(kind) if *kind != descriptor.kind() => {
                return Err(PackageOpenError::MetadataKindMismatch {
                    path: descriptor.path().to_string(),
                });
            }
            Some(_) => {}
        }
    }
    for path in expected.keys() {
        descriptor(descriptors, path)?;
    }
    Ok(())
}

fn validate_cross_object_metadata(
    profile: &ProfileHeader,
    science: &ScienceDescriptor,
    display: &DisplayDefaults,
    ome_images: &BTreeMap<PackagePath, OmeImageGroupMetadata>,
) -> Result<(), PackageOpenError> {
    if profile.scientific_content_id() != science.scientific_content_id() {
        return cross_object("profile and science identifiers differ");
    }
    let profile_layers = profile
        .images()
        .iter()
        .flat_map(|image| image.logical_layers())
        .map(|layer| layer.logical_layer())
        .collect::<Vec<_>>();
    let science_layers = science
        .layers()
        .iter()
        .map(|layer| layer.logical_layer())
        .collect::<Vec<_>>();
    let display_layers = display
        .layers()
        .iter()
        .map(|layer| layer.logical_layer())
        .collect::<Vec<_>>();
    if profile_layers != science_layers || profile_layers != display_layers {
        return cross_object("profile, science, and display layer sets differ");
    }

    for image in profile.images() {
        let ome_path = metadata_path(image.image_group_path())?;
        let ome =
            ome_images
                .get(&ome_path)
                .ok_or_else(|| PackageOpenError::MissingMetadataObject {
                    path: ome_path.to_string(),
                })?;
        if ome.level_transforms().len() != image.levels().len() {
            return cross_object("OME and profile image level counts differ");
        }
        let first = image.logical_layers()[0].logical_layer().ordinal() as usize;
        let reference = &science.layers()[first];
        if ome.regular_time_step_seconds()
            != reference.temporal_calibration().regular_step_seconds()
        {
            return cross_object("OME and science time calibration differ");
        }
        for mapping in &image.logical_layers()[1..] {
            let layer = &science.layers()[mapping.logical_layer().ordinal() as usize];
            if layer.base_shape() != reference.base_shape()
                || layer.dtype() != reference.dtype()
                || layer.temporal_calibration() != reference.temporal_calibration()
                || layer.grid_to_world_micrometer_f64_bits()
                    != reference.grid_to_world_micrometer_f64_bits()
            {
                return cross_object("one physical image maps layers with different science grids");
            }
        }
    }
    Ok(())
}

fn validate_storage_metadata(
    profile: &ProfileHeader,
    science: &ScienceDescriptor,
    ome_images: &BTreeMap<PackagePath, OmeImageGroupMetadata>,
    zarr_arrays: &BTreeMap<PackagePath, ZarrArrayMetadata>,
) -> Result<(), PackageOpenError> {
    for image in profile.images() {
        let first = image.logical_layers()[0].logical_layer().ordinal() as usize;
        let science_layer = &science.layers()[first];
        let base_shape = science_layer.base_shape();
        let two_dimensional = base_shape.z() == 1;
        let channel_count = u64::try_from(image.logical_layers().len()).map_err(|_| {
            PackageOpenError::CrossObjectInconsistency {
                reason: "physical channel count exceeds u64",
            }
        })?;
        let mut physical_channels = image
            .logical_layers()
            .iter()
            .map(|layer| layer.physical_channel())
            .collect::<Vec<_>>();
        physical_channels.sort_unstable();
        if !physical_channels
            .iter()
            .enumerate()
            .all(|(expected, channel)| usize::try_from(*channel).ok() == Some(expected))
        {
            return cross_object("physical channels must exactly cover zero through c-1");
        }

        let ome_path = metadata_path(image.image_group_path())?;
        let ome = &ome_images[&ome_path];
        let expected_base_transform = ome_base_transform(science_layer);
        if ome.level_transforms()[0] != expected_base_transform {
            return cross_object("base OME and science spatial transforms differ");
        }
        if profile.ome_interoperability_base() == OmeInteroperabilityBase::Io2
            && (science_layer.temporal_calibration().kind() != ScienceTemporalKind::Regular
                || expected_base_transform == OmeLevelTransform::UnitlessIdentity)
        {
            return cross_object("IO-2 requires regular time and diagonal micrometer geometry");
        }

        let mut spatial = [base_shape.z(), base_shape.y(), base_shape.x()];
        for level in image.levels() {
            let pixel_shape = [
                base_shape.t(),
                channel_count,
                spatial[0],
                spatial[1],
                spatial[2],
            ];
            let pixel_path = metadata_path(level.pixel_path())?;
            let pixel = required_array(zarr_arrays, &pixel_path)?;
            if pixel.kind() != pixel_kind(science_layer.dtype(), two_dimensional)
                || pixel.shape() != pixel_shape
            {
                return cross_object("pixel Zarr dtype or shape differs from science metadata");
            }

            if level.validity_mode() == ProfileValidityMode::Explicit {
                let validity_path = metadata_path(level.validity_path().ok_or(
                    PackageOpenError::CrossObjectInconsistency {
                        reason: "explicit validity has no profile path",
                    },
                )?)?;
                let validity = required_array(zarr_arrays, &validity_path)?;
                let validity_shape = [
                    base_shape.t(),
                    channel_count,
                    spatial[0],
                    spatial[1],
                    ceil_divide(spatial[2], 8),
                ];
                let expected_kind = if two_dimensional {
                    ShardProfileKind::Validity2d
                } else {
                    ShardProfileKind::Validity3d
                };
                if validity.kind() != expected_kind || validity.shape() != validity_shape {
                    return cross_object("validity Zarr shape is not t,c,z,y,ceil(x/8)");
                }
            }

            let record_count = checked_record_count(pixel_shape, two_dimensional)?;
            let packed_path = metadata_path(level.packed_index_path())?;
            let packed = required_array(zarr_arrays, &packed_path)?;
            if packed.kind() != ShardProfileKind::PackedIndex
                || packed.shape() != [record_count, 64]
            {
                return cross_object("packed-index record count differs from logical bricks");
            }

            spatial = spatial.map(ceil_divide_by_two);
        }
    }
    Ok(())
}

fn required_array<'a>(
    arrays: &'a BTreeMap<PackagePath, ZarrArrayMetadata>,
    path: &PackagePath,
) -> Result<&'a ZarrArrayMetadata, PackageOpenError> {
    arrays
        .get(path)
        .ok_or_else(|| PackageOpenError::MissingMetadataObject {
            path: path.to_string(),
        })
}

fn pixel_kind(dtype: IntensityDType, two_dimensional: bool) -> ShardProfileKind {
    match (dtype, two_dimensional) {
        (IntensityDType::Uint8, false) => ShardProfileKind::Pixel3dUint8,
        (IntensityDType::Uint16, false) => ShardProfileKind::Pixel3dUint16,
        (IntensityDType::Float32, false) => ShardProfileKind::Pixel3dFloat32,
        (IntensityDType::Uint8, true) => ShardProfileKind::Pixel2dUint8,
        (IntensityDType::Uint16, true) => ShardProfileKind::Pixel2dUint16,
        (IntensityDType::Float32, true) => ShardProfileKind::Pixel2dFloat32,
    }
}

fn ome_base_transform(layer: &ScienceLayer) -> OmeLevelTransform {
    let matrix = layer.grid_to_world_micrometer_f64_bits();
    if [1, 2, 4, 6, 8, 9]
        .into_iter()
        .all(|index| matrix[index].bits() == 0)
    {
        OmeLevelTransform::DiagonalMicrometer {
            scale_zyx: [matrix[10], matrix[5], matrix[0]],
            translation_zyx: [matrix[11], matrix[7], matrix[3]],
        }
    } else {
        OmeLevelTransform::UnitlessIdentity
    }
}

fn checked_record_count(
    pixel_shape: [u64; 5],
    two_dimensional: bool,
) -> Result<u64, PackageOpenError> {
    let brick = if two_dimensional {
        [1, 1, 1, 256, 256]
    } else {
        [1, 1, 64, 64, 64]
    };
    let counts = pixel_shape
        .into_iter()
        .zip(brick)
        .map(|(dimension, chunk)| ceil_divide(dimension, chunk))
        .collect::<Vec<_>>();
    const MAX_COORDINATE_COUNT: u64 = u32::MAX as u64 + 1;
    if counts.iter().any(|count| *count > MAX_COORDINATE_COUNT) {
        return Err(PackageOpenError::CrossObjectInconsistency {
            reason: "logical brick grid exceeds packed-index u32 coordinates",
        });
    }
    counts
        .into_iter()
        .try_fold(1_u64, |count, dimension| count.checked_mul(dimension))
        .ok_or(PackageOpenError::CrossObjectInconsistency {
            reason: "logical brick count overflowed u64",
        })
}

const fn ceil_divide(value: u64, divisor: u64) -> u64 {
    value / divisor + if value.is_multiple_of(divisor) { 0 } else { 1 }
}

const fn ceil_divide_by_two(value: u64) -> u64 {
    value / 2 + value % 2
}

fn read_verified_metadata(
    reader: &LocalPackageReader,
    descriptor: &PackageObjectDescriptor,
) -> Result<Vec<u8>, PackageOpenError> {
    let maximum = match descriptor.kind() {
        PackageObjectKind::Profile => MAX_PROFILE_HEADER_BYTES as u64,
        PackageObjectKind::PixelShard
        | PackageObjectKind::ValidityShard
        | PackageObjectKind::PackedIndexShard => {
            return Err(PackageOpenError::UnexpectedMetadataObject {
                path: descriptor.path().to_string(),
            });
        }
        _ => MAX_ZARR_METADATA_BYTES as u64,
    };
    let bytes = reader.read_object(descriptor.path(), maximum)?;
    verify_exact_bytes(descriptor, &bytes)?;
    Ok(bytes)
}

fn verify_exact_bytes(
    descriptor: &PackageObjectDescriptor,
    bytes: &[u8],
) -> Result<(), PackageOpenError> {
    let actual =
        u64::try_from(bytes.len()).map_err(|_| PackageOpenError::MetadataByteCountOverflow)?;
    if actual != descriptor.raw().byte_length() {
        return Err(PackageOpenError::ObjectLengthMismatch {
            path: descriptor.path().to_string(),
            expected: descriptor.raw().byte_length(),
            actual,
        });
    }
    if ExactBytesHasher::hash(bytes)?.digest() != descriptor.raw().digest() {
        return Err(PackageOpenError::ObjectDigestMismatch {
            path: descriptor.path().to_string(),
        });
    }
    Ok(())
}

fn descriptor<'a>(
    descriptors: &'a [PackageObjectDescriptor],
    path: &PackagePath,
) -> Result<&'a PackageObjectDescriptor, PackageOpenError> {
    descriptors
        .binary_search_by(|entry| entry.path().cmp(path))
        .ok()
        .map(|index| &descriptors[index])
        .ok_or_else(|| PackageOpenError::MissingMetadataObject {
            path: path.to_string(),
        })
}

const fn is_shard(kind: PackageObjectKind) -> bool {
    matches!(
        kind,
        PackageObjectKind::PixelShard
            | PackageObjectKind::ValidityShard
            | PackageObjectKind::PackedIndexShard
    )
}

fn fixed_path(path: &str) -> Result<PackagePath, PackageOpenError> {
    Ok(PackagePath::parse(path)?)
}

fn metadata_path(path: &PackagePath) -> Result<PackagePath, PackageOpenError> {
    fixed_path(&format!("{path}/zarr.json"))
}

fn checked_add_bytes(current: u64, bytes: usize) -> Result<u64, PackageOpenError> {
    current
        .checked_add(u64::try_from(bytes).map_err(|_| PackageOpenError::MetadataByteCountOverflow)?)
        .ok_or(PackageOpenError::MetadataByteCountOverflow)
}

fn cross_object<T>(reason: &'static str) -> Result<T, PackageOpenError> {
    Err(PackageOpenError::CrossObjectInconsistency { reason })
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        collections::BTreeMap,
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use mirante4d_domain::{IntensityDType, LogicalLayerKey, Shape4D};
    use mirante4d_identity::{ExactBytesHasher, ScientificContentId};

    use super::*;
    use crate::{
        F32Bits, F64Bits, OmeInteroperabilityBase, OmeLevelTransform, PackageReadError,
        ProfileLevel, ProfileLogicalLayer, ProfileValidityMode, Rgb24, ScienceLayer,
        ScienceTemporalCalibration, ShardProfileKind,
    };

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    #[derive(Clone, Copy)]
    enum FixtureDrift {
        None,
        EmptyDisplay,
        ExtraMetadata,
        WrongPixelShape,
        UnitlessOme,
    }

    #[derive(Clone, Copy)]
    enum BrickFixture {
        PixelPresent,
        AllFill,
        AllFillListedEmpty,
        AllFillUnexpectedPixel,
        ExplicitValidity,
        ExplicitAllInvalid,
        ExplicitAllInvalidUnexpectedValidity,
        MissingPixelInner,
        MissingPixelShard,
        MissingPackedInner,
        PackedUnusedInner,
        PackedPaddingNonzero,
        OutOfGridPixelInner,
        PackedCoordinateMismatch,
        PackedValidityMismatch,
        OutOfGridPixelShard,
        MissingPackedShard,
    }

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "mirante4d-catalog-{}-{nonce}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn write(&self, path: &str, bytes: &[u8]) {
            let full = self.0.join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, bytes).unwrap();
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
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

    fn scientific_id() -> ScientificContentId {
        ScientificContentId::parse(
            "m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap()
    }

    fn fixture_shards(brick: BrickFixture) -> (Option<Vec<u8>>, Vec<u8>, Option<Vec<u8>>) {
        let pixel_kind = ShardProfileKind::Pixel2dUint8;
        let mut pixel_payload = vec![0; pixel_kind.decoded_inner_bytes()];
        pixel_payload[..6].copy_from_slice(&[0, 1, 2, 0, 0, 0]);
        let mut pixel_chunks = vec![None; pixel_kind.chunks_per_shard()];
        let all_fill = matches!(
            brick,
            BrickFixture::AllFill
                | BrickFixture::AllFillListedEmpty
                | BrickFixture::AllFillUnexpectedPixel
        );
        let explicit_all_invalid = matches!(
            brick,
            BrickFixture::ExplicitAllInvalid | BrickFixture::ExplicitAllInvalidUnexpectedValidity
        );
        if !matches!(
            brick,
            BrickFixture::AllFill
                | BrickFixture::AllFillListedEmpty
                | BrickFixture::ExplicitAllInvalid
                | BrickFixture::ExplicitAllInvalidUnexpectedValidity
                | BrickFixture::MissingPixelInner
                | BrickFixture::MissingPixelShard
        ) {
            pixel_chunks[0] = Some(pixel_payload.as_slice());
        }
        if matches!(brick, BrickFixture::OutOfGridPixelInner) {
            pixel_chunks[1] = Some(pixel_payload.as_slice());
        }
        let pixel_shard = (!matches!(
            brick,
            BrickFixture::AllFill
                | BrickFixture::ExplicitAllInvalid
                | BrickFixture::ExplicitAllInvalidUnexpectedValidity
                | BrickFixture::MissingPixelShard
        ))
        .then(|| crate::shard::assemble_shard(pixel_kind, &pixel_chunks).unwrap());

        let packed_kind = ShardProfileKind::PackedIndex;
        let mut packed_payload = vec![0; packed_kind.decoded_inner_bytes()];
        let coordinates = if matches!(brick, BrickFixture::PackedCoordinateMismatch) {
            crate::PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 1)
        } else {
            crate::PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0)
        };
        let explicit_validity_record = matches!(
            brick,
            BrickFixture::ExplicitValidity
                | BrickFixture::ExplicitAllInvalid
                | BrickFixture::ExplicitAllInvalidUnexpectedValidity
                | BrickFixture::PackedValidityMismatch
        );
        let (statistics, pixel_present) = if all_fill {
            (crate::PackedIndexStatistics::new(6, 0, Some((0, 0))), false)
        } else if explicit_all_invalid {
            (crate::PackedIndexStatistics::new(0, 0, None), false)
        } else if matches!(brick, BrickFixture::ExplicitValidity) {
            (crate::PackedIndexStatistics::new(3, 2, Some((0, 2))), true)
        } else {
            (crate::PackedIndexStatistics::new(6, 2, Some((0, 2))), true)
        };
        let record = crate::PackedIndexRecord::new(
            coordinates,
            statistics,
            pixel_present,
            explicit_validity_record,
            IntensityDType::Uint8,
            6,
        )
        .unwrap();
        packed_payload[..crate::PACKED_INDEX_RECORD_BYTES as usize]
            .copy_from_slice(&record.encode());
        if matches!(brick, BrickFixture::PackedPaddingNonzero) {
            packed_payload[crate::PACKED_INDEX_RECORD_BYTES as usize] = 1;
        }
        let mut packed_chunks = vec![None; packed_kind.chunks_per_shard()];
        if !matches!(brick, BrickFixture::MissingPackedInner) {
            packed_chunks[0] = Some(packed_payload.as_slice());
        }
        let unused_packed_payload = vec![0; packed_kind.decoded_inner_bytes()];
        if matches!(brick, BrickFixture::PackedUnusedInner) {
            packed_chunks[1] = Some(unused_packed_payload.as_slice());
        }
        let packed_shard = crate::shard::assemble_shard(packed_kind, &packed_chunks).unwrap();

        let validity_shard = matches!(
            brick,
            BrickFixture::ExplicitValidity | BrickFixture::ExplicitAllInvalidUnexpectedValidity
        )
        .then(|| {
            let kind = ShardProfileKind::Validity2d;
            let mut payload = vec![0; kind.decoded_inner_bytes()];
            payload[0] = 0b0000_0111;
            let mut chunks = vec![None; kind.chunks_per_shard()];
            chunks[0] = Some(payload.as_slice());
            crate::shard::assemble_shard(kind, &chunks).unwrap()
        });
        (pixel_shard, packed_shard, validity_shard)
    }

    fn fixture(drift: FixtureDrift) -> TempRoot {
        fixture_with_brick(drift, BrickFixture::PixelPresent)
    }

    fn fixture_with_brick(drift: FixtureDrift, brick: BrickFixture) -> TempRoot {
        let root = TempRoot::new();
        let temporal = ScienceTemporalCalibration::regular(bits64("3ff0000000000000")).unwrap();
        let explicit_validity = matches!(
            brick,
            BrickFixture::ExplicitValidity
                | BrickFixture::ExplicitAllInvalid
                | BrickFixture::ExplicitAllInvalidUnexpectedValidity
        );
        let validity_mode = if explicit_validity {
            ProfileValidityMode::Explicit
        } else {
            ProfileValidityMode::AllValid
        };
        let profile_level = ProfileLevel::new(0, 0, validity_mode).unwrap();
        let image = crate::ProfileImage::new(
            0,
            vec![ProfileLogicalLayer::new(LogicalLayerKey::new(0), 0)],
            vec![profile_level],
        )
        .unwrap();
        let (pixel_shard, packed_shard, validity_shard) = fixture_shards(brick);
        let profile = ProfileHeader::new(
            scientific_id(),
            vec![image.clone()],
            0,
            if explicit_validity {
                OmeInteroperabilityBase::Io1
            } else {
                OmeInteroperabilityBase::Io2
            },
        )
        .unwrap();
        let science = ScienceDescriptor::new(
            scientific_id(),
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
        let display = DisplayDefaults::new(if matches!(drift, FixtureDrift::EmptyDisplay) {
            Vec::new()
        } else {
            vec![
                crate::DisplayLayerDefaults::new(
                    LogicalLayerKey::new(0),
                    true,
                    Rgb24::parse("ffffff").unwrap(),
                    F32Bits::parse("00000000").unwrap(),
                    F32Bits::parse("3f800000").unwrap(),
                )
                .unwrap(),
            ]
        })
        .unwrap();
        let ome = OmeImageGroupMetadata::new(
            &image,
            &temporal,
            vec![if matches!(drift, FixtureDrift::UnitlessOme) {
                OmeLevelTransform::UnitlessIdentity
            } else {
                OmeLevelTransform::DiagonalMicrometer {
                    scale_zyx: [bits64("3ff0000000000000"); 3],
                    translation_zyx: [bits64("0000000000000000"); 3],
                }
            }],
        )
        .unwrap();

        let group = ZarrGroupMetadata::new().deterministic_bytes().unwrap();
        let mut objects = BTreeMap::from([
            (
                "zarr.json".to_owned(),
                (PackageObjectKind::ZarrRoot, group.clone()),
            ),
            (
                "images/zarr.json".to_owned(),
                (PackageObjectKind::ZarrImagesGroup, group.clone()),
            ),
            (
                "validity/zarr.json".to_owned(),
                (PackageObjectKind::ZarrValidityGroup, group.clone()),
            ),
            (
                "indexes/zarr.json".to_owned(),
                (PackageObjectKind::ZarrIndexesGroup, group),
            ),
            (
                PROFILE_PATH.to_owned(),
                (
                    PackageObjectKind::Profile,
                    profile.canonical_bytes().unwrap(),
                ),
            ),
            (
                "m4d/science.json".to_owned(),
                (
                    PackageObjectKind::Science,
                    science.canonical_bytes().unwrap(),
                ),
            ),
            (
                "m4d/display.json".to_owned(),
                (
                    PackageObjectKind::DisplayDefaults,
                    display.canonical_bytes().unwrap(),
                ),
            ),
            (
                "images/i00000000/zarr.json".to_owned(),
                (
                    PackageObjectKind::ZarrImageGroup,
                    ome.deterministic_bytes().unwrap(),
                ),
            ),
            (
                "images/i00000000/s00/zarr.json".to_owned(),
                (
                    PackageObjectKind::ZarrPixelArray,
                    ZarrArrayMetadata::new(
                        ShardProfileKind::Pixel2dUint8,
                        if matches!(drift, FixtureDrift::WrongPixelShape) {
                            vec![1, 1, 1, 2, 4]
                        } else {
                            vec![1, 1, 1, 2, 3]
                        },
                    )
                    .unwrap()
                    .deterministic_bytes()
                    .unwrap(),
                ),
            ),
            (
                "indexes/i00000000-s00/zarr.json".to_owned(),
                (
                    PackageObjectKind::ZarrPackedIndexArray,
                    ZarrArrayMetadata::new(ShardProfileKind::PackedIndex, vec![1, 64])
                        .unwrap()
                        .deterministic_bytes()
                        .unwrap(),
                ),
            ),
        ]);
        if let Some(pixel_shard) = pixel_shard {
            if matches!(brick, BrickFixture::OutOfGridPixelShard) {
                objects.insert(
                    "images/i00000000/s00/c/0/0/0/0/1".to_owned(),
                    (PackageObjectKind::PixelShard, pixel_shard.clone()),
                );
            }
            objects.insert(
                "images/i00000000/s00/c/0/0/0/0/0".to_owned(),
                (PackageObjectKind::PixelShard, pixel_shard),
            );
        }
        if !matches!(brick, BrickFixture::MissingPackedShard) {
            objects.insert(
                "indexes/i00000000-s00/c/0/0".to_owned(),
                (PackageObjectKind::PackedIndexShard, packed_shard),
            );
        }
        if explicit_validity {
            objects.insert(
                "validity/i00000000-s00/zarr.json".to_owned(),
                (
                    PackageObjectKind::ZarrValidityArray,
                    ZarrArrayMetadata::new(ShardProfileKind::Validity2d, vec![1, 1, 1, 2, 1])
                        .unwrap()
                        .deterministic_bytes()
                        .unwrap(),
                ),
            );
            if let Some(validity_shard) = validity_shard {
                objects.insert(
                    "validity/i00000000-s00/c/0/0/0/0/0".to_owned(),
                    (PackageObjectKind::ValidityShard, validity_shard),
                );
            }
        }
        if matches!(drift, FixtureDrift::ExtraMetadata) {
            objects.insert(
                "images/i00000001/zarr.json".to_owned(),
                (
                    PackageObjectKind::ZarrImageGroup,
                    ome.deterministic_bytes().unwrap(),
                ),
            );
        }

        let descriptors = objects
            .iter()
            .map(|(path, (kind, bytes))| {
                let facts = ExactBytesHasher::hash(bytes).unwrap();
                PackageObjectDescriptor::new(
                    PackagePath::parse(path).unwrap(),
                    *kind,
                    facts.byte_length(),
                    facts.digest(),
                )
                .unwrap()
            })
            .collect();
        let pages = crate::pack_manifest_pages(descriptors).unwrap();
        let manifest_root = ManifestRoot::new(&pages).unwrap();
        for (path, (_, bytes)) in objects {
            root.write(&path, &bytes);
        }
        for (ordinal, page) in pages.iter().enumerate() {
            root.write(
                &format!("m4d/manifest/pages/p{ordinal:08}.json"),
                &page.canonical_bytes().unwrap(),
            );
        }
        root.write(
            MANIFEST_ROOT_PATH,
            &manifest_root.canonical_bytes().unwrap(),
        );
        root
    }

    #[test]
    fn opens_authenticated_metadata_catalog_without_reading_shards() {
        let root = fixture(FixtureDrift::None);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        assert_eq!(
            catalog.declared_package_id(),
            ManifestRoot::parse_canonical(&fs::read(root.0.join(MANIFEST_ROOT_PATH)).unwrap())
                .unwrap()
                .package_id()
                .unwrap()
        );
        assert_eq!(catalog.profile().images().len(), 1);
        assert_eq!(catalog.science().layers().len(), 1);
        assert_eq!(catalog.zarr_arrays.len(), 2);
        assert!(catalog.metadata_bytes_read() > 0);
    }

    #[test]
    fn rejects_manifest_page_and_metadata_byte_corruption() {
        let root = fixture(FixtureDrift::None);
        let page = root.0.join("m4d/manifest/pages/p00000000.json");
        let mut bytes = fs::read(&page).unwrap();
        bytes.push(b' ');
        fs::write(&page, bytes).unwrap();
        assert!(matches!(
            LocalPackageCatalog::open(&root.0),
            Err(PackageOpenError::ManifestPageLengthMismatch { .. })
        ));

        let root = fixture(FixtureDrift::None);
        let science = root.0.join("m4d/science.json");
        let mut bytes = fs::read(&science).unwrap();
        bytes.push(b' ');
        fs::write(&science, bytes).unwrap();
        assert!(matches!(
            LocalPackageCatalog::open(&root.0),
            Err(PackageOpenError::ObjectLengthMismatch { .. })
        ));
    }

    #[test]
    fn rejects_cross_object_layer_drift_and_unexpected_metadata() {
        let root = fixture(FixtureDrift::EmptyDisplay);
        assert!(matches!(
            LocalPackageCatalog::open(&root.0),
            Err(PackageOpenError::CrossObjectInconsistency { .. })
        ));
        let root = fixture(FixtureDrift::ExtraMetadata);
        assert!(matches!(
            LocalPackageCatalog::open(&root.0),
            Err(PackageOpenError::UnexpectedMetadataObject { .. })
        ));
        for drift in [FixtureDrift::WrongPixelShape, FixtureDrift::UnitlessOme] {
            let root = fixture(drift);
            assert!(matches!(
                LocalPackageCatalog::open(&root.0),
                Err(PackageOpenError::CrossObjectInconsistency { .. })
            ));
        }
    }

    #[test]
    fn inspects_exact_bounded_directory_closure() {
        let root = fixture(FixtureDrift::None);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let inventory = catalog.inspect_directory_closure(|| false).unwrap();
        assert_eq!(inventory.regular_files(), 14);
        assert_eq!(inventory.directories(), 17);
        assert_eq!(inventory.maximum_directory_depth(), 8);
        assert_eq!(inventory.maximum_directory_fan_out(), 5);
        assert_eq!(inventory.zarr_metadata_objects(), 7);
        assert_eq!(inventory.manifest_pages(), 1);
        assert_eq!(inventory.fixed_control_objects(), 4);
        assert_eq!(inventory.pixel_shards(), 1);
        assert_eq!(inventory.validity_shards(), 0);
        assert_eq!(inventory.packed_index_shards(), 1);
        assert_eq!(inventory.portable_records(), 0);

        assert_eq!(
            catalog.inspect_directory_closure(|| true),
            Err(crate::DirectoryInventoryError::Cancelled)
        );

        fs::create_dir(root.0.join("extra")).unwrap();
        assert!(matches!(
            catalog.inspect_directory_closure(|| false),
            Err(crate::DirectoryInventoryError::UnexpectedDirectory { .. })
        ));

        let root = fixture(FixtureDrift::None);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let pixel = root.0.join("images/i00000000/s00/zarr.json");
        let mut bytes = fs::read(&pixel).unwrap();
        bytes.push(b' ');
        fs::write(pixel, bytes).unwrap();
        assert!(matches!(
            catalog.inspect_directory_closure(|| false),
            Err(crate::DirectoryInventoryError::ObjectLengthMismatch { .. })
        ));

        let root = fixture(FixtureDrift::None);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let manifest = root.0.join(MANIFEST_ROOT_PATH);
        let mut bytes = fs::read(&manifest).unwrap();
        let marker = b"\"digest\":\"";
        let start = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .unwrap()
            + marker.len();
        bytes[start] = if bytes[start] == b'0' { b'1' } else { b'0' };
        fs::write(manifest, bytes).unwrap();
        assert_eq!(
            catalog.inspect_directory_closure(|| false),
            Err(crate::DirectoryInventoryError::ManifestAuthorityChanged)
        );
    }

    #[test]
    fn derives_descriptor_bound_brick_address_plan() {
        let root = fixture(FixtureDrift::None);
        let mut catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let coordinates = crate::PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0);
        let plan = catalog.plan_brick_storage(coordinates).unwrap();
        assert_eq!(plan.coordinates(), coordinates);
        assert_eq!(plan.record_ordinal(), 0);
        assert_eq!(plan.logical_extent_zyx(), [1, 2, 3]);
        assert_eq!(plan.pixel_kind(), ShardProfileKind::Pixel2dUint8);
        assert_eq!(
            plan.pixel_shard_path().as_str(),
            "images/i00000000/s00/c/0/0/0/0/0"
        );
        assert_eq!(plan.pixel_inner_chunk(), 0);
        assert!(plan.pixel_shard_listed());
        assert_eq!(plan.validity_shard_path(), None);
        assert_eq!(plan.validity_inner_chunk(), None);
        assert_eq!(plan.validity_shard_listed(), None);
        assert_eq!(
            plan.packed_index_shard_path().as_str(),
            "indexes/i00000000-s00/c/0/0"
        );
        assert_eq!(plan.packed_index_inner_chunk(), 0);
        assert_eq!(plan.packed_index_record_byte_offset(), 0);

        assert!(matches!(
            catalog.plan_brick_storage(crate::PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 1)),
            Err(crate::BrickAddressError::CoordinateOutOfBounds { axis: "x", .. })
        ));

        catalog
            .descriptors
            .retain(|descriptor| descriptor.kind() != PackageObjectKind::PixelShard);
        assert!(
            !catalog
                .plan_brick_storage(coordinates)
                .unwrap()
                .pixel_shard_listed()
        );
        catalog
            .descriptors
            .retain(|descriptor| descriptor.kind() != PackageObjectKind::PackedIndexShard);
        assert!(matches!(
            catalog.plan_brick_storage(coordinates),
            Err(crate::BrickAddressError::MissingPackedIndexShard { .. })
        ));
    }

    #[test]
    fn admits_explicit_dataset_profile_from_addressed_and_actual_facts() {
        let root = fixture(FixtureDrift::None);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let admission = catalog
            .admit_dataset_profile(crate::ProfileKind::Ds0, || false)
            .unwrap();
        assert_eq!(admission.profile(), crate::ProfileKind::Ds0);
        assert_eq!(
            catalog
                .validate_package_structure(crate::ProfileKind::Ds0, || false)
                .unwrap(),
            admission
        );
        let counts = admission.counts();
        assert_eq!(counts.maximum_scales_per_image, 1);
        assert_eq!(counts.logical_s0_bytes, 6);
        assert_eq!(counts.logical_bricks, 1);
        assert_eq!(counts.addressed_pixel_shards, 1);
        assert_eq!(counts.actual_pixel_shards, 1);
        assert_eq!(counts.addressed_validity_shards, 0);
        assert_eq!(counts.actual_validity_shards, 0);
        assert_eq!(counts.addressed_packed_index_shards, 1);
        assert_eq!(counts.actual_packed_index_shards, 1);
        assert_eq!(counts.total_physical_objects, 14);
        assert_eq!(counts.maximum_directory_depth, 8);

        let root = fixture_with_brick(FixtureDrift::None, BrickFixture::AllFill);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let counts = catalog
            .admit_dataset_profile(crate::ProfileKind::Ds0, || false)
            .unwrap()
            .counts();
        assert_eq!(counts.addressed_pixel_shards, 1);
        assert_eq!(counts.actual_pixel_shards, 0);
        assert_eq!(counts.total_physical_objects, 13);

        let root = fixture_with_brick(FixtureDrift::None, BrickFixture::ExplicitValidity);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let counts = catalog
            .admit_dataset_profile(crate::ProfileKind::Ds0, || false)
            .unwrap()
            .counts();
        assert_eq!(counts.addressed_validity_shards, 1);
        assert_eq!(counts.actual_validity_shards, 1);

        let root = fixture_with_brick(FixtureDrift::None, BrickFixture::OutOfGridPixelShard);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        assert!(matches!(
            catalog.admit_dataset_profile(crate::ProfileKind::Ds0, || false),
            Err(crate::PackageAdmissionError::ShardCoordinateOutOfBounds { .. })
        ));

        let root = fixture_with_brick(FixtureDrift::None, BrickFixture::MissingPackedShard);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        assert_eq!(
            catalog.admit_dataset_profile(crate::ProfileKind::Ds0, || false),
            Err(crate::PackageAdmissionError::Profile(
                crate::StorageProfileError::PackedIndexShardCoverageMismatch {
                    actual: 0,
                    addressed: 1,
                }
            ))
        );

        let root = fixture(FixtureDrift::None);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        assert_eq!(
            catalog.admit_dataset_profile(crate::ProfileKind::Ds0, || true),
            Err(crate::PackageAdmissionError::Inventory(
                crate::DirectoryInventoryError::Cancelled
            ))
        );
    }

    #[test]
    fn reconciles_every_packed_record_with_canonical_shard_slots() {
        for (brick, listed_pixel, addressed_validity, listed_validity) in [
            (BrickFixture::PixelPresent, 1, 0, 0),
            (BrickFixture::AllFill, 0, 0, 0),
            (BrickFixture::ExplicitValidity, 1, 1, 1),
            (BrickFixture::ExplicitAllInvalid, 0, 1, 0),
        ] {
            let root = fixture_with_brick(FixtureDrift::None, brick);
            let catalog = LocalPackageCatalog::open(&root.0).unwrap();
            let report = catalog
                .reconcile_structure_for_test(crate::ProfileKind::Ds0, || false)
                .unwrap();
            assert_eq!(report.records_visited, 1);
            assert_eq!(report.packed_index_shards, 1);
            assert_eq!(report.addressed_pixel_shards, 1);
            assert_eq!(report.listed_pixel_shards, listed_pixel);
            assert_eq!(report.addressed_validity_shards, addressed_validity);
            assert_eq!(report.listed_validity_shards, listed_validity);
            assert!(report.work_operations > 0);
        }

        for (brick, expected) in [
            (
                BrickFixture::PackedCoordinateMismatch,
                "packed-record coordinates",
            ),
            (
                BrickFixture::PackedValidityMismatch,
                "packed-record validity",
            ),
            (BrickFixture::MissingPackedInner, "missing packed slot"),
            (BrickFixture::PackedUnusedInner, "unused packed slot"),
            (BrickFixture::PackedPaddingNonzero, "packed padding"),
            (BrickFixture::MissingPixelInner, "missing pixel inner"),
            (BrickFixture::MissingPixelShard, "missing pixel shard"),
            (BrickFixture::AllFillListedEmpty, "empty pixel shard"),
            (
                BrickFixture::AllFillUnexpectedPixel,
                "unexpected pixel inner",
            ),
            (
                BrickFixture::ExplicitAllInvalidUnexpectedValidity,
                "unexpected validity inner",
            ),
            (BrickFixture::OutOfGridPixelInner, "out-of-grid pixel inner"),
        ] {
            let root = fixture_with_brick(FixtureDrift::None, brick);
            let catalog = LocalPackageCatalog::open(&root.0).unwrap();
            let error = catalog
                .reconcile_structure_for_test(crate::ProfileKind::Ds0, || false)
                .expect_err(expected);
            match (expected, error) {
                (
                    "packed-record coordinates",
                    PackageStructureError::PackedRecordCoordinateMismatch { .. },
                )
                | (
                    "packed-record validity",
                    PackageStructureError::PackedRecordValidityMismatch { .. },
                )
                | ("missing packed slot", PackageStructureError::MissingPackedIndexSlot { .. })
                | ("unused packed slot", PackageStructureError::UnexpectedPackedIndexSlot { .. })
                | ("packed padding", PackageStructureError::NonzeroPackedIndexPadding { .. })
                | (
                    "missing pixel inner" | "unexpected pixel inner" | "unexpected validity inner",
                    PackageStructureError::InnerPayloadPresenceMismatch { .. },
                )
                | ("missing pixel shard", PackageStructureError::MissingShardDescriptor { .. })
                | ("empty pixel shard", PackageStructureError::ListedAllMissingShard { .. })
                | (
                    "out-of-grid pixel inner",
                    PackageStructureError::OutOfGridInnerPayload { .. },
                ) => {}
                (expected, actual) => panic!("{expected}: unexpected error {actual}"),
            }
        }

        let root = fixture(FixtureDrift::None);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        assert!(matches!(
            catalog.reconcile_structure_for_test(crate::ProfileKind::Ds0, || true),
            Err(PackageStructureError::Admission(
                crate::PackageAdmissionError::Inventory(crate::DirectoryInventoryError::Cancelled)
            ))
        ));

        let admission = catalog
            .admit_dataset_profile(crate::ProfileKind::Ds0, || false)
            .unwrap();
        let completed_polls = Cell::new(0_u64);
        let completed = reconcile_package_structure(
            PackageStructureInput {
                reader: &catalog.reader,
                profile: &catalog.profile,
                arrays: &catalog.zarr_arrays,
                descriptors: &catalog.descriptors,
                admission,
            },
            || {
                completed_polls.set(completed_polls.get() + 1);
                false
            },
        )
        .unwrap();
        let total_polls = completed_polls.get();
        assert!(total_polls > 10);
        for cancel_at in [total_polls / 2, total_polls - 2] {
            let polls = Cell::new(0_u64);
            assert!(matches!(
                reconcile_package_structure(
                    PackageStructureInput {
                        reader: &catalog.reader,
                        profile: &catalog.profile,
                        arrays: &catalog.zarr_arrays,
                        descriptors: &catalog.descriptors,
                        admission,
                    },
                    || {
                        let next = polls.get() + 1;
                        polls.set(next);
                        next == cancel_at
                    },
                ),
                Err(PackageStructureError::Cancelled)
            ));
        }
        assert!(matches!(
            completed.revalidate_snapshots(&catalog.reader, &mut || true),
            Err(PackageStructureError::Cancelled)
        ));

        let root = fixture(FixtureDrift::None);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let admission = catalog
            .admit_dataset_profile(crate::ProfileKind::Ds0, || false)
            .unwrap();
        let report = reconcile_package_structure(
            PackageStructureInput {
                reader: &catalog.reader,
                profile: &catalog.profile,
                arrays: &catalog.zarr_arrays,
                descriptors: &catalog.descriptors,
                admission,
            },
            || false,
        )
        .unwrap();
        let pixel = root.0.join("images/i00000000/s00/c/0/0/0/0/0");
        let replacement = root.0.join("replacement-shard");
        let pixel_bytes = usize::try_from(fs::metadata(&pixel).unwrap().len()).unwrap();
        fs::write(&replacement, vec![7; pixel_bytes]).unwrap();
        fs::rename(replacement, pixel).unwrap();
        catalog.inspect_directory_closure(|| false).unwrap();
        assert!(matches!(
            report.revalidate_snapshots(&catalog.reader, &mut || false),
            Err(PackageStructureError::Range(
                crate::RangeReadError::ObjectChanged { .. }
            ))
        ));

        let root = fixture(FixtureDrift::None);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let packed = root.0.join("indexes/i00000000-s00/c/0/0");
        let mut bytes = fs::read(&packed).unwrap();
        bytes[0] ^= 1;
        fs::write(packed, bytes).unwrap();
        assert!(matches!(
            catalog.reconcile_structure_for_test(crate::ProfileKind::Ds0, || false),
            Err(PackageStructureError::PackedIndexDigestMismatch { .. })
        ));
    }

    #[test]
    fn full_sha_validation_rejects_closure_drift_and_issues_exact_capability() {
        let coordinates = crate::PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0);
        let root = fixture_with_brick(FixtureDrift::None, BrickFixture::ExplicitValidity);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let declared = catalog.declared_package_id();
        let expected_objects = 1 + catalog.manifest_root.pages().len() + catalog.descriptors.len();
        let capability = catalog
            .validate_exact_package(crate::ProfileKind::Ds0, || false)
            .unwrap();
        assert_eq!(capability.package_id(), declared);
        assert_eq!(capability.admission().profile(), crate::ProfileKind::Ds0);
        assert_eq!(capability.objects_hashed(), expected_objects as u64);
        assert!(capability.bytes_hashed() > 0);
        let brick = capability.read_brick(coordinates, || false).unwrap();
        assert_eq!(&brick.pixel_payload().unwrap()[..6], &[0, 1, 2, 0, 0, 0]);
        assert_eq!(brick.validity_payload().unwrap()[0], 0b0000_0111);

        for relative in ["images/i00000000/s00/c/0/0/0/0/0", "m4d/display.json"] {
            let root = fixture(FixtureDrift::None);
            let catalog = LocalPackageCatalog::open(&root.0).unwrap();
            let path = root.0.join(relative);
            let mut bytes = fs::read(&path).unwrap();
            bytes[0] ^= 1;
            fs::write(path, bytes).unwrap();
            assert!(matches!(
                catalog.validate_exact_package(crate::ProfileKind::Ds0, || false),
                Err(crate::PackageValidationError::ObjectDigestMismatch { path })
                    if path == relative
            ));
        }

        let root = fixture(FixtureDrift::None);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        assert!(matches!(
            catalog.validate_exact_package(crate::ProfileKind::Ds0, || true),
            Err(crate::PackageValidationError::Cancelled)
        ));

        let root = fixture(FixtureDrift::None);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let report = catalog
            .reconcile_structure_for_test(crate::ProfileKind::Ds0, || false)
            .unwrap();
        let pixel = root.0.join("images/i00000000/s00/c/0/0/0/0/0");
        let replacement = root.0.join("replacement-between-phases");
        fs::write(&replacement, fs::read(&pixel).unwrap()).unwrap();
        fs::rename(replacement, pixel).unwrap();
        assert!(matches!(
            validate_package_integrity(
                PackageIntegrityInput {
                    reader: &catalog.reader,
                    manifest_root_path: catalog.profile.manifest_root_path(),
                    manifest_root_bytes: catalog.manifest_root_bytes,
                    manifest_root: &catalog.manifest_root,
                    package_id: catalog.declared_package_id,
                    descriptors: &catalog.descriptors,
                    structure: &report,
                },
                || false,
            ),
            Err(crate::PackageValidationError::Range(
                crate::RangeReadError::ObjectChanged { .. }
            ))
        ));

        let root = fixture(FixtureDrift::None);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let report = catalog
            .reconcile_structure_for_test(crate::ProfileKind::Ds0, || false)
            .unwrap();
        let proof = validate_package_integrity(
            PackageIntegrityInput {
                reader: &catalog.reader,
                manifest_root_path: catalog.profile.manifest_root_path(),
                manifest_root_bytes: catalog.manifest_root_bytes,
                manifest_root: &catalog.manifest_root,
                package_id: catalog.declared_package_id,
                descriptors: &catalog.descriptors,
                structure: &report,
            },
            || false,
        )
        .unwrap();
        let mut polls = 0_u64;
        assert!(matches!(
            proof.revalidate_all(&catalog.reader, &mut || {
                polls += 1;
                polls == 2
            }),
            Err(crate::PackageValidationError::Cancelled)
        ));
        let pixel = root.0.join("images/i00000000/s00/c/0/0/0/0/0");
        let replacement = root.0.join("replacement-shard");
        fs::write(&replacement, fs::read(&pixel).unwrap()).unwrap();
        fs::rename(replacement, pixel).unwrap();
        catalog.inspect_directory_closure(|| false).unwrap();
        assert!(matches!(
            proof.revalidate_all(&catalog.reader, &mut || false),
            Err(crate::PackageValidationError::Range(
                crate::RangeReadError::ObjectChanged { .. }
            ))
        ));
    }

    #[test]
    fn exact_capability_guards_consumed_and_complete_snapshot_freshness() {
        let coordinates = crate::PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0);
        let root = fixture(FixtureDrift::None);
        let capability = LocalPackageCatalog::open(&root.0)
            .unwrap()
            .validate_exact_package(crate::ProfileKind::Ds0, || false)
            .unwrap();
        let pixel = root.0.join("images/i00000000/s00/c/0/0/0/0/0");
        let replacement = root.0.join("replacement-pixel");
        fs::write(&replacement, fs::read(&pixel).unwrap()).unwrap();
        fs::rename(replacement, pixel).unwrap();
        assert!(matches!(
            capability.read_brick(coordinates, || false),
            Err(crate::PackageReadError::Range(
                crate::RangeReadError::ObjectChanged { .. }
            ))
        ));

        let root = fixture(FixtureDrift::None);
        let capability = LocalPackageCatalog::open(&root.0)
            .unwrap()
            .validate_exact_package(crate::ProfileKind::Ds0, || false)
            .unwrap();
        let display = root.0.join("m4d/display.json");
        let replacement = root.0.join("replacement-display");
        fs::write(&replacement, fs::read(&display).unwrap()).unwrap();
        fs::rename(replacement, display).unwrap();
        assert!(matches!(
            capability.revalidate_complete(|| false),
            Err(crate::PackageValidationError::Range(
                crate::RangeReadError::ObjectChanged { .. }
            ))
        ));
    }

    #[test]
    fn reads_one_crc_checked_brick_with_bounded_ranges() {
        let root = fixture(FixtureDrift::None);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let coordinates = crate::PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0);
        let brick = catalog.read_brick_core_for_test(coordinates).unwrap();
        assert_eq!(brick.record().coordinates(), coordinates);
        assert_eq!(brick.logical_extent_zyx(), [1, 2, 3]);
        assert_eq!(&brick.pixel_payload().unwrap()[..6], &[0, 1, 2, 0, 0, 0]);
        assert_eq!(brick.pixel_payload().unwrap().len(), 65_536);
        assert_eq!(brick.validity_payload(), None);
        assert_eq!(brick.range_requests(), 4);
        assert_eq!(
            brick.encoded_bytes_read(),
            fs::metadata(root.0.join("images/i00000000/s00/c/0/0/0/0/0"))
                .unwrap()
                .len()
                + fs::metadata(root.0.join("indexes/i00000000-s00/c/0/0"))
                    .unwrap()
                    .len()
        );
        assert!(
            brick.encoded_bytes_read()
                <= crate::amplification_2d(IntensityDType::Uint8).read_bytes_max
        );
        assert_eq!(brick.decoded_bytes(), 83_200);

        let root = fixture_with_brick(FixtureDrift::None, BrickFixture::AllFill);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let brick = catalog.read_brick_core_for_test(coordinates).unwrap();
        assert_eq!(brick.pixel_payload(), None);
        assert_eq!(brick.validity_payload(), None);
        assert_eq!(brick.range_requests(), 2);
        assert_eq!(brick.decoded_bytes(), 17_408);
        assert_eq!(
            brick.encoded_bytes_read(),
            fs::metadata(root.0.join("indexes/i00000000-s00/c/0/0"))
                .unwrap()
                .len()
        );

        let root = fixture_with_brick(FixtureDrift::None, BrickFixture::ExplicitValidity);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let brick = catalog.read_brick_core_for_test(coordinates).unwrap();
        assert!(brick.pixel_payload().is_some());
        assert_eq!(brick.validity_payload().unwrap()[0], 0b0000_0111);
        assert_eq!(brick.range_requests(), 6);
        assert_eq!(brick.decoded_bytes(), 91_648);
        assert_eq!(
            brick.encoded_bytes_read(),
            [
                "images/i00000000/s00/c/0/0/0/0/0",
                "validity/i00000000-s00/c/0/0/0/0/0",
                "indexes/i00000000-s00/c/0/0",
            ]
            .into_iter()
            .map(|path| fs::metadata(root.0.join(path)).unwrap().len())
            .sum::<u64>()
        );
        let limits = crate::amplification_2d(IntensityDType::Uint8);
        assert!(brick.encoded_bytes_read() <= limits.read_bytes_max);
        assert_eq!(brick.decoded_bytes(), limits.decoded_bytes_max);

        let root = fixture(FixtureDrift::None);
        let mut catalog = LocalPackageCatalog::open(&root.0).unwrap();
        catalog
            .descriptors
            .retain(|descriptor| descriptor.kind() != PackageObjectKind::PixelShard);
        assert!(matches!(
            catalog.read_brick_core_for_test(coordinates),
            Err(PackageReadError::MissingRequiredShardDescriptor {
                component: "pixel",
                ..
            })
        ));

        let root = fixture_with_brick(FixtureDrift::None, BrickFixture::MissingPixelInner);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        assert!(matches!(
            catalog.read_brick_core_for_test(coordinates),
            Err(PackageReadError::MissingRequiredInnerPayload {
                component: "pixel",
                ..
            })
        ));

        let root = fixture_with_brick(FixtureDrift::None, BrickFixture::PackedCoordinateMismatch);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        assert_eq!(
            catalog.read_brick_core_for_test(coordinates),
            Err(PackageReadError::PackedRecordCoordinateMismatch)
        );

        let root = fixture_with_brick(FixtureDrift::None, BrickFixture::PackedValidityMismatch);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        assert_eq!(
            catalog.read_brick_core_for_test(coordinates),
            Err(PackageReadError::PackedRecordValidityMismatch)
        );

        let root = fixture(FixtureDrift::None);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let packed = root.0.join("indexes/i00000000-s00/c/0/0");
        let mut bytes = fs::read(&packed).unwrap();
        bytes.push(0);
        fs::write(packed, bytes).unwrap();
        assert!(matches!(
            catalog.read_brick_core_for_test(coordinates),
            Err(PackageReadError::ObjectLengthMismatch { .. })
        ));

        let root = fixture(FixtureDrift::None);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let packed = root.0.join("indexes/i00000000-s00/c/0/0");
        let mut bytes = fs::read(&packed).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        fs::write(packed, bytes).unwrap();
        assert!(matches!(
            catalog.read_brick_core_for_test(coordinates),
            Err(PackageReadError::Shard(
                crate::ShardCodecError::IndexChecksumMismatch
            ))
        ));

        let root = fixture(FixtureDrift::None);
        let catalog = LocalPackageCatalog::open(&root.0).unwrap();
        let pixel = root.0.join("images/i00000000/s00/c/0/0/0/0/0");
        let mut bytes = fs::read(&pixel).unwrap();
        bytes[0] ^= 1;
        fs::write(pixel, bytes).unwrap();
        assert!(matches!(
            catalog.read_brick_core_for_test(coordinates),
            Err(PackageReadError::Shard(_))
        ));
    }
}
