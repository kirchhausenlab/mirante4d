use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

use mirante4d_dataset::{
    CpuByteLease, CpuByteLedger, CpuLedgerCategory, ResourcePayloadView, ResourceRegion,
};
use mirante4d_domain::{IntensityDType, Shape3D};
use wgpu::util::DeviceExt;

use super::{GpuBrickAtlasResidencySnapshot, GpuRenderError, GpuRenderer, GpuRendererStats};
use crate::{
    CurrentLeaseResource, CurrentLeaseVolume, RenderError,
    resources::{BrickAtlasResourceKey, ResourceRepresentation},
};

use super::buffers::{
    checked_buffer_byte_count, checked_u32, checked_u64_buffer_byte_count,
    packed_u32_per_integer_brick, validate_f32_brick_atlas_budget, validate_storage_buffer_bytes,
    validate_u8_brick_atlas_budget, validate_u16_brick_atlas_budget, validity_u32_per_brick,
};

mod f32_packing;
mod upload_ready;

use f32_packing::F32UploadBytes;
use upload_ready::{PackedIntegerBrick, UploadReadyIntegerBrickCache, upload_ready_cache_budget};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum IntegerAtlasDType {
    U8,
    U16,
}

#[derive(Clone, Copy, Debug)]
struct IntegerAtlasGrowthRequest {
    dtype: IntegerAtlasDType,
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
    current_slot_count: usize,
    required_slot_count: usize,
    visible_slot_count: usize,
}

const INTEGER_BRICK_METADATA_WORDS: u64 = 4;
pub(super) const F32_BRICK_PAGE_TABLE_WORDS: u64 = 7;
const BRICK_METADATA_RESIDENT_FLAG: u32 = 0x1;
const BRICK_METADATA_HAS_VALID_FLAG: u32 = 0x2;
const BRICK_METADATA_MIN_MAX_VALID_FLAG: u32 = 0x4;

impl IntegerAtlasDType {
    fn intensity_dtype(self) -> IntensityDType {
        match self {
            Self::U8 => IntensityDType::Uint8,
            Self::U16 => IntensityDType::Uint16,
        }
    }

    fn values_per_word(self) -> u32 {
        match self {
            Self::U8 => 4,
            Self::U16 => 2,
        }
    }

    fn bits_per_value(self) -> u32 {
        match self {
            Self::U8 => 8,
            Self::U16 => 16,
        }
    }

    fn value_mask(self) -> u32 {
        match self {
            Self::U8 => 0x00ff,
            Self::U16 => 0xffff,
        }
    }

    fn packed_u32_per_brick(self, brick_voxel_count: u64) -> u64 {
        packed_u32_per_integer_brick(brick_voxel_count, u64::from(self.values_per_word()))
    }

    fn value_resource(self) -> &'static str {
        match self {
            Self::U8 => "brick atlas packed uint8 values",
            Self::U16 => "brick atlas packed uint16 values",
        }
    }

    fn buffer_label(self) -> &'static str {
        match self {
            Self::U8 => "mirante4d-brick-atlas-packed-u8",
            Self::U16 => "mirante4d-brick-atlas-packed-u16",
        }
    }

    fn validate_budget(
        self,
        budget_bytes: u64,
        limits: &wgpu::Limits,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        slot_count: usize,
    ) -> Result<(), GpuRenderError> {
        match self {
            Self::U8 => validate_u8_brick_atlas_budget(
                budget_bytes,
                limits,
                brick_shape,
                brick_grid_shape,
                slot_count,
            ),
            Self::U16 => validate_u16_brick_atlas_budget(
                budget_bytes,
                limits,
                brick_shape,
                brick_grid_shape,
                slot_count,
            ),
        }
    }
}

#[derive(Clone)]
pub(super) struct GpuBrickAtlasResource {
    pub(super) generation: u64,
    pub(super) packed_values_buffer: Arc<wgpu::Buffer>,
    pub(super) validity_buffer: Arc<wgpu::Buffer>,
    pub(super) page_table_buffer: Arc<wgpu::Buffer>,
    pub(super) metadata_buffer: Arc<wgpu::Buffer>,
    pub(super) bytes: u64,
    pub(super) dtype: IntegerAtlasDType,
    pub(super) brick_shape: Shape3D,
    pub(super) brick_grid_shape: Shape3D,
    pub(super) brick_voxel_count: u64,
    pub(super) packed_u32_per_brick: u64,
    pub(super) valid_u32_per_brick: u64,
    pub(super) values_per_word: u32,
    pub(super) bits_per_value: u32,
    pub(super) value_mask: u32,
    pub(super) slot_count: usize,
    page_table: Vec<u32>,
    metadata: Vec<u32>,
    page_slots: HashMap<AtlasPageIndex, usize>,
    page_regions: HashMap<AtlasPageIndex, ResourceRegion>,
    page_priorities: HashMap<AtlasPageIndex, GpuBrickAtlasPagePriority>,
    active_pages: HashSet<AtlasPageIndex>,
    slot_pages: Vec<Option<AtlasPageIndex>>,
    page_lru: VecDeque<AtlasPageIndex>,
    _cpu_mapping_charge: Arc<dyn CpuByteLease>,
}

