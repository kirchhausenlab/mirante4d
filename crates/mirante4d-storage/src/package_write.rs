use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, Metadata, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use mirante4d_identity::{
    ExactBytesHasher, IdentityHashError, PackageId, Sha256Digest, Sha256Hasher,
};
use rustix::time::{ClockId, clock_gettime};
use thiserror::Error;

use crate::local_publication::{LocalPublication, LocalPublicationError, PublicationCheckpoint};
use crate::package_science::refresh_publication_currentness;
use crate::range_io::LocalObjectSnapshot;
use crate::shard::encode_shard_index_tail;
use crate::{
    CanonicalEncodedInner, ControlError, DatasetProfileAdmission, DisplayDefaults,
    LocalPackageCatalog, ManifestRoot, OmeImageGroupMetadata, PackageObjectDescriptor,
    PackageObjectKind, PackageOpenError, PackagePath, PackageStructureError,
    PackageValidationError, PortableRecord, ProfileHeader, ProfileKind, RangeReadError,
    ScienceDescriptor, ScientificPackageValidationError, ScientificPublicationTransferError,
    SelfConsistentPackageCapability, ShardCodecError, ShardProfileKind, StorageProfileError,
    ZarrArrayMetadata, ZarrGroupMetadata, ZarrMetadataError, encode_inner_payload,
    manifest_page_path, pack_manifest_pages, profile_limits,
};

const PROFILE_PATH: &str = "m4d/profile.json";

/// One profile-addressed Zarr array whose metadata bytes are writer-owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageArrayInput {
    path: PackagePath,
    metadata: ZarrArrayMetadata,
}

impl PackageArrayInput {
    pub const fn new(path: PackagePath, metadata: ZarrArrayMetadata) -> Self {
        Self { path, metadata }
    }

    pub const fn path(&self) -> &PackagePath {
        &self.path
    }

    pub const fn metadata(&self) -> &ZarrArrayMetadata {
        &self.metadata
    }
}

/// One bounded outer shard supplied as validated inner chunks in slot order.
///
/// A caller may generate these values lazily. The writer consumes and drops
/// each complete outer shard before requesting the next one.
#[derive(Debug, PartialEq, Eq)]
pub struct PackageShardInput {
    array_path: PackagePath,
    outer_coordinates: Vec<u64>,
    chunks: Vec<Option<PackageInnerChunk>>,
}

#[derive(Debug, PartialEq, Eq)]
enum PackageInnerChunk {
    Decoded(Vec<u8>),
    Encoded(CanonicalEncodedInner),
}

impl PackageShardInput {
    pub fn new(
        array_path: PackagePath,
        outer_coordinates: Vec<u64>,
        decoded_chunks: Vec<Option<Vec<u8>>>,
    ) -> Self {
        Self {
            array_path,
            outer_coordinates,
            chunks: decoded_chunks
                .into_iter()
                .map(|chunk| chunk.map(PackageInnerChunk::Decoded))
                .collect(),
        }
    }

    /// Supplies already-canonical inner payloads. The caller must construct
    /// fresh values through `CanonicalEncodedInner::encode` and persisted or
    /// otherwise untrusted values through `CanonicalEncodedInner::validate`.
    pub fn new_encoded(
        array_path: PackagePath,
        outer_coordinates: Vec<u64>,
        encoded_chunks: Vec<Option<CanonicalEncodedInner>>,
    ) -> Self {
        Self {
            array_path,
            outer_coordinates,
            chunks: encoded_chunks
                .into_iter()
                .map(|chunk| chunk.map(PackageInnerChunk::Encoded))
                .collect(),
        }
    }

    pub const fn array_path(&self) -> &PackagePath {
        &self.array_path
    }

    pub fn outer_coordinates(&self) -> &[u64] {
        &self.outer_coordinates
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }
}

/// Complete typed input to the deterministic package writer.
///
/// `shards` may be any lazy iterator. Encoded shards, descriptors, manifest
/// pages, roots, and their paths are deliberately not accepted as input.
pub struct PackageWriteInput<I> {
    profile_kind: ProfileKind,
    profile: ProfileHeader,
    science: ScienceDescriptor,
    display_defaults: DisplayDefaults,
    portable_records: Vec<PortableRecord>,
    ome_images: Vec<OmeImageGroupMetadata>,
    arrays: Vec<PackageArrayInput>,
    shards: I,
}

const RESUMABLE_CONTROL_DIRECTORY: &str = ".mirante4d-import-control";
const RESUMABLE_STAGE_HEADER: &str = "stage-header";
const RESUMABLE_STAGE_JOURNAL: &str = "stage-journal";
const RESUMABLE_STAGE_SCHEMA: &[u8] = b"mirante4d-final-layout-stage-v1\0";
const RESUMABLE_JOURNAL_RECORD_MAX: usize = 8 + 1 + 4 + 512 + 32;

/// One destination-layout package stage whose completed shard prefix survives
/// cancellation or process loss.
///
/// The stage is the sole durable encoded payload authority. Callers retain
/// only bounded per-unit scratch and compact identity/index control state.
/// Every committed input record follows a stage-wide durability barrier, so a
/// journal prefix never names bytes that were only present in page cache.
pub struct ResumableLocalPackageStage {
    publication: LocalPublication,
    control_directory: PathBuf,
    journal: File,
    durable_inputs: u64,
    durable_payload_bytes: u64,
    /// Cumulative durable payload after each input ordinal. Entry zero is the
    /// empty prefix; missing/all-fill shard inputs repeat the prior value.
    durable_payload_prefix: Vec<u64>,
    pending: Vec<StageJournalRecord>,
    descriptors: Vec<PackageObjectDescriptor>,
    written_paths: BTreeSet<PackagePath>,
    syncs: PackageSyncCounters,
    codecs: PackageCodecCounters,
}

#[derive(Clone)]
struct StageJournalRecord {
    ordinal: u64,
    descriptor: Option<PackageObjectDescriptor>,
}

