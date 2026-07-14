//! Pure semantic demand planning for the unified runtime.

use std::{collections::BTreeMap, error::Error, fmt};

use mirante4d_dataset::{DatasetCatalog, DatasetResourceKey, ResourceRegion};
use mirante4d_domain::{LogicalLayerKey, RenderMode, ScaleLevel, Shape3D};
use mirante4d_project_model::ViewState;
use mirante4d_render_api::{CameraFrame, PresentationViewport, RenderExtent};

use crate::{
    semantic_demand::{
        CrossSectionPlane, SemanticPlanError, SemanticPlanLimits, SemanticRegionGridSpec,
        VolumePlanOptions, plan_cross_section_resource_regions, plan_visible_resource_regions,
    },
    semantic_tiles::SEMANTIC_TILE_SIDE,
    viewer_layout::PanelId,
};

#[derive(Debug, Clone)]
pub(crate) struct DatasetDemandPlan {
    /// The active layer's selected scale. Every layer's actual scale is in
    /// `layer_scales` and in its semantic resource keys.
    pub(crate) scale: ScaleLevel,
    pub(crate) layer_scales: BTreeMap<LogicalLayerKey, ScaleLevel>,
    pub(crate) resources: Vec<DatasetResourceKey>,
    pub(crate) decoded_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DatasetDemandPlanLimits {
    pub(crate) max_candidates_per_layer: usize,
    pub(crate) max_resources: usize,
    pub(crate) max_decoded_bytes: u64,
}

impl DatasetDemandPlanLimits {
    pub(crate) const fn new(
        max_candidates_per_layer: usize,
        max_resources: usize,
        max_decoded_bytes: u64,
    ) -> Self {
        Self {
            max_candidates_per_layer,
            max_resources,
            max_decoded_bytes,
        }
    }

    const fn reserve_playback_half(self, playback_active: bool) -> Self {
        if playback_active {
            Self {
                max_candidates_per_layer: self.max_candidates_per_layer,
                max_resources: self.max_resources / 2,
                max_decoded_bytes: self.max_decoded_bytes / 2,
            }
        } else {
            self
        }
    }
}

pub(crate) fn render_extent_from_dimensions(
    width: u64,
    height: u64,
) -> anyhow::Result<RenderExtent> {
    let width = u32::try_from(width)
        .map_err(|_| anyhow::anyhow!("render width {width} exceeds u32 limits"))?;
    let height = u32::try_from(height)
        .map_err(|_| anyhow::anyhow!("render height {height} exceeds u32 limits"))?;
    RenderExtent::new(width, height).map_err(Into::into)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DatasetDemandPlanCapacityError {
    limits: DatasetDemandPlanLimits,
}

impl DatasetDemandPlanCapacityError {
    const fn new(limits: DatasetDemandPlanLimits) -> Self {
        Self { limits }
    }
}

impl fmt::Display for DatasetDemandPlanCapacityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "dataset demand cannot fit within {} resources, {} decoded bytes, and {} candidates per visible layer",
            self.limits.max_resources,
            self.limits.max_decoded_bytes,
            self.limits.max_candidates_per_layer,
        )
    }
}

impl Error for DatasetDemandPlanCapacityError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DatasetDemandPlanCompatibilityError;

impl fmt::Display for DatasetDemandPlanCompatibilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "multi-channel DVR requires selected layers to share one grid shape and transform",
        )
    }
}

impl Error for DatasetDemandPlanCompatibilityError {}

enum PlanAttemptError {
    Capacity,
    Other(anyhow::Error),
}

type PlanAttemptResult<T> = Result<T, PlanAttemptError>;

#[derive(Default)]
struct PlanAccumulator {
    resources: Vec<DatasetResourceKey>,
    layer_scales: BTreeMap<LogicalLayerKey, ScaleLevel>,
    decoded_bytes: u64,
}

