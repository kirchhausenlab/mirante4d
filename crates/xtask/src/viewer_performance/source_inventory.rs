use std::{
    fs::{self, File, Metadata},
    io::Read,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use mirante4d_identity::Sha256Hasher;
use rustix::fs::{CWD, Mode, OFlags, ResolveFlags, openat2};

const SOURCE_ENTRY_MAX: usize = 4_096;
const STREAM_BUFFER_BYTES: usize = 64 * 1024;

// Keep this framing identical to the T5 source-inventory authority. The same
// source must have one identity regardless of which performance protocol uses
// it.
const SOURCE_INVENTORY_DOMAIN: &[u8] = b"mirante4d-t5-source-inventory-1\0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InventoryFacts {
    pub(crate) regular_files: u64,
    pub(crate) source_bytes: u64,
    pub(crate) sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MetadataSnapshot {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    owner: u32,
    group: u32,
    device_type: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl MetadataSnapshot {
    fn capture(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            owner: metadata.uid(),
            group: metadata.gid(),
            device_type: metadata.rdev(),
            length: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

struct InventoryFile {
    relative_name: Vec<u8>,
    path: PathBuf,
    discovered: MetadataSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureEvent {
    FileRead,
}

pub(crate) fn capture(root: &Path) -> anyhow::Result<InventoryFacts> {
    capture_with(root, SOURCE_ENTRY_MAX, |_, _| Ok(()))
}

fn capture_with(
    root: &Path,
    entry_max: usize,
    mut observe: impl FnMut(CaptureEvent, &Path) -> anyhow::Result<()>,
) -> anyhow::Result<InventoryFacts> {
    let root_metadata = fs::symlink_metadata(root)
        .context("viewer source inventory root is unavailable or unreadable")?;
    if root_metadata.file_type().is_symlink() {
        bail!("viewer source inventory root must not be a symbolic link")
    }

    let mut pending_directories = Vec::new();
    let mut captured_directories = Vec::new();
    let mut files = Vec::new();
    let mut entries_seen = 0_usize;

    if root_metadata.is_file() {
        require_entry_capacity(&mut entries_seen, entry_max)?;
        let relative_name = root
            .file_name()
            .context("viewer source inventory file has no name")?
            .as_bytes()
            .to_vec();
        files.push(InventoryFile {
            relative_name,
            path: root.to_path_buf(),
            discovered: MetadataSnapshot::capture(&root_metadata),
        });
    } else if root_metadata.is_dir() {
        pending_directories.push((
            root.to_path_buf(),
            MetadataSnapshot::capture(&root_metadata),
        ));
    } else {
        bail!("viewer source inventory root must be a regular file or directory")
    }

    while let Some((directory, discovered)) = pending_directories.pop() {
        let before_metadata = fs::symlink_metadata(&directory)
            .context("viewer source directory changed during inventory capture")?;
        if before_metadata.file_type().is_symlink() || !before_metadata.is_dir() {
            bail!("viewer source directory changed type during inventory capture")
        }
        let before = MetadataSnapshot::capture(&before_metadata);
        if before != discovered {
            bail!("viewer source directory changed during inventory capture")
        }

        for entry in fs::read_dir(&directory)
            .context("viewer source directory is unavailable or unreadable")?
        {
            let entry = entry.context("viewer source directory entry is unreadable")?;
            require_entry_capacity(&mut entries_seen, entry_max)?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .context("viewer source inventory entry is unavailable or unreadable")?;
            if metadata.file_type().is_symlink() {
                bail!("viewer source inventory contains a symbolic link")
            }
            if metadata.is_dir() {
                pending_directories.push((path, MetadataSnapshot::capture(&metadata)));
            } else if metadata.is_file() {
                let relative_name = path
                    .strip_prefix(root)
                    .context("viewer source inventory path escaped its root")?
                    .as_os_str()
                    .as_bytes()
                    .to_vec();
                files.push(InventoryFile {
                    relative_name,
                    path,
                    discovered: MetadataSnapshot::capture(&metadata),
                });
            } else {
                bail!("viewer source inventory contains a special entry")
            }
        }

        let after_metadata = fs::symlink_metadata(&directory)
            .context("viewer source directory changed during inventory capture")?;
        if after_metadata.file_type().is_symlink()
            || !after_metadata.is_dir()
            || MetadataSnapshot::capture(&after_metadata) != before
        {
            bail!("viewer source directory changed during inventory capture")
        }
        captured_directories.push((directory, before));
    }

    if files.is_empty() {
        bail!("viewer source inventory is empty")
    }
    files.sort_unstable_by(|left, right| left.relative_name.cmp(&right.relative_name));

    let mut hasher = Sha256Hasher::new();
    hasher.update(SOURCE_INVENTORY_DOMAIN);
    let mut source_bytes = 0_u64;
    let mut buffer = [0_u8; STREAM_BUFFER_BYTES];

    for source in &files {
        let before_metadata = fs::symlink_metadata(&source.path)
            .context("viewer source file changed during inventory capture")?;
        if before_metadata.file_type().is_symlink() || !before_metadata.is_file() {
            bail!("viewer source file changed type during inventory capture")
        }
        let before = MetadataSnapshot::capture(&before_metadata);
        if before != source.discovered {
            bail!("viewer source file changed during inventory capture")
        }

        let descriptor = openat2(
            CWD,
            &source.path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
            ResolveFlags::NO_MAGICLINKS,
        )
        .context("viewer source file is unavailable, unreadable, or a symbolic link")?;
        let mut file = File::from(descriptor);
        let opened_metadata = file
            .metadata()
            .context("viewer source file metadata is unreadable")?;
        if !opened_metadata.is_file() || MetadataSnapshot::capture(&opened_metadata) != before {
            bail!("viewer source file changed while it was opened")
        }

        let length = before.length;
        source_bytes = source_bytes
            .checked_add(length)
            .context("viewer source inventory byte count overflowed")?;
        hasher.update(
            u64::try_from(source.relative_name.len())
                .context("viewer source relative name is too long")?
                .to_le_bytes(),
        );
        hasher.update(&source.relative_name);
        hasher.update(length.to_le_bytes());

        let mut consumed = 0_u64;
        loop {
            let count = file
                .read(&mut buffer)
                .context("viewer source file became unreadable")?;
            if count == 0 {
                break;
            }
            consumed = consumed
                .checked_add(
                    u64::try_from(count).context("viewer source inventory read is too large")?,
                )
                .context("viewer source inventory byte count overflowed")?;
            hasher.update(&buffer[..count]);
        }

        observe(CaptureEvent::FileRead, &source.path)?;

        let opened_after = file
            .metadata()
            .context("viewer source file changed during inventory capture")?;
        let named_after = fs::symlink_metadata(&source.path)
            .context("viewer source file changed during inventory capture")?;
        if consumed != length
            || !opened_after.is_file()
            || MetadataSnapshot::capture(&opened_after) != before
            || named_after.file_type().is_symlink()
            || !named_after.is_file()
            || MetadataSnapshot::capture(&named_after) != before
        {
            bail!("viewer source file changed while its inventory was captured")
        }
    }

    for (directory, before) in captured_directories {
        let after_metadata = fs::symlink_metadata(directory)
            .context("viewer source directory changed during inventory capture")?;
        if after_metadata.file_type().is_symlink()
            || !after_metadata.is_dir()
            || MetadataSnapshot::capture(&after_metadata) != before
        {
            bail!("viewer source directory changed during inventory capture")
        }
    }

    Ok(InventoryFacts {
        regular_files: u64::try_from(files.len())
            .context("viewer source inventory file count overflowed")?,
        source_bytes,
        sha256: hasher.finalize().to_string(),
    })
}

fn require_entry_capacity(entries_seen: &mut usize, entry_max: usize) -> anyhow::Result<()> {
    *entries_seen = entries_seen
        .checked_add(1)
        .context("viewer source inventory entry count overflowed")?;
    if *entries_seen > entry_max {
        bail!("viewer source inventory exceeds its entry bound")
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs,
        os::unix::{ffi::OsStringExt, fs::symlink, net::UnixListener},
    };

    use mirante4d_identity::Sha256Hasher;

    use super::*;

    fn expected_digest(entries: &[(Vec<u8>, Vec<u8>)]) -> String {
        let mut hasher = Sha256Hasher::new();
        hasher.update(SOURCE_INVENTORY_DOMAIN);
        for (name, contents) in entries {
            hasher.update(u64::try_from(name.len()).unwrap().to_le_bytes());
            hasher.update(name);
            hasher.update(u64::try_from(contents.len()).unwrap().to_le_bytes());
            hasher.update(contents);
        }
        hasher.finalize().to_string()
    }

    #[test]
    fn source_inventory_hashes_sorted_raw_relative_names_and_exact_lengths() {
        let root = tempfile::tempdir().unwrap();
        let non_utf8_name = vec![0x80, b'.', b't', b'i', b'f'];
        fs::write(root.path().join("a.tif"), b"alpha").unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        fs::write(root.path().join("nested/z.tif"), b"inner").unwrap();
        fs::write(
            root.path().join(OsString::from_vec(non_utf8_name.clone())),
            b"omega",
        )
        .unwrap();

        let facts = capture(root.path()).unwrap();
        assert_eq!(facts.regular_files, 3);
        assert_eq!(facts.source_bytes, 15);
        assert_eq!(
            facts.sha256,
            expected_digest(&[
                (b"a.tif".to_vec(), b"alpha".to_vec()),
                (b"nested/z.tif".to_vec(), b"inner".to_vec()),
                (non_utf8_name, b"omega".to_vec()),
            ])
        );
        assert_eq!(capture(root.path()).unwrap(), facts);
    }

    #[test]
    fn source_inventory_accepts_a_file_and_hashes_its_raw_basename() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.tif");
        fs::write(&source, b"pixels").unwrap();

        let facts = capture(&source).unwrap();
        assert_eq!(facts.regular_files, 1);
        assert_eq!(facts.source_bytes, 6);
        assert_eq!(
            facts.sha256,
            "8854e260fc7b29ef5f3148496bb7e07b7c8ffa1dbc6070740fd7f83c9dc1c910"
        );
    }

    #[test]
    fn source_inventory_rejects_empty_symlink_and_special_sources() {
        let empty = tempfile::tempdir().unwrap();
        assert!(capture(empty.path()).is_err());

        let linked = tempfile::tempdir().unwrap();
        fs::write(linked.path().join("source.tif"), b"pixels").unwrap();
        symlink(
            linked.path().join("source.tif"),
            linked.path().join("linked.tif"),
        )
        .unwrap();
        assert!(capture(linked.path()).is_err());
        assert!(capture(&linked.path().join("linked.tif")).is_err());

        let special = tempfile::tempdir().unwrap();
        let _socket = UnixListener::bind(special.path().join("source.socket")).unwrap();
        assert!(capture(special.path()).is_err());
    }

    #[test]
    fn source_inventory_bounds_every_traversed_entry() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        fs::write(root.path().join("source.tif"), b"pixels").unwrap();

        assert!(capture_with(root.path(), 1, |_, _| Ok(())).is_err());
    }

    #[test]
    fn source_inventory_rejects_length_and_type_changes_during_streaming() {
        let length_root = tempfile::tempdir().unwrap();
        fs::write(length_root.path().join("source.tif"), b"pixels").unwrap();
        let mut length_changed = false;
        let result = capture_with(length_root.path(), SOURCE_ENTRY_MAX, |event, path| {
            if event == CaptureEvent::FileRead && !length_changed {
                fs::write(path, b"pixels-expanded")?;
                length_changed = true;
            }
            Ok(())
        });
        assert!(result.is_err());

        let type_root = tempfile::tempdir().unwrap();
        fs::write(type_root.path().join("source.tif"), b"pixels").unwrap();
        let mut type_changed = false;
        let result = capture_with(type_root.path(), SOURCE_ENTRY_MAX, |event, path| {
            if event == CaptureEvent::FileRead && !type_changed {
                fs::remove_file(path)?;
                fs::create_dir(path)?;
                type_changed = true;
            }
            Ok(())
        });
        assert!(result.is_err());
    }

    #[test]
    fn source_inventory_rejects_same_length_replacement_and_directory_mutation() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a-source.tif"), b"before").unwrap();
        fs::write(root.path().join("z-replacement.tif"), b"after!").unwrap();
        let mut replaced = false;
        let result = capture_with(root.path(), SOURCE_ENTRY_MAX, |event, path| {
            if event == CaptureEvent::FileRead
                && path.file_name() == Some(std::ffi::OsStr::new("a-source.tif"))
                && !replaced
            {
                fs::rename(root.path().join("z-replacement.tif"), path)?;
                replaced = true;
            }
            Ok(())
        });
        assert!(result.is_err());
    }
}
