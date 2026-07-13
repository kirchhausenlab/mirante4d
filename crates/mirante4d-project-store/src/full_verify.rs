//! Bounded read-only verification of one stable active project-store snapshot.

#![cfg(target_os = "linux")]
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::BTreeSet;

use mirante4d_identity::{ExactBytesDigest, ExactBytesHasher};

use crate::{
    ProjectStoreDiagnostics, ProjectStoreFault, ProjectStoreLimits,
    generation::{ArtifactStorage, GenerationDocument, LogicalObjectBinding, PhysicalObject},
    inspection::{inspect_store_graph, unique_logical_descriptors},
    local::{LocalPublicationError, LocalStoreRoot},
    wire::generation_id_from_validated_canonical,
};

pub(crate) fn full_verify<C>(
    root: &LocalStoreRoot,
    limits: ProjectStoreLimits,
    mut is_cancelled: C,
) -> Result<ProjectStoreDiagnostics, ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let limits = limits.validate()?;
    verify_generation_identities(root, limits, &mut is_cancelled)?;
    let graph = inspect_store_graph(root, limits, &mut is_cancelled)?;
    let snapshot = graph.snapshot();
    check_cancelled(&mut is_cancelled)?;

    let mut streamed_bytes = 0_u64;
    for object in graph.object_facts() {
        verify_object(
            root,
            *object,
            limits,
            &mut streamed_bytes,
            &mut is_cancelled,
            |_| Ok(()),
        )?;
    }

    let mut verified_bindings = BTreeSet::new();
    for generation_id in graph.generation_ids() {
        check_cancelled(&mut is_cancelled)?;
        let bytes = root
            .read_generation_bytes(
                *generation_id,
                limits.generation_bytes_max,
                &mut is_cancelled,
            )
            .map_err(|error| map_local_error(error, "full_verify_generation"))?;
        let document =
            GenerationDocument::decode(*generation_id, graph.state().project_id(), &bytes, limits)
                .map_err(|_| ProjectStoreFault::Corruption {
                    stage: "full_verify_generation",
                })?;
        verify_paged_logical_objects(
            root,
            &document,
            limits,
            &mut streamed_bytes,
            &mut verified_bindings,
            &mut is_cancelled,
        )?;
    }

    check_cancelled(&mut is_cancelled)?;
    let final_graph = match inspect_store_graph(root, limits, &mut is_cancelled) {
        Ok(graph) => graph,
        Err(ProjectStoreFault::Cancelled) => return Err(ProjectStoreFault::Cancelled),
        Err(_) => return Err(ProjectStoreFault::SourceChanged),
    };
    if final_graph.snapshot() != snapshot {
        return Err(ProjectStoreFault::SourceChanged);
    }
    check_cancelled(&mut is_cancelled)?;

    Ok(ProjectStoreDiagnostics {
        queued_requests: 0,
        queued_completions: 0,
        active_transactions: 1,
        open_file_descriptors: 1,
        streamed_bytes,
        published_objects: 0,
    })
}

fn verify_generation_identities<C>(
    root: &LocalStoreRoot,
    limits: ProjectStoreLimits,
    is_cancelled: &mut C,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let generation_ids = root
        .enumerate_generation_ids(limits, &mut *is_cancelled)
        .map_err(|error| map_local_error(error, "full_verify_generation_namespace"))?;
    for generation_id in generation_ids {
        check_cancelled(is_cancelled)?;
        let bytes = root
            .read_generation_bytes(
                generation_id,
                limits.generation_bytes_max,
                &mut *is_cancelled,
            )
            .map_err(|error| map_local_error(error, "full_verify_generation"))?;
        let actual = generation_id_from_validated_canonical(&bytes).map_err(|_| {
            ProjectStoreFault::Capacity {
                stage: "full_verify_generation",
            }
        })?;
        if actual != generation_id {
            return Err(ProjectStoreFault::DigestMismatch);
        }
    }
    Ok(())
}

