use super::*;

pub struct StreamingU16LayerWriter {
    id: String,
    name: String,
    channel: ChannelMetadata,
    source_dtype: IntensityDType,
    shape: Shape4D,
    grid_to_world: GridToWorld,
    display: LayerDisplay,
    scales: Vec<StreamingU16ScaleState>,
}

impl StreamingU16LayerWriter {
    pub(super) fn create(
        store: ReadableWritableListableStorage,
        spec: StreamingU16LayerSpec,
    ) -> Result<Self, FormatError> {
        validate_streaming_layer_scales(
            spec.id.as_str(),
            spec.shape,
            spec.grid_to_world,
            &spec.scales,
        )?;

        let mut scales = Vec::with_capacity(spec.scales.len());
        for scale in spec.scales {
            let array_path = format!("arrays/intensity/{}/s{}", spec.id, scale.level);
            let array = create_u16_array(&store, &array_path, scale.shape, scale.brick_shape)?;
            let brick_grid = scale.shape.chunk_grid(scale.brick_shape)?;
            scales.push(StreamingU16ScaleState {
                level: scale.level,
                array_path,
                array,
                shape: scale.shape,
                brick_shape: scale.brick_shape,
                grid_to_world: scale.grid_to_world,
                source_scale: scale.source_scale,
                reduction: scale.reduction,
                written_z_planes: vec![vec![false; scale.shape.z as usize]; scale.shape.t as usize],
                statistics: U16StatisticsAccumulator::new(),
                brick_grid,
                brick_statistics: vec![
                    BrickStatisticsAccumulator::new();
                    brick_grid.element_count()? as usize
                ],
            });
        }

        Ok(Self {
            id: spec.id,
            name: spec.name,
            channel: spec.channel,
            source_dtype: spec.source_dtype,
            shape: spec.shape,
            grid_to_world: spec.grid_to_world,
            display: spec.display,
            scales,
        })
    }

    pub fn set_display(&mut self, display: LayerDisplay) {
        self.display = display;
    }

    pub fn scale_statistics(&self, level: u32) -> Result<Statistics, FormatError> {
        let scale = self.scale(level)?;
        Ok(scale.statistics.finish())
    }

    pub fn write_timepoint(
        &mut self,
        level: u32,
        timepoint: u64,
        values_zyx: &[u16],
    ) -> Result<(), FormatError> {
        let layer_id = self.id.clone();
        let scale = self.scale_mut(level)?;
        scale.write_timepoint(layer_id.as_str(), timepoint, values_zyx)
    }

    pub fn write_z_slab(
        &mut self,
        level: u32,
        timepoint: u64,
        z_start: u64,
        values_zyx: &[u16],
    ) -> Result<(), FormatError> {
        let layer_id = self.id.clone();
        let scale = self.scale_mut(level)?;
        scale.write_z_slab(layer_id.as_str(), timepoint, z_start, values_zyx)
    }

    fn scale(&self, level: u32) -> Result<&StreamingU16ScaleState, FormatError> {
        self.scales.iter().find(|scale| scale.level == level).ok_or(
            FormatError::InvalidScaleLevel {
                layer_id: self.id.clone(),
                level,
            },
        )
    }

    fn scale_mut(&mut self, level: u32) -> Result<&mut StreamingU16ScaleState, FormatError> {
        self.scales
            .iter_mut()
            .find(|scale| scale.level == level)
            .ok_or(FormatError::InvalidScaleLevel {
                layer_id: self.id.clone(),
                level,
            })
    }

    pub(super) fn finish(self) -> Result<LayerManifest, FormatError> {
        let mut scales = Vec::with_capacity(self.scales.len());
        for scale in self.scales {
            scales.push(scale.finish(self.id.as_str())?);
        }

        Ok(LayerManifest {
            id: self.id,
            kind: LayerKind::DenseIntensity,
            name: self.name,
            channel: self.channel,
            shape: self.shape,
            dtype: DTypeMetadata {
                source: self.source_dtype,
                stored: IntensityDType::Uint16,
                conversion: DTypeConversion::Lossless,
            },
            grid_to_world: self.grid_to_world,
            display: self.display,
            scales,
            no_data_policy: None,
        })
    }
}

struct StreamingU16ScaleState {
    level: u32,
    array_path: String,
    array: ZarrArray,
    shape: Shape4D,
    brick_shape: Shape4D,
    grid_to_world: GridToWorld,
    source_scale: Option<u32>,
    reduction: ScaleReduction,
    written_z_planes: Vec<Vec<bool>>,
    statistics: U16StatisticsAccumulator,
    brick_grid: Shape4D,
    brick_statistics: Vec<BrickStatisticsAccumulator>,
}

impl StreamingU16ScaleState {
    fn write_timepoint(
        &mut self,
        layer_id: &str,
        timepoint: u64,
        values_zyx: &[u16],
    ) -> Result<(), FormatError> {
        if timepoint >= self.shape.t {
            return Err(FormatError::InvalidTimepoint {
                layer_id: layer_id.to_owned(),
                level: self.level,
                timepoint,
            });
        }
        if self.written_z_planes[timepoint as usize]
            .iter()
            .any(|written| *written)
        {
            return Err(FormatError::DuplicateTimepointWrite {
                layer_id: layer_id.to_owned(),
                level: self.level,
                timepoint,
            });
        }

        let expected =
            Shape4D::new(1, self.shape.z, self.shape.y, self.shape.x)?.element_count()? as usize;
        let actual = values_zyx.len();
        if actual != expected {
            return Err(FormatError::InvalidLayerValues {
                layer_id: layer_id.to_owned(),
                actual,
                expected,
            });
        }

        let subset = ArraySubset::new_with_ranges(&[
            timepoint..timepoint + 1,
            0..self.shape.z,
            0..self.shape.y,
            0..self.shape.x,
        ]);
        self.array
            .store_array_subset_opt(&subset, values_zyx, &store_all_chunks_options())
            .map_err(zarr_storage_error)?;

        self.statistics.observe(values_zyx);
        self.observe_brick_statistics(timepoint, values_zyx);
        for written in &mut self.written_z_planes[timepoint as usize] {
            *written = true;
        }
        Ok(())
    }

