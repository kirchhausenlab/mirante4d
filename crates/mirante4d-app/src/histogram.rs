use anyhow::Context;
use mirante4d_core::{DisplayWindow, IntensityDType, LayerId, Shape3D, TransferCurve};
use mirante4d_data::{SpatialBrickIndex, VolumeBrickF32, VolumeBrickU8, VolumeBrickU16};

use crate::{
    AppState, DvrOpacityTransfer, HistogramStatus, LayerHistogramSummary,
    state::{LayerHistogramCache, LayerHistogramCacheKey, ResidentHistogramSampleKey},
};

const HISTOGRAM_BIN_COUNT: usize = 32;
const RESIDENT_HISTOGRAM_SAMPLE_LIMIT: usize = 1_000_000;
const DATA_ENGINE_HISTOGRAM_BRICK_READ_LIMIT: usize = 16;
const DATA_ENGINE_HISTOGRAM_MAX_DECODE_BYTES: u64 = 16 * 1024 * 1024;

pub(crate) fn active_layer_histogram_summary(state: &mut AppState) -> LayerHistogramSummary {
    match (
        &state.active_volume_u8,
        &state.active_volume,
        &state.active_volume_f32,
    ) {
        (Some(volume), _, _) => histogram_from_u8_values(volume.values(), HISTOGRAM_BIN_COUNT),
        (None, Some(volume), _) => histogram_from_u16_values(volume.values(), HISTOGRAM_BIN_COUNT),
        (None, None, Some(volume)) => {
            histogram_from_f32_values(volume.values(), HISTOGRAM_BIN_COUNT)
        }
        (None, None, None) => cached_resident_histogram_summary_for_active_layer(state)
            .unwrap_or_else(|| cached_data_engine_histogram_summary_for_active_layer(state)),
    }
}

fn histogram_from_u8_values(values: &[u8], bin_count: usize) -> LayerHistogramSummary {
    histogram_from_finite_values_with_status(
        values.iter().map(|value| f32::from(*value)),
        bin_count,
        HistogramStatus::Exact,
    )
}

fn histogram_from_u16_values(values: &[u16], bin_count: usize) -> LayerHistogramSummary {
    histogram_from_finite_values_with_status(
        values.iter().map(|value| f32::from(*value)),
        bin_count,
        HistogramStatus::Exact,
    )
}

fn histogram_from_f32_values(values: &[f32], bin_count: usize) -> LayerHistogramSummary {
    histogram_from_finite_values_with_status(
        values.iter().copied(),
        bin_count,
        HistogramStatus::Exact,
    )
}

fn cached_resident_histogram_summary_for_active_layer(
    state: &mut AppState,
) -> Option<LayerHistogramSummary> {
    let resident_generation = state.resident_histogram_generation;
    let key = LayerHistogramCacheKey {
        layer_id: state.active_layer_id.clone(),
        dtype: state.active_layer_dtype,
        timepoint: state.active_timepoint,
        scale_level: state.brick_stream_scale_level,
        resident_generation: Some(resident_generation),
    };
    if let Some(cache) = &state.active_histogram_cache
        && cache.key == key
    {
        return Some(cache.summary.clone());
    }

    let entries = state
        .resident_histogram_samples
        .iter()
        .filter(|(sample_key, _)| {
            sample_key.layer_id == key.layer_id
                && sample_key.dtype == key.dtype
                && sample_key.timepoint == key.timepoint
                && sample_key.scale_level == key.scale_level
        })
        .map(|(_, sample)| sample)
        .collect::<Vec<_>>();
    if entries.is_empty() {
        if active_layer_resident_brick_count(state) == 0 {
            return None;
        }
        return Some(LayerHistogramSummary {
            status: HistogramStatus::Pending {
                reason: format!(
                    "resident histogram samples pending for {} at s{}",
                    state.active_layer_name, state.brick_stream_scale_level
                ),
            },
            bin_count: HISTOGRAM_BIN_COUNT,
            sample_count: 0,
            min_value: 0.0,
            max_value: 0.0,
            bins: Vec::new(),
        });
    }

    let summary = histogram_from_cached_resident_samples(
        entries.as_slice(),
        HISTOGRAM_BIN_COUNT,
        state.brick_stream_scale_level,
    );
    state.active_histogram_cache = Some(LayerHistogramCache {
        key,
        summary: summary.clone(),
    });
    Some(summary)
}

