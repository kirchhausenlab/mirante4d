#[test]
fn import_options_create_strict_native_output_package_path() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("Synthetic source");
    let output = tempdir.path().join("out");

    let options = import_tiff_source_options(TiffImportSource::Directory(input), &output).unwrap();

    assert_eq!(options.dataset_id, "synthetic-source");
    assert_eq!(options.dataset_name, "Synthetic source");
    assert_eq!(
        options.output_package,
        output.join("synthetic-source.m4d")
    );
    assert_eq!(options.voxel_spacing_um, [1.0, 1.0, 1.0]);
    assert_eq!(options.existing_policy, ExistingPackagePolicy::Fail);
}

#[test]
fn import_options_use_single_tiff_file_stem_for_output_package_path() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("Synthetic source.tif");
    let output = tempdir.path().join("out");

    let options = import_tiff_source_options(TiffImportSource::SingleFile(input), &output).unwrap();

    assert_eq!(options.dataset_id, "synthetic-source");
    assert_eq!(options.dataset_name, "Synthetic source");
    assert_eq!(
        options.output_package,
        output.join("synthetic-source.m4d")
    );
}

#[test]
fn prepare_tiff_import_reports_inspection_errors() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("empty-input");
    let output = tempdir.path().join("out");
    fs::create_dir(&input).unwrap();

    let err = prepare_tiff_source_import(TiffImportSource::Directory(input), &output).unwrap_err();

    assert!(err.to_string().contains("contains no .tif/.tiff files"));
}

#[test]
fn prepare_tiff_import_prefills_ome_voxel_spacing_without_confirming_review() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("ome-source.tif");
    let output = tempdir.path().join("out");
    write_app_ome_stack(&input).unwrap();

    let (options, inspection) =
        prepare_tiff_source_import(TiffImportSource::SingleFile(input), &output).unwrap();
    let pending = PendingTiffImport {
        options: options.clone(),
        inspection,
        voxel_spacing_confirmed: false,
        grouping_confirmed: true,
    };

    assert_eq!(options.voxel_spacing_um, [0.25, 0.5, 0.75]);
    assert!(!pending_tiff_import_ready_to_start(&pending));
    assert!(
        validate_pending_tiff_import(&pending)
            .unwrap_err()
            .to_string()
            .contains("voxel spacing must be reviewed")
    );
}

#[test]
fn import_options_validation_requires_positive_finite_spacing_and_name() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut options = import_tiff_source_options(
        TiffImportSource::Directory(tempdir.path().join("Synthetic source")),
        &tempdir.path().join("out"),
    )
    .unwrap();

    assert!(validate_tiff_import_options(&options).is_ok());

    options.voxel_spacing_um[0] = 0.0;
    assert!(
        validate_tiff_import_options(&options)
            .unwrap_err()
            .to_string()
            .contains("voxel spacing x")
    );

    options.voxel_spacing_um[0] = f64::INFINITY;
    assert!(
        validate_tiff_import_options(&options)
            .unwrap_err()
            .to_string()
            .contains("voxel spacing x")
    );

    options.voxel_spacing_um[0] = 1.0;
    options.dataset_name = "   ".to_owned();
    assert!(
        validate_tiff_import_options(&options)
            .unwrap_err()
            .to_string()
            .contains("dataset name")
    );

    options.dataset_name = "Synthetic source".to_owned();
    options
        .channel_metadata
        .insert(0, default_tiff_channel_metadata_override(0));
    options.channel_metadata.get_mut(&0).unwrap().name = " ".to_owned();
    assert!(
        validate_tiff_import_options(&options)
            .unwrap_err()
            .to_string()
            .contains("channel 0 name")
    );

    options.channel_metadata.get_mut(&0).unwrap().name = "channel".to_owned();
    options.channel_metadata.get_mut(&0).unwrap().color_rgba[1] = f32::NAN;
    assert!(
        validate_tiff_import_options(&options)
            .unwrap_err()
            .to_string()
            .contains("channel 0 color")
    );
}

#[test]
fn pending_tiff_import_requires_reviewed_voxel_spacing() {
    let tempdir = tempfile::tempdir().unwrap();
    let source = tempdir.path().join("source.tif");
    write_app_ome_stack(&source).unwrap();
    let options = import_tiff_source_options(
        TiffImportSource::SingleFile(source),
        &tempdir.path().join("out"),
    )
    .unwrap();
    let inspection = TiffDirectoryInspection {
        input_dir: options.source.path().to_path_buf(),
        source_profile: TiffSourceProfile::StackSeriesMovie,
        file_count: 1,
        channel_count: 1,
        timepoint_count: 1,
        shape: TiffStackShape { z: 1, y: 2, x: 3 },
        source_dtype: IntensityDType::Uint16,
        source_metadata: Default::default(),
        metadata_confidence: TiffMetadataConfidence::MissingSpatialCalibration,
        value_range: TiffValueRangeSummary { min: 0.0, max: 5.0 },
        files: Vec::new(),
        channels: vec![TiffChannelInspection {
            channel: 0,
            timepoint_count: 1,
        }],
    };
    let mut pending = PendingTiffImport {
        options,
        inspection,
        voxel_spacing_confirmed: false,
        grouping_confirmed: true,
    };

    let err = validate_pending_tiff_import(&pending).unwrap_err();
    assert!(err.to_string().contains("voxel spacing must be reviewed"));

    pending.voxel_spacing_confirmed = true;
    pending.grouping_confirmed = false;
    let err = validate_pending_tiff_import(&pending).unwrap_err();
    assert!(err.to_string().contains("source layout must be reviewed"));

    pending.grouping_confirmed = true;
    assert!(validate_pending_tiff_import(&pending).is_ok());
}

#[test]
fn pending_tiff_import_no_data_policy_is_explicit_uint8_review() {
    let tempdir = tempfile::tempdir().unwrap();
    let options = import_tiff_source_options(
        TiffImportSource::SingleFile(tempdir.path().join("source-u8.tif")),
        &tempdir.path().join("out"),
    )
    .unwrap();
    let inspection = TiffDirectoryInspection {
        input_dir: options.source.path().to_path_buf(),
        source_profile: TiffSourceProfile::StackSeriesMovie,
        file_count: 1,
        channel_count: 1,
        timepoint_count: 1,
        shape: TiffStackShape { z: 3, y: 2, x: 3 },
        source_dtype: IntensityDType::Uint8,
        source_metadata: Default::default(),
        metadata_confidence: TiffMetadataConfidence::MissingSpatialCalibration,
        value_range: TiffValueRangeSummary {
            min: 0.0,
            max: 255.0,
        },
        files: Vec::new(),
        channels: vec![TiffChannelInspection {
            channel: 0,
            timepoint_count: 1,
        }],
    };
    let mut pending = PendingTiffImport {
        options,
        inspection,
        voxel_spacing_confirmed: true,
        grouping_confirmed: true,
    };

    set_pending_tiff_no_data_policy(&mut pending, true);

    assert_eq!(
        pending.options.reviewed_plan.no_data_policy,
        Some(TiffNoDataPolicyReview {
            source_dtype: IntensityDType::Uint8,
            source_value_uint8: 255,
        })
    );
    let accepted_plan = accepted_reviewed_plan_for_pending_tiff_import(&pending);
    assert_eq!(
        accepted_plan.review_status,
        TiffImportReviewStatus::Accepted
    );
    assert_eq!(
        accepted_plan.no_data_policy,
        Some(TiffNoDataPolicyReview {
            source_dtype: IntensityDType::Uint8,
            source_value_uint8: 255,
        })
    );

    pending.inspection.source_dtype = IntensityDType::Uint16;
    normalize_pending_tiff_no_data_policy(&mut pending);

    assert_eq!(pending.options.reviewed_plan.no_data_policy, None);
}

#[test]
fn no_data_policy_label_reports_value_dtype_and_dilation() {
    let label = no_data_policy_label(&NoDataPolicy {
        kind: NoDataPolicyKind::SentinelValue,
        source_value: 255.0,
        source_dtype: IntensityDType::Uint8,
        visibility_policy: NoDataVisibilityPolicy::InvisibleWith1VoxelInvalidDilation,
    });

    assert_eq!(label, "value 255 (uint8), dilated 1 voxel");
}

#[test]
fn background_work_repaint_request_wakes_ui_immediately() {
    let ctx = egui::Context::default();

    let output = ctx.run_ui(egui::RawInput::default(), |ui| {
        request_background_work_repaint(ui.ctx());
    });

    let root_output = output.viewport_output.get(&egui::ViewportId::ROOT).unwrap();
    assert_eq!(root_output.repaint_delay, Duration::ZERO);
}

#[test]
fn background_thread_repaint_request_wakes_ui_immediately() {
    let ctx = egui::Context::default();
    let repaint_ctx = ctx.clone();

    thread::spawn(move || {
        request_background_work_repaint(&repaint_ctx);
    })
    .join()
    .unwrap();
    let output = ctx.run_ui(egui::RawInput::default(), |_ui| {});

    let root_output = output.viewport_output.get(&egui::ViewportId::ROOT).unwrap();
    assert_eq!(root_output.repaint_delay, Duration::ZERO);
}

#[test]
fn import_progress_messages_are_stable_for_ui_status() {
    let output = PathBuf::from("/tmp/imported.m4d");

    assert_eq!(
        import_progress_message(&ImportProgressEvent::DiscoveredInput { file_count: 8 }),
        "Discovered 8 TIFF file(s)"
    );
    assert_eq!(
        import_progress_message(&ImportProgressEvent::EstimatedStorage {
            estimate: TiffImportStorageEstimate {
                source_payload_bytes: 96,
                derived_multiscale_payload_bytes: 0,
                estimated_metadata_bytes: 1_179_648,
                estimated_total_bytes: 1_179_744,
                peak_working_stack_bytes: 24,
            },
        }),
        "Estimated native package size 1.13 MiB"
    );
    assert_eq!(
        import_progress_message(&ImportProgressEvent::ReadStack {
            completed: 3,
            total: 8,
            path: PathBuf::from("/tmp/ch0.tif"),
        }),
        "Reading TIFF stack 3/8"
    );
    assert_eq!(
        import_progress_message(&ImportProgressEvent::BuiltScale {
            channel: 2,
            level: 1,
        }),
        "Built channel 2 scale s1"
    );
    assert_eq!(
        import_progress_message(&ImportProgressEvent::WritingPackage {
            output_package: output.clone(),
        }),
        "Writing /tmp/imported.m4d"
    );
    assert_eq!(
        import_progress_message(&ImportProgressEvent::Finished {
            output_package: output,
        }),
        "Finished /tmp/imported.m4d"
    );
}

