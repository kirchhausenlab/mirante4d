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
    DisplayLayerDefaults, F32Bits, F64Bits, NfcText, OmeImageGroupMetadata,
    OmeInteroperabilityBase, OmeLevelTransform, PackageArrayInput, PortableRecord,
    PortableRecordPayload, ProfileHeader, ProfileImage, ProfileKind, ProfileLevel,
    ProfileLogicalLayer, ProfileValidityMode, RecipeBody, RecipeDeterminism, RecipeNumericPolicy,
    RecipeOperation, RecipePayload, Rgb24, ScaleCountRule, ScienceDescriptor, ScienceLayer,
    ScienceTemporalCalibration, ShardProfileKind, SourceIdentifier, SourceIdentifierScheme,
    SourcePayload, TypedId, U64Decimal, ZarrArrayMetadata, profile_limits,
};

use crate::{
    ImportError, NoDataValueRule,
    model::{ResolvedNoDataPolicy, ResolvedNoDataValue},
};

const IMAGE_ORDINAL: u32 = 0;
const PACKED_INDEX_RECORD_BYTES: u64 = 64;
const EXECUTABLE_HASH_BUFFER_BYTES: usize = 64 * 1024;
const BASE_OPERATION_REGISTRY_V1: &[u8] = b"mirante4d-import-pipeline-base-operation-registry-v1";
const NO_DATA_OPERATION_REGISTRY_V2: &[u8] =
    b"mirante4d-import-pipeline-typed-first-volume-no-data-operation-registry-v2";

