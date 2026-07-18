#[test]
fn unified_source_open_starts_with_no_owned_interactive_payloads() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();

    assert_eq!(opened.dataset.retained_leases().required_len(), 0);
    assert_eq!(opened.dataset.retained_leases().retained_len(), 0);
    assert!(!opened.dataset.dispatcher().has_pending_work());
    assert_eq!(opened.dataset.current_scale(), ScaleLevel::BASE);
    opened.dataset.request_shutdown().unwrap();
}

#[test]
fn headless_smoke_installs_demand_through_the_prepared_transaction() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let report = run_headless_smoke(
        &package,
        AppSmokeOptions {
            disable_gpu: true,
            playback_steps: 0,
            timeout: Duration::from_secs(5),
        },
    )
    .unwrap();

    assert!(report.nonzero_pixels > 0);
    assert!(report.max_value > 0);
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
        opened.render_coordination.presentation_viewport,
        opened.render_coordination.render_viewport,
        dataset_demand_plan::DatasetDemandPlanLimits::new(
            mirante4d_render_api::MAX_RENDER_REQUIREMENTS,
            mirante4d_render_api::MAX_RENDER_REQUIREMENTS,
            diagnostics.category_cap_bytes(mirante4d_dataset::CpuLedgerCategory::DecodedResidency),
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
    assert!(plan.payload_bytes > 0);
    opened.dataset.request_shutdown().unwrap();
}

#[test]
fn app_dispatches_and_drains_visible_demand_through_one_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let outcome = await_visible_demand_plan(&mut app);
    assert!(outcome.current_changed);

    let context = egui::Context::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !app
        .dataset
        .scope_complete(dataset_requests::SCOPE_CURRENT_3D)
    {
        assert!(std::time::Instant::now() < deadline);
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }

    assert_eq!(
        app.dataset.retained_leases().required_len(),
        app.dataset.retained_leases().retained_len()
    );
    let diagnostics = app.dataset.dispatcher().diagnostics().unwrap();
    assert!(diagnostics.resident_resources() > 0);
    assert!(diagnostics.total_used_bytes() <= diagnostics.total_cap_bytes());
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn one_camera_change_runs_one_semantic_demand_plan() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let view = application_view(&app.application.snapshot()).clone();
    let camera = view.camera();
    let changed_camera = mirante4d_domain::CameraView::new(
        camera.projection(),
        camera.target(),
        camera.orientation(),
        camera.orthographic_world_per_screen_point() * 0.9,
        camera.perspective_focal_length_screen_points(),
        camera.perspective_view_distance_world(),
    )
    .unwrap();
    let before = app.visible_demand_plan_calls;

    app.apply_application_command(
        ApplicationCommand::SetCamera(changed_camera),
        &egui::Context::default(),
    )
    .unwrap();

    assert_eq!(app.visible_demand_plan_calls, before + 1);
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn unchanged_completion_refreshes_reuse_one_semantic_demand_plan() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);

    await_visible_demand_plan(&mut app);
    let planned = app.visible_demand_plan_calls;
    for _ in 0..100 {
        app.request_visible_bricks();
    }

    assert_eq!(app.visible_demand_plan_calls, planned);
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn resident_extent_only_plan_install_wakes_exactly_one_new_frame() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    await_visible_demand_plan(&mut app);
    let context = egui::Context::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !app
        .dataset
        .scope_complete(dataset_requests::SCOPE_CURRENT_3D)
    {
        assert!(std::time::Instant::now() < deadline);
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }
    while app.render_coordination.take_refresh_request() {}

    let previous_extent = app.render_coordination.render_viewport;
    let resized = mirante4d_render_api::RenderExtent::new(
        previous_extent.width_pixels() + 1,
        previous_extent.height_pixels() + 1,
    )
    .unwrap();
    let submitted_before = app
        .dataset
        .dispatcher()
        .diagnostics()
        .unwrap()
        .submitted_requests();
    assert!(app.render_coordination.set_render_viewport(resized));
    app.render_coordination.request_refresh();
    assert!(app.render_coordination.take_refresh_request());

    let submitted = app.request_visible_bricks();
    assert!(!submitted.current_plan_installed);
    assert!(app.pending_current_3d_demand());
    while !app.render_coordination.refresh_requested() {
        assert!(
            std::time::Instant::now() < deadline,
            "resident extent-only plan did not wake a replacement frame"
        );
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }

    assert!(!app.pending_current_3d_demand());
    assert!(app.render_coordination.take_refresh_request());
    assert!(!app.render_coordination.take_refresh_request());
    app.drain_brick_results(&context);
    assert!(
        !app.render_coordination.take_refresh_request(),
        "an unchanged installed plan must not create a render loop"
    );
    assert_eq!(
        app.dataset
            .dispatcher()
            .diagnostics()
            .unwrap()
            .submitted_requests(),
        submitted_before,
        "extent-only reuse must not re-decode resident resources"
    );
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn transfer_only_edit_skips_planning_and_attempts_one_dynamic_render() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    await_visible_demand_plan(&mut app);
    let context = egui::Context::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !app
        .dataset
        .scope_complete(dataset_requests::SCOPE_CURRENT_3D)
    {
        assert!(std::time::Instant::now() < deadline);
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }

    let snapshot = app.application.snapshot();
    let layer = application_view(&snapshot)
        .layer(application_view(&snapshot).active_layer())
        .unwrap();
    let transfer = layer.transfer();
    let changed_transfer = mirante4d_domain::LayerTransfer::new(
        transfer.window(),
        transfer.color(),
        transfer.opacity(),
        mirante4d_domain::TransferCurve::gamma(2.0).unwrap(),
        transfer.invert(),
    );
    let changed_layer = mirante4d_project_model::LayerViewState::new(
        layer.layer_key(),
        layer.visible(),
        changed_transfer,
        *layer.render_state(),
    );
    let plans_before = app.visible_demand_plan_calls;
    let renders_before = app.product_render_attempts;

    app.apply_application_command(ApplicationCommand::SetLayerView(changed_layer), &context)
        .unwrap();

    assert_eq!(app.visible_demand_plan_calls, plans_before);
    assert_eq!(app.product_render_attempts, renders_before + 1);
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn render_mode_switch_skips_membership_planning_and_attempts_one_dynamic_render() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    await_visible_demand_plan(&mut app);
    let context = egui::Context::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !app
        .dataset
        .scope_complete(dataset_requests::SCOPE_CURRENT_3D)
    {
        assert!(std::time::Instant::now() < deadline);
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }

    let snapshot = app.application.snapshot();
    let layer = application_view(&snapshot)
        .layer(application_view(&snapshot).active_layer())
        .unwrap();
    let changed_render_state = mirante4d_domain::RenderState::dvr(
        mirante4d_domain::SamplingPolicy::VoxelExact,
        mirante4d_domain::DvrOpacityTransfer::new(
            layer.transfer().window(),
            layer.transfer().curve(),
        ),
        12.0,
    )
    .unwrap();
    let changed_layer = mirante4d_project_model::LayerViewState::new(
        layer.layer_key(),
        layer.visible(),
        layer.transfer().clone(),
        changed_render_state,
    );
    let plans_before = app.visible_demand_plan_calls;
    let renders_before = app.product_render_attempts;

    app.apply_application_command(ApplicationCommand::SetLayerView(changed_layer), &context)
        .unwrap();

    assert_eq!(app.visible_demand_plan_calls, plans_before);
    assert_eq!(app.product_render_attempts, renders_before + 1);
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn verification_promotes_the_live_source_without_redecode_or_runtime_rekey() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let runtime_identity = app.dataset.resource_identity();
    await_visible_demand_plan(&mut app);

    let context = egui::Context::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !app
        .dataset
        .scope_complete(dataset_requests::SCOPE_CURRENT_3D)
    {
        assert!(std::time::Instant::now() < deadline);
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }
    await_visible_demand_plan(&mut app);
    let retained_before = app.dataset.retained_leases().retained_len();
    let runtime_before = app.dataset.dispatcher().diagnostics().unwrap();
    let storage_before = app.dataset.local_source_diagnostics().unwrap();

    verify_test_source(&mut app);

    let snapshot = app.application.snapshot();
    assert!(snapshot.catalog().scientific_identity().is_verified());
    assert_eq!(snapshot.catalog().resource_identity(), runtime_identity);
    assert_eq!(app.dataset.resource_identity(), runtime_identity);
    assert_eq!(
        app.dataset.retained_leases().retained_len(),
        retained_before
    );
    assert!(
        app.dataset
            .local_source()
            .is_some_and(|source| source.package_id().is_some())
    );
    let runtime_after = app.dataset.dispatcher().diagnostics().unwrap();
    let storage_after = app.dataset.local_source_diagnostics().unwrap();
    assert_eq!(
        runtime_after.started_decodes(),
        runtime_before.started_decodes()
    );
    assert_eq!(
        runtime_after.completed_decodes(),
        runtime_before.completed_decodes()
    );
    assert_eq!(
        storage_after.physical_brick_unique_decodes,
        storage_before.physical_brick_unique_decodes
    );
    assert_eq!(
        storage_after.reader.codec_decode_operations,
        storage_before.reader.codec_decode_operations
    );

    app.dataset.request_shutdown().unwrap();
    app.source_verification_service
        .take()
        .unwrap()
        .shutdown()
        .unwrap();
}