    fn write_z_slab(
        &mut self,
        layer_id: &str,
        timepoint: u64,
        z_start: u64,
        values_zyx: &[u16],
    ) -> Result<(), FormatError> {
        if timepoint >= self.shape.t {
            return Err(FormatError::InvalidTimepoint {
                layer_id: layer_id.to_owned(),
                level: self.level,
                timepoint,
            });
        }
        let plane_voxels =
            self.shape
                .y
                .checked_mul(self.shape.x)
                .ok_or_else(|| FormatError::ZarrStorage {
                    layer_id: layer_id.to_owned(),
                    message: "plane voxel count overflow".to_owned(),
                })?;
        if plane_voxels == 0
            || values_zyx.is_empty()
            || !(values_zyx.len() as u64).is_multiple_of(plane_voxels)
        {
            let expected = usize::try_from(plane_voxels).unwrap_or(usize::MAX);
            return Err(FormatError::InvalidLayerValues {
                layer_id: layer_id.to_owned(),
                actual: values_zyx.len(),
                expected,
            });
        }
        let z_size = values_zyx.len() as u64 / plane_voxels;
        let z_end = z_start
            .checked_add(z_size)
            .ok_or_else(|| FormatError::ZarrStorage {
                layer_id: layer_id.to_owned(),
                message: "z slab range overflow".to_owned(),
            })?;
        if z_start >= self.shape.z || z_end > self.shape.z {
            return Err(FormatError::InvalidLayerValues {
                layer_id: layer_id.to_owned(),
                actual: values_zyx.len(),
                expected: usize::try_from(plane_voxels * self.shape.z).unwrap_or(usize::MAX),
            });
        }
        if (z_start..z_end).any(|z| self.written_z_planes[timepoint as usize][z as usize]) {
            return Err(FormatError::DuplicateTimepointWrite {
                layer_id: layer_id.to_owned(),
                level: self.level,
                timepoint,
            });
        }

        let subset = ArraySubset::new_with_ranges(&[
            timepoint..timepoint + 1,
            z_start..z_end,
            0..self.shape.y,
            0..self.shape.x,
        ]);
        self.array
            .store_array_subset_opt(&subset, values_zyx, &store_all_chunks_options())
            .map_err(zarr_storage_error)?;

        self.statistics.observe(values_zyx);
        self.observe_brick_statistics_for_slab(timepoint, z_start, z_end, values_zyx);
        let written_z = &mut self.written_z_planes[timepoint as usize];
        for z in z_start..z_end {
            written_z[z as usize] = true;
        }
        Ok(())
    }

    fn observe_brick_statistics(&mut self, timepoint: u64, values_zyx: &[u16]) {
        self.observe_brick_statistics_for_slab(timepoint, 0, self.shape.z, values_zyx);
    }

    fn observe_brick_statistics_for_slab(
        &mut self,
        timepoint: u64,
        z_start: u64,
        z_end: u64,
        values_zyx: &[u16],
    ) {
        let brick_t = timepoint / self.brick_shape.t;
        let first_brick_z = z_start / self.brick_shape.z;
        let last_brick_z = (z_end - 1) / self.brick_shape.z;
        for brick_z in first_brick_z..=last_brick_z {
            let z0 = brick_z * self.brick_shape.z;
            let z1 = (z0 + self.brick_shape.z).min(self.shape.z);
            let intersect_z0 = z0.max(z_start);
            let intersect_z1 = z1.min(z_end);
            for brick_y in 0..self.brick_grid.y {
                let y0 = brick_y * self.brick_shape.y;
                let y1 = (y0 + self.brick_shape.y).min(self.shape.y);
                for brick_x in 0..self.brick_grid.x {
                    let x0 = brick_x * self.brick_shape.x;
                    let x1 = (x0 + self.brick_shape.x).min(self.shape.x);
                    let index = linear_tzyx(self.brick_grid, brick_t, brick_z, brick_y, brick_x);
                    self.brick_statistics[index].observe_slab_region(
                        values_zyx,
                        self.shape,
                        z_start,
                        intersect_z0..intersect_z1,
                        y0..y1,
                        x0..x1,
                    );
                }
            }
        }
    }

    fn finish(self, layer_id: &str) -> Result<ScaleManifest, FormatError> {
        let written = self
            .written_z_planes
            .iter()
            .filter(|written| written.iter().all(|plane| *plane))
            .count();
        if written != self.written_z_planes.len() {
            return Err(FormatError::IncompleteScaleWrites {
                layer_id: layer_id.to_owned(),
                level: self.level,
                written,
                expected: self.written_z_planes.len(),
            });
        }

        let mut records = Vec::with_capacity(self.brick_statistics.len());
        for t in 0..self.brick_grid.t {
            for z in 0..self.brick_grid.z {
                for y in 0..self.brick_grid.y {
                    for x in 0..self.brick_grid.x {
                        let index = BrickIndex { t, z, y, x };
                        let stats =
                            &self.brick_statistics[linear_tzyx(self.brick_grid, t, z, y, x)];
                        let stats = stats.finish();
                        records.push(BrickRecord {
                            index,
                            occupied: stats.occupied,
                            valid_voxel_count: stats.valid_voxel_count,
                            min: f64::from(stats.min),
                            max: f64::from(stats.max),
                            payload_bytes: None,
                            payload_checksum: None,
                        });
                    }
                }
            }
        }
        let storage = sharded_storage_metadata(
            layer_id,
            &self.array,
            self.array_path.as_str(),
            IntensityDType::Uint16,
            self.shape,
            self.brick_shape,
        )?;

        Ok(ScaleManifest {
            level: self.level,
            array_path: self.array_path,
            shape: self.shape,
            storage,
            grid_to_world: self.grid_to_world,
            source_scale: self.source_scale,
            reduction: self.reduction,
            statistics: self.statistics.finish(),
            validity: None,
            bricks: BrickTable::new(self.brick_grid, records),
        })
    }
}

