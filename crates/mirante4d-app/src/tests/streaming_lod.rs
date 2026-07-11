fn set_orthographic_world_per_screen_point_for_height(state: &mut AppState, visible_height: f64) {
    state.camera.orthographic_world_per_screen_point =
        visible_height / state.presentation_viewport.height_points;
}

fn set_orthographic_world_per_screen_point(state: &mut AppState, world_per_point: f64) {
    state.camera.orthographic_world_per_screen_point = world_per_point;
}

fn write_time_multiscale_app_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("app-time-multiscale.m4d");
    let s0_shape = Shape4D::new(3, 4, 4, 4).unwrap();
    let s1_shape = Shape4D::new(3, 2, 2, 2).unwrap();
    let s0_grid_to_world = mirante4d_core::GridToWorld::scale_um(1.0, 1.0, 1.0);
    let s1_grid_to_world = s0_grid_to_world
        .downsampled_integer_centered(2, 2, 2)
        .unwrap();
    write_native_u16_multiscale_dataset(
        &package_root,
        NativeU16MultiscaleDataset {
            id: "app-time-multiscale-fixture".to_owned(),
            name: "App time multiscale fixture".to_owned(),
            world_space: mirante4d_core::WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_core::WorldUnit::Micrometer,
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
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 4).unwrap();

    let submission = submit_visible_bricks_to_pool(&mut state, &pool);
    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 1);
    assert!(submission.prefetch_tickets.is_empty());
    assert!(!submit_visible_bricks_to_pool(&mut state, &pool).queued_current);
    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(apply_brick_read_outcome(&mut state, outcome));
    render_state_from_resident_bricks(&mut state).unwrap();
    let stats = state.dataset.stats().unwrap();

    assert_eq!(state.visible_brick_count, 1);
    assert_eq!(state.brick_stream_requested, 1);
    assert_eq!(state.brick_stream_completed, 1);
    assert_eq!(state.brick_stream_failed, 0);
    assert!(state.brick_stream_complete);
    assert_eq!(state.resident_bricks.len(), 1);
    assert_eq!(state.render_backend, RenderBackend::CpuResidentBricks);
    assert_eq!(stats.brick_requests_queued, 1);
    assert_eq!(stats.brick_requests_completed, 1);
    assert_eq!(stats.brick_reads, 1);
}

#[test]
fn app_requeues_same_visible_brick_request_when_stream_finished_incomplete() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_sparse_spatially_chunked_app_dataset(tempdir.path(), false);
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 16).unwrap();

    let first_submission = submit_visible_bricks_to_pool_with_options(
        &mut state,
        &pool,
        BrickSubmissionOptions::CURRENT_ONLY,
    );
    assert!(first_submission.queued_current);
    assert_eq!(first_submission.current_tickets.len(), 8);
    for _ in 0..first_submission.current_tickets.len() {
        let _ = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    let requested = state.brick_stream_requested;
    let first_generation = state.brick_stream_generation;
    state.brick_stream_completed = 0;
    state.brick_stream_cancelled = requested;
    state.brick_stream_stale = 0;
    state.brick_stream_failed = 0;
    state.brick_stream_complete = false;

    let retry_submission = submit_visible_bricks_to_pool_with_options(
        &mut state,
        &pool,
        BrickSubmissionOptions::CURRENT_ONLY,
    );

    assert!(retry_submission.current_changed);
    assert!(retry_submission.queued_current);
    assert_eq!(retry_submission.current_tickets.len(), requested);
    assert!(state.brick_stream_generation > first_generation);
    assert_eq!(state.brick_stream_requested, requested);
    assert_eq!(state.brick_stream_completed, 0);
    assert_eq!(state.brick_stream_cancelled, 0);
    assert!(!state.brick_stream_complete);
}

#[test]
fn app_reads_valid_zero_visible_bricks_instead_of_materializing_empty_metadata() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_sparse_spatially_chunked_app_dataset(tempdir.path(), false);
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 16).unwrap();

    assert_eq!(state.visible_bricks.len(), 8);
    let submission = submit_visible_bricks_to_pool_with_options(
        &mut state,
        &pool,
        BrickSubmissionOptions::CURRENT_ONLY,
    );

    assert!(submission.current_changed);
    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 8);
    assert_eq!(state.brick_stream_requested, 8);
    assert_eq!(state.brick_stream_completed, 0);
    assert!(!state.brick_stream_complete);
    assert!(state.resident_bricks.is_empty());

    for _ in 0..submission.current_tickets.len() {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.priority, BrickRequestPriority::CurrentFrame);
        assert!(apply_brick_read_outcome(&mut state, outcome));
    }
    render_state_from_resident_bricks(&mut state).unwrap();
    let stats = state.dataset.stats().unwrap();

    assert_eq!(state.brick_stream_completed, 8);
    assert!(state.brick_stream_complete);
    assert_eq!(state.resident_bricks.len(), 8);
    assert_eq!(state.render_backend, RenderBackend::CpuResidentBricks);
    assert!(state.diagnostics.nonzero_pixels > 0);
    assert_eq!(stats.brick_requests_queued, 8);
    assert_eq!(stats.brick_requests_completed, 8);
    assert_eq!(stats.brick_reads, 8);
}

#[test]
fn batched_brick_outcome_apply_preserves_resident_ordering() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_sparse_spatially_chunked_app_dataset(tempdir.path(), false);
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 16).unwrap();
    let submission = submit_visible_bricks_to_pool_with_options(
        &mut state,
        &pool,
        BrickSubmissionOptions::CURRENT_ONLY,
    );

    let mut outcomes = Vec::new();
    for _ in 0..submission.current_tickets.len() {
        outcomes.push(pool.recv_timeout(Duration::from_secs(2)).unwrap());
    }
    outcomes.reverse();
    assert!(apply_brick_read_outcomes(&mut state, outcomes));

    let resident_order = state
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
    assert_eq!(state.brick_stream_completed, 8);
    assert!(state.brick_stream_complete);
    assert_eq!(state.resident_bricks.len(), 8);
    assert_eq!(
        state
            .resident_bricks_by_layer
            .get("ch0")
            .map(Vec::len)
            .unwrap_or_default(),
        8
    );
}

