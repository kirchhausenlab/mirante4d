use std::{fmt, str::FromStr};

use mirante4d_identity::{
    ArtifactContentId, DerivationRecordId, ExactBytesDigest, PackageId, RecipeId, ReleaseId,
    ScientificContentId, is_nfc,
};

use super::ControlError;

/// Maximum encoded length of a version-1 ASCII token.
pub const MAX_ASCII_TOKEN_BYTES: usize = 128;
/// Maximum UTF-8 length of a version-1 NFC text scalar.
pub const MAX_NFC_TEXT_BYTES: usize = 4_096;

/// A canonical unsigned decimal string value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct U64Decimal(u64);

impl U64Decimal {
    pub fn parse(value: &str) -> Result<Self, ControlError> {
        value.parse()
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl FromStr for U64Decimal {
    type Err = ControlError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if !is_canonical_unsigned(value) {
            return invalid("u64 decimal", "expected 0|[1-9][0-9]*");
        }
        value
            .parse()
            .map(Self)
            .map_err(|_| invalid_error("u64 decimal", "the value exceeds u64"))
    }
}

impl fmt::Display for U64Decimal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A canonical signed decimal string value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct I64Decimal(i64);

impl I64Decimal {
    pub fn parse(value: &str) -> Result<Self, ControlError> {
        value.parse()
    }

    pub const fn get(self) -> i64 {
        self.0
    }
}

impl FromStr for I64Decimal {
    type Err = ControlError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let canonical = value == "0"
            || value
                .strip_prefix('-')
                .map_or_else(|| is_canonical_positive(value), is_canonical_positive);
        if !canonical {
            return invalid("i64 decimal", "expected 0|-?[1-9][0-9]*");
        }
        value
            .parse()
            .map(Self)
            .map_err(|_| invalid_error("i64 decimal", "the value exceeds i64"))
    }
}

impl fmt::Display for I64Decimal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A value matching the closed version-1 ASCII-token grammar.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AsciiToken(String);

impl AsciiToken {
    pub fn parse(value: &str) -> Result<Self, ControlError> {
        value.parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for AsciiToken {
    type Err = ControlError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() || value.len() > MAX_ASCII_TOKEN_BYTES {
            return invalid("ASCII token", "the byte length must be in 1..=128");
        }
        if !value.is_ascii() {
            return invalid("ASCII token", "only ASCII is permitted");
        }
        let mut bytes = value.bytes();
        if !bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            || !bytes.all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._:/+-".contains(&byte)
            })
        {
            return invalid("ASCII token", "expected [a-z0-9][a-z0-9._:/+-]{0,127}");
        }
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for AsciiToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A bounded Unicode-17 NFC text scalar.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NfcText(String);

impl NfcText {
    pub fn parse(value: &str) -> Result<Self, ControlError> {
        value.parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for NfcText {
    type Err = ControlError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() > MAX_NFC_TEXT_BYTES {
            return invalid("NFC text", "the value exceeds 4096 UTF-8 bytes");
        }
        if !is_nfc(value) {
            return invalid("NFC text", "the value is not Unicode 17 NFC");
        }
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for NfcText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A finite binary32 value carried as eight lowercase hexadecimal bit digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct F32Bits(u32);

impl F32Bits {
    pub fn parse(value: &str) -> Result<Self, ControlError> {
        let bits = parse_lower_hex(value, 8, "f32 bits")? as u32;
        if !f32::from_bits(bits).is_finite() {
            return invalid("f32 bits", "the represented value must be finite");
        }
        Ok(Self(bits))
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn value(self) -> f32 {
        f32::from_bits(self.0)
    }
}

impl fmt::Display for F32Bits {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:08x}", self.0)
    }
}

/// A finite binary64 value carried as sixteen lowercase hexadecimal bit digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct F64Bits(u64);

impl F64Bits {
    pub fn parse(value: &str) -> Result<Self, ControlError> {
        let bits = parse_lower_hex(value, 16, "f64 bits")?;
        if !f64::from_bits(bits).is_finite() {
            return invalid("f64 bits", "the represented value must be finite");
        }
        Ok(Self(bits))
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn value(self) -> f64 {
        f64::from_bits(self.0)
    }

    /// Canonicalizes either IEEE signed zero to positive zero.
    pub const fn normalized_zero(self) -> Self {
        if self.0 == (-0.0_f64).to_bits() {
            Self(0)
        } else {
            self
        }
    }
}

impl fmt::Display for F64Bits {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:016x}", self.0)
    }
}

/// A six-digit lowercase hexadecimal RGB color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb24([u8; 3]);

impl Rgb24 {
    pub fn parse(value: &str) -> Result<Self, ControlError> {
        let bits = parse_lower_hex(value, 6, "RGB color")? as u32;
        Ok(Self([
            ((bits >> 16) & 0xff) as u8,
            ((bits >> 8) & 0xff) as u8,
            (bits & 0xff) as u8,
        ]))
    }

