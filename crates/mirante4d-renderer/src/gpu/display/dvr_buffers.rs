use crate::gpu::atlas::IntegerAtlasDType;

use super::*;

pub(super) fn dvr_atlas_buffer_words(
    channel: &GpuDvrDisplayAtlasChannel,
) -> Result<GpuDvrAtlasBufferWords, GpuRenderError> {
    match &channel.atlas {
        GpuDvrDisplayAtlas::Integer(atlas) => {
            let slot_count = atlas.slot_count as u64;
            let packed_values = slot_count.checked_mul(atlas.packed_u32_per_brick).ok_or(
                GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel DVR packed values",
                },
            )?;
            let validity = slot_count.checked_mul(atlas.valid_u32_per_brick).ok_or(
                GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel DVR validity",
                },
            )?;
            let page_table = atlas
                .brick_grid_shape
                .element_count()
                .map_err(RenderError::from)?;
            let metadata = page_table.checked_mul(INTEGER_BRICK_METADATA_WORDS).ok_or(
                GpuRenderError::BufferSizeOverflow {
                    resource: "multi-channel DVR metadata",
                },
            )?;
            Ok(GpuDvrAtlasBufferWords {
                packed_values,
                validity,
                f32_values: 0,
                page_table,
                metadata,
            })
        }
        GpuDvrDisplayAtlas::F32(atlas) => {
            let f32_values = atlas.value_words_used.max(1);
            let page_table = atlas.page_table_word_count;
            Ok(GpuDvrAtlasBufferWords {
                packed_values: 0,
                validity: 0,
                f32_values,
                page_table,
                metadata: 0,
            })
        }
    }
}

pub(super) fn push_dvr_channel_descriptors(
    params_u32: &mut Vec<u32>,
    params_f32: &mut Vec<f32>,
    channel: &GpuDvrDisplayAtlasChannel,
    offsets: GpuDvrCombinedOffsets,
) -> Result<(), GpuRenderError> {
    match &channel.atlas {
        GpuDvrDisplayAtlas::Integer(atlas) => {
            let dtype = match atlas.dtype {
                IntegerAtlasDType::U8 => DVR_CHANNEL_DTYPE_U8,
                IntegerAtlasDType::U16 => DVR_CHANNEL_DTYPE_U16,
            };
            params_u32.extend_from_slice(&[
                u32::from(channel.display_visible),
                u32::from(channel.parameters.color_transfer.invert),
                checked_u32("DVR brick_x", atlas.brick_shape.x)?,
                checked_u32("DVR brick_y", atlas.brick_shape.y)?,
                checked_u32("DVR brick_z", atlas.brick_shape.z)?,
                checked_u32("DVR grid_x", atlas.brick_grid_shape.x)?,
                checked_u32("DVR grid_y", atlas.brick_grid_shape.y)?,
                checked_u32("DVR grid_z", atlas.brick_grid_shape.z)?,
                checked_u32("DVR packed_u32_per_brick", atlas.packed_u32_per_brick)?,
                atlas.values_per_word,
                atlas.bits_per_value,
                atlas.value_mask,
                checked_u32("DVR valid_u32_per_brick", atlas.valid_u32_per_brick)?,
                checked_u32("DVR packed value offset", offsets.packed_values_words)?,
                checked_u32("DVR validity offset", offsets.validity_words)?,
                checked_u32("DVR page table offset", offsets.page_table_words)?,
                checked_u32("DVR metadata offset", offsets.metadata_words)?,
                dtype,
            ]);
        }
        GpuDvrDisplayAtlas::F32(atlas) => {
            params_u32.extend_from_slice(&[
                u32::from(channel.display_visible),
                u32::from(channel.parameters.color_transfer.invert),
                checked_u32("DVR brick_x", atlas.brick_shape.x)?,
                checked_u32("DVR brick_y", atlas.brick_shape.y)?,
                checked_u32("DVR brick_z", atlas.brick_shape.z)?,
                checked_u32("DVR grid_x", atlas.brick_grid_shape.x)?,
                checked_u32("DVR grid_y", atlas.brick_grid_shape.y)?,
                checked_u32("DVR grid_z", atlas.brick_grid_shape.z)?,
                checked_u32("DVR brick_voxel_count", atlas.brick_voxel_count)?,
                0,
                0,
                0,
                0,
                checked_u32("DVR float32 value offset", offsets.f32_values_words)?,
                0,
                checked_u32("DVR page table offset", offsets.page_table_words)?,
                0,
                DVR_CHANNEL_DTYPE_F32,
            ]);
        }
    }
    debug_assert_eq!(params_u32.len() % DVR_CHANNEL_U32_STRIDE, 0);
    params_f32.extend_from_slice(&[
        channel.parameters.color_transfer.window.low,
        channel.parameters.color_transfer.window.high,
        channel.parameters.color_transfer.curve.gamma_value(),
        channel.parameters.opacity_transfer.window.low,
        channel.parameters.opacity_transfer.window.high,
        channel.parameters.opacity_transfer.curve.gamma_value(),
        channel.parameters.density_scale as f32,
        channel.parameters.channel_opacity * channel.parameters.color_rgba[3],
        channel.parameters.color_rgba[0],
        channel.parameters.color_rgba[1],
        channel.parameters.color_rgba[2],
    ]);
    debug_assert_eq!(params_f32.len() % DVR_CHANNEL_F32_STRIDE, 0);
    Ok(())
}

