//! Persistent CPU authority for the renderer-global GPU residency directory.
//!
//! A dataset-generation change is a hard-clear boundary outside this module,
//! so the shader key contains only exact logical coordinates. One page record
//! may cover multiple regular logical-grid cells; every covered cell points to
//! that same page. Batch preparation is read-only and change-proportional.
//! Only the bounded tombstone-compaction/probe-recovery path allocates a full
//! replacement directory image.

use std::{
    collections::{BTreeMap, BTreeSet},
    mem::size_of,
};

use bytemuck::{Pod, Zeroable};
use mirante4d_dataset::BrickKey;
use mirante4d_render_api::RenderResourceGrid;
use thiserror::Error;

pub(crate) const DIRECTORY_SLOT_BYTES: u64 = 32;
pub(crate) const PAGE_RECORD_BYTES: u64 = 64;
pub(crate) const MAX_DIRECTORY_PROBES: u32 = 32;

const DIRECTORY_HASH_SEED: u32 = 0x4d34_4431;
const EMPTY_PAGE_WORD: u32 = 0;
const TOMBSTONE_PAGE_WORD: u32 = u32::MAX;
const MAX_DIRECTORY_CAPACITY: u32 = 1 << 31;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub(crate) enum GlobalResidencyError {
    #[error("the global residency page capacity is invalid")]
    InvalidPageCapacity,
    #[error("the global residency host allocation could not be represented or reserved")]
    HostAllocationCapacity,
    #[error("the global residency page-record population is full")]
    PageCapacity,
    #[error("the global residency directory is at its live-entry capacity")]
    DirectoryCapacity,
    #[error("the global residency directory exceeded its fixed probe bound")]
    DirectoryProbeBound,
    #[error("the compact-cell population could not be represented")]
    CellPopulationOverflow,
    #[error("the BrickKey layer does not match its logical resource grid")]
    GridLayerMismatch,
    #[error("the BrickKey scale does not match its logical resource grid")]
    GridScaleMismatch,
    #[error("the BrickKey region extends outside its logical resource grid")]
    RegionOutsideGrid,
    #[error("the BrickKey origin is not aligned to the logical resource grid on axis {axis}")]
    RegionMisaligned { axis: usize },
    #[error(
        "the BrickKey end is neither grid-aligned nor clipped at the volume edge on axis {axis}"
    )]
    RegionEndMisaligned { axis: usize },
    #[error("the compact cell coordinate exceeds u32 on axis {axis}")]
    CellCoordinateOverflow { axis: usize },
    #[error("a residency page must own a non-empty canonical compact-cell set")]
    EmptyCellSet,
    #[error("a residency page's compact-cell keys are duplicated or not in canonical order")]
    NonCanonicalCellSet,
    #[error("a residency mutation contains the same page more than once")]
    DuplicatePage,
    #[error("a residency mutation contains the same compact cell more than once")]
    DuplicateCell,
    #[error("page-record slot {page_index} is not resident")]
    MissingPage { page_index: u32 },
    #[error("page-record slot {page_index} does not own the supplied compact-cell set")]
    PageOwnershipMismatch { page_index: u32 },
    #[error("a resident compact cell does not resolve to its owning page")]
    DirectoryOwnershipMismatch,
    #[error("the page-record index cannot be represented by the directory ABI")]
    PageIndexOverflow,
    #[error("the residency mutation is empty")]
    EmptyBatch,
    #[error("the prepared residency batch no longer matches committed state")]
    StalePreparedBatch,
    #[error("the global residency revision is exhausted")]
    RevisionOverflow,
}

/// Exact shader-visible projection of one logical resource-grid cell.
///
/// Word order is frozen as layer ordinal, time low/high, scale, and logical
/// cell x/y/z. Dataset identity is deliberately absent because the renderer
/// hard-clears this directory before accepting another dataset generation.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Pod, Zeroable)]
pub(crate) struct CompactCellKey {
    words: [u32; 7],
}

impl CompactCellKey {
    const fn new(
        layer_ordinal: u32,
        timepoint: u64,
        scale: u32,
        cell_x: u32,
        cell_y: u32,
        cell_z: u32,
    ) -> Self {
        Self {
            words: [
                layer_ordinal,
                timepoint as u32,
                (timepoint >> 32) as u32,
                scale,
                cell_x,
                cell_y,
                cell_z,
            ],
        }
    }

    #[cfg(test)]
    pub(crate) const fn words(self) -> [u32; 7] {
        self.words
    }
}

const _: () = assert!(size_of::<CompactCellKey>() == 28);

