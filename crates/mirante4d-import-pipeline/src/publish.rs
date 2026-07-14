//! Bounded checkpoint-to-package shard streaming.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use mirante4d_dataset::{CpuByteLease, CpuByteLedger, CpuLedgerCategory};
use mirante4d_storage::{
    GLOBAL_UNCOMPRESSED_OUTER_SHARD_BYTES_MAX, LocalPackageWriter, PACKED_INDEX_RECORD_BYTES,
    PackagePath, PackageShardInput, PackageWriteError, PackageWriteInput, ShardProfileKind,
};

use crate::{
    ImportCancellation, ImportError,
    chunk::chunk_grid,
    package::PackageMetadata,
    plan::ImportPlan,
    spool::{ImportSpool, SpoolWorkUnitKey},
};

/// Bounded writer, descriptor, manifest, and staged-catalog control state.
pub(crate) const PUBLICATION_CONTROL_BYTES_MAX: u64 = 64 * 1024 * 1024;
pub(crate) const PUBLICATION_VALIDATION_BYTES_MAX: u64 =
    PUBLICATION_CONTROL_BYTES_MAX + GLOBAL_UNCOMPRESSED_OUTER_SHARD_BYTES_MAX;

#[derive(Clone)]
struct LevelPaths {
    pixel: PackagePath,
    validity: Option<PackagePath>,
    packed: PackagePath,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Component {
    Pixel,
    Validity,
    Packed,
    Done,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn publish_package(
    destination: &std::path::Path,
    metadata: PackageMetadata,
    spool: &mut ImportSpool,
    plan: &ImportPlan,
    working_memory_bytes: u64,
    resident_working_bytes: u64,
    ledger: &dyn CpuByteLedger,
    cancellation: &ImportCancellation,
    peak_working_bytes: &mut u64,
) -> Result<mirante4d_storage::PackageWriteReceipt, ImportError> {
    let paths = metadata.profile.images()[0]
        .levels()
        .iter()
        .map(|level| LevelPaths {
            pixel: level.pixel_path().clone(),
            validity: level.validity_path().cloned(),
            packed: level.packed_index_path().clone(),
        })
        .collect();
    let deferred_error = Rc::new(RefCell::new(None));
    let observed_peak = Rc::new(Cell::new(*peak_working_bytes));
    let initial_combined = resident_working_bytes
        .checked_add(PUBLICATION_CONTROL_BYTES_MAX)
        .ok_or(ImportError::Overflow)?;
    if initial_combined > working_memory_bytes {
        return Err(ImportError::WorkingMemoryExceeded {
            required_bytes: initial_combined,
            budget_bytes: working_memory_bytes,
        });
    }
    let initial_lease = ledger.try_acquire(
        CpuLedgerCategory::ImportWorkingSet,
        PUBLICATION_CONTROL_BYTES_MAX,
    )?;
    observed_peak.set(observed_peak.get().max(initial_combined));
    let channels =
        u32::try_from(metadata.science.layers().len()).map_err(|_| ImportError::Overflow)?;
    let stream = ShardStream {
        spool,
        plan,
        paths,
        channels,
        working_memory_bytes,
        resident_working_bytes,
        ledger,
        cancellation,
        component: Component::Pixel,
        scale: 0,
        outer_ordinal: 0,
        lease: Some(initial_lease),
        deferred_error: Rc::clone(&deferred_error),
        observed_peak: Rc::clone(&observed_peak),
    };
    let input = PackageWriteInput::new(
        metadata.profile_kind,
        metadata.profile,
        metadata.science,
        metadata.display_defaults,
        metadata.portable_records,
        metadata.ome_images,
        metadata.arrays,
        stream,
    );

    let result = LocalPackageWriter::write_new_scientifically_validated(destination, input, || {
        cancellation.is_cancelled() || deferred_error.borrow().is_some()
    });
    *peak_working_bytes = (*peak_working_bytes).max(observed_peak.get());
    if let Some(error) = deferred_error.borrow_mut().take() {
        return Err(error);
    }
    match result {
        Ok(receipt) => Ok(receipt),
        Err(PackageWriteError::Cancelled) => Err(ImportError::Cancelled),
        Err(error) => Err(error.into()),
    }
}

struct ShardStream<'a> {
    spool: &'a mut ImportSpool,
    plan: &'a ImportPlan,
    paths: Vec<LevelPaths>,
    channels: u32,
    working_memory_bytes: u64,
    resident_working_bytes: u64,
    ledger: &'a dyn CpuByteLedger,
    cancellation: &'a ImportCancellation,
    component: Component,
    scale: usize,
    outer_ordinal: u64,
    lease: Option<Box<dyn CpuByteLease>>,
    deferred_error: Rc<RefCell<Option<ImportError>>>,
    observed_peak: Rc<Cell<u64>>,
}

impl Iterator for ShardStream<'_> {
    type Item = PackageShardInput;

