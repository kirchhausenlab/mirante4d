use std::{
    env, fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::{
    deps::{self, CargoMetadata, cargo_metadata},
    process::{run_cargo, run_command},
    target_fixture::extract_target_u16_fixture,
};

const DIST_ROOT: &str = "target/mirante4d/dist";
const RELEASE_BUILD_PROFILE: &str = "release";
const RELEASE_PACKAGE_KIND: &str = "linux-release";

#[derive(Debug, Clone)]
struct LinuxReleaseArtifacts {
    package_id: String,
    package_root: PathBuf,
    appdir_root: PathBuf,
    tarball_path: PathBuf,
    appimage_path: PathBuf,
    contents_report_path: PathBuf,
    release_dir_smoke_log_path: PathBuf,
    appimage_smoke_log_path: PathBuf,
    tarball_smoke_log_path: PathBuf,
}

impl LinuxReleaseArtifacts {
    fn under(dist_root: &Path, package_id: String) -> Self {
        Self {
            package_root: dist_root.join(&package_id),
            appdir_root: dist_root.join(format!("{package_id}.AppDir")),
            tarball_path: dist_root.join(format!("{package_id}.tar.gz")),
            appimage_path: dist_root.join(format!("{package_id}.AppImage")),
            contents_report_path: dist_root.join(format!("{package_id}-contents.json")),
            release_dir_smoke_log_path: dist_root
                .join(format!("{package_id}-smoke-release-dir.log")),
            appimage_smoke_log_path: dist_root.join(format!("{package_id}-smoke-appimage.log")),
            tarball_smoke_log_path: dist_root.join(format!("{package_id}-smoke-tarball.log")),
            package_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitSourceIdentity {
    commit: String,
    tree: String,
}

pub(crate) fn package_linux_release() -> anyhow::Result<PathBuf> {
    Ok(build_linux_release_package()?.contents_report_path)
}

fn build_linux_release_package() -> anyhow::Result<LinuxReleaseArtifacts> {
    if !cfg!(target_os = "linux") {
        bail!(
            "Linux release packaging must run on Linux; current target_os is {}",
            env::consts::OS
        );
    }

    let source_identity = require_clean_committed_worktree()?;
    deps::verify_deps()?;
    run_cargo(["build", "--release", "-p", "mirante4d-app"])?;

    let metadata = cargo_metadata()?;
    let app_version = package_version(&metadata, "mirante4d-app")?;
    let arch = env::consts::ARCH.to_owned();
    let package_id = linux_release_package_id(&app_version, &arch);
    let dist_root = PathBuf::from(DIST_ROOT);
    fs::create_dir_all(&dist_root)
        .with_context(|| format!("failed to create {}", dist_root.display()))?;

    let artifacts = LinuxReleaseArtifacts::under(&dist_root, package_id);
    remove_path_if_exists(&artifacts.package_root)?;
    remove_path_if_exists(&artifacts.appdir_root)?;
    remove_path_if_exists(&artifacts.tarball_path)?;
    remove_path_if_exists(&artifacts.appimage_path)?;
    remove_path_if_exists(&artifacts.contents_report_path)?;
    remove_path_if_exists(&artifacts.release_dir_smoke_log_path)?;
    remove_path_if_exists(&artifacts.appimage_smoke_log_path)?;
    remove_path_if_exists(&artifacts.tarball_smoke_log_path)?;

    let binary_name = linux_binary_name();
    let source_binary = PathBuf::from("target").join("release").join(binary_name);
    let packaged_binary = artifacts.package_root.join(binary_name);
    fs::create_dir_all(&artifacts.package_root)
        .with_context(|| format!("failed to create {}", artifacts.package_root.display()))?;
    copy_file(&source_binary, &packaged_binary)?;
    set_executable(&packaged_binary)?;
    copy_file(
        Path::new("README.md"),
        &artifacts.package_root.join("README.md"),
    )?;
    copy_file(
        Path::new("LICENSE"),
        &artifacts.package_root.join("LICENSE"),
    )?;
    copy_file(
        Path::new("ASSET_PROVENANCE.md"),
        &artifacts.package_root.join("ASSET_PROVENANCE.md"),
    )?;
    write_third_party_notices(
        &metadata,
        &artifacts.package_root.join("THIRD_PARTY_NOTICES.md"),
    )?;
    copy_file(
        Path::new("packaging/PLATFORM_SUPPORT.md"),
        &artifacts.package_root.join("PLATFORM_SUPPORT.md"),
    )?;
    copy_linux_metadata(&artifacts.package_root)?;
    audit_linux_runtime_dependencies(
        &packaged_binary,
        &artifacts.package_root.join("runtime-dependencies.txt"),
    )?;

    write_release_manifest(
        &artifacts.package_root,
        &artifacts.package_id,
        &app_version,
        &arch,
        binary_name,
    )?;
    create_linux_appdir(&artifacts, binary_name)?;
    build_appimage(&artifacts.appdir_root, &artifacts.appimage_path)?;

    let fixture = extract_target_u16_fixture(Path::new("target/mirante4d/fixtures"))?;
    let release_dir_smoke = run_package_smoke_test(
        &packaged_binary,
        &fixture,
        &app_version,
        &artifacts.release_dir_smoke_log_path,
    )?;
    let appimage_smoke = run_package_smoke_test(
        &artifacts.appimage_path,
        &fixture,
        &app_version,
        &artifacts.appimage_smoke_log_path,
    )?;
    create_tarball(&dist_root, &artifacts.package_id, &artifacts.tarball_path)?;
    let tarball_smoke = smoke_linux_tarball(&artifacts, &fixture, &app_version)?;

    let report = linux_release_contents_report(
        &artifacts,
        &app_version,
        &arch,
        release_dir_smoke,
        appimage_smoke,
        tarball_smoke,
        &source_identity,
    )?;
    let report_text = format!("{}\n", serde_json::to_string_pretty(&report)?);
    fs::write(&artifacts.contents_report_path, &report_text).with_context(|| {
        format!(
            "failed to write {}",
            artifacts.contents_report_path.display()
        )
    })?;
    Ok(artifacts)
}

fn linux_release_package_id(app_version: &str, arch: &str) -> String {
    format!("mirante4d-{app_version}-linux-{arch}-{RELEASE_BUILD_PROFILE}")
}

fn linux_binary_name() -> &'static str {
    "mirante4d-app"
}

fn remove_path_if_exists(path: &Path) -> anyhow::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    } else if path.exists() {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> anyhow::Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(source, destination).with_context(|| {
        format!(
            "failed to copy {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("failed to chmod executable {}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn copy_linux_metadata(root: &Path) -> anyhow::Result<()> {
    copy_file(
        Path::new("packaging/linux/org.kirchhausenlab.Mirante4D.desktop"),
        &root
            .join("share")
            .join("applications")
            .join("org.kirchhausenlab.Mirante4D.desktop"),
    )?;
    copy_file(
        Path::new("packaging/linux/mirante4d.svg"),
        &root
            .join("share")
            .join("icons")
            .join("hicolor")
            .join("scalable")
            .join("apps")
            .join("mirante4d.svg"),
    )?;
    copy_file(
        Path::new("packaging/linux/org.kirchhausenlab.Mirante4D.appdata.xml"),
        &root
            .join("share")
            .join("metainfo")
            .join("org.kirchhausenlab.Mirante4D.appdata.xml"),
    )
}

fn write_third_party_notices(metadata: &CargoMetadata, path: &Path) -> anyhow::Result<()> {
    let mut packages = metadata.packages.iter().collect::<Vec<_>>();
    packages.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.version.cmp(&right.version))
    });
    let mut notice = String::new();
    notice.push_str("# Mirante4D Third-Party Notices\n\n");
    notice.push_str("Generated from `cargo metadata --locked` for release packaging.\n\n");
    notice.push_str("| Package | Version | License |\n");
    notice.push_str("| --- | --- | --- |\n");
    for package in packages {
        notice.push_str(&format!(
            "| {} | {} | {} |\n",
            package.name,
            package.version,
            package.license.as_deref().unwrap_or("unknown")
        ));
    }
    fs::write(path, notice).with_context(|| format!("failed to write {}", path.display()))
}

fn write_release_manifest(
    package_root: &Path,
    package_id: &str,
    app_version: &str,
    arch: &str,
    binary_name: &str,
) -> anyhow::Result<()> {
    let manifest = json!({
        "name": "Mirante4D",
        "package_id": package_id,
        "version": app_version,
        "package_kind": RELEASE_PACKAGE_KIND,
        "platform": "linux",
        "architecture": arch,
        "build_profile": RELEASE_BUILD_PROFILE,
        "dataset_profile": {
            "format_family": mirante4d_storage::PROFILE.format_family,
            "semantic_schema": mirante4d_storage::PROFILE.semantic_schema,
            "storage_profile": mirante4d_storage::PROFILE.storage_profile,
        },
        "binary": binary_name,
        "desktop_metadata": "share/applications/org.kirchhausenlab.Mirante4D.desktop",
        "icon": "share/icons/hicolor/scalable/apps/mirante4d.svg",
        "appstream_metadata": "share/metainfo/org.kirchhausenlab.Mirante4D.appdata.xml",
        "license": "LICENSE",
        "asset_provenance": "ASSET_PROVENANCE.md",
        "third_party_notices": "THIRD_PARTY_NOTICES.md",
        "platform_support": "PLATFORM_SUPPORT.md",
        "runtime_dependency_audit": "runtime-dependencies.txt"
    });
    fs::write(
        package_root.join("manifest.json"),
        format!("{}\n", serde_json::to_string_pretty(&manifest)?),
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            package_root.join("manifest.json").display()
        )
    })
}

fn create_linux_appdir(artifacts: &LinuxReleaseArtifacts, binary_name: &str) -> anyhow::Result<()> {
    let appdir = &artifacts.appdir_root;
    fs::create_dir_all(appdir).with_context(|| format!("failed to create {}", appdir.display()))?;
    copy_file(
        &artifacts.package_root.join(binary_name),
        &appdir.join("usr").join("bin").join(binary_name),
    )?;
    set_executable(&appdir.join("usr").join("bin").join(binary_name))?;
    copy_file(
        Path::new("packaging/linux/org.kirchhausenlab.Mirante4D.desktop"),
        &appdir.join("org.kirchhausenlab.Mirante4D.desktop"),
    )?;
    copy_file(
        Path::new("packaging/linux/org.kirchhausenlab.Mirante4D.desktop"),
        &appdir
            .join("usr")
            .join("share")
            .join("applications")
            .join("org.kirchhausenlab.Mirante4D.desktop"),
    )?;
    copy_file(
        Path::new("packaging/linux/mirante4d.svg"),
        &appdir.join("mirante4d.svg"),
    )?;
    copy_file(
        Path::new("packaging/linux/mirante4d.svg"),
        &appdir
            .join("usr")
            .join("share")
            .join("icons")
            .join("hicolor")
            .join("scalable")
            .join("apps")
            .join("mirante4d.svg"),
    )?;
    copy_file(
        Path::new("packaging/linux/org.kirchhausenlab.Mirante4D.appdata.xml"),
        &appdir
            .join("usr")
            .join("share")
            .join("metainfo")
            .join("org.kirchhausenlab.Mirante4D.appdata.xml"),
    )?;
    copy_file(
        &artifacts.package_root.join("README.md"),
        &appdir
            .join("usr")
            .join("share")
            .join("doc")
            .join("mirante4d")
            .join("README.md"),
    )?;
    copy_file(
        &artifacts.package_root.join("LICENSE"),
        &appdir
            .join("usr")
            .join("share")
            .join("doc")
            .join("mirante4d")
            .join("LICENSE"),
    )?;
    copy_file(
        &artifacts.package_root.join("ASSET_PROVENANCE.md"),
        &appdir
            .join("usr")
            .join("share")
            .join("doc")
            .join("mirante4d")
            .join("ASSET_PROVENANCE.md"),
    )?;
    copy_file(
        &artifacts.package_root.join("THIRD_PARTY_NOTICES.md"),
        &appdir
            .join("usr")
            .join("share")
            .join("doc")
            .join("mirante4d")
            .join("THIRD_PARTY_NOTICES.md"),
    )?;
    let app_run = appdir.join("AppRun");
    fs::write(
        &app_run,
        "#!/bin/sh\nAPPDIR=\"$(dirname \"$(readlink -f \"$0\")\")\"\nexec \"$APPDIR/usr/bin/mirante4d-app\" \"$@\"\n",
    )
    .with_context(|| format!("failed to write {}", app_run.display()))?;
    set_executable(&app_run)
}

fn appimagetool_path() -> anyhow::Result<PathBuf> {
    if let Some(path) = env::var_os("MIRANTE4D_APPIMAGETOOL") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        bail!(
            "MIRANTE4D_APPIMAGETOOL points to a missing file: {}",
            path.display()
        );
    }
    let output = Command::new("sh")
        .args(["-c", "command -v appimagetool"])
        .output()
        .context("failed to search PATH for appimagetool")?;
    if output.status.success() {
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !raw.is_empty() {
            return Ok(PathBuf::from(raw));
        }
    }
    bail!(
        "appimagetool is required for `cargo xtask package-linux-release`; install it on PATH or set MIRANTE4D_APPIMAGETOOL"
    )
}

