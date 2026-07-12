//! Strict off-product storage profile for Mirante4D datasets.
//!
//! This crate is not reachable from the application before WP-10C. The first
//! WP-10A slice owns only immutable profile facts, checked preflight arithmetic,
//! and portable package paths; it performs no filesystem I/O.

#![forbid(unsafe_code)]

mod error;
mod limits;
mod paths;
mod profile;

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
