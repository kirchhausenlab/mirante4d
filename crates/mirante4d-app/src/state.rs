#[derive(Debug, Clone, PartialEq)]
pub struct LayerHistogramSummary {
    pub status: HistogramStatus,
    pub bin_count: usize,
    pub sample_count: u64,
    pub min_value: f32,
    pub max_value: f32,
    pub bins: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistogramStatus {
    Exact,
    Sampled { source: String },
    Pending { reason: String },
    Unavailable { reason: String },
}