/// Derives every exact logical cell covered by one `BrickKey`.
///
/// The region must start on the declared regular grid. Its exclusive end may
/// be clipped inside the final cell, and a larger semantic resource may span
/// any representable number of cells. The returned order is canonical by
/// compact key: x, then y, then z.
pub(crate) fn compact_cell_keys(
    key: BrickKey,
    grid: RenderResourceGrid,
) -> Result<Vec<CompactCellKey>, GlobalResidencyError> {
    if key.layer() != grid.layer() {
        return Err(GlobalResidencyError::GridLayerMismatch);
    }
    if key.scale() != grid.scale() {
        return Err(GlobalResidencyError::GridScaleMismatch);
    }

    let origin = key.region().origin();
    let end = key.region().end_exclusive();
    if !key.region().fits_within(grid.volume_shape()) {
        return Err(GlobalResidencyError::RegionOutsideGrid);
    }
    let volume_shape = grid.volume_shape().dimensions();
    let cell_shape = grid.cell_shape().dimensions();
    let mut first_cell = [0_u32; 3];
    let mut last_cell = [0_u32; 3];
    let mut axis_counts = [0_u64; 3];

    for axis in 0..3 {
        if !origin[axis].is_multiple_of(cell_shape[axis]) {
            return Err(GlobalResidencyError::RegionMisaligned { axis });
        }
        if end[axis] != volume_shape[axis] && !end[axis].is_multiple_of(cell_shape[axis]) {
            return Err(GlobalResidencyError::RegionEndMisaligned { axis });
        }
        let first = origin[axis] / cell_shape[axis];
        let last = end[axis]
            .checked_sub(1)
            .expect("a ResourceRegion is non-empty")
            / cell_shape[axis];
        first_cell[axis] = u32::try_from(first)
            .map_err(|_| GlobalResidencyError::CellCoordinateOverflow { axis })?;
        last_cell[axis] = u32::try_from(last)
            .map_err(|_| GlobalResidencyError::CellCoordinateOverflow { axis })?;
        axis_counts[axis] = last
            .checked_sub(first)
            .and_then(|distance| distance.checked_add(1))
            .ok_or(GlobalResidencyError::CellPopulationOverflow)?;
    }

    let cell_count = axis_counts
        .into_iter()
        .try_fold(1_u64, |product, count| product.checked_mul(count))
        .ok_or(GlobalResidencyError::CellPopulationOverflow)?;
    let cell_count =
        usize::try_from(cell_count).map_err(|_| GlobalResidencyError::CellPopulationOverflow)?;
    let mut compact = Vec::new();
    compact
        .try_reserve_exact(cell_count)
        .map_err(|_| GlobalResidencyError::HostAllocationCapacity)?;

    for cell_x in first_cell[2]..=last_cell[2] {
        for cell_y in first_cell[1]..=last_cell[1] {
            for cell_z in first_cell[0]..=last_cell[0] {
                compact.push(CompactCellKey::new(
                    key.layer().ordinal(),
                    key.timepoint().get(),
                    key.scale().get(),
                    cell_x,
                    cell_y,
                    cell_z,
                ));
            }
        }
    }
    debug_assert_eq!(compact.len(), cell_count);
    debug_assert!(cell_set_is_canonical(&compact));
    Ok(compact)
}

/// Exact MurmurHash3 x86-32 projection shared with the shader.
pub(crate) const fn directory_hash(key: CompactCellKey) -> u32 {
    let mut hash = DIRECTORY_HASH_SEED;
    let mut index = 0;
    while index < key.words.len() {
        let mut word = key.words[index];
        word = word.wrapping_mul(0xcc9e_2d51);
        word = word.rotate_left(15);
        word = word.wrapping_mul(0x1b87_3593);
        hash ^= word;
        hash = hash.rotate_left(13);
        hash = hash.wrapping_mul(5).wrapping_add(0xe654_6b64);
        index += 1;
    }
    hash ^= 28;
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x85eb_ca6b);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(0xc2b2_ae35);
    hash ^ (hash >> 16)
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub(crate) struct GpuDirectorySlot {
    words: [u32; 8],
}

impl GpuDirectorySlot {
    const fn empty() -> Self {
        Self { words: [0; 8] }
    }

    const fn tombstone() -> Self {
        let mut words = [0; 8];
        words[7] = TOMBSTONE_PAGE_WORD;
        Self { words }
    }

    fn occupied(key: CompactCellKey, page_record_index: u32) -> Result<Self, GlobalResidencyError> {
        let page_word = page_record_index
            .checked_add(1)
            .filter(|word| *word != TOMBSTONE_PAGE_WORD)
            .ok_or(GlobalResidencyError::PageIndexOverflow)?;
        let mut words = [0; 8];
        words[..7].copy_from_slice(&key.words);
        words[7] = page_word;
        Ok(Self { words })
    }

    const fn is_empty(self) -> bool {
        self.words[7] == EMPTY_PAGE_WORD
    }

    const fn is_tombstone(self) -> bool {
        self.words[7] == TOMBSTONE_PAGE_WORD
    }

    fn key(self) -> Option<CompactCellKey> {
        (!self.is_empty() && !self.is_tombstone()).then(|| CompactCellKey {
            words: self.words[..7]
                .try_into()
                .expect("a directory key occupies exactly seven words"),
        })
    }

    const fn page_record_index(self) -> Option<u32> {
        let page_word = self.words[7];
        if page_word == EMPTY_PAGE_WORD || page_word == TOMBSTONE_PAGE_WORD {
            None
        } else {
            Some(page_word - 1)
        }
    }

    #[cfg(test)]
    pub(crate) const fn words(self) -> [u32; 8] {
        self.words
    }
}

const _: () = assert!(size_of::<GpuDirectorySlot>() as u64 == DIRECTORY_SLOT_BYTES);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DirectorySlotWrite {
    slot_index: u32,
    slot: GpuDirectorySlot,
}

impl DirectorySlotWrite {
    pub(crate) const fn byte_offset(self) -> u64 {
        self.slot_index as u64 * DIRECTORY_SLOT_BYTES
    }

    pub(crate) const fn slot(&self) -> &GpuDirectorySlot {
        &self.slot
    }
}

#[derive(Debug)]
pub(crate) enum DirectoryPublication {
    Incremental {
        removal_writes: Vec<DirectorySlotWrite>,
        insertion_writes: Vec<DirectorySlotWrite>,
    },
    Rebuilt {
        slots: Vec<GpuDirectorySlot>,
    },
}

impl DirectoryPublication {
    #[cfg(test)]
    pub(crate) const fn is_rebuilt(&self) -> bool {
        matches!(self, Self::Rebuilt { .. })
    }