#[derive(Clone)]
pub(super) struct GpuBrickAtlasF32Resource {
    pub(super) generation: u64,
    pub(super) values_buffer: Arc<wgpu::Buffer>,
    pub(super) page_table_buffer: Arc<wgpu::Buffer>,
    pub(super) bytes: u64,
    pub(super) brick_shape: Shape3D,
    pub(super) brick_grid_shape: Shape3D,
    pub(super) brick_voxel_count: u64,
    pub(super) value_words_used: u64,
    pub(super) values_word_capacity: u64,
    pub(super) page_table_word_count: u64,
    page_table: Vec<u32>,
    page_allocations: HashMap<AtlasPageIndex, GpuF32BrickAllocation>,
    page_priorities: HashMap<AtlasPageIndex, GpuBrickAtlasPagePriority>,
    active_pages: HashSet<AtlasPageIndex>,
    page_lru: VecDeque<AtlasPageIndex>,
    _cpu_mapping_charge: Arc<dyn CpuByteLease>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct AtlasPageIndex {
    z: u64,
    y: u64,
    x: u64,
}

#[derive(Debug, Clone, Copy)]
struct LeaseAtlasPage<'a> {
    brick_index: AtlasPageIndex,
    region: ResourceRegion,
    payload: ResourcePayloadView<'a>,
    resource: CurrentLeaseResource<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GpuF32BrickAllocation {
    value_offset_words: u32,
    value_words: u64,
    x_size: u32,
    y_size: u32,
    z_size: u32,
    x_start: u32,
    y_start: u32,
    z_start: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct GpuBrickAtlasUpdate {
    uploaded_pages: u64,
    uploaded_bytes: u64,
    evicted_pages: u64,
    page_table_rebuilds: u64,
    page_table_bytes_written: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpuBrickAtlasPagePriority {
    pub tier_rank: u32,
    pub score: f64,
}

impl GpuBrickAtlasPagePriority {
    pub fn new(tier_rank: u32, score: f64) -> Self {
        Self { tier_rank, score }
    }
}

impl Default for GpuBrickAtlasPagePriority {
    fn default() -> Self {
        Self {
            tier_rank: u32::MAX,
            score: f64::NEG_INFINITY,
        }
    }
}

pub(super) struct GpuBrickAtlasCache {
    pub(super) max_bytes: u64,
    pub(super) current_bytes: u64,
    current_u8_bytes: u64,
    current_u16_bytes: u64,
    order: VecDeque<BrickAtlasResourceKey>,
    atlases: HashMap<BrickAtlasResourceKey, GpuBrickAtlasResource>,
    next_generation: u64,
    upload_ready_cache: UploadReadyIntegerBrickCache,
    pub(super) stats: GpuRendererStats,
}

pub(super) struct GpuBrickAtlasF32Cache {
    max_bytes: u64,
    current_bytes: u64,
    order: VecDeque<BrickAtlasResourceKey>,
    atlases: HashMap<BrickAtlasResourceKey, GpuBrickAtlasF32Resource>,
    next_generation: u64,
    pub(super) stats: GpuRendererStats,
}

impl GpuRenderer {
    pub(super) fn cached_brick_atlas_u8(
        &self,
        volume: CurrentLeaseVolume<'_>,
        cpu_ledger: &dyn CpuByteLedger,
    ) -> Result<GpuBrickAtlasResource, GpuRenderError> {
        let key = BrickAtlasResourceKey::from_lease_volume(
            volume,
            ResourceRepresentation::BrickedU8Atlas,
        )?;
        self.brick_atlas_cache
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?
            .get_or_update_u8(&self.device, &self.queue, key, volume, cpu_ledger, None)
    }

    pub(super) fn cached_brick_atlas_u8_with_page_priorities(
        &self,
        volume: CurrentLeaseVolume<'_>,
        cpu_ledger: &dyn CpuByteLedger,
        page_priorities: &HashMap<ResourceRegion, GpuBrickAtlasPagePriority>,
    ) -> Result<GpuBrickAtlasResource, GpuRenderError> {
        let key = BrickAtlasResourceKey::from_lease_volume(
            volume,
            ResourceRepresentation::BrickedU8Atlas,
        )?;
        self.brick_atlas_cache
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?
            .get_or_update_u8(
                &self.device,
                &self.queue,
                key,
                volume,
                cpu_ledger,
                Some(&page_priorities_by_index(volume, page_priorities)?),
            )
    }

    pub(super) fn cached_brick_atlas(
        &self,
        volume: CurrentLeaseVolume<'_>,
        cpu_ledger: &dyn CpuByteLedger,
    ) -> Result<GpuBrickAtlasResource, GpuRenderError> {
        let key = BrickAtlasResourceKey::from_lease_volume(
            volume,
            ResourceRepresentation::BrickedU16Atlas,
        )?;
        self.brick_atlas_cache
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?
            .get_or_update(&self.device, &self.queue, key, volume, cpu_ledger, None)
    }

    pub(super) fn cached_brick_atlas_with_page_priorities(
        &self,
        volume: CurrentLeaseVolume<'_>,
        cpu_ledger: &dyn CpuByteLedger,
        page_priorities: &HashMap<ResourceRegion, GpuBrickAtlasPagePriority>,
    ) -> Result<GpuBrickAtlasResource, GpuRenderError> {
        let key = BrickAtlasResourceKey::from_lease_volume(
            volume,
            ResourceRepresentation::BrickedU16Atlas,
        )?;
        self.brick_atlas_cache
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?
            .get_or_update(
                &self.device,
                &self.queue,
                key,
                volume,
                cpu_ledger,
                Some(&page_priorities_by_index(volume, page_priorities)?),
            )
    }

    pub(super) fn cached_brick_atlas_f32(
        &self,
        volume: CurrentLeaseVolume<'_>,
        cpu_ledger: &dyn CpuByteLedger,
    ) -> Result<GpuBrickAtlasF32Resource, GpuRenderError> {
        let key = BrickAtlasResourceKey::from_lease_volume(
            volume,
            ResourceRepresentation::BrickedF32Atlas,
        )?;
        self.brick_atlas_f32_cache
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?
            .get_or_update(&self.device, &self.queue, key, volume, cpu_ledger, None)
    }

    pub(super) fn cached_brick_atlas_f32_with_page_priorities(
        &self,
        volume: CurrentLeaseVolume<'_>,
        cpu_ledger: &dyn CpuByteLedger,
        page_priorities: &HashMap<ResourceRegion, GpuBrickAtlasPagePriority>,
    ) -> Result<GpuBrickAtlasF32Resource, GpuRenderError> {
        let key = BrickAtlasResourceKey::from_lease_volume(
            volume,
            ResourceRepresentation::BrickedF32Atlas,
        )?;
        self.brick_atlas_f32_cache
            .lock()
            .map_err(|_| GpuRenderError::CachePoisoned)?
            .get_or_update(
                &self.device,
                &self.queue,
                key,
                volume,
                cpu_ledger,
                Some(&page_priorities_by_index(volume, page_priorities)?),
            )
    }

    pub fn brick_atlas_residency(
        &self,
        key: &BrickAtlasResourceKey,
    ) -> Result<GpuBrickAtlasResidencySnapshot, GpuRenderError> {
        match key.representation {
            ResourceRepresentation::BrickedU8Atlas | ResourceRepresentation::BrickedU16Atlas => {
                self.brick_atlas_cache
                    .lock()
                    .map_err(|_| GpuRenderError::CachePoisoned)?
                    .residency_snapshot(key)
            }
            ResourceRepresentation::BrickedF32Atlas => self
                .brick_atlas_f32_cache
                .lock()
                .map_err(|_| GpuRenderError::CachePoisoned)?
                .residency_snapshot(key),
        }
    }
}

impl GpuBrickAtlasResource {
    fn new(
        device: &wgpu::Device,
        cpu_ledger: &dyn CpuByteLedger,
        generation: u64,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        slot_count: usize,
        dtype: IntegerAtlasDType,
    ) -> Result<Self, GpuRenderError> {
        let brick_voxel_count = brick_shape.element_count().map_err(RenderError::from)?;
        if brick_voxel_count == 0 {
            return Err(RenderError::InvalidBrickAtlas("brick shape has zero voxels").into());
        }
        let page_count = brick_grid_shape
            .element_count()
            .map_err(RenderError::from)? as usize;
        let packed_u32_per_brick = dtype.packed_u32_per_brick(brick_voxel_count);
        let valid_u32_per_brick = validity_u32_per_brick(brick_voxel_count);
        let packed_values_len = (slot_count as u64)
            .checked_mul(packed_u32_per_brick)
            .ok_or(RenderError::InvalidBrickAtlas(
                "brick atlas packed value count overflow",
            ))?;
        let validity_len = (slot_count as u64).checked_mul(valid_u32_per_brick).ok_or(
            RenderError::InvalidBrickAtlas("brick atlas validity bit count overflow"),
        )?;
        let packed_values_bytes = checked_u64_buffer_byte_count(
            dtype.value_resource(),
            packed_values_len,
            std::mem::size_of::<u32>() as u64,
        )?;
        let validity_bytes = checked_u64_buffer_byte_count(
            "brick atlas integer validity bitset",
            validity_len,
            std::mem::size_of::<u32>() as u64,
        )?;
        let page_table_bytes = checked_buffer_byte_count(
            "brick atlas page table",
            page_count,
            std::mem::size_of::<u32>(),
        )?;
        let metadata_len = (page_count as u64)
            .checked_mul(INTEGER_BRICK_METADATA_WORDS)
            .ok_or(RenderError::InvalidBrickAtlas(
                "brick atlas metadata count overflow",
            ))? as usize;
        let metadata_bytes = checked_buffer_byte_count(
            "brick atlas integer metadata",
            metadata_len,
            std::mem::size_of::<u32>(),
        )?;
        let cpu_mapping_bytes = page_table_bytes.checked_add(metadata_bytes).ok_or(
            GpuRenderError::BufferSizeOverflow {
                resource: "brick atlas CPU mapping mirrors",
            },
        )?;
        let cpu_mapping_charge: Arc<dyn CpuByteLease> =
            Arc::from(cpu_ledger.try_acquire(CpuLedgerCategory::UploadStaging, cpu_mapping_bytes)?);
        let page_table = vec![0u32; page_count];
        let metadata = vec![0u32; metadata_len];
        let limits = device.limits();
        validate_storage_buffer_bytes(&limits, dtype.value_resource(), packed_values_bytes)?;
        validate_storage_buffer_bytes(
            &limits,
            "brick atlas integer validity bitset",
            validity_bytes,
        )?;
        validate_storage_buffer_bytes(&limits, "brick atlas page table", page_table_bytes)?;
        validate_storage_buffer_bytes(&limits, "brick atlas integer metadata", metadata_bytes)?;
        let packed_values_buffer = Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(dtype.buffer_label()),
            size: packed_values_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        let validity_buffer = Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-brick-atlas-integer-validity"),
            size: validity_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        let page_table_buffer = Arc::new(device.create_buffer_init(
            &wgpu::util::BufferInitDescriptor {
                label: Some("mirante4d-brick-page-table"),
                contents: bytemuck::cast_slice(&page_table),
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
            },
        ));
        let metadata_buffer = Arc::new(device.create_buffer_init(
            &wgpu::util::BufferInitDescriptor {
                label: Some("mirante4d-brick-integer-metadata"),
                contents: bytemuck::cast_slice(&metadata),
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
            },
        ));
        Ok(Self {
            generation,
            packed_values_buffer,
            validity_buffer,
            page_table_buffer,
            metadata_buffer,
            bytes: packed_values_bytes + validity_bytes + page_table_bytes + metadata_bytes,
            dtype,
            brick_shape,
            brick_grid_shape,
            brick_voxel_count,
            packed_u32_per_brick,
            valid_u32_per_brick,
            values_per_word: dtype.values_per_word(),
            bits_per_value: dtype.bits_per_value(),
            value_mask: dtype.value_mask(),
            slot_count,
            page_table,
            metadata,
            page_slots: HashMap::new(),
            page_regions: HashMap::new(),
            page_priorities: HashMap::new(),
            active_pages: HashSet::new(),
            slot_pages: vec![None; slot_count],
            page_lru: VecDeque::new(),
            _cpu_mapping_charge: cpu_mapping_charge,
        })
    }

    fn update_resident_pages_u8(
        &mut self,
        queue: &wgpu::Queue,
        volume: CurrentLeaseVolume<'_>,
        upload_ready_cache: &mut UploadReadyIntegerBrickCache,
        cpu_ledger: &dyn CpuByteLedger,
        page_priorities: Option<&HashMap<AtlasPageIndex, GpuBrickAtlasPagePriority>>,
    ) -> Result<GpuBrickAtlasUpdate, GpuRenderError> {
        debug_assert_eq!(self.dtype, IntegerAtlasDType::U8);
        self.update_resident_integer_pages(
            queue,
            volume,
            upload_ready_cache,
            cpu_ledger,
            page_priorities,
        )
    }

    fn update_resident_pages_u16(
        &mut self,
        queue: &wgpu::Queue,
        volume: CurrentLeaseVolume<'_>,
        upload_ready_cache: &mut UploadReadyIntegerBrickCache,
        cpu_ledger: &dyn CpuByteLedger,
        page_priorities: Option<&HashMap<AtlasPageIndex, GpuBrickAtlasPagePriority>>,
    ) -> Result<GpuBrickAtlasUpdate, GpuRenderError> {
        debug_assert_eq!(self.dtype, IntegerAtlasDType::U16);
        self.update_resident_integer_pages(
            queue,
            volume,
            upload_ready_cache,
            cpu_ledger,
            page_priorities,
        )
    }

    fn update_resident_integer_pages(
        &mut self,
        queue: &wgpu::Queue,
        volume: CurrentLeaseVolume<'_>,
        upload_ready_cache: &mut UploadReadyIntegerBrickCache,
        cpu_ledger: &dyn CpuByteLedger,
        page_priorities: Option<&HashMap<AtlasPageIndex, GpuBrickAtlasPagePriority>>,
    ) -> Result<GpuBrickAtlasUpdate, GpuRenderError> {
        let pages =
            validate_lease_pages(volume, self.dtype.intensity_dtype(), Some(self.slot_count))?;
        let current_pages = pages
            .iter()
            .map(|page| page.brick_index)
            .collect::<HashSet<_>>();
        let missing_pages = pages
            .iter()
            .filter(|page| !self.page_slots.contains_key(&page.brick_index))
            .count();
        let changed_pages = pages
            .iter()
            .filter(|page| !self.page_region_matches(page.brick_index, page.region))
            .count();
        if missing_pages == 0
            && changed_pages == 0
            && current_pages_match_active_pages(&current_pages, &self.active_pages)
        {
            return Ok(GpuBrickAtlasUpdate::default());
        }

        let mut page_table_updates = Vec::new();
        let mut metadata_updates = Vec::new();
        self.deactivate_pages_not_in(
            &current_pages,
            &mut page_table_updates,
            &mut metadata_updates,
        );
        let evicted_pages = self.evict_pages_for_missing(missing_pages, &current_pages);
        let uploaded_bytes_per_page = checked_u64_buffer_byte_count(
            "brick atlas uploaded integer value page",
            self.packed_u32_per_brick,
            std::mem::size_of::<u32>() as u64,
        )?
        .checked_add(checked_u64_buffer_byte_count(
            "brick atlas uploaded integer validity page",
            self.valid_u32_per_brick,
            std::mem::size_of::<u32>() as u64,
        )?)
        .ok_or(GpuRenderError::BufferSizeOverflow {
            resource: "brick atlas uploaded integer bytes",
        })?;
        let mut pending_uploads = Vec::new();
        let mut uploaded_pages = 0_u64;

        for page in &pages {
            if let Some(slot) = self.page_slots.get(&page.brick_index).copied() {
                let region_changed = !self.page_region_matches(page.brick_index, page.region);
                if region_changed {
                    let packed = upload_ready_cache.get_or_pack(
                        *page,
                        self.brick_shape,
                        self.packed_u32_per_brick,
                        self.valid_u32_per_brick,
                        self.dtype,
                        cpu_ledger,
                    )?;
                    write_integer_page_metadata(
                        &mut self.metadata,
                        brick_page_index(page.brick_index, self.brick_grid_shape),
                        &packed,
                    );
                    pending_uploads.push((slot, packed));
                    self.page_regions.insert(page.brick_index, page.region);
                    uploaded_pages += 1;
                    metadata_updates
                        .push(brick_page_index(page.brick_index, self.brick_grid_shape));
                }
                let became_active = self.active_pages.insert(page.brick_index);
                self.set_page_priority(page.brick_index, page_priorities);
                let page_index = brick_page_index(page.brick_index, self.brick_grid_shape);
                if became_active || region_changed {
                    self.page_table[page_index] = u32::try_from(slot + 1).map_err(|_| {
                        RenderError::InvalidBrickAtlas("brick atlas slot exceeds u32")
                    })?;
                    page_table_updates.push(page_index);
                    if became_active && !region_changed {
                        metadata_updates.push(page_index);
                    }
                }
                self.touch_page(page.brick_index);
                continue;
            }

            let slot = self.free_slot().ok_or(RenderError::InvalidBrickAtlas(
                "brick atlas has no free slot for required lease-backed page",
            ))?;
            let packed = upload_ready_cache.get_or_pack(
                *page,
                self.brick_shape,
                self.packed_u32_per_brick,
                self.valid_u32_per_brick,
                self.dtype,
                cpu_ledger,
            )?;
            let page_index = brick_page_index(page.brick_index, self.brick_grid_shape);
            write_integer_page_metadata(&mut self.metadata, page_index, &packed);
            pending_uploads.push((slot, packed));
            self.page_slots.insert(page.brick_index, slot);
            self.page_regions.insert(page.brick_index, page.region);
            self.set_page_priority(page.brick_index, page_priorities);
            self.active_pages.insert(page.brick_index);
            self.slot_pages[slot] = Some(page.brick_index);
            self.page_table[page_index] = u32::try_from(slot + 1)
                .map_err(|_| RenderError::InvalidBrickAtlas("brick atlas slot exceeds u32"))?;
            page_table_updates.push(page_index);
            metadata_updates.push(page_index);
            self.touch_page(page.brick_index);
            uploaded_pages += 1;
        }

        let uploaded_bytes = uploaded_pages.checked_mul(uploaded_bytes_per_page).ok_or(
            GpuRenderError::BufferSizeOverflow {
                resource: "brick atlas uploaded integer bytes",
            },
        )?;
        self.upload_packed_bricks(queue, &pending_uploads);
        self.upload_integer_page_table_entries(queue, &page_table_updates);
        self.upload_integer_metadata_entries(queue, &metadata_updates);
        let page_table_bytes_written =
            integer_mapping_update_bytes(page_table_updates.len(), metadata_updates.len())?;
        Ok(GpuBrickAtlasUpdate {
            uploaded_pages,
            uploaded_bytes,
            evicted_pages: evicted_pages.len() as u64,
            page_table_rebuilds: (!page_table_updates.is_empty()
                || !metadata_updates.is_empty()
                || !pending_uploads.is_empty()) as u64,
            page_table_bytes_written,
        })
    }

    fn deactivate_pages_not_in(
        &mut self,
        current_pages: &HashSet<AtlasPageIndex>,
        page_table_updates: &mut Vec<usize>,
        _metadata_updates: &mut Vec<usize>,
    ) {
        let inactive = self
            .active_pages
            .iter()
            .copied()
            .filter(|page| !current_pages.contains(page))
            .collect::<Vec<_>>();
        for page in inactive {
            self.active_pages.remove(&page);
            let page_index = brick_page_index(page, self.brick_grid_shape);
            self.page_table[page_index] = 0;
            page_table_updates.push(page_index);
        }
    }

    fn evict_pages_for_missing(
        &mut self,
        missing_pages: usize,
        current_pages: &HashSet<AtlasPageIndex>,
    ) -> Vec<AtlasPageIndex> {
        let mut evicted = Vec::new();
        for candidate in
            prioritized_eviction_candidates(&self.page_lru, current_pages, &self.page_priorities)
        {
            if self.free_slot_count() >= missing_pages {
                break;
            }
            self.remove_page(candidate);
            evicted.push(candidate);
        }
        evicted
    }

    fn remove_page(&mut self, page: AtlasPageIndex) {
        if let Some(slot) = self.page_slots.remove(&page) {
            self.page_regions.remove(&page);
            self.page_priorities.remove(&page);
            self.active_pages.remove(&page);
            self.slot_pages[slot] = None;
            let page_index = brick_page_index(page, self.brick_grid_shape);
            self.page_table[page_index] = 0;
            self.page_lru.retain(|candidate| *candidate != page);
        }
    }

    fn free_slot(&self) -> Option<usize> {
        self.slot_pages.iter().position(Option::is_none)
    }

    fn free_slot_count(&self) -> usize {
        self.slot_pages.iter().filter(|slot| slot.is_none()).count()
    }

    fn touch_page(&mut self, page: AtlasPageIndex) {
        self.page_lru.retain(|candidate| *candidate != page);
        self.page_lru.push_back(page);
    }

    fn set_page_priority(
        &mut self,
        page: AtlasPageIndex,
        page_priorities: Option<&HashMap<AtlasPageIndex, GpuBrickAtlasPagePriority>>,
    ) {
        if let Some(priority) = page_priorities.and_then(|priorities| priorities.get(&page)) {
            self.page_priorities.insert(page, *priority);
        }
    }

    fn page_region_matches(&self, page: AtlasPageIndex, region: ResourceRegion) -> bool {
        self.page_regions
            .get(&page)
            .is_some_and(|stored| *stored == region)
    }

    fn upload_packed_bricks(
        &self,
        queue: &wgpu::Queue,
        uploads: &[(usize, Arc<PackedIntegerBrick>)],
    ) {
        for (slot, packed) in uploads {
            let value_offset =
                (*slot as u64 * self.packed_u32_per_brick * std::mem::size_of::<u32>() as u64)
                    as wgpu::BufferAddress;
            queue.write_buffer(
                self.packed_values_buffer.as_ref(),
                value_offset,
                bytemuck::cast_slice(&packed.values),
            );
            let validity_offset =
                (*slot as u64 * self.valid_u32_per_brick * std::mem::size_of::<u32>() as u64)
                    as wgpu::BufferAddress;
            queue.write_buffer(
                self.validity_buffer.as_ref(),
                validity_offset,
                bytemuck::cast_slice(&packed.validity_bits),
            );
        }
    }

    fn upload_integer_page_table_entries(&self, queue: &wgpu::Queue, page_indices: &[usize]) {
        for page_index in page_indices {
            let Some(entry) = self.page_table.get(*page_index) else {
                continue;
            };
            let offset =
                (*page_index as u64 * std::mem::size_of::<u32>() as u64) as wgpu::BufferAddress;
            queue.write_buffer(
                self.page_table_buffer.as_ref(),
                offset,
                bytemuck::bytes_of(entry),
            );
        }
    }

    fn upload_integer_metadata_entries(&self, queue: &wgpu::Queue, page_indices: &[usize]) {
        let words_per_page = INTEGER_BRICK_METADATA_WORDS as usize;
        for page_index in page_indices {
            let base = page_index.saturating_mul(words_per_page);
            let end = base.saturating_add(words_per_page);
            let Some(entry) = self.metadata.get(base..end) else {
                continue;
            };
            let offset = (base as u64 * std::mem::size_of::<u32>() as u64) as wgpu::BufferAddress;
            queue.write_buffer(
                self.metadata_buffer.as_ref(),
                offset,
                bytemuck::cast_slice(entry),
            );
        }
    }
}

impl GpuBrickAtlasF32Resource {
    fn new(
        device: &wgpu::Device,
        cpu_ledger: &dyn CpuByteLedger,
        generation: u64,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        value_word_capacity: u64,
    ) -> Result<Self, GpuRenderError> {
        let brick_voxel_count = brick_shape.element_count().map_err(RenderError::from)?;
        if brick_voxel_count == 0 {
            return Err(RenderError::InvalidBrickAtlas("brick shape has zero voxels").into());
        }
        let values_len = value_word_capacity.max(1);
        let values_bytes = checked_u64_buffer_byte_count(
            "brick atlas float32 values",
            values_len,
            std::mem::size_of::<f32>() as u64,
        )?;
        let page_table_word_count = f32_page_table_word_count(brick_grid_shape)?;
        let page_table_len = usize::try_from(page_table_word_count).map_err(|_| {
            GpuRenderError::BufferSizeOverflow {
                resource: "brick atlas float32 page table",
            }
        })?;
        let page_table_bytes = checked_buffer_byte_count(
            "brick atlas float32 page table",
            page_table_len,
            std::mem::size_of::<u32>(),
        )?;
        let cpu_mapping_charge: Arc<dyn CpuByteLease> =
            Arc::from(cpu_ledger.try_acquire(CpuLedgerCategory::UploadStaging, page_table_bytes)?);
        let page_table = vec![0u32; page_table_len];
        let limits = device.limits();
        validate_storage_buffer_bytes(&limits, "brick atlas float32 values", values_bytes)?;
        validate_storage_buffer_bytes(&limits, "brick atlas float32 page table", page_table_bytes)?;
        let values_buffer = Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-brick-atlas-f32-values"),
            size: values_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        let page_table_buffer = Arc::new(device.create_buffer_init(
            &wgpu::util::BufferInitDescriptor {
                label: Some("mirante4d-brick-f32-page-table"),
                contents: bytemuck::cast_slice(&page_table),
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
            },
        ));
        Ok(Self {
            generation,
            values_buffer,
            page_table_buffer,
            bytes: values_bytes + page_table_bytes,
            brick_shape,
            brick_grid_shape,
            brick_voxel_count,
            value_words_used: 0,
            values_word_capacity: values_len,
            page_table_word_count,
            page_table,
            page_allocations: HashMap::new(),
            page_priorities: HashMap::new(),
            active_pages: HashSet::new(),
            page_lru: VecDeque::new(),
            _cpu_mapping_charge: cpu_mapping_charge,
        })
    }

    fn update_resident_pages(
        &mut self,
        queue: &wgpu::Queue,
        volume: CurrentLeaseVolume<'_>,
        cpu_ledger: &dyn CpuByteLedger,
        page_priorities: Option<&HashMap<AtlasPageIndex, GpuBrickAtlasPagePriority>>,
    ) -> Result<GpuBrickAtlasUpdate, GpuRenderError> {
        let pages = validate_lease_pages(volume, IntensityDType::Float32, None)?;
        let current_pages = pages
            .iter()
            .map(|page| page.brick_index)
            .collect::<HashSet<_>>();
        let mut page_table_updates = Vec::new();
        self.deactivate_pages_not_in(&current_pages, &mut page_table_updates);
        let mut uploaded_pages = 0u64;
        let mut uploaded_bytes = 0u64;
        let mut stale_allocations = 0u64;

        for page in &pages {
            let page_index = brick_page_index(page.brick_index, self.brick_grid_shape);
            if let Some(allocation) = self.page_allocations.get(&page.brick_index).copied() {
                if allocation.matches_resource_region(page.region) {
                    let became_active = self.active_pages.insert(page.brick_index);
                    self.set_page_priority(page.brick_index, page_priorities);
                    if became_active {
                        write_f32_brick_page_table(&mut self.page_table, page_index, allocation);
                        page_table_updates.push(page_index);
                    }
                    self.touch_page(page.brick_index);
                    continue;
                }
                self.page_allocations.remove(&page.brick_index);
                self.page_priorities.remove(&page.brick_index);
                self.page_lru
                    .retain(|candidate| *candidate != page.brick_index);
                stale_allocations = stale_allocations.saturating_add(1);
            }

            let allocation =
                self.upload_compact_page(queue, self.value_words_used, *page, cpu_ledger)?;
            self.value_words_used = self
                .value_words_used
                .checked_add(allocation.value_words)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                    resource: "brick atlas uploaded float32 words",
                })?;
            uploaded_bytes = uploaded_bytes
                .checked_add(allocation.value_words * std::mem::size_of::<f32>() as u64)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                    resource: "brick atlas uploaded float32 bytes",
                })?;
            self.page_allocations.insert(page.brick_index, allocation);
            self.set_page_priority(page.brick_index, page_priorities);
            self.active_pages.insert(page.brick_index);
            write_f32_brick_page_table(&mut self.page_table, page_index, allocation);
            self.touch_page(page.brick_index);
            page_table_updates.push(page_index);
            uploaded_pages += 1;
        }

