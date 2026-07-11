fn streaming_test_source(
    path: impl AsRef<std::path::Path>,
) -> anyhow::Result<(
    ApplicationState,
    dataset_opening::OpenedCurrentSource,
    current_runtime::ui::CurrentUiRuntime,
)> {
    let mut opened = open_dataset_and_render_first_frame(path)?;
    let application = test_application_for_opened_source(&opened);
    let ui_runtime = current_runtime::ui::CurrentUiRuntime::new(ResourcePolicy::default(), None);
    rerender_state_with_backend(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &mut opened.render_runtime,
        None,
    )?;
    Ok((application, opened, ui_runtime))
}

fn streaming_test_source_with_policy(
    path: impl AsRef<std::path::Path>,
    resource_policy: ResourcePolicy,
) -> anyhow::Result<(
    ApplicationState,
    dataset_opening::OpenedCurrentSource,
    current_runtime::ui::CurrentUiRuntime,
)> {
    let mut opened =
        dataset_opening::open_test_dataset_with_resource_policy_and_render_first_frame(
            path,
            resource_policy,
            test_initial_render_viewport(),
            dataset_opening::TEST_DENSE_STARTUP_VOXEL_LIMIT,
        )?;
    let application = ApplicationState::new_unbound(
        SourceSessionGeneration::new(1),
        opened.catalog.as_ref().clone(),
        opened.workspace.clone(),
        resource_policy,
    )
    .map_err(|code| anyhow::anyhow!("test application state rejected: {code:?}"))?;
    let ui_runtime = current_runtime::ui::CurrentUiRuntime::new(resource_policy, None);
    rerender_state_with_backend(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &mut opened.render_runtime,
        None,
    )?;
    Ok((application, opened, ui_runtime))
}

fn submit_streaming_test_bricks(
    application: &ApplicationState,
    opened: &mut dataset_opening::OpenedCurrentSource,
    pool: &BrickReadPool,
) -> brick_streaming::BrickSubmissionResult {
    submit_visible_bricks_to_pool(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &mut opened.analysis_runtime,
        &opened.render_runtime,
        pool,
    )
}

fn submit_streaming_test_bricks_with_options(
    application: &ApplicationState,
    opened: &mut dataset_opening::OpenedCurrentSource,
    pool: &BrickReadPool,
    options: BrickSubmissionOptions,
) -> brick_streaming::BrickSubmissionResult {
    submit_visible_bricks_to_pool_with_options(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &mut opened.analysis_runtime,
        &opened.render_runtime,
        pool,
        options,
    )
}

fn apply_streaming_test_outcome(
    application: &ApplicationState,
    opened: &mut dataset_opening::OpenedCurrentSource,
    outcome: BrickReadOutcome,
) -> bool {
    apply_brick_read_outcome(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &mut opened.analysis_runtime,
        &opened.render_runtime,
        outcome,
    )
}

fn apply_streaming_test_outcomes(
    application: &ApplicationState,
    opened: &mut dataset_opening::OpenedCurrentSource,
    outcomes: impl IntoIterator<Item = BrickReadOutcome>,
) -> bool {
    apply_brick_read_outcomes(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &mut opened.analysis_runtime,
        &opened.render_runtime,
        outcomes,
    )
}

fn render_streaming_test_resident_frame(
    application: &ApplicationState,
    opened: &mut dataset_opening::OpenedCurrentSource,
    ui_runtime: &current_runtime::ui::CurrentUiRuntime,
) -> anyhow::Result<()> {
    render_state_from_resident_bricks(
        &application.snapshot(),
        &opened.dataset_runtime,
        &mut opened.render_runtime,
        &opened.analysis_runtime,
        ui_runtime,
    )
}

fn streaming_test_frame_ready(
    application: &ApplicationState,
    opened: &dataset_opening::OpenedCurrentSource,
) -> bool {
    current_resident_frame_ready(
        &application.snapshot(),
        &opened.dataset_runtime,
        &opened.render_runtime,
    )
}

fn set_orthographic_world_per_screen_point_for_height(
    application: &mut ApplicationState,
    presentation_viewport: PresentationViewport,
    visible_height: f64,
) {
    set_orthographic_world_per_screen_point(
        application,
        visible_height / presentation_viewport.height_points(),
    );
}

fn set_orthographic_world_per_screen_point(
    application: &mut ApplicationState,
    world_per_point: f64,
) {
    let snapshot = application.snapshot();
    let camera = *application_view(&snapshot).camera();
    application
        .dispatch(ApplicationCommand::SetCamera(
            CameraView::new(
                camera.projection(),
                camera.target(),
                camera.orientation(),
                world_per_point,
                camera.perspective_focal_length_screen_points(),
                camera.perspective_view_distance_world(),
            )
            .unwrap(),
        ))
        .unwrap();
}

fn set_streaming_test_layer_render_state(
    application: &mut ApplicationState,
    layer_key: LogicalLayerKey,
    visible: bool,
    render_state: CanonicalRenderState,
) {
    let snapshot = application.snapshot();
    let layer = application_view(&snapshot)
        .layer(layer_key)
        .expect("the streaming test layer must exist")
        .clone();
    application
        .dispatch(ApplicationCommand::SetLayerView(LayerViewState::new(
            layer_key,
            visible,
            layer.transfer().clone(),
            render_state,
        )))
        .unwrap();
}

fn translate_streaming_test_camera_target_x(application: &mut ApplicationState, offset: f64) {
    let snapshot = application.snapshot();
    let camera = *application_view(&snapshot).camera();
    let target = camera.target();
    let target = WorldPoint3::new(target.x() + offset, target.y(), target.z()).unwrap();
    application
        .dispatch(ApplicationCommand::SetCamera(
            CameraView::new(
                camera.projection(),
                target,
                camera.orientation(),
                camera.orthographic_world_per_screen_point(),
                camera.perspective_focal_length_screen_points(),
                camera.perspective_view_distance_world(),
            )
            .unwrap(),
        ))
        .unwrap();
}

fn write_time_multiscale_app_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("app-time-multiscale.m4d");
    let s0_shape = Shape4D::new(3, 4, 4, 4).unwrap();
    let s1_shape = Shape4D::new(3, 2, 2, 2).unwrap();
    let s0_grid_to_world = mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0);
    let s1_grid_to_world = s0_grid_to_world
        .downsampled_integer_centered(2, 2, 2)
        .unwrap();
    write_native_u16_multiscale_dataset(
        &package_root,
        NativeU16MultiscaleDataset {
            id: "app-time-multiscale-fixture".to_owned(),
            name: "App time multiscale fixture".to_owned(),
            world_space: mirante4d_format::WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_format::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16MultiscaleLayer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape: s0_shape,
                grid_to_world: s0_grid_to_world,
                display: default_u16_display(),
                scales: vec![
                    DenseU16Scale {
                        level: 0,
                        shape: s0_shape,
                        brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                        grid_to_world: s0_grid_to_world,
                        source_scale: None,
                        reduction: ScaleReduction::Source,
                        values_tzyx: fixture_values(s0_shape),
                    },
                    DenseU16Scale {
                        level: 1,
                        shape: s1_shape,
                        brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                        grid_to_world: s1_grid_to_world,
                        source_scale: Some(0),
                        reduction: ScaleReduction::Mean,
                        values_tzyx: app_multiscale_s1_values(s1_shape),
                    },
                ],
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

#[test]
fn app_submits_visible_bricks_to_worker_pool() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let (application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 4).unwrap();

    let submission = submit_streaming_test_bricks(&application, &mut opened, &pool);
    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 1);
    assert!(submission.prefetch_tickets.is_empty());
    assert!(!submit_streaming_test_bricks(&application, &mut opened, &pool).queued_current);
    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(apply_streaming_test_outcome(
        &application,
        &mut opened,
        outcome
    ));
    render_streaming_test_resident_frame(&application, &mut opened, &ui_runtime).unwrap();
    let stats = opened.dataset_runtime.dataset.stats().unwrap();

    assert_eq!(opened.render_runtime.visible_brick_count, 1);
    assert_eq!(opened.dataset_runtime.brick_stream_requested, 1);
    assert_eq!(opened.dataset_runtime.brick_stream_completed, 1);
    assert_eq!(opened.dataset_runtime.brick_stream_failed, 0);
    assert!(opened.dataset_runtime.brick_stream_complete);
    assert_eq!(opened.dataset_runtime.resident_bricks.len(), 1);
    assert_eq!(
        opened.render_runtime.render_backend,
        RenderBackend::CpuResidentBricks
    );
    assert_eq!(stats.brick_requests_queued, 1);
    assert_eq!(stats.brick_requests_completed, 1);
    assert_eq!(stats.brick_reads, 1);
}

