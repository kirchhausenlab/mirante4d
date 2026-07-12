use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use mirante4d_domain::IntensityDType;
use mirante4d_identity::{SCIENTIFIC_TILE_SHAPE_TZYX, Sha256Hasher};
use mirante4d_storage::{
    LocalPackageCatalog, OmeInteroperabilityBase, PackedIndexCoordinates, ProfileKind,
    ProfileValidityMode, ShardProfileKind, VerifiedScientificPackageCapability,
};
use serde::Deserialize;
use serde_json::Value;

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
    max_depth: u64,
    max_fan_out: u64,
}

#[derive(Deserialize)]
struct FileAuthority {
    bytes: u64,
    sha256: String,
}

#[derive(Deserialize)]
struct ExpectedFacts {
    cases: Vec<CaseFacts>,
}

#[derive(Deserialize)]
struct CaseFacts {
    case_id: String,
    dtype: String,
    shape_tczyx: [u64; 5],
    level_count: u64,
    validity_mode: String,
    physical_mapping: Vec<LayerMapping>,
    temporal_step_f64_bits: String,
    grid_to_world_f64_bits: Vec<String>,
    ome_projection: String,
    scientific_content_id: String,
    scientific_layer_roots: Vec<LayerRootFact>,
    levels: Vec<LevelFacts>,
}

#[derive(Deserialize)]
struct LayerRootFact {
    logical_layer: u32,
    sha256: String,
}

#[derive(Deserialize)]
struct LayerMapping {
    logical_layer: u32,
    physical_channel: u32,
}

#[derive(Deserialize)]
struct LevelFacts {
    ordinal: u32,
    shape_tczyx: [u64; 5],
    raw_values_sha256: String,
    canonical_values_sha256: String,
    validity_sha256: String,
    layers: Vec<LayerDigestFacts>,
    selected_facts: Vec<SelectedFact>,
    brick_statistics: Vec<BrickFact>,
}

#[derive(Deserialize)]
struct LayerDigestFacts {
    logical_layer: u32,
    physical_channel: u32,
    raw_values_sha256: String,
    canonical_values_sha256: String,
    validity_sha256: String,
}

#[derive(Deserialize)]
struct SelectedFact {
    coordinate_tczyx: [u64; 5],
    physical_channel: u32,
    valid: bool,
    raw_value: Value,
    canonical_value: Value,
}

#[derive(Deserialize)]
struct BrickFact {
    t: u32,
    logical_layer: u32,
    brick_zyx: [u32; 3],
    extent_zyx: [u64; 3],
    voxel_count: u64,
    valid_count: u64,
    nonfill_valid_count: u64,
    minimum: Option<Value>,
    maximum: Option<Value>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AmplificationMaxima {
    range_requests: u8,
    encoded_bytes_read: u64,
    decoded_bytes: u64,
}

struct LayerBuffers {
    raw: Vec<u8>,
    canonical: Vec<u8>,
    validity: Vec<u8>,
    written: Vec<bool>,
}

struct ScratchDirectory(PathBuf);

impl ScratchDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let suffix = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mirante4d-target-conformance-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create isolated target-conformance directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove isolated target-conformance directory");
    }
}

#[test]
fn production_reader_consumes_all_positive_target_packages() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("storage crate is inside the repository");
    let manifest: AuthorityManifest = serde_json::from_slice(
        &fs::read(repository.join("fixtures/target/manifest.json"))
            .expect("read promoted target authority"),
    )
    .expect("parse promoted target authority");
    let expected: ExpectedFacts = serde_json::from_slice(
        &fs::read(repository.join("fixtures/target/expected-facts.json"))
            .expect("read independent target facts"),
    )
    .expect("parse independent target facts");

    assert_eq!(manifest.archives.len(), 3);
    assert_eq!(expected.cases.len(), 3);
    for archive in &manifest.archives {
        let facts = expected
            .cases
            .iter()
            .find(|case| case.case_id == archive.case_id)
            .expect("every promoted archive has independent facts");
        exercise_case(repository, archive, facts);
    }
}

