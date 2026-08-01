use std::{fs, path::Path};

use mirante4d_dataset::{CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError};
use mirante4d_domain::{GridToWorld, IntensityDType, LogicalLayerKey, Shape4D};
use mirante4d_identity::{
    SCIENTIFIC_TILE_SHAPE_TZYX, ScientificDatasetHasher, ScientificLayerDescriptor,
    ScientificLayerHasher, ScientificTemporalCalibration, ScientificTile, Sha256Hasher,
};
use mirante4d_import_pipeline::{
    ImportCancellation, ImportEvent, ImportOptions, ImportStage, NoDataPolicy, SpatialCalibration,
    TiffChannelSource, TiffSource, import_tiff, inspect_tiff,
};
use mirante4d_storage::{
    OmeLevelTransform, PackagePath, PackedIndexCoordinates, ProfileKind,
    SelfConsistentPackageCapability, ShardProfileKind,
};
use tiff::encoder::{TiffEncoder, colortype};

const SENTINEL: u8 = 255;
const WORKING_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct DenseLevel {
    shape_zyx: [usize; 3],
    values: Vec<u8>,
    validity: Vec<u8>,
}

impl DenseLevel {
    fn index(&self, z: usize, y: usize, x: usize) -> usize {
        (z * self.shape_zyx[1] + y) * self.shape_zyx[2] + x
    }

    fn valid_count(&self) -> usize {
        self.validity.iter().map(|value| usize::from(*value)).sum()
    }
}

/// Independent dense reference for the reviewed sentinel policy.
///
/// This deliberately does not call importer normalization, morphology,
/// pyramid, transform, or package-construction helpers. It implements the
/// approved formulas directly over tightly packed Z/Y/X vectors.
fn oracle_base(raw: &[u8], shape_zyx: [usize; 3], sentinel: u8) -> DenseLevel {
    let voxel_count = shape_zyx.into_iter().product::<usize>();
    assert_eq!(raw.len(), voxel_count);
    let two_dimensional = shape_zyx[0] == 1;
    let mut values = vec![0; voxel_count];
    let mut validity = vec![0; voxel_count];

    for z in 0..shape_zyx[0] {
        for y in 0..shape_zyx[1] {
            for x in 0..shape_zyx[2] {
                let valid = neighbors(z, shape_zyx[0], !two_dimensional).all(|neighbor_z| {
                    neighbors(y, shape_zyx[1], true).all(|neighbor_y| {
                        neighbors(x, shape_zyx[2], true).all(|neighbor_x| {
                            raw[(neighbor_z * shape_zyx[1] + neighbor_y) * shape_zyx[2]
                                + neighbor_x]
                                != sentinel
                        })
                    })
                });
                let index = (z * shape_zyx[1] + y) * shape_zyx[2] + x;
                validity[index] = u8::from(valid);
                if valid {
                    values[index] = raw[index];
                }
            }
        }
    }

    DenseLevel {
        shape_zyx,
        values,
        validity,
    }
}

