use std::{
    cmp::Ordering,
    collections::{BTreeMap, BinaryHeap, HashMap, HashSet},
};

use mirante4d_data::{
    BrickReadMetrics, BrickReadPayload, BrickReadTicket, BrickRequestPriority, DataRequestId,
    SpatialBrickIndex, VolumeBrickF32, VolumeBrickU8, VolumeBrickU16, VolumeRegion,
};
use mirante4d_domain::{TimeIndex, ViewerLayout};
use mirante4d_format::{DatasetId, LayerId};
use mirante4d_project_model::ViewState;
use mirante4d_render_api::PresentationViewport;
use mirante4d_renderer::{
    BrickGridSpec, CrossSectionPanelBounds, RenderViewport, cross_section_chunk_plane_polygon,
    plan_cross_section_bricks_with_diagnostics,
};

use crate::{
    current_runtime::dataset::CurrentDatasetRuntime,
    render_state::ResidentRenderFailureStatus,
    viewer_layout::{
        CrossSectionPanelScheduleReason, CrossSectionPanelScheduleState,
        CrossSectionPanelScheduleStatus, PanelId, PanelKind, render_cross_section_view_state,
    },
};

pub(crate) const CROSS_SECTION_RUNTIME_CPU_PAYLOAD_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CrossSectionChunkKey {
    pub(crate) dataset_id: DatasetId,
    pub(crate) layer_id: LayerId,
    pub(crate) timepoint: TimeIndex,
    pub(crate) scale_level: u32,
    pub(crate) brick_index: SpatialBrickIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum CrossSectionChunkState {
    Absent,
    Queued,
    Decoding,
    CpuResident,
    UploadQueued,
    GpuResident,
    Failed,
    Evicted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)]
