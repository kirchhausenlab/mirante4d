//! Generation-last publication and the first fresh-store manual authority.
//!
//! This private B2 slice can publish an initial manual head only when every
//! lane ref is absent and a held writer lease is confirmed. Established-head
//! replacement remains deferred until the frozen recovery/head crash-state
//! rule is corrected. Actor execution, recovery, autosave, and garbage
//! collection are still outside this module.

#![cfg(target_os = "linux")]
#![cfg_attr(not(test), allow(dead_code))]

use std::{
    collections::{BTreeMap, btree_map::Entry},
    io::{self, Cursor, Read},
};

use mirante4d_identity::{ExactBytesDigest, ExactBytesHasher, RawObjectDescriptor};
use thiserror::Error;

use crate::{
    ProjectCommitCapture, ProjectGenerationId, ProjectStoreFault, ProjectStoreLimits,
    ProjectStoreReceipt,
    api::CapturedObjectSource,
    generation::{
        ArtifactStorage, EncodedGeneration, GenerationCodecError, GenerationDocument,
        GenerationKind, LogicalObjectBinding, LogicalObjectPage, PAGE_BYTES, PhysicalObject,
    },
    lease::{LeaseError, ProjectStoreLeases},
    local::{ImmutablePublication, LocalPublicationError, LocalStoreRoot, PublicationDisposition},
    wire::{RefKind, RefRecord},
};

const STREAM_BUFFER_BYTES_MAX: usize = 1_048_576;
const PAGE_RECORDS_MAX: u64 = 65_536;

