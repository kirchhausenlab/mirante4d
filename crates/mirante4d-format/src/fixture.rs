use std::path::Path;

use mirante4d_core::{GridToWorld, Shape4D, WorldSpace, WorldUnit};

use crate::{
    manifest::ChannelMetadata,
    validate::FormatError,
    writer::{
        DenseF32Layer, DenseU16Layer, ExistingPackagePolicy, NativeF32Dataset, NativeU16Dataset,
        default_f32_display, default_u16_display, write_native_f32_dataset,
        write_native_u16_dataset,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureKind {
    BasicU16_16Cube,
    AnisotropicU16_16Cube,
    TimeU16_8Cube3T,
    TimeMultiChannelU16_8Cube3T2C,
    MultiChannelU16_8Cube4C,
    BasicF32_8Cube,
}

impl FixtureKind {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "basic-u16-16cube" => Some(Self::BasicU16_16Cube),
            "anisotropic-u16-16cube" => Some(Self::AnisotropicU16_16Cube),
            "time-u16-8cube-3t" => Some(Self::TimeU16_8Cube3T),
            "time-multichannel-u16-8cube-3t-2c" => Some(Self::TimeMultiChannelU16_8Cube3T2C),
            "multichannel-u16-8cube-4c" => Some(Self::MultiChannelU16_8Cube4C),
            "basic-f32-8cube" => Some(Self::BasicF32_8Cube),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::BasicU16_16Cube => "basic-u16-16cube",
            Self::AnisotropicU16_16Cube => "anisotropic-u16-16cube",
            Self::TimeU16_8Cube3T => "time-u16-8cube-3t",
            Self::TimeMultiChannelU16_8Cube3T2C => "time-multichannel-u16-8cube-3t-2c",
            Self::MultiChannelU16_8Cube4C => "multichannel-u16-8cube-4c",
            Self::BasicF32_8Cube => "basic-f32-8cube",
        }
    }

    fn shape(self) -> Shape4D {
        match self {
            Self::BasicU16_16Cube | Self::AnisotropicU16_16Cube => {
                Shape4D::new(1, 16, 16, 16).unwrap()
            }
            Self::TimeU16_8Cube3T | Self::TimeMultiChannelU16_8Cube3T2C => {
                Shape4D::new(3, 8, 8, 8).unwrap()
            }
            Self::MultiChannelU16_8Cube4C => Shape4D::new(1, 8, 8, 8).unwrap(),
            Self::BasicF32_8Cube => Shape4D::new(1, 8, 8, 8).unwrap(),
        }
    }

    fn brick_shape(self) -> Shape4D {
        match self {
            Self::BasicU16_16Cube | Self::AnisotropicU16_16Cube => self.shape(),
            Self::TimeU16_8Cube3T | Self::TimeMultiChannelU16_8Cube3T2C => {
                Shape4D::new(1, 8, 8, 8).unwrap()
            }
            Self::MultiChannelU16_8Cube4C | Self::BasicF32_8Cube => self.shape(),
        }
    }

    fn grid_to_world(self) -> GridToWorld {
        match self {
            Self::BasicU16_16Cube
            | Self::TimeU16_8Cube3T
            | Self::TimeMultiChannelU16_8Cube3T2C
            | Self::MultiChannelU16_8Cube4C
            | Self::BasicF32_8Cube => GridToWorld::scale_um(0.2, 0.2, 0.2),
            Self::AnisotropicU16_16Cube => GridToWorld::scale_um(0.2, 0.2, 0.5),
        }
    }

    fn dataset_name(self) -> &'static str {
        match self {
            Self::BasicU16_16Cube => "Basic uint16 16 cube fixture",
            Self::AnisotropicU16_16Cube => "Anisotropic uint16 16 cube fixture",
            Self::TimeU16_8Cube3T => "Time uint16 8 cube 3T fixture",
            Self::TimeMultiChannelU16_8Cube3T2C => "Time multichannel uint16 8 cube 3T 2C fixture",
            Self::MultiChannelU16_8Cube4C => "Multichannel uint16 8 cube 4C fixture",
            Self::BasicF32_8Cube => "Basic float32 8 cube fixture",
        }
    }

    fn channel_count(self) -> u32 {
        match self {
            Self::TimeMultiChannelU16_8Cube3T2C => 2,
            Self::MultiChannelU16_8Cube4C => 4,
            Self::BasicU16_16Cube
            | Self::AnisotropicU16_16Cube
            | Self::TimeU16_8Cube3T
            | Self::BasicF32_8Cube => 1,
        }
    }
}