pub(crate) enum CrossSectionChunkPriorityTier {
    VisibleActive,
    VisibleLinked,
    Refinement,
    Prefetch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CrossSectionVisibleChunkRequestKey {
    pub(crate) panel_id: PanelId,
    pub(crate) panel_generation: u64,
    pub(crate) layer_ids: Vec<String>,
    pub(crate) scale_level: u32,
    pub(crate) timepoint: TimeIndex,
    pub(crate) visible_chunk_count: usize,
    pub(crate) visible_chunk_fingerprint: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CrossSectionPanelBrickStreamState {
    pub(crate) request_key: CrossSectionVisibleChunkRequestKey,
    pub(crate) priority: BrickRequestPriority,
    pub(crate) active_panel_at_submission: Option<PanelId>,
    pub(crate) fairness_promoted: bool,
    pub(crate) requested: usize,
    pub(crate) queued_current_frame: usize,
    pub(crate) queued_prefetch: usize,
    pub(crate) deferred: usize,
    pub(crate) completed: usize,
    pub(crate) cancelled: usize,
    pub(crate) stale: usize,
    pub(crate) failed: usize,
    pub(crate) materialized_empty: usize,
    pub(crate) visible_chunks: usize,
    pub(crate) occupied_visible_chunks: usize,
    pub(crate) decoded_bytes: u64,
    pub(crate) encoded_payload_bytes_read: u64,
    pub(crate) last_error: Option<String>,
    pub(crate) complete: bool,
}

impl CrossSectionPanelBrickStreamState {
    pub(crate) fn new(
        request_key: CrossSectionVisibleChunkRequestKey,
        priority: BrickRequestPriority,
        active_panel_at_submission: Option<PanelId>,
        fairness_promoted: bool,
    ) -> Self {
        let visible_chunks = request_key.visible_chunk_count;
        Self {
            request_key,
            priority,
            active_panel_at_submission,
            fairness_promoted,
            requested: 0,
            queued_current_frame: 0,
            queued_prefetch: 0,
            deferred: 0,
            completed: 0,
            cancelled: 0,
            stale: 0,
            failed: 0,
            materialized_empty: 0,
            visible_chunks,
            occupied_visible_chunks: 0,
            decoded_bytes: 0,
            encoded_payload_bytes_read: 0,
            last_error: None,
            complete: visible_chunks == 0,
        }
    }

    pub(crate) fn active(&self) -> bool {
        self.completed
            .saturating_add(self.cancelled)
            .saturating_add(self.stale)
            .saturating_add(self.failed)
            < self.requested
    }

    pub(crate) fn refresh_complete(&mut self) {
        self.complete = self.completed >= self.requested.saturating_add(self.deferred)
            && self.cancelled == 0
            && self.stale == 0
            && self.failed == 0;
    }

    pub(crate) fn credit_completed_visible_chunks(&mut self, count: usize) {
        let remaining = self
            .requested
            .saturating_add(self.deferred)
            .saturating_sub(self.completed);
        self.completed = self.completed.saturating_add(count.min(remaining));
        self.refresh_complete();
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CrossSectionRuntimeEvictionStats {
    pub(crate) budget_bytes: u64,
    pub(crate) payload_bytes_before: u64,
    pub(crate) payload_bytes_after: u64,
    pub(crate) evicted_chunks: usize,
    pub(crate) evicted_bytes: u64,
    pub(crate) protected_visible_chunks: usize,
    pub(crate) protected_visible_bytes: u64,
    pub(crate) over_budget_after: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CrossSectionGpuResidencyReconcileStats {
    pub(crate) promoted: usize,
    pub(crate) demoted: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CrossSectionVisibleChunkGeometry {
    pub(crate) key: CrossSectionChunkKey,
    pub(crate) vertex_count: usize,
    pub(crate) panel_bounds: CrossSectionPanelBounds,
    pub(crate) priority_score: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CrossSectionVisibleChunkPlan {
    pub(crate) panel_id: PanelId,
    pub(crate) generation: u64,
    pub(crate) scale_level: u32,
    pub(crate) priority_tier: CrossSectionChunkPriorityTier,
    pub(crate) candidate_chunks: usize,
    pub(crate) visible_chunks: Vec<CrossSectionChunkKey>,
    pub(crate) visible_chunk_geometries: Vec<CrossSectionVisibleChunkGeometry>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CrossSectionVisiblePlanInput<'a> {
    pub(crate) view: &'a ViewState,
    pub(crate) active_panel: Option<PanelId>,
    pub(crate) layer_ids: &'a [LayerId],
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CrossSectionLayerInput<'a> {
    pub(crate) id: &'a LayerId,
    pub(crate) dtype: mirante4d_domain::IntensityDType,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct CrossSectionPanelViewRequest {
    generation: u64,
    view: mirante4d_renderer::CrossSectionView,
    presentation_viewport: PresentationViewport,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CrossSectionChunkSubmissionCandidate {
    pub(crate) key: CrossSectionChunkKey,
    pub(crate) panel_id: PanelId,
    pub(crate) panel_generation: u64,
    pub(crate) scale_level: u32,
    pub(crate) priority_tier: CrossSectionChunkPriorityTier,
    pub(crate) priority_score: f64,
}

#[derive(Debug, Clone)]
pub(crate) struct CrossSectionBrickReadTicket {
    pub(crate) panel_id: PanelId,
    pub(crate) panel_generation: u64,
    pub(crate) layer_id: String,
    pub(crate) scale_level: u32,
    pub(crate) timepoint: TimeIndex,
    pub(crate) brick_index: SpatialBrickIndex,
    pub(crate) region: VolumeRegion,
    pub(crate) ticket: BrickReadTicket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CrossSectionChunkQueueKind {
    DownloadPromotion,
    GpuPromotion,
    CpuEviction,
    GpuEviction,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CrossSectionChunkQueueEntry {
    pub(crate) key: CrossSectionChunkKey,
    pub(crate) panel_id: Option<PanelId>,
    pub(crate) panel_generation: Option<u64>,
    pub(crate) scale_level: u32,
    pub(crate) priority_tier: Option<CrossSectionChunkPriorityTier>,
    pub(crate) priority_score: f64,
    pub(crate) state: CrossSectionChunkState,
    pub(crate) bytes: u64,
    pub(crate) last_visible_generation: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct CrossSectionChunkQueue {
    entries: Vec<CrossSectionChunkQueueEntry>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct CrossSectionRuntimeQueues {
    pub(crate) revision: u64,
    pub(crate) download_promotions: CrossSectionChunkQueue,
    pub(crate) gpu_promotions: CrossSectionChunkQueue,
    pub(crate) cpu_evictions: CrossSectionChunkQueue,
    pub(crate) gpu_evictions: CrossSectionChunkQueue,
}

#[derive(Debug, Clone)]
pub(crate) enum CrossSectionChunkPayload {
    U8(Box<VolumeBrickU8>),
    U16(Box<VolumeBrickU16>),
    F32(Box<VolumeBrickF32>),
}

#[derive(Debug, Clone)]
pub(crate) struct CrossSectionChunkEntry {
    pub(crate) state: CrossSectionChunkState,
    pub(crate) priority_tier: Option<CrossSectionChunkPriorityTier>,
    pub(crate) priority_score: Option<f64>,
    pub(crate) last_visible_generation: Option<u64>,
    pub(crate) region: Option<VolumeRegion>,
    pub(crate) decoded_bytes: u64,
    pub(crate) encoded_payload_bytes_read: u64,
    pub(crate) payload: Option<CrossSectionChunkPayload>,
    pub(crate) last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CrossSectionPanelRuntime {
    pub(crate) panel_id: PanelId,
    pub(crate) kind: PanelKind,
    pub(crate) presentation_viewport: Option<PresentationViewport>,
    pub(crate) render_viewport: Option<RenderViewport>,
    pub(crate) generation: u64,
    pub(crate) displayed_generation: Option<u64>,
    pub(crate) cross_section_schedule: Option<CrossSectionPanelScheduleState>,
    pub(crate) render_failure: Option<ResidentRenderFailureStatus>,
    pub(crate) scale_level: u32,
    pub(crate) priority_tier: CrossSectionChunkPriorityTier,
    pub(crate) candidate_chunks: usize,
    pub(crate) visible_chunks: Vec<CrossSectionChunkKey>,
    pub(crate) visible_chunk_geometries: Vec<CrossSectionVisibleChunkGeometry>,
}

#[derive(Debug)]
pub(crate) struct CrossSectionRuntime {
    pub(crate) chunks: HashMap<CrossSectionChunkKey, CrossSectionChunkEntry>,
    pub(crate) panels: BTreeMap<PanelId, CrossSectionPanelRuntime>,
    pub(crate) panel_streams: BTreeMap<PanelId, CrossSectionPanelBrickStreamState>,
    pub(crate) read_tickets: Vec<CrossSectionBrickReadTicket>,
    pub(crate) queues: CrossSectionRuntimeQueues,
    pub(crate) cpu_payload_budget_bytes: u64,
    pub(crate) cpu_payload_eviction_passes: u64,
    pub(crate) cpu_payload_evicted_chunks: u64,
    pub(crate) cpu_payload_evicted_bytes: u64,
    pub(crate) cpu_payload_last_eviction: CrossSectionRuntimeEvictionStats,
}

impl Clone for CrossSectionRuntime {
    fn clone(&self) -> Self {
        Self {
            chunks: self.chunks.clone(),
            panels: self.panels.clone(),
            panel_streams: self.panel_streams.clone(),
            read_tickets: Vec::new(),
            queues: self.queues.clone(),
            cpu_payload_budget_bytes: self.cpu_payload_budget_bytes,
            cpu_payload_eviction_passes: self.cpu_payload_eviction_passes,
            cpu_payload_evicted_chunks: self.cpu_payload_evicted_chunks,
            cpu_payload_evicted_bytes: self.cpu_payload_evicted_bytes,
            cpu_payload_last_eviction: self.cpu_payload_last_eviction,
        }
    }
}

impl Default for CrossSectionRuntime {
    fn default() -> Self {
        Self {
            chunks: HashMap::new(),
            panels: [PanelId::Xy, PanelId::Xz, PanelId::ThreeD, PanelId::Yz]
                .into_iter()
                .map(|panel_id| (panel_id, CrossSectionPanelRuntime::new(panel_id)))
                .collect(),
            panel_streams: BTreeMap::new(),
            read_tickets: Vec::new(),
            queues: CrossSectionRuntimeQueues::default(),
            cpu_payload_budget_bytes: CROSS_SECTION_RUNTIME_CPU_PAYLOAD_BUDGET_BYTES,
            cpu_payload_eviction_passes: 0,
            cpu_payload_evicted_chunks: 0,
            cpu_payload_evicted_bytes: 0,
            cpu_payload_last_eviction: CrossSectionRuntimeEvictionStats::default(),
        }
    }
}

impl CrossSectionChunkQueue {
    pub(crate) fn entries(&self) -> &[CrossSectionChunkQueueEntry] {
        &self.entries
    }

    fn from_entries(
        kind: CrossSectionChunkQueueKind,
        entries: impl IntoIterator<Item = CrossSectionChunkQueueEntry>,
    ) -> Self {
        let mut heap = BinaryHeap::new();
        for entry in entries {
            heap.push(CrossSectionChunkQueueHeapItem { kind, entry });
        }

        let mut entries = Vec::with_capacity(heap.len());
        while let Some(item) = heap.pop() {
            entries.push(item.entry);
        }
        Self { entries }
    }
}

impl CrossSectionPanelRuntime {
    fn new(panel_id: PanelId) -> Self {
        Self {
            panel_id,
            kind: panel_id.kind(),
            presentation_viewport: None,
            render_viewport: None,
            generation: 0,
            displayed_generation: None,
            cross_section_schedule: panel_id
                .cross_section_panel()
                .map(|_| CrossSectionPanelScheduleState::missing_viewport(0)),
            render_failure: None,
            scale_level: 0,
            priority_tier: CrossSectionChunkPriorityTier::Prefetch,
            candidate_chunks: 0,
            visible_chunks: Vec::new(),
            visible_chunk_geometries: Vec::new(),
        }
    }

    pub(crate) fn display_current(&self) -> bool {
        self.displayed_generation == Some(self.generation)
    }

    fn record_viewports(
        &mut self,
        presentation_viewport: PresentationViewport,
        render_viewport: RenderViewport,
    ) -> bool {
        if self.presentation_viewport == Some(presentation_viewport)
            && self.render_viewport == Some(render_viewport)
        {
            return false;
        }
        self.presentation_viewport = Some(presentation_viewport);
        self.render_viewport = Some(render_viewport);
        self.generation = self.generation.saturating_add(1);
        self.displayed_generation = None;
        self.render_failure = None;
        self.clear_visible_plan();
        if let Some(schedule) = self.cross_section_schedule.as_mut() {
            *schedule = CrossSectionPanelScheduleState::missing_viewport(self.generation);
        }
        true
    }

    fn mark_displayed(&mut self, generation: u64) -> bool {
        if generation != self.generation {
            return false;
        }
        self.displayed_generation = Some(generation);
        self.render_failure = None;
        true
    }

    fn mark_dirty(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.render_failure = None;
        self.visible_chunks.clear();
        self.visible_chunk_geometries.clear();
        self.candidate_chunks = 0;
        if let Some(schedule) = self.cross_section_schedule.as_mut() {
            schedule.generation = self.generation;
            schedule.status = crate::viewer_layout::CrossSectionPanelScheduleStatus::Loading;
            schedule.reason =
                crate::viewer_layout::CrossSectionPanelScheduleReason::ResidentFramePending;
        }
    }

    fn set_schedule(&mut self, schedule: CrossSectionPanelScheduleState) -> bool {
        if self.panel_id.cross_section_panel().is_none() || schedule.generation != self.generation {
            return false;
        }
        self.cross_section_schedule = Some(schedule);
        true
    }

    fn mark_render_failed(
        &mut self,
        generation: u64,
        failure: ResidentRenderFailureStatus,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.render_failure = Some(failure);
        true
    }

    fn clear_visible_plan(&mut self) {
        self.visible_chunks.clear();
        self.visible_chunk_geometries.clear();
        self.candidate_chunks = 0;
        self.priority_tier = CrossSectionChunkPriorityTier::Prefetch;
    }
}

#[derive(Debug, Clone)]
struct CrossSectionChunkQueueHeapItem {
    kind: CrossSectionChunkQueueKind,
    entry: CrossSectionChunkQueueEntry,
}

impl PartialEq for CrossSectionChunkQueueHeapItem {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && cross_section_queue_entry_order(self.kind, &self.entry, &other.entry).is_eq()
    }
}

impl Eq for CrossSectionChunkQueueHeapItem {}

impl PartialOrd for CrossSectionChunkQueueHeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CrossSectionChunkQueueHeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        debug_assert_eq!(self.kind, other.kind);
        cross_section_queue_entry_order(self.kind, &self.entry, &other.entry).reverse()
    }
}

impl CrossSectionRuntime {
    pub(crate) fn panels(&self) -> impl ExactSizeIterator<Item = &CrossSectionPanelRuntime> {
        self.panels.values()
    }

    pub(crate) fn panel(&self, panel_id: PanelId) -> Option<&CrossSectionPanelRuntime> {
        self.panels.get(&panel_id)
    }

    pub(crate) fn record_panel_viewports(
        &mut self,
        panel_id: PanelId,
        presentation_viewport: PresentationViewport,
        render_viewport: RenderViewport,
    ) -> bool {
        self.panels
            .get_mut(&panel_id)
            .is_some_and(|panel| panel.record_viewports(presentation_viewport, render_viewport))
    }

    pub(crate) fn mark_panel_displayed(&mut self, panel_id: PanelId, generation: u64) -> bool {
        self.panels
            .get_mut(&panel_id)
            .is_some_and(|panel| panel.mark_displayed(generation))
    }

    pub(crate) fn set_panel_schedule(
        &mut self,
        panel_id: PanelId,
        schedule: CrossSectionPanelScheduleState,
    ) -> bool {
        self.panels
            .get_mut(&panel_id)
            .is_some_and(|panel| panel.set_schedule(schedule))
    }

    pub(crate) fn mark_panel_render_failed(
        &mut self,
        panel_id: PanelId,
        generation: u64,
        failure: ResidentRenderFailureStatus,
    ) -> bool {
        self.panels
            .get_mut(&panel_id)
            .is_some_and(|panel| panel.mark_render_failed(generation, failure))
    }

    pub(crate) fn clear_render_failures_after_residency_change(&mut self) -> bool {
        let mut changed = false;
        for panel in self.panels.values_mut() {
            if panel.render_failure.take().is_none() {
                continue;
            }
            if let Some(schedule) = panel.cross_section_schedule.as_mut()
                && schedule.reason == CrossSectionPanelScheduleReason::RenderFailed
            {
                schedule.status = CrossSectionPanelScheduleStatus::Loading;
                schedule.reason = CrossSectionPanelScheduleReason::ResidentFramePending;
            }
            changed = true;
        }
        changed
    }

    pub(crate) fn mark_cross_section_panels_dirty(&mut self) -> bool {
        let mut changed = false;
        for panel in self.panels.values_mut() {
            if panel.panel_id.cross_section_panel().is_some() {
                panel.mark_dirty();
                changed = true;
            }
        }
        if changed {
            self.recompute_chunk_priorities_from_panels();
            self.rebuild_queues();
        }
        changed
    }

    pub(crate) fn apply_visible_chunk_plan(&mut self, plan: CrossSectionVisibleChunkPlan) -> bool {
        if self
            .panels
            .get(&plan.panel_id)
            .is_some_and(|panel| plan.generation < panel.generation)
        {
            return false;
        }

        let Some(panel) = self.panels.get_mut(&plan.panel_id) else {
            return false;
        };
        if panel.generation != plan.generation {
            return false;
        }
        panel.scale_level = plan.scale_level;
        panel.priority_tier = plan.priority_tier;
        panel.candidate_chunks = plan.candidate_chunks;
        panel.visible_chunks = plan.visible_chunks;
        panel.visible_chunk_geometries = plan.visible_chunk_geometries;
        self.recompute_chunk_priorities_from_panels();
        self.rebuild_queues();
        true
    }

    pub(crate) fn clear_visible_work(&mut self) {
        for panel in self.panels.values_mut() {
            panel.clear_visible_plan();
        }
        self.panel_streams.clear();
        self.cancel_read_tickets();
        for entry in self.chunks.values_mut() {
            if matches!(
                entry.state,
                CrossSectionChunkState::Queued | CrossSectionChunkState::Decoding
            ) {
                entry.state = CrossSectionChunkState::Absent;
                entry.payload = None;
                entry.last_error = None;
            }
            entry.priority_tier = None;
            entry.priority_score = None;
        }
        self.rebuild_queues();
    }

    pub(crate) fn has_visible_work(&self) -> bool {
        self.panels
            .values()
            .any(|panel| !panel.visible_chunks.is_empty())
    }

    pub(crate) fn pending_read_ticket_count(&self) -> usize {
        self.read_tickets.len()
    }

    pub(crate) fn has_live_read_ticket(
        &self,
        chunk_key: &CrossSectionChunkKey,
        region: VolumeRegion,
    ) -> bool {
        self.read_tickets.iter().any(|ticket| {
            ticket.layer_id == chunk_key.layer_id.as_str()
                && ticket.scale_level == chunk_key.scale_level
                && ticket.timepoint == chunk_key.timepoint
                && ticket.brick_index == chunk_key.brick_index
                && ticket.region == region
        })
    }

    pub(crate) fn register_read_ticket(
        &mut self,
        chunk_key: CrossSectionChunkKey,
        region: VolumeRegion,
        ticket: CrossSectionBrickReadTicket,
    ) -> bool {
        let changed = self.mark_chunk_decoding(chunk_key, region);
        self.read_tickets.push(ticket);
        changed
    }

    pub(crate) fn take_read_ticket_for_request(
        &mut self,
        request_id: DataRequestId,
    ) -> Option<CrossSectionBrickReadTicket> {
        let position = self
            .read_tickets
            .iter()
            .position(|ticket| ticket.ticket.request_id == request_id)?;
        Some(self.read_tickets.swap_remove(position))
    }

    pub(crate) fn cancel_read_tickets(&mut self) {
        let tickets = std::mem::take(&mut self.read_tickets);
        for ticket in tickets {
            self.cancel_read_ticket(ticket);
        }
    }

    pub(crate) fn cancel_read_ticket(&mut self, ticket: CrossSectionBrickReadTicket) {
        ticket.ticket.cancel();
        if let Some(key) = self.chunk_key_for_read_ticket(&ticket) {
            self.mark_chunk_not_resident(&key);
        }
    }

    pub(crate) fn panel_submission_candidates(
        &self,
        panel_id: PanelId,
    ) -> Vec<CrossSectionChunkSubmissionCandidate> {
        let Some(panel) = self.panels.get(&panel_id) else {
            return Vec::new();
        };
        let mut candidates = panel_submission_candidates(panel_id, panel);
        candidates.sort_by(cross_section_submission_candidate_order);
        candidates
    }

    pub(crate) fn submission_candidates_for_panels(
        &self,
        panel_ids: impl IntoIterator<Item = PanelId>,
    ) -> Vec<CrossSectionChunkSubmissionCandidate> {
        let mut candidates_by_key: HashMap<
            CrossSectionChunkKey,
            CrossSectionChunkSubmissionCandidate,
        > = HashMap::new();
        for panel_id in panel_ids {
            let Some(panel) = self.panels.get(&panel_id) else {
                continue;
            };
            for candidate in panel_submission_candidates(panel_id, panel) {
                candidates_by_key
                    .entry(candidate.key.clone())
                    .and_modify(|existing| {
                        if cross_section_submission_candidate_order(&candidate, existing).is_lt() {
                            *existing = candidate.clone();
                        }
                    })
                    .or_insert(candidate);
            }
        }

        let mut candidates = candidates_by_key.into_values().collect::<Vec<_>>();
        candidates.sort_by(cross_section_submission_candidate_order);
        candidates
    }

    pub(crate) fn queued_panel_order_for_panels(
        &self,
        panel_ids: impl IntoIterator<Item = PanelId>,
    ) -> Vec<PanelId> {
        let allowed_panels = panel_ids.into_iter().collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        let mut ordered = Vec::new();
        for queue in [
            &self.queues.download_promotions,
            &self.queues.gpu_promotions,
        ] {
            for entry in queue.entries() {
                let Some(panel_id) = entry.panel_id else {
                    continue;
                };
                if allowed_panels.contains(&panel_id) && seen.insert(panel_id) {
                    ordered.push(panel_id);
                }
            }
        }
        ordered
    }

    #[cfg(test)]
    pub(crate) fn download_promotion_entries_for_panel(
        &self,
        panel_id: PanelId,
    ) -> Vec<CrossSectionChunkQueueEntry> {
        self.queues
            .download_promotions
            .entries()
            .iter()
            .filter(|entry| entry.panel_id == Some(panel_id))
            .cloned()
            .collect()
    }

    pub(crate) fn download_promotion_entries_for_panels(
        &self,
        panel_ids: impl IntoIterator<Item = PanelId>,
    ) -> Vec<CrossSectionChunkQueueEntry> {
        let allowed_panels = panel_ids.into_iter().collect::<HashSet<_>>();
        self.queues
            .download_promotions
            .entries()
            .iter()
            .filter(|entry| {
                entry
                    .panel_id
                    .is_some_and(|panel_id| allowed_panels.contains(&panel_id))
            })
            .cloned()
            .collect()
    }

    fn chunk_key_for_read_ticket(
        &self,
        ticket: &CrossSectionBrickReadTicket,
    ) -> Option<CrossSectionChunkKey> {
        self.chunks
            .iter()
            .find(|(key, entry)| {
                ticket.layer_id == key.layer_id.as_str()
                    && ticket.scale_level == key.scale_level
                    && ticket.timepoint == key.timepoint
                    && ticket.brick_index == key.brick_index
                    && entry.region == Some(ticket.region)
            })
            .map(|(key, _)| key)
            .cloned()
    }

    pub(crate) fn resident_payload_bytes(&self) -> u64 {
        self.chunks
            .values()
            .filter_map(|entry| entry.resident_payload_bytes())
            .fold(0u64, u64::saturating_add)
    }

    pub(crate) fn enforce_cpu_payload_budget(&mut self) -> CrossSectionRuntimeEvictionStats {
        self.rebuild_queues();
        let budget_bytes = self.cpu_payload_budget_bytes;
        let payload_bytes_before = self.resident_payload_bytes();
        let visible_keys = self.current_visible_chunk_keys();
        let mut protected_visible_chunks = 0usize;
        let mut protected_visible_bytes = 0u64;
        for key in &visible_keys {
            if let Some(bytes) = self
                .chunks
                .get(key)
                .and_then(CrossSectionChunkEntry::resident_payload_bytes)
            {
                protected_visible_chunks = protected_visible_chunks.saturating_add(1);
                protected_visible_bytes = protected_visible_bytes.saturating_add(bytes);
            }
        }

        let mut stats = CrossSectionRuntimeEvictionStats {
            budget_bytes,
            payload_bytes_before,
            protected_visible_chunks,
            protected_visible_bytes,
            ..CrossSectionRuntimeEvictionStats::default()
        };

        if payload_bytes_before <= budget_bytes {
            stats.payload_bytes_after = payload_bytes_before;
            stats.over_budget_after = payload_bytes_before > budget_bytes;
            self.cpu_payload_last_eviction = stats;
            return stats;
        }

        let candidates = self.queues.cpu_evictions.entries().to_vec();

        let mut remaining_bytes = payload_bytes_before;
        for candidate in candidates {
            if remaining_bytes <= budget_bytes {
                break;
            }
            let Some(entry) = self.chunks.get_mut(&candidate.key) else {
                continue;
            };
            let Some(bytes) = entry.resident_payload_bytes() else {
                continue;
            };
            entry.state = CrossSectionChunkState::Evicted;
            entry.payload = None;
            entry.last_error = None;
            remaining_bytes = remaining_bytes.saturating_sub(bytes);
            stats.evicted_chunks = stats.evicted_chunks.saturating_add(1);
            stats.evicted_bytes = stats.evicted_bytes.saturating_add(bytes);
        }

        stats.payload_bytes_after = remaining_bytes;
        stats.over_budget_after = remaining_bytes > budget_bytes;
        if stats.evicted_chunks > 0 {
            self.cpu_payload_eviction_passes = self.cpu_payload_eviction_passes.saturating_add(1);
            self.cpu_payload_evicted_chunks = self
                .cpu_payload_evicted_chunks
                .saturating_add(stats.evicted_chunks as u64);
            self.cpu_payload_evicted_bytes = self
                .cpu_payload_evicted_bytes
                .saturating_add(stats.evicted_bytes);
        }
        self.cpu_payload_last_eviction = stats;
        self.rebuild_queues();
        stats
    }

    pub(crate) fn has_cpu_resident_chunk(
        &self,
        key: &CrossSectionChunkKey,
        region: VolumeRegion,
    ) -> bool {
        self.chunks.get(key).is_some_and(|entry| {
            matches!(
                entry.state,
                CrossSectionChunkState::CpuResident
                    | CrossSectionChunkState::UploadQueued
                    | CrossSectionChunkState::GpuResident
            ) && entry.region == Some(region)
                && entry.payload.is_some()
        })
    }

    pub(crate) fn has_pending_chunk(
        &self,
        key: &CrossSectionChunkKey,
        region: VolumeRegion,
    ) -> bool {
        self.chunks.get(key).is_some_and(|entry| {
            matches!(
                entry.state,
                CrossSectionChunkState::Queued | CrossSectionChunkState::Decoding
            ) && entry.region == Some(region)
        })
    }

    #[cfg(test)]
    pub(crate) fn mark_chunk_queued(
        &mut self,
        key: CrossSectionChunkKey,
        region: VolumeRegion,
    ) -> bool {
        self.mark_chunk_pending(key, region, CrossSectionChunkState::Queued)
    }

    pub(crate) fn mark_chunk_decoding(
        &mut self,
        key: CrossSectionChunkKey,
        region: VolumeRegion,
    ) -> bool {
        self.mark_chunk_pending(key, region, CrossSectionChunkState::Decoding)
    }

    fn mark_chunk_pending(
        &mut self,
        key: CrossSectionChunkKey,
        region: VolumeRegion,
        pending_state: CrossSectionChunkState,
    ) -> bool {
        debug_assert!(matches!(
            pending_state,
            CrossSectionChunkState::Queued | CrossSectionChunkState::Decoding
        ));
        let entry = self
            .chunks
            .entry(key)
            .or_insert_with(CrossSectionChunkEntry::absent);
        if matches!(
            entry.state,
            CrossSectionChunkState::CpuResident
                | CrossSectionChunkState::UploadQueued
                | CrossSectionChunkState::GpuResident
        ) && entry.region == Some(region)
        {
            return false;
        }
        let changed = entry.state != pending_state || entry.region != Some(region);
        entry.state = pending_state;
        entry.region = Some(region);
        entry.last_error = None;
        if changed {
            self.rebuild_queues();
        }
        changed
    }

    pub(crate) fn mark_panel_resident_chunks_upload_queued(
        &mut self,
        panel_id: PanelId,
        generation: u64,
    ) -> usize {
        let Some(panel) = self.panels.get(&panel_id) else {
            return 0;
        };
        if panel.generation != generation {
            return 0;
        }
        let mut changed = 0usize;
        for key in &panel.visible_chunks {
            let Some(entry) = self.chunks.get_mut(key) else {
                continue;
            };
            if entry.state != CrossSectionChunkState::CpuResident || entry.payload.is_none() {
                continue;
            }
            entry.state = CrossSectionChunkState::UploadQueued;
            changed = changed.saturating_add(1);
        }
        if changed > 0 {
            self.rebuild_queues();
        }
        changed
    }

    pub(crate) fn reconcile_panel_chunks_with_renderer_gpu_residency(
        &mut self,
        panel_id: PanelId,
        generation: u64,
        renderer_gpu_resident_chunks: &HashSet<CrossSectionChunkKey>,
    ) -> CrossSectionGpuResidencyReconcileStats {
        let Some(panel) = self.panels.get(&panel_id) else {
            return CrossSectionGpuResidencyReconcileStats::default();
        };
        if panel.generation != generation {
            return CrossSectionGpuResidencyReconcileStats::default();
        }
        let mut stats = CrossSectionGpuResidencyReconcileStats::default();
        for key in &panel.visible_chunks {
            let Some(entry) = self.chunks.get_mut(key) else {
                continue;
            };
            if entry.payload.is_none() {
                continue;
            }
            let renderer_retained = renderer_gpu_resident_chunks.contains(key);
            if renderer_retained
                && matches!(
                    entry.state,
                    CrossSectionChunkState::CpuResident | CrossSectionChunkState::UploadQueued
                )
            {
                entry.state = CrossSectionChunkState::GpuResident;
                stats.promoted = stats.promoted.saturating_add(1);
                continue;
            }
            if !renderer_retained
                && matches!(
                    entry.state,
                    CrossSectionChunkState::UploadQueued | CrossSectionChunkState::GpuResident
                )
            {
                entry.state = CrossSectionChunkState::CpuResident;
                stats.demoted = stats.demoted.saturating_add(1);
            }
        }
        if stats.promoted > 0 || stats.demoted > 0 {
            self.rebuild_queues();
        }
        stats
    }

    pub(crate) fn restore_panel_upload_queued_chunks_to_cpu_resident(
        &mut self,
        panel_id: PanelId,
        generation: u64,
    ) -> usize {
        let Some(panel) = self.panels.get(&panel_id) else {
            return 0;
        };
        if panel.generation != generation {
            return 0;
        }
        let mut changed = 0usize;
        for key in &panel.visible_chunks {
            let Some(entry) = self.chunks.get_mut(key) else {
                continue;
            };
            if entry.state != CrossSectionChunkState::UploadQueued {
                continue;
            }
            entry.state = CrossSectionChunkState::CpuResident;
            changed = changed.saturating_add(1);
        }
        if changed > 0 {
            self.rebuild_queues();
        }
        changed
    }

    pub(crate) fn mark_chunk_cpu_resident_from_payload(
        &mut self,
        key: CrossSectionChunkKey,
        payload: &BrickReadPayload,
        metrics: BrickReadMetrics,
    ) -> anyhow::Result<bool> {
        let Some(payload) = CrossSectionChunkPayload::from_read_payload(payload) else {
            anyhow::bail!("grouped payload reached global cross-section chunk runtime");
        };
        let region = payload.region();
        let entry = self
            .chunks
            .entry(key)
            .or_insert_with(CrossSectionChunkEntry::absent);
        let changed =
            entry.state != CrossSectionChunkState::CpuResident || entry.region != Some(region);
        entry.state = CrossSectionChunkState::CpuResident;
        entry.region = Some(region);
        entry.decoded_bytes = entry
            .decoded_bytes
            .saturating_add(metrics.decoded_brick_bytes);
        entry.encoded_payload_bytes_read = entry
            .encoded_payload_bytes_read
            .saturating_add(metrics.encoded_payload_bytes_read);
        entry.payload = Some(payload);
        entry.last_error = None;
        self.rebuild_queues();
        Ok(changed)
    }

    pub(crate) fn mark_chunk_not_resident(&mut self, key: &CrossSectionChunkKey) -> bool {
        let Some(entry) = self.chunks.get_mut(key) else {
            return false;
        };
        if !matches!(
            entry.state,
            CrossSectionChunkState::Queued | CrossSectionChunkState::Decoding
        ) {
            return false;
        }
        entry.state = CrossSectionChunkState::Absent;
        entry.payload = None;
        self.rebuild_queues();
        true
    }

    pub(crate) fn mark_chunk_failed(&mut self, key: CrossSectionChunkKey, message: String) -> bool {
        let entry = self
            .chunks
            .entry(key)
            .or_insert_with(CrossSectionChunkEntry::absent);
        if matches!(
            entry.state,
            CrossSectionChunkState::CpuResident
                | CrossSectionChunkState::UploadQueued
                | CrossSectionChunkState::GpuResident
        ) {
            return false;
        }
        let changed = entry.state != CrossSectionChunkState::Failed
            || entry.last_error.as_ref() != Some(&message);
        entry.state = CrossSectionChunkState::Failed;
        entry.payload = None;
        entry.last_error = Some(message);
        if changed {
            self.rebuild_queues();
        }
        changed
    }

    fn current_visible_chunk_keys(&self) -> HashSet<CrossSectionChunkKey> {
        self.panels
            .values()
            .flat_map(|panel| panel.visible_chunks.iter().cloned())
            .collect()
    }

    fn recompute_chunk_priorities_from_panels(&mut self) {
        for entry in self.chunks.values_mut() {
            entry.priority_tier = None;
            entry.priority_score = None;
        }

        for panel in self.panels.values() {
            let geometry_priority_scores = panel
                .visible_chunk_geometries
                .iter()
                .map(|geometry| (geometry.key.clone(), geometry.priority_score))
                .collect::<HashMap<_, _>>();
            for key in &panel.visible_chunks {
                let entry = self
                    .chunks
                    .entry(key.clone())
                    .or_insert_with(CrossSectionChunkEntry::absent);
                let priority_score = geometry_priority_scores.get(key).copied().unwrap_or(0.0);
                match entry.priority_tier {
                    None => {
                        entry.priority_tier = Some(panel.priority_tier);
                        entry.priority_score = Some(priority_score);
                    }
                    Some(existing) if panel.priority_tier < existing => {
                        entry.priority_tier = Some(panel.priority_tier);
                        entry.priority_score = Some(priority_score);
                    }
                    Some(existing) if panel.priority_tier == existing => {
                        entry.priority_score = Some(
                            entry
                                .priority_score
                                .unwrap_or(f64::NEG_INFINITY)
                                .max(priority_score),
                        );
                    }
                    Some(_) => {}
                }
                entry.last_visible_generation = Some(
                    entry
                        .last_visible_generation
                        .map_or(panel.generation, |generation| {
                            generation.max(panel.generation)
                        }),
                );
            }
        }
    }

    fn rebuild_queues(&mut self) {
        let next_revision = self.queues.revision.saturating_add(1);
        let mut download_promotions = Vec::new();
        let mut gpu_promotions = Vec::new();
        for candidate in self.submission_candidates_for_panels(self.panels.keys().copied()) {
            let Some(entry) = self.chunks.get(&candidate.key) else {
                continue;
            };
            let queue_entry = CrossSectionChunkQueueEntry {
                key: candidate.key.clone(),
                panel_id: Some(candidate.panel_id),
                panel_generation: Some(candidate.panel_generation),
                scale_level: candidate.scale_level,
                priority_tier: Some(candidate.priority_tier),
                priority_score: candidate.priority_score,
                state: entry.state,
                bytes: entry.resident_payload_bytes().unwrap_or(0),
                last_visible_generation: entry.last_visible_generation,
            };
            match entry.state {
                CrossSectionChunkState::Absent
                | CrossSectionChunkState::Evicted
                | CrossSectionChunkState::Failed => {
                    download_promotions.push(queue_entry);
                }
                CrossSectionChunkState::CpuResident if entry.payload.is_some() => {
                    gpu_promotions.push(queue_entry);
                }
                CrossSectionChunkState::Queued
                | CrossSectionChunkState::Decoding
                | CrossSectionChunkState::CpuResident
                | CrossSectionChunkState::UploadQueued
                | CrossSectionChunkState::GpuResident => {}
            }
        }

        let visible_keys = self.current_visible_chunk_keys();
        let mut cpu_evictions = Vec::new();
        let mut gpu_evictions = Vec::new();
        for (key, entry) in &self.chunks {
            if visible_keys.contains(key) {
                continue;
            }
            let queue_entry = CrossSectionChunkQueueEntry {
                key: key.clone(),
                panel_id: None,
                panel_generation: None,
                scale_level: key.scale_level,
                priority_tier: entry.priority_tier,
                priority_score: entry.priority_score.unwrap_or(f64::NEG_INFINITY),
                state: entry.state,
                bytes: entry.resident_payload_bytes().unwrap_or(0),
                last_visible_generation: entry.last_visible_generation,
            };
            if entry.state != CrossSectionChunkState::UploadQueued
                && entry.resident_payload_bytes().is_some()
            {
                cpu_evictions.push(queue_entry.clone());
            }
            if entry.state == CrossSectionChunkState::GpuResident {
                gpu_evictions.push(queue_entry);
            }
        }

        self.queues = CrossSectionRuntimeQueues {
            revision: next_revision,
            download_promotions: CrossSectionChunkQueue::from_entries(
                CrossSectionChunkQueueKind::DownloadPromotion,
                download_promotions,
            ),
            gpu_promotions: CrossSectionChunkQueue::from_entries(
                CrossSectionChunkQueueKind::GpuPromotion,
                gpu_promotions,
            ),
            cpu_evictions: CrossSectionChunkQueue::from_entries(
                CrossSectionChunkQueueKind::CpuEviction,
                cpu_evictions,
            ),
            gpu_evictions: CrossSectionChunkQueue::from_entries(
                CrossSectionChunkQueueKind::GpuEviction,
                gpu_evictions,
            ),
        };
    }
}

fn panel_submission_candidates(
    panel_id: PanelId,
    panel: &CrossSectionPanelRuntime,
) -> Vec<CrossSectionChunkSubmissionCandidate> {
    let geometry_priority_scores = panel
        .visible_chunk_geometries
        .iter()
        .map(|geometry| (geometry.key.clone(), geometry.priority_score))
        .collect::<HashMap<_, _>>();
    panel
        .visible_chunks
        .iter()
        .map(|key| CrossSectionChunkSubmissionCandidate {
            key: key.clone(),
            panel_id,
            panel_generation: panel.generation,
            scale_level: panel.scale_level,
            priority_tier: panel.priority_tier,
            priority_score: geometry_priority_scores.get(key).copied().unwrap_or(0.0),
        })
        .collect()
}

pub(crate) fn cross_section_submission_candidate_order(
    left: &CrossSectionChunkSubmissionCandidate,
    right: &CrossSectionChunkSubmissionCandidate,
) -> std::cmp::Ordering {
    cross_section_submission_tier_rank(left.priority_tier)
        .cmp(&cross_section_submission_tier_rank(right.priority_tier))
        .then_with(|| right.priority_score.total_cmp(&left.priority_score))
        .then_with(|| cross_section_chunk_key_order(&left.key, &right.key))
        .then_with(|| left.panel_id.cmp(&right.panel_id))
        .then_with(|| left.panel_generation.cmp(&right.panel_generation))
}

pub(crate) fn cross_section_chunk_key_order(
    left: &CrossSectionChunkKey,
    right: &CrossSectionChunkKey,
) -> std::cmp::Ordering {
    (
        left.dataset_id.to_string(),
        left.layer_id.to_string(),
        left.timepoint.get(),
        left.scale_level,
        left.brick_index.z,
        left.brick_index.y,
        left.brick_index.x,
    )
        .cmp(&(
            right.dataset_id.to_string(),
            right.layer_id.to_string(),
            right.timepoint.get(),
            right.scale_level,
            right.brick_index.z,
            right.brick_index.y,
            right.brick_index.x,
        ))
}

fn cross_section_queue_entry_order(
    kind: CrossSectionChunkQueueKind,
    left: &CrossSectionChunkQueueEntry,
    right: &CrossSectionChunkQueueEntry,
) -> Ordering {
    match kind {
        CrossSectionChunkQueueKind::DownloadPromotion
        | CrossSectionChunkQueueKind::GpuPromotion => {
            cross_section_queue_promotion_entry_order(left, right)
        }
        CrossSectionChunkQueueKind::CpuEviction | CrossSectionChunkQueueKind::GpuEviction => {
            cross_section_queue_eviction_entry_order(left, right)
        }
    }
}

fn cross_section_queue_promotion_entry_order(
    left: &CrossSectionChunkQueueEntry,
    right: &CrossSectionChunkQueueEntry,
) -> Ordering {
    cross_section_optional_submission_tier_rank(left.priority_tier)
        .cmp(&cross_section_optional_submission_tier_rank(
            right.priority_tier,
        ))
        .then_with(|| right.priority_score.total_cmp(&left.priority_score))
        .then_with(|| cross_section_chunk_key_order(&left.key, &right.key))
        .then_with(|| left.panel_id.cmp(&right.panel_id))
        .then_with(|| left.panel_generation.cmp(&right.panel_generation))
}

fn cross_section_queue_eviction_entry_order(
    left: &CrossSectionChunkQueueEntry,
    right: &CrossSectionChunkQueueEntry,
) -> Ordering {
    eviction_priority_rank(left.priority_tier)
        .cmp(&eviction_priority_rank(right.priority_tier))
        .then_with(|| {
            left.last_visible_generation
                .unwrap_or(0)
                .cmp(&right.last_visible_generation.unwrap_or(0))
        })
        .then_with(|| right.bytes.cmp(&left.bytes))
        .then_with(|| cross_section_chunk_key_order(&left.key, &right.key))
}

fn cross_section_submission_tier_rank(priority_tier: CrossSectionChunkPriorityTier) -> u8 {
    match priority_tier {
        CrossSectionChunkPriorityTier::VisibleActive => 0,
        CrossSectionChunkPriorityTier::VisibleLinked => 1,
        CrossSectionChunkPriorityTier::Refinement => 2,
        CrossSectionChunkPriorityTier::Prefetch => 3,
    }
}

fn cross_section_optional_submission_tier_rank(
    priority_tier: Option<CrossSectionChunkPriorityTier>,
) -> u8 {
    priority_tier
        .map(cross_section_submission_tier_rank)
        .unwrap_or(u8::MAX)
}

impl CrossSectionChunkEntry {
    fn absent() -> Self {
        Self {
            state: CrossSectionChunkState::Absent,
            priority_tier: None,
            priority_score: None,
            last_visible_generation: None,
            region: None,
            decoded_bytes: 0,
            encoded_payload_bytes_read: 0,
            payload: None,
            last_error: None,
        }
    }

    fn resident_payload_bytes(&self) -> Option<u64> {
        if !matches!(
            self.state,
            CrossSectionChunkState::CpuResident
                | CrossSectionChunkState::UploadQueued
                | CrossSectionChunkState::GpuResident
        ) {
            return None;
        }
        self.payload
            .as_ref()
            .map(CrossSectionChunkPayload::decoded_bytes)
    }
}

impl CrossSectionChunkPayload {
    fn from_read_payload(payload: &BrickReadPayload) -> Option<Self> {
        match payload {
            BrickReadPayload::U8(brick) => Some(Self::U8(Box::new((**brick).clone()))),
            BrickReadPayload::U16(brick) => Some(Self::U16(Box::new((**brick).clone()))),
            BrickReadPayload::F32(brick) => Some(Self::F32(Box::new((**brick).clone()))),
            BrickReadPayload::Group(_) => None,
        }
    }

    fn region(&self) -> VolumeRegion {
        match self {
            Self::U8(brick) => brick.region,
            Self::U16(brick) => brick.region,
            Self::F32(brick) => brick.region,
        }
    }

    pub(crate) fn decoded_bytes(&self) -> u64 {
        match self {
            Self::U8(brick) => cross_section_region_decoded_bytes(&brick.region, 1),
            Self::U16(brick) => cross_section_region_decoded_bytes(&brick.region, 2),
            Self::F32(brick) => cross_section_region_decoded_bytes(&brick.region, 4),
        }
    }
}

fn eviction_priority_rank(priority_tier: Option<CrossSectionChunkPriorityTier>) -> u8 {
    match priority_tier {
        None => 0,
        Some(CrossSectionChunkPriorityTier::Prefetch) => 1,
        Some(CrossSectionChunkPriorityTier::Refinement) => 2,
        Some(CrossSectionChunkPriorityTier::VisibleLinked) => 3,
        Some(CrossSectionChunkPriorityTier::VisibleActive) => 4,
    }
}

fn cross_section_region_decoded_bytes(region: &VolumeRegion, bytes_per_voxel: u64) -> u64 {
    region
        .shape()
        .and_then(|shape| shape.element_count())
        .map(|values| values.saturating_mul(bytes_per_voxel))
        .unwrap_or(0)
}

pub(crate) fn plan_cross_section_visible_chunks(
    dataset: &CurrentDatasetRuntime,
    runtime: &CrossSectionRuntime,
    input: CrossSectionVisiblePlanInput<'_>,
    panel_id: PanelId,
    scale_level: u32,
) -> anyhow::Result<CrossSectionVisibleChunkPlan> {
    let request = cross_section_panel_view_request(runtime, input.view, panel_id)?;
    let dataset_id = dataset.dataset.dataset_id().clone();
    let priority_tier = cross_section_priority_tier_for_panel(input.active_panel, panel_id);
    let mut visible_chunks = Vec::new();
    let mut visible_chunk_geometries = Vec::new();
    let mut candidate_chunks = 0usize;

    for layer_id in input.layer_ids {
        let brick_shape = dataset
            .dataset
            .brick_shape_at_scale(layer_id, scale_level)?;
        let scale_shape = dataset.dataset.scale_shape(layer_id, scale_level)?;
        let grid_to_world = dataset.dataset.scale_grid_to_world(layer_id, scale_level)?;
        let spec = BrickGridSpec {
            volume_shape: scale_shape,
            brick_shape,
            grid_to_world,
        };
        let plan = plan_cross_section_bricks_with_diagnostics(
            request.view.slab(request.presentation_viewport),
            spec,
        );
        candidate_chunks = candidate_chunks.saturating_add(plan.candidate_bricks);
        for brick_index in plan.selected_bricks {
            let Some(polygon) = cross_section_chunk_plane_polygon(
                request.view,
                request.presentation_viewport,
                spec,
                brick_index,
            ) else {
                continue;
            };
            let key = CrossSectionChunkKey {
                dataset_id: dataset_id.clone(),
                layer_id: layer_id.clone(),
                timepoint: input.view.timepoint(),
                scale_level,
                brick_index,
            };
            visible_chunks.push(key.clone());
            visible_chunk_geometries.push(CrossSectionVisibleChunkGeometry {
                key,
                vertex_count: polygon.vertices.len(),
                panel_bounds: polygon.panel_bounds,
                priority_score: cross_section_chunk_priority_score(
                    polygon.panel_bounds,
                    request.presentation_viewport,
                ),
            });
        }
    }

    Ok(CrossSectionVisibleChunkPlan {
        panel_id,
        generation: request.generation,
        scale_level,
        priority_tier,
        candidate_chunks,
        visible_chunks,
        visible_chunk_geometries,
    })
}

fn cross_section_panel_view_request(
    runtime: &CrossSectionRuntime,
    view: &ViewState,
    panel_id: PanelId,
) -> anyhow::Result<CrossSectionPanelViewRequest> {
    if view.layout() != ViewerLayout::FourPanel {
        anyhow::bail!("cross-section rendering requires the four-panel layout");
    }
    let cross_section_panel = panel_id
        .cross_section_panel()
        .ok_or_else(|| anyhow::anyhow!("the 3D panel is not a cross-section target"))?;
    let panel = runtime.panel(panel_id).ok_or_else(|| {
        anyhow::anyhow!("cross-section panel {} is unavailable", panel_id.label())
    })?;
    let presentation_viewport = panel.presentation_viewport.ok_or_else(|| {
        anyhow::anyhow!(
            "cross-section panel {} has no presentation viewport",
            panel_id.label()
        )
    })?;
    Ok(CrossSectionPanelViewRequest {
        generation: panel.generation,
        view: render_cross_section_view_state(*view.cross_section()).view(cross_section_panel),
        presentation_viewport,
    })
}

fn cross_section_chunk_priority_score(
    panel_bounds: CrossSectionPanelBounds,
    viewport: PresentationViewport,
) -> f64 {
    let center = (panel_bounds.min_points + panel_bounds.max_points) * 0.5;
    let viewport_center = glam::DVec2::new(
        viewport.width_points() * 0.5,
        viewport.height_points() * 0.5,
    );
    -center.distance(viewport_center)
}

fn cross_section_priority_tier_for_panel(
    active_panel: Option<PanelId>,
    panel_id: PanelId,
) -> CrossSectionChunkPriorityTier {
    match active_panel {
        Some(active_panel) if active_panel == panel_id => {
            CrossSectionChunkPriorityTier::VisibleActive
        }
        Some(_) => CrossSectionChunkPriorityTier::VisibleLinked,
        None => CrossSectionChunkPriorityTier::VisibleActive,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use mirante4d_data::{
        BrickReadPayload, BrickReadTicket, CancellationToken, DataGenerationId, DataRequestId,
        DenseVolumeU16, SpatialBrickIndex, VolumeBrickU16, VolumeRegion,
    };
    use mirante4d_domain::{GridToWorld, Shape3D, TimeIndex};
    use mirante4d_format::BrickIndex;
    use mirante4d_format::{DatasetId, LayerId};

    use crate::{
        FrameFailureKind,
        cross_section_runtime::{
            CrossSectionBrickReadTicket, CrossSectionChunkKey, CrossSectionChunkPayload,
            CrossSectionChunkPriorityTier, CrossSectionChunkState, CrossSectionRuntime,
            CrossSectionVisibleChunkPlan,
        },
        render_state::ResidentRenderFailureStatus,
        viewer_layout::{
            CrossSectionPanelScheduleReason, CrossSectionPanelScheduleStatus, PanelId,
        },
    };

    fn key(timepoint: u64, z: u64, y: u64, x: u64) -> CrossSectionChunkKey {
        CrossSectionChunkKey {
            dataset_id: DatasetId::new("dataset").unwrap(),
            layer_id: LayerId::new("layer").unwrap(),
            timepoint: TimeIndex::new(timepoint),
            scale_level: 0,
            brick_index: SpatialBrickIndex::new(z, y, x),
        }
    }

    #[test]
    fn registering_live_read_ticket_marks_chunk_decoding_and_takes_by_request() {
        let mut runtime = CrossSectionRuntime::default();
        let chunk_key = key(0, 0, 0, 0);
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            1,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![chunk_key.clone()],
        )));
        let region = VolumeRegion::new(0, 0, 0, 1, 1, 1).unwrap();
        let ticket = CrossSectionBrickReadTicket {
            panel_id: PanelId::Xy,
            panel_generation: 1,
            layer_id: chunk_key.layer_id.to_string(),
            scale_level: chunk_key.scale_level,
            timepoint: chunk_key.timepoint,
            brick_index: chunk_key.brick_index,
            region,
            ticket: BrickReadTicket {
                request_id: DataRequestId(7),
                generation_id: DataGenerationId(3),
                scale_level: chunk_key.scale_level,
                cancellation: CancellationToken::new(),
            },
        };

        assert!(runtime.register_read_ticket(chunk_key.clone(), region, ticket));

        assert!(runtime.has_live_read_ticket(&chunk_key, region));
        assert_eq!(runtime.pending_read_ticket_count(), 1);
        assert_eq!(
            runtime.chunks[&chunk_key].state,
            CrossSectionChunkState::Decoding
        );
        assert!(runtime.queues.download_promotions.entries().is_empty());

        let taken = runtime.take_read_ticket_for_request(DataRequestId(7));

        assert!(taken.is_some());
        assert_eq!(runtime.pending_read_ticket_count(), 0);
        assert!(!runtime.has_live_read_ticket(&chunk_key, region));
    }

    #[test]
    fn cancelling_live_read_ticket_clears_decoding_chunk_state() {
        let mut runtime = CrossSectionRuntime::default();
        let chunk_key = key(0, 0, 0, 0);
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            1,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![chunk_key.clone()],
        )));
        let region = VolumeRegion::new(0, 0, 0, 1, 1, 1).unwrap();
        let cancellation = CancellationToken::new();
        let ticket = CrossSectionBrickReadTicket {
            panel_id: PanelId::Xy,
            panel_generation: 1,
            layer_id: chunk_key.layer_id.to_string(),
            scale_level: chunk_key.scale_level,
            timepoint: chunk_key.timepoint,
            brick_index: chunk_key.brick_index,
            region,
            ticket: BrickReadTicket {
                request_id: DataRequestId(9),
                generation_id: DataGenerationId(3),
                scale_level: chunk_key.scale_level,
                cancellation: cancellation.clone(),
            },
        };
        assert!(runtime.register_read_ticket(chunk_key.clone(), region, ticket));

        runtime.cancel_read_tickets();

        assert!(cancellation.is_cancelled());
        assert_eq!(runtime.pending_read_ticket_count(), 0);
        assert_eq!(
            runtime.chunks[&chunk_key].state,
            CrossSectionChunkState::Absent
        );
        assert!(!runtime.has_live_read_ticket(&chunk_key, region));
        assert_eq!(runtime.queues.download_promotions.entries().len(), 1);
    }

    #[test]
    fn runtime_clone_does_not_duplicate_live_read_tickets() {
        let mut runtime = CrossSectionRuntime::default();
        runtime.read_tickets.push(CrossSectionBrickReadTicket {
            panel_id: PanelId::Xy,
            panel_generation: 1,
            layer_id: "layer".to_owned(),
            scale_level: 0,
            timepoint: TimeIndex::new(0),
            brick_index: SpatialBrickIndex::new(0, 0, 0),
            region: VolumeRegion::new(0, 0, 0, 1, 1, 1).unwrap(),
            ticket: BrickReadTicket {
                request_id: DataRequestId(1),
                generation_id: DataGenerationId(1),
                scale_level: 0,
                cancellation: CancellationToken::new(),
            },
        });

        let cloned = runtime.clone();

        assert_eq!(runtime.pending_read_ticket_count(), 1);
        assert_eq!(cloned.pending_read_ticket_count(), 0);
    }

    fn resident_u16_payload(key: &CrossSectionChunkKey) -> CrossSectionChunkPayload {
        let shape = Shape3D::new(1, 1, 1).unwrap();
        let volume = DenseVolumeU16::new(
            key.dataset_id.clone(),
            key.layer_id.clone(),
            key.scale_level,
            key.timepoint,
            shape,
            GridToWorld::identity(),
            vec![7],
        )
        .unwrap()
        .with_render_valid(vec![1])
        .unwrap();
        CrossSectionChunkPayload::U16(Box::new(VolumeBrickU16 {
            scale_level: key.scale_level,
            brick_index: key.brick_index,
            chunk_index: BrickIndex {
                t: key.timepoint.get(),
                z: key.brick_index.z,
                y: key.brick_index.y,
                x: key.brick_index.x,
            },
            region: VolumeRegion::new(0, 0, 0, 1, 1, 1).unwrap(),
            occupied: true,
            valid_voxel_count: 1,
            min: 7.0,
            max: 7.0,
            volume,
        }))
    }

    fn resident_u16_read_payload(key: &CrossSectionChunkKey) -> BrickReadPayload {
        match resident_u16_payload(key) {
            CrossSectionChunkPayload::U16(brick) => BrickReadPayload::U16(brick),
            _ => unreachable!(),
        }
    }

    fn plan(
        panel_id: PanelId,
        generation: u64,
        priority_tier: CrossSectionChunkPriorityTier,
        visible_chunks: Vec<CrossSectionChunkKey>,
    ) -> CrossSectionVisibleChunkPlan {
        CrossSectionVisibleChunkPlan {
            panel_id,
            generation,
            scale_level: 0,
            priority_tier,
            candidate_chunks: visible_chunks.len(),
            visible_chunks,
            visible_chunk_geometries: Vec::new(),
        }
    }

    impl CrossSectionRuntime {
        fn apply_test_visible_plan(&mut self, plan: CrossSectionVisibleChunkPlan) -> bool {
            let panel = self.panels.get_mut(&plan.panel_id).unwrap();
            while panel.generation < plan.generation {
                panel.mark_dirty();
            }
            self.apply_visible_chunk_plan(plan)
        }
    }

    #[test]
    fn chunk_identity_includes_timepoint() {
        assert_ne!(key(0, 0, 0, 0), key(1, 0, 0, 0));
    }

    #[test]
    fn panel_render_failure_suppresses_same_generation_until_residency_changes() {
        let mut runtime = CrossSectionRuntime::default();
        let generation = runtime.panel(PanelId::Xy).unwrap().generation;
        assert!(runtime.mark_panel_render_failed(
            PanelId::Xy,
            generation,
            ResidentRenderFailureStatus::new(
                FrameFailureKind::BudgetExceeded,
                "cross-section GPU budget exceeded",
            ),
        ));
        let panel = runtime.panel(PanelId::Xy).unwrap();
        assert_eq!(
            panel.render_failure.as_ref().map(|failure| failure.kind),
            Some(FrameFailureKind::BudgetExceeded)
        );

        let schedule = runtime
            .panels
            .get_mut(&PanelId::Xy)
            .unwrap()
            .cross_section_schedule
            .as_mut()
            .unwrap();
        schedule.status = CrossSectionPanelScheduleStatus::Unavailable;
        schedule.reason = CrossSectionPanelScheduleReason::RenderFailed;
        assert!(runtime.clear_render_failures_after_residency_change());
        let panel = runtime.panel(PanelId::Xy).unwrap();
        assert!(panel.render_failure.is_none());
        assert_eq!(
            panel.cross_section_schedule.unwrap().reason,
            CrossSectionPanelScheduleReason::ResidentFramePending
        );
        assert!(!runtime.clear_render_failures_after_residency_change());
    }

    #[test]
    fn visible_plan_creates_global_chunk_entries_without_panel_ownership() {
        let mut runtime = CrossSectionRuntime::default();
        let visible = vec![key(0, 0, 0, 0), key(0, 0, 0, 1)];

        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            7,
            CrossSectionChunkPriorityTier::VisibleActive,
            visible.clone(),
        )));

        assert_eq!(runtime.panels[&PanelId::Xy].visible_chunks, visible);
        assert_eq!(runtime.chunks.len(), 2);
        assert!(
            runtime
                .chunks
                .values()
                .all(|entry| entry.state == CrossSectionChunkState::Absent
                    && entry.priority_tier == Some(CrossSectionChunkPriorityTier::VisibleActive)
                    && entry.last_visible_generation == Some(7))
        );
    }

    #[test]
    fn non_current_panel_generation_does_not_replace_visible_set() {
        let mut runtime = CrossSectionRuntime::default();
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            8,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![key(0, 0, 0, 0)],
        )));

        assert!(!runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            7,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![key(0, 0, 0, 1)],
        )));
        assert!(!runtime.apply_visible_chunk_plan(plan(
            PanelId::Xy,
            9,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![key(0, 0, 0, 2)],
        )));

        assert_eq!(runtime.panels[&PanelId::Xy].generation, 8);
        assert_eq!(
            runtime.panels[&PanelId::Xy].visible_chunks,
            vec![key(0, 0, 0, 0)]
        );
    }

    #[test]
    fn linked_panel_priority_does_not_downgrade_active_visible_chunk() {
        let mut runtime = CrossSectionRuntime::default();
        let shared = key(0, 0, 0, 0);

        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            1,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![shared.clone()],
        )));
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xz,
            1,
            CrossSectionChunkPriorityTier::VisibleLinked,
            vec![shared.clone()],
        )));

        assert_eq!(
            runtime.chunks[&shared].priority_tier,
            Some(CrossSectionChunkPriorityTier::VisibleActive)
        );
    }

    #[test]
    fn queue_state_tracks_download_and_gpu_promotions() {
        let mut runtime = CrossSectionRuntime::default();
        let missing_near = key(0, 0, 0, 1);
        let missing_far = key(0, 0, 0, 0);
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            1,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![missing_far.clone(), missing_near.clone()],
        )));

        let download_keys = runtime
            .queues
            .download_promotions
            .entries()
            .iter()
            .map(|entry| entry.key.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            download_keys,
            vec![missing_far.clone(), missing_near.clone()]
        );
        assert!(runtime.queues.gpu_promotions.entries().is_empty());

        let payload = resident_u16_read_payload(&missing_far);
        runtime
            .mark_chunk_cpu_resident_from_payload(missing_far.clone(), &payload, Default::default())
            .unwrap();

        let download_keys = runtime
            .queues
            .download_promotions
            .entries()
            .iter()
            .map(|entry| entry.key.clone())
            .collect::<Vec<_>>();
        let gpu_keys = runtime
            .queues
            .gpu_promotions
            .entries()
            .iter()
            .map(|entry| entry.key.clone())
            .collect::<Vec<_>>();

        assert_eq!(download_keys, vec![missing_near]);
        assert_eq!(gpu_keys, vec![missing_far]);
    }

    #[test]
    fn queue_state_removes_queued_chunks_from_download_promotions() {
        let mut runtime = CrossSectionRuntime::default();
        let visible = key(0, 0, 0, 0);
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            1,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![visible.clone()],
        )));
        let region = VolumeRegion::new(0, 0, 0, 1, 1, 1).unwrap();

        assert_eq!(runtime.queues.download_promotions.entries().len(), 1);
        assert!(runtime.mark_chunk_queued(visible.clone(), region));

        assert!(runtime.queues.download_promotions.entries().is_empty());
        assert!(runtime.queues.gpu_promotions.entries().is_empty());
        assert_eq!(
            runtime.chunks[&visible].state,
            CrossSectionChunkState::Queued
        );
    }

    #[test]
    fn download_promotion_entries_are_panel_scoped_and_state_filtered() {
        let mut runtime = CrossSectionRuntime::default();
        let active_missing = key(0, 0, 0, 0);
        let linked_missing = key(0, 0, 0, 1);
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            1,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![active_missing.clone()],
        )));
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xz,
            1,
            CrossSectionChunkPriorityTier::VisibleLinked,
            vec![linked_missing.clone()],
        )));

        let active_entries = runtime.download_promotion_entries_for_panel(PanelId::Xy);
        let linked_entries = runtime.download_promotion_entries_for_panel(PanelId::Xz);
        assert_eq!(
            active_entries
                .iter()
                .map(|entry| entry.key.clone())
                .collect::<Vec<_>>(),
            vec![active_missing.clone()]
        );
        assert_eq!(
            linked_entries
                .iter()
                .map(|entry| entry.key.clone())
                .collect::<Vec<_>>(),
            vec![linked_missing.clone()]
        );

        let region = VolumeRegion::new(0, 0, 0, 1, 1, 1).unwrap();
        assert!(runtime.mark_chunk_queued(active_missing, region));

        assert!(
            runtime
                .download_promotion_entries_for_panel(PanelId::Xy)
                .is_empty()
        );
        assert_eq!(
            runtime
                .download_promotion_entries_for_panel(PanelId::Xz)
                .into_iter()
                .map(|entry| entry.key)
                .collect::<Vec<_>>(),
            vec![linked_missing]
        );
    }

    #[test]
    fn queued_panel_order_uses_runtime_download_promotions() {
        let mut runtime = CrossSectionRuntime::default();
        let active_missing = key(0, 0, 0, 0);
        let linked_missing = key(0, 0, 0, 1);
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xz,
            1,
            CrossSectionChunkPriorityTier::VisibleLinked,
            vec![linked_missing],
        )));
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            1,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![active_missing],
        )));

        assert_eq!(
            runtime.queued_panel_order_for_panels([PanelId::Xz, PanelId::Xy]),
            vec![PanelId::Xy, PanelId::Xz]
        );
    }

    #[test]
    fn queue_state_moves_stale_gpu_resident_chunks_to_eviction_queues() {
        let mut runtime = CrossSectionRuntime::default();
        let stale = key(0, 0, 0, 0);
        let current = key(0, 0, 0, 1);
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            1,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![stale.clone()],
        )));
        let payload = resident_u16_read_payload(&stale);
        runtime
            .mark_chunk_cpu_resident_from_payload(stale.clone(), &payload, Default::default())
            .unwrap();
        assert_eq!(
            runtime.mark_panel_resident_chunks_upload_queued(PanelId::Xy, 1),
            1
        );
        let stats = runtime.reconcile_panel_chunks_with_renderer_gpu_residency(
            PanelId::Xy,
            1,
            &HashSet::from([stale.clone()]),
        );
        assert_eq!(stats.promoted, 1);

        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            2,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![current],
        )));

        let cpu_eviction_keys = runtime
            .queues
            .cpu_evictions
            .entries()
            .iter()
            .map(|entry| entry.key.clone())
            .collect::<Vec<_>>();
        let gpu_eviction_keys = runtime
            .queues
            .gpu_evictions
            .entries()
            .iter()
            .map(|entry| entry.key.clone())
            .collect::<Vec<_>>();

        assert_eq!(cpu_eviction_keys, vec![stale.clone()]);
        assert_eq!(gpu_eviction_keys, vec![stale]);
    }

    #[test]
    fn replaced_visible_plan_clears_stale_chunk_priority() {
        let mut runtime = CrossSectionRuntime::default();
        let stale = key(0, 0, 0, 0);
        let current = key(0, 0, 0, 1);

        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            1,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![stale.clone()],
        )));
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            2,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![current.clone()],
        )));

        assert_eq!(runtime.chunks[&stale].priority_tier, None);
        assert_eq!(runtime.chunks[&stale].last_visible_generation, Some(1));
        assert_eq!(
            runtime.chunks[&current].priority_tier,
            Some(CrossSectionChunkPriorityTier::VisibleActive)
        );
        assert_eq!(runtime.chunks[&current].last_visible_generation, Some(2));
    }

    #[test]
    fn cpu_payload_budget_eviction_protects_current_visible_chunks() {
        let mut runtime = CrossSectionRuntime {
            cpu_payload_budget_bytes: 2,
            ..Default::default()
        };
        let stale = key(0, 0, 0, 0);
        let current = key(0, 0, 0, 1);

        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            1,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![stale.clone()],
        )));
        {
            let entry = runtime.chunks.get_mut(&stale).unwrap();
            entry.state = CrossSectionChunkState::CpuResident;
            entry.payload = Some(resident_u16_payload(&stale));
        }
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            2,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![current.clone()],
        )));
        {
            let entry = runtime.chunks.get_mut(&current).unwrap();
            entry.state = CrossSectionChunkState::CpuResident;
            entry.payload = Some(resident_u16_payload(&current));
        }

        let stats = runtime.enforce_cpu_payload_budget();

        assert_eq!(stats.payload_bytes_before, 4);
        assert_eq!(stats.payload_bytes_after, 2);
        assert_eq!(stats.evicted_chunks, 1);
        assert_eq!(stats.evicted_bytes, 2);
        assert_eq!(stats.protected_visible_chunks, 1);
        assert_eq!(stats.protected_visible_bytes, 2);
        assert!(!stats.over_budget_after);
        assert_eq!(runtime.cpu_payload_eviction_passes, 1);
        assert_eq!(runtime.cpu_payload_evicted_chunks, 1);
        assert_eq!(runtime.cpu_payload_evicted_bytes, 2);
        assert_eq!(
            runtime.chunks[&stale].state,
            CrossSectionChunkState::Evicted
        );
        assert!(runtime.chunks[&stale].payload.is_none());
        assert_eq!(
            runtime.chunks[&current].state,
            CrossSectionChunkState::CpuResident
        );
        assert!(runtime.chunks[&current].payload.is_some());
    }

    #[test]
    fn cpu_payload_budget_does_not_evict_current_visible_chunk() {
        let mut runtime = CrossSectionRuntime {
            cpu_payload_budget_bytes: 1,
            ..Default::default()
        };
        let current = key(0, 0, 0, 0);

        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            1,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![current.clone()],
        )));
        {
            let entry = runtime.chunks.get_mut(&current).unwrap();
            entry.state = CrossSectionChunkState::CpuResident;
            entry.payload = Some(resident_u16_payload(&current));
        }

        let stats = runtime.enforce_cpu_payload_budget();

        assert_eq!(stats.payload_bytes_before, 2);
        assert_eq!(stats.payload_bytes_after, 2);
        assert_eq!(stats.evicted_chunks, 0);
        assert_eq!(stats.protected_visible_chunks, 1);
        assert!(stats.over_budget_after);
        assert_eq!(
            runtime.chunks[&current].state,
            CrossSectionChunkState::CpuResident
        );
        assert!(runtime.chunks[&current].payload.is_some());
    }

    #[test]
    fn clearing_visible_work_stops_panel_priority_without_deleting_cache_entries() {
        let mut runtime = CrossSectionRuntime::default();
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            1,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![key(0, 0, 0, 0)],
        )));

        runtime.clear_visible_work();

        assert!(!runtime.has_visible_work());
        assert_eq!(runtime.chunks.len(), 1);
        assert!(
            runtime
                .chunks
                .values()
                .all(|entry| entry.priority_tier.is_none())
        );
    }

    #[test]
    fn clearing_visible_work_clears_orphan_pending_chunks_but_preserves_cache_entries() {
        let mut runtime = CrossSectionRuntime::default();
        let queued = key(0, 0, 0, 0);
        let decoding = key(0, 0, 0, 1);
        let resident = key(0, 0, 0, 2);
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            1,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![queued.clone(), decoding.clone(), resident.clone()],
        )));
        let region = VolumeRegion::new(0, 0, 0, 1, 1, 1).unwrap();
        assert!(runtime.mark_chunk_queued(queued.clone(), region));
        assert!(runtime.mark_chunk_decoding(decoding.clone(), region));
        {
            let entry = runtime.chunks.get_mut(&resident).unwrap();
            entry.state = CrossSectionChunkState::CpuResident;
            entry.region = Some(region);
            entry.payload = Some(resident_u16_payload(&resident));
        }

        runtime.clear_visible_work();

        assert_eq!(
            runtime.chunks[&queued].state,
            CrossSectionChunkState::Absent
        );
        assert_eq!(
            runtime.chunks[&decoding].state,
            CrossSectionChunkState::Absent
        );
        assert_eq!(
            runtime.chunks[&resident].state,
            CrossSectionChunkState::CpuResident
        );
        assert!(runtime.chunks[&resident].payload.is_some());
        assert_eq!(runtime.pending_read_ticket_count(), 0);
        assert!(!runtime.has_visible_work());
    }

    #[test]
    fn stale_ticket_cannot_clear_cpu_resident_chunk() {
        let mut runtime = CrossSectionRuntime::default();
        let key = key(0, 0, 0, 0);
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            1,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![key.clone()],
        )));
        runtime.chunks.get_mut(&key).unwrap().state = CrossSectionChunkState::CpuResident;

        assert!(!runtime.mark_chunk_not_resident(&key));
        assert_eq!(
            runtime.chunks[&key].state,
            CrossSectionChunkState::CpuResident
        );
    }

    #[test]
    fn panel_upload_and_gpu_residency_transitions_are_generation_guarded() {
        let mut runtime = CrossSectionRuntime::default();
        let current = key(0, 0, 0, 0);
        let missing = key(0, 0, 0, 1);
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            9,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![current.clone(), missing.clone()],
        )));
        {
            let entry = runtime.chunks.get_mut(&current).unwrap();
            entry.state = CrossSectionChunkState::CpuResident;
            entry.payload = Some(resident_u16_payload(&current));
        }

        assert_eq!(
            runtime.mark_panel_resident_chunks_upload_queued(PanelId::Xy, 8),
            0
        );
        assert_eq!(
            runtime.chunks[&current].state,
            CrossSectionChunkState::CpuResident
        );
        assert_eq!(
            runtime.mark_panel_resident_chunks_upload_queued(PanelId::Xy, 9),
            1
        );
        assert_eq!(
            runtime.chunks[&current].state,
            CrossSectionChunkState::UploadQueued
        );
        assert_eq!(
            runtime.chunks[&missing].state,
            CrossSectionChunkState::Absent
        );

        assert_eq!(
            runtime.restore_panel_upload_queued_chunks_to_cpu_resident(PanelId::Xy, 8),
            0
        );
        assert_eq!(
            runtime.chunks[&current].state,
            CrossSectionChunkState::UploadQueued
        );
        assert_eq!(
            runtime.restore_panel_upload_queued_chunks_to_cpu_resident(PanelId::Xy, 9),
            1
        );
        assert_eq!(
            runtime.chunks[&current].state,
            CrossSectionChunkState::CpuResident
        );

        assert_eq!(
            runtime.mark_panel_resident_chunks_upload_queued(PanelId::Xy, 9),
            1
        );
        let stale_reconcile = runtime.reconcile_panel_chunks_with_renderer_gpu_residency(
            PanelId::Xy,
            8,
            &HashSet::from([current.clone()]),
        );
        assert_eq!(stale_reconcile.promoted, 0);
        assert_eq!(
            runtime.chunks[&current].state,
            CrossSectionChunkState::UploadQueued
        );
        let current_reconcile = runtime.reconcile_panel_chunks_with_renderer_gpu_residency(
            PanelId::Xy,
            9,
            &HashSet::from([current.clone()]),
        );
        assert_eq!(current_reconcile.promoted, 1);
        assert_eq!(
            runtime.chunks[&current].state,
            CrossSectionChunkState::GpuResident
        );
    }

    #[test]
    fn panel_gpu_residency_reconcile_uses_renderer_retained_chunks() {
        let mut runtime = CrossSectionRuntime::default();
        let retained = key(0, 0, 0, 0);
        let not_retained = key(0, 0, 0, 1);
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            9,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![retained.clone(), not_retained.clone()],
        )));
        for key in [&retained, &not_retained] {
            let entry = runtime.chunks.get_mut(key).unwrap();
            entry.state = CrossSectionChunkState::CpuResident;
            entry.payload = Some(resident_u16_payload(key));
        }

        assert_eq!(
            runtime.mark_panel_resident_chunks_upload_queued(PanelId::Xy, 9),
            2
        );
        let renderer_chunks = HashSet::from([retained.clone()]);
        let stats = runtime.reconcile_panel_chunks_with_renderer_gpu_residency(
            PanelId::Xy,
            9,
            &renderer_chunks,
        );

        assert_eq!(stats.promoted, 1);
        assert_eq!(stats.demoted, 1);
        assert_eq!(
            runtime.chunks[&retained].state,
            CrossSectionChunkState::GpuResident
        );
        assert_eq!(
            runtime.chunks[&not_retained].state,
            CrossSectionChunkState::CpuResident
        );
    }

    #[test]
    fn stale_panel_gpu_residency_reconcile_is_ignored() {
        let mut runtime = CrossSectionRuntime::default();
        let retained = key(0, 0, 0, 0);
        assert!(runtime.apply_test_visible_plan(plan(
            PanelId::Xy,
            9,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![retained.clone()],
        )));
        {
            let entry = runtime.chunks.get_mut(&retained).unwrap();
            entry.state = CrossSectionChunkState::UploadQueued;
            entry.payload = Some(resident_u16_payload(&retained));
        }

        let stats = runtime.reconcile_panel_chunks_with_renderer_gpu_residency(
            PanelId::Xy,
            8,
            &HashSet::from([retained.clone()]),
        );

        assert_eq!(stats.promoted, 0);
        assert_eq!(stats.demoted, 0);
        assert_eq!(
            runtime.chunks[&retained].state,
            CrossSectionChunkState::UploadQueued
        );
    }
}
