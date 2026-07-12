use serde::{Deserialize, Serialize};

use super::{
    AsciiToken, ControlError, F32Bits, F64Bits, I64Decimal, NfcText, TypedId, U64Decimal,
    jcs::{self, MAX_JCS_DEPTH},
};

pub const MAX_CANONICAL_VALUE_BYTES: usize = 1_048_576;
const OBJECT: &str = "canonical value";

/// The closed version-1 canonical-value variant set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanonicalValueKind {
    Bool,
    U64,
    I64,
    F32,
    F64,
    Ascii,
    Text,
    Id,
    List,
    Map,
}

/// One key/value entry in a canonical map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalMapEntry {
    key: AsciiToken,
    value: CanonicalValue,
}

impl CanonicalMapEntry {
    pub fn new(key: AsciiToken, value: CanonicalValue) -> Self {
        Self { key, value }
    }

    pub fn key(&self) -> &AsciiToken {
        &self.key
    }

    pub fn value(&self) -> &CanonicalValue {
        &self.value
    }
}

/// A validated value in the closed version-1 tagged control grammar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalValue(ValueKind);

#[derive(Clone, Debug, PartialEq, Eq)]
enum ValueKind {
    Bool(bool),
    U64(U64Decimal),
    I64(I64Decimal),
    F32(F32Bits),
    F64(F64Bits),
    Ascii(AsciiToken),
    Text(NfcText),
    Id(TypedId),
    List(Vec<CanonicalValue>),
    Map(Vec<CanonicalMapEntry>),
}

impl CanonicalValue {
    pub const fn from_bool(value: bool) -> Self {
        Self(ValueKind::Bool(value))
    }

    pub const fn from_u64(value: U64Decimal) -> Self {
        Self(ValueKind::U64(value))
    }

    pub const fn from_i64(value: I64Decimal) -> Self {
        Self(ValueKind::I64(value))
    }

    pub const fn from_f32(value: F32Bits) -> Self {
        Self(ValueKind::F32(value))
    }

    pub const fn from_f64(value: F64Bits) -> Self {
        Self(ValueKind::F64(value))
    }

    pub fn from_ascii(value: AsciiToken) -> Self {
        Self(ValueKind::Ascii(value))
    }

    pub fn from_text(value: NfcText) -> Self {
        Self(ValueKind::Text(value))
    }

    pub const fn from_id(value: TypedId) -> Self {
        Self(ValueKind::Id(value))
    }

    pub fn list(items: Vec<Self>) -> Result<Self, ControlError> {
        let value = Self(ValueKind::List(items));
        value.validate_shape(0)?;
        Ok(value)
    }

