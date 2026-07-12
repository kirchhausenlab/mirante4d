use std::io::{Cursor, Read, Write};
use std::ops::Range;

use thiserror::Error;

const ZSTD_LEVEL: i32 = 3;
const CRC32C_BYTES: usize = 4;
const INDEX_ENTRY_BYTES: usize = 16;
const MISSING: u64 = u64::MAX;

/// One closed storage-profile row for indexed shard payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShardProfileKind {
    Pixel3dUint8,
    Pixel3dUint16,
    Pixel3dFloat32,
    Pixel2dUint8,
    Pixel2dUint16,
    Pixel2dFloat32,
    Validity3d,
    Validity2d,
    PackedIndex,
}

impl ShardProfileKind {
    pub const fn chunks_per_shard(self) -> usize {
        match self {
            Self::Pixel3dUint8
            | Self::Pixel3dUint16
            | Self::Pixel3dFloat32
            | Self::Validity3d
            | Self::PackedIndex => 64,
            Self::Pixel2dUint8 | Self::Pixel2dUint16 | Self::Pixel2dFloat32 | Self::Validity2d => {
                16
            }
        }
    }

    pub const fn decoded_inner_bytes(self) -> usize {
        match self {
            Self::Pixel3dUint8 => 262_144,
            Self::Pixel3dUint16 => 524_288,
            Self::Pixel3dFloat32 => 1_048_576,
            Self::Pixel2dUint8 => 65_536,
            Self::Pixel2dUint16 => 131_072,
            Self::Pixel2dFloat32 => 262_144,
            Self::Validity3d => 32_768,
            Self::Validity2d => 8_192,
            Self::PackedIndex => 16_384,
        }
    }

    /// Maximum encoded inner-payload bytes, including the CRC32C trailer.
    pub const fn encoded_inner_bytes_max(self) -> usize {
        match self {
            Self::Pixel3dUint8 => 327_680,
            Self::Pixel3dUint16 => 655_360,
            Self::Pixel3dFloat32 => 1_310_720,
            Self::Pixel2dUint8 => 81_920,
            Self::Pixel2dUint16 => 163_840,
            Self::Pixel2dFloat32 => 327_680,
            Self::Validity3d => 40_960,
            Self::Validity2d => 10_240,
            Self::PackedIndex => 20_480,
        }
    }

    pub const fn decoded_outer_bytes(self) -> usize {
        self.decoded_inner_bytes() * self.chunks_per_shard()
    }

    /// Maximum complete shard bytes, including the fixed end index.
    pub const fn encoded_shard_bytes_max(self) -> usize {
        match self {
            Self::Pixel3dUint8 => 20_975_616,
            Self::Pixel3dUint16 => 41_947_136,
            Self::Pixel3dFloat32 => 83_890_176,
            Self::Pixel2dUint8 => 1_314_816,
            Self::Pixel2dUint16 => 2_625_536,
            Self::Pixel2dFloat32 => 5_246_976,
            Self::Validity3d => 2_625_536,
            Self::Validity2d => 167_936,
            Self::PackedIndex => 1_314_816,
        }
    }

    pub const fn index_tail_bytes(self) -> usize {
        self.chunks_per_shard() * INDEX_ENTRY_BYTES + CRC32C_BYTES
    }
}

/// A present inner chunk's byte range within the shard payload prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShardIndexEntry {
    offset: u64,
    nbytes: u64,
}

impl ShardIndexEntry {
    pub const fn offset(self) -> u64 {
        self.offset
    }

    pub const fn nbytes(self) -> u64 {
        self.nbytes
    }

    pub const fn range(self) -> Range<u64> {
        self.offset..self.offset + self.nbytes
    }
}

/// A checked fixed-size tail index, ready to drive later range reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShardIndex {
    entries: Vec<Option<ShardIndexEntry>>,
    payload_bytes: u64,
}

impl ShardIndex {
    /// Returns the structural index state for one slot.
    ///
    /// `None` means only that the Zarr missing sentinel was present. Callers
    /// must cross-check packed-index and array facts before treating it as an
    /// authorized fill elision.
    pub fn entry(&self, chunk_index: usize) -> Result<Option<ShardIndexEntry>, ShardCodecError> {
        self.entries
            .get(chunk_index)
            .copied()
            .ok_or(ShardCodecError::ChunkIndexOutOfBounds {
                chunk_index,
                chunk_count: self.entries.len(),
            })
    }

