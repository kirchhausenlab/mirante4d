use std::collections::BTreeMap;

use mirante4d_data::{BrickReadPayload, SpatialBrickIndex, VolumeRegion};
use mirante4d_domain::{IntensityDType, TimeIndex};
use mirante4d_format::LayerId;

use crate::current_runtime::{dataset::CurrentDatasetRuntime, render::CurrentRenderRuntime};

use super::{BrickStreamRequestKey, stream_layer_ids_for_snapshot, view_for_snapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BrickSourceRegion {
    pub(super) brick_index: SpatialBrickIndex,
    pub(super) resident_region: VolumeRegion,
    pub(super) worker_sample_region: Option<VolumeRegion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrickPrefetchRequestKey {
    pub(super) layer_id: LayerId,
    pub(super) scale_level: u32,
    pub(super) active_timepoint: TimeIndex,
    pub(super) timepoints: Vec<TimeIndex>,
    pub(super) visible_bricks: Vec<SpatialBrickIndex>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrickWarmRequestKey {
    pub(super) layer_id: LayerId,
    pub(super) scale_level: u32,
    pub(super) timepoint: TimeIndex,
    pub(super) bricks: Vec<SpatialBrickIndex>,
}

#[derive(Debug)]
pub(crate) struct PrefetchedBrickPayload {
    pub(super) layer_id: LayerId,
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

fn resident_layer_available(
    dataset: &CurrentDatasetRuntime,
    layer_id: &LayerId,
    dtype: IntensityDType,
) -> bool {
    match dtype {
        IntensityDType::Float32 => dataset
            .resident_bricks_f32_by_layer
            .get(layer_id)
            .is_some_and(|bricks| !bricks.is_empty()),
        IntensityDType::Uint8 => dataset
            .resident_bricks_u8_by_layer
            .get(layer_id)
            .is_some_and(|bricks| !bricks.is_empty()),
        IntensityDType::Uint16 => dataset
            .resident_bricks_by_layer
            .get(layer_id)
            .is_some_and(|bricks| !bricks.is_empty()),
    }
}

pub(super) fn visible_resident_layers_ready(
    snapshot: &mirante4d_application::ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
) -> bool {
    let Ok(layer_ids) = stream_layer_ids_for_snapshot(snapshot, dataset) else {
        return false;
    };
    let view = view_for_snapshot(snapshot);
    layer_ids.iter().all(|layer_id| {
        dataset
            .dataset
            .manifest()
            .layers
            .iter()
            .position(|layer| layer.id == layer_id.as_str())
            .and_then(|ordinal| {
                let key = mirante4d_domain::LogicalLayerKey::new(u32::try_from(ordinal).ok()?);
                view.layer(key)
                    .filter(|layer| layer.visible())
                    .and_then(|_| snapshot.catalog().layer(key))
            })
            .is_some_and(|layer| resident_layer_available(dataset, layer_id, layer.dtype()))
    })
}

pub(crate) fn current_brick_stream_request_key(
    snapshot: &mirante4d_application::ApplicationSnapshot,
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
) -> anyhow::Result<BrickStreamRequestKey> {
    let stream_layer_ids = stream_layer_ids_for_snapshot(snapshot, dataset)?;
    let timepoint = view_for_snapshot(snapshot).timepoint();
    let source_regions =
        current_brick_source_regions(dataset, render, timepoint, stream_layer_ids.first())?;
    Ok(BrickStreamRequestKey {
        layer_ids: stream_layer_ids,
        scale_level: dataset.brick_stream_scale_level,
        timepoint,
        visible_bricks: render.visible_bricks.clone(),
        source_regions,
    })
}

fn current_brick_source_regions(
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    timepoint: TimeIndex,
    layer_id: Option<&LayerId>,
) -> anyhow::Result<Vec<BrickSourceRegion>> {
    let Some(layer_id) = layer_id else {
        return Ok(Vec::new());
    };
    let mut regions = Vec::with_capacity(render.visible_bricks.len());
    for brick in &render.visible_bricks {
        let metadata = dataset.dataset.brick_metadata_at_scale(
            layer_id,
            dataset.brick_stream_scale_level,
            timepoint,
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
