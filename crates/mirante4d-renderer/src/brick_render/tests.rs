use glam::DVec3;
use mirante4d_data::{
    DatasetHandle, DenseVolumeF32, DenseVolumeU8, DenseVolumeU16, SpatialBrickIndex,
    VolumeBrickF32, VolumeBrickU8, VolumeBrickU16, VolumeRegion,
};
use mirante4d_domain::{DisplayWindow, GridToWorld, Projection, Shape4D, TimeIndex, TransferCurve};
use mirante4d_format::{
    BrickIndex, ChannelMetadata, CurrentGridToWorldExt, DatasetId, DenseF32Layer, DenseU16Layer,
    ExistingPackagePolicy, NativeF32Dataset, NativeU16Dataset, WorldSpace, WorldUnit,
    default_f32_display, default_u16_display, expected_fixture_value, write_native_f32_dataset,
    write_native_u16_dataset,
};
use mirante4d_render_api::CameraFrame;

use crate::{CameraRenderMode, CameraRenderModeF32, render_camera, render_camera_f32};

use super::*;

fn iso_u16_threshold(threshold: u16) -> crate::IsoSurfaceParameters {
    crate::IsoSurfaceParameters::new(
        f32::from(threshold) / f32::from(u16::MAX),
        crate::ScalarDisplayTransfer::identity_u16(),
    )
}

fn iso_f32_threshold(threshold: f32, low: f32, high: f32) -> crate::IsoSurfaceParameters {
    crate::IsoSurfaceParameters::new(
        ((threshold - low) / (high - low)).clamp(0.0, 1.0),
        crate::ScalarDisplayTransfer::new(
            DisplayWindow::new(low, high).unwrap(),
            TransferCurve::linear(),
            false,
        ),
    )
}

fn dvr_parameters(low: f32, high: f32, density_scale: f64, invert: bool) -> DvrRenderParameters {
    let transfer = crate::ScalarDisplayTransfer::new(
        DisplayWindow::new(low, high).unwrap(),
        TransferCurve::linear(),
        invert,
    );
    DvrRenderParameters::new(transfer, transfer, [1.0, 1.0, 1.0, 1.0], 1.0, density_scale)
}

#[test]
fn complete_resident_bricks_match_dense_camera_mip() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let dense = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
    let resident = resident_bricks(&dataset, &layer_id, true);
    let camera = front_camera();
    let viewport = RenderViewport::new(4, 4).unwrap();

    let (brick_frame, brick_diagnostics) =
        render_camera_mip_from_bricks(&resident, camera, viewport).unwrap();
    let (dense_frame, dense_diagnostics) =
        render_camera(&dense, camera, viewport, CameraRenderMode::Mip).unwrap();

    assert_eq!(brick_frame.pixels(), dense_frame.pixels());
    assert_eq!(brick_diagnostics.frame, dense_diagnostics);
    assert!(brick_diagnostics.complete);
    assert_eq!(brick_diagnostics.missing_voxel_samples, 0);
}

#[test]
fn mip_from_complete_resident_bricks_returns_exact_ray_maxima() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let resident = resident_bricks(&dataset, &layer_id, true);
    let camera = front_camera();
    let viewport = RenderViewport::new(4, 4).unwrap();

    let (frame, diagnostics) = render_camera_mip_from_bricks(&resident, camera, viewport).unwrap();
    let expected = (0..4)
        .flat_map(|y| (0..4).map(move |x| expected_fixture_value(0, 3, y, x)))
        .collect::<Vec<_>>();

    assert_eq!(frame.pixels(), expected.as_slice());
    assert!(diagnostics.complete);
    assert_eq!(diagnostics.missing_voxel_samples, 0);
    assert_eq!(
        diagnostics.frame.max_value,
        expected_fixture_value(0, 3, 3, 3)
    );
}

#[test]
fn complete_resident_bricks_match_dense_camera_iso_and_dvr() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let dense = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
    let resident = resident_bricks(&dataset, &layer_id, true);
    let camera = front_camera();
    let viewport = RenderViewport::new(4, 4).unwrap();

    for mode in [
        CameraRenderMode::Isosurface {
            parameters: iso_u16_threshold(200),
        },
        CameraRenderMode::Dvr {
            parameters: dvr_parameters(0.0, f32::from(u16::MAX), 12.0, false),
        },
    ] {
        let (brick_frame, brick_diagnostics) =
            render_camera_from_bricks(&resident, camera, viewport, mode).unwrap();
        let (dense_frame, dense_diagnostics) =
            render_camera(&dense, camera, viewport, mode).unwrap();

        assert_eq!(brick_frame.pixels(), dense_frame.pixels());
        if matches!(mode, CameraRenderMode::Isosurface { .. }) {
            let brick_surface = brick_frame
                .iso_surface()
                .expect("resident ISO should include a surface frame");
            let dense_surface = dense_frame
                .iso_surface()
                .expect("dense ISO should include a surface frame");
            assert_eq!(brick_surface.source_values(), dense_surface.source_values());
            assert_eq!(
                brick_surface.display_scalars(),
                dense_surface.display_scalars()
            );
            assert_eq!(
                brick_surface.material_scalars(),
                dense_surface.material_scalars()
            );
            assert_eq!(brick_surface.hit_depth(), dense_surface.hit_depth());
            assert_eq!(brick_surface.normals(), dense_surface.normals());
            assert_eq!(
                brick_surface.diffuse_lighting(),
                dense_surface.diffuse_lighting()
            );
            assert_eq!(
                brick_surface.specular_lighting(),
                dense_surface.specular_lighting()
            );
        }
        assert_eq!(brick_diagnostics.frame, dense_diagnostics);
        assert!(brick_diagnostics.complete);
        assert_eq!(brick_diagnostics.missing_voxel_samples, 0);
    }
}