#[test]
fn request_visible_bricks_does_not_render_as_side_effect() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_sparse_spatially_chunked_app_dataset(tempdir.path(), false);
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 16).unwrap();
    let submission = submit_visible_bricks_to_pool_with_options(
        &mut state,
        &pool,
        BrickSubmissionOptions::CURRENT_ONLY,
    );
    let mut outcomes = Vec::new();
    for _ in 0..submission.current_tickets.len() {
        outcomes.push(pool.recv_timeout(Duration::from_secs(2)).unwrap());
    }
    assert!(apply_brick_read_outcomes(&mut state, outcomes));
    assert!(current_resident_frame_ready(&state));

    let mut app = test_workbench_app_without_background_runtime(state);
    app.brick_read_pool = Some(pool);
    app.state.render_backend = RenderBackend::Loading;

    let outcome = app.request_visible_bricks();

    assert!(!outcome.current_changed);
    assert!(!outcome.resident_changed);
    assert!(outcome.current_frame_ready);
    assert_eq!(app.state.render_backend, RenderBackend::Loading);
}

#[test]
fn request_visible_bricks_replans_when_current_request_is_already_resident() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_sparse_spatially_chunked_app_dataset(tempdir.path(), false);
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 16).unwrap();
    let submission = submit_visible_bricks_to_pool_with_options(
        &mut state,
        &pool,
        BrickSubmissionOptions::CURRENT_ONLY,
    );
    let mut outcomes = Vec::new();
    for _ in 0..submission.current_tickets.len() {
        outcomes.push(pool.recv_timeout(Duration::from_secs(2)).unwrap());
    }
    assert!(apply_brick_read_outcomes(&mut state, outcomes));
    assert!(current_resident_frame_ready(&state));

    let mut app = test_workbench_app_without_background_runtime(state);
    app.brick_read_pool = Some(pool);
    app.state.render_backend = RenderBackend::Loading;
    app.state.lod_replan_pending = false;
    app.state.brick_stream_request_key = None;

    let outcome = app.request_visible_bricks();

    assert!(outcome.current_changed);
    assert!(!outcome.resident_changed);
    assert!(outcome.current_frame_ready);
    assert!(app.state.lod_replan_pending);
    assert_eq!(app.state.render_backend, RenderBackend::Loading);
}

#[test]
fn app_reads_all_valid_zero_visible_bricks_before_reporting_complete() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_sparse_spatially_chunked_app_dataset(tempdir.path(), true);
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 16).unwrap();

    assert_eq!(state.visible_bricks.len(), 8);
    let submission = submit_visible_bricks_to_pool(&mut state, &pool);
    for _ in 0..submission.current_tickets.len() {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.priority, BrickRequestPriority::CurrentFrame);
        assert!(apply_brick_read_outcome(&mut state, outcome));
    }
    render_state_from_resident_bricks(&mut state).unwrap();
    let stats = state.dataset.stats().unwrap();

    assert!(submission.current_changed);
    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 8);
    assert_eq!(state.brick_stream_requested, 8);
    assert_eq!(state.brick_stream_completed, 8);
    assert!(state.brick_stream_complete);
    assert_eq!(state.resident_bricks.len(), 8);
    assert_eq!(state.diagnostics.nonzero_pixels, 0);
    assert_eq!(stats.brick_requests_queued, 8);
    assert_eq!(stats.brick_reads, 8);
}

#[test]
fn app_resident_renderer_composites_visible_channel_layers() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 2).unwrap();

    let submission = submit_visible_bricks_to_pool(&mut state, &pool);

    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 2);
    assert!(submission.prefetch_tickets.is_empty());
    for _ in 0..2 {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.priority, BrickRequestPriority::CurrentFrame);
        assert!(apply_brick_read_outcome(&mut state, outcome));
    }
    assert!(state.brick_stream_complete);

    render_state_from_resident_bricks(&mut state).unwrap();

    assert_eq!(state.render_backend, RenderBackend::CpuResidentBricks);
    assert_eq!(state.rendered_channels.len(), 2);
    assert_eq!(state.rendered_channels[0].layer_id, "ch0");
    assert_eq!(state.rendered_channels[1].layer_id, "ch1");
    assert_eq!(state.resident_bricks.len(), 1);
    assert_eq!(state.resident_bricks_by_layer.len(), 2);
    assert_eq!(
        state.rendered_channels[0].frame.pixels(),
        state.frame.pixels()
    );

    let active_only = crate::image_compositing::mip_to_color_image_with_color(
        &state.frame,
        state.active_layer_display,
        state.active_layer_color,
    );
    let composited = color_image_for_state(&state);
    assert_ne!(composited.pixels, active_only.pixels);
}

#[test]
fn app_resident_dvr_uses_same_ray_frame_not_per_channel_composite() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.active_render_mode = RenderMode::Dvr;
    state.dvr_density_scale = 12.0;
    crate::layer_state::sync_active_layer_render_state_from_runtime(&mut state);
    state.layers[1].render_state = ChannelRenderState::for_mode(
        RenderMode::Dvr,
        state.render_sampling_policy,
        state.render_iso_shading_policy,
        state.iso_display_level,
        default_dvr_opacity_transfer(state.layers[1].display),
        state.dvr_density_scale,
    );
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 2).unwrap();

    let submission = submit_visible_bricks_to_pool(&mut state, &pool);
    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 2);
    for _ in 0..2 {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.priority, BrickRequestPriority::CurrentFrame);
        assert!(apply_brick_read_outcome(&mut state, outcome));
    }
    assert!(state.brick_stream_complete);

    render_state_from_resident_bricks(&mut state).unwrap();

    assert_eq!(state.render_backend, RenderBackend::CpuResidentBricks);
    assert_eq!(state.rendered_channels.len(), 2);
    assert!(state.frame.dvr_rgba().is_some());
    assert!(
        state
            .rendered_channels
            .iter()
            .all(|channel| channel.frame.dvr_rgba().is_none())
    );
    let image = color_image_for_state(&state);
    assert_eq!(
        image.size,
        [
            state.render_viewport.width as usize,
            state.render_viewport.height as usize,
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
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 8).unwrap();

    let submission = submit_visible_bricks_to_pool(&mut state, &pool);
    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 2);
    assert_eq!(submission.prefetch_tickets.len(), 2);

    for _ in 0..2 {
        let current = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(current.priority, BrickRequestPriority::CurrentFrame);
        assert!(apply_brick_read_outcome(&mut state, current));
    }
    for _ in 0..2 {
        let prefetch = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(prefetch.priority, BrickRequestPriority::Prefetch);
        assert!(!apply_brick_read_outcome(&mut state, prefetch));
    }
    let stats_before_cache_probe = state.dataset.stats().unwrap();
    let layer_id = LayerId::new(state.active_layer_id.clone()).unwrap();
    let _ = state
        .dataset
        .read_u16_brick(&layer_id, TimeIndex(1), SpatialBrickIndex::new(0, 0, 0))
        .unwrap();
    let stats_after_cache_probe = state.dataset.stats().unwrap();

    assert_eq!(state.brick_stream_requested, 2);
    assert_eq!(state.brick_stream_completed, 2);
    assert!(state.brick_stream_complete);
    assert_eq!(state.resident_bricks.len(), 1);
    assert_eq!(state.resident_bricks_by_layer.len(), 2);
    assert_eq!(
        state.brick_prefetch_timepoints,
        vec![TimeIndex(1), TimeIndex(2)]
    );
    assert_eq!(state.brick_prefetch_requested, 2);
    assert_eq!(state.brick_prefetch_completed, 2);
    assert_eq!(state.brick_prefetch_failed, 0);
    assert_eq!(state.brick_prefetch_skipped, 0);
    assert_eq!(stats_before_cache_probe.brick_requests_queued, 4);
    assert_eq!(stats_before_cache_probe.brick_requests_completed, 4);
    assert!(stats_after_cache_probe.brick_cache_hits > stats_before_cache_probe.brick_cache_hits);
}

