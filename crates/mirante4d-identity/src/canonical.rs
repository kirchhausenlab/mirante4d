use unicode_normalization::{
    UNICODE_VERSION as DEPENDENCY_UNICODE_VERSION, UnicodeNormalization,
    is_nfc as dependency_is_nfc,
};

use crate::Sha256Hasher;

/// The Unicode data version frozen by the version-1 identity contract.
pub const M4D_UNICODE_VERSION: (u8, u8, u8) = (17, 0, 0);

const _: () = {
    assert!(DEPENDENCY_UNICODE_VERSION.0 == M4D_UNICODE_VERSION.0);
    assert!(DEPENDENCY_UNICODE_VERSION.1 == M4D_UNICODE_VERSION.1);
    assert!(DEPENDENCY_UNICODE_VERSION.2 == M4D_UNICODE_VERSION.2);
};

/// Normalizes valid UTF-8 text to NFC using the frozen Unicode tables.
pub fn normalize_nfc(value: &str) -> String {
    value.nfc().collect()
}

/// Reports whether text is already NFC under the frozen Unicode tables.
pub fn is_nfc(value: &str) -> bool {
    dependency_is_nfc(value)
}

pub(crate) fn update_u8(hasher: &mut Sha256Hasher, value: u8) {
    hasher.update([value]);
}

pub(crate) fn update_u32(hasher: &mut Sha256Hasher, value: u32) {
    hasher.update(value.to_be_bytes());
}

pub(crate) fn update_u64(hasher: &mut Sha256Hasher, value: u64) {
    hasher.update(value.to_be_bytes());
}

pub(crate) fn f64_hex(value: f64) -> Option<[u8; 16]> {
    if !value.is_finite() {
        return None;
    }
    let canonical = if value == 0.0 { 0.0 } else { value };
    let bits = canonical.to_bits();
    let mut encoded = [0_u8; 16];
    for (index, byte) in encoded.iter_mut().enumerate() {
        let shift = (15 - index) * 4;
        let nibble = ((bits >> shift) & 0x0f) as u8;
        *byte = match nibble {
            0..=9 => b'0' + nibble,
            10..=15 => b'a' + nibble - 10,
            _ => unreachable!("a masked nibble is in range"),
        };
    }
    Some(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_tables_match_the_frozen_unicode_version() {
        assert_eq!(DEPENDENCY_UNICODE_VERSION, M4D_UNICODE_VERSION);
    }

    #[test]
    fn nfc_normalization_is_canonical_and_idempotent() {
        let decomposed = "A\u{30a}";
        let composed = "Å";
        assert_eq!(normalize_nfc(decomposed), composed);
        assert!(is_nfc(composed));
        assert!(!is_nfc(decomposed));
        assert_eq!(normalize_nfc(&normalize_nfc(decomposed)), composed);
    }

    #[test]
    fn f64_hex_uses_numeric_bit_order_and_normalizes_negative_zero() {
        assert_eq!(f64_hex(1.0).unwrap(), *b"3ff0000000000000");
        assert_eq!(f64_hex(-0.0).unwrap(), *b"0000000000000000");
        assert!(f64_hex(f64::NAN).is_none());
    }
}
