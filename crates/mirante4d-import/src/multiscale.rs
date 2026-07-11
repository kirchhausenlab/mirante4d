use super::*;

pub(super) fn build_mean_multiscale_specs(
    source_shape: Shape4D,
    source_grid_to_world: GridToWorld,
) -> Result<Vec<StreamingU16ScaleSpec>, ImportError> {
    build_mean_multiscale_specs_with_storage(
        source_shape,
        source_grid_to_world,
        TiffImportStorageOptions::default(),
    )
}

pub(super) fn build_mean_multiscale_specs_with_storage(
    source_shape: Shape4D,
    source_grid_to_world: GridToWorld,
    storage: TiffImportStorageOptions,
) -> Result<Vec<StreamingU16ScaleSpec>, ImportError> {
    let mut scales = Vec::new();
    scales.push(StreamingU16ScaleSpec {
        level: 0,
        shape: source_shape,
        brick_shape: import_chunk_shape_with_storage(source_shape, storage)?,
        grid_to_world: source_grid_to_world,
        source_scale: None,
        reduction: ScaleReduction::Source,
    });

    if !should_start_multiscale(source_shape) {
        return Ok(scales);
    }

    let mut cumulative_z_factor = 1.0;
    let mut cumulative_y_factor = 1.0;
    let mut cumulative_x_factor = 1.0;
    loop {
        let previous = scales.last().expect("s0 was inserted");
        if previous.shape.z == 1 && previous.shape.y == 1 && previous.shape.x == 1 {
            break;
        }
        let next_shape = Shape4D::new(
            previous.shape.t,
            previous.shape.z.div_ceil(2),
            previous.shape.y.div_ceil(2),
            previous.shape.x.div_ceil(2),
        )?;
        if previous.shape.z > 1 {
            cumulative_z_factor *= 2.0;
        }
        if previous.shape.y > 1 {
            cumulative_y_factor *= 2.0;
        }
        if previous.shape.x > 1 {
            cumulative_x_factor *= 2.0;
        }
        let level = scales.len() as u32;
        scales.push(StreamingU16ScaleSpec {
            level,
            shape: next_shape,
            brick_shape: import_chunk_shape_with_storage(next_shape, storage)?,
            grid_to_world: source_grid_to_world.downsampled_integer_centered(
                cumulative_x_factor as u64,
                cumulative_y_factor as u64,
                cumulative_z_factor as u64,
            )?,
            source_scale: Some(level - 1),
            reduction: ScaleReduction::Mean,
        });
        if is_terminal_multiscale_shape(next_shape) {
            break;
        }
    }
    Ok(scales)
}

pub(super) fn import_chunk_shape_with_storage(
    shape: Shape4D,
    storage: TiffImportStorageOptions,
) -> Result<Shape4D, ImportError> {
    if let Some(brick_shape) = storage.brick_shape_zyx {
        return Shape4D::new(
            1,
            shape.z.min(brick_shape.z),
            shape.y.min(brick_shape.y),
            shape.x.min(brick_shape.x),
        )
        .map_err(ImportError::from);
    }
    import_chunk_shape(shape)
}

pub(super) fn import_chunk_shape(shape: Shape4D) -> Result<Shape4D, ImportError> {
    if shape.z <= 1 {
        Shape4D::new(
            1,
            1,
            shape.y.min(IMPORT_2D_CHUNK_Y),
            shape.x.min(IMPORT_2D_CHUNK_X),
        )
        .map_err(ImportError::from)
    } else {
        Shape4D::new(
            1,
            shape.z.min(IMPORT_3D_CHUNK_Z),
            shape.y.min(IMPORT_3D_CHUNK_Y),
            shape.x.min(IMPORT_3D_CHUNK_X),
        )
        .map_err(ImportError::from)
    }
}

pub(super) fn should_start_multiscale(shape: Shape4D) -> bool {
    shape.z.max(shape.y).max(shape.x) > MULTISCALE_GENERATE_THRESHOLD
}

pub(super) fn is_terminal_multiscale_shape(shape: Shape4D) -> bool {
    shape.z.max(shape.y).max(shape.x) <= MULTISCALE_STOP_MAX_DIMENSION
        || spatial_voxels_per_timepoint(shape) <= MULTISCALE_STOP_VOXELS_PER_TIMEPOINT
}

pub(super) fn spatial_voxels_per_timepoint(shape: Shape4D) -> u64 {
    shape
        .z
        .checked_mul(shape.y)
        .and_then(|zy| zy.checked_mul(shape.x))
        .unwrap_or(u64::MAX)
}

pub(super) fn import_stored_bytes_per_voxel(dtype: IntensityDType) -> u64 {
    match dtype {
        IntensityDType::Uint8 => 1,
        IntensityDType::Uint16 => 2,
        IntensityDType::Float32 => 4,
    }
}

pub(super) fn checked_import_bytes(lhs: u64, rhs: u64) -> Result<u64, ImportError> {
    lhs.checked_mul(rhs)
        .ok_or(ImportError::StorageEstimateOverflow)
}

pub(super) fn checked_import_sum(lhs: u64, rhs: u64) -> Result<u64, ImportError> {
    lhs.checked_add(rhs)
        .ok_or(ImportError::StorageEstimateOverflow)
}

pub(super) fn write_stack_multiscales<F>(
    channel: u32,
    timepoint: u64,
    layer_writer: &mut StreamingU16LayerWriter,
    scale_specs: &[StreamingU16ScaleSpec],
    source_values_zyx: Vec<u16>,
    cancellation: &ImportCancellationToken,
    progress: &mut F,
) -> Result<(), ImportError>
where
    F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
{
    let mut values = source_values_zyx;
    for (index, scale) in scale_specs.iter().enumerate() {
        layer_writer.write_timepoint(scale.level, timepoint, &values)?;
        progress(ImportProgressEvent::BuiltScale {
            channel,
            level: scale.level,
        })?;
        check_import_cancelled(cancellation)?;
        if let Some(next_scale) = scale_specs.get(index + 1) {
            values = downsample_mean_u16_zyx(&values, scale.shape, next_scale.shape, cancellation)?;
        }
    }
    Ok(())
}