#[test]
fn prefetched_next_timepoint_bricks_promote_without_fresh_current_reads() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.active_volume = None;
    state.active_volume_f32 = None;
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 32).unwrap();

    let initial_submission = submit_visible_bricks_to_pool(&mut state, &pool);
    assert!(initial_submission.queued_current);
    assert_eq!(initial_submission.current_tickets.len(), 8);
    assert_eq!(initial_submission.prefetch_tickets.len(), 16);

    for _ in 0..24 {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        apply_brick_read_outcome(&mut state, outcome);
    }
    assert!(state.brick_stream_complete);
    assert_eq!(state.prefetched_brick_payloads.len(), 16);

    activate_streaming_timepoint_preserving_frame(&mut state, TimeIndex(1)).unwrap();
    let promoted_submission = submit_visible_bricks_to_pool(&mut state, &pool);

    assert!(promoted_submission.current_changed);
    assert!(!promoted_submission.queued_current);
    assert!(promoted_submission.current_tickets.is_empty());
    assert_eq!(state.brick_stream_requested, 8);
    assert_eq!(state.brick_stream_completed, 8);
    assert!(state.brick_stream_complete);
    assert_eq!(state.resident_bricks.len(), 8);
    assert_eq!(state.active_timepoint, TimeIndex(1));

    render_state_from_resident_bricks(&mut state).unwrap();
    assert!(state.diagnostics.nonzero_pixels > 0);
}

#[test]
fn visible_plan_change_reuses_compatible_resident_bricks_without_blank_reload() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.active_volume = None;
    state.active_volume_f32 = None;
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 32).unwrap();

    let initial_submission = submit_visible_bricks_to_pool(&mut state, &pool);
    assert!(initial_submission.queued_current);
    for _ in 0..initial_submission.current_tickets.len() {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(apply_brick_read_outcome(&mut state, outcome));
    }
    assert!(state.brick_stream_complete);
    assert_eq!(state.resident_bricks.len(), 8);

    let retained_brick = state.visible_bricks[0];
    state.visible_bricks = vec![retained_brick];
    state.visible_brick_count = 1;
    state.brick_stream_request_key = None;
    let subset_submission = submit_visible_bricks_to_pool(&mut state, &pool);

    assert!(subset_submission.current_changed);
    assert!(!subset_submission.queued_current);
    assert!(subset_submission.current_tickets.is_empty());
    assert_eq!(state.brick_stream_requested, 1);
    assert_eq!(state.brick_stream_completed, 1);
    assert!(state.brick_stream_complete);
    assert_eq!(state.resident_bricks.len(), 1);
    assert_eq!(state.resident_bricks[0].brick_index, retained_brick);
}

#[test]
fn playback_submission_prioritizes_current_and_prefetch_over_warm_work() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.active_volume = None;
    state.active_volume_f32 = None;
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 32).unwrap();

    let submission = submit_visible_bricks_to_pool_with_options(
        &mut state,
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
fn playback_scheduler_steps_even_when_next_prefetch_is_not_ready() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    let ctx = egui::Context::default();
    let mut commands = Vec::new();

    app.state.active_volume = None;
    app.state.active_volume_f32 = None;
    app.state.brick_stream_complete = false;
    app.state.brick_prefetch_timepoints = vec![TimeIndex(1)];
    app.state.brick_prefetch_requested = 8;
    app.state.brick_prefetch_completed = 0;
    app.playback.playing = true;
    app.playback.last_step_at = Some(Instant::now() - app.playback.frame_interval);

    app.enqueue_playback_command_if_due(&mut commands, &ctx);

    assert_eq!(commands, vec![WorkbenchCommand::StepTimepoint { delta: 1 }]);
}

#[test]
fn playback_scheduler_waits_after_step_until_requested_timepoint_finishes() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    let ctx = egui::Context::default();
    let mut commands = Vec::new();

    app.state.active_timepoint = TimeIndex(1);
    app.state.active_volume = None;
    app.state.active_volume_f32 = None;
    app.state.brick_stream_requested = 8;
    app.state.brick_stream_completed = 4;
    app.state.brick_stream_complete = false;
    app.playback.playing = true;
    app.playback.waiting_for_timepoint = Some(TimeIndex(1));
    app.playback.last_step_at = Some(Instant::now() - app.playback.frame_interval);

    app.enqueue_playback_command_if_due(&mut commands, &ctx);

    assert!(commands.is_empty());
    assert_eq!(app.playback.waiting_for_timepoint, Some(TimeIndex(1)));

    app.state.brick_stream_completed = 8;
    app.enqueue_playback_command_if_due(&mut commands, &ctx);

    assert!(commands.is_empty());
    assert_eq!(app.playback.waiting_for_timepoint, None);

    app.playback.last_step_at = Some(Instant::now() - app.playback.frame_interval);
    app.enqueue_playback_command_if_due(&mut commands, &ctx);

    assert_eq!(commands, vec![WorkbenchCommand::StepTimepoint { delta: 1 }]);
}

