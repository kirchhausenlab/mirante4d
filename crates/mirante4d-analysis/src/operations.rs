use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{AnalysisError, AnalysisProvenance, AnalysisResultState};

pub const ANALYSIS_OPERATION_SCHEMA_VERSION: u32 = 1;
pub const ROI_INTENSITY_OPERATION_VERSION: u32 = 1;
pub const FULL_INTENSITY_SUMMARY_OPERATION_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisOperationKind {
    FullIntensitySummary,
    RoiIntensityStatistics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisExecutionState {
    Queued,
    Running,
    Cancelling,
    Cancelled,
    Failed,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisParameterValue {
    Integer(i64),
    Unsigned(u64),
    Float(f64),
    Text(String),
    Boolean(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisTimeScope {
    pub start: u64,
    pub end_exclusive: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisSpatialScope {
    WholeVolume,
    Roi {
        roi_id: String,
    },
    WorldBox {
        min_xyz: [f64; 3],
        max_xyz: [f64; 3],
        unit: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisOperationInput {
    pub dataset_id: String,
    pub dataset_name: String,
    pub native_format: String,
    pub native_schema_version: u32,
    pub layer_id: String,
    pub time_scope: AnalysisTimeScope,
    pub scale_level: u32,
    pub spatial_scope: AnalysisSpatialScope,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisOperationRecord {
    pub schema_version: u32,
    pub operation_id: String,
    pub operation_version: u32,
    pub kind: AnalysisOperationKind,
    pub input: AnalysisOperationInput,
    pub parameters: BTreeMap<String, AnalysisParameterValue>,
    pub execution_state: AnalysisExecutionState,
    pub result_state: AnalysisResultState,
    pub provenance: Option<AnalysisProvenance>,
}

impl AnalysisExecutionState {
    pub fn transition_to(self, next: Self) -> Result<Self, AnalysisError> {
        let allowed = matches!(
            (self, next),
            (Self::Queued, Self::Running)
                | (Self::Queued, Self::Cancelled)
                | (Self::Running, Self::Cancelling)
                | (Self::Running, Self::Failed)
                | (Self::Running, Self::Complete)
                | (Self::Cancelling, Self::Cancelled)
        );
        if allowed {
            Ok(next)
        } else {
            Err(AnalysisError::InvalidAnalysisOperation(format!(
                "invalid analysis execution transition from {self:?} to {next:?}"
            )))
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Cancelled | Self::Failed | Self::Complete)
    }
}

impl AnalysisTimeScope {
    pub fn new(start: u64, end_exclusive: u64) -> Result<Self, AnalysisError> {
        let scope = Self {
            start,
            end_exclusive,
        };
        scope.validate()?;
        Ok(scope)
    }

    pub fn validate(self) -> Result<(), AnalysisError> {
        if self.start >= self.end_exclusive {
            return Err(AnalysisError::InvalidAnalysisOperation(format!(
                "analysis time scope must satisfy start < end_exclusive, got {}..{}",
                self.start, self.end_exclusive
            )));
        }
        Ok(())
    }
}

impl AnalysisParameterValue {
    fn validate(&self, key: &str) -> Result<(), AnalysisError> {
        match self {
            Self::Float(value) if !value.is_finite() => {
                Err(AnalysisError::InvalidAnalysisOperation(format!(
                    "analysis parameter {key:?} must be finite"
                )))
            }
            Self::Text(value) if value.trim().is_empty() => {
                Err(AnalysisError::InvalidAnalysisOperation(format!(
                    "analysis parameter {key:?} must not be empty text"
                )))
            }
            _ => Ok(()),
        }
    }

    pub fn as_metadata_string(&self) -> String {
        match self {
            Self::Integer(value) => value.to_string(),
            Self::Unsigned(value) => value.to_string(),
            Self::Float(value) => format!("{value:.12}"),
            Self::Text(value) => value.clone(),
            Self::Boolean(value) => value.to_string(),
        }
    }
}

impl AnalysisSpatialScope {
    fn validate(&self) -> Result<(), AnalysisError> {
        match self {
            Self::WholeVolume => Ok(()),
            Self::Roi { roi_id } => validate_nonempty("ROI id", roi_id),
            Self::WorldBox {
                min_xyz,
                max_xyz,
                unit,
            } => {
                validate_nonempty("world-box unit", unit)?;
                validate_finite_xyz("world-box min", min_xyz)?;
                validate_finite_xyz("world-box max", max_xyz)?;
                if min_xyz
                    .iter()
                    .zip(max_xyz.iter())
                    .all(|(min, max)| min < max)
                {
                    Ok(())
                } else {
                    Err(AnalysisError::InvalidAnalysisOperation(
                        "analysis world-box scope must have min < max along x, y, and z".to_owned(),
                    ))
                }
            }
        }
    }
}

impl AnalysisOperationInput {
    pub fn validate(&self) -> Result<(), AnalysisError> {
        validate_nonempty("dataset id", &self.dataset_id)?;
        validate_nonempty("dataset name", &self.dataset_name)?;
        validate_nonempty("native format", &self.native_format)?;
        validate_nonempty("layer id", &self.layer_id)?;
        self.time_scope.validate()?;
        self.spatial_scope.validate()
    }
}

impl AnalysisOperationRecord {
    pub fn new(
        operation_id: impl Into<String>,
        operation_version: u32,
        kind: AnalysisOperationKind,
        input: AnalysisOperationInput,
        parameters: BTreeMap<String, AnalysisParameterValue>,
        result_state: AnalysisResultState,
    ) -> Result<Self, AnalysisError> {
        let record = Self {
            schema_version: ANALYSIS_OPERATION_SCHEMA_VERSION,
            operation_id: operation_id.into(),
            operation_version,
            kind,
            input,
            parameters,
            execution_state: AnalysisExecutionState::Queued,
            result_state,
            provenance: None,
        };
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> Result<(), AnalysisError> {
        if self.schema_version != ANALYSIS_OPERATION_SCHEMA_VERSION {
            return Err(AnalysisError::InvalidAnalysisOperation(format!(
                "unsupported analysis operation schema version {}",
                self.schema_version
            )));
        }
        validate_nonempty("operation id", &self.operation_id)?;
        if self.operation_version == 0 {
            return Err(AnalysisError::InvalidAnalysisOperation(
                "analysis operation version must be nonzero".to_owned(),
            ));
        }
        self.input.validate()?;
        for (key, value) in &self.parameters {
            validate_nonempty("analysis parameter key", key)?;
            value.validate(key)?;
        }
        if self.execution_state.is_terminal()
            && matches!(
                self.result_state,
                AnalysisResultState::Preview
                    | AnalysisResultState::Approximate
                    | AnalysisResultState::Partial
            )
        {
            return Err(AnalysisError::InvalidAnalysisOperation(
                "terminal analysis execution cannot retain preview, approximate, or partial result state"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    pub fn with_execution_state(mut self, state: AnalysisExecutionState) -> Self {
        self.execution_state = state;
        self
    }

    pub fn with_provenance(mut self, provenance: AnalysisProvenance) -> Self {
        self.provenance = Some(provenance);
        self
    }
}

fn validate_nonempty(label: &str, value: &str) -> Result<(), AnalysisError> {
    if value.trim().is_empty() {
        Err(AnalysisError::InvalidAnalysisOperation(format!(
            "analysis {label} must not be empty"
        )))
    } else {
        Ok(())
    }
}

fn validate_finite_xyz(label: &str, xyz: &[f64; 3]) -> Result<(), AnalysisError> {
    if xyz.iter().all(|value| value.is_finite()) {
        Ok(())
    } else {
        Err(AnalysisError::InvalidAnalysisOperation(format!(
            "analysis {label} coordinates must be finite"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_record_validates_and_serializes_exact_roi_scope() {
        let record = AnalysisOperationRecord::new(
            "roi-intensity-statistics",
            ROI_INTENSITY_OPERATION_VERSION,
            AnalysisOperationKind::RoiIntensityStatistics,
            AnalysisOperationInput {
                dataset_id: "dataset-a".to_owned(),
                dataset_name: "Dataset A".to_owned(),
                native_format: "mirante4d-v1".to_owned(),
                native_schema_version: 1,
                layer_id: "ch0".to_owned(),
                time_scope: AnalysisTimeScope::new(0, 1).unwrap(),
                scale_level: 0,
                spatial_scope: AnalysisSpatialScope::Roi {
                    roi_id: "roi-a".to_owned(),
                },
            },
            BTreeMap::from([("roi_count".to_owned(), AnalysisParameterValue::Unsigned(1))]),
            AnalysisResultState::Complete,
        )
        .unwrap();

        let encoded = serde_json::to_string(&record).unwrap();
        let decoded: AnalysisOperationRecord = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded.schema_version, ANALYSIS_OPERATION_SCHEMA_VERSION);
        assert_eq!(decoded.kind, AnalysisOperationKind::RoiIntensityStatistics);
        assert_eq!(decoded.input.time_scope.start, 0);
        assert_eq!(decoded.result_state, AnalysisResultState::Complete);
    }

    #[test]
    fn operation_validation_rejects_invalid_time_and_spatial_scopes() {
        let invalid_time = AnalysisTimeScope::new(2, 2).unwrap_err();
        assert!(invalid_time.to_string().contains("start < end_exclusive"));

        let input = AnalysisOperationInput {
            dataset_id: "dataset-a".to_owned(),
            dataset_name: "Dataset A".to_owned(),
            native_format: "mirante4d-v1".to_owned(),
            native_schema_version: 1,
            layer_id: "ch0".to_owned(),
            time_scope: AnalysisTimeScope::new(0, 1).unwrap(),
            scale_level: 0,
            spatial_scope: AnalysisSpatialScope::WorldBox {
                min_xyz: [0.0, 0.0, 0.0],
                max_xyz: [1.0, f64::NAN, 1.0],
                unit: "um".to_owned(),
            },
        };

        assert!(input.validate().unwrap_err().to_string().contains("finite"));
    }

    #[test]
    fn execution_state_transitions_are_explicit_and_terminal() {
        let running = AnalysisExecutionState::Queued
            .transition_to(AnalysisExecutionState::Running)
            .unwrap();
        let cancelling = running
            .transition_to(AnalysisExecutionState::Cancelling)
            .unwrap();
        let cancelled = cancelling
            .transition_to(AnalysisExecutionState::Cancelled)
            .unwrap();

        assert!(cancelled.is_terminal());
        assert!(
            cancelled
                .transition_to(AnalysisExecutionState::Running)
                .unwrap_err()
                .to_string()
                .contains("invalid analysis execution transition")
        );
    }

    #[test]
    fn terminal_execution_cannot_claim_preview_result_state() {
        let mut record = AnalysisOperationRecord::new(
            "preview",
            1,
            AnalysisOperationKind::FullIntensitySummary,
            AnalysisOperationInput {
                dataset_id: "dataset-a".to_owned(),
                dataset_name: "Dataset A".to_owned(),
                native_format: "mirante4d-v1".to_owned(),
                native_schema_version: 1,
                layer_id: "ch0".to_owned(),
                time_scope: AnalysisTimeScope::new(0, 1).unwrap(),
                scale_level: 0,
                spatial_scope: AnalysisSpatialScope::WholeVolume,
            },
            BTreeMap::new(),
            AnalysisResultState::Preview,
        )
        .unwrap();
        record.execution_state = AnalysisExecutionState::Complete;

        assert!(
            record
                .validate()
                .unwrap_err()
                .to_string()
                .contains("terminal")
        );
    }
}
