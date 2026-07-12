use thiserror::Error;

/// A strict profile or preflight failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum StorageProfileError {
    #[error("invalid package path: {reason}")]
    InvalidPath { reason: &'static str },
    #[error("duplicate package path {path}")]
    DuplicatePath { path: String },
    #[error("{metric} arithmetic overflowed")]
    ArithmeticOverflow { metric: &'static str },
    #[error("{profile} exceeds {metric}: observed {actual}, maximum {maximum}")]
    CeilingExceeded {
        profile: &'static str,
        metric: &'static str,
        actual: u64,
        maximum: u64,
    },
    #[error("{profile} requires exactly {expected} {metric}, observed {actual}")]
    ExactCountMismatch {
        profile: &'static str,
        metric: &'static str,
        actual: u64,
        expected: u64,
    },
    #[error("{metric} must be positive")]
    ZeroCount { metric: &'static str },
    #[error("inconsistent {metric}: reported {reported}, computed {computed}")]
    InconsistentCount {
        metric: &'static str,
        reported: u64,
        computed: u64,
    },
    #[error("a dataset geometry dimension or scale count is zero")]
    ZeroGeometry,
}
