use super::*;

pub(super) fn validate_layer_scales(
    layer_id: &str,
    layer_shape: Shape4D,
    layer_grid_to_world: GridToWorld,
    scales: &[DenseU16Scale],
) -> Result<(), FormatError> {
    if scales.is_empty() {
        return Err(FormatError::InvalidScaleCount {
            layer_id: layer_id.to_owned(),
        });
    }
    for (expected_level, scale) in scales.iter().enumerate() {
        if scale.level != expected_level as u32 {
            return Err(FormatError::InvalidScaleLevel {
                layer_id: layer_id.to_owned(),
                level: scale.level,
            });
        }
        if scale.level == 0
            && (scale.shape != layer_shape
                || scale.grid_to_world != layer_grid_to_world
                || scale.source_scale.is_some()
                || scale.reduction != ScaleReduction::Source)
        {
            return Err(FormatError::ScaleShapeMismatch {
                layer_id: layer_id.to_owned(),
            });
        }
        if scale.level > 0
            && (scale.shape.t != layer_shape.t
                || scale.source_scale != Some(scale.level - 1)
                || scale.reduction == ScaleReduction::Source)
        {
            return Err(FormatError::ScaleShapeMismatch {
                layer_id: layer_id.to_owned(),
            });
        }
        if scale.level > 0 {
            let previous = &scales[expected_level - 1];
            validate_downsampled_scale_registration(
                layer_id,
                scale.level,
                previous.shape,
                previous.grid_to_world,
                scale.shape,
                scale.grid_to_world,
            )?;
        }
        scale
            .grid_to_world
            .inverse()
            .map_err(|source| FormatError::InvalidTransform {
                layer_id: layer_id.to_owned(),
                source,
            })?;
    }
    Ok(())
}

pub(super) fn validate_streaming_layer_scales(
    layer_id: &str,
    layer_shape: Shape4D,
    layer_grid_to_world: GridToWorld,
    scales: &[StreamingU16ScaleSpec],
) -> Result<(), FormatError> {
    if scales.is_empty() {
        return Err(FormatError::InvalidScaleCount {
            layer_id: layer_id.to_owned(),
        });
    }
    for (expected_level, scale) in scales.iter().enumerate() {
        if scale.level != expected_level as u32 {
            return Err(FormatError::InvalidScaleLevel {
                layer_id: layer_id.to_owned(),
                level: scale.level,
            });
        }
        if scale.level == 0
            && (scale.shape != layer_shape
                || scale.grid_to_world != layer_grid_to_world
                || scale.source_scale.is_some()
                || scale.reduction != ScaleReduction::Source)
        {
            return Err(FormatError::ScaleShapeMismatch {
                layer_id: layer_id.to_owned(),
            });
        }
        if scale.level > 0
            && (scale.shape.t != layer_shape.t
                || scale.source_scale != Some(scale.level - 1)
                || scale.reduction == ScaleReduction::Source)
        {
            return Err(FormatError::ScaleShapeMismatch {
                layer_id: layer_id.to_owned(),
            });
        }
        if scale.level > 0 {
            let previous = &scales[expected_level - 1];
            validate_downsampled_scale_registration(
                layer_id,
                scale.level,
                previous.shape,
                previous.grid_to_world,
                scale.shape,
                scale.grid_to_world,
            )?;
        }
        scale
            .grid_to_world
            .inverse()
            .map_err(|source| FormatError::InvalidTransform {
                layer_id: layer_id.to_owned(),
                source,
            })?;
        scale.brick_shape.validate()?;
        scale.shape.chunk_grid(scale.brick_shape)?;
    }
    Ok(())
}

pub(super) fn validate_streaming_u8_layer_scales(
    layer_id: &str,
    layer_shape: Shape4D,
    layer_grid_to_world: GridToWorld,
    scales: &[StreamingU8ScaleSpec],
) -> Result<(), FormatError> {
    if scales.is_empty() {
        return Err(FormatError::InvalidScaleCount {
            layer_id: layer_id.to_owned(),
        });
    }
    for (expected_level, scale) in scales.iter().enumerate() {
        if scale.level != expected_level as u32 {
            return Err(FormatError::InvalidScaleLevel {
                layer_id: layer_id.to_owned(),
                level: scale.level,
            });
        }
        if scale.level == 0
            && (scale.shape != layer_shape
                || scale.grid_to_world != layer_grid_to_world
                || scale.source_scale.is_some()
                || scale.reduction != ScaleReduction::Source)
        {
            return Err(FormatError::ScaleShapeMismatch {
                layer_id: layer_id.to_owned(),
            });
        }
        if scale.level > 0
            && (scale.shape.t != layer_shape.t
                || scale.source_scale != Some(scale.level - 1)
                || scale.reduction == ScaleReduction::Source)
        {
            return Err(FormatError::ScaleShapeMismatch {
                layer_id: layer_id.to_owned(),
            });
        }
        if scale.level > 0 {
            let previous = &scales[expected_level - 1];
            validate_downsampled_scale_registration(
                layer_id,
                scale.level,
                previous.shape,
                previous.grid_to_world,
                scale.shape,
                scale.grid_to_world,
            )?;
        }
        scale
            .grid_to_world
            .inverse()
            .map_err(|source| FormatError::InvalidTransform {
                layer_id: layer_id.to_owned(),
                source,
            })?;
        scale.brick_shape.validate()?;
        scale.shape.chunk_grid(scale.brick_shape)?;
    }
    Ok(())
}

