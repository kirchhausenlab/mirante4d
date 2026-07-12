use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use mirante4d_identity::{ExactBytesHasher, ScientificHashError, Sha256Hasher};
use mirante4d_storage::{
    DirectoryInventoryError, LocalPackageCatalog, ManifestRoot, PackageAdmissionError,
    PackageObjectDescriptor, PackageOpenError, PackagePath, PackageReadError,
    PackageStructureError, PackageValidationError, ProfileKind, ScientificPackageValidationError,
    ShardCodecError, ShardProfileKind, ZarrMetadataError, decode_inner_payload,
    encode_inner_payload, pack_manifest_pages,
};
use serde::Deserialize;
use serde_json::Value;

const BLOCK_BYTES: usize = 512;
const ARCHIVE_BYTES_MAX: usize = 16 * 1024 * 1024;
const MISSING: u64 = u64::MAX;

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

#[derive(Deserialize)]
struct MutationAuthority {
    recipes: Vec<Value>,
}

#[derive(Deserialize)]
struct IndependentReaderReport {
    mutations: Vec<BoundMutation>,
}

#[derive(Deserialize)]
struct BoundMutation {
    id: String,
    case_id: String,
    expected_stage: String,
    expected_rejection: String,
    derived_tree_sha256: String,
}

struct ScratchDirectory(PathBuf);

impl ScratchDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let suffix = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mirante4d-target-mutation-conformance-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create isolated mutation-conformance directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove isolated mutation-conformance directory");
    }
}

#[test]
fn production_rejects_all_promoted_mutations_at_typed_stages() {
    let repository = repository_root();
    let manifest: AuthorityManifest = read_json(&repository.join("fixtures/target/manifest.json"));
    let mutations: MutationAuthority =
        read_json(&repository.join("fixtures/target/mutations.json"));
    let reader_report: IndependentReaderReport =
        read_json(&repository.join("fixtures/target/independent-reader-report.json"));

    assert_eq!(manifest.archives.len(), 3);
    assert_eq!(mutations.recipes.len(), 15);
    assert_eq!(reader_report.mutations.len(), 15);
    for recipe in &mutations.recipes {
        exercise_mutation(&repository, &manifest, &reader_report, recipe);
    }
}

fn exercise_mutation(
    repository: &Path,
    manifest: &AuthorityManifest,
    report: &IndependentReaderReport,
    recipe: &Value,
) {
    let id = string(recipe, "id");
    let case_id = string(recipe, "case_id");
    let expected_stage = string(recipe, "expected_stage");
    let expected_rejection = string(recipe, "expected_rejection");
    let bound = report
        .mutations
        .iter()
        .find(|row| row.id == id)
        .unwrap_or_else(|| panic!("mutation {id} is absent from the independent report"));
    assert_eq!(bound.case_id, case_id);
    assert_eq!(bound.expected_stage, expected_stage);
    assert_eq!(bound.expected_rejection, expected_rejection);
    let archive = manifest
        .archives
        .iter()
        .find(|archive| archive.case_id == case_id)
        .unwrap_or_else(|| panic!("mutation {id} has no promoted base archive"));
    let encoded = read_archive(repository, archive);
    let scratch = ScratchDirectory::new();
    extract_ustar(&encoded, scratch.path(), &archive.inventory);

    let operation = string(recipe, "operation");
    let base_package_id = (operation == "replace_valid_sample_bits").then(|| {
        LocalPackageCatalog::open(scratch.path())
            .expect("open pristine non-finite mutation base")
            .declared_package_id()
    });
    let descriptor_registry = descriptor_registry(scratch.path());
    apply_mutation(scratch.path(), recipe);
    if string(recipe, "package_manifest") == "reseal" {
        reseal_manifest(scratch.path(), &descriptor_registry);
    }
    if operation == "replace_valid_sample_bits" {
        assert_replacement_bits(scratch.path(), recipe);
        let resealed_package_id = LocalPackageCatalog::open(scratch.path())
            .expect("open resealed non-finite mutation")
            .declared_package_id();
        assert_ne!(
            resealed_package_id,
            base_package_id.expect("non-finite mutation recorded its base PackageId"),
            "mutation {id} resealing did not change the declared PackageId"
        );
    } else {
        assert_eq!(
            tree_digest(scratch.path()),
            bound.derived_tree_sha256,
            "mutation {id} did not reproduce the independently recorded tree"
        );
    }
    assert_typed_rejection(scratch.path(), recipe);
}