    fn next(&mut self) -> Option<Self::Item> {
        self.lease = None;
        if self.deferred_error.borrow().is_some() || self.component == Component::Done {
            return None;
        }
        if self.cancellation.is_cancelled() {
            self.fail(ImportError::Cancelled);
            return None;
        }
        match self.next_checked() {
            Ok(value) => value,
            Err(error) => {
                self.fail(error);
                None
            }
        }
    }
}

impl ShardStream<'_> {
    fn next_checked(&mut self) -> Result<Option<PackageShardInput>, ImportError> {
        loop {
            if self.scale >= self.plan.shapes.len() {
                self.component = match self.component {
                    Component::Pixel if self.plan.explicit_validity => Component::Validity,
                    Component::Pixel | Component::Validity => Component::Packed,
                    Component::Packed | Component::Done => Component::Done,
                };
                self.scale = 0;
                self.outer_ordinal = 0;
                if self.component == Component::Done {
                    self.reserve_for_validation()?;
                    return Ok(None);
                }
            }

            let total = self.outer_count()?;
            if self.outer_ordinal >= total {
                self.scale += 1;
                self.outer_ordinal = 0;
                continue;
            }
            self.reserve_for_current()?;
            let ordinal = self.outer_ordinal;
            self.outer_ordinal += 1;
            return self.build_current(ordinal).map(Some);
        }
    }

    fn outer_count(&self) -> Result<u64, ImportError> {
        let shape = self.plan.shapes[self.scale];
        let grid = chunk_grid([shape.z(), shape.y(), shape.x()], self.plan.is_2d);
        match self.component {
            Component::Pixel | Component::Validity => {
                let ratios = if self.plan.is_2d {
                    [1, 4, 4]
                } else {
                    [4, 4, 4]
                };
                checked_product([
                    shape.t(),
                    u64::from(self.channels),
                    grid[0].div_ceil(ratios[0]),
                    grid[1].div_ceil(ratios[1]),
                    grid[2].div_ceil(ratios[2]),
                ])
            }
            Component::Packed => Ok(self.plan.logical_bricks_by_scale[self.scale].div_ceil(16_384)),
            Component::Done => Ok(0),
        }
    }

    fn reserve_for_current(&mut self) -> Result<(), ImportError> {
        let kind = match self.component {
            Component::Pixel => self.plan.pixel_kind,
            Component::Validity => self.plan.validity_kind,
            Component::Packed => ShardProfileKind::PackedIndex,
            Component::Done => return Ok(()),
        };
        let bytes = publication_shard_bytes(kind)?;
        let combined = self
            .resident_working_bytes
            .checked_add(bytes)
            .ok_or(ImportError::Overflow)?;
        if combined > self.working_memory_bytes {
            return Err(ImportError::WorkingMemoryExceeded {
                required_bytes: combined,
                budget_bytes: self.working_memory_bytes,
            });
        }
        let lease = self
            .ledger
            .try_acquire(CpuLedgerCategory::ImportWorkingSet, bytes)?;
        self.observed_peak
            .set(self.observed_peak.get().max(combined));
        self.lease = Some(lease);
        Ok(())
    }

    fn reserve_for_validation(&mut self) -> Result<(), ImportError> {
        let bytes = PUBLICATION_VALIDATION_BYTES_MAX;
        let combined = self
            .resident_working_bytes
            .checked_add(bytes)
            .ok_or(ImportError::Overflow)?;
        if combined > self.working_memory_bytes {
            return Err(ImportError::WorkingMemoryExceeded {
                required_bytes: combined,
                budget_bytes: self.working_memory_bytes,
            });
        }
        self.lease = Some(
            self.ledger
                .try_acquire(CpuLedgerCategory::ImportWorkingSet, bytes)?,
        );
        self.observed_peak
            .set(self.observed_peak.get().max(combined));
        Ok(())
    }

    fn build_current(&mut self, ordinal: u64) -> Result<PackageShardInput, ImportError> {
        match self.component {
            Component::Pixel => self.build_pixel_or_validity(ordinal, false),
            Component::Validity => self.build_pixel_or_validity(ordinal, true),
            Component::Packed => self.build_packed(ordinal),
            Component::Done => Err(ImportError::InvalidRequest(
                "package shard stream advanced after completion",
            )),
        }
    }

    fn build_pixel_or_validity(
        &mut self,
        ordinal: u64,
        validity: bool,
    ) -> Result<PackageShardInput, ImportError> {
        let shape = self.plan.shapes[self.scale];
        let grid = chunk_grid([shape.z(), shape.y(), shape.x()], self.plan.is_2d);
        let ratios = if self.plan.is_2d {
            [1, 4, 4]
        } else {
            [4, 4, 4]
        };
        let outer_grid = [
            grid[0].div_ceil(ratios[0]),
            grid[1].div_ceil(ratios[1]),
            grid[2].div_ceil(ratios[2]),
        ];
        let [t, c, oz, oy, ox] = decode_5d(
            ordinal,
            [
                shape.t(),
                u64::from(self.channels),
                outer_grid[0],
                outer_grid[1],
                outer_grid[2],
            ],
        )?;
        let kind = if validity {
            self.plan.validity_kind
        } else {
            self.plan.pixel_kind
        };
        let mut chunks = std::iter::repeat_with(|| None)
            .take(kind.chunks_per_shard())
            .collect::<Vec<_>>();
        for local_z in 0..ratios[0] {
            for local_y in 0..ratios[1] {
                for local_x in 0..ratios[2] {
                    let chunk = [
                        oz * ratios[0] + local_z,
                        oy * ratios[1] + local_y,
                        ox * ratios[2] + local_x,
                    ];
                    if (0..3).any(|axis| chunk[axis] >= grid[axis]) {
                        continue;
                    }
                    let slot =
                        usize::try_from((local_z * ratios[1] + local_y) * ratios[2] + local_x)
                            .map_err(|_| ImportError::Overflow)?;
                    let key = key(self.scale, t, c, chunk)?;
                    if self.cancellation.is_cancelled() {
                        return Err(ImportError::Cancelled);
                    }
                    if !self.spool.contains(key) {
                        return Err(ImportError::InvalidCheckpoint(
                            "checkpoint is missing a completed work unit".to_owned(),
                        ));
                    }
                    let component = self.spool.read_component(key, validity)?;
                    if let Some(component) = component {
                        if component.kind != kind {
                            return Err(ImportError::InvalidCheckpoint(
                                "checkpoint chunk uses the wrong storage kind".to_owned(),
                            ));
                        }
                        chunks[slot] = Some(component.decoded);
                    }
                }
            }
        }
        let path = if validity {
            self.paths[self.scale].validity.clone().ok_or_else(|| {
                ImportError::InvalidCheckpoint(
                    "checkpoint contains validity for an all-valid level".to_owned(),
                )
            })?
        } else {
            self.paths[self.scale].pixel.clone()
        };
        Ok(PackageShardInput::new(path, vec![t, c, oz, oy, ox], chunks))
    }

    fn build_packed(&mut self, outer: u64) -> Result<PackageShardInput, ImportError> {
        let record_count = self.plan.logical_bricks_by_scale[self.scale];
        let mut chunks = std::iter::repeat_with(|| None)
            .take(ShardProfileKind::PackedIndex.chunks_per_shard())
            .collect::<Vec<_>>();
        for (slot, chunk) in chunks.iter_mut().enumerate() {
            let start = outer
                .checked_mul(16_384)
                .and_then(|value| value.checked_add((slot as u64) * 256))
                .ok_or(ImportError::Overflow)?;
            if start >= record_count {
                break;
            }
            let end = start.saturating_add(256).min(record_count);
            let mut decoded = vec![0; ShardProfileKind::PackedIndex.decoded_inner_bytes()];
            for record_ordinal in start..end {
                if self.cancellation.is_cancelled() {
                    return Err(ImportError::Cancelled);
                }
                let key = self.key_from_record_ordinal(record_ordinal)?;
                let packed_index = self.spool.read_packed_index(key).ok_or_else(|| {
                    ImportError::InvalidCheckpoint(
                        "checkpoint is missing a packed-index work unit".to_owned(),
                    )
                })?;
                let within =
                    usize::try_from(record_ordinal - start).map_err(|_| ImportError::Overflow)?;
                let offset = within
                    .checked_mul(PACKED_INDEX_RECORD_BYTES as usize)
                    .ok_or(ImportError::Overflow)?;
                decoded[offset..offset + PACKED_INDEX_RECORD_BYTES as usize]
                    .copy_from_slice(&packed_index);
            }
            *chunk = Some(decoded);
        }
        Ok(PackageShardInput::new(
            self.paths[self.scale].packed.clone(),
            vec![outer, 0],
            chunks,
        ))
    }

    fn key_from_record_ordinal(&self, ordinal: u64) -> Result<SpoolWorkUnitKey, ImportError> {
        let shape = self.plan.shapes[self.scale];
        let grid = chunk_grid([shape.z(), shape.y(), shape.x()], self.plan.is_2d);
        let [t, c, z, y, x] = decode_5d(
            ordinal,
            [
                shape.t(),
                u64::from(self.channels),
                grid[0],
                grid[1],
                grid[2],
            ],
        )?;
        key(self.scale, t, c, [z, y, x])
    }

    fn fail(&self, error: ImportError) {
        let mut slot = self.deferred_error.borrow_mut();
        if slot.is_none() {
            *slot = Some(error);
        }
    }
}