fn exercise_case(repository: &Path, archive: &ArchiveAuthority, facts: &CaseFacts) {
    let relative = checked_repository_path(&archive.path);
    assert!(relative.starts_with("fixtures/target/archives"));
    let encoded = fs::read(repository.join(relative)).expect("read promoted target archive");
    assert_eq!(encoded.len() as u64, archive.bytes);
    assert!(encoded.len() <= ARCHIVE_BYTES_MAX);
    assert_eq!(Sha256Hasher::digest(&encoded).to_string(), archive.sha256);

    let scratch = ScratchDirectory::new();
    extract_ustar(&encoded, scratch.path(), &archive.inventory);

    let capability = validate_target_package(scratch.path());
    assert_eq!(capability.package_id().to_string(), archive.package_id);
    assert_eq!(
        capability
            .catalog()
            .profile()
            .scientific_content_id()
            .to_string(),
        facts.scientific_content_id
    );
    assert_scientific_report(&capability, facts);
    assert_eq!(
        capability
            .catalog()
            .science()
            .scientific_content_id()
            .to_string(),
        facts.scientific_content_id
    );
    assert_eq!(
        capability.admission().counts().total_physical_objects,
        archive.inventory.file_count
    );
    assert_eq!(
        capability.admission().counts().directories,
        archive.inventory.directory_count + 1
    );
    assert_package_accounting(&capability, archive, facts);
    assert_layout(&capability, facts);
    let maxima = assert_all_bricks(&capability, facts);
    assert_amplification(maxima, facts);
}

fn assert_scientific_report(capability: &VerifiedScientificPackageCapability, facts: &CaseFacts) {
    assert_eq!(
        capability.scientific_content_id().to_string(),
        facts.scientific_content_id
    );
    assert_eq!(
        capability.layer_roots().len(),
        facts.scientific_layer_roots.len()
    );
    for (actual, expected) in capability
        .layer_roots()
        .iter()
        .zip(&facts.scientific_layer_roots)
    {
        assert_eq!(actual.layer().ordinal(), expected.logical_layer);
        assert_eq!(actual.digest().to_string(), expected.sha256);
    }

    let report = capability.validation_report();
    let logical_voxels = facts.shape_tczyx.into_iter().product::<u64>();
    assert_eq!(report.layer_count(), facts.physical_mapping.len() as u32);
    assert_eq!(report.identity_tiles(), expected_identity_tiles(facts));
    assert_eq!(report.brick_reads(), expected_scientific_brick_reads(facts));
    assert_eq!(report.logical_voxels(), logical_voxels);
    assert_eq!(
        report.canonical_value_bytes(),
        logical_voxels * u64::from(dtype_from_name(&facts.dtype).bytes_per_sample())
    );
    assert_eq!(
        report.validity_bytes(),
        expected_identity_validity_bytes(facts)
    );
}

fn validate_target_package(root: &Path) -> VerifiedScientificPackageCapability {
    LocalPackageCatalog::open(root)
        .expect("production catalog opens the independent package")
        .validate_exact_package(ProfileKind::Ds0, || false)
        .expect("production exact validation accepts the independent package")
        .validate_scientific_content(|| false)
        .expect("production scientific validation accepts the independent package")
}

fn assert_layout(capability: &VerifiedScientificPackageCapability, facts: &CaseFacts) {
    let catalog = capability.catalog();
    let profile = catalog.profile();
    assert_eq!(profile.images().len(), 1);
    let image = &profile.images()[0];
    assert_eq!(image.levels().len() as u64, facts.level_count);
    assert_eq!(image.logical_layers().len(), facts.physical_mapping.len());
    for (actual, expected) in image.logical_layers().iter().zip(&facts.physical_mapping) {
        assert_eq!(actual.logical_layer().ordinal(), expected.logical_layer);
        assert_eq!(actual.physical_channel(), expected.physical_channel);
    }
    assert_eq!(
        profile.ome_interoperability_base(),
        if facts.ome_projection == "unitless_identity" {
            OmeInteroperabilityBase::Io1
        } else {
            OmeInteroperabilityBase::Io2
        }
    );

    assert_eq!(
        catalog.science().layers().len(),
        facts.physical_mapping.len()
    );
    for layer in catalog.science().layers() {
        let expected = &facts.physical_mapping[layer.logical_layer().ordinal() as usize];
        assert_eq!(layer.logical_layer().ordinal(), expected.logical_layer);
        assert_eq!(
            layer.base_shape().dimensions(),
            [
                facts.shape_tczyx[0],
                facts.shape_tczyx[2],
                facts.shape_tczyx[3],
                facts.shape_tczyx[4],
            ]
        );
        assert_eq!(dtype_name(layer.dtype()), facts.dtype);
        assert_eq!(
            format!(
                "{:016x}",
                layer
                    .temporal_calibration()
                    .regular_step_seconds()
                    .expect("regular time")
                    .bits()
            ),
            facts.temporal_step_f64_bits
        );
        assert_eq!(
            layer
                .grid_to_world_micrometer_f64_bits()
                .iter()
                .map(|value| format!("{:016x}", value.bits()))
                .collect::<Vec<_>>(),
            facts.grid_to_world_f64_bits
        );
    }

    for (level, expected) in image.levels().iter().zip(&facts.levels) {
        assert_eq!(level.scale_ordinal(), expected.ordinal);
        assert_eq!(
            level.validity_mode(),
            if facts.validity_mode == "explicit" {
                ProfileValidityMode::Explicit
            } else {
                ProfileValidityMode::AllValid
            }
        );
        let metadata_path =
            mirante4d_storage::PackagePath::parse(&format!("{}/zarr.json", level.pixel_path()))
                .expect("canonical pixel metadata path");
        assert_eq!(
            catalog
                .zarr_array(&metadata_path)
                .expect("pixel metadata")
                .shape(),
            expected.shape_tczyx
        );
    }
}