fn assert_typed_rejection(root: &Path, recipe: &Value) {
    let id = string(recipe, "id");
    match string(recipe, "expected_stage") {
        "metadata-validation" => {
            let error = LocalPackageCatalog::open(root)
                .unwrap_err_or_else(|| panic!("mutation {id} opened successfully"));
            match id {
                "contradictory-transform" => assert!(matches!(
                    error,
                    PackageOpenError::CrossObjectInconsistency {
                        reason: "base OME and science spatial transforms differ"
                    }
                )),
                "contradictory-axis-names"
                | "unsupported-codec"
                | "unsupported-dtype"
                | "unsupported-inner-chunk-layout" => assert!(matches!(
                    error,
                    PackageOpenError::Metadata(ZarrMetadataError::Invalid { .. })
                        | PackageOpenError::Metadata(ZarrMetadataError::CoreMetadata { .. })
                )),
                _ => panic!("unexpected metadata mutation {id}"),
            }
        }
        "package-closure" => {
            let catalog = LocalPackageCatalog::open(root)
                .unwrap_or_else(|error| panic!("mutation {id} failed before closure: {error}"));
            let error = catalog
                .validate_exact_package(ProfileKind::Ds0, || false)
                .unwrap_err_or_else(|| panic!("mutation {id} passed exact validation"));
            match id {
                "missing-pixel-shard" => assert!(matches!(
                    error,
                    PackageValidationError::Structure(PackageStructureError::Admission(
                        PackageAdmissionError::Inventory(
                            DirectoryInventoryError::MissingFile { .. }
                        )
                    ))
                )),
                "truncated-pixel-shard" => assert!(matches!(
                    error,
                    PackageValidationError::Structure(PackageStructureError::Admission(
                        PackageAdmissionError::Inventory(
                            DirectoryInventoryError::ObjectLengthMismatch { .. }
                        )
                    ))
                )),
                "bit-flipped-pixel-shard" => assert!(matches!(
                    error,
                    PackageValidationError::ObjectDigestMismatch { .. }
                )),
                "unexpected-object" => assert!(matches!(
                    error,
                    PackageValidationError::Structure(PackageStructureError::Admission(
                        PackageAdmissionError::Inventory(
                            DirectoryInventoryError::UnexpectedFile { .. }
                        )
                    ))
                )),
                _ => panic!("unexpected closure mutation {id}"),
            }
        }
        "shard-decode" => {
            let catalog = LocalPackageCatalog::open(root).unwrap_or_else(|error| {
                panic!("mutation {id} failed before shard decode: {error}")
            });
            if id == "inner-crc32c-failure" {
                let capability = catalog
                    .validate_exact_package(ProfileKind::Ds0, || false)
                    .expect("inner CRC mutation retains an exact package closure");
                let error = capability
                    .read_brick(
                        mirante4d_storage::PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0),
                        || false,
                    )
                    .unwrap_err_or_else(|| panic!("mutation {id} returned a brick"));
                assert!(matches!(
                    error,
                    PackageReadError::Shard(ShardCodecError::InnerChecksumMismatch)
                ));
            } else {
                let error = catalog
                    .validate_exact_package(ProfileKind::Ds0, || false)
                    .unwrap_err_or_else(|| panic!("mutation {id} passed structure validation"));
                match id {
                    "end-index-crc32c-failure" => assert!(matches!(
                        error,
                        PackageValidationError::Structure(PackageStructureError::Shard(
                            ShardCodecError::IndexChecksumMismatch
                        ))
                    )),
                    "noncanonical-index-offset" => assert!(matches!(
                        error,
                        PackageValidationError::Structure(PackageStructureError::Shard(
                            ShardCodecError::NonCanonicalIndexOffset { .. }
                        ))
                    )),
                    _ => panic!("unexpected shard mutation {id}"),
                }
            }
        }
        "scientific-readback" => {
            let exact = LocalPackageCatalog::open(root)
                .unwrap_or_else(|error| panic!("mutation {id} failed before readback: {error}"))
                .validate_exact_package(ProfileKind::Ds0, || false)
                .unwrap_or_else(|error| panic!("mutation {id} failed exact validation: {error}"));
            let error = exact
                .validate_scientific_content(|| false)
                .unwrap_err_or_else(|| panic!("mutation {id} passed scientific validation"));
            assert!(matches!(
                error,
                ScientificPackageValidationError::Identity(
                    ScientificHashError::NonFiniteFloatSample { .. }
                )
            ));
        }
        stage => panic!("mutation {id} has unexpected stage {stage}"),
    }
}

