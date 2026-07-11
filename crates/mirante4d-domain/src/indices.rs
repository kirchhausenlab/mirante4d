/// Scientific sample data types supported by the foundation contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntensityDType {
    Uint8,
    Uint16,
    Float32,
}

impl IntensityDType {
    pub const fn bytes_per_sample(self) -> u8 {
        match self {
            Self::Uint8 => 1,
            Self::Uint16 => 2,
            Self::Float32 => 4,
        }
    }
}

/// Zero-based logical layer ordinal used by the scientific identity contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LogicalLayerKey(u32);

impl LogicalLayerKey {
    pub const fn new(ordinal: u32) -> Self {
        Self(ordinal)
    }

    pub const fn ordinal(self) -> u32 {
        self.0
    }
}

/// Zero-based timepoint index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TimeIndex(u64);

impl TimeIndex {
    pub const fn new(index: u64) -> Self {
        Self(index)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Zero-based multiscale level, where level zero is the scientific base scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScaleLevel(u32);

impl ScaleLevel {
    pub const BASE: Self = Self(0);

    pub const fn new(level: u32) -> Self {
        Self(level)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_wrappers_preserve_their_full_unsigned_ranges() {
        assert_eq!(LogicalLayerKey::new(u32::MAX).ordinal(), u32::MAX);
        assert_eq!(TimeIndex::new(u64::MAX).get(), u64::MAX);
        assert_eq!(ScaleLevel::new(u32::MAX).get(), u32::MAX);
    }

    #[test]
    fn intensity_dtype_reports_exact_sample_width() {
        assert_eq!(IntensityDType::Uint8.bytes_per_sample(), 1);
        assert_eq!(IntensityDType::Uint16.bytes_per_sample(), 2);
        assert_eq!(IntensityDType::Float32.bytes_per_sample(), 4);
    }
}