fn assert_package_accounting(
    capability: &VerifiedScientificPackageCapability,
    archive: &ArchiveAuthority,
    facts: &CaseFacts,
) {
    let counts = capability.admission().counts();
    let inventory = capability
        .catalog()
        .inspect_directory_closure(|| false)
        .expect("production inventory remains coherent");
    assert_eq!(inventory.regular_files(), archive.inventory.file_count);
    assert_eq!(
        inventory.directories(),
        archive.inventory.directory_count + 1
    );
    assert_eq!(
        inventory.maximum_directory_depth(),
        archive.inventory.max_depth
    );
    assert_eq!(
        inventory.maximum_directory_fan_out(),
        archive.inventory.max_fan_out
    );
    assert_eq!(counts.maximum_directory_depth, archive.inventory.max_depth);
    assert_eq!(
        counts.maximum_directory_fan_out,
        archive.inventory.max_fan_out
    );
    assert_eq!(counts.actual_pixel_shards, inventory.pixel_shards());
    assert_eq!(counts.actual_validity_shards, inventory.validity_shards());
    assert_eq!(
        counts.actual_packed_index_shards,
        inventory.packed_index_shards()
    );
    assert_eq!(
        counts.zarr_metadata_objects,
        inventory.zarr_metadata_objects()
    );
    assert_eq!(
        counts.portable_provenance_records,
        inventory.portable_records()
    );
    assert_eq!(counts.manifest_pages, inventory.manifest_pages());
    assert_eq!(inventory.fixed_control_objects(), 4);
    assert_eq!(counts.actual_pixel_shards, addressed_pixel_shards(facts));
    assert_eq!(counts.addressed_pixel_shards, addressed_pixel_shards(facts));
    assert_eq!(
        counts.addressed_validity_shards,
        if facts.validity_mode == "explicit" {
            addressed_pixel_shards(facts)
        } else {
            0
        }
    );
    assert!(counts.actual_validity_shards <= counts.addressed_validity_shards);
    assert_eq!(counts.addressed_packed_index_shards, facts.level_count);
    assert_eq!(
        counts.actual_packed_index_shards,
        counts.addressed_packed_index_shards
    );
    assert_eq!(
        counts
            .recomputed_total_physical_objects()
            .expect("bounded object count"),
        archive.inventory.file_count
    );
}