    /// Requires a payload after higher-level facts have established that the
    /// slot is occupied or otherwise cannot be elided.
    pub fn require_entry(&self, chunk_index: usize) -> Result<ShardIndexEntry, ShardCodecError> {
        self.entry(chunk_index)?
            .ok_or(ShardCodecError::MissingRequiredPayload { chunk_index })
    }

    pub const fn payload_bytes(&self) -> u64 {
        self.payload_bytes
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ShardCodecError {
    #[error("expected {expected} chunk slots, got {actual}")]
    ChunkCount { expected: usize, actual: usize },
    #[error("chunk {chunk_index} is outside the {chunk_count}-chunk shard index")]
    ChunkIndexOutOfBounds {
        chunk_index: usize,
        chunk_count: usize,
    },
    #[error("required chunk {chunk_index} is represented by the missing sentinel")]
    MissingRequiredPayload { chunk_index: usize },
    #[error("decoded inner payload must be exactly {expected} bytes, got {actual}")]
    DecodedInnerLength { expected: usize, actual: usize },
    #[error("encoded inner payload exceeds {limit} bytes: {actual}")]
    EncodedInnerTooLarge { limit: usize, actual: usize },
    #[error("encoded inner payload is too short to contain a CRC32C trailer")]
    EncodedInnerTooShort,
    #[error("inner payload CRC32C mismatch")]
    InnerChecksumMismatch,
    #[error("zstd {operation} failed: {message}")]
    Zstd {
        operation: &'static str,
        message: String,
    },
    #[error("index tail must be exactly {expected} bytes, got {actual}")]
    IndexTailLength { expected: usize, actual: usize },
    #[error("index tail CRC32C mismatch")]
    IndexChecksumMismatch,
    #[error("chunk {chunk_index} has only one half of the missing sentinel pair")]
    InvalidMissingPair { chunk_index: usize },
    #[error("chunk {chunk_index} has a zero-byte present index entry")]
    ZeroLengthEntry { chunk_index: usize },
    #[error("chunk {chunk_index} encoded range exceeds {limit} bytes: {actual}")]
    IndexEntryTooLarge {
        chunk_index: usize,
        limit: u64,
        actual: u64,
    },
    #[error("chunk {chunk_index} index range overflows u64")]
    IndexRangeOverflow { chunk_index: usize },
    #[error(
        "chunk {chunk_index} range ends at {end}, beyond the {payload_bytes}-byte payload prefix"
    )]
    IndexRangeOutOfBounds {
        chunk_index: usize,
        end: u64,
        payload_bytes: u64,
    },
    #[error(
        "chunk {chunk_index} has noncanonical offset {actual}; lexicographic zero-slack offset is {expected}"
    )]
    NonCanonicalIndexOffset {
        chunk_index: usize,
        expected: u64,
        actual: u64,
    },
    #[error("index covers {covered} payload bytes, but the payload prefix has {payload_bytes}")]
    TrailingShardSlack { covered: u64, payload_bytes: u64 },
    #[error("complete shard exceeds {limit} bytes: {actual}")]
    EncodedShardTooLarge { limit: u64, actual: u64 },
    #[error("shard byte count cannot be represented as u64")]
    LengthOverflow,
}

pub fn encode_inner_payload(
    kind: ShardProfileKind,
    decoded: &[u8],
) -> Result<Vec<u8>, ShardCodecError> {
    let expected = kind.decoded_inner_bytes();
    if decoded.len() != expected {
        return Err(ShardCodecError::DecodedInnerLength {
            expected,
            actual: decoded.len(),
        });
    }

    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), ZSTD_LEVEL)
        .map_err(|error| zstd_error("encode", error))?;
    encoder
        .include_checksum(false)
        .map_err(|error| zstd_error("encode", error))?;
    encoder
        .set_pledged_src_size(Some(
            u64::try_from(decoded.len()).map_err(|_| ShardCodecError::LengthOverflow)?,
        ))
        .map_err(|error| zstd_error("encode", error))?;
    encoder
        .write_all(decoded)
        .map_err(|error| zstd_error("encode", error))?;
    let mut encoded = encoder
        .finish()
        .map_err(|error| zstd_error("encode", error))?;
    let checksum = crc32c::crc32c(&encoded);
    encoded.extend_from_slice(&checksum.to_le_bytes());

    let limit = kind.encoded_inner_bytes_max();
    if encoded.len() > limit {
        return Err(ShardCodecError::EncodedInnerTooLarge {
            limit,
            actual: encoded.len(),
        });
    }
    Ok(encoded)
}