        self.upload_f32_page_table_entries(queue, &page_table_updates);
        let page_table_bytes_written = f32_mapping_update_bytes(page_table_updates.len())?;
        Ok(GpuBrickAtlasUpdate {
            uploaded_pages,
            uploaded_bytes,
            evicted_pages: stale_allocations,
            page_table_rebuilds: (!page_table_updates.is_empty() || uploaded_pages > 0) as u64,
            page_table_bytes_written,
        })
    }

    fn deactivate_pages_not_in(
        &mut self,
        current_pages: &HashSet<AtlasPageIndex>,
        page_table_updates: &mut Vec<usize>,
    ) {
        let inactive = self
            .active_pages
            .iter()
            .copied()
            .filter(|page| !current_pages.contains(page))
            .collect::<Vec<_>>();
        for page in inactive {
            self.active_pages.remove(&page);
            let page_index = brick_page_index(page, self.brick_grid_shape);
            clear_f32_brick_page_table(&mut self.page_table, page_index);
            page_table_updates.push(page_index);
        }
    }

    fn touch_page(&mut self, page: AtlasPageIndex) {
        self.page_lru.retain(|candidate| *candidate != page);
        self.page_lru.push_back(page);
    }

    fn set_page_priority(
        &mut self,
        page: AtlasPageIndex,
        page_priorities: Option<&HashMap<AtlasPageIndex, GpuBrickAtlasPagePriority>>,
    ) {
        if let Some(priority) = page_priorities.and_then(|priorities| priorities.get(&page)) {
            self.page_priorities.insert(page, *priority);
        }
    }

    fn upload_compact_page(
        &self,
        queue: &wgpu::Queue,
        value_offset_words: u64,
        page: LeaseAtlasPage<'_>,
        cpu_ledger: &dyn CpuByteLedger,
    ) -> Result<GpuF32BrickAllocation, GpuRenderError> {
        let values = F32UploadBytes::new(page.payload, cpu_ledger)?;
        let value_words = page.payload.sample_count();
        let end_words = value_offset_words.checked_add(value_words).ok_or(
            GpuRenderError::BufferSizeOverflow {
                resource: "brick atlas float32 compact upload",
            },
        )?;
        if end_words > self.values_word_capacity {
            return Err(GpuRenderError::BufferTooLarge {
                resource: "brick atlas float32 compact values",
                required_bytes: end_words * std::mem::size_of::<f32>() as u64,
                limit_bytes: self.values_word_capacity * std::mem::size_of::<f32>() as u64,
            });
        }
        let offset = value_offset_words
            .checked_mul(std::mem::size_of::<f32>() as u64)
            .ok_or(GpuRenderError::BufferSizeOverflow {
                resource: "brick atlas float32 compact upload offset",
            })? as wgpu::BufferAddress;
        queue.write_buffer(self.values_buffer.as_ref(), offset, values.bytes());
        if value_offset_words >= u64::from(u32::MAX) {
            return Err(GpuRenderError::BufferSizeOverflow {
                resource: "brick atlas float32 compact value offset",
            });
        }
        Ok(GpuF32BrickAllocation {
            value_offset_words: u32::try_from(value_offset_words).map_err(|_| {
                GpuRenderError::BufferSizeOverflow {
                    resource: "brick atlas float32 compact value offset",
                }
            })?,
            value_words,
            x_size: checked_u32("f32_resource_x_size", page.payload.shape().x())?,
            y_size: checked_u32("f32_resource_y_size", page.payload.shape().y())?,
            z_size: checked_u32("f32_resource_z_size", page.payload.shape().z())?,
            x_start: checked_u32("f32_resource_x_start", page.region.origin()[2])?,
            y_start: checked_u32("f32_resource_y_start", page.region.origin()[1])?,
            z_start: checked_u32("f32_resource_z_start", page.region.origin()[0])?,
        })
    }

    fn upload_f32_page_table_entries(&self, queue: &wgpu::Queue, page_indices: &[usize]) {
        let words_per_page = F32_BRICK_PAGE_TABLE_WORDS as usize;
        for page_index in page_indices {
            let base = page_index.saturating_mul(words_per_page);
            let end = base.saturating_add(words_per_page);
            let Some(entry) = self.page_table.get(base..end) else {
                continue;
            };
            let offset = (base as u64 * std::mem::size_of::<u32>() as u64) as wgpu::BufferAddress;
            queue.write_buffer(
                self.page_table_buffer.as_ref(),
                offset,
                bytemuck::cast_slice(entry),
            );
        }
    }
}

