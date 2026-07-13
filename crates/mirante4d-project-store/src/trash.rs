//! Bounded quarantine of explicitly selected orphan generations.

#![cfg(target_os = "linux")]
#![cfg_attr(not(test), allow(dead_code))]

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs::File,
    io::{self, Read},
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
};

use mirante4d_identity::{ExactBytesDigest, ExactBytesHasher, Sha256Digest};
use mirante4d_project_model::ArtifactRecoverability;
use rustix::{
    fd::{AsFd, OwnedFd},
    fs::{
        AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, fstat, fsync, mkdirat, openat,
        renameat_with, statat, unlinkat,
    },
    io::Errno,
};

use crate::{
    ProjectGenerationId, ProjectStoreDiagnostics, ProjectStoreFault, ProjectStoreLimits,
    generation::{GenerationDocument, GenerationKind, PhysicalObject},
    inspection::inspect_store_graph,
    lease::{
        GcTransition, GcTransitionOccurrence, LeaseError, MaintenanceTransitionError,
        ProjectStoreLeases,
    },
    local::{LocalPublicationError, LocalStoreRoot},
};

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK);
const TRASH_OPERATION_DESCRIPTORS_MAX: usize = 6;
const STORE_DIRECTORY_DEPTH_MAX: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileFact {
    device: u64,
    inode: u64,
    byte_length: u64,
}

#[derive(Default)]
struct TrashInventory {
    generations: BTreeMap<ProjectGenerationId, FileFact>,
    objects: BTreeMap<ExactBytesDigest, FileFact>,
    directory_entries: BTreeMap<PathBuf, usize>,
    physical_entries: usize,
}

#[derive(Clone, Copy)]
enum FileAction {
    Move,
    RemoveActiveDuplicate,
    AlreadyQuarantined,
}

struct PlannedFile {
    active_path: PathBuf,
    trash_path: PathBuf,
    active_fact: Option<FileFact>,
    trash_fact: Option<FileFact>,
    byte_length: u64,
    action: FileAction,
}

enum Step {
    CreateDirectory(PathBuf),
    File(PlannedFile),
    SynchronizeRetry {
        active_path: PathBuf,
        trash_path: PathBuf,
        trash_fact: FileFact,
    },
}

impl Step {
    const fn namespace_mutations(&self) -> usize {
        match self {
            Self::SynchronizeRetry { .. } => 0,
            Self::CreateDirectory(_) | Self::File(_) => 1,
        }
    }

    const fn checked_bytes(&self) -> u64 {
        match self {
            Self::CreateDirectory(_) | Self::SynchronizeRetry { .. } => 0,
            Self::File(file) => file.byte_length,
        }
    }
}

struct Batch {
    steps: Vec<Step>,
}

#[derive(Clone, Copy)]
struct PurgeFile<I> {
    identity: I,
    fact: FileFact,
}

struct PurgePlan {
    objects: Vec<PurgeFile<ExactBytesDigest>>,
    generations: Vec<PurgeFile<ProjectGenerationId>>,
    object_fanouts: BTreeSet<PathBuf>,
    generation_fanouts: BTreeSet<PathBuf>,
    generation_facts: BTreeMap<ProjectGenerationId, FileFact>,
    recognized_object_prefix: bool,
    streamed_bytes: u64,
}

pub(crate) fn purge_trash<C>(
    root: &LocalStoreRoot,
    leases: &mut ProjectStoreLeases,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<ProjectStoreDiagnostics, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let result =
        leases.with_exclusive_maintenance(root, &mut is_cancelled, |leases, is_cancelled| {
            purge_exclusive(root, leases, limits, is_cancelled)
        });
    match result {
        Ok(result) => {
            if matches!(result, Err(ProjectStoreFault::CommitIndeterminate)) {
                leases.suspend_writes();
            }
            result
        }
        Err(MaintenanceTransitionError::ReadOnly) => Err(ProjectStoreFault::ReadOnly),
        Err(MaintenanceTransitionError::Cancelled) => Err(ProjectStoreFault::Cancelled),
        Err(MaintenanceTransitionError::Lease(LeaseError::Indeterminate))
        | Err(MaintenanceTransitionError::MaintenanceLost { .. })
        | Err(MaintenanceTransitionError::MaintenanceRestoredIndeterminate { .. }) => {
            leases.suspend_writes();
            Err(ProjectStoreFault::CommitIndeterminate)
        }
        Err(MaintenanceTransitionError::Lease(_)) => Err(ProjectStoreFault::Corruption {
            stage: "purge_maintenance_lease",
        }),
    }
}

fn purge_exclusive<C>(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<ProjectStoreDiagnostics, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let limits = limits.validate()?;
    if limits.open_file_descriptors_max < TRASH_OPERATION_DESCRIPTORS_MAX {
        return Err(ProjectStoreFault::Capacity {
            stage: "purge_descriptors",
        });
    }
    check_cancelled(is_cancelled)?;
    let plan = build_purge_plan(root, limits, is_cancelled)?;
    execute_purge_plan(root, leases, limits, is_cancelled, plan)
}

fn build_purge_plan<C>(
    root: &LocalStoreRoot,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<PurgePlan, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let graph = inspect_store_graph(root, limits, &mut *is_cancelled)?;
    let inventory = scan_strict_store(root, limits, is_cancelled)?;
    if graph
        .generation_ids()
        .len()
        .checked_add(inventory.generations.len())
        .is_none_or(|count| count > limits.generations_scanned_max)
    {
        return Err(ProjectStoreFault::Capacity {
            stage: "purge_generations",
        });
    }

    let active_generation_ids = graph
        .generation_ids()
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if inventory
        .generations
        .keys()
        .any(|generation_id| active_generation_ids.contains(generation_id))
    {
        return Err(ProjectStoreFault::SourceChanged);
    }
    let active_objects = graph
        .object_facts()
        .iter()
        .map(|object| (object.digest(), object.byte_length()))
        .collect::<BTreeMap<_, _>>();
    let mut active_closure = BTreeMap::<ExactBytesDigest, u64>::new();
    let mut provenance = BTreeMap::new();
    for generation_id in graph.generation_ids() {
        check_cancelled(is_cancelled)?;
        let (_, document) = read_active_generation(
            root,
            *generation_id,
            graph.state().project_id(),
            limits,
            is_cancelled,
        )?;
        record_generation_provenance(&mut provenance, *generation_id, &document);
        record_purge_closure(
            &mut active_closure,
            document.reachable_objects().iter().copied(),
        )?;
    }

    let mut trash_closure = BTreeMap::<ExactBytesDigest, u64>::new();
    let mut streamed_bytes = 0_u64;
    for (generation_id, fact) in &inventory.generations {
        check_cancelled(is_cancelled)?;
        let bytes = read_stable_file(
            root,
            &trash_generation_path(*generation_id),
            *fact,
            limits.generation_bytes_max,
            limits.stream_buffer_bytes_max,
            is_cancelled,
        )?;
        let document =
            GenerationDocument::decode(*generation_id, graph.state().project_id(), &bytes, limits)
                .map_err(|_| ProjectStoreFault::Corruption {
                    stage: "purge_generation",
                })?;
        validate_purge_generation_lineage(graph.state().authority_generation(), &document)?;
        require_no_non_regenerable(&document)?;
        record_generation_provenance(&mut provenance, *generation_id, &document);
        record_purge_closure(
            &mut trash_closure,
            document.reachable_objects().iter().copied(),
        )?;
        streamed_bytes =
            streamed_bytes
                .checked_add(fact.byte_length)
                .ok_or(ProjectStoreFault::Capacity {
                    stage: "purge_streamed_bytes",
                })?;
    }
    validate_combined_generation_provenance(&provenance)?;

    for (digest, fact) in &inventory.objects {
        if trash_closure.get(digest) != Some(&fact.byte_length) {
            return Err(ProjectStoreFault::Corruption {
                stage: "purge_unreferenced_object",
            });
        }
        hash_exact_file(
            root,
            &trash_object_path(*digest),
            *fact,
            *digest,
            limits,
            is_cancelled,
        )?;
        streamed_bytes =
            streamed_bytes
                .checked_add(fact.byte_length)
                .ok_or(ProjectStoreFault::Capacity {
                    stage: "purge_streamed_bytes",
                })?;
    }

    let mut objects = Vec::new();
    let mut missing_prefix = true;
    let mut recognized_object_prefix = false;
    for (digest, byte_length) in trash_closure {
        check_cancelled(is_cancelled)?;
        let active_present = active_objects.get(&digest).copied();
        let retained = active_closure.get(&digest) == Some(&byte_length);
        let trash_fact = inventory.objects.get(&digest).copied();
        if active_present.is_some_and(|length| length != byte_length) {
            return Err(ProjectStoreFault::Corruption {
                stage: "purge_active_object",
            });
        }
        match (active_present, retained, trash_fact) {
            (Some(_), false, _) => return Err(ProjectStoreFault::SourceChanged),
            (Some(_), true, None) => {}
            (_, _, Some(fact)) => {
                missing_prefix = false;
                objects.push(PurgeFile {
                    identity: digest,
                    fact,
                });
            }
            (None, _, None) if missing_prefix => recognized_object_prefix = true,
            (None, _, None) => return Err(ProjectStoreFault::SourceChanged),
        }
    }

    let object_fanouts = purge_fanout_directories(&inventory, false);
    let generation_fanouts = purge_fanout_directories(&inventory, true);
    let generations = inventory
        .generations
        .iter()
        .map(|(identity, fact)| PurgeFile {
            identity: *identity,
            fact: *fact,
        })
        .collect();
    Ok(PurgePlan {
        objects,
        generations,
        object_fanouts,
        generation_fanouts,
        generation_facts: inventory.generations,
        recognized_object_prefix,
        streamed_bytes,
    })
}

fn validate_purge_generation_lineage(
    authority: &GenerationDocument,
    document: &GenerationDocument,
) -> Result<(), ProjectStoreFault> {
    if document.forked_from() != authority.forked_from()
        || !document
            .projection()
            .state()
            .dataset()
            .has_same_scientific_content(authority.projection().state().dataset())
    {
        Err(ProjectStoreFault::Corruption {
            stage: "purge_generation_lineage",
        })
    } else {
        Ok(())
    }
}

fn record_purge_closure(
    closure: &mut BTreeMap<ExactBytesDigest, u64>,
    objects: impl Iterator<Item = PhysicalObject>,
) -> Result<(), ProjectStoreFault> {
    for object in objects {
        match closure.insert(object.digest(), object.byte_length()) {
            Some(length) if length != object.byte_length() => {
                return Err(ProjectStoreFault::Corruption {
                    stage: "purge_object_closure",
                });
            }
            _ => {}
        }
    }
    Ok(())
}

fn purge_fanout_directories(inventory: &TrashInventory, generation: bool) -> BTreeSet<PathBuf> {
    let namespace = if generation { "generations" } else { "objects" };
    let prefix = PathBuf::from("trash").join(namespace).join("sha256");
    inventory
        .directory_entries
        .keys()
        .filter(|path| path.parent() == Some(prefix.as_path()))
        .cloned()
        .collect()
}

fn execute_purge_plan<C>(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
    plan: PurgePlan,
) -> Result<ProjectStoreDiagnostics, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let PurgePlan {
        objects,
        generations,
        object_fanouts,
        generation_fanouts,
        generation_facts,
        recognized_object_prefix,
        streamed_bytes,
    } = plan;
    let mut operation_mutated = false;
    let mut published_objects = 0_u64;
    let expected_object_facts = objects
        .iter()
        .map(|file| (file.identity, file.fact))
        .collect::<BTreeMap<_, _>>();
    let object_batches = purge_batches(objects, limits, "purge_object_batch")?;
    let generation_batches = purge_batches(generations, limits, "purge_generation_batch")?;

    if recognized_object_prefix {
        purge_sync_sweep(root, leases, &object_fanouts)?;
        let inventory = scan_strict_store_deferred(root, limits)?;
        if inventory.generations != generation_facts {
            return Err(ProjectStoreFault::SourceChanged);
        }
        if inventory.objects != expected_object_facts {
            return Err(ProjectStoreFault::SourceChanged);
        }
    }

    let mut streamed_bytes = streamed_bytes;
    for batch in object_batches {
        check_cancelled(is_cancelled)?;
        let (removed, streamed) =
            execute_purge_batch(root, leases, batch, limits, &mut operation_mutated)?;
        published_objects =
            published_objects
                .checked_add(removed)
                .ok_or(ProjectStoreFault::Capacity {
                    stage: "purge_published_objects",
                })?;
        streamed_bytes =
            streamed_bytes
                .checked_add(streamed)
                .ok_or(ProjectStoreFault::Capacity {
                    stage: "purge_streamed_bytes",
                })?;
    }

    // This is the object-phase durability barrier. Cancellation is deferred
    // from the sweep's first sync through the complete strict re-inventory.
    purge_sync_sweep(root, leases, &object_fanouts)?;
    let object_barrier = scan_strict_store_deferred(root, limits).map_err(|error| {
        if operation_mutated {
            ProjectStoreFault::CommitIndeterminate
        } else {
            error
        }
    })?;
    if !object_barrier.objects.is_empty() || object_barrier.generations != generation_facts {
        return Err(if operation_mutated {
            ProjectStoreFault::CommitIndeterminate
        } else {
            ProjectStoreFault::SourceChanged
        });
    }

    // Always perform the generation recovery sweep before observing the next
    // cancellation boundary. It makes a prior process's generation-unlink
    // prefix durable even though absent generation records cannot prove order.
    purge_sync_sweep(root, leases, &generation_fanouts)?;
    let generation_barrier = scan_strict_store_deferred(root, limits).map_err(|error| {
        if operation_mutated {
            ProjectStoreFault::CommitIndeterminate
        } else {
            error
        }
    })?;
    if !generation_barrier.objects.is_empty() || generation_barrier.generations != generation_facts
    {
        return Err(if operation_mutated {
            ProjectStoreFault::CommitIndeterminate
        } else {
            ProjectStoreFault::SourceChanged
        });
    }

    for batch in generation_batches {
        check_cancelled(is_cancelled)?;
        let (removed, streamed) =
            execute_purge_batch(root, leases, batch, limits, &mut operation_mutated)?;
        published_objects =
            published_objects
                .checked_add(removed)
                .ok_or(ProjectStoreFault::Capacity {
                    stage: "purge_published_objects",
                })?;
        streamed_bytes =
            streamed_bytes
                .checked_add(streamed)
                .ok_or(ProjectStoreFault::Capacity {
                    stage: "purge_streamed_bytes",
                })?;
    }
    let final_inventory = scan_strict_store_deferred(root, limits).map_err(|error| {
        if operation_mutated {
            ProjectStoreFault::CommitIndeterminate
        } else {
            error
        }
    })?;
    if !final_inventory.objects.is_empty() || !final_inventory.generations.is_empty() {
        return Err(if operation_mutated {
            ProjectStoreFault::CommitIndeterminate
        } else {
            ProjectStoreFault::SourceChanged
        });
    }
    Ok(ProjectStoreDiagnostics {
        streamed_bytes,
        published_objects,
        ..ProjectStoreDiagnostics::default()
    })
}

