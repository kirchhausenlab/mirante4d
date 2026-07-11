use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
};

use mirante4d_application::CrossSectionPanelId;
#[cfg(test)]
use mirante4d_data::BrickReadPool;
use mirante4d_data::{
    BrickMetadata, BrickReadOutcome, BrickReadPayload, BrickReadStatus, BrickRequestPriority,
    CancellationToken, DataGenerationId, DenseVolumeF32, DenseVolumeU8, DenseVolumeU16,
    SpatialBrickIndex, VolumeBrickF32, VolumeBrickU8, VolumeBrickU16,
    translated_region_grid_to_world,
};
use mirante4d_domain::{IntensityDType, TimeIndex, ViewerLayout};
use mirante4d_format::LayerId;
use mirante4d_project_model::ViewState;

use crate::{
    cross_section_read_queue::{
        CrossSectionChunkReadSubmission, CrossSectionReadBackend,
        cross_section_read_admissions_for_refresh,
    },
    cross_section_runtime::{
        CrossSectionBrickReadTicket, CrossSectionChunkKey, CrossSectionLayerInput,
        CrossSectionPanelBrickStreamState, CrossSectionRuntime, CrossSectionVisibleChunkRequestKey,
    },
    cross_section_scheduler::{CrossSectionScheduleInput, schedule_cross_section_panel},
    current_runtime::{dataset::CurrentDatasetRuntime, render::CurrentRenderRuntime},
    viewer_layout::PanelId,
};

pub(crate) const CROSS_SECTION_CHUNK_READ_SUBMISSIONS_PER_REFRESH: usize = 64;
#[cfg(test)]
pub(crate) const CROSS_SECTION_CHUNK_READ_SUBMISSIONS_PER_PANEL_CALL: usize =
    CROSS_SECTION_CHUNK_READ_SUBMISSIONS_PER_REFRESH;

#[derive(Debug, Clone)]
struct CrossSectionVisibleChunkStreamPlan {
    layer_id: LayerId,
    metadata: BrickMetadata,
}

#[derive(Debug)]
struct CrossSectionPreparedPanelSubmission {
    result: CrossSectionBrickSubmissionResult,
    stream: CrossSectionPanelBrickStreamState,
    chunk_stream_plans: HashMap<CrossSectionChunkKey, CrossSectionVisibleChunkStreamPlan>,
    missing_occupied_chunks: HashSet<CrossSectionChunkKey>,
    submitted_missing_chunks: HashSet<CrossSectionChunkKey>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct CrossSectionBrickSubmissionResult {
    pub(crate) request_changed: bool,
    pub(crate) queued: bool,
    pub(crate) queued_current_frame: usize,
    pub(crate) queued_prefetch: usize,
    pub(crate) fairness_promoted: bool,
    pub(crate) resident_changed: bool,
}

impl CrossSectionBrickSubmissionResult {
    fn absorb(&mut self, other: Self) {
        self.request_changed |= other.request_changed;
        self.queued |= other.queued;
        self.queued_current_frame = self
            .queued_current_frame
            .saturating_add(other.queued_current_frame);
        self.queued_prefetch = self.queued_prefetch.saturating_add(other.queued_prefetch);
        self.fairness_promoted |= other.fairness_promoted;
        self.resident_changed |= other.resident_changed;
    }
}

#[derive(Debug, Default)]
pub(crate) struct CrossSectionBrickOutcomePartition {
    pub(crate) unhandled: Vec<BrickReadOutcome>,
    pub(crate) resident_changed: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CrossSectionStreamingInput<'a> {
    pub(crate) view: &'a ViewState,
    pub(crate) active_layer_id: &'a LayerId,
    pub(crate) layers: &'a [CrossSectionLayerInput<'a>],
    pub(crate) active_panel: Option<CrossSectionPanelId>,
    pub(crate) gpu_budget_bytes: u64,
}

pub(crate) fn cross_section_panel_stream_work_active(
    runtime: &CrossSectionRuntime,
    panel_id: PanelId,
) -> bool {
    let Some(stream) = runtime.panel_streams.get(&panel_id) else {
        return false;
    };
    if stream.active() {
        return true;
    }
    if stream.complete {
        return false;
    }
    runtime
        .panel(panel_id)
        .and_then(|panel| panel.cross_section_schedule)
        .is_some_and(|schedule| schedule.missing_occupied_bricks > 0)
}

pub(crate) fn cross_section_runtime_work_active(runtime: &CrossSectionRuntime) -> bool {
    runtime
        .panel_streams
        .keys()
        .copied()
        .any(|panel_id| cross_section_panel_stream_work_active(runtime, panel_id))
}

pub(crate) fn cross_section_request_priority_for_panel(
    runtime: &CrossSectionRuntime,
    active_panel: Option<PanelId>,
    panel_id: PanelId,
) -> BrickRequestPriority {
    match active_panel {
        Some(active_panel) if active_panel == panel_id => BrickRequestPriority::CurrentFrame,
        Some(active_panel)
            if cross_section_inactive_panel_fairness_promoted(runtime, active_panel, panel_id) =>
        {
            BrickRequestPriority::CurrentFrame
        }
        Some(_) => BrickRequestPriority::Prefetch,
        None => BrickRequestPriority::CurrentFrame,
    }
}

pub(crate) fn cross_section_inactive_panel_fairness_promoted(
    runtime: &CrossSectionRuntime,
    active_panel: PanelId,
    panel_id: PanelId,
) -> bool {
    if active_panel == panel_id || panel_id.cross_section_panel().is_none() {
        return false;
    }
    let active_panel_has_current_work =
        runtime
            .panel_streams
            .get(&active_panel)
            .is_some_and(|stream| {
                stream.priority == BrickRequestPriority::CurrentFrame && stream.active()
            });
    if !active_panel_has_current_work {
        return false;
    }
    !runtime.panel_streams.iter().any(|(stream_panel, stream)| {
        *stream_panel != active_panel
            && *stream_panel != panel_id
            && stream.fairness_promoted
            && stream.active()
    })
}

