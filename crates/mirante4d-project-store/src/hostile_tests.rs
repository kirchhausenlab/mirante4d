use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Cursor, Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use mirante4d_identity::{ExactBytesHasher, RawObjectDescriptor};
use mirante4d_project_model::ProjectId;
use rustix::io::Errno;

use crate::{
    ProjectCommitCapture, ProjectObjectSource, ProjectOpenMode, ProjectStoreFault,
    ProjectStoreLimits, ProjectStorePath,
    generation::{ArtifactStorage, GenerationDocument},
    inspection::open_established_store,
    local::{LocalPublicationError, LocalStoreRoot, write_all_classified},
    transaction::{InitialPackageMode, install_initial_manual_package, map_local_error},
    wire::ProjectEnvelope,
};

const RECOVERABLE_G2: &str = concat!(
    "m4d-project-generation-v1-sha256:",
    "50fc92ea0e67a54336658f1638596642f17177ceb72c3afbc364c941e6a9b854"
);
const DIVERGENT_INITIAL: &str = concat!(
    "m4d-project-generation-v1-sha256:",
    "10011b8d7dce93c428e1d117b485746522b4ae1d4d8ee89e359739f2cffd3a10"
);
const RECOVERABLE_PROJECT: &str = "11111111-2222-4333-8444-555555555555";

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(parent: &Path, label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = parent.join(format!(
            "mirante4d-hostile-{label}-{}-{nonce}-{}",
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
        make_writable(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn storage_full_is_capacity_and_leaves_no_authority_or_residue() {
    let directory = TestDirectory::new(&std::env::temp_dir(), "storage-full");
    let bytes = b"new immutable object";
    let facts = ExactBytesHasher::hash(bytes).unwrap();
    let digest = facts.digest().digest().to_string();
    fs::create_dir_all(directory.path().join("staging")).unwrap();
    fs::create_dir_all(directory.path().join("objects/sha256").join(&digest[..2])).unwrap();
    fs::create_dir_all(directory.path().join("refs")).unwrap();
    fs::write(directory.path().join("refs/authority-sentinel"), b"old").unwrap();
    let before = tree_facts(directory.path());

    let root = LocalStoreRoot::open(directory.path()).unwrap();
    let error = root
        .publish_declared_object_with_storage_full(
            &mut Cursor::new(bytes),
            facts.digest(),
            facts.byte_length(),
            ProjectStoreLimits::default().object_or_page_bytes_max(),
            || false,
        )
        .expect_err("the injected destination write must report ENOSPC");
    assert!(matches!(
        error,
        LocalPublicationError::StorageFull {
            operation: "write a staged immutable file"
        }
    ));
    assert_eq!(tree_facts(directory.path()), before);
    assert_eq!(
        map_local_error(
            LocalPublicationError::StorageFull {
                operation: "write a staged immutable file",
            },
            "object_write",
        ),
        ProjectStoreFault::Capacity {
            stage: "object_write"
        }
    );
}

#[test]
fn partial_writes_complete_exactly_and_zero_progress_fails_closed() {
    let expected = b"0123456789abcdef";
    let mut short = ShortWriter::new(3);
    write_all_classified(&mut short, expected, "short-write test").unwrap();
    assert_eq!(short.bytes, expected);
    assert!(short.calls > 1);

    let mut zero = ZeroWriter;
    let error = write_all_classified(&mut zero, expected, "zero-write test").unwrap_err();
    assert!(matches!(
        error,
        LocalPublicationError::Io { operation: "zero-write test", source }
            if source.kind() == io::ErrorKind::WriteZero
    ));

    let mut read_only = ErrorWriter(Errno::ROFS);
    assert!(matches!(
        write_all_classified(&mut read_only, expected, "read-only write"),
        Err(LocalPublicationError::ReadOnly {
            operation: "read-only write"
        })
    ));
    let mut full = ErrorWriter(Errno::NOSPC);
    assert!(matches!(
        write_all_classified(&mut full, expected, "full write"),
        Err(LocalPublicationError::StorageFull {
            operation: "full write"
        })
    ));
}

#[test]
fn permission_denial_is_read_only_and_publishes_nothing() {
    let directory = TestDirectory::new(&std::env::temp_dir(), "permission");
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o500)).unwrap();
    let root = LocalStoreRoot::open(directory.path()).unwrap();
    let project_id = ProjectId::parse("12345678-9abc-4def-8123-456789abcdef").unwrap();
    let error = root
        .publish_project_envelope(
            ProjectEnvelope::new(project_id),
            ProjectStoreLimits::default(),
            || false,
        )
        .expect_err("a non-writable project root must reject publication");
    assert!(matches!(error, LocalPublicationError::ReadOnly { .. }));
    assert!(!directory.path().join("project.json").exists());
    assert_eq!(
        map_local_error(
            LocalPublicationError::ReadOnly {
                operation: "create the project envelope",
            },
            "project_envelope",
        ),
        ProjectStoreFault::ReadOnly
    );
}

#[test]
fn whole_package_relocation_survives_a_read_only_cross_device_copy() {
    let source_parent = TestDirectory::new(&std::env::temp_dir(), "relocation-source");
    let destination_parent = TestDirectory::new(Path::new("/dev/shm"), "relocation-destination");
    assert_ne!(
        fs::metadata(source_parent.path()).unwrap().dev(),
        fs::metadata(destination_parent.path()).unwrap().dev(),
        "the relocation fixture requires distinct filesystems"
    );
    extract_fixture(source_parent.path());
    let source = source_parent.path().join("recoverable.m4dproj");
    let source_path = ProjectStorePath::new(source.clone()).unwrap();
    let original = open_established_store(
        &source_path,
        ProjectOpenMode::ReadOnly,
        ProjectStoreLimits::default(),
        || false,
    )
    .unwrap();
    let expected_project = original.inspection().project_id();
    let expected_manual = original.inspection().manual();
    let expected_projection = original
        .inspection()
        .manual_generation()
        .projection()
        .clone();
    drop(original);

    let relocated = destination_parent.path().join("relocated.m4dproj");
    copy_tree(&source, &relocated);
    assert_eq!(tree_facts(&source), tree_facts(&relocated));
    fs::remove_dir_all(&source).unwrap();
    make_read_only(&relocated);

    let relocated_path = ProjectStorePath::new(relocated).unwrap();
    let opened = open_established_store(
        &relocated_path,
        ProjectOpenMode::ReadOnly,
        ProjectStoreLimits::default(),
        || false,
    )
    .unwrap();
    assert_eq!(opened.effective_mode(), ProjectOpenMode::ReadOnly);
    assert_eq!(opened.inspection().project_id(), expected_project);
    assert_eq!(opened.inspection().manual(), expected_manual);
    assert_eq!(
        opened.inspection().manual_generation().projection(),
        &expected_projection
    );
}

#[test]
fn cross_device_save_as_read_failure_preserves_source_and_destination_absence() {
    let source_parent = TestDirectory::new(&std::env::temp_dir(), "save-as-source");
    let destination_parent = TestDirectory::new(Path::new("/dev/shm"), "save-as-destination");
    assert_ne!(
        fs::metadata(source_parent.path()).unwrap().dev(),
        fs::metadata(destination_parent.path()).unwrap().dev(),
        "the Save As failure fixture requires distinct filesystems"
    );
    extract_fixture(source_parent.path());
    let source = source_parent.path().join("recoverable.m4dproj");
    let target_store = source_parent.path().join("divergent.m4dproj");
    let target = load_generation(&target_store, DIVERGENT_INITIAL);
    let source_before = tree_facts(&source);

    let mut sources: Vec<Box<dyn ProjectObjectSource>> = Vec::new();
    for (index, artifact) in target.projection().state().artifacts().iter().enumerate() {
        let ArtifactStorage::Direct { object } = target
            .bindings()
            .get(&artifact.object().digest())
            .expect("the target fixture must bind every artifact")
        else {
            panic!("the cross-device Save As fixture must remain directly stored");
        };
        let digest = object.digest().digest().to_string();
        let path = source
            .join("objects/sha256")
            .join(&digest[..2])
            .join(&digest[2..]);
        assert!(path.is_file());
        sources.push(Box::new(PathObjectSource {
            descriptor: artifact.object().clone(),
            path,
            fail_after: (index == 0).then_some(1),
        }));
    }
    let capture = ProjectCommitCapture::new(
        target.projection().clone(),
        None,
        None,
        target.forked_from(),
        sources,
    )
    .unwrap();
    let destination =
        ProjectStorePath::new(destination_parent.path().join("fork.m4dproj")).unwrap();
    let result = install_initial_manual_package(
        &destination,
        InitialPackageMode::SaveAs {
            source_project_id: ProjectId::parse(RECOVERABLE_PROJECT).unwrap(),
            source_generation_id: crate::ProjectGenerationId::parse(RECOVERABLE_G2).unwrap(),
        },
        capture,
        ProjectStoreLimits::default(),
        || false,
    );
    assert!(matches!(result, Err(ProjectStoreFault::SourceChanged)));
    assert!(!destination.as_path().exists());
    assert_eq!(tree_facts(&source), source_before);
    assert_eq!(fs::read_dir(destination_parent.path()).unwrap().count(), 0);
}

struct ShortWriter {
    maximum: usize,
    bytes: Vec<u8>,
    calls: usize,
}

impl ShortWriter {
    fn new(maximum: usize) -> Self {
        Self {
            maximum,
            bytes: Vec::new(),
            calls: 0,
        }
    }
}

impl Write for ShortWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        let written = bytes.len().min(self.maximum);
        self.bytes.extend_from_slice(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct ZeroWriter;

impl Write for ZeroWriter {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Ok(0)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct ErrorWriter(Errno);

impl Write for ErrorWriter {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(self.0))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct PathObjectSource {
    descriptor: RawObjectDescriptor,
    path: PathBuf,
    fail_after: Option<usize>,
}

impl ProjectObjectSource for PathObjectSource {
    fn descriptor(&self) -> &RawObjectDescriptor {
        &self.descriptor
    }

    fn open(&self) -> io::Result<Box<dyn Read + Send>> {
        let file = File::open(&self.path)?;
        match self.fail_after {
            Some(bytes) => Ok(Box::new(FailAfterReader {
                file,
                remaining: bytes,
            })),
            None => Ok(Box::new(file)),
        }
    }
}

struct FailAfterReader {
    file: File,
    remaining: usize,
}

impl Read for FailAfterReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::other("injected cross-device source failure"));
        }
        let allowed = bytes.len().min(self.remaining);
        let read = self.file.read(&mut bytes[..allowed])?;
        self.remaining = self.remaining.saturating_sub(read);
        Ok(read)
    }
}

