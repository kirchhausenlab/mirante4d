use std::fmt::Write as _;

use mirante4d_dataset::ResourceRegion;
use mirante4d_domain::{IntensityDType, LogicalLayerKey, Shape3D};
use mirante4d_identity::{
    ArtifactContentId, DerivationRecordId, ExactBytesHasher, MediaType, ObjectRole,
    RawObjectDescriptor, RecipeId, ScientificContentId, Sha256Hasher,
};
use serde::{Deserialize, Serialize};

use crate::{AnalysisDefinition, AnalysisError, AnalysisOperation, IntensityStatistics};

pub const ANALYSIS_TABLE_MEDIA_TYPE: &str = "application/vnd.mirante4d.analysis-table-v1+json";
pub const ANALYSIS_TABLE_OBJECT_ROLE: &str = "artifact.analysis-table.v1";
pub const ANALYSIS_PLOT_MEDIA_TYPE: &str = "application/vnd.mirante4d.analysis-plot-v1+json";
pub const ANALYSIS_PLOT_OBJECT_ROLE: &str = "artifact.analysis-plot.v1";

const TABLE_SCHEMA: &str = "mirante4d-analysis-table-v1";
const PLOT_SCHEMA: &str = "mirante4d-analysis-plot-v1";
const RECIPE_SCHEMA: &str = "mirante4d-analysis-recipe-v1";
const DERIVATION_SCHEMA: &str = "mirante4d-analysis-derivation-v1";
const ARTIFACT_DOMAIN: &[u8] = b"M4D-ARTIFACT-V1\0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisProvenance {
    source_content_id: ScientificContentId,
    source_layer: LogicalLayerKey,
    dtype: IntensityDType,
    time_start: u64,
    time_end_exclusive: u64,
    spatial_region: ResourceRegion,
    block_shape: Shape3D,
    operation: AnalysisOperation,
    recipe_id: RecipeId,
    derivation_id: DerivationRecordId,
}

impl AnalysisProvenance {
    pub const fn source_content_id(&self) -> ScientificContentId {
        self.source_content_id
    }

    pub const fn source_layer(&self) -> LogicalLayerKey {
        self.source_layer
    }

    pub const fn dtype(&self) -> IntensityDType {
        self.dtype
    }

    pub const fn time_start(&self) -> u64 {
        self.time_start
    }

    pub const fn time_end_exclusive(&self) -> u64 {
        self.time_end_exclusive
    }

    pub const fn spatial_region(&self) -> ResourceRegion {
        self.spatial_region
    }

    pub const fn block_shape(&self) -> Shape3D {
        self.block_shape
    }

    pub const fn operation(&self) -> AnalysisOperation {
        self.operation
    }

    pub const fn recipe_id(&self) -> RecipeId {
        self.recipe_id
    }

