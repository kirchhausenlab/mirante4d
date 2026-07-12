use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use mirante4d_identity::Sha256Hasher;
use mirante4d_storage::{
    LocalPackageCatalog, LocalPackageReader, LocalPackageWriter, OmeImageGroupMetadata,
    PackageArrayInput, PackageObjectKind, PackagePath, PackageShardInput, PackageWriteInput,
    PackedIndexCoordinates, ProfileKind, ShardProfileKind, VerifiedScientificPackageCapability,
    decode_inner_payload, decode_shard_index_tail,
};
use serde::Deserialize;

const BLOCK_BYTES: usize = 512;
const ARCHIVE_BYTES_MAX: usize = 16 * 1024 * 1024;

#[derive(Deserialize)]
struct AuthorityManifest {
    archives: Vec<ArchiveAuthority>,
}

#[derive(Deserialize)]
struct ArchiveAuthority {
    case_id: String,
    path: String,
    bytes: u64,
    sha256: String,
    package_id: String,
    inventory: ArchiveInventory,
}

#[derive(Deserialize)]
struct ArchiveInventory {
    directories: Vec<String>,
    files: BTreeMap<String, FileAuthority>,
    file_count: u64,
    directory_count: u64,
}

#[derive(Deserialize)]
struct FileAuthority {
    bytes: u64,
    sha256: String,
}

#[derive(Debug, PartialEq, Eq)]
struct PackageTree {
    directories: BTreeSet<String>,
    files: BTreeMap<String, Vec<u8>>,
}

struct ScratchDirectory(PathBuf);

impl ScratchDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let suffix = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mirante4d-target-writer-conformance-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create isolated writer-conformance directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove isolated writer-conformance directory");
    }
}

#[test]
fn production_writer_preserves_promoted_metadata_and_scientific_content() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("storage crate is inside the repository");
    let manifest: AuthorityManifest = serde_json::from_slice(
        &fs::read(repository.join("fixtures/target/manifest.json"))
            .expect("read promoted target authority"),
    )
    .expect("parse promoted target authority");

    assert_eq!(manifest.archives.len(), 3);
    let retained_root = retained_output_root();
    for archive in &manifest.archives {
        reproduce_archive(repository, archive, retained_root.as_deref());
    }
}

fn reproduce_archive(repository: &Path, archive: &ArchiveAuthority, output_root: Option<&Path>) {
    let relative = checked_repository_path(&archive.path);
    assert!(relative.starts_with("fixtures/target/archives"));
    let encoded = fs::read(repository.join(relative)).expect("read promoted target archive");
    assert_eq!(encoded.len() as u64, archive.bytes);
    assert!(encoded.len() <= ARCHIVE_BYTES_MAX);
    assert_eq!(Sha256Hasher::digest(&encoded).to_string(), archive.sha256);

    let scratch = ScratchDirectory::new();
    let source = scratch.path().join("source");
    fs::create_dir(&source).expect("create source extraction root");
    extract_ustar(&encoded, &source, &archive.inventory);
    let written = output_root.map_or_else(
        || scratch.path().join("written"),
        |root| {
            assert_case_id(&archive.case_id);
            root.join(&archive.case_id)
        },
    );

    let catalog = LocalPackageCatalog::open(&source).expect("open independent target package");
    assert_eq!(
        catalog.declared_package_id().to_string(),
        archive.package_id
    );
    assert!(
        catalog.profile().portable_record_paths().is_empty(),
        "the promoted target corpus has no portable records"
    );
    let arrays = package_arrays(&catalog);
    let ome_images = ome_images(&catalog);
    let shards = decode_all_shards(&source, &catalog, &arrays);
    let input = PackageWriteInput::new(
        ProfileKind::Ds0,
        catalog.profile().clone(),
        catalog.science().clone(),
        catalog.display_defaults().clone(),
        Vec::new(),
        ome_images,
        arrays,
        shards,
    );

    let receipt = LocalPackageWriter::write_new(&written, input, || false)
        .expect("production writer recreates the independent package");
    let source_capability = validate_scientific_package(&source);
    let written_capability = validate_scientific_package(&written);
    assert_eq!(
        source_capability.package_id().to_string(),
        archive.package_id
    );
    assert_eq!(written_capability.package_id(), receipt.package_id());
    assert_eq!(
        source_capability.admission(),
        written_capability.admission()
    );

    let differing_files = assert_metadata_and_tree_boundary(
        &source,
        &written,
        source_capability.catalog(),
        &archive.case_id,
    );
    assert_eq!(
        source_capability.package_id() != written_capability.package_id(),
        !differing_files.is_empty(),
        "{}: PackageId equality must exactly track package-byte equality",
        archive.case_id
    );
    if !differing_files.is_empty() {
        assert!(
            differing_files.iter().any(|path| source_capability
                .catalog()
                .descriptor(&PackagePath::parse(path).expect("differing package path is canonical"))
                .is_some_and(|descriptor| matches!(
                    descriptor.kind(),
                    PackageObjectKind::PixelShard
                        | PackageObjectKind::ValidityShard
                        | PackageObjectKind::PackedIndexShard
                ))),
            "{}: a changed PackageId must originate in re-encoded shard bytes",
            archive.case_id
        );
        assert!(
            differing_files.contains("m4d/manifest/root.json"),
            "{}: changed package bytes must change the canonical manifest root",
            archive.case_id
        );
    }

    assert_eq!(
        source_capability.scientific_content_id(),
        written_capability.scientific_content_id()
    );
    assert_eq!(
        source_capability.layer_roots(),
        written_capability.layer_roots()
    );
    compare_all_bricks(&source_capability, &written_capability, &archive.case_id);
}

