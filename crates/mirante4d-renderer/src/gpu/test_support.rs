use std::path::{Path, PathBuf};

use glam::DVec3;
use mirante4d_data::{DenseVolumeF32, DenseVolumeU16, VolumeRegion};
use mirante4d_domain::{DisplayWindow, Projection, Shape4D, TransferCurve};
use mirante4d_format::{
    ChannelMetadata, CurrentGridToWorldExt, DenseF32Layer, DenseU16Layer, ExistingPackagePolicy,
    NativeF32Dataset, NativeU16Dataset, WorldSpace, WorldUnit, default_f32_display,
    default_u16_display, write_native_f32_dataset, write_native_u16_dataset,
};
use mirante4d_render_api::CameraFrame;

use crate::{
    CoordinateSpace, DvrRgbaFrame, OcclusionPolicy, ScalarDisplayTransfer, SceneColorRgba,
    SceneGeometry, SceneLayer, SceneLayerId, SceneLayerKind, SceneObject, SceneObjectId,
    SceneStyle, SceneTime,
};

use super::{GpuIntensitySummaryF32, GpuIntensitySummaryU16};

pub(super) fn iso_u16_threshold(threshold: u16) -> crate::IsoSurfaceParameters {
    crate::IsoSurfaceParameters::new(
        f32::from(threshold) / f32::from(u16::MAX),
        ScalarDisplayTransfer::identity_u16(),
    )
}

pub(super) fn iso_f32_threshold(
    threshold: f32,
    low: f32,
    high: f32,
) -> crate::IsoSurfaceParameters {
    crate::IsoSurfaceParameters::new(
        ((threshold - low) / (high - low)).clamp(0.0, 1.0),
        ScalarDisplayTransfer::new(
            DisplayWindow::new(low, high).unwrap(),
            TransferCurve::linear(),
            false,
        ),
    )
}

pub(super) fn dvr_parameters(
    low: f32,
    high: f32,
    density_scale: f64,
    invert: bool,
) -> crate::DvrRenderParameters {
    let transfer = ScalarDisplayTransfer::new(
        DisplayWindow::new(low, high).unwrap(),
        TransferCurve::linear(),
        invert,
    );
    crate::DvrRenderParameters::new(transfer, transfer, [1.0, 1.0, 1.0, 1.0], 1.0, density_scale)
}

fn orthographic_camera_state(
    eye: DVec3,
    target: DVec3,
    up: DVec3,
    orthographic_height_world: f64,
) -> CameraFrame {
    crate::current_camera::frame_from_look_at(
        Projection::Orthographic,
        eye,
        target,
        up,
        1.0,
        orthographic_height_world / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan()),
        crate::current_camera::presentation(orthographic_height_world, orthographic_height_world),
    )
}

pub(super) fn front_orthographic_camera(
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
    orthographic_camera_state(
        volume.grid_to_world.transform_point_vec(DVec3::new(
            center_x,
            center_y,
            center_z - depth * 1.5,
        )),
        volume
            .grid_to_world
            .transform_point_vec(DVec3::new(center_x, center_y, center_z)),
        volume.grid_to_world.transform_vector(-DVec3::Y).normalize(),
        height,
    )
}

pub(super) fn front_orthographic_camera_f32(
    volume: &mirante4d_data::DenseVolumeF32,
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
    orthographic_camera_state(
        volume.grid_to_world.transform_point_vec(DVec3::new(
            center_x,
            center_y,
            center_z - depth * 1.5,
        )),
        volume
            .grid_to_world
            .transform_point_vec(DVec3::new(center_x, center_y, center_z)),
        volume.grid_to_world.transform_vector(-DVec3::Y).normalize(),
        height,
    )
}

pub(super) fn far_front_orthographic_camera(
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
    orthographic_camera_state(
        volume.grid_to_world.transform_point_vec(DVec3::new(
            center_x,
            center_y,
            center_z - depth * 7.5,
        )),
        volume
            .grid_to_world
            .transform_point_vec(DVec3::new(center_x, center_y, center_z)),
        volume.grid_to_world.transform_vector(-DVec3::Y).normalize(),
        height,
    )
}