fn oracle_next(parent: &DenseLevel, two_dimensional: bool) -> DenseLevel {
    let reduced = parent.shape_zyx.map(|dimension| dimension > 1);
    let shape_zyx = std::array::from_fn(|axis| {
        if reduced[axis] {
            parent.shape_zyx[axis].div_ceil(2)
        } else {
            parent.shape_zyx[axis]
        }
    });
    let voxel_count = shape_zyx.into_iter().product::<usize>();
    let mut means = vec![0_u8; voxel_count];
    let mut support = vec![0_u8; voxel_count];

    for z in 0..shape_zyx[0] {
        for y in 0..shape_zyx[1] {
            for x in 0..shape_zyx[2] {
                let child = (z * shape_zyx[1] + y) * shape_zyx[2] + x;
                let origin = [
                    if reduced[0] { z * 2 } else { z },
                    if reduced[1] { y * 2 } else { y },
                    if reduced[2] { x * 2 } else { x },
                ];
                let end: [usize; 3] = std::array::from_fn(|axis| {
                    (origin[axis] + usize::from(reduced[axis]) + 1).min(parent.shape_zyx[axis])
                });
                let mut count = 0_u32;
                let mut sum = 0_u32;
                for parent_z in origin[0]..end[0] {
                    for parent_y in origin[1]..end[1] {
                        for parent_x in origin[2]..end[2] {
                            let index = parent.index(parent_z, parent_y, parent_x);
                            if parent.validity[index] == 1 {
                                count += 1;
                                sum += u32::from(parent.values[index]);
                            }
                        }
                    }
                }
                if let Some(mean) = (sum + count / 2).checked_div(count) {
                    support[child] = 1;
                    means[child] = u8::try_from(mean).unwrap();
                }
            }
        }
    }

    let mut values = vec![0; voxel_count];
    let mut validity = vec![0; voxel_count];
    for z in 0..shape_zyx[0] {
        for y in 0..shape_zyx[1] {
            for x in 0..shape_zyx[2] {
                let valid = neighbors(z, shape_zyx[0], !two_dimensional).all(|neighbor_z| {
                    neighbors(y, shape_zyx[1], true).all(|neighbor_y| {
                        neighbors(x, shape_zyx[2], true).all(|neighbor_x| {
                            support[(neighbor_z * shape_zyx[1] + neighbor_y) * shape_zyx[2]
                                + neighbor_x]
                                == 1
                        })
                    })
                });
                let index = (z * shape_zyx[1] + y) * shape_zyx[2] + x;
                validity[index] = u8::from(valid);
                if valid {
                    values[index] = means[index];
                }
            }
        }
    }

    DenseLevel {
        shape_zyx,
        values,
        validity,
    }
}

fn oracle_pyramid(raw: &[u8], shape_zyx: [usize; 3], sentinel: u8) -> Vec<DenseLevel> {
    let two_dimensional = shape_zyx[0] == 1;
    let mut levels = vec![oracle_base(raw, shape_zyx, sentinel)];
    while levels.last().unwrap().shape_zyx.into_iter().max().unwrap() > 64
        || levels
            .last()
            .unwrap()
            .shape_zyx
            .into_iter()
            .product::<usize>()
            > 262_144
    {
        let next = oracle_next(levels.last().unwrap(), two_dimensional);
        levels.push(next);
    }
    levels
}

fn neighbors(
    coordinate: usize,
    dimension: usize,
    include_radius: bool,
) -> impl Iterator<Item = usize> {
    let radius = usize::from(include_radius);
    coordinate.saturating_sub(radius)..=(coordinate + radius).min(dimension - 1)
}

fn oracle_digest(level: &DenseLevel) -> String {
    let mut hasher = Sha256Hasher::new();
    hasher.update(b"mirante4d-sentinel-restoration-test-oracle-1\0");
    for dimension in level.shape_zyx {
        hasher.update(u64::try_from(dimension).unwrap().to_le_bytes());
    }
    for (&value, &valid) in level.values.iter().zip(&level.validity) {
        hasher.update([value, valid]);
    }
    hasher.finalize().to_string()
}