struct StreamingValidityScaleState {
    array_path: String,
    array: ZarrArray,
    shape: Shape4D,
    brick_shape: Shape4D,
    brick_grid: Shape4D,
    valid_counts: Vec<u64>,
}

impl StreamingValidityScaleState {
    fn write_timepoint(
        &mut self,
        _layer_id: &str,
        timepoint: u64,
        render_valid_zyx: &[u8],
    ) -> Result<(), FormatError> {
        let subset = ArraySubset::new_with_ranges(&[
            timepoint..timepoint + 1,
            0..self.shape.z,
            0..self.shape.y,
            0..self.shape.x,
        ]);
        self.array
            .store_array_subset_opt(&subset, render_valid_zyx, &store_all_chunks_options())
            .map_err(zarr_storage_error)?;
        self.observe_counts_for_slab(timepoint, 0, self.shape.z, render_valid_zyx);
        Ok(())
    }

    fn write_z_slab(
        &mut self,
        layer_id: &str,
        timepoint: u64,
        z_start: u64,
        render_valid_zyx: &[u8],
    ) -> Result<(), FormatError> {
        let plane_voxels =
            self.shape
                .y
                .checked_mul(self.shape.x)
                .ok_or_else(|| FormatError::ZarrStorage {
                    layer_id: layer_id.to_owned(),
                    message: "validity plane voxel count overflow".to_owned(),
                })?;
        let z_size = render_valid_zyx.len() as u64 / plane_voxels;
        let z_end = z_start
            .checked_add(z_size)
            .ok_or_else(|| FormatError::ZarrStorage {
                layer_id: layer_id.to_owned(),
                message: "validity z slab range overflow".to_owned(),
            })?;
        let subset = ArraySubset::new_with_ranges(&[
            timepoint..timepoint + 1,
            z_start..z_end,
            0..self.shape.y,
            0..self.shape.x,
        ]);
        self.array
            .store_array_subset_opt(&subset, render_valid_zyx, &store_all_chunks_options())
            .map_err(zarr_storage_error)?;
        self.observe_counts_for_slab(timepoint, z_start, z_end, render_valid_zyx);
        Ok(())
    }

    fn observe_counts_for_slab(
        &mut self,
        timepoint: u64,
        z_start: u64,
        z_end: u64,
        render_valid_zyx: &[u8],
    ) {
        let brick_t = timepoint / self.brick_shape.t;
        let first_brick_z = z_start / self.brick_shape.z;
        let last_brick_z = (z_end - 1) / self.brick_shape.z;
        for brick_z in first_brick_z..=last_brick_z {
            let z0 = brick_z * self.brick_shape.z;
            let z1 = (z0 + self.brick_shape.z).min(self.shape.z);
            let intersect_z0 = z0.max(z_start);
            let intersect_z1 = z1.min(z_end);
            for brick_y in 0..self.brick_grid.y {
                let y0 = brick_y * self.brick_shape.y;
                let y1 = (y0 + self.brick_shape.y).min(self.shape.y);
                for brick_x in 0..self.brick_grid.x {
                    let x0 = brick_x * self.brick_shape.x;
                    let x1 = (x0 + self.brick_shape.x).min(self.shape.x);
                    let index = linear_tzyx(self.brick_grid, brick_t, brick_z, brick_y, brick_x);
                    let mut count = 0_u64;
                    for z in intersect_z0..intersect_z1 {
                        let local_z = z - z_start;
                        for y in y0..y1 {
                            for x in x0..x1 {
                                let offset =
                                    ((local_z * self.shape.y + y) * self.shape.x + x) as usize;
                                count += u64::from(render_valid_zyx[offset] == 1);
                            }
                        }
                    }
                    self.valid_counts[index] += count;
                }
            }
        }
    }

    fn finish(self, layer_id: &str) -> Result<ScaleValidityMask, FormatError> {
        let mut records = Vec::with_capacity(self.valid_counts.len());
        let mut valid_voxel_count = 0_u64;
        for t in 0..self.brick_grid.t {
            for z in 0..self.brick_grid.z {
                for y in 0..self.brick_grid.y {
                    for x in 0..self.brick_grid.x {
                        let index = BrickIndex { t, z, y, x };
                        let count = self.valid_counts[linear_tzyx(self.brick_grid, t, z, y, x)];
                        valid_voxel_count += count;
                        records.push(ValidityMaskRecord {
                            index,
                            valid_voxel_count: count,
                            payload_bytes: None,
                            payload_checksum: None,
                        });
                    }
                }
            }
        }
        let total_voxels = self.shape.element_count()?;
        let storage = sharded_storage_metadata(
            layer_id,
            &self.array,
            self.array_path.as_str(),
            IntensityDType::Uint8,
            self.shape,
            self.brick_shape,
        )?;
        Ok(ScaleValidityMask {
            array_path: self.array_path,
            encoding: ValidityMaskEncoding::Uint8RenderValidMask,
            storage,
            valid_voxel_count,
            invalid_voxel_count: total_voxels - valid_voxel_count,
            records,
        })
    }
}

pub struct StreamingU8LayerWriter {
    id: String,
    name: String,
    channel: ChannelMetadata,
    shape: Shape4D,
    no_data_policy: Option<NoDataPolicy>,
    grid_to_world: GridToWorld,
    display: LayerDisplay,
    scales: Vec<StreamingU8ScaleState>,
}