fn purge_batches<I: PurgeIdentity>(
    files: Vec<PurgeFile<I>>,
    limits: ProjectStoreLimits,
    stage: &'static str,
) -> Result<Vec<Vec<PurgeFile<I>>>, ProjectStoreFault> {
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut bytes = 0_u64;
    let mut parent = None::<PathBuf>;
    for file in files {
        if file.fact.byte_length > limits.gc_batch_bytes_max {
            return Err(ProjectStoreFault::Capacity { stage });
        }
        let next_bytes = bytes
            .checked_add(file.fact.byte_length)
            .ok_or(ProjectStoreFault::Capacity { stage })?;
        let file_parent = file
            .identity
            .path()
            .parent()
            .expect("a purge file has a containing directory")
            .to_path_buf();
        if !current.is_empty()
            && (parent.as_ref() != Some(&file_parent)
                || current.len() >= limits.gc_batch_entries_max
                || next_bytes > limits.gc_batch_bytes_max)
        {
            batches.push(current);
            current = Vec::new();
            bytes = 0;
        }
        if current.is_empty() {
            parent = Some(file_parent);
        }
        bytes = bytes
            .checked_add(file.fact.byte_length)
            .ok_or(ProjectStoreFault::Capacity { stage })?;
        current.push(file);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    Ok(batches)
}

trait PurgeIdentity: Copy {
    fn path(self) -> PathBuf;

    fn validate_held(
        self,
        file: &mut File,
        expected: FileFact,
        limits: ProjectStoreLimits,
    ) -> Result<u64, ProjectStoreFault>;
}

impl PurgeIdentity for ExactBytesDigest {
    fn path(self) -> PathBuf {
        trash_object_path(self)
    }

    fn validate_held(
        self,
        file: &mut File,
        expected: FileFact,
        limits: ProjectStoreLimits,
    ) -> Result<u64, ProjectStoreFault> {
        if expected.byte_length > limits.object_or_page_bytes_max {
            return Err(ProjectStoreFault::Capacity {
                stage: "purge_object_bytes",
            });
        }
        let mut hasher = ExactBytesHasher::new();
        let observed = stream_held_file(file, expected, limits.stream_buffer_bytes_max, |bytes| {
            hasher
                .update(bytes)
                .map_err(|_| ProjectStoreFault::Corruption {
                    stage: "purge_object",
                })
        })?;
        if hasher
            .finalize()
            .map_err(|_| ProjectStoreFault::Corruption {
                stage: "purge_object",
            })?
            .digest()
            != self
        {
            return Err(ProjectStoreFault::Corruption {
                stage: "purge_object",
            });
        }
        Ok(observed)
    }
}

impl PurgeIdentity for ProjectGenerationId {
    fn path(self) -> PathBuf {
        trash_generation_path(self)
    }

    fn validate_held(
        self,
        file: &mut File,
        expected: FileFact,
        limits: ProjectStoreLimits,
    ) -> Result<u64, ProjectStoreFault> {
        if expected.byte_length > limits.generation_bytes_max {
            return Err(ProjectStoreFault::Capacity {
                stage: "purge_generation_bytes",
            });
        }
        let capacity =
            usize::try_from(expected.byte_length).map_err(|_| ProjectStoreFault::Capacity {
                stage: "purge_generation_bytes",
            })?;
        let mut bytes = Vec::with_capacity(capacity);
        let observed = stream_held_file(file, expected, limits.stream_buffer_bytes_max, |chunk| {
            bytes.extend_from_slice(chunk);
            Ok(())
        })?;
        if crate::wire::framed_generation_id(&bytes).ok() != Some(self) {
            return Err(ProjectStoreFault::Corruption {
                stage: "purge_generation",
            });
        }
        Ok(observed)
    }
}

fn execute_purge_batch<I: PurgeIdentity>(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    batch: Vec<PurgeFile<I>>,
    limits: ProjectStoreLimits,
    operation_mutated: &mut bool,
) -> Result<(u64, u64), ProjectStoreFault> {
    let first_path = batch
        .first()
        .expect("a purge batch is nonempty")
        .identity
        .path();
    let parent_path = first_path
        .parent()
        .expect("a purge file has a containing directory");
    let parent = open_directory_path(root.descriptor(), parent_path).map_err(|_| {
        if *operation_mutated {
            ProjectStoreFault::CommitIndeterminate
        } else {
            ProjectStoreFault::SourceChanged
        }
    })?;
    let mut removed = 0_u64;
    let mut streamed = 0_u64;
    for file in batch {
        let path = file.identity.path();
        if path.parent() != Some(parent_path) {
            return Err(if *operation_mutated {
                ProjectStoreFault::CommitIndeterminate
            } else {
                ProjectStoreFault::Corruption {
                    stage: "purge_batch_parent",
                }
            });
        }
        let result = unlink_purge_file(
            leases,
            &parent,
            path.file_name().expect("a purge file has a name"),
            file.identity,
            file.fact,
            limits,
            operation_mutated,
        );
        match result {
            Ok(file_streamed) => {
                removed = removed.checked_add(1).ok_or(ProjectStoreFault::Capacity {
                    stage: "purge_published_objects",
                })?;
                streamed =
                    streamed
                        .checked_add(file_streamed)
                        .ok_or(ProjectStoreFault::Capacity {
                            stage: "purge_streamed_bytes",
                        })?;
            }
            Err(error) => {
                return Err(if *operation_mutated {
                    ProjectStoreFault::CommitIndeterminate
                } else {
                    error
                });
            }
        }
    }
    if purge_sync_held_directory(leases, &parent).is_err() {
        return Err(ProjectStoreFault::CommitIndeterminate);
    }
    Ok((removed, streamed))
}

fn unlink_purge_file<I: PurgeIdentity>(
    leases: &ProjectStoreLeases,
    parent: &OwnedFd,
    name: &OsStr,
    identity: I,
    expected: FileFact,
    limits: ProjectStoreLimits,
    operation_mutated: &mut bool,
) -> Result<u64, ProjectStoreFault> {
    let named = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| ProjectStoreFault::SourceChanged)?;
    if stat_file_fact(&named, "purge_file")? != expected {
        return Err(ProjectStoreFault::SourceChanged);
    }
    let held = openat(parent, name, FILE_FLAGS, Mode::empty())
        .map_err(|_| ProjectStoreFault::SourceChanged)?;
    if opened_file_fact(&held, "purge_file")? != expected {
        return Err(ProjectStoreFault::SourceChanged);
    }
    let mut held = File::from(held);
    let streamed = identity.validate_held(&mut held, expected, limits)?;
    let named = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| ProjectStoreFault::SourceChanged)?;
    if stat_file_fact(&named, "purge_file")? != expected
        || opened_file_fact(&held, "purge_file")? != expected
    {
        return Err(ProjectStoreFault::SourceChanged);
    }
    let occurrence = purge_transition_before(leases, GcTransition::PurgeRemove)?;
    unlinkat(parent, name, AtFlags::empty()).map_err(|_| ProjectStoreFault::SourceChanged)?;
    *operation_mutated = true;
    purge_transition_after(leases, occurrence)?;
    Ok(streamed)
}

fn stream_held_file<F>(
    file: &mut File,
    expected: FileFact,
    buffer_bytes: usize,
    mut consume: F,
) -> Result<u64, ProjectStoreFault>
where
    F: FnMut(&[u8]) -> Result<(), ProjectStoreFault>,
{
    let mut observed = 0_u64;
    let mut buffer = vec![0_u8; buffer_bytes];
    loop {
        let remaining = expected.byte_length.saturating_sub(observed);
        let read_limit = if remaining == 0 {
            1
        } else {
            usize::try_from(remaining)
                .unwrap_or(usize::MAX)
                .min(buffer.len())
        };
        let read = match file.read(&mut buffer[..read_limit]) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                return Err(ProjectStoreFault::Corruption {
                    stage: "purge_file",
                });
            }
        };
        observed = observed
            .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
            .ok_or(ProjectStoreFault::Capacity {
                stage: "purge_streamed_bytes",
            })?;
        if observed > expected.byte_length {
            return Err(ProjectStoreFault::SourceChanged);
        }
        consume(&buffer[..read])?;
    }
    if observed != expected.byte_length {
        return Err(ProjectStoreFault::SourceChanged);
    }
    Ok(observed)
}

fn purge_sync_sweep(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    directories: &BTreeSet<PathBuf>,
) -> Result<(), ProjectStoreFault> {
    for directory in directories {
        purge_sync_directory(root, leases, directory)
            .map_err(|_| ProjectStoreFault::CommitIndeterminate)?;
    }
    Ok(())
}

fn purge_sync_directory(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    path: &Path,
) -> Result<(), ProjectStoreFault> {
    let occurrence = purge_transition_before(leases, GcTransition::PurgeDirectorySync)?;
    sync_directory(root, path)?;
    purge_transition_after(leases, occurrence)
}

fn purge_sync_held_directory(
    leases: &ProjectStoreLeases,
    directory: &OwnedFd,
) -> Result<(), ProjectStoreFault> {
    let occurrence = purge_transition_before(leases, GcTransition::PurgeDirectorySync)?;
    sync_fd(directory).map_err(|_| ProjectStoreFault::CommitIndeterminate)?;
    purge_transition_after(leases, occurrence)
}

fn purge_transition_before(
    leases: &ProjectStoreLeases,
    transition: GcTransition,
) -> Result<GcTransitionOccurrence, ProjectStoreFault> {
    leases
        .gc_transition_before(transition)
        .map_err(|_| ProjectStoreFault::Corruption {
            stage: "purge_transition_injected",
        })
}

fn purge_transition_after(
    leases: &ProjectStoreLeases,
    occurrence: GcTransitionOccurrence,
) -> Result<(), ProjectStoreFault> {
    leases
        .gc_transition_after(occurrence)
        .map_err(|_| ProjectStoreFault::Corruption {
            stage: "purge_transition_injected",
        })
}

fn scan_strict_store_deferred(
    root: &LocalStoreRoot,
    limits: ProjectStoreLimits,
) -> Result<TrashInventory, ProjectStoreFault> {
    scan_strict_store(root, limits, &mut || false)
}

pub(crate) fn trash_generations<C>(
    root: &LocalStoreRoot,
    leases: &mut ProjectStoreLeases,
    selected: &[ProjectGenerationId],
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<ProjectStoreDiagnostics, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let result =
        leases.with_exclusive_maintenance(root, &mut is_cancelled, |leases, is_cancelled| {
            trash_exclusive(root, leases, selected, limits, is_cancelled)
        });
    match result {
        Ok(result) => {
            if matches!(result, Err(ProjectStoreFault::CommitIndeterminate)) {
                leases.suspend_writes();
            }
            result
        }
        Err(MaintenanceTransitionError::ReadOnly) => Err(ProjectStoreFault::ReadOnly),
        Err(MaintenanceTransitionError::Cancelled) => Err(ProjectStoreFault::Cancelled),
        Err(MaintenanceTransitionError::Lease(LeaseError::Indeterminate))
        | Err(MaintenanceTransitionError::MaintenanceLost { .. })
        | Err(MaintenanceTransitionError::MaintenanceRestoredIndeterminate { .. }) => {
            leases.suspend_writes();
            Err(ProjectStoreFault::CommitIndeterminate)
        }
        Err(MaintenanceTransitionError::Lease(_)) => Err(ProjectStoreFault::Corruption {
            stage: "trash_maintenance_lease",
        }),
    }
}

