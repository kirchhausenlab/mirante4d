use std::cmp::Ordering;

use serde_json::Value;

use crate::control::ControlError;

pub(crate) const MAX_JCS_DEPTH: usize = 64;

/// Encodes the restricted JCS primitive set used by version-1 control DTOs.
/// The only JSON numbers are schema version `1` and Zarr format `3`.
pub(crate) fn encode(
    value: &Value,
    object: &'static str,
    maximum: usize,
) -> Result<Vec<u8>, ControlError> {
    let mut encoder = Encoder {
        output: Vec::with_capacity(maximum.min(4_096)),
        object,
        maximum,
    };
    encoder.write_value(value, 0)?;
    Ok(encoder.output)
}

struct Encoder {
    output: Vec<u8>,
    object: &'static str,
    maximum: usize,
}

impl Encoder {
    fn write_value(&mut self, value: &Value, depth: usize) -> Result<(), ControlError> {
        match value {
            Value::Null => self.push_bytes(b"null"),
            Value::Bool(false) => self.push_bytes(b"false"),
            Value::Bool(true) => self.push_bytes(b"true"),
            Value::Number(number) if number.as_u64() == Some(1) => self.push_bytes(b"1"),
            Value::Number(number) if number.as_u64() == Some(3) => self.push_bytes(b"3"),
            Value::Number(_) => Err(ControlError::UnsupportedJsonNumber),
            Value::String(value) => self.write_string(value),
            Value::Array(values) => {
                self.require_container_depth(depth)?;
                self.push_byte(b'[')?;
                for (index, value) in values.iter().enumerate() {
                    if index != 0 {
                        self.push_byte(b',')?;
                    }
                    self.write_value(value, depth + 1)?;
                }
                self.push_byte(b']')
            }
            Value::Object(entries) => {
                self.require_container_depth(depth)?;
                self.push_byte(b'{')?;

                let mut entries: Vec<_> = entries.iter().collect();
                entries.sort_unstable_by(|(left, _), (right, _)| utf16_cmp(left, right));
                for (index, (key, value)) in entries.into_iter().enumerate() {
                    if index != 0 {
                        self.push_byte(b',')?;
                    }
                    self.write_string(key)?;
                    self.push_byte(b':')?;
                    self.write_value(value, depth + 1)?;
                }

                self.push_byte(b'}')
            }
        }
    }

    fn require_container_depth(&self, depth: usize) -> Result<(), ControlError> {
        if depth >= MAX_JCS_DEPTH {
            return Err(ControlError::NestingLimitExceeded {
                maximum: MAX_JCS_DEPTH,
            });
        }
        Ok(())
    }

    fn write_string(&mut self, value: &str) -> Result<(), ControlError> {
        self.push_byte(b'"')?;
        for character in value.chars() {
            match character {
                '"' => self.push_bytes(br#"\""#)?,
                '\\' => self.push_bytes(br"\\")?,
                '\u{0008}' => self.push_bytes(br"\b")?,
                '\t' => self.push_bytes(br"\t")?,
                '\n' => self.push_bytes(br"\n")?,
                '\u{000c}' => self.push_bytes(br"\f")?,
                '\r' => self.push_bytes(br"\r")?,
                '\u{0000}'..='\u{001f}' => {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    let value = character as usize;
                    self.push_bytes(&[
                        b'\\',
                        b'u',
                        b'0',
                        b'0',
                        HEX[value >> 4],
                        HEX[value & 0x0f],
                    ])?;
                }
                _ => {
                    let mut encoded = [0; 4];
                    self.push_bytes(character.encode_utf8(&mut encoded).as_bytes())?;
                }
            }
        }
        self.push_byte(b'"')
    }

    fn push_byte(&mut self, byte: u8) -> Result<(), ControlError> {
        self.ensure_capacity(1)?;
        self.output.push(byte);
        Ok(())
    }

    fn push_bytes(&mut self, bytes: &[u8]) -> Result<(), ControlError> {
        self.ensure_capacity(bytes.len())?;
        self.output.extend_from_slice(bytes);
        Ok(())
    }

    fn ensure_capacity(&self, additional: usize) -> Result<(), ControlError> {
        if self
            .output
            .len()
            .checked_add(additional)
            .is_none_or(|length| length > self.maximum)
        {
            return Err(ControlError::ControlObjectTooLarge {
                object: self.object,
                maximum: self.maximum,
            });
        }
        Ok(())
    }
}

fn utf16_cmp(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn utf16_ordering_and_minimal_escaping() {
        let value = json!({
            "\u{e000}": "\u{2028}",
            "\u{1f600}": "\"\\\u{0008}\t\n\u{000c}\r\u{001f}/",
        });

        assert_eq!(
            encode(&value, "test", 256).expect("value is encodable"),
            "{\"😀\":\"\\\"\\\\\\b\\t\\n\\f\\r\\u001f/\",\"\":\"\u{2028}\"}".as_bytes()
        );
    }

    #[test]
    fn number_depth_and_size_rejection() {
        assert_eq!(encode(&json!(1), "test", 1).expect("one is allowed"), b"1");
        assert_eq!(
            encode(&json!(3), "test", 1).expect("Zarr v3 is allowed"),
            b"3"
        );
        assert!(matches!(
            encode(&json!(2), "test", 16),
            Err(ControlError::UnsupportedJsonNumber)
        ));
        assert!(matches!(
            encode(&json!(1.0), "test", 16),
            Err(ControlError::UnsupportedJsonNumber)
        ));

        let too_deep = (0..=MAX_JCS_DEPTH).fold(Value::Null, |value, _| Value::Array(vec![value]));
        assert!(matches!(
            encode(&too_deep, "test", usize::MAX),
            Err(ControlError::NestingLimitExceeded {
                maximum: MAX_JCS_DEPTH
            })
        ));
        assert!(matches!(
            encode(&json!("ab"), "tiny", 3),
            Err(ControlError::ControlObjectTooLarge {
                object: "tiny",
                maximum: 3
            })
        ));
    }
}
