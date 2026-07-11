use std::collections::HashMap;

use glam::DVec3;
use mirante4d_core::{CameraState, GridToWorld, LayerId, Shape3D, TimeIndex};
use mirante4d_data::{SpatialBrickIndex, VolumeBrickF32, VolumeBrickU8, VolumeBrickU16};

use crate::{
    CameraRenderMode, CameraRenderModeF32, CameraRenderQuality, DvrRenderParameters, DvrRgbaFrame,
    FrameDiagnostics, FrameDiagnosticsF32, IntensitySamplingPolicy, IsoShadingMode,
    IsoSurfaceFrameF32, IsoSurfaceFrameU16, IsoSurfaceNormal, MipImageF32, MipImageU16,
    PixelCoverage, RenderError, RenderViewport, frame_diagnostics, frame_diagnostics_f32,
};

const EPSILON: f64 = 1.0e-9;
const SMOOTH_RAY_STEP_VOXELS: f64 = 0.5;

#[derive(Debug, Clone)]
pub struct ResidentBrickSetU16 {
    pub layer_id: LayerId,
    pub timepoint: TimeIndex,
    pub volume_shape: Shape3D,
    pub grid_to_world: GridToWorld,
    brick_shape: Shape3D,
    brick_slots: HashMap<SpatialBrickIndex, usize>,
    bricks: Vec<VolumeBrickU16>,
}

#[derive(Debug, Clone)]
pub struct ResidentBrickSetU8 {
    pub layer_id: LayerId,
    pub timepoint: TimeIndex,
    pub volume_shape: Shape3D,
    pub grid_to_world: GridToWorld,
    brick_shape: Shape3D,
    brick_slots: HashMap<SpatialBrickIndex, usize>,
    bricks: Vec<VolumeBrickU8>,
}

#[derive(Debug, Clone)]
pub struct ResidentBrickSetF32 {
    pub layer_id: LayerId,
    pub timepoint: TimeIndex,
    pub volume_shape: Shape3D,
    pub grid_to_world: GridToWorld,
    brick_shape: Shape3D,
    brick_slots: HashMap<SpatialBrickIndex, usize>,
    bricks: Vec<VolumeBrickF32>,
}