pub(crate) fn publication_shard_bytes(kind: ShardProfileKind) -> Result<u64, ImportError> {
    u64::try_from(kind.decoded_outer_bytes())
        .ok()
        .and_then(|value| value.checked_add(u64::try_from(kind.encoded_inner_bytes_max()).ok()?))
        .and_then(|value| value.checked_add(PUBLICATION_CONTROL_BYTES_MAX))
        .ok_or(ImportError::Overflow)
}

fn key(scale: usize, t: u64, c: u64, chunk: [u64; 3]) -> Result<SpoolWorkUnitKey, ImportError> {
    Ok(SpoolWorkUnitKey::new(
        0,
        u32::try_from(scale).map_err(|_| ImportError::Overflow)?,
        u32::try_from(t).map_err(|_| ImportError::Overflow)?,
        u32::try_from(c).map_err(|_| ImportError::Overflow)?,
        u32::try_from(chunk[0]).map_err(|_| ImportError::Overflow)?,
        u32::try_from(chunk[1]).map_err(|_| ImportError::Overflow)?,
        u32::try_from(chunk[2]).map_err(|_| ImportError::Overflow)?,
    ))
}

fn decode_5d(mut ordinal: u64, dimensions: [u64; 5]) -> Result<[u64; 5], ImportError> {
    let total = checked_product(dimensions)?;
    if ordinal >= total {
        return Err(ImportError::Overflow);
    }
    let mut coordinates = [0; 5];
    for axis in (0..5).rev() {
        coordinates[axis] = ordinal % dimensions[axis];
        ordinal /= dimensions[axis];
    }
    Ok(coordinates)
}

fn checked_product<const N: usize>(values: [u64; N]) -> Result<u64, ImportError> {
    values
        .into_iter()
        .try_fold(1_u64, |product, value| product.checked_mul(value))
        .ok_or(ImportError::Overflow)
}