pub(crate) fn plan_current_3d(
    catalog: &DatasetCatalog,
    view: &ViewState,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    limits: DatasetDemandPlanLimits,
    playback_active: bool,
) -> anyhow::Result<DatasetDemandPlan> {
    let active = catalog
        .layer(view.active_layer())
        .ok_or_else(|| anyhow::anyhow!("active layer is absent from the dataset catalog"))?;
    let camera = CameraFrame::new(*view.camera(), presentation)?;
    let world_per_point = camera.world_per_screen_point_at_target()?.max(f64::EPSILON);
    let mut scales = active
        .scales()
        .map(|scale| {
            (
                representative_voxel_world_size(scale.grid_to_world()),
                scale.level(),
            )
        })
        .collect::<Vec<_>>();
    scales.sort_by(|left, right| left.0.total_cmp(&right.0).then(left.1.cmp(&right.1)));
    let mut target_index = scales
        .iter()
        .rposition(|(resolution, _)| *resolution <= world_per_point)
        .unwrap_or(0);
    if playback_active && target_index + 1 < scales.len() {
        target_index += 1;
    }

    let effective_limits = limits.reserve_playback_half(playback_active);
    for (target_resolution, active_level) in scales.into_iter().skip(target_index) {
        match plan_level(
            catalog,
            view,
            camera,
            viewport,
            active_level,
            target_resolution,
            effective_limits,
        ) {
            Ok(plan) => return Ok(plan),
            Err(PlanAttemptError::Capacity) => {}
            Err(PlanAttemptError::Other(error)) => return Err(error),
        }
    }
    Err(DatasetDemandPlanCapacityError::new(effective_limits).into())
}

pub(crate) fn plan_cross_section_panel(
    catalog: &DatasetCatalog,
    view: &ViewState,
    panel: PanelId,
    presentation: PresentationViewport,
    active_level: ScaleLevel,
    limits: DatasetDemandPlanLimits,
) -> anyhow::Result<DatasetDemandPlan> {
    let active = catalog
        .layer(view.active_layer())
        .ok_or_else(|| anyhow::anyhow!("active layer is absent from the dataset catalog"))?;
    let active_scale = active.scale(active_level).ok_or_else(|| {
        anyhow::anyhow!("active layer has no selected scale {}", active_level.get())
    })?;
    let target_resolution = representative_voxel_world_size(active_scale.grid_to_world());
    let panel = match panel {
        PanelId::Xy => CrossSectionPlane::Xy,
        PanelId::Xz => CrossSectionPlane::Xz,
        PanelId::Yz => CrossSectionPlane::Yz,
        PanelId::ThreeD => anyhow::bail!("the 3D panel is not a cross-section demand target"),
    };
    let mut plan = PlanAccumulator::default();
    for view_layer in view.layers().iter().filter(|layer| layer.visible()) {
        let key = view_layer.layer_key();
        let layer = catalog.layer(key).ok_or_else(|| {
            anyhow::anyhow!(
                "visible layer {} is absent from the dataset catalog",
                key.ordinal()
            )
        })?;
        let scale = if key == view.active_layer() {
            active_scale
        } else {
            scale_for_target_resolution(layer, target_resolution)
        };
        let regions = plan_cross_section_resource_regions(
            *view.cross_section(),
            panel,
            presentation,
            SemanticRegionGridSpec {
                volume_shape: scale.shape(),
                resource_shape: semantic_resource_shape(scale.shape()),
                grid_to_world: scale.grid_to_world(),
            },
            semantic_limits(limits, plan.resources.len()),
        )
        .map_err(plan_attempt_from_semantic_error);
        let regions = match regions {
            Ok(regions) => regions,
            Err(PlanAttemptError::Capacity) => {
                return Err(DatasetDemandPlanCapacityError::new(limits).into());
            }
            Err(PlanAttemptError::Other(error)) => return Err(error),
        };
        append_layer_resources(
            catalog,
            view,
            key,
            scale.level(),
            regions,
            &mut plan,
            limits,
        )
        .map_err(|error| match error {
            PlanAttemptError::Capacity => {
                anyhow::Error::new(DatasetDemandPlanCapacityError::new(limits))
            }
            PlanAttemptError::Other(error) => error,
        })?;
    }
    validate_multi_channel_dvr_grids(catalog, view, &plan.layer_scales)?;
    plan.resources.sort_unstable();
    plan.resources.dedup();
    Ok(DatasetDemandPlan {
        scale: active_level,
        layer_scales: plan.layer_scales,
        resources: plan.resources,
        decoded_bytes: plan.decoded_bytes,
    })
}

