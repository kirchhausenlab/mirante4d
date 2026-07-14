use crate::{FrameDiagnostics, FrameDiagnosticsF32};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BrickSkipDiagnostics {
    pub skipped_brick_intervals: u64,
    pub empty_brick_intervals: u64,
    pub mip_range_intervals: u64,
    pub iso_range_intervals: u64,
    pub dvr_range_intervals: u64,
}

impl BrickSkipDiagnostics {
    pub fn add_assign(&mut self, other: Self) {
        self.skipped_brick_intervals = self
            .skipped_brick_intervals
            .saturating_add(other.skipped_brick_intervals);
        self.empty_brick_intervals = self
            .empty_brick_intervals
            .saturating_add(other.empty_brick_intervals);
        self.mip_range_intervals = self
            .mip_range_intervals
            .saturating_add(other.mip_range_intervals);
        self.iso_range_intervals = self
            .iso_range_intervals
            .saturating_add(other.iso_range_intervals);
        self.dvr_range_intervals = self
            .dvr_range_intervals
            .saturating_add(other.dvr_range_intervals);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrickFrameDiagnostics {
    pub frame: FrameDiagnostics,
    pub complete: bool,
    pub missing_voxel_samples: u64,
    pub skip: BrickSkipDiagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrickFrameDiagnosticsF32 {
    pub frame: FrameDiagnosticsF32,
    pub complete: bool,
    pub missing_voxel_samples: u64,
}