impl GpuF32BrickAllocation {
    fn matches_resource_region(self, region: ResourceRegion) -> bool {
        let origin = region.origin();
        let shape = region.shape();
        self.x_start as u64 == origin[2]
            && self.y_start as u64 == origin[1]
            && self.z_start as u64 == origin[0]
            && self.x_size as u64 == shape.x()
            && self.y_size as u64 == shape.y()
            && self.z_size as u64 == shape.z()
    }

    fn resource_region(self) -> ResourceRegion {
        ResourceRegion::new(
            [
                u64::from(self.z_start),
                u64::from(self.y_start),
                u64::from(self.x_start),
            ],
            Shape3D::new(
                u64::from(self.z_size),
                u64::from(self.y_size),
                u64::from(self.x_size),
            )
            .expect("a live float32 allocation has nonzero dimensions"),
        )
        .expect("a live float32 allocation preserves checked region ends")
    }
}

impl GpuBrickAtlasCache {
    pub(super) fn new(max_bytes: u64) -> Self {
        let upload_ready_budget = upload_ready_cache_budget(max_bytes);
        let stats = GpuRendererStats {
            upload_ready_brick_cache_budget_bytes: upload_ready_budget,
            ..GpuRendererStats::default()
        };
        Self {
            max_bytes,
            current_bytes: 0,
            current_u8_bytes: 0,
            current_u16_bytes: 0,
            order: VecDeque::new(),
            atlases: HashMap::new(),
            next_generation: 1,
            upload_ready_cache: UploadReadyIntegerBrickCache::new(upload_ready_budget),
            stats,
        }
    }