#[test]
fn prefetch_timepoints_wrap_for_looping_playback() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();

    state.active_timepoint = TimeIndex(1);
    assert_eq!(
        prefetch_timepoints_for_state(&state, 2),
        vec![TimeIndex(2), TimeIndex(0)]
    );

    state.active_timepoint = TimeIndex(2);
    assert_eq!(
        prefetch_timepoints_for_state(&state, 2),
        vec![TimeIndex(0), TimeIndex(1)]
    );
}

#[test]
fn app_limits_prefetch_to_queue_capacity_after_current_frame_work() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 10).unwrap();

    assert_eq!(state.visible_bricks.len(), 8);
    let submission = submit_visible_bricks_to_pool(&mut state, &pool);
    let stats = state.dataset.stats().unwrap();

    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 8);
    assert_eq!(submission.prefetch_tickets.len(), 2);
    assert!(submission.warm_tickets.is_empty());
    assert_eq!(state.brick_stream_requested, 8);
    assert_eq!(state.brick_prefetch_requested, 2);
    assert_eq!(state.brick_prefetch_skipped, 14);
    assert_eq!(state.brick_warm_requested, 0);
    assert_eq!(state.brick_prefetch_timepoints, vec![TimeIndex(1)]);
    assert_eq!(state.brick_prefetch_failed, 0);
    assert_eq!(stats.brick_requests_queued, 10);
    assert_eq!(stats.brick_queue_full, 0);
}

#[test]
fn app_selects_stream_scale_from_camera_pixel_footprint() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let layer_id = LayerId::new(state.active_layer_id.clone()).unwrap();

    assert_eq!(select_stream_scale_for_state(&state, &layer_id).unwrap(), 0);

    set_orthographic_world_per_screen_point(&mut state, 2.5);
    rerender_state_with_backend(&mut state, None).unwrap();

    assert_eq!(state.brick_stream_scale_level, 1);
    assert_eq!(
        state.brick_stream_scale_shape,
        Shape3D::new(2, 2, 2).unwrap()
    );
    assert_eq!(state.visible_bricks, vec![SpatialBrickIndex::new(0, 0, 0)]);

    set_orthographic_world_per_screen_point(&mut state, 0.5);
    rerender_state_with_backend(&mut state, None).unwrap();

    assert_eq!(state.brick_stream_scale_level, 0);
    assert_eq!(
        state.brick_stream_scale_shape,
        Shape3D::new(4, 4, 4).unwrap()
    );
}

#[test]
fn playback_lod_downshift_selects_s1_for_normal_s0_view() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let layer_id = LayerId::new(state.active_layer_id.clone()).unwrap();

    assert_eq!(select_stream_scale_for_state(&state, &layer_id).unwrap(), 0);

    state.playback_lod_downshift_active = true;
    update_visible_brick_plan(&mut state);

    assert_eq!(state.lod_schedule.target_scale_level, 1);
    assert_eq!(state.brick_stream_scale_level, 1);
    assert_eq!(state.frame_fidelity.target_scale_level, 1);
    assert_eq!(
        state.frame_fidelity.reason,
        LodDecisionReason::PlaybackDownshift
    );
}

#[test]
fn playback_lod_downshift_does_not_coarsen_normal_s1_view() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();

    set_orthographic_world_per_screen_point(&mut state, 2.5);
    state.playback_lod_downshift_active = true;
    update_visible_brick_plan(&mut state);

    assert_eq!(state.lod_schedule.target_scale_level, 1);
    assert_eq!(state.brick_stream_scale_level, 1);
    assert_eq!(
        state.frame_fidelity.reason,
        LodDecisionReason::ScreenEquivalentCoarserScale
    );
}

#[test]
fn playback_lod_downshift_keeps_single_scale_dataset_at_s0() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_spatially_chunked_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();

    state.playback_lod_downshift_active = true;
    update_visible_brick_plan(&mut state);

    assert_eq!(state.lod_schedule.target_scale_level, 0);
    assert_eq!(state.brick_stream_scale_level, 0);
    assert_eq!(state.frame_fidelity.reason, LodDecisionReason::ExactS0);
}

#[test]
fn playback_lod_downshift_still_obeys_hard_failed_s1_fallback() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_scale_budgeted_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();

    state.playback_lod_downshift_active = true;
    state.lod_schedule.hard_failed_scale_level = Some(1);
    state.lod_schedule.hard_failure_reason = Some(LodDecisionReason::BackendLimit);
    update_visible_brick_plan(&mut state);

    assert_eq!(state.lod_schedule.target_scale_level, 1);
    assert_ne!(state.brick_stream_scale_level, 1);
    assert_eq!(
        state.lod_schedule.fallback_scale_level,
        Some(state.brick_stream_scale_level)
    );
    assert_eq!(state.frame_fidelity.reason, LodDecisionReason::BackendLimit);
}

#[test]
fn slow_frame_time_samples_do_not_mutate_lod_selection() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let layer_id = LayerId::new(state.active_layer_id.clone()).unwrap();
    let before_schedule = state.lod_schedule;
    let before_plan = select_stream_scale_for_state(&state, &layer_id).unwrap();

    state.fixed_frame_time_ms_for_snapshots = Some(250.0);
    record_completed_frame_time(&mut state, Instant::now());
    let after_plan = select_stream_scale_for_state(&state, &layer_id).unwrap();

    assert_eq!(state.frame_fidelity.frame_time_ms, Some(250.0));
    assert_eq!(state.lod_schedule, before_schedule);
    assert_eq!(after_plan, before_plan);
    assert!(!state.lod_replan_pending);
}

#[test]
fn fast_frame_time_samples_do_not_mutate_lod_selection() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let layer_id = LayerId::new(state.active_layer_id.clone()).unwrap();
    let before_schedule = state.lod_schedule;
    let before_plan = select_stream_scale_for_state(&state, &layer_id).unwrap();

    state.fixed_frame_time_ms_for_snapshots = Some(1.0);
    record_completed_frame_time(&mut state, Instant::now());
    let after_plan = select_stream_scale_for_state(&state, &layer_id).unwrap();

    assert_eq!(state.frame_fidelity.frame_time_ms, Some(1.0));
    assert_eq!(state.lod_schedule, before_schedule);
    assert_eq!(after_plan, before_plan);
    assert!(!state.lod_replan_pending);
}

