use std::{fmt::Write as _, fs::File, path::Path};

use mirante4d_core::{LayerId, Shape3D, TimeIndex};
use mirante4d_data::DatasetHandle;
use mirante4d_format::NativeDatasetProvenanceKind;
use mirante4d_format::validate::load_manifest;
use tiff::encoder::{TiffEncoder, colortype};

use super::*;

fn assert_grid_to_world_close(actual: GridToWorld, expected: GridToWorld) {
    for (index, (actual, expected)) in actual
        .matrix4x4_row_major
        .iter()
        .zip(expected.matrix4x4_row_major.iter())
        .enumerate()
    {
        assert!(
            (actual - expected).abs() <= 1.0e-9,
            "matrix element {index}: actual {actual}, expected {expected}"
        );
    }
}

fn assert_grid_to_world_matrix(actual: GridToWorld, expected: [f64; 16]) {
    for (index, (actual, expected)) in actual
        .matrix4x4_row_major
        .iter()
        .zip(expected.iter())
        .enumerate()
    {
        assert!(
            (actual - expected).abs() <= 1.0e-9,
            "matrix element {index}: actual {actual}, expected {expected}"
        );
    }
}

fn accepted_source_import_options(
    source: TiffImportSource,
    output_package: PathBuf,
    dataset_id: &str,
    dataset_name: &str,
    voxel_spacing_um: [f64; 3],
    file_grouping: Option<Vec<TiffFileGrouping>>,
) -> TiffSourceImportOptions {
    let inspection = match &file_grouping {
        Some(grouping) => inspect_tiff_source_with_grouping(&source, grouping).unwrap(),
        None => inspect_tiff_source(&source).unwrap(),
    };
    TiffSourceImportOptions {
        source,
        output_package,
        dataset_id: dataset_id.to_owned(),
        dataset_name: dataset_name.to_owned(),
        voxel_spacing_um,
        channel_metadata: BTreeMap::new(),
        file_grouping,
        existing_policy: ExistingPackagePolicy::Fail,
        storage: Default::default(),
        reviewed_plan: accepted_tiff_reviewed_import_plan(&inspection, voxel_spacing_um, true),
    }
}

fn accepted_directory_import_options(
    input_dir: PathBuf,
    output_package: PathBuf,
    dataset_id: &str,
    dataset_name: &str,
    voxel_spacing_um: [f64; 3],
    channel_metadata: BTreeMap<u32, TiffChannelMetadataOverride>,
) -> TiffDirectoryImportOptions {
    let inspection = inspect_tiff_directory(&input_dir).unwrap();
    TiffDirectoryImportOptions {
        input_dir,
        output_package,
        dataset_id: dataset_id.to_owned(),
        dataset_name: dataset_name.to_owned(),
        voxel_spacing_um,
        channel_metadata,
        file_grouping: None,
        existing_policy: ExistingPackagePolicy::Fail,
        storage: Default::default(),
        reviewed_plan: accepted_tiff_reviewed_import_plan(&inspection, voxel_spacing_um, true),
    }
}

#[test]
fn imports_uint16_tiff_directory_to_native_dataset() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input");
    fs::create_dir(&input).unwrap();
    for channel in 0..2 {
        for timepoint in 0..2 {
            let path = input.join(format!(
                "sample_ch{channel}_stack{timepoint:04}_0000msec.tif"
            ));
            write_stack(&path, channel, timepoint).unwrap();
        }
    }
    let output = tempdir.path().join("imported.m4d");
    let mut channel_metadata = BTreeMap::new();
    channel_metadata.insert(
        1,
        TiffChannelMetadataOverride {
            name: "DNA".to_owned(),
            color_rgba: [0.25, 0.5, 0.75, 1.0],
        },
    );

    let report = import_tiff_directory(accepted_directory_import_options(
        input,
        output.clone(),
        "import-test",
        "Import Test",
        [0.2, 0.3, 0.5],
        channel_metadata,
    ))
    .unwrap();

    assert_eq!(report.output_package, output);
    assert_eq!(report.channel_count, 2);
    assert_eq!(report.timepoint_count, 2);
    assert_eq!(report.scale_count, 1);
    assert_eq!(report.z_planes, 2);
    assert_eq!(report.width, 3);
    assert_eq!(report.height, 2);

    let manifest = load_manifest(&report.output_package).unwrap();
    assert_eq!(manifest.axes, ["t", "z", "y", "x"]);
    assert_eq!(manifest.layers.len(), 2);
    assert_eq!(manifest.layers[1].dtype.source, IntensityDType::Uint16);
    assert_eq!(manifest.layers[1].dtype.stored, IntensityDType::Uint16);
    assert_eq!(manifest.layers[1].id, "ch1");
    assert_eq!(manifest.layers[1].name, "DNA");
    assert_eq!(
        manifest.layers[1].channel.color_rgba,
        [0.25, 0.5, 0.75, 1.0]
    );
    assert_eq!(manifest.layers[1].shape, Shape4D::new(2, 2, 2, 3).unwrap());
    assert_eq!(manifest.layers[1].display.window.low, 1000.0);
    assert_eq!(manifest.layers[1].display.window.high, 1115.0);
    assert_eq!(manifest.layers[1].scales.len(), 1);
    assert_eq!(
        manifest.layers[1].scales[0].storage.brick_shape,
        Shape4D::new(1, 2, 2, 3).unwrap()
    );
    assert_eq!(manifest.layers[1].scales[0].source_scale, None);
    assert_eq!(
        manifest.layers[1].scales[0].reduction,
        ScaleReduction::Source
    );

    let dataset = DatasetHandle::open(&report.output_package).unwrap();
    let volume = dataset
        .read_u16_volume(&LayerId::new("ch1").unwrap(), TimeIndex(1))
        .unwrap();
    assert_eq!(volume.voxel(1, 1, 2), Some(1115));
}

#[test]
fn source_format_matrix_documents_strict_supported_formats() {
    let matrix = supported_source_format_matrix();

    assert!(matrix.iter().any(|entry| {
        entry.id == SOURCE_FORMAT_OME_TIFF
            && entry.status == SourceFormatSupportStatus::Primary
            && entry
                .metadata_guarantees
                .iter()
                .any(|guarantee| guarantee.contains("OME-XML"))
    }));
    assert!(matrix.iter().any(|entry| {
        entry.id == SOURCE_FORMAT_EXPLICIT_TIFF_STACK
            && entry.status == SourceFormatSupportStatus::ApprovedWithExplicitReview
    }));
    assert!(matrix.iter().any(|entry| {
        entry.id == SOURCE_FORMAT_PLANE_SERIES_TIFF_VOLUME
            && entry.status == SourceFormatSupportStatus::ApprovedWithExplicitReview
            && entry
                .unsupported_variants
                .iter()
                .any(|variant| variant.contains("recursive"))
    }));
    assert!(matrix.iter().any(|entry| {
        entry.id == SOURCE_FORMAT_SYNTHETIC_FIXTURE
            && entry.status == SourceFormatSupportStatus::DeveloperFixtureOnly
    }));
}

#[test]
fn import_rejects_unaccepted_review_plan_before_native_output() {
    let tempdir = tempfile::tempdir().unwrap();
    let input_file = tempdir.path().join("single-source.tif");
    write_stack(&input_file, 0, 0).unwrap();
    let output = tempdir.path().join("single.m4d");

    let err = import_tiff_source(TiffSourceImportOptions {
        source: TiffImportSource::SingleFile(input_file),
        output_package: output.clone(),
        dataset_id: "single".to_owned(),
        dataset_name: "Single".to_owned(),
        voxel_spacing_um: [0.2, 0.3, 0.5],
        channel_metadata: BTreeMap::new(),
        file_grouping: None,
        existing_policy: ExistingPackagePolicy::Fail,
        storage: Default::default(),
        reviewed_plan: TiffReviewedImportPlan::pending(),
    })
    .unwrap_err();

    assert!(matches!(err, ImportError::UnreviewedImportPlan));
    assert!(!output.exists());
    assert!(!temporary_output_package_path(&output).exists());
}

