use std::{
    fs,
    path::{Path, PathBuf},
};

use mirante4d_dataset::{CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError};
use mirante4d_import_pipeline::{
    ImportCancellation, ImportEvent, ImportOptions, ImportStage, NoDataPolicy, NoDataValueRule,
    SpatialCalibration, TiffChannelSource, TiffSource, import_tiff, inspect_tiff,
    inspect_tiff_cancellable,
};
use mirante4d_storage::{OmeLevelTransform, PackagePath, PackedIndexCoordinates, ProfileKind};
use tiff::encoder::{TiffEncoder, colortype};

const SOURCE_ARCHIVE: &[u8] =
    include_bytes!("../../../fixtures/source/mirante4d-source-tiff-fixtures-v1.tar");
const WORKING_MEMORY_BYTES: u64 = 192 * 1024 * 1024;

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

#[derive(Default)]
struct TestLedger;

impl CpuByteLedger for TestLedger {
    fn try_acquire(
        &self,
        category: CpuLedgerCategory,
        bytes: u64,
    ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
        assert_eq!(category, CpuLedgerCategory::ImportWorkingSet);
        assert!(bytes > 0);
        Ok(Box::new(TestLease { bytes }))
    }
}

#[test]
fn cancellable_inspection_stops_before_source_work() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.tif");
    fs::write(
        &source,
        ustar_regular_file(SOURCE_ARCHIVE, "spec-004/u8-no-data-corner.tif"),
    )
    .unwrap();
    let cancellation = ImportCancellation::new();
    cancellation.cancel();

    assert!(matches!(
        inspect_tiff_cancellable(TiffSource::single_3d(source), &cancellation),
        Err(mirante4d_import_pipeline::ImportError::Cancelled)
    ));
}