fn cached_data_engine_histogram_summary_for_active_layer(
    state: &mut AppState,
) -> LayerHistogramSummary {
    let key = LayerHistogramCacheKey {
        layer_id: state.active_layer_id.clone(),
        dtype: state.active_layer_dtype,
        timepoint: state.active_timepoint,
        scale_level: state.brick_stream_scale_level,
        resident_generation: None,
    };
    if let Some(cache) = &state.active_histogram_cache
        && cache.key == key
    {
        return cache.summary.clone();
    }

    let summary = match data_engine_histogram_summary_for_active_layer(state, &key) {
        Ok(summary) => summary,
        Err(err) => LayerHistogramSummary {
            status: HistogramStatus::Unavailable {
                reason: format!(
                    "sampled histogram read failed for {} at s{}: {err}",
                    state.active_layer_name, state.brick_stream_scale_level
                ),
            },
            bin_count: HISTOGRAM_BIN_COUNT,
            sample_count: 0,
            min_value: 0.0,
            max_value: 0.0,
            bins: Vec::new(),
        },
    };
    state.active_histogram_cache = Some(LayerHistogramCache {
        key,
        summary: summary.clone(),
    });
    summary
}

fn data_engine_histogram_summary_for_active_layer(
    state: &AppState,
    key: &LayerHistogramCacheKey,
) -> anyhow::Result<LayerHistogramSummary> {
    let layer_id = LayerId::new(key.layer_id.clone())?;
    let brick_grid = state
        .dataset
        .brick_grid_shape_at_scale(&layer_id, key.scale_level)
        .with_context(|| {
            format!(
                "failed to read brick grid for layer {} at s{}",
                key.layer_id, key.scale_level
            )
        })?;
    let brick_shape = state
        .dataset
        .brick_shape_at_scale(&layer_id, key.scale_level)
        .with_context(|| {
            format!(
                "failed to read brick shape for layer {} at s{}",
                key.layer_id, key.scale_level
            )
        })?;
    let total_bricks = brick_grid.element_count()?;
    let brick_read_limit = data_engine_histogram_brick_read_limit(key.dtype, brick_shape)?;
    let sample_bricks = sampled_spatial_brick_indices(brick_grid, brick_read_limit)?;
    if sample_bricks.is_empty() {
        anyhow::bail!("no brick candidates are available for histogram sampling");
    }

    match key.dtype {
        IntensityDType::Float32 => {
            let mut bricks = Vec::with_capacity(sample_bricks.len());
            for brick_index in &sample_bricks {
                bricks.push(state.dataset.read_f32_brick_at_scale(
                    &layer_id,
                    key.scale_level,
                    key.timepoint,
                    *brick_index,
                )?);
            }
            Ok(histogram_from_data_engine_f32_bricks(
                &bricks,
                HISTOGRAM_BIN_COUNT,
                key.scale_level,
                total_bricks,
            ))
        }
        IntensityDType::Uint8 => {
            let mut bricks = Vec::with_capacity(sample_bricks.len());
            for brick_index in &sample_bricks {
                bricks.push(state.dataset.read_u8_brick_at_scale(
                    &layer_id,
                    key.scale_level,
                    key.timepoint,
                    *brick_index,
                )?);
            }
            Ok(histogram_from_data_engine_u8_bricks(
                &bricks,
                HISTOGRAM_BIN_COUNT,
                key.scale_level,
                total_bricks,
            ))
        }
        IntensityDType::Uint16 => {
            let mut bricks = Vec::with_capacity(sample_bricks.len());
            for brick_index in &sample_bricks {
                bricks.push(state.dataset.read_u16_brick_at_scale(
                    &layer_id,
                    key.scale_level,
                    key.timepoint,
                    *brick_index,
                )?);
            }
            Ok(histogram_from_data_engine_u16_bricks(
                &bricks,
                HISTOGRAM_BIN_COUNT,
                key.scale_level,
                total_bricks,
            ))
        }
    }
}