#[test]
fn playback_command_downshifts_lod_then_restores_normal_target_on_stop() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_time_multiscale_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.active_volume = None;
    state.active_volume_f32 = None;
    let mut app = test_workbench_app_without_background_runtime(state);
    app.brick_read_pool = Some(BrickReadPool::new(app.state.dataset.clone(), 1, 128).unwrap());
    let ctx = egui::Context::default();

    app.apply_workbench_command(WorkbenchCommand::SetPlayback { playing: true }, &ctx);

    assert!(app.playback.playing);
    assert!(app.state.playback_lod_downshift_active);
    assert_eq!(app.state.lod_schedule.target_scale_level, 1);
    assert_eq!(app.state.brick_stream_scale_level, 1);
    assert_eq!(
        app.state.frame_fidelity.reason,
        LodDecisionReason::PlaybackDownshift
    );
    assert!(
        app.current_brick_tickets
            .iter()
            .all(|ticket| ticket.scale_level == 1)
    );
    assert!(
        app.prefetch_brick_tickets
            .iter()
            .all(|ticket| ticket.scale_level == 1)
    );

    app.apply_workbench_command(WorkbenchCommand::SetPlayback { playing: false }, &ctx);

    assert!(!app.playback.playing);
    assert!(!app.state.playback_lod_downshift_active);
    assert_eq!(app.state.lod_schedule.target_scale_level, 0);
    assert_eq!(app.state.brick_stream_scale_level, 0);
    assert_eq!(app.state.frame_fidelity.reason, LodDecisionReason::ExactS0);
    assert!(
        app.current_brick_tickets
            .iter()
            .all(|ticket| ticket.scale_level == 0)
    );
}

#[test]
fn app_lod_candidate_visible_brick_budget_scales_with_gpu_budget() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();

    state.renderer_gpu_brick_budget_bytes = 64 * APP_MIB;
    assert_eq!(
        lod_candidate_visible_brick_budget(&state),
        MIN_LOD_CANDIDATE_VISIBLE_BRICKS
    );

    state.renderer_gpu_brick_budget_bytes = 2 * APP_GIB;
    assert_eq!(lod_candidate_visible_brick_budget(&state), 1024);

    state.renderer_gpu_brick_budget_bytes = 64 * APP_GIB;
    assert_eq!(
        lod_candidate_visible_brick_budget(&state),
        MAX_LOD_CANDIDATE_VISIBLE_BRICKS
    );
}

#[test]
fn app_submits_one_selected_budget_fallback_without_extra_lod_work() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_above_minimum_cap_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    set_orthographic_world_per_screen_point_for_height(&mut state, 288.0);

    rerender_state_with_backend(&mut state, None).unwrap();

    let selected_scale = state.brick_stream_scale_level;
    assert_eq!(lod_candidate_visible_brick_budget(&state), 1024);
    assert_eq!(state.frame_fidelity.target_scale_level, 0);
    assert!(selected_scale > 0);
    assert_eq!(
        state.lod_schedule.fallback_scale_level,
        Some(selected_scale)
    );
    assert_eq!(state.lod_schedule.pending_scale_level, Some(selected_scale));
    assert_eq!(
        state.frame_fidelity.reason,
        LodDecisionReason::GpuBudgetLimited
    );

    let pool = BrickReadPool::new(state.dataset.clone(), 1, 2048).unwrap();
    let submission = submit_visible_bricks_to_pool(&mut state, &pool);

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
    let mut state = open_dataset_and_render_first_frame(root).unwrap();

    assert!(!brick_runtime_work_active(&state));

    state.brick_stream_requested = 3;
    state.brick_stream_completed = 2;
    assert!(brick_runtime_work_active(&state));
    state.brick_stream_failed = 1;
    assert!(!brick_runtime_work_active(&state));

    state.brick_prefetch_requested = 2;
    state.brick_prefetch_stale = 1;
    assert!(brick_runtime_work_active(&state));
    state.brick_prefetch_completed = 1;
    assert!(!brick_runtime_work_active(&state));
}

#[test]
fn app_brick_result_drain_is_bounded_per_update() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_app_dataset(tempdir.path());
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    app.brick_read_pool = Some(BrickReadPool::new(app.state.dataset.clone(), 1, 16).unwrap());
    let pool = app.brick_read_pool.as_ref().unwrap();

    let submission = submit_visible_bricks_to_pool(&mut app.state, pool);
    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 8);
    let ctx = egui::Context::default();
    let mut max_completed_per_update = 0_usize;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while app.state.brick_stream_completed < app.state.brick_stream_requested {
        let before = app.state.brick_stream_completed;
        app.drain_brick_results(&ctx);
        let completed_this_update = app.state.brick_stream_completed.saturating_sub(before);
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

    assert_eq!(app.state.brick_stream_completed, 8);
    assert_eq!(max_completed_per_update, 2);
    assert_eq!(app.state.brick_result_drain_limit, 2);
    assert_eq!(app.state.brick_result_drain_time_budget_ms, 8.0);
    assert_eq!(app.state.brick_result_drain_total_drained, 8);
    assert!(app.state.brick_result_drain_budget_hit_count > 0);
    assert_eq!(
        app.state.brick_result_drain_last_repaint_reason.as_deref(),
        Some("resident_frame_pending")
    );
    assert!(app.state.lod_replan_pending);
}

