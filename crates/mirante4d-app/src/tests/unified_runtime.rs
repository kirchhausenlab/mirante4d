use mirante4d_application::PackageIntegrityAuditSnapshot;

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
fn integrity_audit_is_explicit_reports_real_work_and_never_rebinds_the_source() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let source_generation = app.application.snapshot().source_generation();
    let selected_path = app.dataset.selected_path().to_path_buf();

    for _ in 0..32 {
        app.poll_package_integrity_audit_service();
    }
    assert_eq!(
        app.package_integrity_audit_service
            .as_ref()
            .unwrap()
            .diagnostics()
            .started_runs,
        0,
        "ordinary idle polling must not start an audit"
    );

    app.apply_application_command(
        ApplicationCommand::RequestPackageIntegrityAudit,
        &egui::Context::default(),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        app.poll_package_integrity_audit_service();
        if !matches!(
            app.application.snapshot().source().integrity_audit(),
            PackageIntegrityAuditSnapshot::Running { .. }
        ) {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }

    let snapshot = app.application.snapshot();
    let PackageIntegrityAuditSnapshot::SelfConsistent(report) =
        snapshot.source().integrity_audit()
    else {
        panic!("explicit fixture audit did not complete as self-consistent");
    };
    assert!(report.objects_hashed() > 0);
    assert!(report.bytes_hashed() > 0);
    assert!(report.decoded_bricks() > 0);
    assert!(report.decoded_bytes() > 0);
    assert_eq!(snapshot.source_generation(), source_generation);
    assert_eq!(app.dataset.selected_path(), selected_path);
    let diagnostics = app
        .package_integrity_audit_service
        .as_ref()
        .unwrap()
        .diagnostics();
    assert_eq!(diagnostics.started_runs, 1);
    assert_eq!(diagnostics.completed_runs, 1);
    assert!(diagnostics.progress_updates > 0);

    app.package_integrity_audit_service
        .take()
        .unwrap()
        .shutdown()
        .unwrap();
    app.dataset.request_shutdown().unwrap();
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
fn latest_transient_cross_section_stays_provisional_and_finishes_without_replanning_when_exact() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    let initial = app.application.snapshot();
    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section: *initial.view().cross_section(),
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
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while ![
        dataset_requests::SCOPE_CURRENT_3D,
        dataset_requests::SCOPE_CROSS_SECTION_XY,
        dataset_requests::SCOPE_CROSS_SECTION_XZ,
        dataset_requests::SCOPE_CROSS_SECTION_YZ,
    ]
    .into_iter()
    .all(|scope| app.dataset.scope_complete(scope))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "initial production demand did not become resident"
        );
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }

    let durable_snapshot = app.application.snapshot();
    let durable_view = application_view(&durable_snapshot).clone();
    let active_scale = durable_snapshot
        .catalog()
        .layer(durable_view.active_layer())
        .unwrap()
        .scale(ScaleLevel::BASE)
        .unwrap();
    let first_center = active_scale
        .grid_to_world()
        .transform_point(mirante4d_domain::WorldPoint3::new(64.0, 64.0, 64.0).unwrap())
        .unwrap();
    let latest_center = active_scale
        .grid_to_world()
        .transform_point(mirante4d_domain::WorldPoint3::new(32.0, 32.0, 64.0).unwrap())
        .unwrap();
    let make_cross = |center| {
        mirante4d_domain::CrossSectionView::new(
            center,
            mirante4d_domain::UnitQuaternion::identity(),
            durable_view.cross_section().scale_world_per_screen_point(),
            durable_view.cross_section().depth_world(),
        )
        .unwrap()
    };
    let first = make_cross(first_center);
    let latest = mirante4d_domain::CrossSectionView::new(
        latest_center,
        mirante4d_domain::UnitQuaternion::identity(),
        durable_view.cross_section().scale_world_per_screen_point() * 0.1,
        durable_view.cross_section().depth_world(),
    )
    .unwrap();
    let planned_body = |panel, cross_section| {
        let view = mirante4d_project_model::ViewState::new(
            durable_view.layers().to_vec(),
            durable_view.active_layer(),
            durable_view.timepoint(),
            *durable_view.camera(),
            durable_view.layout(),
            cross_section,
            *durable_view.iso_light(),
        )
        .unwrap();
        let planning = dataset_demand_plan::plan_cross_section_panel_cancellable(
            durable_snapshot.catalog(),
            &view,
            panel,
            presentation,
            render,
            dataset_demand_plan::DatasetDemandPlanLimits::new(
                mirante4d_render_api::MAX_RENDER_REQUIREMENTS,
                mirante4d_render_api::MAX_RENDER_REQUIREMENTS,
                u64::MAX,
            ),
            None,
            || false,
        )
        .unwrap()
        .expect("the uncancelled production plane planner returns a plan");
        let primary_resources =
            planning.plan.resources[..planning.plan.primary_resource_count].to_vec();
        let mut resources = planning.plan.resources;
        if !primary_resources.is_empty() {
            let floor = dataset_demand_plan::plan_cross_section_navigation_floor_cancellable(
                durable_snapshot.catalog(),
                &view,
                dataset_demand_plan::DatasetDemandPlanLimits::new(
                    mirante4d_render_api::MAX_RENDER_REQUIREMENTS,
                    mirante4d_render_api::MAX_RENDER_REQUIREMENTS,
                    u64::MAX,
                ),
                None,
                &mut || false,
            )
            .unwrap()
            .expect("the uncancelled linked navigation-floor planner returns a plan");
            resources.extend(floor.plan.resources);
        }
        resources.sort_unstable();
        resources.dedup();
        (resources, primary_resources)
    };
    let panels = [PanelId::Xy, PanelId::Xz, PanelId::Yz];
    let first_plans = panels.map(|panel| planned_body(panel, first));
    let latest_plans = panels.map(|panel| planned_body(panel, latest));
    let first_primary_resources = &first_plans[0].1;
    let latest_primary_resources = &latest_plans[0].1;
    assert!(first_plans.iter().all(|(body, _)| !body.is_empty()));
    assert!(latest_plans.iter().all(|(body, _)| !body.is_empty()));
    assert!(!first_primary_resources.is_empty());
    assert!(!latest_primary_resources.is_empty());
    assert!(
        first_plans
            .iter()
            .zip(&latest_plans)
            .any(|((first, _), (latest, _))| first != latest),
        "the two samples must cross a linked-panel semantic-brick boundary"
    );

    let cross_scopes = [
        dataset_requests::SCOPE_CROSS_SECTION_XY,
        dataset_requests::SCOPE_CROSS_SECTION_XZ,
        dataset_requests::SCOPE_CROSS_SECTION_YZ,
    ];
    let baseline_bodies =
        cross_scopes.map(|scope| app.dataset.scope_requirement_handle(scope));
    let cold_latest_key = latest_primary_resources
        .iter()
        .copied()
        .find(|key| app.dataset.retained_leases().payload(*key).is_some())
        .expect("the initially complete production union retains a visible latest-plane key");
    assert!(
        app.dataset
            .retire_cpu_payload_for_foreground_test(cold_latest_key),
        "the forced cold key must retire an actual retained lease"
    );

    let plans_before = app.cross_section_demand_plan_calls;
    let currentness_before = app.application.snapshot().currentness();
    let durable_commits_before = app
        .render_coordination
        .display_generation()
        .durable_gesture_commits;

    let diagnostics_before = app.dataset.visible_demand_diagnostics();
    let submitted_before = app.dataset.dispatcher().submitted_request_records().len();
    let (first_result_blocked, release_first_result) =
        app.dataset.block_next_visible_demand_result_publication();

    app.apply_render_intent_interaction(
        RenderIntentInteraction::Sample(mirante4d_application::RenderIntentSample::cross_section(
            mirante4d_application::CrossSectionPanelId::Xy,
            mirante4d_application::RenderGestureKind::Drag,
            first,
        )),
        &context,
    )
    .unwrap();
    first_result_blocked
        .recv_timeout(Duration::from_secs(5))
        .expect("intent N reaches the production planner publication boundary");
    app.apply_render_intent_interaction(
        RenderIntentInteraction::Sample(mirante4d_application::RenderIntentSample::cross_section(
            mirante4d_application::CrossSectionPanelId::Xy,
            mirante4d_application::RenderGestureKind::Drag,
            latest,
        )),
        &context,
    )
    .unwrap();
    release_first_result.send(()).unwrap();

    assert_eq!(
        *app.application.snapshot().view().cross_section(),
        *durable_view.cross_section()
    );
    assert_eq!(app.application.snapshot().currentness(), currentness_before);
    assert_eq!(
        app.render_coordination
            .display_generation()
            .durable_gesture_commits,
        durable_commits_before
    );
    let intent_base = RenderIntentBase::from_snapshot(&app.application.snapshot());
    assert_eq!(
        app.render_intent_mailbox
            .effective_cross_section(intent_base, *durable_view.cross_section()),
        latest
    );
    for (scope, baseline) in cross_scopes.into_iter().zip(&baseline_bodies) {
        assert!(
            Arc::ptr_eq(baseline, &app.dataset.scope_requirement_handle(scope)),
            "neither stale nor latest worker output may install before publication"
        );
    }

    let installed = await_visible_demand_plan(&mut app);
    assert!(installed.cross_section_plan_installed);
    let installed_bodies =
        cross_scopes.map(|scope| app.dataset.scope_requirement_handle(scope));
    for index in 0..3 {
        assert_eq!(
            installed_bodies[index].as_ref(),
            latest_plans[index].0.as_slice(),
            "the latest linked-panel body must install"
        );
        assert!(
            !app
                .visible_demand_plan_currentness()
                .cross_section(panels[index]),
            "an active transient worker body is not durable exact demand"
        );
    }
    let submitted = &app.dataset.dispatcher().submitted_request_records()[submitted_before..];
    assert!(
        submitted.iter().any(|(scope, key, priority)| {
            *scope == dataset_requests::SCOPE_CROSS_SECTION_XY
                && *key == cold_latest_key
                && *priority == mirante4d_dataset_runtime::RequestPriority::CurrentView
        }),
        "the active plane's actually missing key must enter the runtime at foreground priority"
    );
    assert_eq!(
        submitted
            .first()
            .map(|(scope, _, priority)| (*scope, *priority)),
        Some((
            dataset_requests::SCOPE_CROSS_SECTION_XY,
            mirante4d_dataset_runtime::RequestPriority::CurrentView,
        )),
        "the active linked scope must be the first real admission"
    );
    let diagnostics_after = app.dataset.visible_demand_diagnostics();
    assert_eq!(
        diagnostics_after.completed - diagnostics_before.completed,
        1,
        "only the latest planner result may publish"
    );
    assert!(
        diagnostics_after.stale_results_suppressed > diagnostics_before.stale_results_suppressed
    );
    assert_eq!(
        app.cross_section_demand_plan_calls,
        plans_before + 6,
        "each of the two latest-only transactions plans all three linked panels"
    );
    assert!(
        app.render_coordination.refresh_requested(),
        "an already-resident or empty cross-only install must still wake display work"
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !cross_scopes
        .into_iter()
        .all(|scope| app.dataset.scope_complete(scope))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "latest linked planes did not become ready"
        );
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }
    assert!(app.visible_demand_renderability().cross_sections);
    assert!(!app.visible_demand_plan_currentness().cross_sections);

    app.apply_render_intent_interaction(
        RenderIntentInteraction::Finish(RenderIntentTarget::CrossSection(
            mirante4d_application::CrossSectionPanelId::Xy,
        )),
        &context,
    )
    .unwrap();

    assert_eq!(*app.application.snapshot().view().cross_section(), latest);
    assert_eq!(
        app.render_coordination
            .display_generation()
            .durable_gesture_commits,
        durable_commits_before + 1
    );
    assert_eq!(
        app.cross_section_demand_plan_calls,
        plans_before + 6,
        "finish must promote the already-current linked transaction without replanning"
    );
    assert!(app.pending_visible_demand_plan.is_none());
    assert!(app.visible_demand_plan_currentness().cross_sections);
    for (scope, installed) in cross_scopes.into_iter().zip(&installed_bodies) {
        assert!(
            Arc::ptr_eq(installed, &app.dataset.scope_requirement_handle(scope)),
            "durable promotion must retain every latest linked body"
        );
    }

    let outside = make_cross(
        active_scale
            .grid_to_world()
            .transform_point(mirante4d_domain::WorldPoint3::new(32.0, 32.0, -1_000.0).unwrap())
            .unwrap(),
    );
    assert!(
        panels
            .into_iter()
            .all(|panel| planned_body(panel, outside).0.is_empty()),
        "the final phase must exercise exact outside-volume plans for all linked panels"
    );
    let plans_before_empty = app.cross_section_demand_plan_calls;
    app.apply_render_intent_interaction(
        RenderIntentInteraction::Sample(mirante4d_application::RenderIntentSample::cross_section(
            mirante4d_application::CrossSectionPanelId::Xy,
            mirante4d_application::RenderGestureKind::Drag,
            outside,
        )),
        &context,
    )
    .unwrap();
    let provisional_empty = await_visible_demand_plan(&mut app);
    assert!(provisional_empty.cross_section_plan_installed);
    assert!(cross_scopes
        .into_iter()
        .all(|scope| app.dataset.scope_is_empty(scope)));
    assert!(cross_scopes.into_iter().all(|scope| {
        !app.prepared_scope_render_plans.contains_key(&scope)
    }), "an empty exact linked transaction must remove all old prepared render bodies");
    assert!(app.visible_demand_failure_latch.is_none());
    assert!(
        !app.visible_demand_plan_currentness().cross_sections,
        "an active empty transient remains provisional until durable settlement"
    );
    app.apply_render_intent_interaction(
        RenderIntentInteraction::Finish(RenderIntentTarget::CrossSection(
            mirante4d_application::CrossSectionPanelId::Xy,
        )),
        &context,
    )
    .unwrap();
    assert!(app.pending_visible_demand_plan.is_some());
    assert!(await_visible_demand_plan(&mut app).cross_section_plan_installed);
    assert!(panels.into_iter().all(|panel| {
        app.visible_demand_plan_currentness()
            .cross_section(panel)
    }));
    let empty_requirements = app
        .dataset
        .scope_requirement_handle(dataset_requests::SCOPE_CROSS_SECTION_XY);
    let empty_target = app
        .dataset
        .scope_layer_scales(dataset_requests::SCOPE_CROSS_SECTION_XY)
        .and_then(|scales| scales.get(&durable_view.active_layer()))
        .copied();
    let empty_schedule = {
        let dataset = &app.dataset;
        cross_section_scheduler::schedule_cross_section_panel(
            &mut app.render_coordination,
            cross_section_scheduler::CrossSectionScheduleInput {
                view: application_view(&app.application.snapshot()),
                active_layer_target: empty_target,
                requirements: empty_requirements.as_ref(),
                first_useful_requirements: 0,
                first_useful_available: false,
                retained_leases: dataset.retained_leases(),
                all_requirements_available: false,
                dataset_failed: false,
                requirement_visit_counter: None,
            },
            PanelId::Xy,
            true,
        )
        .unwrap()
        .schedule
    };
    assert_eq!(
        empty_schedule.status,
        mirante4d_application::CrossSectionPanelScheduleStatus::Empty
    );
    assert_eq!(
        empty_schedule.reason,
        mirante4d_application::CrossSectionPanelScheduleReason::NoSelectedData
    );
    assert_eq!(app.cross_section_demand_plan_calls, plans_before_empty + 6);
    for _ in 0..16 {
        app.request_visible_bricks();
    }
    assert!(app.pending_visible_demand_plan.is_none());
    assert_eq!(
        app.cross_section_demand_plan_calls,
        plans_before_empty + 6,
        "an installed empty linked transaction is terminal and must not retry planning"
    );
    app.cancel_render_intent();
    assert!(
        app.render_intent_mailbox
            .snapshot()
            .active_gesture
            .is_none()
    );
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn resident_cross_section_guard_avoids_planning_until_settlement_or_envelope_exit() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    let initial = app.application.snapshot();
    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section: *initial.view().cross_section(),
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

    let cross_scopes = [
        dataset_requests::SCOPE_CROSS_SECTION_XY,
        dataset_requests::SCOPE_CROSS_SECTION_XZ,
        dataset_requests::SCOPE_CROSS_SECTION_YZ,
    ];
    let panels = [PanelId::Xy, PanelId::Xz, PanelId::Yz];
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !cross_scopes
        .into_iter()
        .all(|scope| app.dataset.scope_all_resources_complete(scope))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the complete immutable plane-guard bodies did not become resident"
        );
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }
    assert!(cross_scopes.into_iter().all(|scope| {
        app.prepared_scope_render_plans
            .get(&scope)
            .is_some_and(|plan| plan.plane_reuse_envelope.is_some())
    }));
    assert!(
        app.resident_plane_guard_body_installs >= 3,
        "all three linked panels must install body-derived guard proofs"
    );

    let installed_bodies = cross_scopes.map(|scope| app.dataset.scope_requirement_handle(scope));
    let plans_before = app.cross_section_demand_plan_calls;
    let async_before = app.resident_plane_async_plan_submissions;
    let planner_before = app.dataset.visible_demand_diagnostics();
    let submitted_bricks_before = app.dataset.dispatcher().submitted_request_records().len();
    let admission_visits_before = app.dataset.admission_requirement_visits();
    let reuses_before = app.resident_plane_guard_reuses;

    let initial_cross_section = *application_view(&app.application.snapshot()).cross_section();
    let mut slice =
        mirante4d_application::viewport_interaction::CrossSectionViewState::from_canonical(
            initial_cross_section,
        );
    let mut sliced_cross_section = initial_cross_section;
    for sample in 1..=8 {
        slice.slice_by_world_distance(
            mirante4d_application::viewport_interaction::CrossSectionPanel::Xy,
            0.01,
        );
        sliced_cross_section = slice.into_canonical().unwrap();
        let refreshes_before_sample = app.refresh_frame_calls;
        app.apply_render_intent_interaction(
            RenderIntentInteraction::Sample(
                mirante4d_application::RenderIntentSample::cross_section(
                    mirante4d_application::CrossSectionPanelId::Xy,
                    mirante4d_application::RenderGestureKind::Scroll,
                    sliced_cross_section,
                ),
            ),
            &context,
        )
        .unwrap();

        assert_eq!(
            app.cross_section_demand_plan_calls, plans_before,
            "resident slice sample {sample} must not enter exact planning"
        );
        assert_eq!(
            app.resident_plane_async_plan_submissions, async_before,
            "resident slice sample {sample} must not submit worker work"
        );
        assert_eq!(
            app.dataset.visible_demand_diagnostics().submitted,
            planner_before.submitted,
            "resident slice sample {sample} must not touch the latest-only worker"
        );
        assert!(
            app.pending_visible_demand_plan.is_none(),
            "resident slice sample {sample} must finish in the same UI turn"
        );
        assert_eq!(
            app.dataset.admission_requirement_visits(),
            admission_visits_before,
            "resident slice sample {sample} must preserve the proven-complete admission cursor"
        );
        assert_eq!(
            app.refresh_frame_calls,
            refreshes_before_sample + 1,
            "resident slice sample {sample} must attempt its display refresh in the same turn"
        );
        assert!(!app.visible_demand_plan_currentness().cross_sections);
        assert!(app.visible_demand_renderability().cross_sections);
    }
    let generations_before_durable_slice = [PanelId::Xy, PanelId::Xz, PanelId::Yz].map(|panel| {
        app.render_coordination
            .surface(panel.presentation_slot())
            .generation()
    });
    app.apply_render_intent_interaction(
        RenderIntentInteraction::Finish(RenderIntentTarget::CrossSection(
            mirante4d_application::CrossSectionPanelId::Xy,
        )),
        &context,
    )
    .unwrap();
    assert_eq!(
        *application_view(&app.application.snapshot()).cross_section(),
        sliced_cross_section
    );
    assert_eq!(
        app.cross_section_demand_plan_calls, plans_before,
        "the durable slice commit must promote all three installed panels"
    );
    assert_eq!(app.resident_plane_async_plan_submissions, async_before);
    assert_eq!(
        [PanelId::Xy, PanelId::Xz, PanelId::Yz].map(|panel| {
            app.render_coordination
                .surface(panel.presentation_slot())
                .generation()
        }),
        generations_before_durable_slice.map(|generation| generation.saturating_add(1)),
        "the durable command owns exactly one linked-panel invalidation; guard promotion must not manufacture another generation"
    );
    assert_eq!(
        app.resident_plane_guard_reuses,
        reuses_before + 27,
        "each of eight samples reuses three linked guards, followed by one three-panel durable promotion"
    );
    for (scope, installed) in cross_scopes.into_iter().zip(installed_bodies.iter()) {
        assert!(
            Arc::ptr_eq(installed, &app.dataset.scope_requirement_handle(scope)),
            "durable promotion must preserve each immutable panel body"
        );
    }

    let mut rotation =
        mirante4d_application::viewport_interaction::CrossSectionViewState::from_canonical(
            sliced_cross_section,
        );
    let mut rotated_cross_section = sliced_cross_section;
    for sample in 1..=8 {
        rotation.rotate_oblique_by_panel_drag(
            mirante4d_application::viewport_interaction::CrossSectionPanel::Xy,
            0.2,
            0.1,
            0.005,
        );
        rotated_cross_section = rotation.into_canonical().unwrap();
        let refreshes_before_sample = app.refresh_frame_calls;
        app.apply_render_intent_interaction(
            RenderIntentInteraction::Sample(
                mirante4d_application::RenderIntentSample::cross_section(
                    mirante4d_application::CrossSectionPanelId::Xy,
                    mirante4d_application::RenderGestureKind::Drag,
                    rotated_cross_section,
                ),
            ),
            &context,
        )
        .unwrap();

        assert_eq!(
            app.cross_section_demand_plan_calls, plans_before,
            "resident rotation sample {sample} must not enter exact planning"
        );
        assert_eq!(
            app.resident_plane_async_plan_submissions, async_before,
            "resident rotation sample {sample} must not submit worker work"
        );
        assert_eq!(
            app.dataset.visible_demand_diagnostics().submitted,
            planner_before.submitted,
            "resident rotation sample {sample} must not touch the latest-only worker"
        );
        assert!(
            app.pending_visible_demand_plan.is_none(),
            "resident rotation sample {sample} must finish in the same UI turn"
        );
        assert_eq!(
            app.dataset.admission_requirement_visits(),
            admission_visits_before,
            "resident rotation sample {sample} must preserve the proven-complete admission cursor"
        );
        assert_eq!(
            app.refresh_frame_calls,
            refreshes_before_sample + 1,
            "resident rotation sample {sample} must attempt its display refresh in the same turn"
        );
        assert!(!app.visible_demand_plan_currentness().cross_sections);
        assert!(app.visible_demand_renderability().cross_sections);
    }
    app.apply_render_intent_interaction(
        RenderIntentInteraction::Finish(RenderIntentTarget::CrossSection(
            mirante4d_application::CrossSectionPanelId::Xy,
        )),
        &context,
    )
    .unwrap();
    assert_eq!(
        *application_view(&app.application.snapshot()).cross_section(),
        rotated_cross_section
    );
    assert_eq!(app.cross_section_demand_plan_calls, plans_before);
    assert_eq!(app.resident_plane_async_plan_submissions, async_before);
    assert_eq!(
        app.resident_plane_guard_reuses,
        reuses_before + 54,
        "both short target phases reuse the linked guards and perform one durable promotion each"
    );
    let planner_after = app.dataset.visible_demand_diagnostics();
    assert_eq!(planner_after.submitted, planner_before.submitted);
    assert_eq!(planner_after.completed, planner_before.completed);
    assert_eq!(
        planner_after.completed_candidates_visited,
        planner_before.completed_candidates_visited
    );
    assert_eq!(
        app.dataset.dispatcher().submitted_request_records().len(),
        submitted_bricks_before,
        "resident navigation must perform neither planning nor dataset I/O"
    );
    for (scope, installed) in cross_scopes.into_iter().zip(installed_bodies.iter()) {
        assert!(Arc::ptr_eq(
            installed,
            &app.dataset.scope_requirement_handle(scope)
        ));
    }

    let [center_x, center_y, center_z] = rotated_cross_section.center_world().components();
    let changed_lod = mirante4d_domain::CrossSectionView::new(
        mirante4d_domain::WorldPoint3::new(center_x, center_y, center_z).unwrap(),
        rotated_cross_section.orientation(),
        rotated_cross_section.scale_world_per_screen_point() * 100.0,
        rotated_cross_section.depth_world(),
    )
    .unwrap();
    app.apply_render_intent_interaction(
        RenderIntentInteraction::Sample(mirante4d_application::RenderIntentSample::cross_section(
            mirante4d_application::CrossSectionPanelId::Xy,
            mirante4d_application::RenderGestureKind::Drag,
            changed_lod,
        )),
        &context,
    )
    .unwrap();
    assert_eq!(
        app.cross_section_demand_plan_calls,
        plans_before + 3,
        "leaving the resident envelope must plan one coherent provisional linked transaction"
    );
    assert_eq!(app.resident_plane_async_plan_submissions, async_before + 3);
    assert_eq!(
        app.dataset.visible_demand_diagnostics().submitted,
        planner_before.submitted + 1
    );
    if app.pending_visible_demand_plan.is_some() {
        for (scope, installed) in cross_scopes.into_iter().zip(&installed_bodies) {
            assert!(Arc::ptr_eq(
                installed,
                &app.dataset.scope_requirement_handle(scope)
            ));
        }
        let installed = await_visible_demand_plan(&mut app);
        assert!(installed.cross_section_plan_installed);
    } else {
        assert_eq!(
            app.dataset.visible_demand_diagnostics().completed,
            planner_before.completed + 1,
            "a worker allowed to finish in the initiating UI turn must publish exactly one result"
        );
    }
    assert!(panels.into_iter().all(|panel| {
        !app
            .visible_demand_plan_currentness()
            .cross_section(panel)
    }));
    assert!(app.resident_cross_section_coverage.is_some());
    assert_eq!(
        app.dataset.visible_demand_diagnostics().completed,
        planner_before.completed + 1
    );
    app.cancel_render_intent();
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn linked_rotation_crosses_repeated_guard_windows_with_current_fallback_and_latest_only_settlement()
{
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    let initial = app.application.snapshot();
    let initial_cross = *initial.view().cross_section();
    let focused_cross = mirante4d_domain::CrossSectionView::new(
        initial_cross.center_world(),
        initial_cross.orientation(),
        initial_cross.scale_world_per_screen_point() * 0.05,
        initial_cross.depth_world(),
    )
    .unwrap();
    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section: focused_cross,
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
    let scopes = [
        dataset_requests::SCOPE_CROSS_SECTION_XY,
        dataset_requests::SCOPE_CROSS_SECTION_XZ,
        dataset_requests::SCOPE_CROSS_SECTION_YZ,
    ];
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !scopes
        .into_iter()
        .all(|scope| app.dataset.scope_all_resources_complete(scope))
    {
        assert!(std::time::Instant::now() < deadline);
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }
    // The compact package happens to fit its complete fine volume inside one
    // guard. Remove only that optimization proof to model an exhausted guard;
    // the retained navigation floor and every production scheduling path stay
    // unchanged.
    for scope in scopes {
        app.prepared_scope_render_plans
            .get_mut(&scope)
            .unwrap()
            .plane_reuse_envelope = None;
    }

    let base = *application_view(&app.application.snapshot()).cross_section();
    let submitted_before = app.dataset.visible_demand_diagnostics().submitted;
    let plans_before = app.cross_section_demand_plan_calls;
    let mut latest = base;
    for angle in [0.6_f64, 1.2, 1.8, 2.4] {
        let half = angle * 0.5;
        latest = mirante4d_domain::CrossSectionView::new(
            base.center_world(),
            mirante4d_domain::UnitQuaternion::new_xyzw(
                half.sin(),
                0.0,
                0.0,
                half.cos(),
            )
            .unwrap(),
            base.scale_world_per_screen_point(),
            base.depth_world(),
        )
        .unwrap();
        app.apply_render_intent_interaction(
            RenderIntentInteraction::Sample(
                mirante4d_application::RenderIntentSample::cross_section(
                    mirante4d_application::CrossSectionPanelId::Xy,
                    mirante4d_application::RenderGestureKind::Drag,
                    latest,
                ),
            ),
            &context,
        )
        .unwrap();
        assert!(
            app.visible_demand_renderability().cross_sections,
            "every geometry sample must remain immediately renderable after crossing a guard"
        );
        assert!(
            app.resident_cross_section_coverage.is_some(),
            "current fallback coverage must remain explicit while the rolling body is replaced"
        );
    }
    assert!(
        app.cross_section_demand_plan_calls >= plans_before + 3,
        "crossing the old finite envelope must request one coherent linked replacement",
    );
    assert!(
        app.dataset.visible_demand_diagnostics().submitted > submitted_before,
        "the rolling window must use the latest-only worker"
    );
    assert_eq!(
        app.dataset.visible_demand_diagnostics().maximum_pending_requests,
        1
    );

    assert!(await_visible_demand_plan(&mut app).cross_section_plan_installed);
    let intent_base = RenderIntentBase::from_snapshot(&app.application.snapshot());
    assert_eq!(
        app.render_intent_mailbox.effective_cross_section(
            intent_base,
            *application_view(&app.application.snapshot()).cross_section(),
        ),
        latest,
        "only the latest large-rotation geometry may remain publishable"
    );
    assert!(app.visible_demand_renderability().cross_sections);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !scopes
        .into_iter()
        .all(|scope| app.dataset.scope_all_resources_complete(scope))
    {
        assert!(std::time::Instant::now() < deadline);
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }
    app.apply_render_intent_interaction(
        RenderIntentInteraction::Finish(RenderIntentTarget::CrossSection(
            mirante4d_application::CrossSectionPanelId::Xy,
        )),
        &context,
    )
    .unwrap();
    if app.pending_visible_demand_plan.is_some() {
        await_visible_demand_plan(&mut app);
    }
    assert_eq!(
        *application_view(&app.application.snapshot()).cross_section(),
        latest
    );
    assert!(app.visible_demand_plan_currentness().cross_sections);
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn many_small_linked_zoom_steps_and_cold_zoom_out_settle_to_independent_targets() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    let initial = app.application.snapshot();
    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section: *initial.view().cross_section(),
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
    let durable_view = application_view(&snapshot);
    let base_cross_section = *durable_view.cross_section();
    let cross_section_at_scale = |scale| {
        mirante4d_domain::CrossSectionView::new(
            base_cross_section.center_world(),
            base_cross_section.orientation(),
            scale,
            base_cross_section.depth_world(),
        )
        .unwrap()
    };
    let projected_xy = |cross_section| {
        let view = mirante4d_project_model::ViewState::new(
            durable_view.layers().to_vec(),
            durable_view.active_layer(),
            durable_view.timepoint(),
            *durable_view.camera(),
            durable_view.layout(),
            cross_section,
            *durable_view.iso_light(),
        )
        .unwrap();
        dataset_demand_plan::cross_section_projected_layer_scales(
            snapshot.catalog(),
            &view,
            PanelId::Xy,
            presentation,
            render,
        )
        .unwrap()
    };
    let active_layer = durable_view.active_layer();
    let samples = (-256..=256)
        .map(|step| {
            let scale =
                base_cross_section.scale_world_per_screen_point() * 2.0_f64.powf(step as f64 / 32.0);
            let cross_section = cross_section_at_scale(scale);
            (cross_section, projected_xy(cross_section))
        })
        .collect::<Vec<_>>();
    let (fine_cross_section, fine_scales, coarse_cross_section, coarse_scales) = samples
        .windows(2)
        .find_map(|pair| {
            let (fine_cross_section, fine_scales) = &pair[0];
            let (coarse_cross_section, coarse_scales) = &pair[1];
            (fine_scales.get(&active_layer)? < coarse_scales.get(&active_layer)?).then(|| {
                (
                    *fine_cross_section,
                    fine_scales.clone(),
                    *coarse_cross_section,
                    coarse_scales.clone(),
                )
            })
        })
        .expect("the multiscale fixture must expose a linked-plane LOD boundary");

    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section: coarse_cross_section,
        },
        &context,
    )
    .unwrap();
    assert!(await_visible_demand_plan(&mut app).cross_section_plan_installed);
    let cross_scopes = [
        dataset_requests::SCOPE_CROSS_SECTION_XY,
        dataset_requests::SCOPE_CROSS_SECTION_XZ,
        dataset_requests::SCOPE_CROSS_SECTION_YZ,
    ];
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !cross_scopes
        .into_iter()
        .all(|scope| app.dataset.scope_all_resources_complete(scope))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the coarse linked guard did not become resident"
        );
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }
    assert_eq!(
        app.dataset
            .scope_layer_scales(dataset_requests::SCOPE_CROSS_SECTION_XY),
        Some(&coarse_scales)
    );

    let exact_signature_before = app.visible_demand_planning_signature.clone();
    let three_d_body_before = app
        .dataset
        .scope_requirement_handle(dataset_requests::SCOPE_CURRENT_3D);
    let plans_before = app.cross_section_demand_plan_calls;
    let submissions_before = app.dataset.visible_demand_diagnostics().submitted;
    const STEPS: usize = 24;
    let coarse_scale = coarse_cross_section.scale_world_per_screen_point();
    let fine_scale = fine_cross_section.scale_world_per_screen_point();
    for step in 1..=STEPS {
        let fraction = step as f64 / STEPS as f64;
        let scale = coarse_scale * (fine_scale / coarse_scale).powf(fraction);
        let cross_section = cross_section_at_scale(scale);
        app.apply_render_intent_interaction(
            RenderIntentInteraction::Sample(
                mirante4d_application::RenderIntentSample::cross_section(
                    mirante4d_application::CrossSectionPanelId::Xy,
                    mirante4d_application::RenderGestureKind::Scroll,
                    cross_section,
                ),
            ),
            &context,
        )
        .unwrap();
        assert_eq!(
            app.cross_section_demand_plan_calls, plans_before,
            "contained sample {step} must reuse resident coverage without planning"
        );
        assert_eq!(
            app.visible_demand_planning_signature,
            exact_signature_before,
            "resident coverage must never rewrite installed exact-demand identity"
        );
        assert!(
            !app.visible_demand_plan_currentness().cross_sections,
            "the coarser installed body is not exact for sample {step}"
        );
        assert!(
            app.visible_demand_renderability().cross_sections,
            "the complete resident guard must remain renderable for sample {step}"
        );
    }

    app.apply_render_intent_interaction(
        RenderIntentInteraction::Finish(RenderIntentTarget::CrossSection(
            mirante4d_application::CrossSectionPanelId::Xy,
        )),
        &context,
    )
    .unwrap();
    assert_eq!(
        *application_view(&app.application.snapshot()).cross_section(),
        fine_cross_section
    );
    assert_eq!(
        app.cross_section_demand_plan_calls,
        plans_before + 3,
        "settlement must submit one exact linked transaction"
    );
    assert_eq!(
        app.dataset.visible_demand_diagnostics().submitted,
        submissions_before + 1
    );
    assert!(
        !app.visible_demand_plan_currentness().cross_sections,
        "a queued replacement cannot be reported exact before installation"
    );

    assert!(await_visible_demand_plan(&mut app).cross_section_plan_installed);
    assert!(app.visible_demand_plan_currentness().cross_sections);
    assert!(app.resident_cross_section_coverage.is_none());
    assert_eq!(
        app.dataset
            .scope_layer_scales(dataset_requests::SCOPE_CROSS_SECTION_XY),
        Some(&fine_scales),
        "the final installed body must match the independently projected durable target"
    );
    assert!(Arc::ptr_eq(
        &three_d_body_before,
        &app
            .dataset
            .scope_requirement_handle(dataset_requests::SCOPE_CURRENT_3D),
    ));

    let recovery_cross_section = cross_section_at_scale(coarse_scale * 100.0);
    let recovery_scales = projected_xy(recovery_cross_section);
    assert_ne!(
        recovery_scales, fine_scales,
        "the recovery phase must cross a projected LOD boundary"
    );
    let plans_before_recovery = app.cross_section_demand_plan_calls;
    let submissions_before_recovery = app.dataset.visible_demand_diagnostics().submitted;
    app.apply_render_intent_interaction(
        RenderIntentInteraction::Sample(mirante4d_application::RenderIntentSample::cross_section(
            mirante4d_application::CrossSectionPanelId::Xy,
            mirante4d_application::RenderGestureKind::Scroll,
            recovery_cross_section,
        )),
        &context,
    )
    .unwrap();
    assert!(
        app.pending_visible_demand_plan.is_some(),
        "leaving the fine guard must submit one provisional linked transaction"
    );
    assert_eq!(
        app.cross_section_demand_plan_calls,
        plans_before_recovery + 3
    );
    assert!(await_visible_demand_plan(&mut app).cross_section_plan_installed);
    assert!(
        !app.visible_demand_plan_currentness().cross_sections,
        "a transient worker body cannot become durable exact demand"
    );
    assert!(app.resident_cross_section_coverage.is_some());
    assert_eq!(
        app.dataset
            .scope_layer_scales(dataset_requests::SCOPE_CROSS_SECTION_XY),
        Some(&recovery_scales),
        "a replacement outside the old guard must pursue the latest projected LOD directly"
    );

    app.apply_render_intent_interaction(
        RenderIntentInteraction::Finish(RenderIntentTarget::CrossSection(
            mirante4d_application::CrossSectionPanelId::Xy,
        )),
        &context,
    )
    .unwrap();
    assert_eq!(
        app.cross_section_demand_plan_calls,
        plans_before_recovery + 3,
        "durable recovery must promote the already-selected latest linked body"
    );
    assert_eq!(
        app.dataset.visible_demand_diagnostics().submitted,
        submissions_before_recovery + 1
    );
    assert!(app.pending_visible_demand_plan.is_none());
    assert!(app.visible_demand_plan_currentness().cross_sections);
    assert!(app.resident_cross_section_coverage.is_none());
    assert_eq!(
        app.dataset
            .scope_layer_scales(dataset_requests::SCOPE_CROSS_SECTION_XY),
        Some(&recovery_scales),
        "zoom-out settlement must replace the provisional finer body with the independently projected coarser target"
    );
    assert!(Arc::ptr_eq(
        &three_d_body_before,
        &app
            .dataset
            .scope_requirement_handle(dataset_requests::SCOPE_CURRENT_3D),
    ));
    app.dataset.request_shutdown().unwrap();
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrustedLinkedCapture {
    rgba8: Vec<u8>,
    coverage: Vec<u8>,
    validity: Vec<u8>,
}