pub(super) fn side_orthographic_camera(
    volume: &DenseVolumeU16,
    orthographic_height_grid: f64,
) -> CameraFrame {
    let width = volume.shape.x() as f64;
    let center_x = (volume.shape.x().saturating_sub(1)) as f64 * 0.5;
    let center_y = (volume.shape.y().saturating_sub(1)) as f64 * 0.5;
    let center_z = (volume.shape.z().saturating_sub(1)) as f64 * 0.5;
    let height = volume
        .grid_to_world
        .transform_vector(DVec3::Y * orthographic_height_grid)
        .length();
    orthographic_camera_state(
        volume.grid_to_world.transform_point_vec(DVec3::new(
            center_x + width * 7.5,
            center_y,
            center_z,
        )),
        volume
            .grid_to_world
            .transform_point_vec(DVec3::new(center_x, center_y, center_z)),
        volume.grid_to_world.transform_vector(DVec3::Y).normalize(),
        height,
    )
}

pub(super) fn assert_pixels_abs_diff_le(actual: &[u16], expected: &[u16], max_diff: u16) {
    assert_eq!(actual.len(), expected.len());
    for (index, (&actual, &expected)) in actual.iter().zip(expected).enumerate() {
        let diff = actual.abs_diff(expected);
        assert!(
            diff <= max_diff,
            "pixel {index} differs by {diff}: actual={actual}, expected={expected}, max_diff={max_diff}"
        );
    }
}

pub(super) fn assert_dvr_rgba_abs_diff_le(
    actual: Option<&DvrRgbaFrame>,
    expected: Option<&DvrRgbaFrame>,
    max_diff: f32,
    label: &str,
) {
    let actual = actual.expect("GPU DVR should include an RGBA frame");
    let expected = expected.expect("CPU DVR should include an RGBA frame");
    assert_eq!(
        actual.premultiplied_rgba().len(),
        expected.premultiplied_rgba().len(),
        "{label} DVR RGBA length differs"
    );
    for (index, (actual_rgba, expected_rgba)) in actual
        .premultiplied_rgba()
        .iter()
        .zip(expected.premultiplied_rgba())
        .enumerate()
    {
        assert_eq!(
            actual.is_covered_index(index),
            expected.is_covered_index(index),
            "{label} DVR RGBA coverage differs at pixel {index}"
        );
        for component in 0..4 {
            let diff = (actual_rgba[component] - expected_rgba[component]).abs();
            assert!(
                diff <= max_diff,
                "{label} DVR RGBA pixel {index} component {component} differs by {diff}: actual={}, expected={}, max_diff={max_diff}",
                actual_rgba[component],
                expected_rgba[component]
            );
        }
    }
}

pub(super) fn assert_f32_pixels_abs_diff_le(
    actual: &[f32],
    expected: &[f32],
    max_diff: f32,
    label: &str,
) {
    assert_eq!(actual.len(), expected.len(), "{label} pixel length differs");
    for (index, (&actual, &expected)) in actual.iter().zip(expected).enumerate() {
        let diff = (actual - expected).abs();
        assert!(
            diff <= max_diff,
            "{label} pixel {index} differs by {diff}: actual={actual}, expected={expected}, max_diff={max_diff}"
        );
    }
}

pub(super) fn cpu_region_summary(
    volume: &DenseVolumeU16,
    region: VolumeRegion,
) -> GpuIntensitySummaryU16 {
    let mut voxel_count = 0u64;
    let mut nonzero_count = 0u64;
    let mut min = u16::MAX;
    let mut max = u16::MIN;
    let mut sum = 0u64;
    for z in region.z_start..region.z_start + region.z_size {
        for y in region.y_start..region.y_start + region.y_size {
            for x in region.x_start..region.x_start + region.x_size {
                let value = volume.voxel(z, y, x).unwrap();
                voxel_count += 1;
                if value != 0 {
                    nonzero_count += 1;
                }
                min = min.min(value);
                max = max.max(value);
                sum += u64::from(value);
            }
        }
    }
    if voxel_count == 0 {
        GpuIntensitySummaryU16 {
            voxel_count: 0,
            nonzero_count: 0,
            min: 0,
            max: 0,
            sum: 0,
            mean: 0.0,
            gpu_compute_ns: None,
        }
    } else {
        GpuIntensitySummaryU16 {
            voxel_count,
            nonzero_count,
            min,
            max,
            sum,
            mean: sum as f64 / voxel_count as f64,
            gpu_compute_ns: None,
        }
    }
}