#[test]
fn render_invalid_resident_u16_samples_are_transparent_not_incomplete() {
    let resident = resident_from_u16_mask(
        Shape3D::new(2, 1, 1).unwrap(),
        |z, _y, _x| if z == 1 { u16::MAX } else { 12 },
        |z, _y, _x| z == 0,
    );
    let camera = single_pixel_front_camera();
    let viewport = RenderViewport::new(1, 1).unwrap();

    let (mip, mip_diagnostics) =
        render_camera_from_bricks(&resident, camera, viewport, CameraRenderMode::Mip).unwrap();
    let (iso, iso_diagnostics) = render_camera_from_bricks(
        &resident,
        camera,
        viewport,
        CameraRenderMode::Isosurface {
            parameters: iso_u16_threshold(u16::MAX),
        },
    )
    .unwrap();
    let dvr_resident = resident_from_u16_mask(
        Shape3D::new(2, 1, 1).unwrap(),
        |z, _y, _x| if z == 1 { u16::MAX } else { 0 },
        |z, _y, _x| z == 0,
    );
    let (dvr, dvr_diagnostics) = render_camera_from_bricks(
        &dvr_resident,
        camera,
        viewport,
        CameraRenderMode::Dvr {
            parameters: dvr_parameters(0.0, f32::from(u16::MAX), 12.0, false),
        },
    )
    .unwrap();

    assert_eq!(mip.pixels(), &[12]);
    assert_eq!(mip.covered_pixel(0, 0), Some(true));
    assert_eq!(iso.pixels(), &[0]);
    assert_eq!(iso.covered_pixel(0, 0), Some(false));
    assert_eq!(dvr.pixels(), &[0]);
    assert_eq!(dvr.covered_pixel(0, 0), Some(false));
    for diagnostics in [mip_diagnostics, iso_diagnostics, dvr_diagnostics] {
        assert!(diagnostics.complete);
        assert_eq!(diagnostics.missing_voxel_samples, 0);
    }
}

#[test]
fn complete_resident_u8_bricks_match_equivalent_u16_camera_modes() {
    let shape = Shape3D::new(3, 2, 2).unwrap();
    let value_at = |z, y, x| u8::try_from(z * 16 + y * 4 + x + 1).unwrap();
    let resident_u8 = resident_u8_from_fn(shape, value_at);
    let resident_u16 = resident_from_fn(shape, |z, y, x| u16::from(value_at(z, y, x)));
    let camera = front_camera();
    let viewport = RenderViewport::new(2, 2).unwrap();

    for mode in [
        CameraRenderMode::Mip,
        CameraRenderMode::Isosurface {
            parameters: iso_u16_threshold(18),
        },
        CameraRenderMode::Dvr {
            parameters: dvr_parameters(0.0, f32::from(u8::MAX), 12.0, false),
        },
    ] {
        let (u8_frame, u8_diagnostics) = render_camera_u8_from_bricks_with_quality(
            &resident_u8,
            camera,
            viewport,
            mode,
            CameraRenderQuality::voxel_exact(),
        )
        .unwrap();
        let (u16_frame, u16_diagnostics) =
            render_camera_from_bricks(&resident_u16, camera, viewport, mode).unwrap();

        assert_eq!(u8_frame.pixels(), u16_frame.pixels());
        assert_eq!(u8_frame.coverage(), u16_frame.coverage());
        assert_eq!(u8_diagnostics.frame, u16_diagnostics.frame);
        assert!(u8_diagnostics.complete);
        assert_eq!(u8_diagnostics.missing_voxel_samples, 0);
        if matches!(mode, CameraRenderMode::Isosurface { .. }) {
            assert_eq!(
                u8_frame.iso_surface().unwrap().source_values(),
                u16_frame.iso_surface().unwrap().source_values()
            );
        }
    }
}

#[test]
fn resident_u8_max_value_is_valid_data_not_invalid_sentinel() {
    let resident = resident_from_u8_mask(
        Shape3D::new(2, 1, 1).unwrap(),
        |z, _y, _x| if z == 1 { u8::MAX } else { 7 },
        |_z, _y, _x| true,
    );
    let camera = single_pixel_front_camera();
    let viewport = RenderViewport::new(1, 1).unwrap();

    let (frame, diagnostics) = render_camera_u8_from_bricks_with_quality(
        &resident,
        camera,
        viewport,
        CameraRenderMode::Mip,
        CameraRenderQuality::voxel_exact(),
    )
    .unwrap();

    assert_eq!(frame.pixels(), &[u16::from(u8::MAX)]);
    assert_eq!(frame.covered_pixel(0, 0), Some(true));
    assert!(diagnostics.complete);
    assert_eq!(diagnostics.missing_voxel_samples, 0);
}