pub(super) struct PlaneSeriesWriteContext<'a> {
    pub(super) channel: u32,
    pub(super) expected_shape: TiffStackShape,
    pub(super) completed_inputs: &'a mut usize,
    pub(super) input_count: usize,
    pub(super) cancellation: &'a ImportCancellationToken,
}

pub(super) fn write_u16_plane_series_multiscales<F>(
    layer_writer: &mut StreamingU16LayerWriter,
    scale_specs: &[StreamingU16ScaleSpec],
    channel_inputs: Vec<TiffInput>,
    context: PlaneSeriesWriteContext<'_>,
    progress: &mut F,
) -> Result<(), ImportError>
where
    F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
{
    let plane_shape = TiffStackShape {
        z: 1,
        y: context.expected_shape.y,
        x: context.expected_shape.x,
    };
    let mut pyramid = U16PlaneSeriesPyramid::new(context.channel, layer_writer, scale_specs);
    for input in channel_inputs {
        check_import_cancelled(context.cancellation)?;
        let stack = read_checked_tiff_stack(&input.path, plane_shape, IntensityDType::Uint16)?;
        *context.completed_inputs += 1;
        progress(ImportProgressEvent::ReadStack {
            completed: *context.completed_inputs,
            total: context.input_count,
            path: input.path.clone(),
        })?;
        check_import_cancelled(context.cancellation)?;
        let values_zyx = match stack.values_zyx {
            TiffStackValues::U16(values) => values,
            other => {
                return Err(ImportError::SourceDTypeMismatch {
                    path: input.path,
                    actual: other.dtype(),
                    expected: IntensityDType::Uint16,
                });
            }
        };
        pyramid.push_plane(0, values_zyx, context.cancellation, progress)?;
    }
    pyramid.finish(context.cancellation, progress)
}

pub(super) struct U8StackMultiscaleWrite<'a> {
    pub(super) channel: u32,
    pub(super) timepoint: u64,
    pub(super) scale_specs: &'a [StreamingU8ScaleSpec],
    pub(super) no_data_policy: Option<TiffNoDataPolicyReview>,
    pub(super) cancellation: &'a ImportCancellationToken,
}

pub(super) fn write_u8_stack_multiscales<F>(
    context: U8StackMultiscaleWrite<'_>,
    layer_writer: &mut StreamingU8LayerWriter,
    source_values_zyx: Vec<u8>,
    progress: &mut F,
) -> Result<(), ImportError>
where
    F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
{
    let mut values = source_values_zyx;
    let mut render_valid = context.no_data_policy.map(|policy| {
        render_valid_mask_for_u8_sentinel(
            &values,
            context.scale_specs[0].shape,
            policy.source_value_uint8,
        )
    });
    for (index, scale) in context.scale_specs.iter().enumerate() {
        if let Some(mask) = &render_valid {
            layer_writer.write_timepoint_with_render_valid(
                scale.level,
                context.timepoint,
                &values,
                mask,
            )?;
        } else {
            layer_writer.write_timepoint(scale.level, context.timepoint, &values)?;
        }
        progress(ImportProgressEvent::BuiltScale {
            channel: context.channel,
            level: scale.level,
        })?;
        check_import_cancelled(context.cancellation)?;
        if let Some(next_scale) = context.scale_specs.get(index + 1) {
            if let Some(mask) = &render_valid {
                let (next_values, next_mask) = downsample_mean_u8_zyx_masked(
                    &values,
                    mask,
                    scale.shape,
                    next_scale.shape,
                    context.cancellation,
                )?;
                values = next_values;
                render_valid = Some(next_mask);
            } else {
                values = downsample_mean_u8_zyx(
                    &values,
                    scale.shape,
                    next_scale.shape,
                    context.cancellation,
                )?;
            }
        }
    }
    Ok(())
}

pub(super) fn write_u8_plane_series_multiscales<F>(
    layer_writer: &mut StreamingU8LayerWriter,
    scale_specs: &[StreamingU8ScaleSpec],
    channel_inputs: Vec<TiffInput>,
    no_data_policy: Option<TiffNoDataPolicyReview>,
    context: PlaneSeriesWriteContext<'_>,
    progress: &mut F,
) -> Result<(), ImportError>
where
    F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
{
    let plane_shape = TiffStackShape {
        z: 1,
        y: context.expected_shape.y,
        x: context.expected_shape.x,
    };
    if let Some(policy) = no_data_policy {
        let mut pyramid = MaskedU8PlaneSeriesPyramid::new(
            context.channel,
            layer_writer,
            scale_specs,
            policy.source_value_uint8,
        );
        for input in channel_inputs {
            check_import_cancelled(context.cancellation)?;
            let stack = read_checked_tiff_stack(&input.path, plane_shape, IntensityDType::Uint8)?;
            *context.completed_inputs += 1;
            progress(ImportProgressEvent::ReadStack {
                completed: *context.completed_inputs,
                total: context.input_count,
                path: input.path.clone(),
            })?;
            check_import_cancelled(context.cancellation)?;
            let values_zyx = match stack.values_zyx {
                TiffStackValues::U8(values) => values,
                other => {
                    return Err(ImportError::SourceDTypeMismatch {
                        path: input.path,
                        actual: other.dtype(),
                        expected: IntensityDType::Uint8,
                    });
                }
            };
            pyramid.push_source_plane(values_zyx, context.cancellation, progress)?;
        }
        return pyramid.finish(context.cancellation, progress);
    }

    let mut pyramid = U8PlaneSeriesPyramid::new(context.channel, layer_writer, scale_specs);
    for input in channel_inputs {
        check_import_cancelled(context.cancellation)?;
        let stack = read_checked_tiff_stack(&input.path, plane_shape, IntensityDType::Uint8)?;
        *context.completed_inputs += 1;
        progress(ImportProgressEvent::ReadStack {
            completed: *context.completed_inputs,
            total: context.input_count,
            path: input.path.clone(),
        })?;
        check_import_cancelled(context.cancellation)?;
        let values_zyx = match stack.values_zyx {
            TiffStackValues::U8(values) => values,
            other => {
                return Err(ImportError::SourceDTypeMismatch {
                    path: input.path,
                    actual: other.dtype(),
                    expected: IntensityDType::Uint8,
                });
            }
        };
        pyramid.push_plane(0, values_zyx, context.cancellation, progress)?;
    }
    pyramid.finish(context.cancellation, progress)
}