fn collect_linked_scope_leases(
    app: &MiranteWorkbenchApp,
) -> [Vec<Arc<dyn mirante4d_dataset::ResourceLease>>; 3] {
    [
        dataset_requests::SCOPE_CROSS_SECTION_XY,
        dataset_requests::SCOPE_CROSS_SECTION_XZ,
        dataset_requests::SCOPE_CROSS_SECTION_YZ,
    ]
    .map(|scope| {
        app.prepared_scope_render_plans[&scope]
            .requirements
            .body()
            .canonical()
            .iter()
            .map(|key| {
                app.dataset
                    .retained_leases()
                    .lease_handle(*key)
                    .expect("the complete pre-render linked body retains every CPU oracle lease")
            })
            .collect()
    })
}

fn linked_reference_frames(
    app: &MiranteWorkbenchApp,
    cross_section: mirante4d_domain::CrossSectionView,
    leases: &[Vec<Arc<dyn mirante4d_dataset::ResourceLease>>; 3],
) -> [mirante4d_render_reference::ReferenceFrame; 3] {
    let snapshot = app.application.snapshot();
    let frame = app.render_intent_mailbox.snapshot().latest_revision;
    [PanelId::Xy, PanelId::Xz, PanelId::Yz].map(|panel| {
        let surface = app.render_coordination.surface(panel.presentation_slot());
        let intent = product_render_intent::cross_section_intent(
            &snapshot,
            frame,
            panel,
            cross_section,
            surface.presentation_viewport().unwrap(),
            surface.render_viewport().unwrap(),
        )
        .unwrap()
        .expect("the fixture has visible linked layers");
        let panel_index = match panel {
            PanelId::Xy => 0,
            PanelId::Xz => 1,
            PanelId::Yz => 2,
            PanelId::ThreeD => unreachable!(),
        };
        let lease_refs = leases[panel_index]
            .iter()
            .map(Arc::as_ref)
            .collect::<Vec<_>>();
        mirante4d_render_reference::ReferenceRenderer::new()
            .render(snapshot.catalog(), &intent, &lease_refs)
            .expect("the independent CPU renderer accepts the exact source leases")
    })
}