pub(super) fn validate_streaming_f32_layer_scales(
    layer_id: &str,
    layer_shape: Shape4D,
    layer_grid_to_world: GridToWorld,
    scales: &[StreamingF32ScaleSpec],
) -> Result<(), FormatError> {
    if scales.is_empty() {
        return Err(FormatError::InvalidScaleCount {
            layer_id: layer_id.to_owned(),
        });
    }
    for (expected_level, scale) in scales.iter().enumerate() {
        if scale.level != expected_level as u32 {
            return Err(FormatError::InvalidScaleLevel {
                layer_id: layer_id.to_owned(),
                level: scale.level,
            });
        }
        if scale.level == 0
            && (scale.shape != layer_shape
                || scale.grid_to_world != layer_grid_to_world
                || scale.source_scale.is_some()
                || scale.reduction != ScaleReduction::Source)
        {
            return Err(FormatError::ScaleShapeMismatch {
                layer_id: layer_id.to_owned(),
            });
        }
        if scale.level > 0
            && (scale.shape.t != layer_shape.t
                || scale.source_scale != Some(scale.level - 1)
                || scale.reduction == ScaleReduction::Source)
        {
            return Err(FormatError::ScaleShapeMismatch {
                layer_id: layer_id.to_owned(),
            });
        }
        if scale.level > 0 {
            let previous = &scales[expected_level - 1];
            validate_downsampled_scale_registration(
                layer_id,
                scale.level,
                previous.shape,
                previous.grid_to_world,
                scale.shape,
                scale.grid_to_world,
            )?;
        }
        scale
            .grid_to_world
            .inverse()
            .map_err(|source| FormatError::InvalidTransform {
                layer_id: layer_id.to_owned(),
                source,
            })?;
        scale.brick_shape.validate()?;
        scale.shape.chunk_grid(scale.brick_shape)?;
    }
    Ok(())
}

pub(super) fn validate_f32_layer_scales(
    layer_id: &str,
    layer_shape: Shape4D,
    layer_grid_to_world: GridToWorld,
    scales: &[DenseF32Scale],
) -> Result<(), FormatError> {
    if scales.is_empty() {
        return Err(FormatError::InvalidScaleCount {
            layer_id: layer_id.to_owned(),
        });
    }
    for (expected_level, scale) in scales.iter().enumerate() {
        if scale.level != expected_level as u32 {
            return Err(FormatError::InvalidScaleLevel {
                layer_id: layer_id.to_owned(),
                level: scale.level,
            });
        }
        if scale.level == 0
            && (scale.shape != layer_shape
                || scale.grid_to_world != layer_grid_to_world
                || scale.source_scale.is_some()
                || scale.reduction != ScaleReduction::Source)
        {
            return Err(FormatError::ScaleShapeMismatch {
                layer_id: layer_id.to_owned(),
            });
        }
        if scale.level > 0
            && (scale.shape.t != layer_shape.t
                || scale.source_scale != Some(scale.level - 1)
                || scale.reduction == ScaleReduction::Source)
        {
            return Err(FormatError::ScaleShapeMismatch {
                layer_id: layer_id.to_owned(),
            });
        }
        if scale.level > 0 {
            let previous = &scales[expected_level - 1];
            validate_downsampled_scale_registration(
                layer_id,
                scale.level,
                previous.shape,
                previous.grid_to_world,
                scale.shape,
                scale.grid_to_world,
            )?;
        }
        scale
            .grid_to_world
            .inverse()
            .map_err(|source| FormatError::InvalidTransform {
                layer_id: layer_id.to_owned(),
                source,
            })?;
    }
    Ok(())
}

pub(super) fn validate_downsampled_scale_registration(
    layer_id: &str,
    level: u32,
    previous_shape: Shape4D,
    previous_grid_to_world: GridToWorld,
    shape: Shape4D,
    grid_to_world: GridToWorld,
) -> Result<(), FormatError> {
    let Some(factors) = infer_downsample_factors(previous_shape, shape) else {
        return Err(FormatError::ScaleTransformMismatch {
            layer_id: layer_id.to_owned(),
            level,
        });
    };
    let expected =
        expected_downsampled_grid_to_world(previous_grid_to_world, factors).map_err(|source| {
            FormatError::InvalidTransform {
                layer_id: layer_id.to_owned(),
                source,
            }
        })?;
    if !grid_to_world_approx_eq(grid_to_world, expected, GRID_TO_WORLD_EPSILON) {
        return Err(FormatError::ScaleTransformMismatch {
            layer_id: layer_id.to_owned(),
            level,
        });
    }
    Ok(())
}

pub(super) fn validate_scale_values(
    layer_id: &str,
    scale: &DenseU16Scale,
) -> Result<(), FormatError> {
    let expected = scale.shape.element_count()? as usize;
    let actual = scale.values_tzyx.len();
    if actual != expected {
        return Err(FormatError::InvalidLayerValues {
            layer_id: layer_id.to_owned(),
            actual,
            expected,
        });
    }
    Ok(())
}

pub(super) fn validate_f32_scale_values(
    layer_id: &str,
    scale: &DenseF32Scale,
) -> Result<(), FormatError> {
    let expected = scale.shape.element_count()? as usize;
    let actual = scale.values_tzyx.len();
    if actual != expected {
        return Err(FormatError::InvalidLayerValues {
            layer_id: layer_id.to_owned(),
            actual,
            expected,
        });
    }
    for (index, value) in scale.values_tzyx.iter().copied().enumerate() {
        if !value.is_finite() {
            return Err(FormatError::InvalidFloatValue {
                layer_id: layer_id.to_owned(),
                index,
                value,
            });
        }
    }
    Ok(())
}