pub(super) fn write_f32_stack_multiscales<F>(
    channel: u32,
    timepoint: u64,
    layer_writer: &mut StreamingF32LayerWriter,
    scale_specs: &[StreamingF32ScaleSpec],
    source_values_zyx: Vec<f32>,
    cancellation: &ImportCancellationToken,
    progress: &mut F,
) -> Result<(), ImportError>
where
    F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
{
    let mut values = source_values_zyx;
    for (index, scale) in scale_specs.iter().enumerate() {
        layer_writer.write_timepoint(scale.level, timepoint, &values)?;
        progress(ImportProgressEvent::BuiltScale {
            channel,
            level: scale.level,
        })?;
        check_import_cancelled(cancellation)?;
        if let Some(next_scale) = scale_specs.get(index + 1) {
            values = downsample_mean_f32_zyx(&values, scale.shape, next_scale.shape, cancellation)?;
        }
    }
    Ok(())
}

pub(super) fn write_f32_plane_series_multiscales<F>(
    layer_writer: &mut StreamingF32LayerWriter,
    scale_specs: &[StreamingF32ScaleSpec],
    channel_inputs: Vec<TiffInput>,
    context: PlaneSeriesWriteContext<'_>,
    progress: &mut F,
) -> Result<(), ImportError>
where
    F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
{
    let plane_shape = TiffStackShape {
        z: 1,
        y: context.expected_shape.y,
        x: context.expected_shape.x,
    };
    let mut pyramid = F32PlaneSeriesPyramid::new(context.channel, layer_writer, scale_specs);
    for input in channel_inputs {
        check_import_cancelled(context.cancellation)?;
        let stack = read_checked_tiff_stack(&input.path, plane_shape, IntensityDType::Float32)?;
        *context.completed_inputs += 1;
        progress(ImportProgressEvent::ReadStack {
            completed: *context.completed_inputs,
            total: context.input_count,
            path: input.path.clone(),
        })?;
        check_import_cancelled(context.cancellation)?;
        let values_zyx = match stack.values_zyx {
            TiffStackValues::F32(values) => values,
            other => {
                return Err(ImportError::SourceDTypeMismatch {
                    path: input.path,
                    actual: other.dtype(),
                    expected: IntensityDType::Float32,
                });
            }
        };
        pyramid.push_plane(0, values_zyx, context.cancellation, progress)?;
    }
    pyramid.finish(context.cancellation, progress)
}

pub(super) struct U16PlaneSeriesPyramid<'a> {
    channel: u32,
    layer_writer: &'a mut StreamingU16LayerWriter,
    scale_specs: &'a [StreamingU16ScaleSpec],
    pending_planes: Vec<Option<Vec<u16>>>,
    written_z: Vec<u64>,
}

impl<'a> U16PlaneSeriesPyramid<'a> {
    fn new(
        channel: u32,
        layer_writer: &'a mut StreamingU16LayerWriter,
        scale_specs: &'a [StreamingU16ScaleSpec],
    ) -> Self {
        Self {
            channel,
            layer_writer,
            scale_specs,
            pending_planes: vec![None; scale_specs.len()],
            written_z: vec![0; scale_specs.len()],
        }
    }

    fn push_plane<F>(
        &mut self,
        level_index: usize,
        plane_yx: Vec<u16>,
        cancellation: &ImportCancellationToken,
        progress: &mut F,
    ) -> Result<(), ImportError>
    where
        F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
    {
        let scale = &self.scale_specs[level_index];
        let z = self.written_z[level_index];
        self.layer_writer
            .write_z_slab(scale.level, 0, z, &plane_yx)?;
        self.written_z[level_index] += 1;
        progress(ImportProgressEvent::BuiltScale {
            channel: self.channel,
            level: scale.level,
        })?;
        check_import_cancelled(cancellation)?;

        if level_index + 1 >= self.scale_specs.len() {
            return Ok(());
        }
        if let Some(previous) = self.pending_planes[level_index].take() {
            let next_plane = downsample_mean_u16_plane_pair(
                &previous,
                Some(&plane_yx),
                scale.shape,
                self.scale_specs[level_index + 1].shape,
                cancellation,
            )?;
            self.push_plane(level_index + 1, next_plane, cancellation, progress)?;
        } else {
            self.pending_planes[level_index] = Some(plane_yx);
        }
        Ok(())
    }

    fn finish<F>(
        &mut self,
        cancellation: &ImportCancellationToken,
        progress: &mut F,
    ) -> Result<(), ImportError>
    where
        F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
    {
        self.flush_level(0, cancellation, progress)
    }

    fn flush_level<F>(
        &mut self,
        level_index: usize,
        cancellation: &ImportCancellationToken,
        progress: &mut F,
    ) -> Result<(), ImportError>
    where
        F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
    {
        if level_index + 1 >= self.scale_specs.len() {
            return Ok(());
        }
        if let Some(plane) = self.pending_planes[level_index].take() {
            let next_plane = downsample_mean_u16_plane_pair(
                &plane,
                None,
                self.scale_specs[level_index].shape,
                self.scale_specs[level_index + 1].shape,
                cancellation,
            )?;
            self.push_plane(level_index + 1, next_plane, cancellation, progress)?;
        }
        self.flush_level(level_index + 1, cancellation, progress)
    }
}

