use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, bail};
use mirante4d_storage::LocalPackageCatalog;

const ARCHIVE: &str = "fixtures/target/archives/m4d-t1-u16-3d-multiscale.tar";
const FIXTURE_NAME: &str = "m4d-t1-u16-3d-multiscale";
const BLOCK_BYTES: usize = 512;
const ARCHIVE_BYTES_MAX: usize = 512 * 1024;
const ENTRY_COUNT_MAX: usize = 128;
const PATH_BYTES_MAX: usize = 240;

/// Extracts the existing promoted target U16 package for local product checks.
///
/// This is deliberately one fixed helper, not another fixture-generation path.
pub(crate) fn extract_target_u16_fixture(output_root: &Path) -> anyhow::Result<PathBuf> {
    let archive_path = repository_root().join(ARCHIVE);
    let archive = fs::read(&archive_path)
        .with_context(|| format!("failed to read {}", archive_path.display()))?;
    if archive.is_empty() || archive.len() > ARCHIVE_BYTES_MAX || archive.len() % BLOCK_BYTES != 0 {
        bail!("target fixture archive has an invalid bounded length");
    }

    fs::create_dir_all(output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let package = output_root.join(FIXTURE_NAME);
    if package.exists() {
        fs::remove_dir_all(&package)
            .with_context(|| format!("failed to replace {}", package.display()))?;
    }
    fs::create_dir(&package).with_context(|| format!("failed to create {}", package.display()))?;
    extract_ustar(&archive, &package)?;
    LocalPackageCatalog::open(&package)
        .with_context(|| format!("extracted target fixture {} is invalid", package.display()))?;
    Ok(package)
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn extract_ustar(archive: &[u8], root: &Path) -> anyhow::Result<()> {
    let mut offset = 0_usize;
    let mut entries = 0_usize;
    while offset + BLOCK_BYTES <= archive.len() {
        let header = &archive[offset..offset + BLOCK_BYTES];
        offset += BLOCK_BYTES;
        if header.iter().all(|byte| *byte == 0) {
            if archive[offset..].iter().any(|byte| *byte != 0) {
                bail!("target fixture archive has bytes after its terminator");
            }
            return Ok(());
        }
        entries += 1;
        if entries > ENTRY_COUNT_MAX {
            bail!("target fixture archive exceeds its entry bound");
        }
        if &header[257..263] != b"ustar\0" {
            bail!("target fixture archive is not USTAR");
        }

        let relative = archive_path(header)?;
        let destination = root.join(&relative);
        let size = parse_octal(&header[124..136])?;
        let size = usize::try_from(size).context("target fixture member is too large")?;
        let padded = size
            .checked_add(BLOCK_BYTES - 1)
            .context("target fixture member size overflowed")?
            / BLOCK_BYTES
            * BLOCK_BYTES;
        let end = offset
            .checked_add(padded)
            .context("target fixture member range overflowed")?;
        if end > archive.len() || offset + size > archive.len() {
            bail!("target fixture member extends past the archive");
        }

        match header[156] {
            b'5' => {
                if size != 0 {
                    bail!("target fixture directory contains a payload");
                }
                fs::create_dir_all(&destination).with_context(|| {
                    format!(
                        "failed to create fixture directory {}",
                        destination.display()
                    )
                })?;
            }
            0 | b'0' => {
                let parent = destination
                    .parent()
                    .context("target fixture file has no parent")?;
                fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create fixture directory {}", parent.display())
                })?;
                fs::write(&destination, &archive[offset..offset + size]).with_context(|| {
                    format!("failed to write fixture file {}", destination.display())
                })?;
            }
            kind => bail!("target fixture archive contains unsupported member type {kind}"),
        }
        offset = end;
    }
    bail!("target fixture archive has no terminator")
}

fn archive_path(header: &[u8]) -> anyhow::Result<PathBuf> {
    let name = string_field(&header[0..100])?;
    let prefix = string_field(&header[345..500])?;
    let encoded = if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    };
    if encoded.is_empty() || encoded.len() > PATH_BYTES_MAX {
        bail!("target fixture archive contains an invalid path length");
    }
    let path = PathBuf::from(encoded);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("target fixture archive contains an unsafe path");
    }
    Ok(path)
}

fn string_field(field: &[u8]) -> anyhow::Result<String> {
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    let value = &field[..end];
    if value.iter().any(|byte| !byte.is_ascii() || *byte == b'\\') {
        bail!("target fixture archive contains a non-portable path");
    }
    String::from_utf8(value.to_vec()).context("target fixture archive path is not UTF-8")
}

fn parse_octal(field: &[u8]) -> anyhow::Result<u64> {
    let value = field
        .iter()
        .copied()
        .take_while(|byte| *byte != 0)
        .filter(|byte| *byte != b' ')
        .collect::<Vec<_>>();
    if value.is_empty() || value.iter().any(|byte| !(b'0'..=b'7').contains(byte)) {
        bail!("target fixture archive contains an invalid octal size");
    }
    let value = std::str::from_utf8(&value).context("target fixture size is not ASCII")?;
    u64::from_str_radix(value, 8).context("target fixture size overflowed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn promoted_target_fixture_extracts_without_changing_the_archive() {
        let archive = repository_root().join(ARCHIVE);
        let before = fs::read(&archive).unwrap();
        let output = tempfile::tempdir().unwrap();

        let package = extract_target_u16_fixture(output.path()).unwrap();

        assert!(package.join("m4d/manifest/root.json").is_file());
        assert_eq!(fs::read(archive).unwrap(), before);
    }

    #[test]
    fn extraction_rejects_parent_paths() {
        let mut archive = fs::read(repository_root().join(ARCHIVE)).unwrap();
        archive[0..7].copy_from_slice(b"../bad\0");
        let output = tempfile::tempdir().unwrap();

        let error = extract_ustar(&archive, output.path()).unwrap_err();

        assert!(error.to_string().contains("unsafe path"));
    }
}