#[test]
fn import_progress_fraction_is_monotonic_by_phase() {
    let discovered = import_progress_fraction(Some(&ImportProgressEvent::DiscoveredInput {
        file_count: 4,
    }))
    .unwrap();
    let estimated = import_progress_fraction(Some(&ImportProgressEvent::EstimatedStorage {
        estimate: TiffImportStorageEstimate {
            source_payload_bytes: 96,
            derived_multiscale_payload_bytes: 0,
            estimated_metadata_bytes: 1_179_648,
            estimated_total_bytes: 1_179_744,
            peak_working_stack_bytes: 24,
        },
    }))
    .unwrap();
    let read_half = import_progress_fraction(Some(&ImportProgressEvent::ReadStack {
        completed: 2,
        total: 4,
        path: PathBuf::from("/tmp/ch0.tif"),
    }))
    .unwrap();
    let built = import_progress_fraction(Some(&ImportProgressEvent::BuiltScale {
        channel: 0,
        level: 2,
    }))
    .unwrap();
    let writing = import_progress_fraction(Some(&ImportProgressEvent::WritingPackage {
        output_package: PathBuf::from("/tmp/imported.m4d"),
    }))
    .unwrap();
    let finished = import_progress_fraction(Some(&ImportProgressEvent::Finished {
        output_package: PathBuf::from("/tmp/imported.m4d"),
    }))
    .unwrap();

    assert!(discovered < read_half);
    assert!(discovered < estimated);
    assert!(estimated < read_half);
    assert!(read_half < built);
    assert!(built < writing);
    assert!(writing < finished);
    assert_eq!(finished, 1.0);
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn app_backend_uses_gpu_for_volume_modes_when_renderer_available() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let renderer = GpuRenderer::new_blocking().unwrap();

    state.active_render_mode = RenderMode::Mip;
    rerender_state_with_backend(&mut state, Some(&renderer)).unwrap();
    assert_eq!(state.render_backend, RenderBackend::GpuCameraMip);
    assert_eq!(state.frame.width, state.render_viewport.width);
    assert_eq!(state.frame.height, state.render_viewport.height);
    assert!(state.diagnostics.nonzero_pixels > 0);

    state.active_render_mode = RenderMode::Isosurface;
    state.iso_display_level = iso_level_for_u16_threshold(3_000);
    rerender_state_with_backend(&mut state, Some(&renderer)).unwrap();
    assert_eq!(state.render_backend, RenderBackend::GpuCameraIso);
    assert!(state.diagnostics.nonzero_pixels > 0);

    state.active_render_mode = RenderMode::Dvr;
    state.dvr_density_scale = 12.0;
    rerender_state_with_backend(&mut state, Some(&renderer)).unwrap();
    assert_eq!(state.render_backend, RenderBackend::GpuCameraDvr);
    assert!(state.diagnostics.nonzero_pixels > 0);
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn app_backend_uses_gpu_resident_bricks_when_stream_complete() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let renderer = GpuRenderer::new_blocking().unwrap();

    let pool = BrickReadPool::new(state.dataset.clone(), 1, 4).unwrap();
    assert!(submit_visible_bricks_to_pool(&mut state, &pool).queued_current);
    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(apply_brick_read_outcome(&mut state, outcome));
    assert!(state.brick_stream_complete);

    for mode in [RenderMode::Mip, RenderMode::Isosurface, RenderMode::Dvr] {
        state.active_render_mode = mode;
        state.iso_display_level = iso_level_for_u16_threshold(3_000);
        state.dvr_density_scale = 12.0;
        rerender_state_with_backend(&mut state, Some(&renderer)).unwrap();
        let dense_gpu_pixels = state.frame.pixels().to_vec();

        render_state_from_resident_bricks_with_backend(&mut state, Some(&renderer)).unwrap();

        assert_eq!(state.render_backend, RenderBackend::GpuResidentBricks);
        assert_eq!(
            state.frame.pixels(),
            dense_gpu_pixels.as_slice(),
            "{mode:?} resident GPU pixels differ from dense GPU pixels"
        );
        assert!(state.diagnostics.nonzero_pixels > 0);
    }
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn app_backend_uses_gpu_resident_bricks_for_float32_layers() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_float32_app_fixture(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(&root).unwrap();
    let renderer = GpuRenderer::new_blocking().unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 32).unwrap();

    assert!(submit_visible_bricks_to_pool(&mut state, &pool).queued_current);
    for _ in 0..state.brick_stream_requested {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.priority, BrickRequestPriority::CurrentFrame);
        assert!(apply_brick_read_outcome(&mut state, outcome));
    }
    assert!(state.brick_stream_complete);
    assert_eq!(state.resident_bricks_f32.len(), state.visible_brick_count);

    for mode in [RenderMode::Mip, RenderMode::Isosurface, RenderMode::Dvr] {
        state.active_render_mode = mode;
        state.iso_display_level = iso_level_for_u16_threshold(10_000);
        state.dvr_density_scale = 12.0;
        rerender_state_with_backend(&mut state, Some(&renderer)).unwrap();
        let dense_f32_pixels = state.frame_f32.as_ref().unwrap().pixels().to_vec();

        let submission = submit_visible_bricks_to_pool(&mut state, &pool);
        for _ in 0..submission.current_tickets.len() {
            let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(outcome.priority, BrickRequestPriority::CurrentFrame);
            assert!(apply_brick_read_outcome(&mut state, outcome));
        }
        assert!(state.brick_stream_complete);
        assert!(current_resident_frame_ready(&state));
        render_state_from_resident_bricks_with_backend(&mut state, Some(&renderer)).unwrap();

        assert_eq!(state.render_backend, RenderBackend::GpuResidentBricks);
        let resident_pixels = state.frame_f32.as_ref().unwrap().pixels();
        assert_eq!(resident_pixels.len(), dense_f32_pixels.len());
        for (resident, dense) in resident_pixels.iter().zip(dense_f32_pixels.iter()) {
            assert!(
                (*resident - *dense).abs() <= 1.0e-4,
                "{mode:?} resident f32 pixel {resident} differs from dense reference {dense}"
            );
        }
        assert!(state.diagnostics_f32.unwrap().nonzero_pixels > 0);
    }
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn app_backend_uses_gpu_resident_bricks_for_visible_channel_layers() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let renderer = GpuRenderer::new_blocking().unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 2).unwrap();

    assert!(submit_visible_bricks_to_pool(&mut state, &pool).queued_current);
    for _ in 0..2 {
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.priority, BrickRequestPriority::CurrentFrame);
        assert!(apply_brick_read_outcome(&mut state, outcome));
    }
    render_state_from_resident_bricks_with_backend(&mut state, Some(&renderer)).unwrap();

    assert_eq!(state.render_backend, RenderBackend::GpuResidentBricks);
    assert_eq!(state.rendered_channels.len(), 2);
    assert_eq!(state.rendered_channels[0].layer_id, "ch0");
    assert_eq!(state.rendered_channels[1].layer_id, "ch1");
    assert!(state.diagnostics.nonzero_pixels > 0);
    let active_only = crate::image_compositing::mip_to_color_image_with_color(
        &state.frame,
        state.active_layer_display,
        state.active_layer_color,
    );
    let composited = color_image_for_state(&state);
    assert_ne!(composited.pixels, active_only.pixels);
}

#[test]
fn mip_texture_conversion_preserves_dimensions_and_nonzero_signal() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();

    let image = mip_to_color_image(&state.frame, state.active_layer_display);

    assert_eq!(image.size, [512, 512]);
    assert_eq!(image.pixels.len(), 512 * 512);
    assert!(
        image
            .pixels
            .iter()
            .any(|pixel| *pixel != egui::Color32::BLACK)
    );
}

#[test]
fn mip_texture_conversion_uses_layer_display_window_and_opacity() {
    let frame = MipImageU16::new(4, 1, vec![500, 1_000, 1_058, 1_115]);
    let display = LayerDisplay::new(
        true,
        mirante4d_core::DisplayWindow::new(1_000.0, 1_115.0).unwrap(),
        0.5,
    )
    .unwrap();

    let image = mip_to_color_image(&frame, display);

    assert_eq!(image.size, [4, 1]);
    assert_eq!(
        image.pixels,
        vec![
            egui::Color32::from_rgba_unmultiplied(0, 0, 0, 128),
            egui::Color32::from_rgba_unmultiplied(0, 0, 0, 128),
            egui::Color32::from_rgba_unmultiplied(129, 129, 129, 128),
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 128),
        ]
    );
}

#[test]
fn app_color_image_keeps_uncovered_pixels_dark_under_invert_lut() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let frame = MipImageU16::with_coverage(3, 1, vec![0, 0, 100], vec![0, 1, 1]).unwrap();
    let display = LayerDisplay::new(true, DisplayWindow::new(0.0, 100.0).unwrap(), 1.0).unwrap();
    let color = ChannelColor::new([1.0, 1.0, 1.0, 1.0]).unwrap();
    let transfer = ChannelTransferFunction::linear(display, color).with_invert(true);
    state.rendered_channels = vec![RenderedIntensityChannel {
        layer_id: state.active_layer_id.clone(),
        render_state: ChannelRenderState::mip(),
        transfer,
        frame,
        frame_f32: None,
    }];

    let image = color_image_for_state(&state);

    assert_eq!(image.size, [3, 1]);
    assert_eq!(
        image.pixels,
        vec![
            egui::Color32::from_rgba_unmultiplied(0, 0, 0, 0),
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 255),
            egui::Color32::from_rgba_unmultiplied(0, 0, 0, 255),
        ]
    );
}

#[test]
fn app_color_image_returns_empty_iso_frame_when_surface_payload_is_missing() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let frame = MipImageU16::with_coverage(2, 1, vec![100, 200], vec![1, 1]).unwrap();
    let display = LayerDisplay::new(true, DisplayWindow::new(0.0, 200.0).unwrap(), 1.0).unwrap();
    let color = ChannelColor::new([1.0, 1.0, 1.0, 1.0]).unwrap();
    state.active_render_mode = RenderMode::Isosurface;
    state.render_viewport = RenderViewport::new(2, 1).unwrap();
    state.frame = frame.clone();
    state.rendered_channels = vec![RenderedIntensityChannel {
        layer_id: state.active_layer_id.clone(),
        render_state: ChannelRenderState::for_mode(
            RenderMode::Isosurface,
            state.render_sampling_policy,
            state.render_iso_shading_policy,
            state.iso_display_level,
            state.active_dvr_opacity_transfer,
            state.dvr_density_scale,
        ),
        transfer: ChannelTransferFunction::linear(display, color),
        frame,
        frame_f32: None,
    }];

    let image = color_image_for_state(&state);

    assert_eq!(image.size, [2, 1]);
    assert_eq!(
        image.pixels,
        vec![
            egui::Color32::from_rgba_unmultiplied(0, 0, 0, 0),
            egui::Color32::from_rgba_unmultiplied(0, 0, 0, 0),
        ]
    );
}

#[test]
fn app_color_image_uses_dvr_rgba_payload_without_scalar_transfer() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let viewport = RenderViewport::new(2, 1).unwrap();
    let dvr_rgba = mirante4d_renderer::DvrRgbaFrame::try_new(
        viewport.width,
        viewport.height,
        vec![[0.125, 0.25, 0.375, 0.5], [0.9, 0.9, 0.9, 0.9]],
        PixelCoverage::Mask(vec![1, 0]),
    )
    .unwrap();
    let frame = MipImageU16::try_new_with_mode_frames(
        viewport.width,
        viewport.height,
        vec![u16::MAX, u16::MAX],
        PixelCoverage::Mask(vec![1, 0]),
        None,
        Some(dvr_rgba),
    )
    .unwrap();
    let display = LayerDisplay::new(true, DisplayWindow::new(0.0, 1.0).unwrap(), 1.0).unwrap();
    let color = ChannelColor::new([1.0, 1.0, 1.0, 1.0]).unwrap();
    state.active_render_mode = RenderMode::Dvr;
    state.render_viewport = viewport;
    state.frame = frame.clone();
    state.rendered_channels = vec![RenderedIntensityChannel {
        layer_id: state.active_layer_id.clone(),
        render_state: ChannelRenderState::for_mode(
            RenderMode::Dvr,
            state.render_sampling_policy,
            state.render_iso_shading_policy,
            state.iso_display_level,
            state.active_dvr_opacity_transfer,
            state.dvr_density_scale,
        ),
        transfer: ChannelTransferFunction::linear(display, color).with_invert(true),
        frame,
        frame_f32: None,
    }];

    let image = color_image_for_state(&state);

    assert_eq!(image.size, [2, 1]);
    assert_eq!(
        image.pixels,
        vec![
            egui::Color32::from_rgba_unmultiplied(64, 128, 191, 128),
            egui::Color32::from_rgba_unmultiplied(0, 0, 0, 0),
        ]
    );
}

#[test]
fn app_color_image_returns_empty_dvr_frame_when_rgba_payload_is_missing() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let frame = MipImageU16::with_coverage(2, 1, vec![u16::MAX, u16::MAX], vec![1, 1]).unwrap();
    let display = LayerDisplay::new(true, DisplayWindow::new(0.0, 1.0).unwrap(), 1.0).unwrap();
    let color = ChannelColor::new([1.0, 1.0, 1.0, 1.0]).unwrap();
    state.active_render_mode = RenderMode::Dvr;
    state.render_viewport = RenderViewport::new(2, 1).unwrap();
    state.frame = frame.clone();
    state.frame_fidelity.completeness = FrameCompleteness::Loading;
    state.frame_fidelity.display_freshness = DisplayedFrameFreshness::Unknown;
    state.rendered_channels = vec![RenderedIntensityChannel {
        layer_id: state.active_layer_id.clone(),
        render_state: ChannelRenderState::for_mode(
            RenderMode::Dvr,
            state.render_sampling_policy,
            state.render_iso_shading_policy,
            state.iso_display_level,
            state.active_dvr_opacity_transfer,
            state.dvr_density_scale,
        ),
        transfer: ChannelTransferFunction::linear(display, color),
        frame,
        frame_f32: None,
    }];

    assert!(!missing_typed_payload_is_reportable_error(&state));
    let image = color_image_for_state(&state);

    assert_eq!(image.size, [2, 1]);
    assert_eq!(
        image.pixels,
        vec![
            egui::Color32::from_rgba_unmultiplied(0, 0, 0, 0),
            egui::Color32::from_rgba_unmultiplied(0, 0, 0, 0),
        ]
    );

    state.frame_fidelity.completeness = FrameCompleteness::BudgetLimited;
    state.frame_fidelity.display_freshness = DisplayedFrameFreshness::Current;
    assert!(missing_typed_payload_is_reportable_error(&state));
}

#[test]
fn viewport_fit_preserves_aspect_ratio() {
    let fitted = fit_size(egui::vec2(16.0, 8.0), egui::vec2(100.0, 100.0));
    assert_eq!(fitted, egui::vec2(100.0, 50.0));
}

#[test]
fn viewport_hover_maps_pointer_to_intensity_pixel() {
    let frame = MipImageU16::new(2, 2, vec![10, 20, 30, 40]);
    let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(200.0, 100.0));

    let hover = viewport_hover_from_image_point(
        &frame,
        None,
        IntensityDType::Uint16,
        rect,
        egui::pos2(160.0, 45.0),
    )
    .unwrap();

    assert_eq!(
        hover,
        ViewportHover {
            x: 1,
            y: 0,
            intensity: ViewportIntensity::U16(20),
        }
    );
}

#[test]
fn viewport_hover_reports_uint8_intensity_for_uint8_layers() {
    let frame = MipImageU16::new(2, 1, vec![12, 255]);
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(20.0, 10.0));

    let hover = viewport_hover_from_image_point(
        &frame,
        None,
        IntensityDType::Uint8,
        rect,
        egui::pos2(15.0, 5.0),
    )
    .unwrap();

    assert_eq!(hover.intensity, ViewportIntensity::U8(255));
}

#[test]
fn viewport_hover_maps_normalized_probe_to_pixel() {
    let frame = MipImageU16::new(4, 2, vec![1, 2, 3, 4, 5, 6, 7, 8]);

    let hover = viewport_hover_from_normalized_point(
        &frame,
        None,
        IntensityDType::Uint16,
        0.50,
        0.99,
    )
    .unwrap();

    assert_eq!(
        hover,
        ViewportHover {
            x: 2,
            y: 1,
            intensity: ViewportIntensity::U16(7),
        }
    );
}

#[test]
fn viewport_hover_rejects_points_outside_image_rect() {
    let frame = MipImageU16::new(1, 1, vec![10]);
    let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(100.0, 100.0));

    assert!(
        viewport_hover_from_image_point(
            &frame,
            None,
            IntensityDType::Uint16,
            rect,
            egui::pos2(9.0, 20.0)
        )
        .is_none()
    );
}

