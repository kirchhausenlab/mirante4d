use mirante4d_domain::IntensityDType;
use thiserror::Error;

const RECORD_BYTES: usize = crate::limits::PACKED_INDEX_RECORD_BYTES as usize;

const FLAG_OCCUPIED: u32 = 1 << 0;
const FLAG_PIXEL_PAYLOAD_PRESENT: u32 = 1 << 1;
const FLAG_EXPLICIT_VALIDITY: u32 = 1 << 2;
const FLAG_ALL_VALID: u32 = 1 << 3;
const FLAG_ALL_INVALID: u32 = 1 << 4;
const FLAG_NUMERIC_RANGE_PRESENT: u32 = 1 << 5;
const KNOWN_FLAGS: u32 = FLAG_OCCUPIED
    | FLAG_PIXEL_PAYLOAD_PRESENT
    | FLAG_EXPLICIT_VALIDITY
    | FLAG_ALL_VALID
    | FLAG_ALL_INVALID
    | FLAG_NUMERIC_RANGE_PRESENT;

/// The seven coordinates stored in one packed-index record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackedIndexCoordinates {
    image_ordinal: u32,
    scale: u32,
    t: u32,
    c: u32,
    z_chunk: u32,
    y_chunk: u32,
    x_chunk: u32,
}

impl PackedIndexCoordinates {
    pub const fn new(
        image_ordinal: u32,
        scale: u32,
        t: u32,
        c: u32,
        z_chunk: u32,
        y_chunk: u32,
        x_chunk: u32,
    ) -> Self {
        Self {
            image_ordinal,
            scale,
            t,
            c,
            z_chunk,
            y_chunk,
            x_chunk,
        }
    }

    pub const fn image_ordinal(self) -> u32 {
        self.image_ordinal
    }

    pub const fn scale(self) -> u32 {
        self.scale
    }

    pub const fn t(self) -> u32 {
        self.t
    }

    pub const fn c(self) -> u32 {
        self.c
    }

    pub const fn z_chunk(self) -> u32 {
        self.z_chunk
    }

    pub const fn y_chunk(self) -> u32 {
        self.y_chunk
    }

    pub const fn x_chunk(self) -> u32 {
        self.x_chunk
    }
}

/// Counts and optional numeric range stored in one packed-index record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackedIndexStatistics {
    valid_voxel_count: u64,
    nonfill_valid_voxel_count: u64,
    numeric_range_bits: Option<(u64, u64)>,
}

impl PackedIndexStatistics {
    pub const fn new(
        valid_voxel_count: u64,
        nonfill_valid_voxel_count: u64,
        numeric_range_bits: Option<(u64, u64)>,
    ) -> Self {
        Self {
            valid_voxel_count,
            nonfill_valid_voxel_count,
            numeric_range_bits,
        }
    }

    pub const fn valid_voxel_count(self) -> u64 {
        self.valid_voxel_count
    }

    pub const fn nonfill_valid_voxel_count(self) -> u64 {
        self.nonfill_valid_voxel_count
    }

    pub const fn numeric_range_bits(self) -> Option<(u64, u64)> {
        self.numeric_range_bits
    }
}

/// A validated fixed-width `m4d-packed-index-1.0` record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackedIndexRecord {
    flags: u32,
    coordinates: PackedIndexCoordinates,
    statistics: PackedIndexStatistics,
}

impl PackedIndexRecord {
    /// Builds a record and derives every count-dependent flag.
    pub fn new(
        coordinates: PackedIndexCoordinates,
        statistics: PackedIndexStatistics,
        pixel_payload_present: bool,
        explicit_validity: bool,
        dtype: IntensityDType,
        logical_brick_capacity: u64,
    ) -> Result<Self, PackedIndexError> {
        let mut flags = 0;
        set_flag(
            &mut flags,
            FLAG_OCCUPIED,
            statistics.nonfill_valid_voxel_count > 0,
        );
        set_flag(
            &mut flags,
            FLAG_PIXEL_PAYLOAD_PRESENT,
            pixel_payload_present,
        );
        set_flag(&mut flags, FLAG_EXPLICIT_VALIDITY, explicit_validity);
        set_flag(
            &mut flags,
            FLAG_ALL_VALID,
            statistics.valid_voxel_count == logical_brick_capacity,
        );
        set_flag(
            &mut flags,
            FLAG_ALL_INVALID,
            statistics.valid_voxel_count == 0,
        );
        set_flag(
            &mut flags,
            FLAG_NUMERIC_RANGE_PRESENT,
            statistics.numeric_range_bits.is_some(),
        );

        let (minimum_bits, maximum_bits) = statistics.numeric_range_bits.unwrap_or((0, 0));
        validate(
            flags,
            statistics.valid_voxel_count,
            statistics.nonfill_valid_voxel_count,
            minimum_bits,
            maximum_bits,
            dtype,
            logical_brick_capacity,
        )?;

        Ok(Self {
            flags,
            coordinates,
            statistics,
        })
    }