impl ResumableLocalPackageStage {
    /// Opens or creates the caller-named hidden stage. `stage_path` and the
    /// destination must be distinct siblings; this is what makes final
    /// publication a create-only atomic rename rather than a copy.
    pub fn open_or_create(
        destination: impl AsRef<Path>,
        stage_path: impl AsRef<Path>,
        binding: Sha256Digest,
    ) -> Result<Self, PackageWriteError> {
        let stage_path = stage_path.as_ref().to_path_buf();
        recover_control_excursion(&stage_path)?;
        if let Ok(metadata) = fs::symlink_metadata(&stage_path) {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return invalid_input("resumable stage path is not a real directory");
            }
            let control = stage_path.join(RESUMABLE_CONTROL_DIRECTORY);
            if !control.exists()
                && fs::read_dir(&stage_path)
                    .map_err(|source| PackageWriteError::Io {
                        operation: "inspect an unbound resumable stage",
                        source,
                    })?
                    .next()
                    .is_some()
            {
                return invalid_input("unbound resumable stage is not empty");
            }
        }
        let mut publication = LocalPublication::open_or_create_persistent(destination, &stage_path)
            .map_err(map_publication_error_without_commit)?;
        let control_directory = stage_path.join(RESUMABLE_CONTROL_DIRECTORY);
        open_or_create_control_directory(&control_directory)?;
        let header_path = control_directory.join(RESUMABLE_STAGE_HEADER);
        open_or_validate_stage_header(&header_path, binding)?;
        let journal_path = control_directory.join(RESUMABLE_STAGE_JOURNAL);
        let (journal, records) = open_stage_journal(&journal_path)?;
        let durable_inputs =
            u64::try_from(records.len()).map_err(|_| PackageWriteError::InvalidInput {
                reason: "resumable stage journal record count exceeds u64",
            })?;
        let mut descriptors = Vec::new();
        let mut written_paths = BTreeSet::new();
        let mut durable_payload_bytes = 0_u64;
        let mut durable_payload_prefix = Vec::with_capacity(records.len().saturating_add(1));
        durable_payload_prefix.push(0);
        for record in &records {
            if let Some(descriptor) = &record.descriptor {
                if !written_paths.insert(descriptor.path().clone()) {
                    return invalid_input("resumable stage journal repeats an object path");
                }
                durable_payload_bytes = durable_payload_bytes
                    .checked_add(descriptor.raw().byte_length())
                    .ok_or(PackageWriteError::InvalidInput {
                        reason: "resumable stage payload byte count overflowed",
                    })?;
                descriptors.push(descriptor.clone());
            }
            durable_payload_prefix.push(durable_payload_bytes);
        }
        remove_uncommitted_stage_entries(&stage_path, &written_paths)?;
        validate_committed_descriptors(&stage_path, &descriptors)?;
        publication
            .refresh_created_directories()
            .map_err(map_publication_error_without_commit)?;
        Ok(Self {
            publication,
            control_directory,
            journal,
            durable_inputs,
            durable_payload_bytes,
            durable_payload_prefix,
            pending: Vec::new(),
            descriptors,
            written_paths,
            syncs: PackageSyncCounters::default(),
            codecs: PackageCodecCounters::default(),
        })
    }

    pub const fn durable_shard_inputs(&self) -> u64 {
        self.durable_inputs
    }

    /// Exact regular-file payload bytes named by the durable shard journal.
    /// Private control and not-yet-produced metadata are intentionally
    /// excluded so import progress can report them separately.
    pub const fn durable_payload_bytes(&self) -> u64 {
        self.durable_payload_bytes
    }

    /// Exact payload bytes already made durable for a half-open shard-input
    /// ordinal range. This lets a resumable producer reserve only the missing
    /// suffix of its current bounded unit instead of charging its completed
    /// prefix again.
    pub fn durable_payload_bytes_in_input_range(
        &self,
        start: u64,
        end: u64,
    ) -> Result<u64, PackageWriteError> {
        if start > end || end > self.durable_inputs {
            return invalid_input("durable payload range is outside the committed input prefix");
        }
        let start = usize::try_from(start).map_err(|_| PackageWriteError::InvalidInput {
            reason: "durable payload range start is not addressable",
        })?;
        let end = usize::try_from(end).map_err(|_| PackageWriteError::InvalidInput {
            reason: "durable payload range end is not addressable",
        })?;
        self.durable_payload_prefix[end]
            .checked_sub(self.durable_payload_prefix[start])
            .ok_or(PackageWriteError::InvalidInput {
                reason: "durable payload prefix is not monotonic",
            })
    }

    pub fn next_shard_input_ordinal(&self) -> Result<u64, PackageWriteError> {
        self.durable_inputs
            .checked_add(u64::try_from(self.pending.len()).map_err(|_| {
                PackageWriteError::InvalidInput {
                    reason: "pending stage input count exceeds u64",
                }
            })?)
            .ok_or(PackageWriteError::InvalidInput {
                reason: "stage input ordinal overflowed",
            })
    }

    /// Writes one deterministic shard input to its final package-relative
    /// location. The object becomes resumable only after `commit_pending`.
    pub fn append_shard(
        &mut self,
        expected_ordinal: u64,
        shard: PackageShardInput,
        object_kind: PackageObjectKind,
        codec_kind: ShardProfileKind,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), PackageWriteError> {
        if expected_ordinal != self.next_shard_input_ordinal()? {
            return invalid_input("resumable stage shard input arrived out of order");
        }
        if !matches!(
            object_kind,
            PackageObjectKind::PixelShard
                | PackageObjectKind::ValidityShard
                | PackageObjectKind::PackedIndexShard
        ) {
            return invalid_input("resumable stage accepts only package shard objects");
        }
        let expected_coordinates = if object_kind == PackageObjectKind::PackedIndexShard {
            2
        } else {
            5
        };
        if shard.outer_coordinates.len() != expected_coordinates
            || (object_kind == PackageObjectKind::PackedIndexShard
                && shard.outer_coordinates[1] != 0)
            || shard.chunks.len() != codec_kind.chunks_per_shard()
        {
            return invalid_input("resumable stage shard geometry is inconsistent");
        }
        check_cancelled(&mut is_cancelled)?;
        let descriptor = if shard.chunks.iter().all(Option::is_none) {
            if object_kind == PackageObjectKind::PackedIndexShard {
                return invalid_input("a packed-index shard cannot contain only missing slots");
            }
            None
        } else {
            let path = shard_path(&shard.array_path, &shard.outer_coordinates)?;
            if self.written_paths.contains(&path)
                || self.pending.iter().any(|record| {
                    record
                        .descriptor
                        .as_ref()
                        .is_some_and(|descriptor| descriptor.path() == &path)
                })
            {
                return invalid_input("two resumable shard inputs derive the same object path");
            }
            let (descriptor, _snapshot) = write_shard(
                &mut self.publication,
                path,
                object_kind,
                codec_kind,
                shard.chunks,
                &mut is_cancelled,
                &mut self.codecs,
            )?;
            Some(descriptor)
        };
        self.pending.push(StageJournalRecord {
            ordinal: expected_ordinal,
            descriptor,
        });
        Ok(())
    }

    /// Makes the current bounded suffix durable and advances the compact
    /// completion journal. A crash before this returns merely redoes that
    /// suffix; it cannot expose a partial public package.
    pub fn commit_pending(
        &mut self,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), PackageWriteError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let report = self
            .publication
            .sync_stage_paths(
                self.pending
                    .iter()
                    .filter_map(|record| record.descriptor.as_ref())
                    .map(PackageObjectDescriptor::path),
                &mut is_cancelled,
            )
            .map_err(map_publication_error_without_commit)?;
        self.syncs.record_publication(report);
        for record in &self.pending {
            append_stage_record(&mut self.journal, record)?;
        }
        let started = Instant::now();
        self.journal
            .sync_data()
            .map_err(|source| PackageWriteError::Io {
                operation: "synchronize the resumable stage journal",
                source,
            })?;
        self.syncs.record(started.elapsed());
        for record in self.pending.drain(..) {
            self.durable_inputs =
                self.durable_inputs
                    .checked_add(1)
                    .ok_or(PackageWriteError::InvalidInput {
                        reason: "durable stage input count overflowed",
                    })?;
            if let Some(descriptor) = record.descriptor {
                self.durable_payload_bytes = self
                    .durable_payload_bytes
                    .checked_add(descriptor.raw().byte_length())
                    .ok_or(PackageWriteError::InvalidInput {
                        reason: "resumable stage payload byte count overflowed",
                    })?;
                self.written_paths.insert(descriptor.path().clone());
                self.descriptors.push(descriptor);
            }
            self.durable_payload_prefix.push(self.durable_payload_bytes);
        }
        Ok(())
    }

    /// Completes metadata and manifest authority, validates the exact staged
    /// package, and atomically publishes the already-produced final layout.
    /// `input.shards` must be empty: payload production belongs exclusively to
    /// `append_shard`, which is what prevents a second dataset-scale copy.
    pub fn finalize_scientifically_validated<I>(
        mut self,
        input: PackageWriteInput<I>,
        mut is_cancelled: impl FnMut() -> bool,
        mut observer: impl FnMut(PackageWriteEvent),
    ) -> Result<PublishedScientificPackageTransfer, PackageWriteError>
    where
        I: IntoIterator<Item = PackageShardInput>,
    {
        self.commit_pending(&mut is_cancelled)?;
        check_cancelled(&mut is_cancelled)?;
        let publication_clock =
            PackageWriteStageClock::start(PackageWriteStage::ShardPublication, &mut observer);
        let PackageWriteInput {
            profile_kind,
            profile,
            science,
            display_defaults,
            portable_records,
            ome_images,
            arrays,
            shards,
        } = input;
        if shards.into_iter().next().is_some() {
            return invalid_input("resumable finalization received a second shard stream");
        }
        let PreparedMetadata { objects, arrays } = prepare_metadata(
            &profile,
            &science,
            &display_defaults,
            &portable_records,
            &ome_images,
            arrays,
        )?;
        drop((science, display_defaults, portable_records, ome_images));
        validate_staged_shard_descriptors(&self.descriptors, &arrays)?;
        let limits = profile_limits(profile_kind);
        let mut snapshots = Vec::new();
        for object in objects {
            check_cancelled(&mut is_cancelled)?;
            require_descriptor_capacity(self.descriptors.len(), limits.total_physical_objects)?;
            require_new_path(&mut self.written_paths, &object.path)?;
            let (descriptor, snapshot) = write_object_bytes(
                &mut self.publication,
                object.path,
                object.kind,
                &object.bytes,
            )?;
            self.descriptors.push(descriptor);
            snapshots.push(snapshot);
        }
        drop(arrays);
        check_cancelled(&mut is_cancelled)?;
        let pages = pack_manifest_pages(self.descriptors)?;
        let root = ManifestRoot::new(&pages)?;
        for (ordinal, page) in pages.iter().enumerate() {
            check_cancelled(&mut is_cancelled)?;
            let ordinal = u32::try_from(ordinal).map_err(|_| PackageWriteError::InvalidInput {
                reason: "manifest page ordinal exceeds u32",
            })?;
            let path = manifest_page_path(ordinal)?;
            let snapshot =
                write_authority_bytes(&mut self.publication, path, &page.canonical_bytes()?)?;
            snapshots.push(snapshot);
        }
        drop(pages);
        let root_path = profile.manifest_root_path().clone();
        drop(profile);
        let root_snapshot =
            write_authority_bytes(&mut self.publication, root_path, &root.canonical_bytes()?)?;
        snapshots.push(root_snapshot);
        let package_id = root.package_id()?;

        // A complete package inventory cannot contain private import control.
        // Move it to a private sibling for validation; the guard restores it
        // after any prepublication failure or removes it after success.
        drop(self.journal);
        let mut control =
            ControlExcursion::begin(self.publication.stage_path(), &self.control_directory)?;
        self.publication
            .refresh_created_directories()
            .map_err(map_publication_error_without_commit)?;
        let report = self
            .publication
            .sync_stage(&mut is_cancelled)
            .map_err(map_publication_error_without_commit)?;
        self.syncs.record_publication(report);
        check_cancelled(&mut is_cancelled)?;
        publication_clock.finish(&mut observer);

        let structure_clock = PackageWriteStageClock::start(
            PackageWriteStage::StagedStructureValidation,
            &mut observer,
        );
        let catalog = LocalPackageCatalog::open(self.publication.stage_path())?;
        if catalog.declared_package_id() != package_id {
            return invalid_input("the staged manifest root changed before validation");
        }
        let admission = catalog
            .validate_package_structure(profile_kind, &mut is_cancelled)
            .map_err(map_structure_error)?;
        for snapshot in &snapshots {
            check_cancelled(&mut is_cancelled)?;
            catalog.reader().revalidate_snapshot(snapshot)?;
        }
        structure_clock.finish(&mut observer);
        let structure_object_reads = catalog.reader().object_open_operations();

        let exact_clock =
            PackageWriteStageClock::start(PackageWriteStage::StagedExactValidation, &mut observer);
        let exact = catalog
            .validate_exact_package(profile_kind, &mut is_cancelled)
            .map_err(map_exact_validation_error)?;
        if exact.package_id() != package_id || exact.admission() != admission {
            return invalid_input("staged exact validation disagrees with writer admission");
        }
        let exact_finished = exact.catalog().reader().object_open_operations();
        let exact_object_reads = exact_finished.checked_sub(structure_object_reads).ok_or(
            PackageWriteError::InvalidInput {
                reason: "staged exact object-read counter regressed",
            },
        )?;
        exact_clock.finish(&mut observer);

        let scientific_clock = PackageWriteStageClock::start(
            PackageWriteStage::StagedScientificValidation,
            &mut observer,
        );
        let self_consistent = exact
            .validate_scientific_content(&mut is_cancelled)
            .map_err(map_scientific_validation_error)?;
        let scientific_validation_report = Some(self_consistent.validation_report());
        let scientific_finished = self_consistent.catalog().reader().object_open_operations();
        let scientific_object_reads = scientific_finished.checked_sub(exact_finished).ok_or(
            PackageWriteError::InvalidInput {
                reason: "staged scientific object-read counter regressed",
            },
        )?;
        check_cancelled(&mut is_cancelled)?;
        scientific_clock.finish(&mut observer);

        let commit_clock = PackageWriteStageClock::start(PackageWriteStage::Commit, &mut observer);
        let publication_currentness_started =
            self_consistent.catalog().reader().object_open_operations();
        let validation_codec_decode_operations =
            self_consistent.catalog().reader().codec_decode_operations();
        let validation_codec_decode_time_ns =
            self_consistent.catalog().reader().codec_decode_time_ns();
        let prepared = self_consistent
            .prepare_atomic_publication(self.publication.stage_path(), &mut is_cancelled)
            .map_err(map_scientific_publication_transfer_error)?;
        let publication_currentness_reads = prepared
            .object_open_operations()
            .checked_sub(publication_currentness_started)
            .ok_or(PackageWriteError::InvalidInput {
                reason: "prepublication object-read counter regressed",
            })?;
        let validation_read_report = crate::PackageValidationReadReport::new(
            structure_object_reads,
            exact_object_reads,
            scientific_object_reads
                .checked_add(publication_currentness_reads)
                .ok_or(PackageWriteError::InvalidInput {
                    reason: "staged scientific object-read counter overflowed",
                })?,
        );
        let codec_report = crate::PackageCodecReport::new(
            self.codecs.encode_calls,
            self.codecs.encode_time_ns,
            validation_codec_decode_operations,
            validation_codec_decode_time_ns,
        );
        let mut hook = |_, _| Ok(());
        let (commit_syncs, capability) = self
            .publication
            .commit_self_consistent(package_id, prepared, &mut is_cancelled, &mut hook)
            .map_err(|error| map_publication_error(error, package_id))?;
        self.syncs.record_publication(commit_syncs);
        commit_clock.finish(&mut observer);
        control.discard_after_publication();
        let receipt = PackageWriteReceipt {
            package_id,
            admission,
            scientific_validation_report,
            validation_read_report,
            codec_report,
            sync_calls: self.syncs.calls,
            sync_time_ns: self.syncs.time_ns,
        };
        let destination = capability.root_path().to_path_buf();
        Ok(PublishedScientificPackageTransfer {
            receipt,
            destination,
            capability,
        })
    }
}

impl<I> PackageWriteInput<I> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        profile_kind: ProfileKind,
        profile: ProfileHeader,
        science: ScienceDescriptor,
        display_defaults: DisplayDefaults,
        portable_records: Vec<PortableRecord>,
        ome_images: Vec<OmeImageGroupMetadata>,
        arrays: Vec<PackageArrayInput>,
        shards: I,
    ) -> Self {
        Self {
            profile_kind,
            profile,
            science,
            display_defaults,
            portable_records,
            ome_images,
            arrays,
            shards,
        }
    }
}

/// Durable result of one create-only package publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackageWriteReceipt {
    package_id: PackageId,
    admission: DatasetProfileAdmission,
    scientific_validation_report: Option<crate::ScientificValidationReport>,
    validation_read_report: crate::PackageValidationReadReport,
    codec_report: crate::PackageCodecReport,
    sync_calls: u64,
    sync_time_ns: u64,
}

/// One material stage performed by the create-only package writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageWriteStage {
    ShardPublication,
    StagedStructureValidation,
    StagedExactValidation,
    StagedScientificValidation,
    Commit,
}

/// Monotonic wall and process-CPU time measured for one completed writer stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackageWriteStageTiming {
    pub stage: PackageWriteStage,
    pub wall_time_ns: u64,
    pub cpu_time_ns: u64,
}

/// Real-time stage observation from the scientific package writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageWriteEvent {
    StageStarted(PackageWriteStage),
    StageFinished(PackageWriteStageTiming),
}

impl PackageWriteReceipt {
    pub const fn package_id(self) -> PackageId {
        self.package_id
    }

    pub const fn admission(self) -> DatasetProfileAdmission {
        self.admission
    }

    pub const fn scientific_validation_report(self) -> Option<crate::ScientificValidationReport> {
        self.scientific_validation_report
    }

    pub const fn validation_read_report(self) -> crate::PackageValidationReadReport {
        self.validation_read_report
    }

    pub const fn codec_report(self) -> crate::PackageCodecReport {
        self.codec_report
    }

    /// Successful staged-filesystem and directory durability calls made by the
    /// package writer. Checkpoint synchronization is accounted by the importer.
    pub const fn sync_calls(self) -> u64 {
        self.sync_calls
    }

    pub const fn sync_time_ns(self) -> u64 {
        self.sync_time_ns
    }
}

/// Linear handoff from a scientifically validated create-only publication to
/// normal product use.
///
/// The transfer owns the exact capability rebound to the published root. It is
/// deliberately not `Clone`; callers must consume it through the final
/// metadata-only currentness sandwich before obtaining the capability.
#[derive(Debug)]
#[must_use = "a scientific publication must consume its one-shot self-consistent-package transfer"]
pub struct PublishedScientificPackageTransfer {
    receipt: PackageWriteReceipt,
    destination: PathBuf,
    capability: SelfConsistentPackageCapability,
}

impl PublishedScientificPackageTransfer {
    pub const fn receipt(&self) -> PackageWriteReceipt {
        self.receipt
    }

    pub fn destination(&self) -> &Path {
        &self.destination
    }

    pub const fn package_id(&self) -> PackageId {
        self.receipt.package_id
    }

    pub const fn scientific_content_id(&self) -> mirante4d_identity::ScientificContentId {
        self.capability.scientific_content_id()
    }

    /// Consumes the one-shot publication transfer and issues the capability
    /// only after the published directory passes inventory/snapshot/inventory
    /// freshness checks. The returned execution evidence is inseparable from
    /// the consumed capability at this hard-cut boundary.
    pub fn consume(
        self,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<
        (
            SelfConsistentPackageCapability,
            crate::ScientificPublicationTransferEvidence,
        ),
        ScientificPublicationTransferError,
    > {
        let evidence = refresh_publication_currentness(&self.capability, &mut is_cancelled)?;
        Ok((self.capability, evidence))
    }
}

#[derive(Default)]
struct PackageSyncCounters {
    calls: u64,
    time_ns: u64,
}

#[derive(Default)]
struct PackageCodecCounters {
    encode_calls: u64,
    encode_time_ns: u64,
}

impl PackageCodecCounters {
    fn record_encode(&mut self, elapsed: Duration) -> Result<(), PackageWriteError> {
        self.encode_calls =
            self.encode_calls
                .checked_add(1)
                .ok_or(PackageWriteError::InvalidInput {
                    reason: "the package codec encode-call counter overflowed",
                })?;
        self.encode_time_ns = self
            .encode_time_ns
            .checked_add(u64::try_from(elapsed.as_nanos()).map_err(|_| {
                PackageWriteError::InvalidInput {
                    reason: "the package codec encode-time counter overflowed",
                }
            })?)
            .ok_or(PackageWriteError::InvalidInput {
                reason: "the package codec encode-time counter overflowed",
            })?;
        Ok(())
    }
}

impl PackageSyncCounters {
    fn record_publication(&mut self, report: crate::local_publication::PublicationSyncReport) {
        // Publication has already performed these durability operations. Keep
        // evidence collection infallible, particularly after atomic rename.
        self.calls = self.calls.saturating_add(report.calls);
        self.time_ns = self.time_ns.saturating_add(report.time_ns);
    }