#[test]
fn camera_drag_orbits_without_changing_projection() {
    let mut camera = CameraView::default_for_bounds(16.0, 16.0, 16.0);
    let start = camera;

    apply_camera_orbit(
        &mut camera,
        start,
        egui::pos2(50.0, 50.0),
        egui::pos2(60.0, 45.0),
        egui::vec2(100.0, 100.0),
    );

    assert_eq!(camera.projection, Projection::Orthographic);
    assert!(camera.axes().forward.x > 0.0);
    assert!(camera.axes().forward.y < 0.0);
}

#[test]
fn camera_pan_moves_target_without_changing_orientation() {
    let mut camera = CameraView::default_for_bounds(16.0, 16.0, 16.0);
    let before = camera;

    apply_camera_pan(&mut camera, egui::vec2(10.0, 5.0));

    assert_eq!(camera.orientation, before.orientation);
    assert_ne!(camera.target, before.target);
}

#[test]
fn default_camera_for_shape_uses_source_z_zero_front_and_fiji_xy() {
    let shape = Shape3D::new(4, 5, 6).unwrap();
    let grid_to_world = GridToWorld::scale_um(0.2, 0.3, 0.5);
    let camera = default_camera_for_shape(shape, grid_to_world);
    let axes = camera.axes();
    let camera_state = camera.to_camera_state(crate::viewport::default_presentation_viewport());
    let center_z_world = (shape.z.saturating_sub(1)) as f64 * 0.5 * 0.5;

    assert!((axes.right.x - 1.0).abs() <= 1e-12);
    assert!((axes.up.y + 1.0).abs() <= 1e-12);
    assert!((axes.forward.z - 1.0).abs() <= 1e-12);
    assert!(camera_state.eye.z < center_z_world);
}

#[test]
fn camera_scroll_zoom_changes_apparent_scale() {
    let mut camera = CameraView::default_for_bounds(16.0, 16.0, 16.0);
    let before = camera.world_per_screen_point_at_target();

    apply_camera_zoom(&mut camera, 120.0);

    assert!(camera.world_per_screen_point_at_target() < before);
}

#[test]
fn fit_camera_to_shape_preserves_projection_orientation_and_refits_scale() {
    let shape = Shape3D::new(32, 16, 8).unwrap();
    let mut camera = CameraView::default_for_bounds(16.0, 16.0, 16.0);
    camera.set_projection(Projection::Perspective);
    camera.orbit_by(0.4, -0.2);
    camera.pan_by(10.0, -5.0);
    camera.zoom_by(0.2);
    let before = camera;

    let presentation = crate::viewport::default_presentation_viewport();
    let fitted =
        fit_camera_to_shape_preserving_view(camera, shape, GridToWorld::identity(), presentation);
    let default_fit = fit_camera_to_shape_preserving_view(
        default_camera_for_shape(shape, GridToWorld::identity()),
        shape,
        GridToWorld::identity(),
        presentation,
    );

    assert_eq!(fitted.projection, Projection::Perspective);
    assert_eq!(fitted.orientation, before.orientation);
    assert_eq!(fitted.target, default_fit.target);
    assert!(fitted.orthographic_world_per_screen_point.is_finite());
    assert_ne!(
        fitted.orthographic_world_per_screen_point,
        before.orthographic_world_per_screen_point
    );
    assert_ne!(
        fitted.perspective_focal_length_screen_points,
        before.perspective_focal_length_screen_points
    );
    assert_ne!(fitted.target, before.target);
}

#[test]
fn perspective_fit_contains_transformed_volume_bounds() {
    let shape = Shape3D::new(9, 17, 31).unwrap();
    let grid_to_world = GridToWorld::from_dmat4(
        glam::DMat4::from_translation(DVec3::new(3.0, -5.0, 7.0))
            * glam::DMat4::from_rotation_z(0.35)
            * glam::DMat4::from_rotation_y(-0.25)
            * glam::DMat4::from_scale(DVec3::new(0.7, 1.3, 2.1)),
    );
    let mut camera = CameraView::default_for_bounds(16.0, 16.0, 16.0);
    camera.set_projection(Projection::Perspective);
    camera.orbit_by(0.6, -0.25);

    let presentation = PresentationViewport::new(640.0, 360.0).unwrap();
    let fitted = fit_camera_to_shape_preserving_view(camera, shape, grid_to_world, presentation);
    let camera_state = fitted.to_camera_state(presentation);
    let half_fit_width = presentation.width_points / (2.0 * 1.25);
    let half_fit_height = presentation.height_points / (2.0 * 1.25);

    for corner in test_shape_bounds_corners_world(shape, grid_to_world) {
        let (screen_x, screen_y) =
            project_perspective_world_point_to_screen(camera_state, corner).unwrap();
        assert!(screen_x.abs() <= half_fit_width + 1.0e-9);
        assert!(screen_y.abs() <= half_fit_height + 1.0e-9);
    }
}

fn test_shape_bounds_corners_world(shape: Shape3D, grid_to_world: GridToWorld) -> [DVec3; 8] {
    let min_x = -0.5;
    let min_y = -0.5;
    let min_z = -0.5;
    let max_x = shape.x as f64 - 0.5;
    let max_y = shape.y as f64 - 0.5;
    let max_z = shape.z as f64 - 0.5;
    [
        grid_to_world.transform_point(DVec3::new(min_x, min_y, min_z)),
        grid_to_world.transform_point(DVec3::new(max_x, min_y, min_z)),
        grid_to_world.transform_point(DVec3::new(min_x, max_y, min_z)),
        grid_to_world.transform_point(DVec3::new(max_x, max_y, min_z)),
        grid_to_world.transform_point(DVec3::new(min_x, min_y, max_z)),
        grid_to_world.transform_point(DVec3::new(max_x, min_y, max_z)),
        grid_to_world.transform_point(DVec3::new(min_x, max_y, max_z)),
        grid_to_world.transform_point(DVec3::new(max_x, max_y, max_z)),
    ]
}

fn project_perspective_world_point_to_screen(
    camera: mirante4d_core::CameraState,
    world: DVec3,
) -> Option<(f64, f64)> {
    let forward = (camera.target - camera.eye).normalize();
    let right = forward.cross(camera.up).normalize();
    let up = right.cross(forward).normalize();
    let from_eye = world - camera.eye;
    let depth = from_eye.dot(forward);
    if depth <= 0.0 {
        return None;
    }
    Some((
        camera.perspective_focal_length_screen_points * from_eye.dot(right) / depth,
        camera.perspective_focal_length_screen_points * from_eye.dot(up) / depth,
    ))
}

#[test]
fn workbench_commands_update_core_viewer_state() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    let ctx = egui::Context::default();
    app.state.frame_fidelity.display_freshness = DisplayedFrameFreshness::Current;

    assert!(
        app.apply_workbench_command(WorkbenchCommand::SetRenderMode(RenderMode::Dvr), &ctx)
            .rerender_requested
    );
    assert_eq!(app.state.active_render_mode, RenderMode::Dvr);
    assert_eq!(
        app.state.frame_fidelity.display_freshness,
        DisplayedFrameFreshness::Unknown
    );

    assert!(
        app.apply_workbench_command(
            WorkbenchCommand::SetIsoDisplayLevel {
                display_level: iso_level_for_u16_threshold(3_000)
            },
            &ctx,
        )
        .rerender_requested
    );
    assert_eq!(
        app.state.iso_display_level,
        iso_level_for_u16_threshold(3_000)
    );

    assert!(
        app.apply_workbench_command(
            WorkbenchCommand::SetDvrDensityScale {
                density_scale: 18.0,
            },
            &ctx,
        )
        .rerender_requested
    );
    assert_eq!(app.state.dvr_density_scale, 18.0);

    assert!(
        !app.apply_workbench_command(
            WorkbenchCommand::SetDvrDensityScale {
                density_scale: f64::NAN,
            },
            &ctx,
        )
        .rerender_requested
    );
    assert_eq!(app.state.dvr_density_scale, 18.0);
    assert!(
        app.state
            .last_render_error
            .as_ref()
            .unwrap()
            .contains("DVR density scale")
    );
    app.state.last_render_error = None;

    app.state.viewport_orbit_drag = Some(ViewportOrbitDragState {
        start_camera: app.state.camera,
    });
    assert!(
        app.apply_workbench_command(
            WorkbenchCommand::SetProjection(Projection::Perspective),
            &ctx,
        )
        .rerender_requested
    );
    assert_eq!(app.state.camera.projection, Projection::Perspective);
    assert_eq!(app.state.active_projection, Projection::Perspective);
    assert!(app.state.viewport_orbit_drag.is_none());

    assert!(
        app.apply_workbench_command(
            WorkbenchCommand::CameraOrbitDrag {
                start_camera: app.state.camera,
                start_position_points: egui::pos2(50.0, 50.0),
                current_position_points: egui::pos2(62.0, 46.0),
                viewport_size_points: egui::vec2(100.0, 100.0),
            },
            &ctx,
        )
        .rerender_requested
    );
    assert_ne!(app.state.camera.orientation, glam::DQuat::IDENTITY);

    app.state.viewport_orbit_drag = Some(ViewportOrbitDragState {
        start_camera: app.state.camera,
    });
    assert!(
        app.apply_workbench_command(
            WorkbenchCommand::CameraPanDrag {
                motion_points: egui::vec2(8.0, -4.0),
            },
            &ctx,
        )
        .rerender_requested
    );
    assert!(app.state.viewport_orbit_drag.is_none());

    let before_zoom = app.state.camera.world_per_screen_point_at_target();
    app.state.viewport_orbit_drag = Some(ViewportOrbitDragState {
        start_camera: app.state.camera,
    });
    assert!(
        app.apply_workbench_command(
            WorkbenchCommand::CameraZoom {
                scroll_y_points: 120.0,
            },
            &ctx,
        )
        .rerender_requested
    );
    assert!(app.state.camera.world_per_screen_point_at_target() < before_zoom);
    assert!(app.state.viewport_orbit_drag.is_none());

    let before_fit = app.state.camera;
    app.state.viewport_orbit_drag = Some(ViewportOrbitDragState {
        start_camera: app.state.camera,
    });
    assert!(
        app.apply_workbench_command(WorkbenchCommand::FitData, &ctx)
            .rerender_requested
    );
    let fitted = fit_camera_to_shape_preserving_view(
        before_fit,
        app.state.active_source_shape,
        app.state.active_source_grid_to_world,
        app.state.presentation_viewport,
    );
    assert_eq!(app.state.camera.projection, before_fit.projection);
    assert_eq!(app.state.camera.orientation, before_fit.orientation);
    assert_eq!(app.state.camera.target, fitted.target);
    assert_eq!(
        app.state.camera.world_per_screen_point_at_target(),
        fitted.world_per_screen_point_at_target()
    );
    assert!(app.state.viewport_orbit_drag.is_none());

    let projection_before_reset = app.state.camera.projection;
    let mut expected_reset_start = default_camera_for_shape(
        app.state.active_source_shape,
        app.state.active_source_grid_to_world,
    );
    expected_reset_start.set_projection(projection_before_reset);
    app.state.viewport_orbit_drag = Some(ViewportOrbitDragState {
        start_camera: app.state.camera,
    });
    assert!(
        app.apply_workbench_command(WorkbenchCommand::ResetView, &ctx)
            .rerender_requested
    );
    assert_eq!(app.state.camera.projection, projection_before_reset);
    assert_eq!(app.state.active_projection, projection_before_reset);
    assert_eq!(
        app.state.camera,
        fit_camera_to_shape_preserving_view(
            expected_reset_start,
            app.state.active_source_shape,
            app.state.active_source_grid_to_world,
            app.state.presentation_viewport
        )
    );
    assert!(app.state.viewport_orbit_drag.is_none());

    app.apply_workbench_command(WorkbenchCommand::SelectLayer(1), &ctx);
    assert_eq!(app.state.active_layer_index, 1);
    assert_eq!(app.state.active_layer_id, "ch1");
    assert_eq!(app.state.active_timepoint, TimeIndex(0));

    assert!(
        app.apply_workbench_command(
            WorkbenchCommand::SetLayerInvert {
                layer_index: 1,
                invert: true,
            },
            &ctx,
        )
        .rerender_requested
    );
    assert!(app.state.layers[1].invert);
    assert!(app.state.active_layer_transfer.invert);

    app.apply_workbench_command(WorkbenchCommand::SetTimepoint(TimeIndex(2)), &ctx);
    assert_eq!(app.state.active_layer_index, 1);
    assert_eq!(app.state.active_timepoint, TimeIndex(2));
    assert!(app.state.active_layer_transfer.invert);
}

#[test]
fn workbench_command_switches_viewer_layout_without_mutating_3d_state() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    let ctx = egui::Context::default();
    let camera_before = app.state.camera;
    let render_mode_before = app.state.active_render_mode;

    assert_eq!(app.state.viewer_layout.layout(), ViewerLayout::Single3d);
    assert!(!app.state.viewer_layout.has_four_panel_runtime());
    assert!(
        app.apply_workbench_command(WorkbenchCommand::SetViewerLayout(ViewerLayout::FourPanel), &ctx)
            .rerender_requested
    );
    assert_eq!(app.state.viewer_layout.layout(), ViewerLayout::FourPanel);
    assert!(app.state.viewer_layout.has_four_panel_runtime());
    assert_eq!(app.state.camera, camera_before);
    assert_eq!(app.state.active_render_mode, render_mode_before);

    assert!(
        !app.apply_workbench_command(WorkbenchCommand::SetViewerLayout(ViewerLayout::FourPanel), &ctx)
            .rerender_requested
    );
    assert_eq!(app.state.camera, camera_before);
    assert!(
        app.apply_workbench_command(WorkbenchCommand::SetViewerLayout(ViewerLayout::Single3d), &ctx)
            .rerender_requested
    );
    assert_eq!(app.state.viewer_layout.layout(), ViewerLayout::Single3d);
    assert!(!app.state.viewer_layout.has_four_panel_runtime());
    assert_eq!(app.state.camera, camera_before);
    assert_eq!(app.state.active_render_mode, render_mode_before);
}

