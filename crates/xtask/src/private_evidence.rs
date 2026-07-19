use std::{
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, bail};
use mirante4d_identity::Sha256Hasher;
use rustix::fs::{CWD, Mode, OFlags, ResolveFlags, fchmod, openat2};

#[derive(Debug)]
pub(crate) struct FinalizedPrivateFile {
    pub(crate) canonical_path: PathBuf,
    pub(crate) bytes: Vec<u8>,
    pub(crate) sha256: String,
}

pub(crate) fn read_finalized_private_file(
    path: &Path,
    repository_root: &Path,
    maximum_bytes: u64,
    label: &str,
) -> anyhow::Result<FinalizedPrivateFile> {
    require_absolute_normal_path(path, label)?;
    let path_metadata = fs::symlink_metadata(path)
        .with_context(|| format!("{label} is unavailable or unreadable"))?;
    require_private_regular_file(&path_metadata, maximum_bytes, label)?;
    let canonical_path =
        fs::canonicalize(path).with_context(|| format!("{label} is unavailable or unreadable"))?;
    let canonical_repository = fs::canonicalize(repository_root)
        .context("repository root is unavailable while reading private evidence")?;
    if canonical_path.starts_with(&canonical_repository) {
        bail!("{label} must remain outside the repository");
    }
    let parent = path
        .parent()
        .with_context(|| format!("{label} has no parent directory"))?;
    let parent_metadata =
        fs::symlink_metadata(parent).with_context(|| format!("{label} parent is unavailable"))?;
    if parent_metadata.file_type().is_symlink()
        || !parent_metadata.is_dir()
        || parent_metadata.permissions().mode() & 0o077 != 0
    {
        bail!("{label} parent must be one private nonsymlink directory");
    }

    let descriptor = openat2(
        CWD,
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
    )
    .with_context(|| format!("{label} could not be opened without symlink traversal"))?;
    let mut file = File::from(descriptor);
    let before = file
        .metadata()
        .with_context(|| format!("{label} descriptor metadata is unavailable"))?;
    require_private_regular_file(&before, maximum_bytes, label)?;
    if !same_generation(&path_metadata, &before) {
        bail!("{label} path changed while opened");
    }

    let mut bytes = Vec::with_capacity(usize::try_from(before.len()).unwrap_or(0));
    Read::by_ref(&mut file)
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("{label} could not be read"))?;
    let after = file
        .metadata()
        .with_context(|| format!("{label} descriptor metadata is unavailable after read"))?;
    let path_after = fs::symlink_metadata(path)
        .with_context(|| format!("{label} path is unavailable after read"))?;
    let canonical_after =
        fs::canonicalize(path).with_context(|| format!("{label} path changed after read"))?;
    if bytes.is_empty()
        || u64::try_from(bytes.len()).ok() != Some(before.len())
        || !same_generation(&before, &after)
        || !same_generation(&after, &path_after)
        || canonical_after != canonical_path
    {
        bail!("{label} changed while read");
    }
    let sha256 = Sha256Hasher::digest(&bytes).to_string();
    Ok(FinalizedPrivateFile {
        canonical_path,
        bytes,
        sha256,
    })
}