#[test]
fn promoted_uint8_uint16_and_float32_sources_publish_valid_packages() {
    let cases = [
        (
            "spec-001/ome-u16-anisotropic.ome.tif",
            None,
            "ome-u16-anisotropic.ome.tif",
        ),
        (
            "spec-004/u8-no-data-corner.tif",
            Some(NoDataPolicy::manual_uint8(255)),
            "u8-no-data-corner.tif",
        ),
        ("spec-004/f32-finite.tif", None, "f32-finite.tif"),
    ];

    for (ordinal, (archive_path, no_data, file_name)) in cases.into_iter().enumerate() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join(file_name);
        fs::write(&source, ustar_regular_file(SOURCE_ARCHIVE, archive_path)).unwrap();
        let source_before = fs::read(&source).unwrap();
        let inspection = inspect_tiff(TiffSource::single_3d(&source)).unwrap();
        let canonical_pixel_bytes = inspection.shape.dimensions().into_iter().product::<u64>()
            * u64::from(inspection.channels)
            * u64::from(inspection.dtype.bytes_per_sample());
        let spacing = inspection.ome_spacing_zyx_um.unwrap_or([1.0; 3]);
        let destination = root.path().join(format!("case-{ordinal}.m4d"));
        let checkpoint = root.path().join(format!("case-{ordinal}.checkpoint"));
        let mut events = Vec::new();
        let published = import_tiff(
            ImportOptions {
                inspection,
                destination: destination.clone(),
                checkpoint_directory: checkpoint.clone(),
                profile: ProfileKind::Current,
                calibration: SpatialCalibration::new(spacing),
                time_step_seconds: None,
                no_data,
            },
            &TestLedger,
            &ImportCancellation::new(),
            |event| events.push(event),
        )
        .unwrap();
        assert_eq!(published.destination(), destination);
        let receipt = published.receipt();

        assert_eq!(fs::read(&source).unwrap(), source_before);
        assert!(!checkpoint.exists());
        assert_eq!(events.last(), Some(&ImportEvent::Published));
        assert!(receipt.statistics.produced_work_units > 0);
        assert!(receipt.statistics.peak_working_bytes <= WORKING_MEMORY_BYTES);
        assert_eq!(
            receipt.statistics.native_decoded_bytes,
            receipt.statistics.base_native_decoded_bytes
                + receipt.statistics.no_data_detection_native_decoded_bytes
                + receipt.statistics.scientific_identity_native_decoded_bytes
        );
        assert_eq!(
            receipt.statistics.source_revalidation_bytes_read, 0,
            "descriptor-generation revalidation must not reread the TIFF payload"
        );
        assert_eq!(
            receipt.statistics.base_native_decoded_bytes,
            canonical_pixel_bytes
        );
        assert_eq!(
            receipt.statistics.scientific_identity_native_decoded_bytes,
            0
        );
        assert!(
            receipt.statistics.source_bytes_read
                >= receipt.statistics.source_revalidation_bytes_read
        );
        assert_eq!(receipt.statistics.tiff_open_count, 1);
        assert!(receipt.statistics.native_chunk_decode_count > 0);
        assert!(receipt.statistics.staged_structure_object_reads > 0);
        assert!(receipt.statistics.staged_exact_object_reads > 0);
        assert!(receipt.statistics.scientific_object_reads > 0);
        assert!(
            receipt.statistics.scientific_object_reads
                >= receipt.statistics.scientific_payload_object_reads
        );
        assert_eq!(
            receipt.statistics.object_reads,
            receipt.statistics.staged_structure_object_reads
                + receipt.statistics.staged_exact_object_reads
                + receipt.statistics.scientific_object_reads
        );
        assert!(receipt.statistics.codec_encode_calls > 0);
        assert!(receipt.statistics.codec_encode_time_ns > 0);
        assert!(receipt.statistics.codec_decode_calls > receipt.statistics.scientific_brick_reads);
        assert!(receipt.statistics.codec_decode_time_ns > 0);
        assert_eq!(receipt.statistics.open_file_descriptor_structural_bound, 39);
        assert_eq!(
            receipt.statistics.peak_open_file_descriptors,
            receipt
                .statistics
                .sampled_peak_open_file_descriptors
                .max(receipt.statistics.open_file_descriptor_structural_bound)
        );
        assert!(receipt.statistics.peak_open_file_descriptors <= 64);
        assert!(receipt.statistics.peak_checkpoint_regular_files > 0);
        assert!(receipt.statistics.peak_checkpoint_regular_files <= 18);
        assert!(receipt.statistics.preflight_required_headroom_bytes > 0);
        assert!(receipt.statistics.primary_wall_time_ns > 0);
        assert!(receipt.statistics.primary_cpu_time_ns > 0);
        assert!(receipt.statistics.stages.iter().any(|timing| {
            timing.stage == ImportStage::BaseProduction && timing.wall_time_ns > 0
        }));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ImportEvent::StageFinished(_)))
                .count(),
            receipt.statistics.stages.len()
        );
        let (receipt, transfer) = published.into_parts();
        let (self_consistent, _) = transfer.consume(|| false).unwrap();
        assert_eq!(self_consistent.package_id(), receipt.package_id);
        assert_eq!(
            self_consistent.scientific_content_id(),
            receipt.scientific_content_id
        );
    }
}

#[test]
fn automatic_no_match_publishes_the_ordinary_all_valid_route() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("no-match.tif");
    let file = fs::File::create(&source).unwrap();
    let mut encoder = TiffEncoder::new(file).unwrap();
    for z in 0_u8..4 {
        let values = (0_u8..16)
            .map(|index| index.wrapping_add(z.wrapping_mul(17)))
            .collect::<Vec<_>>();
        encoder
            .write_image::<colortype::Gray8>(4, 4, &values)
            .unwrap();
    }
    let inspection = inspect_tiff(TiffSource::single_3d(&source)).unwrap();
    let published = import_tiff(
        ImportOptions {
            inspection,
            destination: root.path().join("no-match.m4d"),
            checkpoint_directory: root.path().join("no-match.checkpoint"),
            profile: ProfileKind::Current,
            calibration: SpatialCalibration::new([1.0; 3]),
            time_step_seconds: None,
            no_data: Some(NoDataPolicy::automatic()),
        },
        &TestLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        published
            .receipt()
            .statistics
            .no_data_detection_native_decoded_bytes,
        0,
        "automatic detection must reuse the canonical ingest cache"
    );
    assert!(published.receipt().statistics.base_native_decoded_bytes > 0);
    let (_, transfer) = published.into_parts();
    let (package, _) = transfer.consume(|| false).unwrap();
    let brick = package
        .read_brick(PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0), || false)
        .unwrap();
    assert!(!brick.record().explicit_validity());
    assert!(brick.validity_payload().is_none());
}

