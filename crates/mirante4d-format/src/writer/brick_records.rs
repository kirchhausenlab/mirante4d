use super::*;

#[derive(Debug, Clone)]
pub(super) struct BrickStatisticsAccumulator {
    min: u16,
    max: u16,
    observed: bool,
    occupied: bool,
    valid_voxel_count: u64,
}

pub(super) struct BrickSlabRegion {
    pub(super) shape: Shape4D,
    pub(super) z_start: u64,
    pub(super) z_range: std::ops::Range<u64>,
    pub(super) y_range: std::ops::Range<u64>,
    pub(super) x_range: std::ops::Range<u64>,
}

impl BrickStatisticsAccumulator {
    pub(super) fn new() -> Self {
        Self {
            min: u16::MAX,
            max: u16::MIN,
            observed: false,
            occupied: false,
            valid_voxel_count: 0,
        }
    }

    pub(super) fn observe_slab_region(
        &mut self,
        values_zyx: &[u16],
        shape: Shape4D,
        slab_z_start: u64,
        z_range: std::ops::Range<u64>,
        y_range: std::ops::Range<u64>,
        x_range: std::ops::Range<u64>,
    ) {
        for z in z_range {
            let local_z = z - slab_z_start;
            for y in y_range.clone() {
                for x in x_range.clone() {
                    let value = values_zyx[((local_z * shape.y + y) * shape.x + x) as usize];
                    self.min = self.min.min(value);
                    self.max = self.max.max(value);
                    self.observed = true;
                    self.occupied = true;
                    self.valid_voxel_count += 1;
                }
            }
        }
    }

    pub(super) fn observe_u8_slab_region(&mut self, values_zyx: &[u8], region: &BrickSlabRegion) {
        for z in region.z_range.clone() {
            let local_z = z - region.z_start;
            for y in region.y_range.clone() {
                for x in region.x_range.clone() {
                    let value = u16::from(
                        values_zyx[((local_z * region.shape.y + y) * region.shape.x + x) as usize],
                    );
                    self.min = self.min.min(value);
                    self.max = self.max.max(value);
                    self.observed = true;
                    self.occupied = true;
                    self.valid_voxel_count += 1;
                }
            }
        }
    }

    pub(super) fn observe_u8_masked_slab_region(
        &mut self,
        values_zyx: &[u8],
        render_valid_zyx: &[u8],
        region: &BrickSlabRegion,
    ) {
        for z in region.z_range.clone() {
            let local_z = z - region.z_start;
            for y in region.y_range.clone() {
                for x in region.x_range.clone() {
                    let offset = ((local_z * region.shape.y + y) * region.shape.x + x) as usize;
                    if render_valid_zyx[offset] != 1 {
                        continue;
                    }
                    let value = u16::from(values_zyx[offset]);
                    self.min = self.min.min(value);
                    self.max = self.max.max(value);
                    self.observed = true;
                    self.occupied = true;
                    self.valid_voxel_count += 1;
                }
            }
        }
    }

