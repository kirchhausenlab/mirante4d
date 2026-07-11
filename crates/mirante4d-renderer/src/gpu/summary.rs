use mirante4d_data::{DenseVolumeF32, DenseVolumeU16, VolumeRegion};
use wgpu::util::DeviceExt;

use super::{GpuRenderError, GpuRenderer};
use crate::RenderError;

const INTENSITY_SUMMARY_WORKGROUP_SIZE: u32 = 64;
const INTENSITY_SUMMARY_CHUNK_VOXELS: u32 = 1024;
const INTENSITY_SUMMARY_PARTIAL_FIELDS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpuIntensitySummaryU16 {
    pub voxel_count: u64,
    pub nonzero_count: u64,
    pub min: u16,
    pub max: u16,
    pub sum: u64,
    pub mean: f64,
    pub gpu_compute_ns: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpuIntensitySummaryF32 {
    pub voxel_count: u64,
    pub nonzero_count: u64,
    pub min: f32,
    pub max: f32,
    pub sum: f64,
    pub mean: f64,
    pub gpu_compute_ns: Option<u64>,
}

impl GpuRenderer {
    pub fn summarize_u16_volume(
        &self,
        volume: &DenseVolumeU16,
    ) -> Result<GpuIntensitySummaryU16, GpuRenderError> {
        if volume.render_valid_mask().is_some() {
            return Ok(summarize_u16_region_render_valid(
                volume,
                VolumeRegion::new(0, 0, 0, volume.shape.z, volume.shape.y, volume.shape.x)
                    .expect("volume shape is a valid full-volume region"),
            ));
        }
        let sample_count = volume.values().len() as u64;
        self.summarize_u16_volume_samples(
            volume,
            sample_count,
            [
                super::buffers::checked_u32("voxel_count", sample_count)?,
                INTENSITY_SUMMARY_CHUNK_VOXELS,
                INTENSITY_SUMMARY_PARTIAL_FIELDS as u32,
                0,
                super::buffers::checked_u32("z", volume.shape.z)?,
                super::buffers::checked_u32("y", volume.shape.y)?,
                super::buffers::checked_u32("x", volume.shape.x)?,
                0,
                0,
                0,
                super::buffers::checked_u32("z", volume.shape.z)?,
                super::buffers::checked_u32("y", volume.shape.y)?,
                super::buffers::checked_u32("x", volume.shape.x)?,
                0,
                0,
                0,
            ],
        )
    }

    pub fn summarize_u16_region(
        &self,
        volume: &DenseVolumeU16,
        region: VolumeRegion,
    ) -> Result<GpuIntensitySummaryU16, GpuRenderError> {
        validate_summary_region(volume, region)?;
        if volume.render_valid_mask().is_some() {
            return Ok(summarize_u16_region_render_valid(volume, region));
        }
        let sample_count = region
            .z_size
            .checked_mul(region.y_size)
            .and_then(|value| value.checked_mul(region.x_size))
            .ok_or(RenderError::DimensionTooLarge {
                axis: "roi_sample_count",
                value: u64::MAX,
            })?;
        self.summarize_u16_volume_samples(
            volume,
            sample_count,
            [
                super::buffers::checked_u32("roi_sample_count", sample_count)?,
                INTENSITY_SUMMARY_CHUNK_VOXELS,
                INTENSITY_SUMMARY_PARTIAL_FIELDS as u32,
                1,
                super::buffers::checked_u32("z", volume.shape.z)?,
                super::buffers::checked_u32("y", volume.shape.y)?,
                super::buffers::checked_u32("x", volume.shape.x)?,
                super::buffers::checked_u32("z_start", region.z_start)?,
                super::buffers::checked_u32("y_start", region.y_start)?,
                super::buffers::checked_u32("x_start", region.x_start)?,
                super::buffers::checked_u32("z_size", region.z_size)?,
                super::buffers::checked_u32("y_size", region.y_size)?,
                super::buffers::checked_u32("x_size", region.x_size)?,
                0,
                0,
                0,
            ],
        )
    }

    pub fn summarize_f32_volume(
        &self,
        volume: &DenseVolumeF32,
    ) -> Result<GpuIntensitySummaryF32, GpuRenderError> {
        if volume.render_valid_mask().is_some() {
            return Ok(summarize_f32_region_render_valid(
                volume,
                VolumeRegion::new(0, 0, 0, volume.shape.z, volume.shape.y, volume.shape.x)
                    .expect("volume shape is a valid full-volume region"),
            ));
        }
        let sample_count = volume.values().len() as u64;
        self.summarize_f32_volume_samples(
            volume,
            sample_count,
            [
                super::buffers::checked_u32("voxel_count", sample_count)?,
                INTENSITY_SUMMARY_CHUNK_VOXELS,
                INTENSITY_SUMMARY_PARTIAL_FIELDS as u32,
                0,
                super::buffers::checked_u32("z", volume.shape.z)?,
                super::buffers::checked_u32("y", volume.shape.y)?,
                super::buffers::checked_u32("x", volume.shape.x)?,
                0,
                0,
                0,
                super::buffers::checked_u32("z", volume.shape.z)?,
                super::buffers::checked_u32("y", volume.shape.y)?,
                super::buffers::checked_u32("x", volume.shape.x)?,
                0,
                0,
                0,
            ],
        )
    }

    pub fn summarize_f32_region(
        &self,
        volume: &DenseVolumeF32,
        region: VolumeRegion,
    ) -> Result<GpuIntensitySummaryF32, GpuRenderError> {
        validate_f32_summary_region(volume, region)?;
        if volume.render_valid_mask().is_some() {
            return Ok(summarize_f32_region_render_valid(volume, region));
        }
        let sample_count = region
            .z_size
            .checked_mul(region.y_size)
            .and_then(|value| value.checked_mul(region.x_size))
            .ok_or(RenderError::DimensionTooLarge {
                axis: "roi_sample_count",
                value: u64::MAX,
            })?;
        self.summarize_f32_volume_samples(
            volume,
            sample_count,
            [
                super::buffers::checked_u32("roi_sample_count", sample_count)?,
                INTENSITY_SUMMARY_CHUNK_VOXELS,
                INTENSITY_SUMMARY_PARTIAL_FIELDS as u32,
                1,
                super::buffers::checked_u32("z", volume.shape.z)?,
                super::buffers::checked_u32("y", volume.shape.y)?,
                super::buffers::checked_u32("x", volume.shape.x)?,
                super::buffers::checked_u32("z_start", region.z_start)?,
                super::buffers::checked_u32("y_start", region.y_start)?,
                super::buffers::checked_u32("x_start", region.x_start)?,
                super::buffers::checked_u32("z_size", region.z_size)?,
                super::buffers::checked_u32("y_size", region.y_size)?,
                super::buffers::checked_u32("x_size", region.x_size)?,
                0,
                0,
                0,
            ],
        )
    }

    fn summarize_u16_volume_samples(
        &self,
        volume: &DenseVolumeU16,
        sample_count: u64,
        params_u32: [u32; 16],
    ) -> Result<GpuIntensitySummaryU16, GpuRenderError> {
        if sample_count == 0 {
            return Ok(GpuIntensitySummaryU16 {
                voxel_count: 0,
                nonzero_count: 0,
                min: 0,
                max: 0,
                sum: 0,
                mean: 0.0,
                gpu_compute_ns: None,
            });
        }
        let sample_count_u32 =
            super::buffers::checked_u32("intensity_summary_sample_count", sample_count)?;
        let partial_count = sample_count_u32.div_ceil(INTENSITY_SUMMARY_CHUNK_VOXELS);
        let output_len = (partial_count as usize)
            .checked_mul(INTENSITY_SUMMARY_PARTIAL_FIELDS)
            .ok_or(RenderError::DimensionTooLarge {
                axis: "intensity_summary_partial_fields",
                value: u64::from(partial_count),
            })?;
        let output_bytes = (output_len * std::mem::size_of::<u32>()) as wgpu::BufferAddress;

        let input_buffer = self.cached_volume_buffer(volume)?;
        let output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-intensity-summary-output-u32"),
            size: output_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-intensity-summary-readback-u32"),
            size: output_bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let params_u32_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mirante4d-intensity-summary-params-u32"),
                contents: bytemuck::cast_slice(&params_u32),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mirante4d-intensity-summary-bind-group"),
            layout: &self.intensity_summary_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: input_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: output_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params_u32_buffer.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-intensity-summary-command-encoder"),
            });
        let timestamp = self.timestamp_query_pair("mirante4d-intensity-summary-timestamp");
        let timestamp_writes = timestamp
            .as_ref()
            .map(super::readback::GpuTimestampQueryPair::compute_pass_writes);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-intensity-summary-compute-pass"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.intensity_summary_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                partial_count.div_ceil(INTENSITY_SUMMARY_WORKGROUP_SIZE),
                1,
                1,
            );
        }
        encoder.copy_buffer_to_buffer(&output_buffer, 0, &readback_buffer, 0, output_bytes);

        let (partials, gpu_compute_ns) =
            self.submit_and_read_u32_with_optional_timestamp(encoder, readback_buffer, timestamp)?;
        let mut summary = summarize_intensity_partials_u16(&partials, partial_count as usize);
        summary.gpu_compute_ns = gpu_compute_ns;
        Ok(summary)
    }

    fn summarize_f32_volume_samples(
        &self,
        volume: &DenseVolumeF32,
        sample_count: u64,
        params_u32: [u32; 16],
    ) -> Result<GpuIntensitySummaryF32, GpuRenderError> {
        if sample_count == 0 {
            return Ok(GpuIntensitySummaryF32 {
                voxel_count: 0,
                nonzero_count: 0,
                min: 0.0,
                max: 0.0,
                sum: 0.0,
                mean: 0.0,
                gpu_compute_ns: None,
            });
        }
        let sample_count_u32 =
            super::buffers::checked_u32("intensity_summary_sample_count", sample_count)?;
        let partial_count = sample_count_u32.div_ceil(INTENSITY_SUMMARY_CHUNK_VOXELS);
        let output_len = (partial_count as usize)
            .checked_mul(INTENSITY_SUMMARY_PARTIAL_FIELDS)
            .ok_or(RenderError::DimensionTooLarge {
                axis: "intensity_summary_partial_fields",
                value: u64::from(partial_count),
            })?;
        let output_bytes = (output_len * std::mem::size_of::<f32>()) as wgpu::BufferAddress;

        let input_buffer = self.cached_f32_volume_buffer(volume)?;
        let output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-intensity-summary-f32-output"),
            size: output_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-intensity-summary-f32-readback"),
            size: output_bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let params_u32_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mirante4d-intensity-summary-f32-params-u32"),
                contents: bytemuck::cast_slice(&params_u32),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mirante4d-intensity-summary-f32-bind-group"),
            layout: &self.intensity_summary_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: input_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: output_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params_u32_buffer.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-intensity-summary-f32-command-encoder"),
            });
        let timestamp = self.timestamp_query_pair("mirante4d-intensity-summary-f32-timestamp");
        let timestamp_writes = timestamp
            .as_ref()
            .map(super::readback::GpuTimestampQueryPair::compute_pass_writes);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-intensity-summary-f32-compute-pass"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.intensity_summary_f32_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                partial_count.div_ceil(INTENSITY_SUMMARY_WORKGROUP_SIZE),
                1,
                1,
            );
        }
        encoder.copy_buffer_to_buffer(&output_buffer, 0, &readback_buffer, 0, output_bytes);

        let (partials, gpu_compute_ns) =
            self.submit_and_read_f32_with_optional_timestamp(encoder, readback_buffer, timestamp)?;
        let mut summary = summarize_intensity_partials_f32(&partials, partial_count as usize);
        summary.gpu_compute_ns = gpu_compute_ns;
        Ok(summary)
    }
}