    fn next_resource_generation(&mut self) -> Result<u64, GpuRenderError> {
        let generation = self.next_generation;
        self.next_generation =
            self.next_generation
                .checked_add(1)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                    resource: "brick atlas resource generation",
                })?;
        Ok(generation)
    }

    fn new_integer_resource(
        &mut self,
        device: &wgpu::Device,
        cpu_ledger: &dyn CpuByteLedger,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        required_slot_count: usize,
        dtype: IntegerAtlasDType,
    ) -> Result<GpuBrickAtlasResource, GpuRenderError> {
        GpuBrickAtlasResource::new(
            device,
            cpu_ledger,
            self.next_resource_generation()?,
            brick_shape,
            brick_grid_shape,
            required_slot_count,
            dtype,
        )
    }

    fn preferred_integer_slot_count(
        &self,
        limits: &wgpu::Limits,
        dtype: IntegerAtlasDType,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        required_slot_count: usize,
    ) -> Result<usize, GpuRenderError> {
        dtype.validate_budget(
            self.max_bytes,
            limits,
            brick_shape,
            brick_grid_shape,
            required_slot_count,
        )?;
        let mut selected = required_slot_count;
        for _ in 0..16 {
            let Some(candidate) = selected.checked_mul(2) else {
                return Ok(selected);
            };
            match dtype.validate_budget(
                self.max_bytes,
                limits,
                brick_shape,
                brick_grid_shape,
                candidate,
            ) {
                Ok(()) => selected = candidate,
                Err(
                    GpuRenderError::BudgetExceeded { .. } | GpuRenderError::BufferTooLarge { .. },
                ) => {
                    return self.largest_integer_slot_count_within_budget(
                        limits,
                        dtype,
                        brick_shape,
                        brick_grid_shape,
                        selected,
                        candidate.saturating_sub(1),
                    );
                }
                Err(err) => return Err(err),
            }
        }
        Ok(selected)
    }

    fn largest_integer_slot_count_within_budget(
        &self,
        limits: &wgpu::Limits,
        dtype: IntegerAtlasDType,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        lower_valid: usize,
        upper_candidate: usize,
    ) -> Result<usize, GpuRenderError> {
        let mut low = lower_valid;
        let mut high = upper_candidate.max(lower_valid);
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            match dtype.validate_budget(self.max_bytes, limits, brick_shape, brick_grid_shape, mid)
            {
                Ok(()) => low = mid,
                Err(
                    GpuRenderError::BudgetExceeded { .. } | GpuRenderError::BufferTooLarge { .. },
                ) => high = mid.saturating_sub(1),
                Err(err) => return Err(err),
            }
        }
        Ok(low)
    }

    fn integer_growth_slot_count(
        &self,
        limits: &wgpu::Limits,
        request: IntegerAtlasGrowthRequest,
    ) -> Result<Option<usize>, GpuRenderError> {
        if request.required_slot_count <= request.current_slot_count {
            return Ok(None);
        }
        match self.preferred_integer_slot_count(
            limits,
            request.dtype,
            request.brick_shape,
            request.brick_grid_shape,
            request.required_slot_count,
        ) {
            Ok(slot_count) => Ok(Some(slot_count)),
            Err(GpuRenderError::BudgetExceeded { .. } | GpuRenderError::BufferTooLarge { .. })
                if request.visible_slot_count <= request.current_slot_count =>
            {
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn get_or_update_u8(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: BrickAtlasResourceKey,
        volume: CurrentLeaseVolume<'_>,
        cpu_ledger: &dyn CpuByteLedger,
        page_priorities: Option<&HashMap<AtlasPageIndex, GpuBrickAtlasPagePriority>>,
    ) -> Result<GpuBrickAtlasResource, GpuRenderError> {
        let brick_shape = volume.resource_shape();
        let brick_grid_shape = volume.resource_grid_shape();
        let pages = validate_lease_pages(volume, IntensityDType::Uint8, None)?;
        let visible_slot_count = pages.len().max(1);
        let limits = device.limits();
        let mut resource = if let Some(atlas) = self.atlases.remove(&key) {
            self.stats.brick_atlas_cache_hits += 1;
            self.subtract_resource_bytes(&atlas);
            atlas
        } else {
            self.stats.brick_atlas_cache_misses += 1;
            let required_slot_count = self.preferred_integer_slot_count(
                &limits,
                IntegerAtlasDType::U8,
                brick_shape,
                brick_grid_shape,
                visible_slot_count,
            )?;
            self.new_integer_resource(
                device,
                cpu_ledger,
                brick_shape,
                brick_grid_shape,
                required_slot_count,
                IntegerAtlasDType::U8,
            )?
        };
        self.order.retain(|existing| existing != &key);

        let missing_pages = pages
            .iter()
            .filter(|page| !resource.page_slots.contains_key(&page.brick_index))
            .count();
        let required_slot_count = resource
            .page_slots
            .len()
            .saturating_add(missing_pages)
            .max(visible_slot_count)
            .max(1);
        if let Some(required_slot_count) = self.integer_growth_slot_count(
            &limits,
            IntegerAtlasGrowthRequest {
                dtype: IntegerAtlasDType::U8,
                brick_shape,
                brick_grid_shape,
                current_slot_count: resource.slot_count,
                required_slot_count,
                visible_slot_count,
            },
        )? {
            resource = self.new_integer_resource(
                device,
                cpu_ledger,
                brick_shape,
                brick_grid_shape,
                required_slot_count,
                IntegerAtlasDType::U8,
            )?;
        }

        let update = resource.update_resident_pages_u8(
            queue,
            volume,
            &mut self.upload_ready_cache,
            cpu_ledger,
            page_priorities,
        )?;
        self.record_update(resource.dtype, update);
        self.record_upload_ready_cache_stats();
        self.store_resource(key, resource)
    }

    #[allow(clippy::too_many_arguments)]
    fn get_or_update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: BrickAtlasResourceKey,
        volume: CurrentLeaseVolume<'_>,
        cpu_ledger: &dyn CpuByteLedger,
        page_priorities: Option<&HashMap<AtlasPageIndex, GpuBrickAtlasPagePriority>>,
    ) -> Result<GpuBrickAtlasResource, GpuRenderError> {
        let brick_shape = volume.resource_shape();
        let brick_grid_shape = volume.resource_grid_shape();
        let pages = validate_lease_pages(volume, IntensityDType::Uint16, None)?;
        let visible_slot_count = pages.len().max(1);
        let limits = device.limits();
        let mut resource = if let Some(atlas) = self.atlases.remove(&key) {
            self.stats.brick_atlas_cache_hits += 1;
            self.subtract_resource_bytes(&atlas);
            atlas
        } else {
            self.stats.brick_atlas_cache_misses += 1;
            let required_slot_count = self.preferred_integer_slot_count(
                &limits,
                IntegerAtlasDType::U16,
                brick_shape,
                brick_grid_shape,
                visible_slot_count,
            )?;
            self.new_integer_resource(
                device,
                cpu_ledger,
                brick_shape,
                brick_grid_shape,
                required_slot_count,
                IntegerAtlasDType::U16,
            )?
        };
        self.order.retain(|existing| existing != &key);

        let missing_pages = pages
            .iter()
            .filter(|page| !resource.page_slots.contains_key(&page.brick_index))
            .count();
        let required_slot_count = resource
            .page_slots
            .len()
            .saturating_add(missing_pages)
            .max(visible_slot_count)
            .max(1);
        if let Some(required_slot_count) = self.integer_growth_slot_count(
            &limits,
            IntegerAtlasGrowthRequest {
                dtype: IntegerAtlasDType::U16,
                brick_shape,
                brick_grid_shape,
                current_slot_count: resource.slot_count,
                required_slot_count,
                visible_slot_count,
            },
        )? {
            resource = self.new_integer_resource(
                device,
                cpu_ledger,
                brick_shape,
                brick_grid_shape,
                required_slot_count,
                IntegerAtlasDType::U16,
            )?;
        }

        let update = resource.update_resident_pages_u16(
            queue,
            volume,
            &mut self.upload_ready_cache,
            cpu_ledger,
            page_priorities,
        )?;
        self.record_update(resource.dtype, update);
        self.record_upload_ready_cache_stats();
        self.store_resource(key, resource)
    }

    fn record_update(&mut self, dtype: IntegerAtlasDType, update: GpuBrickAtlasUpdate) {
        self.stats.brick_atlas_uploads += update.uploaded_pages;
        self.stats.brick_atlas_uploaded_bytes += update.uploaded_bytes;
        match dtype {
            IntegerAtlasDType::U8 => {
                self.stats.brick_atlas_u8_uploaded_bytes += update.uploaded_bytes;
            }
            IntegerAtlasDType::U16 => {
                self.stats.brick_atlas_u16_uploaded_bytes += update.uploaded_bytes;
            }
        }
        self.stats.brick_atlas_evictions += update.evicted_pages;
        self.stats.brick_atlas_page_table_rebuilds += update.page_table_rebuilds;
        self.stats.brick_atlas_page_table_bytes_written += update.page_table_bytes_written;
    }

    fn store_resource(
        &mut self,
        key: BrickAtlasResourceKey,
        resource: GpuBrickAtlasResource,
    ) -> Result<GpuBrickAtlasResource, GpuRenderError> {
        self.store_resource_retained(key, resource.clone())?;
        Ok(resource)
    }

    fn store_resource_retained(
        &mut self,
        key: BrickAtlasResourceKey,
        resource: GpuBrickAtlasResource,
    ) -> Result<bool, GpuRenderError> {
        if resource.bytes > self.max_bytes {
            self.stats.brick_atlas_evictions += self.clear();
            self.record_resident_bytes();
            return Ok(false);
        }

        self.order.push_back(key.clone());
        self.add_resource_bytes(&resource);
        self.atlases.insert(key.clone(), resource);

        while self.current_bytes > self.max_bytes {
            let Some(evicted) = self.order.pop_front() else {
                break;
            };
            if let Some(atlas) = self.atlases.remove(&evicted) {
                self.subtract_resource_bytes(&atlas);
                self.stats.brick_atlas_evictions += 1;
            }
        }
        self.record_resident_bytes();
        Ok(self.atlases.contains_key(&key))
    }

    fn add_resource_bytes(&mut self, resource: &GpuBrickAtlasResource) {
        self.current_bytes += resource.bytes;
        match resource.dtype {
            IntegerAtlasDType::U8 => self.current_u8_bytes += resource.bytes,
            IntegerAtlasDType::U16 => self.current_u16_bytes += resource.bytes,
        }
    }

    fn subtract_resource_bytes(&mut self, resource: &GpuBrickAtlasResource) {
        self.current_bytes -= resource.bytes;
        match resource.dtype {
            IntegerAtlasDType::U8 => self.current_u8_bytes -= resource.bytes,
            IntegerAtlasDType::U16 => self.current_u16_bytes -= resource.bytes,
        }
    }

    fn record_resident_bytes(&mut self) {
        self.stats.brick_atlas_resident_bytes = self.current_bytes;
        self.stats.brick_atlas_u8_resident_bytes = self.current_u8_bytes;
        self.stats.brick_atlas_u16_resident_bytes = self.current_u16_bytes;
    }

    fn record_upload_ready_cache_stats(&mut self) {
        self.stats.upload_ready_brick_cache_budget_bytes = self.upload_ready_cache.max_bytes;
        self.stats.upload_ready_brick_cache_hits = self.upload_ready_cache.hits;
        self.stats.upload_ready_brick_cache_misses = self.upload_ready_cache.misses;
        self.stats.upload_ready_brick_cache_evictions = self.upload_ready_cache.evictions;
        self.stats.upload_ready_brick_cache_resident_bytes = self.upload_ready_cache.current_bytes;
    }

    fn clear(&mut self) -> u64 {
        let evictions = self.atlases.len() as u64;
        self.order.clear();
        self.atlases.clear();
        self.current_bytes = 0;
        self.current_u8_bytes = 0;
        self.current_u16_bytes = 0;
        evictions
    }

    fn residency_snapshot(
        &self,
        key: &BrickAtlasResourceKey,
    ) -> Result<GpuBrickAtlasResidencySnapshot, GpuRenderError> {
        let Some(atlas) = self.atlases.get(key) else {
            return Ok(GpuBrickAtlasResidencySnapshot {
                retained: false,
                generation: None,
                resident_pages: HashSet::new(),
                active_pages: HashSet::new(),
                bytes: 0,
                slot_count: 0,
            });
        };
        Ok(GpuBrickAtlasResidencySnapshot {
            retained: true,
            generation: Some(atlas.generation),
            resident_pages: atlas.page_regions.values().copied().collect(),
            active_pages: atlas
                .active_pages
                .iter()
                .filter_map(|page| atlas.page_regions.get(page).copied())
                .collect(),
            bytes: atlas.bytes,
            slot_count: atlas.slot_count,
        })
    }
}

impl GpuBrickAtlasF32Cache {
    pub(super) fn new(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            current_bytes: 0,
            order: VecDeque::new(),
            atlases: HashMap::new(),
            next_generation: 1,
            stats: GpuRendererStats::default(),
        }
    }

    fn next_resource_generation(&mut self) -> Result<u64, GpuRenderError> {
        let generation = self.next_generation;
        self.next_generation =
            self.next_generation
                .checked_add(1)
                .ok_or(GpuRenderError::BufferSizeOverflow {
                    resource: "float32 brick atlas resource generation",
                })?;
        Ok(generation)
    }

    fn new_f32_resource(
        &mut self,
        device: &wgpu::Device,
        cpu_ledger: &dyn CpuByteLedger,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        value_word_capacity: u64,
    ) -> Result<GpuBrickAtlasF32Resource, GpuRenderError> {
        GpuBrickAtlasF32Resource::new(
            device,
            cpu_ledger,
            self.next_resource_generation()?,
            brick_shape,
            brick_grid_shape,
            value_word_capacity,
        )
    }

    fn preferred_f32_value_word_capacity(
        &self,
        limits: &wgpu::Limits,
        required_value_words: u64,
        page_table_word_count: u64,
    ) -> Result<u64, GpuRenderError> {
        validate_f32_brick_atlas_budget(
            self.max_bytes,
            limits,
            required_value_words.max(1),
            page_table_word_count,
        )?;
        let mut selected = required_value_words.max(1);
        for _ in 0..16 {
            let Some(candidate) = selected.checked_mul(2) else {
                return Ok(selected);
            };
            match validate_f32_brick_atlas_budget(
                self.max_bytes,
                limits,
                candidate,
                page_table_word_count,
            ) {
                Ok(()) => selected = candidate,
                Err(
                    GpuRenderError::BudgetExceeded { .. } | GpuRenderError::BufferTooLarge { .. },
                ) => {
                    return self.largest_f32_value_word_capacity_within_budget(
                        limits,
                        selected,
                        candidate.saturating_sub(1),
                        page_table_word_count,
                    );
                }
                Err(err) => return Err(err),
            }
        }
        Ok(selected)
    }

    fn largest_f32_value_word_capacity_within_budget(
        &self,
        limits: &wgpu::Limits,
        lower_valid: u64,
        upper_candidate: u64,
        page_table_word_count: u64,
    ) -> Result<u64, GpuRenderError> {
        let mut low = lower_valid;
        let mut high = upper_candidate.max(lower_valid);
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            match validate_f32_brick_atlas_budget(
                self.max_bytes,
                limits,
                mid,
                page_table_word_count,
            ) {
                Ok(()) => low = mid,
                Err(
                    GpuRenderError::BudgetExceeded { .. } | GpuRenderError::BufferTooLarge { .. },
                ) => high = mid.saturating_sub(1),
                Err(err) => return Err(err),
            }
        }
        Ok(low)
    }

    #[allow(clippy::too_many_arguments)]
    fn get_or_update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: BrickAtlasResourceKey,
        volume: CurrentLeaseVolume<'_>,
        cpu_ledger: &dyn CpuByteLedger,
        page_priorities: Option<&HashMap<AtlasPageIndex, GpuBrickAtlasPagePriority>>,
    ) -> Result<GpuBrickAtlasF32Resource, GpuRenderError> {
        let brick_shape = volume.resource_shape();
        let brick_grid_shape = volume.resource_grid_shape();
        let pages = validate_lease_pages(volume, IntensityDType::Float32, None)?;
        let required_value_words = pages
            .iter()
            .try_fold(0_u64, |total, page| {
                total.checked_add(page.payload.sample_count()).ok_or(
                    GpuRenderError::BufferSizeOverflow {
                        resource: "lease-backed float32 atlas values",
                    },
                )
            })?
            .max(1);
        let page_table_word_count = f32_page_table_word_count(brick_grid_shape)?;
        let limits = device.limits();
        validate_f32_brick_atlas_budget(
            self.max_bytes,
            &limits,
            required_value_words,
            page_table_word_count,
        )?;
        let mut resource = if let Some(atlas) = self.atlases.remove(&key) {
            self.stats.brick_atlas_cache_hits += 1;
            self.current_bytes -= atlas.bytes;
            atlas
        } else {
            self.stats.brick_atlas_cache_misses += 1;
            let value_word_capacity = self.preferred_f32_value_word_capacity(
                &limits,
                required_value_words,
                page_table_word_count,
            )?;
            self.new_f32_resource(
                device,
                cpu_ledger,
                brick_shape,
                brick_grid_shape,
                value_word_capacity,
            )?
        };
        self.order.retain(|existing| existing != &key);

        let missing_value_words = missing_f32_value_words_for_resource(&resource, &pages)?;
        let required_resource_value_words = resource
            .value_words_used
            .checked_add(missing_value_words)
            .ok_or(GpuRenderError::BufferSizeOverflow {
                resource: "brick atlas float32 retained values",
            })?;
        if page_table_word_count != resource.page_table_word_count
            || required_resource_value_words > resource.values_word_capacity
        {
            let value_word_capacity = self.preferred_f32_value_word_capacity(
                &limits,
                required_value_words,
                page_table_word_count,
            )?;
            resource = self.new_f32_resource(
                device,
                cpu_ledger,
                brick_shape,
                brick_grid_shape,
                value_word_capacity,
            )?;
        }

        let update = resource.update_resident_pages(queue, volume, cpu_ledger, page_priorities)?;
        self.stats.brick_atlas_uploads += update.uploaded_pages;
        self.stats.brick_atlas_uploaded_bytes += update.uploaded_bytes;
        self.stats.brick_atlas_f32_uploaded_bytes += update.uploaded_bytes;
        self.stats.brick_atlas_evictions += update.evicted_pages;
        self.stats.brick_atlas_page_table_rebuilds += update.page_table_rebuilds;
        self.stats.brick_atlas_page_table_bytes_written += update.page_table_bytes_written;

        if resource.bytes > self.max_bytes {
            self.stats.brick_atlas_evictions += self.clear();
            self.stats.brick_atlas_resident_bytes = self.current_bytes;
            return Ok(resource);
        }

        self.order.push_back(key.clone());
        self.current_bytes += resource.bytes;
        self.atlases.insert(key.clone(), resource.clone());

        while self.current_bytes > self.max_bytes {
            let Some(evicted) = self.order.pop_front() else {
                break;
            };
            if let Some(atlas) = self.atlases.remove(&evicted) {
                self.current_bytes -= atlas.bytes;
                self.stats.brick_atlas_evictions += 1;
            }
        }
        self.stats.brick_atlas_resident_bytes = self.current_bytes;
        self.stats.brick_atlas_f32_resident_bytes = self.current_bytes;
        Ok(resource)
    }

    fn clear(&mut self) -> u64 {
        let evictions = self.atlases.len() as u64;
        self.order.clear();
        self.atlases.clear();
        self.current_bytes = 0;
        self.stats.brick_atlas_f32_resident_bytes = 0;
        evictions
    }

    fn residency_snapshot(
        &self,
        key: &BrickAtlasResourceKey,
    ) -> Result<GpuBrickAtlasResidencySnapshot, GpuRenderError> {
        let Some(atlas) = self.atlases.get(key) else {
            return Ok(GpuBrickAtlasResidencySnapshot {
                retained: false,
                generation: None,
                resident_pages: HashSet::new(),
                active_pages: HashSet::new(),
                bytes: 0,
                slot_count: 0,
            });
        };
        Ok(GpuBrickAtlasResidencySnapshot {
            retained: true,
            generation: Some(atlas.generation),
            resident_pages: atlas
                .page_allocations
                .values()
                .map(|allocation| allocation.resource_region())
                .collect(),
            active_pages: atlas
                .active_pages
                .iter()
                .filter_map(|page| atlas.page_allocations.get(page))
                .map(|allocation| allocation.resource_region())
                .collect(),
            bytes: atlas.bytes,
            slot_count: atlas.page_allocations.len(),
        })
    }
}

