//! Stateless per-view, per-layer projected-affine LOD selection.
//!
//! Each visible layer is evaluated in the physical render target that will
//! display it. The selected level is the coarsest catalog scale whose complete
//! affine voxel cell projects to at most one physical pixel on both screen
//! axes. If no scale satisfies that rule, the scientific base scale remains
//! the finest available fallback.

use anyhow::{Context, Result, anyhow, ensure};
use glam::DVec3;
use mirante4d_application::viewport_interaction::CrossSectionViewState;
use mirante4d_dataset::{DatasetLayer, DatasetScale};
use mirante4d_domain::{CrossSectionView, GridToWorld, ScaleLevel, WorldPoint3};
use mirante4d_render_api::{CameraFrame, PresentationViewport, RenderExtent};

use crate::viewer_layout::PanelId;

const MAX_CELL_FOOTPRINT_PIXELS: f64 = 1.0;

/// Selects one layer's volume scale from its conservative projected affine
/// cell footprint at all eight volume-boundary corners.
pub(crate) fn select_volume_level(
    layer: &DatasetLayer,
    camera: CameraFrame,
    extent: RenderExtent,
) -> Result<ScaleLevel> {
    select_level(layer, |scale| volume_footprint(scale, camera, extent))
}

/// Selects one layer's cross-section scale in the physical axes of the
/// requested linked panel.
pub(crate) fn select_cross_section_level(
    layer: &DatasetLayer,
    view: CrossSectionView,
    panel: PanelId,
    presentation: PresentationViewport,
    extent: RenderExtent,
) -> Result<ScaleLevel> {
    let panel = panel
        .cross_section_panel()
        .ok_or_else(|| anyhow!("the 3D panel has no cross-section LOD axes"))?;
    let panel_view = CrossSectionViewState::from_canonical(view).view(panel);
    let right = DVec3::from_array(panel_view.right_world());
    let down = DVec3::from_array(panel_view.down_world());
    let world_per_pixel_x = panel_view.scale_world_per_screen_point() * presentation.width_points()
        / f64::from(extent.width_pixels());
    let world_per_pixel_y = panel_view.scale_world_per_screen_point()
        * presentation.height_points()
        / f64::from(extent.height_pixels());
    ensure!(
        finite_positive(world_per_pixel_x) && finite_positive(world_per_pixel_y),
        "cross-section LOD produced invalid physical-pixel geometry"
    );

    select_level(layer, |scale| {
        let basis = affine_basis(scale.grid_to_world())?;
        validate_basis(basis)?;
        Ok([
            projected_cell_extent(basis, right) / world_per_pixel_x,
            projected_cell_extent(basis, down) / world_per_pixel_y,
        ])
    })
}

fn select_level(
    layer: &DatasetLayer,
    mut footprint: impl FnMut(DatasetScale) -> Result<[f64; 2]>,
) -> Result<ScaleLevel> {
    let mut selected = ScaleLevel::BASE;
    for scale in layer.scales().copied() {
        let [footprint_x, footprint_y] = footprint(scale)
            .with_context(|| format!("failed to project scale {}", scale.level().get()))?;
        ensure!(
            footprint_x.is_finite()
                && footprint_x >= 0.0
                && footprint_y.is_finite()
                && footprint_y >= 0.0,
            "projected-affine LOD produced an invalid cell footprint"
        );
        if footprint_x <= MAX_CELL_FOOTPRINT_PIXELS && footprint_y <= MAX_CELL_FOOTPRINT_PIXELS {
            selected = scale.level();
        }
    }
    Ok(selected)
}

fn volume_footprint(
    scale: DatasetScale,
    camera: CameraFrame,
    extent: RenderExtent,
) -> Result<[f64; 2]> {
    let basis = affine_basis(scale.grid_to_world())?;
    validate_basis(basis)?;
    let shape = scale.shape();
    let mut maximum_x = 0.0_f64;
    let mut maximum_y = 0.0_f64;

    for z in [-0.5, shape.z() as f64 - 0.5] {
        for y in [-0.5, shape.y() as f64 - 0.5] {
            for x in [-0.5, shape.x() as f64 - 0.5] {
                let world = transform_point(scale.grid_to_world(), [x, y, z])?;
                let projected = project_pixels(camera, world, extent)?;
                let mut footprint_x = 0.0;
                let mut footprint_y = 0.0;
                for vector in basis {
                    let offset = project_pixels(camera, world + vector, extent)?;
                    footprint_x += (offset.x - projected.x).abs();
                    footprint_y += (offset.y - projected.y).abs();
                }
                maximum_x = maximum_x.max(footprint_x);
                maximum_y = maximum_y.max(footprint_y);
            }
        }
    }
    Ok([maximum_x, maximum_y])
}