#[test]
fn same_ray_resident_dvr_channels_are_order_independent() {
    let shape = Shape3D::new(1, 1, 1).unwrap();
    let red_resident = resident_from_fn(shape, |_z, _y, _x| 100);
    let green_resident = resident_from_fn(shape, |_z, _y, _x| 100);
    let transfer = crate::ScalarDisplayTransfer::new(
        DisplayWindow::new(0.0, 100.0).unwrap(),
        TransferCurve::linear(),
        false,
    );
    let red = DvrRenderParameters::new(transfer, transfer, [1.0, 0.0, 0.0, 1.0], 1.0, 1.0);
    let green = DvrRenderParameters::new(transfer, transfer, [0.0, 1.0, 0.0, 1.0], 1.0, 1.0);
    let viewport = RenderViewport::new(1, 1).unwrap();
    let camera = single_pixel_front_camera();

    let (red_green, red_green_diagnostics) = render_dvr_channels_from_bricks_with_quality(
        &[
            DvrResidentChannel::u16(&red_resident, red),
            DvrResidentChannel::u16(&green_resident, green),
        ],
        camera,
        viewport,
        CameraRenderQuality::voxel_exact(),
    )
    .unwrap();
    let (green_red, green_red_diagnostics) = render_dvr_channels_from_bricks_with_quality(
        &[
            DvrResidentChannel::u16(&green_resident, green),
            DvrResidentChannel::u16(&red_resident, red),
        ],
        camera,
        viewport,
        CameraRenderQuality::voxel_exact(),
    )
    .unwrap();

    assert!(red_green_diagnostics.complete);
    assert!(green_red_diagnostics.complete);
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
fn isosurface_returns_first_threshold_hit_before_larger_deeper_voxel() {
    let resident = resident_from_fn(Shape3D::new(4, 4, 4).unwrap(), |z, _y, _x| match z {
        0 => 0,
        1 => 500,
        2 => 900,
        _ => 10,
    });
    let camera = front_camera();
    let viewport = RenderViewport::new(4, 4).unwrap();

    let (frame, diagnostics) = render_camera_from_bricks(
        &resident,
        camera,
        viewport,
        CameraRenderMode::Isosurface {
            parameters: iso_u16_threshold(400),
        },
    )
    .unwrap();

    assert!(diagnostics.complete);
    assert_eq!(diagnostics.missing_voxel_samples, 0);
    assert!(frame.pixels().iter().all(|pixel| *pixel == 500));
    assert_eq!(diagnostics.frame.max_value, 500);
}

#[test]
fn dvr_opacity_scale_controls_accumulation_monotonically() {
    let resident = resident_from_fn(Shape3D::new(4, 4, 4).unwrap(), |z, _y, _x| match z {
        3 => 48_000,
        2 => 36_000,
        1 => 24_000,
        _ => 12_000,
    });
    let camera = front_camera();
    let viewport = RenderViewport::new(4, 4).unwrap();

    let (zero, zero_diagnostics) = render_camera_from_bricks(
        &resident,
        camera,
        viewport,
        CameraRenderMode::Dvr {
            parameters: dvr_parameters(0.0, f32::from(u16::MAX), 0.0, false),
        },
    )
    .unwrap();
    let (low, low_diagnostics) = render_camera_from_bricks(
        &resident,
        camera,
        viewport,
        CameraRenderMode::Dvr {
            parameters: dvr_parameters(0.0, f32::from(u16::MAX), 1.0, false),
        },
    )
    .unwrap();
    let (high, high_diagnostics) = render_camera_from_bricks(
        &resident,
        camera,
        viewport,
        CameraRenderMode::Dvr {
            parameters: dvr_parameters(0.0, f32::from(u16::MAX), 12.0, false),
        },
    )
    .unwrap();

    assert!(zero_diagnostics.complete);
    assert!(low_diagnostics.complete);
    assert!(high_diagnostics.complete);
    assert!(zero.pixels().iter().all(|pixel| *pixel == 0));
    for ((zero, low), high) in zero.pixels().iter().zip(low.pixels()).zip(high.pixels()) {
        assert!(*zero < *low, "low-opacity DVR should exceed zero opacity");
        assert!(
            *low <= *high,
            "higher opacity scale should not reduce accumulated DVR intensity"
        );
    }
}

#[test]
fn dvr_early_termination_ignores_deeper_voxels_after_saturation() {
    let resident = resident_from_fn(Shape3D::new(4, 4, 4).unwrap(), |z, _y, _x| match z {
        0 => u16::MAX,
        1 => 40_000,
        2 => 30_000,
        _ => 20_000,
    });
    let camera = front_camera();
    let viewport = RenderViewport::new(4, 4).unwrap();

    let (frame, diagnostics) = render_camera_from_bricks(
        &resident,
        camera,
        viewport,
        CameraRenderMode::Dvr {
            parameters: dvr_parameters(0.0, f32::from(u16::MAX), 12.0, false),
        },
    )
    .unwrap();

    assert!(diagnostics.complete);
    assert_eq!(diagnostics.missing_voxel_samples, 0);
    assert!(frame.pixels().iter().all(|pixel| *pixel == u16::MAX));
    assert_eq!(diagnostics.frame.max_value, u16::MAX);
}

#[test]
fn resident_dvr_range_skip_uses_transfer_mapped_brick_interval() {
    let mut resident = resident_from_fn(Shape3D::new(1, 1, 1).unwrap(), |_z, _y, _x| 100);
    resident.bricks[0].min = 0.0;
    resident.bricks[0].max = 0.0;
    let camera = single_pixel_front_camera();
    let viewport = RenderViewport::new(1, 1).unwrap();

    let (frame, diagnostics) = render_camera_from_bricks(
        &resident,
        camera,
        viewport,
        CameraRenderMode::Dvr {
            parameters: dvr_parameters(10.0, 20.0, 12.0, false),
        },
    )
    .unwrap();

    assert!(diagnostics.complete);
    assert_eq!(diagnostics.missing_voxel_samples, 0);
    assert_eq!(frame.pixels(), &[0]);
    assert!(!frame.is_covered_index(0));
    assert_eq!(
        frame.dvr_rgba().unwrap().premultiplied_rgba()[0],
        [0.0, 0.0, 0.0, 0.0]
    );
}

#[test]
fn resident_dvr_range_skip_preserves_inverted_valid_zero_contribution() {
    let resident = resident_from_fn(Shape3D::new(1, 1, 1).unwrap(), |_z, _y, _x| 0);
    let camera = single_pixel_front_camera();
    let viewport = RenderViewport::new(1, 1).unwrap();

    let (normal, normal_diagnostics) = render_camera_from_bricks(
        &resident,
        camera,
        viewport,
        CameraRenderMode::Dvr {
            parameters: dvr_parameters(0.0, 100.0, 12.0, false),
        },
    )
    .unwrap();
    let (inverted, inverted_diagnostics) = render_camera_from_bricks(
        &resident,
        camera,
        viewport,
        CameraRenderMode::Dvr {
            parameters: dvr_parameters(0.0, 100.0, 12.0, true),
        },
    )
    .unwrap();

    assert!(normal_diagnostics.complete);
    assert!(inverted_diagnostics.complete);
    assert_eq!(normal_diagnostics.missing_voxel_samples, 0);
    assert_eq!(inverted_diagnostics.missing_voxel_samples, 0);
    assert!(!normal.is_covered_index(0));
    assert_eq!(normal.pixels(), &[0]);
    assert!(inverted.is_covered_index(0));
    assert!(inverted.pixels()[0] > 0);
    let rgba = inverted.dvr_rgba().unwrap().premultiplied_rgba()[0];
    assert!(rgba[0] > 0.0 && rgba[1] > 0.0 && rgba[2] > 0.0 && rgba[3] > 0.0);
}

#[test]
fn dvr_opacity_uses_physical_step_length_for_anisotropic_spacing() {
    let shape = Shape3D::new(1, 1, 1).unwrap();
    let value_at = |_z, _y, _x| 100;
    let identity_resident = resident_from_fn(shape, value_at);
    let anisotropic_grid = mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 4.0);
    let anisotropic_resident = resident_from_fn_with_grid(shape, anisotropic_grid, value_at);
    let identity_camera = single_pixel_front_camera();
    let anisotropic_height = anisotropic_grid.transform_vector(DVec3::Y).length();
    let anisotropic_camera = crate::current_camera::frame_from_look_at(
        Projection::Orthographic,
        anisotropic_grid.transform_point_vec(DVec3::new(0.0, 0.0, -3.0)),
        anisotropic_grid.transform_point_vec(DVec3::new(0.0, 0.0, 0.5)),
        anisotropic_grid.transform_vector(-DVec3::Y).normalize(),
        1.0,
        anisotropic_height / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan()),
        crate::current_camera::presentation(anisotropic_height, anisotropic_height),
    );
    let viewport = RenderViewport::new(1, 1).unwrap();
    let mode = CameraRenderMode::Dvr {
        parameters: dvr_parameters(0.0, 100.0, 1.0, false),
    };

    let (identity, identity_diagnostics) =
        render_camera_from_bricks(&identity_resident, identity_camera, viewport, mode).unwrap();
    let (anisotropic, anisotropic_diagnostics) =
        render_camera_from_bricks(&anisotropic_resident, anisotropic_camera, viewport, mode)
            .unwrap();

    assert!(identity_diagnostics.complete);
    assert!(anisotropic_diagnostics.complete);
    assert_eq!(identity_diagnostics.missing_voxel_samples, 0);
    assert_eq!(anisotropic_diagnostics.missing_voxel_samples, 0);
    assert!(
        anisotropic.pixels()[0] > identity.pixels()[0],
        "a physically thicker voxel should accumulate more opacity"
    );
    assert!(
        anisotropic.dvr_rgba().unwrap().premultiplied_rgba()[0][3]
            > identity.dvr_rgba().unwrap().premultiplied_rgba()[0][3]
    );
}