fn validate_lease_pages(
    volume: CurrentLeaseVolume<'_>,
    dtype: IntensityDType,
    slot_count: Option<usize>,
) -> Result<Vec<LeaseAtlasPage<'_>>, RenderError> {
    let resources = volume.resident().resources().collect::<Vec<_>>();
    if resources.is_empty() {
        return Err(RenderError::InvalidBrickAtlas(
            "lease-backed atlas input is empty",
        ));
    }
    if slot_count.is_some_and(|limit| resources.len() > limit) {
        return Err(RenderError::InvalidBrickAtlas(
            "lease-backed resource count exceeds atlas slot count",
        ));
    }
    let mut indices = HashSet::with_capacity(resources.len());
    let mut pages = Vec::with_capacity(resources.len());
    for resource in resources {
        let payload = resource.payload();
        if payload.dtype() != dtype {
            return Err(RenderError::InvalidBrickAtlas(
                "lease-backed atlas cohort contains an unexpected dtype",
            ));
        }
        let region = resource.key().region();
        if payload.shape() != region.shape() {
            return Err(RenderError::InvalidBrickAtlas(
                "lease payload shape does not match its semantic region",
            ));
        }
        let brick_index = atlas_page_for_region(volume, region)?;
        if !indices.insert(brick_index) {
            return Err(RenderError::InvalidBrickAtlas(
                "two lease resources map to one semantic atlas page",
            ));
        }
        pages.push(LeaseAtlasPage {
            brick_index,
            region,
            payload,
            resource,
        });
    }
    Ok(pages)
}