pub(super) struct U8PlaneSeriesPyramid<'a> {
    channel: u32,
    layer_writer: &'a mut StreamingU8LayerWriter,
    scale_specs: &'a [StreamingU8ScaleSpec],
    pending_planes: Vec<Option<Vec<u8>>>,
    written_z: Vec<u64>,
}

impl<'a> U8PlaneSeriesPyramid<'a> {
    fn new(
        channel: u32,
        layer_writer: &'a mut StreamingU8LayerWriter,
        scale_specs: &'a [StreamingU8ScaleSpec],
    ) -> Self {
        Self {
            channel,
            layer_writer,
            scale_specs,
            pending_planes: vec![None; scale_specs.len()],
            written_z: vec![0; scale_specs.len()],
        }
    }

    fn push_plane<F>(
        &mut self,
        level_index: usize,
        plane_yx: Vec<u8>,
        cancellation: &ImportCancellationToken,
        progress: &mut F,
    ) -> Result<(), ImportError>
    where
        F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
    {
        let scale = &self.scale_specs[level_index];
        let z = self.written_z[level_index];
        self.layer_writer
            .write_z_slab(scale.level, 0, z, &plane_yx)?;
        self.written_z[level_index] += 1;
        progress(ImportProgressEvent::BuiltScale {
            channel: self.channel,
            level: scale.level,
        })?;
        check_import_cancelled(cancellation)?;

        if level_index + 1 >= self.scale_specs.len() {
            return Ok(());
        }
        if let Some(previous) = self.pending_planes[level_index].take() {
            let next_plane = downsample_mean_u8_plane_pair(
                &previous,
                Some(&plane_yx),
                scale.shape,
                self.scale_specs[level_index + 1].shape,
                cancellation,
            )?;
            self.push_plane(level_index + 1, next_plane, cancellation, progress)?;
        } else {
            self.pending_planes[level_index] = Some(plane_yx);
        }
        Ok(())
    }

    fn finish<F>(
        &mut self,
        cancellation: &ImportCancellationToken,
        progress: &mut F,
    ) -> Result<(), ImportError>
    where
        F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
    {
        self.flush_level(0, cancellation, progress)
    }

    fn flush_level<F>(
        &mut self,
        level_index: usize,
        cancellation: &ImportCancellationToken,
        progress: &mut F,
    ) -> Result<(), ImportError>
    where
        F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
    {
        if level_index + 1 >= self.scale_specs.len() {
            return Ok(());
        }
        if let Some(plane) = self.pending_planes[level_index].take() {
            let next_plane = downsample_mean_u8_plane_pair(
                &plane,
                None,
                self.scale_specs[level_index].shape,
                self.scale_specs[level_index + 1].shape,
                cancellation,
            )?;
            self.push_plane(level_index + 1, next_plane, cancellation, progress)?;
        }
        self.flush_level(level_index + 1, cancellation, progress)
    }
}

pub(super) struct MaskedU8RawPlane {
    values: Vec<u8>,
    source_valid: Vec<u8>,
}

pub(super) struct MaskedU8RenderedPlane {
    values: Vec<u8>,
    render_valid: Vec<u8>,
}

#[derive(Default)]
pub(super) struct MaskedU8PlaneSeriesLevelState {
    previous_raw_plane: Option<MaskedU8RawPlane>,
    previous_previous_source_valid: Option<Vec<u8>>,
    pending_downsample: Option<MaskedU8RenderedPlane>,
    written_z: u64,
}

pub(super) struct MaskedU8PlaneSeriesPyramid<'a> {
    channel: u32,
    layer_writer: &'a mut StreamingU8LayerWriter,
    scale_specs: &'a [StreamingU8ScaleSpec],
    sentinel: u8,
    levels: Vec<MaskedU8PlaneSeriesLevelState>,
}

impl<'a> MaskedU8PlaneSeriesPyramid<'a> {
    fn new(
        channel: u32,
        layer_writer: &'a mut StreamingU8LayerWriter,
        scale_specs: &'a [StreamingU8ScaleSpec],
        sentinel: u8,
    ) -> Self {
        Self {
            channel,
            layer_writer,
            scale_specs,
            sentinel,
            levels: (0..scale_specs.len())
                .map(|_| MaskedU8PlaneSeriesLevelState::default())
                .collect(),
        }
    }

    fn push_source_plane<F>(
        &mut self,
        values: Vec<u8>,
        cancellation: &ImportCancellationToken,
        progress: &mut F,
    ) -> Result<(), ImportError>
    where
        F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
    {
        let source_valid = values
            .iter()
            .map(|value| u8::from(*value != self.sentinel))
            .collect::<Vec<_>>();
        self.push_raw_plane(0, values, source_valid, cancellation, progress)
    }

    fn push_raw_plane<F>(
        &mut self,
        level_index: usize,
        values: Vec<u8>,
        source_valid: Vec<u8>,
        cancellation: &ImportCancellationToken,
        progress: &mut F,
    ) -> Result<(), ImportError>
    where
        F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
    {
        let previous = self.levels[level_index].previous_raw_plane.take();
        if let Some(previous) = previous {
            let previous_previous = self.levels[level_index]
                .previous_previous_source_valid
                .take();
            let render_valid = render_valid_plane_after_one_voxel_invalid_dilation(
                previous_previous.as_deref(),
                &previous.source_valid,
                Some(&source_valid),
                self.scale_specs[level_index].shape,
            )?;
            self.levels[level_index].previous_previous_source_valid = Some(previous.source_valid);
            self.levels[level_index].previous_raw_plane = Some(MaskedU8RawPlane {
                values,
                source_valid,
            });
            self.flush_rendered_plane(
                level_index,
                previous.values,
                render_valid,
                cancellation,
                progress,
            )?;
        } else {
            self.levels[level_index].previous_raw_plane = Some(MaskedU8RawPlane {
                values,
                source_valid,
            });
        }
        Ok(())
    }

