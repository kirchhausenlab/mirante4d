use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use mirante4d_dataset::{CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError};
use mirante4d_domain::{GridToWorld, IntensityDType, LogicalLayerKey, Shape4D};
use mirante4d_identity::{
    ScientificDatasetHasher, ScientificLayerDescriptor, ScientificLayerHasher,
    ScientificTemporalCalibration, ScientificTile,
};
use mirante4d_import_pipeline::{
    ImportCancellation, ImportEvent, ImportOptions, ImportStage, NoDataPolicy, NoDataValueRule,
    SpatialCalibration, TiffChannelSource, TiffSource, import_tiff, inspect_tiff,
    inspect_tiff_cancellable,
};
use mirante4d_storage::{OmeLevelTransform, PackagePath, PackedIndexCoordinates, ProfileKind};
use serde::Deserialize;
use tiff::encoder::{TiffEncoder, colortype};

const SOURCE_ARCHIVE: &[u8] =
    include_bytes!("../../../fixtures/source/mirante4d-source-tiff-fixtures-v2.tar");
const SOURCE_READER_REPORT: &[u8] =
    include_bytes!("../../../fixtures/source/independent-reader-report.json");
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

#[derive(Debug, Deserialize)]
struct SourceExpectedFacts {
    specifications: Vec<SourceFamilyFacts>,
}

#[derive(Debug, Deserialize)]
struct SourceFamilyFacts {
    id: String,
    shape_tczyx: Option<[u64; 5]>,
    calibration_xyz_um: Option<[f64; 3]>,
    files: Vec<SourceFileFacts>,
}

#[derive(Debug, Deserialize)]
struct SourceFileFacts {
    path: String,
    dtype: String,
    ifd_count: u64,
    width: u64,
    height: u64,
    byte_order: String,
    tiff_version: u16,
    compression: u16,
    storage_layout: String,
    expected_class: String,
    logical_value_sha256: String,
    logical_value_hex: String,
}

#[derive(Debug, Deserialize)]
struct IndependentReaderReport {
    status: String,
    files: Vec<IndependentReaderFile>,
}

#[derive(Debug, Deserialize)]
struct IndependentReaderFile {
    path: String,
    byte_order: String,
    tiff_version: u16,
    compression: u16,
    storage_layout: String,
    logical_value_sha256: String,
}

#[derive(Debug, Deserialize)]
struct BoundSourceMutations {
    recipes: Vec<BoundSourceMutation>,
}