fn validate_scientific_package(root: &Path) -> VerifiedScientificPackageCapability {
    LocalPackageCatalog::open(root)
        .expect("open target-profile package")
        .validate_exact_package(ProfileKind::Ds0, || false)
        .expect("target-profile package passes exact validation")
        .validate_scientific_content(|| false)
        .expect("target-profile package passes scientific validation")
}

fn package_arrays(catalog: &LocalPackageCatalog) -> Vec<PackageArrayInput> {
    let mut arrays = Vec::new();
    for image in catalog.profile().images() {
        for level in image.levels() {
            arrays.push(package_array(catalog, level.pixel_path()));
            if let Some(path) = level.validity_path() {
                arrays.push(package_array(catalog, path));
            }
            arrays.push(package_array(catalog, level.packed_index_path()));
        }
    }
    arrays
}

fn package_array(catalog: &LocalPackageCatalog, base: &PackagePath) -> PackageArrayInput {
    let metadata_path = metadata_path(base);
    let metadata = catalog
        .zarr_array(&metadata_path)
        .unwrap_or_else(|| panic!("missing array metadata {metadata_path}"))
        .clone();
    PackageArrayInput::new(base.clone(), metadata)
}

fn ome_images(catalog: &LocalPackageCatalog) -> Vec<OmeImageGroupMetadata> {
    catalog
        .profile()
        .images()
        .iter()
        .map(|image| {
            let path = metadata_path(image.image_group_path());
            catalog
                .ome_image(&path)
                .unwrap_or_else(|| panic!("missing OME image metadata {path}"))
                .clone()
        })
        .collect()
}

fn decode_all_shards(
    root: &Path,
    catalog: &LocalPackageCatalog,
    arrays: &[PackageArrayInput],
) -> Vec<PackageShardInput> {
    let kinds = arrays
        .iter()
        .map(|array| (array.path().clone(), array.metadata().kind()))
        .collect::<BTreeMap<_, _>>();
    let reader = LocalPackageReader::open(root).expect("open independent package range reader");
    catalog
        .descriptors()
        .iter()
        .filter(|descriptor| {
            matches!(
                descriptor.kind(),
                PackageObjectKind::PixelShard
                    | PackageObjectKind::ValidityShard
                    | PackageObjectKind::PackedIndexShard
            )
        })
        .map(|descriptor| {
            let (base, outer_coordinates) = split_shard_path(descriptor.path());
            let kind = *kinds
                .get(&base)
                .unwrap_or_else(|| panic!("shard has no array metadata: {}", descriptor.path()));
            assert_shard_kind(descriptor.kind(), kind);
            let declared_bytes = descriptor.raw().byte_length();
            let tail_bytes = u64::try_from(kind.index_tail_bytes()).expect("tail length fits u64");
            let (tail, payload_bytes) = reader
                .read_shard_index_tail(descriptor.path(), tail_bytes, declared_bytes)
                .expect("read source shard index");
            let index = decode_shard_index_tail(kind, &tail, payload_bytes)
                .expect("decode source shard index");
            let decoded_chunks = (0..kind.chunks_per_shard())
                .map(|slot| {
                    index
                        .entry(slot)
                        .expect("source shard slot is in bounds")
                        .map(|entry| {
                            let encoded = reader
                                .read_range(
                                    descriptor.path(),
                                    entry.offset(),
                                    entry.nbytes(),
                                    declared_bytes,
                                )
                                .expect("read encoded source inner chunk");
                            decode_inner_payload(kind, &encoded).expect("decode source inner chunk")
                        })
                })
                .collect::<Vec<_>>();
            PackageShardInput::new(base, outer_coordinates, decoded_chunks)
        })
        .collect()
}