    pub const fn channels(self) -> [u8; 3] {
        self.0
    }
}

impl fmt::Display for Rgb24 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:02x}{:02x}{:02x}",
            self.0[0], self.0[1], self.0[2]
        )
    }
}

/// The closed set of D-009 typed identities admitted in control metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TypedId {
    Scientific(ScientificContentId),
    ExactBytes(ExactBytesDigest),
    Package(PackageId),
    Recipe(RecipeId),
    DerivationRecord(DerivationRecordId),
    Release(ReleaseId),
    Artifact(ArtifactContentId),
}

impl TypedId {
    pub fn parse(value: &str) -> Result<Self, ControlError> {
        value.parse()
    }
}

impl FromStr for TypedId {
    type Err = ControlError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        macro_rules! parse_variant {
            ($type:ty, $variant:ident) => {
                if value.starts_with(<$type>::PREFIX) {
                    return <$type>::parse(value).map(Self::$variant).map_err(|_| {
                        invalid_error("typed identity", "the digest form is invalid")
                    });
                }
            };
        }

        parse_variant!(ScientificContentId, Scientific);
        parse_variant!(ExactBytesDigest, ExactBytes);
        parse_variant!(PackageId, Package);
        parse_variant!(RecipeId, Recipe);
        parse_variant!(DerivationRecordId, DerivationRecord);
        parse_variant!(ReleaseId, Release);
        parse_variant!(ArtifactContentId, Artifact);
        invalid(
            "typed identity",
            "the value does not use an admitted D-009 prefix",
        )
    }
}

impl fmt::Display for TypedId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scientific(value) => value.fmt(formatter),
            Self::ExactBytes(value) => value.fmt(formatter),
            Self::Package(value) => value.fmt(formatter),
            Self::Recipe(value) => value.fmt(formatter),
            Self::DerivationRecord(value) => value.fmt(formatter),
            Self::Release(value) => value.fmt(formatter),
            Self::Artifact(value) => value.fmt(formatter),
        }
    }
}

fn is_canonical_unsigned(value: &str) -> bool {
    value == "0" || is_canonical_positive(value)
}

fn is_canonical_positive(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes
        .first()
        .is_some_and(|byte| matches!(byte, b'1'..=b'9'))
        && bytes[1..].iter().all(u8::is_ascii_digit)
}

fn parse_lower_hex(value: &str, width: usize, kind: &'static str) -> Result<u64, ControlError> {
    if value.len() != width {
        return invalid(kind, "the hexadecimal width is invalid");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return invalid(kind, "expected lowercase hexadecimal characters");
    }
    u64::from_str_radix(value, 16).map_err(|_| invalid_error(kind, "the value is invalid"))
}

fn invalid<T>(kind: &'static str, reason: &'static str) -> Result<T, ControlError> {
    Err(invalid_error(kind, reason))
}

