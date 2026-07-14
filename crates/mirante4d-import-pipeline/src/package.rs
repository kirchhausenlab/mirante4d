//! Deterministic target-package metadata assembled from an accepted import plan.

use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use mirante4d_domain::{IntensityDType, LogicalLayerKey, Shape4D};
use mirante4d_identity::{ExactBytesDigest, ScientificContentId, Sha256Digest, Sha256Hasher};
use mirante4d_storage::{
    AsciiToken, CanonicalMapEntry, CanonicalValue, DerivationBinding, DerivationBody,
    DerivationExactness, DerivationImplementation, DerivationOutcome, DerivationPayload,
    DerivationScope, DerivationSpaceBox, DerivationTimeRange, DisplayDefaults,
    DisplayLayerDefaults, F32Bits, F64Bits, OmeImageGroupMetadata, OmeInteroperabilityBase,
    OmeLevelTransform, PackageArrayInput, PortableRecord, PortableRecordPayload, ProfileHeader,
    ProfileImage, ProfileKind, ProfileLevel, ProfileLogicalLayer, ProfileValidityMode, RecipeBody,
    RecipeDeterminism, RecipeNumericPolicy, RecipeOperation, RecipePayload, Rgb24, ScaleCountRule,
    ScienceDescriptor, ScienceLayer, ScienceTemporalCalibration, ShardProfileKind,
    SourceIdentifier, SourceIdentifierScheme, SourcePayload, TypedId, U64Decimal,
    ZarrArrayMetadata, profile_limits,
};

use crate::ImportError;

const IMAGE_ORDINAL: u32 = 0;
const PACKED_INDEX_RECORD_BYTES: u64 = 64;
const MAX_LEVELS: usize = 7;
const MAX_SOURCE_FILES: usize = 4_096;
const EXECUTABLE_HASH_BUFFER_BYTES: usize = 64 * 1024;

/// The scientific and storage facts needed to construct package metadata.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PackageMetadataInput {
    pub profile_kind: ProfileKind,
    pub scientific_content_id: ScientificContentId,
    pub base_shape: Shape4D,
    pub channel_count: u32,
    pub dtype: IntensityDType,
    pub pyramid_shapes: Vec<Shape4D>,
    pub spacing_zyx_um: [f64; 3],
    pub regular_time_step_seconds: Option<f64>,
    pub explicit_validity: bool,
    /// Raw TIFF file digests in deterministic logical source order.
    pub source_file_sha256: Vec<Sha256Digest>,
    pub u8_sentinel: Option<u8>,
}

/// Complete non-shard input fields for `PackageWriteInput`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackageMetadata {
    pub profile_kind: ProfileKind,
    pub profile: ProfileHeader,
    pub science: ScienceDescriptor,
    pub display_defaults: DisplayDefaults,
    pub portable_records: Vec<PortableRecord>,
    pub ome_images: Vec<OmeImageGroupMetadata>,
    pub arrays: Vec<PackageArrayInput>,
}

