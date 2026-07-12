//! Strict off-product storage profile for Mirante4D datasets.
//!
//! This crate is not reachable from the application before WP-10C. The first
//! WP-10A-B owns immutable profile facts, strict control primitives, checked
//! preflight arithmetic, portable package paths, packed-index records, the
//! in-memory shard codec, strict Zarr/OME storage metadata, an authenticated
//! local metadata catalog, a bounded exact directory inventory, and root-
//! confined read-only range I/O with descriptor-derived brick address plans.
//! It has no writer and makes no product target-package claim.

#![forbid(unsafe_code)]

mod brick_address;
mod control;
mod directory_inventory;
mod error;
mod limits;
mod ome_metadata;
mod package_catalog;
// Staged behind the future full-SHA package-integrity capability.
#[cfg_attr(not(test), allow(dead_code))]
mod package_read;
mod packed_index;
mod paths;
mod profile;
mod range_io;
mod shard;
mod zarr_metadata;

pub use brick_address::{BrickAddressError, LocalBrickAddressPlan};
pub use control::{
    AsciiToken, CanonicalMapEntry, CanonicalValue, CanonicalValueKind, CitationPayload,
    ControlError, DatasetSeriesUuid, DerivationBinding, DerivationBody, DerivationExactness,
    DerivationImplementation, DerivationOutcome, DerivationPayload, DerivationScope,
    DerivationSpaceBox, DerivationTimeRange, DisplayDefaults, DisplayLayerDefaults, Doi, F32Bits,
    F64Bits, I64Decimal, MAX_ASCII_TOKEN_BYTES, MAX_NFC_TEXT_BYTES,
    MAX_PORTABLE_CONTROL_OBJECT_BYTES, MAX_PROFILE_HEADER_BYTES, ManifestPage,
    ManifestPageReference, ManifestRoot, NfcText, OmeInteroperabilityBase, PackageObjectDescriptor,
    PackageObjectKind, PortableRecord, PortableRecordKind, PortableRecordPayload, ProfileHeader,
    ProfileImage, ProfileLevel, ProfileLogicalLayer, ProfileValidityMode, PublishedAtUtc,
    RecipeBody, RecipeDeterminism, RecipeInput, RecipeNumericPolicy, RecipeOperation,
    RecipePayload, RecipeRng, ReleaseBody, ReleaseCitation, ReleaseEvidence, ReleasePayload, Rgb24,
    RightsPayload, ScienceDescriptor, ScienceLayer, ScienceTemporalCalibration,
    ScienceTemporalKind, SourceIdentifier, SourceIdentifierScheme, SourcePayload, SpdxLicense,
    TypedId, U64Decimal, manifest_page_path, pack_manifest_pages, profile_compatibility_bytes,
};
pub use directory_inventory::{DirectoryInventory, DirectoryInventoryError};
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
pub use ome_metadata::{OmeImageGroupMetadata, OmeLevelTransform};
pub use package_catalog::{LocalPackageCatalog, PackageOpenError};
pub use packed_index::{
    PackedIndexCoordinates, PackedIndexError, PackedIndexRecord, PackedIndexStatistics,
};
pub use paths::{
    MAX_DIRECTORY_DEPTH, MAX_FILE_PATH_COMPONENTS, MAX_RELATIVE_PATH_BYTES, PackagePath,
    validate_unique_paths,
};
pub use profile::{
    CAPABILITIES, CHUNK_KEY_SEPARATOR, CompatibilityTuple, INDEX_CODECS, INDEX_LOCATION,
    INNER_CODECS, OUTER_CODEC, PROFILE, ProfileKind, ScaleCountRule, StorageShape, profile_limits,
};
pub use range_io::{
    LocalObjectInfo, LocalPackageReader, RangeReadError, SHARD_INDEX_RANGE_READ_BYTES_MAX,
};
pub use shard::{
    ShardCodecError, ShardIndex, ShardIndexEntry, ShardProfileKind, decode_inner_payload,
    decode_shard_index_tail, encode_inner_payload,
};
pub use zarr_metadata::{
    MAX_ZARR_METADATA_BYTES, ZarrArrayMetadata, ZarrGroupMetadata, ZarrMetadataError,
};
