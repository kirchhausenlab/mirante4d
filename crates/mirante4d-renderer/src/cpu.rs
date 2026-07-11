use mirante4d_data::{DenseVolumeF32, DenseVolumeU16};

use crate::{
    FrameDiagnostics, FrameDiagnosticsF32, MipImageF32, MipImageU16, PixelCoverage, RenderError,
    frame_diagnostics, frame_diagnostics_f32,
};

pub fn render_mip_z(
    volume: &DenseVolumeU16,
) -> Result<(MipImageU16, FrameDiagnostics), RenderError> {
    if volume.values().is_empty() {
        return Err(RenderError::EmptyVolume);
    }

    let width = volume.shape.x;
    let height = volume.shape.y;
    let mut pixels = vec![0u16; (width * height) as usize];
    let mut coverage = vec![false; (width * height) as usize];

    for z in 0..volume.shape.z {
        for y in 0..volume.shape.y {
            for x in 0..volume.shape.x {
                let pixel_index = (y * width + x) as usize;
                if let Some(value) = volume.render_voxel(z, y, x) {
                    pixels[pixel_index] = pixels[pixel_index].max(value);
                    coverage[pixel_index] = true;
                }
            }
        }
    }

    let diagnostics = frame_diagnostics(volume.render_valid_voxel_count(), &pixels);
    Ok((
        MipImageU16::try_new(
            width,
            height,
            pixels,
            PixelCoverage::from_bool_mask(coverage),
        )?,
        diagnostics,
    ))
}

pub fn render_mip_z_f32(
    volume: &DenseVolumeF32,
) -> Result<(MipImageF32, FrameDiagnosticsF32), RenderError> {
    if volume.values().is_empty() {
        return Err(RenderError::EmptyVolume);
    }

    let width = volume.shape.x;
    let height = volume.shape.y;
    let mut pixels = vec![0.0f32; (width * height) as usize];
    let mut coverage = vec![false; (width * height) as usize];

    for z in 0..volume.shape.z {
        for y in 0..volume.shape.y {
            for x in 0..volume.shape.x {
                let pixel_index = (y * width + x) as usize;
                if let Some(value) = volume.render_voxel(z, y, x) {
                    if coverage[pixel_index] {
                        pixels[pixel_index] = pixels[pixel_index].max(value);
                    } else {
                        pixels[pixel_index] = value;
                    }
                    coverage[pixel_index] = true;
                }
            }
        }
    }

    let diagnostics = frame_diagnostics_f32(volume.render_valid_voxel_count(), &pixels);
    Ok((
        MipImageF32::try_new(
            width,
            height,
            pixels,
            PixelCoverage::from_bool_mask(coverage),
        )?,
        diagnostics,
    ))
}

#[cfg(test)]
mod tests {
    use mirante4d_core::{DatasetId, GridToWorld, LayerId, Shape3D, TimeIndex};
    use mirante4d_data::DatasetHandle;
    use mirante4d_format::{
        ChannelMetadata, DenseF32Layer, ExistingPackagePolicy, FixtureKind, NativeF32Dataset,
        default_f32_display, expected_fixture_value, write_fixture, write_native_f32_dataset,
    };

    use super::*;

    #[test]
    fn renders_known_z_mip_from_basic_fixture() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
        let dataset = DatasetHandle::open(&root).unwrap();
        let layer_id = dataset.first_layer_id().unwrap();
        let volume = dataset.read_u16_volume(&layer_id, TimeIndex(0)).unwrap();

        let (image, diagnostics) = render_mip_z(&volume).unwrap();