#[test]
fn dvr_opacity_is_stable_for_equivalent_downsampled_physical_thickness() {
    let fine_shape = Shape3D::new(2, 1, 1).unwrap();
    let coarse_shape = Shape3D::new(1, 1, 1).unwrap();
    let fine_grid = GridToWorld::identity();
    let coarse_grid = fine_grid.downsampled_integer_centered(1, 1, 2).unwrap();
    let fine = resident_from_fn_with_grid(fine_shape, fine_grid, |_z, _y, _x| 100);
    let coarse = resident_from_fn_with_grid(coarse_shape, coarse_grid, |_z, _y, _x| 100);
    let camera = crate::current_camera::frame_from_look_at(
        Projection::Orthographic,
        DVec3::new(0.0, 0.0, -3.0),
        DVec3::new(0.0, 0.0, 0.5),
        -DVec3::Y,
        1.0,
        1.0 / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan()),
        crate::current_camera::presentation(1.0, 1.0),
    );
    let viewport = RenderViewport::new(1, 1).unwrap();
    let mode = CameraRenderMode::Dvr {
        parameters: dvr_parameters(0.0, 100.0, 0.5, false),
    };

    let (fine_frame, fine_diagnostics) =
        render_camera_from_bricks(&fine, camera, viewport, mode).unwrap();
    let (coarse_frame, coarse_diagnostics) =
        render_camera_from_bricks(&coarse, camera, viewport, mode).unwrap();

    assert!(fine_diagnostics.complete);
    assert!(coarse_diagnostics.complete);
    assert_eq!(fine_diagnostics.missing_voxel_samples, 0);
    assert_eq!(coarse_diagnostics.missing_voxel_samples, 0);
    assert!(
        fine_frame.pixels()[0].abs_diff(coarse_frame.pixels()[0]) <= 1,
        "equivalent physical path lengths should keep DVR alpha stable across LOD"
    );
    let fine_rgba = fine_frame.dvr_rgba().unwrap().premultiplied_rgba()[0];
    let coarse_rgba = coarse_frame.dvr_rgba().unwrap().premultiplied_rgba()[0];
    for component in 0..4 {
        assert!(
            (fine_rgba[component] - coarse_rgba[component]).abs() <= 1.0e-6,
            "component {component}: fine {}, coarse {}",
            fine_rgba[component],
            coarse_rgba[component]
        );
    }
}

