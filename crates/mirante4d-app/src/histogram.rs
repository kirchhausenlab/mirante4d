use mirante4d_dataset::{DatasetResourceIdentity, DatasetResourceKey, ResourcePayloadView};
use mirante4d_domain::{IntensityDType, LogicalLayerKey, ScaleLevel, TimeIndex};

use crate::retained_leases::RetainedLeases;
use mirante4d_application::{HistogramStatus, LayerHistogramSummary};

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
