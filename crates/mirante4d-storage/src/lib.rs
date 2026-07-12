//! Strict off-product storage profile for Mirante4D datasets.
//!
//! This crate is not reachable from the application before WP-10C. The first
//! WP-10A-B owns immutable profile facts, strict control primitives, checked
//! preflight arithmetic, and portable package paths. It performs no filesystem
//! I/O and makes no target-package support claim.

#![forbid(unsafe_code)]

mod control;
mod error;
mod limits;
mod paths;
mod profile;

pub use control::{
    AsciiToken, CanonicalMapEntry, CanonicalValue, CanonicalValueKind, ControlError,
    DisplayDefaults, DisplayLayerDefaults, F32Bits, F64Bits, I64Decimal, MAX_ASCII_TOKEN_BYTES,
    MAX_NFC_TEXT_BYTES, MAX_PORTABLE_CONTROL_OBJECT_BYTES, MAX_PROFILE_HEADER_BYTES, ManifestPage,
    ManifestPageReference, ManifestRoot, NfcText, OmeInteroperabilityBase, PackageObjectDescriptor,
    PackageObjectKind, ProfileHeader, ProfileImage, ProfileLevel, ProfileLogicalLayer,
    ProfileValidityMode, RecipeBody, RecipeDeterminism, RecipeInput, RecipeNumericPolicy,
    RecipeOperation, RecipePayload, RecipeRng, Rgb24, ScienceDescriptor, ScienceLayer,
    ScienceTemporalCalibration, ScienceTemporalKind, TypedId, U64Decimal, manifest_page_path,
    pack_manifest_pages, profile_compatibility_bytes,
};
pub use error::StorageProfileError;
pub use limits::{
    DatasetGeometry, ELIDED_ALL_FILL_AMPLIFICATION, ElidedAllFillAmplification,
    FIXED_CONTROL_OBJECTS, GLOBAL_ENCODED_OUTER_SHARD_BYTES_MAX,
    GLOBAL_UNCOMPRESSED_OUTER_SHARD_BYTES_MAX, MANIFEST_DESCRIPTORS_PER_PAGE_GUARANTEED,
    OneBrickAmplification, PACKED_INDEX_RECORD_BYTES, PACKED_INDEX_RECORDS_PER_INNER_CHUNK,
    PACKED_INDEX_RECORDS_PER_OUTER_SHARD, PORTABLE_PROVENANCE_RECORDS_MAX, PackageCounts,
    ProfileLimits, ScaleCounts, amplification_2d, amplification_3d, checked_ceil_div,
    count_3d_pyramid, encoded_inner_payload_limit, encoded_outer_shard_limit,
};
pub use paths::{
    MAX_DIRECTORY_DEPTH, MAX_FILE_PATH_COMPONENTS, MAX_RELATIVE_PATH_BYTES, PackagePath,
    validate_unique_paths,
};
pub use profile::{
    CAPABILITIES, CHUNK_KEY_SEPARATOR, CompatibilityTuple, INDEX_CODECS, INDEX_LOCATION,
    INNER_CODECS, OUTER_CODEC, PROFILE, ProfileKind, ScaleCountRule, StorageShape, profile_limits,
};