#[test]
fn differential_scope_update_keeps_overlap_and_retires_only_removed_waiters() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let snapshot = app.application.snapshot();
    let layer = application_view(&snapshot).active_layer();
    let scale = snapshot
        .catalog()
        .layer(layer)
        .unwrap()
        .scales()
        .next()
        .unwrap();
    assert!(scale.shape().x() >= 2);
    let key = |x| {
        mirante4d_dataset::DatasetResourceKey::new(
            snapshot.catalog().resource_identity(),
            layer,
            application_view(&snapshot).timepoint(),
            scale.level(),
            mirante4d_dataset::ResourceRegion::new([0, 0, x], Shape3D::new(1, 1, 1).unwrap())
                .unwrap(),
        )
    };
    let retained = key(0);
    let retired = key(1);
    dataset_requests::install_prepared_scope_test_fixture(
        &mut app.dataset,
        dataset_requests::SCOPE_CURRENT_3D,
        vec![retained, retired],
    )
    .unwrap();
    app.dataset.begin_submission_pass();
    app.dataset
        .submit_scope(
            dataset_requests::SCOPE_CURRENT_3D,
            mirante4d_dataset_runtime::RequestPriority::CurrentView,
        )
        .unwrap();
    let retained_ticket = app
        .dataset
        .dispatcher()
        .pending_ticket(dataset_requests::SCOPE_CURRENT_3D, retained)
        .unwrap();
    assert!(
        app.dataset
            .dispatcher()
            .pending_ticket(dataset_requests::SCOPE_CURRENT_3D, retired)
            .is_some()
    );
    let submitted = app
        .dataset
        .dispatcher()
        .diagnostics()
        .unwrap()
        .submitted_requests();

    dataset_requests::install_prepared_scope_test_fixture(
        &mut app.dataset,
        dataset_requests::SCOPE_CURRENT_3D,
        vec![retained],
    )
    .unwrap();
    app.dataset.begin_submission_pass();
    app.dataset
        .submit_scope(
            dataset_requests::SCOPE_CURRENT_3D,
            mirante4d_dataset_runtime::RequestPriority::CurrentView,
        )
        .unwrap();

    assert_eq!(
        app.dataset
            .dispatcher()
            .pending_ticket(dataset_requests::SCOPE_CURRENT_3D, retained),
        Some(retained_ticket)
    );
    assert_eq!(
        app.dataset
            .dispatcher()
            .pending_ticket(dataset_requests::SCOPE_CURRENT_3D, retired),
        None
    );
    assert_eq!(
        app.dataset
            .dispatcher()
            .diagnostics()
            .unwrap()
            .submitted_requests(),
        submitted
    );
    app.dataset.request_shutdown().unwrap();
}