pub fn write_fixture(
    kind: FixtureKind,
    output_root: impl AsRef<Path>,
) -> Result<std::path::PathBuf, FormatError> {
    let package_root = output_root.as_ref().join(format!("{}.m4d", kind.name()));
    let shape = kind.shape();
    let brick_shape = kind.brick_shape();
    if matches!(kind, FixtureKind::BasicF32_8Cube) {
        write_native_f32_dataset(
            &package_root,
            NativeF32Dataset {
                id: format!("fixture-{}", kind.name()),
                name: kind.dataset_name().to_owned(),
                world_space: WorldSpace {
                    name: "sample".to_owned(),
                    unit: WorldUnit::Micrometer,
                },
                layers: vec![DenseF32Layer {
                    id: "ch0".to_owned(),
                    name: "Channel 0".to_owned(),
                    channel: ChannelMetadata {
                        index: 0,
                        color_rgba: channel_color(0),
                    },
                    shape,
                    brick_shape,
                    grid_to_world: kind.grid_to_world(),
                    display: default_f32_display(),
                    values_tzyx: f32_fixture_values(shape),
                }],
            },
            ExistingPackagePolicy::Replace,
        )?;
        return Ok(package_root);
    }

    let layers = (0..kind.channel_count())
        .map(|channel| DenseU16Layer {
            id: format!("ch{channel}"),
            name: format!("Channel {channel}"),
            channel: ChannelMetadata {
                index: channel,
                color_rgba: channel_color(channel),
            },
            shape,
            brick_shape,
            grid_to_world: kind.grid_to_world(),
            display: default_u16_display(),
            values_tzyx: fixture_values(shape, channel),
        })
        .collect();
    write_native_u16_dataset(
        &package_root,
        NativeU16Dataset {
            id: format!("fixture-{}", kind.name()),
            name: kind.dataset_name().to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: WorldUnit::Micrometer,
            },
            layers,
        },
        ExistingPackagePolicy::Replace,
    )?;
    Ok(package_root)
}

pub fn expected_fixture_value(t: u64, z: u64, y: u64, x: u64) -> u16 {
    expected_fixture_value_for_channel(0, t, z, y, x)
}

pub fn expected_fixture_value_for_channel(channel: u32, t: u64, z: u64, y: u64, x: u64) -> u16 {
    (u64::from(channel) * 20_000 + t * 4096 + z * 257 + y * 17 + x) as u16
}

pub fn expected_f32_fixture_value(t: u64, z: u64, y: u64, x: u64) -> f32 {
    let linear = t * 512 + z * 64 + y * 8 + x;
    linear as f32 / 511.0
}

fn fixture_values(shape: Shape4D, channel: u32) -> Vec<u16> {
    let mut values = Vec::with_capacity(shape.element_count().unwrap() as usize);
    for t in 0..shape.t {
        for z in 0..shape.z {
            for y in 0..shape.y {
                for x in 0..shape.x {
                    values.push(expected_fixture_value_for_channel(channel, t, z, y, x));
                }
            }
        }
    }
    values
}

fn f32_fixture_values(shape: Shape4D) -> Vec<f32> {
    let mut values = Vec::with_capacity(shape.element_count().unwrap() as usize);
    for t in 0..shape.t {
        for z in 0..shape.z {
            for y in 0..shape.y {
                for x in 0..shape.x {
                    values.push(expected_f32_fixture_value(t, z, y, x));
                }
            }
        }
    }
    values
}

