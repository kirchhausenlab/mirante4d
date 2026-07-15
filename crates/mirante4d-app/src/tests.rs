use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, bail};
use eframe::egui;

const TARGET_FIXTURE_ARCHIVE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/target/archives/m4d-t1-u16-3d-multiscale.tar"
));
const SOURCE_FIXTURE_ARCHIVE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/source/mirante4d-source-tiff-fixtures-v1.tar"
));
const TARGET_FIXTURE_NAME: &str = "m4d-t1-u16-3d-multiscale";
const USTAR_BLOCK_BYTES: usize = 512;
const TARGET_FIXTURE_ARCHIVE_BYTES_MAX: usize = 512 * 1024;
const TARGET_FIXTURE_ENTRY_COUNT_MAX: usize = 128;
const TARGET_FIXTURE_PATH_BYTES_MAX: usize = 240;

const TEST_INITIAL_RENDER_VIEWPORT_SIDE: u64 = 32;

use super::*;

pub(crate) fn write_target_fixture(output_root: &Path) -> anyhow::Result<PathBuf> {
    if TARGET_FIXTURE_ARCHIVE.is_empty()
        || TARGET_FIXTURE_ARCHIVE.len() > TARGET_FIXTURE_ARCHIVE_BYTES_MAX
        || !TARGET_FIXTURE_ARCHIVE
            .len()
            .is_multiple_of(USTAR_BLOCK_BYTES)
    {
        bail!("target fixture archive has an invalid bounded length");
    }

    fs::create_dir_all(output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let package = output_root.join(TARGET_FIXTURE_NAME);
    fs::create_dir(&package).with_context(|| format!("failed to create {}", package.display()))?;
    if let Err(error) = extract_target_fixture(TARGET_FIXTURE_ARCHIVE, &package) {
        let _ = fs::remove_dir_all(&package);
        return Err(error);
    }
    Ok(package)
}

pub(crate) fn write_source_time_series_fixture(output_root: &Path) -> anyhow::Result<PathBuf> {
    if SOURCE_FIXTURE_ARCHIVE.is_empty()
        || SOURCE_FIXTURE_ARCHIVE.len() > TARGET_FIXTURE_ARCHIVE_BYTES_MAX
        || !SOURCE_FIXTURE_ARCHIVE
            .len()
            .is_multiple_of(USTAR_BLOCK_BYTES)
    {
        bail!("source fixture archive has an invalid bounded length");
    }
    let fixture_root = output_root.join("source-tiff-fixtures");
    fs::create_dir(&fixture_root)
        .with_context(|| format!("failed to create {}", fixture_root.display()))?;
    if let Err(error) = extract_target_fixture(SOURCE_FIXTURE_ARCHIVE, &fixture_root) {
        let _ = fs::remove_dir_all(&fixture_root);
        return Err(error);
    }
    Ok(fixture_root.join("spec-002"))
}

fn extract_target_fixture(archive: &[u8], root: &Path) -> anyhow::Result<()> {
    let mut offset = 0_usize;
    let mut paths = HashSet::new();
    while offset + USTAR_BLOCK_BYTES <= archive.len() {
        let header = &archive[offset..offset + USTAR_BLOCK_BYTES];
        offset += USTAR_BLOCK_BYTES;
        if header.iter().all(|byte| *byte == 0) {
            if archive[offset..].iter().any(|byte| *byte != 0) {
                bail!("target fixture archive has bytes after its terminator");
            }
            return Ok(());
        }
        if paths.len() >= TARGET_FIXTURE_ENTRY_COUNT_MAX {
            bail!("target fixture archive exceeds its entry bound");
        }
        if &header[257..263] != b"ustar\0" {
            bail!("target fixture archive is not USTAR");
        }

        let relative = target_fixture_member_path(header)?;
        if !paths.insert(relative.clone()) {
            bail!("target fixture archive contains a duplicate path");
        }
        let destination = root.join(&relative);
        let size = usize::try_from(parse_ustar_octal(&header[124..136])?)
            .context("target fixture member is too large")?;
        let padded_size = size
            .checked_add(USTAR_BLOCK_BYTES - 1)
            .context("target fixture member size overflowed")?
            / USTAR_BLOCK_BYTES
            * USTAR_BLOCK_BYTES;
        let end = offset
            .checked_add(padded_size)
            .context("target fixture member range overflowed")?;
        let data_end = offset
            .checked_add(size)
            .context("target fixture member range overflowed")?;
        if end > archive.len() || data_end > archive.len() {
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
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&destination)
                    .with_context(|| {
                        format!("failed to create fixture file {}", destination.display())
                    })?;
                file.write_all(&archive[offset..data_end])
                    .with_context(|| {
                        format!("failed to write fixture file {}", destination.display())
                    })?;
            }
            kind => bail!("target fixture archive contains unsupported member type {kind}"),
        }
        offset = end;
    }
    bail!("target fixture archive has no terminator")
}

