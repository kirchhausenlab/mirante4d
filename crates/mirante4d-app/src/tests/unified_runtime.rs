#[test]
fn unified_source_open_starts_with_no_owned_interactive_payloads() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();

    assert_eq!(opened.render_runtime.lease_bridge.required_len(), 0);
    assert_eq!(opened.render_runtime.lease_bridge.retained_len(), 0);
    assert!(!opened.dataset.dispatcher().has_pending_work());
    assert_eq!(opened.dataset.current_scale(), ScaleLevel::BASE);
    opened.dataset.request_shutdown().unwrap();
}

#[test]
fn semantic_tile_shape_is_storage_independent_and_clips_edges() {
    assert_eq!(
        semantic_tiles::SemanticTileGrid::new(Shape3D::new(65, 7, 129).unwrap())
            .grid_shape()
            .dimensions(),
        [2, 1, 3]
    );
}

#[test]
fn unified_demand_plan_uses_semantic_keys_for_every_visible_layer() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let application = test_application_for_opened_source(&opened);
    let snapshot = application.snapshot();
    let diagnostics = opened.dataset.dispatcher().diagnostics().unwrap();
    let plan = dataset_demand_plan::plan_current_3d(
        snapshot.catalog(),
        application_view(&snapshot),
        opened.render_runtime.presentation_viewport,
        opened.render_runtime.render_viewport,
        dataset_demand_plan::DatasetDemandPlanLimits::new(
            mirante4d_render_api::MAX_RENDER_REQUIREMENTS,
            mirante4d_render_api::MAX_RENDER_REQUIREMENTS,
            diagnostics.category_cap_bytes(
                mirante4d_dataset::CpuLedgerCategory::DecodedResidency,
            ),
        ),
        false,
    )
    .unwrap();

    let planned_layers = plan
        .resources
        .iter()
        .map(|resource| resource.layer())
        .collect::<std::collections::BTreeSet<_>>();
    let visible_layers = application_view(&snapshot)
        .layers()
        .iter()
        .filter(|layer| layer.visible())
        .map(|layer| layer.layer_key())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(planned_layers, visible_layers);
    assert!(plan.decoded_bytes > 0);
    opened.dataset.request_shutdown().unwrap();
}

#[test]
fn app_dispatches_and_drains_visible_demand_through_one_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let outcome = app.request_visible_bricks();
    assert!(outcome.current_changed);

    let context = egui::Context::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !app.dataset.scope_complete(
        dataset_requests::SCOPE_CURRENT_3D,
        &app.render_runtime.lease_bridge,
    ) {
        assert!(std::time::Instant::now() < deadline);
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }

    assert_eq!(
        app.render_runtime.lease_bridge.required_len(),
        app.render_runtime.lease_bridge.retained_len()
    );
    let diagnostics = app.dataset.dispatcher().diagnostics().unwrap();
    assert!(diagnostics.resident_resources() > 0);
    assert!(diagnostics.total_used_bytes() <= diagnostics.total_cap_bytes());
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn import_cancellation_waits_for_the_worker_terminal_result() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let token = app
        .begin_background_operation(OperationKind::Import)
        .unwrap();
    let cancellation = ImportCancellation::new();
    let (_sender, receiver) = mpsc::channel();
    app.import_runtime.import_task = Some(ImportTask {
        token: token.clone(),
        destination: temp.path().join("imported.m4d"),
        cancellation: cancellation.clone(),
        receiver,
        latest_event: None,
        retry_options: None,
        worker: None,
    });

    app.cancel_import_task();

    assert!(cancellation.is_cancelled());
    assert!(
        app.application
            .snapshot()
            .active_operations()
            .contains(&token)
    );
    app.import_runtime.import_task = None;
    assert!(app.complete_background_operation(token, OperationCompletion::Succeeded));
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn imported_dataset_uses_the_existing_dirty_project_open_handoff() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    verify_test_source(&mut app);
    app.apply_application_command(ApplicationCommand::AttachVerifiedDataset, &context)
        .unwrap();
    assert!(app.project_dirty());
    let imported = temp.path().join("imported.m4d");

    assert!(!app.open_or_queue_dataset_path(imported.clone(), None).unwrap());

    assert_eq!(app.pending_dataset_open_path.as_ref(), Some(&imported));
    assert!(app.ui_runtime.close_prompt_open);
    assert!(
        app.application
            .snapshot()
            .active_operations()
            .iter()
            .all(|operation| operation.kind() != OperationKind::DatasetOpen)
    );
    app.dataset.request_shutdown().unwrap();
    app.source_verification_service
        .take()
        .unwrap()
        .shutdown()
        .unwrap();
}