fn trash_exclusive<C>(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    selected: &[ProjectGenerationId],
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<ProjectStoreDiagnostics, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let limits = limits.validate()?;
    if limits.open_file_descriptors_max < TRASH_OPERATION_DESCRIPTORS_MAX {
        return Err(ProjectStoreFault::Capacity {
            stage: "trash_descriptors",
        });
    }
    check_cancelled(is_cancelled)?;
    if selected.is_empty() {
        return Err(ProjectStoreFault::Corruption {
            stage: "trash_selection",
        });
    }
    if selected.len() > limits.recovery_candidates_max {
        return Err(ProjectStoreFault::Capacity {
            stage: "trash_selection",
        });
    }
    let selected_count = selected.len();
    let selected = selected.iter().copied().collect::<BTreeSet<_>>();
    if selected.len() != selected_count {
        return Err(ProjectStoreFault::Corruption {
            stage: "trash_selection",
        });
    }

    let root_scan = transition_before(leases, GcTransition::RootScan)?;
    let graph = inspect_store_graph(root, limits, &mut *is_cancelled)?;
    let mut inventory = scan_strict_store(root, limits, is_cancelled)?;
    transition_after(leases, root_scan)?;
    if graph
        .generation_ids()
        .len()
        .checked_add(inventory.generations.len())
        .is_none_or(|count| count > limits.generations_scanned_max)
    {
        return Err(ProjectStoreFault::Capacity {
            stage: "trash_generations",
        });
    }

    let candidate_listing = transition_before(leases, GcTransition::CandidateListing)?;
    let active_generations = graph
        .generation_ids()
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let active_orphans = graph
        .orphan_generation_ids()
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let active_objects = graph
        .object_facts()
        .iter()
        .map(|object| (object.digest(), object.byte_length()))
        .collect::<BTreeMap<_, _>>();
    let mut available_objects = active_objects.clone();
    for (digest, fact) in &inventory.objects {
        if available_objects
            .insert(*digest, fact.byte_length)
            .is_some_and(|length| length != fact.byte_length)
        {
            return Err(ProjectStoreFault::Corruption {
                stage: "trash_object_namespace",
            });
        }
    }

    let mut selected_closure = BTreeSet::<PhysicalObject>::new();
    let mut retained_closure = BTreeSet::<PhysicalObject>::new();
    let mut generation_plans = Vec::new();
    let mut provenance = BTreeMap::new();
    let mut streamed_bytes = 0_u64;
    for generation_id in graph.generation_ids() {
        check_cancelled(is_cancelled)?;
        let (active_bytes, document) = read_active_generation(
            root,
            *generation_id,
            graph.state().project_id(),
            limits,
            is_cancelled,
        )?;
        record_generation_provenance(&mut provenance, *generation_id, &document);
        if let Some(trash_fact) = inventory.generations.get(generation_id).copied() {
            let trash_bytes = read_stable_file(
                root,
                &trash_generation_path(*generation_id),
                trash_fact,
                limits.generation_bytes_max,
                limits.stream_buffer_bytes_max,
                is_cancelled,
            )?;
            if active_bytes != trash_bytes {
                return Err(ProjectStoreFault::Corruption {
                    stage: "trash_generation_collision",
                });
            }
            streamed_bytes = streamed_bytes
                .checked_add(checked_collision_bytes(&active_bytes, &trash_bytes)?)
                .ok_or(ProjectStoreFault::Capacity {
                    stage: "trash_streamed_bytes",
                })?;
        }
        if selected.contains(generation_id) {
            if !active_orphans.contains(generation_id) {
                return Err(ProjectStoreFault::SourceChanged);
            }
            require_no_non_regenerable(&document)?;
            selected_closure.extend(document.reachable_objects());
            generation_plans.push(plan_generation_file(
                root,
                *generation_id,
                inventory.generations.get(generation_id).copied(),
            )?);
        } else {
            retained_closure.extend(document.reachable_objects());
        }
    }

    if selected
        .difference(&active_generations)
        .any(|generation_id| !inventory.generations.contains_key(generation_id))
    {
        return Err(ProjectStoreFault::SourceChanged);
    }
    for (generation_id, trash_fact) in &inventory.generations {
        if active_generations.contains(generation_id) {
            continue;
        }
        check_cancelled(is_cancelled)?;
        let trash_bytes = read_stable_file(
            root,
            &trash_generation_path(*generation_id),
            *trash_fact,
            limits.generation_bytes_max,
            limits.stream_buffer_bytes_max,
            is_cancelled,
        )?;
        let document = GenerationDocument::decode(
            *generation_id,
            graph.state().project_id(),
            &trash_bytes,
            limits,
        )
        .map_err(|_| ProjectStoreFault::Corruption {
            stage: "trash_generation",
        })?;
        validate_trash_generation(
            graph.state().authority_generation(),
            &document,
            &available_objects,
        )?;
        record_generation_provenance(&mut provenance, *generation_id, &document);
        if selected.contains(generation_id) {
            require_no_non_regenerable(&document)?;
            selected_closure.extend(document.reachable_objects());
            generation_plans.push(PlannedFile {
                active_path: active_generation_path(*generation_id),
                trash_path: trash_generation_path(*generation_id),
                active_fact: None,
                trash_fact: Some(*trash_fact),
                byte_length: trash_fact.byte_length,
                action: FileAction::AlreadyQuarantined,
            });
        }
    }
    validate_combined_generation_provenance(&provenance)?;

    let movable_objects = selected_closure
        .difference(&retained_closure)
        .copied()
        .collect::<Vec<_>>();
    let mut object_plans = Vec::with_capacity(movable_objects.len());
    for object in movable_objects {
        check_cancelled(is_cancelled)?;
        let active_fact = active_objects
            .get(&object.digest())
            .copied()
            .map(|_| file_fact(root, &active_object_path(object.digest())))
            .transpose()?
            .flatten();
        let trash_fact = inventory.objects.get(&object.digest()).copied();
        if let Some(fact) = active_fact
            && fact.byte_length != object.byte_length()
        {
            return Err(ProjectStoreFault::Corruption {
                stage: "trash_object",
            });
        }
        if let Some(fact) = trash_fact {
            hash_exact_file(
                root,
                &trash_object_path(object.digest()),
                fact,
                object.digest(),
                limits,
                is_cancelled,
            )?;
            streamed_bytes = streamed_bytes.checked_add(fact.byte_length).ok_or(
                ProjectStoreFault::Capacity {
                    stage: "trash_streamed_bytes",
                },
            )?;
        }
        object_plans.push(plan_file(
            active_object_path(object.digest()),
            trash_object_path(object.digest()),
            active_fact,
            trash_fact,
            object.byte_length(),
        )?);
    }

    generation_plans.sort_unstable_by(|left, right| left.active_path.cmp(&right.active_path));
    object_plans.sort_unstable_by(|left, right| left.active_path.cmp(&right.active_path));
    let plans = generation_plans
        .into_iter()
        .chain(object_plans)
        .collect::<Vec<_>>();
    revalidate_plans(root, &plans)?;

    let steps = build_steps(root, &plans, &mut inventory, limits)?;
    let batches = build_batches(steps, limits)?;
    transition_after(leases, candidate_listing)?;
    execute_batches(root, leases, batches, limits, is_cancelled, streamed_bytes)
}

fn transition_before(
    leases: &ProjectStoreLeases,
    transition: GcTransition,
) -> Result<GcTransitionOccurrence, ProjectStoreFault> {
    leases
        .gc_transition_before(transition)
        .map_err(|_| ProjectStoreFault::Corruption {
            stage: "trash_transition_injected",
        })
}

fn transition_after(
    leases: &ProjectStoreLeases,
    occurrence: GcTransitionOccurrence,
) -> Result<(), ProjectStoreFault> {
    leases
        .gc_transition_after(occurrence)
        .map_err(|_| ProjectStoreFault::Corruption {
            stage: "trash_transition_injected",
        })
}

fn require_no_non_regenerable(document: &GenerationDocument) -> Result<(), ProjectStoreFault> {
    if document
        .projection()
        .state()
        .artifacts()
        .iter()
        .any(|artifact| artifact.recoverability() == ArtifactRecoverability::NonRegenerable)
    {
        Err(ProjectStoreFault::ConfirmationRequired)
    } else {
        Ok(())
    }
}

fn read_active_generation<C>(
    root: &LocalStoreRoot,
    generation_id: ProjectGenerationId,
    project_id: mirante4d_project_model::ProjectId,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<(Vec<u8>, GenerationDocument), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let bytes = root
        .read_generation_bytes(
            generation_id,
            limits.generation_bytes_max,
            &mut *is_cancelled,
        )
        .map_err(|error| map_local_error(error, "trash_generation"))?;
    let document =
        GenerationDocument::decode(generation_id, project_id, &bytes, limits).map_err(|_| {
            ProjectStoreFault::Corruption {
                stage: "trash_generation",
            }
        })?;
    Ok((bytes, document))
}

fn checked_collision_bytes(active: &[u8], trash: &[u8]) -> Result<u64, ProjectStoreFault> {
    active
        .len()
        .checked_add(trash.len())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or(ProjectStoreFault::Capacity {
            stage: "trash_streamed_bytes",
        })
}

fn validate_trash_generation(
    authority: &GenerationDocument,
    document: &GenerationDocument,
    available_objects: &BTreeMap<ExactBytesDigest, u64>,
) -> Result<(), ProjectStoreFault> {
    if document.forked_from() != authority.forked_from()
        || !document
            .projection()
            .state()
            .dataset()
            .has_same_scientific_content(authority.projection().state().dataset())
    {
        return Err(ProjectStoreFault::Corruption {
            stage: "trash_generation_lineage",
        });
    }
    for object in document.reachable_objects() {
        if available_objects.get(&object.digest()) != Some(&object.byte_length()) {
            return Err(ProjectStoreFault::Corruption {
                stage: "trash_object_closure",
            });
        }
    }
    Ok(())
}

fn record_generation_provenance(
    provenance: &mut BTreeMap<
        ProjectGenerationId,
        (
            GenerationKind,
            Option<ProjectGenerationId>,
            Option<ProjectGenerationId>,
        ),
    >,
    generation_id: ProjectGenerationId,
    document: &GenerationDocument,
) {
    provenance.insert(
        generation_id,
        (
            document.kind(),
            document.parent_generation_id(),
            document.base_manual_generation_id(),
        ),
    );
}

fn validate_combined_generation_provenance(
    provenance: &BTreeMap<
        ProjectGenerationId,
        (
            GenerationKind,
            Option<ProjectGenerationId>,
            Option<ProjectGenerationId>,
        ),
    >,
) -> Result<(), ProjectStoreFault> {
    for (kind, parent, base) in provenance.values() {
        if parent.is_some_and(|parent| {
            provenance
                .get(&parent)
                .is_some_and(|(parent_kind, _, _)| parent_kind != kind)
        }) || base.is_some_and(|base| {
            provenance
                .get(&base)
                .is_some_and(|(base_kind, _, _)| *base_kind != GenerationKind::Manual)
        }) {
            return Err(ProjectStoreFault::Corruption {
                stage: "trash_generation_provenance",
            });
        }
    }
    Ok(())
}

fn plan_generation_file(
    root: &LocalStoreRoot,
    generation_id: ProjectGenerationId,
    trash_fact: Option<FileFact>,
) -> Result<PlannedFile, ProjectStoreFault> {
    let active_path = active_generation_path(generation_id);
    let active_fact = file_fact(root, &active_path)?.ok_or(ProjectStoreFault::SourceChanged)?;
    plan_file(
        active_path,
        trash_generation_path(generation_id),
        Some(active_fact),
        trash_fact,
        active_fact.byte_length,
    )
}

fn plan_file(
    active_path: PathBuf,
    trash_path: PathBuf,
    active_fact: Option<FileFact>,
    trash_fact: Option<FileFact>,
    byte_length: u64,
) -> Result<PlannedFile, ProjectStoreFault> {
    let action = match (active_fact, trash_fact) {
        (Some(active), None) if active.byte_length == byte_length => FileAction::Move,
        (None, Some(trash)) if trash.byte_length == byte_length => FileAction::AlreadyQuarantined,
        (Some(active), Some(trash))
            if active.byte_length == byte_length && trash.byte_length == byte_length =>
        {
            FileAction::RemoveActiveDuplicate
        }
        _ => return Err(ProjectStoreFault::SourceChanged),
    };
    Ok(PlannedFile {
        active_path,
        trash_path,
        active_fact,
        trash_fact,
        byte_length,
        action,
    })
}

fn revalidate_plans(root: &LocalStoreRoot, plans: &[PlannedFile]) -> Result<(), ProjectStoreFault> {
    for plan in plans {
        if file_fact(root, &plan.active_path)? != plan.active_fact
            || file_fact(root, &plan.trash_path)? != plan.trash_fact
        {
            return Err(ProjectStoreFault::SourceChanged);
        }
    }
    Ok(())
}