#[test]
fn app_requeues_same_visible_brick_request_when_stream_finished_incomplete() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_sparse_spatially_chunked_app_dataset(tempdir.path(), false);
    let (application, mut opened, _) = streaming_test_source(root).unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 16).unwrap();

    let first_submission = submit_streaming_test_bricks_with_options(
        &application,
        &mut opened,
        &pool,
        BrickSubmissionOptions::CURRENT_ONLY,
    );
    assert!(first_submission.queued_current);
    assert_eq!(first_submission.current_tickets.len(), 8);
    for _ in 0..first_submission.current_tickets.len() {
        let _ = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    let requested = opened.dataset_runtime.brick_stream_requested;
    let first_generation = opened.dataset_runtime.brick_stream_generation;
    opened.dataset_runtime.brick_stream_completed = 0;
    opened.dataset_runtime.brick_stream_cancelled = requested;
    opened.dataset_runtime.brick_stream_stale = 0;
    opened.dataset_runtime.brick_stream_failed = 0;
    opened.dataset_runtime.brick_stream_complete = false;

    let retry_submission = submit_streaming_test_bricks_with_options(
        &application,
        &mut opened,
        &pool,
        BrickSubmissionOptions::CURRENT_ONLY,
    );

    assert!(retry_submission.current_changed);
    assert!(retry_submission.queued_current);
    assert_eq!(retry_submission.current_tickets.len(), requested);
    assert!(opened.dataset_runtime.brick_stream_generation > first_generation);
    assert_eq!(opened.dataset_runtime.brick_stream_requested, requested);
    assert_eq!(opened.dataset_runtime.brick_stream_completed, 0);
    assert_eq!(opened.dataset_runtime.brick_stream_cancelled, 0);
    assert!(!opened.dataset_runtime.brick_stream_complete);
}

#[test]
fn app_reads_valid_zero_visible_bricks_instead_of_materializing_empty_metadata() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_sparse_spatially_chunked_app_dataset(tempdir.path(), false);
    let (application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 16).unwrap();

    assert_eq!(opened.render_runtime.visible_bricks.len(), 8);
    let submission = submit_streaming_test_bricks_with_options(
        &application,
        &mut opened,
        &pool,
        BrickSubmissionOptions::CURRENT_ONLY,
    );

    assert!(submission.current_changed);
    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 8);
    assert_eq!(opened.dataset_runtime.brick_stream_requested, 8);
    assert_eq!(opened.dataset_runtime.brick_stream_completed, 0);
    assert!(!opened.dataset_runtime.brick_stream_complete);
    assert!(opened.dataset_runtime.resident_bricks.is_empty());

    for _ in 0..submission.current_tickets.len() {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.priority, BrickRequestPriority::CurrentFrame);
        assert!(apply_streaming_test_outcome(
            &application,
            &mut opened,
            outcome
        ));
    }
    render_streaming_test_resident_frame(&application, &mut opened, &ui_runtime).unwrap();
    let stats = opened.dataset_runtime.dataset.stats().unwrap();

    assert_eq!(opened.dataset_runtime.brick_stream_completed, 8);
    assert!(opened.dataset_runtime.brick_stream_complete);
    assert_eq!(opened.dataset_runtime.resident_bricks.len(), 8);
    assert_eq!(
        opened.render_runtime.render_backend,
        RenderBackend::CpuResidentBricks
    );
    assert!(opened.render_runtime.diagnostics.nonzero_pixels > 0);
    assert_eq!(stats.brick_requests_queued, 8);
    assert_eq!(stats.brick_requests_completed, 8);
    assert_eq!(stats.brick_reads, 8);
}

#[test]
fn batched_brick_outcome_apply_preserves_resident_ordering() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_sparse_spatially_chunked_app_dataset(tempdir.path(), false);
    let (application, mut opened, _) = streaming_test_source(root).unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 16).unwrap();
    let submission = submit_streaming_test_bricks_with_options(
        &application,
        &mut opened,
        &pool,
        BrickSubmissionOptions::CURRENT_ONLY,
    );

    let mut outcomes = Vec::new();
    for _ in 0..submission.current_tickets.len() {
        outcomes.push(pool.recv_timeout(Duration::from_secs(2)).unwrap());
    }
    outcomes.reverse();
    assert!(apply_streaming_test_outcomes(
        &application,
        &mut opened,
        outcomes
    ));

    let resident_order = opened
        .dataset_runtime
        .resident_bricks
        .iter()
        .map(|brick| {
            (
                brick.brick_index.z,
                brick.brick_index.y,
                brick.brick_index.x,
            )
        })
        .collect::<Vec<_>>();
    let mut sorted_order = resident_order.clone();
    sorted_order.sort();
    assert_eq!(resident_order, sorted_order);
    assert_eq!(opened.dataset_runtime.brick_stream_completed, 8);
    assert!(opened.dataset_runtime.brick_stream_complete);
    assert_eq!(opened.dataset_runtime.resident_bricks.len(), 8);
    assert_eq!(
        opened
            .dataset_runtime
            .resident_bricks_by_layer
            .get(&LayerId::new("ch0").unwrap())
            .map(Vec::len)
            .unwrap_or_default(),
        8
    );
}

#[test]
fn request_visible_bricks_does_not_render_as_side_effect() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_sparse_spatially_chunked_app_dataset(tempdir.path(), false);
    let (application, mut opened, _) = streaming_test_source(root).unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 16).unwrap();
    let submission = submit_streaming_test_bricks_with_options(
        &application,
        &mut opened,
        &pool,
        BrickSubmissionOptions::CURRENT_ONLY,
    );
    let mut outcomes = Vec::new();
    for _ in 0..submission.current_tickets.len() {
        outcomes.push(pool.recv_timeout(Duration::from_secs(2)).unwrap());
    }
    assert!(apply_streaming_test_outcomes(
        &application,
        &mut opened,
        outcomes
    ));
    assert!(streaming_test_frame_ready(&application, &opened));

    let mut app = test_workbench_app_without_background_runtime(opened);
    app.dataset_runtime.brick_read_pool = Some(pool);
    app.render_runtime.render_backend = RenderBackend::Loading;

    let outcome = app.request_visible_bricks();

    assert!(!outcome.current_changed);
    assert!(!outcome.resident_changed);
    assert!(outcome.current_frame_ready);
    assert_eq!(app.render_runtime.render_backend, RenderBackend::Loading);
}

#[test]
fn request_visible_bricks_replans_when_current_request_is_already_resident() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_sparse_spatially_chunked_app_dataset(tempdir.path(), false);
    let (application, mut opened, _) = streaming_test_source(root).unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 16).unwrap();
    let submission = submit_streaming_test_bricks_with_options(
        &application,
        &mut opened,
        &pool,
        BrickSubmissionOptions::CURRENT_ONLY,
    );
    let mut outcomes = Vec::new();
    for _ in 0..submission.current_tickets.len() {
        outcomes.push(pool.recv_timeout(Duration::from_secs(2)).unwrap());
    }
    assert!(apply_streaming_test_outcomes(
        &application,
        &mut opened,
        outcomes
    ));
    assert!(streaming_test_frame_ready(&application, &opened));

    let mut app = test_workbench_app_without_background_runtime(opened);
    app.dataset_runtime.brick_read_pool = Some(pool);
    app.render_runtime.render_backend = RenderBackend::Loading;
    app.render_runtime.lod_replan_pending = false;
    app.dataset_runtime.brick_stream_request_key = None;

    let outcome = app.request_visible_bricks();

    assert!(outcome.current_changed);
    assert!(!outcome.resident_changed);
    assert!(outcome.current_frame_ready);
    assert!(app.render_runtime.lod_replan_pending);
    assert_eq!(app.render_runtime.render_backend, RenderBackend::Loading);
}

#[test]
fn app_reads_all_valid_zero_visible_bricks_before_reporting_complete() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_sparse_spatially_chunked_app_dataset(tempdir.path(), true);
    let (application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 16).unwrap();

    assert_eq!(opened.render_runtime.visible_bricks.len(), 8);
    let submission = submit_streaming_test_bricks(&application, &mut opened, &pool);
    for _ in 0..submission.current_tickets.len() {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.priority, BrickRequestPriority::CurrentFrame);
        assert!(apply_streaming_test_outcome(
            &application,
            &mut opened,
            outcome
        ));
    }
    render_streaming_test_resident_frame(&application, &mut opened, &ui_runtime).unwrap();
    let stats = opened.dataset_runtime.dataset.stats().unwrap();

    assert!(submission.current_changed);
    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 8);
    assert_eq!(opened.dataset_runtime.brick_stream_requested, 8);
    assert_eq!(opened.dataset_runtime.brick_stream_completed, 8);
    assert!(opened.dataset_runtime.brick_stream_complete);
    assert_eq!(opened.dataset_runtime.resident_bricks.len(), 8);
    assert_eq!(opened.render_runtime.diagnostics.nonzero_pixels, 0);
    assert_eq!(stats.brick_requests_queued, 8);
    assert_eq!(stats.brick_reads, 8);
}

