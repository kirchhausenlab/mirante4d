use std::{collections::BTreeMap, path::Path};

use mirante4d_domain::IntensityDType;
use mirante4d_identity::{ExactBytesHasher, IdentityHashError, PackageId};
use thiserror::Error;

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
    package_id: PackageId,
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
        let package_id = manifest_root.package_id()?;
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
            package_id,
            profile,
            science,
            display_defaults,
            descriptors,
            ome_images,
            zarr_arrays,
            metadata_bytes_read,
        })
    }

    pub const fn package_id(&self) -> PackageId {
        self.package_id
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
    pixel_shape
        .into_iter()
        .zip(brick)
        .try_fold(1_u64, |count, (dimension, chunk)| {
            count.checked_mul(ceil_divide(dimension, chunk))
        })
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
        F32Bits, F64Bits, OmeInteroperabilityBase, OmeLevelTransform, ProfileLevel,
        ProfileLogicalLayer, ProfileValidityMode, Rgb24, ScienceLayer, ScienceTemporalCalibration,
        ShardProfileKind,
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

    fn fixture(drift: FixtureDrift) -> TempRoot {
        let root = TempRoot::new();
        let temporal = ScienceTemporalCalibration::regular(bits64("3ff0000000000000")).unwrap();
        let profile_level = ProfileLevel::new(0, 0, ProfileValidityMode::AllValid).unwrap();
        let image = crate::ProfileImage::new(
            0,
            vec![ProfileLogicalLayer::new(LogicalLayerKey::new(0), 0)],
            vec![profile_level],
        )
        .unwrap();
        let profile = ProfileHeader::new(
            scientific_id(),
            vec![image.clone()],
            0,
            OmeInteroperabilityBase::Io2,
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
            catalog.package_id(),
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
}
