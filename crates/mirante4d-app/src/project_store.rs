use std::{
    env,
    ffi::OsString,
    fs::{self, File},
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::Context;
use serde::{Deserialize, Serialize};

const PROJECT_JSON_FILE: &str = "project.json";
pub(crate) const PROJECT_ARTIFACTS_DIR: &str = "artifacts";
pub(crate) const PROJECT_AUTOSAVE_DIR: &str = "autosave";
const AUTOSAVE_PROJECT_JSON_FILE: &str = "recovery.project.json";
pub(crate) const PROJECT_TABLES_DIR: &str = "tables";
pub(crate) const PROJECT_PLOTS_DIR: &str = "plots";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppAnalysisArtifactReference {
    pub artifact_path: PathBuf,
    pub artifact_id: String,
}

pub(crate) fn dataset_reference_path_for_manifest(
    project_path: &Path,
    dataset_path: &Path,
) -> PathBuf {
    let project_root = absolute_lexical_path(project_path);
    let dataset = absolute_lexical_path(dataset_path);
    relative_path_between(&project_root, &dataset).unwrap_or(dataset)
}

pub(crate) fn dataset_reference_path_from_manifest(
    project_path: &Path,
    stored_path: &Path,
) -> PathBuf {
    if stored_path.is_absolute() {
        lexical_normalize_path(stored_path)
    } else {
        lexical_normalize_path(&project_path.join(stored_path))
    }
}

fn absolute_lexical_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        lexical_normalize_path(path)
    } else {
        lexical_normalize_path(
            &env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path),
        )
    }
}

fn relative_path_between(base: &Path, target: &Path) -> Option<PathBuf> {
    let (base_anchor, base_parts) = path_anchor_and_parts(base);
    let (target_anchor, target_parts) = path_anchor_and_parts(target);
    if base_anchor != target_anchor {
        return None;
    }

    let common = base_parts
        .iter()
        .zip(target_parts.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in common..base_parts.len() {
        relative.push("..");
    }
    for part in target_parts.iter().skip(common) {
        relative.push(part);
    }
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    Some(relative)
}

fn lexical_normalize_path(path: &Path) -> PathBuf {
    let (anchor, parts) = path_anchor_and_parts(path);
    let mut normalized = PathBuf::new();
    if let Some(prefix) = anchor.prefix {
        normalized.push(prefix);
    }
    if anchor.absolute {
        normalized.push(std::path::MAIN_SEPARATOR.to_string());
    }
    for part in parts {
        normalized.push(part);
    }
    if normalized.as_os_str().is_empty() {
        normalized.push(".");
    }
    normalized
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathAnchor {
    prefix: Option<OsString>,
    absolute: bool,
}

fn path_anchor_and_parts(path: &Path) -> (PathAnchor, Vec<OsString>) {
    let parent_dir = OsString::from("..");
    let mut anchor = PathAnchor {
        prefix: None,
        absolute: false,
    };
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                anchor.prefix = Some(prefix.as_os_str().to_owned());
                parts.clear();
            }
            Component::RootDir => {
                anchor.absolute = true;
                parts.clear();
            }
            Component::CurDir => {}
            Component::Normal(part) => parts.push(part.to_owned()),
            Component::ParentDir => {
                if parts.last().is_some_and(|last| last != &parent_dir) {
                    parts.pop();
                    continue;
                }
                if !anchor.absolute {
                    parts.push(parent_dir.clone());
                }
            }
        }
    }
    (anchor, parts)
}

pub(crate) fn native_manifest_fingerprint_blake3(
    manifest: &mirante4d_format::NativeManifest,
) -> String {
    let encoded = serde_json::to_vec(manifest).expect("native manifest serializes");
    blake3::hash(&encoded).to_hex().to_string()
}

pub(crate) fn ensure_project_package_layout(project_path: &Path) -> anyhow::Result<()> {
    if project_path.exists() && !project_path.is_dir() {
        anyhow::bail!(
            "Mirante4D project path exists but is not a directory: {}",
            project_path.display()
        );
    }
    fs::create_dir_all(project_path)?;
    for directory in [
        project_artifact_dir(project_path, "rois"),
        project_artifact_dir(project_path, "tracks"),
        project_artifact_dir(project_path, "measurements"),
        project_artifact_dir(project_path, PROJECT_TABLES_DIR),
        project_artifact_dir(project_path, PROJECT_PLOTS_DIR),
        project_path.join(PROJECT_AUTOSAVE_DIR),
        project_path.join("logs"),
    ] {
        fs::create_dir_all(directory)?;
    }
    Ok(())
}

pub(crate) fn project_json_path(project_path: &Path) -> PathBuf {
    project_path.join(PROJECT_JSON_FILE)
}

pub(crate) fn autosave_project_json_path(project_path: &Path) -> PathBuf {
    project_path
        .join(PROJECT_AUTOSAVE_DIR)
        .join(AUTOSAVE_PROJECT_JSON_FILE)
}

pub(crate) fn project_artifact_dir(project_path: &Path, artifact_kind: &str) -> PathBuf {
    project_data_dir(project_path, PROJECT_ARTIFACTS_DIR, artifact_kind)
}

pub(crate) fn project_data_dir(
    project_path: &Path,
    root_dir: &str,
    artifact_kind: &str,
) -> PathBuf {
    project_path.join(root_dir).join(artifact_kind)
}