fn build_steps(
    root: &LocalStoreRoot,
    plans: &[PlannedFile],
    inventory: &mut TrashInventory,
    limits: ProjectStoreLimits,
) -> Result<Vec<Step>, ProjectStoreFault> {
    let mut required_directories = BTreeSet::new();
    for plan in plans {
        if matches!(plan.action, FileAction::Move) {
            let mut parent = plan.trash_path.parent();
            while let Some(path) = parent {
                if path.as_os_str().is_empty() {
                    break;
                }
                if directory_exists(root, path)? {
                    break;
                }
                required_directories.insert(path.to_path_buf());
                parent = path.parent();
            }
        }
    }
    let mut steps = Vec::new();
    for directory in required_directories {
        steps.push(Step::CreateDirectory(directory));
    }
    for plan in plans {
        match plan.action {
            FileAction::Move | FileAction::RemoveActiveDuplicate => {
                steps.push(Step::File(PlannedFile {
                    active_path: plan.active_path.clone(),
                    trash_path: plan.trash_path.clone(),
                    active_fact: plan.active_fact,
                    trash_fact: plan.trash_fact,
                    byte_length: plan.byte_length,
                    action: plan.action,
                }));
            }
            FileAction::AlreadyQuarantined => steps.push(Step::SynchronizeRetry {
                active_path: plan.active_path.clone(),
                trash_path: plan.trash_path.clone(),
                trash_fact: plan
                    .trash_fact
                    .expect("an already-quarantined plan has an exact trash file"),
            }),
        }
    }

    let projected = inventory
        .physical_entries
        .checked_add(
            steps
                .iter()
                .filter(|step| matches!(step, Step::CreateDirectory(_)))
                .count(),
        )
        .ok_or(ProjectStoreFault::Capacity {
            stage: "trash_inventory",
        })?;
    if projected > limits.physical_store_entries_max {
        return Err(ProjectStoreFault::Capacity {
            stage: "trash_inventory",
        });
    }
    simulate_fanout(&steps, &mut inventory.directory_entries, limits)?;
    Ok(steps)
}

fn simulate_fanout(
    steps: &[Step],
    counts: &mut BTreeMap<PathBuf, usize>,
    limits: ProjectStoreLimits,
) -> Result<(), ProjectStoreFault> {
    for step in steps {
        let (parent, adds, removes) = match step {
            Step::CreateDirectory(path) => (path.parent().unwrap_or(Path::new("")), 1, 0),
            Step::File(file) => match file.action {
                FileAction::Move => {
                    let source = file.active_path.parent().unwrap();
                    let source_count =
                        counts
                            .get_mut(source)
                            .ok_or(ProjectStoreFault::Corruption {
                                stage: "trash_inventory",
                            })?;
                    *source_count =
                        source_count
                            .checked_sub(1)
                            .ok_or(ProjectStoreFault::Corruption {
                                stage: "trash_inventory",
                            })?;
                    (file.trash_path.parent().unwrap(), 1, 0)
                }
                FileAction::RemoveActiveDuplicate => {
                    let source = file.active_path.parent().unwrap();
                    let source_count =
                        counts
                            .get_mut(source)
                            .ok_or(ProjectStoreFault::Corruption {
                                stage: "trash_inventory",
                            })?;
                    *source_count =
                        source_count
                            .checked_sub(1)
                            .ok_or(ProjectStoreFault::Corruption {
                                stage: "trash_inventory",
                            })?;
                    continue;
                }
                FileAction::AlreadyQuarantined => continue,
            },
            Step::SynchronizeRetry { .. } => continue,
        };
        let count = counts.entry(parent.to_path_buf()).or_default();
        *count = count
            .checked_add(adds)
            .and_then(|value| value.checked_sub(removes))
            .ok_or(ProjectStoreFault::Capacity {
                stage: "trash_fanout",
            })?;
        if *count > limits.directory_fanout_entries_max {
            return Err(ProjectStoreFault::Capacity {
                stage: "trash_fanout",
            });
        }
        if let Step::CreateDirectory(path) = step {
            counts.entry(path.clone()).or_default();
        }
    }
    Ok(())
}

fn build_batches(
    steps: Vec<Step>,
    limits: ProjectStoreLimits,
) -> Result<Vec<Batch>, ProjectStoreFault> {
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut entries = 0_usize;
    let mut bytes = 0_u64;
    for step in steps {
        let step_entries = step.namespace_mutations();
        let step_bytes = step.checked_bytes();
        if step_bytes > limits.gc_batch_bytes_max {
            return Err(ProjectStoreFault::Capacity {
                stage: "trash_batch_bytes",
            });
        }
        let next_entries =
            entries
                .checked_add(step_entries)
                .ok_or(ProjectStoreFault::Capacity {
                    stage: "trash_batch_entries",
                })?;
        let next_bytes = bytes
            .checked_add(step_bytes)
            .ok_or(ProjectStoreFault::Capacity {
                stage: "trash_batch_bytes",
            })?;
        if !current.is_empty()
            && (current.len() >= limits.gc_batch_entries_max
                || next_entries > limits.gc_batch_entries_max
                || next_bytes > limits.gc_batch_bytes_max)
        {
            batches.push(Batch { steps: current });
            current = Vec::new();
            entries = 0;
            bytes = 0;
        }
        entries = entries
            .checked_add(step_entries)
            .ok_or(ProjectStoreFault::Capacity {
                stage: "trash_batch_entries",
            })?;
        bytes = bytes
            .checked_add(step_bytes)
            .ok_or(ProjectStoreFault::Capacity {
                stage: "trash_batch_bytes",
            })?;
        current.push(step);
    }
    if !current.is_empty() {
        batches.push(Batch { steps: current });
    }
    Ok(batches)
}

fn execute_batches<C>(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    batches: Vec<Batch>,
    _limits: ProjectStoreLimits,
    is_cancelled: &mut C,
    streamed_bytes: u64,
) -> Result<ProjectStoreDiagnostics, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let mut operation_mutated = false;
    let mut published_objects = 0_u64;
    for batch in batches {
        if is_cancelled() {
            return Err(ProjectStoreFault::Cancelled);
        }
        let mut affected = BTreeSet::<PathBuf>::new();
        let mut batch_published = 0_u64;
        for step in batch.steps {
            let result = match step {
                Step::CreateDirectory(path) => {
                    let mut step_mutated = false;
                    let result = execute_directory_create(root, leases, &path, &mut step_mutated);
                    operation_mutated |= step_mutated;
                    result.map(|()| {
                        record_trash_directory_ancestors(&mut affected, &path);
                    })
                }
                Step::File(file) => {
                    let mut step_mutated = false;
                    let result = execute_file_step(root, leases, &file, &mut step_mutated);
                    operation_mutated |= step_mutated;
                    result.map(|()| {
                        record_file_sync_directories(
                            &mut affected,
                            &file.active_path,
                            &file.trash_path,
                        );
                        batch_published += 1;
                    })
                }
                Step::SynchronizeRetry {
                    active_path,
                    trash_path,
                    trash_fact,
                } => synchronize_retry(root, &active_path, &trash_path, trash_fact).map(|()| {
                    record_file_sync_directories(&mut affected, &active_path, &trash_path);
                }),
            };
            if let Err(error) = result {
                return Err(if operation_mutated {
                    ProjectStoreFault::CommitIndeterminate
                } else {
                    error
                });
            }
        }
        for directory in affected {
            if sync_directory_transition(root, leases, &directory).is_err() {
                return Err(ProjectStoreFault::CommitIndeterminate);
            }
        }
        published_objects =
            published_objects
                .checked_add(batch_published)
                .ok_or(ProjectStoreFault::Capacity {
                    stage: "trash_published_objects",
                })?;
    }
    Ok(ProjectStoreDiagnostics {
        streamed_bytes,
        published_objects,
        ..ProjectStoreDiagnostics::default()
    })
}

fn execute_directory_create(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    path: &Path,
    mutated: &mut bool,
) -> Result<(), ProjectStoreFault> {
    let occurrence = transition_before(leases, GcTransition::TrashDirectoryCreate)?;
    create_directory(root, path)?;
    *mutated = true;
    transition_after(leases, occurrence)
}

fn synchronize_retry(
    root: &LocalStoreRoot,
    active_path: &Path,
    trash_path: &Path,
    trash_fact: FileFact,
) -> Result<(), ProjectStoreFault> {
    if file_fact(root, active_path)?.is_some() || file_fact(root, trash_path)? != Some(trash_fact) {
        return Err(ProjectStoreFault::SourceChanged);
    }
    Ok(())
}

fn record_file_sync_directories(
    affected: &mut BTreeSet<PathBuf>,
    active_path: &Path,
    trash_path: &Path,
) {
    affected.insert(
        active_path
            .parent()
            .expect("an active file has a parent")
            .to_path_buf(),
    );
    record_trash_directory_ancestors(
        affected,
        trash_path.parent().expect("a trash file has a parent"),
    );
}

fn record_trash_directory_ancestors(affected: &mut BTreeSet<PathBuf>, directory: &Path) {
    let mut current = Some(directory);
    while let Some(directory) = current {
        affected.insert(directory.to_path_buf());
        if directory.as_os_str().is_empty() {
            break;
        }
        current = directory.parent();
    }
}

fn execute_file_step(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    file: &PlannedFile,
    mutated: &mut bool,
) -> Result<(), ProjectStoreFault> {
    if file_fact(root, &file.active_path)? != file.active_fact
        || file_fact(root, &file.trash_path)? != file.trash_fact
    {
        return Err(ProjectStoreFault::SourceChanged);
    }
    let source_parent = open_directory_path(root.descriptor(), file.active_path.parent().unwrap())
        .map_err(|_| ProjectStoreFault::SourceChanged)?;
    let trash_parent = open_directory_path(root.descriptor(), file.trash_path.parent().unwrap())
        .map_err(|_| ProjectStoreFault::SourceChanged)?;
    let source_name = file.active_path.file_name().unwrap();
    let trash_name = file.trash_path.file_name().unwrap();
    match file.action {
        FileAction::Move => {
            let occurrence = transition_before(leases, GcTransition::TrashMove)?;
            renameat_with(
                &source_parent,
                source_name,
                &trash_parent,
                trash_name,
                RenameFlags::NOREPLACE,
            )
            .map_err(|_| ProjectStoreFault::SourceChanged)?;
            *mutated = true;
            transition_after(leases, occurrence)?;
        }
        FileAction::RemoveActiveDuplicate => {
            let trash = openat(&trash_parent, trash_name, FILE_FLAGS, Mode::empty())
                .map_err(|_| ProjectStoreFault::SourceChanged)?;
            if opened_file_fact(&trash, "trash_collision_file")? != file.trash_fact.unwrap() {
                return Err(ProjectStoreFault::SourceChanged);
            }
            let occurrence = transition_before(leases, GcTransition::TrashCollisionFileSync)?;
            sync_fd(&trash).map_err(|_| ProjectStoreFault::Corruption {
                stage: "trash_collision_file_sync",
            })?;
            transition_after(leases, occurrence)?;
            let occurrence = transition_before(leases, GcTransition::ActiveDeduplicateRemove)?;
            unlinkat(&source_parent, source_name, AtFlags::empty())
                .map_err(|_| ProjectStoreFault::SourceChanged)?;
            *mutated = true;
            transition_after(leases, occurrence)?;
        }
        FileAction::AlreadyQuarantined => return Ok(()),
    }
    let expected_trash = match file.action {
        FileAction::Move => file.active_fact,
        FileAction::RemoveActiveDuplicate => file.trash_fact,
        FileAction::AlreadyQuarantined => unreachable!(),
    };
    if file_fact(root, &file.active_path)?.is_some()
        || file_fact(root, &file.trash_path)? != expected_trash
    {
        return Err(ProjectStoreFault::SourceChanged);
    }
    Ok(())
}

fn create_directory(root: &LocalStoreRoot, path: &Path) -> Result<(), ProjectStoreFault> {
    let parent = open_directory_path(root.descriptor(), path.parent().unwrap_or(Path::new("")))?;
    mkdirat(&parent, path.file_name().unwrap(), Mode::RWXU).map_err(|_| {
        ProjectStoreFault::Corruption {
            stage: "trash_directory_create",
        }
    })
}

fn sync_directory(root: &LocalStoreRoot, path: &Path) -> Result<(), ProjectStoreFault> {
    let directory = open_directory_path(root.descriptor(), path)?;
    sync_fd(&directory).map_err(|_| ProjectStoreFault::CommitIndeterminate)
}

fn sync_directory_transition(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    path: &Path,
) -> Result<(), ProjectStoreFault> {
    let transition = match path.components().next() {
        None => GcTransition::TrashDirectorySync,
        Some(Component::Normal(name)) if name == OsStr::new("trash") => {
            GcTransition::TrashDirectorySync
        }
        Some(Component::Normal(name))
            if name == OsStr::new("generations") || name == OsStr::new("objects") =>
        {
            GcTransition::SourceDirectorySync
        }
        _ => {
            return Err(ProjectStoreFault::Corruption {
                stage: "trash_sync_directory",
            });
        }
    };
    let occurrence = transition_before(leases, transition)?;
    sync_directory(root, path)?;
    transition_after(leases, occurrence)
}

fn sync_fd(fd: &OwnedFd) -> Result<(), Errno> {
    loop {
        match fsync(fd) {
            Ok(()) => return Ok(()),
            Err(Errno::INTR) => {}
            Err(error) => return Err(error),
        }
    }
}

fn check_cancelled<C>(is_cancelled: &mut C) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    if is_cancelled() {
        Err(ProjectStoreFault::Cancelled)
    } else {
        Ok(())
    }
}

fn map_local_error(error: LocalPublicationError, stage: &'static str) -> ProjectStoreFault {
    match error {
        LocalPublicationError::Cancelled => ProjectStoreFault::Cancelled,
        LocalPublicationError::Capacity { .. } | LocalPublicationError::StorageFull { .. } => {
            ProjectStoreFault::Capacity { stage }
        }
        LocalPublicationError::ReadOnly { .. } => ProjectStoreFault::ReadOnly,
        LocalPublicationError::SourceIo { .. } | LocalPublicationError::SourceLength { .. } => {
            ProjectStoreFault::SourceChanged
        }
        LocalPublicationError::SourceDigest => ProjectStoreFault::DigestMismatch,
        LocalPublicationError::RefCommitIndeterminate
        | LocalPublicationError::PackageCommitIndeterminate => {
            ProjectStoreFault::CommitIndeterminate
        }
        LocalPublicationError::DestinationExists
        | LocalPublicationError::InvalidPath
        | LocalPublicationError::ExistingMismatch
        | LocalPublicationError::AtomicPublishUnsupported
        | LocalPublicationError::InvalidGeneration
        | LocalPublicationError::InvalidControl
        | LocalPublicationError::RefAlreadyPresent
        | LocalPublicationError::RefChanged
        | LocalPublicationError::Io { .. } => ProjectStoreFault::Corruption { stage },
    }
}

