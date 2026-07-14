use mirante4d_dataset::{DatasetResourceIdentity, DatasetResourceKey, ResourcePayloadView};
use mirante4d_domain::{
    DisplayWindow, DvrOpacityTransfer, IntensityDType, LogicalLayerKey, ScaleLevel, TimeIndex,
    TransferCurve,
};

use crate::{HistogramStatus, LayerHistogramSummary, retained_leases::RetainedLeases};

const HISTOGRAM_BIN_COUNT: usize = 32;
const LEASE_HISTOGRAM_SAMPLE_LIMIT: u64 = 65_536;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ActiveLayerHistogramInput<'a> {
    pub(crate) requirements: &'a [DatasetResourceKey],
    pub(crate) identity: DatasetResourceIdentity,
    pub(crate) layer: LogicalLayerKey,
    pub(crate) layer_name: &'a str,
    pub(crate) dtype: IntensityDType,
    pub(crate) timepoint: TimeIndex,
    pub(crate) scale: ScaleLevel,
}

#[derive(Clone, Copy)]
struct LeaseHistogramResource<'a> {
    payload: ResourcePayloadView<'a>,
    start: u64,
    end: u64,
}

pub(crate) fn active_layer_histogram_summary(
    leases: &RetainedLeases,
    input: ActiveLayerHistogramInput<'_>,
) -> LayerHistogramSummary {
    let cohort = leases.resident_subset(
        input.requirements,
        input.identity,
        input.layer,
        input.timepoint,
        input.scale,
    );
    let cohort_status = cohort.status();
    let mut resources = Vec::new();
    let mut total_samples = 0_u64;
    for resource in cohort.resources() {
        let payload = resource.payload();
        if payload.dtype() != input.dtype {
            return unavailable(format!(
                "histogram lease dtype mismatch for {} at s{}",
                input.layer_name,
                input.scale.get()
            ));
        }
        let Some(end) = total_samples.checked_add(payload.sample_count()) else {
            return unavailable(format!(
                "histogram lease sample count overflows for {} at s{}",
                input.layer_name,
                input.scale.get()
            ));
        };
        resources.push(LeaseHistogramResource {
            payload,
            start: total_samples,
            end,
        });
        total_samples = end;
    }

    if resources.is_empty() {
        return pending(format!(
            "histogram leases loading for {} at s{}",
            input.layer_name,
            input.scale.get()
        ));
    }

    let selected_samples = total_samples.min(LEASE_HISTOGRAM_SAMPLE_LIMIT);
    let mut min_value = f32::INFINITY;
    let mut max_value = f32::NEG_INFINITY;
    let valid_samples =
        match visit_selected_valid_values(&resources, total_samples, selected_samples, |value| {
            min_value = min_value.min(value);
            max_value = max_value.max(value);
        }) {
            Ok(count) => count,
            Err(reason) => return unavailable(reason.to_owned()),
        };

    if valid_samples == 0 {
        if cohort_status.missing > 0 {
            return pending(format!(
                "histogram leases loading for {} at s{} ({} missing)",
                input.layer_name,
                input.scale.get(),
                cohort_status.missing
            ));
        }
        return unavailable(format!(
            "no valid intensity samples are available for {} at s{}",
            input.layer_name,
            input.scale.get()
        ));
    }

    let mut bins = vec![0_u64; HISTOGRAM_BIN_COUNT];
    let fill_result =
        visit_selected_valid_values(&resources, total_samples, selected_samples, |value| {
            let index = histogram_bin_index(value, min_value, max_value, HISTOGRAM_BIN_COUNT);
            bins[index] = bins[index].saturating_add(1);
        });
    if let Err(reason) = fill_result {
        return unavailable(reason.to_owned());
    }

    let status = if cohort_status.missing > 0 {
        HistogramStatus::Pending {
            reason: format!(
                "histogram leases loading for {} at s{} ({} missing)",
                input.layer_name,
                input.scale.get(),
                cohort_status.missing
            ),
        }
    } else {
        HistogramStatus::Sampled {
            source: format!(
                "lease s{} {valid_samples}/{total_samples} valid samples from {} resources",
                input.scale.get(),
                resources.len()
            ),
        }
    };
    LayerHistogramSummary {
        status,
        bin_count: HISTOGRAM_BIN_COUNT,
        sample_count: valid_samples,
        min_value,
        max_value,
        bins,
    }
}

