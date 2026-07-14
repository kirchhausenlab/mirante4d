use eframe::egui;
use glam::{DMat4, DVec3};
use mirante4d_application::{
    CrossSectionPanelScheduleStatus, RenderCoordinationState, RenderSurfaceState,
};
use mirante4d_dataset::DatasetCatalog;
use mirante4d_domain::{GridToWorld, LogicalLayerKey, ScaleLevel, Shape3D, ViewerLayout};
use mirante4d_project_model::ViewState;
use mirante4d_render_api::PresentationViewport;

use crate::{
    retained_leases::{RetainedLeaseSample, RetainedLeases},
    viewer_layout::{
        PanelId, cross_section_schedule_status_label, render_cross_section_view_state,
    },
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct CrossSectionReadoutInput<'a> {
    pub(crate) view: &'a ViewState,
    pub(crate) catalog: &'a DatasetCatalog,
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
    coordination: &RenderCoordinationState,
    leases: &RetainedLeases,
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
        coordination,
        leases,
        input,
        panel_id,
        f64::from(normalized_x) * presentation_viewport.width_points(),
        f64::from(normalized_y) * presentation_viewport.height_points(),
        presentation_viewport,
    )
}

pub(crate) fn cross_section_hover_readout_for_panel_point(
    coordination: &RenderCoordinationState,
    leases: &RetainedLeases,
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
    let panel_runtime = coordination.surface(panel_id.presentation_slot());
    let layer_key = input.view.active_layer();
    let layer_id = logical_layer_id(layer_key);
    let timepoint = input.view.timepoint();
    let unavailable_generation = ReadoutGeneration::for_panel(
        panel_runtime,
        None,
        CrossSectionHoverGenerationStatus::Unavailable,
    );

    let Some(schedule) = panel_runtime.cross_section_schedule() else {
        return Some(unmapped_readout(
            panel_id,
            &layer_id,
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
            &layer_id,
            timepoint.get(),
            unavailable_generation,
            schedule_status_for_missing_value(schedule.status),
            cross_section_schedule_status_label(schedule),
        ));
    };

    let Some(layer) = input.catalog.layer(layer_key) else {
        return Some(unmapped_readout(
            panel_id,
            &layer_id,
            timepoint.get(),
            unavailable_generation,
            CrossSectionHoverStatus::Unavailable,
            "unavailable (missing logical layer)",
        ));
    };
    let Some(scale) = layer.scale(ScaleLevel::new(scale_level)) else {
        return Some(unmapped_readout(
            panel_id,
            &layer_id,
            timepoint.get(),
            unavailable_generation,
            CrossSectionHoverStatus::Unavailable,
            "unavailable (missing scale metadata)",
        ));
    };
    let grid_to_world = scale.grid_to_world();
    let scale_shape = scale.shape();
    let world_to_grid = match inverse_grid_to_world(grid_to_world) {
        Some(transform) => transform,
        None => {
            return Some(unmapped_readout(
                panel_id,
                &layer_id,
                timepoint.get(),
                unavailable_generation,
                CrossSectionHoverStatus::Unavailable,
                "unavailable (non-invertible transform)",
            ));
        }
    };

    let view = render_cross_section_view_state(*input.view.cross_section()).view(panel);
    let world = view.world_point_for_panel_point(x_points, y_points, presentation_viewport);
    let grid = world_to_grid.transform_point3(world);
    let Some(index) = nearest_grid_index(grid, scale_shape) else {
        return Some(mapped_status_readout(
            panel_id,
            &layer_id,
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
            &layer_id,
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
    if schedule.generation != panel_runtime.generation() {
        let generation = ReadoutGeneration::for_panel(
            panel_runtime,
            Some(schedule.generation),
            CrossSectionHoverGenerationStatus::RetainedStale,
        );
        return Some(mapped_status_readout(
            panel_id,
            &layer_id,
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
        let generation_status = if panel_runtime.displayed_generation().is_some() {
            CrossSectionHoverGenerationStatus::RetainedStale
        } else {
            CrossSectionHoverGenerationStatus::CurrentUndisplayed
        };
        let status = if panel_runtime.displayed_generation().is_some() {
            CrossSectionHoverStatus::Stale
        } else {
            schedule_status_for_missing_value(schedule.status)
        };
        let label = if panel_runtime.displayed_generation().is_some() {
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
            &layer_id,
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
        leases,
        input.catalog,
        layer_key,
        timepoint,
        scale_level,
        index,
    ) {
        ResidentSample::Value(value) => Some(mapped_value_readout(
            panel_id,
            &layer_id,
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
            &layer_id,
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
            &layer_id,
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

#[derive(Debug, Clone, Copy, PartialEq)]
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
        panel: &RenderSurfaceState,
        schedule_generation: Option<u64>,
        status: CrossSectionHoverGenerationStatus,
    ) -> Self {
        Self {
            target_generation: panel.generation(),
            displayed_generation: panel.displayed_generation(),
            schedule_generation,
            display_current: panel.display_current(),
            status,
        }
    }
}

fn sample_resident_value(
    leases: &RetainedLeases,
    catalog: &DatasetCatalog,
    layer: LogicalLayerKey,
    timepoint: mirante4d_domain::TimeIndex,
    scale_level: u32,
    index: CrossSectionGridIndex,
) -> ResidentSample {
    let resident = leases.resident_set(
        catalog.scientific_identity().resource_identity(),
        layer,
        timepoint,
        ScaleLevel::new(scale_level),
    );
    match resident.sample([index.z, index.y, index.x]) {
        RetainedLeaseSample::Uint8(value) => {
            ResidentSample::Value(CrossSectionHoverValue::U8(value))
        }
        RetainedLeaseSample::Uint16(value) => {
            ResidentSample::Value(CrossSectionHoverValue::U16(value))
        }
        RetainedLeaseSample::Float32(value) => {
            ResidentSample::Value(CrossSectionHoverValue::F32(value))
        }
        RetainedLeaseSample::InvalidNoData => ResidentSample::InvalidNoData,
        RetainedLeaseSample::Missing => ResidentSample::Missing,
    }
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

fn inverse_grid_to_world(transform: GridToWorld) -> Option<DMat4> {
    let row_major = transform.row_major();
    let mut column_major = [0.0; 16];
    for row in 0..4 {
        for column in 0..4 {
            column_major[column * 4 + row] = row_major[row * 4 + column];
        }
    }
    let matrix = DMat4::from_cols_array(&column_major);
    let inverse = matrix.inverse();
    (inverse.is_finite() && (matrix * inverse).abs_diff_eq(DMat4::IDENTITY, 1.0e-9))
        .then_some(inverse)
}

fn logical_layer_id(layer: LogicalLayerKey) -> String {
    format!("layer-{}", layer.ordinal())
}

fn schedule_status_for_missing_value(
    status: CrossSectionPanelScheduleStatus,
) -> CrossSectionHoverStatus {
    match status {
        CrossSectionPanelScheduleStatus::Loading => CrossSectionHoverStatus::Loading,
        CrossSectionPanelScheduleStatus::Empty => CrossSectionHoverStatus::Outside,
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
        CrossSectionPanelScheduleStatus::Loading => "loading (resident data unavailable)",
        CrossSectionPanelScheduleStatus::Empty => "outside selected data",
        CrossSectionPanelScheduleStatus::Incomplete => "incomplete (resident data unavailable)",
        CrossSectionPanelScheduleStatus::BudgetLimited => {
            "budget limited (resident data unavailable)"
        }
        CrossSectionPanelScheduleStatus::MissingViewport => "unavailable (missing panel viewport)",
        CrossSectionPanelScheduleStatus::Unavailable => "unavailable",
        CrossSectionPanelScheduleStatus::Ready
        | CrossSectionPanelScheduleStatus::Current
        | CrossSectionPanelScheduleStatus::Coarse => "loading (resident data unavailable)",
    }
}

fn unmapped_readout(
    panel_id: PanelId,
    layer_id: &str,
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
        layer_id: layer_id.to_owned(),
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
    layer_id: &str,
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
        layer_id: layer_id.to_owned(),
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
    layer_id: &str,
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
        layer_id: layer_id.to_owned(),
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mirante4d_dataset::{
        DatasetLayer, DatasetResourceKey, DatasetSourceId, ResourceLease,
        ResourcePayloadDescriptor, ResourcePayloadView, ResourceRegion, ResourceValidity,
        ScientificIdentityStatus,
    };
    use mirante4d_domain::{IntensityDType, Shape4D, TimeIndex};

    use super::*;

    #[derive(Debug)]
    struct FixtureLease {
        key: DatasetResourceKey,
        descriptor: ResourcePayloadDescriptor,
        values: Box<[u8]>,
        validity: Box<[u8]>,
    }

    impl ResourceLease for FixtureLease {
        fn key(&self) -> DatasetResourceKey {
            self.key
        }

        fn payload(&self) -> ResourcePayloadView<'_> {
            self.descriptor
                .view(&self.values, Some(&self.validity))
                .expect("fixture payload remains valid")
        }
    }

    fn fixture() -> (DatasetCatalog, RetainedLeases) {
        let source_id = DatasetSourceId::new(7);
        let layer_key = LogicalLayerKey::new(0);
        let layer = DatasetLayer::new(
            layer_key,
            "channel",
            Shape4D::new(2, 1, 1, 2).unwrap(),
            IntensityDType::Uint16,
            GridToWorld::identity(),
            ResourceValidity::BitMask,
        )
        .unwrap();
        let catalog = DatasetCatalog::new(
            "dataset",
            ScientificIdentityStatus::Unverified(source_id),
            vec![layer],
        )
        .unwrap();
        let key = DatasetResourceKey::new(
            catalog.scientific_identity().resource_identity(),
            layer_key,
            TimeIndex::new(0),
            ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 0], Shape3D::new(1, 1, 2).unwrap()).unwrap(),
        );
        let descriptor = catalog.resource_payload_descriptor(key).unwrap();
        let values = [0_u16, 41]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let validity = vec![0b0000_0001].into_boxed_slice();
        let lease = Arc::new(FixtureLease {
            key,
            descriptor,
            values,
            validity,
        });
        let mut bridge = RetainedLeases::new();
        bridge.replace_requirements([key]).unwrap();
        bridge.install(lease).unwrap();
        (catalog, bridge)
    }

    #[test]
    fn resident_sampling_distinguishes_valid_zero_from_invalid_no_data() {
        let (catalog, bridge) = fixture();

        assert_eq!(
            sample_resident_value(
                &bridge,
                &catalog,
                LogicalLayerKey::new(0),
                TimeIndex::new(0),
                0,
                CrossSectionGridIndex { x: 0, y: 0, z: 0 },
            ),
            ResidentSample::Value(CrossSectionHoverValue::U16(0))
        );
        assert_eq!(
            sample_resident_value(
                &bridge,
                &catalog,
                LogicalLayerKey::new(0),
                TimeIndex::new(0),
                0,
                CrossSectionGridIndex { x: 1, y: 0, z: 0 },
            ),
            ResidentSample::InvalidNoData
        );
    }

    #[test]
    fn resident_sampling_requires_the_exact_semantic_lease() {
        let (catalog, bridge) = fixture();

        assert_eq!(
            sample_resident_value(
                &bridge,
                &catalog,
                LogicalLayerKey::new(0),
                TimeIndex::new(1),
                0,
                CrossSectionGridIndex { x: 0, y: 0, z: 0 },
            ),
            ResidentSample::Missing
        );
    }
}