#[test]
fn float32_dvr_display_window_maps_transfer_before_opacity() {
    let resident = resident_f32_from_fn(Shape3D::new(4, 4, 4).unwrap(), |z, _y, _x| match z {
        0 => -100.0,
        1 => 0.0,
        2 => 5.0,
        _ => 20.0,
    });
    let camera = front_camera();
    let viewport = RenderViewport::new(4, 4).unwrap();

    let (frame, diagnostics) = render_camera_f32_from_bricks(
        &resident,
        camera,
        viewport,
        CameraRenderModeF32::Dvr {
            parameters: dvr_parameters(0.0, 10.0, 12.0, false),
        },
    )
    .unwrap();

    assert!(diagnostics.complete);
    assert_eq!(diagnostics.missing_voxel_samples, 0);
    assert!(
        frame
            .pixels()
            .iter()
            .all(|pixel| (0.0..=1.0).contains(pixel))
    );
    let expected_alpha = (1.0 - f64::exp(-6.0)) as f32;
    assert!(
        frame
            .pixels()
            .iter()
            .all(|pixel| (*pixel - expected_alpha).abs() <= 1.0e-6),
        "the first mapped nonzero sample should drive opacity without using the raw source value"
    );
    let dvr_rgba = frame
        .dvr_rgba()
        .expect("f32 DVR render should include an RGBA frame");
    assert!(dvr_rgba.premultiplied_rgba().iter().all(|rgba| {
        (rgba[0] - expected_alpha * 0.5).abs() <= 1.0e-6
            && (rgba[1] - expected_alpha * 0.5).abs() <= 1.0e-6
            && (rgba[2] - expected_alpha * 0.5).abs() <= 1.0e-6
            && (rgba[3] - expected_alpha).abs() <= 1.0e-6
    }));
    assert!((diagnostics.frame.max_value - expected_alpha).abs() <= 1.0e-6);
}

#[test]
fn resident_f32_dvr_range_skip_uses_transfer_mapped_brick_interval() {
    let mut resident = resident_f32_from_fn(Shape3D::new(1, 1, 1).unwrap(), |_z, _y, _x| 100.0);
    resident.bricks[0].min = 0.0;
    resident.bricks[0].max = 0.0;
    let camera = single_pixel_front_camera();
    let viewport = RenderViewport::new(1, 1).unwrap();

    let (frame, diagnostics) = render_camera_f32_from_bricks(
        &resident,
        camera,
        viewport,
        CameraRenderModeF32::Dvr {
            parameters: dvr_parameters(10.0, 20.0, 12.0, false),
        },
    )
    .unwrap();

    assert!(diagnostics.complete);
    assert_eq!(diagnostics.missing_voxel_samples, 0);
    assert_eq!(frame.pixels(), &[0.0]);
    assert!(!frame.is_covered_index(0));
    assert_eq!(
        frame.dvr_rgba().unwrap().premultiplied_rgba()[0],
        [0.0, 0.0, 0.0, 0.0]
    );
}

#[test]
fn missing_resident_bricks_report_incomplete_frame() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let resident = resident_bricks(&dataset, &layer_id, false);
    let camera = front_camera();
    let viewport = RenderViewport::new(4, 4).unwrap();

    let (_frame, diagnostics) = render_camera_mip_from_bricks(&resident, camera, viewport).unwrap();

    assert!(!diagnostics.complete);
    assert!(diagnostics.missing_voxel_samples > 0);
}

#[test]
fn missing_resident_bricks_report_incomplete_iso_without_fake_surface() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let resident = resident_bricks(&dataset, &layer_id, false);
    let camera = front_camera();
    let viewport = RenderViewport::new(4, 4).unwrap();

    let (frame, diagnostics) = render_camera_from_bricks(
        &resident,
        camera,
        viewport,
        CameraRenderMode::Isosurface {
            parameters: iso_u16_threshold(u16::MAX),
        },
    )
    .unwrap();

    assert!(!diagnostics.complete);
    assert!(diagnostics.missing_voxel_samples > 0);
    assert!(frame.pixels().iter().all(|pixel| *pixel == 0));
}

#[test]
fn missing_resident_bricks_report_incomplete_dvr() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let resident = resident_bricks(&dataset, &layer_id, false);
    let camera = front_camera();
    let viewport = RenderViewport::new(4, 4).unwrap();

    let (_frame, diagnostics) = render_camera_from_bricks(
        &resident,
        camera,
        viewport,
        CameraRenderMode::Dvr {
            parameters: dvr_parameters(0.0, f32::from(u16::MAX), 12.0, false),
        },
    )
    .unwrap();

    assert!(!diagnostics.complete);
    assert!(diagnostics.missing_voxel_samples > 0);
}

#[test]
fn resident_u16_voxel_lookup_indexes_edge_bricks_without_dense_scan() {
    let layer_id = LayerId::new("ch0").unwrap();
    let resident = ResidentBrickSetU16::new(
        layer_id.clone(),
        TimeIndex::new(0),
        Shape3D::new(5, 5, 5).unwrap(),
        GridToWorld::identity(),
        vec![u16_test_brick(
            layer_id,
            SpatialBrickIndex::new(1, 1, 1),
            VolumeRegion::new(3, 3, 3, 2, 2, 2).unwrap(),
            900,
        )],
    );

    assert_eq!(resident.voxel(4, 4, 4), Some(907));
    assert_eq!(resident.voxel(2, 4, 4), None);
    assert_eq!(resident.voxel(4, 2, 4), None);
    assert_eq!(resident.voxel(4, 4, 2), None);
}

#[test]
fn resident_u8_voxel_lookup_indexes_edge_bricks_without_dense_scan() {
    let layer_id = LayerId::new("ch0").unwrap();
    let resident = ResidentBrickSetU8::new(
        layer_id.clone(),
        TimeIndex::new(0),
        Shape3D::new(5, 5, 5).unwrap(),
        GridToWorld::identity(),
        vec![u8_test_brick(
            layer_id,
            SpatialBrickIndex::new(1, 1, 1),
            VolumeRegion::new(3, 3, 3, 2, 2, 2).unwrap(),
            90,
        )],
    );

    assert_eq!(resident.voxel(4, 4, 4), Some(97));
    assert_eq!(resident.voxel(2, 4, 4), None);
    assert_eq!(resident.voxel(4, 2, 4), None);
    assert_eq!(resident.voxel(4, 4, 2), None);
}

