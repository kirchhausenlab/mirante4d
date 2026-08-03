//! Independent scalar facts for the shared binary32 page traversal.
//!
//! This module does not inspect or translate WGSL. Inputs are quantized once
//! to the representation consumed by the shader, then evaluated in widened
//! arithmetic so expected boundary ordering is independent of the renderer's
//! implementation sequence.

use thiserror::Error;

const F32_MAGNITUDE_MASK: u32 = 0x7fff_ffff;
const F32_MIN_NORMAL_BITS: u32 = 0x0080_0000;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum TraversalOracleError {
    #[error("the traversal oracle received a non-finite binary32 input")]
    NonFiniteInput,
    #[error("the traversal oracle page bounds or ray interval are invalid")]
    InvalidInterval,
}

/// The portable representation-level classification required by WGSL.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PortableDirectionComponent {
    Stationary,
    Moving(f32),
}

impl PortableDirectionComponent {
    pub const fn value(self) -> f32 {
        match self {
            Self::Stationary => 0.0,
            Self::Moving(value) => value,
        }
    }
}

pub fn classify_portable_direction(
    value: f64,
) -> Result<PortableDirectionComponent, TraversalOracleError> {
    let value = value as f32;
    if !value.is_finite() {
        return Err(TraversalOracleError::NonFiniteInput);
    }
    let magnitude = value.to_bits() & F32_MAGNITUDE_MASK;
    if magnitude < F32_MIN_NORMAL_BITS {
        Ok(PortableDirectionComponent::Stationary)
    } else {
        Ok(PortableDirectionComponent::Moving(value))
    }
}

/// Independent next-boundary fact for one resident page.
///
/// `page_lower`, `page_upper`, and `point` use grid coordinates. The returned
/// value is an absolute ray parameter. A boundary that is not proven positive
/// and strictly nearer than the finite ray exit contributes `ray_exit`
/// without evaluating its potentially overflowing quotient.
pub fn page_exit_distance_reference(
    page_lower: [f64; 3],
    page_upper: [f64; 3],
    point: [f64; 3],
    direction: [f64; 3],
    current_distance: f64,
    ray_exit: f64,
) -> Result<f64, TraversalOracleError> {
    let lower = quantize_vector(page_lower)?;
    let upper = quantize_vector(page_upper)?;
    let point = quantize_vector(point)?;
    let current = quantize_scalar(current_distance)?;
    let exit = quantize_scalar(ray_exit)?;
    if exit < current
        || (0..3).any(|axis| lower[axis] > upper[axis])
        || (0..3).any(|axis| point[axis] < lower[axis] || point[axis] > upper[axis])
    {
        return Err(TraversalOracleError::InvalidInterval);
    }
    let remaining = exit - current;
    if remaining <= 0.0 {
        return Ok(exit);
    }

    let mut result = exit;
    for axis in 0..3 {
        let component = classify_portable_direction(direction[axis])?;
        let PortableDirectionComponent::Moving(component) = component else {
            continue;
        };
        let component = f64::from(component);
        let speed = component.abs();
        let boundary = if component > 0.0 {
            upper[axis] - point[axis]
        } else {
            point[axis] - lower[axis]
        };
        if boundary > 0.0 && boundary < remaining * speed {
            let candidate = current + boundary / speed;
            if candidate > current {
                result = result.min(candidate);
            }
        }
    }
    Ok(result)
}

/// Independent monotone sample-index cutoff for one page segment.
pub fn segment_end_index_reference(
    entry: f64,
    step: f64,
    current_index: u32,
    segment_exit: f64,
    count: u32,
) -> Result<u32, TraversalOracleError> {
    if ![entry, step, segment_exit].into_iter().all(f64::is_finite)
        || step <= 0.0
        || current_index >= count
    {
        return Err(TraversalOracleError::InvalidInterval);
    }
    let threshold = ((segment_exit - entry) / step - 0.5).ceil().max(0.0);
    let threshold = if threshold >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        threshold as u32
    };
    Ok(threshold.max(current_index.saturating_add(1)).min(count))
}

fn quantize_vector(value: [f64; 3]) -> Result<[f64; 3], TraversalOracleError> {
    Ok([
        quantize_scalar(value[0])?,
        quantize_scalar(value[1])?,
        quantize_scalar(value[2])?,
    ])
}

fn quantize_scalar(value: f64) -> Result<f64, TraversalOracleError> {
    let value = value as f32;
    value
        .is_finite()
        .then_some(f64::from(value))
        .ok_or(TraversalOracleError::NonFiniteInput)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_page_traversal_crosses_sub_epsilon_direction_for_reference() {
        for (direction, lower, upper) in [(5.0e-7, -0.5, 0.5), (-5.0e-7, -0.5, 0.5)] {
            let exit = page_exit_distance_reference(
                [lower, -0.5, -0.5],
                [upper, 0.5, 0.5],
                [0.0, 0.0, 0.0],
                [direction, 0.0, 0.0],
                0.0,
                2_000_000.0,
            )
            .unwrap();
            assert!((exit - 1_000_000.0025247573).abs() < 0.01);
        }
    }

    #[test]
    fn volume_page_traversal_zero_direction_stays_in_one_page() {
        let exit = page_exit_distance_reference(
            [-0.5; 3],
            [0.5; 3],
            [0.0; 3],
            [0.0, -0.0, f64::from(f32::from_bits(0x007f_ffff))],
            3.0,
            11.0,
        )
        .unwrap();
        assert_eq!(exit, 11.0);
    }

    #[test]
    fn volume_page_traversal_far_boundary_overflow_clamps_only_to_ray_exit() {
        let exit = page_exit_distance_reference(
            [-0.5; 3],
            [f64::from(f32::MAX), 0.5, 0.5],
            [0.0; 3],
            [f64::from(f32::MIN_POSITIVE), 0.0, 0.0],
            0.0,
            1.0e10,
        )
        .unwrap();
        assert_eq!(exit, 1.0e10);
    }

    #[test]
    fn segment_progress_is_owned_only_by_the_monotone_index_guard() {
        assert_eq!(
            segment_end_index_reference(0.0, 1.0, 7, 7.25, 32).unwrap(),
            8
        );
        assert_eq!(
            segment_end_index_reference(0.0, 1.0, 7, 19.75, 16).unwrap(),
            16
        );
    }
}