    fn record(&mut self, elapsed: Duration) {
        self.calls = self.calls.saturating_add(1);
        self.time_ns = self
            .time_ns
            .saturating_add(u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX));
    }
}

/// Typed failure from deterministic package construction or publication.
#[derive(Debug, Error)]
pub enum PackageWriteError {
    #[error("package writing was cancelled before publication")]
    Cancelled,
    #[error("invalid package writer input: {reason}")]
    InvalidInput { reason: &'static str },
    #[error("the destination already exists and was not changed")]
    DestinationExists,
    #[error("atomic create-only directory publication is unsupported on this filesystem")]
    AtomicPublishUnsupported,
    #[error("filesystem-wide package durability requires Linux kernel 5.8 or newer")]
    FilesystemDurabilityUnsupported,
    #[error("package {package_id} became visible, but final directory durability is unknown")]
    CommitIndeterminate {
        package_id: PackageId,
        #[source]
        source: io::Error,
    },
    #[error("package filesystem operation {operation} failed")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error(transparent)]
    Identity(#[from] IdentityHashError),
    #[error(transparent)]
    Metadata(#[from] ZarrMetadataError),
    #[error(transparent)]
    Shard(#[from] ShardCodecError),
    #[error(transparent)]
    Open(#[from] PackageOpenError),
    #[error(transparent)]
    Structure(#[from] PackageStructureError),
    #[error(transparent)]
    ExactValidation(PackageValidationError),
    #[error(transparent)]
    ScientificValidation(ScientificPackageValidationError),
    #[error(transparent)]
    ScientificPublicationTransfer(ScientificPublicationTransferError),
    #[error(transparent)]
    Range(#[from] RangeReadError),
    #[error(transparent)]
    Profile(#[from] StorageProfileError),
}

/// Sole writer for a new local target-profile package.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalPackageWriter;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StagedValidation {
    Structure,
    Scientific,
}

enum PackageWriteOutcome {
    Structure(Box<PackageWriteReceipt>),
    Scientific(Box<PublishedScientificPackageTransfer>),
}

impl LocalPackageWriter {
    /// Writes, validates, and atomically publishes one previously absent
    /// package directory.
    pub fn write_new<I>(
        destination: impl AsRef<Path>,
        input: PackageWriteInput<I>,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<PackageWriteReceipt, PackageWriteError>
    where
        I: IntoIterator<Item = PackageShardInput>,
    {
        let mut observer = |_| {};
        let mut self_consistent_commit_hook = |_, _| Ok(());
        match Self::write_new_with_validation(
            destination,
            input,
            &mut is_cancelled,
            StagedValidation::Structure,
            &mut observer,
            &mut self_consistent_commit_hook,
        )? {
            PackageWriteOutcome::Structure(receipt) => Ok(*receipt),
            PackageWriteOutcome::Scientific(_) => unreachable!("structure writer outcome"),
        }
    }

    /// Writes a new package, proves its exact-byte closure and declared
    /// declared content-address consistency while it is staged, and publishes it atomically
    /// only after both validations succeed.
    pub fn write_new_scientifically_validated<I>(
        destination: impl AsRef<Path>,
        input: PackageWriteInput<I>,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<PublishedScientificPackageTransfer, PackageWriteError>
    where
        I: IntoIterator<Item = PackageShardInput>,
    {
        let mut observer = |_| {};
        let mut self_consistent_commit_hook = |_, _| Ok(());
        match Self::write_new_with_validation(
            destination,
            input,
            &mut is_cancelled,
            StagedValidation::Scientific,
            &mut observer,
            &mut self_consistent_commit_hook,
        )? {
            PackageWriteOutcome::Scientific(transfer) => Ok(*transfer),
            PackageWriteOutcome::Structure(_) => unreachable!("scientific writer outcome"),
        }
    }

    /// Writes and scientifically validates a new package while reporting the
    /// five material publication stages as they actually execute.
    pub fn write_new_scientifically_validated_observed<I>(
        destination: impl AsRef<Path>,
        input: PackageWriteInput<I>,
        mut is_cancelled: impl FnMut() -> bool,
        mut observer: impl FnMut(PackageWriteEvent),
    ) -> Result<PublishedScientificPackageTransfer, PackageWriteError>
    where
        I: IntoIterator<Item = PackageShardInput>,
    {
        let mut self_consistent_commit_hook = |_, _| Ok(());
        match Self::write_new_with_validation(
            destination,
            input,
            &mut is_cancelled,
            StagedValidation::Scientific,
            &mut observer,
            &mut self_consistent_commit_hook,
        )? {
            PackageWriteOutcome::Scientific(transfer) => Ok(*transfer),
            PackageWriteOutcome::Structure(_) => unreachable!("scientific writer outcome"),
        }
    }

    fn write_new_with_validation<I>(
        destination: impl AsRef<Path>,
        input: PackageWriteInput<I>,
        mut is_cancelled: &mut impl FnMut() -> bool,
        staged_validation: StagedValidation,
        observer: &mut impl FnMut(PackageWriteEvent),
        self_consistent_commit_hook: &mut impl FnMut(PublicationCheckpoint, PackageId) -> io::Result<()>,
    ) -> Result<PackageWriteOutcome, PackageWriteError>
    where
        I: IntoIterator<Item = PackageShardInput>,
    {
        check_cancelled(&mut is_cancelled)?;
        let publication_clock =
            PackageWriteStageClock::start(PackageWriteStage::ShardPublication, observer);
        let PackageWriteInput {
            profile_kind,
            profile,
            science,
            display_defaults,
            portable_records,
            ome_images,
            arrays,
            shards,
        } = input;
        let prepared = prepare_metadata(
            &profile,
            &science,
            &display_defaults,
            &portable_records,
            &ome_images,
            arrays,
        )?;
        drop((science, display_defaults, portable_records, ome_images));
        let PreparedMetadata { objects, arrays } = prepared;
        let limits = profile_limits(profile_kind);
        let shard_input_limit = limits
            .pixel_shards
            .checked_add(limits.validity_shards)
            .and_then(|value| value.checked_add(limits.packed_index_shards))
            .ok_or(PackageWriteError::InvalidInput {
                reason: "the selected profile shard-input bound overflowed",
            })?;

        check_cancelled(&mut is_cancelled)?;
        let mut publication =
            LocalPublication::begin(destination).map_err(map_publication_error_without_commit)?;
        let mut descriptors = Vec::new();
        let mut snapshots = Vec::new();
        let mut written_paths = BTreeSet::new();
        let mut syncs = PackageSyncCounters::default();
        let mut codecs = PackageCodecCounters::default();

        for object in objects {
            check_cancelled(&mut is_cancelled)?;
            require_descriptor_capacity(descriptors.len(), limits.total_physical_objects)?;
            require_new_path(&mut written_paths, &object.path)?;
            let (descriptor, snapshot) =
                write_object_bytes(&mut publication, object.path, object.kind, &object.bytes)?;
            descriptors.push(descriptor);
            snapshots.push(snapshot);
        }

        let mut shard_inputs_seen = 0_u64;
        let mut shards = shards.into_iter();
        // A `for` loop would consume and drop the iterator before validation.
        #[allow(clippy::while_let_on_iterator)]
        while let Some(shard) = shards.next() {
            check_cancelled(&mut is_cancelled)?;
            shard_inputs_seen =
                shard_inputs_seen
                    .checked_add(1)
                    .ok_or(PackageWriteError::InvalidInput {
                        reason: "the shard-input count overflowed",
                    })?;
            if shard_inputs_seen > shard_input_limit {
                return invalid_input("shard inputs exceed the selected profile bound");
            }
            let array = arrays
                .get(&shard.array_path)
                .ok_or(PackageWriteError::InvalidInput {
                    reason: "a shard names an array outside the profile",
                })?;
            let expected_coordinates = match array.shard_kind {
                PackageObjectKind::PackedIndexShard => 2,
                PackageObjectKind::PixelShard | PackageObjectKind::ValidityShard => 5,
                _ => unreachable!("prepared arrays have only shard object kinds"),
            };
            if shard.outer_coordinates.len() != expected_coordinates
                || (array.shard_kind == PackageObjectKind::PackedIndexShard
                    && shard.outer_coordinates[1] != 0)
            {
                return Err(PackageWriteError::InvalidInput {
                    reason: "a shard has the wrong outer-coordinate shape",
                });
            }
            if shard.chunks.len() != array.metadata.kind().chunks_per_shard() {
                return Err(ShardCodecError::ChunkCount {
                    expected: array.metadata.kind().chunks_per_shard(),
                    actual: shard.chunks.len(),
                }
                .into());
            }
            if shard.chunks.iter().all(Option::is_none) {
                if array.shard_kind == PackageObjectKind::PackedIndexShard {
                    return Err(PackageWriteError::InvalidInput {
                        reason: "a packed-index shard cannot contain only missing slots",
                    });
                }
                continue;
            }

            let shard_path = shard_path(&shard.array_path, &shard.outer_coordinates)?;
            require_descriptor_capacity(descriptors.len(), limits.total_physical_objects)?;
            require_new_path(&mut written_paths, &shard_path)?;
            let (descriptor, snapshot) = write_shard(
                &mut publication,
                shard_path,
                array.shard_kind,
                array.metadata.kind(),
                shard.chunks,
                &mut is_cancelled,
                &mut codecs,
            )?;
            descriptors.push(descriptor);
            snapshots.push(snapshot);
        }

        check_cancelled(&mut is_cancelled)?;
        drop((arrays, written_paths));
        let pages = pack_manifest_pages(descriptors)?;
        let root = ManifestRoot::new(&pages)?;
        for (ordinal, page) in pages.iter().enumerate() {
            check_cancelled(&mut is_cancelled)?;
            let ordinal = u32::try_from(ordinal).map_err(|_| PackageWriteError::InvalidInput {
                reason: "manifest page ordinal exceeds u32",
            })?;
            let path = manifest_page_path(ordinal)?;
            let snapshot = write_authority_bytes(&mut publication, path, &page.canonical_bytes()?)?;
            snapshots.push(snapshot);
        }
        drop(pages);
        let root_path = profile.manifest_root_path().clone();
        drop(profile);
        let root_snapshot =
            write_authority_bytes(&mut publication, root_path, &root.canonical_bytes()?)?;
        snapshots.push(root_snapshot);
        let package_id = root.package_id()?;

        let stage_syncs = publication
            .sync_stage(&mut is_cancelled)
            .map_err(map_publication_error_without_commit)?;
        syncs.record_publication(stage_syncs);
        check_cancelled(&mut is_cancelled)?;
        publication_clock.finish(observer);

        let structure_clock =
            PackageWriteStageClock::start(PackageWriteStage::StagedStructureValidation, observer);
        let catalog = LocalPackageCatalog::open(publication.stage_path())?;
        if catalog.declared_package_id() != package_id {
            return Err(PackageWriteError::InvalidInput {
                reason: "the staged manifest root changed before validation",
            });
        }
        let admission = catalog
            .validate_package_structure(profile_kind, &mut is_cancelled)
            .map_err(map_structure_error)?;
        for snapshot in &snapshots {
            check_cancelled(&mut is_cancelled)?;
            catalog.reader().revalidate_snapshot(snapshot)?;
        }
        drop(snapshots);
        check_cancelled(&mut is_cancelled)?;
        structure_clock.finish(observer);
        let structure_object_reads = catalog.reader().object_open_operations();

        let mut scientific_validation_report = None;
        let mut validation_read_report =
            crate::PackageValidationReadReport::new(structure_object_reads, 0, 0);
        let mut codec_report = crate::PackageCodecReport::new(
            codecs.encode_calls,
            codecs.encode_time_ns,
            catalog.reader().codec_decode_operations(),
            catalog.reader().codec_decode_time_ns(),
        );
        let self_consistent = if staged_validation == StagedValidation::Scientific {
            let exact_clock =
                PackageWriteStageClock::start(PackageWriteStage::StagedExactValidation, observer);
            let exact = catalog
                .validate_exact_package(profile_kind, &mut is_cancelled)
                .map_err(map_exact_validation_error)?;
            if exact.package_id() != package_id || exact.admission() != admission {
                return invalid_input("staged exact validation disagrees with writer admission");
            }
            let exact_finished = exact.catalog().reader().object_open_operations();
            let exact_object_reads = exact_finished.checked_sub(structure_object_reads).ok_or(
                PackageWriteError::InvalidInput {
                    reason: "staged exact object-read counter regressed",
                },
            )?;
            exact_clock.finish(observer);
            let scientific_clock = PackageWriteStageClock::start(
                PackageWriteStage::StagedScientificValidation,
                observer,
            );
            let self_consistent = exact
                .validate_scientific_content(&mut is_cancelled)
                .map_err(map_scientific_validation_error)?;
            scientific_validation_report = Some(self_consistent.validation_report());
            let scientific_finished = self_consistent.catalog().reader().object_open_operations();
            let scientific_object_reads = scientific_finished.checked_sub(exact_finished).ok_or(
                PackageWriteError::InvalidInput {
                    reason: "staged scientific object-read counter regressed",
                },
            )?;
            validation_read_report = crate::PackageValidationReadReport::new(
                structure_object_reads,
                exact_object_reads,
                scientific_object_reads,
            );
            codec_report = crate::PackageCodecReport::new(
                codecs.encode_calls,
                codecs.encode_time_ns,
                self_consistent.catalog().reader().codec_decode_operations(),
                self_consistent.catalog().reader().codec_decode_time_ns(),
            );
            check_cancelled(&mut is_cancelled)?;
            scientific_clock.finish(observer);
            Some(self_consistent)
        } else {
            None
        };

        // Some lazy producers retain the final bounded-memory lease in their
        // iterator. Keep that lease alive through staged validation.
        drop(shards);

        let commit_clock = PackageWriteStageClock::start(PackageWriteStage::Commit, observer);
        match self_consistent {
            Some(self_consistent) => {
                let publication_currentness_started =
                    self_consistent.catalog().reader().object_open_operations();
                let prepared = self_consistent
                    .prepare_atomic_publication(publication.stage_path(), &mut is_cancelled)
                    .map_err(map_scientific_publication_transfer_error)?;
                let publication_currentness_reads = prepared
                    .object_open_operations()
                    .checked_sub(publication_currentness_started)
                    .ok_or(PackageWriteError::InvalidInput {
                        reason: "prepublication object-read counter regressed",
                    })?;
                let scientific_object_reads = validation_read_report
                    .scientific_object_reads()
                    .checked_add(publication_currentness_reads)
                    .ok_or(PackageWriteError::InvalidInput {
                        reason: "staged scientific object-read counter overflowed",
                    })?;
                validation_read_report = crate::PackageValidationReadReport::new(
                    validation_read_report.structure_object_reads(),
                    validation_read_report.exact_object_reads(),
                    scientific_object_reads,
                );
                let (commit_syncs, capability) = publication
                    .commit_self_consistent(
                        package_id,
                        prepared,
                        &mut is_cancelled,
                        self_consistent_commit_hook,
                    )
                    .map_err(|error| map_publication_error(error, package_id))?;
                syncs.record_publication(commit_syncs);
                commit_clock.finish(observer);
                let receipt = PackageWriteReceipt {
                    package_id,
                    admission,
                    scientific_validation_report,
                    validation_read_report,
                    codec_report,
                    sync_calls: syncs.calls,
                    sync_time_ns: syncs.time_ns,
                };
                let destination = capability.root_path().to_path_buf();
                Ok(PackageWriteOutcome::Scientific(Box::new(
                    PublishedScientificPackageTransfer {
                        receipt,
                        destination,
                        capability,
                    },
                )))
            }
            None => {
                let commit_syncs = publication
                    .commit(package_id)
                    .map_err(|error| map_publication_error(error, package_id))?;
                syncs.record_publication(commit_syncs);
                commit_clock.finish(observer);
                Ok(PackageWriteOutcome::Structure(Box::new(
                    PackageWriteReceipt {
                        package_id,
                        admission,
                        scientific_validation_report,
                        validation_read_report,
                        codec_report,
                        sync_calls: syncs.calls,
                        sync_time_ns: syncs.time_ns,
                    },
                )))
            }
        }
    }
}

struct PackageWriteStageClock {
    stage: PackageWriteStage,
    wall_started: Instant,
    cpu_started_ns: u64,
}

impl PackageWriteStageClock {
    fn start(stage: PackageWriteStage, observer: &mut impl FnMut(PackageWriteEvent)) -> Self {
        let clock = Self {
            stage,
            wall_started: Instant::now(),
            cpu_started_ns: process_cpu_time_ns(),
        };
        observer(PackageWriteEvent::StageStarted(stage));
        clock
    }

    fn finish(self, observer: &mut impl FnMut(PackageWriteEvent)) {
        observer(PackageWriteEvent::StageFinished(PackageWriteStageTiming {
            stage: self.stage,
            wall_time_ns: duration_ns(self.wall_started.elapsed()),
            cpu_time_ns: process_cpu_time_ns().saturating_sub(self.cpu_started_ns),
        }));
    }
}

fn process_cpu_time_ns() -> u64 {
    let time = clock_gettime(ClockId::ProcessCPUTime);
    let seconds = u64::try_from(time.tv_sec).expect("process CPU time cannot be negative");
    let nanoseconds = u64::try_from(time.tv_nsec).expect("clock nanoseconds cannot be negative");
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(nanoseconds))
        .expect("process CPU time fits in u64 nanoseconds")
}

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).expect("writer stage duration fits in u64 nanoseconds")
}

struct PreparedObject {
    path: PackagePath,
    kind: PackageObjectKind,
    bytes: Vec<u8>,
}

struct PreparedArray {
    metadata: ZarrArrayMetadata,
    shard_kind: PackageObjectKind,
}

struct PreparedMetadata {
    objects: Vec<PreparedObject>,
    arrays: BTreeMap<PackagePath, PreparedArray>,
}

fn prepare_metadata(
    profile: &ProfileHeader,
    science: &ScienceDescriptor,
    display_defaults: &DisplayDefaults,
    portable_records: &[PortableRecord],
    ome_images: &[OmeImageGroupMetadata],
    arrays: Vec<PackageArrayInput>,
) -> Result<PreparedMetadata, PackageWriteError> {
    if portable_records.len() != profile.portable_record_paths().len() {
        return invalid_input("portable-record count does not match the profile");
    }
    if ome_images.len() != profile.images().len() {
        return invalid_input("OME image metadata count does not match the profile");
    }

    let group_bytes = ZarrGroupMetadata::new().deterministic_bytes()?;
    let mut objects = vec![
        prepared(
            "zarr.json",
            PackageObjectKind::ZarrRoot,
            group_bytes.clone(),
        )?,
        prepared(
            "images/zarr.json",
            PackageObjectKind::ZarrImagesGroup,
            group_bytes.clone(),
        )?,
        prepared(
            "validity/zarr.json",
            PackageObjectKind::ZarrValidityGroup,
            group_bytes.clone(),
        )?,
        prepared(
            "indexes/zarr.json",
            PackageObjectKind::ZarrIndexesGroup,
            group_bytes,
        )?,
        PreparedObject {
            path: PackagePath::parse(PROFILE_PATH)?,
            kind: PackageObjectKind::Profile,
            bytes: profile.canonical_bytes()?,
        },
        PreparedObject {
            path: profile.science_path().clone(),
            kind: PackageObjectKind::Science,
            bytes: science.canonical_bytes()?,
        },
        PreparedObject {
            path: profile.display_defaults_path().clone(),
            kind: PackageObjectKind::DisplayDefaults,
            bytes: display_defaults.canonical_bytes()?,
        },
    ];

    for ((record, path), ordinal) in portable_records
        .iter()
        .zip(profile.portable_record_paths())
        .zip(0_u64..)
    {
        if record.record_ordinal().get() != ordinal {
            return invalid_input("portable-record ordinals must match their profile paths");
        }
        objects.push(PreparedObject {
            path: path.clone(),
            kind: PackageObjectKind::PortableRecord,
            bytes: record.canonical_bytes()?,
        });
    }

    for (image, ome) in profile.images().iter().zip(ome_images) {
        objects.push(PreparedObject {
            path: metadata_path(image.image_group_path())?,
            kind: PackageObjectKind::ZarrImageGroup,
            bytes: ome.deterministic_bytes()?,
        });
    }

    let expected = expected_arrays(profile)?;
    let mut prepared_arrays = BTreeMap::new();
    for array in arrays {
        let metadata_kind =
            expected
                .get(&array.path)
                .copied()
                .ok_or(PackageWriteError::InvalidInput {
                    reason: "array metadata names a path outside the profile",
                })?;
        if !array_kind_matches(metadata_kind, array.metadata.kind()) {
            return invalid_input("array metadata uses the wrong storage-profile row");
        }
        let shard_kind = shard_kind(metadata_kind);
        let metadata_bytes = array.metadata.deterministic_bytes()?;
        let path = metadata_path(&array.path)?;
        if prepared_arrays
            .insert(
                array.path,
                PreparedArray {
                    metadata: array.metadata,
                    shard_kind,
                },
            )
            .is_some()
        {
            return invalid_input("array metadata paths must be unique");
        }
        objects.push(PreparedObject {
            path,
            kind: metadata_kind,
            bytes: metadata_bytes,
        });
    }
    if prepared_arrays.len() != expected.len()
        || expected
            .keys()
            .any(|path| !prepared_arrays.contains_key(path))
    {
        return invalid_input("array metadata does not exactly cover the profile");
    }

    objects.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    Ok(PreparedMetadata {
        objects,
        arrays: prepared_arrays,
    })
}

fn expected_arrays(
    profile: &ProfileHeader,
) -> Result<BTreeMap<PackagePath, PackageObjectKind>, PackageWriteError> {
    let mut expected = BTreeMap::new();
    for image in profile.images() {
        for level in image.levels() {
            insert_expected(
                &mut expected,
                level.pixel_path().clone(),
                PackageObjectKind::ZarrPixelArray,
            )?;
            insert_expected(
                &mut expected,
                level.packed_index_path().clone(),
                PackageObjectKind::ZarrPackedIndexArray,
            )?;
            if let Some(path) = level.validity_path() {
                insert_expected(
                    &mut expected,
                    path.clone(),
                    PackageObjectKind::ZarrValidityArray,
                )?;
            }
        }
    }
    Ok(expected)
}

fn insert_expected(
    expected: &mut BTreeMap<PackagePath, PackageObjectKind>,
    path: PackagePath,
    kind: PackageObjectKind,
) -> Result<(), PackageWriteError> {
    if expected.insert(path, kind).is_some() {
        return invalid_input("profile array paths must be unique");
    }
    Ok(())
}

fn array_kind_matches(kind: PackageObjectKind, storage: ShardProfileKind) -> bool {
    match kind {
        PackageObjectKind::ZarrPixelArray => matches!(
            storage,
            ShardProfileKind::Pixel3dUint8
                | ShardProfileKind::Pixel3dUint16
                | ShardProfileKind::Pixel3dFloat32
                | ShardProfileKind::Pixel2dUint8
                | ShardProfileKind::Pixel2dUint16
                | ShardProfileKind::Pixel2dFloat32
        ),
        PackageObjectKind::ZarrValidityArray => matches!(
            storage,
            ShardProfileKind::Validity3d | ShardProfileKind::Validity2d
        ),
        PackageObjectKind::ZarrPackedIndexArray => storage == ShardProfileKind::PackedIndex,
        _ => false,
    }
}

fn shard_kind(metadata_kind: PackageObjectKind) -> PackageObjectKind {
    match metadata_kind {
        PackageObjectKind::ZarrPixelArray => PackageObjectKind::PixelShard,
        PackageObjectKind::ZarrValidityArray => PackageObjectKind::ValidityShard,
        PackageObjectKind::ZarrPackedIndexArray => PackageObjectKind::PackedIndexShard,
        _ => unreachable!("only array metadata kinds are prepared"),
    }
}

fn prepared(
    path: &str,
    kind: PackageObjectKind,
    bytes: Vec<u8>,
) -> Result<PreparedObject, PackageWriteError> {
    Ok(PreparedObject {
        path: PackagePath::parse(path)?,
        kind,
        bytes,
    })
}

fn metadata_path(base: &PackagePath) -> Result<PackagePath, PackageWriteError> {
    Ok(PackagePath::parse(&format!("{base}/zarr.json"))?)
}

fn shard_path(array: &PackagePath, coordinates: &[u64]) -> Result<PackagePath, PackageWriteError> {
    let coordinates = coordinates
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join("/");
    Ok(PackagePath::parse(&format!("{array}/c/{coordinates}"))?)
}

fn require_new_path(
    written: &mut BTreeSet<PackagePath>,
    path: &PackagePath,
) -> Result<(), PackageWriteError> {
    if !written.insert(path.clone()) {
        return invalid_input("two writer inputs derive the same package path");
    }
    Ok(())
}

fn require_descriptor_capacity(current: usize, maximum: u64) -> Result<(), PackageWriteError> {
    if u64::try_from(current).map_or(true, |current| current >= maximum) {
        invalid_input("manifest descriptors exceed the selected profile bound")
    } else {
        Ok(())
    }
}

fn write_object_bytes(
    publication: &mut LocalPublication,
    path: PackagePath,
    kind: PackageObjectKind,
    bytes: &[u8],
) -> Result<(PackageObjectDescriptor, LocalObjectSnapshot), PackageWriteError> {
    let (facts, snapshot) = write_hashed_file(publication, path.clone(), |file, hasher| {
        write_hashed(file, hasher, bytes)
    })?;
    let descriptor = PackageObjectDescriptor::new(path, kind, facts.byte_length(), facts.digest())?;
    Ok((descriptor, snapshot))
}

fn write_authority_bytes(
    publication: &mut LocalPublication,
    path: PackagePath,
    bytes: &[u8],
) -> Result<LocalObjectSnapshot, PackageWriteError> {
    write_hashed_file(publication, path, |file, hasher| {
        write_hashed(file, hasher, bytes)
    })
    .map(|(_facts, snapshot)| snapshot)
}

fn open_or_create_control_directory(path: &Path) -> Result<(), PackageWriteError> {
    match fs::create_dir(path) {
        Ok(()) => {
            let mut permissions = fs::metadata(path)
                .map_err(|source| PackageWriteError::Io {
                    operation: "inspect the new resumable control directory",
                    source,
                })?
                .permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(0o700);
                fs::set_permissions(path, permissions).map_err(|source| PackageWriteError::Io {
                    operation: "protect the resumable control directory",
                    source,
                })?;
            }
            Ok(())
        }
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path).map_err(|source| PackageWriteError::Io {
                operation: "inspect the resumable control directory",
                source,
            })?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                Ok(())
            } else {
                invalid_input("resumable stage control path is not a real directory")
            }
        }
        Err(source) => Err(PackageWriteError::Io {
            operation: "create the resumable control directory",
            source,
        }),
    }
}

fn open_or_validate_stage_header(
    path: &Path,
    binding: Sha256Digest,
) -> Result<(), PackageWriteError> {
    let mut body = Vec::with_capacity(RESUMABLE_STAGE_SCHEMA.len() + 64);
    body.extend_from_slice(RESUMABLE_STAGE_SCHEMA);
    body.extend_from_slice(binding.as_bytes());
    let digest = Sha256Hasher::digest(&body);
    body.extend_from_slice(digest.as_bytes());
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(&body)
                .map_err(|source| PackageWriteError::Io {
                    operation: "write the resumable stage header",
                    source,
                })?;
            file.sync_data().map_err(|source| PackageWriteError::Io {
                operation: "synchronize the resumable stage header",
                source,
            })?;
            Ok(())
        }
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            let actual = fs::read(path).map_err(|source| PackageWriteError::Io {
                operation: "read the resumable stage header",
                source,
            })?;
            if actual == body {
                Ok(())
            } else {
                invalid_input("resumable stage belongs to different import inputs")
            }
        }
        Err(source) => Err(PackageWriteError::Io {
            operation: "create the resumable stage header",
            source,
        }),
    }
}