fn target_fixture_member_path(header: &[u8]) -> anyhow::Result<PathBuf> {
    let name = ustar_string_field(&header[0..100])?;
    let prefix = ustar_string_field(&header[345..500])?;
    let encoded = if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    };
    if encoded.is_empty() || encoded.len() > TARGET_FIXTURE_PATH_BYTES_MAX {
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

fn ustar_string_field(field: &[u8]) -> anyhow::Result<String> {
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

fn parse_ustar_octal(field: &[u8]) -> anyhow::Result<u64> {
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

#[test]
fn noninteractive_project_paths_are_consumed_once() {
    let mut paths = ProjectStoreNoninteractivePaths {
        open: Some("open.m4dproj".into()),
        initial_save: Some("initial.m4dproj".into()),
        save_as: Some("fork.m4dproj".into()),
    };

    assert_eq!(paths.open.take(), Some("open.m4dproj".into()));
    assert_eq!(paths.initial_save.take(), Some("initial.m4dproj".into()));
    assert_eq!(paths.save_as.take(), Some("fork.m4dproj".into()));
    assert!(paths.open.take().is_none());
    assert!(paths.initial_save.take().is_none());
    assert!(paths.save_as.take().is_none());
}

#[test]
fn recovery_locator_discovery_is_canonical_bounded_and_content_blind() {
    let root = tempfile::tempdir().unwrap();
    let current = ProjectId::from_bytes([1; 16]);
    let older_a = ProjectId::from_bytes([2; 16]);
    let older_b = ProjectId::from_bytes([3; 16]);
    fs::create_dir(root.path().join(format!("{current}.m4dproj"))).unwrap();
    fs::create_dir(root.path().join(format!("{older_b}.m4dproj"))).unwrap();
    fs::create_dir(root.path().join(format!("{older_a}.m4dproj"))).unwrap();
    fs::create_dir(root.path().join("not-a-project.m4dproj")).unwrap();
    fs::write(
        root.path()
            .join(format!("{}.m4dproj", ProjectId::from_bytes([4; 16]))),
        b"not a directory",
    )
    .unwrap();

    let locators = discover_project_recovery_locators(Some(root.path()), current).unwrap();
    assert_eq!(
        locators
            .iter()
            .map(ProjectRecoveryStoreLocator::project_id)
            .collect::<Vec<_>>(),
        vec![older_a, older_b]
    );
}

#[test]
fn recovery_locator_discovery_fails_closed_at_its_exact_capacity() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..=PROJECT_RECOVERY_ROOT_ENTRIES_MAX {
        let mut bytes = [0; 16];
        bytes[8..].copy_from_slice(&u64::try_from(index + 1).unwrap().to_be_bytes());
        fs::create_dir(
            root.path()
                .join(format!("{}.m4dproj", ProjectId::from_bytes(bytes))),
        )
        .unwrap();
    }
    assert!(matches!(
        discover_project_recovery_locators(Some(root.path()), ProjectId::from_bytes([0; 16])),
        Err(ProjectRecoveryDiscoveryError::Capacity)
    ));
}

#[test]
fn recovery_locator_discovery_bounds_noncanonical_root_entries() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..=PROJECT_RECOVERY_ROOT_ENTRIES_MAX {
        fs::write(root.path().join(format!("junk-{index}")), b"junk").unwrap();
    }
    assert!(matches!(
        discover_project_recovery_locators(Some(root.path()), ProjectId::from_bytes([0; 16])),
        Err(ProjectRecoveryDiscoveryError::Capacity)
    ));
}

#[test]
fn recovery_discovery_failure_disables_only_provisional_autosave() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..=PROJECT_RECOVERY_ROOT_ENTRIES_MAX {
        fs::write(root.path().join(format!("junk-{index}")), b"junk").unwrap();
    }
    let project_id = ProjectId::from_bytes([5; 16]);

    let (service, warning) = start_project_store_service(Some(root.path()), project_id).unwrap();

    assert!(warning.is_some());
    assert_eq!(service.status().lifecycle(), ProjectStoreLifecycle::Unbound);
    assert!(!service.status().writable());
    assert!(service.can_open());
    assert!(service.can_save());
    service.join().unwrap();
}