#[test]
fn app_resident_renderer_composites_visible_channel_layers() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let (application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 2).unwrap();

    let submission = submit_streaming_test_bricks(&application, &mut opened, &pool);

    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 2);
    assert!(submission.prefetch_tickets.is_empty());
    for _ in 0..2 {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.priority, BrickRequestPriority::CurrentFrame);
        assert!(apply_streaming_test_outcome(
            &application,
            &mut opened,
            outcome
        ));
    }
    assert!(opened.dataset_runtime.brick_stream_complete);

    render_streaming_test_resident_frame(&application, &mut opened, &ui_runtime).unwrap();

    assert_eq!(
        opened.render_runtime.render_backend,
        RenderBackend::CpuResidentBricks
    );
    assert_eq!(opened.render_runtime.rendered_channels.len(), 2);
    assert_eq!(
        opened.render_runtime.rendered_channels[0].layer_id.as_str(),
        "ch0"
    );
    assert_eq!(
        opened.render_runtime.rendered_channels[1].layer_id.as_str(),
        "ch1"
    );
    assert_eq!(opened.dataset_runtime.resident_bricks.len(), 1);
    assert_eq!(opened.dataset_runtime.resident_bricks_by_layer.len(), 2);
    assert_eq!(
        opened.render_runtime.rendered_channels[0].frame.pixels(),
        opened.render_runtime.frame.pixels()
    );

    let snapshot = application.snapshot();
    let view = application_view(&snapshot);
    let active_layer = view.layer(view.active_layer()).unwrap();
    let active_display = opened.dataset_runtime.dataset.manifest().layers
        [usize::try_from(view.active_layer().ordinal()).unwrap()]
    .display;
    let active_only = crate::image_compositing::mip_to_color_image_with_color(
        &opened.render_runtime.frame,
        active_display,
        active_layer.transfer().color(),
    );
    let composited = color_image_for_snapshot(
        &snapshot,
        &opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &opened.render_runtime,
    );
    assert_ne!(composited.pixels, active_only.pixels);
}

#[test]
fn app_resident_dvr_uses_same_ray_frame_not_per_channel_composite() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let (mut application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();
    let layers = application_view(&application.snapshot()).layers().to_vec();
    for layer in layers {
        application
            .dispatch(ApplicationCommand::SetLayerView(LayerViewState::new(
                layer.layer_key(),
                layer.visible(),
                layer.transfer().clone(),
                CanonicalRenderState::dvr(
                    SamplingPolicy::SmoothLinear,
                    CanonicalDvrOpacityTransfer::new(
                        layer.transfer().window(),
                        layer.transfer().curve(),
                    ),
                    12.0,
                )
                .unwrap(),
            )))
            .unwrap();
    }
    rerender_state_with_backend(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &mut opened.render_runtime,
        None,
    )
    .unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 2).unwrap();

    let submission = submit_streaming_test_bricks(&application, &mut opened, &pool);
    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 2);
    for _ in 0..2 {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.priority, BrickRequestPriority::CurrentFrame);
        assert!(apply_streaming_test_outcome(
            &application,
            &mut opened,
            outcome
        ));
    }
    assert!(opened.dataset_runtime.brick_stream_complete);

    render_streaming_test_resident_frame(&application, &mut opened, &ui_runtime).unwrap();

    assert_eq!(
        opened.render_runtime.render_backend,
        RenderBackend::CpuResidentBricks
    );
    assert_eq!(opened.render_runtime.rendered_channels.len(), 2);
    assert!(opened.render_runtime.frame.dvr_rgba().is_some());
    assert!(
        opened
            .render_runtime
            .rendered_channels
            .iter()
            .all(|channel| channel.frame.dvr_rgba().is_none())
    );
    let image = color_image_for_snapshot(
        &application.snapshot(),
        &opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &opened.render_runtime,
    );
    assert_eq!(
        image.size,
        [
            usize::try_from(opened.render_runtime.render_viewport.width).unwrap(),
            usize::try_from(opened.render_runtime.render_viewport.height).unwrap(),
        ]
    );
    assert!(
        image
            .pixels
            .iter()
            .any(|pixel| *pixel != egui::Color32::TRANSPARENT)
    );
}

#[test]
fn app_prefetches_next_timepoints_without_mutating_current_resident_bricks() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let (application, mut opened, _) = streaming_test_source(root).unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 8).unwrap();

    let submission = submit_streaming_test_bricks(&application, &mut opened, &pool);
    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 2);
    assert_eq!(submission.prefetch_tickets.len(), 2);

    for _ in 0..2 {
        let current = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(current.priority, BrickRequestPriority::CurrentFrame);
        assert!(apply_streaming_test_outcome(
            &application,
            &mut opened,
            current
        ));
    }
    for _ in 0..2 {
        let prefetch = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(prefetch.priority, BrickRequestPriority::Prefetch);
        assert!(!apply_streaming_test_outcome(
            &application,
            &mut opened,
            prefetch
        ));
    }
    let stats_before_cache_probe = opened.dataset_runtime.dataset.stats().unwrap();
    let layer_id = current_physical_layer_id(
        &opened.dataset_runtime,
        application_view(&application.snapshot()).active_layer(),
    )
    .unwrap();
    let _ = opened
        .dataset_runtime
        .dataset
        .read_u16_brick(
            &layer_id,
            TimeIndex::new(1),
            SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    let stats_after_cache_probe = opened.dataset_runtime.dataset.stats().unwrap();

    assert_eq!(opened.dataset_runtime.brick_stream_requested, 2);
    assert_eq!(opened.dataset_runtime.brick_stream_completed, 2);
    assert!(opened.dataset_runtime.brick_stream_complete);
    assert_eq!(opened.dataset_runtime.resident_bricks.len(), 1);
    assert_eq!(opened.dataset_runtime.resident_bricks_by_layer.len(), 2);
    assert_eq!(
        opened.dataset_runtime.brick_prefetch_timepoints,
        vec![TimeIndex::new(1), TimeIndex::new(2)]
    );
    assert_eq!(opened.dataset_runtime.brick_prefetch_requested, 2);
    assert_eq!(opened.dataset_runtime.brick_prefetch_completed, 2);
    assert_eq!(opened.dataset_runtime.brick_prefetch_failed, 0);
    assert_eq!(opened.dataset_runtime.brick_prefetch_skipped, 0);
    assert_eq!(stats_before_cache_probe.brick_requests_queued, 4);
    assert_eq!(stats_before_cache_probe.brick_requests_completed, 4);
    assert!(stats_after_cache_probe.brick_cache_hits > stats_before_cache_probe.brick_cache_hits);
}

#[test]
fn prefetched_next_timepoint_bricks_promote_without_fresh_current_reads() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let (mut application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();
    opened.dataset_runtime.active_volume_u8 = None;
    opened.dataset_runtime.active_volume = None;
    opened.dataset_runtime.active_volume_f32 = None;
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 32).unwrap();

    let initial_submission = submit_streaming_test_bricks(&application, &mut opened, &pool);
    assert!(initial_submission.queued_current);
    assert_eq!(initial_submission.current_tickets.len(), 8);
    assert_eq!(initial_submission.prefetch_tickets.len(), 16);

    for _ in 0..24 {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        apply_streaming_test_outcome(&application, &mut opened, outcome);
    }
    assert!(opened.dataset_runtime.brick_stream_complete);
    assert_eq!(opened.dataset_runtime.prefetched_brick_payloads.len(), 16);

    let previous_view = application_view(&application.snapshot()).clone();
    application
        .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(1)))
        .unwrap();
    let snapshot = application.snapshot();
    assert!(
        layer_state::reconcile_view_runtime(
            &previous_view,
            &snapshot,
            &mut opened.dataset_runtime,
            &mut opened.render_runtime,
            &mut opened.analysis_runtime,
        )
        .unwrap()
    );
    let promoted_submission = submit_streaming_test_bricks(&application, &mut opened, &pool);

    assert!(promoted_submission.current_changed);
    assert!(!promoted_submission.queued_current);
    assert!(promoted_submission.current_tickets.is_empty());
    assert_eq!(opened.dataset_runtime.brick_stream_requested, 8);
    assert_eq!(opened.dataset_runtime.brick_stream_completed, 8);
    assert!(opened.dataset_runtime.brick_stream_complete);
    assert_eq!(opened.dataset_runtime.resident_bricks.len(), 8);
    assert_eq!(application_view(&snapshot).timepoint(), TimeIndex::new(1));

    render_streaming_test_resident_frame(&application, &mut opened, &ui_runtime).unwrap();
    assert!(opened.render_runtime.diagnostics.nonzero_pixels > 0);
}

#[test]
fn visible_plan_change_reuses_compatible_resident_bricks_without_blank_reload() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let (application, mut opened, _) = streaming_test_source(root).unwrap();
    opened.dataset_runtime.active_volume_u8 = None;
    opened.dataset_runtime.active_volume = None;
    opened.dataset_runtime.active_volume_f32 = None;
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 32).unwrap();

    let initial_submission = submit_streaming_test_bricks(&application, &mut opened, &pool);
    assert!(initial_submission.queued_current);
    for _ in 0..initial_submission.current_tickets.len() {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(apply_streaming_test_outcome(
            &application,
            &mut opened,
            outcome
        ));
    }
    assert!(opened.dataset_runtime.brick_stream_complete);
    assert_eq!(opened.dataset_runtime.resident_bricks.len(), 8);

    let retained_brick = opened.render_runtime.visible_bricks[0];
    opened.render_runtime.visible_bricks = vec![retained_brick];
    opened.render_runtime.visible_brick_count = 1;
    opened.dataset_runtime.brick_stream_request_key = None;
    let subset_submission = submit_streaming_test_bricks(&application, &mut opened, &pool);

    assert!(subset_submission.current_changed);
    assert!(!subset_submission.queued_current);
    assert!(subset_submission.current_tickets.is_empty());
    assert_eq!(opened.dataset_runtime.brick_stream_requested, 1);
    assert_eq!(opened.dataset_runtime.brick_stream_completed, 1);
    assert!(opened.dataset_runtime.brick_stream_complete);
    assert_eq!(opened.dataset_runtime.resident_bricks.len(), 1);
    assert_eq!(
        opened.dataset_runtime.resident_bricks[0].brick_index,
        retained_brick
    );
}