pub(crate) fn retire_cross_section_streaming_state(render: &mut CurrentRenderRuntime) {
    render.cross_section_runtime.clear_visible_work();
}

#[cfg(test)]
pub(crate) fn submit_cross_section_panel_bricks_to_pool(
    dataset: &CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    input: CrossSectionStreamingInput<'_>,
    panel_id: PanelId,
    pool: &BrickReadPool,
) -> anyhow::Result<CrossSectionBrickSubmissionResult> {
    retire_tickets_before_generation(
        &mut render.cross_section_runtime,
        CrossSectionReadBackend::active_generation(pool),
    );
    submit_cross_section_panel_bricks_to_pool_with_budget(
        dataset,
        render,
        input,
        panel_id,
        pool,
        CROSS_SECTION_CHUNK_READ_SUBMISSIONS_PER_PANEL_CALL,
    )
}

#[cfg(test)]
pub(crate) fn submit_cross_section_visible_chunks_to_pool(
    dataset: &CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    input: CrossSectionStreamingInput<'_>,
    pool: &BrickReadPool,
) -> anyhow::Result<CrossSectionBrickSubmissionResult> {
    submit_cross_section_visible_chunks_to_read_queue(dataset, render, input, pool)
}

pub(crate) fn submit_cross_section_visible_chunks_to_read_queue(
    dataset: &CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    input: CrossSectionStreamingInput<'_>,
    read_queue: &impl CrossSectionReadBackend,
) -> anyhow::Result<CrossSectionBrickSubmissionResult> {
    if input.view.layout() != ViewerLayout::FourPanel {
        return Ok(CrossSectionBrickSubmissionResult::default());
    }

    retire_tickets_before_generation(
        &mut render.cross_section_runtime,
        read_queue.active_generation(),
    );

    let panel_order = cross_section_global_submission_panel_order(
        &render.cross_section_runtime,
        input.active_panel.map(PanelId::from_application_panel),
    );
    let panel_order =
        cross_section_runtime_panel_submission_order(&render.cross_section_runtime, panel_order);

    let mut prepared_by_panel = HashMap::new();
    for panel_id in &panel_order {
        let Some(prepared) = prepare_cross_section_panel_submission(
            dataset,
            &mut render.cross_section_runtime,
            input,
            *panel_id,
        )?
        else {
            continue;
        };
        prepared_by_panel.insert(*panel_id, prepared);
    }

    let admissions = cross_section_read_admissions_for_refresh(
        &render.cross_section_runtime,
        panel_order
            .iter()
            .copied()
            .filter(|panel_id| prepared_by_panel.contains_key(panel_id)),
        CROSS_SECTION_CHUNK_READ_SUBMISSIONS_PER_REFRESH,
    );

    for admission in admissions {
        let queue_entry = admission.queue_entry;
        let Some(panel_id) = queue_entry.panel_id else {
            continue;
        };
        let Some(prepared) = prepared_by_panel.get_mut(&panel_id) else {
            continue;
        };
        submit_prepared_cross_section_queue_entry(
            &mut render.cross_section_runtime,
            read_queue,
            queue_entry,
            admission.worker_queue_priority,
            prepared,
        )?;
    }

    let mut result = CrossSectionBrickSubmissionResult::default();
    for panel_id in panel_order {
        let Some(prepared) = prepared_by_panel.remove(&panel_id) else {
            continue;
        };
        result.absorb(finalize_prepared_cross_section_panel_submission(
            dataset, render, input, panel_id, prepared,
        ));
    }

    Ok(result)
}

#[cfg(test)]
fn submit_cross_section_panel_bricks_to_pool_with_budget(
    dataset: &CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    input: CrossSectionStreamingInput<'_>,
    panel_id: PanelId,
    pool: &BrickReadPool,
    missing_submission_budget: usize,
) -> anyhow::Result<CrossSectionBrickSubmissionResult> {
    let Some(mut prepared) = prepare_cross_section_panel_submission(
        dataset,
        &mut render.cross_section_runtime,
        input,
        panel_id,
    )?
    else {
        return Ok(CrossSectionBrickSubmissionResult::default());
    };
    let admissions = cross_section_read_admissions_for_refresh(
        &render.cross_section_runtime,
        [panel_id],
        missing_submission_budget,
    );
    for admission in admissions {
        submit_prepared_cross_section_queue_entry(
            &mut render.cross_section_runtime,
            pool,
            admission.queue_entry,
            admission.worker_queue_priority,
            &mut prepared,
        )?;
    }
    Ok(finalize_prepared_cross_section_panel_submission(
        dataset, render, input, panel_id, prepared,
    ))
}