fn data_engine_histogram_brick_read_limit(
    dtype: IntensityDType,
    brick_shape: Shape3D,
) -> anyhow::Result<usize> {
    let bytes_per_value = match dtype {
        IntensityDType::Uint8 => 1_u64,
        IntensityDType::Uint16 => 2_u64,
        IntensityDType::Float32 => 4_u64,
    };
    let values_per_brick = brick_shape.element_count()?;
    let bytes_per_brick = values_per_brick
        .saturating_mul(bytes_per_value)
        .max(bytes_per_value);
    let budget_limited = (DATA_ENGINE_HISTOGRAM_MAX_DECODE_BYTES / bytes_per_brick).max(1);
    Ok(DATA_ENGINE_HISTOGRAM_BRICK_READ_LIMIT.min(budget_limited as usize))
}

fn sampled_spatial_brick_indices(
    brick_grid: Shape3D,
    limit: usize,
) -> anyhow::Result<Vec<SpatialBrickIndex>> {
    let total = brick_grid.element_count()?;
    if total == 0 || limit == 0 {
        return Ok(Vec::new());
    }
    let sample_count = (limit as u64).min(total);
    let stride = total.div_ceil(sample_count);
    let mut indices = Vec::with_capacity(sample_count as usize);
    for sample_index in 0..sample_count {
        let linear = sample_index.saturating_mul(stride).min(total - 1);
        indices.push(spatial_brick_index_from_linear(brick_grid, linear));
    }
    Ok(indices)
}

fn spatial_brick_index_from_linear(brick_grid: Shape3D, linear: u64) -> SpatialBrickIndex {
    let x = linear % brick_grid.x;
    let yz = linear / brick_grid.x;
    let y = yz % brick_grid.y;
    let z = yz / brick_grid.y;
    SpatialBrickIndex::new(z, y, x)
}

fn active_layer_resident_brick_count(state: &AppState) -> usize {
    match state.active_layer_dtype {
        IntensityDType::Uint8 => active_layer_resident_u8_bricks(state).len(),
        IntensityDType::Uint16 => active_layer_resident_u16_bricks(state).len(),
        IntensityDType::Float32 => active_layer_resident_f32_bricks(state).len(),
    }
}

pub(crate) fn resident_histogram_sample_key_for_u8_brick(
    layer_id: String,
    brick: &VolumeBrickU8,
) -> ResidentHistogramSampleKey {
    ResidentHistogramSampleKey {
        layer_id,
        dtype: IntensityDType::Uint8,
        timepoint: brick.volume.timepoint,
        scale_level: brick.scale_level,
        brick_index: brick.brick_index,
    }
}

pub(crate) fn resident_histogram_sample_key_for_u16_brick(
    layer_id: String,
    brick: &VolumeBrickU16,
) -> ResidentHistogramSampleKey {
    ResidentHistogramSampleKey {
        layer_id,
        dtype: IntensityDType::Uint16,
        timepoint: brick.volume.timepoint,
        scale_level: brick.scale_level,
        brick_index: brick.brick_index,
    }
}

pub(crate) fn resident_histogram_sample_key_for_f32_brick(
    layer_id: String,
    brick: &VolumeBrickF32,
) -> ResidentHistogramSampleKey {
    ResidentHistogramSampleKey {
        layer_id,
        dtype: IntensityDType::Float32,
        timepoint: brick.volume.timepoint,
        scale_level: brick.scale_level,
        brick_index: brick.brick_index,
    }
}

fn active_layer_resident_u8_bricks(state: &AppState) -> &[VolumeBrickU8] {
    state
        .resident_bricks_u8_by_layer
        .get(&state.active_layer_id)
        .filter(|bricks| !bricks.is_empty())
        .map(Vec::as_slice)
        .unwrap_or_else(|| state.resident_bricks_u8.as_slice())
}

fn active_layer_resident_u16_bricks(state: &AppState) -> &[VolumeBrickU16] {
    state
        .resident_bricks_by_layer
        .get(&state.active_layer_id)
        .filter(|bricks| !bricks.is_empty())
        .map(Vec::as_slice)
        .unwrap_or_else(|| state.resident_bricks.as_slice())
}

