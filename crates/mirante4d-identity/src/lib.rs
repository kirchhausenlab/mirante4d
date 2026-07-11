//! Typed identity and package-object descriptor values.
//!
//! This crate deliberately parses and carries already-computed SHA-256
//! identities. Canonical preimages and hashing belong to later format and
//! storage work.

#![forbid(unsafe_code)]

mod digest;
mod object;

pub use digest::{
    ArtifactContentId, DerivationRecordId, ExactBytesDigest, PackageId, RecipeId, ReleaseId,
    ScientificContentId, Sha256Digest,
};
pub use object::{
    MAX_MEDIA_TYPE_BYTES, MAX_OBJECT_PATH_BYTES, MAX_OBJECT_ROLE_BYTES, MediaType, ObjectPath,
    ObjectRole, PackageObjectDescriptor, RawObjectDescriptor,
};

use thiserror::Error;

/// A validation error for an identity or package-object descriptor value.
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
    #[error("invalid object path: {reason}")]
    InvalidObjectPath { reason: &'static str },
    #[error("invalid media type: {reason}")]
    InvalidMediaType { reason: &'static str },
    #[error("invalid object role: {reason}")]
    InvalidObjectRole { reason: &'static str },
}
