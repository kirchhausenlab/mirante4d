use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};

const MAX_TRACKED_GENERATED_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SOURCE_FILE_LINES: usize = 2_000;
const LARGE_SOURCE_FILE_ALLOWLIST: &[(&str, &str)] = &[];
const FORBIDDEN_DUMPING_GROUND_MODULE_NAMES: &[&str] =
    &["common.rs", "helpers.rs", "misc.rs", "utils.rs"];
const FORBIDDEN_AXIS_ALIGNED_2D_CHUNK_PATTERNS: &[&str] = &[
    "(512,512,1)",
    "(512, 512, 1)",
    "(512,1,512)",
    "(512, 1, 512)",
    "(1,512,512)",
    "(1, 512, 512)",
    "512x512x1",
    "512x1x512",
    "1x512x512",
    "slice_chunk",
    "slice_chunks",
    "SliceChunk",
    "SliceChunks",
];

pub(crate) fn architecture_self_check() -> anyhow::Result<()> {
    let required = [
        "crates/mirante4d-analysis",
        "crates/mirante4d-core",
        "crates/mirante4d-format",
        "crates/mirante4d-import",
        "crates/mirante4d-data",
        "crates/mirante4d-renderer",
        "crates/mirante4d-app",
        "crates/xtask",
    ];
    for path in required {
        if !Path::new(path).is_dir() {
            bail!("required crate directory is missing: {path}");
        }
    }
    for forbidden in ["crates/mirante4d-preprocess"] {
        if Path::new(forbidden).exists() {
            bail!("first milestone must not create empty future crate: {forbidden}");
        }
    }
    check_crate_dependency_policy()?;
    check_source_architecture_policy()?;
    check_tracked_artifact_policy()?;
    Ok(())
}

fn check_crate_dependency_policy() -> anyhow::Result<()> {
    let policies = [
        ("mirante4d-core", &[][..]),
        ("mirante4d-format", &["mirante4d-core"][..]),
        (
            "mirante4d-data",
            &["mirante4d-core", "mirante4d-format"][..],
        ),
        (
            "mirante4d-renderer",
            &["mirante4d-core", "mirante4d-data"][..],
        ),
        (
            "mirante4d-import",
            &["mirante4d-core", "mirante4d-format"][..],
        ),
        (
            "mirante4d-analysis",
            &["mirante4d-core", "mirante4d-data"][..],
        ),
        (
            "mirante4d-app",
            &[
                "mirante4d-analysis",
                "mirante4d-core",
                "mirante4d-data",
                "mirante4d-format",
                "mirante4d-import",
                "mirante4d-renderer",
            ][..],
        ),
        (
            "xtask",
            &[
                "mirante4d-analysis",
                "mirante4d-core",
                "mirante4d-data",
                "mirante4d-format",
                "mirante4d-import",
                "mirante4d-renderer",
            ][..],
        ),
    ];

    for (crate_name, allowed) in policies {
        let manifest_path = Path::new("crates").join(crate_name).join("Cargo.toml");
        let manifest = fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let dependencies = normal_workspace_dependencies(&manifest);
        for dependency in dependencies {
            if !allowed.contains(&dependency.as_str()) {
                bail!(
                    "crate {crate_name} has forbidden normal dependency {dependency}; allowed Mirante4D dependencies are: {}",
                    if allowed.is_empty() {
                        "<none>".to_owned()
                    } else {
                        allowed.join(", ")
                    }
                );
            }
        }
    }
    Ok(())
}

fn normal_workspace_dependencies(manifest: &str) -> Vec<String> {
    let mut in_dependencies = false;
    let mut dependencies = Vec::new();
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_dependencies = trimmed == "[dependencies]";
            continue;
        }
        if !in_dependencies || trimmed.starts_with('#') {
            continue;
        }
        let Some((name, _rest)) = trimmed.split_once('=') else {
            continue;
        };
        let name = name
            .trim()
            .split_once('.')
            .map(|(crate_name, _field)| crate_name)
            .unwrap_or_else(|| name.trim());
        if name.starts_with("mirante4d-") {
            dependencies.push(name.to_owned());
        }
    }
    dependencies
}