#[test]
fn import_rejects_review_plan_with_tampered_value_range() {
    let tempdir = tempfile::tempdir().unwrap();
    let input_file = tempdir.path().join("single-source.tif");
    write_stack(&input_file, 0, 0).unwrap();
    let output = tempdir.path().join("single.m4d");
    let source = TiffImportSource::SingleFile(input_file);
    let inspection = inspect_tiff_source(&source).unwrap();
    let mut reviewed_plan = accepted_tiff_reviewed_import_plan(&inspection, [0.2, 0.3, 0.5], true);
    reviewed_plan.value_range = Some(TiffValueRangeSummary {
        min: -1.0,
        max: 15.0,
    });

    let err = import_tiff_source(TiffSourceImportOptions {
        source,
        output_package: output.clone(),
        dataset_id: "single".to_owned(),
        dataset_name: "Single".to_owned(),
        voxel_spacing_um: [0.2, 0.3, 0.5],
        channel_metadata: BTreeMap::new(),
        file_grouping: None,
        existing_policy: ExistingPackagePolicy::Fail,
        storage: Default::default(),
        reviewed_plan,
    })
    .unwrap_err();

    assert!(matches!(
        err,
        ImportError::ReviewedValueRangeMismatch { .. }
    ));
    assert!(!output.exists());
}

#[test]
fn import_multiscale_specs_follow_production_storage_policy() {
    let grid_to_world = GridToWorld::scale_um(0.2, 0.3, 0.5);

    let tiny_3d =
        build_mean_multiscale_specs(Shape4D::new(2, 2, 2, 3).unwrap(), grid_to_world).unwrap();
    assert_eq!(tiny_3d.len(), 1);
    assert_eq!(tiny_3d[0].brick_shape, Shape4D::new(1, 2, 2, 3).unwrap());

    let large_3d =
        build_mean_multiscale_specs(Shape4D::new(1, 65, 300, 300).unwrap(), grid_to_world).unwrap();
    assert_eq!(
        large_3d[0].brick_shape,
        Shape4D::new(1, 64, 64, 64).unwrap()
    );
    assert_eq!(large_3d[1].shape, Shape4D::new(1, 33, 150, 150).unwrap());
    assert_eq!(
        large_3d[1].brick_shape,
        Shape4D::new(1, 33, 64, 64).unwrap()
    );
    assert_eq!(large_3d[1].source_scale, Some(0));
    assert_eq!(large_3d[1].reduction, ScaleReduction::Mean);
    assert_grid_to_world_close(
        large_3d[1].grid_to_world,
        grid_to_world.downsampled_integer_centered(2, 2, 2).unwrap(),
    );
    assert_grid_to_world_matrix(
        large_3d[1].grid_to_world,
        [
            0.4, 0.0, 0.0, 0.1, 0.0, 0.6, 0.0, 0.15, 0.0, 0.0, 1.0, 0.25, 0.0, 0.0, 0.0, 1.0,
        ],
    );
    assert_eq!(large_3d[2].shape, Shape4D::new(1, 17, 75, 75).unwrap());
    assert_grid_to_world_matrix(
        large_3d[2].grid_to_world,
        [
            0.8, 0.0, 0.0, 0.3, 0.0, 1.2, 0.0, 0.45, 0.0, 0.0, 2.0, 0.75, 0.0, 0.0, 0.0, 1.0,
        ],
    );

    let large_2d =
        build_mean_multiscale_specs(Shape4D::new(1, 1, 512, 512).unwrap(), grid_to_world).unwrap();
    assert_eq!(large_2d.len(), 2);
    assert_eq!(
        large_2d[0].brick_shape,
        Shape4D::new(1, 1, 256, 256).unwrap()
    );
    assert_eq!(large_2d[1].shape, Shape4D::new(1, 1, 256, 256).unwrap());
    assert_eq!(
        large_2d[1].brick_shape,
        Shape4D::new(1, 1, 256, 256).unwrap()
    );
    assert_grid_to_world_matrix(
        large_2d[1].grid_to_world,
        [
            0.4, 0.0, 0.0, 0.1, 0.0, 0.6, 0.0, 0.15, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 1.0,
        ],
    );
}

#[test]
fn import_multiscale_specs_honor_explicit_storage_brick_shape() {
    let grid_to_world = GridToWorld::scale_um(0.001, 0.001, 0.001);
    let storage = TiffImportStorageOptions {
        brick_shape_zyx: Some(mirante4d_core::Shape3D::new(16, 256, 256).unwrap()),
    };

    let scales = build_mean_multiscale_specs_with_storage(
        Shape4D::new(1, 2563, 2240, 4183).unwrap(),
        grid_to_world,
        storage,
    )
    .unwrap();

    assert_eq!(
        scales[0].brick_shape,
        Shape4D::new(1, 16, 256, 256).unwrap()
    );
    assert_eq!(
        scales[1].brick_shape,
        Shape4D::new(1, 16, 256, 256).unwrap()
    );
    assert_eq!(
        scales.last().unwrap().brick_shape,
        Shape4D::new(1, 16, 35, 66).unwrap()
    );
}

#[test]
fn imports_large_tiff_stack_with_bounded_native_chunks() {
    let tempdir = tempfile::tempdir().unwrap();
    let input_file = tempdir.path().join("large-source.tif");
    write_stack_with_dimensions(&input_file, 0, 0, 65, 65, 65).unwrap();
    let output = tempdir.path().join("large.m4d");

    let report = import_tiff_source(accepted_source_import_options(
        TiffImportSource::SingleFile(input_file),
        output,
        "large",
        "Large",
        [0.2, 0.3, 0.5],
        None,
    ))
    .unwrap();

    let manifest = load_manifest(&report.output_package).unwrap();
    let scale = &manifest.layers[0].scales[0];
    assert_eq!(scale.shape, Shape4D::new(1, 65, 65, 65).unwrap());
    assert_eq!(
        scale.storage.brick_shape,
        Shape4D::new(1, 64, 64, 64).unwrap()
    );
    assert_eq!(scale.bricks.grid_shape, Shape4D::new(1, 2, 2, 2).unwrap());
    assert_eq!(scale.bricks.records.len(), 8);

    let dataset = DatasetHandle::open(&report.output_package).unwrap();
    let edge = dataset
        .read_u16_brick_at_scale(
            &LayerId::new("ch0").unwrap(),
            0,
            TimeIndex(0),
            mirante4d_data::SpatialBrickIndex::new(1, 1, 1),
        )
        .unwrap();
    assert_eq!(edge.volume.shape, Shape3D::new(1, 1, 1).unwrap());
    assert_eq!(edge.voxel(0, 0, 0), Some(896));
}

#[test]
fn imports_single_uint16_tiff_file_to_native_dataset() {
    let tempdir = tempfile::tempdir().unwrap();
    let input_file = tempdir.path().join("single-source.tif");
    write_stack(&input_file, 0, 0).unwrap();
    let output = tempdir.path().join("single.m4d");

    let inspection =
        inspect_tiff_source(&TiffImportSource::SingleFile(input_file.clone())).unwrap();
    assert_eq!(inspection.file_count, 1);
    assert_eq!(inspection.channel_count, 1);
    assert_eq!(inspection.timepoint_count, 1);
    assert_eq!(inspection.source_dtype, IntensityDType::Uint16);

    let report = import_tiff_source(accepted_source_import_options(
        TiffImportSource::SingleFile(input_file),
        output.clone(),
        "single",
        "Single",
        [0.2, 0.3, 0.5],
        None,
    ))
    .unwrap();

    assert_eq!(report.output_package, output);
    assert_eq!(report.channel_count, 1);
    assert_eq!(report.timepoint_count, 1);
    assert_eq!(report.z_planes, 2);

    let manifest = load_manifest(&report.output_package).unwrap();
    assert_eq!(manifest.layers.len(), 1);
    assert_eq!(manifest.layers[0].id, "ch0");
    assert_eq!(manifest.layers[0].channel.index, 0);
    assert_eq!(
        manifest.provenance.kind,
        NativeDatasetProvenanceKind::Imported
    );
    assert_eq!(
        manifest.provenance.source_format.as_deref(),
        Some(SOURCE_FORMAT_EXPLICIT_TIFF_STACK)
    );
    assert_eq!(manifest.provenance.source_files.len(), 1);
    let source_file = &manifest.provenance.source_files[0];
    assert_eq!(source_file.display_name, "single-source.tif");
    assert_eq!(
        source_file.fingerprint_blake3.as_ref().map(String::len),
        Some(64)
    );
    let source_metadata = manifest.provenance.source_metadata.as_ref().unwrap();
    assert_eq!(source_metadata.native_axes, ["t", "z", "y", "x"]);
    assert!(source_metadata.channels_as_layers);
    assert_eq!(source_metadata.voxel_spacing_um, [0.2, 0.3, 0.5]);
    assert_eq!(source_metadata.value_range.min, 0.0);
    assert_eq!(source_metadata.value_range.max, 15.0);
    assert!(
        manifest
            .provenance
            .user_corrections
            .iter()
            .any(|correction| correction.field == "voxel_spacing_um")
    );

    let dataset = DatasetHandle::open(&report.output_package).unwrap();
    let volume = dataset
        .read_u16_volume(&LayerId::new("ch0").unwrap(), TimeIndex(0))
        .unwrap();
    assert_eq!(volume.voxel(1, 1, 2), Some(15));
}

