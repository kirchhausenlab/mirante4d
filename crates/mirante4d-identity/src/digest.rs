use std::{fmt, str::FromStr};

use crate::IdentityError;

const SHA256_BYTE_LENGTH: usize = 32;
const SHA256_HEX_LENGTH: usize = SHA256_BYTE_LENGTH * 2;

/// An already-computed SHA-256 digest.
///
/// Parsing is intentionally strict: only exactly 64 lowercase hexadecimal
/// characters are accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sha256Digest([u8; SHA256_BYTE_LENGTH]);

impl Sha256Digest {
    pub fn parse(value: &str) -> Result<Self, IdentityError> {
        value.parse()
    }

    pub const fn as_bytes(&self) -> &[u8; SHA256_BYTE_LENGTH] {
        &self.0
    }
}

impl FromStr for Sha256Digest {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = value.as_bytes();
        if bytes.len() != SHA256_HEX_LENGTH {
            return Err(IdentityError::InvalidSha256Length {
                actual: bytes.len(),
            });
        }

        let mut digest = [0_u8; SHA256_BYTE_LENGTH];
        for (index, pair) in bytes.chunks_exact(2).enumerate() {
            let high = lowercase_hex_value(pair[0], index * 2)?;
            let low = lowercase_hex_value(pair[1], index * 2 + 1)?;
            digest[index] = (high << 4) | low;
        }
        Ok(Self(digest))
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

fn lowercase_hex_value(byte: u8, index: usize) -> Result<u8, IdentityError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(IdentityError::InvalidSha256Character { index, byte }),
    }
}

fn parse_prefixed_digest(
    value: &str,
    expected_prefix: &'static str,
) -> Result<Sha256Digest, IdentityError> {
    let digest =
        value
            .strip_prefix(expected_prefix)
            .ok_or(IdentityError::InvalidIdentityPrefix {
                expected: expected_prefix,
            })?;
    digest.parse()
}

macro_rules! typed_digest_id {
    ($name:ident, $prefix:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Sha256Digest);

        impl $name {
            pub const PREFIX: &'static str = $prefix;

            pub fn parse(value: &str) -> Result<Self, IdentityError> {
                value.parse()
            }

            pub const fn digest(&self) -> Sha256Digest {
                self.0
            }
        }

        impl FromStr for $name {
            type Err = IdentityError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                parse_prefixed_digest(value, Self::PREFIX).map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}{}", Self::PREFIX, self.0)
            }
        }
    };
}