fn await_linked_validation_captures(
    app: &mut MiranteWorkbenchApp,
    expected_frame: mirante4d_render_api::FrameIdentity,
) -> [TrustedLinkedCapture; 3] {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut last_capture_error = None;
    loop {
        if std::time::Instant::now() >= deadline {
            let product = app.native_presentation.product_gpu.as_ref().unwrap();
            let pending_targets = product
                .pending_validation_captures
                .iter()
                .map(|pending| pending.ticket.target())
                .collect::<Vec<_>>();
            let completed_frames = [PanelId::Xy, PanelId::Xz, PanelId::Yz].map(|panel| {
                product
                    .completed_validation_capture(panel.presentation_slot())
                    .map(|completed| {
                        (
                            completed.ticket.frame(),
                            completed.frame.frame(),
                            completed.ticket.texture_revision().get(),
                        )
                    })
            });
            let presented_frames = [PanelId::Xy, PanelId::Xz, PanelId::Yz].map(|panel| {
                app.render_coordination
                    .surface(panel.presentation_slot())
                    .presented_frame()
                    .map(|frame| frame.frame())
            });
            panic!(
                "linked GPU captures did not become current: expected={expected_frame:?} \
                 pending={pending_targets:?} completed={completed_frames:?} \
                 presented={presented_frames:?} recorded={:?} last_error={last_capture_error:?}",
                product.last_coordinated_recorded_targets,
            );
        }
        let capture_pending = app
            .native_presentation
            .product_gpu
            .as_ref()
            .is_some_and(|product| !product.pending_validation_captures.is_empty());
        if capture_pending {
            if let Err(error) = app.poll_product_validation_captures() {
                last_capture_error = Some(error.to_string());
                assert!(
                    error
                        .to_string()
                        .contains("stale frame generation"),
                    "linked validation readback polling failed: {error}"
                );
            }
        } else {
            app.rerender_coordinated_display_state()
                .expect("the linked product frame renders");
        }
        let captures =
            [PanelId::Xy, PanelId::Xz, PanelId::Yz].map(|panel| {
                product_automation::product_target_capture(app, panel).and_then(|capture| {
                    app.render_coordination
                        .surface(panel.presentation_slot())
                        .presented_frame()
                        .is_some_and(|frame| frame.frame() == expected_frame)
                        .then(|| TrustedLinkedCapture {
                            rgba8: capture.rgba8().to_vec(),
                            coverage: capture.coverage().to_vec(),
                            validity: capture.validity().to_vec(),
                        })
                })
            });
        if let [Some(xy), Some(xz), Some(yz)] = captures {
            return [xy, xz, yz];
        }
        std::thread::yield_now();
    }
}

fn retire_completed_linked_validation_captures(app: &mut MiranteWorkbenchApp) {
    let product = app
        .native_presentation
        .product_gpu
        .as_mut()
        .expect("the trusted linked fixture owns a product renderer");
    assert!(
        product.pending_validation_captures.is_empty(),
        "a predecessor readback must complete before the test retires its observation"
    );
    for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        product.clear_validation_capture(panel.presentation_slot());
    }
}

fn drain_pending_linked_validation_captures(app: &mut MiranteWorkbenchApp) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while app
        .native_presentation
        .product_gpu
        .as_ref()
        .is_some_and(|product| !product.pending_validation_captures.is_empty())
    {
        assert!(
            std::time::Instant::now() < deadline,
            "linked validation readbacks did not retire"
        );
        if let Err(error) = app.poll_product_validation_captures() {
            assert!(
                error
                    .to_string()
                    .contains("stale frame generation"),
                "linked validation readback retirement failed: {error}"
            );
        }
        std::thread::yield_now();
    }
}

fn assert_linked_captures_match_reference(
    captures: &[TrustedLinkedCapture; 3],
    reference: &[mirante4d_render_reference::ReferenceFrame; 3],
) {
    for (panel, (capture, reference)) in [PanelId::Xy, PanelId::Xz, PanelId::Yz]
        .into_iter()
        .zip(captures.iter().zip(reference))
    {
        assert_eq!(
            capture.coverage,
            reference.coverage(),
            "{} coverage differs from the CPU oracle",
            panel.label()
        );
        assert_eq!(
            capture.validity,
            reference.validity(),
            "{} validity differs from the CPU oracle",
            panel.label()
        );
        let max_delta = capture
            .rgba8
            .iter()
            .zip(reference.rgba8())
            .map(|(actual, expected)| actual.abs_diff(*expected))
            .max()
            .unwrap_or(0);
        assert!(
            max_delta <= 1,
            "{} GPU/reference RGBA8 delta was {max_delta}",
            panel.label()
        );
    }
}

