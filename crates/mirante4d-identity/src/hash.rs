use sha2::{Digest, Sha256};

use crate::Sha256Digest;

/// An incremental SHA-256 computation over caller-supplied bytes.
///
/// The hasher performs no I/O and has bounded internal state. Typed identity
/// constructors remain responsible for applying their exact domain and field
/// framing before finalization.
#[derive(Clone, Debug)]
pub struct Sha256Hasher(Sha256);

impl Sha256Hasher {
    pub fn new() -> Self {
        Self(Sha256::new())
    }

    pub fn update(&mut self, bytes: impl AsRef<[u8]>) {
        self.0.update(bytes.as_ref());
    }

    pub fn finalize(self) -> Sha256Digest {
        let bytes: [u8; 32] = self.0.finalize().into();
        Sha256Digest::from_bytes(bytes)
    }

    pub fn digest(bytes: impl AsRef<[u8]>) -> Sha256Digest {
        let mut hasher = Self::new();
        hasher.update(bytes);
        hasher.finalize()
    }
}

impl Default for Sha256Hasher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_and_one_shot_sha256_match_the_published_empty_vector() {
        let expected = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(Sha256Hasher::digest([]).to_string(), expected);

        let mut incremental = Sha256Hasher::new();
        incremental.update([]);
        assert_eq!(incremental.finalize().to_string(), expected);
    }

    #[test]
    fn chunking_does_not_change_the_digest() {
        let whole = Sha256Hasher::digest(b"mirante4d");
        let mut split = Sha256Hasher::new();
        split.update(b"mira");
        split.update(b"nte4d");
        assert_eq!(split.finalize(), whole);
    }
}