fn oracle_scientific_content_id(level: &DenseLevel, spacing_zyx: [f64; 3]) -> String {
    let shape = Shape4D::new(
        1,
        u64::try_from(level.shape_zyx[0]).unwrap(),
        u64::try_from(level.shape_zyx[1]).unwrap(),
        u64::try_from(level.shape_zyx[2]).unwrap(),
    )
    .unwrap();
    let transform = GridToWorld::scale(spacing_zyx[2], spacing_zyx[1], spacing_zyx[0]).unwrap();
    let descriptor = ScientificLayerDescriptor::new(
        LogicalLayerKey::new(0),
        IntensityDType::Uint8,
        shape,
        ScientificTemporalCalibration::Unknown,
        transform,
    )
    .unwrap();
    let mut layer = ScientificLayerHasher::new(descriptor).unwrap();
    let tile_shape = [
        usize::try_from(SCIENTIFIC_TILE_SHAPE_TZYX[1]).unwrap(),
        usize::try_from(SCIENTIFIC_TILE_SHAPE_TZYX[2]).unwrap(),
        usize::try_from(SCIENTIFIC_TILE_SHAPE_TZYX[3]).unwrap(),
    ];
    for tile_z in (0..level.shape_zyx[0]).step_by(tile_shape[0]) {
        for tile_y in (0..level.shape_zyx[1]).step_by(tile_shape[1]) {
            for tile_x in (0..level.shape_zyx[2]).step_by(tile_shape[2]) {
                let extent = [
                    (level.shape_zyx[0] - tile_z).min(tile_shape[0]),
                    (level.shape_zyx[1] - tile_y).min(tile_shape[1]),
                    (level.shape_zyx[2] - tile_x).min(tile_shape[2]),
                ];
                let voxels = extent.into_iter().product::<usize>();
                let mut values = Vec::with_capacity(voxels);
                let mut validity = vec![0_u8; voxels.div_ceil(8)];
                let mut target = 0_usize;
                for z in tile_z..tile_z + extent[0] {
                    for y in tile_y..tile_y + extent[1] {
                        for x in tile_x..tile_x + extent[2] {
                            let source = level.index(z, y, x);
                            values.push(level.values[source]);
                            if level.validity[source] == 1 {
                                validity[target / 8] |= 1 << (target % 8);
                            }
                            target += 1;
                        }
                    }
                }
                layer
                    .push_tile(ScientificTile::new(
                        [
                            0,
                            u64::try_from(tile_z).unwrap(),
                            u64::try_from(tile_y).unwrap(),
                            u64::try_from(tile_x).unwrap(),
                        ],
                        [
                            1,
                            u64::try_from(extent[0]).unwrap(),
                            u64::try_from(extent[1]).unwrap(),
                            u64::try_from(extent[2]).unwrap(),
                        ],
                        &validity,
                        &values,
                    ))
                    .unwrap();
            }
        }
    }
    let mut dataset = ScientificDatasetHasher::new(1).unwrap();
    dataset.push_layer(layer.finalize().unwrap()).unwrap();
    dataset.finalize().unwrap().to_string()
}

#[test]
fn independent_oracle_freezes_corner_dilation_rounding_and_derived_sentinel_rules() {
    let raw_3d = (0..27)
        .map(|index| if index == 0 { 255 } else { index as u8 })
        .collect::<Vec<_>>();
    let corner_3d = oracle_base(&raw_3d, [3, 3, 3], 255);
    assert_eq!(corner_3d.valid_count(), 19);
    for z in 0..2 {
        for y in 0..2 {
            for x in 0..2 {
                let index = corner_3d.index(z, y, x);
                assert_eq!((corner_3d.values[index], corner_3d.validity[index]), (0, 0));
            }
        }
    }

    let raw_2d = [255, 1, 2, 3, 4, 5, 6, 7, 8];
    let corner_2d = oracle_base(&raw_2d, [1, 3, 3], 255);
    assert_eq!(corner_2d.valid_count(), 5);
    let zero_sentinel = oracle_base(&[0, 1, 2, 3, 4, 5, 6, 7, 8], [1, 3, 3], 0);
    assert_eq!(zero_sentinel, corner_2d);

    let odd = oracle_base(&(1..=15).collect::<Vec<_>>(), [1, 3, 5], 255);
    let odd_reduced = oracle_next(&odd, true);
    assert_eq!(odd_reduced.shape_zyx, [1, 2, 3]);
    assert_eq!(odd_reduced.values, [4, 6, 8, 12, 14, 15]);
    assert!(odd_reduced.validity.iter().all(|valid| *valid == 1));

    // Sentinel equality applies only to source values. A valid mean can equal
    // that sentinel and must remain valid at derived levels.
    let derived_sentinel = oracle_next(&oracle_base(&[6, 8], [1, 1, 2], 7), true);
    assert_eq!(derived_sentinel.values, [7]);
    assert_eq!(derived_sentinel.validity, [1]);

    let mut recursive_raw = vec![17; 9 * 9];
    recursive_raw[0] = 255;
    let base = oracle_base(&recursive_raw, [1, 9, 9], 255);
    let first = oracle_next(&base, true);
    let second = oracle_next(&first, true);
    assert_eq!(first.shape_zyx, [1, 5, 5]);
    assert_eq!(first.valid_count(), 21);
    assert_eq!(second.shape_zyx, [1, 3, 3]);
    assert_eq!(second.valid_count(), 5);
}