#[test]
fn playback_submission_prioritizes_current_and_prefetch_over_warm_work() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let (application, mut opened, _) = streaming_test_source(root).unwrap();
    opened.dataset_runtime.active_volume_u8 = None;
    opened.dataset_runtime.active_volume = None;
    opened.dataset_runtime.active_volume_f32 = None;
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 32).unwrap();

    let submission = submit_streaming_test_bricks_with_options(
        &application,
        &mut opened,
        &pool,
        BrickSubmissionOptions::PLAYBACK,
    );

    assert!(submission.current_changed);
    assert!(submission.queued_current);
    assert!(!submission.current_tickets.is_empty());
    assert!(!submission.prefetch_tickets.is_empty());
    assert!(submission.warm_tickets.is_empty());
}

#[test]
fn prefetch_timepoints_wrap_for_looping_playback() {
    assert_eq!(
        brick_streaming::prefetch_timepoints(TimeIndex::new(1), 3, 2),
        vec![TimeIndex::new(2), TimeIndex::new(0)]
    );

    assert_eq!(
        brick_streaming::prefetch_timepoints(TimeIndex::new(2), 3, 2),
        vec![TimeIndex::new(0), TimeIndex::new(1)]
    );
}

#[test]
fn app_limits_prefetch_to_queue_capacity_after_current_frame_work() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let (application, mut opened, _) = streaming_test_source(root).unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 10).unwrap();

    assert_eq!(opened.render_runtime.visible_bricks.len(), 8);
    let submission = submit_streaming_test_bricks(&application, &mut opened, &pool);
    let stats = opened.dataset_runtime.dataset.stats().unwrap();

    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 8);
    assert_eq!(submission.prefetch_tickets.len(), 2);
    assert!(submission.warm_tickets.is_empty());
    assert_eq!(opened.dataset_runtime.brick_stream_requested, 8);
    assert_eq!(opened.dataset_runtime.brick_prefetch_requested, 2);
    assert_eq!(opened.dataset_runtime.brick_prefetch_skipped, 14);
    assert_eq!(opened.dataset_runtime.brick_warm_requested, 0);
    assert_eq!(
        opened.dataset_runtime.brick_prefetch_timepoints,
        vec![TimeIndex::new(1)]
    );
    assert_eq!(opened.dataset_runtime.brick_prefetch_failed, 0);
    assert_eq!(stats.brick_requests_queued, 10);
    assert_eq!(stats.brick_queue_full, 0);
}

#[test]
fn app_selects_stream_scale_from_camera_pixel_footprint() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_app_dataset(tempdir.path());
    let (mut application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();
    let snapshot = application.snapshot();
    let layer_id = current_physical_layer_id(
        &opened.dataset_runtime,
        application_view(&snapshot).active_layer(),
    )
    .unwrap();

    assert_eq!(
        lod_scheduler::select_stream_scale(
            &snapshot,
            &opened.dataset_runtime,
            &opened.render_runtime,
            &layer_id,
        )
        .unwrap(),
        0
    );

    set_orthographic_world_per_screen_point(&mut application, 2.5);
    rerender_state_with_backend(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &mut opened.render_runtime,
        None,
    )
    .unwrap();

    assert_eq!(opened.dataset_runtime.brick_stream_scale_level, 1);
    assert_eq!(
        opened.dataset_runtime.brick_stream_scale_shape,
        Shape3D::new(2, 2, 2).unwrap()
    );
    assert_eq!(
        opened.render_runtime.visible_bricks,
        vec![SpatialBrickIndex::new(0, 0, 0)]
    );

    set_orthographic_world_per_screen_point(&mut application, 0.5);
    rerender_state_with_backend(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &mut opened.render_runtime,
        None,
    )
    .unwrap();

    assert_eq!(opened.dataset_runtime.brick_stream_scale_level, 0);
    assert_eq!(
        opened.dataset_runtime.brick_stream_scale_shape,
        Shape3D::new(4, 4, 4).unwrap()
    );
}

#[test]
fn playback_lod_downshift_selects_s1_for_normal_s0_view() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_app_dataset(tempdir.path());
    let (application, mut opened, _) = streaming_test_source(root).unwrap();
    let snapshot = application.snapshot();
    let layer_id = current_physical_layer_id(
        &opened.dataset_runtime,
        application_view(&snapshot).active_layer(),
    )
    .unwrap();

    assert_eq!(
        lod_scheduler::select_stream_scale(
            &snapshot,
            &opened.dataset_runtime,
            &opened.render_runtime,
            &layer_id,
        )
        .unwrap(),
        0
    );

    opened.render_runtime.playback_lod_downshift_active = true;
    update_visible_brick_plan(
        &snapshot,
        &mut opened.dataset_runtime,
        &mut opened.render_runtime,
    );

    assert_eq!(opened.render_runtime.lod_schedule.target_scale_level, 1);
    assert_eq!(opened.dataset_runtime.brick_stream_scale_level, 1);
    assert_eq!(opened.render_runtime.frame_fidelity.target_scale_level, 1);
    assert_eq!(
        opened.render_runtime.frame_fidelity.reason,
        LodDecisionReason::PlaybackDownshift
    );
}

#[test]
fn playback_lod_downshift_does_not_coarsen_normal_s1_view() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_app_dataset(tempdir.path());
    let (mut application, mut opened, _) = streaming_test_source(root).unwrap();

    set_orthographic_world_per_screen_point(&mut application, 2.5);
    opened.render_runtime.playback_lod_downshift_active = true;
    update_visible_brick_plan(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &mut opened.render_runtime,
    );

    assert_eq!(opened.render_runtime.lod_schedule.target_scale_level, 1);
    assert_eq!(opened.dataset_runtime.brick_stream_scale_level, 1);
    assert_eq!(
        opened.render_runtime.frame_fidelity.reason,
        LodDecisionReason::ScreenEquivalentCoarserScale
    );
}

#[test]
fn playback_lod_downshift_keeps_single_scale_dataset_at_s0() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let (application, mut opened, _) = streaming_test_source(root).unwrap();

    opened.render_runtime.playback_lod_downshift_active = true;
    update_visible_brick_plan(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &mut opened.render_runtime,
    );

    assert_eq!(opened.render_runtime.lod_schedule.target_scale_level, 0);
    assert_eq!(opened.dataset_runtime.brick_stream_scale_level, 0);
    assert_eq!(
        opened.render_runtime.frame_fidelity.reason,
        LodDecisionReason::ExactS0
    );
}

#[test]
fn playback_lod_downshift_still_obeys_hard_failed_s1_fallback() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_scale_budgeted_app_dataset(tempdir.path());
    let (application, mut opened, _) = streaming_test_source(root).unwrap();

    opened.render_runtime.playback_lod_downshift_active = true;
    opened.render_runtime.lod_schedule.hard_failed_scale_level = Some(1);
    opened.render_runtime.lod_schedule.hard_failure_reason = Some(LodDecisionReason::BackendLimit);
    update_visible_brick_plan(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &mut opened.render_runtime,
    );

    assert_eq!(opened.render_runtime.lod_schedule.target_scale_level, 1);
    assert_ne!(opened.dataset_runtime.brick_stream_scale_level, 1);
    assert_eq!(
        opened.render_runtime.lod_schedule.fallback_scale_level,
        Some(opened.dataset_runtime.brick_stream_scale_level)
    );
    assert_eq!(
        opened.render_runtime.frame_fidelity.reason,
        LodDecisionReason::BackendLimit
    );
}

#[test]
fn playback_command_downshifts_lod_then_restores_normal_target_on_stop() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_multiscale_app_dataset(tempdir.path());
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    opened.dataset_runtime.active_volume_u8 = None;
    opened.dataset_runtime.active_volume = None;
    opened.dataset_runtime.active_volume_f32 = None;
    let mut app = test_workbench_app_without_background_runtime(opened);
    app.dataset_runtime.brick_read_pool =
        Some(BrickReadPool::new(app.dataset_runtime.dataset.clone(), 1, 128).unwrap());
    let ctx = egui::Context::default();

    app.apply_application_command(ApplicationCommand::SetPlaybackActive(true), &ctx)
        .unwrap();

    assert!(app.application.snapshot().transient().playback_active());
    assert!(app.render_runtime.playback_lod_downshift_active);
    assert_eq!(app.render_runtime.lod_schedule.target_scale_level, 1);
    assert_eq!(app.dataset_runtime.brick_stream_scale_level, 1);
    assert_eq!(
        app.render_runtime.frame_fidelity.reason,
        LodDecisionReason::PlaybackDownshift
    );
    assert!(!app.dataset_runtime.current_brick_tickets.is_empty());
    assert!(
        app.dataset_runtime
            .current_brick_tickets
            .iter()
            .all(|ticket| ticket.scale_level == 1)
    );
    assert!(!app.dataset_runtime.prefetch_brick_tickets.is_empty());
    assert!(
        app.dataset_runtime
            .prefetch_brick_tickets
            .iter()
            .all(|ticket| ticket.scale_level == 1)
    );

    app.apply_application_command(ApplicationCommand::SetPlaybackActive(false), &ctx)
        .unwrap();

    assert!(!app.application.snapshot().transient().playback_active());
    assert!(!app.render_runtime.playback_lod_downshift_active);
    assert_eq!(app.render_runtime.lod_schedule.target_scale_level, 0);
    assert_eq!(app.dataset_runtime.brick_stream_scale_level, 0);
    assert_eq!(
        app.render_runtime.frame_fidelity.reason,
        LodDecisionReason::ExactS0
    );
    assert!(!app.dataset_runtime.current_brick_tickets.is_empty());
    assert!(
        app.dataset_runtime
            .current_brick_tickets
            .iter()
            .all(|ticket| ticket.scale_level == 0)
    );
}

