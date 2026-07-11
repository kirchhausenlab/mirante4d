use std::collections::{BTreeMap, HashSet};

use mirante4d_application::{ApplicationSnapshot, WorkspaceSnapshot};
use mirante4d_data::{
    BrickMetadata, BrickReadOutcome, BrickReadPayload, BrickReadPool, BrickReadSpec,
    BrickReadStatus, BrickReadTicket, BrickRequestPriority, CancellationToken, DenseVolumeF32,
    DenseVolumeU8, DenseVolumeU16, SpatialBrickIndex, VolumeBrickF32, VolumeBrickU8,
    VolumeBrickU16, VolumeRegion, translated_region_grid_to_world,
};
use mirante4d_domain::{IntensityDType, LogicalLayerKey, Shape3D, TimeIndex};
use mirante4d_format::LayerId;
use mirante4d_project_model::ViewState;

use crate::current_runtime::{
    analysis::CurrentAnalysisRuntime, dataset::CurrentDatasetRuntime, render::CurrentRenderRuntime,
};
use crate::histogram::{
    resident_histogram_sample_key_for_f32_brick, resident_histogram_sample_key_for_u8_brick,
    resident_histogram_sample_key_for_u16_brick,
};

const PREFETCHED_BRICK_PAYLOAD_LIMIT: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrickStreamRequestKey {
    layer_ids: Vec<LayerId>,
    scale_level: u32,
    timepoint: TimeIndex,
    visible_bricks: Vec<SpatialBrickIndex>,
    source_regions: Vec<BrickSourceRegion>,
}

#[path = "brick_streaming_plan.rs"]
pub(crate) mod brick_streaming_plan;
pub(crate) use brick_streaming_plan::*;

pub(crate) fn current_resident_frame_ready(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
) -> bool {
    if !dataset.brick_stream_complete {
        return false;
    }
    if !visible_resident_layers_ready(snapshot, dataset) {
        return false;
    }
    let Ok(expected_key) = current_brick_stream_request_key(snapshot, dataset, render) else {
        return false;
    };
    dataset.brick_stream_request_key.as_ref() == Some(&expected_key)
}

pub(crate) fn brick_runtime_work_active(dataset: &CurrentDatasetRuntime) -> bool {
    current_brick_stream_work_active(dataset)
        || outstanding_work(
            dataset.brick_prefetch_requested,
            dataset.brick_prefetch_completed,
            dataset.brick_prefetch_cancelled,
            dataset.brick_prefetch_stale,
            dataset.brick_prefetch_failed,
        )
        || outstanding_work(
            dataset.brick_warm_requested,
            dataset.brick_warm_completed,
            dataset.brick_warm_cancelled,
            dataset.brick_warm_stale,
            dataset.brick_warm_failed,
        )
}