impl StreamingU8LayerWriter {
    pub(super) fn create(
        store: ReadableWritableListableStorage,
        spec: StreamingU8LayerSpec,
    ) -> Result<Self, FormatError> {
        validate_streaming_u8_layer_scales(
            spec.id.as_str(),
            spec.shape,
            spec.grid_to_world,
            &spec.scales,
        )?;

        let mut scales = Vec::with_capacity(spec.scales.len());
        for scale in spec.scales {
            let array_path = format!("arrays/intensity/{}/s{}", spec.id, scale.level);
            let array = create_u8_array(&store, &array_path, scale.shape, scale.brick_shape)?;
            let brick_grid = scale.shape.chunk_grid(scale.brick_shape)?;
            let validity = if spec.no_data_policy.is_some() {
                let mask_path =
                    format!("arrays/validity/{}/s{}_render_valid", spec.id, scale.level);
                let mask_array =
                    create_u8_array(&store, &mask_path, scale.shape, scale.brick_shape)?;
                Some(StreamingValidityScaleState {
                    array_path: mask_path,
                    array: mask_array,
                    shape: scale.shape,
                    brick_shape: scale.brick_shape,
                    brick_grid,
                    valid_counts: vec![0; brick_grid.element_count()? as usize],
                })
            } else {
                None
            };
            scales.push(StreamingU8ScaleState {
                level: scale.level,
                array_path,
                array,
                shape: scale.shape,
                brick_shape: scale.brick_shape,
                grid_to_world: scale.grid_to_world,
                source_scale: scale.source_scale,
                reduction: scale.reduction,
                written_z_planes: vec![vec![false; scale.shape.z as usize]; scale.shape.t as usize],
                statistics: U8StatisticsAccumulator::new(),
                brick_grid,
                validity,
                brick_statistics: vec![
                    BrickStatisticsAccumulator::new();
                    brick_grid.element_count()? as usize
                ],
            });
        }

        Ok(Self {
            id: spec.id,
            name: spec.name,
            channel: spec.channel,
            shape: spec.shape,
            no_data_policy: spec.no_data_policy,
            grid_to_world: spec.grid_to_world,
            display: spec.display,
            scales,
        })
    }

    pub fn set_display(&mut self, display: LayerDisplay) {
        self.display = display;
    }

    pub fn scale_statistics(&self, level: u32) -> Result<Statistics, FormatError> {
        let scale = self.scale(level)?;
        Ok(scale.statistics.finish())
    }

    pub fn write_timepoint(
        &mut self,
        level: u32,
        timepoint: u64,
        values_zyx: &[u8],
    ) -> Result<(), FormatError> {
        let layer_id = self.id.clone();
        let scale = self.scale_mut(level)?;
        scale.write_timepoint(layer_id.as_str(), timepoint, values_zyx, None)
    }

    pub fn write_timepoint_with_render_valid(
        &mut self,
        level: u32,
        timepoint: u64,
        values_zyx: &[u8],
        render_valid_zyx: &[u8],
    ) -> Result<(), FormatError> {
        let layer_id = self.id.clone();
        let scale = self.scale_mut(level)?;
        scale.write_timepoint(
            layer_id.as_str(),
            timepoint,
            values_zyx,
            Some(render_valid_zyx),
        )
    }

    pub fn write_z_slab(
        &mut self,
        level: u32,
        timepoint: u64,
        z_start: u64,
        values_zyx: &[u8],
    ) -> Result<(), FormatError> {
        let layer_id = self.id.clone();
        let scale = self.scale_mut(level)?;
        scale.write_z_slab(layer_id.as_str(), timepoint, z_start, values_zyx, None)
    }

    pub fn write_z_slab_with_render_valid(
        &mut self,
        level: u32,
        timepoint: u64,
        z_start: u64,
        values_zyx: &[u8],
        render_valid_zyx: &[u8],
    ) -> Result<(), FormatError> {
        let layer_id = self.id.clone();
        let scale = self.scale_mut(level)?;
        scale.write_z_slab(
            layer_id.as_str(),
            timepoint,
            z_start,
            values_zyx,
            Some(render_valid_zyx),
        )
    }

    fn scale(&self, level: u32) -> Result<&StreamingU8ScaleState, FormatError> {
        self.scales.iter().find(|scale| scale.level == level).ok_or(
            FormatError::InvalidScaleLevel {
                layer_id: self.id.clone(),
                level,
            },
        )
    }

    fn scale_mut(&mut self, level: u32) -> Result<&mut StreamingU8ScaleState, FormatError> {
        self.scales
            .iter_mut()
            .find(|scale| scale.level == level)
            .ok_or(FormatError::InvalidScaleLevel {
                layer_id: self.id.clone(),
                level,
            })
    }

    pub(super) fn finish(self) -> Result<LayerManifest, FormatError> {
        let mut scales = Vec::with_capacity(self.scales.len());
        for scale in self.scales {
            scales.push(scale.finish(self.id.as_str())?);
        }

        Ok(LayerManifest {
            id: self.id,
            kind: LayerKind::DenseIntensity,
            name: self.name,
            channel: self.channel,
            shape: self.shape,
            dtype: DTypeMetadata {
                source: IntensityDType::Uint8,
                stored: IntensityDType::Uint8,
                conversion: DTypeConversion::Lossless,
            },
            grid_to_world: self.grid_to_world,
            display: self.display,
            scales,
            no_data_policy: self.no_data_policy,
        })
    }
}