fn active_generation_path(id: ProjectGenerationId) -> PathBuf {
    digest_path("generations", id.digest(), true)
}

fn trash_generation_path(id: ProjectGenerationId) -> PathBuf {
    PathBuf::from("trash").join(digest_path("generations", id.digest(), true))
}

fn active_object_path(digest: ExactBytesDigest) -> PathBuf {
    digest_path("objects", digest.digest(), false)
}

fn trash_object_path(digest: ExactBytesDigest) -> PathBuf {
    PathBuf::from("trash").join(digest_path("objects", digest.digest(), false))
}

fn digest_path(namespace: &str, digest: Sha256Digest, generation: bool) -> PathBuf {
    let digest = digest.to_string();
    let file = if generation {
        format!("{}.json", &digest[2..])
    } else {
        digest[2..].to_owned()
    };
    PathBuf::from(namespace)
        .join("sha256")
        .join(&digest[..2])
        .join(file)
}

fn file_fact(root: &LocalStoreRoot, path: &Path) -> Result<Option<FileFact>, ProjectStoreFault> {
    let (parent, name) = match open_parent(root.descriptor(), path) {
        Ok(opened) => opened,
        Err(ProjectStoreFault::Corruption {
            stage: "trash_missing_directory",
        }) => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = match statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => metadata,
        Err(Errno::NOENT) => return Ok(None),
        Err(_) => {
            return Err(ProjectStoreFault::Corruption {
                stage: "trash_inventory",
            });
        }
    };
    let named = stat_file_fact(&metadata, "trash_inventory")?;
    let descriptor = openat(&parent, name, FILE_FLAGS, Mode::empty()).map_err(|_| {
        ProjectStoreFault::Corruption {
            stage: "trash_inventory",
        }
    })?;
    if named != opened_file_fact(&descriptor, "trash_inventory")? {
        return Err(ProjectStoreFault::SourceChanged);
    }
    Ok(Some(named))
}

fn stat_file_fact(
    metadata: &rustix::fs::Stat,
    stage: &'static str,
) -> Result<FileFact, ProjectStoreFault> {
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile || metadata.st_nlink != 1
    {
        return Err(ProjectStoreFault::Corruption { stage });
    }
    Ok(FileFact {
        device: metadata.st_dev,
        inode: metadata.st_ino,
        byte_length: u64::try_from(metadata.st_size)
            .map_err(|_| ProjectStoreFault::Corruption { stage })?,
    })
}

fn opened_file_fact(
    descriptor: impl AsFd,
    stage: &'static str,
) -> Result<FileFact, ProjectStoreFault> {
    let metadata = fstat(descriptor).map_err(|_| ProjectStoreFault::Corruption { stage })?;
    stat_file_fact(&metadata, stage)
}

fn read_stable_file<C>(
    root: &LocalStoreRoot,
    path: &Path,
    expected: FileFact,
    maximum: u64,
    stream_buffer_bytes: usize,
    is_cancelled: &mut C,
) -> Result<Vec<u8>, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    if expected.byte_length > maximum {
        return Err(ProjectStoreFault::Capacity {
            stage: "trash_file_bytes",
        });
    }
    let (parent, name) = open_parent(root.descriptor(), path)?;
    let descriptor = openat(&parent, name, FILE_FLAGS, Mode::empty()).map_err(|_| {
        ProjectStoreFault::Corruption {
            stage: "trash_file",
        }
    })?;
    if opened_file_fact(&descriptor, "trash_file")? != expected {
        return Err(ProjectStoreFault::SourceChanged);
    }
    let mut file = File::from(descriptor);
    let mut bytes = Vec::with_capacity(usize::try_from(expected.byte_length).map_err(|_| {
        ProjectStoreFault::Capacity {
            stage: "trash_file_bytes",
        }
    })?);
    let mut buffer = vec![0_u8; stream_buffer_bytes];
    loop {
        check_cancelled(is_cancelled)?;
        let observed = u64::try_from(bytes.len()).map_err(|_| ProjectStoreFault::Capacity {
            stage: "trash_file_bytes",
        })?;
        let remaining = expected.byte_length.saturating_sub(observed);
        let read_limit = if remaining == 0 {
            1
        } else {
            usize::try_from(remaining)
                .unwrap_or(usize::MAX)
                .min(buffer.len())
        };
        let read = match file.read(&mut buffer[..read_limit]) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                return Err(ProjectStoreFault::Corruption {
                    stage: "trash_file",
                });
            }
        };
        if read == 0 {
            break;
        }
        if remaining == 0 {
            return Err(ProjectStoreFault::SourceChanged);
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    if u64::try_from(bytes.len()).ok() != Some(expected.byte_length)
        || file_fact(root, path)? != Some(expected)
    {
        return Err(ProjectStoreFault::SourceChanged);
    }
    Ok(bytes)
}

fn hash_exact_file<C>(
    root: &LocalStoreRoot,
    path: &Path,
    expected: FileFact,
    digest: ExactBytesDigest,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    if expected.byte_length > limits.object_or_page_bytes_max {
        return Err(ProjectStoreFault::Capacity {
            stage: "trash_object_bytes",
        });
    }
    let (parent, name) = open_parent(root.descriptor(), path)?;
    let descriptor = openat(&parent, name, FILE_FLAGS, Mode::empty()).map_err(|_| {
        ProjectStoreFault::Corruption {
            stage: "trash_object",
        }
    })?;
    if opened_file_fact(&descriptor, "trash_object")? != expected {
        return Err(ProjectStoreFault::SourceChanged);
    }
    let mut file = File::from(descriptor);
    let mut hasher = ExactBytesHasher::new();
    let mut observed = 0_u64;
    let mut buffer = vec![0_u8; limits.stream_buffer_bytes_max];
    loop {
        check_cancelled(is_cancelled)?;
        let remaining = expected.byte_length.saturating_sub(observed);
        let read_limit = if remaining == 0 {
            1
        } else {
            usize::try_from(remaining)
                .unwrap_or(usize::MAX)
                .min(buffer.len())
        };
        let read = match file.read(&mut buffer[..read_limit]) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                return Err(ProjectStoreFault::Corruption {
                    stage: "trash_object",
                });
            }
        };
        observed = observed
            .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
            .ok_or(ProjectStoreFault::Capacity {
                stage: "trash_object_bytes",
            })?;
        if observed > expected.byte_length {
            return Err(ProjectStoreFault::Corruption {
                stage: "trash_object",
            });
        }
        hasher
            .update(&buffer[..read])
            .map_err(|_| ProjectStoreFault::Corruption {
                stage: "trash_object",
            })?;
    }
    let facts = hasher
        .finalize()
        .map_err(|_| ProjectStoreFault::Corruption {
            stage: "trash_object",
        })?;
    if observed != expected.byte_length
        || facts.digest() != digest
        || file_fact(root, path)? != Some(expected)
    {
        return Err(ProjectStoreFault::Corruption {
            stage: "trash_object",
        });
    }
    Ok(())
}

fn directory_exists(root: &LocalStoreRoot, path: &Path) -> Result<bool, ProjectStoreFault> {
    match open_directory_path(root.descriptor(), path) {
        Ok(_) => Ok(true),
        Err(ProjectStoreFault::Corruption {
            stage: "trash_missing_directory",
        }) => Ok(false),
        Err(error) => Err(error),
    }
}

fn open_parent<'a>(
    root: &OwnedFd,
    path: &'a Path,
) -> Result<(OwnedFd, &'a OsStr), ProjectStoreFault> {
    let name = path.file_name().ok_or(ProjectStoreFault::Corruption {
        stage: "trash_path",
    })?;
    let parent = open_directory_path(root, path.parent().unwrap_or(Path::new("")))?;
    Ok((parent, name))
}

fn open_directory_path(root: &OwnedFd, path: &Path) -> Result<OwnedFd, ProjectStoreFault> {
    let mut current =
        openat(root, Path::new("."), DIRECTORY_FLAGS, Mode::empty()).map_err(|_| {
            ProjectStoreFault::Corruption {
                stage: "trash_inventory",
            }
        })?;
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(ProjectStoreFault::Corruption {
                stage: "trash_path",
            });
        };
        current = match openat(&current, component, DIRECTORY_FLAGS, Mode::empty()) {
            Ok(directory) => directory,
            Err(Errno::NOENT) => {
                return Err(ProjectStoreFault::Corruption {
                    stage: "trash_missing_directory",
                });
            }
            Err(_) => {
                return Err(ProjectStoreFault::Corruption {
                    stage: "trash_inventory",
                });
            }
        };
    }
    Ok(current)
}

fn scan_strict_store<C>(
    root: &LocalStoreRoot,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<TrashInventory, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let mut inventory = TrashInventory::default();
    validate_root_entries(root, is_cancelled)?;
    scan_tree_counts(
        root.descriptor(),
        Path::new(""),
        1,
        limits,
        is_cancelled,
        &mut inventory,
    )?;
    validate_empty_optional_directory(root, Path::new("staging"), is_cancelled)?;
    validate_empty_optional_directory(root, Path::new("locks"), is_cancelled)?;
    scan_trash_namespaces(root, limits, is_cancelled, &mut inventory)?;
    Ok(inventory)
}

fn scan_tree_counts<C>(
    root: &OwnedFd,
    path: &Path,
    depth: usize,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
    inventory: &mut TrashInventory,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    check_cancelled(is_cancelled)?;
    if depth > STORE_DIRECTORY_DEPTH_MAX {
        return Err(ProjectStoreFault::Corruption {
            stage: "trash_inventory",
        });
    }
    let directory = open_directory_path(root, path)?;
    let mut count = 0_usize;
    let mut children = Vec::new();
    for entry in Dir::read_from(&directory).map_err(|_| ProjectStoreFault::Corruption {
        stage: "trash_inventory",
    })? {
        check_cancelled(is_cancelled)?;
        let entry = entry.map_err(|_| ProjectStoreFault::Corruption {
            stage: "trash_inventory",
        })?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        count = count.checked_add(1).ok_or(ProjectStoreFault::Capacity {
            stage: "trash_inventory",
        })?;
        inventory.physical_entries =
            inventory
                .physical_entries
                .checked_add(1)
                .ok_or(ProjectStoreFault::Capacity {
                    stage: "trash_inventory",
                })?;
        if count > limits.directory_fanout_entries_max
            || inventory.physical_entries > limits.physical_store_entries_max
        {
            return Err(ProjectStoreFault::Capacity {
                stage: "trash_inventory",
            });
        }
        let stat = statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| {
            ProjectStoreFault::Corruption {
                stage: "trash_inventory",
            }
        })?;
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::Directory => children.push(path.join(OsStr::from_bytes(name.to_bytes()))),
            FileType::RegularFile if stat.st_nlink == 1 => {}
            _ => {
                return Err(ProjectStoreFault::Corruption {
                    stage: "trash_inventory",
                });
            }
        }
    }
    inventory
        .directory_entries
        .insert(path.to_path_buf(), count);
    children.sort_unstable();
    drop(directory);
    for child in children {
        let child_depth = depth.checked_add(1).ok_or(ProjectStoreFault::Capacity {
            stage: "trash_descriptors",
        })?;
        scan_tree_counts(root, &child, child_depth, limits, is_cancelled, inventory)?;
    }
    Ok(())
}

fn validate_root_entries<C>(
    root: &LocalStoreRoot,
    is_cancelled: &mut C,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let directory = open_directory_path(root.descriptor(), Path::new(""))?;
    for entry in Dir::read_from(&directory).map_err(|_| ProjectStoreFault::Corruption {
        stage: "trash_inventory",
    })? {
        check_cancelled(is_cancelled)?;
        let entry = entry.map_err(|_| ProjectStoreFault::Corruption {
            stage: "trash_inventory",
        })?;
        let name = entry.file_name().to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        if !matches!(
            name,
            b"project.json"
                | b"refs"
                | b"generations"
                | b"objects"
                | b"staging"
                | b"locks"
                | b"trash"
        ) {
            return Err(ProjectStoreFault::Corruption {
                stage: "trash_inventory",
            });
        }
    }
    Ok(())
}