#[test]
fn resident_f32_voxel_lookup_indexes_edge_bricks_without_dense_scan() {
    let layer_id = LayerId::new("ch0").unwrap();
    let resident = ResidentBrickSetF32::new(
        layer_id.clone(),
        TimeIndex::new(0),
        Shape3D::new(5, 5, 5).unwrap(),
        GridToWorld::identity(),
        vec![f32_test_brick(
            layer_id,
            SpatialBrickIndex::new(1, 1, 1),
            VolumeRegion::new(3, 3, 3, 2, 2, 2).unwrap(),
            9.0,
        )],
    );

    assert_eq!(resident.voxel(4, 4, 4), Some(9.7));
    assert_eq!(resident.voxel(2, 4, 4), None);
    assert_eq!(resident.voxel(4, 2, 4), None);
    assert_eq!(resident.voxel(4, 4, 2), None);
}

#[test]
fn complete_float32_resident_bricks_match_dense_camera_modes() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_float32_spatially_chunked_dataset(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let dense = dataset
        .read_f32_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
    let resident = resident_f32_bricks(&dataset, &layer_id, true);
    let camera = front_camera();
    let viewport = RenderViewport::new(4, 4).unwrap();
    let modes = [
        CameraRenderModeF32::Mip,
        CameraRenderModeF32::Isosurface {
            parameters: iso_f32_threshold(1.25, 0.0, 4.0),
        },
        CameraRenderModeF32::Dvr {
            parameters: dvr_parameters(-3.0, 6.0, 8.0, false),
        },
    ];

    for mode in modes {
        let (brick_frame, brick_diagnostics) =
            render_camera_f32_from_bricks(&resident, camera, viewport, mode).unwrap();
        let (dense_frame, dense_diagnostics) =
            render_camera_f32(&dense, camera, viewport, mode).unwrap();

        assert_f32_pixels_eq(brick_frame.pixels(), dense_frame.pixels());
        assert_eq!(brick_diagnostics.frame, dense_diagnostics);
        assert!(brick_diagnostics.complete);
        assert_eq!(brick_diagnostics.missing_voxel_samples, 0);
    }
}

#[test]
fn missing_float32_resident_bricks_report_incomplete_frame() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_float32_spatially_chunked_dataset(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let resident = resident_f32_bricks(&dataset, &layer_id, false);
    let camera = front_camera();
    let viewport = RenderViewport::new(4, 4).unwrap();

    let (_frame, diagnostics) =
        render_camera_f32_from_bricks(&resident, camera, viewport, CameraRenderModeF32::Mip)
            .unwrap();

    assert!(!diagnostics.complete);
    assert!(diagnostics.missing_voxel_samples > 0);
}

fn resident_bricks(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    include_all: bool,
) -> ResidentBrickSetU16 {
    let mut bricks = Vec::new();
    for z in 0..2 {
        for y in 0..2 {
            for x in 0..2 {
                if !include_all && z == 1 && y == 1 && x == 1 {
                    continue;
                }
                bricks.push(
                    dataset
                        .read_u16_brick(
                            layer_id,
                            TimeIndex::new(0),
                            SpatialBrickIndex::new(z, y, x),
                        )
                        .unwrap(),
                );
            }
        }
    }
    let layer = dataset.layer(layer_id).unwrap();
    ResidentBrickSetU16::new(
        layer_id.clone(),
        TimeIndex::new(0),
        layer.shape.spatial(),
        layer.grid_to_world,
        bricks,
    )
}

fn resident_f32_bricks(
    dataset: &DatasetHandle,
    layer_id: &LayerId,
    include_all: bool,
) -> ResidentBrickSetF32 {
    let mut bricks = Vec::new();
    for z in 0..2 {
        for y in 0..2 {
            for x in 0..2 {
                if !include_all && z == 1 && y == 1 && x == 1 {
                    continue;
                }
                bricks.push(
                    dataset
                        .read_f32_brick(
                            layer_id,
                            TimeIndex::new(0),
                            SpatialBrickIndex::new(z, y, x),
                        )
                        .unwrap(),
                );
            }
        }
    }
    let layer = dataset.layer(layer_id).unwrap();
    ResidentBrickSetF32::new(
        layer_id.clone(),
        TimeIndex::new(0),
        layer.shape.spatial(),
        layer.grid_to_world,
        bricks,
    )
}

fn u16_test_brick(
    layer_id: LayerId,
    brick_index: SpatialBrickIndex,
    region: VolumeRegion,
    base_value: u16,
) -> VolumeBrickU16 {
    let shape = region.shape().unwrap();
    let values = (0..shape.element_count().unwrap())
        .map(|index| base_value + u16::try_from(index).unwrap())
        .collect::<Vec<_>>();
    let volume = DenseVolumeU16::new(
        DatasetId::new("renderer-test").unwrap(),
        layer_id,
        0,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        values,
    )
    .unwrap();
    VolumeBrickU16 {
        scale_level: 0,
        brick_index,
        chunk_index: BrickIndex {
            t: 0,
            z: brick_index.z,
            y: brick_index.y,
            x: brick_index.x,
        },
        region,
        occupied: true,
        valid_voxel_count: shape.element_count().unwrap(),
        min: f64::from(base_value),
        max: f64::from(base_value) + shape.element_count().unwrap() as f64 - 1.0,
        volume,
    }
}

fn u8_test_brick(
    layer_id: LayerId,
    brick_index: SpatialBrickIndex,
    region: VolumeRegion,
    base_value: u8,
) -> VolumeBrickU8 {
    let shape = region.shape().unwrap();
    let values = (0..shape.element_count().unwrap())
        .map(|index| base_value + u8::try_from(index).unwrap())
        .collect::<Vec<_>>();
    let volume = DenseVolumeU8::new(
        DatasetId::new("renderer-test").unwrap(),
        layer_id,
        0,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        values,
    )
    .unwrap();
    VolumeBrickU8 {
        scale_level: 0,
        brick_index,
        chunk_index: BrickIndex {
            t: 0,
            z: brick_index.z,
            y: brick_index.y,
            x: brick_index.x,
        },
        region,
        occupied: true,
        valid_voxel_count: shape.element_count().unwrap(),
        min: f64::from(base_value),
        max: f64::from(base_value) + shape.element_count().unwrap() as f64 - 1.0,
        volume,
    }
}