#[test]
fn exact_analyses_cancel_save_and_reopen_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let project_path = temp.path().join("analysis-result.m4dproj");
    let context = egui::Context::default();

    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    install_test_project_store(&mut app);
    verify_test_source(&mut app);
    app.apply_application_command(ApplicationCommand::AttachVerifiedDataset, &context)
        .unwrap();
    assert!(app.project_dirty());
    app.project_store_noninteractive_paths.initial_save = Some(project_path.clone());
    app.apply_application_command(ApplicationCommand::RequestProjectSave, &context)
        .unwrap();
    if wait_for_initial_project_save(&mut app) == InitialProjectSave::UnsupportedFilesystem {
        assert!(!project_path.exists());
        assert!(app.project_dirty());
        assert!(app.analysis_start_unavailable_reason().is_some());
        close_test_project_store(&mut app);
        app.dataset.request_shutdown().unwrap();
        app.source_verification_service
            .take()
            .unwrap()
            .shutdown()
            .unwrap();
        return;
    }

    assert_eq!(app.analysis_start_unavailable_reason(), None);
    app.start_product_analysis(analysis_product::ProductAnalysisScope::FullTimeTrace)
        .unwrap();
    app.request_analysis_cancel().unwrap();
    wait_for_test_app(&mut app, |app| {
        app.analysis_runtime.active_token().is_none()
            && !app
                .application
                .snapshot()
                .active_operations()
                .iter()
                .any(|token| token.kind() == OperationKind::Analysis)
    });
    assert!(app.application.snapshot().transient().analysis_tables().is_empty());
    assert!(app.application.snapshot().transient().analysis_plots().is_empty());

    app.start_product_analysis(analysis_product::ProductAnalysisScope::FullTimeTrace)
        .unwrap();
    wait_for_test_app(&mut app, |app| {
        app.analysis_runtime.active_token().is_none()
            && app.application.snapshot().transient().analysis_tables().len() == 1
            && app.application.snapshot().transient().analysis_plots().len() == 1
            && app
                .project_store
                .as_ref()
                .is_some_and(|service| !service.has_pending_work())
    });
    let saved_snapshot = app.application.snapshot();
    let table_id = saved_snapshot.transient().analysis_tables()[0].id();
    let plot_id = saved_snapshot.transient().analysis_plots()[0].id();
    assert_eq!(app.analysis_runtime.table(table_id).unwrap().rows().len(), 3);
    assert_eq!(app.analysis_runtime.plot(plot_id).unwrap().points().len(), 3);
    assert!(!app.project_dirty());

    app.start_product_analysis(analysis_product::ProductAnalysisScope::FullTimeTrace)
        .unwrap();
    wait_for_test_app(&mut app, |app| {
        app.analysis_runtime.active_token().is_none()
            && app.application.snapshot().transient().analysis_tables().len() == 2
            && app.application.snapshot().transient().analysis_plots().len() == 2
    });

    app.analysis_runtime.set_roi([1, 1, 1], [2, 2, 2]).unwrap();
    app.start_product_analysis(analysis_product::ProductAnalysisScope::CurrentTimepointBox)
        .unwrap();
    wait_for_test_app(&mut app, |app| {
        app.analysis_runtime.active_token().is_none()
            && app.application.snapshot().transient().analysis_tables().len() == 3
            && app.application.snapshot().transient().analysis_plots().len() == 2
            && app
                .project_store
                .as_ref()
                .is_some_and(|service| !service.has_pending_work())
    });
    let box_table_id = app
        .application
        .snapshot()
        .transient()
        .selected_analysis_table()
        .expect("the completed box analysis is selected");
    assert_eq!(app.analysis_runtime.table(box_table_id).unwrap().rows().len(), 1);
    assert!(project_path.is_dir());

    close_test_project_store(&mut app);
    app.dataset.request_shutdown().unwrap();
    app.source_verification_service
        .take()
        .unwrap()
        .shutdown()
        .unwrap();
    drop(app);

    let reopened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut reopened_app = test_workbench_app_without_background_runtime(reopened);
    install_test_project_store(&mut reopened_app);
    verify_test_source(&mut reopened_app);
    reopened_app.project_store_noninteractive_paths.open = Some(project_path);
    reopened_app
        .apply_application_command(ApplicationCommand::RequestProjectOpen, &context)
        .unwrap();
    wait_for_test_app(&mut reopened_app, |reopened_app| {
        let snapshot = reopened_app.application.snapshot();
        matches!(snapshot.workspace(), WorkspaceSnapshot::Bound { .. })
            && snapshot.transient().analysis_tables().len() == 3
            && snapshot.transient().analysis_plots().len() == 2
            && reopened_app.pending_analysis_artifact_load.is_none()
    });
    let reopened_snapshot = reopened_app.application.snapshot();
    let mut row_counts = reopened_snapshot
        .transient()
        .analysis_tables()
        .iter()
        .map(|descriptor| {
            reopened_app
                .analysis_runtime
                .table(descriptor.id())
                .expect("reopened analysis table")
                .rows()
                .len()
        })
        .collect::<Vec<_>>();
    row_counts.sort_unstable();
    assert_eq!(row_counts, vec![1, 3, 3]);
    assert!(reopened_snapshot.transient().analysis_plots().iter().all(
        |descriptor| reopened_app
            .analysis_runtime
            .plot(descriptor.id())
            .is_some_and(|plot| plot.points().len() == 3)
    ));

    close_test_project_store(&mut reopened_app);
    reopened_app.dataset.request_shutdown().unwrap();
    reopened_app
        .source_verification_service
        .take()
        .unwrap()
        .shutdown()
        .unwrap();
}