fn validate_summary_region(
    volume: &DenseVolumeU16,
    region: VolumeRegion,
) -> Result<(), RenderError> {
    if region.z_size == 0 || region.y_size == 0 || region.x_size == 0 {
        return Err(RenderError::InvalidIntensitySummaryRegion(
            "region dimensions must be positive",
        ));
    }
    let z_end = region.z_start.checked_add(region.z_size).ok_or(
        RenderError::InvalidIntensitySummaryRegion("region z end overflows"),
    )?;
    let y_end = region.y_start.checked_add(region.y_size).ok_or(
        RenderError::InvalidIntensitySummaryRegion("region y end overflows"),
    )?;
    let x_end = region.x_start.checked_add(region.x_size).ok_or(
        RenderError::InvalidIntensitySummaryRegion("region x end overflows"),
    )?;
    if z_end > volume.shape.z || y_end > volume.shape.y || x_end > volume.shape.x {
        return Err(RenderError::InvalidIntensitySummaryRegion(
            "region exceeds volume shape",
        ));
    }
    Ok(())
}

fn validate_f32_summary_region(
    volume: &DenseVolumeF32,
    region: VolumeRegion,
) -> Result<(), RenderError> {
    if region.z_size == 0 || region.y_size == 0 || region.x_size == 0 {
        return Err(RenderError::InvalidIntensitySummaryRegion(
            "region dimensions must be positive",
        ));
    }
    let z_end = region.z_start.checked_add(region.z_size).ok_or(
        RenderError::InvalidIntensitySummaryRegion("region z end overflows"),
    )?;
    let y_end = region.y_start.checked_add(region.y_size).ok_or(
        RenderError::InvalidIntensitySummaryRegion("region y end overflows"),
    )?;
    let x_end = region.x_start.checked_add(region.x_size).ok_or(
        RenderError::InvalidIntensitySummaryRegion("region x end overflows"),
    )?;
    if z_end > volume.shape.z || y_end > volume.shape.y || x_end > volume.shape.x {
        return Err(RenderError::InvalidIntensitySummaryRegion(
            "region exceeds volume shape",
        ));
    }
    Ok(())
}