trait ResultExpectErr<T, E> {
    fn unwrap_err_or_else(self, failure: impl FnOnce()) -> E;
}

impl<T, E> ResultExpectErr<T, E> for Result<T, E> {
    fn unwrap_err_or_else(self, failure: impl FnOnce()) -> E {
        match self {
            Ok(_) => {
                failure();
                panic!("expected an error")
            }
            Err(error) => error,
        }
    }
}

fn descriptor_registry(root: &Path) -> Vec<(PackagePath, mirante4d_storage::PackageObjectKind)> {
    LocalPackageCatalog::open(root)
        .expect("open pristine mutation base")
        .descriptors()
        .iter()
        .map(|descriptor| (descriptor.path().clone(), descriptor.kind()))
        .collect()
}

fn apply_mutation(root: &Path, recipe: &Value) {
    match string(recipe, "operation") {
        "remove_object" => {
            fs::remove_file(package_file(root, string(recipe, "object")))
                .expect("remove declared mutation object");
        }
        "truncate_object_tail" => {
            let path = package_file(root, string(recipe, "object"));
            let mut encoded = fs::read(&path).expect("read truncation object");
            let remove = usize_value(recipe, "remove_bytes");
            assert!(remove > 0 && remove < encoded.len());
            encoded.truncate(encoded.len() - remove);
            fs::write(path, encoded).expect("write truncated object");
        }
        "xor_object_byte" => {
            let path = package_file(root, string(recipe, "object"));
            let mut encoded = fs::read(&path).expect("read xor object");
            let offset = usize_value(recipe, "byte_offset");
            encoded[offset] ^= u8_value(recipe, "xor_mask");
            fs::write(path, encoded).expect("write xor object");
        }
        "xor_inner_crc32c_byte" | "xor_end_index_crc32c_byte" | "copy_inner_offset" => {
            mutate_shard_index_or_crc(root, recipe);
        }
        "replace_json_value" => replace_json_value(root, recipe),
        "add_object" => {
            let path = package_file(root, string(recipe, "object"));
            let bytes = decode_hex(string(recipe, "bytes_hex"));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .expect("create mutation object");
            file.write_all(&bytes).expect("write mutation object");
        }
        "replace_valid_sample_bits" => replace_valid_sample_bits(root, recipe),
        operation => panic!("unsupported frozen mutation operation {operation}"),
    }
}

fn mutate_shard_index_or_crc(root: &Path, recipe: &Value) {
    let object = string(recipe, "object");
    let path = package_file(root, object);
    let mut encoded = fs::read(&path).expect("read shard mutation object");
    let geometry = array_geometry(root, object);
    let slots = geometry.slots();
    let index_start = encoded.len() - slots * 16 - 4;
    let entries = parse_shard_entries(&encoded, slots, index_start);
    match string(recipe, "operation") {
        "xor_inner_crc32c_byte" => {
            let slot = usize_value(recipe, "inner_slot");
            let (offset, length) = entries[slot].expect("selected inner slot is present");
            let byte = usize_value(recipe, "crc_byte");
            encoded[offset + length - 4 + byte] ^= u8_value(recipe, "xor_mask");
        }
        "xor_end_index_crc32c_byte" => {
            let byte = usize_value(recipe, "crc_byte");
            let offset = encoded.len() - 4 + byte;
            encoded[offset] ^= u8_value(recipe, "xor_mask");
        }
        "copy_inner_offset" => {
            let source = usize_value(recipe, "source_slot");
            let target = usize_value(recipe, "target_slot");
            let (source_offset, _) = entries[source].expect("source slot is present");
            encoded[index_start + target * 16..index_start + target * 16 + 8]
                .copy_from_slice(&(source_offset as u64).to_le_bytes());
            let checksum = crc32c::crc32c(&encoded[index_start..encoded.len() - 4]);
            let checksum_at = encoded.len() - 4;
            encoded[checksum_at..].copy_from_slice(&checksum.to_le_bytes());
        }
        _ => unreachable!(),
    }
    fs::write(path, encoded).expect("write shard mutation object");
}

fn replace_json_value(root: &Path, recipe: &Value) {
    let path = package_file(root, string(recipe, "object"));
    let mut document: Value = read_json(&path);
    let pointer = string(recipe, "json_pointer");
    let target = document
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("JSON mutation pointer is absent: {pointer}"));
    assert_eq!(target, &recipe["original"]);
    *target = recipe["replacement"].clone();
    fs::write(path, canonical_json(&document)).expect("write JSON mutation");
}

