use std::sync::Arc;

use mirante4d_dataset::{DecodeSinkError, ReservedDecodeSink};
use mirante4d_domain::IntensityDType;
use thiserror::Error;

use crate::range_io::{
    LocalCurrentnessBatch, LocalObjectSnapshot, LocalShardChunkBytes, LocalShardChunkReadError,
};
use crate::shard::DirectInnerDecodeError;
use crate::{
    BrickAddressError, ELIDED_ALL_FILL_AMPLIFICATION, LocalBrickAddressPlan, LocalPackageReader,
    OneBrickAmplification, PackageObjectDescriptor, PackageObjectKind, PackagePath,
    PackedIndexError, PackedIndexRecord, RangeReadError, ShardCodecError, ShardProfileKind,
    amplification_2d, amplification_3d,
};

/// CRC-checked storage payloads and packed facts for one logical brick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalBrickRead {
    record: PackedIndexRecord,
    logical_extent_zyx: [u64; 3],
    pixel_payload: Option<Vec<u8>>,
    validity_payload: Option<Vec<u8>>,
    payload_facts: Option<LocalBrickPayloadFacts>,
    range_requests: u8,
    encoded_bytes_read: u64,
    decoded_bytes: u64,
    object_snapshots: Vec<LocalObjectSnapshot>,
}

impl LocalBrickRead {
    pub const fn record(&self) -> PackedIndexRecord {
        self.record
    }

    pub const fn logical_extent_zyx(&self) -> [u64; 3] {
        self.logical_extent_zyx
    }

    pub fn pixel_payload(&self) -> Option<&[u8]> {
        self.pixel_payload.as_deref()
    }

    pub fn validity_payload(&self) -> Option<&[u8]> {
        self.validity_payload.as_deref()
    }

    /// Facts proved from the decoded logical samples on the normal runtime
    /// read path. Package-wide scientific scans deliberately omit this work
    /// because their tiled identity pass already consumes every sample.
    pub(crate) const fn payload_facts(&self) -> Option<LocalBrickPayloadFacts> {
        self.payload_facts
    }

    pub const fn range_requests(&self) -> u8 {
        self.range_requests
    }

    pub const fn encoded_bytes_read(&self) -> u64 {
        self.encoded_bytes_read
    }

    pub const fn decoded_bytes(&self) -> u64 {
        self.decoded_bytes
    }

    pub(crate) fn retained_payload_bytes(&self) -> Result<u64, PackageReadError> {
        let pixel = self.pixel_payload.as_ref().map_or(0, Vec::len);
        let validity = self.validity_payload.as_ref().map_or(0, Vec::len);
        u64::try_from(pixel)
            .ok()
            .and_then(|bytes| bytes.checked_add(u64::try_from(validity).ok()?))
            .and_then(|bytes| bytes.checked_add(512))
            .ok_or(PackageReadError::AccountingOverflow {
                metric: "retained physical brick bytes",
            })
    }

    pub(crate) fn object_snapshots(&self) -> &[LocalObjectSnapshot] {
        &self.object_snapshots
    }
}

/// Exact-bit payload facts computed together with one physical brick decode.
///
/// Keeping the bit representation makes this value `Eq` and preserves signed
/// zero when comparing the decoded samples with packed-index statistics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LocalBrickPayloadFacts {
    minimum_bits: u32,
    maximum_bits: u32,
    any_valid: bool,
    all_valid: bool,
}

pub(crate) struct LocalDirectBrickRead {
    payload_facts: LocalBrickPayloadFacts,
    object_snapshots: Vec<LocalObjectSnapshot>,
    direct_span_bytes: u64,
    post_decode_copy_bytes: u64,
}

impl LocalDirectBrickRead {
    pub(crate) const fn payload_facts(&self) -> LocalBrickPayloadFacts {
        self.payload_facts
    }

    pub(crate) fn object_snapshots(&self) -> &[LocalObjectSnapshot] {
        &self.object_snapshots
    }

    pub(crate) const fn direct_span_bytes(&self) -> u64 {
        self.direct_span_bytes
    }

