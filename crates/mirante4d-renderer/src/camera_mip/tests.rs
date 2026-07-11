use glam::DVec3;
use mirante4d_data::{DatasetHandle, DenseVolumeF32, DenseVolumeU8, DenseVolumeU16};
use mirante4d_domain::{DisplayWindow, GridToWorld, Projection, Shape3D, TimeIndex, TransferCurve};
use mirante4d_format::{
    ChannelMetadata, CurrentGridToWorldExt, DatasetId, DenseF32Layer, ExistingPackagePolicy,
    FixtureKind, LayerId, NativeF32Dataset, default_f32_display, expected_fixture_value,
    write_fixture, write_native_f32_dataset,
};
use mirante4d_render_api::CameraFrame;

use crate::render_mip_z;

use super::*;

fn iso_u16_threshold(threshold: u16) -> IsoSurfaceParameters {
    IsoSurfaceParameters::new(
        f32::from(threshold) / f32::from(u16::MAX),
        ScalarDisplayTransfer::identity_u16(),
    )
}

fn iso_f32_threshold(threshold: f32, low: f32, high: f32) -> IsoSurfaceParameters {
    IsoSurfaceParameters::new(
        ((threshold - low) / (high - low)).clamp(0.0, 1.0),
        ScalarDisplayTransfer::new(
            DisplayWindow::new(low, high).unwrap(),
            TransferCurve::linear(),
            false,
        ),
    )
}

fn iso_u16_display_parameters(
    display_level: f32,
    low: f32,
    high: f32,
    invert: bool,
) -> IsoSurfaceParameters {
    IsoSurfaceParameters::new(
        display_level,
        ScalarDisplayTransfer::new(
            DisplayWindow::new(low, high).unwrap(),
            TransferCurve::linear(),
            invert,
        ),
    )
}

fn dvr_parameters(low: f32, high: f32, density_scale: f64, invert: bool) -> DvrRenderParameters {
    let transfer = ScalarDisplayTransfer::new(
        DisplayWindow::new(low, high).unwrap(),
        TransferCurve::linear(),
        invert,
    );
    DvrRenderParameters::new(transfer, transfer, [1.0, 1.0, 1.0, 1.0], 1.0, density_scale)
}

#[test]
fn dvr_source_interval_skip_is_transfer_aware() {
    let normal = dvr_parameters(10.0, 20.0, 1.0, false);
    let inverted = dvr_parameters(10.0, 20.0, 1.0, true);
    let invisible = DvrRenderParameters::new(
        ScalarDisplayTransfer::new(
            DisplayWindow::new(10.0, 20.0).unwrap(),
            TransferCurve::linear(),
            false,
        ),
        ScalarDisplayTransfer::new(
            DisplayWindow::new(10.0, 20.0).unwrap(),
            TransferCurve::linear(),
            false,
        ),
        [1.0, 1.0, 1.0, 1.0],
        0.0,
        1.0,
    );

    assert!(!normal.source_interval_can_contribute(0.0, 5.0));
    assert!(normal.source_interval_can_contribute(15.0, 15.0));
    assert!(normal.source_interval_can_contribute(25.0, 30.0));
    assert!(inverted.source_interval_can_contribute(0.0, 5.0));
    assert!(!invisible.source_interval_can_contribute(25.0, 30.0));
    assert!(normal.source_interval_can_contribute(f64::NAN, 5.0));
}

#[test]
fn front_orthographic_camera_mip_matches_axis_aligned_z_mip() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
    let viewport = RenderViewport::new(16, 16).unwrap();
    let camera = front_orthographic_camera(&volume, 16.0);

    let (camera_image, camera_diagnostics) = render_camera_mip(&volume, camera, viewport).unwrap();
    let (axis_image, axis_diagnostics) = render_mip_z(&volume).unwrap();

    assert_eq!(camera_image.pixels(), axis_image.pixels());
    assert_eq!(camera_diagnostics, axis_diagnostics);
}

#[test]
fn orthographic_dolly_does_not_change_camera_mip_pixels() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
    let viewport = RenderViewport::new(16, 16).unwrap();
    let near_camera = front_orthographic_camera(&volume, 16.0);
    let center = DVec3::splat((16_u64.saturating_sub(1)) as f64 * 0.5);
    let height = volume
        .grid_to_world
        .transform_vector(DVec3::Y * 16.0)
        .length();
    let far_camera = crate::current_camera::frame_from_look_at(
        Projection::Orthographic,
        volume
            .grid_to_world
            .transform_point_vec(DVec3::new(center.x, center.y, center.z - 120.0)),
        volume.grid_to_world.transform_point_vec(center),
        volume.grid_to_world.transform_vector(-DVec3::Y).normalize(),
        1.0,
        height / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan()),
        crate::current_camera::presentation(height, height),
    );

    let (near_image, _) = render_camera_mip(&volume, near_camera, viewport).unwrap();
    let (far_image, _) = render_camera_mip(&volume, far_camera, viewport).unwrap();

    assert_eq!(far_image.pixels(), near_image.pixels());
}

