mod jcs;
mod scalar;
mod value;

use thiserror::Error;

pub use scalar::{
    AsciiToken, F32Bits, F64Bits, I64Decimal, MAX_ASCII_TOKEN_BYTES, MAX_NFC_TEXT_BYTES, NfcText,
    Rgb24, TypedId, U64Decimal,
};
pub use value::{CanonicalMapEntry, CanonicalValue, CanonicalValueKind, MAX_CANONICAL_VALUE_BYTES};

/// A strict experimental-v1 control-wire validation failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ControlError {
    #[error("invalid {kind}: {reason}")]
    InvalidScalar {
        kind: &'static str,
        reason: &'static str,
    },
    #[error("the restricted control JSON grammar does not permit this number")]
    UnsupportedJsonNumber,
    #[error("control JSON nesting exceeds {maximum} containers")]
    NestingLimitExceeded { maximum: usize },
    #[error("{object} exceeds its {maximum}-byte canonical limit")]
    ControlObjectTooLarge {
        object: &'static str,
        maximum: usize,
    },
    #[error("malformed {object}: {detail}")]
    MalformedControlObject {
        object: &'static str,
        detail: String,
    },
    #[error("{object} is not in its exact canonical encoding")]
    NonCanonicalControlObject { object: &'static str },
    #[error("invalid {object}: {reason}")]
    InvalidControlObject {
        object: &'static str,
        reason: &'static str,
    },
}

/// Encodes the one compatibility tuple accepted by the experimental profile.
///
/// This is an exact hard-cut API, not a compatibility promise for other tuples.
pub fn profile_compatibility_bytes() -> Result<Vec<u8>, ControlError> {
    let profile = crate::profile::PROFILE;
    let value = serde_json::json!({
        "format_family": profile.format_family,
        "lifecycle": profile.lifecycle,
        "semantic_schema": profile.semantic_schema,
        "storage_profile": profile.storage_profile,
        "index_profile": profile.index_profile,
        "identity_profile": profile.identity_profile,
        "ome_metadata_version": profile.ome_metadata_version,
        "ome_release": profile.ome_release,
        "zarr_format": profile.zarr_format,
        "zarr_core": profile.zarr_core,
        "required_capabilities": crate::profile::CAPABILITIES,
        "unknown_major_or_required_capability": "reject",
        "compatibility_fallback": "forbidden",
    });
    jcs::encode(&value, "profile compatibility", 4_096)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_profile_compatibility_has_exact_canonical_bytes() {
        assert_eq!(
            profile_compatibility_bytes().unwrap(),
            r#"{"compatibility_fallback":"forbidden","format_family":"mirante4d","identity_profile":"m4d-id-1","index_profile":"m4d-packed-index-1.0","lifecycle":"EXPERIMENTAL","ome_metadata_version":"0.5","ome_release":"0.5.2","required_capabilities":["m4d.bit-validity.v1","m4d.identity.v1","m4d.packed-index.v1","m4d.strict-profile.v1","zarr.sharding-indexed.v1"],"semantic_schema":"m4d-science-1.0","storage_profile":"m4d-zarr3-local-1.0","unknown_major_or_required_capability":"reject","zarr_core":"3.0","zarr_format":3}"#.as_bytes()
        );
    }
}
