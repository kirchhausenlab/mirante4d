use crate::{ArtifactContentId, Sha256Hasher};

const ARTIFACT_DOMAIN: &[u8] = b"M4D-ARTIFACT-V1\0";

/// The canonical JCS body of the non-admissible WP-10A specification vector.
///
/// This role is never accepted as a production artifact role.
pub const WP10A_ARTIFACT_HAND_VECTOR_BODY: &str =
    r#"{"role":"spec.vector-only","schema":"m4d-artifact-vector-1"}"#;

/// The typed result frozen for the non-admissible WP-10A specification vector.
pub const WP10A_ARTIFACT_HAND_VECTOR_ID: &str = concat!(
    "m4d-artifact-v1-sha256:",
    "2dba2606763e80b9e5d3f60ebd00c818e46496329dd46fd3b79043a8d0e6c66f"
);

/// Verifies the frozen, non-admissible artifact framing vector.
///
/// Version 1 deliberately exposes no generic artifact-content constructor and
/// admits no production roles. WP-12 must freeze each role's closed body before
/// adding a production computation API.
pub fn verify_wp10a_artifact_hand_vector() -> bool {
    let body = WP10A_ARTIFACT_HAND_VECTOR_BODY.as_bytes();
    let mut hasher = Sha256Hasher::new();
    hasher.update(ARTIFACT_DOMAIN);
    let body_len = u64::try_from(body.len()).expect("the frozen vector length fits u64");
    hasher.update(body_len.to_be_bytes());
    hasher.update(body);
    let actual = ArtifactContentId::from_digest(hasher.finalize());
    ArtifactContentId::parse(WP10A_ARTIFACT_HAND_VECTOR_ID).is_ok_and(|expected| actual == expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_non_admissible_artifact_vector_matches_exactly() {
        assert_eq!(WP10A_ARTIFACT_HAND_VECTOR_BODY.len(), 60);
        assert!(verify_wp10a_artifact_hand_vector());
    }
}