fn channel_color(channel: u32) -> [f32; 4] {
    match channel {
        0 => [0.0, 1.0, 0.0, 1.0],
        1 => [1.0, 0.0, 1.0, 1.0],
        2 => [1.0, 0.8, 0.0, 1.0],
        3 => [0.0, 0.7, 1.0, 1.0],
        _ => [1.0, 1.0, 1.0, 1.0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        load_and_validate_dataset, load_and_validate_dataset_quick,
        manifest::{FORMAT_ID, ScaleReduction},
        validate::{FormatError, load_manifest, write_manifest},
        writer::{
            DenseU16MultiscaleLayer, DenseU16Scale, NativeU16MultiscaleDataset,
            write_native_u16_multiscale_dataset,
        },
        zarr_io::open_array,
    };

    #[test]
    fn writes_and_validates_basic_fixture() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
        let dataset = load_and_validate_dataset(&root).unwrap();

        assert_eq!(dataset.manifest.format, FORMAT_ID);
        assert_eq!(dataset.manifest.axes, ["t", "z", "y", "x"]);
        assert_eq!(
            dataset.manifest.layers[0].shape,
            Shape4D::new(1, 16, 16, 16).unwrap()
        );
        let scale = &dataset.manifest.layers[0].scales[0];
        assert_eq!(scale.bricks.records.len(), 1);
        assert_eq!(scale.statistics.min, 0.0);
        assert_eq!(scale.statistics.max, 4125.0);
        assert_eq!(scale.statistics.histogram.bin_count, 256);
        assert_eq!(scale.statistics.histogram.bins.iter().sum::<u64>(), 4096);
        assert!(scale.statistics.percentiles.p50 > scale.statistics.min);
        assert!(scale.statistics.percentiles.p99 >= scale.statistics.percentiles.p50);
    }

    #[test]
    fn writes_time_fixture_with_one_brick_per_timepoint() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_fixture(FixtureKind::TimeU16_8Cube3T, tempdir.path()).unwrap();
        let manifest = load_manifest(&root).unwrap();

        let scale = &manifest.layers[0].scales[0];
        assert_eq!(scale.shape, Shape4D::new(3, 8, 8, 8).unwrap());
        assert_eq!(scale.storage.brick_shape, Shape4D::new(1, 8, 8, 8).unwrap());
        assert_eq!(scale.bricks.grid_shape, Shape4D::new(3, 1, 1, 1).unwrap());
        assert_eq!(scale.bricks.records.len(), 3);
    }

    #[test]
    fn writes_multichannel_fixture_as_separate_layers_without_channel_axis() {
        let tempdir = tempfile::tempdir().unwrap();
        let root =
            write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
        let manifest = load_manifest(&root).unwrap();

        assert_eq!(manifest.axes, ["t", "z", "y", "x"]);
        assert_eq!(manifest.layers.len(), 2);
        assert_eq!(manifest.layers[0].id, "ch0");
        assert_eq!(manifest.layers[1].id, "ch1");
        assert_eq!(manifest.layers[0].channel.index, 0);
        assert_eq!(manifest.layers[1].channel.index, 1);
        assert_eq!(
            manifest.layers[0].scales[0].array_path,
            "arrays/intensity/ch0/s0"
        );
        assert_eq!(
            manifest.layers[1].scales[0].array_path,
            "arrays/intensity/ch1/s0"
        );
    }

    #[test]
    fn writes_four_channel_fixture_for_mixed_mode_evidence() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_fixture(FixtureKind::MultiChannelU16_8Cube4C, tempdir.path()).unwrap();
        let manifest = load_manifest(&root).unwrap();

        assert_eq!(manifest.axes, ["t", "z", "y", "x"]);
        assert_eq!(manifest.layers.len(), 4);
        for (index, layer) in manifest.layers.iter().enumerate() {
            assert_eq!(layer.id, format!("ch{index}"));
            assert_eq!(layer.channel.index, index as u32);
            assert_eq!(layer.scales[0].shape, Shape4D::new(1, 8, 8, 8).unwrap());
            assert_eq!(
                layer.scales[0].array_path,
                format!("arrays/intensity/ch{index}/s0")
            );
        }
        load_and_validate_dataset(&root).unwrap();
    }

    #[test]
    fn writes_and_validates_basic_float32_fixture() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_fixture(FixtureKind::BasicF32_8Cube, tempdir.path()).unwrap();
        let manifest = load_manifest(&root).unwrap();

        assert_eq!(manifest.axes, ["t", "z", "y", "x"]);
        assert_eq!(manifest.layers.len(), 1);
        let layer = &manifest.layers[0];
        assert_eq!(layer.dtype.source, mirante4d_core::IntensityDType::Float32);
        assert_eq!(layer.dtype.stored, mirante4d_core::IntensityDType::Float32);
        assert_eq!(layer.scales[0].shape, Shape4D::new(1, 8, 8, 8).unwrap());
        assert_eq!(layer.scales[0].statistics.min, 0.0);
        assert_eq!(layer.scales[0].statistics.max, 1.0);
        assert_eq!(
            layer.scales[0]
                .statistics
                .histogram
                .bins
                .iter()
                .sum::<u64>(),
            512
        );
        load_and_validate_dataset(&root).unwrap();
    }

    #[test]
    fn writes_and_validates_multiscale_dense_layer() {
        let tempdir = tempfile::tempdir().unwrap();
        let package_root = tempdir.path().join("multiscale-format.m4d");
        let s0_shape = Shape4D::new(1, 4, 4, 4).unwrap();
        let s1_shape = Shape4D::new(1, 2, 2, 2).unwrap();
        let s0_grid_to_world = GridToWorld::scale_um(0.2, 0.2, 0.2);
        let s1_grid_to_world = s0_grid_to_world
            .downsampled_integer_centered(2, 2, 2)
            .unwrap();

        write_native_u16_multiscale_dataset(
            &package_root,
            NativeU16MultiscaleDataset {
                id: "multiscale-format-fixture".to_owned(),
                name: "Multiscale format fixture".to_owned(),
                world_space: WorldSpace {
                    name: "sample".to_owned(),
                    unit: WorldUnit::Micrometer,
                },
                layers: vec![DenseU16MultiscaleLayer {
                    id: "ch0".to_owned(),
                    name: "Channel 0".to_owned(),
                    channel: ChannelMetadata {
                        index: 0,
                        color_rgba: [0.0, 1.0, 0.0, 1.0],
                    },
                    shape: s0_shape,
                    grid_to_world: s0_grid_to_world,
                    display: default_u16_display(),
                    scales: vec![
                        DenseU16Scale {
                            level: 0,
                            shape: s0_shape,
                            brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                            grid_to_world: s0_grid_to_world,
                            source_scale: None,
                            reduction: ScaleReduction::Source,
                            values_tzyx: fixture_values(s0_shape, 0),
                        },
                        DenseU16Scale {
                            level: 1,
                            shape: s1_shape,
                            brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                            grid_to_world: s1_grid_to_world,
                            source_scale: Some(0),
                            reduction: ScaleReduction::Mean,
                            values_tzyx: fixture_values(s1_shape, 0),
                        },
                    ],
                }],
            },
            ExistingPackagePolicy::Replace,
        )
        .unwrap();

        let manifest = load_manifest(&package_root).unwrap();
        load_and_validate_dataset(&package_root).unwrap();

        assert_eq!(manifest.layers[0].scales.len(), 2);
        assert_eq!(manifest.layers[0].scales[0].level, 0);
        assert_eq!(
            manifest.layers[0].scales[0].reduction,
            ScaleReduction::Source
        );
        assert_eq!(manifest.layers[0].scales[1].level, 1);
        assert_eq!(
            manifest.layers[0].scales[1].array_path,
            "arrays/intensity/ch0/s1"
        );
        assert_eq!(manifest.layers[0].scales[1].source_scale, Some(0));
        assert_eq!(manifest.layers[0].scales[1].reduction, ScaleReduction::Mean);
        assert_eq!(manifest.layers[0].scales[1].grid_to_world, s1_grid_to_world);
        assert_eq!(
            manifest.layers[0].scales[1].bricks.grid_shape,
            Shape4D::new(1, 1, 1, 1).unwrap()
        );
    }

    #[test]
    fn expected_value_pattern_is_stable() {
        assert_eq!(expected_fixture_value(0, 0, 0, 0), 0);
        assert_eq!(expected_fixture_value(0, 1, 2, 3), 294);
        assert_eq!(expected_fixture_value(2, 7, 7, 7), 10117);
        assert_eq!(expected_fixture_value_for_channel(1, 2, 7, 7, 7), 30117);
        assert_eq!(expected_f32_fixture_value(0, 0, 0, 0), 0.0);
        assert_eq!(expected_f32_fixture_value(0, 7, 7, 7), 1.0);
    }

    #[test]
    fn rejects_invalid_format_string() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
        let mut manifest = load_manifest(&root).unwrap();
        manifest.format = "legacy-web-viewer".to_owned();
        write_manifest(&root, &manifest).unwrap();

        let err = load_and_validate_dataset(&root).unwrap_err();

        assert!(matches!(err, FormatError::UnsupportedFormat(_)));
    }

    #[test]
    fn rejects_payload_checksum_mismatch() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
        let mut manifest = load_manifest(&root).unwrap();
        manifest.layers[0].scales[0].storage.shard_records[0]
            .payload_checksum
            .hex = "0".repeat(64);
        write_manifest(&root, &manifest).unwrap();

        load_and_validate_dataset_quick(&root).unwrap();
        let err = load_and_validate_dataset(&root).unwrap_err();

        assert!(matches!(err, FormatError::PayloadChecksumMismatch { .. }));
    }

    #[test]
    fn records_and_validates_payload_byte_counts() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
        let manifest = load_manifest(&root).unwrap();
        let payload_bytes = manifest.layers[0].scales[0].storage.shard_records[0].payload_bytes;

        assert!(payload_bytes > 0);
        load_and_validate_dataset(&root).unwrap();
    }

    #[test]
    fn rejects_payload_byte_count_mismatch() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
        let mut manifest = load_manifest(&root).unwrap();
        manifest.layers[0].scales[0].storage.shard_records[0].payload_bytes += 1;
        write_manifest(&root, &manifest).unwrap();

        load_and_validate_dataset_quick(&root).unwrap();
        let err = load_and_validate_dataset(&root).unwrap_err();

        assert!(matches!(err, FormatError::PayloadByteCountMismatch { .. }));
    }

    #[test]
    fn rejects_missing_payload_chunk() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
        let array = open_array(&root, "arrays/intensity/ch0/s0").unwrap();
        array.erase_chunk(&[0, 0, 0, 0]).unwrap();

        load_and_validate_dataset_quick(&root).unwrap();
        let err = load_and_validate_dataset(&root).unwrap_err();

        assert!(matches!(err, FormatError::PayloadMissing { .. }));
    }
}