fn open_stage_journal(path: &Path) -> Result<(File, Vec<StageJournalRecord>), PackageWriteError> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|source| PackageWriteError::Io {
            operation: "open the resumable stage journal",
            source,
        })?;
    let mut records = Vec::new();
    let mut durable_end = 0_u64;
    loop {
        let mut prefix = [0_u8; 13];
        match file.read_exact(&mut prefix) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::UnexpectedEof => {
                file.set_len(durable_end)
                    .map_err(|source| PackageWriteError::Io {
                        operation: "discard an incomplete resumable journal suffix",
                        source,
                    })?;
                break;
            }
            Err(source) => {
                return Err(PackageWriteError::Io {
                    operation: "read the resumable stage journal",
                    source,
                });
            }
        }
        let ordinal = u64::from_le_bytes(prefix[0..8].try_into().expect("fixed prefix"));
        let present = prefix[8];
        let length = u32::from_le_bytes(prefix[9..13].try_into().expect("fixed prefix"));
        let length = usize::try_from(length).map_err(|_| PackageWriteError::InvalidInput {
            reason: "resumable descriptor length is not addressable",
        })?;
        if ordinal
            != u64::try_from(records.len()).map_err(|_| PackageWriteError::InvalidInput {
                reason: "resumable journal record count exceeds u64",
            })?
            || present > 1
            || (present == 0 && length != 0)
            || length > 512
        {
            return invalid_input("resumable stage journal contains a malformed record");
        }
        let mut suffix = vec![0_u8; length + 32];
        if let Err(source) = file.read_exact(&mut suffix) {
            if source.kind() == io::ErrorKind::UnexpectedEof {
                file.set_len(durable_end)
                    .map_err(|source| PackageWriteError::Io {
                        operation: "discard an incomplete resumable journal suffix",
                        source,
                    })?;
                break;
            }
            return Err(PackageWriteError::Io {
                operation: "read the resumable stage journal record",
                source,
            });
        }
        let mut record_bytes = Vec::with_capacity(13 + length);
        record_bytes.extend_from_slice(&prefix);
        record_bytes.extend_from_slice(&suffix[..length]);
        let expected = Sha256Hasher::digest(&record_bytes);
        if expected.as_bytes() != &suffix[length..] {
            return invalid_input("resumable stage journal checksum failed");
        }
        let descriptor = if present == 1 {
            Some(PackageObjectDescriptor::parse_canonical(&suffix[..length])?)
        } else {
            None
        };
        records.push(StageJournalRecord {
            ordinal,
            descriptor,
        });
        durable_end = file
            .stream_position()
            .map_err(|source| PackageWriteError::Io {
                operation: "locate the resumable stage journal",
                source,
            })?;
    }
    file.seek(SeekFrom::End(0))
        .map_err(|source| PackageWriteError::Io {
            operation: "position the resumable stage journal",
            source,
        })?;
    Ok((file, records))
}