#[test]
fn cross_section_panel_render_request_requires_visible_panel_viewport_generation() {
    let _render_fn: fn(
        &AppState,
        &mirante4d_renderer::gpu::GpuRenderer,
        PanelId,
    ) -> anyhow::Result<resident_rendering::CrossSectionPanelDisplayFrame> =
        render_gpu_cross_section_panel_frame_from_global_runtime;

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();

    let inactive_error =
        cross_section_panel_render_request_for_state(&state, PanelId::Xy).unwrap_err();
    assert!(
        inactive_error
            .to_string()
            .contains("requires the FourPanel layout")
    );

    state.viewer_layout.switch_to_four_panel();
    let three_d_error =
        cross_section_panel_render_request_for_state(&state, PanelId::ThreeD).unwrap_err();
    assert!(
        three_d_error
            .to_string()
            .contains("3D panel is not a cross-section render target")
    );

    let missing_viewport_error =
        cross_section_panel_render_request_for_state(&state, PanelId::Xz).unwrap_err();
    assert!(
        missing_viewport_error
            .to_string()
            .contains("does not have a presentation viewport")
    );

    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();
    assert!(
        state
            .viewer_layout
            .record_panel_viewports(PanelId::Xz, presentation, render)
    );
    let request = cross_section_panel_render_request_for_state(&state, PanelId::Xz).unwrap();
    assert_eq!(request.panel_id, PanelId::Xz);
    assert_eq!(request.generation, 1);
    assert_eq!(request.presentation_viewport, presentation);
    assert_eq!(request.render_viewport, render);
    assert_eq!(
        request.view,
        state
            .viewer_layout
            .cross_section
            .view(mirante4d_renderer::CrossSectionPanel::Xz)
    );

    assert!(state.viewer_layout.mark_panel_displayed(PanelId::Xz, 1));
    assert!(!state.viewer_layout.mark_panel_displayed(PanelId::Xz, 0));

    let resized = PresentationViewport::new(320.0, 180.0).unwrap();
    assert!(
        state
            .viewer_layout
            .record_panel_viewports(PanelId::Xz, resized, render)
    );
    assert!(!state.viewer_layout.mark_panel_displayed(PanelId::Xz, 1));
    assert!(state.viewer_layout.mark_panel_displayed(PanelId::Xz, 2));
}

#[test]
fn cross_section_panel_scheduler_records_missing_viewport_status() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();

    state.viewer_layout.switch_to_four_panel();
    let plan =
        crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
            &mut state,
            PanelId::Xy,
            true,
        )
        .unwrap();

    assert_eq!(
        plan.schedule.status,
        crate::viewer_layout::CrossSectionPanelScheduleStatus::MissingViewport
    );
    let panel = state
        .viewer_layout
        .four_panel_runtime()
        .unwrap()
        .panel(PanelId::Xy)
        .unwrap();
    assert_eq!(panel.cross_section_schedule, Some(plan.schedule));
    assert_eq!(
        panel.cross_section_schedule.unwrap().status_label(),
        "waiting for panel size"
    );
}

#[test]
fn cross_section_scheduler_selects_lod_from_cross_section_scale() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let layer_id = LayerId::new(state.active_layer_id.clone()).unwrap();

    state.brick_stream_scale_level = 0;
    state
        .viewer_layout
        .cross_section
        .scale_world_per_screen_point = 2.1;

    let target =
        crate::cross_section_scheduler::cross_section_target_scale_for_state(&state, &layer_id)
            .unwrap();

    assert_eq!(target, 1);
    assert_eq!(
        state.brick_stream_scale_level, 0,
        "2D LOD selection must not mutate the current 3D stream scale"
    );
}

#[test]
fn cross_section_scheduler_biases_render_scale_until_interaction_settles() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_above_minimum_cap_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let layer_id = LayerId::new(state.active_layer_id.clone()).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    state.brick_stream_scale_level = 0;
    state
        .viewer_layout
        .cross_section
        .scale_world_per_screen_point = 2.1;
    let target_scale =
        crate::cross_section_scheduler::cross_section_target_scale_for_state(&state, &layer_id)
            .unwrap();
    let scale_count = state.dataset.scale_count(&layer_id).unwrap() as u32;
    assert!(
        target_scale + 1 < scale_count,
        "test fixture must leave one coarser scale for interaction bias"
    );
    state.viewer_layout.switch_to_four_panel();
    assert!(
        state
            .viewer_layout
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    state.cross_section_last_interaction_at = Some(std::time::Instant::now());

    let recent_plan = crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
        &mut state,
        PanelId::Xy,
        true,
    )
    .unwrap();

    assert_eq!(recent_plan.schedule.target_scale_level, Some(target_scale));
    assert_eq!(
        recent_plan.schedule.render_scale_level,
        Some(target_scale + 1)
    );
    assert_eq!(
        recent_plan.schedule.fallback_scale_level,
        Some(target_scale + 1)
    );
    assert!(!crate::cross_section_scheduler::cross_section_panel_refinement_due(
        &state,
        PanelId::Xy
    ));
    assert_eq!(
        state.brick_stream_scale_level, 0,
        "2D interaction LOD bias must not mutate the current 3D stream scale"
    );

    state.cross_section_last_interaction_at = Some(
        std::time::Instant::now() - CROSS_SECTION_INTERACTION_SETTLE_DURATION
            - Duration::from_millis(1),
    );
    assert!(crate::cross_section_scheduler::cross_section_panel_refinement_due(
        &state,
        PanelId::Xy
    ));
    assert!(
        test_workbench_app_without_background_runtime(state.clone()).background_work_active(),
        "settled coarse cross-section panels should keep runtime work active until refinement runs"
    );

    let settled_plan = crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
        &mut state,
        PanelId::Xy,
        true,
    )
    .unwrap();

    assert_eq!(settled_plan.schedule.target_scale_level, Some(target_scale));
    assert_eq!(settled_plan.schedule.render_scale_level, Some(target_scale));
    assert_eq!(settled_plan.schedule.fallback_scale_level, None);
    assert!(!crate::cross_section_scheduler::cross_section_panel_refinement_due(
        &state,
        PanelId::Xy
    ));
}

#[test]
fn cross_section_display_refresh_records_unavailable_status_without_gpu() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    let ctx = egui::Context::default();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    app.apply_workbench_command(WorkbenchCommand::SetViewerLayout(ViewerLayout::FourPanel), &ctx);
    assert!(
        app.state
            .viewer_layout
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );

    let timing = app
        .render_cross_section_panel_for_display_if_needed(PanelId::Xy)
        .unwrap();

    assert!(timing.is_none());
    assert!(app.cross_section_gpu_display_frames.is_empty());
    let schedule = app
        .state
        .viewer_layout
        .four_panel_runtime()
        .unwrap()
        .panel(PanelId::Xy)
        .unwrap()
        .cross_section_schedule
        .unwrap();
    assert_eq!(
        schedule.status,
        crate::viewer_layout::CrossSectionPanelScheduleStatus::Unavailable
    );
    assert_eq!(
        schedule.reason,
        crate::viewer_layout::CrossSectionPanelScheduleReason::GpuUnavailable
    );
    assert_eq!(schedule.generation, 1);
    assert!(schedule.target_scale_level.is_some());
}

#[test]
fn cross_section_brick_submission_does_not_mutate_3d_stream_state() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();
    let visible_bricks_before = state.visible_bricks.clone();
    let brick_stream_scale_before = state.brick_stream_scale_level;
    let brick_stream_generation_before = state.brick_stream_generation;
    let brick_stream_requested_before = state.brick_stream_requested;

    state.viewer_layout.switch_to_four_panel();
    assert!(
        state
            .viewer_layout
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
        &mut state,
        PanelId::Xy,
        true,
    )
    .unwrap();
    let submission = crate::cross_section_streaming::submit_cross_section_panel_bricks_to_pool(
        &mut state,
        PanelId::Xy,
        &pool,
    )
    .unwrap();

    assert!(submission.request_changed);
    assert!(submission.queued);
    assert_eq!(state.cross_section_runtime.pending_read_ticket_count(), 1);
    assert!(
        state
            .cross_section_runtime
            .chunks
            .values()
            .any(|entry| entry.state
                == crate::cross_section_runtime::CrossSectionChunkState::Decoding)
    );
    assert_eq!(state.visible_bricks, visible_bricks_before);
    assert_eq!(state.brick_stream_scale_level, brick_stream_scale_before);
    assert_eq!(state.brick_stream_generation, brick_stream_generation_before);
    assert_eq!(state.brick_stream_requested, brick_stream_requested_before);
    assert!(state.resident_bricks_by_layer.is_empty());

    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    let partition = crate::cross_section_streaming::apply_cross_section_brick_read_outcomes(
        &mut state,
        vec![outcome],
    );

    assert!(partition.resident_changed);
    assert!(partition.unhandled.is_empty());
    assert_eq!(state.cross_section_runtime.pending_read_ticket_count(), 0);
    assert!(
        state
            .cross_section_runtime
            .chunks
            .values()
            .any(|entry| entry.state
                == crate::cross_section_runtime::CrossSectionChunkState::CpuResident)
    );
    assert_eq!(state.brick_stream_requested, brick_stream_requested_before);
    assert!(state.resident_bricks_by_layer.is_empty());

    let plan = crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
        &mut state,
        PanelId::Xy,
        true,
    )
    .unwrap();
    assert_eq!(
        plan.schedule.status,
        crate::viewer_layout::CrossSectionPanelScheduleStatus::Ready
    );
    assert_eq!(plan.schedule.missing_occupied_bricks, 0);
}

#[test]
fn cross_section_submission_batches_global_visible_chunks_without_queue_explosion() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_many_xy_chunks_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 8, 256).unwrap();
    let presentation = PresentationViewport::new(9.0, 9.0).unwrap();
    let render = RenderViewport::new(90, 90).unwrap();
    let cap =
        crate::cross_section_streaming::CROSS_SECTION_CHUNK_READ_SUBMISSIONS_PER_PANEL_CALL;

    state
        .viewer_layout
        .cross_section
        .scale_world_per_screen_point = 1.0;
    state.viewer_layout.switch_to_four_panel();
    assert!(
        state
            .viewer_layout
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
        &mut state,
        PanelId::Xy,
        true,
    )
    .unwrap();
    let visible_chunks = state.cross_section_runtime.panels[&PanelId::Xy]
        .visible_chunks
        .len();
    assert!(
        visible_chunks > cap,
        "test fixture must exceed the per-call cross-section chunk submission cap"
    );

    let first_submission = crate::cross_section_streaming::submit_cross_section_panel_bricks_to_pool(
        &mut state,
        PanelId::Xy,
        &pool,
    )
    .unwrap();

    assert!(first_submission.request_changed);
    assert!(first_submission.queued);
    assert_eq!(first_submission.queued_current_frame, cap);
    assert_eq!(state.cross_section_runtime.pending_read_ticket_count(), cap);
    let first_stream = state
        .cross_section_runtime
        .panel_streams
        .get(&PanelId::Xy)
        .unwrap();
    assert_eq!(first_stream.visible_chunks, visible_chunks);
    assert_eq!(first_stream.requested, cap);
    assert_eq!(first_stream.deferred, visible_chunks - cap);
    assert!(!first_stream.complete);
    assert!(crate::cross_section_streaming::cross_section_runtime_work_active(
        &state
    ));

    let outcomes = (0..cap)
        .map(|_| pool.recv_timeout(Duration::from_secs(2)).unwrap())
        .collect::<Vec<_>>();
    let partition = crate::cross_section_streaming::apply_cross_section_brick_read_outcomes(
        &mut state,
        outcomes,
    );
    assert!(partition.resident_changed);
    assert!(partition.unhandled.is_empty());
    assert_eq!(state.cross_section_runtime.pending_read_ticket_count(), 0);

    let second_submission =
        crate::cross_section_streaming::submit_cross_section_panel_bricks_to_pool(
            &mut state,
            PanelId::Xy,
            &pool,
        )
        .unwrap();

    assert!(!second_submission.request_changed);
    assert!(second_submission.queued);
    assert_eq!(second_submission.queued_current_frame, visible_chunks - cap);
    assert_eq!(
        state.cross_section_runtime.pending_read_ticket_count(),
        visible_chunks - cap
    );
    let second_stream = state
        .cross_section_runtime
        .panel_streams
        .get(&PanelId::Xy)
        .unwrap();
    assert_eq!(second_stream.requested, visible_chunks);
    assert_eq!(second_stream.completed, cap);
    assert_eq!(second_stream.deferred, 0);
    assert!(crate::cross_section_streaming::cross_section_runtime_work_active(
        &state
    ));
}