fn plan_level(
    catalog: &DatasetCatalog,
    view: &ViewState,
    camera: CameraFrame,
    viewport: RenderExtent,
    active_level: ScaleLevel,
    target_resolution: f64,
    limits: DatasetDemandPlanLimits,
) -> PlanAttemptResult<DatasetDemandPlan> {
    let mut plan = PlanAccumulator::default();
    for view_layer in view.layers().iter().filter(|layer| layer.visible()) {
        let key = view_layer.layer_key();
        let layer = catalog.layer(key).ok_or_else(|| {
            PlanAttemptError::Other(anyhow::anyhow!(
                "visible layer {} is absent from the dataset catalog",
                key.ordinal()
            ))
        })?;
        let scale = if key == view.active_layer() {
            layer.scale(active_level).ok_or_else(|| {
                PlanAttemptError::Other(anyhow::anyhow!(
                    "active layer has no selected scale {}",
                    active_level.get()
                ))
            })?
        } else {
            scale_for_target_resolution(layer, target_resolution)
        };
        let resource_shape = semantic_resource_shape(scale.shape());
        let regions = plan_visible_resource_regions(
            camera,
            viewport,
            SemanticRegionGridSpec {
                volume_shape: scale.shape(),
                resource_shape,
                grid_to_world: scale.grid_to_world(),
            },
            VolumePlanOptions {
                pixel_stride: u64::from(
                    viewport
                        .width_pixels()
                        .max(viewport.height_pixels())
                        .div_ceil(128)
                        .max(1),
                ),
            },
            semantic_limits(limits, plan.resources.len()),
        )
        .map_err(plan_attempt_from_semantic_error)?;
        append_layer_resources(
            catalog,
            view,
            key,
            scale.level(),
            regions,
            &mut plan,
            limits,
        )?;
    }
    validate_multi_channel_dvr_grids(catalog, view, &plan.layer_scales)
        .map_err(PlanAttemptError::Other)?;
    plan.resources.sort_unstable();
    plan.resources.dedup();
    Ok(DatasetDemandPlan {
        scale: active_level,
        layer_scales: plan.layer_scales,
        resources: plan.resources,
        decoded_bytes: plan.decoded_bytes,
    })
}

fn append_layer_resources(
    catalog: &DatasetCatalog,
    view: &ViewState,
    layer: LogicalLayerKey,
    scale: ScaleLevel,
    regions: Vec<ResourceRegion>,
    plan: &mut PlanAccumulator,
    limits: DatasetDemandPlanLimits,
) -> PlanAttemptResult<()> {
    for region in regions {
        if plan.resources.len() == limits.max_resources {
            return Err(PlanAttemptError::Capacity);
        }
        let key = DatasetResourceKey::new(
            catalog.scientific_identity().resource_identity(),
            layer,
            view.timepoint(),
            scale,
            region,
        );
        let descriptor = catalog
            .resource_payload_descriptor(key)
            .map_err(|error| PlanAttemptError::Other(error.into()))?;
        let next_decoded_bytes = plan
            .decoded_bytes
            .checked_add(descriptor.byte_len())
            .ok_or(PlanAttemptError::Capacity)?;
        if next_decoded_bytes > limits.max_decoded_bytes {
            return Err(PlanAttemptError::Capacity);
        }
        plan.decoded_bytes = next_decoded_bytes;
        plan.resources.push(key);
    }
    plan.layer_scales.insert(layer, scale);
    Ok(())
}