fn reviewed_import_options(
    source: TiffSource,
    inspection: mirante4d_import_pipeline::TiffInspection,
    destination: PathBuf,
) -> (ImportReviewId, ImportOptions) {
    let mut workflow = ImportWorkflow::new();
    let review_id = workflow
        .install_review(source, inspection, destination)
        .unwrap();
    let mut draft = workflow.pending_review.as_ref().unwrap().initial_draft;
    draft.calibration_confirmed = true;
    draft.time_step_seconds = Some(1.0);
    let options = workflow.start_options(review_id, draft).unwrap().unwrap();
    (review_id, options)
}

#[test]
fn inspection_review_and_typed_start_form_one_import_workflow() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let source = write_source_time_series_fixture(temp.path()).unwrap();
    let destination = temp.path().join("typed-import.m4d");
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();

    app.enter_tiff_import_setup_waiting_state(TiffSource::auto(&source), destination)
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        app.drain_tiff_import_setup_results(&context);
        if matches!(
            app.application_snapshot_for_ui().import_workflow(),
            ImportWorkflowSnapshot::Review(_)
        ) {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }

    let ImportWorkflowSnapshot::Review(review) = app.import.snapshot() else {
        panic!("inspection must produce an import review");
    };
    let mut draft = review.initial_draft;
    app.apply_import_command(
        ImportCommand::Start {
            review_id: review.review_id,
            draft,
        },
        &context,
    );
    assert!(matches!(
        app.import.snapshot(),
        ImportWorkflowSnapshot::Failed(_)
    ));
    assert!(app.import.pending_review.is_some());
    app.apply_import_command(ImportCommand::DismissProblem, &context);

    draft.calibration_confirmed = true;
    draft.time_step_seconds = Some(1.0);
    let stale_id = ImportReviewId::new(review.review_id.get() + 1);
    app.apply_import_command(
        ImportCommand::Start {
            review_id: stale_id,
            draft,
        },
        &context,
    );
    assert!(matches!(
        app.import.snapshot(),
        ImportWorkflowSnapshot::Review(_)
    ));

    app.apply_import_command(
        ImportCommand::Start {
            review_id: review.review_id,
            draft,
        },
        &context,
    );
    assert!(matches!(
        app.import.snapshot(),
        ImportWorkflowSnapshot::Importing(_)
    ));
    assert!(app.import.pending_review.is_none());

    let token = app
        .application
        .snapshot()
        .active_operations()
        .iter()
        .find(|token| token.kind() == OperationKind::Import)
        .unwrap()
        .clone();
    app.import.workers.shutdown();
    assert!(app.complete_background_operation(token, OperationCompletion::Cancelled));
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn import_cancellation_waits_for_the_worker_terminal_result() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let source = write_source_time_series_fixture(temp.path()).unwrap();
    let inspection = mirante4d_import_pipeline::inspect_tiff(TiffSource::auto(&source)).unwrap();
    let (review_id, options) = reviewed_import_options(
        TiffSource::auto(&source),
        inspection,
        temp.path().join("cancelled-import.m4d"),
    );
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    assert!(app.start_import_task(review_id, options));
    let token = app
        .application
        .snapshot()
        .active_operations()
        .iter()
        .find(|token| token.kind() == OperationKind::Import)
        .unwrap()
        .clone();

    app.apply_import_command(ImportCommand::CancelImport, &egui::Context::default());

    assert!(matches!(
        app.import.workers.status(),
        ImportWorkerStatus::Importing {
            cancellation_requested: true,
            ..
        }
    ));
    assert!(
        app.application
            .snapshot()
            .active_operations()
            .contains(&token)
    );
    app.import.workers.shutdown();
    assert!(app.complete_background_operation(token, OperationCompletion::Cancelled));
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

    assert!(
        !app.open_or_queue_dataset_path(imported.clone(), None)
            .unwrap()
    );

    assert_eq!(
        app.pending_dataset_open
            .as_ref()
            .map(current_source_open_service::CurrentSourceOpenRequest::selected_path),
        Some(imported.as_path())
    );
    assert!(app.egui_ui.close_prompt_open);
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
fn invalid_external_open_does_not_close_the_current_project_store() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    app.source_open_service = Some(current_source_open_service::CurrentSourceOpenService::new());
    install_test_project_store(&mut app);
    assert!(!app.project_dirty());
    let original_path = app.dataset.selected_path().canonicalize().unwrap();
    let original_lifecycle = app.project_store.as_ref().unwrap().status().lifecycle();

    assert!(
        app.open_or_queue_dataset_path(temp.path().join("missing-external.m4d"), None)
            .unwrap()
    );
    assert!(app.pending_dataset_open.is_none());
    assert_eq!(
        app.dataset_open_project_close,
        DatasetOpenProjectCloseState::NotRequested
    );
    assert_eq!(
        app.project_store.as_ref().unwrap().status().lifecycle(),
        original_lifecycle
    );
    assert!(
        app.source_open_service
            .as_ref()
            .unwrap()
            .active_token()
            .is_some()
    );

    wait_for_test_app(&mut app, |app| {
        app.source_open_service
            .as_ref()
            .is_some_and(|service| service.active_token().is_none())
    });
    assert_eq!(
        app.dataset.selected_path().canonicalize().unwrap(),
        original_path
    );
    assert_eq!(
        app.project_store.as_ref().unwrap().status().lifecycle(),
        original_lifecycle
    );

    close_test_project_store(&mut app);
    app.source_open_service.take().unwrap().shutdown().unwrap();
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn imported_publication_waits_for_project_close_then_installs_without_normal_verifier() {
    let temp = tempfile::tempdir().unwrap();
    let current_package = write_target_fixture(temp.path()).unwrap();
    let source = write_source_single_ome_fixture(temp.path()).unwrap();
    let destination = temp.path().join("direct-verified-import.m4d");
    let inspection = mirante4d_import_pipeline::inspect_tiff(TiffSource::auto(&source)).unwrap();
    let (_, options) =
        reviewed_import_options(TiffSource::auto(&source), inspection, destination.clone());
    let published = mirante4d_import_pipeline::import_tiff(
        options,
        &TestImportLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();
    let receipt = published.receipt().clone();

    let opened = open_dataset_and_render_first_frame(&current_package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    app.source_open_service = Some(current_source_open_service::CurrentSourceOpenService::new());
    install_test_project_store(&mut app);
    verify_test_source(&mut app);
    app.apply_application_command(
        ApplicationCommand::AttachVerifiedDataset,
        &egui::Context::default(),
    )
    .unwrap();
    assert!(app.project_dirty());
    let diagnostics_before = app
        .source_verification_service
        .as_ref()
        .unwrap()
        .diagnostics();

    assert!(!app.open_or_queue_imported_dataset(published, None).unwrap());
    assert!(matches!(
        app.pending_dataset_open.as_ref(),
        Some(current_source_open_service::CurrentSourceOpenRequest::ImportedVerified(_))
    ));
    assert_eq!(
        app.dataset_open_project_close,
        DatasetOpenProjectCloseState::NotRequested
    );
    app.egui_ui.close_prompt_open = false;
    app.close_project_store_before_pending_dataset_open(None)
        .unwrap();
    assert_eq!(
        app.dataset_open_project_close,
        DatasetOpenProjectCloseState::Waiting
    );
    assert!(matches!(
        app.pending_dataset_open.as_ref(),
        Some(current_source_open_service::CurrentSourceOpenRequest::ImportedVerified(_))
    ));
    assert!(
        app.source_open_service
            .as_ref()
            .unwrap()
            .active_token()
            .is_none(),
        "the imported capability must remain undispatched until project close succeeds"
    );
    assert_eq!(
        app.project_store.as_ref().unwrap().status().lifecycle(),
        ProjectStoreLifecycle::Closing
    );

    wait_for_test_app(&mut app, |app| {
        app.dataset.selected_path().canonicalize().unwrap() == destination.canonicalize().unwrap()
            && matches!(
                app.application.snapshot().source(),
                SourceVerificationSnapshot::Verified(_)
            )
            && app.pending_dataset_open.is_none()
            && app
                .source_open_service
                .as_ref()
                .is_some_and(|service| service.active_token().is_none())
    });
    let snapshot = app.application.snapshot();
    assert_eq!(
        snapshot
            .catalog()
            .scientific_identity()
            .verified_id()
            .copied(),
        Some(receipt.scientific_content_id)
    );

    let verification = app.source_verification_service.as_ref().unwrap();
    assert_eq!(verification.diagnostics(), diagnostics_before);
    assert!(verification.active_token().is_none());
    assert!(app.pending_automatic_source_verification.is_none());

    if app.project_store.is_some() {
        close_test_project_store(&mut app);
    }
    app.source_open_service.take().unwrap().shutdown().unwrap();
    app.source_verification_service
        .take()
        .unwrap()
        .shutdown()
        .unwrap();
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn failed_project_close_retains_imported_authority_without_verifier_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let current_package = write_target_fixture(temp.path()).unwrap();
    let source = write_source_single_ome_fixture(temp.path()).unwrap();
    let destination = temp.path().join("retained-after-close-failure.m4d");
    let inspection = mirante4d_import_pipeline::inspect_tiff(TiffSource::auto(&source)).unwrap();
    let (_, options) =
        reviewed_import_options(TiffSource::auto(&source), inspection, destination.clone());
    let published = mirante4d_import_pipeline::import_tiff(
        options,
        &TestImportLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();

    let opened = open_dataset_and_render_first_frame(&current_package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    app.source_open_service = Some(current_source_open_service::CurrentSourceOpenService::new());
    install_test_project_store(&mut app);
    verify_test_source(&mut app);
    app.apply_application_command(
        ApplicationCommand::AttachVerifiedDataset,
        &egui::Context::default(),
    )
    .unwrap();
    assert!(app.project_dirty());
    let original_path = app.dataset.selected_path().canonicalize().unwrap();
    let verification_before = app
        .source_verification_service
        .as_ref()
        .unwrap()
        .diagnostics();

    assert!(!app.open_or_queue_imported_dataset(published, None).unwrap());
    app.egui_ui.close_prompt_open = false;
    app.close_project_store_before_pending_dataset_open(None)
        .unwrap();
    assert_eq!(
        app.dataset_open_project_close,
        DatasetOpenProjectCloseState::Waiting
    );
    assert!(
        app.source_open_service
            .as_ref()
            .unwrap()
            .active_token()
            .is_none()
    );

    app.handle_project_store_event(ProjectStoreServiceEvent::Closed {
        request_id: ProjectStoreRequestId::new(1).unwrap(),
        result: Err(ProjectStoreFault::Corruption {
            stage: "test_project_close_failure",
        }),
    });

    assert_eq!(
        app.dataset_open_project_close,
        DatasetOpenProjectCloseState::Failed
    );
    assert!(app.egui_ui.close_prompt_open);
    assert!(
        app.project_status_message
            .as_deref()
            .is_some_and(|message| message.contains("verified import handoff remains retained"))
    );
    assert!(matches!(
        app.pending_dataset_open.as_ref(),
        Some(current_source_open_service::CurrentSourceOpenRequest::ImportedVerified(
            transfer
        )) if transfer.destination() == destination
    ));
    assert_eq!(
        app.dataset.selected_path().canonicalize().unwrap(),
        original_path
    );
    assert!(
        app.application
            .snapshot()
            .active_operations()
            .iter()
            .all(|operation| operation.kind() != OperationKind::DatasetOpen)
    );
    assert!(
        app.source_open_service
            .as_ref()
            .unwrap()
            .active_token()
            .is_none()
    );
    let verification = app.source_verification_service.as_ref().unwrap();
    assert_eq!(verification.diagnostics(), verification_before);
    assert!(verification.active_token().is_none());
    assert!(
        app.open_or_queue_dataset_path(temp.path().join("must-not-replace-retained.m4d"), None)
            .is_err()
    );
    assert!(matches!(
        app.pending_dataset_open.as_ref(),
        Some(current_source_open_service::CurrentSourceOpenRequest::ImportedVerified(_))
    ));

    app.egui_ui.close_prompt_open = false;
    assert!(
        app.close_project_store_before_pending_dataset_open(None)
            .is_err()
    );
    assert!(app.egui_ui.close_prompt_open);
    app.cancel_pending_dataset_open();
    assert!(app.pending_dataset_open.is_none());
    assert_eq!(
        app.dataset_open_project_close,
        DatasetOpenProjectCloseState::NotRequested
    );

    app.source_open_service.take().unwrap().shutdown().unwrap();
    app.source_verification_service
        .take()
        .unwrap()
        .shutdown()
        .unwrap();
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn imported_transfer_drift_fails_closed_without_external_open_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let current_package = write_target_fixture(temp.path()).unwrap();
    let source = write_source_single_ome_fixture(temp.path()).unwrap();
    let destination = temp.path().join("drifted-import.m4d");
    let inspection = mirante4d_import_pipeline::inspect_tiff(TiffSource::auto(&source)).unwrap();
    let (_, options) =
        reviewed_import_options(TiffSource::auto(&source), inspection, destination.clone());
    let published = mirante4d_import_pipeline::import_tiff(
        options,
        &TestImportLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();
    fs::write(
        destination.join("unlisted-after-publication.bin"),
        b"foreign",
    )
    .unwrap();

    let opened = open_dataset_and_render_first_frame(&current_package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    app.source_open_service = Some(current_source_open_service::CurrentSourceOpenService::new());
    install_test_project_store(&mut app);
    app.source_verification_service =
        Some(current_source_verification_service::CurrentSourceVerificationService::new());
    let original_path = app.dataset.selected_path().canonicalize().unwrap();

    assert!(app.open_or_queue_imported_dataset(published, None).unwrap());
    assert_eq!(
        app.dataset_open_project_close,
        DatasetOpenProjectCloseState::Waiting
    );
    wait_for_test_app(&mut app, |app| {
        app.source_open_service
            .as_ref()
            .is_some_and(|service| service.active_token().is_none())
            && app.project_store.is_some()
            && app
                .project_status_message
                .as_deref()
                .is_some_and(|message| {
                    message.contains("current dataset and its project storage remain available")
                })
    });

    assert_eq!(
        app.dataset.selected_path().canonicalize().unwrap(),
        original_path
    );
    assert_eq!(app.application.snapshot().source_generation().get(), 1);
    assert!(app.application.snapshot().active_operations().is_empty());
    assert_eq!(
        app.project_status_message.as_deref(),
        Some(
            "The package was created and remains on disk, but Mirante4D could not complete its verified imported open; the current dataset and its project storage remain available."
        )
    );
    assert_eq!(
        app.dataset_open_project_close,
        DatasetOpenProjectCloseState::NotRequested
    );
    assert_eq!(
        app.project_store.as_ref().unwrap().status().lifecycle(),
        ProjectStoreLifecycle::Unbound
    );
    let verification = app.source_verification_service.as_ref().unwrap();
    assert_eq!(verification.diagnostics(), Default::default());
    assert!(verification.active_token().is_none());

    close_test_project_store(&mut app);
    app.source_open_service.take().unwrap().shutdown().unwrap();
    app.source_verification_service
        .take()
        .unwrap()
        .shutdown()
        .unwrap();
    app.dataset.request_shutdown().unwrap();
}

struct TestImportLease {
    bytes: u64,
}

impl mirante4d_dataset::CpuByteLease for TestImportLease {
    fn category(&self) -> mirante4d_dataset::CpuLedgerCategory {
        mirante4d_dataset::CpuLedgerCategory::ImportWorkingSet
    }

    fn reserved_bytes(&self) -> u64 {
        self.bytes
    }
}

struct TestImportLedger;

impl mirante4d_dataset::CpuByteLedger for TestImportLedger {
    fn try_acquire(
        &self,
        category: mirante4d_dataset::CpuLedgerCategory,
        bytes: u64,
    ) -> Result<Box<dyn mirante4d_dataset::CpuByteLease>, mirante4d_dataset::CpuLedgerError> {
        assert_eq!(
            category,
            mirante4d_dataset::CpuLedgerCategory::ImportWorkingSet
        );
        assert!(bytes > 0);
        Ok(Box::new(TestImportLease { bytes }))
    }
}

#[test]
fn import_verify_analyze_save_and_reopen_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let source = write_source_time_series_fixture(temp.path()).unwrap();
    let source_bytes = fs::read_dir(&source)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), fs::read(entry.path()).unwrap())
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let package = temp.path().join("imported-target.m4d");
    let project_path = temp.path().join("analysis-result.m4dproj");
    let context = egui::Context::default();

    let inspection = mirante4d_import_pipeline::inspect_tiff(TiffSource::auto(&source)).unwrap();
    let (_, options) =
        reviewed_import_options(TiffSource::auto(&source), inspection, package.clone());
    let published = mirante4d_import_pipeline::import_tiff(
        options,
        &TestImportLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();
    // This test intentionally exercises a later independent external open;
    // the direct verified-import handoff is covered separately above.
    drop(published);
    assert!(package.is_dir());
    assert_eq!(
        fs::read_dir(&source)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), fs::read(entry.path()).unwrap())
            })
            .collect::<std::collections::BTreeMap<_, _>>(),
        source_bytes
    );

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
    wait_for_test_app(&mut app, |app| {
        app.analysis_runtime.active_token().is_none()
            && app
                .application
                .snapshot()
                .transient()
                .analysis_tables()
                .len()
                == 1
            && app
                .application
                .snapshot()
                .transient()
                .analysis_plots()
                .len()
                == 1
            && app
                .project_store
                .as_ref()
                .is_some_and(|service| !service.has_pending_work())
    });
    let saved_snapshot = app.application.snapshot();
    let table_id = saved_snapshot.transient().analysis_tables()[0].id();
    let plot_id = saved_snapshot.transient().analysis_plots()[0].id();
    assert_eq!(
        app.analysis_runtime.table(table_id).unwrap().rows().len(),
        3
    );
    assert_eq!(
        app.analysis_runtime.plot(plot_id).unwrap().points().len(),
        3
    );
    assert!(!app.project_dirty());

    app.analysis_runtime.set_roi([0, 0, 0], [2, 2, 2]).unwrap();
    app.start_product_analysis(analysis_product::ProductAnalysisScope::CurrentTimepointBox)
        .unwrap();
    wait_for_test_app(&mut app, |app| {
        app.analysis_runtime.active_token().is_none()
            && app
                .application
                .snapshot()
                .transient()
                .analysis_tables()
                .len()
                == 2
            && app
                .application
                .snapshot()
                .transient()
                .analysis_plots()
                .len()
                == 1
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
    assert_eq!(
        app.analysis_runtime
            .table(box_table_id)
            .unwrap()
            .rows()
            .len(),
        1
    );
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
            && snapshot.transient().analysis_tables().len() == 2
            && snapshot.transient().analysis_plots().len() == 1
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
    assert_eq!(row_counts, vec![1, 3]);
    assert!(
        reopened_snapshot
            .transient()
            .analysis_plots()
            .iter()
            .all(|descriptor| reopened_app
                .analysis_runtime
                .plot(descriptor.id())
                .is_some_and(|plot| plot.points().len() == 3))
    );

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
    assert!(
        app.application
            .snapshot()
            .transient()
            .analysis_tables()
            .is_empty()
    );
    assert!(
        app.application
            .snapshot()
            .transient()
            .analysis_plots()
            .is_empty()
    );

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
    app.source_verification_service =
        Some(current_source_verification_service::CurrentSourceVerificationService::new());
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
        app.drain_brick_results(&context);
        app.update_source_verification_interactive_busy();
        app.pump_application_services();
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

    await_visible_demand_plan(&mut app);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while app
        .dataset
        .dispatcher()
        .diagnostics()
        .unwrap()
        .failed_requests()
        == 0
    {
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
        app.render_coordination
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
    app.source_verification_service =
        Some(current_source_verification_service::CurrentSourceVerificationService::new());
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
    let verified_catalog_identity = app.application.snapshot().catalog().resource_identity();
    assert_eq!(app.dataset.resource_identity(), verified_catalog_identity);
    await_visible_demand_plan(&mut app);
    assert!(app.dataset.retained_leases().required_len() > 0);
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
    assert_eq!(app.dataset.retained_leases().required_len(), 0);
    assert_eq!(app.dataset.retained_leases().retained_len(), 0);
    assert!(app.dataset.renderer_requirements().is_empty());
    for scope in [
        dataset_requests::SCOPE_CURRENT_3D,
        dataset_requests::SCOPE_CURRENT_3D_REFINEMENT,
        dataset_requests::SCOPE_CROSS_SECTION_XY,
        dataset_requests::SCOPE_CROSS_SECTION_XZ,
        dataset_requests::SCOPE_CROSS_SECTION_YZ,
        dataset_requests::SCOPE_PLAYBACK,
    ] {
        assert!(app.dataset.scope_requirements(scope).is_empty());
    }
    assert_eq!(
        app.dataset.resource_identity(),
        app.application.snapshot().catalog().resource_identity()
    );
    assert!(app.dataset.source_quarantined());
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
    let drain_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while app
        .dataset
        .dispatcher()
        .diagnostics()
        .unwrap()
        .pending_completions()
        > 0
    {
        assert!(std::time::Instant::now() < drain_deadline);
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }
    assert_eq!(
        app.dataset
            .dispatcher()
            .diagnostics()
            .unwrap()
            .pending_completions(),
        0
    );
    assert_eq!(app.dataset.retained_leases().required_len(), 0);
    assert_eq!(app.dataset.retained_leases().retained_len(), 0);

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
        app.application.snapshot().catalog().resource_identity()
    );
    assert!(!app.dataset.source_quarantined());
    let restored = await_visible_demand_plan(&mut app);
    assert!(restored.current_plan_installed);
    assert!(app.dataset.retained_leases().required_len() > 0);

    app.dataset.request_shutdown().unwrap();
    app.source_verification_service
        .take()
        .unwrap()
        .shutdown()
        .unwrap();
}

#[test]
fn terminal_idle_source_fault_quarantines_immediately_and_wakes_the_service_pump() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    verify_test_source(&mut app);
    app.pump_application_services();

    assert!(!app.dataset.dispatcher().has_pending_work());
    assert_eq!(app.application.snapshot().pending_event_count(), 0);

    app.record_dataset_fault(&mirante4d_dataset_runtime::RuntimeFault::new(
        mirante4d_dataset_runtime::RuntimeFaultCode::SourceRejected,
    ));

    assert!(app.dataset.source_quarantined());
    assert_eq!(app.dataset.retained_leases().required_len(), 0);
    assert_eq!(
        app.render_coordination.frame_fidelity.completeness,
        FrameCompleteness::Loading
    );
    let snapshot = app.application.snapshot();
    assert!(matches!(
        snapshot.source(),
        SourceVerificationSnapshot::Required
    ));
    assert!(snapshot.pending_event_count() > 0);
    assert!(!app.dataset.dispatcher().has_pending_work());
    let progressive_render_work = workbench_playback_runtime::progressive_render_submission_work(
        &app.dataset,
        &app.native_presentation,
    );
    assert!(workbench_playback_runtime::background_work_active(
        &snapshot,
        &app.import.workers,
        &app.dataset,
        &app.render_coordination,
        &app.native_presentation,
        progressive_render_work.any_required,
    ));

    app.pump_application_services();
    assert_eq!(app.application.snapshot().pending_event_count(), 0);
    assert!(app.dataset.source_quarantined());

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
    app.source_verification_service =
        Some(current_source_verification_service::CurrentSourceVerificationService::new());

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
        ) -> Result<Box<dyn mirante4d_dataset::CpuByteLease>, mirante4d_dataset::CpuLedgerError>
        {
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
            Arc::clone(&blocking_ledger) as Arc<dyn mirante4d_dataset::CpuByteLedger>,
            Arc::clone(app.dataset.local_source().unwrap()),
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
        render_coordination,
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
    app.install_current_source_runtime(current_source_open_service::CurrentSourceRuntimeTransfer {
        dataset,
        render_coordination,
        analysis_runtime,
    });

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
    let context = egui::Context::default();
    loop {
        app.pump_application_services();
        // Mirror the production update order: interactive source demand owns
        // priority, then the verifier resumes as soon as that bounded demand
        // has drained.
        app.drain_brick_results(&context);
        app.update_source_verification_interactive_busy();
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
    assert_eq!(
        app.application.snapshot().source_generation(),
        replacement_generation
    );
    assert_eq!(
        app.dataset.resource_identity(),
        app.application.snapshot().catalog().resource_identity()
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
    await_visible_demand_plan(&mut app);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !app.dataset.scope_complete(dataset_requests::SCOPE_PLAYBACK) {
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
            .all(|key| app.dataset.retained_leases().payload(*key).is_some())
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
    let render = mirante4d_render_api::RenderExtent::new(64, 64).unwrap();
    for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        app.render_coordination
            .record_viewports(panel.presentation_slot(), presentation, render);
    }
    app.apply_application_command(ApplicationCommand::SetPlaybackActive(true), &context)
        .unwrap();

    await_visible_demand_plan(&mut app);

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
            <= diagnostics
                .category_cap_bytes(mirante4d_dataset::CpuLedgerCategory::DecodedResidency,)
    );
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn three_d_and_linked_cross_section_planners_have_independent_signatures() {
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
    let render = mirante4d_render_api::RenderExtent::new(64, 64).unwrap();
    for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        app.render_coordination
            .record_viewports(panel.presentation_slot(), presentation, render);
    }
    await_visible_demand_plan(&mut app);

    let snapshot = app.application.snapshot();
    let cross = application_view(&snapshot).cross_section();
    let moved_cross = mirante4d_domain::CrossSectionView::new(
        mirante4d_domain::WorldPoint3::new(
            cross.center_world().components()[0] + 1.0,
            cross.center_world().components()[1],
            cross.center_world().components()[2],
        )
        .unwrap(),
        cross.orientation(),
        cross.scale_world_per_screen_point(),
        cross.depth_world(),
    )
    .unwrap();
    let three_d_before_cross = app.visible_demand_plan_calls;
    let linked_before_cross = app.cross_section_demand_plan_calls;
    let renders_before_cross = app.product_render_attempts;
    let fidelity_before_cross = (
        app.render_coordination.frame_fidelity.display_freshness,
        app.render_coordination.frame_fidelity.completeness,
        app.render_coordination.frame_fidelity.reason,
    );
    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section: moved_cross,
        },
        &context,
    )
    .unwrap();

    assert_eq!(app.visible_demand_plan_calls, three_d_before_cross);
    assert_eq!(app.cross_section_demand_plan_calls, linked_before_cross + 3);
    assert_eq!(app.product_render_attempts, renders_before_cross);
    assert_eq!(
        (
            app.render_coordination.frame_fidelity.display_freshness,
            app.render_coordination.frame_fidelity.completeness,
            app.render_coordination.frame_fidelity.reason,
        ),
        fidelity_before_cross,
        "cross-section-only motion must preserve the current 3D frame"
    );
    await_visible_demand_plan(&mut app);

    let snapshot = app.application.snapshot();
    let camera = application_view(&snapshot).camera();
    let moved_camera = mirante4d_domain::CameraView::new(
        camera.projection(),
        mirante4d_domain::WorldPoint3::new(
            camera.target().components()[0] + 1.0,
            camera.target().components()[1],
            camera.target().components()[2],
        )
        .unwrap(),
        camera.orientation(),
        camera.orthographic_world_per_screen_point(),
        camera.perspective_focal_length_screen_points(),
        camera.perspective_view_distance_world(),
    )
    .unwrap();
    let three_d_before_camera = app.visible_demand_plan_calls;
    let linked_before_camera = app.cross_section_demand_plan_calls;
    let linked_generations_before_camera = [PanelId::Xy, PanelId::Xz, PanelId::Yz].map(|panel| {
        app.render_coordination
            .surface(panel.presentation_slot())
            .generation()
    });
    app.apply_application_command(ApplicationCommand::SetCamera(moved_camera), &context)
        .unwrap();

    // The tiny fixture may cover the full volume, in which case the 3D
    // planner itself also has a valid O(1) skip. The essential separation is
    // that camera-only motion never calls any linked-panel planner.
    assert!(app.visible_demand_plan_calls <= three_d_before_camera + 1);
    assert_eq!(app.cross_section_demand_plan_calls, linked_before_camera);
    assert_eq!(
        [PanelId::Xy, PanelId::Xz, PanelId::Yz].map(|panel| {
            app.render_coordination
                .surface(panel.presentation_slot())
                .generation()
        }),
        linked_generations_before_camera,
        "camera-only motion must preserve linked-panel presentations"
    );
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn rejected_worker_union_preserves_visible_signature_and_every_installed_scope() {
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
    let render = mirante4d_render_api::RenderExtent::new(64, 64).unwrap();
    for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        app.render_coordination
            .record_viewports(panel.presentation_slot(), presentation, render);
    }
    await_visible_demand_plan(&mut app);

    let snapshot = app.application.snapshot();
    let cross = application_view(&snapshot).cross_section();
    let moved_cross = mirante4d_domain::CrossSectionView::new(
        mirante4d_domain::WorldPoint3::new(
            cross.center_world().components()[0] + 1.0,
            cross.center_world().components()[1],
            cross.center_world().components()[2],
        )
        .unwrap(),
        cross.orientation(),
        cross.scale_world_per_screen_point(),
        cross.depth_world(),
    )
    .unwrap();
    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section: moved_cross,
        },
        &context,
    )
    .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let result = loop {
        if let Some(result) = app.camera_demand_planner.take_result() {
            break result;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "camera-demand worker did not publish the cross-only transaction"
        );
        std::thread::yield_now();
    };
    let pending = app
        .pending_visible_demand_plan
        .clone()
        .expect("the direct worker result still has its pending transaction");
    let mut prepared = result.outcome.unwrap();
    let installed_scopes = [
        dataset_requests::SCOPE_CURRENT_3D,
        dataset_requests::SCOPE_CURRENT_3D_REFINEMENT,
        dataset_requests::SCOPE_PLAYBACK,
        dataset_requests::SCOPE_CROSS_SECTION_XY,
        dataset_requests::SCOPE_CROSS_SECTION_XZ,
        dataset_requests::SCOPE_CROSS_SECTION_YZ,
    ]
    .map(|scope| (scope, app.dataset.scope_requirement_handle(scope)));
    let installed_union = app.dataset.renderer_requirement_handle();
    let installed_signature = app.visible_demand_planning_signature.clone();
    let cancelled_before = app
        .dataset
        .dispatcher()
        .diagnostics()
        .unwrap()
        .cancelled_requests();

    // Preserve the key values but break the Arc identity to emulate a worker
    // result prepared from a superseded retained-union generation.
    prepared.renderer_requirement_update.previous.requirements =
        Arc::from(installed_union.requirements.to_vec());
    let error = app
        .install_prepared_visible_demand(prepared, &pending)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("retained requirement union changed")
    );

    assert_eq!(app.visible_demand_planning_signature, installed_signature);
    for (scope, requirements) in installed_scopes {
        assert!(Arc::ptr_eq(
            &app.dataset.scope_requirement_handle(scope),
            &requirements
        ));
    }
    assert!(Arc::ptr_eq(
        &app.dataset.renderer_requirement_handle().requirements,
        &installed_union.requirements
    ));
    assert_eq!(
        app.dataset
            .dispatcher()
            .diagnostics()
            .unwrap()
            .cancelled_requests(),
        cancelled_before,
        "a late retained-union rejection must not cancel old useful tickets"
    );
    assert!(app.pending_visible_demand_plan.is_some());
    app.dataset.request_shutdown().unwrap();
}
