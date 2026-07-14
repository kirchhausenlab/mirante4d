use std::sync::mpsc;

use super::{GpuDisplayFrame, GpuRenderError, GpuRenderer};

pub(super) struct GpuTimestampQueryPair {
    query_set: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    readback_buffer: wgpu::Buffer,
}

impl GpuTimestampQueryPair {
    pub(super) fn compute_pass_writes(&self) -> wgpu::ComputePassTimestampWrites<'_> {
        wgpu::ComputePassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: Some(0),
            end_of_pass_write_index: Some(1),
        }
    }

    pub(super) fn compute_pass_begin_write(&self) -> wgpu::ComputePassTimestampWrites<'_> {
        wgpu::ComputePassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: Some(0),
            end_of_pass_write_index: None,
        }
    }

    pub(super) fn compute_pass_end_write(&self) -> wgpu::ComputePassTimestampWrites<'_> {
        wgpu::ComputePassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: None,
            end_of_pass_write_index: Some(1),
        }
    }
}

pub(super) fn timestamp_query_pair_for_device(
    device: &wgpu::Device,
    enabled: bool,
    label: &'static str,
) -> Option<GpuTimestampQueryPair> {
    if !enabled {
        return None;
    }
    let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
        label: Some(label),
        ty: wgpu::QueryType::Timestamp,
        count: 2,
    });
    let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mirante4d-gpu-timestamp-resolve"),
        size: wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mirante4d-gpu-timestamp-readback"),
        size: u64::from(wgpu::QUERY_SIZE) * 2,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    Some(GpuTimestampQueryPair {
        query_set,
        resolve_buffer,
        readback_buffer,
    })
}

pub(super) fn submit_and_read_u32_with_optional_timestamp_from_device(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mut encoder: wgpu::CommandEncoder,
    readback_buffer: wgpu::Buffer,
    timestamp: Option<GpuTimestampQueryPair>,
) -> Result<(Vec<u32>, Option<u64>), GpuRenderError> {
    if let Some(timestamp) = &timestamp {
        encoder.resolve_query_set(&timestamp.query_set, 0..2, &timestamp.resolve_buffer, 0);
        encoder.copy_buffer_to_buffer(
            &timestamp.resolve_buffer,
            0,
            &timestamp.readback_buffer,
            0,
            u64::from(wgpu::QUERY_SIZE) * 2,
        );
    }
    let submission = queue.submit(Some(encoder.finish()));
    let (read_sender, read_receiver) = mpsc::channel();
    readback_buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = read_sender.send(result);
        });
    let timestamp_receiver = timestamp.as_ref().map(|timestamp| {
        let (sender, receiver) = mpsc::channel();
        timestamp
            .readback_buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        receiver
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        })
        .map_err(|err| GpuRenderError::PollFailed(err.to_string()))?;
    read_receiver
        .recv()
        .map_err(|_| GpuRenderError::ReadbackChannelClosed)?
        .map_err(|err| GpuRenderError::MapFailed(err.to_string()))?;
    if let Some(receiver) = timestamp_receiver {
        receiver
            .recv()
            .map_err(|_| GpuRenderError::ReadbackChannelClosed)?
            .map_err(|err| GpuRenderError::MapFailed(err.to_string()))?;
    }

    let mapped = readback_buffer.slice(..).get_mapped_range();
    let output_u32 = bytemuck::cast_slice::<u8, u32>(&mapped).to_vec();
    drop(mapped);
    readback_buffer.unmap();
    let gpu_compute_ns = timestamp.map(|timestamp| mapped_timestamp_elapsed_ns(queue, timestamp));
    Ok((output_u32, gpu_compute_ns))
}