pub(crate) fn write_new_synced_private_file(
    path: &Path,
    bytes: &[u8],
    maximum_bytes: u64,
    label: &str,
) -> anyhow::Result<()> {
    if bytes.is_empty() || u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum_bytes {
        bail!("{label} bytes are empty or exceed their bound");
    }
    require_absolute_normal_path(path, label)?;
    let parent = path
        .parent()
        .with_context(|| format!("{label} has no parent directory"))?;
    let parent_metadata =
        fs::symlink_metadata(parent).with_context(|| format!("{label} parent is unavailable"))?;
    if parent_metadata.file_type().is_symlink()
        || !parent_metadata.is_dir()
        || parent_metadata.permissions().mode() & 0o077 != 0
    {
        bail!("{label} parent must be one existing private nonsymlink directory");
    }
    let descriptor = openat2(
        CWD,
        path,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
        ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
    )
    .with_context(|| format!("failed to create {label} without symlink traversal"))?;
    let mut file = File::from(descriptor);
    fchmod(&file, Mode::RUSR | Mode::WUSR)
        .with_context(|| format!("failed to set exact mode 0600 on {label}"))?;
    let created = file
        .metadata()
        .with_context(|| format!("failed to inspect newly created {label}"))?;
    if !created.is_file()
        || created.file_type().is_symlink()
        || created.permissions().mode() & 0o777 != 0o600
        || created.nlink() != 1
    {
        bail!("newly created {label} is not one mode-0600 single-link regular file");
    }
    file.write_all(bytes)
        .with_context(|| format!("failed to write {label}"))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {label}"))?;
    let finalized = file
        .metadata()
        .with_context(|| format!("failed to inspect finalized {label}"))?;
    require_private_regular_file(&finalized, maximum_bytes, label)?;
    if finalized.len() != u64::try_from(bytes.len()).unwrap_or(u64::MAX) {
        bail!("finalized {label} length differs from the written bytes");
    }
    let parent_descriptor = openat2(
        CWD,
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
    )
    .with_context(|| format!("failed to reopen {label} parent without symlink traversal"))?;
    File::from(parent_descriptor)
        .sync_all()
        .with_context(|| format!("failed to sync {label} parent"))?;
    Ok(())
}

fn require_absolute_normal_path(path: &Path, label: &str) -> anyhow::Result<()> {
    if !path.is_absolute()
        || path.components().any(|component| {
            !matches!(
                component,
                Component::RootDir | Component::Normal(_) | Component::Prefix(_)
            )
        })
    {
        bail!("{label} path must be absolute and contain only normal components");
    }
    Ok(())
}

fn require_private_regular_file(
    metadata: &fs::Metadata,
    maximum_bytes: u64,
    label: &str,
) -> anyhow::Result<()> {
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > maximum_bytes
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.nlink() != 1
    {
        bail!("{label} must be one bounded mode-0600 single-link regular file");
    }
    Ok(())
}

fn same_generation(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.mode() == right.mode()
        && left.nlink() == right.nlink()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalized_private_file_is_current_single_link_and_mode_0600() {
        let repository = tempfile::tempdir().unwrap();
        let evidence = tempfile::tempdir().unwrap();
        fs::set_permissions(evidence.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = evidence.path().join("raw.json");
        write_new_synced_private_file(&path, b"{}\n", 1024, "test evidence").unwrap();
        let read =
            read_finalized_private_file(&path, repository.path(), 1024, "test evidence").unwrap();
        assert_eq!(read.bytes, b"{}\n");

        let linked = evidence.path().join("linked.json");
        fs::hard_link(&path, &linked).unwrap();
        assert!(
            read_finalized_private_file(&path, repository.path(), 1024, "test evidence").is_err()
        );
    }

    #[test]
    fn finalized_private_file_rejects_permissions_symlinks_and_repository_paths() {
        let repository = tempfile::tempdir().unwrap();
        let evidence = tempfile::tempdir().unwrap();
        fs::set_permissions(repository.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(evidence.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = evidence.path().join("raw.json");
        write_new_synced_private_file(&path, b"{}\n", 1024, "test evidence").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(
            read_finalized_private_file(&path, repository.path(), 1024, "test evidence").is_err()
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let symlink = evidence.path().join("symlink.json");
        std::os::unix::fs::symlink(&path, &symlink).unwrap();
        assert!(
            read_finalized_private_file(&symlink, repository.path(), 1024, "test evidence")
                .is_err()
        );

        let inside = repository.path().join("inside.json");
        write_new_synced_private_file(&inside, b"{}\n", 1024, "test evidence").unwrap();
        assert!(
            read_finalized_private_file(&inside, repository.path(), 1024, "test evidence").is_err()
        );
    }
}