fn prepare_cross_section_panel_submission(
    dataset: &CurrentDatasetRuntime,
    runtime: &mut CrossSectionRuntime,
    input: CrossSectionStreamingInput<'_>,
    panel_id: PanelId,
) -> anyhow::Result<Option<CrossSectionPreparedPanelSubmission>> {
    if input.view.layout() != ViewerLayout::FourPanel || panel_id.cross_section_panel().is_none() {
        return Ok(None);
    }

    let Some(panel) = runtime.panel(panel_id) else {
        return Ok(None);
    };
    let Some(schedule) = panel.cross_section_schedule else {
        return Ok(None);
    };
    let Some(scale_level) = schedule.render_scale_level.or(schedule.target_scale_level) else {
        return Ok(None);
    };
    let panel_generation = panel.generation;
    let layer_ids = input
        .layers
        .iter()
        .map(|layer| layer.id.clone())
        .collect::<Vec<_>>();
    if layer_ids.is_empty() {
        return Ok(None);
    }
    let Some(panel_chunks) = runtime.panels.get(&panel_id) else {
        return Ok(None);
    };
    if panel_chunks.generation != panel_generation || panel_chunks.scale_level != scale_level {
        return Ok(None);
    }
    let visible_chunks = runtime
        .panel_submission_candidates(panel_id)
        .into_iter()
        .map(|candidate| candidate.key)
        .collect::<Vec<_>>();
    let request_key = CrossSectionVisibleChunkRequestKey {
        panel_id,
        panel_generation,
        layer_ids: layer_ids.iter().map(ToString::to_string).collect(),
        scale_level,
        timepoint: input.view.timepoint(),
        visible_chunk_count: visible_chunks.len(),
        visible_chunk_fingerprint: cross_section_visible_chunk_fingerprint(&visible_chunks),
    };
    let active_panel_at_submission = input.active_panel.map(PanelId::from_application_panel);
    let fairness_promoted = active_panel_at_submission.is_some_and(|active_panel| {
        cross_section_inactive_panel_fairness_promoted(runtime, active_panel, panel_id)
    });
    let priority =
        cross_section_request_priority_for_panel(runtime, active_panel_at_submission, panel_id);

    let request_changed = runtime
        .panel_streams
        .get(&panel_id)
        .is_none_or(|existing| existing.request_key != request_key);
    if let Some(existing) = runtime.panel_streams.get(&panel_id)
        && existing.request_key == request_key
        && (existing.complete || (existing.active() && existing.priority == priority))
    {
        return Ok(None);
    }

    cancel_obsolete_panel_tickets(runtime, panel_id);

    let mut result = CrossSectionBrickSubmissionResult {
        request_changed,
        ..CrossSectionBrickSubmissionResult::default()
    };
    let mut stream = CrossSectionPanelBrickStreamState::new(
        request_key.clone(),
        priority,
        active_panel_at_submission,
        fairness_promoted,
    );
    result.fairness_promoted = fairness_promoted;
    let mut chunk_stream_plans = HashMap::new();
    let mut missing_occupied_chunks = HashSet::new();
    for chunk_key in &visible_chunks {
        if chunk_key.dataset_id != *dataset.dataset.dataset_id()
            || chunk_key.scale_level != request_key.scale_level
            || chunk_key.timepoint != request_key.timepoint
        {
            continue;
        }
        let layer = layer_for_id(input.layers, &chunk_key.layer_id)?;
        let metadata = dataset.dataset.brick_metadata_at_scale(
            &chunk_key.layer_id,
            request_key.scale_level,
            request_key.timepoint,
            chunk_key.brick_index,
        )?;
        stream.decoded_bytes = stream.decoded_bytes.saturating_add(
            metadata
                .region
                .shape()?
                .element_count()?
                .saturating_mul(dtype_decoded_bytes(layer.dtype)),
        );
        if metadata.occupied {
            stream.occupied_visible_chunks = stream.occupied_visible_chunks.saturating_add(1);
        }
        chunk_stream_plans.insert(
            chunk_key.clone(),
            CrossSectionVisibleChunkStreamPlan {
                layer_id: layer.id.clone(),
                metadata,
            },
        );
        if runtime.has_cpu_resident_chunk(chunk_key, metadata.region) {
            stream.requested = stream.requested.saturating_add(1);
            stream.completed = stream.completed.saturating_add(1);
            continue;
        }
        if runtime.has_pending_chunk(chunk_key, metadata.region) {
            if runtime.has_live_read_ticket(chunk_key, metadata.region) {
                stream.deferred = stream.deferred.saturating_add(1);
                continue;
            }
            runtime.mark_chunk_not_resident(chunk_key);
        }
        if !metadata.occupied {
            stream.requested = stream.requested.saturating_add(1);
            let region = metadata.region;
            let payload = zero_cross_section_brick_payload(
                dataset,
                &chunk_key.layer_id,
                request_key.timepoint,
                metadata,
                region,
                layer.dtype,
            )?;
            match runtime.mark_chunk_cpu_resident_from_payload(
                chunk_key.clone(),
                &payload,
                Default::default(),
            ) {
                Ok(changed) => {
                    result.resident_changed |= changed;
                }
                Err(err) => {
                    stream.failed = stream.failed.saturating_add(1);
                    stream.last_error = Some(err.to_string());
                    continue;
                }
            }
            stream.completed = stream.completed.saturating_add(1);
            stream.materialized_empty = stream.materialized_empty.saturating_add(1);
            continue;
        }
        missing_occupied_chunks.insert(chunk_key.clone());
    }

    Ok(Some(CrossSectionPreparedPanelSubmission {
        result,
        stream,
        chunk_stream_plans,
        missing_occupied_chunks,
        submitted_missing_chunks: HashSet::new(),
    }))
}