fn replace_valid_sample_bits(root: &Path, recipe: &Value) {
    let mut sample = decode_declared_float_sample(root, recipe);
    let original = u32::from_le_bytes(
        sample.decoded[sample.byte_offset..sample.byte_offset + 4]
            .try_into()
            .expect("four float bytes"),
    );
    assert_eq!(format!("{original:08x}"), string(recipe, "original_bits"));
    let replacement = u32::from_str_radix(string(recipe, "replacement_bits"), 16)
        .expect("replacement float bits are hexadecimal");
    sample.decoded[sample.byte_offset..sample.byte_offset + 4]
        .copy_from_slice(&replacement.to_le_bytes());
    let replacement = encode_inner_payload(ShardProfileKind::Pixel3dFloat32, &sample.decoded)
        .expect("encode float mutation inner payload");
    let rebuilt = rebuild_shard(
        &sample.encoded,
        &sample.entries,
        sample.index_start,
        sample.slot,
        replacement,
    );
    fs::write(sample.path, rebuilt).expect("write float mutation shard");
}

fn assert_replacement_bits(root: &Path, recipe: &Value) {
    let sample = decode_declared_float_sample(root, recipe);
    let actual = u32::from_le_bytes(
        sample.decoded[sample.byte_offset..sample.byte_offset + 4]
            .try_into()
            .expect("four float bytes"),
    );
    assert_eq!(
        format!("{actual:08x}"),
        string(recipe, "replacement_bits"),
        "production-rewritten shard does not contain the declared replacement bits"
    );
}

struct DecodedFloatSample {
    path: PathBuf,
    encoded: Vec<u8>,
    entries: Vec<Option<(usize, usize)>>,
    index_start: usize,
    slot: usize,
    decoded: Vec<u8>,
    byte_offset: usize,
}

fn decode_declared_float_sample(root: &Path, recipe: &Value) -> DecodedFloatSample {
    let coordinate = recipe["logical_coordinate_tczyx"]
        .as_array()
        .expect("logical coordinate is an array")
        .iter()
        .map(|value| value.as_u64().expect("coordinate is unsigned"))
        .collect::<Vec<_>>();
    assert_eq!(coordinate.len(), 5);
    let profile: Value = read_json(&root.join("m4d/profile.json"));
    let logical_channel = coordinate[1];
    let physical_channel = profile["images"][0]["logical_layers"]
        .as_array()
        .expect("logical layers are an array")
        .iter()
        .find(|row| decimal_or_number(&row["logical_layer_ordinal"]) == logical_channel)
        .map(|row| decimal_or_number(&row["physical_channel"]))
        .expect("logical channel has a physical mapping");
    let pixel_base = profile["images"][0]["levels"][0]["pixel_path"]
        .as_str()
        .expect("pixel path is text");
    let metadata: Value = read_json(&package_file(root, &format!("{pixel_base}/zarr.json")));
    assert_eq!(metadata["data_type"], "float32");
    let outer = u64_array(&metadata["chunk_grid"]["configuration"]["chunk_shape"]);
    let inner = u64_array(&metadata["codecs"][0]["configuration"]["chunk_shape"]);
    let physical = [
        coordinate[0],
        physical_channel,
        coordinate[2],
        coordinate[3],
        coordinate[4],
    ];
    let outer_coordinate = physical
        .iter()
        .zip(&outer)
        .map(|(value, width)| value / width)
        .collect::<Vec<_>>();
    let ratios = outer
        .iter()
        .zip(&inner)
        .map(|(large, small)| large / small)
        .collect::<Vec<_>>();
    let inner_coordinate = physical
        .iter()
        .zip(&outer)
        .zip(&inner)
        .map(|((value, large), small)| (value % large) / small)
        .collect::<Vec<_>>();
    let slot = inner_coordinate
        .iter()
        .zip(&ratios)
        .fold(0_u64, |ordinal, (value, width)| ordinal * width + value) as usize;
    let object = format!(
        "{pixel_base}/c/{}",
        outer_coordinate
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join("/")
    );
    let path = package_file(root, &object);
    let encoded = fs::read(&path).expect("read float mutation shard");
    let geometry = ArrayGeometry {
        outer,
        inner: inner.clone(),
    };
    let slots = geometry.slots();
    let index_start = encoded.len() - slots * 16 - 4;
    let entries = parse_shard_entries(&encoded, slots, index_start);
    let (offset, length) = entries[slot].expect("float mutation inner slot is present");
    let decoded = decode_inner_payload(
        ShardProfileKind::Pixel3dFloat32,
        &encoded[offset..offset + length],
    )
    .expect("decode float mutation inner payload");
    let local = physical
        .iter()
        .zip(&inner)
        .map(|(value, width)| value % width)
        .collect::<Vec<_>>();
    let sample = local
        .iter()
        .zip(&inner)
        .fold(0_u64, |ordinal, (value, width)| ordinal * width + value) as usize;
    let byte_offset = sample * 4;
    DecodedFloatSample {
        path,
        encoded,
        entries,
        index_start,
        slot,
        decoded,
        byte_offset,
    }
}

