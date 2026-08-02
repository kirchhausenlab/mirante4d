use std::{io, path::PathBuf};

use mirante4d_dataset::CpuLedgerError;
use mirante4d_identity::{IdentityError, ScientificHashError};
use mirante4d_storage::{
    ControlError, PackageWriteError, PackedIndexError, ShardCodecError, StorageProfileError,
    ZarrMetadataError,
};
use thiserror::Error;

/// Typed failure from inspection, bounded production, or publication.
#[derive(Debug, Error)]
pub enum ImportError {
    #[error("import was cancelled; no incomplete destination was published")]
    Cancelled,
    #[error("TIFF source does not exist: {0}")]
    MissingSource(PathBuf),
    #[error("TIFF source layout is ambiguous: {0}")]
    AmbiguousSource(String),
    #[error("unsupported TIFF source: {0}")]
    UnsupportedSource(String),
    #[error("invalid import request: {0}")]
    InvalidRequest(&'static str),
    #[error("source changed after inspection: {0}")]
    SourceChanged(PathBuf),
    #[error("checkpoint is corrupt or belongs to different inputs: {0}")]
    InvalidCheckpoint(String),
    #[error("insufficient free space: need {required_bytes} bytes, found {available_bytes}")]
    InsufficientSpace {
        required_bytes: u64,
        available_bytes: u64,
    },
    #[error(
        "preprocessing paused before its next durable step: need {required_bytes} additional filesystem bytes, found {available_bytes}; free space and Resume to continue from the saved checkpoint"
    )]
    CapacityPaused {
        required_bytes: u64,
        available_bytes: u64,
    },
    #[error(
        "the minimum complete preprocessing path needs {required_bytes} managed CPU bytes, but this process provides {capacity_bytes}"
    )]
    ManagedCapacityInsufficient {
        required_bytes: u64,
        capacity_bytes: u64,
    },
    #[error("TIFF operation failed for {path}: {message}")]
    Tiff { path: PathBuf, message: String },
    #[error("import filesystem operation {operation} failed for {path}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "checkpoint durability became indeterminate while attempting to {operation} for {path}"
    )]
    CheckpointDurabilityIndeterminate {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("import byte/count arithmetic overflowed")]
    Overflow,
    #[error(transparent)]
    Ledger(#[from] CpuLedgerError),
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error(transparent)]
    Scientific(#[from] ScientificHashError),
    #[error(transparent)]
    Storage(#[from] StorageProfileError),
    #[error(transparent)]
    Codec(#[from] ShardCodecError),
    #[error(transparent)]
    PackedIndex(#[from] PackedIndexError),
    #[error(transparent)]
    ZarrMetadata(#[from] ZarrMetadataError),
    #[error(transparent)]
    Writer(#[from] PackageWriteError),
}