/// The scientific and storage facts needed to construct package metadata.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PackageMetadataInput {
    pub profile_kind: ProfileKind,
    pub scientific_content_id: ScientificContentId,
    pub base_shape: Shape4D,
    pub channel_count: u32,
    pub channel_labels: Vec<String>,
    pub dtype: IntensityDType,
    pub pyramid_shapes: Vec<Shape4D>,
    pub spacing_zyx_um: [f64; 3],
    pub regular_time_step_seconds: Option<f64>,
    pub explicit_validity: bool,
    /// Path-free digest of the canonical values decoded from the source.
    pub decoded_source_sha256: Sha256Digest,
    pub no_data: ResolvedNoDataPolicy,
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
    let display_defaults = display_defaults(&input.channel_labels, input.dtype)?;
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
    if input.channel_labels.len()
        != usize::try_from(input.channel_count).map_err(|_| ImportError::Overflow)?
    {
        return Err(ImportError::InvalidRequest(
            "package metadata requires exactly one label per channel",
        ));
    }
    if input.pyramid_shapes.is_empty() || input.pyramid_shapes[0] != input.base_shape {
        return Err(ImportError::InvalidRequest(
            "pyramid shapes must begin with the base shape",
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
    if input.explicit_validity != input.no_data.explicit_validity() {
        return Err(ImportError::InvalidRequest(
            "explicit validity differs from the resolved no-data policy",
        ));
    }
    if input
        .no_data
        .value()
        .is_some_and(|value| value.dtype() != input.dtype)
    {
        return Err(ImportError::InvalidRequest(
            "the resolved no-data value has the wrong source dtype",
        ));
    }
    if input.no_data.automatic_mask().is_some_and(|mask| {
        mask.shape_zyx()
            != [
                input.base_shape.z(),
                input.base_shape.y(),
                input.base_shape.x(),
            ]
    }) {
        return Err(ImportError::InvalidRequest(
            "the automatic no-data mask shape differs from the package base shape",
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
    let centered_mean_pyramid = input.explicit_validity;
    let mut factors_zyx = [1_u64; 3];
    let mut transforms = Vec::with_capacity(input.pyramid_shapes.len());
    for (ordinal, shape) in input.pyramid_shapes.iter().enumerate() {
        if ordinal > 0 {
            let previous = input.pyramid_shapes[ordinal - 1];
            for (axis, prior_length) in previous.dimensions()[1..].iter().copied().enumerate() {
                if !centered_mean_pyramid || prior_length > 1 {
                    factors_zyx[axis] = factors_zyx[axis]
                        .checked_mul(2)
                        .ok_or(ImportError::Overflow)?;
                }
            }
        }
        debug_assert_eq!(shape.t(), input.base_shape.t());
        let scale_zyx =
            std::array::from_fn(|axis| input.spacing_zyx_um[axis] * factors_zyx[axis] as f64);
        let translation_zyx = if centered_mean_pyramid {
            std::array::from_fn(|axis| {
                input.spacing_zyx_um[axis] * (factors_zyx[axis] - 1) as f64 / 2.0
            })
        } else {
            // The no-sentinel route retains its point-sampled, base-origin
            // transform exactly. Restoring mean semantics is intentionally
            // limited to the reviewed sentinel policy.
            [0.0; 3]
        };
        transforms.push(OmeLevelTransform::DiagonalMicrometer {
            scale_zyx: f64_bits3(scale_zyx)?,
            translation_zyx: f64_bits3(translation_zyx)?,
        });
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
    channel_labels: &[String],
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
    let layers = channel_labels
        .iter()
        .enumerate()
        .map(|(channel, label)| {
            let channel = u32::try_from(channel).map_err(|_| ImportError::Overflow)?;
            let color_index =
                usize::try_from(channel).map_err(|_| ImportError::Overflow)? % COLORS.len();
            DisplayLayerDefaults::new_with_label(
                LogicalLayerKey::new(channel),
                Some(NfcText::parse(label)?),
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
    let source = SourcePayload::new(
        vec![SourceIdentifier::new(
            SourceIdentifierScheme::Sha256,
            mirante4d_storage::NfcText::parse(&input.decoded_source_sha256.to_string())?,
        )?],
        None,
    )?;

    let recipe = recipe(input)?;
    let recipe_id = recipe.recipe_id();
    let derivation_inputs = vec![DerivationBinding::new(
        token("decoded-source")?,
        TypedId::ExactBytes(ExactBytesDigest::from_digest(input.decoded_source_sha256)),
    )];
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
    let base_parameters = vec![
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
    let (operation, registry_preimage) = match input.no_data.request() {
        Some(_) => no_data_recipe_operation(input, base_parameters)?,
        None => base_recipe_operation(input, base_parameters)?,
    };
    let registry = ExactBytesDigest::from_digest(Sha256Hasher::digest(registry_preimage));
    RecipePayload::new(RecipeBody::new(
        registry,
        RecipeDeterminism::BitExact,
        vec![operation],
    )?)
    .map_err(ImportError::from)
}

fn base_recipe_operation(
    input: &PackageMetadataInput,
    mut parameters: Vec<CanonicalMapEntry>,
) -> Result<(RecipeOperation, &'static [u8]), ImportError> {
    if let Some(seconds) = input.regular_time_step_seconds {
        parameters.push(CanonicalMapEntry::new(
            token("time_step_seconds")?,
            CanonicalValue::from_f64(f64_bits(seconds)?),
        ));
    }
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
            token("all-valid")?,
            token("tczyx")?,
            token("identity")?,
            None,
        ),
        vec![token("base-image")?],
    )?;
    Ok((operation, BASE_OPERATION_REGISTRY_V1))
}

fn no_data_recipe_operation(
    input: &PackageMetadataInput,
    base_parameters: Vec<CanonicalMapEntry>,
) -> Result<(RecipeOperation, &'static [u8]), ImportError> {
    let request = input
        .no_data
        .request()
        .ok_or(ImportError::InvalidRequest("no-data recipe has no request"))?;
    let automatic_mask = input.no_data.automatic_mask();
    let mut parameters = vec![
        CanonicalMapEntry::new(
            token("automatic_block_edge")?,
            CanonicalValue::from_u64(number(5)?),
        ),
        CanonicalMapEntry::new(
            token("automatic_connectivity")?,
            CanonicalValue::from_ascii(token("face-6")?),
        ),
        CanonicalMapEntry::new(
            token("automatic_mask_encoding")?,
            CanonicalValue::from_ascii(token("row-packed-lsb0-zyx")?),
        ),
        CanonicalMapEntry::new(
            token("automatic_mask_present")?,
            CanonicalValue::from_bool(automatic_mask.is_some()),
        ),
        CanonicalMapEntry::new(
            token("automatic_mask_voxels")?,
            CanonicalValue::from_u64(number(
                automatic_mask.map_or(0, |mask| mask.masked_voxels()),
            )?),
        ),
        CanonicalMapEntry::new(
            token("automatic_reconstruction")?,
            CanonicalValue::from_ascii(token("exact-value-components-containing-5-cube")?),
        ),
        CanonicalMapEntry::new(
            token("automatic_scope")?,
            CanonicalValue::from_ascii(token("first-volume-fixed-spatial-mask")?),
        ),
        CanonicalMapEntry::new(
            token("canonical_invalid")?,
            CanonicalValue::from_ascii(token("typed-zero")?),
        ),
        CanonicalMapEntry::new(
            token("constant_z_planes")?,
            CanonicalValue::list(
                input
                    .no_data
                    .constant_z_planes()
                    .iter()
                    .map(|z| Ok(CanonicalValue::from_u64(number(*z)?)))
                    .collect::<Result<Vec<_>, ImportError>>()?,
            )?,
        ),
        CanonicalMapEntry::new(
            token("constant_z_rule")?,
            CanonicalValue::from_ascii(token("exact-whole-plane-equality")?),
        ),
        CanonicalMapEntry::new(
            token("dilation_application")?,
            CanonicalValue::from_ascii(token("base-and-every-lod")?),
        ),
        CanonicalMapEntry::new(
            token("dilation_metric")?,
            CanonicalValue::from_ascii(token("chebyshev")?),
        ),
        CanonicalMapEntry::new(
            token("dilation_radius_xy")?,
            CanonicalValue::from_u64(number(1)?),
        ),
        CanonicalMapEntry::new(
            token("dilation_radius_z_2d")?,
            CanonicalValue::from_u64(number(0)?),
        ),
        CanonicalMapEntry::new(
            token("dilation_radius_z_3d")?,
            CanonicalValue::from_u64(number(1)?),
        ),
        CanonicalMapEntry::new(
            token("hide_constant_z_planes")?,
            CanonicalValue::from_bool(request.hides_constant_z_planes()),
        ),
        CanonicalMapEntry::new(
            token("mean_block")?,
            CanonicalValue::from_ascii(token("aligned-factor-two-reduced-axes")?),
        ),
        CanonicalMapEntry::new(
            token("no_support")?,
            CanonicalValue::from_ascii(token("invalid")?),
        ),
        CanonicalMapEntry::new(
            token("plane_morphology")?,
            CanonicalValue::from_ascii(token("strict-no-dilation")?),
        ),
        CanonicalMapEntry::new(
            token("resolved_value_present")?,
            CanonicalValue::from_bool(input.no_data.value().is_some()),
        ),
        CanonicalMapEntry::new(
            token("value_classification")?,
            CanonicalValue::from_ascii(token(match request.value_rule() {
                Some(NoDataValueRule::Automatic) => "first-volume-fixed-spatial-mask",
                Some(NoDataValueRule::ManualUint8(_)) => "exact-typed-equality",
                None => "disabled",
            })?),
        ),
        CanonicalMapEntry::new(
            token("value_rule")?,
            CanonicalValue::from_ascii(token(match request.value_rule() {
                None => "disabled",
                Some(NoDataValueRule::Automatic) => "automatic-first-volume",
                Some(NoDataValueRule::ManualUint8(_)) => "manual-uint8",
            })?),
        ),
    ];
    parameters.extend(base_parameters);
    if let Some(mask) = automatic_mask {
        parameters.push(CanonicalMapEntry::new(
            token("automatic_mask_sha256")?,
            CanonicalValue::from_ascii(token(&mask.digest().to_string())?),
        ));
    }
    if let Some(value) = input.no_data.value() {
        parameters.push(CanonicalMapEntry::new(
            token("resolved_value")?,
            match value {
                ResolvedNoDataValue::Uint8(value) => {
                    CanonicalValue::from_u64(number(u64::from(value))?)
                }
                ResolvedNoDataValue::Uint16(value) => {
                    CanonicalValue::from_u64(number(u64::from(value))?)
                }
                ResolvedNoDataValue::Float32Bits(bits) => {
                    CanonicalValue::from_f32(F32Bits::parse(&format!("{bits:08x}"))?)
                }
            },
        ));
    }
    if let Some(seconds) = input.regular_time_step_seconds {
        parameters.push(CanonicalMapEntry::new(
            token("time_step_seconds")?,
            CanonicalValue::from_f64(f64_bits(seconds)?),
        ));
    }
    parameters.sort_by(|left, right| left.key().as_str().cmp(right.key().as_str()));

    let operation = RecipeOperation::new(
        number(0)?,
        token("tiff-import-typed-first-volume-no-data")?,
        token("2.0.0")?,
        token("m4d.import.typed-first-volume-no-data.v2")?,
        CanonicalValue::map(parameters)?,
        Vec::new(),
        if input.explicit_validity {
            RecipeNumericPolicy::new(
                token(dtype_name(input.dtype))?,
                token(if input.dtype == IntensityDType::Float32 {
                    "finite-f32"
                } else {
                    "half-up"
                })?,
                token("valid-only-mean")?,
                token("aligned-factor-two-mean")?,
                token("ignore-out-of-bounds")?,
                token("none")?,
                token(match request.value_rule() {
                    Some(NoDataValueRule::Automatic) if automatic_mask.is_some() => {
                        "first-volume-spatial-mask-dilated-plane-strict"
                    }
                    Some(NoDataValueRule::Automatic) => "constant-plane-strict",
                    Some(NoDataValueRule::ManualUint8(_)) => "typed-value-dilated-plane-strict",
                    None => "constant-plane-strict",
                })?,
                token("tczyx")?,
                token(if input.dtype == IntensityDType::Float32 {
                    "finite-f32"
                } else {
                    "exact-integer"
                })?,
                None,
            )
        } else {
            RecipeNumericPolicy::new(
                token(dtype_name(input.dtype))?,
                token("identity")?,
                token("none")?,
                token("aligned-factor-two-point")?,
                token("ignore-out-of-bounds")?,
                token("none")?,
                token("all-valid-after-no-match")?,
                token("tczyx")?,
                token("identity")?,
                None,
            )
        },
        vec![token("multiscale-image")?],
    )?;
    Ok((operation, NO_DATA_OPERATION_REGISTRY_V2))
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
            profile_kind: ProfileKind::Current,
            scientific_content_id: scientific_id(),
            base_shape,
            channel_count: 1,
            channel_labels: vec!["channel 1".to_owned()],
            dtype: IntensityDType::Uint16,
            pyramid_shapes,
            spacing_zyx_um: [0.5, 0.3, 0.2],
            regular_time_step_seconds: Some(2.0),
            explicit_validity: false,
            decoded_source_sha256: Sha256Digest::from_bytes([9; 32]),
            no_data: ResolvedNoDataPolicy::all_valid(base_shape.z()),
        }
    }

    fn manual_u8_policy(value: u8, depth: u64) -> ResolvedNoDataPolicy {
        ResolvedNoDataPolicy::new(
            Some(crate::NoDataPolicy::manual_uint8(value)),
            Some(ResolvedNoDataValue::Uint8(value)),
            None,
            Vec::new(),
            depth,
        )
        .unwrap()
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
    fn package_metadata_accepts_a_geometry_required_15_level_pyramid() {
        let shapes = [
            1_048_576, 524_288, 262_144, 131_072, 65_536, 32_768, 16_384, 8_192, 4_096, 2_048,
            1_024, 512, 256, 128, 64,
        ]
        .into_iter()
        .map(|x| Shape4D::new(1, 1, 1, x).unwrap())
        .collect::<Vec<_>>();
        let metadata = build_package_metadata(&input(shapes[0], shapes)).unwrap();

        assert_eq!(metadata.profile.images()[0].levels().len(), 15);
        assert_eq!(metadata.ome_images[0].level_transforms().len(), 15);
        assert_eq!(metadata.arrays.len(), 30);
        assert!(
            String::from_utf8(metadata.ome_images[0].deterministic_bytes().unwrap())
                .unwrap()
                .contains("\"path\":\"s14\"")
        );
    }

    #[test]
    fn builds_2d_explicit_validity_arrays_for_all_channels() {
        let base = Shape4D::new(1, 1, 512, 512).unwrap();
        let coarse = Shape4D::new(1, 1, 256, 256).unwrap();
        let mut input = input(base, vec![base, coarse]);
        input.channel_count = 2;
        input.channel_labels = vec!["channel 1".to_owned(), "channel 2".to_owned()];
        input.dtype = IntensityDType::Uint8;
        input.explicit_validity = true;
        input.no_data = manual_u8_policy(255, base.z());
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
                    f64_bits(0.5).unwrap(),
                    f64_bits(0.6).unwrap(),
                    f64_bits(0.4).unwrap()
                ],
                translation_zyx: [
                    f64_bits(0.0).unwrap(),
                    f64_bits(0.15).unwrap(),
                    f64_bits(0.1).unwrap(),
                ],
            }
        );
    }

    #[test]
    fn sentinel_mean_transforms_center_only_axes_reduced_at_each_level() {
        let s0 = Shape4D::new(1, 2, 5, 9).unwrap();
        let s1 = Shape4D::new(1, 1, 3, 5).unwrap();
        let s2 = Shape4D::new(1, 1, 2, 3).unwrap();
        let s3 = Shape4D::new(1, 1, 1, 2).unwrap();
        let mut input = input(s0, vec![s0, s1, s2, s3]);
        input.profile_kind = ProfileKind::Current;
        input.dtype = IntensityDType::Uint8;
        input.explicit_validity = true;
        input.no_data = manual_u8_policy(255, s0.z());

        let metadata = build_package_metadata(&input).unwrap();
        assert_eq!(
            metadata.ome_images[0].level_transforms(),
            [
                centered_transform([0.5, 0.3, 0.2], [1, 1, 1]),
                centered_transform([0.5, 0.3, 0.2], [2, 2, 2]),
                centered_transform([0.5, 0.3, 0.2], [2, 4, 4]),
                centered_transform([0.5, 0.3, 0.2], [2, 8, 8]),
            ]
        );
    }

    #[test]
    fn no_sentinel_transform_remains_origin_anchored_and_uniform() {
        let base = Shape4D::new(1, 1, 8, 8).unwrap();
        let coarse = Shape4D::new(1, 1, 4, 4).unwrap();
        let metadata = build_package_metadata(&input(base, vec![base, coarse])).unwrap();

        assert_eq!(
            metadata.ome_images[0].level_transforms()[1],
            diagonal_transform([1.0, 0.6, 0.4], [0.0; 3])
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
    }

    #[test]
    fn records_decoded_source_identity_executable_build_and_base_only_recipe() {
        let base = Shape4D::new(1, 3, 8, 8).unwrap();
        let mut input = input(base, vec![base]);
        let decoded_source_digest = Sha256Digest::from_bytes([3; 32]);
        input.decoded_source_sha256 = decoded_source_digest;
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
            vec![decoded_source_digest.to_string()]
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
            vec!["decoded-source"]
        );
        assert_eq!(
            derivation
                .body()
                .inputs()
                .iter()
                .map(DerivationBinding::id)
                .collect::<Vec<_>>(),
            vec![TypedId::ExactBytes(ExactBytesDigest::from_digest(
                decoded_source_digest,
            ))]
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
        assert_eq!(
            recipe.recipe_id().to_string(),
            "m4d-recipe-v1-sha256:89ab81cae5f2424aa965b5ec36979b0e1ee251632db008145ed9ec60b1a276ff"
        );
        let operation = &recipe.body().operations()[0];
        assert_eq!(operation.name().as_str(), "tiff-import-canonical-base");
        assert_eq!(operation.semantic_version().as_str(), "1.0.0");
        assert_eq!(operation.parameter_schema().as_str(), "m4d.import.base.v1");
        assert_eq!(operation.output_roles()[0].as_str(), "base-image");
        assert_eq!(
            recipe.body().operation_registry_digest(),
            ExactBytesDigest::from_digest(Sha256Hasher::digest(
                b"mirante4d-import-pipeline-base-operation-registry-v1"
            ))
        );
        let policy = operation.numeric_policy();
        assert_eq!(policy.rounding().as_str(), "identity");
        assert_eq!(policy.reduction().as_str(), "none");
        assert_eq!(policy.kernel().as_str(), "identity");
        assert_eq!(policy.boundary().as_str(), "none");
        assert_eq!(policy.interpolation().as_str(), "none");
        assert_eq!(policy.no_data().as_str(), "all-valid");
        assert_eq!(policy.precision().as_str(), "identity");
    }

    #[test]
    fn typed_no_data_recipe_records_the_complete_resolved_policy() {
        let base = Shape4D::new(1, 1, 9, 11).unwrap();
        let coarse = Shape4D::new(1, 1, 5, 6).unwrap();
        let mut input = input(base, vec![base, coarse]);
        input.dtype = IntensityDType::Uint8;
        input.explicit_validity = true;
        input.no_data = manual_u8_policy(253, base.z());

        let recipe = recipe(&input).unwrap();
        let operation = &recipe.body().operations()[0];
        assert_eq!(
            operation.name().as_str(),
            "tiff-import-typed-first-volume-no-data"
        );
        assert_eq!(operation.semantic_version().as_str(), "2.0.0");
        assert_eq!(
            operation.parameter_schema().as_str(),
            "m4d.import.typed-first-volume-no-data.v2"
        );
        assert_eq!(operation.output_roles()[0].as_str(), "multiscale-image");
        assert_eq!(
            recipe.body().operation_registry_digest(),
            ExactBytesDigest::from_digest(Sha256Hasher::digest(
                b"mirante4d-import-pipeline-typed-first-volume-no-data-operation-registry-v2"
            ))
        );

        let parameter_names = operation
            .parameters()
            .as_map()
            .unwrap()
            .iter()
            .map(|entry| entry.key().as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            parameter_names,
            [
                "automatic_block_edge",
                "automatic_connectivity",
                "automatic_mask_encoding",
                "automatic_mask_present",
                "automatic_mask_voxels",
                "automatic_reconstruction",
                "automatic_scope",
                "canonical_invalid",
                "constant_z_planes",
                "constant_z_rule",
                "dilation_application",
                "dilation_metric",
                "dilation_radius_xy",
                "dilation_radius_z_2d",
                "dilation_radius_z_3d",
                "hide_constant_z_planes",
                "mean_block",
                "no_support",
                "plane_morphology",
                "resolved_value",
                "resolved_value_present",
                "spacing_x_um",
                "spacing_y_um",
                "spacing_z_um",
                "time_step_seconds",
                "value_classification",
                "value_rule",
            ]
        );
        assert_eq!(parameter_u64(operation, "automatic_block_edge"), 5);
        assert_eq!(
            parameter_ascii(operation, "automatic_connectivity"),
            "face-6"
        );
        assert_eq!(
            parameter_ascii(operation, "automatic_mask_encoding"),
            "row-packed-lsb0-zyx"
        );
        assert_eq!(
            parameter(operation, "automatic_mask_present").as_bool(),
            Some(false)
        );
        assert_eq!(parameter_u64(operation, "automatic_mask_voxels"), 0);
        assert_eq!(
            parameter_ascii(operation, "automatic_reconstruction"),
            "exact-value-components-containing-5-cube"
        );
        assert_eq!(
            parameter_ascii(operation, "automatic_scope"),
            "first-volume-fixed-spatial-mask"
        );
        assert_eq!(
            parameter_ascii(operation, "canonical_invalid"),
            "typed-zero"
        );
        assert_eq!(
            parameter_ascii(operation, "dilation_application"),
            "base-and-every-lod"
        );
        assert_eq!(parameter_ascii(operation, "dilation_metric"), "chebyshev");
        assert_eq!(parameter_u64(operation, "dilation_radius_xy"), 1);
        assert_eq!(parameter_u64(operation, "dilation_radius_z_2d"), 0);
        assert_eq!(parameter_u64(operation, "dilation_radius_z_3d"), 1);
        assert_eq!(
            parameter_ascii(operation, "mean_block"),
            "aligned-factor-two-reduced-axes"
        );
        assert_eq!(parameter_ascii(operation, "no_support"), "invalid");
        assert_eq!(
            parameter_ascii(operation, "value_classification"),
            "exact-typed-equality"
        );
        assert_eq!(parameter_u64(operation, "resolved_value"), 253);
        assert_eq!(parameter_ascii(operation, "value_rule"), "manual-uint8");
        assert_eq!(
            parameter_ascii(operation, "plane_morphology"),
            "strict-no-dilation"
        );

        let policy = operation.numeric_policy();
        assert_eq!(policy.dtype().as_str(), "uint8");
        assert_eq!(policy.rounding().as_str(), "half-up");
        assert_eq!(policy.reduction().as_str(), "valid-only-mean");
        assert_eq!(policy.kernel().as_str(), "aligned-factor-two-mean");
        assert_eq!(policy.boundary().as_str(), "ignore-out-of-bounds");
        assert_eq!(policy.interpolation().as_str(), "none");
        assert_eq!(
            policy.no_data().as_str(),
            "typed-value-dilated-plane-strict"
        );
        assert_eq!(policy.ordering().as_str(), "tczyx");
        assert_eq!(policy.precision().as_str(), "exact-integer");
    }

    #[test]
    fn automatic_no_match_recipe_records_resolution_and_all_valid_point_sampling() {
        let base = Shape4D::new(1, 3, 9, 11).unwrap();
        let coarse = Shape4D::new(1, 2, 5, 6).unwrap();
        let mut input = input(base, vec![base, coarse]);
        input.no_data = ResolvedNoDataPolicy::new(
            Some(crate::NoDataPolicy::automatic()),
            None,
            None,
            Vec::new(),
            base.z(),
        )
        .unwrap();

        let recipe = recipe(&input).unwrap();
        let operation = &recipe.body().operations()[0];
        assert_eq!(
            parameter_ascii(operation, "value_rule"),
            "automatic-first-volume"
        );
        assert_eq!(
            parameter(operation, "resolved_value_present").as_bool(),
            Some(false)
        );
        assert_eq!(
            parameter(operation, "automatic_mask_present").as_bool(),
            Some(false)
        );
        assert_eq!(operation.numeric_policy().reduction().as_str(), "none");
        assert_eq!(
            operation.numeric_policy().kernel().as_str(),
            "aligned-factor-two-point"
        );
        assert_eq!(
            operation.numeric_policy().no_data().as_str(),
            "all-valid-after-no-match"
        );
    }

    #[test]
    fn automatic_recipe_binds_the_reconstructed_spatial_mask() {
        let base = Shape4D::new(1, 5, 5, 5).unwrap();
        let coarse = Shape4D::new(1, 3, 3, 3).unwrap();
        let mut input = input(base, vec![base, coarse]);
        let bits = vec![0x1f; 25];
        let mask = crate::model::ResolvedAutomaticNoDataMask::new([5, 5, 5], bits).unwrap();
        let expected_digest = mask.digest().to_string();
        input.explicit_validity = true;
        input.no_data = ResolvedNoDataPolicy::new(
            Some(crate::NoDataPolicy::automatic()),
            Some(ResolvedNoDataValue::Uint16(42)),
            Some(mask),
            Vec::new(),
            base.z(),
        )
        .unwrap();

        let recipe = recipe(&input).unwrap();
        let operation = &recipe.body().operations()[0];
        assert_eq!(
            parameter_ascii(operation, "value_classification"),
            "first-volume-fixed-spatial-mask"
        );
        assert_eq!(
            parameter(operation, "automatic_mask_present").as_bool(),
            Some(true)
        );
        assert_eq!(parameter_u64(operation, "automatic_mask_voxels"), 125);
        assert_eq!(
            parameter_ascii(operation, "automatic_mask_sha256"),
            expected_digest
        );
        assert_eq!(
            operation.numeric_policy().no_data().as_str(),
            "first-volume-spatial-mask-dilated-plane-strict"
        );
    }

    fn diagonal_transform(scale: [f64; 3], translation: [f64; 3]) -> OmeLevelTransform {
        OmeLevelTransform::DiagonalMicrometer {
            scale_zyx: f64_bits3(scale).unwrap(),
            translation_zyx: f64_bits3(translation).unwrap(),
        }
    }

    fn centered_transform(spacing: [f64; 3], factors: [u64; 3]) -> OmeLevelTransform {
        diagonal_transform(
            std::array::from_fn(|axis| spacing[axis] * factors[axis] as f64),
            std::array::from_fn(|axis| spacing[axis] * (factors[axis] - 1) as f64 / 2.0),
        )
    }

    fn parameter<'a>(operation: &'a RecipeOperation, name: &str) -> &'a CanonicalValue {
        operation
            .parameters()
            .as_map()
            .unwrap()
            .iter()
            .find(|entry| entry.key().as_str() == name)
            .unwrap()
            .value()
    }

    fn parameter_ascii<'a>(operation: &'a RecipeOperation, name: &str) -> &'a str {
        parameter(operation, name).as_ascii().unwrap().as_str()
    }

    fn parameter_u64(operation: &RecipeOperation, name: &str) -> u64 {
        parameter(operation, name).as_u64().unwrap().get()
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