#[test]
fn camera_mip_changes_when_view_direction_changes() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
    let viewport = RenderViewport::new(16, 16).unwrap();
    let front_camera = front_orthographic_camera(&volume, 16.0);
    let center = DVec3::splat((16_u64.saturating_sub(1)) as f64 * 0.5);
    let height = volume
        .grid_to_world
        .transform_vector(DVec3::Y * 16.0)
        .length();
    let side_camera = crate::current_camera::frame_from_look_at(
        Projection::Orthographic,
        volume
            .grid_to_world
            .transform_point_vec(DVec3::new(center.x + 120.0, center.y, center.z)),
        volume.grid_to_world.transform_point_vec(center),
        volume.grid_to_world.transform_vector(DVec3::Y).normalize(),
        1.0,
        height / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan()),
        crate::current_camera::presentation(height, height),
    );

    let (front_image, _) = render_camera_mip(&volume, front_camera, viewport).unwrap();
    let (side_image, _) = render_camera_mip(&volume, side_camera, viewport).unwrap();

    assert_ne!(side_image.pixels(), front_image.pixels());
}

#[test]
fn camera_mip_distinguishes_valid_zero_voxel_from_background() {
    let volume = DenseVolumeU16::new(
        DatasetId::new("camera-zero-background").unwrap(),
        LayerId::new("ch0").unwrap(),
        0,
        TimeIndex::new(0),
        Shape3D::new(1, 1, 1).unwrap(),
        GridToWorld::identity(),
        vec![0],
    )
    .unwrap();
    let viewport = RenderViewport::new(3, 3).unwrap();
    let camera = front_orthographic_camera(&volume, 3.0);

    let (image, diagnostics) = render_camera_mip(&volume, camera, viewport).unwrap();

    assert_eq!(image.pixel(1, 1), Some(0));
    assert_eq!(image.covered_pixel(1, 1), Some(true));
    assert_eq!(image.covered_pixel(0, 0), Some(false));
    assert_eq!(image.covered_pixel(2, 2), Some(false));
    assert_eq!(diagnostics.input_voxels, 1);
}

#[test]
fn inverted_iso_can_hit_valid_zero_without_covering_background() {
    let volume = DenseVolumeU16::new(
        DatasetId::new("camera-inverted-iso-zero").unwrap(),
        LayerId::new("ch0").unwrap(),
        0,
        TimeIndex::new(0),
        Shape3D::new(1, 1, 1).unwrap(),
        GridToWorld::identity(),
        vec![0],
    )
    .unwrap();
    let viewport = RenderViewport::new(3, 3).unwrap();
    let camera = front_orthographic_camera(&volume, 3.0);

    let (image, _) = render_camera(
        &volume,
        camera,
        viewport,
        CameraRenderMode::Isosurface {
            parameters: iso_u16_display_parameters(0.75, 0.0, 100.0, true),
        },
    )
    .unwrap();

    assert_eq!(image.pixel(1, 1), Some(u16::MAX));
    assert_eq!(image.covered_pixel(1, 1), Some(true));
    assert_eq!(image.covered_pixel(0, 0), Some(false));
    assert_eq!(image.covered_pixel(2, 2), Some(false));
}

#[test]
fn smooth_iso_surface_frame_preserves_material_scalar_apart_from_threshold_display() {
    let volume = DenseVolumeU16::new(
        DatasetId::new("camera-smooth-iso-surface").unwrap(),
        LayerId::new("ch0").unwrap(),
        0,
        TimeIndex::new(0),
        Shape3D::new(2, 1, 1).unwrap(),
        GridToWorld::identity(),
        vec![0, 100],
    )
    .unwrap();
    let viewport = RenderViewport::new(1, 1).unwrap();
    let camera = front_orthographic_camera(&volume, 1.0);

    let (image, _) = render_camera_with_quality(
        &volume,
        camera,
        viewport,
        CameraRenderMode::Isosurface {
            parameters: iso_u16_display_parameters(0.25, 0.0, 100.0, false),
        },
        CameraRenderQuality::smooth_linear(),
    )
    .unwrap();
    let surface = image
        .iso_surface()
        .expect("ISO frame should carry surface data");

    assert_eq!(image.covered_pixel(0, 0), Some(true));
    assert!(surface.is_covered_index(0));
    assert_eq!(surface.display_scalars()[0], image.pixels()[0]);
    assert!(
        surface.material_scalars()[0] > surface.display_scalars()[0],
        "material scalar should keep the current sample brightness instead of flattening to the threshold"
    );
    assert!(surface.source_values()[0] > 0);
    assert!(surface.hit_depth()[0].is_finite());
}

#[test]
fn inverted_iso_pick_uses_display_threshold_but_reports_source_value() {
    let volume = DenseVolumeU16::new(
        DatasetId::new("camera-inverted-iso-pick").unwrap(),
        LayerId::new("ch0").unwrap(),
        0,
        TimeIndex::new(0),
        Shape3D::new(2, 1, 1).unwrap(),
        GridToWorld::identity(),
        vec![100, 0],
    )
    .unwrap();
    let viewport = RenderViewport::new(1, 1).unwrap();
    let camera = front_orthographic_camera(&volume, 1.0);

    let normal = pick_camera_volume(
        &volume,
        camera,
        viewport,
        0,
        0,
        CameraRenderMode::Isosurface {
            parameters: iso_u16_display_parameters(0.75, 0.0, 100.0, false),
        },
    )
    .unwrap();
    let inverted = pick_camera_volume(
        &volume,
        camera,
        viewport,
        0,
        0,
        CameraRenderMode::Isosurface {
            parameters: iso_u16_display_parameters(0.75, 0.0, 100.0, true),
        },
    )
    .unwrap();

    assert_eq!(normal.intensity, 100);
    assert_eq!(
        normal.grid_position,
        Some(GridPosition {
            z: 0.0,
            y: 0.0,
            x: 0.0
        })
    );
    assert_eq!(inverted.intensity, 0);
    assert_eq!(
        inverted.grid_position,
        Some(GridPosition {
            z: 1.0,
            y: 0.0,
            x: 0.0
        })
    );
}

