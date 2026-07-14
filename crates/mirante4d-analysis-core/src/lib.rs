//! Pure exact analysis over immutable semantic dataset resources.
//!
//! This crate owns definitions, deterministic block planning, scalar
//! reduction, and canonical table/plot payloads. It performs no I/O, starts no
//! workers, and knows nothing about application or project-store state.

#![forbid(unsafe_code)]

mod artifact;
mod plan;
mod reduce;

pub use artifact::{
    ANALYSIS_PLOT_MEDIA_TYPE, ANALYSIS_PLOT_OBJECT_ROLE, ANALYSIS_TABLE_MEDIA_TYPE,
    ANALYSIS_TABLE_OBJECT_ROLE, AnalysisArtifactSet, AnalysisPlot, AnalysisPlotArtifact,
    AnalysisPlotPoint, AnalysisProvenance, AnalysisTable, AnalysisTableArtifact,
};
pub use plan::{
    AnalysisBlock, AnalysisDefinition, AnalysisOperation, AnalysisPlan,
    DEFAULT_ANALYSIS_BLOCK_SHAPE,
};
pub use reduce::{AnalysisAccumulator, IntensityStatistics};

use thiserror::Error;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisError {
    #[error("analysis requires a verified scientific source identity")]
    UnverifiedSource,
    #[error("the requested logical layer is absent")]
    UnknownLayer,
    #[error("the analysis time range is empty or outside the layer")]
    InvalidTimeRange,
    #[error("the box ROI is empty or outside the base-scale layer")]
    InvalidRegion,
    #[error("the analysis block shape exceeds the supported 64x64x64 bound")]
    InvalidBlockShape,
    #[error("the analysis plan exceeds a checked integer bound")]
    CapacityExceeded,
    #[error("a streamed block did not match the next deterministic plan block")]
    UnexpectedBlock,
    #[error("a decoded payload did not match its planned block")]
    PayloadMismatch,
    #[error("a scientifically valid float32 sample was not finite")]
    NonFiniteFloat,
    #[error("the exact reduction overflowed its integer accumulator")]
    AccumulatorOverflow,
    #[error("the analysis finished before all planned blocks were reduced")]
    Incomplete,
    #[error("an analysis artifact payload was invalid or non-canonical")]
    InvalidArtifact,
    #[error("an analysis identity could not be computed")]
    Identity,
}

#[cfg(test)]
mod tests;
