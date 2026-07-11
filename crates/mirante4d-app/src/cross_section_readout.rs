use eframe::egui;
use glam::DVec3;
use mirante4d_data::{VolumeBrickF32, VolumeBrickU8, VolumeBrickU16, VolumeRegion};
use mirante4d_domain::{IntensityDType, Shape3D, ViewerLayout};
use mirante4d_format::{CurrentGridToWorldExt, LayerId};
use mirante4d_project_model::ViewState;
use mirante4d_render_api::PresentationViewport;

use crate::{
    cross_section_runtime::{
        CrossSectionChunkPayload, CrossSectionChunkState, CrossSectionPanelRuntime,
    },
    current_runtime::{dataset::CurrentDatasetRuntime, render::CurrentRenderRuntime},
    viewer_layout::{CrossSectionPanelScheduleStatus, PanelId, render_cross_section_view_state},
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct CrossSectionReadoutInput<'a> {
    pub(crate) view: &'a ViewState,
    pub(crate) active_layer_id: &'a LayerId,
    pub(crate) active_layer_dtype: IntensityDType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CrossSectionHoverStatus {
    Value,
    Loading,
    Stale,
    Incomplete,
    Unavailable,
    InvalidNoData,
    Outside,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CrossSectionHoverGenerationStatus {
    CurrentDisplayed,
    CurrentUndisplayed,
    RetainedStale,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum CrossSectionHoverValue {
    U8(u8),
    U16(u16),
    F32(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CrossSectionGridIndex {
    pub(crate) x: u64,
    pub(crate) y: u64,
    pub(crate) z: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CrossSectionHoverReadout {
    pub(crate) text: String,
    pub(crate) panel_id: PanelId,
    pub(crate) layer_id: String,
    pub(crate) timepoint: u64,
    pub(crate) scale_level: Option<u32>,
    pub(crate) target_generation: u64,
    pub(crate) displayed_generation: Option<u64>,
    pub(crate) schedule_generation: Option<u64>,
    pub(crate) display_current: bool,
    pub(crate) generation_status: CrossSectionHoverGenerationStatus,
    pub(crate) world_position: Option<DVec3>,
    pub(crate) grid_position: Option<DVec3>,
    pub(crate) nearest_grid_index: Option<CrossSectionGridIndex>,
    pub(crate) value: Option<CrossSectionHoverValue>,
    pub(crate) status: CrossSectionHoverStatus,
}

pub(crate) fn cross_section_hover_readout_for_response(
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    input: CrossSectionReadoutInput<'_>,
    panel_id: PanelId,
    presentation_viewport: PresentationViewport,
    response: &egui::Response,
) -> Option<CrossSectionHoverReadout> {
    if !response.hovered() || response.rect.width() <= 0.0 || response.rect.height() <= 0.0 {
        return None;
    }
    let position = response.hover_pos()?;
    if !response.rect.contains(position) {
        return None;
    }
    let normalized_x = ((position.x - response.rect.min.x) / response.rect.width()).clamp(0.0, 1.0);
    let normalized_y =
        ((position.y - response.rect.min.y) / response.rect.height()).clamp(0.0, 1.0);
    cross_section_hover_readout_for_panel_point(
        dataset,
        render,
        input,
        panel_id,
        f64::from(normalized_x) * presentation_viewport.width_points(),
        f64::from(normalized_y) * presentation_viewport.height_points(),
        presentation_viewport,
    )
}

pub(crate) fn cross_section_hover_readout_for_panel_point(
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    input: CrossSectionReadoutInput<'_>,
    panel_id: PanelId,
    x_points: f64,
    y_points: f64,
    presentation_viewport: PresentationViewport,
) -> Option<CrossSectionHoverReadout> {
    if input.view.layout() != ViewerLayout::FourPanel {
        return None;
    }
    let panel = panel_id.cross_section_panel()?;
    let panel_runtime = render.cross_section_runtime.panel(panel_id)?;
    let layer_id = input.active_layer_id.clone();
    let timepoint = input.view.timepoint();
    let unavailable_generation = ReadoutGeneration::for_panel(
        panel_runtime,
        None,
        CrossSectionHoverGenerationStatus::Unavailable,
    );

    let Some(schedule) = panel_runtime.cross_section_schedule else {
        return Some(unmapped_readout(
            panel_id,
            layer_id,
            timepoint.get(),
            unavailable_generation,
            CrossSectionHoverStatus::Unavailable,
            "unavailable (no panel schedule)",
        ));
    };
    let unavailable_generation = ReadoutGeneration::for_panel(
        panel_runtime,
        Some(schedule.generation),
        CrossSectionHoverGenerationStatus::Unavailable,
    );
    let Some(scale_level) = schedule.render_scale_level.or(schedule.target_scale_level) else {
        return Some(unmapped_readout(
            panel_id,
            layer_id,
            timepoint.get(),
            unavailable_generation,
            schedule_status_for_missing_value(schedule.status),
            schedule.status_label(),
        ));
    };

    let layer_id_typed = layer_id.clone();
    let grid_to_world = match dataset
        .dataset
        .scale_grid_to_world(&layer_id_typed, scale_level)
    {
        Ok(transform) => transform,
        Err(_) => {
            return Some(unmapped_readout(
                panel_id,
                layer_id,
                timepoint.get(),
                unavailable_generation,
                CrossSectionHoverStatus::Unavailable,
                "unavailable (missing scale transform)",
            ));
        }
    };
    let scale_shape = match dataset.dataset.scale_shape(&layer_id_typed, scale_level) {
        Ok(shape) => shape,
        Err(_) => {
            return Some(unmapped_readout(
                panel_id,
                layer_id,
                timepoint.get(),
                unavailable_generation,
                CrossSectionHoverStatus::Unavailable,
                "unavailable (missing scale shape)",
            ));
        }
    };
    let world_to_grid = match grid_to_world.inverse() {
        Ok(transform) => transform,
        Err(_) => {
            return Some(unmapped_readout(
                panel_id,
                layer_id,
                timepoint.get(),
                unavailable_generation,
                CrossSectionHoverStatus::Unavailable,
                "unavailable (non-invertible transform)",
            ));
        }
    };

    let view = render_cross_section_view_state(*input.view.cross_section()).view(panel);
    let world = view.world_point_for_panel_point(x_points, y_points, presentation_viewport);
    let grid = world_to_grid.transform_point(world);
    let Some(index) = nearest_grid_index(grid, scale_shape) else {
        return Some(mapped_status_readout(
            panel_id,
            layer_id,
            timepoint.get(),
            Some(scale_level),
            unavailable_generation,
            world,
            grid,
            None,
            CrossSectionHoverStatus::Outside,
            "outside",
        ));
    };

    if !input
        .view
        .layer(input.view.active_layer())
        .is_some_and(|layer| layer.visible())
    {
        return Some(mapped_status_readout(
            panel_id,
            layer_id,
            timepoint.get(),
            Some(scale_level),
            unavailable_generation,
            world,
            grid,
            Some(index),
            CrossSectionHoverStatus::Unavailable,
            "unavailable (active layer hidden)",
        ));
    }
    if schedule.generation != panel_runtime.generation {
        let generation = ReadoutGeneration::for_panel(
            panel_runtime,
            Some(schedule.generation),
            CrossSectionHoverGenerationStatus::RetainedStale,
        );
        return Some(mapped_status_readout(
            panel_id,
            layer_id,
            timepoint.get(),
            Some(scale_level),
            generation,
            world,
            grid,
            Some(index),
            CrossSectionHoverStatus::Stale,
            "stale",
        ));
    }
    if !panel_runtime.display_current() {
        let generation_status = if panel_runtime.displayed_generation.is_some() {
            CrossSectionHoverGenerationStatus::RetainedStale
        } else {
            CrossSectionHoverGenerationStatus::CurrentUndisplayed
        };
        let status = if panel_runtime.displayed_generation.is_some() {
            CrossSectionHoverStatus::Stale
        } else {
            schedule_status_for_missing_value(schedule.status)
        };
        let label = if panel_runtime.displayed_generation.is_some() {
            "stale (retained displayed generation)"
        } else {
            missing_resident_label(schedule.status)
        };
        let generation = ReadoutGeneration::for_panel(
            panel_runtime,
            Some(schedule.generation),
            generation_status,
        );
        return Some(mapped_status_readout(
            panel_id,
            layer_id,
            timepoint.get(),
            Some(scale_level),
            generation,
            world,
            grid,
            Some(index),
            status,
            label,
        ));
    }

    let current_displayed_generation = ReadoutGeneration::for_panel(
        panel_runtime,
        Some(schedule.generation),
        CrossSectionHoverGenerationStatus::CurrentDisplayed,
    );

    match sample_resident_value(
        dataset,
        render,
        panel_id,
        input.active_layer_id,
        input.active_layer_dtype,
        timepoint,
        scale_level,
        index,
    ) {
        ResidentSample::Value(value) => Some(mapped_value_readout(
            panel_id,
            layer_id,
            timepoint.get(),
            scale_level,
            current_displayed_generation,
            world,
            grid,
            index,
            value,
        )),
        ResidentSample::Missing => Some(mapped_status_readout(
            panel_id,
            layer_id,
            timepoint.get(),
            Some(scale_level),
            current_displayed_generation,
            world,
            grid,
            Some(index),
            schedule_status_for_missing_value(schedule.status),
            missing_resident_label(schedule.status),
        )),
        ResidentSample::InvalidNoData => Some(mapped_status_readout(
            panel_id,
            layer_id,
            timepoint.get(),
            Some(scale_level),
            current_displayed_generation,
            world,
            grid,
            Some(index),
            CrossSectionHoverStatus::InvalidNoData,
            "invalid/no-data",
        )),
    }
}

enum ResidentSample {
    Value(CrossSectionHoverValue),
    Missing,
    InvalidNoData,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReadoutGeneration {
    target_generation: u64,
    displayed_generation: Option<u64>,
    schedule_generation: Option<u64>,
    display_current: bool,
    status: CrossSectionHoverGenerationStatus,
}

impl ReadoutGeneration {
    fn for_panel(
        panel: &CrossSectionPanelRuntime,
        schedule_generation: Option<u64>,
        status: CrossSectionHoverGenerationStatus,
    ) -> Self {
        Self {
            target_generation: panel.generation,
            displayed_generation: panel.displayed_generation,
            schedule_generation,
            display_current: panel.display_current(),
            status,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn sample_resident_value(
    dataset: &CurrentDatasetRuntime,
    render: &CurrentRenderRuntime,
    panel_id: PanelId,
    layer_id: &LayerId,
    dtype: IntensityDType,
    timepoint: mirante4d_domain::TimeIndex,
    scale_level: u32,
    index: CrossSectionGridIndex,
) -> ResidentSample {
    let Some(panel_runtime) = render.cross_section_runtime.panels.get(&panel_id) else {
        return ResidentSample::Missing;
    };
    for key in &panel_runtime.visible_chunks {
        if &key.dataset_id != dataset.dataset.dataset_id()
            || &key.layer_id != layer_id
            || key.timepoint != timepoint
            || key.scale_level != scale_level
        {
            continue;
        }
        let Some(entry) = render.cross_section_runtime.chunks.get(key) else {
            continue;
        };
        if !matches!(
            entry.state,
            CrossSectionChunkState::CpuResident
                | CrossSectionChunkState::UploadQueued
                | CrossSectionChunkState::GpuResident
        ) {
            continue;
        }
        let sample = match (dtype, entry.payload.as_ref()) {
            (IntensityDType::Uint8, Some(CrossSectionChunkPayload::U8(brick))) => sample_u8_bricks(
                std::slice::from_ref(brick.as_ref()),
                timepoint,
                scale_level,
                index,
            ),
            (IntensityDType::Uint16, Some(CrossSectionChunkPayload::U16(brick))) => {
                sample_u16_bricks(
                    std::slice::from_ref(brick.as_ref()),
                    timepoint,
                    scale_level,
                    index,
                )
            }
            (IntensityDType::Float32, Some(CrossSectionChunkPayload::F32(brick))) => {
                sample_f32_bricks(
                    std::slice::from_ref(brick.as_ref()),
                    timepoint,
                    scale_level,
                    index,
                )
            }
            _ => ResidentSample::Missing,
        };
        if !matches!(sample, ResidentSample::Missing) {
            return sample;
        }
    }
    ResidentSample::Missing
}

fn sample_u8_bricks(
    bricks: &[VolumeBrickU8],
    timepoint: mirante4d_domain::TimeIndex,
    scale_level: u32,
    index: CrossSectionGridIndex,
) -> ResidentSample {
    let Some(brick) = bricks.iter().find(|brick| {
        brick.scale_level == scale_level
            && brick.volume.timepoint == timepoint
            && region_contains(brick.region, index)
    }) else {
        return ResidentSample::Missing;
    };
    if !brick.occupied || brick.valid_voxel_count == 0 {
        return ResidentSample::InvalidNoData;
    }
    let (z, y, x) = local_zyx(brick.region, index);
    brick
        .render_voxel(z, y, x)
        .map(|value| ResidentSample::Value(CrossSectionHoverValue::U8(value)))
        .unwrap_or(ResidentSample::InvalidNoData)
}

fn sample_u16_bricks(
    bricks: &[VolumeBrickU16],
    timepoint: mirante4d_domain::TimeIndex,
    scale_level: u32,
    index: CrossSectionGridIndex,
) -> ResidentSample {
    let Some(brick) = bricks.iter().find(|brick| {
        brick.scale_level == scale_level
            && brick.volume.timepoint == timepoint
            && region_contains(brick.region, index)
    }) else {
        return ResidentSample::Missing;
    };
    if !brick.occupied || brick.valid_voxel_count == 0 {
        return ResidentSample::InvalidNoData;
    }
    let (z, y, x) = local_zyx(brick.region, index);
    brick
        .render_voxel(z, y, x)
        .map(|value| ResidentSample::Value(CrossSectionHoverValue::U16(value)))
        .unwrap_or(ResidentSample::InvalidNoData)
}

fn sample_f32_bricks(
    bricks: &[VolumeBrickF32],
    timepoint: mirante4d_domain::TimeIndex,
    scale_level: u32,
    index: CrossSectionGridIndex,
) -> ResidentSample {
    let Some(brick) = bricks.iter().find(|brick| {
        brick.scale_level == scale_level
            && brick.volume.timepoint == timepoint
            && region_contains(brick.region, index)
    }) else {
        return ResidentSample::Missing;
    };
    if !brick.occupied || brick.valid_voxel_count == 0 {
        return ResidentSample::InvalidNoData;
    }
    let (z, y, x) = local_zyx(brick.region, index);
    brick
        .render_voxel(z, y, x)
        .map(|value| ResidentSample::Value(CrossSectionHoverValue::F32(value)))
        .unwrap_or(ResidentSample::InvalidNoData)
}

fn nearest_grid_index(grid: DVec3, shape: Shape3D) -> Option<CrossSectionGridIndex> {
    if !grid.is_finite() {
        return None;
    }
    let x = nearest_i64(grid.x)?;
    let y = nearest_i64(grid.y)?;
    let z = nearest_i64(grid.z)?;
    if x < 0 || y < 0 || z < 0 {
        return None;
    }
    let x = u64::try_from(x).ok()?;
    let y = u64::try_from(y).ok()?;
    let z = u64::try_from(z).ok()?;
    if x >= shape.x() || y >= shape.y() || z >= shape.z() {
        return None;
    }
    Some(CrossSectionGridIndex { x, y, z })
}

fn nearest_i64(value: f64) -> Option<i64> {
    let nearest = (value + 0.5).floor();
    if nearest < i64::MIN as f64 || nearest > i64::MAX as f64 {
        None
    } else {
        Some(nearest as i64)
    }
}

fn region_contains(region: VolumeRegion, index: CrossSectionGridIndex) -> bool {
    index.z >= region.z_start
        && index.y >= region.y_start
        && index.x >= region.x_start
        && index.z < region.z_start.saturating_add(region.z_size)
        && index.y < region.y_start.saturating_add(region.y_size)
        && index.x < region.x_start.saturating_add(region.x_size)
}

fn local_zyx(region: VolumeRegion, index: CrossSectionGridIndex) -> (u64, u64, u64) {
    (
        index.z - region.z_start,
        index.y - region.y_start,
        index.x - region.x_start,
    )
}

fn schedule_status_for_missing_value(
    status: CrossSectionPanelScheduleStatus,
) -> CrossSectionHoverStatus {
    match status {
        CrossSectionPanelScheduleStatus::Loading => CrossSectionHoverStatus::Loading,
        CrossSectionPanelScheduleStatus::Incomplete => CrossSectionHoverStatus::Incomplete,
        CrossSectionPanelScheduleStatus::Coarse
        | CrossSectionPanelScheduleStatus::Current
        | CrossSectionPanelScheduleStatus::Ready => CrossSectionHoverStatus::Loading,
        CrossSectionPanelScheduleStatus::MissingViewport
        | CrossSectionPanelScheduleStatus::BudgetLimited
        | CrossSectionPanelScheduleStatus::Unavailable => CrossSectionHoverStatus::Unavailable,
    }
}

fn missing_resident_label(status: CrossSectionPanelScheduleStatus) -> &'static str {
    match status {
        CrossSectionPanelScheduleStatus::Loading => "loading (resident brick unavailable)",
        CrossSectionPanelScheduleStatus::Incomplete => "incomplete (resident brick unavailable)",
        CrossSectionPanelScheduleStatus::BudgetLimited => {
            "budget limited (resident brick unavailable)"
        }
        CrossSectionPanelScheduleStatus::MissingViewport => "unavailable (missing panel viewport)",
        CrossSectionPanelScheduleStatus::Unavailable => "unavailable",
        CrossSectionPanelScheduleStatus::Ready
        | CrossSectionPanelScheduleStatus::Current
        | CrossSectionPanelScheduleStatus::Coarse => "loading (resident brick unavailable)",
    }
}

fn unmapped_readout(
    panel_id: PanelId,
    layer_id: LayerId,
    timepoint: u64,
    generation: ReadoutGeneration,
    status: CrossSectionHoverStatus,
    label: &str,
) -> CrossSectionHoverReadout {
    CrossSectionHoverReadout {
        text: format!(
            "{} {} t{}: {}",
            panel_id.label(),
            layer_id,
            timepoint,
            label
        ),
        panel_id,
        layer_id: layer_id.to_string(),
        timepoint,
        scale_level: None,
        target_generation: generation.target_generation,
        displayed_generation: generation.displayed_generation,
        schedule_generation: generation.schedule_generation,
        display_current: generation.display_current,
        generation_status: generation.status,
        world_position: None,
        grid_position: None,
        nearest_grid_index: None,
        value: None,
        status,
    }
}

#[allow(clippy::too_many_arguments)]
fn mapped_value_readout(
    panel_id: PanelId,
    layer_id: LayerId,
    timepoint: u64,
    scale_level: u32,
    generation: ReadoutGeneration,
    world_position: DVec3,
    grid_position: DVec3,
    index: CrossSectionGridIndex,
    value: CrossSectionHoverValue,
) -> CrossSectionHoverReadout {
    let text = format!(
        "{} {} t{} s{} nearest world={} grid={} xyz={} value={}",
        panel_id.label(),
        layer_id,
        timepoint,
        scale_level,
        format_vec3(world_position),
        format_vec3(grid_position),
        format_index(index),
        format_value(value)
    );
    CrossSectionHoverReadout {
        text,
        panel_id,
        layer_id: layer_id.to_string(),
        timepoint,
        scale_level: Some(scale_level),
        target_generation: generation.target_generation,
        displayed_generation: generation.displayed_generation,
        schedule_generation: generation.schedule_generation,
        display_current: generation.display_current,
        generation_status: generation.status,
        world_position: Some(world_position),
        grid_position: Some(grid_position),
        nearest_grid_index: Some(index),
        value: Some(value),
        status: CrossSectionHoverStatus::Value,
    }
}

#[allow(clippy::too_many_arguments)]
fn mapped_status_readout(
    panel_id: PanelId,
    layer_id: LayerId,
    timepoint: u64,
    scale_level: Option<u32>,
    generation: ReadoutGeneration,
    world_position: DVec3,
    grid_position: DVec3,
    index: Option<CrossSectionGridIndex>,
    status: CrossSectionHoverStatus,
    label: &str,
) -> CrossSectionHoverReadout {
    let scale = scale_level
        .map(|scale| format!(" s{scale}"))
        .unwrap_or_default();
    let nearest = index
        .map(|index| format!(" xyz={}", format_index(index)))
        .unwrap_or_default();
    let text = format!(
        "{} {} t{}{} nearest world={} grid={}{} {}",
        panel_id.label(),
        layer_id,
        timepoint,
        scale,
        format_vec3(world_position),
        format_vec3(grid_position),
        nearest,
        label
    );
    CrossSectionHoverReadout {
        text,
        panel_id,
        layer_id: layer_id.to_string(),
        timepoint,
        scale_level,
        target_generation: generation.target_generation,
        displayed_generation: generation.displayed_generation,
        schedule_generation: generation.schedule_generation,
        display_current: generation.display_current,
        generation_status: generation.status,
        world_position: Some(world_position),
        grid_position: Some(grid_position),
        nearest_grid_index: index,
        value: None,
        status,
    }
}

fn format_vec3(value: DVec3) -> String {
    format!("({:.3}, {:.3}, {:.3})", value.x, value.y, value.z)
}

fn format_index(index: CrossSectionGridIndex) -> String {
    format!("({}, {}, {})", index.x, index.y, index.z)
}

fn format_value(value: CrossSectionHoverValue) -> String {
    match value {
        CrossSectionHoverValue::U8(value) => value.to_string(),
        CrossSectionHoverValue::U16(value) => value.to_string(),
        CrossSectionHoverValue::F32(value) => format!("{value:.6}"),
    }
}