pub fn decode_inner_payload(
    kind: ShardProfileKind,
    encoded: &[u8],
) -> Result<Vec<u8>, ShardCodecError> {
    let limit = kind.encoded_inner_bytes_max();
    if encoded.len() > limit {
        return Err(ShardCodecError::EncodedInnerTooLarge {
            limit,
            actual: encoded.len(),
        });
    }
    if encoded.len() < CRC32C_BYTES {
        return Err(ShardCodecError::EncodedInnerTooShort);
    }

    let checksum_at = encoded.len() - CRC32C_BYTES;
    let (compressed, checksum_bytes) = encoded.split_at(checksum_at);
    let expected_checksum = u32::from_le_bytes(checksum_bytes.try_into().expect("four bytes"));
    if crc32c::crc32c(compressed) != expected_checksum {
        return Err(ShardCodecError::InnerChecksumMismatch);
    }

    let expected = kind.decoded_inner_bytes();
    let mut decoder = zstd::stream::read::Decoder::new(Cursor::new(compressed))
        .map_err(|error| zstd_error("decode", error))?;
    decoder
        .window_log_max(window_log_max(expected))
        .map_err(|error| zstd_error("decode", error))?;
    let output_limit = u64::try_from(expected)
        .map_err(|_| ShardCodecError::LengthOverflow)?
        .checked_add(1)
        .ok_or(ShardCodecError::LengthOverflow)?;
    let mut decoded = Vec::with_capacity(expected);
    decoder
        .take(output_limit)
        .read_to_end(&mut decoded)
        .map_err(|error| zstd_error("decode", error))?;
    if decoded.len() != expected {
        return Err(ShardCodecError::DecodedInnerLength {
            expected,
            actual: decoded.len(),
        });
    }
    Ok(decoded)
}

/// Decode and structurally validate an exact fixed end-index tail.
///
/// `payload_bytes` is the length of the shard prefix before this tail. It is
/// sufficient to prove range bounds, lexicographic order, non-overlap, and the
/// profile's zero-slack rule without reading payload bytes. A missing sentinel
/// remains structurally raw until package facts authorize fill elision.
pub fn decode_shard_index_tail(
    kind: ShardProfileKind,
    tail: &[u8],
    payload_bytes: u64,
) -> Result<ShardIndex, ShardCodecError> {
    let expected = kind.index_tail_bytes();
    if tail.len() != expected {
        return Err(ShardCodecError::IndexTailLength {
            expected,
            actual: tail.len(),
        });
    }

    let tail_bytes = u64::try_from(tail.len()).map_err(|_| ShardCodecError::LengthOverflow)?;
    let complete_bytes = payload_bytes
        .checked_add(tail_bytes)
        .ok_or(ShardCodecError::LengthOverflow)?;
    let shard_limit = u64::try_from(kind.encoded_shard_bytes_max())
        .map_err(|_| ShardCodecError::LengthOverflow)?;
    if complete_bytes > shard_limit {
        return Err(ShardCodecError::EncodedShardTooLarge {
            limit: shard_limit,
            actual: complete_bytes,
        });
    }

    let checksum_at = tail.len() - CRC32C_BYTES;
    let (index_bytes, checksum_bytes) = tail.split_at(checksum_at);
    let expected_checksum = u32::from_le_bytes(checksum_bytes.try_into().expect("four bytes"));
    if crc32c::crc32c(index_bytes) != expected_checksum {
        return Err(ShardCodecError::IndexChecksumMismatch);
    }

    let mut entries = Vec::with_capacity(kind.chunks_per_shard());
    let mut next_offset = 0_u64;
    for (chunk_index, pair) in index_bytes.chunks_exact(INDEX_ENTRY_BYTES).enumerate() {
        let offset = u64::from_le_bytes(pair[..8].try_into().expect("eight bytes"));
        let nbytes = u64::from_le_bytes(pair[8..].try_into().expect("eight bytes"));
        match (offset == MISSING, nbytes == MISSING) {
            (true, true) => entries.push(None),
            (true, false) | (false, true) => {
                return Err(ShardCodecError::InvalidMissingPair { chunk_index });
            }
            (false, false) => {
                if nbytes == 0 {
                    return Err(ShardCodecError::ZeroLengthEntry { chunk_index });
                }
                let inner_limit = u64::try_from(kind.encoded_inner_bytes_max())
                    .map_err(|_| ShardCodecError::LengthOverflow)?;
                if nbytes > inner_limit {
                    return Err(ShardCodecError::IndexEntryTooLarge {
                        chunk_index,
                        limit: inner_limit,
                        actual: nbytes,
                    });
                }
                let end = offset
                    .checked_add(nbytes)
                    .ok_or(ShardCodecError::IndexRangeOverflow { chunk_index })?;
                if end > payload_bytes {
                    return Err(ShardCodecError::IndexRangeOutOfBounds {
                        chunk_index,
                        end,
                        payload_bytes,
                    });
                }
                if offset != next_offset {
                    return Err(ShardCodecError::NonCanonicalIndexOffset {
                        chunk_index,
                        expected: next_offset,
                        actual: offset,
                    });
                }
                entries.push(Some(ShardIndexEntry { offset, nbytes }));
                next_offset = end;
            }
        }
    }
    if next_offset != payload_bytes {
        return Err(ShardCodecError::TrailingShardSlack {
            covered: next_offset,
            payload_bytes,
        });
    }
    Ok(ShardIndex {
        entries,
        payload_bytes,
    })
}