fn f32_test_brick(
    layer_id: LayerId,
    brick_index: SpatialBrickIndex,
    region: VolumeRegion,
    base_value: f32,
) -> VolumeBrickF32 {
    let shape = region.shape().unwrap();
    let values = (0..shape.element_count().unwrap())
        .map(|index| base_value + index as f32 / 10.0)
        .collect::<Vec<_>>();
    let volume = DenseVolumeF32::new(
        DatasetId::new("renderer-test").unwrap(),
        layer_id,
        0,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        values,
    )
    .unwrap();
    VolumeBrickF32 {
        scale_level: 0,
        brick_index,
        chunk_index: BrickIndex {
            t: 0,
            z: brick_index.z,
            y: brick_index.y,
            x: brick_index.x,
        },
        region,
        occupied: true,
        valid_voxel_count: shape.element_count().unwrap(),
        min: f64::from(base_value),
        max: f64::from(base_value + (shape.element_count().unwrap() - 1) as f32 / 10.0),
        volume,
    }
}

fn resident_from_fn(
    shape: Shape3D,
    value_at: impl Fn(u64, u64, u64) -> u16,
) -> ResidentBrickSetU16 {
    resident_from_fn_with_grid(shape, GridToWorld::identity(), value_at)
}

fn resident_from_u16_mask(
    shape: Shape3D,
    value_at: impl Fn(u64, u64, u64) -> u16,
    is_render_valid: impl Fn(u64, u64, u64) -> bool,
) -> ResidentBrickSetU16 {
    let layer_id = LayerId::new("ch0").unwrap();
    let mut values = Vec::with_capacity(shape.element_count().unwrap() as usize);
    let mut render_valid = Vec::with_capacity(shape.element_count().unwrap() as usize);
    let mut valid_count = 0u64;
    let mut valid_min = u16::MAX;
    let mut valid_max = 0u16;
    for z in 0..shape.z() {
        for y in 0..shape.y() {
            for x in 0..shape.x() {
                let value = value_at(z, y, x);
                let valid = is_render_valid(z, y, x);
                values.push(value);
                render_valid.push(u8::from(valid));
                if valid {
                    valid_count += 1;
                    valid_min = valid_min.min(value);
                    valid_max = valid_max.max(value);
                }
            }
        }
    }
    let volume = DenseVolumeU16::new(
        DatasetId::new("renderer-test").unwrap(),
        layer_id.clone(),
        0,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        values,
    )
    .unwrap()
    .with_render_valid(render_valid)
    .unwrap();
    let region = VolumeRegion::new(0, 0, 0, shape.z(), shape.y(), shape.x()).unwrap();
    ResidentBrickSetU16::new(
        layer_id,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        vec![VolumeBrickU16 {
            scale_level: 0,
            brick_index: SpatialBrickIndex::new(0, 0, 0),
            chunk_index: BrickIndex {
                t: 0,
                z: 0,
                y: 0,
                x: 0,
            },
            region,
            occupied: valid_count > 0,
            valid_voxel_count: valid_count,
            min: if valid_count == 0 {
                0.0
            } else {
                f64::from(valid_min)
            },
            max: if valid_count == 0 {
                0.0
            } else {
                f64::from(valid_max)
            },
            volume,
        }],
    )
}

fn resident_from_u8_mask(
    shape: Shape3D,
    value_at: impl Fn(u64, u64, u64) -> u8,
    is_render_valid: impl Fn(u64, u64, u64) -> bool,
) -> ResidentBrickSetU8 {
    let layer_id = LayerId::new("ch0").unwrap();
    let mut values = Vec::with_capacity(shape.element_count().unwrap() as usize);
    let mut render_valid = Vec::with_capacity(shape.element_count().unwrap() as usize);
    let mut valid_count = 0u64;
    let mut valid_min = u8::MAX;
    let mut valid_max = 0u8;
    for z in 0..shape.z() {
        for y in 0..shape.y() {
            for x in 0..shape.x() {
                let value = value_at(z, y, x);
                let valid = is_render_valid(z, y, x);
                values.push(value);
                render_valid.push(u8::from(valid));
                if valid {
                    valid_count += 1;
                    valid_min = valid_min.min(value);
                    valid_max = valid_max.max(value);
                }
            }
        }
    }
    let volume = DenseVolumeU8::new(
        DatasetId::new("renderer-test").unwrap(),
        layer_id.clone(),
        0,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        values,
    )
    .unwrap()
    .with_render_valid(render_valid)
    .unwrap();
    let region = VolumeRegion::new(0, 0, 0, shape.z(), shape.y(), shape.x()).unwrap();
    ResidentBrickSetU8::new(
        layer_id,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        vec![VolumeBrickU8 {
            scale_level: 0,
            brick_index: SpatialBrickIndex::new(0, 0, 0),
            chunk_index: BrickIndex {
                t: 0,
                z: 0,
                y: 0,
                x: 0,
            },
            region,
            occupied: valid_count > 0,
            valid_voxel_count: valid_count,
            min: if valid_count == 0 {
                0.0
            } else {
                f64::from(valid_min)
            },
            max: if valid_count == 0 {
                0.0
            } else {
                f64::from(valid_max)
            },
            volume,
        }],
    )
}

fn resident_u8_from_fn(
    shape: Shape3D,
    value_at: impl Fn(u64, u64, u64) -> u8,
) -> ResidentBrickSetU8 {
    resident_from_u8_mask(shape, value_at, |_z, _y, _x| true)
}