fn check_source_architecture_policy() -> anyhow::Result<()> {
    let mut violations = Vec::new();
    for path in collect_rust_source_files(Path::new("crates"))? {
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read source file {}", path.display()))?;
        violations.extend(source_architecture_violations(&path, &source));
    }
    if !violations.is_empty() {
        bail!(
            "source architecture policy failed:\n{}",
            violations.join("\n")
        );
    }
    Ok(())
}

fn source_architecture_violations(path: &Path, source: &str) -> Vec<String> {
    let normalized = normalize_repo_path(path);
    let mut violations = Vec::new();
    if let Some(violation) = large_source_file_violation(path, &normalized, source) {
        violations.push(violation);
    }
    if let Some(violation) = dumping_ground_module_name_violation(path) {
        violations.push(violation);
    }
    if normalized.starts_with("crates/xtask/") {
        return violations;
    }
    violations.extend(axis_aligned_2d_chunk_dependency_violations(path, source));
    let app_or_app_test = normalized.starts_with("crates/mirante4d-app/");
    if !app_or_app_test {
        violations.extend(source_pattern_violations(
            path,
            source,
            &[
                "eframe::",
                "egui::",
                "egui_kittest",
                "mirante4d_app",
                "rfd::",
            ],
            "non-app crate must not import UI/app layer",
        ));
    }
    if normalized.starts_with("crates/mirante4d-renderer/src/") {
        violations.extend(source_pattern_violations(
            path,
            source,
            &[
                "std::fs",
                "fs::",
                "File::open",
                "File::create",
                "OpenOptions",
                "read_to_string",
                "read_dir",
            ],
            "renderer source must not perform direct filesystem I/O",
        ));
    }
    violations
}

fn axis_aligned_2d_chunk_dependency_violations(path: &Path, source: &str) -> Vec<String> {
    source_pattern_violations(
        path,
        source,
        FORBIDDEN_AXIS_ALIGNED_2D_CHUNK_PATTERNS,
        "implementation must not depend on axis-aligned 2D slice chunk layouts",
    )
}

fn large_source_file_violation(path: &Path, normalized: &str, source: &str) -> Option<String> {
    let line_count = source.lines().count();
    if line_count <= MAX_SOURCE_FILE_LINES
        || large_source_file_allowlist_reason(normalized).is_some()
    {
        return None;
    }
    Some(format!(
        "{} has {line_count} lines, exceeding the {MAX_SOURCE_FILE_LINES}-line architecture limit; split by domain responsibility or add a documented temporary allowlist entry",
        path.display()
    ))
}

fn large_source_file_allowlist_reason(normalized: &str) -> Option<&'static str> {
    LARGE_SOURCE_FILE_ALLOWLIST
        .iter()
        .find_map(|(path, reason)| (*path == normalized).then_some(*reason))
}

fn dumping_ground_module_name_violation(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    if !FORBIDDEN_DUMPING_GROUND_MODULE_NAMES.contains(&file_name) {
        return None;
    }
    Some(format!(
        "{} uses forbidden dumping-ground module name {file_name:?}; choose a domain-specific module name",
        path.display()
    ))
}

fn source_pattern_violations(
    path: &Path,
    source: &str,
    patterns: &[&str],
    message: &str,
) -> Vec<String> {
    source
        .lines()
        .enumerate()
        .flat_map(|(line_index, line)| {
            patterns.iter().filter_map(move |pattern| {
                if line.contains(pattern) {
                    Some(format!(
                        "{}:{}: {message}: found {pattern:?}",
                        path.display(),
                        line_index + 1
                    ))
                } else {
                    None
                }
            })
        })
        .collect()
}