fn affine_basis(transform: GridToWorld) -> Result<[DVec3; 3]> {
    let matrix = transform.row_major();
    let basis = [
        DVec3::new(matrix[0], matrix[4], matrix[8]),
        DVec3::new(matrix[1], matrix[5], matrix[9]),
        DVec3::new(matrix[2], matrix[6], matrix[10]),
    ];
    ensure!(
        basis.iter().all(|vector| vector.is_finite()),
        "LOD affine basis is not finite"
    );
    Ok(basis)
}

fn validate_basis(basis: [DVec3; 3]) -> Result<()> {
    ensure!(
        basis.iter().all(|vector| finite_positive(vector.length())),
        "LOD affine basis must have three nonzero finite vectors"
    );
    Ok(())
}

fn projected_cell_extent(basis: [DVec3; 3], screen_axis: DVec3) -> f64 {
    basis
        .iter()
        .map(|vector| vector.dot(screen_axis).abs())
        .sum()
}

fn transform_point(transform: GridToWorld, point: [f64; 3]) -> Result<DVec3> {
    let point = WorldPoint3::new(point[0], point[1], point[2])
        .context("LOD grid boundary is not finite")?;
    let world = transform
        .transform_point(point)
        .context("LOD grid-to-world projection is not finite")?;
    Ok(DVec3::from_array(world.components()))
}

fn project_pixels(camera: CameraFrame, world: DVec3, extent: RenderExtent) -> Result<glam::DVec2> {
    let world = WorldPoint3::new(world.x, world.y, world.z)
        .context("LOD world projection is not finite")?;
    let projected = camera
        .project_world_point(world)
        .context("camera projection failed during LOD selection")?
        .ok_or_else(|| anyhow!("volume intersects or lies behind the camera eye plane"))?;
    let presentation = camera.presentation();
    let projected = glam::DVec2::new(
        projected.screen_x_points() * f64::from(extent.width_pixels())
            / presentation.width_points(),
        projected.screen_y_points() * f64::from(extent.height_pixels())
            / presentation.height_points(),
    );
    ensure!(
        projected.is_finite(),
        "LOD physical-pixel projection is not finite"
    );
    Ok(projected)
}

fn finite_positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

#[cfg(test)]
mod tests {
    use mirante4d_dataset::{DatasetScale, ResourceValidity};
    use mirante4d_domain::{
        CameraView, GridToWorld, IntensityDType, LogicalLayerKey, Projection, Shape3D,
        UnitQuaternion, WorldPoint3,
    };

    use super::*;

    fn scale(level: u32, edge: u64, transform: GridToWorld) -> DatasetScale {
        DatasetScale::new(
            ScaleLevel::new(level),
            Shape3D::new(edge, edge, edge).unwrap(),
            transform,
            ResourceValidity::AllValid,
        )
    }

    fn layer(scales: Vec<DatasetScale>) -> DatasetLayer {
        DatasetLayer::new_multiscale(
            LogicalLayerKey::new(0),
            "projected LOD test",
            1,
            IntensityDType::Uint16,
            scales,
        )
        .unwrap()
    }

    fn cross_section(world_per_point: f64, orientation: UnitQuaternion) -> CrossSectionView {
        CrossSectionView::new(WorldPoint3::origin(), orientation, world_per_point, 1.0).unwrap()
    }

    fn presentation(side: f64) -> PresentationViewport {
        PresentationViewport::new(side, side).unwrap()
    }

    fn extent(side: u32) -> RenderExtent {
        RenderExtent::new(side, side).unwrap()
    }