#[test]
fn cross_section_hover_readout_samples_panel_resident_value() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    state.resident_bricks.clear();
    state.resident_bricks_by_layer.clear();
    state.viewer_layout.switch_to_four_panel();
    assert!(
        state
            .viewer_layout
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
        &mut state,
        PanelId::Xy,
        true,
    )
    .unwrap();
    crate::cross_section_streaming::submit_cross_section_panel_bricks_to_pool(
        &mut state,
        PanelId::Xy,
        &pool,
    )
    .unwrap();
    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    let partition = crate::cross_section_streaming::apply_cross_section_brick_read_outcomes(
        &mut state,
        vec![outcome],
    );
    assert!(partition.resident_changed);
    let plan = crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
        &mut state,
        PanelId::Xy,
        true,
    )
    .unwrap();
    assert_eq!(plan.schedule.render_scale_level, Some(0));
    assert_eq!(
        plan.schedule.status,
        crate::viewer_layout::CrossSectionPanelScheduleStatus::Ready
    );
    assert!(
        state
            .viewer_layout
            .mark_panel_displayed(PanelId::Xy, plan.schedule.generation)
    );

    let readout = crate::cross_section_readout::cross_section_hover_readout_for_panel_point(
        &state,
        PanelId::Xy,
        120.0,
        90.0,
        presentation,
    )
    .unwrap();

    assert_eq!(
        readout.status,
        crate::cross_section_readout::CrossSectionHoverStatus::Value
    );
    assert_eq!(readout.target_generation, 1);
    assert_eq!(readout.displayed_generation, Some(1));
    assert_eq!(readout.schedule_generation, Some(1));
    assert!(readout.display_current);
    assert_eq!(
        readout.generation_status,
        crate::cross_section_readout::CrossSectionHoverGenerationStatus::CurrentDisplayed
    );
    assert_eq!(readout.scale_level, Some(0));
    assert_eq!(
        readout.nearest_grid_index,
        Some(crate::cross_section_readout::CrossSectionGridIndex { x: 8, y: 8, z: 8 })
    );
    assert_eq!(
        readout.value,
        Some(crate::cross_section_readout::CrossSectionHoverValue::U16(
            expected_fixture_value(0, 8, 8, 8)
        ))
    );
    assert!(readout.text.contains("XY ch0 t0 s0 nearest"));
    assert!(readout.text.contains("value=2200"));
    assert!(
        state.resident_bricks.is_empty(),
        "2D readout must not fall back to the current 3D resident brick set"
    );
}

#[test]
fn cross_section_hover_readout_reports_retained_stale_display_generation() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    state.viewer_layout.switch_to_four_panel();
    assert!(
        state
            .viewer_layout
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
        &mut state,
        PanelId::Xy,
        true,
    )
    .unwrap();
    crate::cross_section_streaming::submit_cross_section_panel_bricks_to_pool(
        &mut state,
        PanelId::Xy,
        &pool,
    )
    .unwrap();
    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    crate::cross_section_streaming::apply_cross_section_brick_read_outcomes(
        &mut state,
        vec![outcome],
    );
    let plan = crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
        &mut state,
        PanelId::Xy,
        true,
    )
    .unwrap();
    assert!(
        state
            .viewer_layout
            .mark_panel_displayed(PanelId::Xy, plan.schedule.generation)
    );

    assert!(state.viewer_layout.mark_cross_section_panels_dirty());
    let readout = crate::cross_section_readout::cross_section_hover_readout_for_panel_point(
        &state,
        PanelId::Xy,
        120.0,
        90.0,
        presentation,
    )
    .unwrap();

    assert_eq!(
        readout.status,
        crate::cross_section_readout::CrossSectionHoverStatus::Stale
    );
    assert_eq!(readout.value, None);
    assert_eq!(readout.target_generation, 2);
    assert_eq!(readout.displayed_generation, Some(1));
    assert_eq!(readout.schedule_generation, Some(2));
    assert!(!readout.display_current);
    assert_eq!(
        readout.generation_status,
        crate::cross_section_readout::CrossSectionHoverGenerationStatus::RetainedStale
    );
    assert!(readout.text.contains("retained displayed generation"));
}

#[test]
fn cross_section_hover_readout_reports_missing_resident_without_requests() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    state.viewer_layout.switch_to_four_panel();
    assert!(
        state
            .viewer_layout
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    let plan = crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
        &mut state,
        PanelId::Xy,
        true,
    )
    .unwrap();
    assert_eq!(
        plan.schedule.status,
        crate::viewer_layout::CrossSectionPanelScheduleStatus::Incomplete
    );
    let requested_before = state.brick_stream_requested;
    let panel_streams_before = state.cross_section_runtime.panel_streams.clone();

    let readout = crate::cross_section_readout::cross_section_hover_readout_for_panel_point(
        &state,
        PanelId::Xy,
        120.0,
        90.0,
        presentation,
    )
    .unwrap();

    assert_eq!(
        readout.status,
        crate::cross_section_readout::CrossSectionHoverStatus::Incomplete
    );
    assert_eq!(readout.displayed_generation, None);
    assert_eq!(
        readout.generation_status,
        crate::cross_section_readout::CrossSectionHoverGenerationStatus::CurrentUndisplayed
    );
    assert_eq!(readout.value, None);
    assert!(readout.text.contains("incomplete (resident brick unavailable)"));
    assert_eq!(state.brick_stream_requested, requested_before);
    assert_eq!(
        state.cross_section_runtime.panel_streams,
        panel_streams_before
    );
}

#[test]
fn active_cross_section_panel_priority_does_not_mutate_3d_stream_state() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();
    let brick_stream_scale_before = state.brick_stream_scale_level;
    let brick_stream_generation_before = state.brick_stream_generation;
    let brick_stream_requested_before = state.brick_stream_requested;

    state.viewer_layout.switch_to_four_panel();
    assert!(
        state
            .viewer_layout
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    assert!(
        state
            .viewer_layout
            .record_panel_viewports(PanelId::Xz, presentation, render)
    );
    assert!(state.viewer_layout.mark_active_cross_section_panel(PanelId::Xz));
    assert_eq!(
        state.viewer_layout.active_cross_section_panel(),
        Some(PanelId::Xz)
    );
    for panel_id in [PanelId::Xy, PanelId::Xz] {
        crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
            &mut state, panel_id, true,
        )
        .unwrap();
    }
    let submission = crate::cross_section_streaming::submit_cross_section_visible_chunks_to_pool(
        &mut state,
        &pool,
    )
    .unwrap();

    assert!(submission.request_changed);
    assert!(submission.queued);
    assert!(submission.queued_current_frame > 0);
    assert_eq!(submission.queued_prefetch, 0);
    assert_eq!(state.cross_section_runtime.pending_read_ticket_count(), 1);
    assert_eq!(
        state
            .cross_section_runtime
            .read_tickets
            .first()
            .map(|ticket| ticket.panel_id),
        Some(PanelId::Xz)
    );

    let active_stream = state
        .cross_section_runtime
        .panel_streams
        .get(&PanelId::Xz)
        .unwrap();
    assert_eq!(
        active_stream.priority,
        mirante4d_data::BrickRequestPriority::CurrentFrame
    );
    assert!(active_stream.queued_current_frame > 0);
    assert_eq!(active_stream.queued_prefetch, 0);
    assert!(!active_stream.fairness_promoted);

    assert_eq!(state.brick_stream_scale_level, brick_stream_scale_before);
    assert_eq!(state.brick_stream_generation, brick_stream_generation_before);
    assert_eq!(state.brick_stream_requested, brick_stream_requested_before);

    let app = test_workbench_app_without_background_runtime(state);
    let diagnostics = app.diagnostics_summary_text();
    assert!(diagnostics.contains("cross_section_active_panel: XZ"));
    assert!(diagnostics.contains("cross_section_panel_XY_target_scale_level: 0"));
    assert!(diagnostics.contains("cross_section_panel_XY_render_scale_level: 0"));
    assert!(diagnostics.contains("cross_section_panel_XZ_target_scale_level: 0"));
    assert!(diagnostics.contains("cross_section_panel_XZ_render_scale_level: 0"));
    assert!(diagnostics.contains("cross_section_panel_XZ_selected_bricks:"));
    assert!(diagnostics.contains("cross_section_global_panels: 2"));
    assert!(diagnostics.contains("cross_section_global_panel_XY_priority_tier: VisibleLinked"));
    assert!(diagnostics.contains("cross_section_global_panel_XZ_priority_tier: VisibleActive"));
    assert!(diagnostics.contains("cross_section_stream_XZ_priority: CurrentFrame"));
}

#[test]
fn cross_section_submission_resubmits_stale_queued_chunk_without_live_ticket() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    state.viewer_layout.switch_to_four_panel();
    assert!(
        state
            .viewer_layout
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
        &mut state,
        PanelId::Xy,
        true,
    )
    .unwrap();
    let chunk_key = state.cross_section_runtime.panels[&PanelId::Xy].visible_chunks[0].clone();
    let metadata = state
        .dataset
        .brick_metadata_at_scale(
            &chunk_key.layer_id,
            chunk_key.scale_level,
            chunk_key.timepoint,
            chunk_key.brick_index,
        )
        .unwrap();
    assert!(
        state
            .cross_section_runtime
            .mark_chunk_queued(chunk_key.clone(), metadata.region)
    );

    let submission = crate::cross_section_streaming::submit_cross_section_visible_chunks_to_pool(
        &mut state,
        &pool,
    )
    .unwrap();

    assert!(submission.queued);
    assert_eq!(state.cross_section_runtime.pending_read_ticket_count(), 1);
    assert_eq!(
        state.cross_section_runtime.read_tickets[0].brick_index,
        chunk_key.brick_index
    );
    assert_eq!(state.cross_section_runtime.read_tickets[0].region, metadata.region);
}

#[test]
fn global_cross_section_submission_waits_for_active_panel_visible_work() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 2, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    state.viewer_layout.switch_to_four_panel();
    assert!(
        state
            .viewer_layout
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    assert!(
        state
            .viewer_layout
            .record_panel_viewports(PanelId::Xz, presentation, render)
    );
    assert!(state.viewer_layout.mark_active_cross_section_panel(PanelId::Xz));
    crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
        &mut state,
        PanelId::Xy,
        true,
    )
    .unwrap();

    let early_submission =
        crate::cross_section_streaming::submit_cross_section_visible_chunks_to_pool(
            &mut state,
            &pool,
        )
        .unwrap();

    assert!(!early_submission.queued);
    assert_eq!(state.cross_section_runtime.pending_read_ticket_count(), 0);
    assert!(!state
        .cross_section_runtime
        .panel_streams
        .contains_key(&PanelId::Xy));

    crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
        &mut state,
        PanelId::Xz,
        true,
    )
    .unwrap();
    let active_submission =
        crate::cross_section_streaming::submit_cross_section_visible_chunks_to_pool(
            &mut state,
            &pool,
        )
        .unwrap();

    assert!(active_submission.queued);
    assert_eq!(
        state
            .cross_section_runtime
            .read_tickets
            .first()
            .map(|ticket| ticket.panel_id),
        Some(PanelId::Xz)
    );
    assert_eq!(state.cross_section_runtime.pending_read_ticket_count(), 1);
    let active_stream = state
        .cross_section_runtime
        .panel_streams
        .get(&PanelId::Xz)
        .unwrap();
    assert_eq!(
        active_stream.priority,
        mirante4d_data::BrickRequestPriority::CurrentFrame
    );
    assert!(active_stream.queued_current_frame > 0);
}

#[test]
fn global_cross_section_submission_queues_worker_reads_in_runtime_order() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_many_xy_chunks_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 256).unwrap();
    let presentation = PresentationViewport::new(9.0, 9.0).unwrap();
    let render = RenderViewport::new(90, 90).unwrap();

    state
        .viewer_layout
        .cross_section
        .scale_world_per_screen_point = 1.0;
    state.viewer_layout.switch_to_four_panel();
    for panel_id in [PanelId::Xy, PanelId::Xz] {
        assert!(
            state
                .viewer_layout
                .record_panel_viewports(panel_id, presentation, render)
        );
        crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
            &mut state, panel_id, true,
        )
        .unwrap();
    }

    let expected_order = state
        .cross_section_runtime
        .download_promotion_entries_for_panels([PanelId::Xy, PanelId::Xz])
        .into_iter()
        .take(crate::cross_section_streaming::CROSS_SECTION_CHUNK_READ_SUBMISSIONS_PER_REFRESH)
        .map(|entry| (entry.panel_id.unwrap(), entry.key.brick_index))
        .collect::<Vec<_>>();
    assert!(
        expected_order
            .windows(2)
            .any(|pair| pair[0].0 != pair[1].0),
        "fixture should expose interleaved runtime queue entries across panels"
    );

    let submission = crate::cross_section_streaming::submit_cross_section_visible_chunks_to_pool(
        &mut state,
        &pool,
    )
    .unwrap();

    assert!(submission.queued);
    let actual_order = state
        .cross_section_runtime
        .read_tickets
        .iter()
        .map(|ticket| (ticket.panel_id, ticket.brick_index))
        .collect::<Vec<_>>();
    assert_eq!(actual_order, expected_order);
}

#[test]
fn global_cross_section_submission_uses_2d_read_backend_boundary() {
    use std::cell::RefCell;

    use mirante4d_data::{BrickReadTicket, DataError, DataGenerationId, DataRequestId};

    #[derive(Debug)]
    struct RecordingReadBackend {
        generation: DataGenerationId,
        submissions: RefCell<Vec<crate::cross_section_read_queue::CrossSectionChunkReadSubmission>>,
    }

    impl crate::cross_section_read_queue::CrossSectionReadBackend for RecordingReadBackend {
        fn active_generation(&self) -> DataGenerationId {
            self.generation
        }

        fn submit_cross_section_chunk_read(
            &self,
            generation_id: DataGenerationId,
            submission: crate::cross_section_read_queue::CrossSectionChunkReadSubmission,
        ) -> Result<BrickReadTicket, DataError> {
            let request_id = DataRequestId(self.submissions.borrow().len() as u64 + 1);
            let scale_level = submission.scale_level;
            let cancellation = submission.cancellation.clone();
            self.submissions.borrow_mut().push(submission);
            Ok(BrickReadTicket {
                request_id,
                generation_id,
                scale_level,
                cancellation,
            })
        }
    }

    let tempdir = tempfile::tempdir().unwrap();
    let root = write_many_xy_chunks_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let backend = RecordingReadBackend {
        generation: DataGenerationId(7),
        submissions: RefCell::new(Vec::new()),
    };
    let presentation = PresentationViewport::new(9.0, 9.0).unwrap();
    let render = RenderViewport::new(90, 90).unwrap();

    state
        .viewer_layout
        .cross_section
        .scale_world_per_screen_point = 1.0;
    state.viewer_layout.switch_to_four_panel();
    for panel_id in [PanelId::Xy, PanelId::Xz] {
        assert!(
            state
                .viewer_layout
                .record_panel_viewports(panel_id, presentation, render)
        );
        crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
            &mut state, panel_id, true,
        )
        .unwrap();
    }

    let expected_admissions = crate::cross_section_read_queue::cross_section_read_admissions_for_refresh(
        &state.cross_section_runtime,
        [PanelId::Xy, PanelId::Xz],
        crate::cross_section_streaming::CROSS_SECTION_CHUNK_READ_SUBMISSIONS_PER_REFRESH,
    );
    assert!(
        expected_admissions
            .windows(2)
            .any(|pair| pair[0].queue_entry.panel_id != pair[1].queue_entry.panel_id),
        "fixture should expose interleaved runtime queue entries across panels"
    );

    let submission =
        crate::cross_section_streaming::submit_cross_section_visible_chunks_to_read_queue(
            &mut state, &backend,
        )
        .unwrap();

    assert!(submission.queued);
    let submissions = backend.submissions.borrow();
    assert_eq!(submissions.len(), expected_admissions.len());
    for (submitted, expected) in submissions.iter().zip(expected_admissions.iter()) {
        assert_eq!(submitted.layer_id, expected.queue_entry.key.layer_id);
        assert_eq!(submitted.scale_level, expected.queue_entry.key.scale_level);
        assert_eq!(submitted.timepoint, expected.queue_entry.key.timepoint);
        assert_eq!(submitted.brick_index, expected.queue_entry.key.brick_index);
        assert_eq!(submitted.queue_priority, expected.worker_queue_priority);
    }
    assert_eq!(
        state.cross_section_runtime.pending_read_ticket_count(),
        expected_admissions.len()
    );
    assert!(
        state
            .cross_section_runtime
            .read_tickets
            .iter()
            .all(|ticket| ticket.ticket.generation_id == backend.generation)
    );
}

