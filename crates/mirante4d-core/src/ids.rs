use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IdError {
    #[error("{kind} id must not be empty")]
    Empty { kind: &'static str },
    #[error("{kind} id must contain only ASCII letters, digits, '-' or '_', got {value:?}")]
    InvalidCharacters { kind: &'static str, value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DatasetId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LayerId(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChannelIndex(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TimeIndex(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScaleLevel(pub u32);

impl DatasetId {
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        validate_id("dataset", value.into()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl LayerId {
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        validate_id("layer", value.into()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DatasetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for LayerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn validate_id(kind: &'static str, value: String) -> Result<String, IdError> {
    if value.is_empty() {
        return Err(IdError::Empty { kind });
    }

    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        Ok(value)
    } else {
        Err(IdError::InvalidCharacters { kind, value })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_stable_ascii_layer_ids() {
        assert_eq!(LayerId::new("ch0").unwrap().as_str(), "ch0");
        assert_eq!(LayerId::new("seg-mask_01").unwrap().as_str(), "seg-mask_01");
    }

    #[test]
    fn rejects_empty_ids() {
        assert_eq!(
            LayerId::new("").unwrap_err(),
            IdError::Empty { kind: "layer" }
        );
    }

    #[test]
    fn rejects_non_ascii_ids() {
        assert_eq!(
            DatasetId::new("cafe").unwrap().as_str(),
            "cafe",
            "plain ASCII is accepted"
        );
        assert!(DatasetId::new("cafe-µ").is_err());
    }
}