fn build_appimage(appdir_root: &Path, appimage_path: &Path) -> anyhow::Result<()> {
    let appstream_metadata = appdir_root
        .join("usr")
        .join("share")
        .join("metainfo")
        .join("org.kirchhausenlab.Mirante4D.appdata.xml");
    let mut validate = Command::new("appstreamcli");
    validate
        .args(["validate", "--no-net"])
        .arg(&appstream_metadata);
    run_command(&mut validate).with_context(|| {
        format!(
            "failed to validate AppStream metadata {} without network access",
            appstream_metadata.display()
        )
    })?;

    let appimagetool = appimagetool_path()?;
    let mut command = Command::new(&appimagetool);
    command
        .arg("--no-appstream")
        .arg(appdir_root)
        .arg(appimage_path)
        .env("ARCH", env::consts::ARCH)
        .env("APPIMAGE_EXTRACT_AND_RUN", "1");
    run_command(&mut command).with_context(|| {
        format!(
            "failed to build AppImage {} from {}",
            appimage_path.display(),
            appdir_root.display()
        )
    })?;
    set_executable(appimage_path)
}

fn create_tarball(dist_root: &Path, package_id: &str, tarball_path: &Path) -> anyhow::Result<()> {
    let mut command = Command::new("tar");
    command
        .arg("-C")
        .arg(dist_root)
        .args(["-czf"])
        .arg(tarball_path)
        .arg(package_id);
    run_command(&mut command)
        .with_context(|| format!("failed to create tarball {}", tarball_path.display()))
}

