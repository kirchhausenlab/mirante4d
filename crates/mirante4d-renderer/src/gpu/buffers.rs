use mirante4d_core::Shape3D;

use super::GpuRenderError;
use crate::RenderError;

const INTEGER_BRICK_METADATA_WORDS: u64 = 4;

pub(super) fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

pub(super) fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

pub(super) fn storage_texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::Rgba8Unorm,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

pub(super) fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

pub(super) fn checked_buffer_byte_count(
    resource: &'static str,
    element_count: usize,
    element_bytes: usize,
) -> Result<u64, GpuRenderError> {
    element_count
        .checked_mul(element_bytes)
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or(GpuRenderError::BufferSizeOverflow { resource })
}

pub(super) fn checked_u64_buffer_byte_count(
    resource: &'static str,
    element_count: u64,
    element_bytes: u64,
) -> Result<u64, GpuRenderError> {
    element_count
        .checked_mul(element_bytes)
        .ok_or(GpuRenderError::BufferSizeOverflow { resource })
}

pub(super) fn validate_general_buffer_bytes(
    limits: &wgpu::Limits,
    resource: &'static str,
    required_bytes: u64,
) -> Result<(), GpuRenderError> {
    let limit_bytes = limits.max_buffer_size;
    if required_bytes > limit_bytes {
        return Err(GpuRenderError::BufferTooLarge {
            resource,
            required_bytes,
            limit_bytes,
        });
    }
    Ok(())
}

pub(super) fn validate_storage_buffer_bytes(
    limits: &wgpu::Limits,
    resource: &'static str,
    required_bytes: u64,
) -> Result<(), GpuRenderError> {
    let limit_bytes = limits
        .max_buffer_size
        .min(limits.max_storage_buffer_binding_size);
    if required_bytes > limit_bytes {
        return Err(GpuRenderError::BufferTooLarge {
            resource,
            required_bytes,
            limit_bytes,
        });
    }
    Ok(())
}

pub(super) fn validate_uniform_buffer_bytes(
    limits: &wgpu::Limits,
    resource: &'static str,
    required_bytes: u64,
) -> Result<(), GpuRenderError> {
    let limit_bytes = limits
        .max_buffer_size
        .min(limits.max_uniform_buffer_binding_size);
    if required_bytes > limit_bytes {
        return Err(GpuRenderError::BufferTooLarge {
            resource,
            required_bytes,
            limit_bytes,
        });
    }
    Ok(())
}

pub(super) fn validate_u8_brick_atlas_budget(
    budget_bytes: u64,
    limits: &wgpu::Limits,
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
    slot_count: usize,
) -> Result<(), GpuRenderError> {
    validate_integer_brick_atlas_budget(
        budget_bytes,
        limits,
        brick_shape,
        brick_grid_shape,
        slot_count,
        4,
        "brick atlas packed uint8 values",
    )
}

pub(super) fn validate_u16_brick_atlas_budget(
    budget_bytes: u64,
    limits: &wgpu::Limits,
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
    slot_count: usize,
) -> Result<(), GpuRenderError> {
    validate_integer_brick_atlas_budget(
        budget_bytes,
        limits,
        brick_shape,
        brick_grid_shape,
        slot_count,
        2,
        "brick atlas packed uint16 values",
    )
}

