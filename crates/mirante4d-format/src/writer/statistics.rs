use super::*;

#[derive(Debug, Clone)]
pub(super) struct U16StatisticsAccumulator {
    min: u16,
    max: u16,
    count: u64,
    exact_histogram: Vec<u64>,
}

#[derive(Debug, Clone)]
pub(super) struct F32StreamingStatisticsAccumulator {
    min: f32,
    max: f32,
    count: u64,
}

impl F32StreamingStatisticsAccumulator {
    pub(super) fn new() -> Self {
        Self {
            min: f32::INFINITY,
            max: f32::NEG_INFINITY,
            count: 0,
        }
    }

    pub(super) fn observe_timepoint(
        &mut self,
        layer_id: &str,
        timepoint: u64,
        shape: Shape4D,
        values: &[f32],
    ) -> Result<(), FormatError> {
        let timepoint_voxels = Shape4D::new(1, shape.z(), shape.y(), shape.x())?.element_count()?;
        let timepoint_offset =
            timepoint
                .checked_mul(timepoint_voxels)
                .ok_or_else(|| FormatError::ZarrStorage {
                    layer_id: layer_id.to_owned(),
                    message: "float32 statistics index overflow".to_owned(),
                })?;
        for (local_index, value) in values.iter().copied().enumerate() {
            if !value.is_finite() {
                let index = usize::try_from(timepoint_offset)
                    .ok()
                    .and_then(|offset| offset.checked_add(local_index))
                    .unwrap_or(local_index);
                return Err(FormatError::InvalidFloatValue {
                    layer_id: layer_id.to_owned(),
                    index,
                    value,
                });
            }
            self.min = self.min.min(value);
            self.max = self.max.max(value);
            self.count += 1;
        }
        Ok(())
    }

    pub(super) fn observe_slab(
        &mut self,
        layer_id: &str,
        timepoint: u64,
        z_start: u64,
        shape: Shape4D,
        values: &[f32],
    ) -> Result<(), FormatError> {
        let timepoint_voxels = Shape4D::new(1, shape.z(), shape.y(), shape.x())?.element_count()?;
        let plane_voxels =
            shape
                .y()
                .checked_mul(shape.x())
                .ok_or_else(|| FormatError::ZarrStorage {
                    layer_id: layer_id.to_owned(),
                    message: "float32 statistics plane voxel count overflow".to_owned(),
                })?;
        let timepoint_offset =
            timepoint
                .checked_mul(timepoint_voxels)
                .ok_or_else(|| FormatError::ZarrStorage {
                    layer_id: layer_id.to_owned(),
                    message: "float32 statistics index overflow".to_owned(),
                })?;
        let slab_offset =
            z_start
                .checked_mul(plane_voxels)
                .ok_or_else(|| FormatError::ZarrStorage {
                    layer_id: layer_id.to_owned(),
                    message: "float32 statistics slab index overflow".to_owned(),
                })?;
        let base_offset =
            timepoint_offset
                .checked_add(slab_offset)
                .ok_or_else(|| FormatError::ZarrStorage {
                    layer_id: layer_id.to_owned(),
                    message: "float32 statistics index overflow".to_owned(),
                })?;
        for (local_index, value) in values.iter().copied().enumerate() {
            if !value.is_finite() {
                let index = usize::try_from(base_offset)
                    .ok()
                    .and_then(|offset| offset.checked_add(local_index))
                    .unwrap_or(local_index);
                return Err(FormatError::InvalidFloatValue {
                    layer_id: layer_id.to_owned(),
                    index,
                    value,
                });
            }
            self.min = self.min.min(value);
            self.max = self.max.max(value);
            self.count += 1;
        }
        Ok(())
    }