pub(crate) fn package_version(
    metadata: &CargoMetadata,
    package_name: &str,
) -> anyhow::Result<String> {
    metadata
        .packages
        .iter()
        .find(|package| package.name == package_name)
        .map(|package| package.version.clone())
        .with_context(|| format!("package {package_name:?} was not found in cargo metadata"))
}

fn audit_linux_runtime_dependencies(binary: &Path, output_path: &Path) -> anyhow::Result<()> {
    let output = Command::new("ldd")
        .arg(binary)
        .output()
        .with_context(|| format!("failed to run ldd on {}", binary.display()))?;
    let mut report = String::new();
    report.push_str(&format!("command: ldd {}\n", binary.display()));
    report.push_str(&format!("status: {}\n\n", output.status));
    report.push_str(&String::from_utf8_lossy(&output.stdout));
    report.push_str(&String::from_utf8_lossy(&output.stderr));
    fs::write(output_path, &report)
        .with_context(|| format!("failed to write {}", output_path.display()))?;
    if !output.status.success() {
        bail!(
            "ldd failed for {}; see {}",
            binary.display(),
            output_path.display()
        );
    }
    if report.contains("not found") {
        bail!(
            "runtime dependency audit found missing libraries for {}; see {}",
            binary.display(),
            output_path.display()
        );
    }
    Ok(())
}