#[test]
#[ignore = "requires a trusted Vulkan GPU"]
fn incremental_linked_zoom_pixels_match_direct_fine_cpu_oracle_after_one_settlement_plan() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    let mut renderer = test_wgpu_renderer(
        mirante4d_render_wgpu::WgpuRenderRuntimeConfig::new(1024 * 1024 * 1024)
            .unwrap()
            .with_validation_capture(true),
    );
    renderer
        .activate_dataset_generation(app.application.snapshot().catalog())
        .expect("the headless renderer activates the same product dataset");
    app.native_presentation =
        native_presentation::NativePresentationBridge::with_headless_product_renderer(renderer);

    let initial = app.application.snapshot();
    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section: *initial.view().cross_section(),
        },
        &context,
    )
    .unwrap();
    let presentation = PresentationViewport::new(32.0, 32.0).unwrap();
    let render = mirante4d_render_api::RenderExtent::new(32, 32).unwrap();
    for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        app.render_coordination
            .record_viewports(panel.presentation_slot(), presentation, render);
    }
    await_visible_demand_plan(&mut app);

    let snapshot = app.application.snapshot();
    let durable_view = application_view(&snapshot);
    let base = *durable_view.cross_section();
    let at_scale = |scale| {
        mirante4d_domain::CrossSectionView::new(
            base.center_world(),
            base.orientation(),
            scale,
            base.depth_world(),
        )
        .unwrap()
    };
    let projected_xy = |cross_section| {
        let view = mirante4d_project_model::ViewState::new(
            durable_view.layers().to_vec(),
            durable_view.active_layer(),
            durable_view.timepoint(),
            *durable_view.camera(),
            durable_view.layout(),
            cross_section,
            *durable_view.iso_light(),
        )
        .unwrap();
        dataset_demand_plan::cross_section_projected_layer_scales(
            snapshot.catalog(),
            &view,
            PanelId::Xy,
            presentation,
            render,
        )
        .unwrap()
    };
    let active_layer = durable_view.active_layer();
    let boundary_samples = (-256..=256)
        .map(|step| {
            let cross_section = at_scale(
                base.scale_world_per_screen_point() * 2.0_f64.powf(step as f64 / 32.0),
            );
            (cross_section, projected_xy(cross_section))
        })
        .collect::<Vec<_>>();
    let (fine_cross_section, coarse_cross_section) = boundary_samples
        .windows(2)
        .find_map(|pair| {
            (pair[0].1.get(&active_layer)? < pair[1].1.get(&active_layer)?)
                .then_some((pair[0].0, pair[1].0))
        })
        .expect("the trusted multiscale fixture exposes a linked LOD boundary");

    let linked_scopes = [
        dataset_requests::SCOPE_CROSS_SECTION_XY,
        dataset_requests::SCOPE_CROSS_SECTION_XZ,
        dataset_requests::SCOPE_CROSS_SECTION_YZ,
    ];
    let install_and_complete =
        |app: &mut MiranteWorkbenchApp,
         cross_section: mirante4d_domain::CrossSectionView| {
            app.apply_application_command(
                ApplicationCommand::SetLayout {
                    layout: CanonicalViewerLayout::FourPanel,
                    cross_section,
                },
                &context,
            )
            .unwrap();
            assert!(await_visible_demand_plan(app).cross_section_plan_installed);
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !linked_scopes
                .into_iter()
                .all(|scope| app.dataset.scope_all_resources_complete(scope))
            {
                assert!(std::time::Instant::now() < deadline);
                app.drain_brick_results(&context);
                std::thread::yield_now();
            }
        };

    install_and_complete(&mut app, coarse_cross_section);
    let coarse_leases = collect_linked_scope_leases(&app);
    let coarse_at_fine_reference =
        linked_reference_frames(&app, fine_cross_section, &coarse_leases);
    let direct_coarse_frame = app.render_intent_mailbox.snapshot().latest_revision;
    let direct_coarse_capture = await_linked_validation_captures(&mut app, direct_coarse_frame);

    install_and_complete(&mut app, fine_cross_section);
    let fine_leases = collect_linked_scope_leases(&app);
    let fine_reference = linked_reference_frames(&app, fine_cross_section, &fine_leases);
    let direct_fine_frame = app.render_intent_mailbox.snapshot().latest_revision;
    let direct_fine_capture = await_linked_validation_captures(&mut app, direct_fine_frame);
    assert_linked_captures_match_reference(&direct_fine_capture, &fine_reference);

    install_and_complete(&mut app, coarse_cross_section);
    // Force the next transient sample beyond the resident rolling-window
    // optimization so the mapped product path first renders current geometry
    // from the coarse predecessor, then installs a different prepared body
    // without allocating another user-intent revision.
    for scope in linked_scopes {
        app.prepared_scope_render_plans
            .get_mut(&scope)
            .unwrap()
            .plane_reuse_envelope = None;
    }
    let bodies_before_cutover =
        linked_scopes.map(|scope| app.dataset.scope_requirement_handle(scope));
    let (fine_plan_blocked, release_fine_plan) =
        app.dataset.block_next_visible_demand_result_publication();
    app.apply_render_intent_interaction(
        RenderIntentInteraction::Sample(
            mirante4d_application::RenderIntentSample::cross_section(
                mirante4d_application::CrossSectionPanelId::Xy,
                mirante4d_application::RenderGestureKind::Scroll,
                fine_cross_section,
            ),
        ),
        &context,
    )
    .unwrap();
    fine_plan_blocked
        .recv_timeout(Duration::from_secs(5))
        .expect("the successor body reaches the blocked planner publication boundary");
    let cutover_frame = app.render_intent_mailbox.snapshot().latest_revision;
    for (scope, predecessor) in linked_scopes.into_iter().zip(&bodies_before_cutover) {
        assert!(
            Arc::ptr_eq(predecessor, &app.dataset.scope_requirement_handle(scope)),
            "the blocked worker cannot install its successor body early"
        );
    }
    let cutover_provisional_capture =
        await_linked_validation_captures(&mut app, cutover_frame);
    assert_linked_captures_match_reference(
        &cutover_provisional_capture,
        &coarse_at_fine_reference,
    );

    release_fine_plan.send(()).unwrap();
    assert!(await_visible_demand_plan(&mut app).cross_section_plan_installed);
    assert_eq!(
        app.render_intent_mailbox.snapshot().latest_revision,
        cutover_frame,
        "planner completion must retain the active input revision"
    );
    retire_completed_linked_validation_captures(&mut app);
    let color_submissions_before_cutover = app
        .native_presentation
        .product_gpu
        .as_ref()
        .unwrap()
        .total_coordinated_color_submissions;
    app.rerender_coordinated_display_state()
        .expect("the installed same-frame successor renders");
    let product = app.native_presentation.product_gpu.as_ref().unwrap();
    assert!(
        product.total_coordinated_color_submissions > color_submissions_before_cutover,
        "the successor request must produce a real color cutoff; recorded targets were {:?}",
        product.last_coordinated_recorded_targets,
    );
    assert_eq!(
        product.last_coordinated_recorded_targets.as_ref(),
        &[
            PresentationSlot::Xy,
            PresentationSlot::Xz,
            PresentationSlot::Yz,
        ],
        "one successor cutoff must repaint all linked panels"
    );
    assert_eq!(
        product.pending_validation_captures.len(),
        3,
        "every recorded linked target must own a successor readback ticket"
    );
    for pending in &product.pending_validation_captures {
        assert_eq!(
            app.render_coordination
                .surface(pending.ticket.target())
                .presented_frame(),
            Some(&pending.frame),
            "the queued successor readback must describe the installed presentation"
        );
        assert!(
            native_presentation::texture_revision_is_current(
                app.native_presentation
                    .texture_binding_identity(pending.ticket.target()),
                pending.ticket.device_generation().get(),
                pending.ticket.texture_revision().get(),
            ),
            "the queued successor readback must describe the current native binding"
        );
    }
    let mut first_successor_all_exact = true;
    for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        let frame = app
            .render_coordination
            .surface(panel.presentation_slot())
            .presented_frame()
            .expect("the successor cutoff publishes every linked panel");
        assert_eq!(frame.frame(), cutover_frame);
        assert!(
            frame.progress().coverage().is_first_useful(),
            "the installed successor must immediately publish its complete fallback floor"
        );
        first_successor_all_exact &= frame.progress().completeness()
            == mirante4d_render_api::FrameCompleteness::Exact;
    }
    drain_pending_linked_validation_captures(&mut app);
    let cutover_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !linked_scopes
        .into_iter()
        .all(|scope| app.dataset.scope_all_resources_complete(scope))
    {
        assert!(
            std::time::Instant::now() < cutover_deadline,
            "the same-intent successor body did not become complete"
        );
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }
    assert_eq!(
        app.render_intent_mailbox.snapshot().latest_revision,
        cutover_frame,
        "asynchronous body installation must not manufacture another input revision"
    );
    assert!(
        linked_scopes
            .into_iter()
            .zip(&bodies_before_cutover)
            .any(|(scope, predecessor)| {
                predecessor.as_ref()
                    != app.dataset.scope_requirement_handle(scope).as_ref()
        }),
        "the regression must exercise a semantically different requirement set"
    );
    retire_completed_linked_validation_captures(&mut app);
    let exact_submissions_before = app
        .native_presentation
        .product_gpu
        .as_ref()
        .unwrap()
        .total_coordinated_color_submissions;
    app.rerender_coordinated_display_state()
        .expect("the complete same-frame successor renders");
    let product = app.native_presentation.product_gpu.as_ref().unwrap();
    if !first_successor_all_exact {
        assert!(
            product.total_coordinated_color_submissions > exact_submissions_before,
            "complete target availability must repaint a progressive same-frame successor"
        );
        assert_eq!(
            product.last_coordinated_recorded_targets.as_ref(),
            &[
                PresentationSlot::Xy,
                PresentationSlot::Xz,
                PresentationSlot::Yz,
            ],
        );
    }
    for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        let frame = app
            .render_coordination
            .surface(panel.presentation_slot())
            .presented_frame()
            .expect("the exact successor publishes every linked panel");
        assert_eq!(frame.frame(), cutover_frame);
        assert_eq!(
            frame.progress().completeness(),
            mirante4d_render_api::FrameCompleteness::Exact,
        );
    }
    drain_pending_linked_validation_captures(&mut app);
    app.apply_render_intent_interaction(
        RenderIntentInteraction::Finish(RenderIntentTarget::CrossSection(
            mirante4d_application::CrossSectionPanelId::Xy,
        )),
        &context,
    )
    .unwrap();

    install_and_complete(&mut app, coarse_cross_section);
    let three_d_body_before = app
        .dataset
        .scope_requirement_handle(dataset_requests::SCOPE_CURRENT_3D);
    let plans_before = app.cross_section_demand_plan_calls;
    let submissions_before = app.dataset.visible_demand_diagnostics().submitted;
    const STEPS: usize = 24;
    let coarse_scale = coarse_cross_section.scale_world_per_screen_point();
    let fine_scale = fine_cross_section.scale_world_per_screen_point();
    for step in 1..=STEPS {
        let fraction = step as f64 / STEPS as f64;
        let cross_section = at_scale(coarse_scale * (fine_scale / coarse_scale).powf(fraction));
        app.apply_render_intent_interaction(
            RenderIntentInteraction::Sample(
                mirante4d_application::RenderIntentSample::cross_section(
                    mirante4d_application::CrossSectionPanelId::Xy,
                    mirante4d_application::RenderGestureKind::Scroll,
                    cross_section,
                ),
            ),
            &context,
        )
        .unwrap();
    }
    assert_eq!(app.cross_section_demand_plan_calls, plans_before);
    assert!(
        direct_coarse_capture
            .iter()
            .zip(&direct_fine_capture)
            .any(|(coarse, fine)| coarse.rgba8 != fine.rgba8),
        "independent GPU readback must observe the linked coarse-to-fine geometry/body change"
    );
    assert!(!app.visible_demand_plan_currentness().cross_sections);
    let provisional_frame = app.render_intent_mailbox.snapshot().latest_revision;
    let provisional_capture = await_linked_validation_captures(&mut app, provisional_frame);
    assert_linked_captures_match_reference(&provisional_capture, &coarse_at_fine_reference);
    assert!(
        direct_coarse_capture
            .iter()
            .zip(&provisional_capture)
            .any(|(coarse_view, fine_view)| coarse_view.rgba8 != fine_view.rgba8),
        "independent GPU readback must observe the geometry change while the coarse source body is retained"
    );
    assert!(
        provisional_capture
            .iter()
            .zip(&direct_fine_capture)
            .any(|(coarse, fine)| coarse.rgba8 != fine.rgba8),
        "the fixture must independently distinguish the coarse and fine source bodies"
    );

    app.apply_render_intent_interaction(
        RenderIntentInteraction::Finish(RenderIntentTarget::CrossSection(
            mirante4d_application::CrossSectionPanelId::Xy,
        )),
        &context,
    )
    .unwrap();
    assert_eq!(app.cross_section_demand_plan_calls, plans_before + 3);
    assert_eq!(
        app.dataset.visible_demand_diagnostics().submitted,
        submissions_before + 1
    );
    assert!(await_visible_demand_plan(&mut app).cross_section_plan_installed);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !linked_scopes
        .into_iter()
        .all(|scope| app.dataset.scope_all_resources_complete(scope))
    {
        assert!(std::time::Instant::now() < deadline);
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }
    let settled_frame = app.render_intent_mailbox.snapshot().latest_revision;
    let incremental_capture = await_linked_validation_captures(&mut app, settled_frame);
    assert_linked_captures_match_reference(&incremental_capture, &fine_reference);
    assert_eq!(incremental_capture, direct_fine_capture);
    assert!(app.visible_demand_plan_currentness().cross_sections);
    assert!(Arc::ptr_eq(
        &three_d_body_before,
        &app
            .dataset
            .scope_requirement_handle(dataset_requests::SCOPE_CURRENT_3D),
    ));
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn failed_resident_plane_promotion_is_atomic_across_dataset_render_and_signature_state() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    let initial = app.application.snapshot();
    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section: *initial.view().cross_section(),
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
    let scopes = [
        dataset_requests::SCOPE_CROSS_SECTION_XY,
        dataset_requests::SCOPE_CROSS_SECTION_XZ,
        dataset_requests::SCOPE_CROSS_SECTION_YZ,
    ];
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !scopes
        .into_iter()
        .all(|scope| app.dataset.scope_all_resources_complete(scope))
    {
        assert!(std::time::Instant::now() < deadline);
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }

    let stale = mirante4d_render_api::FrameIdentity::new(
        app.render_intent_mailbox
            .snapshot()
            .latest_revision
            .get()
            .saturating_add(1),
    );
    assert!(app.dataset.observe_visible_intent(stale, None));
    let required_before = scopes.map(|scope| app.dataset.scope_required_prefix_len(scope));
    let admitted_before = scopes.map(|scope| app.dataset.scope_admitted_prefix_len(scope));
    let render_before = scopes.map(|scope| {
        app.prepared_scope_render_plans[&scope]
            .requirements
            .prefetch_promoted()
    });
    let bodies_before = scopes.map(|scope| app.dataset.scope_requirement_handle(scope));
    let signature_before = app.visible_demand_planning_signature.clone();
    let reuses_before = app.resident_plane_guard_reuses;

    assert!(
        !app.prepare_resident_durable_cross_sections(),
        "a dataset revision newer than the durable mailbox revision must fail preflight"
    );
    assert_eq!(
        scopes.map(|scope| app.dataset.scope_required_prefix_len(scope)),
        required_before
    );
    assert_eq!(
        scopes.map(|scope| app.dataset.scope_admitted_prefix_len(scope)),
        admitted_before
    );
    assert_eq!(
        scopes.map(|scope| {
            app.prepared_scope_render_plans[&scope]
                .requirements
                .prefetch_promoted()
        }),
        render_before
    );
    assert_eq!(app.visible_demand_planning_signature, signature_before);
    assert_eq!(app.resident_plane_guard_reuses, reuses_before);
    for (scope, before) in scopes.into_iter().zip(bodies_before.iter()) {
        assert!(Arc::ptr_eq(
            before,
            &app.dataset.scope_requirement_handle(scope)
        ));
    }

    while app.render_intent_mailbox.snapshot().latest_revision <= stale {
        app.render_intent_mailbox
            .observe_durable_intent(RenderIntentFamily::Both)
            .unwrap();
    }
    assert!(
        app.dataset
            .scope_required_prefix_len(dataset_requests::SCOPE_CROSS_SECTION_XY)
            < app
                .dataset
                .scope_requirements(dataset_requests::SCOPE_CROSS_SECTION_XY)
                .len()
    );
    app.dataset
        .commit_complete_scope_prefetch_tail(dataset_requests::SCOPE_CROSS_SECTION_XY);
    let required_mismatch = scopes.map(|scope| app.dataset.scope_required_prefix_len(scope));
    let admitted_mismatch = scopes.map(|scope| app.dataset.scope_admitted_prefix_len(scope));
    let render_mismatch = scopes.map(|scope| {
        app.prepared_scope_render_plans[&scope]
            .requirements
            .prefetch_promoted()
    });
    let bodies_mismatch = scopes.map(|scope| app.dataset.scope_requirement_handle(scope));
    let signature_mismatch = app.visible_demand_planning_signature.clone();

    assert!(
        !app.prepare_resident_durable_cross_sections(),
        "a dataset/render prefix mismatch must fail before any panel commits"
    );
    assert_eq!(
        scopes.map(|scope| app.dataset.scope_required_prefix_len(scope)),
        required_mismatch
    );
    assert_eq!(
        scopes.map(|scope| app.dataset.scope_admitted_prefix_len(scope)),
        admitted_mismatch
    );
    assert_eq!(
        scopes.map(|scope| {
            app.prepared_scope_render_plans[&scope]
                .requirements
                .prefetch_promoted()
        }),
        render_mismatch
    );
    assert_eq!(app.visible_demand_planning_signature, signature_mismatch);
    assert_eq!(app.resident_plane_guard_reuses, reuses_before);
    for (scope, before) in scopes.into_iter().zip(bodies_mismatch.iter()) {
        assert!(Arc::ptr_eq(
            before,
            &app.dataset.scope_requirement_handle(scope)
        ));
    }
    app.dataset.request_shutdown().unwrap();
}