fn submit_prepared_cross_section_queue_entry(
    runtime: &mut CrossSectionRuntime,
    read_queue: &impl CrossSectionReadBackend,
    queue_entry: crate::cross_section_runtime::CrossSectionChunkQueueEntry,
    worker_queue_priority: i64,
    prepared: &mut CrossSectionPreparedPanelSubmission,
) -> anyhow::Result<()> {
    let Some(panel_id) = queue_entry.panel_id else {
        return Ok(());
    };
    let chunk_key = queue_entry.key;
    if !prepared.missing_occupied_chunks.contains(&chunk_key)
        || prepared.submitted_missing_chunks.contains(&chunk_key)
    {
        return Ok(());
    }
    let Some(chunk_plan) = prepared.chunk_stream_plans.get(&chunk_key) else {
        return Ok(());
    };
    if runtime.has_cpu_resident_chunk(&chunk_key, chunk_plan.metadata.region) {
        return Ok(());
    }
    if runtime.has_pending_chunk(&chunk_key, chunk_plan.metadata.region) {
        if runtime.has_live_read_ticket(&chunk_key, chunk_plan.metadata.region) {
            prepared.stream.deferred = prepared.stream.deferred.saturating_add(1);
            return Ok(());
        }
        runtime.mark_chunk_not_resident(&chunk_key);
    }

    let request_key = &prepared.stream.request_key;
    let priority = prepared.stream.priority;
    prepared.stream.requested = prepared.stream.requested.saturating_add(1);
    let cancellation = CancellationToken::new();
    match read_queue.submit_cross_section_chunk_read(
        read_queue.active_generation(),
        CrossSectionChunkReadSubmission::new(
            &chunk_key,
            priority,
            worker_queue_priority,
            cancellation,
        ),
    ) {
        Ok(ticket) => {
            prepared.submitted_missing_chunks.insert(chunk_key.clone());
            prepared.result.queued = true;
            match priority {
                BrickRequestPriority::CurrentFrame => {
                    prepared.stream.queued_current_frame =
                        prepared.stream.queued_current_frame.saturating_add(1);
                    prepared.result.queued_current_frame =
                        prepared.result.queued_current_frame.saturating_add(1);
                }
                BrickRequestPriority::Prefetch => {
                    prepared.stream.queued_prefetch =
                        prepared.stream.queued_prefetch.saturating_add(1);
                    prepared.result.queued_prefetch =
                        prepared.result.queued_prefetch.saturating_add(1);
                }
                BrickRequestPriority::Warm => {}
            }
            runtime.register_read_ticket(
                chunk_key.clone(),
                chunk_plan.metadata.region,
                CrossSectionBrickReadTicket {
                    panel_id,
                    panel_generation: request_key.panel_generation,
                    layer_id: chunk_plan.layer_id.to_string(),
                    scale_level: request_key.scale_level,
                    timepoint: request_key.timepoint,
                    brick_index: chunk_key.brick_index,
                    region: chunk_plan.metadata.region,
                    ticket,
                },
            );
        }
        Err(err) => {
            runtime.mark_chunk_failed(chunk_key.clone(), err.to_string());
            prepared.stream.failed = prepared.stream.failed.saturating_add(1);
            prepared.stream.last_error = Some(err.to_string());
        }
    }
    Ok(())
}

fn finalize_prepared_cross_section_panel_submission(
    dataset: &CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    input: CrossSectionStreamingInput<'_>,
    panel_id: PanelId,
    mut prepared: CrossSectionPreparedPanelSubmission,
) -> CrossSectionBrickSubmissionResult {
    for chunk_key in &prepared.missing_occupied_chunks {
        if prepared.submitted_missing_chunks.contains(chunk_key) {
            continue;
        }
        let Some(chunk_plan) = prepared.chunk_stream_plans.get(chunk_key) else {
            continue;
        };
        if render
            .cross_section_runtime
            .has_cpu_resident_chunk(chunk_key, chunk_plan.metadata.region)
        {
            continue;
        }
        if render
            .cross_section_runtime
            .has_pending_chunk(chunk_key, chunk_plan.metadata.region)
            && render
                .cross_section_runtime
                .has_live_read_ticket(chunk_key, chunk_plan.metadata.region)
        {
            prepared.stream.deferred = prepared.stream.deferred.saturating_add(1);
            continue;
        }
        prepared.stream.deferred = prepared.stream.deferred.saturating_add(1);
    }
    prepared.stream.refresh_complete();
    render
        .cross_section_runtime
        .panel_streams
        .insert(panel_id, prepared.stream);
    if prepared.result.resident_changed {
        let _ = render.cross_section_runtime.enforce_cpu_payload_budget();
        let _ = schedule_cross_section_panel(
            dataset,
            render,
            CrossSectionScheduleInput {
                view: input.view,
                active_layer_id: input.active_layer_id,
                layers: input.layers,
                active_panel: input.active_panel,
                gpu_budget_bytes: input.gpu_budget_bytes,
            },
            panel_id,
            true,
        );
    }
    prepared.result
}

fn cross_section_global_submission_panel_order(
    runtime: &CrossSectionRuntime,
    active_panel: Option<PanelId>,
) -> Vec<PanelId> {
    let active_panel_ready = cross_section_active_panel_visible_work_ready(runtime, active_panel);
    let mut panel_ids = runtime
        .panels
        .keys()
        .copied()
        .filter(|panel_id| panel_id.cross_section_panel().is_some())
        .filter(|panel_id| {
            active_panel_ready
                || active_panel.is_none()
                || active_panel == Some(*panel_id)
                || cross_section_request_priority_for_panel(runtime, active_panel, *panel_id)
                    == BrickRequestPriority::CurrentFrame
        })
        .collect::<Vec<_>>();
    panel_ids.sort_by_key(|panel_id| {
        let active_rank = if active_panel == Some(*panel_id) {
            0
        } else {
            1
        };
        let priority_tier = runtime
            .panels
            .get(panel_id)
            .map(|panel| panel.priority_tier)
            .unwrap_or(crate::cross_section_runtime::CrossSectionChunkPriorityTier::Prefetch);
        (active_rank, priority_tier, *panel_id)
    });
    panel_ids
}

fn cross_section_runtime_panel_submission_order(
    runtime: &CrossSectionRuntime,
    panel_order: Vec<PanelId>,
) -> Vec<PanelId> {
    let mut seen = HashSet::new();
    let mut ordered = Vec::with_capacity(panel_order.len());
    for panel_id in runtime.queued_panel_order_for_panels(panel_order.iter().copied()) {
        if seen.insert(panel_id) {
            ordered.push(panel_id);
        }
    }
    for candidate in runtime.submission_candidates_for_panels(panel_order.iter().copied()) {
        if seen.insert(candidate.panel_id) {
            ordered.push(candidate.panel_id);
        }
    }
    for panel_id in panel_order {
        if seen.insert(panel_id) {
            ordered.push(panel_id);
        }
    }
    ordered
}

