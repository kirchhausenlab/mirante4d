use std::collections::{BTreeMap, BTreeSet};

use mirante4d_identity::{ExactBytesHasher, IdentityHashError};
use thiserror::Error;

use crate::brick_address::{brick_grid, edge_extent, pixel_brick, spatial_inner_chunk};
use crate::range_io::LocalObjectSnapshot;
use crate::{
    DatasetProfileAdmission, LocalPackageReader, PACKED_INDEX_RECORD_BYTES,
    PACKED_INDEX_RECORDS_PER_INNER_CHUNK, PACKED_INDEX_RECORDS_PER_OUTER_SHARD,
    PackageObjectDescriptor, PackageObjectKind, PackagePath, PackedIndexCoordinates,
    PackedIndexError, PackedIndexRecord, ProfileHeader, ProfileValidityMode, RangeReadError,
    ShardCodecError, ShardProfileKind, StorageProfileError, ZarrArrayMetadata,
    decode_inner_payload, decode_shard_index_tail,
};

const SPATIAL_INNER_CHUNKS_PER_SHARD_AXIS: u64 = 4;

/// Inputs already authenticated and admitted by the local package catalog.
///
/// The reconciliation result remains structural. It does not authorize pixel
/// bytes as belonging to the claimed package identity.
pub(crate) struct PackageStructureInput<'a> {
    pub(crate) reader: &'a LocalPackageReader,
    pub(crate) profile: &'a ProfileHeader,
    pub(crate) arrays: &'a BTreeMap<PackagePath, ZarrArrayMetadata>,
    pub(crate) descriptors: &'a [PackageObjectDescriptor],
    pub(crate) admission: DatasetProfileAdmission,
}

/// Bounded facts from one complete packed-record/shard reconciliation pass.
#[derive(Debug, Default)]
pub(crate) struct PackageStructureReport {
    pub(crate) records_visited: u64,
    pub(crate) packed_index_shards: u64,
    pub(crate) addressed_pixel_shards: u64,
    pub(crate) addressed_validity_shards: u64,
    pub(crate) listed_pixel_shards: u64,
    pub(crate) listed_validity_shards: u64,
    pub(crate) work_operations: u64,
    snapshots: Vec<LocalObjectSnapshot>,
}

impl PackageStructureReport {
    pub(crate) fn revalidate_snapshots(
        &self,
        reader: &LocalPackageReader,
        is_cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), PackageStructureError> {
        for snapshot in &self.snapshots {
            if is_cancelled() {
                return Err(PackageStructureError::Cancelled);
            }
            reader.revalidate_snapshot(snapshot)?;
        }
        if is_cancelled() {
            Err(PackageStructureError::Cancelled)
        } else {
            Ok(())
        }
    }

    pub(crate) fn snapshots(&self) -> &[LocalObjectSnapshot] {
        &self.snapshots
    }
}