#[test]
#[ignore = "requires a trusted Vulkan GPU"]
fn unsafe_3d_profile_keeps_native_preview_visible_until_atomic_exact_strips_finish() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    let mut renderer = test_wgpu_renderer(
        mirante4d_render_wgpu::WgpuRenderRuntimeConfig::new(1024 * 1024 * 1024)
            .unwrap()
            .with_validation_capture(true),
    );
    renderer
        .activate_dataset_generation(app.application.snapshot().catalog())
        .expect("the headless renderer activates the same product dataset");
    app.native_presentation =
        native_presentation::NativePresentationBridge::with_headless_product_renderer(renderer);
    app.volume_presentation
        .set_work_limits_for_test(1_000_000, 100_000);

    let bootstrap_presentation = PresentationViewport::new(32.0, 32.0).unwrap();
    let bootstrap_render = mirante4d_render_api::RenderExtent::new(32, 32).unwrap();
    app.render_coordination.record_viewports(
        PresentationSlot::ThreeD,
        bootstrap_presentation,
        bootstrap_render,
    );
    await_visible_demand_plan(&mut app);
    let bootstrap_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while app
        .render_coordination
        .surface(PresentationSlot::ThreeD)
        .presented_frame()
        .is_none()
    {
        assert!(
            std::time::Instant::now() < bootstrap_deadline,
            "the bootstrap navigation frame did not become visible"
        );
        app.drain_brick_results(&context);
        app.rerender_coordinated_display_state().unwrap();
        std::thread::yield_now();
    }

    let initial = app.application.snapshot();
    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section: *initial.view().cross_section(),
        },
        &context,
    )
    .unwrap();
    let presentation = PresentationViewport::new(640.0, 480.0).unwrap();
    let render = mirante4d_render_api::RenderExtent::new(640, 480).unwrap();
    assert_eq!(
        crate::workbench_ui_output::apply_viewport_observations(
            &mut app.render_coordination,
            &mut app.render_intent_mailbox,
            [PanelId::ThreeD, PanelId::Xy, PanelId::Xz, PanelId::Yz].map(|panel| {
                ViewportObservation::new(panel.presentation_slot(), presentation, render)
            }),
        )
        .unwrap(),
        Some(CoordinatedPresentationGroup::FullLayout)
    );
    await_visible_demand_plan(&mut app);
    assert_eq!(
        app.render_coordination.frame_fidelity.target_scale_level, 0,
        "the fixture and mapped viewport must require exact scale zero"
    );
    let expected_preview_frame = app.render_intent_mailbox.snapshot().three_d_revision;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !app
        .dataset
        .scope_resources_complete(dataset_requests::SCOPE_CURRENT_3D)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the selected exact 3D body did not become CPU-resident"
        );
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }
    while app
        .native_presentation
        .product_gpu
        .as_ref()
        .is_some_and(|product| !product.pending_validation_captures.is_empty())
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the bootstrap validation capture did not retire"
        );
        app.poll_product_validation_captures().unwrap();
        std::thread::yield_now();
    }

    while !(app.render_coordination.frame_fidelity.three_d_preview
        && app
            .render_coordination
            .surface(PresentationSlot::ThreeD)
            .presented_frame()
            .is_some_and(|frame| {
                frame.frame() == expected_preview_frame && frame.extent() == render
            }))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the unsafe current-camera preview did not publish"
        );
        app.drain_brick_results(&context);
        app.rerender_coordinated_display_state().unwrap();
        std::thread::yield_now();
    }
    let preview_frame = app
        .render_coordination
        .surface(PresentationSlot::ThreeD)
        .presented_frame()
        .expect("the native preview is a real current presentation")
        .clone();
    let preview_binding = app
        .native_presentation
        .texture_binding_identity(PresentationSlot::ThreeD);
    assert_eq!(preview_frame.extent(), render);
    assert_eq!(
        app.render_coordination.frame_fidelity.display_freshness,
        DisplayedFrameFreshness::Current
    );
    assert_eq!(
        app.render_coordination
            .frame_fidelity
            .three_d_render_viewport,
        render,
        "an unsafe fine profile must use a coarser body at native output resolution"
    );
    assert!(
        app.native_presentation
            .product_gpu
            .as_ref()
            .is_some_and(|product| {
                product
                    .pending_validation_captures
                    .iter()
                    .all(|pending| {
                        pending.ticket.target()
                            != mirante4d_render_api::PresentationTarget::ThreeD
                    })
            }),
        "a provisional preview must not enter exact validation capture"
    );

    let mut observed_hidden_strip = false;
    let mut linked_input_checked = false;
    while app.render_coordination.frame_fidelity.three_d_preview {
        assert!(
            std::time::Instant::now() < deadline,
            "the native exact 3D candidate did not publish"
        );
        while app.render_coordination.take_refresh_request() {}
        app.drain_brick_results(&context);
        app.rerender_coordinated_display_state().unwrap();
        if app.render_coordination.frame_fidelity.three_d_preview {
            assert!(
                app.render_coordination.take_refresh_request(),
                "an unfinished hidden candidate, including a backpressure deferral, must enqueue its next immediate display transaction"
            );
            let completed = app
                .render_coordination
                .frame_fidelity
                .three_d_refinement_strips_completed;
            let total = app
                .render_coordination
                .frame_fidelity
                .three_d_refinement_strips_total;
            observed_hidden_strip |= completed > 0 && completed < total;
            if !linked_input_checked && completed > 0 && completed < total {
                let before_revision = app.render_intent_mailbox.snapshot().three_d_revision;
                let cross_section = *application_view(&app.application.snapshot()).cross_section();
                app.apply_render_intent_interaction(
                    RenderIntentInteraction::Sample(
                        mirante4d_application::RenderIntentSample::cross_section(
                            mirante4d_application::CrossSectionPanelId::Xy,
                            mirante4d_application::RenderGestureKind::Drag,
                            cross_section,
                        ),
                    ),
                    &context,
                )
                .unwrap();
                assert_eq!(
                    app.render_intent_mailbox.snapshot().three_d_revision,
                    before_revision,
                    "linked-only input must not advance 3D frame identity"
                );
                app.rerender_coordinated_display_state().unwrap();
                assert_eq!(
                    app.render_coordination
                        .frame_fidelity
                        .three_d_refinement_strips_total,
                    total,
                    "linked-only input must not replace the hidden 3D candidate"
                );
                assert!(
                    app.render_coordination
                        .frame_fidelity
                        .three_d_refinement_strips_completed
                        >= completed,
                    "linked-only input must not reset hidden 3D strip progress"
                );
                linked_input_checked = true;
            }
            assert_eq!(
                app.render_coordination
                    .surface(PresentationSlot::ThreeD)
                    .presented_frame(),
                Some(&preview_frame),
                "partial exact strips must retain the complete preview front"
            );
            assert_eq!(
                app.native_presentation
                    .texture_binding_identity(PresentationSlot::ThreeD),
                preview_binding,
                "partial exact strips must not revise the visible texture binding"
            );
        }
        std::thread::yield_now();
    }
    assert!(
        observed_hidden_strip,
        "the forced unsafe exact body must make hidden strip progress"
    );
    assert!(
        linked_input_checked,
        "the hidden candidate must survive a real linked-only input sample"
    );

    let exact = app
        .render_coordination
        .surface(PresentationSlot::ThreeD)
        .presented_frame()
        .expect("the completed exact candidate atomically replaces the preview");
    assert_eq!(exact.extent(), render);
    assert_eq!(
        exact.progress().completeness(),
        mirante4d_render_api::FrameCompleteness::Exact
    );
    assert_eq!(
        app.render_coordination.frame_fidelity.displayed_scale_level,
        Some(0)
    );
    assert_eq!(
        app.render_coordination
            .frame_fidelity
            .three_d_render_viewport,
        render
    );
    assert_eq!(
        app.render_coordination
            .frame_fidelity
            .three_d_refinement_strips_total,
        0
    );
    assert!(
        !app.dataset.staging_current_refinement(),
        "the exact atomic swap must also finish any dataset refinement handoff"
    );
    assert_ne!(
        app.native_presentation
            .texture_binding_identity(PresentationSlot::ThreeD),
        preview_binding,
        "the final exact swap must revise the visible texture binding once"
    );
    assert!(
        app.native_presentation
            .product_gpu
            .as_ref()
            .is_some_and(|product| {
                product
                    .pending_validation_captures
                    .iter()
                    .any(|pending| {
                        pending.ticket.target()
                            == mirante4d_render_api::PresentationTarget::ThreeD
                    })
            }),
        "the final exact cutoff must own the first post-resize validation capture"
    );

    let submissions = app
        .native_presentation
        .product_gpu
        .as_ref()
        .unwrap()
        .total_coordinated_color_submissions;
    app.rerender_coordinated_display_state().unwrap();
    assert_eq!(
        app.native_presentation
            .product_gpu
            .as_ref()
            .unwrap()
            .total_coordinated_color_submissions,
        submissions,
        "settled idle must submit no additional exact strips"
    );
    app.dataset.request_shutdown().unwrap();
}

