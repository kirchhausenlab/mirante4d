#[test]
fn unified_source_open_starts_with_no_owned_interactive_payloads() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, temp.path()).unwrap();
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
    let package = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, temp.path()).unwrap();
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
    let package = write_fixture(FixtureKind::BasicU16_16Cube, temp.path()).unwrap();
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
fn terminal_decode_failure_is_stable_until_the_scope_changes() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_fixture(FixtureKind::BasicU16_16Cube, temp.path()).unwrap();
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
fn playback_prefetch_readiness_is_backed_by_retained_accounted_leases() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_fixture(FixtureKind::TimeU16_8Cube3T, temp.path()).unwrap();
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
    let package = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, temp.path()).unwrap();
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
