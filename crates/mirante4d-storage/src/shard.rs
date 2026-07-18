use std::io::{Cursor, Read, Write};
use std::ops::Range;

use mirante4d_dataset::{DecodeSinkError, ReservedDecodeSink};
use thiserror::Error;

const ZSTD_LEVEL: i32 = 3;
const ZSTD_FRAME_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
const ZSTD_FRAME_DESCRIPTOR_OFFSET: usize = ZSTD_FRAME_MAGIC.len();
const ZSTD_FRAME_DICTIONARY_ID_FLAG_MASK: u8 = 0b0000_0011;
const ZSTD_FRAME_CONTENT_CHECKSUM_FLAG: u8 = 0b0000_0100;
const ZSTD_FRAME_RESERVED_FLAG_MASK: u8 = 0b0001_1000;
/// Conservative native Zstd context/workspace authority charged by every
/// importer or validator that can execute an inner-payload codec call.
pub const INNER_CODEC_WORKING_BYTES_MAX: u64 = 8 * 1024 * 1024;
const CRC32C_BYTES: usize = 4;
const INDEX_ENTRY_BYTES: usize = 16;
const MISSING: u64 = u64::MAX;
pub(crate) const DIRECT_DECODE_SPAN_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub(crate) enum DirectInnerDecodeError<E> {
    Shard(ShardCodecError),
    Sink(DecodeSinkError),
    Observer(E),
}

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
    #[error("encoded inner Zstandard frame violates the storage profile: {reason}")]
    ZstdFrameProfile { reason: &'static str },
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
    let compressed = validate_encoded_inner_envelope(kind, encoded)?;

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

/// Decodes one validated inner frame directly into the caller-owned reserved
/// sink in bounded spans. `observe` sees each span after decode and before its
/// explicit commit; verified delivery uses a no-op observer while provisional
/// delivery derives facts from the bytes in their final allocation. No
/// decoded payload-sized staging allocation or post-decode copy is involved.
pub(crate) fn decode_inner_payload_direct<E>(
    kind: ShardProfileKind,
    encoded: &[u8],
    sink: &mut dyn ReservedDecodeSink,
    mut observe: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), DirectInnerDecodeError<E>> {
    let compressed =
        validate_encoded_inner_envelope(kind, encoded).map_err(DirectInnerDecodeError::Shard)?;
    let expected = kind.decoded_inner_bytes();
    let mut decoder = zstd::stream::read::Decoder::new(Cursor::new(compressed))
        .map_err(|error| DirectInnerDecodeError::Shard(zstd_error("decode", error)))?;
    decoder
        .window_log_max(window_log_max(expected))
        .map_err(|error| DirectInnerDecodeError::Shard(zstd_error("decode", error)))?;
    let mut decoded = 0_usize;
    while decoded != expected {
        let requested = (expected - decoded).min(DIRECT_DECODE_SPAN_BYTES);
        {
            let output = sink
                .writable_span(requested)
                .map_err(DirectInnerDecodeError::Sink)?;
            if output.len() != requested {
                return Err(DirectInnerDecodeError::Sink(
                    DecodeSinkError::WritableCommitExceeded {
                        offered: output.len(),
                        attempted: requested,
                    },
                ));
            }
            decoder
                .read_exact(output)
                .map_err(|error| DirectInnerDecodeError::Shard(zstd_error("decode", error)))?;
            observe(output).map_err(DirectInnerDecodeError::Observer)?;
        }
        sink.commit_written(requested)
            .map_err(DirectInnerDecodeError::Sink)?;
        decoded += requested;
    }
    let mut extra = [0_u8; 1];
    let extra_bytes = decoder
        .read(&mut extra)
        .map_err(|error| DirectInnerDecodeError::Shard(zstd_error("decode", error)))?;
    if extra_bytes != 0 {
        return Err(DirectInnerDecodeError::Shard(
            ShardCodecError::DecodedInnerLength {
                expected,
                actual: expected.saturating_add(extra_bytes),
            },
        ));
    }
    Ok(())
}

fn validate_encoded_inner_envelope(
    kind: ShardProfileKind,
    encoded: &[u8],
) -> Result<&[u8], ShardCodecError> {
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

    validate_zstd_frame_profile(compressed)?;
    let frame_bytes = zstd::zstd_safe::find_frame_compressed_size(compressed).map_err(|code| {
        ShardCodecError::Zstd {
            operation: "validate encoded frame",
            message: code.to_string(),
        }
    })?;
    if frame_bytes != compressed.len() {
        return Err(ShardCodecError::Zstd {
            operation: "validate encoded frame",
            message: "inner payload is not exactly one Zstandard frame".to_owned(),
        });
    }
    let content_bytes = zstd::zstd_safe::get_frame_content_size(compressed)
        .map_err(|error| ShardCodecError::Zstd {
            operation: "validate encoded frame content size",
            message: error.to_string(),
        })?
        .ok_or(ShardCodecError::ZstdFrameProfile {
            reason: "the frame must declare its decoded content size",
        })?;
    let expected =
        u64::try_from(kind.decoded_inner_bytes()).map_err(|_| ShardCodecError::LengthOverflow)?;
    if content_bytes != expected {
        return Err(ShardCodecError::DecodedInnerLength {
            expected: kind.decoded_inner_bytes(),
            actual: usize::try_from(content_bytes).unwrap_or(usize::MAX),
        });
    }
    Ok(compressed)
}

fn validate_zstd_frame_profile(compressed: &[u8]) -> Result<(), ShardCodecError> {
    if !compressed.starts_with(&ZSTD_FRAME_MAGIC) {
        return Err(ShardCodecError::ZstdFrameProfile {
            reason: "the payload must be one standard Zstandard frame",
        });
    }
    let descriptor = compressed
        .get(ZSTD_FRAME_DESCRIPTOR_OFFSET)
        .copied()
        .ok_or(ShardCodecError::ZstdFrameProfile {
            reason: "the frame descriptor is missing",
        })?;
    if descriptor & ZSTD_FRAME_RESERVED_FLAG_MASK != 0 {
        return Err(ShardCodecError::ZstdFrameProfile {
            reason: "reserved frame-descriptor flags must be zero",
        });
    }
    if descriptor & ZSTD_FRAME_DICTIONARY_ID_FLAG_MASK != 0 {
        return Err(ShardCodecError::ZstdFrameProfile {
            reason: "dictionary IDs are forbidden",
        });
    }
    if descriptor & ZSTD_FRAME_CONTENT_CHECKSUM_FLAG != 0 {
        return Err(ShardCodecError::ZstdFrameProfile {
            reason: "the Zstandard content checksum must be disabled",
        });
    }
    Ok(())
}

/// One fully codec-validated inner payload ready for outer-shard assembly
/// without a re-encode cycle.
///
/// Persisted or otherwise untrusted bytes must enter through [`Self::validate`],
/// which checks the selected storage kind, encoded ceiling, CRC, complete
/// single-frame extent, observable frame-profile flags, declared decoded
/// length, bounded window, and a full decode of the frame body. Fresh decoded
/// bytes should enter through [`Self::encode`] so the sole canonical encoder
/// establishes those properties without an immediately redundant decode.
///
/// Zstandard frames do not record the compression level. Level 3 is therefore
/// provenance established by [`Self::encode`], not a property that
/// [`Self::validate`] can infer from persisted bytes without re-encoding them.
/// The fields stay opaque so validated bytes cannot be mutated afterward.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalEncodedInner {
    kind: ShardProfileKind,
    bytes: Vec<u8>,
}