    fn flush_rendered_plane<F>(
        &mut self,
        level_index: usize,
        values: Vec<u8>,
        render_valid: Vec<u8>,
        cancellation: &ImportCancellationToken,
        progress: &mut F,
    ) -> Result<(), ImportError>
    where
        F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
    {
        let scale = &self.scale_specs[level_index];
        let z = self.levels[level_index].written_z;
        self.layer_writer.write_z_slab_with_render_valid(
            scale.level,
            0,
            z,
            &values,
            &render_valid,
        )?;
        self.levels[level_index].written_z += 1;
        progress(ImportProgressEvent::BuiltScale {
            channel: self.channel,
            level: scale.level,
        })?;
        check_import_cancelled(cancellation)?;

        if level_index + 1 >= self.scale_specs.len() {
            return Ok(());
        }
        let rendered = MaskedU8RenderedPlane {
            values,
            render_valid,
        };
        if let Some(previous) = self.levels[level_index].pending_downsample.take() {
            let (next_values, next_source_valid) = downsample_mean_u8_rendered_plane_pair(
                &previous.values,
                Some(&rendered.values),
                &previous.render_valid,
                Some(&rendered.render_valid),
                scale.shape,
                self.scale_specs[level_index + 1].shape,
                cancellation,
            )?;
            self.push_raw_plane(
                level_index + 1,
                next_values,
                next_source_valid,
                cancellation,
                progress,
            )?;
        } else {
            self.levels[level_index].pending_downsample = Some(rendered);
        }
        Ok(())
    }

    fn finish<F>(
        &mut self,
        cancellation: &ImportCancellationToken,
        progress: &mut F,
    ) -> Result<(), ImportError>
    where
        F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
    {
        self.finish_level(0, cancellation, progress)
    }

    fn finish_level<F>(
        &mut self,
        level_index: usize,
        cancellation: &ImportCancellationToken,
        progress: &mut F,
    ) -> Result<(), ImportError>
    where
        F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
    {
        if let Some(previous) = self.levels[level_index].previous_raw_plane.take() {
            let previous_previous = self.levels[level_index]
                .previous_previous_source_valid
                .take();
            let render_valid = render_valid_plane_after_one_voxel_invalid_dilation(
                previous_previous.as_deref(),
                &previous.source_valid,
                None,
                self.scale_specs[level_index].shape,
            )?;
            self.flush_rendered_plane(
                level_index,
                previous.values,
                render_valid,
                cancellation,
                progress,
            )?;
        }

        if level_index + 1 >= self.scale_specs.len() {
            return Ok(());
        }
        if let Some(previous) = self.levels[level_index].pending_downsample.take() {
            let (next_values, next_source_valid) = downsample_mean_u8_rendered_plane_pair(
                &previous.values,
                None,
                &previous.render_valid,
                None,
                self.scale_specs[level_index].shape,
                self.scale_specs[level_index + 1].shape,
                cancellation,
            )?;
            self.push_raw_plane(
                level_index + 1,
                next_values,
                next_source_valid,
                cancellation,
                progress,
            )?;
        }
        self.finish_level(level_index + 1, cancellation, progress)
    }
}

pub(super) struct F32PlaneSeriesPyramid<'a> {
    channel: u32,
    layer_writer: &'a mut StreamingF32LayerWriter,
    scale_specs: &'a [StreamingF32ScaleSpec],
    pending_planes: Vec<Option<Vec<f32>>>,
    written_z: Vec<u64>,
}

impl<'a> F32PlaneSeriesPyramid<'a> {
    fn new(
        channel: u32,
        layer_writer: &'a mut StreamingF32LayerWriter,
        scale_specs: &'a [StreamingF32ScaleSpec],
    ) -> Self {
        Self {
            channel,
            layer_writer,
            scale_specs,
            pending_planes: vec![None; scale_specs.len()],
            written_z: vec![0; scale_specs.len()],
        }
    }

    fn push_plane<F>(
        &mut self,
        level_index: usize,
        plane_yx: Vec<f32>,
        cancellation: &ImportCancellationToken,
        progress: &mut F,
    ) -> Result<(), ImportError>
    where
        F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
    {
        let scale = &self.scale_specs[level_index];
        let z = self.written_z[level_index];
        self.layer_writer
            .write_z_slab(scale.level, 0, z, &plane_yx)?;
        self.written_z[level_index] += 1;
        progress(ImportProgressEvent::BuiltScale {
            channel: self.channel,
            level: scale.level,
        })?;
        check_import_cancelled(cancellation)?;

        if level_index + 1 >= self.scale_specs.len() {
            return Ok(());
        }
        if let Some(previous) = self.pending_planes[level_index].take() {
            let next_plane = downsample_mean_f32_plane_pair(
                &previous,
                Some(&plane_yx),
                scale.shape,
                self.scale_specs[level_index + 1].shape,
                cancellation,
            )?;
            self.push_plane(level_index + 1, next_plane, cancellation, progress)?;
        } else {
            self.pending_planes[level_index] = Some(plane_yx);
        }
        Ok(())
    }

    fn finish<F>(
        &mut self,
        cancellation: &ImportCancellationToken,
        progress: &mut F,
    ) -> Result<(), ImportError>
    where
        F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
    {
        self.flush_level(0, cancellation, progress)
    }

    fn flush_level<F>(
        &mut self,
        level_index: usize,
        cancellation: &ImportCancellationToken,
        progress: &mut F,
    ) -> Result<(), ImportError>
    where
        F: FnMut(ImportProgressEvent) -> Result<(), ImportError>,
    {
        if level_index + 1 >= self.scale_specs.len() {
            return Ok(());
        }
        if let Some(plane) = self.pending_planes[level_index].take() {
            let next_plane = downsample_mean_f32_plane_pair(
                &plane,
                None,
                self.scale_specs[level_index].shape,
                self.scale_specs[level_index + 1].shape,
                cancellation,
            )?;
            self.push_plane(level_index + 1, next_plane, cancellation, progress)?;
        }
        self.flush_level(level_index + 1, cancellation, progress)
    }
}

