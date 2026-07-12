use std::{
    fs::{self, File, Metadata},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use thiserror::Error;

use crate::{GLOBAL_ENCODED_OUTER_SHARD_BYTES_MAX, PackagePath};

pub const SHARD_INDEX_RANGE_READ_BYTES_MAX: u64 = 4_096;

/// Metadata for one checked regular object in a local target package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalObjectInfo {
    bytes: u64,
}

impl LocalObjectInfo {
    pub const fn bytes(self) -> u64 {
        self.bytes
    }
}

/// Read-only, root-confined access to an immutable local package.
///
/// WP-10A currently claims Linux/Unix local storage only. Every object open
/// rejects symlinks, hardlinks, non-regular files, root escape, and identity
/// changes around the open before any bytes are returned.
#[derive(Debug)]
pub struct LocalPackageReader {
    root: PathBuf,
    root_identity: FileIdentity,
}

impl LocalPackageReader {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, RangeReadError> {
        #[cfg(not(unix))]
        {
            let _ = root;
            return Err(RangeReadError::UnsupportedPlatform);
        }

        #[cfg(unix)]
        {
            let root = root.as_ref();
            let metadata = symlink_metadata(root, "inspect package root", "<root>")?;
            if metadata.file_type().is_symlink() {
                return Err(RangeReadError::Symlink {
                    path: "<root>".to_owned(),
                });
            }
            if !metadata.is_dir() {
                return Err(RangeReadError::RootNotDirectory);
            }
            let canonical = fs::canonicalize(root)
                .map_err(|error| io_error("canonicalize package root", "<root>", error))?;
            let canonical_metadata =
                symlink_metadata(&canonical, "reinspect package root", "<root>")?;
            if canonical_metadata.file_type().is_symlink() || !canonical_metadata.is_dir() {
                return Err(RangeReadError::RootNotDirectory);
            }
            Ok(Self {
                root: canonical,
                root_identity: FileIdentity::from_metadata(&canonical_metadata),
            })
        }
    }

    pub fn object_info(
        &self,
        path: &PackagePath,
        object_bytes_max: u64,
    ) -> Result<LocalObjectInfo, RangeReadError> {
        let checked = self.open_object(path, object_bytes_max)?;
        Ok(LocalObjectInfo {
            bytes: checked.bytes,
        })
    }

    pub(crate) fn read_object(
        &self,
        path: &PackagePath,
        object_bytes_max: u64,
    ) -> Result<Vec<u8>, RangeReadError> {
        let mut checked = self.open_object(path, object_bytes_max)?;
        let bytes = read_exact_at(&mut checked.file, path, 0, checked.bytes)?;
        self.revalidate_open_object(path, &checked)?;
        Ok(bytes)
    }

    /// Reads one nonempty checked `(object, offset, length)` range.
    pub fn read_range(
        &self,
        path: &PackagePath,
        offset: u64,
        length: u64,
        object_bytes_max: u64,
    ) -> Result<Vec<u8>, RangeReadError> {
        if length == 0 {
            return Err(RangeReadError::EmptyRange);
        }
        let end = offset
            .checked_add(length)
            .ok_or(RangeReadError::RangeOverflow)?;
        let mut checked = self.open_object(path, object_bytes_max)?;
        if end > checked.bytes {
            return Err(RangeReadError::RangeOutOfBounds {
                offset,
                length,
                object_bytes: checked.bytes,
            });
        }
        let bytes = read_exact_at(&mut checked.file, path, offset, length)?;
        self.revalidate_open_object(path, &checked)?;
        Ok(bytes)
    }

    /// Reads the exact fixed shard-index tail without reading a whole shard.
    pub fn read_shard_index_tail(
        &self,
        path: &PackagePath,
        tail_bytes: u64,
        object_bytes_max: u64,
    ) -> Result<(Vec<u8>, u64), RangeReadError> {
        if tail_bytes == 0 || tail_bytes > SHARD_INDEX_RANGE_READ_BYTES_MAX {
            return Err(RangeReadError::InvalidShardIndexRange {
                actual: tail_bytes,
                maximum: SHARD_INDEX_RANGE_READ_BYTES_MAX,
            });
        }
        let mut checked = self.open_object(path, object_bytes_max)?;
        if tail_bytes > checked.bytes {
            return Err(RangeReadError::RangeOutOfBounds {
                offset: 0,
                length: tail_bytes,
                object_bytes: checked.bytes,
            });
        }
        let payload_bytes = checked.bytes - tail_bytes;
        let bytes = read_exact_at(&mut checked.file, path, payload_bytes, tail_bytes)?;
        self.revalidate_open_object(path, &checked)?;
        Ok((bytes, payload_bytes))
    }