impl CanonicalEncodedInner {
    /// Encodes decoded bytes through the sole canonical level-3 authority.
    pub fn encode(kind: ShardProfileKind, decoded: &[u8]) -> Result<Self, ShardCodecError> {
        let bytes = encode_inner_payload(kind, decoded)?;
        Ok(Self { kind, bytes })
    }

    /// Fully validates and decodes one persisted or otherwise untrusted inner.
    ///
    /// The decoded bytes are intentionally discarded: successful construction
    /// proves codec validity while preserving the exact encoded bytes for
    /// pass-through outer-shard assembly.
    pub fn validate(kind: ShardProfileKind, bytes: Vec<u8>) -> Result<Self, ShardCodecError> {
        drop(decode_inner_payload(kind, &bytes)?);
        Ok(Self { kind, bytes })
    }

    pub const fn kind(&self) -> ShardProfileKind {
        self.kind
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
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

/// Encodes the sole canonical fixed end-index from payload lengths in slot
/// order.
///
/// Present payloads must already have been written contiguously in increasing
/// slot order. Offsets are derived here so callers cannot introduce gaps,
/// overlap, or an alternate index layout.
pub(crate) fn encode_shard_index_tail(
    kind: ShardProfileKind,
    encoded_chunk_lengths: &[Option<u64>],
) -> Result<Vec<u8>, ShardCodecError> {
    let expected = kind.chunks_per_shard();
    if encoded_chunk_lengths.len() != expected {
        return Err(ShardCodecError::ChunkCount {
            expected,
            actual: encoded_chunk_lengths.len(),
        });
    }

    let inner_limit = u64::try_from(kind.encoded_inner_bytes_max())
        .map_err(|_| ShardCodecError::LengthOverflow)?;
    let mut index_bytes = Vec::with_capacity(expected * INDEX_ENTRY_BYTES);
    let mut next_offset = 0_u64;
    for (chunk_index, encoded_length) in encoded_chunk_lengths.iter().copied().enumerate() {
        let (offset, nbytes) = match encoded_length {
            None => (MISSING, MISSING),
            Some(0) => return Err(ShardCodecError::ZeroLengthEntry { chunk_index }),
            Some(nbytes) if nbytes > inner_limit => {
                return Err(ShardCodecError::IndexEntryTooLarge {
                    chunk_index,
                    limit: inner_limit,
                    actual: nbytes,
                });
            }
            Some(nbytes) => {
                let offset = next_offset;
                next_offset = next_offset
                    .checked_add(nbytes)
                    .ok_or(ShardCodecError::IndexRangeOverflow { chunk_index })?;
                (offset, nbytes)
            }
        };
        index_bytes.extend_from_slice(&offset.to_le_bytes());
        index_bytes.extend_from_slice(&nbytes.to_le_bytes());
    }

    let tail_bytes = kind.index_tail_bytes();
    let complete_bytes = next_offset
        .checked_add(u64::try_from(tail_bytes).map_err(|_| ShardCodecError::LengthOverflow)?)
        .ok_or(ShardCodecError::LengthOverflow)?;
    let shard_limit = u64::try_from(kind.encoded_shard_bytes_max())
        .map_err(|_| ShardCodecError::LengthOverflow)?;
    if complete_bytes > shard_limit {
        return Err(ShardCodecError::EncodedShardTooLarge {
            limit: shard_limit,
            actual: complete_bytes,
        });
    }

    let checksum = crc32c::crc32c(&index_bytes);
    index_bytes.extend_from_slice(&checksum.to_le_bytes());
    debug_assert_eq!(index_bytes.len(), tail_bytes);
    Ok(index_bytes)
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
    let mut encoded_chunk_lengths = Vec::with_capacity(expected);
    for decoded in decoded_chunks {
        if let Some(decoded) = decoded {
            let encoded = encode_inner_payload(kind, decoded)?;
            let nbytes =
                u64::try_from(encoded.len()).map_err(|_| ShardCodecError::LengthOverflow)?;
            shard.extend_from_slice(&encoded);
            encoded_chunk_lengths.push(Some(nbytes));
        } else {
            encoded_chunk_lengths.push(None);
        }
    }

    shard.extend_from_slice(&encode_shard_index_tail(kind, &encoded_chunk_lengths)?);
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

    fn profiled_zstd(
        bytes: &[u8],
        level: i32,
        include_checksum: bool,
        pledged_size: bool,
    ) -> Vec<u8> {
        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), level).unwrap();
        encoder.include_checksum(include_checksum).unwrap();
        if pledged_size {
            encoder
                .set_pledged_src_size(Some(bytes.len() as u64))
                .unwrap();
        }
        encoder.write_all(bytes).unwrap();
        let mut encoded = encoder.finish().unwrap();
        encoded.extend_from_slice(&crc32c::crc32c(&encoded).to_le_bytes());
        encoded
    }