#[test]
fn app_lod_candidate_visible_brick_budget_scales_with_gpu_budget() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_app_dataset(tempdir.path());
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let gib = 1024 * 1024 * 1024;
    let budget_for = |gpu_budget_bytes| {
        let policy = ResourcePolicy::new(2 * gib, gpu_budget_bytes).unwrap();
        let application = ApplicationState::new_unbound(
            SourceSessionGeneration::new(1),
            opened.catalog.as_ref().clone(),
            opened.workspace.clone(),
            policy,
        )
        .unwrap();
        lod_candidate_visible_brick_budget(&application.snapshot())
    };

    let minimum_policy_budget = budget_for(gib);
    let middle_policy_budget = budget_for(4 * gib);
    let maximum_policy_budget = budget_for(8 * gib);

    assert_eq!(minimum_policy_budget, 332);
    assert_eq!(middle_policy_budget, 1_331);
    assert_eq!(maximum_policy_budget, 2_662);
    assert!(minimum_policy_budget >= MIN_LOD_CANDIDATE_VISIBLE_BRICKS);
    assert!(maximum_policy_budget <= MAX_LOD_CANDIDATE_VISIBLE_BRICKS);
}

#[test]
fn app_submits_one_selected_budget_fallback_without_extra_lod_work() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_above_minimum_cap_app_dataset(tempdir.path());
    let (mut application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();
    set_orthographic_world_per_screen_point_for_height(
        &mut application,
        opened.render_runtime.presentation_viewport,
        288.0,
    );

    rerender_state_with_backend(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &mut opened.render_runtime,
        None,
    )
    .unwrap();

    let selected_scale = opened.dataset_runtime.brick_stream_scale_level;
    assert_eq!(
        lod_candidate_visible_brick_budget(&application.snapshot()),
        332
    );
    assert_eq!(opened.render_runtime.frame_fidelity.target_scale_level, 0);
    assert!(selected_scale > 0);
    assert_eq!(
        opened.render_runtime.lod_schedule.fallback_scale_level,
        Some(selected_scale)
    );
    assert_eq!(
        opened.render_runtime.lod_schedule.pending_scale_level,
        Some(selected_scale)
    );
    assert_eq!(
        opened.render_runtime.frame_fidelity.reason,
        LodDecisionReason::GpuBudgetLimited
    );

    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 2048).unwrap();
    let submission = submit_streaming_test_bricks(&application, &mut opened, &pool);

    assert!(submission.queued_current);
    assert!(!submission.current_tickets.is_empty());
    assert!(
        submission
            .current_tickets
            .iter()
            .all(|ticket| ticket.scale_level == selected_scale)
    );
}

#[test]
fn brick_runtime_work_active_tracks_streaming_prefetch_and_warm_counters() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_app_dataset(tempdir.path());
    let mut opened = open_dataset_and_render_first_frame(root).unwrap();
    let dataset = &mut opened.dataset_runtime;

    assert!(!brick_runtime_work_active(dataset));

    dataset.brick_stream_requested = 3;
    dataset.brick_stream_completed = 2;
    assert!(brick_runtime_work_active(dataset));
    dataset.brick_stream_failed = 1;
    assert!(!brick_runtime_work_active(dataset));

    dataset.brick_prefetch_requested = 2;
    dataset.brick_prefetch_stale = 1;
    assert!(brick_runtime_work_active(dataset));
    dataset.brick_prefetch_completed = 1;
    assert!(!brick_runtime_work_active(dataset));
}

#[test]
fn app_brick_result_drain_is_bounded_per_update() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_app_dataset(tempdir.path());
    let opened = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(opened);
    app.dataset_runtime.brick_read_pool =
        Some(BrickReadPool::new(app.dataset_runtime.dataset.clone(), 1, 16).unwrap());

    let request = app.request_visible_bricks();
    assert!(request.current_changed);
    assert_eq!(app.dataset_runtime.current_brick_tickets.len(), 8);
    let ctx = egui::Context::default();
    let mut max_completed_per_update = 0_usize;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while app.dataset_runtime.brick_stream_completed < app.dataset_runtime.brick_stream_requested {
        let before = app.dataset_runtime.brick_stream_completed;
        app.drain_brick_results(&ctx);
        let completed_this_update = app
            .dataset_runtime
            .brick_stream_completed
            .saturating_sub(before);
        max_completed_per_update = max_completed_per_update.max(completed_this_update);
        assert!(
            completed_this_update <= 2,
            "drained {completed_this_update} current bricks in one app update"
        );
        if completed_this_update == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for fixture brick reads"
        );
    }

    assert_eq!(app.dataset_runtime.brick_stream_completed, 8);
    assert_eq!(max_completed_per_update, 2);
    assert_eq!(app.dataset_runtime.brick_result_drain_limit, 2);
    assert_eq!(app.dataset_runtime.brick_result_drain_time_budget_ms, 8.0);
    assert_eq!(app.dataset_runtime.brick_result_drain_total_drained, 8);
    assert!(app.dataset_runtime.brick_result_drain_budget_hit_count > 0);
    assert_eq!(
        app.dataset_runtime
            .brick_result_drain_last_repaint_reason
            .as_deref(),
        Some("resident_frame_pending")
    );
    assert!(app.render_runtime.lod_replan_pending);
}

#[test]
fn resident_capacity_failure_requests_coarser_lod_and_visible_error() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_large_multiscale_app_dataset(tempdir.path());
    let (application, mut opened, _) = streaming_test_source(root).unwrap();
    opened.render_runtime.frame_fidelity.target_scale_level = 0;
    opened.render_runtime.frame_fidelity.displayed_scale_level = Some(0);
    opened.dataset_runtime.brick_stream_scale_level = 0;
    opened.render_runtime.lod_replan_pending = false;

    let downgraded = request_lod_downgrade_after_resident_capacity_failure(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &mut opened.render_runtime,
        ResidentRenderFailureStatus::new(
            FrameFailureKind::BudgetExceeded,
            "resident atlas exceeded configured budget",
        ),
    );

    assert!(downgraded);
    assert_eq!(
        opened.render_runtime.lod_schedule.hard_failed_scale_level,
        Some(0)
    );
    assert_eq!(
        opened.render_runtime.lod_schedule.hard_failure_reason,
        Some(LodDecisionReason::GpuBudgetLimited)
    );
    assert!(opened.render_runtime.lod_replan_pending);
    assert_eq!(
        opened.render_runtime.frame_fidelity.completeness,
        FrameCompleteness::BudgetLimited
    );
    assert_eq!(
        opened.render_runtime.frame_fidelity.reason,
        LodDecisionReason::GpuBudgetLimited
    );
    assert_eq!(
        opened.render_runtime.frame_fidelity.last_failure_kind,
        Some(FrameFailureKind::BudgetExceeded)
    );
    assert_eq!(
        opened
            .render_runtime
            .frame_fidelity
            .last_capacity_error
            .as_deref(),
        Some("resident atlas exceeded configured budget")
    );

    update_visible_brick_plan(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &mut opened.render_runtime,
    );

    assert_eq!(opened.render_runtime.frame_fidelity.target_scale_level, 0);
    assert_eq!(
        opened.render_runtime.lod_schedule.fallback_scale_level,
        Some(opened.dataset_runtime.brick_stream_scale_level)
    );
    assert!(
        opened.dataset_runtime.brick_stream_scale_level >= 1,
        "capacity failure must not immediately reselect the failed shown scale"
    );
    assert_eq!(
        opened
            .render_runtime
            .frame_fidelity
            .last_capacity_error
            .as_deref(),
        Some("resident atlas exceeded configured budget")
    );
}

#[test]
fn resident_capacity_failure_skips_failed_nonzero_scale_when_possible() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_scale_budgeted_app_dataset(tempdir.path());
    let (application, mut opened, _) = streaming_test_source(root).unwrap();
    opened.render_runtime.frame_fidelity.target_scale_level = 0;
    opened.dataset_runtime.brick_stream_scale_level = 1;
    opened.render_runtime.lod_replan_pending = false;

    let downgraded = request_lod_downgrade_after_resident_capacity_failure(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &mut opened.render_runtime,
        ResidentRenderFailureStatus::new(
            FrameFailureKind::BackendLimit,
            "resident ISO pass exceeded backend binding limit",
        ),
    );

    assert!(downgraded);
    assert_eq!(
        opened.render_runtime.lod_schedule.hard_failed_scale_level,
        Some(1),
        "the failed displayed scale should be skipped on the next plan"
    );
    assert_eq!(
        opened.render_runtime.lod_schedule.hard_failure_reason,
        Some(LodDecisionReason::BackendLimit)
    );
    assert!(opened.render_runtime.lod_replan_pending);

    update_visible_brick_plan(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &mut opened.render_runtime,
    );

    assert_ne!(opened.dataset_runtime.brick_stream_scale_level, 1);
}