fn verify_paged_logical_objects<C>(
    root: &LocalStoreRoot,
    document: &GenerationDocument,
    limits: ProjectStoreLimits,
    streamed_bytes: &mut u64,
    verified_bindings: &mut BTreeSet<ExactBytesDigest>,
    is_cancelled: &mut C,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
{
    let descriptors = unique_logical_descriptors(document)?;
    for (digest, storage) in document.bindings() {
        let descriptor = descriptors
            .get(digest)
            .ok_or(ProjectStoreFault::Corruption {
                stage: "full_verify_logical",
            })?;
        let ArtifactStorage::Paged { binding_manifest } = storage else {
            continue;
        };
        if verified_bindings.contains(&binding_manifest.digest()) {
            continue;
        }
        if verified_bindings.len() >= limits.physical_store_entries_max {
            return Err(ProjectStoreFault::Capacity {
                stage: "full_verify_bindings",
            });
        }
        let capacity = usize::try_from(binding_manifest.byte_length()).map_err(|_| {
            ProjectStoreFault::Capacity {
                stage: "full_verify_binding",
            }
        })?;
        let mut binding_bytes = Vec::with_capacity(capacity);
        verify_object(
            root,
            PhysicalObject::new(binding_manifest.digest(), binding_manifest.byte_length()),
            limits,
            streamed_bytes,
            &mut *is_cancelled,
            |bytes| {
                binding_bytes.extend_from_slice(bytes);
                Ok(())
            },
        )?;
        let binding =
            LogicalObjectBinding::decode(&binding_bytes, descriptor, binding_manifest, limits)
                .map_err(|_| ProjectStoreFault::Corruption {
                    stage: "full_verify_binding",
                })?;
        let mut logical_hasher = ExactBytesHasher::new();
        for page in binding.pages() {
            verify_object(
                root,
                page.object(),
                limits,
                streamed_bytes,
                &mut *is_cancelled,
                |bytes| {
                    logical_hasher
                        .update(bytes)
                        .map_err(|_| ProjectStoreFault::Capacity {
                            stage: "full_verify_logical",
                        })
                },
            )?;
        }
        let facts = logical_hasher
            .finalize()
            .map_err(|_| ProjectStoreFault::Capacity {
                stage: "full_verify_logical",
            })?;
        if facts.digest() != descriptor.digest() || facts.byte_length() != descriptor.byte_length()
        {
            return Err(ProjectStoreFault::DigestMismatch);
        }
        verified_bindings.insert(binding_manifest.digest());
    }
    Ok(())
}

fn verify_object<C, F>(
    root: &LocalStoreRoot,
    object: PhysicalObject,
    limits: ProjectStoreLimits,
    streamed_bytes: &mut u64,
    is_cancelled: &mut C,
    mut on_chunk: F,
) -> Result<(), ProjectStoreFault>
where
    C: FnMut() -> bool,
    F: FnMut(&[u8]) -> Result<(), ProjectStoreFault>,
{
    let mut observed = *streamed_bytes;
    let mut callback_error = None;
    let result = root.verify_exact_object(
        object.digest(),
        object.byte_length(),
        limits.object_or_page_bytes_max,
        limits.stream_buffer_bytes_max(),
        &mut *is_cancelled,
        |bytes| {
            if callback_error.is_none() {
                observed =
                    match observed.checked_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX)) {
                        Some(observed) => observed,
                        None => {
                            callback_error = Some(ProjectStoreFault::Capacity {
                                stage: "full_verify_bytes",
                            });
                            observed
                        }
                    };
            }
            if callback_error.is_none()
                && let Err(error) = on_chunk(bytes)
            {
                callback_error = Some(error);
            }
        },
    );
    if let Some(error) = callback_error {
        return Err(error);
    }
    result.map_err(|error| map_local_error(error, "full_verify_object"))?;
    *streamed_bytes = observed;
    Ok(())
}

fn check_cancelled(is_cancelled: &mut impl FnMut() -> bool) -> Result<(), ProjectStoreFault> {
    if is_cancelled() {
        Err(ProjectStoreFault::Cancelled)
    } else {
        Ok(())
    }
}