#[test]
fn resident_capacity_failure_requests_coarser_lod_and_visible_error() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_large_multiscale_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.frame_fidelity.target_scale_level = 0;
    state.frame_fidelity.displayed_scale_level = Some(0);
    state.brick_stream_scale_level = 0;
    state.lod_replan_pending = false;

    let downgraded = request_lod_downgrade_after_resident_capacity_failure(
        &mut state,
        ResidentRenderFailureStatus::new(
            FrameFailureKind::BudgetExceeded,
            "resident atlas exceeded configured budget",
        ),
    );

    assert!(downgraded);
    assert_eq!(state.lod_schedule.hard_failed_scale_level, Some(0));
    assert_eq!(
        state.lod_schedule.hard_failure_reason,
        Some(LodDecisionReason::GpuBudgetLimited)
    );
    assert!(state.lod_replan_pending);
    assert_eq!(
        state.frame_fidelity.completeness,
        FrameCompleteness::BudgetLimited
    );
    assert_eq!(
        state.frame_fidelity.reason,
        LodDecisionReason::GpuBudgetLimited
    );
    assert_eq!(
        state.frame_fidelity.last_failure_kind,
        Some(FrameFailureKind::BudgetExceeded)
    );
    assert_eq!(
        state.frame_fidelity.last_capacity_error.as_deref(),
        Some("resident atlas exceeded configured budget")
    );

    update_visible_brick_plan(&mut state);

    assert_eq!(state.frame_fidelity.target_scale_level, 0);
    assert_eq!(
        state.lod_schedule.fallback_scale_level,
        Some(state.brick_stream_scale_level)
    );
    assert!(
        state.brick_stream_scale_level >= 1,
        "capacity failure must not immediately reselect the failed shown scale"
    );
    assert_eq!(
        state.frame_fidelity.last_capacity_error.as_deref(),
        Some("resident atlas exceeded configured budget")
    );
}

#[test]
fn resident_capacity_failure_skips_failed_nonzero_scale_when_possible() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_scale_budgeted_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.frame_fidelity.target_scale_level = 0;
    state.brick_stream_scale_level = 1;
    state.lod_replan_pending = false;

    let downgraded = request_lod_downgrade_after_resident_capacity_failure(
        &mut state,
        ResidentRenderFailureStatus::new(
            FrameFailureKind::BackendLimit,
            "resident ISO pass exceeded backend binding limit",
        ),
    );

    assert!(downgraded);
    assert_eq!(
        state.lod_schedule.hard_failed_scale_level,
        Some(1),
        "the failed displayed scale should be skipped on the next plan"
    );
    assert_eq!(
        state.lod_schedule.hard_failure_reason,
        Some(LodDecisionReason::BackendLimit)
    );
    assert!(state.lod_replan_pending);

    update_visible_brick_plan(&mut state);

    assert_ne!(state.brick_stream_scale_level, 1);
}

#[test]
fn resident_capacity_failure_at_coarsest_scale_remains_visible() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_scale_budgeted_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.frame_fidelity.target_scale_level = 0;
    state.brick_stream_scale_level = 2;
    state.lod_replan_pending = false;

    let downgraded = request_lod_downgrade_after_resident_capacity_failure(
        &mut state,
        ResidentRenderFailureStatus::new(
            FrameFailureKind::AllocationFailed,
            "coarsest resident DVR pass still exceeded budget",
        ),
    );

    assert!(!downgraded);
    assert_eq!(state.lod_schedule.hard_failed_scale_level, None);
    assert!(!state.lod_replan_pending);
    assert_eq!(
        state.frame_fidelity.completeness,
        FrameCompleteness::BudgetLimited
    );
    assert_eq!(
        state.frame_fidelity.reason,
        LodDecisionReason::AllocationFailed
    );
    assert_eq!(
        state.frame_fidelity.last_failure_kind,
        Some(FrameFailureKind::AllocationFailed)
    );
    assert_eq!(
        state.frame_fidelity.last_capacity_error.as_deref(),
        Some("coarsest resident DVR pass still exceeded budget")
    );
}

#[test]
fn resident_non_capacity_failure_does_not_request_lod_downgrade() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_scale_budgeted_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.frame_fidelity.target_scale_level = 0;
    state.brick_stream_scale_level = 1;
    state.lod_replan_pending = false;

    let downgraded = request_lod_downgrade_after_resident_capacity_failure(
        &mut state,
        ResidentRenderFailureStatus::new(
            FrameFailureKind::IncompleteResidency,
            "resident brick set is incomplete",
        ),
    );

    assert!(!downgraded);
    assert_eq!(state.lod_schedule.hard_failed_scale_level, None);
    assert!(!state.lod_replan_pending);
    assert_eq!(
        state.frame_fidelity.completeness,
        FrameCompleteness::Incomplete
    );
    assert_eq!(
        state.frame_fidelity.reason,
        LodDecisionReason::IncompleteResidency
    );
    assert_eq!(
        state.frame_fidelity.last_failure_kind,
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
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let stats = state.dataset.stats().unwrap();

    assert!(state.active_volume.is_none());
    assert!(state.active_volume_f32.is_none());
    assert_eq!(stats.subset_reads, 0);
    assert_eq!(stats.decoded_values, 0);
    assert_eq!(stats.volume_cache_misses, 0);
    assert_eq!(
        state.frame_fidelity.completeness,
        FrameCompleteness::Loading
    );
    assert_eq!(
        state.lod_schedule.pending_scale_level,
        Some(state.brick_stream_scale_level)
    );
}

#[test]
fn app_lod_policy_downgrades_large_visible_set_with_visible_budget_reason() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_large_multiscale_app_dataset(tempdir.path());
    let preferences = AppPreferences {
        format: PREFERENCES_FORMAT.to_owned(),
        runtime: AppRuntimePreferences {
            gpu_brick_cache_budget_bytes: 64 * APP_MIB,
            ..AppRuntimePreferences::default()
        },
    };
    let mut state =
        open_dataset_with_preferences_and_render_first_frame(root, &preferences).unwrap();
    set_orthographic_world_per_screen_point_for_height(&mut state, 500.0);

    rerender_state_with_backend(&mut state, None).unwrap();

    assert_eq!(state.frame_fidelity.target_scale_level, 0);
    assert_eq!(state.brick_stream_scale_level, 1);
    assert_eq!(
        state.frame_fidelity.reason,
        LodDecisionReason::GpuBudgetLimited
    );
    assert_eq!(
        state.frame_fidelity.completeness,
        FrameCompleteness::BudgetLimited
    );
}

#[test]
fn app_selects_one_budget_fallback_after_coarse_loading_frame() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_scale_budgeted_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    set_orthographic_world_per_screen_point_for_height(&mut state, 360.0);

    rerender_state_with_backend(&mut state, None).unwrap();

    let selected_scale = state.brick_stream_scale_level;
    assert!(state.active_volume.is_none());
    assert_eq!(state.frame_fidelity.target_scale_level, 0);
    assert!(selected_scale > 0);
    assert_eq!(
        state.frame_fidelity.reason,
        LodDecisionReason::GpuBudgetLimited
    );
    assert_eq!(state.lod_schedule.target_scale_level, 0);
    assert_eq!(
        state.lod_schedule.fallback_scale_level,
        Some(selected_scale)
    );
    assert_eq!(state.lod_schedule.pending_scale_level, Some(selected_scale));
}