    pub(crate) const fn post_decode_copy_bytes(&self) -> u64 {
        self.post_decode_copy_bytes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DirectPayloadFactsAuthority {
    ScanDecoded,
    PublishedPackedRecord,
}

pub(crate) enum LocalDirectBrickReadError {
    Package(PackageReadError),
    Sink(DecodeSinkError),
}

impl From<PackageReadError> for LocalDirectBrickReadError {
    fn from(error: PackageReadError) -> Self {
        Self::Package(error)
    }
}

impl From<DecodeSinkError> for LocalDirectBrickReadError {
    fn from(error: DecodeSinkError) -> Self {
        Self::Sink(error)
    }
}

impl From<RangeReadError> for LocalDirectBrickReadError {
    fn from(error: RangeReadError) -> Self {
        Self::Package(PackageReadError::Range(error))
    }
}

impl From<BrickAddressError> for LocalDirectBrickReadError {
    fn from(error: BrickAddressError) -> Self {
        Self::Package(PackageReadError::Address(error))
    }
}

impl From<PackedIndexError> for LocalDirectBrickReadError {
    fn from(error: PackedIndexError) -> Self {
        Self::Package(PackageReadError::PackedIndex(error))
    }
}

impl LocalBrickPayloadFacts {
    pub(crate) const fn minimum(self) -> f32 {
        f32::from_bits(self.minimum_bits)
    }

    pub(crate) const fn maximum(self) -> f32 {
        f32::from_bits(self.maximum_bits)
    }

    pub(crate) const fn any_valid(self) -> bool {
        self.any_valid
    }

    pub(crate) const fn all_valid(self) -> bool {
        self.all_valid
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PackageReadError {
    #[error(transparent)]
    Address(#[from] BrickAddressError),
    #[error(transparent)]
    Range(#[from] RangeReadError),
    #[error(transparent)]
    Shard(#[from] ShardCodecError),
    #[error(transparent)]
    PackedIndex(#[from] PackedIndexError),
    #[error("exact-package brick read was cancelled")]
    Cancelled,
    #[error("manifest object {path} has kind {actual:?}; expected {expected:?}")]
    DescriptorKindMismatch {
        path: String,
        expected: PackageObjectKind,
        actual: PackageObjectKind,
    },
    #[error("required {component} shard {path} is absent from the manifest")]
    MissingRequiredShardDescriptor {
        component: &'static str,
        path: String,
    },
    #[error("object {path} has {actual} bytes; manifest declares {expected}")]
    ObjectLengthMismatch {
        path: String,
        expected: u64,
        actual: u64,
    },
    #[error("required {component} inner chunk {chunk_index} is missing from shard {path}")]
    MissingRequiredInnerPayload {
        component: &'static str,
        path: String,
        chunk_index: usize,
    },
    #[error("packed-index inner payload cannot contain record byte range at offset {offset}")]
    PackedRecordOutOfBounds { offset: u64 },
    #[error("packed-index record coordinates differ from the requested brick")]
    PackedRecordCoordinateMismatch,
    #[error("packed-index explicit-validity flag differs from the profile")]
    PackedRecordValidityMismatch,
    #[error(
        "decoded float32 pixel sample {sample_index} is non-finite (sample_valid={sample_valid})"
    )]
    NonFinitePixelPayload {
        sample_index: usize,
        sample_valid: bool,
    },
    #[error("packed-index statistics do not match the decoded logical samples")]
    PackedStatisticsMismatch,
    #[error("decoded {component} payload does not cover its fixed physical brick")]
    DecodedPayloadInvariant { component: &'static str },
    #[error("the requested brick is not eligible for aligned direct delivery")]
    DirectDeliveryIneligible,
    #[error("{metric} accounting overflowed")]
    AccountingOverflow { metric: &'static str },
    #[error("one-brick {metric} is {actual}; maximum is {maximum}")]
    AmplificationExceeded {
        metric: &'static str,
        actual: u64,
        maximum: u64,
    },
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn read_local_brick(
    reader: &LocalPackageReader,
    descriptors: &[PackageObjectDescriptor],
    plan: LocalBrickAddressPlan,
) -> Result<LocalBrickRead, PackageReadError> {
    read_local_brick_core(reader, descriptors, plan, false, false)
}

pub(crate) fn read_local_brick_reusing(
    reader: &LocalPackageReader,
    descriptors: &[PackageObjectDescriptor],
    plan: LocalBrickAddressPlan,
) -> Result<LocalBrickRead, PackageReadError> {
    read_local_brick_core(reader, descriptors, plan, true, true)
}

pub(crate) fn read_local_brick_reusing_for_scientific_scan(
    reader: &LocalPackageReader,
    descriptors: &[PackageObjectDescriptor],
    plan: LocalBrickAddressPlan,
) -> Result<LocalBrickRead, PackageReadError> {
    // This is the sole package-wide proof pass for packed record facts. The
    // resulting scientific capability may later derive renderer facts from
    // authenticated records without rescanning delivered voxels.
    read_local_brick_core(reader, descriptors, plan, true, true)
}

/// Decodes one physical brick inside a caller-owned currentness transaction.
/// The caller must finish that transaction before publishing any semantic
/// result derived from the returned bytes.
pub(crate) fn read_local_brick_reusing_in_transaction(
    reader: &LocalPackageReader,
    descriptors: &[PackageObjectDescriptor],
    plan: LocalBrickAddressPlan,
    transaction: &mut LocalCurrentnessBatch<'_>,
    facts_authority: DirectPayloadFactsAuthority,
) -> Result<LocalBrickRead, PackageReadError> {
    read_local_brick_components(
        reader,
        descriptors,
        plan,
        Some(transaction),
        Some(facts_authority),
    )
}

/// Delivers one complete 3D physical brick through the caller's reservation
/// without allocating or retaining a decoded pixel-sized `Vec`.
pub(crate) fn read_local_brick_reusing_into_sink_in_transaction(
    reader: &LocalPackageReader,
    descriptors: &[PackageObjectDescriptor],
    plan: LocalBrickAddressPlan,
    sink: &mut dyn ReservedDecodeSink,
    transaction: &mut LocalCurrentnessBatch<'_>,
    facts_authority: DirectPayloadFactsAuthority,
) -> Result<LocalDirectBrickRead, LocalDirectBrickReadError> {
    if plan.logical_extent_zyx() != [64, 64, 64]
        || !matches!(
            plan.pixel_kind(),
            ShardProfileKind::Pixel3dUint8
                | ShardProfileKind::Pixel3dUint16
                | ShardProfileKind::Pixel3dFloat32
        )
    {
        return Err(PackageReadError::DirectDeliveryIneligible.into());
    }
    if sink.is_cancelled() {
        return Err(PackageReadError::Cancelled.into());
    }
    let dtype = pixel_dtype(plan.pixel_kind());
    let logical_capacity = 64_u64 * 64 * 64;

    let packed_descriptor = required_descriptor(
        descriptors,
        plan.packed_index_shard_path(),
        PackageObjectKind::PackedIndexShard,
        "packed-index",
    )?;
    let packed = read_component_in_transaction(
        reader,
        packed_descriptor,
        ShardProfileKind::PackedIndex,
        plan.packed_index_inner_chunk(),
        transaction,
    )?;
    let packed_payload =
        packed
            .payload
            .ok_or_else(|| PackageReadError::MissingRequiredInnerPayload {
                component: "packed-index",
                path: plan.packed_index_shard_path().to_string(),
                chunk_index: usize::try_from(plan.packed_index_inner_chunk()).unwrap_or(usize::MAX),
            })?;
    let record_offset = usize::try_from(plan.packed_index_record_byte_offset()).map_err(|_| {
        PackageReadError::PackedRecordOutOfBounds {
            offset: plan.packed_index_record_byte_offset(),
        }
    })?;
    let record_end = record_offset
        .checked_add(crate::PACKED_INDEX_RECORD_BYTES as usize)
        .ok_or(PackageReadError::PackedRecordOutOfBounds {
            offset: plan.packed_index_record_byte_offset(),
        })?;
    let record_bytes = packed_payload
        .as_slice()
        .get(record_offset..record_end)
        .ok_or(PackageReadError::PackedRecordOutOfBounds {
            offset: plan.packed_index_record_byte_offset(),
        })?;
    let record = PackedIndexRecord::decode(record_bytes, dtype, logical_capacity)?;
    if record.coordinates() != plan.coordinates() {
        return Err(PackageReadError::PackedRecordCoordinateMismatch.into());
    }
    let explicit_validity = plan.validity_shard_path().is_some();
    if record.explicit_validity() != explicit_validity {
        return Err(PackageReadError::PackedRecordValidityMismatch.into());
    }

    let scan_facts = facts_authority == DirectPayloadFactsAuthority::ScanDecoded;
    let mut metrics = packed.metrics;
    let mut used_objects = vec![packed.snapshot];
    // An admitted source derives facts from validity before the pixel stream.
    // An importer-published capability already checked the record, so it
    // retains the raw component and decodes it directly into the final
    // validity span after the values.
    let mut validity_raw = None;
    // An admitted external package has not proved its packed statistics. On
    // that path (`ScanDecoded`) the record's zero-validity hint must not
    // suppress a physically present validity payload read: decode it and
    // compare authoritative facts. A canonical all-invalid brick has no
    // validity payload, so there is no hidden payload to suppress.
    let untrusted_validity_probe = scan_facts
        && explicit_validity
        && record.statistics().valid_voxel_count() == 0
        && plan.validity_shard_listed() == Some(true);
    let validity_payload = if explicit_validity
        && (record.statistics().valid_voxel_count() > 0 || untrusted_validity_probe)
    {
        let path = plan
            .validity_shard_path()
            .ok_or(PackageReadError::PackedRecordValidityMismatch)?;
        if plan.validity_shard_listed() != Some(true) {
            return Err(PackageReadError::MissingRequiredShardDescriptor {
                component: "validity",
                path: path.to_string(),
            }
            .into());
        }
        let descriptor = required_descriptor(
            descriptors,
            path,
            PackageObjectKind::ValidityShard,
            "validity",
        )?;
        let chunk_index = plan
            .validity_inner_chunk()
            .ok_or(PackageReadError::PackedRecordValidityMismatch)?;
        if scan_facts {
            let validity = read_component_in_transaction(
                reader,
                descriptor,
                ShardProfileKind::Validity3d,
                chunk_index,
                transaction,
            )?;
            metrics.add(validity.metrics)?;
            used_objects.push(validity.snapshot);
            match validity.payload {
                Some(payload) => Some(payload.into_vec()),
                None if record.statistics().valid_voxel_count() == 0 => None,
                None => {
                    return Err(PackageReadError::MissingRequiredInnerPayload {
                        component: "validity",
                        path: path.to_string(),
                        chunk_index: usize::try_from(chunk_index).unwrap_or(usize::MAX),
                    }
                    .into());
                }
            }
        } else {
            let raw = read_raw_component_in_transaction(
                descriptor,
                ShardProfileKind::Validity3d,
                chunk_index,
                transaction,
            )?;
            metrics.add(raw_component_metrics(&raw, ShardProfileKind::Validity3d)?)?;
            used_objects.push(raw.snapshot.clone());
            validity_raw = Some(raw);
            None
        }
    } else {
        None
    };

    let mut authoritative = AuthoritativeFactsAccumulator::default();
    let mut next_sample = 0_usize;
    let mut direct_span_bytes = 0_u64;
    let mut post_decode_copy_bytes = 0_u64;
    if record.pixel_payload_present() {
        if !plan.pixel_shard_listed() {
            return Err(PackageReadError::MissingRequiredShardDescriptor {
                component: "pixel",
                path: plan.pixel_shard_path().to_string(),
            }
            .into());
        }
        let descriptor = required_descriptor(
            descriptors,
            plan.pixel_shard_path(),
            PackageObjectKind::PixelShard,
            "pixel",
        )?;
        let raw = read_raw_component_in_transaction(
            descriptor,
            plan.pixel_kind(),
            plan.pixel_inner_chunk(),
            transaction,
        )?;
        let raw_metrics = raw_component_metrics(&raw, plan.pixel_kind())?;
        metrics.add(raw_metrics)?;
        used_objects.push(raw.snapshot.clone());
        match (raw.decoded, raw.encoded) {
            (Some(decoded), _) => {
                for bytes in decoded.chunks(64 * 1024) {
                    if sink.is_cancelled() {
                        return Err(PackageReadError::Cancelled.into());
                    }
                    if scan_facts {
                        scan_contiguous_payload_chunk(
                            &mut authoritative,
                            record,
                            dtype,
                            validity_payload.as_deref(),
                            &mut next_sample,
                            bytes,
                        )?;
                    }
                    sink.write(bytes)?;
                    post_decode_copy_bytes = post_decode_copy_bytes
                        .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                }
            }
            (None, Some(encoded)) => {
                let observe = |bytes: &[u8]| -> Result<(), PackageReadError> {
                    if scan_facts {
                        scan_contiguous_payload_chunk(
                            &mut authoritative,
                            record,
                            dtype,
                            validity_payload.as_deref(),
                            &mut next_sample,
                            bytes,
                        )?;
                    }
                    Ok(())
                };
                match reader.decode_inner_payload_direct_accounted(
                    plan.pixel_kind(),
                    &encoded,
                    sink,
                    observe,
                ) {
                    Ok(()) => {}
                    Err(DirectInnerDecodeError::Shard(error)) => {
                        return Err(PackageReadError::Shard(error).into());
                    }
                    Err(DirectInnerDecodeError::Sink(error)) => {
                        return Err(LocalDirectBrickReadError::Sink(error));
                    }
                    Err(DirectInnerDecodeError::Observer(error)) => {
                        return Err(LocalDirectBrickReadError::Package(error));
                    }
                }
                direct_span_bytes = direct_span_bytes.saturating_add(
                    u64::try_from(plan.pixel_kind().decoded_inner_bytes()).unwrap_or(u64::MAX),
                );
            }
            (None, None) => {
                return Err(PackageReadError::MissingRequiredInnerPayload {
                    component: "pixel",
                    path: plan.pixel_shard_path().to_string(),
                    chunk_index: usize::try_from(plan.pixel_inner_chunk()).unwrap_or(usize::MAX),
                }
                .into());
            }
        }
    } else {
        let value_bytes = usize::try_from(logical_capacity)
            .ok()
            .and_then(|samples| samples.checked_mul(usize::from(dtype.bytes_per_sample())))
            .ok_or(PackageReadError::AccountingOverflow {
                metric: "aligned direct value bytes",
            })?;
        let mut remaining = value_bytes;
        while remaining != 0 {
            if sink.is_cancelled() {
                return Err(PackageReadError::Cancelled.into());
            }
            let requested = remaining.min(64 * 1024);
            {
                let bytes = sink.writable_span(requested)?;
                if bytes.len() != requested {
                    return Err(
                        PackageReadError::DecodedPayloadInvariant { component: "pixel" }.into(),
                    );
                }
                bytes.fill(0);
                if scan_facts {
                    scan_contiguous_payload_chunk(
                        &mut authoritative,
                        record,
                        dtype,
                        validity_payload.as_deref(),
                        &mut next_sample,
                        bytes,
                    )?;
                }
            }
            sink.commit_written(requested)?;
            direct_span_bytes =
                direct_span_bytes.saturating_add(u64::try_from(requested).unwrap_or(u64::MAX));
            remaining -= requested;
        }
    }
    let payload_facts = if scan_facts {
        if next_sample != usize::try_from(logical_capacity).unwrap() {
            return Err(PackageReadError::DecodedPayloadInvariant { component: "pixel" }.into());
        }
        finish_authoritative_payload_facts(authoritative, record, dtype, logical_capacity)?
    } else {
        payload_facts_from_published_record(record, dtype, logical_capacity)?
    };

    if explicit_validity {
        if let Some(validity) = &validity_payload {
            for bytes in validity.chunks(64 * 1024) {
                if sink.is_cancelled() {
                    return Err(PackageReadError::Cancelled.into());
                }
                sink.write(bytes)?;
                post_decode_copy_bytes = post_decode_copy_bytes
                    .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            }
        } else if let Some(raw) = validity_raw {
            match (raw.decoded, raw.encoded) {
                (Some(decoded), _) => {
                    for source in decoded.chunks(64 * 1024) {
                        if sink.is_cancelled() {
                            return Err(PackageReadError::Cancelled.into());
                        }
                        let written = source.len();
                        {
                            let destination = sink.writable_span(written)?;
                            if destination.len() != written {
                                return Err(PackageReadError::DecodedPayloadInvariant {
                                    component: "validity",
                                }
                                .into());
                            }
                            destination.copy_from_slice(source);
                        }
                        sink.commit_written(written)?;
                        post_decode_copy_bytes = post_decode_copy_bytes
                            .saturating_add(u64::try_from(written).unwrap_or(u64::MAX));
                    }
                }
                (None, Some(encoded)) => {
                    match reader.decode_inner_payload_direct_accounted(
                        ShardProfileKind::Validity3d,
                        &encoded,
                        sink,
                        |_| Ok::<(), PackageReadError>(()),
                    ) {
                        Ok(()) => {}
                        Err(DirectInnerDecodeError::Shard(error)) => {
                            return Err(PackageReadError::Shard(error).into());
                        }
                        Err(DirectInnerDecodeError::Sink(error)) => {
                            return Err(LocalDirectBrickReadError::Sink(error));
                        }
                        Err(DirectInnerDecodeError::Observer(error)) => {
                            return Err(LocalDirectBrickReadError::Package(error));
                        }
                    }
                    direct_span_bytes = direct_span_bytes.saturating_add(
                        u64::try_from(ShardProfileKind::Validity3d.decoded_inner_bytes())
                            .unwrap_or(u64::MAX),
                    );
                }
                (None, None) => {
                    return Err(PackageReadError::MissingRequiredInnerPayload {
                        component: "validity",
                        path: plan
                            .validity_shard_path()
                            .expect("explicit validity retains a path")
                            .to_string(),
                        chunk_index: usize::try_from(
                            plan.validity_inner_chunk()
                                .expect("explicit validity retains a chunk"),
                        )
                        .unwrap_or(usize::MAX),
                    }
                    .into());
                }
            }
        } else {
            let requested = 32 * 1024;
            {
                let bytes = sink.writable_span(requested)?;
                if bytes.len() != requested {
                    return Err(PackageReadError::DecodedPayloadInvariant {
                        component: "validity",
                    }
                    .into());
                }
                bytes.fill(0);
            }
            sink.commit_written(requested)?;
            direct_span_bytes = direct_span_bytes.saturating_add(requested as u64);
        }
    }

    enforce_amplification(
        plan.pixel_kind(),
        !record.pixel_payload_present()
            && (!explicit_validity || record.statistics().valid_voxel_count() == 0),
        untrusted_validity_probe,
        metrics,
    )?;
    Ok(LocalDirectBrickRead {
        payload_facts,
        object_snapshots: used_objects,
        direct_span_bytes,
        post_decode_copy_bytes,
    })
}

fn read_local_brick_core(
    reader: &LocalPackageReader,
    descriptors: &[PackageObjectDescriptor],
    plan: LocalBrickAddressPlan,
    reuse: bool,
    compute_payload_facts: bool,
) -> Result<LocalBrickRead, PackageReadError> {
    let mut transaction = if reuse {
        Some(reader.begin_cached_read_transaction()?)
    } else {
        None
    };
    let read = read_local_brick_components(
        reader,
        descriptors,
        plan,
        transaction.as_mut(),
        compute_payload_facts.then_some(DirectPayloadFactsAuthority::ScanDecoded),
    )?;
    if let Some(transaction) = transaction {
        transaction.finish_read()?;
    } else {
        reader.revalidate_snapshots(read.object_snapshots())?;
    }
    Ok(read)
}

fn read_local_brick_components(
    reader: &LocalPackageReader,
    descriptors: &[PackageObjectDescriptor],
    plan: LocalBrickAddressPlan,
    mut transaction: Option<&mut LocalCurrentnessBatch<'_>>,
    facts_authority: Option<DirectPayloadFactsAuthority>,
) -> Result<LocalBrickRead, PackageReadError> {
    let dtype = pixel_dtype(plan.pixel_kind());
    let logical_capacity = plan
        .logical_extent_zyx()
        .into_iter()
        .try_fold(1_u64, |count, dimension| count.checked_mul(dimension))
        .ok_or(PackageReadError::AccountingOverflow {
            metric: "logical brick capacity",
        })?;

    let packed_descriptor = required_descriptor(
        descriptors,
        plan.packed_index_shard_path(),
        PackageObjectKind::PackedIndexShard,
        "packed-index",
    )?;
    let packed = read_component(
        reader,
        packed_descriptor,
        ShardProfileKind::PackedIndex,
        plan.packed_index_inner_chunk(),
        transaction.as_deref_mut(),
    )?;
    let packed_payload =
        packed
            .payload
            .ok_or_else(|| PackageReadError::MissingRequiredInnerPayload {
                component: "packed-index",
                path: plan.packed_index_shard_path().to_string(),
                chunk_index: usize::try_from(plan.packed_index_inner_chunk()).unwrap_or(usize::MAX),
            })?;
    let record_offset = usize::try_from(plan.packed_index_record_byte_offset()).map_err(|_| {
        PackageReadError::PackedRecordOutOfBounds {
            offset: plan.packed_index_record_byte_offset(),
        }
    })?;
    let record_end = record_offset
        .checked_add(crate::PACKED_INDEX_RECORD_BYTES as usize)
        .ok_or(PackageReadError::PackedRecordOutOfBounds {
            offset: plan.packed_index_record_byte_offset(),
        })?;
    let record_bytes = packed_payload
        .as_slice()
        .get(record_offset..record_end)
        .ok_or(PackageReadError::PackedRecordOutOfBounds {
            offset: plan.packed_index_record_byte_offset(),
        })?;
    let record = PackedIndexRecord::decode(record_bytes, dtype, logical_capacity)?;
    if record.coordinates() != plan.coordinates() {
        return Err(PackageReadError::PackedRecordCoordinateMismatch);
    }
    let explicit_validity = plan.validity_shard_path().is_some();
    if record.explicit_validity() != explicit_validity {
        return Err(PackageReadError::PackedRecordValidityMismatch);
    }

    let mut metrics = packed.metrics;
    let mut used_objects = vec![packed.snapshot];
    let pixel_payload = if record.pixel_payload_present() {
        if !plan.pixel_shard_listed() {
            return Err(PackageReadError::MissingRequiredShardDescriptor {
                component: "pixel",
                path: plan.pixel_shard_path().to_string(),
            });
        }
        let descriptor = required_descriptor(
            descriptors,
            plan.pixel_shard_path(),
            PackageObjectKind::PixelShard,
            "pixel",
        )?;
        let pixel = read_component(
            reader,
            descriptor,
            plan.pixel_kind(),
            plan.pixel_inner_chunk(),
            transaction.as_deref_mut(),
        )?;
        metrics.add(pixel.metrics)?;
        used_objects.push(pixel.snapshot);
        Some(
            pixel
                .payload
                .ok_or_else(|| PackageReadError::MissingRequiredInnerPayload {
                    component: "pixel",
                    path: plan.pixel_shard_path().to_string(),
                    chunk_index: usize::try_from(plan.pixel_inner_chunk()).unwrap_or(usize::MAX),
                })?
                .into_vec(),
        )
    } else {
        None
    };

    let scan_facts = matches!(
        facts_authority,
        Some(DirectPayloadFactsAuthority::ScanDecoded)
    );
    // The ordinary admitted path cannot use an all-invalid hint to suppress a
    // validity payload that is physically present. Canonical all-invalid
    // records have no corresponding payload; that absence remains a format
    // fact rather than work that can be decoded.
    let untrusted_validity_probe = scan_facts
        && explicit_validity
        && record.statistics().valid_voxel_count() == 0
        && plan.validity_shard_listed() == Some(true);
    let validity_payload = if explicit_validity
        && (record.statistics().valid_voxel_count() > 0 || untrusted_validity_probe)
    {
        let path = plan
            .validity_shard_path()
            .ok_or(PackageReadError::PackedRecordValidityMismatch)?;
        if plan.validity_shard_listed() != Some(true) {
            return Err(PackageReadError::MissingRequiredShardDescriptor {
                component: "validity",
                path: path.to_string(),
            });
        }
        let descriptor = required_descriptor(
            descriptors,
            path,
            PackageObjectKind::ValidityShard,
            "validity",
        )?;
        let kind = if is_two_dimensional(plan.pixel_kind()) {
            ShardProfileKind::Validity2d
        } else {
            ShardProfileKind::Validity3d
        };
        let chunk_index = plan
            .validity_inner_chunk()
            .ok_or(PackageReadError::PackedRecordValidityMismatch)?;
        let validity = read_component(reader, descriptor, kind, chunk_index, transaction)?;
        metrics.add(validity.metrics)?;
        used_objects.push(validity.snapshot);
        match validity.payload {
            Some(payload) => Some(payload.into_vec()),
            None if scan_facts && record.statistics().valid_voxel_count() == 0 => None,
            None => {
                return Err(PackageReadError::MissingRequiredInnerPayload {
                    component: "validity",
                    path: path.to_string(),
                    chunk_index: usize::try_from(chunk_index).unwrap_or(usize::MAX),
                });
            }
        }
    } else {
        None
    };

    let payload_facts = facts_authority
        .map(|authority| match authority {
            DirectPayloadFactsAuthority::ScanDecoded => compute_authoritative_payload_facts(
                record,
                dtype,
                plan.pixel_kind(),
                plan.logical_extent_zyx(),
                pixel_payload.as_deref(),
                validity_payload.as_deref(),
            ),
            DirectPayloadFactsAuthority::PublishedPackedRecord => {
                payload_facts_from_published_record(record, dtype, logical_capacity)
            }
        })
        .transpose()?;

    enforce_amplification(
        plan.pixel_kind(),
        pixel_payload.is_none() && validity_payload.is_none(),
        untrusted_validity_probe,
        metrics,
    )?;
    Ok(LocalBrickRead {
        record,
        logical_extent_zyx: plan.logical_extent_zyx(),
        pixel_payload,
        validity_payload,
        payload_facts,
        range_requests: metrics.range_requests,
        encoded_bytes_read: metrics.encoded_bytes_read,
        decoded_bytes: metrics.decoded_bytes,
        object_snapshots: used_objects,
    })
}

enum DecodedComponentPayload {
    Owned(Vec<u8>),
    Shared(Arc<[u8]>),
}

impl DecodedComponentPayload {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            Self::Shared(bytes) => bytes,
        }
    }

    fn len(&self) -> usize {
        self.as_slice().len()
    }

    fn into_vec(self) -> Vec<u8> {
        match self {
            Self::Owned(bytes) => bytes,
            Self::Shared(bytes) => bytes.as_ref().to_vec(),
        }
    }
}

struct DecodedComponent {
    payload: Option<DecodedComponentPayload>,
    metrics: ReadMetrics,
    snapshot: LocalObjectSnapshot,
}

fn read_component(
    reader: &LocalPackageReader,
    descriptor: &PackageObjectDescriptor,
    kind: ShardProfileKind,
    chunk_index: u64,
    transaction: Option<&mut LocalCurrentnessBatch<'_>>,
) -> Result<DecodedComponent, PackageReadError> {
    let chunk_index = usize::try_from(chunk_index).map_err(|_| ShardCodecError::LengthOverflow)?;
    let raw = if let Some(transaction) = transaction {
        transaction.read_shard_chunk(
            descriptor.path(),
            kind,
            chunk_index,
            descriptor.raw().byte_length(),
        )
    } else {
        reader.read_shard_chunk_uncached(
            descriptor.path(),
            kind,
            chunk_index,
            descriptor.raw().byte_length(),
        )
    }
    .map_err(|error| map_chunk_error(descriptor.path(), error))?;
    decode_component(reader, kind, raw)
}

fn read_component_in_transaction(
    reader: &LocalPackageReader,
    descriptor: &PackageObjectDescriptor,
    kind: ShardProfileKind,
    chunk_index: u64,
    transaction: &mut LocalCurrentnessBatch<'_>,
) -> Result<DecodedComponent, PackageReadError> {
    let raw = read_raw_component_in_transaction(descriptor, kind, chunk_index, transaction)?;
    decode_component(reader, kind, raw)
}

fn read_raw_component_in_transaction(
    descriptor: &PackageObjectDescriptor,
    kind: ShardProfileKind,
    chunk_index: u64,
    transaction: &mut LocalCurrentnessBatch<'_>,
) -> Result<LocalShardChunkBytes, PackageReadError> {
    let chunk_index = usize::try_from(chunk_index).map_err(|_| ShardCodecError::LengthOverflow)?;
    transaction
        .read_shard_chunk(
            descriptor.path(),
            kind,
            chunk_index,
            descriptor.raw().byte_length(),
        )
        .map_err(|error| map_chunk_error(descriptor.path(), error))
}

fn raw_component_metrics(
    raw: &LocalShardChunkBytes,
    kind: ShardProfileKind,
) -> Result<ReadMetrics, PackageReadError> {
    let payload_bytes = if raw.decoded.is_some() || raw.encoded.is_some() {
        u64::try_from(kind.decoded_inner_bytes()).map_err(|_| {
            PackageReadError::AccountingOverflow {
                metric: "decoded bytes",
            }
        })?
    } else {
        0
    };
    Ok(ReadMetrics {
        range_requests: raw.range_requests,
        encoded_bytes_read: raw.encoded_bytes_read,
        decoded_bytes: raw.decoded_index_bytes.checked_add(payload_bytes).ok_or(
            PackageReadError::AccountingOverflow {
                metric: "decoded bytes",
            },
        )?,
    })
}

fn decode_component(
    reader: &LocalPackageReader,
    kind: ShardProfileKind,
    raw: LocalShardChunkBytes,
) -> Result<DecodedComponent, PackageReadError> {
    let payload = match (raw.decoded, raw.encoded) {
        (Some(decoded), _) => Some(DecodedComponentPayload::Shared(decoded)),
        (None, Some(encoded)) => Some(DecodedComponentPayload::Owned(
            reader.decode_inner_payload_accounted(kind, &encoded)?,
        )),
        (None, None) => None,
    };
    let payload_bytes = payload.as_ref().map_or(0, DecodedComponentPayload::len);
    let decoded_bytes = raw
        .decoded_index_bytes
        .checked_add(u64::try_from(payload_bytes).map_err(|_| {
            PackageReadError::AccountingOverflow {
                metric: "decoded bytes",
            }
        })?)
        .ok_or(PackageReadError::AccountingOverflow {
            metric: "decoded bytes",
        })?;
    Ok(DecodedComponent {
        payload,
        metrics: ReadMetrics {
            range_requests: raw.range_requests,
            encoded_bytes_read: raw.encoded_bytes_read,
            decoded_bytes,
        },
        snapshot: raw.snapshot,
    })
}

#[derive(Default)]
struct AuthoritativeFactsAccumulator {
    minimum: Option<(u64, f64)>,
    maximum: Option<(u64, f64)>,
    valid_samples: u64,
    nonfill_valid_samples: u64,
}

impl AuthoritativeFactsAccumulator {
    fn include_valid(
        &mut self,
        bits: u64,
        numeric: f64,
        is_fill: bool,
    ) -> Result<(), PackageReadError> {
        self.valid_samples =
            self.valid_samples
                .checked_add(1)
                .ok_or(PackageReadError::AccountingOverflow {
                    metric: "authoritative valid sample count",
                })?;
        if !is_fill {
            self.nonfill_valid_samples = self.nonfill_valid_samples.checked_add(1).ok_or(
                PackageReadError::AccountingOverflow {
                    metric: "authoritative non-fill sample count",
                },
            )?;
        }
        if self.minimum.is_none_or(|(_, value)| numeric < value) {
            self.minimum = Some((bits, numeric));
        }
        if self.maximum.is_none_or(|(_, value)| numeric > value) {
            self.maximum = Some((bits, numeric));
        }
        Ok(())
    }
}

fn decode_authoritative_sample(
    dtype: IntensityDType,
    bytes: Option<&[u8]>,
    sample_index: usize,
    sample_valid: bool,
) -> Result<(u64, f64, bool), PackageReadError> {
    let Some(bytes) = bytes else {
        return Ok((0, 0.0, true));
    };
    match dtype {
        IntensityDType::Uint8 => {
            let value = *bytes
                .first()
                .ok_or(PackageReadError::DecodedPayloadInvariant { component: "pixel" })?;
            Ok((u64::from(value), f64::from(value), value == 0))
        }
        IntensityDType::Uint16 => {
            let bytes: [u8; 2] = bytes
                .try_into()
                .map_err(|_| PackageReadError::DecodedPayloadInvariant { component: "pixel" })?;
            let value = u16::from_le_bytes(bytes);
            Ok((u64::from(value), f64::from(value), value == 0))
        }
        IntensityDType::Float32 => {
            let bytes: [u8; 4] = bytes
                .try_into()
                .map_err(|_| PackageReadError::DecodedPayloadInvariant { component: "pixel" })?;
            let bits = u32::from_le_bytes(bytes);
            let value = f32::from_bits(bits);
            if !value.is_finite() {
                return Err(PackageReadError::NonFinitePixelPayload {
                    sample_index,
                    sample_valid,
                });
            }
            Ok((u64::from(bits), f64::from(value), bits == 0))
        }
    }
}

fn authoritative_sample_is_valid(
    record: PackedIndexRecord,
    validity: Option<&[u8]>,
    sample_index: usize,
) -> Result<bool, PackageReadError> {
    if !record.explicit_validity() {
        return Ok(true);
    }
    // A missing explicit-validity inner is the profile's canonical all-zero
    // fill representation. Packed statistics are merely a claim on admitted
    // external packages, so they must never decide sample validity while the
    // decoded payload is being checked.
    let Some(bits) = validity else {
        return Ok(false);
    };
    let byte = bits
        .get(sample_index / 8)
        .ok_or(PackageReadError::DecodedPayloadInvariant {
            component: "validity",
        })?;
    Ok(byte & (1_u8 << (sample_index & 7)) != 0)
}

fn scan_contiguous_payload_chunk(
    facts: &mut AuthoritativeFactsAccumulator,
    record: PackedIndexRecord,
    dtype: IntensityDType,
    validity: Option<&[u8]>,
    next_sample: &mut usize,
    bytes: &[u8],
) -> Result<(), PackageReadError> {
    let sample_bytes = usize::from(dtype.bytes_per_sample());
    if !bytes.len().is_multiple_of(sample_bytes) {
        return Err(PackageReadError::DecodedPayloadInvariant { component: "pixel" });
    }
    for sample in bytes.chunks_exact(sample_bytes) {
        let sample_valid = authoritative_sample_is_valid(record, validity, *next_sample)?;
        let (bits, numeric, is_fill) =
            decode_authoritative_sample(dtype, Some(sample), *next_sample, sample_valid)?;
        if sample_valid {
            facts.include_valid(bits, numeric, is_fill)?;
        }
        *next_sample = next_sample
            .checked_add(1)
            .ok_or(PackageReadError::AccountingOverflow {
                metric: "authoritative sample ordinal",
            })?;
    }
    Ok(())
}

fn finish_authoritative_payload_facts(
    facts: AuthoritativeFactsAccumulator,
    record: PackedIndexRecord,
    dtype: IntensityDType,
    logical_samples: u64,
) -> Result<LocalBrickPayloadFacts, PackageReadError> {
    let computed_range = facts
        .minimum
        .zip(facts.maximum)
        .map(|(minimum, maximum)| (minimum.0, maximum.0));
    let declared = record.statistics();
    if declared.valid_voxel_count() != facts.valid_samples
        || declared.nonfill_valid_voxel_count() != facts.nonfill_valid_samples
        || declared.numeric_range_bits() != computed_range
    {
        return Err(PackageReadError::PackedStatisticsMismatch);
    }
    let any_valid = facts.valid_samples != 0;
    let minimum_bits = if any_valid {
        native_range_to_f32_bits(
            dtype,
            computed_range.expect("a valid range was accumulated").0,
        )?
    } else {
        0
    };
    let maximum_bits = if any_valid {
        native_range_to_f32_bits(
            dtype,
            computed_range.expect("a valid range was accumulated").1,
        )?
    } else {
        0
    };
    Ok(LocalBrickPayloadFacts {
        minimum_bits,
        maximum_bits,
        any_valid,
        all_valid: facts.valid_samples == logical_samples,
    })
}

fn payload_facts_from_published_record(
    record: PackedIndexRecord,
    dtype: IntensityDType,
    logical_samples: u64,
) -> Result<LocalBrickPayloadFacts, PackageReadError> {
    let declared = record.statistics();
    let any_valid = declared.valid_voxel_count() != 0;
    let (minimum_bits, maximum_bits) = match declared.numeric_range_bits() {
        Some((minimum, maximum)) if any_valid => (
            native_range_to_f32_bits(dtype, minimum)?,
            native_range_to_f32_bits(dtype, maximum)?,
        ),
        None if !any_valid => (0, 0),
        _ => return Err(PackageReadError::PackedStatisticsMismatch),
    };
    if declared.valid_voxel_count() > logical_samples {
        return Err(PackageReadError::PackedStatisticsMismatch);
    }
    Ok(LocalBrickPayloadFacts {
        minimum_bits,
        maximum_bits,
        any_valid,
        all_valid: declared.valid_voxel_count() == logical_samples,
    })
}

fn compute_authoritative_payload_facts(
    record: PackedIndexRecord,
    dtype: IntensityDType,
    pixel_kind: ShardProfileKind,
    logical_extent: [u64; 3],
    pixel: Option<&[u8]>,
    validity: Option<&[u8]>,
) -> Result<LocalBrickPayloadFacts, PackageReadError> {
    let physical_shape = match pixel_kind {
        ShardProfileKind::Pixel3dUint8
        | ShardProfileKind::Pixel3dUint16
        | ShardProfileKind::Pixel3dFloat32 => [64_usize, 64, 64],
        ShardProfileKind::Pixel2dUint8
        | ShardProfileKind::Pixel2dUint16
        | ShardProfileKind::Pixel2dFloat32 => [1_usize, 256, 256],
        ShardProfileKind::Validity3d
        | ShardProfileKind::Validity2d
        | ShardProfileKind::PackedIndex => {
            return Err(PackageReadError::DecodedPayloadInvariant { component: "pixel" });
        }
    };
    let [logical_z, logical_y, logical_x] = logical_extent.map(|dimension| {
        usize::try_from(dimension).map_err(|_| PackageReadError::AccountingOverflow {
            metric: "logical brick extent",
        })
    });
    let [logical_z, logical_y, logical_x] = [logical_z?, logical_y?, logical_x?];
    if logical_z == 0
        || logical_y == 0
        || logical_x == 0
        || logical_z > physical_shape[0]
        || logical_y > physical_shape[1]
        || logical_x > physical_shape[2]
    {
        return Err(PackageReadError::DecodedPayloadInvariant { component: "pixel" });
    }

    let sample_bytes = usize::from(dtype.bytes_per_sample());
    let physical_samples = physical_shape.into_iter().product::<usize>();
    if pixel.is_some_and(|bytes| bytes.len() != physical_samples * sample_bytes) {
        return Err(PackageReadError::DecodedPayloadInvariant { component: "pixel" });
    }
    if validity.is_some_and(|bits| bits.len() != physical_samples.div_ceil(8)) {
        return Err(PackageReadError::DecodedPayloadInvariant {
            component: "validity",
        });
    }

    let mut facts = AuthoritativeFactsAccumulator::default();
    for z in 0..logical_z {
        for y in 0..logical_y {
            let row_start = (z * physical_shape[1] + y) * physical_shape[2];
            for x in 0..logical_x {
                let sample_index = row_start + x;
                let at = sample_index * sample_bytes;
                let sample = pixel
                    .map(|pixel| {
                        pixel
                            .get(at..at + sample_bytes)
                            .ok_or(PackageReadError::DecodedPayloadInvariant { component: "pixel" })
                    })
                    .transpose()?;
                let sample_valid = authoritative_sample_is_valid(record, validity, sample_index)?;
                let (bits, numeric, is_fill) =
                    decode_authoritative_sample(dtype, sample, sample_index, sample_valid)?;
                if sample_valid {
                    facts.include_valid(bits, numeric, is_fill)?;
                }
            }
        }
    }

    let logical_samples = u64::try_from(logical_z)
        .ok()
        .and_then(|z| z.checked_mul(u64::try_from(logical_y).ok()?))
        .and_then(|zy| zy.checked_mul(u64::try_from(logical_x).ok()?))
        .ok_or(PackageReadError::AccountingOverflow {
            metric: "logical brick samples",
        })?;
    finish_authoritative_payload_facts(facts, record, dtype, logical_samples)
}

fn native_range_to_f32_bits(dtype: IntensityDType, bits: u64) -> Result<u32, PackageReadError> {
    match dtype {
        IntensityDType::Uint8 => u8::try_from(bits)
            .map(f32::from)
            .map(f32::to_bits)
            .map_err(|_| PackageReadError::PackedStatisticsMismatch),
        IntensityDType::Uint16 => u16::try_from(bits)
            .map(f32::from)
            .map(f32::to_bits)
            .map_err(|_| PackageReadError::PackedStatisticsMismatch),
        IntensityDType::Float32 => {
            u32::try_from(bits).map_err(|_| PackageReadError::PackedStatisticsMismatch)
        }
    }
}

#[derive(Clone, Copy)]
struct ReadMetrics {
    range_requests: u8,
    encoded_bytes_read: u64,
    decoded_bytes: u64,
}

impl ReadMetrics {
    fn add(&mut self, other: Self) -> Result<(), PackageReadError> {
        self.range_requests = self
            .range_requests
            .checked_add(other.range_requests)
            .ok_or(PackageReadError::AccountingOverflow {
                metric: "range requests",
            })?;
        self.encoded_bytes_read = self
            .encoded_bytes_read
            .checked_add(other.encoded_bytes_read)
            .ok_or(PackageReadError::AccountingOverflow {
                metric: "encoded bytes read",
            })?;
        self.decoded_bytes = self.decoded_bytes.checked_add(other.decoded_bytes).ok_or(
            PackageReadError::AccountingOverflow {
                metric: "decoded bytes",
            },
        )?;
        Ok(())
    }
}

fn enforce_amplification(
    pixel_kind: ShardProfileKind,
    all_payloads_elided: bool,
    untrusted_validity_probe: bool,
    metrics: ReadMetrics,
) -> Result<(), PackageReadError> {
    if all_payloads_elided && !untrusted_validity_probe {
        check_limit(
            "cold range requests",
            u64::from(metrics.range_requests),
            u64::from(ELIDED_ALL_FILL_AMPLIFICATION.cold_range_requests_max),
        )?;
        check_limit(
            "read bytes",
            metrics.encoded_bytes_read,
            ELIDED_ALL_FILL_AMPLIFICATION.read_bytes_max,
        )?;
        return check_limit(
            "decoded bytes",
            metrics.decoded_bytes,
            ELIDED_ALL_FILL_AMPLIFICATION.decoded_bytes_max,
        );
    }
    let dtype = pixel_dtype(pixel_kind);
    let maximum = if is_two_dimensional(pixel_kind) {
        amplification_2d(dtype)
    } else {
        amplification_3d(dtype)
    };
    enforce_regular_amplification(metrics, maximum)
}

fn enforce_regular_amplification(
    metrics: ReadMetrics,
    maximum: OneBrickAmplification,
) -> Result<(), PackageReadError> {
    check_limit(
        "cold range requests",
        u64::from(metrics.range_requests),
        u64::from(maximum.cold_range_requests_max),
    )?;
    check_limit(
        "read bytes",
        metrics.encoded_bytes_read,
        maximum.read_bytes_max,
    )?;
    check_limit(
        "decoded bytes",
        metrics.decoded_bytes,
        maximum.decoded_bytes_max,
    )
}

fn check_limit(metric: &'static str, actual: u64, maximum: u64) -> Result<(), PackageReadError> {
    if actual > maximum {
        Err(PackageReadError::AmplificationExceeded {
            metric,
            actual,
            maximum,
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
) -> Result<&'a PackageObjectDescriptor, PackageReadError> {
    let descriptor = descriptors
        .binary_search_by(|descriptor| descriptor.path().cmp(path))
        .ok()
        .map(|index| &descriptors[index])
        .ok_or_else(|| PackageReadError::MissingRequiredShardDescriptor {
            component,
            path: path.to_string(),
        })?;
    if descriptor.kind() != expected {
        return Err(PackageReadError::DescriptorKindMismatch {
            path: path.to_string(),
            expected,
            actual: descriptor.kind(),
        });
    }
    Ok(descriptor)
}

fn map_chunk_error(path: &PackagePath, error: LocalShardChunkReadError) -> PackageReadError {
    match error {
        LocalShardChunkReadError::Range(error) => PackageReadError::Range(error),
        LocalShardChunkReadError::Shard(error) => PackageReadError::Shard(error),
        LocalShardChunkReadError::DeclaredLengthMismatch { expected, actual } => {
            PackageReadError::ObjectLengthMismatch {
                path: path.to_string(),
                expected,
                actual,
            }
        }
    }
}

fn pixel_dtype(kind: ShardProfileKind) -> IntensityDType {
    match kind {
        ShardProfileKind::Pixel3dUint8 | ShardProfileKind::Pixel2dUint8 => IntensityDType::Uint8,
        ShardProfileKind::Pixel3dUint16 | ShardProfileKind::Pixel2dUint16 => IntensityDType::Uint16,
        ShardProfileKind::Pixel3dFloat32 | ShardProfileKind::Pixel2dFloat32 => {
            IntensityDType::Float32
        }
        ShardProfileKind::Validity3d
        | ShardProfileKind::Validity2d
        | ShardProfileKind::PackedIndex => unreachable!("address plan contains a non-pixel kind"),
    }
}

const fn is_two_dimensional(kind: ShardProfileKind) -> bool {
    matches!(
        kind,
        ShardProfileKind::Pixel2dUint8
            | ShardProfileKind::Pixel2dUint16
            | ShardProfileKind::Pixel2dFloat32
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_record(
        dtype: IntensityDType,
        statistics: crate::PackedIndexStatistics,
        explicit_validity: bool,
    ) -> PackedIndexRecord {
        PackedIndexRecord::new(
            crate::PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0),
            statistics,
            true,
            explicit_validity,
            dtype,
            3,
        )
        .unwrap()
    }

    #[test]
    fn authoritative_physical_facts_cover_every_dtype_and_reject_lied_statistics() {
        let cases = [
            (
                IntensityDType::Uint8,
                ShardProfileKind::Pixel2dUint8,
                vec![2_u8, 0, 7],
                crate::PackedIndexStatistics::new(3, 2, Some((0, 7))),
                (0.0, 7.0),
            ),
            (
                IntensityDType::Uint16,
                ShardProfileKind::Pixel2dUint16,
                [2_u16, 0, u16::MAX]
                    .into_iter()
                    .flat_map(u16::to_le_bytes)
                    .collect(),
                crate::PackedIndexStatistics::new(3, 2, Some((0, u64::from(u16::MAX)))),
                (0.0, f32::from(u16::MAX)),
            ),
            (
                IntensityDType::Float32,
                ShardProfileKind::Pixel2dFloat32,
                [2.0_f32, -0.0, -3.0]
                    .into_iter()
                    .flat_map(f32::to_le_bytes)
                    .collect(),
                crate::PackedIndexStatistics::new(
                    3,
                    3,
                    Some((
                        u64::from((-3.0_f32).to_bits()),
                        u64::from(2.0_f32.to_bits()),
                    )),
                ),
                (-3.0, 2.0),
            ),
        ];
        for (dtype, kind, prefix, statistics, expected) in cases {
            let mut pixel = vec![0; kind.decoded_inner_bytes()];
            pixel[..prefix.len()].copy_from_slice(&prefix);
            let facts = compute_authoritative_payload_facts(
                test_record(dtype, statistics, false),
                dtype,
                kind,
                [1, 1, 3],
                Some(&pixel),
                None,
            )
            .unwrap();
            assert_eq!((facts.minimum(), facts.maximum()), expected);
            assert!(facts.any_valid());
            assert!(facts.all_valid());
        }

        let kind = ShardProfileKind::Pixel2dUint8;
        let mut pixel = vec![0; kind.decoded_inner_bytes()];
        pixel[..3].copy_from_slice(&[2, 0, 7]);
        let lied = test_record(
            IntensityDType::Uint8,
            crate::PackedIndexStatistics::new(3, 2, Some((0, 6))),
            false,
        );
        assert_eq!(
            compute_authoritative_payload_facts(
                lied,
                IntensityDType::Uint8,
                kind,
                [1, 1, 3],
                Some(&pixel),
                None,
            ),
            Err(PackageReadError::PackedStatisticsMismatch)
        );
    }

    #[test]
    fn authoritative_physical_facts_reject_valid_and_invalid_float_nan() {
        let kind = ShardProfileKind::Pixel2dFloat32;
        for valid_nan in [true, false] {
            let mut pixel = vec![0; kind.decoded_inner_bytes()];
            pixel[..4].copy_from_slice(&f32::NAN.to_le_bytes());
            let mut validity = vec![0; ShardProfileKind::Validity2d.decoded_inner_bytes()];
            validity[0] = if valid_nan { 0b0000_0111 } else { 0b0000_0110 };
            let valid_samples = if valid_nan { 3 } else { 2 };
            let statistics =
                crate::PackedIndexStatistics::new(valid_samples, 0, Some((0_u64, 0_u64)));
            let record = test_record(IntensityDType::Float32, statistics, true);
            assert_eq!(
                compute_authoritative_payload_facts(
                    record,
                    IntensityDType::Float32,
                    kind,
                    [1, 1, 3],
                    Some(&pixel),
                    Some(&validity),
                ),
                Err(PackageReadError::NonFinitePixelPayload {
                    sample_index: 0,
                    sample_valid: valid_nan,
                }),
                "valid_nan={valid_nan}"
            );
        }
    }

    #[test]
    fn authoritative_explicit_validity_uses_the_payload_or_its_canonical_zero_fill() {
        let kind = ShardProfileKind::Pixel2dUint8;
        let mut pixel = vec![0; kind.decoded_inner_bytes()];
        pixel[..3].copy_from_slice(&[2, 0, 7]);
        let declared_all_invalid = test_record(
            IntensityDType::Uint8,
            crate::PackedIndexStatistics::new(0, 0, None),
            true,
        );

        let all_invalid = compute_authoritative_payload_facts(
            declared_all_invalid,
            IntensityDType::Uint8,
            kind,
            [1, 1, 3],
            Some(&pixel),
            None,
        )
        .unwrap();
        assert!(!all_invalid.any_valid());
        assert!(!all_invalid.all_valid());

        let mut contradictory_validity =
            vec![0; ShardProfileKind::Validity2d.decoded_inner_bytes()];
        contradictory_validity[0] = 0b0000_0001;
        assert_eq!(
            compute_authoritative_payload_facts(
                declared_all_invalid,
                IntensityDType::Uint8,
                kind,
                [1, 1, 3],
                Some(&pixel),
                Some(&contradictory_validity),
            ),
            Err(PackageReadError::PackedStatisticsMismatch),
            "a zero-validity packed claim must not hide a physically present valid bit"
        );
    }

    #[test]
    fn amplification_checker_accepts_exact_limits_and_rejects_one_above() {
        let maximum = amplification_2d(IntensityDType::Uint8);
        let exact = ReadMetrics {
            range_requests: maximum.cold_range_requests_max,
            encoded_bytes_read: maximum.read_bytes_max,
            decoded_bytes: maximum.decoded_bytes_max,
        };
        assert_eq!(enforce_regular_amplification(exact, maximum), Ok(()));
        for (metric, above) in [
            (
                "cold range requests",
                ReadMetrics {
                    range_requests: maximum.cold_range_requests_max + 1,
                    ..exact
                },
            ),
            (
                "read bytes",
                ReadMetrics {
                    encoded_bytes_read: maximum.read_bytes_max + 1,
                    ..exact
                },
            ),
            (
                "decoded bytes",
                ReadMetrics {
                    decoded_bytes: maximum.decoded_bytes_max + 1,
                    ..exact
                },
            ),
        ] {
            assert!(matches!(
                enforce_regular_amplification(above, maximum),
                Err(PackageReadError::AmplificationExceeded {
                    metric: actual,
                    ..
                }) if actual == metric
            ));
        }

        let elided = ReadMetrics {
            range_requests: ELIDED_ALL_FILL_AMPLIFICATION.cold_range_requests_max,
            encoded_bytes_read: ELIDED_ALL_FILL_AMPLIFICATION.read_bytes_max,
            decoded_bytes: ELIDED_ALL_FILL_AMPLIFICATION.decoded_bytes_max,
        };
        assert_eq!(
            enforce_amplification(ShardProfileKind::Pixel2dUint8, true, false, elided),
            Ok(())
        );
        assert!(matches!(
            enforce_amplification(
                ShardProfileKind::Pixel2dUint8,
                true,
                false,
                ReadMetrics {
                    decoded_bytes: ELIDED_ALL_FILL_AMPLIFICATION.decoded_bytes_max + 1,
                    ..elided
                },
            ),
            Err(PackageReadError::AmplificationExceeded {
                metric: "decoded bytes",
                ..
            })
        ));
        assert_eq!(
            enforce_amplification(
                ShardProfileKind::Pixel2dUint8,
                true,
                true,
                ReadMetrics {
                    range_requests: ELIDED_ALL_FILL_AMPLIFICATION.cold_range_requests_max + 1,
                    encoded_bytes_read: elided.encoded_bytes_read,
                    decoded_bytes: elided.decoded_bytes,
                },
            ),
            Ok(()),
            "an untrusted fill probe uses the ordinary bounded-read budget"
        );
    }
}