#[test]
fn imports_single_uint8_tiff_file_to_native_uint8_dataset() {
    let tempdir = tempfile::tempdir().unwrap();
    let input_file = tempdir.path().join("single-source-u8.tif");
    write_u8_stack(&input_file).unwrap();
    let output = tempdir.path().join("single-u8.m4d");

    let inspection =
        inspect_tiff_source(&TiffImportSource::SingleFile(input_file.clone())).unwrap();
    assert_eq!(inspection.file_count, 1);
    assert_eq!(inspection.channel_count, 1);
    assert_eq!(inspection.timepoint_count, 1);
    assert_eq!(inspection.shape, TiffStackShape { z: 2, y: 2, x: 3 });
    assert_eq!(inspection.source_dtype, IntensityDType::Uint8);

    let report = import_tiff_source(accepted_source_import_options(
        TiffImportSource::SingleFile(input_file),
        output.clone(),
        "single-u8",
        "Single U8",
        [0.2, 0.3, 0.5],
        None,
    ))
    .unwrap();

    let manifest = load_manifest(&report.output_package).unwrap();
    assert_eq!(manifest.layers.len(), 1);
    assert_eq!(manifest.layers[0].dtype.source, IntensityDType::Uint8);
    assert_eq!(manifest.layers[0].dtype.stored, IntensityDType::Uint8);
    assert_eq!(
        manifest.layers[0].dtype.conversion,
        mirante4d_format::manifest::DTypeConversion::Lossless
    );
    assert_eq!(manifest.layers[0].scales[0].statistics.min, 0.0);
    assert_eq!(manifest.layers[0].scales[0].statistics.max, 15.0);
    assert_eq!(
        manifest.layers[0].scales[0].statistics.histogram.range_max,
        255.0
    );

    let array =
        mirante4d_format::zarr_io::open_array(&report.output_package, "arrays/intensity/ch0/s0")
            .unwrap();
    let stored: Vec<u8> = array
        .retrieve_array_subset(&[0..1, 0..2, 0..2, 0..3])
        .unwrap();
    assert_eq!(stored[0], 0);
    assert_eq!(stored[11], 15);

    let dataset = DatasetHandle::open(&report.output_package).unwrap();
    let volume = dataset
        .read_u8_volume(&LayerId::new("ch0").unwrap(), TimeIndex(0))
        .unwrap();
    assert_eq!(volume.voxel(0, 0, 0), Some(0));
    assert_eq!(volume.voxel(1, 1, 2), Some(15));
}

#[test]
fn imports_uint8_no_data_policy_with_render_valid_mask() {
    let tempdir = tempfile::tempdir().unwrap();
    let input_file = tempdir.path().join("single-source-u8-nodata.tif");
    write_u8_stack_with_no_data_corner(&input_file).unwrap();
    let output = tempdir.path().join("single-u8-nodata.m4d");
    let mut options = accepted_source_import_options(
        TiffImportSource::SingleFile(input_file),
        output.clone(),
        "single-u8-nodata",
        "Single U8 No Data",
        [0.2, 0.3, 0.5],
        None,
    );
    options.reviewed_plan.no_data_policy = Some(TiffNoDataPolicyReview {
        source_dtype: IntensityDType::Uint8,
        source_value_uint8: 255,
    });

    let report = import_tiff_source(options).unwrap();

    let manifest = load_manifest(&report.output_package).unwrap();
    let layer = &manifest.layers[0];
    let policy = layer.no_data_policy.as_ref().unwrap();
    assert_eq!(policy.kind, NoDataPolicyKind::SentinelValue);
    assert_eq!(policy.source_value, 255.0);
    assert_eq!(policy.source_dtype, IntensityDType::Uint8);
    assert_eq!(
        policy.visibility_policy,
        NoDataVisibilityPolicy::InvisibleWith1VoxelInvalidDilation
    );

    let scale = &layer.scales[0];
    let validity = scale.validity.as_ref().unwrap();
    assert_eq!(validity.array_path, "arrays/validity/ch0/s0_render_valid");
    assert_eq!(validity.valid_voxel_count, 19);
    assert_eq!(validity.invalid_voxel_count, 8);
    assert_eq!(validity.records.len(), 1);
    assert_eq!(validity.records[0].valid_voxel_count, 19);
    assert_eq!(scale.bricks.records[0].valid_voxel_count, 19);
    assert!(scale.bricks.records[0].occupied);
    assert_eq!(scale.bricks.records[0].min, 2.0);
    assert_eq!(scale.bricks.records[0].max, 26.0);
    assert_eq!(scale.statistics.min, 2.0);
    assert_eq!(scale.statistics.max, 26.0);
    assert_eq!(scale.statistics.histogram.bins.iter().sum::<u64>(), 19);
    assert!(
        manifest
            .provenance
            .user_corrections
            .iter()
            .any(|correction| correction.field == "no_data_policy"
                && correction.reviewed_value.contains("255")
                && correction
                    .reviewed_value
                    .contains("invisible_with_1_voxel_invalid_dilation"))
    );

    let intensity =
        mirante4d_format::zarr_io::open_array(&report.output_package, "arrays/intensity/ch0/s0")
            .unwrap();
    let stored: Vec<u8> = intensity
        .retrieve_array_subset(&[0..1, 0..3, 0..3, 0..3])
        .unwrap();
    assert_eq!(stored[0], 255);
    assert_eq!(stored[26], 26);

    let mask_array = mirante4d_format::zarr_io::open_array(
        &report.output_package,
        "arrays/validity/ch0/s0_render_valid",
    )
    .unwrap();
    let render_valid: Vec<u8> = mask_array
        .retrieve_array_subset(&[0..1, 0..3, 0..3, 0..3])
        .unwrap();
    assert_eq!(render_valid.iter().filter(|&&value| value == 1).count(), 19);
    assert_eq!(render_valid[0], 0);
    assert_eq!(render_valid[13], 0);
    assert_eq!(render_valid[2], 1);
    assert_eq!(render_valid[26], 1);

    let dataset = DatasetHandle::open(&report.output_package).unwrap();
    let volume = dataset
        .read_u8_volume(&LayerId::new("ch0").unwrap(), TimeIndex(0))
        .unwrap();
    assert_eq!(volume.voxel(0, 0, 0), Some(255));
    assert_eq!(volume.render_voxel(0, 0, 0), None);
    assert_eq!(volume.render_voxel(1, 1, 1), None);
    assert_eq!(volume.render_voxel(0, 0, 2), Some(2));
    assert_eq!(volume.render_voxel(2, 2, 2), Some(26));
    assert_eq!(volume.render_valid_voxel_count(), 19);
}