#[test]
fn restored_corner_packages_match_the_independent_2d_and_3d_facts() {
    for (name, shape, expected_valid) in [
        ("sentinel-corner-2d", [1, 3, 3], 5),
        ("sentinel-corner-3d", [3, 3, 3], 19),
    ] {
        let mut raw = (0..shape.into_iter().product::<usize>())
            .map(|index| u8::try_from(index).unwrap())
            .collect::<Vec<_>>();
        raw[0] = SENTINEL;
        let expected = oracle_pyramid(&raw, shape, SENTINEL);
        assert_eq!(expected[0].valid_count(), expected_valid);

        let root = tempfile::tempdir().unwrap();
        let source = root.path().join(format!("{name}.tif"));
        write_multipage_u8(&source, shape, &raw);
        let self_consistent = import_sentinel(&source, root.path(), name, SENTINEL);
        assert_package_matches_oracle(&self_consistent, &expected, [2.0, 4.0, 6.0]);
    }
}

#[test]
fn production_packages_support_zero_and_non_255_sentinels_without_reclassifying_means() {
    let cases = [
        (
            "sentinel-zero",
            [1, 3, 3],
            0,
            vec![0, 1, 2, 3, 4, 5, 6, 7, 8],
        ),
        ("sentinel-seven", [1, 257, 3], 7, {
            let mut raw = vec![6; 257 * 3];
            raw[1] = 8;
            raw[3] = 8;
            *raw.last_mut().unwrap() = 7;
            raw
        }),
    ];

    for (name, shape, sentinel, raw) in cases {
        let expected = oracle_pyramid(&raw, shape, sentinel);
        if sentinel == 7 {
            assert_eq!(expected.len(), 4);
            assert_eq!((expected[1].values[0], expected[1].validity[0]), (7, 1));
        }
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join(format!("{name}.tif"));
        write_multipage_u8(&source, shape, &raw);
        let self_consistent = import_sentinel(&source, root.path(), name, sentinel);
        assert_package_matches_oracle(&self_consistent, &expected, [2.0, 4.0, 6.0]);
    }
}

#[test]
fn sentinel_neighborhoods_do_not_cross_timepoints_or_logical_layers() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("sentinel-tc-grid");
    fs::create_dir(&source).unwrap();
    let shape = [3, 3, 3];
    for channel in 0..2 {
        let channel_root = source.join(format!("channel{channel}"));
        fs::create_dir_all(&channel_root).unwrap();
        for timepoint in 0..2 {
            let mut raw = vec![9; 27];
            if channel == 0 && timepoint == 0 {
                raw[13] = SENTINEL;
            }
            write_multipage_u8(
                &channel_root.join(format!("time{timepoint}.tif")),
                shape,
                &raw,
            );
        }
    }

    let source_manifest = TiffSource::new(vec![
        TiffChannelSource::folder_of_3d("channel 1", source.join("channel0")).unwrap(),
        TiffChannelSource::folder_of_3d("channel 2", source.join("channel1")).unwrap(),
    ])
    .unwrap();
    let self_consistent =
        import_sentinel_manifest(source_manifest, root.path(), "sentinel-tc-grid", SENTINEL);
    for channel in 0..2 {
        for timepoint in 0..2 {
            let brick = self_consistent
                .read_brick(
                    PackedIndexCoordinates::new(0, 0, timepoint, channel, 0, 0, 0),
                    || false,
                )
                .unwrap();
            assert!(brick.record().explicit_validity());
            if channel == 0 && timepoint == 0 {
                assert!(brick.record().all_voxels_invalid());
                assert!(brick.pixel_payload().is_none());
            } else {
                assert!(brick.record().all_voxels_valid());
                assert_eq!(brick.pixel_payload().unwrap()[0], 9);
            }
        }
    }
}