fn assert_all_bricks(
    capability: &VerifiedScientificPackageCapability,
    facts: &CaseFacts,
) -> AmplificationMaxima {
    let image = &capability.catalog().profile().images()[0];
    let physical_to_logical = facts
        .physical_mapping
        .iter()
        .map(|row| (row.physical_channel, row.logical_layer))
        .collect::<BTreeMap<_, _>>();
    let mut brick_facts_seen = 0_usize;
    let mut selected_seen = 0_usize;
    let mut brick_reads = 0_u64;
    let mut maxima = AmplificationMaxima::default();

    for (level, expected_level) in image.levels().iter().zip(&facts.levels) {
        let metadata_path =
            mirante4d_storage::PackagePath::parse(&format!("{}/zarr.json", level.pixel_path()))
                .expect("canonical pixel metadata path");
        let metadata = capability
            .catalog()
            .zarr_array(&metadata_path)
            .expect("pixel metadata");
        let shape: [u64; 5] = metadata
            .shape()
            .try_into()
            .expect("five-dimensional pixels");
        let inner = pixel_inner_shape(metadata.kind());
        let width = pixel_width(metadata.kind());
        let layer_voxels = checked_product(&[shape[0], shape[2], shape[3], shape[4]]);
        let mut buffers = (0..shape[1])
            .map(|_| LayerBuffers {
                raw: vec![0; layer_voxels * width],
                canonical: vec![0; layer_voxels * width],
                validity: vec![0; layer_voxels],
                written: vec![false; layer_voxels],
            })
            .collect::<Vec<_>>();
        let grid = [
            ceil_div(shape[2], inner[0]),
            ceil_div(shape[3], inner[1]),
            ceil_div(shape[4], inner[2]),
        ];
        for t in 0..shape[0] {
            for physical_channel in 0..shape[1] {
                let logical_layer = physical_to_logical[&(physical_channel as u32)];
                for z in 0..grid[0] {
                    for y in 0..grid[1] {
                        for x in 0..grid[2] {
                            let coordinates = PackedIndexCoordinates::new(
                                image.image_ordinal(),
                                level.scale_ordinal(),
                                t as u32,
                                physical_channel as u32,
                                z as u32,
                                y as u32,
                                x as u32,
                            );
                            let read = capability
                                .read_brick(coordinates, || false)
                                .expect("production reader decodes every logical brick");
                            maxima.range_requests =
                                maxima.range_requests.max(read.range_requests());
                            maxima.encoded_bytes_read =
                                maxima.encoded_bytes_read.max(read.encoded_bytes_read());
                            maxima.decoded_bytes = maxima.decoded_bytes.max(read.decoded_bytes());
                            brick_reads += 1;
                            assert_eq!(read.record().coordinates(), coordinates);
                            let expected_brick = expected_level
                                .brick_statistics
                                .iter()
                                .find(|row| {
                                    row.t == t as u32
                                        && row.logical_layer == logical_layer
                                        && row.brick_zyx == [z as u32, y as u32, x as u32]
                                })
                                .expect("independent facts cover every logical brick");
                            assert_brick_fact(&read, expected_brick, &facts.dtype);
                            brick_facts_seen += 1;
                            copy_brick_into_layer(
                                &read,
                                &mut buffers[logical_layer as usize],
                                t,
                                [z, y, x],
                                shape,
                                inner,
                                metadata.kind(),
                            );

                            for selected in expected_level.selected_facts.iter().filter(|row| {
                                row.coordinate_tczyx[0] == t
                                    && row.physical_channel == physical_channel as u32
                                    && row.coordinate_tczyx[2] / inner[0] == z
                                    && row.coordinate_tczyx[3] / inner[1] == y
                                    && row.coordinate_tczyx[4] / inner[2] == x
                            }) {
                                assert_selected(&read, selected, metadata.kind(), inner);
                                selected_seen += 1;
                            }
                        }
                    }
                }
            }
        }
        assert_level_digests(&buffers, expected_level, shape);
    }

    let expected_bricks = facts
        .levels
        .iter()
        .map(|level| level.brick_statistics.len())
        .sum::<usize>();
    let expected_selected = facts
        .levels
        .iter()
        .map(|level| level.selected_facts.len())
        .sum::<usize>();
    assert_eq!(brick_facts_seen, expected_bricks);
    assert_eq!(selected_seen, expected_selected);
    assert_eq!(brick_reads, capability.admission().counts().logical_bricks);
    maxima
}