    pub(super) fn finish(&self) -> BrickStatistics {
        if self.observed {
            BrickStatistics {
                min: self.min,
                max: self.max,
                occupied: self.occupied,
                valid_voxel_count: self.valid_voxel_count,
            }
        } else {
            BrickStatistics::empty()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BrickStatistics {
    pub(super) min: u16,
    pub(super) max: u16,
    pub(super) occupied: bool,
    pub(super) valid_voxel_count: u64,
}

impl BrickStatistics {
    pub(super) fn empty() -> Self {
        Self {
            min: 0,
            max: 0,
            occupied: false,
            valid_voxel_count: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct F32BrickStatisticsAccumulator {
    min: f32,
    max: f32,
    observed: bool,
    occupied: bool,
    valid_voxel_count: u64,
}

impl F32BrickStatisticsAccumulator {
    pub(super) fn new() -> Self {
        Self {
            min: f32::INFINITY,
            max: f32::NEG_INFINITY,
            observed: false,
            occupied: false,
            valid_voxel_count: 0,
        }
    }

    pub(super) fn observe_slab_region(
        &mut self,
        values_zyx: &[f32],
        shape: Shape4D,
        slab_z_start: u64,
        z_range: std::ops::Range<u64>,
        y_range: std::ops::Range<u64>,
        x_range: std::ops::Range<u64>,
    ) {
        for z in z_range {
            let local_z = z - slab_z_start;
            for y in y_range.clone() {
                for x in x_range.clone() {
                    let value = values_zyx[((local_z * shape.y + y) * shape.x + x) as usize];
                    self.min = self.min.min(value);
                    self.max = self.max.max(value);
                    self.observed = true;
                    self.occupied = true;
                    self.valid_voxel_count += 1;
                }
            }
        }
    }

    pub(super) fn finish(&self) -> F32BrickStatistics {
        if self.observed {
            F32BrickStatistics {
                min: self.min,
                max: self.max,
                occupied: self.occupied,
                valid_voxel_count: self.valid_voxel_count,
            }
        } else {
            F32BrickStatistics::empty()
        }
    }
}

pub(super) fn build_brick_table(
    _array: &crate::zarr_io::ZarrArray,
    _layer_id: &str,
    scale: &DenseU16Scale,
) -> Result<BrickTable, FormatError> {
    let grid_shape = scale.shape.chunk_grid(scale.brick_shape)?;
    let mut records = Vec::with_capacity(grid_shape.element_count()? as usize);
    for t in 0..grid_shape.t {
        for z in 0..grid_shape.z {
            for y in 0..grid_shape.y {
                for x in 0..grid_shape.x {
                    let index = BrickIndex { t, z, y, x };
                    let stats = brick_statistics(scale, index);
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
    Ok(BrickTable::new(grid_shape, records))
}

pub(super) fn build_f32_brick_table(
    _array: &crate::zarr_io::ZarrArray,
    _layer_id: &str,
    scale: &DenseF32Scale,
) -> Result<BrickTable, FormatError> {
    let grid_shape = scale.shape.chunk_grid(scale.brick_shape)?;
    let mut records = Vec::with_capacity(grid_shape.element_count()? as usize);
    for t in 0..grid_shape.t {
        for z in 0..grid_shape.z {
            for y in 0..grid_shape.y {
                for x in 0..grid_shape.x {
                    let index = BrickIndex { t, z, y, x };
                    let stats = f32_brick_statistics(scale, index);
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
    Ok(BrickTable::new(grid_shape, records))
}

pub(super) fn brick_statistics(scale: &DenseU16Scale, index: BrickIndex) -> BrickStatistics {
    let t0 = index.t * scale.brick_shape.t;
    let z0 = index.z * scale.brick_shape.z;
    let y0 = index.y * scale.brick_shape.y;
    let x0 = index.x * scale.brick_shape.x;
    let t1 = (t0 + scale.brick_shape.t).min(scale.shape.t);
    let z1 = (z0 + scale.brick_shape.z).min(scale.shape.z);
    let y1 = (y0 + scale.brick_shape.y).min(scale.shape.y);
    let x1 = (x0 + scale.brick_shape.x).min(scale.shape.x);

    let mut min = u16::MAX;
    let mut max = u16::MIN;
    let mut occupied = false;
    let mut valid_voxel_count = 0_u64;
    let mut observed = false;
    for t in t0..t1 {
        for z in z0..z1 {
            for y in y0..y1 {
                for x in x0..x1 {
                    let value = scale.values_tzyx[linear_tzyx(scale.shape, t, z, y, x)];
                    min = min.min(value);
                    max = max.max(value);
                    occupied = true;
                    valid_voxel_count += 1;
                    observed = true;
                }
            }
        }
    }
    if observed {
        BrickStatistics {
            min,
            max,
            occupied,
            valid_voxel_count,
        }
    } else {
        BrickStatistics::empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct F32BrickStatistics {
    pub(super) min: f32,
    pub(super) max: f32,
    pub(super) occupied: bool,
    pub(super) valid_voxel_count: u64,
}

impl F32BrickStatistics {
    pub(super) fn empty() -> Self {
        Self {
            min: 0.0,
            max: 0.0,
            occupied: false,
            valid_voxel_count: 0,
        }
    }
}

pub(super) fn f32_brick_statistics(scale: &DenseF32Scale, index: BrickIndex) -> F32BrickStatistics {
    let t0 = index.t * scale.brick_shape.t;
    let z0 = index.z * scale.brick_shape.z;
    let y0 = index.y * scale.brick_shape.y;
    let x0 = index.x * scale.brick_shape.x;
    let t1 = (t0 + scale.brick_shape.t).min(scale.shape.t);
    let z1 = (z0 + scale.brick_shape.z).min(scale.shape.z);
    let y1 = (y0 + scale.brick_shape.y).min(scale.shape.y);
    let x1 = (x0 + scale.brick_shape.x).min(scale.shape.x);

    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let mut occupied = false;
    let mut valid_voxel_count = 0_u64;
    let mut observed = false;
    for t in t0..t1 {
        for z in z0..z1 {
            for y in y0..y1 {
                for x in x0..x1 {
                    let value = scale.values_tzyx[linear_tzyx(scale.shape, t, z, y, x)];
                    min = min.min(value);
                    max = max.max(value);
                    occupied = true;
                    valid_voxel_count += 1;
                    observed = true;
                }
            }
        }
    }
    if observed {
        F32BrickStatistics {
            min,
            max,
            occupied,
            valid_voxel_count,
        }
    } else {
        F32BrickStatistics::empty()
    }
}