fn validate_integer_brick_atlas_budget(
    budget_bytes: u64,
    limits: &wgpu::Limits,
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
    slot_count: usize,
    values_per_word: u64,
    value_resource: &'static str,
) -> Result<(), GpuRenderError> {
    let brick_voxel_count = brick_shape.element_count().map_err(RenderError::from)?;
    let packed_u32_per_brick = packed_u32_per_integer_brick(brick_voxel_count, values_per_word);
    let valid_u32_per_brick = validity_u32_per_brick(brick_voxel_count);
    let packed_values_len = (slot_count as u64)
        .checked_mul(packed_u32_per_brick)
        .ok_or(GpuRenderError::BufferSizeOverflow {
            resource: value_resource,
        })?;
    let valid_values_len = (slot_count as u64).checked_mul(valid_u32_per_brick).ok_or(
        GpuRenderError::BufferSizeOverflow {
            resource: "brick atlas integer validity bitset",
        },
    )?;
    let packed_values_bytes = checked_u64_buffer_byte_count(
        value_resource,
        packed_values_len,
        std::mem::size_of::<u32>() as u64,
    )?;
    let validity_bytes = checked_u64_buffer_byte_count(
        "brick atlas integer validity bitset",
        valid_values_len,
        std::mem::size_of::<u32>() as u64,
    )?;
    let page_table_bytes = checked_u64_buffer_byte_count(
        "brick atlas page table",
        brick_grid_shape
            .element_count()
            .map_err(RenderError::from)?,
        std::mem::size_of::<u32>() as u64,
    )?;
    let metadata_words = brick_grid_shape
        .element_count()
        .map_err(RenderError::from)?
        .checked_mul(INTEGER_BRICK_METADATA_WORDS)
        .ok_or(GpuRenderError::BufferSizeOverflow {
            resource: "brick atlas integer metadata",
        })?;
    let metadata_bytes = checked_u64_buffer_byte_count(
        "brick atlas integer metadata",
        metadata_words,
        std::mem::size_of::<u32>() as u64,
    )?;
    validate_storage_buffer_bytes(limits, value_resource, packed_values_bytes)?;
    validate_storage_buffer_bytes(
        limits,
        "brick atlas integer validity bitset",
        validity_bytes,
    )?;
    validate_storage_buffer_bytes(limits, "brick atlas page table", page_table_bytes)?;
    validate_storage_buffer_bytes(limits, "brick atlas integer metadata", metadata_bytes)?;
    let total_bytes = packed_values_bytes
        .checked_add(validity_bytes)
        .and_then(|bytes| bytes.checked_add(page_table_bytes))
        .and_then(|bytes| bytes.checked_add(metadata_bytes))
        .ok_or(GpuRenderError::BufferSizeOverflow {
            resource: "brick atlas",
        })?;
    validate_budget_bytes(value_resource, total_bytes, budget_bytes)
}

pub(super) fn validate_f32_brick_atlas_budget(
    budget_bytes: u64,
    limits: &wgpu::Limits,
    value_words: u64,
    page_table_words: u64,
) -> Result<(), GpuRenderError> {
    let values_bytes = checked_u64_buffer_byte_count(
        "brick atlas float32 values",
        value_words,
        std::mem::size_of::<f32>() as u64,
    )?;
    let page_table_bytes = checked_u64_buffer_byte_count(
        "brick atlas float32 page table",
        page_table_words,
        std::mem::size_of::<u32>() as u64,
    )?;
    validate_storage_buffer_bytes(limits, "brick atlas float32 values", values_bytes)?;
    validate_storage_buffer_bytes(limits, "brick atlas float32 page table", page_table_bytes)?;
    let total_bytes =
        values_bytes
            .checked_add(page_table_bytes)
            .ok_or(GpuRenderError::BufferSizeOverflow {
                resource: "brick atlas",
            })?;
    validate_budget_bytes("brick atlas float32 values", total_bytes, budget_bytes)
}

pub(super) fn checked_u32(axis: &'static str, value: u64) -> Result<u32, RenderError> {
    u32::try_from(value).map_err(|_| RenderError::DimensionTooLarge { axis, value })
}

pub(super) fn packed_u32_per_integer_brick(brick_voxel_count: u64, values_per_word: u64) -> u64 {
    brick_voxel_count.div_ceil(values_per_word)
}

pub(super) fn validity_u32_per_brick(brick_voxel_count: u64) -> u64 {
    brick_voxel_count.div_ceil(32)
}