#[test]
fn automatic_value_and_constant_planes_apply_first_volume_facts_dataset_wide() {
    const Z: usize = 6;
    const Y: usize = 8;
    const X: usize = 8;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("automatic-series");
    fs::create_dir(&source).unwrap();
    for timepoint in 0..2 {
        let mut values = vec![0_u8; Z * Y * X];
        for z in 0..Z {
            for y in 0..Y {
                for x in 0..X {
                    values[(z * Y + y) * X + x] =
                        u8::try_from(1 + (z * 31 + y * 7 + x * 3 + timepoint) % 180).unwrap();
                }
            }
        }
        if timepoint == 0 {
            values[..Y * X].fill(9);
            for z in 1..Z {
                for y in 0..5 {
                    for x in 0..5 {
                        values[(z * Y + y) * X + x] = 211;
                    }
                }
            }
            // This exact-value voxel is disconnected from every qualifying
            // 5-cube and therefore is scientific data, not automatic mask.
            values[(5 * Y + 7) * X + 7] = 211;
        } else {
            // The first-volume plane map still hides this nonconstant plane.
            values[(5 * Y + 7) * X + 7] = 211;
        }
        let file = fs::File::create(source.join(format!("sample_time{timepoint}.tif"))).unwrap();
        let mut encoder = TiffEncoder::new(file).unwrap();
        for plane in values.chunks_exact(Y * X) {
            encoder
                .write_image::<colortype::Gray8>(X as u32, Y as u32, plane)
                .unwrap();
        }
    }

    let inspection = inspect_tiff(folder_of_3d_source(&source)).unwrap();
    let published = import_tiff(
        ImportOptions {
            inspection,
            destination: root.path().join("automatic-series.m4d"),
            checkpoint_directory: root.path().join("automatic-series.checkpoint"),
            profile: ProfileKind::Current,
            calibration: SpatialCalibration::new([1.0; 3]),
            time_step_seconds: Some(1.0),
            no_data: Some(NoDataPolicy::new(Some(NoDataValueRule::Automatic), true)),
        },
        &TestLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();
    let (_, transfer) = published.into_parts();
    let (package, _) = transfer.consume(|| false).unwrap();
    for timepoint in 0..2 {
        let brick = package
            .read_brick(
                PackedIndexCoordinates::new(0, 0, timepoint, 0, 0, 0, 0),
                || false,
            )
            .unwrap();
        let validity = brick.validity_payload().unwrap();
        let is_valid = |z: usize, y: usize, x: usize| {
            let padded = (z * 64 + y) * 64 + x;
            validity[padded / 8] & (1 << (padded % 8)) != 0
        };
        assert!((0..Y).all(|y| (0..X).all(|x| !is_valid(0, y, x))));
        assert!(is_valid(1, 7, 7), "constant-plane masking leaked into z=1");
        assert!(
            !is_valid(3, 2, 2),
            "the fixed first-volume spatial mask was not reused dataset-wide"
        );
        assert!(
            is_valid(5, 7, 7),
            "a disconnected exact-value data voxel was globally reclassified"
        );
    }
    let later = package
        .read_brick(PackedIndexCoordinates::new(0, 0, 1, 0, 0, 0, 0), || false)
        .unwrap();
    let validity = later.validity_payload().unwrap();
    let sentinel_index = (5 * 64 + 7) * 64 + 7;
    assert_eq!(
        validity[sentinel_index / 8] & (1 << (sentinel_index % 8)),
        1 << (sentinel_index % 8)
    );
}

#[test]
fn automatic_float32_value_produces_explicit_typed_validity() {
    const SIDE: usize = 8;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("automatic-f32.tif");
    let mut values = (0..SIDE * SIDE * SIDE)
        .map(|index| index as f32 + 0.125)
        .collect::<Vec<_>>();
    for z in 0..5 {
        for y in 0..5 {
            for x in 0..5 {
                values[(z * SIDE + y) * SIDE + x] = 17.75;
            }
        }
    }
    let file = fs::File::create(&source).unwrap();
    let mut encoder = TiffEncoder::new(file).unwrap();
    for plane in values.chunks_exact(SIDE * SIDE) {
        encoder
            .write_image::<colortype::Gray32Float>(SIDE as u32, SIDE as u32, plane)
            .unwrap();
    }
    let inspection = inspect_tiff(TiffSource::single_3d(&source)).unwrap();
    let published = import_tiff(
        ImportOptions {
            inspection,
            destination: root.path().join("automatic-f32.m4d"),
            checkpoint_directory: root.path().join("automatic-f32.checkpoint"),
            profile: ProfileKind::Current,
            calibration: SpatialCalibration::new([1.0; 3]),
            time_step_seconds: None,
            no_data: Some(NoDataPolicy::automatic()),
        },
        &TestLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();
    let (_, transfer) = published.into_parts();
    let (package, _) = transfer.consume(|| false).unwrap();
    let brick = package
        .read_brick(PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0), || false)
        .unwrap();
    assert!(brick.record().explicit_validity());
    let validity = brick.validity_payload().unwrap();
    assert_eq!(validity[0] & 1, 0);
    let valid_index = (7 * 64 + 7) * 64 + 7;
    assert_ne!(validity[valid_index / 8] & (1 << (valid_index % 8)), 0);
    let byte = valid_index * 4;
    let pixels = brick.pixel_payload().unwrap();
    assert_eq!(
        f32::from_le_bytes(pixels[byte..byte + 4].try_into().unwrap()),
        values[(7 * SIDE + 7) * SIDE + 7]
    );
}

#[test]
fn automatic_spatial_reconstruction_keeps_all_seeded_components_and_excludes_dust() {
    const SIDE: usize = 16;
    const CVAL: u16 = 60_000;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("automatic-connected-components.tif");
    let mut values = (0..SIDE * SIDE * SIDE)
        .map(|index| u16::try_from(index + 1).unwrap())
        .collect::<Vec<_>>();
    let index = |z: usize, y: usize, x: usize| (z * SIDE + y) * SIDE + x;

    // The first component contains the detector's earliest 5-cube and a thin
    // face-connected tail which must be retained by reconstruction.
    for z in 0..5 {
        for y in 0..5 {
            for x in 0..5 {
                values[index(z, y, x)] = CVAL;
            }
        }
    }
    for x in 5..=8 {
        values[index(4, 4, x)] = CVAL;
    }

    // A later disconnected component has its own 5-cube. The complete second
    // pass must seed it too; stopping after the first component is incorrect.
    for z in 10..15 {
        for y in 10..15 {
            for x in 10..15 {
                values[index(z, y, x)] = CVAL;
            }
        }
    }

    // This diagonal-only chain shares the cval but has no face connection and
    // no 5-cube. Its far end also lies outside the one-voxel rendering guard.
    for coordinate in 5..=7 {
        values[index(coordinate, coordinate, coordinate)] = CVAL;
    }

    let file = fs::File::create(&source).unwrap();
    let mut encoder = TiffEncoder::new(file).unwrap();
    for plane in values.chunks_exact(SIDE * SIDE) {
        encoder
            .write_image::<colortype::Gray16>(SIDE as u32, SIDE as u32, plane)
            .unwrap();
    }
    let inspection = inspect_tiff(TiffSource::single_3d(&source)).unwrap();
    let published = import_tiff(
        ImportOptions {
            inspection,
            destination: root.path().join("automatic-connected-components.m4d"),
            checkpoint_directory: root
                .path()
                .join("automatic-connected-components.checkpoint"),
            profile: ProfileKind::Current,
            calibration: SpatialCalibration::new([1.0; 3]),
            time_step_seconds: None,
            no_data: Some(NoDataPolicy::automatic()),
        },
        &TestLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();
    let (_, transfer) = published.into_parts();
    let (package, _) = transfer.consume(|| false).unwrap();
    let brick = package
        .read_brick(PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0), || false)
        .unwrap();
    let validity = brick.validity_payload().unwrap();
    let is_valid = |z: usize, y: usize, x: usize| {
        let padded = (z * 64 + y) * 64 + x;
        validity[padded / 8] & (1 << (padded % 8)) != 0
    };
    assert!(
        !is_valid(4, 4, 8),
        "face-connected tail was not reconstructed"
    );
    assert!(!is_valid(12, 12, 12), "later seeded component was omitted");
    assert!(
        is_valid(7, 7, 7),
        "diagonal-only exact-value dust was incorrectly connected"
    );
}

#[test]
fn identical_source_bytes_produce_the_same_exact_package_id() {
    let root = tempfile::tempdir().unwrap();
    let bytes = ustar_regular_file(SOURCE_ARCHIVE, "spec-001/ome-u16-anisotropic.ome.tif");
    let mut package_ids = Vec::new();
    let mut scientific_ids = Vec::new();
    for run in 0..2 {
        let run_root = root.path().join(format!("run-{run}"));
        fs::create_dir(&run_root).unwrap();
        let source = run_root.join(if run == 0 {
            "source.ome.tif"
        } else {
            "renamed-copy.ome.tif"
        });
        fs::write(&source, bytes).unwrap();
        let inspection = inspect_tiff(TiffSource::single_3d(&source)).unwrap();
        let published = import_tiff(
            ImportOptions {
                calibration: SpatialCalibration::new(inspection.ome_spacing_zyx_um.unwrap()),
                inspection,
                destination: run_root.join("dataset.m4d"),
                checkpoint_directory: run_root.join("checkpoint"),
                profile: ProfileKind::Current,
                time_step_seconds: None,
                no_data: None,
            },
            &TestLedger,
            &ImportCancellation::new(),
            |_| {},
        )
        .unwrap();
        let receipt = published.receipt();
        package_ids.push(receipt.package_id);
        scientific_ids.push(receipt.scientific_content_id);
    }
    assert_eq!(package_ids[0], package_ids[1]);
    assert_eq!(scientific_ids[0], scientific_ids[1]);
}

#[test]
fn cancellation_keeps_one_checkpoint_and_resume_finishes_without_partial_destination() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    for time in 0..3 {
        let name = format!("stack-t{time:03}.tif");
        let archive_path = format!("spec-002/{name}");
        fs::write(
            source.join(name),
            ustar_regular_file(SOURCE_ARCHIVE, &archive_path),
        )
        .unwrap();
    }
    let source_before = directory_bytes(&source);
    let inspection = inspect_tiff(folder_of_3d_source(&source)).unwrap();
    let decoded_dataset_bytes = inspection.shape.dimensions().into_iter().product::<u64>()
        * u64::from(inspection.channels)
        * u64::from(inspection.dtype.bytes_per_sample());
    let destination = root.path().join("resumed.m4d");
    let checkpoint = root.path().join("resumed.checkpoint");
    let options = ImportOptions {
        inspection,
        destination: destination.clone(),
        checkpoint_directory: checkpoint.clone(),
        profile: ProfileKind::Current,
        calibration: SpatialCalibration::new([1.0; 3]),
        time_step_seconds: Some(1.0),
        no_data: None,
    };

    let cancellation = ImportCancellation::new();
    // Cancel after the ordered owner commits its first base work unit. This is
    // independent of how many byte leases the admitted source workers need.
    let cancellation_from_progress = cancellation.clone();
    let error = import_tiff(options.clone(), &TestLedger, &cancellation, move |event| {
        if matches!(
            event,
            ImportEvent::StageProgress {
                stage: ImportStage::BaseProduction,
                completed_work_units: 1,
                ..
            }
        ) {
            cancellation_from_progress.cancel();
        }
    })
    .unwrap_err();
    assert!(matches!(
        error,
        mirante4d_import_pipeline::ImportError::Cancelled
    ));
    assert!(!destination.exists());
    assert_eq!(directory_bytes(&source), source_before);
    let entries = fs::read_dir(&checkpoint)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(entries.contains(std::ffi::OsStr::new(".mirante4d-import-control")));
    assert!(!entries.contains(std::ffi::OsStr::new("payload")));

    let published = import_tiff(options, &TestLedger, &ImportCancellation::new(), |_| {}).unwrap();
    let receipt = published.receipt();
    // The cancelled run retains its complete current cache and safely
    // discards the speculative future cache while joining that worker.
    // Resume therefore skips the current source and decodes the other two.
    assert_eq!(
        receipt.statistics.base_native_decoded_bytes,
        decoded_dataset_bytes * 2 / 3
    );
    assert_eq!(receipt.statistics.maximum_temporal_pipeline_width, 2);
    assert_eq!(receipt.statistics.prefetch_units_admitted, 2);
    assert_eq!(receipt.statistics.prefetch_units_consumed, 2);
    assert_eq!(receipt.statistics.prefetch_cache_hits, 2);
    assert!(receipt.statistics.prefetch_ingest_busy_time_ns > 0);
    assert!(receipt.statistics.prefetch_overlap_time_ns > 0);
    assert!(destination.is_dir());
    assert!(!checkpoint.exists());
    assert_eq!(directory_bytes(&source), source_before);
}