fn cross_section_active_panel_visible_work_ready(
    runtime: &CrossSectionRuntime,
    active_panel: Option<PanelId>,
) -> bool {
    let Some(active_panel) = active_panel else {
        return true;
    };
    if active_panel.cross_section_panel().is_none() {
        return true;
    }
    let Some(panel) = runtime.panel(active_panel) else {
        return false;
    };
    let Some(schedule) = panel.cross_section_schedule else {
        return false;
    };
    let Some(scale_level) = schedule.render_scale_level.or(schedule.target_scale_level) else {
        return false;
    };
    runtime.panels.get(&active_panel).is_some_and(|chunks| {
        chunks.generation == panel.generation && chunks.scale_level == scale_level
    })
}

pub(crate) fn apply_cross_section_brick_read_outcomes(
    dataset: &CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    view: &ViewState,
    outcomes: Vec<BrickReadOutcome>,
) -> CrossSectionBrickOutcomePartition {
    let mut partition = CrossSectionBrickOutcomePartition::default();
    for outcome in outcomes {
        let Some(ticket) = render
            .cross_section_runtime
            .take_read_ticket_for_request(outcome.request_id)
        else {
            partition.unhandled.push(outcome);
            continue;
        };
        partition.resident_changed |= apply_cross_section_brick_read_outcome(
            dataset,
            &mut render.cross_section_runtime,
            view,
            ticket,
            outcome,
        );
    }
    partition
}

fn apply_cross_section_brick_read_outcome(
    dataset: &CurrentDatasetRuntime,
    runtime: &mut CrossSectionRuntime,
    view: &ViewState,
    ticket: CrossSectionBrickReadTicket,
    outcome: BrickReadOutcome,
) -> bool {
    let ticket_panel_generation_current =
        ticket_matches_current_panel_generation(runtime, view, &ticket);
    let ticket_chunk_still_visible = ticket_chunk_is_current_in_global_runtime(
        runtime,
        view,
        dataset.dataset.dataset_id(),
        &ticket,
    );
    if !ticket_panel_generation_current && !ticket_chunk_still_visible {
        if let Some(chunk_key) =
            cross_section_chunk_key_for_ticket(dataset.dataset.dataset_id(), &ticket)
        {
            runtime.mark_chunk_not_resident(&chunk_key);
        }
        return false;
    }
    let mut resident_changed = false;
    let mut completed = 0usize;
    let mut cancelled = 0usize;
    let mut stale = 0usize;
    let mut failed = 0usize;
    let mut last_error = None;
    let mut decoded_bytes = outcome.read_metrics.decoded_brick_bytes;
    let encoded_payload_bytes_read = outcome.read_metrics.encoded_payload_bytes_read;

    if !outcome_matches_cross_section_ticket(&ticket, &outcome) {
        failed = 1;
        last_error = Some("cross-section brick outcome did not match its ticket".to_owned());
        decoded_bytes = 0;
    } else {
        match outcome.status {
            BrickReadStatus::Completed(payload) => {
                if !payload_matches_cross_section_ticket(&ticket, &payload) {
                    failed = 1;
                    last_error =
                        Some("cross-section brick payload did not match its ticket".to_owned());
                } else {
                    if let Some(chunk_key) =
                        cross_section_chunk_key_for_ticket(dataset.dataset.dataset_id(), &ticket)
                    {
                        match runtime.mark_chunk_cpu_resident_from_payload(
                            chunk_key.clone(),
                            &payload,
                            outcome.read_metrics,
                        ) {
                            Ok(changed) => {
                                completed = 1;
                                resident_changed = changed;
                            }
                            Err(err) => {
                                let message = err.to_string();
                                let _ = runtime.mark_chunk_failed(chunk_key, message.clone());
                                failed = 1;
                                last_error = Some(message);
                            }
                        }
                    } else {
                        failed = 1;
                        last_error =
                            Some("cross-section brick ticket has invalid layer id".to_owned());
                    }
                }
            }
            BrickReadStatus::Cancelled => {
                cancelled = 1;
                if let Some(chunk_key) =
                    cross_section_chunk_key_for_ticket(dataset.dataset.dataset_id(), &ticket)
                {
                    runtime.mark_chunk_not_resident(&chunk_key);
                }
            }
            BrickReadStatus::Stale => {
                stale = 1;
                if let Some(chunk_key) =
                    cross_section_chunk_key_for_ticket(dataset.dataset.dataset_id(), &ticket)
                {
                    runtime.mark_chunk_not_resident(&chunk_key);
                }
            }
            BrickReadStatus::Failed(message) => {
                if let Some(chunk_key) =
                    cross_section_chunk_key_for_ticket(dataset.dataset.dataset_id(), &ticket)
                {
                    runtime.mark_chunk_failed(chunk_key, message.clone());
                }
                failed = 1;
                last_error = Some(message);
            }
        }
    }

    if ticket_panel_generation_current
        && let Some(stream) = runtime.panel_streams.get_mut(&ticket.panel_id)
        && stream.request_key.panel_generation == ticket.panel_generation
        && stream.request_key.scale_level == ticket.scale_level
        && stream.request_key.timepoint == ticket.timepoint
    {
        stream.completed = stream.completed.saturating_add(completed);
        stream.cancelled = stream.cancelled.saturating_add(cancelled);
        stream.stale = stream.stale.saturating_add(stale);
        stream.failed = stream.failed.saturating_add(failed);
        stream.decoded_bytes = stream.decoded_bytes.saturating_add(decoded_bytes);
        stream.encoded_payload_bytes_read = stream
            .encoded_payload_bytes_read
            .saturating_add(encoded_payload_bytes_read);
        if last_error.is_some() {
            stream.last_error = last_error;
        }
        stream.refresh_complete();
    }
    if completed > 0
        && let Some(chunk_key) =
            cross_section_chunk_key_for_ticket(dataset.dataset.dataset_id(), &ticket)
    {
        credit_shared_cross_section_completion_to_visible_streams(runtime, &ticket, &chunk_key);
    }
    if resident_changed {
        let _ = runtime.enforce_cpu_payload_budget();
    }
    resident_changed
}