#[test]
fn app_drains_cross_section_read_pool_without_3d_brick_pool() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_many_xy_chunks_app_dataset(tempdir.path());
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    app.cross_section_read_pool = Some(
        mirante4d_data::CrossSectionChunkReadPool::new(app.state.dataset.clone(), 1, 16).unwrap(),
    );
    let presentation = PresentationViewport::new(9.0, 9.0).unwrap();
    let render = RenderViewport::new(90, 90).unwrap();

    app.state
        .viewer_layout
        .cross_section
        .scale_world_per_screen_point = 1.0;
    app.state.viewer_layout.switch_to_four_panel();
    assert!(
        app.state
            .viewer_layout
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
        &mut app.state,
        PanelId::Xy,
        true,
    )
    .unwrap();
    let pool = app.cross_section_read_pool.as_ref().unwrap();
    let submission =
        crate::cross_section_streaming::submit_cross_section_visible_chunks_to_read_queue(
            &mut app.state,
            pool,
        )
        .unwrap();

    assert!(submission.queued);
    assert!(app.brick_read_pool.is_none());
    assert!(app.state.cross_section_runtime.pending_read_ticket_count() > 0);

    let ctx = egui::Context::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while app.state.cross_section_runtime.pending_read_ticket_count() > 0 {
        app.drain_brick_results(&ctx);
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    assert_eq!(app.state.cross_section_runtime.pending_read_ticket_count(), 0);
    assert!(
        app.state
            .cross_section_runtime
            .chunks
            .values()
            .any(|entry| entry.state
                == crate::cross_section_runtime::CrossSectionChunkState::CpuResident)
    );
    assert_eq!(
        app.state.brick_result_drain_last_repaint_reason.as_deref(),
        Some("cross_section_panel_resident_pending")
    );
}

#[test]
fn sustained_active_cross_section_work_promotes_one_linked_inactive_panel_for_fairness() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_many_xy_chunks_app_dataset(tempdir.path());
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 8, 256).unwrap();
    let presentation = PresentationViewport::new(9.0, 9.0).unwrap();
    let render = RenderViewport::new(90, 90).unwrap();

    state
        .viewer_layout
        .cross_section
        .scale_world_per_screen_point = 1.0;
    state.viewer_layout.switch_to_four_panel();
    for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        assert!(
            state
                .viewer_layout
                .record_panel_viewports(panel_id, presentation, render)
        );
        crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
            &mut state, panel_id, true,
        )
        .unwrap();
    }
    assert!(state.viewer_layout.mark_active_cross_section_panel(PanelId::Xz));

    let active_submission =
        crate::cross_section_streaming::submit_cross_section_panel_bricks_to_pool(
            &mut state,
            PanelId::Xz,
            &pool,
        )
        .unwrap();
    assert!(active_submission.queued);
    assert!(active_submission.queued_current_frame > 0);
    assert!(!active_submission.fairness_promoted);

    let fairness_submission =
        crate::cross_section_streaming::submit_cross_section_panel_bricks_to_pool(
            &mut state,
            PanelId::Xy,
            &pool,
        )
        .unwrap();
    assert!(fairness_submission.queued);
    assert!(fairness_submission.queued_current_frame > 0);
    assert_eq!(fairness_submission.queued_prefetch, 0);
    assert!(fairness_submission.fairness_promoted);

    let bounded_inactive_submission =
        crate::cross_section_streaming::submit_cross_section_panel_bricks_to_pool(
            &mut state,
            PanelId::Yz,
            &pool,
        )
        .unwrap();
    assert!(bounded_inactive_submission.queued);
    assert_eq!(bounded_inactive_submission.queued_current_frame, 0);
    assert!(bounded_inactive_submission.queued_prefetch > 0);
    assert!(!bounded_inactive_submission.fairness_promoted);

    let xy_stream = state
        .cross_section_runtime
        .panel_streams
        .get(&PanelId::Xy)
        .unwrap();
    assert_eq!(xy_stream.priority, mirante4d_data::BrickRequestPriority::CurrentFrame);
    assert!(xy_stream.fairness_promoted);
    assert_eq!(xy_stream.active_panel_at_submission, Some(PanelId::Xz));
    let yz_stream = state
        .cross_section_runtime
        .panel_streams
        .get(&PanelId::Yz)
        .unwrap();
    assert_eq!(yz_stream.priority, mirante4d_data::BrickRequestPriority::Prefetch);
    assert!(!yz_stream.fairness_promoted);

    let app = test_workbench_app_without_background_runtime(state);
    let diagnostics = app.diagnostics_summary_text();
    assert!(diagnostics.contains("cross_section_stream_XY_fairness_promoted: true"));
    assert!(diagnostics.contains("cross_section_stream_YZ_fairness_promoted: false"));
}

#[test]
fn cross_section_stale_generation_outcome_does_not_update_global_runtime() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    state.viewer_layout.switch_to_four_panel();
    assert!(
        state
            .viewer_layout
            .record_panel_viewports(PanelId::Xy, presentation, render)
    );
    crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
        &mut state,
        PanelId::Xy,
        true,
    )
    .unwrap();
    let submission = crate::cross_section_streaming::submit_cross_section_panel_bricks_to_pool(
        &mut state,
        PanelId::Xy,
        &pool,
    )
    .unwrap();
    assert!(submission.queued);
    assert_eq!(state.cross_section_runtime.pending_read_ticket_count(), 1);

    assert!(state.viewer_layout.mark_cross_section_panels_dirty());
    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    let partition = crate::cross_section_streaming::apply_cross_section_brick_read_outcomes(
        &mut state,
        vec![outcome],
    );

    assert!(!partition.resident_changed);
    assert!(partition.unhandled.is_empty());
    assert_eq!(state.cross_section_runtime.pending_read_ticket_count(), 0);
    assert!(
        state
            .cross_section_runtime
            .chunks
            .values()
            .all(|entry| entry.state
                != crate::cross_section_runtime::CrossSectionChunkState::CpuResident)
    );
    assert!(
        state
            .cross_section_runtime
            .chunks
            .values()
            .all(|entry| entry.state
                != crate::cross_section_runtime::CrossSectionChunkState::Decoding)
    );
}

#[test]
fn cross_section_request_change_keeps_shared_global_chunk_ticket_live() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    state.viewer_layout.switch_to_four_panel();
    for panel_id in [PanelId::Xy, PanelId::Xz] {
        assert!(
            state
                .viewer_layout
                .record_panel_viewports(panel_id, presentation, render)
        );
        crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
            &mut state, panel_id, true,
        )
        .unwrap();
    }

    let shared_chunk = state.cross_section_runtime.panels[&PanelId::Xy].visible_chunks[0].clone();
    assert!(
        state.cross_section_runtime.panels[&PanelId::Xz]
            .visible_chunks
            .contains(&shared_chunk),
        "fixture must expose the same chunk through the linked panel"
    );

    let first_submission = crate::cross_section_streaming::submit_cross_section_panel_bricks_to_pool(
        &mut state,
        PanelId::Xy,
        &pool,
    )
    .unwrap();
    assert!(first_submission.queued);
    assert_eq!(state.cross_section_runtime.pending_read_ticket_count(), 1);

    assert!(
        state.viewer_layout.record_panel_viewports(
            PanelId::Xy,
            PresentationViewport::new(241.0, 180.0).unwrap(),
            render,
        ),
        "changing only the submitting panel should stale that panel generation"
    );
    crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
        &mut state,
        PanelId::Xy,
        true,
    )
    .unwrap();

    let changed_submission =
        crate::cross_section_streaming::submit_cross_section_panel_bricks_to_pool(
            &mut state,
            PanelId::Xy,
            &pool,
        )
        .unwrap();

    assert!(changed_submission.request_changed);
    assert!(!changed_submission.queued);
    assert_eq!(
        state.cross_section_runtime.pending_read_ticket_count(),
        1,
        "the old ticket remains the single live global read because XZ still needs the chunk"
    );
    let stream = state
        .cross_section_runtime
        .panel_streams
        .get(&PanelId::Xy)
        .unwrap();
    assert_eq!(stream.deferred, 1);
}

#[test]
fn cross_section_stale_panel_ticket_updates_global_runtime_when_chunk_is_still_visible() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    let pool = BrickReadPool::new(state.dataset.clone(), 1, 16).unwrap();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    state.viewer_layout.switch_to_four_panel();
    for panel_id in [PanelId::Xy, PanelId::Xz] {
        assert!(
            state
                .viewer_layout
                .record_panel_viewports(panel_id, presentation, render)
        );
        crate::cross_section_scheduler::schedule_cross_section_panel_for_state(
            &mut state, panel_id, true,
        )
        .unwrap();
    }

    let shared_chunk = state.cross_section_runtime.panels[&PanelId::Xy].visible_chunks[0].clone();
    assert!(
        state.cross_section_runtime.panels[&PanelId::Xz]
            .visible_chunks
            .contains(&shared_chunk),
        "fixture must expose the same chunk through the linked panel"
    );

    let submission = crate::cross_section_streaming::submit_cross_section_panel_bricks_to_pool(
        &mut state,
        PanelId::Xy,
        &pool,
    )
    .unwrap();
    assert!(submission.queued);
    assert_eq!(state.cross_section_runtime.pending_read_ticket_count(), 1);

    assert!(
        state.viewer_layout.record_panel_viewports(
            PanelId::Xy,
            PresentationViewport::new(241.0, 180.0).unwrap(),
            render,
        ),
        "changing only the submitting panel should stale that panel generation"
    );

    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    let partition = crate::cross_section_streaming::apply_cross_section_brick_read_outcomes(
        &mut state,
        vec![outcome],
    );

    assert!(partition.resident_changed);
    assert!(partition.unhandled.is_empty());
    assert_eq!(state.cross_section_runtime.pending_read_ticket_count(), 0);
    assert_eq!(
        state.cross_section_runtime.chunks[&shared_chunk].state,
        crate::cross_section_runtime::CrossSectionChunkState::CpuResident
    );
    let stale_xy_stream = state
        .cross_section_runtime
        .panel_streams
        .get(&PanelId::Xy)
        .unwrap();
    assert_eq!(
        stale_xy_stream.completed, 0,
        "stale panel stream accounting must not claim the completion"
    );
}

#[test]
fn display_commands_dirty_cross_section_panels_without_dirtying_3d_panel() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    let ctx = egui::Context::default();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    app.apply_workbench_command(WorkbenchCommand::SetViewerLayout(ViewerLayout::FourPanel), &ctx);
    for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::ThreeD, PanelId::Yz] {
        assert!(
            app.state
                .viewer_layout
                .record_panel_viewports(panel_id, presentation, render)
        );
        assert!(app.state.viewer_layout.mark_panel_displayed(panel_id, 1));
    }

    let next_projection = match app.state.camera.projection {
        Projection::Perspective => Projection::Orthographic,
        Projection::Orthographic => Projection::Perspective,
    };
    assert!(
        app.apply_workbench_command(WorkbenchCommand::SetProjection(next_projection), &ctx)
            .rerender_requested
    );
    {
        let runtime = app.state.viewer_layout.four_panel_runtime().unwrap();
        for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::ThreeD, PanelId::Yz] {
            assert!(
                runtime.panel(panel_id).unwrap().display_current(),
                "{} should remain current after a 3D projection command",
                panel_id.label()
            );
        }
    }

    assert!(
        app.apply_workbench_command(
            WorkbenchCommand::SetLayerInvert {
                layer_index: 0,
                invert: true,
            },
            &ctx,
        )
        .rerender_requested
    );
    let runtime = app.state.viewer_layout.four_panel_runtime().unwrap();
    for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        let panel = runtime.panel(panel_id).unwrap();
        assert_eq!(panel.generation, 2);
        assert!(
            !panel.display_current(),
            "{} should be dirty after a display-transfer command",
            panel_id.label()
        );
    }
    let three_d = runtime.panel(PanelId::ThreeD).unwrap();
    assert_eq!(three_d.generation, 1);
    assert!(three_d.display_current());
}