fn load_generation(store: &Path, id: &str) -> GenerationDocument {
    let limits = ProjectStoreLimits::default();
    let id = crate::ProjectGenerationId::parse(id).unwrap();
    let root = LocalStoreRoot::open(store).unwrap();
    let project_id = root
        .read_project_envelope(limits, || false)
        .unwrap()
        .project_id();
    let bytes = root
        .read_generation_bytes(id, limits.generation_bytes_max, || false)
        .unwrap();
    GenerationDocument::decode(id, project_id, &bytes, limits).unwrap()
}

fn fixture_archive() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/project/project-store-v1.tar.gz")
}

fn extract_fixture(destination: &Path) {
    let output = Command::new("tar")
        .arg("-xzf")
        .arg(fixture_archive())
        .arg("-C")
        .arg(destination)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fixture extraction failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir(destination).unwrap();
    let mut entries = fs::read_dir(source)
        .unwrap()
        .map(|entry| entry.unwrap())
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let source = entry.path();
        let destination = destination.join(entry.file_name());
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            copy_tree(&source, &destination);
        } else if kind.is_file() {
            fs::copy(&source, &destination).unwrap();
        } else {
            panic!("fixture contains an unsupported entry type");
        }
    }
}

fn tree_facts(root: &Path) -> BTreeMap<PathBuf, Option<String>> {
    fn visit(root: &Path, current: &Path, facts: &mut BTreeMap<PathBuf, Option<String>>) {
        let mut entries = fs::read_dir(current)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                facts.insert(relative, None);
                visit(root, &path, facts);
            } else if kind.is_file() {
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
                facts.insert(
                    relative,
                    Some(hasher.finalize().unwrap().digest().to_string()),
                );
            } else {
                panic!("tree contains an unsupported entry type");
            }
        }
    }

    let mut facts = BTreeMap::new();
    visit(root, root, &mut facts);
    facts
}

fn make_read_only(path: &Path) {
    let metadata = fs::symlink_metadata(path).unwrap();
    if metadata.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            make_read_only(&entry.unwrap().path());
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o500)).unwrap();
    } else if metadata.is_file() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o400)).unwrap();
    }
}

fn make_writable(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                make_writable(&entry.path());
            }
        }
    } else if metadata.is_file() {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
}