pub(super) fn u8_scale_specs_from_u16(
    scales: &[StreamingU16ScaleSpec],
) -> Vec<StreamingU8ScaleSpec> {
    scales
        .iter()
        .map(|scale| StreamingU8ScaleSpec {
            level: scale.level,
            shape: scale.shape,
            brick_shape: scale.brick_shape,
            grid_to_world: scale.grid_to_world,
            source_scale: scale.source_scale,
            reduction: scale.reduction,
        })
        .collect()
}

pub(super) fn f32_scale_specs_from_u16(
    scales: &[StreamingU16ScaleSpec],
) -> Vec<StreamingF32ScaleSpec> {
    scales
        .iter()
        .map(|scale| StreamingF32ScaleSpec {
            level: scale.level,
            shape: scale.shape,
            brick_shape: scale.brick_shape,
            grid_to_world: scale.grid_to_world,
            source_scale: scale.source_scale,
            reduction: scale.reduction,
        })
        .collect()
}

pub(super) fn default_u8_display() -> LayerDisplay {
    LayerDisplay::new(true, DisplayWindow::new(0.0, 255.0).unwrap(), 1.0).unwrap()
}

pub(super) fn display_from_statistics(
    statistics: &Statistics,
) -> Result<LayerDisplay, ImportError> {
    let low = statistics.percentiles.p0_1 as f32;
    let mut high = statistics.percentiles.p99_9 as f32;
    if high <= low {
        high = (low + 1.0).min(f32::from(u16::MAX));
    }
    if high <= low {
        high = low + 1.0;
    }
    Ok(LayerDisplay::new(
        true,
        DisplayWindow::new(low, high)?,
        1.0,
    )?)
}

pub(super) fn downsample_mean_u16_zyx(
    source: &[u16],
    source_shape: Shape4D,
    output_shape: Shape4D,
    cancellation: &ImportCancellationToken,
) -> Result<Vec<u16>, ImportError> {
    let output_len =
        Shape4D::new(1, output_shape.z, output_shape.y, output_shape.x)?.element_count()? as usize;
    let mut output = Vec::with_capacity(output_len);
    for z in 0..output_shape.z {
        check_import_cancelled(cancellation)?;
        for y in 0..output_shape.y {
            for x in 0..output_shape.x {
                output.push(downsample_mean_voxel_zyx(
                    source,
                    source_shape,
                    z * 2,
                    y * 2,
                    x * 2,
                ));
            }
        }
    }
    Ok(output)
}

pub(super) fn downsample_mean_f32_zyx(
    source: &[f32],
    source_shape: Shape4D,
    output_shape: Shape4D,
    cancellation: &ImportCancellationToken,
) -> Result<Vec<f32>, ImportError> {
    let output_len =
        Shape4D::new(1, output_shape.z, output_shape.y, output_shape.x)?.element_count()? as usize;
    let mut output = Vec::with_capacity(output_len);
    for z in 0..output_shape.z {
        check_import_cancelled(cancellation)?;
        for y in 0..output_shape.y {
            for x in 0..output_shape.x {
                output.push(downsample_mean_f32_voxel_zyx(
                    source,
                    source_shape,
                    z * 2,
                    y * 2,
                    x * 2,
                ));
            }
        }
    }
    Ok(output)
}

pub(super) fn downsample_mean_u8_zyx(
    source: &[u8],
    source_shape: Shape4D,
    output_shape: Shape4D,
    cancellation: &ImportCancellationToken,
) -> Result<Vec<u8>, ImportError> {
    let output_len =
        Shape4D::new(1, output_shape.z, output_shape.y, output_shape.x)?.element_count()? as usize;
    let mut output = Vec::with_capacity(output_len);
    for z in 0..output_shape.z {
        check_import_cancelled(cancellation)?;
        for y in 0..output_shape.y {
            for x in 0..output_shape.x {
                output.push(downsample_mean_u8_voxel_zyx(
                    source,
                    source_shape,
                    z * 2,
                    y * 2,
                    x * 2,
                ));
            }
        }
    }
    Ok(output)
}

pub(super) fn render_valid_mask_for_u8_sentinel(
    source: &[u8],
    shape: Shape4D,
    sentinel: u8,
) -> Vec<u8> {
    let source_valid = source
        .iter()
        .map(|value| u8::from(*value != sentinel))
        .collect::<Vec<_>>();
    render_valid_after_one_voxel_invalid_dilation(&source_valid, shape)
}

pub(super) fn render_valid_after_one_voxel_invalid_dilation(
    source_valid: &[u8],
    shape: Shape4D,
) -> Vec<u8> {
    let mut render_valid = vec![1_u8; source_valid.len()];
    let z_radius = if shape.z > 1 { 1_i64 } else { 0_i64 };
    for z in 0..shape.z {
        for y in 0..shape.y {
            for x in 0..shape.x {
                let index = ((z * shape.y + y) * shape.x + x) as usize;
                if source_valid[index] == 1 {
                    continue;
                }
                for dz in -z_radius..=z_radius {
                    for dy in -1_i64..=1 {
                        for dx in -1_i64..=1 {
                            let zz = z as i64 + dz;
                            let yy = y as i64 + dy;
                            let xx = x as i64 + dx;
                            if zz < 0
                                || yy < 0
                                || xx < 0
                                || zz >= shape.z as i64
                                || yy >= shape.y as i64
                                || xx >= shape.x as i64
                            {
                                continue;
                            }
                            let invalidated =
                                ((zz as u64 * shape.y + yy as u64) * shape.x + xx as u64) as usize;
                            render_valid[invalidated] = 0;
                        }
                    }
                }
            }
        }
    }
    render_valid
}