#[test]
fn center_aligned_downsampled_lod_does_not_shift_under_fixed_oblique_camera() {
    let dataset_id = DatasetId::new("lod-stability").unwrap();
    let layer_id = LayerId::new("ch0").unwrap();
    let s0_shape = Shape3D::new(8, 8, 8).unwrap();
    let s1_shape = Shape3D::new(4, 4, 4).unwrap();
    let s0_grid_to_world = GridToWorld::identity();
    let s1_grid_to_world = s0_grid_to_world
        .downsampled_integer_centered(2, 2, 2)
        .unwrap();
    let stale_s1_grid_to_world = mirante4d_format::grid_to_world_scale_um(2.0, 2.0, 2.0);
    let mut s0_values = vec![0; s0_shape.element_count().unwrap() as usize];
    for z in 4..=5 {
        for y in 4..=5 {
            for x in 2..=3 {
                s0_values[((z * s0_shape.y() + y) * s0_shape.x() + x) as usize] = 50_000;
            }
        }
    }
    let mut s1_values = vec![0; s1_shape.element_count().unwrap() as usize];
    s1_values[((2 * s1_shape.y() + 2) * s1_shape.x() + 1) as usize] = 50_000;
    let s0_volume = DenseVolumeU16::new(
        dataset_id.clone(),
        layer_id.clone(),
        0,
        TimeIndex::new(0),
        s0_shape,
        s0_grid_to_world,
        s0_values,
    )
    .unwrap();
    let s1_volume = DenseVolumeU16::new(
        dataset_id.clone(),
        layer_id.clone(),
        1,
        TimeIndex::new(0),
        s1_shape,
        s1_grid_to_world,
        s1_values.clone(),
    )
    .unwrap();
    let stale_s1_volume = DenseVolumeU16::new(
        dataset_id,
        layer_id,
        1,
        TimeIndex::new(0),
        s1_shape,
        stale_s1_grid_to_world,
        s1_values,
    )
    .unwrap();
    let camera = crate::current_camera::frame_from_look_at(
        Projection::Orthographic,
        DVec3::new(12.0, 10.0, 14.0),
        DVec3::new(3.5, 3.5, 3.5),
        DVec3::Y,
        1.0,
        10.0 / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan()),
        crate::current_camera::presentation(10.0, 10.0),
    );
    let viewport = RenderViewport::new(96, 96).unwrap();

    let (s0_image, s0_diagnostics) = render_camera_mip(&s0_volume, camera, viewport).unwrap();
    let (s1_image, s1_diagnostics) = render_camera_mip(&s1_volume, camera, viewport).unwrap();
    let (stale_image, stale_diagnostics) =
        render_camera_mip(&stale_s1_volume, camera, viewport).unwrap();

    assert!(s0_diagnostics.nonzero_pixels > 0);
    assert!(s1_diagnostics.nonzero_pixels > 0);
    assert!(stale_diagnostics.nonzero_pixels > 0);
    let s0_centroid = weighted_u16_centroid(&s0_image).unwrap();
    let s1_centroid = weighted_u16_centroid(&s1_image).unwrap();
    let stale_centroid = weighted_u16_centroid(&stale_image).unwrap();
    assert!(
        screen_distance(s0_centroid, s1_centroid) <= 0.25,
        "center-aligned LOD moved from {s0_centroid:?} to {s1_centroid:?}"
    );
    assert!(
        screen_distance(s0_centroid, stale_centroid) > 1.0,
        "stale-origin LOD should visibly move; source {s0_centroid:?}, stale {stale_centroid:?}"
    );
}

#[test]
fn isosurface_mode_returns_first_threshold_hit() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
    let viewport = RenderViewport::new(16, 16).unwrap();
    let camera = front_orthographic_camera(&volume, 16.0);

    let (image, diagnostics) = render_camera(
        &volume,
        camera,
        viewport,
        CameraRenderMode::Isosurface {
            parameters: iso_u16_threshold(3_000),
        },
    )
    .unwrap();

    assert_eq!(image.pixel(0, 0), Some(expected_fixture_value(0, 12, 0, 0)));
    assert!(diagnostics.nonzero_pixels > 0);
}

#[test]
fn dvr_mode_composites_nonzero_volume_signal() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
    let viewport = RenderViewport::new(16, 16).unwrap();
    let camera = front_orthographic_camera(&volume, 16.0);

    let (dvr_image, diagnostics) = render_camera(
        &volume,
        camera,
        viewport,
        CameraRenderMode::Dvr {
            parameters: dvr_parameters(0.0, f32::from(u16::MAX), 12.0, false),
        },
    )
    .unwrap();
    let (mip_image, _) = render_camera_mip(&volume, camera, viewport).unwrap();

    assert!(diagnostics.nonzero_pixels > 0);
    assert!(dvr_image.dvr_rgba().is_some());
    assert_ne!(dvr_image.pixels(), mip_image.pixels());
}