fn validate_empty_optional_directory<C>(
    root: &LocalStoreRoot,
    path: &Path,
    is_cancelled: &mut C,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let directory = match open_directory_path(root.descriptor(), path) {
        Ok(directory) => directory,
        Err(ProjectStoreFault::Corruption {
            stage: "trash_missing_directory",
        }) => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in Dir::read_from(&directory).map_err(|_| ProjectStoreFault::Corruption {
        stage: "trash_inventory",
    })? {
        check_cancelled(is_cancelled)?;
        let entry = entry.map_err(|_| ProjectStoreFault::Corruption {
            stage: "trash_inventory",
        })?;
        if !matches!(entry.file_name().to_bytes(), b"." | b"..") {
            return Err(ProjectStoreFault::Corruption {
                stage: "trash_inventory",
            });
        }
    }
    Ok(())
}

fn scan_trash_namespaces<C>(
    root: &LocalStoreRoot,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
    inventory: &mut TrashInventory,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let trash = match open_directory_path(root.descriptor(), Path::new("trash")) {
        Ok(directory) => directory,
        Err(ProjectStoreFault::Corruption {
            stage: "trash_missing_directory",
        }) => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in Dir::read_from(&trash).map_err(|_| ProjectStoreFault::Corruption {
        stage: "trash_inventory",
    })? {
        check_cancelled(is_cancelled)?;
        let entry = entry.map_err(|_| ProjectStoreFault::Corruption {
            stage: "trash_inventory",
        })?;
        let name = entry.file_name().to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        if !matches!(name, b"generations" | b"objects") {
            return Err(ProjectStoreFault::Corruption {
                stage: "trash_inventory",
            });
        }
    }
    scan_trash_namespace(root, true, limits, is_cancelled, inventory)?;
    scan_trash_namespace(root, false, limits, is_cancelled, inventory)
}

fn scan_trash_namespace<C>(
    root: &LocalStoreRoot,
    generation: bool,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
    inventory: &mut TrashInventory,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let namespace = if generation { "generations" } else { "objects" };
    let base = PathBuf::from("trash").join(namespace);
    let namespace_directory = match open_directory_path(root.descriptor(), &base) {
        Ok(directory) => directory,
        Err(ProjectStoreFault::Corruption {
            stage: "trash_missing_directory",
        }) => return Ok(()),
        Err(error) => return Err(error),
    };
    validate_only_named_directory(&namespace_directory, b"sha256", is_cancelled)?;
    let sha_path = base.join("sha256");
    let sha = match open_directory_path(root.descriptor(), &sha_path) {
        Ok(directory) => directory,
        Err(ProjectStoreFault::Corruption {
            stage: "trash_missing_directory",
        }) => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in Dir::read_from(&sha).map_err(|_| ProjectStoreFault::Corruption {
        stage: "trash_inventory",
    })? {
        check_cancelled(is_cancelled)?;
        let entry = entry.map_err(|_| ProjectStoreFault::Corruption {
            stage: "trash_inventory",
        })?;
        let fanout = entry.file_name().to_bytes();
        if matches!(fanout, b"." | b"..") {
            continue;
        }
        if fanout.len() != 2
            || !fanout.iter().all(u8::is_ascii_hexdigit)
            || fanout.iter().any(u8::is_ascii_uppercase)
        {
            return Err(ProjectStoreFault::Corruption {
                stage: "trash_inventory",
            });
        }
        let fanout_path = sha_path.join(OsStr::from_bytes(fanout));
        let fanout_dir = open_directory_path(root.descriptor(), &fanout_path)?;
        for file in Dir::read_from(&fanout_dir).map_err(|_| ProjectStoreFault::Corruption {
            stage: "trash_inventory",
        })? {
            check_cancelled(is_cancelled)?;
            let file = file.map_err(|_| ProjectStoreFault::Corruption {
                stage: "trash_inventory",
            })?;
            let name = file.file_name().to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            let suffix = if generation {
                name.strip_suffix(b".json")
                    .ok_or(ProjectStoreFault::Corruption {
                        stage: "trash_inventory",
                    })?
            } else {
                name
            };
            if suffix.len() != 62
                || !suffix.iter().all(u8::is_ascii_hexdigit)
                || suffix.iter().any(u8::is_ascii_uppercase)
            {
                return Err(ProjectStoreFault::Corruption {
                    stage: "trash_inventory",
                });
            }
            let mut hex = Vec::with_capacity(64);
            hex.extend_from_slice(fanout);
            hex.extend_from_slice(suffix);
            let hex = std::str::from_utf8(&hex).map_err(|_| ProjectStoreFault::Corruption {
                stage: "trash_inventory",
            })?;
            let digest =
                hex.parse::<Sha256Digest>()
                    .map_err(|_| ProjectStoreFault::Corruption {
                        stage: "trash_inventory",
                    })?;
            let path = fanout_path.join(OsStr::from_bytes(name));
            let fact = file_fact(root, &path)?.ok_or(ProjectStoreFault::SourceChanged)?;
            if generation {
                if fact.byte_length > limits.generation_bytes_max
                    || inventory
                        .generations
                        .insert(ProjectGenerationId::from_digest(digest), fact)
                        .is_some()
                {
                    return Err(ProjectStoreFault::Corruption {
                        stage: "trash_inventory",
                    });
                }
            } else if fact.byte_length > limits.object_or_page_bytes_max
                || inventory
                    .objects
                    .insert(ExactBytesDigest::from_digest(digest), fact)
                    .is_some()
            {
                return Err(ProjectStoreFault::Corruption {
                    stage: "trash_inventory",
                });
            }
        }
    }
    Ok(())
}

fn validate_only_named_directory<C>(
    directory: &OwnedFd,
    expected: &[u8],
    is_cancelled: &mut C,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    for entry in Dir::read_from(directory).map_err(|_| ProjectStoreFault::Corruption {
        stage: "trash_inventory",
    })? {
        check_cancelled(is_cancelled)?;
        let entry = entry.map_err(|_| ProjectStoreFault::Corruption {
            stage: "trash_inventory",
        })?;
        let name = entry.file_name().to_bytes();
        if !matches!(name, b"." | b"..") && name != expected {
            return Err(ProjectStoreFault::Corruption {
                stage: "trash_inventory",
            });
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs,
        os::unix::fs::symlink,
        path::{Path, PathBuf},
        process::Command,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    use serde_json::Value;

    use super::*;
    use crate::{
        ProjectOpenMode,
        lease::{GcTransitionInjector, GcTransitionTarget, LeaseError, TransitionEdge},
        wire,
    };

    const RECOVERABLE_ORPHAN: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "cfd67414728bb345edb7d5eabffac2530f04ed3b768d720782efe88e2d7ca370"
    );
    const ZERO_NON_REGENERABLE_ORPHAN: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "2106460bd83ed53a6042c623c756854e672c72ba31867bbd413500463fa8fb3a"
    );
    const DIVERGENT_ORPHAN: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "10011b8d7dce93c428e1d117b485746522b4ae1d4d8ee89e359739f2cffd3a10"
    );
    static TEST_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn selected_zero_non_regenerable_orphan_preserves_shared_objects_and_retries_exactly() {
        let project = TestProject::extracted("trash-shared", "recoverable.m4dproj");
        let selected = install_zero_non_regenerable_orphan(project.path());
        let active = project.path().join(active_generation_path(selected));
        let trash = project.path().join(trash_generation_path(selected));
        let selected_bytes = fs::read(&active).unwrap();
        let anonymous = install_anonymous_object(project.path());
        let objects_before = all_file_bytes(&project.path().join("objects"));
        let root = LocalStoreRoot::open(project.path()).unwrap();
        let mut leases =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();

        let one_mutation_batches = ProjectStoreLimits {
            gc_batch_entries_max: 1,
            ..ProjectStoreLimits::default()
        };
        let cancellation_trace = GcTransitionInjector::recorder();
        leases.set_gc_transition_injector(Arc::clone(&cancellation_trace));
        assert_eq!(
            trash_generations(
                &root,
                &mut leases,
                &[selected],
                one_mutation_batches,
                || project.path().join("trash").exists(),
            ),
            Err(ProjectStoreFault::Cancelled)
        );
        assert!(active.exists());
        assert!(!trash.exists());
        assert!(anonymous.exists());
        assert_eq!(
            cancellation_trace.attempts(GcTransition::TrashDirectoryCreate),
            1
        );
        assert_eq!(
            cancellation_trace.attempts(GcTransition::TrashDirectorySync),
            2
        );

        let first = trash_generations(
            &root,
            &mut leases,
            &[selected],
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(first.published_objects, 1);
        assert_eq!(first.streamed_bytes, 0);
        assert!(!active.exists());
        assert_eq!(fs::read(&trash).unwrap(), selected_bytes);
        assert!(!project.path().join("trash/objects").exists());
        assert!(anonymous.exists());

        let retry = trash_generations(
            &root,
            &mut leases,
            &[selected],
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(retry.published_objects, 0);
        assert_eq!(retry.streamed_bytes, 0);
        assert_eq!(fs::read(&trash).unwrap(), selected_bytes);

        fs::create_dir_all(active.parent().unwrap()).unwrap();
        fs::write(&active, &selected_bytes).unwrap();
        let deduplicated = trash_generations(
            &root,
            &mut leases,
            &[selected],
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(deduplicated.published_objects, 1);
        assert_eq!(
            deduplicated.streamed_bytes,
            u64::try_from(selected_bytes.len() * 2).unwrap()
        );
        assert!(!active.exists());
        assert_eq!(fs::read(&trash).unwrap(), selected_bytes);
        assert_eq!(
            all_file_bytes(&project.path().join("objects")),
            objects_before
        );
    }

    #[test]
    fn purge_object_first_bounded_recovery_and_strict_preflight_are_exact() {
        for copied_object_count in [1_usize, 2] {
            let project = TestProject::extracted("purge-success", "recoverable.m4dproj");
            let prepared = prepare_shared_object_purge(project.path(), copied_object_count);
            let trash_directories = all_directories(&project.path().join("trash"));
            let root = LocalStoreRoot::open(project.path()).unwrap();
            let mut leases =
                ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
            let trace = GcTransitionInjector::recorder();
            leases.set_gc_transition_injector(Arc::clone(&trace));
            let result = purge_trash(
                &root,
                &mut leases,
                ProjectStoreLimits {
                    gc_batch_entries_max: 1,
                    ..ProjectStoreLimits::default()
                },
                || false,
            )
            .unwrap();
            assert_eq!(
                result.published_objects,
                u64::try_from(copied_object_count + 1).unwrap()
            );
            assert_eq!(result.streamed_bytes, prepared.streamed_bytes);
            assert!(!prepared.generation_path.exists());
            for (active, trash, bytes) in &prepared.objects {
                assert_eq!(fs::read(active).unwrap(), *bytes);
                assert!(!trash.exists());
            }
            assert_eq!(
                all_directories(&project.path().join("trash")),
                trash_directories
            );
            assert_eq!(
                trace.attempts(GcTransition::PurgeRemove),
                copied_object_count + 1
            );
            assert_eq!(trace.attempts(GcTransition::MaintenanceUpgrade), 1);
            assert_eq!(trace.attempts(GcTransition::MaintenanceRestore), 1);

            let retry =
                purge_trash(&root, &mut leases, ProjectStoreLimits::default(), || false).unwrap();
            assert_eq!(retry.published_objects, 0);
            assert_eq!(retry.streamed_bytes, 0);
        }

        let project = TestProject::extracted("purge-cancel", "recoverable.m4dproj");
        let prepared = prepare_shared_object_purge(project.path(), 2);
        let first_trash = prepared.objects[0].1.clone();
        let root = LocalStoreRoot::open(project.path()).unwrap();
        let mut leases =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        assert_eq!(
            purge_trash(
                &root,
                &mut leases,
                ProjectStoreLimits {
                    gc_batch_entries_max: 1,
                    ..ProjectStoreLimits::default()
                },
                || !first_trash.exists(),
            ),
            Err(ProjectStoreFault::Cancelled)
        );
        assert!(!first_trash.exists());
        assert!(prepared.objects[1].1.exists());
        assert!(prepared.generation_path.exists());
        assert!(leases.confirm_writer(&root).unwrap());
        let recovered = purge_trash(
            &root,
            &mut leases,
            ProjectStoreLimits {
                gc_batch_entries_max: 1,
                ..ProjectStoreLimits::default()
            },
            || false,
        )
        .unwrap();
        assert_eq!(recovered.published_objects, 2);
        assert!(!prepared.objects[1].1.exists());
        assert!(!prepared.generation_path.exists());

        let capacity = TestProject::extracted("purge-capacity", "recoverable.m4dproj");
        prepare_shared_object_purge(capacity.path(), 1);
        let files_before = all_file_bytes(capacity.path());
        let directories_before = all_directories(capacity.path());
        let root = LocalStoreRoot::open(capacity.path()).unwrap();
        let mut leases =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        assert_eq!(
            purge_trash(
                &root,
                &mut leases,
                ProjectStoreLimits {
                    gc_batch_bytes_max: 1_000,
                    ..ProjectStoreLimits::default()
                },
                || false,
            ),
            Err(ProjectStoreFault::Capacity {
                stage: "purge_generation_batch"
            })
        );
        assert!(leases.confirm_writer(&root).unwrap());
        assert_eq!(all_file_bytes(capacity.path()), files_before);
        assert_eq!(all_directories(capacity.path()), directories_before);

        let changed_parent = TestProject::extracted("purge-changed-parent", "recoverable.m4dproj");
        let prepared = prepare_shared_object_purge(changed_parent.path(), 2);
        let first_trash = prepared.objects[0].1.clone();
        let second_trash = prepared.objects[1].1.clone();
        let root = LocalStoreRoot::open(changed_parent.path()).unwrap();
        let mut leases =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let mut removed_later_parent = false;
        assert_eq!(
            purge_trash(
                &root,
                &mut leases,
                ProjectStoreLimits {
                    gc_batch_entries_max: 1,
                    ..ProjectStoreLimits::default()
                },
                || {
                    if !first_trash.exists() && second_trash.exists() {
                        fs::remove_file(&second_trash).unwrap();
                        fs::remove_dir(second_trash.parent().unwrap()).unwrap();
                        removed_later_parent = true;
                    }
                    false
                },
            ),
            Err(ProjectStoreFault::CommitIndeterminate)
        );
        assert!(removed_later_parent);
        assert!(prepared.generation_path.exists());
        assert!(matches!(
            leases.confirm_writer(&root),
            Err(LeaseError::Indeterminate)
        ));
        assert!(!first_trash.exists());
        drop(leases);
        drop(root);
        let root = LocalStoreRoot::open(changed_parent.path()).unwrap();
        let mut leases =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let recovered =
            purge_trash(&root, &mut leases, ProjectStoreLimits::default(), || false).unwrap();
        assert_eq!(recovered.published_objects, 1);
        assert!(!prepared.generation_path.exists());

        let non_regenerable = TestProject::extracted("purge-nonreg", "divergent.m4dproj");
        let selected = generation_id(DIVERGENT_ORPHAN);
        quarantine_generation_only(non_regenerable.path(), selected);
        let before = all_file_bytes(non_regenerable.path());
        let root = LocalStoreRoot::open(non_regenerable.path()).unwrap();
        let mut leases =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        assert_eq!(
            purge_trash(&root, &mut leases, ProjectStoreLimits::default(), || false,),
            Err(ProjectStoreFault::ConfirmationRequired)
        );
        assert_eq!(all_file_bytes(non_regenerable.path()), before);

        let incomplete = TestProject::extracted("purge-incomplete", "recoverable.m4dproj");
        let (selected, object) = install_unique_regenerable_orphan(incomplete.path());
        quarantine_generation_only(incomplete.path(), selected);
        let before = all_file_bytes(incomplete.path());
        let root = LocalStoreRoot::open(incomplete.path()).unwrap();
        let mut leases =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        assert_eq!(
            purge_trash(&root, &mut leases, ProjectStoreLimits::default(), || false,),
            Err(ProjectStoreFault::SourceChanged)
        );
        assert!(object.exists());
        assert_eq!(all_file_bytes(incomplete.path()), before);

        let object_bytes = fs::read(&object).unwrap();
        let digest = ExactBytesHasher::hash(&object_bytes).unwrap().digest();
        let trash_duplicate = incomplete.path().join(trash_object_path(digest));
        fs::create_dir_all(trash_duplicate.parent().unwrap()).unwrap();
        fs::write(&trash_duplicate, &object_bytes).unwrap();
        let duplicate_before = all_file_bytes(incomplete.path());
        assert_eq!(
            purge_trash(&root, &mut leases, ProjectStoreLimits::default(), || false,),
            Err(ProjectStoreFault::SourceChanged)
        );
        assert_eq!(all_file_bytes(incomplete.path()), duplicate_before);

        let unreferenced = TestProject::extracted("purge-unreferenced", "recoverable.m4dproj");
        prepare_shared_object_purge(unreferenced.path(), 1);
        let bytes = b"unreferenced-trash-object";
        let digest = ExactBytesHasher::hash(bytes).unwrap().digest();
        let path = unreferenced.path().join(trash_object_path(digest));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        let before = all_file_bytes(unreferenced.path());
        let root = LocalStoreRoot::open(unreferenced.path()).unwrap();
        let mut leases =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        assert_eq!(
            purge_trash(&root, &mut leases, ProjectStoreLimits::default(), || false,),
            Err(ProjectStoreFault::Corruption {
                stage: "purge_unreferenced_object"
            })
        );
        assert_eq!(all_file_bytes(unreferenced.path()), before);
    }

    #[test]
    fn purge_transition_failures_and_sync_retries_are_exact() {
        assert_eq!(GcTransition::ALL.len(), 10);
        assert_eq!(
            GcTransition::PURGE.map(GcTransition::name),
            [
                "gc_maintenance_upgrade",
                "purge_remove",
                "purge_directory_sync",
                "gc_maintenance_restore",
            ]
        );
        for transition in GcTransition::ALL
            .into_iter()
            .chain([GcTransition::PurgeRemove, GcTransition::PurgeDirectorySync])
        {
            assert_eq!(GcTransition::parse(transition.name()), Some(transition));
        }
        assert_eq!(GcTransition::parse("purge_unknown"), None);

        let trace_project = TestProject::extracted("purge-trace", "recoverable.m4dproj");
        let prepared = prepare_shared_object_purge(trace_project.path(), 1);
        let root = LocalStoreRoot::open(trace_project.path()).unwrap();
        let mut leases =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let trace = GcTransitionInjector::recorder();
        leases.set_gc_transition_injector(Arc::clone(&trace));
        purge_trash(&root, &mut leases, ProjectStoreLimits::default(), || false).unwrap();
        for (transition, expected) in [
            (GcTransition::MaintenanceUpgrade, 1),
            (GcTransition::PurgeRemove, 2),
            (GcTransition::PurgeDirectorySync, 4),
            (GcTransition::MaintenanceRestore, 1),
        ] {
            assert_eq!(
                trace.attempts(transition),
                expected,
                "unexpected {} count",
                transition.name()
            );
        }
        assert!(!prepared.generation_path.exists());
        drop(leases);
        drop(root);

        for transition in GcTransition::PURGE {
            let occurrences = match transition {
                GcTransition::PurgeRemove => 2,
                GcTransition::PurgeDirectorySync => 4,
                _ => 1,
            };
            for edge in [TransitionEdge::Before, TransitionEdge::After] {
                for occurrence in 0..occurrences {
                    let project = TestProject::extracted("purge-transition", "recoverable.m4dproj");
                    let prepared = prepare_shared_object_purge(project.path(), 1);
                    let files_before = all_file_bytes(project.path());
                    let injector = GcTransitionInjector::failing(GcTransitionTarget {
                        transition,
                        edge,
                        occurrence,
                    });
                    let root = LocalStoreRoot::open(project.path()).unwrap();
                    let mut leases =
                        ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable)
                            .unwrap();
                    leases.set_gc_transition_injector(Arc::clone(&injector));
                    let result =
                        purge_trash(&root, &mut leases, ProjectStoreLimits::default(), || false);
                    assert_eq!(
                        injector.fired(),
                        1,
                        "{} {} occurrence {occurrence}",
                        transition.name(),
                        edge.name()
                    );
                    let indeterminate = !matches!(
                        (transition, edge, occurrence),
                        (GcTransition::MaintenanceUpgrade, _, _)
                            | (GcTransition::PurgeRemove, TransitionEdge::Before, 0)
                    );
                    if indeterminate {
                        assert_eq!(result, Err(ProjectStoreFault::CommitIndeterminate));
                        assert!(matches!(
                            leases.confirm_writer(&root),
                            Err(LeaseError::Indeterminate)
                        ));
                        if transition == GcTransition::MaintenanceRestore {
                            assert_eq!(leases.maintenance_lost(), edge == TransitionEdge::Before);
                        }
                    } else {
                        assert!(matches!(result, Err(ProjectStoreFault::Corruption { .. })));
                        assert!(leases.confirm_writer(&root).unwrap());
                        assert_eq!(all_file_bytes(project.path()), files_before);
                    }
                    drop(leases);

                    let mut retry_leases =
                        ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable)
                            .unwrap();
                    purge_trash(
                        &root,
                        &mut retry_leases,
                        ProjectStoreLimits::default(),
                        || false,
                    )
                    .unwrap();
                    assert!(!prepared.generation_path.exists());
                    assert!(!prepared.objects[0].1.exists());
                    assert_eq!(
                        fs::read(&prepared.objects[0].0).unwrap(),
                        prepared.objects[0].2
                    );
                    let zero_removal = purge_trash(
                        &root,
                        &mut retry_leases,
                        ProjectStoreLimits::default(),
                        || false,
                    )
                    .unwrap();
                    assert_eq!(zero_removal.published_objects, 0);
                    assert_eq!(zero_removal.streamed_bytes, 0);
                }
            }
        }
    }

    #[test]
    fn transition_inventory_failures_and_sync_only_retries_are_exact() {
        assert_eq!(
            GcTransition::ALL.map(GcTransition::name),
            [
                "gc_maintenance_upgrade",
                "gc_root_scan",
                "gc_candidate_listing",
                "gc_trash_directory_create",
                "gc_trash_collision_file_sync",
                "gc_trash_move",
                "gc_active_deduplicate_remove",
                "gc_source_directory_sync",
                "gc_trash_directory_sync",
                "gc_maintenance_restore",
            ]
        );
        let traced = TestProject::extracted("trash-transition-trace", "recoverable.m4dproj");
        let selected = install_zero_non_regenerable_orphan(traced.path());
        let active = traced.path().join(active_generation_path(selected));
        let selected_bytes = fs::read(&active).unwrap();
        let root = LocalStoreRoot::open(traced.path()).unwrap();
        let mut leases =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let move_trace = GcTransitionInjector::recorder();
        leases.set_gc_transition_injector(Arc::clone(&move_trace));
        let first = trash_generations(
            &root,
            &mut leases,
            &[selected],
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(first.published_objects, 1);
        for (transition, expected) in [
            (GcTransition::MaintenanceUpgrade, 1),
            (GcTransition::RootScan, 1),
            (GcTransition::CandidateListing, 1),
            (GcTransition::TrashDirectoryCreate, 4),
            (GcTransition::TrashCollisionFileSync, 0),
            (GcTransition::TrashMove, 1),
            (GcTransition::ActiveDeduplicateRemove, 0),
            (GcTransition::SourceDirectorySync, 1),
            (GcTransition::TrashDirectorySync, 5),
            (GcTransition::MaintenanceRestore, 1),
        ] {
            assert_eq!(
                move_trace.attempts(transition),
                expected,
                "unexpected {} count",
                transition.name()
            );
        }

        let retry_trace = GcTransitionInjector::recorder();
        leases.set_gc_transition_injector(Arc::clone(&retry_trace));
        let retry = trash_generations(
            &root,
            &mut leases,
            &[selected],
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(retry.published_objects, 0);
        assert_eq!(retry.streamed_bytes, 0);
        for (transition, expected) in [
            (GcTransition::MaintenanceUpgrade, 1),
            (GcTransition::RootScan, 1),
            (GcTransition::CandidateListing, 1),
            (GcTransition::TrashDirectoryCreate, 0),
            (GcTransition::TrashCollisionFileSync, 0),
            (GcTransition::TrashMove, 0),
            (GcTransition::ActiveDeduplicateRemove, 0),
            (GcTransition::SourceDirectorySync, 1),
            (GcTransition::TrashDirectorySync, 5),
            (GcTransition::MaintenanceRestore, 1),
        ] {
            assert_eq!(retry_trace.attempts(transition), expected);
        }

        fs::create_dir_all(active.parent().unwrap()).unwrap();
        fs::write(&active, &selected_bytes).unwrap();
        let duplicate_trace = GcTransitionInjector::recorder();
        leases.set_gc_transition_injector(Arc::clone(&duplicate_trace));
        trash_generations(
            &root,
            &mut leases,
            &[selected],
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        for (transition, expected) in [
            (GcTransition::MaintenanceUpgrade, 1),
            (GcTransition::RootScan, 1),
            (GcTransition::CandidateListing, 1),
            (GcTransition::TrashDirectoryCreate, 0),
            (GcTransition::TrashCollisionFileSync, 1),
            (GcTransition::TrashMove, 0),
            (GcTransition::ActiveDeduplicateRemove, 1),
            (GcTransition::SourceDirectorySync, 1),
            (GcTransition::TrashDirectorySync, 5),
            (GcTransition::MaintenanceRestore, 1),
        ] {
            assert_eq!(duplicate_trace.attempts(transition), expected);
        }
        drop(leases);
        drop(root);

        #[derive(Clone, Copy)]
        enum Scenario {
            Move,
            Duplicate,
        }

        let mut cases = Vec::new();
        for edge in [TransitionEdge::Before, TransitionEdge::After] {
            for transition in GcTransition::ALL {
                let scenario = match transition {
                    GcTransition::TrashCollisionFileSync
                    | GcTransition::ActiveDeduplicateRemove => Scenario::Duplicate,
                    _ => Scenario::Move,
                };
                let occurrences = match transition {
                    GcTransition::TrashDirectoryCreate => 4,
                    GcTransition::TrashDirectorySync => 5,
                    _ => 1,
                };
                for occurrence in 0..occurrences {
                    let indeterminate = match (transition, edge) {
                        (GcTransition::TrashDirectoryCreate, TransitionEdge::Before) => {
                            occurrence > 0
                        }
                        (GcTransition::TrashDirectoryCreate, TransitionEdge::After)
                        | (GcTransition::TrashMove, _)
                        | (GcTransition::ActiveDeduplicateRemove, TransitionEdge::After)
                        | (GcTransition::SourceDirectorySync, _)
                        | (GcTransition::TrashDirectorySync, _)
                        | (GcTransition::MaintenanceRestore, _) => true,
                        _ => false,
                    };
                    cases.push((transition, edge, occurrence, scenario, indeterminate));
                }
            }
        }

        for (transition, edge, occurrence, scenario, indeterminate) in cases {
            let project = TestProject::extracted("trash-transition", "recoverable.m4dproj");
            let selected = install_zero_non_regenerable_orphan(project.path());
            let active = project.path().join(active_generation_path(selected));
            let trash = project.path().join(trash_generation_path(selected));
            let selected_bytes = fs::read(&active).unwrap();
            if matches!(scenario, Scenario::Duplicate) {
                let root = LocalStoreRoot::open(project.path()).unwrap();
                let mut leases =
                    ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
                trash_generations(
                    &root,
                    &mut leases,
                    &[selected],
                    ProjectStoreLimits::default(),
                    || false,
                )
                .unwrap();
                fs::create_dir_all(active.parent().unwrap()).unwrap();
                fs::write(&active, &selected_bytes).unwrap();
            }
            let files_before = all_file_bytes(project.path());
            let directories_before = all_directories(project.path());
            let refs_before = all_file_bytes(&project.path().join("refs"));
            let objects_before = all_file_bytes(&project.path().join("objects"));
            let injector = GcTransitionInjector::failing(GcTransitionTarget {
                transition,
                edge,
                occurrence,
            });
            let root = LocalStoreRoot::open(project.path()).unwrap();
            let mut leases =
                ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
            leases.set_gc_transition_injector(Arc::clone(&injector));
            let result = trash_generations(
                &root,
                &mut leases,
                &[selected],
                ProjectStoreLimits::default(),
                || false,
            );
            assert_eq!(
                injector.fired(),
                1,
                "{} {} occurrence {occurrence} was not reached",
                transition.name(),
                edge.name()
            );
            if indeterminate {
                assert_eq!(result, Err(ProjectStoreFault::CommitIndeterminate));
                assert!(matches!(
                    leases.confirm_writer(&root),
                    Err(LeaseError::Indeterminate)
                ));
                if transition == GcTransition::MaintenanceRestore {
                    assert_eq!(leases.maintenance_lost(), edge == TransitionEdge::Before);
                }
            } else {
                assert!(matches!(result, Err(ProjectStoreFault::Corruption { .. })));
                assert!(leases.confirm_writer(&root).unwrap());
                assert_eq!(all_file_bytes(project.path()), files_before);
                assert_eq!(all_directories(project.path()), directories_before);
            }
            drop(leases);

            let mut retry_leases =
                ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
            trash_generations(
                &root,
                &mut retry_leases,
                &[selected],
                ProjectStoreLimits::default(),
                || false,
            )
            .unwrap();
            assert!(!active.exists());
            assert_eq!(fs::read(&trash).unwrap(), selected_bytes);
            assert_eq!(all_file_bytes(&project.path().join("refs")), refs_before);
            assert_eq!(
                all_file_bytes(&project.path().join("objects")),
                objects_before
            );
            let zero_mutation = trash_generations(
                &root,
                &mut retry_leases,
                &[selected],
                ProjectStoreLimits::default(),
                || false,
            )
            .unwrap();
            assert_eq!(zero_mutation.published_objects, 0);
            assert_eq!(zero_mutation.streamed_bytes, 0);
        }
    }

    #[test]
    fn unsafe_selection_and_symlinked_inventory_do_not_mutate() {
        let project = TestProject::extracted("trash-reject", "divergent.m4dproj");
        let selected = generation_id(DIVERGENT_ORPHAN);
        let root = LocalStoreRoot::open(project.path()).unwrap();
        let mut leases =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let before = all_file_bytes(project.path());

        for selection in [Vec::new(), vec![selected, selected]] {
            assert_eq!(
                trash_generations(
                    &root,
                    &mut leases,
                    &selection,
                    ProjectStoreLimits::default(),
                    || false,
                ),
                Err(ProjectStoreFault::Corruption {
                    stage: "trash_selection"
                })
            );
            assert_eq!(all_file_bytes(project.path()), before);
            assert!(!project.path().join("trash").exists());
        }

        assert_eq!(
            trash_generations(
                &root,
                &mut leases,
                &[selected],
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::ConfirmationRequired)
        );
        assert_eq!(all_file_bytes(project.path()), before);
        assert!(!project.path().join("trash").exists());

        let generation = project.path().join(active_generation_path(selected));
        let generation_bytes = fs::read(&generation).unwrap();
        fs::remove_file(&generation).unwrap();
        symlink(project.path().join("project.json"), &generation).unwrap();
        let linked = all_file_bytes(project.path());
        assert!(matches!(
            trash_generations(
                &root,
                &mut leases,
                &[selected],
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::Corruption { .. })
        ));
        assert_eq!(all_file_bytes(project.path()), linked);
        assert!(!project.path().join("trash").exists());

        fs::remove_file(&generation).unwrap();
        fs::write(&generation, &generation_bytes).unwrap();
        let foreign = install_foreign_lineage_retry(project.path(), &generation_bytes);
        let foreign_before = all_file_bytes(project.path());
        assert_eq!(
            trash_generations(
                &root,
                &mut leases,
                &[foreign],
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::Corruption {
                stage: "trash_generation_lineage"
            })
        );
        assert_eq!(all_file_bytes(project.path()), foreign_before);
    }

    struct PreparedPurge {
        generation_path: PathBuf,
        objects: Vec<(PathBuf, PathBuf, Vec<u8>)>,
        streamed_bytes: u64,
    }

    fn prepare_shared_object_purge(root_path: &Path, object_count: usize) -> PreparedPurge {
        let selected = install_zero_non_regenerable_orphan(root_path);
        let root = LocalStoreRoot::open(root_path).unwrap();
        let mut leases =
            ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        trash_generations(
            &root,
            &mut leases,
            &[selected],
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        drop(leases);
        drop(root);

        let generation_path = root_path.join(trash_generation_path(selected));
        let generation_bytes = fs::read(&generation_path).unwrap();
        let document = serde_json::from_slice::<Value>(&generation_bytes).unwrap();
        let mut candidates = document["reachable_objects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|object| {
                let digest = ExactBytesDigest::parse(object["digest"].as_str().unwrap()).unwrap();
                let byte_length = object["byte_length"]
                    .as_str()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap();
                (digest, byte_length)
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|(_, byte_length)| *byte_length);
        candidates.truncate(object_count);
        assert_eq!(candidates.len(), object_count);
        candidates.sort_unstable_by_key(|(digest, _)| *digest);

        let mut objects = Vec::new();
        for (digest, _) in candidates {
            let active = root_path.join(active_object_path(digest));
            let trash = root_path.join(trash_object_path(digest));
            let bytes = fs::read(&active).unwrap();
            fs::create_dir_all(trash.parent().unwrap()).unwrap();
            fs::write(&trash, &bytes).unwrap();
            objects.push((active, trash, bytes));
        }
        let present_bytes = u64::try_from(generation_bytes.len()).unwrap()
            + objects
                .iter()
                .map(|(_, _, bytes)| u64::try_from(bytes.len()).unwrap())
                .sum::<u64>();
        PreparedPurge {
            generation_path,
            objects,
            streamed_bytes: present_bytes * 2,
        }
    }

    fn quarantine_generation_only(root: &Path, generation_id: ProjectGenerationId) {
        let active = root.join(active_generation_path(generation_id));
        let trash = root.join(trash_generation_path(generation_id));
        fs::create_dir_all(trash.parent().unwrap()).unwrap();
        fs::rename(active, trash).unwrap();
    }

    pub(crate) fn install_unique_regenerable_orphan(root: &Path) -> (ProjectGenerationId, PathBuf) {
        install_unique_regenerable_orphan_inner(root, true)
    }

    fn install_unique_regenerable_orphan_inner(
        root: &Path,
        remove_source: bool,
    ) -> (ProjectGenerationId, PathBuf) {
        let old = generation_id(RECOVERABLE_ORPHAN);
        let old_path = root.join(active_generation_path(old));
        let mut document = serde_json::from_slice::<Value>(&fs::read(&old_path).unwrap()).unwrap();
        let artifacts = document["artifacts"].as_array_mut().unwrap();
        artifacts.retain(|artifact| {
            artifact.get("recoverability").and_then(Value::as_str) == Some("regenerable")
        });
        assert_eq!(artifacts.len(), 1);
        let object_bytes = b"unique regenerable purge object";
        let digest = ExactBytesHasher::hash(object_bytes).unwrap().digest();
        let byte_length = u64::try_from(object_bytes.len()).unwrap();
        let artifact = &mut artifacts[0];
        artifact["logical_object"]["digest"] = Value::String(digest.to_string());
        artifact["logical_object"]["byte_length"] = Value::String(byte_length.to_string());
        artifact["storage"] = serde_json::json!({
            "kind": "direct",
            "object": {
                "byte_length": byte_length.to_string(),
                "digest": digest.to_string(),
            }
        });
        document["reachable_objects"] = serde_json::json!([{
            "byte_length": byte_length.to_string(),
            "digest": digest.to_string(),
        }]);
        let canonical = wire::encode_canonical_json(&document).unwrap();
        let selected = wire::generation_id_from_validated_canonical(&canonical).unwrap();
        let selected_path = root.join(active_generation_path(selected));
        fs::create_dir_all(selected_path.parent().unwrap()).unwrap();
        fs::write(&selected_path, canonical).unwrap();
        if remove_source {
            fs::remove_file(old_path).unwrap();
        }
        let object_path = root.join(active_object_path(digest));
        fs::create_dir_all(object_path.parent().unwrap()).unwrap();
        fs::write(&object_path, object_bytes).unwrap();
        (selected, object_path)
    }

    pub(crate) fn install_two_safe_orphans(root: &Path) -> [ProjectGenerationId; 2] {
        let (normal, _) = install_unique_regenerable_orphan_inner(root, false);
        let collision = install_zero_non_regenerable_orphan(root);
        assert_ne!(normal, collision);
        [normal, collision]
    }

    pub(crate) fn install_zero_non_regenerable_orphan(root: &Path) -> ProjectGenerationId {
        let old = generation_id(RECOVERABLE_ORPHAN);
        let old_path = root.join(active_generation_path(old));
        let mut document = serde_json::from_slice::<Value>(&fs::read(&old_path).unwrap()).unwrap();
        let artifacts = document
            .get_mut("artifacts")
            .and_then(Value::as_array_mut)
            .unwrap();
        artifacts.retain(|artifact| {
            artifact.get("handle_id").and_then(Value::as_str)
                != Some("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee")
        });
        assert_eq!(artifacts.len(), 1);
        let reachable = document
            .get_mut("reachable_objects")
            .and_then(Value::as_array_mut)
            .unwrap();
        reachable.retain(|object| {
            object.get("digest").and_then(Value::as_str)
                != Some("sha256:f317b2208b90efc088e10edda67cef73f8cedda059cb53538183fa94e12df94d")
        });
        assert_eq!(reachable.len(), 3);

        let canonical = wire::encode_canonical_json(&document).unwrap();
        assert_eq!(canonical.len(), 4_316);
        let selected = wire::generation_id_from_validated_canonical(&canonical).unwrap();
        assert_eq!(selected, generation_id(ZERO_NON_REGENERABLE_ORPHAN));
        let selected_path = root.join(active_generation_path(selected));
        fs::create_dir_all(selected_path.parent().unwrap()).unwrap();
        fs::write(selected_path, canonical).unwrap();
        fs::remove_file(old_path).unwrap();
        selected
    }

    pub(crate) fn active_generation_file(
        root: &Path,
        generation_id: ProjectGenerationId,
    ) -> PathBuf {
        root.join(active_generation_path(generation_id))
    }

    pub(crate) fn trash_generation_file(
        root: &Path,
        generation_id: ProjectGenerationId,
    ) -> PathBuf {
        root.join(trash_generation_path(generation_id))
    }

    fn install_foreign_lineage_retry(root: &Path, generation_bytes: &[u8]) -> ProjectGenerationId {
        let mut document = serde_json::from_slice::<Value>(generation_bytes).unwrap();
        document["dataset"]["scientific_content_id"] =
            Value::String(format!("m4d-sc-v1-sha256:{}", "9".repeat(64)));
        let canonical = wire::encode_canonical_json(&document).unwrap();
        let generation_id = wire::generation_id_from_validated_canonical(&canonical).unwrap();
        let path = root.join(trash_generation_path(generation_id));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, canonical).unwrap();
        generation_id
    }

    pub(crate) fn install_anonymous_object(root: &Path) -> PathBuf {
        let bytes = b"anonymous-unrooted-object";
        let mut hasher = ExactBytesHasher::new();
        hasher.update(bytes).unwrap();
        let digest = hasher.finalize().unwrap().digest();
        let path = root.join(active_object_path(digest));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    fn generation_id(value: &str) -> ProjectGenerationId {
        ProjectGenerationId::parse(value).unwrap()
    }

    fn all_file_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(root: &Path, current: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(current).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if entry.file_type().unwrap().is_dir() {
                    visit(root, &path, files);
                } else {
                    files.insert(
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        fs::read(path).unwrap(),
                    );
                }
            }
        }
        let mut files = BTreeMap::new();
        visit(root, root, &mut files);
        files
    }

    fn all_directories(root: &Path) -> BTreeSet<PathBuf> {
        fn visit(root: &Path, current: &Path, directories: &mut BTreeSet<PathBuf>) {
            for entry in fs::read_dir(current).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    let path = entry.path();
                    directories.insert(path.strip_prefix(root).unwrap().to_path_buf());
                    visit(root, &path, directories);
                }
            }
        }

        let mut directories = BTreeSet::new();
        visit(root, root, &mut directories);
        directories
    }

    struct TestProject(PathBuf);

    impl TestProject {
        fn extracted(label: &str, store: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "mirante4d-trash-{label}-{}-{nonce}-{}.m4dproj",
                std::process::id(),
                TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            let output = Command::new("tar")
                .arg("-xzf")
                .arg(fixture_archive())
                .arg("-C")
                .arg(&path)
                .arg("--strip-components=1")
                .arg(store)
                .output()
                .unwrap();
            assert!(output.status.success(), "failed to extract {store}");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestProject {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture_archive() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/project/project-store-v1.tar.gz")
    }
}