    pub(super) fn finish_from_array(
        &self,
        layer_id: &str,
        array: &ZarrArray,
        shape: Shape4D,
        chunk_shape: Shape4D,
    ) -> Result<Statistics, FormatError> {
        if self.count == 0 {
            return Ok(empty_f32_statistics());
        }

        let min = f64::from(self.min);
        let max = f64::from(self.max);
        let mut histogram_bins = vec![0u64; 4096];
        let chunk_grid = shape.chunk_grid(chunk_shape)?;
        for t in 0..chunk_grid.t() {
            for z in 0..chunk_grid.z() {
                let z0 = z * chunk_shape.z();
                let z1 = (z0 + chunk_shape.z()).min(shape.z());
                for y in 0..chunk_grid.y() {
                    let y0 = y * chunk_shape.y();
                    let y1 = (y0 + chunk_shape.y()).min(shape.y());
                    for x in 0..chunk_grid.x() {
                        let x0 = x * chunk_shape.x();
                        let x1 = (x0 + chunk_shape.x()).min(shape.x());
                        let values: Vec<f32> = array
                            .retrieve_array_subset(&[t..t + 1, z0..z1, y0..y1, x0..x1])
                            .map_err(zarr_storage_error)?;
                        observe_f32_histogram_values(
                            layer_id,
                            &values,
                            min,
                            max,
                            &mut histogram_bins,
                        )?;
                    }
                }
            }
        }

        Ok(Statistics {
            min,
            max,
            histogram: Histogram {
                bin_count: 4096,
                range_min: min,
                range_max: max,
                bins: histogram_bins.clone(),
            },
            percentiles: Percentiles {
                p0_1: percentile_from_f32_histogram(&histogram_bins, min, max, self.count, 0.001),
                p1: percentile_from_f32_histogram(&histogram_bins, min, max, self.count, 0.01),
                p50: percentile_from_f32_histogram(&histogram_bins, min, max, self.count, 0.5),
                p99: percentile_from_f32_histogram(&histogram_bins, min, max, self.count, 0.99),
                p99_9: percentile_from_f32_histogram(&histogram_bins, min, max, self.count, 0.999),
            },
        })
    }
}

impl U16StatisticsAccumulator {
    pub(super) fn new() -> Self {
        Self {
            min: u16::MAX,
            max: u16::MIN,
            count: 0,
            exact_histogram: vec![0; usize::from(u16::MAX) + 1],
        }
    }

    pub(super) fn observe(&mut self, values: &[u16]) {
        for &value in values {
            self.min = self.min.min(value);
            self.max = self.max.max(value);
            self.count += 1;
            self.exact_histogram[usize::from(value)] += 1;
        }
    }

    pub(super) fn finish(&self) -> Statistics {
        if self.count == 0 {
            return Statistics {
                min: 0.0,
                max: 0.0,
                histogram: Histogram {
                    bin_count: 256,
                    range_min: f64::from(u16::MIN),
                    range_max: f64::from(u16::MAX),
                    bins: vec![0; 256],
                },
                percentiles: Percentiles {
                    p0_1: 0.0,
                    p1: 0.0,
                    p50: 0.0,
                    p99: 0.0,
                    p99_9: 0.0,
                },
            };
        }

        let mut histogram_bins = vec![0u64; 256];
        for (value, count) in self.exact_histogram.iter().copied().enumerate() {
            histogram_bins[value * 256 / 65_536] += count;
        }

        Statistics {
            min: f64::from(self.min),
            max: f64::from(self.max),
            histogram: Histogram {
                bin_count: 256,
                range_min: f64::from(u16::MIN),
                range_max: f64::from(u16::MAX),
                bins: histogram_bins,
            },
            percentiles: Percentiles {
                p0_1: self.percentile_nearest_rank(0.001),
                p1: self.percentile_nearest_rank(0.01),
                p50: self.percentile_nearest_rank(0.5),
                p99: self.percentile_nearest_rank(0.99),
                p99_9: self.percentile_nearest_rank(0.999),
            },
        }
    }

    pub(super) fn percentile_nearest_rank(&self, quantile: f64) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        let clamped = quantile.clamp(0.0, 1.0);
        let target = (clamped * (self.count - 1) as f64).round() as u64;
        let mut cumulative = 0u64;
        for (value, count) in self.exact_histogram.iter().copied().enumerate() {
            cumulative += count;
            if cumulative > target {
                return value as f64;
            }
        }
        f64::from(u16::MAX)
    }
}