fn collect_rust_source_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_rust_source_files_inner(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_rust_source_files_inner(root: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let entries = fs::read_dir(root)
        .with_context(|| format!("failed to read source directory {}", root.display()))?;
    for entry in entries {
        let entry =
            entry.with_context(|| format!("failed to read entry under {}", root.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if file_type.is_dir() {
            collect_rust_source_files_inner(&path, files)?;
        } else if file_type.is_file() && path.extension().is_some_and(|extension| extension == "rs")
        {
            files.push(path);
        }
    }
    Ok(())
}

fn check_tracked_artifact_policy() -> anyhow::Result<()> {
    let files = tracked_repository_files()?;
    let mut violations = Vec::new();
    for path in files {
        if !path.exists() {
            continue;
        }
        let metadata = fs::metadata(&path)
            .with_context(|| format!("failed to inspect tracked file {}", path.display()))?;
        if let Some(violation) = tracked_artifact_policy_violation(&path, metadata.len()) {
            violations.push(violation);
        }
    }
    if !violations.is_empty() {
        bail!("tracked artifact policy failed:\n{}", violations.join("\n"));
    }
    Ok(())
}

fn tracked_repository_files() -> anyhow::Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .output()
        .context("failed to run git ls-files for artifact policy check")?;
    if !output.status.success() {
        bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| PathBuf::from(String::from_utf8_lossy(entry).into_owned()))
        .collect())
}

fn tracked_artifact_policy_violation(path: &Path, byte_count: u64) -> Option<String> {
    let normalized = normalize_repo_path(path);
    for forbidden_prefix in ["target/", ".nextest/", "sample_data/"] {
        if normalized.starts_with(forbidden_prefix) {
            return Some(format!(
                "{} is a generated/local artifact path and must not be tracked",
                path.display()
            ));
        }
    }
    if byte_count > MAX_TRACKED_GENERATED_ARTIFACT_BYTES
        && has_generated_data_extension(&normalized)
    {
        return Some(format!(
            "{} is a large generated/data artifact ({} bytes) and must not be tracked",
            path.display(),
            byte_count
        ));
    }
    None
}

fn has_generated_data_extension(normalized_path: &str) -> bool {
    [
        ".czi",
        ".h5",
        ".hdf5",
        ".lif",
        ".m4d",
        ".m4dproj",
        ".mrc",
        ".nd2",
        ".npy",
        ".npz",
        ".ome.tif",
        ".ome.tiff",
        ".tif",
        ".tiff",
        ".zarr",
    ]
    .iter()
    .any(|extension| normalized_path.ends_with(extension))
}

fn normalize_repo_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_workspace_dependencies_reads_only_normal_dependency_section() {
        let manifest = r#"
[package]
name = "example"

[dependencies]
mirante4d-core.workspace = true
serde.workspace = true
mirante4d-data = { workspace = true }

[dev-dependencies]
mirante4d-format.workspace = true
"#;

        assert_eq!(
            normal_workspace_dependencies(manifest),
            vec!["mirante4d-core".to_owned(), "mirante4d-data".to_owned()]
        );
    }

    #[test]
    fn normal_workspace_dependencies_ignores_comments_and_other_sections() {
        let manifest = r#"
[dependencies]
# mirante4d-app.workspace = true

[build-dependencies]
mirante4d-renderer.workspace = true
"#;

        assert!(normal_workspace_dependencies(manifest).is_empty());
    }

    #[test]
    fn source_architecture_policy_rejects_ui_imports_outside_app_crate() {
        let violations = source_architecture_violations(
            Path::new("crates/mirante4d-data/src/lib.rs"),
            "use egui::Context;\n",
        );

        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("non-app crate must not import UI/app layer"));
    }

    #[test]
    fn source_architecture_policy_allows_ui_imports_inside_app_crate() {
        let violations = source_architecture_violations(
            Path::new("crates/mirante4d-app/src/lib.rs"),
            "use egui::Context;\nuse rfd::FileDialog;\n",
        );

        assert!(violations.is_empty());
    }

    #[test]
    fn source_architecture_policy_accepts_current_resident_rendering_bridge_only() {
        let path = Path::new("crates/mirante4d-app/src/resident_rendering.rs");
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let source = fs::read_to_string(repo_root.join(path)).unwrap();
        let violations = source_architecture_violations(path, &source);

        assert!(violations.is_empty(), "{violations:#?}");
    }

    #[test]
    fn source_architecture_policy_accepts_current_app_root() {
        let path = Path::new("crates/mirante4d-app/src/lib.rs");
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let source = fs::read_to_string(repo_root.join(path)).unwrap();
        let violations = source_architecture_violations(path, &source);

        assert!(violations.is_empty(), "{violations:#?}");
    }

    #[test]
    fn source_architecture_policy_rejects_renderer_file_io() {
        let violations = source_architecture_violations(
            Path::new("crates/mirante4d-renderer/src/lib.rs"),
            "use std::fs;\nlet _ = File::open(path);\n",
        );

        assert_eq!(violations.len(), 2);
        assert!(violations[0].contains("renderer source must not perform direct filesystem I/O"));
    }

    #[test]
    fn source_architecture_policy_rejects_new_large_source_files() {
        let source = "fn oversized() {}\n".repeat(MAX_SOURCE_FILE_LINES + 1);
        let violations = source_architecture_violations(
            Path::new("crates/mirante4d-renderer/src/new_monolith.rs"),
            &source,
        );

        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("exceeding the 2000-line architecture limit"));
    }

    #[test]
    fn source_architecture_policy_has_no_current_large_source_allowlist() {
        let source = "fn oversized() {}\n".repeat(MAX_SOURCE_FILE_LINES + 1);
        let violations =
            source_architecture_violations(Path::new("crates/xtask/src/main.rs"), &source);

        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("exceeding the 2000-line architecture limit"));
        assert_eq!(
            large_source_file_allowlist_reason("crates/xtask/src/main.rs"),
            None
        );
        assert_eq!(
            large_source_file_allowlist_reason("crates/mirante4d-format/src/writer.rs"),
            None
        );
        assert_eq!(
            large_source_file_allowlist_reason("crates/mirante4d-renderer/src/gpu/mod.rs"),
            None
        );
        assert_eq!(
            large_source_file_allowlist_reason("crates/mirante4d-app/src/lib.rs"),
            None
        );
        assert_eq!(
            large_source_file_allowlist_reason("crates/mirante4d-import/src/lib.rs"),
            None
        );
        assert_eq!(
            large_source_file_allowlist_reason("crates/mirante4d-renderer/src/brick_render.rs"),
            None
        );
        assert_eq!(
            large_source_file_allowlist_reason("crates/mirante4d-renderer/src/camera_mip.rs"),
            None
        );
        assert_eq!(
            large_source_file_allowlist_reason("crates/mirante4d-data/src/lib.rs"),
            None
        );
        assert_eq!(
            large_source_file_allowlist_reason("crates/mirante4d-analysis/src/lib.rs"),
            None
        );
    }

    #[test]
    fn source_architecture_policy_rejects_dumping_ground_module_names() {
        let violations = source_architecture_violations(
            Path::new("crates/mirante4d-data/src/utils.rs"),
            "pub fn unrelated() {}\n",
        );

        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("forbidden dumping-ground module name"));
    }

    #[test]
    fn source_architecture_policy_rejects_axis_aligned_2d_slice_chunk_dependency() {
        let violations = source_architecture_violations(
            Path::new("crates/mirante4d-app/src/cross_section_runtime.rs"),
            "const REQUIRED_2D_CHUNK_SHAPE: (u32, u32, u32) = (512, 512, 1);\nstruct SliceChunk;\n",
        );

        assert_eq!(violations.len(), 2);
        assert!(violations[0].contains("axis-aligned 2D slice chunk layouts"));
        assert!(violations[1].contains("axis-aligned 2D slice chunk layouts"));
    }

    #[test]
    fn source_architecture_policy_accepts_current_sources_without_axis_aligned_2d_chunks() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut violations = Vec::new();
        for path in collect_rust_source_files(&repo_root.join("crates")).unwrap() {
            let relative_path = path.strip_prefix(&repo_root).unwrap();
            if normalize_repo_path(relative_path).starts_with("crates/xtask/") {
                continue;
            }
            let source = fs::read_to_string(&path).unwrap();
            violations.extend(axis_aligned_2d_chunk_dependency_violations(
                relative_path,
                &source,
            ));
        }

        assert!(violations.is_empty(), "{violations:#?}");
    }

    #[test]
    fn tracked_artifact_policy_rejects_generated_paths_and_large_data_files() {
        assert!(
            tracked_artifact_policy_violation(Path::new("target/mirante4d/out.bin"), 1)
                .unwrap()
                .contains("must not be tracked")
        );
        assert!(
            tracked_artifact_policy_violation(
                Path::new("fixtures/large-source.ome.tiff"),
                MAX_TRACKED_GENERATED_ARTIFACT_BYTES + 1,
            )
            .unwrap()
            .contains("large generated/data artifact")
        );
        assert!(
            tracked_artifact_policy_violation(
                Path::new("docs/ARCHITECTURE.md"),
                MAX_TRACKED_GENERATED_ARTIFACT_BYTES + 1,
            )
            .is_none()
        );
    }
}
