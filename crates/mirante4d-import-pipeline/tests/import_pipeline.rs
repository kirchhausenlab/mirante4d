use std::{
    fs,
    path::{Path, PathBuf},
};

use mirante4d_dataset::{CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError};
use mirante4d_import_pipeline::{
    ImportCancellation, ImportEvent, ImportOptions, ImportStage, NoDataPolicy, SpatialCalibration,
    TiffSource, import_tiff, inspect_tiff, inspect_tiff_cancellable,
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
        inspect_tiff_cancellable(TiffSource::auto(source), &cancellation),
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
            Some(NoDataPolicy::U8Sentinel(255)),
            "u8-no-data-corner.tif",
        ),
        ("spec-004/f32-finite.tif", None, "f32-finite.tif"),
    ];

    for (ordinal, (archive_path, no_data, file_name)) in cases.into_iter().enumerate() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join(file_name);
        fs::write(&source, ustar_regular_file(SOURCE_ARCHIVE, archive_path)).unwrap();
        let source_before = fs::read(&source).unwrap();
        let inspection = inspect_tiff(TiffSource::auto(&source)).unwrap();
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
                profile: ProfileKind::Ds0,
                calibration: SpatialCalibration::new(spacing),
                time_step_seconds: None,
                no_data,
                working_memory_bytes: WORKING_MEMORY_BYTES,
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
                + receipt.statistics.scientific_identity_native_decoded_bytes
        );
        assert_eq!(
            receipt.statistics.source_revalidation_bytes_read,
            u64::try_from(source_before.len()).unwrap()
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
        assert_eq!(receipt.statistics.tiff_open_count, 2);
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
        assert_eq!(receipt.statistics.open_file_descriptor_structural_bound, 35);
        assert_eq!(
            receipt.statistics.peak_open_file_descriptors,
            receipt
                .statistics
                .sampled_peak_open_file_descriptors
                .max(receipt.statistics.open_file_descriptor_structural_bound)
        );
        assert!(receipt.statistics.peak_open_file_descriptors <= 64);
        assert_eq!(receipt.statistics.peak_checkpoint_regular_files, 6);
        assert!(
            receipt.statistics.peak_temporary_bytes
                <= receipt.statistics.preflight_temporary_bytes_bound
        );
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
        let (verified, _) = transfer.consume(|| false).unwrap();
        assert_eq!(verified.package_id(), receipt.package_id);
        assert_eq!(
            verified.scientific_content_id(),
            receipt.scientific_content_id
        );
    }
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
        let inspection = inspect_tiff(TiffSource::auto(&source)).unwrap();
        let published = import_tiff(
            ImportOptions {
                calibration: SpatialCalibration::new(inspection.ome_spacing_zyx_um.unwrap()),
                inspection,
                destination: run_root.join("dataset.m4d"),
                checkpoint_directory: run_root.join("checkpoint"),
                profile: ProfileKind::Ds0,
                time_step_seconds: None,
                no_data: None,
                working_memory_bytes: WORKING_MEMORY_BYTES,
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
    let inspection = inspect_tiff(TiffSource::auto(&source)).unwrap();
    let destination = root.path().join("resumed.m4d");
    let checkpoint = root.path().join("resumed.checkpoint");
    let options = ImportOptions {
        inspection,
        destination: destination.clone(),
        checkpoint_directory: checkpoint.clone(),
        profile: ProfileKind::Ds0,
        calibration: SpatialCalibration::new([1.0; 3]),
        time_step_seconds: Some(1.0),
        no_data: None,
        working_memory_bytes: WORKING_MEMORY_BYTES,
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
    assert_eq!(
        fs::read_dir(&checkpoint)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        6
    );

    let published = import_tiff(options, &TestLedger, &ImportCancellation::new(), |_| {}).unwrap();
    let receipt = published.receipt();
    // The interrupted spool batch is intentionally discarded, while the
    // separately durable canonical ingest is reused without another TIFF
    // native decode.
    assert_eq!(receipt.statistics.base_native_decoded_bytes, 0);
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
    let inspection = inspect_tiff(TiffSource::auto(&source)).unwrap();

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
                profile: ProfileKind::Ds0,
                calibration: SpatialCalibration::new([1.0; 3]),
                time_step_seconds: None,
                no_data: None,
                working_memory_bytes: WORKING_MEMORY_BYTES,
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

    let inspection = inspect_tiff(TiffSource::auto(&source)).unwrap();
    let destination = root.path().join("wide.m4d");
    let published = import_tiff(
        ImportOptions {
            inspection,
            destination: destination.clone(),
            checkpoint_directory: root.path().join("wide.checkpoint"),
            profile: ProfileKind::Ds0,
            calibration: SpatialCalibration::new([1.0; 3]),
            time_step_seconds: Some(1.0),
            no_data: None,
            working_memory_bytes: WORKING_MEMORY_BYTES,
        },
        &TestLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();
    let receipt = published.receipt();
    assert!(receipt.statistics.produced_work_units > 10);

    let (_receipt, transfer) = published.into_parts();
    let (verified, _) = transfer.consume(|| false).unwrap();
    assert!(verified.catalog().profile().images()[0].levels().len() > 1);
    let ome_path = PackagePath::parse("images/i00000000/zarr.json").unwrap();
    let OmeLevelTransform::DiagonalMicrometer {
        scale_zyx,
        translation_zyx,
    } = verified
        .catalog()
        .ome_image(&ome_path)
        .unwrap()
        .level_transforms()[1]
    else {
        panic!("calibrated import must retain a diagonal OME transform");
    };
    assert_eq!(scale_zyx.map(|value| value.value()), [2.0; 3]);
    assert_eq!(translation_zyx.map(|value| value.value()), [0.0; 3]);

    let base_tail = verified
        .read_brick(PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 4), || false)
        .unwrap();
    assert_eq!(base_tail.logical_extent_zyx(), [1, 256, 1]);
    assert_eq!(base_tail.pixel_payload().unwrap()[0], pattern(1_024, 0));

    let coarse = verified
        .read_brick(PackedIndexCoordinates::new(0, 1, 0, 0, 0, 0, 0), || false)
        .unwrap();
    let coarse_pixels = coarse.pixel_payload().unwrap();
    for (x, y) in [(127_u32, 0_u32), (128, 64)] {
        let index = usize::try_from(y * 256 + x).unwrap();
        assert_eq!(coarse_pixels[index], pattern(x * 2, y * 2));
    }

    let coarse_tail = verified
        .read_brick(PackedIndexCoordinates::new(0, 1, 0, 0, 0, 0, 2), || false)
        .unwrap();
    assert_eq!(coarse_tail.logical_extent_zyx(), [1, 129, 1]);
    assert_eq!(coarse_tail.pixel_payload().unwrap()[0], pattern(1_024, 0));
}

#[test]
fn matched_channel_folders_publish_one_multichannel_volume() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("channels");
    for channel in 0..2 {
        let folder = source.join(format!("channel{channel}"));
        fs::create_dir_all(&folder).unwrap();
        for z in 0..3 {
            let archive_path = format!("spec-003/channel-{channel:02}/z{z:03}.tif");
            fs::write(
                folder.join(format!("z{z:03}.tif")),
                ustar_regular_file(SOURCE_ARCHIVE, &archive_path),
            )
            .unwrap();
        }
    }
    let source_before = directory_tree_bytes(&source);
    let inspection = inspect_tiff(TiffSource::auto(&source)).unwrap();
    assert_eq!(inspection.channels, 2);
    assert_eq!(inspection.shape.dimensions(), [1, 3, 3, 4]);

    let destination = root.path().join("channels.m4d");
    let published = import_tiff(
        ImportOptions {
            inspection,
            destination: destination.clone(),
            checkpoint_directory: root.path().join("channels.checkpoint"),
            profile: ProfileKind::Ds0,
            calibration: SpatialCalibration::new([1.0; 3]),
            time_step_seconds: None,
            no_data: None,
            working_memory_bytes: WORKING_MEMORY_BYTES,
        },
        &TestLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();

    let (_receipt, transfer) = published.into_parts();
    let (verified, _) = transfer.consume(|| false).unwrap();
    assert_eq!(verified.catalog().science().layers().len(), 2);
    assert_eq!(directory_tree_bytes(&source), source_before);
}

fn pattern(x: u32, y: u32) -> u8 {
    u8::try_from((x + 3 * y) % 251).unwrap()
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