fn active_layer_resident_f32_bricks(state: &AppState) -> &[VolumeBrickF32] {
    state
        .resident_bricks_f32_by_layer
        .get(&state.active_layer_id)
        .filter(|bricks| !bricks.is_empty())
        .map(Vec::as_slice)
        .unwrap_or_else(|| state.resident_bricks_f32.as_slice())
}

fn histogram_from_cached_resident_samples(
    samples: &[&mirante4d_data::BrickHistogramSample],
    bin_count: usize,
    scale_level: u32,
) -> LayerHistogramSummary {
    let total_values = samples
        .iter()
        .map(|sample| sample.total_values)
        .sum::<u64>();
    let finite_values = samples
        .iter()
        .map(|sample| sample.finite_values)
        .sum::<u64>();
    let min_value = samples
        .iter()
        .map(|sample| sample.min_value)
        .fold(f32::INFINITY, f32::min);
    let max_value = samples
        .iter()
        .map(|sample| sample.max_value)
        .fold(f32::NEG_INFINITY, f32::max);
    let values = downsample_finite_values(
        samples
            .iter()
            .flat_map(|sample| sample.samples.iter().copied()),
        RESIDENT_HISTOGRAM_SAMPLE_LIMIT,
    );
    let source = format!(
        "resident s{scale_level} {}/{finite_values} sampled finite values {total_values} total {}br",
        values.len(),
        samples.len()
    );
    histogram_from_finite_values_with_known_range(
        values,
        bin_count,
        HistogramStatus::Sampled { source },
        min_value,
        max_value,
    )
}

fn histogram_from_data_engine_u8_bricks(
    bricks: &[VolumeBrickU8],
    bin_count: usize,
    scale_level: u32,
    total_bricks: u64,
) -> LayerHistogramSummary {
    let total_values = bricks
        .iter()
        .map(|brick| brick.values().len())
        .sum::<usize>();
    let values = sampled_resident_u8_values(bricks, RESIDENT_HISTOGRAM_SAMPLE_LIMIT);
    histogram_from_data_engine_sampled_values(
        values,
        bin_count,
        scale_level,
        bricks.len(),
        total_bricks,
        total_values,
    )
}

fn histogram_from_data_engine_u16_bricks(
    bricks: &[VolumeBrickU16],
    bin_count: usize,
    scale_level: u32,
    total_bricks: u64,
) -> LayerHistogramSummary {
    let total_values = bricks
        .iter()
        .map(|brick| brick.values().len())
        .sum::<usize>();
    let values = sampled_resident_u16_values(bricks, RESIDENT_HISTOGRAM_SAMPLE_LIMIT);
    histogram_from_data_engine_sampled_values(
        values,
        bin_count,
        scale_level,
        bricks.len(),
        total_bricks,
        total_values,
    )
}

fn histogram_from_data_engine_f32_bricks(
    bricks: &[VolumeBrickF32],
    bin_count: usize,
    scale_level: u32,
    total_bricks: u64,
) -> LayerHistogramSummary {
    let total_values = bricks
        .iter()
        .flat_map(|brick| brick.values())
        .filter(|value| value.is_finite())
        .count();
    let values = sampled_resident_f32_values(bricks, RESIDENT_HISTOGRAM_SAMPLE_LIMIT);
    histogram_from_data_engine_sampled_values(
        values,
        bin_count,
        scale_level,
        bricks.len(),
        total_bricks,
        total_values,
    )
}

fn histogram_from_data_engine_sampled_values(
    values: Vec<f32>,
    bin_count: usize,
    scale_level: u32,
    brick_count: usize,
    total_bricks: u64,
    total_values: usize,
) -> LayerHistogramSummary {
    let sample_count = values.len();
    let source = format!(
        "data engine s{scale_level} {sample_count}/{total_values} read values {brick_count}/{total_bricks}br"
    );
    histogram_from_finite_values_with_status(values, bin_count, HistogramStatus::Sampled { source })
}

fn downsample_finite_values(
    values: impl IntoIterator<Item = f32>,
    sample_limit: usize,
) -> Vec<f32> {
    let finite = values
        .into_iter()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if finite.len() <= sample_limit {
        return finite;
    }
    let stride = histogram_sample_stride(finite.len(), sample_limit);
    finite
        .into_iter()
        .step_by(stride)
        .take(sample_limit)
        .collect()
}

