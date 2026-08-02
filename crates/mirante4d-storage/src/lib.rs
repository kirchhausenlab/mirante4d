//! Strict active storage profile for Mirante4D datasets.
//!
//! This crate owns immutable profile facts, strict control primitives, checked
//! limits, portable package paths, packed indexes and shards, strict Zarr/OME
//! metadata, checksum-checked bounded catalog and range reads, exact-package
//! integrity and canonical-content validation, and create-only local publication.

#![forbid(unsafe_code)]

mod brick_address;
mod control;
mod dataset_source;
mod directory_inventory;
mod error;
mod limits;
mod local_publication;
mod ome_metadata;
mod package_admission;
mod package_catalog;
mod package_integrity;
mod package_read;
mod package_science;
mod package_structure;
mod package_validation_reads;
mod package_write;
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
pub use dataset_source::{
    LocalDatasetSource, LocalDatasetSourceDiagnostics, LocalDatasetSourceOpenError,
    PACKAGE_VALIDATION_WORKING_BYTES,
};
pub use directory_inventory::{DirectoryInventory, DirectoryInventoryError};
pub use error::StorageProfileError;
pub use limits::{
    COMPOSITIONAL_DIRECTORIES_MAX, COMPOSITIONAL_DIRECTORY_FAN_OUT_MAX,
    COMPOSITIONAL_LOGICAL_BRICKS_MAX, COMPOSITIONAL_PHYSICAL_OBJECTS_MAX,
    COMPOSITIONAL_SHARDS_PER_COMPONENT_MAX, COMPOSITIONAL_ZARR_METADATA_OBJECTS_MAX,
    DatasetGeometry, ELIDED_ALL_FILL_AMPLIFICATION, ElidedAllFillAmplification,
    FIXED_CONTROL_OBJECTS, GLOBAL_ENCODED_OUTER_SHARD_BYTES_MAX,
    GLOBAL_UNCOMPRESSED_OUTER_SHARD_BYTES_MAX, MANIFEST_DESCRIPTOR_WORKING_BYTES,
    MANIFEST_DESCRIPTOR_WORKING_SET_BYTES_MAX, MANIFEST_DESCRIPTORS_MAX,
    MANIFEST_DESCRIPTORS_PER_PAGE_GUARANTEED, MANIFEST_FORMAT_DESCRIPTORS_MAX,
    MANIFEST_PAGE_REFERENCES_MAX, OneBrickAmplification, PACKED_INDEX_RECORD_BYTES,
    PACKED_INDEX_RECORDS_PER_INNER_CHUNK, PACKED_INDEX_RECORDS_PER_OUTER_SHARD,
    PORTABLE_PROVENANCE_RECORDS_MAX, PROFILE_PYRAMID_SCALE_COUNT_MAX,
    PROFILE_PYRAMID_TERMINAL_MAX_DIMENSION, PROFILE_PYRAMID_TERMINAL_VOXELS_PER_TIMEPOINT,
    PackageCounts, ProfileLimits, ScaleCounts, amplification_2d, amplification_3d,
    checked_ceil_div, count_3d_profile_pyramid, count_3d_pyramid, encoded_inner_payload_limit,
    encoded_outer_shard_limit, profile_pyramid_shape_is_terminal, profile_pyramid_shapes,
};
pub use ome_metadata::{OmeImageGroupMetadata, OmeLevelTransform};
pub use package_admission::{DatasetProfileAdmission, PackageAdmissionError};
pub use package_catalog::{LocalPackageCatalog, PackageOpenError};
pub use package_integrity::{
    ExactPackageCapability, ExactPackageValidationProgress, PackageValidationError,
};
pub use package_read::{LocalBrickRead, PackageReadError};
pub use package_science::{
    SCIENTIFIC_PUBLICATION_CURRENTNESS_CONTRACT_ID, ScientificPackageValidationError,
    ScientificPublicationTransferError, ScientificPublicationTransferEvidence,
    ScientificValidationProgress, ScientificValidationProgressStage, ScientificValidationReport,
    SelfConsistentPackageCapability,
};
pub use package_structure::PackageStructureError;
pub use package_validation_reads::{PackageCodecReport, PackageValidationReadReport};
pub use package_write::{
    LocalPackageWriter, PackageArrayInput, PackageShardInput, PackageWriteError, PackageWriteEvent,
    PackageWriteInput, PackageWriteReceipt, PackageWriteStage, PackageWriteStageTiming,
    PublishedScientificPackageTransfer, ResumableLocalPackageStage,
};
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
    LOCAL_OBJECT_CACHE_ACCOUNTED_BYTES_MAX, LOCAL_OBJECT_CACHE_ENTRY_BYTES_MAX,
    LOCAL_OBJECT_HANDLE_CACHE_MAX, LocalObjectInfo, LocalPackageReadDiagnostics,
    LocalPackageReader, RangeReadError, SHARD_INDEX_RANGE_READ_BYTES_MAX,
};
pub use shard::{
    CanonicalEncodedInner, INNER_CODEC_WORKING_BYTES_MAX, ShardCodecError, ShardIndex,
    ShardIndexEntry, ShardProfileKind, decode_inner_payload, decode_shard_index_tail,
    encode_inner_payload,
};
pub use zarr_metadata::{
    MAX_ZARR_METADATA_BYTES, ZarrArrayMetadata, ZarrGroupMetadata, ZarrMetadataError,
};