#[test]
#[ignore = "requires a trusted Vulkan GPU"]
fn exact_transient_cross_section_updates_all_linked_panels_before_finish_and_empty_clears_them() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    let mut renderer = test_wgpu_renderer(
        mirante4d_render_wgpu::WgpuRenderRuntimeConfig::new(1024 * 1024 * 1024).unwrap(),
    );
    renderer
        .activate_dataset_generation(app.application.snapshot().catalog())
        .expect("the headless renderer activates the same product dataset");
    app.native_presentation =
        native_presentation::NativePresentationBridge::with_headless_product_renderer(renderer);
    let initial = app.application.snapshot();
    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section: *initial.view().cross_section(),
        },
        &context,
    )
    .unwrap();
    let presentation = PresentationViewport::new(32.0, 32.0).unwrap();
    let render = mirante4d_render_api::RenderExtent::new(32, 32).unwrap();
    for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        app.render_coordination
            .record_viewports(panel.presentation_slot(), presentation, render);
    }
    await_visible_demand_plan(&mut app);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while ![
        dataset_requests::SCOPE_CROSS_SECTION_XY,
        dataset_requests::SCOPE_CROSS_SECTION_XZ,
        dataset_requests::SCOPE_CROSS_SECTION_YZ,
    ]
    .into_iter()
    .all(|scope| app.dataset.scope_complete(scope))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the initial linked demand did not become resident"
        );
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }

    app.rerender_coordinated_display_state()
        .expect("the initial linked surface must produce a real retained presentation");
    let durable_snapshot = app.application.snapshot();
    let durable_view = application_view(&durable_snapshot).clone();
    let scale = durable_snapshot
        .catalog()
        .layer(durable_view.active_layer())
        .unwrap()
        .scale(ScaleLevel::BASE)
        .unwrap();
    let transient_center = scale
        .grid_to_world()
        .transform_point(mirante4d_domain::WorldPoint3::new(32.0, 32.0, 64.0).unwrap())
        .unwrap();
    let transient_cross = mirante4d_domain::CrossSectionView::new(
        transient_center,
        mirante4d_domain::UnitQuaternion::identity(),
        durable_view.cross_section().scale_world_per_screen_point(),
        durable_view.cross_section().depth_world(),
    )
    .unwrap();
    assert_ne!(transient_cross, *durable_view.cross_section());

    app.apply_render_intent_interaction(
        RenderIntentInteraction::Sample(mirante4d_application::RenderIntentSample::cross_section(
            mirante4d_application::CrossSectionPanelId::Xy,
            mirante4d_application::RenderGestureKind::Drag,
            transient_cross,
        )),
        &context,
    )
    .unwrap();
    await_visible_demand_plan(&mut app);
    let transient_revision = app.render_intent_mailbox.snapshot().latest_revision;
    assert_eq!(
        *application_view(&app.application.snapshot()).cross_section(),
        *durable_view.cross_section(),
        "a raw sample must not mutate durable cross-section geometry"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while ![
        dataset_requests::SCOPE_CROSS_SECTION_XY,
        dataset_requests::SCOPE_CROSS_SECTION_XZ,
        dataset_requests::SCOPE_CROSS_SECTION_YZ,
    ]
    .into_iter()
    .all(|scope| app.dataset.scope_complete(scope))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the transient linked bodies did not become resident"
        );
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }
    let transient_publish_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while ![
        PresentationSlot::Xy,
        PresentationSlot::Xz,
        PresentationSlot::Yz,
    ]
    .into_iter()
    .all(|target| {
        let surface = app.render_coordination.surface(target);
        surface.presented_frame().is_some_and(|presented| {
            presented.frame() == transient_revision
                && presented.progress().completeness()
                    == mirante4d_render_api::FrameCompleteness::Exact
        }) && surface.display_current()
    }) {
        assert!(
            std::time::Instant::now() < transient_publish_deadline,
            "all three linked panels must publish the active transient revision"
        );
        app.rerender_coordinated_display_state()
            .expect("all transient linked planes must reach the real presentation path");
        std::thread::yield_now();
    }

    for target in [
        PresentationSlot::Xy,
        PresentationSlot::Xz,
        PresentationSlot::Yz,
    ] {
        let surface = app.render_coordination.surface(target);
        let presented = surface
            .presented_frame()
            .expect("every linked panel must publish during the active gesture");
        assert_eq!(
            presented.frame(),
            transient_revision,
            "every linked panel must show the same latest transient geometry before release"
        );
        assert_eq!(
            presented.progress().completeness(),
            mirante4d_render_api::FrameCompleteness::Exact
        );
        assert!(surface.display_current());
        assert!(
            app.native_presentation.texture_id(target).is_some(),
            "every linked panel must own visibly present pixels during the gesture"
        );
    }

    let linked_scopes = [
        dataset_requests::SCOPE_CROSS_SECTION_XY,
        dataset_requests::SCOPE_CROSS_SECTION_XZ,
        dataset_requests::SCOPE_CROSS_SECTION_YZ,
    ];
    let linked_targets = [
        PresentationSlot::Xy,
        PresentationSlot::Xz,
        PresentationSlot::Yz,
    ];
    let bodies_before_finish =
        linked_scopes.map(|scope| app.dataset.scope_requirement_handle(scope));
    let generations_before_finish =
        linked_targets.map(|target| app.render_coordination.surface(target).generation());
    let frames_before_finish = linked_targets.map(|target| {
        app.render_coordination
            .surface(target)
            .presented_frame()
            .expect("the transient linked frame remains presented")
            .frame()
    });
    let textures_before_finish =
        linked_targets.map(|target| app.native_presentation.texture_id(target));
    let bindings_before_finish = linked_targets
        .map(|target| app.native_presentation.texture_binding_identity(target));
    let (submissions_before_finish, executions_before_finish) = {
        let product = app.native_presentation.product_gpu.as_ref().unwrap();
        (
            product.renderer.diagnostics().queue_submissions(),
            product.renderer.diagnostics().frames_executed(),
        )
    };
    let plans_before_finish = app.cross_section_demand_plan_calls;
    app.apply_render_intent_interaction(
        RenderIntentInteraction::Finish(RenderIntentTarget::CrossSection(
            mirante4d_application::CrossSectionPanelId::Xy,
        )),
        &context,
    )
    .unwrap();
    let durable_revision = app.render_intent_mailbox.snapshot().latest_revision;
    assert!(
        durable_revision > transient_revision,
        "the durable linked state must own a newer global mailbox revision"
    );
    assert_eq!(
        *application_view(&app.application.snapshot()).cross_section(),
        transient_cross,
        "Finish must first make the exact transient geometry durable"
    );
    assert_eq!(
        app.cross_section_demand_plan_calls, plans_before_finish,
        "settling an already-resident linked gesture must not replan"
    );
    assert!(
        app.pending_visible_demand_plan.is_none(),
        "settling an already-resident linked gesture must not submit worker work"
    );
    for (scope, body) in linked_scopes.into_iter().zip(&bodies_before_finish) {
        assert!(
            Arc::ptr_eq(body, &app.dataset.scope_requirement_handle(scope)),
            "settlement must retain each immutable linked-panel body"
        );
    }
    assert!(linked_targets.into_iter().all(|target| {
        let surface = app.render_coordination.surface(target);
        surface.presented_frame().is_some_and(|frame| {
            frame.frame() == transient_revision
                && frame.progress().completeness()
                == mirante4d_render_api::FrameCompleteness::Exact
        }) && surface.display_current()
            && app.native_presentation.texture_id(target).is_some()
    }), "all linked panels must adopt the exact transient pixels synchronously");
    assert_eq!(
        linked_targets.map(|target| app.render_coordination.surface(target).generation()),
        generations_before_finish.map(|generation| generation.saturating_add(1)),
        "the durable linked commit must advance every panel exactly once"
    );
    assert_eq!(
        linked_targets.map(|target| {
            app.render_coordination
                .surface(target)
                .presented_frame()
                .unwrap()
                .frame()
        }),
        frames_before_finish,
        "durable adoption must retain every exact transient frame"
    );
    assert_eq!(
        linked_targets.map(|target| app.native_presentation.texture_id(target)),
        textures_before_finish,
        "durable adoption must retain every native texture"
    );
    assert_eq!(
        linked_targets
            .map(|target| app.native_presentation.texture_binding_identity(target)),
        bindings_before_finish,
        "durable adoption must retain every renderer texture revision"
    );
    app.rerender_coordinated_display_state()
        .expect("the queued observation after adoption must be a clean no-op");
    {
        let product = app.native_presentation.product_gpu.as_ref().unwrap();
        assert_eq!(
            product.renderer.diagnostics().queue_submissions(),
            submissions_before_finish,
            "durable adoption and its clean observation must submit no GPU work"
        );
        assert_eq!(
            product.renderer.diagnostics().frames_executed(),
            executions_before_finish,
            "durable adoption and its clean observation must execute no renderer frame"
        );
    }
    assert!(
        [PanelId::Xy, PanelId::Xz, PanelId::Yz]
            .into_iter()
            .all(|panel| app
                .visible_demand_plan_currentness()
                .cross_section(panel)),
        "the durable result must remain one coherent three-panel transaction"
    );

    let outside_center = scale
        .grid_to_world()
        .transform_point(mirante4d_domain::WorldPoint3::new(32.0, 32.0, 1_000.0).unwrap())
        .unwrap();
    let outside = mirante4d_domain::CrossSectionView::new(
        outside_center,
        mirante4d_domain::UnitQuaternion::identity(),
        durable_view.cross_section().scale_world_per_screen_point(),
        durable_view.cross_section().depth_world(),
    )
    .unwrap();
    app.apply_render_intent_interaction(
        RenderIntentInteraction::Sample(mirante4d_application::RenderIntentSample::cross_section(
            mirante4d_application::CrossSectionPanelId::Xy,
            mirante4d_application::RenderGestureKind::Drag,
            outside,
        )),
        &context,
    )
    .unwrap();
    await_visible_demand_plan(&mut app);
    assert!(
        linked_scopes
            .into_iter()
            .all(|scope| app.dataset.scope_is_empty(scope)),
        "the exact outside-volume transaction must be empty for all linked panels"
    );
    assert!(
        linked_targets.into_iter().all(|target| {
            app.render_coordination
                .surface(target)
                .presented_frame()
                .is_some()
        }),
        "the empty transition must start with real old pixels to clear in every linked panel"
    );
    app.rerender_coordinated_display_state()
        .expect("exact empty linked planes are useful terminal presentations");
    for target in linked_targets {
        let empty_surface = app.render_coordination.surface(target);
        assert!(
            empty_surface.presented_frame().is_none(),
            "{target:?} retained {:?} with current={} and schedule={:?}",
            empty_surface
                .presented_frame()
                .map(|frame| frame.frame()),
            empty_surface.display_current(),
            empty_surface.cross_section_schedule()
        );
        assert_eq!(
            app.native_presentation.texture_id(target),
            None,
            "an omitted empty linked target must release its native binding"
        );
        assert!(empty_surface.display_current());
        assert_eq!(
            empty_surface
                .cross_section_schedule()
                .map(|schedule| (schedule.status, schedule.reason)),
            Some((
                mirante4d_application::CrossSectionPanelScheduleStatus::Empty,
                mirante4d_application::CrossSectionPanelScheduleReason::NoSelectedData,
            ))
        );
    }

    let (submissions_after_empty, executions_after_empty) = {
        let product = app.native_presentation.product_gpu.as_ref().unwrap();
        (
            product.renderer.diagnostics().queue_submissions(),
            product.renderer.diagnostics().frames_executed(),
        )
    };
    let settled_work =
        workbench_playback_runtime::progressive_render_submission_work(&app.native_presentation);
    assert!(
        !settled_work.any_required,
        "an omitted Empty target must not leave renderer-owned progress to poll"
    );
    {
        let snapshot = app.application.snapshot();
        assert!(
            !workbench_playback_runtime::background_work_active(
                &snapshot,
                &app.import.workers,
                &app.dataset,
                &app.render_coordination,
                &app.native_presentation,
                settled_work.any_required,
            ),
            "a settled Empty presentation must not keep the UI background-work loop alive"
        );
    }

    app.rerender_coordinated_display_state()
        .expect("a repeated settled Empty refresh must remain a no-op");
    let repeated_work =
        workbench_playback_runtime::progressive_render_submission_work(&app.native_presentation);
    assert!(
        !repeated_work.any_required,
        "a repeated Empty refresh must not recreate renderer-owned progress"
    );
    {
        let product = app.native_presentation.product_gpu.as_ref().unwrap();
        assert_eq!(
            product.renderer.diagnostics().queue_submissions(),
            submissions_after_empty,
            "a repeated settled Empty refresh must submit no GPU work"
        );
        assert_eq!(
            product.renderer.diagnostics().frames_executed(),
            executions_after_empty,
            "a repeated settled Empty refresh must execute no renderer frame"
        );
    }
    for target in linked_targets {
        assert!(
            app.render_coordination
                .surface(target)
                .presented_frame()
                .is_none(),
            "a repeated Empty refresh must not resurrect a retired linked semantic frame"
        );
        assert_eq!(
            app.native_presentation.texture_id(target),
            None,
            "a repeated Empty refresh must not recreate a retired linked native binding"
        );
    }

    app.cancel_render_intent();
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn complete_navigation_floor_makes_latest_camera_immediately_renderable() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    let original = *application_view(&app.application.snapshot()).camera();
    let zoomed = mirante4d_domain::CameraView::new(
        original.projection(),
        original.target(),
        original.orientation(),
        original.orthographic_world_per_screen_point() * 0.01,
        original.perspective_focal_length_screen_points(),
        original.perspective_view_distance_world(),
    )
    .unwrap();
    app.apply_application_command(ApplicationCommand::SetCamera(zoomed), &context)
        .unwrap();
    await_visible_demand_plan(&mut app);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !app
        .dataset
        .scope_all_resources_complete(dataset_requests::SCOPE_CURRENT_3D)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the selected body and its full-volume navigation floor did not become complete"
        );
        app.drain_brick_results(&context);
        app.request_visible_bricks();
        std::thread::yield_now();
    }
    assert!(
        !app.navigation_render_plans.is_empty(),
        "a planned 3D view owns a retained navigation ladder"
    );
    let aggregate_body = app.navigation_render_plans[0]
        .requirements
        .body()
        .canonical()
        .clone();
    for candidate in &app.navigation_render_plans {
        assert_eq!(
            candidate.requirements.body().canonical(),
            &aggregate_body,
            "every navigation wrapper must share one terminal-first residency body"
        );
        assert!(
            candidate
                .selection_body
                .canonical()
                .iter()
                .all(|key| key.scale() == candidate.layer_scales[&key.layer()]),
            "every navigation candidate must retain one truthful selection scale per layer"
        );
        assert_eq!(
            candidate.requirements.first_useful_prefix_len(),
            candidate.selection_body.ranked().len(),
            "only the candidate's exact coherent rung may contribute to frame coverage"
        );
        assert_eq!(
            &candidate.requirements.body().ranked()
                [..candidate.requirements.first_useful_prefix_len()],
            candidate.selection_body.ranked().as_ref(),
            "the exact candidate rung must lead its residency wrapper"
        );
        assert!(
            candidate
                .requirements
                .scale_chains()
                .iter()
                .all(|chain| chain.scales().len() == 1),
            "a volume wrapper must expose exactly one render scale per layer"
        );
    }
    assert!(
        app.navigation_render_plans[0]
            .requirements
            .prefetch_resource_count()
            > 0,
        "the terminal wrapper must carry the finer ladder as dormant residency prefetch"
    );

    let [x, y, z] = zoomed.target().components();
    let outside_guard = mirante4d_domain::CameraView::new(
        zoomed.projection(),
        mirante4d_domain::WorldPoint3::new(x + 1_000.0, y, z).unwrap(),
        zoomed.orientation(),
        zoomed.orthographic_world_per_screen_point(),
        zoomed.perspective_focal_length_screen_points(),
        zoomed.perspective_view_distance_world(),
    )
    .unwrap();
    app.apply_render_intent_interaction(
        RenderIntentInteraction::Sample(mirante4d_application::RenderIntentSample::camera(
            mirante4d_application::RenderGestureKind::Drag,
            outside_guard,
        )),
        &context,
    )
    .unwrap();

    let snapshot = app.application.snapshot();
    let base = RenderIntentBase::from_snapshot(&snapshot);
    assert_eq!(*application_view(&snapshot).camera(), zoomed);
    assert_eq!(
        app.render_intent_mailbox
            .effective_camera(base, *application_view(&snapshot).camera()),
        outside_guard
    );
    assert_eq!(
        app.render_intent_mailbox.renderable_camera(base),
        Some(outside_guard),
        "the complete full-volume floor must authorize the latest camera without waiting for refinement"
    );
    assert_eq!(
        app.render_coordination.frame_fidelity.display_freshness,
        mirante4d_application::DisplayedFrameFreshness::Stale
    );
    app.cancel_render_intent();
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn predecessor_timepoint_navigation_floor_cannot_make_a_new_camera_intent_renderable() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    await_visible_demand_plan(&mut app);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !app
        .navigation_render_plans
        .first()
        .is_some_and(|plan| {
            workbench_brick_runtime::first_useful_resources_complete_with_renderer(
                &app.dataset,
                &app.native_presentation,
                plan,
            )
        })
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the predecessor navigation floor did not become resident"
        );
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }
    assert_eq!(
        app.navigation_render_plans[0].requirements.timepoint(),
        TimeIndex::new(0)
    );
    let generation = app
        .render_coordination
        .surface(PresentationSlot::ThreeD)
        .generation();
    assert!(app.render_coordination.record_presented_frame(
        PresentationSlot::ThreeD,
        generation,
        synthetic_presented_frame(
            PresentationSlot::ThreeD,
            app.render_coordination.render_viewport,
        ),
    ));

    app.apply_application_command(
        ApplicationCommand::SetTimepoint(TimeIndex::new(1)),
        &context,
    )
    .unwrap();
    assert_eq!(application_view(&app.application.snapshot()).timepoint(), TimeIndex::new(1));
    assert_eq!(
        app.navigation_render_plans[0].requirements.timepoint(),
        TimeIndex::new(0),
        "the retained predecessor remains paintable while the successor is planned"
    );

    let durable_camera = *application_view(&app.application.snapshot()).camera();
    assert!(
        !app.prepare_resident_camera_intent(durable_camera),
        "resident resources from t0 must not suppress demand for t1 during camera input"
    );

    assert!(await_visible_demand_plan(&mut app).current_plan_installed);
    assert!(
        app.dataset.holding_previous_presentation(),
        "the t0 presentation remains protected while t1 becomes renderable"
    );
    assert_eq!(
        app.navigation_render_plans[0].requirements.timepoint(),
        TimeIndex::new(1),
        "successor navigation metadata must not remain bound to the retained predecessor"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !workbench_brick_runtime::first_useful_resources_complete_with_renderer(
        &app.dataset,
        &app.native_presentation,
        &app.navigation_render_plans[0],
    ) {
        assert!(
            std::time::Instant::now() < deadline,
            "the successor navigation floor did not become resident"
        );
        app.drain_brick_results(&context);
        std::thread::yield_now();
    }
    assert!(
        app.prepare_resident_camera_intent(durable_camera),
        "the ready t1 navigation floor must decouple camera rendering from further demand planning"
    );

    app.dataset.request_shutdown().unwrap();
}

#[test]
fn finishing_camera_gesture_commits_without_allocating_a_second_3d_frame() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    let original = *application_view(&app.application.snapshot()).camera();
    let [x, y, z] = original.target().components();
    let moved = mirante4d_domain::CameraView::new(
        original.projection(),
        mirante4d_domain::WorldPoint3::new(x + 0.25, y, z).unwrap(),
        original.orientation(),
        original.orthographic_world_per_screen_point(),
        original.perspective_focal_length_screen_points(),
        original.perspective_view_distance_world(),
    )
    .unwrap();
    let before = app.render_intent_mailbox.snapshot();

    app.apply_render_intent_interaction(
        RenderIntentInteraction::Sample(mirante4d_application::RenderIntentSample::camera(
            mirante4d_application::RenderGestureKind::Drag,
            moved,
        )),
        &context,
    )
    .unwrap();
    let sampled = app.render_intent_mailbox.snapshot();
    assert_eq!(sampled.three_d_revision.get(), before.latest_revision.get() + 1);
    assert!(sampled.active_gesture.is_some());

    app.apply_render_intent_interaction(
        RenderIntentInteraction::Finish(RenderIntentTarget::ThreeD),
        &context,
    )
    .unwrap();
    let committed = app.render_intent_mailbox.snapshot();
    assert_eq!(
        committed.three_d_revision, sampled.three_d_revision,
        "the durable commit must preserve the gesture's final visible 3D identity"
    );
    assert_eq!(committed.latest_revision, sampled.latest_revision);
    assert!(committed.active_gesture.is_none());
    assert_eq!(*application_view(&app.application.snapshot()).camera(), moved);

    app.dataset.request_shutdown().unwrap();
}