#[test]
fn imports_uint8_plane_series_no_data_policy_streaming() {
    let tempdir = tempfile::tempdir().unwrap();
    let input_root = tempdir.path().join("plane-source");
    let channel_root = input_root.join("cell");
    fs::create_dir_all(&channel_root).unwrap();
    for z in 0..3 {
        let values = (0..9)
            .map(|index| {
                if z == 0 && index == 0 {
                    255
                } else {
                    (z * 9 + index) as u8
                }
            })
            .collect::<Vec<_>>();
        write_u8_plane_values(
            &channel_root.join(format!("plane-{z:03}.tif")),
            3,
            3,
            &values,
        )
        .unwrap();
    }
    let output = tempdir.path().join("plane-u8-nodata.m4d");
    let source = TiffImportSource::Directory(input_root);
    let inspection = inspect_tiff_source_for_review(&source).unwrap();
    assert_eq!(
        inspection.source_profile,
        TiffSourceProfile::PlaneSeriesVolume
    );
    let mut reviewed_plan = accepted_tiff_reviewed_import_plan(&inspection, [0.2, 0.3, 0.5], true);
    reviewed_plan.no_data_policy = Some(TiffNoDataPolicyReview {
        source_dtype: IntensityDType::Uint8,
        source_value_uint8: 255,
    });
    let options = TiffSourceImportOptions {
        source,
        output_package: output.clone(),
        dataset_id: "plane-u8-nodata".to_owned(),
        dataset_name: "Plane U8 No Data".to_owned(),
        voxel_spacing_um: [0.2, 0.3, 0.5],
        channel_metadata: BTreeMap::new(),
        file_grouping: None,
        existing_policy: ExistingPackagePolicy::Fail,
        storage: Default::default(),
        reviewed_plan,
    };

    let report = import_tiff_source(options).unwrap();

    let manifest = load_manifest(&report.output_package).unwrap();
    let layer = &manifest.layers[0];
    assert_eq!(layer.shape, Shape4D::new(1, 3, 3, 3).unwrap());
    assert_eq!(layer.no_data_policy.as_ref().unwrap().source_value, 255.0);
    let validity = layer.scales[0].validity.as_ref().unwrap();
    assert_eq!(validity.valid_voxel_count, 19);
    assert_eq!(validity.invalid_voxel_count, 8);
    assert_eq!(layer.scales[0].statistics.min, 2.0);
    assert_eq!(layer.scales[0].statistics.max, 26.0);
    assert_eq!(
        layer.scales[0]
            .statistics
            .histogram
            .bins
            .iter()
            .sum::<u64>(),
        19
    );

    let intensity =
        mirante4d_format::zarr_io::open_array(&report.output_package, "arrays/intensity/ch0/s0")
            .unwrap();
    let stored: Vec<u8> = intensity
        .retrieve_array_subset(&[0..1, 0..3, 0..3, 0..3])
        .unwrap();
    assert_eq!(stored[0], 255);
    assert_eq!(stored[26], 26);

    let mask_array = mirante4d_format::zarr_io::open_array(
        &report.output_package,
        "arrays/validity/ch0/s0_render_valid",
    )
    .unwrap();
    let render_valid: Vec<u8> = mask_array
        .retrieve_array_subset(&[0..1, 0..3, 0..3, 0..3])
        .unwrap();
    assert_eq!(render_valid.iter().filter(|&&value| value == 1).count(), 19);
    assert_eq!(render_valid[0], 0);
    assert_eq!(render_valid[13], 0);
    assert_eq!(render_valid[2], 1);
    assert_eq!(render_valid[26], 1);

    let dataset = DatasetHandle::open(&report.output_package).unwrap();
    let volume = dataset
        .read_u8_volume(&LayerId::new("ch0").unwrap(), TimeIndex(0))
        .unwrap();
    assert_eq!(volume.voxel(0, 0, 0), Some(255));
    assert_eq!(volume.render_voxel(0, 0, 0), None);
    assert_eq!(volume.render_voxel(1, 1, 1), None);
    assert_eq!(volume.render_voxel(2, 2, 2), Some(26));
    assert_eq!(volume.render_valid_voxel_count(), 19);
}

#[test]
fn no_data_dilation_uses_3d_or_2d_neighborhood_by_shape() {
    let shape_3d = Shape4D::new(1, 3, 1, 1).unwrap();
    let mut source_valid_3d = vec![1, 0, 1];
    let render_valid_3d = render_valid_after_one_voxel_invalid_dilation(&source_valid_3d, shape_3d);
    assert_eq!(render_valid_3d, vec![0, 0, 0]);

    let shape_2d = Shape4D::new(1, 1, 3, 3).unwrap();
    let mut source_valid_2d = vec![1; 9];
    source_valid_2d[4] = 0;
    let render_valid_2d = render_valid_after_one_voxel_invalid_dilation(&source_valid_2d, shape_2d);
    assert_eq!(render_valid_2d, vec![0; 9]);

    source_valid_3d[1] = 1;
    let all_valid = render_valid_after_one_voxel_invalid_dilation(&source_valid_3d, shape_3d);
    assert_eq!(all_valid, vec![1, 1, 1]);
}

#[test]
fn masked_uint8_downsample_ignores_invalid_sentinel_values() {
    let cancellation = ImportCancellationToken::new();
    let source_shape = Shape4D::new(1, 2, 2, 2).unwrap();
    let output_shape = Shape4D::new(1, 1, 1, 1).unwrap();
    let values = vec![255, 255, 255, 255, 255, 255, 255, 10];
    let render_valid = vec![0, 0, 0, 0, 0, 0, 0, 1];

    let (downsampled, downsampled_valid) = downsample_mean_u8_zyx_masked(
        &values,
        &render_valid,
        source_shape,
        output_shape,
        &cancellation,
    )
    .unwrap();

    assert_eq!(downsampled, vec![10]);
    assert_eq!(downsampled_valid, vec![1]);
}

#[test]
fn imports_single_float32_tiff_file_to_native_float32_dataset() {
    let tempdir = tempfile::tempdir().unwrap();
    let input_file = tempdir.path().join("single-source-f32.tif");
    write_f32_stack(&input_file).unwrap();
    let output = tempdir.path().join("single-f32.m4d");

    let inspection =
        inspect_tiff_source(&TiffImportSource::SingleFile(input_file.clone())).unwrap();
    assert_eq!(inspection.file_count, 1);
    assert_eq!(inspection.channel_count, 1);
    assert_eq!(inspection.timepoint_count, 1);
    assert_eq!(inspection.shape, TiffStackShape { z: 2, y: 2, x: 3 });
    assert_eq!(inspection.source_dtype, IntensityDType::Float32);

    let report = import_tiff_source(accepted_source_import_options(
        TiffImportSource::SingleFile(input_file),
        output.clone(),
        "single-f32",
        "Single F32",
        [0.2, 0.3, 0.5],
        None,
    ))
    .unwrap();

    let manifest = load_manifest(&report.output_package).unwrap();
    let layer = &manifest.layers[0];
    let scale = &layer.scales[0];
    assert_eq!(report.output_package, output);
    assert_eq!(layer.dtype.source, IntensityDType::Float32);
    assert_eq!(layer.dtype.stored, IntensityDType::Float32);
    assert_eq!(
        layer.dtype.conversion,
        mirante4d_format::manifest::DTypeConversion::Lossless
    );
    assert_eq!(scale.statistics.min, -1.5);
    assert_eq!(scale.statistics.max, 15.75);
    assert_eq!(scale.statistics.histogram.bin_count, 4096);
    assert_eq!(scale.statistics.histogram.bins.iter().sum::<u64>(), 12);
    assert!(
        scale
            .storage
            .shard_records
            .iter()
            .all(|record| record.payload_bytes > 0)
    );

    let dataset = DatasetHandle::open(&report.output_package).unwrap();
    let volume = dataset
        .read_f32_volume(&LayerId::new("ch0").unwrap(), TimeIndex(0))
        .unwrap();
    assert_eq!(volume.voxel(0, 0, 0), Some(-1.5));
    assert_eq!(volume.voxel(0, 0, 2), Some(0.25));
    assert_eq!(volume.voxel(1, 1, 2), Some(15.75));
}

#[test]
fn rejects_uint32_tiff_instead_of_treating_it_as_float32() {
    let tempdir = tempfile::tempdir().unwrap();
    let input_file = tempdir.path().join("single-source-u32.tif");
    write_u32_stack(&input_file).unwrap();

    let err = inspect_tiff_source(&TiffImportSource::SingleFile(input_file.clone())).unwrap_err();

    assert!(matches!(
        err,
        ImportError::UnsupportedPixelType { path } if path == input_file
    ));
}