/// Internal deterministic assembler. The package writer will supply missing
/// slots only after semantic fill-elision validation exists.
#[cfg(test)]
pub(crate) fn assemble_shard(
    kind: ShardProfileKind,
    decoded_chunks: &[Option<&[u8]>],
) -> Result<Vec<u8>, ShardCodecError> {
    let expected = kind.chunks_per_shard();
    if decoded_chunks.len() != expected {
        return Err(ShardCodecError::ChunkCount {
            expected,
            actual: decoded_chunks.len(),
        });
    }

    let mut shard = Vec::new();
    let mut pairs = Vec::with_capacity(expected);
    for decoded in decoded_chunks {
        if let Some(decoded) = decoded {
            let offset = u64::try_from(shard.len()).map_err(|_| ShardCodecError::LengthOverflow)?;
            let encoded = encode_inner_payload(kind, decoded)?;
            let nbytes =
                u64::try_from(encoded.len()).map_err(|_| ShardCodecError::LengthOverflow)?;
            shard.extend_from_slice(&encoded);
            pairs.push((offset, nbytes));
        } else {
            pairs.push((MISSING, MISSING));
        }
    }

    let mut index_bytes = Vec::with_capacity(expected * INDEX_ENTRY_BYTES);
    for (offset, nbytes) in pairs {
        index_bytes.extend_from_slice(&offset.to_le_bytes());
        index_bytes.extend_from_slice(&nbytes.to_le_bytes());
    }
    let checksum = crc32c::crc32c(&index_bytes);
    shard.extend_from_slice(&index_bytes);
    shard.extend_from_slice(&checksum.to_le_bytes());

    let limit = kind.encoded_shard_bytes_max();
    if shard.len() > limit {
        return Err(ShardCodecError::EncodedShardTooLarge {
            limit: u64::try_from(limit).map_err(|_| ShardCodecError::LengthOverflow)?,
            actual: u64::try_from(shard.len()).map_err(|_| ShardCodecError::LengthOverflow)?,
        });
    }
    Ok(shard)
}

fn zstd_error(operation: &'static str, error: std::io::Error) -> ShardCodecError {
    ShardCodecError::Zstd {
        operation,
        message: error.to_string(),
    }
}