#[test]
fn resident_capacity_failure_at_coarsest_scale_remains_visible() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_scale_budgeted_app_dataset(tempdir.path());
    let (application, mut opened, _) = streaming_test_source(root).unwrap();
    opened.render_runtime.frame_fidelity.target_scale_level = 0;
    opened.dataset_runtime.brick_stream_scale_level = 2;
    opened.render_runtime.lod_replan_pending = false;

    let downgraded = request_lod_downgrade_after_resident_capacity_failure(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &mut opened.render_runtime,
        ResidentRenderFailureStatus::new(
            FrameFailureKind::AllocationFailed,
            "coarsest resident DVR pass still exceeded budget",
        ),
    );

    assert!(!downgraded);
    assert_eq!(
        opened.render_runtime.lod_schedule.hard_failed_scale_level,
        None
    );
    assert!(!opened.render_runtime.lod_replan_pending);
    assert_eq!(
        opened.render_runtime.frame_fidelity.completeness,
        FrameCompleteness::BudgetLimited
    );
    assert_eq!(
        opened.render_runtime.frame_fidelity.reason,
        LodDecisionReason::AllocationFailed
    );
    assert_eq!(
        opened.render_runtime.frame_fidelity.last_failure_kind,
        Some(FrameFailureKind::AllocationFailed)
    );
    assert_eq!(
        opened
            .render_runtime
            .frame_fidelity
            .last_capacity_error
            .as_deref(),
        Some("coarsest resident DVR pass still exceeded budget")
    );
}

#[test]
fn resident_non_capacity_failure_does_not_request_lod_downgrade() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_scale_budgeted_app_dataset(tempdir.path());
    let (application, mut opened, _) = streaming_test_source(root).unwrap();
    opened.render_runtime.frame_fidelity.target_scale_level = 0;
    opened.dataset_runtime.brick_stream_scale_level = 1;
    opened.render_runtime.lod_replan_pending = false;

    let downgraded = request_lod_downgrade_after_resident_capacity_failure(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &mut opened.render_runtime,
        ResidentRenderFailureStatus::new(
            FrameFailureKind::IncompleteResidency,
            "resident brick set is incomplete",
        ),
    );

    assert!(!downgraded);
    assert_eq!(
        opened.render_runtime.lod_schedule.hard_failed_scale_level,
        None
    );
    assert!(!opened.render_runtime.lod_replan_pending);
    assert_eq!(
        opened.render_runtime.frame_fidelity.completeness,
        FrameCompleteness::Incomplete
    );
    assert_eq!(
        opened.render_runtime.frame_fidelity.reason,
        LodDecisionReason::IncompleteResidency
    );
    assert_eq!(
        opened.render_runtime.frame_fidelity.last_failure_kind,
        Some(FrameFailureKind::IncompleteResidency)
    );
}

#[test]
fn resident_gpu_errors_classify_to_typed_failure_kinds() {
    let cases = [
        (
            GpuRenderError::BudgetExceeded {
                resource: "brick atlas",
                required_bytes: 32,
                budget_bytes: 16,
            },
            FrameFailureKind::BudgetExceeded,
        ),
        (
            GpuRenderError::BufferTooLarge {
                resource: "brick atlas",
                required_bytes: 32,
                limit_bytes: 16,
            },
            FrameFailureKind::BackendLimit,
        ),
        (
            GpuRenderError::UnsupportedCameraMode("mode"),
            FrameFailureKind::InvalidModeParameter,
        ),
        (
            GpuRenderError::Render(RenderError::DimensionTooLarge {
                axis: "x",
                value: u64::from(u32::MAX) + 1,
            }),
            FrameFailureKind::BackendLimit,
        ),
        (
            GpuRenderError::Render(RenderError::InvalidViewport {
                width: 0,
                height: 1,
            }),
            FrameFailureKind::InvalidModeParameter,
        ),
    ];

    for (error, expected_kind) in cases {
        assert_eq!(frame_failure_kind_for_gpu_error(&error), expected_kind);
    }
}

#[test]
fn app_opens_large_package_without_dense_source_volume_read() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_large_multiscale_app_dataset(tempdir.path());
    let (_application, opened, _ui_runtime) = streaming_test_source(root).unwrap();
    let stats = opened.dataset_runtime.dataset.stats().unwrap();

    assert!(opened.dataset_runtime.active_volume.is_none());
    assert!(opened.dataset_runtime.active_volume_f32.is_none());
    assert_eq!(stats.subset_reads, 0);
    assert_eq!(stats.decoded_values, 0);
    assert_eq!(stats.volume_cache_misses, 0);
    assert_eq!(
        opened.render_runtime.frame_fidelity.completeness,
        FrameCompleteness::BudgetLimited
    );
    assert_eq!(
        opened.render_runtime.frame_fidelity.reason,
        LodDecisionReason::GpuBudgetLimited
    );
    assert_eq!(
        opened.render_runtime.lod_schedule.pending_scale_level,
        Some(opened.dataset_runtime.brick_stream_scale_level)
    );
}

#[test]
fn app_lod_policy_downgrades_large_visible_set_with_visible_budget_reason() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_large_multiscale_app_dataset(tempdir.path());
    let gib = 1024 * 1024 * 1024;
    let policy = ResourcePolicy::new(2 * gib, gib).unwrap();
    let (mut application, mut opened, ui_runtime) =
        streaming_test_source_with_policy(root, policy).unwrap();
    set_orthographic_world_per_screen_point_for_height(
        &mut application,
        opened.render_runtime.presentation_viewport,
        500.0,
    );

    rerender_state_with_backend(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &mut opened.render_runtime,
        None,
    )
    .unwrap();

    assert_eq!(opened.render_runtime.frame_fidelity.target_scale_level, 0);
    assert_eq!(opened.dataset_runtime.brick_stream_scale_level, 1);
    assert_eq!(
        opened.render_runtime.frame_fidelity.reason,
        LodDecisionReason::GpuBudgetLimited
    );
    assert_eq!(
        opened.render_runtime.frame_fidelity.completeness,
        FrameCompleteness::BudgetLimited
    );
}

#[test]
fn app_selects_one_budget_fallback_after_coarse_loading_frame() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_scale_budgeted_app_dataset(tempdir.path());
    let (mut application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();
    set_orthographic_world_per_screen_point_for_height(
        &mut application,
        opened.render_runtime.presentation_viewport,
        360.0,
    );

    rerender_state_with_backend(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &mut opened.render_runtime,
        None,
    )
    .unwrap();

    let selected_scale = opened.dataset_runtime.brick_stream_scale_level;
    assert!(opened.dataset_runtime.active_volume.is_none());
    assert_eq!(opened.render_runtime.frame_fidelity.target_scale_level, 0);
    assert!(selected_scale > 0);
    assert_eq!(
        opened.render_runtime.frame_fidelity.reason,
        LodDecisionReason::GpuBudgetLimited
    );
    assert_eq!(opened.render_runtime.lod_schedule.target_scale_level, 0);
    assert_eq!(
        opened.render_runtime.lod_schedule.fallback_scale_level,
        Some(selected_scale)
    );
    assert_eq!(
        opened.render_runtime.lod_schedule.pending_scale_level,
        Some(selected_scale)
    );
}

#[test]
fn app_submits_only_selected_pending_lod_after_coarse_loading_frame() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_large_multiscale_app_dataset(tempdir.path());
    let (mut application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();
    set_orthographic_world_per_screen_point_for_height(
        &mut application,
        opened.render_runtime.presentation_viewport,
        20.0,
    );
    rerender_state_with_backend(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &mut opened.render_runtime,
        None,
    )
    .unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 512).unwrap();

    assert!(opened.dataset_runtime.active_volume.is_none());
    assert_eq!(opened.render_runtime.frame_fidelity.target_scale_level, 0);
    let selected_scale = opened.dataset_runtime.brick_stream_scale_level;
    assert_eq!(
        opened.render_runtime.lod_schedule.pending_scale_level,
        Some(selected_scale)
    );

    let submission = submit_streaming_test_bricks(&application, &mut opened, &pool);
    let current = pool.recv_timeout(Duration::from_secs(2)).unwrap();

    assert!(submission.queued_current);
    assert!(!submission.current_tickets.is_empty());
    assert!(submission.prefetch_tickets.is_empty());
    assert!(
        submission
            .current_tickets
            .iter()
            .all(|ticket| ticket.scale_level == selected_scale)
    );
    assert!(
        submission
            .warm_tickets
            .iter()
            .all(|ticket| ticket.scale_level == selected_scale)
    );
    assert_eq!(current.priority, BrickRequestPriority::CurrentFrame);
    assert_eq!(current.scale_level, selected_scale);
}