#[derive(Debug, Clone)]
pub(super) struct U8StatisticsAccumulator {
    min: u8,
    max: u8,
    count: u64,
    exact_histogram: [u64; 256],
}

impl U8StatisticsAccumulator {
    pub(super) fn new() -> Self {
        Self {
            min: u8::MAX,
            max: u8::MIN,
            count: 0,
            exact_histogram: [0; 256],
        }
    }

    pub(super) fn observe(&mut self, values: &[u8]) {
        for &value in values {
            self.min = self.min.min(value);
            self.max = self.max.max(value);
            self.count += 1;
            self.exact_histogram[usize::from(value)] += 1;
        }
    }

    pub(super) fn observe_masked(&mut self, values: &[u8], render_valid: &[u8]) {
        for (&value, &valid) in values.iter().zip(render_valid) {
            if valid != 1 {
                continue;
            }
            self.min = self.min.min(value);
            self.max = self.max.max(value);
            self.count += 1;
            self.exact_histogram[usize::from(value)] += 1;
        }
    }

    pub(super) fn finish(&self) -> Statistics {
        if self.count == 0 {
            return Statistics {
                min: 0.0,
                max: 0.0,
                histogram: Histogram {
                    bin_count: 256,
                    range_min: f64::from(u8::MIN),
                    range_max: f64::from(u8::MAX),
                    bins: vec![0; 256],
                },
                percentiles: Percentiles {
                    p0_1: 0.0,
                    p1: 0.0,
                    p50: 0.0,
                    p99: 0.0,
                    p99_9: 0.0,
                },
            };
        }

        Statistics {
            min: f64::from(self.min),
            max: f64::from(self.max),
            histogram: Histogram {
                bin_count: 256,
                range_min: f64::from(u8::MIN),
                range_max: f64::from(u8::MAX),
                bins: self.exact_histogram.to_vec(),
            },
            percentiles: Percentiles {
                p0_1: self.percentile_nearest_rank(0.001),
                p1: self.percentile_nearest_rank(0.01),
                p50: self.percentile_nearest_rank(0.5),
                p99: self.percentile_nearest_rank(0.99),
                p99_9: self.percentile_nearest_rank(0.999),
            },
        }
    }

    pub(super) fn percentile_nearest_rank(&self, quantile: f64) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        let clamped = quantile.clamp(0.0, 1.0);
        let target = (clamped * (self.count - 1) as f64).round() as u64;
        let mut cumulative = 0u64;
        for (value, count) in self.exact_histogram.iter().copied().enumerate() {
            cumulative += count;
            if cumulative > target {
                return value as f64;
            }
        }
        f64::from(u8::MAX)
    }
}

