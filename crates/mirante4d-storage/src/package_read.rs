use mirante4d_domain::IntensityDType;
use thiserror::Error;

use crate::range_io::{LocalObjectSnapshot, LocalShardChunkBytes, LocalShardChunkReadError};
use crate::{
    BrickAddressError, ELIDED_ALL_FILL_AMPLIFICATION, LocalBrickAddressPlan, LocalPackageReader,
    OneBrickAmplification, PackageObjectDescriptor, PackageObjectKind, PackagePath,
    PackedIndexError, PackedIndexRecord, RangeReadError, ShardCodecError, ShardProfileKind,
    amplification_2d, amplification_3d, decode_inner_payload,
};

/// CRC-checked storage payloads and packed facts for one logical brick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalBrickRead {
    record: PackedIndexRecord,
    logical_extent_zyx: [u64; 3],
    pixel_payload: Option<Vec<u8>>,
    validity_payload: Option<Vec<u8>>,
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

    pub const fn range_requests(&self) -> u8 {
        self.range_requests
    }

    pub const fn encoded_bytes_read(&self) -> u64 {
        self.encoded_bytes_read
    }

    pub const fn decoded_bytes(&self) -> u64 {
        self.decoded_bytes
    }

    pub(crate) fn object_snapshots(&self) -> &[LocalObjectSnapshot] {
        &self.object_snapshots
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
    #[error("{metric} accounting overflowed")]
    AccountingOverflow { metric: &'static str },
    #[error("one-brick {metric} is {actual}; maximum is {maximum}")]
    AmplificationExceeded {
        metric: &'static str,
        actual: u64,
        maximum: u64,
    },
}

pub(crate) fn read_local_brick(
    reader: &LocalPackageReader,
    descriptors: &[PackageObjectDescriptor],
    plan: LocalBrickAddressPlan,
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
    let record_bytes = packed_payload.get(record_offset..record_end).ok_or(
        PackageReadError::PackedRecordOutOfBounds {
            offset: plan.packed_index_record_byte_offset(),
        },
    )?;
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
                })?,
        )
    } else {
        None
    };

    let validity_payload = if explicit_validity && record.statistics().valid_voxel_count() > 0 {
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
        let validity = read_component(reader, descriptor, kind, chunk_index)?;
        metrics.add(validity.metrics)?;
        used_objects.push(validity.snapshot);
        Some(
            validity
                .payload
                .ok_or_else(|| PackageReadError::MissingRequiredInnerPayload {
                    component: "validity",
                    path: path.to_string(),
                    chunk_index: usize::try_from(chunk_index).unwrap_or(usize::MAX),
                })?,
        )
    } else {
        None
    };

    enforce_amplification(
        plan.pixel_kind(),
        pixel_payload.is_none() && validity_payload.is_none(),
        metrics,
    )?;
    reader.revalidate_snapshots(&used_objects)?;
    Ok(LocalBrickRead {
        record,
        logical_extent_zyx: plan.logical_extent_zyx(),
        pixel_payload,
        validity_payload,
        range_requests: metrics.range_requests,
        encoded_bytes_read: metrics.encoded_bytes_read,
        decoded_bytes: metrics.decoded_bytes,
        object_snapshots: used_objects,
    })
}

struct DecodedComponent {
    payload: Option<Vec<u8>>,
    metrics: ReadMetrics,
    snapshot: LocalObjectSnapshot,
}

fn read_component(
    reader: &LocalPackageReader,
    descriptor: &PackageObjectDescriptor,
    kind: ShardProfileKind,
    chunk_index: u64,
) -> Result<DecodedComponent, PackageReadError> {
    let chunk_index = usize::try_from(chunk_index).map_err(|_| ShardCodecError::LengthOverflow)?;
    let raw = reader
        .read_shard_chunk(
            descriptor.path(),
            kind,
            chunk_index,
            descriptor.raw().byte_length(),
        )
        .map_err(|error| map_chunk_error(descriptor.path(), error))?;
    decode_component(kind, raw)
}

fn decode_component(
    kind: ShardProfileKind,
    raw: LocalShardChunkBytes,
) -> Result<DecodedComponent, PackageReadError> {
    let payload = raw
        .encoded
        .as_deref()
        .map(|encoded| decode_inner_payload(kind, encoded))
        .transpose()?;
    let payload_bytes = payload.as_ref().map_or(0, Vec::len);
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
    metrics: ReadMetrics,
) -> Result<(), PackageReadError> {
    if all_payloads_elided {
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
            enforce_amplification(ShardProfileKind::Pixel2dUint8, true, elided),
            Ok(())
        );
        assert!(matches!(
            enforce_amplification(
                ShardProfileKind::Pixel2dUint8,
                true,
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
    }
}