#[test]
fn app_submits_only_selected_pending_lod_after_coarse_loading_frame() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_large_multiscale_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    set_orthographic_world_per_screen_point_for_height(&mut state, 20.0);
    rerender_state_with_backend(&mut state, None).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 512).unwrap();

    assert!(state.active_volume.is_none());
    assert_eq!(state.frame_fidelity.target_scale_level, 0);
    let selected_scale = state.brick_stream_scale_level;
    assert_eq!(state.lod_schedule.pending_scale_level, Some(selected_scale));

    let submission = submit_visible_bricks_to_pool(&mut state, &pool);
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
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    set_orthographic_world_per_screen_point_for_height(&mut state, 20.0);
    rerender_state_with_backend(&mut state, None).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 512).unwrap();
    let selected_scale = state.brick_stream_scale_level;

    let submission = submit_visible_bricks_to_pool(&mut state, &pool);
    for _ in 0..submission.current_tickets.len() {
        let current = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(current.priority, BrickRequestPriority::CurrentFrame);
        assert_eq!(current.scale_level, selected_scale);
        assert!(apply_brick_read_outcome(&mut state, current));
    }

    assert!(state.brick_stream_complete);
    assert_eq!(state.brick_stream_scale_level, selected_scale);
    assert_eq!(state.lod_schedule.displayed_scale_level, None);

    render_state_from_resident_bricks(&mut state).unwrap();

    assert_eq!(state.frame_fidelity.target_scale_level, 0);
    assert_eq!(
        state.frame_fidelity.displayed_scale_level,
        Some(selected_scale)
    );
    assert_eq!(
        state.lod_schedule.displayed_scale_level,
        Some(selected_scale)
    );
    assert_eq!(state.lod_schedule.pending_scale_level, None);
    assert_eq!(state.resident_bricks.len(), state.visible_brick_count);
    assert!(
        state
            .resident_bricks
            .iter()
            .all(|brick| brick.scale_level == selected_scale)
    );
}

#[test]
fn coarse_async_completion_is_ignored_when_not_current_pending_scale() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_scale_budgeted_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    set_orthographic_world_per_screen_point_for_height(&mut state, 360.0);
    rerender_state_with_backend(&mut state, None).unwrap();
    let selected_scale = state.brick_stream_scale_level;
    assert!(selected_scale > 0);

    let stale = BrickReadOutcome {
        request_id: mirante4d_data::DataRequestId(0),
        generation_id: mirante4d_data::DataGenerationId(state.brick_stream_generation),
        layer_id: LayerId::new(state.active_layer_id.clone()).unwrap(),
        scale_level: selected_scale.saturating_sub(1),
        timepoint: state.active_timepoint,
        brick_index: state.visible_bricks[0],
        sample_region: None,
        priority: BrickRequestPriority::CurrentFrame,
        read_metrics: mirante4d_data::BrickReadMetrics::default(),
        histogram_sample: None,
        status: BrickReadStatus::Stale,
    };

    assert!(!apply_brick_read_outcome(&mut state, stale));
    assert_eq!(state.brick_stream_scale_level, selected_scale);
    assert_eq!(state.frame_fidelity.target_scale_level, 0);
    assert_eq!(
        state.frame_fidelity.reason,
        LodDecisionReason::GpuBudgetLimited
    );
}

#[test]
fn app_smoke_streams_large_dataset_before_reporting_frame() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_large_multiscale_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();

    render_first_streamed_frame_for_smoke(&mut state, None, Duration::from_secs(5)).unwrap();

    assert!(state.brick_stream_complete);
    assert!(state.diagnostics.nonzero_pixels > 0);
    assert!(state.diagnostics.max_value > 0);
    assert_eq!(state.render_backend, RenderBackend::CpuResidentBricks);
}

#[test]
fn app_streams_selected_lod_scale_into_resident_renderer() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    assert!(set_render_viewport(
        &mut state,
        RenderViewport::new(64, 64).unwrap()
    ));
    state.presentation_viewport = PresentationViewport::new(64.0, 64.0).unwrap();
    set_orthographic_world_per_screen_point(&mut state, 2.5);
    rerender_state_with_backend(&mut state, None).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 4).unwrap();

    let submission = submit_visible_bricks_to_pool(&mut state, &pool);
    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();

    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 1);
    assert_eq!(submission.current_tickets[0].scale_level, 1);
    assert_eq!(outcome.scale_level, 1);
    assert!(apply_brick_read_outcome(&mut state, outcome));
    assert_eq!(state.resident_bricks.len(), 1);
    assert_eq!(state.resident_bricks[0].scale_level, 1);

    render_state_from_resident_bricks(&mut state).unwrap();

    assert_eq!(state.render_backend, RenderBackend::CpuResidentBricks);
    assert_eq!(state.frame.width, state.render_viewport.width);
    assert_eq!(state.frame.height, state.render_viewport.height);
    assert!(state.diagnostics.nonzero_pixels > 0);
}