fn rebuild_shard(
    old: &[u8],
    entries: &[Option<(usize, usize)>],
    index_start: usize,
    replacement_slot: usize,
    replacement: Vec<u8>,
) -> Vec<u8> {
    let mut payload = Vec::new();
    let mut index = Vec::with_capacity(entries.len() * 16);
    let mut cursor = 0_usize;
    for (slot, entry) in entries.iter().enumerate() {
        if let Some((offset, length)) = entry {
            assert_eq!(*offset, cursor);
            let bytes = if slot == replacement_slot {
                replacement.as_slice()
            } else {
                &old[*offset..*offset + *length]
            };
            index.extend_from_slice(&(payload.len() as u64).to_le_bytes());
            index.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            payload.extend_from_slice(bytes);
            cursor += length;
        } else {
            index.extend_from_slice(&MISSING.to_le_bytes());
            index.extend_from_slice(&MISSING.to_le_bytes());
        }
    }
    assert_eq!(cursor, index_start);
    let checksum = crc32c::crc32c(&index);
    payload.extend_from_slice(&index);
    payload.extend_from_slice(&checksum.to_le_bytes());
    payload
}

fn parse_shard_entries(
    encoded: &[u8],
    slots: usize,
    index_start: usize,
) -> Vec<Option<(usize, usize)>> {
    assert_eq!(encoded.len(), index_start + slots * 16 + 4);
    assert_eq!(
        crc32c::crc32c(&encoded[index_start..encoded.len() - 4]),
        u32::from_le_bytes(
            encoded[encoded.len() - 4..]
                .try_into()
                .expect("four CRC bytes")
        )
    );
    (0..slots)
        .map(|slot| {
            let at = index_start + slot * 16;
            let offset = u64::from_le_bytes(encoded[at..at + 8].try_into().expect("offset"));
            let length = u64::from_le_bytes(encoded[at + 8..at + 16].try_into().expect("length"));
            match (offset == MISSING, length == MISSING) {
                (true, true) => None,
                (false, false) => Some((
                    usize::try_from(offset).expect("offset fits usize"),
                    usize::try_from(length).expect("length fits usize"),
                )),
                _ => panic!("half-missing shard entry"),
            }
        })
        .collect()
}

struct ArrayGeometry {
    outer: Vec<u64>,
    inner: Vec<u64>,
}

impl ArrayGeometry {
    fn slots(&self) -> usize {
        self.outer
            .iter()
            .zip(&self.inner)
            .map(|(outer, inner)| usize::try_from(outer / inner).expect("ratio fits usize"))
            .product()
    }
}

fn array_geometry(root: &Path, object: &str) -> ArrayGeometry {
    let (base, _) = object
        .split_once("/c/")
        .unwrap_or_else(|| panic!("shard path lacks /c/: {object}"));
    let metadata: Value = read_json(&package_file(root, &format!("{base}/zarr.json")));
    ArrayGeometry {
        outer: u64_array(&metadata["chunk_grid"]["configuration"]["chunk_shape"]),
        inner: u64_array(&metadata["codecs"][0]["configuration"]["chunk_shape"]),
    }
}