fn atlas_page_for_region(
    volume: CurrentLeaseVolume<'_>,
    region: ResourceRegion,
) -> Result<AtlasPageIndex, RenderError> {
    if !region.fits_within(volume.volume_shape()) {
        return Err(RenderError::InvalidBrickAtlas(
            "lease resource region exceeds the semantic volume",
        ));
    }
    let origin = region.origin();
    let resource_shape = volume.resource_shape().dimensions();
    if origin
        .into_iter()
        .zip(resource_shape)
        .any(|(start, length)| start % length != 0)
    {
        return Err(RenderError::InvalidBrickAtlas(
            "lease resource origin is not aligned to the semantic resource grid",
        ));
    }
    let index = AtlasPageIndex {
        z: origin[0] / resource_shape[0],
        y: origin[1] / resource_shape[1],
        x: origin[2] / resource_shape[2],
    };
    let grid = volume.resource_grid_shape();
    if index.z >= grid.z() || index.y >= grid.y() || index.x >= grid.x() {
        return Err(RenderError::InvalidBrickAtlas(
            "lease resource index exceeds the semantic resource grid",
        ));
    }
    let volume_shape = volume.volume_shape().dimensions();
    let expected_shape = Shape3D::new(
        resource_shape[0].min(volume_shape[0] - origin[0]),
        resource_shape[1].min(volume_shape[1] - origin[1]),
        resource_shape[2].min(volume_shape[2] - origin[2]),
    )
    .expect("an in-grid semantic resource is nonempty");
    if region.shape() != expected_shape {
        return Err(RenderError::InvalidBrickAtlas(
            "lease resource is not the exact full or clipped semantic tile",
        ));
    }
    Ok(index)
}

