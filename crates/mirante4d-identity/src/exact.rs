use thiserror::Error;

use crate::{
    DerivationRecordId, ExactBytesDigest, PackageId, RecipeId, ReleaseId, Sha256Digest,
    Sha256Hasher,
};

const RECIPE_DOMAIN: &[u8] = b"M4D-RECIPE-V1\0";
const DERIVATION_RECORD_DOMAIN: &[u8] = b"M4D-DERIVATION-RECORD-V1\0";
const RELEASE_DOMAIN: &[u8] = b"M4D-RELEASE-V1\0";

/// A checked failure while constructing an exact-byte identity.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum IdentityHashError {
    #[error("exact byte length exceeds the version-1 u64 framing limit")]
    ByteLengthOverflow,
    #[error("the exact-byte hasher previously failed and cannot be finalized")]
    PreviouslyFailed,
}

/// The digest and checked byte length of one exact byte sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExactBytesFacts {
    digest: ExactBytesDigest,
    byte_length: u64,
}

impl ExactBytesFacts {
    pub const fn digest(self) -> ExactBytesDigest {
        self.digest
    }

    pub const fn byte_length(self) -> u64 {
        self.byte_length
    }
}

/// Incrementally hashes exact object bytes while counting their length.
///
/// This type performs no I/O and applies no canonicalization or domain prefix.
#[derive(Clone, Debug)]
pub struct ExactBytesHasher {
    hasher: Option<Sha256Hasher>,
    byte_length: u64,
}

impl ExactBytesHasher {
    pub fn new() -> Self {
        Self {
            hasher: Some(Sha256Hasher::new()),
            byte_length: 0,
        }
    }

    /// Adds one exact byte slice.
    ///
    /// Length overflow rejects the entire chunk and permanently poisons this
    /// computation so an incomplete digest cannot later be finalized.
    pub fn update(&mut self, bytes: &[u8]) -> Result<(), IdentityHashError> {
        if self.hasher.is_none() {
            return Err(IdentityHashError::PreviouslyFailed);
        }
        let added = match u64::try_from(bytes.len()) {
            Ok(added) => added,
            Err(_) => return self.poison(),
        };
        let Some(byte_length) = self.byte_length.checked_add(added) else {
            return self.poison();
        };
        self.hasher
            .as_mut()
            .expect("the failed state was rejected above")
            .update(bytes);
        self.byte_length = byte_length;
        Ok(())
    }

    pub fn finalize(self) -> Result<ExactBytesFacts, IdentityHashError> {
        let hasher = self.hasher.ok_or(IdentityHashError::PreviouslyFailed)?;
        Ok(ExactBytesFacts {
            digest: ExactBytesDigest::from_digest(hasher.finalize()),
            byte_length: self.byte_length,
        })
    }

    pub fn hash(bytes: &[u8]) -> Result<ExactBytesFacts, IdentityHashError> {
        let mut hasher = Self::new();
        hasher.update(bytes)?;
        hasher.finalize()
    }

    fn poison(&mut self) -> Result<(), IdentityHashError> {
        self.hasher = None;
        Err(IdentityHashError::ByteLengthOverflow)
    }
}

impl Default for ExactBytesHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl PackageId {
    /// Computes a package identity from exact canonical manifest-root bytes.
    ///
    /// The caller must validate the root schema and JCS encoding before
    /// calling; this method deliberately hashes the supplied bytes exactly.
    pub fn from_manifest_root_bytes(bytes: &[u8]) -> Self {
        Self::from_digest(Sha256Hasher::digest(bytes))
    }

    pub fn matches_manifest_root_bytes(self, bytes: &[u8]) -> bool {
        self == Self::from_manifest_root_bytes(bytes)
    }
}

impl RecipeId {
    /// Computes an identity from an already-validated canonical JCS body.
    pub fn from_canonical_body_bytes(bytes: &[u8]) -> Result<Self, IdentityHashError> {
        framed_body_digest(RECIPE_DOMAIN, bytes).map(Self::from_digest)
    }

    pub fn matches_canonical_body_bytes(self, bytes: &[u8]) -> Result<bool, IdentityHashError> {
        Self::from_canonical_body_bytes(bytes).map(|actual| self == actual)
    }
}