fn semantic_limits(
    limits: DatasetDemandPlanLimits,
    resources_already_planned: usize,
) -> SemanticPlanLimits {
    SemanticPlanLimits::new(
        limits.max_candidates_per_layer,
        limits
            .max_resources
            .saturating_sub(resources_already_planned),
    )
}

fn plan_attempt_from_semantic_error(error: SemanticPlanError) -> PlanAttemptError {
    if error.is_capacity() {
        PlanAttemptError::Capacity
    } else {
        PlanAttemptError::Other(error.into())
    }
}

fn scale_for_target_resolution(
    layer: &mirante4d_dataset::DatasetLayer,
    target_resolution: f64,
) -> &mirante4d_dataset::DatasetScale {
    let mut finest = None;
    let mut at_or_below_target = None;
    for scale in layer.scales() {
        let resolution = representative_voxel_world_size(scale.grid_to_world());
        if finest.is_none_or(|(current, _)| resolution < current) {
            finest = Some((resolution, scale));
        }
        if resolution <= target_resolution
            && at_or_below_target.is_none_or(|(current, _)| resolution > current)
        {
            at_or_below_target = Some((resolution, scale));
        }
    }
    at_or_below_target
        .or(finest)
        .map(|(_, scale)| scale)
        .expect("DatasetLayer always has at least one scale")
}

fn validate_multi_channel_dvr_grids(
    catalog: &DatasetCatalog,
    view: &ViewState,
    layer_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
) -> anyhow::Result<()> {
    let visible = view
        .layers()
        .iter()
        .filter(|layer| layer.visible())
        .collect::<Vec<_>>();
    if visible.len() < 2
        || !visible
            .iter()
            .all(|layer| layer.render_state().mode() == RenderMode::Dvr)
    {
        return Ok(());
    }
    let selected_scale = |key: LogicalLayerKey| -> anyhow::Result<_> {
        let level = layer_scales
            .get(&key)
            .copied()
            .ok_or(DatasetDemandPlanCompatibilityError)?;
        catalog
            .layer(key)
            .and_then(|layer| layer.scale(level))
            .ok_or_else(|| DatasetDemandPlanCompatibilityError.into())
    };
    let first = selected_scale(visible[0].layer_key())?;
    for layer in visible.into_iter().skip(1) {
        let selected = selected_scale(layer.layer_key())?;
        if selected.shape() != first.shape() || selected.grid_to_world() != first.grid_to_world() {
            return Err(DatasetDemandPlanCompatibilityError.into());
        }
    }
    Ok(())
}

pub(crate) fn semantic_resource_shape(volume: Shape3D) -> Shape3D {
    Shape3D::new(
        volume.z().min(SEMANTIC_TILE_SIDE),
        volume.y().min(SEMANTIC_TILE_SIDE),
        volume.x().min(SEMANTIC_TILE_SIDE),
    )
    .expect("a semantic resource clipped to a non-empty volume is non-empty")
}

fn representative_voxel_world_size(grid_to_world: mirante4d_domain::GridToWorld) -> f64 {
    let matrix = grid_to_world.row_major();
    let x = (matrix[0] * matrix[0] + matrix[4] * matrix[4] + matrix[8] * matrix[8]).sqrt();
    let y = (matrix[1] * matrix[1] + matrix[5] * matrix[5] + matrix[9] * matrix[9]).sqrt();
    let z = (matrix[2] * matrix[2] + matrix[6] * matrix[6] + matrix[10] * matrix[10]).sqrt();
    x.max(y).max(z).max(f64::EPSILON)
}