    #[cfg(test)]
    pub(crate) fn removal_writes(&self) -> &[DirectorySlotWrite] {
        match self {
            Self::Incremental { removal_writes, .. } => removal_writes,
            Self::Rebuilt { .. } => &[],
        }
    }

    #[cfg(test)]
    pub(crate) fn insertion_writes(&self) -> &[DirectorySlotWrite] {
        match self {
            Self::Incremental {
                insertion_writes, ..
            } => insertion_writes,
            Self::Rebuilt { .. } => &[],
        }
    }

    #[cfg(test)]
    pub(crate) fn rebuilt_slots(&self) -> Option<&[GpuDirectorySlot]> {
        match self {
            Self::Incremental { .. } => None,
            Self::Rebuilt { slots } => Some(slots),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DirectoryRemoval<'a> {
    page_index: u32,
    keys: &'a [CompactCellKey],
}

impl<'a> DirectoryRemoval<'a> {
    pub(crate) const fn new(page_index: u32, keys: &'a [CompactCellKey]) -> Self {
        Self { page_index, keys }
    }

    pub(crate) const fn page_index(self) -> u32 {
        self.page_index
    }

    pub(crate) const fn keys(self) -> &'a [CompactCellKey] {
        self.keys
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DirectoryAdmission<'a> {
    keys: &'a [CompactCellKey],
}

impl<'a> DirectoryAdmission<'a> {
    pub(crate) const fn new(keys: &'a [CompactCellKey]) -> Self {
        Self { keys }
    }

    pub(crate) const fn keys(self) -> &'a [CompactCellKey] {
        self.keys
    }
}

/// Read-only result of one exact residency mutation.
///
/// The caller publishes the page records and this directory publication, then
/// consumes the plan through [`GlobalResidencyDirectory::commit`]. Dropping a
/// plan is a complete rollback because preparation does not mutate authority.
#[derive(Debug)]
pub(crate) struct PreparedDirectoryBatch {
    base_revision: u64,
    next_revision: u64,
    removal_pages: Vec<u32>,
    admission_pages: Vec<u32>,
    admission_keys: Vec<Box<[CompactCellKey]>>,
    reused_removal_pages: usize,
    free_pages_taken: usize,
    final_live_pages: u32,
    final_live_entries: u32,
    final_tombstones: u32,
    maximum_observed_probes: u32,
    publication: DirectoryPublication,
}

impl PreparedDirectoryBatch {
    pub(crate) fn admission_page_indices(&self) -> &[u32] {
        &self.admission_pages
    }

    pub(crate) const fn publication(&self) -> &DirectoryPublication {
        &self.publication
    }
}

/// One fixed-capacity page-record allocator and persistent compact directory.
#[derive(Debug)]
pub(crate) struct GlobalResidencyDirectory {
    page_capacity: u32,
    slots: Vec<GpuDirectorySlot>,
    page_keys: Vec<Option<Box<[CompactCellKey]>>>,
    free_pages: Vec<u32>,
    live_pages: u32,
    live_entries: u32,
    tombstones: u32,
    maximum_observed_probes: u32,
    revision: u64,
}

impl GlobalResidencyDirectory {
    pub(crate) fn new(page_capacity: u32) -> Result<Self, GlobalResidencyError> {
        let slot_capacity = directory_capacity(page_capacity)?;
        let slot_capacity_usize = usize::try_from(slot_capacity)
            .map_err(|_| GlobalResidencyError::HostAllocationCapacity)?;
        let page_capacity_usize = usize::try_from(page_capacity)
            .map_err(|_| GlobalResidencyError::HostAllocationCapacity)?;

        let mut slots = Vec::new();
        slots
            .try_reserve_exact(slot_capacity_usize)
            .map_err(|_| GlobalResidencyError::HostAllocationCapacity)?;
        slots.resize(slot_capacity_usize, GpuDirectorySlot::empty());

        let mut page_keys = Vec::new();
        page_keys
            .try_reserve_exact(page_capacity_usize)
            .map_err(|_| GlobalResidencyError::HostAllocationCapacity)?;
        page_keys.resize_with(page_capacity_usize, || None);

        let mut free_pages = Vec::new();
        free_pages
            .try_reserve_exact(page_capacity_usize)
            .map_err(|_| GlobalResidencyError::HostAllocationCapacity)?;
        free_pages.extend((0..page_capacity).rev());

        Ok(Self {
            page_capacity,
            slots,
            page_keys,
            free_pages,
            live_pages: 0,
            live_entries: 0,
            tombstones: 0,
            maximum_observed_probes: 0,
            revision: 0,
        })
    }

    pub(crate) fn directory_slot_capacity(&self) -> u32 {
        u32::try_from(self.slots.len()).expect("directory capacity is validated as u32")
    }

    pub(crate) fn directory_buffer_bytes(&self) -> u64 {
        u64::from(self.directory_slot_capacity()) * DIRECTORY_SLOT_BYTES
    }

    pub(crate) const fn page_record_buffer_bytes(&self) -> u64 {
        self.page_capacity as u64 * PAGE_RECORD_BYTES
    }

    pub(crate) const fn live_pages(&self) -> u32 {
        self.live_pages
    }

    #[cfg(test)]
    pub(crate) const fn live_entries(&self) -> u32 {
        self.live_entries
    }

    #[cfg(test)]
    pub(crate) const fn tombstones(&self) -> u32 {
        self.tombstones
    }

    pub(crate) fn page_keys(&self, page_index: u32) -> Option<&[CompactCellKey]> {
        self.page_keys
            .get(page_index as usize)
            .and_then(Option::as_deref)
    }