pub(super) fn render_valid_plane_after_one_voxel_invalid_dilation(
    previous_source_valid_yx: Option<&[u8]>,
    current_source_valid_yx: &[u8],
    next_source_valid_yx: Option<&[u8]>,
    shape: Shape4D,
) -> Result<Vec<u8>, ImportError> {
    let plane_len = usize::try_from(
        shape
            .y
            .checked_mul(shape.x)
            .ok_or(ImportError::StorageEstimateOverflow)?,
    )
    .map_err(|_| ImportError::StorageEstimateOverflow)?;
    if current_source_valid_yx.len() != plane_len {
        return Err(ImportError::StorageEstimateOverflow);
    }
    let mut render_valid = vec![1_u8; plane_len];
    if shape.z > 1 {
        if let Some(previous) = previous_source_valid_yx {
            invalidate_render_valid_plane_from_source_valid(&mut render_valid, previous, shape)?;
        }
        if let Some(next) = next_source_valid_yx {
            invalidate_render_valid_plane_from_source_valid(&mut render_valid, next, shape)?;
        }
    }
    invalidate_render_valid_plane_from_source_valid(
        &mut render_valid,
        current_source_valid_yx,
        shape,
    )?;
    Ok(render_valid)
}

pub(super) fn invalidate_render_valid_plane_from_source_valid(
    render_valid_yx: &mut [u8],
    source_valid_yx: &[u8],
    shape: Shape4D,
) -> Result<(), ImportError> {
    let plane_len = usize::try_from(
        shape
            .y
            .checked_mul(shape.x)
            .ok_or(ImportError::StorageEstimateOverflow)?,
    )
    .map_err(|_| ImportError::StorageEstimateOverflow)?;
    if source_valid_yx.len() != plane_len || render_valid_yx.len() != plane_len {
        return Err(ImportError::StorageEstimateOverflow);
    }
    for y in 0..shape.y {
        for x in 0..shape.x {
            let index = (y * shape.x + x) as usize;
            if source_valid_yx[index] == 1 {
                continue;
            }
            for yy in y.saturating_sub(1)..=(y + 1).min(shape.y - 1) {
                for xx in x.saturating_sub(1)..=(x + 1).min(shape.x - 1) {
                    render_valid_yx[(yy * shape.x + xx) as usize] = 0;
                }
            }
        }
    }
    Ok(())
}

pub(super) fn downsample_mean_u8_zyx_masked(
    source: &[u8],
    source_render_valid: &[u8],
    source_shape: Shape4D,
    output_shape: Shape4D,
    cancellation: &ImportCancellationToken,
) -> Result<(Vec<u8>, Vec<u8>), ImportError> {
    let output_len =
        Shape4D::new(1, output_shape.z, output_shape.y, output_shape.x)?.element_count()? as usize;
    let mut output = Vec::with_capacity(output_len);
    let mut support_valid = Vec::with_capacity(output_len);
    for z in 0..output_shape.z {
        check_import_cancelled(cancellation)?;
        for y in 0..output_shape.y {
            for x in 0..output_shape.x {
                let (value, has_support) = downsample_mean_u8_masked_voxel_zyx(
                    source,
                    source_render_valid,
                    source_shape,
                    z * 2,
                    y * 2,
                    x * 2,
                );
                output.push(value);
                support_valid.push(u8::from(has_support));
            }
        }
    }
    let render_valid = render_valid_after_one_voxel_invalid_dilation(&support_valid, output_shape);
    Ok((output, render_valid))
}

pub(super) fn downsample_mean_voxel_zyx(
    source: &[u16],
    source_shape: Shape4D,
    z_start: u64,
    y_start: u64,
    x_start: u64,
) -> u16 {
    let mut sum = 0u64;
    let mut count = 0u64;
    for z in z_start..(z_start + 2).min(source_shape.z) {
        for y in y_start..(y_start + 2).min(source_shape.y) {
            for x in x_start..(x_start + 2).min(source_shape.x) {
                let index = ((z * source_shape.y + y) * source_shape.x + x) as usize;
                sum += u64::from(source[index]);
                count += 1;
            }
        }
    }
    ((sum + count / 2) / count) as u16
}

pub(super) fn downsample_mean_f32_voxel_zyx(
    source: &[f32],
    source_shape: Shape4D,
    z_start: u64,
    y_start: u64,
    x_start: u64,
) -> f32 {
    let mut sum = 0.0_f64;
    let mut count = 0u64;
    for z in z_start..(z_start + 2).min(source_shape.z) {
        for y in y_start..(y_start + 2).min(source_shape.y) {
            for x in x_start..(x_start + 2).min(source_shape.x) {
                let index = ((z * source_shape.y + y) * source_shape.x + x) as usize;
                sum += f64::from(source[index]);
                count += 1;
            }
        }
    }
    (sum / count as f64) as f32
}

pub(super) fn downsample_mean_u8_voxel_zyx(
    source: &[u8],
    source_shape: Shape4D,
    z0: u64,
    y0: u64,
    x0: u64,
) -> u8 {
    let mut sum = 0u64;
    let mut count = 0u64;
    for z in z0..(z0 + 2).min(source_shape.z) {
        for y in y0..(y0 + 2).min(source_shape.y) {
            for x in x0..(x0 + 2).min(source_shape.x) {
                sum += u64::from(source[((z * source_shape.y + y) * source_shape.x + x) as usize]);
                count += 1;
            }
        }
    }
    let rounded = (sum + count / 2) / count;
    u8::try_from(rounded).unwrap_or(u8::MAX)
}