#[test]
fn cross_section_commands_update_shared_state_without_mutating_3d_camera() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    let ctx = egui::Context::default();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    app.apply_workbench_command(WorkbenchCommand::SetViewerLayout(ViewerLayout::FourPanel), &ctx);
    for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::ThreeD, PanelId::Yz] {
        assert!(
            app.state
                .viewer_layout
                .record_panel_viewports(panel_id, presentation, render)
        );
        assert!(app.state.viewer_layout.mark_panel_displayed(panel_id, 1));
    }
    let camera_before = app.state.camera;
    let cross_section_before = app.state.viewer_layout.cross_section;
    assert_eq!(app.state.viewer_layout.active_cross_section_panel(), None);

    app.apply_workbench_command(
        WorkbenchCommand::CameraPanDrag {
            motion_points: egui::vec2(0.0, 0.0),
        },
        &ctx,
    );
    assert_eq!(app.state.viewer_layout.active_cross_section_panel(), None);
    assert_eq!(app.state.camera, camera_before);

    let pan_outcome = app.apply_workbench_command(
        WorkbenchCommand::CrossSectionPanDrag {
            panel_id: PanelId::Xy,
            motion_points: egui::vec2(12.0, -4.0),
        },
        &ctx,
    );
    assert!(!pan_outcome.rerender_requested);
    assert!(!pan_outcome.texture_refresh_requested);
    assert_eq!(app.state.camera, camera_before);
    assert_ne!(
        app.state.viewer_layout.cross_section.center_world,
        cross_section_before.center_world
    );
    assert_eq!(
        app.state.viewer_layout.active_cross_section_panel(),
        Some(PanelId::Xy)
    );

    let after_pan = app.state.viewer_layout.cross_section;
    let slice_outcome = app.apply_workbench_command(
        WorkbenchCommand::CrossSectionSliceStep {
            panel_id: PanelId::Xz,
            notches: 1.0,
            fast: true,
        },
        &ctx,
    );
    assert!(!slice_outcome.rerender_requested);
    assert_eq!(app.state.camera, camera_before);
    assert_ne!(
        app.state.viewer_layout.cross_section.center_world,
        after_pan.center_world
    );
    assert_eq!(
        app.state.viewer_layout.active_cross_section_panel(),
        Some(PanelId::Xz)
    );

    let after_slice = app.state.viewer_layout.cross_section;
    app.apply_workbench_command(
        WorkbenchCommand::CrossSectionZoom {
            panel_id: PanelId::Yz,
            presentation_viewport: presentation,
            pointer_position_points: egui::pos2(90.0, 70.0),
            scroll_y_points: 120.0,
        },
        &ctx,
    );
    assert_eq!(app.state.camera, camera_before);
    assert_ne!(
        app.state
            .viewer_layout
            .cross_section
            .scale_world_per_screen_point,
        after_slice.scale_world_per_screen_point
    );
    assert_eq!(
        app.state.viewer_layout.active_cross_section_panel(),
        Some(PanelId::Yz)
    );

    let after_zoom = app.state.viewer_layout.cross_section;
    app.apply_workbench_command(
        WorkbenchCommand::CrossSectionRotateDrag {
            panel_id: PanelId::Xy,
            motion_points: egui::vec2(20.0, 0.0),
        },
        &ctx,
    );
    assert_eq!(app.state.camera, camera_before);
    assert_ne!(
        app.state.viewer_layout.cross_section.orientation,
        after_zoom.orientation
    );
    assert_eq!(
        app.state.viewer_layout.active_cross_section_panel(),
        Some(PanelId::Xy)
    );

    let runtime = app.state.viewer_layout.four_panel_runtime().unwrap();
    for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        let panel = runtime.panel(panel_id).unwrap();
        assert_eq!(panel.generation, 5);
        assert!(
            !panel.display_current(),
            "{} should be dirty after linked 2D commands",
            panel_id.label()
        );
    }
    let three_d = runtime.panel(PanelId::ThreeD).unwrap();
    assert_eq!(three_d.generation, 1);
    assert!(three_d.display_current());
}

#[test]
fn reset_view_resets_cross_section_oblique_orientation() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    let ctx = egui::Context::default();
    let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
    let render = RenderViewport::new(480, 360).unwrap();

    app.apply_workbench_command(WorkbenchCommand::SetViewerLayout(ViewerLayout::FourPanel), &ctx);
    for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::ThreeD, PanelId::Yz] {
        assert!(
            app.state
                .viewer_layout
                .record_panel_viewports(panel_id, presentation, render)
        );
        assert!(app.state.viewer_layout.mark_panel_displayed(panel_id, 1));
    }
    let camera_before = app.state.camera;
    let initial_cross_section = app.state.viewer_layout.cross_section;

    app.apply_workbench_command(
        WorkbenchCommand::CrossSectionRotateDrag {
            panel_id: PanelId::Xz,
            motion_points: egui::vec2(32.0, -12.0),
        },
        &ctx,
    );
    assert_ne!(
        app.state.viewer_layout.cross_section.orientation,
        initial_cross_section.orientation
    );
    assert_eq!(app.state.camera, camera_before);

    let reset_outcome = app.apply_workbench_command(WorkbenchCommand::ResetView, &ctx);
    assert!(!reset_outcome.texture_refresh_requested);
    assert_eq!(
        app.state.viewer_layout.cross_section.orientation,
        initial_cross_section.orientation
    );
    assert_eq!(
        app.state.viewer_layout.cross_section.center_world,
        initial_cross_section.center_world
    );
    assert_eq!(
        app.state
            .viewer_layout
            .cross_section
            .scale_world_per_screen_point,
        initial_cross_section.scale_world_per_screen_point
    );
    assert_eq!(app.state.viewer_layout.active_cross_section_panel(), Some(PanelId::Xz));

    let runtime = app.state.viewer_layout.four_panel_runtime().unwrap();
    for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        let panel = runtime.panel(panel_id).unwrap();
        assert!(
            !panel.display_current(),
            "{} should be dirty after Reset View resets 2D state",
            panel_id.label()
        );
    }
    let three_d = runtime.panel(PanelId::ThreeD).unwrap();
    assert_eq!(three_d.generation, 1);
    assert!(three_d.display_current());
}

#[test]
fn virtual_product_automation_generated_fixture_camera_sequence() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    let ctx = egui::Context::default();
    let starting_camera = app.state.camera;

    let initial_commands = [WorkbenchCommand::SetRenderMode(RenderMode::Mip), WorkbenchCommand::FitData];
    for command in initial_commands {
        let outcome = app.apply_workbench_command(command, &ctx);
        if outcome.rerender_requested {
            app.refresh_frame(&ctx);
        } else if outcome.texture_refresh_requested {
            app.refresh_texture_only(&ctx);
        }
    }
    let orbit_start_camera = app.state.camera;
    let interaction_commands = [
        WorkbenchCommand::CameraOrbitDrag {
            start_camera: orbit_start_camera,
            start_position_points: egui::pos2(360.0, 360.0),
            current_position_points: egui::pos2(480.0, 392.0),
            viewport_size_points: egui::vec2(720.0, 720.0),
        },
        WorkbenchCommand::CameraPanDrag {
            motion_points: egui::vec2(40.0, -24.0),
        },
        WorkbenchCommand::CameraZoom {
            scroll_y_points: -120.0,
        },
    ];

    for command in interaction_commands {
        let outcome = app.apply_workbench_command(command, &ctx);
        if outcome.rerender_requested {
            app.refresh_frame(&ctx);
        } else if outcome.texture_refresh_requested {
            app.refresh_texture_only(&ctx);
        }
    }

    assert_eq!(app.state.dataset_name, "Basic uint16 16 cube fixture");
    assert_eq!(app.state.active_render_mode, RenderMode::Mip);
    assert_ne!(app.state.camera, starting_camera);
    assert!(app.state.diagnostics.nonzero_pixels > 0);
    assert!(app.state.last_render_error.is_none());
    assert!(matches!(
        app.state.frame_fidelity.completeness,
        FrameCompleteness::Exact | FrameCompleteness::Complete
    ));
    assert_ne!(
        app.state.frame_fidelity.display_freshness,
        DisplayedFrameFreshness::Stale
    );
    assert!(!app.diagnostics_summary_text().is_empty());
}

#[test]
fn virtual_product_automation_generated_fixture_render_mode_sequence() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    let ctx = egui::Context::default();

    app.apply_workbench_command(WorkbenchCommand::FitData, &ctx);

    for mode in [RenderMode::Mip, RenderMode::Dvr, RenderMode::Isosurface] {
        let outcome = app.apply_workbench_command(WorkbenchCommand::SetRenderMode(mode), &ctx);
        if outcome.rerender_requested {
            app.refresh_frame(&ctx);
        } else if outcome.texture_refresh_requested {
            app.refresh_texture_only(&ctx);
        }
        let mode_parameter_outcome = match mode {
            RenderMode::Mip => None,
            RenderMode::Dvr => Some(app.apply_workbench_command(
                WorkbenchCommand::SetDvrDensityScale {
                    density_scale: 12.0,
                },
                &ctx,
            )),
            RenderMode::Isosurface => Some(app.apply_workbench_command(
                WorkbenchCommand::SetIsoDisplayLevel {
                    display_level: 3_000.0 / f32::from(u16::MAX),
                },
                &ctx,
            )),
        };
        if let Some(outcome) = mode_parameter_outcome {
            if outcome.rerender_requested {
                app.refresh_frame(&ctx);
            } else if outcome.texture_refresh_requested {
                app.refresh_texture_only(&ctx);
            }
        }

        assert_eq!(app.state.active_render_mode, mode);
        assert!(
            app.state.diagnostics.nonzero_pixels > 0,
            "{mode:?} product automation render-mode sequence produced a blank frame"
        );
        assert!(
            app.state.last_render_error.is_none(),
            "{mode:?} product automation render-mode sequence recorded render error {:?}",
            app.state.last_render_error
        );
        assert!(matches!(
            app.state.frame_fidelity.completeness,
            FrameCompleteness::Exact | FrameCompleteness::Complete
        ));
        assert_ne!(
            app.state.frame_fidelity.display_freshness,
            DisplayedFrameFreshness::Stale
        );
    }
}

#[test]
fn iso_light_commands_relight_cached_frame_without_rerendering() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let state = open_dataset_and_render_first_frame(root).unwrap();
    let mut app = test_workbench_app_without_background_runtime(state);
    let ctx = egui::Context::default();

    let mode_outcome =
        app.apply_workbench_command(WorkbenchCommand::SetRenderMode(RenderMode::Isosurface), &ctx);
    assert!(mode_outcome.rerender_requested);
    app.refresh_frame(&ctx);
    app.ensure_texture(&ctx);

    let frame_before = app.state.frame.clone();
    let frame_f32_before = app.state.frame_f32.clone();
    let rendered_channels_before: Vec<_> = app
        .state
        .rendered_channels
        .iter()
        .map(|channel| {
            (
                channel.layer_id.clone(),
                channel.transfer.clone(),
                channel.frame.clone(),
                channel.frame_f32.clone(),
            )
        })
        .collect();
    let fidelity_before = app.state.frame_fidelity.clone();
    let lod_schedule_before = app.state.lod_schedule;
    let stream_counters_before = (
        app.state.brick_stream_generation,
        app.state.brick_stream_requested,
        app.state.brick_stream_completed,
        app.state.brick_stream_cancelled,
        app.state.brick_stream_stale,
        app.state.brick_stream_failed,
        app.state.visible_brick_count,
        app.current_brick_tickets.len(),
    );

    let detach_outcome = app.apply_workbench_command(
        WorkbenchCommand::SetIsoLightDetachedPosition { x: 1.0, y: 0.0 },
        &ctx,
    );

    assert!(!detach_outcome.rerender_requested);
    assert_eq!(
        app.state.iso_light_state,
        IsoLightState::detached_screen(1.0, 0.0).unwrap()
    );
    assert_eq!(app.state.frame, frame_before);
    assert_eq!(app.state.frame_f32, frame_f32_before);
    let rendered_channels_after: Vec<_> = app
        .state
        .rendered_channels
        .iter()
        .map(|channel| {
            (
                channel.layer_id.clone(),
                channel.transfer.clone(),
                channel.frame.clone(),
                channel.frame_f32.clone(),
            )
        })
        .collect();
    assert_eq!(rendered_channels_after, rendered_channels_before);
    assert_eq!(app.state.frame_fidelity, fidelity_before);
    assert_eq!(app.state.lod_schedule, lod_schedule_before);
    assert_eq!(
        (
            app.state.brick_stream_generation,
            app.state.brick_stream_requested,
            app.state.brick_stream_completed,
            app.state.brick_stream_cancelled,
            app.state.brick_stream_stale,
            app.state.brick_stream_failed,
            app.state.visible_brick_count,
            app.current_brick_tickets.len(),
        ),
        stream_counters_before
    );

    let reset_outcome = app.apply_workbench_command(WorkbenchCommand::ResetIsoLight, &ctx);
    assert!(!reset_outcome.rerender_requested);
    assert_eq!(app.state.iso_light_state, IsoLightState::attached_camera());
    assert_eq!(app.state.frame, frame_before);
}