#[test]
fn guarded_sentinel_resume_matches_a_fresh_import() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("sentinel-resume.tif");
    let shape: [usize; 3] = [1, 17, 257];
    let raw = (0..shape[1])
        .flat_map(|y| {
            (0..shape[2]).map(move |x| {
                if (x + 3 * y).is_multiple_of(97) {
                    SENTINEL
                } else {
                    u8::try_from((11 * x + 7 * y) % 255).unwrap()
                }
            })
        })
        .collect::<Vec<_>>();
    write_multipage_u8(&source, shape, &raw);
    let inspection = inspect_tiff(TiffSource::single_3d(&source)).unwrap();
    let destination = root.path().join("sentinel-resumed.m4d");
    let checkpoint = root.path().join("sentinel-resumed.checkpoint");
    let options = ImportOptions {
        inspection: inspection.clone(),
        destination: destination.clone(),
        checkpoint_directory: checkpoint.clone(),
        profile: ProfileKind::Current,
        calibration: SpatialCalibration::new([2.0, 4.0, 6.0]),
        time_step_seconds: None,
        no_data: Some(NoDataPolicy::manual_uint8(SENTINEL)),
    };
    let cancellation = ImportCancellation::new();
    let from_progress = cancellation.clone();
    let error = import_tiff(options.clone(), &TestLedger, &cancellation, move |event| {
        if matches!(
            event,
            ImportEvent::StageProgress {
                stage: ImportStage::BaseProduction,
                completed_work_units: 1,
                ..
            }
        ) {
            from_progress.cancel();
        }
    })
    .unwrap_err();
    assert!(matches!(
        error,
        mirante4d_import_pipeline::ImportError::Cancelled
    ));
    assert!(!destination.exists());
    assert!(
        checkpoint
            .join(".mirante4d-import-control")
            .join("stage-header")
            .is_file()
    );
    assert!(!checkpoint.join("payload").exists());

    let resumed = import_tiff(options, &TestLedger, &ImportCancellation::new(), |_| {}).unwrap();
    let fresh = import_tiff(
        ImportOptions {
            inspection,
            destination: root.path().join("sentinel-fresh.m4d"),
            checkpoint_directory: root.path().join("sentinel-fresh.checkpoint"),
            profile: ProfileKind::Current,
            calibration: SpatialCalibration::new([2.0, 4.0, 6.0]),
            time_step_seconds: None,
            no_data: Some(NoDataPolicy::manual_uint8(SENTINEL)),
        },
        &TestLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        resumed.receipt().scientific_content_id,
        fresh.receipt().scientific_content_id
    );
    assert_eq!(resumed.receipt().package_id, fresh.receipt().package_id);
}

#[test]
fn restored_2d_package_matches_oracle_at_every_lod_and_chunk_seam() {
    const SHAPE: [usize; 3] = [1, 1_025, 1_025];
    const EXPECTED_DIGESTS: [&str; 6] = [
        "92ca12df47cfe1308fcfb94868e8e95650a28f878b08f8f72187b2bb69595c2a",
        "300cbd68dc41d0779f1ec6f50aaad96b01addee64d1175488bd584e0454d58ae",
        "9aa80d765ff31b903ea6143575430c0ab0e6250ca15873bddad131fa605a08b3",
        "eaecab00d480d137a00ee92dca399a6d1e71ba3067ce7c8910196495fddd4920",
        "092cba51d0e68b72e06bb087229cb521814ab5d3f00ea97695a1589940a7a250",
        "9be02d22d929a5c31ac3db72cb6c2d2086166cc6090c4d91d504d9bdcabf2d40",
    ];

    let raw = boundary_fixture_2d(SHAPE);
    let expected = oracle_pyramid(&raw, SHAPE, SENTINEL);
    assert_eq!(expected.len(), EXPECTED_DIGESTS.len());
    assert_eq!(
        expected.iter().map(oracle_digest).collect::<Vec<_>>(),
        EXPECTED_DIGESTS
    );
    let bright = expected[0].index(0, 700, 700);
    assert_eq!(
        (expected[0].values[bright], expected[0].validity[bright]),
        (254, 1)
    );

    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("sentinel-boundary-2d.tif");
    write_multipage_u8(&source, SHAPE, &raw);
    let self_consistent = import_sentinel(&source, root.path(), "sentinel-boundary-2d", SENTINEL);
    assert_package_matches_oracle(&self_consistent, &expected, [2.0, 4.0, 6.0]);
}