fn split_shard_path(path: &PackagePath) -> (PackagePath, Vec<u64>) {
    let (base, coordinates) = path
        .as_str()
        .split_once("/c/")
        .unwrap_or_else(|| panic!("shard path lacks /c/: {path}"));
    let base = PackagePath::parse(base).expect("shard base path is canonical");
    let coordinates = coordinates
        .split('/')
        .map(|value| value.parse::<u64>().expect("shard coordinate is unsigned"))
        .collect::<Vec<_>>();
    (base, coordinates)
}

fn assert_shard_kind(object: PackageObjectKind, storage: ShardProfileKind) {
    let matches = match object {
        PackageObjectKind::PixelShard => matches!(
            storage,
            ShardProfileKind::Pixel3dUint8
                | ShardProfileKind::Pixel3dUint16
                | ShardProfileKind::Pixel3dFloat32
                | ShardProfileKind::Pixel2dUint8
                | ShardProfileKind::Pixel2dUint16
                | ShardProfileKind::Pixel2dFloat32
        ),
        PackageObjectKind::ValidityShard => {
            matches!(
                storage,
                ShardProfileKind::Validity3d | ShardProfileKind::Validity2d
            )
        }
        PackageObjectKind::PackedIndexShard => storage == ShardProfileKind::PackedIndex,
        _ => false,
    };
    assert!(matches, "manifest shard kind and array metadata disagree");
}

fn metadata_path(base: &PackagePath) -> PackagePath {
    PackagePath::parse(&format!("{base}/zarr.json")).expect("metadata path is canonical")
}

fn assert_metadata_and_tree_boundary(
    source: &Path,
    written: &Path,
    source_catalog: &LocalPackageCatalog,
    case_id: &str,
) -> BTreeSet<String> {
    let source_tree = package_tree(source);
    let written_tree = package_tree(written);
    assert_eq!(
        source_tree.directories, written_tree.directories,
        "{case_id}: directory set differs"
    );
    assert_eq!(
        source_tree.files.keys().collect::<Vec<_>>(),
        written_tree.files.keys().collect::<Vec<_>>(),
        "{case_id}: file set differs"
    );
    let shard_paths = source_catalog
        .descriptors()
        .iter()
        .filter(|descriptor| {
            matches!(
                descriptor.kind(),
                PackageObjectKind::PixelShard
                    | PackageObjectKind::ValidityShard
                    | PackageObjectKind::PackedIndexShard
            )
        })
        .map(|descriptor| descriptor.path().to_string())
        .collect::<BTreeSet<_>>();
    let mut differing = BTreeSet::new();
    for (path, expected) in source_tree.files {
        let actual = &written_tree.files[&path];
        if actual == &expected {
            continue;
        }
        assert!(
            shard_paths.contains(&path)
                || path.starts_with("m4d/manifest/pages/")
                || path == "m4d/manifest/root.json",
            "{case_id}: schema/control/Zarr/OME metadata bytes differ at {path}"
        );
        assert!(differing.insert(path));
    }
    differing
}