fn credit_shared_cross_section_completion_to_visible_streams(
    runtime: &mut CrossSectionRuntime,
    ticket: &CrossSectionBrickReadTicket,
    chunk_key: &CrossSectionChunkKey,
) {
    let linked_panels = runtime
        .panels
        .iter()
        .filter(|(panel_id, panel)| {
            **panel_id != ticket.panel_id
                && panel.scale_level == ticket.scale_level
                && panel
                    .visible_chunks
                    .iter()
                    .any(|visible| visible == chunk_key)
        })
        .map(|(panel_id, panel)| (*panel_id, panel.generation))
        .collect::<Vec<_>>();
    for (panel_id, panel_generation) in linked_panels {
        let Some(stream) = runtime.panel_streams.get_mut(&panel_id) else {
            continue;
        };
        if stream.request_key.panel_generation != panel_generation
            || stream.request_key.scale_level != ticket.scale_level
            || stream.request_key.timepoint != ticket.timepoint
        {
            continue;
        }
        stream.credit_completed_visible_chunks(1);
    }
}

fn cross_section_chunk_key_for_parts(
    dataset_id: &mirante4d_format::DatasetId,
    layer_id: LayerId,
    scale_level: u32,
    timepoint: TimeIndex,
    brick_index: SpatialBrickIndex,
) -> CrossSectionChunkKey {
    CrossSectionChunkKey {
        dataset_id: dataset_id.clone(),
        layer_id,
        timepoint,
        scale_level,
        brick_index,
    }
}

fn cross_section_chunk_key_for_ticket(
    dataset_id: &mirante4d_format::DatasetId,
    ticket: &CrossSectionBrickReadTicket,
) -> Option<CrossSectionChunkKey> {
    Some(cross_section_chunk_key_for_parts(
        dataset_id,
        LayerId::new(ticket.layer_id.clone()).ok()?,
        ticket.scale_level,
        ticket.timepoint,
        ticket.brick_index,
    ))
}

fn cross_section_visible_chunk_fingerprint(chunks: &[CrossSectionChunkKey]) -> u64 {
    let mut hasher = DefaultHasher::new();
    chunks.len().hash(&mut hasher);
    for chunk in chunks {
        chunk.hash(&mut hasher);
    }
    hasher.finish()
}

fn retire_tickets_before_generation(
    runtime: &mut CrossSectionRuntime,
    active_generation: DataGenerationId,
) {
    let tickets = std::mem::take(&mut runtime.read_tickets);
    for ticket in tickets {
        if ticket.ticket.generation_id.0 >= active_generation.0 {
            runtime.read_tickets.push(ticket);
            continue;
        }
        let panel_id = ticket.panel_id;
        let panel_generation = ticket.panel_generation;
        runtime.cancel_read_ticket(ticket);
        if let Some(stream) = runtime.panel_streams.get_mut(&panel_id)
            && stream.request_key.panel_generation == panel_generation
        {
            stream.stale = stream.stale.saturating_add(1);
            stream.refresh_complete();
        }
    }
}

fn cancel_obsolete_panel_tickets(runtime: &mut CrossSectionRuntime, panel_id: PanelId) {
    let tickets = std::mem::take(&mut runtime.read_tickets);
    for ticket in tickets {
        let current = runtime.panels.values().any(|panel| {
            panel.scale_level == ticket.scale_level
                && panel.generation == ticket.panel_generation
                && panel.visible_chunks.iter().any(|key| {
                    key.layer_id.as_str() == ticket.layer_id
                        && key.timepoint == ticket.timepoint
                        && key.brick_index == ticket.brick_index
                })
        });
        if ticket.panel_id != panel_id || current {
            runtime.read_tickets.push(ticket);
            continue;
        }
        runtime.cancel_read_ticket(ticket);
    }
}

fn ticket_matches_current_panel_generation(
    runtime: &CrossSectionRuntime,
    view: &ViewState,
    ticket: &CrossSectionBrickReadTicket,
) -> bool {
    if view.layout() != ViewerLayout::FourPanel {
        return false;
    }
    runtime
        .panel(ticket.panel_id)
        .is_some_and(|panel| panel.generation == ticket.panel_generation)
}

fn ticket_chunk_is_current_in_global_runtime(
    runtime: &CrossSectionRuntime,
    view: &ViewState,
    dataset_id: &mirante4d_format::DatasetId,
    ticket: &CrossSectionBrickReadTicket,
) -> bool {
    if view.layout() != ViewerLayout::FourPanel {
        return false;
    }
    let Some(chunk_key) = cross_section_chunk_key_for_ticket(dataset_id, ticket) else {
        return false;
    };
    runtime.panels.iter().any(|(_, panel_runtime)| {
        if panel_runtime.scale_level != ticket.scale_level
            || !panel_runtime
                .visible_chunks
                .iter()
                .any(|visible_key| visible_key == &chunk_key)
        {
            return false;
        }
        panel_runtime.generation == ticket.panel_generation
    })
}

fn outcome_matches_cross_section_ticket(
    ticket: &CrossSectionBrickReadTicket,
    outcome: &BrickReadOutcome,
) -> bool {
    outcome.layer_id.as_str() == ticket.layer_id
        && outcome.scale_level == ticket.scale_level
        && outcome.timepoint == ticket.timepoint
        && outcome.brick_index == ticket.brick_index
        && outcome.sample_region.is_none()
}

fn payload_matches_cross_section_ticket(
    ticket: &CrossSectionBrickReadTicket,
    payload: &BrickReadPayload,
) -> bool {
    match payload {
        BrickReadPayload::U8(brick) => {
            brick.scale_level == ticket.scale_level
                && brick.volume.timepoint == ticket.timepoint
                && brick.brick_index == ticket.brick_index
                && brick.region == ticket.region
        }
        BrickReadPayload::U16(brick) => {
            brick.scale_level == ticket.scale_level
                && brick.volume.timepoint == ticket.timepoint
                && brick.brick_index == ticket.brick_index
                && brick.region == ticket.region
        }
        BrickReadPayload::F32(brick) => {
            brick.scale_level == ticket.scale_level
                && brick.volume.timepoint == ticket.timepoint
                && brick.brick_index == ticket.brick_index
                && brick.region == ticket.region
        }
        BrickReadPayload::Group(_) => false,
    }
}

