use std::{collections::BTreeSet, fmt, str::FromStr};

use crate::StorageProfileError;

pub const MAX_RELATIVE_PATH_BYTES: usize = 240;
pub const MAX_DIRECTORY_DEPTH: usize = 8;
pub const MAX_FILE_PATH_COMPONENTS: usize = 9;

/// A portable lowercase-ASCII package-relative file path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackagePath(String);

impl PackagePath {
    pub fn parse(value: &str) -> Result<Self, StorageProfileError> {
        value.parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn component_count(&self) -> usize {
        self.0.split('/').count()
    }

    pub fn directory_depth(&self) -> usize {
        self.component_count().saturating_sub(1)
    }
}

impl FromStr for PackagePath {
    type Err = StorageProfileError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        validate_path(value)?;
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for PackagePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Rejects duplicate normalized paths. Case-fold and Unicode-normalization
/// collisions are impossible because the grammar permits lowercase ASCII only.
pub fn validate_unique_paths<'a>(
    paths: impl IntoIterator<Item = &'a PackagePath>,
) -> Result<(), StorageProfileError> {
    let mut seen = BTreeSet::new();
    for path in paths {
        if !seen.insert(path.as_str()) {
            return Err(StorageProfileError::DuplicatePath {
                path: path.to_string(),
            });
        }
    }
    Ok(())
}

fn validate_path(value: &str) -> Result<(), StorageProfileError> {
    if value.is_empty() {
        return invalid("the value is empty");
    }
    if value.len() > MAX_RELATIVE_PATH_BYTES {
        return invalid("the value exceeds 240 UTF-8 bytes");
    }
    if !value.is_ascii() {
        return invalid("only ASCII is permitted");
    }
    if value.starts_with('/') {
        return invalid("absolute paths are forbidden");
    }
    if value.contains('\\') {
        return invalid("backslashes are forbidden");
    }

    let mut components = 0_usize;
    for component in value.split('/') {
        components += 1;
        if component.is_empty() {
            return invalid("empty path components are forbidden");
        }
        if matches!(component, "." | "..") {
            return invalid("dot and parent components are forbidden");
        }
        if !component.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        }) {
            return invalid(
                "components may contain only lowercase ASCII letters, digits, '.', '_' and '-'",
            );
        }
    }
    if components > MAX_FILE_PATH_COMPONENTS {
        return invalid("the file path exceeds nine components");
    }
    if components.saturating_sub(1) > MAX_DIRECTORY_DEPTH {
        return invalid("the directory depth exceeds eight");
    }
    Ok(())
}

fn invalid<T>(reason: &'static str) -> Result<T, StorageProfileError> {
    Err(StorageProfileError::InvalidPath { reason })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_frozen_paths_and_reports_depth() {
        let path = PackagePath::parse("images/i00000000/s00/c/0/0/0/0/0").unwrap();
        assert_eq!(path.component_count(), 9);
        assert_eq!(path.directory_depth(), 8);
    }

    #[test]
    fn rejects_nonportable_and_overdeep_paths() {
        for value in [
            "",
            "/zarr.json",
            "A/zarr.json",
            "a\\zarr.json",
            "a//b",
            "a/../b",
            "é/zarr.json",
            "a/b/c/d/e/f/g/h/i/j",
        ] {
            assert!(PackagePath::parse(value).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn rejects_duplicate_paths() {
        let path = PackagePath::parse("m4d/profile.json").unwrap();
        assert!(validate_unique_paths([&path, &path]).is_err());
    }
}