pub(crate) fn build_package_metadata(
    input: &PackageMetadataInput,
) -> Result<PackageMetadata, ImportError> {
    validate_input(input)?;

    let temporal = match input.regular_time_step_seconds {
        Some(seconds) => ScienceTemporalCalibration::regular(f64_bits(seconds)?)?,
        None => ScienceTemporalCalibration::unknown(),
    };
    let validity_mode = if input.explicit_validity {
        ProfileValidityMode::Explicit
    } else {
        ProfileValidityMode::AllValid
    };
    let levels = input
        .pyramid_shapes
        .iter()
        .enumerate()
        .map(|(ordinal, _)| {
            let ordinal = u32::try_from(ordinal).map_err(|_| ImportError::Overflow)?;
            ProfileLevel::new(IMAGE_ORDINAL, ordinal, validity_mode).map_err(ImportError::from)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let logical_layers = (0..input.channel_count)
        .map(|channel| ProfileLogicalLayer::new(LogicalLayerKey::new(channel), channel))
        .collect::<Vec<_>>();
    let image = ProfileImage::new(IMAGE_ORDINAL, logical_layers, levels)?;

    let portable_records = portable_records(input)?;
    let interoperability = if !input.explicit_validity && input.regular_time_step_seconds.is_some()
    {
        OmeInteroperabilityBase::Io2
    } else {
        OmeInteroperabilityBase::Io1
    };
    let profile = ProfileHeader::new(
        input.scientific_content_id,
        vec![image.clone()],
        u32::try_from(portable_records.len()).map_err(|_| ImportError::Overflow)?,
        interoperability,
    )?;

    let transform = base_grid_to_world(input.spacing_zyx_um)?;
    let science_layers = (0..input.channel_count)
        .map(|channel| {
            ScienceLayer::new(
                LogicalLayerKey::new(channel),
                input.base_shape,
                input.dtype,
                temporal.clone(),
                transform,
            )
            .map_err(ImportError::from)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let science = ScienceDescriptor::new(input.scientific_content_id, science_layers)?;
    let display_defaults = display_defaults(input.channel_count, input.dtype)?;
    let ome_images = vec![ome_metadata(&image, &temporal, input)?];
    let arrays = package_arrays(&image, input)?;

    Ok(PackageMetadata {
        profile_kind: input.profile_kind,
        profile,
        science,
        display_defaults,
        portable_records,
        ome_images,
        arrays,
    })
}

fn validate_input(input: &PackageMetadataInput) -> Result<(), ImportError> {
    if input.channel_count == 0 {
        return Err(ImportError::InvalidRequest(
            "package metadata requires at least one channel",
        ));
    }
    if input.pyramid_shapes.is_empty()
        || input.pyramid_shapes.len() > MAX_LEVELS
        || input.pyramid_shapes[0] != input.base_shape
    {
        return Err(ImportError::InvalidRequest(
            "pyramid shapes must begin with the base shape and contain one through seven levels",
        ));
    }
    for pair in input.pyramid_shapes.windows(2) {
        let expected = [
            pair[0].t(),
            pair[0].z().div_ceil(2),
            pair[0].y().div_ceil(2),
            pair[0].x().div_ceil(2),
        ];
        if pair[1].dimensions() != expected {
            return Err(ImportError::InvalidRequest(
                "pyramid shapes must use deterministic spatial factor-two reduction",
            ));
        }
    }
    let level_count =
        u64::try_from(input.pyramid_shapes.len()).map_err(|_| ImportError::Overflow)?;
    match profile_limits(input.profile_kind).scales {
        ScaleCountRule::Maximum(maximum) if level_count > maximum => {
            return Err(ImportError::InvalidRequest(
                "pyramid level count exceeds the selected storage profile",
            ));
        }
        ScaleCountRule::Exact(expected) if level_count != expected => {
            return Err(ImportError::InvalidRequest(
                "pyramid level count differs from the selected storage profile",
            ));
        }
        ScaleCountRule::Maximum(_) | ScaleCountRule::Exact(_) => {}
    }

    if input
        .spacing_zyx_um
        .iter()
        .any(|spacing| !spacing.is_finite() || *spacing <= 0.0)
    {
        return Err(ImportError::InvalidRequest(
            "spatial spacing must be finite and positive",
        ));
    }
    if input
        .regular_time_step_seconds
        .is_some_and(|seconds| !seconds.is_finite() || seconds <= 0.0)
    {
        return Err(ImportError::InvalidRequest(
            "regular time spacing must be finite and positive",
        ));
    }
    if input.explicit_validity != input.u8_sentinel.is_some() {
        return Err(ImportError::InvalidRequest(
            "WP-11 explicit validity requires exactly one u8 sentinel policy",
        ));
    }
    if input.u8_sentinel.is_some() && input.dtype != IntensityDType::Uint8 {
        return Err(ImportError::InvalidRequest(
            "the u8 sentinel policy is valid only for uint8 source pixels",
        ));
    }
    if input.source_file_sha256.is_empty() || input.source_file_sha256.len() > MAX_SOURCE_FILES {
        return Err(ImportError::InvalidRequest(
            "package provenance requires one through 4096 source TIFF file digests",
        ));
    }
    Ok(())
}

fn base_grid_to_world(spacing_zyx_um: [f64; 3]) -> Result<[F64Bits; 16], ImportError> {
    let zero = f64_bits(0.0)?;
    let one = f64_bits(1.0)?;
    let [z, y, x] = f64_bits3(spacing_zyx_um)?;
    Ok([
        x, zero, zero, zero, zero, y, zero, zero, zero, zero, z, zero, zero, zero, zero, one,
    ])
}

fn ome_metadata(
    image: &ProfileImage,
    temporal: &ScienceTemporalCalibration,
    input: &PackageMetadataInput,
) -> Result<OmeImageGroupMetadata, ImportError> {
    let mut factor = 1_u64;
    let mut transforms = Vec::with_capacity(input.pyramid_shapes.len());
    for _ in &input.pyramid_shapes {
        let factor_f64 = factor as f64;
        let scale_zyx = [
            input.spacing_zyx_um[0] * factor_f64,
            input.spacing_zyx_um[1] * factor_f64,
            input.spacing_zyx_um[2] * factor_f64,
        ];
        transforms.push(OmeLevelTransform::DiagonalMicrometer {
            scale_zyx: f64_bits3(scale_zyx)?,
            // The accepted target profile anchors every level at the base
            // grid origin; only the spatial scale changes with level.
            translation_zyx: f64_bits3([0.0; 3])?,
        });
        factor = factor.checked_mul(2).ok_or(ImportError::Overflow)?;
    }
    OmeImageGroupMetadata::new(image, temporal, transforms).map_err(|_| {
        ImportError::InvalidRequest("package OME metadata is inconsistent with the import plan")
    })
}

fn package_arrays(
    image: &ProfileImage,
    input: &PackageMetadataInput,
) -> Result<Vec<PackageArrayInput>, ImportError> {
    let two_dimensional = input.base_shape.z() == 1;
    let pixel_kind = pixel_kind(input.dtype, two_dimensional);
    let validity_kind = if two_dimensional {
        ShardProfileKind::Validity2d
    } else {
        ShardProfileKind::Validity3d
    };
    let brick_zyx = if two_dimensional {
        [1, 256, 256]
    } else {
        [64, 64, 64]
    };
    let mut arrays = Vec::with_capacity(
        input.pyramid_shapes.len() * if input.explicit_validity { 3 } else { 2 },
    );

    for (level, shape) in image.levels().iter().zip(&input.pyramid_shapes) {
        let pixel_shape = vec![
            shape.t(),
            u64::from(input.channel_count),
            shape.z(),
            shape.y(),
            shape.x(),
        ];
        arrays.push(PackageArrayInput::new(
            level.pixel_path().clone(),
            zarr_array(pixel_kind, pixel_shape)?,
        ));

        if let Some(path) = level.validity_path() {
            arrays.push(PackageArrayInput::new(
                path.clone(),
                zarr_array(
                    validity_kind,
                    vec![
                        shape.t(),
                        u64::from(input.channel_count),
                        shape.z(),
                        shape.y(),
                        shape.x().div_ceil(8),
                    ],
                )?,
            ));
        }

        let records = [
            shape.t(),
            u64::from(input.channel_count),
            shape.z().div_ceil(brick_zyx[0]),
            shape.y().div_ceil(brick_zyx[1]),
            shape.x().div_ceil(brick_zyx[2]),
        ]
        .into_iter()
        .try_fold(1_u64, |product, count| product.checked_mul(count))
        .ok_or(ImportError::Overflow)?;
        arrays.push(PackageArrayInput::new(
            level.packed_index_path().clone(),
            zarr_array(
                ShardProfileKind::PackedIndex,
                vec![records, PACKED_INDEX_RECORD_BYTES],
            )?,
        ));
    }
    Ok(arrays)
}

fn zarr_array(kind: ShardProfileKind, shape: Vec<u64>) -> Result<ZarrArrayMetadata, ImportError> {
    ZarrArrayMetadata::new(kind, shape).map_err(|_| {
        ImportError::InvalidRequest("target Zarr array metadata is inconsistent with the plan")
    })
}

const fn pixel_kind(dtype: IntensityDType, two_dimensional: bool) -> ShardProfileKind {
    match (dtype, two_dimensional) {
        (IntensityDType::Uint8, false) => ShardProfileKind::Pixel3dUint8,
        (IntensityDType::Uint16, false) => ShardProfileKind::Pixel3dUint16,
        (IntensityDType::Float32, false) => ShardProfileKind::Pixel3dFloat32,
        (IntensityDType::Uint8, true) => ShardProfileKind::Pixel2dUint8,
        (IntensityDType::Uint16, true) => ShardProfileKind::Pixel2dUint16,
        (IntensityDType::Float32, true) => ShardProfileKind::Pixel2dFloat32,
    }
}

fn display_defaults(
    channel_count: u32,
    dtype: IntensityDType,
) -> Result<DisplayDefaults, ImportError> {
    const COLORS: [&str; 7] = [
        "ffffff", "ff00ff", "00ff00", "00ffff", "ffff00", "ff0000", "0000ff",
    ];
    let window_max = match dtype {
        IntensityDType::Uint8 => 255.0,
        IntensityDType::Uint16 => 65_535.0,
        IntensityDType::Float32 => 1.0,
    };
    let layers = (0..channel_count)
        .map(|channel| {
            let color_index =
                usize::try_from(channel).map_err(|_| ImportError::Overflow)? % COLORS.len();
            DisplayLayerDefaults::new(
                LogicalLayerKey::new(channel),
                channel == 0,
                Rgb24::parse(COLORS[color_index])?,
                f32_bits(0.0)?,
                f32_bits(window_max)?,
            )
            .map_err(ImportError::from)
        })
        .collect::<Result<Vec<_>, _>>()?;
    DisplayDefaults::new(layers).map_err(ImportError::from)
}

fn portable_records(input: &PackageMetadataInput) -> Result<Vec<PortableRecord>, ImportError> {
    let subject = vec![TypedId::Scientific(input.scientific_content_id)];
    let mut unique_source_digests = input.source_file_sha256.clone();
    unique_source_digests.sort_unstable();
    unique_source_digests.dedup();
    let source = SourcePayload::new(
        unique_source_digests
            .into_iter()
            .map(|digest| {
                SourceIdentifier::new(
                    SourceIdentifierScheme::Sha256,
                    mirante4d_storage::NfcText::parse(&digest.to_string())?,
                )
                .map_err(ImportError::from)
            })
            .collect::<Result<Vec<_>, _>>()?,
        None,
    )?;

    let recipe = recipe(input)?;
    let recipe_id = recipe.recipe_id();
    let derivation_inputs = input
        .source_file_sha256
        .iter()
        .copied()
        .enumerate()
        .map(|(ordinal, digest)| {
            Ok(DerivationBinding::new(
                token(&format!("source-{ordinal:04}"))?,
                TypedId::ExactBytes(ExactBytesDigest::from_digest(digest)),
            ))
        })
        .collect::<Result<Vec<_>, ImportError>>()?;
    let zero = number(0)?;
    let scope = DerivationScope::new(
        (0..input.channel_count)
            .map(|channel| number(u64::from(channel)))
            .collect::<Result<Vec<_>, _>>()?,
        vec![DerivationTimeRange::new(
            zero,
            number(input.base_shape.t() - 1)?,
        )?],
        vec![DerivationSpaceBox::new(
            [zero; 4],
            number4(input.base_shape.dimensions())?,
        )?],
    )?;
    let derivation = DerivationPayload::new(DerivationBody::new(
        recipe_id,
        derivation_inputs,
        vec![DerivationBinding::new(
            token("result")?,
            TypedId::Scientific(input.scientific_content_id),
        )],
        scope,
        DerivationImplementation::new(
            token("mirante4d-import-pipeline")?,
            token(env!("CARGO_PKG_VERSION"))?,
            running_executable_digest()?,
        ),
        DerivationOutcome::Success,
        DerivationExactness::Exact,
    )?)?;

    Ok(vec![
        PortableRecord::new(
            number(0)?,
            subject.clone(),
            PortableRecordPayload::Source(source),
        )?,
        PortableRecord::new(
            number(1)?,
            subject.clone(),
            PortableRecordPayload::Recipe(recipe),
        )?,
        PortableRecord::new(
            number(2)?,
            subject,
            PortableRecordPayload::Derivation(derivation),
        )?,
    ])
}

fn recipe(input: &PackageMetadataInput) -> Result<RecipePayload, ImportError> {
    let mut parameters = vec![
        CanonicalMapEntry::new(
            token("spacing_x_um")?,
            CanonicalValue::from_f64(f64_bits(input.spacing_zyx_um[2])?),
        ),
        CanonicalMapEntry::new(
            token("spacing_y_um")?,
            CanonicalValue::from_f64(f64_bits(input.spacing_zyx_um[1])?),
        ),
        CanonicalMapEntry::new(
            token("spacing_z_um")?,
            CanonicalValue::from_f64(f64_bits(input.spacing_zyx_um[0])?),
        ),
    ];
    if let Some(seconds) = input.regular_time_step_seconds {
        parameters.push(CanonicalMapEntry::new(
            token("time_step_seconds")?,
            CanonicalValue::from_f64(f64_bits(seconds)?),
        ));
    }
    if let Some(sentinel) = input.u8_sentinel {
        parameters.push(CanonicalMapEntry::new(
            token("u8_sentinel")?,
            CanonicalValue::from_u64(number(u64::from(sentinel))?),
        ));
    }

    let no_data = if input.explicit_validity {
        "sentinel-to-invalid"
    } else {
        "all-valid"
    };
    let operation = RecipeOperation::new(
        number(0)?,
        token("tiff-import-canonical-base")?,
        token("1.0.0")?,
        token("m4d.import.base.v1")?,
        CanonicalValue::map(parameters)?,
        Vec::new(),
        RecipeNumericPolicy::new(
            token(dtype_name(input.dtype))?,
            token("identity")?,
            token("none")?,
            token("identity")?,
            token("none")?,
            token("none")?,
            token(no_data)?,
            token("tczyx")?,
            token("identity")?,
            None,
        ),
        vec![token("base-image")?],
    )?;
    let registry = ExactBytesDigest::from_digest(Sha256Hasher::digest(
        b"mirante4d-import-pipeline-base-operation-registry-v1",
    ));
    RecipePayload::new(RecipeBody::new(
        registry,
        RecipeDeterminism::BitExact,
        vec![operation],
    )?)
    .map_err(ImportError::from)
}

fn running_executable_digest() -> Result<ExactBytesDigest, ImportError> {
    let path = std::env::current_exe().map_err(|source| ImportError::Io {
        operation: "resolve running executable",
        path: PathBuf::from("/proc/self/exe"),
        source,
    })?;
    hash_file_exact_bytes(&path)
}

fn hash_file_exact_bytes(path: &Path) -> Result<ExactBytesDigest, ImportError> {
    let mut file = File::open(path).map_err(|source| ImportError::Io {
        operation: "open running executable",
        path: path.to_owned(),
        source,
    })?;
    let mut buffer = [0_u8; EXECUTABLE_HASH_BUFFER_BYTES];
    let mut hasher = Sha256Hasher::new();
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buffer[..read]),
            Err(source) if source.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(ImportError::Io {
                    operation: "read running executable",
                    path: path.to_owned(),
                    source,
                });
            }
        }
    }
    Ok(ExactBytesDigest::from_digest(hasher.finalize()))
}