fn visit_selected_valid_values(
    resources: &[LeaseHistogramResource<'_>],
    total_samples: u64,
    selected_samples: u64,
    mut visit: impl FnMut(f32),
) -> Result<u64, &'static str> {
    if total_samples == 0 || selected_samples == 0 {
        return Ok(0);
    }
    let mut resource_index = 0_usize;
    let mut valid_samples = 0_u64;
    for ordinal in 0..selected_samples {
        let global_index = u64::try_from(
            (u128::from(ordinal) * u128::from(total_samples)) / u128::from(selected_samples),
        )
        .expect("an evenly selected index is smaller than its u64 sample total");
        while global_index >= resources[resource_index].end {
            resource_index += 1;
        }
        let resource = resources[resource_index];
        let local_index = global_index - resource.start;
        if let Some(value) = valid_payload_sample(resource.payload, local_index)? {
            visit(value);
            valid_samples += 1;
        }
    }
    Ok(valid_samples)
}

fn valid_payload_sample(
    payload: ResourcePayloadView<'_>,
    sample_index: u64,
) -> Result<Option<f32>, &'static str> {
    if !payload
        .sample_is_valid(sample_index)
        .map_err(|_| "histogram lease validity indexing failed")?
    {
        return Ok(None);
    }
    let byte_offset = usize::try_from(
        sample_index
            .checked_mul(u64::from(payload.dtype().bytes_per_sample()))
            .ok_or("histogram lease byte offset overflowed")?,
    )
    .map_err(|_| "histogram lease byte offset is not addressable")?;
    let bytes = payload.value_bytes();
    let value = match payload.dtype() {
        IntensityDType::Uint8 => f32::from(bytes[byte_offset]),
        IntensityDType::Uint16 => f32::from(u16::from_le_bytes(
            bytes[byte_offset..byte_offset + 2]
                .try_into()
                .expect("a validated uint16 payload contains a complete sample"),
        )),
        IntensityDType::Float32 => f32::from_le_bytes(
            bytes[byte_offset..byte_offset + 4]
                .try_into()
                .expect("a validated float32 payload contains a complete sample"),
        ),
    };
    if !value.is_finite() {
        return Err("histogram lease contains a non-finite intensity sample");
    }
    Ok(Some(value))
}

fn histogram_bin_index(value: f32, min_value: f32, max_value: f32, bin_count: usize) -> usize {
    if max_value <= min_value {
        return 0;
    }
    let normalized = ((value - min_value) / (max_value - min_value)).clamp(0.0, 1.0);
    ((normalized * bin_count as f32).floor() as usize).min(bin_count - 1)
}

fn pending(reason: String) -> LayerHistogramSummary {
    empty_histogram(HistogramStatus::Pending { reason })
}

fn unavailable(reason: String) -> LayerHistogramSummary {
    empty_histogram(HistogramStatus::Unavailable { reason })
}

fn empty_histogram(status: HistogramStatus) -> LayerHistogramSummary {
    LayerHistogramSummary {
        status,
        bin_count: HISTOGRAM_BIN_COUNT,
        sample_count: 0,
        min_value: 0.0,
        max_value: 0.0,
        bins: Vec::new(),
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
            Ok(DvrOpacityTransfer::new(
                window,
                TransferCurve::gamma(crate::DEFAULT_DVR_OPACITY_GAMMA)
                    .expect("default DVR opacity gamma is valid"),
            ))
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