fn summarize_u16_region_render_valid(
    volume: &DenseVolumeU16,
    region: VolumeRegion,
) -> GpuIntensitySummaryU16 {
    let mut voxel_count = 0u64;
    let mut nonzero_count = 0u64;
    let mut min = u16::MAX;
    let mut max = u16::MIN;
    let mut sum = 0u64;

    for z in region.z_start..region.z_start + region.z_size {
        for y in region.y_start..region.y_start + region.y_size {
            for x in region.x_start..region.x_start + region.x_size {
                let Some(value) = volume.render_voxel(z, y, x) else {
                    continue;
                };
                voxel_count += 1;
                if value != 0 {
                    nonzero_count += 1;
                }
                min = min.min(value);
                max = max.max(value);
                sum += u64::from(value);
            }
        }
    }

    if voxel_count == 0 {
        GpuIntensitySummaryU16 {
            voxel_count: 0,
            nonzero_count: 0,
            min: 0,
            max: 0,
            sum: 0,
            mean: 0.0,
            gpu_compute_ns: None,
        }
    } else {
        GpuIntensitySummaryU16 {
            voxel_count,
            nonzero_count,
            min,
            max,
            sum,
            mean: sum as f64 / voxel_count as f64,
            gpu_compute_ns: None,
        }
    }
}