#[test]
fn app_promotes_completed_pending_lod_only_after_render() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_large_multiscale_app_dataset(tempdir.path());
    let (mut application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();
    set_orthographic_world_per_screen_point_for_height(
        &mut application,
        opened.render_runtime.presentation_viewport,
        20.0,
    );
    rerender_state_with_backend(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &mut opened.render_runtime,
        None,
    )
    .unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 512).unwrap();
    let selected_scale = opened.dataset_runtime.brick_stream_scale_level;

    let submission = submit_streaming_test_bricks(&application, &mut opened, &pool);
    for _ in 0..submission.current_tickets.len() {
        let current = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(current.priority, BrickRequestPriority::CurrentFrame);
        assert_eq!(current.scale_level, selected_scale);
        assert!(apply_streaming_test_outcome(
            &application,
            &mut opened,
            current
        ));
    }

    assert!(opened.dataset_runtime.brick_stream_complete);
    assert_eq!(
        opened.dataset_runtime.brick_stream_scale_level,
        selected_scale
    );
    assert_eq!(
        opened.render_runtime.lod_schedule.displayed_scale_level,
        None
    );

    render_streaming_test_resident_frame(&application, &mut opened, &ui_runtime).unwrap();

    assert_eq!(opened.render_runtime.frame_fidelity.target_scale_level, 0);
    assert_eq!(
        opened.render_runtime.frame_fidelity.displayed_scale_level,
        Some(selected_scale)
    );
    assert_eq!(
        opened.render_runtime.lod_schedule.displayed_scale_level,
        Some(selected_scale)
    );
    assert_eq!(opened.render_runtime.lod_schedule.pending_scale_level, None);
    assert_eq!(
        opened.dataset_runtime.resident_bricks.len(),
        opened.render_runtime.visible_brick_count
    );
    assert!(
        opened
            .dataset_runtime
            .resident_bricks
            .iter()
            .all(|brick| brick.scale_level == selected_scale)
    );
}

#[test]
fn coarse_async_completion_is_ignored_when_not_current_pending_scale() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_scale_budgeted_app_dataset(tempdir.path());
    let (mut application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();
    set_orthographic_world_per_screen_point_for_height(
        &mut application,
        opened.render_runtime.presentation_viewport,
        360.0,
    );
    rerender_state_with_backend(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &mut opened.render_runtime,
        None,
    )
    .unwrap();
    let selected_scale = opened.dataset_runtime.brick_stream_scale_level;
    assert!(selected_scale > 0);
    let snapshot = application.snapshot();
    let view = application_view(&snapshot);

    let stale = BrickReadOutcome {
        request_id: mirante4d_data::DataRequestId(0),
        generation_id: mirante4d_data::DataGenerationId(
            opened.dataset_runtime.brick_stream_generation,
        ),
        layer_id: current_physical_layer_id(&opened.dataset_runtime, view.active_layer()).unwrap(),
        scale_level: selected_scale.saturating_sub(1),
        timepoint: view.timepoint(),
        brick_index: opened.render_runtime.visible_bricks[0],
        sample_region: None,
        priority: BrickRequestPriority::CurrentFrame,
        read_metrics: mirante4d_data::BrickReadMetrics::default(),
        histogram_sample: None,
        status: BrickReadStatus::Stale,
    };

    assert!(!apply_streaming_test_outcome(
        &application,
        &mut opened,
        stale
    ));
    assert_eq!(
        opened.dataset_runtime.brick_stream_scale_level,
        selected_scale
    );
    assert_eq!(opened.render_runtime.frame_fidelity.target_scale_level, 0);
    assert_eq!(
        opened.render_runtime.frame_fidelity.reason,
        LodDecisionReason::GpuBudgetLimited
    );
}

#[test]
fn app_smoke_streams_large_dataset_before_reporting_frame() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_large_multiscale_app_dataset(tempdir.path());
    let (application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();

    smoke::render_first_streamed_frame_for_smoke(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &mut opened.render_runtime,
        &mut opened.analysis_runtime,
        &ui_runtime,
        None,
        Duration::from_secs(5),
    )
    .unwrap();

    assert!(opened.dataset_runtime.brick_stream_complete);
    assert!(opened.render_runtime.diagnostics.nonzero_pixels > 0);
    assert!(opened.render_runtime.diagnostics.max_value > 0);
    assert_eq!(
        opened.render_runtime.render_backend,
        RenderBackend::CpuResidentBricks
    );
}

#[test]
fn app_streams_selected_lod_scale_into_resident_renderer() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_app_dataset(tempdir.path());
    let (mut application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();
    assert!(set_render_viewport(
        &mut opened.render_runtime,
        RenderViewport::new(64, 64).unwrap()
    ));
    assert!(set_presentation_viewport(
        &mut opened.render_runtime,
        PresentationViewport::new(64.0, 64.0).unwrap()
    ));
    set_orthographic_world_per_screen_point(&mut application, 2.5);
    rerender_state_with_backend(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &mut opened.render_runtime,
        None,
    )
    .unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 4).unwrap();

    let submission = submit_streaming_test_bricks(&application, &mut opened, &pool);
    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();

    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 1);
    assert_eq!(submission.current_tickets[0].scale_level, 1);
    assert_eq!(outcome.scale_level, 1);
    assert!(apply_streaming_test_outcome(
        &application,
        &mut opened,
        outcome
    ));
    assert_eq!(opened.dataset_runtime.resident_bricks.len(), 1);
    assert_eq!(opened.dataset_runtime.resident_bricks[0].scale_level, 1);

    render_streaming_test_resident_frame(&application, &mut opened, &ui_runtime).unwrap();

    assert_eq!(
        opened.render_runtime.render_backend,
        RenderBackend::CpuResidentBricks
    );
    assert_eq!(
        opened.render_runtime.frame.width,
        opened.render_runtime.render_viewport.width
    );
    assert_eq!(
        opened.render_runtime.frame.height,
        opened.render_runtime.render_viewport.height
    );
    assert!(opened.render_runtime.diagnostics.nonzero_pixels > 0);
}

#[test]
fn app_requeues_instead_of_rendering_stale_resident_bricks_after_camera_change() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_scale_budgeted_app_dataset(tempdir.path());
    let (mut application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();
    set_orthographic_world_per_screen_point_for_height(
        &mut application,
        opened.render_runtime.presentation_viewport,
        20.0,
    );
    rerender_state_with_backend(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &mut opened.render_runtime,
        None,
    )
    .unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 512).unwrap();

    let first_submission = submit_streaming_test_bricks(&application, &mut opened, &pool);
    assert!(first_submission.queued_current);
    for _ in 0..first_submission.current_tickets.len() {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.priority, BrickRequestPriority::CurrentFrame);
        assert!(apply_streaming_test_outcome(
            &application,
            &mut opened,
            outcome
        ));
    }
    assert!(streaming_test_frame_ready(&application, &opened));
    render_streaming_test_resident_frame(&application, &mut opened, &ui_runtime).unwrap();
    let previous_request = opened.dataset_runtime.brick_stream_request_key.clone();

    translate_streaming_test_camera_target_x(&mut application, 96.0);
    rerender_state_with_backend(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &mut opened.render_runtime,
        None,
    )
    .unwrap();

    assert_eq!(
        opened.dataset_runtime.brick_stream_request_key,
        previous_request
    );
    assert!(!streaming_test_frame_ready(&application, &opened));
    assert_ne!(
        opened.dataset_runtime.brick_stream_request_key.as_ref(),
        Some(
            &current_brick_stream_request_key(
                &application.snapshot(),
                &opened.dataset_runtime,
                &opened.render_runtime,
            )
            .unwrap()
        )
    );
    assert!(matches!(
        opened.render_runtime.frame_fidelity.completeness,
        FrameCompleteness::Loading | FrameCompleteness::BudgetLimited
    ));
    let second_submission = submit_streaming_test_bricks(&application, &mut opened, &pool);
    assert!(second_submission.current_changed);
    assert!(second_submission.queued_current);
}

#[test]
fn spatial_warm_candidates_expand_visible_set_without_duplicates() {
    let grid = Shape3D::new(3, 3, 3).unwrap();
    let center = SpatialBrickIndex::new(1, 1, 1);
    let candidates = spatial_warm_brick_candidates(&[center], grid);

    assert_eq!(candidates.len(), 26);
    assert!(!candidates.contains(&center));
    assert!(candidates.contains(&SpatialBrickIndex::new(0, 0, 0)));
    assert!(candidates.contains(&SpatialBrickIndex::new(2, 2, 2)));

    let edge = spatial_warm_brick_candidates(&[SpatialBrickIndex::new(0, 0, 0)], grid);
    assert_eq!(edge.len(), 7);
    assert!(!edge.contains(&SpatialBrickIndex::new(0, 0, 0)));
    assert!(edge.contains(&SpatialBrickIndex::new(1, 1, 1)));
}