fn window_log_max(decoded_bytes: usize) -> u32 {
    let log = usize::BITS - decoded_bytes.saturating_sub(1).leading_zeros();
    log.max(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(kind: ShardProfileKind, shard: &[u8]) -> (&[u8], u64) {
        let tail_bytes = kind.index_tail_bytes();
        let payload_bytes = shard.len() - tail_bytes;
        (
            &shard[payload_bytes..],
            u64::try_from(payload_bytes).unwrap(),
        )
    }

    fn crc_protected_zstd(bytes: &[u8]) -> Vec<u8> {
        let mut encoded = zstd::stream::encode_all(Cursor::new(bytes), ZSTD_LEVEL).unwrap();
        encoded.extend_from_slice(&crc32c::crc32c(&encoded).to_le_bytes());
        encoded
    }

    #[test]
    fn profile_rows_fix_chunk_counts_and_tail_lengths() {
        let rows = [
            (
                ShardProfileKind::Pixel3dUint8,
                64,
                262_144,
                327_680,
                20_975_616,
                1_028,
            ),
            (
                ShardProfileKind::Pixel3dUint16,
                64,
                524_288,
                655_360,
                41_947_136,
                1_028,
            ),
            (
                ShardProfileKind::Pixel3dFloat32,
                64,
                1_048_576,
                1_310_720,
                83_890_176,
                1_028,
            ),
            (
                ShardProfileKind::Pixel2dUint8,
                16,
                65_536,
                81_920,
                1_314_816,
                260,
            ),
            (
                ShardProfileKind::Pixel2dUint16,
                16,
                131_072,
                163_840,
                2_625_536,
                260,
            ),
            (
                ShardProfileKind::Pixel2dFloat32,
                16,
                262_144,
                327_680,
                5_246_976,
                260,
            ),
            (
                ShardProfileKind::Validity3d,
                64,
                32_768,
                40_960,
                2_625_536,
                1_028,
            ),
            (
                ShardProfileKind::Validity2d,
                16,
                8_192,
                10_240,
                167_936,
                260,
            ),
            (
                ShardProfileKind::PackedIndex,
                64,
                16_384,
                20_480,
                1_314_816,
                1_028,
            ),
        ];
        for (kind, chunks, decoded_inner, encoded_inner, encoded_shard, index_tail) in rows {
            assert_eq!(kind.chunks_per_shard(), chunks);
            assert_eq!(kind.decoded_inner_bytes(), decoded_inner);
            assert_eq!(kind.encoded_inner_bytes_max(), encoded_inner);
            assert_eq!(kind.decoded_outer_bytes(), decoded_inner * chunks);
            assert_eq!(kind.encoded_shard_bytes_max(), encoded_shard);
            assert_eq!(kind.index_tail_bytes(), index_tail);
        }
    }

    #[test]
    fn inner_payload_round_trips_and_corruption_is_rejected() {
        let kind = ShardProfileKind::Validity2d;
        let decoded = vec![0x5a; kind.decoded_inner_bytes()];
        let encoded = encode_inner_payload(kind, &decoded).unwrap();
        assert_eq!(decode_inner_payload(kind, &encoded).unwrap(), decoded);

        let mut corrupt = encoded;
        corrupt[0] ^= 1;
        assert_eq!(
            decode_inner_payload(kind, &corrupt),
            Err(ShardCodecError::InnerChecksumMismatch)
        );
    }

    #[test]
    fn inner_decode_rejects_bombs_and_encoded_oversize_before_decompression() {
        let kind = ShardProfileKind::Validity2d;
        let bomb = crc_protected_zstd(&vec![0; kind.decoded_inner_bytes() + 1]);
        assert!(matches!(
            decode_inner_payload(kind, &bomb),
            Err(ShardCodecError::Zstd {
                operation: "decode",
                ..
            }) | Err(ShardCodecError::DecodedInnerLength { .. })
        ));

        let oversize = vec![0; kind.encoded_inner_bytes_max() + 1];
        assert_eq!(
            decode_inner_payload(kind, &oversize),
            Err(ShardCodecError::EncodedInnerTooLarge {
                limit: kind.encoded_inner_bytes_max(),
                actual: kind.encoded_inner_bytes_max() + 1,
            })
        );
    }

    #[test]
    fn deterministic_shard_tail_drives_present_and_missing_range_reads() {
        let kind = ShardProfileKind::Validity2d;
        let decoded = vec![7; kind.decoded_inner_bytes()];
        let mut chunks = vec![None; kind.chunks_per_shard()];
        chunks[0] = Some(decoded.as_slice());
        chunks[3] = Some(decoded.as_slice());

        let first = assemble_shard(kind, &chunks).unwrap();
        let second = assemble_shard(kind, &chunks).unwrap();
        assert_eq!(first, second);
        let (tail, payload_bytes) = tail(kind, &first);
        let index = decode_shard_index_tail(kind, tail, payload_bytes).unwrap();
        assert_eq!(index.payload_bytes(), payload_bytes);
        assert_eq!(index.require_entry(0).unwrap().offset(), 0);
        assert_eq!(
            index.require_entry(0).unwrap().range().end,
            index.require_entry(3).unwrap().offset()
        );
        assert_eq!(
            index.require_entry(1),
            Err(ShardCodecError::MissingRequiredPayload { chunk_index: 1 })
        );
        assert_eq!(tail.len(), 260);
    }

    #[test]
    fn tail_validation_rejects_checksum_pairs_bounds_and_slack() {
        let kind = ShardProfileKind::Pixel2dUint8;
        let decoded = vec![1; kind.decoded_inner_bytes()];
        let mut chunks = vec![None; kind.chunks_per_shard()];
        chunks[0] = Some(decoded.as_slice());
        chunks[1] = Some(decoded.as_slice());
        let shard = assemble_shard(kind, &chunks).unwrap();
        let (valid_tail, payload_bytes) = tail(kind, &shard);

        let mut corrupt = valid_tail.to_vec();
        corrupt[0] ^= 1;
        assert_eq!(
            decode_shard_index_tail(kind, &corrupt, payload_bytes),
            Err(ShardCodecError::IndexChecksumMismatch)
        );

        let mut mixed = valid_tail.to_vec();
        mixed[2 * INDEX_ENTRY_BYTES..2 * INDEX_ENTRY_BYTES + 8]
            .copy_from_slice(&0_u64.to_le_bytes());
        let crc_at = mixed.len() - CRC32C_BYTES;
        let crc = crc32c::crc32c(&mixed[..crc_at]);
        mixed[crc_at..].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            decode_shard_index_tail(kind, &mixed, payload_bytes),
            Err(ShardCodecError::InvalidMissingPair { chunk_index: 2 })
        );

        let mut out_of_bounds = valid_tail.to_vec();
        out_of_bounds[8..16].copy_from_slice(&(payload_bytes + 1).to_le_bytes());
        let crc_at = out_of_bounds.len() - CRC32C_BYTES;
        let crc = crc32c::crc32c(&out_of_bounds[..crc_at]);
        out_of_bounds[crc_at..].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(
            decode_shard_index_tail(kind, &out_of_bounds, payload_bytes),
            Err(ShardCodecError::IndexRangeOutOfBounds { chunk_index: 0, .. })
        ));

        let mut gap = valid_tail.to_vec();
        let first_nbytes = u64::from_le_bytes(gap[8..16].try_into().unwrap());
        gap[INDEX_ENTRY_BYTES..INDEX_ENTRY_BYTES + 8]
            .copy_from_slice(&(first_nbytes + 1).to_le_bytes());
        let crc_at = gap.len() - CRC32C_BYTES;
        let crc = crc32c::crc32c(&gap[..crc_at]);
        gap[crc_at..].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            decode_shard_index_tail(kind, &gap, payload_bytes + 1),
            Err(ShardCodecError::NonCanonicalIndexOffset {
                chunk_index: 1,
                expected: first_nbytes,
                actual: first_nbytes + 1,
            })
        );

        let empty_chunks = vec![None; kind.chunks_per_shard()];
        let empty_shard = assemble_shard(kind, &empty_chunks).unwrap();
        let (empty_tail, _) = tail(kind, &empty_shard);
        let complete_oversize_payload = u64::try_from(kind.encoded_shard_bytes_max()).unwrap();
        assert!(matches!(
            decode_shard_index_tail(kind, empty_tail, complete_oversize_payload),
            Err(ShardCodecError::EncodedShardTooLarge { .. })
        ));

        let mut oversized_entry = empty_tail.to_vec();
        let inner_oversize = u64::try_from(kind.encoded_inner_bytes_max()).unwrap() + 1;
        oversized_entry[..8].copy_from_slice(&0_u64.to_le_bytes());
        oversized_entry[8..16].copy_from_slice(&inner_oversize.to_le_bytes());
        let crc_at = oversized_entry.len() - CRC32C_BYTES;
        let crc = crc32c::crc32c(&oversized_entry[..crc_at]);
        oversized_entry[crc_at..].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            decode_shard_index_tail(kind, &oversized_entry, inner_oversize),
            Err(ShardCodecError::IndexEntryTooLarge {
                chunk_index: 0,
                limit: u64::try_from(kind.encoded_inner_bytes_max()).unwrap(),
                actual: inner_oversize,
            })
        );
    }
}