#[test]
fn imports_zero_tiff_stack_as_valid_dense_data() {
    let tempdir = tempfile::tempdir().unwrap();
    let input_file = tempdir.path().join("zero-source.tif");
    write_zero_stack(&input_file, 4, 1, 1).unwrap();
    let output = tempdir.path().join("zero.m4d");

    let report = import_tiff_source(accepted_source_import_options(
        TiffImportSource::SingleFile(input_file),
        output,
        "zero",
        "Zero",
        [1.0, 1.0, 1.0],
        None,
    ))
    .unwrap();

    let manifest = load_manifest(&report.output_package).unwrap();
    let records = &manifest.layers[0].scales[0].bricks.records;
    assert_eq!(records.len(), 1);
    assert!(records[0].occupied);
    assert_eq!(records[0].valid_voxel_count, 4);
    assert_eq!(records[0].min, 0.0);
    assert_eq!(records[0].max, 0.0);

    let dataset = DatasetHandle::open(&report.output_package).unwrap();
    let brick = dataset
        .read_u16_brick_at_scale(
            &LayerId::new("ch0").unwrap(),
            0,
            TimeIndex(0),
            mirante4d_data::SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    assert!(brick.occupied);
    assert_eq!(brick.valid_voxel_count, 4);
    assert_eq!(brick.values(), &[0, 0, 0, 0]);
}

#[test]
fn reads_multi_strip_tiff_stack_by_chunks() {
    let tempdir = tempfile::tempdir().unwrap();
    let input_file = tempdir.path().join("striped-source.tif");
    write_striped_stack(&input_file).unwrap();

    let stack = read_tiff_stack(&input_file).unwrap();

    assert_eq!(stack.shape, TiffStackShape { z: 2, y: 4, x: 4 });
    assert_eq!(stack.source_dtype, IntensityDType::Uint16);
    let TiffStackValues::U16(values_zyx) = stack.values_zyx else {
        panic!("striped uint16 stack decoded into wrong value buffer");
    };
    assert_eq!(values_zyx[0], 0);
    assert_eq!(values_zyx[3], 3);
    assert_eq!(values_zyx[4], 10);
    assert_eq!(values_zyx[15], 33);
    assert_eq!(values_zyx[16], 100);
    assert_eq!(values_zyx[31], 133);
}

#[test]
fn inspection_extracts_ome_tiff_voxel_spacing_metadata() {
    let tempdir = tempfile::tempdir().unwrap();
    let input_file = tempdir.path().join("ome-source.tif");
    write_ome_stack(&input_file, [0.2, 300.0, 0.0005], ["um", "nm", "mm"]).unwrap();

    let inspection =
        inspect_tiff_source(&TiffImportSource::SingleFile(input_file.clone())).unwrap();

    assert_eq!(
        inspection.source_metadata.voxel_spacing_status,
        TiffVoxelSpacingMetadataStatus::Complete
    );
    assert_eq!(
        inspection.source_metadata.voxel_spacing_source,
        Some(TiffVoxelSpacingMetadataSource::OmeXml)
    );
    assert_eq!(
        inspection.source_metadata.voxel_spacing_um.unwrap(),
        [0.2, 0.3, 0.5]
    );
}

fn assert_incomplete_ome_xml(description: &str) {
    assert_eq!(
        parse_ome_tiff_source_metadata(description).voxel_spacing_status,
        TiffVoxelSpacingMetadataStatus::Incomplete
    );
}

#[test]
fn ome_xml_rejects_input_over_byte_budget() {
    assert_incomplete_ome_xml(&" ".repeat(MAX_OME_XML_BYTES + 1));
}

#[test]
fn ome_xml_rejects_documents_over_event_budget() {
    let mut xml = String::from("<OME>");
    for _ in 0..MAX_OME_XML_EVENTS {
        xml.push_str("<Node/>");
    }
    xml.push_str("<Node/></OME>");

    assert!(xml.len() < MAX_OME_XML_BYTES);
    assert_incomplete_ome_xml(&xml);
}

#[test]
fn ome_xml_rejects_documents_over_depth_budget() {
    let mut xml = String::from("<OME>");
    for _ in 0..MAX_OME_XML_DEPTH {
        xml.push_str("<Node>");
    }

    assert_incomplete_ome_xml(&xml);
}

#[test]
fn ome_xml_rejects_elements_over_attribute_count_budget() {
    let mut xml = String::from("<OME");
    for index in 0..=MAX_OME_XML_ATTRIBUTES_PER_ELEMENT {
        write!(&mut xml, " a{index}=\"{index}\"").unwrap();
    }
    xml.push_str("><Pixels/></OME>");

    assert_incomplete_ome_xml(&xml);
}

#[test]
fn ome_xml_rejects_namespace_declarations_over_element_byte_budget() {
    let namespace = "n".repeat(MAX_OME_XML_ATTRIBUTE_BYTES_PER_ELEMENT);
    let xml = format!("<OME xmlns:hostile=\"{namespace}\"><Pixels/></OME>");

    assert!(xml.len() < MAX_OME_XML_BYTES);
    assert_incomplete_ome_xml(&xml);
}

#[test]
fn ome_xml_rejects_documents_over_total_attribute_byte_budget() {
    let payload = "a".repeat(MAX_OME_XML_ATTRIBUTE_BYTES_PER_ELEMENT / 2);
    let element_count = MAX_OME_XML_TOTAL_ATTRIBUTE_BYTES / payload.len() + 1;
    let mut xml = String::from("<OME>");
    for _ in 0..element_count {
        write!(&mut xml, "<Node payload=\"{payload}\"/>").unwrap();
    }
    xml.push_str("<Pixels/></OME>");

    assert!(xml.len() < MAX_OME_XML_BYTES);
    assert_incomplete_ome_xml(&xml);
}

#[test]
fn ome_xml_rejects_metadata_values_over_decode_allocation_budget() {
    let physical_size_x = format!("{}1", "0".repeat(MAX_OME_XML_METADATA_VALUE_BYTES));
    let xml = format!(
        "<OME><Pixels PhysicalSizeX=\"{physical_size_x}\" PhysicalSizeXUnit=\"um\" PhysicalSizeY=\"0.3\" PhysicalSizeYUnit=\"um\" PhysicalSizeZ=\"0.5\" PhysicalSizeZUnit=\"um\"/></OME>"
    );

    assert_incomplete_ome_xml(&xml);
}

#[test]
fn ome_xml_rejects_duplicate_metadata_attributes() {
    assert_incomplete_ome_xml(
        "<OME><Pixels PhysicalSizeX=\"0.2\" PhysicalSizeX=\"0.3\" PhysicalSizeXUnit=\"um\" PhysicalSizeY=\"0.3\" PhysicalSizeYUnit=\"um\" PhysicalSizeZ=\"0.5\" PhysicalSizeZUnit=\"um\"/></OME>",
    );
}

#[test]
fn inspection_marks_conflicting_ome_tiff_voxel_spacing_metadata() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input");
    fs::create_dir(&input).unwrap();
    write_ome_stack(
        &input.join("sample_ch0_stack0000_0000msec.tif"),
        [0.2, 0.3, 0.5],
        ["um", "um", "um"],
    )
    .unwrap();
    write_ome_stack(
        &input.join("sample_ch0_stack0001_0000msec.tif"),
        [0.2, 0.3, 0.6],
        ["um", "um", "um"],
    )
    .unwrap();

    let inspection = inspect_tiff_directory(&input).unwrap();

    assert_eq!(
        inspection.source_metadata.voxel_spacing_status,
        TiffVoxelSpacingMetadataStatus::Conflicting
    );
    assert!(inspection.source_metadata.voxel_spacing_um.is_none());
}

#[test]
fn imports_directory_with_explicit_grouping_without_filename_tokens() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input");
    fs::create_dir(&input).unwrap();
    let timepoint_0 = input.join("first.tif");
    let timepoint_1 = input.join("second.tif");
    write_stack(&timepoint_0, 0, 0).unwrap();
    write_stack(&timepoint_1, 0, 1).unwrap();
    let output = tempdir.path().join("explicit-grouping.m4d");

    let strict_err = inspect_tiff_directory(&input).unwrap_err();
    assert!(matches!(strict_err, ImportError::MissingChannel(_)));

    let review =
        inspect_tiff_source_for_review(&TiffImportSource::Directory(input.clone())).unwrap();
    assert_eq!(review.file_count, 2);
    assert_eq!(review.files[0].channel, 0);
    assert_eq!(review.files[0].stack_index, 0);
    assert_eq!(review.files[1].channel, 0);
    assert_eq!(review.files[1].stack_index, 1);

    let file_grouping = vec![
        TiffFileGrouping {
            path: timepoint_0,
            channel: 0,
            stack_index: 0,
        },
        TiffFileGrouping {
            path: timepoint_1,
            channel: 0,
            stack_index: 1,
        },
    ];
    let reviewed = inspect_tiff_source_with_grouping(
        &TiffImportSource::Directory(input.clone()),
        &file_grouping,
    )
    .unwrap();
    let report = import_tiff_directory(TiffDirectoryImportOptions {
        input_dir: input,
        output_package: output,
        dataset_id: "explicit-grouping".to_owned(),
        dataset_name: "Explicit Grouping".to_owned(),
        voxel_spacing_um: [0.2, 0.3, 0.5],
        channel_metadata: BTreeMap::new(),
        file_grouping: Some(file_grouping),
        existing_policy: ExistingPackagePolicy::Fail,
        storage: Default::default(),
        reviewed_plan: accepted_tiff_reviewed_import_plan(&reviewed, [0.2, 0.3, 0.5], true),
    })
    .unwrap();

    assert_eq!(report.channel_count, 1);
    assert_eq!(report.timepoint_count, 2);
    let dataset = DatasetHandle::open(&report.output_package).unwrap();
    let volume = dataset
        .read_u16_volume(&LayerId::new("ch0").unwrap(), TimeIndex(1))
        .unwrap();
    assert_eq!(volume.voxel(1, 1, 2), Some(115));
}