fn summarize_f32_region_render_valid(
    volume: &DenseVolumeF32,
    region: VolumeRegion,
) -> GpuIntensitySummaryF32 {
    let mut voxel_count = 0u64;
    let mut nonzero_count = 0u64;
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let mut sum = 0.0f64;

    for z in region.z_start..region.z_start + region.z_size {
        for y in region.y_start..region.y_start + region.y_size {
            for x in region.x_start..region.x_start + region.x_size {
                let Some(value) = volume.render_voxel(z, y, x) else {
                    continue;
                };
                voxel_count += 1;
                if value != 0.0 {
                    nonzero_count += 1;
                }
                min = min.min(value);
                max = max.max(value);
                sum += f64::from(value);
            }
        }
    }

    if voxel_count == 0 {
        GpuIntensitySummaryF32 {
            voxel_count: 0,
            nonzero_count: 0,
            min: 0.0,
            max: 0.0,
            sum: 0.0,
            mean: 0.0,
            gpu_compute_ns: None,
        }
    } else {
        GpuIntensitySummaryF32 {
            voxel_count,
            nonzero_count,
            min,
            max,
            sum,
            mean: sum / voxel_count as f64,
            gpu_compute_ns: None,
        }
    }
}

fn summarize_intensity_partials_u16(
    partials: &[u32],
    partial_count: usize,
) -> GpuIntensitySummaryU16 {
    let mut voxel_count = 0u64;
    let mut nonzero_count = 0u64;
    let mut min = u16::MAX;
    let mut max = u16::MIN;
    let mut sum = 0u64;

    for partial in partials
        .chunks_exact(INTENSITY_SUMMARY_PARTIAL_FIELDS)
        .take(partial_count)
    {
        let count = u64::from(partial[3]);
        if count == 0 {
            continue;
        }
        voxel_count += count;
        nonzero_count += u64::from(partial[2]);
        min = min.min(partial[0] as u16);
        max = max.max(partial[1] as u16);
        sum += u64::from(partial[4]);
    }

    if voxel_count == 0 {
        GpuIntensitySummaryU16 {
            voxel_count: 0,
            nonzero_count: 0,
            min: 0,
            max: 0,
            sum: 0,
            mean: 0.0,
            gpu_compute_ns: None,
        }
    } else {
        GpuIntensitySummaryU16 {
            voxel_count,
            nonzero_count,
            min,
            max,
            sum,
            mean: sum as f64 / voxel_count as f64,
            gpu_compute_ns: None,
        }
    }
}