#[test]
fn source_destination_and_checkpoint_must_be_separate_unnested_paths() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("plane.tif"),
        ustar_regular_file(SOURCE_ARCHIVE, "spec-004/u8-no-data-corner.tif"),
    )
    .unwrap();
    let source_before = directory_bytes(&source);
    let inspection = inspect_tiff(folder_of_3d_source(&source)).unwrap();

    let cases = [
        (
            source.join("nested-destination.m4d"),
            root.path().join("safe-checkpoint"),
        ),
        (
            root.path().join("safe-destination.m4d"),
            source.join("nested-checkpoint"),
        ),
    ];
    for (destination, checkpoint_directory) in cases {
        let error = import_tiff(
            ImportOptions {
                inspection: inspection.clone(),
                destination: destination.clone(),
                checkpoint_directory: checkpoint_directory.clone(),
                profile: ProfileKind::Current,
                calibration: SpatialCalibration::new([1.0; 3]),
                time_step_seconds: None,
                no_data: None,
            },
            &TestLedger,
            &ImportCancellation::new(),
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(
            error,
            mirante4d_import_pipeline::ImportError::InvalidRequest(_)
        ));
        assert!(!destination.exists());
        assert!(!checkpoint_directory.exists());
        assert_eq!(directory_bytes(&source), source_before);
    }
}