impl DerivationRecordId {
    /// Computes an identity from an already-validated canonical JCS body.
    pub fn from_canonical_body_bytes(bytes: &[u8]) -> Result<Self, IdentityHashError> {
        framed_body_digest(DERIVATION_RECORD_DOMAIN, bytes).map(Self::from_digest)
    }

    pub fn matches_canonical_body_bytes(self, bytes: &[u8]) -> Result<bool, IdentityHashError> {
        Self::from_canonical_body_bytes(bytes).map(|actual| self == actual)
    }
}

impl ReleaseId {
    /// Computes an identity from an already-validated canonical JCS body.
    pub fn from_canonical_body_bytes(bytes: &[u8]) -> Result<Self, IdentityHashError> {
        framed_body_digest(RELEASE_DOMAIN, bytes).map(Self::from_digest)
    }

    pub fn matches_canonical_body_bytes(self, bytes: &[u8]) -> Result<bool, IdentityHashError> {
        Self::from_canonical_body_bytes(bytes).map(|actual| self == actual)
    }
}

fn framed_body_digest(domain: &[u8], bytes: &[u8]) -> Result<Sha256Digest, IdentityHashError> {
    let byte_length =
        u64::try_from(bytes.len()).map_err(|_| IdentityHashError::ByteLengthOverflow)?;
    let mut hasher = Sha256Hasher::new();
    hasher.update(domain);
    hasher.update(byte_length.to_be_bytes());
    hasher.update(bytes);
    Ok(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_object_and_package_ids_hash_only_the_supplied_bytes() {
        let empty = ExactBytesHasher::hash(b"").unwrap();
        assert_eq!(empty.byte_length(), 0);
        assert_eq!(
            empty.digest().to_string(),
            concat!(
                "sha256:",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            )
        );
        assert_eq!(
            PackageId::from_manifest_root_bytes(b"{}").to_string(),
            concat!(
                "m4d-package-v1-sha256:",
                "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
            )
        );
    }

    #[test]
    fn exact_byte_counting_is_chunk_invariant_and_failure_poisoned() {
        let mut split = ExactBytesHasher::new();
        split.update(b"mira").unwrap();
        split.update(b"nte4d").unwrap();
        assert_eq!(split.finalize(), ExactBytesHasher::hash(b"mirante4d"));

        let mut overflow = ExactBytesHasher {
            hasher: Some(Sha256Hasher::new()),
            byte_length: u64::MAX,
        };
        assert_eq!(
            overflow.update(&[0]),
            Err(IdentityHashError::ByteLengthOverflow)
        );
        assert_eq!(
            overflow.update(&[]),
            Err(IdentityHashError::PreviouslyFailed)
        );
        assert_eq!(
            overflow.finalize(),
            Err(IdentityHashError::PreviouslyFailed)
        );
    }

    #[test]
    fn canonical_body_ids_use_the_frozen_domains_length_and_exact_bytes() {
        let body = b"{}";
        let recipe = RecipeId::from_canonical_body_bytes(body).unwrap();
        assert_eq!(
            recipe.to_string(),
            concat!(
                "m4d-recipe-v1-sha256:",
                "c1a685620e60cdd8e5e4fbfe3d02a3f9a609adf88aff618aa362474334b96818"
            )
        );
        assert_eq!(
            DerivationRecordId::from_canonical_body_bytes(body)
                .unwrap()
                .to_string(),
            concat!(
                "m4d-derivation-record-v1-sha256:",
                "40be75e5f20c6769c991e88493b2f09da4dd986a513234413b964c5abbbccf4e"
            )
        );
        assert_eq!(
            ReleaseId::from_canonical_body_bytes(body)
                .unwrap()
                .to_string(),
            concat!(
                "m4d-release-v1-sha256:",
                "f8199d7cbeec6149afcf5e6ed26e109197aac117a7b13d53447ea029baa8e399"
            )
        );
        assert!(recipe.matches_canonical_body_bytes(body).unwrap());
        assert!(!recipe.matches_canonical_body_bytes(b"{}\n").unwrap());
    }
}