fn run_package_smoke_test(
    binary: &Path,
    dataset: &Path,
    expected_version: &str,
    output_path: &Path,
) -> anyhow::Result<Value> {
    let output = Command::new(binary)
        .env("MIRANTE4D_APP_SMOKE", "1")
        .env("MIRANTE4D_DEV_DATASET", dataset)
        .env("APPIMAGE_EXTRACT_AND_RUN", "1")
        .env(
            "RUST_LOG",
            env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned()),
        )
        .output()
        .with_context(|| format!("failed to run package smoke test {}", binary.display()))?;
    let mut report = String::new();
    report.push_str(&format!("binary: {}\n", binary.display()));
    report.push_str(&format!("dataset: {}\n", dataset.display()));
    report.push_str(&format!("status: {}\n\n", output.status));
    report.push_str("stdout:\n");
    report.push_str(&String::from_utf8_lossy(&output.stdout));
    report.push_str("\nstderr:\n");
    report.push_str(&String::from_utf8_lossy(&output.stderr));
    fs::write(output_path, &report)
        .with_context(|| format!("failed to write {}", output_path.display()))?;
    if !output.status.success() {
        bail!(
            "package smoke test failed for {}; see {}",
            binary.display(),
            output_path.display()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains(&format!("Mirante4D {expected_version} opened")) {
        bail!(
            "package smoke test did not report expected version {expected_version}; see {}",
            output_path.display()
        );
    }
    Ok(json!({
        "artifact": binary,
        "dataset": dataset,
        "log": output_path,
        "status": output.status.to_string(),
        "stdout_summary": stdout.lines().find(|line| line.contains("Mirante4D")).unwrap_or(""),
        "diagnostics_summary_present": stdout.contains("Mirante4D diagnostics"),
        "gpu_adapter_present": stdout.contains("gpu_adapter:") || stdout.contains("GPU adapter:"),
    }))
}

fn smoke_linux_tarball(
    artifacts: &LinuxReleaseArtifacts,
    fixture: &Path,
    app_version: &str,
) -> anyhow::Result<Value> {
    let smoke_root = PathBuf::from("target")
        .join("mirante4d")
        .join("dist-smoke")
        .join(format!("{}-tarball", artifacts.package_id));
    remove_path_if_exists(&smoke_root)?;
    fs::create_dir_all(&smoke_root)
        .with_context(|| format!("failed to create {}", smoke_root.display()))?;
    let mut command = Command::new("tar");
    command
        .arg("-C")
        .arg(&smoke_root)
        .args(["-xzf"])
        .arg(&artifacts.tarball_path);
    run_command(&mut command).with_context(|| {
        format!(
            "failed to extract tarball {} to {}",
            artifacts.tarball_path.display(),
            smoke_root.display()
        )
    })?;
    let binary = smoke_root
        .join(&artifacts.package_id)
        .join(linux_binary_name());
    run_package_smoke_test(
        &binary,
        fixture,
        app_version,
        &artifacts.tarball_smoke_log_path,
    )
}

fn linux_release_contents_report(
    artifacts: &LinuxReleaseArtifacts,
    app_version: &str,
    arch: &str,
    release_dir_smoke: Value,
    appimage_smoke: Value,
    tarball_smoke: Value,
    source_identity: &GitSourceIdentity,
) -> anyhow::Result<Value> {
    let package_files = list_relative_files(&artifacts.package_root)?;
    let appdir_files = list_relative_files(&artifacts.appdir_root)?;
    let package_has_sample_data = package_files
        .iter()
        .any(|path| path_contains_local_sample_data(Path::new(path)));
    let appdir_has_sample_data = appdir_files
        .iter()
        .any(|path| path_contains_local_sample_data(Path::new(path)));
    if package_has_sample_data || appdir_has_sample_data {
        bail!("release artifacts must not include local sample data paths");
    }
    let tarball_hash = file_sha256(&artifacts.tarball_path)?;
    let appimage_hash = file_sha256(&artifacts.appimage_path)?;
    Ok(json!({
        "artifact_schema_version": 2,
        "package_kind": RELEASE_PACKAGE_KIND,
        "package_id": artifacts.package_id,
        "app_version": app_version,
        "git_commit": source_identity.commit,
        "git_tree": source_identity.tree,
        "platform": "linux",
        "architecture": arch,
        "build_profile": RELEASE_BUILD_PROFILE,
        "dataset_profile": {
            "format_family": mirante4d_storage::PROFILE.format_family,
            "semantic_schema": mirante4d_storage::PROFILE.semantic_schema,
            "storage_profile": mirante4d_storage::PROFILE.storage_profile,
        },
        "dependency_gate": "passed",
        "artifacts": {
            "release_directory": {
                "path": artifacts.package_root,
                "file_count": package_files.len(),
            },
            "appdir": {
                "path": artifacts.appdir_root,
                "file_count": appdir_files.len(),
            },
            "tarball": {
                "path": artifacts.tarball_path,
                "sha256": tarball_hash,
                "bytes": file_size(&artifacts.tarball_path)?,
            },
            "appimage": {
                "path": artifacts.appimage_path,
                "sha256": appimage_hash,
                "bytes": file_size(&artifacts.appimage_path)?,
            },
        },
        "required_contents": {
            "binary": package_files.contains(&linux_binary_name().to_owned()),
            "manifest": package_files.contains(&"manifest.json".to_owned()),
            "readme": package_files.contains(&"README.md".to_owned()),
            "license": package_files.contains(&"LICENSE".to_owned()),
            "asset_provenance": package_files.contains(&"ASSET_PROVENANCE.md".to_owned()),
            "third_party_notices": package_files.contains(&"THIRD_PARTY_NOTICES.md".to_owned()),
            "appdir_license": appdir_files.contains(&"usr/share/doc/mirante4d/LICENSE".to_owned()),
            "appdir_asset_provenance": appdir_files.contains(&"usr/share/doc/mirante4d/ASSET_PROVENANCE.md".to_owned()),
            "appdir_third_party_notices": appdir_files.contains(&"usr/share/doc/mirante4d/THIRD_PARTY_NOTICES.md".to_owned()),
            "platform_support": package_files.contains(&"PLATFORM_SUPPORT.md".to_owned()),
            "desktop_entry": package_files.contains(&"share/applications/org.kirchhausenlab.Mirante4D.desktop".to_owned()),
            "icon": package_files.contains(&"share/icons/hicolor/scalable/apps/mirante4d.svg".to_owned()),
            "appstream_metadata": package_files.contains(&"share/metainfo/org.kirchhausenlab.Mirante4D.appdata.xml".to_owned()),
            "runtime_dependency_audit": package_files.contains(&"runtime-dependencies.txt".to_owned()),
        },
        "sample_data_absent": true,
        "smoke_tests": {
            "release_directory": release_dir_smoke,
            "appimage": appimage_smoke,
            "tarball": tarball_smoke,
        },
        "package_files": package_files,
        "appdir_files": appdir_files,
    }))
}

fn list_relative_files(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut files = Vec::new();
    list_relative_files_inner(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

fn list_relative_files_inner(
    root: &Path,
    current: &Path,
    files: &mut Vec<String>,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(current)
        .with_context(|| format!("failed to read directory {}", current.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read entry in {}", current.display()))?;
        let path = entry.path();
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if metadata.is_dir() {
            list_relative_files_inner(root, &path, files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .with_context(|| format!("failed to relativize {}", path.display()))?;
            files.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

fn path_contains_local_sample_data(path: &Path) -> bool {
    path.components().any(|component| match component {
        Component::Normal(name) => name == "sample_data" || name == "benchmarks",
        _ => false,
    })
}

fn file_sha256(path: &Path) -> anyhow::Result<String> {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .with_context(|| format!("failed to run sha256sum on {}", path.display()))?;
    if !output.status.success() {
        bail!(
            "sha256sum failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .with_context(|| format!("sha256sum output was empty for {}", path.display()))
}

fn file_size(path: &Path) -> anyhow::Result<u64> {
    fs::metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))
        .map(|metadata| metadata.len())
}

fn require_clean_committed_worktree() -> anyhow::Result<GitSourceIdentity> {
    let status = git_stdout(&["status", "--porcelain=v1", "--untracked-files=normal"])?;
    let commit = git_stdout(&["rev-parse", "--verify", "HEAD^{commit}"])?;
    let tree = git_stdout(&["rev-parse", "--verify", "HEAD^{tree}"])?;
    validate_git_source_identity(commit, tree, &status)
}

fn git_stdout(args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn validate_git_source_identity(
    commit: String,
    tree: String,
    worktree_status: &str,
) -> anyhow::Result<GitSourceIdentity> {
    if !worktree_status.is_empty() {
        bail!(
            "release packaging requires a clean committed worktree; commit or stash these changes first:\n{worktree_status}"
        );
    }
    for (label, value) in [("commit", &commit), ("tree", &tree)] {
        if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("git {label} must be a full 40-character object ID, got {value:?}");
        }
    }
    Ok(GitSourceIdentity { commit, tree })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_release_package_id_includes_version_platform_arch_and_profile() {
        assert_eq!(
            linux_release_package_id("0.1.0", "x86_64"),
            "mirante4d-0.1.0-linux-x86_64-release"
        );
    }

    #[test]
    fn release_manifest_names_license_and_provenance_notices() {
        let tempdir = tempfile::tempdir().unwrap();
        write_release_manifest(
            tempdir.path(),
            "mirante4d-test-linux-x86_64-release",
            "0.1.0",
            "x86_64",
            "mirante4d-app",
        )
        .unwrap();

        let manifest: Value =
            serde_json::from_slice(&fs::read(tempdir.path().join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["license"], "LICENSE");
        assert_eq!(manifest["asset_provenance"], "ASSET_PROVENANCE.md");
        assert_eq!(manifest["third_party_notices"], "THIRD_PARTY_NOTICES.md");
        assert!(manifest.get("release_dir_smoke_log").is_none());
    }

    #[test]
    fn release_evidence_files_are_siblings_of_distributable_artifacts() {
        let artifacts = LinuxReleaseArtifacts::under(
            Path::new("target/mirante4d/dist"),
            "mirante4d-test-linux-x86_64-release".to_owned(),
        );

        for evidence_path in [
            &artifacts.contents_report_path,
            &artifacts.release_dir_smoke_log_path,
            &artifacts.appimage_smoke_log_path,
            &artifacts.tarball_smoke_log_path,
        ] {
            assert_eq!(evidence_path.parent(), artifacts.package_root.parent());
            assert!(!evidence_path.starts_with(&artifacts.package_root));
        }
    }

    #[test]
    fn source_identity_requires_clean_status_and_full_object_ids() {
        let commit = "1".repeat(40);
        let tree = "a".repeat(40);
        assert_eq!(
            validate_git_source_identity(commit.clone(), tree.clone(), "").unwrap(),
            GitSourceIdentity {
                commit: commit.clone(),
                tree: tree.clone(),
            }
        );
        assert!(validate_git_source_identity(commit.clone(), tree, " M README.md").is_err());
        assert!(validate_git_source_identity("1".repeat(12), commit, "").is_err());
    }

    #[test]
    fn release_sample_data_policy_rejects_local_sample_paths() {
        assert!(path_contains_local_sample_data(Path::new(
            "sample_data/T5-001/source.tif"
        )));
        assert!(path_contains_local_sample_data(Path::new(
            "target/mirante4d/benchmarks/private-qualification/t5-001.m4d"
        )));
        assert!(!path_contains_local_sample_data(Path::new(
            "share/icons/hicolor/scalable/apps/mirante4d.svg"
        )));
    }
}