const fn invalid_error(kind: &'static str, reason: &'static str) -> ControlError {
    ControlError::InvalidScalar { kind, reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_decimals_reject_alternate_and_out_of_range_forms() {
        for (wire, value) in [("0", 0), ("1", 1), ("18446744073709551615", u64::MAX)] {
            let parsed = U64Decimal::parse(wire).unwrap();
            assert_eq!(parsed.get(), value);
            assert_eq!(parsed.to_string(), wire);
        }
        for wire in ["", "00", "01", "+1", "-0", "18446744073709551616"] {
            assert!(U64Decimal::parse(wire).is_err(), "accepted {wire:?}");
        }

        for (wire, value) in [
            ("0", 0),
            ("-1", -1),
            ("9223372036854775807", i64::MAX),
            ("-9223372036854775808", i64::MIN),
        ] {
            let parsed = I64Decimal::parse(wire).unwrap();
            assert_eq!(parsed.get(), value);
            assert_eq!(parsed.to_string(), wire);
        }
        for wire in ["", "00", "-0", "+1", "01", "9223372036854775808"] {
            assert!(I64Decimal::parse(wire).is_err(), "accepted {wire:?}");
        }
    }

    #[test]
    fn tokens_text_float_bits_and_rgb_use_the_frozen_wire_grammars() {
        let token = AsciiToken::parse("m4d.schema/v1+test").unwrap();
        assert_eq!(token.as_str(), "m4d.schema/v1+test");
        for value in ["", "Upper", "é", "-starts-wrong"] {
            assert!(AsciiToken::parse(value).is_err(), "accepted {value:?}");
        }
        assert!(AsciiToken::parse(&"a".repeat(MAX_ASCII_TOKEN_BYTES + 1)).is_err());

        assert_eq!(NfcText::parse("café").unwrap().as_str(), "café");
        assert!(NfcText::parse("cafe\u{301}").is_err());
        assert!(NfcText::parse(&"é".repeat(2_049)).is_err());

        let negative_zero = F32Bits::parse("80000000").unwrap();
        assert_eq!(negative_zero.bits(), 0x8000_0000);
        assert!(negative_zero.value().is_sign_negative());
        assert_eq!(negative_zero.to_string(), "80000000");
        assert!(F32Bits::parse("7f800000").is_err());
        assert!(F32Bits::parse("3F800000").is_err());

        let subnormal = F64Bits::parse("0000000000000001").unwrap();
        assert_eq!(subnormal.bits(), 1);
        assert!(subnormal.value().is_subnormal());
        assert!(F64Bits::parse("7ff0000000000000").is_err());
        assert_eq!(
            F64Bits::parse("8000000000000000")
                .unwrap()
                .normalized_zero()
                .bits(),
            0
        );

        let color = Rgb24::parse("00ff7a").unwrap();
        assert_eq!(color.channels(), [0, 255, 122]);
        assert_eq!(color.to_string(), "00ff7a");
        assert!(Rgb24::parse("00FF7A").is_err());
        assert!(Rgb24::parse("00000").is_err());
    }

    #[test]
    fn typed_ids_roundtrip_every_frozen_prefix() {
        let zeros = "0".repeat(64);
        for prefix in [
            ScientificContentId::PREFIX,
            ExactBytesDigest::PREFIX,
            PackageId::PREFIX,
            RecipeId::PREFIX,
            DerivationRecordId::PREFIX,
            ReleaseId::PREFIX,
            ArtifactContentId::PREFIX,
        ] {
            let wire = format!("{prefix}{zeros}");
            assert_eq!(TypedId::parse(&wire).unwrap().to_string(), wire);
        }
        assert!(TypedId::parse(&zeros).is_err());
        assert!(TypedId::parse(&format!("sha256:{}", "A".repeat(64))).is_err());
        assert!(TypedId::parse(&format!("m4d-unknown-v1-sha256:{zeros}")).is_err());
    }
}