#[test]
fn imports_plane_series_folder_per_channel_in_lexicographic_order() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("plane-series");
    let channel_a = input.join("alpha");
    let channel_b = input.join("beta");
    fs::create_dir_all(&channel_a).unwrap();
    fs::create_dir_all(&channel_b).unwrap();
    write_u8_plane(&channel_a.join("b.tif"), 20).unwrap();
    write_u8_plane(&channel_a.join("a.tif"), 10).unwrap();
    write_u8_plane(&channel_a.join("c.tif"), 30).unwrap();
    write_u8_plane(&channel_b.join("a.tif"), 110).unwrap();
    write_u8_plane(&channel_b.join("b.tif"), 120).unwrap();
    write_u8_plane(&channel_b.join("c.tif"), 130).unwrap();

    let source = TiffImportSource::Directory(input.clone());
    let inspection = inspect_tiff_source_for_review(&source).unwrap();
    assert_eq!(
        inspection.source_profile,
        TiffSourceProfile::PlaneSeriesVolume
    );
    assert_eq!(inspection.file_count, 6);
    assert_eq!(inspection.channel_count, 2);
    assert_eq!(inspection.timepoint_count, 1);
    assert_eq!(inspection.shape, TiffStackShape { z: 3, y: 2, x: 3 });
    assert_eq!(inspection.source_dtype, IntensityDType::Uint8);
    assert_eq!(inspection.files[0].path, channel_a.join("a.tif"));
    assert_eq!(inspection.files[0].channel, 0);
    assert_eq!(inspection.files[0].stack_index, 0);
    assert_eq!(inspection.files[1].path, channel_a.join("b.tif"));
    assert_eq!(inspection.files[3].path, channel_b.join("a.tif"));
    assert_eq!(
        inspection.source_metadata.voxel_spacing_status,
        TiffVoxelSpacingMetadataStatus::Missing
    );

    let output = tempdir.path().join("plane-series.m4d");
    let reviewed_plan =
        accepted_tiff_reviewed_import_plan(&inspection, [0.001, 0.001, 0.001], true);
    assert_eq!(
        reviewed_plan.source_format,
        SOURCE_FORMAT_PLANE_SERIES_TIFF_VOLUME
    );
    assert_eq!(
        reviewed_plan.source_profile,
        TiffSourceProfile::PlaneSeriesVolume
    );
    let report = import_tiff_source(TiffSourceImportOptions {
        source,
        output_package: output.clone(),
        dataset_id: "plane-series".to_owned(),
        dataset_name: "Plane Series".to_owned(),
        voxel_spacing_um: [0.001, 0.001, 0.001],
        channel_metadata: BTreeMap::new(),
        file_grouping: None,
        existing_policy: ExistingPackagePolicy::Fail,
        storage: Default::default(),
        reviewed_plan,
    })
    .unwrap();

    assert_eq!(report.output_package, output);
    assert_eq!(report.channel_count, 2);
    assert_eq!(report.timepoint_count, 1);
    assert_eq!(report.z_planes, 3);

    let manifest = load_manifest(&report.output_package).unwrap();
    assert_eq!(
        manifest.provenance.source_format.as_deref(),
        Some(SOURCE_FORMAT_PLANE_SERIES_TIFF_VOLUME)
    );
    assert!(
        manifest
            .provenance
            .user_corrections
            .iter()
            .any(|correction| correction.field == "source_profile"
                && correction.reviewed_value == TiffSourceProfile::PlaneSeriesVolume.id())
    );
    assert_eq!(manifest.layers[0].shape, Shape4D::new(1, 3, 2, 3).unwrap());
    assert_eq!(manifest.layers[0].dtype.source, IntensityDType::Uint8);
    assert_eq!(manifest.layers[0].dtype.stored, IntensityDType::Uint8);

    let dataset = DatasetHandle::open(&report.output_package).unwrap();
    let ch0 = dataset
        .read_u8_volume(&LayerId::new("ch0").unwrap(), TimeIndex(0))
        .unwrap();
    let ch1 = dataset
        .read_u8_volume(&LayerId::new("ch1").unwrap(), TimeIndex(0))
        .unwrap();
    assert_eq!(ch0.voxel(0, 0, 0), Some(10));
    assert_eq!(ch0.voxel(1, 0, 0), Some(20));
    assert_eq!(ch0.voxel(2, 0, 0), Some(30));
    assert_eq!(ch1.voxel(0, 0, 0), Some(110));
    assert_eq!(ch1.voxel(1, 0, 0), Some(120));
    assert_eq!(ch1.voxel(2, 0, 0), Some(130));
}

#[test]
fn plane_series_review_accepts_single_channel_child_folder() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("plane-series");
    let channel = input.join("volume");
    fs::create_dir_all(&channel).unwrap();
    write_u8_plane(&channel.join("a.tif"), 10).unwrap();
    write_u8_plane(&channel.join("b.tif"), 20).unwrap();

    let inspection =
        inspect_tiff_source_for_review(&TiffImportSource::Directory(input.clone())).unwrap();

    assert_eq!(
        inspection.source_profile,
        TiffSourceProfile::PlaneSeriesVolume
    );
    assert_eq!(inspection.channel_count, 1);
    assert_eq!(inspection.timepoint_count, 1);
    assert_eq!(inspection.shape, TiffStackShape { z: 2, y: 2, x: 3 });
    assert_eq!(inspection.files[0].path, channel.join("a.tif"));
    assert_eq!(inspection.files[0].channel, 0);
    assert_eq!(inspection.files[1].path, channel.join("b.tif"));
    assert_eq!(inspection.files[1].stack_index, 1);
}

#[test]
fn direct_plane_folder_is_not_accepted_as_plane_series_profile() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("direct-planes");
    fs::create_dir_all(&input).unwrap();
    write_u8_plane(&input.join("a.tif"), 10).unwrap();
    write_u8_plane(&input.join("b.tif"), 20).unwrap();

    let source = TiffImportSource::Directory(input);
    let inspection = inspect_tiff_source_for_review(&source).unwrap();
    assert_eq!(
        inspection.source_profile,
        TiffSourceProfile::StackSeriesMovie
    );

    let output = tempdir.path().join("should-not-import.m4d");
    let mut reviewed_plan = accepted_tiff_reviewed_import_plan(&inspection, [1.0, 1.0, 1.0], true);
    reviewed_plan.source_profile = TiffSourceProfile::PlaneSeriesVolume;
    reviewed_plan.source_format = SOURCE_FORMAT_PLANE_SERIES_TIFF_VOLUME.to_owned();
    let err = import_tiff_source(TiffSourceImportOptions {
        source,
        output_package: output,
        dataset_id: "direct-planes".to_owned(),
        dataset_name: "Direct Planes".to_owned(),
        voxel_spacing_um: [1.0, 1.0, 1.0],
        channel_metadata: BTreeMap::new(),
        file_grouping: None,
        existing_policy: ExistingPackagePolicy::Fail,
        storage: Default::default(),
        reviewed_plan,
    })
    .unwrap_err();

    assert!(matches!(
        err,
        ImportError::ReviewedSourceProfileMismatch {
            reviewed: TiffSourceProfile::PlaneSeriesVolume,
            inspected: TiffSourceProfile::StackSeriesMovie,
        }
    ));
}

#[test]
fn plane_series_review_rejects_recursive_and_mixed_layouts() {
    let tempdir = tempfile::tempdir().unwrap();
    let recursive = tempdir.path().join("recursive");
    let nested = recursive.join("ch0").join("nested");
    fs::create_dir_all(&nested).unwrap();
    write_u8_plane(&recursive.join("ch0").join("a.tif"), 1).unwrap();
    write_u8_plane(&nested.join("b.tif"), 2).unwrap();
    let err = inspect_tiff_source_for_review(&TiffImportSource::Directory(recursive)).unwrap_err();
    assert!(matches!(err, ImportError::InvalidPlaneSeriesLayout { .. }));

    let mixed = tempdir.path().join("mixed");
    fs::create_dir_all(mixed.join("ch0")).unwrap();
    write_u8_plane(&mixed.join("direct.tif"), 1).unwrap();
    write_u8_plane(&mixed.join("ch0").join("a.tif"), 2).unwrap();
    let err = inspect_tiff_source_for_review(&TiffImportSource::Directory(mixed)).unwrap_err();
    assert!(matches!(err, ImportError::AmbiguousTiffSourceLayout { .. }));
}

