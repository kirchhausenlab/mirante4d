use std::{fs, time::Duration};

use eframe::egui;
use mirante4d_format::{FixtureKind, write_fixture};

const TEST_INITIAL_RENDER_VIEWPORT_SIDE: u64 = 32;

use super::*;

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
        render_runtime,
        analysis_runtime,
    } = opened;
    let resource_policy = ResourcePolicy::default();
    let ui_runtime = current_runtime::ui::CurrentUiRuntime::new(resource_policy, None);
    let (mut settings_connection, _) =
        current_settings_connection::CurrentSettingsConnection::start();
    settings_connection
        .shutdown()
        .expect("the test settings connection must stop before the harness starts");

    MiranteWorkbenchApp {
        application,
        startup_diagnostics,
        dataset,
        render_runtime,
        ui_runtime,
        import_runtime: current_runtime::import::ImportRuntime::idle(),
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