struct StreamingU8ScaleState {
    level: u32,
    array_path: String,
    array: ZarrArray,
    shape: Shape4D,
    brick_shape: Shape4D,
    grid_to_world: GridToWorld,
    source_scale: Option<u32>,
    reduction: ScaleReduction,
    written_z_planes: Vec<Vec<bool>>,
    statistics: U8StatisticsAccumulator,
    brick_grid: Shape4D,
    validity: Option<StreamingValidityScaleState>,
    brick_statistics: Vec<BrickStatisticsAccumulator>,
}

impl StreamingU8ScaleState {
    fn write_timepoint(
        &mut self,
        layer_id: &str,
        timepoint: u64,
        values_zyx: &[u8],
        render_valid_zyx: Option<&[u8]>,
    ) -> Result<(), FormatError> {
        if timepoint >= self.shape.t {
            return Err(FormatError::InvalidTimepoint {
                layer_id: layer_id.to_owned(),
                level: self.level,
                timepoint,
            });
        }
        if self.written_z_planes[timepoint as usize]
            .iter()
            .any(|written| *written)
        {
            return Err(FormatError::DuplicateTimepointWrite {
                layer_id: layer_id.to_owned(),
                level: self.level,
                timepoint,
            });
        }

        let expected =
            Shape4D::new(1, self.shape.z, self.shape.y, self.shape.x)?.element_count()? as usize;
        let actual = values_zyx.len();
        if actual != expected {
            return Err(FormatError::InvalidLayerValues {
                layer_id: layer_id.to_owned(),
                actual,
                expected,
            });
        }
        self.validate_render_valid_write(layer_id, render_valid_zyx, expected)?;

        let subset = ArraySubset::new_with_ranges(&[
            timepoint..timepoint + 1,
            0..self.shape.z,
            0..self.shape.y,
            0..self.shape.x,
        ]);
        self.array
            .store_array_subset_opt(&subset, values_zyx, &store_all_chunks_options())
            .map_err(zarr_storage_error)?;

        if let Some(render_valid) = render_valid_zyx {
            self.validity
                .as_mut()
                .expect("render_valid write was validated")
                .write_timepoint(layer_id, timepoint, render_valid)?;
            self.statistics.observe_masked(values_zyx, render_valid);
            self.observe_brick_statistics(timepoint, values_zyx, Some(render_valid));
        } else {
            self.statistics.observe(values_zyx);
            self.observe_brick_statistics(timepoint, values_zyx, None);
        }
        for written in &mut self.written_z_planes[timepoint as usize] {
            *written = true;
        }
        Ok(())
    }

    fn write_z_slab(
        &mut self,
        layer_id: &str,
        timepoint: u64,
        z_start: u64,
        values_zyx: &[u8],
        render_valid_zyx: Option<&[u8]>,
    ) -> Result<(), FormatError> {
        if timepoint >= self.shape.t {
            return Err(FormatError::InvalidTimepoint {
                layer_id: layer_id.to_owned(),
                level: self.level,
                timepoint,
            });
        }
        let plane_voxels =
            self.shape
                .y
                .checked_mul(self.shape.x)
                .ok_or_else(|| FormatError::ZarrStorage {
                    layer_id: layer_id.to_owned(),
                    message: "plane voxel count overflow".to_owned(),
                })?;
        if plane_voxels == 0
            || values_zyx.is_empty()
            || !(values_zyx.len() as u64).is_multiple_of(plane_voxels)
        {
            let expected = usize::try_from(plane_voxels).unwrap_or(usize::MAX);
            return Err(FormatError::InvalidLayerValues {
                layer_id: layer_id.to_owned(),
                actual: values_zyx.len(),
                expected,
            });
        }
        self.validate_render_valid_write(layer_id, render_valid_zyx, values_zyx.len())?;
        let z_size = values_zyx.len() as u64 / plane_voxels;
        let z_end = z_start
            .checked_add(z_size)
            .ok_or_else(|| FormatError::ZarrStorage {
                layer_id: layer_id.to_owned(),
                message: "z slab range overflow".to_owned(),
            })?;
        if z_start >= self.shape.z || z_end > self.shape.z {
            return Err(FormatError::InvalidLayerValues {
                layer_id: layer_id.to_owned(),
                actual: values_zyx.len(),
                expected: usize::try_from(plane_voxels * self.shape.z).unwrap_or(usize::MAX),
            });
        }
        if (z_start..z_end).any(|z| self.written_z_planes[timepoint as usize][z as usize]) {
            return Err(FormatError::DuplicateTimepointWrite {
                layer_id: layer_id.to_owned(),
                level: self.level,
                timepoint,
            });
        }

        let subset = ArraySubset::new_with_ranges(&[
            timepoint..timepoint + 1,
            z_start..z_end,
            0..self.shape.y,
            0..self.shape.x,
        ]);
        self.array
            .store_array_subset_opt(&subset, values_zyx, &store_all_chunks_options())
            .map_err(zarr_storage_error)?;

        if let Some(render_valid) = render_valid_zyx {
            self.validity
                .as_mut()
                .expect("render_valid write was validated")
                .write_z_slab(layer_id, timepoint, z_start, render_valid)?;
            self.statistics.observe_masked(values_zyx, render_valid);
            self.observe_brick_statistics_for_slab(
                timepoint,
                z_start,
                z_end,
                values_zyx,
                Some(render_valid),
            );
        } else {
            self.statistics.observe(values_zyx);
            self.observe_brick_statistics_for_slab(timepoint, z_start, z_end, values_zyx, None);
        }
        let written_z = &mut self.written_z_planes[timepoint as usize];
        for z in z_start..z_end {
            written_z[z as usize] = true;
        }
        Ok(())
    }