#[allow(clippy::too_many_arguments)]
fn copy_brick_into_layer(
    read: &mirante4d_storage::LocalBrickRead,
    output: &mut LayerBuffers,
    t: u64,
    brick_zyx: [u64; 3],
    shape: [u64; 5],
    inner: [u64; 3],
    kind: ShardProfileKind,
) {
    let width = pixel_width(kind);
    let extent = read.logical_extent_zyx();
    for local_z in 0..extent[0] {
        for local_y in 0..extent[1] {
            for local_x in 0..extent[2] {
                let local = [local_z, local_y, local_x];
                let sample = checked_product(&[local[0], inner[1], inner[2]])
                    + checked_product(&[local[1], inner[2]])
                    + local[2] as usize;
                let raw = read.pixel_payload().map_or_else(
                    || vec![0; width],
                    |payload| payload[sample * width..(sample + 1) * width].to_vec(),
                );
                let valid = validity_at(read, sample);
                let global = [
                    brick_zyx[0] * inner[0] + local[0],
                    brick_zyx[1] * inner[1] + local[1],
                    brick_zyx[2] * inner[2] + local[2],
                ];
                let logical = checked_product(&[t, shape[2], shape[3], shape[4]])
                    + checked_product(&[global[0], shape[3], shape[4]])
                    + checked_product(&[global[1], shape[4]])
                    + global[2] as usize;
                assert!(!output.written[logical], "logical voxel written twice");
                output.written[logical] = true;
                output.raw[logical * width..(logical + 1) * width].copy_from_slice(&raw);
                if valid {
                    output.canonical[logical * width..(logical + 1) * width].copy_from_slice(&raw);
                    output.validity[logical] = 1;
                }
            }
        }
    }
}

fn assert_level_digests(buffers: &[LayerBuffers], expected: &LevelFacts, shape: [u64; 5]) {
    assert_eq!(buffers.len(), expected.layers.len());
    let mut level_raw =
        Vec::with_capacity(buffers.iter().map(|layer| layer.raw.len()).sum::<usize>());
    let mut level_canonical = Vec::with_capacity(level_raw.capacity());
    let mut level_validity = Vec::new();

    for (logical_layer, (actual, expected_layer)) in
        buffers.iter().zip(&expected.layers).enumerate()
    {
        assert_eq!(expected_layer.logical_layer, logical_layer as u32);
        assert!(expected_layer.physical_channel < shape[1] as u32);
        assert!(actual.written.iter().all(|written| *written));
        assert_eq!(
            Sha256Hasher::digest(&actual.raw).to_string(),
            expected_layer.raw_values_sha256
        );
        assert_eq!(
            Sha256Hasher::digest(&actual.canonical).to_string(),
            expected_layer.canonical_values_sha256
        );
        let packed = pack_validity(&actual.validity);
        assert_eq!(
            Sha256Hasher::digest(&packed).to_string(),
            expected_layer.validity_sha256
        );
        level_raw.extend_from_slice(&actual.raw);
        level_canonical.extend_from_slice(&actual.canonical);
        level_validity.extend_from_slice(&packed);
    }
    assert_eq!(
        Sha256Hasher::digest(&level_raw).to_string(),
        expected.raw_values_sha256
    );
    assert_eq!(
        Sha256Hasher::digest(&level_canonical).to_string(),
        expected.canonical_values_sha256
    );
    assert_eq!(
        Sha256Hasher::digest(&level_validity).to_string(),
        expected.validity_sha256
    );
}

fn assert_brick_fact(read: &mirante4d_storage::LocalBrickRead, expected: &BrickFact, dtype: &str) {
    assert_eq!(read.logical_extent_zyx(), expected.extent_zyx);
    assert_eq!(
        expected.extent_zyx.into_iter().product::<u64>(),
        expected.voxel_count
    );
    let statistics = read.record().statistics();
    assert_eq!(statistics.valid_voxel_count(), expected.valid_count);
    assert_eq!(
        statistics.nonfill_valid_voxel_count(),
        expected.nonfill_valid_count
    );
    let range = match (&expected.minimum, &expected.maximum) {
        (Some(minimum), Some(maximum)) => {
            Some((fact_bits(minimum, dtype), fact_bits(maximum, dtype)))
        }
        (None, None) => None,
        _ => panic!("independent numeric range is incomplete"),
    };
    assert_eq!(statistics.numeric_range_bits(), range);
}

fn assert_selected(
    read: &mirante4d_storage::LocalBrickRead,
    expected: &SelectedFact,
    kind: ShardProfileKind,
    inner: [u64; 3],
) {
    let coordinate = expected.coordinate_tczyx;
    let local = [
        coordinate[2] % inner[0],
        coordinate[3] % inner[1],
        coordinate[4] % inner[2],
    ];
    let sample = (local[0] * inner[1] * inner[2] + local[1] * inner[2] + local[2]) as usize;
    let width = pixel_width(kind);
    let raw = read.pixel_payload().map_or_else(
        || vec![0; width],
        |payload| payload[sample * width..(sample + 1) * width].to_vec(),
    );
    let valid = validity_at(read, sample);
    assert_eq!(valid, expected.valid);
    assert_eq!(
        sample_bits(&raw),
        fact_bits(&expected.raw_value, dtype_name_for_kind(kind))
    );
    let canonical = if valid { sample_bits(&raw) } else { 0 };
    assert_eq!(
        canonical,
        fact_bits(&expected.canonical_value, dtype_name_for_kind(kind))
    );
}