fn sampled_resident_u8_values(bricks: &[VolumeBrickU8], sample_limit: usize) -> Vec<f32> {
    let total_values = bricks
        .iter()
        .map(|brick| brick.values().len())
        .sum::<usize>();
    if total_values == 0 || sample_limit == 0 {
        return Vec::new();
    }
    let stride = histogram_sample_stride(total_values, sample_limit);
    let mut sampled = Vec::with_capacity(total_values.div_ceil(stride).min(sample_limit));
    let mut value_index = 0_usize;
    for brick in bricks {
        for value in brick.values() {
            if value_index.is_multiple_of(stride) && sampled.len() < sample_limit {
                sampled.push(f32::from(*value));
            }
            value_index += 1;
        }
    }
    sampled
}

fn sampled_resident_u16_values(bricks: &[VolumeBrickU16], sample_limit: usize) -> Vec<f32> {
    let total_values = bricks
        .iter()
        .map(|brick| brick.values().len())
        .sum::<usize>();
    if total_values == 0 || sample_limit == 0 {
        return Vec::new();
    }
    let stride = histogram_sample_stride(total_values, sample_limit);
    let mut sampled = Vec::with_capacity(total_values.div_ceil(stride).min(sample_limit));
    let mut value_index = 0_usize;
    for brick in bricks {
        for value in brick.values() {
            if value_index.is_multiple_of(stride) && sampled.len() < sample_limit {
                sampled.push(f32::from(*value));
            }
            value_index += 1;
        }
    }
    sampled
}

fn sampled_resident_f32_values(bricks: &[VolumeBrickF32], sample_limit: usize) -> Vec<f32> {
    let total_finite = bricks
        .iter()
        .flat_map(|brick| brick.values())
        .filter(|value| value.is_finite())
        .count();
    if total_finite == 0 || sample_limit == 0 {
        return Vec::new();
    }
    let stride = histogram_sample_stride(total_finite, sample_limit);
    let mut sampled = Vec::with_capacity(total_finite.div_ceil(stride).min(sample_limit));
    let mut finite_index = 0_usize;
    for brick in bricks {
        for value in brick
            .values()
            .iter()
            .copied()
            .filter(|value| value.is_finite())
        {
            if finite_index.is_multiple_of(stride) && sampled.len() < sample_limit {
                sampled.push(value);
            }
            finite_index += 1;
        }
    }
    sampled
}

fn histogram_sample_stride(total_values: usize, sample_limit: usize) -> usize {
    if sample_limit == 0 {
        return usize::MAX;
    }
    total_values.div_ceil(sample_limit).max(1)
}

fn histogram_from_finite_values_with_status(
    values: impl IntoIterator<Item = f32>,
    bin_count: usize,
    status: HistogramStatus,
) -> LayerHistogramSummary {
    let finite = values
        .into_iter()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if finite.is_empty() || bin_count == 0 {
        return LayerHistogramSummary {
            status: HistogramStatus::Unavailable {
                reason: "no finite intensity values are available".to_owned(),
            },
            bin_count,
            sample_count: 0,
            min_value: 0.0,
            max_value: 0.0,
            bins: Vec::new(),
        };
    }
    let mut min_value = f32::INFINITY;
    let mut max_value = f32::NEG_INFINITY;
    for value in &finite {
        min_value = min_value.min(*value);
        max_value = max_value.max(*value);
    }
    let mut bins = vec![0_u64; bin_count];
    if max_value <= min_value {
        bins[0] = finite.len() as u64;
    } else {
        let width = max_value - min_value;
        for value in &finite {
            let normalized = ((*value - min_value) / width).clamp(0.0, 1.0);
            let index = ((normalized * bin_count as f32).floor() as usize).min(bin_count - 1);
            bins[index] += 1;
        }
    }
    LayerHistogramSummary {
        status,
        bin_count,
        sample_count: finite.len() as u64,
        min_value,
        max_value,
        bins,
    }
}