    pub(crate) fn lookup_page(
        &self,
        key: CompactCellKey,
    ) -> Result<Option<u32>, GlobalResidencyError> {
        let found = find_occupied_slot(self.slots.len(), key, |slot| self.slots[slot])?;
        Ok(found.map(|(_, page, _)| page))
    }

    pub(crate) fn prepare_batch(
        &self,
        removals: &[DirectoryRemoval<'_>],
        admissions: &[DirectoryAdmission<'_>],
    ) -> Result<PreparedDirectoryBatch, GlobalResidencyError> {
        if removals.is_empty() && admissions.is_empty() {
            return Err(GlobalResidencyError::EmptyBatch);
        }
        let next_revision = self
            .revision
            .checked_add(1)
            .ok_or(GlobalResidencyError::RevisionOverflow)?;

        let mut removal_page_set = BTreeSet::new();
        let mut removal_pages = Vec::new();
        removal_pages
            .try_reserve_exact(removals.len())
            .map_err(|_| GlobalResidencyError::HostAllocationCapacity)?;
        let mut removed_cells = BTreeSet::new();
        let mut removal_cell_count = 0_usize;

        for removal in removals {
            validate_cell_set(removal.keys)?;
            if !removal_page_set.insert(removal.page_index) {
                return Err(GlobalResidencyError::DuplicatePage);
            }
            let committed =
                self.page_keys(removal.page_index)
                    .ok_or(GlobalResidencyError::MissingPage {
                        page_index: removal.page_index,
                    })?;
            if committed != removal.keys {
                return Err(GlobalResidencyError::PageOwnershipMismatch {
                    page_index: removal.page_index,
                });
            }
            for key in removal.keys {
                if !removed_cells.insert(*key) {
                    return Err(GlobalResidencyError::DuplicateCell);
                }
                let (_, page, _) =
                    find_occupied_slot(self.slots.len(), *key, |slot| self.slots[slot])?
                        .ok_or(GlobalResidencyError::DirectoryOwnershipMismatch)?;
                if page != removal.page_index {
                    return Err(GlobalResidencyError::DirectoryOwnershipMismatch);
                }
            }
            removal_cell_count = removal_cell_count
                .checked_add(removal.keys.len())
                .ok_or(GlobalResidencyError::CellPopulationOverflow)?;
            removal_pages.push(removal.page_index);
        }

        let mut admitted_cells = BTreeSet::new();
        let mut admission_keys = Vec::new();
        admission_keys
            .try_reserve_exact(admissions.len())
            .map_err(|_| GlobalResidencyError::HostAllocationCapacity)?;
        let mut admission_cell_count = 0_usize;
        for admission in admissions {
            validate_cell_set(admission.keys)?;
            for key in admission.keys {
                if !admitted_cells.insert(*key) || removed_cells.contains(key) {
                    return Err(GlobalResidencyError::DuplicateCell);
                }
                if self.lookup_page(*key)?.is_some() {
                    return Err(GlobalResidencyError::DuplicateCell);
                }
            }
            admission_cell_count = admission_cell_count
                .checked_add(admission.keys.len())
                .ok_or(GlobalResidencyError::CellPopulationOverflow)?;
            admission_keys.push(clone_cell_set(admission.keys)?);
        }

        let reusable_removals = removals.len().min(admissions.len());
        let free_pages_needed = admissions.len() - reusable_removals;
        if free_pages_needed > self.free_pages.len() {
            return Err(GlobalResidencyError::PageCapacity);
        }
        let mut admission_pages = Vec::new();
        admission_pages
            .try_reserve_exact(admissions.len())
            .map_err(|_| GlobalResidencyError::HostAllocationCapacity)?;
        admission_pages.extend(
            removals
                .iter()
                .take(reusable_removals)
                .map(|removal| removal.page_index),
        );
        admission_pages.extend(
            self.free_pages
                .iter()
                .rev()
                .take(free_pages_needed)
                .copied(),
        );

        let final_live_pages_usize = (self.live_pages as usize)
            .checked_sub(removals.len())
            .and_then(|live| live.checked_add(admissions.len()))
            .ok_or(GlobalResidencyError::PageCapacity)?;
        if final_live_pages_usize > self.page_capacity as usize {
            return Err(GlobalResidencyError::PageCapacity);
        }
        let final_live_pages = u32::try_from(final_live_pages_usize)
            .map_err(|_| GlobalResidencyError::PageCapacity)?;

        let final_live_entries_usize = (self.live_entries as usize)
            .checked_sub(removal_cell_count)
            .and_then(|live| live.checked_add(admission_cell_count))
            .ok_or(GlobalResidencyError::CellPopulationOverflow)?;
        if final_live_entries_usize > self.slots.len() / 2 {
            return Err(GlobalResidencyError::DirectoryCapacity);
        }
        let final_live_entries = u32::try_from(final_live_entries_usize)
            .map_err(|_| GlobalResidencyError::DirectoryCapacity)?;

        let projected_tombstones = u64::from(self.tombstones)
            .checked_add(
                u64::try_from(removal_cell_count)
                    .map_err(|_| GlobalResidencyError::CellPopulationOverflow)?,
            )
            .ok_or(GlobalResidencyError::CellPopulationOverflow)?;
        let projected_used = u64::from(self.live_entries)
            .checked_add(u64::from(self.tombstones))
            .and_then(|used| used.checked_add(u64::try_from(admission_cell_count).ok()?))
            .ok_or(GlobalResidencyError::CellPopulationOverflow)?;
        let force_rebuild = projected_tombstones > self.slots.len() as u64 / 8
            || projected_used
                .checked_mul(2)
                .is_none_or(|used| used > self.slots.len() as u64);

        let prepared_directory = if force_rebuild {
            self.prepare_rebuild(
                &removal_page_set,
                &admission_pages,
                &admission_keys,
                final_live_entries,
            )?
        } else {
            match self.prepare_incremental(
                removals,
                &admission_pages,
                &admission_keys,
                final_live_entries,
            ) {
                Ok(prepared) => prepared,
                Err(GlobalResidencyError::DirectoryProbeBound) => self.prepare_rebuild(
                    &removal_page_set,
                    &admission_pages,
                    &admission_keys,
                    final_live_entries,
                )?,
                Err(error) => return Err(error),
            }
        };

        Ok(PreparedDirectoryBatch {
            base_revision: self.revision,
            next_revision,
            removal_pages,
            admission_pages,
            admission_keys,
            reused_removal_pages: reusable_removals,
            free_pages_taken: free_pages_needed,
            final_live_pages,
            final_live_entries,
            final_tombstones: prepared_directory.final_tombstones,
            maximum_observed_probes: prepared_directory.maximum_observed_probes,
            publication: prepared_directory.publication,
        })
    }

    /// Commits exactly one already-published mutation after GPU submission.
    ///
    /// All fallible capacity and directory work occurs during read-only
    /// preparation. A stale plan is rejected before any CPU authority changes.
    pub(crate) fn commit(
        &mut self,
        prepared: PreparedDirectoryBatch,
    ) -> Result<(), GlobalResidencyError> {
        if prepared.base_revision != self.revision {
            return Err(GlobalResidencyError::StalePreparedBatch);
        }

        for page_index in &prepared.removal_pages {
            if self.page_keys(*page_index).is_none() {
                return Err(GlobalResidencyError::StalePreparedBatch);
            }
        }
        let planned_free_pages = &prepared.admission_pages[prepared.reused_removal_pages..];
        if planned_free_pages.len() != prepared.free_pages_taken
            || !self
                .free_pages
                .iter()
                .rev()
                .take(prepared.free_pages_taken)
                .copied()
                .eq(planned_free_pages.iter().copied())
        {
            return Err(GlobalResidencyError::StalePreparedBatch);
        }

        match prepared.publication {
            DirectoryPublication::Incremental {
                removal_writes,
                insertion_writes,
            } => {
                for write in removal_writes.into_iter().chain(insertion_writes) {
                    self.slots[write.slot_index as usize] = write.slot;
                }
            }
            DirectoryPublication::Rebuilt { slots } => {
                self.slots = slots;
            }
        }

        for page_index in &prepared.removal_pages {
            self.page_keys[*page_index as usize] = None;
        }
        self.free_pages
            .truncate(self.free_pages.len() - prepared.free_pages_taken);
        for (page_index, keys) in prepared
            .admission_pages
            .iter()
            .copied()
            .zip(prepared.admission_keys)
        {
            self.page_keys[page_index as usize] = Some(keys);
        }
        for page_index in prepared.removal_pages[prepared.reused_removal_pages..]
            .iter()
            .rev()
        {
            self.free_pages.push(*page_index);
        }

        self.live_pages = prepared.final_live_pages;
        self.live_entries = prepared.final_live_entries;
        self.tombstones = prepared.final_tombstones;
        self.maximum_observed_probes = prepared.maximum_observed_probes;
        self.revision = prepared.next_revision;
        debug_assert_eq!(
            self.free_pages.len() + self.live_pages as usize,
            self.page_capacity as usize
        );
        Ok(())
    }

    fn prepare_incremental(
        &self,
        removals: &[DirectoryRemoval<'_>],
        admission_pages: &[u32],
        admission_keys: &[Box<[CompactCellKey]>],
        final_live_entries: u32,
    ) -> Result<PreparedDirectory, GlobalResidencyError> {
        let removal_write_count = removals.iter().try_fold(0_usize, |count, removal| {
            count.checked_add(removal.keys.len())
        });
        let removal_write_count =
            removal_write_count.ok_or(GlobalResidencyError::CellPopulationOverflow)?;
        let insertion_write_count = admission_keys
            .iter()
            .try_fold(0_usize, |count, keys| count.checked_add(keys.len()));
        let insertion_write_count =
            insertion_write_count.ok_or(GlobalResidencyError::CellPopulationOverflow)?;

        let mut removal_writes = Vec::new();
        removal_writes
            .try_reserve_exact(removal_write_count)
            .map_err(|_| GlobalResidencyError::HostAllocationCapacity)?;
        let mut insertion_writes = Vec::new();
        insertion_writes
            .try_reserve_exact(insertion_write_count)
            .map_err(|_| GlobalResidencyError::HostAllocationCapacity)?;

        let mut overlay = DirectoryOverlay::new(&self.slots);
        let mut live_entries = self.live_entries;
        let mut tombstones = self.tombstones;
        let mut maximum = self.maximum_observed_probes;

        for removal in removals {
            for key in removal.keys {
                let (slot, page, probes) =
                    find_occupied_slot(overlay.len(), *key, |slot| overlay.get(slot))?
                        .ok_or(GlobalResidencyError::DirectoryOwnershipMismatch)?;
                if page != removal.page_index {
                    return Err(GlobalResidencyError::DirectoryOwnershipMismatch);
                }
                let tombstone = GpuDirectorySlot::tombstone();
                overlay.set(slot, tombstone);
                live_entries = live_entries
                    .checked_sub(1)
                    .ok_or(GlobalResidencyError::DirectoryOwnershipMismatch)?;
                tombstones = tombstones
                    .checked_add(1)
                    .ok_or(GlobalResidencyError::CellPopulationOverflow)?;
                maximum = maximum.max(probes);
                removal_writes.push(DirectorySlotWrite {
                    slot_index: u32::try_from(slot)
                        .map_err(|_| GlobalResidencyError::DirectoryCapacity)?,
                    slot: tombstone,
                });
            }
        }

        for (page_index, keys) in admission_pages.iter().copied().zip(admission_keys) {
            for key in keys.iter().copied() {
                let (slot, reused_tombstone, probes) =
                    find_insertion_slot(overlay.len(), key, |slot| overlay.get(slot))?;
                let occupied = GpuDirectorySlot::occupied(key, page_index)?;
                overlay.set(slot, occupied);
                live_entries = live_entries
                    .checked_add(1)
                    .ok_or(GlobalResidencyError::DirectoryCapacity)?;
                if reused_tombstone {
                    tombstones = tombstones
                        .checked_sub(1)
                        .ok_or(GlobalResidencyError::DirectoryOwnershipMismatch)?;
                }
                maximum = maximum.max(probes);
                if !insertion_preserves_probe_bound(overlay.len(), slot, |slot| overlay.get(slot)) {
                    return Err(GlobalResidencyError::DirectoryProbeBound);
                }
                insertion_writes.push(DirectorySlotWrite {
                    slot_index: u32::try_from(slot)
                        .map_err(|_| GlobalResidencyError::DirectoryCapacity)?,
                    slot: occupied,
                });
            }
        }
        if live_entries != final_live_entries {
            return Err(GlobalResidencyError::DirectoryOwnershipMismatch);
        }

        Ok(PreparedDirectory {
            final_tombstones: tombstones,
            maximum_observed_probes: maximum,
            publication: DirectoryPublication::Incremental {
                removal_writes,
                insertion_writes,
            },
        })
    }

    fn prepare_rebuild(
        &self,
        removal_pages: &BTreeSet<u32>,
        admission_pages: &[u32],
        admission_keys: &[Box<[CompactCellKey]>],
        final_live_entries: u32,
    ) -> Result<PreparedDirectory, GlobalResidencyError> {
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(self.slots.len())
            .map_err(|_| GlobalResidencyError::HostAllocationCapacity)?;
        slots.resize(self.slots.len(), GpuDirectorySlot::empty());
        let mut inserted = 0_u32;
        let mut maximum = self.maximum_observed_probes;

        for (page_index, keys) in self.page_keys.iter().enumerate() {
            let page_index =
                u32::try_from(page_index).map_err(|_| GlobalResidencyError::PageCapacity)?;
            if removal_pages.contains(&page_index) {
                continue;
            }
            if let Some(keys) = keys {
                for key in keys.iter().copied() {
                    maximum = maximum.max(insert_rebuilt_slot(&mut slots, key, page_index)?);
                    inserted = inserted
                        .checked_add(1)
                        .ok_or(GlobalResidencyError::DirectoryCapacity)?;
                }
            }
        }
        for (page_index, keys) in admission_pages.iter().copied().zip(admission_keys) {
            for key in keys.iter().copied() {
                maximum = maximum.max(insert_rebuilt_slot(&mut slots, key, page_index)?);
                inserted = inserted
                    .checked_add(1)
                    .ok_or(GlobalResidencyError::DirectoryCapacity)?;
            }
        }
        if inserted != final_live_entries {
            return Err(GlobalResidencyError::DirectoryOwnershipMismatch);
        }
        validate_probe_bound(&slots)?;

        Ok(PreparedDirectory {
            final_tombstones: 0,
            maximum_observed_probes: maximum,
            publication: DirectoryPublication::Rebuilt { slots },
        })
    }
}

#[derive(Debug)]
struct PreparedDirectory {
    final_tombstones: u32,
    maximum_observed_probes: u32,
    publication: DirectoryPublication,
}

struct DirectoryOverlay<'a> {
    base: &'a [GpuDirectorySlot],
    changed: BTreeMap<usize, GpuDirectorySlot>,
}

impl<'a> DirectoryOverlay<'a> {
    fn new(base: &'a [GpuDirectorySlot]) -> Self {
        Self {
            base,
            changed: BTreeMap::new(),
        }
    }

    fn len(&self) -> usize {
        self.base.len()
    }

    fn get(&self, slot: usize) -> GpuDirectorySlot {
        self.changed.get(&slot).copied().unwrap_or(self.base[slot])
    }

    fn set(&mut self, slot: usize, value: GpuDirectorySlot) {
        self.changed.insert(slot, value);
    }
}

fn clone_cell_set(keys: &[CompactCellKey]) -> Result<Box<[CompactCellKey]>, GlobalResidencyError> {
    let mut cloned = Vec::new();
    cloned
        .try_reserve_exact(keys.len())
        .map_err(|_| GlobalResidencyError::HostAllocationCapacity)?;
    cloned.extend_from_slice(keys);
    Ok(cloned.into_boxed_slice())
}

fn validate_cell_set(keys: &[CompactCellKey]) -> Result<(), GlobalResidencyError> {
    if keys.is_empty() {
        return Err(GlobalResidencyError::EmptyCellSet);
    }
    if !cell_set_is_canonical(keys) {
        return Err(GlobalResidencyError::NonCanonicalCellSet);
    }
    Ok(())
}

fn cell_set_is_canonical(keys: &[CompactCellKey]) -> bool {
    keys.windows(2).all(|pair| pair[0] < pair[1])
}

fn directory_capacity(page_capacity: u32) -> Result<u32, GlobalResidencyError> {
    if page_capacity == 0 {
        return Err(GlobalResidencyError::InvalidPageCapacity);
    }
    page_capacity
        .checked_mul(2)
        .and_then(u32::checked_next_power_of_two)
        .filter(|capacity| *capacity <= MAX_DIRECTORY_CAPACITY)
        .ok_or(GlobalResidencyError::InvalidPageCapacity)
}

fn directory_slot(hash: u32, probe: u32, capacity: usize) -> usize {
    (hash.wrapping_add(probe) as usize) & (capacity - 1)
}

fn find_occupied_slot(
    capacity: usize,
    key: CompactCellKey,
    mut slot_at: impl FnMut(usize) -> GpuDirectorySlot,
) -> Result<Option<(usize, u32, u32)>, GlobalResidencyError> {
    let hash = directory_hash(key);
    for probe in 0..MAX_DIRECTORY_PROBES {
        let slot_index = directory_slot(hash, probe, capacity);
        let slot = slot_at(slot_index);
        if slot.is_empty() {
            return Ok(None);
        }
        if slot.key() == Some(key) {
            return Ok(Some((
                slot_index,
                slot.page_record_index()
                    .expect("an occupied slot has a page"),
                probe + 1,
            )));
        }
    }
    Err(GlobalResidencyError::DirectoryProbeBound)
}

fn find_insertion_slot(
    capacity: usize,
    key: CompactCellKey,
    mut slot_at: impl FnMut(usize) -> GpuDirectorySlot,
) -> Result<(usize, bool, u32), GlobalResidencyError> {
    let hash = directory_hash(key);
    let mut first_tombstone = None;
    for probe in 0..MAX_DIRECTORY_PROBES {
        let slot_index = directory_slot(hash, probe, capacity);
        let slot = slot_at(slot_index);
        if slot.key() == Some(key) {
            return Err(GlobalResidencyError::DuplicateCell);
        }
        if slot.is_tombstone() {
            first_tombstone.get_or_insert(slot_index);
            continue;
        }
        if slot.is_empty() {
            let insertion = first_tombstone.unwrap_or(slot_index);
            return Ok((insertion, first_tombstone.is_some(), probe + 1));
        }
    }
    Err(GlobalResidencyError::DirectoryProbeBound)
}

fn insertion_preserves_probe_bound(
    capacity: usize,
    inserted: usize,
    mut slot_at: impl FnMut(usize) -> GpuDirectorySlot,
) -> bool {
    let mask = capacity - 1;
    let mut run = 1_usize;
    for distance in 1..MAX_DIRECTORY_PROBES as usize {
        let slot = inserted.wrapping_sub(distance) & mask;
        if slot_at(slot).is_empty() {
            break;
        }
        run += 1;
        if run >= MAX_DIRECTORY_PROBES as usize {
            return false;
        }
    }
    for distance in 1..MAX_DIRECTORY_PROBES as usize {
        let slot = inserted.wrapping_add(distance) & mask;
        if slot_at(slot).is_empty() {
            break;
        }
        run += 1;
        if run >= MAX_DIRECTORY_PROBES as usize {
            return false;
        }
    }
    true
}

fn insert_rebuilt_slot(
    slots: &mut [GpuDirectorySlot],
    key: CompactCellKey,
    page_index: u32,
) -> Result<u32, GlobalResidencyError> {
    let (slot, reused_tombstone, probes) =
        find_insertion_slot(slots.len(), key, |slot| slots[slot])?;
    debug_assert!(!reused_tombstone);
    slots[slot] = GpuDirectorySlot::occupied(key, page_index)?;
    if !insertion_preserves_probe_bound(slots.len(), slot, |slot| slots[slot]) {
        return Err(GlobalResidencyError::DirectoryProbeBound);
    }
    Ok(probes)
}

fn validate_probe_bound(slots: &[GpuDirectorySlot]) -> Result<(), GlobalResidencyError> {
    for start in 0..slots.len() {
        let terminated = (0..MAX_DIRECTORY_PROBES as usize)
            .any(|probe| slots[start.wrapping_add(probe) & (slots.len() - 1)].is_empty());
        if !terminated {
            return Err(GlobalResidencyError::DirectoryProbeBound);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use mirante4d_dataset::{DatasetResourceIdentity, DatasetSourceId, ResourceRegion};
    use mirante4d_domain::{LogicalLayerKey, ScaleLevel, Shape3D, TimeIndex};

    use super::*;

    fn brick(
        origin: [u64; 3],
        shape: [u64; 3],
        layer: u32,
        timepoint: u64,
        scale: u32,
    ) -> BrickKey {
        BrickKey::new(
            DatasetResourceIdentity::Unverified(DatasetSourceId::new(7)),
            LogicalLayerKey::new(layer),
            TimeIndex::new(timepoint),
            ScaleLevel::new(scale),
            ResourceRegion::new(origin, Shape3D::new(shape[0], shape[1], shape[2]).unwrap())
                .unwrap(),
        )
    }

    fn key(value: u32) -> CompactCellKey {
        CompactCellKey::new(3, 5, 2, value, 0, 0)
    }

    fn colliding_keys(count: usize, mask: u32) -> Vec<CompactCellKey> {
        let mut buckets = BTreeMap::<u32, Vec<CompactCellKey>>::new();
        for value in 0..10_000 {
            let candidate = key(value);
            let bucket = directory_hash(candidate) & mask;
            let values = buckets.entry(bucket).or_default();
            values.push(candidate);
            if values.len() == count {
                return values.clone();
            }
        }
        panic!("the bounded search must find the requested collision set");
    }

    #[test]
    fn compact_projection_covers_every_spanned_grid_cell_exactly() {
        let key = brick([64, 128, 192], [128, 64, 128], 9, 0x0000_0001_0000_0002, 3);
        let grid = RenderResourceGrid::new(
            LogicalLayerKey::new(9),
            ScaleLevel::new(3),
            Shape3D::new(192, 192, 320).unwrap(),
            Shape3D::new(64, 64, 64).unwrap(),
        );

        let compact = compact_cell_keys(key, grid).unwrap();
        assert_eq!(
            compact.iter().map(|key| key.words()).collect::<Vec<_>>(),
            vec![
                [9, 2, 1, 3, 3, 2, 1],
                [9, 2, 1, 3, 3, 2, 2],
                [9, 2, 1, 3, 4, 2, 1],
                [9, 2, 1, 3, 4, 2, 2],
            ]
        );

        let misaligned = brick([0, 0, 1], [64, 64, 63], 9, 0, 3);
        assert_eq!(
            compact_cell_keys(misaligned, grid),
            Err(GlobalResidencyError::RegionMisaligned { axis: 2 })
        );

        let overflow = brick([0, 0, u32::MAX as u64 + 1], [1, 1, 1], 9, 0, 3);
        let unit_grid = RenderResourceGrid::new(
            LogicalLayerKey::new(9),
            ScaleLevel::new(3),
            Shape3D::new(1, 1, u32::MAX as u64 + 2).unwrap(),
            Shape3D::new(1, 1, 1).unwrap(),
        );
        assert_eq!(
            compact_cell_keys(overflow, unit_grid),
            Err(GlobalResidencyError::CellCoordinateOverflow { axis: 2 })
        );
    }

    #[test]
    fn collision_batch_is_read_only_until_commit_and_reuses_a_tombstoned_page() {
        let mut directory = GlobalResidencyDirectory::new(8).unwrap();
        let colliding = colliding_keys(4, directory.directory_slot_capacity() - 1);
        let initial_sets = colliding[..3]
            .iter()
            .map(|key| vec![*key])
            .collect::<Vec<_>>();
        let initial = initial_sets
            .iter()
            .map(|keys| DirectoryAdmission::new(keys))
            .collect::<Vec<_>>();

        let prepared = directory.prepare_batch(&[], &initial).unwrap();
        assert_eq!(prepared.admission_page_indices(), &[0, 1, 2]);
        assert!(
            colliding[..3]
                .iter()
                .all(|key| directory.lookup_page(*key).unwrap().is_none())
        );
        directory.commit(prepared).unwrap();
        assert_eq!(directory.lookup_page(colliding[0]).unwrap(), Some(0));
        assert_eq!(directory.lookup_page(colliding[1]).unwrap(), Some(1));
        assert_eq!(directory.lookup_page(colliding[2]).unwrap(), Some(2));

        let removal_keys = [colliding[0]];
        let admission_keys = [colliding[3]];
        let removals = [DirectoryRemoval::new(0, &removal_keys)];
        let admissions = [DirectoryAdmission::new(&admission_keys)];
        let rolled_back = directory.prepare_batch(&removals, &admissions).unwrap();
        assert_eq!(rolled_back.admission_page_indices(), &[0]);
        assert_eq!(
            rolled_back.publication().removal_writes()[0].slot().words()[7],
            TOMBSTONE_PAGE_WORD
        );
        assert_ne!(
            rolled_back.publication().insertion_writes()[0]
                .slot()
                .words()[7],
            TOMBSTONE_PAGE_WORD
        );
        assert_eq!(directory.lookup_page(colliding[0]).unwrap(), Some(0));
        assert_eq!(directory.lookup_page(colliding[3]).unwrap(), None);
        drop(rolled_back);
        assert_eq!(directory.lookup_page(colliding[0]).unwrap(), Some(0));
        assert_eq!(directory.lookup_page(colliding[3]).unwrap(), None);

        let replacement = directory.prepare_batch(&removals, &admissions).unwrap();
        directory.commit(replacement).unwrap();
        assert_eq!(directory.lookup_page(colliding[0]).unwrap(), None);
        assert_eq!(directory.lookup_page(colliding[3]).unwrap(), Some(0));
        assert_eq!(directory.lookup_page(colliding[1]).unwrap(), Some(1));
        assert_eq!(directory.lookup_page(colliding[2]).unwrap(), Some(2));
    }

    #[test]
    fn tombstone_threshold_prepares_one_complete_compacted_image() {
        let mut directory = GlobalResidencyDirectory::new(8).unwrap();
        let sets = (0..4).map(|value| vec![key(value)]).collect::<Vec<_>>();
        let admissions = sets
            .iter()
            .map(|keys| DirectoryAdmission::new(keys))
            .collect::<Vec<_>>();
        let initial = directory.prepare_batch(&[], &admissions).unwrap();
        directory.commit(initial).unwrap();

        let removals = (0..3)
            .map(|page| DirectoryRemoval::new(page, &sets[page as usize]))
            .collect::<Vec<_>>();
        let compacted = directory.prepare_batch(&removals, &[]).unwrap();
        assert!(compacted.publication().is_rebuilt());
        assert_eq!(
            compacted.publication().rebuilt_slots().unwrap().len(),
            directory.directory_slot_capacity() as usize
        );
        assert_eq!(directory.lookup_page(key(0)).unwrap(), Some(0));

        directory.commit(compacted).unwrap();
        assert_eq!(directory.lookup_page(key(0)).unwrap(), None);
        assert_eq!(directory.lookup_page(key(1)).unwrap(), None);
        assert_eq!(directory.lookup_page(key(2)).unwrap(), None);
        assert_eq!(directory.lookup_page(key(3)).unwrap(), Some(3));
        assert_eq!(directory.tombstones(), 0);
        assert_eq!(directory.live_pages(), 1);
        assert_eq!(directory.live_entries(), 1);
    }
}