fn validity_at(read: &mirante4d_storage::LocalBrickRead, sample: usize) -> bool {
    match read.validity_payload() {
        Some(payload) => payload[sample / 8] & (1 << (sample % 8)) != 0,
        None if read.record().explicit_validity() => read.record().all_voxels_valid(),
        None => true,
    }
}

fn pack_validity(values: &[u8]) -> Vec<u8> {
    let mut packed = vec![0_u8; values.len().div_ceil(8)];
    for (index, value) in values.iter().copied().enumerate() {
        assert!(value <= 1);
        packed[index / 8] |= value << (index % 8);
    }
    packed
}

fn assert_amplification(maxima: AmplificationMaxima, facts: &CaseFacts) {
    let expected = match facts.case_id.as_str() {
        "m4d-t1-u8-2d-sparse" => AmplificationMaxima {
            range_requests: 4,
            encoded_bytes_read: 1_413,
            decoded_bytes: 83_200,
        },
        "m4d-t1-u16-3d-multiscale" => AmplificationMaxima {
            range_requests: 4,
            encoded_bytes_read: 27_672,
            decoded_bytes: 542_720,
        },
        "m4d-t1-f32-3d-validity" => AmplificationMaxima {
            range_requests: 6,
            encoded_bytes_read: 3_400,
            decoded_bytes: 1_100_800,
        },
        _ => panic!("unregistered target-conformance case"),
    };
    assert_eq!(maxima, expected);
    let dtype = dtype_from_name(&facts.dtype);
    let limits = if facts.shape_tczyx[2] == 1 {
        mirante4d_storage::amplification_2d(dtype)
    } else {
        mirante4d_storage::amplification_3d(dtype)
    };
    assert!(maxima.range_requests > 0);
    assert!(maxima.encoded_bytes_read > 0);
    assert!(maxima.decoded_bytes > 0);
    assert!(maxima.range_requests <= limits.cold_range_requests_max);
    assert!(maxima.encoded_bytes_read <= limits.read_bytes_max);
    assert!(maxima.decoded_bytes <= limits.decoded_bytes_max);
}

fn expected_identity_tiles(facts: &CaseFacts) -> u64 {
    let [t, c, z, y, x] = facts.shape_tczyx;
    t * c
        * z.div_ceil(SCIENTIFIC_TILE_SHAPE_TZYX[1])
        * y.div_ceil(SCIENTIFIC_TILE_SHAPE_TZYX[2])
        * x.div_ceil(SCIENTIFIC_TILE_SHAPE_TZYX[3])
}

fn expected_identity_validity_bytes(facts: &CaseFacts) -> u64 {
    identity_tile_extents(facts)
        .into_iter()
        .map(|extent| extent.into_iter().product::<u64>().div_ceil(8))
        .sum()
}

fn expected_scientific_brick_reads(facts: &CaseFacts) -> u64 {
    let brick = if facts.shape_tczyx[2] == 1 {
        [1, 256, 256]
    } else {
        [64, 64, 64]
    };
    let [_, _, z, y, x] = facts.shape_tczyx;
    let tile = [
        SCIENTIFIC_TILE_SHAPE_TZYX[1],
        SCIENTIFIC_TILE_SHAPE_TZYX[2],
        SCIENTIFIC_TILE_SHAPE_TZYX[3],
    ];
    let mut reads_per_layer_timepoint = 0_u64;
    for z_origin in (0..z).step_by(tile[0] as usize) {
        for y_origin in (0..y).step_by(tile[1] as usize) {
            for x_origin in (0..x).step_by(tile[2] as usize) {
                let origin = [z_origin, y_origin, x_origin];
                let end = [
                    (z_origin + tile[0]).min(z),
                    (y_origin + tile[1]).min(y),
                    (x_origin + tile[2]).min(x),
                ];
                reads_per_layer_timepoint += (0..3)
                    .map(|axis| (end[axis] - 1) / brick[axis] - origin[axis] / brick[axis] + 1)
                    .product::<u64>();
            }
        }
    }
    reads_per_layer_timepoint * facts.shape_tczyx[0] * facts.shape_tczyx[1]
}