    /// Decodes and validates exactly one 64-byte little-endian record.
    pub fn decode(
        bytes: &[u8],
        dtype: IntensityDType,
        logical_brick_capacity: u64,
    ) -> Result<Self, PackedIndexError> {
        if bytes.len() != RECORD_BYTES {
            return Err(PackedIndexError::InvalidByteLength {
                actual: bytes.len(),
            });
        }

        let flags = read_u32(bytes, 0);
        let coordinates = PackedIndexCoordinates::new(
            read_u32(bytes, 4),
            read_u32(bytes, 8),
            read_u32(bytes, 12),
            read_u32(bytes, 16),
            read_u32(bytes, 20),
            read_u32(bytes, 24),
            read_u32(bytes, 28),
        );
        let valid_voxel_count = read_u64(bytes, 32);
        let nonfill_valid_voxel_count = read_u64(bytes, 40);
        let minimum_bits = read_u64(bytes, 48);
        let maximum_bits = read_u64(bytes, 56);

        validate(
            flags,
            valid_voxel_count,
            nonfill_valid_voxel_count,
            minimum_bits,
            maximum_bits,
            dtype,
            logical_brick_capacity,
        )?;

        let numeric_range_bits =
            flag(flags, FLAG_NUMERIC_RANGE_PRESENT).then_some((minimum_bits, maximum_bits));
        Ok(Self {
            flags,
            coordinates,
            statistics: PackedIndexStatistics::new(
                valid_voxel_count,
                nonfill_valid_voxel_count,
                numeric_range_bits,
            ),
        })
    }

    /// Encodes this already-validated record as exact little-endian bytes.
    pub fn encode(self) -> [u8; RECORD_BYTES] {
        let mut bytes = [0; RECORD_BYTES];
        write_u32(&mut bytes, 0, self.flags);
        write_u32(&mut bytes, 4, self.coordinates.image_ordinal);
        write_u32(&mut bytes, 8, self.coordinates.scale);
        write_u32(&mut bytes, 12, self.coordinates.t);
        write_u32(&mut bytes, 16, self.coordinates.c);
        write_u32(&mut bytes, 20, self.coordinates.z_chunk);
        write_u32(&mut bytes, 24, self.coordinates.y_chunk);
        write_u32(&mut bytes, 28, self.coordinates.x_chunk);
        write_u64(&mut bytes, 32, self.statistics.valid_voxel_count);
        write_u64(&mut bytes, 40, self.statistics.nonfill_valid_voxel_count);
        let (minimum_bits, maximum_bits) = self.statistics.numeric_range_bits.unwrap_or((0, 0));
        write_u64(&mut bytes, 48, minimum_bits);
        write_u64(&mut bytes, 56, maximum_bits);
        bytes
    }

    pub const fn coordinates(self) -> PackedIndexCoordinates {
        self.coordinates
    }

    pub const fn statistics(self) -> PackedIndexStatistics {
        self.statistics
    }

    pub const fn flags_bits(self) -> u32 {
        self.flags
    }

    pub const fn occupied(self) -> bool {
        flag(self.flags, FLAG_OCCUPIED)
    }

    pub const fn pixel_payload_present(self) -> bool {
        flag(self.flags, FLAG_PIXEL_PAYLOAD_PRESENT)
    }

    pub const fn explicit_validity(self) -> bool {
        flag(self.flags, FLAG_EXPLICIT_VALIDITY)
    }

    pub const fn all_voxels_valid(self) -> bool {
        flag(self.flags, FLAG_ALL_VALID)
    }

    pub const fn all_voxels_invalid(self) -> bool {
        flag(self.flags, FLAG_ALL_INVALID)
    }
}