#[cfg(test)]
mod tests {
    use mirante4d_dataset::{
        DatasetLayer, DatasetScale, DatasetSourceId, ResourceValidity, ScientificIdentityStatus,
    };
    use mirante4d_domain::{
        CameraView, CrossSectionView, DisplayWindow, DvrOpacityTransfer, GridToWorld,
        IntensityDType, IsoLightState, LayerTransfer, Opacity, Projection, RenderState, RgbColor,
        SamplingPolicy, TimeIndex, TransferCurve, UnitQuaternion, ViewerLayout, WorldPoint3,
    };
    use mirante4d_project_model::LayerViewState;

    use super::*;

    #[test]
    fn semantic_resource_shape_clips_small_axes() {
        assert_eq!(
            semantic_resource_shape(Shape3D::new(3, 65, 128).unwrap()).dimensions(),
            [3, 64, 64]
        );
    }

    #[test]
    fn no_scale_plan_returns_stable_capacity_error_instead_of_over_budget_plan() {
        let (catalog, view) = two_layer_catalog_and_view();
        let error = plan_current_3d(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(4_096, 64, 0),
            false,
        )
        .unwrap_err();

        assert!(error.is::<DatasetDemandPlanCapacityError>());
        assert_eq!(
            error.to_string(),
            "dataset demand cannot fit within 64 resources, 0 decoded bytes, and 4096 candidates per visible layer"
        );
    }

    #[test]
    fn heterogeneous_visible_layers_use_physical_resolution_and_actual_scale_keys() {
        let (catalog, view) = two_layer_catalog_and_view();
        let plan = plan_current_3d(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(4_096, 64, 1_048_576),
            false,
        )
        .unwrap();

        assert_eq!(plan.scale, ScaleLevel::new(2));
        assert_eq!(
            plan.layer_scales,
            BTreeMap::from([
                (LogicalLayerKey::new(0), ScaleLevel::new(2)),
                (LogicalLayerKey::new(1), ScaleLevel::new(7)),
            ])
        );
        assert!(plan.resources.iter().any(|key| {
            key.layer() == LogicalLayerKey::new(0) && key.scale() == ScaleLevel::new(2)
        }));
        assert!(plan.resources.iter().any(|key| {
            key.layer() == LogicalLayerKey::new(1) && key.scale() == ScaleLevel::new(7)
        }));
        assert_eq!(
            plan.resources
                .iter()
                .map(|key| (key.layer(), key.scale()))
                .collect::<std::collections::BTreeSet<_>>(),
            plan.layer_scales.into_iter().collect()
        );
    }

    #[test]
    fn visible_layer_union_obeys_one_resource_bound() {
        let (catalog, view) = two_layer_catalog_and_view();
        let error = plan_current_3d(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(4_096, 1, 1_048_576),
            false,
        )
        .unwrap_err();

        assert!(error.is::<DatasetDemandPlanCapacityError>());

        let playback_plan = plan_current_3d(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(4_096, 4, 262_144),
            true,
        )
        .unwrap();
        assert_eq!(playback_plan.resources.len(), 2);
        assert!(playback_plan.resources.len() * 2 <= 4);
        assert!(playback_plan.decoded_bytes * 2 <= 262_144);
    }

    #[test]
    fn incompatible_multi_channel_dvr_grids_fail_before_demand_submission() {
        let active = LogicalLayerKey::new(0);
        let other = LogicalLayerKey::new(1);
        let catalog = DatasetCatalog::new(
            "incompatible-dvr-grids",
            ScientificIdentityStatus::Unverified(DatasetSourceId::new(1)),
            vec![
                multiscale_layer(active, ScaleLevel::new(2)),
                multiscale_layer_with_coarse_shape(
                    other,
                    ScaleLevel::new(7),
                    Shape3D::new(16, 32, 32).unwrap(),
                ),
            ],
        )
        .unwrap();
        let (_, mip_view) = two_layer_catalog_and_view();
        let view = ViewState::new(
            vec![dvr_view_layer(active), dvr_view_layer(other)],
            active,
            mip_view.timepoint(),
            *mip_view.camera(),
            mip_view.layout(),
            *mip_view.cross_section(),
            *mip_view.iso_light(),
        )
        .unwrap();

        let error = plan_current_3d(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(4_096, 64, 1_048_576),
            false,
        )
        .unwrap_err();

        assert!(error.is::<DatasetDemandPlanCompatibilityError>());
    }