    fn validate_render_valid_write(
        &self,
        layer_id: &str,
        render_valid_zyx: Option<&[u8]>,
        expected: usize,
    ) -> Result<(), FormatError> {
        match (&self.validity, render_valid_zyx) {
            (Some(_), Some(render_valid)) | (None, Some(render_valid)) => {
                if render_valid.len() != expected
                    || render_valid.iter().any(|value| !matches!(value, 0 | 1))
                {
                    return Err(FormatError::InvalidLayerValues {
                        layer_id: layer_id.to_owned(),
                        actual: render_valid.len(),
                        expected,
                    });
                }
            }
            (Some(_), None) => {
                return Err(FormatError::InvalidLayerValues {
                    layer_id: layer_id.to_owned(),
                    actual: 0,
                    expected,
                });
            }
            (None, None) => {}
        }
        if self.validity.is_none() && render_valid_zyx.is_some() {
            return Err(FormatError::InvalidLayerValues {
                layer_id: layer_id.to_owned(),
                actual: expected,
                expected: 0,
            });
        }
        Ok(())
    }

    fn observe_brick_statistics(
        &mut self,
        timepoint: u64,
        values_zyx: &[u8],
        render_valid_zyx: Option<&[u8]>,
    ) {
        self.observe_brick_statistics_for_slab(
            timepoint,
            0,
            self.shape.z,
            values_zyx,
            render_valid_zyx,
        );
    }

    fn observe_brick_statistics_for_slab(
        &mut self,
        timepoint: u64,
        z_start: u64,
        z_end: u64,
        values_zyx: &[u8],
        render_valid_zyx: Option<&[u8]>,
    ) {
        let brick_t = timepoint / self.brick_shape.t;
        let first_brick_z = z_start / self.brick_shape.z;
        let last_brick_z = (z_end - 1) / self.brick_shape.z;
        for brick_z in first_brick_z..=last_brick_z {
            let z0 = brick_z * self.brick_shape.z;
            let z1 = (z0 + self.brick_shape.z).min(self.shape.z);
            let intersect_z0 = z0.max(z_start);
            let intersect_z1 = z1.min(z_end);
            for brick_y in 0..self.brick_grid.y {
                let y0 = brick_y * self.brick_shape.y;
                let y1 = (y0 + self.brick_shape.y).min(self.shape.y);
                for brick_x in 0..self.brick_grid.x {
                    let x0 = brick_x * self.brick_shape.x;
                    let x1 = (x0 + self.brick_shape.x).min(self.shape.x);
                    let index = linear_tzyx(self.brick_grid, brick_t, brick_z, brick_y, brick_x);
                    let region = BrickSlabRegion {
                        shape: self.shape,
                        z_start,
                        z_range: intersect_z0..intersect_z1,
                        y_range: y0..y1,
                        x_range: x0..x1,
                    };
                    if let Some(render_valid) = render_valid_zyx {
                        self.brick_statistics[index].observe_u8_masked_slab_region(
                            values_zyx,
                            render_valid,
                            &region,
                        );
                    } else {
                        self.brick_statistics[index].observe_u8_slab_region(values_zyx, &region);
                    }
                }
            }
        }
    }

    fn finish(self, layer_id: &str) -> Result<ScaleManifest, FormatError> {
        let written = self
            .written_z_planes
            .iter()
            .filter(|written| written.iter().all(|plane| *plane))
            .count();
        if written != self.written_z_planes.len() {
            return Err(FormatError::IncompleteScaleWrites {
                layer_id: layer_id.to_owned(),
                level: self.level,
                written,
                expected: self.written_z_planes.len(),
            });
        }

        let mut records = Vec::with_capacity(self.brick_statistics.len());
        for t in 0..self.brick_grid.t {
            for z in 0..self.brick_grid.z {
                for y in 0..self.brick_grid.y {
                    for x in 0..self.brick_grid.x {
                        let index = BrickIndex { t, z, y, x };
                        let record_offset = linear_tzyx(self.brick_grid, t, z, y, x);
                        let stats = &self.brick_statistics[record_offset];
                        let stats = stats.finish();
                        let valid_voxel_count = self
                            .validity
                            .as_ref()
                            .map(|validity| validity.valid_counts[record_offset]);
                        let valid_voxel_count =
                            valid_voxel_count.unwrap_or(stats.valid_voxel_count);
                        records.push(BrickRecord {
                            index,
                            occupied: valid_voxel_count > 0,
                            valid_voxel_count,
                            min: f64::from(stats.min),
                            max: f64::from(stats.max),
                            payload_bytes: None,
                            payload_checksum: None,
                        });
                    }
                }
            }
        }
        let storage = sharded_storage_metadata(
            layer_id,
            &self.array,
            self.array_path.as_str(),
            IntensityDType::Uint8,
            self.shape,
            self.brick_shape,
        )?;

        Ok(ScaleManifest {
            level: self.level,
            array_path: self.array_path,
            shape: self.shape,
            storage,
            grid_to_world: self.grid_to_world,
            source_scale: self.source_scale,
            reduction: self.reduction,
            statistics: self.statistics.finish(),
            validity: self
                .validity
                .map(|validity| validity.finish(layer_id))
                .transpose()?,
            bricks: BrickTable::new(self.brick_grid, records),
        })
    }
}

pub struct StreamingF32LayerWriter {
    id: String,
    name: String,
    channel: ChannelMetadata,
    shape: Shape4D,
    grid_to_world: GridToWorld,
    display: LayerDisplay,
    scales: Vec<StreamingF32ScaleState>,
}