#[test]
fn analysis_only_source_failure_invalidates_the_verified_source() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let project_path = temp.path().join("analysis-source-failure.m4dproj");
    let context = egui::Context::default();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    install_test_project_store(&mut app);
    verify_test_source(&mut app);
    app.apply_application_command(ApplicationCommand::AttachVerifiedDataset, &context)
        .unwrap();
    app.project_store_noninteractive_paths.initial_save = Some(project_path);
    app.apply_application_command(ApplicationCommand::RequestProjectSave, &context)
        .unwrap();
    if wait_for_initial_project_save(&mut app) == InitialProjectSave::UnsupportedFilesystem {
        assert!(app.project_dirty());
        assert!(app.analysis_start_unavailable_reason().is_some());
        close_test_project_store(&mut app);
        app.dataset.request_shutdown().unwrap();
        app.source_verification_service
            .take()
            .unwrap()
            .shutdown()
            .unwrap();
        return;
    }

    fs::remove_dir_all(&package).unwrap();
    app.start_product_analysis(analysis_product::ProductAnalysisScope::FullTimeTrace)
        .unwrap();
    wait_for_test_app(&mut app, |app| {
        app.analysis_runtime.active_token().is_none()
            && matches!(
                app.application.snapshot().source(),
                SourceVerificationSnapshot::Required
            )
    });
    assert!(app.application.snapshot().transient().analysis_tables().is_empty());
    assert!(app.application.snapshot().transient().analysis_plots().is_empty());

    close_test_project_store(&mut app);
    app.dataset.request_shutdown().unwrap();
    app.source_verification_service
        .take()
        .unwrap()
        .shutdown()
        .unwrap();
}