#[test]
fn multiscale_import_crosses_chunk_and_outer_shard_boundaries() {
    const WIDTH: u32 = 1_025;
    const HEIGHT: u32 = 257;

    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("wide.tif");
    let values = (0..HEIGHT)
        .flat_map(|y| (0..WIDTH).map(move |x| pattern(x, y)))
        .collect::<Vec<_>>();
    let file = fs::File::create(&source).unwrap();
    TiffEncoder::new(file)
        .unwrap()
        .write_image::<colortype::Gray8>(WIDTH, HEIGHT, &values)
        .unwrap();

    let inspection = inspect_tiff(TiffSource::single_3d(&source)).unwrap();
    let destination = root.path().join("wide.m4d");
    let published = import_tiff(
        ImportOptions {
            inspection,
            destination: destination.clone(),
            checkpoint_directory: root.path().join("wide.checkpoint"),
            profile: ProfileKind::Current,
            calibration: SpatialCalibration::new([1.0; 3]),
            time_step_seconds: Some(1.0),
            no_data: None,
        },
        &TestLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();
    let receipt = published.receipt();
    assert!(receipt.statistics.produced_work_units > 10);

    let (_receipt, transfer) = published.into_parts();
    let (self_consistent, _) = transfer.consume(|| false).unwrap();
    assert!(
        self_consistent.catalog().profile().images()[0]
            .levels()
            .len()
            > 1
    );
    let ome_path = PackagePath::parse("images/i00000000/zarr.json").unwrap();
    let OmeLevelTransform::DiagonalMicrometer {
        scale_zyx,
        translation_zyx,
    } = self_consistent
        .catalog()
        .ome_image(&ome_path)
        .unwrap()
        .level_transforms()[1]
    else {
        panic!("calibrated import must retain a diagonal OME transform");
    };
    assert_eq!(scale_zyx.map(|value| value.value()), [2.0; 3]);
    assert_eq!(translation_zyx.map(|value| value.value()), [0.0; 3]);

    let base_tail = self_consistent
        .read_brick(PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 4), || false)
        .unwrap();
    assert_eq!(base_tail.logical_extent_zyx(), [1, 256, 1]);
    assert_eq!(base_tail.pixel_payload().unwrap()[0], pattern(1_024, 0));

    let coarse = self_consistent
        .read_brick(PackedIndexCoordinates::new(0, 1, 0, 0, 0, 0, 0), || false)
        .unwrap();
    let coarse_pixels = coarse.pixel_payload().unwrap();
    for (x, y) in [(127_u32, 0_u32), (128, 64)] {
        let index = usize::try_from(y * 256 + x).unwrap();
        assert_eq!(coarse_pixels[index], pattern(x * 2, y * 2));
    }

    let coarse_tail = self_consistent
        .read_brick(PackedIndexCoordinates::new(0, 1, 0, 0, 0, 0, 2), || false)
        .unwrap();
    assert_eq!(coarse_tail.logical_extent_zyx(), [1, 129, 1]);
    assert_eq!(coarse_tail.pixel_payload().unwrap()[0], pattern(1_024, 0));
}