fn validate_budget_bytes(
    resource: &'static str,
    required_bytes: u64,
    budget_bytes: u64,
) -> Result<(), GpuRenderError> {
    if required_bytes > budget_bytes {
        return Err(GpuRenderError::BudgetExceeded {
            resource,
            required_bytes,
            budget_bytes,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_buffer_limit_checks_use_storage_binding_limit_for_storage_resources() {
        let limits = wgpu::Limits {
            max_buffer_size: 1024,
            max_storage_buffer_binding_size: 256,
            ..Default::default()
        };

        validate_storage_buffer_bytes(&limits, "test storage", 256).unwrap();
        assert!(matches!(
            validate_storage_buffer_bytes(&limits, "test storage", 257),
            Err(GpuRenderError::BufferTooLarge {
                resource: "test storage",
                required_bytes: 257,
                limit_bytes: 256,
            })
        ));
        validate_general_buffer_bytes(&limits, "test readback", 1024).unwrap();
        assert!(matches!(
            validate_general_buffer_bytes(&limits, "test readback", 1025),
            Err(GpuRenderError::BufferTooLarge {
                resource: "test readback",
                required_bytes: 1025,
                limit_bytes: 1024,
            })
        ));
    }

    #[test]
    fn gpu_buffer_byte_count_detects_overflow() {
        assert_eq!(
            checked_buffer_byte_count("small", 3, std::mem::size_of::<u32>()).unwrap(),
            12
        );
        assert!(matches!(
            checked_u64_buffer_byte_count("huge", u64::MAX, 2),
            Err(GpuRenderError::BufferSizeOverflow { resource: "huge" })
        ));
    }

    #[test]
    fn gpu_brick_atlas_budget_preflight_rejects_u16_allocations_before_device_work() {
        let limits = generous_test_limits();
        let brick_shape = Shape3D::new(8, 8, 8).unwrap();
        let brick_grid_shape = Shape3D::new(2, 2, 2).unwrap();
        let expected_bytes = 2_336;

        validate_u16_brick_atlas_budget(expected_bytes, &limits, brick_shape, brick_grid_shape, 2)
            .unwrap();
        assert!(matches!(
            validate_u16_brick_atlas_budget(
                expected_bytes - 1,
                &limits,
                brick_shape,
                brick_grid_shape,
                2,
            ),
            Err(GpuRenderError::BudgetExceeded {
                resource: "brick atlas packed uint16 values",
                required_bytes: 2_336,
                budget_bytes: 2_335,
            })
        ));
    }

    #[test]
    fn gpu_brick_atlas_budget_preflight_rejects_u8_allocations_before_device_work() {
        let limits = generous_test_limits();
        let brick_shape = Shape3D::new(8, 8, 8).unwrap();
        let brick_grid_shape = Shape3D::new(2, 2, 2).unwrap();
        let expected_bytes = 1_312;

        validate_u8_brick_atlas_budget(expected_bytes, &limits, brick_shape, brick_grid_shape, 2)
            .unwrap();
        assert!(matches!(
            validate_u8_brick_atlas_budget(
                expected_bytes - 1,
                &limits,
                brick_shape,
                brick_grid_shape,
                2,
            ),
            Err(GpuRenderError::BudgetExceeded {
                resource: "brick atlas packed uint8 values",
                required_bytes: 1_312,
                budget_bytes: 1_311,
            })
        ));
    }

    #[test]
    fn gpu_brick_atlas_budget_preflight_rejects_f32_allocations_before_device_work() {
        let limits = generous_test_limits();
        let value_words = 2 * 8 * 8 * 8;
        let page_table_words = 2 * 2 * 2 * 4;
        let expected_bytes = 4_224;

        validate_f32_brick_atlas_budget(expected_bytes, &limits, value_words, page_table_words)
            .unwrap();
        assert!(matches!(
            validate_f32_brick_atlas_budget(
                expected_bytes - 1,
                &limits,
                value_words,
                page_table_words,
            ),
            Err(GpuRenderError::BudgetExceeded {
                resource: "brick atlas float32 values",
                required_bytes: 4_224,
                budget_bytes: 4_223,
            })
        ));
    }

    fn generous_test_limits() -> wgpu::Limits {
        wgpu::Limits {
            max_buffer_size: 1 << 30,
            max_storage_buffer_binding_size: 1 << 30,
            ..Default::default()
        }
    }
}