fn reseal_manifest(root: &Path, registry: &[(PackagePath, mirante4d_storage::PackageObjectKind)]) {
    let descriptors = registry
        .iter()
        .filter_map(|(path, kind)| {
            let file = package_file(root, path.as_str());
            file.is_file().then(|| {
                let bytes = fs::read(file).expect("read resealed manifest object");
                let facts = ExactBytesHasher::hash(&bytes).expect("hash resealed manifest object");
                PackageObjectDescriptor::new(
                    path.clone(),
                    *kind,
                    facts.byte_length(),
                    facts.digest(),
                )
                .expect("construct resealed object descriptor")
            })
        })
        .collect();
    let pages = pack_manifest_pages(descriptors).expect("pack resealed manifest pages");
    let page_root = root.join("m4d/manifest/pages");
    for entry in fs::read_dir(&page_root).expect("read old manifest pages") {
        let path = entry.expect("read manifest-page entry").path();
        if path.is_file() {
            fs::remove_file(path).expect("remove old manifest page");
        }
    }
    for (ordinal, page) in pages.iter().enumerate() {
        fs::write(
            page_root.join(format!("p{ordinal:08}.json")),
            page.canonical_bytes().expect("encode resealed page"),
        )
        .expect("write resealed manifest page");
    }
    let root_manifest = ManifestRoot::new(&pages).expect("construct resealed manifest root");
    fs::write(
        root.join("m4d/manifest/root.json"),
        root_manifest
            .canonical_bytes()
            .expect("encode resealed manifest root"),
    )
    .expect("write resealed manifest root");
}

fn tree_digest(root: &Path) -> String {
    let mut paths = Vec::new();
    collect_files(root, root, &mut paths);
    paths.sort();
    let rows = paths
        .into_iter()
        .map(|relative| {
            let bytes = fs::read(root.join(&relative)).expect("read tree object");
            let mut row = serde_json::Map::new();
            row.insert("bytes".to_owned(), Value::from(bytes.len() as u64));
            row.insert(
                "path".to_owned(),
                Value::from(relative.to_string_lossy().replace('\\', "/")),
            );
            row.insert(
                "sha256".to_owned(),
                Value::from(Sha256Hasher::digest(&bytes).to_string()),
            );
            Value::Object(row)
        })
        .collect::<Vec<_>>();
    Sha256Hasher::digest(canonical_json(&Value::Array(rows))).to_string()
}

fn collect_files(root: &Path, directory: &Path, output: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(directory)
        .expect("read package tree")
        .map(|entry| entry.expect("read package-tree entry"))
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let metadata = fs::symlink_metadata(entry.path()).expect("inspect package-tree entry");
        assert!(!metadata.file_type().is_symlink());
        if metadata.is_dir() {
            collect_files(root, &entry.path(), output);
        } else {
            assert!(metadata.is_file());
            output.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .expect("tree path stays below root")
                    .to_owned(),
            );
        }
    }
}

fn read_archive(repository: &Path, archive: &ArchiveAuthority) -> Vec<u8> {
    let relative = checked_repository_path(&archive.path);
    assert!(relative.starts_with("fixtures/target/archives"));
    let encoded = fs::read(repository.join(relative)).expect("read promoted target archive");
    assert_eq!(encoded.len() as u64, archive.bytes);
    assert!(encoded.len() <= ARCHIVE_BYTES_MAX);
    assert_eq!(Sha256Hasher::digest(&encoded).to_string(), archive.sha256);
    encoded
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
        let end = offset.checked_add(size).expect("USTAR size overflow");
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

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("storage crate is inside repository")
        .to_owned()
}

fn package_file(root: &Path, value: &str) -> PathBuf {
    root.join(checked_archive_path(value))
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

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> T {
    serde_json::from_slice(
        &fs::read(path).unwrap_or_else(|error| panic!("read JSON {}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("parse JSON {}: {error}", path.display()))
}

fn canonical_json(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("encode canonical test JSON")
}

fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key]
        .as_str()
        .unwrap_or_else(|| panic!("{key} is not text"))
}

fn usize_value(value: &Value, key: &str) -> usize {
    usize::try_from(
        value[key]
            .as_u64()
            .unwrap_or_else(|| panic!("{key} is not unsigned")),
    )
    .expect("mutation integer fits usize")
}

fn u8_value(value: &Value, key: &str) -> u8 {
    u8::try_from(
        value[key]
            .as_u64()
            .unwrap_or_else(|| panic!("{key} is not unsigned")),
    )
    .expect("mutation integer fits u8")
}

fn u64_array(value: &Value) -> Vec<u64> {
    value
        .as_array()
        .expect("value is an array")
        .iter()
        .map(|value| value.as_u64().expect("array value is unsigned"))
        .collect()
}

fn decimal_or_number(value: &Value) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .expect("value is an unsigned integer or canonical decimal")
}

fn decode_hex(value: &str) -> Vec<u8> {
    assert!(value.len().is_multiple_of(2));
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).expect("hex is ASCII"), 16)
                .expect("valid hexadecimal byte")
        })
        .collect()
}