    #[cfg(unix)]
    fn open_object(
        &self,
        path: &PackagePath,
        object_bytes_max: u64,
    ) -> Result<CheckedObject, RangeReadError> {
        validate_object_limit(object_bytes_max)?;
        self.validate_root_identity()?;
        let full_path = self.root.join(path.as_str());
        self.validate_components(path, &full_path)?;
        let canonical = fs::canonicalize(&full_path)
            .map_err(|error| io_error("canonicalize object", path.as_str(), error))?;
        if !canonical.starts_with(&self.root) {
            return Err(RangeReadError::EscapedRoot {
                path: path.to_string(),
            });
        }

        let file = File::open(&full_path)
            .map_err(|error| io_error("open object", path.as_str(), error))?;
        let opened = file
            .metadata()
            .map_err(|error| io_error("inspect opened object", path.as_str(), error))?;
        self.validate_components(path, &full_path)?;
        let post_open = symlink_metadata(&full_path, "reinspect object", path.as_str())?;
        validate_regular_identity(path, &opened, &post_open)?;
        let canonical_after = fs::canonicalize(&full_path)
            .map_err(|error| io_error("recanonicalize object", path.as_str(), error))?;
        if canonical_after != canonical || !canonical_after.starts_with(&self.root) {
            return Err(RangeReadError::ObjectChanged {
                path: path.to_string(),
            });
        }
        if opened.len() > object_bytes_max {
            return Err(RangeReadError::ObjectTooLarge {
                path: path.to_string(),
                actual: opened.len(),
                maximum: object_bytes_max,
            });
        }
        Ok(CheckedObject {
            file,
            full_path,
            bytes: opened.len(),
            identity: FileIdentity::from_metadata(&opened),
        })
    }

    #[cfg(not(unix))]
    fn open_object(
        &self,
        _path: &PackagePath,
        _object_bytes_max: u64,
    ) -> Result<CheckedObject, RangeReadError> {
        Err(RangeReadError::UnsupportedPlatform)
    }

    #[cfg(unix)]
    fn validate_root_identity(&self) -> Result<(), RangeReadError> {
        let metadata = symlink_metadata(&self.root, "reinspect package root", "<root>")?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || FileIdentity::from_metadata(&metadata) != self.root_identity
        {
            return Err(RangeReadError::RootChanged);
        }
        Ok(())
    }