#[test]
fn restored_3d_package_matches_oracle_across_z_and_x_chunk_faces() {
    const SHAPE: [usize; 3] = [65, 17, 257];
    const EXPECTED_DIGESTS: [&str; 4] = [
        "e34e7c5b4cc869e1a6f563579d8814987cd1f15033a4f0bc5b744dcf2ea16225",
        "79df180e2a35b340971d3f7175b961ecc6dc27242a877506bc9231f6a4ad9e63",
        "99e4ee71eab3338e3881cb67a8ecc1a4af3df1989e10f235a91a4f72c72c982a",
        "60dc2c0867ee60a46a4b685dcb7011341bc0d4fc5e0e31ab3695cf37fb2f2efb",
    ];

    let raw = boundary_fixture_3d(SHAPE);
    let expected = oracle_pyramid(&raw, SHAPE, SENTINEL);
    assert_eq!(expected.len(), EXPECTED_DIGESTS.len());
    assert_eq!(
        expected.iter().map(oracle_digest).collect::<Vec<_>>(),
        EXPECTED_DIGESTS
    );
    let bright = expected[0].index(10, 8, 200);
    assert_eq!(
        (expected[0].values[bright], expected[0].validity[bright]),
        (254, 1)
    );

    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("sentinel-boundary-3d.tif");
    write_multipage_u8(&source, SHAPE, &raw);
    let self_consistent = import_sentinel(&source, root.path(), "sentinel-boundary-3d", SENTINEL);
    assert_package_matches_oracle(&self_consistent, &expected, [2.0, 4.0, 6.0]);
}

fn boundary_fixture_2d(shape_zyx: [usize; 3]) -> Vec<u8> {
    let invalid = |y: usize, x: usize| {
        let exterior = y < 18
            || y >= shape_zyx[1] - 19
            || x < 23 + (y / 11) % 9
            || x >= shape_zyx[2] - 21 - (y / 13) % 7;
        let interrupted_vertical = x == 256 && (80..940).contains(&y) && !y.is_multiple_of(17);
        let interrupted_horizontal = y == 512 && (160..880).contains(&x) && !x.is_multiple_of(19);
        let diagonal = (180..760).contains(&y) && x == 310 + y / 2 && !y.is_multiple_of(23);
        let hole = x.abs_diff(760).pow(2) + y.abs_diff(280).pow(2) <= 36;
        let thin_island = (8..=9).contains(&x) && (220..=231).contains(&y);
        (exterior || interrupted_vertical || interrupted_horizontal || diagonal || hole)
            && !thin_island
    };

    let mut raw = vec![0; shape_zyx.into_iter().product()];
    for y in 0..shape_zyx[1] {
        for x in 0..shape_zyx[2] {
            let index = y * shape_zyx[2] + x;
            if invalid(y, x) {
                raw[index] = SENTINEL;
                continue;
            }
            let boundary = neighbors(y, shape_zyx[1], true).any(|neighbor_y| {
                neighbors(x, shape_zyx[2], true).any(|neighbor_x| invalid(neighbor_y, neighbor_x))
            });
            raw[index] = if boundary {
                if (x + 3 * y).is_multiple_of(5) {
                    253
                } else {
                    254
                }
            } else {
                u8::try_from(1 + (7 * x + 11 * y + x * y % 43) % 239).unwrap()
            };
        }
    }
    raw[700 * shape_zyx[2] + 700] = 254;
    raw[701 * shape_zyx[2] + 702] = 253;
    raw
}

fn boundary_fixture_3d(shape_zyx: [usize; 3]) -> Vec<u8> {
    let invalid = |z: usize, y: usize, x: usize| {
        let exterior = z == shape_zyx[0] - 1
            || y < 2
            || y >= shape_zyx[1] - 2
            || x < 4 + (z + y) % 4
            || x >= shape_zyx[2] - 3;
        let seam_wall = x == 64 && (3..14).contains(&y) && !(z + 3 * y).is_multiple_of(11);
        let interrupted_wall = x == 128 && (8..57).contains(&z) && !(z + y).is_multiple_of(13);
        let diagonal = (12..54).contains(&z) && x == 170 + z && !y.is_multiple_of(4);
        let hole = x.abs_diff(220).pow(2) + y.abs_diff(8).pow(2) + z.abs_diff(31).pow(2) <= 16;
        exterior || seam_wall || interrupted_wall || diagonal || hole
    };

    let mut raw = vec![0; shape_zyx.into_iter().product()];
    for z in 0..shape_zyx[0] {
        for y in 0..shape_zyx[1] {
            for x in 0..shape_zyx[2] {
                let index = (z * shape_zyx[1] + y) * shape_zyx[2] + x;
                if invalid(z, y, x) {
                    raw[index] = SENTINEL;
                    continue;
                }
                let boundary = neighbors(z, shape_zyx[0], true).any(|neighbor_z| {
                    neighbors(y, shape_zyx[1], true).any(|neighbor_y| {
                        neighbors(x, shape_zyx[2], true)
                            .any(|neighbor_x| invalid(neighbor_z, neighbor_y, neighbor_x))
                    })
                });
                raw[index] = if boundary {
                    if (x + 3 * y + 5 * z).is_multiple_of(7) {
                        253
                    } else {
                        254
                    }
                } else {
                    u8::try_from(1 + (5 * x + 7 * y + 11 * z + x * z % 47) % 239).unwrap()
                };
            }
        }
    }
    raw[(10 * shape_zyx[1] + 8) * shape_zyx[2] + 200] = 254;
    raw[(11 * shape_zyx[1] + 9) * shape_zyx[2] + 201] = 253;
    raw
}