fn histogram_from_finite_values_with_known_range(
    values: impl IntoIterator<Item = f32>,
    bin_count: usize,
    status: HistogramStatus,
    min_value: f32,
    max_value: f32,
) -> LayerHistogramSummary {
    let finite = values
        .into_iter()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if finite.is_empty() || bin_count == 0 || !min_value.is_finite() || !max_value.is_finite() {
        return LayerHistogramSummary {
            status: HistogramStatus::Unavailable {
                reason: "no finite intensity values are available".to_owned(),
            },
            bin_count,
            sample_count: 0,
            min_value: 0.0,
            max_value: 0.0,
            bins: Vec::new(),
        };
    }
    let mut bins = vec![0_u64; bin_count];
    if max_value <= min_value {
        bins[0] = finite.len() as u64;
    } else {
        let width = max_value - min_value;
        for value in &finite {
            let normalized = ((*value - min_value) / width).clamp(0.0, 1.0);
            let index = ((normalized * bin_count as f32).floor() as usize).min(bin_count - 1);
            bins[index] += 1;
        }
    }
    LayerHistogramSummary {
        status,
        bin_count,
        sample_count: finite.len() as u64,
        min_value,
        max_value,
        bins,
    }
}

pub(crate) fn histogram_can_auto_window(histogram: &LayerHistogramSummary) -> bool {
    matches!(
        histogram.status,
        HistogramStatus::Exact | HistogramStatus::Sampled { .. }
    ) && histogram.sample_count > 0
}

pub(crate) fn auto_dense_window_from_histogram(
    histogram: &LayerHistogramSummary,
) -> anyhow::Result<DisplayWindow> {
    match &histogram.status {
        HistogramStatus::Exact | HistogramStatus::Sampled { .. } => {
            if histogram.sample_count == 0 {
                anyhow::bail!("histogram has no samples");
            }
            let low = histogram_percentile_value(histogram, 0, histogram.bins.len(), 0.001)
                .unwrap_or(histogram.min_value);
            let high = histogram_percentile_value(histogram, 0, histogram.bins.len(), 0.999)
                .unwrap_or(histogram.max_value);
            validated_auto_window(low, high, histogram.min_value, histogram.max_value)
        }
        HistogramStatus::Pending { reason } | HistogramStatus::Unavailable { reason } => {
            anyhow::bail!("cannot auto-window dense-valid histogram: {reason}");
        }
    }
}

pub(crate) fn auto_signal_window_from_histogram(
    histogram: &LayerHistogramSummary,
) -> anyhow::Result<DisplayWindow> {
    match &histogram.status {
        HistogramStatus::Exact | HistogramStatus::Sampled { .. } => {
            if histogram.sample_count == 0 {
                anyhow::bail!("histogram has no samples");
            }
            let start = histogram_signal_start_bin(histogram)
                .ok_or_else(|| anyhow::anyhow!("histogram has no positive signal bins"))?;
            let low = histogram_bin_lower_bound(histogram, start);
            let high = histogram_percentile_value(histogram, start, histogram.bins.len(), 0.995)
                .unwrap_or(histogram.max_value);
            validated_auto_window(low, high, histogram.min_value, histogram.max_value)
        }
        HistogramStatus::Pending { reason } | HistogramStatus::Unavailable { reason } => {
            anyhow::bail!("cannot auto-window signal histogram: {reason}");
        }
    }
}

pub(crate) fn auto_dvr_opacity_transfer_from_histogram(
    histogram: &LayerHistogramSummary,
) -> anyhow::Result<DvrOpacityTransfer> {
    match &histogram.status {
        HistogramStatus::Exact | HistogramStatus::Sampled { .. } => {
            if histogram.sample_count == 0 {
                anyhow::bail!("histogram has no samples");
            }
            let start = histogram_signal_start_bin(histogram).unwrap_or(0);
            let low = histogram_percentile_value(histogram, start, histogram.bins.len(), 0.50)
                .unwrap_or(histogram.min_value);
            let high = histogram_percentile_value(histogram, start, histogram.bins.len(), 0.999)
                .unwrap_or(histogram.max_value);
            let window =
                validated_auto_window(low, high, histogram.min_value, histogram.max_value)?;
            DvrOpacityTransfer::new(
                window,
                TransferCurve::gamma(crate::DEFAULT_DVR_OPACITY_GAMMA)
                    .expect("default DVR opacity gamma is valid"),
            )
        }
        HistogramStatus::Pending { reason } | HistogramStatus::Unavailable { reason } => {
            anyhow::bail!("cannot auto-window DVR opacity histogram: {reason}");
        }
    }
}

