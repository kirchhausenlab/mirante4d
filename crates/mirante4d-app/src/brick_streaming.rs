use std::collections::{BTreeMap, HashSet};

use mirante4d_core::{IntensityDType, LayerId, Shape3D, TimeIndex};
use mirante4d_data::{
    BrickMetadata, BrickReadOutcome, BrickReadPayload, BrickReadPool, BrickReadSpec,
    BrickReadStatus, BrickReadTicket, BrickRequestPriority, CancellationToken, DenseVolumeF32,
    DenseVolumeU8, DenseVolumeU16, SpatialBrickIndex, VolumeBrickF32, VolumeBrickU8,
    VolumeBrickU16, VolumeRegion, translated_region_grid_to_world,
};

use crate::AppState;
use crate::histogram::{
    resident_histogram_sample_key_for_f32_brick, resident_histogram_sample_key_for_u8_brick,
    resident_histogram_sample_key_for_u16_brick,
};

const PREFETCHED_BRICK_PAYLOAD_LIMIT: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrickStreamRequestKey {
    layer_ids: Vec<String>,
    scale_level: u32,
    timepoint: TimeIndex,
    visible_bricks: Vec<SpatialBrickIndex>,
    source_regions: Vec<BrickSourceRegion>,
}

#[path = "brick_streaming_plan.rs"]
mod brick_streaming_plan;
pub(crate) use brick_streaming_plan::*;

pub(crate) fn current_resident_frame_ready(state: &AppState) -> bool {
    if !state.brick_stream_complete {
        return false;
    }
    if !visible_resident_layers_ready(state) {
        return false;
    }
    let Ok(expected_key) = current_brick_stream_request_key(state) else {
        return false;
    };
    state.brick_stream_request_key.as_ref() == Some(&expected_key)
}

pub(crate) fn brick_runtime_work_active(state: &AppState) -> bool {
    current_brick_stream_work_active(state)
        || outstanding_work(
            state.brick_prefetch_requested,
            state.brick_prefetch_completed,
            state.brick_prefetch_cancelled,
            state.brick_prefetch_stale,
            state.brick_prefetch_failed,
        )
        || outstanding_work(
            state.brick_warm_requested,
            state.brick_warm_completed,
            state.brick_warm_cancelled,
            state.brick_warm_stale,
            state.brick_warm_failed,
        )
}

fn current_brick_stream_work_active(state: &AppState) -> bool {
    outstanding_work(
        state.brick_stream_requested,
        state.brick_stream_completed,
        state.brick_stream_cancelled,
        state.brick_stream_stale,
        state.brick_stream_failed,
    )
}

fn outstanding_work(
    requested: usize,
    completed: usize,
    cancelled: usize,
    stale: usize,
    failed: usize,
) -> bool {
    completed
        .saturating_add(cancelled)
        .saturating_add(stale)
        .saturating_add(failed)
        < requested
}

pub(crate) fn create_brick_read_pool(state: &AppState) -> Option<BrickReadPool> {
    match BrickReadPool::new(
        state.dataset.clone(),
        default_brick_worker_count(),
        default_brick_queue_capacity(),
    ) {
        Ok(pool) => Some(pool),
        Err(err) => {
            tracing::error!(error = %err, "failed to create brick read pool");
            None
        }
    }
}

fn default_brick_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|parallelism| parallelism.get().saturating_sub(1).clamp(1, 4))
        .unwrap_or(1)
}

fn default_brick_queue_capacity() -> usize {
    8192
}

pub(crate) fn reset_prefetch_state(state: &mut AppState) {
    state.brick_prefetch_timepoints.clear();
    state.brick_prefetch_requested = 0;
    state.brick_prefetch_completed = 0;
    state.brick_prefetch_cancelled = 0;
    state.brick_prefetch_stale = 0;
    state.brick_prefetch_failed = 0;
    state.brick_prefetch_skipped = 0;
    state.brick_prefetch_last_error = None;
    state.brick_prefetch_request_key = None;
    reset_warm_state(state);
}