fn compare_all_bricks(
    source: &VerifiedScientificPackageCapability,
    written: &VerifiedScientificPackageCapability,
    case_id: &str,
) {
    assert_eq!(source.catalog().profile(), written.catalog().profile());
    let mut compared = 0_u64;
    for image in source.catalog().profile().images() {
        for level in image.levels() {
            let metadata_path = metadata_path(level.pixel_path());
            let source_metadata = source
                .catalog()
                .zarr_array(&metadata_path)
                .expect("source pixel metadata");
            let written_metadata = written
                .catalog()
                .zarr_array(&metadata_path)
                .expect("written pixel metadata");
            assert_eq!(source_metadata, written_metadata);
            let shape: [u64; 5] = source_metadata
                .shape()
                .try_into()
                .expect("pixel array has five dimensions");
            let inner = pixel_inner_shape(source_metadata.kind());
            let grid = [
                shape[2].div_ceil(inner[0]),
                shape[3].div_ceil(inner[1]),
                shape[4].div_ceil(inner[2]),
            ];
            for t in 0..shape[0] {
                for c in 0..shape[1] {
                    for z in 0..grid[0] {
                        for y in 0..grid[1] {
                            for x in 0..grid[2] {
                                let coordinates = PackedIndexCoordinates::new(
                                    image.image_ordinal(),
                                    level.scale_ordinal(),
                                    u32::try_from(t).expect("T1 time fits u32"),
                                    u32::try_from(c).expect("T1 channel fits u32"),
                                    u32::try_from(z).expect("T1 z brick fits u32"),
                                    u32::try_from(y).expect("T1 y brick fits u32"),
                                    u32::try_from(x).expect("T1 x brick fits u32"),
                                );
                                let expected = source
                                    .read_brick(coordinates, || false)
                                    .expect("read independent source brick");
                                let actual = written
                                    .read_brick(coordinates, || false)
                                    .expect("read production-written brick");
                                assert_eq!(
                                    expected.record(),
                                    actual.record(),
                                    "{case_id}: packed record differs at {coordinates:?}"
                                );
                                assert_eq!(
                                    expected.logical_extent_zyx(),
                                    actual.logical_extent_zyx(),
                                    "{case_id}: brick extent differs at {coordinates:?}"
                                );
                                assert_eq!(
                                    expected.pixel_payload(),
                                    actual.pixel_payload(),
                                    "{case_id}: decoded pixels differ at {coordinates:?}"
                                );
                                assert_eq!(
                                    expected.validity_payload(),
                                    actual.validity_payload(),
                                    "{case_id}: decoded validity differs at {coordinates:?}"
                                );
                                compared += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(compared, source.admission().counts().logical_bricks);
    assert_eq!(compared, written.admission().counts().logical_bricks);
}

fn pixel_inner_shape(kind: ShardProfileKind) -> [u64; 3] {
    match kind {
        ShardProfileKind::Pixel3dUint8
        | ShardProfileKind::Pixel3dUint16
        | ShardProfileKind::Pixel3dFloat32 => [64, 64, 64],
        ShardProfileKind::Pixel2dUint8
        | ShardProfileKind::Pixel2dUint16
        | ShardProfileKind::Pixel2dFloat32 => [1, 256, 256],
        _ => panic!("expected pixel storage kind"),
    }
}

fn retained_output_root() -> Option<PathBuf> {
    let requested = env::var_os("MIRANTE4D_WRITER_CONFORMANCE_OUTPUT_ROOT")?;
    let requested = PathBuf::from(requested);
    assert!(!requested.as_os_str().is_empty());
    assert!(
        requested.components().all(|component| matches!(
            component,
            Component::RootDir | Component::Prefix(_) | Component::Normal(_)
        )),
        "writer-conformance output root must be normalized"
    );
    let root = if requested.is_absolute() {
        requested
    } else {
        env::current_dir()
            .expect("resolve current directory")
            .join(requested)
    };
    assert!(root.file_name().is_some(), "refuse filesystem-root output");
    let parent = root.parent().expect("output root has a parent");
    let parent_metadata = fs::symlink_metadata(parent).expect("output parent must already exist");
    assert!(!parent_metadata.file_type().is_symlink());
    assert!(parent_metadata.is_dir());
    if root.exists() {
        let metadata = fs::symlink_metadata(&root).expect("inspect existing output root");
        assert!(!metadata.file_type().is_symlink());
        assert!(metadata.is_dir());
        fs::remove_dir_all(&root).expect("recreate writer-conformance output root");
    }
    fs::create_dir(&root).expect("create writer-conformance output root");
    Some(root)
}

fn assert_case_id(case_id: &str) {
    assert!(!case_id.is_empty());
    assert!(case_id.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
    }));
}

fn package_tree(root: &Path) -> PackageTree {
    let mut directories = BTreeSet::new();
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let mut children = fs::read_dir(&directory)
            .expect("read package directory")
            .map(|entry| entry.expect("read package directory entry"))
            .collect::<Vec<_>>();
        children.sort_unstable_by_key(|entry| entry.file_name());
        for entry in children {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).expect("inspect package tree entry");
            assert!(!metadata.file_type().is_symlink());
            let relative = normalized_relative(root, &path);
            if metadata.is_dir() {
                assert!(directories.insert(relative));
                pending.push(path);
            } else {
                assert!(metadata.is_file());
                assert!(
                    files
                        .insert(relative, fs::read(path).expect("read package file"))
                        .is_none()
                );
            }
        }
    }
    PackageTree { directories, files }
}

fn normalized_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("package entry is under root")
        .components()
        .map(|component| match component {
            Component::Normal(value) => value.to_str().expect("package path is UTF-8"),
            _ => panic!("package path is not normalized"),
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn extract_ustar(encoded: &[u8], root: &Path, inventory: &ArchiveInventory) {
    assert_eq!(encoded.len() % BLOCK_BYTES, 0);
    assert_eq!(inventory.files.len() as u64, inventory.file_count);
    assert_eq!(
        inventory.directories.len() as u64,
        inventory.directory_count
    );
    let expected_directories = inventory
        .directories
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut seen_directories = BTreeSet::new();
    let mut seen_files = BTreeSet::new();
    let mut case_folded = BTreeSet::new();
    let mut offset = 0_usize;
    let mut terminated = false;

    while offset + BLOCK_BYTES <= encoded.len() {
        let header = &encoded[offset..offset + BLOCK_BYTES];
        if header.iter().all(|byte| *byte == 0) {
            assert!(encoded[offset..].iter().all(|byte| *byte == 0));
            assert!(offset + 2 * BLOCK_BYTES <= encoded.len());
            terminated = true;
            break;
        }
        assert_eq!(&header[257..263], b"ustar\0");
        assert_eq!(&header[263..265], b"00");
        let expected_checksum = parse_octal(&header[148..156]);
        let actual_checksum = header
            .iter()
            .enumerate()
            .map(|(index, byte)| {
                if (148..156).contains(&index) {
                    b' '
                } else {
                    *byte
                }
            })
            .map(u64::from)
            .sum::<u64>();
        assert_eq!(actual_checksum, expected_checksum);

        let name = field(&header[..100]);
        let prefix = field(&header[345..500]);
        let raw_path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let type_flag = header[156];
        let is_directory = type_flag == b'5';
        assert!(matches!(type_flag, 0 | b'0' | b'5'));
        assert!(field(&header[157..257]).is_empty());
        let path = raw_path.strip_suffix('/').unwrap_or(&raw_path);
        let relative = checked_archive_path(path);
        assert!(case_folded.insert(path.to_ascii_lowercase()));
        let size = parse_octal(&header[124..136]) as usize;
        offset += BLOCK_BYTES;
        let end = offset
            .checked_add(size)
            .expect("USTAR member size overflow");
        assert!(end <= encoded.len());

        let destination = root.join(&relative);
        if is_directory {
            assert_eq!(size, 0);
            assert!(expected_directories.contains(path));
            assert!(seen_directories.insert(path.to_owned()));
            fs::create_dir(&destination).expect("create declared archive directory");
        } else {
            let expected = inventory.files.get(path).expect("archive file is declared");
            assert_eq!(size as u64, expected.bytes);
            let bytes = &encoded[offset..end];
            assert_eq!(Sha256Hasher::digest(bytes).to_string(), expected.sha256);
            assert!(seen_files.insert(path.to_owned()));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)
                .expect("create declared archive file exactly once");
            file.write_all(bytes).expect("write declared archive file");
        }
        offset = end.div_ceil(BLOCK_BYTES) * BLOCK_BYTES;
        assert!(encoded[end..offset].iter().all(|byte| *byte == 0));
    }

    assert!(terminated);
    assert_eq!(seen_directories, expected_directories);
    assert_eq!(seen_files, inventory.files.keys().cloned().collect());
}

fn checked_repository_path(value: &str) -> PathBuf {
    let path = Path::new(value);
    assert!(!path.is_absolute());
    assert!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_)))
    );
    path.to_owned()
}

fn checked_archive_path(value: &str) -> PathBuf {
    assert!(!value.is_empty());
    assert!(value.is_ascii());
    assert!(!value.contains('\\'));
    checked_repository_path(value)
}

fn field(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    assert!(bytes[end..].iter().all(|byte| *byte == 0));
    let value = &bytes[..end];
    assert!(value.is_ascii());
    String::from_utf8(value.to_vec()).expect("USTAR field is ASCII")
}

fn parse_octal(bytes: &[u8]) -> u64 {
    let value = bytes
        .iter()
        .copied()
        .take_while(|byte| !matches!(byte, 0 | b' '))
        .collect::<Vec<_>>();
    assert!(!value.is_empty());
    value.into_iter().fold(0_u64, |total, byte| {
        assert!((b'0'..=b'7').contains(&byte));
        total * 8 + u64::from(byte - b'0')
    })
}