typed_digest_id!(
    ScientificContentId,
    "m4d-sc-v1-sha256:",
    "A version-1 Mirante4D scientific-content identity."
);
typed_digest_id!(
    ExactBytesDigest,
    "sha256:",
    "The SHA-256 digest of the exact bytes of one typed object."
);
typed_digest_id!(
    PackageId,
    "m4d-package-v1-sha256:",
    "A version-1 Mirante4D exact package-payload identity."
);
typed_digest_id!(
    RecipeId,
    "m4d-recipe-v1-sha256:",
    "A version-1 Mirante4D typed recipe identity."
);
typed_digest_id!(
    DerivationRecordId,
    "m4d-derivation-record-v1-sha256:",
    "A version-1 Mirante4D exact derivation-record identity."
);
typed_digest_id!(
    ReleaseId,
    "m4d-release-v1-sha256:",
    "A version-1 Mirante4D immutable dataset-release identity."
);
typed_digest_id!(
    ArtifactContentId,
    "m4d-artifact-v1-sha256:",
    "A version-1 Mirante4D scientific-artifact-content identity."
);

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use proptest::test_runner::RngSeed;

    use super::*;

    const ZERO_HEX: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    #[test]
    fn typed_identities_parse_and_display_their_exact_prefixes() {
        macro_rules! assert_roundtrip {
            ($type:ty, $prefix:literal) => {{
                let encoded = format!("{}{}", $prefix, ZERO_HEX);
                let parsed = <$type>::parse(&encoded).unwrap();
                assert_eq!(parsed.to_string(), encoded);
                assert_eq!(parsed.digest().to_string(), ZERO_HEX);
            }};
        }

        assert_roundtrip!(ScientificContentId, "m4d-sc-v1-sha256:");
        assert_roundtrip!(ExactBytesDigest, "sha256:");
        assert_roundtrip!(PackageId, "m4d-package-v1-sha256:");
        assert_roundtrip!(RecipeId, "m4d-recipe-v1-sha256:");
        assert_roundtrip!(DerivationRecordId, "m4d-derivation-record-v1-sha256:");
        assert_roundtrip!(ReleaseId, "m4d-release-v1-sha256:");
        assert_roundtrip!(ArtifactContentId, "m4d-artifact-v1-sha256:");
    }

    #[test]
    fn strict_digest_rejects_uppercase_wrong_length_and_non_hexadecimal_bytes() {
        let uppercase = "A".repeat(SHA256_HEX_LENGTH);
        assert!(matches!(
            Sha256Digest::parse(&uppercase),
            Err(IdentityError::InvalidSha256Character { index: 0, .. })
        ));
        assert_eq!(
            Sha256Digest::parse(&"0".repeat(SHA256_HEX_LENGTH - 1)),
            Err(IdentityError::InvalidSha256Length {
                actual: SHA256_HEX_LENGTH - 1
            })
        );
        assert_eq!(
            Sha256Digest::parse(&"0".repeat(SHA256_HEX_LENGTH + 1)),
            Err(IdentityError::InvalidSha256Length {
                actual: SHA256_HEX_LENGTH + 1
            })
        );
        let invalid = format!("{}g", "0".repeat(SHA256_HEX_LENGTH - 1));
        assert!(matches!(
            Sha256Digest::parse(&invalid),
            Err(IdentityError::InvalidSha256Character { index: 63, .. })
        ));
    }

    #[test]
    fn typed_parsers_reject_wrong_or_bare_prefixes() {
        let package = format!("{}{}", PackageId::PREFIX, ZERO_HEX);
        assert_eq!(
            ScientificContentId::parse(&package),
            Err(IdentityError::InvalidIdentityPrefix {
                expected: ScientificContentId::PREFIX
            })
        );
        assert_eq!(
            ExactBytesDigest::parse(ZERO_HEX),
            Err(IdentityError::InvalidIdentityPrefix {
                expected: ExactBytesDigest::PREFIX
            })
        );
    }

    #[test]
    fn typed_identifiers_with_the_same_digest_remain_distinct() {
        let scientific =
            ScientificContentId::parse(&format!("{}{}", ScientificContentId::PREFIX, ZERO_HEX))
                .unwrap();
        let package = PackageId::parse(&format!("{}{}", PackageId::PREFIX, ZERO_HEX)).unwrap();

        assert_eq!(scientific.digest(), package.digest());
        assert_ne!(scientific.to_string(), package.to_string());
        assert!(ScientificContentId::parse(&package.to_string()).is_err());
        assert!(PackageId::parse(&scientific.to_string()).is_err());
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 128,
            max_shrink_iters: 1_024,
            failure_persistence: None,
            rng_seed: RngSeed::Fixed(0x4d34_4944_4449_4731),
            .. ProptestConfig::default()
        })]

        #[test]
        fn sha256_and_all_typed_forms_roundtrip(bytes in any::<[u8; SHA256_BYTE_LENGTH]>()) {
            let digest = Sha256Digest(bytes);
            let hex = digest.to_string();

            prop_assert_eq!(Sha256Digest::parse(&hex).unwrap(), digest);
            prop_assert_eq!(
                ScientificContentId::parse(&format!("{}{}", ScientificContentId::PREFIX, hex)).unwrap().digest(),
                digest
            );
            prop_assert_eq!(
                ExactBytesDigest::parse(&format!("{}{}", ExactBytesDigest::PREFIX, hex)).unwrap().digest(),
                digest
            );
            prop_assert_eq!(
                PackageId::parse(&format!("{}{}", PackageId::PREFIX, hex)).unwrap().digest(),
                digest
            );
            prop_assert_eq!(
                RecipeId::parse(&format!("{}{}", RecipeId::PREFIX, hex)).unwrap().digest(),
                digest
            );
            prop_assert_eq!(
                DerivationRecordId::parse(&format!("{}{}", DerivationRecordId::PREFIX, hex)).unwrap().digest(),
                digest
            );
            prop_assert_eq!(
                ReleaseId::parse(&format!("{}{}", ReleaseId::PREFIX, hex)).unwrap().digest(),
                digest
            );
            prop_assert_eq!(
                ArtifactContentId::parse(&format!("{}{}", ArtifactContentId::PREFIX, hex)).unwrap().digest(),
                digest
            );
        }
    }
}
