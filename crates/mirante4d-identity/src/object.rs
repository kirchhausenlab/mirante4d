use std::{fmt, str::FromStr};

use crate::{ExactBytesDigest, IdentityError};

pub const MAX_OBJECT_PATH_BYTES: usize = 4096;
pub const MAX_MEDIA_TYPE_BYTES: usize = 255;
pub const MAX_OBJECT_ROLE_BYTES: usize = 128;

/// A normalized relative package-object path value.
///
/// This is deliberately not a filesystem path API. It accepts only nonempty
/// lowercase-ASCII segments separated by `/`, with no absolute, empty, `.` or
/// `..` segment.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectPath(String);

impl ObjectPath {
    pub fn parse(value: &str) -> Result<Self, IdentityError> {
        value.parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ObjectPath {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        validate_object_path(value)?;
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for ObjectPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A strict lowercase-ASCII media type without parameters.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MediaType(String);

impl MediaType {
    pub fn parse(value: &str) -> Result<Self, IdentityError> {
        value.parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for MediaType {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        validate_media_type(value)?;
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for MediaType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A strict lowercase-ASCII logical object-role identifier.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectRole(String);

impl ObjectRole {
    pub fn parse(value: &str) -> Result<Self, IdentityError> {
        value.parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ObjectRole {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        validate_object_role(value)?;
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for ObjectRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The exact-byte facts for one typed object, independent of its package path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawObjectDescriptor {
    digest: ExactBytesDigest,
    byte_length: u64,
    media_type: MediaType,
    role: ObjectRole,
}

impl RawObjectDescriptor {
    /// Constructs a descriptor from values already validated by their types.
    /// Zero-length objects are valid.
    pub fn new(
        digest: ExactBytesDigest,
        byte_length: u64,
        media_type: MediaType,
        role: ObjectRole,
    ) -> Self {
        Self {
            digest,
            byte_length,
            media_type,
            role,
        }
    }

    pub const fn digest(&self) -> ExactBytesDigest {
        self.digest
    }

    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }

    pub fn media_type(&self) -> &MediaType {
        &self.media_type
    }

    pub fn role(&self) -> &ObjectRole {
        &self.role
    }
}

/// A raw object descriptor bound to one normalized package path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageObjectDescriptor {
    path: ObjectPath,
    object: RawObjectDescriptor,
}

impl PackageObjectDescriptor {
    pub fn new(path: ObjectPath, object: RawObjectDescriptor) -> Self {
        Self { path, object }
    }

    pub fn path(&self) -> &ObjectPath {
        &self.path
    }

    pub fn object(&self) -> &RawObjectDescriptor {
        &self.object
    }
}

fn validate_object_path(value: &str) -> Result<(), IdentityError> {
    if value.is_empty() {
        return Err(IdentityError::InvalidObjectPath {
            reason: "the value is empty",
        });
    }
    if value.len() > MAX_OBJECT_PATH_BYTES {
        return Err(IdentityError::InvalidObjectPath {
            reason: "the value exceeds 4096 bytes",
        });
    }
    if !value.is_ascii() {
        return Err(IdentityError::InvalidObjectPath {
            reason: "only ASCII is permitted",
        });
    }
    if value.starts_with('/') {
        return Err(IdentityError::InvalidObjectPath {
            reason: "absolute paths are forbidden",
        });
    }

    for segment in value.split('/') {
        if segment.is_empty() {
            return Err(IdentityError::InvalidObjectPath {
                reason: "empty path segments are forbidden",
            });
        }
        if matches!(segment, "." | "..") {
            return Err(IdentityError::InvalidObjectPath {
                reason: "dot path segments are forbidden",
            });
        }
        if !segment.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        }) {
            return Err(IdentityError::InvalidObjectPath {
                reason: "segments may contain only lowercase letters, digits, '.', '_' and '-'",
            });
        }
    }
    Ok(())
}

fn validate_media_type(value: &str) -> Result<(), IdentityError> {
    if value.len() > MAX_MEDIA_TYPE_BYTES {
        return Err(IdentityError::InvalidMediaType {
            reason: "the value exceeds 255 bytes",
        });
    }
    if !value.is_ascii() {
        return Err(IdentityError::InvalidMediaType {
            reason: "only ASCII is permitted",
        });
    }
    let Some((top_level, subtype)) = value.split_once('/') else {
        return Err(IdentityError::InvalidMediaType {
            reason: "exactly one type/subtype separator is required",
        });
    };
    if subtype.contains('/') || !valid_media_token(top_level) || !valid_media_token(subtype) {
        return Err(IdentityError::InvalidMediaType {
            reason: "type and subtype must be nonempty lowercase ASCII tokens",
        });
    }
    Ok(())
}

fn valid_media_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"!#$&^_.+-".contains(&byte)
        })
}

fn validate_object_role(value: &str) -> Result<(), IdentityError> {
    if value.is_empty() {
        return Err(IdentityError::InvalidObjectRole {
            reason: "the value is empty",
        });
    }
    if value.len() > MAX_OBJECT_ROLE_BYTES {
        return Err(IdentityError::InvalidObjectRole {
            reason: "the value exceeds 128 bytes",
        });
    }
    if !value.is_ascii() {
        return Err(IdentityError::InvalidObjectRole {
            reason: "only ASCII is permitted",
        });
    }
    let bytes = value.as_bytes();
    if !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(byte))
    {
        return Err(IdentityError::InvalidObjectRole {
            reason: "roles must start and end with a lowercase letter or digit and contain only '.', '_' and '-' separators",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use proptest::test_runner::RngSeed;

    use super::*;

    const ZERO_EXACT_DIGEST: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    #[test]
    fn object_path_accepts_only_normalized_relative_lowercase_ascii() {
        let path = ObjectPath::parse("layers/0/s0/c/0001-00.zarr-shard").unwrap();
        assert_eq!(path.as_str(), "layers/0/s0/c/0001-00.zarr-shard");
        assert_eq!(path.to_string(), path.as_str());

        for invalid in [
            "",
            "/layers/0",
            "layers//0",
            "layers/./0",
            "layers/../0",
            "layers/0/",
            "Layers/0",
            "layers\\0",
            "layers/space here",
            "layers/café",
        ] {
            assert!(ObjectPath::parse(invalid).is_err(), "accepted {invalid:?}");
        }
        assert!(ObjectPath::parse(&"x".repeat(MAX_OBJECT_PATH_BYTES + 1)).is_err());
        assert!(ObjectPath::parse(&"µ".repeat(MAX_OBJECT_PATH_BYTES)).is_err());
    }

    #[test]
    fn media_type_and_role_enforce_strict_ascii_grammars() {
        assert_eq!(
            MediaType::parse("application/vnd.mirante4d.zarr-shard")
                .unwrap()
                .as_str(),
            "application/vnd.mirante4d.zarr-shard"
        );
        for invalid in [
            "",
            "application",
            "application/",
            "/json",
            "Application/json",
            "application/json; charset=utf-8",
            "application/json/extra",
            "application/café",
        ] {
            assert!(MediaType::parse(invalid).is_err(), "accepted {invalid:?}");
        }

        assert_eq!(
            ObjectRole::parse("zarr.shard").unwrap().as_str(),
            "zarr.shard"
        );
        for invalid in [
            "",
            "Zarr.shard",
            ".zarr-shard",
            "zarr-shard.",
            "zarr/shard",
            "zarr shard",
            "café",
        ] {
            assert!(ObjectRole::parse(invalid).is_err(), "accepted {invalid:?}");
        }
        let oversized_media_type = format!("a/{}", "x".repeat(MAX_MEDIA_TYPE_BYTES));
        assert!(MediaType::parse(&oversized_media_type).is_err());
        assert!(ObjectRole::parse(&"x".repeat(MAX_OBJECT_ROLE_BYTES + 1)).is_err());
        assert!(ObjectRole::parse(&"µ".repeat(MAX_OBJECT_ROLE_BYTES)).is_err());
    }

    #[test]
    fn descriptors_keep_raw_object_facts_separate_from_package_path() {
        let digest = ExactBytesDigest::parse(ZERO_EXACT_DIGEST).unwrap();
        let raw = RawObjectDescriptor::new(
            digest,
            0,
            MediaType::parse("application/json").unwrap(),
            ObjectRole::parse("zarr.metadata").unwrap(),
        );

        assert_eq!(raw.digest(), digest);
        assert_eq!(raw.byte_length(), 0);
        assert_eq!(raw.media_type().as_str(), "application/json");
        assert_eq!(raw.role().as_str(), "zarr.metadata");

        let package = PackageObjectDescriptor::new(
            ObjectPath::parse("layers/0/zarr.json").unwrap(),
            raw.clone(),
        );
        assert_eq!(package.path().as_str(), "layers/0/zarr.json");
        assert_eq!(package.object(), &raw);
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 128,
            max_shrink_iters: 1_024,
            failure_persistence: None,
            rng_seed: RngSeed::Fixed(0x4d34_4f42_4a45_4354),
            .. ProptestConfig::default()
        })]

        #[test]
        fn normalized_object_paths_roundtrip(
            segment in "[a-z][a-z0-9_-]{0,15}",
            leaf in "[a-z][a-z0-9_-]{0,15}",
        ) {
            let encoded = format!("objects/{segment}/{leaf}.bin");
            let path = ObjectPath::parse(&encoded).unwrap();
            prop_assert_eq!(path.as_str(), encoded.as_str());
            prop_assert_eq!(ObjectPath::parse(&path.to_string()).unwrap(), path);
        }
    }
}
