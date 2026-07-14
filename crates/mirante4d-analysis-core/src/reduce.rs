use mirante4d_dataset::{DatasetResourceKey, ResourcePayloadView};
use mirante4d_domain::IntensityDType;

use crate::{AnalysisArtifactSet, AnalysisError, AnalysisPlan, artifact::build_artifacts};

#[derive(Debug, Clone, PartialEq)]
pub struct IntensityStatistics {
    pub(crate) timepoint: u64,
    pub(crate) geometric_sample_count: u64,
    pub(crate) valid_sample_count: u64,
    pub(crate) nonzero_sample_count: u64,
    pub(crate) minimum: Option<f64>,
    pub(crate) maximum: Option<f64>,
    pub(crate) sum: Option<f64>,
    pub(crate) mean: Option<f64>,
    pub(crate) population_variance: Option<f64>,
}

impl IntensityStatistics {
    pub const fn timepoint(&self) -> u64 {
        self.timepoint
    }

    pub const fn geometric_sample_count(&self) -> u64 {
        self.geometric_sample_count
    }

    pub const fn valid_sample_count(&self) -> u64 {
        self.valid_sample_count
    }

    pub const fn nonzero_sample_count(&self) -> u64 {
        self.nonzero_sample_count
    }

    pub const fn minimum(&self) -> Option<f64> {
        self.minimum
    }

    pub const fn maximum(&self) -> Option<f64> {
        self.maximum
    }

    pub const fn sum(&self) -> Option<f64> {
        self.sum
    }

    pub const fn mean(&self) -> Option<f64> {
        self.mean
    }

    pub const fn population_variance(&self) -> Option<f64> {
        self.population_variance
    }
}

#[derive(Debug, Clone)]
enum Moments {
    Integer(IntegerMoments),
    Float(FloatMoments),
}

impl Moments {
    fn for_dtype(dtype: IntensityDType) -> Self {
        match dtype {
            IntensityDType::Uint8 | IntensityDType::Uint16 => {
                Self::Integer(IntegerMoments::default())
            }
            IntensityDType::Float32 => Self::Float(FloatMoments::default()),
        }
    }

    fn include_integer(&mut self, value: u64) -> Result<(), AnalysisError> {
        let Self::Integer(moments) = self else {
            return Err(AnalysisError::PayloadMismatch);
        };
        moments.include(value)
    }

    fn include_float(&mut self, value: f32) -> Result<(), AnalysisError> {
        let Self::Float(moments) = self else {
            return Err(AnalysisError::PayloadMismatch);
        };
        moments.include(value)
    }