#[test]
fn same_ray_dvr_channels_are_order_independent_for_overlapping_samples() {
    let shape = Shape3D::new(1, 1, 1).unwrap();
    let grid_to_world = GridToWorld::identity();
    let red_volume = DenseVolumeU16::new(
        DatasetId::new("same-ray-dvr").unwrap(),
        LayerId::new("red").unwrap(),
        0,
        TimeIndex::new(0),
        shape,
        grid_to_world,
        vec![100],
    )
    .unwrap();
    let green_volume = DenseVolumeU16::new(
        DatasetId::new("same-ray-dvr").unwrap(),
        LayerId::new("green").unwrap(),
        0,
        TimeIndex::new(0),
        shape,
        grid_to_world,
        vec![100],
    )
    .unwrap();
    let transfer = ScalarDisplayTransfer::new(
        DisplayWindow::new(0.0, 100.0).unwrap(),
        TransferCurve::linear(),
        false,
    );
    let red = DvrRenderParameters::new(transfer, transfer, [1.0, 0.0, 0.0, 1.0], 1.0, 1.0);
    let green = DvrRenderParameters::new(transfer, transfer, [0.0, 1.0, 0.0, 1.0], 1.0, 1.0);
    let viewport = RenderViewport::new(1, 1).unwrap();
    let camera = front_orthographic_camera(&red_volume, 1.0);

    let (red_green, _) = render_dvr_channels_with_quality(
        &[
            DvrVolumeChannel::u16(&red_volume, red),
            DvrVolumeChannel::u16(&green_volume, green),
        ],
        camera,
        viewport,
        CameraRenderQuality::voxel_exact(),
    )
    .unwrap();
    let (green_red, _) = render_dvr_channels_with_quality(
        &[
            DvrVolumeChannel::u16(&green_volume, green),
            DvrVolumeChannel::u16(&red_volume, red),
        ],
        camera,
        viewport,
        CameraRenderQuality::voxel_exact(),
    )
    .unwrap();

    assert_eq!(red_green.pixels(), green_red.pixels());
    let red_green_rgba = red_green.dvr_rgba().unwrap().premultiplied_rgba()[0];
    let green_red_rgba = green_red.dvr_rgba().unwrap().premultiplied_rgba()[0];
    for component in 0..4 {
        assert!((red_green_rgba[component] - green_red_rgba[component]).abs() < 1.0e-6);
    }
    assert!(red_green_rgba[0] > 0.0);
    assert!(red_green_rgba[1] > 0.0);
    assert_eq!(red_green_rgba[2], 0.0);
    assert!(red_green_rgba[3] > 0.0);
}

#[test]
fn dvr_opacity_transfer_controls_front_to_back_visibility() {
    let shape = Shape3D::new(2, 1, 1).unwrap();
    let grid_to_world = GridToWorld::identity();
    let near_red_volume = DenseVolumeU16::new(
        DatasetId::new("dvr-depth").unwrap(),
        LayerId::new("near-red").unwrap(),
        0,
        TimeIndex::new(0),
        shape,
        grid_to_world,
        vec![100, 0],
    )
    .unwrap();
    let far_green_volume = DenseVolumeU16::new(
        DatasetId::new("dvr-depth").unwrap(),
        LayerId::new("far-green").unwrap(),
        0,
        TimeIndex::new(0),
        shape,
        grid_to_world,
        vec![0, 100],
    )
    .unwrap();
    let color_transfer = ScalarDisplayTransfer::new(
        DisplayWindow::new(0.0, 100.0).unwrap(),
        TransferCurve::linear(),
        false,
    );
    let visible_opacity = ScalarDisplayTransfer::new(
        DisplayWindow::new(50.0, 100.0).unwrap(),
        TransferCurve::linear(),
        false,
    );
    let transparent_opacity = ScalarDisplayTransfer::new(
        DisplayWindow::new(150.0, 200.0).unwrap(),
        TransferCurve::linear(),
        false,
    );
    let near_opaque_red = DvrRenderParameters::new(
        color_transfer,
        visible_opacity,
        [1.0, 0.0, 0.0, 1.0],
        1.0,
        64.0,
    );
    let near_transparent_red = DvrRenderParameters::new(
        color_transfer,
        transparent_opacity,
        [1.0, 0.0, 0.0, 1.0],
        1.0,
        64.0,
    );
    let far_green = DvrRenderParameters::new(
        color_transfer,
        visible_opacity,
        [0.0, 1.0, 0.0, 1.0],
        1.0,
        64.0,
    );
    let viewport = RenderViewport::new(1, 1).unwrap();
    let camera = front_orthographic_camera(&near_red_volume, 1.0);

    let (red_occludes_green, _) = render_dvr_channels_with_quality(
        &[
            DvrVolumeChannel::u16(&near_red_volume, near_opaque_red),
            DvrVolumeChannel::u16(&far_green_volume, far_green),
        ],
        camera,
        viewport,
        CameraRenderQuality::voxel_exact(),
    )
    .unwrap();
    let occluding_rgba = red_occludes_green.dvr_rgba().unwrap().premultiplied_rgba()[0];
    assert!(occluding_rgba[0] > 0.99);
    assert!(occluding_rgba[1] < 0.01);
    assert!(occluding_rgba[3] > 0.99);

    let (green_visible, _) = render_dvr_channels_with_quality(
        &[
            DvrVolumeChannel::u16(&near_red_volume, near_transparent_red),
            DvrVolumeChannel::u16(&far_green_volume, far_green),
        ],
        camera,
        viewport,
        CameraRenderQuality::voxel_exact(),
    )
    .unwrap();
    let visible_rgba = green_visible.dvr_rgba().unwrap().premultiplied_rgba()[0];
    assert!(visible_rgba[0] < 0.01);
    assert!(visible_rgba[1] > 0.99);
    assert!(visible_rgba[3] > 0.99);
}