fn layer_for_id<'a>(
    layers: &'a [CrossSectionLayerInput<'a>],
    layer_id: &LayerId,
) -> anyhow::Result<CrossSectionLayerInput<'a>> {
    layers
        .iter()
        .copied()
        .find(|layer| layer.id == layer_id)
        .ok_or_else(|| anyhow::anyhow!("layer {} is not available for cross-sections", layer_id))
}

fn zero_cross_section_brick_payload(
    dataset: &CurrentDatasetRuntime,
    layer_id: &LayerId,
    timepoint: TimeIndex,
    metadata: BrickMetadata,
    region: mirante4d_data::VolumeRegion,
    dtype: IntensityDType,
) -> anyhow::Result<BrickReadPayload> {
    let grid_to_world = if region == metadata.region {
        metadata.grid_to_world
    } else {
        translated_region_grid_to_world(
            dataset
                .dataset
                .scale_grid_to_world(layer_id, metadata.scale_level)?,
            region,
        )
    };
    let shape = region.shape()?;
    let voxel_count = shape.element_count()? as usize;
    match dtype {
        IntensityDType::Uint8 => {
            let volume = DenseVolumeU8::new(
                dataset.dataset.dataset_id().clone(),
                layer_id.clone(),
                metadata.scale_level,
                timepoint,
                shape,
                grid_to_world,
                vec![0; voxel_count],
            )?;
            let volume = if metadata.valid_voxel_count == 0 {
                volume.with_render_valid(vec![0; voxel_count])?
            } else {
                volume
            };
            Ok(BrickReadPayload::U8(Box::new(VolumeBrickU8 {
                scale_level: metadata.scale_level,
                brick_index: metadata.brick_index,
                chunk_index: metadata.chunk_index,
                region,
                occupied: false,
                valid_voxel_count: metadata.valid_voxel_count,
                min: metadata.min,
                max: metadata.max,
                volume,
            })))
        }
        IntensityDType::Uint16 => {
            let volume = DenseVolumeU16::new(
                dataset.dataset.dataset_id().clone(),
                layer_id.clone(),
                metadata.scale_level,
                timepoint,
                shape,
                grid_to_world,
                vec![0; voxel_count],
            )?;
            let volume = if metadata.valid_voxel_count == 0 {
                volume.with_render_valid(vec![0; voxel_count])?
            } else {
                volume
            };
            Ok(BrickReadPayload::U16(Box::new(VolumeBrickU16 {
                scale_level: metadata.scale_level,
                brick_index: metadata.brick_index,
                chunk_index: metadata.chunk_index,
                region,
                occupied: false,
                valid_voxel_count: metadata.valid_voxel_count,
                min: metadata.min,
                max: metadata.max,
                volume,
            })))
        }
        IntensityDType::Float32 => {
            let volume = DenseVolumeF32::new(
                dataset.dataset.dataset_id().clone(),
                layer_id.clone(),
                metadata.scale_level,
                timepoint,
                shape,
                grid_to_world,
                vec![0.0; voxel_count],
            )?;
            let volume = if metadata.valid_voxel_count == 0 {
                volume.with_render_valid(vec![0; voxel_count])?
            } else {
                volume
            };
            Ok(BrickReadPayload::F32(Box::new(VolumeBrickF32 {
                scale_level: metadata.scale_level,
                brick_index: metadata.brick_index,
                chunk_index: metadata.chunk_index,
                region,
                occupied: false,
                valid_voxel_count: metadata.valid_voxel_count,
                min: metadata.min,
                max: metadata.max,
                volume,
            })))
        }
    }
}

fn dtype_decoded_bytes(dtype: IntensityDType) -> u64 {
    match dtype {
        IntensityDType::Uint8 => 1,
        IntensityDType::Uint16 => 2,
        IntensityDType::Float32 => 4,
    }
}

#[cfg(test)]
mod tests {
    use glam::DVec2;
    use mirante4d_data::SpatialBrickIndex;
    use mirante4d_domain::TimeIndex;
    use mirante4d_format::{DatasetId, LayerId};
    use mirante4d_renderer::CrossSectionPanelBounds;

    use super::*;
    use crate::cross_section_runtime::{
        CrossSectionChunkPriorityTier, CrossSectionRuntime, CrossSectionVisibleChunkGeometry,
        CrossSectionVisibleChunkPlan,
    };