impl GpuRenderer {
    pub fn read_display_frame_rgba_for_diagnostics(
        &self,
        frame: &GpuDisplayFrame,
    ) -> Result<Vec<u8>, GpuRenderError> {
        let width =
            u32::try_from(frame.viewport.width).map_err(|_| GpuRenderError::BufferTooLarge {
                resource: "GPU display frame readback width",
                required_bytes: frame.viewport.width,
                limit_bytes: u64::from(u32::MAX),
            })?;
        let height =
            u32::try_from(frame.viewport.height).map_err(|_| GpuRenderError::BufferTooLarge {
                resource: "GPU display frame readback height",
                required_bytes: frame.viewport.height,
                limit_bytes: u64::from(u32::MAX),
            })?;
        let unpadded_bytes_per_row = width * 4;
        let padded_bytes_per_row = unpadded_bytes_per_row
            .div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer_size = u64::from(padded_bytes_per_row) * u64::from(height);
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-display-frame-diagnostic-readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-display-frame-diagnostic-readback-encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: frame.texture(),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let submission = self.queue.submit(Some(encoder.finish()));
        let (sender, receiver) = mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })
            .map_err(|err| GpuRenderError::PollFailed(err.to_string()))?;
        receiver
            .recv()
            .map_err(|_| GpuRenderError::ReadbackChannelClosed)?
            .map_err(|err| GpuRenderError::MapFailed(err.to_string()))?;

        let mapped = readback.slice(..).get_mapped_range();
        let mut rgba = Vec::with_capacity((u64::from(width) * u64::from(height) * 4) as usize);
        for row in 0..height as usize {
            let start = row * padded_bytes_per_row as usize;
            let end = start + unpadded_bytes_per_row as usize;
            rgba.extend_from_slice(&mapped[start..end]);
        }
        drop(mapped);
        readback.unmap();
        Ok(rgba)
    }

    pub(super) fn timestamp_query_pair(
        &self,
        label: &'static str,
    ) -> Option<GpuTimestampQueryPair> {
        timestamp_query_pair_for_device(&self.device, self.timestamp_queries_enabled, label)
    }

    pub(super) fn submit_with_optional_timestamp(
        &self,
        mut encoder: wgpu::CommandEncoder,
        timestamp: Option<GpuTimestampQueryPair>,
    ) -> Result<Option<u64>, GpuRenderError> {
        if let Some(timestamp) = timestamp {
            encoder.resolve_query_set(&timestamp.query_set, 0..2, &timestamp.resolve_buffer, 0);
            encoder.copy_buffer_to_buffer(
                &timestamp.resolve_buffer,
                0,
                &timestamp.readback_buffer,
                0,
                u64::from(wgpu::QUERY_SIZE) * 2,
            );
            let submission = self.queue.submit(Some(encoder.finish()));
            return self.read_timestamp_pair(timestamp, submission).map(Some);
        }
        self.queue.submit(Some(encoder.finish()));
        Ok(None)
    }

    fn read_timestamp_pair(
        &self,
        timestamp: GpuTimestampQueryPair,
        submission: wgpu::SubmissionIndex,
    ) -> Result<u64, GpuRenderError> {
        let (sender, receiver) = mpsc::channel();
        timestamp
            .readback_buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })
            .map_err(|err| GpuRenderError::PollFailed(err.to_string()))?;
        receiver
            .recv()
            .map_err(|_| GpuRenderError::ReadbackChannelClosed)?
            .map_err(|err| GpuRenderError::MapFailed(err.to_string()))?;

        Ok(mapped_timestamp_elapsed_ns(&self.queue, timestamp))
    }

    pub(super) fn submit_and_read_u32_with_optional_timestamp(
        &self,
        encoder: wgpu::CommandEncoder,
        readback_buffer: wgpu::Buffer,
        timestamp: Option<GpuTimestampQueryPair>,
    ) -> Result<(Vec<u32>, Option<u64>), GpuRenderError> {
        submit_and_read_u32_with_optional_timestamp_from_device(
            &self.device,
            &self.queue,
            encoder,
            readback_buffer,
            timestamp,
        )
    }
}

fn mapped_timestamp_elapsed_ns(queue: &wgpu::Queue, timestamp: GpuTimestampQueryPair) -> u64 {
    let mapped = timestamp.readback_buffer.slice(..).get_mapped_range();
    let timestamps = bytemuck::cast_slice::<u8, u64>(&mapped);
    let start = timestamps[0];
    let end = timestamps[1];
    let elapsed_ticks = end.saturating_sub(start);
    let timestamp_period_ns = f64::from(queue.get_timestamp_period());
    let elapsed_ns = ((elapsed_ticks as f64) * timestamp_period_ns)
        .round()
        .clamp(0.0, u64::MAX as f64) as u64;
    drop(mapped);
    timestamp.readback_buffer.unmap();
    elapsed_ns
}