fn identity_tile_extents(facts: &CaseFacts) -> Vec<[u64; 4]> {
    let [t, c, z, y, x] = facts.shape_tczyx;
    let mut extents = Vec::new();
    for _logical_layer in 0..c {
        for _timepoint in 0..t {
            for z_origin in (0..z).step_by(SCIENTIFIC_TILE_SHAPE_TZYX[1] as usize) {
                for y_origin in (0..y).step_by(SCIENTIFIC_TILE_SHAPE_TZYX[2] as usize) {
                    for x_origin in (0..x).step_by(SCIENTIFIC_TILE_SHAPE_TZYX[3] as usize) {
                        extents.push([
                            1,
                            SCIENTIFIC_TILE_SHAPE_TZYX[1].min(z - z_origin),
                            SCIENTIFIC_TILE_SHAPE_TZYX[2].min(y - y_origin),
                            SCIENTIFIC_TILE_SHAPE_TZYX[3].min(x - x_origin),
                        ]);
                    }
                }
            }
        }
    }
    extents
}

fn addressed_pixel_shards(facts: &CaseFacts) -> u64 {
    facts
        .levels
        .iter()
        .map(|level| {
            let shape = level.shape_tczyx;
            let outer = if shape[2] == 1 {
                [1, 1, 1, 1_024, 1_024]
            } else {
                [1, 1, 256, 256, 256]
            };
            shape
                .into_iter()
                .zip(outer)
                .map(|(dimension, group)| dimension.div_ceil(group))
                .product::<u64>()
        })
        .sum()
}

fn checked_product(values: &[u64]) -> usize {
    let product = values
        .iter()
        .try_fold(1_u64, |total, value| total.checked_mul(*value))
        .expect("T1 logical count is bounded");
    usize::try_from(product).expect("T1 logical count fits usize")
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

fn pixel_width(kind: ShardProfileKind) -> usize {
    match kind {
        ShardProfileKind::Pixel3dUint8 | ShardProfileKind::Pixel2dUint8 => 1,
        ShardProfileKind::Pixel3dUint16 | ShardProfileKind::Pixel2dUint16 => 2,
        ShardProfileKind::Pixel3dFloat32 | ShardProfileKind::Pixel2dFloat32 => 4,
        _ => panic!("expected pixel storage kind"),
    }
}

fn dtype_name(dtype: IntensityDType) -> &'static str {
    match dtype {
        IntensityDType::Uint8 => "uint8",
        IntensityDType::Uint16 => "uint16",
        IntensityDType::Float32 => "float32",
    }
}

fn dtype_from_name(dtype: &str) -> IntensityDType {
    match dtype {
        "uint8" => IntensityDType::Uint8,
        "uint16" => IntensityDType::Uint16,
        "float32" => IntensityDType::Float32,
        _ => panic!("unsupported expected dtype"),
    }
}

fn dtype_name_for_kind(kind: ShardProfileKind) -> &'static str {
    match kind {
        ShardProfileKind::Pixel3dUint8 | ShardProfileKind::Pixel2dUint8 => "uint8",
        ShardProfileKind::Pixel3dUint16 | ShardProfileKind::Pixel2dUint16 => "uint16",
        ShardProfileKind::Pixel3dFloat32 | ShardProfileKind::Pixel2dFloat32 => "float32",
        _ => panic!("expected pixel storage kind"),
    }
}

fn sample_bits(bytes: &[u8]) -> u64 {
    match bytes {
        [value] => u64::from(*value),
        [a, b] => u64::from(u16::from_le_bytes([*a, *b])),
        [a, b, c, d] => u64::from(u32::from_le_bytes([*a, *b, *c, *d])),
        _ => panic!("unsupported sample width"),
    }
}

fn fact_bits(value: &Value, dtype: &str) -> u64 {
    match dtype {
        "uint8" | "uint16" => value["uint"].as_u64().expect("unsigned expected fact"),
        "float32" => u64::from_str_radix(
            value["f32_bits"].as_str().expect("float-bit expected fact"),
            16,
        )
        .expect("canonical float-bit expected fact"),
        _ => panic!("unsupported expected dtype"),
    }
}

fn ceil_div(value: u64, divisor: u64) -> u64 {
    value.div_ceil(divisor)
}