/// A strict packed-index construction or decoding failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PackedIndexError {
    #[error("packed-index record has {actual} bytes; expected exactly 64")]
    InvalidByteLength { actual: usize },
    #[error("logical brick capacity must be positive")]
    ZeroLogicalBrickCapacity,
    #[error("packed-index record contains unknown flag bits {bits:#010x}")]
    UnknownFlagBits { bits: u32 },
    #[error("valid voxel count {count} exceeds logical brick capacity {capacity}")]
    ValidCountExceedsCapacity { count: u64, capacity: u64 },
    #[error("nonfill valid voxel count {nonfill} exceeds valid voxel count {valid}")]
    NonfillCountExceedsValid { nonfill: u64, valid: u64 },
    #[error("{flag} flag is {actual}, but the counts require {expected}")]
    FlagCountMismatch {
        flag: &'static str,
        actual: bool,
        expected: bool,
    },
    #[error("a record without explicit validity must mark every logical voxel valid")]
    ImplicitValidityNotAllValid,
    #[error("pixel payload is absent despite {nonfill} valid nonfill voxels")]
    MissingOccupiedPixelPayload { nonfill: u64 },
    #[error("numeric range bits must both be zero when the range-present flag is clear")]
    UnexpectedNumericRangeBits,
    #[error("{field} bits {bits:#018x} are not a zero-extended {dtype:?} value")]
    InvalidIntegerRangeBits {
        field: &'static str,
        dtype: IntensityDType,
        bits: u64,
    },
    #[error("{field} float32 bits must have a zero high word, observed {bits:#018x}")]
    FloatRangeHighBitsSet { field: &'static str, bits: u64 },
    #[error("{field} float32 bits encode a non-finite value: {bits:#010x}")]
    NonFiniteFloatRange { field: &'static str, bits: u32 },
    #[error("numeric minimum is greater than numeric maximum")]
    InvertedNumericRange,
}

#[allow(clippy::too_many_arguments)]
fn validate(
    flags: u32,
    valid_voxel_count: u64,
    nonfill_valid_voxel_count: u64,
    minimum_bits: u64,
    maximum_bits: u64,
    dtype: IntensityDType,
    logical_brick_capacity: u64,
) -> Result<(), PackedIndexError> {
    if logical_brick_capacity == 0 {
        return Err(PackedIndexError::ZeroLogicalBrickCapacity);
    }

    let unknown = flags & !KNOWN_FLAGS;
    if unknown != 0 {
        return Err(PackedIndexError::UnknownFlagBits { bits: unknown });
    }
    if valid_voxel_count > logical_brick_capacity {
        return Err(PackedIndexError::ValidCountExceedsCapacity {
            count: valid_voxel_count,
            capacity: logical_brick_capacity,
        });
    }
    if nonfill_valid_voxel_count > valid_voxel_count {
        return Err(PackedIndexError::NonfillCountExceedsValid {
            nonfill: nonfill_valid_voxel_count,
            valid: valid_voxel_count,
        });
    }

    require_flag(
        flags,
        FLAG_OCCUPIED,
        "occupied",
        nonfill_valid_voxel_count > 0,
    )?;
    require_flag(
        flags,
        FLAG_ALL_VALID,
        "all-valid",
        valid_voxel_count == logical_brick_capacity,
    )?;
    require_flag(
        flags,
        FLAG_ALL_INVALID,
        "all-invalid",
        valid_voxel_count == 0,
    )?;
    require_flag(
        flags,
        FLAG_NUMERIC_RANGE_PRESENT,
        "numeric-range-present",
        valid_voxel_count > 0,
    )?;

    if !flag(flags, FLAG_EXPLICIT_VALIDITY) && !flag(flags, FLAG_ALL_VALID) {
        return Err(PackedIndexError::ImplicitValidityNotAllValid);
    }
    if !flag(flags, FLAG_PIXEL_PAYLOAD_PRESENT) && nonfill_valid_voxel_count != 0 {
        return Err(PackedIndexError::MissingOccupiedPixelPayload {
            nonfill: nonfill_valid_voxel_count,
        });
    }

    if !flag(flags, FLAG_NUMERIC_RANGE_PRESENT) {
        if minimum_bits != 0 || maximum_bits != 0 {
            return Err(PackedIndexError::UnexpectedNumericRangeBits);
        }
        return Ok(());
    }

    validate_numeric_range(dtype, minimum_bits, maximum_bits)
}

fn validate_numeric_range(
    dtype: IntensityDType,
    minimum_bits: u64,
    maximum_bits: u64,
) -> Result<(), PackedIndexError> {
    match dtype {
        IntensityDType::Uint8 => {
            validate_integer_bits(dtype, "minimum", minimum_bits, u8::MAX.into())?;
            validate_integer_bits(dtype, "maximum", maximum_bits, u8::MAX.into())?;
            if minimum_bits > maximum_bits {
                return Err(PackedIndexError::InvertedNumericRange);
            }
        }
        IntensityDType::Uint16 => {
            validate_integer_bits(dtype, "minimum", minimum_bits, u16::MAX.into())?;
            validate_integer_bits(dtype, "maximum", maximum_bits, u16::MAX.into())?;
            if minimum_bits > maximum_bits {
                return Err(PackedIndexError::InvertedNumericRange);
            }
        }
        IntensityDType::Float32 => {
            let minimum = validate_float_bits("minimum", minimum_bits)?;
            let maximum = validate_float_bits("maximum", maximum_bits)?;
            if minimum > maximum {
                return Err(PackedIndexError::InvertedNumericRange);
            }
        }
    }
    Ok(())
}

fn validate_integer_bits(
    dtype: IntensityDType,
    field: &'static str,
    bits: u64,
    maximum: u64,
) -> Result<(), PackedIndexError> {
    if bits > maximum {
        return Err(PackedIndexError::InvalidIntegerRangeBits { field, dtype, bits });
    }
    Ok(())
}

fn validate_float_bits(field: &'static str, bits: u64) -> Result<f32, PackedIndexError> {
    if bits >> 32 != 0 {
        return Err(PackedIndexError::FloatRangeHighBitsSet { field, bits });
    }
    let bits = bits as u32;
    let value = f32::from_bits(bits);
    if !value.is_finite() {
        return Err(PackedIndexError::NonFiniteFloatRange { field, bits });
    }
    Ok(value)
}

const fn flag(flags: u32, mask: u32) -> bool {
    flags & mask != 0
}

fn set_flag(flags: &mut u32, mask: u32, value: bool) {
    if value {
        *flags |= mask;
    }
}

fn require_flag(
    flags: u32,
    mask: u32,
    name: &'static str,
    expected: bool,
) -> Result<(), PackedIndexError> {
    let actual = flag(flags, mask);
    if actual != expected {
        return Err(PackedIndexError::FlagCountMismatch {
            flag: name,
            actual,
            expected,
        });
    }
    Ok(())
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    let mut value = [0; 4];
    value.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_le_bytes(value)
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    let mut value = [0; 8];
    value.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(value)
}

fn write_u32(bytes: &mut [u8; RECORD_BYTES], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8; RECORD_BYTES], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coordinates() -> PackedIndexCoordinates {
        PackedIndexCoordinates::new(1, 2, 3, 4, 5, 6, 7)
    }

    fn u16_record() -> PackedIndexRecord {
        PackedIndexRecord::new(
            coordinates(),
            PackedIndexStatistics::new(64, 3, Some((0, 0x1234))),
            true,
            true,
            IntensityDType::Uint16,
            64,
        )
        .unwrap()
    }

    #[test]
    fn exact_little_endian_record_round_trips_with_accessors() {
        let record = u16_record();
        let bytes = record.encode();

        assert_eq!(&bytes[0..4], &0x2f_u32.to_le_bytes());
        for (offset, value) in (4..32).step_by(4).zip(1_u32..=7) {
            assert_eq!(&bytes[offset..offset + 4], &value.to_le_bytes());
        }
        assert_eq!(&bytes[32..40], &64_u64.to_le_bytes());
        assert_eq!(&bytes[40..48], &3_u64.to_le_bytes());
        assert_eq!(&bytes[48..56], &0_u64.to_le_bytes());
        assert_eq!(&bytes[56..64], &0x1234_u64.to_le_bytes());

        let decoded = PackedIndexRecord::decode(&bytes, IntensityDType::Uint16, 64).unwrap();
        assert_eq!(decoded, record);
        assert_eq!(decoded.coordinates(), coordinates());
        assert_eq!(decoded.statistics().valid_voxel_count(), 64);
        assert_eq!(decoded.statistics().nonfill_valid_voxel_count(), 3);
        assert_eq!(decoded.statistics().numeric_range_bits(), Some((0, 0x1234)));
        assert!(decoded.occupied());
        assert!(decoded.pixel_payload_present());
        assert!(decoded.explicit_validity());
        assert!(decoded.all_voxels_valid());
        assert!(!decoded.all_voxels_invalid());
    }

    #[test]
    fn accepts_fill_elision_and_preserves_finite_float_signed_zero_bits() {
        let fill = PackedIndexRecord::new(
            coordinates(),
            PackedIndexStatistics::new(64, 0, Some((0, 0))),
            false,
            false,
            IntensityDType::Uint8,
            64,
        )
        .unwrap();
        assert!(!fill.pixel_payload_present());
        assert!(!fill.occupied());
        assert_eq!(
            fill.flags_bits(),
            FLAG_ALL_VALID | FLAG_NUMERIC_RANGE_PRESENT
        );

        let signed_zero = PackedIndexRecord::new(
            coordinates(),
            PackedIndexStatistics::new(2, 1, Some((0x8000_0000, 0))),
            true,
            false,
            IntensityDType::Float32,
            2,
        )
        .unwrap();
        let decoded =
            PackedIndexRecord::decode(&signed_zero.encode(), IntensityDType::Float32, 2).unwrap();
        assert_eq!(
            decoded.statistics().numeric_range_bits(),
            Some((0x8000_0000, 0))
        );
    }

    #[test]
    fn rejects_corrupt_flags_counts_payload_and_numeric_ranges() {
        let bytes = u16_record().encode();

        assert!(matches!(
            PackedIndexRecord::decode(&bytes[..63], IntensityDType::Uint16, 64),
            Err(PackedIndexError::InvalidByteLength { actual: 63 })
        ));

        let mut unknown_flag = bytes;
        unknown_flag[0..4].copy_from_slice(&(0x2f_u32 | (1 << 6)).to_le_bytes());
        assert!(matches!(
            PackedIndexRecord::decode(&unknown_flag, IntensityDType::Uint16, 64),
            Err(PackedIndexError::UnknownFlagBits { bits: 0x40 })
        ));

        let mut wrong_occupied = bytes;
        wrong_occupied[0..4].copy_from_slice(&(0x2f & !FLAG_OCCUPIED).to_le_bytes());
        assert!(matches!(
            PackedIndexRecord::decode(&wrong_occupied, IntensityDType::Uint16, 64),
            Err(PackedIndexError::FlagCountMismatch {
                flag: "occupied",
                ..
            })
        ));

        let mut too_many_valid = bytes;
        too_many_valid[32..40].copy_from_slice(&65_u64.to_le_bytes());
        assert!(matches!(
            PackedIndexRecord::decode(&too_many_valid, IntensityDType::Uint16, 64),
            Err(PackedIndexError::ValidCountExceedsCapacity { .. })
        ));

        let mut too_many_nonfill = bytes;
        too_many_nonfill[40..48].copy_from_slice(&65_u64.to_le_bytes());
        assert!(matches!(
            PackedIndexRecord::decode(&too_many_nonfill, IntensityDType::Uint16, 64),
            Err(PackedIndexError::NonfillCountExceedsValid { .. })
        ));

        let mut missing_payload = bytes;
        missing_payload[0..4].copy_from_slice(&(0x2f & !FLAG_PIXEL_PAYLOAD_PRESENT).to_le_bytes());
        assert!(matches!(
            PackedIndexRecord::decode(&missing_payload, IntensityDType::Uint16, 64),
            Err(PackedIndexError::MissingOccupiedPixelPayload { nonfill: 3 })
        ));

        let mut wide_u16 = bytes;
        wide_u16[56..64].copy_from_slice(&0x1_0000_u64.to_le_bytes());
        assert!(matches!(
            PackedIndexRecord::decode(&wide_u16, IntensityDType::Uint16, 64),
            Err(PackedIndexError::InvalidIntegerRangeBits {
                field: "maximum",
                ..
            })
        ));

        let float_record = PackedIndexRecord::new(
            coordinates(),
            PackedIndexStatistics::new(1, 1, Some((0, 1.0_f32.to_bits().into()))),
            true,
            false,
            IntensityDType::Float32,
            1,
        )
        .unwrap()
        .encode();

        let mut high_float_bits = float_record;
        high_float_bits[56..64].copy_from_slice(&0x1_3f80_0000_u64.to_le_bytes());
        assert!(matches!(
            PackedIndexRecord::decode(&high_float_bits, IntensityDType::Float32, 1),
            Err(PackedIndexError::FloatRangeHighBitsSet {
                field: "maximum",
                ..
            })
        ));

        let mut non_finite = float_record;
        non_finite[56..64].copy_from_slice(&u64::from(f32::INFINITY.to_bits()).to_le_bytes());
        assert!(matches!(
            PackedIndexRecord::decode(&non_finite, IntensityDType::Float32, 1),
            Err(PackedIndexError::NonFiniteFloatRange {
                field: "maximum",
                ..
            })
        ));

        let mut inverted = float_record;
        inverted[48..56].copy_from_slice(&u64::from(2.0_f32.to_bits()).to_le_bytes());
        assert!(matches!(
            PackedIndexRecord::decode(&inverted, IntensityDType::Float32, 1),
            Err(PackedIndexError::InvertedNumericRange)
        ));
    }
}