    fn replace_inner_crc(encoded: &mut Vec<u8>) {
        encoded.truncate(encoded.len() - CRC32C_BYTES);
        let checksum = crc32c::crc32c(encoded);
        encoded.extend_from_slice(&checksum.to_le_bytes());
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
        let kind = ShardProfileKind::Pixel2dUint16;
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
    fn canonical_encoded_inner_distinguishes_fresh_encoding_from_persisted_validation() {
        let kind = ShardProfileKind::Validity2d;
        let decoded = (0..kind.decoded_inner_bytes())
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();

        let fresh = CanonicalEncodedInner::encode(kind, &decoded).unwrap();
        assert_eq!(fresh.kind(), kind);
        assert_eq!(decode_inner_payload(kind, fresh.bytes()).unwrap(), decoded);

        let persisted = CanonicalEncodedInner::validate(kind, fresh.bytes().to_vec()).unwrap();
        assert_eq!(persisted, fresh);
    }

    #[test]
    fn persisted_inner_rejects_nonprofile_frame_flags_missing_size_and_extra_frames() {
        let kind = ShardProfileKind::Validity2d;
        let decoded = vec![0x5a; kind.decoded_inner_bytes()];

        let checksum_frame = profiled_zstd(&decoded, ZSTD_LEVEL, true, true);
        assert_eq!(
            CanonicalEncodedInner::validate(kind, checksum_frame),
            Err(ShardCodecError::ZstdFrameProfile {
                reason: "the Zstandard content checksum must be disabled",
            })
        );

        let no_content_size = profiled_zstd(&decoded, ZSTD_LEVEL, false, false);
        assert_eq!(
            CanonicalEncodedInner::validate(kind, no_content_size),
            Err(ShardCodecError::ZstdFrameProfile {
                reason: "the frame must declare its decoded content size",
            })
        );

        let canonical = encode_inner_payload(kind, &decoded).unwrap();
        let compressed = &canonical[..canonical.len() - CRC32C_BYTES];

        let mut bad_inner_crc = canonical.clone();
        bad_inner_crc[ZSTD_FRAME_DESCRIPTOR_OFFSET + 1] ^= 1;
        assert_eq!(
            CanonicalEncodedInner::validate(kind, bad_inner_crc),
            Err(ShardCodecError::InnerChecksumMismatch)
        );

        let mut bad_magic = canonical.clone();
        bad_magic[0] ^= 1;
        replace_inner_crc(&mut bad_magic);
        assert_eq!(
            CanonicalEncodedInner::validate(kind, bad_magic),
            Err(ShardCodecError::ZstdFrameProfile {
                reason: "the payload must be one standard Zstandard frame",
            })
        );

        let mut reserved_flag = canonical.clone();
        reserved_flag[ZSTD_FRAME_DESCRIPTOR_OFFSET] |= 0b0000_1000;
        replace_inner_crc(&mut reserved_flag);
        assert_eq!(
            CanonicalEncodedInner::validate(kind, reserved_flag),
            Err(ShardCodecError::ZstdFrameProfile {
                reason: "reserved frame-descriptor flags must be zero",
            })
        );

        let mut concatenated = Vec::with_capacity(compressed.len() * 2 + CRC32C_BYTES);
        concatenated.extend_from_slice(compressed);
        concatenated.extend_from_slice(compressed);
        let checksum = crc32c::crc32c(&concatenated);
        concatenated.extend_from_slice(&checksum.to_le_bytes());
        assert!(matches!(
            CanonicalEncodedInner::validate(kind, concatenated),
            Err(ShardCodecError::Zstd {
                operation: "validate encoded frame",
                ..
            })
        ));

        let mut dictionary_flag = canonical;
        dictionary_flag[ZSTD_FRAME_DESCRIPTOR_OFFSET] |= 1;
        replace_inner_crc(&mut dictionary_flag);
        assert_eq!(
            CanonicalEncodedInner::validate(kind, dictionary_flag),
            Err(ShardCodecError::ZstdFrameProfile {
                reason: "dictionary IDs are forbidden",
            })
        );
    }

    #[test]
    fn persisted_inner_fully_decodes_a_structurally_sized_frame_body() {
        let kind = ShardProfileKind::Validity2d;
        let decoded = (0..kind.decoded_inner_bytes())
            .map(|index| ((index * 31 + index / 7) % 251) as u8)
            .collect::<Vec<_>>();
        let canonical = encode_inner_payload(kind, &decoded).unwrap();
        let checksum_at = canonical.len() - CRC32C_BYTES;
        let mut malformed = None;

        'candidate: for index in (ZSTD_FRAME_DESCRIPTOR_OFFSET + 1)..checksum_at {
            for bit in 0..u8::BITS {
                let mut candidate = canonical.clone();
                candidate[index] ^= 1 << bit;
                replace_inner_crc(&mut candidate);
                let compressed = &candidate[..candidate.len() - CRC32C_BYTES];
                if zstd::zstd_safe::find_frame_compressed_size(compressed) != Ok(compressed.len())
                    || !matches!(
                        zstd::zstd_safe::get_frame_content_size(compressed),
                        Ok(Some(bytes)) if bytes == kind.decoded_inner_bytes() as u64
                    )
                {
                    continue;
                }
                if matches!(
                    decode_inner_payload(kind, &candidate),
                    Err(ShardCodecError::Zstd {
                        operation: "decode",
                        ..
                    })
                ) {
                    malformed = Some(candidate);
                    break 'candidate;
                }
            }
        }

        let malformed = malformed.expect(
            "the fixture must contain a body mutation accepted by frame sizing but rejected by decoding",
        );
        assert!(matches!(
            CanonicalEncodedInner::validate(kind, malformed),
            Err(ShardCodecError::Zstd {
                operation: "decode",
                ..
            })
        ));
    }