pub(super) fn downsample_mean_u8_masked_voxel_zyx(
    source: &[u8],
    source_render_valid: &[u8],
    source_shape: Shape4D,
    z0: u64,
    y0: u64,
    x0: u64,
) -> (u8, bool) {
    let mut sum = 0u64;
    let mut count = 0u64;
    for z in z0..(z0 + 2).min(source_shape.z) {
        for y in y0..(y0 + 2).min(source_shape.y) {
            for x in x0..(x0 + 2).min(source_shape.x) {
                let index = ((z * source_shape.y + y) * source_shape.x + x) as usize;
                if source_render_valid[index] != 1 {
                    continue;
                }
                sum += u64::from(source[index]);
                count += 1;
            }
        }
    }
    let Some(rounded) = (sum + count / 2).checked_div(count) else {
        return (0, false);
    };
    (u8::try_from(rounded).unwrap_or(u8::MAX), true)
}

pub(super) fn downsample_mean_u16_plane_pair(
    first_yx: &[u16],
    second_yx: Option<&[u16]>,
    source_shape: Shape4D,
    output_shape: Shape4D,
    cancellation: &ImportCancellationToken,
) -> Result<Vec<u16>, ImportError> {
    let output_len = output_shape
        .y
        .checked_mul(output_shape.x)
        .ok_or(ImportError::StorageEstimateOverflow)? as usize;
    let mut output = Vec::with_capacity(output_len);
    for y in 0..output_shape.y {
        check_import_cancelled(cancellation)?;
        for x in 0..output_shape.x {
            let mut sum = 0u64;
            let mut count = 0u64;
            for plane in [Some(first_yx), second_yx].into_iter().flatten() {
                for yy in y * 2..(y * 2 + 2).min(source_shape.y) {
                    for xx in x * 2..(x * 2 + 2).min(source_shape.x) {
                        sum += u64::from(plane[(yy * source_shape.x + xx) as usize]);
                        count += 1;
                    }
                }
            }
            output.push(((sum + count / 2) / count) as u16);
        }
    }
    Ok(output)
}

pub(super) fn downsample_mean_u8_plane_pair(
    first_yx: &[u8],
    second_yx: Option<&[u8]>,
    source_shape: Shape4D,
    output_shape: Shape4D,
    cancellation: &ImportCancellationToken,
) -> Result<Vec<u8>, ImportError> {
    let output_len = output_shape
        .y
        .checked_mul(output_shape.x)
        .ok_or(ImportError::StorageEstimateOverflow)? as usize;
    let mut output = Vec::with_capacity(output_len);
    for y in 0..output_shape.y {
        check_import_cancelled(cancellation)?;
        for x in 0..output_shape.x {
            let mut sum = 0u64;
            let mut count = 0u64;
            for plane in [Some(first_yx), second_yx].into_iter().flatten() {
                for yy in y * 2..(y * 2 + 2).min(source_shape.y) {
                    for xx in x * 2..(x * 2 + 2).min(source_shape.x) {
                        sum += u64::from(plane[(yy * source_shape.x + xx) as usize]);
                        count += 1;
                    }
                }
            }
            let rounded = (sum + count / 2) / count;
            output.push(u8::try_from(rounded).unwrap_or(u8::MAX));
        }
    }
    Ok(output)
}

pub(super) fn downsample_mean_u8_rendered_plane_pair(
    first_yx: &[u8],
    second_yx: Option<&[u8]>,
    first_render_valid_yx: &[u8],
    second_render_valid_yx: Option<&[u8]>,
    source_shape: Shape4D,
    output_shape: Shape4D,
    cancellation: &ImportCancellationToken,
) -> Result<(Vec<u8>, Vec<u8>), ImportError> {
    let output_len = output_shape
        .y
        .checked_mul(output_shape.x)
        .ok_or(ImportError::StorageEstimateOverflow)? as usize;
    let mut output = Vec::with_capacity(output_len);
    let mut support_valid = Vec::with_capacity(output_len);
    for y in 0..output_shape.y {
        check_import_cancelled(cancellation)?;
        for x in 0..output_shape.x {
            let mut sum = 0u64;
            let mut count = 0u64;
            for (plane, render_valid) in [
                Some((first_yx, first_render_valid_yx)),
                second_yx.zip(second_render_valid_yx),
            ]
            .into_iter()
            .flatten()
            {
                for yy in y * 2..(y * 2 + 2).min(source_shape.y) {
                    for xx in x * 2..(x * 2 + 2).min(source_shape.x) {
                        let source_index = (yy * source_shape.x + xx) as usize;
                        if render_valid[source_index] != 1 {
                            continue;
                        }
                        sum += u64::from(plane[source_index]);
                        count += 1;
                    }
                }
            }
            if let Some(rounded) = (sum + count / 2).checked_div(count) {
                output.push(u8::try_from(rounded).unwrap_or(u8::MAX));
                support_valid.push(1);
            } else {
                output.push(0);
                support_valid.push(0);
            }
        }
    }
    Ok((output, support_valid))
}

pub(super) fn downsample_mean_f32_plane_pair(
    first_yx: &[f32],
    second_yx: Option<&[f32]>,
    source_shape: Shape4D,
    output_shape: Shape4D,
    cancellation: &ImportCancellationToken,
) -> Result<Vec<f32>, ImportError> {
    let output_len = output_shape
        .y
        .checked_mul(output_shape.x)
        .ok_or(ImportError::StorageEstimateOverflow)? as usize;
    let mut output = Vec::with_capacity(output_len);
    for y in 0..output_shape.y {
        check_import_cancelled(cancellation)?;
        for x in 0..output_shape.x {
            let mut sum = 0.0_f64;
            let mut count = 0u64;
            for plane in [Some(first_yx), second_yx].into_iter().flatten() {
                for yy in y * 2..(y * 2 + 2).min(source_shape.y) {
                    for xx in x * 2..(x * 2 + 2).min(source_shape.x) {
                        sum += f64::from(plane[(yy * source_shape.x + xx) as usize]);
                        count += 1;
                    }
                }
            }
            output.push((sum / count as f64) as f32);
        }
    }
    Ok(output)
}