fn resident_from_fn_with_grid(
    shape: Shape3D,
    grid_to_world: GridToWorld,
    value_at: impl Fn(u64, u64, u64) -> u16,
) -> ResidentBrickSetU16 {
    let layer_id = LayerId::new("ch0").unwrap();
    let values = (0..shape.z())
        .flat_map(|z| (0..shape.y()).flat_map(move |y| (0..shape.x()).map(move |x| (z, y, x))))
        .map(|(z, y, x)| value_at(z, y, x))
        .collect::<Vec<_>>();
    let volume = DenseVolumeU16::new(
        DatasetId::new("renderer-test").unwrap(),
        layer_id.clone(),
        0,
        TimeIndex::new(0),
        shape,
        grid_to_world,
        values,
    )
    .unwrap();
    let region = VolumeRegion::new(0, 0, 0, shape.z(), shape.y(), shape.x()).unwrap();
    ResidentBrickSetU16::new(
        layer_id,
        TimeIndex::new(0),
        shape,
        grid_to_world,
        vec![VolumeBrickU16 {
            scale_level: 0,
            brick_index: SpatialBrickIndex::new(0, 0, 0),
            chunk_index: BrickIndex {
                t: 0,
                z: 0,
                y: 0,
                x: 0,
            },
            region,
            occupied: true,
            valid_voxel_count: shape.element_count().unwrap(),
            min: 0.0,
            max: f64::from(*volume.values().iter().max().unwrap_or(&0)),
            volume,
        }],
    )
}

fn resident_f32_from_fn(
    shape: Shape3D,
    value_at: impl Fn(u64, u64, u64) -> f32,
) -> ResidentBrickSetF32 {
    let layer_id = LayerId::new("ch0").unwrap();
    let values = (0..shape.z())
        .flat_map(|z| (0..shape.y()).flat_map(move |y| (0..shape.x()).map(move |x| (z, y, x))))
        .map(|(z, y, x)| value_at(z, y, x))
        .collect::<Vec<_>>();
    let volume = DenseVolumeF32::new(
        DatasetId::new("renderer-test").unwrap(),
        layer_id.clone(),
        0,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        values,
    )
    .unwrap();
    let region = VolumeRegion::new(0, 0, 0, shape.z(), shape.y(), shape.x()).unwrap();
    let (min, max) = volume
        .values()
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), value| {
            (min.min(*value), max.max(*value))
        });
    ResidentBrickSetF32::new(
        layer_id,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        vec![VolumeBrickF32 {
            scale_level: 0,
            brick_index: SpatialBrickIndex::new(0, 0, 0),
            chunk_index: BrickIndex {
                t: 0,
                z: 0,
                y: 0,
                x: 0,
            },
            region,
            occupied: true,
            valid_voxel_count: shape.element_count().unwrap(),
            min: f64::from(min),
            max: f64::from(max),
            volume,
        }],
    )
}

fn write_spatially_chunked_dataset(output_root: &std::path::Path) -> std::path::PathBuf {
    let package_root = output_root.join("renderer-spatially-chunked.m4d");
    let shape = Shape4D::new(1, 4, 4, 4).unwrap();
    write_native_u16_dataset(
        &package_root,
        NativeU16Dataset {
            id: "renderer-spatially-chunked-fixture".to_owned(),
            name: "Renderer spatially chunked fixture".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: WorldUnit::Micrometer,
            },
            layers: vec![DenseU16Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape,
                brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
                display: default_u16_display(),
                values_tzyx: fixture_values(shape),
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn write_float32_spatially_chunked_dataset(output_root: &std::path::Path) -> std::path::PathBuf {
    let package_root = output_root.join("renderer-f32-spatially-chunked.m4d");
    let shape = Shape4D::new(1, 4, 4, 4).unwrap();
    write_native_f32_dataset(
        &package_root,
        NativeF32Dataset {
            id: "renderer-f32-spatially-chunked-fixture".to_owned(),
            name: "Renderer float32 spatially chunked fixture".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: WorldUnit::Micrometer,
            },
            layers: vec![DenseF32Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape,
                brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
                display: default_f32_display(),
                values_tzyx: fixture_f32_values(shape),
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn fixture_values(shape: Shape4D) -> Vec<u16> {
    let mut values = Vec::with_capacity(shape.element_count().unwrap() as usize);
    for t in 0..shape.t() {
        for z in 0..shape.z() {
            for y in 0..shape.y() {
                for x in 0..shape.x() {
                    values.push(expected_fixture_value(t, z, y, x));
                }
            }
        }
    }
    values
}

fn fixture_f32_values(shape: Shape4D) -> Vec<f32> {
    let mut values = Vec::with_capacity(shape.element_count().unwrap() as usize);
    for t in 0..shape.t() {
        for z in 0..shape.z() {
            for y in 0..shape.y() {
                for x in 0..shape.x() {
                    let signed_z = z as f32 - 1.5;
                    let signed_y = y as f32 - 1.5;
                    let signed_x = x as f32 - 1.5;
                    values.push(
                        t as f32 * 100.0 + signed_z * 2.25 + signed_y * 0.75 + signed_x * 0.5,
                    );
                }
            }
        }
    }
    values
}

fn assert_f32_pixels_eq(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert!(
            (actual - expected).abs() <= 1.0e-6,
            "pixel {index}: actual {actual}, expected {expected}"
        );
    }
}

fn front_camera() -> CameraFrame {
    crate::current_camera::frame_from_look_at(
        Projection::Orthographic,
        DVec3::new(1.5, 1.5, -7.0),
        DVec3::new(1.5, 1.5, 1.5),
        -DVec3::Y,
        1.0,
        4.0 / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan()),
        crate::current_camera::presentation(4.0, 4.0),
    )
}

fn single_pixel_front_camera() -> CameraFrame {
    crate::current_camera::frame_from_look_at(
        Projection::Orthographic,
        DVec3::new(0.0, 0.0, -3.0),
        DVec3::new(0.0, 0.0, 0.5),
        -DVec3::Y,
        1.0,
        1.0 / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan()),
        crate::current_camera::presentation(1.0, 1.0),
    )
}