    fn two_layer_catalog_and_view() -> (DatasetCatalog, ViewState) {
        let active = LogicalLayerKey::new(0);
        let other = LogicalLayerKey::new(1);
        let catalog = DatasetCatalog::new(
            "heterogeneous-scales",
            ScientificIdentityStatus::Unverified(DatasetSourceId::new(1)),
            vec![
                multiscale_layer(active, ScaleLevel::new(2)),
                multiscale_layer(other, ScaleLevel::new(7)),
            ],
        )
        .unwrap();
        let camera = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::new(64.0, 64.0, 64.0).unwrap(),
            UnitQuaternion::identity(),
            4.0,
            320.0,
            200.0,
        )
        .unwrap();
        let view = ViewState::new(
            vec![view_layer(active), view_layer(other)],
            active,
            TimeIndex::new(0),
            camera,
            ViewerLayout::Single3d,
            CrossSectionView::new(
                WorldPoint3::new(64.0, 64.0, 64.0).unwrap(),
                UnitQuaternion::identity(),
                1.0,
                1.0,
            )
            .unwrap(),
            IsoLightState::attached_camera(),
        )
        .unwrap();
        (catalog, view)
    }

    fn multiscale_layer(key: LogicalLayerKey, coarse_level: ScaleLevel) -> DatasetLayer {
        multiscale_layer_with_coarse_shape(key, coarse_level, Shape3D::new(32, 32, 32).unwrap())
    }

    fn multiscale_layer_with_coarse_shape(
        key: LogicalLayerKey,
        coarse_level: ScaleLevel,
        coarse_shape: Shape3D,
    ) -> DatasetLayer {
        DatasetLayer::new_multiscale(
            key,
            format!("layer-{}", key.ordinal()),
            1,
            IntensityDType::Uint16,
            vec![
                DatasetScale::new(
                    ScaleLevel::BASE,
                    Shape3D::new(128, 128, 128).unwrap(),
                    GridToWorld::scale(1.0, 1.0, 1.0).unwrap(),
                    ResourceValidity::AllValid,
                ),
                DatasetScale::new(
                    coarse_level,
                    coarse_shape,
                    GridToWorld::scale(4.0, 4.0, 4.0).unwrap(),
                    ResourceValidity::AllValid,
                ),
            ],
        )
        .unwrap()
    }

    fn view_layer(key: LogicalLayerKey) -> LayerViewState {
        LayerViewState::new(
            key,
            true,
            LayerTransfer::new(
                DisplayWindow::new(0.0, 65_535.0).unwrap(),
                RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
                Opacity::new(1.0).unwrap(),
                TransferCurve::linear(),
                false,
            ),
            RenderState::mip(SamplingPolicy::SmoothLinear),
        )
    }

    fn dvr_view_layer(key: LogicalLayerKey) -> LayerViewState {
        LayerViewState::new(
            key,
            true,
            LayerTransfer::new(
                DisplayWindow::new(0.0, 65_535.0).unwrap(),
                RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
                Opacity::new(1.0).unwrap(),
                TransferCurve::linear(),
                false,
            ),
            RenderState::dvr(
                SamplingPolicy::SmoothLinear,
                DvrOpacityTransfer::new(
                    DisplayWindow::new(0.0, 65_535.0).unwrap(),
                    TransferCurve::linear(),
                ),
                12.0,
            )
            .unwrap(),
        )
    }
}