fn write_sparse_spatially_chunked_app_dataset(output_root: &Path, all_empty: bool) -> PathBuf {
    let package_root = if all_empty {
        output_root.join("app-empty-spatially-chunked.m4d")
    } else {
        output_root.join("app-sparse-spatially-chunked.m4d")
    };
    let shape = Shape4D::new(1, 4, 4, 4).unwrap();
    let mut values = vec![0u16; shape.element_count().unwrap() as usize];
    if !all_empty {
        values[0] = 10_000;
    }
    write_native_u16_dataset(
        &package_root,
        NativeU16Dataset {
            id: if all_empty {
                "app-empty-spatially-chunked-fixture".to_owned()
            } else {
                "app-sparse-spatially-chunked-fixture".to_owned()
            },
            name: if all_empty {
                "App empty spatially chunked fixture".to_owned()
            } else {
                "App sparse spatially chunked fixture".to_owned()
            },
            world_space: mirante4d_core::WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_core::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape,
                brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                grid_to_world: mirante4d_core::GridToWorld::scale_um(1.0, 1.0, 1.0),
                display: default_u16_display(),
                values_tzyx: values,
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn write_many_xy_chunks_app_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("app-many-xy-chunks.m4d");
    let shape = Shape4D::new(1, 3, 9, 9).unwrap();
    write_native_u16_dataset(
        &package_root,
        NativeU16Dataset {
            id: "app-many-xy-chunks-fixture".to_owned(),
            name: "App many XY chunks fixture".to_owned(),
            world_space: mirante4d_core::WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_core::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape,
                brick_shape: Shape4D::new(1, 1, 1, 1).unwrap(),
                grid_to_world: mirante4d_core::GridToWorld::scale_um(1.0, 1.0, 1.0),
                display: default_u16_display(),
                values_tzyx: vec![1; shape.element_count().unwrap() as usize],
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn write_time_spatially_chunked_app_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("app-time-spatially-chunked.m4d");
    let shape = Shape4D::new(3, 4, 4, 4).unwrap();
    write_native_u16_dataset(
        &package_root,
        NativeU16Dataset {
            id: "app-time-spatially-chunked-fixture".to_owned(),
            name: "App time spatially chunked fixture".to_owned(),
            world_space: mirante4d_core::WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_core::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape,
                brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                grid_to_world: mirante4d_core::GridToWorld::scale_um(1.0, 1.0, 1.0),
                display: default_u16_display(),
                values_tzyx: fixture_values(shape),
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn write_warm_spatially_chunked_app_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("app-warm-spatially-chunked.m4d");
    let shape = Shape4D::new(1, 6, 6, 6).unwrap();
    write_native_u16_dataset(
        &package_root,
        NativeU16Dataset {
            id: "app-warm-spatially-chunked-fixture".to_owned(),
            name: "App warm spatially chunked fixture".to_owned(),
            world_space: mirante4d_core::WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_core::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape,
                brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                grid_to_world: mirante4d_core::GridToWorld::scale_um(1.0, 1.0, 1.0),
                display: default_u16_display(),
                values_tzyx: fixture_values(shape),
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn write_multiscale_app_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("app-multiscale.m4d");
    let s0_shape = Shape4D::new(1, 4, 4, 4).unwrap();
    let s1_shape = Shape4D::new(1, 2, 2, 2).unwrap();
    let s0_grid_to_world = mirante4d_core::GridToWorld::scale_um(1.0, 1.0, 1.0);
    let s1_grid_to_world = s0_grid_to_world
        .downsampled_integer_centered(2, 2, 2)
        .unwrap();
    write_native_u16_multiscale_dataset(
        &package_root,
        NativeU16MultiscaleDataset {
            id: "app-multiscale-fixture".to_owned(),
            name: "App multiscale fixture".to_owned(),
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

fn write_large_multiscale_app_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("app-large-multiscale.m4d");
    let s0_shape = Shape4D::new(1, 258, 258, 258).unwrap();
    let s1_shape = Shape4D::new(1, 33, 33, 33).unwrap();
    let s0_grid_to_world = mirante4d_core::GridToWorld::scale_um(1.0, 1.0, 1.0);
    let s1_grid_to_world = s0_grid_to_world
        .downsampled_integer_centered(8, 8, 8)
        .unwrap();
    write_native_u16_multiscale_dataset(
        &package_root,
        NativeU16MultiscaleDataset {
            id: "app-large-multiscale-fixture".to_owned(),
            name: "App large multiscale fixture".to_owned(),
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
                        brick_shape: Shape4D::new(1, 32, 32, 32).unwrap(),
                        grid_to_world: s0_grid_to_world,
                        source_scale: None,
                        reduction: ScaleReduction::Source,
                        values_tzyx: vec![1; s0_shape.element_count().unwrap() as usize],
                    },
                    DenseU16Scale {
                        level: 1,
                        shape: s1_shape,
                        brick_shape: Shape4D::new(1, 33, 33, 33).unwrap(),
                        grid_to_world: s1_grid_to_world,
                        source_scale: Some(0),
                        reduction: ScaleReduction::Mean,
                        values_tzyx: vec![1; s1_shape.element_count().unwrap() as usize],
                    },
                ],
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn write_three_scale_budgeted_app_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("app-three-scale-budgeted.m4d");
    let s0_shape = Shape4D::new(1, 288, 288, 288).unwrap();
    let s1_shape = Shape4D::new(1, 144, 144, 144).unwrap();
    let s2_shape = Shape4D::new(1, 72, 72, 72).unwrap();
    let s0_grid_to_world = mirante4d_core::GridToWorld::scale_um(1.0, 1.0, 1.0);
    let s1_grid_to_world = s0_grid_to_world
        .downsampled_integer_centered(2, 2, 2)
        .unwrap();
    let s2_grid_to_world = s1_grid_to_world
        .downsampled_integer_centered(2, 2, 2)
        .unwrap();
    write_native_u16_multiscale_dataset(
        &package_root,
        NativeU16MultiscaleDataset {
            id: "app-three-scale-budgeted-fixture".to_owned(),
            name: "App three-scale budgeted fixture".to_owned(),
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
                        brick_shape: Shape4D::new(1, 24, 24, 24).unwrap(),
                        grid_to_world: s0_grid_to_world,
                        source_scale: None,
                        reduction: ScaleReduction::Source,
                        values_tzyx: vec![1; s0_shape.element_count().unwrap() as usize],
                    },
                    DenseU16Scale {
                        level: 1,
                        shape: s1_shape,
                        brick_shape: Shape4D::new(1, 24, 24, 24).unwrap(),
                        grid_to_world: s1_grid_to_world,
                        source_scale: Some(0),
                        reduction: ScaleReduction::Mean,
                        values_tzyx: vec![1; s1_shape.element_count().unwrap() as usize],
                    },
                    DenseU16Scale {
                        level: 2,
                        shape: s2_shape,
                        brick_shape: Shape4D::new(1, 24, 24, 24).unwrap(),
                        grid_to_world: s2_grid_to_world,
                        source_scale: Some(1),
                        reduction: ScaleReduction::Mean,
                        values_tzyx: vec![1; s2_shape.element_count().unwrap() as usize],
                    },
                ],
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn write_above_minimum_cap_app_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("app-above-minimum-cap.m4d");
    let s0_shape = Shape4D::new(1, 288, 288, 288).unwrap();
    let s1_shape = Shape4D::new(1, 144, 144, 144).unwrap();
    let s2_shape = Shape4D::new(1, 72, 72, 72).unwrap();
    let s0_grid_to_world = mirante4d_core::GridToWorld::scale_um(1.0, 1.0, 1.0);
    let s1_grid_to_world = s0_grid_to_world
        .downsampled_integer_centered(2, 2, 2)
        .unwrap();
    let s2_grid_to_world = s1_grid_to_world
        .downsampled_integer_centered(2, 2, 2)
        .unwrap();
    write_native_u16_multiscale_dataset(
        &package_root,
        NativeU16MultiscaleDataset {
            id: "app-above-minimum-cap-fixture".to_owned(),
            name: "App above minimum cap fixture".to_owned(),
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
                        brick_shape: Shape4D::new(1, 18, 18, 18).unwrap(),
                        grid_to_world: s0_grid_to_world,
                        source_scale: None,
                        reduction: ScaleReduction::Source,
                        values_tzyx: vec![1; s0_shape.element_count().unwrap() as usize],
                    },
                    DenseU16Scale {
                        level: 1,
                        shape: s1_shape,
                        brick_shape: Shape4D::new(1, 18, 18, 18).unwrap(),
                        grid_to_world: s1_grid_to_world,
                        source_scale: Some(0),
                        reduction: ScaleReduction::Mean,
                        values_tzyx: vec![1; s1_shape.element_count().unwrap() as usize],
                    },
                    DenseU16Scale {
                        level: 2,
                        shape: s2_shape,
                        brick_shape: Shape4D::new(1, 36, 36, 36).unwrap(),
                        grid_to_world: s2_grid_to_world,
                        source_scale: Some(1),
                        reduction: ScaleReduction::Mean,
                        values_tzyx: vec![1; s2_shape.element_count().unwrap() as usize],
                    },
                ],
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn sample_scene_artifacts() -> SceneArtifactStore {
    let mut store = SceneArtifactStore::default();
    let track = TrackArtifact::new(
        SceneArtifactId::new("track", "track-a").unwrap(),
        "track a",
        Some(LayerId::new("ch0").unwrap()),
        vec![
            TrackPoint::new(TimeIndex(0), DVec3::ZERO).unwrap(),
            TrackPoint::new(TimeIndex(1), DVec3::new(1.0, 0.0, 0.0)).unwrap(),
            TrackPoint::new(TimeIndex(3), DVec3::new(3.0, 0.0, 0.0)).unwrap(),
        ],
    )
    .unwrap();
    let roi = RoiArtifact::new(
        SceneArtifactId::new("roi", "roi-a").unwrap(),
        "roi a",
        AnalysisWorldGeometry::Box3D {
            min: DVec3::new(-1.0, -1.0, -1.0),
            max: DVec3::new(1.0, 1.0, 1.0),
        },
        SceneArtifactTime::interval(TimeIndex(0), TimeIndex(4)).unwrap(),
    )
    .unwrap();
    let annotation = AnnotationArtifact::new(
        SceneArtifactId::new("annotation", "note-a").unwrap(),
        "note a",
        AnalysisWorldGeometry::Point {
            position: DVec3::new(0.0, 2.0, 0.0),
            radius_px: 4.0,
        },
        Some("interesting".to_owned()),
        SceneArtifactTime::Static,
    )
    .unwrap();
    let measurement = MeasurementArtifact::distance(
        SceneArtifactId::new("measurement", "distance-a").unwrap(),
        "distance a",
        DVec3::ZERO,
        DVec3::new(0.0, 3.0, 4.0),
        MeasurementProvenance {
            source: "manual".to_owned(),
            scope: "world".to_owned(),
        },
        SceneArtifactTime::Static,
    )
    .unwrap();
    store
        .apply(SceneEditCommand::PutTrack { artifact: track })
        .unwrap();
    store
        .apply(SceneEditCommand::PutRoi { artifact: roi })
        .unwrap();
    store
        .apply(SceneEditCommand::PutAnnotation {
            artifact: annotation,
        })
        .unwrap();
    store
        .apply(SceneEditCommand::PutMeasurement {
            artifact: measurement,
        })
        .unwrap();
    store
}

fn scene_handle_pick_value(
    artifact_kind: EditableSceneArtifactKind,
    handle: SceneEditHandle,
) -> PickValue {
    PickValue::ObjectMetadata(
        SceneEditHandleId {
            artifact_kind,
            artifact_id: "test".to_owned(),
            handle,
        }
        .metadata_value(),
    )
}

fn fixture_values(shape: Shape4D) -> Vec<u16> {
    let mut values = Vec::with_capacity(shape.element_count().unwrap() as usize);
    for t in 0..shape.t {
        for z in 0..shape.z {
            for y in 0..shape.y {
                for x in 0..shape.x {
                    values.push(expected_fixture_value(t, z, y, x));
                }
            }
        }
    }
    values
}

fn write_app_ome_stack(path: &Path) -> Result<(), tiff::TiffError> {
    let file = File::create(path)?;
    let mut encoder = TiffEncoder::new(file)?;
    let ome_xml = r#"<OME xmlns="http://www.openmicroscopy.org/Schemas/OME/2016-06"><Image ID="Image:0"><Pixels ID="Pixels:0" DimensionOrder="XYZCT" Type="uint16" SizeX="3" SizeY="2" SizeZ="2" SizeC="1" SizeT="1" PhysicalSizeX="0.25" PhysicalSizeXUnit="um" PhysicalSizeY="0.5" PhysicalSizeYUnit="um" PhysicalSizeZ="0.75" PhysicalSizeZUnit="um"><Channel ID="Channel:0:0" SamplesPerPixel="1"/></Pixels></Image></OME>"#;
    for z in 0..2 {
        let values = (0..2)
            .flat_map(|y| (0..3).map(move |x| (z * 10 + y * 3 + x) as u16))
            .collect::<Vec<_>>();
        let mut image = encoder.new_image::<colortype::Gray16>(3, 2)?;
        if z == 0 {
            image.encoder().write_tag(Tag::ImageDescription, ome_xml)?;
        }
        image.write_data(&values)?;
    }
    Ok(())
}

fn app_multiscale_s1_values(shape: Shape4D) -> Vec<u16> {
    let mut values = Vec::with_capacity(shape.element_count().unwrap() as usize);
    for t in 0..shape.t {
        for z in 0..shape.z {
            for y in 0..shape.y {
                for x in 0..shape.x {
                    values.push((30_000 + t * 1_000 + z * 100 + y * 10 + x) as u16);
                }
            }
        }
    }
    values
}
#[test]
fn app_can_render_all_dense_camera_modes() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let mut state = open_dataset_and_render_first_frame(root).unwrap();
    state.iso_display_level = iso_level_for_u16_threshold(1);

    for mode in [RenderMode::Mip, RenderMode::Isosurface, RenderMode::Dvr] {
        let (frame, diagnostics) = render_app_frame(
            state
                .active_volume
                .as_ref()
                .expect("small fixture opens with dense volume"),
            state.camera,
            state.presentation_viewport,
            state.render_viewport,
            mode,
            &state.active_layer_transfer,
            state.active_dvr_opacity_transfer,
            state.iso_display_level,
            state.dvr_density_scale,
            camera_render_quality(&state),
        )
        .unwrap();

        assert_eq!(frame.width, state.render_viewport.width);
        assert_eq!(frame.height, state.render_viewport.height);
        assert!(diagnostics.nonzero_pixels > 0);
    }
}