pub(crate) fn analysis_artifact_reference(
    root_dir: &str,
    artifact_kind: &str,
    extension: &str,
    index: usize,
    artifact_id: &str,
) -> AppAnalysisArtifactReference {
    let safe_id = safe_artifact_file_stem(artifact_id);
    AppAnalysisArtifactReference {
        artifact_path: PathBuf::from(root_dir)
            .join(artifact_kind)
            .join(format!("{index:04}-{safe_id}.{extension}")),
        artifact_id: artifact_id.to_owned(),
    }
}

fn safe_artifact_file_stem(artifact_id: &str) -> String {
    let safe = artifact_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    if safe.is_empty() {
        "analysis-artifact".to_owned()
    } else {
        safe
    }
}

pub(crate) fn resolve_project_artifact_reference(
    project_path: &Path,
    reference: &AppAnalysisArtifactReference,
    root_dir: &str,
    artifact_kind: &str,
) -> anyhow::Result<PathBuf> {
    validate_relative_artifact_path(&reference.artifact_path)?;
    let expected_prefix = PathBuf::from(root_dir).join(artifact_kind);
    if !reference.artifact_path.starts_with(&expected_prefix) {
        anyhow::bail!(
            "analysis artifact path {:?} is not under {:?}",
            reference.artifact_path,
            expected_prefix
        );
    }
    Ok(project_path.join(&reference.artifact_path))
}

fn validate_relative_artifact_path(path: &Path) -> anyhow::Result<()> {
    if path.as_os_str().is_empty() {
        anyhow::bail!("analysis artifact path must not be empty");
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            anyhow::bail!(
                "analysis artifact path {:?} must be relative and normalized",
                path
            );
        }
    }
    Ok(())
}

pub(crate) fn write_json_artifact_atomically(path: &Path, encoded: &str) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("artifact path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("artifact path has no file name: {}", path.display()))?;
    let temporary = path.with_file_name(format!(".{file_name}.tmp"));
    let backup = path.with_file_name(format!(".{file_name}.replace-backup"));
    write_json_atomically_with_commit(path, &temporary, &backup, encoded, |temporary, target| {
        fs::rename(temporary, target)
    })
}

#[cfg(test)]
pub(crate) fn write_json_artifact_atomically_with_forced_commit_failure(
    path: &Path,
    encoded: &str,
) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("artifact path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("artifact path has no file name: {}", path.display()))?;
    let temporary = path.with_file_name(format!(".{file_name}.tmp"));
    let backup = path.with_file_name(format!(".{file_name}.replace-backup"));
    write_json_atomically_with_commit(path, &temporary, &backup, encoded, |_temporary, _target| {
        Err(io::Error::other("forced atomic artifact commit failure"))
    })
}

pub(crate) fn write_project_json_atomically(
    project_path: &Path,
    encoded: &str,
) -> anyhow::Result<()> {
    fs::create_dir_all(project_path)?;
    let project_json = project_json_path(project_path);
    let temporary = project_path.join(".project.json.tmp");
    let backup = project_path.join(".project.json.replace-backup");
    write_json_atomically_with_commit(
        &project_json,
        &temporary,
        &backup,
        encoded,
        |temporary, target| fs::rename(temporary, target),
    )
}

#[cfg(test)]
pub(crate) fn write_project_json_atomically_with_forced_commit_failure(
    project_path: &Path,
    encoded: &str,
) -> anyhow::Result<()> {
    fs::create_dir_all(project_path)?;
    let project_json = project_json_path(project_path);
    let temporary = project_path.join(".project.json.tmp");
    let backup = project_path.join(".project.json.replace-backup");
    write_json_atomically_with_commit(
        &project_json,
        &temporary,
        &backup,
        encoded,
        |_temporary, _target| Err(io::Error::other("forced atomic project commit failure")),
    )
}

fn write_json_atomically_with_commit(
    target: &Path,
    temporary: &Path,
    backup: &Path,
    encoded: &str,
    commit: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> anyhow::Result<()> {
    remove_file_if_exists(temporary)?;
    remove_file_if_exists(backup)?;
    write_json_temporary_file(temporary, encoded)?;
    if let Err(err) = validate_json_file(temporary) {
        let _ = remove_file_if_exists(temporary);
        return Err(err);
    }

    if target.exists() {
        fs::rename(target, backup)?;
    }

    match commit(temporary, target) {
        Ok(()) => {
            validate_json_file(target)?;
            remove_file_if_exists(backup)?;
            Ok(())
        }
        Err(err) => {
            if backup.exists() {
                let _ = fs::rename(backup, target);
            }
            let _ = remove_file_if_exists(temporary);
            Err(err).with_context(|| format!("failed to commit {}", target.display()))
        }
    }
}

fn write_json_temporary_file(path: &Path, encoded: &str) -> anyhow::Result<()> {
    let mut file =
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    file.write_all(encoded.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("failed to finish {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to flush {}", path.display()))
}

fn validate_json_file(path: &Path) -> anyhow::Result<()> {
    let encoded =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let _: serde_json::Value = serde_json::from_str(&encoded)
        .with_context(|| format!("failed to validate JSON {}", path.display()))?;
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}