#[test]
fn plane_series_review_rejects_multipage_tiff_planes() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("plane-series");
    let channel = input.join("ch0");
    fs::create_dir_all(&channel).unwrap();
    write_u8_stack(&channel.join("a.tif")).unwrap();

    let err = inspect_tiff_source_for_review(&TiffImportSource::Directory(input)).unwrap_err();

    assert!(matches!(
        err,
        ImportError::PlaneSeriesFileHasMultipleImages { z: 2, .. }
    ));
}

#[test]
fn reports_progress_events_during_import() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input");
    fs::create_dir(&input).unwrap();
    for channel in 0..2 {
        for timepoint in 0..2 {
            let path = input.join(format!(
                "sample_ch{channel}_stack{timepoint:04}_0000msec.tif"
            ));
            write_stack(&path, channel, timepoint).unwrap();
        }
    }
    let output = tempdir.path().join("imported.m4d");
    let cancellation = ImportCancellationToken::new();
    let mut events = Vec::new();

    import_tiff_directory_with_progress(
        accepted_directory_import_options(
            input,
            output.clone(),
            "import-test",
            "Import Test",
            [1.0, 1.0, 1.0],
            BTreeMap::new(),
        ),
        &cancellation,
        |event| {
            events.push(event);
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(
        events.first(),
        Some(&ImportProgressEvent::DiscoveredInput { file_count: 4 })
    );
    let expected_estimate = TiffImportStorageEstimate {
        source_payload_bytes: 96,
        derived_multiscale_payload_bytes: 0,
        estimated_metadata_bytes: 1_179_648,
        estimated_total_bytes: 1_179_744,
        peak_working_stack_bytes: 24,
    };
    assert_eq!(
        events.get(1),
        Some(&ImportProgressEvent::EstimatedStorage {
            estimate: expected_estimate
        })
    );
    let read_progress = events
        .iter()
        .filter_map(|event| match event {
            ImportProgressEvent::ReadStack {
                completed, total, ..
            } => Some((*completed, *total)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(read_progress, [(1, 4), (2, 4), (3, 4), (4, 4)]);
    assert!(events.iter().any(|event| {
        matches!(
            event,
            ImportProgressEvent::BuiltScale {
                channel: 1,
                level: 0
            }
        )
    }));
    assert!(events.iter().any(|event| {
        matches!(
            event,
            ImportProgressEvent::WritingPackage {
                output_package
            } if output_package == &output
        )
    }));
    assert!(matches!(
        events.last(),
        Some(ImportProgressEvent::Finished {
            output_package
        }) if output_package == &output
    ));
}

#[test]
fn inspects_tiff_directory_without_writing_output() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input");
    fs::create_dir(&input).unwrap();
    for channel in 0..2 {
        for timepoint in 0..3 {
            write_stack(
                &input.join(format!(
                    "sample_ch{channel}_stack{timepoint:04}_0000msec.tif"
                )),
                channel,
                timepoint,
            )
            .unwrap();
        }
    }

    let inspection = inspect_tiff_directory(&input).unwrap();

    assert_eq!(inspection.input_dir, input);
    assert_eq!(inspection.file_count, 6);
    assert_eq!(inspection.channel_count, 2);
    assert_eq!(inspection.timepoint_count, 3);
    assert_eq!(inspection.shape, TiffStackShape { z: 2, y: 2, x: 3 });
    assert_eq!(inspection.source_dtype, IntensityDType::Uint16);
    assert_eq!(inspection.files.len(), 6);
    assert_eq!(
        inspection.channels,
        vec![
            TiffChannelInspection {
                channel: 0,
                timepoint_count: 3,
            },
            TiffChannelInspection {
                channel: 1,
                timepoint_count: 3,
            },
        ]
    );
}

#[test]
fn inspection_rejects_stack_shape_mismatch() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input");
    fs::create_dir(&input).unwrap();
    write_stack(&input.join("sample_ch0_stack0000_0000msec.tif"), 0, 0).unwrap();
    write_stack_with_dimensions(
        &input.join("sample_ch0_stack0001_0000msec.tif"),
        0,
        1,
        4,
        2,
        2,
    )
    .unwrap();

    let err = inspect_tiff_directory(&input).unwrap_err();

    assert!(matches!(err, ImportError::StackShapeMismatch { .. }));
}

#[test]
fn inspection_rejects_mixed_tiff_source_dtypes() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input");
    fs::create_dir(&input).unwrap();
    write_stack(&input.join("sample_ch0_stack0000_0000msec.tif"), 0, 0).unwrap();
    write_u8_stack(&input.join("sample_ch0_stack0001_0000msec.tif")).unwrap();

    let err = inspect_tiff_directory(&input).unwrap_err();

    assert!(matches!(err, ImportError::SourceDTypeMismatch { .. }));
}

#[test]
fn cancellation_stops_import_without_output_or_temporary_package() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input");
    fs::create_dir(&input).unwrap();
    for timepoint in 0..2 {
        write_stack(
            &input.join(format!("sample_ch0_stack{timepoint:04}_0000msec.tif")),
            0,
            timepoint,
        )
        .unwrap();
    }
    let output = tempdir.path().join("imported.m4d");
    let cancellation = ImportCancellationToken::new();

    let err = import_tiff_directory_with_progress(
        accepted_directory_import_options(
            input,
            output.clone(),
            "import-test",
            "Import Test",
            [1.0, 1.0, 1.0],
            BTreeMap::new(),
        ),
        &cancellation,
        |event| {
            if matches!(event, ImportProgressEvent::ReadStack { completed: 1, .. }) {
                cancellation.cancel();
            }
            Ok(())
        },
    )
    .unwrap_err();

    assert!(matches!(err, ImportError::Cancelled));
    assert!(!output.exists());
    assert!(!temporary_output_package_path(&output).exists());
}

#[test]
fn cancellation_during_streamed_scale_write_removes_temporary_package() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input");
    fs::create_dir(&input).unwrap();
    for timepoint in 0..2 {
        write_stack(
            &input.join(format!("sample_ch0_stack{timepoint:04}_0000msec.tif")),
            0,
            timepoint,
        )
        .unwrap();
    }
    let output = tempdir.path().join("imported.m4d");
    let cancellation = ImportCancellationToken::new();

    let err = import_tiff_directory_with_progress(
        accepted_directory_import_options(
            input,
            output.clone(),
            "import-test",
            "Import Test",
            [1.0, 1.0, 1.0],
            BTreeMap::new(),
        ),
        &cancellation,
        |event| {
            if matches!(
                event,
                ImportProgressEvent::BuiltScale {
                    channel: 0,
                    level: 0
                }
            ) {
                cancellation.cancel();
            }
            Ok(())
        },
    )
    .unwrap_err();

    assert!(matches!(err, ImportError::Cancelled));
    assert!(!output.exists());
    assert!(!temporary_output_package_path(&output).exists());
}

#[test]
fn cancellation_before_replace_commit_preserves_existing_output() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input");
    fs::create_dir(&input).unwrap();
    write_stack(&input.join("sample_ch0_stack0000_0000msec.tif"), 0, 0).unwrap();
    let output = tempdir.path().join("imported.m4d");
    fs::create_dir(&output).unwrap();
    fs::write(output.join("old.txt"), "old package").unwrap();
    let cancellation = ImportCancellationToken::new();

    let mut options = accepted_directory_import_options(
        input,
        output.clone(),
        "import-test",
        "Import Test",
        [1.0, 1.0, 1.0],
        BTreeMap::new(),
    );
    options.existing_policy = ExistingPackagePolicy::Replace;
    let err = import_tiff_directory_with_progress(options, &cancellation, |event| {
        if matches!(event, ImportProgressEvent::WritingPackage { .. }) {
            cancellation.cancel();
        }
        Ok(())
    })
    .unwrap_err();

    assert!(matches!(err, ImportError::Cancelled));
    assert_eq!(
        fs::read_to_string(output.join("old.txt")).unwrap(),
        "old package"
    );
    assert!(!temporary_output_package_path(&output).exists());
    assert!(!replacement_backup_package_path(&output).exists());
}