fn append_stage_record(
    journal: &mut File,
    record: &StageJournalRecord,
) -> Result<(), PackageWriteError> {
    let descriptor = record
        .descriptor
        .as_ref()
        .map(PackageObjectDescriptor::canonical_bytes)
        .transpose()?;
    let length = descriptor.as_ref().map_or(0, Vec::len);
    let mut bytes = Vec::with_capacity(RESUMABLE_JOURNAL_RECORD_MAX);
    bytes.extend_from_slice(&record.ordinal.to_le_bytes());
    bytes.push(u8::from(descriptor.is_some()));
    bytes.extend_from_slice(
        &u32::try_from(length)
            .map_err(|_| PackageWriteError::InvalidInput {
                reason: "resumable descriptor length exceeds u32",
            })?
            .to_le_bytes(),
    );
    if let Some(descriptor) = descriptor {
        bytes.extend_from_slice(&descriptor);
    }
    let digest = Sha256Hasher::digest(&bytes);
    bytes.extend_from_slice(digest.as_bytes());
    journal
        .write_all(&bytes)
        .map_err(|source| PackageWriteError::Io {
            operation: "append the resumable stage journal",
            source,
        })
}

fn remove_uncommitted_stage_entries(
    stage: &Path,
    committed: &BTreeSet<PackagePath>,
) -> Result<(), PackageWriteError> {
    fn visit(
        root: &Path,
        current: &Path,
        committed: &BTreeSet<PackagePath>,
    ) -> Result<bool, PackageWriteError> {
        let mut empty = true;
        for entry in fs::read_dir(current).map_err(|source| PackageWriteError::Io {
            operation: "enumerate the resumable package stage",
            source,
        })? {
            let entry = entry.map_err(|source| PackageWriteError::Io {
                operation: "read a resumable package stage entry",
                source,
            })?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| PackageWriteError::Io {
                operation: "inspect a resumable package stage entry",
                source,
            })?;
            if current == root && entry.file_name() == RESUMABLE_CONTROL_DIRECTORY {
                empty = false;
                continue;
            }
            if metadata.file_type().is_symlink() {
                return invalid_input("resumable package stage contains a symbolic link");
            }
            if metadata.is_dir() {
                if visit(root, &path, committed)? {
                    fs::remove_dir(&path).map_err(|source| PackageWriteError::Io {
                        operation: "remove an empty uncommitted staging directory",
                        source,
                    })?;
                } else {
                    empty = false;
                }
            } else if metadata.is_file() {
                let relative =
                    path.strip_prefix(root)
                        .map_err(|_| PackageWriteError::InvalidInput {
                            reason: "staging object escaped its root",
                        })?;
                let package_path = PackagePath::parse(relative.to_str().ok_or(
                    PackageWriteError::InvalidInput {
                        reason: "staging object path is not UTF-8",
                    },
                )?)?;
                if committed.contains(&package_path) {
                    empty = false;
                } else {
                    fs::remove_file(&path).map_err(|source| PackageWriteError::Io {
                        operation: "remove an uncommitted staging object",
                        source,
                    })?;
                }
            } else {
                return invalid_input("resumable package stage contains a non-regular object");
            }
        }
        Ok(empty)
    }
    let _ = visit(stage, stage, committed)?;
    Ok(())
}

fn validate_committed_descriptors(
    stage: &Path,
    descriptors: &[PackageObjectDescriptor],
) -> Result<(), PackageWriteError> {
    let mut buffer = vec![0_u8; 1024 * 1024];
    for descriptor in descriptors {
        let path = stage.join(descriptor.path().as_str());
        let metadata = fs::symlink_metadata(&path).map_err(|source| PackageWriteError::Io {
            operation: "inspect a committed staging object",
            source,
        })?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != descriptor.raw().byte_length()
        {
            return invalid_input("a committed staging object changed after its journal record");
        }
        let mut file = File::open(&path).map_err(|source| PackageWriteError::Io {
            operation: "open a committed staging object",
            source,
        })?;
        let mut hasher = ExactBytesHasher::new();
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|source| PackageWriteError::Io {
                    operation: "verify a committed staging object",
                    source,
                })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read])?;
        }
        if hasher.finalize()?.digest() != descriptor.raw().digest() {
            return invalid_input("a committed staging object checksum changed");
        }
    }
    Ok(())
}

fn validate_staged_shard_descriptors(
    descriptors: &[PackageObjectDescriptor],
    arrays: &BTreeMap<PackagePath, PreparedArray>,
) -> Result<(), PackageWriteError> {
    for descriptor in descriptors {
        if !matches!(
            descriptor.kind(),
            PackageObjectKind::PixelShard
                | PackageObjectKind::ValidityShard
                | PackageObjectKind::PackedIndexShard
        ) {
            return invalid_input("resumable stage journal contains a non-shard descriptor");
        }
        let matched = arrays.iter().find(|(path, _)| {
            descriptor
                .path()
                .as_str()
                .strip_prefix(path.as_str())
                .is_some_and(|suffix| suffix.starts_with("/c/"))
        });
        let Some((_path, array)) = matched else {
            return invalid_input("resumable stage shard names an array outside the profile");
        };
        if descriptor.kind() != array.shard_kind {
            return invalid_input("resumable stage shard kind differs from its array");
        }
    }
    Ok(())
}

struct ControlExcursion {
    stage_control: PathBuf,
    sidecar: PathBuf,
    active: bool,
}

impl ControlExcursion {
    fn begin(stage: &Path, stage_control: &Path) -> Result<Self, PackageWriteError> {
        let sidecar = control_sidecar(stage)?;
        if sidecar.exists() {
            return invalid_input("resumable stage control sidecar already exists");
        }
        fs::rename(stage_control, &sidecar).map_err(|source| PackageWriteError::Io {
            operation: "move private import control outside the validation inventory",
            source,
        })?;
        Ok(Self {
            stage_control: stage_control.to_path_buf(),
            sidecar,
            active: true,
        })
    }

    fn discard_after_publication(&mut self) {
        // Publication is already visible and durable. Control cleanup is
        // intentionally best-effort so an orphaned private sidecar cannot turn
        // a successful create-only commit into a false import failure.
        let _ = fs::remove_dir_all(&self.sidecar);
        self.active = false;
    }
}

impl Drop for ControlExcursion {
    fn drop(&mut self) {
        if self.active && !self.stage_control.exists() && self.sidecar.exists() {
            let _ = fs::rename(&self.sidecar, &self.stage_control);
        }
    }
}

fn recover_control_excursion(stage: &Path) -> Result<(), PackageWriteError> {
    let control = stage.join(RESUMABLE_CONTROL_DIRECTORY);
    let sidecar = control_sidecar(stage)?;
    match (control.exists(), sidecar.exists()) {
        (true, false) | (false, false) => Ok(()),
        (false, true) => fs::rename(&sidecar, &control).map_err(|source| PackageWriteError::Io {
            operation: "restore private import control after interrupted validation",
            source,
        }),
        (true, true) => invalid_input("resumable stage has two private control authorities"),
    }
}

fn control_sidecar(stage: &Path) -> Result<PathBuf, PackageWriteError> {
    let name = stage.file_name().and_then(|name| name.to_str()).ok_or(
        PackageWriteError::InvalidInput {
            reason: "resumable stage name is not UTF-8",
        },
    )?;
    Ok(stage.with_file_name(format!(".{name}.finalizing-control")))
}

#[allow(clippy::too_many_arguments)]
fn write_shard(
    publication: &mut LocalPublication,
    path: PackagePath,
    object_kind: PackageObjectKind,
    codec_kind: ShardProfileKind,
    chunks: Vec<Option<PackageInnerChunk>>,
    is_cancelled: &mut impl FnMut() -> bool,
    codecs: &mut PackageCodecCounters,
) -> Result<(PackageObjectDescriptor, LocalObjectSnapshot), PackageWriteError> {
    let (facts, snapshot) = write_hashed_file(publication, path.clone(), |file, hasher| {
        let mut lengths = Vec::with_capacity(codec_kind.chunks_per_shard());
        for chunk in chunks {
            check_cancelled(is_cancelled)?;
            match chunk {
                Some(PackageInnerChunk::Decoded(decoded)) => {
                    let encode_started = Instant::now();
                    let encoded = encode_inner_payload(codec_kind, &decoded)?;
                    codecs.record_encode(encode_started.elapsed())?;
                    let encoded_length = u64::try_from(encoded.len())
                        .map_err(|_| ShardCodecError::LengthOverflow)?;
                    write_hashed(file, hasher, &encoded)?;
                    lengths.push(Some(encoded_length));
                }
                Some(PackageInnerChunk::Encoded(encoded)) => {
                    if encoded.kind() != codec_kind {
                        return Err(PackageWriteError::InvalidInput {
                            reason: "an encoded inner payload uses the wrong storage kind",
                        });
                    }
                    let encoded_length = u64::try_from(encoded.bytes().len())
                        .map_err(|_| ShardCodecError::LengthOverflow)?;
                    write_hashed(file, hasher, encoded.bytes())?;
                    lengths.push(Some(encoded_length));
                }
                None => lengths.push(None),
            }
        }
        let tail = encode_shard_index_tail(codec_kind, &lengths)?;
        write_hashed(file, hasher, &tail)
    })?;
    let descriptor =
        PackageObjectDescriptor::new(path, object_kind, facts.byte_length(), facts.digest())?;
    Ok((descriptor, snapshot))
}

