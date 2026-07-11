use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use glam::DVec3;
use mirante4d_core::{GridToWorld, Shape3D};
use mirante4d_data::{DenseVolumeF32, DenseVolumeU8, DenseVolumeU16, VolumeRegion};
use serde::{Deserialize, Serialize};

use crate::{
    AnalysisError, IntensitySummary, IntensitySummaryF32, RoiArtifact, WorldGeometry,
    summarize_f32_volume, summarize_u16_volume,
};

type GridRange = (u64, u64);
type GridBoxRanges = (GridRange, GridRange, GridRange);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisExecutionClass {
    ViewLocalInteractive,
    RoiLocalExact,
    FullScopeBatch,
    MultiscaleApproximate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisResultState {
    Preview,
    Approximate,
    Partial,
    Cancelled,
    Failed,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AnalysisCell {
    Text(String),
    Integer(u64),
    Float(f64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisColumn {
    pub key: String,
    pub label: String,
    pub unit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisTableRow {
    pub cells: BTreeMap<String, AnalysisCell>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisProvenance {
    pub source_dataset_id: String,
    pub source_dataset: String,
    pub native_format: String,
    pub native_schema_version: u32,
    pub app_version: String,
    pub created_at_utc: String,
    pub source_layer_id: String,
    pub timepoint_start: u64,
    pub timepoint_end_exclusive: u64,
    pub scale_level: u32,
    pub operation: String,
    pub operation_version: u32,
    pub parameters: BTreeMap<String, String>,
    pub scope: String,
    pub execution_class: AnalysisExecutionClass,
    pub result_state: AnalysisResultState,
    pub data_source: String,
    pub compute_precision: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisTable {
    pub id: String,
    pub name: String,
    pub state: AnalysisResultState,
    pub provenance: AnalysisProvenance,
    pub columns: Vec<AnalysisColumn>,
    pub rows: Vec<AnalysisTableRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisPlotPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisPlotSeries {
    pub name: String,
    pub points: Vec<AnalysisPlotPoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisPlot {
    pub id: String,
    pub name: String,
    pub state: AnalysisResultState,
    pub provenance: AnalysisProvenance,
    pub x_label: String,
    pub y_label: String,
    pub series: Vec<AnalysisPlotSeries>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisTableExportPolicy {
    FailIfExists,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisTableExportMetadata {
    pub export_format: String,
    pub table_id: String,
    pub table_name: String,
    pub state: AnalysisResultState,
    pub row_count: usize,
    pub columns: Vec<AnalysisColumn>,
    pub provenance: AnalysisProvenance,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoiIntensityStatistics {
    pub roi_id: String,
    pub voxel_count: u64,
    pub geometric_voxel_count: u64,
    pub min: u16,
    pub max: u16,
    pub sum: f64,
    pub mean: f64,
    pub standard_deviation: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoiIntensityStatisticsF32 {
    pub roi_id: String,
    pub voxel_count: u64,
    pub geometric_voxel_count: u64,
    pub min: f32,
    pub max: f32,
    pub sum: f64,
    pub mean: f64,
    pub standard_deviation: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IntensitySummaryAccumulator {
    pub voxel_count: u64,
    pub geometric_voxel_count: u64,
    pub nonzero_count: u64,
    pub min: u16,
    pub max: u16,
    pub sum: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IntensitySummaryF32Accumulator {
    pub voxel_count: u64,
    pub geometric_voxel_count: u64,
    pub nonzero_count: u64,
    pub min: f32,
    pub max: f32,
    pub sum: f64,
}

impl AnalysisColumn {
    pub fn new(key: impl Into<String>, label: impl Into<String>, unit: Option<&str>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            unit: unit.map(str::to_owned),
        }
    }
}

impl AnalysisTableRow {
    pub fn new(cells: impl IntoIterator<Item = (impl Into<String>, AnalysisCell)>) -> Self {
        Self {
            cells: cells
                .into_iter()
                .map(|(key, value)| (key.into(), value))
                .collect(),
        }
    }
}

impl AnalysisCell {
    fn as_csv_value(&self) -> String {
        match self {
            Self::Text(value) => value.clone(),
            Self::Integer(value) => value.to_string(),
            Self::Float(value) => format!("{value:.12}"),
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Integer(value) => Some(*value as f64),
            Self::Float(value) => Some(*value),
            Self::Text(_) => None,
        }
    }
}

pub fn intensity_summary_row(timepoint: u64, summary: IntensitySummary) -> AnalysisTableRow {
    AnalysisTableRow::new([
        ("timepoint", AnalysisCell::Integer(timepoint)),
        ("voxel_count", AnalysisCell::Integer(summary.voxel_count)),
        (
            "geometric_voxel_count",
            AnalysisCell::Integer(summary.geometric_voxel_count),
        ),
        (
            "nonzero_count",
            AnalysisCell::Integer(summary.nonzero_count),
        ),
        ("min", AnalysisCell::Integer(u64::from(summary.min))),
        ("max", AnalysisCell::Integer(u64::from(summary.max))),
        ("mean", AnalysisCell::Float(summary.mean)),
    ])
}

pub fn intensity_summary_f32_row(timepoint: u64, summary: IntensitySummaryF32) -> AnalysisTableRow {
    AnalysisTableRow::new([
        ("timepoint", AnalysisCell::Integer(timepoint)),
        ("voxel_count", AnalysisCell::Integer(summary.voxel_count)),
        (
            "geometric_voxel_count",
            AnalysisCell::Integer(summary.geometric_voxel_count),
        ),
        (
            "nonzero_count",
            AnalysisCell::Integer(summary.nonzero_count),
        ),
        ("min", AnalysisCell::Float(f64::from(summary.min))),
        ("max", AnalysisCell::Float(f64::from(summary.max))),
        ("sum", AnalysisCell::Float(summary.sum)),
        ("mean", AnalysisCell::Float(summary.mean)),
    ])
}

pub fn intensity_summary_columns() -> Vec<AnalysisColumn> {
    vec![
        AnalysisColumn::new("timepoint", "timepoint", None),
        AnalysisColumn::new("voxel_count", "voxel count", Some("voxel")),
        AnalysisColumn::new(
            "geometric_voxel_count",
            "geometric voxel count",
            Some("voxel"),
        ),
        AnalysisColumn::new("nonzero_count", "nonzero count", Some("voxel")),
        AnalysisColumn::new("min", "min", Some("uint16")),
        AnalysisColumn::new("max", "max", Some("uint16")),
        AnalysisColumn::new("mean", "mean", Some("uint16")),
    ]
}

impl Default for IntensitySummaryAccumulator {
    fn default() -> Self {
        Self {
            voxel_count: 0,
            geometric_voxel_count: 0,
            nonzero_count: 0,
            min: u16::MAX,
            max: u16::MIN,
            sum: 0.0,
        }
    }
}

impl Default for IntensitySummaryF32Accumulator {
    fn default() -> Self {
        Self {
            voxel_count: 0,
            geometric_voxel_count: 0,
            nonzero_count: 0,
            min: f32::INFINITY,
            max: f32::NEG_INFINITY,
            sum: 0.0,
        }
    }
}

impl IntensitySummaryAccumulator {
    pub fn include_volume(&mut self, volume: &DenseVolumeU16) {
        self.geometric_voxel_count += volume.geometric_voxel_count();
        for (index, &value) in volume.values().iter().enumerate() {
            if volume
                .render_valid_mask()
                .and_then(|mask| mask.get(index))
                .is_some_and(|valid| *valid != 1)
            {
                continue;
            }
            self.voxel_count += 1;
            if value != 0 {
                self.nonzero_count += 1;
            }
            self.min = self.min.min(value);
            self.max = self.max.max(value);
            self.sum += f64::from(value);
        }
    }

    pub fn include_u8_volume(&mut self, volume: &DenseVolumeU8) {
        self.geometric_voxel_count += volume.geometric_voxel_count();
        for (index, &value) in volume.values().iter().enumerate() {
            if volume
                .render_valid_mask()
                .and_then(|mask| mask.get(index))
                .is_some_and(|valid| *valid != 1)
            {
                continue;
            }
            self.voxel_count += 1;
            if value != 0 {
                self.nonzero_count += 1;
            }
            let value = u16::from(value);
            self.min = self.min.min(value);
            self.max = self.max.max(value);
            self.sum += f64::from(value);
        }
    }

    pub fn finish(self) -> IntensitySummary {
        let min = if self.voxel_count == 0 { 0 } else { self.min };
        let max = if self.voxel_count == 0 { 0 } else { self.max };
        IntensitySummary {
            voxel_count: self.voxel_count,
            geometric_voxel_count: self.geometric_voxel_count,
            nonzero_count: self.nonzero_count,
            min,
            max,
            mean: if self.voxel_count == 0 {
                0.0
            } else {
                self.sum / self.voxel_count as f64
            },
        }
    }
}

impl IntensitySummaryF32Accumulator {
    pub fn include_volume(&mut self, volume: &DenseVolumeF32) {
        self.geometric_voxel_count += volume.geometric_voxel_count();
        for (index, &value) in volume.values().iter().enumerate() {
            if volume
                .render_valid_mask()
                .and_then(|mask| mask.get(index))
                .is_some_and(|valid| *valid != 1)
            {
                continue;
            }
            self.voxel_count += 1;
            if value != 0.0 {
                self.nonzero_count += 1;
            }
            self.min = self.min.min(value);
            self.max = self.max.max(value);
            self.sum += f64::from(value);
        }
    }

    pub fn finish(self) -> IntensitySummaryF32 {
        let min = if self.voxel_count == 0 { 0.0 } else { self.min };
        let max = if self.voxel_count == 0 { 0.0 } else { self.max };
        IntensitySummaryF32 {
            voxel_count: self.voxel_count,
            geometric_voxel_count: self.geometric_voxel_count,
            nonzero_count: self.nonzero_count,
            min,
            max,
            sum: self.sum,
            mean: if self.voxel_count == 0 {
                0.0
            } else {
                self.sum / self.voxel_count as f64
            },
        }
    }
}

pub fn intensity_summary_f32_columns() -> Vec<AnalysisColumn> {
    vec![
        AnalysisColumn::new("timepoint", "timepoint", None),
        AnalysisColumn::new("voxel_count", "voxel count", Some("voxel")),
        AnalysisColumn::new(
            "geometric_voxel_count",
            "geometric voxel count",
            Some("voxel"),
        ),
        AnalysisColumn::new("nonzero_count", "nonzero count", Some("voxel")),
        AnalysisColumn::new("min", "min", Some("float32")),
        AnalysisColumn::new("max", "max", Some("float32")),
        AnalysisColumn::new("sum", "sum", Some("float32")),
        AnalysisColumn::new("mean", "mean", Some("float32")),
    ]
}

pub fn roi_intensity_columns() -> Vec<AnalysisColumn> {
    vec![
        AnalysisColumn::new("roi_id", "ROI", None),
        AnalysisColumn::new("timepoint", "timepoint", None),
        AnalysisColumn::new("voxel_count", "voxel count", Some("voxel")),
        AnalysisColumn::new(
            "geometric_voxel_count",
            "geometric voxel count",
            Some("voxel"),
        ),
        AnalysisColumn::new("min", "min", Some("uint16")),
        AnalysisColumn::new("max", "max", Some("uint16")),
        AnalysisColumn::new("sum", "sum", Some("uint16")),
        AnalysisColumn::new("mean", "mean", Some("uint16")),
        AnalysisColumn::new("standard_deviation", "std dev", Some("uint16")),
    ]
}

pub fn roi_intensity_f32_columns() -> Vec<AnalysisColumn> {
    vec![
        AnalysisColumn::new("roi_id", "ROI", None),
        AnalysisColumn::new("timepoint", "timepoint", None),
        AnalysisColumn::new("voxel_count", "voxel count", Some("voxel")),
        AnalysisColumn::new(
            "geometric_voxel_count",
            "geometric voxel count",
            Some("voxel"),
        ),
        AnalysisColumn::new("min", "min", Some("float32")),
        AnalysisColumn::new("max", "max", Some("float32")),
        AnalysisColumn::new("sum", "sum", Some("float32")),
        AnalysisColumn::new("mean", "mean", Some("float32")),
        AnalysisColumn::new("standard_deviation", "std dev", Some("float32")),
    ]
}

pub fn measure_box_roi_u16(
    volume: &DenseVolumeU16,
    roi: &RoiArtifact,
) -> Result<RoiIntensityStatistics, AnalysisError> {
    let Some(region) = box_roi_u16_grid_region(volume, roi)? else {
        return Ok(empty_roi_intensity_statistics(roi));
    };
    let geometric_voxel_count = region.z_size * region.y_size * region.x_size;
    let mut voxel_count = 0u64;
    let mut min_value = u16::MAX;
    let mut max_value = u16::MIN;
    let mut sum = 0.0f64;
    let mut sum_squares = 0.0f64;
    for z in region.z_start..region.z_start + region.z_size {
        for y in region.y_start..region.y_start + region.y_size {
            for x in region.x_start..region.x_start + region.x_size {
                let Some(value) = volume.render_voxel(z, y, x) else {
                    if volume.voxel(z, y, x).is_none() {
                        return Err(AnalysisError::InvalidAnalysisTable(
                            "ROI sample out of bounds",
                        ));
                    }
                    continue;
                };
                voxel_count += 1;
                min_value = min_value.min(value);
                max_value = max_value.max(value);
                let value = f64::from(value);
                sum += value;
                sum_squares += value * value;
            }
        }
    }
    if voxel_count == 0 {
        min_value = 0;
        max_value = 0;
    }
    Ok(RoiIntensityStatistics {
        roi_id: roi.id.as_str().to_owned(),
        voxel_count,
        geometric_voxel_count,
        min: min_value,
        max: max_value,
        sum,
        mean: if voxel_count == 0 {
            0.0
        } else {
            sum / voxel_count as f64
        },
        standard_deviation: population_standard_deviation(voxel_count, sum, sum_squares),
    })
}

pub fn summarize_u16_volume_as_roi(
    volume: &DenseVolumeU16,
    roi_id: &str,
) -> RoiIntensityStatistics {
    let mut voxel_count = 0u64;
    let geometric_voxel_count = volume.geometric_voxel_count();
    let mut min_value = u16::MAX;
    let mut max_value = u16::MIN;
    let mut sum = 0.0f64;
    let mut sum_squares = 0.0f64;
    for (index, &value) in volume.values().iter().enumerate() {
        if volume
            .render_valid_mask()
            .and_then(|mask| mask.get(index))
            .is_some_and(|valid| *valid != 1)
        {
            continue;
        }
        voxel_count += 1;
        min_value = min_value.min(value);
        max_value = max_value.max(value);
        let value = f64::from(value);
        sum += value;
        sum_squares += value * value;
    }
    if voxel_count == 0 {
        min_value = 0;
        max_value = 0;
    }
    RoiIntensityStatistics {
        roi_id: roi_id.to_owned(),
        voxel_count,
        geometric_voxel_count,
        min: min_value,
        max: max_value,
        sum,
        mean: if voxel_count == 0 {
            0.0
        } else {
            sum / voxel_count as f64
        },
        standard_deviation: population_standard_deviation(voxel_count, sum, sum_squares),
    }
}

pub fn summarize_u8_volume_as_roi(volume: &DenseVolumeU8, roi_id: &str) -> RoiIntensityStatistics {
    let mut voxel_count = 0u64;
    let geometric_voxel_count = volume.geometric_voxel_count();
    let mut min_value = u16::MAX;
    let mut max_value = u16::MIN;
    let mut sum = 0.0f64;
    let mut sum_squares = 0.0f64;
    for (index, &value) in volume.values().iter().enumerate() {
        if volume
            .render_valid_mask()
            .and_then(|mask| mask.get(index))
            .is_some_and(|valid| *valid != 1)
        {
            continue;
        }
        voxel_count += 1;
        let value = u16::from(value);
        min_value = min_value.min(value);
        max_value = max_value.max(value);
        let value = f64::from(value);
        sum += value;
        sum_squares += value * value;
    }
    if voxel_count == 0 {
        min_value = 0;
        max_value = 0;
    }
    RoiIntensityStatistics {
        roi_id: roi_id.to_owned(),
        voxel_count,
        geometric_voxel_count,
        min: min_value,
        max: max_value,
        sum,
        mean: if voxel_count == 0 {
            0.0
        } else {
            sum / voxel_count as f64
        },
        standard_deviation: population_standard_deviation(voxel_count, sum, sum_squares),
    }
}

pub fn measure_box_roi_f32(
    volume: &DenseVolumeF32,
    roi: &RoiArtifact,
) -> Result<RoiIntensityStatisticsF32, AnalysisError> {
    let Some(region) = box_roi_f32_grid_region(volume, roi)? else {
        return Ok(empty_roi_intensity_statistics_f32(roi));
    };
    let geometric_voxel_count = region.z_size * region.y_size * region.x_size;
    let mut voxel_count = 0u64;
    let mut min_value = f32::INFINITY;
    let mut max_value = f32::NEG_INFINITY;
    let mut sum = 0.0f64;
    let mut sum_squares = 0.0f64;
    for z in region.z_start..region.z_start + region.z_size {
        for y in region.y_start..region.y_start + region.y_size {
            for x in region.x_start..region.x_start + region.x_size {
                let Some(value) = volume.render_voxel(z, y, x) else {
                    if volume.voxel(z, y, x).is_none() {
                        return Err(AnalysisError::InvalidAnalysisTable(
                            "ROI sample out of bounds",
                        ));
                    }
                    continue;
                };
                voxel_count += 1;
                min_value = min_value.min(value);
                max_value = max_value.max(value);
                let value = f64::from(value);
                sum += value;
                sum_squares += value * value;
            }
        }
    }
    if voxel_count == 0 {
        min_value = 0.0;
        max_value = 0.0;
    }
    Ok(RoiIntensityStatisticsF32 {
        roi_id: roi.id.as_str().to_owned(),
        voxel_count,
        geometric_voxel_count,
        min: min_value,
        max: max_value,
        sum,
        mean: if voxel_count == 0 {
            0.0
        } else {
            sum / voxel_count as f64
        },
        standard_deviation: population_standard_deviation(voxel_count, sum, sum_squares),
    })
}

pub fn summarize_f32_volume_as_roi(
    volume: &DenseVolumeF32,
    roi_id: &str,
) -> RoiIntensityStatisticsF32 {
    let mut voxel_count = 0u64;
    let geometric_voxel_count = volume.geometric_voxel_count();
    let mut min_value = f32::INFINITY;
    let mut max_value = f32::NEG_INFINITY;
    let mut sum = 0.0f64;
    let mut sum_squares = 0.0f64;
    for (index, &value) in volume.values().iter().enumerate() {
        if volume
            .render_valid_mask()
            .and_then(|mask| mask.get(index))
            .is_some_and(|valid| *valid != 1)
        {
            continue;
        }
        voxel_count += 1;
        min_value = min_value.min(value);
        max_value = max_value.max(value);
        let value = f64::from(value);
        sum += value;
        sum_squares += value * value;
    }
    if voxel_count == 0 {
        min_value = 0.0;
        max_value = 0.0;
    }
    RoiIntensityStatisticsF32 {
        roi_id: roi_id.to_owned(),
        voxel_count,
        geometric_voxel_count,
        min: min_value,
        max: max_value,
        sum,
        mean: if voxel_count == 0 {
            0.0
        } else {
            sum / voxel_count as f64
        },
        standard_deviation: population_standard_deviation(voxel_count, sum, sum_squares),
    }
}

pub fn box_roi_grid_region(
    grid_to_world: GridToWorld,
    shape: Shape3D,
    roi: &RoiArtifact,
) -> Result<Option<VolumeRegion>, AnalysisError> {
    let WorldGeometry::Box3D { min, max } = roi.geometry else {
        return Err(AnalysisError::UnsupportedAnalysisGeometry(
            "ROI intensity statistics require a 3D box ROI",
        ));
    };
    let (z_range, y_range, x_range) = grid_ranges_for_world_box(&grid_to_world, shape, min, max)?;
    let z_size = z_range.1.saturating_sub(z_range.0);
    let y_size = y_range.1.saturating_sub(y_range.0);
    let x_size = x_range.1.saturating_sub(x_range.0);
    if z_size == 0 || y_size == 0 || x_size == 0 {
        return Ok(None);
    }
    VolumeRegion::new(z_range.0, y_range.0, x_range.0, z_size, y_size, x_size)
        .map(Some)
        .map_err(|_| AnalysisError::InvalidAnalysisTable("ROI grid region is invalid"))
}

pub fn box_roi_u16_grid_region(
    volume: &DenseVolumeU16,
    roi: &RoiArtifact,
) -> Result<Option<VolumeRegion>, AnalysisError> {
    box_roi_grid_region(volume.grid_to_world, volume.shape, roi)
}

pub fn box_roi_f32_grid_region(
    volume: &DenseVolumeF32,
    roi: &RoiArtifact,
) -> Result<Option<VolumeRegion>, AnalysisError> {
    box_roi_grid_region(volume.grid_to_world, volume.shape, roi)
}

pub fn empty_roi_intensity_statistics(roi: &RoiArtifact) -> RoiIntensityStatistics {
    RoiIntensityStatistics {
        roi_id: roi.id.as_str().to_owned(),
        voxel_count: 0,
        geometric_voxel_count: 0,
        min: 0,
        max: 0,
        sum: 0.0,
        mean: 0.0,
        standard_deviation: 0.0,
    }
}

pub fn empty_roi_intensity_statistics_f32(roi: &RoiArtifact) -> RoiIntensityStatisticsF32 {
    RoiIntensityStatisticsF32 {
        roi_id: roi.id.as_str().to_owned(),
        voxel_count: 0,
        geometric_voxel_count: 0,
        min: 0.0,
        max: 0.0,
        sum: 0.0,
        mean: 0.0,
        standard_deviation: 0.0,
    }
}

pub fn roi_intensity_row(timepoint: u64, statistics: RoiIntensityStatistics) -> AnalysisTableRow {
    AnalysisTableRow::new([
        ("roi_id", AnalysisCell::Text(statistics.roi_id)),
        ("timepoint", AnalysisCell::Integer(timepoint)),
        ("voxel_count", AnalysisCell::Integer(statistics.voxel_count)),
        (
            "geometric_voxel_count",
            AnalysisCell::Integer(statistics.geometric_voxel_count),
        ),
        ("min", AnalysisCell::Integer(u64::from(statistics.min))),
        ("max", AnalysisCell::Integer(u64::from(statistics.max))),
        ("sum", AnalysisCell::Float(statistics.sum)),
        ("mean", AnalysisCell::Float(statistics.mean)),
        (
            "standard_deviation",
            AnalysisCell::Float(statistics.standard_deviation),
        ),
    ])
}

pub fn roi_intensity_f32_row(
    timepoint: u64,
    statistics: RoiIntensityStatisticsF32,
) -> AnalysisTableRow {
    AnalysisTableRow::new([
        ("roi_id", AnalysisCell::Text(statistics.roi_id)),
        ("timepoint", AnalysisCell::Integer(timepoint)),
        ("voxel_count", AnalysisCell::Integer(statistics.voxel_count)),
        (
            "geometric_voxel_count",
            AnalysisCell::Integer(statistics.geometric_voxel_count),
        ),
        ("min", AnalysisCell::Float(f64::from(statistics.min))),
        ("max", AnalysisCell::Float(f64::from(statistics.max))),
        ("sum", AnalysisCell::Float(statistics.sum)),
        ("mean", AnalysisCell::Float(statistics.mean)),
        (
            "standard_deviation",
            AnalysisCell::Float(statistics.standard_deviation),
        ),
    ])
}

pub fn final_intensity_summary_table(
    id: impl Into<String>,
    name: impl Into<String>,
    provenance: AnalysisProvenance,
    rows: Vec<AnalysisTableRow>,
) -> Result<AnalysisTable, AnalysisError> {
    if provenance.result_state != AnalysisResultState::Complete {
        return Err(AnalysisError::InvalidAnalysisTable(
            "complete intensity summary table requires complete provenance",
        ));
    }
    Ok(AnalysisTable {
        id: id.into(),
        name: name.into(),
        state: AnalysisResultState::Complete,
        provenance,
        columns: intensity_summary_columns(),
        rows,
    })
}

pub fn final_intensity_summary_f32_table(
    id: impl Into<String>,
    name: impl Into<String>,
    provenance: AnalysisProvenance,
    rows: Vec<AnalysisTableRow>,
) -> Result<AnalysisTable, AnalysisError> {
    if provenance.result_state != AnalysisResultState::Complete {
        return Err(AnalysisError::InvalidAnalysisTable(
            "complete float32 intensity summary table requires complete provenance",
        ));
    }
    Ok(AnalysisTable {
        id: id.into(),
        name: name.into(),
        state: AnalysisResultState::Complete,
        provenance,
        columns: intensity_summary_f32_columns(),
        rows,
    })
}

pub fn final_roi_intensity_table(
    id: impl Into<String>,
    name: impl Into<String>,
    provenance: AnalysisProvenance,
    rows: Vec<AnalysisTableRow>,
) -> Result<AnalysisTable, AnalysisError> {
    if provenance.result_state != AnalysisResultState::Complete {
        return Err(AnalysisError::InvalidAnalysisTable(
            "complete ROI intensity table requires complete provenance",
        ));
    }
    Ok(AnalysisTable {
        id: id.into(),
        name: name.into(),
        state: AnalysisResultState::Complete,
        provenance,
        columns: roi_intensity_columns(),
        rows,
    })
}

pub fn final_roi_intensity_f32_table(
    id: impl Into<String>,
    name: impl Into<String>,
    provenance: AnalysisProvenance,
    rows: Vec<AnalysisTableRow>,
) -> Result<AnalysisTable, AnalysisError> {
    if provenance.result_state != AnalysisResultState::Complete {
        return Err(AnalysisError::InvalidAnalysisTable(
            "complete float32 ROI intensity table requires complete provenance",
        ));
    }
    Ok(AnalysisTable {
        id: id.into(),
        name: name.into(),
        state: AnalysisResultState::Complete,
        provenance,
        columns: roi_intensity_f32_columns(),
        rows,
    })
}

pub fn time_trace_plot_from_table(
    id: impl Into<String>,
    name: impl Into<String>,
    table: &AnalysisTable,
    x_key: &str,
    y_key: &str,
) -> Result<AnalysisPlot, AnalysisError> {
    let mut points = Vec::with_capacity(table.rows.len());
    for row in &table.rows {
        let x = row.cells.get(x_key).and_then(AnalysisCell::as_f64).ok_or(
            AnalysisError::InvalidAnalysisTable("plot x column is missing or not numeric"),
        )?;
        let y = row.cells.get(y_key).and_then(AnalysisCell::as_f64).ok_or(
            AnalysisError::InvalidAnalysisTable("plot y column is missing or not numeric"),
        )?;
        points.push(AnalysisPlotPoint { x, y });
    }
    Ok(AnalysisPlot {
        id: id.into(),
        name: name.into(),
        state: table.state,
        provenance: table.provenance.clone(),
        x_label: x_key.to_owned(),
        y_label: y_key.to_owned(),
        series: vec![AnalysisPlotSeries {
            name: y_key.to_owned(),
            points,
        }],
    })
}

pub fn export_table_csv(table: &AnalysisTable) -> Result<String, AnalysisError> {
    if table.state != AnalysisResultState::Complete {
        return Err(AnalysisError::InvalidAnalysisTable(
            "only complete analysis tables can be exported as final CSV",
        ));
    }
    let mut lines = Vec::new();
    lines.push(format!("# analysis_state,{}", csv_escape("complete")));
    lines.push(format!(
        "# source_dataset_id,{}",
        csv_escape(&table.provenance.source_dataset_id)
    ));
    lines.push(format!(
        "# source_dataset,{}",
        csv_escape(&table.provenance.source_dataset)
    ));
    lines.push(format!(
        "# source_layer_id,{}",
        csv_escape(&table.provenance.source_layer_id)
    ));
    lines.push(format!(
        "# native_format,{}",
        csv_escape(&table.provenance.native_format)
    ));
    lines.push(format!(
        "# native_schema_version,{}",
        table.provenance.native_schema_version
    ));
    lines.push(format!(
        "# app_version,{}",
        csv_escape(&table.provenance.app_version)
    ));
    lines.push(format!(
        "# created_at_utc,{}",
        csv_escape(&table.provenance.created_at_utc)
    ));
    lines.push(format!(
        "# operation,{}",
        csv_escape(&table.provenance.operation)
    ));
    lines.push(format!(
        "# operation_version,{}",
        table.provenance.operation_version
    ));
    lines.push(format!("# scope,{}", csv_escape(&table.provenance.scope)));
    lines.push(format!(
        "# timepoint_start,{}",
        table.provenance.timepoint_start
    ));
    lines.push(format!(
        "# timepoint_end_exclusive,{}",
        table.provenance.timepoint_end_exclusive
    ));
    lines.push(format!("# scale_level,{}", table.provenance.scale_level));
    lines.push(format!(
        "# execution_class,{}",
        csv_escape(&format!("{:?}", table.provenance.execution_class))
    ));
    lines.push(format!(
        "# data_source,{}",
        csv_escape(&table.provenance.data_source)
    ));
    lines.push(format!(
        "# compute_precision,{}",
        csv_escape(&table.provenance.compute_precision)
    ));
    for (key, value) in &table.provenance.parameters {
        lines.push(format!(
            "# parameter.{},{}",
            csv_escape(key),
            csv_escape(value)
        ));
    }
    lines.push(
        table
            .columns
            .iter()
            .map(|column| csv_escape(&column.key))
            .collect::<Vec<_>>()
            .join(","),
    );
    for row in &table.rows {
        lines.push(
            table
                .columns
                .iter()
                .map(|column| {
                    row.cells
                        .get(&column.key)
                        .map(AnalysisCell::as_csv_value)
                        .unwrap_or_default()
                })
                .map(|value| csv_escape(&value))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    Ok(format!("{}\n", lines.join("\n")))
}

pub fn table_export_metadata(
    table: &AnalysisTable,
) -> Result<AnalysisTableExportMetadata, AnalysisError> {
    if table.state != AnalysisResultState::Complete {
        return Err(AnalysisError::InvalidAnalysisTable(
            "only complete analysis tables can produce final export metadata",
        ));
    }
    Ok(AnalysisTableExportMetadata {
        export_format: "csv+json-metadata".to_owned(),
        table_id: table.id.clone(),
        table_name: table.name.clone(),
        state: table.state,
        row_count: table.rows.len(),
        columns: table.columns.clone(),
        provenance: table.provenance.clone(),
    })
}

pub fn export_table_metadata_json(table: &AnalysisTable) -> Result<String, AnalysisError> {
    let metadata = table_export_metadata(table)?;
    serde_json::to_string_pretty(&metadata)
        .map(|json| format!("{json}\n"))
        .map_err(|err| AnalysisError::AnalysisExportFailed(err.to_string()))
}

pub fn write_table_csv_with_metadata(
    path: &Path,
    table: &AnalysisTable,
    policy: AnalysisTableExportPolicy,
) -> Result<PathBuf, AnalysisError> {
    let csv = export_table_csv(table)?;
    let metadata = export_table_metadata_json(table)?;
    let metadata_path = table_metadata_path(path)?;
    if matches!(policy, AnalysisTableExportPolicy::FailIfExists)
        && (path.exists() || metadata_path.exists())
    {
        return Err(AnalysisError::AnalysisExportExists(
            path.display().to_string(),
        ));
    }
    write_text_atomically(path, &csv, policy)?;
    write_text_atomically(&metadata_path, &metadata, policy)?;
    Ok(metadata_path)
}

pub fn export_plot_svg(plot: &AnalysisPlot) -> Result<String, AnalysisError> {
    if plot.state != AnalysisResultState::Complete {
        return Err(AnalysisError::InvalidAnalysisTable(
            "only complete analysis plots can be exported as final SVG",
        ));
    }
    let finite_points = plot
        .series
        .iter()
        .flat_map(|series| series.points.iter())
        .filter(|point| point.x.is_finite() && point.y.is_finite())
        .collect::<Vec<_>>();
    let (min_x, max_x, min_y, max_y) = if finite_points.is_empty() {
        (0.0, 1.0, 0.0, 1.0)
    } else {
        let mut min_x = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for point in finite_points {
            min_x = min_x.min(point.x);
            max_x = max_x.max(point.x);
            min_y = min_y.min(point.y);
            max_y = max_y.max(point.y);
        }
        expand_bounds(min_x, max_x, min_y, max_y)
    };
    let metadata = serde_json::to_string(&plot.provenance)
        .map_err(|err| AnalysisError::AnalysisExportFailed(err.to_string()))?;
    let mut lines = vec![
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="960" height="540" viewBox="0 0 960 540" role="img">"#.to_owned(),
        format!("<title>{}</title>", xml_escape(&plot.name)),
        format!("<metadata>{}</metadata>", xml_escape(&metadata)),
        r#"<rect x="0" y="0" width="960" height="540" fill="white"/>"#.to_owned(),
        r#"<g transform="translate(72 32)">"#.to_owned(),
        r##"<rect x="0" y="0" width="840" height="440" fill="none" stroke="#1f2937" stroke-width="1"/>"##.to_owned(),
    ];
    for (series_index, series) in plot.series.iter().enumerate() {
        let color = plot_series_color(series_index);
        let points = series
            .points
            .iter()
            .filter(|point| point.x.is_finite() && point.y.is_finite())
            .map(|point| {
                let x = normalize_to_plot(point.x, min_x, max_x, 840.0);
                let y = 440.0 - normalize_to_plot(point.y, min_y, max_y, 440.0);
                format!("{x:.3},{y:.3}")
            })
            .collect::<Vec<_>>();
        if !points.is_empty() {
            lines.push(format!(
                r#"<polyline fill="none" stroke="{color}" stroke-width="2" points="{}"/>"#,
                xml_escape(&points.join(" "))
            ));
        }
    }
    lines.extend([
        "</g>".to_owned(),
        format!(
            r#"<text x="492" y="520" text-anchor="middle" font-family="sans-serif" font-size="14">{}</text>"#,
            xml_escape(&plot.x_label)
        ),
        format!(
            r#"<text x="18" y="252" text-anchor="middle" font-family="sans-serif" font-size="14" transform="rotate(-90 18 252)">{}</text>"#,
            xml_escape(&plot.y_label)
        ),
        "</svg>".to_owned(),
    ]);
    Ok(format!("{}\n", lines.join("\n")))
}

fn population_standard_deviation(voxel_count: u64, sum: f64, sum_squares: f64) -> f64 {
    if voxel_count == 0 {
        return 0.0;
    }
    let count = voxel_count as f64;
    let mean = sum / count;
    ((sum_squares / count) - mean * mean).max(0.0).sqrt()
}

pub fn summarize_volume_for_analysis(volume: &DenseVolumeU16) -> IntensitySummary {
    summarize_u16_volume(volume)
}

pub fn summarize_f32_volume_for_analysis(volume: &DenseVolumeF32) -> IntensitySummaryF32 {
    summarize_f32_volume(volume)
}

fn grid_ranges_for_world_box(
    grid_to_world: &GridToWorld,
    shape: Shape3D,
    min: DVec3,
    max: DVec3,
) -> Result<GridBoxRanges, AnalysisError> {
    let world_to_grid = grid_to_world.inverse()?;
    let world_min = min.min(max);
    let world_max = min.max(max);
    let corners = [
        DVec3::new(world_min.x, world_min.y, world_min.z),
        DVec3::new(world_min.x, world_min.y, world_max.z),
        DVec3::new(world_min.x, world_max.y, world_min.z),
        DVec3::new(world_min.x, world_max.y, world_max.z),
        DVec3::new(world_max.x, world_min.y, world_min.z),
        DVec3::new(world_max.x, world_min.y, world_max.z),
        DVec3::new(world_max.x, world_max.y, world_min.z),
        DVec3::new(world_max.x, world_max.y, world_max.z),
    ];
    let mut grid_min = DVec3::splat(f64::INFINITY);
    let mut grid_max = DVec3::splat(f64::NEG_INFINITY);
    for corner in corners {
        let grid = world_to_grid.transform_point(corner);
        grid_min = grid_min.min(grid);
        grid_max = grid_max.max(grid);
    }
    Ok((
        clamp_grid_range(grid_min.z, grid_max.z, shape.z),
        clamp_grid_range(grid_min.y, grid_max.y, shape.y),
        clamp_grid_range(grid_min.x, grid_max.x, shape.x),
    ))
}

fn clamp_grid_range(min: f64, max: f64, limit: u64) -> (u64, u64) {
    let start = min.floor().max(0.0).min(limit as f64) as u64;
    let end = max.ceil().max(0.0).min(limit as f64) as u64;
    (start, end.max(start))
}

fn csv_escape(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn table_metadata_path(path: &Path) -> Result<PathBuf, AnalysisError> {
    let file_name = path
        .file_name()
        .ok_or_else(|| {
            AnalysisError::AnalysisExportFailed("export path has no file name".to_owned())
        })?
        .to_string_lossy();
    Ok(path.with_file_name(format!("{file_name}.metadata.json")))
}

fn write_text_atomically(
    path: &Path,
    contents: &str,
    policy: AnalysisTableExportPolicy,
) -> Result<(), AnalysisError> {
    if matches!(policy, AnalysisTableExportPolicy::FailIfExists) && path.exists() {
        return Err(AnalysisError::AnalysisExportExists(
            path.display().to_string(),
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        AnalysisError::AnalysisExportFailed("export path has no parent".to_owned())
    })?;
    fs::create_dir_all(parent)
        .map_err(|err| AnalysisError::AnalysisExportFailed(err.to_string()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| {
            AnalysisError::AnalysisExportFailed("export path has no file name".to_owned())
        })?
        .to_string_lossy();
    let tmp_path = path.with_file_name(format!(".{file_name}.tmp"));
    fs::write(&tmp_path, contents)
        .map_err(|err| AnalysisError::AnalysisExportFailed(err.to_string()))?;
    if matches!(policy, AnalysisTableExportPolicy::Replace) && path.exists() {
        fs::remove_file(path)
            .map_err(|err| AnalysisError::AnalysisExportFailed(err.to_string()))?;
    }
    fs::rename(&tmp_path, path).map_err(|err| AnalysisError::AnalysisExportFailed(err.to_string()))
}

fn expand_bounds(min_x: f64, max_x: f64, min_y: f64, max_y: f64) -> (f64, f64, f64, f64) {
    let (mut min_x, mut max_x) = (min_x, max_x);
    let (mut min_y, mut max_y) = (min_y, max_y);
    if min_x == max_x {
        min_x -= 1.0;
        max_x += 1.0;
    }
    if min_y == max_y {
        min_y -= 1.0;
        max_y += 1.0;
    }
    (min_x, max_x, min_y, max_y)
}

fn normalize_to_plot(value: f64, min: f64, max: f64, span: f64) -> f64 {
    if min == max {
        span * 0.5
    } else {
        ((value - min) / (max - min)).clamp(0.0, 1.0) * span
    }
}

fn plot_series_color(index: usize) -> &'static str {
    const COLORS: [&str; 6] = [
        "#2563eb", "#dc2626", "#16a34a", "#9333ea", "#ea580c", "#0891b2",
    ];
    COLORS[index % COLORS.len()]
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SceneArtifactId, SceneArtifactTime};
    use approx::assert_abs_diff_eq;
    use mirante4d_core::{
        DatasetId, GridToWorld, LayerId, Shape3D, Shape4D, TimeIndex, WorldSpace, WorldUnit,
    };
    use mirante4d_data::{DatasetHandle, DenseVolumeF32, DenseVolumeU8, DenseVolumeU16};
    use mirante4d_format::{
        ChannelMetadata, DenseF32Layer, ExistingPackagePolicy, FixtureKind, NativeF32Dataset,
        default_f32_display, write_fixture, write_native_f32_dataset,
    };

    #[test]
    fn roi_box_intensity_measurement_uses_source_volume_values() {
        let volume = basic_volume();
        let roi = roi_box(
            "roi-a",
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(0.4, 0.4, 0.4),
        );

        let statistics = measure_box_roi_u16(&volume, &roi).unwrap();

        assert_eq!(statistics.roi_id, "roi-a");
        assert_eq!(statistics.voxel_count, 8);
        assert_eq!(statistics.min, 0);
        assert_eq!(statistics.max, 275);
        assert_eq!(statistics.mean, 137.5);
        assert_abs_diff_eq!(
            statistics.standard_deviation,
            128.78179219128765,
            epsilon = 1e-12
        );
    }

    #[test]
    fn analysis_summaries_exclude_render_invalid_voxels() {
        let volume = DenseVolumeU16::new(
            DatasetId::new("analysis-masked").unwrap(),
            LayerId::new("ch0").unwrap(),
            0,
            TimeIndex(0),
            Shape3D::new(2, 1, 2).unwrap(),
            GridToWorld::identity(),
            vec![255, 1, 2, 0],
        )
        .unwrap()
        .with_render_valid(vec![0, 1, 1, 0])
        .unwrap();
        let roi = roi_box(
            "whole",
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(2.0, 1.0, 2.0),
        );

        let summary = summarize_volume_for_analysis(&volume);
        let statistics = measure_box_roi_u16(&volume, &roi).unwrap();
        let row = roi_intensity_row(0, statistics.clone());

        assert_eq!(summary.geometric_voxel_count, 4);
        assert_eq!(summary.voxel_count, 2);
        assert_eq!(summary.nonzero_count, 2);
        assert_eq!(summary.min, 1);
        assert_eq!(summary.max, 2);
        assert_eq!(summary.mean, 1.5);

        assert_eq!(statistics.geometric_voxel_count, 4);
        assert_eq!(statistics.voxel_count, 2);
        assert_eq!(statistics.min, 1);
        assert_eq!(statistics.max, 2);
        assert_eq!(statistics.sum, 3.0);
        assert_eq!(statistics.mean, 1.5);
        assert_eq!(
            row.cells.get("geometric_voxel_count"),
            Some(&AnalysisCell::Integer(4))
        );
    }

    #[test]
    fn uint8_analysis_summaries_use_uint8_values_and_render_valid_mask() {
        let volume = DenseVolumeU8::new(
            DatasetId::new("analysis-u8-masked").unwrap(),
            LayerId::new("ch0").unwrap(),
            0,
            TimeIndex(0),
            Shape3D::new(2, 1, 2).unwrap(),
            GridToWorld::identity(),
            vec![255, 1, 2, 0],
        )
        .unwrap()
        .with_render_valid(vec![0, 1, 1, 0])
        .unwrap();

        let mut accumulator = IntensitySummaryAccumulator::default();
        accumulator.include_u8_volume(&volume);
        let summary = accumulator.finish();
        let statistics = summarize_u8_volume_as_roi(&volume, "whole");

        assert_eq!(summary.geometric_voxel_count, 4);
        assert_eq!(summary.voxel_count, 2);
        assert_eq!(summary.nonzero_count, 2);
        assert_eq!(summary.min, 1);
        assert_eq!(summary.max, 2);
        assert_eq!(summary.mean, 1.5);

        assert_eq!(statistics.roi_id, "whole");
        assert_eq!(statistics.geometric_voxel_count, 4);
        assert_eq!(statistics.voxel_count, 2);
        assert_eq!(statistics.min, 1);
        assert_eq!(statistics.max, 2);
        assert_eq!(statistics.sum, 3.0);
        assert_eq!(statistics.mean, 1.5);
        assert_abs_diff_eq!(statistics.standard_deviation, 0.5, epsilon = 1e-12);
    }

    #[test]
    fn box_roi_grid_region_uses_exact_source_volume_bounds() {
        let volume = basic_volume();
        let roi = roi_box(
            "roi-a",
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(0.4, 0.4, 0.4),
        );

        let region = box_roi_u16_grid_region(&volume, &roi).unwrap().unwrap();

        assert_eq!(region.z_start, 0);
        assert_eq!(region.y_start, 0);
        assert_eq!(region.x_start, 0);
        assert_eq!(region.z_size, 2);
        assert_eq!(region.y_size, 2);
        assert_eq!(region.x_size, 2);
    }

    #[test]
    fn box_roi_grid_region_reports_empty_outside_volume_roi() {
        let volume = basic_volume();
        let roi = roi_box(
            "outside",
            DVec3::new(100.0, 100.0, 100.0),
            DVec3::new(101.0, 101.0, 101.0),
        );

        let region = box_roi_u16_grid_region(&volume, &roi).unwrap();
        let statistics = measure_box_roi_u16(&volume, &roi).unwrap();

        assert_eq!(region, None);
        assert_eq!(statistics.roi_id, "outside");
        assert_eq!(statistics.voxel_count, 0);
        assert_eq!(statistics.min, 0);
        assert_eq!(statistics.max, 0);
        assert_eq!(statistics.sum, 0.0);
        assert_eq!(statistics.mean, 0.0);
        assert_eq!(statistics.standard_deviation, 0.0);
    }

    #[test]
    fn float32_intensity_summary_table_preserves_negative_and_fractional_values() {
        let volume = f32_volume();
        let summary = summarize_f32_volume_for_analysis(&volume);
        let row = intensity_summary_f32_row(0, summary);
        let provenance = final_provenance("full_float32_summary");

        let table =
            final_intensity_summary_f32_table("summary-f32", "Summary F32", provenance, vec![row])
                .unwrap();
        let csv = export_table_csv(&table).unwrap();

        assert_eq!(summary.voxel_count, 12);
        assert_eq!(summary.nonzero_count, 11);
        assert_eq!(summary.min, -3.5);
        assert_eq!(summary.max, 9.75);
        assert_abs_diff_eq!(summary.sum, 29.5, epsilon = 1e-12);
        assert_eq!(
            table
                .columns
                .iter()
                .find(|column| column.key == "min")
                .and_then(|column| column.unit.as_deref()),
            Some("float32")
        );
        assert!(csv.contains(
            "timepoint,voxel_count,geometric_voxel_count,nonzero_count,min,max,sum,mean"
        ));
        assert!(csv.contains("-3.500000000000"));
    }

    #[test]
    fn float32_roi_box_measurement_uses_exact_source_values() {
        let volume = f32_volume();
        let roi = roi_box(
            "roi-f32",
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(2.0, 1.0, 2.0),
        );

        let region = box_roi_f32_grid_region(&volume, &roi).unwrap().unwrap();
        let statistics = measure_box_roi_f32(&volume, &roi).unwrap();
        let row = roi_intensity_f32_row(0, statistics.clone());
        let table = final_roi_intensity_f32_table(
            "roi-f32",
            "ROI F32",
            final_provenance("roi_f32"),
            vec![row],
        )
        .unwrap();

        assert_eq!(region.z_start, 0);
        assert_eq!(region.z_size, 2);
        assert_eq!(region.y_start, 0);
        assert_eq!(region.y_size, 1);
        assert_eq!(region.x_start, 0);
        assert_eq!(region.x_size, 2);
        assert_eq!(statistics.roi_id, "roi-f32");
        assert_eq!(statistics.voxel_count, 4);
        assert_eq!(statistics.min, -1.0);
        assert_eq!(statistics.max, 8.0);
        assert_abs_diff_eq!(statistics.sum, 6.75, epsilon = 1e-12);
        assert_abs_diff_eq!(statistics.mean, 1.6875, epsilon = 1e-12);
        assert_abs_diff_eq!(
            statistics.standard_deviation,
            3.663054565523151,
            epsilon = 1e-12
        );
        assert_eq!(
            table
                .columns
                .iter()
                .find(|column| column.key == "mean")
                .and_then(|column| column.unit.as_deref()),
            Some("float32")
        );
    }

    #[test]
    fn float32_box_roi_reports_empty_outside_volume_roi() {
        let volume = f32_volume();
        let roi = roi_box(
            "outside-f32",
            DVec3::new(100.0, 100.0, 100.0),
            DVec3::new(101.0, 101.0, 101.0),
        );

        let region = box_roi_f32_grid_region(&volume, &roi).unwrap();
        let statistics = measure_box_roi_f32(&volume, &roi).unwrap();

        assert_eq!(region, None);
        assert_eq!(statistics.roi_id, "outside-f32");
        assert_eq!(statistics.voxel_count, 0);
        assert_eq!(statistics.min, 0.0);
        assert_eq!(statistics.max, 0.0);
        assert_eq!(statistics.sum, 0.0);
        assert_eq!(statistics.mean, 0.0);
        assert_eq!(statistics.standard_deviation, 0.0);
    }

    #[test]
    fn analysis_states_are_distinct_and_preview_is_not_final_export() {
        let provenance = AnalysisProvenance {
            source_dataset_id: "dataset".to_owned(),
            source_dataset: "dataset".to_owned(),
            native_format: "mirante4d-v1".to_owned(),
            native_schema_version: 1,
            app_version: "0.1.0-test".to_owned(),
            created_at_utc: "test-clock".to_owned(),
            source_layer_id: "ch0".to_owned(),
            timepoint_start: 0,
            timepoint_end_exclusive: 1,
            scale_level: 0,
            operation: "preview".to_owned(),
            operation_version: 1,
            parameters: BTreeMap::new(),
            scope: "view".to_owned(),
            execution_class: AnalysisExecutionClass::ViewLocalInteractive,
            result_state: AnalysisResultState::Preview,
            data_source: "renderer_view".to_owned(),
            compute_precision: "f64".to_owned(),
        };
        let table = AnalysisTable {
            id: "preview".to_owned(),
            name: "Preview".to_owned(),
            state: AnalysisResultState::Preview,
            provenance,
            columns: intensity_summary_columns(),
            rows: Vec::new(),
        };

        let error = export_table_csv(&table).unwrap_err();

        assert!(matches!(error, AnalysisError::InvalidAnalysisTable(_)));
        assert_ne!(AnalysisResultState::Preview, AnalysisResultState::Complete);
        assert_ne!(
            AnalysisResultState::Approximate,
            AnalysisResultState::Partial
        );
    }

    #[test]
    fn final_tables_export_csv_and_feed_plot_series() {
        let provenance = AnalysisProvenance {
            source_dataset_id: "dataset".to_owned(),
            source_dataset: "dataset".to_owned(),
            native_format: "mirante4d-v1".to_owned(),
            native_schema_version: 1,
            app_version: "0.1.0-test".to_owned(),
            created_at_utc: "test-clock".to_owned(),
            source_layer_id: "ch0".to_owned(),
            timepoint_start: 0,
            timepoint_end_exclusive: 2,
            scale_level: 0,
            operation: "full_intensity_summary".to_owned(),
            operation_version: 1,
            parameters: BTreeMap::new(),
            scope: "timepoints 0..2".to_owned(),
            execution_class: AnalysisExecutionClass::FullScopeBatch,
            result_state: AnalysisResultState::Complete,
            data_source: "data_engine_volume_reads".to_owned(),
            compute_precision: "f64".to_owned(),
        };
        let table = final_intensity_summary_table(
            "summary",
            "Summary",
            provenance,
            vec![
                AnalysisTableRow::new([
                    ("timepoint", AnalysisCell::Integer(0)),
                    ("voxel_count", AnalysisCell::Integer(8)),
                    ("geometric_voxel_count", AnalysisCell::Integer(8)),
                    ("nonzero_count", AnalysisCell::Integer(7)),
                    ("min", AnalysisCell::Integer(0)),
                    ("max", AnalysisCell::Integer(7)),
                    ("mean", AnalysisCell::Float(3.5)),
                ]),
                AnalysisTableRow::new([
                    ("timepoint", AnalysisCell::Integer(1)),
                    ("voxel_count", AnalysisCell::Integer(8)),
                    ("geometric_voxel_count", AnalysisCell::Integer(8)),
                    ("nonzero_count", AnalysisCell::Integer(8)),
                    ("min", AnalysisCell::Integer(10)),
                    ("max", AnalysisCell::Integer(17)),
                    ("mean", AnalysisCell::Float(13.5)),
                ]),
            ],
        )
        .unwrap();

        let csv = export_table_csv(&table).unwrap();
        let plot =
            time_trace_plot_from_table("mean-trace", "Mean trace", &table, "timepoint", "mean")
                .unwrap();

        assert!(csv.contains("# analysis_state,complete"));
        assert!(csv.contains("# source_dataset_id,dataset"));
        assert!(csv.contains("# operation_version,1"));
        assert!(
            csv.contains("timepoint,voxel_count,geometric_voxel_count,nonzero_count,min,max,mean")
        );
        assert_eq!(plot.series[0].points.len(), 2);
        assert_eq!(plot.series[0].points[1].y, 13.5);
    }

    #[test]
    fn complete_table_exports_adjacent_metadata_and_protects_overwrite() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("summary.csv");
        let table = final_intensity_summary_table(
            "summary",
            "Summary",
            final_provenance("full_intensity_summary"),
            vec![AnalysisTableRow::new([
                ("timepoint", AnalysisCell::Integer(0)),
                ("voxel_count", AnalysisCell::Integer(8)),
                ("geometric_voxel_count", AnalysisCell::Integer(8)),
                ("nonzero_count", AnalysisCell::Integer(7)),
                ("min", AnalysisCell::Integer(0)),
                ("max", AnalysisCell::Integer(7)),
                ("mean", AnalysisCell::Float(3.5)),
            ])],
        )
        .unwrap();

        let metadata_path =
            write_table_csv_with_metadata(&path, &table, AnalysisTableExportPolicy::FailIfExists)
                .unwrap();
        let blocked =
            write_table_csv_with_metadata(&path, &table, AnalysisTableExportPolicy::FailIfExists)
                .unwrap_err();
        let metadata_json = fs::read_to_string(&metadata_path).unwrap();
        let metadata: AnalysisTableExportMetadata = serde_json::from_str(&metadata_json).unwrap();

        assert!(matches!(blocked, AnalysisError::AnalysisExportExists(_)));
        assert_eq!(metadata.table_id, "summary");
        assert_eq!(metadata.row_count, 1);
        assert_eq!(metadata.provenance.source_dataset_id, "dataset");
        assert!(path.exists());
        assert!(metadata_path.ends_with("summary.csv.metadata.json"));
    }

    #[test]
    fn complete_plot_exports_svg_with_embedded_provenance_metadata() {
        let table = final_intensity_summary_table(
            "summary",
            "Summary",
            final_provenance("full_intensity_summary"),
            vec![
                AnalysisTableRow::new([
                    ("timepoint", AnalysisCell::Integer(0)),
                    ("voxel_count", AnalysisCell::Integer(8)),
                    ("geometric_voxel_count", AnalysisCell::Integer(8)),
                    ("nonzero_count", AnalysisCell::Integer(7)),
                    ("min", AnalysisCell::Integer(0)),
                    ("max", AnalysisCell::Integer(7)),
                    ("mean", AnalysisCell::Float(3.5)),
                ]),
                AnalysisTableRow::new([
                    ("timepoint", AnalysisCell::Integer(1)),
                    ("voxel_count", AnalysisCell::Integer(8)),
                    ("geometric_voxel_count", AnalysisCell::Integer(8)),
                    ("nonzero_count", AnalysisCell::Integer(8)),
                    ("min", AnalysisCell::Integer(10)),
                    ("max", AnalysisCell::Integer(17)),
                    ("mean", AnalysisCell::Float(13.5)),
                ]),
            ],
        )
        .unwrap();
        let plot =
            time_trace_plot_from_table("mean-trace", "Mean trace", &table, "timepoint", "mean")
                .unwrap();

        let svg = export_plot_svg(&plot).unwrap();

        assert!(svg.contains("<svg"));
        assert!(svg.contains("<metadata>"));
        assert!(svg.contains("full_intensity_summary"));
        assert!(svg.contains("<polyline"));
    }

    fn basic_volume() -> DenseVolumeU16 {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
        let dataset = DatasetHandle::open(root).unwrap();
        let layer_id = dataset.first_layer_id().unwrap();
        dataset.read_u16_volume(&layer_id, TimeIndex(0)).unwrap()
    }

    fn f32_volume() -> DenseVolumeF32 {
        let tempdir = tempfile::tempdir().unwrap();
        let root = tempdir.path().join("analysis-f32.m4d");
        write_native_f32_dataset(
            &root,
            NativeF32Dataset {
                id: "analysis-f32".to_owned(),
                name: "Analysis F32".to_owned(),
                world_space: WorldSpace {
                    name: "sample".to_owned(),
                    unit: WorldUnit::Micrometer,
                },
                layers: vec![DenseF32Layer {
                    id: "ch0".to_owned(),
                    name: "Channel 0".to_owned(),
                    channel: ChannelMetadata {
                        index: 0,
                        color_rgba: [1.0, 1.0, 1.0, 1.0],
                    },
                    shape: Shape4D::new(1, 2, 2, 3).unwrap(),
                    brick_shape: Shape4D::new(1, 1, 1, 3).unwrap(),
                    grid_to_world: GridToWorld::scale_um(1.0, 1.0, 1.0),
                    display: default_f32_display(),
                    values_tzyx: vec![
                        -1.0, 0.0, 0.5, 2.0, 4.25, -3.5, 8.0, -0.25, 1.25, 3.0, 5.5, 9.75,
                    ],
                }],
            },
            ExistingPackagePolicy::Fail,
        )
        .unwrap();
        let dataset = DatasetHandle::open(&root).unwrap();
        let layer_id = dataset.first_layer_id().unwrap();
        dataset.read_f32_volume(&layer_id, TimeIndex(0)).unwrap()
    }

    fn final_provenance(operation: &str) -> AnalysisProvenance {
        AnalysisProvenance {
            source_dataset_id: "dataset".to_owned(),
            source_dataset: "dataset".to_owned(),
            native_format: "mirante4d-v1".to_owned(),
            native_schema_version: 1,
            app_version: "0.1.0-test".to_owned(),
            created_at_utc: "test-clock".to_owned(),
            source_layer_id: "ch0".to_owned(),
            timepoint_start: 0,
            timepoint_end_exclusive: 1,
            scale_level: 0,
            operation: operation.to_owned(),
            operation_version: 1,
            parameters: BTreeMap::new(),
            scope: "test".to_owned(),
            execution_class: AnalysisExecutionClass::RoiLocalExact,
            result_state: AnalysisResultState::Complete,
            data_source: "data_engine_volume_reads".to_owned(),
            compute_precision: "source_float32_f64_accumulation".to_owned(),
        }
    }

    fn roi_box(id: &str, min: DVec3, max: DVec3) -> RoiArtifact {
        RoiArtifact::new(
            SceneArtifactId::new("roi", id).unwrap(),
            id,
            WorldGeometry::Box3D { min, max },
            SceneArtifactTime::Timepoint(TimeIndex(0)),
        )
        .unwrap()
    }
}