pub(super) fn copy_dvr_channel_atlas_buffers(
    encoder: &mut wgpu::CommandEncoder,
    channel: &GpuDvrDisplayAtlasChannel,
    words: GpuDvrAtlasBufferWords,
    offsets: GpuDvrCombinedOffsets,
    buffers: GpuDvrCombinedBufferSet<'_>,
) -> Result<(), GpuRenderError> {
    match &channel.atlas {
        GpuDvrDisplayAtlas::Integer(atlas) => {
            copy_words_to_combined_buffer(
                encoder,
                atlas.packed_values_buffer.as_ref(),
                buffers.packed_values_buffer,
                offsets.packed_values_words,
                words.packed_values,
                "multi-channel DVR packed values",
            )?;
            copy_words_to_combined_buffer(
                encoder,
                atlas.validity_buffer.as_ref(),
                buffers.validity_buffer,
                offsets.validity_words,
                words.validity,
                "multi-channel DVR validity",
            )?;
            copy_words_to_combined_buffer(
                encoder,
                atlas.page_table_buffer.as_ref(),
                buffers.page_table_buffer,
                offsets.page_table_words,
                words.page_table,
                "multi-channel DVR page tables",
            )?;
            copy_words_to_combined_buffer(
                encoder,
                atlas.metadata_buffer.as_ref(),
                buffers.metadata_buffer,
                offsets.metadata_words,
                words.metadata,
                "multi-channel DVR metadata",
            )
        }
        GpuDvrDisplayAtlas::F32(atlas) => {
            copy_words_to_combined_buffer(
                encoder,
                atlas.values_buffer.as_ref(),
                buffers.f32_values_buffer,
                offsets.f32_values_words,
                words.f32_values,
                "multi-channel DVR float32 values",
            )?;
            copy_words_to_combined_buffer(
                encoder,
                atlas.page_table_buffer.as_ref(),
                buffers.page_table_buffer,
                offsets.page_table_words,
                words.page_table,
                "multi-channel DVR page tables",
            )
        }
    }
}

pub(super) fn copy_words_to_combined_buffer(
    encoder: &mut wgpu::CommandEncoder,
    source: &wgpu::Buffer,
    destination: &wgpu::Buffer,
    destination_offset_words: u64,
    word_count: u64,
    resource: &'static str,
) -> Result<(), GpuRenderError> {
    let destination_offset = checked_u64_buffer_byte_count(
        resource,
        destination_offset_words,
        std::mem::size_of::<u32>() as u64,
    )?;
    let byte_count =
        checked_u64_buffer_byte_count(resource, word_count, std::mem::size_of::<u32>() as u64)?;
    encoder.copy_buffer_to_buffer(
        source,
        0,
        destination,
        destination_offset as wgpu::BufferAddress,
        byte_count as wgpu::BufferAddress,
    );
    Ok(())
}
