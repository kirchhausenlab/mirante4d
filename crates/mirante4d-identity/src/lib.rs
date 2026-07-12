//! Typed identities, canonical scientific preimages, and bounded Merkle
//! hashing for Mirante4D.
//!
//! This crate is pure computation. It owns no filesystem, serialization,
//! storage, runtime, UI, or device behavior.

#![forbid(unsafe_code)]

mod artifact;
mod canonical;
mod digest;
mod exact;
mod hash;
mod object;
mod scientific;

pub use artifact::{
    WP10A_ARTIFACT_HAND_VECTOR_BODY, WP10A_ARTIFACT_HAND_VECTOR_ID,
    verify_wp10a_artifact_hand_vector,
};
pub use canonical::{M4D_UNICODE_VERSION, is_nfc, normalize_nfc};
pub use digest::{
    ArtifactContentId, DerivationRecordId, ExactBytesDigest, PackageId, RecipeId, ReleaseId,
    ScientificContentId, Sha256Digest,
};
pub use exact::{ExactBytesFacts, ExactBytesHasher, IdentityHashError};
pub use hash::Sha256Hasher;
pub use object::{
    MAX_MEDIA_TYPE_BYTES, MAX_OBJECT_ROLE_BYTES, MediaType, ObjectRole, RawObjectDescriptor,
};
pub use scientific::{
    SCIENTIFIC_TILE_SHAPE_TZYX, ScientificDatasetHasher, ScientificHashError,
    ScientificLayerDescriptor, ScientificLayerHasher, ScientificLayerRoot,
    ScientificTemporalCalibration, ScientificTile,
};

use thiserror::Error;

/// A validation error for an identity or typed object value.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum IdentityError {
    #[error(
        "a SHA-256 digest must contain exactly 64 lowercase hexadecimal characters, got {actual}"
    )]
    InvalidSha256Length { actual: usize },
    #[error("invalid lowercase hexadecimal byte 0x{byte:02x} at SHA-256 digest offset {index}")]
    InvalidSha256Character { index: usize, byte: u8 },
    #[error("identity must start with {expected:?}")]
    InvalidIdentityPrefix { expected: &'static str },
    #[error("invalid media type: {reason}")]
    InvalidMediaType { reason: &'static str },
    #[error("invalid object role: {reason}")]
    InvalidObjectRole { reason: &'static str },
}
