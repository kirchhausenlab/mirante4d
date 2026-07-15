/// Package-object access operations performed by successful staged
/// publication validation.
///
/// One read means one successful OS open of a package object through the
/// strict local reader. It is an operation count, not a distinct-path count:
/// whole-object reads, range reads, streamed hashes, and snapshot-only
/// revalidations each count the object access they perform. Directory walks
/// and metadata checks that do not open a package object are excluded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PackageValidationReadReport {
    structure_object_reads: u64,
    exact_object_reads: u64,
    scientific_object_reads: u64,
}

/// Storage-codec work performed by successful package construction and
/// staged validation.
///
/// Import-checkpoint codec work is reported separately by the importer. Calls
/// are operation counts rather than distinct payload counts, and elapsed time
/// is monotonic wall time spent inside the codec operations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PackageCodecReport {
    encode_calls: u64,
    encode_time_ns: u64,
    decode_calls: u64,
    decode_time_ns: u64,
}

impl PackageCodecReport {
    pub(crate) const fn new(
        encode_calls: u64,
        encode_time_ns: u64,
        decode_calls: u64,
        decode_time_ns: u64,
    ) -> Self {
        Self {
            encode_calls,
            encode_time_ns,
            decode_calls,
            decode_time_ns,
        }
    }

    pub const fn encode_calls(self) -> u64 {
        self.encode_calls
    }

    pub const fn encode_time_ns(self) -> u64 {
        self.encode_time_ns
    }

    pub const fn decode_calls(self) -> u64 {
        self.decode_calls
    }

    pub const fn decode_time_ns(self) -> u64 {
        self.decode_time_ns
    }
}

impl PackageValidationReadReport {
    pub(crate) const fn new(
        structure_object_reads: u64,
        exact_object_reads: u64,
        scientific_object_reads: u64,
    ) -> Self {
        Self {
            structure_object_reads,
            exact_object_reads,
            scientific_object_reads,
        }
    }

    pub const fn structure_object_reads(self) -> u64 {
        self.structure_object_reads
    }

    pub const fn exact_object_reads(self) -> u64 {
        self.exact_object_reads
    }

    pub const fn scientific_object_reads(self) -> u64 {
        self.scientific_object_reads
    }

    pub fn total_object_reads(self) -> Option<u64> {
        self.structure_object_reads
            .checked_add(self.exact_object_reads)
            .and_then(|total| total.checked_add(self.scientific_object_reads))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_is_the_checked_sum_of_all_staged_validation_components() {
        let report = PackageValidationReadReport::new(3, 5, 7);
        assert_eq!(report.total_object_reads(), Some(15));
        assert_eq!(
            PackageValidationReadReport::new(u64::MAX, 1, 0).total_object_reads(),
            None
        );
    }

    #[test]
    fn codec_report_preserves_each_staged_validation_counter() {
        let report = PackageCodecReport::new(2, 3, 5, 7);
        assert_eq!(report.encode_calls(), 2);
        assert_eq!(report.encode_time_ns(), 3);
        assert_eq!(report.decode_calls(), 5);
        assert_eq!(report.decode_time_ns(), 7);
    }
}