    #[cfg(unix)]
    fn validate_components(
        &self,
        path: &PackagePath,
        full_path: &Path,
    ) -> Result<(), RangeReadError> {
        let mut current = self.root.clone();
        let component_count = path.component_count();
        for (index, component) in path.as_str().split('/').enumerate() {
            current.push(component);
            let metadata = symlink_metadata(&current, "inspect path component", path.as_str())?;
            if metadata.file_type().is_symlink() {
                return Err(RangeReadError::Symlink {
                    path: path.to_string(),
                });
            }
            if index + 1 == component_count {
                if current != full_path || !metadata.is_file() {
                    return Err(RangeReadError::NonRegularObject {
                        path: path.to_string(),
                    });
                }
            } else if !metadata.is_dir() {
                return Err(RangeReadError::NonDirectoryComponent {
                    path: path.to_string(),
                });
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    fn revalidate_open_object(
        &self,
        path: &PackagePath,
        checked: &CheckedObject,
    ) -> Result<(), RangeReadError> {
        self.validate_root_identity()?;
        self.validate_components(path, &checked.full_path)?;
        let opened = checked
            .file
            .metadata()
            .map_err(|error| io_error("reinspect opened object", path.as_str(), error))?;
        let current = symlink_metadata(&checked.full_path, "reinspect object", path.as_str())?;
        validate_regular_identity(path, &opened, &current)?;
        if FileIdentity::from_metadata(&opened) != checked.identity || opened.len() != checked.bytes
        {
            return Err(RangeReadError::ObjectChanged {
                path: path.to_string(),
            });
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn revalidate_open_object(
        &self,
        _path: &PackagePath,
        _checked: &CheckedObject,
    ) -> Result<(), RangeReadError> {
        Err(RangeReadError::UnsupportedPlatform)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RangeReadError {
    #[error("strict local package reads are currently supported only on Unix")]
    UnsupportedPlatform,
    #[error("package root is not a real directory")]
    RootNotDirectory,
    #[error("package root changed after it was opened")]
    RootChanged,
    #[error("package path {path} contains a symlink")]
    Symlink { path: String },
    #[error("package path {path} contains a non-directory parent component")]
    NonDirectoryComponent { path: String },
    #[error("package object {path} is not a regular file")]
    NonRegularObject { path: String },
    #[error("package object {path} has {links} hardlinks; exactly one is required")]
    Hardlink { path: String, links: u64 },
    #[error("package object {path} escaped the package root")]
    EscapedRoot { path: String },
    #[error("package object {path} changed while it was being opened or read")]
    ObjectChanged { path: String },
    #[error("package object {path} has {actual} bytes; maximum is {maximum}")]
    ObjectTooLarge {
        path: String,
        actual: u64,
        maximum: u64,
    },
    #[error("object byte limit must be in 1 through {maximum}, observed {actual}")]
    InvalidObjectLimit { actual: u64, maximum: u64 },
    #[error("range reads must be nonempty")]
    EmptyRange,
    #[error("range offset plus length overflowed u64")]
    RangeOverflow,
    #[error("range ({offset}, {length}) exceeds the {object_bytes}-byte package object")]
    RangeOutOfBounds {
        offset: u64,
        length: u64,
        object_bytes: u64,
    },
    #[error("shard-index range has {actual} bytes; expected 1 through {maximum}")]
    InvalidShardIndexRange { actual: u64, maximum: u64 },
    #[error("range length cannot be represented as usize")]
    LengthOverflow,
    #[error("short range read for {path}: expected {expected} bytes")]
    ShortRead { path: String, expected: u64 },
    #[error("{operation} failed for {path}: {kind:?}")]
    Io {
        operation: &'static str,
        path: String,
        kind: io::ErrorKind,
    },
}

#[derive(Debug)]
struct CheckedObject {
    file: File,
    full_path: PathBuf,
    bytes: u64,
    identity: FileIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    #[cfg(unix)]
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[cfg(unix)]
fn validate_regular_identity(
    path: &PackagePath,
    opened: &Metadata,
    current: &Metadata,
) -> Result<(), RangeReadError> {
    if current.file_type().is_symlink() || !opened.is_file() || !current.is_file() {
        return Err(RangeReadError::NonRegularObject {
            path: path.to_string(),
        });
    }
    if opened.nlink() != 1 || current.nlink() != 1 {
        return Err(RangeReadError::Hardlink {
            path: path.to_string(),
            links: opened.nlink().max(current.nlink()),
        });
    }
    if FileIdentity::from_metadata(opened) != FileIdentity::from_metadata(current) {
        return Err(RangeReadError::ObjectChanged {
            path: path.to_string(),
        });
    }
    Ok(())
}

fn read_exact_at(
    file: &mut File,
    path: &PackagePath,
    offset: u64,
    length: u64,
) -> Result<Vec<u8>, RangeReadError> {
    let length = usize::try_from(length).map_err(|_| RangeReadError::LengthOverflow)?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| io_error("seek object", path.as_str(), error))?;
    let mut bytes = vec![0; length];
    match file.read_exact(&mut bytes) {
        Ok(()) => Ok(bytes),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            Err(RangeReadError::ShortRead {
                path: path.to_string(),
                expected: u64::try_from(length).map_err(|_| RangeReadError::LengthOverflow)?,
            })
        }
        Err(error) => Err(io_error("read object range", path.as_str(), error)),
    }
}

fn validate_object_limit(limit: u64) -> Result<(), RangeReadError> {
    if limit == 0 || limit > GLOBAL_ENCODED_OUTER_SHARD_BYTES_MAX {
        return Err(RangeReadError::InvalidObjectLimit {
            actual: limit,
            maximum: GLOBAL_ENCODED_OUTER_SHARD_BYTES_MAX,
        });
    }
    Ok(())
}

fn symlink_metadata(
    path: &Path,
    operation: &'static str,
    display_path: &str,
) -> Result<Metadata, RangeReadError> {
    fs::symlink_metadata(path).map_err(|error| io_error(operation, display_path, error))
}

fn io_error(operation: &'static str, path: &str, error: io::Error) -> RangeReadError {
    RangeReadError::Io {
        operation,
        path: path.to_owned(),
        kind: error.kind(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "mirante4d-range-{}-{nonce}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn write(&self, relative: &str, bytes: &[u8]) {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    #[cfg(unix)]
    fn reads_only_the_requested_nonempty_range_and_index_tail() {
        let root = TempRoot::new();
        let bytes = (0_u16..8_192)
            .map(|value| (value % 251) as u8)
            .collect::<Vec<_>>();
        root.write("images/i00000000/s00/c/0/0/0/0/0", &bytes);
        let reader = LocalPackageReader::open(&root.0).unwrap();
        let path = PackagePath::parse("images/i00000000/s00/c/0/0/0/0/0").unwrap();

        assert_eq!(reader.object_info(&path, 8_192).unwrap().bytes(), 8_192);
        assert_eq!(
            reader.read_range(&path, 17, 31, 8_192).unwrap(),
            bytes[17..48]
        );
        let (tail, payload_bytes) = reader.read_shard_index_tail(&path, 260, 8_192).unwrap();
        assert_eq!(payload_bytes, 7_932);
        assert_eq!(tail, bytes[7_932..]);
    }

    #[test]
    #[cfg(unix)]
    fn rejects_empty_overflowing_out_of_bounds_and_oversized_reads() {
        let root = TempRoot::new();
        root.write("m4d/profile.json", &[1; 32]);
        let reader = LocalPackageReader::open(&root.0).unwrap();
        let path = PackagePath::parse("m4d/profile.json").unwrap();

        assert_eq!(
            reader.read_range(&path, 0, 0, 32),
            Err(RangeReadError::EmptyRange)
        );
        assert_eq!(
            reader.read_range(&path, u64::MAX, 2, 32),
            Err(RangeReadError::RangeOverflow)
        );
        assert!(matches!(
            reader.read_range(&path, 31, 2, 32),
            Err(RangeReadError::RangeOutOfBounds { .. })
        ));
        assert!(matches!(
            reader.object_info(&path, 31),
            Err(RangeReadError::ObjectTooLarge { .. })
        ));
        assert!(matches!(
            reader.object_info(&path, u64::MAX),
            Err(RangeReadError::InvalidObjectLimit { .. })
        ));
        assert!(matches!(
            reader.read_shard_index_tail(&path, 4_097, 32),
            Err(RangeReadError::InvalidShardIndexRange { .. })
        ));
    }

    #[test]
    #[cfg(unix)]
    fn rejects_symlink_hardlink_and_nonregular_objects() {
        use std::os::unix::fs::symlink;

        let root = TempRoot::new();
        root.write("outside.bin", &[7; 8]);
        fs::create_dir_all(root.0.join("m4d")).unwrap();
        symlink(root.0.join("outside.bin"), root.0.join("m4d/link.bin")).unwrap();
        fs::create_dir(root.0.join("real")).unwrap();
        fs::write(root.0.join("real/data.bin"), [9; 8]).unwrap();
        symlink(root.0.join("real"), root.0.join("linked")).unwrap();
        fs::hard_link(root.0.join("outside.bin"), root.0.join("m4d/hard.bin")).unwrap();
        fs::create_dir(root.0.join("m4d/directory.bin")).unwrap();
        let reader = LocalPackageReader::open(&root.0).unwrap();

        let link = PackagePath::parse("m4d/link.bin").unwrap();
        assert!(matches!(
            reader.object_info(&link, 8),
            Err(RangeReadError::Symlink { .. })
        ));
        let linked_parent = PackagePath::parse("linked/data.bin").unwrap();
        assert!(matches!(
            reader.object_info(&linked_parent, 8),
            Err(RangeReadError::Symlink { .. })
        ));
        let hard = PackagePath::parse("m4d/hard.bin").unwrap();
        assert!(matches!(
            reader.object_info(&hard, 8),
            Err(RangeReadError::Hardlink { .. })
        ));
        let directory = PackagePath::parse("m4d/directory.bin").unwrap();
        assert!(matches!(
            reader.object_info(&directory, 8),
            Err(RangeReadError::NonRegularObject { .. })
        ));
    }
}