fn validated_auto_window(
    low: f32,
    high: f32,
    fallback_low: f32,
    fallback_high: f32,
) -> anyhow::Result<DisplayWindow> {
    if high > low {
        return Ok(DisplayWindow::new(low, high)?);
    }
    if fallback_high > fallback_low {
        return Ok(DisplayWindow::new(fallback_low, fallback_high)?);
    }
    Ok(DisplayWindow::new(fallback_low, fallback_low + 1.0)?)
}

fn histogram_percentile_value(
    histogram: &LayerHistogramSummary,
    start_bin: usize,
    end_bin: usize,
    quantile: f32,
) -> Option<f32> {
    if histogram.bins.is_empty() || start_bin >= end_bin || end_bin > histogram.bins.len() {
        return None;
    }
    let total = histogram.bins[start_bin..end_bin]
        .iter()
        .copied()
        .sum::<u64>();
    if total == 0 {
        return None;
    }
    let rank = ((total.saturating_sub(1)) as f32 * quantile.clamp(0.0, 1.0)).round() as u64;
    let mut cumulative = 0_u64;
    for bin in start_bin..end_bin {
        cumulative = cumulative.saturating_add(histogram.bins[bin]);
        if cumulative > rank {
            return Some(histogram_bin_center(histogram, bin));
        }
    }
    Some(histogram_bin_center(histogram, end_bin - 1))
}

fn histogram_signal_start_bin(histogram: &LayerHistogramSummary) -> Option<usize> {
    let first_nonzero = histogram.bins.iter().position(|count| *count > 0)?;
    let later_nonzero = histogram
        .bins
        .iter()
        .enumerate()
        .skip(first_nonzero + 1)
        .find(|(_, count)| **count > 0)
        .map(|(index, _)| index);
    let first_count = histogram.bins[first_nonzero];
    if let Some(later) = later_nonzero
        && first_count.saturating_mul(4) >= histogram.sample_count
    {
        return Some(later);
    }
    Some(first_nonzero)
}

fn histogram_bin_center(histogram: &LayerHistogramSummary, bin: usize) -> f32 {
    let low = histogram_bin_lower_bound(histogram, bin);
    let high = histogram_bin_upper_bound(histogram, bin);
    (low + high) * 0.5
}

fn histogram_bin_lower_bound(histogram: &LayerHistogramSummary, bin: usize) -> f32 {
    if histogram.bins.is_empty() || histogram.max_value <= histogram.min_value {
        return histogram.min_value;
    }
    let width = (histogram.max_value - histogram.min_value) / histogram.bins.len() as f32;
    histogram.min_value + width * bin.min(histogram.bins.len() - 1) as f32
}

fn histogram_bin_upper_bound(histogram: &LayerHistogramSummary, bin: usize) -> f32 {
    if histogram.bins.is_empty() || histogram.max_value <= histogram.min_value {
        return histogram.max_value.max(histogram.min_value);
    }
    let width = (histogram.max_value - histogram.min_value) / histogram.bins.len() as f32;
    if bin + 1 >= histogram.bins.len() {
        histogram.max_value
    } else {
        histogram.min_value + width * (bin + 1) as f32
    }
}

pub(crate) fn histogram_status_label(histogram: &LayerHistogramSummary) -> String {
    match &histogram.status {
        HistogramStatus::Exact => format!(
            "exact {}, {:.3}..{:.3}",
            histogram.sample_count, histogram.min_value, histogram.max_value
        ),
        HistogramStatus::Sampled { source } => format!(
            "sampled {}, {:.3}..{:.3} ({source})",
            histogram.sample_count, histogram.min_value, histogram.max_value
        ),
        HistogramStatus::Pending { reason } => format!("pending: {reason}"),
        HistogramStatus::Unavailable { reason } => format!("unavailable: {reason}"),
    }
}

pub(crate) fn histogram_bins_label(histogram: &LayerHistogramSummary) -> String {
    if histogram.bins.is_empty() {
        return "no bins".to_owned();
    }
    let max_bin = histogram.bins.iter().copied().max().unwrap_or(0).max(1);
    format!("{} bins, peak count {}", histogram.bin_count, max_bin)
}