fn current_brick_stream_work_active(dataset: &CurrentDatasetRuntime) -> bool {
    outstanding_work(
        dataset.brick_stream_requested,
        dataset.brick_stream_completed,
        dataset.brick_stream_cancelled,
        dataset.brick_stream_stale,
        dataset.brick_stream_failed,
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

pub(crate) fn create_brick_read_pool(dataset: &CurrentDatasetRuntime) -> Option<BrickReadPool> {
    match BrickReadPool::new(
        dataset.dataset.clone(),
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

pub(crate) fn reset_prefetch_state(dataset: &mut CurrentDatasetRuntime) {
    dataset.brick_prefetch_timepoints.clear();
    dataset.brick_prefetch_requested = 0;
    dataset.brick_prefetch_completed = 0;
    dataset.brick_prefetch_cancelled = 0;
    dataset.brick_prefetch_stale = 0;
    dataset.brick_prefetch_failed = 0;
    dataset.brick_prefetch_skipped = 0;
    dataset.brick_prefetch_last_error = None;
    dataset.brick_prefetch_request_key = None;
    reset_warm_state(dataset);
}

pub(crate) fn reset_warm_state(dataset: &mut CurrentDatasetRuntime) {
    dataset.brick_warm_brick_count = 0;
    dataset.brick_warm_requested = 0;
    dataset.brick_warm_completed = 0;
    dataset.brick_warm_cancelled = 0;
    dataset.brick_warm_stale = 0;
    dataset.brick_warm_failed = 0;
    dataset.brick_warm_skipped = 0;
    dataset.brick_warm_last_error = None;
    dataset.brick_warm_request_key = None;
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
    snapshot: &ApplicationSnapshot,
    dataset: &mut CurrentDatasetRuntime,
    analysis: &mut CurrentAnalysisRuntime,
    render: &CurrentRenderRuntime,
    pool: &BrickReadPool,
) -> BrickSubmissionResult {
    submit_visible_bricks_to_pool_with_options(
        snapshot,
        dataset,
        analysis,
        render,
        pool,
        BrickSubmissionOptions::DEFAULT,
    )
}

pub(crate) fn submit_visible_bricks_to_pool_with_options(
    snapshot: &ApplicationSnapshot,
    dataset: &mut CurrentDatasetRuntime,
    analysis: &mut CurrentAnalysisRuntime,
    render: &CurrentRenderRuntime,
    pool: &BrickReadPool,
    options: BrickSubmissionOptions,
) -> BrickSubmissionResult {
    let layer_ids = match stream_layer_ids_for_snapshot(snapshot, dataset) {
        Ok(layer_ids) => layer_ids,
        Err(err) => {
            dataset.brick_stream_last_error = Some(err.to_string());
            return BrickSubmissionResult::default();
        }
    };
    if layer_ids.is_empty() {
        return BrickSubmissionResult::default();
    }
    let request_key = match current_brick_stream_request_key(snapshot, dataset, render) {
        Ok(request_key) => request_key,
        Err(err) => {
            dataset.brick_stream_last_error = Some(err.to_string());
            return BrickSubmissionResult::default();
        }
    };
    if dataset.brick_stream_request_key.as_ref() == Some(&request_key)
        && dataset.brick_stream_failed == 0
        && (dataset.brick_stream_complete || current_brick_stream_work_active(dataset))
    {
        return BrickSubmissionResult::default();
    }
    let view = view_for_snapshot(snapshot);
    let timepoint = view.timepoint();
    let active_layer_id = match physical_layer_id_for_key(dataset, view.active_layer()) {
        Ok(layer_id) => layer_id,
        Err(err) => {
            dataset.brick_stream_last_error = Some(err.to_string());
            return BrickSubmissionResult::default();
        }
    };
    let generation = pool.advance_generation();
    dataset.brick_stream_generation = generation.0;
    dataset.brick_stream_requested = 0;
    dataset.brick_stream_completed = 0;
    dataset.brick_stream_cancelled = 0;
    dataset.brick_stream_stale = 0;
    dataset.brick_stream_failed = 0;
    dataset.brick_stream_last_error = None;
    dataset.brick_stream_complete = render.visible_bricks.is_empty();
    let request_regions = request_region_map(&request_key);
    let retained_resident_changed = retain_current_resident_bricks_for_request(
        dataset,
        analysis,
        render,
        &active_layer_id,
        &layer_ids,
        dataset.brick_stream_scale_level,
        timepoint,
        &request_regions,
    );
    dataset.brick_stream_request_key = Some(request_key);
    reset_prefetch_state(dataset);
    let mut result = BrickSubmissionResult {
        current_changed: true,
        resident_changed: retained_resident_changed,
        ..BrickSubmissionResult::default()
    };
    let mut completed_prefetch_promotions = Vec::new();

    for layer_id in &layer_ids {
        for brick in render.visible_bricks.clone() {
            let Some(region_request) = request_regions.get(&brick).copied() else {
                dataset.brick_stream_failed += 1;
                dataset.brick_stream_complete = false;
                dataset.brick_stream_last_error = Some(format!(
                    "missing source region for visible brick z={}, y={}, x={}",
                    brick.z, brick.y, brick.x
                ));
                continue;
            };
            dataset.brick_stream_requested += 1;
            if resident_current_brick_exists(
                dataset,
                layer_id,
                dataset.brick_stream_scale_level,
                timepoint,
                brick,
                region_request.resident_region,
            ) {
                dataset.brick_stream_completed += 1;
                continue;
            }
            match materialize_empty_current_brick(
                snapshot,
                dataset,
                analysis,
                &active_layer_id,
                layer_id,
                timepoint,
                brick,
                region_request.resident_region,
            ) {
                Ok(true) => {
                    result.resident_changed = true;
                    continue;
                }
                Ok(false) => {}
                Err(err) => {
                    dataset.brick_stream_failed += 1;
                    dataset.brick_stream_complete = false;
                    dataset.brick_stream_last_error = Some(err.to_string());
                    continue;
                }
            }
            if let Some(payload) = take_prefetched_brick_payload(
                dataset,
                layer_id,
                dataset.brick_stream_scale_level,
                timepoint,
                brick,
                region_request.worker_sample_region,
            ) {
                dataset.brick_stream_completed += 1;
                completed_prefetch_promotions.push(CompletedCurrentBrick {
                    layer_id: layer_id.clone(),
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
                    scale_level: dataset.brick_stream_scale_level,
                    timepoint,
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
                    dataset.brick_stream_failed += 1;
                    dataset.brick_stream_complete = false;
                    dataset.brick_stream_last_error = Some(err.to_string());
                }
            }
        }
    }
    if !completed_prefetch_promotions.is_empty() {
        insert_resident_brick_payloads(
            dataset,
            analysis,
            &active_layer_id,
            completed_prefetch_promotions,
        );
    }
    dataset.brick_stream_complete = dataset.brick_stream_failed == 0
        && dataset.brick_stream_cancelled == 0
        && dataset.brick_stream_stale == 0
        && dataset.brick_stream_completed == dataset.brick_stream_requested;
    if dataset.brick_stream_failed == 0
        && options.submit_prefetch
        && let Some(primary_visible_layer_id) =
            primary_visible_layer_id(snapshot, dataset, &active_layer_id)
    {
        let prefetch_policy = prefetch_policy(dataset, pool.queue_capacity());
        result.prefetch_tickets = submit_prefetch_bricks_for_generation(
            snapshot,
            dataset,
            render,
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
                snapshot,
                dataset,
                render,
                pool,
                generation,
                primary_visible_layer_id,
                warm_budget,
            );
        }
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn retain_current_resident_bricks_for_request(
    dataset: &mut CurrentDatasetRuntime,
    analysis: &mut CurrentAnalysisRuntime,
    render: &CurrentRenderRuntime,
    active_layer_id: &LayerId,
    layer_ids: &[LayerId],
    scale_level: u32,
    timepoint: TimeIndex,
    request_regions: &BTreeMap<SpatialBrickIndex, BrickSourceRegion>,
) -> bool {
    let resident_count_before = resident_brick_count(dataset);
    let visible: HashSet<SpatialBrickIndex> = render.visible_bricks.iter().copied().collect();
    let layers: HashSet<LayerId> = layer_ids.iter().cloned().collect();
    retain_u16_resident_map(
        &mut dataset.resident_bricks_u8_by_layer,
        &layers,
        &visible,
        scale_level,
        timepoint,
        request_regions,
    );
    retain_u16_resident_map(
        &mut dataset.resident_bricks_by_layer,
        &layers,
        &visible,
        scale_level,
        timepoint,
        request_regions,
    );
    retain_f32_resident_map(
        &mut dataset.resident_bricks_f32_by_layer,
        &layers,
        &visible,
        scale_level,
        timepoint,
        request_regions,
    );
    dataset.resident_bricks_u8 = dataset
        .resident_bricks_u8_by_layer
        .get(active_layer_id)
        .cloned()
        .unwrap_or_default();
    dataset.resident_bricks = dataset
        .resident_bricks_by_layer
        .get(active_layer_id)
        .cloned()
        .unwrap_or_default();
    dataset.resident_bricks_f32 = dataset
        .resident_bricks_f32_by_layer
        .get(active_layer_id)
        .cloned()
        .unwrap_or_default();
    let sample_count_before = analysis.resident_histogram_samples.len();
    analysis.resident_histogram_samples.retain(|key, _| {
        layers
            .iter()
            .any(|layer_id| layer_id.as_str() == key.layer_id)
            && visible.contains(&key.brick_index)
            && key.scale_level == scale_level
            && key.timepoint == timepoint
    });
    if analysis.resident_histogram_samples.len() != sample_count_before {
        analysis.resident_histogram_generation =
            analysis.resident_histogram_generation.saturating_add(1);
        analysis.active_histogram_cache = None;
    }
    resident_count_before != resident_brick_count(dataset)
        || sample_count_before != analysis.resident_histogram_samples.len()
}

fn resident_brick_count(dataset: &CurrentDatasetRuntime) -> usize {
    dataset
        .resident_bricks_u8_by_layer
        .values()
        .map(Vec::len)
        .sum::<usize>()
        + dataset
            .resident_bricks_by_layer
            .values()
            .map(Vec::len)
            .sum::<usize>()
        + dataset
            .resident_bricks_f32_by_layer
            .values()
            .map(Vec::len)
            .sum::<usize>()
}

fn retain_u16_resident_map<T>(
    map: &mut BTreeMap<LayerId, Vec<T>>,
    layers: &HashSet<LayerId>,
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
    map: &mut BTreeMap<LayerId, Vec<VolumeBrickF32>>,
    layers: &HashSet<LayerId>,
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
    dataset: &CurrentDatasetRuntime,
    layer_id: &LayerId,
    scale_level: u32,
    timepoint: TimeIndex,
    brick_index: SpatialBrickIndex,
    region: VolumeRegion,
) -> bool {
    dataset
        .resident_bricks_u8_by_layer
        .get(layer_id)
        .is_some_and(|bricks| {
            bricks.iter().any(|brick| {
                brick.scale_level == scale_level
                    && brick.volume.timepoint == timepoint
                    && brick.brick_index == brick_index
                    && brick.region == region
            })
        })
        || dataset
            .resident_bricks_by_layer
            .get(layer_id)
            .is_some_and(|bricks| {
                bricks.iter().any(|brick| {
                    brick.scale_level == scale_level
                        && brick.volume.timepoint == timepoint
                        && brick.brick_index == brick_index
                        && brick.region == region
                })
            })
        || dataset
            .resident_bricks_f32_by_layer
            .get(layer_id)
            .is_some_and(|bricks| {
                bricks.iter().any(|brick| {
                    brick.scale_level == scale_level
                        && brick.volume.timepoint == timepoint
                        && brick.brick_index == brick_index
                        && brick.region == region
                })
            })
}

fn primary_visible_layer_id(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    active_layer_id: &LayerId,
) -> Option<LayerId> {
    let layer_ids = stream_layer_ids_for_snapshot(snapshot, dataset).ok()?;
    layer_ids
        .iter()
        .find(|layer_id| *layer_id == active_layer_id)
        .cloned()
        .or_else(|| layer_ids.first().cloned())
}

#[allow(clippy::too_many_arguments)]
fn materialize_empty_current_brick(
    snapshot: &ApplicationSnapshot,
    dataset: &mut CurrentDatasetRuntime,
    analysis: &mut CurrentAnalysisRuntime,
    active_layer_id: &LayerId,
    layer_id: &LayerId,
    timepoint: TimeIndex,
    brick_index: SpatialBrickIndex,
    region: VolumeRegion,
) -> anyhow::Result<bool> {
    let metadata = dataset.dataset.brick_metadata_at_scale(
        layer_id,
        dataset.brick_stream_scale_level,
        timepoint,
        brick_index,
    )?;
    if metadata.occupied {
        return Ok(false);
    }
    let key = logical_layer_key_for_id(dataset, layer_id)?;
    let layer = snapshot
        .catalog()
        .layer(key)
        .ok_or_else(|| anyhow::anyhow!("layer {layer_id} is absent from the canonical catalog"))?;
    let payload = zero_resident_brick_payload(
        dataset,
        layer_id,
        timepoint,
        metadata,
        region,
        layer.dtype(),
    )?;
    dataset.brick_stream_completed += 1;
    apply_completed_current_brick(
        dataset,
        analysis,
        active_layer_id,
        layer_id.clone(),
        payload,
        None,
    );
    Ok(true)
}

pub(crate) fn zero_resident_brick_payload(
    dataset: &CurrentDatasetRuntime,
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
            dataset
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
                    dataset.dataset.dataset_id().clone(),
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
                    dataset.dataset.dataset_id().clone(),
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
                    dataset.dataset.dataset_id().clone(),
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
                    dataset.dataset.dataset_id().clone(),
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
                    dataset.dataset.dataset_id().clone(),
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
                    dataset.dataset.dataset_id().clone(),
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

pub(super) fn view_for_snapshot(snapshot: &ApplicationSnapshot) -> &ViewState {
    match snapshot.workspace() {
        WorkspaceSnapshot::Unbound { workspace } => workspace.view(),
        WorkspaceSnapshot::Bound { project, .. } => project.view(),
    }
}

pub(crate) fn physical_layer_id_for_key(
    dataset: &CurrentDatasetRuntime,
    key: LogicalLayerKey,
) -> anyhow::Result<LayerId> {
    let layer = dataset
        .dataset
        .manifest()
        .layers
        .get(usize::try_from(key.ordinal())?)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "logical layer key {} has no current physical layer",
                key.ordinal()
            )
        })?;
    Ok(LayerId::new(&layer.id)?)
}

fn logical_layer_key_for_id(
    dataset: &CurrentDatasetRuntime,
    layer_id: &LayerId,
) -> anyhow::Result<LogicalLayerKey> {
    let ordinal = dataset
        .dataset
        .manifest()
        .layers
        .iter()
        .position(|layer| layer.id == layer_id.as_str())
        .ok_or_else(|| anyhow::anyhow!("physical layer {layer_id} is absent from the manifest"))?;
    Ok(LogicalLayerKey::new(u32::try_from(ordinal)?))
}

pub(crate) fn stream_layer_ids_for_snapshot(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
) -> anyhow::Result<Vec<LayerId>> {
    let view = view_for_snapshot(snapshot);
    let active_layer = snapshot
        .catalog()
        .layer(view.active_layer())
        .ok_or_else(|| anyhow::anyhow!("active logical layer is absent from the catalog"))?;
    let mut layer_ids = Vec::new();
    for layer_view in view.layers() {
        if !layer_view.visible() {
            continue;
        }
        let layer = snapshot
            .catalog()
            .layer(layer_view.layer_key())
            .ok_or_else(|| anyhow::anyhow!("visible logical layer is absent from the catalog"))?;
        let layer_id = physical_layer_id_for_key(dataset, layer.key())?;
        if layer.shape().spatial() != active_layer.shape().spatial() {
            anyhow::bail!(
                "visible layer {} has shape {:?}, expected active shape {:?}",
                layer_id,
                layer.shape().spatial(),
                active_layer.shape().spatial()
            );
        }
        if view.timepoint().get() >= layer.shape().t() {
            anyhow::bail!(
                "visible layer {} has no timepoint {}",
                layer_id,
                view.timepoint().get()
            );
        }
        layer_ids.push(layer_id);
    }
    Ok(layer_ids)
}

fn prefetch_policy(dataset: &CurrentDatasetRuntime, queue_capacity: usize) -> BrickPrefetchPolicy {
    let reserved_for_current = dataset.brick_stream_requested;
    BrickPrefetchPolicy {
        timepoint_horizon: 2,
        max_requests: queue_capacity.saturating_sub(reserved_for_current),
    }
}

fn submit_prefetch_bricks_for_generation(
    snapshot: &ApplicationSnapshot,
    dataset: &mut CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    pool: &BrickReadPool,
    generation: mirante4d_data::DataGenerationId,
    layer_id: LayerId,
    policy: BrickPrefetchPolicy,
) -> Vec<BrickReadTicket> {
    let view = view_for_snapshot(snapshot);
    let timepoint_count = snapshot
        .catalog()
        .layer(view.active_layer())
        .map(|layer| layer.shape().t())
        .unwrap_or(0);
    let timepoints =
        prefetch_timepoints(view.timepoint(), timepoint_count, policy.timepoint_horizon);
    if timepoints.is_empty() || render.visible_bricks.is_empty() {
        return Vec::new();
    }

    let mut tickets = Vec::new();
    let mut submitted_timepoints = Vec::new();
    for timepoint in timepoints {
        for brick in render.visible_bricks.clone() {
            match brick_is_occupied_for_stream(dataset, &layer_id, timepoint, brick) {
                Ok(true) => {}
                Ok(false) => {
                    dataset.brick_prefetch_skipped += 1;
                    continue;
                }
                Err(err) => {
                    dataset.brick_prefetch_failed += 1;
                    dataset.brick_prefetch_last_error = Some(err.to_string());
                    continue;
                }
            }
            if tickets.len() >= policy.max_requests {
                dataset.brick_prefetch_skipped += 1;
                continue;
            }
            let cancellation = CancellationToken::new();
            match pool.submit_brick_spec_for_generation(
                generation,
                BrickReadSpec {
                    layer_id: layer_id.clone(),
                    scale_level: dataset.brick_stream_scale_level,
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
                    dataset.brick_prefetch_requested += 1;
                    if submitted_timepoints.last() != Some(&timepoint) {
                        submitted_timepoints.push(timepoint);
                    }
                    tickets.push(ticket);
                }
                Err(err) => {
                    dataset.brick_prefetch_failed += 1;
                    dataset.brick_prefetch_last_error = Some(err.to_string());
                }
            }
        }
    }
    if !submitted_timepoints.is_empty() {
        let request_key = BrickPrefetchRequestKey {
            layer_id: layer_id.clone(),
            scale_level: dataset.brick_stream_scale_level,
            active_timepoint: view.timepoint(),
            timepoints: submitted_timepoints.clone(),
            visible_bricks: render.visible_bricks.clone(),
        };
        dataset.brick_prefetch_request_key = Some(request_key);
        dataset.brick_prefetch_timepoints = submitted_timepoints;
    }
    tickets
}

fn submit_warm_bricks_for_generation(
    snapshot: &ApplicationSnapshot,
    dataset: &mut CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    pool: &BrickReadPool,
    generation: mirante4d_data::DataGenerationId,
    layer_id: LayerId,
    max_requests: usize,
) -> Vec<BrickReadTicket> {
    let candidates = match warm_brick_candidates(dataset, render, &layer_id) {
        Ok(candidates) => candidates,
        Err(err) => {
            dataset.brick_warm_failed += 1;
            dataset.brick_warm_last_error = Some(err.to_string());
            return Vec::new();
        }
    };
    dataset.brick_warm_brick_count = candidates.len();
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut tickets = Vec::new();
    let mut submitted_bricks = Vec::new();
    let timepoint = view_for_snapshot(snapshot).timepoint();
    for brick in candidates {
        match brick_is_occupied_for_stream(dataset, &layer_id, timepoint, brick) {
            Ok(true) => {}
            Ok(false) => {
                dataset.brick_warm_skipped += 1;
                continue;
            }
            Err(err) => {
                dataset.brick_warm_failed += 1;
                dataset.brick_warm_last_error = Some(err.to_string());
                continue;
            }
        }
        if tickets.len() >= max_requests {
            dataset.brick_warm_skipped += 1;
            continue;
        }
        let cancellation = CancellationToken::new();
        match pool.submit_brick_spec_for_generation(
            generation,
            BrickReadSpec {
                layer_id: layer_id.clone(),
                scale_level: dataset.brick_stream_scale_level,
                timepoint,
                brick_index: brick,
                sample_region: None,
                coalesced_brick_indices: Vec::new(),
                priority: BrickRequestPriority::Warm,
                queue_priority: 0,
                cancellation,
            },
        ) {
            Ok(ticket) => {
                dataset.brick_warm_requested += 1;
                submitted_bricks.push(brick);
                tickets.push(ticket);
            }
            Err(err) => {
                dataset.brick_warm_failed += 1;
                dataset.brick_warm_last_error = Some(err.to_string());
            }
        }
    }

    if !submitted_bricks.is_empty() {
        dataset.brick_warm_request_key = Some(BrickWarmRequestKey {
            layer_id: layer_id.clone(),
            scale_level: dataset.brick_stream_scale_level,
            timepoint,
            bricks: submitted_bricks,
        });
    }
    tickets
}

fn brick_is_occupied_for_stream(
    dataset: &CurrentDatasetRuntime,
    layer_id: &LayerId,
    timepoint: TimeIndex,
    brick_index: SpatialBrickIndex,
) -> anyhow::Result<bool> {
    Ok(dataset
        .dataset
        .brick_metadata_at_scale(
            layer_id,
            dataset.brick_stream_scale_level,
            timepoint,
            brick_index,
        )?
        .occupied)
}

fn warm_brick_candidates(
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    layer_id: &LayerId,
) -> anyhow::Result<Vec<SpatialBrickIndex>> {
    let grid_shape = dataset
        .dataset
        .brick_grid_shape_at_scale(layer_id, dataset.brick_stream_scale_level)?;
    Ok(spatial_warm_brick_candidates(
        &render.visible_bricks,
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
        let z_end = brick.z.saturating_add(1).min(grid_shape.z() - 1);
        let y_end = brick.y.saturating_add(1).min(grid_shape.y() - 1);
        let x_end = brick.x.saturating_add(1).min(grid_shape.x() - 1);
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

pub(crate) fn prefetch_timepoints(
    active_timepoint: TimeIndex,
    timepoint_count: u64,
    horizon: u64,
) -> Vec<TimeIndex> {
    if timepoint_count <= 1 || horizon == 0 {
        return Vec::new();
    }
    let count = horizon.min(timepoint_count.saturating_sub(1));
    (1..=count)
        .map(|offset| TimeIndex::new((active_timepoint.get() + offset) % timepoint_count))
        .collect()
}

pub(crate) fn playback_timepoint_finished_loading(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    timepoint: TimeIndex,
) -> bool {
    if view_for_snapshot(snapshot).timepoint() != timepoint {
        return true;
    }
    if dataset.active_volume_u8.is_some()
        || dataset.active_volume.is_some()
        || dataset.active_volume_f32.is_some()
    {
        return true;
    }
    if current_resident_frame_ready(snapshot, dataset, render) {
        return true;
    }
    let terminal = dataset
        .brick_stream_completed
        .saturating_add(dataset.brick_stream_failed)
        .saturating_add(dataset.brick_stream_cancelled)
        .saturating_add(dataset.brick_stream_stale);
    dataset.brick_stream_requested > 0 && terminal >= dataset.brick_stream_requested
}

pub(crate) fn apply_brick_read_outcome(
    snapshot: &ApplicationSnapshot,
    dataset: &mut CurrentDatasetRuntime,
    analysis: &mut CurrentAnalysisRuntime,
    render: &CurrentRenderRuntime,
    outcome: BrickReadOutcome,
) -> bool {
    apply_brick_read_outcomes(snapshot, dataset, analysis, render, [outcome])
}

pub(crate) fn apply_brick_read_outcomes(
    snapshot: &ApplicationSnapshot,
    dataset: &mut CurrentDatasetRuntime,
    analysis: &mut CurrentAnalysisRuntime,
    render: &CurrentRenderRuntime,
    outcomes: impl IntoIterator<Item = BrickReadOutcome>,
) -> bool {
    let Ok(active_layer_id) =
        physical_layer_id_for_key(dataset, view_for_snapshot(snapshot).active_layer())
    else {
        return false;
    };
    let mut changed = false;
    let mut completed_current_bricks = Vec::new();
    for outcome in outcomes {
        changed |= apply_brick_read_outcome_to_batch(
            snapshot,
            dataset,
            render,
            outcome,
            &mut completed_current_bricks,
        );
    }
    if !completed_current_bricks.is_empty() {
        insert_resident_brick_payloads(
            dataset,
            analysis,
            &active_layer_id,
            completed_current_bricks,
        );
    }
    changed
}

fn apply_brick_read_outcome_to_batch(
    snapshot: &ApplicationSnapshot,
    dataset: &mut CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    outcome: BrickReadOutcome,
    completed_current_bricks: &mut Vec<CompletedCurrentBrick>,
) -> bool {
    if outcome.generation_id.0 != dataset.brick_stream_generation {
        return false;
    }
    if outcome_matches_prefetch_request(dataset, render, &outcome) {
        apply_prefetch_outcome(dataset, outcome);
        return false;
    }
    if outcome_matches_warm_request(dataset, &outcome) {
        apply_warm_outcome(dataset, outcome);
        return false;
    }
    if !outcome_matches_current_request(snapshot, dataset, render, &outcome) {
        return false;
    }
    let logical_request_count = outcome_logical_request_count(&outcome);
    match outcome.status {
        BrickReadStatus::Completed(payload) => {
            dataset.brick_stream_completed += 1;
            completed_current_bricks.push(CompletedCurrentBrick {
                layer_id: outcome.layer_id.clone(),
                payload,
                histogram_sample: outcome.histogram_sample,
            });
        }
        BrickReadStatus::Cancelled => {
            dataset.brick_stream_cancelled += logical_request_count;
        }
        BrickReadStatus::Stale => {
            dataset.brick_stream_stale += logical_request_count;
        }
        BrickReadStatus::Failed(message) => {
            dataset.brick_stream_failed += logical_request_count;
            dataset.brick_stream_last_error = Some(message);
        }
    }
    dataset.brick_stream_complete = dataset.brick_stream_completed
        == dataset.brick_stream_requested
        && dataset.brick_stream_failed == 0
        && dataset.brick_stream_cancelled == 0
        && dataset.brick_stream_stale == 0;
    true
}

fn outcome_logical_request_count(outcome: &BrickReadOutcome) -> usize {
    let _ = outcome;
    1
}

fn apply_completed_current_brick(
    dataset: &mut CurrentDatasetRuntime,
    analysis: &mut CurrentAnalysisRuntime,
    active_layer_id: &LayerId,
    layer_id: LayerId,
    payload: BrickReadPayload,
    histogram_sample: Option<mirante4d_data::BrickHistogramSample>,
) {
    insert_resident_brick_payload(
        dataset,
        analysis,
        active_layer_id,
        layer_id,
        payload,
        histogram_sample,
    );
}

struct CompletedCurrentBrick {
    layer_id: LayerId,
    payload: BrickReadPayload,
    histogram_sample: Option<mirante4d_data::BrickHistogramSample>,
}

fn insert_resident_brick_payloads(
    dataset: &mut CurrentDatasetRuntime,
    analysis: &mut CurrentAnalysisRuntime,
    active_layer_id: &LayerId,
    bricks: Vec<CompletedCurrentBrick>,
) {
    let mut u8_by_layer: BTreeMap<LayerId, Vec<VolumeBrickU8>> = BTreeMap::new();
    let mut u16_by_layer: BTreeMap<LayerId, Vec<VolumeBrickU16>> = BTreeMap::new();
    let mut f32_by_layer: BTreeMap<LayerId, Vec<VolumeBrickF32>> = BTreeMap::new();
    let mut histogram_changed = false;

    for completed in bricks {
        match completed.payload {
            BrickReadPayload::U8(brick) => {
                let brick = *brick;
                update_resident_histogram_sample_for_batch(
                    analysis,
                    resident_histogram_sample_key_for_u8_brick(
                        completed.layer_id.to_string(),
                        &brick,
                    ),
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
                    analysis,
                    resident_histogram_sample_key_for_u16_brick(
                        completed.layer_id.to_string(),
                        &brick,
                    ),
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
                    analysis,
                    resident_histogram_sample_key_for_f32_brick(
                        completed.layer_id.to_string(),
                        &brick,
                    ),
                    completed.histogram_sample,
                    &mut histogram_changed,
                );
                f32_by_layer
                    .entry(completed.layer_id)
                    .or_default()
                    .push(brick);
            }
            BrickReadPayload::Group(_) => {
                dataset.brick_stream_last_error =
                    Some("grouped payload reached resident brick streaming path".to_owned());
            }
        }
    }

    if histogram_changed {
        analysis.resident_histogram_generation =
            analysis.resident_histogram_generation.saturating_add(1);
        analysis.active_histogram_cache = None;
    }
    for (layer_id, new_bricks) in u8_by_layer {
        let bricks = dataset
            .resident_bricks_u8_by_layer
            .entry(layer_id.clone())
            .or_default();
        replace_resident_bricks(bricks, new_bricks);
        if &layer_id == active_layer_id {
            dataset.resident_bricks_u8 = bricks.clone();
        }
    }
    for (layer_id, new_bricks) in u16_by_layer {
        let bricks = dataset
            .resident_bricks_by_layer
            .entry(layer_id.clone())
            .or_default();
        replace_resident_bricks(bricks, new_bricks);
        if &layer_id == active_layer_id {
            dataset.resident_bricks = bricks.clone();
        }
    }
    for (layer_id, new_bricks) in f32_by_layer {
        let bricks = dataset
            .resident_bricks_f32_by_layer
            .entry(layer_id.clone())
            .or_default();
        replace_resident_bricks_f32(bricks, new_bricks);
        if &layer_id == active_layer_id {
            dataset.resident_bricks_f32 = bricks.clone();
        }
    }
}

fn insert_resident_brick_payload(
    dataset: &mut CurrentDatasetRuntime,
    analysis: &mut CurrentAnalysisRuntime,
    active_layer_id: &LayerId,
    layer_id: LayerId,
    payload: BrickReadPayload,
    histogram_sample: Option<mirante4d_data::BrickHistogramSample>,
) {
    match payload {
        BrickReadPayload::U8(brick) => {
            let brick = *brick;
            update_resident_histogram_sample(
                analysis,
                resident_histogram_sample_key_for_u8_brick(layer_id.to_string(), &brick),
                histogram_sample,
            );
            let bricks = dataset
                .resident_bricks_u8_by_layer
                .entry(layer_id.clone())
                .or_default();
            replace_resident_brick(bricks, brick);
            if &layer_id == active_layer_id {
                dataset.resident_bricks_u8 = bricks.clone();
            }
        }
        BrickReadPayload::U16(brick) => {
            let brick = *brick;
            update_resident_histogram_sample(
                analysis,
                resident_histogram_sample_key_for_u16_brick(layer_id.to_string(), &brick),
                histogram_sample,
            );
            let bricks = dataset
                .resident_bricks_by_layer
                .entry(layer_id.clone())
                .or_default();
            replace_resident_brick(bricks, brick);
            if &layer_id == active_layer_id {
                dataset.resident_bricks = bricks.clone();
            }
        }
        BrickReadPayload::F32(brick) => {
            let brick = *brick;
            update_resident_histogram_sample(
                analysis,
                resident_histogram_sample_key_for_f32_brick(layer_id.to_string(), &brick),
                histogram_sample,
            );
            let bricks = dataset
                .resident_bricks_f32_by_layer
                .entry(layer_id.clone())
                .or_default();
            replace_resident_brick_f32(bricks, brick);
            if &layer_id == active_layer_id {
                dataset.resident_bricks_f32 = bricks.clone();
            }
        }
        BrickReadPayload::Group(_) => {
            dataset.brick_stream_last_error =
                Some("grouped payload reached resident brick streaming path".to_owned());
        }
    }
}

fn update_resident_histogram_sample(
    analysis: &mut CurrentAnalysisRuntime,
    key: crate::state::ResidentHistogramSampleKey,
    sample: Option<mirante4d_data::BrickHistogramSample>,
) {
    let mut changed = false;
    update_resident_histogram_sample_for_batch(analysis, key, sample, &mut changed);
    if changed {
        analysis.resident_histogram_generation =
            analysis.resident_histogram_generation.saturating_add(1);
        analysis.active_histogram_cache = None;
    }
}

fn update_resident_histogram_sample_for_batch(
    analysis: &mut CurrentAnalysisRuntime,
    key: crate::state::ResidentHistogramSampleKey,
    sample: Option<mirante4d_data::BrickHistogramSample>,
    changed: &mut bool,
) {
    let sample_changed = match sample {
        Some(sample) => {
            let sample_changed = analysis.resident_histogram_samples.get(&key) != Some(&sample);
            if sample_changed {
                analysis.resident_histogram_samples.insert(key, sample);
            }
            sample_changed
        }
        None => analysis.resident_histogram_samples.remove(&key).is_some(),
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
    dataset: &mut CurrentDatasetRuntime,
    layer_id: &LayerId,
    scale_level: u32,
    timepoint: TimeIndex,
    brick_index: SpatialBrickIndex,
    sample_region: Option<VolumeRegion>,
) -> Option<BrickReadPayload> {
    let position = dataset
        .prefetched_brick_payloads
        .iter()
        .position(|prefetch| {
            &prefetch.layer_id == layer_id
                && prefetch.scale_level == scale_level
                && prefetch.timepoint == timepoint
                && prefetch.brick_index == brick_index
                && prefetch.sample_region == sample_region
        })?;
    Some(
        dataset
            .prefetched_brick_payloads
            .swap_remove(position)
            .payload,
    )
}

fn store_prefetched_brick_payload(
    dataset: &mut CurrentDatasetRuntime,
    layer_id: LayerId,
    scale_level: u32,
    timepoint: TimeIndex,
    brick_index: SpatialBrickIndex,
    sample_region: Option<VolumeRegion>,
    payload: BrickReadPayload,
) {
    if let Some(position) = dataset
        .prefetched_brick_payloads
        .iter()
        .position(|prefetch| {
            prefetch.layer_id == layer_id
                && prefetch.scale_level == scale_level
                && prefetch.timepoint == timepoint
                && prefetch.brick_index == brick_index
                && prefetch.sample_region == sample_region
        })
    {
        dataset.prefetched_brick_payloads.swap_remove(position);
    }
    dataset
        .prefetched_brick_payloads
        .push(PrefetchedBrickPayload {
            layer_id,
            scale_level,
            timepoint,
            brick_index,
            sample_region,
            payload,
        });
    if dataset.prefetched_brick_payloads.len() > PREFETCHED_BRICK_PAYLOAD_LIMIT {
        let overflow = dataset
            .prefetched_brick_payloads
            .len()
            .saturating_sub(PREFETCHED_BRICK_PAYLOAD_LIMIT);
        dataset.prefetched_brick_payloads.drain(0..overflow);
    }
}

fn outcome_matches_current_request(
    snapshot: &ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    outcome: &BrickReadOutcome,
) -> bool {
    let Some(key) = &dataset.brick_stream_request_key else {
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
        && outcome.scale_level == dataset.brick_stream_scale_level
        && key.layer_ids.contains(&outcome.layer_id)
        && outcome.timepoint == view_for_snapshot(snapshot).timepoint()
        && render.visible_bricks.contains(&outcome.brick_index)
        && outcome.sample_region == region_request.worker_sample_region
}

fn outcome_matches_prefetch_request(
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    outcome: &BrickReadOutcome,
) -> bool {
    let Some(key) = &dataset.brick_prefetch_request_key else {
        return false;
    };
    outcome.priority == BrickRequestPriority::Prefetch
        && outcome.scale_level == dataset.brick_stream_scale_level
        && outcome.layer_id == key.layer_id
        && dataset
            .brick_prefetch_timepoints
            .contains(&outcome.timepoint)
        && render.visible_bricks.contains(&outcome.brick_index)
        && outcome.sample_region.is_none()
}

fn outcome_matches_warm_request(
    dataset: &CurrentDatasetRuntime,
    outcome: &BrickReadOutcome,
) -> bool {
    let Some(key) = &dataset.brick_warm_request_key else {
        return false;
    };
    outcome.priority == BrickRequestPriority::Warm
        && outcome.scale_level == key.scale_level
        && outcome.layer_id == key.layer_id
        && outcome.timepoint == key.timepoint
        && key.bricks.contains(&outcome.brick_index)
        && outcome.sample_region.is_none()
}

fn apply_prefetch_outcome(dataset: &mut CurrentDatasetRuntime, outcome: BrickReadOutcome) {
    match outcome.status {
        BrickReadStatus::Completed(payload) => {
            dataset.brick_prefetch_completed += 1;
            store_prefetched_brick_payload(
                dataset,
                outcome.layer_id.clone(),
                outcome.scale_level,
                outcome.timepoint,
                outcome.brick_index,
                outcome.sample_region,
                payload,
            );
        }
        BrickReadStatus::Cancelled => {
            dataset.brick_prefetch_cancelled += 1;
        }
        BrickReadStatus::Stale => {
            dataset.brick_prefetch_stale += 1;
        }
        BrickReadStatus::Failed(message) => {
            dataset.brick_prefetch_failed += 1;
            dataset.brick_prefetch_last_error = Some(message);
        }
    }
}

fn apply_warm_outcome(dataset: &mut CurrentDatasetRuntime, outcome: BrickReadOutcome) {
    match outcome.status {
        BrickReadStatus::Completed(payload) => {
            dataset.brick_warm_completed += 1;
            store_prefetched_brick_payload(
                dataset,
                outcome.layer_id.clone(),
                outcome.scale_level,
                outcome.timepoint,
                outcome.brick_index,
                outcome.sample_region,
                payload,
            );
        }
        BrickReadStatus::Cancelled => {
            dataset.brick_warm_cancelled += 1;
        }
        BrickReadStatus::Stale => {
            dataset.brick_warm_stale += 1;
        }
        BrickReadStatus::Failed(message) => {
            dataset.brick_warm_failed += 1;
            dataset.brick_warm_last_error = Some(message);
        }
    }
}