fn summarize_intensity_partials_f32(
    partials: &[f32],
    partial_count: usize,
) -> GpuIntensitySummaryF32 {
    let mut voxel_count = 0u64;
    let mut nonzero_count = 0u64;
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let mut sum = 0.0f64;

    for partial in partials
        .chunks_exact(INTENSITY_SUMMARY_PARTIAL_FIELDS)
        .take(partial_count)
    {
        let count = partial[3].round().max(0.0) as u64;
        if count == 0 {
            continue;
        }
        voxel_count += count;
        nonzero_count += partial[2].round().max(0.0) as u64;
        min = min.min(partial[0]);
        max = max.max(partial[1]);
        sum += f64::from(partial[4]);
    }

    if voxel_count == 0 {
        GpuIntensitySummaryF32 {
            voxel_count: 0,
            nonzero_count: 0,
            min: 0.0,
            max: 0.0,
            sum: 0.0,
            mean: 0.0,
            gpu_compute_ns: None,
        }
    } else {
        GpuIntensitySummaryF32 {
            voxel_count,
            nonzero_count,
            min,
            max,
            sum,
            mean: sum / voxel_count as f64,
            gpu_compute_ns: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use mirante4d_core::{DatasetId, GridToWorld, LayerId, Shape3D, TimeIndex};

    use super::*;

    #[test]
    fn gpu_masked_u16_summary_uses_render_valid_domain() {
        let volume = DenseVolumeU16::new(
            DatasetId::new("gpu-masked-summary").unwrap(),
            LayerId::new("ch0").unwrap(),
            0,
            TimeIndex(0),
            Shape3D::new(1, 1, 4).unwrap(),
            GridToWorld::identity(),
            vec![255, 5, 0, 7],
        )
        .unwrap()
        .with_render_valid(vec![0, 1, 0, 1])
        .unwrap();
        let summary = summarize_u16_region_render_valid(
            &volume,
            VolumeRegion::new(0, 0, 0, 1, 1, 4).unwrap(),
        );

        assert_eq!(
            summary,
            GpuIntensitySummaryU16 {
                voxel_count: 2,
                nonzero_count: 2,
                min: 5,
                max: 7,
                sum: 12,
                mean: 6.0,
                gpu_compute_ns: None,
            }
        );
    }

    #[test]
    fn gpu_masked_f32_summary_uses_render_valid_domain() {
        let volume = DenseVolumeF32::new(
            DatasetId::new("gpu-masked-summary-f32").unwrap(),
            LayerId::new("ch0").unwrap(),
            0,
            TimeIndex(0),
            Shape3D::new(1, 1, 3).unwrap(),
            GridToWorld::identity(),
            vec![-10.0, 0.0, 4.5],
        )
        .unwrap()
        .with_render_valid(vec![0, 1, 1])
        .unwrap();
        let summary = summarize_f32_region_render_valid(
            &volume,
            VolumeRegion::new(0, 0, 0, 1, 1, 3).unwrap(),
        );

        assert_eq!(summary.voxel_count, 2);
        assert_eq!(summary.nonzero_count, 1);
        assert_eq!(summary.min, 0.0);
        assert_eq!(summary.max, 4.5);
        assert_eq!(summary.sum, 4.5);
        assert_eq!(summary.mean, 2.25);
    }

    #[test]
    fn gpu_masked_summary_all_invalid_reports_empty_domain() {
        let volume = DenseVolumeU16::new(
            DatasetId::new("gpu-masked-empty-summary").unwrap(),
            LayerId::new("ch0").unwrap(),
            0,
            TimeIndex(0),
            Shape3D::new(1, 1, 2).unwrap(),
            GridToWorld::identity(),
            vec![255, 1],
        )
        .unwrap()
        .with_render_valid(vec![0, 0])
        .unwrap();
        let summary = summarize_u16_region_render_valid(
            &volume,
            VolumeRegion::new(0, 0, 0, 1, 1, 2).unwrap(),
        );

        assert_eq!(
            summary,
            GpuIntensitySummaryU16 {
                voxel_count: 0,
                nonzero_count: 0,
                min: 0,
                max: 0,
                sum: 0,
                mean: 0.0,
                gpu_compute_ns: None,
            }
        );
    }

    #[test]
    fn summarizes_intensity_partials_exactly() {
        let partials = vec![
            3, 7, 2, 4, 20, 0, 0, 0, 1, 9, 3, 3, 18, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];

        let summary = summarize_intensity_partials_u16(&partials, 3);

        assert_eq!(
            summary,
            GpuIntensitySummaryU16 {
                voxel_count: 7,
                nonzero_count: 5,
                min: 1,
                max: 9,
                sum: 38,
                mean: 38.0 / 7.0,
                gpu_compute_ns: None,
            }
        );
    }

    #[test]
    fn summarizes_empty_intensity_partials_as_zero() {
        let summary = summarize_intensity_partials_u16(&[], 0);

        assert_eq!(
            summary,
            GpuIntensitySummaryU16 {
                voxel_count: 0,
                nonzero_count: 0,
                min: 0,
                max: 0,
                sum: 0,
                mean: 0.0,
                gpu_compute_ns: None,
            }
        );
    }

    #[test]
    fn summarizes_float32_intensity_partials_with_f64_host_mean() {
        let partials = vec![
            -1.5, 7.25, 2.0, 4.0, 20.5, 0.0, 0.0, 0.0, -2.0, 9.0, 3.0, 3.0, 18.25, 0.0, 0.0, 0.0,
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ];

        let summary = summarize_intensity_partials_f32(&partials, 3);

        assert_eq!(summary.voxel_count, 7);
        assert_eq!(summary.nonzero_count, 5);
        assert_eq!(summary.min, -2.0);
        assert_eq!(summary.max, 9.0);
        assert!((summary.sum - 38.75).abs() <= 1.0e-6);
        assert!((summary.mean - (38.75 / 7.0)).abs() <= 1.0e-6);
    }

    #[test]
    fn summary_region_validation_rejects_empty_and_out_of_bounds_regions() {
        let volume = DenseVolumeU16::new(
            DatasetId::new("summary-validation").unwrap(),
            LayerId::new("ch0").unwrap(),
            0,
            TimeIndex(0),
            Shape3D::new(2, 2, 2).unwrap(),
            GridToWorld::identity(),
            vec![0; 8],
        )
        .unwrap();
        let empty = VolumeRegion {
            z_start: 0,
            y_start: 0,
            x_start: 0,
            z_size: 0,
            y_size: 1,
            x_size: 1,
        };
        let out_of_bounds = VolumeRegion::new(1, 0, 0, 2, 1, 1).unwrap();

        assert!(matches!(
            validate_summary_region(&volume, empty),
            Err(RenderError::InvalidIntensitySummaryRegion(
                "region dimensions must be positive"
            ))
        ));
        assert!(matches!(
            validate_summary_region(&volume, out_of_bounds),
            Err(RenderError::InvalidIntensitySummaryRegion(
                "region exceeds volume shape"
            ))
        ));
    }
}