#[test]
fn successful_replace_removes_stale_temporary_and_backup_packages() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input");
    fs::create_dir(&input).unwrap();
    write_stack(&input.join("sample_ch0_stack0000_0000msec.tif"), 0, 0).unwrap();
    let output = tempdir.path().join("imported.m4d");
    fs::create_dir(&output).unwrap();
    fs::write(output.join("old.txt"), "old package").unwrap();
    let temporary_output = temporary_output_package_path(&output);
    fs::create_dir(&temporary_output).unwrap();
    fs::write(temporary_output.join("stale.txt"), "stale temp").unwrap();
    let backup_output = replacement_backup_package_path(&output);
    fs::create_dir(&backup_output).unwrap();
    fs::write(backup_output.join("stale.txt"), "stale backup").unwrap();

    let mut options = accepted_directory_import_options(
        input,
        output.clone(),
        "import-test",
        "Import Test",
        [1.0, 1.0, 1.0],
        BTreeMap::new(),
    );
    options.existing_policy = ExistingPackagePolicy::Replace;
    import_tiff_directory(options).unwrap();

    assert!(!output.join("old.txt").exists());
    assert!(!temporary_output.exists());
    assert!(!backup_output.exists());
    let manifest = load_manifest(&output).unwrap();
    assert_eq!(manifest.dataset.id, "import-test");
}

#[test]
fn import_without_accepted_review_rejects_tokenless_stack_series() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input");
    fs::create_dir(&input).unwrap();
    write_stack(&input.join("sample_stack0000.tif"), 0, 0).unwrap();

    let err = import_tiff_directory(TiffDirectoryImportOptions {
        input_dir: input,
        output_package: tempdir.path().join("imported.m4d"),
        dataset_id: "import-test".to_owned(),
        dataset_name: "Import Test".to_owned(),
        voxel_spacing_um: [1.0, 1.0, 1.0],
        channel_metadata: BTreeMap::new(),
        file_grouping: None,
        existing_policy: ExistingPackagePolicy::Fail,
        storage: Default::default(),
        reviewed_plan: TiffReviewedImportPlan::pending(),
    })
    .unwrap_err();

    assert!(matches!(err, ImportError::UnreviewedImportPlan));
}

fn write_stack(path: &Path, channel: u32, timepoint: u32) -> Result<(), tiff::TiffError> {
    write_stack_with_dimensions(path, channel, timepoint, 3, 2, 2)
}

fn write_u8_stack(path: &Path) -> Result<(), tiff::TiffError> {
    let file = File::create(path)?;
    let mut encoder = TiffEncoder::new(file)?;
    for z in 0..2 {
        let values = (0..2)
            .flat_map(|y| (0..3).map(move |x| (z * 10 + y * 3 + x) as u8))
            .collect::<Vec<_>>();
        encoder.write_image::<colortype::Gray8>(3, 2, &values)?;
    }
    Ok(())
}

fn write_u8_stack_with_no_data_corner(path: &Path) -> Result<(), tiff::TiffError> {
    let file = File::create(path)?;
    let mut encoder = TiffEncoder::new(file)?;
    for z in 0..3 {
        let values = (0..3)
            .flat_map(|y| {
                (0..3).map(move |x| {
                    if z == 0 && y == 0 && x == 0 {
                        255
                    } else {
                        (z * 9 + y * 3 + x) as u8
                    }
                })
            })
            .collect::<Vec<_>>();
        encoder.write_image::<colortype::Gray8>(3, 3, &values)?;
    }
    Ok(())
}

fn write_u8_plane(path: &Path, base: u8) -> Result<(), tiff::TiffError> {
    let file = File::create(path)?;
    let mut encoder = TiffEncoder::new(file)?;
    let values = (0..2)
        .flat_map(|y| (0..3).map(move |x| base + (y * 3 + x) as u8))
        .collect::<Vec<_>>();
    encoder.write_image::<colortype::Gray8>(3, 2, &values)?;
    Ok(())
}

fn write_u8_plane_values(
    path: &Path,
    width: u32,
    height: u32,
    values: &[u8],
) -> Result<(), tiff::TiffError> {
    let file = File::create(path)?;
    let mut encoder = TiffEncoder::new(file)?;
    encoder.write_image::<colortype::Gray8>(width, height, values)?;
    Ok(())
}

fn write_f32_stack(path: &Path) -> Result<(), tiff::TiffError> {
    let file = File::create(path)?;
    let mut encoder = TiffEncoder::new(file)?;
    let pages = [
        [-1.5, 0.0, 0.25, 1.0, 2.0, 3.0],
        [10.0, 11.5, 12.25, 13.0, 14.5, 15.75],
    ];
    for values in pages {
        encoder.write_image::<colortype::Gray32Float>(3, 2, &values)?;
    }
    Ok(())
}

fn write_u32_stack(path: &Path) -> Result<(), tiff::TiffError> {
    let file = File::create(path)?;
    let mut encoder = TiffEncoder::new(file)?;
    let values = [0_u32, 1, 2, 3, 4, 5];
    encoder.write_image::<colortype::Gray32>(3, 2, &values)?;
    Ok(())
}

fn write_striped_stack(path: &Path) -> Result<(), tiff::TiffError> {
    let file = File::create(path)?;
    let mut encoder = TiffEncoder::new(file)?;
    for z in 0..2 {
        let values = (0..4)
            .flat_map(|y| (0..4).map(move |x| (z * 100 + y * 10 + x) as u16))
            .collect::<Vec<_>>();
        let mut image = encoder.new_image::<colortype::Gray16>(4, 4)?;
        image.rows_per_strip(1)?;
        image.write_data(&values)?;
    }
    Ok(())
}

fn write_zero_stack(
    path: &Path,
    width: u32,
    height: u32,
    depth: u32,
) -> Result<(), tiff::TiffError> {
    let file = File::create(path)?;
    let mut encoder = TiffEncoder::new(file)?;
    let values = vec![0u16; (width * height) as usize];
    for _ in 0..depth {
        encoder.write_image::<colortype::Gray16>(width, height, &values)?;
    }
    Ok(())
}

fn write_ome_stack(
    path: &Path,
    physical_size: [f64; 3],
    physical_unit: [&str; 3],
) -> Result<(), tiff::TiffError> {
    let file = File::create(path)?;
    let mut encoder = TiffEncoder::new(file)?;
    let ome_xml = format!(
        r#"<OME xmlns="http://www.openmicroscopy.org/Schemas/OME/2016-06"><Image ID="Image:0"><Pixels ID="Pixels:0" DimensionOrder="XYZCT" Type="uint16" SizeX="3" SizeY="2" SizeZ="2" SizeC="1" SizeT="1" PhysicalSizeX="{}" PhysicalSizeXUnit="{}" PhysicalSizeY="{}" PhysicalSizeYUnit="{}" PhysicalSizeZ="{}" PhysicalSizeZUnit="{}"><Channel ID="Channel:0:0" SamplesPerPixel="1"/></Pixels></Image></OME>"#,
        physical_size[0],
        physical_unit[0],
        physical_size[1],
        physical_unit[1],
        physical_size[2],
        physical_unit[2]
    );
    for z in 0..2 {
        let values = (0..2)
            .flat_map(|y| (0..3).map(move |x| (z * 10 + y * 3 + x) as u16))
            .collect::<Vec<_>>();
        let mut image = encoder.new_image::<colortype::Gray16>(3, 2)?;
        if z == 0 {
            image
                .encoder()
                .write_tag(Tag::ImageDescription, ome_xml.as_str())?;
        }
        image.write_data(&values)?;
    }
    Ok(())
}

fn write_stack_with_dimensions(
    path: &Path,
    channel: u32,
    timepoint: u32,
    width: u32,
    height: u32,
    depth: u32,
) -> Result<(), tiff::TiffError> {
    let file = File::create(path)?;
    let mut encoder = TiffEncoder::new(file)?;
    for z in 0..depth {
        let values = (0..height)
            .flat_map(|y| {
                (0..width)
                    .map(move |x| (channel * 1000 + timepoint * 100 + z * 10 + y * 3 + x) as u16)
            })
            .collect::<Vec<_>>();
        encoder.write_image::<colortype::Gray16>(width, height, &values)?;
    }
    Ok(())
}
