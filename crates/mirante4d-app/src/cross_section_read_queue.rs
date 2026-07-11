use mirante4d_core::{LayerId, TimeIndex};
#[cfg(test)]
use mirante4d_data::{BrickReadPool, BrickReadSpec};
use mirante4d_data::{
    BrickReadTicket, BrickRequestPriority, CancellationToken, CrossSectionChunkReadPool,
    CrossSectionChunkReadSpec, DataError, DataGenerationId, SpatialBrickIndex,
};

use crate::{
    AppState,
    cross_section_runtime::{
        CrossSectionChunkKey, CrossSectionChunkQueueEntry, CrossSectionRuntime,
    },
    viewer_layout::PanelId,
};

#[derive(Debug, Clone)]
pub(crate) struct CrossSectionChunkReadSubmission {
    pub(crate) layer_id: LayerId,
    pub(crate) scale_level: u32,
    pub(crate) timepoint: TimeIndex,
    pub(crate) brick_index: SpatialBrickIndex,
    pub(crate) priority: BrickRequestPriority,
    pub(crate) queue_priority: i64,
    pub(crate) cancellation: CancellationToken,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CrossSectionReadAdmission {
    pub(crate) queue_entry: CrossSectionChunkQueueEntry,
    pub(crate) worker_queue_priority: i64,
}

pub(crate) trait CrossSectionReadBackend {
    fn active_generation(&self) -> DataGenerationId;

    fn submit_cross_section_chunk_read(
        &self,
        generation_id: DataGenerationId,
        submission: CrossSectionChunkReadSubmission,
    ) -> Result<BrickReadTicket, DataError>;
}

impl CrossSectionReadBackend for CrossSectionChunkReadPool {
    fn active_generation(&self) -> DataGenerationId {
        CrossSectionChunkReadPool::active_generation(self)
    }

    fn submit_cross_section_chunk_read(
        &self,
        generation_id: DataGenerationId,
        submission: CrossSectionChunkReadSubmission,
    ) -> Result<BrickReadTicket, DataError> {
        self.submit_chunk_for_generation(
            generation_id,
            CrossSectionChunkReadSpec {
                layer_id: submission.layer_id,
                scale_level: submission.scale_level,
                timepoint: submission.timepoint,
                brick_index: submission.brick_index,
                priority: submission.priority,
                queue_priority: submission.queue_priority,
                cancellation: submission.cancellation,
            },
        )
    }
}

#[cfg(test)]
impl CrossSectionReadBackend for BrickReadPool {
    fn active_generation(&self) -> DataGenerationId {
        BrickReadPool::active_generation(self)
    }

    fn submit_cross_section_chunk_read(
        &self,
        generation_id: DataGenerationId,
        submission: CrossSectionChunkReadSubmission,
    ) -> Result<BrickReadTicket, DataError> {
        self.submit_brick_spec_for_generation(
            generation_id,
            BrickReadSpec {
                layer_id: submission.layer_id,
                scale_level: submission.scale_level,
                timepoint: submission.timepoint,
                brick_index: submission.brick_index,
                sample_region: None,
                coalesced_brick_indices: Vec::new(),
                priority: submission.priority,
                queue_priority: submission.queue_priority,
                cancellation: submission.cancellation,
            },
        )
    }
}

impl CrossSectionChunkReadSubmission {
    pub(crate) fn new(
        key: &CrossSectionChunkKey,
        priority: BrickRequestPriority,
        queue_priority: i64,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            layer_id: key.layer_id.clone(),
            scale_level: key.scale_level,
            timepoint: key.timepoint,
            brick_index: key.brick_index,
            priority,
            queue_priority,
            cancellation,
        }
    }
}

pub(crate) fn create_cross_section_read_pool(
    state: &AppState,
) -> Option<CrossSectionChunkReadPool> {
    match CrossSectionChunkReadPool::new(
        state.dataset.clone(),
        default_cross_section_worker_count(),
        default_cross_section_queue_capacity(),
    ) {
        Ok(pool) => Some(pool),
        Err(err) => {
            tracing::error!(error = %err, "failed to create cross-section read pool");
            None
        }
    }
}