fn page_priorities_by_index(
    volume: CurrentLeaseVolume<'_>,
    priorities: &HashMap<ResourceRegion, GpuBrickAtlasPagePriority>,
) -> Result<HashMap<AtlasPageIndex, GpuBrickAtlasPagePriority>, RenderError> {
    priorities
        .iter()
        .map(|(region, priority)| Ok((atlas_page_for_region(volume, *region)?, *priority)))
        .collect()
}

fn current_pages_match_active_pages(
    current_pages: &HashSet<AtlasPageIndex>,
    active_pages: &HashSet<AtlasPageIndex>,
) -> bool {
    current_pages == active_pages
}

fn integer_mapping_update_bytes(
    page_table_entries: usize,
    metadata_entries: usize,
) -> Result<u64, GpuRenderError> {
    let page_table_bytes = checked_buffer_byte_count(
        "brick atlas page table entry upload",
        page_table_entries,
        std::mem::size_of::<u32>(),
    )?;
    let metadata_words = metadata_entries
        .checked_mul(INTEGER_BRICK_METADATA_WORDS as usize)
        .ok_or(GpuRenderError::BufferSizeOverflow {
            resource: "brick atlas integer metadata entry upload",
        })?;
    let metadata_bytes = checked_buffer_byte_count(
        "brick atlas integer metadata entry upload",
        metadata_words,
        std::mem::size_of::<u32>(),
    )?;
    page_table_bytes
        .checked_add(metadata_bytes)
        .ok_or(GpuRenderError::BufferSizeOverflow {
            resource: "brick atlas integer mapping entry upload",
        })
}

fn f32_mapping_update_bytes(page_table_entries: usize) -> Result<u64, GpuRenderError> {
    let words = page_table_entries
        .checked_mul(F32_BRICK_PAGE_TABLE_WORDS as usize)
        .ok_or(GpuRenderError::BufferSizeOverflow {
            resource: "brick atlas float32 page table entry upload",
        })?;
    checked_buffer_byte_count(
        "brick atlas float32 page table entry upload",
        words,
        std::mem::size_of::<u32>(),
    )
}

fn prioritized_eviction_candidates(
    page_lru: &VecDeque<AtlasPageIndex>,
    current_pages: &HashSet<AtlasPageIndex>,
    page_priorities: &HashMap<AtlasPageIndex, GpuBrickAtlasPagePriority>,
) -> Vec<AtlasPageIndex> {
    let mut candidates = page_lru
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, page)| !current_pages.contains(page))
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_index, left), (right_index, right)| {
        page_eviction_order(
            page_priorities.get(left).copied().unwrap_or_default(),
            page_priorities.get(right).copied().unwrap_or_default(),
        )
        .then_with(|| left_index.cmp(right_index))
        .then_with(|| (left.z, left.y, left.x).cmp(&(right.z, right.y, right.x)))
    });
    candidates
        .into_iter()
        .map(|(_, page)| page)
        .collect::<Vec<_>>()
}

fn page_eviction_order(
    left: GpuBrickAtlasPagePriority,
    right: GpuBrickAtlasPagePriority,
) -> Ordering {
    right
        .tier_rank
        .cmp(&left.tier_rank)
        .then_with(|| left.score.total_cmp(&right.score))
}

fn brick_page_index(index: AtlasPageIndex, brick_grid_shape: Shape3D) -> usize {
    ((index.z * brick_grid_shape.y() + index.y) * brick_grid_shape.x() + index.x) as usize
}

fn f32_page_table_word_count(brick_grid_shape: Shape3D) -> Result<u64, GpuRenderError> {
    brick_grid_shape
        .element_count()
        .map_err(RenderError::from)?
        .checked_mul(F32_BRICK_PAGE_TABLE_WORDS)
        .ok_or(GpuRenderError::BufferSizeOverflow {
            resource: "brick atlas float32 page table",
        })
}

fn missing_f32_value_words_for_resource(
    resource: &GpuBrickAtlasF32Resource,
    pages: &[LeaseAtlasPage<'_>],
) -> Result<u64, GpuRenderError> {
    pages.iter().try_fold(0u64, |total, page| {
        if resource
            .page_allocations
            .get(&page.brick_index)
            .is_some_and(|allocation| allocation.matches_resource_region(page.region))
        {
            return Ok(total);
        }
        total
            .checked_add(page.payload.sample_count())
            .ok_or(GpuRenderError::BufferSizeOverflow {
                resource: "brick atlas missing float32 compact values",
            })
    })
}

fn write_f32_brick_page_table(
    page_table: &mut [u32],
    page_index: usize,
    allocation: GpuF32BrickAllocation,
) {
    let base = page_index * F32_BRICK_PAGE_TABLE_WORDS as usize;
    if base + F32_BRICK_PAGE_TABLE_WORDS as usize > page_table.len() {
        return;
    }
    page_table[base] = allocation.value_offset_words + 1;
    page_table[base + 1] = allocation.x_size;
    page_table[base + 2] = allocation.y_size;
    page_table[base + 3] = allocation.z_size;
    page_table[base + 4] = allocation.x_start;
    page_table[base + 5] = allocation.y_start;
    page_table[base + 6] = allocation.z_start;
}

fn clear_f32_brick_page_table(page_table: &mut [u32], page_index: usize) {
    let base = page_index * F32_BRICK_PAGE_TABLE_WORDS as usize;
    if base + F32_BRICK_PAGE_TABLE_WORDS as usize > page_table.len() {
        return;
    }
    page_table[base..base + F32_BRICK_PAGE_TABLE_WORDS as usize].fill(0);
}

fn write_integer_page_metadata(
    metadata: &mut [u32],
    page_index: usize,
    packed: &PackedIntegerBrick,
) {
    write_integer_brick_metadata(
        metadata,
        page_index,
        packed.valid_voxel_count != 0,
        packed.valid_voxel_count,
        f64::from(packed.min_value),
        f64::from(packed.max_value),
    );
}

fn write_integer_brick_metadata(
    metadata: &mut [u32],
    page_index: usize,
    occupied: bool,
    valid_voxel_count: u64,
    min: f64,
    max: f64,
) {
    let base = page_index * INTEGER_BRICK_METADATA_WORDS as usize;
    if base + INTEGER_BRICK_METADATA_WORDS as usize > metadata.len() {
        return;
    }
    let has_valid = occupied && valid_voxel_count > 0;
    let min_max_valid = has_valid && min.is_finite() && max.is_finite() && max >= min;
    let mut flags = BRICK_METADATA_RESIDENT_FLAG;
    if has_valid {
        flags |= BRICK_METADATA_HAS_VALID_FLAG;
    }
    if min_max_valid {
        flags |= BRICK_METADATA_MIN_MAX_VALID_FLAG;
    }
    metadata[base] = flags;
    metadata[base + 1] = (min as f32).to_bits();
    metadata[base + 2] = (max as f32).to_bits();
    metadata[base + 3] = 0;
}

#[cfg(test)]
mod lease_tests;