    #[test]
    fn persisted_validation_cannot_infer_the_encoder_compression_level() {
        let kind = ShardProfileKind::Validity2d;
        let decoded = (0..kind.decoded_inner_bytes())
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let level_one = profiled_zstd(&decoded, 1, false, true);

        let validated = CanonicalEncodedInner::validate(kind, level_one).unwrap();
        assert_eq!(
            decode_inner_payload(kind, validated.bytes()).unwrap(),
            decoded
        );
    }

    #[test]
    fn inner_decode_rejects_bombs_and_encoded_oversize_before_decompression() {
        let kind = ShardProfileKind::Validity2d;
        let bomb = profiled_zstd(
            &vec![0; kind.decoded_inner_bytes() + 1],
            ZSTD_LEVEL,
            false,
            true,
        );
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
    fn canonical_tail_encoder_derives_offsets_missing_pairs_and_checksum() {
        let kind = ShardProfileKind::Validity2d;
        let mut lengths = vec![None; kind.chunks_per_shard()];
        lengths[0] = Some(7);
        lengths[2] = Some(11);

        let first = encode_shard_index_tail(kind, &lengths).unwrap();
        let second = encode_shard_index_tail(kind, &lengths).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), kind.index_tail_bytes());

        let pair = |slot: usize| {
            let at = slot * INDEX_ENTRY_BYTES;
            (
                u64::from_le_bytes(first[at..at + 8].try_into().unwrap()),
                u64::from_le_bytes(first[at + 8..at + 16].try_into().unwrap()),
            )
        };
        assert_eq!(pair(0), (0, 7));
        assert_eq!(pair(1), (MISSING, MISSING));
        assert_eq!(pair(2), (7, 11));