fn default_cross_section_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|parallelism| parallelism.get().saturating_sub(1).clamp(1, 4))
        .unwrap_or(1)
}

fn default_cross_section_queue_capacity() -> usize {
    8192
}

pub(crate) fn cross_section_read_admissions_for_refresh(
    runtime: &CrossSectionRuntime,
    panel_order: impl IntoIterator<Item = PanelId>,
    budget: usize,
) -> Vec<CrossSectionReadAdmission> {
    let queue_entries = runtime
        .download_promotion_entries_for_panels(panel_order)
        .into_iter()
        .take(budget)
        .collect::<Vec<_>>();
    let admission_count = queue_entries.len();
    queue_entries
        .into_iter()
        .enumerate()
        .map(|(admission_index, queue_entry)| CrossSectionReadAdmission {
            queue_entry,
            worker_queue_priority: cross_section_worker_queue_priority_for_admission(
                admission_index,
                admission_count,
            ),
        })
        .collect()
}

fn cross_section_worker_queue_priority_for_admission(index: usize, len: usize) -> i64 {
    len.saturating_sub(index) as i64
}

#[cfg(test)]
mod tests {
    use glam::DVec2;
    use mirante4d_core::{DatasetId, LayerId, TimeIndex};
    use mirante4d_data::SpatialBrickIndex;
    use mirante4d_renderer::CrossSectionPanelBounds;

    use super::*;
    use crate::cross_section_runtime::{
        CrossSectionChunkPriorityTier, CrossSectionVisibleChunkGeometry,
        CrossSectionVisibleChunkPlan,
    };

    fn key(z: u64, y: u64, x: u64) -> CrossSectionChunkKey {
        CrossSectionChunkKey {
            dataset_id: DatasetId::new("dataset").unwrap(),
            layer_id: LayerId::new("layer").unwrap(),
            timepoint: TimeIndex(0),
            scale_level: 0,
            brick_index: SpatialBrickIndex::new(z, y, x),
        }
    }

    fn geometry(
        key: CrossSectionChunkKey,
        priority_score: f64,
    ) -> CrossSectionVisibleChunkGeometry {
        CrossSectionVisibleChunkGeometry {
            key,
            vertex_count: 4,
            panel_bounds: CrossSectionPanelBounds {
                min_points: DVec2::ZERO,
                max_points: DVec2::new(1.0, 1.0),
            },
            priority_score,
        }
    }

    fn plan(
        panel_id: PanelId,
        geometries: Vec<CrossSectionVisibleChunkGeometry>,
    ) -> CrossSectionVisibleChunkPlan {
        CrossSectionVisibleChunkPlan {
            panel_id,
            generation: 1,
            scale_level: 0,
            priority_tier: CrossSectionChunkPriorityTier::VisibleActive,
            candidate_chunks: geometries.len(),
            visible_chunks: geometries
                .iter()
                .map(|geometry| geometry.key.clone())
                .collect(),
            visible_chunk_geometries: geometries,
        }
    }

    #[test]
    fn read_admissions_preserve_runtime_order_and_encode_worker_priority() {
        let mut runtime = CrossSectionRuntime::default();
        let first = key(0, 0, 1);
        let second = key(0, 0, 2);
        let outside_budget = key(0, 0, 3);
        runtime.apply_visible_chunk_plan(plan(
            PanelId::Xy,
            vec![
                geometry(outside_budget.clone(), -3.0),
                geometry(first.clone(), -1.0),
                geometry(second.clone(), -2.0),
            ],
        ));

        let admissions = cross_section_read_admissions_for_refresh(&runtime, [PanelId::Xy], 2);

        assert_eq!(admissions.len(), 2);
        assert_eq!(admissions[0].queue_entry.key, first);
        assert_eq!(admissions[0].worker_queue_priority, 2);
        assert_eq!(admissions[1].queue_entry.key, second);
        assert_eq!(admissions[1].worker_queue_priority, 1);
    }
}