#[test]
fn dvr_inverted_color_does_not_create_opacity() {
    let shape = Shape3D::new(1, 1, 1).unwrap();
    let volume = DenseVolumeU16::new(
        DatasetId::new("dvr-color-opacity").unwrap(),
        LayerId::new("zero").unwrap(),
        0,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        vec![0],
    )
    .unwrap();
    let inverted_color = ScalarDisplayTransfer::new(
        DisplayWindow::new(0.0, 100.0).unwrap(),
        TransferCurve::linear(),
        true,
    );
    let opacity_transfer = ScalarDisplayTransfer::new(
        DisplayWindow::new(10.0, 100.0).unwrap(),
        TransferCurve::linear(),
        false,
    );
    let parameters = DvrRenderParameters::new(
        inverted_color,
        opacity_transfer,
        [1.0, 1.0, 1.0, 1.0],
        1.0,
        64.0,
    );
    let viewport = RenderViewport::new(1, 1).unwrap();
    let camera = front_orthographic_camera(&volume, 1.0);

    let (image, diagnostics) = render_camera(
        &volume,
        camera,
        viewport,
        CameraRenderMode::Dvr { parameters },
    )
    .unwrap();
    let rgba = image.dvr_rgba().unwrap().premultiplied_rgba()[0];

    assert_eq!(diagnostics.nonzero_pixels, 0);
    assert_eq!(rgba, [0.0, 0.0, 0.0, 0.0]);
}

#[test]
fn camera_modes_treat_render_invalid_samples_as_transparent() {
    let volume = DenseVolumeU16::new(
        DatasetId::new("camera-masked").unwrap(),
        LayerId::new("ch0").unwrap(),
        0,
        TimeIndex::new(0),
        Shape3D::new(2, 1, 1).unwrap(),
        GridToWorld::identity(),
        vec![u16::MAX, 12],
    )
    .unwrap()
    .with_render_valid(vec![0, 1])
    .unwrap();
    let viewport = RenderViewport::new(1, 1).unwrap();
    let camera = front_orthographic_camera(&volume, 1.0);
    let (mip, mip_diagnostics) =
        render_camera(&volume, camera, viewport, CameraRenderMode::Mip).unwrap();
    let (iso, _) = render_camera(
        &volume,
        camera,
        viewport,
        CameraRenderMode::Isosurface {
            parameters: iso_u16_threshold(u16::MAX),
        },
    )
    .unwrap();
    let dvr_volume = DenseVolumeU16::new(
        DatasetId::new("camera-masked-dvr").unwrap(),
        LayerId::new("ch0").unwrap(),
        0,
        TimeIndex::new(0),
        Shape3D::new(2, 1, 1).unwrap(),
        GridToWorld::identity(),
        vec![u16::MAX, 0],
    )
    .unwrap()
    .with_render_valid(vec![0, 1])
    .unwrap();
    let (dvr, dvr_diagnostics) = render_camera(
        &dvr_volume,
        front_orthographic_camera(&dvr_volume, 1.0),
        viewport,
        CameraRenderMode::Dvr {
            parameters: dvr_parameters(0.0, f32::from(u16::MAX), 12.0, false),
        },
    )
    .unwrap();

    assert_eq!(mip.pixel(0, 0), Some(12));
    assert_eq!(mip.covered_pixel(0, 0), Some(true));
    assert_eq!(mip_diagnostics.input_voxels, 1);
    assert_eq!(iso.pixel(0, 0), Some(0));
    assert_eq!(iso.covered_pixel(0, 0), Some(false));
    assert_eq!(dvr.pixel(0, 0), Some(0));
    assert_eq!(dvr.covered_pixel(0, 0), Some(false));
    assert_eq!(dvr_diagnostics.input_voxels, 1);
}