#[test]
fn project_destinations_cannot_overlap_the_canonical_source_closure() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.m4d");
    fs::create_dir(&source).unwrap();
    let nested_project = source.join("inside.m4dproj");
    fs::create_dir(&nested_project).unwrap();

    assert!(!project_destination_is_outside_source_closure(&source, &source).unwrap());
    assert!(!project_destination_is_outside_source_closure(&source, &nested_project).unwrap());
    assert!(
        project_destination_is_outside_source_closure(
            &source,
            &root.path().join("sibling.m4dproj"),
        )
        .unwrap()
    );
}

#[test]
fn recovery_root_overlap_is_rejected_before_directory_creation() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.m4d");
    fs::create_dir(&source).unwrap();
    let state_root = source.join("state");
    let recovery_root = state_root.join("mirante4d").join("recovery");

    let error = project_recovery_root_path(&state_root, &source).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(!recovery_root.exists());
}

#[cfg(unix)]
#[test]
fn project_destination_check_resolves_a_symlinked_existing_parent() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.m4d");
    let alias = root.path().join("source-alias");
    fs::create_dir(&source).unwrap();
    symlink(&source, &alias).unwrap();

    assert!(
        !project_destination_is_outside_source_closure(&source, &alias.join("inside.m4dproj"),)
            .unwrap()
    );
}

fn open_dataset_and_render_first_frame(
    path: impl AsRef<std::path::Path>,
) -> anyhow::Result<unified_source_open::UnifiedOpenedSource> {
    crate::unified_source_open::open(path, ResourcePolicy::default(), DatasetSourceId::new(1))
}

fn test_application_for_opened_source(
    opened: &unified_source_open::UnifiedOpenedSource,
) -> ApplicationState {
    ApplicationState::new_unbound(
        SourceSessionGeneration::new(1),
        opened.catalog.as_ref().clone(),
        opened.workspace.clone(),
        ResourcePolicy::default(),
    )
    .expect("the opened test source must satisfy the canonical application model")
}

fn test_workbench_app_without_background_runtime(
    opened: unified_source_open::UnifiedOpenedSource,
) -> MiranteWorkbenchApp {
    let application = test_application_for_opened_source(&opened);
    let unified_source_open::UnifiedOpenedSource {
        startup_diagnostics,
        catalog: _,
        workspace: _,
        dataset,
        render_coordination,
        analysis_runtime,
    } = opened;
    let resource_policy = ResourcePolicy::default();
    let egui_ui = ui_kit::EguiUiState::new(
        resource_policy.cpu_dataset_budget_bytes(),
        resource_policy.gpu_budget_bytes(),
    );
    let (mut settings_connection, _) =
        current_settings_connection::CurrentSettingsConnection::start();
    settings_connection
        .shutdown()
        .expect("the test settings connection must stop before the harness starts");

    MiranteWorkbenchApp {
        application,
        startup_diagnostics,
        dataset,
        render_coordination,
        native_presentation: native_presentation::NativePresentationBridge::unavailable(),
        egui_ui,
        import: ImportWorkflow::new(),
        analysis_runtime,
        validation_runtime: current_runtime::validation::CurrentValidationRuntime {
            product_automation: None,
            test_render_viewport_max_side: None,
        },
        project_store: None,
        project_recovery_root: None,
        project_recovery_candidates: Vec::new(),
        project_recovery_review: None,
        project_recovery_panel_open: false,
        pending_recovery_selection: None,
        pending_project_open_locator: None,
        pending_analysis_artifact_load: None,
        project_store_noninteractive_paths: ProjectStoreNoninteractivePaths::default(),
        project_store_product_evidence: ProjectStoreProductEvidence::default(),
        pending_dataset_open_path: None,
        project_status_message: None,
        close_after_project_save: false,
        exit_after_project_close: false,
        restart_project_store_after_close: false,
        pending_viewport_close: false,
        pending_source_install: None,
        settings_connection,
        source_open_service: None,
        source_verification_service: None,
        pending_automatic_source_verification: None,
    }
}

fn test_workbench_app_for_ui_harness(
    cc: &eframe::CreationContext<'_>,
    opened: unified_source_open::UnifiedOpenedSource,
) -> MiranteWorkbenchApp {
    ui_kit::configure_visuals(&cc.egui_ctx);
    let mut app = test_workbench_app_without_background_runtime(opened);
    app.validation_runtime.test_render_viewport_max_side =
        Some(TEST_INITIAL_RENDER_VIEWPORT_SIDE as usize);
    app
}

include!("tests/fidelity_shell.rs");
include!("tests/architecture.rs");
include!("tests/channels_project.rs");
include!("tests/analysis_workspace.rs");
include!("tests/unified_runtime.rs");