    pub const fn derivation_id(&self) -> DerivationRecordId {
        self.derivation_id
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnalysisTable {
    name: String,
    provenance: AnalysisProvenance,
    rows: Vec<IntensityStatistics>,
}

impl AnalysisTable {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn provenance(&self) -> &AnalysisProvenance {
        &self.provenance
    }

    pub fn rows(&self) -> &[IntensityStatistics] {
        &self.rows
    }

    pub fn to_csv(&self) -> String {
        let mut csv = String::from(
            "timepoint,geometric_sample_count,valid_sample_count,nonzero_sample_count,minimum,maximum,sum,mean,population_variance\n",
        );
        for row in &self.rows {
            let values = [
                row.minimum(),
                row.maximum(),
                row.sum(),
                row.mean(),
                row.population_variance(),
            ];
            let _ = write!(
                csv,
                "{},{},{},{}",
                row.timepoint(),
                row.geometric_sample_count(),
                row.valid_sample_count(),
                row.nonzero_sample_count()
            );
            for value in values {
                csv.push(',');
                if let Some(value) = value {
                    let _ = write!(csv, "{value:.17}");
                }
            }
            csv.push('\n');
        }
        csv
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalysisPlotPoint {
    timepoint: u64,
    mean: Option<f64>,
}

impl AnalysisPlotPoint {
    pub const fn timepoint(self) -> u64 {
        self.timepoint
    }

    pub const fn mean(self) -> Option<f64> {
        self.mean
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnalysisPlot {
    name: String,
    provenance: AnalysisProvenance,
    points: Vec<AnalysisPlotPoint>,
}

impl AnalysisPlot {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn provenance(&self) -> &AnalysisProvenance {
        &self.provenance
    }

    pub fn points(&self) -> &[AnalysisPlotPoint] {
        &self.points
    }

    pub const fn x_label(&self) -> &'static str {
        "timepoint"
    }

    pub const fn y_label(&self) -> &'static str {
        "mean intensity"
    }
}

#[derive(Debug, Clone)]
pub struct AnalysisTableArtifact {
    value: AnalysisTable,
    bytes: Box<[u8]>,
    content_id: ArtifactContentId,
    descriptor: RawObjectDescriptor,
}

impl AnalysisTableArtifact {
    pub const fn value(&self) -> &AnalysisTable {
        &self.value
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub const fn content_id(&self) -> ArtifactContentId {
        self.content_id
    }

    pub const fn descriptor(&self) -> &RawObjectDescriptor {
        &self.descriptor
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, AnalysisError> {
        let wire: TableWire =
            serde_json::from_slice(bytes).map_err(|_| AnalysisError::InvalidArtifact)?;
        if serde_json::to_vec(&wire).map_err(|_| AnalysisError::InvalidArtifact)? != bytes {
            return Err(AnalysisError::InvalidArtifact);
        }
        let value = wire.into_value()?;
        build_table_artifact(value, bytes.to_vec().into_boxed_slice())
    }
}

#[derive(Debug, Clone)]
pub struct AnalysisPlotArtifact {
    value: AnalysisPlot,
    bytes: Box<[u8]>,
    content_id: ArtifactContentId,
    descriptor: RawObjectDescriptor,
}

impl AnalysisPlotArtifact {
    pub const fn value(&self) -> &AnalysisPlot {
        &self.value
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub const fn content_id(&self) -> ArtifactContentId {
        self.content_id
    }

    pub const fn descriptor(&self) -> &RawObjectDescriptor {
        &self.descriptor
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, AnalysisError> {
        let wire: PlotWire =
            serde_json::from_slice(bytes).map_err(|_| AnalysisError::InvalidArtifact)?;
        if serde_json::to_vec(&wire).map_err(|_| AnalysisError::InvalidArtifact)? != bytes {
            return Err(AnalysisError::InvalidArtifact);
        }
        let value = wire.into_value()?;
        build_plot_artifact(value, bytes.to_vec().into_boxed_slice())
    }
}

#[derive(Debug, Clone)]
pub struct AnalysisArtifactSet {
    table: AnalysisTableArtifact,
    plot: Option<AnalysisPlotArtifact>,
}

impl AnalysisArtifactSet {
    pub const fn table(&self) -> &AnalysisTableArtifact {
        &self.table
    }

    pub const fn plot(&self) -> Option<&AnalysisPlotArtifact> {
        self.plot.as_ref()
    }

    pub fn payload_bytes(&self) -> u64 {
        u64::try_from(self.table.bytes.len()).expect("artifact byte slices fit u64")
            + self.plot.as_ref().map_or(0, |plot| {
                u64::try_from(plot.bytes.len()).expect("bytes fit u64")
            })
    }
}

pub(crate) fn build_artifacts(
    definition: &AnalysisDefinition,
    rows: Vec<IntensityStatistics>,
) -> Result<AnalysisArtifactSet, AnalysisError> {
    let provenance = provenance_from_definition(definition)?;
    validate_rows(&provenance, &rows)?;
    let table = AnalysisTable {
        name: match definition.operation() {
            AnalysisOperation::FullIntensitySummary => "Full intensity summary".to_owned(),
            AnalysisOperation::BoxRoiIntensityStatistics => {
                "Box ROI intensity statistics".to_owned()
            }
        },
        provenance: provenance.clone(),
        rows,
    };
    let table_bytes = serde_json::to_vec(&TableWire::from_value(&table))
        .map_err(|_| AnalysisError::InvalidArtifact)?
        .into_boxed_slice();
    let table = build_table_artifact(table, table_bytes)?;
    let plot = if definition.operation() == AnalysisOperation::FullIntensitySummary {
        let value = AnalysisPlot {
            name: "Mean intensity over time".to_owned(),
            provenance,
            points: table
                .value()
                .rows()
                .iter()
                .map(|row| AnalysisPlotPoint {
                    timepoint: row.timepoint(),
                    mean: row.mean(),
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&PlotWire::from_value(&value))
            .map_err(|_| AnalysisError::InvalidArtifact)?
            .into_boxed_slice();
        Some(build_plot_artifact(value, bytes)?)
    } else {
        None
    };
    Ok(AnalysisArtifactSet { table, plot })
}

fn build_table_artifact(
    value: AnalysisTable,
    bytes: Box<[u8]>,
) -> Result<AnalysisTableArtifact, AnalysisError> {
    validate_provenance(&value.provenance)?;
    validate_rows(&value.provenance, &value.rows)?;
    let (content_id, descriptor) = artifact_identity(
        &bytes,
        ANALYSIS_TABLE_MEDIA_TYPE,
        ANALYSIS_TABLE_OBJECT_ROLE,
    )?;
    Ok(AnalysisTableArtifact {
        value,
        bytes,
        content_id,
        descriptor,
    })
}

fn build_plot_artifact(
    value: AnalysisPlot,
    bytes: Box<[u8]>,
) -> Result<AnalysisPlotArtifact, AnalysisError> {
    validate_provenance(&value.provenance)?;
    let expected = value
        .provenance
        .time_end_exclusive
        .checked_sub(value.provenance.time_start)
        .ok_or(AnalysisError::InvalidArtifact)?;
    if usize::try_from(expected).ok() != Some(value.points.len())
        || value.points.iter().enumerate().any(|(index, point)| {
            point.timepoint != value.provenance.time_start + index as u64
                || point.mean.is_some_and(|mean| !mean.is_finite())
        })
    {
        return Err(AnalysisError::InvalidArtifact);
    }
    let (content_id, descriptor) =
        artifact_identity(&bytes, ANALYSIS_PLOT_MEDIA_TYPE, ANALYSIS_PLOT_OBJECT_ROLE)?;
    Ok(AnalysisPlotArtifact {
        value,
        bytes,
        content_id,
        descriptor,
    })
}

fn provenance_from_definition(
    definition: &AnalysisDefinition,
) -> Result<AnalysisProvenance, AnalysisError> {
    let recipe_wire = RecipeWire::from_definition(definition);
    let recipe_body =
        serde_json::to_vec(&recipe_wire).map_err(|_| AnalysisError::InvalidArtifact)?;
    let recipe_id =
        RecipeId::from_canonical_body_bytes(&recipe_body).map_err(|_| AnalysisError::Identity)?;
    let derivation_wire = DerivationWire {
        recipe_id: recipe_id.to_string(),
        schema: DERIVATION_SCHEMA.to_owned(),
        source_content_id: definition.source_content_id().to_string(),
    };
    let derivation_body =
        serde_json::to_vec(&derivation_wire).map_err(|_| AnalysisError::InvalidArtifact)?;
    let derivation_id = DerivationRecordId::from_canonical_body_bytes(&derivation_body)
        .map_err(|_| AnalysisError::Identity)?;
    Ok(AnalysisProvenance {
        source_content_id: definition.source_content_id(),
        source_layer: definition.layer(),
        dtype: definition.dtype(),
        time_start: definition.time_start(),
        time_end_exclusive: definition.time_end_exclusive(),
        spatial_region: definition.spatial_region(),
        block_shape: definition.block_shape(),
        operation: definition.operation(),
        recipe_id,
        derivation_id,
    })
}

fn validate_provenance(provenance: &AnalysisProvenance) -> Result<(), AnalysisError> {
    let recipe_wire = RecipeWire::from_provenance(provenance);
    let recipe_body =
        serde_json::to_vec(&recipe_wire).map_err(|_| AnalysisError::InvalidArtifact)?;
    let recipe_id =
        RecipeId::from_canonical_body_bytes(&recipe_body).map_err(|_| AnalysisError::Identity)?;
    let derivation_wire = DerivationWire {
        recipe_id: recipe_id.to_string(),
        schema: DERIVATION_SCHEMA.to_owned(),
        source_content_id: provenance.source_content_id.to_string(),
    };
    let derivation_body =
        serde_json::to_vec(&derivation_wire).map_err(|_| AnalysisError::InvalidArtifact)?;
    let derivation_id = DerivationRecordId::from_canonical_body_bytes(&derivation_body)
        .map_err(|_| AnalysisError::Identity)?;
    if recipe_id != provenance.recipe_id || derivation_id != provenance.derivation_id {
        return Err(AnalysisError::InvalidArtifact);
    }
    Ok(())
}

fn validate_rows(
    provenance: &AnalysisProvenance,
    rows: &[IntensityStatistics],
) -> Result<(), AnalysisError> {
    let expected = provenance
        .time_end_exclusive
        .checked_sub(provenance.time_start)
        .ok_or(AnalysisError::InvalidArtifact)?;
    let geometric_per_timepoint = provenance
        .spatial_region
        .shape()
        .element_count()
        .map_err(|_| AnalysisError::InvalidArtifact)?;
    if usize::try_from(expected).ok() != Some(rows.len()) {
        return Err(AnalysisError::InvalidArtifact);
    }
    for (index, row) in rows.iter().enumerate() {
        let all_values = [
            row.minimum(),
            row.maximum(),
            row.sum(),
            row.mean(),
            row.population_variance(),
        ];
        let values_present = all_values.iter().filter(|value| value.is_some()).count();
        if row.timepoint() != provenance.time_start + index as u64
            || row.geometric_sample_count() != geometric_per_timepoint
            || row.valid_sample_count() > row.geometric_sample_count()
            || row.nonzero_sample_count() > row.valid_sample_count()
            || (row.valid_sample_count() == 0 && values_present != 0)
            || (row.valid_sample_count() != 0 && values_present != all_values.len())
            || all_values
                .into_iter()
                .flatten()
                .any(|value| !value.is_finite())
            || row.population_variance().is_some_and(|value| value < 0.0)
            || matches!((row.minimum(), row.maximum()), (Some(min), Some(max)) if min > max)
        {
            return Err(AnalysisError::InvalidArtifact);
        }
    }
    Ok(())
}

fn artifact_identity(
    bytes: &[u8],
    media_type: &str,
    role: &str,
) -> Result<(ArtifactContentId, RawObjectDescriptor), AnalysisError> {
    let body_len = u64::try_from(bytes.len()).map_err(|_| AnalysisError::Identity)?;
    let mut artifact_hasher = Sha256Hasher::new();
    artifact_hasher.update(ARTIFACT_DOMAIN);
    artifact_hasher.update(body_len.to_be_bytes());
    artifact_hasher.update(bytes);
    let content_id = ArtifactContentId::from_digest(artifact_hasher.finalize());
    let exact = ExactBytesHasher::hash(bytes).map_err(|_| AnalysisError::Identity)?;
    let descriptor = RawObjectDescriptor::new(
        exact.digest(),
        exact.byte_length(),
        MediaType::parse(media_type).expect("analysis media types are frozen valid constants"),
        ObjectRole::parse(role).expect("analysis object roles are frozen valid constants"),
    );
    Ok((content_id, descriptor))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TableWire {
    name: String,
    provenance: ProvenanceWire,
    rows: Vec<StatisticsWire>,
    schema: String,
}

impl TableWire {
    fn from_value(value: &AnalysisTable) -> Self {
        Self {
            name: value.name.clone(),
            provenance: ProvenanceWire::from_value(&value.provenance),
            rows: value.rows.iter().map(StatisticsWire::from_value).collect(),
            schema: TABLE_SCHEMA.to_owned(),
        }
    }

    fn into_value(self) -> Result<AnalysisTable, AnalysisError> {
        if self.schema != TABLE_SCHEMA || self.name.trim().is_empty() {
            return Err(AnalysisError::InvalidArtifact);
        }
        Ok(AnalysisTable {
            name: self.name,
            provenance: self.provenance.into_value()?,
            rows: self
                .rows
                .into_iter()
                .map(StatisticsWire::into_value)
                .collect::<Result<_, _>>()?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlotWire {
    name: String,
    points: Vec<PlotPointWire>,
    provenance: ProvenanceWire,
    schema: String,
    x_label: String,
    y_label: String,
}

impl PlotWire {
    fn from_value(value: &AnalysisPlot) -> Self {
        Self {
            name: value.name.clone(),
            points: value.points.iter().map(PlotPointWire::from_value).collect(),
            provenance: ProvenanceWire::from_value(&value.provenance),
            schema: PLOT_SCHEMA.to_owned(),
            x_label: value.x_label().to_owned(),
            y_label: value.y_label().to_owned(),
        }
    }

    fn into_value(self) -> Result<AnalysisPlot, AnalysisError> {
        if self.schema != PLOT_SCHEMA
            || self.name.trim().is_empty()
            || self.x_label != "timepoint"
            || self.y_label != "mean intensity"
        {
            return Err(AnalysisError::InvalidArtifact);
        }
        Ok(AnalysisPlot {
            name: self.name,
            provenance: self.provenance.into_value()?,
            points: self
                .points
                .into_iter()
                .map(PlotPointWire::into_value)
                .collect::<Result<_, _>>()?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvenanceWire {
    block_shape_zyx: [u64; 3],
    derivation_id: String,
    dtype: String,
    operation: String,
    recipe_id: String,
    source_content_id: String,
    source_layer: u32,
    spatial_origin_zyx: [u64; 3],
    spatial_shape_zyx: [u64; 3],
    time_end_exclusive: u64,
    time_start: u64,
}

impl ProvenanceWire {
    fn from_value(value: &AnalysisProvenance) -> Self {
        Self {
            block_shape_zyx: value.block_shape.dimensions(),
            derivation_id: value.derivation_id.to_string(),
            dtype: dtype_name(value.dtype).to_owned(),
            operation: value.operation.contract_name().to_owned(),
            recipe_id: value.recipe_id.to_string(),
            source_content_id: value.source_content_id.to_string(),
            source_layer: value.source_layer.ordinal(),
            spatial_origin_zyx: value.spatial_region.origin(),
            spatial_shape_zyx: value.spatial_region.shape().dimensions(),
            time_end_exclusive: value.time_end_exclusive,
            time_start: value.time_start,
        }
    }

    fn into_value(self) -> Result<AnalysisProvenance, AnalysisError> {
        if self.time_start >= self.time_end_exclusive
            || self
                .block_shape_zyx
                .into_iter()
                .any(|value| value == 0 || value > 64)
        {
            return Err(AnalysisError::InvalidArtifact);
        }
        let spatial_shape = Shape3D::new(
            self.spatial_shape_zyx[0],
            self.spatial_shape_zyx[1],
            self.spatial_shape_zyx[2],
        )
        .map_err(|_| AnalysisError::InvalidArtifact)?;
        let spatial_region = ResourceRegion::new(self.spatial_origin_zyx, spatial_shape)
            .map_err(|_| AnalysisError::InvalidArtifact)?;
        Ok(AnalysisProvenance {
            source_content_id: ScientificContentId::parse(&self.source_content_id)
                .map_err(|_| AnalysisError::InvalidArtifact)?,
            source_layer: LogicalLayerKey::new(self.source_layer),
            dtype: parse_dtype(&self.dtype)?,
            time_start: self.time_start,
            time_end_exclusive: self.time_end_exclusive,
            spatial_region,
            block_shape: Shape3D::new(
                self.block_shape_zyx[0],
                self.block_shape_zyx[1],
                self.block_shape_zyx[2],
            )
            .map_err(|_| AnalysisError::InvalidArtifact)?,
            operation: parse_operation(&self.operation)?,
            recipe_id: RecipeId::parse(&self.recipe_id)
                .map_err(|_| AnalysisError::InvalidArtifact)?,
            derivation_id: DerivationRecordId::parse(&self.derivation_id)
                .map_err(|_| AnalysisError::InvalidArtifact)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatisticsWire {
    geometric_sample_count: u64,
    maximum_bits: Option<String>,
    mean_bits: Option<String>,
    minimum_bits: Option<String>,
    nonzero_sample_count: u64,
    population_variance_bits: Option<String>,
    sum_bits: Option<String>,
    timepoint: u64,
    valid_sample_count: u64,
}

impl StatisticsWire {
    fn from_value(value: &IntensityStatistics) -> Self {
        Self {
            geometric_sample_count: value.geometric_sample_count(),
            maximum_bits: value.maximum().map(float_bits),
            mean_bits: value.mean().map(float_bits),
            minimum_bits: value.minimum().map(float_bits),
            nonzero_sample_count: value.nonzero_sample_count(),
            population_variance_bits: value.population_variance().map(float_bits),
            sum_bits: value.sum().map(float_bits),
            timepoint: value.timepoint(),
            valid_sample_count: value.valid_sample_count(),
        }
    }

    fn into_value(self) -> Result<IntensityStatistics, AnalysisError> {
        Ok(IntensityStatistics {
            timepoint: self.timepoint,
            geometric_sample_count: self.geometric_sample_count,
            valid_sample_count: self.valid_sample_count,
            nonzero_sample_count: self.nonzero_sample_count,
            minimum: parse_optional_float(self.minimum_bits)?,
            maximum: parse_optional_float(self.maximum_bits)?,
            sum: parse_optional_float(self.sum_bits)?,
            mean: parse_optional_float(self.mean_bits)?,
            population_variance: parse_optional_float(self.population_variance_bits)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlotPointWire {
    mean_bits: Option<String>,
    timepoint: u64,
}

impl PlotPointWire {
    fn from_value(value: &AnalysisPlotPoint) -> Self {
        Self {
            mean_bits: value.mean.map(float_bits),
            timepoint: value.timepoint,
        }
    }

    fn into_value(self) -> Result<AnalysisPlotPoint, AnalysisError> {
        Ok(AnalysisPlotPoint {
            timepoint: self.timepoint,
            mean: parse_optional_float(self.mean_bits)?,
        })
    }
}

#[derive(Debug, Serialize)]
struct RecipeWire {
    block_shape_zyx: [u64; 3],
    dtype: &'static str,
    operation: &'static str,
    schema: &'static str,
    source_layer: u32,
    spatial_origin_zyx: [u64; 3],
    spatial_shape_zyx: [u64; 3],
    time_end_exclusive: u64,
    time_start: u64,
}

impl RecipeWire {
    fn from_definition(value: &AnalysisDefinition) -> Self {
        Self {
            block_shape_zyx: value.block_shape().dimensions(),
            dtype: dtype_name(value.dtype()),
            operation: value.operation().contract_name(),
            schema: RECIPE_SCHEMA,
            source_layer: value.layer().ordinal(),
            spatial_origin_zyx: value.spatial_region().origin(),
            spatial_shape_zyx: value.spatial_region().shape().dimensions(),
            time_end_exclusive: value.time_end_exclusive(),
            time_start: value.time_start(),
        }
    }

    fn from_provenance(value: &AnalysisProvenance) -> Self {
        Self {
            block_shape_zyx: value.block_shape.dimensions(),
            dtype: dtype_name(value.dtype),
            operation: value.operation.contract_name(),
            schema: RECIPE_SCHEMA,
            source_layer: value.source_layer.ordinal(),
            spatial_origin_zyx: value.spatial_region.origin(),
            spatial_shape_zyx: value.spatial_region.shape().dimensions(),
            time_end_exclusive: value.time_end_exclusive,
            time_start: value.time_start,
        }
    }
}

#[derive(Debug, Serialize)]
struct DerivationWire {
    recipe_id: String,
    schema: String,
    source_content_id: String,
}

const fn dtype_name(dtype: IntensityDType) -> &'static str {
    match dtype {
        IntensityDType::Uint8 => "uint8",
        IntensityDType::Uint16 => "uint16",
        IntensityDType::Float32 => "float32",
    }
}

fn parse_dtype(value: &str) -> Result<IntensityDType, AnalysisError> {
    match value {
        "uint8" => Ok(IntensityDType::Uint8),
        "uint16" => Ok(IntensityDType::Uint16),
        "float32" => Ok(IntensityDType::Float32),
        _ => Err(AnalysisError::InvalidArtifact),
    }
}

fn parse_operation(value: &str) -> Result<AnalysisOperation, AnalysisError> {
    match value {
        "full-intensity-summary-v1" => Ok(AnalysisOperation::FullIntensitySummary),
        "box-roi-intensity-statistics-v1" => Ok(AnalysisOperation::BoxRoiIntensityStatistics),
        _ => Err(AnalysisError::InvalidArtifact),
    }
}

fn float_bits(value: f64) -> String {
    format!("{:016x}", value.to_bits())
}

fn parse_optional_float(value: Option<String>) -> Result<Option<f64>, AnalysisError> {
    value
        .map(|value| {
            if value.len() != 16 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(AnalysisError::InvalidArtifact);
            }
            let bits =
                u64::from_str_radix(&value, 16).map_err(|_| AnalysisError::InvalidArtifact)?;
            let value = f64::from_bits(bits);
            if !value.is_finite() {
                return Err(AnalysisError::InvalidArtifact);
            }
            Ok(value)
        })
        .transpose()
}