#[derive(Debug, Error)]
pub(crate) enum GenerationTransactionError {
    #[error("generation publication was cancelled before any authority changed")]
    Cancelled,
    #[error("a captured logical object changed while it was streamed")]
    SourceChanged,
    #[error("a captured logical object source could not be opened or read")]
    SourceIo,
    #[error("generation publication exceeded the bound for {stage}")]
    Capacity { stage: &'static str },
    #[error("the physical object closure is invalid")]
    Closure,
    #[error(transparent)]
    Codec(#[from] GenerationCodecError),
    #[error(transparent)]
    Local(LocalPublicationError),
}

impl From<LocalPublicationError> for GenerationTransactionError {
    fn from(error: LocalPublicationError) -> Self {
        match error {
            LocalPublicationError::Cancelled => Self::Cancelled,
            LocalPublicationError::SourceLength { .. } | LocalPublicationError::SourceDigest => {
                Self::SourceChanged
            }
            LocalPublicationError::Capacity { .. } => Self::Capacity {
                stage: "physical object",
            },
            other => Self::Local(other),
        }
    }
}

#[derive(Debug)]
pub(crate) struct UnreferencedGenerationPublication {
    document: GenerationDocument,
    generation: EncodedGeneration,
    generation_disposition: PublicationDisposition,
    created_objects: u64,
    created_object_bytes: u64,
}

impl UnreferencedGenerationPublication {
    pub(crate) const fn generation_id(&self) -> ProjectGenerationId {
        self.generation.id()
    }

    pub(crate) fn generation_bytes(&self) -> &[u8] {
        self.generation.bytes()
    }

    pub(crate) fn document(&self) -> &GenerationDocument {
        &self.document
    }

    pub(crate) const fn generation_disposition(&self) -> PublicationDisposition {
        self.generation_disposition
    }

    pub(crate) const fn created_objects(&self) -> u64 {
        self.created_objects
    }

    pub(crate) const fn created_object_bytes(&self) -> u64 {
        self.created_object_bytes
    }
}

/// Publishes the first manual head into an already prepared, unpublished store
/// root. This is not the public Create command: envelope creation, package
/// installation, established-head replacement, and actor ownership are not
/// implemented by this checkpoint.
pub(crate) fn publish_initial_manual_generation<C>(
    root: &LocalStoreRoot,
    leases: &ProjectStoreLeases,
    capture: ProjectCommitCapture,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<ProjectStoreReceipt, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let limits = limits.validate()?;
    if !leases.confirm_writer(root).map_err(map_lease_error)? {
        return Err(ProjectStoreFault::ReadOnly);
    }
    let project_id = capture.projection().state().project_id();
    let envelope = root
        .read_project_envelope(limits, &mut is_cancelled)
        .map_err(|error| map_local_error(error, "project_envelope"))?;
    if envelope.project_id() != project_id {
        return Err(ProjectStoreFault::Corruption {
            stage: "project_envelope_identity",
        });
    }
    if capture.expected_parent().is_some() {
        return Err(ProjectStoreFault::StaleParent);
    }
    if capture.autosave_base().is_some() || capture.forked_from().is_some() {
        return Err(ProjectStoreFault::Corruption {
            stage: "initial_manual_capture",
        });
    }
    require_fresh_lane_refs(root, project_id, limits, &mut is_cancelled)?;

    let captured_revision = capture.projection().revision();
    let captured_revision_high_water = capture.projection().revision_high_water().clone();
    let publication = publish_unreferenced_generation(
        root,
        capture,
        GenerationKind::Manual,
        0,
        limits,
        &mut is_cancelled,
    )
    .map_err(map_generation_error)?;

    if !leases.confirm_writer(root).map_err(map_lease_error)? {
        return Err(ProjectStoreFault::ReadOnly);
    }
    require_fresh_lane_refs(root, project_id, limits, &mut is_cancelled)?;
    check_cancelled(&mut is_cancelled).map_err(map_generation_error)?;
    let head = RefRecord::new(
        RefKind::ManualHead,
        project_id,
        publication.generation_id(),
        None,
        None,
    )
    .map_err(|_| ProjectStoreFault::Corruption {
        stage: "initial_manual_head",
    })?;
    if let Err(error) = root.publish_initial_manual_head(head, limits, &mut is_cancelled) {
        if matches!(error, LocalPublicationError::RefCommitIndeterminate) {
            leases.suspend_writes();
        }
        return Err(map_local_error(error, "initial_manual_head"));
    }

    Ok(ProjectStoreReceipt::initial_manual(
        captured_revision,
        captured_revision_high_water,
        publication.generation_id(),
        publication.created_objects(),
        publication.created_object_bytes(),
    ))
}

fn require_fresh_lane_refs<C>(
    root: &LocalStoreRoot,
    project_id: mirante4d_project_model::ProjectId,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    if let Some(head) = root
        .read_ref(RefKind::ManualHead, limits, &mut *is_cancelled)
        .map_err(|error| map_local_error(error, "manual_head"))?
    {
        if head.project_id() != project_id {
            return Err(ProjectStoreFault::Corruption {
                stage: "manual_head_identity",
            });
        }
        return Err(ProjectStoreFault::StaleParent);
    }
    for (kind, stage) in [
        (RefKind::ManualRecovery, "manual_recovery"),
        (RefKind::AutosaveHead, "autosave_head"),
        (RefKind::AutosaveRecovery, "autosave_recovery"),
    ] {
        if root
            .read_ref(kind, limits, &mut *is_cancelled)
            .map_err(|error| map_local_error(error, stage))?
            .is_some()
        {
            return Err(ProjectStoreFault::Corruption { stage });
        }
    }
    root.require_empty_ref_namespace(limits, &mut *is_cancelled)
        .map_err(|error| map_local_error(error, "initial_ref_namespace"))?;
    Ok(())
}

fn map_lease_error(error: LeaseError) -> ProjectStoreFault {
    match error {
        LeaseError::Indeterminate => ProjectStoreFault::CommitIndeterminate,
        LeaseError::InvalidAnchor | LeaseError::Io { .. } => ProjectStoreFault::Corruption {
            stage: "writer_lease",
        },
    }
}

fn map_generation_error(error: GenerationTransactionError) -> ProjectStoreFault {
    match error {
        GenerationTransactionError::Cancelled => ProjectStoreFault::Cancelled,
        GenerationTransactionError::SourceChanged | GenerationTransactionError::SourceIo => {
            ProjectStoreFault::SourceChanged
        }
        GenerationTransactionError::Capacity { stage } => ProjectStoreFault::Capacity { stage },
        GenerationTransactionError::Closure | GenerationTransactionError::Codec(_) => {
            ProjectStoreFault::Corruption {
                stage: "generation_closure",
            }
        }
        GenerationTransactionError::Local(error) => map_local_error(error, "generation_io"),
    }
}

fn map_local_error(error: LocalPublicationError, stage: &'static str) -> ProjectStoreFault {
    match error {
        LocalPublicationError::Cancelled => ProjectStoreFault::Cancelled,
        LocalPublicationError::Capacity { .. } => ProjectStoreFault::Capacity { stage },
        LocalPublicationError::SourceLength { .. } => ProjectStoreFault::SourceChanged,
        LocalPublicationError::SourceDigest => ProjectStoreFault::DigestMismatch,
        LocalPublicationError::RefAlreadyPresent => ProjectStoreFault::StaleParent,
        LocalPublicationError::RefCommitIndeterminate => ProjectStoreFault::CommitIndeterminate,
        LocalPublicationError::AtomicPublishUnsupported => ProjectStoreFault::UnsupportedFilesystem,
        LocalPublicationError::InvalidPath
        | LocalPublicationError::ExistingMismatch
        | LocalPublicationError::InvalidGeneration
        | LocalPublicationError::InvalidControl
        | LocalPublicationError::Io { .. } => ProjectStoreFault::Corruption { stage },
    }
}

pub(crate) fn publish_unreferenced_generation<C>(
    root: &LocalStoreRoot,
    capture: ProjectCommitCapture,
    kind: GenerationKind,
    generation_sequence: u64,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<UnreferencedGenerationPublication, GenerationTransactionError>
where
    C: FnMut() -> bool,
{
    let limits = limits
        .validate()
        .map_err(|_| GenerationTransactionError::Capacity {
            stage: "configuration",
        })?;
    check_cancelled(&mut is_cancelled)?;
    let parts = capture.into_parts();

    let logical = unique_logical_descriptors(&parts.projection)?;
    preflight(&logical, limits)?;
    let mut sources = parts
        .object_sources
        .into_iter()
        .map(|source| (source.descriptor().digest(), source))
        .collect::<BTreeMap<_, _>>();
    if sources.len() != logical.len() {
        return Err(GenerationTransactionError::Closure);
    }

    let mut bindings = BTreeMap::new();
    let mut closure = BTreeMap::new();
    let mut created_objects = 0_u64;
    let mut created_object_bytes = 0_u64;

    for (digest, descriptor) in logical {
        check_cancelled(&mut is_cancelled)?;
        let source = sources
            .remove(&digest)
            .ok_or(GenerationTransactionError::Closure)?;
        let storage = if descriptor.byte_length() <= PAGE_BYTES {
            let object = PhysicalObject::new(descriptor.digest(), descriptor.byte_length());
            insert_closure(&mut closure, object, limits)?;
            if !root.durabilize_object_if_present(
                object.digest(),
                object.byte_length(),
                limits.object_or_page_bytes_max,
                &mut is_cancelled,
            )? {
                let mut reader = source
                    .open()
                    .map_err(|_| GenerationTransactionError::SourceIo)?;
                let publication = root.publish_declared_object(
                    &mut reader,
                    object.digest(),
                    object.byte_length(),
                    limits.object_or_page_bytes_max,
                    &mut is_cancelled,
                )?;
                record_created(publication, &mut created_objects, &mut created_object_bytes)?;
            }
            ArtifactStorage::direct(&descriptor)?
        } else {
            let pages = derive_page_plan(&source, &descriptor, limits, &mut is_cancelled)?;
            let binding = LogicalObjectBinding::new(descriptor.clone(), pages, limits)?;
            let encoded_binding = binding.encode(limits)?;

            for page in binding.pages() {
                insert_closure(&mut closure, page.object(), limits)?;
            }
            let binding_object = PhysicalObject::new(
                encoded_binding.descriptor().digest(),
                encoded_binding.descriptor().byte_length(),
            );
            insert_closure(&mut closure, binding_object, limits)?;

            let mut reader = source
                .open()
                .map_err(|_| GenerationTransactionError::SourceIo)?;
            for page in binding.pages() {
                let page = page.object();
                let publication = root.publish_consuming_object(
                    &mut reader,
                    page.digest(),
                    page.byte_length(),
                    limits.object_or_page_bytes_max,
                    &mut is_cancelled,
                )?;
                record_created(publication, &mut created_objects, &mut created_object_bytes)?;
            }
            require_eof(&mut reader)?;

            let mut binding_reader = Cursor::new(encoded_binding.bytes());
            let publication = root.publish_declared_object(
                &mut binding_reader,
                binding_object.digest(),
                binding_object.byte_length(),
                limits.object_or_page_bytes_max,
                &mut is_cancelled,
            )?;
            record_created(publication, &mut created_objects, &mut created_object_bytes)?;
            ArtifactStorage::paged(&descriptor, encoded_binding.descriptor().clone())?
        };
        bindings.insert(digest, storage);
    }
    if !sources.is_empty() {
        return Err(GenerationTransactionError::Closure);
    }

    let reachable_objects = closure.values().copied().collect::<Vec<_>>();
    let document = GenerationDocument::build_from_projection(
        parts.projection,
        parts.expected_parent,
        parts.autosave_base,
        parts.forked_from,
        kind,
        generation_sequence,
        bindings,
        reachable_objects,
        limits,
    )?;
    validate_physical_closure(root, &document, limits, &mut is_cancelled)?;

    let generation = document.encode(limits)?;
    let checked = GenerationDocument::decode(
        generation.id(),
        document.projection().state().project_id(),
        generation.bytes(),
        limits,
    )?;
    if checked != document {
        return Err(GenerationTransactionError::Closure);
    }
    check_cancelled(&mut is_cancelled)?;
    let generation_publication =
        root.publish_generation(&generation, limits.generation_bytes_max, &mut is_cancelled)?;

    Ok(UnreferencedGenerationPublication {
        document,
        generation,
        generation_disposition: generation_publication.disposition(),
        created_objects,
        created_object_bytes,
    })
}

fn unique_logical_descriptors(
    projection: &mirante4d_project_model::ProjectGenerationProjection,
) -> Result<BTreeMap<ExactBytesDigest, RawObjectDescriptor>, GenerationTransactionError> {
    let mut descriptors = BTreeMap::new();
    for artifact in projection.state().artifacts() {
        match descriptors.entry(artifact.object().digest()) {
            Entry::Vacant(entry) => {
                entry.insert(artifact.object().clone());
            }
            Entry::Occupied(entry) if entry.get() == artifact.object() => {}
            Entry::Occupied(_) => return Err(GenerationTransactionError::Closure),
        }
    }
    Ok(descriptors)
}

fn preflight(
    logical: &BTreeMap<ExactBytesDigest, RawObjectDescriptor>,
    limits: ProjectStoreLimits,
) -> Result<(), GenerationTransactionError> {
    if logical.len() > limits.artifact_records_per_generation_max {
        return Err(GenerationTransactionError::Capacity {
            stage: "logical objects",
        });
    }
    let mut minimum_closure = 0_usize;
    for descriptor in logical.values() {
        if descriptor.byte_length() <= PAGE_BYTES {
            minimum_closure =
                minimum_closure
                    .checked_add(1)
                    .ok_or(GenerationTransactionError::Capacity {
                        stage: "physical closure",
                    })?;
        } else {
            let page_count = descriptor.byte_length().div_ceil(PAGE_BYTES);
            if page_count > PAGE_RECORDS_MAX {
                return Err(GenerationTransactionError::Capacity {
                    stage: "logical object pages",
                });
            }
            minimum_closure =
                minimum_closure
                    .checked_add(2)
                    .ok_or(GenerationTransactionError::Capacity {
                        stage: "physical closure",
                    })?;
            if limits.object_or_page_bytes_max < PAGE_BYTES {
                return Err(GenerationTransactionError::Capacity {
                    stage: "page geometry",
                });
            }
        }
    }
    if minimum_closure > limits.reachable_objects_per_generation_max {
        return Err(GenerationTransactionError::Capacity {
            stage: "physical closure",
        });
    }
    Ok(())
}

fn derive_page_plan<C>(
    source: &CapturedObjectSource,
    logical: &RawObjectDescriptor,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<Vec<LogicalObjectPage>, GenerationTransactionError>
where
    C: FnMut() -> bool,
{
    let page_count = logical.byte_length().div_ceil(PAGE_BYTES);
    if page_count == 0 || page_count > PAGE_RECORDS_MAX {
        return Err(GenerationTransactionError::Capacity {
            stage: "logical object pages",
        });
    }
    let page_capacity =
        usize::try_from(page_count).map_err(|_| GenerationTransactionError::Capacity {
            stage: "logical object pages",
        })?;
    let buffer_bytes = limits.stream_buffer_bytes_max.min(STREAM_BUFFER_BYTES_MAX);
    let mut buffer = vec![0_u8; buffer_bytes];
    let mut reader = source
        .open()
        .map_err(|_| GenerationTransactionError::SourceIo)?;
    let mut logical_hasher = ExactBytesHasher::new();
    let mut pages = Vec::with_capacity(page_capacity);
    let mut remaining = logical.byte_length();
    let mut offset = 0_u64;
    for ordinal in 0..page_count {
        let page_bytes = remaining.min(PAGE_BYTES);
        let mut page_hasher = ExactBytesHasher::new();
        let mut page_remaining = page_bytes;
        while page_remaining != 0 {
            check_cancelled(is_cancelled)?;
            let request = usize::try_from(page_remaining.min(buffer.len() as u64))
                .expect("the request is bounded by the allocated buffer");
            let read = read_some(&mut reader, &mut buffer[..request])?;
            if read == 0 {
                return Err(GenerationTransactionError::SourceChanged);
            }
            page_hasher
                .update(&buffer[..read])
                .map_err(|_| GenerationTransactionError::SourceChanged)?;
            logical_hasher
                .update(&buffer[..read])
                .map_err(|_| GenerationTransactionError::SourceChanged)?;
            page_remaining -= u64::try_from(read).expect("a read byte count fits u64");
        }
        let facts = page_hasher
            .finalize()
            .map_err(|_| GenerationTransactionError::SourceChanged)?;
        pages.push(LogicalObjectPage::new(
            u32::try_from(ordinal).map_err(|_| GenerationTransactionError::Capacity {
                stage: "page ordinal",
            })?,
            offset,
            PhysicalObject::new(facts.digest(), facts.byte_length()),
        ));
        offset = offset
            .checked_add(page_bytes)
            .ok_or(GenerationTransactionError::Capacity {
                stage: "page offsets",
            })?;
        remaining -= page_bytes;
    }
    require_eof(&mut reader)?;
    let facts = logical_hasher
        .finalize()
        .map_err(|_| GenerationTransactionError::SourceChanged)?;
    if facts.digest() != logical.digest() || facts.byte_length() != logical.byte_length() {
        return Err(GenerationTransactionError::SourceChanged);
    }
    Ok(pages)
}

fn validate_physical_closure<C>(
    root: &LocalStoreRoot,
    document: &GenerationDocument,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<(), GenerationTransactionError>
where
    C: FnMut() -> bool,
{
    let logical = unique_logical_descriptors(document.projection())?;
    let mut observed = BTreeMap::new();
    for (digest, storage) in document.bindings() {
        let descriptor = logical
            .get(digest)
            .ok_or(GenerationTransactionError::Closure)?;
        match storage {
            ArtifactStorage::Direct { object } => {
                root.read_exact_object(
                    object.digest(),
                    object.byte_length(),
                    limits.object_or_page_bytes_max,
                    &mut *is_cancelled,
                    |_| {},
                )?;
                insert_closure(&mut observed, *object, limits)?;
            }
            ArtifactStorage::Paged { binding_manifest } => {
                let capacity = usize::try_from(binding_manifest.byte_length()).map_err(|_| {
                    GenerationTransactionError::Capacity {
                        stage: "binding bytes",
                    }
                })?;
                let mut binding_bytes = Vec::with_capacity(capacity);
                root.read_exact_object(
                    binding_manifest.digest(),
                    binding_manifest.byte_length(),
                    limits.object_or_page_bytes_max,
                    &mut *is_cancelled,
                    |bytes| binding_bytes.extend_from_slice(bytes),
                )?;
                let binding = LogicalObjectBinding::decode(
                    &binding_bytes,
                    descriptor,
                    binding_manifest,
                    limits,
                )?;
                let mut logical_hasher = ExactBytesHasher::new();
                for page in binding.pages() {
                    let page = page.object();
                    root.read_exact_object(
                        page.digest(),
                        page.byte_length(),
                        limits.object_or_page_bytes_max,
                        &mut *is_cancelled,
                        |bytes| {
                            logical_hasher
                                .update(bytes)
                                .expect("validated page closure cannot overflow u64")
                        },
                    )?;
                    insert_closure(&mut observed, page, limits)?;
                }
                let facts = logical_hasher
                    .finalize()
                    .map_err(|_| GenerationTransactionError::Closure)?;
                if facts.digest() != descriptor.digest()
                    || facts.byte_length() != descriptor.byte_length()
                {
                    return Err(GenerationTransactionError::Closure);
                }
                insert_closure(
                    &mut observed,
                    PhysicalObject::new(binding_manifest.digest(), binding_manifest.byte_length()),
                    limits,
                )?;
            }
        }
    }
    let observed = observed.values().copied().collect::<Vec<_>>();
    if observed != document.reachable_objects() {
        return Err(GenerationTransactionError::Closure);
    }
    Ok(())
}

fn insert_closure(
    closure: &mut BTreeMap<ExactBytesDigest, PhysicalObject>,
    object: PhysicalObject,
    limits: ProjectStoreLimits,
) -> Result<(), GenerationTransactionError> {
    if let Some(existing) = closure.get(&object.digest()) {
        return if *existing == object {
            Ok(())
        } else {
            Err(GenerationTransactionError::Closure)
        };
    }
    if closure.len() >= limits.reachable_objects_per_generation_max {
        return Err(GenerationTransactionError::Capacity {
            stage: "physical closure",
        });
    }
    closure.insert(object.digest(), object);
    Ok(())
}

fn record_created(
    publication: ImmutablePublication,
    created_objects: &mut u64,
    created_bytes: &mut u64,
) -> Result<(), GenerationTransactionError> {
    if publication.disposition() == PublicationDisposition::Created {
        *created_objects =
            created_objects
                .checked_add(1)
                .ok_or(GenerationTransactionError::Capacity {
                    stage: "publication counters",
                })?;
        *created_bytes = created_bytes
            .checked_add(publication.facts().byte_length())
            .ok_or(GenerationTransactionError::Capacity {
                stage: "publication counters",
            })?;
    }
    Ok(())
}

fn check_cancelled(
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<(), GenerationTransactionError> {
    if is_cancelled() {
        Err(GenerationTransactionError::Cancelled)
    } else {
        Ok(())
    }
}

fn read_some(
    reader: &mut dyn Read,
    buffer: &mut [u8],
) -> Result<usize, GenerationTransactionError> {
    loop {
        match reader.read(buffer) {
            Ok(read) => return Ok(read),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(GenerationTransactionError::SourceIo),
        }
    }
}

fn require_eof(reader: &mut dyn Read) -> Result<(), GenerationTransactionError> {
    let mut byte = [0_u8; 1];
    if read_some(reader, &mut byte)? == 0 {
        Ok(())
    } else {
        Err(GenerationTransactionError::SourceChanged)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Cursor,
        path::{Path, PathBuf},
        process::Command,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    use mirante4d_domain::{
        CameraView, CrossSectionView, DisplayWindow, IsoLightState, LayerTransfer, LogicalLayerKey,
        Opacity, Projection, RenderState, RgbColor, SamplingPolicy, TimeIndex, TransferCurve,
        UnitQuaternion, ViewerLayout, WorldPoint3,
    };
    use mirante4d_identity::{
        ArtifactContentId, ExactBytesHasher, MediaType, ObjectRole, ScientificContentId,
    };
    use mirante4d_project_model::{
        ArtifactCompleteness, ArtifactHandleId, ArtifactRecoverability, ArtifactReference,
        ArtifactSchema, DatasetReference, LayerViewState, ProjectGenerationProjection, ProjectId,
        ProjectRevisionHighWater, ProjectRevisionId, ProjectState, ViewState,
    };

    use super::*;
    use crate::{ProjectObjectSource, ProjectOpenMode};

    static TEST_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "mirante4d-project-transaction-{label}-{}-{nonce}-{}",
                std::process::id(),
                TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct MemorySource {
        descriptor: RawObjectDescriptor,
        bytes: Arc<[u8]>,
        opens: Arc<AtomicUsize>,
        flip_second: bool,
    }

    impl ProjectObjectSource for MemorySource {
        fn descriptor(&self) -> &RawObjectDescriptor {
            &self.descriptor
        }

        fn open(&self) -> io::Result<Box<dyn Read + Send>> {
            let open = self.opens.fetch_add(1, Ordering::SeqCst);
            let mut bytes = self.bytes.to_vec();
            if self.flip_second && open == 1 && !bytes.is_empty() {
                bytes[0] ^= 1;
            }
            Ok(Box::new(Cursor::new(bytes)))
        }
    }

    struct MustNotOpen {
        descriptor: RawObjectDescriptor,
    }

    impl ProjectObjectSource for MustNotOpen {
        fn descriptor(&self) -> &RawObjectDescriptor {
            &self.descriptor
        }

        fn open(&self) -> io::Result<Box<dyn Read + Send>> {
            panic!("preflight opened a rejected source")
        }
    }

    struct RepeatedPageSource {
        descriptor: RawObjectDescriptor,
        page: Arc<[u8]>,
        opens: Arc<AtomicUsize>,
    }

    const STALE_PROJECT: &str = "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff";
    const STALE_GENERATION: &str = concat!(
        "m4d-project-generation-v1-sha256:",
        "d5020fa3c69a493b34ffbbf3a67a249354e83e5a6d738479d46c7e301786d2ec"
    );

    impl ProjectObjectSource for RepeatedPageSource {
        fn descriptor(&self) -> &RawObjectDescriptor {
            &self.descriptor
        }

        fn open(&self) -> io::Result<Box<dyn Read + Send>> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(
                Cursor::new(self.page.clone()).chain(Cursor::new(self.page.clone())),
            ))
        }
    }

    #[test]
    fn two_pass_paging_publishes_the_exact_closure_before_the_generation() {
        let directory = TestDirectory::new("success");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let generation_member = concat!(
            "recoverable.m4dproj/generations/sha256/50/",
            "fc92ea0e67a54336658f1638596642f17177ceb72c3afbc364c941e6a9b854.json"
        );
        let generation_bytes = fixture_extract(generation_member);
        let expected_id = ProjectGenerationId::parse(concat!(
            "m4d-project-generation-v1-sha256:",
            "50fc92ea0e67a54336658f1638596642f17177ceb72c3afbc364c941e6a9b854"
        ))
        .unwrap();
        let project_id = ProjectId::parse("11111111-2222-4333-8444-555555555555").unwrap();
        let frozen = GenerationDocument::decode(
            expected_id,
            project_id,
            &generation_bytes,
            ProjectStoreLimits::default(),
        )
        .unwrap();

        let direct_opens = Arc::new(AtomicUsize::new(0));
        let paged_opens = Arc::new(AtomicUsize::new(0));
        let mut sources: Vec<Box<dyn ProjectObjectSource>> = Vec::new();
        for artifact in frozen.projection().state().artifacts() {
            let storage = frozen.bindings().get(&artifact.object().digest()).unwrap();
            let (bytes, opens) = match storage {
                ArtifactStorage::Direct { object } => (
                    Arc::<[u8]>::from(fixture_extract(&fixture_object_member(
                        "recoverable.m4dproj",
                        object.digest(),
                    ))),
                    Arc::clone(&direct_opens),
                ),
                ArtifactStorage::Paged { binding_manifest } => {
                    let binding_bytes = fixture_extract(&fixture_object_member(
                        "recoverable.m4dproj",
                        binding_manifest.digest(),
                    ));
                    let binding = LogicalObjectBinding::decode(
                        &binding_bytes,
                        artifact.object(),
                        binding_manifest,
                        ProjectStoreLimits::default(),
                    )
                    .unwrap();
                    let mut logical = Vec::with_capacity(
                        usize::try_from(artifact.object().byte_length()).unwrap(),
                    );
                    for page in binding.pages() {
                        logical.extend_from_slice(&fixture_extract(&fixture_object_member(
                            "recoverable.m4dproj",
                            page.object().digest(),
                        )));
                    }
                    (Arc::<[u8]>::from(logical), Arc::clone(&paged_opens))
                }
            };
            sources.push(Box::new(MemorySource {
                descriptor: artifact.object().clone(),
                bytes,
                opens,
                flip_second: false,
            }));
        }
        let capture = ProjectCommitCapture::new(
            frozen.projection().clone(),
            frozen.parent_generation_id(),
            frozen.base_manual_generation_id(),
            frozen.forked_from(),
            sources,
        )
        .unwrap();

        let published = publish_unreferenced_generation(
            &root,
            capture,
            frozen.kind(),
            frozen.generation_sequence(),
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(published.generation_id(), expected_id);
        assert_eq!(published.generation_bytes(), generation_bytes);
        assert_eq!(published.document(), &frozen);
        assert_eq!(direct_opens.load(Ordering::SeqCst), 1);
        assert_eq!(paged_opens.load(Ordering::SeqCst), 2);
        assert_eq!(published.document().reachable_objects().len(), 4);
        assert_eq!(published.created_objects(), 4);
        assert!(published.created_object_bytes() > PAGE_BYTES);
        assert_eq!(
            published.generation_disposition(),
            PublicationDisposition::Created
        );
        assert_eq!(
            fs::read(generation_path(directory.path(), published.generation_id())).unwrap(),
            published.generation_bytes()
        );
    }

    #[test]
    fn preflight_and_second_pass_drift_publish_no_generation() {
        let huge_directory = TestDirectory::new("preflight");
        let huge_root = LocalStoreRoot::open(huge_directory.path()).unwrap();
        let huge_length = PAGE_BYTES * (PAGE_RECORDS_MAX + 1);
        let huge_descriptor = RawObjectDescriptor::new(
            mirante4d_identity::ExactBytesDigest::from_digest(
                mirante4d_identity::Sha256Digest::from_bytes([7; 32]),
            ),
            huge_length,
            MediaType::parse(ArtifactSchema::AnalysisTableV1.media_type()).unwrap(),
            ObjectRole::parse(ArtifactSchema::AnalysisTableV1.object_role()).unwrap(),
        );
        let huge_capture = capture(
            vec![artifact(
                3,
                ArtifactSchema::AnalysisTableV1,
                huge_descriptor.clone(),
            )],
            vec![Box::new(MustNotOpen {
                descriptor: huge_descriptor,
            })],
        );
        assert!(matches!(
            publish_unreferenced_generation(
                &huge_root,
                huge_capture,
                GenerationKind::Manual,
                0,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(GenerationTransactionError::Capacity { .. })
        ));
        assert!(!huge_directory.path().join("generations").exists());

        let drift_directory = TestDirectory::new("drift");
        let drift_root = LocalStoreRoot::open(drift_directory.path()).unwrap();
        let bytes = Arc::<[u8]>::from(vec![19_u8; usize::try_from(PAGE_BYTES).unwrap() + 1]);
        let drift_descriptor = descriptor(ArtifactSchema::AnalysisTableV1, &bytes);
        let drift_capture = capture(
            vec![artifact(
                4,
                ArtifactSchema::AnalysisTableV1,
                drift_descriptor.clone(),
            )],
            vec![Box::new(MemorySource {
                descriptor: drift_descriptor,
                bytes,
                opens: Arc::new(AtomicUsize::new(0)),
                flip_second: true,
            })],
        );
        assert!(matches!(
            publish_unreferenced_generation(
                &drift_root,
                drift_capture,
                GenerationKind::Manual,
                0,
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(GenerationTransactionError::SourceChanged)
        ));
        assert!(!drift_directory.path().join("generations").exists());

        let cancelled_directory = TestDirectory::new("cancelled");
        let cancelled_root = LocalStoreRoot::open(cancelled_directory.path()).unwrap();
        let cancelled_bytes =
            Arc::<[u8]>::from(vec![29_u8; usize::try_from(PAGE_BYTES).unwrap() + 1]);
        let cancelled_descriptor = descriptor(ArtifactSchema::AnalysisTableV1, &cancelled_bytes);
        let cancelled_opens = Arc::new(AtomicUsize::new(0));
        let cancelled_capture = capture(
            vec![artifact(
                6,
                ArtifactSchema::AnalysisTableV1,
                cancelled_descriptor.clone(),
            )],
            vec![Box::new(MemorySource {
                descriptor: cancelled_descriptor,
                bytes: cancelled_bytes,
                opens: Arc::clone(&cancelled_opens),
                flip_second: false,
            })],
        );
        let mut polls = 0_u8;
        assert!(matches!(
            publish_unreferenced_generation(
                &cancelled_root,
                cancelled_capture,
                GenerationKind::Manual,
                0,
                ProjectStoreLimits::default(),
                || {
                    polls += 1;
                    polls == 5
                },
            ),
            Err(GenerationTransactionError::Cancelled)
        ));
        assert_eq!(polls, 5);
        assert_eq!(cancelled_opens.load(Ordering::SeqCst), 1);
        assert!(!cancelled_directory.path().join("objects").exists());
        assert!(!cancelled_directory.path().join("generations").exists());
    }

    #[test]
    fn closure_union_reuses_identical_physical_objects() {
        let directory = TestDirectory::new("repeated-page");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let page = Arc::<[u8]>::from(vec![23_u8; usize::try_from(PAGE_BYTES).unwrap()]);
        let mut logical_hasher = ExactBytesHasher::new();
        logical_hasher.update(&page).unwrap();
        logical_hasher.update(&page).unwrap();
        let logical = logical_hasher.finalize().unwrap();
        let descriptor = RawObjectDescriptor::new(
            logical.digest(),
            logical.byte_length(),
            MediaType::parse(ArtifactSchema::AnalysisTableV1.media_type()).unwrap(),
            ObjectRole::parse(ArtifactSchema::AnalysisTableV1.object_role()).unwrap(),
        );
        let opens = Arc::new(AtomicUsize::new(0));
        let capture = capture(
            vec![artifact(
                5,
                ArtifactSchema::AnalysisTableV1,
                descriptor.clone(),
            )],
            vec![Box::new(RepeatedPageSource {
                descriptor,
                page,
                opens: Arc::clone(&opens),
            })],
        );
        let published = publish_unreferenced_generation(
            &root,
            capture,
            GenerationKind::Manual,
            0,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(opens.load(Ordering::SeqCst), 2);
        assert_eq!(published.document().reachable_objects().len(), 2);
        assert_eq!(published.created_objects(), 2);
    }

    #[test]
    fn first_manual_commit_matches_the_frozen_initial_head() {
        let directory = prepared_frozen_root("initial-success");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let leases = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let (frozen_id, frozen, generation_bytes) = frozen_stale_manual();
        let (capture, opens) = frozen_capture("stale.m4dproj", &frozen);

        let receipt = publish_initial_manual_generation(
            &root,
            &leases,
            capture,
            ProjectStoreLimits::default(),
            || false,
        )
        .unwrap();
        assert_eq!(receipt.new_generation_id(), frozen_id);
        assert_eq!(receipt.current_generation_id(), frozen_id);
        assert_eq!(receipt.previous_generation_id(), None);
        assert_eq!(receipt.autosave_base_generation_id(), None);
        assert_eq!(receipt.captured_revision(), frozen.projection().revision());
        assert_eq!(
            receipt.captured_revision_high_water(),
            frozen.projection().revision_high_water()
        );
        assert_eq!(
            receipt.published_objects(),
            u64::try_from(frozen.reachable_objects().len()).unwrap()
        );
        assert_eq!(
            fs::read(generation_path(directory.path(), frozen_id)).unwrap(),
            generation_bytes
        );
        assert_eq!(
            fs::read(directory.path().join("refs/head")).unwrap(),
            fixture_extract("stale.m4dproj/refs/head")
        );
        assert!(!directory.path().join("refs/recovery").exists());
        assert!(!directory.path().join("refs/autosave-head").exists());
        assert!(!directory.path().join("refs/autosave-recovery").exists());
        assert!(opens.load(Ordering::SeqCst) >= frozen.projection().state().artifacts().len());
    }

    #[test]
    fn first_manual_commit_rejects_stale_or_corrupt_head_before_publication() {
        let (_, frozen, _) = frozen_stale_manual();

        let read_only_directory = prepared_frozen_root("initial-read-only");
        let read_only_root = LocalStoreRoot::open(read_only_directory.path()).unwrap();
        let read_only_leases =
            ProjectStoreLeases::acquire(&read_only_root, ProjectOpenMode::ReadOnly).unwrap();
        assert!(matches!(
            publish_initial_manual_generation(
                &read_only_root,
                &read_only_leases,
                rejected_frozen_capture(&frozen),
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::ReadOnly)
        ));
        assert!(!read_only_directory.path().join("objects").exists());
        assert!(!read_only_directory.path().join("generations").exists());

        let stale_directory = prepared_frozen_root("initial-stale");
        fs::create_dir(stale_directory.path().join("refs")).unwrap();
        fs::write(
            stale_directory.path().join("refs/head"),
            fixture_extract("stale.m4dproj/refs/head"),
        )
        .unwrap();
        let stale_root = LocalStoreRoot::open(stale_directory.path()).unwrap();
        let stale_leases =
            ProjectStoreLeases::acquire(&stale_root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(matches!(
            publish_initial_manual_generation(
                &stale_root,
                &stale_leases,
                rejected_frozen_capture(&frozen),
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::StaleParent)
        ));
        assert!(!stale_directory.path().join("objects").exists());
        assert!(!stale_directory.path().join("generations").exists());

        let corrupt_directory = prepared_frozen_root("initial-corrupt");
        fs::create_dir(corrupt_directory.path().join("refs")).unwrap();
        fs::write(
            corrupt_directory.path().join("refs/head"),
            fixture_extract("stale.m4dproj/refs/autosave-head"),
        )
        .unwrap();
        let corrupt_root = LocalStoreRoot::open(corrupt_directory.path()).unwrap();
        let corrupt_leases =
            ProjectStoreLeases::acquire(&corrupt_root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(matches!(
            publish_initial_manual_generation(
                &corrupt_root,
                &corrupt_leases,
                rejected_frozen_capture(&frozen),
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::Corruption { .. })
        ));
        assert!(!corrupt_directory.path().join("objects").exists());
        assert!(!corrupt_directory.path().join("generations").exists());

        let pinned_directory = prepared_frozen_root("initial-pinned");
        fs::create_dir_all(pinned_directory.path().join("refs/pins")).unwrap();
        fs::write(
            pinned_directory.path().join("refs/pins/checkpoint-a"),
            fixture_extract("recoverable.m4dproj/refs/pins/checkpoint-a"),
        )
        .unwrap();
        let pinned_root = LocalStoreRoot::open(pinned_directory.path()).unwrap();
        let pinned_leases =
            ProjectStoreLeases::acquire(&pinned_root, ProjectOpenMode::PreferWritable).unwrap();
        assert!(matches!(
            publish_initial_manual_generation(
                &pinned_root,
                &pinned_leases,
                rejected_frozen_capture(&frozen),
                ProjectStoreLimits::default(),
                || false,
            ),
            Err(ProjectStoreFault::Corruption { .. })
        ));
        assert!(!pinned_directory.path().join("objects").exists());
        assert!(!pinned_directory.path().join("generations").exists());
    }

    #[test]
    fn first_manual_commit_cancellation_after_generation_leaves_no_ref() {
        let directory = prepared_frozen_root("initial-cancel");
        let root = LocalStoreRoot::open(directory.path()).unwrap();
        let leases = ProjectStoreLeases::acquire(&root, ProjectOpenMode::PreferWritable).unwrap();
        let (frozen_id, frozen, _) = frozen_stale_manual();
        let (capture, _) = frozen_capture("stale.m4dproj", &frozen);
        let generation = generation_path(directory.path(), frozen_id);

        assert!(matches!(
            publish_initial_manual_generation(
                &root,
                &leases,
                capture,
                ProjectStoreLimits::default(),
                || generation.exists(),
            ),
            Err(ProjectStoreFault::Cancelled)
        ));
        assert!(generation.exists());
        assert!(!directory.path().join("refs/head").exists());
        assert!(!directory.path().join("refs/recovery").exists());
    }

    fn capture(
        artifacts: Vec<ArtifactReference>,
        sources: Vec<Box<dyn ProjectObjectSource>>,
    ) -> ProjectCommitCapture {
        let project_id = ProjectId::parse("11111111-2222-4333-8444-555555555555").unwrap();
        let layer = LayerViewState::new(
            LogicalLayerKey::new(0),
            true,
            transfer(),
            RenderState::mip(SamplingPolicy::SmoothLinear),
        );
        let camera = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            1.0,
            320.0,
            40.0,
        )
        .unwrap();
        let view = ViewState::new(
            vec![layer],
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            camera,
            ViewerLayout::Single3d,
            CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 1.0, 1.0)
                .unwrap(),
            IsoLightState::attached_camera(),
        )
        .unwrap();
        let dataset = DatasetReference::new(
            ScientificContentId::parse(&format!(
                "{}{}",
                ScientificContentId::PREFIX,
                "13".repeat(32)
            ))
            .unwrap(),
            None,
            None,
            None,
        );
        let state = ProjectState::new(project_id, dataset, view, Vec::new(), artifacts).unwrap();
        let projection = ProjectGenerationProjection::new(
            ProjectRevisionId::initial(project_id),
            ProjectRevisionHighWater::initial(project_id),
            state,
        )
        .unwrap();
        ProjectCommitCapture::new(projection, None, None, None, sources).unwrap()
    }

    fn artifact(
        byte: u8,
        schema: ArtifactSchema,
        descriptor: RawObjectDescriptor,
    ) -> ArtifactReference {
        ArtifactReference::new(
            ArtifactHandleId::from_bytes([byte; 16]),
            schema,
            ArtifactContentId::parse(&format!(
                "{}{}",
                ArtifactContentId::PREFIX,
                format!("{byte:02x}").repeat(32)
            ))
            .unwrap(),
            descriptor,
            None,
            None,
            vec![LogicalLayerKey::new(0)],
            "artifact",
            true,
            ArtifactCompleteness::Complete,
            ArtifactRecoverability::NonRegenerable,
        )
        .unwrap()
    }

    fn descriptor(schema: ArtifactSchema, bytes: &[u8]) -> RawObjectDescriptor {
        let facts = ExactBytesHasher::hash(bytes).unwrap();
        RawObjectDescriptor::new(
            facts.digest(),
            facts.byte_length(),
            MediaType::parse(schema.media_type()).unwrap(),
            ObjectRole::parse(schema.object_role()).unwrap(),
        )
    }

    fn transfer() -> LayerTransfer {
        LayerTransfer::new(
            DisplayWindow::new(0.0, 1.0).unwrap(),
            RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
            Opacity::new(1.0).unwrap(),
            TransferCurve::linear(),
            false,
        )
    }

    fn generation_path(root: &Path, id: ProjectGenerationId) -> PathBuf {
        let digest = id.digest().to_string();
        root.join("generations")
            .join("sha256")
            .join(&digest[..2])
            .join(format!("{}.json", &digest[2..]))
    }

    fn prepared_frozen_root(label: &str) -> TestDirectory {
        let directory = TestDirectory::new(label);
        fs::write(
            directory.path().join("project.json"),
            fixture_extract("stale.m4dproj/project.json"),
        )
        .unwrap();
        directory
    }

    fn frozen_stale_manual() -> (ProjectGenerationId, GenerationDocument, Vec<u8>) {
        let id = ProjectGenerationId::parse(STALE_GENERATION).unwrap();
        let member = fixture_generation_member("stale.m4dproj", id);
        let bytes = fixture_extract(&member);
        let project_id = ProjectId::parse(STALE_PROJECT).unwrap();
        let document =
            GenerationDocument::decode(id, project_id, &bytes, ProjectStoreLimits::default())
                .unwrap();
        (id, document, bytes)
    }

    fn frozen_capture(
        store: &str,
        frozen: &GenerationDocument,
    ) -> (ProjectCommitCapture, Arc<AtomicUsize>) {
        let opens = Arc::new(AtomicUsize::new(0));
        let mut sources: Vec<Box<dyn ProjectObjectSource>> = Vec::new();
        for artifact in frozen.projection().state().artifacts() {
            let storage = frozen.bindings().get(&artifact.object().digest()).unwrap();
            let bytes = match storage {
                ArtifactStorage::Direct { object } => {
                    fixture_extract(&fixture_object_member(store, object.digest()))
                }
                ArtifactStorage::Paged { binding_manifest } => {
                    let binding_bytes =
                        fixture_extract(&fixture_object_member(store, binding_manifest.digest()));
                    let binding = LogicalObjectBinding::decode(
                        &binding_bytes,
                        artifact.object(),
                        binding_manifest,
                        ProjectStoreLimits::default(),
                    )
                    .unwrap();
                    let mut logical = Vec::with_capacity(
                        usize::try_from(artifact.object().byte_length()).unwrap(),
                    );
                    for page in binding.pages() {
                        logical.extend_from_slice(&fixture_extract(&fixture_object_member(
                            store,
                            page.object().digest(),
                        )));
                    }
                    logical
                }
            };
            sources.push(Box::new(MemorySource {
                descriptor: artifact.object().clone(),
                bytes: Arc::<[u8]>::from(bytes),
                opens: Arc::clone(&opens),
                flip_second: false,
            }));
        }
        let capture = ProjectCommitCapture::new(
            frozen.projection().clone(),
            frozen.parent_generation_id(),
            frozen.base_manual_generation_id(),
            frozen.forked_from(),
            sources,
        )
        .unwrap();
        (capture, opens)
    }

    fn rejected_frozen_capture(frozen: &GenerationDocument) -> ProjectCommitCapture {
        let mut descriptors = BTreeMap::new();
        for artifact in frozen.projection().state().artifacts() {
            descriptors
                .entry(artifact.object().digest())
                .or_insert_with(|| artifact.object().clone());
        }
        let sources = descriptors
            .into_values()
            .map(|descriptor| Box::new(MustNotOpen { descriptor }) as Box<dyn ProjectObjectSource>)
            .collect();
        ProjectCommitCapture::new(
            frozen.projection().clone(),
            frozen.parent_generation_id(),
            frozen.base_manual_generation_id(),
            frozen.forked_from(),
            sources,
        )
        .unwrap()
    }

    fn fixture_extract(member: &str) -> Vec<u8> {
        let archive = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/project/project-store-v1.tar.gz");
        let output = Command::new("tar")
            .arg("-xOf")
            .arg(archive)
            .arg(member)
            .output()
            .unwrap();
        assert!(output.status.success(), "failed to extract {member}");
        output.stdout
    }

    fn fixture_generation_member(store: &str, id: ProjectGenerationId) -> String {
        let digest = id.digest().to_string();
        format!(
            "{store}/generations/sha256/{}/{}.json",
            &digest[..2],
            &digest[2..]
        )
    }

    fn fixture_object_member(store: &str, digest: ExactBytesDigest) -> String {
        let digest = digest.digest().to_string();
        format!("{store}/objects/sha256/{}/{}", &digest[..2], &digest[2..])
    }
}