#[derive(Debug, Error)]
pub enum PackageStructureError {
    #[error(transparent)]
    Admission(#[from] crate::PackageAdmissionError),
    #[error(transparent)]
    Path(#[from] StorageProfileError),
    #[error(transparent)]
    Range(#[from] RangeReadError),
    #[error(transparent)]
    Shard(#[from] ShardCodecError),
    #[error(transparent)]
    PackedIndex(#[from] PackedIndexError),
    #[error(transparent)]
    Identity(#[from] IdentityHashError),
    #[error("packed-record structural reconciliation was cancelled")]
    Cancelled,
    #[error("required {component} array metadata {path} is missing")]
    MissingArrayMetadata {
        component: &'static str,
        path: String,
    },
    #[error("{component} array {path} has unexpected storage kind {kind:?}")]
    UnexpectedArrayKind {
        component: &'static str,
        path: String,
        kind: ShardProfileKind,
    },
    #[error("packed-index array {path} shape differs from [{records}, 64]")]
    PackedIndexShapeMismatch { path: String, records: u64 },
    #[error("required {component} shard descriptor {path} is missing")]
    MissingShardDescriptor {
        component: &'static str,
        path: String,
    },
    #[error("manifest object {path} has kind {actual:?}; expected {expected:?}")]
    DescriptorKindMismatch {
        path: String,
        expected: PackageObjectKind,
        actual: PackageObjectKind,
    },
    #[error("manifest contains an unexpected {component} shard descriptor {path}")]
    UnexpectedShardDescriptor {
        component: &'static str,
        path: String,
    },
    #[error("object {path} has {actual} bytes; manifest declares {expected}")]
    ObjectLengthMismatch {
        path: String,
        expected: u64,
        actual: u64,
    },
    #[error("packed-index shard {path} does not match its manifest SHA-256")]
    PackedIndexDigestMismatch { path: String },
    #[error("packed-index shard {path} is missing required inner slot {slot}")]
    MissingPackedIndexSlot { path: String, slot: usize },
    #[error("packed-index shard {path} has an unused inner slot {slot}")]
    UnexpectedPackedIndexSlot { path: String, slot: usize },
    #[error("packed-index shard {path} inner slot {slot} has nonzero trailing array-fill bytes")]
    NonzeroPackedIndexPadding { path: String, slot: usize },
    #[error(
        "packed-index record {record} coordinates differ from the required C-order coordinates"
    )]
    PackedRecordCoordinateMismatch { record: u64 },
    #[error("packed-index record {record} explicit-validity flag differs from the profile")]
    PackedRecordValidityMismatch { record: u64 },
    #[error("logical slot {slot} was visited twice for shard {path}")]
    DuplicateLogicalSlot { path: String, slot: u64 },
    #[error("shard {path} is used with incompatible storage kinds")]
    InconsistentShardKind { path: String },
    #[error("listed {component} shard {path} contains no inner payload")]
    ListedAllMissingShard {
        component: &'static str,
        path: String,
    },
    #[error("{component} shard {path} has an out-of-grid inner payload in slot {slot}")]
    OutOfGridInnerPayload {
        component: &'static str,
        path: String,
        slot: usize,
    },
    #[error(
        "{component} shard {path} inner slot {slot} presence is {actual}; packed records require {expected}"
    )]
    InnerPayloadPresenceMismatch {
        component: &'static str,
        path: String,
        slot: usize,
        actual: bool,
        expected: bool,
    },
    #[error("{metric} is {actual}; admitted package fact is {expected}")]
    AdmissionCountMismatch {
        metric: &'static str,
        actual: u64,
        expected: u64,
    },
    #[error("{metric} arithmetic overflowed")]
    ArithmeticOverflow { metric: &'static str },
    #[error("{metric} cannot be represented on this platform")]
    PlatformLength { metric: &'static str },
}

#[derive(Clone, Copy)]
struct SlotExpectation {
    kind: ShardProfileKind,
    logical_mask: u64,
    required_mask: u64,
}

impl SlotExpectation {
    const fn new(kind: ShardProfileKind) -> Self {
        Self {
            kind,
            logical_mask: 0,
            required_mask: 0,
        }
    }
}

struct Reconciler<'a, 'cancel, F> {
    reader: &'a LocalPackageReader,
    arrays: &'a BTreeMap<PackagePath, ZarrArrayMetadata>,
    descriptors: &'a [PackageObjectDescriptor],
    is_cancelled: &'cancel mut F,
    pixel: BTreeMap<PackagePath, SlotExpectation>,
    validity: BTreeMap<PackagePath, SlotExpectation>,
    packed_paths: BTreeSet<PackagePath>,
    report: PackageStructureReport,
}

impl<F: FnMut() -> bool> Reconciler<'_, '_, F> {
    fn poll(&mut self) -> Result<(), PackageStructureError> {
        if (self.is_cancelled)() {
            Err(PackageStructureError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn tick(&mut self, amount: u64) -> Result<(), PackageStructureError> {
        self.report.work_operations = self.report.work_operations.checked_add(amount).ok_or(
            PackageStructureError::ArithmeticOverflow {
                metric: "structural work count",
            },
        )?;
        Ok(())
    }

    fn reconcile_level(
        &mut self,
        image_ordinal: u32,
        scale_ordinal: u32,
        pixel_base: &PackagePath,
        validity_base: Option<&PackagePath>,
        packed_base: &PackagePath,
        validity_mode: ProfileValidityMode,
    ) -> Result<(), PackageStructureError> {
        self.poll()?;
        let pixel_metadata_path = metadata_path(pixel_base)?;
        let pixel = required_array(self.arrays, &pixel_metadata_path, "pixel")?;
        let (brick_zyx, two_dimensional) = pixel_brick(pixel.kind()).ok_or_else(|| {
            PackageStructureError::UnexpectedArrayKind {
                component: "pixel",
                path: pixel_metadata_path.to_string(),
                kind: pixel.kind(),
            }
        })?;
        let shape: [u64; 5] =
            pixel
                .shape()
                .try_into()
                .map_err(|_| PackageStructureError::UnexpectedArrayKind {
                    component: "pixel",
                    path: pixel_metadata_path.to_string(),
                    kind: pixel.kind(),
                })?;
        let grid = brick_grid(shape, brick_zyx).map_err(|error| match error {
            crate::BrickAddressError::Path(error) => PackageStructureError::Path(error),
            _ => PackageStructureError::ArithmeticOverflow {
                metric: "logical brick grid",
            },
        })?;
        let record_count = checked_product("packed-index record count", &grid)?;

        let packed_metadata_path = metadata_path(packed_base)?;
        let packed = required_array(self.arrays, &packed_metadata_path, "packed-index")?;
        if packed.kind() != ShardProfileKind::PackedIndex {
            return Err(PackageStructureError::UnexpectedArrayKind {
                component: "packed-index",
                path: packed_metadata_path.to_string(),
                kind: packed.kind(),
            });
        }
        if packed.shape() != [record_count, PACKED_INDEX_RECORD_BYTES] {
            return Err(PackageStructureError::PackedIndexShapeMismatch {
                path: packed_metadata_path.to_string(),
                records: record_count,
            });
        }

        let packed_shards = checked_ceil_div(
            record_count,
            PACKED_INDEX_RECORDS_PER_OUTER_SHARD,
            "packed-index shard count",
        )?;
        for outer in 0..packed_shards {
            self.poll()?;
            let path = PackagePath::parse(&format!("{packed_base}/c/{outer}/0"))?;
            let descriptor = required_descriptor(
                self.descriptors,
                &path,
                PackageObjectKind::PackedIndexShard,
                "packed-index",
            )?
            .clone();
            let inserted = self.packed_paths.insert(path.clone());
            if !inserted {
                return Err(PackageStructureError::UnexpectedShardDescriptor {
                    component: "packed-index",
                    path: path.to_string(),
                });
            }
            self.scan_packed_shard(
                &descriptor,
                outer,
                record_count,
                image_ordinal,
                scale_ordinal,
                pixel_base,
                validity_base,
                validity_mode,
                pixel.kind(),
                shape,
                grid,
                brick_zyx,
                two_dimensional,
            )?;
            self.report.packed_index_shards = checked_add(
                "packed-index shards reconciled",
                self.report.packed_index_shards,
                1,
            )?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_packed_shard(
        &mut self,
        descriptor: &PackageObjectDescriptor,
        outer: u64,
        record_count: u64,
        image_ordinal: u32,
        scale_ordinal: u32,
        pixel_base: &PackagePath,
        validity_base: Option<&PackagePath>,
        validity_mode: ProfileValidityMode,
        pixel_kind: ShardProfileKind,
        shape: [u64; 5],
        grid: [u64; 5],
        brick_zyx: [u64; 3],
        two_dimensional: bool,
    ) -> Result<(), PackageStructureError> {
        let path = descriptor.path();
        let kind = ShardProfileKind::PackedIndex;
        let maximum = to_u64(kind.encoded_shard_bytes_max(), "packed-index shard ceiling")?;
        let (bytes, snapshot) = self.reader.read_object_with_snapshot(path, maximum)?;
        self.report.snapshots.push(snapshot);
        let actual_bytes = to_u64(bytes.len(), "packed-index object length")?;
        let declared_bytes = descriptor.raw().byte_length();
        if actual_bytes != declared_bytes {
            return Err(PackageStructureError::ObjectLengthMismatch {
                path: path.to_string(),
                expected: declared_bytes,
                actual: actual_bytes,
            });
        }
        if ExactBytesHasher::hash(&bytes)?.digest() != descriptor.raw().digest() {
            return Err(PackageStructureError::PackedIndexDigestMismatch {
                path: path.to_string(),
            });
        }

        let tail_bytes = kind.index_tail_bytes();
        let payload_bytes = bytes.len().checked_sub(tail_bytes).ok_or(
            PackageStructureError::ObjectLengthMismatch {
                path: path.to_string(),
                expected: to_u64(tail_bytes, "packed-index tail length")?,
                actual: actual_bytes,
            },
        )?;
        let index = decode_shard_index_tail(
            kind,
            &bytes[payload_bytes..],
            to_u64(payload_bytes, "packed-index payload length")?,
        )?;

        let outer_start = checked_mul(
            "packed-index outer record origin",
            outer,
            PACKED_INDEX_RECORDS_PER_OUTER_SHARD,
        )?;
        let records_in_outer = record_count
            .checked_sub(outer_start)
            .ok_or(PackageStructureError::ArithmeticOverflow {
                metric: "packed-index records remaining",
            })?
            .min(PACKED_INDEX_RECORDS_PER_OUTER_SHARD);
        let required_chunks = checked_ceil_div(
            records_in_outer,
            PACKED_INDEX_RECORDS_PER_INNER_CHUNK,
            "packed-index inner chunk count",
        )?;

        for slot in 0..kind.chunks_per_shard() {
            self.poll()?;
            self.tick(1)?;
            let required = u64::try_from(slot)
                .ok()
                .is_some_and(|slot| slot < required_chunks);
            let present = index.entry(slot)?.is_some();
            match (required, present) {
                (true, false) => {
                    return Err(PackageStructureError::MissingPackedIndexSlot {
                        path: path.to_string(),
                        slot,
                    });
                }
                (false, true) => {
                    return Err(PackageStructureError::UnexpectedPackedIndexSlot {
                        path: path.to_string(),
                        slot,
                    });
                }
                (true, true) | (false, false) => {}
            }
        }

        for inner in 0..required_chunks {
            self.poll()?;
            let slot =
                usize::try_from(inner).map_err(|_| PackageStructureError::PlatformLength {
                    metric: "packed-index inner slot",
                })?;
            let entry = index.entry(slot)?.ok_or_else(|| {
                PackageStructureError::MissingPackedIndexSlot {
                    path: path.to_string(),
                    slot,
                }
            })?;
            let range = entry.range();
            let start = usize::try_from(range.start).map_err(|_| {
                PackageStructureError::PlatformLength {
                    metric: "packed-index encoded range start",
                }
            })?;
            let end =
                usize::try_from(range.end).map_err(|_| PackageStructureError::PlatformLength {
                    metric: "packed-index encoded range end",
                })?;
            let encoded =
                bytes
                    .get(start..end)
                    .ok_or(PackageStructureError::ArithmeticOverflow {
                        metric: "packed-index encoded range",
                    })?;
            let decoded = decode_inner_payload(kind, encoded)?;
            let inner_start = checked_add(
                "packed-index inner record origin",
                outer_start,
                checked_mul(
                    "packed-index inner record origin",
                    inner,
                    PACKED_INDEX_RECORDS_PER_INNER_CHUNK,
                )?,
            )?;
            let records_in_inner = record_count
                .checked_sub(inner_start)
                .ok_or(PackageStructureError::ArithmeticOverflow {
                    metric: "packed-index inner records remaining",
                })?
                .min(PACKED_INDEX_RECORDS_PER_INNER_CHUNK);
            let used_bytes = checked_mul(
                "packed-index used decoded bytes",
                records_in_inner,
                PACKED_INDEX_RECORD_BYTES,
            )?;
            let used_bytes =
                usize::try_from(used_bytes).map_err(|_| PackageStructureError::PlatformLength {
                    metric: "packed-index used decoded bytes",
                })?;
            if decoded[used_bytes..].iter().any(|byte| *byte != 0) {
                return Err(PackageStructureError::NonzeroPackedIndexPadding {
                    path: path.to_string(),
                    slot,
                });
            }

            for row in 0..records_in_inner {
                self.poll()?;
                self.tick(1)?;
                let ordinal = checked_add("packed-index record ordinal", inner_start, row)?;
                let expected =
                    coordinates_for_ordinal(image_ordinal, scale_ordinal, grid, ordinal)?;
                let row_offset = checked_mul(
                    "packed-index record byte offset",
                    row,
                    PACKED_INDEX_RECORD_BYTES,
                )?;
                let row_offset = usize::try_from(row_offset).map_err(|_| {
                    PackageStructureError::PlatformLength {
                        metric: "packed-index record byte offset",
                    }
                })?;
                let row_end = row_offset
                    .checked_add(PACKED_INDEX_RECORD_BYTES as usize)
                    .ok_or(PackageStructureError::ArithmeticOverflow {
                        metric: "packed-index record byte end",
                    })?;
                let extent = edge_extent(
                    shape,
                    brick_zyx,
                    [
                        u64::from(expected.z_chunk()),
                        u64::from(expected.y_chunk()),
                        u64::from(expected.x_chunk()),
                    ],
                )
                .map_err(|_| PackageStructureError::ArithmeticOverflow {
                    metric: "logical edge-brick extent",
                })?;
                let logical_capacity = checked_product("logical edge-brick capacity", &extent)?;
                let record = PackedIndexRecord::decode(
                    &decoded[row_offset..row_end],
                    pixel_dtype(pixel_kind)?,
                    logical_capacity,
                )?;
                if record.coordinates() != expected {
                    return Err(PackageStructureError::PackedRecordCoordinateMismatch {
                        record: ordinal,
                    });
                }
                let explicit_validity = validity_mode == ProfileValidityMode::Explicit;
                if record.explicit_validity() != explicit_validity {
                    return Err(PackageStructureError::PackedRecordValidityMismatch {
                        record: ordinal,
                    });
                }
                self.mark_record_expectations(
                    pixel_base,
                    validity_base,
                    pixel_kind,
                    two_dimensional,
                    record,
                )?;
                self.report.records_visited = checked_add(
                    "packed-index records visited",
                    self.report.records_visited,
                    1,
                )?;
            }
        }
        Ok(())
    }

    fn mark_record_expectations(
        &mut self,
        pixel_base: &PackagePath,
        validity_base: Option<&PackagePath>,
        pixel_kind: ShardProfileKind,
        two_dimensional: bool,
        record: PackedIndexRecord,
    ) -> Result<(), PackageStructureError> {
        let coordinates = record.coordinates();
        let t = u64::from(coordinates.t());
        let c = u64::from(coordinates.c());
        let z = u64::from(coordinates.z_chunk());
        let y = u64::from(coordinates.y_chunk());
        let x = u64::from(coordinates.x_chunk());
        let slot = spatial_inner_chunk(z, y, x, two_dimensional);
        let pixel_path = spatial_shard_path(pixel_base, t, c, z, y, x)?;
        mark_slot(
            &mut self.pixel,
            pixel_path,
            pixel_kind,
            slot,
            record.pixel_payload_present(),
        )?;
        self.tick(1)?;

        if record.explicit_validity() {
            let base =
                validity_base.ok_or_else(|| PackageStructureError::MissingArrayMetadata {
                    component: "validity",
                    path: format!(
                        "validity path for image {} scale {}",
                        coordinates.image_ordinal(),
                        coordinates.scale()
                    ),
                })?;
            let validity_kind = if two_dimensional {
                ShardProfileKind::Validity2d
            } else {
                ShardProfileKind::Validity3d
            };
            let validity_path = spatial_shard_path(base, t, c, z, y, x)?;
            mark_slot(
                &mut self.validity,
                validity_path,
                validity_kind,
                slot,
                record.statistics().valid_voxel_count() > 0,
            )?;
            self.tick(1)?;
        }
        Ok(())
    }

    fn validate_descriptor_membership(
        &mut self,
        admission: DatasetProfileAdmission,
    ) -> Result<(), PackageStructureError> {
        let mut pixel = 0_u64;
        let mut validity = 0_u64;
        let mut packed = 0_u64;
        for descriptor in self.descriptors {
            if (self.is_cancelled)() {
                return Err(PackageStructureError::Cancelled);
            }
            match descriptor.kind() {
                PackageObjectKind::PixelShard => {
                    pixel = checked_add("listed pixel shard count", pixel, 1)?;
                    if !self.pixel.contains_key(descriptor.path()) {
                        return Err(PackageStructureError::UnexpectedShardDescriptor {
                            component: "pixel",
                            path: descriptor.path().to_string(),
                        });
                    }
                }
                PackageObjectKind::ValidityShard => {
                    validity = checked_add("listed validity shard count", validity, 1)?;
                    if !self.validity.contains_key(descriptor.path()) {
                        return Err(PackageStructureError::UnexpectedShardDescriptor {
                            component: "validity",
                            path: descriptor.path().to_string(),
                        });
                    }
                }
                PackageObjectKind::PackedIndexShard => {
                    packed = checked_add("listed packed-index shard count", packed, 1)?;
                    if !self.packed_paths.contains(descriptor.path()) {
                        return Err(PackageStructureError::UnexpectedShardDescriptor {
                            component: "packed-index",
                            path: descriptor.path().to_string(),
                        });
                    }
                }
                _ => {}
            }
        }
        let counts = admission.counts();
        require_admission_count("actual pixel shards", pixel, counts.actual_pixel_shards)?;
        require_admission_count(
            "actual validity shards",
            validity,
            counts.actual_validity_shards,
        )?;
        require_admission_count(
            "actual packed-index shards",
            packed,
            counts.actual_packed_index_shards,
        )?;
        self.report.listed_pixel_shards = pixel;
        self.report.listed_validity_shards = validity;
        Ok(())
    }

    fn reconcile_component_shards(
        &mut self,
        component: &'static str,
        expectations: BTreeMap<PackagePath, SlotExpectation>,
    ) -> Result<(), PackageStructureError> {
        for (path, expectation) in expectations {
            self.poll()?;
            let descriptor = optional_descriptor(self.descriptors, &path).cloned();
            let Some(descriptor) = descriptor else {
                if expectation.required_mask != 0 {
                    return Err(PackageStructureError::MissingShardDescriptor {
                        component,
                        path: path.to_string(),
                    });
                }
                continue;
            };
            let expected_kind = match component {
                "pixel" => PackageObjectKind::PixelShard,
                "validity" => PackageObjectKind::ValidityShard,
                _ => unreachable!("closed structural component"),
            };
            if descriptor.kind() != expected_kind {
                return Err(PackageStructureError::DescriptorKindMismatch {
                    path: path.to_string(),
                    expected: expected_kind,
                    actual: descriptor.kind(),
                });
            }
            let maximum = to_u64(
                expectation.kind.encoded_shard_bytes_max(),
                "encoded shard ceiling",
            )?;
            let tail_bytes = to_u64(expectation.kind.index_tail_bytes(), "shard tail length")?;
            let (tail, payload_bytes, snapshot) = self
                .reader
                .read_shard_index_tail_with_snapshot(&path, tail_bytes, maximum)?;
            self.report.snapshots.push(snapshot);
            let actual_bytes = payload_bytes.checked_add(tail_bytes).ok_or(
                PackageStructureError::ArithmeticOverflow {
                    metric: "complete shard length",
                },
            )?;
            if actual_bytes != descriptor.raw().byte_length() {
                return Err(PackageStructureError::ObjectLengthMismatch {
                    path: path.to_string(),
                    expected: descriptor.raw().byte_length(),
                    actual: actual_bytes,
                });
            }
            let index = decode_shard_index_tail(expectation.kind, &tail, payload_bytes)?;
            let mut actual_mask = 0_u64;
            for slot in 0..expectation.kind.chunks_per_shard() {
                self.poll()?;
                self.tick(1)?;
                let bit = bit_for_slot(slot)?;
                let actual = index.entry(slot)?.is_some();
                if actual {
                    actual_mask |= bit;
                }
                let logical = expectation.logical_mask & bit != 0;
                let required = expectation.required_mask & bit != 0;
                if actual && !logical {
                    return Err(PackageStructureError::OutOfGridInnerPayload {
                        component,
                        path: path.to_string(),
                        slot,
                    });
                }
                if actual != required {
                    return Err(PackageStructureError::InnerPayloadPresenceMismatch {
                        component,
                        path: path.to_string(),
                        slot,
                        actual,
                        expected: required,
                    });
                }
            }
            if actual_mask == 0 {
                return Err(PackageStructureError::ListedAllMissingShard {
                    component,
                    path: path.to_string(),
                });
            }
        }
        Ok(())
    }
}

pub(crate) fn reconcile_package_structure(
    input: PackageStructureInput<'_>,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<PackageStructureReport, PackageStructureError> {
    let PackageStructureInput {
        reader,
        profile,
        arrays,
        descriptors,
        admission,
    } = input;
    let mut reconciler = Reconciler {
        reader,
        arrays,
        descriptors,
        is_cancelled: &mut is_cancelled,
        pixel: BTreeMap::new(),
        validity: BTreeMap::new(),
        packed_paths: BTreeSet::new(),
        report: PackageStructureReport::default(),
    };
    reconciler.poll()?;
    for image in profile.images() {
        reconciler.poll()?;
        for level in image.levels() {
            reconciler.reconcile_level(
                image.image_ordinal(),
                level.scale_ordinal(),
                level.pixel_path(),
                level.validity_path(),
                level.packed_index_path(),
                level.validity_mode(),
            )?;
        }
    }

    let admitted = admission.counts();
    require_admission_count(
        "logical brick records",
        reconciler.report.records_visited,
        admitted.logical_bricks,
    )?;
    require_admission_count(
        "addressed packed-index shards",
        reconciler.report.packed_index_shards,
        admitted.addressed_packed_index_shards,
    )?;
    reconciler.report.addressed_pixel_shards = to_u64(
        reconciler.pixel.len(),
        "addressed pixel shard expectation count",
    )?;
    reconciler.report.addressed_validity_shards = to_u64(
        reconciler.validity.len(),
        "addressed validity shard expectation count",
    )?;
    require_admission_count(
        "addressed pixel shards",
        reconciler.report.addressed_pixel_shards,
        admitted.addressed_pixel_shards,
    )?;
    require_admission_count(
        "addressed validity shards",
        reconciler.report.addressed_validity_shards,
        admitted.addressed_validity_shards,
    )?;
    reconciler.validate_descriptor_membership(admission)?;

    let pixel = std::mem::take(&mut reconciler.pixel);
    reconciler.reconcile_component_shards("pixel", pixel)?;
    let validity = std::mem::take(&mut reconciler.validity);
    reconciler.reconcile_component_shards("validity", validity)?;
    reconciler.poll()?;
    Ok(reconciler.report)
}

fn mark_slot(
    expectations: &mut BTreeMap<PackagePath, SlotExpectation>,
    path: PackagePath,
    kind: ShardProfileKind,
    slot: u64,
    required: bool,
) -> Result<(), PackageStructureError> {
    let bit = bit_for_slot(usize::try_from(slot).map_err(|_| {
        PackageStructureError::PlatformLength {
            metric: "inner slot",
        }
    })?)?;
    let expectation = expectations
        .entry(path.clone())
        .or_insert_with(|| SlotExpectation::new(kind));
    if expectation.kind != kind {
        return Err(PackageStructureError::InconsistentShardKind {
            path: path.to_string(),
        });
    }
    if expectation.logical_mask & bit != 0 {
        return Err(PackageStructureError::DuplicateLogicalSlot {
            path: path.to_string(),
            slot,
        });
    }
    expectation.logical_mask |= bit;
    if required {
        expectation.required_mask |= bit;
    }
    Ok(())
}

fn coordinates_for_ordinal(
    image: u32,
    scale: u32,
    grid: [u64; 5],
    ordinal: u64,
) -> Result<PackedIndexCoordinates, PackageStructureError> {
    let mut remaining = ordinal;
    let mut coordinates = [0_u64; 5];
    for axis in (0..coordinates.len()).rev() {
        let count = grid[axis];
        if count == 0 {
            return Err(PackageStructureError::ArithmeticOverflow {
                metric: "zero logical brick-grid dimension",
            });
        }
        coordinates[axis] = remaining % count;
        remaining /= count;
    }
    if remaining != 0 {
        return Err(PackageStructureError::ArithmeticOverflow {
            metric: "packed-index record coordinate",
        });
    }
    Ok(PackedIndexCoordinates::new(
        image,
        scale,
        to_u32(coordinates[0], "t packed coordinate")?,
        to_u32(coordinates[1], "c packed coordinate")?,
        to_u32(coordinates[2], "z packed coordinate")?,
        to_u32(coordinates[3], "y packed coordinate")?,
        to_u32(coordinates[4], "x packed coordinate")?,
    ))
}

fn reconcile_descriptor_kind(
    descriptor: &PackageObjectDescriptor,
    expected: PackageObjectKind,
) -> Result<(), PackageStructureError> {
    if descriptor.kind() != expected {
        Err(PackageStructureError::DescriptorKindMismatch {
            path: descriptor.path().to_string(),
            expected,
            actual: descriptor.kind(),
        })
    } else {
        Ok(())
    }
}

fn required_descriptor<'a>(
    descriptors: &'a [PackageObjectDescriptor],
    path: &PackagePath,
    expected: PackageObjectKind,
    component: &'static str,
) -> Result<&'a PackageObjectDescriptor, PackageStructureError> {
    let descriptor = optional_descriptor(descriptors, path).ok_or_else(|| {
        PackageStructureError::MissingShardDescriptor {
            component,
            path: path.to_string(),
        }
    })?;
    reconcile_descriptor_kind(descriptor, expected)?;
    Ok(descriptor)
}

fn optional_descriptor<'a>(
    descriptors: &'a [PackageObjectDescriptor],
    path: &PackagePath,
) -> Option<&'a PackageObjectDescriptor> {
    descriptors
        .binary_search_by(|descriptor| descriptor.path().cmp(path))
        .ok()
        .map(|index| &descriptors[index])
}

fn required_array<'a>(
    arrays: &'a BTreeMap<PackagePath, ZarrArrayMetadata>,
    path: &PackagePath,
    component: &'static str,
) -> Result<&'a ZarrArrayMetadata, PackageStructureError> {
    arrays
        .get(path)
        .ok_or_else(|| PackageStructureError::MissingArrayMetadata {
            component,
            path: path.to_string(),
        })
}

fn metadata_path(base: &PackagePath) -> Result<PackagePath, StorageProfileError> {
    PackagePath::parse(&format!("{base}/zarr.json"))
}

fn spatial_shard_path(
    base: &PackagePath,
    t: u64,
    c: u64,
    z: u64,
    y: u64,
    x: u64,
) -> Result<PackagePath, StorageProfileError> {
    PackagePath::parse(&format!(
        "{base}/c/{t}/{c}/{}/{}/{}",
        z / SPATIAL_INNER_CHUNKS_PER_SHARD_AXIS,
        y / SPATIAL_INNER_CHUNKS_PER_SHARD_AXIS,
        x / SPATIAL_INNER_CHUNKS_PER_SHARD_AXIS
    ))
}

fn pixel_dtype(
    kind: ShardProfileKind,
) -> Result<mirante4d_domain::IntensityDType, PackageStructureError> {
    use mirante4d_domain::IntensityDType;
    match kind {
        ShardProfileKind::Pixel3dUint8 | ShardProfileKind::Pixel2dUint8 => {
            Ok(IntensityDType::Uint8)
        }
        ShardProfileKind::Pixel3dUint16 | ShardProfileKind::Pixel2dUint16 => {
            Ok(IntensityDType::Uint16)
        }
        ShardProfileKind::Pixel3dFloat32 | ShardProfileKind::Pixel2dFloat32 => {
            Ok(IntensityDType::Float32)
        }
        ShardProfileKind::Validity3d
        | ShardProfileKind::Validity2d
        | ShardProfileKind::PackedIndex => Err(PackageStructureError::UnexpectedArrayKind {
            component: "pixel",
            path: "<profile-kind>".to_owned(),
            kind,
        }),
    }
}

fn bit_for_slot(slot: usize) -> Result<u64, PackageStructureError> {
    let shift = u32::try_from(slot).map_err(|_| PackageStructureError::PlatformLength {
        metric: "inner slot bit",
    })?;
    1_u64
        .checked_shl(shift)
        .ok_or(PackageStructureError::ArithmeticOverflow {
            metric: "inner slot bit",
        })
}

fn require_admission_count(
    metric: &'static str,
    actual: u64,
    expected: u64,
) -> Result<(), PackageStructureError> {
    if actual == expected {
        Ok(())
    } else {
        Err(PackageStructureError::AdmissionCountMismatch {
            metric,
            actual,
            expected,
        })
    }
}

fn checked_ceil_div(
    value: u64,
    divisor: u64,
    metric: &'static str,
) -> Result<u64, PackageStructureError> {
    if divisor == 0 {
        return Err(PackageStructureError::ArithmeticOverflow { metric });
    }
    value
        .checked_add(divisor - 1)
        .map(|value| value / divisor)
        .ok_or(PackageStructureError::ArithmeticOverflow { metric })
}

fn checked_product(metric: &'static str, values: &[u64]) -> Result<u64, PackageStructureError> {
    values
        .iter()
        .try_fold(1_u64, |product, value| checked_mul(metric, product, *value))
}

fn checked_add(metric: &'static str, left: u64, right: u64) -> Result<u64, PackageStructureError> {
    left.checked_add(right)
        .ok_or(PackageStructureError::ArithmeticOverflow { metric })
}

fn checked_mul(metric: &'static str, left: u64, right: u64) -> Result<u64, PackageStructureError> {
    left.checked_mul(right)
        .ok_or(PackageStructureError::ArithmeticOverflow { metric })
}

fn to_u64(value: usize, metric: &'static str) -> Result<u64, PackageStructureError> {
    u64::try_from(value).map_err(|_| PackageStructureError::PlatformLength { metric })
}

fn to_u32(value: u64, metric: &'static str) -> Result<u32, PackageStructureError> {
    u32::try_from(value).map_err(|_| PackageStructureError::PlatformLength { metric })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_order_edges_slots_and_packed_boundaries_are_exact() {
        let shape = [1, 1, 1, 257, 257];
        let brick = [1, 256, 256];
        let grid = brick_grid(shape, brick).unwrap();
        assert_eq!(grid, [1, 1, 1, 2, 2]);

        let expected = [
            (0, [0, 0, 0], 0, [1, 256, 256], 65_536),
            (1, [0, 0, 1], 1, [1, 256, 1], 256),
            (2, [0, 1, 0], 4, [1, 1, 256], 256),
            (3, [0, 1, 1], 5, [1, 1, 1], 1),
        ];
        let base = PackagePath::parse("images/i00000000/s00").unwrap();
        let mut slots = BTreeMap::new();
        for (ordinal, zyx, slot, extent, capacity) in expected {
            let coordinates = coordinates_for_ordinal(0, 0, grid, ordinal).unwrap();
            assert_eq!(
                [
                    coordinates.z_chunk(),
                    coordinates.y_chunk(),
                    coordinates.x_chunk(),
                ],
                zyx
            );
            assert_eq!(
                spatial_inner_chunk(
                    u64::from(zyx[0]),
                    u64::from(zyx[1]),
                    u64::from(zyx[2]),
                    true,
                ),
                slot
            );
            assert_eq!(
                edge_extent(shape, brick, zyx.map(u64::from)).unwrap(),
                extent
            );
            assert_eq!(
                checked_product("test edge capacity", &extent).unwrap(),
                capacity
            );
            mark_slot(
                &mut slots,
                spatial_shard_path(
                    &base,
                    0,
                    0,
                    u64::from(zyx[0]),
                    u64::from(zyx[1]),
                    u64::from(zyx[2]),
                )
                .unwrap(),
                ShardProfileKind::Pixel2dUint8,
                slot,
                ordinal.is_multiple_of(2),
            )
            .unwrap();
        }
        let slots = slots.values().next().unwrap();
        assert_eq!(
            slots.logical_mask,
            (1 << 0) | (1 << 1) | (1 << 4) | (1 << 5)
        );
        assert_eq!(slots.required_mask, (1 << 0) | (1 << 4));

        assert_eq!(
            checked_ceil_div(256, PACKED_INDEX_RECORDS_PER_INNER_CHUNK, "test").unwrap(),
            1
        );
        assert_eq!(
            checked_ceil_div(257, PACKED_INDEX_RECORDS_PER_INNER_CHUNK, "test").unwrap(),
            2
        );
        assert_eq!(
            checked_ceil_div(16_384, PACKED_INDEX_RECORDS_PER_OUTER_SHARD, "test").unwrap(),
            1
        );
        assert_eq!(
            checked_ceil_div(16_385, PACKED_INDEX_RECORDS_PER_OUTER_SHARD, "test").unwrap(),
            2
        );
        for (ordinal, expected_location) in [
            (255, (0, 0, 16_320)),
            (256, (0, 1, 0)),
            (16_383, (0, 63, 16_320)),
            (16_384, (1, 0, 0)),
        ] {
            let outer = ordinal / PACKED_INDEX_RECORDS_PER_OUTER_SHARD;
            let within_outer = ordinal % PACKED_INDEX_RECORDS_PER_OUTER_SHARD;
            let inner = within_outer / PACKED_INDEX_RECORDS_PER_INNER_CHUNK;
            let offset =
                (within_outer % PACKED_INDEX_RECORDS_PER_INNER_CHUNK) * PACKED_INDEX_RECORD_BYTES;
            assert_eq!((outer, inner, offset), expected_location);
            assert_eq!(
                coordinates_for_ordinal(7, 3, [1, 1, 1, 1, 16_385], ordinal)
                    .unwrap()
                    .x_chunk(),
                u32::try_from(ordinal).unwrap()
            );
        }
    }
}
