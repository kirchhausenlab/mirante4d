use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::Path,
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use thiserror::Error;

use crate::{
    LocalPackageReader, PackageObjectKind, PackagePath, ProfileKind, RangeReadError,
    StorageProfileError, profile_limits,
};

const GLOBAL_OBJECTS_MAX: u64 = profile_limits(ProfileKind::Ds4).total_physical_objects;
const GLOBAL_DIRECTORIES_MAX: u64 = profile_limits(ProfileKind::Ds4).directories;
const GLOBAL_DIRECTORY_FAN_OUT_MAX: u64 =
    profile_limits(ProfileKind::Ds4).maximum_directory_fan_out;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExpectedFileRole {
    Descriptor(PackageObjectKind),
    ManifestPage,
    ManifestRoot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExpectedFile {
    pub(crate) bytes: u64,
    pub(crate) role: ExpectedFileRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

/// Exact bounded facts from one strict local package directory closure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirectoryInventory {
    regular_files: u64,
    directories: u64,
    maximum_directory_depth: u64,
    maximum_directory_fan_out: u64,
    pixel_shards: u64,
    validity_shards: u64,
    packed_index_shards: u64,
    zarr_metadata_objects: u64,
    portable_records: u64,
    manifest_pages: u64,
    fixed_control_objects: u64,
}

impl DirectoryInventory {
    pub const fn regular_files(self) -> u64 {
        self.regular_files
    }

    pub const fn directories(self) -> u64 {
        self.directories
    }

    pub const fn maximum_directory_fan_out(self) -> u64 {
        self.maximum_directory_fan_out
    }

    pub const fn maximum_directory_depth(self) -> u64 {
        self.maximum_directory_depth
    }

    pub const fn pixel_shards(self) -> u64 {
        self.pixel_shards
    }

    pub const fn validity_shards(self) -> u64 {
        self.validity_shards
    }

    pub const fn packed_index_shards(self) -> u64 {
        self.packed_index_shards
    }

    pub const fn zarr_metadata_objects(self) -> u64 {
        self.zarr_metadata_objects
    }

    pub const fn portable_records(self) -> u64 {
        self.portable_records
    }

    pub const fn manifest_pages(self) -> u64 {
        self.manifest_pages
    }

    pub const fn fixed_control_objects(self) -> u64 {
        self.fixed_control_objects
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DirectoryInventoryError {
    #[error(transparent)]
    Path(#[from] StorageProfileError),
    #[error(transparent)]
    Range(#[from] RangeReadError),
    #[error("directory inventory was cancelled")]
    Cancelled,
    #[error("package declares {actual} physical objects; global maximum is {maximum}")]
    ObjectCountExceeded { actual: u64, maximum: u64 },
    #[error("package requires or contains {actual} directories; global maximum is {maximum}")]
    DirectoryCountExceeded { actual: u64, maximum: u64 },
    #[error("directory {path} has more than {maximum} direct children")]
    DirectoryFanOutExceeded { path: String, maximum: u64 },
    #[error("package contains unlisted file {path}")]
    UnexpectedFile { path: String },
    #[error("package is missing listed file {path}")]
    MissingFile { path: String },
    #[error("package contains an unlisted directory {path}")]
    UnexpectedDirectory { path: String },
    #[error("package directory {path} contains a non-UTF-8 name")]
    NonUtf8Name { path: String },
    #[error("object {path} has {actual} bytes; expected {expected}")]
    ObjectLengthMismatch {
        path: String,
        expected: u64,
        actual: u64,
    },
    #[error("manifest authority changed after metadata open")]
    ManifestAuthorityChanged,
    #[error("{operation} failed for {path}: {kind:?}")]
    Io {
        operation: &'static str,
        path: String,
        kind: io::ErrorKind,
    },
}

pub(crate) fn inspect_directory_closure(
    reader: &LocalPackageReader,
    mut expected_files: BTreeMap<PackagePath, ExpectedFile>,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<DirectoryInventory, DirectoryInventoryError> {
    check_cancelled(&mut is_cancelled)?;
    let expected_file_count = checked_len(expected_files.len())?;
    if expected_file_count > GLOBAL_OBJECTS_MAX {
        return Err(DirectoryInventoryError::ObjectCountExceeded {
            actual: expected_file_count,
            maximum: GLOBAL_OBJECTS_MAX,
        });
    }
    let mut expected_directories = expected_directory_closure(expected_files.keys())?;
    let expected_directory_count = checked_len(expected_directories.len())?;
    if expected_directory_count > GLOBAL_DIRECTORIES_MAX {
        return Err(DirectoryInventoryError::DirectoryCountExceeded {
            actual: expected_directory_count,
            maximum: GLOBAL_DIRECTORIES_MAX,
        });
    }

    reader.validate_root_identity()?;
    expected_directories.remove("");
    let mut pending = vec![(reader.root_path().to_path_buf(), String::new())];
    let mut visited_directories = Vec::new();
    let mut visited_files = Vec::new();
    let mut inventory = DirectoryInventory::default();
    while let Some((directory, relative)) = pending.pop() {
        check_cancelled(&mut is_cancelled)?;
        let before = checked_directory_identity(reader, &directory, &relative)?;
        inventory.directories = checked_increment(inventory.directories)?;
        let depth = if relative.is_empty() {
            0
        } else {
            checked_len(relative.split('/').count())?
        };
        inventory.maximum_directory_depth = inventory.maximum_directory_depth.max(depth);
        if inventory.directories > GLOBAL_DIRECTORIES_MAX {
            return Err(DirectoryInventoryError::DirectoryCountExceeded {
                actual: inventory.directories,
                maximum: GLOBAL_DIRECTORIES_MAX,
            });
        }

        let mut children = Vec::new();
        let entries = fs::read_dir(&directory)
            .map_err(|error| io_error("read directory", display_directory(&relative), error))?;
        for entry in entries {
            check_cancelled(&mut is_cancelled)?;
            if checked_len(children.len())? >= GLOBAL_DIRECTORY_FAN_OUT_MAX {
                return Err(DirectoryInventoryError::DirectoryFanOutExceeded {
                    path: display_directory(&relative).to_owned(),
                    maximum: GLOBAL_DIRECTORY_FAN_OUT_MAX,
                });
            }
            let entry = entry.map_err(|error| {
                io_error("read directory entry", display_directory(&relative), error)
            })?;
            let name = entry.file_name().into_string().map_err(|_| {
                DirectoryInventoryError::NonUtf8Name {
                    path: display_directory(&relative).to_owned(),
                }
            })?;
            children.push((name, entry.path()));
        }
        let fan_out = checked_len(children.len())?;
        inventory.maximum_directory_fan_out = inventory.maximum_directory_fan_out.max(fan_out);
        children.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        let after = checked_directory_identity(reader, &directory, &relative)?;
        if before != after {
            return Err(RangeReadError::ObjectChanged {
                path: display_directory(&relative).to_owned(),
            }
            .into());
        }
        visited_directories.push((directory.clone(), relative.clone(), after));

        for (name, full_path) in children.into_iter().rev() {
            check_cancelled(&mut is_cancelled)?;
            let child = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            let metadata = fs::symlink_metadata(&full_path)
                .map_err(|error| io_error("inspect directory entry", &child, error))?;
            if metadata.file_type().is_symlink() {
                return Err(RangeReadError::Symlink { path: child }.into());
            }
            if metadata.is_dir() {
                if !expected_directories.remove(&child) {
                    return Err(DirectoryInventoryError::UnexpectedDirectory { path: child });
                }
                pending.push((full_path, child));
                continue;
            }
            if !metadata.is_file() {
                return Err(RangeReadError::NonRegularObject { path: child }.into());
            }

            let path = PackagePath::parse(&child)?;
            let expected = expected_files.remove(&path).ok_or_else(|| {
                DirectoryInventoryError::UnexpectedFile {
                    path: path.to_string(),
                }
            })?;
            let actual = reader.object_info(&path, crate::GLOBAL_ENCODED_OUTER_SHARD_BYTES_MAX)?;
            if actual.bytes() != expected.bytes {
                return Err(DirectoryInventoryError::ObjectLengthMismatch {
                    path: path.to_string(),
                    expected: expected.bytes,
                    actual: actual.bytes(),
                });
            }
            inventory.regular_files = checked_increment(inventory.regular_files)?;
            if inventory.regular_files > GLOBAL_OBJECTS_MAX {
                return Err(DirectoryInventoryError::ObjectCountExceeded {
                    actual: inventory.regular_files,
                    maximum: GLOBAL_OBJECTS_MAX,
                });
            }
            record_role(&mut inventory, expected.role)?;
            visited_files.push((path, expected.bytes));
        }
    }
    for (path, expected) in visited_files {
        check_cancelled(&mut is_cancelled)?;
        let actual = reader.object_info(&path, crate::GLOBAL_ENCODED_OUTER_SHARD_BYTES_MAX)?;
        if actual.bytes() != expected {
            return Err(DirectoryInventoryError::ObjectLengthMismatch {
                path: path.to_string(),
                expected,
                actual: actual.bytes(),
            });
        }
    }
    for (directory, relative, identity) in visited_directories {
        check_cancelled(&mut is_cancelled)?;
        if checked_directory_identity(reader, &directory, &relative)? != identity {
            return Err(RangeReadError::ObjectChanged {
                path: display_directory(&relative).to_owned(),
            }
            .into());
        }
    }
    reader.validate_root_identity()?;

    if let Some(path) = expected_files.keys().next() {
        return Err(DirectoryInventoryError::MissingFile {
            path: path.to_string(),
        });
    }
    debug_assert!(expected_directories.is_empty());
    Ok(inventory)
}

fn expected_directory_closure<'a>(
    paths: impl IntoIterator<Item = &'a PackagePath>,
) -> Result<BTreeSet<String>, DirectoryInventoryError> {
    let mut directories = BTreeSet::from([String::new()]);
    for path in paths {
        let components = path.as_str().split('/').collect::<Vec<_>>();
        for depth in 1..components.len() {
            directories.insert(components[..depth].join("/"));
            if checked_len(directories.len())? > GLOBAL_DIRECTORIES_MAX {
                return Err(DirectoryInventoryError::DirectoryCountExceeded {
                    actual: checked_len(directories.len())?,
                    maximum: GLOBAL_DIRECTORIES_MAX,
                });
            }
        }
    }
    Ok(directories)
}

fn record_role(
    inventory: &mut DirectoryInventory,
    role: ExpectedFileRole,
) -> Result<(), DirectoryInventoryError> {
    match role {
        ExpectedFileRole::ManifestPage => {
            inventory.manifest_pages = checked_increment(inventory.manifest_pages)?;
        }
        ExpectedFileRole::ManifestRoot => {
            inventory.fixed_control_objects = checked_increment(inventory.fixed_control_objects)?;
        }
        ExpectedFileRole::Descriptor(kind) => match kind {
            PackageObjectKind::PixelShard => {
                inventory.pixel_shards = checked_increment(inventory.pixel_shards)?;
            }
            PackageObjectKind::ValidityShard => {
                inventory.validity_shards = checked_increment(inventory.validity_shards)?;
            }
            PackageObjectKind::PackedIndexShard => {
                inventory.packed_index_shards = checked_increment(inventory.packed_index_shards)?;
            }
            PackageObjectKind::ZarrRoot
            | PackageObjectKind::ZarrImagesGroup
            | PackageObjectKind::ZarrValidityGroup
            | PackageObjectKind::ZarrIndexesGroup
            | PackageObjectKind::ZarrImageGroup
            | PackageObjectKind::ZarrPixelArray
            | PackageObjectKind::ZarrValidityArray
            | PackageObjectKind::ZarrPackedIndexArray => {
                inventory.zarr_metadata_objects =
                    checked_increment(inventory.zarr_metadata_objects)?;
            }
            PackageObjectKind::PortableRecord => {
                inventory.portable_records = checked_increment(inventory.portable_records)?;
            }
            PackageObjectKind::Profile
            | PackageObjectKind::Science
            | PackageObjectKind::DisplayDefaults => {
                inventory.fixed_control_objects =
                    checked_increment(inventory.fixed_control_objects)?;
            }
        },
    }
    Ok(())
}

#[cfg(unix)]
fn checked_directory_identity(
    reader: &LocalPackageReader,
    directory: &Path,
    relative: &str,
) -> Result<DirectoryIdentity, DirectoryInventoryError> {
    reader.validate_root_identity()?;
    let metadata = fs::symlink_metadata(directory)
        .map_err(|error| io_error("inspect directory", display_directory(relative), error))?;
    if metadata.file_type().is_symlink() {
        return Err(RangeReadError::Symlink {
            path: display_directory(relative).to_owned(),
        }
        .into());
    }
    if !metadata.is_dir() {
        return Err(RangeReadError::NonDirectoryComponent {
            path: display_directory(relative).to_owned(),
        }
        .into());
    }
    let canonical = fs::canonicalize(directory)
        .map_err(|error| io_error("canonicalize directory", display_directory(relative), error))?;
    if !canonical.starts_with(reader.root_path()) {
        return Err(RangeReadError::EscapedRoot {
            path: display_directory(relative).to_owned(),
        }
        .into());
    }
    Ok(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
    })
}

#[cfg(not(unix))]
fn checked_directory_identity(
    _reader: &LocalPackageReader,
    _directory: &Path,
    _relative: &str,
) -> Result<DirectoryIdentity, DirectoryInventoryError> {
    Err(RangeReadError::UnsupportedPlatform.into())
}

fn check_cancelled(is_cancelled: &mut impl FnMut() -> bool) -> Result<(), DirectoryInventoryError> {
    if is_cancelled() {
        Err(DirectoryInventoryError::Cancelled)
    } else {
        Ok(())
    }
}

fn checked_increment(value: u64) -> Result<u64, DirectoryInventoryError> {
    value
        .checked_add(1)
        .ok_or(DirectoryInventoryError::ObjectCountExceeded {
            actual: u64::MAX,
            maximum: GLOBAL_OBJECTS_MAX,
        })
}

fn checked_len(value: usize) -> Result<u64, DirectoryInventoryError> {
    u64::try_from(value).map_err(|_| DirectoryInventoryError::ObjectCountExceeded {
        actual: u64::MAX,
        maximum: GLOBAL_OBJECTS_MAX,
    })
}

fn display_directory(relative: &str) -> &str {
    if relative.is_empty() {
        "<root>"
    } else {
        relative
    }
}

fn io_error(operation: &'static str, path: &str, error: io::Error) -> DirectoryInventoryError {
    DirectoryInventoryError::Io {
        operation,
        path: path.to_owned(),
        kind: error.kind(),
    }
}
