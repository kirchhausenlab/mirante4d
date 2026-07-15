use mirante4d_domain::{DisplayWindow, DvrOpacityTransfer, TransferCurve};

pub const DEFAULT_DVR_OPACITY_GAMMA: f32 = 0.25;

#[derive(Debug, Clone, PartialEq)]
pub struct LayerHistogramSummary {
    pub status: HistogramStatus,
    pub bin_count: usize,
    pub sample_count: u64,
    pub min_value: f32,
    pub max_value: f32,
    pub bins: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistogramStatus {
    Exact,
    Sampled { source: String },
    Pending { reason: String },
    Unavailable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistogramAutoError {
    NoSamples,
    NotReady { reason: String },
    NoPositiveSignal,
    InvalidWindow,
}

impl std::fmt::Display for HistogramAutoError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSamples => formatter.write_str("histogram has no samples"),
            Self::NotReady { reason } => {
                write!(formatter, "cannot auto-window histogram: {reason}")
            }
            Self::NoPositiveSignal => formatter.write_str("histogram has no positive signal bins"),
            Self::InvalidWindow => {
                formatter.write_str("histogram produced an invalid display window")
            }
        }
    }
}

impl std::error::Error for HistogramAutoError {}

pub fn histogram_can_auto_window(histogram: &LayerHistogramSummary) -> bool {
    matches!(
        histogram.status,
        HistogramStatus::Exact | HistogramStatus::Sampled { .. }
    ) && histogram.sample_count > 0
}

pub fn auto_dense_window_from_histogram(
    histogram: &LayerHistogramSummary,
) -> Result<DisplayWindow, HistogramAutoError> {
    require_ready(histogram)?;
    let low = histogram_percentile_value(histogram, 0, histogram.bins.len(), 0.001)
        .unwrap_or(histogram.min_value);
    let high = histogram_percentile_value(histogram, 0, histogram.bins.len(), 0.999)
        .unwrap_or(histogram.max_value);
    validated_auto_window(low, high, histogram.min_value, histogram.max_value)
}

pub fn auto_signal_window_from_histogram(
    histogram: &LayerHistogramSummary,
) -> Result<DisplayWindow, HistogramAutoError> {
    require_ready(histogram)?;
    let start =
        histogram_signal_start_bin(histogram).ok_or(HistogramAutoError::NoPositiveSignal)?;
    let low = histogram_bin_lower_bound(histogram, start);
    let high = histogram_percentile_value(histogram, start, histogram.bins.len(), 0.995)
        .unwrap_or(histogram.max_value);
    validated_auto_window(low, high, histogram.min_value, histogram.max_value)
}

pub fn auto_dvr_opacity_transfer_from_histogram(
    histogram: &LayerHistogramSummary,
) -> Result<DvrOpacityTransfer, HistogramAutoError> {
    require_ready(histogram)?;
    let start = histogram_signal_start_bin(histogram).unwrap_or(0);
    let low = histogram_percentile_value(histogram, start, histogram.bins.len(), 0.50)
        .unwrap_or(histogram.min_value);
    let high = histogram_percentile_value(histogram, start, histogram.bins.len(), 0.999)
        .unwrap_or(histogram.max_value);
    let window = validated_auto_window(low, high, histogram.min_value, histogram.max_value)?;
    Ok(DvrOpacityTransfer::new(
        window,
        TransferCurve::gamma(DEFAULT_DVR_OPACITY_GAMMA)
            .expect("default DVR opacity gamma is valid"),
    ))
}

fn require_ready(histogram: &LayerHistogramSummary) -> Result<(), HistogramAutoError> {
    match &histogram.status {
        HistogramStatus::Exact | HistogramStatus::Sampled { .. } => {
            if histogram.sample_count == 0 {
                Err(HistogramAutoError::NoSamples)
            } else {
                Ok(())
            }
        }
        HistogramStatus::Pending { reason } | HistogramStatus::Unavailable { reason } => {
            Err(HistogramAutoError::NotReady {
                reason: reason.clone(),
            })
        }
    }
}

fn validated_auto_window(
    low: f32,
    high: f32,
    fallback_low: f32,
    fallback_high: f32,
) -> Result<DisplayWindow, HistogramAutoError> {
    if high > low {
        return DisplayWindow::new(low, high).map_err(|_| HistogramAutoError::InvalidWindow);
    }
    if fallback_high > fallback_low {
        return DisplayWindow::new(fallback_low, fallback_high)
            .map_err(|_| HistogramAutoError::InvalidWindow);
    }
    DisplayWindow::new(fallback_low, fallback_low + 1.0)
        .map_err(|_| HistogramAutoError::InvalidWindow)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn histogram(
        status: HistogramStatus,
        bins: Vec<u64>,
        min: f32,
        max: f32,
    ) -> LayerHistogramSummary {
        LayerHistogramSummary {
            status,
            bin_count: bins.len(),
            sample_count: bins.iter().sum(),
            min_value: min,
            max_value: max,
            bins,
        }
    }

    #[test]
    fn auto_window_errors_preserve_why_histogram_is_not_usable() {
        let empty = histogram(HistogramStatus::Exact, vec![0, 0], 0.0, 1.0);
        assert_eq!(
            auto_dense_window_from_histogram(&empty),
            Err(HistogramAutoError::NoSamples)
        );

        let pending = histogram(
            HistogramStatus::Pending {
                reason: "source data is loading".to_owned(),
            },
            vec![1],
            0.0,
            1.0,
        );
        assert_eq!(
            auto_dense_window_from_histogram(&pending),
            Err(HistogramAutoError::NotReady {
                reason: "source data is loading".to_owned(),
            })
        );

        let mut no_signal = histogram(HistogramStatus::Exact, vec![0, 0], 0.0, 1.0);
        no_signal.sample_count = 1;
        assert_eq!(
            auto_signal_window_from_histogram(&no_signal),
            Err(HistogramAutoError::NoPositiveSignal)
        );
    }

    #[test]
    fn constant_histogram_gets_a_valid_fallback_window() {
        let histogram = histogram(HistogramStatus::Exact, vec![8], 42.0, 42.0);

        let window = auto_dense_window_from_histogram(&histogram).unwrap();

        assert_eq!(window.low(), 42.0);
        assert_eq!(window.high(), 43.0);
    }

    #[test]
    fn automatic_dvr_opacity_uses_the_shared_default_gamma() {
        let histogram = histogram(HistogramStatus::Exact, vec![2, 6], 0.0, 2.0);

        let transfer = auto_dvr_opacity_transfer_from_histogram(&histogram).unwrap();

        assert_eq!(
            transfer.curve(),
            TransferCurve::gamma(DEFAULT_DVR_OPACITY_GAMMA).unwrap()
        );
    }
}