fn write_multipage_u8(path: &Path, shape_zyx: [usize; 3], raw: &[u8]) {
    let file = fs::File::create(path).unwrap();
    let mut encoder = TiffEncoder::new(file).unwrap();
    let plane_voxels = shape_zyx[1] * shape_zyx[2];
    for plane in raw.chunks_exact(plane_voxels) {
        encoder
            .write_image::<colortype::Gray8>(
                u32::try_from(shape_zyx[2]).unwrap(),
                u32::try_from(shape_zyx[1]).unwrap(),
                plane,
            )
            .unwrap();
    }
}

fn import_sentinel(
    source: &Path,
    root: &Path,
    name: &str,
    sentinel: u8,
) -> SelfConsistentPackageCapability {
    import_sentinel_manifest(TiffSource::single_3d(source), root, name, sentinel)
}

fn import_sentinel_manifest(
    source: TiffSource,
    root: &Path,
    name: &str,
    sentinel: u8,
) -> SelfConsistentPackageCapability {
    let inspection = inspect_tiff(source).unwrap();
    let published = import_tiff(
        ImportOptions {
            inspection,
            destination: root.join(format!("{name}.m4d")),
            checkpoint_directory: root.join(format!("{name}.checkpoint")),
            profile: ProfileKind::Current,
            calibration: SpatialCalibration::new([2.0, 4.0, 6.0]),
            time_step_seconds: None,
            no_data: Some(NoDataPolicy::manual_uint8(sentinel)),
        },
        &TestLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();
    let (_, transfer) = published.into_parts();
    transfer.consume(|| false).unwrap().0
}

fn assert_package_matches_oracle(
    self_consistent: &SelfConsistentPackageCapability,
    expected: &[DenseLevel],
    spacing_zyx: [f64; 3],
) {
    assert_eq!(
        self_consistent.scientific_content_id().to_string(),
        oracle_scientific_content_id(&expected[0], spacing_zyx),
        "the scientific content address must cover the oracle's dilated base values and validity"
    );
    let image = &self_consistent.catalog().profile().images()[0];
    assert_eq!(image.levels().len(), expected.len());
    let transforms = self_consistent
        .catalog()
        .ome_image(&PackagePath::parse("images/i00000000/zarr.json").unwrap())
        .unwrap()
        .level_transforms();
    assert_eq!(transforms.len(), expected.len());

    let mut factors = [1_u64; 3];
    for (scale, level) in expected.iter().enumerate() {
        if scale > 0 {
            for (axis, factor) in factors.iter_mut().enumerate() {
                if expected[scale - 1].shape_zyx[axis] > 1 {
                    *factor *= 2;
                }
            }
        }
        let OmeLevelTransform::DiagonalMicrometer {
            scale_zyx,
            translation_zyx,
        } = transforms[scale]
        else {
            panic!("sentinel import with physical spacing requires a diagonal transform");
        };
        for axis in 0..3 {
            assert_eq!(
                scale_zyx[axis].value(),
                spacing_zyx[axis] * factors[axis] as f64
            );
            assert_eq!(
                translation_zyx[axis].value(),
                spacing_zyx[axis] * (factors[axis] - 1) as f64 / 2.0
            );
        }
        let actual = read_dense_level(self_consistent, u32::try_from(scale).unwrap());
        assert_eq!(&actual, level, "sentinel output differs at LOD {scale}");
        assert!(
            actual
                .values
                .iter()
                .zip(&actual.validity)
                .all(|(value, valid)| *valid == 1 || *value == 0),
            "every final invalid integer value must be canonical zero"
        );
    }
}

fn read_dense_level(self_consistent: &SelfConsistentPackageCapability, scale: u32) -> DenseLevel {
    let image = &self_consistent.catalog().profile().images()[0];
    let profile_level = &image.levels()[usize::try_from(scale).unwrap()];
    let pixel_metadata_path =
        PackagePath::parse(&format!("{}/zarr.json", profile_level.pixel_path())).unwrap();
    let array = self_consistent
        .catalog()
        .zarr_array(&pixel_metadata_path)
        .unwrap();
    assert_eq!(array.shape()[0..2], [1, 1]);
    let shape_zyx = [
        usize::try_from(array.shape()[2]).unwrap(),
        usize::try_from(array.shape()[3]).unwrap(),
        usize::try_from(array.shape()[4]).unwrap(),
    ];
    let brick_shape = match array.kind() {
        ShardProfileKind::Pixel2dUint8 => [1_usize, 256, 256],
        ShardProfileKind::Pixel3dUint8 => [64_usize; 3],
        other => panic!("sentinel oracle expected a uint8 pixel array, got {other:?}"),
    };
    let grid: [usize; 3] = std::array::from_fn(|axis| shape_zyx[axis].div_ceil(brick_shape[axis]));
    let voxel_count = shape_zyx.into_iter().product::<usize>();
    let mut values = vec![0; voxel_count];
    let mut validity = vec![0; voxel_count];

    for brick_z in 0..grid[0] {
        for brick_y in 0..grid[1] {
            for brick_x in 0..grid[2] {
                let brick = self_consistent
                    .read_brick(
                        PackedIndexCoordinates::new(
                            0,
                            scale,
                            0,
                            0,
                            u32::try_from(brick_z).unwrap(),
                            u32::try_from(brick_y).unwrap(),
                            u32::try_from(brick_x).unwrap(),
                        ),
                        || false,
                    )
                    .unwrap();
                assert!(brick.record().explicit_validity());
                let extent = brick
                    .logical_extent_zyx()
                    .map(|value| usize::try_from(value).unwrap());
                for local_z in 0..extent[0] {
                    for local_y in 0..extent[1] {
                        for local_x in 0..extent[2] {
                            let padded =
                                (local_z * brick_shape[1] + local_y) * brick_shape[2] + local_x;
                            let target_z = brick_z * brick_shape[0] + local_z;
                            let target_y = brick_y * brick_shape[1] + local_y;
                            let target_x = brick_x * brick_shape[2] + local_x;
                            let target =
                                (target_z * shape_zyx[1] + target_y) * shape_zyx[2] + target_x;
                            values[target] =
                                brick.pixel_payload().map_or(0, |payload| payload[padded]);
                            validity[target] = u8::from(if brick.record().all_voxels_valid() {
                                true
                            } else if brick.record().all_voxels_invalid() {
                                false
                            } else {
                                let bits = brick.validity_payload().unwrap();
                                bits[padded / 8] & (1 << (padded % 8)) != 0
                            });
                        }
                    }
                }
            }
        }
    }

    DenseLevel {
        shape_zyx,
        values,
        validity,
    }
}

struct TestLease {
    bytes: u64,
}

impl CpuByteLease for TestLease {
    fn category(&self) -> CpuLedgerCategory {
        CpuLedgerCategory::ImportWorkingSet
    }

    fn reserved_bytes(&self) -> u64 {
        self.bytes
    }
}

struct TestLedger;

impl CpuByteLedger for TestLedger {
    fn try_acquire(
        &self,
        category: CpuLedgerCategory,
        bytes: u64,
    ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
        assert_eq!(category, CpuLedgerCategory::ImportWorkingSet);
        assert!(bytes > 0);
        assert!(bytes <= WORKING_MEMORY_BYTES);
        Ok(Box::new(TestLease { bytes }))
    }
}