    fn finish(self, timepoint: u64, geometric_sample_count: u64) -> IntensityStatistics {
        match self {
            Self::Integer(moments) => moments.finish(timepoint, geometric_sample_count),
            Self::Float(moments) => moments.finish(timepoint, geometric_sample_count),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct IntegerMoments {
    count: u64,
    nonzero: u64,
    minimum: Option<u64>,
    maximum: Option<u64>,
    sum: u128,
    sum_squares: u128,
}

impl IntegerMoments {
    fn include(&mut self, value: u64) -> Result<(), AnalysisError> {
        self.count = self
            .count
            .checked_add(1)
            .ok_or(AnalysisError::AccumulatorOverflow)?;
        self.nonzero = self
            .nonzero
            .checked_add(u64::from(value != 0))
            .ok_or(AnalysisError::AccumulatorOverflow)?;
        self.minimum = Some(self.minimum.map_or(value, |minimum| minimum.min(value)));
        self.maximum = Some(self.maximum.map_or(value, |maximum| maximum.max(value)));
        self.sum = self
            .sum
            .checked_add(u128::from(value))
            .ok_or(AnalysisError::AccumulatorOverflow)?;
        self.sum_squares = self
            .sum_squares
            .checked_add(u128::from(value) * u128::from(value))
            .ok_or(AnalysisError::AccumulatorOverflow)?;
        Ok(())
    }

    fn finish(self, timepoint: u64, geometric_sample_count: u64) -> IntensityStatistics {
        if self.count == 0 {
            return empty_statistics(timepoint, geometric_sample_count);
        }
        let count = self.count as f64;
        let sum = self.sum as f64;
        let mean = sum / count;
        let variance = ((self.sum_squares as f64) / count - mean * mean).max(0.0);
        IntensityStatistics {
            timepoint,
            geometric_sample_count,
            valid_sample_count: self.count,
            nonzero_sample_count: self.nonzero,
            minimum: self.minimum.map(|value| value as f64),
            maximum: self.maximum.map(|value| value as f64),
            sum: Some(sum),
            mean: Some(mean),
            population_variance: Some(variance),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct FloatMoments {
    count: u64,
    nonzero: u64,
    minimum: Option<f32>,
    maximum: Option<f32>,
    sum: f64,
    mean: f64,
    m2: f64,
}

impl FloatMoments {
    fn include(&mut self, value: f32) -> Result<(), AnalysisError> {
        if !value.is_finite() {
            return Err(AnalysisError::NonFiniteFloat);
        }
        self.count = self
            .count
            .checked_add(1)
            .ok_or(AnalysisError::AccumulatorOverflow)?;
        self.nonzero = self
            .nonzero
            .checked_add(u64::from(value != 0.0))
            .ok_or(AnalysisError::AccumulatorOverflow)?;
        self.minimum = Some(self.minimum.map_or(value, |minimum| minimum.min(value)));
        self.maximum = Some(self.maximum.map_or(value, |maximum| maximum.max(value)));
        let value = f64::from(value);
        self.sum += value;
        let delta = value - self.mean;
        self.mean += delta / self.count as f64;
        let delta_after = value - self.mean;
        self.m2 += delta * delta_after;
        Ok(())
    }

    fn finish(self, timepoint: u64, geometric_sample_count: u64) -> IntensityStatistics {
        if self.count == 0 {
            return empty_statistics(timepoint, geometric_sample_count);
        }
        IntensityStatistics {
            timepoint,
            geometric_sample_count,
            valid_sample_count: self.count,
            nonzero_sample_count: self.nonzero,
            minimum: self.minimum.map(f64::from),
            maximum: self.maximum.map(f64::from),
            sum: Some(self.sum),
            mean: Some(self.mean),
            population_variance: Some((self.m2 / self.count as f64).max(0.0)),
        }
    }
}

fn empty_statistics(timepoint: u64, geometric_sample_count: u64) -> IntensityStatistics {
    IntensityStatistics {
        timepoint,
        geometric_sample_count,
        valid_sample_count: 0,
        nonzero_sample_count: 0,
        minimum: None,
        maximum: None,
        sum: None,
        mean: None,
        population_variance: None,
    }
}

#[derive(Debug, Clone)]
pub struct AnalysisAccumulator {
    plan: AnalysisPlan,
    next_ordinal: u64,
    geometric_sample_count: u64,
    moments: Moments,
    rows: Vec<IntensityStatistics>,
}

impl AnalysisAccumulator {
    pub fn new(plan: AnalysisPlan) -> Self {
        let moments = Moments::for_dtype(plan.definition().dtype());
        Self {
            plan,
            next_ordinal: 0,
            geometric_sample_count: 0,
            moments,
            rows: Vec::new(),
        }
    }

    pub const fn plan(&self) -> &AnalysisPlan {
        &self.plan
    }

    pub const fn completed_blocks(&self) -> u64 {
        self.next_ordinal
    }

    pub fn include(
        &mut self,
        resource: DatasetResourceKey,
        payload: ResourcePayloadView<'_>,
    ) -> Result<(), AnalysisError> {
        let expected = self
            .plan
            .block(self.next_ordinal)
            .ok_or(AnalysisError::UnexpectedBlock)?;
        if expected.resource() != resource {
            return Err(AnalysisError::UnexpectedBlock);
        }
        if payload.dtype() != self.plan.definition().dtype()
            || payload.shape() != resource.region().shape()
        {
            return Err(AnalysisError::PayloadMismatch);
        }
        self.geometric_sample_count = self
            .geometric_sample_count
            .checked_add(payload.sample_count())
            .ok_or(AnalysisError::AccumulatorOverflow)?;
        for index in 0..payload.sample_count() {
            if !payload
                .sample_is_valid(index)
                .map_err(|_| AnalysisError::PayloadMismatch)?
            {
                continue;
            }
            let index = usize::try_from(index).map_err(|_| AnalysisError::CapacityExceeded)?;
            match payload.dtype() {
                IntensityDType::Uint8 => {
                    self.moments
                        .include_integer(u64::from(payload.value_bytes()[index]))?;
                }
                IntensityDType::Uint16 => {
                    let offset = index
                        .checked_mul(2)
                        .ok_or(AnalysisError::CapacityExceeded)?;
                    let bytes = payload
                        .value_bytes()
                        .get(offset..offset + 2)
                        .ok_or(AnalysisError::PayloadMismatch)?;
                    self.moments
                        .include_integer(u64::from(u16::from_le_bytes([bytes[0], bytes[1]])))?;
                }
                IntensityDType::Float32 => {
                    let offset = index
                        .checked_mul(4)
                        .ok_or(AnalysisError::CapacityExceeded)?;
                    let bytes = payload
                        .value_bytes()
                        .get(offset..offset + 4)
                        .ok_or(AnalysisError::PayloadMismatch)?;
                    self.moments.include_float(f32::from_le_bytes([
                        bytes[0], bytes[1], bytes[2], bytes[3],
                    ]))?;
                }
            }
        }

        self.next_ordinal += 1;
        if self
            .next_ordinal
            .is_multiple_of(self.plan.blocks_per_timepoint())
        {
            let time_offset = self.next_ordinal / self.plan.blocks_per_timepoint() - 1;
            let timepoint = self.plan.definition().time_start() + time_offset;
            let next = Moments::for_dtype(self.plan.definition().dtype());
            let finished = std::mem::replace(&mut self.moments, next)
                .finish(timepoint, self.geometric_sample_count);
            self.geometric_sample_count = 0;
            self.rows.push(finished);
        }
        Ok(())
    }

    pub fn finish(self) -> Result<AnalysisArtifactSet, AnalysisError> {
        if self.next_ordinal != self.plan.total_blocks() {
            return Err(AnalysisError::Incomplete);
        }
        build_artifacts(self.plan.definition(), self.rows)
    }
}