#[test]
fn explicit_channel_folders_with_unrelated_names_publish_one_multichannel_volume() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("channels");
    for channel in 0..2 {
        let folder = source.join(format!("channel{channel}"));
        fs::create_dir_all(&folder).unwrap();
        for z in 0..3 {
            let archive_path = format!("spec-003/channel-{channel:02}/z{z:03}.tif");
            let selected_name = if channel == 0 {
                format!("nucleus-alpha-{z:03}.tif")
            } else {
                format!("membrane-unrelated-{z:03}.tif")
            };
            fs::write(
                folder.join(selected_name),
                ustar_regular_file(SOURCE_ARCHIVE, &archive_path),
            )
            .unwrap();
        }
    }
    let source_before = directory_tree_bytes(&source);
    let source_manifest = TiffSource::new(vec![
        TiffChannelSource::folder_of_2d("nucleus", source.join("channel0")).unwrap(),
        TiffChannelSource::folder_of_2d("membrane", source.join("channel1")).unwrap(),
    ])
    .unwrap();
    let inspection = inspect_tiff(source_manifest).unwrap();
    assert_eq!(inspection.channels, 2);
    assert_eq!(inspection.shape.dimensions(), [1, 3, 3, 4]);

    let destination = root.path().join("channels.m4d");
    let published = import_tiff(
        ImportOptions {
            inspection,
            destination: destination.clone(),
            checkpoint_directory: root.path().join("channels.checkpoint"),
            profile: ProfileKind::Current,
            calibration: SpatialCalibration::new([1.0; 3]),
            time_step_seconds: None,
            no_data: None,
        },
        &TestLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();

    let (_receipt, transfer) = published.into_parts();
    let (self_consistent, _) = transfer.consume(|| false).unwrap();
    assert_eq!(self_consistent.catalog().science().layers().len(), 2);
    assert_eq!(
        self_consistent
            .catalog()
            .display_defaults()
            .layers()
            .iter()
            .map(|layer| layer.label())
            .collect::<Vec<_>>(),
        vec![Some("nucleus"), Some("membrane")],
        "user-authored channel labels must survive publication and reopen"
    );
    assert_eq!(directory_tree_bytes(&source), source_before);
}