pub(super) fn cpu_f32_region_summary(
    volume: &DenseVolumeF32,
    region: VolumeRegion,
) -> GpuIntensitySummaryF32 {
    let mut voxel_count = 0u64;
    let mut nonzero_count = 0u64;
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let mut sum = 0.0f64;
    for z in region.z_start..region.z_start + region.z_size {
        for y in region.y_start..region.y_start + region.y_size {
            for x in region.x_start..region.x_start + region.x_size {
                let value = volume.voxel(z, y, x).unwrap();
                voxel_count += 1;
                if value != 0.0 {
                    nonzero_count += 1;
                }
                min = min.min(value);
                max = max.max(value);
                sum += f64::from(value);
            }
        }
    }
    if voxel_count == 0 {
        GpuIntensitySummaryF32 {
            voxel_count: 0,
            nonzero_count: 0,
            min: 0.0,
            max: 0.0,
            sum: 0.0,
            mean: 0.0,
            gpu_compute_ns: None,
        }
    } else {
        GpuIntensitySummaryF32 {
            voxel_count,
            nonzero_count,
            min,
            max,
            sum,
            mean: sum / voxel_count as f64,
            gpu_compute_ns: None,
        }
    }
}

pub(super) fn write_three_brick_gpu_fixture(output_root: &Path) -> PathBuf {
    let root = output_root.join("three-brick.m4d");
    write_native_u16_dataset(
        &root,
        NativeU16Dataset {
            id: "three-brick".to_owned(),
            name: "Three brick GPU fixture".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: WorldUnit::Micrometer,
            },
            layers: vec![DenseU16Layer {
                id: "ch0".to_owned(),
                name: "channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape: Shape4D::new(1, 2, 2, 6).unwrap(),
                brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
                display: default_u16_display(),
                values_tzyx: (0..24).collect(),
            }],
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();
    root
}

pub(super) fn write_three_brick_f32_gpu_fixture(output_root: &Path) -> PathBuf {
    let root = output_root.join("three-brick-f32.m4d");
    write_native_f32_dataset(
        &root,
        NativeF32Dataset {
            id: "three-brick-f32".to_owned(),
            name: "Three brick f32 GPU fixture".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: WorldUnit::Micrometer,
            },
            layers: vec![DenseF32Layer {
                id: "ch0".to_owned(),
                name: "channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape: Shape4D::new(1, 2, 2, 6).unwrap(),
                brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
                display: default_f32_display(),
                values_tzyx: (0..24).map(|value| value as f32 * 0.5 - 5.75).collect(),
            }],
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();
    root
}

pub(super) fn scene_test_camera() -> CameraFrame {
    orthographic_camera_state(DVec3::new(0.0, 0.0, 10.0), DVec3::ZERO, DVec3::Y, 10.0)
}

pub(super) fn line_layer(
    layer_id: &str,
    kind: SceneLayerKind,
    color: SceneColorRgba,
    object_id: &str,
    start: DVec3,
    end: DVec3,
) -> SceneLayer {
    SceneLayer::new(SceneLayerId::new(layer_id).unwrap(), kind)
        .with_style(SceneStyle::new(color))
        .with_object(SceneObject::new(
            SceneObjectId::new(object_id).unwrap(),
            CoordinateSpace::World,
            SceneTime::Static,
            OcclusionPolicy::VolumeDepthCued,
            SceneGeometry::LineSegment {
                start,
                end,
                width_px: 4.0,
            },
        ))
}