fn write_hashed_file(
    publication: &mut LocalPublication,
    path: PackagePath,
    write_body: impl FnOnce(&mut File, &mut ExactBytesHasher) -> Result<(), PackageWriteError>,
) -> Result<(mirante4d_identity::ExactBytesFacts, LocalObjectSnapshot), PackageWriteError> {
    let mut file = publication
        .create_file(&path)
        .map_err(map_publication_error_without_commit)?;
    let mut hasher = ExactBytesHasher::new();
    write_body(&mut file, &mut hasher)?;
    let metadata = file.metadata().map_err(|source| PackageWriteError::Io {
        operation: "inspect staged package object",
        source,
    })?;
    drop(file);
    let facts = hasher.finalize()?;
    if facts.byte_length() != metadata.len() {
        return invalid_input("the staged object length changed while it was written");
    }
    let snapshot = snapshot(path, &metadata)?;
    Ok((facts, snapshot))
}

fn write_hashed(
    file: &mut File,
    hasher: &mut ExactBytesHasher,
    bytes: &[u8],
) -> Result<(), PackageWriteError> {
    file.write_all(bytes)
        .map_err(|source| PackageWriteError::Io {
            operation: "write staged package object",
            source,
        })?;
    hasher.update(bytes)?;
    Ok(())
}

#[cfg(unix)]
fn snapshot(
    path: PackagePath,
    metadata: &Metadata,
) -> Result<LocalObjectSnapshot, PackageWriteError> {
    Ok(LocalObjectSnapshot::from_metadata(path, metadata)?)
}

#[cfg(not(unix))]
fn snapshot(
    _path: PackagePath,
    _metadata: &Metadata,
) -> Result<LocalObjectSnapshot, PackageWriteError> {
    Err(PackageWriteError::Range(
        RangeReadError::UnsupportedPlatform,
    ))
}

fn check_cancelled(is_cancelled: &mut impl FnMut() -> bool) -> Result<(), PackageWriteError> {
    if is_cancelled() {
        Err(PackageWriteError::Cancelled)
    } else {
        Ok(())
    }
}

fn invalid_input<T>(reason: &'static str) -> Result<T, PackageWriteError> {
    Err(PackageWriteError::InvalidInput { reason })
}

fn map_publication_error_without_commit(error: LocalPublicationError) -> PackageWriteError {
    match error {
        LocalPublicationError::Cancelled => PackageWriteError::Cancelled,
        LocalPublicationError::DestinationExists => PackageWriteError::DestinationExists,
        LocalPublicationError::AtomicPublishUnsupported { .. } => {
            PackageWriteError::AtomicPublishUnsupported
        }
        LocalPublicationError::FilesystemDurabilityUnsupported => {
            PackageWriteError::FilesystemDurabilityUnsupported
        }
        LocalPublicationError::CommitIndeterminate { source } => PackageWriteError::Io {
            operation: "unexpected precommit durability state",
            source,
        },
        LocalPublicationError::Io { operation, source } => {
            PackageWriteError::Io { operation, source }
        }
    }
}

fn map_publication_error(error: LocalPublicationError, package_id: PackageId) -> PackageWriteError {
    match error {
        LocalPublicationError::CommitIndeterminate { source } => {
            PackageWriteError::CommitIndeterminate { package_id, source }
        }
        other => map_publication_error_without_commit(other),
    }
}

fn map_structure_error(error: PackageStructureError) -> PackageWriteError {
    if matches!(
        error,
        PackageStructureError::Cancelled
            | PackageStructureError::Admission(crate::PackageAdmissionError::Inventory(
                crate::DirectoryInventoryError::Cancelled
            ))
    ) {
        PackageWriteError::Cancelled
    } else {
        PackageWriteError::Structure(error)
    }
}

fn map_exact_validation_error(error: PackageValidationError) -> PackageWriteError {
    if matches!(error, PackageValidationError::Cancelled) {
        PackageWriteError::Cancelled
    } else {
        PackageWriteError::ExactValidation(error)
    }
}

fn map_scientific_validation_error(error: ScientificPackageValidationError) -> PackageWriteError {
    if matches!(error, ScientificPackageValidationError::Cancelled) {
        PackageWriteError::Cancelled
    } else {
        PackageWriteError::ScientificValidation(error)
    }
}