#[test]
fn iso_gradient_mode_writes_normals_with_neutral_camera_pass_lighting() {
    let shape = Shape3D::new(3, 3, 3).unwrap();
    let mut values = Vec::with_capacity(shape.element_count().unwrap() as usize);
    for _z in 0..shape.z() {
        for _y in 0..shape.y() {
            for x in 0..shape.x() {
                values.push(1_000 + (x as u16) * 1_000);
            }
        }
    }
    let volume = DenseVolumeU16::new(
        DatasetId::new("iso-lighting").unwrap(),
        LayerId::new("ch0").unwrap(),
        0,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        values,
    )
    .unwrap();
    let camera = crate::current_camera::frame_from_look_at(
        Projection::Orthographic,
        DVec3::new(1.0, 1.0, 8.0),
        DVec3::new(1.0, 1.0, 1.0),
        DVec3::Y,
        1.0,
        1.0 / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan()),
        crate::current_camera::presentation(1.0, 1.0),
    );
    let viewport = RenderViewport::new(1, 1).unwrap();
    let mode = CameraRenderMode::Isosurface {
        parameters: iso_u16_threshold(1),
    };

    let (flat, _) = render_camera_with_quality(
        &volume,
        camera,
        viewport,
        mode,
        CameraRenderQuality {
            intensity_sampling: IntensitySamplingPolicy::VoxelExact,
            iso_shading: IsoShadingMode::Flat,
        },
    )
    .unwrap();
    let (lit, _) = render_camera_with_quality(
        &volume,
        camera,
        viewport,
        mode,
        CameraRenderQuality {
            intensity_sampling: IntensitySamplingPolicy::VoxelExact,
            iso_shading: IsoShadingMode::GradientLighting,
        },
    )
    .unwrap();

    assert_eq!(flat.pixel(0, 0), Some(2_000));
    assert_eq!(lit.pixel(0, 0), Some(2_000));
    assert_eq!(flat.surface_lighting_factor_index(0), 1.0);
    assert_eq!(lit.surface_lighting_factor_index(0), 1.0);
    assert_eq!(
        flat.iso_surface().unwrap().normals()[0],
        IsoSurfaceNormal::ZERO
    );
    let normal = lit.iso_surface().unwrap().normals()[0].components_f32();
    assert!(normal[0] > 0.999);
    assert!(normal[1].abs() < 1.0e-6);
    assert!(normal[2].abs() < 1.0e-6);
    assert_eq!(lit.iso_surface().unwrap().specular_lighting()[0], 0);
}

#[test]
fn camera_volume_pick_reports_policy_source_value_and_position() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
    let viewport = RenderViewport::new(16, 16).unwrap();
    let camera = front_orthographic_camera(&volume, 16.0);
    let mip = pick_camera_volume(&volume, camera, viewport, 0, 0, CameraRenderMode::Mip).unwrap();
    let iso = pick_camera_volume(
        &volume,
        camera,
        viewport,
        0,
        0,
        CameraRenderMode::Isosurface {
            parameters: iso_u16_threshold(3_000),
        },
    )
    .unwrap();
    let dvr = pick_camera_volume(
        &volume,
        camera,
        viewport,
        0,
        0,
        CameraRenderMode::Dvr {
            parameters: dvr_parameters(0.0, f32::from(u16::MAX), 12.0, false),
        },
    )
    .unwrap();
    assert_eq!(mip.policy, PickPolicy::MipArgmax);
    assert_eq!(mip.intensity, expected_fixture_value(0, 15, 0, 0));
    assert_eq!(
        mip.grid_position,
        Some(GridPosition {
            z: 15.0,
            y: 0.0,
            x: 0.0
        })
    );
    assert_eq!(mip.completeness, PickCompleteness::Exact);

    assert_eq!(iso.policy, PickPolicy::FirstThresholdHit);
    assert_eq!(iso.intensity, expected_fixture_value(0, 12, 0, 0));
    assert_eq!(
        iso.grid_position,
        Some(GridPosition {
            z: 12.0,
            y: 0.0,
            x: 0.0
        })
    );

    assert_eq!(dvr.policy, PickPolicy::ProbeRay);
    assert_eq!(dvr.intensity, expected_fixture_value(0, 1, 0, 0));
    assert_eq!(
        dvr.grid_position,
        Some(GridPosition {
            z: 1.0,
            y: 0.0,
            x: 0.0
        })
    );
}

#[test]
fn camera_volume_pick_rejects_out_of_bounds_pixels() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
    let viewport = RenderViewport::new(16, 16).unwrap();
    let camera = front_orthographic_camera(&volume, 16.0);

    let error =
        pick_camera_volume(&volume, camera, viewport, 16, 0, CameraRenderMode::Mip).unwrap_err();

    assert_eq!(
        error,
        RenderError::InvalidReadoutPixel {
            x: 16,
            y: 0,
            width: 16,
            height: 16
        }
    );
}

#[test]
fn float32_camera_modes_preserve_source_values_with_explicit_dvr_window() {
    let volume = float32_camera_volume();
    let viewport = RenderViewport::new(3, 2).unwrap();
    let camera = front_orthographic_camera_f32(&volume, 2.0);

    let (mip, mip_diagnostics) = render_camera_mip_f32(&volume, camera, viewport).unwrap();
    let (iso, _) = render_camera_f32(
        &volume,
        camera,
        viewport,
        CameraRenderModeF32::Isosurface {
            parameters: iso_f32_threshold(2.0, 0.0, 8.0),
        },
    )
    .unwrap();
    let (dvr, _) = render_camera_f32(
        &volume,
        camera,
        viewport,
        CameraRenderModeF32::Dvr {
            parameters: dvr_parameters(0.0, 8.0, 4.0, false),
        },
    )
    .unwrap();

    assert_eq!(mip.pixels(), &[-2.0, 2.25, 1.5, 4.0, -0.5, 8.0]);
    assert_eq!(mip_diagnostics.input_voxels, 12);
    assert_eq!(mip_diagnostics.output_pixels, 6);
    assert_eq!(mip_diagnostics.max_value, 8.0);
    assert_eq!(iso.pixels(), &[0.0, 0.28125, 0.0, 0.5, 0.0, 1.0]);
    assert!(dvr.pixels().iter().all(|value| (0.0..=1.0).contains(value)));
    assert!(dvr.dvr_rgba().is_some());
    assert_ne!(dvr.pixels(), mip.pixels());
}