const fn dtype_name(dtype: IntensityDType) -> &'static str {
    match dtype {
        IntensityDType::Uint8 => "uint8",
        IntensityDType::Uint16 => "uint16",
        IntensityDType::Float32 => "float32",
    }
}

fn token(value: &str) -> Result<AsciiToken, ImportError> {
    AsciiToken::parse(value).map_err(ImportError::from)
}

fn number(value: u64) -> Result<U64Decimal, ImportError> {
    U64Decimal::parse(&value.to_string()).map_err(ImportError::from)
}

fn f32_bits(value: f32) -> Result<F32Bits, ImportError> {
    F32Bits::parse(&format!("{:08x}", value.to_bits())).map_err(ImportError::from)
}

fn f64_bits(value: f64) -> Result<F64Bits, ImportError> {
    let value = if value == 0.0 { 0.0 } else { value };
    F64Bits::parse(&format!("{:016x}", value.to_bits())).map_err(ImportError::from)
}

fn f64_bits3(values: [f64; 3]) -> Result<[F64Bits; 3], ImportError> {
    Ok([
        f64_bits(values[0])?,
        f64_bits(values[1])?,
        f64_bits(values[2])?,
    ])
}

fn number4(values: [u64; 4]) -> Result<[U64Decimal; 4], ImportError> {
    Ok([
        number(values[0])?,
        number(values[1])?,
        number(values[2])?,
        number(values[3])?,
    ])
}