fn map_scientific_publication_transfer_error(
    error: ScientificPublicationTransferError,
) -> PackageWriteError {
    if matches!(error, ScientificPublicationTransferError::Cancelled) {
        PackageWriteError::Cancelled
    } else {
        PackageWriteError::ScientificPublicationTransfer(error)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        fs,
        path::{Path, PathBuf},
        rc::Rc,
        sync::atomic::{AtomicU64, Ordering},
    };

    use mirante4d_domain::{IntensityDType, LogicalLayerKey, Shape4D};
    use mirante4d_identity::ScientificContentId;

    use super::*;
    use crate::{
        DisplayLayerDefaults, F32Bits, F64Bits, OmeInteroperabilityBase, OmeLevelTransform,
        PackedIndexCoordinates, PackedIndexRecord, PackedIndexStatistics, ProfileImage,
        ProfileLevel, ProfileLogicalLayer, ProfileValidityMode, Rgb24, ScienceLayer,
        ScienceTemporalCalibration,
    };

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[derive(Clone, Copy)]
    enum BrickMode {
        PixelPresent,
        AllFill,
        ExplicitValidity,
        ExplicitAllInvalid,
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "mirante4d-package-writer-{label}-{}-{}",
                std::process::id(),
                TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn writes_byte_identical_exact_packages_independent_of_parent_and_input_order() {
        let root = TestDirectory::new("deterministic");
        let first_parent = root.0.join("first");
        let second_parent = root.0.join("second");
        fs::create_dir(&first_parent).unwrap();
        fs::create_dir(&second_parent).unwrap();
        let first = first_parent.join("data.m4d");
        let second = second_parent.join("renamed.m4d");

        let first_receipt = LocalPackageWriter::write_new(
            &first,
            fixture_input(BrickMode::PixelPresent, false),
            || false,
        )
        .unwrap();
        let second_receipt = LocalPackageWriter::write_new(
            &second,
            fixture_input(BrickMode::PixelPresent, true),
            || false,
        )
        .unwrap();

        assert_eq!(first_receipt.package_id(), second_receipt.package_id());
        assert_eq!(first_receipt.admission(), second_receipt.admission());
        assert_eq!(
            first_receipt.validation_read_report(),
            second_receipt.validation_read_report()
        );
        assert_eq!(first_receipt.sync_calls(), second_receipt.sync_calls());
        assert_eq!(tree_bytes(&first), tree_bytes(&second));
        let capability = LocalPackageCatalog::open(&first)
            .unwrap()
            .validate_exact_package(ProfileKind::Current, || false)
            .unwrap();
        assert_eq!(capability.package_id(), first_receipt.package_id());
        assert_eq!(capability.admission(), first_receipt.admission());
        let brick = capability
            .read_brick(PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0), || false)
            .unwrap();
        assert_eq!(brick.logical_extent_zyx(), [1, 2, 3]);
        assert!(brick.pixel_payload().is_some());
        assert!(brick.validity_payload().is_none());
    }

    #[test]
    fn package_write_route_has_one_prevalidation_stage_barrier_and_no_object_sync() {
        let source = include_str!("package_write.rs");
        let route_start = source
            .find("    fn write_new_with_validation<I>(")
            .expect("package-write route must exist");
        let route_tail = &source[route_start..];
        let route_end = route_tail
            .find("\nstruct PackageWriteStageClock")
            .expect("stage clock must follow the package-write route");
        let route = &route_tail[..route_end];
        let helper_start = source
            .find("fn write_object_bytes(")
            .expect("object writer helpers must exist");
        let helper_tail = &source[helper_start..];
        let helper_end = helper_tail
            .find("\nfn write_authority_bytes(")
            .expect("authority writer must follow the ordinary object writer");
        let object_writers = &helper_tail[..helper_end];

        assert_eq!(route.matches(".sync_stage(").count(), 1);
        let barrier = route
            .find(".sync_stage(")
            .expect("the package route must synchronize its complete stage");
        let validation = route
            .find("PackageWriteStage::StagedStructureValidation")
            .expect("staged structure validation must remain explicit");
        assert!(barrier < validation);
        for forbidden in [
            ".sync_all(",
            ".sync_data(",
            "fdatasync(",
            "fsync(",
            "syncfs(",
        ] {
            assert!(
                !route.contains(forbidden) && !object_writers.contains(forbidden),
                "package construction contains a forbidden per-object durability route {forbidden}"
            );
        }
    }

    #[test]
    fn unsupported_filesystem_durability_remains_a_typed_writer_capability() {
        let error = map_publication_error_without_commit(
            LocalPublicationError::FilesystemDurabilityUnsupported,
        );
        assert!(matches!(
            error,
            PackageWriteError::FilesystemDurabilityUnsupported
        ));
    }

    #[test]
    fn writer_outputs_cover_fill_and_explicit_validity_modes() {
        for (label, mode, pixel, validity) in [
            ("all-fill", BrickMode::AllFill, false, false),
            ("explicit-validity", BrickMode::ExplicitValidity, true, true),
            (
                "explicit-all-invalid",
                BrickMode::ExplicitAllInvalid,
                false,
                false,
            ),
        ] {
            let root = TestDirectory::new(label);
            let destination = root.0.join("data.m4d");
            let receipt =
                LocalPackageWriter::write_new(&destination, fixture_input(mode, false), || false)
                    .unwrap();
            let capability = LocalPackageCatalog::open(&destination)
                .unwrap()
                .validate_exact_package(ProfileKind::Current, || false)
                .unwrap();
            assert_eq!(capability.package_id(), receipt.package_id());
            let brick = capability
                .read_brick(PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0), || false)
                .unwrap();
            assert_eq!(brick.pixel_payload().is_some(), pixel);
            assert_eq!(brick.validity_payload().is_some(), validity);
        }
    }

    #[test]
    fn cancellation_and_collision_never_touch_destination_or_source() {
        let root = TestDirectory::new("safety");
        let source = root.0.join("source.tif");
        fs::write(&source, b"immutable-source").unwrap();
        let checks = Cell::new(0_usize);
        LocalPackageWriter::write_new(
            root.0.join("count-checks.m4d"),
            fixture_input(BrickMode::PixelPresent, false),
            || {
                checks.set(checks.get() + 1);
                false
            },
        )
        .unwrap();
        let total_checks = checks.get();
        assert!(total_checks > 6);

        for (ordinal, cancel_at) in [
            0,
            total_checks / 3,
            (total_checks * 2) / 3,
            total_checks - 1,
        ]
        .into_iter()
        .enumerate()
        {
            let cancelled_destination = root.0.join(format!("cancelled-{ordinal}.m4d"));
            let checks = Cell::new(0_usize);
            let error = LocalPackageWriter::write_new(
                &cancelled_destination,
                fixture_input(BrickMode::PixelPresent, false),
                || {
                    let current = checks.get();
                    checks.set(current + 1);
                    current == cancel_at
                },
            )
            .unwrap_err();
            assert!(matches!(error, PackageWriteError::Cancelled));
            assert!(!cancelled_destination.exists());
            assert!(!fs::read_dir(&root.0).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".mirante4d-stage-")
            }));
        }

        let collision = root.0.join("collision.m4d");
        fs::write(&collision, b"keep-existing").unwrap();
        let error = LocalPackageWriter::write_new(
            &collision,
            fixture_input(BrickMode::PixelPresent, false),
            || false,
        )
        .unwrap_err();
        assert!(matches!(error, PackageWriteError::DestinationExists));
        assert_eq!(fs::read(collision).unwrap(), b"keep-existing");
        assert_eq!(fs::read(source).unwrap(), b"immutable-source");
    }

    #[test]
    fn scientific_writer_validates_the_stage_before_publication() {
        let root = TestDirectory::new("scientific-stage-validation");
        let mismatch = root.0.join("mismatch.m4d");
        let computed = match LocalPackageWriter::write_new_scientifically_validated(
            &mismatch,
            fixture_input(BrickMode::PixelPresent, false),
            || false,
        )
        .unwrap_err()
        {
            PackageWriteError::ScientificValidation(
                ScientificPackageValidationError::ScientificContentMismatch { computed, .. },
            ) => computed,
            other => panic!("unexpected staged-validation error: {other}"),
        };
        assert!(!mismatch.exists());

        let validated = root.0.join("validated.m4d");
        let transfer = LocalPackageWriter::write_new_scientifically_validated(
            &validated,
            fixture_input_with_scientific_id(BrickMode::PixelPresent, false, computed),
            || false,
        )
        .unwrap();
        let receipt = transfer.receipt();
        assert_eq!(transfer.destination(), validated.canonicalize().unwrap());
        let (capability, currentness) = transfer.consume(|| false).unwrap();
        assert_eq!(
            currentness.contract_id(),
            crate::SCIENTIFIC_PUBLICATION_CURRENTNESS_CONTRACT_ID
        );
        assert!(currentness.expected_snapshot_object_reads() > 0);
        assert_eq!(
            currentness.observed_snapshot_object_reads(),
            currentness.expected_snapshot_object_reads()
        );
        assert_eq!(
            currentness.first_inventory_object_reads(),
            currentness.second_inventory_object_reads()
        );
        assert_eq!(
            currentness.observed_total_object_reads(),
            currentness.first_inventory_object_reads()
                + currentness.observed_snapshot_object_reads()
                + currentness.second_inventory_object_reads()
        );
        assert_eq!(currentness.observed_codec_decode_calls(), 0);
        assert_eq!(capability.root_path(), validated.canonicalize().unwrap());
        assert_eq!(capability.package_id(), receipt.package_id());
        assert_eq!(capability.scientific_content_id(), computed);

        let cancelled = root.0.join("cancelled.m4d");
        let error = LocalPackageWriter::write_new_scientifically_validated(
            &cancelled,
            fixture_input_with_scientific_id(BrickMode::PixelPresent, false, computed),
            || true,
        )
        .unwrap_err();
        assert!(matches!(error, PackageWriteError::Cancelled));
        assert!(!cancelled.exists());
        assert!(!fs::read_dir(&root.0).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".mirante4d-stage-")
        }));
    }

    #[test]
    fn resumable_final_layout_stage_reopens_and_publishes_without_payload_copy() {
        let root = TestDirectory::new("resumable-final-layout");
        let scientific = computed_fixture_scientific_id(&root.0, "resumable");
        let destination = root.0.join("published.m4d");
        let stage_path = root.0.join("published.checkpoint");
        let binding = Sha256Hasher::digest(b"resumable fixture plan");
        let PackageWriteInput {
            profile_kind,
            profile,
            science,
            display_defaults,
            portable_records,
            ome_images,
            arrays,
            shards,
        } = fixture_input_with_scientific_id(BrickMode::PixelPresent, false, scientific);
        let mut shards = shards.into_iter();
        let pixel = shards.next().unwrap();
        let packed = shards.next().unwrap();
        assert!(shards.next().is_none());

        let mut stage =
            ResumableLocalPackageStage::open_or_create(&destination, &stage_path, binding).unwrap();
        stage
            .append_shard(
                0,
                pixel,
                PackageObjectKind::PixelShard,
                ShardProfileKind::Pixel2dUint8,
                || false,
            )
            .unwrap();
        stage.commit_pending(|| false).unwrap();
        let first_payload_bytes = stage.durable_payload_bytes();
        assert!(first_payload_bytes > 0);
        assert_eq!(
            stage.durable_payload_bytes_in_input_range(0, 1).unwrap(),
            first_payload_bytes
        );
        assert_eq!(stage.durable_payload_bytes_in_input_range(0, 0).unwrap(), 0);
        drop(stage);
        assert!(!destination.exists());
        assert!(stage_path.join(RESUMABLE_CONTROL_DIRECTORY).is_dir());

        let mut stage =
            ResumableLocalPackageStage::open_or_create(&destination, &stage_path, binding).unwrap();
        assert_eq!(stage.durable_shard_inputs(), 1);
        assert_eq!(stage.durable_payload_bytes(), first_payload_bytes);
        stage
            .append_shard(
                1,
                packed,
                PackageObjectKind::PackedIndexShard,
                ShardProfileKind::PackedIndex,
                || false,
            )
            .unwrap();
        stage.commit_pending(|| false).unwrap();
        let total_payload_bytes = stage.durable_payload_bytes();
        assert!(total_payload_bytes > first_payload_bytes);
        assert_eq!(
            stage.durable_payload_bytes_in_input_range(1, 2).unwrap(),
            total_payload_bytes - first_payload_bytes
        );
        let transfer = stage
            .finalize_scientifically_validated(
                PackageWriteInput::new(
                    profile_kind,
                    profile,
                    science,
                    display_defaults,
                    portable_records,
                    ome_images,
                    arrays,
                    Vec::<PackageShardInput>::new(),
                ),
                || false,
                |_| {},
            )
            .unwrap();

        assert!(destination.is_dir());
        assert!(!stage_path.exists());
        assert!(!destination.join(RESUMABLE_CONTROL_DIRECTORY).exists());
        let reference_destination = root.0.join("reference.m4d");
        let reference = LocalPackageWriter::write_new_scientifically_validated(
            &reference_destination,
            fixture_input_with_scientific_id(BrickMode::PixelPresent, false, scientific),
            || false,
        )
        .unwrap();
        assert_eq!(transfer.package_id(), reference.package_id());
    }

    #[test]
    fn resumable_stage_discards_uncommitted_suffix_and_rejects_corrupt_committed_bytes() {
        let root = TestDirectory::new("resumable-recovery");
        let destination = root.0.join("published.m4d");
        let stage_path = root.0.join("published.checkpoint");
        let binding = Sha256Hasher::digest(b"recovery fixture plan");
        let input = fixture_input(BrickMode::PixelPresent, false);
        let pixel_path = input.profile.images()[0].levels()[0].pixel_path().clone();
        let pixel = input.shards.into_iter().next().unwrap();
        let object_path = stage_path.join(format!("{pixel_path}/c/0/0/0/0/0"));

        let mut stage =
            ResumableLocalPackageStage::open_or_create(&destination, &stage_path, binding).unwrap();
        stage
            .append_shard(
                0,
                pixel,
                PackageObjectKind::PixelShard,
                ShardProfileKind::Pixel2dUint8,
                || false,
            )
            .unwrap();
        assert!(object_path.is_file());
        drop(stage);

        let mut stage =
            ResumableLocalPackageStage::open_or_create(&destination, &stage_path, binding).unwrap();
        assert_eq!(stage.durable_shard_inputs(), 0);
        assert!(!object_path.exists());
        let pixel = fixture_input(BrickMode::PixelPresent, false)
            .shards
            .into_iter()
            .next()
            .unwrap();
        stage
            .append_shard(
                0,
                pixel,
                PackageObjectKind::PixelShard,
                ShardProfileKind::Pixel2dUint8,
                || false,
            )
            .unwrap();
        stage.commit_pending(|| false).unwrap();
        drop(stage);

        fs::write(&object_path, b"corrupt committed shard").unwrap();
        assert!(
            ResumableLocalPackageStage::open_or_create(&destination, &stage_path, binding).is_err()
        );
        assert!(!destination.exists());
    }

    #[test]
    fn scientific_publication_transfer_rejects_precommit_inventory_and_root_substitution() {
        let root = TestDirectory::new("scientific-transfer-precommit-drift");
        let computed = computed_fixture_scientific_id(&root.0, "identity");

        let unlisted_destination = root.0.join("unlisted.m4d");
        let error = LocalPackageWriter::write_new_scientifically_validated_observed(
            &unlisted_destination,
            fixture_input_with_scientific_id(BrickMode::PixelPresent, false, computed),
            || false,
            |event| {
                if event == PackageWriteEvent::StageStarted(PackageWriteStage::Commit) {
                    fs::write(
                        only_publication_stage(&root.0).join("unlisted.bin"),
                        b"foreign",
                    )
                    .unwrap();
                }
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            PackageWriteError::ScientificPublicationTransfer(
                ScientificPublicationTransferError::Inventory(
                    crate::DirectoryInventoryError::UnexpectedFile { .. }
                )
            )
        ));
        assert!(!unlisted_destination.exists());

        let replaced_destination = root.0.join("replaced-root.m4d");
        let moved_stage = root.0.join("moved-validated-stage");
        let error = LocalPackageWriter::write_new_scientifically_validated_observed(
            &replaced_destination,
            fixture_input_with_scientific_id(BrickMode::PixelPresent, false, computed),
            || false,
            |event| {
                if event == PackageWriteEvent::StageStarted(PackageWriteStage::Commit) {
                    let stage = only_publication_stage(&root.0);
                    fs::rename(&stage, &moved_stage).unwrap();
                    fs::create_dir(&stage).unwrap();
                    fs::write(stage.join("replacement-marker"), b"keep").unwrap();
                }
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            PackageWriteError::ScientificPublicationTransfer(
                ScientificPublicationTransferError::Inventory(
                    crate::DirectoryInventoryError::Range(RangeReadError::RootChanged)
                )
            )
        ));
        assert!(!replaced_destination.exists());
        assert_eq!(
            fs::read(only_publication_stage(&root.0).join("replacement-marker")).unwrap(),
            b"keep"
        );
        assert!(moved_stage.is_dir());
        assert!(!moved_stage.join("m4d/profile.json").exists());
    }

    #[test]
    fn scientific_publication_transfer_consume_rejects_postpublication_drift() {
        let root = TestDirectory::new("scientific-transfer-consume-drift");
        let computed = computed_fixture_scientific_id(&root.0, "identity");

        let cancelled = root.0.join("cancelled-transfer.m4d");
        let transfer = LocalPackageWriter::write_new_scientifically_validated(
            &cancelled,
            fixture_input_with_scientific_id(BrickMode::PixelPresent, false, computed),
            || false,
        )
        .unwrap();
        assert!(matches!(
            transfer.consume(|| true),
            Err(ScientificPublicationTransferError::Cancelled)
        ));
        assert!(cancelled.join("m4d/profile.json").is_file());

        let unlisted_file = root.0.join("unlisted-file.m4d");
        let transfer = LocalPackageWriter::write_new_scientifically_validated(
            &unlisted_file,
            fixture_input_with_scientific_id(BrickMode::PixelPresent, false, computed),
            || false,
        )
        .unwrap();
        fs::write(unlisted_file.join("foreign.bin"), b"foreign").unwrap();
        assert!(matches!(
            transfer.consume(|| false),
            Err(ScientificPublicationTransferError::Inventory(
                crate::DirectoryInventoryError::UnexpectedFile { .. }
            ))
        ));

        let unlisted_directory = root.0.join("unlisted-directory.m4d");
        let transfer = LocalPackageWriter::write_new_scientifically_validated(
            &unlisted_directory,
            fixture_input_with_scientific_id(BrickMode::PixelPresent, false, computed),
            || false,
        )
        .unwrap();
        fs::create_dir(unlisted_directory.join("foreign-directory")).unwrap();
        assert!(matches!(
            transfer.consume(|| false),
            Err(ScientificPublicationTransferError::Inventory(
                crate::DirectoryInventoryError::UnexpectedDirectory { .. }
            ))
        ));

        let replaced_object = root.0.join("replaced-object.m4d");
        let transfer = LocalPackageWriter::write_new_scientifically_validated(
            &replaced_object,
            fixture_input_with_scientific_id(BrickMode::PixelPresent, false, computed),
            || false,
        )
        .unwrap();
        let display = replaced_object.join("m4d/display.json");
        let replacement = root.0.join("replacement-display.json");
        fs::write(&replacement, fs::read(&display).unwrap()).unwrap();
        fs::rename(&replacement, &display).unwrap();
        assert!(matches!(
            transfer.consume(|| false),
            Err(ScientificPublicationTransferError::Snapshot(
                PackageValidationError::Range(RangeReadError::ObjectChanged { .. })
            ))
        ));

        let replaced_root = root.0.join("replaced-published-root.m4d");
        let moved_root = root.0.join("moved-published-root");
        let transfer = LocalPackageWriter::write_new_scientifically_validated(
            &replaced_root,
            fixture_input_with_scientific_id(BrickMode::PixelPresent, false, computed),
            || false,
        )
        .unwrap();
        fs::rename(&replaced_root, &moved_root).unwrap();
        fs::create_dir(&replaced_root).unwrap();
        assert!(matches!(
            transfer.consume(|| false),
            Err(ScientificPublicationTransferError::Inventory(
                crate::DirectoryInventoryError::Range(RangeReadError::RootChanged)
            ))
        ));
        assert!(moved_root.join("m4d/profile.json").is_file());
    }

    #[test]
    fn self_consistent_commit_never_issues_after_a_postrename_root_substitution() {
        let root = TestDirectory::new("scientific-transfer-postrename-substitution");
        let computed = computed_fixture_scientific_id(&root.0, "identity");
        let destination = root.0.join("published.m4d");
        let moved_validated_root = root.0.join("moved-validated-root");

        let error = write_fixture_with_self_consistent_commit_controls(
            &destination,
            fixture_input_with_scientific_id(BrickMode::PixelPresent, false, computed),
            || false,
            |checkpoint, _| {
                if checkpoint == PublicationCheckpoint::AfterRenameBeforeParentSync {
                    fs::rename(&destination, &moved_validated_root)?;
                    fs::create_dir(&destination)?;
                    fs::write(destination.join("replacement-marker"), b"replacement")?;
                }
                Ok(())
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            PackageWriteError::CommitIndeterminate { .. }
        ));
        assert_eq!(
            fs::read(destination.join("replacement-marker")).unwrap(),
            b"replacement"
        );
        assert!(moved_validated_root.join("m4d/profile.json").is_file());
    }

    #[test]
    fn self_consistent_commit_cancellation_boundary_is_the_atomic_rename() {
        let root = TestDirectory::new("scientific-transfer-cancellation-boundary");
        let computed = computed_fixture_scientific_id(&root.0, "identity");

        let cancelled_before_rename = Cell::new(false);
        let pre_rename_destination = root.0.join("cancelled-before-rename.m4d");
        let error = write_fixture_with_self_consistent_commit_controls(
            &pre_rename_destination,
            fixture_input_with_scientific_id(BrickMode::PixelPresent, false, computed),
            || cancelled_before_rename.get(),
            |checkpoint, _| {
                if checkpoint == PublicationCheckpoint::BeforeRename {
                    cancelled_before_rename.set(true);
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert!(matches!(error, PackageWriteError::Cancelled));
        assert!(!pre_rename_destination.exists());

        let cancelled_after_rename = Cell::new(false);
        let post_rename_destination = root.0.join("cancelled-after-rename.m4d");
        let transfer = write_fixture_with_self_consistent_commit_controls(
            &post_rename_destination,
            fixture_input_with_scientific_id(BrickMode::PixelPresent, false, computed),
            || cancelled_after_rename.get(),
            |checkpoint, _| {
                if checkpoint == PublicationCheckpoint::AfterRenameBeforeParentSync {
                    cancelled_after_rename.set(true);
                }
                Ok(())
            },
        )
        .unwrap();
        assert!(cancelled_after_rename.get());
        assert!(post_rename_destination.join("m4d/profile.json").is_file());
        let (capability, _) = transfer.consume(|| false).unwrap();
        assert_eq!(capability.scientific_content_id(), computed);
        assert_eq!(capability.root_path(), post_rename_destination);
    }

    #[test]
    fn observed_scientific_writer_reports_ordered_real_stage_timings() {
        let root = TestDirectory::new("observed-scientific-writer");
        let mismatch = root.0.join("mismatch.m4d");
        let computed = match LocalPackageWriter::write_new_scientifically_validated(
            &mismatch,
            fixture_input(BrickMode::PixelPresent, false),
            || false,
        )
        .unwrap_err()
        {
            PackageWriteError::ScientificValidation(
                ScientificPackageValidationError::ScientificContentMismatch { computed, .. },
            ) => computed,
            other => panic!("unexpected staged-validation error: {other}"),
        };

        let destination = root.0.join("observed.m4d");
        let mut events = Vec::new();
        let transfer = LocalPackageWriter::write_new_scientifically_validated_observed(
            &destination,
            fixture_input_with_scientific_id(BrickMode::PixelPresent, false, computed),
            || false,
            |event| {
                if matches!(event, PackageWriteEvent::StageStarted(_)) {
                    let started = Instant::now();
                    while started.elapsed() < Duration::from_millis(1) {
                        std::hint::black_box(());
                    }
                }
                events.push(event);
            },
        )
        .unwrap();
        let receipt = transfer.receipt();

        let reads = receipt.validation_read_report();
        assert!(reads.structure_object_reads() > 0);
        assert!(reads.exact_object_reads() > 0);
        assert!(reads.scientific_object_reads() > 0);
        assert_eq!(
            reads.total_object_reads(),
            Some(
                reads.structure_object_reads()
                    + reads.exact_object_reads()
                    + reads.scientific_object_reads()
            )
        );
        assert_eq!(
            reads.total_object_reads(),
            Some(
                transfer
                    .capability
                    .catalog()
                    .reader()
                    .object_open_operations()
            ),
            "the receipt must include every strict object open through the pre-rename currentness proof"
        );
        let scientific = receipt.scientific_validation_report().unwrap();
        assert_eq!(scientific.brick_reads(), 1);
        // The one present brick consumes packed-index and pixel objects. The
        // exact scan reuses their generation-bound handles/indexes and proves
        // the complete object set again at the scan boundary.
        assert_eq!(scientific.object_reads(), 2);
        let codecs = receipt.codec_report();
        assert!(codecs.encode_calls() > 0);
        assert!(codecs.encode_time_ns() > 0);
        // Structure and exact validation each decode the two shard indexes and
        // packed-index payload; the scientific brick read decodes both indexes
        // and both present payloads.
        assert_eq!(codecs.decode_calls(), 10);
        assert!(codecs.decode_time_ns() > 0);
        assert_eq!(
            receipt.sync_calls(),
            receipt.admission().counts().directories + 2,
            "one stage-wide filesystem barrier, every package directory, and the destination parent must be counted exactly"
        );
        assert!(receipt.sync_time_ns() > 0);

        let expected = [
            PackageWriteStage::ShardPublication,
            PackageWriteStage::StagedStructureValidation,
            PackageWriteStage::StagedExactValidation,
            PackageWriteStage::StagedScientificValidation,
            PackageWriteStage::Commit,
        ];
        assert_eq!(events.len(), expected.len() * 2);
        for (pair, expected_stage) in events.chunks_exact(2).zip(expected) {
            assert_eq!(pair[0], PackageWriteEvent::StageStarted(expected_stage));
            let PackageWriteEvent::StageFinished(timing) = pair[1] else {
                panic!("a writer stage did not finish before the next stage started");
            };
            assert_eq!(timing.stage, expected_stage);
            assert!(timing.wall_time_ns >= 1_000_000);
            assert!(timing.cpu_time_ns > 0);
        }
        assert!(destination.is_dir());
        let (capability, _) = transfer.consume(|| false).unwrap();
        drop(capability);
    }

    #[test]
    fn lazy_shard_input_stops_at_the_selected_profile_bound() {
        let root = TestDirectory::new("input-bound");
        let destination = root.0.join("bounded.m4d");
        let template = fixture_input(BrickMode::AllFill, false);
        let PackageWriteInput {
            profile_kind,
            profile,
            science,
            display_defaults,
            portable_records,
            ome_images,
            arrays,
            shards: _,
        } = template;
        let pixel_path = profile.images()[0].levels()[0].pixel_path().clone();
        let limits = profile_limits(profile_kind);
        let maximum = limits.pixel_shards + limits.validity_shards + limits.packed_index_shards;
        let yielded = Rc::new(Cell::new(0_u64));
        let observed = Rc::clone(&yielded);
        let shards = std::iter::from_fn(move || {
            observed.set(observed.get() + 1);
            Some(PackageShardInput::new(
                pixel_path.clone(),
                vec![0, 0, 0, 0, 0],
                missing_chunks(ShardProfileKind::Pixel2dUint8),
            ))
        });
        let input = PackageWriteInput::new(
            profile_kind,
            profile,
            science,
            display_defaults,
            portable_records,
            ome_images,
            arrays,
            shards,
        );

        let error = LocalPackageWriter::write_new(&destination, input, || false).unwrap_err();
        assert!(matches!(
            error,
            PackageWriteError::InvalidInput {
                reason: "shard inputs exceed the selected profile bound"
            }
        ));
        assert_eq!(yielded.get(), maximum + 1);
        assert!(!destination.exists());
        assert!(!fs::read_dir(&root.0).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".mirante4d-stage-")
        }));
    }

    fn fixture_input(mode: BrickMode, reverse: bool) -> PackageWriteInput<Vec<PackageShardInput>> {
        fixture_input_with_scientific_id(mode, reverse, scientific_id())
    }

    fn write_fixture_with_self_consistent_commit_controls(
        destination: &Path,
        input: PackageWriteInput<Vec<PackageShardInput>>,
        mut is_cancelled: impl FnMut() -> bool,
        mut hook: impl FnMut(PublicationCheckpoint, PackageId) -> io::Result<()>,
    ) -> Result<PublishedScientificPackageTransfer, PackageWriteError> {
        let mut observer = |_| {};
        match LocalPackageWriter::write_new_with_validation(
            destination,
            input,
            &mut is_cancelled,
            StagedValidation::Scientific,
            &mut observer,
            &mut hook,
        )? {
            PackageWriteOutcome::Scientific(transfer) => Ok(*transfer),
            PackageWriteOutcome::Structure(_) => unreachable!("scientific writer outcome"),
        }
    }

    fn computed_fixture_scientific_id(parent: &Path, label: &str) -> ScientificContentId {
        let mismatch = parent.join(format!("{label}-mismatch.m4d"));
        match LocalPackageWriter::write_new_scientifically_validated(
            &mismatch,
            fixture_input(BrickMode::PixelPresent, false),
            || false,
        )
        .unwrap_err()
        {
            PackageWriteError::ScientificValidation(
                ScientificPackageValidationError::ScientificContentMismatch { computed, .. },
            ) => computed,
            other => panic!("unexpected staged-validation error: {other}"),
        }
    }

    fn only_publication_stage(parent: &Path) -> PathBuf {
        let stages = fs::read_dir(parent)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".mirante4d-stage-"))
            })
            .collect::<Vec<_>>();
        assert_eq!(stages.len(), 1, "expected one private publication stage");
        stages.into_iter().next().unwrap()
    }

    fn fixture_input_with_scientific_id(
        mode: BrickMode,
        reverse: bool,
        scientific_id: ScientificContentId,
    ) -> PackageWriteInput<Vec<PackageShardInput>> {
        let temporal = ScienceTemporalCalibration::regular(bits64("3ff0000000000000")).unwrap();
        let explicit = matches!(
            mode,
            BrickMode::ExplicitValidity | BrickMode::ExplicitAllInvalid
        );
        let validity_mode = if explicit {
            ProfileValidityMode::Explicit
        } else {
            ProfileValidityMode::AllValid
        };
        let level = ProfileLevel::new(0, 0, validity_mode).unwrap();
        let image = ProfileImage::new(
            0,
            vec![ProfileLogicalLayer::new(LogicalLayerKey::new(0), 0)],
            vec![level.clone()],
        )
        .unwrap();
        let profile = ProfileHeader::new(
            scientific_id,
            vec![image.clone()],
            0,
            if explicit {
                OmeInteroperabilityBase::Io1
            } else {
                OmeInteroperabilityBase::Io2
            },
        )
        .unwrap();
        let science = ScienceDescriptor::new(
            scientific_id,
            vec![
                ScienceLayer::new(
                    LogicalLayerKey::new(0),
                    Shape4D::new(1, 1, 2, 3).unwrap(),
                    IntensityDType::Uint8,
                    temporal.clone(),
                    identity_transform(),
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let display = DisplayDefaults::new(vec![
            DisplayLayerDefaults::new(
                LogicalLayerKey::new(0),
                true,
                Rgb24::parse("ffffff").unwrap(),
                F32Bits::parse("00000000").unwrap(),
                F32Bits::parse("3f800000").unwrap(),
            )
            .unwrap(),
        ])
        .unwrap();
        let ome = OmeImageGroupMetadata::new(
            &image,
            &temporal,
            vec![OmeLevelTransform::DiagonalMicrometer {
                scale_zyx: [bits64("3ff0000000000000"); 3],
                translation_zyx: [bits64("0000000000000000"); 3],
            }],
        )
        .unwrap();

        let mut arrays = vec![
            PackageArrayInput::new(
                level.pixel_path().clone(),
                ZarrArrayMetadata::new(ShardProfileKind::Pixel2dUint8, vec![1, 1, 1, 2, 3])
                    .unwrap(),
            ),
            PackageArrayInput::new(
                level.packed_index_path().clone(),
                ZarrArrayMetadata::new(ShardProfileKind::PackedIndex, vec![1, 64]).unwrap(),
            ),
        ];
        if let Some(path) = level.validity_path() {
            arrays.push(PackageArrayInput::new(
                path.clone(),
                ZarrArrayMetadata::new(ShardProfileKind::Validity2d, vec![1, 1, 1, 2, 1]).unwrap(),
            ));
        }

        let pixel_present = matches!(mode, BrickMode::PixelPresent | BrickMode::ExplicitValidity);
        let (statistics, explicit_validity) = match mode {
            BrickMode::PixelPresent => (PackedIndexStatistics::new(6, 2, Some((0, 2))), false),
            BrickMode::AllFill => (PackedIndexStatistics::new(6, 0, Some((0, 0))), false),
            BrickMode::ExplicitValidity => (PackedIndexStatistics::new(3, 2, Some((0, 2))), true),
            BrickMode::ExplicitAllInvalid => (PackedIndexStatistics::new(0, 0, None), true),
        };
        let record = PackedIndexRecord::new(
            PackedIndexCoordinates::new(0, 0, 0, 0, 0, 0, 0),
            statistics,
            pixel_present,
            explicit_validity,
            IntensityDType::Uint8,
            6,
        )
        .unwrap();

        let mut shards = Vec::new();
        if pixel_present {
            let kind = ShardProfileKind::Pixel2dUint8;
            let mut payload = vec![0; kind.decoded_inner_bytes()];
            payload[..6].copy_from_slice(&[0, 1, 2, 0, 0, 0]);
            let mut chunks = missing_chunks(kind);
            chunks[0] = Some(payload);
            shards.push(PackageShardInput::new(
                level.pixel_path().clone(),
                vec![0, 0, 0, 0, 0],
                chunks,
            ));
        }

        let packed_kind = ShardProfileKind::PackedIndex;
        let mut packed_payload = vec![0; packed_kind.decoded_inner_bytes()];
        packed_payload[..crate::PACKED_INDEX_RECORD_BYTES as usize]
            .copy_from_slice(&record.encode());
        let mut packed_chunks = missing_chunks(packed_kind);
        packed_chunks[0] = Some(packed_payload);
        shards.push(PackageShardInput::new(
            level.packed_index_path().clone(),
            vec![0, 0],
            packed_chunks,
        ));

        if matches!(mode, BrickMode::ExplicitValidity) {
            let kind = ShardProfileKind::Validity2d;
            let mut payload = vec![0; kind.decoded_inner_bytes()];
            payload[0] = 0b0000_0111;
            let mut chunks = missing_chunks(kind);
            chunks[0] = Some(payload);
            shards.push(PackageShardInput::new(
                level.validity_path().unwrap().clone(),
                vec![0, 0, 0, 0, 0],
                chunks,
            ));
        }
        if reverse {
            arrays.reverse();
            shards.reverse();
        }

        PackageWriteInput::new(
            ProfileKind::Current,
            profile,
            science,
            display,
            Vec::new(),
            vec![ome],
            arrays,
            shards,
        )
    }

    fn missing_chunks(kind: ShardProfileKind) -> Vec<Option<Vec<u8>>> {
        std::iter::repeat_with(|| None)
            .take(kind.chunks_per_shard())
            .collect()
    }

    fn scientific_id() -> ScientificContentId {
        ScientificContentId::parse(
            "m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap()
    }

    fn bits64(value: &str) -> F64Bits {
        F64Bits::parse(value).unwrap()
    }

    fn identity_transform() -> [F64Bits; 16] {
        [
            bits64("3ff0000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("3ff0000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("3ff0000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("0000000000000000"),
            bits64("3ff0000000000000"),
        ]
    }

    fn tree_bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
        fn visit(root: &Path, current: &Path, output: &mut BTreeMap<String, Vec<u8>>) {
            let mut entries = fs::read_dir(current)
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                if path.is_dir() {
                    visit(root, &path, output);
                } else {
                    let relative = path
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    output.insert(relative, fs::read(path).unwrap());
                }
            }
        }

        let mut output = BTreeMap::new();
        visit(root, root, &mut output);
        output
    }
}
