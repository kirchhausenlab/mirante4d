use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use mirante4d_dataset::{CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError};
use mirante4d_import_pipeline::{
    ImportCancellation, ImportEvent, ImportOptions, NoDataPolicy, SpatialCalibration, TiffSource,
    import_tiff, inspect_tiff, inspect_tiff_cancellable,
};
use mirante4d_storage::{
    LocalPackageCatalog, OmeLevelTransform, PackagePath, PackedIndexCoordinates, ProfileKind,
};
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
struct TestLedger {
    calls: AtomicUsize,
    cancellation: Option<ImportCancellation>,
    cancel_at_call: Option<usize>,
}

impl TestLedger {
    fn cancelling(cancellation: ImportCancellation, cancel_at_call: usize) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            cancellation: Some(cancellation),
            cancel_at_call: Some(cancel_at_call),
        }
    }
}

impl CpuByteLedger for TestLedger {
    fn try_acquire(
        &self,
        category: CpuLedgerCategory,
        bytes: u64,
    ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
        assert_eq!(category, CpuLedgerCategory::ImportWorkingSet);
        assert!(bytes > 0);
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        if self.cancel_at_call == Some(call) {
            self.cancellation.as_ref().unwrap().cancel();
        }
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
        let spacing = inspection.ome_spacing_zyx_um.unwrap_or([1.0; 3]);
        let destination = root.path().join(format!("case-{ordinal}.m4d"));
        let checkpoint = root.path().join(format!("case-{ordinal}.checkpoint"));
        let mut events = Vec::new();
        let receipt = import_tiff(
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
            &TestLedger::default(),
            &ImportCancellation::new(),
            |event| events.push(event),
        )
        .unwrap();

        assert_eq!(fs::read(&source).unwrap(), source_before);
        assert!(!checkpoint.exists());
        assert_eq!(events.last(), Some(&ImportEvent::Finished));
        assert!(receipt.statistics.produced_work_units > 0);
        assert!(receipt.statistics.peak_working_bytes <= WORKING_MEMORY_BYTES);
        let verified = LocalPackageCatalog::open(&destination)
            .unwrap()
            .validate_exact_package(ProfileKind::Ds0, || false)
            .unwrap()
            .validate_scientific_content(|| false)
            .unwrap();
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
        let receipt = import_tiff(
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
            &TestLedger::default(),
            &ImportCancellation::new(),
            |_| {},
        )
        .unwrap();
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
    // Persistent spool records and checkpoint validation take the first two
    // leases; cancel as the second production unit begins.
    let cancelling_ledger = TestLedger::cancelling(cancellation.clone(), 3);
    let error =
        import_tiff(options.clone(), &cancelling_ledger, &cancellation, |_| {}).unwrap_err();
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
        3
    );

    let receipt = import_tiff(
        options,
        &TestLedger::default(),
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();
    assert!(receipt.statistics.resumed_work_units > 0);
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
            &TestLedger::default(),
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
    let receipt = import_tiff(
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
        &TestLedger::default(),
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();
    assert!(receipt.statistics.produced_work_units > 10);

    let verified = LocalPackageCatalog::open(&destination)
        .unwrap()
        .validate_exact_package(ProfileKind::Ds0, || false)
        .unwrap()
        .validate_scientific_content(|| false)
        .unwrap();
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
    import_tiff(
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
        &TestLedger::default(),
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();

    let verified = LocalPackageCatalog::open(&destination)
        .unwrap()
        .validate_exact_package(ProfileKind::Ds0, || false)
        .unwrap()
        .validate_scientific_content(|| false)
        .unwrap();
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