#[cfg(test)]
mod tests {
    use mirante4d_identity::Sha256Digest;
    use mirante4d_storage::PortableRecordKind;

    use super::*;

    fn scientific_id() -> ScientificContentId {
        ScientificContentId::from_digest(Sha256Digest::from_bytes([7; 32]))
    }

    fn input(base_shape: Shape4D, pyramid_shapes: Vec<Shape4D>) -> PackageMetadataInput {
        PackageMetadataInput {
            profile_kind: ProfileKind::Ds0,
            scientific_content_id: scientific_id(),
            base_shape,
            channel_count: 1,
            dtype: IntensityDType::Uint16,
            pyramid_shapes,
            spacing_zyx_um: [0.5, 0.3, 0.2],
            regular_time_step_seconds: Some(2.0),
            explicit_validity: false,
            source_file_sha256: vec![Sha256Digest::from_bytes([9; 32])],
            u8_sentinel: None,
        }
    }

    #[test]
    fn builds_deterministic_3d_metadata_and_target_profile_transforms() {
        let base = Shape4D::new(2, 65, 300, 300).unwrap();
        let coarse = Shape4D::new(2, 33, 150, 150).unwrap();
        let input = input(base, vec![base, coarse]);
        let first = build_package_metadata(&input).unwrap();
        let second = build_package_metadata(&input).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.profile.images()[0].levels().len(), 2);
        assert_eq!(first.science.layers().len(), 1);
        assert_eq!(first.arrays.len(), 4);
        assert_eq!(
            first.arrays[0].metadata().kind(),
            ShardProfileKind::Pixel3dUint16
        );
        assert_eq!(first.arrays[0].metadata().shape(), [2, 1, 65, 300, 300]);
        assert_eq!(first.arrays[1].metadata().shape(), [100, 64]);
        assert_eq!(
            first.ome_images[0].level_transforms()[1],
            OmeLevelTransform::DiagonalMicrometer {
                scale_zyx: [
                    f64_bits(1.0).unwrap(),
                    f64_bits(0.6).unwrap(),
                    f64_bits(0.4).unwrap()
                ],
                translation_zyx: [
                    f64_bits(0.0).unwrap(),
                    f64_bits(0.0).unwrap(),
                    f64_bits(0.0).unwrap(),
                ],
            }
        );
        assert_eq!(
            first
                .portable_records
                .iter()
                .map(PortableRecord::kind)
                .collect::<Vec<_>>(),
            vec![
                PortableRecordKind::Source,
                PortableRecordKind::Recipe,
                PortableRecordKind::Derivation,
            ]
        );
    }

    #[test]
    fn builds_2d_explicit_validity_arrays_for_all_channels() {
        let base = Shape4D::new(1, 1, 512, 512).unwrap();
        let coarse = Shape4D::new(1, 1, 256, 256).unwrap();
        let mut input = input(base, vec![base, coarse]);
        input.channel_count = 2;
        input.dtype = IntensityDType::Uint8;
        input.explicit_validity = true;
        input.u8_sentinel = Some(255);
        let metadata = build_package_metadata(&input).unwrap();

        assert_eq!(metadata.science.layers().len(), 2);
        assert_eq!(metadata.arrays.len(), 6);
        assert_eq!(
            metadata.arrays[0].metadata().kind(),
            ShardProfileKind::Pixel2dUint8
        );
        assert_eq!(
            metadata.arrays[1].metadata().kind(),
            ShardProfileKind::Validity2d
        );
        assert_eq!(metadata.arrays[1].metadata().shape(), [1, 2, 1, 512, 64]);
        assert_eq!(metadata.arrays[2].metadata().shape(), [8, 64]);
        assert_eq!(
            metadata.profile.ome_interoperability_base(),
            OmeInteroperabilityBase::Io1
        );
        assert_eq!(
            metadata.ome_images[0].level_transforms()[1],
            OmeLevelTransform::DiagonalMicrometer {
                scale_zyx: [
                    f64_bits(1.0).unwrap(),
                    f64_bits(0.6).unwrap(),
                    f64_bits(0.4).unwrap()
                ],
                translation_zyx: [
                    f64_bits(0.0).unwrap(),
                    f64_bits(0.0).unwrap(),
                    f64_bits(0.0).unwrap(),
                ],
            }
        );
    }

    #[test]
    fn rejects_malformed_pyramids_and_sentinel_mismatches() {
        let base = Shape4D::new(1, 1, 512, 512).unwrap();
        let wrong = Shape4D::new(1, 1, 255, 256).unwrap();
        assert!(build_package_metadata(&input(base, vec![base, wrong])).is_err());

        let mut mismatch = input(base, vec![base]);
        mismatch.explicit_validity = true;
        assert!(build_package_metadata(&mismatch).is_err());

        let mut too_many_sources = input(base, vec![base]);
        too_many_sources.source_file_sha256 =
            vec![Sha256Digest::from_bytes([1; 32]); MAX_SOURCE_FILES + 1];
        assert!(matches!(
            build_package_metadata(&too_many_sources),
            Err(ImportError::InvalidRequest(
                "package provenance requires one through 4096 source TIFF file digests"
            ))
        ));
    }

    #[test]
    fn records_raw_source_files_executable_build_and_base_only_recipe() {
        let base = Shape4D::new(1, 3, 8, 8).unwrap();
        let mut input = input(base, vec![base]);
        let logical_source_digests = vec![
            Sha256Digest::from_bytes([3; 32]),
            Sha256Digest::from_bytes([1; 32]),
            Sha256Digest::from_bytes([3; 32]),
            Sha256Digest::from_bytes([2; 32]),
        ];
        input.source_file_sha256 = logical_source_digests.clone();
        let metadata = build_package_metadata(&input).unwrap();

        let PortableRecordPayload::Source(source) = metadata.portable_records[0].payload() else {
            panic!("record zero must be source provenance");
        };
        assert_eq!(
            source
                .source_identifiers()
                .iter()
                .map(|identifier| {
                    assert_eq!(identifier.scheme(), SourceIdentifierScheme::Sha256);
                    identifier.value().as_str().to_owned()
                })
                .collect::<Vec<_>>(),
            vec![
                Sha256Digest::from_bytes([1; 32]).to_string(),
                Sha256Digest::from_bytes([2; 32]).to_string(),
                Sha256Digest::from_bytes([3; 32]).to_string(),
            ]
        );

        let PortableRecordPayload::Derivation(derivation) = metadata.portable_records[2].payload()
        else {
            panic!("record two must be derivation provenance");
        };
        assert_eq!(
            derivation
                .body()
                .inputs()
                .iter()
                .map(|binding| binding.role().as_str())
                .collect::<Vec<_>>(),
            vec!["source-0000", "source-0001", "source-0002", "source-0003"]
        );
        assert_eq!(
            derivation
                .body()
                .inputs()
                .iter()
                .map(DerivationBinding::id)
                .collect::<Vec<_>>(),
            logical_source_digests
                .into_iter()
                .map(|digest| TypedId::ExactBytes(ExactBytesDigest::from_digest(digest)))
                .collect::<Vec<_>>()
        );
        let build = derivation.body().implementation().build();
        assert_eq!(build, independently_hash_running_executable());
        let old_version_literal =
            format!("mirante4d-import-pipeline/{}", env!("CARGO_PKG_VERSION"));
        assert_ne!(
            build,
            ExactBytesDigest::from_digest(Sha256Hasher::digest(old_version_literal.as_bytes()))
        );

        let PortableRecordPayload::Recipe(recipe) = metadata.portable_records[1].payload() else {
            panic!("record one must be recipe provenance");
        };
        let operation = &recipe.body().operations()[0];
        assert_eq!(operation.name().as_str(), "tiff-import-canonical-base");
        assert_eq!(operation.parameter_schema().as_str(), "m4d.import.base.v1");
        assert_eq!(operation.output_roles()[0].as_str(), "base-image");
        let policy = operation.numeric_policy();
        assert_eq!(policy.rounding().as_str(), "identity");
        assert_eq!(policy.reduction().as_str(), "none");
        assert_eq!(policy.kernel().as_str(), "identity");
        assert_eq!(policy.boundary().as_str(), "none");
        assert_eq!(policy.interpolation().as_str(), "none");
        assert_eq!(policy.precision().as_str(), "identity");
    }

    fn independently_hash_running_executable() -> ExactBytesDigest {
        let path = std::env::current_exe().unwrap();
        let mut file = File::open(path).unwrap();
        let mut buffer = [0_u8; 4 * 1024];
        let mut hasher = Sha256Hasher::new();
        loop {
            let read = file.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        ExactBytesDigest::from_digest(hasher.finalize())
    }
}