#[test]
fn float32_camera_pick_reports_exact_source_value_and_position() {
    let volume = float32_camera_volume();
    let viewport = RenderViewport::new(3, 2).unwrap();
    let camera = front_orthographic_camera_f32(&volume, 2.0);

    let mip =
        pick_camera_volume_f32(&volume, camera, viewport, 1, 0, CameraRenderModeF32::Mip).unwrap();
    let iso = pick_camera_volume_f32(
        &volume,
        camera,
        viewport,
        1,
        0,
        CameraRenderModeF32::Isosurface {
            parameters: iso_f32_threshold(2.0, 0.0, 8.0),
        },
    )
    .unwrap();
    assert_eq!(mip.policy, PickPolicy::MipArgmax);
    assert_eq!(mip.intensity, 2.25);
    assert_eq!(
        mip.grid_position,
        Some(GridPosition {
            z: 1.0,
            y: 0.0,
            x: 1.0
        })
    );
    assert_eq!(mip.completeness, PickCompleteness::Exact);

    assert_eq!(iso.policy, PickPolicy::FirstThresholdHit);
    assert_eq!(iso.intensity, 2.25);
    assert_eq!(iso.grid_position, mip.grid_position);
}

#[test]
fn uint8_camera_pick_reports_exact_source_value_and_position() {
    let volume = uint8_camera_volume();
    let viewport = RenderViewport::new(3, 2).unwrap();
    let camera = front_orthographic_camera_u8(&volume, 2.0);

    let mip =
        pick_camera_volume_u8(&volume, camera, viewport, 1, 0, CameraRenderMode::Mip).unwrap();
    let iso = pick_camera_volume_u8(
        &volume,
        camera,
        viewport,
        1,
        0,
        CameraRenderMode::Isosurface {
            parameters: iso_u16_display_parameters(0.5, 0.0, f32::from(u8::MAX), false),
        },
    )
    .unwrap();
    assert_eq!(mip.policy, PickPolicy::MipArgmax);
    assert_eq!(mip.intensity, 225);
    assert_eq!(
        mip.grid_position,
        Some(GridPosition {
            z: 1.0,
            y: 0.0,
            x: 1.0
        })
    );
    assert_eq!(mip.completeness, PickCompleteness::Exact);

    assert_eq!(iso.policy, PickPolicy::FirstThresholdHit);
    assert_eq!(iso.intensity, 225);
    assert_eq!(iso.grid_position, mip.grid_position);
}

#[test]
fn uint8_camera_render_matches_equivalent_uint16_modes() {
    let volume = uint8_camera_volume();
    let volume_u16 = uint16_from_uint8_camera_volume(&volume);
    let viewport = RenderViewport::new(3, 2).unwrap();
    let camera = front_orthographic_camera_u8(&volume, 2.0);
    let modes = [
        ("mip", CameraRenderMode::Mip),
        (
            "iso",
            CameraRenderMode::Isosurface {
                parameters: iso_u16_display_parameters(0.5, 0.0, f32::from(u8::MAX), false),
            },
        ),
        (
            "dvr",
            CameraRenderMode::Dvr {
                parameters: dvr_parameters(0.0, f32::from(u8::MAX), 4.0, false),
            },
        ),
    ];

    for (label, mode) in modes {
        let (u8_frame, u8_diagnostics) = render_camera_u8_with_quality(
            &volume,
            camera,
            viewport,
            mode,
            CameraRenderQuality::smooth_linear(),
        )
        .unwrap();
        let (u16_frame, u16_diagnostics) = render_camera_with_quality(
            &volume_u16,
            camera,
            viewport,
            mode,
            CameraRenderQuality::smooth_linear(),
        )
        .unwrap();

        assert_eq!(u8_frame, u16_frame, "{label}");
        assert_eq!(u8_diagnostics, u16_diagnostics, "{label}");
    }
}

#[test]
fn rejects_zero_sized_viewport() {
    assert_eq!(
        RenderViewport::new(0, 16).unwrap_err(),
        RenderError::InvalidViewport {
            width: 0,
            height: 16
        }
    );
}

fn front_orthographic_camera(
    volume: &DenseVolumeU16,
    orthographic_height_grid: f64,
) -> CameraFrame {
    let center_x = (volume.shape.x().saturating_sub(1)) as f64 * 0.5;
    let center_y = (volume.shape.y().saturating_sub(1)) as f64 * 0.5;
    let center_z = (volume.shape.z().saturating_sub(1)) as f64 * 0.5;
    let depth = volume.shape.z() as f64;
    let height = volume
        .grid_to_world
        .transform_vector(DVec3::Y * orthographic_height_grid)
        .length();
    crate::current_camera::frame_from_look_at(
        Projection::Orthographic,
        volume.grid_to_world.transform_point_vec(DVec3::new(
            center_x,
            center_y,
            center_z - depth * 1.5,
        )),
        volume
            .grid_to_world
            .transform_point_vec(DVec3::new(center_x, center_y, center_z)),
        volume.grid_to_world.transform_vector(-DVec3::Y).normalize(),
        1.0,
        height / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan()),
        crate::current_camera::presentation(height, height),
    )
}