        let checksum_at = first.len() - CRC32C_BYTES;
        assert_eq!(
            u32::from_le_bytes(first[checksum_at..].try_into().unwrap()),
            crc32c::crc32c(&first[..checksum_at])
        );
        let decoded = decode_shard_index_tail(kind, &first, 18).unwrap();
        assert_eq!(decoded.require_entry(0).unwrap().range(), 0..7);
        assert_eq!(decoded.require_entry(2).unwrap().range(), 7..18);
        assert_eq!(decoded.entry(1).unwrap(), None);
    }

    #[test]
    fn canonical_tail_encoder_rejects_count_zero_and_oversized_entries() {
        let kind = ShardProfileKind::Pixel2dUint8;
        assert_eq!(
            encode_shard_index_tail(kind, &[]),
            Err(ShardCodecError::ChunkCount {
                expected: kind.chunks_per_shard(),
                actual: 0,
            })
        );

        let mut lengths = vec![None; kind.chunks_per_shard()];
        lengths[3] = Some(0);
        assert_eq!(
            encode_shard_index_tail(kind, &lengths),
            Err(ShardCodecError::ZeroLengthEntry { chunk_index: 3 })
        );

        let oversized = u64::try_from(kind.encoded_inner_bytes_max()).unwrap() + 1;
        lengths[3] = Some(oversized);
        assert_eq!(
            encode_shard_index_tail(kind, &lengths),
            Err(ShardCodecError::IndexEntryTooLarge {
                chunk_index: 3,
                limit: u64::try_from(kind.encoded_inner_bytes_max()).unwrap(),
                actual: oversized,
            })
        );
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