    pub fn map(entries: Vec<CanonicalMapEntry>) -> Result<Self, ControlError> {
        require_map_order(&entries)?;
        let value = Self(ValueKind::Map(entries));
        value.validate_shape(0)?;
        Ok(value)
    }

    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ControlError> {
        if bytes.len() > MAX_CANONICAL_VALUE_BYTES {
            return Err(ControlError::ControlObjectTooLarge {
                object: OBJECT,
                maximum: MAX_CANONICAL_VALUE_BYTES,
            });
        }
        let wire: WireValue = serde_json::from_slice(bytes).map_err(|error| {
            ControlError::MalformedControlObject {
                object: OBJECT,
                detail: error.to_string(),
            }
        })?;
        let value = Self::try_from(wire)?;
        if value.canonical_bytes()?.as_slice() != bytes {
            return Err(ControlError::NonCanonicalControlObject { object: OBJECT });
        }
        Ok(value)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ControlError> {
        self.validate_shape(0)?;
        let wire = WireValue::from(self);
        let value =
            serde_json::to_value(wire).map_err(|error| ControlError::MalformedControlObject {
                object: OBJECT,
                detail: error.to_string(),
            })?;
        jcs::encode(&value, OBJECT, MAX_CANONICAL_VALUE_BYTES)
    }

    pub const fn kind(&self) -> CanonicalValueKind {
        match self.0 {
            ValueKind::Bool(_) => CanonicalValueKind::Bool,
            ValueKind::U64(_) => CanonicalValueKind::U64,
            ValueKind::I64(_) => CanonicalValueKind::I64,
            ValueKind::F32(_) => CanonicalValueKind::F32,
            ValueKind::F64(_) => CanonicalValueKind::F64,
            ValueKind::Ascii(_) => CanonicalValueKind::Ascii,
            ValueKind::Text(_) => CanonicalValueKind::Text,
            ValueKind::Id(_) => CanonicalValueKind::Id,
            ValueKind::List(_) => CanonicalValueKind::List,
            ValueKind::Map(_) => CanonicalValueKind::Map,
        }
    }

    pub const fn as_bool(&self) -> Option<bool> {
        match self.0 {
            ValueKind::Bool(value) => Some(value),
            _ => None,
        }
    }

    pub const fn as_u64(&self) -> Option<U64Decimal> {
        match self.0 {
            ValueKind::U64(value) => Some(value),
            _ => None,
        }
    }

    pub const fn as_i64(&self) -> Option<I64Decimal> {
        match self.0 {
            ValueKind::I64(value) => Some(value),
            _ => None,
        }
    }

    pub const fn as_f32(&self) -> Option<F32Bits> {
        match self.0 {
            ValueKind::F32(value) => Some(value),
            _ => None,
        }
    }

    pub const fn as_f64(&self) -> Option<F64Bits> {
        match self.0 {
            ValueKind::F64(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_ascii(&self) -> Option<&AsciiToken> {
        match &self.0 {
            ValueKind::Ascii(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&NfcText> {
        match &self.0 {
            ValueKind::Text(value) => Some(value),
            _ => None,
        }
    }

    pub const fn as_id(&self) -> Option<TypedId> {
        match self.0 {
            ValueKind::Id(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Self]> {
        match &self.0 {
            ValueKind::List(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&[CanonicalMapEntry]> {
        match &self.0 {
            ValueKind::Map(entries) => Some(entries),
            _ => None,
        }
    }

    fn validate_shape(&self, depth: usize) -> Result<(), ControlError> {
        require_depth(depth)?;
        match &self.0 {
            ValueKind::List(items) => {
                require_depth(depth + 1)?;
                for item in items {
                    item.validate_shape(depth + 2)?;
                }
            }
            ValueKind::Map(entries) => {
                require_depth(depth + 1)?;
                require_map_order(entries)?;
                for entry in entries {
                    require_depth(depth + 2)?;
                    entry.value.validate_shape(depth + 3)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn require_depth(depth: usize) -> Result<(), ControlError> {
    if depth >= MAX_JCS_DEPTH {
        return Err(ControlError::NestingLimitExceeded {
            maximum: MAX_JCS_DEPTH,
        });
    }
    Ok(())
}

fn require_map_order(entries: &[CanonicalMapEntry]) -> Result<(), ControlError> {
    if !entries
        .windows(2)
        .all(|pair| pair[0].key.as_str() < pair[1].key.as_str())
    {
        return Err(ControlError::InvalidControlObject {
            object: OBJECT,
            reason: "map keys must be strictly sorted and unique",
        });
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum WireValue {
    #[serde(rename = "bool")]
    Bool { value: bool },
    #[serde(rename = "u64")]
    U64 { value: String },
    #[serde(rename = "i64")]
    I64 { value: String },
    #[serde(rename = "f32")]
    F32 { bits: String },
    #[serde(rename = "f64")]
    F64 { bits: String },
    #[serde(rename = "ascii")]
    Ascii { value: String },
    #[serde(rename = "text")]
    Text { value: String },
    #[serde(rename = "id")]
    Id { value: String },
    #[serde(rename = "list")]
    List { items: Vec<WireValue> },
    #[serde(rename = "map")]
    Map { entries: Vec<WireMapEntry> },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireMapEntry {
    key: String,
    value: WireValue,
}

impl TryFrom<WireValue> for CanonicalValue {
    type Error = ControlError;

    fn try_from(wire: WireValue) -> Result<Self, Self::Error> {
        match wire {
            WireValue::Bool { value } => Ok(Self::from_bool(value)),
            WireValue::U64 { value } => U64Decimal::parse(&value).map(Self::from_u64),
            WireValue::I64 { value } => I64Decimal::parse(&value).map(Self::from_i64),
            WireValue::F32 { bits } => F32Bits::parse(&bits).map(Self::from_f32),
            WireValue::F64 { bits } => F64Bits::parse(&bits).map(Self::from_f64),
            WireValue::Ascii { value } => AsciiToken::parse(&value).map(Self::from_ascii),
            WireValue::Text { value } => NfcText::parse(&value).map(Self::from_text),
            WireValue::Id { value } => TypedId::parse(&value).map(Self::from_id),
            WireValue::List { items } => items
                .into_iter()
                .map(Self::try_from)
                .collect::<Result<Vec<_>, _>>()
                .and_then(Self::list),
            WireValue::Map { entries } => entries
                .into_iter()
                .map(|entry| {
                    Ok(CanonicalMapEntry::new(
                        AsciiToken::parse(&entry.key)?,
                        Self::try_from(entry.value)?,
                    ))
                })
                .collect::<Result<Vec<_>, ControlError>>()
                .and_then(Self::map),
        }
    }
}

impl From<&CanonicalValue> for WireValue {
    fn from(value: &CanonicalValue) -> Self {
        match &value.0 {
            ValueKind::Bool(value) => Self::Bool { value: *value },
            ValueKind::U64(value) => Self::U64 {
                value: value.to_string(),
            },
            ValueKind::I64(value) => Self::I64 {
                value: value.to_string(),
            },
            ValueKind::F32(value) => Self::F32 {
                bits: value.to_string(),
            },
            ValueKind::F64(value) => Self::F64 {
                bits: value.to_string(),
            },
            ValueKind::Ascii(value) => Self::Ascii {
                value: value.to_string(),
            },
            ValueKind::Text(value) => Self::Text {
                value: value.to_string(),
            },
            ValueKind::Id(value) => Self::Id {
                value: value.to_string(),
            },
            ValueKind::List(items) => Self::List {
                items: items.iter().map(Self::from).collect(),
            },
            ValueKind::Map(entries) => Self::Map {
                entries: entries
                    .iter()
                    .map(|entry| WireMapEntry {
                        key: entry.key.to_string(),
                        value: Self::from(&entry.value),
                    })
                    .collect(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use mirante4d_identity::ScientificContentId;

    use super::*;

    #[test]
    fn every_canonical_value_variant_roundtrips_exact_bytes() {
        let id = TypedId::parse(&format!(
            "{}{}",
            ScientificContentId::PREFIX,
            "0".repeat(64)
        ))
        .unwrap();
        let nested = CanonicalValue::list(vec![CanonicalValue::from_bool(false)]).unwrap();
        let map = CanonicalValue::map(vec![CanonicalMapEntry::new(
            AsciiToken::parse("a").unwrap(),
            CanonicalValue::from_bool(true),
        )])
        .unwrap();
        let cases = [
            (
                CanonicalValue::from_bool(true),
                r#"{"type":"bool","value":true}"#,
            ),
            (
                CanonicalValue::from_u64(U64Decimal::parse("7").unwrap()),
                r#"{"type":"u64","value":"7"}"#,
            ),
            (
                CanonicalValue::from_i64(I64Decimal::parse("-7").unwrap()),
                r#"{"type":"i64","value":"-7"}"#,
            ),
            (
                CanonicalValue::from_f32(F32Bits::parse("3f800000").unwrap()),
                r#"{"bits":"3f800000","type":"f32"}"#,
            ),
            (
                CanonicalValue::from_f64(F64Bits::parse("3ff0000000000000").unwrap()),
                r#"{"bits":"3ff0000000000000","type":"f64"}"#,
            ),
            (
                CanonicalValue::from_ascii(AsciiToken::parse("token").unwrap()),
                r#"{"type":"ascii","value":"token"}"#,
            ),
            (
                CanonicalValue::from_text(NfcText::parse("café").unwrap()),
                r#"{"type":"text","value":"café"}"#,
            ),
            (
                CanonicalValue::from_id(id),
                r#"{"type":"id","value":"m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000"}"#,
            ),
            (
                nested,
                r#"{"items":[{"type":"bool","value":false}],"type":"list"}"#,
            ),
            (
                map,
                r#"{"entries":[{"key":"a","value":{"type":"bool","value":true}}],"type":"map"}"#,
            ),
        ];

        for (value, expected) in cases {
            let bytes = value.canonical_bytes().unwrap();
            assert_eq!(bytes, expected.as_bytes());
            assert_eq!(CanonicalValue::parse_canonical(&bytes).unwrap(), value);
        }
    }

    #[test]
    fn malformed_noncanonical_unordered_and_overdeep_values_reject() {
        for wire in [
            r#"{"type":"bool","value":true,"value":false}"#,
            r#"{"type":"bool","type":"bool","value":true}"#,
            r#"{"extra":false,"type":"bool","value":true}"#,
            r#"{"type":"bool"}"#,
            r#"{"type":"bool","bits":"00000000"}"#,
            r#"null"#,
            r#"1"#,
            r#"{ "type":"bool","value":true}"#,
            r#"{"type":"u64","value":"01"}"#,
            r#"{"items":[{"type":"bool","value":true,"value":false}],"type":"list"}"#,
            r#"{"entries":[{"key":"a","key":"b","value":{"type":"bool","value":true}}],"type":"map"}"#,
            r#"{"entries":[{"key":"a","value":{"type":"bool","value":true}},{"key":"a","value":{"type":"bool","value":false}}],"type":"map"}"#,
            r#"{"entries":[{"key":"b","value":{"type":"bool","value":true}},{"key":"a","value":{"type":"bool","value":false}}],"type":"map"}"#,
        ] {
            assert!(
                CanonicalValue::parse_canonical(wire.as_bytes()).is_err(),
                "accepted {wire}"
            );
        }

        let mut nested = CanonicalValue::from_bool(true);
        for _ in 0..31 {
            nested = CanonicalValue::list(vec![nested]).unwrap();
        }
        assert!(CanonicalValue::list(vec![nested]).is_err());
        assert!(
            CanonicalValue::parse_canonical(&vec![b' '; MAX_CANONICAL_VALUE_BYTES + 1]).is_err()
        );
    }
}