        assert_eq!(image.width, 16);
        assert_eq!(image.height, 16);
        assert_eq!(image.pixel(0, 0), Some(expected_fixture_value(0, 15, 0, 0)));
        assert_eq!(image.pixel(7, 3), Some(expected_fixture_value(0, 15, 7, 3)));
        assert_eq!(diagnostics.input_voxels, 4096);
        assert_eq!(diagnostics.output_pixels, 256);
        assert_eq!(diagnostics.nonzero_pixels, 256);
        assert_eq!(diagnostics.max_value, expected_fixture_value(0, 15, 15, 15));
    }

    #[test]
    fn z_mip_ignores_render_invalid_u16_samples() {
        let volume = DenseVolumeU16::new(
            DatasetId::new("masked-mip").unwrap(),
            LayerId::new("ch0").unwrap(),
            0,
            TimeIndex(0),
            Shape3D::new(2, 1, 1).unwrap(),
            GridToWorld::identity(),
            vec![u16::MAX, 12],
        )
        .unwrap()
        .with_render_valid(vec![0, 1])
        .unwrap();

        let (image, diagnostics) = render_mip_z(&volume).unwrap();

        assert_eq!(image.pixel(0, 0), Some(12));
        assert_eq!(image.covered_pixel(0, 0), Some(true));
        assert_eq!(diagnostics.input_voxels, 1);
        assert_eq!(diagnostics.max_value, 12);
    }

    #[test]
    fn z_mip_valid_zero_u16_pixel_is_covered() {
        let volume = DenseVolumeU16::new(
            DatasetId::new("zero-mip").unwrap(),
            LayerId::new("ch0").unwrap(),
            0,
            TimeIndex(0),
            Shape3D::new(1, 1, 1).unwrap(),
            GridToWorld::identity(),
            vec![0],
        )
        .unwrap();

        let (image, diagnostics) = render_mip_z(&volume).unwrap();

        assert_eq!(image.pixel(0, 0), Some(0));
        assert_eq!(image.covered_pixel(0, 0), Some(true));
        assert_eq!(diagnostics.input_voxels, 1);
    }

    #[test]
    fn z_mip_all_invalid_u16_pixel_is_uncovered() {
        let volume = DenseVolumeU16::new(
            DatasetId::new("invalid-mip").unwrap(),
            LayerId::new("ch0").unwrap(),
            0,
            TimeIndex(0),
            Shape3D::new(1, 1, 1).unwrap(),
            GridToWorld::identity(),
            vec![255],
        )
        .unwrap()
        .with_render_valid(vec![0])
        .unwrap();

        let (image, diagnostics) = render_mip_z(&volume).unwrap();

        assert_eq!(image.pixel(0, 0), Some(0));
        assert_eq!(image.covered_pixel(0, 0), Some(false));
        assert_eq!(diagnostics.input_voxels, 0);
    }

    #[test]
    fn z_mip_all_invalid_f32_pixels_resolve_to_zero() {
        let volume = DenseVolumeF32::new(
            DatasetId::new("masked-f32-mip").unwrap(),
            LayerId::new("ch0").unwrap(),
            0,
            TimeIndex(0),
            Shape3D::new(2, 1, 1).unwrap(),
            GridToWorld::identity(),
            vec![255.0, -2.0],
        )
        .unwrap()
        .with_render_valid(vec![0, 0])
        .unwrap();

        let (image, diagnostics) = render_mip_z_f32(&volume).unwrap();

        assert_eq!(image.pixel(0, 0), Some(0.0));
        assert_eq!(image.covered_pixel(0, 0), Some(false));
        assert_eq!(diagnostics.input_voxels, 0);
        assert_eq!(diagnostics.nonzero_pixels, 0);
        assert_eq!(diagnostics.max_value, 0.0);
    }

    #[test]
    fn renders_known_z_mip_from_float32_native_dataset() {
        let tempdir = tempfile::tempdir().unwrap();
        let package = tempdir.path().join("float32-render.m4d");
        let shape = mirante4d_core::Shape4D::new(1, 2, 2, 3).unwrap();
        write_native_f32_dataset(
            &package,
            NativeF32Dataset {
                id: "float32-render".to_owned(),
                name: "Float32 Render".to_owned(),
                world_space: mirante4d_core::WorldSpace {
                    name: "sample".to_owned(),
                    unit: mirante4d_core::WorldUnit::Micrometer,
                },
                layers: vec![DenseF32Layer {
                    id: "ch0".to_owned(),
                    name: "Channel 0".to_owned(),
                    channel: ChannelMetadata {
                        index: 0,
                        color_rgba: [1.0, 1.0, 1.0, 1.0],
                    },
                    shape,
                    brick_shape: mirante4d_core::Shape4D::new(1, 1, 2, 3).unwrap(),
                    grid_to_world: mirante4d_core::GridToWorld::scale_um(1.0, 1.0, 1.0),
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
        let volume = dataset.read_f32_volume(&layer_id, TimeIndex(0)).unwrap();

        let (image, diagnostics) = render_mip_z_f32(&volume).unwrap();

        assert_eq!(image.width, 3);
        assert_eq!(image.height, 2);
        assert_eq!(image.pixels(), &[-2.0, 2.25, 1.5, 4.0, -0.5, 8.0]);
        assert_eq!(diagnostics.input_voxels, 12);
        assert_eq!(diagnostics.output_pixels, 6);
        assert_eq!(diagnostics.nonzero_pixels, 6);
        assert_eq!(diagnostics.max_value, 8.0);
    }
}