#[test]
fn app_requeues_instead_of_rendering_stale_resident_bricks_after_camera_change() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_scale_budgeted_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    set_orthographic_world_per_screen_point_for_height(&mut state, 20.0);
    rerender_state_with_backend(&mut state, None).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 512).unwrap();

    let first_submission = submit_visible_bricks_to_pool(&mut state, &pool);
    assert!(first_submission.queued_current);
    for _ in 0..first_submission.current_tickets.len() {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.priority, BrickRequestPriority::CurrentFrame);
        assert!(apply_brick_read_outcome(&mut state, outcome));
    }
    assert!(current_resident_frame_ready(&state));
    render_state_from_resident_bricks(&mut state).unwrap();
    let previous_request = state.brick_stream_request_key.clone();

    state.camera.target.x += 96.0;
    rerender_state_with_backend(&mut state, None).unwrap();

    assert_eq!(state.brick_stream_request_key, previous_request);
    assert!(!current_resident_frame_ready(&state));
    assert_ne!(
        state.brick_stream_request_key.as_ref(),
        Some(&current_brick_stream_request_key(&state).unwrap())
    );
    assert!(matches!(
        state.frame_fidelity.completeness,
        FrameCompleteness::Loading | FrameCompleteness::BudgetLimited
    ));
    let second_submission = submit_visible_bricks_to_pool(&mut state, &pool);
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
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 10).unwrap();
    state.visible_bricks = vec![SpatialBrickIndex::new(1, 1, 1)];
    state.visible_brick_count = 1;

    let submission = submit_visible_bricks_to_pool(&mut state, &pool);
    let current = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(current.priority, BrickRequestPriority::CurrentFrame);
    assert!(apply_brick_read_outcome(&mut state, current));
    for _ in 0..submission.warm_tickets.len() {
        let warm = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(warm.priority, BrickRequestPriority::Warm);
        assert!(!apply_brick_read_outcome(&mut state, warm));
    }

    let stats = state.dataset.stats().unwrap();

    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 1);
    assert!(submission.prefetch_tickets.is_empty());
    assert_eq!(submission.warm_tickets.len(), 9);
    assert_eq!(state.brick_stream_requested, 1);
    assert_eq!(state.brick_stream_completed, 1);
    assert_eq!(state.resident_bricks.len(), 1);
    assert_eq!(state.prefetched_brick_payloads.len(), 9);
    assert_eq!(state.brick_warm_brick_count, 26);
    assert_eq!(state.brick_warm_requested, 9);
    assert_eq!(state.brick_warm_completed, 9);
    assert_eq!(state.brick_warm_skipped, 17);
    assert_eq!(state.brick_warm_failed, 0);
    assert_eq!(stats.brick_requests_queued, 10);
    assert_eq!(stats.brick_queue_full, 0);

    state.visible_bricks = vec![SpatialBrickIndex::new(0, 0, 0)];
    state.visible_brick_count = 1;
    let warmed_submission = submit_visible_bricks_to_pool(&mut state, &pool);

    assert!(warmed_submission.current_changed);
    assert!(!warmed_submission.queued_current);
    assert!(warmed_submission.current_tickets.is_empty());
    assert_eq!(state.brick_stream_requested, 1);
    assert_eq!(state.brick_stream_completed, 1);
    assert!(state.brick_stream_complete);
    assert_eq!(state.resident_bricks.len(), 1);
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
        let mut state = open_dataset_and_render_first_frame(root).unwrap();
        state.active_render_mode = mode;
        state.iso_display_level = iso_level_for_u16_threshold(3_000);
        state.dvr_density_scale = 12.0;
        rerender_state_with_backend(&mut state, None).unwrap();
        let dense_pixels = state.frame.pixels().to_vec();
        let pool = BrickReadPool::new(state.dataset.clone(), 1, 4).unwrap();

        assert!(submit_visible_bricks_to_pool(&mut state, &pool).queued_current);
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(apply_brick_read_outcome(&mut state, outcome));
        render_state_from_resident_bricks(&mut state).unwrap();

        assert_eq!(state.render_backend, RenderBackend::CpuResidentBricks);
        assert_eq!(state.frame.pixels(), dense_pixels.as_slice());
        assert!(state.diagnostics.nonzero_pixels > 0);
        assert!(state.brick_stream_complete);
    }
}

#[test]
fn resident_rendering_uses_visible_channel_render_modes() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.layers[0].display.visible = true;
    state.layers[1].display.visible = true;
    state.layers[1].render_state = ChannelRenderState::for_mode(
        RenderMode::Dvr,
        RenderSamplingPolicy::VoxelExact,
        RenderIsoShadingPolicy::Flat,
        state.iso_display_level,
        default_dvr_opacity_transfer(state.layers[1].display),
        18.0,
    );
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 4).unwrap();

    let submission = submit_visible_bricks_to_pool(&mut state, &pool);
    assert!(submission.queued_current);
    assert_eq!(submission.current_tickets.len(), 2);
    for _ in 0..submission.current_tickets.len() {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(apply_brick_read_outcome(&mut state, outcome));
    }
    render_state_from_resident_bricks(&mut state).unwrap();

    assert_eq!(state.render_backend, RenderBackend::CpuResidentBricks);
    assert_eq!(state.rendered_channels.len(), 2);
    assert_eq!(state.rendered_channels[0].layer_id, "ch0");
    assert_eq!(state.rendered_channels[1].layer_id, "ch1");
    assert_eq!(state.active_render_mode, RenderMode::Mip);
    assert_eq!(state.layers[1].render_state.mode(), RenderMode::Dvr);
    assert_ne!(
        state.rendered_channels[0].frame.pixels(),
        state.rendered_channels[1].frame.pixels()
    );
}

#[test]
fn app_requeues_visible_bricks_after_source_change() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 8).unwrap();

    assert!(submit_visible_bricks_to_pool(&mut state, &pool).queued_current);
    let first_generation = state.brick_stream_generation;
    activate_layer_timepoint_state_only(&mut state, 0, TimeIndex(1)).unwrap();
    rerender_state_with_backend(&mut state, None).unwrap();
    assert!(submit_visible_bricks_to_pool(&mut state, &pool).queued_current);
    let second_generation = state.brick_stream_generation;

    assert!(second_generation > first_generation);
    assert_eq!(state.brick_stream_requested, state.visible_brick_count * 2);
}

#[test]
fn app_ignores_stale_brick_worker_outcomes() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 4).unwrap();
    assert!(submit_visible_bricks_to_pool(&mut state, &pool).queued_current);
    let current_generation = state.brick_stream_generation;
    let stale = BrickReadOutcome {
        request_id: mirante4d_data::DataRequestId(99),
        generation_id: mirante4d_data::DataGenerationId(current_generation - 1),
        layer_id: LayerId::new(state.active_layer_id.clone()).unwrap(),
        scale_level: 0,
        timepoint: state.active_timepoint,
        brick_index: SpatialBrickIndex::new(0, 0, 0),
        sample_region: None,
        priority: BrickRequestPriority::CurrentFrame,
        read_metrics: mirante4d_data::BrickReadMetrics::default(),
        histogram_sample: None,
        status: BrickReadStatus::Stale,
    };

    assert!(!apply_brick_read_outcome(&mut state, stale));

    assert_eq!(state.brick_stream_generation, current_generation);
    assert_eq!(state.brick_stream_completed, 0);
    assert_eq!(state.brick_stream_stale, 0);
    assert!(state.resident_bricks.is_empty());
}