fn pattern(x: u32, y: u32) -> u8 {
    u8::try_from((x + 3 * y) % 251).unwrap()
}

fn folder_of_3d_source(path: &Path) -> TiffSource {
    TiffSource::new(vec![
        TiffChannelSource::folder_of_3d("channel 1", path).unwrap(),
    ])
    .unwrap()
}

fn ustar_regular_file<'a>(archive: &'a [u8], expected_path: &str) -> &'a [u8] {
    let mut offset = 0_usize;
    while offset + 512 <= archive.len() {
        let header = &archive[offset..offset + 512];
        if header.iter().all(|byte| *byte == 0) {
            break;
        }
        let name_end = header[..100]
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(100);
        let name = std::str::from_utf8(&header[..name_end]).unwrap();
        let size_text = std::str::from_utf8(&header[124..136])
            .unwrap()
            .trim_matches(['\0', ' ']);
        let size = usize::from_str_radix(size_text, 8).unwrap();
        let data_start = offset + 512;
        let data_end = data_start + size;
        assert!(data_end <= archive.len());
        if name == expected_path {
            assert!(matches!(header[156], 0 | b'0'));
            return &archive[data_start..data_end];
        }
        offset = data_start + size.div_ceil(512) * 512;
    }
    panic!("fixture archive is missing {expected_path}");
}

fn directory_bytes(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .map(|path| {
            let name = PathBuf::from(path.file_name().unwrap());
            (name, fs::read(path).unwrap())
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

fn directory_tree_bytes(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn visit(root: &Path, directory: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.push((
                    path.strip_prefix(root).unwrap().to_owned(),
                    fs::read(path).unwrap(),
                ));
            }
        }
    }

    let mut files = Vec::new();
    visit(root, root, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}