impl StreamingF32LayerWriter {
    pub(super) fn create(
        store: ReadableWritableListableStorage,
        spec: StreamingF32LayerSpec,
    ) -> Result<Self, FormatError> {
        validate_streaming_f32_layer_scales(
            spec.id.as_str(),
            spec.shape,
            spec.grid_to_world,
            &spec.scales,
        )?;

        let mut scales = Vec::with_capacity(spec.scales.len());
        for scale in spec.scales {
            let array_path = format!("arrays/intensity/{}/s{}", spec.id, scale.level);
            let array = create_f32_array(&store, &array_path, scale.shape, scale.brick_shape)?;
            let brick_grid = scale.shape.chunk_grid(scale.brick_shape)?;
            scales.push(StreamingF32ScaleState {
                level: scale.level,
                array_path,
                array,
                shape: scale.shape,
                brick_shape: scale.brick_shape,
                grid_to_world: scale.grid_to_world,
                source_scale: scale.source_scale,
                reduction: scale.reduction,
                written_z_planes: vec![vec![false; scale.shape.z as usize]; scale.shape.t as usize],
                statistics: F32StreamingStatisticsAccumulator::new(),
                brick_grid,
                brick_statistics: vec![
                    F32BrickStatisticsAccumulator::new();
                    brick_grid.element_count()? as usize
                ],
            });
        }

        Ok(Self {
            id: spec.id,
            name: spec.name,
            channel: spec.channel,
            shape: spec.shape,
            grid_to_world: spec.grid_to_world,
            display: spec.display,
            scales,
        })
    }

    pub fn set_display(&mut self, display: LayerDisplay) {
        self.display = display;
    }

    pub fn scale_statistics(&self, level: u32) -> Result<Statistics, FormatError> {
        let scale = self.scale(level)?;
        scale.finish_statistics(self.id.as_str())
    }

    pub fn write_timepoint(
        &mut self,
        level: u32,
        timepoint: u64,
        values_zyx: &[f32],
    ) -> Result<(), FormatError> {
        let layer_id = self.id.clone();
        let scale = self.scale_mut(level)?;
        scale.write_timepoint(layer_id.as_str(), timepoint, values_zyx)
    }

    pub fn write_z_slab(
        &mut self,
        level: u32,
        timepoint: u64,
        z_start: u64,
        values_zyx: &[f32],
    ) -> Result<(), FormatError> {
        let layer_id = self.id.clone();
        let scale = self.scale_mut(level)?;
        scale.write_z_slab(layer_id.as_str(), timepoint, z_start, values_zyx)
    }

    fn scale(&self, level: u32) -> Result<&StreamingF32ScaleState, FormatError> {
        self.scales.iter().find(|scale| scale.level == level).ok_or(
            FormatError::InvalidScaleLevel {
                layer_id: self.id.clone(),
                level,
            },
        )
    }

    fn scale_mut(&mut self, level: u32) -> Result<&mut StreamingF32ScaleState, FormatError> {
        self.scales
            .iter_mut()
            .find(|scale| scale.level == level)
            .ok_or(FormatError::InvalidScaleLevel {
                layer_id: self.id.clone(),
                level,
            })
    }

    pub(super) fn finish(self) -> Result<LayerManifest, FormatError> {
        let mut scales = Vec::with_capacity(self.scales.len());
        for scale in self.scales {
            scales.push(scale.finish(self.id.as_str())?);
        }

        Ok(LayerManifest {
            id: self.id,
            kind: LayerKind::DenseIntensity,
            name: self.name,
            channel: self.channel,
            shape: self.shape,
            dtype: DTypeMetadata {
                source: IntensityDType::Float32,
                stored: IntensityDType::Float32,
                conversion: DTypeConversion::Lossless,
            },
            no_data_policy: None,
            grid_to_world: self.grid_to_world,
            display: self.display,
            scales,
        })
    }
}

struct StreamingF32ScaleState {
    level: u32,
    array_path: String,
    array: ZarrArray,
    shape: Shape4D,
    brick_shape: Shape4D,
    grid_to_world: GridToWorld,
    source_scale: Option<u32>,
    reduction: ScaleReduction,
    written_z_planes: Vec<Vec<bool>>,
    statistics: F32StreamingStatisticsAccumulator,
    brick_grid: Shape4D,
    brick_statistics: Vec<F32BrickStatisticsAccumulator>,
}

impl StreamingF32ScaleState {
    fn write_timepoint(
        &mut self,
        layer_id: &str,
        timepoint: u64,
        values_zyx: &[f32],
    ) -> Result<(), FormatError> {
        if timepoint >= self.shape.t {
            return Err(FormatError::InvalidTimepoint {
                layer_id: layer_id.to_owned(),
                level: self.level,
                timepoint,
            });
        }
        if self.written_z_planes[timepoint as usize]
            .iter()
            .any(|written| *written)
        {
            return Err(FormatError::DuplicateTimepointWrite {
                layer_id: layer_id.to_owned(),
                level: self.level,
                timepoint,
            });
        }

        let expected =
            Shape4D::new(1, self.shape.z, self.shape.y, self.shape.x)?.element_count()? as usize;
        let actual = values_zyx.len();
        if actual != expected {
            return Err(FormatError::InvalidLayerValues {
                layer_id: layer_id.to_owned(),
                actual,
                expected,
            });
        }
        self.statistics
            .observe_timepoint(layer_id, timepoint, self.shape, values_zyx)?;

        let subset = ArraySubset::new_with_ranges(&[
            timepoint..timepoint + 1,
            0..self.shape.z,
            0..self.shape.y,
            0..self.shape.x,
        ]);
        self.array
            .store_array_subset_opt(&subset, values_zyx, &store_all_chunks_options())
            .map_err(zarr_storage_error)?;

        self.observe_brick_statistics(timepoint, values_zyx);
        for written in &mut self.written_z_planes[timepoint as usize] {
            *written = true;
        }
        Ok(())
    }