#[derive(Debug, Clone, Copy)]
pub enum DvrResidentChannel<'a> {
    U8 {
        resident: &'a ResidentBrickSetU8,
        parameters: DvrRenderParameters,
    },
    U16 {
        resident: &'a ResidentBrickSetU16,
        parameters: DvrRenderParameters,
    },
    F32 {
        resident: &'a ResidentBrickSetF32,
        parameters: DvrRenderParameters,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrickFrameDiagnostics {
    pub frame: FrameDiagnostics,
    pub complete: bool,
    pub missing_voxel_samples: u64,
    pub skip: BrickSkipDiagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrickFrameDiagnosticsF32 {
    pub frame: FrameDiagnosticsF32,
    pub complete: bool,
    pub missing_voxel_samples: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BrickSkipDiagnostics {
    pub skipped_brick_intervals: u64,
    pub empty_brick_intervals: u64,
    pub mip_range_intervals: u64,
    pub iso_range_intervals: u64,
    pub dvr_range_intervals: u64,
}

impl BrickSkipDiagnostics {
    pub fn add_assign(&mut self, other: Self) {
        self.skipped_brick_intervals = self
            .skipped_brick_intervals
            .saturating_add(other.skipped_brick_intervals);
        self.empty_brick_intervals = self
            .empty_brick_intervals
            .saturating_add(other.empty_brick_intervals);
        self.mip_range_intervals = self
            .mip_range_intervals
            .saturating_add(other.mip_range_intervals);
        self.iso_range_intervals = self
            .iso_range_intervals
            .saturating_add(other.iso_range_intervals);
        self.dvr_range_intervals = self
            .dvr_range_intervals
            .saturating_add(other.dvr_range_intervals);
    }
}

#[derive(Debug, Clone, Copy)]
struct GridRay {
    origin: DVec3,
    direction: DVec3,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct IsoSurfaceHit {
    source_value: f64,
    display_scalar: f64,
    material_display_scalar: f64,
    hit_t: f64,
    grid_position: DVec3,
}

#[derive(Debug, Clone, Copy)]
struct RayBoxHit {
    enter: f64,
    exit: f64,
}

#[derive(Debug, Clone, Copy)]
struct AxisTraversal {
    index: i64,
    step: i64,
    next_t: f64,
    delta_t: f64,
    limit: i64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ResidentVoxel<T> {
    Visible(T),
    RenderInvalid,
    Missing,
}

trait IntegerResidentSet {
    fn volume_shape(&self) -> Shape3D;
    fn grid_to_world(&self) -> GridToWorld;
    fn sample_u16(&self, z: u64, y: u64, x: u64) -> ResidentVoxel<u16>;
    fn dvr_sample_u16(
        &self,
        z: u64,
        y: u64,
        x: u64,
        parameters: DvrRenderParameters,
    ) -> ResidentVoxel<u16>;
}

impl ResidentBrickSetU16 {
    pub fn new(
        layer_id: LayerId,
        timepoint: TimeIndex,
        volume_shape: Shape3D,
        grid_to_world: GridToWorld,
        bricks: Vec<VolumeBrickU16>,
    ) -> Self {
        let brick_shape = infer_u16_brick_shape(volume_shape, &bricks);
        Self::new_with_brick_shape(
            layer_id,
            timepoint,
            volume_shape,
            grid_to_world,
            brick_shape,
            bricks,
        )
    }

    pub fn new_with_brick_shape(
        layer_id: LayerId,
        timepoint: TimeIndex,
        volume_shape: Shape3D,
        grid_to_world: GridToWorld,
        brick_shape: Shape3D,
        bricks: Vec<VolumeBrickU16>,
    ) -> Self {
        let brick_slots = indexed_u16_brick_slots(&bricks);
        Self {
            layer_id,
            timepoint,
            volume_shape,
            grid_to_world,
            brick_shape,
            brick_slots,
            bricks,
        }
    }

    pub fn bricks(&self) -> &[VolumeBrickU16] {
        &self.bricks
    }

    pub fn insert_or_replace_brick(&mut self, brick: VolumeBrickU16) {
        if let Some(slot) = self.brick_slots.get(&brick.brick_index).copied() {
            self.bricks[slot] = brick;
            return;
        }
        let slot = self.bricks.len();
        self.brick_slots.insert(brick.brick_index, slot);
        self.bricks.push(brick);
    }

    pub fn remove_brick(&mut self, brick_index: SpatialBrickIndex) -> Option<VolumeBrickU16> {
        let slot = self.brick_slots.remove(&brick_index)?;
        let removed = self.bricks.swap_remove(slot);
        if let Some(swapped) = self.bricks.get(slot) {
            self.brick_slots.insert(swapped.brick_index, slot);
        }
        Some(removed)
    }

    pub fn retain_bricks(&mut self, mut keep: impl FnMut(&VolumeBrickU16) -> bool) {
        self.bricks.retain(|brick| keep(brick));
        self.brick_slots = indexed_u16_brick_slots(&self.bricks);
    }

    pub fn is_empty(&self) -> bool {
        self.bricks.is_empty()
    }

    pub fn scale_level(&self) -> u32 {
        self.bricks
            .first()
            .map(|brick| brick.scale_level)
            .unwrap_or(0)
    }

    fn brick_at(&self, z: u64, y: u64, x: u64) -> Option<&VolumeBrickU16> {
        let brick_index = SpatialBrickIndex::new(
            z / self.brick_shape.z,
            y / self.brick_shape.y,
            x / self.brick_shape.x,
        );
        self.brick_slots.get(&brick_index).and_then(|slot| {
            self.bricks
                .get(*slot)
                .filter(|brick| region_contains_voxel(brick.region, z, y, x))
        })
    }

    fn sample_from_brick(
        &self,
        brick: &VolumeBrickU16,
        z: u64,
        y: u64,
        x: u64,
    ) -> ResidentVoxel<u16> {
        let local_z = z - brick.region.z_start;
        let local_y = y - brick.region.y_start;
        let local_x = x - brick.region.x_start;
        match brick.volume.is_render_valid(local_z, local_y, local_x) {
            Some(false) => ResidentVoxel::RenderInvalid,
            Some(true) => brick
                .voxel(local_z, local_y, local_x)
                .map(ResidentVoxel::Visible)
                .unwrap_or(ResidentVoxel::Missing),
            None => ResidentVoxel::Missing,
        }
    }

    fn sample(&self, z: u64, y: u64, x: u64) -> ResidentVoxel<u16> {
        let Some(brick) = self.brick_at(z, y, x) else {
            return ResidentVoxel::Missing;
        };
        self.sample_from_brick(brick, z, y, x)
    }

    fn dvr_sample(
        &self,
        z: u64,
        y: u64,
        x: u64,
        parameters: DvrRenderParameters,
    ) -> ResidentVoxel<u16> {
        let Some(brick) = self.brick_at(z, y, x) else {
            return ResidentVoxel::Missing;
        };
        if !brick.occupied
            || brick.valid_voxel_count == 0
            || !parameters.source_interval_can_contribute(brick.min, brick.max)
        {
            return ResidentVoxel::RenderInvalid;
        }
        self.sample_from_brick(brick, z, y, x)
    }

    #[cfg(test)]
    fn voxel(&self, z: u64, y: u64, x: u64) -> Option<u16> {
        match self.sample(z, y, x) {
            ResidentVoxel::Visible(value) => Some(value),
            ResidentVoxel::RenderInvalid | ResidentVoxel::Missing => None,
        }
    }
}

impl IntegerResidentSet for ResidentBrickSetU16 {
    fn volume_shape(&self) -> Shape3D {
        self.volume_shape
    }

    fn grid_to_world(&self) -> GridToWorld {
        self.grid_to_world
    }

    fn sample_u16(&self, z: u64, y: u64, x: u64) -> ResidentVoxel<u16> {
        self.sample(z, y, x)
    }

    fn dvr_sample_u16(
        &self,
        z: u64,
        y: u64,
        x: u64,
        parameters: DvrRenderParameters,
    ) -> ResidentVoxel<u16> {
        self.dvr_sample(z, y, x, parameters)
    }
}

impl ResidentBrickSetU8 {
    pub fn new(
        layer_id: LayerId,
        timepoint: TimeIndex,
        volume_shape: Shape3D,
        grid_to_world: GridToWorld,
        bricks: Vec<VolumeBrickU8>,
    ) -> Self {
        let brick_shape = infer_u8_brick_shape(volume_shape, &bricks);
        Self::new_with_brick_shape(
            layer_id,
            timepoint,
            volume_shape,
            grid_to_world,
            brick_shape,
            bricks,
        )
    }

    pub fn new_with_brick_shape(
        layer_id: LayerId,
        timepoint: TimeIndex,
        volume_shape: Shape3D,
        grid_to_world: GridToWorld,
        brick_shape: Shape3D,
        bricks: Vec<VolumeBrickU8>,
    ) -> Self {
        let brick_slots = indexed_u8_brick_slots(&bricks);
        Self {
            layer_id,
            timepoint,
            volume_shape,
            grid_to_world,
            brick_shape,
            brick_slots,
            bricks,
        }
    }

    pub fn bricks(&self) -> &[VolumeBrickU8] {
        &self.bricks
    }

    pub fn insert_or_replace_brick(&mut self, brick: VolumeBrickU8) {
        if let Some(slot) = self.brick_slots.get(&brick.brick_index).copied() {
            self.bricks[slot] = brick;
            return;
        }
        let slot = self.bricks.len();
        self.brick_slots.insert(brick.brick_index, slot);
        self.bricks.push(brick);
    }

    pub fn remove_brick(&mut self, brick_index: SpatialBrickIndex) -> Option<VolumeBrickU8> {
        let slot = self.brick_slots.remove(&brick_index)?;
        let removed = self.bricks.swap_remove(slot);
        if let Some(swapped) = self.bricks.get(slot) {
            self.brick_slots.insert(swapped.brick_index, slot);
        }
        Some(removed)
    }

    pub fn retain_bricks(&mut self, mut keep: impl FnMut(&VolumeBrickU8) -> bool) {
        self.bricks.retain(|brick| keep(brick));
        self.brick_slots = indexed_u8_brick_slots(&self.bricks);
    }

    pub fn is_empty(&self) -> bool {
        self.bricks.is_empty()
    }

    pub fn scale_level(&self) -> u32 {
        self.bricks
            .first()
            .map(|brick| brick.scale_level)
            .unwrap_or(0)
    }

    fn brick_at(&self, z: u64, y: u64, x: u64) -> Option<&VolumeBrickU8> {
        let brick_index = SpatialBrickIndex::new(
            z / self.brick_shape.z,
            y / self.brick_shape.y,
            x / self.brick_shape.x,
        );
        self.brick_slots.get(&brick_index).and_then(|slot| {
            self.bricks
                .get(*slot)
                .filter(|brick| region_contains_voxel(brick.region, z, y, x))
        })
    }

    fn sample_from_brick(
        &self,
        brick: &VolumeBrickU8,
        z: u64,
        y: u64,
        x: u64,
    ) -> ResidentVoxel<u8> {
        let local_z = z - brick.region.z_start;
        let local_y = y - brick.region.y_start;
        let local_x = x - brick.region.x_start;
        match brick.volume.is_render_valid(local_z, local_y, local_x) {
            Some(false) => ResidentVoxel::RenderInvalid,
            Some(true) => brick
                .voxel(local_z, local_y, local_x)
                .map(ResidentVoxel::Visible)
                .unwrap_or(ResidentVoxel::Missing),
            None => ResidentVoxel::Missing,
        }
    }

    fn sample(&self, z: u64, y: u64, x: u64) -> ResidentVoxel<u8> {
        let Some(brick) = self.brick_at(z, y, x) else {
            return ResidentVoxel::Missing;
        };
        self.sample_from_brick(brick, z, y, x)
    }

    fn dvr_sample(
        &self,
        z: u64,
        y: u64,
        x: u64,
        parameters: DvrRenderParameters,
    ) -> ResidentVoxel<u8> {
        let Some(brick) = self.brick_at(z, y, x) else {
            return ResidentVoxel::Missing;
        };
        if !brick.occupied
            || brick.valid_voxel_count == 0
            || !parameters.source_interval_can_contribute(brick.min, brick.max)
        {
            return ResidentVoxel::RenderInvalid;
        }
        self.sample_from_brick(brick, z, y, x)
    }

    #[cfg(test)]
    fn voxel(&self, z: u64, y: u64, x: u64) -> Option<u8> {
        match self.sample(z, y, x) {
            ResidentVoxel::Visible(value) => Some(value),
            ResidentVoxel::RenderInvalid | ResidentVoxel::Missing => None,
        }
    }
}

impl IntegerResidentSet for ResidentBrickSetU8 {
    fn volume_shape(&self) -> Shape3D {
        self.volume_shape
    }

    fn grid_to_world(&self) -> GridToWorld {
        self.grid_to_world
    }

    fn sample_u16(&self, z: u64, y: u64, x: u64) -> ResidentVoxel<u16> {
        match self.sample(z, y, x) {
            ResidentVoxel::Visible(value) => ResidentVoxel::Visible(u16::from(value)),
            ResidentVoxel::RenderInvalid => ResidentVoxel::RenderInvalid,
            ResidentVoxel::Missing => ResidentVoxel::Missing,
        }
    }

    fn dvr_sample_u16(
        &self,
        z: u64,
        y: u64,
        x: u64,
        parameters: DvrRenderParameters,
    ) -> ResidentVoxel<u16> {
        match self.dvr_sample(z, y, x, parameters) {
            ResidentVoxel::Visible(value) => ResidentVoxel::Visible(u16::from(value)),
            ResidentVoxel::RenderInvalid => ResidentVoxel::RenderInvalid,
            ResidentVoxel::Missing => ResidentVoxel::Missing,
        }
    }
}

impl<'a> DvrResidentChannel<'a> {
    pub fn u8(resident: &'a ResidentBrickSetU8, parameters: DvrRenderParameters) -> Self {
        Self::U8 {
            resident,
            parameters,
        }
    }

    pub fn u16(resident: &'a ResidentBrickSetU16, parameters: DvrRenderParameters) -> Self {
        Self::U16 {
            resident,
            parameters,
        }
    }

    pub fn f32(resident: &'a ResidentBrickSetF32, parameters: DvrRenderParameters) -> Self {
        Self::F32 {
            resident,
            parameters,
        }
    }

    fn volume_shape(self) -> Shape3D {
        match self {
            Self::U8 { resident, .. } => resident.volume_shape,
            Self::U16 { resident, .. } => resident.volume_shape,
            Self::F32 { resident, .. } => resident.volume_shape,
        }
    }

    fn grid_to_world(self) -> GridToWorld {
        match self {
            Self::U8 { resident, .. } => resident.grid_to_world,
            Self::U16 { resident, .. } => resident.grid_to_world,
            Self::F32 { resident, .. } => resident.grid_to_world,
        }
    }

    fn parameters(self) -> DvrRenderParameters {
        match self {
            Self::U8 { parameters, .. }
            | Self::U16 { parameters, .. }
            | Self::F32 { parameters, .. } => parameters,
        }
    }
}

impl ResidentBrickSetF32 {
    pub fn new(
        layer_id: LayerId,
        timepoint: TimeIndex,
        volume_shape: Shape3D,
        grid_to_world: GridToWorld,
        bricks: Vec<VolumeBrickF32>,
    ) -> Self {
        let brick_shape = infer_f32_brick_shape(volume_shape, &bricks);
        Self::new_with_brick_shape(
            layer_id,
            timepoint,
            volume_shape,
            grid_to_world,
            brick_shape,
            bricks,
        )
    }

    pub fn new_with_brick_shape(
        layer_id: LayerId,
        timepoint: TimeIndex,
        volume_shape: Shape3D,
        grid_to_world: GridToWorld,
        brick_shape: Shape3D,
        bricks: Vec<VolumeBrickF32>,
    ) -> Self {
        let brick_slots = indexed_f32_brick_slots(&bricks);
        Self {
            layer_id,
            timepoint,
            volume_shape,
            grid_to_world,
            brick_shape,
            brick_slots,
            bricks,
        }
    }

    pub fn bricks(&self) -> &[VolumeBrickF32] {
        &self.bricks
    }

    pub fn insert_or_replace_brick(&mut self, brick: VolumeBrickF32) {
        if let Some(slot) = self.brick_slots.get(&brick.brick_index).copied() {
            self.bricks[slot] = brick;
            return;
        }
        let slot = self.bricks.len();
        self.brick_slots.insert(brick.brick_index, slot);
        self.bricks.push(brick);
    }

    pub fn remove_brick(&mut self, brick_index: SpatialBrickIndex) -> Option<VolumeBrickF32> {
        let slot = self.brick_slots.remove(&brick_index)?;
        let removed = self.bricks.swap_remove(slot);
        if let Some(swapped) = self.bricks.get(slot) {
            self.brick_slots.insert(swapped.brick_index, slot);
        }
        Some(removed)
    }

    pub fn retain_bricks(&mut self, mut keep: impl FnMut(&VolumeBrickF32) -> bool) {
        self.bricks.retain(|brick| keep(brick));
        self.brick_slots = indexed_f32_brick_slots(&self.bricks);
    }

    pub fn is_empty(&self) -> bool {
        self.bricks.is_empty()
    }

    pub fn scale_level(&self) -> u32 {
        self.bricks
            .first()
            .map(|brick| brick.scale_level)
            .unwrap_or(0)
    }

    fn brick_at(&self, z: u64, y: u64, x: u64) -> Option<&VolumeBrickF32> {
        let brick_index = SpatialBrickIndex::new(
            z / self.brick_shape.z,
            y / self.brick_shape.y,
            x / self.brick_shape.x,
        );
        self.brick_slots.get(&brick_index).and_then(|slot| {
            self.bricks
                .get(*slot)
                .filter(|brick| region_contains_voxel(brick.region, z, y, x))
        })
    }

    fn sample_from_brick(
        &self,
        brick: &VolumeBrickF32,
        z: u64,
        y: u64,
        x: u64,
    ) -> ResidentVoxel<f32> {
        let local_z = z - brick.region.z_start;
        let local_y = y - brick.region.y_start;
        let local_x = x - brick.region.x_start;
        match brick.volume.is_render_valid(local_z, local_y, local_x) {
            Some(false) => ResidentVoxel::RenderInvalid,
            Some(true) => brick
                .voxel(local_z, local_y, local_x)
                .map(ResidentVoxel::Visible)
                .unwrap_or(ResidentVoxel::Missing),
            None => ResidentVoxel::Missing,
        }
    }

    fn sample(&self, z: u64, y: u64, x: u64) -> ResidentVoxel<f32> {
        let Some(brick) = self.brick_at(z, y, x) else {
            return ResidentVoxel::Missing;
        };
        self.sample_from_brick(brick, z, y, x)
    }

    fn dvr_sample(
        &self,
        z: u64,
        y: u64,
        x: u64,
        parameters: DvrRenderParameters,
    ) -> ResidentVoxel<f32> {
        let Some(brick) = self.brick_at(z, y, x) else {
            return ResidentVoxel::Missing;
        };
        if !brick.occupied
            || brick.valid_voxel_count == 0
            || !parameters.source_interval_can_contribute(brick.min, brick.max)
        {
            return ResidentVoxel::RenderInvalid;
        }
        self.sample_from_brick(brick, z, y, x)
    }

    #[cfg(test)]
    fn voxel(&self, z: u64, y: u64, x: u64) -> Option<f32> {
        match self.sample(z, y, x) {
            ResidentVoxel::Visible(value) => Some(value),
            ResidentVoxel::RenderInvalid | ResidentVoxel::Missing => None,
        }
    }
}

fn indexed_u16_brick_slots(bricks: &[VolumeBrickU16]) -> HashMap<SpatialBrickIndex, usize> {
    bricks
        .iter()
        .enumerate()
        .map(|(slot, brick)| (brick.brick_index, slot))
        .collect()
}

fn indexed_u8_brick_slots(bricks: &[VolumeBrickU8]) -> HashMap<SpatialBrickIndex, usize> {
    bricks
        .iter()
        .enumerate()
        .map(|(slot, brick)| (brick.brick_index, slot))
        .collect()
}

fn indexed_f32_brick_slots(bricks: &[VolumeBrickF32]) -> HashMap<SpatialBrickIndex, usize> {
    bricks
        .iter()
        .enumerate()
        .map(|(slot, brick)| (brick.brick_index, slot))
        .collect()
}

fn infer_u8_brick_shape(volume_shape: Shape3D, bricks: &[VolumeBrickU8]) -> Shape3D {
    let z = infer_axis_brick_size(
        volume_shape.z,
        bricks.iter().map(|brick| {
            (
                brick.brick_index.z,
                brick.region.z_start,
                brick.region.z_size,
            )
        }),
    );
    let y = infer_axis_brick_size(
        volume_shape.y,
        bricks.iter().map(|brick| {
            (
                brick.brick_index.y,
                brick.region.y_start,
                brick.region.y_size,
            )
        }),
    );
    let x = infer_axis_brick_size(
        volume_shape.x,
        bricks.iter().map(|brick| {
            (
                brick.brick_index.x,
                brick.region.x_start,
                brick.region.x_size,
            )
        }),
    );
    Shape3D { z, y, x }
}

fn infer_u16_brick_shape(volume_shape: Shape3D, bricks: &[VolumeBrickU16]) -> Shape3D {
    let z = infer_axis_brick_size(
        volume_shape.z,
        bricks.iter().map(|brick| {
            (
                brick.brick_index.z,
                brick.region.z_start,
                brick.region.z_size,
            )
        }),
    );
    let y = infer_axis_brick_size(
        volume_shape.y,
        bricks.iter().map(|brick| {
            (
                brick.brick_index.y,
                brick.region.y_start,
                brick.region.y_size,
            )
        }),
    );
    let x = infer_axis_brick_size(
        volume_shape.x,
        bricks.iter().map(|brick| {
            (
                brick.brick_index.x,
                brick.region.x_start,
                brick.region.x_size,
            )
        }),
    );
    Shape3D { z, y, x }
}

fn infer_f32_brick_shape(volume_shape: Shape3D, bricks: &[VolumeBrickF32]) -> Shape3D {
    let z = infer_axis_brick_size(
        volume_shape.z,
        bricks.iter().map(|brick| {
            (
                brick.brick_index.z,
                brick.region.z_start,
                brick.region.z_size,
            )
        }),
    );
    let y = infer_axis_brick_size(
        volume_shape.y,
        bricks.iter().map(|brick| {
            (
                brick.brick_index.y,
                brick.region.y_start,
                brick.region.y_size,
            )
        }),
    );
    let x = infer_axis_brick_size(
        volume_shape.x,
        bricks.iter().map(|brick| {
            (
                brick.brick_index.x,
                brick.region.x_start,
                brick.region.x_size,
            )
        }),
    );
    Shape3D { z, y, x }
}

fn infer_axis_brick_size(
    volume_axis: u64,
    regions: impl IntoIterator<Item = (u64, u64, u64)>,
) -> u64 {
    let mut size = 1;
    for (brick_index, start, extent) in regions {
        size = size.max(extent);
        if let Some(inferred_size) = start.checked_div(brick_index) {
            size = size.max(inferred_size);
        }
    }
    size.min(volume_axis).max(1)
}

fn region_contains_voxel(region: mirante4d_data::VolumeRegion, z: u64, y: u64, x: u64) -> bool {
    z >= region.z_start
        && z < region.z_start + region.z_size
        && y >= region.y_start
        && y < region.y_start + region.y_size
        && x >= region.x_start
        && x < region.x_start + region.x_size
}

mod sampling;
pub use sampling::{
    render_camera_f32_from_bricks, render_camera_f32_from_bricks_with_quality,
    render_camera_from_bricks, render_camera_from_bricks_with_quality,
    render_camera_mip_from_bricks, render_camera_u8_from_bricks_with_quality,
    render_dvr_channels_from_bricks_with_quality,
};

#[cfg(test)]
mod tests;