pub(crate) fn reset_warm_state(state: &mut AppState) {
    state.brick_warm_brick_count = 0;
    state.brick_warm_requested = 0;
    state.brick_warm_completed = 0;
    state.brick_warm_cancelled = 0;
    state.brick_warm_stale = 0;
    state.brick_warm_failed = 0;
    state.brick_warm_skipped = 0;
    state.brick_warm_last_error = None;
    state.brick_warm_request_key = None;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BrickPrefetchPolicy {
    timepoint_horizon: u64,
    max_requests: usize,
}

#[derive(Debug, Default)]
pub(crate) struct BrickSubmissionResult {
    pub(crate) current_changed: bool,
    pub(crate) queued_current: bool,
    pub(crate) resident_changed: bool,
    pub(crate) current_tickets: Vec<BrickReadTicket>,
    pub(crate) prefetch_tickets: Vec<BrickReadTicket>,
    pub(crate) warm_tickets: Vec<BrickReadTicket>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BrickSubmissionOptions {
    submit_prefetch: bool,
    submit_warm: bool,
}

impl BrickSubmissionOptions {
    pub(crate) const DEFAULT: Self = Self {
        submit_prefetch: true,
        submit_warm: true,
    };

    pub(crate) const PLAYBACK: Self = Self {
        submit_prefetch: true,
        submit_warm: false,
    };

    pub(crate) const CURRENT_ONLY: Self = Self {
        submit_prefetch: false,
        submit_warm: false,
    };
}

pub(crate) fn cancel_brick_tickets(tickets: &mut Vec<BrickReadTicket>) {
    for ticket in tickets.drain(..) {
        ticket.cancel();
    }
}

pub(crate) fn submit_visible_bricks_to_pool(
    state: &mut AppState,
    pool: &BrickReadPool,
) -> BrickSubmissionResult {
    submit_visible_bricks_to_pool_with_options(state, pool, BrickSubmissionOptions::DEFAULT)
}

pub(crate) fn submit_visible_bricks_to_pool_with_options(
    state: &mut AppState,
    pool: &BrickReadPool,
    options: BrickSubmissionOptions,
) -> BrickSubmissionResult {
    let layer_ids = match stream_layer_ids_for_state(state) {
        Ok(layer_ids) => layer_ids,
        Err(err) => {
            state.brick_stream_last_error = Some(err.to_string());
            return BrickSubmissionResult::default();
        }
    };
    if layer_ids.is_empty() {
        return BrickSubmissionResult::default();
    }
    let request_key = match current_brick_stream_request_key(state) {
        Ok(request_key) => request_key,
        Err(err) => {
            state.brick_stream_last_error = Some(err.to_string());
            return BrickSubmissionResult::default();
        }
    };
    if state.brick_stream_request_key.as_ref() == Some(&request_key)
        && state.brick_stream_failed == 0
        && (state.brick_stream_complete || current_brick_stream_work_active(state))
    {
        return BrickSubmissionResult::default();
    }
    let generation = pool.advance_generation();
    state.brick_stream_generation = generation.0;
    state.brick_stream_requested = 0;
    state.brick_stream_completed = 0;
    state.brick_stream_cancelled = 0;
    state.brick_stream_stale = 0;
    state.brick_stream_failed = 0;
    state.brick_stream_last_error = None;
    state.brick_stream_complete = state.visible_bricks.is_empty();
    let request_regions = request_region_map(&request_key);
    let retained_resident_changed = retain_current_resident_bricks_for_request(
        state,
        &layer_ids,
        state.brick_stream_scale_level,
        state.active_timepoint,
        &request_regions,
    );
    state.brick_stream_request_key = Some(request_key);
    reset_prefetch_state(state);
    let mut result = BrickSubmissionResult {
        current_changed: true,
        resident_changed: retained_resident_changed,
        ..BrickSubmissionResult::default()
    };
    let mut completed_prefetch_promotions = Vec::new();

    for layer_id in &layer_ids {
        for brick in state.visible_bricks.clone() {
            let Some(region_request) = request_regions.get(&brick).copied() else {
                state.brick_stream_failed += 1;
                state.brick_stream_complete = false;
                state.brick_stream_last_error = Some(format!(
                    "missing source region for visible brick z={}, y={}, x={}",
                    brick.z, brick.y, brick.x
                ));
                continue;
            };
            state.brick_stream_requested += 1;
            if resident_current_brick_exists(
                state,
                layer_id,
                state.brick_stream_scale_level,
                state.active_timepoint,
                brick,
                region_request.resident_region,
            ) {
                state.brick_stream_completed += 1;
                continue;
            }
            match materialize_empty_current_brick(
                state,
                layer_id,
                state.active_timepoint,
                brick,
                region_request.resident_region,
            ) {
                Ok(true) => {
                    result.resident_changed = true;
                    continue;
                }
                Ok(false) => {}
                Err(err) => {
                    state.brick_stream_failed += 1;
                    state.brick_stream_complete = false;
                    state.brick_stream_last_error = Some(err.to_string());
                    continue;
                }
            }
            if let Some(payload) = take_prefetched_brick_payload(
                state,
                layer_id,
                state.brick_stream_scale_level,
                state.active_timepoint,
                brick,
                region_request.worker_sample_region,
            ) {
                state.brick_stream_completed += 1;
                completed_prefetch_promotions.push(CompletedCurrentBrick {
                    layer_id: layer_id.to_string(),
                    payload,
                    histogram_sample: None,
                });
                result.resident_changed = true;
                continue;
            }
            let cancellation = CancellationToken::new();
            match pool.submit_brick_spec_for_generation(
                generation,
                BrickReadSpec {
                    layer_id: layer_id.clone(),
                    scale_level: state.brick_stream_scale_level,
                    timepoint: state.active_timepoint,
                    brick_index: brick,
                    sample_region: region_request.worker_sample_region,
                    coalesced_brick_indices: Vec::new(),
                    priority: BrickRequestPriority::CurrentFrame,
                    queue_priority: 0,
                    cancellation,
                },
            ) {
                Ok(ticket) => {
                    result.queued_current = true;
                    result.current_tickets.push(ticket);
                }
                Err(err) => {
                    state.brick_stream_failed += 1;
                    state.brick_stream_complete = false;
                    state.brick_stream_last_error = Some(err.to_string());
                }
            }
        }
    }
    if !completed_prefetch_promotions.is_empty() {
        insert_resident_brick_payloads(state, completed_prefetch_promotions);
    }
    state.brick_stream_complete = state.brick_stream_failed == 0
        && state.brick_stream_cancelled == 0
        && state.brick_stream_stale == 0
        && state.brick_stream_completed == state.brick_stream_requested;
    if state.brick_stream_failed == 0
        && options.submit_prefetch
        && let Some(primary_visible_layer_id) = primary_visible_layer_id_for_state(state)
    {
        let prefetch_policy = prefetch_policy_for_state(state, pool.queue_capacity());
        result.prefetch_tickets = submit_prefetch_bricks_for_generation(
            state,
            pool,
            generation,
            primary_visible_layer_id.clone(),
            prefetch_policy,
        );
        if options.submit_warm {
            let warm_budget = prefetch_policy
                .max_requests
                .saturating_sub(result.prefetch_tickets.len());
            result.warm_tickets = submit_warm_bricks_for_generation(
                state,
                pool,
                generation,
                primary_visible_layer_id,
                warm_budget,
            );
        }
    }
    result
}

fn retain_current_resident_bricks_for_request(
    state: &mut AppState,
    layer_ids: &[LayerId],
    scale_level: u32,
    timepoint: TimeIndex,
    request_regions: &BTreeMap<SpatialBrickIndex, BrickSourceRegion>,
) -> bool {
    let resident_count_before = resident_brick_count(state);
    let visible: HashSet<SpatialBrickIndex> = state.visible_bricks.iter().copied().collect();
    let layers: HashSet<String> = layer_ids.iter().map(ToString::to_string).collect();
    retain_u16_resident_map(
        &mut state.resident_bricks_u8_by_layer,
        &layers,
        &visible,
        scale_level,
        timepoint,
        request_regions,
    );
    retain_u16_resident_map(
        &mut state.resident_bricks_by_layer,
        &layers,
        &visible,
        scale_level,
        timepoint,
        request_regions,
    );
    retain_f32_resident_map(
        &mut state.resident_bricks_f32_by_layer,
        &layers,
        &visible,
        scale_level,
        timepoint,
        request_regions,
    );
    state.resident_bricks_u8 = state
        .resident_bricks_u8_by_layer
        .get(&state.active_layer_id)
        .cloned()
        .unwrap_or_default();
    state.resident_bricks = state
        .resident_bricks_by_layer
        .get(&state.active_layer_id)
        .cloned()
        .unwrap_or_default();
    state.resident_bricks_f32 = state
        .resident_bricks_f32_by_layer
        .get(&state.active_layer_id)
        .cloned()
        .unwrap_or_default();
    let sample_count_before = state.resident_histogram_samples.len();
    state.resident_histogram_samples.retain(|key, _| {
        layers.contains(&key.layer_id)
            && visible.contains(&key.brick_index)
            && key.scale_level == scale_level
            && key.timepoint == timepoint
    });
    if state.resident_histogram_samples.len() != sample_count_before {
        state.resident_histogram_generation = state.resident_histogram_generation.saturating_add(1);
        state.active_histogram_cache = None;
    }
    resident_count_before != resident_brick_count(state)
        || sample_count_before != state.resident_histogram_samples.len()
}

fn resident_brick_count(state: &AppState) -> usize {
    state
        .resident_bricks_u8_by_layer
        .values()
        .map(Vec::len)
        .sum::<usize>()
        + state
            .resident_bricks_by_layer
            .values()
            .map(Vec::len)
            .sum::<usize>()
        + state
            .resident_bricks_f32_by_layer
            .values()
            .map(Vec::len)
            .sum::<usize>()
}

fn retain_u16_resident_map<T>(
    map: &mut BTreeMap<String, Vec<T>>,
    layers: &HashSet<String>,
    visible: &HashSet<SpatialBrickIndex>,
    scale_level: u32,
    timepoint: TimeIndex,
    request_regions: &BTreeMap<SpatialBrickIndex, BrickSourceRegion>,
) where
    T: ResidentBrickInfo,
{
    map.retain(|layer_id, bricks| {
        if !layers.contains(layer_id) {
            return false;
        }
        let mut seen = HashSet::new();
        bricks.retain(|brick| {
            brick.scale_level() == scale_level
                && brick.timepoint() == timepoint
                && visible.contains(&brick.brick_index())
                && request_regions
                    .get(&brick.brick_index())
                    .is_some_and(|request| request.resident_region == brick.region())
                && seen.insert(brick.brick_index())
        });
        !bricks.is_empty()
    });
}

trait ResidentBrickInfo {
    fn scale_level(&self) -> u32;
    fn timepoint(&self) -> TimeIndex;
    fn brick_index(&self) -> SpatialBrickIndex;
    fn region(&self) -> VolumeRegion;
}

impl ResidentBrickInfo for VolumeBrickU8 {
    fn scale_level(&self) -> u32 {
        self.scale_level
    }

    fn timepoint(&self) -> TimeIndex {
        self.volume.timepoint
    }

    fn brick_index(&self) -> SpatialBrickIndex {
        self.brick_index
    }

    fn region(&self) -> VolumeRegion {
        self.region
    }
}

impl ResidentBrickInfo for VolumeBrickU16 {
    fn scale_level(&self) -> u32 {
        self.scale_level
    }

    fn timepoint(&self) -> TimeIndex {
        self.volume.timepoint
    }

    fn brick_index(&self) -> SpatialBrickIndex {
        self.brick_index
    }

    fn region(&self) -> VolumeRegion {
        self.region
    }
}

fn retain_f32_resident_map(
    map: &mut BTreeMap<String, Vec<VolumeBrickF32>>,
    layers: &HashSet<String>,
    visible: &HashSet<SpatialBrickIndex>,
    scale_level: u32,
    timepoint: TimeIndex,
    request_regions: &BTreeMap<SpatialBrickIndex, BrickSourceRegion>,
) {
    map.retain(|layer_id, bricks| {
        if !layers.contains(layer_id) {
            return false;
        }
        let mut seen = HashSet::new();
        bricks.retain(|brick| {
            brick.scale_level == scale_level
                && brick.volume.timepoint == timepoint
                && visible.contains(&brick.brick_index)
                && request_regions
                    .get(&brick.brick_index)
                    .is_some_and(|request| request.resident_region == brick.region)
                && seen.insert(brick.brick_index)
        });
        !bricks.is_empty()
    });
}

fn resident_current_brick_exists(
    state: &AppState,
    layer_id: &LayerId,
    scale_level: u32,
    timepoint: TimeIndex,
    brick_index: SpatialBrickIndex,
    region: VolumeRegion,
) -> bool {
    state
        .resident_bricks_u8_by_layer
        .get(layer_id.as_str())
        .is_some_and(|bricks| {
            bricks.iter().any(|brick| {
                brick.scale_level == scale_level
                    && brick.volume.timepoint == timepoint
                    && brick.brick_index == brick_index
                    && brick.region == region
            })
        })
        || state
            .resident_bricks_by_layer
            .get(layer_id.as_str())
            .is_some_and(|bricks| {
                bricks.iter().any(|brick| {
                    brick.scale_level == scale_level
                        && brick.volume.timepoint == timepoint
                        && brick.brick_index == brick_index
                        && brick.region == region
                })
            })
        || state
            .resident_bricks_f32_by_layer
            .get(layer_id.as_str())
            .is_some_and(|bricks| {
                bricks.iter().any(|brick| {
                    brick.scale_level == scale_level
                        && brick.volume.timepoint == timepoint
                        && brick.brick_index == brick_index
                        && brick.region == region
                })
            })
}

fn primary_visible_layer_id_for_state(state: &AppState) -> Option<LayerId> {
    let layer_ids = stream_layer_ids_for_state(state).ok()?;
    let active = LayerId::new(state.active_layer_id.clone()).ok()?;
    layer_ids
        .iter()
        .find(|layer_id| **layer_id == active)
        .cloned()
        .or_else(|| layer_ids.first().cloned())
}

fn materialize_empty_current_brick(
    state: &mut AppState,
    layer_id: &LayerId,
    timepoint: TimeIndex,
    brick_index: SpatialBrickIndex,
    region: VolumeRegion,
) -> anyhow::Result<bool> {
    let metadata = state.dataset.brick_metadata_at_scale(
        layer_id,
        state.brick_stream_scale_level,
        timepoint,
        brick_index,
    )?;
    if metadata.occupied {
        return Ok(false);
    }
    let layer = state
        .layers
        .iter()
        .find(|layer| layer.id == layer_id.as_str())
        .ok_or_else(|| anyhow::anyhow!("layer {} is not loaded in app state", layer_id))?;
    let payload =
        zero_resident_brick_payload(state, layer_id, timepoint, metadata, region, layer.dtype)?;
    state.brick_stream_completed += 1;
    apply_completed_current_brick(state, layer_id.to_string(), payload, None);
    Ok(true)
}

pub(crate) fn zero_resident_brick_payload(
    state: &AppState,
    layer_id: &LayerId,
    timepoint: TimeIndex,
    metadata: BrickMetadata,
    region: VolumeRegion,
    dtype: IntensityDType,
) -> anyhow::Result<BrickReadPayload> {
    let grid_to_world = if region == metadata.region {
        metadata.grid_to_world
    } else {
        translated_region_grid_to_world(
            state
                .dataset
                .scale_grid_to_world(layer_id, metadata.scale_level)?,
            region,
        )
    };
    match dtype {
        IntensityDType::Uint8 => {
            let shape = region.shape()?;
            let voxel_count = shape.element_count()? as usize;
            let values = vec![0u8; voxel_count];
            let volume = if metadata.valid_voxel_count == 0 {
                DenseVolumeU8::new(
                    state.dataset.dataset_id().clone(),
                    layer_id.clone(),
                    metadata.scale_level,
                    timepoint,
                    shape,
                    grid_to_world,
                    values,
                )?
                .with_render_valid(vec![0; voxel_count])?
            } else {
                DenseVolumeU8::new(
                    state.dataset.dataset_id().clone(),
                    layer_id.clone(),
                    metadata.scale_level,
                    timepoint,
                    shape,
                    grid_to_world,
                    values,
                )?
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
            let shape = region.shape()?;
            let voxel_count = shape.element_count()? as usize;
            let values = vec![0u16; voxel_count];
            let volume = if metadata.valid_voxel_count == 0 {
                DenseVolumeU16::new(
                    state.dataset.dataset_id().clone(),
                    layer_id.clone(),
                    metadata.scale_level,
                    timepoint,
                    shape,
                    grid_to_world,
                    values,
                )?
                .with_render_valid(vec![0; voxel_count])?
            } else {
                DenseVolumeU16::new(
                    state.dataset.dataset_id().clone(),
                    layer_id.clone(),
                    metadata.scale_level,
                    timepoint,
                    shape,
                    grid_to_world,
                    values,
                )?
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
            let shape = region.shape()?;
            let voxel_count = shape.element_count()? as usize;
            let values = vec![0.0f32; voxel_count];
            let volume = if metadata.valid_voxel_count == 0 {
                DenseVolumeF32::new(
                    state.dataset.dataset_id().clone(),
                    layer_id.clone(),
                    metadata.scale_level,
                    timepoint,
                    shape,
                    grid_to_world,
                    values,
                )?
                .with_render_valid(vec![0; voxel_count])?
            } else {
                DenseVolumeF32::new(
                    state.dataset.dataset_id().clone(),
                    layer_id.clone(),
                    metadata.scale_level,
                    timepoint,
                    shape,
                    grid_to_world,
                    values,
                )?
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

pub(crate) fn stream_layer_ids_for_state(state: &AppState) -> anyhow::Result<Vec<LayerId>> {
    let mut layer_ids = Vec::new();
    for layer in &state.layers {
        if !layer.display.visible {
            continue;
        }
        if layer.shape.spatial() != state.active_layer_shape.spatial() {
            anyhow::bail!(
                "visible layer {} has shape {:?}, expected active shape {:?}",
                layer.id,
                layer.shape.spatial(),
                state.active_layer_shape.spatial()
            );
        }
        if state.active_timepoint.0 >= layer.shape.t {
            anyhow::bail!(
                "visible layer {} has no timepoint {}",
                layer.id,
                state.active_timepoint.0
            );
        }
        layer_ids.push(LayerId::new(layer.id.clone())?);
    }
    Ok(layer_ids)
}

fn prefetch_policy_for_state(state: &AppState, queue_capacity: usize) -> BrickPrefetchPolicy {
    let reserved_for_current = state.brick_stream_requested;
    BrickPrefetchPolicy {
        timepoint_horizon: 2,
        max_requests: queue_capacity.saturating_sub(reserved_for_current),
    }
}

fn submit_prefetch_bricks_for_generation(
    state: &mut AppState,
    pool: &BrickReadPool,
    generation: mirante4d_data::DataGenerationId,
    layer_id: LayerId,
    policy: BrickPrefetchPolicy,
) -> Vec<BrickReadTicket> {
    let timepoints = prefetch_timepoints_for_state(state, policy.timepoint_horizon);
    if timepoints.is_empty() || state.visible_bricks.is_empty() {
        return Vec::new();
    }

    let mut tickets = Vec::new();
    let mut submitted_timepoints = Vec::new();
    for timepoint in timepoints {
        for brick in state.visible_bricks.clone() {
            match brick_is_occupied_for_stream(state, &layer_id, timepoint, brick) {
                Ok(true) => {}
                Ok(false) => {
                    state.brick_prefetch_skipped += 1;
                    continue;
                }
                Err(err) => {
                    state.brick_prefetch_failed += 1;
                    state.brick_prefetch_last_error = Some(err.to_string());
                    continue;
                }
            }
            if tickets.len() >= policy.max_requests {
                state.brick_prefetch_skipped += 1;
                continue;
            }
            let cancellation = CancellationToken::new();
            match pool.submit_brick_spec_for_generation(
                generation,
                BrickReadSpec {
                    layer_id: layer_id.clone(),
                    scale_level: state.brick_stream_scale_level,
                    timepoint,
                    brick_index: brick,
                    sample_region: None,
                    coalesced_brick_indices: Vec::new(),
                    priority: BrickRequestPriority::Prefetch,
                    queue_priority: 0,
                    cancellation,
                },
            ) {
                Ok(ticket) => {
                    state.brick_prefetch_requested += 1;
                    if submitted_timepoints.last() != Some(&timepoint) {
                        submitted_timepoints.push(timepoint);
                    }
                    tickets.push(ticket);
                }
                Err(err) => {
                    state.brick_prefetch_failed += 1;
                    state.brick_prefetch_last_error = Some(err.to_string());
                }
            }
        }
    }
    if !submitted_timepoints.is_empty() {
        let request_key = BrickPrefetchRequestKey {
            layer_id: layer_id.to_string(),
            scale_level: state.brick_stream_scale_level,
            active_timepoint: state.active_timepoint,
            timepoints: submitted_timepoints.clone(),
            visible_bricks: state.visible_bricks.clone(),
        };
        state.brick_prefetch_request_key = Some(request_key);
        state.brick_prefetch_timepoints = submitted_timepoints;
    }
    tickets
}

fn submit_warm_bricks_for_generation(
    state: &mut AppState,
    pool: &BrickReadPool,
    generation: mirante4d_data::DataGenerationId,
    layer_id: LayerId,
    max_requests: usize,
) -> Vec<BrickReadTicket> {
    let candidates = match warm_brick_candidates_for_state(state, &layer_id) {
        Ok(candidates) => candidates,
        Err(err) => {
            state.brick_warm_failed += 1;
            state.brick_warm_last_error = Some(err.to_string());
            return Vec::new();
        }
    };
    state.brick_warm_brick_count = candidates.len();
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut tickets = Vec::new();
    let mut submitted_bricks = Vec::new();
    for brick in candidates {
        match brick_is_occupied_for_stream(state, &layer_id, state.active_timepoint, brick) {
            Ok(true) => {}
            Ok(false) => {
                state.brick_warm_skipped += 1;
                continue;
            }
            Err(err) => {
                state.brick_warm_failed += 1;
                state.brick_warm_last_error = Some(err.to_string());
                continue;
            }
        }
        if tickets.len() >= max_requests {
            state.brick_warm_skipped += 1;
            continue;
        }
        let cancellation = CancellationToken::new();
        match pool.submit_brick_spec_for_generation(
            generation,
            BrickReadSpec {
                layer_id: layer_id.clone(),
                scale_level: state.brick_stream_scale_level,
                timepoint: state.active_timepoint,
                brick_index: brick,
                sample_region: None,
                coalesced_brick_indices: Vec::new(),
                priority: BrickRequestPriority::Warm,
                queue_priority: 0,
                cancellation,
            },
        ) {
            Ok(ticket) => {
                state.brick_warm_requested += 1;
                submitted_bricks.push(brick);
                tickets.push(ticket);
            }
            Err(err) => {
                state.brick_warm_failed += 1;
                state.brick_warm_last_error = Some(err.to_string());
            }
        }
    }

    if !submitted_bricks.is_empty() {
        state.brick_warm_request_key = Some(BrickWarmRequestKey {
            layer_id: layer_id.to_string(),
            scale_level: state.brick_stream_scale_level,
            timepoint: state.active_timepoint,
            bricks: submitted_bricks,
        });
    }
    tickets
}

fn brick_is_occupied_for_stream(
    state: &AppState,
    layer_id: &LayerId,
    timepoint: TimeIndex,
    brick_index: SpatialBrickIndex,
) -> anyhow::Result<bool> {
    Ok(state
        .dataset
        .brick_metadata_at_scale(
            layer_id,
            state.brick_stream_scale_level,
            timepoint,
            brick_index,
        )?
        .occupied)
}

fn warm_brick_candidates_for_state(
    state: &AppState,
    layer_id: &LayerId,
) -> anyhow::Result<Vec<SpatialBrickIndex>> {
    let grid_shape = state
        .dataset
        .brick_grid_shape_at_scale(layer_id, state.brick_stream_scale_level)?;
    Ok(spatial_warm_brick_candidates(
        &state.visible_bricks,
        grid_shape,
    ))
}

pub(crate) fn spatial_warm_brick_candidates(
    visible_bricks: &[SpatialBrickIndex],
    grid_shape: Shape3D,
) -> Vec<SpatialBrickIndex> {
    let visible: HashSet<_> = visible_bricks.iter().copied().collect();
    let mut candidates = HashSet::new();
    for brick in visible_bricks {
        let z_start = brick.z.saturating_sub(1);
        let y_start = brick.y.saturating_sub(1);
        let x_start = brick.x.saturating_sub(1);
        let z_end = brick.z.saturating_add(1).min(grid_shape.z - 1);
        let y_end = brick.y.saturating_add(1).min(grid_shape.y - 1);
        let x_end = brick.x.saturating_add(1).min(grid_shape.x - 1);
        for z in z_start..=z_end {
            for y in y_start..=y_end {
                for x in x_start..=x_end {
                    let candidate = SpatialBrickIndex::new(z, y, x);
                    if !visible.contains(&candidate) {
                        candidates.insert(candidate);
                    }
                }
            }
        }
    }
    let mut candidates: Vec<_> = candidates.into_iter().collect();
    candidates.sort_by_key(|brick| (brick.z, brick.y, brick.x));
    candidates
}

pub(crate) fn prefetch_timepoints_for_state(state: &AppState, horizon: u64) -> Vec<TimeIndex> {
    if state.timepoint_count <= 1 || horizon == 0 {
        return Vec::new();
    }
    let count = horizon.min(state.timepoint_count.saturating_sub(1));
    (1..=count)
        .map(|offset| TimeIndex((state.active_timepoint.0 + offset) % state.timepoint_count))
        .collect()
}

pub(crate) fn playback_timepoint_finished_loading(state: &AppState, timepoint: TimeIndex) -> bool {
    if state.active_timepoint != timepoint {
        return true;
    }
    if state.active_volume_u8.is_some()
        || state.active_volume.is_some()
        || state.active_volume_f32.is_some()
    {
        return true;
    }
    if current_resident_frame_ready(state) {
        return true;
    }
    let terminal = state
        .brick_stream_completed
        .saturating_add(state.brick_stream_failed)
        .saturating_add(state.brick_stream_cancelled)
        .saturating_add(state.brick_stream_stale);
    state.brick_stream_requested > 0 && terminal >= state.brick_stream_requested
}

pub(crate) fn apply_brick_read_outcome(state: &mut AppState, outcome: BrickReadOutcome) -> bool {
    apply_brick_read_outcomes(state, [outcome])
}

pub(crate) fn apply_brick_read_outcomes(
    state: &mut AppState,
    outcomes: impl IntoIterator<Item = BrickReadOutcome>,
) -> bool {
    let mut changed = false;
    let mut completed_current_bricks = Vec::new();
    for outcome in outcomes {
        changed |= apply_brick_read_outcome_to_batch(state, outcome, &mut completed_current_bricks);
    }
    if !completed_current_bricks.is_empty() {
        insert_resident_brick_payloads(state, completed_current_bricks);
    }
    changed
}

fn apply_brick_read_outcome_to_batch(
    state: &mut AppState,
    outcome: BrickReadOutcome,
    completed_current_bricks: &mut Vec<CompletedCurrentBrick>,
) -> bool {
    if outcome.generation_id.0 != state.brick_stream_generation {
        return false;
    }
    if outcome_matches_prefetch_request(state, &outcome) {
        apply_prefetch_outcome(state, outcome);
        return false;
    }
    if outcome_matches_warm_request(state, &outcome) {
        apply_warm_outcome(state, outcome);
        return false;
    }
    if !outcome_matches_current_request(state, &outcome) {
        return false;
    }
    let logical_request_count = outcome_logical_request_count(&outcome);
    match outcome.status {
        BrickReadStatus::Completed(payload) => {
            state.brick_stream_completed += 1;
            completed_current_bricks.push(CompletedCurrentBrick {
                layer_id: outcome.layer_id.to_string(),
                payload,
                histogram_sample: outcome.histogram_sample,
            });
        }
        BrickReadStatus::Cancelled => {
            state.brick_stream_cancelled += logical_request_count;
        }
        BrickReadStatus::Stale => {
            state.brick_stream_stale += logical_request_count;
        }
        BrickReadStatus::Failed(message) => {
            state.brick_stream_failed += logical_request_count;
            state.brick_stream_last_error = Some(message);
        }
    }
    state.brick_stream_complete = state.brick_stream_completed == state.brick_stream_requested
        && state.brick_stream_failed == 0
        && state.brick_stream_cancelled == 0
        && state.brick_stream_stale == 0;
    true
}

fn outcome_logical_request_count(outcome: &BrickReadOutcome) -> usize {
    let _ = outcome;
    1
}

fn apply_completed_current_brick(
    state: &mut AppState,
    layer_id: String,
    payload: BrickReadPayload,
    histogram_sample: Option<mirante4d_data::BrickHistogramSample>,
) {
    insert_resident_brick_payload(state, layer_id, payload, histogram_sample);
}

struct CompletedCurrentBrick {
    layer_id: String,
    payload: BrickReadPayload,
    histogram_sample: Option<mirante4d_data::BrickHistogramSample>,
}

fn insert_resident_brick_payloads(state: &mut AppState, bricks: Vec<CompletedCurrentBrick>) {
    let mut u8_by_layer: BTreeMap<String, Vec<VolumeBrickU8>> = BTreeMap::new();
    let mut u16_by_layer: BTreeMap<String, Vec<VolumeBrickU16>> = BTreeMap::new();
    let mut f32_by_layer: BTreeMap<String, Vec<VolumeBrickF32>> = BTreeMap::new();
    let mut histogram_changed = false;

    for completed in bricks {
        match completed.payload {
            BrickReadPayload::U8(brick) => {
                let brick = *brick;
                update_resident_histogram_sample_for_batch(
                    state,
                    resident_histogram_sample_key_for_u8_brick(completed.layer_id.clone(), &brick),
                    completed.histogram_sample,
                    &mut histogram_changed,
                );
                u8_by_layer
                    .entry(completed.layer_id)
                    .or_default()
                    .push(brick);
            }
            BrickReadPayload::U16(brick) => {
                let brick = *brick;
                update_resident_histogram_sample_for_batch(
                    state,
                    resident_histogram_sample_key_for_u16_brick(completed.layer_id.clone(), &brick),
                    completed.histogram_sample,
                    &mut histogram_changed,
                );
                u16_by_layer
                    .entry(completed.layer_id)
                    .or_default()
                    .push(brick);
            }
            BrickReadPayload::F32(brick) => {
                let brick = *brick;
                update_resident_histogram_sample_for_batch(
                    state,
                    resident_histogram_sample_key_for_f32_brick(completed.layer_id.clone(), &brick),
                    completed.histogram_sample,
                    &mut histogram_changed,
                );
                f32_by_layer
                    .entry(completed.layer_id)
                    .or_default()
                    .push(brick);
            }
            BrickReadPayload::Group(_) => {
                state.brick_stream_last_error =
                    Some("grouped payload reached resident brick streaming path".to_owned());
            }
        }
    }

    if histogram_changed {
        state.resident_histogram_generation = state.resident_histogram_generation.saturating_add(1);
        state.active_histogram_cache = None;
    }
    for (layer_id, new_bricks) in u8_by_layer {
        let bricks = state
            .resident_bricks_u8_by_layer
            .entry(layer_id.clone())
            .or_default();
        replace_resident_bricks(bricks, new_bricks);
        if layer_id == state.active_layer_id {
            state.resident_bricks_u8 = bricks.clone();
        }
    }
    for (layer_id, new_bricks) in u16_by_layer {
        let bricks = state
            .resident_bricks_by_layer
            .entry(layer_id.clone())
            .or_default();
        replace_resident_bricks(bricks, new_bricks);
        if layer_id == state.active_layer_id {
            state.resident_bricks = bricks.clone();
        }
    }
    for (layer_id, new_bricks) in f32_by_layer {
        let bricks = state
            .resident_bricks_f32_by_layer
            .entry(layer_id.clone())
            .or_default();
        replace_resident_bricks_f32(bricks, new_bricks);
        if layer_id == state.active_layer_id {
            state.resident_bricks_f32 = bricks.clone();
        }
    }
}

fn insert_resident_brick_payload(
    state: &mut AppState,
    layer_id: String,
    payload: BrickReadPayload,
    histogram_sample: Option<mirante4d_data::BrickHistogramSample>,
) {
    match payload {
        BrickReadPayload::U8(brick) => {
            let brick = *brick;
            update_resident_histogram_sample(
                state,
                resident_histogram_sample_key_for_u8_brick(layer_id.clone(), &brick),
                histogram_sample,
            );
            let bricks = state
                .resident_bricks_u8_by_layer
                .entry(layer_id.clone())
                .or_default();
            replace_resident_brick(bricks, brick);
            if layer_id == state.active_layer_id {
                state.resident_bricks_u8 = bricks.clone();
            }
        }
        BrickReadPayload::U16(brick) => {
            let brick = *brick;
            update_resident_histogram_sample(
                state,
                resident_histogram_sample_key_for_u16_brick(layer_id.clone(), &brick),
                histogram_sample,
            );
            let bricks = state
                .resident_bricks_by_layer
                .entry(layer_id.clone())
                .or_default();
            replace_resident_brick(bricks, brick);
            if layer_id == state.active_layer_id {
                state.resident_bricks = bricks.clone();
            }
        }
        BrickReadPayload::F32(brick) => {
            let brick = *brick;
            update_resident_histogram_sample(
                state,
                resident_histogram_sample_key_for_f32_brick(layer_id.clone(), &brick),
                histogram_sample,
            );
            let bricks = state
                .resident_bricks_f32_by_layer
                .entry(layer_id.clone())
                .or_default();
            replace_resident_brick_f32(bricks, brick);
            if layer_id == state.active_layer_id {
                state.resident_bricks_f32 = bricks.clone();
            }
        }
        BrickReadPayload::Group(_) => {
            state.brick_stream_last_error =
                Some("grouped payload reached resident brick streaming path".to_owned());
        }
    }
}

fn update_resident_histogram_sample(
    state: &mut AppState,
    key: crate::state::ResidentHistogramSampleKey,
    sample: Option<mirante4d_data::BrickHistogramSample>,
) {
    let mut changed = false;
    update_resident_histogram_sample_for_batch(state, key, sample, &mut changed);
    if changed {
        state.resident_histogram_generation = state.resident_histogram_generation.saturating_add(1);
        state.active_histogram_cache = None;
    }
}

fn update_resident_histogram_sample_for_batch(
    state: &mut AppState,
    key: crate::state::ResidentHistogramSampleKey,
    sample: Option<mirante4d_data::BrickHistogramSample>,
    changed: &mut bool,
) {
    let sample_changed = match sample {
        Some(sample) => {
            let sample_changed = state.resident_histogram_samples.get(&key) != Some(&sample);
            if sample_changed {
                state.resident_histogram_samples.insert(key, sample);
            }
            sample_changed
        }
        None => state.resident_histogram_samples.remove(&key).is_some(),
    };
    if sample_changed {
        *changed = true;
    }
}

fn replace_resident_bricks<T>(bricks: &mut Vec<T>, new_bricks: Vec<T>)
where
    T: ResidentBrickInfo,
{
    if new_bricks.is_empty() {
        return;
    }
    let replacements = new_bricks
        .iter()
        .map(|brick| (brick.brick_index(), brick.scale_level(), brick.timepoint()))
        .collect::<Vec<_>>();
    bricks.retain(|existing| {
        !replacements.iter().any(|(index, scale_level, timepoint)| {
            existing.brick_index() == *index
                && existing.scale_level() == *scale_level
                && existing.timepoint() == *timepoint
        })
    });
    bricks.extend(new_bricks);
    bricks.sort_by_key(|brick| {
        (
            brick.brick_index().z,
            brick.brick_index().y,
            brick.brick_index().x,
        )
    });
}

fn replace_resident_bricks_f32(bricks: &mut Vec<VolumeBrickF32>, new_bricks: Vec<VolumeBrickF32>) {
    if new_bricks.is_empty() {
        return;
    }
    let replacements = new_bricks
        .iter()
        .map(|brick| (brick.brick_index, brick.scale_level, brick.volume.timepoint))
        .collect::<Vec<_>>();
    bricks.retain(|existing| {
        !replacements.iter().any(|(index, scale_level, timepoint)| {
            existing.brick_index == *index
                && existing.scale_level == *scale_level
                && existing.volume.timepoint == *timepoint
        })
    });
    bricks.extend(new_bricks);
    bricks.sort_by_key(|brick| {
        (
            brick.brick_index.z,
            brick.brick_index.y,
            brick.brick_index.x,
        )
    });
}

fn replace_resident_brick<T>(bricks: &mut Vec<T>, brick: T)
where
    T: ResidentBrickInfo,
{
    let index = brick.brick_index();
    let scale_level = brick.scale_level();
    let timepoint = brick.timepoint();
    bricks.retain(|existing| {
        !(existing.brick_index() == index
            && existing.scale_level() == scale_level
            && existing.timepoint() == timepoint)
    });
    bricks.push(brick);
    bricks.sort_by_key(|brick| {
        (
            brick.brick_index().z,
            brick.brick_index().y,
            brick.brick_index().x,
        )
    });
}

fn replace_resident_brick_f32(bricks: &mut Vec<VolumeBrickF32>, brick: VolumeBrickF32) {
    let index = brick.brick_index;
    let scale_level = brick.scale_level;
    let timepoint = brick.volume.timepoint;
    bricks.retain(|existing| {
        !(existing.brick_index == index
            && existing.scale_level == scale_level
            && existing.volume.timepoint == timepoint)
    });
    bricks.push(brick);
    bricks.sort_by_key(|brick| {
        (
            brick.brick_index.z,
            brick.brick_index.y,
            brick.brick_index.x,
        )
    });
}

fn take_prefetched_brick_payload(
    state: &mut AppState,
    layer_id: &LayerId,
    scale_level: u32,
    timepoint: TimeIndex,
    brick_index: SpatialBrickIndex,
    sample_region: Option<VolumeRegion>,
) -> Option<BrickReadPayload> {
    let position = state
        .prefetched_brick_payloads
        .iter()
        .position(|prefetch| {
            prefetch.layer_id == layer_id.as_str()
                && prefetch.scale_level == scale_level
                && prefetch.timepoint == timepoint
                && prefetch.brick_index == brick_index
                && prefetch.sample_region == sample_region
        })?;
    Some(
        state
            .prefetched_brick_payloads
            .swap_remove(position)
            .payload,
    )
}

fn store_prefetched_brick_payload(
    state: &mut AppState,
    layer_id: String,
    scale_level: u32,
    timepoint: TimeIndex,
    brick_index: SpatialBrickIndex,
    sample_region: Option<VolumeRegion>,
    payload: BrickReadPayload,
) {
    if let Some(position) = state.prefetched_brick_payloads.iter().position(|prefetch| {
        prefetch.layer_id == layer_id
            && prefetch.scale_level == scale_level
            && prefetch.timepoint == timepoint
            && prefetch.brick_index == brick_index
            && prefetch.sample_region == sample_region
    }) {
        state.prefetched_brick_payloads.swap_remove(position);
    }
    state
        .prefetched_brick_payloads
        .push(PrefetchedBrickPayload {
            layer_id,
            scale_level,
            timepoint,
            brick_index,
            sample_region,
            payload,
        });
    if state.prefetched_brick_payloads.len() > PREFETCHED_BRICK_PAYLOAD_LIMIT {
        let overflow = state
            .prefetched_brick_payloads
            .len()
            .saturating_sub(PREFETCHED_BRICK_PAYLOAD_LIMIT);
        state.prefetched_brick_payloads.drain(0..overflow);
    }
}

fn outcome_matches_current_request(state: &AppState, outcome: &BrickReadOutcome) -> bool {
    let Some(key) = &state.brick_stream_request_key else {
        return false;
    };
    let Some(region_request) = key
        .source_regions
        .iter()
        .find(|request| request.brick_index == outcome.brick_index)
    else {
        return false;
    };
    outcome.priority == BrickRequestPriority::CurrentFrame
        && outcome.scale_level == state.brick_stream_scale_level
        && key.layer_ids.contains(&outcome.layer_id.to_string())
        && outcome.timepoint == state.active_timepoint
        && state.visible_bricks.contains(&outcome.brick_index)
        && outcome.sample_region == region_request.worker_sample_region
}

fn outcome_matches_prefetch_request(state: &AppState, outcome: &BrickReadOutcome) -> bool {
    let Some(key) = &state.brick_prefetch_request_key else {
        return false;
    };
    outcome.priority == BrickRequestPriority::Prefetch
        && outcome.scale_level == state.brick_stream_scale_level
        && outcome.layer_id.to_string() == key.layer_id
        && state.brick_prefetch_timepoints.contains(&outcome.timepoint)
        && state.visible_bricks.contains(&outcome.brick_index)
        && outcome.sample_region.is_none()
}

fn outcome_matches_warm_request(state: &AppState, outcome: &BrickReadOutcome) -> bool {
    let Some(key) = &state.brick_warm_request_key else {
        return false;
    };
    outcome.priority == BrickRequestPriority::Warm
        && outcome.scale_level == key.scale_level
        && outcome.layer_id.to_string() == key.layer_id
        && outcome.timepoint == key.timepoint
        && key.bricks.contains(&outcome.brick_index)
        && outcome.sample_region.is_none()
}

fn apply_prefetch_outcome(state: &mut AppState, outcome: BrickReadOutcome) {
    match outcome.status {
        BrickReadStatus::Completed(payload) => {
            state.brick_prefetch_completed += 1;
            store_prefetched_brick_payload(
                state,
                outcome.layer_id.to_string(),
                outcome.scale_level,
                outcome.timepoint,
                outcome.brick_index,
                outcome.sample_region,
                payload,
            );
        }
        BrickReadStatus::Cancelled => {
            state.brick_prefetch_cancelled += 1;
        }
        BrickReadStatus::Stale => {
            state.brick_prefetch_stale += 1;
        }
        BrickReadStatus::Failed(message) => {
            state.brick_prefetch_failed += 1;
            state.brick_prefetch_last_error = Some(message);
        }
    }
}

fn apply_warm_outcome(state: &mut AppState, outcome: BrickReadOutcome) {
    match outcome.status {
        BrickReadStatus::Completed(payload) => {
            state.brick_warm_completed += 1;
            store_prefetched_brick_payload(
                state,
                outcome.layer_id.to_string(),
                outcome.scale_level,
                outcome.timepoint,
                outcome.brick_index,
                outcome.sample_region,
                payload,
            );
        }
        BrickReadStatus::Cancelled => {
            state.brick_warm_cancelled += 1;
        }
        BrickReadStatus::Stale => {
            state.brick_warm_stale += 1;
        }
        BrickReadStatus::Failed(message) => {
            state.brick_warm_failed += 1;
            state.brick_warm_last_error = Some(message);
        }
    }
}
