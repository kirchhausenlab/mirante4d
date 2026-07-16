use std::{
    cmp,
    fs::{self, File, OpenOptions},
    os::unix::{
        ffi::OsStrExt,
        fs::{FileExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use mirante4d_domain::{GridToWorld, IntensityDType, LogicalLayerKey, Shape4D};
use mirante4d_identity::{
    SCIENTIFIC_TILE_SHAPE_TZYX, ScientificDatasetHasher, ScientificLayerDescriptor,
    ScientificLayerHasher, ScientificTemporalCalibration, ScientificTile, Sha256Hasher,
};
use serde::Serialize;
use tiff::{
    ColorType,
    decoder::{Decoder, DecodingResult, Limits},
    tags::{SampleFormat, Tag},
};

pub(crate) const FACT_AUTHORITY: &str = "mirante4d-t5-source-derived-guarded-sentinel-oracle-1";

const T5_SCALE_COUNT: usize = 7;
const MAX_SOURCE_FILES: usize = 4_096;
const MAX_SOURCE_NAME_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SINGLE_NAME_BYTES: usize = 4_096;
const MAX_TIFF_OVERHEAD_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TIFF_IFD_ENTRIES: usize = 4_096;
const MAX_TIFF_CHUNKS: u64 = 4_096;
const MAX_TIFF_EXTERNAL_VALUE_BYTES: u64 = 8 * 1024 * 1024;
const INVENTORY_BYTES_PER_FILE: u64 = 2 * 1024;
const INVENTORY_FIXED_BYTES: u64 = 256 * 1024;
const ORACLE_FIXED_BYTES: u64 = 8 * 1024 * 1024;
// This covers the independently bounded aggregate decoder tag values, two
// maximum entry/range vectors (including allocator slack), and decoder maps.
const DECODER_FIXED_BYTES: u64 = MAX_TIFF_EXTERNAL_VALUE_BYTES
    + MAX_TIFF_IFD_ENTRIES as u64 * 256
    + MAX_TIFF_CHUNKS * 64
    + 1024 * 1024;
const SCRATCH_PREFIX: &str = ".mirante4d-t5-sentinel-oracle-";
const OPEN_FILE_BOUND: u64 = 4;

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy)]
pub(crate) struct OracleRequest<'a> {
    pub source: &'a Path,
    pub scratch: &'a Path,
    pub sentinel: u8,
    pub spacing_zyx_um: [f64; 3],
    pub time_step_seconds: Option<f64>,
    pub working_memory_bytes: u64,
    pub scale_digest_scheme: &'a str,
    pub cancelled: &'a AtomicBool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct OracleFacts {
    pub scientific_content_id: String,
    pub layer_roots: Vec<OracleLayerRoot>,
    pub scales: Vec<OracleScaleFact>,
    pub transforms: Vec<OracleTransformFact>,
    pub canonical_base_pixel_bytes: u64,
    pub resources: OracleResources,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct OracleLayerRoot {
    pub logical_layer: u32,
    pub digest_sha256: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct OracleScaleFact {
    pub image_ordinal: u32,
    pub scale_ordinal: u32,
    pub digest_sha256: String,
    pub brick_reads: u64,
    pub logical_voxels: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct OracleTransformFact {
    pub scale_ordinal: u32,
    pub scale_zyx: [f64; 3],
    pub translation_zyx: [f64; 3],
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct OracleResources {
    pub calculated_peak_working_bytes: u64,
    pub required_scratch_bytes: u64,
    pub scratch_free_bytes: u64,
    pub peak_scratch_regular_files: u64,
    pub scratch_files_remaining: u64,
    pub calculated_open_files_bound: u64,
    pub source_planes: u64,
    pub source_files: u64,
    pub source_inventory_name_bytes: u64,
    pub deterministic_filename_order: bool,
    pub source_unchanged: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Generation {
    device: u64,
    inode: u64,
    bytes: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PlaneSource {
    path: PathBuf,
    name: Vec<u8>,
    generation: Generation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceInventory {
    root: PathBuf,
    channel: PathBuf,
    root_generation: Generation,
    channel_generation: Generation,
    planes: Vec<PlaneSource>,
    name_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SourceGeometry {
    shape_zyx: [u64; 3],
    plane_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RawEndian {
    Little,
    Big,
}

#[derive(Clone, Copy, Debug)]
struct RawTiffEntry {
    tag: u16,
    field_type: u16,
    count: u64,
    value_bytes: u64,
    inline_capacity: u64,
    value_field: [u8; 8],
    external_offset: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct RawByteRange {
    start: u64,
    end: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RawTiffFacts {
    width: u64,
    height: u64,
}

struct SourcePlane {
    pixels: Vec<u8>,
    invalid_xy: Vec<u8>,
}

struct RawPlane {
    records: Vec<u8>,
    invalid_xy: Vec<u8>,
}

struct ScratchLevel {
    path: PathBuf,
    file: File,
    generation: Generation,
    shape_zyx: [u64; 3],
    cleaned: bool,
}

impl ScratchLevel {
    fn create(path: PathBuf, shape_zyx: [u64; 3]) -> anyhow::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(nofollow_flag())
            .open(&path)
            .context("T5 oracle could not create its private scratch level")?;
        let generation = generation_from_metadata(
            &file
                .metadata()
                .context("T5 oracle could not inspect its opened scratch level")?,
        );
        Ok(Self {
            path,
            file,
            generation,
            shape_zyx,
            cleaned: false,
        })
    }

    fn expected_bytes(&self) -> anyhow::Result<u64> {
        checked_product(self.shape_zyx)?
            .checked_mul(2)
            .context("T5 oracle scratch level byte count overflowed")
    }

    fn verify_complete(&self) -> anyhow::Result<()> {
        let actual = self
            .file
            .metadata()
            .context("T5 oracle could not inspect its completed scratch level")?
            .len();
        if actual != self.expected_bytes()? {
            bail!("T5 oracle scratch level is incomplete");
        }
        Ok(())
    }

    fn read_plane(&self, z: u64) -> anyhow::Result<Vec<u8>> {
        if z >= self.shape_zyx[0] {
            bail!("T5 oracle scratch plane coordinate is out of bounds");
        }
        let bytes = plane_record_bytes(self.shape_zyx)?;
        let mut records = vec![0_u8; usize::try_from(bytes)?];
        self.file
            .read_exact_at(
                &mut records,
                z.checked_mul(bytes).context("scratch offset overflow")?,
            )
            .context("T5 oracle could not read a scratch plane")?;
        validate_records(&records)?;
        Ok(records)
    }

    fn write_plane(&self, z: u64, records: &[u8]) -> anyhow::Result<()> {
        if z >= self.shape_zyx[0]
            || u64::try_from(records.len())? != plane_record_bytes(self.shape_zyx)?
        {
            bail!("T5 oracle scratch plane has the wrong shape");
        }
        validate_records(records)?;
        self.file
            .write_all_at(
                records,
                z.checked_mul(u64::try_from(records.len())?)
                    .context("scratch offset overflow")?,
            )
            .context("T5 oracle could not write a scratch plane")
    }

    fn read_row(&self, z: u64, y: u64, x: u64, width: u64, out: &mut [u8]) -> anyhow::Result<()> {
        let [shape_z, shape_y, shape_x] = self.shape_zyx;
        if z >= shape_z
            || y >= shape_y
            || x.checked_add(width).is_none_or(|end| end > shape_x)
            || u64::try_from(out.len())? != width.checked_mul(2).context("row overflow")?
        {
            bail!("T5 oracle scratch row is out of bounds");
        }
        let voxel = z
            .checked_mul(shape_y)
            .and_then(|value| value.checked_add(y))
            .and_then(|value| value.checked_mul(shape_x))
            .and_then(|value| value.checked_add(x))
            .context("T5 oracle scratch row offset overflowed")?;
        self.file
            .read_exact_at(
                out,
                voxel.checked_mul(2).context("row byte offset overflow")?,
            )
            .context("T5 oracle could not read a scratch row")?;
        validate_records(out)
    }

    fn remove(mut self) -> anyhow::Result<()> {
        self.remove_inner()?;
        self.cleaned = true;
        Ok(())
    }

    fn remove_inner(&mut self) -> anyhow::Result<()> {
        let metadata = fs::symlink_metadata(&self.path)
            .context("T5 oracle could not inspect its scratch level before cleanup")?;
        let actual = generation_from_metadata(&metadata);
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || actual.device != self.generation.device
            || actual.inode != self.generation.inode
        {
            bail!("T5 oracle scratch level identity changed before cleanup");
        }
        fs::remove_file(&self.path).context("T5 oracle could not remove its scratch level")
    }
}

impl Drop for ScratchLevel {
    fn drop(&mut self) {
        if !self.cleaned && self.remove_inner().is_ok() {
            self.cleaned = true;
        }
    }
}

pub(crate) fn derive(request: OracleRequest<'_>) -> anyhow::Result<OracleFacts> {
    validate_request(request)?;
    check_cancelled(request.cancelled)?;
    let source = checked_directory(request.source, "T5 oracle source")?;
    let scratch = checked_directory(request.scratch, "T5 oracle scratch")?;
    if source.starts_with(&scratch) || scratch.starts_with(&source) {
        bail!("T5 oracle source and scratch directories must be disjoint");
    }

    let inventory = enumerate_source(&source, request.cancelled)?;
    let geometry = inspect_source(&inventory, request.cancelled)?;
    let shapes = t5_shapes(geometry.shape_zyx);
    debug_assert_eq!(shapes.len(), T5_SCALE_COUNT);
    let calculated_peak_working_bytes = calculated_peak_bytes(&inventory, &shapes)?;
    if calculated_peak_working_bytes > request.working_memory_bytes {
        bail!(
            "T5 oracle calculated working set exceeds the selected bound (required {calculated_peak_working_bytes}, bound {})",
            request.working_memory_bytes
        );
    }
    let filesystem =
        rustix::fs::statvfs(&scratch).context("T5 oracle could not inspect scratch free space")?;
    if filesystem.f_frsize == 0 {
        bail!("T5 oracle scratch filesystem reports a zero allocation unit");
    }
    let required_scratch_bytes = required_scratch_bytes(&shapes, filesystem.f_frsize)?;
    let scratch_free_bytes = filesystem
        .f_bavail
        .checked_mul(filesystem.f_frsize)
        .context("T5 oracle scratch free-space count overflowed")?;
    if scratch_free_bytes < required_scratch_bytes {
        bail!(
            "T5 oracle scratch filesystem lacks space (required {required_scratch_bytes}, available {scratch_free_bytes})"
        );
    }

    let session = session_token();
    let paths = [
        scratch.join(format!("{SCRATCH_PREFIX}{session}-0.records")),
        scratch.join(format!("{SCRATCH_PREFIX}{session}-1.records")),
    ];
    check_cancelled(request.cancelled)?;
    let mut level = ScratchLevel::create(paths[0].clone(), shapes[0])?;
    build_base(
        &inventory,
        geometry,
        request.sentinel,
        &level,
        request.cancelled,
    )?;
    level.verify_complete()?;

    let (scientific_content_id, layer_root) = scientific_facts(
        &level,
        request.spacing_zyx_um,
        request.time_step_seconds,
        request.cancelled,
    )?;
    let canonical_base_pixel_bytes = checked_product(shapes[0])?;
    let is_2d = shapes[0][0] == 1;
    let mut scales = Vec::with_capacity(T5_SCALE_COUNT);
    let mut transforms = Vec::with_capacity(T5_SCALE_COUNT);
    let mut factors = [1_u64; 3];

    for (scale, expected_shape) in shapes.iter().copied().enumerate() {
        if level.shape_zyx != expected_shape {
            bail!("T5 oracle recursive level has an unexpected shape");
        }
        scales.push(scale_fact(
            &level,
            u32::try_from(scale)?,
            is_2d,
            request.scale_digest_scheme,
            request.cancelled,
        )?);
        transforms.push(transform_fact(
            u32::try_from(scale)?,
            factors,
            request.spacing_zyx_um,
        )?);
        if scale + 1 < T5_SCALE_COUNT {
            check_cancelled(request.cancelled)?;
            let child_slot = (scale + 1) % 2;
            let child = ScratchLevel::create(paths[child_slot].clone(), shapes[scale + 1])?;
            reduce_level(&level, &child, request.cancelled)?;
            child.verify_complete()?;
            for (axis, factor) in factors.iter_mut().enumerate() {
                if level.shape_zyx[axis] > 1 {
                    *factor = factor
                        .checked_mul(2)
                        .context("T5 oracle transform factor overflowed")?;
                }
            }
            level.remove()?;
            level = child;
        }
    }
    level.remove()?;

    let current_inventory = enumerate_source(&source, request.cancelled)?;
    if current_inventory != inventory {
        bail!("T5 oracle source changed while expected facts were derived");
    }
    let mut scratch_files_remaining = 0_usize;
    for path in &paths {
        match fs::symlink_metadata(path) {
            Ok(_) => scratch_files_remaining += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).context("T5 oracle could not verify scratch cleanup");
            }
        }
    }
    if scratch_files_remaining != 0 {
        bail!("T5 oracle left a scratch-level remnant");
    }
    check_cancelled(request.cancelled)?;

    Ok(OracleFacts {
        scientific_content_id,
        layer_roots: vec![OracleLayerRoot {
            logical_layer: 0,
            digest_sha256: layer_root,
        }],
        scales,
        transforms,
        canonical_base_pixel_bytes,
        resources: OracleResources {
            calculated_peak_working_bytes,
            required_scratch_bytes,
            scratch_free_bytes,
            peak_scratch_regular_files: 2,
            scratch_files_remaining: u64::try_from(scratch_files_remaining)?,
            calculated_open_files_bound: OPEN_FILE_BOUND,
            source_planes: u64::try_from(inventory.planes.len())?,
            source_files: u64::try_from(inventory.planes.len())?,
            source_inventory_name_bytes: inventory.name_bytes,
            deterministic_filename_order: true,
            source_unchanged: true,
        },
    })
}

fn check_cancelled(cancelled: &AtomicBool) -> anyhow::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        bail!("T5 source-derived sentinel oracle was cancelled");
    }
    Ok(())
}

fn validate_request(request: OracleRequest<'_>) -> anyhow::Result<()> {
    if request.working_memory_bytes == 0 {
        bail!("T5 oracle working-memory bound must be positive");
    }
    if request.scale_digest_scheme.is_empty() || request.scale_digest_scheme.as_bytes().contains(&0)
    {
        bail!("T5 oracle scale-digest scheme is empty or contains NUL");
    }
    if request
        .spacing_zyx_um
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        bail!("T5 oracle spacing must be positive and finite");
    }
    if request
        .time_step_seconds
        .is_some_and(|value| !value.is_finite() || value <= 0.0)
    {
        bail!("T5 oracle time step must be positive and finite when present");
    }
    Ok(())
}

fn checked_directory(path: &Path, label: &str) -> anyhow::Result<PathBuf> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("{label} is unavailable"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("{label} must be a non-symlink directory");
    }
    fs::canonicalize(path).with_context(|| format!("{label} could not be resolved"))
}

fn enumerate_source(root: &Path, cancelled: &AtomicBool) -> anyhow::Result<SourceInventory> {
    check_cancelled(cancelled)?;
    let root_metadata = fs::symlink_metadata(root).context("T5 oracle source is unavailable")?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        bail!("T5 oracle source must remain a non-symlink directory");
    }
    let mut root_entries = fs::read_dir(root).context("T5 oracle could not enumerate source")?;
    let channel_entry = root_entries
        .next()
        .transpose()
        .context("T5 oracle could not read the channel-folder entry")?
        .context("T5 oracle source contains no channel folder")?;
    if root_entries.next().transpose()?.is_some() {
        bail!("T5 oracle source must contain exactly one channel folder");
    }
    let channel = channel_entry.path();
    let channel_metadata =
        fs::symlink_metadata(&channel).context("T5 oracle could not inspect the channel folder")?;
    if channel_metadata.file_type().is_symlink() || !channel_metadata.is_dir() {
        bail!("T5 oracle channel entry must be a non-symlink directory");
    }
    let channel_name = channel_entry.file_name();
    if channel_name.to_str().is_none() {
        bail!("T5 oracle channel-folder name must be valid UTF-8");
    }
    let channel_name_bytes = channel_name.as_bytes();
    if channel_name_bytes.is_empty() || channel_name_bytes.len() > MAX_SINGLE_NAME_BYTES {
        bail!("T5 oracle channel-folder name is outside the bounded inventory");
    }
    let mut name_bytes = u64::try_from(channel_name_bytes.len())?;
    let mut planes = Vec::new();
    for entry in fs::read_dir(&channel).context("T5 oracle could not enumerate TIFF planes")? {
        check_cancelled(cancelled)?;
        let entry = entry.context("T5 oracle could not read a TIFF-plane entry")?;
        if planes.len() >= MAX_SOURCE_FILES {
            bail!("T5 oracle source exceeds the bounded plane-file count");
        }
        let name = entry.file_name();
        if name.to_str().is_none() {
            bail!("T5 oracle TIFF-plane filenames must be valid UTF-8");
        }
        let name = name.as_bytes();
        if name.is_empty() || name.len() > MAX_SINGLE_NAME_BYTES || !is_tiff_name(name) {
            bail!("T5 oracle channel folder contains an unsupported entry name");
        }
        name_bytes = name_bytes
            .checked_add(u64::try_from(name.len())?)
            .context("T5 oracle source-name count overflowed")?;
        if name_bytes > MAX_SOURCE_NAME_BYTES {
            bail!("T5 oracle source names exceed the bounded inventory");
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .context("T5 oracle could not inspect a TIFF-plane entry")?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("T5 oracle channel folder must contain only non-symlink regular TIFF planes");
        }
        planes.push(PlaneSource {
            path,
            name: name.to_vec(),
            generation: generation_from_metadata(&metadata),
        });
    }
    if planes.is_empty() {
        bail!("T5 oracle channel folder contains no TIFF planes");
    }
    planes.sort_by(|left, right| left.name.cmp(&right.name));
    check_cancelled(cancelled)?;
    if planes.windows(2).any(|pair| pair[0].name >= pair[1].name) {
        bail!("T5 oracle TIFF-plane filenames are not unique");
    }
    Ok(SourceInventory {
        root: root.to_path_buf(),
        channel,
        root_generation: generation_from_metadata(&root_metadata),
        channel_generation: generation_from_metadata(&channel_metadata),
        planes,
        name_bytes,
    })
}

fn inspect_source(
    inventory: &SourceInventory,
    cancelled: &AtomicBool,
) -> anyhow::Result<SourceGeometry> {
    let mut dimensions = None;
    for plane in &inventory.planes {
        check_cancelled(cancelled)?;
        require_generation(plane)?;
        let file = open_source(&plane.path)?;
        let raw = raw_tiff_preflight(&file, cancelled)?;
        check_cancelled(cancelled)?;
        let mut decoder = Decoder::new(file)
            .context("T5 oracle could not open a TIFF decoder")?
            .with_limits(decoder_limits(1)?);
        let observed = inspect_decoder(&mut decoder)?;
        check_cancelled(cancelled)?;
        if observed != [raw.width, raw.height] {
            bail!("T5 oracle decoder geometry disagrees with raw TIFF preflight");
        }
        require_open_generation(plane, decoder.inner())?;
        require_generation(plane)?;
        if let Some(expected) = dimensions {
            if observed != expected {
                bail!("T5 oracle TIFF planes do not share one geometry");
            }
        } else {
            dimensions = Some(observed);
        }
    }
    let [width, height] = dimensions.context("T5 oracle source has no dimensions")?;
    let plane_bytes = width
        .checked_mul(height)
        .context("T5 oracle TIFF plane byte count overflowed")?;
    Ok(SourceGeometry {
        shape_zyx: [u64::try_from(inventory.planes.len())?, height, width],
        plane_bytes,
    })
}

fn inspect_decoder(decoder: &mut Decoder<File>) -> anyhow::Result<[u64; 2]> {
    let (width, height) = decoder
        .dimensions()
        .context("T5 oracle could not read TIFF dimensions")?;
    if width == 0 || height == 0 {
        bail!("T5 oracle rejects zero-sized TIFF planes");
    }
    if decoder.colortype()? != ColorType::Gray(8)
        || decoder.image_chunk_buffer_layout(0)?.sample_format != SampleFormat::Uint
    {
        bail!("T5 oracle accepts only grayscale uint8 TIFF planes");
    }
    let compression = decoder
        .find_tag_unsigned::<u16>(Tag::Compression)?
        .unwrap_or(1);
    if compression != 1 {
        bail!("T5 oracle accepts only uncompressed TIFF planes");
    }
    if decoder.more_images() {
        bail!("T5 oracle accepts exactly one image page per TIFF file");
    }
    Ok([u64::from(width), u64::from(height)])
}

/// Proves a strict, bounded first-IFD layout before `tiff::Decoder::new` is
/// allowed to parse caller-controlled counts. The TIFF decoder remains the
/// pixel decoder; this parser owns only admission and allocation bounds.
fn raw_tiff_preflight(file: &File, cancelled: &AtomicBool) -> anyhow::Result<RawTiffFacts> {
    check_cancelled(cancelled)?;
    let file_bytes = file
        .metadata()
        .context("T5 oracle could not inspect raw TIFF length")?
        .len();
    let mut header = [0_u8; 16];
    raw_read(file, file_bytes, 0, &mut header[..8])?;
    let endian = match &header[..2] {
        b"II" => RawEndian::Little,
        b"MM" => RawEndian::Big,
        _ => bail!("T5 oracle raw TIFF has an invalid byte-order marker"),
    };
    let magic = raw_u16(endian, &header[2..4]);
    let (big_tiff, header_bytes, count_bytes, entry_bytes, next_bytes, first_ifd) = match magic {
        42 => (
            false,
            8_u64,
            2_u64,
            12_u64,
            4_u64,
            u64::from(raw_u32(endian, &header[4..8])),
        ),
        43 => {
            raw_read(file, file_bytes, 8, &mut header[8..16])?;
            if raw_u16(endian, &header[4..6]) != 8 || raw_u16(endian, &header[6..8]) != 0 {
                bail!("T5 oracle BigTIFF header has an unsupported offset layout");
            }
            (
                true,
                16_u64,
                8_u64,
                20_u64,
                8_u64,
                raw_u64(endian, &header[8..16]),
            )
        }
        _ => bail!("T5 oracle raw TIFF has an unsupported magic number"),
    };
    if first_ifd < header_bytes {
        bail!("T5 oracle raw TIFF first-IFD offset is invalid");
    }

    let mut count_buffer = [0_u8; 8];
    raw_read(
        file,
        file_bytes,
        first_ifd,
        &mut count_buffer[..usize::try_from(count_bytes)?],
    )?;
    let entry_count = if big_tiff {
        raw_u64(endian, &count_buffer)
    } else {
        u64::from(raw_u16(endian, &count_buffer[..2]))
    };
    if entry_count == 0 || entry_count > u64::try_from(MAX_TIFF_IFD_ENTRIES)? {
        bail!("T5 oracle raw TIFF IFD entry count exceeds its strict bound");
    }
    let entries_start = first_ifd
        .checked_add(count_bytes)
        .context("T5 oracle raw TIFF IFD offset overflowed")?;
    let entries_extent = entry_count
        .checked_mul(entry_bytes)
        .context("T5 oracle raw TIFF IFD extent overflowed")?;
    let next_offset = entries_start
        .checked_add(entries_extent)
        .context("T5 oracle raw TIFF next-IFD offset overflowed")?;
    let ifd_end = next_offset
        .checked_add(next_bytes)
        .context("T5 oracle raw TIFF IFD end overflowed")?;
    if ifd_end > file_bytes {
        bail!("T5 oracle raw TIFF IFD is truncated");
    }

    let mut entries = Vec::with_capacity(usize::try_from(entry_count)?);
    let mut external_ranges = Vec::with_capacity(usize::try_from(entry_count)?);
    let mut external_value_bytes = 0_u64;
    let mut raw_entry = [0_u8; 20];
    for index in 0..entry_count {
        check_cancelled(cancelled)?;
        let offset = entries_start
            .checked_add(
                index
                    .checked_mul(entry_bytes)
                    .context("IFD entry overflow")?,
            )
            .context("IFD entry offset overflow")?;
        raw_read(
            file,
            file_bytes,
            offset,
            &mut raw_entry[..usize::try_from(entry_bytes)?],
        )?;
        let tag = raw_u16(endian, &raw_entry[..2]);
        let field_type = raw_u16(endian, &raw_entry[2..4]);
        if !big_tiff && matches!(field_type, 16..=18) {
            bail!("T5 oracle classic TIFF uses a BigTIFF-only field type");
        }
        let count = if big_tiff {
            raw_u64(endian, &raw_entry[4..12])
        } else {
            u64::from(raw_u32(endian, &raw_entry[4..8]))
        };
        if count == 0 {
            bail!("T5 oracle raw TIFF tag has a zero value count");
        }
        let type_bytes = raw_type_bytes(field_type)
            .context("T5 oracle raw TIFF tag has an unsupported field type")?;
        let value_bytes = count
            .checked_mul(type_bytes)
            .context("T5 oracle raw TIFF tag byte count overflowed")?;
        if value_bytes > MAX_TIFF_EXTERNAL_VALUE_BYTES {
            bail!("T5 oracle raw TIFF tag exceeds the external-value bound");
        }
        let inline_capacity = if big_tiff { 8 } else { 4 };
        let value_start = if big_tiff { 12 } else { 8 };
        let mut value_field = [0_u8; 8];
        value_field[..usize::try_from(inline_capacity)?].copy_from_slice(
            &raw_entry[value_start..value_start + usize::try_from(inline_capacity)?],
        );
        let external_offset = if value_bytes > inline_capacity {
            let external_offset = if big_tiff {
                raw_u64(endian, &value_field)
            } else {
                u64::from(raw_u32(endian, &value_field[..4]))
            };
            if external_offset < header_bytes {
                bail!("T5 oracle raw TIFF external-value offset is invalid");
            }
            let range = checked_raw_range(external_offset, value_bytes, file_bytes)?;
            external_value_bytes = external_value_bytes
                .checked_add(value_bytes)
                .context("T5 oracle raw TIFF retained-value total overflowed")?;
            if external_value_bytes > MAX_TIFF_EXTERNAL_VALUE_BYTES {
                bail!("T5 oracle raw TIFF retained tag values exceed their aggregate bound");
            }
            external_ranges.push(range);
            Some(external_offset)
        } else {
            None
        };
        entries.push(RawTiffEntry {
            tag,
            field_type,
            count,
            value_bytes,
            inline_capacity,
            value_field,
            external_offset,
        });
    }
    entries.sort_by_key(|entry| entry.tag);
    if entries.windows(2).any(|pair| pair[0].tag == pair[1].tag) {
        bail!("T5 oracle raw TIFF contains a duplicate tag");
    }
    validate_raw_tag_shapes(&entries)?;

    let mut next_buffer = [0_u8; 8];
    raw_read(
        file,
        file_bytes,
        next_offset,
        &mut next_buffer[..usize::try_from(next_bytes)?],
    )?;
    let next_ifd = if big_tiff {
        raw_u64(endian, &next_buffer)
    } else {
        u64::from(raw_u32(endian, &next_buffer[..4]))
    };
    if next_ifd != 0 {
        bail!("T5 oracle accepts exactly one TIFF IFD/page");
    }

    let width = raw_required_scalar(file, file_bytes, endian, &entries, 256)?;
    let height = raw_required_scalar(file, file_bytes, endian, &entries, 257)?;
    if width == 0 || height == 0 || width > u64::from(u32::MAX) || height > u64::from(u32::MAX) {
        bail!("T5 oracle raw TIFF dimensions are invalid");
    }
    if raw_required_scalar(file, file_bytes, endian, &entries, 258)? != 8
        || raw_scalar(file, file_bytes, endian, &entries, 259, 1)? != 1
        || !matches!(
            raw_required_scalar(file, file_bytes, endian, &entries, 262)?,
            0 | 1
        )
        || raw_scalar(file, file_bytes, endian, &entries, 266, 1)? != 1
        || raw_scalar(file, file_bytes, endian, &entries, 277, 1)? != 1
        || raw_scalar(file, file_bytes, endian, &entries, 284, 1)? != 1
        || raw_scalar(file, file_bytes, endian, &entries, 317, 1)? != 1
        || raw_scalar(file, file_bytes, endian, &entries, 339, 1)? != 1
    {
        bail!("T5 oracle raw TIFF is not uncompressed grayscale uint8");
    }

    let strips = (
        raw_entry_for_tag(&entries, 273),
        raw_entry_for_tag(&entries, 279),
    );
    let tiles = (
        raw_entry_for_tag(&entries, 324),
        raw_entry_for_tag(&entries, 325),
    );
    let (offsets, byte_counts, expected_chunks, expected_chunk_bytes) = match (strips, tiles) {
        ((Some(offsets), Some(byte_counts)), (None, None)) => {
            let rows = raw_scalar(file, file_bytes, endian, &entries, 278, height)?;
            if rows == 0 {
                bail!("T5 oracle raw TIFF rows-per-strip is zero");
            }
            let chunks = height.div_ceil(rows);
            if chunks == 0 || chunks > MAX_TIFF_CHUNKS {
                bail!("T5 oracle raw TIFF strip count exceeds its strict bound");
            }
            let expected = (0..chunks)
                .map(|chunk| {
                    let start = chunk.checked_mul(rows)?;
                    cmp::min(rows, height.checked_sub(start)?).checked_mul(width)
                })
                .collect::<Option<Vec<_>>>()
                .context("T5 oracle raw TIFF strip geometry overflowed")?;
            (offsets, byte_counts, chunks, expected)
        }
        ((None, None), (Some(offsets), Some(byte_counts))) => {
            let tile_width = raw_required_scalar(file, file_bytes, endian, &entries, 322)?;
            let tile_height = raw_required_scalar(file, file_bytes, endian, &entries, 323)?;
            if tile_width == 0 || tile_height == 0 {
                bail!("T5 oracle raw TIFF tile geometry is zero");
            }
            let chunks = width
                .div_ceil(tile_width)
                .checked_mul(height.div_ceil(tile_height))
                .context("T5 oracle raw TIFF tile count overflowed")?;
            if chunks == 0 || chunks > MAX_TIFF_CHUNKS {
                bail!("T5 oracle raw TIFF tile count exceeds its strict bound");
            }
            let chunk_bytes = tile_width
                .checked_mul(tile_height)
                .context("T5 oracle raw TIFF tile byte count overflowed")?;
            (
                offsets,
                byte_counts,
                chunks,
                vec![chunk_bytes; usize::try_from(chunks)?],
            )
        }
        _ => bail!("T5 oracle raw TIFF has an ambiguous strip/tile layout"),
    };
    if offsets.count != expected_chunks || byte_counts.count != expected_chunks {
        bail!("T5 oracle raw TIFF chunk tables have an invalid bounded count");
    }

    let mut pixel_ranges = Vec::with_capacity(usize::try_from(expected_chunks)?);
    let mut stored_pixel_bytes = 0_u64;
    for chunk in 0..expected_chunks {
        check_cancelled(cancelled)?;
        let offset = raw_unsigned_at(file, file_bytes, endian, offsets, chunk)?;
        let byte_count = raw_unsigned_at(file, file_bytes, endian, byte_counts, chunk)?;
        if byte_count != expected_chunk_bytes[usize::try_from(chunk)?] {
            bail!("T5 oracle raw TIFF uncompressed chunk byte count is inconsistent");
        }
        pixel_ranges.push(checked_raw_range(offset, byte_count, file_bytes)?);
        stored_pixel_bytes = stored_pixel_bytes
            .checked_add(byte_count)
            .context("T5 oracle raw TIFF stored-pixel byte count overflowed")?;
    }
    if file_bytes
        > stored_pixel_bytes
            .checked_add(MAX_TIFF_OVERHEAD_BYTES)
            .context("T5 oracle raw TIFF file-size bound overflowed")?
    {
        bail!("T5 oracle raw TIFF exceeds its bounded non-pixel overhead");
    }

    let mut occupied = Vec::with_capacity(
        external_ranges
            .len()
            .checked_add(pixel_ranges.len())
            .and_then(|value| value.checked_add(2))
            .context("T5 oracle raw TIFF range count overflowed")?,
    );
    occupied.push(RawByteRange {
        start: 0,
        end: header_bytes,
    });
    occupied.push(RawByteRange {
        start: first_ifd,
        end: ifd_end,
    });
    occupied.extend(external_ranges);
    occupied.extend(pixel_ranges);
    occupied.sort_unstable();
    if occupied.windows(2).any(|pair| pair[0].end > pair[1].start) {
        bail!("T5 oracle raw TIFF contains overlapping structural/value extents");
    }

    check_cancelled(cancelled)?;
    Ok(RawTiffFacts { width, height })
}

fn raw_entry_for_tag(entries: &[RawTiffEntry], tag: u16) -> Option<&RawTiffEntry> {
    entries
        .binary_search_by_key(&tag, |entry| entry.tag)
        .ok()
        .map(|index| &entries[index])
}

fn validate_raw_tag_shapes(entries: &[RawTiffEntry]) -> anyhow::Result<()> {
    for entry in entries {
        match entry.tag {
            // Scalar fields admitted for a strict single-sample grayscale
            // page. Decoder-side representation is constant-sized.
            254 | 255 | 256 | 257 | 258 | 259 | 262 | 266 | 274 | 277 | 278 | 280 | 281 | 282
            | 283 | 284 | 286 | 287 | 296 | 317 | 322 | 323 | 339 => {
                if entry.count != 1 {
                    bail!("T5 oracle raw TIFF scalar metadata has a vector count");
                }
            }
            // Bounded pixel-location tables. Exact cardinality is checked
            // against strip/tile geometry below.
            273 | 279 | 324 | 325 => {
                if entry.count > MAX_TIFF_CHUNKS {
                    bail!("T5 oracle raw TIFF chunk table exceeds its vector bound");
                }
            }
            // Explicit text vectors retained by Decoder::new. ASCII storage
            // expands by at most its already charged raw byte count.
            269 | 270 | 271 | 272 | 285 | 305 | 306 | 315 | 316 | 33432 => {
                if entry.field_type != 2 {
                    bail!("T5 oracle raw TIFF text metadata is not ASCII");
                }
            }
            // A two-short scalar pair, kept explicit rather than allowing an
            // arbitrary uncharged vector tag.
            297 if entry.field_type == 3 && entry.count == 2 => {}
            // These tags can make nominal Gray8 pages materialize additional
            // sample/chroma vectors and are absent from the pinned Gray1
            // authority.
            338 | 530 => {
                bail!("T5 oracle raw TIFF contains an extra-sample/chroma vector tag");
            }
            _ => {
                bail!("T5 oracle raw TIFF contains an unbounded or unsupported metadata tag");
            }
        }
    }
    Ok(())
}

fn raw_required_scalar(
    file: &File,
    file_bytes: u64,
    endian: RawEndian,
    entries: &[RawTiffEntry],
    tag: u16,
) -> anyhow::Result<u64> {
    let entry =
        raw_entry_for_tag(entries, tag).context("T5 oracle raw TIFF lacks a required tag")?;
    if entry.count != 1 {
        bail!("T5 oracle raw TIFF scalar tag has a non-scalar count");
    }
    raw_unsigned_at(file, file_bytes, endian, entry, 0)
}

fn raw_scalar(
    file: &File,
    file_bytes: u64,
    endian: RawEndian,
    entries: &[RawTiffEntry],
    tag: u16,
    default: u64,
) -> anyhow::Result<u64> {
    match raw_entry_for_tag(entries, tag) {
        Some(entry) if entry.count == 1 => raw_unsigned_at(file, file_bytes, endian, entry, 0),
        Some(_) => bail!("T5 oracle raw TIFF scalar tag has a non-scalar count"),
        None => Ok(default),
    }
}

fn raw_unsigned_at(
    file: &File,
    file_bytes: u64,
    endian: RawEndian,
    entry: &RawTiffEntry,
    index: u64,
) -> anyhow::Result<u64> {
    if index >= entry.count {
        bail!("T5 oracle raw TIFF tag index is out of bounds");
    }
    let width = raw_unsigned_type_bytes(entry.field_type)
        .context("T5 oracle raw TIFF numeric tag has a non-unsigned type")?;
    let byte_offset = index
        .checked_mul(width)
        .context("T5 oracle raw TIFF tag offset overflowed")?;
    let mut bytes = [0_u8; 8];
    if entry.value_bytes <= entry.inline_capacity {
        let start = usize::try_from(byte_offset)?;
        let end = start
            .checked_add(usize::try_from(width)?)
            .context("inline value overflow")?;
        bytes[..usize::try_from(width)?].copy_from_slice(&entry.value_field[start..end]);
    } else {
        let offset = entry
            .external_offset
            .context("T5 oracle raw TIFF external value lacks an offset")?
            .checked_add(byte_offset)
            .context("T5 oracle raw TIFF external value offset overflowed")?;
        raw_read(
            file,
            file_bytes,
            offset,
            &mut bytes[..usize::try_from(width)?],
        )?;
    }
    Ok(match width {
        1 => u64::from(bytes[0]),
        2 => u64::from(raw_u16(endian, &bytes[..2])),
        4 => u64::from(raw_u32(endian, &bytes[..4])),
        8 => raw_u64(endian, &bytes),
        _ => unreachable!("unsigned TIFF widths are closed"),
    })
}

fn raw_unsigned_type_bytes(field_type: u16) -> Option<u64> {
    match field_type {
        1 => Some(1),
        3 => Some(2),
        4 | 13 => Some(4),
        16 | 18 => Some(8),
        _ => None,
    }
}

fn raw_type_bytes(field_type: u16) -> Option<u64> {
    match field_type {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 | 13 => Some(4),
        5 | 10 | 12 | 16 | 17 | 18 => Some(8),
        _ => None,
    }
}

fn checked_raw_range(start: u64, bytes: u64, file_bytes: u64) -> anyhow::Result<RawByteRange> {
    if bytes == 0 {
        bail!("T5 oracle raw TIFF contains an empty extent");
    }
    let end = start
        .checked_add(bytes)
        .context("T5 oracle raw TIFF extent overflowed")?;
    if end > file_bytes {
        bail!("T5 oracle raw TIFF extent leaves the regular file");
    }
    Ok(RawByteRange { start, end })
}

fn raw_read(file: &File, file_bytes: u64, offset: u64, out: &mut [u8]) -> anyhow::Result<()> {
    checked_raw_range(offset, u64::try_from(out.len())?, file_bytes)?;
    file.read_exact_at(out, offset)
        .context("T5 oracle could not read a bounded raw TIFF extent")
}

fn raw_u16(endian: RawEndian, bytes: &[u8]) -> u16 {
    let bytes = [bytes[0], bytes[1]];
    match endian {
        RawEndian::Little => u16::from_le_bytes(bytes),
        RawEndian::Big => u16::from_be_bytes(bytes),
    }
}

fn raw_u32(endian: RawEndian, bytes: &[u8]) -> u32 {
    let bytes = [bytes[0], bytes[1], bytes[2], bytes[3]];
    match endian {
        RawEndian::Little => u32::from_le_bytes(bytes),
        RawEndian::Big => u32::from_be_bytes(bytes),
    }
}

fn raw_u64(endian: RawEndian, bytes: &[u8]) -> u64 {
    let bytes = [
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ];
    match endian {
        RawEndian::Little => u64::from_le_bytes(bytes),
        RawEndian::Big => u64::from_be_bytes(bytes),
    }
}

fn build_base(
    inventory: &SourceInventory,
    geometry: SourceGeometry,
    sentinel: u8,
    output: &ScratchLevel,
    cancelled: &AtomicBool,
) -> anyhow::Result<()> {
    check_cancelled(cancelled)?;
    let z_count = geometry.shape_zyx[0];
    let mut previous = None;
    let mut current = decode_source_plane(&inventory.planes[0], geometry, sentinel, cancelled)?;
    let mut next = if z_count > 1 {
        Some(decode_source_plane(
            &inventory.planes[1],
            geometry,
            sentinel,
            cancelled,
        )?)
    } else {
        None
    };
    for z in 0..z_count {
        check_cancelled(cancelled)?;
        let records = finish_source_plane(
            previous.as_ref(),
            &current,
            next.as_ref(),
            geometry.shape_zyx,
            cancelled,
        )?;
        output.write_plane(z, &records)?;
        drop(records);
        previous = Some(current);
        current = match next.take() {
            Some(plane) => plane,
            None if z + 1 == z_count => break,
            None => bail!("T5 oracle source plane ring ended early"),
        };
        let next_z = z + 2;
        if next_z < z_count {
            next = Some(decode_source_plane(
                &inventory.planes[usize::try_from(next_z)?],
                geometry,
                sentinel,
                cancelled,
            )?);
        }
    }
    Ok(())
}

fn decode_source_plane(
    source: &PlaneSource,
    geometry: SourceGeometry,
    sentinel: u8,
    cancelled: &AtomicBool,
) -> anyhow::Result<SourcePlane> {
    check_cancelled(cancelled)?;
    require_generation(source)?;
    if source.generation.bytes
        > geometry
            .plane_bytes
            .checked_add(MAX_TIFF_OVERHEAD_BYTES)
            .context("T5 oracle TIFF size bound overflowed")?
    {
        bail!("T5 oracle TIFF plane exceeds the bounded metadata overhead");
    }
    let file = open_source(&source.path)?;
    let raw = raw_tiff_preflight(&file, cancelled)?;
    check_cancelled(cancelled)?;
    if [raw.width, raw.height] != [geometry.shape_zyx[2], geometry.shape_zyx[1]] {
        bail!("T5 oracle raw TIFF geometry changed before decode");
    }
    let mut decoder = Decoder::new(file)
        .context("T5 oracle could not open a TIFF decoder")?
        .with_limits(decoder_limits(usize::try_from(geometry.plane_bytes)?)?);
    let observed = inspect_decoder(&mut decoder)?;
    if observed != [geometry.shape_zyx[2], geometry.shape_zyx[1]] {
        bail!("T5 oracle TIFF geometry changed before decode");
    }
    let pixels = match decoder.read_image()? {
        DecodingResult::U8(pixels) => pixels,
        _ => bail!("T5 oracle TIFF decoder returned a non-uint8 plane"),
    };
    check_cancelled(cancelled)?;
    if u64::try_from(pixels.len())? != geometry.plane_bytes {
        bail!("T5 oracle decoded TIFF plane has the wrong byte count");
    }
    require_open_generation(source, decoder.inner())?;
    require_generation(source)?;
    let invalid_xy = invalid_xy_from_predicate(
        &pixels,
        usize::try_from(geometry.shape_zyx[1])?,
        usize::try_from(geometry.shape_zyx[2])?,
        |value| value == sentinel,
        cancelled,
    )?;
    Ok(SourcePlane { pixels, invalid_xy })
}

fn finish_source_plane(
    previous: Option<&SourcePlane>,
    current: &SourcePlane,
    next: Option<&SourcePlane>,
    shape_zyx: [u64; 3],
    cancelled: &AtomicBool,
) -> anyhow::Result<Vec<u8>> {
    let voxels = usize::try_from(
        shape_zyx[1]
            .checked_mul(shape_zyx[2])
            .context("T5 oracle plane voxel count overflowed")?,
    )?;
    if current.pixels.len() != voxels || current.invalid_xy.len() != voxels {
        bail!("T5 oracle source plane ring has inconsistent geometry");
    }
    let mut records = vec![0_u8; voxels.checked_mul(2).context("base record overflow")?];
    for index in 0..voxels {
        if index.is_multiple_of(65_536) {
            check_cancelled(cancelled)?;
        }
        let invalid = current.invalid_xy[index] != 0
            || previous.is_some_and(|plane| plane.invalid_xy[index] != 0)
            || next.is_some_and(|plane| plane.invalid_xy[index] != 0);
        if !invalid {
            records[index * 2] = current.pixels[index];
            records[index * 2 + 1] = 1;
        }
    }
    Ok(records)
}

fn reduce_level(
    parent: &ScratchLevel,
    child: &ScratchLevel,
    cancelled: &AtomicBool,
) -> anyhow::Result<()> {
    check_cancelled(cancelled)?;
    let child_z = child.shape_zyx[0];
    let mut previous = None;
    let mut current = reduce_raw_plane(parent, 0, cancelled)?;
    let mut next = if child_z > 1 {
        Some(reduce_raw_plane(parent, 1, cancelled)?)
    } else {
        None
    };
    for z in 0..child_z {
        check_cancelled(cancelled)?;
        let records = finish_raw_plane(previous.as_ref(), &current, next.as_ref(), cancelled)?;
        child.write_plane(z, &records)?;
        drop(records);
        previous = Some(current);
        current = match next.take() {
            Some(plane) => plane,
            None if z + 1 == child_z => break,
            None => bail!("T5 oracle recursive plane ring ended early"),
        };
        let next_z = z + 2;
        if next_z < child_z {
            next = Some(reduce_raw_plane(parent, next_z, cancelled)?);
        }
    }
    Ok(())
}

fn reduce_raw_plane(
    parent: &ScratchLevel,
    child_z: u64,
    cancelled: &AtomicBool,
) -> anyhow::Result<RawPlane> {
    check_cancelled(cancelled)?;
    let parent_z_start = if parent.shape_zyx[0] > 1 {
        child_z.checked_mul(2).context("parent z overflow")?
    } else {
        0
    };
    let parent_z_end = if parent.shape_zyx[0] > 1 {
        cmp::min(parent_z_start + 2, parent.shape_zyx[0])
    } else {
        1
    };
    let mut parent_planes = Vec::with_capacity(2);
    for z in parent_z_start..parent_z_end {
        parent_planes.push(parent.read_plane(z)?);
    }
    let child_shape = parent.shape_zyx.map(|dimension| dimension.div_ceil(2));
    let child_y = usize::try_from(child_shape[1])?;
    let child_x = usize::try_from(child_shape[2])?;
    let parent_y = usize::try_from(parent.shape_zyx[1])?;
    let parent_x = usize::try_from(parent.shape_zyx[2])?;
    let voxels = child_y
        .checked_mul(child_x)
        .context("child plane overflow")?;
    let mut records = vec![0_u8; voxels.checked_mul(2).context("raw records overflow")?];
    for y in 0..child_y {
        check_cancelled(cancelled)?;
        let y_start = if parent_y > 1 { y * 2 } else { 0 };
        let y_end = if parent_y > 1 {
            cmp::min(y_start + 2, parent_y)
        } else {
            1
        };
        for x in 0..child_x {
            let x_start = if parent_x > 1 { x * 2 } else { 0 };
            let x_end = if parent_x > 1 {
                cmp::min(x_start + 2, parent_x)
            } else {
                1
            };
            let mut sum = 0_u32;
            let mut count = 0_u32;
            for plane in &parent_planes {
                for parent_y_coordinate in y_start..y_end {
                    for parent_x_coordinate in x_start..x_end {
                        let index = parent_y_coordinate * parent_x + parent_x_coordinate;
                        if plane[index * 2 + 1] != 0 {
                            sum += u32::from(plane[index * 2]);
                            count += 1;
                        }
                    }
                }
            }
            if let Some(mean) = (sum + count / 2).checked_div(count) {
                let target = y * child_x + x;
                records[target * 2] = u8::try_from(mean)?;
                records[target * 2 + 1] = 1;
            }
        }
    }
    let invalid_xy =
        invalid_xy_from_predicate(&records, child_y, child_x, |valid| valid == 0, cancelled)?;
    Ok(RawPlane {
        records,
        invalid_xy,
    })
}

fn finish_raw_plane(
    previous: Option<&RawPlane>,
    current: &RawPlane,
    next: Option<&RawPlane>,
    cancelled: &AtomicBool,
) -> anyhow::Result<Vec<u8>> {
    let voxels = current.invalid_xy.len();
    if current.records.len() != voxels.checked_mul(2).context("raw plane overflow")? {
        bail!("T5 oracle raw recursive plane has inconsistent geometry");
    }
    let mut records = vec![0_u8; current.records.len()];
    for index in 0..voxels {
        if index.is_multiple_of(65_536) {
            check_cancelled(cancelled)?;
        }
        let invalid = current.invalid_xy[index] != 0
            || previous.is_some_and(|plane| plane.invalid_xy[index] != 0)
            || next.is_some_and(|plane| plane.invalid_xy[index] != 0);
        if !invalid {
            records[index * 2] = current.records[index * 2];
            records[index * 2 + 1] = 1;
        }
    }
    Ok(records)
}

fn invalid_xy_from_predicate(
    values: &[u8],
    height: usize,
    width: usize,
    predicate: impl Fn(u8) -> bool,
    cancelled: &AtomicBool,
) -> anyhow::Result<Vec<u8>> {
    let samples = height.checked_mul(width).context("plane shape overflow")?;
    let stride = values
        .len()
        .checked_div(samples)
        .filter(|stride| *stride > 0 && stride.checked_mul(samples) == Some(values.len()))
        .context("plane bytes disagree with geometry")?;
    let predicate_offset = stride - 1;
    let mut horizontal = vec![0_u8; samples];
    for y in 0..height {
        check_cancelled(cancelled)?;
        for x in 0..width {
            let start = x.saturating_sub(1);
            let end = cmp::min(x + 2, width);
            horizontal[y * width + x] = u8::from((start..end).any(|neighbor| {
                predicate(values[(y * width + neighbor) * stride + predicate_offset])
            }));
        }
    }
    let mut invalid = vec![0_u8; samples];
    for y in 0..height {
        check_cancelled(cancelled)?;
        let start = y.saturating_sub(1);
        let end = cmp::min(y + 2, height);
        for x in 0..width {
            invalid[y * width + x] =
                u8::from((start..end).any(|neighbor| horizontal[neighbor * width + x] != 0));
        }
    }
    Ok(invalid)
}

fn scientific_facts(
    base: &ScratchLevel,
    spacing_zyx_um: [f64; 3],
    time_step_seconds: Option<f64>,
    cancelled: &AtomicBool,
) -> anyhow::Result<(String, String)> {
    check_cancelled(cancelled)?;
    let shape = Shape4D::new(1, base.shape_zyx[0], base.shape_zyx[1], base.shape_zyx[2])?;
    let temporal = match time_step_seconds {
        Some(step_seconds) => ScientificTemporalCalibration::Regular { step_seconds },
        None => ScientificTemporalCalibration::Unknown,
    };
    let transform = GridToWorld::scale(spacing_zyx_um[2], spacing_zyx_um[1], spacing_zyx_um[0])?;
    let descriptor = ScientificLayerDescriptor::new(
        LogicalLayerKey::new(0),
        IntensityDType::Uint8,
        shape,
        temporal,
        transform,
    )?;
    let mut layer = ScientificLayerHasher::new(descriptor)?;
    for origin_z in (0..shape.z()).step_by(usize::try_from(SCIENTIFIC_TILE_SHAPE_TZYX[1])?) {
        check_cancelled(cancelled)?;
        for origin_y in (0..shape.y()).step_by(usize::try_from(SCIENTIFIC_TILE_SHAPE_TZYX[2])?) {
            check_cancelled(cancelled)?;
            for origin_x in (0..shape.x()).step_by(usize::try_from(SCIENTIFIC_TILE_SHAPE_TZYX[3])?)
            {
                let extent = [
                    cmp::min(SCIENTIFIC_TILE_SHAPE_TZYX[1], shape.z() - origin_z),
                    cmp::min(SCIENTIFIC_TILE_SHAPE_TZYX[2], shape.y() - origin_y),
                    cmp::min(SCIENTIFIC_TILE_SHAPE_TZYX[3], shape.x() - origin_x),
                ];
                let voxels = checked_product(extent)?;
                let mut values = Vec::with_capacity(usize::try_from(voxels)?);
                let mut validity = vec![0_u8; usize::try_from(voxels.div_ceil(8))?];
                let mut row =
                    vec![
                        0_u8;
                        usize::try_from(extent[2].checked_mul(2).context("tile row overflow")?)?
                    ];
                let mut tile_index = 0_usize;
                for z in origin_z..origin_z + extent[0] {
                    check_cancelled(cancelled)?;
                    for y in origin_y..origin_y + extent[1] {
                        check_cancelled(cancelled)?;
                        base.read_row(z, y, origin_x, extent[2], &mut row)?;
                        for record in row.chunks_exact(2) {
                            values.push(record[0]);
                            if record[1] != 0 {
                                validity[tile_index / 8] |= 1 << (tile_index % 8);
                            }
                            tile_index += 1;
                        }
                    }
                }
                layer.push_tile(ScientificTile::new(
                    [0, origin_z, origin_y, origin_x],
                    [1, extent[0], extent[1], extent[2]],
                    &validity,
                    &values,
                ))?;
            }
        }
    }
    let root = layer.finalize()?;
    let root_digest = root.digest().to_string();
    let mut dataset = ScientificDatasetHasher::new(1)?;
    dataset.push_layer(root)?;
    Ok((dataset.finalize()?.to_string(), root_digest))
}

fn scale_fact(
    level: &ScratchLevel,
    scale_ordinal: u32,
    is_2d: bool,
    scheme: &str,
    cancelled: &AtomicBool,
) -> anyhow::Result<OracleScaleFact> {
    check_cancelled(cancelled)?;
    let shape = [
        1_u64,
        1,
        level.shape_zyx[0],
        level.shape_zyx[1],
        level.shape_zyx[2],
    ];
    let brick_shape = if is_2d { [1, 256, 256] } else { [64, 64, 64] };
    let kind_tag = if is_2d { 4 } else { 1 };
    let chunk_counts = [
        ceil_div(shape[2], brick_shape[0])?,
        ceil_div(shape[3], brick_shape[1])?,
        ceil_div(shape[4], brick_shape[2])?,
    ];
    let brick_reads = checked_product(chunk_counts)?;
    let logical_voxels = checked_product(shape)?;
    let mut hasher = Sha256Hasher::new();
    hasher.update(scheme.as_bytes());
    hasher.update([0]);
    hasher.update(0_u32.to_le_bytes());
    hasher.update(scale_ordinal.to_le_bytes());
    for dimension in shape {
        hasher.update(dimension.to_le_bytes());
    }
    hasher.update([kind_tag]);
    hasher.update([1]);
    let mut records = Vec::new();
    let mut canonical = Vec::new();
    for z_chunk in 0..chunk_counts[0] {
        check_cancelled(cancelled)?;
        for y_chunk in 0..chunk_counts[1] {
            check_cancelled(cancelled)?;
            for x_chunk in 0..chunk_counts[2] {
                check_cancelled(cancelled)?;
                for coordinate in [
                    0_u64,
                    u64::from(scale_ordinal),
                    0,
                    0,
                    z_chunk,
                    y_chunk,
                    x_chunk,
                ] {
                    hasher.update(coordinate.to_le_bytes());
                }
                let origin = [
                    z_chunk * brick_shape[0],
                    y_chunk * brick_shape[1],
                    x_chunk * brick_shape[2],
                ];
                let extent = [
                    cmp::min(brick_shape[0], level.shape_zyx[0] - origin[0]),
                    cmp::min(brick_shape[1], level.shape_zyx[1] - origin[1]),
                    cmp::min(brick_shape[2], level.shape_zyx[2] - origin[2]),
                ];
                for dimension in extent {
                    hasher.update(dimension.to_le_bytes());
                }
                let row_bytes =
                    usize::try_from(extent[2].checked_mul(2).context("scale row overflow")?)?;
                records.resize(row_bytes, 0);
                canonical.clear();
                canonical.reserve(row_bytes.saturating_sub(canonical.len()));
                for z in origin[0]..origin[0] + extent[0] {
                    check_cancelled(cancelled)?;
                    for y in origin[1]..origin[1] + extent[1] {
                        check_cancelled(cancelled)?;
                        level.read_row(z, y, origin[2], extent[2], &mut records)?;
                        canonical.clear();
                        for record in records.chunks_exact(2) {
                            canonical.push(record[1]);
                            canonical.push(record[0]);
                        }
                        hasher.update(&canonical);
                    }
                }
            }
        }
    }
    Ok(OracleScaleFact {
        image_ordinal: 0,
        scale_ordinal,
        digest_sha256: hasher.finalize().to_string(),
        brick_reads,
        logical_voxels,
    })
}

fn transform_fact(
    scale_ordinal: u32,
    factors: [u64; 3],
    spacing_zyx_um: [f64; 3],
) -> anyhow::Result<OracleTransformFact> {
    let mut scale_zyx = [0.0; 3];
    let mut translation_zyx = [0.0; 3];
    for axis in 0..3 {
        scale_zyx[axis] = spacing_zyx_um[axis] * factors[axis] as f64;
        translation_zyx[axis] = spacing_zyx_um[axis] * (factors[axis] - 1) as f64 / 2.0;
        if !scale_zyx[axis].is_finite() || !translation_zyx[axis].is_finite() {
            bail!("T5 oracle transform is not finite");
        }
    }
    Ok(OracleTransformFact {
        scale_ordinal,
        scale_zyx,
        translation_zyx,
    })
}

fn t5_shapes(base: [u64; 3]) -> Vec<[u64; 3]> {
    let mut shapes = Vec::with_capacity(T5_SCALE_COUNT);
    shapes.push(base);
    while shapes.len() < T5_SCALE_COUNT {
        shapes.push(
            shapes
                .last()
                .copied()
                .unwrap()
                .map(|dimension| dimension.div_ceil(2)),
        );
    }
    shapes
}

fn required_scratch_bytes(shapes: &[[u64; 3]], allocation_unit: u64) -> anyhow::Result<u64> {
    if allocation_unit == 0 {
        bail!("T5 oracle scratch allocation unit must be positive");
    }
    let mut maximum = 0;
    for (index, shape) in shapes.iter().enumerate() {
        let parent = rounded_scratch_bytes(
            checked_product(*shape)?
                .checked_mul(2)
                .context("T5 oracle scratch bytes overflowed")?,
            allocation_unit,
        )?;
        let child = shapes
            .get(index + 1)
            .map(|shape| {
                checked_product(*shape)
                    .and_then(|voxels| {
                        voxels
                            .checked_mul(2)
                            .context("T5 oracle scratch bytes overflowed")
                    })
                    .and_then(|bytes| rounded_scratch_bytes(bytes, allocation_unit))
            })
            .transpose()?
            .unwrap_or(0);
        maximum = maximum.max(
            parent
                .checked_add(child)
                .context("T5 oracle scratch pair overflowed")?,
        );
    }
    Ok(maximum)
}

fn rounded_scratch_bytes(bytes: u64, allocation_unit: u64) -> anyhow::Result<u64> {
    bytes
        .div_ceil(allocation_unit)
        .checked_mul(allocation_unit)
        .context("T5 oracle rounded scratch byte count overflowed")
}

fn calculated_peak_bytes(inventory: &SourceInventory, shapes: &[[u64; 3]]) -> anyhow::Result<u64> {
    let source_count = u64::try_from(inventory.planes.len())?;
    let inventory_bytes = inventory
        .name_bytes
        .checked_mul(2)
        .and_then(|value| value.checked_add(source_count.checked_mul(INVENTORY_BYTES_PER_FILE)?))
        .and_then(|value| value.checked_add(INVENTORY_FIXED_BYTES))
        .context("T5 oracle inventory charge overflowed")?;
    let base_plane = shapes[0][1]
        .checked_mul(shapes[0][2])
        .context("T5 oracle base-plane charge overflowed")?;
    let mut transient = base_plane
        .checked_mul(8)
        .and_then(|value| value.checked_add(DECODER_FIXED_BYTES))
        .context("T5 oracle base-ring charge overflowed")?;
    for pair in shapes.windows(2) {
        let parent_plane = pair[0][1]
            .checked_mul(pair[0][2])
            .context("parent plane overflow")?;
        let child_plane = pair[1][1]
            .checked_mul(pair[1][2])
            .context("child plane overflow")?;
        transient = transient.max(
            parent_plane
                .checked_mul(4)
                .and_then(|value| value.checked_add(child_plane.checked_mul(10)?))
                .context("T5 oracle recursive-ring charge overflowed")?,
        );
        transient = transient.max(
            child_plane
                .checked_mul(11)
                .context("child output charge overflow")?,
        );
    }
    let identity_voxels = cmp::min(shapes[0][0], SCIENTIFIC_TILE_SHAPE_TZYX[1])
        .checked_mul(cmp::min(shapes[0][1], SCIENTIFIC_TILE_SHAPE_TZYX[2]))
        .and_then(|value| value.checked_mul(cmp::min(shapes[0][2], SCIENTIFIC_TILE_SHAPE_TZYX[3])))
        .context("T5 oracle identity-tile charge overflowed")?;
    transient = transient.max(
        identity_voxels
            .checked_add(identity_voxels.div_ceil(8))
            .and_then(|value| value.checked_add(cmp::min(shapes[0][2], 256).checked_mul(2)?))
            .context("T5 oracle identity charge overflowed")?,
    );
    ORACLE_FIXED_BYTES
        .checked_add(inventory_bytes)
        .and_then(|value| value.checked_add(transient))
        .context("T5 oracle peak working-set charge overflowed")
}

fn decoder_limits(plane_bytes: usize) -> anyhow::Result<Limits> {
    let mut limits = Limits::default();
    limits.decoding_buffer_size = plane_bytes.max(1);
    limits.intermediate_buffer_size = plane_bytes.max(1);
    limits.ifd_value_size = usize::try_from(MAX_TIFF_OVERHEAD_BYTES)?;
    Ok(limits)
}

fn open_source(path: &Path) -> anyhow::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(nofollow_flag())
        .open(path)
        .context("T5 oracle could not open a reviewed source plane")
}

fn nofollow_flag() -> i32 {
    i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits())
        .expect("O_NOFOLLOW is representable as a platform open flag")
}

fn require_generation(source: &PlaneSource) -> anyhow::Result<()> {
    let metadata =
        fs::symlink_metadata(&source.path).context("T5 oracle could not recheck a source plane")?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || generation_from_metadata(&metadata) != source.generation
    {
        bail!("T5 oracle source plane changed during derivation");
    }
    Ok(())
}

fn require_open_generation(source: &PlaneSource, file: &File) -> anyhow::Result<()> {
    let metadata = file
        .metadata()
        .context("T5 oracle could not inspect an opened source plane")?;
    if !metadata.is_file() || generation_from_metadata(&metadata) != source.generation {
        bail!("T5 oracle opened source plane differs from the reviewed generation");
    }
    Ok(())
}

fn generation_from_metadata(metadata: &fs::Metadata) -> Generation {
    Generation {
        device: metadata.dev(),
        inode: metadata.ino(),
        bytes: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    }
}

fn is_tiff_name(name: &[u8]) -> bool {
    let lower = name.iter().map(u8::to_ascii_lowercase).collect::<Vec<_>>();
    lower.ends_with(b".tif") || lower.ends_with(b".tiff")
}

fn validate_records(records: &[u8]) -> anyhow::Result<()> {
    if !records.len().is_multiple_of(2) {
        bail!("T5 oracle scratch records are truncated");
    }
    if records
        .chunks_exact(2)
        .any(|record| record[1] > 1 || (record[1] == 0 && record[0] != 0))
    {
        bail!("T5 oracle scratch records violate canonical value/validity representation");
    }
    Ok(())
}

fn plane_record_bytes(shape: [u64; 3]) -> anyhow::Result<u64> {
    shape[1]
        .checked_mul(shape[2])
        .and_then(|value| value.checked_mul(2))
        .context("T5 oracle plane-record byte count overflowed")
}

fn checked_product<const N: usize>(values: [u64; N]) -> anyhow::Result<u64> {
    values.into_iter().try_fold(1_u64, |product, value| {
        product
            .checked_mul(value)
            .context("T5 oracle dimension product overflowed")
    })
}

fn ceil_div(value: u64, divisor: u64) -> anyhow::Result<u64> {
    if value == 0 || divisor == 0 {
        bail!("T5 oracle ceil-div operands must be positive");
    }
    Ok(value.div_ceil(divisor))
}

fn session_token() -> String {
    let counter = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{timestamp}-{counter}", std::process::id())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;
    use tiff::encoder::{Compression, TiffEncoder, colortype};

    use super::*;

    const TEST_SCHEME: &str = "mirante4d-t5-canonical-scale-voxels-1";
    static NOT_CANCELLED: AtomicBool = AtomicBool::new(false);

    fn source_folder(temporary: &TempDir) -> PathBuf {
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::create_dir(source.join("channel-000")).unwrap();
        source
    }

    fn write_plane(path: &Path, width: u32, height: u32, pixels: &[u8]) {
        let file = File::create(path).unwrap();
        TiffEncoder::new(file)
            .unwrap()
            .write_image::<colortype::Gray8>(width, height, pixels)
            .unwrap();
    }

    fn write_source(temporary: &TempDir, width: u32, height: u32, planes: &[Vec<u8>]) -> PathBuf {
        let source = source_folder(temporary);
        // Deliberately create in reverse order. Enumeration, not directory
        // insertion order, owns the canonical plane sequence.
        for z in (0..planes.len()).rev() {
            write_plane(
                &source
                    .join("channel-000")
                    .join(format!("plane-z{z:05}.tif")),
                width,
                height,
                &planes[z],
            );
        }
        source
    }

    fn request<'a>(source: &'a Path, scratch: &'a Path) -> OracleRequest<'a> {
        OracleRequest {
            source,
            scratch,
            sentinel: 255,
            spacing_zyx_um: [2.0, 3.0, 5.0],
            time_step_seconds: None,
            working_memory_bytes: 64 * 1024 * 1024,
            scale_digest_scheme: TEST_SCHEME,
            cancelled: &NOT_CANCELLED,
        }
    }

    fn scratch_entries(path: &Path) -> Vec<PathBuf> {
        fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect()
    }

    fn all_level_records(level: &ScratchLevel) -> Vec<u8> {
        (0..level.shape_zyx[0])
            .flat_map(|z| level.read_plane(z).unwrap())
            .collect()
    }

    #[test]
    fn guarded_base_and_recursive_mean_match_the_exact_contract() {
        let temporary = tempfile::tempdir().unwrap();
        let width = 5;
        let height = 3;
        let mut planes = (0..3)
            .map(|z| {
                (0..height * width)
                    .map(|index| u8::try_from(10 + z * 20 + index).unwrap())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        planes[1][usize::try_from(width + 2).unwrap()] = 255;
        let source = write_source(&temporary, width, height, &planes);
        let scratch = temporary.path().join("scratch");
        fs::create_dir(&scratch).unwrap();
        let inventory = enumerate_source(&source, &NOT_CANCELLED).unwrap();
        let geometry = inspect_source(&inventory, &NOT_CANCELLED).unwrap();
        let base_path = scratch.join("base.records");
        let base = ScratchLevel::create(base_path, geometry.shape_zyx).unwrap();
        build_base(&inventory, geometry, 255, &base, &NOT_CANCELLED).unwrap();
        base.verify_complete().unwrap();

        for z in 0..3_u64 {
            let records = base.read_plane(z).unwrap();
            for y in 0..3_usize {
                for x in 0..5_usize {
                    let index = y * 5 + x;
                    let expected_valid = x == 0 || x == 4;
                    assert_eq!(records[index * 2 + 1] != 0, expected_valid);
                    assert_eq!(
                        records[index * 2],
                        if expected_valid {
                            planes[usize::try_from(z).unwrap()][index]
                        } else {
                            0
                        }
                    );
                }
            }
        }

        let child = ScratchLevel::create(scratch.join("child.records"), [2, 2, 3]).unwrap();
        reduce_level(&base, &child, &NOT_CANCELLED).unwrap();
        child.verify_complete().unwrap();
        for z in 0..2 {
            assert!(child.read_plane(z).unwrap().iter().all(|byte| *byte == 0));
        }
        base.remove().unwrap();
        child.remove().unwrap();
        assert!(scratch_entries(&scratch).is_empty());
    }

    #[test]
    fn derived_value_equal_to_the_source_sentinel_remains_valid() {
        let temporary = tempfile::tempdir().unwrap();
        let source = write_source(&temporary, 2, 1, &[vec![6, 8]]);
        let scratch = temporary.path().join("scratch");
        fs::create_dir(&scratch).unwrap();
        let inventory = enumerate_source(&source, &NOT_CANCELLED).unwrap();
        let geometry = inspect_source(&inventory, &NOT_CANCELLED).unwrap();
        let base = ScratchLevel::create(scratch.join("base.records"), [1, 1, 2]).unwrap();
        build_base(&inventory, geometry, 7, &base, &NOT_CANCELLED).unwrap();
        let child = ScratchLevel::create(scratch.join("child.records"), [1, 1, 1]).unwrap();
        reduce_level(&base, &child, &NOT_CANCELLED).unwrap();
        assert_eq!(child.read_plane(0).unwrap(), vec![7, 1]);
        base.remove().unwrap();
        child.remove().unwrap();
    }

    #[test]
    fn corner_sentinels_have_clipped_two_and_three_dimensional_guards() {
        let two = tempfile::tempdir().unwrap();
        let mut two_pixels = vec![9; 9];
        two_pixels[0] = 255;
        let two_source = write_source(&two, 3, 3, &[two_pixels]);
        let two_scratch = two.path().join("scratch");
        fs::create_dir(&two_scratch).unwrap();
        let two_inventory = enumerate_source(&two_source, &NOT_CANCELLED).unwrap();
        let two_geometry = inspect_source(&two_inventory, &NOT_CANCELLED).unwrap();
        let two_base = ScratchLevel::create(two_scratch.join("base.records"), [1, 3, 3]).unwrap();
        build_base(&two_inventory, two_geometry, 255, &two_base, &NOT_CANCELLED).unwrap();
        let two_records = all_level_records(&two_base);
        assert_eq!(
            two_records
                .chunks_exact(2)
                .filter(|record| record[1] == 0)
                .count(),
            4
        );
        assert_eq!(
            two_records
                .chunks_exact(2)
                .filter(|record| record[1] == 1)
                .count(),
            5
        );
        two_base.remove().unwrap();

        let three = tempfile::tempdir().unwrap();
        let mut three_planes = vec![vec![9; 9]; 3];
        three_planes[0][0] = 255;
        let three_source = write_source(&three, 3, 3, &three_planes);
        let three_scratch = three.path().join("scratch");
        fs::create_dir(&three_scratch).unwrap();
        let three_inventory = enumerate_source(&three_source, &NOT_CANCELLED).unwrap();
        let three_geometry = inspect_source(&three_inventory, &NOT_CANCELLED).unwrap();
        let three_base =
            ScratchLevel::create(three_scratch.join("base.records"), [3, 3, 3]).unwrap();
        build_base(
            &three_inventory,
            three_geometry,
            255,
            &three_base,
            &NOT_CANCELLED,
        )
        .unwrap();
        let three_records = all_level_records(&three_base);
        assert_eq!(
            three_records
                .chunks_exact(2)
                .filter(|record| record[1] == 0)
                .count(),
            8
        );
        assert_eq!(
            three_records
                .chunks_exact(2)
                .filter(|record| record[1] == 1)
                .count(),
            19
        );
        three_base.remove().unwrap();
    }

    #[test]
    fn odd_tail_valid_only_half_up_mean_precedes_child_dilation() {
        let temporary = tempfile::tempdir().unwrap();
        let parent =
            ScratchLevel::create(temporary.path().join("parent.records"), [1, 3, 5]).unwrap();
        parent
            .write_plane(
                0,
                &[
                    1, 1, 0, 0, 4, 1, 5, 1, 0, 0, 7, 1, 8, 1, 0, 0, 10, 1, 0, 0, 12, 1, 13, 1, 14,
                    1, 15, 1, 16, 1,
                ],
            )
            .unwrap();
        let raw = reduce_raw_plane(&parent, 0, &NOT_CANCELLED).unwrap();
        assert_eq!(raw.records, vec![5, 1, 6, 1, 0, 0, 13, 1, 15, 1, 16, 1]);
        let final_records = finish_raw_plane(None, &raw, None, &NOT_CANCELLED).unwrap();
        assert_eq!(final_records, vec![5, 1, 0, 0, 0, 0, 13, 1, 0, 0, 0, 0]);
        parent.remove().unwrap();
    }

    #[test]
    fn generated_stack_facts_are_deterministic_and_count_every_scale_record() {
        let temporary = tempfile::tempdir().unwrap();
        let width = 300_u32;
        let height = 3_u32;
        let mut planes = (0..3_u32)
            .map(|z| {
                (0..width * height)
                    .map(|index| u8::try_from((index + z * 17) % 251).unwrap())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        planes[1][450] = 255;
        let source = write_source(&temporary, width, height, &planes);
        let scratch = temporary.path().join("scratch");
        fs::create_dir(&scratch).unwrap();

        assert_eq!(
            t5_shapes([3, 3, 300]),
            vec![
                [3, 3, 300],
                [2, 2, 150],
                [1, 1, 75],
                [1, 1, 38],
                [1, 1, 19],
                [1, 1, 10],
                [1, 1, 5],
            ]
        );
        let first = derive(request(&source, &scratch)).unwrap();
        assert!(scratch_entries(&scratch).is_empty());
        let second = derive(request(&source, &scratch)).unwrap();
        assert!(scratch_entries(&scratch).is_empty());

        assert_eq!(first.scientific_content_id, second.scientific_content_id);
        assert_eq!(first.layer_roots, second.layer_roots);
        assert_eq!(first.scales, second.scales);
        assert_eq!(first.transforms, second.transforms);
        assert_eq!(first.canonical_base_pixel_bytes, 2_700);
        assert_eq!(first.scales.len(), 7);
        assert_eq!(first.scales[0].brick_reads, 5);
        assert_eq!(first.scales[0].logical_voxels, 2_700);
        assert_eq!(first.scales[1].brick_reads, 3);
        assert_eq!(first.scales[1].logical_voxels, 600);
        assert_eq!(
            first.scientific_content_id,
            "m4d-sc-v1-sha256:70497ea4a813bc02a93cd9cbea348aee689ec5d27658ee809c180f319cfedf48"
        );
        assert_eq!(
            first.layer_roots[0].digest_sha256,
            "e1ba902b5a3a1f93122a3a3efd14142e8fa066525143b5bc8592d1f645d1e42a"
        );
        assert_eq!(
            first.scales[0].digest_sha256,
            "e6f323faa8dcec711b38705c1f949cd2a134d13a66f225c847a9ba0fbda50f14"
        );
        assert_eq!(
            first.scales[1].digest_sha256,
            "4443fbff0f72ccbd4f5f0d9a8d28bf9e2e5928fdd3d53122b10a2ac5c4b5867f"
        );
        assert_eq!(first.transforms[0].scale_zyx, [2.0, 3.0, 5.0]);
        assert_eq!(first.transforms[0].translation_zyx, [0.0, 0.0, 0.0]);
        assert_eq!(first.transforms[1].scale_zyx, [4.0, 6.0, 10.0]);
        assert_eq!(first.transforms[1].translation_zyx, [1.0, 1.5, 2.5]);
        assert_eq!(first.resources.source_planes, 3);
        assert_eq!(first.resources.source_files, 3);
        assert_eq!(first.resources.peak_scratch_regular_files, 2);
        assert_eq!(first.resources.scratch_files_remaining, 0);
        assert_eq!(first.resources.calculated_open_files_bound, 4);
        assert!(first.resources.deterministic_filename_order);
        assert!(first.resources.source_unchanged);
        assert!(first.resources.calculated_peak_working_bytes <= 64 * 1024 * 1024);
    }

    #[test]
    fn t5_shape_authority_is_independent_and_requires_the_pinned_seven_levels() {
        assert_eq!(
            t5_shapes([1, 64, 256]),
            vec![
                [1, 64, 256],
                [1, 32, 128],
                [1, 16, 64],
                [1, 8, 32],
                [1, 4, 16],
                [1, 2, 8],
                [1, 1, 4],
            ]
        );
        let shapes = t5_shapes([1_025, 2_049, 4_097]);
        assert_eq!(shapes.len(), 7);
        assert_eq!(shapes.last(), Some(&[17, 33, 65]));
    }

    #[test]
    fn malformed_layouts_compression_and_insufficient_memory_fail_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let source = source_folder(&temporary);
        fs::create_dir(source.join("channel-001")).unwrap();
        let scratch = temporary.path().join("scratch");
        fs::create_dir(&scratch).unwrap();
        assert!(derive(request(&source, &scratch)).is_err());

        fs::remove_dir(source.join("channel-001")).unwrap();
        let compressed = source.join("channel-000/plane-z00000.tif");
        let file = File::create(&compressed).unwrap();
        TiffEncoder::new(file)
            .unwrap()
            .with_compression(Compression::Lzw)
            .write_image::<colortype::Gray8>(2, 2, &[1, 2, 3, 4])
            .unwrap();
        assert!(derive(request(&source, &scratch)).is_err());

        fs::remove_file(&compressed).unwrap();
        let outside = temporary.path().join("outside.tif");
        write_plane(&outside, 2, 2, &[1, 2, 3, 4]);
        symlink(&outside, &compressed).unwrap();
        assert!(derive(request(&source, &scratch)).is_err());

        fs::remove_file(&compressed).unwrap();
        write_plane(&compressed, 2, 2, &[1, 2, 3, 4]);
        let mut tiny = request(&source, &scratch);
        tiny.working_memory_bytes = 1;
        assert!(derive(tiny).is_err());
        assert!(scratch_entries(&scratch).is_empty());
    }

    #[test]
    fn raw_preflight_rejects_huge_truncated_and_out_of_file_ifd_extents() {
        let temporary = tempfile::tempdir().unwrap();
        let valid = temporary.path().join("valid.tif");
        write_plane(&valid, 2, 2, &[1, 2, 3, 4]);
        assert_eq!(
            raw_tiff_preflight(&File::open(&valid).unwrap(), &NOT_CANCELLED).unwrap(),
            RawTiffFacts {
                width: 2,
                height: 2
            }
        );
        let big = temporary.path().join("valid-big.tif");
        TiffEncoder::new_big(File::create(&big).unwrap())
            .unwrap()
            .write_image::<colortype::Gray8>(2, 2, &[1, 2, 3, 4])
            .unwrap();
        assert_eq!(
            raw_tiff_preflight(&File::open(big).unwrap(), &NOT_CANCELLED).unwrap(),
            RawTiffFacts {
                width: 2,
                height: 2
            }
        );

        let mut huge = fs::read(&valid).unwrap();
        assert_eq!(&huge[..2], b"II");
        let ifd = usize::try_from(u32::from_le_bytes(huge[4..8].try_into().unwrap())).unwrap();
        huge[ifd..ifd + 2].copy_from_slice(
            &u16::try_from(MAX_TIFF_IFD_ENTRIES + 1)
                .unwrap()
                .to_le_bytes(),
        );
        let huge_path = temporary.path().join("huge-count.tif");
        fs::write(&huge_path, huge).unwrap();
        assert!(raw_tiff_preflight(&File::open(huge_path).unwrap(), &NOT_CANCELLED).is_err());

        let truncated_path = temporary.path().join("truncated.tif");
        fs::write(&truncated_path, [b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0]).unwrap();
        assert!(raw_tiff_preflight(&File::open(truncated_path).unwrap(), &NOT_CANCELLED).is_err());

        let mut outside = fs::read(&valid).unwrap();
        let ifd = usize::try_from(u32::from_le_bytes(outside[4..8].try_into().unwrap())).unwrap();
        let entries = usize::from(u16::from_le_bytes(
            outside[ifd..ifd + 2].try_into().unwrap(),
        ));
        let bad_offset = u32::try_from(outside.len() + 64).unwrap().to_le_bytes();
        let mut patched = false;
        for entry in 0..entries {
            let start = ifd + 2 + entry * 12;
            if u16::from_le_bytes(outside[start..start + 2].try_into().unwrap()) == 273 {
                outside[start + 8..start + 12].copy_from_slice(&bad_offset);
                patched = true;
            }
        }
        assert!(patched);
        let outside_path = temporary.path().join("outside.tif");
        fs::write(&outside_path, outside).unwrap();
        assert!(raw_tiff_preflight(&File::open(outside_path).unwrap(), &NOT_CANCELLED).is_err());
    }

    #[test]
    fn raw_preflight_rejects_unbounded_constructor_vector_tags() {
        let entry = |tag| RawTiffEntry {
            tag,
            field_type: 3,
            count: 2,
            value_bytes: 4,
            inline_capacity: 4,
            value_field: [0; 8],
            external_offset: None,
        };
        assert!(validate_raw_tag_shapes(&[entry(338)]).is_err());
        assert!(validate_raw_tag_shapes(&[entry(530)]).is_err());
        assert!(validate_raw_tag_shapes(&[entry(65_000)]).is_err());
    }

    #[test]
    fn cancellation_in_base_and_reduction_cleans_levels_and_preserves_source() {
        let temporary = tempfile::tempdir().unwrap();
        let source = write_source(&temporary, 8, 4, &[vec![9; 32], vec![10; 32]]);
        let source_files = fs::read_dir(source.join("channel-000"))
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                (path.clone(), fs::read(path).unwrap())
            })
            .collect::<Vec<_>>();
        let inventory = enumerate_source(&source, &NOT_CANCELLED).unwrap();
        let geometry = inspect_source(&inventory, &NOT_CANCELLED).unwrap();

        let base_scratch = temporary.path().join("base-cancel");
        fs::create_dir(&base_scratch).unwrap();
        let base_cancelled = AtomicBool::new(true);
        {
            let base = ScratchLevel::create(
                base_scratch.join(format!("{SCRATCH_PREFIX}base.records")),
                geometry.shape_zyx,
            )
            .unwrap();
            assert!(build_base(&inventory, geometry, 255, &base, &base_cancelled).is_err());
        }
        assert!(scratch_entries(&base_scratch).is_empty());

        let reduction_scratch = temporary.path().join("reduction-cancel");
        fs::create_dir(&reduction_scratch).unwrap();
        let reduction_cancelled = AtomicBool::new(false);
        {
            let base = ScratchLevel::create(
                reduction_scratch.join(format!("{SCRATCH_PREFIX}parent.records")),
                geometry.shape_zyx,
            )
            .unwrap();
            build_base(&inventory, geometry, 255, &base, &reduction_cancelled).unwrap();
            let child = ScratchLevel::create(
                reduction_scratch.join(format!("{SCRATCH_PREFIX}child.records")),
                geometry.shape_zyx.map(|dimension| dimension.div_ceil(2)),
            )
            .unwrap();
            reduction_cancelled.store(true, Ordering::Release);
            assert!(reduce_level(&base, &child, &reduction_cancelled).is_err());
        }
        assert!(scratch_entries(&reduction_scratch).is_empty());
        for (path, expected) in source_files {
            assert_eq!(fs::read(path).unwrap(), expected);
        }
    }

    #[test]
    fn scratch_level_drop_removes_only_its_create_new_artifact() {
        let temporary = tempfile::tempdir().unwrap();
        let retained = temporary.path().join("retained.txt");
        fs::write(&retained, b"caller-owned").unwrap();
        {
            let scratch = ScratchLevel::create(
                temporary
                    .path()
                    .join(format!("{SCRATCH_PREFIX}drop.records")),
                [1, 1, 1],
            )
            .unwrap();
            scratch.write_plane(0, &[9, 1]).unwrap();
        }
        assert_eq!(fs::read(&retained).unwrap(), b"caller-owned");
        assert_eq!(scratch_entries(temporary.path()), vec![retained]);
    }
}
