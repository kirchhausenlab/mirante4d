use std::collections::BTreeMap;

use mirante4d_core::{IntensityDType, LayerId, TimeIndex};
use mirante4d_data::{BrickReadPayload, SpatialBrickIndex, VolumeRegion};

use crate::{AppLayerSummary, AppState};

use super::{BrickStreamRequestKey, stream_layer_ids_for_state};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BrickSourceRegion {
    pub(super) brick_index: SpatialBrickIndex,
    pub(super) resident_region: VolumeRegion,
    pub(super) worker_sample_region: Option<VolumeRegion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrickPrefetchRequestKey {
    pub(super) layer_id: String,
    pub(super) scale_level: u32,
    pub(super) active_timepoint: TimeIndex,
    pub(super) timepoints: Vec<TimeIndex>,
    pub(super) visible_bricks: Vec<SpatialBrickIndex>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrickWarmRequestKey {
    pub(super) layer_id: String,
    pub(super) scale_level: u32,
    pub(super) timepoint: TimeIndex,
    pub(super) bricks: Vec<SpatialBrickIndex>,
}

#[derive(Debug)]
pub(crate) struct PrefetchedBrickPayload {
    pub(super) layer_id: String,
    pub(super) scale_level: u32,
    pub(super) timepoint: TimeIndex,
    pub(super) brick_index: SpatialBrickIndex,
    pub(super) sample_region: Option<VolumeRegion>,
    pub(super) payload: BrickReadPayload,
}

impl Clone for PrefetchedBrickPayload {
    fn clone(&self) -> Self {
        Self {
            layer_id: self.layer_id.clone(),
            scale_level: self.scale_level,
            timepoint: self.timepoint,
            brick_index: self.brick_index,
            sample_region: self.sample_region,
            payload: clone_brick_read_payload(&self.payload),
        }
    }
}

fn clone_brick_read_payload(payload: &BrickReadPayload) -> BrickReadPayload {
    match payload {
        BrickReadPayload::U8(brick) => BrickReadPayload::U8(Box::new((**brick).clone())),
        BrickReadPayload::U16(brick) => BrickReadPayload::U16(Box::new((**brick).clone())),
        BrickReadPayload::F32(brick) => BrickReadPayload::F32(Box::new((**brick).clone())),
        BrickReadPayload::Group(payloads) => {
            BrickReadPayload::Group(payloads.iter().map(clone_brick_read_payload).collect())
        }
    }
}

fn resident_layer_available(state: &AppState, layer: &AppLayerSummary) -> bool {
    match layer.dtype {
        IntensityDType::Float32 => {
            state
                .resident_bricks_f32_by_layer
                .get(&layer.id)
                .is_some_and(|bricks| !bricks.is_empty())
                || (layer.id == state.active_layer_id && !state.resident_bricks_f32.is_empty())
        }
        IntensityDType::Uint8 => {
            state
                .resident_bricks_u8_by_layer
                .get(&layer.id)
                .is_some_and(|bricks| !bricks.is_empty())
                || (layer.id == state.active_layer_id && !state.resident_bricks_u8.is_empty())
        }
        IntensityDType::Uint16 => {
            state
                .resident_bricks_by_layer
                .get(&layer.id)
                .is_some_and(|bricks| !bricks.is_empty())
                || (layer.id == state.active_layer_id && !state.resident_bricks.is_empty())
        }
    }
}

pub(super) fn visible_resident_layers_ready(state: &AppState) -> bool {
    let Ok(layer_ids) = stream_layer_ids_for_state(state) else {
        return false;
    };
    resident_layer_ids_ready(state, &layer_ids)
}

fn resident_layer_ids_ready(state: &AppState, layer_ids: &[LayerId]) -> bool {
    layer_ids.iter().all(|layer_id| {
        state
            .layers
            .iter()
            .find(|layer| layer.id == layer_id.as_str())
            .is_some_and(|layer| resident_layer_available(state, layer))
    })
}

pub(crate) fn current_brick_stream_request_key(
    state: &AppState,
) -> anyhow::Result<BrickStreamRequestKey> {
    let stream_layer_ids = stream_layer_ids_for_state(state)?;
    let source_regions = current_brick_source_regions_for_state(state, stream_layer_ids.first())?;
    let layer_ids = stream_layer_ids
        .into_iter()
        .map(|layer_id| layer_id.to_string())
        .collect::<Vec<_>>();
    Ok(BrickStreamRequestKey {
        layer_ids,
        scale_level: state.brick_stream_scale_level,
        timepoint: state.active_timepoint,
        visible_bricks: state.visible_bricks.clone(),
        source_regions,
    })
}

fn current_brick_source_regions_for_state(
    state: &AppState,
    layer_id: Option<&LayerId>,
) -> anyhow::Result<Vec<BrickSourceRegion>> {
    let Some(layer_id) = layer_id else {
        return Ok(Vec::new());
    };
    let mut regions = Vec::with_capacity(state.visible_bricks.len());
    for brick in &state.visible_bricks {
        let metadata = state.dataset.brick_metadata_at_scale(
            layer_id,
            state.brick_stream_scale_level,
            state.active_timepoint,
            *brick,
        )?;
        regions.push(BrickSourceRegion {
            brick_index: *brick,
            resident_region: metadata.region,
            worker_sample_region: None,
        });
    }
    regions.sort_by_key(|entry| {
        (
            entry.brick_index.z,
            entry.brick_index.y,
            entry.brick_index.x,
        )
    });
    Ok(regions)
}

pub(super) fn request_region_map(
    key: &BrickStreamRequestKey,
) -> BTreeMap<SpatialBrickIndex, BrickSourceRegion> {
    key.source_regions
        .iter()
        .copied()
        .map(|entry| (entry.brick_index, entry))
        .collect()
}