fn map_local_error(error: LocalPublicationError, stage: &'static str) -> ProjectStoreFault {
    match error {
        LocalPublicationError::Cancelled => ProjectStoreFault::Cancelled,
        LocalPublicationError::Capacity { .. } => ProjectStoreFault::Capacity { stage },
        LocalPublicationError::SourceLength { .. } | LocalPublicationError::SourceDigest => {
            ProjectStoreFault::DigestMismatch
        }
        LocalPublicationError::InvalidPath
        | LocalPublicationError::ExistingMismatch
        | LocalPublicationError::InvalidGeneration
        | LocalPublicationError::InvalidControl
        | LocalPublicationError::Io { .. }
        | LocalPublicationError::DestinationExists
        | LocalPublicationError::AtomicPublishUnsupported
        | LocalPublicationError::RefAlreadyPresent
        | LocalPublicationError::RefChanged
        | LocalPublicationError::RefCommitIndeterminate
        | LocalPublicationError::PackageCommitIndeterminate => {
            ProjectStoreFault::Corruption { stage }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        collections::BTreeMap,
        fs::{self, File, OpenOptions},
        io::{Read, Seek, SeekFrom, Write},
        path::{Path, PathBuf},
        process::Command,
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use mirante4d_identity::{ExactBytesFacts, ExactBytesHasher};

    use super::*;

    static TEST_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    struct TestProject(PathBuf);

    impl TestProject {
        fn extracted(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "mirante4d-project-full-verify-{label}-{}-{nonce}-{}",
                std::process::id(),
                TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let status = Command::new("tar")
                .args(["-xzf"])
                .arg(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../fixtures/project/project-store-v1.tar.gz"),
                )
                .args(["-C"])
                .arg(&root)
                .arg("recoverable.m4dproj")
                .status()
                .unwrap();
            assert!(status.success());
            Self(root.join("recoverable.m4dproj"))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestProject {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    #[test]
    fn healthy_store_verifies_every_physical_and_paged_logical_byte_without_mutation() {
        let project = TestProject::extracted("healthy");
        let before = store_facts(project.path());
        let root = LocalStoreRoot::open(project.path()).unwrap();
        let limits = ProjectStoreLimits {
            stream_buffer_bytes_max: 2 * 1024 * 1024,
            ..ProjectStoreLimits::default()
        };
        let expected_bytes = expected_streamed_bytes(&root, limits);

        let diagnostics = full_verify(&root, limits, || false).unwrap();

        assert_eq!(diagnostics.streamed_bytes, expected_bytes);
        assert!(diagnostics.streamed_bytes > graph_object_bytes(&root, limits));
        assert_eq!(diagnostics.active_transactions, 1);
        assert_eq!(diagnostics.open_file_descriptors, 1);
        assert_eq!(diagnostics.published_objects, 0);
        assert_eq!(store_facts(project.path()), before);
    }

    #[test]
    fn corruption_cancellation_and_snapshot_drift_fail_without_mutation() {
        let limits = ProjectStoreLimits::default();

        for (label, select) in [
            ("direct-corruption", ObjectSelection::Direct),
            ("page-corruption", ObjectSelection::Page),
        ] {
            let project = TestProject::extracted(label);
            let root = LocalStoreRoot::open(project.path()).unwrap();
            let object = selected_object(&root, limits, select);
            flip_first_byte(&object_path(project.path(), object));
            let corrupted = store_facts(project.path());
            assert!(matches!(
                full_verify(&root, limits, || false),
                Err(ProjectStoreFault::DigestMismatch)
            ));
            assert_eq!(store_facts(project.path()), corrupted);
        }

        let generation = TestProject::extracted("generation-corruption");
        let generation_root = LocalStoreRoot::open(generation.path()).unwrap();
        let generation_id = inspect_store_graph(&generation_root, limits, || false)
            .unwrap()
            .generation_ids()[0];
        flip_first_byte(&generation_path(generation.path(), generation_id));
        let corrupted = store_facts(generation.path());
        assert!(matches!(
            full_verify(&generation_root, limits, || false),
            Err(ProjectStoreFault::DigestMismatch)
        ));
        assert_eq!(store_facts(generation.path()), corrupted);

        let cancelled = TestProject::extracted("cancelled");
        let cancelled_root = LocalStoreRoot::open(cancelled.path()).unwrap();
        let before_cancel = store_facts(cancelled.path());
        let initial_polls = prefix_poll_count(&cancelled_root, limits);
        let polls = Cell::new(0_usize);
        let byte_sized = ProjectStoreLimits {
            stream_buffer_bytes_max: 1,
            ..limits
        };
        assert!(matches!(
            full_verify(&cancelled_root, byte_sized, || {
                let next = polls.get().saturating_add(1);
                polls.set(next);
                next >= initial_polls.saturating_add(100)
            }),
            Err(ProjectStoreFault::Cancelled)
        ));
        assert_eq!(store_facts(cancelled.path()), before_cancel);

        let drifted = TestProject::extracted("drifted");
        let drifted_root = LocalStoreRoot::open(drifted.path()).unwrap();
        let mut expected_after_drift = store_facts(drifted.path());
        let old_pin = PathBuf::from("refs/pins/checkpoint-a");
        let new_pin = PathBuf::from("refs/pins/checkpoint-b");
        let pin_facts = expected_after_drift.remove(&old_pin).unwrap();
        expected_after_drift.insert(new_pin.clone(), pin_facts);
        let initial_polls = prefix_poll_count(&drifted_root, limits);
        let polls = Cell::new(0_usize);
        let changed = Cell::new(false);
        assert!(matches!(
            full_verify(&drifted_root, limits, || {
                let next = polls.get().saturating_add(1);
                polls.set(next);
                if next == initial_polls.saturating_add(1) {
                    fs::rename(drifted.path().join(&old_pin), drifted.path().join(&new_pin))
                        .unwrap();
                    changed.set(true);
                }
                false
            }),
            Err(ProjectStoreFault::SourceChanged)
        ));
        assert!(changed.get());
        assert_eq!(store_facts(drifted.path()), expected_after_drift);
    }

    #[derive(Clone, Copy)]
    enum ObjectSelection {
        Direct,
        Page,
    }

    fn selected_object(
        root: &LocalStoreRoot,
        limits: ProjectStoreLimits,
        selection: ObjectSelection,
    ) -> PhysicalObject {
        let graph = inspect_store_graph(root, limits, || false).unwrap();
        for generation_id in graph.generation_ids() {
            let bytes = root
                .read_generation_bytes(*generation_id, limits.generation_bytes_max, || false)
                .unwrap();
            let document = GenerationDocument::decode(
                *generation_id,
                graph.state().project_id(),
                &bytes,
                limits,
            )
            .unwrap();
            let descriptors = unique_logical_descriptors(&document).unwrap();
            for (digest, storage) in document.bindings() {
                match (selection, storage) {
                    (ObjectSelection::Direct, ArtifactStorage::Direct { object }) => {
                        return *object;
                    }
                    (ObjectSelection::Page, ArtifactStorage::Paged { binding_manifest }) => {
                        let bytes = root
                            .read_exact_object_bytes(
                                binding_manifest.digest(),
                                binding_manifest.byte_length(),
                                limits.object_or_page_bytes_max,
                                || false,
                            )
                            .unwrap();
                        let binding = LogicalObjectBinding::decode(
                            &bytes,
                            descriptors.get(digest).unwrap(),
                            binding_manifest,
                            limits,
                        )
                        .unwrap();
                        return binding.pages()[0].object();
                    }
                    _ => {}
                }
            }
        }
        panic!("fixture must contain the requested object kind");
    }

    fn expected_streamed_bytes(root: &LocalStoreRoot, limits: ProjectStoreLimits) -> u64 {
        let graph = inspect_store_graph(root, limits, || false).unwrap();
        let mut total = graph
            .object_facts()
            .iter()
            .map(|object| object.byte_length())
            .sum::<u64>();
        let mut bindings = BTreeSet::new();
        for generation_id in graph.generation_ids() {
            let bytes = root
                .read_generation_bytes(*generation_id, limits.generation_bytes_max, || false)
                .unwrap();
            let document = GenerationDocument::decode(
                *generation_id,
                graph.state().project_id(),
                &bytes,
                limits,
            )
            .unwrap();
            let descriptors = unique_logical_descriptors(&document).unwrap();
            for (digest, storage) in document.bindings() {
                if let ArtifactStorage::Paged { binding_manifest } = storage
                    && bindings.insert(binding_manifest.digest())
                {
                    total = total
                        .checked_add(binding_manifest.byte_length())
                        .and_then(|value| {
                            value.checked_add(descriptors.get(digest).unwrap().byte_length())
                        })
                        .unwrap();
                }
            }
        }
        total
    }

    fn graph_object_bytes(root: &LocalStoreRoot, limits: ProjectStoreLimits) -> u64 {
        inspect_store_graph(root, limits, || false)
            .unwrap()
            .object_facts()
            .iter()
            .map(|object| object.byte_length())
            .sum()
    }

    fn prefix_poll_count(root: &LocalStoreRoot, limits: ProjectStoreLimits) -> usize {
        let polls = Cell::new(0_usize);
        verify_generation_identities(root, limits, &mut || {
            polls.set(polls.get().saturating_add(1));
            false
        })
        .unwrap();
        inspect_store_graph(root, limits, || {
            polls.set(polls.get().saturating_add(1));
            false
        })
        .unwrap();
        polls.get()
    }

    fn flip_first_byte(path: &Path) {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let mut byte = [0_u8; 1];
        file.read_exact(&mut byte).unwrap();
        byte[0] ^= 0x01;
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&byte).unwrap();
    }

    fn object_path(root: &Path, object: PhysicalObject) -> PathBuf {
        let digest = object.digest().digest().to_string();
        root.join("objects")
            .join("sha256")
            .join(&digest[..2])
            .join(&digest[2..])
    }

    fn generation_path(root: &Path, generation_id: crate::ProjectGenerationId) -> PathBuf {
        let digest = generation_id.digest().to_string();
        root.join("generations")
            .join("sha256")
            .join(&digest[..2])
            .join(format!("{}.json", &digest[2..]))
    }

    fn store_facts(root: &Path) -> BTreeMap<PathBuf, ExactBytesFacts> {
        fn visit(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, ExactBytesFacts>) {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                let metadata = fs::symlink_metadata(&path).unwrap();
                if metadata.is_dir() {
                    visit(root, &path, files);
                } else {
                    assert!(metadata.is_file());
                    let mut file = File::open(&path).unwrap();
                    let mut hasher = ExactBytesHasher::new();
                    let mut buffer = [0_u8; 64 * 1024];
                    loop {
                        let read = file.read(&mut buffer).unwrap();
                        if read == 0 {
                            break;
                        }
                        hasher.update(&buffer[..read]).unwrap();
                    }
                    files.insert(
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        hasher.finalize().unwrap(),
                    );
                }
            }
        }
        let mut files = BTreeMap::new();
        visit(root, root, &mut files);
        files
    }
}