#[derive(Debug, Deserialize)]
struct BoundSourceMutation {
    id: String,
    operation: String,
    base_path: String,
    replacement_path: Option<String>,
    expected_production_fault: String,
    byte_offset: Option<usize>,
    original_hex: Option<String>,
    replacement_hex: Option<String>,
    truncate_at: Option<usize>,
}

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
fn promoted_source_facts_drive_production_inspection_decode_and_publication() {
    let facts: SourceExpectedFacts = serde_json::from_slice(ustar_regular_file(
        SOURCE_ARCHIVE,
        "records/expected-facts.json",
    ))
    .unwrap();
    let reader: IndependentReaderReport = serde_json::from_slice(SOURCE_READER_REPORT).unwrap();
    assert_eq!(reader.status, "passed");
    assert_eq!(reader.files.len(), 21);
    let reader_by_path = reader
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<BTreeMap<_, _>>();

    for family in &facts.specifications {
        for file in &family.files {
            let observed = reader_by_path
                .get(file.path.as_str())
                .unwrap_or_else(|| panic!("independent reader omitted {}", file.path));
            assert_eq!(observed.byte_order, file.byte_order, "{}", file.path);
            assert_eq!(observed.tiff_version, file.tiff_version, "{}", file.path);
            assert_eq!(observed.compression, file.compression, "{}", file.path);
            assert_eq!(
                observed.storage_layout, file.storage_layout,
                "{}",
                file.path
            );
            assert_eq!(
                observed.logical_value_sha256, file.logical_value_sha256,
                "{}",
                file.path
            );

            let root = tempfile::tempdir().unwrap();
            let source = root.path().join(Path::new(&file.path).file_name().unwrap());
            fs::write(&source, ustar_regular_file(SOURCE_ARCHIVE, &file.path)).unwrap();
            let source_before = fs::read(&source).unwrap();
            let result = inspect_tiff(TiffSource::single_3d(&source));
            match file.expected_class.as_str() {
                "accepted" => {
                    let inspection = result.unwrap_or_else(|error| {
                        panic!("production inspection rejected {}: {error}", file.path)
                    });
                    assert_eq!(
                        inspection.shape.dimensions(),
                        [1, file.ifd_count, file.height, file.width],
                        "{}",
                        file.path
                    );
                    assert_eq!(inspection.channels, 1, "{}", file.path);
                    assert_eq!(
                        inspection.dtype,
                        dtype_from_fact(&file.dtype),
                        "{}",
                        file.path
                    );
                    let expected_spacing = family.calibration_xyz_um.map(|[x, y, z]| [z, y, x]);
                    assert_eq!(
                        inspection.ome_spacing_zyx_um, expected_spacing,
                        "{}",
                        file.path
                    );
                }
                "reject_unsupported_dtype" => {
                    assert!(
                        matches!(
                            &result,
                            Err(mirante4d_import_pipeline::ImportError::UnsupportedSource(_))
                                | Err(mirante4d_import_pipeline::ImportError::Tiff { .. })
                        ),
                        "production inspection returned the wrong rejection for {}: {result:?}",
                        file.path
                    );
                }
                "reject_nonfinite" => {
                    let inspection = result.expect("float32 metadata remains inspectable");
                    let destination = root.path().join("must-not-publish.m4d");
                    let error = import_tiff(
                        ImportOptions {
                            inspection,
                            destination: destination.clone(),
                            checkpoint_directory: root.path().join("nonfinite-checkpoint"),
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
                    assert!(
                        matches!(
                            error,
                            mirante4d_import_pipeline::ImportError::UnsupportedSource(_)
                                | mirante4d_import_pipeline::ImportError::Tiff { .. }
                                | mirante4d_import_pipeline::ImportError::Scientific(_)
                        ),
                        "production import returned the wrong nonfinite rejection for {}: {error}",
                        file.path
                    );
                    assert!(!destination.exists());
                }
                other => panic!("unhandled promoted source class {other:?}"),
            }
            assert_eq!(fs::read(&source).unwrap(), source_before, "{}", file.path);
        }
    }

    assert_grouped_source_facts(&facts);

    let publication_paths = [
        "spec-001/ome-u16-anisotropic.ome.tif",
        "spec-004/u8-no-data-corner.tif",
        "spec-004/f32-finite.tif",
        "spec-005/uncompressed-big-endian-u16.tif",
        "spec-005/lzw-u8-striped.tif",
        "spec-005/deflate-u8-tiled.tif",
        "spec-005/old-deflate-u8-striped.tif",
        "spec-005/packbits-u8-bigtiff.tif",
    ];
    for path in publication_paths {
        let (family, file) = find_source_fact(&facts, path);
        publish_and_match_independent_fact(family, file);
    }
}

#[test]
fn promoted_source_mutations_report_every_exact_production_rejection() {
    let mutations: BoundSourceMutations =
        serde_json::from_slice(ustar_regular_file(SOURCE_ARCHIVE, "records/mutations.json"))
            .unwrap();
    assert_eq!(mutations.recipes.len(), 8);
    let mut failures = Vec::new();
    for recipe in &mutations.recipes {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            exercise_source_mutation(recipe)
        }));
        match outcome {
            Ok(Ok(())) => eprintln!("M4D_SOURCE_MUTATION_PASS {}", recipe.id),
            Ok(Err(error)) => failures.push(format!("{}: {error}", recipe.id)),
            Err(payload) => {
                failures.push(format!("{}: panic: {}", recipe.id, panic_message(payload)))
            }
        }
    }
    assert!(
        failures.is_empty(),
        "promoted source mutation failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn representative_publications_report_bounded_resource_statistics() {
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
fn cancellation_matrix_resumes_to_one_identity_across_every_prepublication_stage() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("matrix-source.tif");
    let values = (0_u8..130)
        .map(|value| value.wrapping_add(1))
        .collect::<Vec<_>>();
    TiffEncoder::new(fs::File::create(&source).unwrap())
        .unwrap()
        .write_image::<colortype::Gray8>(65, 2, &values)
        .unwrap();
    let source_before = fs::read(&source).unwrap();
    let inspection = inspect_tiff(TiffSource::single_3d(&source)).unwrap();
    let options = |label: &str| ImportOptions {
        inspection: inspection.clone(),
        destination: root.path().join(format!("{label}.m4d")),
        checkpoint_directory: root.path().join(format!("{label}.checkpoint")),
        profile: ProfileKind::Current,
        calibration: SpatialCalibration::new([1.0; 3]),
        time_step_seconds: None,
        no_data: Some(NoDataPolicy::manual_uint8(255)),
    };
    let baseline = import_tiff(
        options("baseline"),
        &TestLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();

    let stages = [
        ImportStage::SourceIngest,
        ImportStage::NoDataDetection,
        ImportStage::BaseProduction,
        ImportStage::PyramidProduction { scale: 1 },
        ImportStage::ShardPublication,
        ImportStage::StagedStructureValidation,
        ImportStage::StagedExactValidation,
        ImportStage::StagedScientificValidation,
        ImportStage::Commit,
    ];
    for (ordinal, target) in stages.into_iter().enumerate() {
        let options = options(&format!("cancel-{ordinal}"));
        let cancellation = ImportCancellation::new();
        let callback_cancellation = cancellation.clone();
        let mut reached = false;
        let error = import_tiff(options.clone(), &TestLedger, &cancellation, |event| {
            if matches!(event, ImportEvent::StageStarted { stage, .. } if stage == target) {
                reached = true;
                callback_cancellation.cancel();
            }
        })
        .unwrap_err();
        assert!(reached, "stage {} was not observed", target.name());
        assert!(
            matches!(error, mirante4d_import_pipeline::ImportError::Cancelled),
            "stage {} returned {error}",
            target.name()
        );
        assert!(!options.destination.exists(), "{}", target.name());
        assert_eq!(
            fs::read(&source).unwrap(),
            source_before,
            "{}",
            target.name()
        );

        let resumed = import_tiff(
            options.clone(),
            &TestLedger,
            &ImportCancellation::new(),
            |_| {},
        )
        .unwrap();
        assert_eq!(resumed.receipt().package_id, baseline.receipt().package_id);
        assert_eq!(
            resumed.receipt().scientific_content_id,
            baseline.receipt().scientific_content_id
        );
        assert!(options.destination.is_dir());
        assert!(!options.checkpoint_directory.exists());
        assert_eq!(
            fs::read(&source).unwrap(),
            source_before,
            "{}",
            target.name()
        );
    }
}

#[test]
#[ignore = "registered developer-local subprocess checkpoint recovery boundary"]
fn process_checkpoint_recovery_survives_forced_worker_termination() {
    const ROLE: &str = "M4D_IMPORT_PROCESS_RECOVERY_ROLE";
    const ROOT: &str = "M4D_IMPORT_PROCESS_RECOVERY_ROOT";
    const MARKER: &str = "M4D_IMPORT_DURABLE_CHECKPOINT_READY";

    if std::env::var_os(ROLE).as_deref() == Some(std::ffi::OsStr::new("child")) {
        let root = PathBuf::from(std::env::var_os(ROOT).expect("child recovery root"));
        let source = root.join("source");
        let inspection = inspect_tiff(folder_of_3d_source(&source)).unwrap();
        let cancellation = ImportCancellation::new();
        let _ = import_tiff(
            ImportOptions {
                inspection,
                destination: root.join("recovered.m4d"),
                checkpoint_directory: root.join("recovered.checkpoint"),
                profile: ProfileKind::Current,
                calibration: SpatialCalibration::new([1.0; 3]),
                time_step_seconds: Some(1.0),
                no_data: None,
            },
            &TestLedger,
            &cancellation,
            |event| {
                if matches!(
                    event,
                    ImportEvent::StageProgress {
                        stage: ImportStage::BaseProduction,
                        completed_work_units: 1,
                        ..
                    }
                ) {
                    println!("{MARKER}");
                    use std::io::Write as _;
                    std::io::stdout().flush().unwrap();
                    loop {
                        std::thread::park();
                    }
                }
            },
        );
        panic!("process-recovery child unexpectedly left its kill boundary");
    }

    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    for time in 0..3 {
        let name = format!("stack-t{time:03}.tif");
        fs::write(
            source.join(&name),
            ustar_regular_file(SOURCE_ARCHIVE, &format!("spec-002/{name}")),
        )
        .unwrap();
    }
    let source_before = directory_bytes(&source);
    let executable = std::env::current_exe().unwrap();
    let mut child = std::process::Command::new(executable)
        .args([
            "process_checkpoint_recovery_survives_forced_worker_termination",
            "--ignored",
            "--exact",
            "--nocapture",
        ])
        .env(ROLE, "child")
        .env(ROOT, root.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead as _;
        for line in std::io::BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if line.contains(MARKER) {
                let _ = sender.send(());
                break;
            }
        }
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("child reached its durable checkpoint boundary");
    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert!(!status.success());
    reader.join().unwrap();

    let destination = root.path().join("recovered.m4d");
    let checkpoint = root.path().join("recovered.checkpoint");
    assert!(!destination.exists());
    assert!(checkpoint.is_dir());
    assert_eq!(directory_bytes(&source), source_before);
    let inspection = inspect_tiff(folder_of_3d_source(&source)).unwrap();
    let resumed = import_tiff(
        ImportOptions {
            inspection: inspection.clone(),
            destination: destination.clone(),
            checkpoint_directory: checkpoint,
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
    let control = import_tiff(
        ImportOptions {
            inspection,
            destination: root.path().join("control.m4d"),
            checkpoint_directory: root.path().join("control.checkpoint"),
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
    assert_eq!(resumed.receipt().package_id, control.receipt().package_id);
    assert_eq!(
        resumed.receipt().scientific_content_id,
        control.receipt().scientific_content_id
    );
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

fn exercise_source_mutation(recipe: &BoundSourceMutation) -> Result<(), String> {
    match recipe.operation.as_str() {
        "remove_group_member" | "duplicate_group_member" => exercise_group_count_mutation(recipe),
        "replace_with_multipage_file" => exercise_plane_series_mutation(recipe),
        _ => exercise_byte_source_mutation(recipe),
    }
}

fn exercise_byte_source_mutation(recipe: &BoundSourceMutation) -> Result<(), String> {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let source = root
        .path()
        .join(Path::new(&recipe.base_path).file_name().unwrap());
    let base = ustar_regular_file(SOURCE_ARCHIVE, &recipe.base_path);
    let mutated = match recipe.operation.as_str() {
        "replace_bytes" | "replace_ome_sizez" => {
            let offset = recipe.byte_offset.ok_or("missing byte offset")?;
            let original = decode_hex(recipe.original_hex.as_deref().ok_or("missing original")?);
            let replacement = decode_hex(
                recipe
                    .replacement_hex
                    .as_deref()
                    .ok_or("missing replacement")?,
            );
            if base.get(offset..offset + original.len()) != Some(original.as_slice()) {
                return Err("bound mutation original bytes differ".to_owned());
            }
            let mut bytes = base.to_vec();
            bytes.splice(offset..offset + original.len(), replacement);
            bytes
        }
        "truncate_header" | "truncate_ifd" | "truncate_strip_data" => {
            base[..recipe.truncate_at.ok_or("missing truncation offset")?].to_vec()
        }
        other => return Err(format!("unexpected byte operation {other:?}")),
    };
    fs::write(&source, &mutated).map_err(|error| error.to_string())?;
    let source_before = fs::read(&source).map_err(|error| error.to_string())?;
    let inspection = inspect_tiff(TiffSource::single_3d(&source));
    let error = match inspection {
        Err(error) => error,
        Ok(inspection) => {
            let destination = root.path().join("must-not-publish.m4d");
            let result = import_tiff(
                ImportOptions {
                    calibration: SpatialCalibration::new(
                        inspection.ome_spacing_zyx_um.unwrap_or([1.0; 3]),
                    ),
                    inspection,
                    destination: destination.clone(),
                    checkpoint_directory: root.path().join("checkpoint"),
                    profile: ProfileKind::Current,
                    time_step_seconds: None,
                    no_data: None,
                },
                &TestLedger,
                &ImportCancellation::new(),
                |_| {},
            );
            if destination.exists() {
                return Err("mutated source published a destination".to_owned());
            }
            match result {
                Ok(_) => return Err("mutated source was accepted".to_owned()),
                Err(error) => error,
            }
        }
    };
    require_source_fault(recipe, &error)?;
    if fs::read(&source).map_err(|error| error.to_string())? != source_before {
        return Err("production changed the mutated source bytes".to_owned());
    }
    Ok(())
}

fn exercise_group_count_mutation(recipe: &BoundSourceMutation) -> Result<(), String> {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let reference = root.path().join("reference");
    let mutated = root.path().join("mutated");
    fs::create_dir(&reference).map_err(|error| error.to_string())?;
    fs::create_dir(&mutated).map_err(|error| error.to_string())?;
    for time in 0..3 {
        let name = format!("stack-t{time:03}.tif");
        let archive_path = format!("spec-002/{name}");
        let bytes = ustar_regular_file(SOURCE_ARCHIVE, &archive_path);
        fs::write(reference.join(&name), bytes).map_err(|error| error.to_string())?;
        if recipe.operation != "remove_group_member" || archive_path != recipe.base_path {
            fs::write(mutated.join(&name), bytes).map_err(|error| error.to_string())?;
        }
        if recipe.operation == "duplicate_group_member" && archive_path == recipe.base_path {
            fs::write(mutated.join("stack-t001-duplicate.tif"), bytes)
                .map_err(|error| error.to_string())?;
        }
    }
    let before = directory_tree_bytes(root.path());
    let error = inspect_tiff(
        TiffSource::new(vec![
            TiffChannelSource::folder_of_3d("reference", reference).unwrap(),
            TiffChannelSource::folder_of_3d("mutated", mutated).unwrap(),
        ])
        .unwrap(),
    )
    .expect_err("mismatched channel timepoint counts must fail");
    require_source_fault(recipe, &error)?;
    if directory_tree_bytes(root.path()) != before {
        return Err("production changed grouped source bytes".to_owned());
    }
    Ok(())
}

fn exercise_plane_series_mutation(recipe: &BoundSourceMutation) -> Result<(), String> {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    for z in 0..3 {
        let path = format!("spec-003/channel-00/z{z:03}.tif");
        let bytes = if path == recipe.base_path {
            ustar_regular_file(
                SOURCE_ARCHIVE,
                recipe
                    .replacement_path
                    .as_deref()
                    .ok_or("missing replacement path")?,
            )
        } else {
            ustar_regular_file(SOURCE_ARCHIVE, &path)
        };
        fs::write(root.path().join(format!("z{z:03}.tif")), bytes)
            .map_err(|error| error.to_string())?;
    }
    let before = directory_tree_bytes(root.path());
    let error = inspect_tiff(
        TiffSource::new(vec![
            TiffChannelSource::folder_of_2d("channel", root.path()).unwrap(),
        ])
        .unwrap(),
    )
    .expect_err("multipage plane-series member must fail");
    require_source_fault(recipe, &error)?;
    if directory_tree_bytes(root.path()) != before {
        return Err("production changed plane-series source bytes".to_owned());
    }
    Ok(())
}

fn require_source_fault(
    recipe: &BoundSourceMutation,
    error: &mirante4d_import_pipeline::ImportError,
) -> Result<(), String> {
    let actual = match error {
        mirante4d_import_pipeline::ImportError::Tiff { .. } => "tiff",
        mirante4d_import_pipeline::ImportError::UnsupportedSource(_) => "unsupported_source",
        mirante4d_import_pipeline::ImportError::Io { source, .. }
            if source.kind() == std::io::ErrorKind::UnexpectedEof =>
        {
            "io_unexpected_eof"
        }
        other => return Err(format!("unexpected production fault {other:?}")),
    };
    if actual != recipe.expected_production_fault {
        return Err(format!(
            "expected fault {}, found {actual}: {error}",
            recipe.expected_production_fault
        ));
    }
    Ok(())
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|message| (*message).to_owned())
        })
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

fn find_source_fact<'a>(
    facts: &'a SourceExpectedFacts,
    path: &str,
) -> (&'a SourceFamilyFacts, &'a SourceFileFacts) {
    facts
        .specifications
        .iter()
        .find_map(|family| {
            family
                .files
                .iter()
                .find(|file| file.path == path)
                .map(|file| (family, file))
        })
        .unwrap_or_else(|| panic!("expected facts omit {path}"))
}

fn dtype_from_fact(dtype: &str) -> IntensityDType {
    match dtype {
        "u8" => IntensityDType::Uint8,
        "u16" => IntensityDType::Uint16,
        "f32" => IntensityDType::Float32,
        other => panic!("unsupported test dtype {other:?}"),
    }
}

fn assert_grouped_source_facts(facts: &SourceExpectedFacts) {
    let time_family = facts
        .specifications
        .iter()
        .find(|family| family.id == "SRC-TIFF-SPEC-002")
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("time-series");
    fs::create_dir(&source).unwrap();
    for file in &time_family.files {
        fs::write(
            source.join(Path::new(&file.path).file_name().unwrap()),
            ustar_regular_file(SOURCE_ARCHIVE, &file.path),
        )
        .unwrap();
    }
    let before = directory_tree_bytes(&source);
    let inspection = inspect_tiff(folder_of_3d_source(&source)).unwrap();
    let [t, c, z, y, x] = time_family.shape_tczyx.unwrap();
    assert_eq!(c, 1);
    assert_eq!(inspection.shape.dimensions(), [t, z, y, x]);
    assert_eq!(inspection.dtype, IntensityDType::Uint16);
    assert_eq!(directory_tree_bytes(&source), before);

    let channel_family = facts
        .specifications
        .iter()
        .find(|family| family.id == "SRC-TIFF-SPEC-003")
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("channels");
    for channel in 0..2 {
        fs::create_dir_all(source.join(format!("channel-{channel:02}"))).unwrap();
    }
    for file in &channel_family.files {
        fs::write(
            source.join(file.path.strip_prefix("spec-003/").unwrap()),
            ustar_regular_file(SOURCE_ARCHIVE, &file.path),
        )
        .unwrap();
    }
    let before = directory_tree_bytes(&source);
    let inspection = inspect_tiff(
        TiffSource::new(vec![
            TiffChannelSource::folder_of_2d("channel 0", source.join("channel-00")).unwrap(),
            TiffChannelSource::folder_of_2d("channel 1", source.join("channel-01")).unwrap(),
        ])
        .unwrap(),
    )
    .unwrap();
    let [t, c, z, y, x] = channel_family.shape_tczyx.unwrap();
    assert_eq!(inspection.shape.dimensions(), [t, z, y, x]);
    assert_eq!(u64::from(inspection.channels), c);
    assert_eq!(inspection.dtype, IntensityDType::Uint8);
    assert_eq!(directory_tree_bytes(&source), before);
}

fn publish_and_match_independent_fact(family: &SourceFamilyFacts, file: &SourceFileFacts) {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join(Path::new(&file.path).file_name().unwrap());
    fs::write(&source, ustar_regular_file(SOURCE_ARCHIVE, &file.path)).unwrap();
    let source_before = fs::read(&source).unwrap();
    let inspection = inspect_tiff(TiffSource::single_3d(&source)).unwrap();
    let spacing_zyx = family
        .calibration_xyz_um
        .map_or([1.0; 3], |[x, y, z]| [z, y, x]);
    let manual_no_data = file.path == "spec-004/u8-no-data-corner.tif";
    let (expected_values, expected_validity) = normalized_fact_payload(file, manual_no_data);
    let expected_scientific_id = independent_scientific_id(
        dtype_from_fact(&file.dtype),
        [1, file.ifd_count, file.height, file.width],
        spacing_zyx,
        &expected_values,
        &expected_validity,
    );
    let destination = root.path().join("published.m4d");
    let checkpoint = root.path().join("checkpoint");
    let published = import_tiff(
        ImportOptions {
            inspection,
            destination: destination.clone(),
            checkpoint_directory: checkpoint.clone(),
            profile: ProfileKind::Current,
            calibration: SpatialCalibration::new(spacing_zyx),
            time_step_seconds: None,
            no_data: manual_no_data.then(|| NoDataPolicy::manual_uint8(255)),
        },
        &TestLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap_or_else(|error| panic!("production publication rejected {}: {error}", file.path));
    assert_eq!(
        published.receipt().scientific_content_id,
        expected_scientific_id,
        "{}",
        file.path
    );
    assert_eq!(fs::read(&source).unwrap(), source_before, "{}", file.path);
    assert!(!checkpoint.exists(), "{}", file.path);

    let (_, transfer) = published.into_parts();
    let (package, _) = transfer.consume(|| false).unwrap();
    assert_eq!(
        package.scientific_content_id(),
        expected_scientific_id,
        "{}",
        file.path
    );
    let layer = &package.catalog().science().layers()[0];
    assert_eq!(
        layer.base_shape().dimensions(),
        [1, file.ifd_count, file.height, file.width],
        "{}",
        file.path
    );
    assert_eq!(layer.dtype(), dtype_from_fact(&file.dtype), "{}", file.path);
    let expected_transform =
        GridToWorld::scale(spacing_zyx[2], spacing_zyx[1], spacing_zyx[0]).unwrap();
    assert_eq!(
        layer
            .grid_to_world_micrometer_f64_bits()
            .map(|value| value.value()),
        expected_transform.row_major(),
        "{}",
        file.path
    );

    let brick = package
        .read_brick(PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0), || false)
        .unwrap();
    assert_eq!(
        brick.logical_extent_zyx(),
        [file.ifd_count, file.height, file.width],
        "{}",
        file.path
    );
    let (actual_values, actual_validity) = logical_brick_payload(
        &brick,
        dtype_from_fact(&file.dtype),
        [file.ifd_count, file.height, file.width],
    );
    assert_eq!(actual_values, expected_values, "{}", file.path);
    assert_eq!(actual_validity, expected_validity, "{}", file.path);
}

fn normalized_fact_payload(file: &SourceFileFacts, manual_no_data: bool) -> (Vec<u8>, Vec<u8>) {
    let mut values = decode_hex(&file.logical_value_hex);
    let voxel_count = usize::try_from(file.ifd_count * file.height * file.width).unwrap();
    let mut validity = vec![0xff; voxel_count.div_ceil(8)];
    if !voxel_count.is_multiple_of(8) {
        *validity.last_mut().unwrap() = (1 << (voxel_count % 8)) - 1;
    }
    if manual_no_data {
        assert_eq!(file.dtype, "u8");
        let mut invalid = vec![false; voxel_count];
        for z in 0..file.ifd_count {
            for y in 0..file.height {
                for x in 0..file.width {
                    let index = usize::try_from((z * file.height + y) * file.width + x).unwrap();
                    if values[index] != 255 {
                        continue;
                    }
                    let z_start = z.saturating_sub(u64::from(file.ifd_count > 1));
                    let z_end = (z + u64::from(file.ifd_count > 1) + 1).min(file.ifd_count);
                    let y_start = y.saturating_sub(1);
                    let y_end = (y + 2).min(file.height);
                    let x_start = x.saturating_sub(1);
                    let x_end = (x + 2).min(file.width);
                    for invalid_z in z_start..z_end {
                        for invalid_y in y_start..y_end {
                            for invalid_x in x_start..x_end {
                                invalid[usize::try_from(
                                    (invalid_z * file.height + invalid_y) * file.width + invalid_x,
                                )
                                .unwrap()] = true;
                            }
                        }
                    }
                }
            }
        }
        for (index, invalid) in invalid.into_iter().enumerate() {
            if invalid {
                validity[index / 8] &= !(1 << (index % 8));
                values[index] = 0;
            }
        }
    }
    (values, validity)
}

fn independent_scientific_id(
    dtype: IntensityDType,
    shape_tzyx: [u64; 4],
    spacing_zyx: [f64; 3],
    values: &[u8],
    validity: &[u8],
) -> mirante4d_identity::ScientificContentId {
    let shape = Shape4D::new(shape_tzyx[0], shape_tzyx[1], shape_tzyx[2], shape_tzyx[3]).unwrap();
    let descriptor = ScientificLayerDescriptor::new(
        LogicalLayerKey::new(0),
        dtype,
        shape,
        ScientificTemporalCalibration::Unknown,
        GridToWorld::scale(spacing_zyx[2], spacing_zyx[1], spacing_zyx[0]).unwrap(),
    )
    .unwrap();
    let mut layer = ScientificLayerHasher::new(descriptor).unwrap();
    assert_eq!(layer.expected_tile_count(), 1);
    layer
        .push_tile(ScientificTile::new([0; 4], shape_tzyx, validity, values))
        .unwrap();
    let mut dataset = ScientificDatasetHasher::new(1).unwrap();
    dataset.push_layer(layer.finalize().unwrap()).unwrap();
    dataset.finalize().unwrap()
}

fn logical_brick_payload(
    brick: &mirante4d_storage::LocalBrickRead,
    dtype: IntensityDType,
    shape_zyx: [u64; 3],
) -> (Vec<u8>, Vec<u8>) {
    let pixels = brick
        .pixel_payload()
        .expect("fixture brick has pixel payload");
    let sample_bytes = usize::from(dtype.bytes_per_sample());
    let brick_shape = if shape_zyx[0] == 1 {
        [1_usize, 256, 256]
    } else {
        [64_usize, 64, 64]
    };
    let voxel_count = usize::try_from(shape_zyx.into_iter().product::<u64>()).unwrap();
    let mut values = Vec::with_capacity(voxel_count * sample_bytes);
    let mut validity = vec![0_u8; voxel_count.div_ceil(8)];
    let validity_payload = brick.validity_payload();
    let mut logical_index = 0_usize;
    for z in 0..usize::try_from(shape_zyx[0]).unwrap() {
        for y in 0..usize::try_from(shape_zyx[1]).unwrap() {
            for x in 0..usize::try_from(shape_zyx[2]).unwrap() {
                let physical_index = (z * brick_shape[1] + y) * brick_shape[2] + x;
                let start = physical_index * sample_bytes;
                values.extend_from_slice(&pixels[start..start + sample_bytes]);
                let valid = validity_payload
                    .is_none_or(|bits| bits[physical_index / 8] & (1 << (physical_index % 8)) != 0);
                if valid {
                    validity[logical_index / 8] |= 1 << (logical_index % 8);
                }
                logical_index += 1;
            }
        }
    }
    (values, validity)
}

fn decode_hex(value: &str) -> Vec<u8> {
    assert!(value.len().is_multiple_of(2));
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|digits| u8::from_str_radix(std::str::from_utf8(digits).unwrap(), 16).unwrap())
        .collect()
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