pub(super) fn statistics_for_values(values: &[u16]) -> Statistics {
    let min = values.iter().copied().min().unwrap_or(0);
    let max = values.iter().copied().max().unwrap_or(0);
    let mut histogram_bins = vec![0u64; 256];
    for &value in values {
        histogram_bins[usize::from(value) * 256 / 65_536] += 1;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    Statistics {
        min: f64::from(min),
        max: f64::from(max),
        histogram: Histogram {
            bin_count: 256,
            range_min: f64::from(u16::MIN),
            range_max: f64::from(u16::MAX),
            bins: histogram_bins,
        },
        percentiles: Percentiles {
            p0_1: percentile_nearest_rank(&sorted, 0.001),
            p1: percentile_nearest_rank(&sorted, 0.01),
            p50: percentile_nearest_rank(&sorted, 0.5),
            p99: percentile_nearest_rank(&sorted, 0.99),
            p99_9: percentile_nearest_rank(&sorted, 0.999),
        },
    }
}

pub(super) fn percentile_nearest_rank(sorted_values: &[u16], quantile: f64) -> f64 {
    if sorted_values.is_empty() {
        return 0.0;
    }
    let clamped = quantile.clamp(0.0, 1.0);
    let index = (clamped * (sorted_values.len() - 1) as f64).round() as usize;
    f64::from(sorted_values[index])
}

pub(super) fn statistics_for_f32_values(values: &[f32]) -> Statistics {
    if values.is_empty() {
        return empty_f32_statistics();
    }

    let mut sorted = values.to_vec();
    sorted.sort_by(f32::total_cmp);
    let min = f64::from(sorted[0]);
    let max = f64::from(*sorted.last().expect("values is not empty"));
    let mut histogram_bins = vec![0u64; 4096];
    if min == max {
        histogram_bins[0] = values.len() as u64;
    } else {
        let scale = (histogram_bins.len() - 1) as f64 / (max - min);
        for &value in values {
            let index = ((f64::from(value) - min) * scale).floor() as usize;
            let max_index = histogram_bins.len() - 1;
            histogram_bins[index.min(max_index)] += 1;
        }
    }

    Statistics {
        min,
        max,
        histogram: Histogram {
            bin_count: 4096,
            range_min: min,
            range_max: max,
            bins: histogram_bins,
        },
        percentiles: Percentiles {
            p0_1: percentile_nearest_rank_f32(&sorted, 0.001),
            p1: percentile_nearest_rank_f32(&sorted, 0.01),
            p50: percentile_nearest_rank_f32(&sorted, 0.5),
            p99: percentile_nearest_rank_f32(&sorted, 0.99),
            p99_9: percentile_nearest_rank_f32(&sorted, 0.999),
        },
    }
}

pub(super) fn percentile_nearest_rank_f32(sorted_values: &[f32], quantile: f64) -> f64 {
    if sorted_values.is_empty() {
        return 0.0;
    }
    let clamped = quantile.clamp(0.0, 1.0);
    let index = (clamped * (sorted_values.len() - 1) as f64).round() as usize;
    f64::from(sorted_values[index])
}

pub(super) fn empty_f32_statistics() -> Statistics {
    Statistics {
        min: 0.0,
        max: 0.0,
        histogram: Histogram {
            bin_count: 4096,
            range_min: 0.0,
            range_max: 0.0,
            bins: vec![0; 4096],
        },
        percentiles: Percentiles {
            p0_1: 0.0,
            p1: 0.0,
            p50: 0.0,
            p99: 0.0,
            p99_9: 0.0,
        },
    }
}

pub(super) fn observe_f32_histogram_values(
    layer_id: &str,
    values: &[f32],
    min: f64,
    max: f64,
    histogram_bins: &mut [u64],
) -> Result<(), FormatError> {
    if min == max {
        for (index, value) in values.iter().copied().enumerate() {
            if !value.is_finite() {
                return Err(FormatError::InvalidFloatValue {
                    layer_id: layer_id.to_owned(),
                    index,
                    value,
                });
            }
        }
        histogram_bins[0] += values.len() as u64;
        return Ok(());
    }

    let scale = (histogram_bins.len() - 1) as f64 / (max - min);
    let max_index = histogram_bins.len() - 1;
    for (index, value) in values.iter().copied().enumerate() {
        if !value.is_finite() {
            return Err(FormatError::InvalidFloatValue {
                layer_id: layer_id.to_owned(),
                index,
                value,
            });
        }
        let bin = ((f64::from(value) - min) * scale).floor() as usize;
        histogram_bins[bin.min(max_index)] += 1;
    }
    Ok(())
}

pub(super) fn percentile_from_f32_histogram(
    histogram_bins: &[u64],
    min: f64,
    max: f64,
    count: u64,
    quantile: f64,
) -> f64 {
    if count == 0 || histogram_bins.is_empty() {
        return 0.0;
    }
    if min == max {
        return min;
    }
    let clamped = quantile.clamp(0.0, 1.0);
    let target = (clamped * (count - 1) as f64).round() as u64;
    let mut cumulative = 0u64;
    for (bin, bin_count) in histogram_bins.iter().copied().enumerate() {
        cumulative += bin_count;
        if cumulative > target {
            if bin == 0 {
                return min;
            }
            if bin == histogram_bins.len() - 1 {
                return max;
            }
            let fraction = bin as f64 / (histogram_bins.len() - 1) as f64;
            return min + fraction * (max - min);
        }
    }
    max
}
use crate::CurrentShape4DExt;