fn install_test_project_store(app: &mut MiranteWorkbenchApp) {
    let snapshot = app.application.snapshot();
    let WorkspaceSnapshot::Unbound { workspace } = snapshot.workspace() else {
        panic!("test project store must start before the workspace is bound");
    };
    let (service, warning) =
        start_project_store_service(None, workspace.provisional_project_id()).unwrap();
    assert_eq!(warning, None);
    app.project_store = Some(service);
}

fn verify_test_source(app: &mut MiranteWorkbenchApp) {
    app.source_verification_service = Some(
        current_source_verification_service::CurrentSourceVerificationService::new(),
    );
    app.request_current_source_verification();
    wait_for_test_app(app, |app| {
        matches!(
            app.application.snapshot().source(),
            SourceVerificationSnapshot::Verified(_)
        ) && app
            .source_verification_service
            .as_ref()
            .is_some_and(|service| service.active_token().is_none())
    });
}

fn wait_for_test_app(
    app: &mut MiranteWorkbenchApp,
    mut ready: impl FnMut(&MiranteWorkbenchApp) -> bool,
) {
    let context = egui::Context::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !ready(app) {
        if std::time::Instant::now() >= deadline {
            let snapshot = app.application.snapshot();
            panic!(
                "test app timed out: active={:?}, tables={}, plots={}, pending_load={:?}, status={:?}, store={:?}",
                app.analysis_runtime.active_token(),
                snapshot.transient().analysis_tables().len(),
                snapshot.transient().analysis_plots().len(),
                app.pending_analysis_artifact_load,
                app.project_status_message,
                app.project_store.as_ref().map(|service| service.status()),
            );
        }
        app.pump_application_services();
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InitialProjectSave {
    Established,
    UnsupportedFilesystem,
}

fn wait_for_initial_project_save(app: &mut MiranteWorkbenchApp) -> InitialProjectSave {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let unsupported = format!(
        "Project operation failed: {}",
        ProjectStoreFault::UnsupportedFilesystem
    );
    loop {
        app.pump_application_services();
        let status = app.project_store.as_ref().unwrap().status();
        if status.lifecycle() == ProjectStoreLifecycle::Established
            && !status.foreground_active()
            && !status.autosave_active()
            && !app.project_dirty()
        {
            return InitialProjectSave::Established;
        }
        if status.lifecycle() == ProjectStoreLifecycle::Unbound
            && !status.foreground_active()
            && !status.autosave_active()
            && app.project_status_message.as_deref() == Some(unsupported.as_str())
        {
            return InitialProjectSave::UnsupportedFilesystem;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "initial project save did not complete: status={status:?}, message={:?}",
            app.project_status_message,
        );
        std::thread::yield_now();
    }
}

fn close_test_project_store(app: &mut MiranteWorkbenchApp) {
    app.project_store.as_mut().unwrap().close().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while app.project_store.is_some() {
        assert!(std::time::Instant::now() < deadline);
        app.pump_application_services();
        std::thread::yield_now();
    }
}

#[test]
fn terminal_decode_failure_is_stable_until_the_scope_changes() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    fs::remove_dir_all(&package).unwrap();
    let context = egui::Context::default();

    app.request_visible_bricks();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while app.dataset.dispatcher().diagnostics().unwrap().failed_requests() == 0 {
        assert!(std::time::Instant::now() < deadline);
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }
    let submitted = app
        .dataset
        .dispatcher()
        .diagnostics()
        .unwrap()
        .submitted_requests();
    for _ in 0..8 {
        app.request_visible_bricks();
        app.drain_brick_results(&context);
    }

    assert_eq!(
        app.dataset
            .dispatcher()
            .diagnostics()
            .unwrap()
            .submitted_requests(),
        submitted
    );
    assert!(
        app.render_runtime
            .frame_fidelity
            .last_capacity_error
            .is_some()
    );
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn observed_source_fault_invalidates_then_retry_restores_runtime_identity_coherence() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    app.source_verification_service = Some(
        current_source_verification_service::CurrentSourceVerificationService::new(),
    );
    app.request_current_source_verification();
    app.pump_application_services();

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        app.pump_application_services();
        let verified = matches!(
            app.application.snapshot().source(),
            SourceVerificationSnapshot::Verified(_)
        );
        let idle = app
            .source_verification_service
            .as_ref()
            .unwrap()
            .active_token()
            .is_none();
        if verified && idle {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    let verified_identity = app
        .application
        .snapshot()
        .catalog()
        .scientific_identity()
        .resource_identity();
    assert_eq!(app.dataset.resource_identity(), verified_identity);
    assert!(app.render_runtime.lease_bridge.required_len() > 0);
    let completion_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let diagnostics = app.dataset.dispatcher().diagnostics().unwrap();
        if diagnostics.pending_completions() > 0 {
            assert!(diagnostics.ready_requests() > 0);
            break;
        }
        assert!(std::time::Instant::now() < completion_deadline);
        std::thread::yield_now();
    }

    app.record_dataset_fault(&mirante4d_dataset_runtime::RuntimeFault::new(
        mirante4d_dataset_runtime::RuntimeFaultCode::SourceRejected,
    ));
    app.pump_application_services();
    assert!(matches!(
        app.application.snapshot().source(),
        SourceVerificationSnapshot::Required
    ));
    assert_eq!(app.render_runtime.lease_bridge.required_len(), 0);
    assert_eq!(app.render_runtime.lease_bridge.retained_len(), 0);
    assert!(app.dataset.renderer_requirements().is_empty());
    for scope in [
        dataset_requests::SCOPE_CURRENT_3D,
        dataset_requests::SCOPE_CROSS_SECTION_XY,
        dataset_requests::SCOPE_CROSS_SECTION_XZ,
        dataset_requests::SCOPE_CROSS_SECTION_YZ,
        dataset_requests::SCOPE_PLAYBACK,
    ] {
        assert!(app.dataset.scope_requirements(scope).is_empty());
    }
    assert_ne!(
        app.dataset.resource_identity(),
        app.application
            .snapshot()
            .catalog()
            .scientific_identity()
            .resource_identity()
    );
    let submitted_before = app
        .dataset
        .dispatcher()
        .diagnostics()
        .unwrap()
        .submitted_requests();
    assert_eq!(
        app.request_visible_bricks(),
        workbench_brick_runtime::VisibleBrickRequestOutcome::default()
    );
    assert_eq!(
        app.dataset
            .dispatcher()
            .diagnostics()
            .unwrap()
            .submitted_requests(),
        submitted_before
    );
    let context = egui::Context::default();
    for _ in 0..4 {
        app.drain_brick_results(&context);
    }
    assert_eq!(
        app.dataset
            .dispatcher()
            .diagnostics()
            .unwrap()
            .pending_completions(),
        0
    );
    assert_eq!(app.render_runtime.lease_bridge.required_len(), 0);
    assert_eq!(app.render_runtime.lease_bridge.retained_len(), 0);

    app.apply_application_command(ApplicationCommand::RequestSourceVerification, &context)
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        app.pump_application_services();
        let verified = matches!(
            app.application.snapshot().source(),
            SourceVerificationSnapshot::Verified(_)
        );
        let idle = app
            .source_verification_service
            .as_ref()
            .unwrap()
            .active_token()
            .is_none();
        if verified && idle {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(
        app.dataset.resource_identity(),
        app.application
            .snapshot()
            .catalog()
            .scientific_identity()
            .resource_identity()
    );

    app.dataset.request_shutdown().unwrap();
    app.source_verification_service
        .take()
        .unwrap()
        .shutdown()
        .unwrap();
}

#[test]
fn automatic_source_verification_waits_for_the_previous_worker_to_retire() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let replacement = crate::unified_source_open::open(
        &package,
        ResourcePolicy::default(),
        DatasetSourceId::new(2),
    )
    .unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    app.source_verification_service = Some(
        current_source_verification_service::CurrentSourceVerificationService::new(),
    );

    struct BlockingLedger {
        delegate: Arc<dyn mirante4d_dataset::CpuByteLedger>,
        entered: std::sync::atomic::AtomicBool,
        released: std::sync::Mutex<bool>,
        release: std::sync::Condvar,
    }

    impl mirante4d_dataset::CpuByteLedger for BlockingLedger {
        fn try_acquire(
            &self,
            category: mirante4d_dataset::CpuLedgerCategory,
            bytes: u64,
        ) -> Result<
            Box<dyn mirante4d_dataset::CpuByteLease>,
            mirante4d_dataset::CpuLedgerError,
        > {
            self.entered
                .store(true, std::sync::atomic::Ordering::Release);
            let mut released = self.released.lock().unwrap();
            while !*released {
                released = self.release.wait(released).unwrap();
            }
            self.delegate.try_acquire(category, bytes)
        }
    }

    let blocking_ledger = Arc::new(BlockingLedger {
        delegate: app.dataset.cpu_ledger_arc(),
        entered: std::sync::atomic::AtomicBool::new(false),
        released: std::sync::Mutex::new(false),
        release: std::sync::Condvar::new(),
    });

    app.request_current_source_verification();
    let events = app.application.drain_events(256);
    let token = events
        .iter()
        .find_map(|event| match event {
            ApplicationEvent::SourceVerificationRequested { token } => Some(token.clone()),
            _ => None,
        })
        .expect("the production request must emit its worker token");
    app.source_verification_service
        .as_mut()
        .unwrap()
        .request_verification(
            token.clone(),
            package.clone(),
            ResourcePolicy::default(),
            Arc::clone(&blocking_ledger) as Arc<dyn mirante4d_dataset::CpuByteLedger>,
        )
        .unwrap();
    let worker_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !blocking_ledger
        .entered
        .load(std::sync::atomic::Ordering::Acquire)
    {
        assert!(std::time::Instant::now() < worker_deadline);
        std::thread::yield_now();
    }

    app.application
        .dispatch(ApplicationCommand::CancelOperation(token.operation_id()))
        .unwrap();
    for event in app.application.drain_events(256) {
        app.observe_source_application_event(&event);
    }
    assert!(matches!(
        app.application.snapshot().source(),
        SourceVerificationSnapshot::Required
    ));

    app.application
        .dispatch(ApplicationCommand::RequestDatasetOpen)
        .unwrap();
    let open_token = app
        .application
        .drain_events(256)
        .into_iter()
        .find_map(|event| match event {
            ApplicationEvent::DatasetOpenRequested { token } => Some(token),
            _ => None,
        })
        .expect("the replacement source must issue a dataset-open token");
    let replacement_generation = SourceSessionGeneration::new(2);
    let unified_source_open::UnifiedOpenedSource {
        dataset,
        catalog,
        workspace,
        render_runtime,
        analysis_runtime,
        startup_diagnostics: _,
    } = replacement;
    app.application
        .dispatch(ApplicationCommand::CompleteOperation {
            token: open_token,
            completion: OperationCompletion::DatasetOpened {
                catalog,
                workspace: Box::new(workspace),
                source_generation: replacement_generation,
            },
        })
        .unwrap();
    app.install_current_source_runtime(
        current_source_open_service::CurrentSourceRuntimeTransfer {
            dataset,
            render_runtime,
            analysis_runtime,
        },
    );

    assert_eq!(
        app.pending_automatic_source_verification,
        Some(replacement_generation)
    );
    assert!(matches!(
        app.application.snapshot().source(),
        SourceVerificationSnapshot::Required
    ));
    assert_eq!(
        app.source_verification_service
            .as_ref()
            .unwrap()
            .active_token(),
        Some(&token)
    );

    *blocking_ledger.released.lock().unwrap() = true;
    blocking_ledger.release.notify_all();
    let verification_deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        app.pump_application_services();
        let snapshot = app.application.snapshot();
        let verified = snapshot.source_generation() == replacement_generation
            && matches!(snapshot.source(), SourceVerificationSnapshot::Verified(_));
        let idle = app
            .source_verification_service
            .as_ref()
            .unwrap()
            .active_token()
            .is_none();
        if verified && idle {
            break;
        }
        assert!(std::time::Instant::now() < verification_deadline);
        std::thread::yield_now();
    }
    assert_eq!(app.pending_automatic_source_verification, None);
    assert_eq!(app.application.snapshot().source_generation(), replacement_generation);
    assert_eq!(
        app.dataset.resource_identity(),
        app.application
            .snapshot()
            .catalog()
            .scientific_identity()
            .resource_identity()
    );
    let diagnostics = app
        .source_verification_service
        .as_ref()
        .unwrap()
        .diagnostics();
    assert!(diagnostics.cancelled_runs >= 1);
    assert!(diagnostics.accepted_successes >= 1);

    app.source_verification_service
        .take()
        .unwrap()
        .shutdown()
        .unwrap();
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn playback_prefetch_readiness_is_backed_by_retained_accounted_leases() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    app.apply_application_command(ApplicationCommand::SetPlaybackActive(true), &context)
        .unwrap();
    app.request_visible_bricks();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !app.dataset.scope_complete(
        dataset_requests::SCOPE_PLAYBACK,
        &app.render_runtime.lease_bridge,
    ) {
        assert!(std::time::Instant::now() < deadline);
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }
    let playback = app
        .dataset
        .scope_requirements(dataset_requests::SCOPE_PLAYBACK);
    assert!(!playback.is_empty());
    assert!(
        playback
            .iter()
            .all(|key| app.render_runtime.lease_bridge.payload(*key).is_some())
    );
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn four_panel_playback_demand_shares_one_aggregate_resource_and_byte_budget() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    let snapshot = app.application.snapshot();
    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section: *application_view(&snapshot).cross_section(),
        },
        &context,
    )
    .unwrap();
    let presentation = PresentationViewport::new(64.0, 64.0).unwrap();
    let render = RenderViewport::new(64, 64).unwrap();
    for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        app.render_runtime
            .cross_section_runtime
            .record_panel_viewports(panel, presentation, render);
    }
    app.apply_application_command(ApplicationCommand::SetPlaybackActive(true), &context)
        .unwrap();

    app.request_visible_bricks();

    assert_eq!(app.dataset.last_plan_error(), None);
    for scope in [
        dataset_requests::SCOPE_CURRENT_3D,
        dataset_requests::SCOPE_PLAYBACK,
        dataset_requests::SCOPE_CROSS_SECTION_XY,
        dataset_requests::SCOPE_CROSS_SECTION_XZ,
        dataset_requests::SCOPE_CROSS_SECTION_YZ,
    ] {
        assert!(!app.dataset.scope_requirements(scope).is_empty());
    }
    let requirements = app.dataset.renderer_requirements();
    let decoded_bytes = requirements.iter().try_fold(0_u64, |total, resource| {
        total.checked_add(
            app.application
                .snapshot()
                .catalog()
                .resource_payload_descriptor(*resource)
                .unwrap()
                .byte_len(),
        )
    });
    let diagnostics = app.dataset.dispatcher().diagnostics().unwrap();
    assert!(requirements.len() <= mirante4d_render_api::MAX_RENDER_REQUIREMENTS);
    assert!(
        decoded_bytes.unwrap()
            <= diagnostics.category_cap_bytes(
                mirante4d_dataset::CpuLedgerCategory::DecodedResidency,
            )
    );
    app.dataset.request_shutdown().unwrap();
}