    #[test]
    fn anisotropic_scales_are_selected_in_each_panels_physical_axes() {
        let layer = layer(vec![
            scale(0, 64, GridToWorld::scale(0.25, 0.25, 1.0).unwrap()),
            scale(1, 32, GridToWorld::scale(0.5, 0.5, 2.0).unwrap()),
        ]);
        let view = cross_section(1.0, UnitQuaternion::identity());
        let presentation = presentation(64.0);
        let extent = extent(64);

        assert_eq!(
            select_cross_section_level(&layer, view, PanelId::Xy, presentation, extent).unwrap(),
            ScaleLevel::new(1)
        );
        assert_eq!(
            select_cross_section_level(&layer, view, PanelId::Xz, presentation, extent).unwrap(),
            ScaleLevel::BASE
        );
        assert_eq!(
            select_cross_section_level(&layer, view, PanelId::Yz, presentation, extent).unwrap(),
            ScaleLevel::BASE
        );
    }

    #[test]
    fn rotated_view_and_sheared_grid_use_the_complete_affine_cell() {
        let fine = GridToWorld::from_row_major([
            0.4, 0.3, 0.0, 0.0, 0.0, 0.4, 0.0, 0.0, 0.0, 0.0, 0.4, 0.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .unwrap();
        let coarse = GridToWorld::from_row_major([
            0.8, 0.6, 0.0, 0.0, 0.0, 0.8, 0.0, 0.0, 0.0, 0.0, 0.8, 0.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .unwrap();
        let layer = layer(vec![scale(0, 64, fine), scale(1, 32, coarse)]);
        let half_angle = std::f64::consts::FRAC_PI_8;
        let orientation =
            UnitQuaternion::new_xyzw(0.0, 0.0, half_angle.sin(), half_angle.cos()).unwrap();

        assert_eq!(
            select_cross_section_level(
                &layer,
                cross_section(1.0, orientation),
                PanelId::Xy,
                presentation(64.0),
                extent(64),
            )
            .unwrap(),
            ScaleLevel::BASE
        );
    }

    #[test]
    fn perspective_selection_uses_physical_pixels_at_all_volume_corners() {
        let translated_scale = |spacing: f64, translation: f64| {
            GridToWorld::from_row_major([
                spacing,
                0.0,
                0.0,
                translation,
                0.0,
                spacing,
                0.0,
                translation,
                0.0,
                0.0,
                spacing,
                translation,
                0.0,
                0.0,
                0.0,
                1.0,
            ])
            .unwrap()
        };
        let layer = layer(vec![
            scale(0, 16, translated_scale(0.25, -1.875)),
            scale(1, 8, translated_scale(0.5, -1.75)),
            scale(2, 4, translated_scale(1.0, -1.5)),
        ]);
        let presentation = presentation(100.0);
        let camera = CameraFrame::new(
            CameraView::new(
                Projection::Perspective,
                WorldPoint3::origin(),
                UnitQuaternion::identity(),
                1.0,
                5.0,
                10.0,
            )
            .unwrap(),
            presentation,
        )
        .unwrap();

        assert_eq!(
            select_volume_level(&layer, camera, extent(100)).unwrap(),
            ScaleLevel::new(2)
        );
        assert_eq!(
            select_volume_level(&layer, camera, extent(200)).unwrap(),
            ScaleLevel::new(1)
        );
    }

    #[test]
    fn volume_orientation_can_change_lod_without_changing_camera_sampling_scalars() {
        let layer = layer(vec![
            scale(0, 64, GridToWorld::scale(0.25, 0.25, 0.25).unwrap()),
            scale(1, 32, GridToWorld::scale(2.0, 0.5, 0.5).unwrap()),
        ]);
        let presentation = presentation(64.0);
        let camera = |orientation| {
            CameraFrame::new(
                CameraView::new(
                    Projection::Orthographic,
                    WorldPoint3::new(32.0, 32.0, 32.0).unwrap(),
                    orientation,
                    1.0,
                    320.0,
                    200.0,
                )
                .unwrap(),
                presentation,
            )
            .unwrap()
        };
        let quarter_turn_y = UnitQuaternion::new_xyzw(
            0.0,
            std::f64::consts::FRAC_PI_4.sin(),
            0.0,
            std::f64::consts::FRAC_PI_4.cos(),
        )
        .unwrap();

        assert_eq!(
            select_volume_level(&layer, camera(UnitQuaternion::identity()), extent(64)).unwrap(),
            ScaleLevel::BASE
        );
        assert_eq!(
            select_volume_level(&layer, camera(quarter_turn_y), extent(64)).unwrap(),
            ScaleLevel::new(1)
        );
    }
}