    fn write_z_slab(
        &mut self,
        layer_id: &str,
        timepoint: u64,
        z_start: u64,
        values_zyx: &[f32],
    ) -> Result<(), FormatError> {
        if timepoint >= self.shape.t {
            return Err(FormatError::InvalidTimepoint {
                layer_id: layer_id.to_owned(),
                level: self.level,
                timepoint,
            });
        }
        let plane_voxels =
            self.shape
                .y
                .checked_mul(self.shape.x)
                .ok_or_else(|| FormatError::ZarrStorage {
                    layer_id: layer_id.to_owned(),
                    message: "plane voxel count overflow".to_owned(),
                })?;
        if plane_voxels == 0
            || values_zyx.is_empty()
            || !(values_zyx.len() as u64).is_multiple_of(plane_voxels)
        {
            let expected = usize::try_from(plane_voxels).unwrap_or(usize::MAX);
            return Err(FormatError::InvalidLayerValues {
                layer_id: layer_id.to_owned(),
                actual: values_zyx.len(),
                expected,
            });
        }
        let z_size = values_zyx.len() as u64 / plane_voxels;
        let z_end = z_start
            .checked_add(z_size)
            .ok_or_else(|| FormatError::ZarrStorage {
                layer_id: layer_id.to_owned(),
                message: "z slab range overflow".to_owned(),
            })?;
        if z_start >= self.shape.z || z_end > self.shape.z {
            return Err(FormatError::InvalidLayerValues {
                layer_id: layer_id.to_owned(),
                actual: values_zyx.len(),
                expected: usize::try_from(plane_voxels * self.shape.z).unwrap_or(usize::MAX),
            });
        }
        if (z_start..z_end).any(|z| self.written_z_planes[timepoint as usize][z as usize]) {
            return Err(FormatError::DuplicateTimepointWrite {
                layer_id: layer_id.to_owned(),
                level: self.level,
                timepoint,
            });
        }
        self.statistics
            .observe_slab(layer_id, timepoint, z_start, self.shape, values_zyx)?;

        let subset = ArraySubset::new_with_ranges(&[
            timepoint..timepoint + 1,
            z_start..z_end,
            0..self.shape.y,
            0..self.shape.x,
        ]);
        self.array
            .store_array_subset_opt(&subset, values_zyx, &store_all_chunks_options())
            .map_err(zarr_storage_error)?;

        self.observe_brick_statistics_for_slab(timepoint, z_start, z_end, values_zyx);
        let written_z = &mut self.written_z_planes[timepoint as usize];
        for z in z_start..z_end {
            written_z[z as usize] = true;
        }
        Ok(())
    }

    fn observe_brick_statistics(&mut self, timepoint: u64, values_zyx: &[f32]) {
        self.observe_brick_statistics_for_slab(timepoint, 0, self.shape.z, values_zyx);
    }

    fn observe_brick_statistics_for_slab(
        &mut self,
        timepoint: u64,
        z_start: u64,
        z_end: u64,
        values_zyx: &[f32],
    ) {
        let brick_t = timepoint / self.brick_shape.t;
        let first_brick_z = z_start / self.brick_shape.z;
        let last_brick_z = (z_end - 1) / self.brick_shape.z;
        for brick_z in first_brick_z..=last_brick_z {
            let z0 = brick_z * self.brick_shape.z;
            let z1 = (z0 + self.brick_shape.z).min(self.shape.z);
            let intersect_z0 = z0.max(z_start);
            let intersect_z1 = z1.min(z_end);
            for brick_y in 0..self.brick_grid.y {
                let y0 = brick_y * self.brick_shape.y;
                let y1 = (y0 + self.brick_shape.y).min(self.shape.y);
                for brick_x in 0..self.brick_grid.x {
                    let x0 = brick_x * self.brick_shape.x;
                    let x1 = (x0 + self.brick_shape.x).min(self.shape.x);
                    let index = linear_tzyx(self.brick_grid, brick_t, brick_z, brick_y, brick_x);
                    self.brick_statistics[index].observe_slab_region(
                        values_zyx,
                        self.shape,
                        z_start,
                        intersect_z0..intersect_z1,
                        y0..y1,
                        x0..x1,
                    );
                }
            }
        }
    }

    fn finish_statistics(&self, layer_id: &str) -> Result<Statistics, FormatError> {
        self.statistics
            .finish_from_array(layer_id, &self.array, self.shape, self.brick_shape)
    }

    fn finish(self, layer_id: &str) -> Result<ScaleManifest, FormatError> {
        let written = self
            .written_z_planes
            .iter()
            .filter(|written| written.iter().all(|plane| *plane))
            .count();
        if written != self.written_z_planes.len() {
            return Err(FormatError::IncompleteScaleWrites {
                layer_id: layer_id.to_owned(),
                level: self.level,
                written,
                expected: self.written_z_planes.len(),
            });
        }

        let statistics = self.finish_statistics(layer_id)?;
        let mut records = Vec::with_capacity(self.brick_statistics.len());
        for t in 0..self.brick_grid.t {
            for z in 0..self.brick_grid.z {
                for y in 0..self.brick_grid.y {
                    for x in 0..self.brick_grid.x {
                        let index = BrickIndex { t, z, y, x };
                        let stats =
                            &self.brick_statistics[linear_tzyx(self.brick_grid, t, z, y, x)];
                        let stats = stats.finish();
                        records.push(BrickRecord {
                            index,
                            occupied: stats.occupied,
                            valid_voxel_count: stats.valid_voxel_count,
                            min: f64::from(stats.min),
                            max: f64::from(stats.max),
                            payload_bytes: None,
                            payload_checksum: None,
                        });
                    }
                }
            }
        }
        let storage = sharded_storage_metadata(
            layer_id,
            &self.array,
            self.array_path.as_str(),
            IntensityDType::Float32,
            self.shape,
            self.brick_shape,
        )?;

        Ok(ScaleManifest {
            level: self.level,
            array_path: self.array_path,
            shape: self.shape,
            storage,
            grid_to_world: self.grid_to_world,
            source_scale: self.source_scale,
            reduction: self.reduction,
            statistics,
            validity: None,
            bricks: BrickTable::new(self.brick_grid, records),
        })
    }
}