fn front_orthographic_camera_f32(
    volume: &DenseVolumeF32,
    orthographic_height_grid: f64,
) -> CameraFrame {
    let center_x = (volume.shape.x().saturating_sub(1)) as f64 * 0.5;
    let center_y = (volume.shape.y().saturating_sub(1)) as f64 * 0.5;
    let center_z = (volume.shape.z().saturating_sub(1)) as f64 * 0.5;
    let depth = volume.shape.z() as f64;
    let height = volume
        .grid_to_world
        .transform_vector(DVec3::Y * orthographic_height_grid)
        .length();
    crate::current_camera::frame_from_look_at(
        Projection::Orthographic,
        volume.grid_to_world.transform_point_vec(DVec3::new(
            center_x,
            center_y,
            center_z - depth * 1.5,
        )),
        volume
            .grid_to_world
            .transform_point_vec(DVec3::new(center_x, center_y, center_z)),
        volume.grid_to_world.transform_vector(-DVec3::Y).normalize(),
        1.0,
        height / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan()),
        crate::current_camera::presentation(height, height),
    )
}

fn front_orthographic_camera_u8(
    volume: &DenseVolumeU8,
    orthographic_height_grid: f64,
) -> CameraFrame {
    let center_x = (volume.shape.x().saturating_sub(1)) as f64 * 0.5;
    let center_y = (volume.shape.y().saturating_sub(1)) as f64 * 0.5;
    let center_z = (volume.shape.z().saturating_sub(1)) as f64 * 0.5;
    let depth = volume.shape.z() as f64;
    let height = volume
        .grid_to_world
        .transform_vector(DVec3::Y * orthographic_height_grid)
        .length();
    crate::current_camera::frame_from_look_at(
        Projection::Orthographic,
        volume.grid_to_world.transform_point_vec(DVec3::new(
            center_x,
            center_y,
            center_z - depth * 1.5,
        )),
        volume
            .grid_to_world
            .transform_point_vec(DVec3::new(center_x, center_y, center_z)),
        volume.grid_to_world.transform_vector(-DVec3::Y).normalize(),
        1.0,
        height / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan()),
        crate::current_camera::presentation(height, height),
    )
}

fn weighted_u16_centroid(image: &MipImageU16) -> Option<(f64, f64)> {
    let mut sum = 0.0;
    let mut x_sum = 0.0;
    let mut y_sum = 0.0;
    for (index, value) in image.pixels().iter().copied().enumerate() {
        if value == 0 {
            continue;
        }
        let weight = f64::from(value);
        let x = (index as u64 % image.width) as f64;
        let y = (index as u64 / image.width) as f64;
        sum += weight;
        x_sum += x * weight;
        y_sum += y * weight;
    }
    (sum > 0.0).then_some((x_sum / sum, y_sum / sum))
}

fn screen_distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    let dx = a.0 - b.0;
    let dy = a.1 - b.1;
    (dx * dx + dy * dy).sqrt()
}

fn float32_camera_volume() -> DenseVolumeF32 {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("float32-camera.m4d");
    let shape = mirante4d_domain::Shape4D::new(1, 2, 2, 3).unwrap();
    write_native_f32_dataset(
        &package,
        NativeF32Dataset {
            id: "float32-camera".to_owned(),
            name: "Float32 Camera".to_owned(),
            world_space: mirante4d_format::WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_format::WorldUnit::Micrometer,
            },
            layers: vec![DenseF32Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [1.0, 1.0, 1.0, 1.0],
                },
                shape,
                brick_shape: mirante4d_domain::Shape4D::new(1, 1, 2, 3).unwrap(),
                grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
                display: default_f32_display(),
                values_tzyx: vec![
                    -3.0, 0.25, 1.5, //
                    4.0, -1.0, 0.5, //
                    -2.0, 2.25, 1.0, //
                    3.0, -0.5, 8.0,
                ],
            }],
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();
    let dataset = DatasetHandle::open(&package).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    dataset
        .read_f32_volume(&layer_id, TimeIndex::new(0))
        .unwrap()
}

fn uint8_camera_volume() -> DenseVolumeU8 {
    DenseVolumeU8::new(
        DatasetId::new("uint8-camera").unwrap(),
        LayerId::new("ch0").unwrap(),
        0,
        TimeIndex::new(0),
        Shape3D::new(2, 2, 3).unwrap(),
        mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
        vec![
            3, 25, 80, //
            4, 5, 6, //
            2, 225, 10, //
            30, 255, 8,
        ],
    )
    .unwrap()
}

fn uint16_from_uint8_camera_volume(volume: &DenseVolumeU8) -> DenseVolumeU16 {
    DenseVolumeU16::new(
        volume.dataset_id.clone(),
        volume.layer_id.clone(),
        volume.scale_level,
        volume.timepoint,
        volume.shape,
        volume.grid_to_world,
        volume
            .values()
            .iter()
            .map(|value| u16::from(*value))
            .collect(),
    )
    .unwrap()
}