    fn key(z: u64, y: u64, x: u64) -> CrossSectionChunkKey {
        CrossSectionChunkKey {
            dataset_id: DatasetId::new("dataset").unwrap(),
            layer_id: LayerId::new("layer").unwrap(),
            timepoint: TimeIndex::new(0),
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
        priority_tier: CrossSectionChunkPriorityTier,
        geometries: Vec<CrossSectionVisibleChunkGeometry>,
    ) -> CrossSectionVisibleChunkPlan {
        CrossSectionVisibleChunkPlan {
            panel_id,
            generation: 1,
            scale_level: 0,
            priority_tier,
            candidate_chunks: geometries.len(),
            visible_chunks: geometries
                .iter()
                .map(|geometry| geometry.key.clone())
                .collect(),
            visible_chunk_geometries: geometries,
        }
    }

    fn request_key(
        panel_id: PanelId,
        visible_chunk_count: usize,
    ) -> CrossSectionVisibleChunkRequestKey {
        CrossSectionVisibleChunkRequestKey {
            panel_id,
            panel_generation: 1,
            layer_ids: vec!["layer".to_owned()],
            scale_level: 0,
            timepoint: TimeIndex::new(0),
            visible_chunk_count,
            visible_chunk_fingerprint: visible_chunk_count as u64,
        }
    }

    #[test]
    fn stream_completion_allows_deferred_chunks_completed_by_shared_reads() {
        let mut stream = CrossSectionPanelBrickStreamState::new(
            request_key(PanelId::Yz, 30),
            BrickRequestPriority::Prefetch,
            Some(PanelId::Xz),
            false,
        );
        stream.requested = 17;
        stream.deferred = 13;
        stream.completed = 30;

        stream.refresh_complete();

        assert!(!stream.active());
        assert!(stream.complete);
    }

    #[test]
    fn shared_completion_credit_is_capped_by_deferred_visible_work() {
        let mut stream = CrossSectionPanelBrickStreamState::new(
            request_key(PanelId::Yz, 1),
            BrickRequestPriority::Prefetch,
            Some(PanelId::Xz),
            false,
        );
        stream.deferred = 1;

        stream.credit_completed_visible_chunks(3);

        assert_eq!(stream.completed, 1);
        assert!(stream.complete);
    }

    #[test]
    fn refresh_download_budget_selects_chunks_by_global_runtime_queue_order() {
        let mut runtime = CrossSectionRuntime::default();
        assert!(runtime.mark_cross_section_panels_dirty());
        let xy_best = key(0, 0, 0);
        let xz_middle = key(0, 0, 1);
        let xy_after_budget = key(0, 0, 2);
        runtime.apply_visible_chunk_plan(plan(
            PanelId::Xy,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![
                geometry(xy_best.clone(), -1.0),
                geometry(xy_after_budget.clone(), -3.0),
            ],
        ));
        runtime.apply_visible_chunk_plan(plan(
            PanelId::Xz,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![geometry(xz_middle.clone(), -2.0)],
        ));

        let allowed =
            cross_section_read_admissions_for_refresh(&runtime, [PanelId::Xy, PanelId::Xz], 2);
        let allowed = allowed
            .into_iter()
            .map(|admission| admission.queue_entry.key)
            .collect::<HashSet<_>>();

        assert_eq!(allowed.len(), 2);
        assert!(allowed.contains(&xy_best));
        assert!(allowed.contains(&xz_middle));
        assert!(!allowed.contains(&xy_after_budget));
    }

    #[test]
    fn chunk_submission_order_uses_visible_priority_score_before_key_order() {
        let mut runtime = CrossSectionRuntime::default();
        assert!(runtime.mark_cross_section_panels_dirty());
        let far_key_order_first = key(0, 0, 0);
        let near_key_order_second = key(0, 0, 1);
        runtime.apply_visible_chunk_plan(plan(
            PanelId::Xy,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![
                geometry(far_key_order_first.clone(), -10.0),
                geometry(near_key_order_second.clone(), -1.0),
            ],
        ));

        let chunks = runtime
            .panel_submission_candidates(PanelId::Xy)
            .into_iter()
            .map(|candidate| candidate.key)
            .collect::<Vec<_>>();

        assert_eq!(chunks, vec![near_key_order_second, far_key_order_first]);
    }

    #[test]
    fn chunk_submission_order_prioritizes_active_tier_before_score() {
        let mut runtime = CrossSectionRuntime::default();
        assert!(runtime.mark_cross_section_panels_dirty());
        let active_far = key(0, 0, 1);
        let linked_near = key(0, 0, 0);
        runtime.apply_visible_chunk_plan(plan(
            PanelId::Xy,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![geometry(active_far.clone(), -10.0)],
        ));
        runtime.apply_visible_chunk_plan(plan(
            PanelId::Xz,
            CrossSectionChunkPriorityTier::VisibleLinked,
            vec![geometry(linked_near.clone(), -1.0)],
        ));

        let chunks = runtime
            .submission_candidates_for_panels([PanelId::Xy, PanelId::Xz])
            .into_iter()
            .map(|candidate| candidate.key)
            .collect::<Vec<_>>();

        assert_eq!(chunks, vec![active_far, linked_near]);
    }

    #[test]
    fn global_submission_candidates_deduplicate_shared_chunk_by_best_panel_priority() {
        let mut runtime = CrossSectionRuntime::default();
        assert!(runtime.mark_cross_section_panels_dirty());
        let shared = key(0, 0, 0);
        runtime.apply_visible_chunk_plan(plan(
            PanelId::Xy,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![geometry(shared.clone(), -10.0)],
        ));
        runtime.apply_visible_chunk_plan(plan(
            PanelId::Xz,
            CrossSectionChunkPriorityTier::VisibleLinked,
            vec![geometry(shared.clone(), -1.0)],
        ));

        let candidates = runtime.submission_candidates_for_panels([PanelId::Xz, PanelId::Xy]);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].key, shared);
        assert_eq!(candidates[0].panel_id, PanelId::Xy);
        assert_eq!(
            candidates[0].priority_tier,
            CrossSectionChunkPriorityTier::VisibleActive
        );
    }

    #[test]
    fn runtime_panel_submission_order_uses_global_candidate_priority() {
        let mut runtime = CrossSectionRuntime::default();
        assert!(runtime.mark_cross_section_panels_dirty());
        let active_far = key(0, 0, 1);
        let linked_near = key(0, 0, 0);
        runtime.apply_visible_chunk_plan(plan(
            PanelId::Xz,
            CrossSectionChunkPriorityTier::VisibleLinked,
            vec![geometry(linked_near.clone(), -1.0)],
        ));
        runtime.apply_visible_chunk_plan(plan(
            PanelId::Xy,
            CrossSectionChunkPriorityTier::VisibleActive,
            vec![geometry(active_far.clone(), -10.0)],
        ));

        let ordered =
            cross_section_runtime_panel_submission_order(&runtime, vec![PanelId::Xz, PanelId::Xy]);

        assert_eq!(ordered, vec![PanelId::Xy, PanelId::Xz]);
    }
}