#[test]
fn complete_resident_3d_target_rebinds_directly_without_coarse_staging_or_new_io() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    await_visible_demand_plan(&mut app);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while app.dataset.staging_current_refinement()
        || !app
            .dataset
            .scope_all_resources_complete(dataset_requests::SCOPE_CURRENT_3D)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the selected 3D body did not become complete and promote"
        );
        app.drain_brick_results(&context);
        app.request_visible_bricks();
        std::thread::yield_now();
    }
    await_visible_demand_plan(&mut app);
    assert!(
        app.pending_visible_demand_plan.is_none(),
        "the initial selected body must settle before measuring a resident-only camera change"
    );

    let camera = *application_view(&app.application.snapshot()).camera();
    let [x, y, z] = camera.target().components();
    let contained = mirante4d_domain::CameraView::new(
        camera.projection(),
        mirante4d_domain::WorldPoint3::new(x + 0.01, y, z).unwrap(),
        camera.orientation(),
        camera.orthographic_world_per_screen_point(),
        camera.perspective_focal_length_screen_points(),
        camera.perspective_view_distance_world(),
    )
    .unwrap();
    assert!(
        app.resident_camera_target_body_is_complete(contained),
        "the small same-LOD camera change must be covered by the complete selected body"
    );
    let body_before = app
        .dataset
        .scope_requirement_handle(dataset_requests::SCOPE_CURRENT_3D);
    let requests_before = app.dataset.dispatcher().submitted_request_records().len();
    let plans_before = app.visible_demand_plan_calls;

    app.apply_render_intent_interaction(
        RenderIntentInteraction::Sample(mirante4d_application::RenderIntentSample::camera(
            mirante4d_application::RenderGestureKind::Drag,
            contained,
        )),
        &context,
    )
    .unwrap();
    assert!(app.pending_visible_demand_plan.is_none());
    assert!(Arc::ptr_eq(
        &body_before,
        &app
            .dataset
            .scope_requirement_handle(dataset_requests::SCOPE_CURRENT_3D)
    ));
    assert_eq!(
        app.visible_demand_plan_calls, plans_before,
        "a resident selected 3D body must not enter the planner"
    );
    assert_eq!(
        app.dataset.dispatcher().submitted_request_records().len(),
        requests_before,
        "a resident selected 3D rebind must not enqueue I/O"
    );
    app.cancel_render_intent();
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn outside_transient_camera_uses_navigation_floor_instead_of_fake_empty_publication() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    await_visible_demand_plan(&mut app);
    let navigation_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !app
        .dataset
        .scope_all_resources_complete(dataset_requests::SCOPE_CURRENT_3D)
    {
        assert!(
            std::time::Instant::now() < navigation_deadline,
            "the full-volume navigation floor did not become complete"
        );
        app.drain_brick_results(&context);
        app.request_visible_bricks();
        std::thread::yield_now();
    }

    let snapshot = app.application.snapshot();
    let camera = *application_view(&snapshot).camera();
    let [x, y, z] = camera.target().components();
    let outside = mirante4d_domain::CameraView::new(
        camera.projection(),
        mirante4d_domain::WorldPoint3::new(x + 10_000.0, y, z).unwrap(),
        camera.orientation(),
        camera.orthographic_world_per_screen_point() * 0.01,
        camera.perspective_focal_length_screen_points(),
        camera.perspective_view_distance_world(),
    )
    .unwrap();
    app.render_coordination
        .set_layer_presentations(
            PresentationSlot::ThreeD,
            vec![mirante4d_application::LayerPresentationStatus {
                layer_ordinal: application_view(&snapshot).active_layer().ordinal(),
                expected_scale_level: Some(0),
                displayed_scale_level: Some(0),
                target_scale_level: Some(0),
                finest_fallback_scale_level: None,
                fallback_scale_level: None,
                target_available_requirements: 1,
                target_total_requirements: 1,
                available_requirements: 1,
                total_requirements: 1,
                mixed: false,
                current: true,
            }],
        )
        .unwrap();

    app.apply_render_intent_interaction(
        RenderIntentInteraction::Sample(mirante4d_application::RenderIntentSample::camera(
            mirante4d_application::RenderGestureKind::Drag,
            outside,
        )),
        &context,
    )
    .unwrap();
    await_visible_demand_plan(&mut app);
    assert!(
        !app.dataset
            .scope_is_empty(dataset_requests::SCOPE_CURRENT_3D),
        "an outside camera still owns the complete navigation body needed to render truthful background pixels"
    );
    assert!(app.dataset.current_covers_full_volume());
    let base = RenderIntentBase::from_snapshot(&app.application.snapshot());
    assert_eq!(
        app.render_intent_mailbox.renderable_camera(base),
        Some(outside)
    );
    let generation = app
        .render_coordination
        .display_generation()
        .input_generation;
    assert_ne!(
        app.render_coordination
            .display_generation()
            .current_presentation_generation,
        Some(generation)
    );

    app.rerender_coordinated_display_state().unwrap();

    assert_ne!(
        app.render_coordination.frame_fidelity.backend,
        mirante4d_application::RenderBackend::Empty,
        "nonempty navigation demand must not manufacture a semantic Empty result without rendering pixels"
    );
    assert!(
        !app
            .render_coordination
            .surface(PresentationSlot::ThreeD)
            .display_current(),
        "without a renderer, the latest camera must remain stale rather than being declared current"
    );
    assert!(
        !app.render_coordination
            .surface(PresentationSlot::ThreeD)
            .layer_presentations()
            .is_empty(),
        "the old complete presentation remains visible until real latest-camera pixels replace it"
    );
    assert_ne!(
        app.render_coordination
            .display_generation()
            .current_presentation_generation,
        Some(generation),
        "no synthetic empty publication may complete the latest generation"
    );

    app.cancel_render_intent();
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
    let revision_before_resize = app.render_intent_mailbox.snapshot().latest_revision;
    let presentation = app.render_coordination.presentation_viewport;
    assert_eq!(
        crate::workbench_ui_output::apply_viewport_observations(
            &mut app.render_coordination,
            &mut app.render_intent_mailbox,
            [ViewportObservation::new(
                PresentationSlot::ThreeD,
                presentation,
                resized,
            )],
        )
        .unwrap(),
        Some(CoordinatedPresentationGroup::ThreeD)
    );
    let resize_revision = app.render_intent_mailbox.snapshot().latest_revision;
    assert_eq!(resize_revision.get(), revision_before_resize.get() + 1);
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
    assert_eq!(
        app.render_intent_mailbox.snapshot().latest_revision,
        resize_revision,
        "asynchronous plan completion must install under the viewport revision, not allocate another frame"
    );
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn transfer_only_edit_skips_planning_and_admits_one_display_generation() {
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
    let generation_before = app.render_coordination.display_generation().input_generation;

    app.apply_application_command(ApplicationCommand::SetLayerView(changed_layer), &context)
        .unwrap();

    assert_eq!(app.visible_demand_plan_calls, plans_before);
    assert_eq!(
        app.render_coordination.display_generation().input_generation,
        generation_before + 1
    );
    assert_eq!(
        app.render_coordination.frame_fidelity.display_freshness,
        mirante4d_application::DisplayedFrameFreshness::Stale
    );
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn render_mode_switch_skips_membership_planning_and_admits_one_display_generation() {
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
    let generation_before = app.render_coordination.display_generation().input_generation;

    app.apply_application_command(ApplicationCommand::SetLayerView(changed_layer), &context)
        .unwrap();

    assert_eq!(app.visible_demand_plan_calls, plans_before);
    assert_eq!(
        app.render_coordination.display_generation().input_generation,
        generation_before + 1
    );
    assert_eq!(
        app.render_coordination.frame_fidelity.display_freshness,
        mirante4d_application::DisplayedFrameFreshness::Stale
    );
    app.dataset.request_shutdown().unwrap();
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
        mirante4d_dataset::BrickKey::new(
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

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let diagnostics = app.dataset.dispatcher().diagnostics().unwrap();
        if diagnostics.queued_requests() == 0
            && diagnostics.in_flight_decodes() == 0
            && diagnostics.pending_completions() > 0
            && diagnostics.completed_decodes() == diagnostics.started_decodes()
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the overlap fixture did not reach a stable decoded baseline"
        );
        std::thread::yield_now();
    }
    let decoded_before = app.dataset.dispatcher().diagnostics().unwrap();
    let storage_before = app.dataset.local_source_diagnostics().unwrap();

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

    let decoded_after = app.dataset.dispatcher().diagnostics().unwrap();
    let storage_after = app.dataset.local_source_diagnostics().unwrap();
    assert_eq!(
        decoded_after.submitted_requests(),
        decoded_before.submitted_requests(),
        "a completed overlapping key awaiting application ingestion must submit no duplicate CPU request"
    );
    assert_eq!(
        decoded_after.started_decodes(),
        decoded_before.started_decodes(),
        "a completed overlapping key awaiting application ingestion must start no duplicate decode"
    );
    assert_eq!(
        decoded_after.completed_decodes(),
        decoded_before.completed_decodes(),
        "a completed overlapping key awaiting application ingestion must complete no duplicate decode"
    );
    assert_eq!(
        storage_after.physical_brick_unique_decodes, storage_before.physical_brick_unique_decodes,
        "a completed overlapping key awaiting application ingestion must not reread its physical brick"
    );
    assert_eq!(
        storage_after.reader.codec_decode_operations, storage_before.reader.codec_decode_operations,
        "a completed overlapping key awaiting application ingestion must not repeat codec work"
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

    app.import.begin_setup();
    app.import.set_channel_kind(
        0,
        mirante4d_application::import_workflow::ImportChannelSourceKind::FolderOf3dTiffs,
    );
    app.import.install_channel_selection(0, source.clone());
    let manifest = TiffSource::new(vec![
        TiffChannelSource::folder_of_3d("channel 1", &source).unwrap(),
    ])
    .unwrap();
    app.import
        .workers
        .start_inspection(manifest.clone(), PathBuf::new())
        .unwrap();
    app.import.mark_channel_inspection_active(0);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        app.drain_tiff_import_setup_results(&context);
        if app
            .import
            .setup
            .as_ref()
            .and_then(|setup| setup.channels[0].inspection.as_ref())
            .is_some()
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    let inspection = app.import.validated_setup_inspection().unwrap();
    app.import.setup = None;
    app.import
        .install_review(manifest, inspection, destination)
        .unwrap();

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
    let manifest = TiffSource::new(vec![
        TiffChannelSource::folder_of_3d("channel 1", &source).unwrap(),
    ])
    .unwrap();
    let inspection = mirante4d_import_pipeline::inspect_tiff(manifest.clone()).unwrap();
    let (review_id, options) = reviewed_import_options(
        manifest,
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
    app.apply_application_command(ApplicationCommand::AttachDataset, &context)
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
    app.package_integrity_audit_service
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
    app.source_open_service = Some(current_source_open_service::CurrentSourceOpenService::new(
        app.cpu_broker.clone(),
    ));
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
#[ignore = "developer-local: deep imported-publication/project-close integration"]
fn imported_publication_waits_for_project_close_then_installs_without_an_integrity_audit() {
    let temp = tempfile::tempdir().unwrap();
    let current_package = write_target_fixture(temp.path()).unwrap();
    let source = write_source_single_ome_fixture(temp.path()).unwrap();
    let destination = temp.path().join("direct-import-publication.m4d");
    let inspection = mirante4d_import_pipeline::inspect_tiff(TiffSource::single_3d(&source)).unwrap();
    let (_, options) =
        reviewed_import_options(TiffSource::single_3d(&source), inspection, destination.clone());
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
    app.source_open_service = Some(current_source_open_service::CurrentSourceOpenService::new(
        app.cpu_broker.clone(),
    ));
    install_test_project_store(&mut app);
    app.apply_application_command(
        ApplicationCommand::AttachDataset,
        &egui::Context::default(),
    )
    .unwrap();
    assert!(app.project_dirty());
    let diagnostics_before = app
        .package_integrity_audit_service
        .as_ref()
        .unwrap()
        .diagnostics();

    assert!(!app.open_or_queue_imported_dataset(published, None).unwrap());
    assert!(matches!(
        app.pending_dataset_open.as_ref(),
        Some(current_source_open_service::CurrentSourceOpenRequest::ImportedPublication(_))
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
        Some(current_source_open_service::CurrentSourceOpenRequest::ImportedPublication(_))
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
                app.application.snapshot().source().content_address_origin(),
                mirante4d_application::ContentAddressOrigin::ComputedDuringImport
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
            .content_address_status()
            .content_address()
            .copied(),
        Some(receipt.scientific_content_id)
    );

    let audit = app.package_integrity_audit_service.as_ref().unwrap();
    assert_eq!(audit.diagnostics(), diagnostics_before);
    assert!(audit.active_token().is_none());

    if app.project_store.is_some() {
        close_test_project_store(&mut app);
    }
    app.source_open_service.take().unwrap().shutdown().unwrap();
    app.package_integrity_audit_service
        .take()
        .unwrap()
        .shutdown()
        .unwrap();
    app.dataset.request_shutdown().unwrap();
}

#[test]
#[ignore = "developer-local: deep imported-publication/project-close integration"]
fn failed_project_close_retains_imported_authority_without_an_audit_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let current_package = write_target_fixture(temp.path()).unwrap();
    let source = write_source_single_ome_fixture(temp.path()).unwrap();
    let destination = temp.path().join("retained-after-close-failure.m4d");
    let inspection = mirante4d_import_pipeline::inspect_tiff(TiffSource::single_3d(&source)).unwrap();
    let (_, options) =
        reviewed_import_options(TiffSource::single_3d(&source), inspection, destination.clone());
    let published = mirante4d_import_pipeline::import_tiff(
        options,
        &TestImportLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();

    let opened = open_dataset_and_render_first_frame(&current_package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    app.source_open_service = Some(current_source_open_service::CurrentSourceOpenService::new(
        app.cpu_broker.clone(),
    ));
    install_test_project_store(&mut app);
    app.apply_application_command(
        ApplicationCommand::AttachDataset,
        &egui::Context::default(),
    )
    .unwrap();
    assert!(app.project_dirty());
    let original_path = app.dataset.selected_path().canonicalize().unwrap();
    let audit_before = app
        .package_integrity_audit_service
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
            .is_some_and(|message| message.contains("publication handoff remains retained"))
    );
    assert!(matches!(
        app.pending_dataset_open.as_ref(),
        Some(current_source_open_service::CurrentSourceOpenRequest::ImportedPublication(
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
    let audit = app.package_integrity_audit_service.as_ref().unwrap();
    assert_eq!(audit.diagnostics(), audit_before);
    assert!(audit.active_token().is_none());
    assert!(
        app.open_or_queue_dataset_path(temp.path().join("must-not-replace-retained.m4d"), None)
            .is_err()
    );
    assert!(matches!(
        app.pending_dataset_open.as_ref(),
        Some(current_source_open_service::CurrentSourceOpenRequest::ImportedPublication(_))
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
    app.package_integrity_audit_service
        .take()
        .unwrap()
        .shutdown()
        .unwrap();
    app.dataset.request_shutdown().unwrap();
}

#[test]
#[ignore = "developer-local: deep imported-publication drift integration"]
fn imported_transfer_drift_fails_closed_without_external_open_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let current_package = write_target_fixture(temp.path()).unwrap();
    let source = write_source_single_ome_fixture(temp.path()).unwrap();
    let destination = temp.path().join("drifted-import.m4d");
    let inspection = mirante4d_import_pipeline::inspect_tiff(TiffSource::single_3d(&source)).unwrap();
    let (_, options) =
        reviewed_import_options(TiffSource::single_3d(&source), inspection, destination.clone());
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
    app.source_open_service = Some(current_source_open_service::CurrentSourceOpenService::new(
        app.cpu_broker.clone(),
    ));
    install_test_project_store(&mut app);
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
            "The package was created and remains on disk, but Mirante4D could not complete its published import handoff; the current dataset and its project storage remain available."
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
    let audit = app.package_integrity_audit_service.as_ref().unwrap();
    assert_eq!(audit.diagnostics(), Default::default());
    assert!(audit.active_token().is_none());

    close_test_project_store(&mut app);
    app.source_open_service.take().unwrap().shutdown().unwrap();
    app.package_integrity_audit_service
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
fn import_analyze_save_and_reopen_without_a_global_integrity_audit() {
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

    let manifest = TiffSource::new(vec![
        TiffChannelSource::folder_of_3d("channel 1", &source).unwrap(),
    ])
    .unwrap();
    let inspection = mirante4d_import_pipeline::inspect_tiff(manifest.clone()).unwrap();
    let (_, options) = reviewed_import_options(manifest, inspection, package.clone());
    let published = mirante4d_import_pipeline::import_tiff(
        options,
        &TestImportLedger,
        &ImportCancellation::new(),
        |_| {},
    )
    .unwrap();
    // This test intentionally exercises a later independent external open;
    // the direct publication handoff is covered separately above.
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
    app.apply_application_command(ApplicationCommand::AttachDataset, &context)
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
        app.package_integrity_audit_service
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
    app.package_integrity_audit_service
        .take()
        .unwrap()
        .shutdown()
        .unwrap();
    drop(app);

    let reopened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut reopened_app = test_workbench_app_without_background_runtime(reopened);
    install_test_project_store(&mut reopened_app);
    reopened_app.project_store_noninteractive_paths.open = Some(project_path);
    let reopened_camera = *application_view(&reopened_app.application.snapshot()).camera();
    reopened_app
        .apply_render_intent_interaction(
            RenderIntentInteraction::Sample(mirante4d_application::RenderIntentSample::camera(
                mirante4d_application::RenderGestureKind::Drag,
                reopened_camera,
            )),
            &context,
        )
        .unwrap();
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
    assert_eq!(
        reopened_app.render_intent_mailbox.snapshot().active_gesture,
        None,
        "project-open completion retires transient render authority"
    );
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
        .package_integrity_audit_service
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

fn await_exact_three_d_timepoint_capture(
    app: &mut MiranteWorkbenchApp,
    context: &egui::Context,
    expected: TimeIndex,
) -> Vec<u8> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "the complete GPU presentation for t{} did not settle: current={:?}, \
             staged={}, plan_error={:?}, render_error={:?}",
            expected.get(),
            app.render_coordination
                .surface(PresentationSlot::ThreeD)
                .presented_frame()
                .map(|frame| (frame.timepoint(), frame.frame(), frame.progress().completeness())),
            app.dataset.staging_current_refinement(),
            app.dataset.last_plan_error(),
            app.render_coordination.frame_fidelity.last_capacity_error,
        );

        app.request_visible_bricks();
        app.drain_brick_results(context);
        if app
            .native_presentation
            .product_gpu
            .as_ref()
            .is_some_and(|product| !product.pending_validation_captures.is_empty())
        {
            app.poll_product_validation_captures()
                .expect("the temporal GPU validation capture completes");
        } else {
            let target_scope = if app.dataset.staging_current_refinement()
                && app
                    .dataset
                    .scope_requirements(dataset_requests::SCOPE_CURRENT_3D_REFINEMENT)
                    .iter()
                    .all(|key| key.timepoint() == expected)
            {
                dataset_requests::SCOPE_CURRENT_3D_REFINEMENT
            } else {
                dataset_requests::SCOPE_CURRENT_3D
            };
            if app.dataset.scope_resources_complete(target_scope) {
                app.rerender_coordinated_display_state()
                    .expect("the temporal successor renders through the product coordinator");
            }
        }

        let presented = app
            .render_coordination
            .surface(PresentationSlot::ThreeD)
            .presented_frame();
        if presented.is_some_and(|frame| {
            frame.timepoint() == expected
                && frame.progress().completeness()
                    == mirante4d_render_api::FrameCompleteness::Exact
        }) && let Some(capture) =
            product_automation::product_target_capture(app, PanelId::ThreeD)
        {
            return capture.rgba8().to_vec();
        }
        std::thread::yield_now();
    }
}

#[test]
#[ignore = "requires a trusted Vulkan GPU"]
fn direct_timepoint_selection_replaces_retained_gpu_pixels_with_the_successor_body() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    let mut renderer = test_wgpu_renderer(
        mirante4d_render_wgpu::WgpuRenderRuntimeConfig::new(1024 * 1024 * 1024)
            .unwrap()
            .with_validation_capture(true),
    );
    renderer
        .activate_dataset_generation(app.application.snapshot().catalog())
        .expect("the temporal fixture activates on the product renderer");
    app.native_presentation =
        native_presentation::NativePresentationBridge::with_headless_product_renderer(renderer);
    app.render_coordination.record_viewports(
        PresentationSlot::ThreeD,
        PresentationViewport::new(64.0, 64.0).unwrap(),
        mirante4d_render_api::RenderExtent::new(64, 64).unwrap(),
    );
    assert!(await_visible_demand_plan(&mut app).current_plan_installed);

    let timepoint_zero = await_exact_three_d_timepoint_capture(
        &mut app,
        &context,
        TimeIndex::new(0),
    );
    assert!(timepoint_zero.iter().any(|value| *value != 0));

    app.apply_application_command(
        ApplicationCommand::SetTimepoint(TimeIndex::new(1)),
        &context,
    )
    .unwrap();
    let retained = app
        .render_coordination
        .surface(PresentationSlot::ThreeD)
        .presented_frame()
        .expect("a temporal seek must retain the predecessor instead of exposing an empty frame");
    assert_eq!(retained.timepoint(), TimeIndex::new(0));

    let timepoint_one = await_exact_three_d_timepoint_capture(
        &mut app,
        &context,
        TimeIndex::new(1),
    );
    assert_ne!(
        timepoint_one, timepoint_zero,
        "different public-fixture timepoints must produce different GPU pixels"
    );
    assert_eq!(app.application.snapshot().view().timepoint(), TimeIndex::new(1));
    assert!(
        app.dataset
            .scope_requirements(dataset_requests::SCOPE_CURRENT_3D)
            .iter()
            .all(|key| key.timepoint() == TimeIndex::new(1)),
        "the promoted current body must be bound only to the visible successor timepoint"
    );
    assert!(!app.dataset.staging_current_refinement());
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn stopping_playback_removes_the_transient_window_from_cpu_and_renderer_authority() {
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
    let playback_only = app
        .dataset
        .scope_requirements(dataset_requests::SCOPE_PLAYBACK)
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    assert!(!playback_only.is_empty());

    app.apply_application_command(ApplicationCommand::SetPlaybackActive(false), &context)
        .unwrap();
    await_visible_demand_plan(&mut app);
    assert!(
        app.dataset
            .scope_requirements(dataset_requests::SCOPE_PLAYBACK)
            .is_empty()
    );
    assert!(
        app.dataset
            .renderer_requirements()
            .iter()
            .all(|key| !playback_only.contains(key))
    );
    assert!(
        playback_only
            .iter()
            .all(|key| app.dataset.retained_leases().payload(*key).is_none())
    );
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn stopping_playback_commits_the_last_coherently_presented_cursor() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();

    app.apply_application_command(ApplicationCommand::SetPlaybackActive(true), &context)
        .unwrap();
    await_visible_demand_plan(&mut app);
    app.apply_application_command(ApplicationCommand::MarkPlaybackPrepared, &context)
        .unwrap();
    app.apply_application_command(ApplicationCommand::AdvancePlaybackTick(100), &context)
        .unwrap();
    app.apply_application_command(ApplicationCommand::AdvancePlaybackTick(101), &context)
        .unwrap();

    assert_eq!(
        application_view(&app.application.snapshot()).timepoint(),
        TimeIndex::new(1),
        "the reducer cursor must be allowed to lead successor preparation"
    );
    assert_eq!(
        app.playback_session.presented_timepoint(),
        Some(TimeIndex::new(0)),
        "without a coordinated publication, the visible session front remains t0"
    );

    app.apply_application_command(ApplicationCommand::SetPlaybackActive(false), &context)
        .unwrap();

    let stopped = app.application.snapshot();
    assert!(!stopped.transient().playback_active());
    assert_eq!(
        application_view(&stopped).timepoint(),
        TimeIndex::new(0),
        "stop must commit what the user actually saw, not an unpublished requested successor"
    );
    assert!(app.playback_session.contract().is_none());
    app.dataset.request_shutdown().unwrap();
}

#[test]
fn four_panel_stop_handoff_captures_the_post_reconciliation_cutoff() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    let initial = app.application.snapshot();
    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section: *application_view(&initial).cross_section(),
        },
        &context,
    )
    .unwrap();
    let presentation = PresentationViewport::new(64.0, 64.0).unwrap();
    let extent = mirante4d_render_api::RenderExtent::new(64, 64).unwrap();
    for slot in PresentationSlot::ALL {
        app.render_coordination
            .record_viewports(slot, presentation, extent);
    }

    app.apply_application_command(ApplicationCommand::SetPlaybackActive(true), &context)
        .unwrap();
    await_visible_demand_plan(&mut app);
    app.apply_application_command(ApplicationCommand::MarkPlaybackPrepared, &context)
        .unwrap();
    app.apply_application_command(ApplicationCommand::AdvancePlaybackTick(100), &context)
        .unwrap();
    app.apply_application_command(ApplicationCommand::AdvancePlaybackTick(101), &context)
        .unwrap();
    assert_eq!(
        app.playback_session.presented_timepoint(),
        Some(TimeIndex::new(0))
    );
    for slot in PresentationSlot::ALL {
        let generation = app.render_coordination.surface(slot).generation();
        assert!(app.render_coordination.record_presented_frame(
            slot,
            generation,
            synthetic_presented_frame(slot, extent),
        ));
    }

    app.apply_application_command(ApplicationCommand::SetPlaybackActive(false), &context)
        .unwrap();

    let stopped = app.application.snapshot();
    let mailbox = app.render_intent_mailbox.snapshot();
    let transaction = app
        .presentation_scheduler
        .transaction(
            &stopped,
            &app.playback_session,
            &app.render_intent_mailbox,
        )
        .expect("Stop must retain one quality-only transaction after cursor reconciliation");
    assert!(transaction.is_retained_quality());
    assert_eq!(
        transaction.expected_revision(PresentationSlot::ThreeD),
        mailbox.three_d_revision
    );
    for slot in [
        PresentationSlot::Xy,
        PresentationSlot::Xz,
        PresentationSlot::Yz,
    ] {
        assert_eq!(
            transaction.expected_revision(slot),
            mailbox.linked_2d_revision
        );
    }
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
    let cross_generations_before_install = [PanelId::Xy, PanelId::Xz, PanelId::Yz].map(|panel| {
        app.render_coordination
            .surface(panel.presentation_slot())
            .generation()
    });

    await_visible_demand_plan(&mut app);
    assert_eq!(
        [PanelId::Xy, PanelId::Xz, PanelId::Yz].map(|panel| {
            app.render_coordination
                .surface(panel.presentation_slot())
                .generation()
        }),
        cross_generations_before_install,
        "worker plan installation must not manufacture a second semantic surface generation"
    );

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
fn four_panel_linked_navigation_and_playback_advance_under_one_composed_identity() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    let initial = app.application.snapshot();
    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section: *application_view(&initial).cross_section(),
        },
        &context,
    )
    .unwrap();
    let presentation = PresentationViewport::new(64.0, 64.0).unwrap();
    let render = mirante4d_render_api::RenderExtent::new(64, 64).unwrap();
    for slot in PresentationSlot::ALL {
        app.render_coordination
            .record_viewports(slot, presentation, render);
    }

    app.apply_application_command(ApplicationCommand::SetPlaybackActive(true), &context)
        .unwrap();
    await_visible_demand_plan(&mut app);
    let session_generation = app
        .playback_session
        .contract()
        .expect("warmup planning admits one playback contract")
        .generation();
    let playback_scales = app
        .playback_session
        .contract()
        .unwrap()
        .layer_scales()
        .clone();
    for scope in [
        dataset_requests::SCOPE_CROSS_SECTION_XY,
        dataset_requests::SCOPE_CROSS_SECTION_XZ,
        dataset_requests::SCOPE_CROSS_SECTION_YZ,
    ] {
        let plan = app
            .prepared_scope_render_plans
            .get(&scope)
            .expect("four-panel warmup installs every linked target");
        assert!(plan.covers_full_selected_volumes);
        assert_eq!(plan.layer_scales, playback_scales);
    }
    app.apply_application_command(ApplicationCommand::MarkPlaybackPrepared, &context)
        .unwrap();
    app.apply_application_command(ApplicationCommand::AdvancePlaybackTick(100), &context)
        .unwrap();

    let mut linked = mirante4d_application::viewport_interaction::CrossSectionViewState::from_canonical(
        *application_view(&app.application.snapshot()).cross_section(),
    );
    let linked_panel = mirante4d_application::viewport_interaction::CrossSectionPanel::Xy;
    let timepoint_count = app.application.snapshot().timepoint_count();
    for step in 0_u64..3 {
        match step {
            0 => linked.pan_by_panel_points(linked_panel, 2.0, -1.0),
            1 => linked.zoom_around_panel_point(linked_panel, presentation, 32.0, 32.0, 0.9),
            2 => linked.rotate_oblique_by_panel_drag(linked_panel, 2.0, 1.0, 0.005),
            _ => unreachable!(),
        }
        let expected_cross_section = linked.into_canonical().unwrap();
        app.apply_render_intent_interaction(
            RenderIntentInteraction::Sample(
                mirante4d_application::RenderIntentSample::cross_section(
                    mirante4d_application::CrossSectionPanelId::Xy,
                    mirante4d_application::RenderGestureKind::Drag,
                    expected_cross_section,
                ),
            ),
            &context,
        )
        .unwrap();
        let before_tick = app.application.snapshot();
        let base_before = RenderIntentBase::from_snapshot(&before_tick);
        let gesture_revision = app
            .render_intent_mailbox
            .active_revision(base_before)
            .expect("the linked sample owns one spatial revision");

        app.apply_application_command(
            ApplicationCommand::AdvancePlaybackTick(101 + step),
            &context,
        )
        .unwrap();

        let after_tick = app.application.snapshot();
        let base_after = RenderIntentBase::from_snapshot(&after_tick);
        let composed_revision = app
            .render_intent_mailbox
            .active_composed_revision(base_after)
            .expect("the active linked payload composes with the temporal frame");
        let (_, active_target, demand_revision) =
            app.effective_visible_demand_inputs(&after_tick);
        assert_eq!(
            after_tick.transient().playback_phase(),
            mirante4d_application::PlaybackPhase::Playing
        );
        assert!(after_tick.transient().playback_active());
        assert_eq!(
            application_view(&after_tick).timepoint(),
            mirante4d_domain::TimeIndex::new((step + 1) % timepoint_count)
        );
        assert_eq!(
            app.playback_session.contract().unwrap().generation(),
            session_generation,
            "linked navigation must not restart or rebase the temporal session"
        );
        assert_eq!(
            active_target,
            Some(mirante4d_application::RenderIntentTarget::CrossSection(
                mirante4d_application::CrossSectionPanelId::Xy,
            ))
        );
        assert_eq!(
            app.render_intent_mailbox.effective_cross_section(
                base_after,
                *application_view(&after_tick).cross_section(),
            ),
            expected_cross_section
        );
        assert!(composed_revision > gesture_revision);
        assert_eq!(demand_revision, composed_revision);
        assert_eq!(
            demand_revision,
            app.render_intent_mailbox.snapshot().linked_2d_revision,
            "demand and linked presentation must observe the same composed frame"
        );

        await_visible_demand_plan(&mut app);
        assert_eq!(app.dataset.last_plan_error(), None);
    }

    app.dataset.request_shutdown().unwrap();
}

#[test]
fn linked_input_rebases_an_in_flight_temporal_bundle_without_restarting_it() {
    let temp = tempfile::tempdir().unwrap();
    let package = write_target_fixture(temp.path()).unwrap();
    let opened = open_dataset_and_render_first_frame(&package).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    let context = egui::Context::default();
    let initial = app.application.snapshot();
    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section: *application_view(&initial).cross_section(),
        },
        &context,
    )
    .unwrap();
    let presentation = PresentationViewport::new(64.0, 64.0).unwrap();
    let render = mirante4d_render_api::RenderExtent::new(64, 64).unwrap();
    for slot in PresentationSlot::ALL {
        app.render_coordination
            .record_viewports(slot, presentation, render);
    }
    app.apply_application_command(ApplicationCommand::SetPlaybackActive(true), &context)
        .unwrap();
    await_visible_demand_plan(&mut app);
    let session_generation = app.playback_session.contract().unwrap().generation();
    app.apply_application_command(ApplicationCommand::MarkPlaybackPrepared, &context)
        .unwrap();
    app.apply_application_command(ApplicationCommand::AdvancePlaybackTick(100), &context)
        .unwrap();
    for slot in PresentationSlot::ALL {
        let generation = app.render_coordination.surface(slot).generation();
        assert!(app.render_coordination.record_presented_frame(
            slot,
            generation,
            synthetic_presented_frame(slot, render),
        ));
    }

    let (result_blocked, release_result) =
        app.dataset.block_next_visible_demand_result_publication();
    app.apply_application_command(ApplicationCommand::AdvancePlaybackTick(101), &context)
        .unwrap();
    result_blocked
        .recv_timeout(Duration::from_secs(5))
        .expect("the temporal successor reaches the worker publication boundary");
    let before_linked = app.dataset.visible_demand_diagnostics();

    let mut linked = mirante4d_application::viewport_interaction::CrossSectionViewState::from_canonical(
        app.render_intent_mailbox.effective_cross_section(
            RenderIntentBase::from_snapshot(&app.application.snapshot()),
            *application_view(&app.application.snapshot()).cross_section(),
        ),
    );
    let panel = mirante4d_application::viewport_interaction::CrossSectionPanel::Xy;
    let mut linked_samples = Vec::new();
    for step in 0_u8..3 {
        match step {
            0 => linked.pan_by_panel_points(panel, 2.0, -1.0),
            1 => linked.zoom_around_panel_point(panel, presentation, 32.0, 32.0, 0.9),
            2 => linked.rotate_oblique_by_panel_drag(panel, 2.0, 1.0, 0.005),
            _ => unreachable!(),
        }
        let latest = linked.into_canonical().unwrap();
        linked_samples.push(latest);
        app.apply_render_intent_interaction(
            RenderIntentInteraction::Sample(
                mirante4d_application::RenderIntentSample::cross_section(
                    mirante4d_application::CrossSectionPanelId::Xy,
                    mirante4d_application::RenderGestureKind::Drag,
                    latest,
                ),
            ),
            &context,
        )
        .unwrap();
        let snapshot = app.application.snapshot();
        assert_eq!(
            snapshot.transient().playback_phase(),
            mirante4d_application::PlaybackPhase::Playing
        );
        assert_eq!(application_view(&snapshot).timepoint(), TimeIndex::new(1));
        assert_eq!(
            app.playback_session.contract().unwrap().generation(),
            session_generation
        );
    }
    let after_linked = app.dataset.visible_demand_diagnostics();
    assert_eq!(after_linked.submitted, before_linked.submitted);
    assert_eq!(
        after_linked.pending_replacements,
        before_linked.pending_replacements
    );
    assert_eq!(
        after_linked.cancelled_running,
        before_linked.cancelled_running,
        "linked spatial input must not cancel temporal residency planning"
    );

    release_result.send(()).unwrap();
    await_visible_demand_plan(&mut app);
    let latest = *linked_samples.last().unwrap();
    assert_eq!(app.dataset.last_plan_error(), None);
    assert!(app.visible_demand_plan_currentness().cross_sections);
    let snapshot = app.application.snapshot();
    assert_eq!(
        app.render_intent_mailbox.effective_cross_section(
            RenderIntentBase::from_snapshot(&snapshot),
            *application_view(&snapshot).cross_section(),
        ),
        latest
    );
    app.apply_render_intent_interaction(
        RenderIntentInteraction::Finish(
            mirante4d_application::RenderIntentTarget::CrossSection(
                mirante4d_application::CrossSectionPanelId::Xy,
            ),
        ),
        &context,
    )
    .unwrap();
    assert_eq!(
        app.application.snapshot().transient().playback_phase(),
        mirante4d_application::PlaybackPhase::Playing
    );
    assert_eq!(
        app.playback_session.contract().unwrap().generation(),
        session_generation
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
fn successful_linked_only_install_clears_its_historical_capacity_warning() {
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
    let historical = "linked 2D historical capacity refusal";
    app.dataset.record_plan_error(historical);
    app.render_coordination.frame_fidelity.last_failure_kind =
        Some(FrameFailureKind::BudgetExceeded);
    app.render_coordination.frame_fidelity.last_capacity_error =
        Some(historical.to_owned());

    app.apply_application_command(
        ApplicationCommand::SetLayout {
            layout: CanonicalViewerLayout::FourPanel,
            cross_section: moved_cross,
        },
        &context,
    )
    .unwrap();
    let installed = await_visible_demand_plan(&mut app);

    assert!(installed.cross_section_plan_installed);
    assert!(!installed.current_plan_installed);
    assert_eq!(app.dataset.last_plan_error(), None);
    assert_eq!(
        app.render_coordination.frame_fidelity.last_failure_kind,
        None
    );
    assert_eq!(
        app.render_coordination.frame_fidelity.last_capacity_error,
        None
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
        if let Some(result) = app.dataset.take_visible_demand_plan_result() {
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