#[test]
fn app_warms_neighboring_bricks_with_leftover_queue_budget() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_warm_spatially_chunked_app_dataset(tempdir.path());
    let (application, mut opened, _) = streaming_test_source(root).unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 10).unwrap();
    opened.render_runtime.visible_bricks = vec![SpatialBrickIndex::new(1, 1, 1)];
    opened.render_runtime.visible_brick_count = 1;

    let submission = submit_streaming_test_bricks(&application, &mut opened, &pool);
    let current = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(current.priority, BrickRequestPriority::CurrentFrame);
    assert!(apply_streaming_test_outcome(
        &application,
        &mut opened,
        current
    ));
    for _ in 0..submission.warm_tickets.len() {
        let warm = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(warm.priority, BrickRequestPriority::Warm);
        assert!(!apply_streaming_test_outcome(
            &application,
            &mut opened,
            warm
        ));
    }

    let stats = opened.dataset_runtime.dataset.stats().unwrap();

    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 1);
    assert!(submission.prefetch_tickets.is_empty());
    assert_eq!(submission.warm_tickets.len(), 9);
    assert_eq!(opened.dataset_runtime.brick_stream_requested, 1);
    assert_eq!(opened.dataset_runtime.brick_stream_completed, 1);
    assert_eq!(opened.dataset_runtime.resident_bricks.len(), 1);
    assert_eq!(opened.dataset_runtime.prefetched_brick_payloads.len(), 9);
    assert_eq!(opened.dataset_runtime.brick_warm_brick_count, 26);
    assert_eq!(opened.dataset_runtime.brick_warm_requested, 9);
    assert_eq!(opened.dataset_runtime.brick_warm_completed, 9);
    assert_eq!(opened.dataset_runtime.brick_warm_skipped, 17);
    assert_eq!(opened.dataset_runtime.brick_warm_failed, 0);
    assert_eq!(stats.brick_requests_queued, 10);
    assert_eq!(stats.brick_queue_full, 0);

    opened.render_runtime.visible_bricks = vec![SpatialBrickIndex::new(0, 0, 0)];
    opened.render_runtime.visible_brick_count = 1;
    let warmed_submission = submit_streaming_test_bricks(&application, &mut opened, &pool);

    assert!(warmed_submission.current_changed);
    assert!(!warmed_submission.queued_current);
    assert!(warmed_submission.current_tickets.is_empty());
    assert_eq!(opened.dataset_runtime.brick_stream_requested, 1);
    assert_eq!(opened.dataset_runtime.brick_stream_completed, 1);
    assert!(opened.dataset_runtime.brick_stream_complete);
    assert_eq!(opened.dataset_runtime.resident_bricks.len(), 1);
}

#[test]
fn app_cancels_and_clears_obsolete_brick_tickets() {
    let first = CancellationToken::new();
    let second = CancellationToken::new();
    let mut tickets = vec![
        BrickReadTicket {
            request_id: mirante4d_data::DataRequestId(1),
            generation_id: mirante4d_data::DataGenerationId(1),
            scale_level: 0,
            cancellation: first.clone(),
        },
        BrickReadTicket {
            request_id: mirante4d_data::DataRequestId(2),
            generation_id: mirante4d_data::DataGenerationId(1),
            scale_level: 0,
            cancellation: second.clone(),
        },
    ];

    cancel_brick_tickets(&mut tickets);

    assert!(tickets.is_empty());
    assert!(first.is_cancelled());
    assert!(second.is_cancelled());
}

#[test]
fn app_renders_completed_visible_bricks_in_iso_and_dvr_modes() {
    for mode in [RenderMode::Isosurface, RenderMode::Dvr] {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
        let (mut application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();
        let snapshot = application.snapshot();
        let view = application_view(&snapshot);
        let active_layer = view.layer(view.active_layer()).unwrap();
        let render_state = match mode {
            RenderMode::Isosurface => CanonicalRenderState::iso(
                SamplingPolicy::SmoothLinear,
                IsoShadingPolicy::Flat,
                f32::from(3_000_u16) / f32::from(u16::MAX),
            )
            .unwrap(),
            RenderMode::Dvr => CanonicalRenderState::dvr(
                SamplingPolicy::SmoothLinear,
                CanonicalDvrOpacityTransfer::new(
                    active_layer.transfer().window(),
                    active_layer.transfer().curve(),
                ),
                12.0,
            )
            .unwrap(),
            RenderMode::Mip => unreachable!(),
        };
        set_streaming_test_layer_render_state(
            &mut application,
            view.active_layer(),
            true,
            render_state,
        );
        rerender_state_with_backend(
            &application.snapshot(),
            &mut opened.dataset_runtime,
            &opened.analysis_runtime,
            &ui_runtime,
            &mut opened.render_runtime,
            None,
        )
        .unwrap();
        let dense_pixels = opened.render_runtime.frame.pixels().to_vec();
        let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 4).unwrap();

        assert!(submit_streaming_test_bricks(&application, &mut opened, &pool).queued_current);
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(apply_streaming_test_outcome(
            &application,
            &mut opened,
            outcome
        ));
        render_streaming_test_resident_frame(&application, &mut opened, &ui_runtime).unwrap();

        assert_eq!(
            opened.render_runtime.render_backend,
            RenderBackend::CpuResidentBricks
        );
        assert_eq!(
            opened.render_runtime.frame.pixels(),
            dense_pixels.as_slice()
        );
        assert!(opened.render_runtime.diagnostics.nonzero_pixels > 0);
        assert!(opened.dataset_runtime.brick_stream_complete);
    }
}

#[test]
fn resident_rendering_uses_visible_channel_render_modes() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let (mut application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();
    let layers = application_view(&application.snapshot()).layers().to_vec();
    for layer in &layers {
        let render_state = if layer.layer_key() == layers[1].layer_key() {
            CanonicalRenderState::dvr(
                SamplingPolicy::VoxelExact,
                CanonicalDvrOpacityTransfer::new(
                    layer.transfer().window(),
                    layer.transfer().curve(),
                ),
                18.0,
            )
            .unwrap()
        } else {
            *layer.render_state()
        };
        set_streaming_test_layer_render_state(
            &mut application,
            layer.layer_key(),
            true,
            render_state,
        );
    }
    rerender_state_with_backend(
        &application.snapshot(),
        &mut opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &mut opened.render_runtime,
        None,
    )
    .unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 4).unwrap();

    let submission = submit_streaming_test_bricks(&application, &mut opened, &pool);
    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 2);
    for _ in 0..submission.current_tickets.len() {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(apply_streaming_test_outcome(
            &application,
            &mut opened,
            outcome
        ));
    }
    render_streaming_test_resident_frame(&application, &mut opened, &ui_runtime).unwrap();

    assert_eq!(
        opened.render_runtime.render_backend,
        RenderBackend::CpuResidentBricks
    );
    assert_eq!(opened.render_runtime.rendered_channels.len(), 2);
    assert_eq!(
        opened.render_runtime.rendered_channels[0].layer_id.as_str(),
        "ch0"
    );
    assert_eq!(
        opened.render_runtime.rendered_channels[1].layer_id.as_str(),
        "ch1"
    );
    let final_snapshot = application.snapshot();
    let final_view = application_view(&final_snapshot);
    assert_eq!(
        final_view
            .layer(final_view.active_layer())
            .unwrap()
            .render_state()
            .mode(),
        RenderMode::Mip
    );
    assert_eq!(
        final_view
            .layer(layers[1].layer_key())
            .unwrap()
            .render_state()
            .mode(),
        RenderMode::Dvr
    );
    assert_ne!(
        opened.render_runtime.rendered_channels[0].frame.pixels(),
        opened.render_runtime.rendered_channels[1].frame.pixels()
    );
}

#[test]
fn app_requeues_visible_bricks_after_source_change() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let (mut application, mut opened, ui_runtime) = streaming_test_source(root).unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 8).unwrap();

    assert!(submit_streaming_test_bricks(&application, &mut opened, &pool).queued_current);
    let first_generation = opened.dataset_runtime.brick_stream_generation;
    let previous_view = application_view(&application.snapshot()).clone();
    application
        .dispatch(ApplicationCommand::SetTimepoint(TimeIndex::new(1)))
        .unwrap();
    let snapshot = application.snapshot();
    layer_state::reconcile_view_runtime(
        &previous_view,
        &snapshot,
        &mut opened.dataset_runtime,
        &mut opened.render_runtime,
        &mut opened.analysis_runtime,
    )
    .unwrap();
    rerender_state_with_backend(
        &snapshot,
        &mut opened.dataset_runtime,
        &opened.analysis_runtime,
        &ui_runtime,
        &mut opened.render_runtime,
        None,
    )
    .unwrap();
    assert!(submit_streaming_test_bricks(&application, &mut opened, &pool).queued_current);
    let second_generation = opened.dataset_runtime.brick_stream_generation;

    assert!(second_generation > first_generation);
    assert_eq!(
        opened.dataset_runtime.brick_stream_requested,
        opened.render_runtime.visible_brick_count * 2
    );
}

#[test]
fn app_ignores_stale_brick_worker_outcomes() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let (application, mut opened, _) = streaming_test_source(root).unwrap();
    let pool = BrickReadPool::new(opened.dataset_runtime.dataset.clone(), 1, 4).unwrap();
    assert!(submit_streaming_test_bricks(&application, &mut opened, &pool).queued_current);
    let current_generation = opened.dataset_runtime.brick_stream_generation;
    let snapshot = application.snapshot();
    let view = application_view(&snapshot);
    let stale = BrickReadOutcome {
        request_id: mirante4d_data::DataRequestId(99),
        generation_id: mirante4d_data::DataGenerationId(current_generation - 1),
        layer_id: current_physical_layer_id(&opened.dataset_runtime, view.active_layer()).unwrap(),
        scale_level: 0,
        timepoint: view.timepoint(),
        brick_index: SpatialBrickIndex::new(0, 0, 0),
        sample_region: None,
        priority: BrickRequestPriority::CurrentFrame,
        read_metrics: mirante4d_data::BrickReadMetrics::default(),
        histogram_sample: None,
        status: BrickReadStatus::Stale,
    };

    assert!(!apply_streaming_test_outcome(
        &application,
        &mut opened,
        stale
    ));

    assert_eq!(
        opened.dataset_runtime.brick_stream_generation,
        current_generation
    );
    assert_eq!(opened.dataset_runtime.brick_stream_completed, 0);
    assert_eq!(opened.dataset_runtime.brick_stream_stale, 0);
    assert!(opened.dataset_runtime.resident_bricks.is_empty());
}
